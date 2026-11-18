<!-- agent-navigation: {"cited": ["crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "3e553caaf2ea238128c5bfe37afd651be5adfd71e8be645ff7781e0515fbba4f", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Starting | Disconnected | transport loss before recovery resumes | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Starting | Failed | initial connection fails permanently | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | shutdown requested | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport loss and recovery unavailable | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | remote session exits cleanly | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | fatal SSH error while active | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Stopping | Stopped | shutdown completes | `crates/festerm-ssh/src/lib.rs::sftp_worker`<br>`crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | session shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +2 more.
