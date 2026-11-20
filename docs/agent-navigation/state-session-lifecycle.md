<!-- agent-navigation: {"cited": ["crates/festerm-ssh/src/lib.rs"], "domain": "state-session-lifecycle", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6bc1ab68c9cbb993062da3b9d4dd58c9d631fe75bcb4e1bf85ec8eb5b64e4dd7", "state_version": "2"} -->
# State model: Session lifecycle

> Generated candidate model of `crates/festerm-session/src/lib.rs::SessionLifecycle`. States and the
> terminal set are parsed from source; transitions are model-proposed and kept
> only if the cited function's body names the target state. Not checked
> against runtime traces; verify in source.

Domain: [Sessions, PTY, serial, and persistence daemon](sessions.md). States: Starting (initial), Running, Stopping, Disconnected, Exited (terminal), Failed (terminal), Stopped (terminal).

| From | To | Trigger | Evidence |
| --- | --- | --- | --- |
| Starting | Running | connection attempt succeeds and PTY attaches | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Starting | Failed | connection setup or auth fails permanently | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Starting | Stopped | shutdown occurs before channel startup completes | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Starting | manual reconnect is requested | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Stopping | shutdown request is accepted while active | `crates/festerm-ssh/src/lib.rs::process_authenticated_commands`<br>`crates/festerm-ssh/src/lib.rs::wait_for_running_ssh_operation` |
| Running | Disconnected | SSH connection lost or liveness probe times out | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Exited | remote shell exits with exit status or signal | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Running | Failed | input, resize, or session setup error occurs | `crates/festerm-ssh/src/lib.rs::ssh_failure_with_kind` |
| Running | Stopped | shutdown is requested and completed | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Stopping | Stopped | shutdown teardown finishes | `crates/festerm-ssh/src/lib.rs::stop_handle`<br>`crates/festerm-ssh/src/lib.rs::sftp_worker` |
| Disconnected | Starting | user reconnects after a transport loss | `crates/festerm-ssh/src/lib.rs::ssh_worker` |
| Disconnected | Stopped | user shuts the disconnected session down | `crates/festerm-ssh/src/lib.rs::ssh_worker` |

## Property checks

- PASS: every state is reachable from Starting (all reachable).
- PASS: terminal states have no outgoing transitions (none).
- PASS: every non-terminal state has an exit (none).
- PASS: every reachable state can reach a terminal state (all can).

## Coverage

Transitions cite 1 of 17 production files that name a variant. A FAIL
or an uncited file means the model may be incomplete, not that the code is wrong.
Uncited: `app/festerm-mobile/src/live_ssh.rs`, `app/festerm/src/app/gallery.rs`, `app/festerm/src/automation/controller.rs`, `app/festerm/src/automation/mod.rs`, `app/festerm/src/multiplexer_churn.rs`, `app/festerm/src/session_controller.rs`, `app/festerm/src/tabs.rs`, `app/festerm/src/tunnel_reconnect.rs`, `crates/festerm-pty/src/lib.rs`, `crates/festerm-serial/src/lib.rs`, `crates/festerm-session/src/lib.rs`, `crates/festerm-sessiond/src/lib.rs`, +4 more.
