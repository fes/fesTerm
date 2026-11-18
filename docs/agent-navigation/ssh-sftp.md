<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "55928d98fc7520b19d8658de1427e3e4fe5aa3951650c3b5f1ad72210ab3dc3f"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-trust gating, GUI SFTP session setup, and the app-facing transfer and remote-file pipeline.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gate

Use for: When SSH host fingerprints must be accepted or rejected before shell or SFTP work continues.

- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns the worker thread, lifecycle, and host-key/password gates.
- [`HostTrustDecision`](../../crates/festerm-ssh/src/lib.rs) — User response enum for reject, accept once, and persist.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt arrives before password, matching ssh ordering.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key change stops recovery instead of continuing.

Caveats:
- Trust is keyed by canonical host and port, so mismatches are easy to miss.
- Pending host-key prompts pause the worker without dropping the TCP session.
- GUI SFTP rejects interactive auth and requires explicit credentials.

## gui sftp connect

Use for: When the app opens a GUI-backed SFTP session and must pause for trust while keeping the transport alive.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts the dedicated connect thread and emits host-key decisions as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh-sftp client and local working-directory state.

Evidence:
- [`cancelling_gui_connect_wait_closes_the_dedicated_transport_thread`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Cancelled GUI connect must tear down the dedicated transport thread.
- [`text_sftp_refuses_overlong_prefixes_and_fragments_then_executes_the_next_command`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connection and command phases fail cleanly before shell work resumes.

Caveats:
- The connect task uses a dedicated runtime so russh dispatch is not killed by a throw-away runtime.
- Dropping the completion cancels the connect attempt and tears down the owned thread.
- Interactive auth is intentionally unsupported; GUI SFTP must supply explicit credentials.

## sftp transfer queue

Use for: When remote browsing or copy operations need a bounded transfer queue, conflict resolution, and event reporting.

- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, progress events, batch state, and final commit.
- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local/remote path pairs and direction before scheduling work.

Evidence:
- [`download_conflicts_keep_both_skip_replace_and_remember`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Collision handling is decided before data moves and remembered across retries.
- [`queue_bounds_history_and_closed_client`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Queue limits, retained history, and closed-client shutdown stay enforced.

Caveats:
- Temporary files are hidden and renamed only after successful commit; partials must be cleaned up.
- Remote names and listings are untrusted; validation occurs before any write.
- A closed client turns future operations into Disconnected instead of silently continuing.

## app remote sftp

Use for: When the GUI opens an SFTP tab, resolves trust prompts, and turns remote file reads into app pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Reads remote snapshots with verified endpoint identity and generation binding.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.
- [`docs/sftp-ui-design.md`](../../docs/sftp-ui-design.md) — Spec defines the two-pane SFTP flow, entry points, and transfer behavior.

Caveats:
- Remote reads are bound to a verified fingerprint and transport generation to avoid stale reads.
- The app keeps terminal and GUI SFTP lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
