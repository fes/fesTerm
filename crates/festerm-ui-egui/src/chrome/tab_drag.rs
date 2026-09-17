//! Cross-window tab drag: publishing window footprints and resolving where a
//! released chip was dropped (ADR 0033).
//!
//! Every desktop platform gives the window where a mouse button went down
//! exclusive pointer capture until it comes back up, so a window a chip is
//! dragged *onto* observes nothing at all: no motion, no hover, no release.
//! The drop therefore has to be resolved by the drag source, against geometry
//! the other windows published earlier, in the one coordinate space both
//! windows share - the screen.
//!
//! Each window records its own footprint while it paints. The application
//! composition root then promotes those recordings into the registry this
//! module resolves against, which is what keeps a closed window's stale
//! footprint from catching a later drop.

use egui::{Context, Id, Pos2, Rect, ViewportId};

use super::ChipId;

/// One window's screen-space footprint for the current pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowFootprint {
    /// The whole window, or `None` when the platform refuses to report a
    /// window's own screen position (Wayland).
    window: Option<Rect>,
    /// The chip row band, in screen coordinates.
    row: Option<Rect>,
    /// Each chip's footprint, in screen coordinates and row order.
    chips: Vec<(ChipId, Rect)>,
}

impl WindowFootprint {
    /// Builds a footprint directly, for tests that stand in for a window that
    /// is not really open.
    #[cfg(test)]
    pub fn for_test(window: Rect, row: Rect, chips: &[(ChipId, Rect)]) -> Self {
        Self {
            window: Some(window),
            row: Some(row),
            chips: chips.to_vec(),
        }
    }
}

/// The published footprints of every currently open window, in window order.
#[derive(Clone, Debug, Default)]
pub struct WindowFootprints {
    windows: Vec<(ViewportId, WindowFootprint)>,
}

impl WindowFootprints {
    /// Records one window's footprint, replacing any previous entry for it.
    pub fn insert(&mut self, viewport: ViewportId, footprint: WindowFootprint) {
        self.windows.retain(|(id, _)| *id != viewport);
        self.windows.push((viewport, footprint));
    }

    fn get(&self, viewport: ViewportId) -> Option<&WindowFootprint> {
        self.windows
            .iter()
            .find(|(id, _)| *id == viewport)
            .map(|(_, footprint)| footprint)
    }
}

/// Where a released chip landed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TabDrop {
    /// Into another open window, before one of its chips (or appended).
    Window {
        viewport: ViewportId,
        before: Option<ChipId>,
    },
    /// Outside every open window: detach into a window of its own.
    Detached,
}

fn recording_id(viewport: ViewportId) -> Id {
    Id::new(("festerm-window-footprint", viewport))
}

fn published_id() -> Id {
    Id::new("festerm-published-window-footprints")
}

/// Converts a position in this viewport's local coordinates to screen
/// coordinates, or `None` when this platform will not say where the window is.
pub(super) fn to_screen(context: &Context, position: Pos2) -> Option<Pos2> {
    let origin = context.input(|input| input.viewport().inner_rect)?.min;
    Some(position + origin.to_vec2())
}

fn rect_to_screen(context: &Context, rect: Rect) -> Option<Rect> {
    Some(Rect::from_min_max(
        to_screen(context, rect.min)?,
        to_screen(context, rect.max)?,
    ))
}

/// Records this window's chip row and chip footprints for the current pass.
///
/// Called by the chip row with its own local coordinates; conversion to screen
/// space happens here, and a platform that reports no window position records
/// a footprint with no geometry rather than a guess.
pub(super) fn record_footprint(context: &Context, row: Rect, chips: &[(ChipId, Rect)]) {
    let footprint = WindowFootprint {
        window: context.input(|input| input.viewport().inner_rect),
        row: rect_to_screen(context, row),
        chips: chips
            .iter()
            .filter_map(|(id, rect)| Some((*id, rect_to_screen(context, *rect)?)))
            .collect(),
    };
    let id = recording_id(context.viewport_id());
    context.data_mut(|data| data.insert_temp(id, footprint));
}

