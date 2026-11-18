<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "7cb66642086953a87060637b25e6cce1a0f678e2ef1cedac7ecfd31b680c51ca", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopping | stop requested while start in progress | `crates/festerm-pty/src/lib.rs::request_stop` |
| Starting | Stopped | connection loop exits during shutdown | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown or cancellation requested | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost and no recovery resumed | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | child or remote process exits cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopped | shutdown command completes cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Stopping | Stopped | reader exits during shutdown | `crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Running | reconnect succeeds | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown while waiting for manual recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Failed).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 3 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
