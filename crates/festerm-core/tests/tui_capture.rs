//! Replays raw pseudoterminal captures of real terminal programs.
//!
//! Every fixture under `fixtures/tui/` is the verbatim byte stream a real
//! program wrote to a 120x40 pty, recorded by `scripts/capture-tui.py`. They
//! exist because hand-written escape-sequence tests only ever contain the
//! sequences we already thought of, which is precisely why the compact-colon
//! SGR gap fixed in #188 reached a release with a green suite behind it.
//!
//! Between them these captures cover entering and leaving the alternate
//! screen, reverse-video status lines and search highlights, scroll regions,
//! `ESC[K` line clears, SGR mouse reporting, bracketed paste, the DEC special
//! graphics charset, 256-colour indexed palettes, and full-screen repaints.
//!
//! **Assertions here must key off structure, not values.** A recording is one
//! moment on one machine: htop's percentages, every PID, the load average and
//! the clock all differ on the next recording. Asserting on those would make
//! this suite a trap for whoever re-records. Asserting that the meter is drawn
//! as a bracketed bar, that the header row is highlighted across the full
//! width, or that the divider is a box-drawing character, does not.
//!
//! Re-record with `scripts/capture-tui.py --all`, and read the diff: the
//! fixture is the evidence these assertions were written against, so replacing
//! it means re-checking them.

use festerm_core::{Attributes, Color, Dimensions, MouseTrackingMode, Terminal};

const COLUMNS: usize = 120;
const ROWS: usize = 40;

const VIM: &[u8] = include_bytes!("fixtures/tui/vim.raw");
const HTOP: &[u8] = include_bytes!("fixtures/tui/htop.raw");
const LESS: &[u8] = include_bytes!("fixtures/tui/less.raw");
const NANO: &[u8] = include_bytes!("fixtures/tui/nano.raw");
const TMUX: &[u8] = include_bytes!("fixtures/tui/tmux.raw");
const FZF: &[u8] = include_bytes!("fixtures/tui/fzf.raw");

const EVERY_CAPTURE: [(&str, &[u8]); 6] = [
    ("vim", VIM),
    ("htop", HTOP),
    ("less", LESS),
    ("nano", NANO),
    ("tmux", TMUX),
    ("fzf", FZF),
];

fn replay(capture: &[u8]) -> Terminal {
    let mut terminal =
        Terminal::new(Dimensions::new(COLUMNS, ROWS).expect("dimensions")).expect("terminal");
    terminal.ingest(capture);
    terminal
}

/// Replays everything up to the point the program starts handing the screen
/// back.
///
/// The interesting state - the status line, the highlighted match, the pane
/// divider - only exists while the program owns the alternate screen. Once it
/// leaves, all of that is gone by design, so most assertions need the prefix
/// rather than the whole stream.
///
/// Cutting at the final `ESC[?1049l` is not enough, because a program empties
/// the screen and turns its modes off *before* it gets there: tmux has already
/// disabled mouse reporting and cleared by then, so the prefix would be a
/// blank screen. The teardown is emitted as one short burst, so the cut is the
/// first of those sequences within the burst at the end of the stream.
fn while_still_on_the_alternate_screen(capture: &[u8]) -> Terminal {
    const LEAVE: &[u8] = b"\x1b[?1049l";
    // Only sequences near the end count: tmux toggles mouse reporting dozens of
    // times mid-session as focus moves between panes.
    const TEARDOWN_BURST: usize = 512;
    const HANDING_BACK: [&[u8]; 6] = [
        LEAVE,
        // The erase comes first in the burst - tmux blanks the screen before
        // it turns the modes off - so without it the prefix is a clean screen
        // and there is nothing left to assert about.
        b"\x1b[2J",
        b"\x1b[?1000l",
        b"\x1b[?1002l",
        b"\x1b[?1006l",
        b"\x1b[?2004l",
    ];

    let leave = capture
        .windows(LEAVE.len())
        .rposition(|window| window == LEAVE)
        .expect("the capture never leaves the alternate screen");
    let burst_begins = leave.saturating_sub(TEARDOWN_BURST);
    let end = HANDING_BACK
        .iter()
        .filter_map(|marker| {
            capture[burst_begins..leave + LEAVE.len()]
                .windows(marker.len())
                .position(|window| window == *marker)
                .map(|offset| burst_begins + offset)
        })
        .min()
        .expect("the teardown burst contains at least the leave sequence");

    let terminal = replay(&capture[..end]);
    assert!(
        terminal.modes().alternate_screen(),
        "the prefix should still be on the alternate screen"
    );
    terminal
}

