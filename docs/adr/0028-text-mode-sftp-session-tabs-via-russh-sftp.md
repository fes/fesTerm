# ADR 0028: Text-Mode SFTP Session Tabs via `russh-sftp`

- **Status:** Proposed
- **Date:** 2026-09-03
- **Supersedes:** None

## Context

`ROADMAP.md` intentionally left SFTP outside the first native SSH milestone.
That was the right call: SFTP is not terminal emulation, and it is not just a
different shell command. It is a separate SSH subsystem with its own binary
protocol, transfer semantics, local-path policy, transcript UX, and validation
surface.

The repository already has two architectural constraints that should shape any
SFTP design.

First, `festerm-ssh` already owns the authenticated SSH transport. The current
shell path opens a raw session channel with `handle.channel_open_session()`,
requests a PTY, and then requests a shell or provider-specific exec command.
That makes `festerm-ssh` the correct place to open a different authenticated
session channel for the `"sftp"` subsystem as well. Reaching around it from
`app/festerm` would duplicate host-trust, authentication, reconnect, and
bounded event handling that already exist.

Second, `app/festerm` already has a reusable live-session surface. A
`SessionTab` owns one `Terminal`, one `SessionController<ApplicationSession>`,
one `TerminalView`, stable tab identity, search state, launch metadata, and
status-bar/chip integration. Workspace restore is deliberately metadata only:
`WorkspaceTab::SshSession` restores as
`TabContent::SshAuthenticationRequired(SshAuthenticationRequiredTab)` rather
than a live connection, requiring fresh credentials on restore. Any SFTP tab
should reuse those same surface rules instead of inventing a second transcript
framework with different lifecycle semantics.

The configuration layer provides the third constraint. `InterfaceSettings`
already uses additive `#[serde(default)]` fields plus `with_*` validated
replacement helpers, and existing path-like metadata such as
`LocalProfileConfiguration::working_directory` is validated for non-empty,
control-character-free strings without filesystem existence checks during parse
time. A user-facing "default local directory for SFTP" preference should follow
that same pattern because it is a client-side UI preference, not transport
identity or remote authority.

## Decision

### Use `russh-sftp`, not a hand-rolled SSH_FXP client

fesTerm will adopt the `russh-sftp` crate for SFTP client operations. The SFTP
channel is opened inside `festerm-ssh` on a raw authenticated SSH session
channel that requests the `"sftp"` subsystem, mirroring the pairing already
used in `russh`'s own examples: `russh` owns the transport/channel, and
`russh-sftp` speaks the subsystem protocol on top of it.

This is the preferred boundary because it reuses the repository's selected SSH
stack (`russh = 0.62.5` with `default-features = false` and `features =
["ring"]`) and avoids reimplementing a mature, well-specified binary protocol
whose correctness matters for file integrity and error handling.

### SFTP is a first-class session kind, but it reuses the existing text surface

An SFTP connection becomes a new live session kind, for example
`ApplicationSession::Sftp`, alongside today's `Local`, `Persistent`, `Ssh`, and
`Serial` variants. It reuses the existing `SessionTab` surface:

- command transcript and responses render through the same `Terminal`;
- scrolling, selection, Copy, Find, and scrollback limits remain the same
  `SessionTab`/`TerminalView` machinery;
- chips, inspector, and status bar keep the existing remote-session chrome
  model rather than introducing a new document-viewer frame; and
- bounded session events still flow through `SessionController`.

The user experience is therefore "a terminal-shaped transcript for SFTP
commands," not "an embedded file browser."

### Launch and restore reuse SSH identity and auth policy

SFTP sessions use the same SSH destination/profile identity and trust/auth
policy as shell SSH sessions. A saved SSH profile is therefore sufficient
connection metadata for either a shell or an SFTP session; SFTP does not create
its own second host-profile type.

Workspace restoration for saved SFTP tabs follows the exact behavioral rule
already used for SSH tabs today: the workspace stores only metadata, never a
live authenticated channel, and restore produces an authentication-required
surface rather than a resumed connection. In concrete terms, the implementation
should add a metadata-only `WorkspaceTab::SftpSession(SessionTabConfiguration)`
that restores as a dedicated auth-required tab (for example
`TabContent::SftpAuthenticationRequired(...)`) analogous to today's
`WorkspaceTab::SshSession` →
`TabContent::SshAuthenticationRequired(SshAuthenticationRequiredTab)` path.

Restoring a saved SFTP tab therefore means "recreate the tab and ask for fresh
credentials/trust resolution if needed," not "resume an old transfer or an old
subsystem channel."

### Initial command set is intentionally small and explicit

The first-pass text-mode client supports exactly these commands:

- `help`
- `pwd`
- `lpwd`
- `cd`
- `lcd`
- `ls`
- `mkdir`
- `rmdir`
- `rm`
- `rename`
- `chmod`
- `get`
- `put`
- `quit`
- `exit`

`quit` and `exit` are synonyms that close the live SFTP session cleanly and
leave the transcript in the ordinary read-only post-session state.

This first pass explicitly does **not** support:

- `reget` / `reput`
- `symlink`
- `chown`
- shell escapes such as `!`
- recursive `-r` upload/download
- globbing / wildcard expansion

