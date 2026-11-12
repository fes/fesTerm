<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "02381ba062be0f5bfef335cd0d2441e696a7671f813ff04e206ae0cabcce6e05", "state_version": "1"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's file names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | authenticated SSH channel established | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-serial/src/lib.rs::open` |
| Starting | Failed | host connection fails during refresh | `crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Running | Stopping | shutdown requested while active | `crates/festerm-pty/src/lib.rs::request_stop`<br>`crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport loss without automatic recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::event` |
| Running | Exited | remote shell exits | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::run` |
| Running | Failed | PTY write or child-watch failure | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::reader_worker` |
| Running | Stopped | mobile bridge detaches cleanly | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Stopping | Stopped | PTY shutdown completes cleanly | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-serial/src/lib.rs::control_worker` |
| Disconnected | Starting | explicit reconnect is requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 19 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app.rs`, `app/festerm/src/app/automation.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/javascript.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, +2 more.
