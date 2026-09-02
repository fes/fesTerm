//! Terminal-content search (`docs/gui-design.md` "Terminal-content
//! search"). Deliberately kept in the `festerm` application crate rather
//! than `festerm-ui-egui`: match-scanning and navigation are pure text-model
//! logic over `festerm_core::Terminal`'s public accessors and stay testable
//! here without touching renderer internals.

use festerm_core::{Cell, Terminal};

/// Per-session find-bar state. Owned by `SessionTab` so it lives and dies
/// with the tab and is never logged or persisted (`docs/gui-design.md`:
/// "Queries are never logged or persisted").
#[derive(Clone, Debug, Default)]
pub struct TerminalSearchState {
    open: bool,
    query: String,
    /// Absolute "document row" indices of every current match, oldest
    /// content first: retained scrollback rows (unless the alternate
    /// screen is active) followed by the live visible screen's rows. This
    /// addressing matches `festerm_ui_egui::TerminalSnapshot`'s own
    /// scrollback-then-screen row space, so a match can be handed straight
    /// to `TerminalView::reveal_document_row`.
    matches: Vec<usize>,
    current: Option<usize>,
    /// Set on `open()` and consumed by the rendering code to request
    /// keyboard focus on the query field exactly once per open, mirroring
    /// the `cancel_focus_requested` pattern used by confirmation dialogs.
    focus_requested: bool,
    /// `scrollback_stats().physical_rows()` as of the last scan, used by
    /// `refresh_if_stale` to cheaply detect new retained output without
    /// re-scanning every row every frame while the bar sits open.
    last_scrollback_rows: usize,
}

impl TerminalSearchState {
    pub const fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// Opens the find bar. Re-opening an already-open bar just re-requests
    /// focus (e.g. the shortcut was pressed again after a click elsewhere).
    pub fn open(&mut self) {
        self.open = true;
        self.focus_requested = true;
    }

    /// Closes the find bar and discards all query/match state. Escape must
    /// clear the transient query/highlights, not merely hide the bar.
    pub fn close(&mut self) {
        *self = Self::default();
    }

    pub fn take_focus_request(&mut self) -> bool {
        std::mem::take(&mut self.focus_requested)
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// 1-based position of the current match for a "N of M" display.
    /// `None` before any match is selected (e.g. an empty query).
    pub fn current_position(&self) -> Option<usize> {
        self.current.map(|index| index + 1)
    }

    pub fn has_query(&self) -> bool {
        !self.query.is_empty()
    }

    /// Updates the query and rescans `terminal`.
    pub fn set_query(&mut self, terminal: &Terminal, query: String) {
        self.query = query;
        self.rescan(terminal);
    }

    /// Re-scans without changing the query text. Used when new terminal
    /// output may have introduced additional matches while the bar stays
    /// open: "New output may extend the result set without moving the
    /// current match unexpectedly."
    pub fn rescan(&mut self, terminal: &Terminal) {
        let current_row = self
            .current
            .and_then(|index| self.matches.get(index).copied());
        self.matches = scan_matches(terminal, &self.query);
        self.current = current_row
            .and_then(|row| self.matches.iter().position(|&candidate| candidate == row))
            .or(if self.matches.is_empty() {
                None
            } else {
                Some(0)
            });
        self.last_scrollback_rows = terminal.scrollback_stats().physical_rows();
    }

    /// Cheaply re-scans only if retained scrollback has grown since the
    /// last scan, so an open find bar doesn't re-walk every searchable row
    /// every single frame. Output that stays entirely within the live
    /// visible screen (no new scrollback rows) is not re-detected until the
    /// query next changes; this is a deliberate, disclosed scope limit.
    pub fn refresh_if_stale(&mut self, terminal: &Terminal) {
        if !self.query.is_empty()
            && terminal.scrollback_stats().physical_rows() != self.last_scrollback_rows
        {
            self.rescan(terminal);
        }
    }

    /// The document row the caller should scroll into view for the
    /// currently selected match, if any.
    pub fn current_match_row(&self) -> Option<usize> {
        self.current
            .and_then(|index| self.matches.get(index).copied())
    }

    /// Enter/Down: advances to the next match, wrapping past the last one.
    pub fn advance(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = Some(match self.current {
            Some(index) => (index + 1) % self.matches.len(),
            None => 0,
        });
    }

    /// Shift+Enter/Up: reverses to the previous match, wrapping past the
    /// first one.
    pub fn retreat(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(index) => index - 1,
        });
    }
}

