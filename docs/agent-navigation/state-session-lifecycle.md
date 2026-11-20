<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "cb8b5dc46e1363d6b6cae649f9c58312f21aa2f54cd65b0053e757a8b986c8da", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | session starts and transport reaches running state | `crates/festerm-pty/src/lib.rs::start_with_notifier`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopping | shutdown requested before startup completes | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Stopping | shutdown requested while active | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport loss while running; no auto recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | child process or remote session exits cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | failure path records terminal error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | explicit shutdown stops session | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | user reconnect request after disconnect | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user shuts down while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Stopping).
- FAIL: every reachable state can reach a terminal state (Stopping).

## Coverage

Transitions cite 3 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +2 more.
