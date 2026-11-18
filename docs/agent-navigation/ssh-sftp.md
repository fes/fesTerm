<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "b6344936964cb9c1e33c2758d8d7c9a7f4c22161787e275238ba0d81452d3e1c"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust gating, and the GUI SFTP flow that backs remote file reads and queued transfers.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust gating

Use for: When a new SSH host fingerprint must be accepted or rejected before shell or SFTP work can continue.

- [`HostTrustDecision`](../../crates/festerm-ssh/src/lib.rs) — User response enum for final host-key accept or reject decisions.
- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns worker thread, lifecycle, and host-key/password decision gates.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt arrives before password, matching ssh ordering.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key change aborts recovery instead of silently continuing.

Caveats:
- Host trust is keyed by canonical host and port, so mismatches are easy to miss.
- A pending prompt pauses the worker without dropping the TCP session.
- Interactive auth is intentionally unsupported in GUI SFTP.

## gui sftp connect

Use for: When the app needs to open a GUI-backed SFTP session and pause on trust or keep the transport alive across connect.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts the dedicated GUI SFTP connect thread and pauses on host trust.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the russh-sftp client and keeps transport alive during GUI session use.

Evidence:
- [`cancelling_gui_connect_wait_closes_the_dedicated_transport_thread`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Cancelled connect tears down its dedicated transport thread.
- [`text_sftp_refuses_overlong_prefixes_and_fragments_then_executes_the_next_command`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connection and command phases fail cleanly before shell work resumes.

Caveats:
- The connect path keeps a dedicated runtime alive until the SFTP session is dropped.
- Dropping the completion cancels the in-flight connect attempt.
- GUI SFTP requires an explicit credential instead of interactive prompt flow.

## sftp transfer queue

Use for: When remote browsing or copy operations need a bounded transfer queue, conflict resolution, and event reporting.

- [`SftpTransferRequest`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Validates local/remote path pairs and direction before queueing.
- [`SftpTransferManager`](../../crates/festerm-ssh/src/sftp_transfer.rs) — Owns queued transfers, collision handling, event stream, and shutdown.

Evidence:
- [`download_conflicts_keep_both_skip_replace_and_remember`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Collision choices are resolved before data moves and remembered across retries.
- [`queue_bounds_history_and_closed_client`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Queue limits, retained history, and closed-client shutdown are enforced.

Caveats:
- Partial files are hidden and renamed only after successful commit.
- Remote names and listings are untrusted and validated before any write.
- Closed clients turn future operations into Disconnected instead of continuing.

## app remote reads

Use for: When the GUI opens an SFTP tab, resolves host-key prompts, and converts remote file actions into pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into the GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Binds remote reads to endpoint identity and transport generation.

Evidence:
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Reads return exact bytes and honest max-length errors.
- [`docs/sftp-ui-design.md`](../../docs/sftp-ui-design.md) — Spec defines the two-pane SFTP flow and transfer behavior.

Caveats:
- Remote reads are bound to verified endpoint identity and generation.
- A host-key prompt is resume-oriented; canceling it aborts the connect.
- The app keeps SFTP and terminal lifecycles separate even with the same profile.
