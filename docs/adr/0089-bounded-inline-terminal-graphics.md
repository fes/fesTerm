# ADR 0089: Bounded Inline Terminal Graphics

- **Status:** Proposed
- **Date:** 2026-11-19
- **Supersedes:** None

## Context

Inline raster graphics are a deferred product capability, not part of fesTerm's
xterm-compatibility baseline: `REQUIREMENTS.md` and `COMPATIBILITY.md` list Kitty
graphics and Sixel only as future protocol extensions. Current `festerm-core`
semantics remain text-and-cell based. The parser discards APC and recognizes DCS
only for DECRQSS; a DCS payload that reaches the 4096-byte string ceiling returns
to Ground, after which the remaining protocol bytes can be interpreted as
ordinary terminal input. `Cell` has no image field and its existing 64-byte
historical charge is a frozen recovery and scrollback accounting invariant
([ADR 0038](0038-sessiond-terminal-recovery-snapshots.md), [ADR
0081](0081-compact-cells-with-stable-recovery-charges.md)).

Kitty graphics and Sixel can both put raster data in a cell-oriented terminal,
but they have different wire formats, capability discovery, placement models
and ecosystem behaviors. They also create effects that ordinary text rendering
does not have:

- uploads and decoded pixels can exhaust memory, CPU, texture and queue
  budgets;
- images obscure text that remains the source for selection, hyperlink
  hit-testing, rectangle copy and accessibility;
- protocol commands can move the cursor, scroll rows or replace earlier
  placements, so image decode cannot be presentation-only;
- alternate screen, reset, erasure, margins, history, resize, recording and
  durable recovery each need an explicit image lifetime;
- Kitty file, temporary-file and shared-memory transports name and mutate host
  resources, while the producing program may be on another machine;
- Kitty queries and Sixel capability replies can make a profile, worker,
  frontend and renderer disagree about support, image IDs or geometry;
- `festerm-sessiond` is an authoritative terminal mirror and owns replies while
  a session is attached or detached; rendering pixels only in the frontend
  cannot safely determine the daemon's answer.

The architectural question is therefore not which image codec to add. It is
how a protocol-visible, privacy-sensitive raster store can coexist with
fesTerm's single-writer terminal, bounded recovery, raw-output recording and
renderer boundaries.

This ADR proposes a staged architecture and a narrow first enablement. It does
not claim that Kitty or Sixel is currently accepted or release-enabled. Phase 0
is an enabler and safety repair for a demonstrated parser failure. Kitty
enablement remains a deferred product capability; Sixel remains a separately
reviewed capability.

## Decision

### 1. Keep protocol state in the terminal core and pixels out of cells

`festerm-core` owns protocol parsing, validation, image/placement identity,
quota, cursor movement, scrolling and erase effects. A future shared
graphics-validation crate may hold deterministic parsing/decoding helpers so
the same decisions can be used by the app and `festerm-sessiond`, but no
application UI object, texture or worker thread enters `festerm-core`.

Images and placements are a separate, per-buffer side table. A `Cell` is not
extended with image data, identifiers or placement metadata. This preserves the
compact-cell charge, existing recovery schema layout and ordinary text-copy
semantics. A placement records:

- the terminal buffer and an opaque, generation-bound image identity;
- a visible-screen anchor or row token, column and pixel offset;
- source pixel rectangle and destination cell span;
- stacking order and protocol ownership;
- whether the placement is virtual, if that capability is enabled later.

The store is not a movable free-standing UI object. Every operation that can
change terminal rows invokes a corresponding graphics lifecycle operation in
the same ordered `Terminal::apply` path. A placement in v1 is visible-screen
only; retaining an image in scrollback, reflowing image rows or restoring image
state from a prior session is not implied by this decision.

Kitty's virtual placement and Unicode-placeholder model, relative placements,
animation and Sixel placements in retained history require separate follow-up
decisions because they interact with wide-cell width, underline color, reflow
and recovery.

