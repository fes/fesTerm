//! Offline staging through reviewed CLI pins and the shared artifact verifier.
//! No downloaded bytes are executed and no service is registered.

use crate::artifacts::{self, Architecture, ExpectedArtifact, Package, Platform, VerifiedArtifact};
use festerm_tunnel_bridge::{cli_pins, setup_storage as storage};
use fs2::FileExt;
use std::{
    fs::File,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Complete, immutable staged bytes, not a running/registered host.
/// Keep this directory handle alive when consuming its paths.
pub struct StagedArtifact {
    path: PathBuf,
    _directory: File,
}

impl StagedArtifact {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Shared desktop/headless offline artifact policy: only this release/target,
/// existing updater trust, bounded no-follow private input, no downgrade switch.
pub fn stage_release_files(
    manifest: &Path,
    signature: &Path,
    archive: &Path,
    root: &Path,
) -> io::Result<StagedArtifact> {
    let platform = match std::env::consts::OS {
        "linux" => Platform::Linux,
        "macos" => Platform::Macos,
        "windows" => Platform::Windows,
        _ => return Err(io::Error::other("host.unsupported_artifact_target")),
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => Architecture::X86_64,
        "aarch64" => Architecture::Aarch64,
        _ => return Err(io::Error::other("host.unsupported_artifact_target")),
    };
    let manifest = read_input(manifest, artifacts::MAX_MANIFEST_BYTES)?;
    let signature = read_input(signature, artifacts::MAX_SIGNATURE_BYTES)?;
    let archive = read_input(archive, artifacts::MAX_ARCHIVE_BYTES)?;
    let artifact = artifacts::verify(
        &manifest,
        &signature,
        &archive,
        &ExpectedArtifact {
            version: crate::VERSION,
            platform,
            architecture,
            package: Package::Helpers,
        },
    )
    .map_err(|_| io::Error::other("host.artifact_authentication_failed"))?;
    stage_artifact(&artifact, root)
}

fn read_input(path: &Path, max: usize) -> io::Result<Vec<u8>> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("host.artifact_input_path"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("host.artifact_input_path"))?;
    let directory = storage::private_directory(parent, false)?;
    let file = storage::open_private_file(&directory, Path::new(name), false)?;
    let length = usize::try_from(file.metadata()?.len())
        .ok()
        .filter(|length| *length <= max)
        .ok_or_else(|| io::Error::other("host.artifact_input_limit"))?;
    Ok(storage::read_private(&directory, Path::new(name), length)?.to_vec())
}

/// Stage authenticated entries and exact signed provenance without executing.
/// On restart, reverify retained provenance and call this again to compare every
/// cached entry before any execution. A completion marker alone is not trust.
pub fn stage_artifact(artifact: &VerifiedArtifact, root: &Path) -> io::Result<StagedArtifact> {
    let manifest = artifact.manifest();
    let key = format!(
        "helpers-{}-{}-{}-{}",
        manifest.version,
        manifest.platform.name(),
        manifest.architecture.name(),
        manifest.sha256
    );
    let _root = storage::private_directory(root, true)?;
    let path = root.join(key);
    let directory = storage::private_directory(&path, true)?;
    let lock_name = Path::new(".publish.lock");
    let lock = match storage::open_private_file(&directory, lock_name, true) {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            storage::open_private_file(&directory, lock_name, false)?
        }
        Err(error) => return Err(error),
    };
    lock.try_lock_exclusive()?;
    let _lease = PublicationLease(lock);
    for entry in artifact.entries() {
        publish(&directory, Path::new(entry.name()), entry.bytes())?;
    }
    publish(
        &directory,
        Path::new(".manifest.json"),
        artifact.manifest_bytes(),
    )?;
    publish(
        &directory,
        Path::new(".manifest.json.sig"),
        artifact.signature_bytes(),
    )?;
    publish(&directory, Path::new(".archive"), artifact.archive_bytes())?;
    publish(
        &directory,
        Path::new(".complete"),
        manifest.sha256.as_bytes(),
    )?;
    Ok(StagedArtifact {
        path,
        _directory: directory,
    })
}

struct PublicationLease(File);

