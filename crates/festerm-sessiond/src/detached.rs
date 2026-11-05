//! Non-admitting native launch and exact-generation hosting evidence.
use super::*;
use crate::private_storage as storage;
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const JOURNAL_LIMIT: usize = 4096;
const REGISTRY_LIMIT: usize = 4 * 1024 * 1024;
const START_DEADLINE: Duration = Duration::from_secs(8);

/// One explicit hosting consent. Reusing its UUID never authorizes a second shell.
pub struct DetachedSessionRequest<'a> {
    pub name: &'a str,
    pub profile: &'a LocalProfile,
    pub size: TerminalSize,
    pub scrollback_limit_bytes: usize,
    pub launch_request: &'a str,
}

fn refused(reason: &str) -> PersistentSessionError {
    PersistentSessionError::new(format!("native hosting refused: {reason}"))
}

fn storage_error(_: io::Error) -> PersistentSessionError {
    refused("private native state unavailable; inspect its ownership and permissions")
}

fn text(value: &OsStr) -> Result<&str, PersistentSessionError> {
    value
        .to_str()
        .filter(|value| !value.contains('\0'))
        .ok_or_else(|| refused("spawn parameters cannot be verified"))
}

fn environment_path(profile: &LocalProfile) -> Result<OsString, PersistentSessionError> {
    let mut path = std::env::var_os("PATH").unwrap_or_default();
    match profile.environment() {
        EnvironmentPolicy::Inherit => {}
        EnvironmentPolicy::InheritWith(environment) => {
            let mut keys = std::collections::BTreeSet::new();
            for (key, value) in environment {
                if !is_launchd_correction_variable(key)
                    || !keys.insert(text(key)?.to_ascii_uppercase())
                {
                    return Err(refused("unsupported environment overrides"));
                }
                text(value)?;
                if text(key)?.eq_ignore_ascii_case("PATH") {
                    // Unix environment names are case-sensitive.
                    if cfg!(unix) && key != "PATH" {
                        return Err(refused("unsupported environment overrides"));
                    }
                    path = value.clone();
                }
            }
        }
        EnvironmentPolicy::Clear(_) => return Err(refused("unsupported environment overrides")),
    }
    text(&path)?;
    Ok(path)
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        path.extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"))
    }
}

