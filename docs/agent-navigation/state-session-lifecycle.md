<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "e9e4a985641fca4420c7717a7a445e3ef135d2f51ad8872c1e7f17528f54cca6", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | startup completes and the session is live | `crates/festerm-pty/src/lib.rs::start_with_notifier` |
| Running | Stopping | shutdown or cancellation is requested | `crates/festerm-ssh/src/lib.rs::sftp_worker`<br>`crates/festerm-ssh/src/lib.rs::finish_sftp_cancellation` |
| Running | Disconnected | transport is lost and recovery is not resumed | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Exited | child process exits cleanly | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | fatal worker or transport error occurs | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown cleanup finishes | `crates/festerm-ssh/src/lib.rs::sftp_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect or reconnect request begins | `crates/festerm-ssh/src/lib.rs::schedule_reconnect` |
| Disconnected | Stopped | user shuts down while disconnected or reconnect is cancelled | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 3 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
