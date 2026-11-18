<!-- agent-navigation: {"cited": ["crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "c2375b88c52acb81384e09dbbf2f8433c007ca50fef675ddd524fa4b4dbe8524", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Failed | startup failure is permanent | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | shutdown requested while session is live | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Stopping | Stopped | cleanup finished and worker exits | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Disconnected | Stopped | user chooses shutdown while disconnected | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- FAIL: every state is reachable from Starting (Running, Stopping, Disconnected, Exited, Stopped).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +4 more.
