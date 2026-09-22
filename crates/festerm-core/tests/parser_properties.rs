//! Property tests for the escape-sequence parser.
//!
//! The parser consumes untrusted bytes straight off a pseudoterminal: any
//! program a user runs can send it anything at all. Every other test in this
//! crate feeds it sequences a human wrote, which is the opposite of the input
//! that finds panics.
//!
//! These properties are deliberately about *invariants* rather than about
//! specific outputs - what a terminal does with `ESC[999999999999m` is a
//! judgement call, but that it neither panics nor leaves the cursor outside
//! the screen is not.
//!
//! The generators mix random bytes with well-formed sequence shapes on
//! purpose. Uniformly random bytes almost never form a valid CSI, so a purely
//! random generator spends its whole budget on the printable-text path and
//! never reaches the parameter handling that is actually delicate.

use festerm_core::{Color, Dimensions, Terminal};
use proptest::prelude::*;

const COLUMNS: usize = 24;
const ROWS: usize = 6;

fn terminal() -> Terminal {
    Terminal::new(Dimensions::new(COLUMNS, ROWS).expect("dimensions")).expect("terminal")
}

/// Everything that must be true of a terminal no matter what it was fed.
fn assert_invariants(terminal: &Terminal, input: &[u8]) {
    let cursor = terminal.cursor();
    assert!(
        cursor.column() < COLUMNS,
        "cursor column {} escaped a {COLUMNS}-column screen after {input:?}",
        cursor.column()
    );
    assert!(
        cursor.row() < ROWS,
        "cursor row {} escaped a {ROWS}-row screen after {input:?}",
        cursor.row()
    );

    for row in 0..ROWS {
        let text = terminal
            .row_text(row)
            .unwrap_or_else(|| panic!("row {row} vanished after {input:?}"));
        assert!(
            text.chars().count() <= COLUMNS,
            "row {row} holds {} characters on a {COLUMNS}-column screen after {input:?}",
            text.chars().count()
        );
        for column in 0..COLUMNS {
            assert!(
                terminal.cell(column, row).is_some(),
                "cell ({column}, {row}) vanished after {input:?}"
            );
        }
    }
}

/// A CSI sequence with plausible-but-hostile parameters.
///
/// The interesting values are the boundaries: zero, the implicit-default empty
/// parameter, something past the screen size, and something past `u32`.
fn csi_sequence() -> impl Strategy<Value = Vec<u8>> {
    let parameter = prop_oneof![
        Just(String::new()),
        Just("0".to_owned()),
        (1u64..=8).prop_map(|value| value.to_string()),
        (9u64..=1000).prop_map(|value| value.to_string()),
        Just(u32::MAX.to_string()),
        Just("99999999999999999999".to_owned()),
    ];
    let final_byte = prop_oneof![
        Just('m'),
        Just('H'),
        Just('A'),
        Just('B'),
        Just('C'),
        Just('D'),
        Just('J'),
        Just('K'),
        Just('L'),
        Just('M'),
        Just('P'),
        Just('X'),
        Just('r'),
        Just('S'),
        Just('T'),
        Just('@'),
        Just('d'),
        Just('G'),
    ];
    let private = prop_oneof![Just(""), Just("?"), Just(">"), Just("=")];
    let separator = prop_oneof![Just(";"), Just(":")];

    (
        private,
        proptest::collection::vec(parameter, 0..5),
        separator,
        final_byte,
    )
        .prop_map(|(private, parameters, separator, final_byte)| {
            let mut sequence = format!("\x1b[{private}");
            sequence.push_str(&parameters.join(separator));
            sequence.push(final_byte);
            sequence.into_bytes()
        })
}

