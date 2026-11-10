//! Dev Tunnel installer scripts and GUI-free helper artifact authentication.
//!
//! The scripts are embedded, not fetched, so what a user reviews in the app is
//! byte-for-byte what the matching GitHub release publishes for this version.

use sha2::{Digest, Sha256};

#[cfg(feature = "artifacts")]
pub mod artifacts;
#[cfg(feature = "lifecycle")]
pub mod lifecycle;
#[cfg(feature = "lifecycle")]
pub mod staging;

/// A host platform the installer supports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPlatform {
    MacOs,
    Linux,
    Windows,
}

impl HostPlatform {
    pub const ALL: [Self; 3] = [Self::MacOs, Self::Linux, Self::Windows];

    pub const fn label(self) -> &'static str {
        match self {
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        }
    }

    /// The installer file name as published on the release.
    pub const fn script_name(self) -> &'static str {
        match self {
            Self::Windows => "install-dev-tunnel.ps1",
            _ => "install-dev-tunnel.sh",
        }
    }

    /// The embedded installer text.
    pub const fn script(self) -> &'static str {
        match self {
            Self::Windows => POWERSHELL_INSTALLER,
            _ => BASH_INSTALLER,
        }
    }
}

/// Options the installer accepts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOptions {
    /// Allow a remote client to displace the current holder (the default).
    pub takeover: bool,
    /// Linux only: install missing runtime packages with sudo.
    pub install_dependencies: bool,
    /// Switch an existing host to this version's helpers, keeping its
    /// tunnel, host key and running session.
    pub upgrade: bool,
    /// Optional user-facing description for a newly created tunnel.
    pub friendly_name: String,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            takeover: true,
            install_dependencies: false,
            upgrade: false,
            friendly_name: String::new(),
        }
    }
}

pub const BASH_INSTALLER: &str = include_str!("../../../scripts/install-dev-tunnel.sh");
pub const POWERSHELL_INSTALLER: &str = include_str!("../../../scripts/install-dev-tunnel.ps1");

/// This build's version; the command pins the host bundle to it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The release asset URL for this build's installer.
pub fn script_url(platform: HostPlatform) -> String {
    format!(
        "https://github.com/fes/fesTerm/releases/download/v{VERSION}/{}",
        platform.script_name()
    )
}

fn windows_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn unix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn windows_flags(options: &InstallOptions) -> String {
    let mut flags = String::new();
    if !options.takeover {
        flags.push_str(" -NoTakeover");
    }
    if options.upgrade {
        flags.push_str(" -Upgrade");
    }
    if !options.upgrade && !options.friendly_name.trim().is_empty() {
        flags.push_str(" -FriendlyName ");
        flags.push_str(&windows_quote(options.friendly_name.trim()));
    }
    flags
}

fn unix_flags(platform: HostPlatform, options: &InstallOptions) -> String {
    let mut flags = String::new();
    if options.install_dependencies && platform == HostPlatform::Linux {
        flags.push_str(" --install-dependencies");
    }
    if !options.takeover {
        flags.push_str(" --no-takeover");
    }
    if options.upgrade {
        flags.push_str(" --upgrade");
    }
    if !options.upgrade && !options.friendly_name.trim().is_empty() {
        flags.push_str(" --friendly-name ");
        flags.push_str(&unix_quote(options.friendly_name.trim()));
    }
    flags
}

/// The command to paste on the host. It downloads this version's installer,
/// verifies its bytes against this build's embedded script before execution,
/// and pins the host bundle to this version.
pub fn install_command(platform: HostPlatform, options: InstallOptions) -> String {
    verified_install_command(platform, options, platform.script())
}

