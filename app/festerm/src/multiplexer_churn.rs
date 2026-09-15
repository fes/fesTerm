//! Opt-in real-provider coverage. The runner installs test-owned wrappers:
//! tmux always uses -L, screen always uses a private SCREENDIR.

use super::*;
use festerm_config::PersistenceProviderKind as Provider;
use festerm_pty::LocalPtySession;
use festerm_session::{Session, SessionEvent, SessionLifecycle, TerminalSize};
use std::path::PathBuf;

struct OwnedSessions {
    provider: Provider,
    names: Vec<String>,
}

impl Drop for OwnedSessions {
    fn drop(&mut self) {
        let mut errors = Vec::new();
        for name in &self.names {
            if let Err(error) = kill(self.provider, name) {
                errors.push(error);
            }
        }
        if !errors.is_empty() {
            eprintln!("multiplexer cleanup incomplete: {errors:?}");
            assert!(
                std::thread::panicking(),
                "multiplexer cleanup failed: {errors:?}"
            );
        }
    }
}

fn list(provider: Provider) -> Result<Vec<MultiplexerSession>, String> {
    match provider {
        Provider::Tmux => list_tmux_sessions(),
        Provider::Screen => list_screen_sessions(),
        _ => unreachable!(),
    }
}

fn kill(provider: Provider, target: &str) -> Result<(), String> {
    let output = match provider {
        Provider::Tmux => {
            let target = format!("={target}");
            bounded_output("tmux", &["kill-session", "-t", &target])?
        }
        Provider::Screen => bounded_output("screen", &["-S", screen_target(target), "-X", "quit"])?,
        _ => unreachable!(),
    };
    let output = output.ok_or("provider disappeared during cleanup")?;
    let message = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success()
        || message.contains("can't find session")
        || message.contains("no server running")
        || message.contains("No such file or directory")
        || message.contains("No screen session found")
    {
        Ok(())
    } else {
        Err(format!("provider cleanup failed: {message}"))
    }
}

