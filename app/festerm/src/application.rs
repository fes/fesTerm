//! The Application half of ADR 0014's `Application -> Window -> Workspace
//! view -> Tabs -> Session` hierarchy, implementing multi-window support per
//! ADR 0032.
//!
//! `FesTermApp` is one *window*. This module owns the ordered list of them,
//! renders each additional window as an egui viewport inside the single
//! process, and keeps every window's configuration coherent by broadcasting
//! each committed write to its siblings.

use eframe::egui;

use crate::app::FesTermApp;
use crate::tabs::{Tab, TabMoveRequest};

/// The stable identity of one open window, used for its egui `ViewportId` and
/// its title. Monotonic and never reused, so closing window 2 and opening
/// another does not produce two windows egui believes are the same viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub(crate) struct WindowId(u64);

impl WindowId {
    const PRIMARY: Self = Self(0);

    fn viewport_id(self) -> egui::ViewportId {
        egui::ViewportId::from_hash_of(("festerm-window", self.0))
    }

    fn title(self) -> String {
        if self == Self::PRIMARY {
            crate::APPLICATION_TITLE.to_owned()
        } else {
            format!(
                "{} \u{2014} Window {}",
                crate::APPLICATION_TITLE,
                self.0 + 1
            )
        }
    }
}

struct Window {
    id: WindowId,
    app: FesTermApp,
    /// Where this window should open, when it was detached under the pointer
    /// or restored from a saved workspace (ADR 0033). egui only emits a
    /// position command when this value *changes*, so keeping it set does not
    /// fight the user dragging the window afterwards.
    placement: Option<WindowPlacement>,
}

/// A window's requested screen position and size, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowPlacement {
    position: egui::Pos2,
    size: egui::Vec2,
}

/// Owns every fesTerm window in this process and the cross-window policy
/// between them (ADR 0014: "Application ... owns cross-window policy").
pub(crate) struct FesTermApplication {
    /// Always non-empty, and `windows[0]` is always the primary window that
    /// renders into `ViewportId::ROOT`.
    windows: Vec<Window>,
    next_window_id: u64,
}

impl FesTermApplication {
    pub(crate) fn new(primary: FesTermApp) -> Self {
        Self {
            windows: vec![Window {
                id: WindowId::PRIMARY,
                app: primary,
                placement: None,
            }],
            next_window_id: 1,
        }
    }

    /// Reopens the additional windows a restored workspace recorded, each with
    /// its own tabs and saved position (ADR 0033).
    ///
    /// Window creation is Application-scoped, so this cannot happen while the
    /// primary window is being built; it runs once, immediately afterwards.
    pub(crate) fn restore_windows(&mut self, context: &egui::Context) {
        for restored in self.windows[0].app.take_restored_windows() {
            let index = self.open_window(context, None);
            let placement = restored.geometry().map(|geometry| {
                let (x, y) = geometry.position();
                let (width, height) = geometry.size();
                WindowPlacement {
                    position: egui::pos2(x, y),
                    size: egui::vec2(width, height),
                }
            });
            self.windows[index].placement = placement;
            self.windows[index]
                .app
                .restore_window_tabs(context, &restored);
        }
    }

    pub(crate) fn primary_mut(&mut self) -> &mut FesTermApp {
        &mut self.windows[0].app
    }

    #[cfg(test)]
    pub(crate) fn window_count(&self) -> usize {
        self.windows.len()
    }

    #[cfg(test)]
    pub(crate) fn window_mut(&mut self, index: usize) -> &mut FesTermApp {
        &mut self.windows[index].app
    }

