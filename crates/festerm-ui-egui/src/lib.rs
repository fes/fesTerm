//! `egui` presentation for the GUI-independent terminal core.
//!
//! This crate owns points, fonts, glyph layout, selection presentation, and
//! native-window input translation. Terminal protocol state and input encoding
//! remain in `festerm-core`.

use egui::Color32;
use festerm_core::{Cell, ContentPosition, Cursor, CursorStyle, Terminal, TerminalModes};

mod cache;
pub mod chrome;
pub mod controls;
mod fonts;
mod geometry;
pub mod icon;
mod input;
pub mod overlay;
pub mod palette;
mod renderer;
pub mod routing_trace;
mod selection;
pub mod statusbar;
pub mod theme;
mod view;

pub(crate) const DEFAULT_FOREGROUND: Color32 = theme::TEXT_PRIMARY;
pub(crate) const DEFAULT_BACKGROUND: Color32 = theme::SURFACE_TERMINAL;
pub(crate) const SELECTION_BACKGROUND: Color32 = theme::SURFACE_SELECTION;
pub(crate) const GLYPH_CACHE_CAPACITY: usize = 4_096;

// --- Public re-exports ---
pub use cache::{
    RenderCacheUpdate, RenderedCell, ResizeOutcome, ResizeTracker, TerminalRenderCache,
};
pub use fonts::{
    install_terminal_font_family, install_terminal_fonts, terminal_font, terminal_fonts_installed,
    TerminalFontFamily, TerminalFontGeneration, TerminalFontSet, DEFAULT_TERMINAL_FONT_SIZE,
};
pub use geometry::{
    cell_from_point, dimensions_from_points, CellMetrics, CellPosition, CellRange, ViewSize,
};
pub use input::{
    route_input, route_mouse_input, EncodedInputSink, InputRoute, InputSinkDiagnostics,
    TERMINAL_RESIZE_DEBOUNCE,
};
pub use renderer::{resolve_color, terminal_color_scheme, FontSettings};
pub use selection::{normalize_selection_position, selection_text, Selection};
pub use view::{
    FrameDiagnostics, TerminalContextMenuAction, TerminalContextTarget, TerminalHistoryAction,
    TerminalView, TerminalViewOptions,
};

/// A read-only, renderer-facing view of the currently visible terminal grid.
///
/// It borrows core state and therefore cannot outlive or mutate the terminal.
/// The renderer copies only rows announced as dirty into its presentation
/// cache; it does not clone a complete core grid per GUI frame.
#[derive(Clone, Copy)]
pub struct TerminalSnapshot<'a> {
    terminal: &'a Terminal,
    viewport_offset_rows: usize,
    cursor: Cursor,
    cursor_style: CursorStyle,
    cursor_style_requested_by_program: bool,
    modes: TerminalModes,
}

impl<'a> TerminalSnapshot<'a> {
    pub fn from_terminal(terminal: &'a Terminal) -> Self {
        Self {
            terminal,
            viewport_offset_rows: 0,
            cursor: terminal.cursor(),
            cursor_style: terminal.cursor_style(),
            cursor_style_requested_by_program: terminal.cursor_style_requested_by_program(),
            modes: terminal.modes(),
        }
    }

    pub fn from_terminal_viewport(terminal: &'a Terminal, offset_rows: usize) -> Self {
        let mut snapshot = Self::from_terminal(terminal);
        snapshot.viewport_offset_rows = if terminal.modes().alternate_screen() {
            0
        } else {
            offset_rows.min(terminal.scrollback_stats().physical_rows())
        };
        snapshot
    }

    pub fn dimensions(self) -> festerm_core::Dimensions {
        self.terminal.screen().dimensions()
    }

    pub const fn cursor(self) -> Cursor {
        self.cursor
    }

    pub const fn cursor_style(self) -> CursorStyle {
        self.cursor_style
    }

    /// Whether the running program inside the terminal has ever explicitly
    /// requested a cursor style (DECSCUSR). When `false`, the terminal is
    /// still in its untouched initial state and the GUI is free to apply
    /// its own preferred default appearance instead of `cursor_style()`'s
    /// spec-mandated blinking-block value.
    pub const fn cursor_style_requested_by_program(self) -> bool {
        self.cursor_style_requested_by_program
    }

    pub const fn modes(self) -> TerminalModes {
        self.modes
    }

