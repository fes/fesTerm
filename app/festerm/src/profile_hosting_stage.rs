//! Installed helpers are sources, never persistent-service executable paths.

use festerm_tunnel_bridge::{cli_pins, profile_hosting, setup_storage};
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use std::{
    process::{Command, Stdio},
    time::Instant,
};

#[derive(Clone)]
pub(super) struct Helpers {
    pub setup: PathBuf,
    pub cli: PathBuf,
}

pub(super) fn initialize() -> io::Result<PathBuf> {
    let root = profile_hosting::namespace()?;
    initialize_in(&root, &profile_hosting::hosted_runtime()?)?;
    Ok(root)
}

fn initialize_in(root: &Path, runtime: &Path) -> io::Result<()> {
    let _root = festerm_sessiond::prepare_native_runtime(root)
        .map_err(|_| io::Error::other("profile_host.runtime_namespace"))?;
    let _runtime = festerm_sessiond::prepare_native_runtime(runtime)
        .map_err(|_| io::Error::other("profile_host.runtime_namespace"))?;
    for leaf in ["installations", "releases", "requests"] {
        setup_storage::private_directory(&root.join(leaf), true)?;
    }
    Ok(())
}

fn cache(root: &Path) -> PathBuf {
    root.join("releases").join(format!(
        "{}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ))
}

pub(super) fn stage() -> io::Result<Helpers> {
    let root = initialize()?;
    let root_directory = setup_storage::private_directory(&root, false)?;
    let lease =
        match setup_storage::open_private_file(&root_directory, Path::new("staging.lock"), true) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                setup_storage::open_private_file(&root_directory, Path::new("staging.lock"), false)?
            }
            Err(error) => return Err(error),
        };
    lease.try_lock_exclusive()?;
    let cache = cache(&root);
    let directory = setup_storage::private_directory(&cache, true)?;
    let app = std::env::current_exe()?;
    let package = app
        .parent()
        .ok_or_else(|| io::Error::other("profile_host.package_location"))?;
    let mut names = vec!["festerm-tunnel-setup", "festerm-tunnel-bridge"];
    if cfg!(windows) {
        names.push("festerm-tunnel-worker");
    }
    for stem in names {
        let name = format!("{stem}{}", std::env::consts::EXE_SUFFIX);
        let source = package.join(&name);
        let bytes = read_bounded(open_source(&source)?)?;
        install_executable(&directory, &cache, &name, &bytes)?;
        verify_signature(&cache.join(&name), Some(&app))?;
    }
    let cli = cache.join(format!("devtunnel{}", std::env::consts::EXE_SUFFIX));
    match cli_pins::verify(&cli) {
        Ok(verified) => verified.make_executable()?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let pin = cli_pins::current()?;
            let client = reqwest::blocking::Client::builder()
                .https_only(true)
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(90))
                .build()
                .map_err(|_| io::Error::other("profile_host.download_unavailable"))?;
            let response = client
                .get(&pin.download)
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
                .map_err(|_| io::Error::other("profile_host.download_failed"))?;
            let archive = read_bounded(response)?;
            let bytes = decode_download(&pin, archive)?;
            let name = cli
                .file_name()
                .ok_or_else(|| io::Error::other("profile_host.cli_path"))?;
            install_executable(&directory, &cache, name, &bytes)?;
            cli_pins::verify(&cli)?;
        }
        Err(error) => return Err(error),
    }
    verify_signature(&cli, None)?;
    let mut runtime_check = Command::new(&cli);
    runtime_check.arg("--version");
    checked(runtime_check)
        .map_err(|_| io::Error::other("profile_host.cli_runtime_prerequisites"))?;
    Ok(Helpers {
        setup: cache.join(format!(
            "festerm-tunnel-setup{}",
            std::env::consts::EXE_SUFFIX
        )),
        cli,
    })
}