/// A string sequence, which is where unbounded buffering would show up.
fn string_sequence() -> impl Strategy<Value = Vec<u8>> {
    let introducer = prop_oneof![
        Just(&b"\x1b]"[..]),
        Just(&b"\x1bP"[..]),
        Just(&b"\x1b^"[..]),
        Just(&b"\x1b_"[..]),
    ];
    // Deliberately includes terminators that never arrive.
    let terminator = prop_oneof![
        Just(&b"\x1b\\"[..]),
        Just(&b"\x07"[..]),
        Just(&b""[..]),
        // An ESC inside the payload, which has to abandon the string rather
        // than be swallowed by it.
        Just(&b"\x1b[31m"[..]),
    ];
    (introducer, "[ -~]{0,12}", terminator).prop_map(|(introducer, payload, terminator)| {
        let mut sequence = introducer.to_vec();
        sequence.extend_from_slice(payload.as_bytes());
        sequence.extend_from_slice(terminator);
        sequence
    })
}

/// Bytes that are not valid UTF-8, spliced in where a sequence expects text.
fn invalid_utf8() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(
        prop_oneof![
            Just(0x80u8),
            Just(0xbf),
            Just(0xc0),
            Just(0xc2),
            Just(0xe0),
            Just(0xed),
            Just(0xf0),
            Just(0xf5),
            Just(0xff),
        ],
        1..5,
    )
}

/// A string sequence whose payload runs past `MAX_STRING_BYTES`. The parser
/// has to abandon the payload without abandoning the *sequence*: if it drops
/// back to ground at the limit, everything after it prints as text.
fn oversized_string() -> impl Strategy<Value = Vec<u8>> {
    (
        prop_oneof![
            Just(&b"\x1b]0;"[..]),
            Just(&b"\x1b]8;;"[..]),
            Just(&b"\x1bP"[..]),
            Just(&b"\x1b^"[..]),
        ],
        (festerm_core::MAX_STRING_BYTES)..(festerm_core::MAX_STRING_BYTES + 64),
        prop_oneof![Just(&b"\x1b\\"[..]), Just(&b"\x07"[..]), Just(&b""[..])],
    )
        .prop_map(|(introducer, length, terminator)| {
            let mut sequence = introducer.to_vec();
            sequence.extend(std::iter::repeat_n(b'A', length));
            sequence.extend_from_slice(terminator);
            sequence
        })
}

fn fragment() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => csi_sequence(),
        2 => "[ -~]{0,10}".prop_map(String::into_bytes),
        1 => string_sequence(),
        1 => oversized_string(),
        1 => invalid_utf8(),
        1 => proptest::collection::vec(any::<u8>(), 1..8),
        // The single-byte controls that have to be honoured mid-sequence.
        1 => prop_oneof![
            Just(vec![0x1b]),
            Just(vec![0x18]), // CAN
            Just(vec![0x1a]), // SUB
            Just(vec![0x0d]),
            Just(vec![0x0a]),
            Just(vec![0x08]),
            Just(vec![0x09]),
            Just(vec![0x07]),
            Just(b"\x1b(0".to_vec()),
            Just(b"\x1b(B".to_vec()),
            Just(b"\x1b[?1049h".to_vec()),
            Just(b"\x1b[?1049l".to_vec()),
        ],
    ]
}

fn byte_stream() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(fragment(), 0..12)
        .prop_map(|fragments| fragments.into_iter().flatten().collect())
}

