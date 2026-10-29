# ADR 0052: Session-Bound Local File OSC 8 Links

- **Status:** Proposed
- **Date:** 2026-10-29
- **Supersedes:** None; extends the web-only terminal hyperlink contract

## Context

Copilot CLI can emit real OSC 8 metadata for local document links, but fesTerm
discarded every non-web target. Making `file:` eligible for the existing OS
URL opener would allow untrusted terminal output to invoke file associations
and network/device namespaces. Local and remote files already have separate
application-owned document-opening policies.

## Decision

The core preserves bounded, normalized absolute `file://` metadata with empty
or localhost authority, alongside HTTP/HTTPS. This is passive metadata, not
permission to access a filesystem. The web-only normalizer and OS-opening
sink remain web-only; cell serialization and session snapshot schemas do not
change.

Ctrl/Cmd-click and context Open emit one application command bound to the
originating tab and lifecycle generation. Only a matching local session can
open a file URI, through fesTerm's internal document registry. SSH, SFTP and
serial file-link activation is disabled; a URI's localhost assertion cannot
override session origin. Changing source generation retires pending gestures,
menus and activation. Existing documents retain their unsaved buffers.

File URIs are ASCII with strict, single-pass UTF-8 percent decoding. Raw paths,
relative targets, foreign authorities, credentials/ports, query/fragment
semantics, controls/bidi overrides, ambiguous separators, parent/current
segments, Windows device names and alternate streams are refused. Previews
and Copy path use the decoded destination, with native drive separators on
Windows, never the OSC 8 label.

The file-opening path walks retained directories without following symlinks
or reparse points, opens a no-follow regular-file handle, and uses the existing
bounded document decoder. It never canonicalizes an untrusted pathname before
this walk. Windows rejects mapped network roots and derives the retained file
name from its handle, not a second pathname traversal. Existing platform
security crates continue owning native operations.

## Alternatives considered

- Add `file` to the external web allowlist: rejected; that grants OS-handler
  capability rather than internal document access.
- Reuse ordinary detected-path opening unchanged: rejected; file-link origin
  and no-follow requirements must hold through actual file opening.
- Guess a relative target's cwd or map SSH paths locally: rejected; terminal
  output cannot establish that authority.
- Add remote file OSC 8 activation now: deferred to explicit verified-session
  mapping and its own tests.

## Consequences

This is enabled by default for local sessions as requested. Ordinary clicks
still select/report input. Unsupported/binary/oversize files refuse honestly,
with Copy path still available; no shell, external application, or network
fetch is a fallback. Markdown uses the existing editor/preview surface.

Symlinks and junctions are deliberately unsupported even when their targets
are local. Users must link the actual path. Local PTYs can contain nested SSH
commands, so transport classification is not authentication of the producer:
activation and destination previews remain deliberate. OS-mounted filesystems
remain OS-owned; this is not a filesystem sandbox or a guarantee against
network-backed Unix mounts. An old session daemon's parser may still discard
file metadata in its recovery mirror; this does not hot-upgrade that parser.

No ownership, dependency direction, queue, browser or scripting boundary moves.
Native desktop, clipboard, accessibility and producer-specific usability still
require the TI-10 evidence; automated checks do not promote those gates.

## Validation impact

- **Invariants introduced or changed:** The core admits passive local file
  metadata; only a same-generation local session may authorize an internal,
  no-follow document open. The OS URL sink remains HTTP/HTTPS-only.
- **GUI/action edges affected:** `TERM-05`, `TERM-08`, `TERM-13`.
- **Automated tests required:**
  `file_hyperlinks_decode_local_absolute_paths_without_widening_web_policy`,
  `file_hyperlinks_reject_ambiguous_network_device_and_encoded_targets`,
  `file_link_gestures_preview_and_copy_use_the_decoded_destination`,
  `changing_file_link_generation_retires_press_menu_and_pending_activation`,
  `terminal_file_links_open_local_documents_and_preserve_unsaved_edits`,
  `terminal_file_link_opens_from_production_view_routing`,
  `terminal_file_links_refuse_remote_serial_stale_closed_and_unsafe_targets`,
  `terminal_file_link_load_is_bounded_and_rejects_non_files`,
  `terminal_file_link_load_never_follows_file_or_parent_symlinks`,
  `terminal_file_link_load_refuses_junction_redirection`.
- **Native/manual evidence required:** TI-10 on Windows, macOS and Linux,
  including Copilot-produced file links, modifier/context gestures and
  native path preview/clipboard readability; TI-17 frozen-link interaction.
- **Coverage superseded:** The parser's blanket non-web rejection becomes
  local-file admission plus independent file authorization; the existing
  application's web-only policy tests remain unchanged.