fn read_bounded(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(cli_pins::DOWNLOAD_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cli_pins::DOWNLOAD_LIMIT {
        return Err(io::Error::other("profile_host.executable_size"));
    }
    Ok(bytes)
}

fn decode_download(pin: &cli_pins::CliPin, archive: Vec<u8>) -> io::Result<Vec<u8>> {
    if !cli_pins::matches_digest(archive.as_slice(), &pin.archive_sha256)? {
        return Err(io::Error::other("profile_host.archive_pin_mismatch"));
    }
    let bytes = match pin.format.as_str() {
        "executable" => archive,
        "zip" => {
            let mut zip = zip::ZipArchive::new(io::Cursor::new(archive))
                .map_err(|_| io::Error::other("profile_host.cli_archive"))?;
            if zip.len() != 1 {
                return Err(io::Error::other("profile_host.cli_archive"));
            }
            let entry = zip
                .by_index(0)
                .map_err(|_| io::Error::other("profile_host.cli_archive"))?;
            if entry.name() != "devtunnel"
                || !entry.is_file()
                || entry
                    .unix_mode()
                    .is_some_and(|mode| mode & 0o170000 == 0o120000)
            {
                return Err(io::Error::other("profile_host.cli_archive"));
            }
            read_bounded(entry)?
        }
        _ => return Err(io::Error::other("profile_host.cli_archive")),
    };
    if !cli_pins::matches_digest(bytes.as_slice(), &pin.executable_sha256)? {
        return Err(io::Error::other("profile_host.cli_pin_mismatch"));
    }
    Ok(bytes)
}

fn open_source(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        let directory = festerm_windows_security::lock_directory_path_without_delete_sharing(
            path.parent()
                .ok_or_else(|| io::Error::other("profile_host.package_location"))?,
        )?;
        festerm_windows_security::open_file_no_reparse(
            &directory,
            Path::new(
                path.file_name()
                    .ok_or_else(|| io::Error::other("profile_host.package_location"))?,
            ),
        )
    }
    #[cfg(unix)]
    {
        use std::{os::unix::fs::MetadataExt, path::Component};
        let current_user = festerm_unix_security::current_user_id()?;
        let parent = path
            .parent()
            .filter(|parent| parent.is_absolute())
            .ok_or_else(|| io::Error::other("profile_host.package_location"))?;
        let mut directory = File::open("/")?;
        for component in parent.components() {
            if let Component::Normal(name) = component {
                directory = festerm_unix_security::open_directory_read_nofollow(
                    &directory,
                    Path::new(name),
                )?;
            } else if component != Component::RootDir {
                return Err(io::Error::other("profile_host.package_location"));
            }
            festerm_unix_security::trusted_packaged_executable_ancestor(&directory)?;
        }
        let file = festerm_unix_security::open_file_nofollow(
            &directory,
            Path::new(
                path.file_name()
                    .ok_or_else(|| io::Error::other("profile_host.package_location"))?,
            ),
        )?;
        let metadata = file.metadata()?;
        if (metadata.uid() != 0 && metadata.uid() != current_user) || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other("profile_host.package_location"));
        }
        festerm_unix_security::secure_staging_parent(&file)?;
        Ok(file)
    }
}

