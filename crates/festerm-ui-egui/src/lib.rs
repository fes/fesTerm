//! `egui` presentation for the GUI-independent terminal core.
//!
//! This crate owns points, fonts, glyph layout, selection presentation, and
//! native-window input translation. Terminal protocol state and input encoding
//! remain in `festerm-core`.

use egui::Color32;
use festerm_core::{Cell, Cursor, CursorStyle, Screen, Terminal, TerminalModes};

mod cache;
pub mod chrome;
mod geometry;
mod input;
pub mod overlay;
pub mod palette;
mod renderer;
mod selection;
mod view;

pub(crate) const DEFAULT_FOREGROUND: Color32 = Color32::from_rgb(220, 220, 220);
pub(crate) const DEFAULT_BACKGROUND: Color32 = Color32::from_rgb(24, 24, 24);
pub(crate) const SELECTION_BACKGROUND: Color32 = Color32::from_rgb(52, 91, 135);
pub(crate) const GLYPH_CACHE_CAPACITY: usize = 4_096;

// --- Public re-exports ---
pub use cache::{
    RenderCacheUpdate, RenderedCell, ResizeOutcome, ResizeTracker, TerminalRenderCache,
};
pub use geometry::{
    cell_from_point, dimensions_from_points, CellMetrics, CellPosition, CellRange, ViewSize,
};
pub use input::{
    route_input, route_mouse_input, EncodedInputSink, InputRoute, InputSinkDiagnostics,
};
pub use renderer::{resolve_color, FontSettings};
pub use selection::{normalize_selection_position, selection_text, Selection};
pub use view::{FrameDiagnostics, TerminalView};

// --- Crate-internal re-exports (only needed by the test module) ---
#[cfg(test)]
pub(crate) use egui::{Pos2, Rect, Vec2};
#[cfg(test)]
pub(crate) use festerm_core::{
    Attributes, CellWidth, Color, Dimensions, FocusEvent, InputEvent, InputEventOutcome, Key,
    Modifiers, MouseButton, MouseEvent, MouseEventKind, MAX_CELL_COUNT,
};
#[cfg(test)]
pub(crate) use geometry::{dimensions_from_viewport, grid_view_size, viewport_layout};
#[cfg(test)]
pub(crate) use input::{
    record_terminal_input, route_pointer_event, InputRoutingReports, KeyboardOwnership,
    PointerInputEvent, TerminalPointerState,
};
#[cfg(test)]
pub(crate) use renderer::{
    cell_colors, cell_needs_background_paint, grid_cell_rect, measure_input_to_paint_submission,
    rendered_cell_columns, rendered_cell_is_selected, GridLayout,
};
#[cfg(test)]
pub(crate) use std::time::{Duration, Instant};

/// A read-only, renderer-facing view of the currently visible terminal grid.
///
/// It borrows core state and therefore cannot outlive or mutate the terminal.
/// The renderer copies only rows announced as dirty into its presentation
/// cache; it does not clone a complete core grid per GUI frame.
#[derive(Clone, Copy)]
pub struct TerminalSnapshot<'a> {
    screen: &'a Screen,
    cursor: Cursor,
    cursor_style: CursorStyle,
    modes: TerminalModes,
}

impl<'a> TerminalSnapshot<'a> {
    pub fn from_terminal(terminal: &'a Terminal) -> Self {
        Self {
            screen: terminal.screen(),
            cursor: terminal.cursor(),
            cursor_style: terminal.cursor_style(),
            modes: terminal.modes(),
        }
    }

    pub const fn dimensions(self) -> festerm_core::Dimensions {
        self.screen.dimensions()
    }

    pub const fn cursor(self) -> Cursor {
        self.cursor
    }

    pub const fn cursor_style(self) -> CursorStyle {
        self.cursor_style
    }

    pub const fn modes(self) -> TerminalModes {
        self.modes
    }

    /// Returns a borrowed core cell, preserving width-two/continuation roles.
    pub fn cell(self, column: usize, row: usize) -> Option<&'a Cell> {
        self.screen.cell_ref(column, row)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use egui_kittest::{kittest::Queryable, Harness, SnapshotResults};
    use festerm_test_support::load_fixture;