proptest! {
    /// The headline property: nothing a program can send may bring the
    /// terminal down or push its state outside the screen it was given.
    #[test]
    fn arbitrary_input_leaves_the_terminal_in_a_usable_state(input in byte_stream()) {
        let mut terminal = terminal();
        terminal.ingest(&input);
        assert_invariants(&terminal, &input);
    }

    /// Bytes arrive from a pseudoterminal in whatever sized chunks the kernel
    /// feels like. A parser that only works on whole sequences works by luck,
    /// so where the stream is cut must not change where it lands.
    #[test]
    fn where_the_stream_is_cut_does_not_change_where_it_lands(
        input in byte_stream(),
        cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..6),
    ) {
        let mut whole = terminal();
        whole.ingest(&input);

        let mut boundaries: Vec<usize> = cuts
            .iter()
            .map(|cut| cut.index(input.len() + 1))
            .collect();
        boundaries.push(0);
        boundaries.push(input.len());
        boundaries.sort_unstable();
        boundaries.dedup();

        let mut piecemeal = terminal();
        for window in boundaries.windows(2) {
            piecemeal.ingest(&input[window[0]..window[1]]);
        }

        for row in 0..ROWS {
            prop_assert_eq!(
                piecemeal.row_text(row),
                whole.row_text(row),
                "row {} differs when the stream is cut at {:?}",
                row,
                boundaries
            );
        }
        prop_assert_eq!(piecemeal.cursor(), whole.cursor());
    }

    /// Resizing is the ordinary case of a user dragging a window, and it can
    /// happen at any point in a stream rather than politely between sequences.
    #[test]
    fn resizing_part_way_through_keeps_the_terminal_consistent(
        input in byte_stream(),
        cut in any::<prop::sample::Index>(),
        columns in 2usize..40,
        rows in 1usize..12,
    ) {
        let mut terminal = terminal();
        let cut = cut.index(input.len() + 1);
        terminal.ingest(&input[..cut]);
        terminal
            .resize(Dimensions::new(columns, rows).expect("dimensions"))
            .expect("resize");
        terminal.ingest(&input[cut..]);

        let cursor = terminal.cursor();
        prop_assert!(cursor.column() < columns, "cursor column {} escaped {} columns", cursor.column(), columns);
        prop_assert!(cursor.row() < rows, "cursor row {} escaped {} rows", cursor.row(), rows);
        for row in 0..rows {
            prop_assert!(terminal.row_text(row).is_some(), "row {} vanished after a resize", row);
        }
    }

    /// Anything the terminal says back has to be a complete, well-formed
    /// reply. A half-written report is worse than no report: the program that
    /// asked will sit waiting for the rest of it.
    #[test]
    fn every_reply_is_a_complete_sequence(input in byte_stream()) {
        let mut terminal = terminal();
        terminal.ingest(&input);

        let replies = terminal.drain_replies();
        if replies.is_empty() {
            return Ok(());
        }
        prop_assert!(
            replies.iter().all(|byte| *byte != 0),
            "a reply contained a NUL: {replies:?}"
        );
        // Every reply this terminal can emit either starts with ESC or is a
        // bare control answer; none of them may be left unterminated.
        if replies.starts_with(b"\x1bP") {
            prop_assert!(
                replies.ends_with(b"\x1b\\"),
                "a device-control reply was left unterminated: {replies:?}"
            );
        }
    }

    /// An over-long string sequence has to leave the terminal usable. fesTerm
    /// deliberately returns to ground at `MAX_STRING_BYTES` rather than
    /// waiting for a terminator that may never come: a string that is never
    /// terminated would otherwise swallow the rest of the session, and
    /// because `ESC c` would be swallowed with it, not even a reset could
    /// recover. The documented cost is that the remainder of an over-long
    /// payload prints as text. What must never happen is a truncated payload
    /// being *acted on*, or the terminal failing to come back at all.
    #[test]
    fn an_oversized_string_recovers_without_applying_a_truncated_payload(
        introducer in prop_oneof![
            Just(&b"\x1b]0;"[..]),
            Just(&b"\x1b]8;;"[..]),
            Just(&b"\x1bP"[..]),
            Just(&b"\x1b^"[..]),
        ],
        overrun in 0usize..64,
    ) {
        let mut sequence = introducer.to_vec();
        sequence.extend(std::iter::repeat_n(b'A', festerm_core::MAX_STRING_BYTES + overrun));
        sequence.extend_from_slice(b"\x1b\\");

        let mut terminal = terminal();
        terminal.ingest(&sequence);
        // The terminal has to be back in ground: a sequence sent afterwards
        // must be obeyed, not eaten.
        terminal.ingest(b"\x1b[2J\x1b[HX");

        prop_assert_eq!(
            terminal.cell(0, 0).map(|cell| cell.character()),
            Some('X'),
            "the terminal never came back out of the over-long string"
        );
        prop_assert_eq!(
            terminal.title(),
            "",
            "a truncated payload was applied as a title"
        );
    }
}

/// A round trip through the terminal's own report: set a pen, ask what the pen
/// is, feed the answer to a fresh terminal, and ask again. The two reports
/// have to agree, or the report is not a faithful description of the state.
///
/// This is the probe `termstandard/colors` defines, turned into a property.
#[cfg(test)]
mod pen_round_trip {
    use super::*;