    /// Renders every additional window for this pass.
    ///
    /// Immediate rather than deferred viewports: a deferred callback must be
    /// `Fn + Send + Sync + 'static` and so cannot borrow a window's live
    /// PTYs, SSH transports, and texture handles mutably (ADR 0032).
    fn show_secondary_windows(&mut self, context: &egui::Context) {
        for window in self.windows.iter_mut().skip(1) {
            let size = window.placement.map_or(
                egui::vec2(crate::DEFAULT_WINDOW_WIDTH, crate::DEFAULT_WINDOW_HEIGHT),
                |placement| placement.size,
            );
            let mut builder = egui::ViewportBuilder::default()
                .with_title(window.id.title())
                .with_icon(crate::application_icon_data())
                .with_inner_size(size)
                .with_min_inner_size([360.0, 240.0]);
            if let Some(placement) = window.placement {
                builder = builder.with_position(placement.position);
            }
            let app = &mut window.app;
            // The `Ui` egui hands back is the child viewport's root, already
            // free of margin and background - the same contract
            // `eframe::App::ui` gives the primary window - so the window's
            // content goes straight into it.
            context.show_viewport_immediate(window.id.viewport_id(), builder, |ui, _class| {
                app.frame_logic(ui.ctx());
                app.ui_content(ui);
            });
        }
    }

    /// Applies each window's post-pass, Application-scoped effects: any
    /// configuration it committed is broadcast to its siblings, any window it
    /// asked for is opened, and any window whose close was accepted is
    /// dropped.
    ///
    /// Runs after every window has finished its pass, so nothing here has to
    /// mutate a window that is still borrowed by a viewport callback.
    fn settle_windows(&mut self, context: &egui::Context) {
        self.broadcast_committed_configuration();
        self.move_requested_tabs(context);
        self.open_requested_windows(context);
        self.close_finished_windows(context);
        self.save_workspace_if_requested();
        self.publish_window_footprints(context);
    }

    /// Publishes every open window's screen footprint, so the *next* pass can
    /// resolve a chip released over one of them (ADR 0033).
    ///
    /// Built from the Application's own window list rather than accumulated
    /// by the windows themselves, so a window that has just closed stops
    /// being a drop target immediately instead of catching one more release.
    fn publish_window_footprints(&self, context: &egui::Context) {
        let mut footprints = festerm_ui_egui::chrome::tab_drag::WindowFootprints::default();
        for window in &self.windows {
            let viewport = window.id.viewport_id();
            if let Some(footprint) =
                festerm_ui_egui::chrome::tab_drag::recorded_footprint(context, viewport)
            {
                footprints.insert(viewport, footprint);
            }
        }
        festerm_ui_egui::chrome::tab_drag::publish_footprints(context, footprints);
    }

    /// Performs the tab moves windows asked for during this pass (ADR 0033).
    ///
    /// Requests are collected before any of them is applied, because applying
    /// one can close a window and shift every index after it.
    fn move_requested_tabs(&mut self, context: &egui::Context) {
        let requests: Vec<(WindowId, TabMoveRequest)> = self
            .windows
            .iter_mut()
            .filter_map(|window| {
                window
                    .app
                    .take_tab_move_request()
                    .map(|request| (window.id, request))
            })
            .collect();
        for (source, request) in requests {
            self.move_tab(source, request, context);
        }
    }

