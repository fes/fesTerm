<!-- agent-navigation: {"cited": ["crates/festerm-javascript-session/src/lib.rs", "crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7aabc6e4d3d4c37af4744a312d344909a9816d2269c25f9be9d240ad4f2907df", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | startup succeeds and shell is ready | `crates/festerm-pty/src/lib.rs::start_with_notifier`<br>`crates/festerm-javascript-session/src/lib.rs::runtime_worker` |
| Running | Stopping | shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-javascript-session/src/lib.rs::request_stop` |
| Running | Disconnected | remote transport is lost | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child process exits normally | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Failed | backend reports an error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | worker closes cleanly | `crates/festerm-javascript-session/src/lib.rs::runtime_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Disconnected).
- FAIL: every reachable state can reach a terminal state (Disconnected).

## Coverage

Transitions cite 4 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +2 more.
