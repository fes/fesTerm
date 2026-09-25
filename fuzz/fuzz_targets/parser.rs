//! Whole-buffer fuzz target for the escape-sequence parser.
//!
//! The parser is fed bytes that any program a user runs can choose freely, so
//! it is the one place in fesTerm where the input is genuinely adversarial.
//! `crates/festerm-core/tests/parser_properties.rs` covers the same surface
//! with a structured generator; this target covers what a generator cannot,
//! which is byte sequences nobody thought to describe.
//!
//! The assertions are the invariants, not the output. What a terminal *does*
//! with a nonsensical sequence is a judgement call; that it neither panics nor
//! leaves its own state outside the screen it was given is not.

#![no_main]

use festerm_core::{Dimensions, Terminal};
use festerm_test_support::replies::terminal_replies_are_complete;
use libfuzzer_sys::fuzz_target;

const COLUMNS: usize = 24;
const ROWS: usize = 6;

fuzz_target!(|data: &[u8]| {
    let dimensions = Dimensions::new(COLUMNS, ROWS).expect("dimensions");
    let mut terminal = Terminal::new(dimensions).expect("terminal");
    terminal.ingest(data);

    let cursor = terminal.cursor();
    assert!(
        cursor.column() < COLUMNS,
        "cursor column escaped the screen"
    );
    assert!(cursor.row() < ROWS, "cursor row escaped the screen");

    for row in 0..ROWS {
        let text = terminal.row_text(row).expect("row vanished");
        assert!(
            text.chars().count() <= COLUMNS,
            "row holds more characters than it has columns"
        );
        for column in 0..COLUMNS {
            assert!(terminal.cell(column, row).is_some(), "cell vanished");
        }
    }

    // A half-written report leaves the program that asked for it waiting
    // forever, so anything we say back has to be complete.
    let replies = terminal.drain_replies();
    assert!(
        terminal_replies_are_complete(&replies),
        "an incomplete or malformed reply was emitted: {replies:?}"
    );
});
