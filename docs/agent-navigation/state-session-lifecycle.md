<!-- agent-navigation: {"cited": ["crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "10f532af44cd6a6b15845ebbbd48d3c1b9381cc03271f44756b22c0edcd74394", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established after auth and PTY setup | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Stopping | shutdown requested while session is active | `crates/festerm-ssh/src/lib.rs::wait_for_running_ssh_operation`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Disconnected | transport lost or daemon closed unexpectedly | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Exited | remote process exited normally | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Failed | fatal SSH/SFTP failure or unrecoverable startup error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-ssh/src/lib.rs::sftp_failure_with_kind` |
| Stopping | Stopped | disconnect and cleanup completed | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Disconnected | Starting | manual reconnect requested after disconnected state | `crates/festerm-sessiond/src/lib.rs::try_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +4 more.
