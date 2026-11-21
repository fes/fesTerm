<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "codex-source-verified", "sources": "1103e18d914ee53ce0808aae6e7403eccf029c66edf59cb47f4206f0b585fef3", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Running | Stopping | shutdown requested | `crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Running | Failed | session failed with a terminal error | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Stopping | Stopped | shutdown finished | `crates/festerm-ssh/src/lib.rs::sftp_worker` |

## Property checks

- FAIL: every state is reachable from Starting (Running, Stopping, Disconnected, Exited, Failed, Stopped).
- PASS: terminal states have no outgoing transitions (none).
- FAIL: every non-terminal state has an exit (Starting, Disconnected).
- FAIL: every reachable state can reach a terminal state (Starting).

## Coverage

Transitions cite 1 of 18 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-javascript-session/src/lib.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, +5 more.