fn row_text(terminal: &Terminal, row: usize) -> String {
    terminal.row_text(row).unwrap_or_default()
}

/// Finds the one row containing `needle`, failing with the screen attached.
fn row_containing(terminal: &Terminal, needle: &str) -> usize {
    let matches: Vec<usize> = (0..ROWS)
        .filter(|row| row_text(terminal, *row).contains(needle))
        .collect();
    match matches.as_slice() {
        [row] => *row,
        [] => panic!("no row contains {needle:?}\n{}", screen(terminal)),
        rows => panic!("{} rows contain {needle:?}: {rows:?}", rows.len()),
    }
}

fn columns_where(
    terminal: &Terminal,
    row: usize,
    predicate: impl Fn(&festerm_core::Cell) -> bool,
) -> Vec<usize> {
    (0..COLUMNS)
        .filter(|column| terminal.cell_ref(*column, row).is_some_and(&predicate))
        .collect()
}

fn inverse_columns(terminal: &Terminal, row: usize) -> Vec<usize> {
    columns_where(terminal, row, |cell| {
        cell.attributes().contains(Attributes::INVERSE)
    })
}

fn coloured_columns(terminal: &Terminal, row: usize) -> Vec<usize> {
    columns_where(terminal, row, |cell| {
        cell.foreground() != Color::Default || cell.background() != Color::Default
    })
}

fn columns_with_a_background(terminal: &Terminal, row: usize) -> Vec<usize> {
    columns_where(terminal, row, |cell| cell.background() != Color::Default)
}

fn screen(terminal: &Terminal) -> String {
    (0..ROWS)
        .map(|row| format!("{row:>2}|{}\n", row_text(terminal, row).trim_end()))
        .collect()
}

// --- vim ------------------------------------------------------------------

/// vim's status line is drawn by setting reverse video and clearing to the end
/// of the line, so the highlight has to reach the full width even though the
/// text stops short. Getting this wrong leaves a ragged bar.
#[test]
fn vim_draws_its_status_line_in_reverse_video_across_the_full_width() {
    let terminal = while_still_on_the_alternate_screen(VIM);
    let row = row_containing(&terminal, "NOTES.md");

    assert_eq!(
        inverse_columns(&terminal, row).len(),
        COLUMNS,
        "the status line highlight stops short of the right margin:\n{}",
        row_text(&terminal, row)
    );
}

/// The end-of-buffer markers come from the 256-colour palette, and vim paints
/// them by setting the colour once and clearing to end of line - so the colour
/// has to survive the erase, not just the characters before it.
#[test]
fn vim_paints_end_of_buffer_markers_from_the_indexed_palette() {
    let terminal = while_still_on_the_alternate_screen(VIM);

    let marker_row = (0..ROWS)
        .find(|row| row_text(&terminal, *row).starts_with('~'))
        .expect("no end-of-buffer marker on screen");
    let cell = terminal.cell(0, marker_row).expect("the marker cell");

    assert_eq!(cell.character(), '~');
    assert!(
        matches!(cell.foreground(), Color::Indexed(_)),
        "the marker lost its indexed colour: {:?}",
        cell.foreground()
    );
    assert_eq!(
        coloured_columns(&terminal, marker_row).len(),
        COLUMNS,
        "the colour did not survive the clear-to-end-of-line"
    );
}

/// `:set number` puts the line numbers in a gutter, which is the simplest
/// check that the file was drawn at all rather than the screen being blank.
#[test]
fn vim_draws_the_file_with_a_line_number_gutter() {
    let terminal = while_still_on_the_alternate_screen(VIM);
    let row = row_containing(&terminal, "# Nimbus Relay");

    let text = row_text(&terminal, row);
    assert!(
        text.trim_start().starts_with("1 #"),
        "the line number gutter is missing: {text:?}"
    );
}

/// Leaving the alternate screen has to put back what was underneath rather
/// than leaving vim's rendering behind.
#[test]
fn leaving_vim_restores_the_screen_underneath_it() {
    let terminal = replay(VIM);

    assert!(!terminal.modes().alternate_screen());
    let leftovers: Vec<String> = (0..ROWS)
        .map(|row| row_text(&terminal, row).trim_end().to_owned())
        .filter(|text| !text.is_empty())
        .collect();
    assert!(
        leftovers.is_empty(),
        "vim's screen survived its own exit: {leftovers:?}"
    );
}

// --- htop -----------------------------------------------------------------

