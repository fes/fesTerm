//! Host-owned default resolution preserves every explicit public recipe field.
use super::*;

fn recipe(shell: Option<&str>, args: &[&str], cwd: Option<&str>) -> Recipe {
    Recipe {
        shell: shell.map(str::to_owned),
        args: args.iter().map(|s| (*s).to_owned()).collect(),
        cwd: cwd.map(str::to_owned),
    }
}

#[test]
fn explicit_program_empty_arguments_default_to_account_home_not_service_cwd() {
    let home = std::env::temp_dir().join("host-account-home");
    let p = profile_with_defaults(
        &recipe(Some("custom-program"), &[], None),
        &home,
        None,
        None,
    )
    .unwrap();
    assert_eq!(p.executable(), Path::new("custom-program"));
    assert!(p.arguments().is_empty());
    assert_eq!(p.working_directory(), Some(home.as_path()));
    assert_ne!(
        p.working_directory(),
        std::env::current_dir().ok().as_deref()
    );
}

#[test]
fn explicit_directory_and_no_profile_arguments_are_never_rewritten() {
    let home = std::env::temp_dir().join("host-account-home");
    let explicit = std::env::temp_dir().join("explicit-project");
    let p = profile_with_defaults(
        &recipe(
            Some("pwsh"),
            &["-NoProfile", "-NoLogo"],
            Some(explicit.to_str().unwrap()),
        ),
        &home,
        None,
        None,
    )
    .unwrap();
    assert_eq!(p.executable(), Path::new("pwsh"));
    assert_eq!(
        p.arguments(),
        &[std::ffi::OsString::from("-NoProfile"), "-NoLogo".into()]
    );
    assert_eq!(p.working_directory(), Some(explicit.as_path()));
}

#[test]
#[cfg(unix)]
fn generic_shell_uses_account_login_defaults_and_host_home() {
    let home = std::env::temp_dir().join("host-account-home");
    let p = profile_with_defaults(
        &recipe(None, &[], None),
        &home,
        Some(Path::new("/bin/sh")),
        None,
    )
    .unwrap();
    assert_eq!(p.executable(), Path::new("/bin/sh"));
    assert_eq!(p.arguments(), &[std::ffi::OsString::from("-l")]);
    assert_eq!(p.working_directory(), Some(home.as_path()));
}

#[test]
#[cfg(unix)]
fn explicit_arguments_also_survive_a_default_program() {
    let p = profile_with_defaults(
        &recipe(None, &["-c", "printf exact"], None),
        &std::env::temp_dir(),
        Some(Path::new("/bin/sh")),
        None,
    )
    .unwrap();
    assert_eq!(
        p.arguments(),
        &[std::ffi::OsString::from("-c"), "printf exact".into()]
    );
}

#[test]
#[cfg(unix)]
fn corrected_path_is_used_before_canonical_spawn_and_fingerprint() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("festerm-host-shell-path-{}", std::process::id()));
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let program = home.join("copilot-like");
    std::fs::write(&program, "#!/bin/sh\nprintf fixture-only\n").unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let local = profile_with_defaults(&recipe(Some("copilot-like"), &[], None), &home, None, None)
        .unwrap()
        .with_environment(festerm_pty::EnvironmentPolicy::InheritWith(BTreeMap::from(
            [("PATH".into(), home.as_os_str().to_owned())],
        )));
    let normalized = detached::normalize_spawn_profile(&local).unwrap();
    assert_eq!(
        normalized.executable(),
        std::fs::canonicalize(&program).unwrap()
    );
    assert_eq!(
        normalized.working_directory(),
        Some(std::fs::canonicalize(&home).unwrap().as_path())
    );
    assert_eq!(
        detached::spawn_fingerprint(&normalized).unwrap(),
        detached::spawn_fingerprint(&local).unwrap()
    );
    assert!(normalized.arguments().is_empty());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn native_home_and_path_defaults_preserve_explicit_environment_choices() {
    use festerm_pty::EnvironmentPolicy;
    let home = std::env::temp_dir().join("native-account-home");
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let p = LocalProfile::new("custom");
    let corrected = with_host_environment_defaults(
        p.clone(),
        &home,
        Some(std::ffi::OsStr::new("normal-user-path")),
    );
    let EnvironmentPolicy::InheritWith(values) = corrected.environment() else {
        panic!("missing default")
    };
    assert_eq!(
        values.get(std::ffi::OsStr::new(key)),
        Some(&home.as_os_str().to_owned())
    );
    assert_eq!(
        values.get(std::ffi::OsStr::new("PATH")),
        Some(&std::ffi::OsString::from("normal-user-path"))
    );
    let explicit = p
        .clone()
        .with_environment(EnvironmentPolicy::InheritWith(BTreeMap::from([
            (key.into(), "explicit-home".into()),
            ("PATH".into(), "explicit-path".into()),
        ])));
    assert_eq!(
        with_host_environment_defaults(
            explicit.clone(),
            &home,
            Some(std::ffi::OsStr::new("normal-path"))
        ),
        explicit
    );
    let clear = p.with_environment(EnvironmentPolicy::Clear(BTreeMap::new()));
    assert_eq!(
        with_host_environment_defaults(clear.clone(), &home, None),
        clear
    );
}

