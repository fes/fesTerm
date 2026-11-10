//! Offline staging of Microsoft's CLI using the release's existing reviewed pins.
//! fesTerm-owned packages require the separately authenticated artifact verifier.

use festerm_tunnel_bridge::{cli_pins, setup_storage as storage};
use std::{
    io,
    path::{Path, PathBuf},
};

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
    // Keep staging non-executable. Execution adapters must reopen and verify
    // via cli_pins::verify before setting executable permissions/using the CLI.
    Ok(destination.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

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
