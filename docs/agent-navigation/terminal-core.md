<!-- agent-navigation: {"domain": "terminal-core", "generator": "2", "model": "mai-code-1.1-flash", "sources": "f6d1fbe86a924aab5e62843e0f11751fc64cb0bc9e7677da65a246642f8d700f"} -->
# Terminal core and fixtures

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal core owns VT parsing, screen state, queued input/reply transport, and golden-fixture validation for regressions.

Canonical docs: [tests/fixtures/README.md](../../tests/fixtures/README.md), [docs/standards-and-implementation-notes.md](../../docs/standards-and-implementation-notes.md)

## Parser to output

Use for: A PTY byte stream fails at the parser boundary or reset semantics must be checked

- [`Parser`](../../crates/festerm-core/src/parser.rs) — state machine for CSI/DCS/OSC parsing and recovery
- [`ingest`](../../crates/festerm-core/src/terminal.rs) — ASCII fast path plus parser loop for raw output
- [`reset_to_initial_state`](../../crates/festerm-core/src/terminal.rs) — RIS and direct reset restore fresh visible state

Evidence:
- [`ris_restores_fresh_visible_behavior_after_arbitrary_output`](../../crates/festerm-core/tests/parser_properties.rs) — RIS resets visible state after arbitrary output
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — cursor and grid bounds stay valid under hostile input

Caveats:
- Ground-state ambiguity from raw C1 and UTF-8 bytes is easy to regress.
- CSI parameter overflow and string terminators must reset cleanly.
- Printable ASCII fast-path must not bypass parser state or wrap semantics.

## Input encoding

Use for: Key, paste, focus, or mouse events must become PTY bytes without mutating the queue

- [`InputEvent`](../../crates/festerm-core/src/input.rs) — typed UI contract for keys, paste, focus, and mouse events
- [`prepare_input`](../../crates/festerm-core/src/input.rs) — bounded, inert encoding snapshot for one transport batch
- [`handle_input`](../../crates/festerm-core/src/terminal.rs) — mode-aware encoding and atomic queue admission for typed input
- [`drain_input`](../../crates/festerm-core/src/terminal.rs) — delivers queued bytes and clears the transport queue

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
- [`take_dirty_rows`](../../crates/festerm-core/src/terminal.rs) — publishes per-row redraw state after output or reflow

Evidence:
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — cursor and screen bounds remain valid across arbitrary output

Caveats:
- Two-cell grapheme repairs and continuation cells are easy to break.
- Resize must preserve logical anchors and margins without double-wrap.
- Alternate screen and scrollback state can drift if not restored together.

## Recovery validation

Use for: A live terminal cannot produce a recovery snapshot

- [`validate_recovery_snapshot`](../../crates/festerm-core/src/terminal.rs) — validates buffers, wide cluster ends, and pending wrap edges
- [`recovery_clone`](../../crates/festerm-core/src/terminal.rs) — drops transport queues but preserves visible and parser state
- [`RecordingCheckpoint`](../../crates/festerm-core/src/terminal.rs) — records a bounded visible checkpoint for replay and scrubbing

Evidence:
- [`recovery_snapshots_accept_pending_wrap_in_an_inactive_alternate_buffer`](../../crates/festerm-core/src/lib.rs) — preserves a legitimate armed wrap outside the presented buffer
- [`recovery_snapshots_reject_pending_wrap_inside_a_line_or_cluster`](../../crates/festerm-core/src/lib.rs) — rejects forged interior wrap states

Caveats:
- Pending wrap is valid only outside the active line and cluster boundary.
- Snapshot validation must reject queued replies and input as well as malformed dimensions.
- Graphics-enabled terminals cannot be snapshotted until validation completes.

## Fixture goldens

Use for: Adding a terminal regression or compatibility fix with golden corpus coverage

- [`Fixture`](../../crates/festerm-test-support/src/lib.rs) — golden-corpus specification for grid, cursor, modes, and replies
- [`assert_fixture`](../../crates/festerm-test-support/src/lib.rs) — ingests bytes, applies resize, and compares final terminal state
- [`discover_fixtures`](../../crates/festerm-test-support/src/lib.rs) — locates repository fixtures for CI and local validation

Evidence:
- [`repository_fixtures_are_discovered_and_pass`](../../crates/festerm-test-support/tests/fixtures.rs) — all repository fixtures are discovered and asserted in CI

Caveats:
- Fixture input and replies must stay exact; escapes are easy to misread.
- Dirty rows and cell width are often the only proof a bug was fixed.
- Compatibility assumptions are encoded by the golden corpus across versions.
