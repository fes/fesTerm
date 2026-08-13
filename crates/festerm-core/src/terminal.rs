use std::{fmt, sync::Arc};

use unicode_width::UnicodeWidthChar;

use crate::{
    cell::{blank_cell, Attributes, Cell, CellWidth, Color},
    history::{LogicalLine, Scrollback, ScrollbackStats, DEFAULT_SCROLLBACK_LIMIT_BYTES},
    input::{
        encode_key, encode_legacy_mouse, encode_paste, encode_sgr_mouse, mouse_event_is_reported,
        paste_encoded_length, FocusEvent, InputEvent, InputEventOutcome, MouseEvent,
    },
    modes::{CursorStyle, MouseTrackingMode, TerminalModes},
    parser::{CsiParameters, OscAction, ParameterSeparator, Parser, TerminalOp},
    replies::{queue_transport_bytes, QueuePushResult},
    screen::Screen,
    unicode::{is_combining_character, Utf8Advance, Utf8Decoder},
    Cursor, Dimensions, TRANSPORT_QUEUE_HIGH_WATERMARK,
};

#[derive(Debug)]
pub struct TerminalError {
    message: String,
}

impl TerminalError {
    pub(crate) fn allocation(resource: &str, error: std::collections::TryReserveError) -> Self {
        Self {
            message: format!("unable to allocate {resource}: {error}"),
        }
    }
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for TerminalError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveScreen {
    Primary,
    Alternate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BufferState {
    screen: Screen,
    cursor: Cursor,
    scroll_top: usize,
    scroll_bottom: usize,
    pending_wrap: bool,
    combining_anchor: Option<Cursor>,
    dec_saved: Option<SavedDecState>,
    ansi_saved: Option<SavedAnsiCursor>,
}

impl BufferState {
    fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        Ok(Self {
            screen: Screen::new(dimensions)?,
            cursor: Cursor { column: 0, row: 0 },
            scroll_top: 0,
            scroll_bottom: dimensions.rows() - 1,
            pending_wrap: false,
            combining_anchor: None,
            dec_saved: None,
            ansi_saved: None,
        })
    }

    fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let mut resized = Self {
            screen: self.screen.resized(dimensions)?,
            cursor: Cursor {
                column: self.cursor.column.min(dimensions.columns() - 1),
                row: self.cursor.row.min(dimensions.rows() - 1),
            },
            scroll_top: self.scroll_top.min(dimensions.rows() - 1),
            scroll_bottom: self.scroll_bottom.min(dimensions.rows() - 1),
            pending_wrap: self.pending_wrap,
            combining_anchor: self
                .combining_anchor
                .filter(|anchor| self.combining_anchor_survives_resize(*anchor, dimensions)),
            dec_saved: self.dec_saved,
            ansi_saved: self.ansi_saved,
        };
        if resized.scroll_top >= resized.scroll_bottom && dimensions.rows() > 1 {
            resized.scroll_top = 0;
            resized.scroll_bottom = dimensions.rows() - 1;
        }
        resized.pending_wrap &= resized.cursor.column + 1 == dimensions.columns();
        resized.clamp_saved_states(dimensions);
        Ok(resized)
    }

    fn combining_anchor_survives_resize(&self, anchor: Cursor, dimensions: Dimensions) -> bool {
        if anchor.column >= dimensions.columns() || anchor.row >= dimensions.rows() {
            return false;
        }
        match self.screen.cell(anchor.column, anchor.row) {
            Some(cell) if cell.width() == CellWidth::Single => true,
            Some(cell) if cell.width() == CellWidth::Double => {
                anchor.column + 1 < dimensions.columns()
                    && self
                        .screen
                        .cell(anchor.column + 1, anchor.row)
                        .is_some_and(|next| next.is_continuation())
            }
            _ => false,
        }
    }

    fn reset(&mut self) {
        self.screen.clear_all(blank_cell());
        self.cursor = Cursor { column: 0, row: 0 };
        self.scroll_top = 0;
        self.scroll_bottom = self.screen.dimensions().rows() - 1;
        self.pending_wrap = false;
        self.combining_anchor = None;
        self.dec_saved = None;
        self.ansi_saved = None;
    }

