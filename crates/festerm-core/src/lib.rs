//! GUI-independent ANSI/VT terminal state primitives.
//!
//! The parser accepts C0 controls plus 7-bit ESC/CSI syntax. The terminal
//! incrementally decodes printable UTF-8 while the parser is in ground state.
//! Raw C1 bytes are deliberately not controls: treating them as such would
//! make UTF-8 continuation bytes ambiguous.

use std::fmt;

mod cell;
mod input;
mod modes;
mod parser;
mod replies;
mod screen;
mod terminal;
mod unicode;

/// The largest screen that the core will allocate.
pub const MAX_CELL_COUNT: usize = 4 * 1024 * 1024;
/// The maximum number of CSI parameters retained by the parser.
pub const MAX_CSI_PARAMETERS: usize = 32;
/// The maximum number of CSI intermediate bytes retained by the parser.
pub const MAX_CSI_INTERMEDIATES: usize = 2;
/// The maximum unsupported string-protocol payload discarded before recovery.
pub const MAX_STRING_BYTES: usize = 4096;
/// The maximum number of bytes retained by either session transport queue.
///
/// A queued write is accepted atomically: if the entire write does not fit,
/// none of its bytes are retained and the caller receives an overflow result.
pub const TRANSPORT_QUEUE_HIGH_WATERMARK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Dimensions {
    columns: usize,
    rows: usize,
    cell_count: usize,
}

impl Dimensions {
    pub fn new(columns: usize, rows: usize) -> Result<Self, DimensionsError> {
        if columns < 2 {
            return Err(DimensionsError::TooFewColumns { columns });
        }
        if rows == 0 {
            return Err(DimensionsError::ZeroRows);
        }

        let cell_count = columns
            .checked_mul(rows)
            .ok_or(DimensionsError::CellCountOverflow { columns, rows })?;
        if cell_count > MAX_CELL_COUNT {
            return Err(DimensionsError::TooManyCells {
                cell_count,
                maximum: MAX_CELL_COUNT,
            });
        }

        Ok(Self {
            columns,
            rows,
            cell_count,
        })
    }

    pub const fn columns(self) -> usize {
        self.columns
    }

    pub const fn rows(self) -> usize {
        self.rows
    }

    pub const fn cell_count(self) -> usize {
        self.cell_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionsError {
    TooFewColumns { columns: usize },
    ZeroRows,
    CellCountOverflow { columns: usize, rows: usize },
    TooManyCells { cell_count: usize, maximum: usize },
}

impl fmt::Display for DimensionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooFewColumns { columns } => {
                write!(
                    formatter,
                    "terminal requires at least 2 columns, received {columns}"
                )
            }
            Self::ZeroRows => formatter.write_str("terminal requires at least 1 row, received 0"),
            Self::CellCountOverflow { columns, rows } => write!(
                formatter,
                "terminal dimensions {columns} columns by {rows} rows overflow the cell count"
            ),
            Self::TooManyCells {
                cell_count,
                maximum,
            } => write!(
                formatter,
                "terminal dimensions require {cell_count} cells, exceeding the maximum of {maximum}"
            ),
        }
    }
}

impl std::error::Error for DimensionsError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    column: usize,
    row: usize,
}

impl Cursor {
    pub const fn column(self) -> usize {
        self.column
    }

    pub const fn row(self) -> usize {
        self.row
    }
}

pub use cell::{Attributes, Cell, CellWidth, Color};
pub use input::{
    FocusEvent, InputEvent, InputEventOutcome, Key, KeypadKey, Modifiers, MouseButton, MouseEvent,
    MouseEventKind, MouseWheel,
};
pub use modes::{CursorStyle, MouseTrackingMode, TerminalModes};
pub use parser::{CsiParameters, ParameterSeparator, Parser, TerminalOp};
pub use replies::QueuePushResult;
pub use screen::Screen;
pub use terminal::{Terminal, TerminalError};

