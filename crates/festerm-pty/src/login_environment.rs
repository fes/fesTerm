//! Bounded, per-child login PATH/locale correction shared by GUI and hosted shells.
//! No process environment mutation or other login-shell output is exposed.

#[cfg(unix)]
use crate::EnvironmentPolicy;
use crate::LocalProfile;
use std::collections::BTreeMap;

/// Environment variables resolved from the user's login shell and applied
/// to a profile's environment when it does not already set them and the
/// fesTerm process itself did not inherit them.
#[cfg(unix)]
const CORRECTED_VARIABLES: [&str; 4] = ["PATH", "LANG", "LC_ALL", "LC_CTYPE"];

/// Returns `profile` unchanged, except that on Unix any of `PATH`,
/// `LANG`, `LC_ALL`, and `LC_CTYPE` missing from the profile's own
/// environment overrides are filled in from the user's login shell.
///
/// This deliberately does not check fesTerm's own inherited (launchd)
/// process environment: `launchd` always sets some minimal `PATH` (for
/// example `/usr/bin:/bin:/usr/sbin:/sbin`), so treating "fesTerm's own
/// process already has a value" as a reason to skip correction would
/// silently defeat the very gap this module exists to close.
///
/// A profile using [`EnvironmentPolicy::Clear`] is never touched: clearing
/// the environment is itself an explicit, deliberate choice to control the
/// full environment, and this correction only ever fills ordinary
/// inherited-environment gaps.
#[cfg(unix)]
pub fn with_corrected_local_path(
    profile: LocalProfile,
    shell: &std::ffi::OsStr,
    home: &std::path::Path,
) -> LocalProfile {
    if matches!(profile.environment(), EnvironmentPolicy::Clear(_)) {
        return profile;
    }
    let Some(login_env) = cached_login_shell_environment(shell, home) else {
        return profile;
    };
    apply_login_shell_environment(profile, login_env.clone())
}

fn apply_login_shell_environment(
    profile: LocalProfile,
    login_env: BTreeMap<String, std::ffi::OsString>,
) -> LocalProfile {
    if matches!(profile.environment(), EnvironmentPolicy::Clear(_)) {
        return profile;
    }
    let mut overrides = match profile.environment() {
        EnvironmentPolicy::InheritWith(existing) => existing.clone(),
        _ => BTreeMap::new(),
    };
    let mut changed = false;
    for name in CORRECTED_VARIABLES {
        let key = std::ffi::OsStr::new(name);
        if overrides.contains_key(key) {
            continue;
        }
        if let Some(value) = login_env.get(name) {
            overrides.insert(std::ffi::OsString::from(name), value.clone());
            changed = true;
        }
    }
    if changed {
        profile.with_environment(EnvironmentPolicy::InheritWith(overrides))
    } else {
        profile
    }
}

/// Resolves and memoizes the login shell's `PATH`/locale variables for the
/// lifetime of the process: every tab that starts a local session reaches
/// this, and the login shell only needs to be asked once.
#[cfg(unix)]
fn cached_login_shell_environment(
    shell: &std::ffi::OsStr,
    home: &std::path::Path,
) -> &'static Option<std::collections::BTreeMap<String, std::ffi::OsString>> {
    static CACHED: std::sync::OnceLock<
        Option<std::collections::BTreeMap<String, std::ffi::OsString>>,
    > = std::sync::OnceLock::new();
    CACHED.get_or_init(|| login_shell_environment(Some(shell.to_owned()), Some(home)))
}

#[cfg(unix)]
const LOGIN_SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Delimiter surrounding the resolved-variable block in the login shell's
/// output, so a noisy `motd`/rc-file banner printed before it cannot be
/// mistaken for part of the values.
#[cfg(unix)]
const DELIMITER: &str = "__festerm_login_shell_path__";

#[cfg(unix)]
fn login_shell_environment(
    shell: Option<std::ffi::OsString>,
    home: Option<&std::path::Path>,
) -> Option<std::collections::BTreeMap<String, std::ffi::OsString>> {
    let shell = shell.filter(|shell| !shell.is_empty())?;
    let output = run_with_timeout(&shell, LOGIN_SHELL_TIMEOUT, home)?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let block = stdout.split(DELIMITER).nth(1)?;
    let mut resolved = std::collections::BTreeMap::new();
    for (name, line) in CORRECTED_VARIABLES.iter().zip(block.lines().skip(1)) {
        let value = line.trim();
        if !value.is_empty() {
            resolved.insert((*name).to_owned(), std::ffi::OsString::from(value));
        }
    }
    (!resolved.is_empty()).then_some(resolved)
}

