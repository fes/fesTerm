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
