//! Non-destructive checks and an explicitly selected experimental probe.

#[path = "ownership_probe/role.rs"]
pub mod role;
#[path = "ownership_probe/runner_gate.rs"]
pub mod runner_gate;

#[cfg(all(windows, feature = "experimental-windows-job-probe"))]
#[path = "ownership_probe/fixture.rs"]
pub mod fixture;

use std::{
    ffi::{OsStr, OsString},
    io,
};

const QUALIFICATION: &str = "windows_job_ownership_qualification";
const RECEIPT: &str = "FESTERM_WINDOWS_JOB_OWNERSHIP_PROBE_V1: PASS";

fn explicitly_selected(args: impl Iterator<Item = OsString>) -> bool {
    let args: Vec<OsString> = args.collect();
    let required = ["--ignored", "--exact", QUALIFICATION, "--nocapture"];
    args.len() == required.len()
        && required.iter().all(|required| {
            args.iter()
                .filter(|argument| argument.as_os_str() == OsStr::new(required))
                .count()
                == 1
        })
}

#[test]
#[ignore = "experimental: dedicated GitHub-hosted Windows job only"]
fn windows_job_ownership_qualification() -> io::Result<()> {
    if !explicitly_selected(std::env::args_os().skip(1)) {
        return Err(io::Error::other(
            "ownership probe refused: select the exact ignored qualification test with --nocapture",
        ));
    }
    runner_gate::require_current()?;

    #[cfg(all(windows, feature = "experimental-windows-job-probe"))]
    {
        let helper = std::path::Path::new(env!(
            "CARGO_BIN_EXE_festerm-windows-job-ownership-probe-child"
        ));
        assert!(helper.is_absolute());
        fixture::probe::observer(helper)?;
        println!("\n{RECEIPT}");
        Ok(())
    }
    #[cfg(not(all(windows, feature = "experimental-windows-job-probe")))]
    {
        Err(io::Error::other(
            "ownership probe refused: the Windows fixture feature is required",
        ))
    }
}

#[test]
fn runner_gate_requires_every_hosted_windows_signal() {
    let lookup = |name: &str| {
        runner_gate::REQUIRED_ENVIRONMENT
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    };
    assert!(runner_gate::require_with(true, lookup).is_ok());
    assert!(runner_gate::require_with(false, lookup).is_err());
    for (name, _) in runner_gate::REQUIRED_ENVIRONMENT {
        assert!(runner_gate::require_with(true, |key| {
            if key == name {
                None
            } else {
                lookup(key)
            }
        })
        .is_err());
        assert!(runner_gate::require_with(true, |key| {
            if key == name {
                Some("not-authorized".to_owned())
            } else {
                lookup(key)
            }
        })
        .is_err());
    }
}

#[test]
fn child_environment_is_allowlisted_sorted_and_double_terminated() {
    let environment = runner_gate::child_environment();
    assert!(environment.ends_with(&[0, 0]));
    let entries: Vec<String> = environment[..environment.len() - 2]
        .split(|unit| *unit == 0)
        .map(|entry| String::from_utf16(entry).unwrap())
        .collect();
    let expected: Vec<String> = runner_gate::REQUIRED_ENVIRONMENT
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    assert_eq!(entries, expected);
    assert!(runner_gate::REQUIRED_ENVIRONMENT
        .windows(2)
        .all(|entries| entries[0].0 < entries[1].0));
}

#[test]
fn qualification_and_child_roles_require_exact_arguments() {
    let selected = |args: &[&str]| explicitly_selected(args.iter().map(OsString::from));
    assert!(selected(&[
        "--ignored",
        "--exact",
        QUALIFICATION,
        "--nocapture"
    ]));
    assert!(!selected(&["--ignored", QUALIFICATION, "--nocapture"]));
    assert!(!selected(&["--exact", QUALIFICATION, "--nocapture"]));
    assert!(!selected(&[
        "--ignored",
        "--exact",
        QUALIFICATION,
        "--nocapture",
        "--list"
    ]));
    assert!(!selected(&[
        "--ignored",
        "--exact",
        QUALIFICATION,
        QUALIFICATION
    ]));

    let parse = |args: &[&str]| role::parse(args.iter().map(OsString::from));
    assert_eq!(
        parse(&["--ownership-probe-creator"]).unwrap(),
        role::Role::Creator
    );
    assert_eq!(
        parse(&["--ownership-probe-worker"]).unwrap(),
        role::Role::Worker
    );
    for args in [
        &[][..],
        &["--list"][..],
        &["unmatched-filter"][..],
        &["--ownership-probe-creator", "--list"][..],
    ] {
        assert!(parse(args).is_err());
    }
}

