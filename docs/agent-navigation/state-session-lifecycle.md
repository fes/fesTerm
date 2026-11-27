<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "020c5bedc2006dbafd7505a80397acf2a3b3eb7fb2b09943e5257d079adaf29a", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | backend connected and the live read loop started | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Starting | Disconnected | initial recovery stream ended unexpectedly | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Stopping | shutdown requested before terminal exits | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | daemon closed or transport broke while active | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Exited | remote server reported process exit | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Running | Failed | runtime write or child observation failed | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | shutdown command was received | `crates/festerm-sessiond/src/lib.rs::client_worker` |
| Stopping | Stopped | reader finished during shutdown | `crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | reconnect begins after transport loss | `crates/festerm-sessiond/src/lib.rs::try_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-ssh/src/lib.rs`, +4 more.