/// The daemon uses this same normalization before executing and recording evidence.
/// Non-UTF-8/NUL parameters and unsupported environments fail closed for hosting.
#[doc(hidden)]
pub fn normalize_spawn_profile(
    profile: &LocalProfile,
) -> Result<LocalProfile, PersistentSessionError> {
    let path = environment_path(profile)?;
    let cwd = profile
        .working_directory()
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .and_then(std::fs::canonicalize)
        .map_err(|_| refused("effective working directory unavailable"))?;
    if !cwd.is_dir() {
        return Err(refused("effective working directory unavailable"));
    }
    text(cwd.as_os_str())?;
    let program = profile.executable();
    let program_text = text(program.as_os_str())?;
    if program_text.is_empty() {
        return Err(refused("effective executable unavailable"));
    }
    for argument in profile.arguments() {
        text(argument)?;
    }
    let has_directory = program.is_absolute() || program.components().count() > 1;
    #[cfg(windows)]
    if has_directory && !program.is_absolute() {
        return Err(refused("relative executable paths cannot be verified"));
    }
    let resolved = if has_directory {
        let candidate = if program.is_absolute() {
            program.to_path_buf()
        } else {
            cwd.join(program)
        };
        executable(&candidate).then_some(candidate)
    } else {
        // Resolve once using the effective PATH, then execute that absolute path.
        // Windows command-shell scripts are deliberately not hosting evidence.
        #[cfg(windows)]
        let extensions = std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".EXE"));
        #[cfg(windows)]
        let extensions = text(&extensions)?
            .split(';')
            .map(|extension| {
                extension
                    .strip_prefix('.')
                    .filter(|extension| {
                        !extension.is_empty()
                            && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    })
                    .ok_or_else(|| refused("executable search extensions cannot be verified"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut found = None;
        for directory in std::env::split_paths(&path) {
            #[cfg(windows)]
            if !directory.is_absolute() {
                return Err(refused(
                    "relative executable search paths cannot be verified",
                ));
            }
            let directory = if directory.is_absolute() {
                directory
            } else {
                cwd.join(directory)
            };
            let candidate = directory.join(program);
            if executable(&candidate) {
                found = Some(candidate);
                break;
            }
            #[cfg(windows)]
            {
                if candidate.exists() {
                    return Err(refused("executable search selected an unsupported program"));
                }
                for extension in &extensions {
                    let candidate = candidate.with_extension(extension);
                    if candidate.exists() {
                        if !executable(&candidate) {
                            return Err(refused(
                                "executable search selected an unsupported program",
                            ));
                        }
                        found = Some(candidate);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
        }
        found
    }
    .ok_or_else(|| refused("effective executable unavailable"))?;
    // Keep Unix's final filename: resolving `sh` to a `dash` symlink target
    // would change argv[0] and therefore the shell's execution mode.
    #[cfg(unix)]
    let resolved = {
        let parent = resolved
            .parent()
            .ok_or_else(|| refused("effective executable unavailable"))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|_| refused("effective executable unavailable"))?;
        parent.join(
            resolved
                .file_name()
                .ok_or_else(|| refused("effective executable unavailable"))?,
        )
    };
    #[cfg(windows)]
    let resolved = {
        use std::os::windows::fs::MetadataExt;
        if std::fs::symlink_metadata(&resolved)
            .map_err(|_| refused("effective executable unavailable"))?
            .file_attributes()
            & 0x400
            != 0
        {
            return Err(refused("reparse executable cannot be verified"));
        }
        std::fs::canonicalize(resolved).map_err(|_| refused("effective executable unavailable"))?
    };
    text(resolved.as_os_str())?;
    Ok(LocalProfile::new(resolved)
        .with_arguments(profile.arguments().iter().cloned())
        .with_working_directory(cwd)
        .with_environment(profile.environment().clone()))
}

fn normalized_fingerprint(profile: &LocalProfile) -> Result<String, PersistentSessionError> {
    let mut digest = Sha256::new();
    digest.update(b"festerm-native-spawn-v1\0");
    for value in std::iter::once(profile.executable().as_os_str())
        .chain(profile.working_directory().map(Path::as_os_str))
        .chain(profile.arguments().iter().map(OsString::as_os_str))
    {
        let value = text(value)?.as_bytes();
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    }
    Ok(format!("v1:{:x}", digest.finalize()))
}

/// Versioned, content-free evidence of effective executable, ordered argv and cwd.
/// Call after the application's approved local-environment correction.
pub fn spawn_fingerprint(profile: &LocalProfile) -> Result<String, PersistentSessionError> {
    normalized_fingerprint(&normalize_spawn_profile(profile)?)
}

fn valid_fingerprint(value: &str) -> bool {
    value.len() == 67
        && value.starts_with("v1:")
        && value.as_bytes()[3..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn uuid(value: &str) -> Result<Uuid, PersistentSessionError> {
    Uuid::parse_str(value)
        .ok()
        .filter(|id| {
            id.get_version_num() == 4
                && id.get_variant() == uuid::Variant::RFC4122
                && id.hyphenated().to_string() == value
        })
        .ok_or_else(|| refused("a canonical fresh hosting-consent UUID is required"))
}

/// Creates missing runtime components privately; existing permissions are never repaired.
#[doc(hidden)]
pub fn prepare_native_runtime(root: &Path) -> Result<File, PersistentSessionError> {
    match storage::private_directory(root, true) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = root
                .parent()
                .ok_or_else(|| refused("invalid runtime directory"))?;
            let _parent = prepare_native_runtime(parent)?;
            storage::private_directory(root, true).map_err(storage_error)
        }
        Err(error) => Err(storage_error(error)),
    }
}

fn open_lock(directory: &File, name: &Path) -> Result<File, PersistentSessionError> {
    match storage::open_private_file(directory, name, true) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            storage::open_private_file(directory, name, false).map_err(storage_error)
        }
        Err(error) => Err(storage_error(error)),
    }
}

fn lock(file: &File, exclusive: bool) -> Result<(), PersistentSessionError> {
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        let result = if exclusive {
            FileExt::try_lock_exclusive(file)
        } else {
            FileExt::try_lock_shared(file)
        };
        match result {
            Ok(()) => return Ok(()),
            Err(error) if lock_is_contended(&error) && Instant::now() < deadline => {
                thread::sleep(POLL_INTERVAL);
            }
            Err(_) => {
                return Err(refused(
                    "native state busy; retry without creating a fresh shell",
                ))
            }
        }
    }
}

/// Shared with ordinary helper starts. Never hold a registry lock while launching a child.
#[doc(hidden)]
pub fn lock_native_start_in(root: &Path, name: &str) -> Result<File, PersistentSessionError> {
    PersistentSessionName::new(name).map_err(|_| refused("invalid session name"))?;
    let directory = storage::private_directory(root, false).map_err(storage_error)?;
    let file = open_lock(&directory, Path::new(&format!("{name}.start.lock")))?;
    lock(&file, true)?;
    Ok(file)
}

fn registry(directory: &File) -> Result<SessionRegistry, PersistentSessionError> {
    let file = open_lock(directory, Path::new("registry.lock"))?;
    lock(&file, false)?;
    match storage::read_private(directory, Path::new("registry.json"), REGISTRY_LIMIT) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| refused("invalid native registry")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SessionRegistry::default()),
        Err(error) => Err(storage_error(error)),
    }
}