/// The meters are the point of htop: a bracketed bar whose fill is coloured
/// and whose label is not.
#[test]
fn htop_draws_bracketed_meter_bars_with_colour() {
    let terminal = while_still_on_the_alternate_screen(HTOP);
    let row = row_containing(&terminal, "Tasks:");

    let text = row_text(&terminal, row);
    assert!(
        text.contains('[') && text.contains("%]"),
        "the meter is not drawn as a bracketed percentage bar: {text:?}"
    );
    assert!(
        coloured_columns(&terminal, row).len() > 20,
        "the meter bar lost its colour"
    );
}

/// The process header is a full-width highlighted bar, drawn the same way as
/// vim's status line but with a background colour rather than reverse video.
#[test]
fn htop_highlights_its_column_header_across_the_full_width() {
    let terminal = while_still_on_the_alternate_screen(HTOP);
    let row = row_containing(&terminal, "Command");

    assert_eq!(
        columns_with_a_background(&terminal, row).len(),
        COLUMNS,
        "the column header background stops short:\n{}",
        row_text(&terminal, row)
    );
}

/// Tree view joins processes with box-drawing characters. The capture is
/// pinned to a single process we started ourselves, so the tree is one row.
#[test]
fn htop_tree_view_joins_processes_with_box_drawing_characters() {
    let terminal = while_still_on_the_alternate_screen(HTOP);
    let row = row_containing(&terminal, "sleep 120");

    let text = row_text(&terminal, row);
    assert!(
        text.contains('\u{2500}') || text.contains('\u{251c}') || text.contains('\u{2502}'),
        "the process tree drew no box characters: {text:?}"
    );
}

// --- less -----------------------------------------------------------------

/// A search highlight has to cover the match and nothing else. This is the
/// assertion that would have caught a background bleeding past its span.
#[test]
fn a_less_search_highlights_the_match_and_only_the_match() {
    let terminal = while_still_on_the_alternate_screen(LESS);
    const NEEDLE: &str = "BACKPRESSURE";

    let row = (0..ROWS)
        .find(|row| {
            row_text(&terminal, *row).contains(NEEDLE)
                && !inverse_columns(&terminal, *row).is_empty()
        })
        .unwrap_or_else(|| panic!("no highlighted match on screen:\n{}", screen(&terminal)));

    let text = row_text(&terminal, row);
    let start = text.find(NEEDLE).expect("the match is on this row");
    let highlighted = inverse_columns(&terminal, row);

    assert_eq!(
        highlighted,
        (start..start + NEEDLE.len()).collect::<Vec<_>>(),
        "the highlight does not line up with {NEEDLE:?} in {text:?}"
    );
}

#[test]
fn leaving_less_restores_the_screen_underneath_it() {
    let terminal = replay(LESS);
    assert!(!terminal.modes().alternate_screen());
}

// --- nano -----------------------------------------------------------------

/// The title bar is full-width reverse video, and the file name has to be in
/// it - which also proves the editor opened the file rather than an argument.
#[test]
fn nano_draws_a_full_width_reverse_video_title_bar() {
    let terminal = while_still_on_the_alternate_screen(NANO);
    let row = row_containing(&terminal, "NOTES.md");

    assert_eq!(
        inverse_columns(&terminal, row).len(),
        COLUMNS,
        "the title bar highlight stops short:\n{}",
        row_text(&terminal, row)
    );
}

/// The save prompt is drawn over the text without disturbing it, which is the
/// ordinary case of a program writing to one line and leaving the rest alone.
#[test]
fn nano_draws_its_modal_prompt_in_reverse_video_over_the_text() {
    let terminal = while_still_on_the_alternate_screen(NANO);
    let prompt = row_containing(&terminal, "Save modified buffer");

    assert!(
        !inverse_columns(&terminal, prompt).is_empty(),
        "the prompt is not highlighted"
    );
    assert!(
        (0..ROWS).any(|row| row_text(&terminal, row).contains("Queue draining is slow")),
        "the prompt wiped out the text underneath it:\n{}",
        screen(&terminal)
    );
}

// --- tmux -----------------------------------------------------------------

/// tmux turns on mouse reporting and bracketed paste on the way in. These are
/// pure mode state rather than anything drawn, and nothing else here covers
/// the SGR mouse encoding being requested.
#[test]
fn tmux_turns_on_sgr_mouse_reporting_and_bracketed_paste() {
    let terminal = while_still_on_the_alternate_screen(TMUX);

    assert_eq!(
        terminal.modes().mouse_tracking(),
        MouseTrackingMode::ButtonMotion
    );
    assert!(
        terminal.modes().sgr_mouse(),
        "SGR mouse encoding was not enabled"
    );
    assert!(terminal.modes().bracketed_paste());
}

