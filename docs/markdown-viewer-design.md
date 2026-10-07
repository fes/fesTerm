# fesTerm Native Markdown Viewer — Product/UI specification

**Status:** approved product design; implementation and validation pending

**Interactive workflow mockup:**
[`images/gui-mockups/markdown-viewer-workflow.html`](images/gui-mockups/markdown-viewer-workflow.html)
steps through opening a document, reading and navigating it, Find, Source view,
blocked resources, and a disconnected remote source.

## Product decision

fesTerm should include a bounded, read-only Markdown viewer. Its primary quality
target is **readability-first**: dependable document structure, typography,
tables, task lists, code, links, selection, Copy, Find, and accessibility. It
does not promise browser-identical rendering or become an editor, preview
server, web browser, notebook, or general IDE.

The viewer is a first-class non-terminal application surface in the existing
chip row. Each open document owns one viewer chip labeled with its basename and
a secondary origin (`Local` or the stable SSH/SFTP session or profile name).
Opening the same canonical source again focuses its existing viewer; opening a
different document creates another chip. Viewer lifecycles never resize, split,
or take ownership of a terminal viewport.

## Entry routes and ownership

- **Desktop Open With:** installed desktop packages advertise `.md` and
  `.markdown` without replacing the user's default application. Finder document
  events and `festerm --open -- <paths...>` enter a bounded application queue,
  not terminal drag-and-drop or SFTP upload handling. New documents open in the
  last-active window's Preview mode; an already-open document focuses its
  existing view across windows without reloading unsaved text. Requests wait
  while a blocking dialog or open-error notice is active. Multiple files are
  opened in order, with ordinary local-document load failures shown explicitly.
  A file-bearing second launch forwards to the process owning the per-user
  activation endpoint before loading workspace/configuration state; an ordinary
  no-argument launch retains independent-process behavior. Linux AppImage
  associations require desktop integration, not just downloading the image.
- **Local:** More actions **Open File…** opens the application file picker, which
  lists text and Markdown files. Its **File or folder path** field accepts pasted
  absolute paths, `~/...`, and paths relative to the displayed folder. Enter or
  **Open path** opens a regular file or navigates into a directory; invalid paths
  retain the input and show the error. Ctrl+L (Command+L on macOS) focuses the
  field. Metadata lookup shares the bounded background directory loader, and
  edits/navigation invalidate stale lookup results. A Markdown file lands here; anything else
  opens in the text editor, which has nothing to render it as. An explicitly activated local `file:` link may offer **Preview
  Markdown** when it resolves to a readable Markdown file.
- **SFTP:** a selected `.md`/`.markdown` row offers **Preview Markdown**. The
  viewer receives a bounded read-only snapshot through the SFTP/application
  layer; a remote path never becomes a local path.
