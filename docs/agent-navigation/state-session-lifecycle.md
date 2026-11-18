<!-- agent-navigation: {"cited": ["crates/festerm-javascript-session/src/lib.rs", "crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "d33a090d77217c94107b0fe8d91c34bb5a9e4b9bfa3719b9b8e09bbd8372740d", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::run` |
| Starting | Failed | startup attempt failed | `crates/festerm-javascript-session/src/lib.rs::runtime_worker` |
| Running | Stopping | shutdown requested | `crates/festerm-serial/src/lib.rs::request_stop`<br>`crates/festerm-javascript-session/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child process exited | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Failed | backend error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-javascript-session/src/lib.rs::fail` |
| Stopping | Stopped | reader completed shutdown | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::control_worker` |
| Disconnected | Starting | user reconnect granted | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +1 more.
