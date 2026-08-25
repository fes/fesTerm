//! Corrects a macOS-only `PATH` gap before a [`LocalProfile`] is spawned.
//!
//! A macOS `.app` bundle launched from Finder, Launchpad, or the Dock is
//! started directly by `launchd`, not by a login shell: it never sources
//! `/etc/paths.d`, `~/.zprofile`, or similar, so its inherited `PATH` lacks
//! locations such as Homebrew's `/opt/homebrew/bin`. A user who can run
//! `tmux` from their own terminal can still find an explicit Local profile
//! (including the built-in tmux/screen persistence providers built by
//! `festerm_config::PersistenceConfig::to_local_profile`) fail with "no such
//! file" when fesTerm itself was launched from the Applications folder,
//! because `festerm_pty` resolves a bare executable name against exactly
//! this inherited `PATH` at spawn time.
//!
//! This module does not mutate the fesTerm process's own environment (that
//! would need an `unsafe` `std::env::set_var`, forbidden workspace-wide);
//! instead, on macOS only, it asks the user's real login shell for its
//! resolved `PATH` once per process and applies it as a single
//! [`EnvironmentPolicy`] override on the profile about to be spawned,
//! leaving every other inherited variable untouched. Terminal.app and
//! iTerm2 resolve the same gap by reading the login shell's environment at
//! startup; this reaches the same outcome without a process-wide mutation.

#[cfg(target_os = "macos")]
use festerm_pty::EnvironmentPolicy;
use festerm_pty::LocalProfile;

/// Returns `profile` unchanged, except that on macOS its `PATH` is
/// overridden with the user's login-shell `PATH`, provided the profile does
/// not already declare an explicit `PATH` of its own.
///
/// A profile using [`EnvironmentPolicy::Clear`] is never touched: clearing
/// the environment is itself an explicit, deliberate choice to control the
/// full environment, and this correction only ever fills the ordinary
/// inherited-`PATH` gap.
pub fn with_corrected_local_path(profile: LocalProfile) -> LocalProfile {
    #[cfg(target_os = "macos")]
    {
        if profile_overrides_path(profile.environment()) {
            return profile;
        }
        if let Some(path) = cached_login_shell_path() {
            let mut overrides = match profile.environment() {
                EnvironmentPolicy::InheritWith(existing) => existing.clone(),
                _ => std::collections::BTreeMap::new(),
            };
            overrides.insert(std::ffi::OsString::from("PATH"), path.clone());
            return profile.with_environment(EnvironmentPolicy::InheritWith(overrides));
        }
    }
    profile
}

#[cfg(target_os = "macos")]
fn profile_overrides_path(policy: &EnvironmentPolicy) -> bool {
    match policy {
        EnvironmentPolicy::Inherit => false,
        EnvironmentPolicy::Clear(_) => true,
        EnvironmentPolicy::InheritWith(overrides) => {
            overrides.contains_key(std::ffi::OsStr::new("PATH"))
        }
    }
}

/// Resolves and memoizes the login shell's `PATH` for the lifetime of the
/// process: every tab that starts a local session reaches this, and the
/// login shell only needs to be asked once.
#[cfg(target_os = "macos")]
fn cached_login_shell_path() -> Option<std::ffi::OsString> {
    static CACHED: std::sync::OnceLock<Option<std::ffi::OsString>> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| login_shell_path(std::env::var_os("SHELL")))
        .clone()
}

#[cfg(target_os = "macos")]
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Delimiter surrounding the `PATH` line in the login shell's output, so a
/// noisy `motd`/rc-file banner printed before it cannot be mistaken for
/// part of the value.
#[cfg(target_os = "macos")]
const DELIMITER: &str = "__festerm_login_shell_path__";

#[cfg(target_os = "macos")]
fn login_shell_path(shell: Option<std::ffi::OsString>) -> Option<std::ffi::OsString> {
    let shell = shell.filter(|shell| !shell.is_empty())?;
    let output = run_with_timeout(&shell, LOGIN_SHELL_TIMEOUT)?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let path = stdout
        .split(DELIMITER)
        .nth(1)
        .map(str::trim)
        .filter(|path| !path.is_empty())?;
    Some(std::ffi::OsString::from(path))
}

