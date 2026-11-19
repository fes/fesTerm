<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6001f1c2a22539629597d4b3630dc18e2289a029a460a6889d723fc28b0fa324"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust enforcement, and the GUI-backed SFTP lifecycle for file transfers and remote reads.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When an SSH host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns the worker thread, trust gates, and session lifecycle.
- [`HostTrustDecision`](../../crates/festerm-ssh/src/lib.rs) — User response enum for reject, accept once, and persist.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt arrives before password, matching ssh ordering.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key change stops recovery instead of continuing.

Caveats:
- Trust is keyed by canonical host and port, so mismatches are easy to miss.
- Pending prompts pause the worker without dropping the TCP session.
- GUI SFTP rejects interactive auth and requires explicit credentials.

## gui sftp connect

Use for: When the app opens a GUI-backed SFTP session and must pause for trust while keeping the transport alive.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts the dedicated connect thread and emits trust prompts as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh SFTP client and working-directory state.

Evidence:
- [`cancelling_gui_connect_wait_closes_the_dedicated_transport_thread`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Cancelled GUI connect must tear down the dedicated transport thread.
- [`text_sftp_refuses_overlong_prefixes_and_fragments_then_executes_the_next_command`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connection and command phases fail before shell work resumes.

Caveats:
- The connect task uses a dedicated runtime so russh dispatch is not killed by a throw-away runtime.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- Interactive auth is intentionally unsupported; GUI SFTP must supply explicit credentials.

## sftp transfer queue

Use for: When remote browsing or copy operations need a bounded transfer queue, collision resolution, and event reporting.

- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, progress events, batch state, and final commit.
- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local and remote path pairs before scheduling work.

Evidence:
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Desktop engine decisions remain scoped to one batch, including later conflicts.
- [`skip_all_and_replace_all_leave_other_paused_batches_unresolved`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Skip/Replace resolve compatible paused conflicts without resolving another batch.

Caveats:
- Uncertain temporary ownership preserves the partial and reports recovery details; no suffix-based cleanup or verified-resume claim.
- Remote names and listings are untrusted; validation occurs before any write.
- A closed client turns future operations into Disconnected instead of silently continuing.

## app remote sftp

Use for: When the GUI opens an SFTP tab, resolves trust prompts, and turns remote file reads into app pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`show_collision_modal`](../../app/festerm/src/sftp_file_manager.rs) — Single-item choices and explicit Skip all/Replace all emit typed item/batch scope.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds remote reads to verified endpoint identity and transport generation.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`docs/sftp-ui-design.md`](../../docs/sftp-ui-design.md) — Defines the two-pane SFTP flow, entry points, and transfer behavior.
- [`collision_modal_actions_keep_explicit_item_and_batch_scopes`](../../app/festerm/src/sftp_file_manager.rs) — Actual buttons preserve scope and Local/Remote labels for both transfer directions.
- [`collision_modal_batch_actions_require_batch_and_allowed_decisions`](../../app/festerm/src/sftp_file_manager.rs) — Batch controls retain backend batch/type restrictions and ordinary Merge folders.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
