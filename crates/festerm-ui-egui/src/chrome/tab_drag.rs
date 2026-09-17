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

/// The id of the borderless window that carries a dragged chip's ghost.
fn ghost_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("festerm-drag-ghost")
}

/// Whether a chip released at `pointer` would leave this window, and so needs
/// a ghost the source window's own painter cannot draw.
///
/// A window's painter is clipped to its own surface, so a chip dragged past
/// the window edge simply vanishes. When that happens the ghost is carried by
/// a separate borderless window instead (ADR 0033).
pub(super) fn ghost_escapes_window(context: &Context, pointer: Pos2) -> Option<Pos2> {
    let pointer = to_screen(context, pointer)?;
    let window = published(context)?
        .get(context.viewport_id())
        .and_then(|footprint| footprint.window)?;
    (!window.contains(pointer)).then_some(pointer)
}

/// A chip ghost one window has asked the composition root to carry.
#[derive(Clone, Debug, PartialEq)]
pub struct DragGhost {
    primary: String,
    secondary: Option<String>,
    /// The pointer position the ghost is centred on, in screen coordinates.
    position: Pos2,
}

fn ghost_request_id() -> Id {
    Id::new("festerm-drag-ghost-request")
}

/// Records that this window's drag has left it and needs a carried ghost.
///
/// The ghost is only *requested* here: showing it is the composition root's
/// job, because a viewport opened from inside another window's pass is
/// nested under that window, and its position commands end up moving the
/// parent window instead of the ghost.
pub(super) fn request_drag_ghost(context: &Context, chip: &super::ChipViewModel, position: Pos2) {
    let ghost = DragGhost {
        primary: chip.primary.clone(),
        secondary: chip.secondary.clone(),
        position,
    };
    context.data_mut(|data| data.insert_temp(ghost_request_id(), ghost));
}

/// Shows the carried ghost any window asked for this pass, and clears the
/// request so the ghost disappears as soon as the drag ends or comes home.
///
/// Called by the composition root from the root viewport's own pass, so the
/// ghost window is a sibling of the real windows rather than a child of one.
pub fn show_requested_drag_ghost(context: &Context) {
    let Some(ghost) = context.data_mut(|data| {
        let ghost = data.get_temp::<DragGhost>(ghost_request_id());
        data.remove::<DragGhost>(ghost_request_id());
        ghost
    }) else {
        return;
    };
    let size = egui::vec2(GHOST_WIDTH, GHOST_HEIGHT);
    let builder = egui::ViewportBuilder::default()
        .with_title("fesTerm tab")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_mouse_passthrough(true)
        .with_resizable(false)
        .with_taskbar(false)
        .with_inner_size(size)
        .with_position(ghost.position - size / 2.0);
    context.show_viewport_immediate(ghost_viewport_id(), builder, move |ui, _class| {
        paint_ghost(ui, &ghost);
    });
}

fn paint_ghost(ui: &egui::Ui, ghost: &DragGhost) {
    let rect =
        Rect::from_min_size(Pos2::ZERO, egui::vec2(GHOST_WIDTH, GHOST_HEIGHT)).shrink(GHOST_MARGIN);
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, super::CHIP_ACTIVE_FILL);
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, super::CHIP_ACTIVE_OUTLINE),
        egui::StrokeKind::Inside,
    );
    let text_left = rect.left() + 10.0;
    painter.text(
        egui::pos2(
            text_left,
            rect.center().y - if ghost.secondary.is_some() { 7.0 } else { 0.0 },
        ),
        egui::Align2::LEFT_CENTER,
        &ghost.primary,
        egui::FontId::proportional(13.0),
        super::CHIP_PRIMARY_TEXT,
    );
    if let Some(secondary) = &ghost.secondary {
        painter.text(
            egui::pos2(text_left, rect.center().y + 8.0),
            egui::Align2::LEFT_CENTER,
            secondary,
            egui::FontId::proportional(10.0),
            super::CHIP_SECONDARY_TEXT,
        );
    }
}

const GHOST_WIDTH: f32 = 180.0;
const GHOST_HEIGHT: f32 = 44.0;
const GHOST_MARGIN: f32 = 2.0;

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
