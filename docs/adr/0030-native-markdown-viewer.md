# ADR 0030: Native Markdown Viewer as a First-Class Bounded Application Surface

- **Status:** Proposed
- **Date:** 2026-09-03
- **Supersedes:** None

## Context

`docs/markdown-viewer-design.md` approves a native, read-only Markdown viewer
with local and SFTP entry routes, Preview/Source modes, Find, bounded resource
loading, and explicit remote-origin pinning. That scope crosses several stable
repository boundaries and therefore needs an ADR rather than an implementation-
local choice.

First, the application already distinguishes between session-backed tabs and
non-terminal application surfaces. `ApplicationSession` owns only live local,
persistent-local, SSH, SFTP, and serial transports; `TabContent` separately
owns Launcher, Settings, Profiles, and auth-required non-session surfaces.
ADR 0014 makes that ownership distinction explicit: tabs may outlive a
transport attempt, while application surfaces have no terminal/session pair at
all. The Markdown viewer must fit that model instead of smuggling a document
surface through the terminal/session path.

Second, the configuration/workspace layer is intentionally additive and
metadata-only. `festerm-config` keeps `SCHEMA_VERSION = 1`, extends
`InterfaceSettings` via `#[serde(default)]` fields and validated replacement
helpers, and persists only stable workspace metadata through `WorkspaceTab`.
At the same time, the approved Markdown-viewer product spec explicitly says
viewer documents, paths, contents, scroll positions, and Find queries are not
persisted in workspace state in v1. The design therefore needs a clear answer
about which parts of the feature become configuration/workspace schema and
which stay runtime-only.

Third, the repository already has a reusable remote-file boundary for SFTP.
ADR 0028 selected `russh-sftp` and the existing `festerm-ssh::sftp::SftpSession`
backend. Live SFTP tabs already carry typed `HostIdentity`-based connection
metadata, optional persisted known-host fingerprints, and a session-controller
lifecycle generation that distinguishes one live transport episode from the
next. The Markdown viewer should reuse that existing authenticated SFTP path
for remote snapshots rather than inventing a second SSH file-read mechanism.

Finally, bounded behavior is a repository-wide invariant, not an isolated UI
preference. `festerm-session` caps one I/O chunk at `MAX_IO_CHUNK_BYTES = 64
KiB`, and ADR 0017 established the project's general memory/diagnostics
philosophy: explicit limits, powers-of-two sizing, content-free errors, and
truthful recovery semantics. The Markdown viewer needs concrete bounds for
input bytes, decoded lines, nesting, tables, code blocks, and explicit image
loads that follow that same philosophy.

## Decision

### The Markdown viewer is a runtime `TabContent` surface, not an `ApplicationSession`

fesTerm will implement the Markdown viewer as a dedicated non-terminal tab
surface, for example `TabContent::MarkdownViewer(MarkdownViewerTab)`, owned
entirely by the application/UI layer.

It will **not** be represented as an `ApplicationSession` variant:

- a viewer has no `Session`, PTY, SSH channel, transcript, or terminal grid;
- it should not participate in terminal input, resize, scrollback, or session
  shutdown semantics; and
- its lifetime matches Launcher/Settings-style application surfaces more than
  SFTP/SSH session tabs.

Likewise, v1 will **not** add `WorkspaceTab::MarkdownViewer`. The approved
product spec explicitly excludes persistence of open viewer documents and their
state, so workspace capture/restoration should skip Markdown viewer tabs just
as it currently skips other runtime-only state. If a future product decision
chooses viewer persistence, that can be an additive later change with its own
`WorkspaceTab::MarkdownViewer(MarkdownViewerTabConfiguration)` metadata shape.

This means the integration pattern is:

- **`TabContent`:** gains a first-class Markdown-viewer variant.
- **`ApplicationSession`:** no new variant.
- **`WorkspaceTab`:** no new v1 variant; workspace save/restore omits viewer
  tabs by design.

### Add a small parser/model crate and keep rendering egui-native

fesTerm will add a small workspace crate dedicated to bounded Markdown loading
and projection, e.g. `crates/festerm-markdown`. That crate owns:

- typed source identities (`Local` and `Remote`);
- bounded UTF-8 decoding and limit enforcement;
- Markdown parsing into a deterministic document model for Preview;
- source offsets/heading metadata needed by Find, Outline, and Preview/Source
  scroll mapping; and
- content-free parse/load errors.

`app/festerm` remains responsible for chip lifecycle, commands, focus,
keybindings, and egui rendering.

Rendering remains fully egui-native. The implementation should use existing
`egui`/`eframe` primitives already present in the workspace — `ScrollArea`,
text layout, read-only multiline text editing or selectable labels, `Grid`,
and targeted custom painting where needed — rather than embedding a webview,
HTML renderer, or a second UI framework. Wide tables and code blocks may use
nested bounded scroll regions inside the main reading column; this is
consistent with the approved mockup and with the application's current bounded
scroll-surface patterns.

### Use `pulldown-cmark` for Markdown parsing and `syntect` for bounded syntax highlighting

fesTerm will parse Markdown with `pulldown-cmark`.

