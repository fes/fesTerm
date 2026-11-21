use super::refused;
use crate::{
    artifacts::{Architecture, Package, Platform, VerifiedArtifact},
    staging::{self, StagedArtifact},
};
use festerm_sessiond::private_storage as storage;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

/// Launch authority for exact updater-authenticated native host bytes, not a path
/// supplied by configuration. Keep the receipt alive through native registration.
pub struct TrustedStage {
    pub(super) path: PathBuf,
    pub(super) digest: String,
    file: File,
    _directory: File,
    _staged: StagedArtifact,
    helpers: Vec<TrustedExecutable>,
    resources: Vec<TrustedExecutable>,
    release_version: String,
    bundle_digest: String,
    platform: Platform,
}

struct TrustedExecutable {
    path: PathBuf,
    digest: String,
    file: File,
}

impl TrustedStage {
    /// Reauthenticate retained signed bytes after restart; a cached path or
    /// registration digest alone is never launch authority.
    pub fn reopen_release_cache(provenance: &Path, root: &Path) -> io::Result<Self> {
        Self::publish_release_files(
            &provenance.join(".manifest.json"),
            &provenance.join(".manifest.json.sig"),
            &provenance.join(".archive"),
            root,
        )
    }

    pub fn publish_release_files(
        manifest: &Path,
        signature: &Path,
        archive: &Path,
        root: &Path,
    ) -> io::Result<Self> {
        native_publisher_admission(std::env::consts::OS)?;
        let artifact = staging::verify_release_files(manifest, signature, archive)?;
        Self::publish(&artifact, root)
    }

    pub fn publish(artifact: &VerifiedArtifact, root: &Path) -> io::Result<Self> {
        native_publisher_admission(std::env::consts::OS)?;
        Self::publish_with(artifact, root, native_publisher_qualification)
    }