    fn window_index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|window| window.id == id)
    }

    fn move_tab(&mut self, source: WindowId, request: TabMoveRequest, context: &egui::Context) {
        let Some(source_index) = self.window_index(source) else {
            return;
        };
        let target = request.target.map(|viewport| {
            self.windows
                .iter()
                .find(|window| window.id.viewport_id() == viewport)
                .map(|window| window.id)
        });

        match target {
            // The destination closed between the drop being resolved and this
            // pass. Leaving the tab where it is costs the user a drag; a
            // detach here would open a window they did not ask for.
            Some(None) => {}
            Some(Some(target)) => {
                // A window cannot receive its own tab: that release was an
                // in-window reorder, already settled while the pointer moved.
                if target == source {
                    return;
                }
                let Some(tab) = self.windows[source_index].app.detach_tab(request.moved) else {
                    return;
                };
                let Some(target_index) = self.window_index(target) else {
                    return;
                };
                self.windows[target_index]
                    .app
                    .adopt_tab(tab, request.before);
                context.send_viewport_cmd_to(
                    self.windows[target_index].id.viewport_id(),
                    egui::ViewportCommand::Focus,
                );
                self.collapse_if_emptied(source, context);
            }
            None => self.detach_tab_into_new_window(source_index, request, context),
        }
    }

    /// Opens a window owning just the dragged tab, under the pointer that
    /// released it (ADR 0033).
    fn detach_tab_into_new_window(
        &mut self,
        source_index: usize,
        request: TabMoveRequest,
        context: &egui::Context,
    ) {
        let source = self.windows[source_index].id;
        // Detaching a secondary window's only tab would produce the same
        // single tab in a different window, minus the position the user chose.
        if source != WindowId::PRIMARY && self.windows[source_index].app.tab_count() <= 1 {
            return;
        }
        let Some(tab) = self.windows[source_index].app.detach_tab(request.moved) else {
            return;
        };
        let size = self.windows[source_index]
            .app
            .window_size()
            .unwrap_or(egui::vec2(
                crate::DEFAULT_WINDOW_WIDTH,
                crate::DEFAULT_WINDOW_HEIGHT,
            ));
        // Offset so the new window's own chip row lands under the pointer
        // rather than starting at it.
        let position = request.screen_position - egui::vec2(DETACH_POINTER_INSET, 0.0);
        let index = self.open_window(context, Some(tab));
        self.windows[index].placement = Some(WindowPlacement { position, size });
        self.collapse_if_emptied(source, context);
    }

    /// Settles a window that has just given up its last tab (ADR 0033): a
    /// secondary window closes, and the primary window - which owns the menu
    /// bar, the quit path, and the root viewport - returns to the Launcher.
    fn collapse_if_emptied(&mut self, id: WindowId, context: &egui::Context) {
        let Some(index) = self.window_index(id) else {
            return;
        };
        if !self.windows[index].app.has_no_tabs() {
            return;
        }
        if id == WindowId::PRIMARY {
            self.windows[index].app.open_launcher_if_empty(context);
        } else {
            self.windows.remove(index);
            context.request_repaint();
        }
    }

    /// Saves one workspace covering every window, when any window's tabs
    /// changed and workspace restore is enabled (ADR 0033).
    ///
    /// The primary window performs the single write, through the same choke
    /// point as every other configuration write, so a failure is reported and
    /// committed exactly as before (ADR 0015).
    fn save_workspace_if_requested(&mut self) {
        let requested = self
            .windows
            .iter_mut()
            .map(|window| usize::from(window.app.take_workspace_save_request()))
            .sum::<usize>();
        if requested == 0 || !self.windows[0].app.restores_workspace() {
            return;
        }
        // Tab identifiers must be unique across the whole workspace, so one
        // counter runs through every window in order.
        let mut next_identifier = 1;
        let mut additional = Vec::new();
        for window in self.windows.iter().skip(1) {
            if let Some(captured) = window.app.capture_additional_window(&mut next_identifier) {
                additional.push(captured);
            }
        }
        self.windows[0]
            .app
            .save_workspace(additional, &mut next_identifier);
    }

    /// Hands a configuration one window has just committed to disk to every
    /// other window (ADR 0032).
    ///
    /// This is an in-process broadcast of fesTerm's own write, not a
    /// configuration reload: nothing is re-read from disk, and a third
    /// party's edit still cannot reach a running window. ADR 0015's
    /// no-file-watching decision is therefore untouched.
    fn broadcast_committed_configuration(&mut self) {
        for index in 0..self.windows.len() {
            let Some(configuration) = self.windows[index].app.take_configuration_broadcast() else {
                continue;
            };
            for (sibling, window) in self.windows.iter_mut().enumerate() {
                if sibling != index {
                    window
                        .app
                        .adopt_broadcast_configuration(configuration.clone());
                }
            }
        }
    }

    fn open_requested_windows(&mut self, context: &egui::Context) {
        let requested = self
            .windows
            .iter_mut()
            .map(|window| usize::from(window.app.take_window_open_request()))
            .sum::<usize>();
        for _ in 0..requested {
            self.open_window(context, None);
        }
    }

    /// Creates one additional window, optionally owning a tab dragged out of
    /// another one, and returns its index.
    fn open_window(&mut self, context: &egui::Context, detached: Option<Tab>) -> usize {
        // The new window is only rendered by the *next* pass, so ask for one.
        // Without this, opening a window from an otherwise idle application
        // leaves egui with no reason to repaint and the window never appears.
        context.request_repaint();
        let (configuration, status, reloader, secret_store) =
            self.windows[0].app.shared_application_services();
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        tracing::info!(target: "festerm::app", window = id.0, "opening an additional window");
        let mut app =
            FesTermApp::secondary_window(context, configuration, status, reloader, secret_store);
        if let Some(tab) = detached {
            app.adopt_detached_tab(tab);
        }
        self.windows.push(Window {
            id,
            app,
            placement: None,
        });
        self.windows.len() - 1
    }

    /// Drops secondary windows whose close request has been accepted.
    ///
    /// The primary window is never removed here: its close is the application
    /// quit path, which `eframe` owns, and removing it would leave the root
    /// viewport with nothing to render.
    fn close_finished_windows(&mut self, context: &egui::Context) {
        let before = self.windows.len();
        self.windows
            .retain(|window| window.id == WindowId::PRIMARY || !window.app.window_close_accepted());
        if self.windows.len() != before {
            context.request_repaint();
        }
    }
}

