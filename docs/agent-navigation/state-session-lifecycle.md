<!-- agent-navigation: {"cited": ["crates/festerm-javascript-session/src/lib.rs", "crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "c77eb510e293905ccb909ec62f64911016740cc36ec0d40ae5d519dfab8f9e3f", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | successful session startup | `crates/festerm-pty/src/lib.rs::start_with_notifier`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Stopping | shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-javascript-session/src/lib.rs::request_stop` |
| Running | Disconnected | persistent daemon closes unexpectedly | `crates/festerm-sessiond/src/lib.rs::client_worker`<br>`crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Running | Exited | child exits with a status | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Failed | write/read/transport error during session | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | session shutdown command finishes | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Stopping | Stopped | shutdown completes | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Disconnected | Running | reconnect resumes a recovered session | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Disconnected | Stopped | reconnect is cancelled before resume | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 3 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-ssh/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
