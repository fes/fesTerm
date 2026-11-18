<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9c9a63f55f96c0c3d66666a9cae73ba34a0dc192254be7ff4c400a6fd87614ff", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | session start succeeds | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-pty/src/lib.rs::start_with_notifier` |
| Starting | Failed | initial connect or auth fails | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | shutdown or stop requested | `crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport drops without auto-recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::fail_transport` |
| Running | Exited | session exits cleanly | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | runtime failure is reported | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-serial/src/lib.rs::control_worker` |
| Stopping | Stopped | worker finished shutdown | `crates/festerm-serial/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::stop_handle` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user shuts down while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +2 more.
