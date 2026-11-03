# ADR 0056: iOS Shell, Design Tokens and Extended Key Encoding

- **Status:** Accepted
- **Date:** 2026-10-31
- **Related:** ADRs 0031, 0042
- **Scope:** `app/festerm-mobile` host shell; additive `festerm-core` key variants.

## Context

The iOS spike host showed a single terminal with a connection form. The owner
mock-ups define a tabbed shell, terminal chrome, extended keys and connection
sheets.

## Decision

- Colors, radii, spacing, type and shadows live in `theme.rs` and are applied
  globally to egui; reusable widgets live in `widgets.rs`.
- Screens are driven by view-model structs and small traits (`ProfileStore`)
  in `model.rs`, so profile storage, Keychain and SFTP backends can plug in
  without UI changes. The shipped store is in-memory and says so.
- A `Capabilities` set hides features lacking a production path; fixtures and
  `Capabilities::all()` are confined to the gallery and tests.
- Extended keys reuse the core encoder. `festerm-core` gains
  `Key::{Home, End, Insert, Delete, Function(u8)}` with xterm encodings
  including modifiers; Ctrl/Alt remain one-shot.
- The live SSH flow keeps its policies (explicit host trust, password retry,
  no replay); only its presentation moved into shared sheets.
- The optional terminal-user keyboards follow the owner-supplied 2026-11-02
  designs, with Hold symbols and Shell row choices. Live switching to/from
  the built-in keyboard is always available and the latest pick becomes the
  saved startup default. Shared
  one-shot modifiers, local clipboard shortcuts, hide/show, Paste and extended
  keys remain available; layout/control reconciliations are documented in
  `docs/mobile-layout-design.md#optional-terminal-user-keyboards`.
  Following device feedback, switching uses a globe key (and an inline More
  list) rather than an underlined ABC key; the bottom row has a centered,
  expanded Space bar. Every app-owned key shows an enlarged press callout and
  input keys repeat like a hardware keyboard (Return excepted). Hide/show uses
  a plain keyboard icon in the utility strip. Native dictation remains available by switching
  to the built-in keyboard; no remote microphone forwarding is implied.

## Consequences

SFTP/transfer and identity/privacy screens, persistent profiles and native
device validation remain future work. Headless captures are review images, not
acceptance evidence.

## Validation impact

`mobile_editor_fits_measured_phone_tablet_space_and_releases_it`,
`mobile_key_row_and_panel_apply_modifiers_once_through_core_encoding`,
`mobile_memory_profile_store_assigns_unique_ids_and_reports_not_persistent` and
`navigation_and_function_keys_use_xterm_sequences_with_modifiers` cover the
shell, one-shot modifiers and new encodings; the loopback tests cover the
unchanged live SSH policy. Headless captures are non-native review images.

The optional keyboard slice maps to `MOB-02` in `docs/gui-action-graph.md`
and `validation/traceability.json`. Tests distinguish saved defaults from live
selection, exercise hold/repeat cancellation and bounded scheduling, measure
pinned controls across short/narrow layouts, and observe actual backend bytes
for characters, Shift/Ctrl, navigation and function keys. Native IME/hardware
handoff, physical typing and accessibility remain in the manual MOB-02 inventory.
The feedback revision adds press/hold preview assertions and checks Space's
width/centering alongside the real More-menu switching path.