/// How far left of the pointer a detached window's origin is placed, so the
/// tab the user is still holding lands on the new window's chip row rather
/// than at its very corner.
const DETACH_POINTER_INSET: f32 = 60.0;

impl eframe::App for FesTermApplication {
    fn logic(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        eframe::App::logic(self.primary_mut(), context, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(self.primary_mut(), ui, frame);
        let context = ui.ctx().clone();
        self.show_secondary_windows(&context);
        self.settle_windows(&context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::AppCommand;
    use festerm_config::{Configuration, InterfaceSettings, Profile};

    fn application() -> (FesTermApplication, egui::Context) {
        let context = egui::Context::default();
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        (FesTermApplication::new(app), context)
    }

    /// Window creation is Application-scoped, so a window may only *request*
    /// one; nothing happens until the composition root settles the pass.
    #[test]
    fn opening_a_window_adds_one_window_to_the_application() {
        let (mut application, context) = application();
        assert_eq!(application.window_count(), 1);

        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 2);
    }

    #[test]
    fn a_new_window_starts_on_the_launcher_rather_than_cloning_the_originating_tabs() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        let opened = application.window_mut(1);
        assert_eq!(opened.tab_count_for_test(), 1);
        assert!(opened.active_tab_is_launcher_for_test());
    }

    /// Issue #119's central requirement: a setting changed in one window
    /// applies to every window, not only to the one that made the change.
    #[test]
    fn an_interface_setting_committed_in_one_window_reaches_every_other_window() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        assert!(!application.window_mut(1).compact_launcher_grid_for_test());

        application
            .window_mut(0)
            .broadcast_for_test(configuration_with_compact_launcher_grid());
        application.settle_windows(&context);

        assert!(application.window_mut(1).compact_launcher_grid_for_test());
    }

    #[test]
    fn a_profile_saved_in_one_window_is_visible_in_every_other_windows_launcher() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        assert_eq!(application.window_mut(1).profile_count_for_test(), 0);

        let configuration =
            Configuration::new(vec![Profile::local("shell", "/bin/sh", Vec::new(), None)
                .expect("a local profile with an executable is valid")])
            .expect("a single local profile is a valid configuration");
        application.window_mut(0).broadcast_for_test(configuration);
        application.settle_windows(&context);

