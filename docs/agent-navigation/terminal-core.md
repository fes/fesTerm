<!-- agent-navigation: {"domain": "terminal-core", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f07cea8fce3e4f0a526d43810686a427fd12989545beb31487e4814ffb0d93de"} -->
# Terminal core and fixtures

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal core owns VT parsing and screen state, encodes typed input into a single transport queue, and validates behavior against property and golden-fixture regressions.

Canonical docs: [tests/fixtures/README.md](../../tests/fixtures/README.md), [docs/standards-and-implementation-notes.md](../../docs/standards-and-implementation-notes.md)

## Parser to output

Use for: A PTY byte stream fails at the parser boundary or reset semantics must be checked

- [`Parser`](../../crates/festerm-core/src/parser.rs) — state machine for CSI/DCS/OSC parsing and recovery
- [`ingest`](../../crates/festerm-core/src/terminal.rs) — ASCII fast path plus parser loop for raw output

Evidence:
- [`ris_restores_fresh_visible_behavior_after_arbitrary_output`](../../crates/festerm-core/tests/parser_properties.rs) — RIS resets visible state after arbitrary output
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — cursor and grid bounds remain valid after hostile input

Caveats:
- Ground-state ambiguity from raw C1 and UTF-8 bytes is easy to regress.
- CSI parameter overflow and string terminators must reset cleanly.
- Printable ASCII fast-path must not bypass parser state or wrap semantics.

## Input encoding

Use for: Key, paste, focus, or mouse events must become PTY bytes without mutating the queue

- [`InputEvent`](../../crates/festerm-core/src/input.rs) — typed UI contract for keys, paste, focus, and mouse events
- [`handle_input`](../../crates/festerm-core/src/terminal.rs) — mode-aware encoding and atomic queue admission for typed input

Evidence:
- [`prepared_input_matches_core_encoding_without_mutating_the_queue`](../../crates/festerm-core/src/input.rs) — prepared bytes match terminal modes without touching queued input

Caveats:
- Mode-dependent bytes differ across DECKPAM and DECAWM.
- Bracketed paste must remain atomic or markers remain unclosed.
- Queue overflow must be observable and sticky.

## Screen reflow

Use for: Resizing, wrapping, or scrollback logic is changing

- [`Screen`](../../crates/festerm-core/src/screen.rs) — visible grid, dirty rows, and cell ownership for the presented screen
- [`resize_with_content_positions`](../../crates/festerm-core/src/terminal.rs) — reflows primary screen and preserves scrollback anchors across resize

Evidence:
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — cursor and screen bounds remain valid across arbitrary output

Caveats:
- Two-cell grapheme repairs and continuation cells are easy to break.
- Resize must preserve logical anchors and margins without double-wrap.
- Alternate screen and scrollback state can drift if not restored together.

## Fixture goldens

Use for: Adding a terminal regression or compatibility fix with golden corpus coverage

- [`assert_fixture`](../../crates/festerm-test-support/src/lib.rs) — loads a fixture, ingests bytes, and checks the final grid
- [`discover_fixtures`](../../crates/festerm-test-support/src/lib.rs) — discovers repository fixtures for validation

Evidence:
- [`repository_fixtures_are_discovered_and_pass`](../../crates/festerm-test-support/tests/fixtures.rs) — all repository fixtures are discovered and asserted in CI

Caveats:
- Fixture input and replies must stay exact; escapes are easy to misread.
- Dirty rows and cell width are often the only proof a bug was fixed.
- Compatibility assumptions are encoded by the golden corpus across versions.