#[test]
#[cfg(unix)]
fn fresh_generic_login_sources_controlled_private_home_once() {
    use festerm_session::{Session, SessionEvent, SessionTryReceiveError};
    use std::os::unix::fs::PermissionsExt;

    // The resolver cache is process-wide. A single-test child makes the OLD
    // preparation probe observable regardless of other tests' execution order.
    const INNER: &str = "FESTERM_TEST_FRESH_LOGIN_INNER";
    if std::env::var_os(INNER).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "host_control::shell_defaults_tests::fresh_generic_login_sources_controlled_private_home_once", "--nocapture"])
            .env(INNER, "1")
            .output().unwrap();
        assert!(
            output.status.success(),
            "isolated controlled login fixture: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let private = storage::private_test_directory();
    let home = private.path().join("home");
    let _directory = storage::private_directory(&home, true).unwrap();
    let bin = home.join("private-bin");
    std::fs::create_dir(&bin).unwrap();
    let shell = home.join("controlled-login-shell");
    std::fs::write(&shell, "#!/bin/sh\n[ \"$1\" = -l ] || exit 81\nshift\n. \"$HOME/.profile\"\nunset ENV BASH_ENV\nif [ \"$#\" -eq 0 ]; then exec /bin/sh -i; fi\nexec /bin/sh \"$@\"\n").unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(home.join(".profile"), "printf 'startup\\n' >> \"$HOME/start-count\"\nPATH=\"$PATH:$HOME/private-bin\"\nexport PATH\n").unwrap();
    let tool = bin.join("fixture-tool");
    std::fs::write(&tool, "#!/bin/sh\nprintf 'private-tool-ok'\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();

    let generic = recipe(None, &[], None);
    let p = profile_for_account(&generic, &home, Some(&shell), None).unwrap();
    assert_eq!(p.arguments(), &[std::ffi::OsString::from("-l")]);
    let fingerprint = detached::spawn_fingerprint(&p).unwrap();
    let session = festerm_pty::LocalPtySession::start(p.clone(), terminal_size()).unwrap();
    // Split markers prevent the shell's input echo from satisfying the reply.
    session.try_send_input(b"printf 'FES''TERM_LOGIN_BEGIN\\n%s\\n%s\\n%s\\nFES''TERM_LOGIN_END\\n' \"$HOME\" \"$PWD\" \"$PATH\"\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let text = loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Output(bytes)) => output.extend(bytes),
            Ok(_) | Err(SessionTryReceiveError::Empty) => {}
            Err(error) => panic!("controlled login closed: {error:?}"),
        }
        assert!(output.len() <= 64 * 1024);
        let text = String::from_utf8_lossy(&output);
        if text.contains("FESTERM_LOGIN_BEGIN\r\n") && text.contains("FESTERM_LOGIN_END") {
            break text.into_owned();
        }
        assert!(
            Instant::now() < deadline,
            "controlled login response deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let block = text
        .split_once("FESTERM_LOGIN_BEGIN\r\n")
        .unwrap()
        .1
        .split_once("FESTERM_LOGIN_END")
        .unwrap()
        .0;
    let fields: Vec<_> = block.lines().collect();
    assert_eq!(fields.len(), 3);
    assert_eq!(Path::new(fields[0]), home);
    assert_eq!(
        std::fs::canonicalize(fields[1]).unwrap(),
        std::fs::canonicalize(&home).unwrap()
    );
    let suffixes = std::env::split_paths(std::ffi::OsStr::new(fields[2]))
        .filter(|p| p == &bin)
        .count();
    let startups = std::fs::read_to_string(home.join("start-count"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        (startups, suffixes),
        (1, 1),
        "startup side effects and PATH suffix count"
    );
    assert_eq!(detached::spawn_fingerprint(&p).unwrap(), fingerprint);
    session.try_send_input(b"exit\n").unwrap();
    let exit_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match session.try_recv_event() {
            Ok(SessionEvent::Lifecycle(festerm_session::SessionLifecycle::Exited(_))) => break,
            Ok(_) | Err(SessionTryReceiveError::Empty) => {}
            Err(error) => panic!("controlled login exit: {error:?}"),
        }
        assert!(
            Instant::now() < exit_deadline,
            "controlled login ordinary exit deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    session.shutdown(Duration::from_secs(2)).unwrap();
    drop(session);

    // Explicit commands and stored effective recipes keep their preparation
    // path. Their argv/cwd/fingerprint are not rewritten by the default fix.
    let explicit = recipe(Some("fixture-tool"), &[], Some(home.to_str().unwrap()));
    let tool_profile = profile_for_account(&explicit, &home, Some(&shell), None).unwrap();
    assert_eq!(tool_profile.executable(), tool);
    assert!(tool_profile.arguments().is_empty());
    assert_eq!(
        std::process::Command::new(tool_profile.executable())
            .output()
            .unwrap()
            .stdout,
        b"private-tool-ok"
    );
    assert_eq!(
        std::fs::read_to_string(home.join("start-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    let frozen = recipe(
        Some(p.executable().to_str().unwrap()),
        &["-l"],
        p.working_directory().unwrap().to_str(),
    );
    let restored = profile_for_account(&frozen, &home, Some(&shell), None).unwrap();
    assert_eq!(restored.executable(), p.executable());
    assert_eq!(restored.arguments(), p.arguments());
    assert_eq!(restored.working_directory(), p.working_directory());
    assert_eq!(detached::spawn_fingerprint(&restored).unwrap(), fingerprint);
}