    use super::*;

    #[derive(Default)]
    struct Sink(Vec<Vec<u8>>);

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    fn grid_layout(columns: usize, rows: usize) -> GridLayout {
        GridLayout {
            rect: Rect::from_min_size(
                Pos2::new(5.0, 7.0),
                Vec2::new(columns as f32 * 10.0, rows as f32 * 20.0),
            ),
            dimensions: Dimensions::new(columns, rows).expect("valid test size"),
            metrics: CellMetrics::new(10.0, 20.0).expect("valid test cell metrics"),
        }
    }

    #[test]
    fn point_dimensions_are_bounded_and_valid() {
        let cell = CellMetrics::new(8.0, 16.0).unwrap();
        assert_eq!(
            dimensions_from_points(
                ViewSize {
                    width: 1.0,
                    height: 1.0
                },
                cell
            ),
            Some(Dimensions::new(2, 1).unwrap())
        );
        let maximum = dimensions_from_points(
            ViewSize {
                width: f32::MAX,
                height: f32::MAX,
            },
            cell,
        )
        .unwrap();
        assert!(maximum.cell_count() <= MAX_CELL_COUNT);
        assert_eq!(
            dimensions_from_points(
                ViewSize {
                    width: f32::NAN,
                    height: 5.0
                },
                cell
            ),
            None
        );
    }

    #[test]
    fn grid_view_reserves_the_diagnostics_footer() {
        assert_eq!(
            grid_view_size(Vec2::new(800.0, 600.0), 18.0),
            ViewSize {
                width: 800.0,
                height: 582.0,
            }
        );
        assert_eq!(
            grid_view_size(Vec2::new(800.0, 12.0), 18.0),
            ViewSize {
                width: 800.0,
                height: 0.0,
            }
        );
    }

    struct HeadlessViewState {
        view: TerminalView,
        terminal: Terminal,
        sink: Sink,
    }

    impl HeadlessViewState {
        fn new() -> Self {
            Self {
                view: TerminalView::default(),
                terminal: terminal(80, 24),
                sink: Sink::default(),
            }
        }

        fn with_terminal(terminal: Terminal) -> Self {
            Self {
                view: TerminalView::default(),
                terminal,
                sink: Sink::default(),
            }
        }
    }

