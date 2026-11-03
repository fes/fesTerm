//! Dedicated, unelevated login services. Definitions are create-only: an
//! existing entry must match exactly, and no daemon/session is ever stopped.
//! The one exception is an explicit upgrade ([`stop_and_replace`]), which may
//! stop and rewrite only this UUID's own worker definition.
use festerm_tunnel_bridge::setup_storage;
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

const OUTPUT_LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(15);

pub struct ServiceSpec {
    pub id: String,
    pub executable: PathBuf,
    pub state: PathBuf,
}

fn refused(stage: &'static str) -> io::Error {
    io::Error::other(stage)
}

/// Must run before managed install/serve performs any mutation or sign-in.
pub(super) fn ensure_unprivileged_user() -> io::Result<()> {
    #[cfg(unix)]
    {
        festerm_unix_security::current_user_id()
            .map(|_| ())
            .map_err(|_| refused("service.user_context"))
    }
    #[cfg(windows)]
    {
        festerm_windows_security::current_user_service_context()
            .map(|_| ())
            .map_err(|_| refused("service.user_context"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(refused("service.unsupported_platform"))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && id != "00000000-0000-0000-0000-000000000000"
}

fn path_text(path: &Path) -> io::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| refused("service.invalid_path"))?;
    if !path.is_absolute()
        || text.len() > 4096
        || text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{fffe}' | '\u{ffff}'))
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err(refused("service.invalid_path"));
    }
    // Components normalizes interior "."; reject them in the original spelling.
    if text
        .split(['/', '\\'])
        .any(|part| matches!(part, "." | ".."))
    {
        return Err(refused("service.invalid_path"));
    }
    #[cfg(windows)]
    if !festerm_tunnel_bridge::files::valid_path(
        text,
        festerm_tunnel_bridge::files::PathStyle::Windows,
    ) {
        return Err(refused("service.invalid_path"));
    }
    Ok(text)
}

impl ServiceSpec {
    fn validate_syntax(&self) -> io::Result<()> {
        if !valid_id(&self.id) {
            return Err(refused("service.invalid_identity"));
        }
        path_text(&self.executable)?;
        path_text(&self.state)?;
        let expected = if cfg!(windows) {
            "festerm-tunnel-setup.exe"
        } else {
            "festerm-tunnel-setup"
        };
        if self.executable.file_name() != Some(std::ffi::OsStr::new(expected))
            || self.state.file_name().is_none()
        {
            return Err(refused("service.invalid_path"));
        }
        Ok(())
    }

    fn validate(&self) -> io::Result<File> {
        ensure_unprivileged_user()?;
        self.validate_syntax()?;
        let state = setup_storage::private_directory(&self.state, false)
            .map_err(|_| refused("service.state_permissions"))?;
        let metadata = std::fs::symlink_metadata(&self.executable)
            .map_err(|_| refused("service.executable"))?;
        if !metadata.is_file() {
            return Err(refused("service.executable"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = festerm_unix_security::current_user_id()
                .map_err(|_| refused("service.user_context"))?;
            if metadata.mode() & 0o111 == 0
                || metadata.mode() & 0o6022 != 0
                || !matches!(metadata.uid(), 0) && metadata.uid() != uid
            {
                return Err(refused("service.executable"));
            }
            // Pin all ancestors too; the installer already verifies the binary.
            let parent = self
                .executable
                .parent()
                .ok_or_else(|| refused("service.executable"))?;
            trusted_directory(parent, false)?;
        }
        #[cfg(windows)]
        {
            let parent = self
                .executable
                .parent()
                .ok_or_else(|| refused("service.executable"))?;
            let directory =
                festerm_windows_security::lock_directory_path_without_delete_sharing(parent)
                    .map_err(|_| refused("service.executable"))?;
            if festerm_windows_security::secure_staging_parent(&directory)?.is_none() {
                return Err(refused("service.executable"));
            }
            festerm_windows_security::open_file_no_reparse(
                &directory,
                Path::new(
                    self.executable
                        .file_name()
                        .ok_or_else(|| refused("service.executable"))?,
                ),
            )
            .map_err(|_| refused("service.executable"))?;
        }
        Ok(state)
    }
}

#[cfg(any(target_os = "linux", test))]
fn systemd_word(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

/// `--serve` exits with this status when a retry cannot succeed without
/// operator action (stale daemon generation, changed account or identity).
/// Linux units exclude it from automatic restart.
pub const PERMANENT_REFUSAL_EXIT: i32 = 78;

#[cfg(any(target_os = "linux", test))]
fn linux_unit(spec: &ServiceSpec, unit: &str, restart: &str) -> io::Result<String> {
    spec.validate_syntax()?;
    Ok(format!(
        "[Unit]\nDescription=fesTerm dedicated Dev Tunnel {}\n{unit}\
         [Service]\nType=exec\nExecStart={} --serve --state-dir {}\n\
         {restart}KillMode=control-group\nStandardOutput=null\nStandardError=null\n\
         [Install]\nWantedBy=default.target\n",
        spec.id,
        systemd_word(path_text(&spec.executable)?),
        systemd_word(path_text(&spec.state)?),
    ))
}

/// Transient bridge/relay failures restart indefinitely at a fixed interval;
/// permanent refusals stop. The worker records each exit in its private log.
#[cfg(any(target_os = "linux", test))]
fn linux_definition(spec: &ServiceSpec) -> io::Result<String> {
    linux_unit(
        spec,
        "StartLimitIntervalSec=0\n",
        &format!("Restart=on-failure\nRestartSec=15\nRestartPreventExitStatus={PERMANENT_REFUSAL_EXIT}\n"),
    )
}

/// The exact definition earlier releases installed (no restart). Accepted only
/// by an explicit `--upgrade` ([`stop_and_replace`]), which also switches
/// `ExecStart` to helpers that implement [`PERMANENT_REFUSAL_EXIT`]. A plain
/// rerun keeps the old binary, so its unit must not gain restarts.
#[cfg(any(target_os = "linux", test))]
fn legacy_linux_definition(spec: &ServiceSpec) -> io::Result<String> {
    linux_unit(spec, "", "Restart=no\n")
}

/// The definition `install` requires: the current one, except that an existing
/// legacy definition is kept for a rerun that keeps older helpers;
/// [`migrate_legacy_definition`] switches it once the helpers are current.
#[cfg(all(unix, any(target_os = "linux", test)))]
fn kept_linux_definition(directory: &File, name: &Path, spec: &ServiceSpec) -> io::Result<String> {
    let current = linux_definition(spec)?;
    let legacy = legacy_linux_definition(spec)?;
    Ok(
        match owned_definition(directory, name, &[current.clone(), legacy.clone()]) {
            Ok(Some(existing)) if existing == legacy.as_bytes() => legacy,
            _ => current,
        },
    )
}

#[cfg(any(target_os = "linux", test))]
fn linux_loaded_matches(bytes: &[u8], path: &Path, allow_absent: bool) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused("service.systemd_inspect"))?;
    let property = |name| -> Option<&str> {
        let mut values = text.lines().filter_map(|line| line.strip_prefix(name));
        let first = values.next()?;
        if values.next().is_some() {
            None
        } else {
            Some(first)
        }
    };
    let load = property("LoadState=");
    let fragment = property("FragmentPath=");
    if property("DropInPaths=") != Some("")
        || !(load == Some("loaded") && fragment == Some(path_text(path)?)
            || allow_absent && load == Some("not-found") && fragment == Some(""))
    {
        return Err(refused("service.foreign_definition"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", windows, test))]
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "macos", test))]
fn mac_definition(spec: &ServiceSpec) -> io::Result<String> {
    spec.validate_syntax()?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>dev.fes.festerm.tunnel.{}</string>\n\
         <key>ProgramArguments</key><array><string>{}</string>\
         <string>--serve</string><string>--state-dir</string><string>{}</string></array>\n\
         <key>RunAtLoad</key><true/>\n<key>KeepAlive</key><false/>\n\
         <key>AbandonProcessGroup</key><false/>\n\
         <key>StandardOutPath</key><string>/dev/null</string>\n\
         <key>StandardErrorPath</key><string>/dev/null</string>\n\
         </dict></plist>\n",
        spec.id,
        xml(path_text(&spec.executable)?),
        xml(path_text(&spec.state)?),
    ))
}

#[cfg(any(windows, test))]
fn valid_sid(sid: &str) -> bool {
    let tail = sid
        .strip_prefix("S-1-5-21-")
        .or_else(|| sid.strip_prefix("S-1-12-1-"));
    sid.len() < 128
        && tail.is_some_and(|tail| {
            tail.split('-').count() == 4
                && tail
                    .split('-')
                    .all(|p| !p.is_empty() && p.parse::<u32>().is_ok())
        })
}