    fn publish_with(
        artifact: &VerifiedArtifact,
        root: &Path,
        mut signature: impl FnMut(&File, &Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        Self::publish_version_with(artifact, root, crate::VERSION, &mut signature)
    }

    fn publish_version_with(
        artifact: &VerifiedArtifact,
        root: &Path,
        version: &str,
        signature: impl FnMut(&File, &Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        let platform = match std::env::consts::OS {
            "linux" => Platform::Linux,
            "macos" => Platform::Macos,
            "windows" => Platform::Windows,
            _ => return Err(refused("host.service_unsupported_platform")),
        };
        let architecture = match std::env::consts::ARCH {
            "x86_64" => Architecture::X86_64,
            "aarch64" => Architecture::Aarch64,
            _ => return Err(refused("host.service_unsupported_platform")),
        };
        Self::publish_target_with(artifact, root, version, platform, architecture, signature)
    }

    // The public production path always supplies the compile-time native tuple.
    // Tests supply an explicit tuple to exercise Windows inventory portably.
    fn publish_target_with(
        artifact: &VerifiedArtifact,
        root: &Path,
        version: &str,
        platform: Platform,
        architecture: Architecture,
        mut signature: impl FnMut(&File, &Path) -> io::Result<()>,
    ) -> io::Result<Self> {
        let manifest = artifact.manifest();
        if manifest.package != Package::Host
            || manifest.schema != 2
            || manifest.version != version
            || manifest.platform != platform
            || manifest.architecture != architecture
        {
            return Err(refused(
                "host.service_current_native_authenticated_bundle_required",
            ));
        }
        let name = if platform == Platform::Windows {
            "festerm-host.exe"
        } else {
            "festerm-host"
        }
        .to_owned();
        let entry = artifact
            .entries()
            .iter()
            .find(|entry| entry.name() == name && entry.executable())
            .ok_or_else(|| refused("host.service_authenticated_host_entry_required"))?;
        let staged = staging::stage_artifact(artifact, root)?;
        let publication = root.join(format!("executable-{}", manifest.sha256));
        let directory = storage::private_directory(&publication, true)?;
        let path = publication.join(&name);
        match open(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                crate::protected_state::publish_new(&directory, Path::new(&name), entry.bytes())?;
            }
            Err(error) => return Err(error),
        }
        let file = open(&path)?;
        let expected = digest(entry.bytes());
        check(&file, entry.bytes().len() as u64, &expected)?;
        signature(&file, &path)?;
        let mut helpers = Vec::new();
        for entry in artifact
            .entries()
            .iter()
            .filter(|entry| entry.executable() && entry.name() != name)
        {
            let helper_path = publication.join(entry.name());
            match open(&helper_path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    crate::protected_state::publish_new(
                        &directory,
                        Path::new(entry.name()),
                        entry.bytes(),
                    )?;
                }
                Err(error) => return Err(error),
            }
            let helper = open(&helper_path)?;
            let expected = digest(entry.bytes());
            check(&helper, entry.bytes().len() as u64, &expected)?;
            signature(&helper, &helper_path)?;
            helpers.push(TrustedExecutable {
                path: helper_path,
                digest: expected,
                file: helper,
            });
        }
        // Microsoft sidecars are pinned native resources, not first-party
        // executables or Task targets. Artifact verification already checked
        // both SHA-512 pins; keep their immutable private bytes beside sessiond.
        let mut resources = Vec::new();
        for entry in artifact
            .entries()
            .iter()
            .filter(|entry| entry.name().starts_with("runtime/"))
        {
            let (parent, leaf) = staging::entry_parent(&publication, entry.name(), true)?;
            crate::protected_state::publish_immutable(
                &parent,
                Path::new(&leaf),
                entry.bytes(),
                "host.service_authenticated_resource_changed",
            )?;
            let resource_path = publication.join(entry.name());
            let resource = storage::open_private_file(&parent, Path::new(&leaf), false)?;
            let expected = digest(entry.bytes());
            check(&resource, entry.bytes().len() as u64, &expected)?;
            resources.push(TrustedExecutable {
                path: resource_path,
                digest: expected,
                file: resource,
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Qualify the entire signed executable set before promoting any file.
            for helper in &helpers {
                helper
                    .file
                    .set_permissions(std::fs::Permissions::from_mode(0o700))?;
                helper.file.sync_all()?;
            }
            file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            file.sync_all()?;
        }
        let receipt = Self {
            path,
            digest: expected,
            file,
            _directory: directory,
            _staged: staged,
            helpers,
            resources,
            release_version: manifest.version.clone(),
            bundle_digest: manifest.sha256.clone(),
            platform,
        };
        receipt.revalidate()?;
        Ok(receipt)
    }
}

fn native_publisher_qualification(file: &File, path: &Path) -> io::Result<()> {
    native_publisher_admission(std::env::consts::OS)?;
    #[cfg(target_os = "macos")]
    {
        use super::native::{CommandRunner, NativeRequest, NativeRunner};
        let verified = NativeRunner.execute(&NativeRequest::Process {
            tool: "/usr/bin/codesign".into(),
            args: [
                "--verify".into(),
                "--strict".into(),
                "--all-architectures".into(),
                format!(
                    "-R=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"{MACOS_RELEASE_TEAM}\""
                ).into(),
                path.as_os_str().to_owned(),
            ]
            .into(),
        })?;
        if !verified.success {
            return Err(refused("host.service_macos_release_publisher_required"));
        }
        let metadata = super::native::signature_metadata(path)?;
        if !metadata.success {
            return Err(refused("host.service_macos_release_publisher_required"));
        }
        macos_publisher_metadata(&metadata.stdout)?;
    }
    #[cfg(windows)]
    super::native::signature_publisher(file, path)?;
    let _ = (file, path);
    Ok(())
}

// Protected release environment APPLE_SIGNING_IDENTITY, not candidate metadata.
#[cfg(any(target_os = "macos", test))]
const MACOS_RELEASE_TEAM: &str = "F884J4784J";
#[cfg(any(target_os = "macos", test))]
fn macos_publisher_metadata(bytes: &[u8]) -> io::Result<()> {
    const HARDENED_RUNTIME: u32 = 0x10000;
    const AD_HOC: u32 = 0x2;
    let invalid = || refused("host.service_macos_release_publisher_and_runtime_required");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let teams: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("TeamIdentifier="))
        .collect();
    let flags: Vec<_> = text
        .lines()
        .filter(|line| line.starts_with("CodeDirectory "))
        .flat_map(str::split_whitespace)
        .filter_map(|field| field.strip_prefix("flags="))
        .collect();
    if teams != [MACOS_RELEASE_TEAM] || flags.len() != 1 {
        return Err(invalid());
    }
    let flags = flags[0]
        .split('(')
        .next()
        .and_then(|flags| flags.strip_prefix("0x"))
        .and_then(|flags| u32::from_str_radix(flags, 16).ok())
        .ok_or_else(invalid)?;
    if flags & HARDENED_RUNTIME == 0 || flags & AD_HOC != 0 {
        return Err(invalid());
    }
    Ok(())
}

fn native_publisher_admission(platform: &str) -> io::Result<()> {
    match platform {
        "linux" | "macos" | "windows" => Ok(()),
        _ => Err(refused("host.service_unsupported_platform")),
    }
}

#[cfg(test)]
pub(super) fn fixture(root: &Path) -> TrustedStage {
    TrustedStage::publish_with(
        &crate::artifacts::host_staging_fixture(),
        &root.join("artifacts"),
        |_, _| Ok(()),
    )
    .unwrap()
}

#[cfg(all(test, any(unix, all(feature = "cloud", feature = "host-runtime"))))]
pub(super) fn version_fixture(root: &Path, version: &str) -> TrustedStage {
    TrustedStage::publish_version_with(
        &crate::artifacts::host_version_fixture(version),
        &root.join("artifacts"),
        version,
        |_, _| Ok(()),
    )
    .unwrap()
}

#[cfg(all(test, unix))]
pub(super) fn version_fixture_with_sessiond(
    root: &Path,
    version: &str,
    sessiond: &[u8],
) -> TrustedStage {
    TrustedStage::publish_version_with(
        &crate::artifacts::host_version_fixture_with_sessiond(version, sessiond),
        &root.join("artifacts"),
        version,
        |_, _| Ok(()),
    )
    .unwrap()
}

#[cfg(test)]
pub(super) fn windows_fixture(root: &Path, version: &str) -> TrustedStage {
    let _directory = storage::private_directory(root, true).unwrap();
    let expected = crate::artifacts::ExpectedArtifact {
        version,
        platform: Platform::Windows,
        architecture: Architecture::Aarch64,
        package: Package::Host,
    };
    let artifact = crate::artifacts::host_acquisition_fixture(&expected);
    TrustedStage::publish_target_with(
        &artifact,
        &root.join("windows-artifacts"),
        version,
        expected.platform,
        expected.architecture,
        |_, _| Ok(()),
    )
    .unwrap()
}

#[cfg(all(test, feature = "native-service-fixtures", unix))]
pub(super) fn lab_fixture(root: &Path, bytes: &[u8]) -> TrustedStage {
    use std::os::unix::fs::PermissionsExt;
    let artifact = crate::artifacts::host_staging_fixture();
    let staged = staging::stage_artifact(&artifact, &root.join("synthetic-provenance")).unwrap();
    let directory = storage::private_directory(&root.join("executable"), true).unwrap();
    let name = Path::new("festerm-host-service-test-child");
    crate::protected_state::publish_new(&directory, name, bytes).unwrap();
    let path = root.join("executable").join(name);
    let file = open(&path).unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o700))
        .unwrap();
    file.sync_all().unwrap();
    TrustedStage {
        path,
        digest: digest(bytes),
        file,
        _directory: directory,
        _staged: staged,
        helpers: Vec::new(),
        resources: Vec::new(),
        release_version: crate::VERSION.into(),
        bundle_digest: artifact.manifest().sha256.clone(),
        platform: artifact.manifest().platform,
    }
}

