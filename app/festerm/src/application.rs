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
            }],
            next_window_id: 1,
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
            let builder = egui::ViewportBuilder::default()
                .with_title(window.id.title())
                .with_icon(crate::application_icon_data())
                .with_inner_size([crate::DEFAULT_WINDOW_WIDTH, crate::DEFAULT_WINDOW_HEIGHT])
                .with_min_inner_size([360.0, 240.0]);
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
        self.open_requested_windows(context);
        self.close_finished_windows(context);
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
            self.open_window(context);
        }
    }

    fn open_window(&mut self, context: &egui::Context) {
        // The new window is only rendered by the *next* pass, so ask for one.
        // Without this, opening a window from an otherwise idle application
        // leaves egui with no reason to repaint and the window never appears.
        context.request_repaint();
        let (configuration, status, reloader, secret_store) =
            self.windows[0].app.shared_application_services();
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        tracing::info!(target: "festerm::app", window = id.0, "opening an additional window");
        self.windows.push(Window {
            id,
            app: FesTermApp::secondary_window(
                context,
                configuration,
                status,
                reloader,
                secret_store,
            ),
        });
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