/// Recorded in a non-UTF-8 locale, so tmux falls back from UTF-8 box drawing
/// to the DEC special graphics charset: it selects the charset with `ESC(0`
/// and then sends plain ASCII `x`, which has to come out as a vertical line.
/// Nothing else in the suite exercises that translation.
#[test]
fn tmux_draws_pane_dividers_with_the_dec_special_graphics_charset() {
    assert!(
        TMUX.windows(3).any(|window| window == b"\x1b(0"),
        "the capture no longer selects the DEC special graphics charset; \
         re-recording in a UTF-8 locale loses this coverage"
    );

    let terminal = while_still_on_the_alternate_screen(TMUX);
    let dividers = (0..ROWS)
        .filter(|row| row_text(&terminal, *row).contains('\u{2502}'))
        .count();

    assert!(
        dividers > 1,
        "the pane divider was not translated out of the graphics charset:\n{}",
        screen(&terminal)
    );
}

/// tmux draws its prompt by setting a background, writing the text, resetting
/// only the *foreground*, and then clearing to the end of the line. So the
/// fill after the text has to carry the background that is still in the pen
/// while taking the default foreground.
///
/// This is the same class of defect as the inline-code backgrounds lost in
/// #188: a background that does not reach the margin leaves a ragged bar. It
/// is isolated here because the foreground is reset first, so a test that
/// merely asks whether the cell is "coloured" cannot tell the difference.
#[test]
fn tmux_fills_its_prompt_line_to_the_margin_with_the_pen_background() {
    let terminal = while_still_on_the_alternate_screen(TMUX);
    let row = row_containing(&terminal, "kill-window");

    assert_eq!(
        columns_with_a_background(&terminal, row).len(),
        COLUMNS,
        "the prompt background stops where the text does:\n{}",
        row_text(&terminal, row)
    );

    let text = row_text(&terminal, row);
    let past_the_text = text.trim_end().chars().count() + 4;
    let filler = terminal
        .cell(past_the_text, row)
        .expect("a cell past the end of the prompt text");
    assert_eq!(
        filler.background(),
        terminal
            .cell(0, row)
            .expect("the first cell of the prompt")
            .background(),
        "the fill past the text uses a different background from the text"
    );
    assert_eq!(
        filler.foreground(),
        Color::Default,
        "the fill should take the reset foreground, not the one before it"
    );
}

/// On the way out tmux has to be able to put every mode back. A terminal that
/// keeps mouse reporting on after the program exits leaves the user with a
/// shell that cannot be clicked into.
#[test]
fn leaving_tmux_puts_every_mode_back() {
    let terminal = replay(TMUX);

    assert!(!terminal.modes().alternate_screen());
    assert_eq!(terminal.modes().mouse_tracking(), MouseTrackingMode::None);
    assert!(!terminal.modes().sgr_mouse());
    assert!(!terminal.modes().bracketed_paste());
}

// --- fzf ------------------------------------------------------------------

/// fzf redraws the entire list on every keystroke. The capture ends just after
/// the query is cleared, so the full candidate list and an unfiltered counter
/// have to be back on screen.
#[test]
fn fzf_redraws_its_list_and_counter_after_the_query_is_cleared() {
    let terminal = while_still_on_the_alternate_screen(FZF);

    let counter = row_containing(&terminal, "149/149");
    assert!(
        row_text(&terminal, counter).contains('\u{2500}'),
        "the counter's horizontal rule is missing"
    );
    assert!(
        (0..ROWS)
            .filter(|row| row_text(&terminal, *row).contains("festerm-core/src/module_"))
            .count()
            > 30,
        "the candidate list did not come back:\n{}",
        screen(&terminal)
    );

    // fzf picks its colours from the upper part of the 256-colour palette,
    // which is the range a 16-colour fallback would quietly flatten.
    let beyond_the_first_sixteen = (0..ROWS).any(|row| {
        (0..COLUMNS).any(|column| {
            terminal.cell_ref(column, row).is_some_and(|cell| {
                matches!(cell.foreground(), Color::Indexed(index) if index >= 16)
                    || matches!(cell.background(), Color::Indexed(index) if index >= 16)
            })
        })
    });
    assert!(
        beyond_the_first_sixteen,
        "nothing on screen came from the extended palette"
    );
}

