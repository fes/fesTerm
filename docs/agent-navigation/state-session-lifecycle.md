<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ad3f9b3f4d20104368a031f6bd42e695ecf3d06e7843a778f6371e95f7e84d78", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | authenticated connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Failed | connect/auth setup failed permanently | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Starting | Stopped | shutdown requested before session is running | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | stop requested or shutdown signal | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport dropped unexpectedly | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | child exited normally | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | child I/O or SSH failure | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | disconnect completes | `crates/festerm-ssh/src/lib.rs::stop_handle` |
| Stopping | Stopped | shutdown cleanup completed | `crates/festerm-ssh/src/lib.rs::sftp_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect approved | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user shuts down while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
