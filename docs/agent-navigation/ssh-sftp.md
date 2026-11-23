<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "codex-source-verified", "sources": "c4d2ae45d60a4829d5639082a63138bbc819ebf2323f917ab5dcda616eaf4fe6"} -->
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
- [`RemoteTransferEndpoint`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs)

Evidence:
- [`single_upload_lost_open_reply_reports_uncertainty_with_primary_error`](../../crates/festerm-ssh/src/sftp.rs)
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Batch-scoped collision decisions stay scoped to one batch.
- [`rooted_queue_parks_second_replacement_until_first_review_releases`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Parks work behind the sole retained replacement review.
- [`interrupted_copy_preserves_failed_handle_cleanup`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — Keeps uncertain handle cleanup ahead of the progress callback’s cancellation classification.
- [`files_transfer_stays_live_while_browsing_beyond_server_idle_deadline`](../../app/festerm/src/host_files.rs) — Actual paired servers retain the first transfer after idle browsing.

Caveats:
- Uncertain output is preserved and reported without claiming ownership or authorizing deletion.
- GUI reports survive restart without granting remote cleanup or resume; text SFTP keeps live-only reporting.
- Remote names and listings are untrusted; validation occurs before any write.

## app remote sftp

Use for: When GUI SFTP tabs read remote files or start a remote pane after trust resolution.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs)

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs)

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.

## Interrupted upload reports

Use for: Upload uncertainty after exit.

- [`UploadRecoverySink`](../../crates/festerm-ssh/src/sftp_upload_recovery.rs) — Pre-OPEN/publication reporting.
- [`Ledger`](../../app/festerm/src/sftp_recovery.rs) — Private report store.
- [Inspection](../../app/festerm/src/sftp_file_manager/upload_inspection.rs)

Evidence:
- [`durable_upload_report_survives_killed_owner_and_refuses_other_process_ack`](../../app/festerm/src/sftp_recovery/store.rs) — Crash retains reports.

Caveats:
- Reports grant no deletion/resume authority.