This is the preferred parser because it matches the required contract with the
smallest reasonable new dependency surface:

- it is the conventional Rust choice for CommonMark parsing;
- it supports the required GFM-style extensions needed here (tables, task
  lists, strikethrough, autolinks, fenced code);
- its event-stream model fits fesTerm's bounded, non-HTML, non-browser design
  better than an HTML-first renderer; and
- it keeps the viewer's parser boundary small and testable inside a repository
  that already favors focused crates over large framework integrations.

fesTerm will not adopt an HTML-rendering Markdown stack such as a browser
surface or an egui wrapper around HTML output. Raw HTML in Markdown remains
inert source or a visible placeholder exactly as the approved product spec
requires.

For syntax highlighting, fesTerm will add `syntect` behind a small adapter in
`festerm-markdown` and will document a bounded language allowlist for v1.
Unknown or unsupported language labels render as plain monospace code. `syntect`
is chosen despite being a larger dependency than the parser because it gives a
fully local, deterministic, bundled highlighter with mature syntax definitions
and no web/runtime dependency. That is a better fit than introducing a browser
engine or a tree-sitter-based highlighting stack solely for this feature.

The initial documented highlight set should stay intentionally small and useful,
for example: `text`, `md`, `rust`, `toml`, `json`, `yaml`, `shell`/`bash`/`sh`,
`diff`, and `powershell`.

### Remote snapshots must reuse the existing SFTP backend and typed SSH identity

Remote Markdown loading will reuse the authenticated SFTP stack already chosen
by ADR 0028.

Concretely:

- a remote viewer may only open from an explicit SFTP-origin action;
- the snapshot read must flow through the live `ApplicationSession::Sftp`
  transport path, using the existing `festerm-ssh::sftp::SftpSession`
  machinery and the same `russh-sftp` file-open/read loop already used for SFTP
  transfers;
- the viewer must not translate a remote path into a local path, mount a
  remote filesystem, or open a second ad hoc SSH connection just to read a
  file.

The viewer's remote source identity should be captured as a typed value derived
from existing session concepts, not display strings. At minimum it should bind:

- the remote `HostIdentity` (`host` + `port`);
- the SSH username or saved profile identifier that launched the SFTP session;
- the canonical remote path returned by the SFTP layer; and
- the producing SFTP tab's `SessionController::lifecycle_generation()`.

That generation value prevents a live viewer from silently treating a later,
different transport episode as the same still-verified source. While a session
remains connected, reloads and explicit remote resource fetches must require
that same generation.

To satisfy the product requirement of reloading only after reconnecting to the
**same verified origin**, the implementation should also expose the verified
host-key fingerprint used by the live SFTP session (or an equivalent typed
trust identity) as sanitized session metadata. Existing known-host plumbing
already carries a persisted expected fingerprint into SFTP startup; the viewer
should build on that seam rather than trusting a chip label or display string.
A disconnected viewer may continue to display its last good snapshot, but it may
reload only after the application has re-established an SFTP transport whose
host identity and verified trust identity match the source the snapshot was
pinned to.

### Preview, Source, Outline, and Find are in-memory snapshot features

Each viewer tab owns one immutable loaded snapshot plus small transient UI
state:

- active mode (`Preview` or `Source`);
- outline-open state and selected/current heading;
- scroll anchor;
- Find query plus match list/current match; and
- per-resource approval state for the current tab lifetime only.

Preview is rendered from the parsed bounded document model. Source renders the
exact decoded UTF-8 text (after optional BOM stripping) from the same snapshot;
there is no second parser or re-read path for Source mode.

Find works over the decoded in-memory text only. The parser/model crate should
retain source offsets or line/byte spans so Find can:

- report `N of M` deterministically;
- move next/previous with wrap;
- preserve the current match across a same-source reload when the match still
  exists; and
- map Preview matches back to headings/blocks without reparsing for every
  keystroke.

Preview/Source mode switches preserve the nearest heading and relative position
within that heading's section, not a raw widget scroll offset from unrelated
layouts.

**Keybinding conflict resolution:** the approved product spec proposes
`Ctrl/Cmd+Shift+M` for the Preview/Source toggle, but that chord is already
bound to `ApplicationShortcut::PortForwardManager` (`app/festerm/src/app.rs`
lines 216, and macOS variant at 215). fesTerm will bind the Markdown viewer's
Preview/Source toggle to **`Ctrl/Cmd+Shift+V`** instead, which is unused by any
existing `ApplicationShortcut`. The command palette remains authoritative for
discoverability regardless of the bound chord, consistent with the spec's own
"the command palette is authoritative" language.

### Explicit, non-persisted resource approval only

The viewer never performs implicit secondary loads.

For each image-like resource reference, Preview renders a placeholder with its
alt text and source class. Explicit per-item activation may then attempt a
bounded raster load subject to source-type policy:

- **Local document:** canonicalize the document path, resolve the relative
  target against it, require a regular local file, reject disallowed schemes,
  reject SVG and non-raster formats, and enforce resource limits before decode.
