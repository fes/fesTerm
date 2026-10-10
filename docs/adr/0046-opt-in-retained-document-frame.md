# ADR 0046: Retained Document Frame

- **Status:** Proposed (owner-authorized qualified-route rollout; independent architectural review pending)
- **Date:** 2026-10-10
- **Supersedes:** None; extends ADR 0041 without changing terminal-prefix policy

## Context

Owned, source-pinned Windows x64 WARP probes of a 200-line text editor and
an eight-section Markdown document painted 41 frames during each 30-second
quiet interval. They consumed 46.97 and 28.16 process CPU seconds respectively
on a 16-logical-CPU machine. UI construction and host paint return took about
1-2 ms per frame; host return is not completion of software rasterization.
These measurements do not reproduce or explain the reported near-zero-CPU
interaction freeze.

ADR 0041 only retains the prefix before an eligible final terminal-image copy.
Document views do not supply that callback. Their unchanged pixels are
rasterized again when the existing freshness/autosave poll requests a repaint.
Removing the poll would compromise the document contract; retaining arbitrary
callbacks or swap-chain contents would compromise rendering correctness.
The owner authorized a bounded, default-off whole-frame experiment.

## Decision

Complete-frame retention defaults on for the existing Windows x64 DX12
CPU-adapter/BGRA gamma route after successful native painter installation.
`FESTERM_WARP_RETAIN_DOCUMENT_FRAMES=0` explicitly disables it; unset, empty
or `1` requests it. Invalid/non-Unicode values produce an explicit warning and
remain disabled. The owner requested regression qualification followed by
this supported-route rollout. This is not a configuration setting, broader
hardware enablement or architectural acceptance decision. Generic vendor
renderer defaults remain off.

The host still processes input, builds UI, updates textures and buffers, runs
every callback's prepare/finish_prepare, acquires a target, submits and
presents every requested frame. Freshness, autosave and caret scheduling are
unchanged. The prototype also covers eligible surrounding non-terminal root
controls; there is no widget-specific policy or second application command path.

An opaque root surface without MSAA/depth and with COPY_DST support may use the
same one-image cache as ADR 0041 when there is no eligible final terminal copy.
All ordered meshes, clips, callback viewports and immutable keys, managed-texture
generation, renderer identity, physical size, scale, target format and clear
color must match exactly. Prefix and complete-frame signatures are distinct.
Unknown callbacks, user textures, externally exposed/rebound managed textures,
and any callback offering a terminal/image copy decline complete-frame reuse.
The existing terminal-prefix path takes precedence and retains its policy.

On a miss, paint the complete frame into a new immutable private texture. On a
hit, copy the entire previously painted image. Do not retain swap-chain pixels
or skip presentation. The cache-owned limits remain 16,777,216 pixels (64 MiB
at four bytes per pixel) and 1 MiB of exact paint signatures, shared between
both modes. Candidate signatures/images and queued GPU references can coexist
with the current image; these are not total-process or peak-allocation bounds.
Disabling, ineligibility, surface changes, failed acquisition, viewport
retirement and destruction discard the image. Screenshot targets use the same
validated route. Device/allocation errors retain the existing wgpu error policy.

## Alternatives considered

- Remove or slow freshness polling: changes correctness instead of addressing
  repeated rasterization.
- Only optimize widget backgrounds: useful, but the editor outer-frame repair
  is already present and cannot eliminate unchanged full-frame rasterization.
- Cache by hashes or callback pointer identity: neither proves exact pixels or
  immutable resources.
- Add a second document renderer/cache: unnecessary ownership and memory
  duplication; reuse the existing bounded cache instead.
- Enable without qualifying changing-frame and lifecycle controls: rejected.
  The owner separately authorized supported-route enablement after regression
  qualification; independent architectural review remains required before merge.

## Consequences

Misses add an image copy and may regress changing scenes. Large/ineligible
frames retain ordinary painting. The experiment does not change public input
or document APIs, terminal ownership, dependencies or queues. It adds a narrow
vendor host API and maintenance obligation that requires architectural review.
All platforms must still compile and check the extension.

Native CPU benefit must be measured with paired, balanced-order owned-window
controls using the same frozen executable, fixture, viewport, DPI and adapter.
CPU improvement alone does not prove input-to-display latency, clipboard
delivery, sustained dragging, device recovery or a fix for the original freeze.

### Bounded native result

Windows x64 DX12 WARP controls on 2026-10-10 used the same optimized executable
from `ac56bb491dfa808a72cbf97d6d3d9a70bf0442af`, SHA-256
`E755C711433B98CD0E4DC3611EA8253F8A134688A60BA48F25B12104EDF10B38`.
Eight quiet samples used the same fixture content per surface, a 1504 x 1032
physical client at 200% scale, five-second warmup and 30-second measurement,
without concurrent builds/tests. Paths/header text differed between cases;
this is not a claim of identical whole-window pixels across controls.

