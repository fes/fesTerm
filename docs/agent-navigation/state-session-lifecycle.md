<!-- agent-navigation: {"cited": ["crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f61784bf351961eb8769302efd65e91172048257f4778b49f35e9c0c0c5026d5", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | session connects and worker marks it running | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown requested while session is active | `crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost and no automatic recovery resumed it | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | channel exits cleanly or terminal reports exit | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | fatal SSH or serial worker error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | disconnect handle completes shutdown | `crates/festerm-ssh/src/lib.rs::stop_handle` |
| Stopping | Stopped | shutdown completes and worker stops | `crates/festerm-serial/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect or recovery retry begins | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user chooses shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