/// Returns the footprint a window recorded during its own pass, if it has
/// painted its chip row yet.
pub fn recorded_footprint(context: &Context, viewport: ViewportId) -> Option<WindowFootprint> {
    context.data_mut(|data| data.get_temp::<WindowFootprint>(recording_id(viewport)))
}

/// Publishes the footprints a drop may resolve against.
///
/// The composition root calls this with exactly the windows it still owns, so
/// a window that has closed stops being a drop target immediately.
pub fn publish_footprints(context: &Context, footprints: WindowFootprints) {
    context.data_mut(|data| data.insert_temp(published_id(), footprints));
}

fn published(context: &Context) -> Option<WindowFootprints> {
    context.data_mut(|data| data.get_temp::<WindowFootprints>(published_id()))
}

/// Resolves a chip released at `pointer` (this viewport's local coordinates)
/// into a cross-window drop, or `None` when the gesture belongs to this
/// window and its ordinary reorder handling.
///
/// Returns `None` - keeping the gesture an in-window reorder - when the
/// release is anywhere over this window, and also when this platform reports
/// no window geometry, because without it no screen position can be trusted.
pub(super) fn resolve_drop(context: &Context, pointer: Pos2) -> Option<TabDrop> {
    let footprints = published(context)?;
    let source = context.viewport_id();
    let pointer = to_screen(context, pointer)?;

    // A release over the source window is that window's own business: an
    // in-window reorder over its chip row, and otherwise nothing at all.
    // Checked first so an overlapping sibling cannot steal it.
    if footprints
        .get(source)
        .and_then(|footprint| footprint.window)
        .is_some_and(|window| window.contains(pointer))
    {
        return None;
    }

    for (viewport, footprint) in &footprints.windows {
        if *viewport == source {
            continue;
        }
        if footprint.row.is_some_and(|row| row.contains(pointer)) {
            let before = footprint
                .chips
                .iter()
                .find(|(_, rect)| rect.contains(pointer))
                .map(|(id, _)| *id);
            return Some(TabDrop::Window {
                viewport: *viewport,
                before,
            });
        }
        if footprint.window.is_some_and(|rect| rect.contains(pointer)) {
            return Some(TabDrop::Window {
                viewport: *viewport,
                before: None,
            });
        }
    }

    // Outside every window fesTerm knows about. Requires that this window's
    // own geometry was published, which the source check above established.
    footprints
        .get(source)
        .and_then(|footprint| footprint.window)
        .map(|_| TabDrop::Detached)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footprint(window: Rect, row: Rect, chips: &[(ChipId, Rect)]) -> WindowFootprint {
        WindowFootprint {
            window: Some(window),
            row: Some(row),
            chips: chips.to_vec(),
        }
    }

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect::from_min_size(Pos2::new(x, y), egui::vec2(width, height))
    }

    /// Resolution reads this viewport's own screen position from its
    /// `ViewportInfo`, which only exists inside a real pass, so these tests
    /// drive one with the geometry a platform would report.
    fn run_with_window<R>(context: &Context, window: Rect, body: impl FnOnce(&Context) -> R) -> R {
        let mut input = egui::RawInput::default();
        let info = egui::ViewportInfo {
            inner_rect: Some(window),
            ..Default::default()
        };
        input.viewports.insert(context.viewport_id(), info);
        let mut body = Some(body);
        let mut result = None;
        let mut output = context.run_ui(input, |ui| {
            if let Some(body) = body.take() {
                result = Some(body(&ui.ctx().clone()));
            }
        });
        // `TexturesDelta` panics if dropped with unapplied deltas, and these
        // tests have no painter to apply them.
        output.textures_delta.clear();
        result.expect("the pass body runs exactly once")
    }

    /// The source window's own geometry has to be published for a drop to
    /// resolve at all, so these tests build both sides the way the
    /// composition root does.
    fn two_windows(context: &Context) -> ViewportId {
        let source = context.viewport_id();
        let other = ViewportId::from_hash_of("other-window");
        let mut footprints = WindowFootprints::default();
        footprints.insert(
            source,
            footprint(
                SOURCE_WINDOW,
                rect(0.0, 0.0, 400.0, 40.0),
                &[(ChipId(1), rect(0.0, 0.0, 100.0, 40.0))],
            ),
        );
        footprints.insert(
            other,
            footprint(
                rect(500.0, 0.0, 400.0, 300.0),
                rect(500.0, 0.0, 400.0, 40.0),
                &[
                    (ChipId(7), rect(500.0, 0.0, 100.0, 40.0)),
                    (ChipId(8), rect(600.0, 0.0, 100.0, 40.0)),
                ],
            ),
        );
        publish_footprints(context, footprints);
        other
    }

    const SOURCE_WINDOW: Rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(400.0, 300.0));

    #[test]
    fn a_release_over_another_windows_chip_names_that_window_and_chip() {
        let context = Context::default();
        let other = two_windows(&context);

        let drop = run_with_window(&context, SOURCE_WINDOW, |context| {
            resolve_drop(context, Pos2::new(650.0, 20.0))
        });

        assert_eq!(
            drop,
            Some(TabDrop::Window {
                viewport: other,
                before: Some(ChipId(8)),
            })
        );
    }

    #[test]
    fn a_release_over_another_windows_body_appends_to_that_window() {
        let context = Context::default();
        let other = two_windows(&context);

        let drop = run_with_window(&context, SOURCE_WINDOW, |context| {
            resolve_drop(context, Pos2::new(650.0, 200.0))
        });

        assert_eq!(
            drop,
            Some(TabDrop::Window {
                viewport: other,
                before: None,
            })
        );
    }

    #[test]
    fn a_release_over_the_source_window_is_left_to_its_own_reorder() {
        let context = Context::default();
        two_windows(&context);

        let (over_row, over_body) = run_with_window(&context, SOURCE_WINDOW, |context| {
            (
                resolve_drop(context, Pos2::new(120.0, 20.0)),
                resolve_drop(context, Pos2::new(120.0, 200.0)),
            )
        });

        assert_eq!(over_row, None);
        assert_eq!(over_body, None);
    }

    #[test]
    fn a_release_outside_every_window_detaches() {
        let context = Context::default();
        two_windows(&context);

        let drop = run_with_window(&context, SOURCE_WINDOW, |context| {
            resolve_drop(context, Pos2::new(450.0, 500.0))
        });

        assert_eq!(drop, Some(TabDrop::Detached));
    }

    /// Wayland deliberately refuses to tell a client where its own window is
    /// (ADR 0033). Without that, no release position can be mapped onto
    /// another window, and guessing would drop the tab into the wrong one.
    #[test]
    fn a_platform_that_reports_no_window_geometry_resolves_no_cross_window_drop() {
        let context = Context::default();
        let mut footprints = WindowFootprints::default();
        footprints.insert(context.viewport_id(), WindowFootprint::default());
        footprints.insert(
            ViewportId::from_hash_of("other-window"),
            WindowFootprint::default(),
        );
        publish_footprints(&context, footprints);

        // No `inner_rect`: exactly what Wayland reports.
        let mut input = egui::RawInput::default();
        input
            .viewports
            .insert(context.viewport_id(), egui::ViewportInfo::default());
        let mut drop = None;
        let mut output = context.run_ui(input, |ui| {
            drop = Some(resolve_drop(&ui.ctx().clone(), Pos2::new(650.0, 20.0)));
        });
        output.textures_delta.clear();

        assert_eq!(drop, Some(None));
    }
}
