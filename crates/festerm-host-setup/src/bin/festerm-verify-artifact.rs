//! Offline verifier. Bootstrap this executable from a separately trusted build,
//! not from the unverified archive it is supposed to authenticate.

use std::{fs::File, io::Read, path::Path};

use festerm_host_setup::artifacts::{
    self, Architecture, ExpectedArtifact, Package, Platform, MAX_ARCHIVE_BYTES, MAX_MANIFEST_BYTES,
    MAX_SIGNATURE_BYTES,
};

fn read_bounded(path: &Path, bound: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(bound as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > bound {
        return Err(artifacts::ArtifactError::Bounds.into());
    }
    Ok(bytes)
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let package = if args.first().is_some_and(|arg| arg == "--host") {
        args.remove(0);
        Package::Host
    } else {
        Package::Helpers
    };
    if args.len() != 4 {
        return Err("usage: festerm-verify-artifact [--host] VERSION linux|macos|windows x86_64|aarch64 ARCHIVE (no extraction/execution)".into());
    }
    let version = args[0].to_str().ok_or("invalid version")?;
    let platform = match args[1].to_str() {
        Some("linux") => Platform::Linux,
        Some("macos") => Platform::Macos,
        Some("windows") => Platform::Windows,
        _ => return Err("unsupported platform".into()),
    };
    let architecture = match args[2].to_str() {
        Some("x86_64") => Architecture::X86_64,
        Some("aarch64") => Architecture::Aarch64,
        _ => return Err("unsupported architecture".into()),
    };
    let expected = ExpectedArtifact {
        version,
        platform,
        architecture,
        package,
    };
    let archive_path = Path::new(&args[3]);
    if archive_path.file_name() != Some(std::ffi::OsStr::new(&artifacts::filename(&expected))) {
        return Err("archive filename does not match expected target".into());
    }
    let mut manifest_path = args[3].clone();
    manifest_path.push(".manifest.json");
    let mut signature_path = manifest_path.clone();
    signature_path.push(".sig");
    let verified = artifacts::verify(
        &read_bounded(Path::new(&manifest_path), MAX_MANIFEST_BYTES)?,
        &read_bounded(Path::new(&signature_path), MAX_SIGNATURE_BYTES)?,
        &read_bounded(archive_path, MAX_ARCHIVE_BYTES)?,
        &expected,
    )?;
    println!(
        "Authenticated {} ({} validated regular files; nothing extracted or executed)",
        verified.manifest().filename,
        verified.entries().len()
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}
