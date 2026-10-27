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

The standalone viewer is a first-class non-terminal application surface for
bounded remote snapshots that have no editable local file behind them. Saved
local Markdown instead owns one shared text-editor document and opens in that
tab's Preview mode, with `Edit | Preview | Split` selecting its presentation.
Opening the same canonical local source again focuses its existing editor view
without discarding unsaved text. Neither surface resizes, splits, or takes
ownership of a terminal viewport.

## Entry routes and ownership

- **Desktop Open With:** installed desktop packages advertise `.md` and
  `.markdown` without replacing the user's default application. Finder document
  events and `festerm --open -- <paths...>` enter a bounded application queue,
  not terminal drag-and-drop or SFTP upload handling. New local documents open
  in the last-active window's editor Preview mode; an already-open document
  focuses its existing view across windows without reloading unsaved text. Requests wait
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
  edits/navigation invalidate stale lookup results. Every regular text file
  opens in the shared editor; Markdown starts in Preview and other text starts
  in Edit. An explicitly activated local `file:` link may offer **Open file**
  when it resolves to a readable text document.
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

## Standalone viewer and shared Preview layout

The application chrome remains unchanged. The standalone remote viewer content
contains the following controls; saved-local editor Preview reuses the same
bounded renderer inside its `Edit | Preview | Split` workflow:

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

Preview now retains complete table preparation across frames, in both the
standalone viewer and editor Preview/Split, independently of the native,
software or GPU renderer. Immutable snapshot ownership plus clearing on each
successful reload/reparse supplies exact document identity; no source hash or
matching-text-ends heuristic is used. Table source offset, exact available
width and complete inline style identify one prepared set of final cell
galleys, column widths and row heights. Every Find range and current match
are compared exactly; changing either retires the set.

The shared layout epoch uses a tiny ordinary egui galley as a font-cache
witness. Its retained Arc prevents address reuse. A different context,
viewport, scale, theme text override/dark mode or witness retires preparation.
Egui recreates that cache when definitions/text options change or a full
atlas resets at begin-pass; witness eviction also conservatively invalidates.
No atlas pixels or font-definition clones are retained. Reuse remains bounded
to 512 tables, 8,192 cells and 16 MiB of accounted allocation capacities per
view; the exact Find key separately admits at most 8,192 fixed-size matches.
Job Strings/sections, row/cell vectors, glyphs and row meshes are charged,
including spare capacity and repeated references conservatively. Bounded
BTreeMap nodes, Arc/allocator overhead, the tiny witness, ordinary egui
memoization and current frame/output ownership are not a total-memory claim.
Unsupported or over-budget entries still prepare ordinarily.

All blocks and all selectable cell widgets remain live, including offscreen
ones. This is neither whole-document virtualization nor table-cell byte
navigation (which remains unimplemented). Headings, code-byte navigation,
Copy/selection, full accessibility information, image/resource policy and
loading remain unchanged. CPU construction/copy counters and ordinary-versus-
cached complete shape/accessibility/input oracles are deterministic evidence,
not measured end-to-end speedups or native/GPU qualification.

On a preparation miss, table columns are measured from unwrapped cell content
before sharing the reading width. A cell whose measured galley fits the final,
integer-normalized wrap constraint reuses it; only a squeezed cell needs another
layout. This is frame-local measurement reuse within the persistent
preparation: every cell remains a selectable label with the same alignment, Find formatting and
source identity.

The same fresh render invocation also shares identical unwrapped, plain-text
table-cell galleys across its tables. Eligibility is checked on borrowed text
before job preparation: one nonempty text inline, at most 256 UTF-8 bytes,
an explicit non-custom font family, and no overlapping Find match. Complete
section formatting and the actual painter's context, viewport/pass, scale and
active text options must match. All other default-job properties, including
the infinite unwrapped width, are fixed by this callsite; final constrained
wrapping remains ordinary. Definitions and atlas changes activate at begin-pass,
and no entry survives the fresh `MarkdownRenderState`. No font-definition clone
or persistent document-text key is introduced.