- **Remote document:** resolve the relative target against the viewer's typed
  remote source, then fetch it only through the same verified SFTP
  origin/generation described above.

Approvals are stored only in the live `MarkdownViewerTab` state, keyed by a
stable per-resource identity, and are cleared on close, on source-identity
change, and after a successful reload that produces a new snapshot. They are
never persisted in configuration, workspace state, recent history, or trust
stores.

### Bounded-loading limits

The first implementation will ship with these concrete limits:

- **Maximum Markdown source size:** **4 MiB** per snapshot (`64 ×
  MAX_IO_CHUNK_BYTES`).
- **Maximum decoded line count:** **65,536** lines.
- **Maximum block/list nesting depth:** **16**.
- **Maximum rendered table cells:** **8,192** cells across one document.
- **Maximum fenced code block payload:** **256 KiB** for any single block.
- **Maximum explicit image/resource payload:** **8 MiB** compressed input per
  item.

In addition, image decoding should enforce a raster-area cap (for example,
16 megapixels) so a highly compressed image cannot expand into an unreasonable
texture allocation after passing the byte-size limit.

These numbers follow the same repository conventions already used elsewhere:
explicit powers-of-two, limits materially below the much larger 64 MiB
terminal-scrollback default, and crisp content-free failure categories rather
than best-effort unbounded parsing.

A source that exceeds any viewer-specific limit fails closed with a concise,
content-free error. A partial parse or partial remote read must never replace a
previously valid snapshot.

## Alternatives considered

- **Use `comrak` or another larger AST/HTML-oriented Markdown stack.**
  Rejected. The product contract is not "render arbitrary HTML"; it is a small,
  bounded CommonMark/GFM subset rendered through native widgets. `pulldown-
  cmark` is the smaller and more direct fit.
- **Use a webview/browser surface to get Markdown rendering "for free."**
  Rejected. It would violate the product's explicit non-browser posture,
  reintroduce HTML/script/resource trust problems the spec forbids, and add a
  second rendering/runtime stack to an application that already committed to
  egui in ADR 0007.
- **Represent the viewer as a session or transcript tab.**
  Rejected. A viewer owns no terminal, no byte-stream session, and no transport
  lifecycle of its own. Modeling it as an `ApplicationSession` would blur an
  ownership boundary ADR 0014 deliberately keeps sharp.
- **Persist Markdown viewers in workspace state immediately.**
  Rejected for v1. The approved product spec explicitly says viewer documents
  and state are not persisted. Persisting them now would be a product change,
  not an implementation detail.

## Consequences

- The workspace gains one new focused crate boundary for bounded document
  loading/projection, but not a new UI framework.
- `app/festerm` gains a first-class runtime document surface in the chip row,
  with new typed commands for opening, focusing, closing, reloading, toggling
  Preview/Source, Find, outline navigation, and explicit resource loads.
- `festerm-config` does not gain a persisted `WorkspaceTab::MarkdownViewer` in
  v1, preserving the approved non-persistence behavior.
- `festerm-ssh` will need a small additive remote-snapshot API layered on the
  existing SFTP backend rather than a second file-read path.
- Remote viewer reload and remote-image load semantics become stricter and more
  honest: a disconnected snapshot stays readable, but nothing silently retargets
  to a different transport episode or host.
- Syntax highlighting will add one non-trivial dependency (`syntect`), but only
  for a documented small language allowlist and only inside the bounded document
  path.

## Validation impact

- **Invariants introduced or changed:** Markdown viewers are runtime-only
  `TabContent` surfaces, not `ApplicationSession`s; remote snapshots and remote
  resources use only the existing SFTP transport boundary; viewer diagnostics
  remain content-free; no secondary resource loads occur without explicit,
  non-persisted approval; viewer parsing/loading respects fixed byte, line,
  nesting, table-cell, code-block, and resource limits.
- **GUI/action edges affected:** New stable workflow IDs are required for local
  "Open Markdown File…", remote SFTP "Preview Markdown", Preview/Source/Find/
  Outline actions, explicit image loads, and offline-snapshot reload behavior.
  `validation/traceability.json` should be updated in the implementing change
  once those IDs exist.
- **Automated tests required:** Planned coverage should include
  `markdown_source_over_limit_is_rejected_without_content_leak`,
  `markdown_preview_and_source_preserve_heading_anchor_across_mode_switch`,
  `markdown_find_wraps_and_preserves_current_match_on_same_source_reload`,
  `markdown_raw_html_is_never_rendered`,
  `markdown_remote_snapshot_reloads_only_for_matching_verified_origin`,
  `markdown_remote_image_requires_explicit_approval_and_matching_generation`,
  `markdown_workspace_capture_omits_runtime_viewer_tabs`, and
  `markdown_code_block_over_limit_degrades_with_a_content_free_error`.
- **Native/manual evidence required:** Manual scenario coverage is required for
  keyboard traversal, screen-reader order, wide-table/code scrolling, explicit
  local and remote image loads, offline snapshot presentation, and content-free
  oversize/binary/invalid-UTF-8 failures. Stable scenario IDs should be added in
  the implementing change.
- **Coverage superseded:** None.