#[cfg(any(windows, test))]
fn windows_argument(value: &str) -> io::Result<String> {
    if value.contains('"') || value.chars().any(char::is_control) || value.matches('%').count() > 1
    {
        return Err(refused("service.invalid_path"));
    }
    // CommandLineToArgvW doubles a trailing backslash before a closing quote.
    let trailing = value.chars().rev().take_while(|c| *c == '\\').count();
    Ok(format!("\"{}{}\"", value, "\\".repeat(trailing)))
}

#[cfg(any(windows, test))]
fn windows_definition(spec: &ServiceSpec, sid: &str) -> io::Result<String> {
    spec.validate_syntax()?;
    if !valid_sid(sid) {
        return Err(refused("service.user_context"));
    }
    let executable = path_text(&spec.executable)?;
    // Scheduler expands paired %NAME% references even without a shell. There
    // is no documented literal-percent escape, so reject ambiguous paths.
    if executable.contains('"') || executable.matches('%').count() > 1 {
        return Err(refused("service.invalid_path"));
    }
    let arguments = format!(
        "--serve --state-dir {}",
        windows_argument(path_text(&spec.state)?)?
    );
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\n\
         <Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n\
         <RegistrationInfo><URI>\\fesTerm-DevTunnel-{}</URI></RegistrationInfo>\n\
         <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId>\
         </LogonTrigger></Triggers>\n\
         <Principals><Principal id=\"User\"><UserId>{sid}</UserId>\
         <LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>\
         </Principal></Principals>\n\
         <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>\
         <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>\
         <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>\
         <AllowHardTerminate>false</AllowHardTerminate>\
         <StartWhenAvailable>false</StartWhenAvailable>\
         <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>\
         <Enabled>true</Enabled><ExecutionTimeLimit>PT0S</ExecutionTimeLimit></Settings>\n\
         <Actions Context=\"User\"><Exec><Command>{}</Command>\
         <Arguments>{}</Arguments></Exec></Actions>\n</Task>\n",
        spec.id,
        xml(executable),
        xml(&arguments),
    ))
}

#[cfg(any(windows, test))]
fn windows_task_bytes(definition: &str) -> Vec<u8> {
    // schtasks rejects declared UTF-8 and misdecodes undeclared UTF-8.
    [0xff, 0xfe]
        .into_iter()
        .chain(definition.encode_utf16().flat_map(u16::to_le_bytes))
        .collect()
}

struct NativeOutput {
    success: bool,
    bytes: Vec<u8>,
}

// A private runtime on a dedicated thread also works when called from the
// installer's async runtime. Neither stderr nor command output reaches logs.
fn native(tool: &Path, args: &[OsString], stage: &'static str) -> io::Result<NativeOutput> {
    native_bounded(tool, args, stage, DEADLINE, OUTPUT_LIMIT)
}

fn native_bounded(
    tool: &Path,
    args: &[OsString],
    stage: &'static str,
    deadline: Duration,
    output_limit: usize,
) -> io::Result<NativeOutput> {
    let tool = tool.to_owned();
    let args = args.to_owned();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| refused(stage))?;
        runtime.block_on(async move {
            use tokio::io::AsyncReadExt;
            let mut command = tokio::process::Command::new(tool);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .env_remove("SYSTEMD_UNIT_PATH")
                .env("SYSTEMD_PAGER", "")
                .env("SYSTEMD_COLORS", "0");
            let mut child = command.spawn().map_err(|_| refused(stage))?;
            let mut stdout = child.stdout.take().ok_or_else(|| refused(stage))?;
            let result = tokio::time::timeout(deadline, async {
                let mut bytes = Vec::new();
                (&mut stdout)
                    .take(output_limit as u64 + 1)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| refused(stage))?;
                if bytes.len() > output_limit {
                    return Err(refused(stage));
                }
                let status = child.wait().await.map_err(|_| refused(stage))?;
                Ok(NativeOutput {
                    success: status.success(),
                    bytes,
                })
            })
            .await;
            if !matches!(result, Ok(Ok(_))) {
                let _ = child.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            }
            result.map_err(|_| refused(stage))?
        })
    })
    .join()
    .map_err(|_| refused(stage))?
}

fn run(tool: &Path, args: &[OsString], stage: &'static str) -> io::Result<Vec<u8>> {
    let result = native(tool, args, stage)?;
    if result.success {
        Ok(result.bytes)
    } else {
        Err(refused(stage))
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn exact_file(file: File, expected: &[u8]) -> io::Result<()> {
    if file.metadata()?.len() > OUTPUT_LIMIT as u64 {
        return Err(refused("service.foreign_definition"));
    }
    let mut existing = Vec::new();
    file.take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut existing)?;
    if existing != expected {
        return Err(refused("service.foreign_definition"));
    }
    Ok(())
}

#[cfg(unix)]
fn check_trusted_directory(directory: &File, path: &Path) -> io::Result<()> {
    festerm_unix_security::trusted_private_storage_ancestor(directory).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            io::Error::new(
                error.kind(),
                format!(
                    "service.directory_permissions: {} requires root/current-user ownership and no group/world write; existing permissions were not changed",
                    path.display()
                ),
            )
        } else {
            error
        }
    })
}

#[cfg(unix)]
fn trusted_directory(path: &Path, create: bool) -> io::Result<File> {
    path_text(path)?;
    let mut directory = File::open("/")?;
    let mut prefix = PathBuf::from("/");
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(refused("service.registration_directory"));
        };
        check_trusted_directory(&directory, &prefix)?;
        directory = match festerm_unix_security::open_directory_read_nofollow(
            &directory,
            Path::new(name),
        ) {
            Ok(directory) => directory,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                festerm_unix_security::create_private_directory(&directory, Path::new(name))?
            }
            Err(error) => return Err(error),
        };
        prefix.push(name);
    }
    check_trusted_directory(&directory, &prefix)?;
    Ok(directory)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn registration_directory_path() -> io::Result<PathBuf> {
    let home =
        festerm_unix_security::current_user_home().map_err(|_| refused("service.user_context"))?;
    #[cfg(target_os = "linux")]
    let directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"))
        .join("systemd/user");
    #[cfg(target_os = "macos")]
    let directory = home.join("Library/LaunchAgents");
    path_text(&directory)?;
    Ok(directory)
}

