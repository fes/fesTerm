<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "18051e72eac7c710c13606588060819ae1d50bc36ebe77f3d3a80cd77c78402b", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Running | Stopping | shutdown requested | `crates/festerm-ssh/src/lib.rs::wait_for_running_ssh_operation` |
| Running | Failed | permanent auth or transport failure | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | cleanup completed | `crates/festerm-ssh/src/lib.rs::sftp_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Running, Stopping, Disconnected, Exited, Failed, Stopped).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Starting, Disconnected).
- FAIL: every reachable state can reach a terminal state (Starting).

## Coverage

Transitions cite 1 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +5 more.