    /// Returns a borrowed core cell, preserving width-two/continuation roles.
    pub fn cell(self, column: usize, row: usize) -> Option<&'a Cell> {
        if column >= self.dimensions().columns() || row >= self.dimensions().rows() {
            return None;
        }
        if self.viewport_offset_rows == 0 || self.modes.alternate_screen() {
            return self.terminal.screen().cell_ref(column, row);
        }
        self.absolute_cell(column, self.content_row_for_viewport_row(row)?)
    }

    pub fn content_position(self, position: CellPosition) -> Option<ContentPosition> {
        (position.column < self.dimensions().columns() && position.row < self.dimensions().rows())
            .then(|| ContentPosition {
                column: position.column,
                absolute_row: self
                    .content_row_for_viewport_row(position.row)
                    .expect("validated viewport row has content coordinate"),
            })
    }

    pub(crate) fn content_row_for_viewport_row(self, row: usize) -> Option<u64> {
        (row < self.dimensions().rows()).then(|| {
            if self.modes.alternate_screen() {
                return row as u64;
            }
            let stats = self.terminal.scrollback_stats();
            if row < self.viewport_offset_rows {
                stats.content_row_origin().saturating_add(
                    (stats.physical_rows() - self.viewport_offset_rows + row) as u64,
                )
            } else {
                stats
                    .screen_row_origin()
                    .saturating_add((row - self.viewport_offset_rows) as u64)
            }
        })
    }

    pub fn contains_content_row(self, content_row: u64) -> bool {
        if self.modes.alternate_screen() {
            return content_row < self.dimensions().rows() as u64;
        }
        let stats = self.terminal.scrollback_stats();
        let history_end = stats
            .content_row_origin()
            .saturating_add(stats.physical_rows() as u64);
        (content_row >= stats.content_row_origin() && content_row < history_end)
            || (content_row >= stats.screen_row_origin()
                && content_row
                    < stats
                        .screen_row_origin()
                        .saturating_add(self.dimensions().rows() as u64))
    }

    pub fn next_content_row(self, content_row: u64) -> Option<u64> {
        if self.modes.alternate_screen() {
            return (content_row + 1 < self.dimensions().rows() as u64).then_some(content_row + 1);
        }
        let stats = self.terminal.scrollback_stats();
        let history_end = stats
            .content_row_origin()
            .saturating_add(stats.physical_rows() as u64);
        if content_row + 1 < history_end {
            Some(content_row + 1)
        } else if content_row < stats.screen_row_origin() {
            Some(stats.screen_row_origin())
        } else {
            (content_row + 1
                < stats
                    .screen_row_origin()
                    .saturating_add(self.dimensions().rows() as u64))
            .then_some(content_row + 1)
        }
    }

    pub fn absolute_cell(self, column: usize, content_row: u64) -> Option<&'a Cell> {
        if column >= self.dimensions().columns() {
            return None;
        }
        if self.modes.alternate_screen() {
            return self
                .terminal
                .screen()
                .cell_ref(column, usize::try_from(content_row).ok()?);
        }
        let stats = self.terminal.scrollback_stats();
        let history_rows = stats.physical_rows();
        if content_row < stats.screen_row_origin() {
            let relative_row =
                usize::try_from(content_row.checked_sub(stats.content_row_origin())?).ok()?;
            if relative_row >= history_rows {
                return None;
            }
            return self
                .terminal
                .scrollback_physical_row(relative_row)
                .and_then(|cells| cells.get(column));
        }
        self.terminal.screen().cell_ref(
            column,
            usize::try_from(content_row - stats.screen_row_origin()).ok()?,
        )
    }

    pub fn absolute_row_soft_wrapped(self, content_row: u64) -> Option<bool> {
        if self.modes.alternate_screen() {
            return self
                .terminal
                .screen()
                .row_soft_wrapped(usize::try_from(content_row).ok()?);
        }
        let stats = self.terminal.scrollback_stats();
        let history_rows = stats.physical_rows();
        if content_row < stats.screen_row_origin() {
            let relative_row =
                usize::try_from(content_row.checked_sub(stats.content_row_origin())?).ok()?;
            if relative_row >= history_rows {
                return None;
            }
            return self
                .terminal
                .scrollback_physical_row_soft_wrapped(relative_row);
        }
        self.terminal
            .screen()
            .row_soft_wrapped(usize::try_from(content_row - stats.screen_row_origin()).ok()?)
    }

    pub const fn viewport_offset_rows(self) -> usize {
        self.viewport_offset_rows
    }

    pub fn cursor_in_viewport(self) -> Option<(usize, usize)> {
        if self.viewport_offset_rows == 0 || self.modes.alternate_screen() {
            return Some((self.cursor.column(), self.cursor.row()));
        }
        let history_rows = self.terminal.scrollback_stats().physical_rows();
        let first = history_rows.saturating_sub(self.viewport_offset_rows);
        let content_row = history_rows + self.cursor.row();
        let row = content_row.checked_sub(first)?;
        (row < self.dimensions().rows()).then_some((self.cursor.column(), row))
    }
}

#[cfg(test)]
mod tests {
    use festerm_core::{Dimensions, Terminal};

    use super::*;

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    #[test]
    fn history_snapshot_projects_retained_rows_without_moving_the_live_cursor() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"one\r\ntwo\r\ntri\r\n");

        let one_row_back = TerminalSnapshot::from_terminal_viewport(&terminal, 1);
        assert_eq!(
            (0..4)
                .filter_map(|column| one_row_back.cell(column, 0))
                .map(Cell::character)
                .collect::<String>(),
            "two"
        );
        assert_eq!(one_row_back.cursor_in_viewport(), None);

        let oldest = TerminalSnapshot::from_terminal_viewport(&terminal, 2);
        assert_eq!(
            (0..4)
                .filter_map(|column| oldest.cell(column, 0))
                .map(Cell::character)
                .collect::<String>(),
            "one"
        );
        assert_eq!(oldest.cursor_in_viewport(), None);
    }
}
