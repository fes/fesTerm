<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7c8aedac7da22a212ab4016eb533f844c0af50817c225b8dcb2c5e61e363106f", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | SSH connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Disconnected | reconnect attempt loses transport before recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Failed | worker reports setup or attach failure | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Running | Stopping | user shutdown begins detach | `crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown` |
| Running | Disconnected | transport loss or displaced attachment | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Running | Exited | remote session exits | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Stopping | Stopped | detach response confirms shutdown | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Disconnected | Starting | manual recovery reconnect starts | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | manual shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, +3 more.
