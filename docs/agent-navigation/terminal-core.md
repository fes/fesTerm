<!-- agent-navigation: {"domain": "terminal-core", "generator": "2", "model": "mai-code-1.1-flash", "sources": "6fb9564fca9142a2a8dcd9910e3572ec7e59b6300f1fa2dbca87cc877e19a639"} -->
# Terminal core and fixtures

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Core terminal state, parser/queue boundaries, and golden-fixture assertions for a GUI-independent VT terminal model.

Canonical docs: [tests/fixtures/README.md](../../tests/fixtures/README.md), [docs/standards-and-implementation-notes.md](../../docs/standards-and-implementation-notes.md)

## Parser flow

Use for: When untrusted PTY bytes are being diagnosed or a sequence fails at the parser boundary

- [`Parser`](../../crates/festerm-core/src/parser.rs) — state machine for CSI/DCS/OSC and recovery
- [`ingest`](../../crates/festerm-core/src/terminal.rs) — fast path plus parser+apply loop per byte

Evidence:
- [`ris_restores_fresh_visible_behavior_after_arbitrary_output`](../../crates/festerm-core/tests/parser_properties.rs) — arbitrary output cannot corrupt visible state

Caveats:
- Ground-state ambiguity from raw C1 and UTF-8 is easy to regress.
- Sequence payload limits must reset cleanly after overflow.
- CSI parameter bounds and string terminators are easy to violate.

## Input encoding

Use for: When key, paste, focus, or mouse events must become PTY bytes

- [`InputEvent`](../../crates/festerm-core/src/input.rs) — typed UI contract for Key/Paste/Focus/Mouse
- [`PreparedInput`](../../crates/festerm-core/src/input.rs) — bounded batch encoding without mutating input queue
- [`handle_input`](../../crates/festerm-core/src/terminal.rs) — mode-aware encoding and atomic queue admission

Evidence:
- [`prepared_input_matches_core_encoding_without_mutating_the_queue`](../../crates/festerm-core/src/input.rs) — encoding matches terminal modes without touching input queue

Caveats:
- Mode-dependent bytes differ across DECKPAM and DECAWM.
- Bracketed paste must remain atomic or markers remain unclosed.
- Queue overflow must be observable and sticky.

## Screen reflow

Use for: When resizing, wrapping, or scrollback logic is changing

- [`Screen`](../../crates/festerm-core/src/screen.rs) — visible grid, dirty rows, and cell ownership
- [`resize_with_content_positions`](../../crates/festerm-core/src/terminal.rs) — reflow keeps logical anchors stable across resize

Evidence:
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — cursor and grid bounds remain valid across arbitrary output

Caveats:
- Two-cell grapheme repairs and continuation cells are easy to break.
- Resize must preserve logical anchors and margins without double-wrap.
- Alternate screen and scrollback state can drift if not restored together.

## Fixture goldens

Use for: When adding a terminal regression or compatibility fix

- [`assert_fixture`](../../crates/festerm-test-support/src/lib.rs) — loads a fixture, ingests bytes, and checks the final grid

Evidence:
- [`repository_fixtures_are_discovered_and_pass`](../../crates/festerm-test-support/tests/fixtures.rs) — all repo fixtures are discovered and asserted in CI

Caveats:
- Fixture input and replies must stay exact; escapes are easy to misread.
- Dirty rows and cell width are often the only proof a bug was fixed.
- Fixture files encode compatibility assumptions across versions.
