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
}

impl TrustedStage {
    pub fn publish(artifact: &VerifiedArtifact, root: &Path) -> io::Result<Self> {
        native_publisher_admission(std::env::consts::OS)?;
        Self::publish_with(artifact, root, native_publisher_qualification)
    }

    fn publish_with(
        artifact: &VerifiedArtifact,
        root: &Path,
        signature: impl FnOnce(&File, &Path) -> io::Result<()>,
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
        let manifest = artifact.manifest();
        if manifest.package != Package::Host
            || manifest.schema != 2
            || manifest.version != crate::VERSION
            || manifest.platform != platform
            || manifest.architecture != architecture
        {
            return Err(refused(
                "host.service_current_native_authenticated_bundle_required",
            ));
        }
        let name = format!("festerm-host{}", std::env::consts::EXE_SUFFIX);
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
            file.sync_all()?;
        }
        let receipt = Self {
            path,
            digest: expected,
            file,
            _directory: directory,
            _staged: staged,
        };
        receipt.revalidate()?;
        Ok(receipt)
    }
}

fn native_publisher_qualification(_: &File, path: &Path) -> io::Result<()> {
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
    let _ = path;
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
        "linux" | "macos" => Ok(()),
        "windows" => Err(refused(
            "host.service_windows_release_publisher_policy_unconfigured",
        )),
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

impl TrustedStage {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn revalidate(&self) -> io::Result<()> {
        let current = open(&self.path)?;
        if !same(&current, &self.file)? {
            return Err(refused("host.service_executable_replaced"));
        }
        check(&current, self.file.metadata()?.len(), &self.digest)
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
    fn native_publisher_admission_never_substitutes_generic_signature_trust() {
        assert!(native_publisher_admission("windows").is_err());
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
            assert!(!root.path().join("public").exists());
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
            TrustedStage::publish_with(&artifact, &cache, |_, _| native_publisher_admission(
                "windows"
            ))
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
}