fn verified_live(
    root: &Path,
    directory: &File,
    record: &SessionRecord,
) -> Result<bool, PersistentSessionError> {
    if record.pid == 0 || record.created_at_unix_ms == 0 {
        return Err(refused("invalid native generation"));
    }
    if !process_alive(record.pid) {
        return Ok(false);
    }
    let generation = format!("{}-{}", record.pid, record.created_at_unix_ms);
    if !record.socket.ends_with(&generation)
        && !record.socket.ends_with(&format!("{generation}.sock"))
    {
        return Err(refused("legacy daemon lacks hosting evidence; preserve it and authorize a fresh session separately"));
    }
    // Inspect the lease through the pinned private directory, not an arbitrary
    // registry-supplied path. The endpoint itself is never opened for admission.
    let lease = match storage::open_private_file(
        directory,
        Path::new(&format!("lease-{generation}")),
        false,
    ) {
        Ok(lease) => lease,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(storage_error(error)),
    };
    match FileExt::try_lock_shared(&lease) {
        Ok(()) => Ok(false),
        Err(error) if lock_is_contended(&error) => {
            if cfg!(unix) {
                let expected = root.join(format!("{generation}.sock"));
                if Path::new(&record.socket) != expected {
                    return Err(refused("invalid native endpoint"));
                }
            }
            #[cfg(windows)]
            if record.socket != format!(r"\\.\pipe\festerm-sessiond-{generation}") {
                return Err(refused("invalid native endpoint"));
            }
            Ok(process_alive(record.pid))
        }
        Err(error) => Err(storage_error(error)),
    }
}

fn matching_record<'a>(
    registry: &'a SessionRegistry,
    name: &str,
) -> Result<Option<&'a SessionRecord>, PersistentSessionError> {
    if registry.unreadable.contains_key(name) {
        return Err(refused("native daemon metadata is not supported"));
    }
    let record = registry.sessions.get(name);
    if record.is_some_and(|record| record.name != name) {
        return Err(refused("inconsistent native identity"));
    }
    Ok(record)
}

