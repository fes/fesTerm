<!-- agent-navigation: {"cited": ["crates/festerm-javascript-session/src/lib.rs", "crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "74c786cd6152d24a465a63a00cd4d3bbaf932ff907bb5a078fca3ec29d4dfa75", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | runtime startup succeeded | `crates/festerm-javascript-session/src/lib.rs::runtime_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Starting | Failed | startup worker failed | `crates/festerm-javascript-session/src/lib.rs::fail`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Running | Stopping | shutdown requested | `crates/festerm-javascript-session/src/lib.rs::request_stop`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown` |
| Running | Disconnected | worker cancelled or stream failed | `crates/festerm-tunnel-bridge/src/host_client/session.rs::cancel`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child process exited cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Failed | backend failure or stale attach | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-javascript-session/src/lib.rs::fail` |
| Running | Stopped | shutdown completed or detached | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Stopping | Stopped | shutdown completed | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Disconnected | Starting | reconnect began | `crates/festerm-sessiond/src/lib.rs::try_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-ssh/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +1 more.