/// Runs the user's login shell as a login (non-interactive) shell to print
/// its resolved `PATH`, giving up after `timeout` rather than blocking
/// fesTerm startup indefinitely if the user's shell startup files hang
/// (e.g. on a stalled network mount or prompt).
#[cfg(target_os = "macos")]
fn run_with_timeout(
    shell: &std::ffi::OsStr,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    let child = std::process::Command::new(shell)
        .arg("-l")
        .arg("-c")
        .arg(format!(
            "echo {DELIMITER}; command printenv PATH; echo {DELIMITER}"
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(child.wait_with_output());
    });

    // Either branch leaves the sending thread to finish (or stay blocked
    // forever on a truly hung shell) on its own; there is nothing further
    // to reconcile it with once this function has an answer or has given up.
    receiver.recv_timeout(timeout).ok()?.ok()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn inherit_policy_gains_an_explicit_path_override_when_a_shell_path_is_known() {
        let profile = LocalProfile::new("tmux");
        let corrected = apply_login_shell_path(profile, Some("/opt/homebrew/bin:/usr/bin".into()));
        assert_eq!(
            corrected.environment(),
            &EnvironmentPolicy::InheritWith(BTreeMap::from([(
                "PATH".into(),
                "/opt/homebrew/bin:/usr/bin".into()
            )]))
        );
    }

    #[test]
    fn inherit_policy_is_unchanged_when_no_shell_path_is_known() {
        let profile = LocalProfile::new("tmux");
        let corrected = apply_login_shell_path(profile.clone(), None);
        assert_eq!(corrected, profile);
    }

    #[test]
    fn an_explicit_path_override_already_present_is_never_replaced() {
        let profile = LocalProfile::new("tmux").with_environment(EnvironmentPolicy::InheritWith(
            BTreeMap::from([("PATH".into(), "/custom/bin".into())]),
        ));
        let corrected = apply_login_shell_path(profile.clone(), Some("/opt/homebrew/bin".into()));
        assert_eq!(corrected, profile);
    }

    #[test]
    fn a_cleared_environment_is_never_touched() {
        let profile =
            LocalProfile::new("tmux").with_environment(EnvironmentPolicy::Clear(BTreeMap::new()));
        let corrected = apply_login_shell_path(profile.clone(), Some("/opt/homebrew/bin".into()));
        assert_eq!(corrected, profile);
    }

    #[test]
    fn other_inherited_with_overrides_are_preserved_alongside_the_path_override() {
        let profile = LocalProfile::new("tmux").with_environment(EnvironmentPolicy::InheritWith(
            BTreeMap::from([("EDITOR".into(), "vim".into())]),
        ));
        let corrected = apply_login_shell_path(profile, Some("/opt/homebrew/bin".into()));
        assert_eq!(
            corrected.environment(),
            &EnvironmentPolicy::InheritWith(BTreeMap::from([
                ("EDITOR".into(), "vim".into()),
                ("PATH".into(), "/opt/homebrew/bin".into()),
            ]))
        );
    }

    /// Test-only variant of [`with_corrected_local_path`] that takes an
    /// explicit resolved shell `PATH` instead of resolving one from a real
    /// login shell, so the override logic is exercised deterministically.
    fn apply_login_shell_path(
        profile: LocalProfile,
        path: Option<std::ffi::OsString>,
    ) -> LocalProfile {
        if profile_overrides_path(profile.environment()) {
            return profile;
        }
        let Some(path) = path else {
            return profile;
        };
        let mut overrides = match profile.environment() {
            EnvironmentPolicy::InheritWith(existing) => existing.clone(),
            _ => BTreeMap::new(),
        };
        overrides.insert("PATH".into(), path);
        profile.with_environment(EnvironmentPolicy::InheritWith(overrides))
    }

    #[test]
    fn run_with_timeout_reports_the_login_shells_path() {
        let output = run_with_timeout(std::ffi::OsStr::new("/bin/sh"), LOGIN_SHELL_TIMEOUT)
            .expect("a real shell must produce output before the timeout");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(DELIMITER));
    }

    #[test]
    fn run_with_timeout_gives_up_on_a_shell_that_never_exits() {
        // A minimal script that ignores its `-l -c '...'` arguments and
        // sleeps well past the short timeout below, so this deterministically
        // exercises the give-up path rather than depending on a real shell's
        // startup-file timing.
        let script = write_executable_script("#!/bin/sh\nsleep 5\n");
        let output = run_with_timeout(script.as_os_str(), std::time::Duration::from_millis(50));
        let _ = std::fs::remove_file(&script);
        assert!(output.is_none());
    }

    /// Writes an executable shell script to a uniquely named path under the
    /// system temporary directory and returns that path.
    fn write_executable_script(contents: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "festerm-login-shell-path-test-{}-{unique}.sh",
            std::process::id()
        ));
        std::fs::write(&path, contents).expect("temp script must be writable");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("temp script permissions must be settable");
        path
    }
}