fn create(provider: Provider, name: &str, nonce: &str) {
    // The state and PID are expanded by the existing shell in response to a
    // *new* challenge after attach. Echo/replay cannot manufacture this line.
    let script = format!("state={nonce}; printf 'READY:%s:%s\\n' \"$state\" \"$$\"; while IFS= read -r challenge; do case \"$challenge\" in exit) exit 0;; *) printf 'STATE:%s:%s:%s\\n' \"$state\" \"$$\" \"$challenge\";; esac; done");
    let output = match provider {
        Provider::Tmux => bounded_output(
            "tmux",
            &["new-session", "-d", "-s", name, "/bin/sh", "-c", &script],
        ),
        Provider::Screen => bounded_output("screen", &["-dmS", name, "/bin/sh", "-c", &script]),
        _ => unreachable!(),
    }
    .unwrap()
    .expect("provider must exist");
    assert!(
        output.status.success(),
        "create: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[track_caller]
fn poll(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "provider readiness deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn challenge(client: &LocalPtySession, nonce: &str, fresh: &str) -> String {
    let prefix = format!("STATE:{nonce}:");
    let mut output = Vec::new();
    let mut answer = None;
    let mut sent = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    poll(|| {
        while let Ok(event) = client.try_recv_event() {
            if let SessionEvent::Output(bytes) = event {
                output.extend(bytes);
            }
        }
        assert!(output.len() < 1024 * 1024);
        let text = String::from_utf8_lossy(&output);
        assert!(
            std::time::Instant::now() < deadline,
            "missing {nonce}/{fresh}: {text:?}"
        );
        if !sent && text.contains(&format!("READY:{nonce}:")) {
            client
                .try_send_input(format!("{fresh}\r").as_bytes())
                .unwrap();
            sent = true;
        }
        for line in text.lines() {
            if let Some(rest) = line.split(&prefix).nth(1) {
                if let Some((pid, echoed)) = rest.split_once(':') {
                    if echoed.trim() == fresh && pid.parse::<u32>().is_ok() {
                        answer = Some(pid.to_owned());
                        return true;
                    }
                }
            }
        }
        false
    });
    answer.unwrap()
}

fn screen_attachment_failure_keeps_launcher_and_existing_client(root: &std::path::Path) {
    use crate::tabs::{AppCommand, AppState, TabContent};
    let selected = list(Provider::Screen).unwrap().remove(0);
    let primary = LocalPtySession::start(
        attach_profile(Provider::Screen, &selected).unwrap(),
        TerminalSize::new(80, 24).unwrap(),
    )
    .unwrap();
    poll(|| {
        client_attached(
            Provider::Screen,
            &selected,
            primary.process_id().unwrap(),
            primary.terminal_device(),
        )
        .unwrap()
    });
    let pid = challenge(&primary, "sentinel", "primary");
    let selected = list(Provider::Screen).unwrap().remove(0);
    assert!(selected.attached);
    assert!(!client_attached(
        Provider::Screen,
        &selected,
        i32::MAX as u32,
        primary.terminal_device()
    )
    .unwrap());

    struct Gate(PathBuf);
    impl Drop for Gate {
        fn drop(&mut self) {
            std::fs::remove_file(&self.0).unwrap();
        }
    }
    let gate = Gate(root.join("screen-attach-failure"));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&gate.0)
        .unwrap();
    let context = eframe::egui::Context::default();
    let mut state = AppState::for_test();
    if !state.show_resumable_sessions() {
        state.dispatch(AppCommand::ToggleShowResumableSessions, &context);
    }
    state.dispatch(
        AppCommand::ResumeMultiplexerSession {
            provider: Provider::Screen,
            session: selected.clone(),
        },
        &context,
    );
    poll(|| {
        state.update_running_sessions(&context);
        assert!(
            matches!(state.active_tab().content, TabContent::Launcher),
            "another client's attachment must not publish this failed client"
        );
        state.resume_error.is_some()
    });
    let error = state.resume_error.as_ref().unwrap();
    assert!(error.contains("No replacement shell"));
    assert!(
        error.contains("controlled screen attachment failure"),
        "{error}"
    );
    assert_eq!(list(Provider::Screen).unwrap().len(), 1);
    primary.try_send_input(b"unaffected\r").unwrap();
    let mut output = Vec::new();
    poll(|| {
        while let Ok(event) = primary.try_recv_event() {
            if let SessionEvent::Output(bytes) = event {
                output.extend(bytes);
            }
        }
        assert!(output.len() < 1024 * 1024);
        String::from_utf8_lossy(&output).contains(&format!("STATE:sentinel:{pid}:unaffected"))
    });
    drop(gate);
    state.dispatch(AppCommand::RefreshRunningSessions, &context);
    state.dispatch(
        AppCommand::ResumeMultiplexerSession {
            provider: Provider::Screen,
            session: selected.clone(),
        },
        &context,
    );
    poll(|| {
        state.update_running_sessions(&context);
        assert!(state.resume_error.is_none(), "{:?}", state.resume_error);
        matches!(state.active_tab().content, TabContent::Session(_))
    });
    let TabContent::Session(tab) = &mut state.active_tab_mut().content else {
        unreachable!()
    };
    tab.controller
        .session()
        .unwrap()
        .try_send_input(b"recovered\r")
        .unwrap();
    poll(|| {
        tab.controller.pump_events(&mut tab.terminal);
        (0..24).any(|row| {
            tab.terminal
                .row_text(row)
                .is_some_and(|text| text.contains(&format!("STATE:sentinel:{pid}:recovered")))
        })
    });
    tab.controller
        .session()
        .unwrap()
        .shutdown(Duration::from_secs(2))
        .unwrap();
    drop(state);
    let remaining = list(Provider::Screen).unwrap();
    assert_eq!(
        remaining.len(), 1,
        "Screen shell disappeared after shutting down the recovered client; primary lifecycle: {:?}",
        primary.lifecycle()
    );
    assert_eq!(remaining[0].match_key, selected.match_key);
    assert!(remaining[0].attached);
    primary.try_send_input(b"after-detach\r").unwrap();
    let mut output = Vec::new();
    poll(|| {
        while let Ok(event) = primary.try_recv_event() {
            if let SessionEvent::Output(bytes) = event {
                output.extend(bytes);
            }
        }
        assert!(output.len() < 1024 * 1024);
        String::from_utf8_lossy(&output).contains(&format!("STATE:sentinel:{pid}:after-detach"))
    });
    primary.shutdown(Duration::from_secs(2)).unwrap();
    poll(|| {
        list(Provider::Screen)
            .unwrap()
            .iter()
            .all(|entry| !entry.attached)
    });
    eprintln!("provider=screen owned-client-failure-recovery-and-continuity status=pass");
}

#[test]
#[ignore = "run via scripts/check_running_sessions.py for isolated provider namespaces"]
fn isolated_multiplexer_discovery_churn() {
    eprintln!("churn-process pid={}", std::process::id());
    let root = std::env::var_os("FESTERM_MUX_CHURN_ROOT")
        .map(PathBuf::from)
        .expect("use scripts/check_running_sessions.py; never run against ordinary user sessions");
    assert!(root.join("owned-running-session-validation").is_file());
    let batch = setting("FESTERM_SESSION_CHURN_BATCH", 8, 128);
    let cycles = setting("FESTERM_SESSION_CHURN_CYCLES", 3, 100);
    for (provider, program) in [(Provider::Tmux, "tmux"), (Provider::Screen, "screen")] {
        if !root.join("bin").join(program).is_file() {
            eprintln!("provider={program} status=skipped reason=binary-unavailable");
            continue;
        }
        assert!(
            list(provider).unwrap().is_empty(),
            "runner namespace must start empty"
        );
        eprintln!("provider={program} phase=starting");
        let mut owned = OwnedSessions {
            provider,
            names: Vec::new(),
        };
        let sentinel = "unrelated-owned-sentinel";
        owned.names.push(sentinel.into());
        create(provider, sentinel, "sentinel");
        poll(|| list(provider).unwrap().len() == 1);
        let sentinel_identity = list(provider).unwrap()[0].match_key.clone();
        if provider == Provider::Screen {
            screen_attachment_failure_keeps_launcher_and_existing_client(&root);
        }
        let context = eframe::egui::Context::default();
        let mut discovery = crate::discovery::Discovery::default();
        for cycle in 0..cycles {
            for index in 0..batch {
                discovery.refresh();
                discovery.update(true, &context);
                let name = format!("churn-{index:04}");
                owned.names.push(name.clone());
                create(provider, &name, &format!("c{cycle}i{index}"));
            }
            poll(|| list(provider).unwrap().len() == batch + 1);
            let snapshot = list(provider).unwrap();
            assert!(snapshot
                .windows(2)
                .all(|pair| (&pair[0].name, &pair[0].match_key)
                    < (&pair[1].name, &pair[1].match_key)));
            let mut keys = std::collections::BTreeSet::new();
            assert!(snapshot.iter().all(|entry| keys.insert(&entry.match_key)));
            for (index, selected) in snapshot
                .iter()
                .filter(|entry| entry.name != sentinel)
                .enumerate()
            {
                let nonce = format!("c{cycle}i{index}");
                let profile = attach_profile(provider, selected).unwrap();
                let client =
                    LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
                poll(|| {
                    client_attached(
                        provider,
                        selected,
                        client.process_id().unwrap(),
                        client.terminal_device(),
                    )
                    .unwrap()
                });
                let pid = challenge(&client, &nonce, "before");
                // Duplicate clicks/refreshes cannot create another server shell.
                assert_eq!(list(provider).unwrap().len(), batch + 1 - index);
                client
                    .shutdown(Duration::from_secs(2))
                    .expect("detached client's control and reader workers must stop");
                drop(client);
                poll(|| {
                    list(provider)
                        .unwrap()
                        .iter()
                        .any(|entry| entry.match_key == selected.match_key && !entry.attached)
                });
                let profile = attach_profile(provider, selected).unwrap();
                let resumed =
                    LocalPtySession::start(profile, TerminalSize::new(80, 24).unwrap()).unwrap();
                assert_eq!(challenge(&resumed, &nonce, "after"), pid);
                resumed.try_send_input(b"exit\r").unwrap();
                poll(|| {
                    !matches!(
                        resumed.lifecycle(),
                        SessionLifecycle::Starting | SessionLifecycle::Running
                    )
                });
                resumed
                    .shutdown(Duration::from_secs(2))
                    .expect("exited client's control and reader workers must stop");
                drop(resumed);
                poll(|| {
                    !list(provider)
                        .unwrap()
                        .iter()
                        .any(|entry| entry.match_key == selected.match_key)
                });
                assert!(
                    attach_profile(provider, selected).is_err(),
                    "stale click must not spawn a replacement"
                );
                create(provider, &selected.name, "replacement");
                poll(|| {
                    list(provider)
                        .unwrap()
                        .iter()
                        .any(|entry| entry.name == selected.name)
                });
                let replacement = list(provider)
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.name == selected.name)
                    .unwrap();
                assert_ne!(replacement.match_key, selected.match_key);
                assert!(
                    attach_profile(provider, selected).is_err(),
                    "same name is not same identity"
                );
                kill(
                    provider,
                    if provider == Provider::Tmux {
                        &replacement.name
                    } else {
                        &replacement.match_key
                    },
                )
                .unwrap();
                for _ in 0..100 {
                    discovery.refresh();
                }
                discovery.update(true, &context);
                poll(|| {
                    !list(provider)
                        .unwrap()
                        .iter()
                        .any(|entry| entry.name == selected.name)
                });
                let remaining = list(provider).unwrap();
                assert!(remaining.iter().any(|entry| entry.name == sentinel && entry.match_key == sentinel_identity), "sentinel {sentinel_identity}, remaining {remaining:?}");
            }
            assert_eq!(list(provider).unwrap().len(), 1);
            eprintln!(
                "provider={program} cycle={} batch={batch} status=pass",
                cycle + 1
            );
        }
        discovery.update(false, &context);
        drop(discovery);
        drop(owned);
        poll(|| list(provider).unwrap().is_empty());
    }
}

fn setting(name: &str, default: usize, max: usize) -> usize {
    let value = std::env::var(name)
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(default);
    assert!((1..=max).contains(&value));
    value
}