    fn clamp_saved_states(&mut self, dimensions: Dimensions) {
        if let Some(saved) = &mut self.dec_saved {
            saved.cursor.column = saved.cursor.column.min(dimensions.columns() - 1);
            saved.cursor.row = saved.cursor.row.min(dimensions.rows() - 1);
            if saved.origin_mode {
                saved.cursor.row = saved.cursor.row.clamp(self.scroll_top, self.scroll_bottom);
            }
            saved.pending_wrap &= saved.cursor.column + 1 == dimensions.columns();
        }
        if let Some(saved) = &mut self.ansi_saved {
            saved.cursor.column = saved.cursor.column.min(dimensions.columns() - 1);
            saved.cursor.row = saved.cursor.row.min(dimensions.rows() - 1);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedDecState {
    cursor: Cursor,
    pending_wrap: bool,
    attributes: Attributes,
    foreground: Color,
    background: Color,
    origin_mode: bool,
    auto_wrap: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SavedAnsiCursor {
    cursor: Cursor,
}

/// GUI-independent terminal state. The terminal owns one logical writer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terminal {
    parser: Parser,
    utf8: Utf8Decoder,
    primary: BufferState,
    alternate: Option<BufferState>,
    active_screen: ActiveScreen,
    modes: TerminalModes,
    cursor_style: CursorStyle,
    /// Whether the running program has ever requested a cursor style via
    /// DECSCUSR. GUI front ends use this to distinguish "the spec-mandated
    /// blinking-block reset state" from "no preference has been expressed
    /// yet", so they can apply their own default appearance until a program
    /// actually asks for something specific, without changing what
    /// `cursor_style()` itself reports (still spec-accurate either way).
    cursor_style_set: bool,
    tab_stops: Vec<bool>,
    current_attributes: Attributes,
    current_foreground: Color,
    current_background: Color,
    title: String,
    current_hyperlink: Option<Arc<str>>,
    reply_queue: Vec<u8>,
    input_queue: Vec<u8>,
    reply_queue_overflowed: bool,
    input_queue_overflowed: bool,
    scrollback: Scrollback,
}

impl Terminal {
    pub fn new(dimensions: Dimensions) -> Result<Self, TerminalError> {
        Self::with_scrollback_limit(dimensions, DEFAULT_SCROLLBACK_LIMIT_BYTES)
    }

    /// Creates a terminal with an explicit retained primary-history byte limit.
    pub fn with_scrollback_limit(
        dimensions: Dimensions,
        scrollback_limit_bytes: usize,
    ) -> Result<Self, TerminalError> {
        Ok(Self {
            parser: Parser::new(),
            utf8: Utf8Decoder::new(),
            primary: BufferState::new(dimensions)?,
            alternate: None,
            active_screen: ActiveScreen::Primary,
            modes: TerminalModes::default(),
            cursor_style: CursorStyle::default(),
            cursor_style_set: false,
            tab_stops: default_tab_stops(dimensions),
            current_attributes: Attributes::NONE,
            current_foreground: Color::Default,
            current_background: Color::Default,
            title: String::new(),
            current_hyperlink: None,
            reply_queue: Vec::new(),
            input_queue: Vec::new(),
            reply_queue_overflowed: false,
            input_queue_overflowed: false,
            scrollback: Scrollback::new(scrollback_limit_bytes),
        })
    }

    pub const fn dimensions(&self) -> Dimensions {
        self.primary.screen.dimensions()
    }

    pub const fn cursor(&self) -> Cursor {
        match self.active_screen {
            ActiveScreen::Primary => self.primary.cursor,
            ActiveScreen::Alternate => match &self.alternate {
                Some(alternate) => alternate.cursor,
                None => self.primary.cursor,
            },
        }
    }

    pub const fn modes(&self) -> TerminalModes {
        self.modes
    }

    pub const fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    /// Whether a running program has ever requested a cursor style via
    /// DECSCUSR (`set_cursor_style`). GUI front ends can use this to apply
    /// their own default cursor appearance until a program actually
    /// expresses a preference, without affecting what `cursor_style()`
    /// itself reports.
    pub const fn cursor_style_requested_by_program(&self) -> bool {
        self.cursor_style_set
    }

    pub const fn attributes(&self) -> Attributes {
        self.current_attributes
    }

    pub const fn foreground(&self) -> Color {
        self.current_foreground
    }

    pub const fn background(&self) -> Color {
        self.current_background
    }

    /// Returns the current OSC 0/2 window title after control sanitization.
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn screen(&self) -> &Screen {
        &self.active_buffer().screen
    }

    pub fn primary_screen(&self) -> &Screen {
        &self.primary.screen
    }

    pub fn alternate_screen(&self) -> Option<&Screen> {
        self.alternate.as_ref().map(|alternate| &alternate.screen)
    }

    pub fn cell(&self, column: usize, row: usize) -> Option<Cell> {
        self.screen().cell(column, row)
    }

    /// Borrows a visible cell without cloning it.
    pub fn cell_ref(&self, column: usize, row: usize) -> Option<&Cell> {
        self.screen().cell_ref(column, row)
    }

    pub fn row_text(&self, row: usize) -> Option<String> {
        self.screen().row_text(row)
    }

    /// Borrows retained primary-screen logical lines from oldest to newest.
    pub fn scrollback_lines(&self) -> impl ExactSizeIterator<Item = &LogicalLine> {
        self.scrollback.lines()
    }

    /// Returns content-free retained-history accounting and eviction metrics.
    pub fn scrollback_stats(&self) -> ScrollbackStats {
        self.scrollback.stats()
    }

    /// Clears retained primary-screen history without changing the visible grid.
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    pub fn is_row_dirty(&self, row: usize) -> Option<bool> {
        self.screen().is_row_dirty(row)
    }

    pub fn ingest(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.ingest_byte(byte);
        }
    }

    fn ingest_byte(&mut self, byte: u8) {
        if self.utf8.pending() {
            match self.utf8.advance(byte) {
                Utf8Advance::Pending => return,
                Utf8Advance::Character(character) => {
                    self.print(character);
                    return;
                }
                Utf8Advance::Invalid => {
                    self.print(char::REPLACEMENT_CHARACTER);
                    self.ingest_byte(byte);
                    return;
                }
            }
        }

        if self.parser.is_ground() && byte >= 0x80 {
            if !self.utf8.start(byte) {
                self.print(char::REPLACEMENT_CHARACTER);
            }
            return;
        }

        let operation = self.parser.advance(byte);
        self.apply(operation);
        let action = self.parser.take_osc_action();
        self.apply_osc_action(action);
    }

    /// Resizes grids without reflow, retaining the upper-left intersection.
    pub fn resize(&mut self, dimensions: Dimensions) -> Result<(), TerminalError> {
        let primary = self.primary.resized(dimensions)?;
        let alternate = match &self.alternate {
            Some(alternate) => Some(alternate.resized(dimensions)?),
            None => None,
        };

        self.primary = primary;
        self.alternate = alternate;
        self.tab_stops = resized_tab_stops(&self.tab_stops, dimensions);
        Ok(())
    }

    pub fn take_dirty_rows(&mut self) -> Vec<usize> {
        self.active_buffer_mut().screen.take_dirty_rows()
    }

    /// Queues an atomic input write for the session transport.
    ///
    /// The write is rejected when it would exceed
    /// [`TRANSPORT_QUEUE_HIGH_WATERMARK`]. Call
    /// [`Self::take_input_queue_overflowed`] to observe automatic or prior
    /// rejected writes.
    pub fn queue_input(&mut self, bytes: &[u8]) -> QueuePushResult {
        let result = queue_transport_bytes(&mut self.input_queue, bytes);
        self.input_queue_overflowed |= result.overflowed();
        result
    }

    pub fn queued_input(&self) -> &[u8] {
        &self.input_queue
    }

    pub fn drain_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.input_queue)
    }

