<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "3ad1d287b2e72483231299b4a71fb8b624ccf289a1395518e5b7ca052d554939", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established and authenticated | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Starting | Stopped | shutdown before connection completes | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Stopping | stop requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport loss or SSH channel closes | `crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Running | Exited | remote process or PTY child exits | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | reader/writer or worker error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown completes after cancellation | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, `crates/festerm-tunnel-bridge/src/mobile.rs`, +1 more.
