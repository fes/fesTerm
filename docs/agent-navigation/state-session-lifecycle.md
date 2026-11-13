<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "71feb0568e9fcaf7457c62837bd366535700e4540f898c7ef48ebbcd5628a18b", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established and channel authenticated | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Stopping | shutdown requested before session reaches running | `crates/festerm-pty/src/lib.rs::request_stop` |
| Starting | Failed | connection attempt fails permanently before running | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | session shutdown requested | `crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost before recovery resumed | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | remote process exits normally | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | startup or remote failure becomes permanent | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | shutdown completes or process tree terminates | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | user reconnect requested or manual recovery accepted | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | shutdown requested while disconnected | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 2 of 16 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +2 more.
