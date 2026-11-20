<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-serial/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "754b934aa8174e9084f38d1b31e908edf127c0cb7d68d29e9cac1fd091d8fc83", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | SSH connection authenticated and transport opened | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Failed | reconnect worker startup or connection failed while starting | `crates/festerm-sessiond/src/lib.rs::try_reconnect` |
| Running | Stopping | shutdown requested on active session | `crates/festerm-serial/src/lib.rs::request_stop` |
| Running | Disconnected | transport lost or remote session cancelled | `crates/festerm-sessiond/src/lib.rs::reconnect_failed` |
| Running | Exited | child process exited successfully | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | local PTY write or worker observation failed | `crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Stopped | session detached or explicitly ended without a child exit | `crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Stopping | Stopped | shutdown completed after canceling reader | `crates/festerm-pty/src/lib.rs::control_worker` |
| Disconnected | Starting | explicit reconnect requested to re-establish session | `crates/festerm-sessiond/src/lib.rs::try_reconnect` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_client/session.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, +1 more.