fn run_without_ambient_environment(
    executable: &std::path::Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> io::Result<std::process::Output> {
    use std::{
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct ChildGuard(Option<Child>);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                let _ = child.kill();
                let deadline = Instant::now() + Duration::from_secs(10);
                while Instant::now() < deadline {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    let child = Command::new(executable)
        .args(args)
        .env_clear()
        .envs(extra_env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut child = ChildGuard(Some(child));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child
            .0
            .as_mut()
            .expect("child is owned")
            .try_wait()?
            .is_some()
        {
            return child.0.take().expect("child is owned").wait_with_output();
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "non-destructive test subprocess timed out",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn self_hosted_environment() -> [(&'static str, &'static str); 7] {
    let mut environment = runner_gate::REQUIRED_ENVIRONMENT;
    for (name, value) in &mut environment {
        if *name == "FESTERM_WINDOWS_JOB_RUNNER_ENVIRONMENT" || *name == "RUNNER_ENVIRONMENT" {
            *value = "self-hosted";
        }
    }
    environment
}

#[test]
fn discovery_filters_and_disabled_execution_do_not_run_the_probe() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let listed = run_without_ambient_environment(&executable, &["--list"], &[])?;
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout)
        .lines()
        .any(|line| line == format!("{QUALIFICATION}: test")));

    let unmatched = run_without_ambient_environment(
        &executable,
        &["--ignored", "--exact", "not-a-probe-test", "--nocapture"],
        &[],
    )?;
    assert!(unmatched.status.success());
    assert!(String::from_utf8_lossy(&unmatched.stdout).contains("running 0 tests"));
    assert!(!String::from_utf8_lossy(&unmatched.stdout).contains(RECEIPT));

    let opted_in_only = [("FESTERM_WINDOWS_JOB_DISPOSABLE_VM", "1")];
    let denied = run_without_ambient_environment(
        &executable,
        &["--ignored", "--exact", QUALIFICATION, "--nocapture"],
        &opted_in_only,
    )?;
    assert!(!denied.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&denied.stdout),
        String::from_utf8_lossy(&denied.stderr)
    );
    assert!(output.contains("ownership probe refused"));
    assert!(!output.contains(RECEIPT));

    let denied = run_without_ambient_environment(
        &executable,
        &["--ignored", "--exact", QUALIFICATION, "--nocapture"],
        &self_hosted_environment(),
    )?;
    assert!(!denied.status.success());
    assert!(!String::from_utf8_lossy(&denied.stdout).contains(RECEIPT));

    let broad = run_without_ambient_environment(&executable, &["--ignored", "--nocapture"], &[])?;
    assert!(!broad.status.success());
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&broad.stdout),
        String::from_utf8_lossy(&broad.stderr)
    );
    assert!(output.contains("select the exact ignored qualification test"));
    assert!(!output.contains(RECEIPT));
    Ok(())
}

#[cfg(not(feature = "experimental-windows-job-probe"))]
#[test]
fn disabled_fixture_cannot_emit_a_qualification_receipt() -> io::Result<()> {
    let output = run_without_ambient_environment(
        &std::env::current_exe()?,
        &["--ignored", "--exact", QUALIFICATION, "--nocapture"],
        &runner_gate::REQUIRED_ENVIRONMENT,
    )?;
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains(RECEIPT));
    Ok(())
}

#[cfg(feature = "experimental-windows-job-probe")]
#[test]
fn direct_child_roles_refuse_untrusted_or_unrelated_invocations() -> io::Result<()> {
    let helper = std::path::Path::new(env!(
        "CARGO_BIN_EXE_festerm-windows-job-ownership-probe-child"
    ));
    for args in [
        &["--list"][..],
        &["unmatched-filter"][..],
        &["--ownership-probe-creator"][..],
        &["--ownership-probe-worker"][..],
        &["--ownership-probe-creator", "--list"][..],
    ] {
        let output = run_without_ambient_environment(
            helper,
            args,
            &[("FESTERM_WINDOWS_JOB_DISPOSABLE_VM", "1")],
        )?;
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(RECEIPT));
    }
    for role in ["--ownership-probe-creator", "--ownership-probe-worker"] {
        let self_hosted =
            run_without_ambient_environment(helper, &[role], &self_hosted_environment())?;
        assert!(!self_hosted.status.success());
        assert!(!String::from_utf8_lossy(&self_hosted.stdout).contains(RECEIPT));
    }
    Ok(())
}
