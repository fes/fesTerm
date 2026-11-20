<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "2aaace15ada1f70e621a75d18b4fbe8c7954d1ca1b5b3341390ba5b1f618f129", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | SSH channel established and authenticated | `crates/festerm-tunnel-bridge/src/mobile.rs::run` |
| Starting | Failed | auth or setup failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-ssh/src/lib.rs::sftp_failure_with_kind` |
| Starting | Stopped | shutdown before session became live | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Stopping | shutdown requested | `crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport loss or displaced attachment | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Running | Exited | remote session exited cleanly | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Failed | non-recoverable SSH/session error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-ssh/src/lib.rs::sftp_failure_with_kind` |
| Stopping | Stopped | detach or shutdown cleanup completed | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event`<br>`crates/festerm-ssh/src/lib.rs::stop_handle` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Disconnected).
- FAIL: every reachable state can reach a terminal state (Disconnected).

## Coverage

Transitions cite 3 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, +2 more.
