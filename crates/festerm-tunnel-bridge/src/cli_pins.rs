//! Reviewed official CLI bytes, shared by desktop staging and managed workers.

use crate::setup_storage;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

pub const DOWNLOAD_LIMIT: u64 = 128 * 1024 * 1024;
pub const DOWNLOAD_VERSION: &str = "1.0.2115+e16833f284";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliPin {
    pub platform: String,
    pub architecture: String,
    pub download: String,
    pub format: String,
    pub archive_sha256: String,
    pub executable_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    reviewed: String,
    pins: Vec<CliPin>,
}

pub fn current() -> io::Result<CliPin> {
    legacy_for_target(std::env::consts::OS, std::env::consts::ARCH)
}

fn legacy_for_target(platform: &str, architecture: &str) -> io::Result<CliPin> {
    let architecture = if platform == "windows" && architecture == "aarch64" {
        "x86_64"
    } else {
        architecture
    };
    for_target(platform, architecture)
}

/// Exact native-target pin selection without legacy Windows ARM x64 emulation.
pub fn for_target(platform: &str, architecture: &str) -> io::Result<CliPin> {
    let manifest: Manifest = serde_json::from_str(include_str!("../devtunnel-cli-pins.json"))
        .map_err(|_| io::Error::other("profile_host.cli_manifest"))?;
    if manifest.reviewed.is_empty() {
        return Err(io::Error::other("profile_host.cli_manifest"));
    }
    manifest
        .pins
        .into_iter()
        .find(|pin| pin.platform == platform && pin.architecture == architecture)
        .ok_or_else(|| io::Error::other("profile_host.cli_platform"))
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    #[test]
    fn reviewed_cli_pins_use_versioned_raw_assets_and_no_windows_arm_fallback() {
        for (platform, architecture) in [
            ("windows", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
        ] {
            let pin = for_target(platform, architecture).unwrap();
            assert!(pin.download.starts_with(&format!(
                "https://tunnelsassetsprod.blob.core.windows.net/cli/{DOWNLOAD_VERSION}/"
            )));
            assert_eq!(pin.format, "executable");
            assert_eq!(pin.archive_sha256, pin.executable_sha256);
        }
        assert!(for_target("windows", "aarch64").is_err());
        assert!(for_target("linux", "riscv64").is_err());
    }

    #[test]
    fn legacy_cli_selector_preserves_windows_arm_x64_emulation() {
        let legacy = legacy_for_target("windows", "aarch64").unwrap();
        let x64 = for_target("windows", "x86_64").unwrap();
        assert_eq!(legacy.architecture, "x86_64");
        assert_eq!(legacy.executable_sha256, x64.executable_sha256);
        assert_eq!(legacy.download, x64.download);
        let current = current().unwrap();
        let expected = legacy_for_target(std::env::consts::OS, std::env::consts::ARCH).unwrap();
        assert_eq!(current.architecture, expected.architecture);
        assert_eq!(current.executable_sha256, expected.executable_sha256);
    }
}

/// Keep the trusted directory and no-follow executable handles alive while
/// spawning/using the cached command; do not service from a downloaded script.
pub struct VerifiedCli {
    path: PathBuf,
    _directory: File,
    _executable: File,
}

impl VerifiedCli {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Recover an interrupted staging chmod on the already verified file handle.
    pub fn make_executable(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            self._executable
                .set_permissions(std::fs::Permissions::from_mode(0o700))?;
            self._executable.sync_all()?;
        }
        Ok(())
    }
}

pub fn verify(path: &Path) -> io::Result<VerifiedCli> {
    verify_with_pin(path, &current()?)
}

fn verify_with_pin(path: &Path, pin: &CliPin) -> io::Result<VerifiedCli> {
    let name = format!("devtunnel{}", std::env::consts::EXE_SUFFIX);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new(&name)) {
        return Err(io::Error::other("profile_host.cli_path"));
    }
    let directory = setup_storage::private_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("profile_host.cli_path"))?,
        false,
    )?;
    #[cfg(windows)]
    let mut executable = {
        let file = festerm_windows_security::open_file_no_reparse(&directory, Path::new(&name))?;
        if !file.metadata()?.is_file() || !festerm_windows_security::is_current_user_only(&file)? {
            return Err(io::Error::other("profile_host.cli_permissions"));
        }
        file
    };
    #[cfg(unix)]
    let mut executable = {
        use std::os::unix::fs::MetadataExt;
        let file = festerm_unix_security::open_file_nofollow(&directory, Path::new(&name))?;
        let metadata = file.metadata()?;
        if metadata.uid() != festerm_unix_security::current_user_id()?
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::other("profile_host.cli_permissions"));
        }
        file
    };
    if !matches_digest(&mut executable, &pin.executable_sha256)? {
        return Err(io::Error::other("profile_host.cli_pin_mismatch"));
    }
    Ok(VerifiedCli {
        path: path.to_owned(),
        _directory: directory,
        _executable: executable,
    })
}

#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn verified_cli_repairs_interrupted_mode_only_after_pin_verification() {
        let root = crate::test_support::private_directory("festerm-cli-mode");
        let directory = setup_storage::private_directory(&root, false).unwrap();
        let path = root.join("devtunnel");
        let bytes = b"#!/bin/sh\nexit 0\n";
        setup_storage::replace_private(&directory, Path::new("devtunnel"), bytes).unwrap();
        let mut pin = current().unwrap();
        pin.executable_sha256 = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let verified = verify_with_pin(&path, &pin).unwrap();
        verified.make_executable().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(std::process::Command::new(verified.path())
            .status()
            .unwrap()
            .success());
        drop(verified);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&path, b"tampered").unwrap();
        assert!(verify_with_pin(&path, &pin).is_err());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(directory);
        std::fs::remove_dir_all(root).unwrap();
    }
}

pub fn matches_digest(reader: impl Read, expected: &str) -> io::Result<bool> {
    let mut reader = reader.take(DOWNLOAD_LIMIT + 1);
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > DOWNLOAD_LIMIT {
            return Err(io::Error::other("profile_host.cli_size"));
        }
        digest.update(&buffer[..count]);
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(actual == expected)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    #[ignore = "bounded child used by the mapped executable regression"]
    fn mapped_binary_child() {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }

    #[test]
    fn cached_cli_verification_opens_a_mapped_executable_read_only() {
        let state = crate::test_support::private_directory("festerm-cli-mapped");
        let directory = setup_storage::private_directory(&state, false).unwrap();
        let path = state.join("devtunnel.exe");
        let mut source = File::open(std::env::current_exe().unwrap()).unwrap();
        let mut output =
            setup_storage::open_private_file(&directory, Path::new("devtunnel.exe"), true).unwrap();
        io::copy(&mut source, &mut output).unwrap();
        output.sync_all().unwrap();
        drop(output);
        let mut child = Command::new(&path)
            .args([
                "--ignored",
                "--exact",
                "cli_pins::tests::mapped_binary_child",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let result = verify(&path);
        let _ = child.kill();
        child.wait().unwrap();
        let Err(error) = result else {
            panic!("the fixture must not match the official CLI pin")
        };
        assert_eq!(error.to_string(), "profile_host.cli_pin_mismatch");
        drop(directory);
        std::fs::remove_dir_all(state).unwrap();
    }
}
