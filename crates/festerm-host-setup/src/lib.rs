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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstallOptions {
    /// Allow a remote client to displace the current holder (the default).
    pub takeover: bool,
    /// Linux only: install missing runtime packages with sudo.
    pub install_dependencies: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            takeover: true,
            install_dependencies: false,
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

/// The command to paste on the host. It downloads this version's installer
/// and pins the host bundle to this version.
pub fn install_command(platform: HostPlatform, options: InstallOptions) -> String {
    let url = script_url(platform);
    match platform {
        HostPlatform::Windows => {
            let flag = if options.takeover { "" } else { " -NoTakeover" };
            format!(
                "& ([scriptblock]::Create((Invoke-WebRequest -UseBasicParsing {url}).Content)) -Version {VERSION}{flag}"
            )
        }
        _ => {
            let mut flags = String::new();
            if options.install_dependencies && platform == HostPlatform::Linux {
                flags.push_str(" --install-dependencies");
            }
            if !options.takeover {
                flags.push_str(" --no-takeover");
            }
            format!("curl -fsSL {url} | bash -s -- {VERSION}{flags}")
        }
    }
}

/// The command to run after saving the reviewed script locally.
pub fn local_command(platform: HostPlatform, options: InstallOptions) -> String {
    let name = platform.script_name();
    match platform {
        HostPlatform::Windows => {
            let flag = if options.takeover { "" } else { " -NoTakeover" };
            format!("./{name} -Version {VERSION}{flag}")
        }
        _ => {
            let mut flags = String::new();
            if options.install_dependencies && platform == HostPlatform::Linux {
                flags.push_str(" --install-dependencies");
            }
            if !options.takeover {
                flags.push_str(" --no-takeover");
            }
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
    }

    #[test]
    fn commands_pin_this_version_and_respect_options() {
        let on = InstallOptions::default();
        let off = InstallOptions {
            takeover: false,
            install_dependencies: true,
        };
        assert!(
            install_command(HostPlatform::MacOs, on).ends_with(&format!("bash -s -- {VERSION}"))
        );
        assert!(install_command(HostPlatform::Linux, off)
            .ends_with(&format!("{VERSION} --install-dependencies --no-takeover")));
        assert!(!install_command(HostPlatform::MacOs, off).contains("--install-dependencies"));
        assert!(install_command(HostPlatform::Windows, off).ends_with("-NoTakeover"));
        assert!(local_command(HostPlatform::Windows, on).starts_with("./install-dev-tunnel.ps1"));
    }
}