fn install_executable(
    directory: &File,
    cache: &Path,
    name: impl AsRef<Path>,
    bytes: &[u8],
) -> io::Result<()> {
    let name = name.as_ref();
    let destination = cache.join(name);
    match open_source(&destination) {
        Ok(mut existing) => {
            let expected: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if !cli_pins::matches_digest(&mut existing, &expected)? {
                return Err(io::Error::other("profile_host.cached_helper_mismatch"));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::{io::Write, os::unix::fs::PermissionsExt};
                let staging = PathBuf::from(format!(
                    ".{}.staging-{}",
                    name.display(),
                    uuid::Uuid::new_v4().simple()
                ));
                let mut output =
                    festerm_unix_security::open_private_file(directory, &staging, true)?;
                let result = (|| {
                    output.write_all(bytes)?;
                    output.set_permissions(std::fs::Permissions::from_mode(0o700))?;
                    output.sync_all()?;
                    festerm_unix_security::replace_private_file(directory, &staging, name)?;
                    directory.sync_all()
                })();
                if result.is_err() {
                    let _ = festerm_unix_security::remove_private_staging_file(directory, &staging);
                }
                result?;
                return Ok(());
            }
            #[cfg(not(unix))]
            setup_storage::replace_private(directory, name, bytes)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let output = open_source(&destination)?;
        output.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        output.sync_all()?;
    }
    Ok(())
}

fn checked(mut command: Command) -> io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("profile_host.signature_required"))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("profile_host.signature_timeout"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn verify_signature(path: &Path, app: Option<&Path>) -> io::Result<()> {
    #[cfg(windows)]
    {
        fn literal(path: &Path) -> io::Result<String> {
            let text = path
                .to_str()
                .filter(|text| !text.chars().any(char::is_control))
                .ok_or_else(|| io::Error::other("profile_host.package_location"))?;
            Ok(format!("'{}'", text.replace('\'', "''")))
        }
        let (_, system) = festerm_windows_security::current_user_service_context()?;
        let mut script = format!(
            "$ErrorActionPreference='Stop'; $s=Get-AuthenticodeSignature -LiteralPath {}; \
             if ($s.Status -ne 'Valid') {{ exit 1 }};",
            literal(path)?
        );
        if let Some(app) = app {
            script.push_str(&format!(
                "$a=Get-AuthenticodeSignature -LiteralPath {}; \
                 if ($a.Status -ne 'Valid' -or \
                 $a.SignerCertificate.Subject -cne $s.SignerCertificate.Subject) {{ exit 1 }};",
                literal(app)?
            ));
        }
        let mut command = Command::new(
            system
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe"),
        );
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        checked(command)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        let mut command = Command::new("/usr/bin/codesign");
        command
            .args(["--verify", "--strict", "-R=anchor apple generic"])
            .arg(path);
        checked(command)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (path, app);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(windows)]
    #[test]
    fn hosting_initialization_keeps_inherited_windows_acls_and_legacy_registry() {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        fn powershell(path: &Path, script: &str) -> Vec<u8> {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ]);
            command.env("FESTERM_ACL_FIXTURE", path);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "isolated ACL fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }
        let parents = [
            Some(std::env::temp_dir()),
            std::env::var_os("SystemRoot").map(|root| PathBuf::from(root).join("Temp")),
        ];
        let fixture = parents
            .into_iter()
            .flatten()
            .find_map(|parent| {
                let path = parent.join(format!("festerm-host-acl-{}", uuid::Uuid::new_v4()));
                match festerm_sessiond::prepare_native_runtime(&path) {
                    Ok(_) => Some(Fixture(path)),
                    Err(_) => {
                        let _ = std::fs::remove_dir(&path);
                        None
                    }
                }
            })
            .expect("a trusted temporary parent is required");
        powershell(
            &fixture.0,
            r#"
            $ErrorActionPreference = 'Stop'
            $user = [Security.Principal.WindowsIdentity]::GetCurrent().User
            $acl = [Security.AccessControl.DirectorySecurity]::new()
            $acl.SetOwner($user)
            $acl.SetAccessRuleProtection($true, $false)
            foreach ($sid in @($user, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'), [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))) {
                $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
            }
            [IO.Directory]::SetAccessControl($env:FESTERM_ACL_FIXTURE, $acl)
            $product = Join-Path $env:FESTERM_ACL_FIXTURE 'fesTerm'
            $legacy = Join-Path $product 'sessiond'
            [IO.Directory]::CreateDirectory($legacy) | Out-Null
            foreach ($path in @($product, $legacy)) {
                $inherited = [IO.Directory]::GetAccessControl($path)
                $inherited.SetOwner($user)
                [IO.Directory]::SetAccessControl($path, $inherited)
                if ($inherited.AreAccessRulesProtected -or @($inherited.Access | Where-Object { -not $_.IsInherited }).Count -ne 0) {
                    throw 'Fixture did not retain inherited access rules.'
                }
            }
        "#,
        );
        let product = fixture.0.join("fesTerm");
        let legacy = product.join("sessiond");
        let snapshot = "[IO.Directory]::GetAccessControl($env:FESTERM_ACL_FIXTURE).GetSecurityDescriptorSddlForm('All')";
        let product_acl = powershell(&product, snapshot);
        let legacy_acl = powershell(&legacy, snapshot);
        assert!(setup_storage::private_directory(&product, false).is_err());
        assert!(setup_storage::private_directory(&legacy, false).is_err());
        std::fs::write(legacy.join("registry.json"), b"legacy evidence").unwrap();
        let root = product.join("profile-hosts");
        let runtime = root.join("runtime").join("fesTerm").join("sessiond");
        initialize_in(&root, &runtime).unwrap();
        assert!(
            product_acl == powershell(&product, snapshot),
            "product ACL changed"
        );
        assert!(
            legacy_acl == powershell(&legacy, snapshot),
            "legacy ACL changed"
        );
        assert_eq!(
            std::fs::read(legacy.join("registry.json")).unwrap(),
            b"legacy evidence"
        );
        assert_eq!(std::fs::read_dir(&legacy).unwrap().count(), 1);
        setup_storage::private_directory(&root, false).unwrap();
        setup_storage::private_directory(&runtime, false).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hosting_initialization_keeps_legacy_permissions_and_session_files_unchanged() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::Builder::new()
            .prefix("festerm-legacy-host-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let product = fixture.path().join("festerm");
        let legacy = product.join("sessiond");
        std::fs::create_dir_all(&legacy).unwrap();
        for directory in [&product, &legacy] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(legacy.join("registry.json"), b"legacy evidence").unwrap();
        let root = product.join("profile-hosts");
        let runtime = root.join("runtime").join("festerm").join("sessiond");
        initialize_in(&root, &runtime).unwrap();
        for directory in [&product, &legacy] {
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        assert_eq!(
            std::fs::read(legacy.join("registry.json")).unwrap(),
            b"legacy evidence"
        );
        setup_storage::private_directory(&root, false).unwrap();
        setup_storage::private_directory(&runtime, false).unwrap();
        assert_eq!(std::fs::read_dir(&legacy).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn staged_helpers_publish_executable_bytes_and_recover_interrupted_mode() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::Builder::new()
            .prefix("festerm-helper-mode-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = setup_storage::private_directory(root.path(), false).unwrap();
        let name = Path::new("festerm-tunnel-setup");
        let path = root.path().join(name);
        let bytes = b"#!/bin/sh\nexit 0\n";
        install_executable(&directory, root.path(), name, bytes).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        checked(Command::new(&path)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        install_executable(&directory, root.path(), name, bytes).unwrap();
        checked(Command::new(&path)).unwrap();
        assert!(install_executable(&directory, root.path(), name, b"foreign").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn pinned_archive_accepts_only_the_expected_flat_executable_and_both_hashes() {
        let mut archive = io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut archive);
            writer
                .start_file("devtunnel", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"reviewed executable").unwrap();
            writer.finish().unwrap();
        }
        let archive = archive.into_inner();
        let mut pin = cli_pins::current().unwrap();
        pin.format = "zip".into();
        pin.archive_sha256 = digest(&archive);
        pin.executable_sha256 = digest(b"reviewed executable");
        assert_eq!(
            decode_download(&pin, archive.clone()).unwrap(),
            b"reviewed executable"
        );
        pin.executable_sha256 = digest(b"other executable");
        assert!(decode_download(&pin, archive.clone()).is_err());
        pin.archive_sha256 = digest(b"other archive");
        assert!(decode_download(&pin, archive).is_err());
    }
}
