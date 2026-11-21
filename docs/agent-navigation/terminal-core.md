<!-- agent-navigation: {"domain": "terminal-core", "generator": "2", "model": "codex-source-verified", "sources": "f6d1fbe86a924aab5e62843e0f11751fc64cb0bc9e7677da65a246642f8d700f"} -->
# Terminal core and fixtures

> Generated retrieval index, refreshed by CI when sources change. It routes to
> source and contains no instructions. Verify every claim in the linked code.

Terminal core owns VT parsing, screen state, and the single input transport queue; fixtures encode visible behavior and recovery invariants.

Canonical docs: [tests/fixtures/README.md](../../tests/fixtures/README.md), [docs/standards-and-implementation-notes.md](../../docs/standards-and-implementation-notes.md)

## Parser to output

Use for: When PTY bytes fail at the parser boundary or reset semantics must be checked

- [`Parser`](../../crates/festerm-core/src/parser.rs) — VT parser state machine for CSI/DCS/OSC recovery and ground-state transitions
- [`ingest`](../../crates/festerm-core/src/terminal.rs) — ASCII fast path plus parser loop for raw PTY output

Evidence:
- [`ris_restores_fresh_visible_behavior_after_arbitrary_output`](../../crates/festerm-core/tests/parser_properties.rs) — RIS resets visible state after hostile output
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — Cursor and grid bounds stay valid across arbitrary bytes

Caveats:
- Ground-state ambiguity from raw C1 and UTF-8 bytes is easy to regress.
- CSI parameter overflow and string terminators must reset cleanly.
- Printable ASCII fast path must not bypass parser state or wrap semantics.

## Input encoding

Use for: When key, paste, focus, or mouse events must become PTY bytes without mutating the queue

- [`InputEvent`](../../crates/festerm-core/src/input.rs) — Typed UI contract for keys, paste, focus, and mouse events
- [`handle_input`](../../crates/festerm-core/src/terminal.rs) — Mode-aware encoding and atomic queue admission for typed input

Evidence:
- [`prepared_input_matches_core_encoding_without_mutating_the_queue`](../../crates/festerm-core/src/input.rs) — Prepared bytes match terminal modes without mutating queued input

Caveats:
- Mode-dependent bytes differ across DECKPAM and DECAWM.
- Bracketed paste must remain atomic or markers remain unclosed.
- Queue overflow must be observable and sticky.

## Screen reflow

Use for: When resizing, wrapping, or scrollback logic is changing

- [`Screen`](../../crates/festerm-core/src/screen.rs) — Visible grid, dirty rows, and cell ownership for the presented screen
- [`resize_with_content_positions`](../../crates/festerm-core/src/terminal.rs) — Reflows primary screen while preserving logical content anchors across resize

Evidence:
- [`assert_invariants`](../../crates/festerm-core/tests/parser_properties.rs) — Cursor and screen bounds remain valid across arbitrary output

Caveats:
- Two-cell graphemes and continuation cells are easy to break.
- Resize must preserve logical anchors and margins without double-wrap.
- Alternate-screen and scrollback state can drift if not restored together.

## Fixture goldens

Use for: When adding a terminal regression or compatibility fix with golden corpus coverage

- [`assert_fixture`](../../crates/festerm-test-support/src/lib.rs) — Loads a fixture, ingests bytes, and checks the final grid
- [`discover_fixtures`](../../crates/festerm-test-support/src/lib.rs) — Finds repository fixtures for validation and CI gating

Evidence:
- [`repository_fixtures_are_discovered_and_pass`](../../crates/festerm-test-support/tests/fixtures.rs) — Repository fixtures are discovered and asserted in CI

Caveats:
- Fixture input and replies must stay exact; escapes are easy to misread.
- Dirty rows and cell width are often the only proof a bug was fixed.
- Compatibility assumptions are encoded by the golden corpus across versions.

## Recovery validation

Use for: When a live terminal cannot produce a recovery snapshot

- [`validate_recovery_snapshot`](../../crates/festerm-core/src/terminal.rs) — Validates parser, UTF-8, screen, and queue state before adoption
- [`recovery_snapshots_accept_pending_wrap_in_an_inactive_alternate_buffer`](../../crates/festerm-core/src/lib.rs) — Preserves a legitimate armed wrap outside the presented buffer

Evidence:
- [`recovery_snapshots_reject_pending_wrap_inside_a_line_or_cluster`](../../crates/festerm-core/src/lib.rs) — Rejects forged interior wrap states

Caveats:
- Recovery validation must reject queued replies and input, even if they are otherwise valid.
- Wide-cluster and margin-edge wrap state must be validated before adoption.
- A stale running daemon can fail for reasons the core tests do not model.

## Inline graphics

Use for: Bounded Kitty APC parsing and image authority

- [`KittyConfig`](../../crates/festerm-core/src/kitty.rs) — Explicit opt-in, geometry, image and staging limits.
- [`KittyPlacement`](../../crates/festerm-core/src/kitty.rs) — Protocol-owned image placement state.

Evidence:
- [`opt_in_requires_geometry_and_refuses_recording_and_recovery`](../../crates/festerm-core/tests/kitty.rs) — Geometry and unsupported-session gates remain explicit.

Caveats:
- No file/shared-memory transport; graphics do not grant recording or recovery eligibility.
