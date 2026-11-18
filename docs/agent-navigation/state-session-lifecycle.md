<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5b6e95a2d773e1cbbd8c179e777bb520c45f47e298a942733bc887565ffd00e6", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Stopping | shutdown requested before live startup completes | `crates/festerm-serial/src/lib.rs::request_stop`<br>`crates/festerm-pty/src/lib.rs::request_stop` |
| Starting | Failed | startup error or rejected auth | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopping | user or worker requested shutdown | `crates/festerm-serial/src/lib.rs::request_stop`<br>`crates/festerm-ssh/src/lib.rs::wait_for_running_ssh_operation` |
| Running | Disconnected | transport lost without durable recovery | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child or remote session exited cleanly | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | backend reported unrecoverable failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | shutdown completed while live | `crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown cleanup completed | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Running, Disconnected, Exited).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Disconnected).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +2 more.
