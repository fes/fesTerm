//! Terminal-content search (`docs/gui-design.md` "Terminal-content
//! search"). Deliberately kept in the `festerm` application crate rather
//! than `festerm-ui-egui`: match-scanning and navigation are pure text-model
//! logic over `festerm_core::Terminal`'s public accessors and stay testable
//! here without touching renderer internals.

use std::time::{Duration, Instant};

use festerm_core::{Cell, Dimensions, Terminal};

const OUTPUT_SETTLE_DELAY: Duration = Duration::from_millis(100);
const MAX_OUTPUT_REFRESH_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SearchSnapshot {
    screen_generation: u64,
    dimensions: Dimensions,
    alternate: bool,
    content_row_origin: u64,
    screen_row_origin: u64,
}

impl SearchSnapshot {
    fn capture(terminal: &Terminal) -> Self {
        let alternate = terminal.modes().alternate_screen();
        let (content_row_origin, screen_row_origin) = if alternate {
            (0, 0)
        } else {
            let history = terminal.scrollback_stats();
            (history.content_row_origin(), history.screen_row_origin())
        };
        Self {
            screen_generation: terminal.screen_generation(),
            dimensions: terminal.screen().dimensions(),
            alternate,
            content_row_origin,
            screen_row_origin,
        }
    }