        assert_eq!(application.window_mut(1).profile_count_for_test(), 1);
    }

    /// Propagation must not disturb window-scoped state, so a sibling's save
    /// leaves this window's tabs and active-tab cursor exactly as they were.
    #[test]
    fn adopting_a_siblings_configuration_leaves_this_windows_tabs_untouched() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        let tabs_before = application.window_mut(1).tab_count_for_test();
        let active_before = application.window_mut(1).active_tab_id_for_test();

        application
            .window_mut(0)
            .broadcast_for_test(configuration_with_compact_launcher_grid());
        application.settle_windows(&context);

        assert_eq!(application.window_mut(1).tab_count_for_test(), tabs_before);
        assert_eq!(
            application.window_mut(1).active_tab_id_for_test(),
            active_before
        );
    }

    #[test]
    fn a_closed_secondary_window_is_dropped_and_the_primary_window_is_not() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        application.window_mut(1).accept_window_close_for_test();
        application.window_mut(0).accept_window_close_for_test();
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 1);
    }

    /// A window must not re-broadcast a document it merely adopted, or two
    /// windows would ping-pong the same write for as long as the app runs.
    #[test]
    fn adopting_a_broadcast_does_not_re_broadcast_it() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        application
            .window_mut(0)
            .broadcast_for_test(configuration_with_compact_launcher_grid());
        application.settle_windows(&context);

        // Both windows, not just the receiving one: an adopt that re-queued
        // the document would leave the *originating* window holding it again
        // by the end of the same settle pass.
        assert!(application
            .window_mut(1)
            .take_configuration_broadcast()
            .is_none());
        assert!(application
            .window_mut(0)
            .take_configuration_broadcast()
            .is_none());
    }

    /// The model tests above never render, so none of them would notice the
    /// viewport path failing outright. Drive a real egui pass and prove the
    /// additional window's full chrome/session UI actually runs inside its
    /// own viewport.
    ///
    /// The native window itself is the backend's job: a bare `egui::Context`
    /// installs no immediate-viewport renderer, so egui falls back to
    /// rendering the child in-place. What this test can and does prove is
    /// that the child callback runs once per additional window and paints.
    #[test]
    fn an_additional_window_renders_its_own_content_in_a_real_pass() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        // Two passes: a window installs the terminal font family on its first
        // pass and deliberately paints nothing until egui has rebuilt the
        // atlas, so only the second pass carries real content.
        let mut output = None;
        for _ in 0..2 {
            context.begin_pass(egui::RawInput::default());
            application.show_secondary_windows(&context);
            // Only the additional window drew this pass; the primary window's
            // own content is not rendered here.
            if let Some(mut previous) = output.replace(context.end_pass()) {
                previous.textures_delta.clear();
            }
        }
        let mut output = output.expect("two passes were run");
        let painted = context.tessellate(output.shapes, output.pixels_per_point);
        output.textures_delta.clear();

        assert!(
            painted.iter().any(|clipped| match &clipped.primitive {
                egui::epaint::Primitive::Mesh(mesh) => !mesh.is_empty(),
                egui::epaint::Primitive::Callback(_) => true,
            }),
            "the additional window must paint its own content"
        );
    }

    /// The whole point of ADR 0033: the dragged tab itself - with whatever
    /// session it owns - ends up in the other window, rather than a new tab
    /// being opened there.
    #[test]
    fn a_tab_dropped_on_another_window_moves_into_it() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        let moved = application.window_mut(0).active_tab_id_for_test();
        let target = application.windows[1].id.viewport_id();

        application.window_mut(0).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: Some(target),
                before: None,
                screen_position: egui::pos2(900.0, 40.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert!(
            !application
                .window_mut(0)
                .tab_ids_for_test()
                .contains(&moved),
            "the source window must give the tab up rather than keep a copy"
        );
        assert!(
            application
                .window_mut(1)
                .tab_ids_for_test()
                .contains(&moved),
            "the destination window must own the very tab that was dragged"
        );
    }

    /// A window is a container for tabs, so one that has given up its last
    /// tab has nothing left to show and closes (ADR 0033).
    #[test]
    fn a_secondary_window_that_loses_its_last_tab_closes() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        let moved = application.window_mut(1).active_tab_id_for_test();
        let primary = application.windows[0].id.viewport_id();

        application.window_mut(1).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: Some(primary),
                before: None,
                screen_position: egui::pos2(40.0, 40.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 1);
        assert!(application
            .window_mut(0)
            .tab_ids_for_test()
            .contains(&moved));
    }

    /// The primary window owns the menu bar, the quit path, and the root
    /// viewport, so it cannot close; it falls back to the Launcher exactly as
    /// it does when its last tab is closed.
    #[test]
    fn the_primary_window_falls_back_to_the_launcher_instead_of_closing() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        let moved = application.window_mut(0).active_tab_id_for_test();
        let target = application.windows[1].id.viewport_id();

        application.window_mut(0).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: Some(target),
                before: None,
                screen_position: egui::pos2(900.0, 40.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert_eq!(application.window_mut(0).tab_count_for_test(), 1);
        assert!(application.window_mut(0).active_tab_is_launcher_for_test());
        assert!(
            !application
                .window_mut(0)
                .tab_ids_for_test()
                .contains(&moved),
            "the fallback Launcher must be a new tab, not the one that moved"
        );
    }

    /// Releasing a chip away from every window detaches it into a window of
    /// its own, positioned where it was dropped (ADR 0033).
    #[test]
    fn a_tab_dropped_outside_every_window_detaches_into_a_new_one() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application.window_mut(0).set_window_geometry_for_test(
            festerm_config::WorkspaceWindowGeometry::new(0.0, 0.0, 640.0, 480.0),
        );
        let moved = application.window_mut(0).active_tab_id_for_test();

        application.window_mut(0).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: None,
                before: None,
                screen_position: egui::pos2(720.0, 300.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert_eq!(application.window_mut(1).tab_count_for_test(), 1);
        assert!(
            application
                .window_mut(1)
                .tab_ids_for_test()
                .contains(&moved),
            "the detached window must own the dragged tab, not a fresh Launcher"
        );
        let placement = application.windows[1]
            .placement
            .expect("a detached window opens where it was dropped");
        assert_eq!(placement.position.y, 300.0);
        assert!(placement.position.x < 720.0);
        assert_eq!(
            placement.size,
            egui::vec2(640.0, 480.0),
            "a detached window is sized like the window the tab left"
        );
    }

    /// Detaching the only tab of an additional window would replace that
    /// window with an identical one somewhere else, losing the position the
    /// user chose for it, so the drop is simply refused.
    #[test]
    fn detaching_the_only_tab_of_a_secondary_window_does_nothing() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        let moved = application.window_mut(1).active_tab_id_for_test();

        application.window_mut(1).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: None,
                before: None,
                screen_position: egui::pos2(720.0, 300.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert!(application
            .window_mut(1)
            .tab_ids_for_test()
            .contains(&moved));
    }

    /// A window may only *request* a move, and only for a tab it owns; a
    /// stale or foreign identifier must not disturb anyone's tabs.
    #[test]
    fn a_move_to_a_window_that_no_longer_exists_keeps_the_tab_where_it_is() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        let moved = application.window_mut(0).active_tab_id_for_test();

        application.window_mut(0).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: Some(egui::ViewportId::from_hash_of("a closed window")),
                before: None,
                screen_position: egui::pos2(900.0, 40.0),
            },
            &context,
        );
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 1);
        assert!(application
            .window_mut(0)
            .tab_ids_for_test()
            .contains(&moved));
    }

    /// One workspace covers every window (ADR 0033): the primary window's
    /// tabs stay where a single-window build expects them, the additional
    /// windows are listed separately with their geometry, and tab
    /// identifiers stay unique across the whole file.
    #[test]
    fn a_saved_workspace_covers_every_window() {
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                true,
                true,
                true,
            ))
            .expect("a configuration with workspace restore enabled is valid");
        let context = egui::Context::default();
        let mut application =
            FesTermApplication::new(FesTermApp::for_test_with_configuration(configuration));
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-multi-window-workspace-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("config.toml");
        application.window_mut(0).set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(path.clone()),
        );

        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenProfiles, &context);
        application.window_mut(1).set_window_geometry_for_test(
            festerm_config::WorkspaceWindowGeometry::new(120.0, 64.0, 900.0, 600.0),
        );
        application.window_mut(1).request_workspace_save_for_test();
        application.settle_windows(&context);

        let saved = Configuration::load_from_path(&path).expect("the workspace was written");
        let workspace = saved.workspace().expect("a saved workspace");
        assert_eq!(
            workspace.tabs().len(),
            2,
            "the primary window's tabs stay in the workspace's own tab list"
        );
        let windows = workspace.windows();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].tabs().len(), 2);
        let geometry = windows[0].geometry().expect("the window's saved geometry");
        assert_eq!(geometry.position(), (120.0, 64.0));
        assert_eq!(geometry.size(), (900.0, 600.0));
        let identifiers: std::collections::HashSet<&str> = workspace
            .tabs()
            .iter()
            .chain(windows[0].tabs())
            .map(festerm_config::WorkspaceTab::identifier)
            .collect();
        assert_eq!(
            identifiers.len(),
            4,
            "tab identifiers address tabs workspace-wide, so no two windows may reuse one"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    /// Restoring reopens the additional windows a workspace recorded, each
    /// with its own tabs, rather than piling every tab into one window.
    #[test]
    fn restoring_a_workspace_reopens_its_additional_windows() {
        let context = egui::Context::default();
        let configuration = Configuration::parse(
            r#"
schema_version = 1
workspace_enabled = true

[workspace]

[[workspace.tabs]]
kind = "launcher"
id = "tab-1"

[[workspace.windows]]

[workspace.windows.geometry]
x = 120.0
y = 64.0
width = 900.0
height = 600.0

[[workspace.windows.tabs]]
kind = "settings"
id = "tab-2"

[[workspace.windows.tabs]]
kind = "profiles"
id = "tab-3"
"#,
        )
        .expect("a two-window workspace is valid");
        let mut application = FesTermApplication::new(
            FesTermApp::with_restored_workspace_for_test(&context, configuration),
        );

        application.restore_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert_eq!(application.window_mut(0).tab_count_for_test(), 1);
        assert_eq!(application.window_mut(1).tab_count_for_test(), 2);
        assert_eq!(
            application.windows[1].placement.map(|placement| (
                placement.position.x,
                placement.position.y,
                placement.size.x,
                placement.size.y
            )),
            Some((120.0, 64.0, 900.0, 600.0)),
            "an additional window reopens where and as large as it was saved"
        );
    }

    fn configuration_with_compact_launcher_grid() -> Configuration {
        Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT.with_compact_launcher_grid(true))
            .expect("a compact-launcher-grid preference is a valid configuration")
    }

    /// Keyboard bindings are part of the same document, and issue #119 calls
    /// them out separately because a window showing the bindings editor must
    /// also see a sibling's change.
    #[test]
    fn a_keyboard_binding_changed_in_one_window_applies_in_every_other_window() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        let action = festerm_config::KeyboardAction::CommandPalette;
        let before = application.window_mut(1).keyboard_binding_for_test(action);

        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(action, Some("Ctrl+Shift+F9".to_owned()));
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT.with_keyboard_bindings(bindings))
            .expect("a rebound command palette is a valid configuration");
        application.window_mut(0).broadcast_for_test(configuration);
        application.settle_windows(&context);

        let after = application.window_mut(1).keyboard_binding_for_test(action);
        assert_ne!(before, after);
        assert_eq!(after, "Ctrl+Shift+F9");
    }
}
