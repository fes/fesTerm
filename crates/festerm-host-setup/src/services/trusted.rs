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
        Self::publish_with(artifact, root, native_signature)
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

fn native_signature(file: &File, path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use super::native::{CommandRunner, NativeRequest, NativeRunner};
        let result = NativeRunner.execute(&NativeRequest::Process {
            tool: "/usr/bin/codesign".into(),
            args: ["--verify", "--strict", "--all-architectures"]
                .into_iter()
                .map(Into::into)
                .chain([path.as_os_str().to_owned()])
                .collect(),
        })?;
        if !result.success {
            return Err(refused("host.service_native_signature_required"));
        }
    }
    #[cfg(windows)]
    festerm_windows_security::host_service::verify_authenticode(file, path)?;
    let _ = (file, path);
    Ok(())
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