The fixed array admits at most 32 galleys, under a separate 64 KiB accounted
payload ceiling. Admission after ordinary construction charges actual String,
section/placed-row/glyph Vec and row-mesh vertex/index Vec capacities, plus the
Galley, LayoutJob and row object bodies. Checked arithmetic rejects overflow.
At most 257 rows per entry are eligible; array/key metadata and bounded Arc
control blocks/allocator overhead are separate, not a total-memory claim.
Ordinary construction, egui's existing memoization and current widget/output
owners are outside this retained-payload allowance.
There is no replacement/eviction overlap: full or over-budget entries simply
prepare normally, while already-admitted entries remain usable. Complex,
highlighted, oversized and custom-family cells also prepare normally. Every
cell still has its original live selectable Label, identity, response,
accessibility and alignment. This is frame-local preparation reuse, not table
virtualization, Find truncation, or a native/GPU performance claim.

Fences labelled `mermaid` render offline diagrams in both standalone Preview
and saved-local editor Preview/Split, using the native renderer and bounds in
[ADR 0050](adr/0050-native-mermaid-preview.md). Available families are flowchart,
sequence, class, state, ER, pie, Gantt, journey, timeline, Sankey, quadrant, XY,
radar, packet, mindmap, git graph, requirement, C4, block, architecture, Kanban,
treemap and ZenUML. This is not full Mermaid.js compatibility. Copy retains the
exact fenced payload; **Show Mermaid source**, Find and byte navigation expose
selectable source. Invalid, unsupported or over-limit diagrams show source and
a content-free explanation. Diagrams are noninteractive raster previews:
callbacks, links, animations and resource references cannot execute or fetch.
No browser, JavaScript or external rendering service is required.

Scroll position belongs to each open document, not the last document shown
in the window. Newly opened documents start at the top; switching back to an
already-open document retains its own position.

Inline and block raw HTML are displayed as inert source or an explicit
`HTML not rendered` placeholder, except the bounded native SVG and HTML anchor
projections defined by [ADR 0048](adr/0048-bounded-native-svg-and-html-anchors.md).
This is not a browser, DOM, or general HTML renderer. Fenced/backtick code,
comments, scripts and other opaque HTML blocks remain literal. Footnotes, math,
other diagram extensions, custom containers, includes, embedded web content, and
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

Bounded relative raster and static SVG images of saved local documents load automatically,
up to 64 distinct references per snapshot; later references keep **Load local
image**. Both standalone viewers and editor Preview share four actual running
loads and the Settings **Image memory budget** (512 MiB default). Existing
admissions survive lowering/saturation; additional growth is visibly refused,
and temporary refusals recover when their required space or slot is available.
Permanent failures do not retry every frame; explicit retry remains available.
Complete inline `<svg>` fragments use the same image pipeline on every origin,
including remote and untitled documents, without acquiring file authority.
Their XML bytes are part of the snapshot, not a path, URL, or persisted approval.

The complete canonical saved Markdown file identity grants reads, never its
lexical symlink-file parent, a presentation label or a fallback path. A source
that cannot be resolved keeps its text Preview and visibly disables image reads.
The document store records canonical path and identity/size/modification
generation from the no-follow handle that supplied the editor bytes.
Reload/save generation changes retire old image state before reparsing. Each
image read walks that canonical parent without following newly inserted
aliases, verifies the Markdown file through the captured directory against the
loaded generation, and resolves the canonical in-root image destination
beneath the same handle. Source replacement, component rebinding and
symlink/reparse-point escapes therefore cannot retarget a load; authorized
in-root aliases remain readable. Directory capabilities belong only to active
reads, not open tabs. Actual encoded input is bounded to 8 MiB for raster files
and 1 MiB for UTF-8 SVG, dimensions to
16 Mi-pixels and the live texture-axis limit, before owned expansion. Opened
metadata must identify a regular file, and Unix nonblocking opens reject
special files without waiting for a FIFO writer. Reservations track actual
worker/result/texture/CPU-upload owners through close and snapshot replacement.
See ADR 0030 for the conservative envelope and bounded retirement traversal.

Remote, untitled and terminal-history Preview never read local images. Remote
resource fetch remains unimplemented and may only be added through the same
verified SFTP origin/generation. Network images, data URLs, external SVG
dependencies, fonts, scripts, external stylesheets, iframes, and includes never
load. Approval is per snapshot/view,
not a persisted trust grant. The shared allowance is not a process-RAM/VRAM
cap: decoder-private allocation, native renderer/GPU retirement and allocator
fragmentation are outside it.

### Native SVG and HTML anchors

