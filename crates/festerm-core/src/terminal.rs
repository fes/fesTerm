use std::{fmt, sync::Arc};

use compact_str::CompactString;
use unicode_width::UnicodeWidthChar;

use crate::{
    cell::{blank_cell, Attributes, Cell, CellWidth, Color},
    history::{LogicalLine, Scrollback, ScrollbackStats, DEFAULT_SCROLLBACK_LIMIT_BYTES},
    input::{
        encode_key, encode_legacy_mouse, encode_paste, encode_sgr_mouse, mouse_event_is_reported,
        paste_encoded_length, FocusEvent, InputEvent, InputEventOutcome, MouseEvent,
    },
    modes::{CursorStyle, MouseTrackingMode, TerminalModes},
    parser::{CsiParameters, DcsAction, OscAction, ParameterSeparator, Parser, TerminalOp},
    replies::{queue_transport_bytes, QueuePushResult},
    screen::{ColumnSpan, Screen},
    unicode::{extends_grapheme, grapheme_width, Utf8Advance, Utf8Decoder, MAX_GRAPHEME_BYTES},
    Cursor, Dimensions, TRANSPORT_QUEUE_HIGH_WATERMARK,
};

/// A physical cell position in the combined retained-history and primary
/// screen row stream, counted from the oldest retained row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentPosition {
    pub column: usize,
    pub absolute_row: u64,
}

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
    scroll_left: usize,
    scroll_right: usize,
    pending_wrap: bool,
    grapheme_anchor: Option<Cursor>,
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
            scroll_left: 0,
            scroll_right: dimensions.columns() - 1,
            pending_wrap: false,
            grapheme_anchor: None,
            dec_saved: None,
            ansi_saved: None,
        })
    }

    fn resized(&self, dimensions: Dimensions) -> Result<Self, TerminalError> {
        let old_rows = self.screen.dimensions().rows();
        // A scroll region spanning the full old screen (the default when no
        // app has set a custom DECSTBM margin) must keep spanning the full
        // screen after growing taller; otherwise it stays pinned to the old,
        // now-undersized row range and later output addressed below it can
        // never trigger a scroll, silently overwriting rows in place instead
        // of appending a new one.
        let was_full_screen = self.scroll_top == 0 && self.scroll_bottom + 1 == old_rows;
        let (scroll_left, scroll_right) = self.resized_horizontal_margins(dimensions);
        let mut resized = Self {
            screen: self.screen.resized(dimensions)?,
            cursor: Cursor {
                column: self.cursor.column.min(dimensions.columns() - 1),
                row: self.cursor.row.min(dimensions.rows() - 1),
            },
            scroll_top: self.scroll_top.min(dimensions.rows() - 1),
            scroll_bottom: self.scroll_bottom.min(dimensions.rows() - 1),
            scroll_left,
            scroll_right,
            pending_wrap: self.pending_wrap,
            grapheme_anchor: self
                .grapheme_anchor
                .filter(|anchor| self.grapheme_anchor_survives_resize(*anchor, dimensions)),
            dec_saved: self.dec_saved,
            ansi_saved: self.ansi_saved,
        };
        if was_full_screen || (resized.scroll_top >= resized.scroll_bottom && dimensions.rows() > 1)
        {
            resized.scroll_top = 0;
            resized.scroll_bottom = dimensions.rows() - 1;
        }
        resized.pending_wrap &= resized.cursor.column + 1 == dimensions.columns();
        resized.clamp_saved_states(dimensions);
        Ok(resized)
    }

    /// Resizes the primary buffer by unified logical reflow instead of the
    /// rectangular clip/pad model: the current screen is folded into
    /// `scrollback` exactly as a full-viewport scroll would, the combined
    /// logical content is reflowed at the new width, and the trailing
    /// `dimensions.rows()` physical rows become the new visible screen
    /// while everything above stays retained history. `scrollback` is
    /// updated in place. This matches ADR 0017's primary-resize model:
    /// reconstruct physical rows from logical lines, then map the cursor
    /// through a stable logical position (logical-line identity plus
    /// cell-stream offset) rather than clamping raw coordinates.
    fn reflowed(
        &self,
        dimensions: Dimensions,
        scrollback: &mut Scrollback,
        positions: &[ContentPosition],
    ) -> Result<(Self, Vec<Option<ContentPosition>>), TerminalError> {
        let old_dimensions = self.screen.dimensions();
        let rows_before_screen = scrollback.total_physical_rows();
        let stats_before = scrollback.stats();
        let old_content_row_origin = stats_before.content_row_origin();
        let old_screen_row_origin = stats_before.screen_row_origin();
        let scrollback_limit = stats_before.limit_bytes();
        // Only fold rows up through the cursor's own row or the last row
        // with any occupied content, whichever is greater. Wholly-blank
        // trailing rows are not real logical content; folding them in
        // would otherwise become phantom empty logical lines that inflate
        // the combined row count and push real content further into
        // history than it belongs (`Screen::from_rows` already leaves
        // unfilled new-screen rows blank by default).
        let content_rows = old_dimensions
            .rows()
            .min(self.screen.occupied_row_count().max(self.cursor.row + 1));
        let mut fold_rows = self.screen.to_rows();
        fold_rows.truncate(content_rows);

        // Take ownership of the current scrollback instead of cloning it:
        // every retained line gets folded into `combined` and (if columns
        // changed) rewrapped anyway, and `scrollback` is fully overwritten
        // with the result below, so cloning first only pays an extra
        // O(retained-lines) copy for content that's about to be replaced
        // or discarded regardless. This matters even when the column width
        // is unchanged (a pure vertical resize, e.g. dragging the bottom
        // edge of the window), since `reflow()` is skipped in that case but
        // the clone previously ran unconditionally on every resize call.
        let mut combined = std::mem::replace(scrollback, Scrollback::new(scrollback_limit));
        // Resize must reflow the complete live screen even when retained
        // history is disabled or near its bound. Apply the real scrollback
        // policy only after the new visible tail has been split back out.
        combined.set_limit_bytes(usize::MAX);
        combined.push_rows(fold_rows);
        let combined_rows = combined.total_physical_rows();
        // Capture the cursor's stable logical anchor using the current
        // (pre-reflow) row boundaries, which still mirror the screen's
        // actual per-row breaks at this point.
        let cursor_absolute_row = rows_before_screen + self.cursor.row.min(content_rows - 1);
        let cursor_anchor = combined.line_and_offset_at(cursor_absolute_row, self.cursor.column);
        let position_anchors = positions
            .iter()
            .map(|position| {
                let relative_row = if position.absolute_row < old_screen_row_origin {
                    position
                        .absolute_row
                        .checked_sub(old_content_row_origin)
                        .and_then(|row| usize::try_from(row).ok())
                        .filter(|row| *row < rows_before_screen)?
                } else {
                    rows_before_screen.checked_add(
                        usize::try_from(position.absolute_row - old_screen_row_origin).ok()?,
                    )?
                };
                (relative_row < combined_rows)
                    .then(|| combined.line_and_offset_at(relative_row, position.column))
                    .flatten()
            })
            .collect::<Vec<_>>();

        if dimensions.columns() != old_dimensions.columns() {
            combined.reflow(dimensions.columns());
        }

        // Resolve the anchor against the just-reflowed (but not yet
        // split) layout while the line it names is still whole: once
        // `split_off_tail` runs, a line straddling the split boundary is
        // truncated to its kept prefix and reuses the same identity for
        // that shorter remainder, so resolving after the split could
        // silently relocate an anchor that belonged in the tail.
        let resolved_anchor = cursor_anchor.and_then(|anchor| combined.resolve_anchor(anchor));
        let resolved_positions = position_anchors
            .into_iter()
            .map(|anchor| anchor.and_then(|anchor| combined.resolve_anchor(anchor)))
            .collect::<Vec<_>>();

        let origin_before_split = combined.stats().content_row_origin();
        let tail_rows = combined.split_off_tail(dimensions.rows());
        combined.set_limit_bytes(scrollback_limit);
        let scrollback_rows_after = combined.total_physical_rows();
        let retained_origin = combined.stats().content_row_origin();
        let screen_origin = combined.stats().screen_row_origin();
        let evicted_rows = usize::try_from(retained_origin.saturating_sub(origin_before_split))
            .unwrap_or(usize::MAX);
        let resolved_positions = resolved_positions
            .into_iter()
            .map(|position| {
                position.and_then(|(column, relative_row)| {
                    let adjusted_row = relative_row.checked_sub(evicted_rows)?;
                    let absolute_row = if adjusted_row < scrollback_rows_after {
                        retained_origin.saturating_add(adjusted_row as u64)
                    } else {
                        screen_origin.saturating_add((adjusted_row - scrollback_rows_after) as u64)
                    };
                    (adjusted_row < scrollback_rows_after + dimensions.rows()).then_some(
                        ContentPosition {
                            column,
                            absolute_row,
                        },
                    )
                })
            })
            .collect();
        let screen = Screen::from_rows(dimensions, tail_rows)?;
        *scrollback = combined;

        let mut cursor = Cursor {
            column: 0,
            row: dimensions.rows() - 1,
        };
        if let Some((column, absolute_row)) = resolved_anchor
            .and_then(|(column, row)| row.checked_sub(evicted_rows).map(|row| (column, row)))
        {
            if absolute_row >= scrollback_rows_after {
                cursor = Cursor {
                    column: column.min(dimensions.columns() - 1),
                    row: (absolute_row - scrollback_rows_after).min(dimensions.rows() - 1),
                };
            }
        }

        // As in `resized`, a scroll region spanning the full old screen must
        // keep spanning the full screen after reflow, or output addressed
        // below the stale margin can never trigger a scroll and instead
        // overwrites the last row in place (see `resized` for details).
        let was_full_screen =
            self.scroll_top == 0 && self.scroll_bottom + 1 == old_dimensions.rows();
        let mut scroll_top = self.scroll_top.min(dimensions.rows() - 1);
        let mut scroll_bottom = self.scroll_bottom.min(dimensions.rows() - 1);
        if was_full_screen || (scroll_top >= scroll_bottom && dimensions.rows() > 1) {
            scroll_top = 0;
            scroll_bottom = dimensions.rows() - 1;
        }

        let (scroll_left, scroll_right) = self.resized_horizontal_margins(dimensions);
        let mut resized = Self {
            screen,
            cursor,
            scroll_top,
            scroll_bottom,
            scroll_left,
            scroll_right,
            pending_wrap: self.pending_wrap,
            // A reflow can move cell content arbitrarily relative to a
            // grapheme anchor's original row/column, so (like the
            // rectangular resize path) it is not carried forward.
            grapheme_anchor: None,
            dec_saved: self.dec_saved,
            ansi_saved: self.ansi_saved,
        };
        resized.pending_wrap &= resized.cursor.column + 1 == dimensions.columns();
        resized.clamp_saved_states(dimensions);
        Ok((resized, resolved_positions))
    }

    /// Left and right margins after a resize.
    ///
    /// Margins spanning the full old width are the default, and must keep
    /// spanning the full width once the screen is wider - otherwise output
    /// would silently wrap at the old right edge. Narrower margins are
    /// clamped, and collapse back to the full width if the screen shrank
    /// past them, because a margin wider than the screen cannot be honoured
    /// and half-honouring it is worse than dropping it.
    fn resized_horizontal_margins(&self, dimensions: Dimensions) -> (usize, usize) {
        let old_columns = self.screen.dimensions().columns();
        let columns = dimensions.columns();
        let was_full_width = self.scroll_left == 0 && self.scroll_right + 1 == old_columns;
        let left = self.scroll_left.min(columns - 1);
        let right = self.scroll_right.min(columns - 1);
        if was_full_width || (left >= right && columns > 1) {
            (0, columns - 1)
        } else {
            (left, right)
        }
    }

    fn grapheme_anchor_survives_resize(&self, anchor: Cursor, dimensions: Dimensions) -> bool {
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
        self.scroll_left = 0;
        self.scroll_right = self.screen.dimensions().columns() - 1;
        self.pending_wrap = false;
        self.grapheme_anchor = None;
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
    /// Whether characters printed from now on are protected from erasure, and
    /// which family of sequences last said so. The source is kept because the
    /// two families disagree about which erases honour it.
    current_protected: bool,
    protection: ProtectionSource,
    current_foreground: Color,
    current_background: Color,
    title: String,
    current_hyperlink: Option<Arc<str>>,
    current_hyperlink_cells_remaining: usize,
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
            current_protected: false,
            protection: ProtectionSource::None,
            current_foreground: Color::Default,
            current_background: Color::Default,
            title: String::new(),
            current_hyperlink: None,
            current_hyperlink_cells_remaining: 0,
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

    #[cfg(test)]
    pub(crate) const fn scrollback_trim_compactions(&self) -> u64 {
        self.scrollback.trim_compactions()
    }

    /// Changes the retained primary-history byte limit and immediately evicts
    /// complete oldest logical lines if the new limit is smaller.
    pub fn set_scrollback_limit(&mut self, limit_bytes: usize) {
        self.scrollback.set_limit_bytes(limit_bytes);
    }

    /// Borrows one retained physical row by its oldest-first history index.
    pub fn scrollback_physical_row(&self, row: usize) -> Option<&[Cell]> {
        self.scrollback.physical_row(row)
    }

    /// Reports whether one retained physical row, by its oldest-first
    /// history index, is soft-wrapped into the next row.
    pub fn scrollback_physical_row_soft_wrapped(&self, row: usize) -> Option<bool> {
        self.scrollback.physical_row_soft_wrapped(row)
    }

    /// Clears retained primary-screen history without changing the visible grid.
    pub fn clear_scrollback(&mut self) {
        self.scrollback.clear();
    }

    /// Resets the terminal's display state to how it looks right after
    /// construction: clears the visible screen, homes the cursor, restores
    /// default colors/attributes/modes/tab stops, exits the alternate
    /// screen if active, and clears any parser byte-stream state (e.g. a
    /// truncated escape sequence). This mirrors what a real terminal does
    /// on `ESC c` (RIS) or a shell's `reset` command, but is invoked
    /// directly by the GUI so it works even if the running program is
    /// wedged and can't be asked to emit that sequence itself.
    ///
    /// Retained scrollback history is left untouched, matching how
    /// terminal emulators typically distinguish "reset the screen" from
    /// "clear the scrollback" as separate user actions; see
    /// [`Self::clear_scrollback`] for the latter.
    pub fn reset_to_initial_state(&mut self) {
        let dimensions = self.dimensions();
        self.primary.reset();
        self.alternate = None;
        self.active_screen = ActiveScreen::Primary;
        self.modes = TerminalModes::default();
        self.cursor_style = CursorStyle::default();
        self.cursor_style_set = false;
        self.tab_stops = default_tab_stops(dimensions);
        self.current_attributes = Attributes::NONE;
        self.current_foreground = Color::Default;
        self.current_background = Color::Default;
        self.title.clear();
        self.current_hyperlink = None;
        self.current_hyperlink_cells_remaining = 0;
        self.parser = Parser::new();
        self.utf8 = Utf8Decoder::new();
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
        let request = self.parser.take_dcs_action();
        self.apply_dcs_action(request);
    }

    /// Resizes the primary buffer by unified logical reflow across its
    /// visible content and retained history together (see ADR 0017 and
    /// [`BufferState::reflowed`]): shrinking rewraps content into more
    /// physical rows and can push rows into history, growing can pull
    /// previously scrolled-off rows back onto the visible screen, and the
    /// cursor is relocated through a stable logical position rather than
    /// clamped raw coordinates. The alternate screen has no history and
    /// keeps the rectangular clip/pad model, relying on the application to
    /// redraw after the PTY resize.
    pub fn resize(&mut self, dimensions: Dimensions) -> Result<(), TerminalError> {
        self.resize_with_content_positions(dimensions, &[])
            .map(drop)
    }

    /// Resizes like [`Self::resize`] while remapping primary-content positions
    /// through the same stable logical-line anchors used for the cursor.
    pub fn resize_with_content_positions(
        &mut self,
        dimensions: Dimensions,
        positions: &[ContentPosition],
    ) -> Result<Vec<Option<ContentPosition>>, TerminalError> {
        let (primary, positions) =
            self.primary
                .reflowed(dimensions, &mut self.scrollback, positions)?;
        let alternate = match &self.alternate {
            Some(alternate) => Some(alternate.resized(dimensions)?),
            None => None,
        };

        self.primary = primary;
        self.alternate = alternate;
        self.tab_stops = resized_tab_stops(&self.tab_stops, dimensions);
        Ok(positions)
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
            self.clear_grapheme_anchor();
        }
        match operation {
            TerminalOp::Print(character) => self.print(character),
            TerminalOp::CarriageReturn => {
                let column = self.carriage_return_column();
                self.active_buffer_mut().cursor.column = column;
                self.clear_pending_wrap();
            }
            TerminalOp::LineFeed => {
                self.current_hyperlink = None;
                self.current_hyperlink_cells_remaining = 0;
                self.index();
                if self.modes.line_feed_new_line {
                    let column = self.carriage_return_column();
                    self.active_buffer_mut().cursor.column = column;
                }
                self.clear_pending_wrap();
            }
            TerminalOp::Index => {
                self.current_hyperlink = None;
                self.current_hyperlink_cells_remaining = 0;
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
            TerminalOp::CursorForwardTab(parameters) => {
                for _ in 0..Self::parameter_or(parameters, 0, 1) {
                    self.tab();
                }
                self.clear_pending_wrap();
            }
            TerminalOp::CursorBackwardTab(parameters) => {
                for _ in 0..Self::parameter_or(parameters, 0, 1) {
                    self.backward_tab();
                }
                self.clear_pending_wrap();
            }
            TerminalOp::NextLine => {
                self.current_hyperlink = None;
                self.current_hyperlink_cells_remaining = 0;
                // NEL indexes first and returns second, and with left/right
                // margins the order is observable: a cursor outside the
                // margins must not scroll, and returning first would carry it
                // inside them and let it scroll after all.
                self.index();
                let column = self.carriage_return_column();
                self.active_buffer_mut().cursor.column = column;
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
                let column = self.carriage_return_column();
                self.active_buffer_mut().cursor.column = column;
            }
            TerminalOp::CursorPreviousLine(parameters) => {
                self.move_vertical(parameters, false);
                let column = self.carriage_return_column();
                self.active_buffer_mut().cursor.column = column;
            }
            TerminalOp::CursorHorizontalAbsolute(parameters) => {
                self.cursor_horizontal_absolute(parameters, true)
            }
            TerminalOp::HorizontalPositionAbsolute(parameters) => {
                self.cursor_horizontal_absolute(parameters, false)
            }
            TerminalOp::CursorPosition(parameters) => self.cursor_position(parameters),
            TerminalOp::VerticalPositionAbsolute(parameters) => {
                self.vertical_position_absolute(parameters)
            }
            TerminalOp::EraseDisplay(parameters) => self.erase_display(parameters, false),
            TerminalOp::EraseLine(parameters) => self.erase_line(parameters, false),
            TerminalOp::EraseCharacters(parameters) => self.erase_characters(parameters),
            TerminalOp::InsertCharacters(parameters) => self.insert_characters(parameters),
            TerminalOp::DeleteCharacters(parameters) => self.delete_characters(parameters),
            TerminalOp::InsertLines(parameters) => self.insert_lines(parameters),
            TerminalOp::DeleteLines(parameters) => self.delete_lines(parameters),
            TerminalOp::ScrollUp(parameters) => self.scroll_up(parameters),
            TerminalOp::ScrollDown(parameters) => self.scroll_down(parameters),
            TerminalOp::SetScrollRegion(parameters) => self.set_scroll_region(parameters),
            TerminalOp::SaveAnsiOrSetHorizontalMargins(parameters) => {
                if self.modes.left_right_margin_mode {
                    self.set_horizontal_margins(parameters);
                } else if parameters.is_empty() {
                    self.save_ansi();
                }
            }
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
            TerminalOp::SetProtection { iso, protected } => {
                self.protection = if iso {
                    ProtectionSource::Iso
                } else {
                    ProtectionSource::Dec
                };
                self.current_protected = protected;
            }
            TerminalOp::SelectiveEraseDisplay(parameters) => self.erase_display(parameters, true),
            TerminalOp::SelectiveEraseLine(parameters) => self.erase_line(parameters, true),
            TerminalOp::RequestMode {
                private,
                parameters,
            } => self.request_mode(private, parameters),
            TerminalOp::SoftReset => self.soft_reset(),
            TerminalOp::RequestRectangleChecksum(parameters) => {
                self.request_rectangle_checksum(parameters);
            }
            TerminalOp::WindowOperation(parameters) => self.window_operation(parameters),
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
            Some(OscAction::SetHyperlink(hyperlink)) => {
                self.current_hyperlink_cells_remaining =
                    if hyperlink.is_some() { 4_096 } else { 0 };
                self.current_hyperlink = hyperlink;
            }
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
            text: CompactString::const_new(" "),
            width: CellWidth::Single,
            foreground: self.current_foreground,
            background: self.current_background,
            attributes: self.current_attributes,
            hyperlink: None,
        }
    }

    fn print(&mut self, character: char) {
        if self.try_extend_grapheme(character) {
            return;
        }
        let width = UnicodeWidthChar::width(character).unwrap_or(0);
        if width == 0 {
            self.clear_grapheme_anchor();
            return;
        }
        if self.active_buffer().pending_wrap && self.modes.auto_wrap {
            let (_, home) = self.wrap_geometry();
            let buffer = self.active_buffer_mut();
            buffer.screen.mark_soft_wrapped(buffer.cursor.row);
            buffer.cursor.column = home;
            buffer.pending_wrap = false;
            self.index();
        }

        let (wrap_limit, wrap_home) = self.wrap_geometry();
        let auto_wrap = self.modes.auto_wrap;
        let width = width.min(2);
        if width == 2 && self.cursor().column() + 1 == wrap_limit {
            if auto_wrap {
                let buffer = self.active_buffer_mut();
                buffer.screen.mark_soft_wrapped(buffer.cursor.row);
                buffer.cursor.column = wrap_home;
                buffer.pending_wrap = false;
                self.index();
            } else {
                self.print(char::REPLACEMENT_CHARACTER);
                return;
            }
        }

        if self.modes.insert_mode {
            // IRM shifts the rest of the line right to make room, and the
            // room it makes ends at the same place a line wraps: the right
            // margin when there is one, the screen's edge otherwise. Cells
            // pushed past that end are lost rather than carried to the next
            // line - insert mode does not wrap.
            let span = ColumnSpan::new(0, wrap_limit - 1);
            let cell = self.erase_cell();
            let cursor = self.cursor();
            self.active_buffer_mut().screen.insert_characters(
                cursor.column,
                cursor.row,
                width,
                cell,
                span,
            );
        }

        let cursor = self.cursor();
        let foreground = self.current_foreground;
        let background = self.current_background;
        let attributes = if self.current_protected {
            self.current_attributes.with(Attributes::PROTECTED)
        } else {
            self.current_attributes
        };
        let hyperlink = self.current_hyperlink.clone();
        if self.current_hyperlink_cells_remaining > 0 {
            self.current_hyperlink_cells_remaining -= 1;
            if self.current_hyperlink_cells_remaining == 0 {
                self.current_hyperlink = None;
            }
        }
        let buffer = self.active_buffer_mut();
        buffer.screen.replace_cluster(
            cursor.column,
            cursor.row,
            Cell {
                text: {
                    let mut text = CompactString::const_new("");
                    text.push(character);
                    text
                },
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
        buffer.grapheme_anchor = Some(cursor);
        Self::place_cursor_after_cluster(buffer, cursor, width, wrap_limit, auto_wrap);
    }

    /// Where a line wraps, and where it wraps *to*.
    ///
    /// With left/right margins in force a line wraps at the right margin and
    /// continues at the left one, so the visible effect is a column of text
    /// rather than the whole screen. A cursor sitting outside the margins is
    /// not in that column and wraps at the screen's own edges instead.
    fn wrap_geometry(&self) -> (usize, usize) {
        if self.cursor_within_margins() {
            let margins = self.horizontal_margins();
            (margins.right + 1, margins.left)
        } else {
            (self.dimensions().columns(), 0)
        }
    }

    fn try_extend_grapheme(&mut self, character: char) -> bool {
        let Some(anchor) = self.active_buffer().grapheme_anchor else {
            return false;
        };
        let Some(mut cell) = self.active_buffer().screen.cell(anchor.column, anchor.row) else {
            return false;
        };
        if cell.is_continuation() || !extends_grapheme(cell.text(), character) {
            return false;
        }
        if cell.text().len().saturating_add(character.len_utf8()) > MAX_GRAPHEME_BYTES {
            cell.text.clear();
            cell.text.push(char::REPLACEMENT_CHARACTER);
            cell.width = CellWidth::Single;
            let (wrap_limit, _) = self.wrap_geometry();
            let auto_wrap = self.modes.auto_wrap;
            let buffer = self.active_buffer_mut();
            buffer
                .screen
                .replace_cluster(anchor.column, anchor.row, cell);
            buffer.grapheme_anchor = None;
            buffer.pending_wrap = false;
            Self::place_cursor_after_cluster(buffer, anchor, 1, wrap_limit, auto_wrap);
            return true;
        }
        cell.text.push(character);
        let old_width = cell.width.columns();
        let new_width = grapheme_width(cell.text());
        if new_width == 0 {
            return true;
        }
        cell.width = if new_width == 2 {
            CellWidth::Double
        } else {
            CellWidth::Single
        };

        let columns = self.dimensions().columns();
        let (wrap_limit, wrap_home) = self.wrap_geometry();
        let auto_wrap = self.modes.auto_wrap;
        if old_width == 1 && new_width == 2 && anchor.column + 1 == wrap_limit {
            if !auto_wrap {
                cell.text.clear();
                cell.text.push(char::REPLACEMENT_CHARACTER);
                cell.width = CellWidth::Single;
                let buffer = self.active_buffer_mut();
                buffer
                    .screen
                    .replace_cluster(anchor.column, anchor.row, cell);
                buffer.grapheme_anchor = Some(anchor);
                buffer.pending_wrap = false;
                return true;
            }

            let fill = self.erase_cell();
            let linear = anchor.row * columns + anchor.column;
            {
                let buffer = self.active_buffer_mut();
                buffer.screen.fill_linear(linear, linear + 1, fill);
                buffer.screen.mark_soft_wrapped(anchor.row);
                buffer.cursor.column = wrap_home;
                buffer.pending_wrap = false;
            }
            self.index();
            let cursor = self.cursor();
            let buffer = self.active_buffer_mut();
            buffer
                .screen
                .replace_cluster(cursor.column, cursor.row, cell);
            buffer.grapheme_anchor = Some(cursor);
            Self::place_cursor_after_cluster(buffer, cursor, new_width, wrap_limit, auto_wrap);
            return true;
        }

        let buffer = self.active_buffer_mut();
        if old_width == new_width {
            buffer.screen.replace_cell(anchor.column, anchor.row, cell);
        } else {
            buffer
                .screen
                .replace_cluster(anchor.column, anchor.row, cell);
        }
        buffer.grapheme_anchor = Some(anchor);
        Self::place_cursor_after_cluster(buffer, anchor, new_width, wrap_limit, auto_wrap);
        true
    }

    /// `wrap_limit` is one past the last column a cluster may occupy - the
    /// right margin's column plus one, or the screen width when no margin
    /// applies. Reaching it arms the pending-wrap flag rather than moving the
    /// cursor, so the wrap only happens if something is actually printed.
    fn place_cursor_after_cluster(
        buffer: &mut BufferState,
        anchor: Cursor,
        width: usize,
        wrap_limit: usize,
        auto_wrap: bool,
    ) {
        buffer.cursor.row = anchor.row;
        if anchor.column + width >= wrap_limit {
            buffer.cursor.column = anchor.column;
            buffer.pending_wrap = auto_wrap;
        } else {
            buffer.cursor.column = anchor.column + width;
            buffer.pending_wrap = false;
        }
    }

    fn clear_grapheme_anchor(&mut self) {
        self.active_buffer_mut().grapheme_anchor = None;
    }

    fn index(&mut self) {
        let fill = self.erase_cell();
        let dimensions = self.dimensions();
        let span = self.horizontal_margins();
        // A cursor outside the left/right margins is not in the window that
        // scrolls, so at the bottom margin it neither scrolls nor moves.
        let within_margins = self.cursor_within_margins();
        let retain_history = self.active_screen == ActiveScreen::Primary
            && self.primary.scroll_top == 0
            && self.primary.scroll_bottom + 1 == dimensions.rows()
            && span.left == 0
            && span.right + 1 == dimensions.columns();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_bottom {
            if !within_margins {
                return;
            }
            let removed =
                buffer
                    .screen
                    .scroll_up(buffer.scroll_top, buffer.scroll_bottom, 1, fill, span);
            if retain_history {
                self.scrollback.push_rows(removed);
            }
        } else if buffer.cursor.row + 1 < dimensions.rows() {
            buffer.cursor.row += 1;
        }
    }

    fn reverse_index(&mut self) {
        let fill = self.erase_cell();
        let span = self.horizontal_margins();
        let within_margins = self.cursor_within_margins();
        let buffer = self.active_buffer_mut();
        if buffer.cursor.row == buffer.scroll_top {
            if !within_margins {
                return;
            }
            buffer
                .screen
                .scroll_down(buffer.scroll_top, buffer.scroll_bottom, 1, fill, span);
        } else {
            buffer.cursor.row = buffer.cursor.row.saturating_sub(1);
        }
    }

    fn tab(&mut self) {
        let columns = self.dimensions().columns();
        let cursor_column = self.cursor().column;
        // A tab stops at the right margin rather than running past it. That
        // holds for a cursor left of the left margin too - it tabs *into* the
        // margins and is caught by the far one - so the only cursor the
        // margin does not bind is one already past it.
        let right_margin = self.horizontal_margins().right;
        let limit = if cursor_column > right_margin {
            columns - 1
        } else {
            right_margin
        };
        let next_tab_stop = self
            .tab_stops
            .iter()
            .enumerate()
            .skip(cursor_column.saturating_add(1))
            .find_map(|(column, set)| set.then_some(column))
            .unwrap_or(limit)
            .min(limit.max(cursor_column));
        self.active_buffer_mut().cursor.column = next_tab_stop;
    }

    /// CBT (`CSI Z`). Unlike a forward tab this is bounded by the screen's
    /// own edge rather than by the left margin: esctest2 tabs backwards out
    /// of a left/right region and expects to land on column one.
    fn backward_tab(&mut self) {
        let cursor_column = self.cursor().column;
        let previous_tab_stop = self
            .tab_stops
            .iter()
            .enumerate()
            .take(cursor_column)
            .filter_map(|(column, set)| set.then_some(column))
            .next_back()
            .unwrap_or(0);
        self.active_buffer_mut().cursor.column = previous_tab_stop;
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

    /// The left and right margins in force.
    ///
    /// DECSLRM's margins only apply while DECLRMM (`DECSET 69`) is set, so
    /// this is the single place that decision is made; everything else asks
    /// here rather than reading `scroll_left`/`scroll_right` directly.
    fn horizontal_margins(&self) -> ColumnSpan {
        if self.modes.left_right_margin_mode {
            let buffer = self.active_buffer();
            ColumnSpan {
                left: buffer.scroll_left,
                right: buffer.scroll_right,
            }
        } else {
            ColumnSpan::full(self.dimensions().columns())
        }
    }

    /// Whether the cursor is between the left and right margins.
    ///
    /// Operations that shift cells sideways (`ICH`, `DCH`) and the ones that
    /// scroll (`IND`, `RI`, `LF`, `NEL`) do nothing at all when the cursor is
    /// outside the margins: the cursor is not in the window those operations
    /// act on, so there is nothing for them to act on.
    fn cursor_within_margins(&self) -> bool {
        let margins = self.horizontal_margins();
        let column = self.active_buffer().cursor.column;
        (margins.left..=margins.right).contains(&column)
    }

    /// The column a carriage return, `NEL`, `CNL` or `CPL` returns to.
    ///
    /// The left margin, unless the cursor is already left of it, in which
    /// case the screen's own left edge - a cursor outside the margins was
    /// never in that window, so pulling it *into* one would move it somewhere
    /// it had not been. Origin mode is the exception: there the left margin
    /// is where column 1 is, so that is where the cursor goes regardless.
    fn carriage_return_column(&self) -> usize {
        let margins = self.horizontal_margins();
        if self.modes.origin_mode || self.active_buffer().cursor.column >= margins.left {
            margins.left
        } else {
            0
        }
    }

    /// The columns `CUF`/`CUB` may move between.
    ///
    /// The same rule as `relative_vertical_bounds`, one axis over: a margin
    /// binds relative motion only for a cursor that starts inside it.
    fn relative_horizontal_bounds(&self) -> (usize, usize) {
        let margins = self.horizontal_margins();
        let column = self.active_buffer().cursor.column;
        let left = if column >= margins.left {
            margins.left
        } else {
            0
        };
        let right = if column <= margins.right {
            margins.right
        } else {
            self.dimensions().columns() - 1
        };
        (left, right)
    }

    fn vertical_bounds(&self) -> (usize, usize) {
        if self.modes.origin_mode {
            let buffer = self.active_buffer();
            (buffer.scroll_top, buffer.scroll_bottom)
        } else {
            (0, self.dimensions().rows() - 1)
        }
    }

    /// The rows `CUU`/`CUD` may move between.
    ///
    /// The scroll region binds relative vertical motion whenever the cursor
    /// *starts inside* it, whether or not origin mode is set: a program that
    /// reserved rows 2..4 and then moves down from row 3 is moving within the
    /// pane it reserved, and letting it fall out of the region puts its next
    /// write in someone else's pane. A cursor that starts outside the region
    /// is bound by the screen instead - it was never in the pane, so the
    /// margin is not its boundary.
    ///
    /// This is deliberately not `vertical_bounds`, which answers a different
    /// question: absolute addressing (`CUP`, `VPA`) is measured from the
    /// region only in origin mode, because origin mode is exactly what
    /// redefines where row 1 is.
    fn relative_vertical_bounds(&self) -> (usize, usize) {
        let buffer = self.active_buffer();
        let row = buffer.cursor.row;
        let top = if row >= buffer.scroll_top {
            buffer.scroll_top
        } else {
            0
        };
        let bottom = if row <= buffer.scroll_bottom {
            buffer.scroll_bottom
        } else {
            self.dimensions().rows() - 1
        };
        (top, bottom)
    }

    fn move_vertical(&mut self, parameters: CsiParameters, down: bool) {
        let count = Self::parameter_or(parameters, 0, 1);
        let (top, bottom) = self.relative_vertical_bounds();
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
        let (left, right) = self.relative_horizontal_bounds();
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = if forward {
            buffer.cursor.column.saturating_add(count).min(right)
        } else {
            buffer.cursor.column.saturating_sub(count).max(left)
        };
        buffer.pending_wrap = false;
    }

    /// CHA (`CSI G`) and HPA (`CSI \``) address the same column, but only CHA
    /// measures it from the left margin in origin mode. HPA is defined
    /// against the screen, which is why the two cannot share a code path.
    fn cursor_horizontal_absolute(&mut self, parameters: CsiParameters, origin_relative: bool) {
        let requested = Self::parameter_or(parameters, 0, 1) - 1;
        let column = if origin_relative {
            self.absolute_column(requested)
        } else {
            requested.min(self.dimensions().columns() - 1)
        };
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = column;
        buffer.pending_wrap = false;
    }

    /// Resolves a zero-based absolute column request.
    ///
    /// In origin mode column 1 is the left margin, exactly as row 1 is the
    /// top one, and the result cannot escape past the right margin.
    fn absolute_column(&self, requested: usize) -> usize {
        let columns = self.dimensions().columns();
        if self.modes.origin_mode {
            let margins = self.horizontal_margins();
            margins.left.saturating_add(requested).min(margins.right)
        } else {
            requested.min(columns - 1)
        }
    }

    fn cursor_position(&mut self, parameters: CsiParameters) {
        let requested_row = Self::parameter_or(parameters, 0, 1) - 1;
        let requested_column = Self::parameter_or(parameters, 1, 1) - 1;
        let (top, bottom) = self.vertical_bounds();
        let row = if self.modes.origin_mode {
            top.saturating_add(requested_row).min(bottom)
        } else {
            requested_row.min(bottom)
        };
        let column = self.absolute_column(requested_column);
        let buffer = self.active_buffer_mut();
        buffer.cursor.row = row;
        buffer.cursor.column = column;
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

    fn erase_display(&mut self, parameters: CsiParameters, selective: bool) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        if mode == 3 {
            if self.active_screen == ActiveScreen::Primary {
                self.scrollback.clear();
            }
            return;
        }
        let spare = self.spares_protected_cells(selective);
        let columns = self.dimensions().columns();
        let rows = self.dimensions().rows();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let screen = &mut self.active_buffer_mut().screen;
        let start = cursor.row * columns + cursor.column;
        match (mode, spare) {
            (0, false) => screen.fill_linear(start, columns * rows, cell),
            (0, true) => screen.fill_linear_sparing_protected(start, columns * rows, cell),
            (1, false) => screen.fill_linear(0, start + 1, cell),
            (1, true) => screen.fill_linear_sparing_protected(0, start + 1, cell),
            (2, false) => screen.clear_all(cell),
            // The whole-screen clear collapses the ring and resets every
            // row's extent, which it cannot do while some cells are staying
            // put, so a sparing erase takes the general path instead.
            (2, true) => screen.fill_linear_sparing_protected(0, columns * rows, cell),
            _ => {}
        }
    }

    fn erase_line(&mut self, parameters: CsiParameters, selective: bool) {
        let mode = Self::raw_parameter(parameters, 0, 0);
        let spare = self.spares_protected_cells(selective);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let row = cursor.row * columns;
        let (from, to) = match mode {
            0 => (row + cursor.column, row + columns),
            1 => (row, row + cursor.column + 1),
            2 => (row, row + columns),
            _ => return,
        };
        let screen = &mut self.active_buffer_mut().screen;
        if spare {
            screen.fill_linear_sparing_protected(from, to, cell);
        } else {
            screen.fill_linear(from, to, cell);
        }
    }

    /// Whether an erase leaves protected cells alone.
    ///
    /// A selective erase (`DECSED`, `DECSEL`) always does. An ordinary
    /// `ED`, `EL` or `ECH` does not - except while protection came from
    /// `SPA`/`EPA` rather than from `DECSCA`. That asymmetry is not ours:
    /// the DEC sequences define protection as something only the selective
    /// erases honour, while ISO 6429's guarded area is meant to be proof
    /// against erasure generally, and a terminal that implements both has to
    /// remember which of the two it was last told about.
    fn spares_protected_cells(&self, selective: bool) -> bool {
        selective || self.protection == ProtectionSource::Iso
    }

    fn erase_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let columns = self.dimensions().columns();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        let start = cursor.row * columns + cursor.column;
        let end = start.saturating_add(count).min((cursor.row + 1) * columns);
        let spare = self.spares_protected_cells(false);
        let screen = &mut self.active_buffer_mut().screen;
        if spare {
            screen.fill_linear_sparing_protected(start, end, cell);
        } else {
            screen.fill_linear(start, end, cell);
        }
    }

    fn insert_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        if !self.cursor_within_margins() {
            return;
        }
        let span = self.horizontal_margins();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut().screen.insert_characters(
            cursor.column,
            cursor.row,
            count,
            cell,
            span,
        );
    }

    fn delete_characters(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        if !self.cursor_within_margins() {
            return;
        }
        let span = self.horizontal_margins();
        let cell = self.erase_cell();
        let cursor = self.cursor();
        self.active_buffer_mut().screen.delete_characters(
            cursor.column,
            cursor.row,
            count,
            cell,
            span,
        );
    }

    fn insert_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        if !self.cursor_within_margins() {
            return;
        }
        let span = self.horizontal_margins();
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .insert_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell, span);
        }
    }

    fn delete_lines(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        if !self.cursor_within_margins() {
            return;
        }
        let span = self.horizontal_margins();
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        if (buffer.scroll_top..=buffer.scroll_bottom).contains(&buffer.cursor.row) {
            buffer
                .screen
                .delete_lines(buffer.cursor.row, buffer.scroll_bottom, count, cell, span);
        }
    }

    fn scroll_up(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let cell = self.erase_cell();
        let dimensions = self.dimensions();
        let span = self.horizontal_margins();
        let retain_history = self.active_screen == ActiveScreen::Primary
            && self.primary.scroll_top == 0
            && self.primary.scroll_bottom + 1 == dimensions.rows()
            && span.left == 0
            && span.right + 1 == dimensions.columns();
        let buffer = self.active_buffer_mut();
        let removed =
            buffer
                .screen
                .scroll_up(buffer.scroll_top, buffer.scroll_bottom, count, cell, span);
        if retain_history {
            self.scrollback.push_rows(removed);
        }
    }

    fn scroll_down(&mut self, parameters: CsiParameters) {
        let count = Self::parameter_or(parameters, 0, 1);
        let span = self.horizontal_margins();
        let cell = self.erase_cell();
        let buffer = self.active_buffer_mut();
        buffer
            .screen
            .scroll_down(buffer.scroll_top, buffer.scroll_bottom, count, cell, span);
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
        self.home_cursor();
    }

    /// `CSI Pl ; Pr s` (DECSLRM), which shares its final byte with SCOSC.
    ///
    /// The two are told apart by DECLRMM, not by the parameters: while the
    /// mode is set `CSI s` is always DECSLRM, and a bare `CSI s` therefore
    /// resets the margins to the full width rather than saving the cursor.
    /// That is xterm's behaviour and the suite asserts it - `SCOSC` inside
    /// left/right margin mode is expected *not* to save anything.
    fn set_horizontal_margins(&mut self, parameters: CsiParameters) {
        let columns = self.dimensions().columns();
        let left = Self::parameter_or(parameters, 0, 1) - 1;
        let right = Self::parameter_or(parameters, 1, columns) - 1;
        if left >= right || right >= columns {
            return;
        }
        let buffer = self.active_buffer_mut();
        buffer.scroll_left = left;
        buffer.scroll_right = right;
        self.home_cursor();
    }

    fn save_dec(&mut self) {
        let attributes = self.current_attributes;
        let foreground = self.current_foreground;
        let background = self.current_background;
        let origin_mode = self.modes.origin_mode;
        let buffer = self.active_buffer_mut();
        buffer.dec_saved = Some(SavedDecState {
            cursor: buffer.cursor,
            pending_wrap: buffer.pending_wrap,
            attributes,
            foreground,
            background,
            origin_mode,
        });
    }

    fn restore_dec(&mut self) {
        let Some(saved) = self.active_buffer().dec_saved else {
            // "Nothing saved" is the power-on state, not "do nothing": DEC
            // STD 070 and xterm both home the cursor and drop origin mode.
            // Returning early instead leaves whatever the cursor happened to
            // be doing, which makes a restore's effect depend on history the
            // caller cannot see.
            self.modes.origin_mode = false;
            self.current_attributes = Attributes::default();
            self.current_foreground = Color::Default;
            self.current_background = Color::Default;
            let buffer = self.active_buffer_mut();
            buffer.cursor = Cursor { column: 0, row: 0 };
            buffer.pending_wrap = false;
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
    }

    fn save_ansi(&mut self) {
        let cursor = self.cursor();
        self.active_buffer_mut().ansi_saved = Some(SavedAnsiCursor { cursor });
    }

    fn restore_ansi(&mut self) {
        // The SCO form saves a position and nothing else, so everything it
        // does not save comes back at its power-on value - which for origin
        // mode means off. Leaving it on would make the restored position
        // mean something different from the one that was saved.
        self.modes.origin_mode = false;
        let Some(saved) = self.active_buffer().ansi_saved else {
            // Same rule as DECRC: nothing saved means the power-on position.
            // The SCO form only ever saved a position, so that is all it
            // restores.
            let buffer = self.active_buffer_mut();
            buffer.cursor = Cursor { column: 0, row: 0 };
            buffer.pending_wrap = false;
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
            (Some(ParameterSeparator::Colon), 5)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon) =>
            {
                parameters
                    .value(index + 2)
                    .and_then(|value| u8::try_from(value).ok())
                    .map(Color::Indexed)
            }
            // Accept both the canonical `38:2::red:green:blue` form and the
            // widespread compact `38:2:red:green:blue` form.
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
            (Some(ParameterSeparator::Colon), 2)
                if parameters.separator(index + 2) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 3) == Some(ParameterSeparator::Colon)
                    && parameters.separator(index + 4) == Some(ParameterSeparator::Colon) =>
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
            (Some(ParameterSeparator::Colon), 2)
                if parameters.separator(index + 5) == Some(ParameterSeparator::Colon) =>
            {
                index + 5
            }
            (Some(ParameterSeparator::Colon), 2) => index + 4,
            _ => index,
        }
    }

    fn reset_graphics_rendition(&mut self) {
        self.current_attributes = Attributes::NONE;
        self.current_foreground = Color::Default;
        self.current_background = Color::Default;
        self.current_hyperlink = None;
        self.current_hyperlink_cells_remaining = 0;
    }

    fn set_modes(&mut self, private: bool, enabled: bool, parameters: CsiParameters) {
        if !private {
            for index in 0..parameters.len() {
                match parameters.value(index) {
                    Some(2) => self.modes.keyboard_locked = enabled,
                    Some(4) => self.modes.insert_mode = enabled,
                    Some(20) => self.modes.line_feed_new_line = enabled,
                    _ => {}
                }
            }
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
                66 => self.modes.application_keypad = enabled,
                67 => self.modes.backarrow_sends_backspace = enabled,
                69 => {
                    self.modes.left_right_margin_mode = enabled;
                    if !enabled {
                        // Turning the mode off does not merely stop honouring
                        // the margins, it discards them: xterm's DECRESET 69
                        // resets them to the full width, so turning the mode
                        // back on later starts from a clean screen rather
                        // than silently reviving a stale pair.
                        let columns = self.dimensions().columns();
                        let buffer = self.active_buffer_mut();
                        buffer.scroll_left = 0;
                        buffer.scroll_right = columns - 1;
                    }
                }
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
        self.current_hyperlink = None;
        self.current_hyperlink_cells_remaining = 0;
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
            alternate.grapheme_anchor = None;
        }
        self.active_screen = ActiveScreen::Alternate;
        self.modes.alternate_screen = true;
        self.active_buffer_mut().screen.mark_all_dirty();
    }

    fn leave_alternate(&mut self, reset: bool) {
        self.current_hyperlink = None;
        self.current_hyperlink_cells_remaining = 0;
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
        let left = self.horizontal_margins().left;
        let buffer = self.active_buffer_mut();
        buffer.cursor.column = if origin_mode { left } else { 0 };
        buffer.cursor.row = if origin_mode { buffer.scroll_top } else { 0 };
        buffer.pending_wrap = false;
    }

    /// Answers `CSI Pid ; Pp ; Pt ; Pl ; Pb ; Pr * y` (DECRQCRA) with
    /// `DCS Pid ! ~ <hex> ST` (DECCKSR).
    ///
    /// This is the only way a program can read the screen back out of the
    /// terminal, and it is what every screen assertion in the esctest2
    /// conformance suite is built on. It is also, unavoidably, a
    /// screen-reading primitive, so what it reports is deliberately narrow:
    /// the sum of the character codes in the rectangle, and nothing else. No
    /// attributes, no colours, no hyperlink targets.
    ///
    /// Three choices worth stating, because each is a real fork in the road:
    ///
    /// - **Characters only, no attribute contribution.** xterm has variants
    ///   that fold bold, underline and the protected bit into the sum. A
    ///   caller cannot tell those apart from a different character, so the
    ///   extra bits make the answer ambiguous rather than richer, and every
    ///   conformance expectation is written against the bare character code.
    /// - **An unwritten cell counts as a space**, matching xterm from patch
    ///   334 onwards. The older behaviour distinguished "empty" from "holds a
    ///   space", which is a distinction the rest of our model does not make.
    /// - **Not negated.** Old xterm returned the two's complement; current
    ///   xterm returns the sum. A negated checksum is the older convention
    ///   and there is no reason to carry it forward.
    ///
    /// A checksum cannot distinguish `"ab"` from `"ba"`, which is why
    /// esctest2 asks one cell at a time. That is the caller's problem to know
    /// about, not something to try to fix here.
    fn request_rectangle_checksum(&mut self, parameters: CsiParameters) {
        let identifier = parameters.value(0).unwrap_or(0);
        let dimensions = self.dimensions();
        let rows = dimensions.rows();
        let columns = dimensions.columns();

        // Parameters 1 is the page, which we have exactly one of. The
        // rectangle is one-based and inclusive, and an omitted or zero edge
        // means the edge of the screen.
        let edge = |index: usize, default: usize| -> usize {
            match parameters.value(index) {
                Some(0) | None => default,
                Some(value) => usize::from(value),
            }
        };
        // In origin mode the rectangle is measured from the scroll region's
        // top-left corner and cannot reach outside it, exactly as CUP is.
        // A program that has set a region and asked to work relative to it
        // means the same thing when it reads the screen back.
        let (row_origin, row_limit, column_origin, column_limit) = if self.modes.origin_mode {
            let margins = self.horizontal_margins();
            let buffer = self.active_buffer();
            (
                buffer.scroll_top,
                buffer.scroll_bottom + 1,
                margins.left,
                margins.right + 1,
            )
        } else {
            (0, rows, 0, columns)
        };
        let top = (row_origin + edge(2, 1) - 1).min(row_limit);
        let left = (column_origin + edge(3, 1) - 1).min(column_limit);
        let bottom = (row_origin + edge(4, row_limit - row_origin)).min(row_limit);
        let right = (column_origin + edge(5, column_limit - column_origin)).min(column_limit);

        let mut checksum: u16 = 0;
        if top < bottom && left < right {
            for row in top..bottom {
                for column in left..right {
                    let Some(cell) = self.cell_ref(column, row) else {
                        continue;
                    };
                    if cell.text().is_empty() {
                        checksum = checksum.wrapping_add(u16::from(b' '));
                        continue;
                    }
                    for character in cell.text().chars() {
                        checksum = checksum.wrapping_add(character as u16);
                    }
                }
            }
        }

        let reply = format!("\x1bP{identifier}!~{checksum:04X}\x1b\\");
        self.queue_reply(reply.as_bytes());
    }

    /// `CSI ! p` (DECSTR), a soft reset.
    ///
    /// The distinction from RIS is what survives: a soft reset puts the
    /// *modes* back to their power-on values but leaves the screen's
    /// contents, the scrollback, the tab stops and the title alone. It is
    /// what a program sends to get a predictable terminal without throwing
    /// away what the user is looking at.
    ///
    /// Two details worth stating because they are easy to get wrong:
    ///
    /// - The cursor does not move. Only the *saved* cursor is reset, to the
    ///   home position - which here is spelled as "nothing saved", since
    ///   `restore_dec` already treats that as the power-on state.
    /// - Autowrap comes back *on*. DEC STD 070 says off, but xterm restores
    ///   it to the resource default and notes that it does so to avoid
    ///   breaking applications that rely on it; ours defaults on, so that is
    ///   where a soft reset leaves it.
    ///
    /// Character sets are deliberately not touched here. Resetting those
    /// belongs to RIS, which is tracked separately.
    fn soft_reset(&mut self) {
        self.modes.origin_mode = false;
        self.modes.auto_wrap = true;
        self.modes.cursor_visible = true;
        self.modes.application_cursor = false;
        self.modes.application_keypad = false;
        // DEC STD 070 has DECSTR reset left/right margin mode, and the
        // margins with it - resetting one without the other would leave a
        // pair of margins that reappear the moment an application enables
        // the mode for its own purposes.
        self.modes.left_right_margin_mode = false;
        self.modes.insert_mode = false;
        // DECSTR lists DECSCA among the things it returns to normal, and the
        // source has to go with it: leaving it at Iso would have the next
        // ordinary erase keep sparing cells nothing has protected.
        self.current_protected = false;
        self.protection = ProtectionSource::None;
        self.reset_graphics_rendition();

        let bottom = self.dimensions().rows() - 1;
        let right = self.dimensions().columns() - 1;
        let buffer = self.active_buffer_mut();
        buffer.scroll_top = 0;
        buffer.scroll_bottom = bottom;
        buffer.scroll_left = 0;
        buffer.scroll_right = right;
        buffer.pending_wrap = false;
        buffer.dec_saved = None;
        buffer.ansi_saved = None;
    }

    /// Answers the two `CSI ... t` size *reports*.
    ///
    /// Everything else in the xterm window-operation set moves, resizes,
    /// raises or iconifies a window, which is the embedder's business and not
    /// something the grid can honour or usefully refuse. Those are ignored.
    /// The reports are different: a caller asking how large the screen is has
    /// a question we can answer exactly, and one that cannot be answered any
    /// other way, so leaving it unanswered strands the caller until its read
    /// times out.
    ///
    /// `18` reports the text area and `19` the display; with no window
    /// decoration to account for, both are the grid.
    fn window_operation(&mut self, parameters: CsiParameters) {
        let dimensions = self.screen().dimensions();
        let kind = match parameters.value(0) {
            Some(18) => 8,
            Some(19) => 9,
            _ => return,
        };
        let reply = format!(
            "\x1b[{kind};{};{}t",
            dimensions.rows(),
            dimensions.columns()
        );
        self.queue_reply(reply.as_bytes());
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
                // Origin mode redefines column 1 as the left margin just as
                // it redefines row 1, so a report that stayed absolute would
                // not round-trip through the CUP that produced it.
                let left = self.horizontal_margins().left;
                let column = if self.modes.origin_mode && cursor.column >= left {
                    cursor.column - left + 1
                } else {
                    // A cursor left of the left margin has no meaningful
                    // offset from an origin it is not inside, and a negative
                    // column is not something CPR can express, so it reports
                    // where the cursor actually is.
                    cursor.column + 1
                };
                let reply = format!("\x1b[{row};{column}R");
                self.queue_reply(reply.as_bytes());
            }
            _ => {}
        }
    }

    /// Answers DECRQM (`CSI Pm $ p`, or `CSI ? Pm $ p` for a DEC private
    /// mode) with DECRPM.
    ///
    /// The reply says whether a mode is *set*, not whether we perform its
    /// function, and the two are easy to confuse into a lie. A terminal that
    /// stores a bit for a mode it does not act on and then reports that bit
    /// back has told the caller it will do something it will not: an
    /// application that asks about `DECNRCM` and is told "set" will send
    /// text we render wrongly. The reply for a function we do not perform is
    /// "permanently reset" (4), which is exactly the answer that value
    /// exists for, and "not recognized" (0) is reserved for modes we cannot
    /// name at all.
    fn request_mode(&mut self, private: bool, parameters: CsiParameters) {
        let Some(mode) = parameters.value(0) else {
            return;
        };
        let state = if private {
            self.dec_mode_state(mode)
        } else {
            self.ansi_mode_state(mode)
        };
        let marker = if private { "?" } else { "" };
        let reply = format!("\x1b[{marker}{mode};{state}$y");
        self.queue_reply(reply.as_bytes());
    }

    fn ansi_mode_state(&self, mode: u16) -> u8 {
        match mode {
            2 => Self::mode_state(self.modes.keyboard_locked),
            4 => Self::mode_state(self.modes.insert_mode),
            20 => Self::mode_state(self.modes.line_feed_new_line),
            // SRM reset is local echo, which a terminal emulator with no
            // half-duplex line to echo onto can never do. We are permanently
            // in send-receive mode rather than able to leave it.
            12 => PERMANENTLY_SET,
            // The rest of the original ANSI set governs the behaviour of a
            // hardware terminal's keyboard, printer and transmission line -
            // guarded areas, area transfer, editing extents, positioning
            // units. There is no such hardware here to switch.
            1 | 5 | 7 | 10 | 11 | 13..=19 => PERMANENTLY_RESET,
            _ => NOT_RECOGNIZED,
        }
    }

    fn dec_mode_state(&self, mode: u16) -> u8 {
        match mode {
            1 => Self::mode_state(self.modes.application_cursor),
            6 => Self::mode_state(self.modes.origin_mode),
            7 => Self::mode_state(self.modes.auto_wrap),
            9 => Self::mode_state(self.modes.mouse_tracking == MouseTrackingMode::X10),
            25 => Self::mode_state(self.modes.cursor_visible),
            47 | 1047 | 1049 => Self::mode_state(self.modes.alternate_screen),
            66 => Self::mode_state(self.modes.application_keypad),
            67 => Self::mode_state(self.modes.backarrow_sends_backspace),
            69 => Self::mode_state(self.modes.left_right_margin_mode),
            1000 => Self::mode_state(self.modes.mouse_tracking == MouseTrackingMode::ButtonEvent),
            1002 => Self::mode_state(self.modes.mouse_tracking == MouseTrackingMode::ButtonMotion),
            1003 => Self::mode_state(self.modes.mouse_tracking == MouseTrackingMode::AnyMotion),
            1004 => Self::mode_state(self.modes.focus_reporting),
            1006 => Self::mode_state(self.modes.sgr_mouse),
            2004 => Self::mode_state(self.modes.bracketed_paste),
            // Named, and deliberately not performed. The column-width modes
            // (3, 95) would have the terminal resize the window, which is the
            // embedder's to decide; 4 and 8 are the timing of a scroll and of
            // key autorepeat, neither of which a grid controls; 18 and 19 are
            // a printer; and the remainder are the national, bidirectional
            // and keyboard-hardware features of real DEC terminals. 5 is
            // reverse video, which we could perform and do not yet.
            2..=5 | 8 | 18 | 19 | 34..=36 | 42 | 57 | 60 | 61 | 64 | 68 | 73 | 81 | 95..=106 => {
                PERMANENTLY_RESET
            }
            _ => NOT_RECOGNIZED,
        }
    }

    const fn mode_state(set: bool) -> u8 {
        if set {
            SET
        } else {
            RESET
        }
    }

    /// Answers `DCS $ q <selector> ST` (DECRQSS).
    ///
    /// Only SGR (`m`) is reportable. This is what a program uses to discover
    /// what the terminal actually accepted: `termstandard/colors` documents
    /// setting a truecolor and reading it back as *the* truecolor detection,
    /// so a terminal that silently drops a colour form it does not parse is
    /// indistinguishable from one that has no truecolor at all. Anything
    /// else is answered with the "not recognized" form rather than left
    /// unanswered, so a caller is never left waiting.
    fn apply_dcs_action(&mut self, action: Option<DcsAction>) {
        let Some(DcsAction::RequestStatusString(selector)) = action else {
            return;
        };
        if selector == b"m" {
            let report = self.graphics_rendition_report();
            self.queue_reply(format!("\x1bP1$r{report}m\x1b\\").as_bytes());
        } else {
            self.queue_reply(b"\x1bP0$r\x1b\\");
        }
    }

    /// The current pen as the SGR parameters that would reproduce it.
    ///
    /// Colours are reported in the colon-delimited ITU T.416 form xterm
    /// reports, which is also what tells the caller that this terminal
    /// accepts colons at all.
    fn graphics_rendition_report(&self) -> String {
        let mut parameters = vec!["0".to_owned()];
        for (attribute, code) in [
            (Attributes::BOLD, 1),
            (Attributes::FAINT, 2),
            (Attributes::ITALIC, 3),
            (Attributes::UNDERLINE, 4),
            (Attributes::DOUBLE_UNDERLINE, 21),
            (Attributes::SLOW_BLINK, 5),
            (Attributes::RAPID_BLINK, 6),
            (Attributes::INVERSE, 7),
            (Attributes::CONCEALED, 8),
            (Attributes::STRIKETHROUGH, 9),
        ] {
            if self.current_attributes.contains(attribute) {
                parameters.push(code.to_string());
            }
        }
        if let Some(color) = color_report(self.current_foreground, false) {
            parameters.push(color);
        }
        if let Some(color) = color_report(self.current_background, true) {
            parameters.push(color);
        }
        parameters.join(";")
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

/// One colour as the SGR parameter that would set it again, or `None` for
/// the default colour, which `0` has already reported.
fn color_report(color: Color, background: bool) -> Option<String> {
    let extended = if background { 48 } else { 38 };
    match color {
        Color::Default => None,
        Color::Indexed(index) if index < 8 => {
            let base = if background { 40 } else { 30 };
            Some((base + u16::from(index)).to_string())
        }
        Color::Indexed(index) if index < 16 => {
            let base = if background { 100 } else { 90 };
            Some((base + u16::from(index) - 8).to_string())
        }
        Color::Indexed(index) => Some(format!("{extended}:5:{index}")),
        Color::Rgb { red, green, blue } => Some(format!("{extended}:2::{red}:{green}:{blue}")),
    }
}

/// Which family of sequences last set or cleared character protection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtectionSource {
    None,
    /// `DECSCA`. Only `DECSED` and `DECSEL` honour it.
    Dec,
    /// `SPA`/`EPA`. Every erase honours it.
    Iso,
}

/// The DECRPM states. A mode is set or reset, or it is one the terminal can
/// name but whose state cannot change, or one it cannot name at all.
const NOT_RECOGNIZED: u8 = 0;
const SET: u8 = 1;
const RESET: u8 = 2;
const PERMANENTLY_SET: u8 = 3;
const PERMANENTLY_RESET: u8 = 4;

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
