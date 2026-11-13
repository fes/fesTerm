<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b169e36c9e83a77799c93fe7105e79854a72e4a54f4d0047c394dcb3aa900f80", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection/authenticated channel established | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopping | shutdown requested before the session runs | `crates/festerm-pty/src/lib.rs::request_stop` |
| Starting | Failed | connection/authentication or startup fails permanently | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | shutdown requested while session is active | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | remote transport loss with no automatic recovery | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | remote shell/process exits normally | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | non-transport failure or worker error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | shutdown cleanup completes and the handle closes | `crates/festerm-ssh/src/lib.rs::stop_handle` |
| Disconnected | Starting | user reconnect is requested and accepted | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 16 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +2 more.
