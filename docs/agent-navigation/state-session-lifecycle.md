<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "18051e72eac7c710c13606588060819ae1d50bc36ebe77f3d3a80cd77c78402b", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established and authenticated | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopped | shutdown requested before connection | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown requested while active | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport loss without durable recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | authenticated channel exited cleanly | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopped | shutdown requested during active session | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Stopping | Stopped | shutdown cleanup finished | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | manual shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Failed).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 1 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +5 more.
