<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "4db798335e11bc3e8b84c6ff6488fa05c9ade44d9fc54a14a46fc233b62140c2"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns SSH trust enforcement and the bounded GUI/SFTP transport flows that authenticate and stage remote file and transfer work.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When a host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`HostIdentity`](../../crates/festerm-ssh/src/lib.rs) — Canonical SSH destination identity used for trust decisions.
- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Worker thread owns transport, authentication, and trust gate.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt precedes password in the OpenSSH interop path.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key changes stop recovery instead of proceeding.

Caveats:
- Host trust is keyed by canonical host and port, so mismatches are easy to miss.
- Pending prompts pause the worker without dropping the TCP session.
- A host-key rejection blocks the current connection path instead of auto-recovering.

## gui sftp connect

Use for: When the app opens a GUI-backed SFTP session and must pause for trust while keeping the transport alive.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts a dedicated connect thread and yields trust prompts as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh SFTP client and working-directory state.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The GUI connect flow shows trust gating before password handling.

Caveats:
- The connect path keeps a dedicated runtime so russh dispatch survives the handshake.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- GUI SFTP rejects interactive auth and requires explicit credentials.

## sftp transfer queue

Use for: When bounded upload or download work needs queueing, collision handling, and publication certainty.

- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local/remote path pairs before scheduling work.
- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, events, batches, and commit logic.

Evidence:
- [`single_upload_lost_open_reply_reports_uncertainty_with_primary_error`](../../crates/festerm-ssh/src/sftp.rs) — Uncertain open replies preserve uncertainty instead of claiming success.
- [`collision_decisions_replace_skip_keep_both_and_apply_to_all_are_batch_scoped`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Batch-scoped collision decisions stay within one transfer batch.

Caveats:
- Uncertain output is preserved and reported without claiming ownership or authorizing deletion.
- Remote names and listings are untrusted; validation occurs before any write.
- The queue reserves a single replacement review admission at a time.

## app remote sftp

Use for: When GUI SFTP tabs read remote files or reopen a remote pane after trust resolves.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds a live remote read to a verified transport and endpoint identity.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — The app path resolves host trust before prompting for credentials.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