    fn same_screen_layout(self, other: Self) -> bool {
        self.screen_generation == other.screen_generation
            && self.dimensions == other.dimensions
            && self.alternate == other.alternate
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingRefresh {
    first_change: Instant,
    last_change: Instant,
}

/// Per-session find-bar state. Owned by `SessionTab` so it lives and dies
/// with the tab and is never logged or persisted (`docs/gui-design.md`:
/// "Queries are never logged or persisted").
#[derive(Clone, Debug, Default)]
pub struct TerminalSearchState {
    open: bool,
    query: String,
    /// Oldest-first "document row" indices of every current match:
    /// retained scrollback rows (unless the alternate
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
    last_scan: Option<SearchSnapshot>,
    last_observed: Option<SearchSnapshot>,
    pending_refresh: Option<PendingRefresh>,
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

    /// A replacement terminal has a different row-identity namespace.
    pub fn terminal_replaced(&mut self, terminal: &Terminal) {
        self.current = None;
        self.last_scan = None;
        self.last_observed = None;
        self.pending_refresh = None;
        if self.open {
            self.rescan(terminal);
        }
    }

    /// Re-scans without changing the query text. Used when new terminal
    /// output may have introduced additional matches while the bar stays
    /// open: "New output may extend the result set without moving the
    /// current match unexpectedly."
    pub fn rescan(&mut self, terminal: &Terminal) {
        let snapshot = SearchSnapshot::capture(terminal);
        let previous_row = self
            .current
            .and_then(|index| self.matches.get(index).copied());
        let current_row = match (previous_row, self.last_scan) {
            (Some(_), Some(previous))
                if previous.screen_generation != snapshot.screen_generation
                    || previous.alternate != snapshot.alternate =>
            {
                None
            }
            (Some(row), Some(previous))
                if !previous.alternate && previous.same_screen_layout(snapshot) =>
            {
                u64::try_from(row)
                    .ok()
                    .and_then(|row| previous.content_row_origin.checked_add(row))
                    .and_then(|row| row.checked_sub(snapshot.content_row_origin))
                    .and_then(|row| usize::try_from(row).ok())
            }
            (row, _) => row,
        };
        self.matches = scan_matches(terminal, &self.query);
        self.current = current_row
            .and_then(|row| self.matches.iter().position(|&candidate| candidate == row))
            .or(if self.matches.is_empty() {
                None
            } else {
                Some(0)
            });
        self.last_scan = Some(snapshot);
        self.last_observed = Some(snapshot);
        self.pending_refresh = None;
    }

    fn note_change(&mut self, now: Instant) {
        let pending = self.pending_refresh.get_or_insert(PendingRefresh {
            first_change: now,
            last_change: now,
        });
        pending.last_change = now;
    }

    /// Observe dirty live rows before a terminal view consumes their flags.
    pub fn observe_screen_dirty(&mut self, terminal: &Terminal, now: Instant) {
        if self.open
            && !self.query.is_empty()
            && (0..terminal.screen().dimensions().rows())
                .any(|row| terminal.is_row_dirty(row) == Some(true))
        {
            self.note_change(now);
        }
    }

    /// Track layout changes made while rendering, including passive panes.
    pub fn observe_layout_change(&mut self, terminal: &Terminal, now: Instant) -> bool {
        if self.open
            && !self.query.is_empty()
            && self
                .last_observed
                .is_some_and(|previous| previous != SearchSnapshot::capture(terminal))
        {
            self.note_change(now);
            true
        } else {
            false
        }
    }

    /// Coalesces output-driven scans, but never delays them indefinitely.
    /// Returns a repaint delay while a refresh is pending.
    pub fn refresh_if_stale(&mut self, terminal: &Terminal, now: Instant) -> Option<Duration> {
        if self.query.is_empty() {
            return None;
        }
        let snapshot = SearchSnapshot::capture(terminal);
        let Some(previous) = self.last_observed else {
            self.rescan(terminal);
            return None;
        };
        if !previous.same_screen_layout(snapshot) {
            self.rescan(terminal);
            return None;
        }
        if snapshot != previous {
            self.note_change(now);
            self.last_observed = Some(snapshot);
        }
        self.observe_screen_dirty(terminal, now);
        let pending = self.pending_refresh?;
        let deadline = (pending.last_change + OUTPUT_SETTLE_DELAY)
            .min(pending.first_change + MAX_OUTPUT_REFRESH_DELAY);
        if now >= deadline {
            self.rescan(terminal);
            None
        } else {
            Some(deadline.duration_since(now))
        }
    }

    /// Navigation must not use a result that is waiting for a delayed refresh.
    pub fn refresh_pending(&mut self, terminal: &Terminal) {
        if self.pending_refresh.is_some() {
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

    let mut row = 0;
    let mut text = String::new();
    if history_rows > 0 {
        for line in terminal.scrollback_lines() {
            for line_row in 0..line.physical_rows() {
                let cells = line
                    .physical_row(line_row)
                    .expect("retained physical row exists");
                if row_contains(cells, &needle, &mut text) {
                    matches.push(row);
                }
                row += 1;
            }
        }
        debug_assert_eq!(row, history_rows);
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

fn row_contains(cells: &[Cell], needle: &str, text: &mut String) -> bool {
    text.clear();
    text.extend(cells.iter().map(Cell::character));
    if text.is_ascii() {
        text.make_ascii_lowercase();
        text.contains(needle)
    } else {
        text.to_lowercase().contains(needle)
    }
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

    fn indexed_scan_matches(terminal: &Terminal, query: &str) -> Vec<usize> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle = query.to_lowercase();
        let history_rows = if terminal.modes().alternate_screen() {
            0
        } else {
            terminal.scrollback_stats().physical_rows()
        };
        let mut matches = Vec::new();
        for row in 0..history_rows {
            if let Some(cells) = terminal.scrollback_physical_row(row) {
                let text: String = cells.iter().map(Cell::character).collect();
                if text.to_lowercase().contains(&needle) {
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

    fn assert_matches_indexed_scan(terminal: &Terminal) {
        for query in [
            "", "alpha", "NEEDLE", "Σ", "ος", "İ", "i", "東京", "🙂", " ", "missing",
        ] {
            assert_eq!(
                scan_matches(terminal, query),
                indexed_scan_matches(terminal, query),
                "query {query:?}"
            );
        }
    }

    #[test]
    fn sequential_history_scan_matches_indexed_scan() {
        let mut terminal =
            Terminal::with_scrollback_limit(Dimensions::new(12, 4).unwrap(), 16 * 1024).unwrap();
        for _ in 0..32 {
            terminal.ingest("alpha NEEDLE wrapped row ΟΣ 東京\r\nİstanbul 🙂\r\n".as_bytes());
        }
        assert!(terminal.scrollback_stats().physical_rows() > 0);
        assert!(terminal.scrollback_stats().evicted_lines() > 0);
        assert_matches_indexed_scan(&terminal);

        terminal.resize(Dimensions::new(8, 4).unwrap()).unwrap();
        assert_matches_indexed_scan(&terminal);
        terminal.ingest("\x1b[?1049halpha ΟΣ 東京\r\nNEEDLE🙂".as_bytes());
        assert_matches_indexed_scan(&terminal);
        terminal.ingest(b"\x1b[?1049l");
        assert_matches_indexed_scan(&terminal);
    }

    #[test]
    fn output_refresh_coalesces_but_never_starves() {
        let mut terminal = terminal_with_rows(&["match", "other", ""]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "match".to_owned());
        terminal.take_dirty_rows();
        let initial_count = state.match_count();
        let start = Instant::now();

        for frame in 0..16 {
            terminal.ingest(b"\r\nmatch");
            let now = start + Duration::from_millis(frame * 16);
            assert!(state.refresh_if_stale(&terminal, now).is_some());
            terminal.take_dirty_rows();
            assert_eq!(state.match_count(), initial_count);
        }
        terminal.ingest(b"\r\nmatch");
        assert_eq!(
            state.refresh_if_stale(&terminal, start + MAX_OUTPUT_REFRESH_DELAY),
            None
        );
        terminal.take_dirty_rows();
        assert!(state.match_count() > initial_count);

        let count = state.match_count();
        terminal.ingest(b"\r\nmatch");
        let last_change = start + Duration::from_millis(260);
        assert_eq!(
            state.refresh_if_stale(&terminal, last_change),
            Some(OUTPUT_SETTLE_DELAY)
        );
        terminal.take_dirty_rows();
        assert_eq!(
            state.refresh_if_stale(&terminal, last_change + OUTPUT_SETTLE_DELAY),
            None
        );
        assert!(state.match_count() > count);
    }

    #[test]
    fn full_history_refresh_preserves_a_retained_match_through_eviction() {
        let mut terminal =
            Terminal::with_scrollback_limit(Dimensions::new(20, 3).unwrap(), 8 * 1024).unwrap();
        for _ in 0..1_000 {
            terminal.ingest(b"same\r\n");
            if terminal.scrollback_stats().evicted_lines() > 0 {
                break;
            }
        }
        let before = terminal.scrollback_stats();
        assert!(before.evicted_lines() > 0);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "same".to_owned());
        for _ in 0..4 {
            state.advance();
        }
        let old_row = state.current_match_row().unwrap();
        terminal.take_dirty_rows();

        terminal.ingest(b"same\r\n");
        let after = terminal.scrollback_stats();
        assert_eq!(after.physical_rows(), before.physical_rows());
        assert_eq!(after.content_row_origin(), before.content_row_origin() + 1);
        let start = Instant::now();
        assert_eq!(
            state.refresh_if_stale(&terminal, start),
            Some(OUTPUT_SETTLE_DELAY)
        );
        terminal.take_dirty_rows();
        assert_eq!(
            state.refresh_if_stale(&terminal, start + OUTPUT_SETTLE_DELAY),
            None
        );
        assert_eq!(state.current_match_row(), Some(old_row - 1));
    }

    #[test]
    fn passive_view_dirty_rows_refresh_live_screen_matches() {
        let mut terminal = terminal_with_rows(&["first", ""]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "needle".to_owned());
        terminal.take_dirty_rows();
        assert_eq!(state.match_count(), 0);

        terminal.ingest(b"\x1b[1;1Hneedle");
        let start = Instant::now();
        state.observe_screen_dirty(&terminal, start);
        terminal.take_dirty_rows();
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(50)),
            Some(Duration::from_millis(50))
        );
        assert_eq!(
            state.refresh_if_stale(&terminal, start + OUTPUT_SETTLE_DELAY),
            None
        );
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn screen_switch_and_navigation_refresh_without_delay() {
        let mut terminal = terminal_with_rows(&["first", ""]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "needle".to_owned());
        terminal.take_dirty_rows();
        let start = Instant::now();

        terminal.ingest(b"\x1b[1;1Hneedle");
        assert!(state.refresh_if_stale(&terminal, start).is_some());
        state.refresh_pending(&terminal);
        assert_eq!(state.match_count(), 1);
        terminal.take_dirty_rows();

        terminal.ingest(b"\x1b[?1049hneedle");
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(1)),
            None
        );
        assert_eq!(state.current_match_row(), Some(0));
        terminal.ingest(b"\x1b[?1049l");
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(2)),
            None
        );
        assert_eq!(state.match_count(), 1);
    }

    #[test]
    fn screen_switch_does_not_alias_old_selection() {
        let mut terminal = terminal_with_rows(&["match first", "match second"]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "match".to_owned());
        state.advance();
        assert_eq!(state.current_match_row(), Some(1));

        terminal.ingest(b"\x1b[?1049hmatch new\r\nmatch also new");
        assert_eq!(state.refresh_if_stale(&terminal, Instant::now()), None);
        assert_eq!(state.match_count(), 2);
        assert_eq!(state.current_match_row(), Some(0));
    }

    #[test]
    fn query_resize_and_reset_refresh_without_delay() {
        let mut terminal = terminal_with_rows(&["first", ""]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "missing".to_owned());
        terminal.take_dirty_rows();
        let start = Instant::now();

        terminal.ingest(b"\x1b[1;1Hneedle");
        assert!(state.refresh_if_stale(&terminal, start).is_some());
        state.set_query(&terminal, "needle".to_owned());
        assert_eq!(state.match_count(), 1);
        terminal.take_dirty_rows();
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(1)),
            None
        );

        terminal.resize(Dimensions::new(20, 2).unwrap()).unwrap();
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(2)),
            None
        );
        assert_eq!(
            state.matches,
            indexed_scan_matches(&terminal, state.query())
        );
        terminal.reset_to_initial_state();
        assert_eq!(
            state.refresh_if_stale(&terminal, start + Duration::from_millis(3)),
            None
        );
        assert_eq!(state.match_count(), 0);
    }

    #[test]
    fn replacement_does_not_alias_old_terminal_rows() {
        let terminal = terminal_with_rows(&["match first", "match second"]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "match".to_owned());
        state.advance();
        assert_eq!(state.current_match_row(), Some(1));

        let replacement = terminal_with_rows(&["match new", "match also new"]);
        assert_eq!(
            SearchSnapshot::capture(&terminal),
            SearchSnapshot::capture(&replacement)
        );
        state.terminal_replaced(&replacement);
        assert_eq!(state.match_count(), 2);
        assert_eq!(state.current_match_row(), Some(0));
    }

    #[test]
    fn passive_resize_is_remembered_even_if_dimensions_return() {
        let mut terminal = terminal_with_rows(&["needle", ""]);
        let mut state = TerminalSearchState::default();
        state.open();
        state.set_query(&terminal, "needle".to_owned());
        terminal.take_dirty_rows();
        let original = terminal.dimensions();
        let start = Instant::now();

        terminal.resize(Dimensions::new(20, 2).unwrap()).unwrap();
        terminal.take_dirty_rows();
        assert!(state.observe_layout_change(&terminal, start));
        terminal.resize(original).unwrap();
        terminal.take_dirty_rows();
        assert!(!state.observe_layout_change(&terminal, start));
        assert_eq!(
            state.refresh_if_stale(&terminal, start),
            Some(OUTPUT_SETTLE_DELAY)
        );
        assert_eq!(
            state.refresh_if_stale(&terminal, start + OUTPUT_SETTLE_DELAY),
            None
        );
        assert_eq!(
            state.matches,
            indexed_scan_matches(&terminal, state.query())
        );
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
