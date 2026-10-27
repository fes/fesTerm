# ADR 0050: Offline Mermaid Preview Without a Browser

- **Status:** Proposed
- **Date:** 2026-10-27
- **Supersedes:** None; extends ADR 0030's supported Markdown contract

## Context

The owner requested Mermaid charts in Markdown Preview and chose native,
offline rendering with all diagram types available from the selected renderer,
rather than full Mermaid.js compatibility. ADR 0030 deliberately deferred
diagrams and forbids embedded browser execution and implicit resource access.

## Decision

Recognize `mermaid` fenced code in the shared desktop Preview renderer.
Use pinned `mermaid-svg` 0.7.0 to produce an internal SVG, then `resvg` to
rasterize it to an ordinary egui texture. This does not widen ADR 0048's separate
document-supplied SVG admission or image budget. No browser, JavaScript runtime,
shell command, network
fetch, installed-font scan or external image resolver is involved. Raster text
uses the existing bundled JetBrains Mono font. Generated hyperlinks, callbacks,
HTML and animation are inert pixels, never application actions.

Rendering remains application-owned; the Markdown model, source offsets,
terminal ownership and mobile dependencies do not change. All renderer-supported
diagram families are available, without claiming complete Mermaid.js parity.
Invalid/unsupported syntax and refused previews retain selectable source.
Copy always copies the exact original fenced payload. Successful previews offer
**Show Mermaid source**; Find and byte navigation reveal source automatically.

At most two workers run across windows, including work whose view has closed.
Each immutable snapshot admits at most 16 diagrams. Work accepts at most
8 KiB, 128 lines, 512 alphanumeric/underscore items, nesting depth 16 and
integer runs no greater than 4096. These deliberately conservative bounds can
refuse otherwise valid diagrams, including large numeric labels or dates.
Generated SVG is limited to 2 MiB after serialization. Output is downscaled to
2048 pixels per axis and 1 Mi-pixel before pixmap allocation. A view therefore
retains at most 64 MiB of RGBA texture payload, separately from ordinary image
admission; native GPU/CPU copies, renderer-private allocations and temporary
SVG/parser/layout ownership are not included in that accounting.

Workers publish one bounded result and wake their originating context. Closing
or successfully reparsing drops its receiver; stale results cannot populate a
new snapshot.
Busy admission retries with a delayed repaint because completion may wake
another window. This is input/concurrency/output bounding, not a hard runtime
deadline or a sandbox for third-party parser/layout code.

## Alternatives considered

- Mermaid.js/webview: strongest upstream fidelity, but changes the native
  execution/resource boundary and was not selected by the owner.
- A handwritten flowchart-only parser: smaller initial coverage but incompatible
  with the requested available-diagram scope.
- `mermaid-rs-renderer`: its text metrics scan installed fonts and write a font
  cache; the smaller `mermaid-svg` implementation avoids those side effects.

## Consequences

No persisted configuration or document migration is required. Preview pixels
use a white canvas and fixed bundled font, independent of application theme.
Diagram text is available through source/Find/Copy rather than individual
interactive or accessible diagram nodes. Native readability and accessibility
remain qualification work. Source changes retire all diagram preparation.

## Validation impact

- **Invariants introduced or changed:** ADR 0030's diagram deferral is narrowed
  to allow only internally generated, noninteractive offline Mermaid previews;
  document resource and execution prohibitions remain.
- **GUI/action edges affected:** `MD-04`, `MD-05`, `EDIT-09`.
- **Automated tests required:**
  `mermaid_rasterizes_flowcharts_sequences_and_charts_offline`,
  `mermaid_fences_preserve_normalized_label_and_exact_source`,
  `mermaid_rejects_bad_syntax_and_excessive_work_without_echoing_source`,
  `mermaid_generated_links_and_html_remain_inert_pixels`,
  `mermaid_preview_reuses_pixels_and_exposes_source_on_request_and_find`,
  `mermaid_fences_render_in_shared_preview_keep_copy_and_reveal_find_matches`.
- **Native/manual evidence required:** CP-06 and CP-15 for visual readability,
  clipboard, scale and screen-reader/source workflow across desktop platforms.
- **Coverage superseded:** None.