    fn report(terminal: &mut Terminal) -> Vec<u8> {
        terminal.ingest(b"\x1bP$qm\x1b\\");
        terminal.drain_replies()
    }

    /// Pulls the SGR parameters out of `DCS 1 $ r <parameters> m ST`.
    fn parameters(report: &[u8]) -> Vec<u8> {
        let body = report
            .strip_prefix(b"\x1bP1$r")
            .unwrap_or_else(|| panic!("not a DECRQSS report: {report:?}"));
        let body = body
            .strip_suffix(b"m\x1b\\")
            .unwrap_or_else(|| panic!("report does not describe SGR: {report:?}"));
        body.to_vec()
    }

    fn pen_sequence() -> impl Strategy<Value = Vec<u8>> {
        let code = prop_oneof![
            (0u32..=9).prop_map(|value| value.to_string()),
            (30u32..=37).prop_map(|value| value.to_string()),
            (40u32..=47).prop_map(|value| value.to_string()),
            (90u32..=97).prop_map(|value| value.to_string()),
            (100u32..=107).prop_map(|value| value.to_string()),
            (0u32..=255).prop_map(|value| format!("38;5;{value}")),
            (0u32..=255).prop_map(|value| format!("48;5;{value}")),
            (0u32..=255, 0u32..=255, 0u32..=255)
                .prop_map(|(red, green, blue)| format!("38;2;{red};{green};{blue}")),
            (0u32..=255, 0u32..=255, 0u32..=255)
                .prop_map(|(red, green, blue)| format!("48:2::{red}:{green}:{blue}")),
        ];
        proptest::collection::vec(code, 1..4)
            .prop_map(|codes| format!("\x1b[{}m", codes.join(";")).into_bytes())
    }

    proptest! {
        #[test]
        fn a_reported_pen_describes_itself_well_enough_to_be_restored(pen in pen_sequence()) {
            let mut original = terminal();
            original.ingest(&pen);
            let first = report(&mut original);

            let mut restored = terminal();
            restored.ingest(b"\x1b[");
            restored.ingest(&parameters(&first));
            restored.ingest(b"m");
            let second = report(&mut restored);

            prop_assert_eq!(
                String::from_utf8_lossy(&first).into_owned(),
                String::from_utf8_lossy(&second).into_owned(),
                "the pen reported after restoring differs from the pen reported before"
            );
        }

        /// The colours themselves have to survive the round trip, not just the
        /// shape of the report.
        #[test]
        fn a_restored_pen_paints_the_same_colours(pen in pen_sequence()) {
            let mut original = terminal();
            original.ingest(&pen);
            let reported = parameters(&report(&mut original));
            original.ingest(b"X");

            let mut restored = terminal();
            restored.ingest(b"\x1b[");
            restored.ingest(&reported);
            restored.ingest(b"mX");

            let before = original.cell(0, 0).expect("printed cell");
            let after = restored.cell(0, 0).expect("printed cell");
            prop_assert_eq!(before.foreground(), after.foreground());
            prop_assert_eq!(before.background(), after.background());
            prop_assert_eq!(before.attributes(), after.attributes());
        }
    }
}

/// Parameters far past what any real program sends must not wrap around into
/// something that looks reasonable. `ESC[99999999999999999999H` should leave
/// the cursor somewhere on the screen, not at row 3 because the value
/// overflowed.
#[test]
fn absurd_parameters_are_clamped_rather_than_wrapped() {
    for sequence in [
        &b"\x1b[99999999999999999999;99999999999999999999H"[..],
        b"\x1b[4294967296H",
        b"\x1b[4294967295A",
        b"\x1b[99999999999999999999X",
        b"\x1b[99999999999999999999@",
        b"\x1b[99999999999999999999L",
        b"\x1b[99999999999999999999P",
        b"\x1b[1;99999999999999999999r",
    ] {
        let mut terminal = terminal();
        terminal.ingest(sequence);
        assert_invariants(&terminal, sequence);
    }
}

