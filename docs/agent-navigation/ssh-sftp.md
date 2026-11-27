<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "54279b053747663847909b69d002d6317f707405198129a89728768aa79518f5"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust enforcement, and bounded GUI/SFTP transfer flows.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When a host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`HostIdentity`](../../crates/festerm-ssh/src/lib.rs) — canonical host/port identity used for trust decisions
- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — worker thread owns transport and trust gate

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — host-key prompt is delivered before password, matching ssh ordering
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — rejected key changes stop recovery instead of continuing

Caveats:
- Host trust is keyed by canonical host and port, so mismatches are easy to miss.
- Pending prompts pause the worker without dropping the TCP session.
- A host-key rejection blocks the current connection path instead of auto-recovering.

## gui sftp connect

Use for: When the app opens a GUI-backed SFTP session and must pause for trust while keeping the transport alive.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — starts a dedicated connect thread and yields trust prompts as needed
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — wraps the authenticated russh SFTP client and working-directory state

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — the GUI connect flow shows host-key trust before password handling
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — connected remote reads succeed with verified, bounded results

Caveats:
- The connect path keeps a dedicated runtime so russh dispatch survives the handshake.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- GUI SFTP rejects interactive auth and requires explicit credentials.

## sftp transfer queue

Use for: When bounded upload or download work needs queueing, collision handling, and publication certainty.

- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — validates local/remote path pairs before scheduling work
- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — owns queued transfers, events, batches, and commit logic

Evidence:
- [`single_upload_lost_open_reply_reports_uncertainty_with_primary_error`](../../crates/festerm-ssh/src/sftp.rs) — uncertain open replies preserve uncertainty instead of claiming success
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — batch-scoped decisions stay scoped to one batch
- [`rooted_queue_parks_second_replacement_until_first_review_releases`](../../crates/festerm-ssh/src/sftp_transfer/endpoint.rs) — the queue parks work behind the sole retained replacement review

Caveats:
- Uncertain output is preserved and reported without claiming ownership or authorizing deletion.
- Remote names and listings are untrusted; validation occurs before any write.
- The queue reserves a single replacement review admission at a time.

## app remote sftp

Use for: When GUI SFTP tabs read remote files or reopen a remote pane after trust resolves.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — bridges profile auth and host-key resolution into the GUI SFTP session
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — binds a live remote read to a verified transport and endpoint identity

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — remote reads surface exact bytes plus honest max-length boundaries
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — the app path resolves host-key trust before password prompt handling

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
