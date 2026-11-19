<!-- agent-navigation: {"domain": "terminal-core", "generator": "2", "model": "mai-code-1.1-flash", "sources": "9f0fa9c5876758fffeac05c236e17fb8c24ee30fad9514d51036d6150d429cea"} -->
# Terminal core and fixtures

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal core owns VT parsing, input transport, screen reflow, and fixture-backed validation of the visible terminal state.

Canonical docs: [tests/fixtures/README.md](../../tests/fixtures/README.md), [docs/standards-and-implementation-notes.md](../../docs/standards-and-implementation-notes.md)

## Parser to output

Use for: A PTY byte stream fails at the parser boundary or reset semantics must be checked

- [`Parser`](../../crates/festerm-core/src/parser.rs) — VT parser state machine for CSI/DCS/OSC decoding and recovery.
- [`TerminalOp`](../../crates/festerm-core/src/parser.rs) — Typed output operations applied to terminal state after parsing.
- [`ingest`](../../crates/festerm-core/src/terminal.rs) — ASCII fast path plus parser loop for raw output bytes.

Evidence:
- [`ris_restores_fresh_visible_behavior_after_arbitrary_output`](../../crates/festerm-core/tests/parser_properties.rs) — RIS resets visible state after arbitrary output.
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — Cursor and grid bounds remain valid after hostile input.

Caveats:
- Ground-state ambiguity from raw C1 and UTF-8 bytes is easy to regress.
- CSI parameter overflow and string terminators must reset cleanly.
- Printable ASCII fast-path must not bypass parser state or wrap semantics.

## Input encoding

Use for: Key, paste, focus, or mouse events must become PTY bytes without mutating the queue

- [`InputEvent`](../../crates/festerm-core/src/input.rs) — Typed UI contract for keys, paste, focus, and mouse events.
- [`PreparedInput`](../../crates/festerm-core/src/input.rs) — Mode-locked byte batch that is inert until admitted to the queue.
- [`handle_input`](../../crates/festerm-core/src/terminal.rs) — Mode-aware encoding and atomic queue admission for typed input.

Evidence:
- [`prepared_input_matches_core_encoding_without_mutating_the_queue`](../../crates/festerm-core/src/input.rs) — Prepared bytes match terminal modes without touching queued input.

Caveats:
- Mode-dependent bytes differ across DECKPAM and DECAWM.
- Bracketed paste must remain atomic or markers remain unclosed.
- Queue overflow must be observable and sticky.
Allocation regression: [`character_input_avoids_temporary_allocations`](../../crates/festerm-core/tests/character_input_allocations.rs) runs its process-wide counter in a single-thread entrypoint; Cargo and nextest retain the named test, two character cases, positive control and unchanged event allocation bound.


## Screen reflow

Use for: Resizing, wrapping, or scrollback logic is changing

- [`Screen`](../../crates/festerm-core/src/screen.rs) — Visible grid, dirty rows, and cell ownership for the presented screen.
- [`resize_with_content_positions`](../../crates/festerm-core/src/terminal.rs) — Reflows primary screen while preserving logical anchors across resize.
- [`resize`](../../crates/festerm-core/src/terminal.rs) — Primary-screen resize with scrollback and alternate-screen handling.

Evidence:
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — Cursor and screen bounds remain valid across arbitrary output.

Caveats:
- Two-cell grapheme repairs and continuation cells are easy to break.
- Resize must preserve logical anchors and margins without double-wrap.
- Alternate screen and scrollback state can drift if not restored together.

## Fixture goldens

Use for: Adding a terminal regression or compatibility fix with golden corpus coverage

- [`assert_fixture`](../../crates/festerm-test-support/src/lib.rs) — Loads a fixture, ingests bytes, and asserts the final grid.
- [`discover_fixtures`](../../crates/festerm-test-support/src/lib.rs) — Discovers repository fixtures for validation and CI coverage.

Evidence:
- [`repository_fixtures_are_discovered_and_pass`](../../crates/festerm-test-support/tests/fixtures.rs) — All repository fixtures are discovered and asserted in CI.

Caveats:
- Fixture input and replies must stay exact; escapes are easy to misread.
- Dirty rows and cell width are often the only proof a bug was fixed.
- Compatibility assumptions are encoded by the golden corpus across versions.
