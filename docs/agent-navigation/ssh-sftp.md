<!-- agent-navigation: {"domain": "ssh-sftp", "generator": "2", "model": "mai-code-1.1-flash", "sources": "ecb6e1070df163ed8599f725c7292360c0446b993493d330ef0beaa3367eb2c3"} -->
# SSH transport, trust, and SFTP

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

This domain owns authenticated SSH transport, host-key trust gating, and the GUI/text SFTP transfer stack used by remote file and session flows.

Canonical docs: [docs/sftp-ui-design.md](../../docs/sftp-ui-design.md)

## ssh trust

Use for: When opening or reconnecting an SSH session and a new host fingerprint must be accepted or rejected before shell or SFTP work continues.

- [`HostTrustDecision`](../../crates/festerm-ssh/src/lib.rs) — User response enum for reject, accept once, and persist.
- [`SshSession`](../../crates/festerm-ssh/src/lib.rs) — Owns worker thread, lifecycle, and host-key/password decision gates.

Evidence:
- [`controlled_openssh_interactive_password_interoperability`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Host-key prompt arrives before password, matching ssh ordering.
- [`controlled_openssh_automatic_recovery_stops_when_host_key_change_is_rejected`](../../crates/festerm-ssh/tests/openssh_interop.rs) — Rejected key change stops recovery instead of silently continuing.

Caveats:
- Host trust is keyed by canonical host plus port identity, so mismatches are easy to miss.
- A pending prompt pauses the worker without dropping the TCP session.
- Interactive authentication is intentionally unsupported in GUI SFTP.

## gui sftp boot

Use for: When the app opens a GUI-backed SFTP session and needs to pause on trust or keep the transport alive across the connect handshake.

- [`connect_gui_sftp_session`](../../crates/festerm-ssh/src/lib.rs) — Starts a dedicated connect thread and emits host-key decisions as needed.
- [`SftpSession`](../../crates/festerm-ssh/src/sftp.rs) — Wraps the authenticated russh-sftp session and local working directory state.

Evidence:
- [`cancelling_gui_connect_wait_closes_the_dedicated_transport_thread`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Cancelled GUI connect must tear down the dedicated transport thread.
- [`text_sftp_refuses_overlong_prefixes_and_fragments_then_executes_the_next_command`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Connection and command phases fail cleanly before shell work resumes.

Caveats:
- The connect task uses a dedicated runtime to avoid russh dispatch being killed by a throw-away runtime.
- The keepalive must end when the session is dropped or the channel stays alive unexpectedly.
- Interactive auth is rejected here; GUI SFTP must supply an explicit credential.

## sftp client queue

Use for: When remote browsing or copy operations need a bounded SFTP client, queue, conflict resolution, and event reporting.

- [`SftpClient`](../../crates/festerm-sftp/src/remote.rs) — Wraps a russh-sftp connection with private runtime and bounded pane calls.
- [`TransferManager`](../../crates/festerm-sftp/src/transfer.rs) — Owns queued transfers, progress events, conflict state, and final commit.

Evidence:
- [`download_conflicts_keep_both_skip_replace_and_remember`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Collision handling is decided before data moves and remembered across retries.
- [`queue_bounds_history_and_closed_client`](../../crates/festerm-sftp/tests/sftp_integration.rs) — Queue limits, retained history, and closed-client shutdown are enforced.

Caveats:
- Temporary files are hidden and renamed only after successful commit; partials must be cleaned up.
- Remote names and listings are untrusted; validation occurs before any write.
- A closed client turns future operations into Disconnected instead of silently continuing.

## app file manager

Use for: When the GUI opens an SFTP tab, resolves host-key prompts, and converts remote filesystem actions into local and remote pane operations.

- [`connect_remote_session`](../../app/festerm/src/sftp_file_manager.rs) — Bridges profile auth and host-key resolution into a GUI SFTP session.
- [`RemoteFileCapability`](../../app/festerm/src/remote_files.rs) — Reads remote file snapshots with endpoint identity and generation binding.

Evidence:
- [`docs/sftp-ui-design.md`](../../docs/sftp-ui-design.md) — Spec defines the two-pane SFTP flow, entry points, and transfer behavior.
- [`live_remote_file_read_returns_exact_bytes_and_honest_bounds`](../../crates/festerm-ssh/tests/live_remote_files.rs) — Remote reads surface exact bytes plus honest max-length boundaries.

Caveats:
- The app keeps SFTP and terminal lifecycles separate even when they share a profile.
- A host-key prompt is resume-oriented; dropping the completion cancels the connect attempt.
- Remote reads are bound to verified endpoint identity and generation to avoid stale reads.
