<!-- agent-navigation: {"cited": ["crates/festerm-pty/src/lib.rs", "crates/festerm-sessiond/src/lib.rs", "crates/festerm-ssh/src/lib.rs", "crates/festerm-tunnel-bridge/src/host_client/session.rs", "crates/festerm-tunnel-bridge/src/mobile.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "1aa0be9db8208f8d7a91adab43d6bf99bd2ed19396471313e5e3c872b9b5650c", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection established and authenticated | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/host_client/session.rs::refresh` |
| Starting | Failed | connection attempt permanently failed | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopping | user shutdown request | `crates/festerm-tunnel-bridge/src/host_client/session.rs::try_shutdown`<br>`crates/festerm-pty/src/lib.rs::request_stop` |
| Running | Disconnected | SSH transport lost while running | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-tunnel-bridge/src/mobile.rs::finish` |
| Running | Exited | child exited normally | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-pty/src/lib.rs::control_worker` |
| Running | Failed | local PTY or SSH worker error | `crates/festerm-pty/src/lib.rs::control_worker`<br>`crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | shutdown completed | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Stopping | Stopped | detach reply or process exit completed | `crates/festerm-tunnel-bridge/src/host_client/session.rs::event`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Starting | manual reconnect requested | `crates/festerm-ssh/src/lib.rs::ssh_worker`<br>`crates/festerm-sessiond/src/lib.rs::try_reconnect` |
| Disconnected | Running | reconnect succeeded | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | reconnect cancelled or shutdown while disconnected | `crates/festerm-sessiond/src/lib.rs::finish_cancelled_reconnect`<br>`crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 5 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-tunnel-bridge/src/host_terminal/native.rs`, `crates/festerm-tunnel-bridge/src/transport.rs`.