impl TrustedStage {
    /// This additive read-only probe is needed only for explicit file consent.
    /// Same-version signatures authenticate bytes, not the features in them.
    pub fn require_host_files(&self, settings: &crate::lifecycle::Settings) -> io::Result<()> {
        self.require_host_files_with(settings, &super::native::NativeRunner)
    }

    fn require_host_files_with(
        &self,
        settings: &crate::lifecycle::Settings,
        runner: &impl super::native::CommandRunner,
    ) -> io::Result<()> {
        if settings.filesystem.is_none() {
            return Ok(());
        }
        self.revalidate()?;
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            schema: u16,
            host_files_v2: bool,
        }
        let unsupported = || refused("host.file_helper_unsupported");
        let reply = runner
            .execute_read_only(&super::native::NativeRequest::Process {
                tool: self.path.clone(),
                args: vec!["capabilities".into()],
            })
            .map_err(|_| unsupported())?;
        if !reply.success || reply.code != 0 || reply.stdout.len() > 1024 {
            return Err(unsupported());
        }
        let reply: Reply = serde_json::from_slice(&reply.stdout).map_err(|_| unsupported())?;
        if reply.schema != 1 || !reply.host_files_v2 {
            return Err(unsupported());
        }
        self.revalidate()?;
        if settings
            .filesystem
            .as_ref()
            .is_some_and(|policy| policy.delete)
        {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct DeleteReply {
                schema: u16,
                host_files_delete_v3: bool,
            }
            let reply = runner
                .execute_read_only(&super::native::NativeRequest::Process {
                    tool: self.path.clone(),
                    args: vec!["capabilities".into(), "files-delete".into()],
                })
                .map_err(|_| unsupported())?;
            if !reply.success || reply.code != 0 || reply.stdout.len() > 1024 {
                return Err(unsupported());
            }
            let reply: DeleteReply =
                serde_json::from_slice(&reply.stdout).map_err(|_| unsupported())?;
            if reply.schema != 1 || !reply.host_files_delete_v3 {
                return Err(unsupported());
            }
            self.revalidate()?;
        }
        Ok(())
    }

    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn revalidate_for_desktop_start(&self) -> io::Result<()> {
        self.revalidate()
    }

    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn require_current_executable(&self) -> io::Result<()> {
        self.revalidate()?;
        if std::env::current_exe()? != self.path {
            return Err(refused("host.service_registered_helper_required"));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn require_mac_host_access(&self) -> io::Result<()> {
        use super::native::{CommandRunner, NativeRequest, NativeRunner};
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            schema: u16,
            mac_host_access_v1: bool,
            desktop_start_v1: bool,
        }
        self.revalidate()?;
        let reply = NativeRunner.execute_read_only(&NativeRequest::Process {
            tool: self.path.clone(),
            args: vec!["capabilities".into(), "mac-host-access".into()],
        })?;
        if !reply.success || reply.code != 0 || reply.stdout.len() > 1024 {
            return Err(refused("host.mac_host_access_helper_required"));
        }
        let reply: Reply = serde_json::from_slice(&reply.stdout)
            .map_err(|_| refused("host.mac_host_access_helper_required"))?;
        if reply.schema != 1 || !reply.mac_host_access_v1 || !reply.desktop_start_v1 {
            return Err(refused("host.mac_host_access_helper_required"));
        }
        self.revalidate()
    }

    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn require_mac_host_creation_identity(&self) -> io::Result<()> {
        self.revalidate()?;
        festerm_secret_store::MacHostCreationContext::validate_identities(&self.path, &self.file)
            .map_err(|_| refused("host.mac_host_identity_incompatible"))?;
        self.revalidate()
    }

    pub fn version(&self) -> &str {
        &self.release_version
    }

    pub(super) fn bundle_digest(&self) -> &str {
        &self.bundle_digest
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(all(target_os = "macos", feature = "cloud", feature = "host-runtime"))]
    pub(crate) fn create_host_item(
        &self,
        purpose: festerm_secret_store::HostSecretPurpose,
        reference: &festerm_secret_store::SecretReference,
        secret: &festerm_secret_store::SecretBytes,
    ) -> io::Result<()> {
        self.revalidate()?;
        let context = festerm_secret_store::MacHostCreationContext::capture(
            purpose, reference, &self.path, &self.file,
        )
        .map_err(|_| io::Error::other("host.secure_store_unavailable"))?;
        // Capture/use/drop stays in this synchronous owned worker: native
        // handles never enter the Send + Sync store or an async suspension.
        context
            .create_reserved(secret)
            .map_err(|_| io::Error::other("host.secure_store_unavailable"))?;
        self.revalidate()
    }

    pub(super) fn authenticate_registered(
        record: &super::Registration,
        version: &str,
    ) -> io::Result<()> {
        Self::authenticate_registered_with(
            record,
            version,
            crate::artifacts::verify,
            native_publisher_qualification,
        )
    }

    fn authenticate_registered_with(
        record: &super::Registration,
        version: &str,
        verify: impl FnOnce(
            &[u8],
            &[u8],
            &[u8],
            &crate::artifacts::ExpectedArtifact<'_>,
        ) -> Result<VerifiedArtifact, crate::artifacts::ArtifactError>,
        mut qualify: impl FnMut(&File, &Path) -> io::Result<()>,
    ) -> io::Result<()> {
        let provenance = record
            .provenance
            .as_ref()
            .ok_or_else(|| refused("host.service_signed_provenance_required"))?;
        let artifact = staging::verify_release_version(
            &provenance.join(".manifest.json"),
            &provenance.join(".manifest.json.sig"),
            &provenance.join(".archive"),
            version,
            verify,
        )?;
        let directory = record
            .executable
            .parent()
            .ok_or_else(|| refused("host.service_executable_path"))?;
        if directory.file_name().and_then(|name| name.to_str())
            != Some(format!("executable-{}", artifact.manifest().sha256).as_str())
        {
            return Err(refused("host.service_authenticated_bundle_changed"));
        }
        let name = if artifact.manifest().platform == Platform::Windows {
            "festerm-host.exe"
        } else {
            "festerm-host"
        }
        .to_owned();
        if record.executable.file_name().and_then(|name| name.to_str()) != Some(name.as_str()) {
            return Err(refused("host.service_authenticated_host_entry_required"));
        }
        if let Some(worker) = &record.windows_managed {
            let entry = artifact
                .entries()
                .iter()
                .find(|entry| entry.executable() && entry.name() == "festerm-tunnel-worker.exe")
                .ok_or_else(|| refused("host.service_authenticated_worker_required"))?;
            if worker.path != directory.join(entry.name()) || worker.digest != digest(entry.bytes())
            {
                return Err(refused("host.service_authenticated_worker_changed"));
            }
        }
        for entry in artifact.entries().iter().filter(|entry| entry.executable()) {
            let path = directory.join(entry.name());
            let file = open(&path)?;
            let expected = digest(entry.bytes());
            check(&file, entry.bytes().len() as u64, &expected)?;
            if entry.name() == name && expected != record.digest {
                return Err(refused("host.service_authenticated_bundle_changed"));
            }
            qualify(&file, &path)?;
        }
        for entry in artifact
            .entries()
            .iter()
            .filter(|entry| entry.name().starts_with("runtime/"))
        {
            let (parent, leaf) = staging::entry_parent(directory, entry.name(), false)?;
            let file = storage::open_private_file(&parent, Path::new(&leaf), false)?;
            check(&file, entry.bytes().len() as u64, &digest(entry.bytes()))?;
        }
        Ok(())
    }

    #[cfg(all(test, any(unix, all(feature = "cloud", feature = "host-runtime"))))]
    pub(super) fn authenticate_fixture_registered(
        record: &super::Registration,
        version: &str,
    ) -> io::Result<()> {
        Self::authenticate_registered_with(
            record,
            version,
            |manifest, signature, archive, expected| {
                // Portable model only: synthetic signatures and an explicit
                // authenticated manifest tuple, never native publisher proof.
                let descriptor: crate::artifacts::Manifest = serde_json::from_slice(manifest)
                    .map_err(|_| crate::artifacts::ArtifactError::Manifest)?;
                let target = crate::artifacts::ExpectedArtifact {
                    version: expected.version,
                    platform: descriptor.platform,
                    architecture: descriptor.architecture,
                    package: expected.package,
                };
                crate::artifacts::verify_staging_fixture(manifest, signature, archive, &target)
            },
            |_, _| Ok(()),
        )
    }

    /// Retained signed provenance, not executable authority by itself.
    pub fn provenance_path(&self) -> &Path {
        self._staged.path()
    }

    /// Exact same-version signed helper; never search PATH or accept a request path.
    pub fn sessiond_path(&self) -> io::Result<&Path> {
        self.revalidate()?;
        let name = if self.platform == Platform::Windows {
            "festerm-sessiond.exe"
        } else {
            "festerm-sessiond"
        };
        self.helpers
            .iter()
            .find(|helper| helper.path.file_name().is_some_and(|file| file == name))
            .map(|helper| helper.path.as_path())
            .ok_or_else(|| refused("host.service_authenticated_sessiond_required"))
    }

    pub(super) fn windows_managed_execution(&self) -> io::Result<super::WindowsManagedExecution> {
        if self.platform != Platform::Windows {
            return Err(refused("host.service_authenticated_worker_required"));
        }
        self.revalidate()?;
        let helper = self
            .helpers
            .iter()
            .find(|helper| helper.path == self.path.with_file_name("festerm-tunnel-worker.exe"))
            .ok_or_else(|| refused("host.service_authenticated_worker_required"))?;
        Ok(super::WindowsManagedExecution {
            path: helper.path.clone(),
            digest: helper.digest.clone(),
        })
    }

    pub(super) fn revalidate_registration(&self, record: &super::Registration) -> io::Result<()> {
        self.revalidate()?;
        if self.path != record.executable
            || self.digest != record.digest
            || record.provenance.as_deref() != Some(self.provenance_path())
        {
            return Err(refused("host.service_authenticated_bundle_changed"));
        }
        if let Some(worker) = &record.windows_managed {
            if *worker != self.windows_managed_execution()? {
                return Err(refused("host.service_authenticated_worker_changed"));
            }
        }
        Ok(())
    }

    pub(super) fn require_running_worker(
        &self,
        record: &super::Registration,
        running: &Path,
    ) -> io::Result<()> {
        self.revalidate_registration(record)?;
        let worker = record
            .windows_managed
            .as_ref()
            .ok_or_else(|| refused("host.service_authenticated_worker_required"))?;
        let helper = self
            .helpers
            .iter()
            .find(|helper| helper.path == worker.path)
            .ok_or_else(|| refused("host.service_authenticated_worker_required"))?;
        let current = open(running)?;
        if !same(&current, &helper.file)? {
            return Err(refused("host.service_running_worker_mismatch"));
        }
        check(&current, helper.file.metadata()?.len(), &worker.digest)
    }

    pub(super) fn revalidate(&self) -> io::Result<()> {
        let current = open(&self.path)?;
        if !same(&current, &self.file)? {
            return Err(refused("host.service_executable_replaced"));
        }
        check(&current, self.file.metadata()?.len(), &self.digest)?;
        for helper in self.helpers.iter().chain(&self.resources) {
            let current = open(&helper.path)?;
            if !same(&current, &helper.file)? {
                return Err(refused("host.service_executable_replaced"));
            }
            check(&current, helper.file.metadata()?.len(), &helper.digest)?;
        }
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn check(file: &File, length: u64, expected: &str) -> io::Result<()> {
    let mut file = file.try_clone()?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    if file.metadata()?.len() != length || length > crate::artifacts::MAX_ARCHIVE_BYTES as u64 {
        return Err(refused("host.service_executable_changed"));
    }
    let mut hash = Sha256::new();
    let mut bytes = [0; 8192];
    let mut reader = file.take(length + 1);
    loop {
        let count = reader.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    let actual: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Err(refused("host.service_executable_changed"));
    }
    Ok(())
}

fn open(path: &Path) -> io::Result<File> {
    let directory = storage::private_directory(
        path.parent()
            .ok_or_else(|| refused("host.service_executable_path"))?,
        false,
    )?;
    let name = Path::new(
        path.file_name()
            .ok_or_else(|| refused("host.service_executable_path"))?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let file = festerm_unix_security::open_file_nofollow(&directory, name)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != festerm_unix_security::current_user_id()?
            || metadata.mode() & 0o6077 != 0
            || metadata.nlink() != 1
            || !(festerm_unix_security::is_current_user_only(&file, false)?
                || festerm_unix_security::is_current_user_only_executable(&file)?)
        {
            return Err(refused("host.service_executable_permissions"));
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        let file = festerm_windows_security::open_file_no_reparse(&directory, name)?;
        if !file.metadata()?.is_file()
            || !festerm_windows_security::is_current_user_only(&file)?
            || festerm_windows_security::file_link_count(&file)? != 1
        {
            return Err(refused("host.service_executable_permissions"));
        }
        Ok(file)
    }
}

pub(super) fn same(left: &File, right: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = left.metadata()?;
        let right = right.metadata()?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(windows)]
    festerm_windows_security::same_file_identity(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_file_helper_probe_is_exact_bounded_and_optional() {
        use super::super::native::{CommandRunner, NativeOutput, NativeRequest};
        struct Probe {
            path: PathBuf,
            output: Option<Vec<u8>>,
            success: bool,
            calls: std::cell::Cell<usize>,
        }
        impl CommandRunner for Probe {
            fn execute_read_only(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
                self.execute(request)
            }
            fn execute(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
                assert_eq!(
                    request,
                    &NativeRequest::Process {
                        tool: self.path.clone(),
                        args: vec!["capabilities".into()]
                    }
                );
                self.calls.set(self.calls.get() + 1);
                let stdout = self
                    .output
                    .clone()
                    .ok_or_else(|| io::Error::other("private-path-token-canary"))?;
                Ok(NativeOutput {
                    success: self.success,
                    code: if self.success { 0 } else { 1 },
                    stdout,
                })
            }
        }
        let root = storage::private_test_directory();
        let stage = fixture(root.path());
        let mut settings = crate::lifecycle::Settings {
            filesystem: None,
            ordinary_access: crate::lifecycle::OrdinaryAccess::ProviderOwner,
            friendly_name: "Probe fixture".into(),
            allow_creation: false,
            startup: crate::lifecycle::Startup::Manual,
        };
        let mut probe = Probe {
            path: stage.path().into(),
            output: None,
            success: false,
            calls: std::cell::Cell::new(0),
        };
        stage.require_host_files_with(&settings, &probe).unwrap();
        assert_eq!(probe.calls.get(), 0);
        settings.filesystem = Some(festerm_tunnel_bridge::file_protocol::Policy {
            delete: false,
            root: root.path().into(),
            write: false,
        });
        for output in [
            None,
            Some(Vec::new()),
            Some(vec![b'x'; 1025]),
            Some(br#"{"schema":1,"host_files_v2":false}"#.to_vec()),
            Some(br#"{"schema":2,"host_files_v2":true}"#.to_vec()),
            Some(br#"{"schema":1,"host_files_v2":true,"future":true}"#.to_vec()),
            Some(br#"{"schema":1}"#.to_vec()),
            Some(b"private-path-token-canary".to_vec()),
        ] {
            probe.output = output;
            probe.success = true;
            assert_eq!(
                stage
                    .require_host_files_with(&settings, &probe)
                    .unwrap_err()
                    .to_string(),
                "host.file_helper_unsupported"
            );
        }
        probe.output = Some(br#"{"schema":1,"host_files_v2":true}"#.to_vec());
        probe.success = false;
        assert!(stage.require_host_files_with(&settings, &probe).is_err());
        probe.success = true;
        stage.require_host_files_with(&settings, &probe).unwrap();
        assert_eq!(probe.calls.get(), 10);
        std::fs::write(stage.path(), b"replaced signed helper").unwrap();
        assert!(stage.require_host_files_with(&settings, &probe).is_err());
        assert_eq!(probe.calls.get(), 10, "changed helper must never execute");
    }

    #[test]
    fn host_delete_probe_is_separate_strict_bounded_and_revalidates_stage() {
        use super::super::native::{CommandRunner, NativeOutput, NativeRequest};
        struct Probe {
            path: PathBuf,
            delete: Vec<u8>,
            calls: std::cell::RefCell<Vec<Vec<std::ffi::OsString>>>,
            tamper: bool,
        }
        impl CommandRunner for Probe {
            fn execute(&self, _: &NativeRequest) -> io::Result<NativeOutput> {
                panic!("mutating runner must never be used");
            }
            fn execute_read_only(&self, request: &NativeRequest) -> io::Result<NativeOutput> {
                let NativeRequest::Process { tool, args } = request else {
                    panic!("process probe only");
                };
                assert_eq!(tool, &self.path);
                self.calls.borrow_mut().push(args.clone());
                let stdout = if args == &["capabilities"] {
                    br#"{"schema":1,"host_files_v2":true}"#.to_vec()
                } else {
                    assert_eq!(args, &["capabilities", "files-delete"]);
                    if self.tamper {
                        std::fs::write(tool, b"changed signed helper")?;
                    }
                    self.delete.clone()
                };
                Ok(NativeOutput {
                    success: true,
                    code: 0,
                    stdout,
                })
            }
        }
        let root = storage::private_test_directory();
        let stage = fixture(root.path());
        let mut settings = crate::lifecycle::Settings {
            filesystem: Some(festerm_tunnel_bridge::file_protocol::Policy {
                root: root.path().into(),
                write: true,
                delete: false,
            }),
            ordinary_access: crate::lifecycle::OrdinaryAccess::ProviderOwner,
            friendly_name: "Delete probe fixture".into(),
            allow_creation: false,
            startup: crate::lifecycle::Startup::Manual,
        };
        let mut probe = Probe {
            path: stage.path().into(),
            delete: Vec::new(),
            calls: Default::default(),
            tamper: false,
        };
        stage.require_host_files_with(&settings, &probe).unwrap();
        assert_eq!(
            &*probe.calls.borrow(),
            &[vec![std::ffi::OsString::from("capabilities")]],
            "old write grant never invokes the new feature probe"
        );
        settings.filesystem.as_mut().unwrap().delete = true;
        for bytes in [
            Vec::new(),
            vec![b'x'; 1025],
            br#"{"schema":1,"host_files_delete_v3":false}"#.to_vec(),
            br#"{"schema":2,"host_files_delete_v3":true}"#.to_vec(),
            br#"{"schema":1,"host_files_delete_v3":true,"future":true}"#.to_vec(),
            br#"{"schema":1,"host_files_v2":true}"#.to_vec(),
        ] {
            probe.calls.borrow_mut().clear();
            probe.delete = bytes;
            assert_eq!(
                stage
                    .require_host_files_with(&settings, &probe)
                    .unwrap_err()
                    .to_string(),
                "host.file_helper_unsupported"
            );
            assert_eq!(probe.calls.borrow().len(), 2);
        }
        probe.delete = br#"{"schema":1,"host_files_delete_v3":true}"#.to_vec();
        stage.require_host_files_with(&settings, &probe).unwrap();
        probe.tamper = true;
        assert!(
            stage.require_host_files_with(&settings, &probe).is_err(),
            "successful probe cannot authorize changed staged bytes"
        );
    }

    #[test]
    fn microsoft_resources_are_pinned_but_not_first_party_task_executables() {
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::windows_conpty_staging_fixture();
        let mut qualified = Vec::new();
        let stage = TrustedStage::publish_target_with(
            &artifact,
            &root.path().join("runtime"),
            "1.2.3",
            Platform::Windows,
            Architecture::Aarch64,
            |_, path| {
                qualified.push(path.file_name().unwrap().to_owned());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(qualified.len(), 5);
        assert!(!qualified
            .iter()
            .any(|name| name == "conpty.dll" || name == "OpenConsole.exe"));
        assert_eq!(stage.resources.len(), 2);
        assert!(stage
            .sessiond_path()
            .unwrap()
            .ends_with("festerm-sessiond.exe"));
        let resource = &stage.resources[0].path;
        // Owner writes are permitted; retained identity alone is not trust.
        // Keep length and identity unchanged so the digest check must catch it.
        let mut changed = std::fs::read(resource).unwrap();
        changed[0] ^= 1;
        std::fs::write(resource, changed).unwrap();
        assert!(stage.revalidate().is_err());
    }

    #[test]
    fn native_publisher_admission_never_substitutes_generic_signature_trust() {
        assert!(native_publisher_admission("windows").is_ok());
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::host_staging_fixture();
        #[cfg(target_os = "linux")]
        TrustedStage::publish(&artifact, &root.path().join("linux-public"))
            .unwrap()
            .revalidate()
            .unwrap();
        #[cfg(windows)]
        {
            assert!(TrustedStage::publish(&artifact, &root.path().join("public")).is_err());
            assert!(root.path().join("public").exists());
        }
        #[cfg(target_os = "macos")]
        {
            assert!(TrustedStage::publish(&artifact, &root.path().join("public")).is_err());
            let path = root
                .path()
                .join("public")
                .join(format!("executable-{}", artifact.manifest().sha256))
                .join("festerm-host");
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let cache = root.path().join("artifacts");
        assert!(
            TrustedStage::publish_with(&artifact, &cache, |_, _| Err(refused(
                "fixture.native_publisher_rejected"
            )))
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = cache
                .join(format!("executable-{}", artifact.manifest().sha256))
                .join("festerm-host");
            assert_eq!(
                std::fs::metadata(executable).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn macos_publisher_policy_requires_exact_team_nonadhoc_and_hardened_runtime() {
        let approved = "TeamIdentifier=F884J4784J\nCodeDirectory v=20500 size=320 flags=0x10000(runtime) hashes=1+2\n";
        macos_publisher_metadata(approved.as_bytes()).unwrap();
        for rejected in [
            approved.replace("F884J4784J", "OTHERTEAM1"),
            approved.replace("F884J4784J", "not set"),
            approved.replace("0x10000(runtime)", "0x0(none)"),
            approved.replace("0x10000(runtime)", "0x10002(adhoc,runtime)"),
            format!("{approved}TeamIdentifier=F884J4784J\n"),
            approved.replace("0x10000(runtime)", "invalid"),
        ] {
            assert!(macos_publisher_metadata(rejected.as_bytes()).is_err());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_gate_rejects_apple_signed_foreign_publisher() {
        let path = Path::new("/usr/bin/true");
        let file = File::open(path).unwrap();
        assert!(native_publisher_qualification(&file, path).is_err());
    }

    #[test]
    fn authenticated_executable_publication_keeps_data_private_and_refuses_changed_cache() {
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::host_staging_fixture();
        let cache = root.path().join("artifacts");
        assert!(TrustedStage::publish_with(
            &crate::artifacts::staging_fixture(),
            &cache,
            |_, _| Ok(())
        )
        .is_err());
        assert!(!cache.exists());
        let stage = TrustedStage::publish_with(&artifact, &cache, |_, _| Ok(())).unwrap();
        assert_eq!(
            std::fs::read(stage.path()).unwrap(),
            artifact
                .entries()
                .iter()
                .find(|entry| entry.name().starts_with("festerm-host"))
                .unwrap()
                .bytes()
        );
        stage.revalidate().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                stage.file.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(stage._staged.path().join("festerm-host"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            std::fs::write(stage.path(), b"changed").unwrap();
            assert!(stage.revalidate().is_err());
            assert!(TrustedStage::publish_with(&artifact, &cache, |_, _| Ok(())).is_err());
            assert_eq!(std::fs::read(stage.path()).unwrap(), b"changed");
        }
    }

    #[test]
    fn failed_native_qualification_never_adds_execute_permission() {
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::host_staging_fixture();
        let cache = root.path().join("artifacts");
        assert!(
            TrustedStage::publish_with(&artifact, &cache, |_, _| Err(refused(
                "fixture.native_publisher_rejected"
            )))
            .is_err()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = cache
                .join(format!("executable-{}", artifact.manifest().sha256))
                .join("festerm-host");
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn authenticated_runtime_bundle_qualifies_every_helper_and_fences_changed_sessiond() {
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::host_staging_fixture();
        let cache = root.path().join("runtime");
        let mut qualified = Vec::new();
        let stage = TrustedStage::publish_with(&artifact, &cache, |_, path| {
            qualified.push(path.file_name().unwrap().to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            qualified.len(),
            artifact
                .entries()
                .iter()
                .filter(|entry| entry.executable())
                .count()
        );
        let helper = stage.sessiond_path().unwrap().to_owned();
        assert_eq!(helper.parent(), stage.path().parent());
        assert!(qualified.contains(&helper.file_name().unwrap().to_owned()));
        assert!(stage.provenance_path().join(".manifest.json.sig").is_file());
        let expected = artifact
            .entries()
            .iter()
            .find(|entry| entry.name() == helper.file_name().unwrap().to_str().unwrap())
            .unwrap();
        assert_eq!(std::fs::read(&helper).unwrap(), expected.bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&helper).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(stage.provenance_path().join(expected.name()))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::write(&helper, b"changed helper").unwrap();
        assert!(stage.sessiond_path().is_err());
        assert!(stage.revalidate().is_err());
        assert!(TrustedStage::publish_with(&artifact, &cache, |_, _| Ok(())).is_err());
        assert_eq!(std::fs::read(&helper).unwrap(), b"changed helper");
    }

    #[test]
    fn authenticated_runtime_bundle_helper_rejection_never_promotes_fresh_host() {
        let root = storage::private_test_directory();
        let artifact = crate::artifacts::host_staging_fixture();
        let cache = root.path().join("runtime");
        let mut calls = 0;
        assert!(TrustedStage::publish_with(&artifact, &cache, |_, _| {
            calls += 1;
            if calls == 1 {
                Ok(())
            } else {
                Err(refused("fixture.helper_publisher_rejected"))
            }
        })
        .is_err());
        assert_eq!(calls, 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let directory = cache.join(format!("executable-{}", artifact.manifest().sha256));
            for entry in std::fs::read_dir(directory).unwrap() {
                assert_eq!(
                    entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }
}