### 2. Add a safe streaming string mode before enabling either protocol

The parser gets distinct APC and DCS sub-states rather than treating every
string as a 4096-byte blob that eventually falls back to Ground. The framing
layer classifies an APC beginning with `G` as a Kitty command and a DCS whose
final byte is `q` with no intermediates as a Sixel candidate without changing
the existing DECRQSS `DCS $ q` path.

An oversized, malformed or disabled graphics string enters bounded
discard-and-resynchronize behavior:

- ST, CAN and SUB terminate the sequence;
- a fresh ESC starts a new recognized control sequence;
- bytes are never printed merely because an inline-graphics limit was reached;
- C0 bytes inside the graphics payload never become terminal cursor actions;
- no unbounded allocation is used. There is no byte-count exit from graphics
  discard back to Ground: that would print the remainder of the rejected
  payload. An oversized sequence continues in constant-memory discard until
  ST, CAN, SUB or a fresh ESC starts another recognized control sequence. A
  work counter may produce bounded diagnostics, but it does not exit the
  protocol state.

This framing hardening is a Phase 0 prerequisite and is required even if the
user never enables a graphics protocol. Existing OSC/DCS behavior, including
DECRQSS, retains its replies and bounds.

The Phase 0 states are terminal-parser state, not renderer state. The changed
overflow semantics receive a new `TerminalSemantics` variant carried through
sessiond, tunnel/mobile snapshots and recording, with explicit old/new adapters
and replay tests.
A snapshot, detach or recording checkpoint taken while graphics framing is
incomplete retains the bounded discard state through that schema/semantics
adapter, or refuses capture honestly; it never restarts the parser from Ground
and reinterprets the next bytes. Phase 0 applies to APC/PM/SOS discard as well as
graphics DCS. `festerm-sessiond` and recording remain
non-graphics-capable, but the old behavior stays reproducible under old
semantics while new captures use the safe behavior.

### 3. Enable Kitty first, narrowly and explicitly

The first image-capable profile is the **Kitty Graphics Protocol**, direct
transport (`t=d`) only. Kitty filesystem (`t=f`), temporary-file (`t=t`) and
shared-memory (`t=s`) transports are permanently outside this slice and are
rejected without any filesystem, shared-memory or device API call. The error
must not distinguish the reason or reveal anything about the local filesystem.

Direct mode supports:

