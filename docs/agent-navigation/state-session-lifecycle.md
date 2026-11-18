<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "982591e77fa9ea0826a29d6fa928b7b5dd5ac47c7b7d09c8dc4b7cd2bd5801c1", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::run` |
| Starting | Failed | non-retryable auth/configuration failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Running | Stopping | shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown` |
| Running | Disconnected | transport lost and recovery unavailable | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | remote/local process exited cleanly | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | worker or IO failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | shutdown/close path | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::stop_handle` |
| Stopping | Stopped | shutdown completed | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +1 more.