fn check_evidence(record: &SessionRecord, expected: &str) -> Result<(), PersistentSessionError> {
    if !valid_fingerprint(expected) || record.spawn_fingerprint.as_deref() != Some(expected) {
        return Err(refused("spawn evidence missing or different; preserve the running session and review hosting consent"));
    }
    ensure_protocol_compatible(record)
        .map_err(|_| refused("native daemon protocol is not supported"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchJournal {
    version: u8,
    launch_request: String,
    name: String,
    fingerprint: String,
    generation: u128,
    identity: Option<JournalIdentity>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalIdentity {
    pid: u32,
    created_at_unix_ms: u128,
    endpoint: String,
}

impl LaunchJournal {
    fn bind(&mut self, identity: &NativeSessionIdentity) {
        self.identity = Some(JournalIdentity {
            pid: identity.pid,
            created_at_unix_ms: identity.created_at_unix_ms,
            endpoint: identity.endpoint.clone(),
        });
    }

    fn accepts(&self, identity: &NativeSessionIdentity) -> bool {
        self.generation == identity.created_at_unix_ms
            && self.identity.as_ref().is_none_or(|bound| {
                bound.pid == identity.pid
                    && bound.created_at_unix_ms == identity.created_at_unix_ms
                    && bound.endpoint == identity.endpoint
            })
    }
}

/// Ensures one explicitly authorized shell without a frontend stream or lease.
pub fn ensure_detached(
    request: DetachedSessionRequest<'_>,
) -> Result<NativeSessionIdentity, PersistentSessionError> {
    ensure_detached_in(&runtime_root()?, request)
}

/// Explicit-root seam. A reserved but unavailable generation is never relaunched.
pub fn ensure_detached_in(
    root: &Path,
    request: DetachedSessionRequest<'_>,
) -> Result<NativeSessionIdentity, PersistentSessionError> {
    ensure_with_launcher(root, request, |root, request, profile, generation| {
        launch(root, request, profile, generation)
    })
}

fn ensure_with_launcher(
    root: &Path,
    request: DetachedSessionRequest<'_>,
    launch: impl FnOnce(
        &Path,
        &DetachedSessionRequest<'_>,
        &LocalProfile,
        u128,
    ) -> Result<(), PersistentSessionError>,
) -> Result<NativeSessionIdentity, PersistentSessionError> {
    PersistentSessionName::new(request.name).map_err(|_| refused("invalid session name"))?;
    let id = uuid(request.launch_request)?;
    let profile = normalize_spawn_profile(request.profile)?;
    let fingerprint = normalized_fingerprint(&profile)?;
    let directory = prepare_native_runtime(root)?;
    // UUID-global serialization also prevents reusing a request for another name.
    let request_lock = open_lock(&directory, Path::new(&format!("launch-{id}.lock")))?;
    lock(&request_lock, true)?;
    let start_lock = lock_native_start_in(root, request.name)?;
    let journal_name = PathBuf::from(format!("launch-{id}.json"));
    let previous = match storage::read_private(&directory, &journal_name, JOURNAL_LIMIT) {
        Ok(bytes) => Some(
            serde_json::from_slice::<LaunchJournal>(&bytes).map_err(|_| {
                refused("invalid launch journal; new explicit fresh-shell consent is required")
            })?,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(storage_error(error)),
    };
    let records = registry(&directory)?;
    let record = matching_record(&records, request.name)?;
    let live = match record {
        Some(record) => verified_live(root, &directory, record)?,
        None => false,
    };
    if let Some(mut journal) = previous {
        if journal.version != 1
            || journal.launch_request != request.launch_request
            || uuid(&journal.launch_request)? != id
            || journal.name != request.name
            || journal.fingerprint != fingerprint
            || journal.generation == 0
        {
            return Err(refused("foreign or incompatible launch journal"));
        }
        let record = record.filter(|_| live).ok_or_else(|| {
            refused("reserved generation unavailable; new explicit fresh-shell consent is required")
        })?;
        check_evidence(record, &fingerprint)?;
        let identity = NativeSessionIdentity::from_record(record);
        if !journal.accepts(&identity)
            || (journal.identity.is_none()
                && record.launch_request.as_deref() != Some(request.launch_request))
        {
            return Err(refused(
                "reserved generation replaced; new explicit fresh-shell consent is required",
            ));
        }
        if journal.identity.is_none() {
            journal.bind(&identity);
            checkpoint(&directory, &journal_name, &journal)?;
        }
        return Ok(identity);
    }
    if live {
        let record = record.unwrap();
        check_evidence(record, &fingerprint)?;
        let identity = NativeSessionIdentity::from_record(record);
        let mut journal = LaunchJournal {
            version: 1,
            launch_request: request.launch_request.into(),
            name: request.name.into(),
            fingerprint,
            generation: identity.created_at_unix_ms,
            identity: None,
        };
        journal.bind(&identity);
        write_reservation(&directory, &journal_name, &journal)?;
        return Ok(identity);
    }
    let mut journal = LaunchJournal {
        version: 1,
        launch_request: request.launch_request.into(),
        name: request.name.into(),
        fingerprint,
        generation: lifecycle::now_ms(),
        identity: None,
    };
    // Durable before launching, including a failure to spawn the start helper.
    write_reservation(&directory, &journal_name, &journal)?;
    drop(start_lock);
    launch(root, &request, &profile, journal.generation)?;
    let _start_lock = lock_native_start_in(root, request.name)?;
    let records = registry(&directory)?;
    let record = matching_record(&records, request.name)?.ok_or_else(|| {
        refused("reserved generation unavailable; inspect native startup diagnostics")
    })?;
    check_evidence(record, &journal.fingerprint)?;
    let identity = NativeSessionIdentity::from_record(record);
    if !journal.accepts(&identity)
        || record.launch_request.as_deref() != Some(request.launch_request)
        || !verified_live(root, &directory, record)?
    {
        return Err(refused(
            "native creation race or identity loss; no replacement was authorized",
        ));
    }
    journal.bind(&identity);
    checkpoint(&directory, &journal_name, &journal)?;
    Ok(identity)
}

fn write_reservation(
    directory: &File,
    name: &Path,
    journal: &LaunchJournal,
) -> Result<(), PersistentSessionError> {
    let bytes = serde_json::to_vec(journal).map_err(|_| refused("invalid launch journal"))?;
    storage::write_new_private(directory, name, &bytes).map_err(storage_error)
}

fn checkpoint(
    directory: &File,
    name: &Path,
    journal: &LaunchJournal,
) -> Result<(), PersistentSessionError> {
    let bytes = serde_json::to_vec(journal).map_err(|_| refused("invalid launch journal"))?;
    storage::replace_private(directory, name, &bytes).map_err(storage_error)
}

fn launch(
    root: &Path,
    request: &DetachedSessionRequest<'_>,
    profile: &LocalProfile,
    generation: u128,
) -> Result<(), PersistentSessionError> {
    launch_with_helper(
        daemon_executable_for_root(Some(root))?,
        root,
        request,
        profile,
        generation,
    )
}

fn launch_with_helper(
    helper: PathBuf,
    root: &Path,
    request: &DetachedSessionRequest<'_>,
    profile: &LocalProfile,
    generation: u128,
) -> Result<(), PersistentSessionError> {
    let mut command = Command::new(helper);
    command
        .env("FESTERM_SESSIOND_RUNTIME_ROOT", root)
        .env("FESTERM_SESSIOND_GENERATION", generation.to_string())
        .env("FESTERM_SESSIOND_LAUNCH_REQUEST", request.launch_request)
        .env(
            "FESTERM_SESSIOND_EXPECTED_SPAWN",
            normalized_fingerprint(profile)?,
        )
        .args(["start", "--name", request.name, "--shell"])
        .arg(profile.executable())
        .args([
            "--cols",
            &request.size.columns().to_string(),
            "--rows",
            &request.size.rows().to_string(),
        ])
        .args([
            "--scrollback-limit-bytes",
            &request.scrollback_limit_bytes.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for argument in profile.arguments() {
        command.arg("--arg").arg(argument);
    }
    command
        .arg("--cwd")
        .arg(profile.working_directory().unwrap());
    if let EnvironmentPolicy::InheritWith(environment) = profile.environment() {
        command.envs(environment);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().map_err(|_| {
        refused("native helper did not start; new explicit fresh-shell consent is required")
    })?;
    let deadline = Instant::now() + START_DEADLINE;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) | Err(_) => return Err(refused(
                "native launch outcome unavailable; retry can only adopt the reserved generation",
            )),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(refused(
                    "native start deadline exceeded; retry can only adopt the reserved generation",
                ));
            }
        }
    }
}

/// Checks live generation and recorded spawn evidence without connecting or respawning.
pub fn verify_spawn_fingerprint_in(
    root: &Path,
    selected: &NativeSessionIdentity,
    expected: &str,
) -> Result<(), PersistentSessionError> {
    PersistentSessionName::new(&selected.name).map_err(|_| refused("invalid session name"))?;
    let directory = storage::private_directory(root, false).map_err(storage_error)?;
    let records = registry(&directory)?;
    let record = matching_record(&records, &selected.name)?
        .ok_or_else(|| refused("selected generation unavailable"))?;
    if NativeSessionIdentity::from_record(record) != *selected
        || !verified_live(root, &directory, record)?
    {
        return Err(refused("selected generation unavailable or replaced"));
    }
    check_evidence(record, expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, fs};

    struct Fixture {
        root: PathBuf,
        directory: File,
        leases: RefCell<Vec<File>>,
        _cleanup: OwnedRoot,
    }

    struct OwnedRoot(PathBuf);

    impl Drop for OwnedRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    impl Fixture {
        fn new() -> Self {
            #[cfg(windows)]
            let root = storage::test_directory();
            #[cfg(not(windows))]
            let root = std::env::current_dir()
                .unwrap()
                .join(format!("nd-{}", &Uuid::new_v4().simple().to_string()[..8],));
            let directory = storage::private_directory(&root, true).unwrap();
            Self {
                _cleanup: OwnedRoot(root.clone()),
                root,
                directory,
                leases: RefCell::new(Vec::new()),
            }
        }

        fn profile(&self) -> LocalProfile {
            LocalProfile::new(std::env::current_exe().unwrap()).with_working_directory(&self.root)
        }

        fn request<'a>(
            &self,
            profile: &'a LocalProfile,
            id: &'a str,
        ) -> DetachedSessionRequest<'a> {
            DetachedSessionRequest {
                name: "owned",
                profile,
                size: TerminalSize::new(80, 24).unwrap(),
                scrollback_limit_bytes: 4096,
                launch_request: id,
            }
        }

        fn publish(
            &self,
            generation: u128,
            fingerprint: Option<&str>,
            attached: bool,
            request: Option<&str>,
        ) -> NativeSessionIdentity {
            let pid = std::process::id();
            let suffix = format!("{pid}-{generation}");
            #[cfg(unix)]
            let endpoint = self
                .root
                .join(format!("{suffix}.sock"))
                .to_str()
                .unwrap()
                .to_owned();
            #[cfg(windows)]
            let endpoint = format!(r"\\.\pipe\festerm-sessiond-{suffix}");
            let lease = storage::open_private_file(
                &self.directory,
                Path::new(&format!("lease-{suffix}")),
                true,
            )
            .unwrap();
            lease.lock_exclusive().unwrap();
            self.leases.borrow_mut().push(lease);
            let record = serde_json::json!({
                "name": "owned", "pid": pid, "created_at_unix_ms": generation,
                "socket": endpoint, "shell": "existing display metadata",
                "arguments": [], "attached": attached, "protocol_version": PROTOCOL_VERSION,
                "snapshot_schema_version": RECOVERY_SNAPSHOT_SCHEMA_VERSION,
                "spawn_fingerprint": fingerprint,
                "launch_request": request,
                "future_optional_metadata": true,
            });
            let bytes =
                serde_json::to_vec(&serde_json::json!({"sessions": {"owned": record}})).unwrap();
            storage::replace_private(&self.directory, Path::new("registry.json"), &bytes).unwrap();
            NativeSessionIdentity {
                name: "owned".into(),
                pid,
                created_at_unix_ms: generation,
                endpoint,
            }
        }

        fn reserve(&self, id: &str, profile: &LocalProfile, generation: u128) {
            let journal = LaunchJournal {
                version: 1,
                launch_request: id.into(),
                name: "owned".into(),
                fingerprint: spawn_fingerprint(profile).unwrap(),
                generation,
                identity: None,
            };
            write_reservation(
                &self.directory,
                Path::new(&format!("launch-{id}.json")),
                &journal,
            )
            .unwrap();
        }
    }

    fn no_launch(
        _: &Path,
        _: &DetachedSessionRequest<'_>,
        _: &LocalProfile,
        _: u128,
    ) -> Result<(), PersistentSessionError> {
        panic!("this request must not launch or admit a client")
    }

    #[test]
    fn detached_matching_attached_reuse_is_non_admitting_and_request_bound() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let fingerprint = spawn_fingerprint(&profile).unwrap();
        let first = fixture.publish(101, Some(&fingerprint), true, None);
        let id = Uuid::new_v4().to_string();
        assert_eq!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).unwrap(),
            first
        );
        assert_eq!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).unwrap(),
            first
        );
        verify_spawn_fingerprint_in(&fixture.root, &first, &fingerprint).unwrap();
        let replacement = fixture.publish(102, Some(&fingerprint), true, None);
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
        assert!(verify_spawn_fingerprint_in(&fixture.root, &first, &fingerprint).is_err());
        verify_spawn_fingerprint_in(&fixture.root, &replacement, &fingerprint).unwrap();
        let records = registry(&fixture.directory).unwrap();
        assert!(records.sessions["owned"].attached);
    }

    #[test]
    fn detached_absent_start_reserves_before_launch_and_starts_once() {
        let fixture = Fixture::new();
        let profile = fixture.profile().with_arguments(["ordered", "arguments"]);
        let id = Uuid::new_v4().to_string();
        let identity = ensure_with_launcher(
            &fixture.root,
            fixture.request(&profile, &id),
            |_, _, normalized, generation| {
                let bytes = storage::read_private(
                    &fixture.directory,
                    Path::new(&format!("launch-{id}.json")),
                    JOURNAL_LIMIT,
                )
                .unwrap();
                let journal: LaunchJournal = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(journal.generation, generation);
                assert!(journal.identity.is_none());
                assert!(normalized.executable().is_absolute());
                fixture.publish(
                    generation,
                    Some(&spawn_fingerprint(normalized).unwrap()),
                    false,
                    Some(&id),
                );
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).unwrap(),
            identity
        );
        fixture.leases.borrow_mut().clear();
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
    }

    #[test]
    fn detached_crash_after_reservation_never_relaunches() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let id = Uuid::new_v4().to_string();
        let error = ensure_with_launcher(
            &fixture.root,
            fixture.request(&profile, &id),
            |_, _, _, _| Err(refused("injected crash before spawn")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected crash"));
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch)
                .unwrap_err()
                .to_string()
                .contains("fresh-shell consent")
        );
    }

    #[test]
    fn detached_uncertain_launch_adopts_only_reserved_generation() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let fingerprint = spawn_fingerprint(&profile).unwrap();
        let id = Uuid::new_v4().to_string();
        fixture.reserve(&id, &profile, 201);
        let selected = fixture.publish(201, Some(&fingerprint), false, Some(&id));
        assert_eq!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).unwrap(),
            selected
        );
        let other_id = Uuid::new_v4().to_string();
        fixture.reserve(&other_id, &profile, 202);
        assert!(ensure_with_launcher(
            &fixture.root,
            fixture.request(&profile, &other_id),
            no_launch
        )
        .is_err());
        let collision = Fixture::new();
        let profile = collision.profile();
        let fingerprint = spawn_fingerprint(&profile).unwrap();
        collision.reserve(&id, &profile, 203);
        collision.publish(203, Some(&fingerprint), false, Some(&other_id));
        assert!(
            ensure_with_launcher(&collision.root, collision.request(&profile, &id), no_launch)
                .is_err()
        );
    }

    #[test]
    fn detached_creation_race_refuses_foreign_winner_without_adoption() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let id = Uuid::new_v4().to_string();
        let error = ensure_with_launcher(
            &fixture.root,
            fixture.request(&profile, &id),
            |_, _, profile, reserved| {
                fixture.publish(
                    reserved - 1,
                    Some(&spawn_fingerprint(profile).unwrap()),
                    false,
                    None,
                );
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("creation race"));
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
    }

    #[test]
    fn detached_mismatched_and_legacy_evidence_refuse_but_registry_remains_readable() {
        let fixture = Fixture::new();
        let profile = fixture
            .profile()
            .with_arguments(["do-not-report-this-argument"]);
        let id = Uuid::new_v4().to_string();
        let unknown = fixture.publish(301, None, true, None);
        let error = ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch)
            .unwrap_err();
        assert!(error.to_string().contains("evidence"));
        assert!(!error.to_string().contains("do-not-report"));
        assert!(verify_spawn_fingerprint_in(
            &fixture.root,
            &unknown,
            &spawn_fingerprint(&profile).unwrap()
        )
        .is_err());
        // The ordinary registry reader still reads metadata without evidence,
        // including older protocol records and ignored optional fields.
        let records = load_registry_in(&fixture.root).unwrap();
        assert_eq!(
            NativeSessionIdentity::from_record(&records.sessions["owned"]),
            unknown
        );
        let wrong = spawn_fingerprint(&fixture.profile()).unwrap();
        fixture.publish(302, Some(&wrong), true, None);
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
        assert!(!fixture.root.join(format!("launch-{id}.json")).exists());
    }

    #[test]
    fn detached_fingerprint_preserves_argv_cwd_and_effective_path() {
        let fixture = Fixture::new();
        let profile = fixture.profile().with_arguments(["one", "two"]);
        let fingerprint = spawn_fingerprint(&profile).unwrap();
        assert_ne!(
            fingerprint,
            spawn_fingerprint(&fixture.profile().with_arguments(["two", "one"])).unwrap()
        );
        assert_ne!(
            fingerprint,
            spawn_fingerprint(&fixture.profile().with_arguments(["onetwo"])).unwrap()
        );
        let other = Fixture::new();
        assert_ne!(
            fingerprint,
            spawn_fingerprint(&other.profile().with_arguments(["one", "two"])).unwrap()
        );
        let executable = std::env::current_exe().unwrap();
        let environment = BTreeMap::from([(
            OsString::from("PATH"),
            executable.parent().unwrap().as_os_str().to_owned(),
        )]);
        let bare = LocalProfile::new(executable.file_name().unwrap())
            .with_working_directory(&fixture.root)
            .with_arguments(["one", "two"])
            .with_environment(EnvironmentPolicy::InheritWith(environment));
        assert_eq!(fingerprint, spawn_fingerprint(&bare).unwrap());
        assert_eq!(
            fingerprint,
            spawn_fingerprint(&normalize_spawn_profile(&bare).unwrap()).unwrap()
        );
        let unsupported = profile
            .clone()
            .with_environment(EnvironmentPolicy::InheritWith(BTreeMap::from([(
                OsString::from("SECRET"),
                OsString::from("never include"),
            )])));
        assert!(spawn_fingerprint(&unsupported).is_err());
        assert!(spawn_fingerprint(&profile.with_arguments(["nul\0argument"])).is_err());
    }

    #[test]
    fn detached_fingerprint_resolves_effective_path_in_order() {
        let first = Fixture::new();
        let second = Fixture::new();
        let filename = if cfg!(windows) {
            "owned-program.exe"
        } else {
            "owned-program"
        };
        for fixture in [&first, &second] {
            storage::write_new_private(&fixture.directory, Path::new(filename), b"fixture program")
                .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    fixture.root.join(filename),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
        }
        let make_profile = |paths: &[&Path]| {
            LocalProfile::new("owned-program")
                .with_working_directory(&first.root)
                .with_arguments(["one", "two"])
                .with_environment(EnvironmentPolicy::InheritWith(BTreeMap::from([(
                    OsString::from("PATH"),
                    std::env::join_paths(paths).unwrap(),
                )])))
        };
        let forward = make_profile(&[&first.root, &second.root]);
        let backward = make_profile(&[&second.root, &first.root]);
        assert_ne!(
            spawn_fingerprint(&forward).unwrap(),
            spawn_fingerprint(&backward).unwrap()
        );
        let absolute = LocalProfile::new(first.root.join(filename))
            .with_working_directory(first.root.join("."))
            .with_arguments(["one", "two"]);
        assert_eq!(
            spawn_fingerprint(&forward).unwrap(),
            spawn_fingerprint(&absolute).unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn detached_preserves_executable_symlink_name_but_refuses_state_symlinks() {
        use std::os::unix::{ffi::OsStringExt, fs::symlink};
        let fixture = Fixture::new();
        let alias = fixture.root.join("shell-alias");
        symlink(std::env::current_exe().unwrap(), &alias).unwrap();
        let profile = LocalProfile::new(alias).with_working_directory(&fixture.root);
        assert_eq!(
            normalize_spawn_profile(&profile)
                .unwrap()
                .executable()
                .file_name()
                .unwrap(),
            "shell-alias"
        );
        assert!(spawn_fingerprint(
            &profile
                .clone()
                .with_arguments([OsString::from_vec(vec![0xff])])
        )
        .is_err());
        let id = Uuid::new_v4().to_string();
        storage::write_new_private(&fixture.directory, Path::new("payload"), b"{}").unwrap();
        symlink(
            fixture.root.join("payload"),
            fixture.root.join(format!("launch-{id}.json")),
        )
        .unwrap();
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
    }

    #[test]
    fn detached_journal_rejects_malformed_foreign_uuid_and_incompatible_schema() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        assert!(ensure_with_launcher(
            &fixture.root,
            fixture.request(&profile, "../escape"),
            no_launch
        )
        .is_err());
        for bytes in [
            b"partial".to_vec(),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "launch_request": Uuid::new_v4().to_string(), "name": "owned",
                "fingerprint": spawn_fingerprint(&profile).unwrap(), "generation": 44, "identity": null,
            })).unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "version": 2, "launch_request": "foreign", "name": "owned",
                "fingerprint": spawn_fingerprint(&profile).unwrap(), "generation": 44, "identity": null,
                "future_authority": true,
            })).unwrap(),
        ] {
            let id = Uuid::new_v4().to_string();
            storage::write_new_private(&fixture.directory, Path::new(&format!("launch-{id}.json")), &bytes).unwrap();
            assert!(ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err());
        }
    }

    #[test]
    fn detached_start_lock_deadline_and_private_state_refusal() {
        let fixture = Fixture::new();
        let profile = fixture.profile();
        let id = Uuid::new_v4().to_string();
        let held = lock_native_start_in(&fixture.root, "owned").unwrap();
        let started = Instant::now();
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(held);
        let name = Path::new("registry.json");
        fs::write(fixture.root.join(name), b"{}").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(fixture.root.join(name), fs::Permissions::from_mode(0o644))
                .unwrap();
        }
        assert!(
            ensure_with_launcher(&fixture.root, fixture.request(&profile, &id), no_launch).is_err()
        );
    }
}
