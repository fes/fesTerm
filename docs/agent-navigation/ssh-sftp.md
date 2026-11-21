<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "5d82d1781af51a25655843b18967f22cd19412abceedbb17b3e728ca5e8eb515"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust enforcement, and GUI-backed SFTP transfer and remote-file reads.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When an SSH host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns the worker thread, trust gates, and session lifetime for SSH transport.
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
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The connect flow shows host-key trust before password prompts.

Caveats:
- The connect task uses a dedicated runtime so russh dispatch is not killed by a throw-away runtime.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- Interactive auth is intentionally unsupported; GUI SFTP must supply explicit credentials.

## sftp transfer queue

Use for: When a bounded copy queue needs collision decisions, retries, or publication certainty.

- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, events, batches, and commit logic.
- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local or remote path pairs before scheduling work.
- [`RemoteTransferEndpoint`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Composes rooted atomic publication with the planner and queue.

Evidence:
- [`single_upload_lost_open_reply_reports_uncertainty_with_primary_error`](../../crates/festerm-ssh/src/sftp.rs) — Lost OPEN reply preserves the primary error and reports uncertainty without deletion.
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Batch-scoped collision decisions stay scoped to one batch.
- [`rooted_queue_parks_second_replacement_until_first_review_releases`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Parks work behind the sole retained replacement review.

Caveats:
- Uncertain output is preserved and reported without claiming ownership or authorizing deletion.
- Unix cleanup requires a matching regular file in a freshly trusted parent; post-crash recovery remains open.
- Remote names and listings are untrusted; validation occurs before any write.

## app remote sftp

Use for: When GUI SFTP tabs read remote files or start a remote pane after trust resolution.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds remote reads to the verified endpoint identity and transport generation.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The app sees host-key trust before password prompts in the same order as ssh.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