    #[test]
    fn headless_harness_drives_terminal_view_input_resize_and_diagnostics() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(800.0, 600.0))
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show_with_status(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        "headless session",
                        "headless diagnostics",
                    );
                },
                HeadlessViewState::new(),
            );

        let grid = harness
            .state()
            .view
            .diagnostics()
            .grid_rect
            .expect("headless frame records grid geometry");
        assert_eq!(
            harness.state().view.diagnostics().calculated_dimensions,
            Some(harness.state().terminal.dimensions())
        );
        assert!(grid.is_finite());

        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::Text("Q".to_owned()));
        harness.run();
        assert_eq!(harness.state().sink.0, vec![b"Q".to_vec()]);

        harness.key_press(egui::Key::Tab);
        harness.run();
        harness.key_press(egui::Key::ArrowUp);
        harness.run();
        harness.key_press(egui::Key::ArrowDown);
        harness.run();
        harness.key_press(egui::Key::ArrowLeft);
        harness.run();
        harness.key_press(egui::Key::ArrowRight);
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert_eq!(
            harness.state().sink.0,
            vec![
                b"Q".to_vec(),
                b"\t".to_vec(),
                b"\x1b[A".to_vec(),
                b"\x1b[B".to_vec(),
                b"\x1b[D".to_vec(),
                b"\x1b[C".to_vec(),
                b"\x1b".to_vec(),
            ]
        );

        {
            harness.get_by_label("Diagnostics").click();
        }
        harness.run();
        assert!(harness.state().view.show_diagnostics);

        harness.set_size(Vec2::new(730.0, 520.0));
        harness.run();
        let state = harness.state();
        assert_eq!(
            state.view.diagnostics().calculated_dimensions,
            Some(state.terminal.dimensions())
        );
        assert_eq!(
            state.view.cache.dimensions(),
            Some(state.terminal.dimensions())
        );
        assert!(state
            .view
            .diagnostics()
            .grid_rect
            .is_some_and(|grid| grid.is_finite()));
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn visual_harness(terminal: Terminal) -> Harness<'static, HeadlessViewState> {
        Harness::builder()
            .with_size(Vec2::new(640.0, 360.0))
            .with_pixels_per_point(1.0)
            .with_theme(egui::Theme::Dark)
            .wgpu()
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show_with_status(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        "snapshot session",
                        "snapshot diagnostics",
                    );
                },
                HeadlessViewState::with_terminal(terminal),
            )
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn assert_snapshot_invariants(harness: &Harness<'_, HeadlessViewState>) {
        let state = harness.state();
        assert_eq!(
            state.view.diagnostics().calculated_dimensions,
            Some(state.terminal.dimensions())
        );
        assert_eq!(
            state.view.cache.dimensions(),
            Some(state.terminal.dimensions())
        );
        assert!(state
            .view
            .diagnostics()
            .grid_rect
            .is_some_and(|grid| grid.is_finite() && grid.width() > 0.0 && grid.height() > 0.0));
        for row in 0..state.terminal.dimensions().rows() {
            assert_eq!(
                state.view.cache.row(row).map(<[RenderedCell]>::len),
                Some(state.terminal.dimensions().columns())
            );
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn snapshot_after_structural_assertions(
        harness: &mut Harness<'_, HeadlessViewState>,
        name: &str,
        snapshots: &mut SnapshotResults,
    ) {
        assert_snapshot_invariants(harness);
        snapshots.add(harness.try_snapshot(name));
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn focus_terminal_grid(harness: &mut Harness<'_, HeadlessViewState>) {
        let grid = harness
            .state()
            .view
            .diagnostics()
            .grid_rect
            .expect("rendered frame records grid geometry");
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn rendered_terminal_frames_match_reviewed_snapshots() {
        let mut snapshots = SnapshotResults::new();
        let mut empty = visual_harness(terminal(80, 24));
        snapshot_after_structural_assertions(&mut empty, "terminal-empty", &mut snapshots);

        let mut attributes_terminal = terminal(80, 24);
        attributes_terminal.ingest(
            b"\x1b[31mred \x1b[38;5;39mindexed \x1b[38;2;70;150;240mrgb \
              \x1b[7minverse \x1b[4munderline \x1b[9mstrike\x1b[0m",
        );
        let mut attributes = visual_harness(attributes_terminal);
        snapshot_after_structural_assertions(
            &mut attributes,
            "terminal-attributes",
            &mut snapshots,
        );

        for (name, style) in [
            ("terminal-cursor-block", b"\x1b[2 q".as_slice()),
            ("terminal-cursor-underline", b"\x1b[4 q".as_slice()),
            ("terminal-cursor-bar", b"\x1b[6 q".as_slice()),
        ] {
            let mut cursor_terminal = terminal(80, 24);
            cursor_terminal.ingest(style);
            cursor_terminal.ingest(b"cursor");
            let mut cursor = visual_harness(cursor_terminal);
            focus_terminal_grid(&mut cursor);
            snapshot_after_structural_assertions(&mut cursor, name, &mut snapshots);
        }

        let mut unicode_terminal = terminal(80, 24);
        unicode_terminal.ingest("wide \u{754c} combining e\u{301}".as_bytes());
        let mut unicode = visual_harness(unicode_terminal);
        unicode
            .state_mut()
            .view
            .selection
            .begin(CellPosition { column: 5, row: 0 });
        unicode
            .state_mut()
            .view
            .selection
            .extend(CellPosition { column: 10, row: 0 });
        unicode.state_mut().view.selection.finish();
        unicode.step();
        snapshot_after_structural_assertions(
            &mut unicode,
            "terminal-unicode-selection",
            &mut snapshots,
        );

        let mut alternate_terminal = terminal(80, 24);
        alternate_terminal.ingest(b"primary\x1b[?1049h\x1b[6 qalternate screen");
        let mut alternate = visual_harness(alternate_terminal);
        snapshot_after_structural_assertions(
            &mut alternate,
            "terminal-alternate-screen",
            &mut snapshots,
        );

        let mut resize_terminal = terminal(80, 24);
        resize_terminal.ingest(b"banner\r\nprompt> ");
        let mut resize = visual_harness(resize_terminal);
        resize
            .state_mut()
            .view
            .selection
            .begin(CellPosition { column: 0, row: 0 });
        resize
            .state_mut()
            .view
            .selection
            .extend(CellPosition { column: 5, row: 0 });
        resize.state_mut().view.selection.finish();
        for (name, size, output) in [
            (
                "terminal-resize-narrow",
                Vec2::new(370.0, 300.0),
                b"\x1b[2;1Hpartial narrow".as_slice(),
            ),
            (
                "terminal-resize-wide",
                Vec2::new(730.0, 560.0),
                b"\x1b[3;1Hpartial wide".as_slice(),
            ),
            (
                "terminal-resize-medium",
                Vec2::new(500.0, 400.0),
                b"\x1b[4;1Hpartial medium".as_slice(),
            ),
            (
                "terminal-resize-wide-repeat",
                Vec2::new(730.0, 560.0),
                b"\x1b[5;1Hpartial wide repeat".as_slice(),
            ),
        ] {
            resize.set_size(size);
            resize.state_mut().terminal.ingest(output);
            resize.step();
            snapshot_after_structural_assertions(&mut resize, name, &mut snapshots);
        }
        snapshots.unwrap();
    }

    #[test]
    fn undersized_viewport_does_not_shrink_the_terminal_or_lose_cached_content() {
        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        let mut terminal = terminal(80, 24);
        terminal.ingest(b"Windows banner\r\nC:\\Users\\fes>");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );
        let mut resize = ResizeTracker::default();

        for viewport in [
            ViewSize {
                width: 370.0,
                height: 260.0,
            },
            ViewSize {
                width: 0.0,
                height: 0.0,
            },
            ViewSize {
                width: 8.0,
                height: 19.0,
            },
            ViewSize {
                width: 730.0,
                height: 520.0,
            },
        ] {
            resize.apply_viewport(&mut terminal, viewport, cell);
            let dirty_rows = terminal.take_dirty_rows();
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

            assert!(terminal
                .row_text(0)
                .is_some_and(|row| row.starts_with("Windows banner")));
            assert!(cache
                .row(0)
                .is_some_and(|row| row.first().is_some_and(|cell| cell.text() == "W")));
        }
        assert_eq!(terminal.dimensions(), Dimensions::new(73, 26).unwrap());
    }

    #[test]
    fn viewport_replay_preserves_cache_geometry_and_selection_during_output_resizes() {
        enum Step {
            Output(&'static [u8]),
            Viewport(ViewSize),
            Selection(CellPosition, CellPosition),
        }

        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        let mut terminal = terminal(80, 24);
        let mut cache = TerminalRenderCache::default();
        let mut resize = ResizeTracker::default();
        let mut selection = Selection::default();
        let mut sink = Sink::default();

        for step in [
            Step::Output(b"Windows banner\r\nC:\\Users\\fes>"),
            Step::Viewport(ViewSize {
                width: 370.0,
                height: 260.0,
            }),
            Step::Selection(
                CellPosition { column: 0, row: 0 },
                CellPosition { column: 6, row: 0 },
            ),
            Step::Output(b"\x1b[2;1Hactive output"),
            Step::Viewport(ViewSize {
                width: 0.0,
                height: 0.0,
            }),
            Step::Output(b"\x1b[3;1Hpartial"),
            Step::Viewport(ViewSize {
                width: 500.0,
                height: 360.0,
            }),
            Step::Viewport(ViewSize {
                width: 730.0,
                height: 520.0,
            }),
        ] {
            match step {
                Step::Output(bytes) => terminal.ingest(bytes),
                Step::Viewport(viewport) => {
                    let outcome = resize.apply_viewport(&mut terminal, viewport, cell);
                    if matches!(outcome, ResizeOutcome::Resized(_)) {
                        selection.clear();
                        assert!(
                            selection.range().is_none(),
                            "a real terminal resize must clear local selection"
                        );
                    }
                    let layout =
                        viewport_layout(Pos2::new(0.0, 0.0), viewport, cell, terminal.dimensions());
                    assert_eq!(layout.dimensions, terminal.dimensions());
                    assert_eq!(layout.viewport.min, layout.grid.min);
                    if dimensions_from_viewport(viewport, cell).is_some() {
                        assert!(
                            layout.viewport.contains_rect(layout.grid),
                            "accepted terminal dimensions must fit the allocated viewport"
                        );
                    }
                    let cursor = terminal.cursor();
                    assert!(cursor.column() < terminal.dimensions().columns());
                    assert!(cursor.row() < terminal.dimensions().rows());
                    let cursor_rect = grid_cell_rect(
                        GridLayout {
                            rect: layout.grid,
                            dimensions: layout.dimensions,
                            metrics: cell,
                        },
                        CellPosition {
                            column: cursor.column(),
                            row: cursor.row(),
                        },
                        1,
                    );
                    assert!(cursor_rect.is_finite());
                    if dimensions_from_viewport(viewport, cell).is_some() {
                        assert!(layout.viewport.contains_rect(cursor_rect));
                    }
                }
                Step::Selection(start, end) => {
                    route_mouse_input(
                        &mut terminal,
                        MouseEvent {
                            kind: MouseEventKind::Press(MouseButton::Left),
                            column: start.column,
                            row: start.row,
                            modifiers: Modifiers::NONE,
                        },
                        &mut selection,
                        &mut sink,
                    );
                    route_mouse_input(
                        &mut terminal,
                        MouseEvent {
                            kind: MouseEventKind::Release(MouseButton::Left),
                            column: end.column,
                            row: end.row,
                            modifiers: Modifiers::NONE,
                        },
                        &mut selection,
                        &mut sink,
                    );
                    assert!(selection.range().is_some());
                }
            }

            let dirty_rows = terminal.take_dirty_rows();
            assert!(dirty_rows
                .iter()
                .all(|row| *row < terminal.dimensions().rows()));
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
            assert_eq!(cache.dimensions(), Some(terminal.dimensions()));
            for row in 0..terminal.dimensions().rows() {
                assert_eq!(
                    cache.row(row).map(<[RenderedCell]>::len),
                    Some(terminal.dimensions().columns())
                );
            }
        }

        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("Windows banner")));
        assert!(terminal
            .row_text(1)
            .is_some_and(|row| row.starts_with("active output")));
        assert!(terminal
            .row_text(2)
            .is_some_and(|row| row.starts_with("partial")));
        assert!(sink.0.is_empty());
    }

    #[test]
    fn point_mapping_uses_zero_based_cell_coordinates() {
        let dimensions = Dimensions::new(4, 2).unwrap();
        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        assert_eq!(
            cell_from_point(Pos2::new(5.0, 7.0), dimensions, cell, Pos2::new(34.9, 46.9)),
            Some(CellPosition { column: 2, row: 1 })
        );
        assert_eq!(
            cell_from_point(Pos2::new(5.0, 7.0), dimensions, cell, Pos2::new(45.0, 7.0)),
            None
        );
    }

    #[test]
    fn selection_expands_continuations_and_copies_leading_text() {
        let mut terminal = terminal(8, 1);
        terminal.ingest("A界e".as_bytes());
        terminal.take_dirty_rows();
        let mut selection = Selection::default();
        let mut sink = Sink::default();

        let press = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 2,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(press.outcome, InputEventOutcome::SelectionAllowed);
        let release = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Release(MouseButton::Left),
                column: 3,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );
        assert_eq!(release.outcome, InputEventOutcome::SelectionAllowed);
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 0 }
            ))
        );
        assert_eq!(
            selection_text(TerminalSnapshot::from_terminal(&terminal), &selection),
            Some("界e".to_owned())
        );
        assert!(sink.0.is_empty());
    }

    #[test]
    fn wide_cells_use_one_two_column_paint_and_selection_span() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[4;38;2;1;2;3;48;5;196m\xe7\x95\x8c");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cells = cache.row(0).expect("cached row");
        let leading = &cells[0];
        let continuation = &cells[1];

        assert_eq!(leading.width(), CellWidth::Double);
        assert_eq!(continuation.width(), CellWidth::Continuation);
        assert_eq!(leading.attributes(), continuation.attributes());
        assert_eq!(leading.foreground(), continuation.foreground());
        assert_eq!(leading.background(), continuation.background());

        let layout = grid_layout(4, 1);
        let rect = grid_cell_rect(layout, CellPosition { column: 0, row: 0 }, 2);
        assert_eq!(rect.min, Pos2::new(5.0, 7.0));
        assert_eq!(rect.size(), Vec2::new(20.0, 20.0));
        assert!(rendered_cell_is_selected(
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 1, row: 0 },
            )),
            CellPosition { column: 0, row: 0 },
            rendered_cell_columns(leading, layout.dimensions, 0),
        ));
    }

    #[test]
    fn application_mouse_claim_prevents_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1000h");
        let mut selection = Selection::default();
        let mut sink = Sink::default();
        let route = route_mouse_input(
            &mut terminal,
            MouseEvent {
                kind: MouseEventKind::Press(MouseButton::Left),
                column: 1,
                row: 0,
                modifiers: Modifiers::NONE,
            },
            &mut selection,
            &mut sink,
        );

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 6 });
        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"\x1b[M \"!".to_vec()]);
    }

    #[test]
    fn focus_out_routes_once_after_prior_terminal_keyboard_ownership() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1004h");
        let mut keyboard = KeyboardOwnership {
            terminal_owned: true,
        };
        let mut sink = Sink::default();

        let first = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));
        let second = keyboard
            .focus_out_if_owned()
            .map(|focus| route_input(&mut terminal, InputEvent::Focus(focus), &mut sink));

        assert_eq!(
            first.map(|route| route.outcome),
            Some(InputEventOutcome::Encoded { bytes: 3 })
        );
        assert_eq!(second, None);
        assert_eq!(sink.0, vec![b"\x1b[O".to_vec()]);
    }

    #[test]
    fn ordered_drag_uses_button_state_before_same_frame_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1002h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Moved {
                position: Pos2::new(26.0, 8.0),
            },
            PointerInputEvent::Button {
                position: Pos2::new(26.0, 8.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert!(route_pointer_event(
                event,
                layout,
                &mut terminal,
                &mut selection,
                &mut pointer,
                &mut sink,
            )
            .is_some());
        }

        assert_eq!(
            sink.0,
            vec![
                b"\x1b[<0;1;1M".to_vec(),
                b"\x1b[<32;3;1M".to_vec(),
                b"\x1b[<0;3;1m".to_vec(),
            ]
        );
    }

    #[test]
    fn application_mouse_release_outside_grid_is_captured_and_clamped() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"\x1b[?1000h\x1b[?1006h");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(6.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
        )
        .is_some());
        assert!(route_pointer_event(
            PointerInputEvent::Button {
                position: Pos2::new(-50.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
            layout,
            &mut terminal,
            &mut selection,
            &mut pointer,
            &mut sink,
        )
        .is_some());

        assert_eq!(
            sink.0,
            vec![b"\x1b[<0;1;1M".to_vec(), b"\x1b[<0;1;2m".to_vec()]
        );
        assert_eq!(selection.range(), None);
    }

    #[test]
    fn local_selection_capture_clamps_an_outside_release() {
        let mut terminal = terminal(4, 2);
        terminal.ingest(b"ABCD");
        let mut selection = Selection::default();
        let mut pointer = TerminalPointerState::default();
        let mut sink = Sink::default();
        let layout = grid_layout(4, 2);

        for event in [
            PointerInputEvent::Button {
                position: Pos2::new(16.0, 8.0),
                button: MouseButton::Left,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            PointerInputEvent::Button {
                position: Pos2::new(200.0, 300.0),
                button: MouseButton::Left,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ] {
            assert_eq!(
                route_pointer_event(
                    event,
                    layout,
                    &mut terminal,
                    &mut selection,
                    &mut pointer,
                    &mut sink,
                )
                .map(|route| route.outcome),
                Some(InputEventOutcome::SelectionAllowed)
            );
        }

        assert!(!selection.is_active());
        assert_eq!(
            selection.range(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 3, row: 1 },
            ))
        );
    }

    #[test]
    fn routing_uses_core_keyboard_modes_and_drains_to_sink() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?1h");
        let mut sink = Sink::default();

        let route = route_input(&mut terminal, InputEvent::Key(Key::ArrowUp), &mut sink);

        assert_eq!(route.outcome, InputEventOutcome::Encoded { bytes: 3 });
        assert_eq!(route.queue_depth, 3);
        assert_eq!(sink.0, vec![b"\x1bOA".to_vec()]);
        assert!(terminal.queued_input().is_empty());
    }

    #[test]
    fn accepted_typed_input_clears_local_selection() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"selected");
        let mut selection = Selection::default();
        selection.begin(CellPosition { column: 0, row: 0 });
        selection.extend(CellPosition { column: 3, row: 0 });
        selection.finish();
        let mut sink = Sink::default();
        let mut reports = InputRoutingReports::default();

        record_terminal_input(
            &mut reports,
            &mut selection,
            Instant::now(),
            route_input(
                &mut terminal,
                InputEvent::Key(Key::Character('x')),
                &mut sink,
            ),
        );

        assert_eq!(selection.range(), None);
        assert_eq!(sink.0, vec![b"x".to_vec()]);
    }

    #[test]
    fn routing_uses_core_paste_and_focus_modes() {
        let mut terminal = terminal(8, 1);
        terminal.ingest(b"\x1b[?2004h\x1b[?1004h");
        let mut sink = Sink::default();

        assert_eq!(
            route_input(
                &mut terminal,
                InputEvent::Paste("paste".to_owned()),
                &mut sink
            )
            .outcome,
            InputEventOutcome::Encoded { bytes: 17 }
        );
        assert_eq!(
            route_input(&mut terminal, InputEvent::Focus(FocusEvent::In), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        assert_eq!(
            sink.0,
            vec![b"\x1b[200~paste\x1b[201~".to_vec(), b"\x1b[I".to_vec()]
        );
    }

    #[test]
    fn input_latency_finishes_after_paint_submission_work() {
        let observed = Instant::now();
        let (_, elapsed) = measure_input_to_paint_submission(Some(observed), || {
            std::thread::sleep(Duration::from_millis(2));
        });

        assert!(
            elapsed.is_some_and(|duration| duration >= Duration::from_millis(2)),
            "the measurement must include the submitted grid paint work"
        );
        assert_eq!(measure_input_to_paint_submission::<()>(None, || ()).1, None);
    }

    #[test]
    fn cache_updates_dirty_rows_without_full_grid_copies() {
        let mut terminal = terminal(4, 2);
        terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        terminal.ingest(b"A");
        let dirty_rows = terminal.take_dirty_rows();

        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(update.updated_rows, vec![0, 1]);
        terminal.ingest(b"B");
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        assert!(!update.full_refresh);
        assert_eq!(update.updated_rows, vec![0]);
        assert_eq!(cache.row(0).unwrap()[1].text(), "B");
    }

    #[test]
    fn recorded_fixture_state_preserves_renderer_cell_structure() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/m3/unicode-cells.fixture");
        let fixture = load_fixture(&path).expect("fixture parses");
        let mut terminal = Terminal::new(fixture.dimensions).unwrap();
        terminal.ingest(&fixture.input);
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        let row = cache.row(0).unwrap();
        assert_eq!(row[1].text(), "界");
        assert_eq!(row[1].width(), CellWidth::Double);
        assert_eq!(row[2].width(), CellWidth::Continuation);
        assert_eq!(row[3].text(), "e\u{301}");
    }

    #[test]
    fn cache_preserves_passive_hyperlink_metadata() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\go\x1b]8;;\x1b\\");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert_eq!(
            cache.row(0).unwrap()[0].hyperlink(),
            Some("https://example.com")
        );
        assert_eq!(cache.row(0).unwrap()[2].hyperlink(), None);
    }

    #[test]
    fn renderer_resolves_terminal_colors_and_basic_attributes() {
        assert_eq!(
            resolve_color(Color::Indexed(196), DEFAULT_BACKGROUND),
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            resolve_color(
                Color::Rgb {
                    red: 1,
                    green: 2,
                    blue: 3
                },
                DEFAULT_BACKGROUND
            ),
            Color32::from_rgb(1, 2, 3)
        );

        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[1;3;4;7;8;9;38;2;1;2;3;48;5;196mX");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cell = &cache.row(0).unwrap()[0];

        assert!(cell.attributes.contains(Attributes::BOLD));
        assert!(cell.attributes.contains(Attributes::ITALIC));
        assert!(cell.attributes.contains(Attributes::UNDERLINE));
        assert!(cell.attributes.contains(Attributes::INVERSE));
        assert!(cell.attributes.contains(Attributes::CONCEALED));
        assert!(cell.attributes.contains(Attributes::STRIKETHROUGH));
        let (foreground, background) = cell_colors(cell);
        assert_eq!(
            foreground, background,
            "conceal uses the effective background"
        );
        assert_eq!(background, Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn default_cells_share_the_grid_background_without_individual_paints() {
        let default = RenderedCell {
            text: String::new(),
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            attributes: Attributes::NONE,
            hyperlink: None,
        };
        let colored = RenderedCell {
            background: Color::Indexed(4),
            ..default.clone()
        };
        let inverse = RenderedCell {
            attributes: Attributes::INVERSE,
            ..default.clone()
        };

        assert!(!cell_needs_background_paint(&default, false));
        assert!(cell_needs_background_paint(&default, true));
        assert!(cell_needs_background_paint(&colored, false));
        assert!(cell_needs_background_paint(&inverse, false));
    }

    #[test]
    fn sustained_output_keeps_cache_input_and_resize_paths_usable() {
        let started = Instant::now();
        let mut terminal = terminal(120, 40);
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );

        for _ in 0..1_000 {
            terminal.ingest(
                b"representative output line exercises terminal scrolling and dirty rows\r\n",
            );
            let dirty_rows = terminal.take_dirty_rows();
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        }

        let mut sink = Sink::default();
        assert_eq!(
            route_input(&mut terminal, InputEvent::Key(Key::ArrowDown), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        let mut resize = ResizeTracker::default();
        assert_eq!(
            resize.apply(&mut terminal, Dimensions::new(100, 30).unwrap()),
            ResizeOutcome::Resized(Dimensions::new(100, 30).unwrap())
        );
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(cache.dimensions(), Some(Dimensions::new(100, 30).unwrap()));
        assert_eq!(sink.0, vec![b"\x1b[B".to_vec()]);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "representative output path became unexpectedly slow"
        );
    }

    #[test]
    fn repeated_resize_refreshes_cached_banner_and_prompt_cells() {
        let mut terminal = terminal(12, 4);
        terminal.ingest(b"Windows cmd\r\nCopyright\r\nC:\\Users\\fes>");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );
        let mut resize = ResizeTracker::default();

        for dimensions in [
            Dimensions::new(11, 4).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 3).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 4).unwrap(),
        ] {
            assert_eq!(
                resize.apply(&mut terminal, dimensions),
                ResizeOutcome::Resized(dimensions)
            );
            let dirty_rows = terminal.take_dirty_rows();
            let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

            assert!(update.full_refresh);
            assert_eq!(cache.row(0).unwrap()[0].text(), "W");
            assert_eq!(cache.row(1).unwrap()[0].text(), "C");
            assert_eq!(cache.row(2).unwrap()[0].text(), "C");
        }
    }
}