fn verified_install_command(
    platform: HostPlatform,
    options: InstallOptions,
    script: &str,
) -> String {
    let digest: String = Sha256::digest(script.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let url = script_url(platform);
    match platform {
        HostPlatform::Windows => {
            let flags = windows_flags(&options);
            format!(
                "& {{ $ErrorActionPreference = 'Stop'; \
                $r = Invoke-WebRequest -UseBasicParsing -TimeoutSec 60 -Uri '{url}'; \
                $b = $r.RawContentStream.ToArray(); \
                $s = [Security.Cryptography.SHA256]::Create(); \
                try {{ $h = [BitConverter]::ToString($s.ComputeHash($b)).Replace('-', '').ToLowerInvariant() }} \
                finally {{ $s.Dispose() }}; \
                if ($h -cne '{digest}') {{ throw 'Installer SHA-256 mismatch; nothing executed.' }}; \
                & ([scriptblock]::Create([Text.Encoding]::UTF8.GetString($b))) -Version {VERSION}{flags} }}"
            )
        }
        _ => {
            let flags = unix_flags(platform, &options);
            format!(
                "(set -eu; p=$(mktemp \"${{TMPDIR:-/tmp}}/festerm-setup.XXXXXXXX\") || exit 1; \
                trap 'rm -f -- \"$p\"' EXIT; \
                curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
                --connect-timeout 20 --max-time 60 --max-filesize 1048576 -o \"$p\" '{url}' || exit 1; \
                if command -v sha256sum >/dev/null 2>&1; then h=$(sha256sum \"$p\") || exit 1; \
                else h=$(shasum -a 256 \"$p\") || exit 1; fi; \
                [ \"${{h%% *}}\" = '{digest}' ] || {{ printf '%s\\n' 'Installer SHA-256 mismatch; nothing executed.' >&2; exit 1; }}; \
                bash \"$p\" {VERSION}{flags})"
            )
        }
    }
}

/// The command to run after saving the reviewed script locally.
pub fn local_command(platform: HostPlatform, options: InstallOptions) -> String {
    let name = platform.script_name();
    match platform {
        HostPlatform::Windows => {
            let flags = windows_flags(&options);
            format!("./{name} -Version {VERSION}{flags}")
        }
        _ => {
            let flags = unix_flags(platform, &options);
            format!("bash {name} {VERSION}{flags}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_scripts_are_the_release_installers() {
        assert!(BASH_INSTALLER.starts_with("#!/usr/bin/env bash"));
        assert!(POWERSHELL_INSTALLER.contains("[switch]$NoTakeover"));
        assert!(POWERSHELL_INSTALLER.contains("[switch]$Upgrade"));
        assert!(BASH_INSTALLER.contains("[--no-takeover] [--upgrade]"));
    }

    #[test]
    fn commands_pin_this_version_and_respect_options() {
        let on = InstallOptions::default();
        let off = InstallOptions {
            takeover: false,
            install_dependencies: true,
            upgrade: false,
            ..InstallOptions::default()
        };
        assert!(install_command(HostPlatform::MacOs, on.clone())
            .ends_with(&format!("bash \"$p\" {VERSION})")));
        assert!(install_command(HostPlatform::Linux, off.clone())
            .ends_with(&format!("{VERSION} --install-dependencies --no-takeover)")));
        assert!(
            !install_command(HostPlatform::MacOs, off.clone()).contains("--install-dependencies")
        );
        assert!(install_command(HostPlatform::Windows, off).ends_with("-NoTakeover }"));
        assert!(local_command(HostPlatform::Windows, on).starts_with("./install-dev-tunnel.ps1"));
    }

    #[test]
    fn upgrade_defaults_off_and_appends_the_platform_flag_last() {
        assert!(!InstallOptions::default().upgrade);
        let upgrade = InstallOptions {
            takeover: false,
            upgrade: true,
            ..InstallOptions::default()
        };
        for platform in [HostPlatform::MacOs, HostPlatform::Linux] {
            assert!(
                install_command(platform, upgrade.clone()).ends_with(" --no-takeover --upgrade)")
            );
            assert!(local_command(platform, upgrade.clone()).ends_with(" --no-takeover --upgrade"));
            assert!(!install_command(platform, InstallOptions::default()).contains("--upgrade"));
        }
        assert!(install_command(HostPlatform::Windows, upgrade.clone())
            .ends_with(" -NoTakeover -Upgrade }"));
        assert!(local_command(HostPlatform::Windows, upgrade).ends_with(" -NoTakeover -Upgrade"));
    }

    #[test]
    fn friendly_name_is_quoted_and_not_forwarded_during_upgrade() {
        let options = InstallOptions {
            friendly_name: "Pi's build box".into(),
            ..InstallOptions::default()
        };
        assert!(install_command(HostPlatform::Linux, options.clone())
            .ends_with("--friendly-name 'Pi'\\''s build box')"));
        assert!(install_command(HostPlatform::Windows, options.clone())
            .ends_with("-FriendlyName 'Pi''s build box' }"));
        let upgrade = InstallOptions {
            upgrade: true,
            ..options
        };
        assert!(!install_command(HostPlatform::Linux, upgrade.clone()).contains("friendly-name"));
        assert!(!install_command(HostPlatform::Windows, upgrade).contains("FriendlyName"));
    }

    #[test]
    fn download_commands_pin_the_embedded_script_digest() {
        for platform in HostPlatform::ALL {
            let expected: String = Sha256::digest(platform.script().as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let command = install_command(platform, InstallOptions::default());
            assert!(command.contains(&expected));
            assert!(command.contains(&script_url(platform)));
            assert!(!command.contains("| bash"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_download_verifies_before_execution_and_cleans_up() {
        use std::{os::unix::fs::PermissionsExt, process::Command};

        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let curl = directory.path().join("curl");
        std::fs::write(
            &curl,
            "#!/bin/sh\nwhile [ \"$1\" != '-o' ]; do shift; done\ncp \"$FIXTURE\" \"$2\"\nexit \"${DOWNLOAD_EXIT:-0}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = directory.path().join("fixture");
        let marker = directory.path().join("executed");
        let stage = directory.path().join("stage");
        std::fs::create_dir(&stage).unwrap();
        let script = "#!/bin/bash\nprintf '%s\\n' \"$@\" > \"$MARKER\"\n";
        let options = InstallOptions {
            takeover: false,
            friendly_name: "Pi's build box".into(),
            ..InstallOptions::default()
        };
        let command = verified_install_command(HostPlatform::Linux, options, script);
        for (contents, download_exit, succeeds) in [
            (script.to_owned(), "0", true),
            (format!("{script}# modified\n"), "0", false),
            (String::new(), "0", false),
            (script.to_owned(), "1", false),
        ] {
            std::fs::write(&fixture, contents).unwrap();
            let _ = std::fs::remove_file(&marker);
            let output = Command::new("bash")
                .args(["-c", &command])
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        directory.path().display(),
                        std::env::var("PATH").unwrap()
                    ),
                )
                .env("FIXTURE", &fixture)
                .env("MARKER", &marker)
                .env("TMPDIR", &stage)
                .env("DOWNLOAD_EXIT", download_exit)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(marker.exists(), succeeds);
            if succeeds {
                assert_eq!(
                    std::fs::read_to_string(&marker).unwrap(),
                    format!("{VERSION}\n--no-takeover\n--friendly-name\nPi's build box\n")
                );
            }
            assert_eq!(std::fs::read_dir(&stage).unwrap().count(), 0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn powershell_download_verifies_exact_bytes_before_execution() {
        use std::process::Command;

        let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let fixture = directory.path().join("fixture");
        let marker = directory.path().join("executed");
        let script = "param($Version, [switch]$NoTakeover, $FriendlyName)\n[IO.File]::WriteAllText($env:MARKER, \"$Version|$NoTakeover|$FriendlyName\")\n";
        let options = InstallOptions {
            takeover: false,
            friendly_name: "Pi's build box".into(),
            ..InstallOptions::default()
        };
        let command = verified_install_command(HostPlatform::Windows, options, script);
        let harness = format!(
            "function Invoke-WebRequest {{ \
            if ($env:DOWNLOAD_EXIT -eq '1') {{ throw 'download failed' }}; \
            [pscustomobject]@{{ RawContentStream = [IO.MemoryStream]::new([IO.File]::ReadAllBytes($env:FIXTURE)) }} \
            }}; {command}"
        );
        for (contents, download_exit, succeeds) in [
            (script.to_owned(), "0", true),
            (format!("{script}# modified\n"), "0", false),
            (script.replace('\n', "\r\n"), "0", false),
            (String::new(), "0", false),
            (script.to_owned(), "1", false),
        ] {
            std::fs::write(&fixture, contents).unwrap();
            let _ = std::fs::remove_file(&marker);
            let output = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &harness])
                .env("FIXTURE", &fixture)
                .env("MARKER", &marker)
                .env("DOWNLOAD_EXIT", download_exit)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(marker.exists(), succeeds);
            if succeeds {
                assert_eq!(
                    std::fs::read_to_string(&marker).unwrap(),
                    format!("{VERSION}|True|Pi's build box")
                );
            }
        }
    }
}