/// A parameter list longer than any real sequence must not push the parser
/// into allocating without bound, and the sequence must still terminate.
#[test]
fn an_absurdly_long_parameter_list_still_terminates() {
    let mut sequence = b"\x1b[".to_vec();
    for _ in 0..10_000 {
        sequence.extend_from_slice(b"1;");
    }
    sequence.push(b'm');
    sequence.push(b'X');

    let mut terminal = terminal();
    terminal.ingest(&sequence);

    assert_eq!(
        terminal.cell(0, 0).expect("printed cell").character(),
        'X',
        "the parser never came back out of the parameter list"
    );
}

/// An unterminated string sequence must not swallow the rest of the session.
/// The bound is what guarantees this: at `MAX_STRING_BYTES` the parser gives
/// up on a terminator that may never arrive and returns to ground.
#[test]
fn an_unterminated_string_sequence_does_not_swallow_everything_after_it() {
    let mut sequence = b"\x1b]0;".to_vec();
    sequence.extend(std::iter::repeat_n(b'A', 100_000));

    let mut terminal = terminal();
    terminal.ingest(&sequence);
    terminal.ingest(b"\x1b[2J\x1b[HX");

    assert_eq!(
        terminal.cell(0, 0).expect("printed cell").character(),
        'X',
        "the parser is still inside the string sequence"
    );
    assert_eq!(
        terminal.title(),
        "",
        "an unterminated title was applied anyway"
    );
}

/// `CAN` and `SUB` abandon a sequence in progress. A parser that ignores them
/// stays in the escape state and eats the text that follows.
#[test]
fn cancel_and_substitute_abandon_a_sequence_in_progress() {
    for cancel in [&b"\x18"[..], b"\x1a"] {
        let mut sequence = b"\x1b[31".to_vec();
        sequence.extend_from_slice(cancel);
        sequence.extend_from_slice(b"X");

        let mut terminal = terminal();
        terminal.ingest(&sequence);

        let cell = terminal.cell(0, 0).expect("printed cell");
        assert_eq!(
            cell.character(),
            'X',
            "the text after {cancel:?} was swallowed by the abandoned sequence"
        );
        assert_eq!(
            cell.foreground(),
            Color::Default,
            "the abandoned sequence still applied its colour"
        );
    }
}

/// Line insertion and deletion that names more lines than the scroll region
/// holds. Both clamp the count to the region, and both then computed the
/// last row to copy as `bottom - count`, which underflows to a panic the
/// moment the count covers the whole region. `CSI 6 L` on a six-row screen
/// was enough to take the process down, and any program can send it.
///
/// These are the cases the property generators found; they are pinned here by
/// name so the fix cannot regress quietly.
#[test]
fn editing_more_lines_than_the_region_holds_does_not_panic() {
    for sequence in [
        &b"\x1b[6L"[..],              // insert every line on the screen
        b"\x1b[99L",                  // insert far more than the screen holds
        b"\x1b[6M",                   // delete every line on the screen
        b"\x1b[99M",                  // delete far more than the screen holds
        b"\x1b[6T",                   // scroll down by the whole screen
        b"\x1b[99T",                  // scroll down by more than the whole screen
        b"\x1b[6S",                   // scroll up by the whole screen
        b"\x1b[99S",                  // scroll up by more than the whole screen
        b"\x1b[2;4r\x1b[2;1H\x1b[9L", // and again inside a partial region
        b"\x1b[2;4r\x1b[2;1H\x1b[9T",
    ] {
        let mut terminal = terminal();
        terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
        terminal.ingest(sequence);
        assert_invariants(&terminal, sequence);
    }
}

/// Inserting or deleting a whole region blanks it, rather than leaving the
/// rows it was supposed to have shifted away.
#[test]
fn inserting_a_whole_region_of_lines_blanks_it() {
    let mut terminal = terminal();
    terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    terminal.ingest(b"\x1b[H\x1b[6L");

    for row in 0..ROWS {
        assert_eq!(
            terminal.row_text(row).as_deref().map(str::trim_end),
            Some(""),
            "row {row} survived an insertion that covered the whole screen"
        );
    }
}