Balanced off/on/on/off controls reduced mean process CPU, expressed as a
percentage of the 16-logical-CPU machine, from 9.562% to 0.353% for Edit and
7.532% to 0.236% for Preview (96.3% and 96.9% reductions). Every sample still
painted 41 frames. Each enabled sample recorded 41 complete-frame hits and
zero measured rebuilds, with 6,208,512 cache-image bytes. All eight exited
normally. Per-case values and interaction limits are recorded under
[`CP-18`](../manual-validation.md#adr-0046-document-frame-experiment-cp-18).

Fresh enabled/disabled controls exercised reversible typing, undo, keyboard
selection, caret movement, scroll and Save As Cancel/reopen/Escape in Edit
and Markdown Preview/Split, with normal cleanup. A separate actual PTY-history
workflow published exactly 200 rows / 10,600 LF bytes but required forced
cleanup after window close. The final capture shows the expected Quit
confirmation for its live PTY; the harness did not confirm it, so this is
not evidence of a stuck dialog or failed confirmed shutdown.
A pointer-drag attempt was rejected by the
unchanged exact-cursor guard; no successful sustained-drag result is inferred.
An outline/source-versus-preview section mismatch appeared in both controls,
so the outline click is not precise-navigation acceptance or an attributed
retention regression.

Changing scenes remain expensive; observer-inclusive response timings are not
physical display latency. The original near-zero-CPU freeze was not reproduced.
Independent architectural review and broader native qualification remain open;
the initial result did not itself change the Proposed status or default-off gate.

### Qualified-route rollout

The owner subsequently requested regression checks followed by default
enablement. Same-frozen-executable off/on/on/off controls measured ordinary
16-input workloads at 750-ms cadence, followed by a four-second drain, without
captures, WM_NULL probes or concurrent builds/tests inside the CPU interval.
Fresh Edit controls independently settled focus before CtrlHome. Mean process
CPU fell 49.3% for typing/undo and 21.0% for Preview scrolling; all sixteen
wheels reached the adapter. All eight windows exited normally. An earlier
Edit control started at the wrong cursor position because CtrlHome preceded
focus readiness; that four-case set is excluded, not pooled into the result.

Enabled/disabled native resize, minimize, restore and subsequent scrolling
controls returned to the original geometry and exited normally. Sixteen
paired document-content/status captures matched exactly; path-bearing header
and toolbar pixels were deliberately outside this oracle. The deterministic
resource/target/queued-image regressions remain required.

An opt-in, completed-render all-miss control uses prepared real Edit, Preview
and Split jobs at 200% scale with alternating exact clear color and ABBA
order. Every enabled frame must rebuild, final pixels must match ordinary
painting, and mean CPU/completed-render time must remain within a 10%
regression ceiling for these exact fixtures. It measures allocation/copy
overhead without an idle-hit subsidy, not live input or physical display
latency. Commands, bounded results and remaining prerequisites are in `CP-18`.

### Architectural review disposition

Independent review agrees with this bounded shared-cache design. Acceptance
remains conditional on binding the reported all-miss control to its actual
release test-executable hash, source/tree and clean status, invocation and
retained raw numeric profile/log location. Application hashes from the native
interaction controls identify different executables. CP-18 records this gap;
the ADR remains Proposed until those receipts are supplied or regenerated.
Broader native qualification remains open regardless of this disposition.

## Validation impact

- **Invariants introduced or changed:** One shared bounded immutable image
  can represent an eligible non-terminal complete frame; exact invalidation,
  supported-route default with explicit opt-out, unchanged preparation/input/cadence.
- **GUI/action edges affected:** `EDIT-*`, `MD-*`; no action semantics change.
- **Automated tests required:** `document_retention_defaults_on_only_for_supported_route`,
  `retained_document_frames_preserve_editor_preview_and_split_pixels`,
  `retained_document_frames_invalidate_exact_inputs_and_texture_ownership`,
  `retained_document_frames_decline_targets_callbacks_and_budgets`,
  `retained_document_frames_keep_preparation_and_queued_images`,
  and the existing retained-prefix, editor, Markdown and Save As regressions.
  `profile_document_retention_changing_frames` is supplemental opt-in
  completed-render regression qualification, not deterministic CI timing.
- **Native/manual evidence required:** `CP-18`; paired quiet and interaction
  CPU/cadence/reuse measurements, actual editor and Preview captures, save
  recovery, resize/focus and unsupported-route controls; all-miss qualification
  must retain actual test-executable/source identity and raw numeric evidence.
  Broader mixed-DPI,
  hardware, device-loss, clipboard and physical-latency evidence remains open.
- **Coverage superseded:** None.
