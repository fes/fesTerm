<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ee93752e7cd497dbd4700e9dd168e022fbd9ba60f02e12b746f7c38798cf912b"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns SSH host trust, GUI-backed SFTP connection setup, and the transfer queue that enforces safe remote writes and remote-file reads across the app and SSH crate.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## SSH trust gate

Use for: When an SSH host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns the worker thread, trust gates, and session lifecycle for SSH.
- [`HostTrustDecision`](../../crates/festerm-ssh/src/lib.rs) — User response enum for reject, accept once, and persist.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt arrives before password, matching ssh ordering.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key change stops recovery instead of continuing.

Caveats:
- Trust is keyed by canonical host and port, so mismatches are easy to miss.
- Pending prompts pause the worker without dropping the TCP session.

## GUI SFTP connect

Use for: When the app opens a GUI-backed SFTP session and must pause for trust while keeping the transport alive.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts a dedicated connect thread and emits trust prompts as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh SFTP client and working-directory state.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connected remote reads succeed with bounded, exact-byte results.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The GUI flow sees host-key trust before password prompts.

Caveats:
- The connect task uses a dedicated runtime so russh dispatch is not killed by a throw-away runtime.
- Dropping the completion cancels the connect attempt and tears down the owned thread.

## SFTP transfer queue

Use for: When remote browsing or copy operations need a bounded transfer queue, collision resolution, and event reporting.

- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, progress events, batch state, and the final commit.
- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local or remote path pairs before scheduling work.

Evidence:
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Batch-scoped collision decisions stay scoped to one batch.
- [`rooted_queue_parks_second_replacement_until_first_review_releases`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Replacement work is parked behind the single retained review.

Caveats:
- Uncertain temporary ownership preserves the partial and reports recovery details.
- Remote names and listings are untrusted; validation occurs before any write.

## App remote SFTP

Use for: When the GUI opens an SFTP tab, resolves trust prompts, and turns remote file reads into app pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds remote reads to verified endpoint identity and transport generation.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The app sees host-key trust before password prompts in ssh order.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