/// The selected row is marked with a background that runs past the text.
#[test]
fn fzf_marks_the_selected_row_with_a_background_beyond_the_text() {
    let terminal = while_still_on_the_alternate_screen(FZF);

    let candidates: Vec<usize> = (0..ROWS)
        .filter(|row| row_text(&terminal, *row).contains("module_"))
        .collect();
    let selected = *candidates
        .iter()
        .max_by_key(|row| coloured_columns(&terminal, **row).len())
        .expect("no candidate rows on screen");

    let highlighted = coloured_columns(&terminal, selected).len();
    let label = row_text(&terminal, selected);
    let label_width = label.trim_end_matches([' ', '\u{2502}']).chars().count();

    assert!(
        highlighted >= label_width,
        "the selection highlight does not reach the end of {label:?}: \
         {highlighted} columns for a {label_width}-column label"
    );
    let unselected = candidates
        .iter()
        .map(|row| coloured_columns(&terminal, *row).len())
        .min()
        .expect("no candidate rows on screen");
    assert!(
        highlighted > unselected,
        "every candidate row is highlighted the same, so nothing looks selected"
    );
}

/// fzf writes `ESC[;38;5;108m` - a *leading empty parameter*, meaning an
/// implicit zero, followed by an indexed colour. It is spelled that way
/// several hundred times in this one capture, and no hand-written test in this
/// repository had ever produced the form.
#[test]
fn a_leading_empty_sgr_parameter_resets_before_the_rest_applies() {
    assert!(
        FZF.windows(11).any(|window| window == b"\x1b[;38;5;108"),
        "the capture no longer contains the leading-empty-parameter form"
    );

    let mut terminal = Terminal::new(Dimensions::new(8, 2).expect("dimensions")).expect("terminal");
    terminal.ingest(b"\x1b[1;31mX\x1b[;38;5;108mY");

    let second = terminal.cell(1, 0).expect("the second cell");
    assert_eq!(
        second.foreground(),
        Color::Indexed(108),
        "the leading empty parameter swallowed the colour after it"
    );
    assert!(
        !second.attributes().contains(Attributes::BOLD),
        "the leading empty parameter has to reset the bold set before it"
    );
}

// --- properties that hold for every capture -------------------------------

/// Bytes arrive from a pty in whatever sized chunks the kernel feels like, so
/// a parser that only works on whole sequences works only by luck. Feeding a
/// real session one byte at a time has to land on exactly the same screen.
#[test]
fn a_capture_replays_the_same_whether_it_arrives_whole_or_one_byte_at_a_time() {
    for (name, capture) in EVERY_CAPTURE {
        let whole = replay(capture);

        let mut dribbled =
            Terminal::new(Dimensions::new(COLUMNS, ROWS).expect("dimensions")).expect("terminal");
        for byte in capture {
            dribbled.ingest(&[*byte]);
        }

        for row in 0..ROWS {
            assert_eq!(
                row_text(&dribbled, row),
                row_text(&whole, row),
                "{name}: row {row} differs when the capture is split across reads"
            );
        }
        assert_eq!(dribbled.cursor(), whole.cursor(), "{name}: cursor differs");
        assert_eq!(
            dribbled.modes().alternate_screen(),
            whole.modes().alternate_screen(),
            "{name}: alternate screen state differs"
        );
    }
}

/// The recordings were made at 120x40, but a terminal does not get to choose
/// the window it is given. Replaying into a smaller and a larger screen is a
/// cheap check that nothing indexes off the recorded geometry.
#[test]
fn every_capture_replays_into_a_window_it_was_not_recorded_for() {
    for (name, capture) in EVERY_CAPTURE {
        for (columns, rows) in [(80, 24), (200, 50), (40, 10)] {
            let mut terminal = Terminal::new(Dimensions::new(columns, rows).expect("dimensions"))
                .expect("terminal");
            terminal.ingest(capture);

            let cursor = terminal.cursor();
            assert!(
                cursor.column() < columns && cursor.row() < rows,
                "{name}: cursor {cursor:?} left a {columns}x{rows} screen"
            );
        }
    }
}

/// Resizing mid-stream is the ordinary case of a user dragging a window while
/// a full-screen program is running.
#[test]
fn every_capture_survives_a_resize_halfway_through() {
    for (name, capture) in EVERY_CAPTURE {
        let mut terminal =
            Terminal::new(Dimensions::new(COLUMNS, ROWS).expect("dimensions")).expect("terminal");

        let middle = capture.len() / 2;
        terminal.ingest(&capture[..middle]);
        terminal
            .resize(Dimensions::new(90, 30).expect("dimensions"))
            .unwrap_or_else(|error| panic!("{name}: resize failed: {error}"));
        terminal.ingest(&capture[middle..]);

        let cursor = terminal.cursor();
        assert!(
            cursor.column() < 90 && cursor.row() < 30,
            "{name}: cursor {cursor:?} left the resized screen"
        );
    }
}