impl Drop for PublicationLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn publish(directory: &File, name: &Path, bytes: &[u8]) -> io::Result<()> {
    match storage::read_private(directory, name, bytes.len()) {
        Ok(existing) if existing.as_slice() == bytes => return Ok(()),
        Ok(_) => return Err(io::Error::other("host.staged_artifact_mismatch")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let staging = PathBuf::from(format!(".stage-{}", uuid::Uuid::new_v4()));
    let mut output = storage::open_private_file(directory, &staging, true)?;
    let result = (|| {
        output.write_all(bytes)?;
        output.sync_all()?;
        #[cfg(unix)]
        {
            festerm_unix_security::rename_noreplace(directory, &staging, directory, name)?;
            directory.sync_all()?;
        }
        #[cfg(windows)]
        festerm_windows_security::rename_file_noreplace(&output, directory, name)?;
        Ok(())
    })();
    if result.is_err() {
        #[cfg(unix)]
        let _ = festerm_unix_security::remove_private_staging_file(directory, &staging);
        #[cfg(windows)]
        let _ = festerm_windows_security::remove_private_staging_file(&output);
    }
    result
}

/// Copies only the exactly pinned executable from a private source directory.
/// Does not download, execute, sign in, register a service or provision a shell.
pub fn stage_cli(source: &Path, destination: &Path) -> io::Result<PathBuf> {
    let pin = cli_pins::current()?;
    stage_with_pin(source, destination, &pin)
}

fn stage_with_pin(
    source: &Path,
    destination: &Path,
    pin: &cli_pins::CliPin,
) -> io::Result<PathBuf> {
    let name = format!("devtunnel{}", std::env::consts::EXE_SUFFIX);
    let name = Path::new(&name);
    let input = storage::private_directory(source, false)?;
    let bytes = storage::read_private(&input, name, cli_pins::DOWNLOAD_LIMIT as usize)?;
    if !cli_pins::matches_digest(bytes.as_slice(), &pin.executable_sha256)? {
        return Err(io::Error::other(
            "host.cli_pin_mismatch_dependency_update_required",
        ));
    }
    let output = storage::private_directory(destination, true)?;
    match storage::write_new_private(&output, name, &bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let existing = storage::read_private(&output, name, cli_pins::DOWNLOAD_LIMIT as usize)?;
            if !cli_pins::matches_digest(existing.as_slice(), &pin.executable_sha256)? {
                return Err(io::Error::other("host.cached_cli_mismatch"));
            }
        }
        Err(error) => return Err(error),
    }
    // New files remain non-executable. Execution adapters must reopen and verify
    // via cli_pins::verify before setting executable permissions/using the CLI.
    Ok(destination.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn authenticated_artifact_staging_retains_signed_provenance_and_revalidates_cache() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let verified = crate::artifacts::staging_fixture();
        let cache = root.path().join("cache");
        let staged = stage_artifact(&verified, &cache).unwrap();
        let path = staged.path().to_path_buf();
        let directory = storage::private_directory(&path, false).unwrap();
        for entry in verified.entries() {
            assert_eq!(
                storage::read_private(&directory, Path::new(entry.name()), entry.bytes().len())
                    .unwrap()
                    .as_slice(),
                entry.bytes()
            );
        }
        for (name, bytes) in [
            (".manifest.json", verified.manifest_bytes()),
            (".manifest.json.sig", verified.signature_bytes()),
            (".archive", verified.archive_bytes()),
        ] {
            assert_eq!(
                storage::read_private(&directory, Path::new(name), bytes.len())
                    .unwrap()
                    .as_slice(),
                bytes
            );
        }
        drop(staged);
        assert_eq!(stage_artifact(&verified, &cache).unwrap().path(), path);
        let name = Path::new(verified.entries()[0].name());
        storage::replace_private(&directory, name, b"changed").unwrap();
        assert!(stage_artifact(&verified, &cache).is_err());
        assert_eq!(
            storage::read_private(&directory, name, 32)
                .unwrap()
                .as_slice(),
            b"changed"
        );
        assert!(path.join(".complete").exists());
    }

    #[test]
    fn authenticated_staging_preserves_exact_bytes_and_refuses_partial_or_changed_entries() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let directory = storage::private_directory(&root.path().join("stage"), true).unwrap();
        let name = Path::new("festerm-sessiond");
        let bytes = b"fixture verified upstream, never executed";
        publish(&directory, name, bytes).unwrap();
        publish(&directory, name, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.path().join("stage").join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        storage::replace_private(&directory, name, b"partial").unwrap();
        assert!(publish(&directory, name, bytes).is_err());
        assert_eq!(
            storage::read_private(&directory, name, bytes.len())
                .unwrap()
                .as_slice(),
            b"partial"
        );
        assert!(publish(&directory, Path::new("../escape"), bytes).is_err());
        assert!(!root.path().join("escape").exists());
    }

    #[test]
    fn offline_cli_staging_refuses_changed_bytes_and_never_overwrites_a_cache() {
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("cache");
        let directory = storage::private_directory(&source, true).unwrap();
        let name = format!("devtunnel{}", std::env::consts::EXE_SUFFIX);
        let name = Path::new(&name);
        let bytes = b"fixture only, never executed";
        storage::write_new_private(&directory, name, bytes).unwrap();
        let mut pin = cli_pins::current().unwrap();
        assert!(stage_with_pin(&source, &destination, &pin).is_err());
        assert!(!destination.exists());
        pin.executable_sha256 = Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = stage_with_pin(&source, &destination, &pin).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        stage_with_pin(&source, &destination, &pin).unwrap();
        let output = storage::private_directory(&destination, false).unwrap();
        storage::replace_private(&output, name, b"foreign").unwrap();
        assert!(stage_with_pin(&source, &destination, &pin).is_err());
        assert_eq!(std::fs::read(destination.join(name)).unwrap(), b"foreign");
    }
}
