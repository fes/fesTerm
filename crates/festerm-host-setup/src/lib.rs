//! The Dev Tunnel host installers exactly as this build shipped them.
//!
//! The scripts are embedded, not fetched, so what a user reviews in the app is
//! byte-for-byte what the matching GitHub release publishes for this version.

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

/// The command to paste on the host. It downloads this version's installer
/// and pins the host bundle to this version.
pub fn install_command(platform: HostPlatform, options: InstallOptions) -> String {
    let url = script_url(platform);
    match platform {
        HostPlatform::Windows => {
            let flags = windows_flags(&options);
            format!(
                "& ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing {url}).Content)) -Version {VERSION}{flags}"
            )
        }
        _ => {
            let flags = unix_flags(platform, &options);
            format!("curl -fsSL {url} | bash -s -- {VERSION}{flags}")
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
            .ends_with(&format!("bash -s -- {VERSION}")));
        assert!(install_command(HostPlatform::Linux, off.clone())
            .ends_with(&format!("{VERSION} --install-dependencies --no-takeover")));
        assert!(
            !install_command(HostPlatform::MacOs, off.clone()).contains("--install-dependencies")
        );
        assert!(install_command(HostPlatform::Windows, off).ends_with("-NoTakeover"));
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
                install_command(platform, upgrade.clone()).ends_with(" --no-takeover --upgrade")
            );
            assert!(local_command(platform, upgrade.clone()).ends_with(" --no-takeover --upgrade"));
            assert!(!install_command(platform, InstallOptions::default()).contains("--upgrade"));
        }
        assert!(install_command(HostPlatform::Windows, upgrade.clone())
            .ends_with(" -NoTakeover -Upgrade"));
        assert!(local_command(HostPlatform::Windows, upgrade).ends_with(" -NoTakeover -Upgrade"));
    }

    #[test]
    fn friendly_name_is_quoted_and_not_forwarded_during_upgrade() {
        let options = InstallOptions {
            friendly_name: "Pi's build box".into(),
            ..InstallOptions::default()
        };
        assert!(install_command(HostPlatform::Linux, options.clone())
            .ends_with("--friendly-name 'Pi'\\''s build box'"));
        assert!(install_command(HostPlatform::Windows, options.clone())
            .ends_with("-FriendlyName 'Pi''s build box'"));
        let upgrade = InstallOptions {
            upgrade: true,
            ..options
        };
        assert!(!install_command(HostPlatform::Linux, upgrade.clone()).contains("friendly-name"));
        assert!(!install_command(HostPlatform::Windows, upgrade).contains("FriendlyName"));
    }
}
