<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "558c8d6c0822eb06bbc4e8b5e6ed41d05cb96c67c5b7e7dcf730da4aea626406", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Disconnected | persistent reconnect failed | `crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Starting | Stopped | cancelled reconnect | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect` |
| Running | Stopping | shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost without auto recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | child process exits | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | local child/read failure | `crates/festerm-pty/src/lib.rs::control_worker` |
| Stopping | Stopped | shutdown completes | `crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | manual reconnect approved | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | manual shutdown while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 3 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, +3 more.
