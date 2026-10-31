# ADR 0054: Platform-Neutral SFTP Core Crate for Mobile

- **Status:** Accepted
- **Date:** 2026-10-01
- **Supersedes:** None. Related: ADR 0013, ADR 0018, ADR 0031, ADR 0042, ADR 0047

## Context

The iOS host (ADR 0042) shows Files/SFTP only as synthetic previews. A real
implementation needs bounded, cancellable listing and transfers, the
Keep both / Skip / Replace conflict flow and a "This iPhone" sandbox pane.
Desktop SFTP lives in `festerm-ssh` text-mode code and app-level remote-file
code that depends on desktop types. The mobile graph must stay free of
desktop-only crates (ADR 0042 audit).

## Decision

Add `crates/festerm-sftp`, a UI-free crate depending only on `russh-sftp`
(already vendored and pinned), `tokio` and `getrandom`. It does not depend on
`festerm-ssh`, `festerm-session` or any UI crate. `festerm-ssh` gains
`SftpSession::into_transport()` returning the raw `russh-sftp` session plus an
opaque keep-alive; the host passes both to `SftpClient::from_russh`. Dependency
direction: host → {festerm-ssh, festerm-sftp}; festerm-sftp → russh-sftp.

The crate owns a private tokio runtime so callers are async-free. Policy:

- Listing is bounded (default scan 20k, result 5k, absolute scan 100k) and
  cancellable; remote symlink resolution is capped per listing.
- Names reject empty, `.`/`..`, NUL, separators, control and bidirectional or
  invisible characters, and >255 bytes. Unsafe remote names are listed with a
  sanitized display name, flagged `name_safe=false`, and never become local
  names unless the UI supplies a safe name.
- Local paths are confined to the sandbox root. Local symlinks are followed
  only while the target stays inside the root, otherwise refused. Mutations
  act on the link itself; recursive delete never follows links and is bounded.
  Remote symlinks to files transfer as contents; to directories are not
  transferred.
- Transfers write to an exclusive `.festerm-<random>.part` temp and commit by
  atomic rename (no-clobber hard link/rename unless Replace). Failure or
  cancel removes the temp; a final file is never partial. Transfers are not
  resumable and not recursive (documented limitations).
- Conflicts compare size/mtime and ask per transfer; the user's choice can be
  remembered for the remaining queue. Queue, concurrency, chunk, window and
  file-size limits are fixed (256 queued, 4 concurrent, 8 GiB).

A real user-level OpenSSH server harness (`scripts/run-sftp-integration.sh`)
exercises the crate; its key is SFTP-only, loopback-only and lives under /tmp.
`scripts/build-ios-spike.py --check-dependencies` also audits `festerm-sftp`
for both iOS targets.

## Alternatives considered

- Reuse `festerm-ssh`'s text-mode SFTP: coupled to desktop UX and unbounded
  listing; rejected.
- Put the logic in `app/festerm-mobile`: untestable without UI and not reusable.
- Use russh-sftp `File`/`read_dir`: loses error structure and is unbounded.

## Consequences

Mobile can wire real panes without new dependencies. Cancellation and
disconnect are tested against real sshd. Resume and recursive transfer remain
future work. Physical devices need a LAN-reachable server.

## Validation impact

- **Invariants introduced or changed:** bounded listing/queue; no final file
  unless complete; no write outside the sandbox root; platform-neutral graph.
- **GUI/action edges affected:** None; no UI is changed (`MOB-*` wiring later).
- **Automated tests required:** `festerm-sftp` unit tests and the
  `sftp_integration` suite run by `scripts/run-sftp-integration.sh`.
- **Native/manual evidence required:** Simulator wiring against the harness
  when the UI lands.
- **Coverage superseded:** None.