- chunked APC `ESC _ G ... ESC \` framing;
- unique `i` image identity plus non-unique `I` image numbers, with
  terminal-owned allocation of `i` when `I` is created and newest-number
  resolution for later references;
- PNG (`f=100`), RGB (`f=24`) and RGBA (`f=32`);
- bounded zlib (`o=z`) only when the expected size is supplied and pre-admitted;
- query (`a=q`), transmit, transmit-and-place, place, delete and supported
  feature-query actions;
- deterministic z-order, same-ID replacement and fixed `OK`/error replies;
- cell-scaled placements and source rectangles.

Animation, relative placements, Unicode placeholders, virtual placements and
non-direct transmission are deferred. A feature query reports only the exact subset above.

Image-data replacement and placement replacement are separate: retransmitting
data for an existing image removes its placements and displays no new pixels
until a placement command; placing the same `(image_id, placement_id)` replaces
only that placement. Supplying both `i` and `I` is invalid. A response is sent
only when `i` or `I` was supplied and allowed by the quiet flags: `q=1`
suppress only `OK`, `q=2` suppresses all responses, an `I` response includes
the allocated or resolved `i`, and responses echo `p` on both success and
error. A later chunk's quiet flag controls the response for that command.
Anonymous `a=T` receives no response. A Kitty `a=q` query validates the
requested operation and formats without retaining or replacing image data; it
does not mutate quota, IDs or the screen.

Multipart chunks are staged without blocking the final chunk or unrelated
control-path admission. Once a complete command arrives, the protocol-visible
decision is synchronous with terminal mutation. Further PTY reads are
backpressured at a high-water mark while bounded resumable admission, header
validation and the decode required for a valid reply finish. Success or error
depends only on byte-deterministic inputs, dimensions, quotas and
operation/work counters; temporary queue occupancy or elapsed worker time never
selects `OK` versus error. Cursor movement, image ID allocation, quota
eviction, placement extent, reply and subsequent text become visible in one
ordered commit. Only texture materialization and upload are render-side. A
failed command never consumes an ID, evicts another image, produces a
placement or moves the cursor.

An incomplete chunked upload is aborted, without publishing state, when a
delete or state-mutating graphics command interleaves, the screen resets or the
session closes. A feature query has no effect on the upload in progress. A
continuation chunk is bounded and, except for the final chunk, carries only
permitted continuation data and a base64 length divisible by four.

For PNG, size must be checked from the IHDR before allocation and PNG
structure must be validated before an `OK` result. Raw RGB(A) byte count must
equal `width × height × bytes_per_pixel`. Decoder and inflation limits are
checked with `checked_mul`/`checked_add`; decoder timing or a worker's return
latency never changes protocol semantics.

### 4. Require trusted pixel geometry before advertising Kitty

fesTerm's session and PTY resize types already carry optional terminal pixel
dimensions. The image-capable path defines those as terminal-grid device-pixel
width and height, derives each cell dimension as
`max(1, round(points_per_cell * pixels_per_point))`, and records those values
with the authoritative rows/columns resize generation. Missing, zero or
internally inconsistent geometry refuses a
graphics placement; it is never inferred from outer-window dimensions or an
optional renderer texture. `CSI 14 t` and `CSI 16 t` are added with the same
geometry and use the protocol-defined height-then-width replies. A DPI-only
change is a geometry event even when rows and columns do not change. Each
resize request carries a generation that the local PTY echoes in its
acknowledgement. Geometry is published only for the acknowledgement of the
newest generation, when that request is also the newest requested size and
matches the terminal's current grid. An untagged, stale, or superseded
acknowledgement withholds geometry until the matching generation applies, so a
delayed acknowledgement of an earlier request for the same size (A-B-A) never
matches. The local PTY never drops the newest acknowledgement because its
event queue is full. While a final graphics reply waits for write capacity,
the application retries on a short timer, because no later output may arrive
to wake it.

Kitty detection continues to use the protocol's graphics query plus DA1
sequence. fesTerm does not advertise Kitty by changing `TERM` alone, and Kitty
support does not add DA1 attribute `4`, which advertises Sixel. When graphics
are disabled or geometry is unavailable, fesTerm sends no graphics reply and
retains the conservative DA reply. When enabled, the exact Kitty feature-query
reply must be tested against real clients; a momentary texture-budget or decode
failure never changes terminal identity. The graphics query response is ordered
before the following DA1 sentinel; a late response is not a capability answer.

Client cell spans and cursor movement follow verified Kitty behavior: a normal
placement advances the cursor by the effective columns and by `rows - 1`, then
wraps when x reaches the screen width and scrolls by every line needed to leave
the bottom margin. This is a single ordered commit, not one fixed scroll or a
pending-wrap emulation. `C=1`, virtual and relative placements do not move the
cursor.

### 5. Limit, reserve and evict deterministically

Initial platform-neutral ceilings for the Kitty slice are:

- one in-flight direct upload per terminal buffer;
- 4096 base64 bytes per non-final client chunk, with intermediate chunks a
  multiple of four, and a bounded final chunk;
- 4 MiB maximum admitted image data after base64 decode and before
  protocol-specific decompression expansion; decompression is bounded by the
  expected decoded byte count and the 64 MiB terminal reservation;
- 8192 pixels on either axis and at most 16,777,216 pixels per image;
- 64 MiB decoded RGBA per terminal and a 256 MiB aggregate application
  reservation across terminal graphics;
- 256 live placements and 64 live images per buffer;
- bounded control data, key count, placement extent and repeat/iteration counts,
  with no wall-clock-based acceptance.

These values are protocol admission ceilings, not a promise of available
memory. At eligible-session creation the app assigns a fixed per-session
protocol byte reservation inside a separately reserved terminal-graphics pool.
Only that fixed per-session reservation and deterministic image, placement and
work counters select acceptance or eviction. Global image-memory pressure from
Markdown, another terminal or a preference change affects only render-side
texture materialization; it never changes a protocol reply, ID or placement.
The implementation reserves decoded and staging charges before allocation.
A larger-than-cap image is refused with a fixed protocol error. Within the
cap, deterministic quota eviction is: unreferenced transient (`N=1`) images,
then other unreferenced images, then the oldest referenced image. Render-side
texture-cache eviction is independent and never changes image IDs, cursor state
or protocol replies.

The ceilings may be lowered for mobile and measured upward only through a
separate acceptance change. High-volume decode or paint work participates in
ADR 0079's ingest budget and the app's existing image-memory budget rather than
blocking the GUI frame.

### 6. Define erase, scroll, resize and reset behavior

The Kitty v1 compatibility rule is:

- ED 0 and ED 1, EL, ECH and ordinary character writes do not delete images;
- ED 2 clears visible placements;
- ED 3 clears retained scrollback graphics, which is a no-op in v1 because
  visible-only placements are not retained;
- scroll up/down moves a placement only when it is wholly inside the effective
  vertical and horizontal margins; partially crossed content is source-rect
  clipped and a placement is deleted only after its last visible row leaves;
- insert/delete-line and rectangle text operations do not become image-moving
  operations;
- a placement is clipped while later rows remain visible and is released only
  after its last visible row leaves; a terminal reflow that changes the covered
  visible-row mapping evicts the affected placements rather than inventing
  image pixels in another row;
- alternate-screen entry/exit follows the corresponding text-buffer lifecycle:
  `?47` retains, while `?1047` and `?1049` clear where the selected buffer is
  cleared;
- RIS clears visible and alternate placements, while any future
  scrollback-retained placement policy is decided separately;
- a resize, font zoom or cell-pixel change clips or evicts placements it cannot
  map exactly. It does not reflow image pixels through the renderer and then
  leave core hit-test/overwrite geometry stale.

The render order is: the protocol's under-background layer, cell background,
the between-background-and-text layer, text, the above-text layer, selection,
cursor and native overlays. Equal-z placements use the lower image ID as the
first order. A placement is clipped to the terminal viewport and cannot paint
over chrome, other panes or native dialogs.
Only a non-transparent pixel in the above-text layer occludes interaction.
In that covered area ordinary and rectangle Copy omit hidden cell text, smart
selection cannot select through it and the hyperlink cannot activate. Text
drawn over below-text or background layers remains selectable and clickable
exactly as before. v1 exposes only a bounded “image” accessibility label.
Ordinary terminal Copy and OSC 52 still never export image bytes. A separate
explicit, user-initiated image action is allowed: right-clicking a
non-transparent pixel of a drawable Kitty placement captures the topmost hit
in z/image-ID paint order and offers **Copy image** and **Save image…**. Copy
image uses the platform image clipboard; Save image uses fesTerm's
application-owned destination picker and writes a PNG. Both operate on a
frozen copy of the exact decoded source image, not a terminal screenshot and
not a placement crop; transparent hits do not offer image actions. The
protocol, PTY and OSC 52 never receive a read, copy, filename or save request.
The action remains bounded by the renderer's admitted decoded-image
reservation; the context-menu hit does not retain or decode pixels on the UI
thread. An explicit Copy or confirmed Save freezes the validated source
reference and decodes it once on an application-owned worker.

### 7. Restrict v1 lifetime to local, non-recorded, non-recovering sessions

V1 enables Kitty only for an opted-in in-process Unix local PTY backend on
macOS or Linux. Windows ConPTY remains ineligible until a pinned sidecar or
supported inbox runtime proves that APC bytes pass through and that cursor
state resynchronizes after graphics commands. Kitty graphics are disabled for:

- `festerm-sessiond` and any recovery/mirror/tunnel session;
- synchronized terminal recording;
- SSH, Windows local PTY/ConPTY, serial, PowerShell, JavaScript-program and
  multiplexer/provider backends, including tmux and screen recovery;
- sessions whose authoritative terminal can be adopted, moved or reconstructed
  without the exact graphics store and pending protocol state;
- profiles/sessions that have not explicitly opted in.

Eligibility is fixed at session creation. A request that would start
synchronized recording for an eligible graphics session is refused before it
creates or enrolls a recording; recording cannot later turn a graphics-enabled
session into an ineligible one or silently clear its store.
Adoption, migration and authoritative recovery are not performed for a
graphics-enabled v1 session.

This restriction is deliberate. `festerm-sessiond` owns replies while attached
and detached, so an app-only image store can answer `OK` while the adopted
frontend has no pixels. A graphics-disabled session may still be recorded, but
its graphics bytes are consumed as inert protocol payload and must not become
active merely because replay has a newer decoder. A future graphics-recording
decision has to version that raw-byte interpretation explicitly. Mobile and
tunnel paths remain disabled until pixel geometry, snapshot, transport and
privacy behavior are separately qualified.

Recovery, recording and synchronization are enablement prerequisites, not
implementation details. A future slice must either synchronize encoded image
state and pending parser state in the authoritative snapshot, or retire the
store on both sides in one generation so that a reference returns `ENOENT`
honestly. It must separately define recording consent and replay of graphics
bytes.

### 8. Keep Sixel disabled and define a separate compatibility profile

Sixel is not enabled by this ADR. A future Sixel profile intended to match
modern clients explicitly adopts the xterm/foot/Windows Terminal DECSDM
dialect: `?80h` is page-origin display without scroll/cursor movement and
`?80l` restores inline scrolling. The DEC hardware wording has the opposite
polarity and must be treated as a different profile.

A future Sixel slice must:

- use a streaming DCS decoder outside the ordinary 4096-byte string buffer;
- commit pixels, cursor and scroll effects atomically at ST;
- choose and document the xterm/foot/Windows Terminal DECSDM dialect rather than
  silently mixing DEC hardware and xterm mode polarity;
- support bounded raster attributes, repeats, 256-register private palettes,
  explicit HLS conversion and a fixed shared/private palette policy;
- implement XTSMGRAPHICS and DA attribute `4` only when the rest of the
  compatibility profile is complete, distinguishing current from maximum
  geometry and using the protocol's width-before-height geometry reply;
- make `?1070`/`?8452` and their DECRQM replies explicit, including private
  registers and cursor-right behavior;
- pin the DEC hue origin, RGB/HLS rounding and the default palette with golden
  fixtures;
- remain visible-screen only until history/reflow ownership is independently
  reviewed;
- satisfy the same memory, ingest, recording and recovery gates as Kitty.

No terminal identifies itself as Sixel-capable merely because the framing parser
can discard a Sixel stream. The shared graphics store can be reused, but the
wire decoder and its compatibility decisions require a follow-up acceptance
change or amendment to this ADR.

### 9. Delivery sequence

1. **Phase 0 — framing safety:** add bounded APC/DCS subtype streaming and
   discard-and-resynchronize behavior. Ship regression tests for oversized,
   malformed and split-terminator inputs with graphics disabled. A schema or
   adapter change preserves the new parser state; recording and recovery tests
   cover split graphics strings.
2. **Phase 1 — geometry and local Kitty authority:** add pixel dimensions to
   local PTY/session state; implement the deterministic direct-transmission
   store, replies, placement/cursor/erase semantics and a bounded decode
   barrier on an eligible non-recording local PTY. This remains an
   unadvertised developer slice until renderer qualification completes: no
   shipped profile can receive a successful capability reply.
3. **Phase 2 — rendering and usability:** add bounded textures, clipping and
   z-layers; qualify real clients, DPI/zoom, selection, copy and native
   software/native painter fallback. Only this phase may advertise Kitty
   support to a user who opted in.
4. **Phase 3 — authority expansion:** consider sessiond/tunnel/mobile only
   after snapshot ownership, byte/reply determinism, recovery and privacy are
   solved end-to-end.
5. **Later proposals:** placeholders, relative placements, animation,
   scrollback-retained graphics and Sixel.

## Alternatives considered

- **Implement Kitty and Sixel together.** Rejected for the first slice. Their
  framing, palettes, placement and capability semantics differ, and one decoder
  would obscure which compatibility contract failed.
- **Treat graphics as renderer decorations only.** Rejected because image
  placement changes cursor movement, scroll behavior, capability replies and
  recovery. Presentation cannot be the protocol authority.
- **Put image references on `Cell`.** Rejected because it expands the frozen
  cell recovery charge, forces every scrollback operation to understand
  textures, and makes one spanning image repeat mutable metadata across cells.
- **Accept Kitty file/temp/shared-memory media behind a “local session” check.**
  Rejected. The producing process and terminal do not reliably share a
  filesystem, a successful read leaks an existence/size oracle and malformed
  temp/shared-memory names create delete authority that is unrelated to terminal
  rendering.
- **Keep images in sessiond snapshots or raw recordings automatically.**
  Rejected for v1. It would consume unbounded sensitive data and leave protocol,
  parser, frontend and replay state coupled without a qualified synchronized
  schema.
- **Enable Unicode placeholders only.** Deferred. Placeholders fit text
  reflow better, but most Kitty clients use ordinary placements, and underline
  color, private-use width and multi-placement identity still require explicit
  support.
- **Use a single unqualified 4096-byte DCS cap.** Rejected. It prints trailing
  graphics bytes as terminal text in the current parser and is both
  incompatible and unsafe.
- **Replace the terminal engine with a library that already supports
  graphics.** Rejected. It would bypass fesTerm's authoritative parser,
  recovery, input, privacy and native-rendering decisions.

## Consequences

Kitty v1 can reach useful real-client interoperability without granting a
filesystem or shared-memory authority. The direct protocol remains bounded,
but fesTerm withholds it from SSH, mirrored and recovered sessions until the
authoritative daemon and replay/snapshot rules are ready.

The terminal state gains a distinct graphics side table and per-buffer quota.
Parser, recovery, dirty-row, selection, resize and renderer code need coordinated
tests. Pixels are charged independently from text scrollback because the text
byte and visual resource budgets are different.

The first implementation cannot claim Sixel, Kitty animation, placeholders,
relative placements, image recording, image recovery or file transport. The
UI keeps a visible disabled/default-off policy and must disclose when a session
has graphics enabled.

No original image source, decoded pixels, filesystem name, shared-memory name
or control payload is written to logs. If a future recording design retains
graphics, it requires a separate privacy/consent ADR and schema.

## Validation impact

- **Invariants introduced or changed:** terminal-protocol authority remains in
  `festerm-core`; image state is side-owned and never added to `Cell`; every
  protocol-visible image outcome is ordered with terminal input, cursor and
  replies; direct Kitty transport is the only approved medium; pixel geometry
  is a first-class terminal fact; visible placements are clipped to the terminal
  surface; sessiond/tunnel/recorded sessions remain graphics-disabled in v1.
- **GUI/action edges affected:** `GFX-01`, `GFX-02` and `GFX-03`. Explicit
  **Copy image** and **Save image…** actions are approved only for a
  user-selected visible decoded placement; automatic export, OSC 52 image
  export and terminal screenshots are not approved.
- **Automated tests required:** the final implementation must add
  `oversize_graphics_dcs_never_becomes_text_or_executes_c0`,
  `graphics_discard_state_replays_and_reattaches_mid_sequence`,
  `kitty_chunking_aborts_cleanly_at_st_can_sub_and_fresh_escape`,
  `kitty_direct_png_rgba_raw_dimensions_are_exact_and_bounded`,
  `kitty_zlib_respects_expected_size_without_late_allocation`,
  `kitty_query_reply_precedes_following_da1_and_never_echoes_payload`,
  `kitty_ids_replacements_delete_and_quota_eviction_are_deterministic`,
  `kitty_placement_cursor_erase_scroll_reset_and_margins_match_policy`,
  `kitty_file_temp_shared_memory_never_reach_host_apis`,
  `kitty_graphics_budget_refuses_before_decode_or_texture_allocation`,
  `graphics_pixel_geometry_reaches_pty_core_and_replies`,
  `legacy_string_overflow_replays_old_artifacts_under_legacy_semantics`,
  `new_graphics_framing_semantics_replays_and_reattaches_only_the_new_schema`,
  `new_graphics_semantics_negotiates_tunnel_and_mobile_snapshot_versions`,
  `fixed_protocol_budget_is_independent_of_other_sessions_and_markdown_images`,
  `exported_png_round_trips_the_exact_decoded_pixels`,
  `image_hit_prefers_topmost_non_transparent_drawable_placement`,
  `terminal_context_menu_copies_and_saves_the_hit_kitty_image_only`,
  `terminal_image_save_writes_exact_png_through_confirmed_destination`,
  `transparent_kitty_pixel_does_not_offer_image_export`,
  renderer clipping/z-order/texture-LRU tests and native painter fallback tests.
- **Native/manual evidence required:** physical macOS and Linux local PTY tests
  with real Kitty-protocol clients; a Windows refusal test plus a matched
  inbox-versus-pinned ConPTY qualification before Windows can become eligible;
  DPI/zoom/cell-geometry checks; high-output and memory-pressure evidence;
  recording and recovery refusal evidence; and terminal
  accessibility/selection evidence. A real Sixel client is not acceptance
  evidence for this Kitty slice.
- **Coverage superseded:** Phase 0 supersedes the documented assertion that an
  over-limit APC/DCS should return to Ground and expose its tail. The old
  behavior remains replayable only under legacy terminal semantics; the existing
  inline-graphics deferred classification otherwise remains. This ADR does not
  mark either protocol passing.

## References

- [Kitty Graphics Protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [DEC VT330/VT340 Programmer Reference, Sixel](https://vt100.net/docs/vt3xx-gp/chapter14.html)
- [xterm control sequences: Sixel Graphics and XTSMGRAPHICS](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html)
- [RUSTSEC-2026-0430: Rio terminal graphics protocol file handling](https://rustsec.org/advisories/RUSTSEC-2026-0430.html)
- [libsixel integer-overflow advisory](https://github.com/saitoha/libsixel/security/advisories/GHSA-9jm7-77gr-qghv)
- [Windows Terminal Sixel parser](https://github.com/microsoft/terminal/blob/main/src/terminal/adapter/SixelParser.cpp)
- [foot Sixel implementation](https://codeberg.org/dnkl/foot/src/branch/master/sixel.c)

Current fesTerm source anchors for implementation planning:
`crates/festerm-core/src/parser.rs`,
`crates/festerm-core/src/terminal.rs`,
`crates/festerm-core/src/history.rs`,
`crates/festerm-ui-egui/src/renderer.rs`,
`app/festerm/src/markdown_images.rs`, and
`docs/adr/0038-sessiond-terminal-recovery-snapshots.md`.