- **SSH terminal / text-mode SFTP transcript:** fesTerm may offer **Open in
  viewer** only after an explicit right-click on one detected path-like target
  in the frozen terminal snapshot. Detection is bounded to the clicked logical
  line/cells, never evaluates shell syntax, and opens a remote path only
  through the same live verified SSH/SFTP transport that produced the text.
  Relative paths remain disabled unless the session exposes a trustworthy cwd
  (for example the text-mode SFTP session's own cwd); unknown cwd is reported
  honestly instead of guessed from prompts or launch directories.
- **Serial:** has no implied filesystem and therefore no Markdown entry route.

Local and remote sources use typed identities, not display strings. A remote
viewer is pinned to the SSH connection/profile identity and lifecycle
generation that supplied it; reconnect cannot silently retarget the document
to a different host.

Terminal-origin remote reads open a separate SFTP channel on the authenticated
connection, so session-only host trust and typed-password sessions work without
re-authentication. Two concurrent reads per connection and four pending opens
per application window are allowed; reads have a 30-second deadline and the
document byte cap. Shell input/output and lifecycle commands continue while
SFTP is pending. Manual or automatic transport replacement invalidates queued
requests; no late result is relabelled as belonging to the new connection.

## Viewer layout

The application chrome remains unchanged. The viewer content contains:

1. A compact document toolbar with origin icon/label, elided full path, manual
   Reload, Preview/Source switch, Find, Outline toggle, and overflow.
2. An optional resizable/collapsible heading outline on the left. It reflects
   the semantic heading tree, highlights the section nearest the viewport, and
   supports arrow-key traversal and Enter to navigate.
3. One centered reading column with a comfortable maximum line length while
   wide tables and code blocks scroll within their own bounded region.
4. A contextual footer only for useful source state: Local/Remote, UTF-8,
   stale/disconnected, loading, or an actionable error. It does not show
   invented word counts, reading time, or continuous telemetry.

Preview is the default. Source displays the exact decoded text in a selectable
read-only monospace surface with line numbers off by default; switching modes
preserves the nearest heading and relative scroll position. Viewer zoom is an
application-text preference local to the surface and does not change terminal
font size or terminal grid geometry.

## Supported Markdown contract

The first implementation supports CommonMark plus these bounded GFM features:

- tables;
- task lists rendered as non-interactive checked/unchecked indicators;
- strikethrough;
- autolinks;
- fenced code blocks with an optional language label and Copy action; and
- deterministic, bundled syntax highlighting for a documented language set.

Fenced highlighting uses the shared syntax engine, with process-wide
immutable compiled queries but independent document/parser state (ADR 0035).
The Markdown model walks ordered spans forward when building code lines,
preserving newline-spanning roles, plain gaps and exact source bytes instead
of scanning all spans again for every line. Parsing and the existing code-size
and syntax-budget bounds remain unchanged.

The identical inactive **Copy** caption is prepared lazily at the first code
header's actual painter and reused only within a fresh `MarkdownRenderState`
invocation. A single entry checks painter-context identity, viewport/pass,
pixels per point, explicit font and colour; a mismatch prepares normally.
Egui activates font definitions, text options and atlas resets at begin-pass,
so no prepared entry survives into another renderer invocation. Width is not a
caption dependency (`layout_no_wrap`); the button's current allocation, ID,
accessibility information, clip, hover, focus, tooltip and click remain live.
Every fence retains its own raw Copy payload and selectable highlighted body.
This removes repeated caption string preparation and memoized egui cache
calls, not whole text layouts or code/table widgets. Ordinary/prepared CPU
oracles cover the complete clipped shapes, response geometry, accessibility,
selection, navigation and raw Copy payloads across dependency changes.
The matched synthetic measurement and its adverse controls are recorded in
[`milestone-progress.md`](milestone-progress.md#preparing-a-repeated-code-header-caption-without-retaining-document-layout).
A one-versus-400 preparation count alone does not establish a speedup, and
neither that experiment nor these regressions qualify native clipboard,
screen-reader usability or input-to-display latency.

Table columns are measured from unwrapped cell content before sharing the
reading width. A cell whose measured galley fits the final, integer-normalized
wrap constraint reuses it; only a squeezed cell needs another layout. This is
frame-local reuse, not a persistent cache or block virtualization: every cell
remains a selectable label with the same alignment, Find formatting and
source identity.

Inline and block raw HTML are displayed as inert source or an explicit
`HTML not rendered` placeholder. They are never interpreted. Footnotes, math,
Mermaid/diagrams, custom containers, includes, embedded web content, and
Markdown extensions outside the declared contract are deferred. Unsupported
constructs degrade to readable literal text rather than disappearing.

## Links and resources

Heading anchors navigate within the current viewer and update outline focus.
Every other link exposes its destination on hover/focus and uses explicit
activation:

- `https:` links use the existing safe external-link handoff; they are never
  embedded inside the viewer.
- Relative Markdown links resolve against the typed source origin, then focus
  or open a viewer only after activation.
- Other local or remote files use their owning surface/application; the viewer
  does not become a generic file launcher.
- Dangerous or unsupported schemes are inert and explained.

Only bounded relative raster images of saved local documents load automatically,
up to 64 distinct references per snapshot; later references keep **Load local
image**. Both standalone viewers and editor Preview share four actual running
loads and the Settings **Image memory budget** (512 MiB default). Existing
admissions survive lowering/saturation; additional growth is visibly refused,
and temporary refusals recover when their required space or slot is available.
Permanent failures do not retry every frame; explicit retry remains available.

The complete canonical saved Markdown file identity grants reads, never its
lexical symlink-file parent, a presentation label or a fallback path. A source
that cannot be resolved keeps its text Preview and visibly disables image reads.
Actual encoded input is bounded to 8 MiB, dimensions to 16 Mi-pixels and
the live texture-axis limit, before owned expansion. Canonical image destinations
must stay inside the opened file's canonical parent directory; filesystem-rooted
paths and traversal/symlink escapes are blocked. Root acquisition walks the
already-authorized canonical parent without following newly inserted aliases;
final image resolution stays beneath that directory handle even when names or
intermediate components are rebound. Opened-handle metadata must identify a
regular file before any content read. Authorized in-root image aliases remain
readable. Directory capabilities belong only to active reads, not open tabs.
Unix nonblocking opens reject
special files without waiting for a FIFO writer. Reservations track actual
worker/result/texture/CPU-upload owners through close and snapshot replacement.
See ADR 0030 for the conservative envelope and bounded retirement traversal.

Remote, untitled and terminal-history Preview never read local images. Remote
resource fetch remains unimplemented and may only be added through the same
verified SFTP origin/generation. Network images, data URLs, SVG, fonts, scripts,
stylesheets, iframes, and includes never load. Approval is per snapshot/view,
not a persisted trust grant. The shared allowance is not a process-RAM/VRAM
cap: decoder-private allocation, native renderer/GPU retirement and allocator
fragmentation are outside it.

## Find, selection, Copy, and keyboard behavior

Find is scoped to the decoded document and works in Preview and Source. It
shows `N of M`, supports next/previous with wrap, highlights all visible
matches with a stronger current match, and preserves the current match across
a same-source manual reload when possible.

All non-overlapping literal matches remain available in source order; Find
does not cap or truncate the result set. Ordered source-position lookups reuse
the already-counted Unicode prefix, and highlighting visits only the matches
overlapping each text run. These are internal cost reductions, not changes to
match counts, current-match selection, clipping, or Unicode source offsets.

Preview code-byte navigation forwards the existing selected row's vertical
target after its horizontal code scroller closes. Find and the shared editor
Preview therefore reach offscreen fences instead of consuming the byte request
inside a horizontal-only viewport. The original row choice, horizontal target,
wrapping, selection and raw code Copy remain unchanged. Table cells do not yet
handle byte targets; this separate gap is not covered by the code-row fix.

Text selection and Copy produce plain text by default. Code-block Copy copies
only code content, excluding the language label and line numbers. A future
**Copy as Markdown** command may be added only with a precise source-range
mapping; v1 does not reconstruct Markdown from rendered selections.

Keyboard paths:

- `Ctrl/Cmd+F` opens Find; Enter/Shift+Enter moves next/previous; Escape clears
  Find before closing the viewer.
- `Ctrl/Cmd+R` manually reloads the source after revalidating identity.
- `Ctrl/Cmd+Shift+M` toggles Preview/Source when it does not conflict with a
  platform-reserved binding; the command palette is authoritative.
- `Ctrl/Cmd+Shift+O` toggles the heading outline.
- Tab traverses toolbar, outline, links, resource actions, table regions, and
  code Copy actions; heading navigation and document scrolling remain usable
  without pointer precision.
- Existing `Ctrl+Tab` / `Ctrl+Shift+Tab` session switching is unchanged.

All routes dispatch the same typed application commands. Terminal input never
receives viewer shortcuts while the viewer owns focus.

## Freshness and lifecycle

The viewer is read-only. V1 performs one bounded load when opened and reloads
only on explicit user action; there is no file watcher, autosave, conflict
resolution, or background polling. A successful reload replaces the snapshot
and restores the nearest heading/scroll position. Failure leaves the prior
snapshot visible and marks it stale.

Remote disconnect keeps the last complete snapshot visibly available as
**Offline snapshot**. Reload and unresolved remote resources are disabled until
Reconnect succeeds for the same origin; no partial response replaces a valid
snapshot. Closing/Escape returns to the exact prior surface when it still
exists, and closing the final viewer returns to Launcher. Viewer documents,
paths, contents, scroll positions, and Find queries are not persisted in
workspace state or recent history in v1.

## Bounds, loading, and errors

The implementation must define tested byte, decoded-line, nesting, table-cell,
code-block, and resource limits before shipping. Loading/parsing is cancellable
and cannot block terminal/session event handling. The first pass accepts UTF-8
with an optional BOM; invalid encoding, binary input, oversize input, unreadable
paths, permission failure, parse failure, and remote disconnect produce concise
content-free errors with Retry/Back/Details as applicable.

States retain stable chrome:

- **Loading:** source identity and Cancel remain visible; no fake document
  skeleton is shown.
- **Empty:** `This Markdown file is empty` with Source still available.
- **Unsupported/binary/oversize:** explain the category and limit without
  leaking content into diagnostics.
- **Reload failed:** keep the last complete snapshot, label it stale, and offer
  Retry.
- **Source deleted:** keep the snapshot, label the source unavailable, and do
  not claim an editable conflict.

Diagnostics may record source class, bounded size category, operation kind,
duration, and sanitized failure category. They never retain document content,
literal local/remote paths, link destinations, Find queries, or copied text.

## Accessibility and visual fit

Use the approved blue-graphite palette and first-party 24-unit semantic icons.
Implementation should add stable semantic icons such as `MarkdownDocument`,
`Outline`, `RenderedView`, `SourceView`, and `ExternalLink` only through the
existing icon pipeline. Rendered headings expose a correct semantic hierarchy;
tables, lists, links, code, blockquotes, and task states use platform
accessibility roles rather than visual styling alone. Focus remains visible,
status is never color-only, and document text supports selection, screen-reader
reading order, platform UI scaling, high contrast, and reduced motion.

## Acceptance sequence

1. From `dev-shell`, select **Open Markdown File…** from More actions and select a local README;
   a sibling viewer chip opens without changing the terminal session.
2. Navigate the heading outline, a table, task list, link, and code block using
   keyboard and screen reader; Copy a code block and verify exact plain text.
3. Find `security`, move between matches, then switch Preview/Source while
   preserving the current section and match.
4. Reach a relative image placeholder and explicitly load a bounded local
   raster; verify network/data/SVG resources remain blocked and nothing loads
   before the action.
5. Preview a remote Markdown file from SFTP, disconnect, and verify the complete
   snapshot remains readable but visibly stale; reconnect/reload only against
   the same verified origin.
6. Exercise binary, invalid UTF-8, oversize, deleted, and permission-denied
   fixtures; confirm bounded cancellation/recovery, content-free diagnostics,
   and return to the prior surface.