/// Runs the user's login shell as an interactive login shell to print its
/// resolved `PATH`, giving up after `timeout` rather than blocking
/// fesTerm startup indefinitely if the user's shell startup files hang
/// (e.g. on a stalled network mount or prompt).
#[cfg(unix)]
fn run_with_timeout(
    shell: &std::ffi::OsStr,
    timeout: std::time::Duration,
    home: Option<&std::path::Path>,
) -> Option<std::process::Output> {
    let mut command = std::process::Command::new(shell);
    command.args(["-l", "-i", "-c"]).arg(format!(
        "echo {DELIMITER}; echo \"$PATH\"; echo \"$LANG\"; echo \"$LC_ALL\"; \
         echo \"$LC_CTYPE\"; echo {DELIMITER}"
    ));
    if let Some(home) = home {
        command.env("HOME", home).current_dir(home);
    }
    bounded_output(command, timeout)
}

// Drain both nonblocking pipes under one deadline. No reader threads can outlive
// a hung startup file or a descendant that inherits its output handles.
fn bounded_output(
    mut command: std::process::Command,
    timeout: std::time::Duration,
) -> Option<std::process::Output> {
    use std::{
        io::{self, Read},
        os::unix::process::CommandExt,
        process::Stdio,
        time::Instant,
    };
    struct Owned(std::process::Child, bool);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.1 {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(self.0.id() as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    fn drain(reader: &mut impl Read, bytes: &mut Vec<u8>) -> io::Result<bool> {
        let mut buffer = [0; 8192];
        for _ in 0..8 {
            match reader.read(&mut buffer) {
                Ok(0) => return Ok(true),
                Ok(n) => {
                    if bytes.len() + n > 1024 * 1024 {
                        return Err(io::ErrorKind::FileTooLarge.into());
                    }
                    bytes.extend_from_slice(&buffer[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }
    let deadline = Instant::now() + timeout;
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Owned(command.spawn().ok()?, false);
    let mut stdout = filedescriptor::FileDescriptor::dup(&child.0.stdout.take()?).ok()?;
    let mut stderr = filedescriptor::FileDescriptor::dup(&child.0.stderr.take()?).ok()?;
    stdout.set_non_blocking(true).ok()?;
    stderr.set_non_blocking(true).ok()?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let out_done = drain(&mut stdout, &mut out).ok()?;
        let err_done = drain(&mut stderr, &mut err).ok()?;
        if let Some(status) = child.0.try_wait().ok()? {
            if out_done && err_done {
                child.1 = true;
                return Some(std::process::Output {
                    status,
                    stdout: out,
                    stderr: err,
                });
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn inherit_policy_gains_explicit_overrides_when_login_shell_variables_are_known() {
        let profile = LocalProfile::new("tmux");
        let corrected = apply_login_shell_environment(
            profile,
            BTreeMap::from([
                ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".into()),
                ("LANG".to_owned(), "en_US.UTF-8".into()),
            ]),
        );
        assert_eq!(
            corrected.environment(),
            &EnvironmentPolicy::InheritWith(BTreeMap::from([
                ("LANG".into(), "en_US.UTF-8".into()),
                ("PATH".into(), "/opt/homebrew/bin:/usr/bin".into()),
            ]))
        );
    }

    #[test]
    fn inherit_policy_is_unchanged_when_no_login_shell_variables_are_known() {
        let profile = LocalProfile::new("tmux");
        let corrected = apply_login_shell_environment(profile.clone(), BTreeMap::new());
        assert_eq!(corrected, profile);
    }

    #[test]
    fn an_explicit_override_already_present_is_never_replaced() {
        let profile = LocalProfile::new("tmux").with_environment(EnvironmentPolicy::InheritWith(
            BTreeMap::from([("PATH".into(), "/custom/bin".into())]),
        ));
        let corrected = apply_login_shell_environment(
            profile.clone(),
            BTreeMap::from([("PATH".to_owned(), "/opt/homebrew/bin".into())]),
        );
        assert_eq!(corrected, profile);
    }

    #[test]
    fn a_cleared_environment_is_never_touched() {
        let profile =
            LocalProfile::new("tmux").with_environment(EnvironmentPolicy::Clear(BTreeMap::new()));
        let corrected = apply_login_shell_environment(
            profile.clone(),
            BTreeMap::from([("PATH".to_owned(), "/opt/homebrew/bin".into())]),
        );
        assert_eq!(corrected, profile);
    }

    #[test]
    fn other_inherited_with_overrides_are_preserved_alongside_the_new_overrides() {
        let profile = LocalProfile::new("tmux").with_environment(EnvironmentPolicy::InheritWith(
            BTreeMap::from([("EDITOR".into(), "vim".into())]),
        ));
        let corrected = apply_login_shell_environment(
            profile,
            BTreeMap::from([("PATH".to_owned(), "/opt/homebrew/bin".into())]),
        );
        assert_eq!(
            corrected.environment(),
            &EnvironmentPolicy::InheritWith(BTreeMap::from([
                ("EDITOR".into(), "vim".into()),
                ("PATH".into(), "/opt/homebrew/bin".into()),
            ]))
        );
    }

    #[test]
    fn locale_variables_are_filled_in_alongside_path() {
        let profile = LocalProfile::new("tmux");
        let corrected = apply_login_shell_environment(
            profile,
            BTreeMap::from([
                ("PATH".to_owned(), "/usr/bin".into()),
                ("LANG".to_owned(), "en_US.UTF-8".into()),
                ("LC_ALL".to_owned(), "en_US.UTF-8".into()),
                ("LC_CTYPE".to_owned(), "en_US.UTF-8".into()),
            ]),
        );
        assert_eq!(
            corrected.environment(),
            &EnvironmentPolicy::InheritWith(BTreeMap::from([
                ("LANG".into(), "en_US.UTF-8".into()),
                ("LC_ALL".into(), "en_US.UTF-8".into()),
                ("LC_CTYPE".into(), "en_US.UTF-8".into()),
                ("PATH".into(), "/usr/bin".into()),
            ]))
        );
    }

    #[test]
    fn run_with_timeout_reports_the_login_shells_variables() {
        let output = run_with_timeout(std::ffi::OsStr::new("/bin/sh"), LOGIN_SHELL_TIMEOUT, None)
            .expect("a real shell must produce output before the timeout");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains(DELIMITER));
    }

    #[test]
    fn login_shell_environment_loads_interactive_startup_files() {
        let script = write_executable_script(&format!(
            "#!/bin/sh\n\
             test \"$1\" = -l && test \"$2\" = -i && test \"$3\" = -c || exit 1\n\
             printf '{DELIMITER}\\n/test/.local/bin:/usr/bin\\nen_US.UTF-8\\n\\n\\n{DELIMITER}\\n'\n"
        ));
        let resolved = login_shell_environment(Some(script.clone().into_os_string()), None)
            .expect("the interactive login environment must be resolved");
        let _ = std::fs::remove_file(&script);
        assert_eq!(
            resolved.get("PATH"),
            Some(&std::ffi::OsString::from("/test/.local/bin:/usr/bin"))
        );
    }

    #[test]
    fn run_with_timeout_gives_up_on_a_shell_that_never_exits() {
        // A minimal script that ignores its `-l -i -c '...'` arguments and
        // sleeps well past the short timeout below, so this deterministically
        // exercises the give-up path rather than depending on a real shell's
        // startup-file timing.
        let script = write_executable_script("#!/bin/sh\nsleep 5\n");
        let output = run_with_timeout(
            script.as_os_str(),
            std::time::Duration::from_millis(50),
            None,
        );
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
    #[test]
    fn explicit_private_home_login_path_finds_a_bare_program() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!(
            "festerm-login-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(home.join("bin")).unwrap();
        let program = home.join("bin/copilot-like");
        std::fs::write(&program, "#!/bin/sh\nprintf private-startup-program\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let shell = write_executable_script(&format!("#!/bin/sh\ntest \"$1\" = -l && test \"$2\" = -i && test \"$3\" = -c || exit 1\nprintf '{DELIMITER}\\n%s/bin:/usr/bin:/bin\\nen_US.UTF-8\\n\\n\\n{DELIMITER}\\n' \"$HOME\"\n"));
        let env =
            login_shell_environment(Some(shell.clone().into_os_string()), Some(&home)).unwrap();
        let corrected = apply_login_shell_environment(LocalProfile::new("copilot-like"), env);
        let EnvironmentPolicy::InheritWith(overrides) = corrected.environment() else {
            panic!("missing correction")
        };
        let output = std::process::Command::new(corrected.executable())
            .envs(overrides)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"private-startup-program");
        assert!(overrides
            .keys()
            .all(|k| CORRECTED_VARIABLES.iter().any(|v| k == v)));
        std::fs::remove_file(shell).unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn oversized_startup_output_is_bounded_and_refused() {
        let shell = write_executable_script("#!/bin/sh\nhead -c 1048577 /dev/zero\n");
        let started = std::time::Instant::now();
        assert!(
            run_with_timeout(shell.as_os_str(), std::time::Duration::from_secs(3), None).is_none()
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        std::fs::remove_file(shell).unwrap();
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn native_zsh_reads_only_the_private_fixture_home_startup() {
        use std::os::unix::fs::PermissionsExt;
        let home =
            std::env::temp_dir().join(format!("festerm-native-zsh-home-{}", std::process::id()));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            home.join(".zshrc"),
            "export PATH=\"$HOME/private-bin:/usr/bin:/bin\"\n",
        )
        .unwrap();
        let env = login_shell_environment(Some("/bin/zsh".into()), Some(&home)).unwrap();
        assert!(env["PATH"]
            .to_string_lossy()
            .starts_with(home.join("private-bin").to_str().unwrap()));
        std::fs::remove_dir_all(home).unwrap();
    }
}
