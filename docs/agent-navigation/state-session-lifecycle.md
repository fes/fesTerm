<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7de1c30b19035bc0fe5de3cc06072850249ce1a2f3f06e395bb97f5df0d264ec", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | session startup completes and process is ready | `crates/festerm-pty/src/lib.rs::start_with_notifier`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Failed | connect attempt fails before session starts | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | shutdown requested on a running session | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | transport loss while connected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | remote process exited cleanly | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | runtime failure or failed transport while active | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown completed cleanly | `crates/festerm-ssh/src/lib.rs::stop_handle` |
| Disconnected | Starting | user reconnects or recovery resumes | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown after disconnection | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, +4 more.