#[cfg(unix)]
fn preflight_directory(path: &Path) -> io::Result<()> {
    path_text(path)?;
    let mut existing = path.to_path_buf();
    loop {
        match trusted_directory(&existing, false) {
            Ok(directory) => {
                return festerm_unix_security::writable_directory(&directory).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "service.directory_not_writable: {} requires current-user write/search access; existing permissions were not changed",
                            existing.display()
                        ),
                    )
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if !existing.pop() {
                    return Err(refused("service.registration_directory"));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn preflight_registration_directory() -> io::Result<()> {
    preflight_directory(&registration_directory_path()?)
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) fn preflight_service_manager() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    run(
        Path::new("/usr/bin/systemctl"),
        &args(&["--user", "--no-pager", "show", "--property=Version"]),
        "service.systemd_user_manager_required",
    )?;
    #[cfg(target_os = "macos")]
    {
        let uid = festerm_unix_security::current_user_id()
            .map_err(|_| refused("service.user_context"))?;
        run(
            Path::new("/bin/launchctl"),
            &args(&["print-disabled", &format!("gui/{uid}")]),
            "service.gui_login_required",
        )?;
    }
    #[cfg(windows)]
    {
        let (_, system) = festerm_windows_security::current_user_service_context()
            .map_err(|_| refused("service.user_context"))?;
        run(
            &system.join("WindowsPowerShell/v1.0/powershell.exe"),
            &args(&[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$ErrorActionPreference='Stop'; \
                 $s=[Activator]::CreateInstance([type]::GetTypeFromProgID('Schedule.Service')); \
                 $s.Connect(); $null=$s.GetFolder('\\')",
            ]),
            "service.task_scheduler_required",
        )?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(super) fn preflight_service_manager() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "service.unsupported_platform",
    ))
}

#[cfg(unix)]
fn definition_file(directory: &File, name: &Path, expected: &[u8], create: bool) -> io::Result<()> {
    let open = |new| {
        let file = festerm_unix_security::open_private_file(directory, name, new)?;
        if !festerm_unix_security::is_current_user_only(&file, false)? {
            return Err(refused("service.definition_permissions"));
        }
        Ok(file)
    };
    match if create { open(true) } else { open(false) } {
        Ok(mut file) if create => {
            file.write_all(expected)?;
            file.sync_all()
        }
        Ok(file) => exact_file(file, expected),
        Err(error) if create && error.kind() == io::ErrorKind::AlreadyExists => {
            exact_file(open(false)?, expected)
        }
        Err(error) => Err(error),
    }
}

/// Installs only this UUID's service and starts it without stopping any process.
/// A preexisting definition is accepted only when its complete content matches.
pub fn install_and_start(spec: &ServiceSpec) -> io::Result<()> {
    install(spec, true)
}

/// Starts an exactly matching, already installed service. Does not repair or
/// replace an unknown definition; the operator must explicitly resolve it.
#[allow(dead_code)]
pub fn start_existing(spec: &ServiceSpec) -> io::Result<()> {
    install(spec, false)
}

/// Explicit upgrade: stops only this UUID's worker (and its bridge child) and
/// rewrites its definition for `spec`'s executable without starting it. The
/// existing definition must be exactly one of `previous` (or already `spec`);
/// any other content, identity or state directory still refuses. The daemon
/// session is not part of the service and keeps running.
pub fn stop_and_replace(previous: &[ServiceSpec], spec: &ServiceSpec) -> io::Result<()> {
    spec.validate_syntax()?;
    for prior in previous {
        prior.validate_syntax()?;
        if prior.id != spec.id || prior.state != spec.state {
            return Err(refused("service.foreign_definition"));
        }
    }
    replace(previous, spec)
}

/// Rewrites this UUID's pre-restart Linux definition and reloads systemd when
/// `spec`'s helpers are this release's, which implement
/// [`PERMANENT_REFUSAL_EXIT`]. A running worker keeps running; the reload
/// applies the restart policy to it. Returns whether a definition changed. Any
/// other definition, including a foreign one, is left for `install` to judge.
#[cfg(target_os = "linux")]
pub fn migrate_legacy_definition(spec: &ServiceSpec) -> io::Result<bool> {
    let _state = spec.validate()?;
    let directory_path = registration_directory_path()?;
    let Ok(directory) = trusted_directory(&directory_path, false) else {
        return Ok(false);
    };
    let name = format!("festerm-tunnel-{}.service", spec.id);
    let legacy = legacy_linux_definition(spec)?;
    if !matches!(
        owned_definition(&directory, Path::new(&name), std::slice::from_ref(&legacy)),
        Ok(Some(_))
    ) {
        return Ok(false);
    }
    let tool = Path::new("/usr/bin/systemctl");
    let shown = native(
        tool,
        &args(&[
            "--user",
            "--no-pager",
            "show",
            &name,
            "--property=LoadState,FragmentPath,DropInPaths",
        ]),
        "service.systemd_inspect",
    )?
    .bytes;
    linux_loaded_matches(&shown, &directory_path.join(&name), true)?;
    publish_definition(
        &directory,
        Path::new(&name),
        linux_definition(spec)?.as_bytes(),
    )?;
    run(
        tool,
        &args(&["--user", "daemon-reload"]),
        "service.systemd_reload",
    )?;
    Ok(true)
}

#[cfg(not(target_os = "linux"))]
pub fn migrate_legacy_definition(_spec: &ServiceSpec) -> io::Result<bool> {
    Ok(false)
}

/// Reads this UUID's current definition and requires it to be one we own.
/// Returns `None` only when no definition exists.
#[cfg(unix)]
fn owned_definition(
    directory: &File,
    name: &Path,
    accepted: &[String],
) -> io::Result<Option<Vec<u8>>> {
    let file = match festerm_unix_security::open_private_file(directory, name, false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !festerm_unix_security::is_current_user_only(&file, false)? {
        return Err(refused("service.definition_permissions"));
    }
    owned_bytes(file, accepted).map(Some)
}

fn owned_bytes(file: File, accepted: &[impl AsRef<[u8]>]) -> io::Result<Vec<u8>> {
    if file.metadata()?.len() > OUTPUT_LIMIT as u64 {
        return Err(refused("service.foreign_definition"));
    }
    let mut existing = Vec::new();
    file.take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut existing)?;
    if !accepted
        .iter()
        .any(|definition| definition.as_ref() == existing)
    {
        return Err(refused("service.foreign_definition"));
    }
    Ok(existing)
}

/// Atomically publishes `expected` over an owned definition already verified
/// by [`owned_definition`]; a failed publication leaves the original in place.
#[cfg(unix)]
fn publish_definition(directory: &File, name: &Path, expected: &[u8]) -> io::Result<()> {
    let staging = PathBuf::from(format!(
        ".{}.upgrade-{}",
        name.display(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut file = festerm_unix_security::open_private_file(directory, &staging, true)?;
    let result = (|| {
        file.write_all(expected)?;
        file.sync_all()?;
        festerm_unix_security::replace_private_file(directory, &staging, name)?;
        directory.sync_all()
    })();
    if result.is_err() {
        let _ = festerm_unix_security::remove_private_staging_file(directory, &staging);
    }
    result
}

#[cfg(target_os = "linux")]
fn replace(previous: &[ServiceSpec], spec: &ServiceSpec) -> io::Result<()> {
    let _state = spec.validate()?;
    let directory_path = registration_directory_path()?;
    let directory = trusted_directory(&directory_path, false)?;
    let name = format!("festerm-tunnel-{}.service", spec.id);
    let path = directory_path.join(&name);
    let expected = linux_definition(spec)?;
    let accepted = previous
        .iter()
        .chain([spec])
        .flat_map(|owned| [linux_definition(owned), legacy_linux_definition(owned)])
        .collect::<io::Result<Vec<_>>>()?;
    let tool = Path::new("/usr/bin/systemctl");
    let shown = native(
        tool,
        &args(&[
            "--user",
            "--no-pager",
            "show",
            &name,
            "--property=LoadState,FragmentPath,DropInPaths",
        ]),
        "service.systemd_inspect",
    )?
    .bytes;
    linux_loaded_matches(&shown, &path, true)?;
    let Some(existing) = owned_definition(&directory, Path::new(&name), &accepted)? else {
        // Never registered: nothing runs, and a normal install creates it.
        return linux_loaded_matches(&shown, &path, false)
            .map_or(Ok(()), |()| Err(refused("service.foreign_definition")));
    };
    // KillMode=control-group stops the worker and its bridge child only.
    let _ = native(
        tool,
        &args(&["--user", "stop", &name]),
        "service.systemd_stop",
    )?;
    if native(
        tool,
        &args(&["--user", "is-active", "--quiet", &name]),
        "service.systemd_stop",
    )?
    .success
    {
        return Err(refused("service.systemd_stop"));
    }
    if existing != expected.as_bytes() {
        publish_definition(&directory, Path::new(&name), expected.as_bytes())?;
    }
    run(
        tool,
        &args(&["--user", "daemon-reload"]),
        "service.systemd_reload",
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn replace(previous: &[ServiceSpec], spec: &ServiceSpec) -> io::Result<()> {
    let _state = spec.validate()?;
    let uid =
        festerm_unix_security::current_user_id().map_err(|_| refused("service.user_context"))?;
    let directory_path = registration_directory_path()?;
    let directory = trusted_directory(&directory_path, false)?;
    let label = format!("dev.fes.festerm.tunnel.{}", spec.id);
    let name = format!("{label}.plist");
    let path = directory_path.join(&name);
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let tool = Path::new("/bin/launchctl");
    run(
        tool,
        &args(&["print-disabled", &domain]),
        "service.gui_login_required",
    )?;
    let expected = mac_definition(spec)?;
    let accepted = previous
        .iter()
        .map(mac_definition)
        .chain([Ok(expected.clone())])
        .collect::<io::Result<Vec<_>>>()?;
    let existing = owned_definition(&directory, Path::new(&name), &accepted)?;
    let loaded = native(tool, &args(&["print", &target]), "service.launchd_inspect")?;
    if loaded.success {
        if existing.is_none()
            || !previous
                .iter()
                .chain([spec])
                .any(|owned| mac_loaded_matches(&loaded.bytes, &path, owned).is_ok())
        {
            return Err(refused("service.foreign_definition"));
        }
        // bootout terminates the worker's process group (its bridge child);
        // the daemon was started outside the service and is not in it.
        run(tool, &args(&["bootout", &target]), "service.launchd_stop")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while native(tool, &args(&["print", &target]), "service.launchd_inspect")?.success {
            if std::time::Instant::now() > deadline {
                return Err(refused("service.launchd_stop"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    match existing {
        Some(existing) if existing != expected.as_bytes() => {
            publish_definition(&directory, Path::new(&name), expected.as_bytes())
        }
        _ => Ok(()),
    }
}

#[cfg(any(windows, test))]
fn powershell_literal(value: &str) -> io::Result<String> {
    // PowerShell also treats typographic single quotes as delimiters.
    if value
        .chars()
        .any(|c| matches!(c, '\u{2018}'..='\u{201B}') || c.is_control())
    {
        return Err(refused("service.invalid_path"));
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

/// Fixed script that terminates only this state directory's `--serve` worker
/// and its bridge child running one of the owned helper images. Never the
/// daemon (a different image) and never the calling process.
#[cfg(any(windows, test))]
fn windows_stop_script(owned: &[&ServiceSpec], state: &Path, own_pid: u32) -> io::Result<String> {
    let mut images = Vec::new();
    for spec in owned {
        let directory = spec
            .executable
            .parent()
            .ok_or_else(|| refused("service.invalid_path"))?;
        images.push(powershell_literal(path_text(&spec.executable)?)?);
        images.push(powershell_literal(path_text(
            &directory.join("festerm-tunnel-bridge.exe"),
        )?)?);
    }
    Ok(format!(
        "$ErrorActionPreference='Stop'; $state={}; $images=@({}); \
         Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne {own_pid} -and \
         $_.ExecutablePath -and ($images -contains $_.ExecutablePath) -and $_.CommandLine -and \
         $_.CommandLine.Contains($state) -and ($_.CommandLine.Contains(' --serve ') -or \
         $_.CommandLine.Contains('--existing-host-identity')) }} | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
        powershell_literal(path_text(state)?)?,
        images.join(","),
    ))
}

#[cfg(windows)]
fn replace(previous: &[ServiceSpec], spec: &ServiceSpec) -> io::Result<()> {
    let state = spec.validate()?;
    let (sid, system) = festerm_windows_security::current_user_service_context()
        .map_err(|_| refused("service.user_context"))?;
    let expected = windows_task_bytes(&windows_definition(spec, &sid)?);
    let accepted = previous
        .iter()
        .chain([spec])
        .map(|prior| {
            let definition = windows_definition(prior, &sid)?;
            Ok([
                windows_task_bytes(&definition),
                definition
                    .replacen("encoding=\"UTF-16\"", "encoding=\"UTF-8\"", 1)
                    .into_bytes(),
            ])
        })
        .collect::<io::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let definition_name = Path::new("service-task.xml");
    let snapshot_name = Path::new("service-task-owned.xml");
    let tool = system.join("schtasks.exe");
    let name = format!("fesTerm-DevTunnel-{}", spec.id);
    let existing = match setup_storage::open_private_file(&state, definition_name, false) {
        Ok(file) => Some(owned_bytes(file, &accepted)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => return Err(refused("service.foreign_definition")),
    };
    let current = native(
        &tool,
        &args(&["/Query", "/TN", &name, "/XML"]),
        "service.task_inspect",
    )?;
    if current.success {
        if existing.is_none() {
            return Err(refused("service.foreign_definition"));
        }
        exact_file(
            setup_storage::open_private_file(&state, snapshot_name, false)
                .map_err(|_| refused("service.foreign_definition"))?,
            &current.bytes,
        )?;
        // AllowHardTerminate=false means the Scheduler cannot stop the task,
        // so terminate exactly this installation's worker and bridge.
        let owned: Vec<_> = previous.iter().chain([spec]).collect();
        run(
            &system.join("WindowsPowerShell/v1.0/powershell.exe"),
            &[
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from(windows_stop_script(
                    &owned,
                    &spec.state,
                    std::process::id(),
                )?),
            ],
            "service.task_stop",
        )?;
        run(
            &tool,
            &args(&["/Delete", "/TN", &name, "/F"]),
            "service.task_delete",
        )?;
    }
    match std::fs::remove_file(spec.state.join(snapshot_name)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match existing {
        Some(existing) if existing != expected => {
            setup_storage::replace_private(&state, definition_name, &expected)
        }
        _ => Ok(()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn replace(_previous: &[ServiceSpec], _spec: &ServiceSpec) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "service.unsupported_platform",
    ))
}

#[cfg(target_os = "linux")]
fn install(spec: &ServiceSpec, create: bool) -> io::Result<()> {
    let _state = spec.validate()?;
    let directory_path = registration_directory_path()?;
    let directory = trusted_directory(&directory_path, create)?;
    let name = format!("festerm-tunnel-{}.service", spec.id);
    let path = directory_path.join(&name);
    let tool = Path::new("/usr/bin/systemctl");
    let show = || {
        // Some systemd releases return a nonzero status while still reporting
        // LoadState=not-found. Only the complete validated properties matter.
        native(
            tool,
            &args(&[
                "--user",
                "--no-pager",
                "show",
                &name,
                "--property=LoadState,FragmentPath,DropInPaths",
            ]),
            "service.systemd_inspect",
        )
        .map(|result| result.bytes)
    };
    linux_loaded_matches(&show()?, &path, create)?;
    let definition = kept_linux_definition(&directory, Path::new(&name), spec)?;
    definition_file(&directory, Path::new(&name), definition.as_bytes(), create)?;
    if create {
        run(
            tool,
            &args(&["--user", "daemon-reload"]),
            "service.systemd_reload",
        )?;
    }
    linux_loaded_matches(&show()?, &path, false)?;
    if create {
        run(
            tool,
            &args(&["--user", "enable", &name]),
            "service.systemd_enable",
        )?;
    }
    run(
        tool,
        &args(&["--user", "start", &name]),
        "service.systemd_start",
    )?;
    std::thread::sleep(Duration::from_millis(500));
    run(
        tool,
        &args(&["--user", "is-active", "--quiet", &name]),
        "service.worker_not_running",
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn install(spec: &ServiceSpec, create: bool) -> io::Result<()> {
    let _state = spec.validate()?;
    let uid =
        festerm_unix_security::current_user_id().map_err(|_| refused("service.user_context"))?;
    let directory_path = registration_directory_path()?;
    let directory = trusted_directory(&directory_path, create)?;
    let label = format!("dev.fes.festerm.tunnel.{}", spec.id);
    let name = format!("{label}.plist");
    let path = directory_path.join(&name);
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{label}");
    let tool = Path::new("/bin/launchctl");
    // Require a live GUI login domain, never fall back to a root/system daemon.
    run(
        tool,
        &args(&["print-disabled", &domain]),
        "service.gui_login_required",
    )?;
    let inspect = || native(tool, &args(&["print", &target]), "service.launchd_inspect");
    let loaded = inspect()?;
    if loaded.success {
        mac_loaded_matches(&loaded.bytes, &path, spec)?;
        definition_file(
            &directory,
            Path::new(&name),
            mac_definition(spec)?.as_bytes(),
            false,
        )?;
    } else {
        definition_file(
            &directory,
            Path::new(&name),
            mac_definition(spec)?.as_bytes(),
            create,
        )?;
        run(
            tool,
            &[
                OsString::from("bootstrap"),
                OsString::from(&domain),
                path.as_os_str().to_owned(),
            ],
            "service.launchd_bootstrap",
        )?;
    }
    if loaded.success {
        // No -k: an already running worker (and its exported session) is untouched.
        run(
            tool,
            &args(&["kickstart", &target]),
            "service.launchd_start",
        )?;
    }
    std::thread::sleep(Duration::from_millis(500));
    let status = run(tool, &args(&["print", &target]), "service.launchd_inspect")?;
    mac_loaded_matches(&status, &path, spec)?;
    if !status
        .split(|b| *b == b'\n')
        .any(|line| std::str::from_utf8(line).is_ok_and(|s| s.trim() == "state = running"))
    {
        return Err(refused("service.worker_not_running"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn mac_loaded_matches(bytes: &[u8], path: &Path, spec: &ServiceSpec) -> io::Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| refused("service.foreign_definition"))?;
    let lines: Vec<_> = text.lines().map(str::trim_start).collect();
    let program = format!("program = {}", path_text(&spec.executable)?);
    let path = format!("path = {}", path_text(path)?);
    let arguments = [
        path_text(&spec.executable)?,
        "--serve",
        "--state-dir",
        path_text(&spec.state)?,
    ];
    let matches_arguments = lines
        .windows(6)
        .any(|lines| lines[0] == "arguments = {" && lines[1..5] == arguments && lines[5] == "}");
    if !lines.contains(&path.as_str()) || !lines.contains(&program.as_str()) || !matches_arguments {
        return Err(refused("service.foreign_definition"));
    }
    Ok(())
}

#[cfg(windows)]
fn install(spec: &ServiceSpec, create: bool) -> io::Result<()> {
    let state = spec.validate()?;
    let (sid, system) = festerm_windows_security::current_user_service_context()
        .map_err(|_| refused("service.user_context"))?;
    let definition = windows_task_bytes(&windows_definition(spec, &sid)?);
    let definition_name = Path::new("service-task.xml");
    let snapshot_name = Path::new("service-task-owned.xml");
    let tool = system.join("schtasks.exe");
    let name = format!("fesTerm-DevTunnel-{}", spec.id);
    let query = || {
        native(
            &tool,
            &args(&["/Query", "/TN", &name, "/XML"]),
            "service.task_inspect",
        )
    };
    let current = query()?;
    if current.success {
        exact_file(
            setup_storage::open_private_file(&state, definition_name, false)
                .map_err(|_| refused("service.foreign_definition"))?,
            &definition,
        )?;
        exact_file(
            setup_storage::open_private_file(&state, snapshot_name, false)
                .map_err(|_| refused("service.foreign_definition"))?,
            &current.bytes,
        )?;
    } else {
        if !create {
            return Err(refused("service.task_missing"));
        }
        match setup_storage::open_private_file(&state, definition_name, true) {
            Ok(mut file) => {
                file.write_all(&definition)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                exact_file(
                    setup_storage::open_private_file(&state, definition_name, false)?,
                    &definition,
                )?;
            }
            Err(error) => return Err(error),
        }
        // No /F: even an inspection failure or a concurrent creator can never
        // cause an existing task to be overwritten.
        run(
            &tool,
            &[
                OsString::from("/Create"),
                OsString::from("/TN"),
                OsString::from(&name),
                OsString::from("/XML"),
                spec.state.join(definition_name).into_os_string(),
            ],
            "service.task_create",
        )?;
        let registered = run(
            &tool,
            &args(&["/Query", "/TN", &name, "/XML"]),
            "service.task_inspect",
        )?;
        // Scheduler normalizes XML. Pin its complete returned representation,
        // not a subset of security-sensitive fields.
        let mut snapshot = setup_storage::open_private_file(&state, snapshot_name, true)
            .map_err(|_| refused("service.foreign_definition"))?;
        snapshot.write_all(&registered)?;
        snapshot.sync_all()?;
    }
    run(&tool, &args(&["/Run", "/TN", &name]), "service.task_start")?;
    std::thread::sleep(Duration::from_millis(500));
    // Task state is localized in schtasks output. Query the native Scheduler
    // COM API with fixed code; only the validated UUID is substituted.
    let expression = format!(
        "$ErrorActionPreference='Stop'; \
         $s=[Activator]::CreateInstance([type]::GetTypeFromProgID('Schedule.Service')); \
         $s.Connect(); $t=$s.GetFolder('\\').GetTask('{name}'); \
         if($t.State -ne 4){{exit 1}}"
    );
    run(
        &system.join("WindowsPowerShell/v1.0/powershell.exe"),
        &[
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(expression),
        ],
        "service.worker_not_running",
    )?;
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn install(_spec: &ServiceSpec, _create: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "service.unsupported_platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec {
            id: "c17273e0-a8a3-4fce-b3cd-49f112d8b15c".into(),
            executable: if cfg!(windows) {
                PathBuf::from("C:\\Users\\test\\festerm-tunnel-setup.exe")
            } else {
                PathBuf::from("/Users/test/bin/festerm-tunnel-setup")
            },
            state: if cfg!(windows) {
                PathBuf::from("C:\\Users\\test\\state folder & été%")
            } else {
                PathBuf::from("/Users/test/state folder & été%")
            },
        }
    }

    #[test]
    fn service_definitions_are_deterministic_scoped_and_have_no_secrets() {
        let spec = spec();
        let linux = linux_definition(&spec).unwrap();
        let mac = mac_definition(&spec).unwrap();
        let windows = windows_definition(&spec, "S-1-5-21-123-456-789-1001").unwrap();
        assert_eq!(linux, linux_definition(&spec).unwrap());
        assert_eq!(mac, mac_definition(&spec).unwrap());
        assert_eq!(
            windows,
            windows_definition(&spec, "S-1-5-21-123-456-789-1001").unwrap()
        );
        assert!(linux.contains("Restart=on-failure\nRestartSec=15\n"));
        assert!(linux.contains("RestartPreventExitStatus=78\n"));
        assert!(linux.contains("StartLimitIntervalSec=0\n"));
        assert!(!linux.contains("Restart=no"));
        assert!(linux.contains("WantedBy=default.target"));
        assert!(mac.contains("<key>RunAtLoad</key><true/>"));
        assert!(mac.contains("<key>KeepAlive</key><false/>"));
        assert!(windows.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(windows.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(!windows.contains("RestartOnFailure"));
        for definition in [&linux, &mac, &windows] {
            for forbidden in [
                "token",
                "--anonymous",
                "--config",
                "--bridge",
                "SYSTEM",
                "HighestAvailable",
                "Password",
                "rp5-1313",
            ] {
                assert!(!definition.contains(forbidden), "{forbidden}");
            }
            assert!(!definition.contains("--state-dir --"));
        }
    }

    #[test]
    fn service_linux_legacy_definition_is_byte_exact_for_migration() {
        let spec = spec();
        let unit = |restart: &str| {
            format!(
                "[Unit]\nDescription=fesTerm dedicated Dev Tunnel {}\n[Service]\nType=exec\n\
                 ExecStart={} --serve --state-dir {}\n{restart}KillMode=control-group\n\
                 StandardOutput=null\nStandardError=null\n[Install]\nWantedBy=default.target\n",
                spec.id,
                systemd_word(spec.executable.to_str().unwrap()),
                systemd_word(spec.state.to_str().unwrap()),
            )
        };
        assert_eq!(
            legacy_linux_definition(&spec).unwrap(),
            unit("Restart=no\n")
        );
        assert_ne!(
            legacy_linux_definition(&spec).unwrap(),
            linux_definition(&spec).unwrap()
        );
    }

    #[test]
    fn service_definitions_own_the_complete_worker_process_group() {
        let linux = linux_definition(&spec()).unwrap();
        let mac = mac_definition(&spec()).unwrap();
        assert!(linux.contains("KillMode=control-group\n"));
        assert!(!linux.contains("KillMode=process\n"));
        assert!(mac.contains("<key>AbandonProcessGroup</key><false/>"));
        assert!(!mac.contains("<key>AbandonProcessGroup</key><true/>"));
        assert!(!linux.contains("ExecStop="));
        assert!(!linux.contains("sessiond"));
        assert!(!mac.contains("sessiond"));
    }

    #[test]
    fn service_windows_task_file_preserves_unicode_in_scheduler_encoding() {
        let mut spec = spec();
        spec.state.push("terminal-\u{1f980}");
        let definition = windows_definition(&spec, "S-1-5-21-123-456-789-1001").unwrap();
        let bytes = windows_task_bytes(&definition);
        assert!(bytes.starts_with(&[0xff, 0xfe]));
        let (pairs, remainder) = bytes[2..].as_chunks::<2>();
        assert!(remainder.is_empty());
        let units: Vec<_> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        let decoded = String::from_utf16(&units).unwrap();
        assert_eq!(decoded, definition);
        assert!(decoded.starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
        assert!(decoded.contains("folder &amp; été%"));
        assert!(decoded.contains("terminal-\u{1f980}"));
    }

    #[test]
    fn service_escaping_preserves_literal_paths() {
        assert_eq!(
            systemd_word("/a %n $HOME \\\" été"),
            "\"/a %%n $$HOME \\\\\\\" été\""
        );
        assert_eq!(xml("é&<>\"'"), "é&amp;&lt;&gt;&quot;&apos;");
        assert_eq!(
            windows_argument("C:\\space dir\\").unwrap(),
            "\"C:\\space dir\\\\\""
        );
        assert!(windows_argument("C:\\bad\"path").is_err());
        assert!(windows_argument("C:\\%USERNAME%\\state").is_err());
        assert!(windows_argument("C:\\two%literal%marks").is_err());
        assert!(valid_sid("S-1-12-1-123-456-789-1001"));
        let mac = mac_definition(&spec()).unwrap();
        assert!(mac.contains("folder &amp; été%"));
    }

    #[test]
    fn service_rejects_noncanonical_identity_and_path_injections() {
        let mut spec = spec();
        for id in [
            "",
            "../../foreign",
            "devtunnel-fes-rp5-1313",
            "00000000-0000-0000-0000-000000000000",
            "C17273e0-a8a3-4fce-b3cd-49f112d8b15c",
            "c17273e0-a8a3-4fce-b3cd-49f112d8b15c\n",
        ] {
            spec.id = id.into();
            assert!(spec.validate_syntax().is_err(), "{id:?}");
        }
        spec.id = "c17273e0-a8a3-4fce-b3cd-49f112d8b15c".into();
        for state in [
            "relative",
            "/absolute/\n[Service]",
            "/absolute/\0bad",
            "/absolute/../elsewhere",
            "/absolute/./elsewhere",
        ] {
            spec.state = PathBuf::from(state);
            assert!(spec.validate_syntax().is_err(), "{state:?}");
        }
        spec.state = PathBuf::from(if cfg!(windows) { "C:\\state" } else { "/state" });
        spec.executable = spec.state.join("other-executable");
        assert!(spec.validate_syntax().is_err());
        for sid in ["S-1-5-18", "S-1-5-19", "S-1-5-20", "S-1-5-21-1-2-3-4'"] {
            assert!(!valid_sid(sid));
            assert!(windows_definition(&spec, sid).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn service_directory_preflight_refuses_unsafe_ancestors_without_repair_or_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = std::env::current_dir().unwrap().join(format!(
            ".service-preflight-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        drop(setup_storage::private_directory(&root, true).unwrap());
        let config = root.join("config");
        let directory = config.join("systemd/user");
        preflight_directory(&directory).unwrap();
        assert!(!config.exists());
        for path in [&config, &config.join("systemd"), &directory] {
            drop(setup_storage::private_directory(path, true).unwrap());
        }
        for path in [&config, &config.join("systemd"), &directory] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o775)).unwrap();
            let error = preflight_directory(&directory).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("service.directory_permissions"));
            assert!(error.to_string().contains(&path.display().to_string()));
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o775
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        preflight_directory(&directory).unwrap();
        for path in [&config, &config.join("systemd"), &directory] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
            let target = if path == &directory {
                directory.clone()
            } else {
                path.join("missing/user")
            };
            let error = preflight_directory(&target).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
            assert!(error.to_string().contains("service.directory_not_writable"));
            assert!(error.to_string().contains(&path.display().to_string()));
            assert!(!path.join("missing").exists());
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o555
            );
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let link = root.join("link");
        symlink(&config, &link).unwrap();
        assert!(preflight_directory(&link.join("missing/user")).is_err());
        assert!(!config.join("missing").exists());
        assert!(preflight_directory(&root.join("missing/../config")).is_err());
        assert!(preflight_directory(Path::new("relative/missing")).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(&directory).unwrap();
        std::fs::remove_dir(config.join("systemd")).unwrap();
        std::fs::remove_dir(config).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_definition_collision_refuses_and_identical_install_is_idempotent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = std::env::current_dir().unwrap().join(format!(
            ".service-install-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let directory = setup_storage::private_directory(&root, true).unwrap();
        let name = Path::new("owned.service");
        definition_file(&directory, name, b"exact definition", true).unwrap();
        definition_file(&directory, name, b"exact definition", true).unwrap();
        definition_file(&directory, name, b"exact definition", false).unwrap();
        let unrelated = root.join("original.service");
        std::fs::write(&unrelated, b"unrelated untouched").unwrap();
        assert!(definition_file(&directory, name, b"foreign definition", true).is_err());
        assert!(definition_file(&directory, Path::new("missing"), b"x", false).is_err());
        definition_file(&directory, name, b"exact definition", false).unwrap();
        symlink(root.join(name), root.join("link.service")).unwrap();
        assert!(definition_file(&directory, Path::new("link.service"), b"replace", true).is_err());
        std::fs::remove_file(root.join("link.service")).unwrap();
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(definition_file(&directory, name, b"exact definition", true).is_err());
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"unrelated untouched");
        assert_eq!(std::fs::read(root.join(name)).unwrap(), b"exact definition");
        std::fs::remove_file(root.join(name)).unwrap();
        std::fs::remove_file(unrelated).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_upgrade_replaces_only_an_owned_definition_atomically() {
        let root = std::env::current_dir().unwrap().join(format!(
            ".service-upgrade-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let directory = setup_storage::private_directory(&root, true).unwrap();
        let name = Path::new("owned.service");
        let accepted = ["old definition".to_owned(), "new definition".to_owned()];
        assert!(owned_definition(&directory, name, &accepted)
            .unwrap()
            .is_none());
        definition_file(&directory, name, b"old definition", true).unwrap();
        let existing = owned_definition(&directory, name, &accepted)
            .unwrap()
            .unwrap();
        assert_eq!(existing, b"old definition");
        publish_definition(&directory, name, b"new definition").unwrap();
        assert_eq!(std::fs::read(root.join(name)).unwrap(), b"new definition");
        // The strict create-only check now accepts the replaced definition.
        definition_file(&directory, name, b"new definition", true).unwrap();
        std::fs::write(root.join(name), b"foreign definition").unwrap();
        assert!(owned_definition(&directory, name, &accepted).is_err());
        let entries: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from("owned.service")]);
        std::fs::remove_file(root.join(name)).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn service_rerun_keeps_a_legacy_linux_definition_until_helpers_are_current() {
        let root = std::env::current_dir().unwrap().join(format!(
            ".service-legacy-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let directory = setup_storage::private_directory(&root, true).unwrap();
        let name = Path::new("owned.service");
        let spec = spec();
        let current = linux_definition(&spec).unwrap();
        let legacy = legacy_linux_definition(&spec).unwrap();
        let kept = || kept_linux_definition(&directory, name, &spec).unwrap();
        assert_eq!(kept(), current);
        definition_file(&directory, name, legacy.as_bytes(), true).unwrap();
        assert_eq!(kept(), legacy);
        definition_file(&directory, name, kept().as_bytes(), true).unwrap();
        publish_definition(&directory, name, current.as_bytes()).unwrap();
        assert_eq!(kept(), current);
        std::fs::write(root.join(name), b"foreign definition").unwrap();
        assert_eq!(kept(), current);
        assert!(definition_file(&directory, name, kept().as_bytes(), true).is_err());
        std::fs::remove_file(root.join(name)).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn service_upgrade_refuses_another_identity_or_state() {
        let new = spec();
        let mut foreign = spec();
        foreign.id = "d17273e0-a8a3-4fce-b3cd-49f112d8b15c".into();
        assert!(stop_and_replace(&[foreign], &new).is_err());
        let mut moved = spec();
        moved.state = moved.state.with_file_name("other state");
        assert!(stop_and_replace(&[moved], &new).is_err());
    }

    #[test]
    fn service_windows_stop_script_targets_only_owned_workers_and_bridges() {
        let mut old = spec();
        old.executable = old
            .executable
            .with_file_name("o'ld")
            .join("festerm-tunnel-setup.exe");
        let script = windows_stop_script(&[&old], &spec().state, 4242).unwrap();
        assert!(script.contains("$_.ProcessId -ne 4242"));
        assert!(script.contains("o''ld"));
        assert!(script.contains("festerm-tunnel-bridge.exe"));
        assert!(script.contains("' --serve '"));
        assert!(script.contains("'--existing-host-identity'"));
        assert!(!script.contains("sessiond"));
        assert!(powershell_literal("C:\\a\u{2019}b").is_err());
    }

    #[test]
    fn service_systemd_loaded_collision_refuses_foreign_fragment_and_dropins() {
        let path = spec().state.join("owned.service");
        let expected = format!(
            "LoadState=loaded\nFragmentPath={}\nDropInPaths=\n",
            path.display()
        );
        linux_loaded_matches(expected.as_bytes(), &path, false).unwrap();
        for foreign in [
            expected.replace("owned.service", "foreign.service"),
            expected.replace("DropInPaths=", "DropInPaths=/foreign.conf"),
            expected.replace("loaded", "masked"),
            format!("{expected}FragmentPath={}\n", path.display()),
            String::new(),
        ] {
            assert!(linux_loaded_matches(foreign.as_bytes(), &path, true).is_err());
        }
        let missing = b"LoadState=not-found\nFragmentPath=\nDropInPaths=\n";
        linux_loaded_matches(missing, &path, true).unwrap();
        assert!(linux_loaded_matches(missing, &path, false).is_err());
    }

    #[test]
    fn service_launchd_loaded_collision_requires_exact_path_program_and_arguments() {
        let spec = spec();
        let path = spec.state.join("owned.plist");
        let text = format!(
            "path = {}\nprogram = {}\narguments = {{\n{}\n--serve\n--state-dir\n{}\n}}\n",
            path.display(),
            spec.executable.display(),
            spec.executable.display(),
            spec.state.display(),
        );
        mac_loaded_matches(text.as_bytes(), &path, &spec).unwrap();
        assert!(mac_loaded_matches(
            text.replace("--serve", "--anonymous").as_bytes(),
            &path,
            &spec
        )
        .is_err());
        assert!(mac_loaded_matches(
            text.replace("owned.plist", "foreign.plist").as_bytes(),
            &path,
            &spec
        )
        .is_err());
    }

    #[test]
    fn service_native_runner_bounds_deadline_output_and_redacts_errors() {
        let executable = std::env::current_exe().unwrap();
        let child = |name| args(&["--exact", name, "--ignored", "--nocapture"]);
        let before = std::time::Instant::now();
        let timeout = native_bounded(
            &executable,
            &child("service_install::tests::service_runner_slow_child"),
            "service.fixed_timeout",
            Duration::from_millis(100),
            OUTPUT_LIMIT,
        );
        assert_eq!(timeout.err().unwrap().to_string(), "service.fixed_timeout");
        assert!(before.elapsed() < Duration::from_secs(5));
        let overflow = native_bounded(
            &executable,
            &child("service_install::tests::service_runner_verbose_child"),
            "service.fixed_output",
            DEADLINE,
            1024,
        );
        assert_eq!(overflow.err().unwrap().to_string(), "service.fixed_output");
        let failure = run(
            &executable,
            &args(&["--this-option-does-not-exist"]),
            "service.fixed_exit",
        );
        assert_eq!(failure.err().unwrap().to_string(), "service.fixed_exit");
    }

    #[test]
    #[ignore = "read-only native prerequisite check; requires FESTERM_TUNNEL_SERVICE_NATIVE_SMOKE=1"]
    fn service_native_manager_preflight_smoke() {
        assert_eq!(
            std::env::var("FESTERM_TUNNEL_SERVICE_NATIVE_SMOKE").as_deref(),
            Ok("1")
        );
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        preflight_registration_directory().unwrap();
        preflight_service_manager().unwrap();
        #[cfg(windows)]
        {
            let (sid, system) = festerm_windows_security::current_user_service_context().unwrap();
            let root = std::env::temp_dir().join(format!(
                "festerm-task-xml-validation-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir(&root).unwrap();
            let path = root.join("service-task.xml");
            let result = (|| {
                let mut spec = spec();
                spec.state.push("terminal-\u{1f980}");
                std::fs::write(&path, windows_task_bytes(&windows_definition(&spec, &sid)?))?;
                let expression = format!(
                    "$ErrorActionPreference='Stop'; \
                     $s=[Activator]::CreateInstance([type]::GetTypeFromProgID('Schedule.Service')); \
                     $s.Connect(); $xml=[IO.File]::ReadAllText({}); \
                     $null=$s.GetFolder('\\').RegisterTask('fesTerm-XmlValidation-{}',\
                     $xml,1,$null,$null,3,$null)",
                    powershell_literal(path_text(&path)?)?,
                    spec.id,
                );
                run(
                    &system.join("WindowsPowerShell/v1.0/powershell.exe"),
                    &[
                        OsString::from("-NoProfile"),
                        OsString::from("-NonInteractive"),
                        OsString::from("-Command"),
                        OsString::from(expression),
                    ],
                    "service.task_xml_validation",
                )
            })();
            let file_cleanup = std::fs::remove_file(&path);
            let directory_cleanup = std::fs::remove_dir(&root);
            result.unwrap();
            file_cleanup.unwrap();
            directory_cleanup.unwrap();
        }
    }

    #[cfg(target_os = "macos")]
    const SMOKE_WORKER: &[u8] = b"#!/bin/sh\n\
        umask 077\n\
        [ \"$#\" -eq 3 ] && [ \"$1\" = '--serve' ] && [ \"$2\" = '--state-dir' ] || exit 2\n\
        /bin/sleep 60 &\n\
        printf '%s %s\\n' \"$$\" \"$!\" >> \"$3/children\" || exit 3\n\
        printf 'public-native-service-marker\\n' >> \"$3/marker\" || exit 3\n\
        exec /bin/sleep 5\n";
    #[cfg(target_os = "macos")]
    const SMOKE_MARKER: &[u8] = b"public-native-service-marker\n";

    #[cfg(target_os = "macos")]
    struct NativeSmoke {
        spec: ServiceSpec,
        root: PathBuf,
        plist: PathBuf,
        target: String,
        cleaned: bool,
    }

    #[cfg(target_os = "macos")]
    impl NativeSmoke {
        fn children(&self) -> io::Result<Vec<(u32, u32)>> {
            let state = match setup_storage::private_directory(&self.spec.state, false) {
                Ok(state) => state,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            let bytes = match setup_storage::read_private(&state, Path::new("children"), 4096) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(error),
            };
            std::str::from_utf8(&bytes)
                .map_err(|_| refused("smoke.unverified_child_record"))?
                .lines()
                .map(|line| {
                    let mut fields = line.split_whitespace();
                    let mut next_pid = || {
                        fields
                            .next()
                            .and_then(|value| value.parse::<u32>().ok())
                            .filter(|value| *value > 1)
                            .ok_or_else(|| refused("smoke.unverified_child_record"))
                    };
                    let group = next_pid()?;
                    let pid = next_pid()?;
                    if fields.next().is_some() {
                        return Err(refused("smoke.unverified_child_record"));
                    }
                    Ok((group, pid))
                })
                .collect()
        }

        fn child_alive(&self, group: u32, pid: u32) -> io::Result<bool> {
            let output = native(
                Path::new("/bin/ps"),
                &args(&["-p", &pid.to_string(), "-o", "uid=,pgid=,stat=,comm="]),
                "smoke.child_inspect",
            )?;
            if !output.success && output.bytes.is_empty() {
                return Ok(false);
            }
            if !output.success {
                return Err(refused("smoke.child_inspect"));
            }
            let text =
                std::str::from_utf8(&output.bytes).map_err(|_| refused("smoke.child_inspect"))?;
            let fields: Vec<_> = text.split_whitespace().collect();
            if fields.len() != 4
                || fields[0].parse::<u32>().ok() != Some(festerm_unix_security::current_user_id()?)
                || fields[1].parse::<u32>().ok() != Some(group)
                || !matches!(fields[3], "/bin/sleep" | "sleep")
            {
                return Err(refused("smoke.child_identity_changed"));
            }
            Ok(!fields[2].starts_with('Z'))
        }

        fn wait_children_stopped(&self) -> io::Result<()> {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            loop {
                let mut alive = false;
                for (group, pid) in self.children()? {
                    alive |= self.child_alive(group, pid)?;
                }
                if !alive {
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(refused("smoke.child_survived_service_stop"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        fn cleanup_children(&self) -> io::Result<()> {
            match self.wait_children_stopped() {
                Ok(()) => return Ok(()),
                Err(error) if error.to_string() == "smoke.child_survived_service_stop" => {}
                Err(error) => return Err(error),
            }
            // Failure-only cleanup: never signal an unverified PID or group.
            // The test itself still fails if the native manager left a child.
            for signal in ["-TERM", "-KILL"] {
                for (group, pid) in self.children()? {
                    if self.child_alive(group, pid)? {
                        run(
                            Path::new("/bin/kill"),
                            &args(&[signal, &pid.to_string()]),
                            "smoke.child_cleanup",
                        )?;
                    }
                }
                if self.wait_children_stopped().is_ok() {
                    return Ok(());
                }
            }
            Err(refused("smoke.child_cleanup_deadline"))
        }

        fn cleanup(&mut self) -> io::Result<()> {
            use std::os::unix::fs::MetadataExt;

            if self.cleaned {
                return Ok(());
            }
            let tool = Path::new("/bin/launchctl");
            match std::fs::symlink_metadata(&self.plist) {
                Ok(_) => {
                    let parent = trusted_directory(
                        self.plist
                            .parent()
                            .ok_or_else(|| refused("smoke.plist_parent"))?,
                        false,
                    )?;
                    let name = Path::new(
                        self.plist
                            .file_name()
                            .ok_or_else(|| refused("smoke.plist_name"))?,
                    );
                    definition_file(&parent, name, mac_definition(&self.spec)?.as_bytes(), false)
                        .map_err(|_| refused("smoke.cleanup_unverified_plist"))?;
                    // This random label was proved absent before the test and
                    // its exact newly created plist has just been verified.
                    let loaded = native(
                        tool,
                        &args(&["print", &self.target]),
                        "smoke.cleanup_inspect",
                    )?;
                    if loaded.success {
                        run(
                            tool,
                            &args(&["bootout", &self.target]),
                            "smoke.cleanup_bootout",
                        )?;
                    }
                    let remaining = native(
                        tool,
                        &args(&["print", &self.target]),
                        "smoke.cleanup_inspect",
                    )?;
                    if remaining.success {
                        return Err(refused("smoke.cleanup_service_still_loaded"));
                    }
                    definition_file(&parent, name, mac_definition(&self.spec)?.as_bytes(), false)?;
                    std::fs::remove_file(&self.plist)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return Err(refused("smoke.cleanup_unverified_plist")),
            }
            self.cleanup_children()?;
            let root = setup_storage::private_directory(&self.root, false)?;
            match setup_storage::private_directory(&self.spec.state, false) {
                Ok(state) => {
                    if !self.children()?.is_empty() {
                        std::fs::remove_file(self.spec.state.join("children"))?;
                    }
                    match setup_storage::read_private(&state, Path::new("marker"), 4096) {
                        Ok(marker) => {
                            if marker.is_empty()
                                || marker.len() % SMOKE_MARKER.len() != 0
                                || !marker
                                    .as_chunks::<{ SMOKE_MARKER.len() }>()
                                    .0
                                    .iter()
                                    .all(|part| part == SMOKE_MARKER)
                            {
                                return Err(refused("smoke.cleanup_unverified_marker"));
                            }
                            std::fs::remove_file(self.spec.state.join("marker"))?;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(_) => return Err(refused("smoke.cleanup_unverified_marker")),
                    }
                    std::fs::remove_dir(&self.spec.state)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            match festerm_unix_security::open_file_nofollow(
                &root,
                Path::new("festerm-tunnel-setup"),
            ) {
                Ok(file) => {
                    let metadata = file.metadata()?;
                    if metadata.uid() != festerm_unix_security::current_user_id()?
                        || metadata.mode() & 0o7777 != 0o700
                        || metadata.nlink() != 1
                    {
                        return Err(refused("smoke.cleanup_unverified_worker"));
                    }
                    exact_file(file, SMOKE_WORKER)?;
                    std::fs::remove_file(&self.spec.executable)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // Never recurse: unexpected entries are preserved and fail cleanup.
            match setup_storage::private_directory(&self.root.join("foreign-state"), false) {
                Ok(_) => std::fs::remove_dir(self.root.join("foreign-state"))?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            std::fs::remove_dir(&self.root)?;
            self.cleaned = true;
            Ok(())
        }

        fn wait_for_successful_exit(&self) -> io::Result<()> {
            let deadline = std::time::Instant::now() + Duration::from_secs(12);
            loop {
                let bytes = run(
                    Path::new("/bin/launchctl"),
                    &args(&["print", &self.target]),
                    "smoke.wait_worker",
                )?;
                mac_loaded_matches(&bytes, &self.plist, &self.spec)?;
                let text = std::str::from_utf8(&bytes).map_err(|_| refused("smoke.wait_worker"))?;
                let running = text.lines().any(|line| line.trim() == "state = running");
                if !running {
                    return if text.lines().any(|line| line.trim() == "last exit code = 0") {
                        Ok(())
                    } else {
                        Err(refused("smoke.worker_did_not_exit_successfully"))
                    };
                }
                if std::time::Instant::now() >= deadline {
                    return Err(refused("smoke.worker_exit_deadline"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for NativeSmoke {
        fn drop(&mut self) {
            if let Err(error) = self.cleanup() {
                eprintln!("native service smoke cleanup failed: {error}");
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "registers only an isolated dummy LaunchAgent; requires FESTERM_TUNNEL_SERVICE_NATIVE_SMOKE=1"]
    fn service_macos_native_launchagent_smoke() {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::env::var("FESTERM_TUNNEL_SERVICE_NATIVE_SMOKE").as_deref(),
            Ok("1"),
            "explicit FESTERM_TUNNEL_SERVICE_NATIVE_SMOKE=1 is required"
        );
        let result = (|| -> io::Result<()> {
            let id = uuid::Uuid::new_v4().to_string();
            let uid = festerm_unix_security::current_user_id()?;
            let home = festerm_unix_security::current_user_home()?;
            let label = format!("dev.fes.festerm.tunnel.{id}");
            let plist = home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"));
            if std::fs::symlink_metadata(&plist).is_ok() {
                return Err(refused("smoke.preexisting_plist"));
            }
            let target = format!("gui/{uid}/{label}");
            let tool = Path::new("/bin/launchctl");
            run(
                tool,
                &args(&["print-disabled", &format!("gui/{uid}")]),
                "smoke.gui_login_required",
            )?;
            if native(tool, &args(&["print", &target]), "smoke.preflight")?.success {
                return Err(refused("smoke.preexisting_service"));
            }
            // A canonical, owner-only project-local directory; never /tmp or a
            // personal application's state. The random UUID scopes every entry.
            let root_path = std::env::current_dir()?
                .canonicalize()?
                .join(format!(".native-service-smoke-{id}"));
            let root = setup_storage::private_directory(&root_path, true)?;
            let state_path = root_path.join("state");
            let mut smoke = NativeSmoke {
                spec: ServiceSpec {
                    id,
                    executable: root_path.join("festerm-tunnel-setup"),
                    state: state_path,
                },
                root: root_path,
                plist,
                target,
                cleaned: false,
            };
            let lifecycle = (|| -> io::Result<()> {
                setup_storage::private_directory(&smoke.spec.state, true)?;
                setup_storage::private_directory(&smoke.root.join("foreign-state"), true)?;
                let mut worker = setup_storage::open_private_file(
                    &root,
                    Path::new("festerm-tunnel-setup"),
                    true,
                )?;
                worker.write_all(SMOKE_WORKER)?;
                worker.set_permissions(std::fs::Permissions::from_mode(0o700))?;
                worker.sync_all()?;
                drop(worker);
                install_and_start(&smoke.spec)?;
                let directory = trusted_directory(
                    smoke
                        .plist
                        .parent()
                        .ok_or_else(|| refused("smoke.plist_parent"))?,
                    false,
                )?;
                let name = Path::new(
                    smoke
                        .plist
                        .file_name()
                        .ok_or_else(|| refused("smoke.plist_name"))?,
                );
                let expected = mac_definition(&smoke.spec)?;
                definition_file(&directory, name, expected.as_bytes(), false)?;
                install_and_start(&smoke.spec)?;
                start_existing(&smoke.spec)?;
                let state = setup_storage::private_directory(&smoke.spec.state, false)?;
                if setup_storage::read_private(&state, Path::new("marker"), 4096)?.as_slice()
                    != SMOKE_MARKER
                {
                    return Err(refused("smoke.idempotency_started_duplicate_worker"));
                }
                let foreign = ServiceSpec {
                    id: smoke.spec.id.clone(),
                    executable: smoke.spec.executable.clone(),
                    state: smoke.root.join("foreign-state"),
                };
                for collision in [install_and_start(&foreign), start_existing(&foreign)] {
                    match collision {
                        Err(error) if error.to_string() == "service.foreign_definition" => {}
                        _ => return Err(refused("smoke.foreign_collision_not_refused")),
                    }
                }
                definition_file(&directory, name, expected.as_bytes(), false)?;
                smoke.wait_for_successful_exit()?;
                smoke.wait_children_stopped()?;
                // Explicitly restarting a completed worker is not an automatic
                // retry. Its second marker must be the only additional launch.
                start_existing(&smoke.spec)?;
                if setup_storage::read_private(&state, Path::new("marker"), 4096)?.as_slice()
                    != [SMOKE_MARKER, SMOKE_MARKER].concat()
                {
                    return Err(refused("smoke.explicit_restart_marker"));
                }
                definition_file(&directory, name, expected.as_bytes(), false)?;
                let children = smoke.children()?;
                if children.len() != 2 || !smoke.child_alive(children[1].0, children[1].1)? {
                    return Err(refused("smoke.child_not_started"));
                }
                run(
                    tool,
                    &args(&["bootout", &smoke.target]),
                    "smoke.stop_owned_service",
                )?;
                smoke.wait_children_stopped()?;
                if native(tool, &args(&["print", &smoke.target]), "smoke.verify_stop")?.success {
                    return Err(refused("smoke.service_survived_stop"));
                }
                definition_file(&directory, name, expected.as_bytes(), false)?;
                Ok(())
            })();
            let cleanup = smoke.cleanup();
            lifecycle.and(cleanup)
        })();
        assert!(
            result.is_ok(),
            "native LaunchAgent smoke failed: {result:?}"
        );
    }

    #[test]
    #[ignore = "bounded runner child; no service registration"]
    fn service_runner_slow_child() {
        std::thread::sleep(Duration::from_secs(2));
    }

    #[test]
    #[ignore = "bounded runner child; no service registration"]
    fn service_runner_verbose_child() {
        let _ = std::io::stdout().write_all(&vec![b'x'; 4096]);
    }
}