SVG is statically rasterized at its intrinsic dimensions on the existing image
workers. Shapes, text, internal `defs`/`use`, gradients, clipping and
bounded embedded CSS are supported. Text substitutes the fixed bundled
JetBrains Mono family; no system-font discovery or document-supplied fonts occur.
DTDs, processing instructions, scripts/events, embedded images, external/data
hrefs (including SVG hyperlink hrefs), foreign HTML, animation, filters, masks
and patterns are refused. Markers are unsupported pending bounded per-vertex
expansion: any `marker` element, including nested/unused definitions and
direct/inherited/CSS applications, fails explicitly during XML preflight before
the SVG parser can multiply it at path vertices. Worker/reference caps are not
per-image expansion limits. Resource resolvers are disabled independently.

Admission bounds XML to 16,384 nodes, XML/reference depth to 64, expanded work
to 65,536 nodes/4 MiB attribute-text bytes, text to 16,384 glyphs, and aggregate
CSS to 16 KiB/128 rules. Output and temporary layers have a 16 Mi-pixel ceiling
and output respects the current texture-axis limit. Parser, output, retained
pixel copies and conservative layer/clip scratch must fit the shared allowance
before their respective expansion/rendering stages. Expanded nodes plus glyphs
times output pixels additionally obey a 512 Mi pixel-work ceiling before
raster allocation/painting, conservatively refusing excessive overdraw even
when its memory would fit. Malformed, incomplete or
over-projection-limit inline SVG remains literal; projected but unsafe,
unsupported or over-budget SVG has a visible failure with explicit retry.

Restricted native `<a>` tags accept only `href`, `title`, `id` and `name`.
Attribute names are case-insensitive; quoted/unquoted values decode XML named
entities and numeric character references before destination classification.
Duplicate/unknown attributes, unsupported entities and malformed/nested tags
remain literal. Targets retain exact source byte spans; the first duplicate
name wins, explicit named targets precede heading fallback, and targets never
create outline headings. Other HTML styling is not interpreted.
Source-byte navigation consumes the request and scrolls to actual shared
block/anchor geometry, including empty anchors and anchors containing plain
text, in standalone Preview and editor Preview/Split. The owner accepted
ADR 0048 on 2026-10-14 after these resource/navigation repairs; native fidelity,
accessibility and DPI qualification remain pending.

HTML and Markdown links share application-command routing and scheme policy.
Decoded rooted paths, Windows UNC/device/drive destinations and control
characters are blocked before any filesystem access; relative Markdown links
remain supported. Blocked links are disabled with an explanatory tooltip.
Ordinary reparsing retains completed inline images only when their XML payload
as well as reference kind/class/target is unchanged. Changed XML retires the
old image and admission even if its descriptive target and source offset match.

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

The standalone viewer retains exact wrapped outline-row geometry for at most
4096 headings in its current snapshot. An unchanged offscreen row reuses its
height and text ink bounds instead of cloning its heading into another layout
job. Every row still has a live response, focus identity and accessibility node;
visible rows always prepare normally. Actual width, pixels per point, explicit
font, active font definitions and text options invalidate geometry; successful
snapshot replacement clears it. Theme/selection colors remain live and cannot
affect the stored dimensions. Rows beyond the cap prepare normally, never
disappear. No galley or atlas UV survives a frame in this cache.

The shared editor Preview also reuses its existing heading-position vector,
clearing positions before each render; the standalone viewer no longer collects
unused positions. These are bounded preparation/allocation reductions, not
Preview block virtualization. Full Source traversal, full Preview traversal,
and the editor's separate outline remain follow-up work. Deterministic
old-control/candidate layout counts and visible-shape/navigation/accessibility
oracles do not establish native latency, GPU drawing or total-memory gains.

Source also retains bounded **unwrapped formatting instructions**, not laid-out
text: at most 8192 line jobs and 4 MiB of their actual string/section-vector
capacities per loaded viewer snapshot. Entry/Arc/map metadata is separately
bounded by the entry count; this is not a process-memory cap. Snapshot replacement
and Find-query changes discard the jobs. Moving the current match refreshes
cached lines overlapping its old/new location; all matches remain available.
Queries over 4096 UTF-8 bytes use ordinary preparation without truncating Find,
and lines that exceed either cache bound use the ordinary path.

The existing egui 0.36.1 `WidgetText::LayoutJob(Arc<LayoutJob>)` API is already
used here. Sharing the formatting job does not avoid its live `Label` clone:
`Label::layout_in_ui` unwraps/clones the shared job before applying UI wrapping
and alignment, and `FontsView::layout_job` takes an owned job even on a cache
hit. The lower-level `epaint::text::layout` accepts an Arc, but bypasses that
memoization and does not keep the ordinary Label layout path untouched. It is
not a smaller exact replacement for warm Source labels. No dependency API or
font-cache ownership change is part of this candidate.