Those are future extensions, not silently omitted behavior.

### A new app-level default local-directory setting defines starting `lpwd`

`InterfaceSettings` gains an additive SFTP client preference for the default
local directory used when a new SFTP session starts. To preserve backward
compatibility with existing configuration documents, the field should deserialize
with `#[serde(default)]`; older documents therefore remain valid under schema
version `1`.

The field is client-side UI state, not profile data:

- it is stored in `InterfaceSettings`, not in `SshProfileConfiguration`;
- it is validated like other stored path-like strings: non-empty and
  control-character-free when present, but not existence-checked at parse time;
  and
- an `lcd` command changes only the live session's current local directory. It
  never rewrites the saved default.

An unset legacy/default value may resolve at runtime to a platform-appropriate
starting directory such as the user's home directory, but the persisted setting
remains the authoritative user preference once explicitly chosen.

### Overwrite policy is refusal by default

For the first pass, `get` and `put` refuse to overwrite an existing destination
file. The user must rename or remove the destination explicitly before
retrying.

This is the most truthful and lowest-risk starting policy:

- it avoids accidental local data loss from a mistyped `get`;
- it avoids accidental remote data loss from a mistyped `put`; and
- it keeps the command grammar small while the transcript UX is still being
  introduced.

If a future revision wants explicit overwrite flags or prompts, that should be
reviewed as an additive UX decision. Silent overwrite is not acceptable as the
baseline.

### Path resolution must be explicit and diagnostics must stay content-free

Local and remote paths are resolved against their current working-directory
context (`lpwd`/`pwd`) when the user supplies a relative path. When a command
omits a destination, fesTerm derives it only from the source basename and the
current local or remote directory; it must not silently write somewhere
unexpected.

The implementation must normalize and validate destination paths before any
write so traversal-like inputs are either resolved within the explicit target
context or rejected with a concise error. In particular, fesTerm must not
silently let an omitted or relative destination escape the current local target
directory just because the remote file name happened to contain awkward path
segments.

Diagnostics remain content-free:

- never log file contents or transfer payloads;
- never duplicate transferred bytes into persistent diagnostics;
- emit only sanitized operation summaries such as command kind, success/failure,
  and bounded error detail; and
- keep secret material out of SFTP transcript plumbing exactly as the SSH shell
  path already keeps credentials out of logs and configuration.

## Alternatives considered

### Hand-roll the SSH_FXP protocol inside `festerm-ssh`

Rejected. `russh-sftp` already exists specifically to pair with `russh`, and
the repository gains little by reimplementing packet framing, request/response
correlation, attribute encoding, and transfer corner cases itself. The risk of
an incomplete or subtly incorrect homegrown SFTP client is not justified when a
maintained, stack-compatible crate exists.

### Ship a fully recursive, glob-capable client in v1

Rejected as premature scope. Recursive copy, wildcard expansion, resume, and
ownership/symlink manipulation all widen the command grammar, error states, and
validation burden. A smaller first pass is easier to validate for file-content
correctness and easier to explain honestly in the UI.

## Consequences

- `festerm-ssh` gains a second authenticated channel mode beside PTY shell
  sessions: raw subsystem-backed SFTP.
- The application gets a new remote session kind, but does not get a separate
  text rendering stack; the existing transcript/search/status machinery is
  reused.
- Workspace restore remains metadata only for SFTP just as it already is for
  SSH shell tabs, so there is no false promise of resumed live transfers.
- The settings surface gains one new persisted client preference for the local
  SFTP starting directory.
- Users get a predictable, explicit overwrite policy and a clear unsupported
  command boundary instead of "maybe supported" behavior.

## Validation impact

- **Invariants introduced or changed:** SFTP uses `russh-sftp` over a raw SSH
  `"sftp"` subsystem channel; SFTP tabs reuse `SessionTab` transcript/search/
  status machinery; workspace restore recreates an auth-required SFTP surface
  instead of resuming a live channel; `get`/`put` refuse overwrite by default;
  diagnostics never include transferred file content.
- **GUI/action edges affected:** Planned new edges `LAUNCH-09` (start a new
  SFTP session from SSH destination/profile metadata), `SSH-09` (interact with
  a live text-mode SFTP tab and run a supported command successfully), and
  `SET-09` (change the default local SFTP directory preference and verify it
  persists without `lcd` mutating it).
- **Automated tests required:** Planned coverage includes
  `put_uploads_bytes_identical_to_local_source`,
  `get_downloads_bytes_identical_to_remote_source`,
  `sftp_get_refuses_to_overwrite_an_existing_local_file`,
  `sftp_put_refuses_to_overwrite_an_existing_remote_file`,
  `sftp_workspace_restore_requires_fresh_authentication`,
  `interface_settings_parse_default_sftp_local_directory_additively`, and
  `lcd_changes_only_the_live_session_local_directory`.
- **Native/manual evidence required:** Manual fixture evidence is required for
  interactive command usability, local-directory defaulting, overwrite refusal,
  and transcript clarity during representative transfers. Stable scenario IDs
  should be added to `docs/manual-validation.md` in the implementing change.
- **Coverage superseded:** None yet. `validation/traceability.json` must be
  updated in the implementing change that adds the real SFTP edges, tests, and
  manual scenarios.
