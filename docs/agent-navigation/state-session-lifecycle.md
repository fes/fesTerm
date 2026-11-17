<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "cafb2bd2b18a4ffe0cd695c4cc92da09a59bd523623a6cb71371629cc0792332", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Disconnected | reconnect backoff exhausted | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Starting | Stopped | reconnect delay timeout shutdown | `crates/festerm-ssh/src/lib.rs::schedule_reconnect`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown` |
| Running | Disconnected | transport lost no retry resumed | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child exits cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | read/write failure | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::fail` |
| Stopping | Stopped | shutdown completes | `crates/festerm-serial/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Disconnected | Starting | manual or automatic reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::schedule_reconnect` |
| Disconnected | Stopped | user shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, `crates/festerm-tunnel-bridge/src/transport.rs`.