Every Source line retains its live identity, response and accessibility node.
For admitted jobs in the ordinary top-down, left-aligned, non-grid layout,
fully clipped-offscreen rows can reuse their exact measured local size and
intrinsic size. This finite geometry belongs to the existing bounded job;
there is no second text copy, persistent galley, atlas UV or separate row map.
The new per-entry metadata is one `Option<SourceRowGeometry>` (five f32 values
plus its discriminant/padding); its actual Rust size is counted independently
of the unchanged charged text/section payload. One active dependency key also
owns cloned font-definition map/family metadata and shared font-data Arcs, not
another font-byte copy. Source and Outline each admit at most one dependency
key. Before any definition clone, each font-data/family map must have at most
128 entries; summed owned key/name String capacities plus the UTF-8 lengths of
shared custom `FontFamily::Name` keys must fit 8192 bytes. Total family Vec
**capacity**, not only length, must fit 256 name-reference slots (6144 bytes
when `String` is 24 bytes). Spare capacity is conservatively charged. Checked
addition protects every cumulative count. Actual cloned capacities are checked
again before retention; font bytes and custom-family name bytes remain shared.
Map-node storage is bounded by the two map cardinalities, not an asserted
allocator-byte total. These allowances are separate from the unchanged 4 MiB
instruction payload; they are internal cache eligibility, not user settings.

Pre-admission overflow drops the owner's previous key and clears all stale row
geometry before rendering ordinary live labels, without cloning rejected
definitions. A failed actual cloned-capacity verification also drops the
candidate. Neither case retains new geometry; valid definitions must rebuild
cold before warm reuse resumes. Replacement drops the old key **before** construction:
there is no old/new cloned-key overlap per owner. An unchanged frame performs
bounded borrowed admission/comparison, not key reconstruction. One admission
scan visits at most 128 font keys, 128 family entries and 256 names; replacement
uses at most three such scans including actual cloned-capacity verification.
Comparison uses font-data Arc identity, never traversing large font bytes;
equal-byte replacement is conservatively invalidated. String comparisons stay
within admitted name bounds. Source key reset/rejection/clone counts and actual
cloned String/Vec capacities are reported separately from label/layout work.
The earlier 140/168-byte fixture observations were **not admission ceilings**;
the missing bound was found during handoff and repaired before publication.
Visible, cold, unsupported-layout and uncached rows use ordinary label behavior.
Width, pixels per point, explicit source font, active font definitions and text
options invalidate geometry. Snapshot/query replacement and current-match job
refresh discard it with its job. Theme, opacity, selection and screen-reader/
touch interaction remain live; no approximate heights or omitted widgets are
allowed. Source spans remain tied to the immutable snapshot, and no helper
rewrites decoded text, line endings or raw Copy payloads.

The geometry candidate's focused CPU regressions compare
complete clipped shapes, every response (including intrinsic size and
sense), accessibility, selection/Copy and offscreen byte/heading navigation to
the descriptor-cached ordinary path. Cold/warm/scroll/width/font/scale/options/
theme/revision controls count actual layout requests and their text payloads,
not job-cache hits. Font-map cardinality, owned-name bytes (including custom
family keys) and family-reference overflow require ordinary work with identical
full shapes/responses/accessibility and raw Unicode Copy. Recovery and exact
128/8192/256 admission-boundary controls require cold rebuilding followed by
warm reuse without another key clone. The warm 400-section narrow/normal controls
retain all 4800 rows while reducing 4800 ordinary requests to 31/34 (34/37 after scrolling),
below the required limit of 96. Cloned candidate String/Vec capacities are
4408/4822 bytes versus the ordinary shared Label inputs' 680870-byte clone
payload. The ordinary clone condition follows the pinned Label implementation;
candidate capacities are captured after its actual unwrap/clone. These are
specific owned-buffer payloads, not allocator usable-size or total allocations.
The fixture's geometry slots add 115200 bytes (24 per admitted job; at most
196608 at 8192), plus a 112-byte inline key and separately reported font map/
family/string/vector metadata. Map-node allocator overhead is not claimed.
Real dependency changes relayout every row, including the width settling one
frame after a scale change; only an actually stable key qualifies as warm.
Total UI time, GPU drawing, native latency and total memory remain separately
unqualified. Full live Source traversal and Preview work remain open under #348.