/// Literal, case-insensitive scan across searchable terminal content. In
/// alternate-screen mode only the live alternate-screen rows are searched
/// ("it searches only the retained alternate-screen content and does not
/// pretend that the inaccessible primary buffer is simultaneously
/// visible"); otherwise retained primary scrollback (oldest first) is
/// searched, followed by the live primary screen's rows.
fn scan_matches(terminal: &Terminal, query: &str) -> Vec<usize> {
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    let mut matches = Vec::new();

    let history_rows = if terminal.modes().alternate_screen() {
        0
    } else {
        terminal.scrollback_stats().physical_rows()
    };

    for row in 0..history_rows {
        if let Some(cells) = terminal.scrollback_physical_row(row) {
            if row_text(cells).to_lowercase().contains(&needle) {
                matches.push(row);
            }
        }
    }

    for row in 0..terminal.screen().dimensions().rows() {
        if let Some(text) = terminal.row_text(row) {
            if text.to_lowercase().contains(&needle) {
                matches.push(history_rows + row);
            }
        }
    }

    matches
}

fn row_text(cells: &[Cell]) -> String {
    cells.iter().map(Cell::character).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::Dimensions;

    fn terminal_with_rows(rows: &[&str]) -> Terminal {
        let dimensions = Dimensions::new(40, rows.len().max(1)).unwrap();
        let mut terminal = Terminal::new(dimensions).unwrap();
        terminal.ingest(rows.join("\r\n").as_bytes());
        terminal
    }

    #[test]
    fn empty_query_has_no_matches() {
        let terminal = terminal_with_rows(&["hello world"]);
        let mut state = TerminalSearchState::default();
        state.set_query(&terminal, String::new());
        assert_eq!(state.match_count(), 0);
        assert_eq!(state.current_position(), None);
    }

    #[test]
    fn matches_case_insensitively_and_selects_first() {
        let terminal = terminal_with_rows(&["Hello World", "goodbye"]);
        let mut state = TerminalSearchState::default();
        state.set_query(&terminal, "WORLD".to_owned());
        assert_eq!(state.match_count(), 1);
        assert_eq!(state.current_position(), Some(1));
        assert_eq!(state.current_match_row(), Some(0));
    }

    #[test]
    fn advance_and_retreat_wrap() {
        let terminal = terminal_with_rows(&["cat", "dog", "cat"]);
        let mut state = TerminalSearchState::default();
        state.set_query(&terminal, "cat".to_owned());
        assert_eq!(state.match_count(), 2);
        assert_eq!(state.current_match_row(), Some(0));
        state.advance();
        assert_eq!(state.current_match_row(), Some(2));
        state.advance();
        assert_eq!(state.current_match_row(), Some(0));
        state.retreat();
        assert_eq!(state.current_match_row(), Some(2));
    }

    #[test]
    fn no_match_reports_empty_result() {
        let terminal = terminal_with_rows(&["hello world"]);
        let mut state = TerminalSearchState::default();
        state.set_query(&terminal, "xyz".to_owned());
        assert_eq!(state.match_count(), 0);
        assert_eq!(state.current_position(), None);
        assert_eq!(state.current_match_row(), None);
    }

    #[test]
    fn close_discards_query_and_matches() {
        let terminal = terminal_with_rows(&["hello world"]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "hello".to_owned());
        assert!(state.take_focus_request());
        state.close();
        assert!(!state.is_open());
        assert_eq!(state.query(), "");
        assert_eq!(state.match_count(), 0);
    }

    #[test]
    fn rescan_preserves_current_match_row_when_still_present() {
        let terminal = terminal_with_rows(&["alpha", "beta", "alpha"]);
        let mut state = TerminalSearchState::default();
        state.set_query(&terminal, "alpha".to_owned());
        state.advance();
        assert_eq!(state.current_match_row(), Some(2));
        // Re-scanning the same content (e.g. triggered by new output
        // elsewhere) must not silently move the current selection.
        state.rescan(&terminal);
        assert_eq!(state.current_match_row(), Some(2));
    }
}
