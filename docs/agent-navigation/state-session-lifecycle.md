<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "279134f578e08a60821c619ae794f6b23be777440f6e1e251e051492272337ac", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Failed | startup or connect failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | stop requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost without recovery | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | session process exited | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | fatal runtime error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | shutdown while active | `crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown completes | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-pty/src/lib.rs::control_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Running, Stopping, Disconnected, Exited, Stopped).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Disconnected).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +2 more.