The standalone viewer's ordinary opaque `SURFACE_WINDOW` outer frame goes
through the existing `software_background::show_frame`, just like other
eligible application surfaces. Only that frame changes painting route; child
Preview/Source frames, text, controls and resource policy are unchanged.
The helper retains its existing renderer/opacity/visibility/root-viewport/
transform/frame guards and ordinary fallback. Real Preview/Source caller
pixels are compared exactly on Windows DX12 CPU `Rgba8Unorm`, at narrow/wide
widths and 1/1.25x scale with fractional clipping; 0.5 opacity must stay ordinary.
This qualifies rendering fidelity and callback execution, not draw time or
native latency. Completed real Markdown workload attribution remains separate.

Preview code-byte navigation forwards the existing selected row's vertical
target after its horizontal code scroller closes. Find and the shared editor
Preview therefore reach offscreen fences instead of consuming the byte request
inside a horizontal-only viewport. The original row choice, horizontal target,
wrapping, selection and raw code Copy remain unchanged. Table cells do not yet
handle byte targets; this separate gap is not covered by the code-row fix.

Preview selection Copy publishes restricted semantic HTML with the existing
plain-text selection as its fallback. This applies to the standalone viewer
and editor Preview/Split. A scoped selection opt-in preserves right-click only
for labels rendered inside the Markdown surface; ordinary selectable labels
retain upstream secondary-click behavior. The Markdown menu offers
**Copy rich text**; holding Shift while right-clicking additionally offers
**Copy Markdown source**, using parser-owned byte spans from the selected
snapshot, not searching for repeated text or reconstructing Markdown.
Fully selected inline formatting includes its original delimiters; partial
selections retain only their mapped source bytes. When a partial transformed
run cannot be mapped exactly, the source action is disabled with guidance to
select the complete run or use Source mode.

The rich exporter escapes literal HTML and permits only HTTPS link targets;
it does not embed images, SVG, document CSS, executable HTML, or local/remote
resource authority. A selection containing unsupported rendered widgets or
exceeding the 16 MiB rich-output limit retains plain-text Copy instead of
publishing incomplete rich content. Clipboard export does not fetch resources
or read the clipboard. Rich inline boundaries and soft/hard breaks come from
the parsed document rather than widget placement, so wrapping cannot add or
remove semantic separators; keyboard and context-menu rich Copy both carry
egui's exact plain selection rather than a second geometry heuristic. Menus
retain only a bounded selected snapshot and bind it to a process-unique surface
owner plus that surface's monotonic document revision. They retire on
replacement, reparse, hidden ownership, or a closed viewport; source bytes are
retained only for an exact Shift-requested source action. Native HTML delivery
uses the existing clipboard
backend; unsupported/failed HTML writes explicitly log and fall back to text.
The current Wayland backend is text-only.

Source-mode Copy and code-block Copy remain plain text; code-block Copy copies
only code content, excluding fences, language labels and line numbers.
Code-only keyboard selections also retain their existing plain-text behavior.

Keyboard paths:

- `Ctrl/Cmd+F` opens Find; Enter/Shift+Enter moves next/previous; Escape clears
  Find before closing the viewer.
- `Ctrl/Cmd+R` manually reloads the source after revalidating identity.
- `Ctrl/Cmd+Shift+V` toggles Preview/Source when it does not conflict with a
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

1. From `dev-shell`, select **Open File…** from More actions and select
   a local README; a sibling editor chip opens in Preview without changing the
   terminal session.
2. Navigate the heading outline, a table, task list, link, and code block using
   keyboard and screen reader; switch through `Edit | Preview | Split`, Copy a
   code block, and verify exact plain text.
3. Find `security`, move between matches, and preserve the current section and
   match while changing editor modes.
4. Verify the first 64 eligible raster/local-SVG/inline-SVG references load
   automatically, explicitly load a later reference, and confirm unsafe SVG,
   network/data resources and rooted/UNC links remain blocked. Navigate HTML
   named anchors without adding outline headings; repeat in Preview and Split.
5. Preview a remote Markdown file from SFTP, disconnect, and verify the complete
   snapshot remains readable but visibly stale; reconnect/reload only against
   the same verified origin.
6. Exercise binary, invalid UTF-8, oversize, deleted, and permission-denied
   fixtures; confirm bounded cancellation/recovery, content-free diagnostics,
   and return to the prior surface.
