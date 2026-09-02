//! GUI-independent ANSI/VT terminal state primitives.
//!
//! The parser accepts C0 controls plus 7-bit ESC/CSI syntax. The terminal
//! incrementally decodes printable UTF-8 while the parser is in ground state.
//! Raw C1 bytes are deliberately not controls: treating them as such would
//! make UTF-8 continuation bytes ambiguous.

use std::fmt;

mod cell;
mod history;
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
pub use history::{LogicalLine, ScrollbackStats, DEFAULT_SCROLLBACK_LIMIT_BYTES};

/// Parses and normalizes an untrusted external web target.
///
/// Only absolute ASCII HTTP/HTTPS URLs with a host are eligible for OS
/// activation. Callers must still require an explicit user gesture.
pub fn normalize_external_web_url(target: &str) -> Option<String> {
    if target.len() > 2_048
        || !target.is_ascii()
        || target.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    let parsed = url::Url::parse(target).ok()?;
    (matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none())
    .then(|| parsed.to_string())
}

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
pub use terminal::{ContentPosition, Terminal, TerminalError};

#[cfg(test)]
mod model_tests;

#[cfg(test)]
mod tests {
    use super::{
        Attributes, CellWidth, Color, ContentPosition, Dimensions, FocusEvent, InputEvent,
        InputEventOutcome, Key, KeypadKey, Modifiers, MouseButton, MouseEvent, MouseEventKind,
        MouseTrackingMode, MouseWheel, Parser, QueuePushResult, Terminal, TerminalOp,
        MAX_CELL_COUNT, MAX_CSI_PARAMETERS, MAX_STRING_BYTES, TRANSPORT_QUEUE_HIGH_WATERMARK,
    };
    use std::sync::Arc;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap()
    }

    fn unicode_emoji_15_1_fully_qualified() -> Vec<String> {
        include_str!("../tests/fixtures/unicode/emoji-15.1/emoji-test.txt")
            .lines()
            .filter_map(|line| {
                let (code_points, remainder) = line.split_once(';')?;
                remainder
                    .trim_start()
                    .starts_with("fully-qualified")
                    .then(|| {
                        code_points
                            .split_whitespace()
                            .map(|code_point| {
                                char::from_u32(
                                    u32::from_str_radix(code_point, 16)
                                        .expect("emoji data uses hexadecimal code points"),
                                )
                                .expect("emoji data contains valid scalar values")
                            })
                            .collect()
                    })
            })
            .collect()
    }

    #[test]
    #[ignore = "manual throughput probe, not a regression test"]
    fn manual_ingest_throughput_probe() {
        let mut term = terminal(120, 40);
        let pattern = b"C:\\Windows\\System32\\some\\deep\\path\\to\\a\\file.txt\r\n";
        let mut workload = Vec::new();
        while workload.len() < 4 * 1024 * 1024 {
            workload.extend_from_slice(pattern);
        }
        let start = std::time::Instant::now();
        term.ingest(&workload);
        let elapsed = start.elapsed();
        eprintln!(
            "ingest {} bytes took {:?} ({:.2} MB/s)",
            workload.len(),
            elapsed,
            workload.len() as f64 / elapsed.as_secs_f64() / 1_048_576.0
        );
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

    /// tmux (and ncurses' `smacs`/`rmacs`) draws every window border and
    /// status-bar divider by designating DEC Special Graphics into G1 with
    /// `ESC ) 0`, then toggling it on with SO (0x0E) and off with SI
    /// (0x0F) around each run of line-drawing bytes. Without this, those
    /// bytes print as raw ASCII (`q`, `x`, `l`, `k`, `m`, ...) instead of
    /// the box-drawing glyphs they represent.
    #[test]
    fn shift_out_translates_g1_dec_special_graphics_until_shift_in() {
        let mut parser = Parser::new();
        // ESC ) 0 designates DEC Special Graphics into G1.
        assert_eq!(parser.advance(0x1b), TerminalOp::Ignored);
        assert_eq!(parser.advance(b')'), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'0'), TerminalOp::Ignored);

        // Before SO, G0 (still ASCII) is active: bytes print unchanged.
        assert_eq!(parser.advance(b'q'), TerminalOp::Print('q'));

        // SO shifts to G1 (DEC Special Graphics).
        assert_eq!(parser.advance(0x0e), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'q'), TerminalOp::Print('\u{2500}')); // ─
        assert_eq!(parser.advance(b'x'), TerminalOp::Print('\u{2502}')); // │
        assert_eq!(parser.advance(b'l'), TerminalOp::Print('\u{250c}')); // ┌
        assert_eq!(parser.advance(b'k'), TerminalOp::Print('\u{2510}')); // ┐
        assert_eq!(parser.advance(b'm'), TerminalOp::Print('\u{2514}')); // └
        assert_eq!(parser.advance(b'j'), TerminalOp::Print('\u{2518}')); // ┘

        // SI shifts back to G0 (ASCII): bytes print unchanged again.
        assert_eq!(parser.advance(0x0f), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'q'), TerminalOp::Print('q'));
    }

    #[test]
    fn designating_dec_special_graphics_into_g0_translates_without_shift_out() {
        let mut parser = Parser::new();
        // ESC ( 0 designates DEC Special Graphics directly into G0, the
        // default active slot (no SO required); some programs use this
        // form instead of the G1 + SO/SI convention.
        assert_eq!(parser.advance(0x1b), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'('), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'0'), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'q'), TerminalOp::Print('\u{2500}'));

        // ESC ( B restores G0 to ASCII (US-ASCII designation).
        assert_eq!(parser.advance(0x1b), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'('), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'B'), TerminalOp::Ignored);
        assert_eq!(parser.advance(b'q'), TerminalOp::Print('q'));
    }

    /// End-to-end version of the parser-level SO/SI tests above, through
    /// `Terminal::ingest`: this is the same byte sequence tmux emits to
    /// draw a one-cell-tall horizontal rule (e.g. a pane divider).
    #[test]
    fn terminal_renders_tmux_style_line_drawing_through_shift_out_and_in() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b)0\x0eqqqq\x0f");
        assert_eq!(
            terminal.row_text(0).as_deref(),
            Some("\u{2500}\u{2500}\u{2500}\u{2500}")
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
    fn osc8_rejects_malformed_spoofable_and_non_web_targets() {
        for target in [
            "https://",
            "https://a b",
            "https://example.com/\u{202e}spoof",
            "https://github.com@evil.example/login",
            "mailto:user@example.com",
            "javascript:alert(1)",
            "file:///etc/passwd",
        ] {
            let mut terminal = terminal(8, 1);
            terminal.ingest(format!("\x1b]8;;{target}\x1b\\X").as_bytes());
            assert_eq!(
                terminal.cell(0, 0).unwrap().hyperlink(),
                None,
                "{target:?} must not become an activatable link"
            );
        }

        let mut terminal = terminal(8, 1);
        terminal.ingest(
            b"\x1b]8;;https://example.com\x1b\\A\
              \x1b]8;;https://\x1b\\B",
        );
        assert_eq!(
            terminal.cell(0, 0).unwrap().hyperlink(),
            Some("https://example.com/")
        );
        assert_eq!(terminal.cell(1, 0).unwrap().hyperlink(), None);
    }

    #[test]
    fn unterminated_osc8_links_end_at_security_boundaries() {
        let mut terminal = terminal(16, 2);
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\linked\nplain");
        assert_eq!(
            terminal.cell(0, 0).unwrap().hyperlink(),
            Some("https://example.com/")
        );
        assert_eq!(terminal.cell(6, 1).unwrap().hyperlink(), None);

        terminal.reset_to_initial_state();
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\linked\x1b[0mplain");
        assert_eq!(terminal.cell(6, 0).unwrap().hyperlink(), None);

        terminal.reset_to_initial_state();
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\linked\x1b[?1049hplain");
        assert_eq!(terminal.cell(0, 0).unwrap().hyperlink(), None);

        let mut bounded = Terminal::new(Dimensions::new(80, 60).unwrap()).unwrap();
        bounded.ingest(b"\x1b]8;;https://example.com\x1b\\");
        bounded.ingest(&vec![b'x'; 4_097]);
        assert_eq!(
            bounded.cell(15, 51).unwrap().hyperlink(),
            Some("https://example.com/")
        );
        assert_eq!(bounded.cell(16, 51).unwrap().hyperlink(), None);
    }

    #[test]
    fn resize_reflows_visible_content_and_clamps_state() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"abcdefghi\x1b[2;3r\x1b[?6h\x1b[3;5H");
        terminal.resize(Dimensions::new(3, 2).unwrap()).unwrap();
        // Reflow (ADR 0017): "abcdefghi" plus the still-blank row the
        // cursor was addressed onto rewraps at width 3 into
        // "abc"/"def"/"ghi"/"   "; with only 2 rows now visible, the
        // first two rewrapped rows move into retained history.
        assert_eq!(terminal.row_text(0).as_deref(), Some("ghi"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("   "));
        let scrollback = terminal.scrollback_lines().next().unwrap();
        assert_eq!(
            scrollback
                .cells()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "abcdef"
        );
        assert_eq!(scrollback.physical_rows(), 2);
        // The cursor had been moved (via origin-mode CSI addressing) onto
        // a blank row below all printed content; that row has no stable
        // logical anchor of its own, so the cursor falls back to the
        // bottom-left of the new screen.
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (0, 1)
        );
        assert_eq!(terminal.take_dirty_rows(), vec![0, 1]);
    }

    #[test]
    fn repeated_resize_reflows_banner_and_prompt_cells_and_recovers_scrolled_off_lines() {
        // Under reflow (ADR 0017), shrinking the row count can push older
        // hard-broken lines into retained history rather than always
        // keeping them clipped in place at the top; growing back to a
        // taller size pulls them back onto the visible screen unchanged.
        let mut terminal = terminal(12, 4);
        terminal.ingest(b"Windows cmd\r\nCopyright\r\nC:\\Users\\fes>");

        terminal.resize(Dimensions::new(11, 4).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("Windows cmd"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("Copyright  "));
        assert_eq!(terminal.row_text(2).as_deref(), Some("C:\\Users\\fe"));
        assert_eq!(terminal.row_text(3).as_deref(), Some("s>         "));

        // Only 3 rows now fit; the wrapped prompt alone needs 2 of them, so
        // "Windows cmd" scrolls into retained history.
        terminal.resize(Dimensions::new(12, 3).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("Copyright   "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("C:\\Users\\fes"));
        assert_eq!(terminal.row_text(2).as_deref(), Some(">           "));
        assert!(terminal.scrollback_lines().next().is_some_and(|line| line
            .cells()
            .iter()
            .map(|cell| cell.character())
            .collect::<String>()
            == "Windows cmd"));

        terminal.resize(Dimensions::new(11, 3).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("Copyright  "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("C:\\Users\\fe"));
        assert_eq!(terminal.row_text(2).as_deref(), Some("s>         "));

        terminal.resize(Dimensions::new(12, 3).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("Copyright   "));
        assert_eq!(terminal.row_text(1).as_deref(), Some("C:\\Users\\fes"));
        assert_eq!(terminal.row_text(2).as_deref(), Some(">           "));

        // Growing back to the original height recovers "Windows cmd" from
        // history onto the visible screen, exactly as it was originally.
        terminal.resize(Dimensions::new(11, 4).unwrap()).unwrap();
        assert_eq!(terminal.row_text(0).as_deref(), Some("Windows cmd"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("Copyright  "));
        assert_eq!(terminal.row_text(2).as_deref(), Some("C:\\Users\\fe"));
        assert_eq!(terminal.row_text(3).as_deref(), Some("s>         "));
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
    fn encodes_ctrl_chords_as_their_conventional_control_bytes() {
        let mut terminal = terminal(8, 2);

        // Letters, either case: same control byte (Ctrl+B and Ctrl+Shift+B
        // both send 0x02, matching what every other terminal emulator sends
        // and what tmux/GNU Screen expect for their default prefix key).
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control('b'))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control('B'))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(terminal.drain_input(), [0x02, 0x02]);

        // A handful of punctuation keys conventionally paired with Ctrl for
        // the remaining low control codes.
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control(' '))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control('['))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control('\\'))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control(']'))),
            InputEventOutcome::Encoded { bytes: 1 }
        );
        assert_eq!(terminal.drain_input(), [0x00, 0x1b, 0x1c, 0x1d]);

        // A character without an established Ctrl mapping sends nothing,
        // rather than guessing at a byte.
        assert_eq!(
            terminal.handle_input(InputEvent::Key(Key::Control('1'))),
            InputEventOutcome::Rejected
        );
        assert!(terminal.drain_input().is_empty());
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
        // Reflow (ADR 0017): "A" and "界" no longer fit together at width
        // 2, so "A" moves into retained history and "界" (kept intact as
        // one atomic double-width unit) becomes the sole visible row.
        assert_eq!(resized.row_text(0).as_deref(), Some("界 "));
        assert_eq!(resized.cell(0, 0).unwrap().width(), CellWidth::Double);
        assert_eq!(
            resized.scrollback_lines().next().unwrap().cells()[0].character(),
            'A'
        );
    }

    #[test]
    fn emoji_sequences_use_grapheme_width_across_incremental_input() {
        let mut agency = terminal(12, 1);
        agency.ingest("🤖⚠".as_bytes());
        agency.ingest("\u{fe0f}ℹ".as_bytes());
        agency.ingest("\u{fe0f}🧹".as_bytes());

        for column in [0, 2, 4, 6] {
            assert_eq!(agency.cell(column, 0).unwrap().width(), CellWidth::Double);
            assert!(agency.cell(column + 1, 0).unwrap().is_continuation());
        }
        assert_eq!(agency.cell(0, 0).unwrap().text(), "🤖");
        assert_eq!(agency.cell(2, 0).unwrap().text(), "⚠️");
        assert_eq!(agency.cell(4, 0).unwrap().text(), "ℹ️");
        assert_eq!(agency.cell(6, 0).unwrap().text(), "🧹");
        assert_eq!((agency.cursor().column(), agency.cursor().row()), (8, 0));

        let mut joined = terminal(8, 1);
        joined.ingest("👩".as_bytes());
        joined.ingest("\u{200d}".as_bytes());
        joined.ingest("🔬".as_bytes());
        assert_eq!(joined.cell(0, 0).unwrap().text(), "👩‍🔬");
        assert_eq!(joined.cell(0, 0).unwrap().width(), CellWidth::Double);
        assert!(joined.cell(1, 0).unwrap().is_continuation());
        assert_eq!((joined.cursor().column(), joined.cursor().row()), (2, 0));

        let mut composed = terminal(8, 1);
        composed.ingest("1".as_bytes());
        composed.ingest("\u{fe0f}\u{20e3}".as_bytes());
        composed.ingest("🇺".as_bytes());
        composed.ingest("🇸".as_bytes());
        assert_eq!(composed.cell(0, 0).unwrap().text(), "1️⃣");
        assert_eq!(composed.cell(0, 0).unwrap().width(), CellWidth::Double);
        assert_eq!(composed.cell(2, 0).unwrap().text(), "🇺🇸");
        assert_eq!(composed.cell(2, 0).unwrap().width(), CellWidth::Double);
        assert_eq!(
            (composed.cursor().column(), composed.cursor().row()),
            (4, 0)
        );
    }

    #[test]
    fn emoji_width_changes_preserve_right_margin_wrap_policy() {
        let mut wrapped = terminal(3, 2);
        wrapped.ingest("ab⚠".as_bytes());
        wrapped.ingest("\u{fe0f}".as_bytes());
        assert_eq!(wrapped.row_text(0).as_deref(), Some("ab "));
        assert_eq!(wrapped.cell(0, 1).unwrap().text(), "⚠️");
        assert_eq!(wrapped.cell(0, 1).unwrap().width(), CellWidth::Double);
        assert!(wrapped.cell(1, 1).unwrap().is_continuation());
        assert_eq!((wrapped.cursor().column(), wrapped.cursor().row()), (2, 1));

        let mut no_wrap = terminal(3, 1);
        no_wrap.ingest(b"\x1b[?7l");
        no_wrap.ingest("ab⚠".as_bytes());
        no_wrap.ingest("\u{fe0f}".as_bytes());
        assert_eq!(no_wrap.row_text(0).as_deref(), Some("ab�"));
        assert_eq!(no_wrap.cell(2, 0).unwrap().width(), CellWidth::Single);
    }

    #[test]
    fn emoji_sequence_families_match_across_byte_chunk_boundaries() {
        let mut cases = vec![
            "🤖".to_owned(),
            "⚠️".to_owned(),
            "ℹ️".to_owned(),
            "👋🏻".to_owned(),
            "👍🏽".to_owned(),
            "🧑🏿".to_owned(),
            "👩‍🔬".to_owned(),
            "👨‍💻".to_owned(),
            "🧑🏽‍🚀".to_owned(),
            "👨‍👩‍👧‍👦".to_owned(),
            "🏃‍♀️".to_owned(),
            "🏳️‍🌈".to_owned(),
            "🏴‍☠️".to_owned(),
            "❤️‍🔥".to_owned(),
            "🇺🇸".to_owned(),
            "🇨🇦".to_owned(),
            "🇯🇵".to_owned(),
            "🇧🇷".to_owned(),
            "🇿🇦".to_owned(),
            "🇪🇺".to_owned(),
            "🏴\u{e0067}\u{e0062}\u{e0065}\u{e006e}\u{e0067}\u{e007f}".to_owned(),
        ];
        cases.extend(
            ['#', '*']
                .into_iter()
                .chain('0'..='9')
                .map(|base| format!("{base}\u{fe0f}\u{20e3}")),
        );

        for emoji in cases {
            let input = format!("{emoji}X");
            let mut contiguous = terminal(6, 1);
            contiguous.ingest(input.as_bytes());

            let mut fragmented = terminal(6, 1);
            for byte in input.as_bytes() {
                fragmented.ingest(std::slice::from_ref(byte));
            }

            for column in 0..6 {
                let expected = contiguous.cell(column, 0).unwrap();
                let actual = fragmented.cell(column, 0).unwrap();
                assert_eq!(actual.text(), expected.text(), "{emoji} text at {column}");
                assert_eq!(
                    actual.width(),
                    expected.width(),
                    "{emoji} width at {column}"
                );
            }
            assert_eq!(fragmented.cell(0, 0).unwrap().text(), emoji);
            assert_eq!(
                fragmented.cell(0, 0).unwrap().width(),
                CellWidth::Double,
                "{emoji}"
            );
            assert!(fragmented.cell(1, 0).unwrap().is_continuation(), "{emoji}");
            assert_eq!(fragmented.cell(2, 0).unwrap().text(), "X", "{emoji}");
            assert_eq!(
                (fragmented.cursor().column(), fragmented.cursor().row()),
                (3, 0),
                "{emoji}"
            );
        }
    }

    #[test]
    fn every_unicode_15_1_rgi_emoji_is_an_atomic_double_width_grapheme() {
        for emoji in unicode_emoji_15_1_fully_qualified() {
            let input = format!("{emoji}X");
            let mut terminal = terminal(5, 1);
            for byte in input.as_bytes() {
                terminal.ingest(std::slice::from_ref(byte));
            }
            assert_eq!(terminal.cell(0, 0).unwrap().text(), emoji);
            assert_eq!(
                terminal.cell(0, 0).unwrap().width(),
                CellWidth::Double,
                "{emoji}"
            );
            assert!(terminal.cell(1, 0).unwrap().is_continuation(), "{emoji}");
            assert_eq!(terminal.cell(2, 0).unwrap().text(), "X", "{emoji}");
        }
    }

    #[test]
    fn text_presentation_sequences_remain_single_width() {
        for text in ["⚠︎", "ℹ︎", "☀︎", "▶︎"] {
            let mut terminal = terminal(4, 1);
            for byte in format!("{text}X").as_bytes() {
                terminal.ingest(std::slice::from_ref(byte));
            }
            assert_eq!(terminal.cell(0, 0).unwrap().text(), text);
            assert_eq!(terminal.cell(0, 0).unwrap().width(), CellWidth::Single);
            assert_eq!(terminal.cell(1, 0).unwrap().text(), "X");
            assert_eq!(
                (terminal.cursor().column(), terminal.cursor().row()),
                (2, 0)
            );
        }
    }

    #[test]
    fn a_grapheme_at_the_storage_limit_remains_intact() {
        let mut text = String::from("é");
        text.extend(std::iter::repeat_n('\u{301}', 127));
        assert_eq!(text.len(), crate::unicode::MAX_GRAPHEME_BYTES);

        let mut terminal = terminal(4, 1);
        terminal.ingest(text.as_bytes());
        terminal.ingest(b"b");

        assert_eq!(terminal.cell(0, 0).unwrap().text(), text);
        assert_eq!(terminal.cell(1, 0).unwrap().text(), "b");
    }

    #[test]
    fn oversized_graphemes_are_replaced_and_stop_absorbing_input() {
        let mut terminal = terminal(4, 1);
        let mut input = String::from("a");
        input.extend(std::iter::repeat_n('\u{301}', 300));
        input.push('b');
        terminal.ingest(input.as_bytes());

        assert_eq!(terminal.cell(0, 0).unwrap().text(), "�");
        assert_eq!(terminal.cell(0, 0).unwrap().width(), CellWidth::Single);
        assert_eq!(terminal.cell(1, 0).unwrap().text(), "b");
        assert_eq!(
            (terminal.cursor().column(), terminal.cursor().row()),
            (2, 0)
        );
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

        // Reflow (ADR 0017): "A" moves into retained history and the wide
        // "界" cell (with its already-attached combining mark) becomes the
        // sole visible row. The combining anchor itself does not survive a
        // reflow (its position is not stable across a rewrap), so the
        // extra combining mark ingested after resize has nothing to attach
        // to and is dropped rather than appended anywhere.
        assert_eq!(terminal.row_text(0).as_deref(), Some("界 "));
        assert_eq!(terminal.cell(0, 0).unwrap().text(), "界\u{09bc}");
        assert_eq!(terminal.cell(0, 0).unwrap().width(), CellWidth::Double);
    }

    #[test]
    fn primary_full_screen_scroll_retains_hard_and_soft_logical_lines() {
        let mut primary = terminal(4, 2);
        primary.ingest(b"one\r\ntwo\r\n");

        let lines = primary.scrollback_lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0]
                .cells()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "one"
        );
        assert!(lines[0].has_hard_break());
        assert_eq!(lines[0].physical_rows(), 1);

        let mut spaced = terminal(5, 2);
        spaced.ingest(b"a  \r\nb\r\n");
        let spaced_line = spaced.scrollback_lines().next().unwrap();
        assert_eq!(
            spaced_line
                .cells()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "a  "
        );

        let mut wrapped = terminal(4, 2);
        wrapped.ingest(b"abcdefghX\r\nz\r\n");
        let lines = wrapped.scrollback_lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0]
                .cells()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "abcdefghX"
        );
        assert!(lines[0].has_hard_break());
        assert_eq!(lines[0].physical_rows(), 3);
    }

    #[test]
    fn resize_reflows_retained_scrollback_to_the_new_width() {
        // "abcdefghX" wraps to 3 physical rows at width 4 ("abcd","efgh","X").
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"abcdefghX\r\nz\r\n");
        assert_eq!(terminal.scrollback_stats().physical_rows(), 3);

        // Widen: the same logical content now fits in fewer physical rows.
        terminal.resize(Dimensions::new(9, 2).unwrap()).unwrap();
        let lines = terminal.scrollback_lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0]
                .cells()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "abcdefghX"
        );
        assert!(lines[0].has_hard_break());
        assert_eq!(lines[0].physical_rows(), 1);
        assert_eq!(terminal.scrollback_stats().physical_rows(), 1);
        assert_eq!(
            terminal
                .scrollback_physical_row(0)
                .unwrap()
                .iter()
                .map(|cell| cell.character())
                .collect::<String>(),
            "abcdefghX"
        );

        // Shrink further: the same content now needs more physical rows.
        terminal.resize(Dimensions::new(3, 2).unwrap()).unwrap();
        let lines = terminal.scrollback_lines().collect::<Vec<_>>();
        assert_eq!(lines[0].physical_rows(), 3);
        assert_eq!(terminal.scrollback_stats().physical_rows(), 3);
        assert_eq!(
            (0..3)
                .map(|row| terminal
                    .scrollback_physical_row(row)
                    .unwrap()
                    .iter()
                    .map(|cell| cell.character())
                    .collect::<String>())
                .collect::<Vec<_>>(),
            vec!["abc", "def", "ghX"]
        );

        // Growing rows only (columns unchanged) pulls the previously
        // scrolled-off content back onto the now-taller visible screen
        // rather than leaving it stranded in history.
        terminal.resize(Dimensions::new(3, 5).unwrap()).unwrap();
        assert_eq!(terminal.scrollback_stats().physical_rows(), 0);
        assert_eq!(terminal.row_text(0).as_deref(), Some("abc"));
        assert_eq!(terminal.row_text(1).as_deref(), Some("def"));
        assert_eq!(terminal.row_text(2).as_deref(), Some("ghX"));
        assert_eq!(terminal.row_text(3).as_deref(), Some("z  "));
    }

    #[test]
    fn resize_remaps_content_positions_through_logical_lines() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"abcdefghX\r\nz\r\n");

        let mapped = terminal
            .resize_with_content_positions(
                Dimensions::new(3, 2).unwrap(),
                &[
                    ContentPosition {
                        column: 1,
                        absolute_row: 1,
                    },
                    ContentPosition {
                        column: 0,
                        absolute_row: 3,
                    },
                ],
            )
            .unwrap();

        assert_eq!(
            mapped,
            vec![
                Some(ContentPosition {
                    column: 2,
                    absolute_row: 1,
                }),
                Some(ContentPosition {
                    column: 0,
                    absolute_row: 3,
                }),
            ]
        );
    }

    #[test]
    fn resize_does_not_alias_blank_trailing_rows_to_real_content() {
        let mut terminal = terminal(4, 4);
        terminal.ingest(b"abc");

        let mapped = terminal
            .resize_with_content_positions(
                Dimensions::new(3, 4).unwrap(),
                &[ContentPosition {
                    column: 0,
                    absolute_row: 3,
                }],
            )
            .unwrap();

        assert_eq!(mapped, vec![None]);
    }

    #[test]
    fn overwriting_a_wide_cell_mid_row_shrinks_the_occupied_extent_for_reflow() {
        // Regression test for Screen::extend_occupied_or_recompute's
        // fast/slow path split: printing at or beyond a row's known
        // occupied extent may take an O(1) shortcut, but overwriting mid-
        // row (column < the row's occupied extent) must still fall back to
        // a full rescan, since breaking a wide character's continuation
        // pair can shrink - not just extend - what's actually occupied.
        let mut terminal = terminal(6, 2);
        // Print 'A' + wide '界' (row 0 occupies columns 0..3), then move the
        // cursor back to column 1 (1-based column 2) and overwrite the wide
        // character's leading cell with a single-width 'X'. This orphans
        // the old continuation cell at column 2, which
        // `repair_neighborhood` blanks out, so the row's true occupied
        // extent shrinks from 3 to 2 ('A', 'X'). The remaining printed
        // lines push this row deep into scrollback history so it stays
        // retained (rather than folded back onto the visible screen) after
        // the reflow below.
        terminal.ingest("A界\x1b[1;2HX\r\nz\r\n1\r\n2\r\n3\r\n4\r\n5\r\n".as_bytes());

        // Reflowing to width 2 forces every logical line to wrap two
        // characters per physical row. If the occupied extent had wrongly
        // stayed at 3 (i.e. the shrink was missed), this logical line
        // would fold in a phantom trailing blank cell, splitting it across
        // two physical rows instead of fitting on one.
        terminal.resize(Dimensions::new(2, 2).unwrap()).unwrap();
        let lines = terminal.scrollback_lines().collect::<Vec<_>>();
        let line = &lines[0];
        assert_eq!(line.physical_rows(), 1, "'A','X' should fit on one row");
        let row0 = line.physical_row(0).unwrap();
        assert_eq!(row0.len(), 2);
        assert_eq!(row0[0].character(), 'A');
        assert_eq!(row0[1].character(), 'X');
    }

    #[test]
    fn reflow_keeps_a_double_width_cell_and_its_continuation_together() {
        let mut terminal = terminal(6, 2);
        // "A" + wide "界" + "B" + wide "界" -> 6 columns at width 6, one row.
        terminal.ingest("A界B界\r\nz\r\n".as_bytes());
        let before = terminal.scrollback_lines().next().unwrap();
        assert_eq!(before.physical_rows(), 1);

        // Shrink to width 3: "A界" (3 cols: A + double) must stay on one row
        // rather than splitting the double cell from its continuation.
        terminal.resize(Dimensions::new(3, 2).unwrap()).unwrap();
        let lines = terminal.scrollback_lines().collect::<Vec<_>>();
        let line = &lines[0];
        assert_eq!(line.physical_rows(), 2);
        let row0 = line.physical_row(0).unwrap();
        let row1 = line.physical_row(1).unwrap();
        assert_eq!(row0.len(), 3, "A + double-width cell + its continuation");
        assert_eq!(row0[0].character(), 'A');
        assert_eq!(row0[1].character(), '界');
        assert!(row0[2].is_continuation());
        assert_eq!(row1[0].character(), 'B');
        assert_eq!(row1[1].character(), '界');
        assert!(row1[2].is_continuation());
    }

    #[test]
    fn repeated_grow_and_shrink_reflow_preserves_content_and_stays_within_budget() {
        let dimensions = Dimensions::new(10, 3).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 65536).unwrap();
        for line in 0..40 {
            terminal.ingest(format!("line-{line:03}-abcdefghijklmnop\r\n").as_bytes());
        }
        let stats_before = terminal.scrollback_stats();
        assert!(stats_before.logical_lines() > 0);

        for columns in [40, 5, 25, 3, 80, 10] {
            terminal
                .resize(Dimensions::new(columns, 3).unwrap())
                .unwrap();
            let stats = terminal.scrollback_stats();
            assert!(stats.charged_bytes() <= stats.limit_bytes());
            let total_physical_rows: usize = terminal
                .scrollback_lines()
                .map(|line| line.physical_rows())
                .sum();
            assert_eq!(total_physical_rows, stats.physical_rows());
            for line in terminal.scrollback_lines() {
                let reconstructed = (0..line.physical_rows())
                    .flat_map(|row| line.physical_row(row).unwrap().iter())
                    .map(|cell| cell.character())
                    .collect::<String>();
                let expected = line
                    .cells()
                    .iter()
                    .map(|cell| cell.character())
                    .collect::<String>();
                assert_eq!(reconstructed, expected);
            }
        }
    }

    #[test]
    fn alternate_and_partial_margin_scrolling_never_enter_primary_history() {
        let mut terminal = terminal(5, 3);
        terminal.ingest(b"base\r\n");
        terminal.ingest(b"\x1b[?1049halt\r\none\r\ntwo\r\nthree\x1b[?1049l");
        assert_eq!(terminal.scrollback_stats().logical_lines(), 0);

        terminal.ingest(b"\x1b[2;3r\x1b[3;1H\n\n");
        assert_eq!(terminal.scrollback_stats().logical_lines(), 0);
    }

    #[test]
    fn scrollback_is_strictly_bounded_and_clear_preserves_visible_cells() {
        let dimensions = Dimensions::new(4, 2).unwrap();
        let mut disabled = Terminal::with_scrollback_limit(dimensions, 0).unwrap();
        disabled.ingest(b"one\r\ntwo\r\nthree\r\n");
        assert_eq!(disabled.scrollback_stats().charged_bytes(), 0);
        assert_eq!(disabled.scrollback_stats().logical_lines(), 0);

        let mut bounded = Terminal::with_scrollback_limit(dimensions, 256).unwrap();
        for _ in 0..32 {
            bounded.ingest(b"abc\r\n");
        }
        let stats = bounded.scrollback_stats();
        assert!(stats.charged_bytes() <= stats.limit_bytes());
        assert!(stats.evicted_lines() > 0 || stats.oversize_lines() > 0);

        let before = bounded.row_text(0);
        bounded.ingest(b"\x1b[3J");
        assert_eq!(bounded.scrollback_stats().charged_bytes(), 0);
        assert_eq!(bounded.scrollback_stats().logical_lines(), 0);
        assert_eq!(bounded.row_text(0), before);
    }

    #[test]
    fn an_unrelated_oversized_line_does_not_evict_prior_in_budget_history() {
        // Regression test: a single logical line that grows past the
        // scrollback byte budget without ever hitting a hard break (a long
        // unbroken write with no trailing newline - e.g. a giant minified
        // JSON/base64 blob, or a `\r`-updated progress line) must not wipe
        // out unrelated history that was already comfortably retained
        // under budget. Only as much prior history as is genuinely needed
        // to stay within budget may be evicted (normal FIFO pressure);
        // nothing may be wiped wholesale just because *some* line elsewhere
        // in the stream happened to grow large (see `Scrollback::push_row`
        // and `Scrollback::enforce_limit`).
        let dimensions = Dimensions::new(8, 4).unwrap();
        let mut terminal = Terminal::with_scrollback_limit(dimensions, 65536).unwrap();
        for index in 0..20 {
            terminal.ingest(format!("early-line-{index}\r\n").as_bytes());
        }
        let logical_lines_before = terminal.scrollback_stats().logical_lines();
        assert!(
            logical_lines_before > 0,
            "some early lines must be retained under budget"
        );
        let early_lines_before: Vec<String> = terminal
            .scrollback_lines()
            .map(|line| line.cells().iter().map(crate::Cell::character).collect())
            .collect();
        assert!(
            early_lines_before
                .iter()
                .any(|line| line.contains("early-line")),
            "at least one early line must still be present before the oversize burst"
        );

        // One unbroken line, with no `\r\n`, comfortably larger than a
        // typical line but nowhere near the entire budget by itself.
        terminal.ingest(&vec![b'x'; 400]);
        for index in 0..5 {
            terminal.ingest(format!("late-line-{index}\r\n").as_bytes());
        }

        let stats = terminal.scrollback_stats();
        assert!(stats.charged_bytes() <= stats.limit_bytes());

        let lines_after: Vec<String> = terminal
            .scrollback_lines()
            .map(|line| line.cells().iter().map(crate::Cell::character).collect())
            .collect();
        assert!(
            lines_after.iter().any(|line| line.contains("early-line")),
            "an unrelated oversized line elsewhere in the stream must not evict \
             previously retained, in-budget history: {lines_after:?}"
        );

        // Now push a burst so large that it alone exceeds the entire
        // budget; only *this* line's own oldest rows may be discarded (via
        // incremental front-trimming), and total retained bytes must never
        // exceed the configured limit, however this plays out.
        terminal.ingest(&vec![b'y'; 200_000]);
        terminal.ingest(b"\r\n");
        let stats = terminal.scrollback_stats();
        assert!(stats.charged_bytes() <= stats.limit_bytes());
        assert!(
            stats.oversize_lines() > 0,
            "a line that alone exceeds the whole budget must be recorded as oversize"
        );
    }

    #[test]
    fn changing_scrollback_limit_evicts_immediately_and_can_disable_history() {
        let mut terminal = terminal(8, 2);
        terminal.ingest(b"first\r\nsecond\r\nthird\r\n");
        assert!(terminal.scrollback_stats().logical_lines() > 0);

        terminal.set_scrollback_limit(0);
        assert_eq!(terminal.scrollback_stats().limit_bytes(), 0);
        assert_eq!(terminal.scrollback_stats().charged_bytes(), 0);
        assert_eq!(terminal.scrollback_stats().logical_lines(), 0);

        terminal.ingest(b"fourth\r\nfifth\r\n");
        assert_eq!(terminal.scrollback_stats().logical_lines(), 0);
    }

    #[test]
    fn resize_growing_past_a_bottom_row_prompt_still_ingests_new_output() {
        let mut terminal = terminal(20, 6);
        // Fill the screen with 5 filler lines + a prompt, landing the
        // cursor exactly on the last visible row (mirrors "prompt at the
        // bottom row" after two `ls -l`s).
        terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nprompt$ ");
        terminal.resize(Dimensions::new(20, 12).unwrap()).unwrap();
        terminal.ingest(b"foo");
        let found = (0..12)
            .map(|row| terminal.row_text(row).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("")
            .contains("foo");
        assert!(found, "foo should appear somewhere on screen");
    }

    #[test]
    fn reset_to_initial_state_clears_screen_cursor_style_and_modes_but_keeps_scrollback() {
        let mut terminal = terminal(10, 4);
        // Push enough content to build up real scrollback, then leave the
        // screen in a non-default state: colored/bold text, cursor moved
        // away from home, alternate screen active, and a custom scroll
        // region.
        terminal.ingest(b"first\r\nsecond\r\nthird\r\nfourth\r\n");
        terminal.ingest(b"\x1b[1;31mred bold\x1b[3;5H\x1b[?1049h\x1b[2;3r");
        assert_ne!(terminal.attributes(), Attributes::NONE);
        assert_ne!(terminal.foreground(), Color::Default);
        assert!(terminal.scrollback_stats().logical_lines() > 0);
        let scrollback_lines_before = terminal.scrollback_stats().logical_lines();

        terminal.reset_to_initial_state();

        assert_eq!(terminal.cursor().column, 0);
        assert_eq!(terminal.cursor().row, 0);
        assert_eq!(terminal.attributes(), Attributes::NONE);
        assert_eq!(terminal.foreground(), Color::Default);
        assert_eq!(terminal.background(), Color::Default);
        assert!(terminal.title().is_empty());
        assert_eq!(
            terminal.alternate_screen(),
            None,
            "reset should exit the alternate screen"
        );
        for row in 0..terminal.dimensions().rows() {
            assert_eq!(
                terminal.row_text(row).unwrap_or_default().trim(),
                "",
                "the visible screen should be blank after reset"
            );
        }
        // Scrollback history is a separate, opt-in action
        // (`clear_scrollback`) and must survive a display reset.
        assert_eq!(
            terminal.scrollback_stats().logical_lines(),
            scrollback_lines_before
        );

        // The reset terminal must still behave normally afterward: a scroll
        // region set before reset must not still be constraining new
        // output (this is exactly the stale-scroll-region bug class fixed
        // above, so a full reset must not reintroduce it).
        terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\n");
        assert!(
            (0..terminal.dimensions().rows())
                .map(|row| terminal.row_text(row).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("")
                .contains("five"),
            "output after reset should scroll normally"
        );
    }

    #[test]
    fn scroll_region_spanning_full_screen_grows_with_the_screen_on_resize() {
        // Regression test for a real bug: after growing the primary screen
        // (e.g. dragging a window corner to make it both wider and taller),
        // the scroll region's bottom margin stayed pinned to the *old*
        // screen's last row instead of expanding to the new one. Since a
        // shell never sets a custom DECSTBM margin, the region always
        // spans the whole screen (`scroll_top == 0`,
        // `scroll_bottom == rows - 1`) — but the old margin clamp logic
        // only reset the margin when it was inverted, not when it was
        // simply stale-but-still-valid for the smaller old height. That
        // left the margin stuck at the old bottom row, so once the cursor
        // (already past that stale row) received a newline, the terminal
        // never scrolled and instead silently overwrote the last row in
        // place on every subsequent line, discarding all of that output.
        let mut terminal = terminal(20, 6);
        // Build up plenty of prior scrollback (more lines than fit in the
        // original 6-row screen) before landing the cursor on the last
        // visible row, mirroring a bottom-row prompt after real output like
        // `ls -l` that already scrolled several lines into history. This
        // matters because after growing, the reflow anchor resolves the
        // cursor to its logical position across *all* retained content, not
        // just the old screen — with enough prior scrollback, that lands
        // the cursor well below the old screen's last row, exactly where
        // the stale scroll margin bug bites.
        for line in 1..=20 {
            terminal.ingest(format!("line{line}\r\n").as_bytes());
        }
        terminal.ingest(b"prompt$ ");
        terminal
            .resize(Dimensions::new(40, 16).unwrap())
            .expect("grow both columns and rows");
        // Typing a command and pressing enter must still scroll fresh lines
        // into view rather than getting stuck overwriting the last row in
        // place, which (in the buggy version) garbles the final prompt row
        // with leftover fragments of the overwritten text instead of
        // leaving it as a clean, fresh prompt.
        terminal.ingest(b"echo done\r\ndone\r\nprompt$ ");
        let last_row = terminal
            .row_text(terminal.dimensions().rows() - 1)
            .unwrap_or_default();
        assert_eq!(
            last_row.trim_end(),
            "prompt$",
            "the final prompt row should be clean, not overwritten in place"
        );
        let rendered = (0..terminal.dimensions().rows())
            .map(|row| terminal.row_text(row).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("echo done"),
            "typed command should remain visible, got:\n{rendered}"
        );
        assert!(
            rendered.contains("done"),
            "command output should remain visible, got:\n{rendered}"
        );
    }

    #[test]
    fn incremental_grow_matches_a_single_atomic_grow() {
        // Simulate a real live drag: many rapid incremental resize steps
        // (as the window edge moves pixel by pixel) instead of one atomic
        // jump, and compare the final content to a single atomic resize.
        let mut incremental = terminal(20, 6);
        incremental.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nprompt$ ");
        for rows in 7..=12 {
            incremental
                .resize(Dimensions::new(20, rows).unwrap())
                .unwrap();
        }
        incremental.ingest(b"foo");

        let mut atomic = terminal(20, 6);
        atomic.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nprompt$ ");
        atomic.resize(Dimensions::new(20, 12).unwrap()).unwrap();
        atomic.ingest(b"foo");

        let render = |terminal: &Terminal| {
            (0..12)
                .map(|row| terminal.row_text(row).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(render(&incremental), render(&atomic));
    }

    #[test]
    fn jittery_diagonal_grow_preserves_content() {
        // Real corner-drags change both columns and rows together, and
        // mouse movement isn't perfectly monotonic (small jitter/overshoot
        // is common), unlike a clean single-axis resize.
        let mut terminal = terminal(20, 6);
        terminal.ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nprompt$ ");
        let steps: &[(usize, usize)] = &[
            (21, 7),
            (19, 8),
            (23, 7),
            (25, 9),
            (24, 11),
            (28, 10),
            (30, 12),
        ];
        for &(columns, rows) in steps {
            terminal
                .resize(Dimensions::new(columns, rows).unwrap())
                .unwrap();
        }
        terminal.ingest(b"foo");
        let rendered = (0..terminal.dimensions().rows())
            .map(|row| terminal.row_text(row).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("foo"), "foo should still be visible");
        assert!(
            rendered.contains("prompt$"),
            "prompt should still be visible"
        );
        assert!(
            rendered.contains("one"),
            "oldest filler line should survive"
        );
    }
}
