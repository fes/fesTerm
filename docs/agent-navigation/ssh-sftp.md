<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "codex-source-verified", "sources": "8e7926a12be224680b55f4b11427d6de61b8741ca652220e3c5af653628eaae5"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust enforcement, and GUI-backed SFTP transfer and remote-file reads.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When an SSH host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns the worker thread, trust gates, and session lifecycle for SSH.
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

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts a dedicated connect thread and emits trust prompts as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh SFTP client and working-directory state.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connected remote reads succeed with verified, bounded results.

Caveats:
- The connect task uses a dedicated runtime so russh dispatch is not killed by a throw-away runtime.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- Interactive auth is intentionally unsupported; GUI SFTP must supply explicit credentials.

## sftp transfer queue

Use for: When remote browsing or copy operations need a bounded transfer queue, collision resolution, and event reporting.

- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, progress events, batch state, and the final commit.
- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local or remote path pairs before scheduling work.
- [`RemoteTransferEndpoint`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Composes rooted atomic publication with the existing planner, queue and local staging.
- [`Creation`](../../crates/festerm-ssh/src/sftp_download_unix.rs) — Owns Unix creation/handoff cleanup with retained parent/file identity and ordinary permissions.

Evidence:
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Batch-scoped collision decisions stay scoped to one batch.
- [`rooted_queue_parks_second_replacement_until_first_review_releases`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Parks work behind the sole retained replacement review.
- [`windows_download_cleanup_preserves_replaced_leaf`](../../crates/festerm-ssh/src/sftp.rs) — Retained-handle cleanup preserves a replacement leaf on Windows.
- [`windows_download_retained_handle_prevents_ancestor_rename_until_cleanup`](../../crates/festerm-ssh/src/sftp.rs) — Windows refuses moving the open file’s ancestor; the same rename succeeds after cleanup.
- [`unix_download_lost_creation_handoff_waits_for_actual_cleanup`](../../crates/festerm-ssh/src/sftp_download_unix.rs) — Waits for actual abandoned-creation cleanup.
- [`unix_download_ancestor_replacement_cleans_retained_parent_only`](../../crates/festerm-ssh/src/sftp_download_unix.rs) — Cleans only the retained parent; refuses changed-path publication.

Caveats:
- Uncertain temporary ownership preserves the partial and reports recovery details.
- Unix cleanup requires a matching regular file in a freshly trusted parent; unprovable parents, remote output and crash/restart recovery remain open. POSIX unlink excludes hostile same-user/root concurrent writers.
- Remote names and listings are untrusted; validation occurs before any write.
- A closed client turns future operations into Disconnected instead of continuing silently.

## app remote sftp

Use for: When the GUI opens an SFTP tab, resolves trust prompts, and turns remote file reads into app pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds remote reads to verified endpoint identity and transport generation.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The app sees host-key trust before password prompts in the same order as ssh.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
