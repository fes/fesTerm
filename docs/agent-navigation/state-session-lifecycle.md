<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "02f61644e9154e4e7dc28ade47939644987c60b789b80c18e180ee40683b1ed3", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | attach succeeds and terminal becomes live | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopping | shutdown requested before session reaches running | `crates/festerm-pty/src/lib.rs::request_stop` |
| Starting | Failed | startup attachment or auth fails before running | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Disconnected | transport loss unexpectedly cuts the live session | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::try_recv_event` |
| Running | Exited | child or remote session exits normally | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | runtime or transport failure while live | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | shutdown completes while session is live | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Stopping | Stopped | shutdown finishes after stop request | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event`<br>`crates/festerm-pty/src/lib.rs::request_stop` |
| Disconnected | Starting | manual reconnect resumes a disconnected session | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user cancels reconnect or closes disconnected session | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 4 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, `crates/festerm-tunnel-bridge/src/mobile.rs`, +1 more.
