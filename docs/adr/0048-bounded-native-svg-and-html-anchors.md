# ADR 0048: Bounded Native SVG and HTML Anchors in Markdown

- **Status:** Accepted
- **Date:** 2026-10-14
- **Acceptance:** Owner accepted 2026-10-14 after the resource-policy and
  named-target navigation repairs below; native fidelity, accessibility and DPI
  qualification remain pending under CP-06/CP-15.
- **Supersedes:** None; narrowly amends ADR 0030's inert-HTML and raster-only policies

## Context

The owner requested both local SVG image links and inline SVG markup, then
approved native HTML links and named anchors. ADR 0030 currently blocks SVG
and renders all HTML inertly. These additions need an explicit resource-policy
review, not a browser or a general HTML interpreter.

## Decision

Keep Markdown projection UI-independent and rendering application-owned.
Recognize complete SVG fragments and a restricted set of HTML anchors in the
bounded source model. Code fences remain literal. Source mode always preserves
the exact document. Other HTML remains inert and visible.

Rasterize static SVG with `resvg` on the existing bounded image workers, then
use ordinary egui textures in standalone Preview and editor Preview/Split.
Local SVG files retain the existing canonical directory/source-generation
authority. Inline SVG owns source bytes only and grants no filesystem authority,
including in remote or untitled documents.

All SVG loads share the image-memory allowance, four-worker limit, automatic
reference limit, explicit failure/retry policy, and actual-owner retirement.
Enforce encoded-input, XML complexity, expansion, output-axis/area, and temporary
raster admission limits before rendering. Reject DTDs, scripts, foreign HTML,
animation, external resources, and unsupported costly filter/mask content.
Disable all image resolvers and system-font discovery. Text uses a fixed
bundled font database, never document-supplied font files.

HTML `a` elements may declare `id`/`name` targets or `href` links. Links enter
the same typed resource classification and application commands as Markdown
links; dangerous schemes remain inert. Named anchors navigate by source byte
without introducing phantom outline headings. Attribute values are decoded
before policy classification. No event handlers, CSS, DOM, or browser execution
are introduced.
The shared renderer consumes source-byte requests using block geometry and
explicit zero-text anchor runs. Empty anchors and plain-containing anchors
therefore move the actual viewport in standalone Preview and editor
Preview/Split, including offscreen targets; they do not add headings.

Both Markdown and HTML links refuse decoded rooted/UNC/device/drive targets and
control characters before file operations; native activation independently
checks relativity. Blocked links are disabled with visible refusal guidance.
The supported anchor attributes are `href`, `title`, `id`, and `name`; XML
entities/numeric character references are decoded, while unknown/duplicate
attributes, unsupported entities and malformed/nested tags remain literal.
Opaque comments/script/other HTML blocks never promote embedded SVG or anchors.

SVG limits are 1 MiB encoded UTF-8 XML, 16,384 XML nodes, depth 64, 65,536
expanded nodes, 4 MiB expanded attribute/text bytes, 16,384 glyphs, aggregate
16 KiB/128 CSS rules and 16 Mi-pixel output/temporary layers. Parser expansion
and conservative isolated-layer/clip scratch are reserved before the respective
decoder stages. Filters, masks, patterns, embedded images and external/data
hrefs, including SVG hyperlink hrefs, are unsupported. Rendering uses intrinsic
dimensions; native fidelity and DPI remain qualification items.
Markers are also unsupported pending bounded per-vertex expansion. Reject every
`marker` element during XML preflight, before `usvg` parsing, including unused or
nested definitions and markers applied directly, through inheritance or CSS.
Charging a marker reference once does not bound the parser's expansion at every
path vertex; neither the four-worker nor 64-reference cap bounds one image.
Refusal is an explicit permanent image error, never successful marker dropping.
Before raster allocation/painting, expanded XML nodes plus glyphs multiplied
by output pixels must also fit 512 Mi pixel-work units. Separate node/area
limits alone would still admit excessive repeated full-canvas overdraw.
This conservative work bound may refuse complex otherwise-valid diagrams.

## Alternatives considered

- A webview or general HTML renderer: broadens execution, resource, platform,
  and ownership boundaries unnecessarily.
- Raster files only: fails the explicitly requested SVG and anchor workflows.
- A global egui SVG loader: cannot enforce per-document file authority,
  snapshot lifetime, worker admission, and the existing image allowance.
- System-font lookup: introduces filesystem access and nondeterministic output.

## Consequences

SVG and anchors become documented exceptions to inert HTML, not general HTML
support. Rendering may differ from a browser, particularly font substitution.
Unsupported or over-budget SVG stays an explicit error rather than silently
fetching dependencies or consuming unbounded work. Remote image-file fetch
remains deferred; inline content does not change remote transport ownership.
Native renderer, DPI, clipboard, accessibility, and usability qualification
remain separate from deterministic parser/pixel/input regressions.
Malformed, incomplete or over-projection-limit inline fragments remain literal;
only successfully projected images can produce decoder/refusal placeholders.
Unchanged inline images survive ordinary reparsing only when their XML payload
also matches, not merely their descriptive target or source position.

## Validation impact

- **Invariants introduced or changed:** ADR 0030's SVG/inert-HTML restriction
  gains only the native static SVG and anchor exceptions; file authority,
  resource budgets, application-command ownership, and source privacy remain.
- **GUI/action edges affected:** `MD-04`, `MD-05`, `EDIT-09`, `EDIT-18`.
- **Automated tests required:** SVG source projection and code-fence exclusion;
  HTML href classification and named-anchor navigation; bounded SVG pixels,
  bundled text, malicious XML/resource rejection, memory/worker admission,
  lifetime/reparse, and shared editor/viewer behavior. Encoded input, XML nodes,
  aggregate CSS bytes/rules, texture axes and resource admission require both
  exact-limit success and limit-plus-one refusal.
  Paint-work admission also requires inclusive boundary/+1 checks and actual
  overdraw/glyph SVG refusal before rasterization.
  `svg_rejects_marker_vertex_expansion_before_parser_admission` must refuse the
  repeated-vertex marker fixture and direct/inherited/CSS/nested variants.
  `native_html_named_anchors_scroll_offscreen_targets_and_consume_requests` and
  `native_html_real_editor_preview_and_split_scroll_to_named_targets` must prove
  actual viewport movement/target visibility, not only request creation.
  Empty/plain-containing anchors, UTF-8 byte positions, first-duplicate
  precedence, and request consumption are covered without timed sleeps.
- **Native/manual evidence required:** Existing `MD-04`, `MD-05`, and editor
  Preview image scenarios gain SVG/anchor checks; complete native evidence
  remains pending.
- **Coverage superseded:** SVG-is-always-blocked assertions are replaced by
  bounded rendering and explicit unsafe/unsupported SVG rejection.
