//! Regression coverage against a real GitHub Copilot CLI session.
//!
//! `fixtures/copilot-cli-session.raw` is the verbatim byte stream Copilot CLI
//! wrote to its pseudoterminal during one short session, captured through a
//! `pty.fork()` harness that answered the capability probes Copilot blocks on
//! and set the window to 120x40. It is committed because hand-written SGR
//! cases kept missing how programs really spell things: Copilot emits a
//! combined foreground *and* background in one semicolon-delimited sequence
//! (`ESC[38;2;255;255;255;48;2;38;44;54m`) and closes the span with a reset
//! combined with the body colour (`ESC[0;38;2;211;217;223m`). Its inline code
//! backgrounds were lost in a released build while every synthetic test
//! passed.
//!
//! The capture was reviewed for credentials, tokens, mail addresses and home
//! paths before committing; it contains only the prompt, the reply, and
//! Copilot's own chrome.

use festerm_core::{Color, Dimensions, Terminal};

const CAPTURE: &[u8] = include_bytes!("fixtures/copilot-cli-session.raw");

const CODE_FOREGROUND: Color = Color::Rgb {
    red: 255,
    green: 255,
    blue: 255,
};
const CODE_BACKGROUND: Color = Color::Rgb {
    red: 38,
    green: 44,
    blue: 54,
};

fn replayed() -> Terminal {
    let mut terminal =
        Terminal::new(Dimensions::new(120, 40).expect("dimensions")).expect("terminal");
    terminal.ingest(CAPTURE);
    terminal
}

/// Finds the run of cells carrying the inline-code background, as
/// `(row, first column, last column)`.
fn code_span(terminal: &Terminal) -> (usize, usize, usize) {
    for row in 0..40 {
        let columns: Vec<usize> = (0..120)
            .filter(|column| {
                terminal
                    .cell(*column, row)
                    .is_some_and(|cell| cell.background() == CODE_BACKGROUND)
            })
            .collect();
        if let (Some(first), Some(last)) = (columns.first(), columns.last()) {
            assert_eq!(
                columns.len(),
                last - first + 1,
                "the inline code background is not one unbroken run on row {row}"
            );
            return (row, *first, *last);
        }
    }
    panic!("no inline code background survived the replay");
}

fn text(terminal: &Terminal, row: usize, columns: std::ops::RangeInclusive<usize>) -> String {
    columns
        .map(|column| {
            terminal
                .cell(column, row)
                .expect("cell within the screen")
                .character()
        })
        .collect()
}

#[test]
fn an_inline_code_span_keeps_its_background_through_a_real_session() {
    let terminal = replayed();
    let (row, first, last) = code_span(&terminal);

    // Copilot pads the span with no-break spaces rather than drawing a border,
    // so the background has to reach them or the span looks clipped.
    assert_eq!(
        text(&terminal, row, first..=last),
        "\u{a0}cargo build\u{a0}"
    );

    for column in first..=last {
        let cell = terminal.cell(column, row).expect("cell within the screen");
        assert_eq!(
            cell.foreground(),
            CODE_FOREGROUND,
            "column {column} lost the inline code foreground"
        );
    }
}

/// The span has to *end*, too: the reset that Copilot combines with the body
/// foreground must clear the background rather than smear it across the line.
#[test]
fn the_inline_code_background_does_not_leak_past_the_span() {
    let terminal = replayed();
    let (row, first, last) = code_span(&terminal);

    let before = terminal
        .cell(first - 1, row)
        .expect("cell before the span")
        .background();
    let after = terminal
        .cell(last + 1, row)
        .expect("cell after the span")
        .background();
    assert_eq!(before, Color::Default);
    assert_eq!(after, Color::Default);
}

/// The capture also carries Copilot's own chrome, which exercises the same
/// combined-colour shape outside a code span.
#[test]
fn the_session_replays_without_losing_its_other_colors() {
    let terminal = replayed();

    let mut backgrounds = std::collections::BTreeSet::new();
    let mut foregrounds = std::collections::BTreeSet::new();
    for row in 0..40 {
        for column in 0..120 {
            let Some(cell) = terminal.cell(column, row) else {
                continue;
            };
            if let Color::Rgb { red, green, blue } = cell.background() {
                backgrounds.insert((red, green, blue));
            }
            if let Color::Rgb { red, green, blue } = cell.foreground() {
                foregrounds.insert((red, green, blue));
            }
        }
    }

    // The tab bar's own background, emitted as `ESC[38;2;...;48;2;20;27;34m`.
    assert!(
        backgrounds.contains(&(20, 27, 34)),
        "the tab bar background was dropped: {backgrounds:?}"
    );
    // The body foreground Copilot returns to after every highlighted span.
    assert!(
        foregrounds.contains(&(211, 217, 223)),
        "the body foreground was dropped: {foregrounds:?}"
    );
}
