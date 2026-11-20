<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "02f61644e9154e4e7dc28ade47939644987c60b789b80c18e180ee40683b1ed3", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established after auth and channel open | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Starting | Disconnected | worker cancelled before live session established | `crates/festerm-tunnel-bridge/src/host_client/session.rs::cancel` |
| Starting | Failed | host attachment or initial connect failed | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Running | Stopping | shutdown or stop requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport lost and automatic recovery exhausted | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Running | Exited | child process exited successfully | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | fatal PTY or session error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | shutdown cleanup completed | `crates/festerm-ssh/src/lib.rs::sftp_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect or recovery reattempt | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::try_reconnect` |
| Disconnected | Stopped | user shut down while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, `crates/festerm-tunnel-bridge/src/mobile.rs`, +1 more.