#[cfg(test)]
mod tests {
    use super::{
        Attributes, CellWidth, Color, Dimensions, FocusEvent, InputEvent, InputEventOutcome, Key,
        KeypadKey, Modifiers, MouseButton, MouseEvent, MouseEventKind, MouseTrackingMode,
        MouseWheel, Parser, QueuePushResult, Terminal, TerminalOp, MAX_CELL_COUNT,
        MAX_CSI_PARAMETERS, MAX_STRING_BYTES, TRANSPORT_QUEUE_HIGH_WATERMARK,
    };
    use std::sync::Arc;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
    }

    #[test]
    fn validates_dimensions_before_allocating() {
        assert!(Dimensions::new(0, 1).is_err());
        assert!(Dimensions::new(1, 1).is_err());
        assert!(Dimensions::new(2, 0).is_err());
        assert!(Dimensions::new(usize::MAX, 2).is_err());
        assert!(Dimensions::new(2, (MAX_CELL_COUNT / 2) + 1).is_err());
        assert!(Terminal::new(Dimensions::new(2, 1).unwrap()).is_ok());
    }

    #[test]
    fn parser_emits_typed_operations_and_ignores_raw_c1() {
        let mut parser = Parser::new();
        assert_eq!(parser.advance(b'A'), TerminalOp::Print('A'));
        assert_eq!(parser.advance(b'\r'), TerminalOp::CarriageReturn);
        assert_eq!(parser.advance(0x9b), TerminalOp::Ignored);
        assert_eq!(parser.advance(0x1b), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'['), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'2'), TerminalOp::Ignored);
        assert_eq!(
            parser.advance(b'J'),
            TerminalOp::EraseDisplay({
                let mut parameters = super::CsiParameters::default();
                assert!(parameters.append_digit(2));
                parameters
            })
        );
    }

    #[test]
    fn malformed_and_over_limit_sequences_recover_without_printing_payload() {
        let mut terminal = terminal(8, 1);
        let oversized_parameters = format!("\x1b[{}mOK", "1;".repeat(MAX_CSI_PARAMETERS + 1));
        terminal.ingest(oversized_parameters.as_bytes());
        terminal.ingest(b"\x1b[123456mX");
        terminal.ingest(b"\x1b[!\"#AB");
        terminal.ingest(&[0x1b, b']']);
        terminal.ingest(&vec![b'x'; MAX_STRING_BYTES]);
        terminal.ingest(b"Y");

        assert_eq!(terminal.row_text(0).as_deref(), Some("OKXBY   "));
        assert_eq!(terminal.attributes(), Attributes::NONE);
    }

    #[test]
    fn preserves_right_margin_until_the_next_printable_byte() {
        let mut terminal = terminal(3, 2);
        terminal.ingest(b"abcdef");
        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("def"));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );

        terminal.ingest(b"\x1b[31mg");
        assert_eq!(terminal.row_text(0).as_deref(), Some("def"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("g  "));
        assert_eq!(terminal.cell(0, 1).unwrap().foreground(), Color::Indexed(1));
    }

    #[test]
    fn applies_movement_erasure_and_editing_within_the_active_region() {
        let mut terminal = terminal(5, 4);
        terminal.ingest(b"11111\r\n22222\r\n33333\r\n44444");
        terminal.ingest(b"\x1b[2;4r\x1b[2;1H\x1b[2L\x1b[1M");
        terminal.ingest(b"\x1b[2;2H\x1b[2@A\x1b[1P\x1b[2X");

        assert_eq!(terminal.row_text(0).as_deref(), Some("11111"));
        assert_eq!(terminal.row_text(1).as_deref(), Some(" A   "));
        assert_eq!(terminal.row_text(2).as_deref(), Some("22222"));
        assert_eq!(terminal.row_text(3).as_deref(), Some("     "));
    }

    #[test]
    fn origin_mode_and_scroll_region_bound_cursor_addressing_and_scrolling() {
        let mut terminal = terminal(4, 4);
        terminal.ingest(b"top \r\none \r\ntwo \r\nbot ");
        terminal.ingest(b"\x1b[2;3r\x1b[?6h\x1b[1;1HABCDZWXYQ");
        assert_eq!(terminal.row_text(0).as_deref(), Some("top "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("ZWXY"));
        assert_eq!(terminal.row_text(2).as_deref(), Some("Q   "));
        assert_eq!(terminal.row_text(3).as_deref(), Some("bot "));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (1, 2)
        );
    }

    #[test]
    fn saves_and_restores_dec_and_ansi_state() {
        let mut terminal = terminal(6, 2);
        terminal.ingest(b"\x1b[31;1mAB\x1b7\x1b[2;6H\x1b[0mZ\x1b8C");
        assert_eq!(terminal.cell(2, 0).unwrap().foreground(), Color::Indexed(1));
        assert!(terminal
            .cell(2, 0)
            .unwrap()
            .attributes()
            .contains(Attributes::BOLD));
        terminal.ingest(b"\x1b[s\x1b[2;1H\x1b[uD");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (4, 0)
        );
    }

    #[test]
    fn alternate_modes_preserve_primary_and_1049_restores_saved_cursor() {
        let mut terminal = terminal(5, 2);
        terminal.ingest(b"main\x1b[?47halt\x1b[?47l");
        assert_eq!(terminal.row_text(0).as_deref(), Some("main "));
        terminal.ingest(b"\x1b[?1049halt\x1b[?1049lX");
        assert_eq!(terminal.row_text(0).as_deref(), Some("mainX"));
        assert!(!terminal.modes().alternate_screen());
    }

    #[test]
    fn dec_restore_clamps_an_origin_cursor_to_current_margins_before_cpr() {
        let mut terminal = terminal(5, 4);
        terminal.ingest(b"\x1b[2;3r\x1b[?6h\x1b7\x1b[?6l\x1b[3;4r\x1b8\x1b[6n");

        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (0, 2)
        );
        assert!(terminal.modes().origin_mode());
        assert_eq!(terminal.drain_replies(), b"\x1b[1;1R");
    }

    #[test]
    fn dec_and_ansi_saved_cursors_are_scoped_to_the_active_screen() {
        let mut terminal = terminal(5, 2);
        terminal.ingest(
            b"\x1b[1;2H\x1b7\x1b[s\x1b[?47h\
              \x1b[2;3H\x1b7\x1b[s\x1b[1;1H\x1b8A\
              \x1b[?47l\x1b8P",
        );

        assert_eq!(
            terminal.primary_screen().row_text(0).as_deref(),
            Some(" P   ")
        );
        assert_eq!(
            terminal.alternate_screen().unwrap().row_text(1).as_deref(),
            Some("  A  ")
        );
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 0)
        );

        terminal.ingest(b"\x1b[?47h\x1b8");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
        terminal.ingest(b"\x1b[u");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
    }

    #[test]
    fn applies_standard_indexed_and_true_color_sgr() {
        let mut terminal = terminal(5, 1);
        terminal.ingest(b"\x1b[1;4;31;48;5;200mA\x1b[38;2;1;2;3;49mB\x1b[0mC");
        let first = terminal.cell(0, 0).unwrap();
        assert_eq!(first.foreground(), Color::Indexed(1));
        assert_eq!(first.background(), Color::Indexed(200));
        assert!(first.attributes().contains(Attributes::BOLD));
        assert!(first.attributes().contains(Attributes::UNDERLINE));
        assert_eq!(
            terminal.cell(1, 0).unwrap().foreground(),
            Color::Rgb {
                red: 1,
                green: 2,
                blue: 3
            }
        );
        assert_eq!(terminal.cell(2, 0).unwrap().foreground(), Color::Default);
    }

    #[test]
    fn reports_device_status_and_conservative_primary_identity() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"\x1b[2;3H\x1b[5n\x1b[6n\x1b[c\x1b[>c");
        assert_eq!(
            terminal.drain_replies(),
            b"\x1b[0n\x1b[2;3R\x1b[?6c\x1b[>0;0;0c"
        );
    }

    #[test]
    fn osc_titles_and_allowlisted_hyperlinks_are_passive_and_bounded() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(
            b"\x1b]2;fesTerm\x07\
              \x1b]8;id=one;https://example.com/path\x1b\\link\
              \x1b]8;;\x1b\\\
              \x1b]8;;javascript:alert(1)\x1b\\X",
        );

        assert_eq!(terminal.title(), "fesTerm");
        assert_eq!(terminal.cell(0, 0).unwrap().text(), "l");
        assert_eq!(
            terminal.cell(0, 0).unwrap().hyperlink(),
            Some("https://example.com/path")
        );
        assert_eq!(terminal.cell(4, 0).unwrap().text(), "X");
        assert_eq!(terminal.cell(4, 0).unwrap().hyperlink(), None);
        assert!(terminal.drain_replies().is_empty());
    }

    #[test]
    fn hyperlink_targets_are_shared_across_cells() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b]8;;https://example.com/long-target\x1b\\links");

        let first = terminal.cell(0, 0).unwrap().hyperlink_target().unwrap();
        let second = terminal.cell(4, 0).unwrap().hyperlink_target().unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn resize_preserves_upper_left_cells_and_clamps_state() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"abcdefghi\x1b[2;3r\x1b[?6h\x1b[3;5H");
        terminal.resize(Dimensions::new(3, 2).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("fgh"));
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 1)
        );
        assert_eq!(terminal.take_dirty_rows(), vec![0, 1]);
    }

    #[test]
    fn repeated_resize_preserves_visible_banner_and_prompt_cells() {
        let mut terminal = terminal(12, 4);
        terminal.ingest(b"Windows cmd\r\nCopyright\r\nC:\\Users\\fes>");

        for dimensions in [
            Dimensions::new(11, 4).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 3).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 4).unwrap(),
        ] {
            terminal.resize(dimensions).unwrap();
            assert!(terminal
                .row_text(0)
                .is_some_and(|row| row.starts_with("Windows cmd")));
            assert!(terminal
                .row_text(1)
                .is_some_and(|row| row.starts_with("Copyright")));
            assert!(terminal
                .row_text(2)
                .is_some_and(|row| row.starts_with("C:\\Users\\")));
        }
    }

    #[test]
    fn transport_queues_are_bounded_observable_and_preserve_accepted_order() {
        let mut terminal = terminal(2, 1);
        assert_eq!(
            terminal.queue_input(&[0x80, b'A']),
            QueuePushResult {
                accepted: 2,
                overflowed: false
            }
        );
        let input_fill = vec![b'i'; TRANSPORT_QUEUE_HIGH_WATERMARK - 2];
        assert_eq!(
            terminal.queue_input(&input_fill).accepted(),
            input_fill.len()
        );
        assert_eq!(
            terminal.queue_input(b"overflow"),
            QueuePushResult {
                accepted: 0,
                overflowed: true
            }
        );

        assert_eq!(&terminal.queued_input()[..2], &[0x80, b'A']);
        assert_eq!(
            terminal.queued_input().len(),
            TRANSPORT_QUEUE_HIGH_WATERMARK
        );
        assert!(terminal.take_input_queue_overflowed());
        assert!(!terminal.take_input_queue_overflowed());
        let drained_input = terminal.drain_input();
        assert_eq!(&drained_input[..2], &[0x80, b'A']);
        assert!(terminal.queued_input().is_empty());

        let reply_fill = vec![b'r'; TRANSPORT_QUEUE_HIGH_WATERMARK - 3];
        assert_eq!(
            terminal.queue_reply(&reply_fill).accepted(),
            reply_fill.len()
        );
        terminal.ingest(b"\x1b[5n");
        assert_eq!(terminal.queued_replies(), reply_fill);
        assert!(terminal.take_reply_queue_overflowed());
        assert!(!terminal.take_reply_queue_overflowed());
        assert_eq!(terminal.drain_replies(), reply_fill);
    }

    #[test]
    fn encodes_cursor_keypad_paste_and_focus_input_modes_exactly() {
        let mut terminal = terminal(8, 2);

        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::ArrowUp)),
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Keypad(KeypadKey::Digit(1)))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(terminal.drain_input(), b"\x1b[A1");

        terminal.ingest(b"\x1b[?1h\x1b=");
        assert!(terminal.modes().application_cursor());
        assert!(terminal.modes().application_keypad());
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::ArrowLeft)),
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Keypad(KeypadKey::Decimal))),
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(terminal.drain_input(), b"\x1bOD\x1bOn");

        terminal.ingest(b"\x1b[?1l\x1b>");
        assert!(!terminal.modes().application_cursor());
        assert!(!terminal.modes().application_keypad());

        assert_eq!(
            terminal.handle_input(InputEvent::Focus(FocusEvent::In)),
            InputEventOutcome::Rejected
        );
        terminal.ingest(b"\x1b[?1004h\x1b[?2004h");
        assert_eq!(
            terminal.handle_input(InputEvent::Focus(FocusEvent::In)),
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Focus(FocusEvent::Out)),
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Paste("a\x1b[201~b".to_owned())),
            InputEventOutcome::Encoded { bytes: 20 }
        );
        assert_eq!(
            terminal.drain_input(),
            b"\x1b[I\x1b[O\x1b[200~a\x1b[201~b\x1b[201~"
        );

        terminal.ingest(b"\x1b[?1004l\x1b[?2004l");
        assert_eq!(
            terminal.handle_input(InputEvent::Focus(FocusEvent::Out)),
            InputEventOutcome::Rejected
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Paste("plain".to_owned())),
            InputEventOutcome::Encoded { bytes: 5 }
        );
        assert_eq!(terminal.drain_input(), b"plain");
    }

    #[test]
    fn mouse_modes_claim_selection_and_encode_sgr_coordinates_beyond_legacy_limits() {
        let mut terminal = terminal(8, 2);
        let press = MouseEvent {
            kind: MouseEventKind::Press(MouseButton::Left),
            column: 300,
            row: 400,
            modifiers: Modifiers::SHIFT.with(Modifiers::CONTROL),
        };
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(press)),
            InputEventOutcome::SelectionAllowed
        );

        terminal.ingest(b"\x1b[?9h");
        assert_eq!(terminal.modes().mouse_tracking(), MouseTrackingMode::X10);
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                ..press
            })),
            InputEventOutcome::SelectionClaimed
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(press)),
            InputEventOutcome::Rejected
        );

        terminal.ingest(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(
            terminal.modes().mouse_tracking(),
            MouseTrackingMode::ButtonMotion
        );
        assert!(terminal.modes().sgr_mouse());
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(press)),
            InputEventOutcome::Encoded { bytes: 14 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                ..press
            })),
            InputEventOutcome::Encoded { bytes: 14 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Move {
                    button: Some(MouseButton::Right),
                },
                ..press
            })),
            InputEventOutcome::Encoded { bytes: 14 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Move { button: None },
                ..press
            })),
            InputEventOutcome::SelectionClaimed
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Wheel(MouseWheel::Down),
                ..press
            })),
            InputEventOutcome::Encoded { bytes: 14 }
        );
        assert_eq!(
            terminal.drain_input(),
            b"\x1b[<20;301;401M\x1b[<20;301;401m\x1b[<54;301;401M\x1b[<85;301;401M"
        );

        terminal.ingest(b"\x1b[?1003h\x1b[?1002l");
        assert_eq!(
            terminal.modes().mouse_tracking(),
            MouseTrackingMode::AnyMotion
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Move { button: None },
                ..press
            })),
            InputEventOutcome::Encoded { bytes: 14 }
        );
        terminal.ingest(b"\x1b[?1003l\x1b[?1000h\x1b[?1006l");
        assert_eq!(
            terminal.modes().mouse_tracking(),
            MouseTrackingMode::ButtonEvent
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Mouse(MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Middle),
                column: 2,
                row: 3,
                modifiers: Modifiers::SHIFT,
            })),
            InputEventOutcome::Encoded { bytes: 6 }
        );
        assert_eq!(terminal.drain_input(), b"\x1b[<55;301;401M\x1b[M%#$");
    }

    #[test]
    fn rejects_input_atomically_when_the_bounded_queue_is_full() {
        let mut terminal = terminal(2, 1);
        terminal.ingest(b"\x1b[?2004h");
        assert_eq!(
            terminal.handle_input(InputEvent::Paste(
                "x".repeat(TRANSPORT_QUEUE_HIGH_WATERMARK)
            )),
            InputEventOutcome::QueueOverflow
        );
        assert!(terminal.queued_input().is_empty());
        assert!(terminal.take_input_queue_overflowed());
        terminal.queue_input(&vec![b'x'; TRANSPORT_QUEUE_HIGH_WATERMARK]);
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Character('a'))),
            InputEventOutcome::QueueOverflow
        );
        assert_eq!(
            terminal.queued_input().len(),
            TRANSPORT_QUEUE_HIGH_WATERMARK
        );
    }

    #[test]
    fn decodes_unicode_incrementally_and_repairs_wide_cells_after_grid_mutations() {
        let mut unicode_terminal = terminal(8, 1);
        unicode_terminal.ingest(b"A\xe7");
        unicode_terminal.ingest(b"\x95\x8ce\xcc");
        unicode_terminal.ingest(b"\x81\xf0\x9f\x98\x80");
        assert_eq!(unicode_terminal.row_text(0).as_deref(), Some("A界 e😀   "));
        assert_eq!(
            unicode_terminal.cell(1, 0).unwrap().width(),
            CellWidth::Double
        );
        assert!(unicode_terminal.cell(2, 0).unwrap().is_continuation());
        assert_eq!(unicode_terminal.cell(3, 0).unwrap().text(), "e\u{301}");
        assert_eq!(
            unicode_terminal.cell(4, 0).unwrap().width(),
            CellWidth::Double
        );
        assert!(unicode_terminal.cell(5, 0).unwrap().is_continuation());
        assert_eq!(
            (
                unicode_terminal.cursor().column(),
                unicode_terminal.cursor().row()
            ),
            (6, 0)
        );

        let mut malformed = terminal(5, 1);
        malformed.ingest(b"\xf0\x28A");
        assert_eq!(malformed.row_text(0).as_deref(), Some("�(A  "));

        let mut edited = terminal(6, 1);
        edited.ingest("A界BC".as_bytes());
        edited.ingest(b"\x1b[1;2H\x1b[@");
        assert_eq!(edited.row_text(0).as_deref(), Some("A 界 BC"));
        assert_eq!(edited.cell(2, 0).unwrap().width(), CellWidth::Double);
        assert!(edited.cell(3, 0).unwrap().is_continuation());
        edited.ingest(b"\x1b[1;4H\x1b[1P");
        assert_eq!(edited.row_text(0).as_deref(), Some("A  BC "));
        assert_eq!(edited.cell(2, 0).unwrap().width(), CellWidth::Single);
        assert!(!edited.cell(3, 0).unwrap().is_continuation());

        let mut resized = terminal(5, 1);
        resized.ingest("A界".as_bytes());
        resized.resize(Dimensions::new(2, 1).unwrap()).unwrap();
        assert_eq!(resized.row_text(0).as_deref(), Some("A "));
        assert_eq!(resized.cell(1, 0).unwrap().width(), CellWidth::Single);
    }

    #[test]
    fn rejects_invalid_utf8_second_byte_ranges_without_delaying_replacement() {
        for (leading, invalid_second) in [(0xe0, 0x80), (0xed, 0xa0), (0xf0, 0x80), (0xf4, 0x90)] {
            let mut terminal = terminal(3, 1);
            terminal.ingest(&[leading]);
            assert_eq!(terminal.row_text(0).as_deref(), Some("   "));

            terminal.ingest(&[invalid_second]);
            assert_eq!(terminal.row_text(0).as_deref(), Some("�� "));
        }

        let mut terminal = terminal(3, 1);
        terminal.ingest(&[0xe0, 0xc2]);
        assert_eq!(terminal.row_text(0).as_deref(), Some("�  "));
        terminal.ingest(&[0xa2]);
        assert_eq!(terminal.row_text(0).as_deref(), Some("�¢ "));
    }

    #[test]
    fn resize_clears_a_clipped_combining_anchor() {
        let mut terminal = terminal(3, 1);
        terminal.ingest("A界\u{09bc}".as_bytes());
        assert_eq!(terminal.cell(1, 0).unwrap().text(), "界\u{09bc}");

        terminal.resize(Dimensions::new(2, 1).unwrap()).unwrap();
        terminal.ingest("\u{09bc}".as_bytes());

        assert_eq!(terminal.row_text(0).as_deref(), Some("A "));
        assert_eq!(terminal.cell(1, 0).unwrap().text(), " ");
        assert_eq!(terminal.cell(1, 0).unwrap().width(), CellWidth::Single);
    }
}
