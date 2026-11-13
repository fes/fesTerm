<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1ab7133c7d36e1c42b19f22d9125b81ae274a47431fcc0073bb2908b247a1add", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established and the transport begins serving I/O | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-pty/src/lib.rs::start_with_notifier` |
| Starting | Failed | initial connection or worker setup fails | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Starting | Stopped | shutdown before the session is fully active | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown or cancel requested while active | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost with no automatic recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Exited | child process exits normally | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Failed | runtime I/O or process observation errors | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | application shutdown while session is active | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown completes after orderly stop | `crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect accepted | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::try_reconnect` |
| Disconnected | Running | reconnect succeeds and a live channel resumes | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::client_worker` |
| Disconnected | Stopped | user shuts down while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 3 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +2 more.