    /// Reports and clears whether an input write overflowed since the last call.
    pub fn take_input_queue_overflowed(&mut self) -> bool {
        std::mem::take(&mut self.input_queue_overflowed)
    }

    /// Queues an atomic terminal-protocol reply for the session transport.
    ///
    /// The write is rejected when it would exceed
    /// [`TRANSPORT_QUEUE_HIGH_WATERMARK`]. Call
    /// [`Self::take_reply_queue_overflowed`] to observe rejected automatic
    /// replies, including DSR responses.
    pub fn queue_reply(&mut self, bytes: &[u8]) -> QueuePushResult {
        let result = queue_transport_bytes(&mut self.reply_queue, bytes);
        self.reply_queue_overflowed |= result.overflowed();
        result
    }

    pub fn queued_replies(&self) -> &[u8] {
        &self.reply_queue
    }

    pub fn drain_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.reply_queue)
    }

    /// Reports and clears whether a reply write overflowed since the last call.
    pub fn take_reply_queue_overflowed(&mut self) -> bool {
        std::mem::take(&mut self.reply_queue_overflowed)
    }

    /// Encodes one typed UI event according to the active terminal modes.
    ///
    /// Paste is always queued as one atomic write. In bracketed-paste mode the
    /// delimiters and payload therefore either all enter the bounded queue or
    /// none do; marker-looking bytes inside the payload are preserved exactly.
    pub fn handle_input(&mut self, event: InputEvent) -> InputEventOutcome {
        let encoded = match event {
            InputEvent::Key(key) => encode_key(key, self.modes),
            InputEvent::Paste(text) => return self.handle_paste(text),
            InputEvent::Focus(focus) => self.modes.focus_reporting.then(|| match focus {
                FocusEvent::In => b"\x1b[I".to_vec(),
                FocusEvent::Out => b"\x1b[O".to_vec(),
            }),
            InputEvent::Mouse(event) => return self.handle_mouse(event),
        };

        let Some(encoded) = encoded else {
            return InputEventOutcome::Rejected;
        };
        self.queue_encoded_input(&encoded)
    }

    fn handle_paste(&mut self, text: String) -> InputEventOutcome {
        let Some(length) = paste_encoded_length(&text, self.modes) else {
            self.input_queue_overflowed = true;
            return InputEventOutcome::QueueOverflow;
        };
        if length > TRANSPORT_QUEUE_HIGH_WATERMARK {
            self.input_queue_overflowed = true;
            return InputEventOutcome::QueueOverflow;
        }
        let Some(encoded) = encode_paste(text, self.modes) else {
            self.input_queue_overflowed = true;
            return InputEventOutcome::QueueOverflow;
        };
        self.queue_encoded_input(&encoded)
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> InputEventOutcome {
        let tracking = self.modes.mouse_tracking;
        if tracking == MouseTrackingMode::None {
            return InputEventOutcome::SelectionAllowed;
        }
        if !mouse_event_is_reported(event.kind, tracking) {
            return InputEventOutcome::SelectionClaimed;
        }
        let encoded = if self.modes.sgr_mouse {
            encode_sgr_mouse(event)
        } else {
            encode_legacy_mouse(event)
        };
        match encoded {
            Some(encoded) => self.queue_encoded_input(&encoded),
            None => InputEventOutcome::Rejected,
        }
    }

    fn queue_encoded_input(&mut self, encoded: &[u8]) -> InputEventOutcome {
        let result = self.queue_input(encoded);
        if result.overflowed() {
            InputEventOutcome::QueueOverflow
        } else {
            InputEventOutcome::Encoded {
                bytes: result.accepted(),
            }
        }
    }

    pub fn apply(&mut self, operation: TerminalOp) {
        if !matches!(
            operation,
            TerminalOp::Print(_) | TerminalOp::SetGraphicsRendition(_) | TerminalOp::Ignored
        ) {
            self.clear_combining_anchor();
        }
        match operation {
            TerminalOp::Print(character) => self.print(character),
            TerminalOp::CarriageReturn => {
                self.active_buffer_mut().cursor.column = 0;
                self.clear_pending_wrap();
            }
            TerminalOp::LineFeed | TerminalOp::Index => {
                self.index();
                self.clear_pending_wrap();
            }
            TerminalOp::Backspace => {
                let buffer = self.active_buffer_mut();
                buffer.cursor.column = buffer.cursor.column.saturating_sub(1);
                buffer.pending_wrap = false;
            }
            TerminalOp::Tab => {
                self.tab();
                self.clear_pending_wrap();
            }
            TerminalOp::NextLine => {
                self.active_buffer_mut().cursor.column = 0;
                self.index();
                self.clear_pending_wrap();
            }
            TerminalOp::ReverseIndex => {
                self.reverse_index();
                self.clear_pending_wrap();
            }
            TerminalOp::SaveDec => self.save_dec(),
            TerminalOp::RestoreDec => self.restore_dec(),
            TerminalOp::SetTabStop => self.set_tab_stop(),
            TerminalOp::SetApplicationKeypad(enabled) => self.modes.application_keypad = enabled,
            TerminalOp::SetCursorStyle(parameters) => self.set_cursor_style(parameters),
            TerminalOp::CursorUp(parameters) => self.move_vertical(parameters, false),
            TerminalOp::CursorDown(parameters) => self.move_vertical(parameters, true),
            TerminalOp::CursorForward(parameters) => self.move_horizontal(parameters, true),
            TerminalOp::CursorBack(parameters) => self.move_horizontal(parameters, false),
            TerminalOp::CursorNextLine(parameters) => {
                self.move_vertical(parameters, true);
                self.active_buffer_mut().cursor.column = 0;
            }
            TerminalOp::CursorPreviousLine(parameters) => {
                self.move_vertical(parameters, false);
                self.active_buffer_mut().cursor.column = 0;
            }
            TerminalOp::CursorHorizontalAbsolute(parameters) => {
                self.cursor_horizontal_absolute(parameters)
            }
            TerminalOp::CursorPosition(parameters) => self.cursor_position(parameters),
            TerminalOp::VerticalPositionAbsolute(parameters) => {
                self.vertical_position_absolute(parameters)
            }
            TerminalOp::EraseDisplay(parameters) => self.erase_display(parameters),
            TerminalOp::EraseLine(parameters) => self.erase_line(parameters),
            TerminalOp::EraseCharacters(parameters) => self.erase_characters(parameters),
            TerminalOp::InsertCharacters(parameters) => self.insert_characters(parameters),
            TerminalOp::DeleteCharacters(parameters) => self.delete_characters(parameters),
            TerminalOp::InsertLines(parameters) => self.insert_lines(parameters),
            TerminalOp::DeleteLines(parameters) => self.delete_lines(parameters),
            TerminalOp::ScrollUp(parameters) => self.scroll_up(parameters),
            TerminalOp::ScrollDown(parameters) => self.scroll_down(parameters),
            TerminalOp::SetScrollRegion(parameters) => self.set_scroll_region(parameters),
            TerminalOp::SaveAnsi => self.save_ansi(),
            TerminalOp::RestoreAnsi => self.restore_ansi(),
            TerminalOp::SetGraphicsRendition(parameters) => self.set_graphics_rendition(parameters),
            TerminalOp::SetModes {
                private,
                enabled,
                parameters,
            } => self.set_modes(private, enabled, parameters),
            TerminalOp::DeviceStatus(parameters) => self.device_status(parameters),
            TerminalOp::DeviceAttributes { secondary } => self.device_attributes(secondary),
            TerminalOp::ClearTabStops(parameters) => self.clear_tab_stops(parameters),
            TerminalOp::Ignored => {}
        }
    }

    fn active_buffer(&self) -> &BufferState {
        match self.active_screen {
            ActiveScreen::Primary => &self.primary,
            ActiveScreen::Alternate => self
                .alternate
                .as_ref()
                .expect("alternate state exists while active"),
        }
    }

    fn apply_osc_action(&mut self, action: Option<OscAction>) {
        match action {
            Some(OscAction::SetTitle(title)) => self.title = title,
            Some(OscAction::SetHyperlink(hyperlink)) => self.current_hyperlink = hyperlink,
            None => {}
        }
    }

    fn active_buffer_mut(&mut self) -> &mut BufferState {
        match self.active_screen {
            ActiveScreen::Primary => &mut self.primary,
            ActiveScreen::Alternate => self
                .alternate
                .as_mut()
                .expect("alternate state exists while active"),
        }
    }

    fn erase_cell(&self) -> Cell {
        Cell {
            text: " ".to_owned(),
            width: CellWidth::Single,
            foreground: self.current_foreground,
            background: self.current_background,
            attributes: self.current_attributes,
            hyperlink: None,
        }
    }

    fn print(&mut self, character: char) {
        if is_combining_character(character) {
            self.append_combining(character);
            return;
        }
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 {
            self.clear_combining_anchor();
            return;
        }
        if self.active_buffer().pending_wrap && self.modes.auto_wrap {
            let buffer = self.active_buffer_mut();
            buffer.screen.mark_soft_wrapped(buffer.cursor.row);
            buffer.cursor.column = 0;
            buffer.pending_wrap = false;
            self.index();
        }

        let columns = self.dimensions().columns();
        let auto_wrap = self.modes.auto_wrap;
        let width = width.min(2);
        if width == 2 && self.cursor().column() + 1 == columns {
            if auto_wrap {
                let buffer = self.active_buffer_mut();
                buffer.cursor.column = 0;
                buffer.pending_wrap = false;
                self.index();
            } else {
                self.print(char::REPLACEMENT_CHARACTER);
                return;
            }
        }

        let cursor = self.cursor();
        let foreground = self.current_foreground;
        let background = self.current_background;
        let attributes = self.current_attributes;
        let hyperlink = self.current_hyperlink.clone();
        let buffer = self.active_buffer_mut();
        buffer.screen.replace_cluster(
            cursor.column,
            cursor.row,
            Cell {
                text: character.to_string(),
                width: if width == 2 {
                    CellWidth::Double
                } else {
                    CellWidth::Single
                },
                foreground,
                background,
                attributes,
                hyperlink,
            },
        );
        buffer.combining_anchor = Some(cursor);
        if cursor.column + width == columns {
            buffer.pending_wrap = auto_wrap;
        } else {
            buffer.cursor.column += width;
        }
    }

    fn append_combining(&mut self, character: char) {
        let Some(anchor) = self.active_buffer().combining_anchor else {
            return;
        };
        let Some(mut cell) = self.active_buffer().screen.cell(anchor.column, anchor.row) else {
            return;
        };
        if cell.is_continuation() {
            return;
        }
        cell.text.push(character);
        self.active_buffer_mut()
            .screen
            .replace_cell(anchor.column, anchor.row, cell);
    }

    fn clear_combining_anchor(&mut self) {
        self.active_buffer_mut().combining_anchor = None;
    }

    fn index(&mut self) {
        let fill = self.erase_cell();
        let dimensions = self.dimensions();
        let retain_history = self.active_screen == ActiveScreen::Primary
            && self.primary.scroll_top == 0
            && self.primary.scroll_bottom + 1 == dimensions.rows();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_bottom {
            let removed = buffer
                .screen
                .scroll_up(buffer.scroll_top, buffer.scroll_bottom, 1, fill);
            if retain_history {
                self.scrollback.push_rows(removed);
            }
        } else if buffer.cursor.row + 1 < dimensions.rows() {
            buffer.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        let fill = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_top {
            buffer
                .screen
                .scroll_down(buffer.scroll_top, buffer.scroll_bottom, 1, fill);
        } else {
            buffer.cursor.row = buffer.cursor.row.saturating_sub(1);
        }
    }

    fn tab(&mut self) {
        let columns = self.dimensions().columns();
        let cursor_column = self.cursor().column;
        let next_tab_stop = self
            .tab_stops
            .iter()
            .enumerate()
            .skip(cursor_column.saturating_add(1))
            .find_map(|(column, set)| set.then_some(column))
            .unwrap_or(columns - 1);
        self.active_buffer_mut().cursor.column = next_tab_stop;
    }

    fn set_tab_stop(&mut self) {
        let column = self.cursor().column;
        if let Some(tab_stop) = self.tab_stops.get_mut(column) {
            *tab_stop = true;
        }
    }

    fn clear_tab_stops(&mut self, parameters: CsiParameters) {
        match Self::raw_parameter(parameters, 0, 0) {
            0 => {
                let column = self.cursor().column;
                if let Some(tab_stop) = self.tab_stops.get_mut(column) {
                    *tab_stop = false;
                }
            }
            3 => self.tab_stops.fill(false),
            _ => {}
        }
    }

    fn set_cursor_style(&mut self, parameters: CsiParameters) {
        self.cursor_style = match Self::raw_parameter(parameters, 0, 0) {
            0 | 1 => CursorStyle::BlinkingBlock,
            2 => CursorStyle::SteadyBlock,
            3 => CursorStyle::BlinkingUnderline,
            4 => CursorStyle::SteadyUnderline,
            5 => CursorStyle::BlinkingBar,
            6 => CursorStyle::SteadyBar,
            _ => return,
        };
        self.cursor_style_set = true;
    }

    fn clear_pending_wrap(&mut self) {
        self.active_buffer_mut().pending_wrap = false;
    }

    fn parameter_or(parameters: CsiParameters, index: usize, default: usize) -> usize {
        match parameters.value(index) {
            Some(0) | None => default,
            Some(value) => usize::from(value),
        }
    }

    fn raw_parameter(parameters: CsiParameters, index: usize, default: usize) -> usize {
        parameters.value(index).map_or(default, usize::from)
    }

    fn vertical_bounds(&self) -> (usize, usize) {
        if self.modes.origin_mode {
            let buffer = self.active_buffer();
            (buffer.scroll_top, buffer.scroll_bottom)
        } else {
            (0, self.dimensions().rows() - 1)
        }
    }

    fn move_vertical(&mut self, parameters: CsiParameters, down: bool) {
        let count = Self::parameter_or(parameters, 0, 1);
        let (top, bottom) = self.vertical_bounds();
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = if down {
            buffer.cursor.row.saturating_add(count).min(bottom)
        } else {
            buffer.cursor.row.saturating_sub(count).max(top)
        };
        buffer.pending_wrap = false;
    }

    fn move_horizontal(&mut self, parameters: CsiParameters, forward: bool) {
        let count = Self::parameter_or(parameters, 0, 1);
        let columns = self.dimensions().columns();
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = if forward {
            buffer.cursor.column.saturating_add(count).min(columns - 1)
        } else {
            buffer.cursor.column.saturating_sub(count)
        };
        buffer.pending_wrap = false;
    }

    fn cursor_horizontal_absolute(&mut self, parameters: CsiParameters) {
        let column = Self::parameter_or(parameters, 0, 1) - 1;
        let columns = self.dimensions().columns();
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = column.min(columns - 1);
        buffer.pending_wrap = false;
    }

    fn cursor_position(&mut self, parameters: CsiParameters) {
        let requested_row = Self::parameter_or(parameters, 0, 1) - 1;
        let requested_column = Self::parameter_or(parameters, 1, 1) - 1;
        let columns = self.dimensions().columns();
        let (top, bottom) = self.vertical_bounds();
        let row = if self.modes.origin_mode {
            top.saturating_add(requested_row).min(bottom)
        } else {
            requested_row.min(bottom)
        };
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = row;
        buffer.cursor.column = requested_column.min(columns - 1);
        buffer.pending_wrap = false;
    }

    fn vertical_position_absolute(&mut self, parameters: CsiParameters) {
        let requested_row = Self::parameter_or(parameters, 0, 1) - 1;
        let (top, bottom) = self.vertical_bounds();
        let row = if self.modes.origin_mode {
            top.saturating_add(requested_row).min(bottom)
        } else {
            requested_row.min(bottom)
        };
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = row;
        buffer.pending_wrap = false;
    }

    fn erase_display(&mut self, parameters: CsiParameters) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        if mode == 3 {
            if self.active_screen == ActiveScreen::Primary {
                self.scrollback.clear();
            }
            return;
        }
        let columns = self.dimensions().columns();
        let rows = self.dimensions().rows();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let screen = &mut self.active_buffer_mut().screen;
        match mode {
            0 => screen.fill_linear(cursor.row * columns + cursor.column, columns * rows, cell),
            1 => screen.fill_linear(0, cursor.row * columns + cursor.column + 1, cell),
            2 => screen.clear_all(cell),
            _ => {}
        }
    }

    fn erase_line(&mut self, parameters: CsiParameters) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let start = cursor.row * columns;
        let screen = &mut self.active_buffer_mut().screen;
        match mode {
            0 => screen.fill_linear(start + cursor.column, start + columns, cell),
            1 => screen.fill_linear(start, start + cursor.column + 1, cell),
            2 => screen.fill_linear(start, start + columns, cell),
            _ => {}
        }
    }

    fn erase_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let start = cursor.row * columns + cursor.column;
        let end = start.saturating_add(count).min((cursor.row + 1) * columns);
        self.active_buffer_mut()
            .screen
            .fill_linear(start, end, cell);
    }

    fn insert_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut()
            .screen
            .insert_characters(cursor.column, cursor.row, count, cell);
    }

    fn delete_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut()
            .screen
            .delete_characters(cursor.column, cursor.row, count, cell);
    }

    fn insert_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .insert_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell);
        }
    }

    fn delete_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .delete_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell);
        }
    }

    fn scroll_up(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let dimensions = self.dimensions();
        let retain_history = self.active_screen == ActiveScreen::Primary
            && self.primary.scroll_top == 0
            && self.primary.scroll_bottom + 1 == dimensions.rows();
        let buffer = self.active_buffer_mut();
        let removed = buffer
            .screen
            .scroll_up(buffer.scroll_top, buffer.scroll_bottom, count, cell);
        if retain_history {
            self.scrollback.push_rows(removed);
        }
    }

    fn scroll_down(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        buffer
            .screen
            .scroll_down(buffer.scroll_top, buffer.scroll_bottom, count, cell);
    }

    fn set_scroll_region(&mut self, parameters: CsiParameters) {
        let rows = self.dimensions().rows();
        let top = Self::parameter_or(parameters, 0, 1) - 1;
        let bottom = Self::parameter_or(parameters, 1, rows) - 1;
        if top >= bottom || bottom >= rows {
            return;
        }
        let origin_mode = self.modes.origin_mode;
        let buffer = self.active_buffer_mut();
        buffer.scroll_top = top;
        buffer.scroll_bottom = bottom;
        buffer.cursor.column = 0;
        buffer.cursor.row = if origin_mode { top } else { 0 };
        buffer.pending_wrap = false;
    }

    fn save_dec(&mut self) {
        let attributes = self.current_attributes;
        let foreground = self.current_foreground;
        let background = self.current_background;
        let origin_mode = self.modes.origin_mode;
        let auto_wrap = self.modes.auto_wrap;
        let buffer = self.active_buffer_mut();
        buffer.dec_saved = Some(SavedDecState {
            cursor: buffer.cursor,
            pending_wrap: buffer.pending_wrap,
            attributes,
            foreground,
            background,
            origin_mode,
            auto_wrap,
        });
    }

    fn restore_dec(&mut self) {
        let Some(saved) = self.active_buffer().dec_saved else {
            return;
        };
        let dimensions = self.dimensions();
        let buffer = self.active_buffer_mut();
        buffer.cursor = Cursor {
            column: saved.cursor.column.min(dimensions.columns() - 1),
            row: saved.cursor.row.min(dimensions.rows() - 1),
        };
        if saved.origin_mode {
            buffer.cursor.row = buffer
                .cursor
                .row
                .clamp(buffer.scroll_top, buffer.scroll_bottom);
        }
        buffer.pending_wrap =
            saved.pending_wrap && buffer.cursor.column + 1 == dimensions.columns();
        self.current_attributes = saved.attributes;
        self.current_foreground = saved.foreground;
        self.current_background = saved.background;
        self.modes.origin_mode = saved.origin_mode;
        self.modes.auto_wrap = saved.auto_wrap;
    }

    fn save_ansi(&mut self) {
        let cursor = self.cursor();
        self.active_buffer_mut().ansi_saved = Some(SavedAnsiCursor { cursor });
    }

    fn restore_ansi(&mut self) {
        let Some(saved) = self.active_buffer().ansi_saved else {
            return;
        };
        let dimensions = self.dimensions();
        let buffer = self.active_buffer_mut();
        buffer.cursor = Cursor {
            column: saved.cursor.column.min(dimensions.columns() - 1),
            row: saved.cursor.row.min(dimensions.rows() - 1),
        };
        buffer.pending_wrap = false;
    }

    fn set_graphics_rendition(&mut self, parameters: CsiParameters) {
        if parameters.is_empty() {
            self.reset_graphics_rendition();
            return;
        }

        let mut index = 0;
        while index < parameters.len() {
            let Some(code) = parameters.value(index) else {
                break;
            };
            match code {
                0 => self.reset_graphics_rendition(),
                1 => self.current_attributes = self.current_attributes.with(Attributes::BOLD),
                2 => self.current_attributes = self.current_attributes.with(Attributes::FAINT),
                3 => self.current_attributes = self.current_attributes.with(Attributes::ITALIC),
                4 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::DOUBLE_UNDERLINE)
                        .with(Attributes::UNDERLINE)
                }
                5 => self.current_attributes = self.current_attributes.with(Attributes::SLOW_BLINK),
                6 => {
                    self.current_attributes = self.current_attributes.with(Attributes::RAPID_BLINK)
                }
                7 => self.current_attributes = self.current_attributes.with(Attributes::INVERSE),
                8 => self.current_attributes = self.current_attributes.with(Attributes::CONCEALED),
                9 => {
                    self.current_attributes =
                        self.current_attributes.with(Attributes::STRIKETHROUGH)
                }
                21 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::UNDERLINE)
                        .with(Attributes::DOUBLE_UNDERLINE)
                }
                22 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::BOLD)
                        .without(Attributes::FAINT)
                }
                23 => self.current_attributes = self.current_attributes.without(Attributes::ITALIC),
                24 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::UNDERLINE)
                        .without(Attributes::DOUBLE_UNDERLINE)
                }
                25 => {
                    self.current_attributes = self
                        .current_attributes
                        .without(Attributes::SLOW_BLINK)
                        .without(Attributes::RAPID_BLINK)
                }
                27 => {
                    self.current_attributes = self.current_attributes.without(Attributes::INVERSE)
                }
                28 => {
                    self.current_attributes = self.current_attributes.without(Attributes::CONCEALED)
                }
                29 => {
                    self.current_attributes =
                        self.current_attributes.without(Attributes::STRIKETHROUGH)
                }
                30..=37 => self.current_foreground = Color::Indexed((code - 30) as u8),
                39 => self.current_foreground = Color::Default,
                40..=47 => self.current_background = Color::Indexed((code - 40) as u8),
                49 => self.current_background = Color::Default,
                90..=97 => self.current_foreground = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.current_background = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    let foreground = code == 38;
                    index = self.set_extended_color(parameters, index, foreground);
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn set_extended_color(
        &mut self,
        parameters: CsiParameters,
        index: usize,
        foreground: bool,
    ) -> usize {
        let Some(mode) = parameters.value(index + 1) else {
            return index;
        };
        let separator = parameters.separator(index + 1);
        let color = match (separator, mode) {
            (Some(ParameterSeparator::Semicolon), 5)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Semicolon) =>
            {
                parameters
                    .value(index + 2)
                    .and_then(|value| u8::try_from(value).ok())
                    .map(Color::Indexed)
            }
            (Some(ParameterSeparator::Semicolon), 2)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Semicolon)
                    && parameters.separator(index + 3) == Some(ParameterSeparator::Semicolon)
                    && parameters.separator(index + 4) == Some(ParameterSeparator::Semicolon) =>
            {
                match (
                    parameters.value(index + 2),
                    parameters.value(index + 3),
                    parameters.value(index + 4),
                ) {
                    (Some(red), Some(green), Some(blue)) => {
                        match (u8::try_from(red), u8::try_from(green), u8::try_from(blue)) {
                            (Ok(red), Ok(green), Ok(blue)) => Some(Color::Rgb { red, green, blue }),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            // Canonical colon true-color syntax reserves an empty color-space
            // subparameter: `38:2::red:green:blue`.
            (Some(ParameterSeparator::Colon), 5)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon) =>
            {
                parameters
                    .value(index + 2)
                    .and_then(|value| u8::try_from(value).ok())
                    .map(Color::Indexed)
            }
            (Some(ParameterSeparator::Colon), 2)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 3) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 4) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 5) == Some(ParameterSeparator::Colon) =>
            {
                match (
                    parameters.value(index + 3),
                    parameters.value(index + 4),
                    parameters.value(index + 5),
                ) {
                    (Some(red), Some(green), Some(blue)) => {
                        match (u8::try_from(red), u8::try_from(green), u8::try_from(blue)) {
                            (Ok(red), Ok(green), Ok(blue)) => Some(Color::Rgb { red, green, blue }),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(color) = color {
            if foreground {
                self.current_foreground = color;
            } else {
                self.current_background = color;
            }
        }
        match (separator, mode) {
            (Some(ParameterSeparator::Semicolon), 5) => index + 2,
            (Some(ParameterSeparator::Semicolon), 2) => index + 4,
            (Some(ParameterSeparator::Colon), 5) => index + 2,
            (Some(ParameterSeparator::Colon), 2) => index + 5,
            _ => index,
        }
    }

    fn reset_graphics_rendition(&mut self) {
        self.current_attributes = Attributes::NONE;
        self.current_foreground = Color::Default;
        self.current_background = Color::Default;
    }

    fn set_modes(&mut self, private: bool, enabled: bool, parameters: CsiParameters) {
        if !private {
            return;
        }
        for index in 0..parameters.len() {
            let Some(mode) = parameters.value(index) else {
                continue;
            };
            match mode {
                1 => self.modes.application_cursor = enabled,
                6 => {
                    self.modes.origin_mode = enabled;
                    self.home_cursor();
                }
                7 => {
                    self.modes.auto_wrap = enabled;
                    if !enabled {
                        self.clear_pending_wrap();
                    }
                }
                25 => self.modes.cursor_visible = enabled,
                9 => self.set_mouse_tracking(MouseTrackingMode::X10, enabled),
                1000 => self.set_mouse_tracking(MouseTrackingMode::ButtonEvent, enabled),
                1002 => self.set_mouse_tracking(MouseTrackingMode::ButtonMotion, enabled),
                1003 => self.set_mouse_tracking(MouseTrackingMode::AnyMotion, enabled),
                1004 => self.modes.focus_reporting = enabled,
                1006 => self.modes.sgr_mouse = enabled,
                2004 => self.modes.bracketed_paste = enabled,
                47 => {
                    if enabled {
                        self.enter_alternate(false);
                    } else {
                        self.leave_alternate(false);
                    }
                }
                1047 => {
                    if enabled {
                        self.enter_alternate(true);
                    } else {
                        self.leave_alternate(true);
                    }
                }
                1048 => {
                    if enabled {
                        self.save_dec();
                    } else {
                        self.restore_dec();
                    }
                }
                1049 => {
                    if enabled {
                        self.save_dec();
                        self.enter_alternate(true);
                    } else {
                        self.leave_alternate(true);
                        self.restore_dec();
                    }
                }
                _ => {}
            }
        }
    }

    fn set_mouse_tracking(&mut self, requested: MouseTrackingMode, enabled: bool) {
        if enabled {
            self.modes.mouse_tracking = requested;
        } else if self.modes.mouse_tracking == requested {
            self.modes.mouse_tracking = MouseTrackingMode::None;
        }
    }

    fn enter_alternate(&mut self, clear: bool) {
        if self.active_screen == ActiveScreen::Alternate {
            return;
        }
        if self.alternate.is_none() {
            let Ok(alternate) = BufferState::new(self.dimensions()) else {
                return;
            };
            self.alternate = Some(alternate);
        }
        if clear {
            let cell = self.erase_cell();
            let rows = self.dimensions().rows();
            let alternate = self
                .alternate
                .as_mut()
                .expect("alternate buffer was just allocated");
            alternate.screen.clear_all(cell);
            alternate.cursor = Cursor { column: 0, row: 0 };
            alternate.scroll_top = 0;
            alternate.scroll_bottom = rows - 1;
            alternate.pending_wrap = false;
            alternate.combining_anchor = None;
        }
        self.active_screen = ActiveScreen::Alternate;
        self.modes.alternate_screen = true;
        self.active_buffer_mut().screen.mark_all_dirty();
    }

    fn leave_alternate(&mut self, reset: bool) {
        if self.active_screen == ActiveScreen::Alternate {
            self.active_screen = ActiveScreen::Primary;
            self.primary.screen.mark_all_dirty();
        }
        if reset {
            if let Some(alternate) = &mut self.alternate {
                alternate.reset();
            }
        }
        self.modes.alternate_screen = false;
    }

    fn home_cursor(&mut self) {
        let origin_mode = self.modes.origin_mode;
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = 0;
        buffer.cursor.row = if origin_mode { buffer.scroll_top } else { 0 };
        buffer.pending_wrap = false;
    }

    fn device_status(&mut self, parameters: CsiParameters) {
        match parameters.value(0) {
            Some(5) => {
                self.queue_reply(b"\x1b[0n");
            }
            Some(6) => {
                let cursor = self.cursor();
                let row = if self.modes.origin_mode {
                    cursor
                        .row
                        .saturating_sub(self.active_buffer().scroll_top)
                        .saturating_add(1)
                } else {
                    cursor.row.saturating_add(1)
                };
                let reply = format!("\x1b[{row};{}R", cursor.column.saturating_add(1));
                self.queue_reply(reply.as_bytes());
            }
            _ => {}
        }
    }

    fn device_attributes(&mut self, secondary: bool) {
        if secondary {
            self.queue_reply(b"\x1b[>0;0;0c");
        } else {
            // VT102 is the most conservative identity compatible with the
            // implemented ANSI/DEC subset; do not advertise unsupported
            // xterm extensions through DA feature codes.
            self.queue_reply(b"\x1b[?6c");
        }
    }
}

fn default_tab_stops(dimensions: Dimensions) -> Vec<bool> {
    (0..dimensions.columns())
        .map(|column| column != 0 && column % 8 == 0)
        .collect()
}

fn resized_tab_stops(existing: &[bool], dimensions: Dimensions) -> Vec<bool> {
    (0..dimensions.columns())
        .map(|column| {
            existing
                .get(column)
                .copied()
                .unwrap_or(column != 0 && column % 8 == 0)
        })
        .collect()
}
