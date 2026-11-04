//! The Application half of ADR 0014's `Application -> Window -> Workspace
//! view -> Tabs -> Session` hierarchy, implementing multi-window support per
//! ADR 0032.
//!
//! `FesTermApp` is one *window*. This module owns the ordered list of them,
//! renders each additional window as an egui viewport inside the single
//! process, and keeps every window's configuration coherent by broadcasting
//! each committed write to its siblings.

use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use eframe::egui;

use crate::app::FesTermApp;
use crate::document_activation::{ActivationError, ActivationQueue};
use crate::tabs::{Tab, TabMoveRequest};

/// The stable identity of one open window, used for its egui `ViewportId` and
/// its title. Monotonic and never reused, so closing window 2 and opening
/// another does not produce two windows egui believes are the same viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub(crate) struct WindowId(u64);

impl WindowId {
    pub(crate) const PRIMARY: Self = Self(0);

    #[cfg(test)]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }

    fn viewport_id(self) -> egui::ViewportId {
        // The first window *is* eframe's root viewport; anything else here
        // would leave it unable to publish a drop footprint or receive a
        // viewport command (ADR 0033).
        if self == Self::PRIMARY {
            egui::ViewportId::ROOT
        } else {
            egui::ViewportId::from_hash_of(("festerm-window", self.0))
        }
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

struct ImeAdmission {
    epoch: Arc<festerm_session::AutomationEpoch>,
    composing: HashSet<WindowId>,
    observed_this_pass: bool,
}

impl ImeAdmission {
    fn new() -> Self {
        let epoch = Arc::new(festerm_session::AutomationEpoch::new());
        epoch.transition(true);
        Self {
            epoch,
            composing: HashSet::new(),
            observed_this_pass: false,
        }
    }

    #[allow(deprecated)]
    fn observe(&mut self, window: WindowId, events: &[egui::Event]) {
        for event in events {
            let active = match event {
                egui::Event::Ime(egui::ImeEvent::Preedit { .. }) => true,
                egui::Event::Ime(egui::ImeEvent::Commit(_)) => false,
                egui::Event::Ime(egui::ImeEvent::Disabled) if self.composing.contains(&window) => {
                    false
                }
                _ => continue,
            };
            if !self.observed_this_pass {
                self.epoch.transition(false);
                self.observed_this_pass = true;
            }
            if active {
                self.composing.insert(window);
            } else {
                self.composing.remove(&window);
            }
        }
    }

    fn finish_pass(&mut self) -> bool {
        if !self.observed_this_pass {
            return false;
        }
        self.observed_this_pass = false;
        if self.composing.is_empty() {
            self.epoch.transition(true);
            true
        } else {
            false
        }
    }
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
    last_active_window: WindowId,
    document_activation: ActivationQueue,
    native_documents: Option<festerm_macos_window::OpenDocumentBridge>,
    activation_errors: Arc<Mutex<VecDeque<String>>>,
    automation: crate::automation::Coordinator,
    presentation_workspace: crate::automation::WorkspaceRevision,
    deferred_automation_commands: Vec<crate::automation::NativeWindowCommand>,
    deferred_automation_overflow: bool,
    ime_admission: ImeAdmission,
}

impl FesTermApplication {
    pub(crate) fn new(mut primary: FesTermApp) -> Self {
        primary.set_automation_window(WindowId::PRIMARY);
        primary.initialize_automation_preferences();
        let presentation_workspace = crate::automation::WorkspaceRevision::default();
        primary.set_presentation_workspace(&presentation_workspace);
        let mut automation = crate::automation::Coordinator::default();
        automation
            .bind_presentation_workspace(&presentation_workspace)
            .expect("a new coordinator has no presentation workspace");
        Self {
            windows: vec![Window {
                id: WindowId::PRIMARY,
                app: primary,
                placement: None,
            }],
            next_window_id: 1,
            last_active_window: WindowId::PRIMARY,
            document_activation: ActivationQueue::default(),
            native_documents: None,
            activation_errors: Default::default(),
            automation,
            presentation_workspace,
            deferred_automation_commands: Vec::new(),
            deferred_automation_overflow: false,
            ime_admission: ImeAdmission::new(),
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
            self.windows[index]
                .app
                .set_presentation_workspace(&self.presentation_workspace);
        }
    }

    pub(crate) fn primary_mut(&mut self) -> &mut FesTermApp {
        &mut self.windows[0].app
    }

    pub(crate) fn install_document_activation(
        &mut self,
        queue: ActivationQueue,
        context: &egui::Context,
    ) {
        let context = context.clone();
        queue.set_waker(Arc::new(move || context.request_repaint()));
        self.document_activation = queue;
    }

    fn process_document_activation(&mut self, context: &egui::Context) {
        if self
            .windows
            .iter()
            .any(|window| window.app.document_activation_blocked())
        {
            return;
        }
        let error = self
            .activation_errors
            .lock()
            .expect("activation errors lock healthy")
            .pop_front();
        if let Some(error) = error {
            let index = self.window_index(self.last_active_window).unwrap_or(0);
            self.windows[index]
                .app
                .report_document_activation_error(error);
            self.focus_window(index, context);
            self.flush_native_document_backlog();
            return;
        }
        // One request per pass lets the ordinary document-open error surface
        // retain focus and acknowledgement before the next file is attempted.
        if let Some(path) = self.document_activation.pop_front() {
            let metrics = self.document_activation.metrics();
            tracing::debug!(
                target: "festerm::app",
                queued = metrics.queue_depth,
                high_watermark = metrics.queue_high_watermark,
                "processing document activation"
            );
            self.open_external_document(path, context);
            self.flush_native_document_backlog();
        }
    }

    pub(crate) fn install_native_document_activation(
        &mut self,
        bridge: festerm_macos_window::OpenDocumentBridge,
        context: &egui::Context,
    ) {
        use festerm_macos_window::{OpenDocumentCallbacks, OpenDocumentEnqueueResult};
        if !bridge.is_installed() {
            return;
        }
        let errors = self.activation_errors.clone();
        let enqueue_error = Arc::new(move |message: String| {
            let mut errors = errors.lock().expect("activation errors lock healthy");
            if errors.len() == festerm_macos_window::OPEN_DOCUMENT_ERROR_CAPACITY {
                OpenDocumentEnqueueResult::Backpressured
            } else {
                errors.push_back(message);
                OpenDocumentEnqueueResult::Accepted
            }
        });
        let queue = self.document_activation.clone();
        let path_error = enqueue_error.clone();
        bridge.register_callbacks(OpenDocumentCallbacks::new(
            Arc::new(move |path| match queue.enqueue([path]) {
                Ok(()) => OpenDocumentEnqueueResult::Accepted,
                Err(ActivationError::QueueFull) => OpenDocumentEnqueueResult::Backpressured,
                Err(error) => path_error(error.to_string()),
            }),
            Arc::new(move |error| enqueue_error(error.to_string())),
        ));
        let wake_context = context.clone();
        bridge.register_ui_wake_callback(Arc::new(move || wake_context.request_repaint()));
        self.native_documents = Some(bridge);
        context.request_repaint();
    }

    fn flush_native_document_backlog(&self) {
        if let Some(bridge) = &self.native_documents {
            bridge.flush_pending_to_callbacks();
        }
    }

    fn open_external_document(&mut self, path: PathBuf, context: &egui::Context) {
        let index = self
            .windows
            .iter_mut()
            .position(|window| window.app.has_local_document(&path))
            .or_else(|| self.window_index(self.last_active_window))
            .unwrap_or(0);
        self.windows[index]
            .app
            .open_external_document(path, context);
        self.focus_window(index, context);
    }

    fn focus_window(&mut self, index: usize, context: &egui::Context) {
        let window = &self.windows[index];
        self.last_active_window = window.id;
        context.send_viewport_cmd_to(
            window.id.viewport_id(),
            egui::ViewportCommand::Minimized(false),
        );
        context.send_viewport_cmd_to(window.id.viewport_id(), egui::ViewportCommand::Focus);
        context.request_repaint();
    }

    /// Inert on every production backend until a qualified native monitor and
    /// the enrollment/grant adapters exist. This only queues presentation; the
    /// caller must still await a fresh native barrier and revalidate delivery.
    #[allow(dead_code)]
    fn queue_controller_presentation(
        &mut self,
        channel: u64,
        registration: festerm_automation::ControllerRegistrationId,
        enrollment: festerm_automation::ControllerEnrollmentId,
        timeout: std::time::Duration,
        context: &egui::Context,
    ) -> festerm_automation::Result<festerm_automation::PresentationAttempt> {
        self.queue_controller_presentation_if(
            channel,
            registration,
            enrollment,
            timeout,
            context,
            |session| {
                matches!(
                    session.controller.session(),
                    Some(crate::tabs::ApplicationSession::Local(_))
                )
            },
        )
    }

    fn queue_controller_presentation_if(
        &mut self,
        channel: u64,
        registration: festerm_automation::ControllerRegistrationId,
        enrollment: festerm_automation::ControllerEnrollmentId,
        timeout: std::time::Duration,
        context: &egui::Context,
        eligible: impl FnOnce(&crate::tabs::SessionTab) -> bool,
    ) -> festerm_automation::Result<festerm_automation::PresentationAttempt> {
        use festerm_automation::{Error, PresentationWindowId};

        let target =
            self.automation
                .controller_presentation_target(channel, registration, enrollment)?;
        let tab = crate::tabs::TabId::from_chip_id(target.session());
        let mut matches = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.app.automation_session(tab).is_some());
        let (index, window) = matches.next().ok_or(Error::StaleTarget)?;
        if !eligible(
            window
                .app
                .automation_session(tab)
                .ok_or(Error::StaleTarget)?,
        ) || matches.next().is_some()
        {
            return Err(Error::Unavailable);
        }
        let window = PresentationWindowId::from_host(window.id.0);
        let guard =
            festerm_session::try_automation_admission_lock().map_err(|error| match error {
                festerm_session::AutomationAdmissionTryError::Busy => Error::Busy,
                festerm_session::AutomationAdmissionTryError::Poisoned => Error::Unavailable,
            })?;
        if self.presentation_workspace.current().is_none() {
            self.automation.invalidate_presentation_locked();
            return Err(Error::Unavailable);
        }
        let attempt = self.automation.prepare_presentation_locked(
            channel,
            registration,
            enrollment,
            (target, window),
            timeout,
            &guard,
        )?;
        if !self.windows[index]
            .app
            .activate_presentation_target(tab, context)
        {
            self.automation
                .cancel_presentation_locked(channel, attempt, &guard);
            return Err(Error::StaleTarget);
        }
        if self.presentation_workspace.current().is_none() {
            self.automation
                .cancel_presentation_locked(channel, attempt, &guard);
            self.automation.invalidate_presentation_locked();
            return Err(Error::Unavailable);
        }
        self.focus_window(index, context);
        if let Err(error) = self.automation.presentation_requested_locked(
            channel,
            attempt,
            std::time::Instant::now(),
            &guard,
        ) {
            self.automation
                .cancel_presentation_locked(channel, attempt, &guard);
            return Err(error);
        }
        context.request_repaint_after(
            attempt
                .deadline()
                .saturating_duration_since(std::time::Instant::now()),
        );
        Ok(attempt)
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
            let builder = Self::viewport_builder(window);
            let app = &mut window.app;
            let ime_admission = &mut self.ime_admission;
            // The `Ui` egui hands back is the child viewport's root, already
            // free of margin and background - the same contract
            // `eframe::App::ui` gives the primary window - so the window's
            // content goes straight into it.
            context.show_viewport_immediate(window.id.viewport_id(), builder, |ui, _class| {
                ui.ctx()
                    .input(|input| ime_admission.observe(window.id, &input.events));
                if ui.ctx().input(|input| input.viewport().focused) == Some(true) {
                    self.last_active_window = window.id;
                }
                app.frame_logic(ui.ctx());
                app.ui_content(ui);
            });
        }
    }

    /// Builds one additional window's viewport, with the chrome every fesTerm
    /// window shares plus whatever placement it was detached or restored at.
    fn viewport_builder(window: &Window) -> egui::ViewportBuilder {
        let size = window.placement.map_or(
            egui::vec2(crate::DEFAULT_WINDOW_WIDTH, crate::DEFAULT_WINDOW_HEIGHT),
            |placement| placement.size,
        );
        let mut builder = crate::window_viewport_builder(&window.id.title(), size);
        if let Some(placement) = window.placement {
            builder = builder.with_position(placement.position);
        }
        builder
    }

    /// Applies each window's post-pass, Application-scoped effects: any
    /// configuration it committed is broadcast to its siblings, any window it
    /// asked for is opened, and any window whose close was accepted is
    /// dropped.
    ///
    /// Runs after every window has finished its pass, so nothing here has to
    /// mutate a window that is still borrowed by a viewport callback.
    fn settle_windows(&mut self, context: &egui::Context) {
        // Drain before a move/close can remove the originating window.
        let name_changes: Vec<_> = self
            .windows
            .iter_mut()
            .map(|window| window.app.take_session_name_changes())
            .collect();
        let name_failures: Vec<_> = self
            .windows
            .iter()
            .zip(&name_changes)
            .filter_map(|(window, changes)| {
                changes
                    .validation_failure
                    .map(|failure| (window.id, failure))
            })
            .collect();
        self.broadcast_committed_configuration();
        self.move_requested_tabs(context);
        self.open_requested_windows(context);
        self.close_finished_windows(context);
        self.save_workspace_if_requested(context, name_changes);
        self.broadcast_committed_configuration();
        for (id, failure) in name_failures {
            if let Some(index) = self.window_index(id) {
                self.windows[index]
                    .app
                    .show_session_name_validation_notice(context, failure);
            }
        }
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
                self.automation.controller_target_moving(request.moved);
                self.presentation_workspace.changed();
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
        self.automation.controller_target_moving(request.moved);
        self.presentation_workspace.changed();
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
            self.automation.controller_window_closing(id);
            self.presentation_workspace.changed();
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
    fn save_workspace_if_requested(
        &mut self,
        context: &egui::Context,
        name_changes: Vec<crate::tabs::SessionNameChanges>,
    ) {
        let requested = self
            .windows
            .iter_mut()
            .map(|window| usize::from(window.app.take_workspace_save_request()))
            .sum::<usize>();
        let explicit_names_changed = name_changes.iter().any(|changes| changes.changed);
        let persistence_unavailable = name_changes
            .iter()
            .any(|changes| changes.persistence_unavailable);
        let capacity_exceeded = name_changes.iter().any(|changes| changes.capacity_exceeded);
        let durable_aliases: Vec<_> = name_changes
            .into_iter()
            .flat_map(|changes| changes.durable_aliases)
            .collect();
        let include_workspace = self.windows[0].app.restores_workspace();
        let names_changed = explicit_names_changed
            || (requested != 0 && include_workspace && !durable_aliases.is_empty());
        if !names_changed && (requested == 0 || !include_workspace) {
            return;
        }
        // Tab identifiers must be unique across the whole workspace, so one
        // counter runs through every window in order.
        let mut next_identifier = 1;
        let mut additional = Vec::new();
        for window in self.windows.iter().skip(1).filter(|_| include_workspace) {
            if let Some(captured) = window.app.capture_additional_window(&mut next_identifier) {
                additional.push(captured);
            }
        }
        if names_changed {
            let needs_save = include_workspace || !durable_aliases.is_empty();
            let saved = !capacity_exceeded
                && (!needs_save
                    || self.windows[0].app.save_session_names(
                        durable_aliases,
                        additional,
                        &mut next_identifier,
                        include_workspace,
                    ));
            let committed = self.windows[0].app.shared_application_services().0;
            for window in &mut self.windows {
                window.app.settle_session_name_save(saved, &committed);
            }
            if !saved || persistence_unavailable {
                for window in &mut self.windows {
                    window.app.show_session_name_persistence_notice(
                        context,
                        saved && persistence_unavailable,
                    );
                }
            }
        } else {
            self.windows[0]
                .app
                .save_workspace(additional, &mut next_identifier);
            let (committed, status, ..) = self.windows[0].app.shared_application_services();
            for window in &mut self.windows {
                window
                    .app
                    .settle_session_name_save(status.was_saved(), &committed);
            }
        }
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
        self.presentation_workspace.changed();
        let (configuration, status, reloader, secret_store, documents) =
            self.windows[0].app.shared_application_services();
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        tracing::info!(target: "festerm::app", window = id.0, "opening an additional window");
        let mut app = FesTermApp::secondary_window(
            context,
            configuration,
            status,
            reloader,
            secret_store,
            documents,
        );
        app.set_automation_window(id);
        app.set_presentation_workspace(&self.presentation_workspace);
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
        let closing: Vec<_> = self
            .windows
            .iter()
            .filter(|window| window.id != WindowId::PRIMARY && window.app.window_close_accepted())
            .map(|window| window.id)
            .collect();
        for id in closing {
            self.automation.controller_window_closing(id);
        }
        if self
            .windows
            .iter()
            .any(|window| window.id != WindowId::PRIMARY && window.app.window_close_accepted())
        {
            self.presentation_workspace.changed();
        }
        self.windows
            .retain(|window| window.id == WindowId::PRIMARY || !window.app.window_close_accepted());
        if self.windows.len() != before {
            context.request_repaint();
        }
    }

    fn refresh_application_close_context(&mut self) {
        let counts = self.windows.iter().fold(
            crate::tabs::LiveSessionCounts::default(),
            |mut total, window| {
                let current = window.app.live_session_counts();
                total.local += current.local;
                total.ssh += current.ssh;
                total.serial += current.serial;
                total
            },
        );
        self.windows[0]
            .app
            .set_application_live_session_counts(counts);
    }

    fn collect_automation_commands(
        &mut self,
        commands: &mut Vec<crate::automation::NativeWindowCommand>,
        overflow: &mut bool,
    ) {
        for window in &mut self.windows {
            let (pending, window_overflow) = window.app.take_automation_commands();
            if window_overflow
                || commands.len().saturating_add(pending.len())
                    > crate::automation::MAX_NATIVE_COMMANDS
            {
                if !*overflow {
                    self.automation.close_conflicting_admission();
                }
                *overflow = true;
                commands.clear();
            } else if !*overflow {
                commands.extend(pending.into_iter().map(|command| {
                    crate::automation::NativeWindowCommand {
                        window: window.id,
                        command,
                    }
                }));
            }
        }
        if !*overflow
            && crate::automation::NativeCommand::batch_requires_safety_stop(
                commands.iter().map(|item| &item.command),
            )
        {
            self.automation.close_conflicting_admission();
            *overflow = true;
            commands.clear();
        } else if !*overflow
            && commands.iter().any(|item| {
                matches!(
                    &item.command,
                    crate::automation::NativeCommand::PreferenceChanged { .. }
                )
            })
            && self
                .windows
                .first()
                .is_some_and(|window| !window.app.automation_mcp_enabled())
        {
            self.automation.close_overflow_admission();
        }
        if let Some(window) = self.windows.first().filter(|_| *overflow) {
            window.app.report_automation_action_conflict();
        }
    }

    fn settle_windows_and_drive_automation(&mut self, context: &egui::Context) {
        let mut commands = std::mem::take(&mut self.deferred_automation_commands);
        let mut overflow = std::mem::take(&mut self.deferred_automation_overflow);
        // A window can close during settlement; collect its stop actions first.
        self.collect_automation_commands(&mut commands, &mut overflow);
        self.settle_windows(context);
        if context.will_discard() {
            // A later layout pass can contain a cancellation from another
            // window. Do not commit approvals before the frame settles.
            self.collect_automation_commands(&mut commands, &mut overflow);
            self.deferred_automation_commands = commands;
            self.deferred_automation_overflow = overflow;
            return;
        }
        self.drive_automation(context, commands, overflow);
        self.windows[0].app.finish_automation_preference_pass();
    }

    fn drive_automation(
        &mut self,
        context: &egui::Context,
        mut commands: Vec<crate::automation::NativeWindowCommand>,
        mut overflow: bool,
    ) {
        self.collect_automation_commands(&mut commands, &mut overflow);
        if overflow
            || commands.iter().any(|item| {
                matches!(
                    &item.command,
                    crate::automation::NativeCommand::ClearWorkflow
                        | crate::automation::NativeCommand::SafetyStop
                        | crate::automation::NativeCommand::PreferenceChanged {
                            clear_workflow: true
                        }
                )
            })
        {
            for window in &mut self.windows {
                window.app.cancel_automation_workflow_picker(context);
            }
        }
        for window in &mut self.windows {
            window.app.sync_automation_preferences();
            window
                .app
                .set_automation_ime_epoch(&self.ime_admission.epoch);
        }
        let apps: Vec<_> = self.windows.iter().map(|window| &window.app).collect();
        let conflicted = if overflow {
            self.automation.command_overflow_for_apps(&apps);
            true
        } else {
            self.automation.tick(context, &apps);
            self.automation.commands(commands, context, &apps)
        };
        if let Some(app) = apps.first().filter(|_| conflicted) {
            app.report_automation_action_conflict();
            context.request_repaint();
        }
        let snapshot = self.automation.settings(&apps);
        let mut changed = false;
        for window in &mut self.windows {
            changed |= window.app.set_automation_settings(snapshot.clone());
        }
        if changed {
            context.request_repaint();
        }
    }
}

/// How far left of the pointer a detached window's origin is placed, so the
/// tab the user is still holding lands on the new window's chip row rather
/// than at its very corner.
const DETACH_POINTER_INSET: f32 = 60.0;

impl eframe::App for FesTermApplication {
    fn raw_input_hook(&mut self, context: &egui::Context, input: &mut egui::RawInput) {
        self.ime_admission.observe(WindowId::PRIMARY, &input.events);
        eframe::App::raw_input_hook(self.primary_mut(), context, input);
    }

    fn logic(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.automation.advance_presentation_deadline(context);
        self.refresh_application_close_context();
        eframe::App::logic(self.primary_mut(), context, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        ui.ctx()
            .input(|input| self.ime_admission.observe(WindowId::PRIMARY, &input.events));
        if ui.ctx().input(|input| input.viewport().focused) == Some(true) {
            self.last_active_window = WindowId::PRIMARY;
        }
        eframe::App::ui(self.primary_mut(), ui, frame);
        let context = ui.ctx().clone();
        self.show_secondary_windows(&context);
        festerm_ui_egui::chrome::tab_drag::show_requested_drag_ghost(&context);
        self.settle_windows_and_drive_automation(&context);
        self.process_document_activation(&context);
        if !context.will_discard() && self.ime_admission.finish_pass() {
            context.request_repaint();
        }
        tracing::debug!(
            target: "festerm::rendering",
            gui_frame_number = context.cumulative_frame_nr(),
            "built GUI frame"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::preferences::{Feature, Values};
    use crate::configuration_startup::{
        ConfigurationReloader, SessionNameConfigurationFixture, TestSaveFailure,
    };
    use crate::tabs::{AppCommand, TabId};
    use festerm_config::{Configuration, InterfaceSettings, Profile, TerminalFontPreference};
    use festerm_ui_egui::TerminalFontFamily;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn application() -> (FesTermApplication, egui::Context) {
        let context = egui::Context::default();
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        (FesTermApplication::new(app), context)
    }

    fn with_feature(mut values: Values, feature: Feature, enabled: bool) -> Values {
        match feature {
            Feature::Mcp => values.mcp = enabled,
            Feature::Javascript => values.javascript = enabled,
        }
        values
    }

    fn other_feature(feature: Feature) -> Feature {
        match feature {
            Feature::Mcp => Feature::Javascript,
            Feature::Javascript => Feature::Mcp,
        }
    }

    fn automation_configuration(values: Values) -> Configuration {
        Configuration::empty()
            .with_interface_settings(
                InterfaceSettings::DEFAULT
                    .with_mcp_enabled(values.mcp)
                    .with_javascript_enabled(values.javascript),
            )
            .unwrap()
    }

    fn application_with_saved_automation_preferences(
        values: Values,
    ) -> (
        FesTermApplication,
        egui::Context,
        SessionNameConfigurationFixture,
    ) {
        let fixture = SessionNameConfigurationFixture::new();
        let configuration = automation_configuration(values);
        configuration.save_to_path(&fixture.path).unwrap();
        let mut primary = FesTermApp::for_test_with_configuration(configuration);
        primary.set_reloader_for_test(ConfigurationReloader::from_path_for_test(
            fixture.path.clone(),
        ));
        let context = egui::Context::default();
        let mut application = FesTermApplication::new(primary);
        application.open_window(&context, None);
        (application, context, fixture)
    }

    fn assert_saved_automation_preferences(path: &std::path::Path, expected: Values) {
        let configuration = Configuration::load_from_path(path).unwrap();
        let settings = configuration.interface_settings();
        assert_eq!(settings.mcp_enabled(), expected.mcp);
        assert_eq!(settings.javascript_enabled(), expected.javascript);
    }

    struct LivePreferenceFixture {
        application: FesTermApplication,
        context: egui::Context,
        file: SessionNameConfigurationFixture,
        target: festerm_automation::TargetRef,
        stop: Arc<AtomicBool>,
        _receive: Box<dyn Fn() -> Option<Vec<u8>>>,
    }

    fn live_preference_fixture(approved: bool) -> LivePreferenceFixture {
        let (coordinator, mut primary, target, stop, receive) =
            crate::automation::preference_application_fixture(approved);
        let file = SessionNameConfigurationFixture::new();
        let configuration = automation_configuration(Values {
            mcp: true,
            javascript: true,
        });
        configuration.save_to_path(&file.path).unwrap();
        primary.adopt_broadcast_configuration(configuration);
        primary.set_reloader_for_test(ConfigurationReloader::from_path_for_test(file.path.clone()));
        let context = egui::Context::default();
        let mut application = FesTermApplication::new(primary);
        application.automation = coordinator;
        application.open_window(&context, None);
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        assert_eq!(snapshot.channels.len(), 1);
        for window in &mut application.windows {
            window.app.set_automation_settings(snapshot.clone());
        }
        LivePreferenceFixture {
            application,
            context,
            file,
            target,
            stop,
            _receive: Box::new(receive),
        }
    }

    fn reviewed_controller_application() -> (
        FesTermApplication,
        egui::Context,
        SessionNameConfigurationFixture,
        festerm_automation::ControllerRegistrationId,
        u64,
        impl Fn() -> Option<Vec<u8>>,
    ) {
        let (mut primary, tab) = FesTermApp::automation_local_fixture();
        let file = SessionNameConfigurationFixture::new();
        let configuration = automation_configuration(Values {
            mcp: true,
            javascript: false,
        });
        configuration.save_to_path(&file.path).unwrap();
        primary.adopt_broadcast_configuration(configuration);
        primary.set_reloader_for_test(ConfigurationReloader::from_path_for_test(file.path.clone()));
        let context = egui::Context::default();
        let mut application = FesTermApplication::new(primary);
        application.open_window(&context, None);
        let (coordinator, registration, token, receive) =
            crate::automation::reviewed_controller_for_application(
                &application.windows[0].app,
                tab,
                &application.presentation_workspace,
            );
        application.automation = coordinator;
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        for window in &mut application.windows {
            window.app.set_automation_settings(snapshot.clone());
        }
        (application, context, file, registration, token, receive)
    }

    #[test]
    fn cross_window_disable_or_cancel_precedes_controller_confirmation() {
        use crate::automation::NativeCommand;

        for disable in [false, true] {
            for reverse in [false, true] {
                let (mut application, context, file, registration, token, _receive) =
                    reviewed_controller_application();
                let queue_confirmation = |application: &mut FesTermApplication| {
                    application.window_mut(0).queue_automation_command(
                        NativeCommand::ConfirmController {
                            channel: 1,
                            registration,
                            selection: token,
                        },
                    );
                };
                let cancel_or_disable = |application: &mut FesTermApplication| {
                    if disable {
                        application
                            .window_mut(1)
                            .toggle_automation_preference_for_test(Feature::Mcp, &context);
                    } else {
                        application.window_mut(1).queue_automation_command(
                            NativeCommand::CancelController {
                                channel: 1,
                                registration,
                            },
                        );
                        application.window_mut(1).accept_window_close_for_test();
                    }
                };
                if reverse {
                    cancel_or_disable(&mut application);
                    queue_confirmation(&mut application);
                } else {
                    queue_confirmation(&mut application);
                    cancel_or_disable(&mut application);
                }
                application.settle_windows_and_drive_automation(&context);
                let apps: Vec<_> = application
                    .windows
                    .iter()
                    .map(|window| &window.app)
                    .collect();
                let snapshot = application.automation.settings(&apps);
                assert!(snapshot.controllers.is_empty());
                assert!(snapshot.status.contains("conflicted"));
                assert!(application.windows[0]
                    .app
                    .automation_preference_feedback_for_test()
                    .unwrap()
                    .contains("may not have been applied"));
                if disable {
                    assert_saved_automation_preferences(
                        &file.path,
                        Values {
                            mcp: false,
                            javascript: false,
                        },
                    );
                }
            }
        }
    }

    #[test]
    fn controller_review_rejects_away_and_back_tab_selection() {
        use crate::automation::{ControllerSelectionView, NativeCommand};

        let (mut application, context, _file, registration, token, _receive) =
            reviewed_controller_application();
        let candidate = {
            let apps: Vec<_> = application
                .windows
                .iter()
                .map(|window| &window.app)
                .collect();
            application.automation.settings(&apps).controller_targets[0].clone()
        };
        let active = application.window_mut(0).active_tab_id_for_test();
        let revision = application.presentation_workspace.current().unwrap();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::ActivateTab(active), &context);
        assert!(application.presentation_workspace.current().unwrap() >= revision + 2);
        let current_candidate = {
            let apps: Vec<_> = application
                .windows
                .iter()
                .map(|window| &window.app)
                .collect();
            application.automation.settings(&apps).controller_targets[0].clone()
        };
        assert!(candidate == current_candidate);

        application
            .window_mut(0)
            .queue_automation_command(NativeCommand::ConfirmController {
                channel: 1,
                registration,
                selection: token,
            });
        application.settle_windows_and_drive_automation(&context);
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        assert!(snapshot.controllers[0].associated_target.is_none());
        assert!(matches!(
            snapshot.controllers[0].selection,
            ControllerSelectionView::Idle
        ));
        assert!(snapshot.status.contains("refused"));
    }

    #[test]
    fn controller_chooser_tracks_surviving_window_identity_without_hidden_titles() {
        use crate::automation::{ControllerSelectionView, NativeCommand};

        let (mut application, context, _file, registration, _review, _receive) =
            reviewed_controller_application();
        let third_index = application.open_window(&context, None);
        let (mut donor, third_tab) = FesTermApp::automation_local_fixture();
        application.windows[third_index]
            .app
            .adopt_detached_tab(donor.detach_tab(third_tab).unwrap());
        application.window_mut(1).accept_window_close_for_test();
        application.settle_windows_and_drive_automation(&context);
        assert_eq!(application.windows.len(), 2);
        let third = application.windows[1].id;
        assert_eq!(third, WindowId::for_test(2));
        assert_eq!(application.windows[1].app.automation_window(), third);

        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        let target = snapshot
            .controller_targets
            .iter()
            .find(|candidate| candidate.reference.session() == third_tab.chip_id())
            .unwrap();
        assert_eq!(target.window, third);
        assert!(!target.label.contains("Window 2"));
        let primary_target = snapshot
            .controller_targets
            .iter()
            .find(|candidate| candidate.window == WindowId::PRIMARY)
            .unwrap();
        assert_eq!(
            application.windows[1]
                .app
                .automation_session(third_tab)
                .unwrap()
                .label,
            application.windows[0]
                .app
                .automation_session(TabId::from_chip_id(primary_target.reference.session()))
                .unwrap()
                .label
        );
        assert_ne!(target.reference, primary_target.reference);
        let reference = target.reference;
        application
            .window_mut(1)
            .queue_automation_command(NativeCommand::ChooseController {
                channel: 1,
                registration,
            });
        application.settle_windows_and_drive_automation(&context);
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        let ControllerSelectionView::Choosing { token, window } = snapshot.controllers[0].selection
        else {
            panic!("expected native chooser");
        };
        assert_eq!(window, third);
        application
            .window_mut(1)
            .queue_automation_command(NativeCommand::SelectControllerTarget {
                channel: 1,
                registration,
                selection: token,
                target: reference,
            });
        application.settle_windows_and_drive_automation(&context);
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        let snapshot = application.automation.settings(&apps);
        let ControllerSelectionView::Reviewing { token, window, .. } =
            snapshot.controllers[0].selection
        else {
            panic!("expected native review");
        };
        assert_eq!(window, third);
        application
            .window_mut(1)
            .queue_automation_command(NativeCommand::ConfirmController {
                channel: 1,
                registration,
                selection: token,
            });
        application.settle_windows_and_drive_automation(&context);
        let apps: Vec<_> = application
            .windows
            .iter()
            .map(|window| &window.app)
            .collect();
        assert_eq!(
            application.automation.settings(&apps).controllers[0].associated_target,
            Some(reference)
        );
    }

    #[test]
    fn automation_disables_fence_stale_cross_window_saves_and_reload() {
        for disabled in [Feature::Mcp, Feature::Javascript] {
            let other = other_feature(disabled);
            for other_initial in [false, true] {
                for first_window in [0, 1] {
                    let initial = with_feature(
                        with_feature(
                            Values {
                                mcp: false,
                                javascript: false,
                            },
                            disabled,
                            true,
                        ),
                        other,
                        other_initial,
                    );
                    let (mut application, context, file) =
                        application_with_saved_automation_preferences(initial);
                    let (stale, _, stale_reloader, ..) = application.windows[1 - first_window]
                        .app
                        .shared_application_services();

                    application
                        .window_mut(first_window)
                        .toggle_automation_preference_for_test(disabled, &context);
                    let mut expected = with_feature(initial, disabled, false);
                    assert_saved_automation_preferences(&file.path, expected);

                    application
                        .window_mut(1 - first_window)
                        .toggle_automation_preference_for_test(other, &context);
                    expected = with_feature(expected, other, !other_initial);
                    assert_saved_automation_preferences(&file.path, expected);

                    let compact = !stale.interface_settings().compact_launcher_grid();
                    let unrelated = stale
                        .with_interface_settings(
                            stale
                                .interface_settings()
                                .clone()
                                .with_compact_launcher_grid(compact),
                        )
                        .unwrap();
                    let saved = stale_reloader.save_configuration(&unrelated).unwrap();
                    assert_eq!(saved.interface_settings().mcp_enabled(), expected.mcp);
                    assert_eq!(
                        saved.interface_settings().javascript_enabled(),
                        expected.javascript
                    );
                    assert_eq!(saved.interface_settings().compact_launcher_grid(), compact);
                    assert_saved_automation_preferences(&file.path, expected);

                    application.settle_windows_and_drive_automation(&context);
                    for window in &application.windows {
                        let configuration = window.app.shared_application_services().0;
                        assert_eq!(
                            configuration.interface_settings().mcp_enabled(),
                            expected.mcp
                        );
                        assert_eq!(
                            configuration.interface_settings().javascript_enabled(),
                            expected.javascript
                        );
                    }
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    assert!(application
                        .automation
                        .settings(&apps)
                        .status
                        .contains("may not have been applied"));

                    application.settle_windows_and_drive_automation(&context);
                    assert_saved_automation_preferences(&file.path, expected);
                    let loaded = Configuration::load_from_path(&file.path).unwrap();
                    let restarted =
                        FesTermApplication::new(FesTermApp::for_test_with_configuration(loaded));
                    assert_eq!(
                        restarted.windows[0].app.automation_mcp_enabled(),
                        expected.mcp
                    );
                    assert_eq!(
                        restarted.windows[0].app.automation_javascript_enabled(),
                        expected.javascript
                    );
                    assert!(restarted
                        .automation
                        .settings(&[&restarted.windows[0].app])
                        .channels
                        .is_empty());
                }
            }
        }
    }

    #[test]
    fn automation_disables_survive_stale_broadcasts_unrelated_saves_and_close() {
        let initial = Values {
            mcp: true,
            javascript: true,
        };
        for disabled in [Feature::Mcp, Feature::Javascript] {
            for first_window in [0, 1] {
                let (mut application, context, file) =
                    application_with_saved_automation_preferences(initial);
                let stale = application.windows[1 - first_window]
                    .app
                    .shared_application_services()
                    .0;
                application
                    .window_mut(first_window)
                    .toggle_automation_preference_for_test(disabled, &context);
                assert!(application
                    .window_mut(1 - first_window)
                    .save_unrelated_interface_setting_for_test(&context));
                let expected = with_feature(initial, disabled, false);
                assert_saved_automation_preferences(&file.path, expected);

                application
                    .window_mut(1 - first_window)
                    .broadcast_for_test(stale);
                if first_window == 1 {
                    application.window_mut(1).accept_window_close_for_test();
                }
                application.settle_windows_and_drive_automation(&context);
                application.settle_windows_and_drive_automation(&context);
                assert_saved_automation_preferences(&file.path, expected);
                for window in &application.windows {
                    let configuration = window.app.shared_application_services().0;
                    assert_eq!(
                        configuration.interface_settings().mcp_enabled(),
                        expected.mcp
                    );
                    assert_eq!(
                        configuration.interface_settings().javascript_enabled(),
                        expected.javascript
                    );
                    assert_eq!(window.app.automation_mcp_enabled(), expected.mcp);
                    assert_eq!(
                        window.app.automation_javascript_enabled(),
                        expected.javascript
                    );
                }
            }
        }
    }

    #[test]
    fn automation_preference_save_failures_stay_stopped_and_visible_until_retry() {
        let initial = Values {
            mcp: true,
            javascript: true,
        };
        for disabled in [Feature::Mcp, Feature::Javascript] {
            for failure in [TestSaveFailure::BeforeWrite, TestSaveFailure::AfterWrite] {
                for first_window in [0, 1] {
                    let (mut application, context, file) =
                        application_with_saved_automation_preferences(initial);
                    let stale = application.windows[1 - first_window]
                        .app
                        .shared_application_services()
                        .0;
                    application.windows[first_window]
                        .app
                        .shared_application_services()
                        .2
                        .fail_next_save_for_test(failure);
                    application
                        .window_mut(first_window)
                        .toggle_automation_preference_for_test(disabled, &context);
                    for window in &application.windows {
                        assert!(!window.app.automation_mcp_enabled());
                        assert!(!window.app.automation_javascript_enabled());
                        let warning = window
                            .app
                            .automation_preference_feedback_for_test()
                            .unwrap();
                        assert!(warning.contains("outcome is uncertain"));
                        assert!(warning.contains("may remain on disk"));
                        assert!(!warning.contains(file.path.to_string_lossy().as_ref()));
                    }
                    let desired = with_feature(initial, disabled, false);
                    let expected_disk = match failure {
                        TestSaveFailure::BeforeWrite => initial,
                        TestSaveFailure::AfterWrite => desired,
                    };
                    assert_saved_automation_preferences(&file.path, expected_disk);
                    let loaded = Configuration::load_from_path(&file.path).unwrap();
                    let restarted =
                        FesTermApplication::new(FesTermApp::for_test_with_configuration(loaded));
                    assert!(restarted
                        .automation
                        .settings(&[&restarted.windows[0].app])
                        .channels
                        .is_empty());

                    application
                        .window_mut(1 - first_window)
                        .broadcast_for_test(stale);
                    application.settle_windows_and_drive_automation(&context);
                    for window in &application.windows {
                        let configuration = window.app.shared_application_services().0;
                        assert_eq!(
                            configuration.interface_settings().mcp_enabled(),
                            desired.mcp
                        );
                        assert_eq!(
                            configuration.interface_settings().javascript_enabled(),
                            desired.javascript
                        );
                        assert!(!window.app.automation_mcp_enabled());
                        assert!(!window.app.automation_javascript_enabled());
                    }
                    assert!(application
                        .window_mut(1 - first_window)
                        .save_unrelated_interface_setting_for_test(&context));
                    assert_saved_automation_preferences(&file.path, desired);
                    assert!(application.windows[0]
                        .app
                        .automation_preference_feedback_for_test()
                        .unwrap()
                        .contains("outcome is uncertain"));
                    application.settle_windows_and_drive_automation(&context);
                    application
                        .window_mut(0)
                        .retry_automation_preferences_for_test(&context);
                    application.settle_windows_and_drive_automation(&context);
                    for window in &application.windows {
                        assert!(!window.app.automation_mcp_enabled());
                        assert!(!window.app.automation_javascript_enabled());
                        assert!(window
                            .app
                            .automation_preference_feedback_for_test()
                            .unwrap()
                            .contains("remain stopped until restart"));
                    }
                    assert_saved_automation_preferences(&file.path, desired);
                }
            }
        }
    }

    #[test]
    fn automation_uncertain_enable_is_not_copied_by_an_unrelated_save() {
        let off = Values {
            mcp: false,
            javascript: false,
        };
        for feature in [Feature::Mcp, Feature::Javascript] {
            for failure in [TestSaveFailure::BeforeWrite, TestSaveFailure::AfterWrite] {
                let (mut application, context, file) =
                    application_with_saved_automation_preferences(off);
                application.windows[0]
                    .app
                    .shared_application_services()
                    .2
                    .fail_next_save_for_test(failure);
                application
                    .window_mut(0)
                    .toggle_automation_preference_for_test(feature, &context);
                let expected_disk = match failure {
                    TestSaveFailure::BeforeWrite => off,
                    TestSaveFailure::AfterWrite => with_feature(off, feature, true),
                };
                assert_saved_automation_preferences(&file.path, expected_disk);
                assert!(!application.windows[0].app.automation_mcp_enabled());
                assert!(!application.windows[1].app.automation_javascript_enabled());
                assert!(application
                    .window_mut(1)
                    .save_unrelated_interface_setting_for_test(&context));
                assert_saved_automation_preferences(&file.path, off);
                assert!(application.windows[1]
                    .app
                    .automation_preference_feedback_for_test()
                    .unwrap()
                    .contains("outcome is uncertain"));
            }
        }
    }

    #[test]
    fn automation_same_pass_stale_toggles_and_reset_cannot_reenable() {
        let initial = Values {
            mcp: true,
            javascript: true,
        };
        for feature in [Feature::Mcp, Feature::Javascript] {
            for first_window in [0, 1] {
                let (mut application, context, file) =
                    application_with_saved_automation_preferences(initial);
                application
                    .window_mut(first_window)
                    .toggle_automation_preference_for_test(feature, &context);
                application
                    .window_mut(1 - first_window)
                    .toggle_automation_preference_for_test(feature, &context);
                let disabled = with_feature(initial, feature, false);
                assert_saved_automation_preferences(&file.path, disabled);
                assert!(application.windows[0]
                    .app
                    .automation_preference_feedback_for_test()
                    .unwrap()
                    .contains("may not have been applied"));
                application.settle_windows_and_drive_automation(&context);
                assert_saved_automation_preferences(&file.path, disabled);

                application
                    .window_mut(1 - first_window)
                    .toggle_automation_preference_for_test(feature, &context);
                application.settle_windows_and_drive_automation(&context);
                assert_saved_automation_preferences(&file.path, initial);

                application
                    .window_mut(first_window)
                    .reset_automation_preferences_for_test(&context);
                application
                    .window_mut(1 - first_window)
                    .toggle_automation_preference_for_test(feature, &context);
                let off = Values {
                    mcp: false,
                    javascript: false,
                };
                assert_saved_automation_preferences(&file.path, off);
                application.settle_windows_and_drive_automation(&context);
                assert_saved_automation_preferences(&file.path, off);
                assert!(application.windows[0]
                    .app
                    .automation_preference_feedback_for_test()
                    .unwrap()
                    .contains("may not have been applied"));
            }
        }
    }

    #[test]
    fn automation_cross_window_aba_invalidates_the_old_toggle_interaction() {
        let off = Values {
            mcp: false,
            javascript: false,
        };
        for feature in [Feature::Mcp, Feature::Javascript] {
            let (mut application, context, file) =
                application_with_saved_automation_preferences(off);
            let preferences = application.windows[0]
                .app
                .shared_application_services()
                .2
                .automation_preferences()
                .unwrap();
            let old_revision = preferences.ui_revision(feature);
            application
                .window_mut(1)
                .toggle_automation_preference_for_test(feature, &context);
            application
                .window_mut(0)
                .toggle_automation_preference_for_test(feature, &context);
            assert_ne!(preferences.ui_revision(feature), old_revision);
            assert_saved_automation_preferences(&file.path, off);
            application.settle_windows_and_drive_automation(&context);
            assert_saved_automation_preferences(&file.path, off);
        }
    }

    #[test]
    fn automation_disable_guard_survives_discarded_ui_pass() {
        let initial = Values {
            mcp: true,
            javascript: true,
        };
        for feature in [Feature::Mcp, Feature::Javascript] {
            let (mut application, context, file) =
                application_with_saved_automation_preferences(initial);
            context.options_mut(|options| options.max_passes = 2.try_into().unwrap());
            let mut passes = 0;
            context
                .run_ui(egui::RawInput::default(), |ui| {
                    if passes == 0 {
                        application
                            .window_mut(0)
                            .toggle_automation_preference_for_test(feature, ui.ctx());
                        ui.ctx().request_discard("preference conflict fixture");
                    } else {
                        application
                            .window_mut(1)
                            .toggle_automation_preference_for_test(feature, ui.ctx());
                    }
                    application.settle_windows_and_drive_automation(ui.ctx());
                    passes += 1;
                })
                .drop_without_applying_deltas();
            assert_eq!(passes, 2);
            assert_saved_automation_preferences(&file.path, with_feature(initial, feature, false));
            assert!(application.windows[0]
                .app
                .automation_preference_feedback_for_test()
                .unwrap()
                .contains("may not have been applied"));
        }
    }

    #[test]
    fn automation_reset_from_stale_default_window_saves_or_warns() {
        let off = Values {
            mcp: false,
            javascript: false,
        };
        for feature in [Feature::Mcp, Feature::Javascript] {
            for first_window in [0, 1] {
                for failure in [
                    None,
                    Some(TestSaveFailure::BeforeWrite),
                    Some(TestSaveFailure::AfterWrite),
                ] {
                    let (mut application, context, file) =
                        application_with_saved_automation_preferences(off);
                    application
                        .window_mut(first_window)
                        .toggle_automation_preference_for_test(feature, &context);
                    let enabled = with_feature(off, feature, true);
                    assert_saved_automation_preferences(&file.path, enabled);
                    let resetting = 1 - first_window;
                    if let Some(failure) = failure {
                        application.windows[resetting]
                            .app
                            .shared_application_services()
                            .2
                            .fail_next_save_for_test(failure);
                    }
                    application
                        .window_mut(resetting)
                        .request_reset_interface_settings_for_test(&context);
                    let expected_disk = match failure {
                        Some(TestSaveFailure::BeforeWrite) => enabled,
                        _ => off,
                    };
                    assert_saved_automation_preferences(&file.path, expected_disk);
                    if failure.is_some() {
                        assert!(application.windows[resetting]
                            .app
                            .automation_preference_feedback_for_test()
                            .unwrap()
                            .contains("may remain on disk"));
                    }
                    application.settle_windows_and_drive_automation(&context);
                    for window in &application.windows {
                        assert!(!window.app.automation_mcp_enabled());
                        assert!(!window.app.automation_javascript_enabled());
                    }
                    assert_saved_automation_preferences(&file.path, expected_disk);
                }
            }
        }
    }

    #[test]
    fn automation_preferences_remain_independent_for_normal_toggles() {
        let off = Values {
            mcp: false,
            javascript: false,
        };
        let (mut application, context, file) = application_with_saved_automation_preferences(off);
        for (window, feature, expected) in [
            (0, Feature::Mcp, with_feature(off, Feature::Mcp, true)),
            (
                1,
                Feature::Javascript,
                Values {
                    mcp: true,
                    javascript: true,
                },
            ),
            (
                0,
                Feature::Javascript,
                Values {
                    mcp: true,
                    javascript: false,
                },
            ),
            (1, Feature::Mcp, off),
        ] {
            application
                .window_mut(window)
                .toggle_automation_preference_for_test(feature, &context);
            application.settle_windows_and_drive_automation(&context);
            assert_saved_automation_preferences(&file.path, expected);
            for sibling in &application.windows {
                assert_eq!(sibling.app.automation_mcp_enabled(), expected.mcp);
                assert_eq!(
                    sibling.app.automation_javascript_enabled(),
                    expected.javascript
                );
                assert!(sibling
                    .app
                    .automation_preference_feedback_for_test()
                    .is_none());
            }
        }
        let loaded = Configuration::load_from_path(&file.path).unwrap();
        let restarted = FesTermApplication::new(FesTermApp::for_test_with_configuration(loaded));
        assert!(!restarted.windows[0].app.automation_mcp_enabled());
        assert!(!restarted.windows[0].app.automation_javascript_enabled());
    }

    #[test]
    fn automation_conflict_warning_survives_frames_and_unrelated_saves() {
        let initial = Values {
            mcp: true,
            javascript: true,
        };
        let (mut application, context, file) =
            application_with_saved_automation_preferences(initial);
        application
            .window_mut(0)
            .queue_automation_command(crate::automation::NativeCommand::SafetyStop);
        application.settle_windows_and_drive_automation(&context);
        assert!(application
            .window_mut(1)
            .save_unrelated_interface_setting_for_test(&context));
        application.settle_windows_and_drive_automation(&context);
        for window in &application.windows {
            assert!(window
                .app
                .automation_preference_feedback_for_test()
                .unwrap()
                .contains("may not have been applied"));
        }
        assert_saved_automation_preferences(&file.path, initial);

        application
            .window_mut(1)
            .toggle_automation_preference_for_test(Feature::Javascript, &context);
        application.settle_windows_and_drive_automation(&context);
        assert_saved_automation_preferences(
            &file.path,
            with_feature(initial, Feature::Javascript, false),
        );
        assert!(application.windows[0]
            .app
            .automation_preference_feedback_for_test()
            .is_none());
    }

    #[test]
    fn automation_failed_disables_stop_live_admission_before_settlement() {
        use crate::automation::WorkflowView;
        use festerm_automation_js::snapshot::{ApprovedBundle, Manifest, SourceSnapshot};
        use std::collections::{BTreeMap, BTreeSet};

        for disabled in [Feature::Mcp, Feature::Javascript] {
            for failure in [TestSaveFailure::BeforeWrite, TestSaveFailure::AfterWrite] {
                for first_window in [0, 1] {
                    let LivePreferenceFixture {
                        mut application,
                        context,
                        file,
                        stop,
                        _receive,
                        ..
                    } = live_preference_fixture(true);
                    let bundle = ApprovedBundle::new(SourceSnapshot {
                        manifest: Manifest {
                            version: 1,
                            api_version: 1,
                            name: "test".into(),
                            entry: "main.js".into(),
                            modules: vec!["main.js".into()],
                            parameters: BTreeMap::new(),
                            capabilities: BTreeSet::new(),
                        },
                        modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
                    })
                    .unwrap();
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    let mut snapshot = application.automation.settings(&apps);
                    assert!(snapshot.channels[0].approved);
                    snapshot.workflow = WorkflowView::captured_for_test(bundle);
                    let preview = snapshot.workflow.clone();
                    for window in &mut application.windows {
                        window.app.set_automation_settings(snapshot.clone());
                    }
                    application.windows[first_window]
                        .app
                        .shared_application_services()
                        .2
                        .fail_next_save_for_test(failure);
                    application
                        .window_mut(first_window)
                        .toggle_automation_preference_for_test(disabled, &context);

                    assert!(stop.load(Ordering::Acquire));
                    assert!(preview.current_bundle().is_none());
                    for window in &application.windows {
                        assert!(!window.app.automation_mcp_enabled());
                        assert!(!window.app.automation_javascript_enabled());
                        assert!(window
                            .app
                            .automation_preference_feedback_for_test()
                            .unwrap()
                            .contains("may remain on disk"));
                    }
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    assert!(application.automation.settings(&apps).channels.is_empty());
                    let expected_disk = match failure {
                        TestSaveFailure::BeforeWrite => Values {
                            mcp: true,
                            javascript: true,
                        },
                        TestSaveFailure::AfterWrite => with_feature(
                            Values {
                                mcp: true,
                                javascript: true,
                            },
                            disabled,
                            false,
                        ),
                    };
                    assert_saved_automation_preferences(&file.path, expected_disk);
                }
            }
        }
    }

    #[test]
    fn automation_cross_window_disable_blocks_queued_approvals() {
        use crate::automation::NativeCommand;

        for disabled in [Feature::Mcp, Feature::Javascript] {
            for first_window in [0, 1] {
                for approval_first in [false, true] {
                    for failure in [
                        None,
                        Some(TestSaveFailure::BeforeWrite),
                        Some(TestSaveFailure::AfterWrite),
                    ] {
                        let LivePreferenceFixture {
                            mut application,
                            context,
                            file,
                            target,
                            stop,
                            _receive,
                        } = live_preference_fixture(false);
                        if let Some(failure) = failure {
                            application.windows[first_window]
                                .app
                                .shared_application_services()
                                .2
                                .fail_next_save_for_test(failure);
                        }
                        let approval = || NativeCommand::ApproveObservation {
                            channel: 1,
                            targets: vec![target],
                        };
                        if approval_first {
                            application
                                .window_mut(1 - first_window)
                                .queue_automation_command(approval());
                        }
                        application
                            .window_mut(first_window)
                            .toggle_automation_preference_for_test(disabled, &context);
                        if !approval_first {
                            application
                                .window_mut(1 - first_window)
                                .queue_automation_command(approval());
                        }
                        application.settle_windows_and_drive_automation(&context);
                        assert!(stop.load(Ordering::Acquire));
                        let apps: Vec<_> = application
                            .windows
                            .iter()
                            .map(|window| &window.app)
                            .collect();
                        let snapshot = application.automation.settings(&apps);
                        assert!(snapshot.channels.is_empty());
                        assert!(snapshot.status.contains("conflicted"));
                        let initial = Values {
                            mcp: true,
                            javascript: true,
                        };
                        let expected = match failure {
                            Some(TestSaveFailure::BeforeWrite) => initial,
                            _ => with_feature(initial, disabled, false),
                        };
                        assert_saved_automation_preferences(&file.path, expected);
                        application.settle_windows_and_drive_automation(&context);
                        let apps: Vec<_> = application
                            .windows
                            .iter()
                            .map(|window| &window.app)
                            .collect();
                        assert!(application.automation.settings(&apps).channels.is_empty());
                        assert!(stop.load(Ordering::Acquire));
                    }
                }
            }
        }
    }

    #[test]
    fn automation_discarded_pass_never_approves_before_a_later_disable() {
        use crate::automation::NativeCommand;

        for disabled in [Feature::Mcp, Feature::Javascript] {
            for approval_first in [false, true] {
                let LivePreferenceFixture {
                    mut application,
                    context,
                    file,
                    target,
                    stop,
                    _receive,
                } = live_preference_fixture(false);
                context.options_mut(|options| options.max_passes = 2.try_into().unwrap());
                let mut passes = 0;
                context
                    .run_ui(egui::RawInput::default(), |ui| {
                        if passes == 0 {
                            ui.ctx().request_discard("approval conflict fixture");
                        }
                        if (passes == 0) == approval_first {
                            application.window_mut(1).queue_automation_command(
                                NativeCommand::ApproveObservation {
                                    channel: 1,
                                    targets: vec![target],
                                },
                            );
                        } else {
                            application
                                .window_mut(0)
                                .toggle_automation_preference_for_test(disabled, ui.ctx());
                        }
                        application.settle_windows_and_drive_automation(ui.ctx());
                        if passes == 0 && approval_first {
                            let apps: Vec<_> = application
                                .windows
                                .iter()
                                .map(|window| &window.app)
                                .collect();
                            let snapshot = application.automation.settings(&apps);
                            assert_eq!(snapshot.channels.len(), 1);
                            assert!(!snapshot.channels[0].approved);
                        }
                        passes += 1;
                    })
                    .drop_without_applying_deltas();
                assert_eq!(passes, 2);
                assert!(stop.load(Ordering::Acquire));
                let apps: Vec<_> = application
                    .windows
                    .iter()
                    .map(|window| &window.app)
                    .collect();
                let snapshot = application.automation.settings(&apps);
                assert!(snapshot.channels.is_empty());
                assert!(snapshot.status.contains("conflicted"));
                assert_saved_automation_preferences(
                    &file.path,
                    with_feature(
                        Values {
                            mcp: true,
                            javascript: true,
                        },
                        disabled,
                        false,
                    ),
                );
            }
        }
    }

    #[test]
    fn automation_discarded_pass_cancellation_suppresses_approval() {
        use crate::automation::NativeCommand;

        for clear_first in [false, true] {
            let LivePreferenceFixture {
                mut application,
                context,
                target,
                stop,
                file: _file,
                _receive,
            } = live_preference_fixture(false);
            context.options_mut(|options| options.max_passes = 2.try_into().unwrap());
            let mut passes = 0;
            context
                .run_ui(egui::RawInput::default(), |ui| {
                    if passes == 0 {
                        ui.ctx().request_discard("cancellation conflict fixture");
                    }
                    let command = if (passes == 0) == clear_first {
                        NativeCommand::ClearWorkflow
                    } else {
                        NativeCommand::ApproveObservation {
                            channel: 1,
                            targets: vec![target],
                        }
                    };
                    application
                        .window_mut(passes)
                        .queue_automation_command(command);
                    application.settle_windows_and_drive_automation(ui.ctx());
                    passes += 1;
                })
                .drop_without_applying_deltas();
            assert_eq!(passes, 2);
            assert!(!stop.load(Ordering::Acquire));
            let apps: Vec<_> = application
                .windows
                .iter()
                .map(|window| &window.app)
                .collect();
            let snapshot = application.automation.settings(&apps);
            assert_eq!(snapshot.channels.len(), 1);
            assert!(!snapshot.channels[0].approved);
            assert!(snapshot.status.contains("blocked"));
        }
    }

    #[test]
    fn automation_cross_window_competing_captures_fail_closed_across_passes() {
        use crate::automation::{NativeCommand, SelectedWorkflow, WorkflowStatus};

        for identical in [false, true] {
            for first_window in [0, 1] {
                for split_passes in [false, true] {
                    let LivePreferenceFixture {
                        mut application,
                        context,
                        file,
                        stop,
                        _receive,
                        ..
                    } = live_preference_fixture(false);
                    let capture =
                        |path: &str| NativeCommand::CaptureWorkflow(SelectedWorkflow(path.into()));
                    let complete_capture =
                        application.automation.pending_workflow_capture_for_test();
                    application.settle_windows_and_drive_automation(&context);
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    assert_eq!(
                        application.automation.settings(&apps).workflow.status,
                        WorkflowStatus::Capturing
                    );

                    let second = if identical {
                        "first.json"
                    } else {
                        "second.json"
                    };
                    context.options_mut(|options| options.max_passes = 2.try_into().unwrap());
                    let mut passes = 0;
                    context
                        .run_ui(egui::RawInput::default(), |ui| {
                            if passes == 0 {
                                ui.ctx().request_discard("competing capture fixture");
                                application
                                    .window_mut(first_window)
                                    .queue_automation_command(capture("first.json"));
                                if !split_passes {
                                    application
                                        .window_mut(1 - first_window)
                                        .queue_automation_command(capture(second));
                                }
                            } else if split_passes {
                                application
                                    .window_mut(1 - first_window)
                                    .queue_automation_command(capture(second));
                            }
                            application.settle_windows_and_drive_automation(ui.ctx());
                            if passes == 0 {
                                let apps: Vec<_> = application
                                    .windows
                                    .iter()
                                    .map(|window| &window.app)
                                    .collect();
                                let preview = application.automation.settings(&apps).workflow;
                                if split_passes {
                                    assert!(!stop.load(Ordering::Acquire));
                                    assert_eq!(preview.status, WorkflowStatus::Capturing);
                                } else {
                                    assert!(stop.load(Ordering::Acquire));
                                    assert_eq!(preview.status, WorkflowStatus::Cancelling);
                                    assert!(application.windows[0]
                                        .app
                                        .automation_preference_feedback_for_test()
                                        .unwrap()
                                        .contains("conflicted"));
                                }
                            }
                            passes += 1;
                        })
                        .drop_without_applying_deltas();
                    assert_eq!(passes, 2);
                    assert!(stop.load(Ordering::Acquire));
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    let snapshot = application.automation.settings(&apps);
                    assert!(snapshot.channels.is_empty());
                    assert!(snapshot.status.contains("conflicted"));
                    assert_eq!(snapshot.workflow.status, WorkflowStatus::Cancelling);
                    assert!(snapshot.workflow.current_bundle().is_none());
                    for window in &application.windows {
                        assert!(window
                            .app
                            .automation_preference_feedback_for_test()
                            .unwrap()
                            .contains("conflicted"));
                    }
                    assert_saved_automation_preferences(
                        &file.path,
                        Values {
                            mcp: true,
                            javascript: true,
                        },
                    );
                    complete_capture.send(Err("late result".into())).unwrap();
                    application.settle_windows_and_drive_automation(&context);
                    let apps: Vec<_> = application
                        .windows
                        .iter()
                        .map(|window| &window.app)
                        .collect();
                    let snapshot = application.automation.settings(&apps);
                    assert_eq!(snapshot.workflow.status, WorkflowStatus::Empty);
                    assert!(snapshot.workflow.current_bundle().is_none());
                    assert!(stop.load(Ordering::Acquire));
                }
            }
        }
    }

    #[test]
    fn presentation_routes_exact_tab_but_queueing_focus_is_not_confirmation() {
        use festerm_automation::{Error, NativePresentationState, PresentationRequirement};

        let context = egui::Context::default();
        let (mut app, first) = FesTermApp::automation_fixture();
        app.dispatch_for_test(AppCommand::OpenLauncher, &context);
        let second = app.automation_replace_fixture_session();
        app.automation_session_mut(first).label = "same".into();
        app.automation_session_mut(second).label = "same".into();
        let mut application = FesTermApplication::new(app);
        let (channel, registration, enrollment, monitor, target) = application
            .automation
            .setup_presentation_for_test(&application.windows[0].app, first);
        assert_eq!(application.primary_mut().active_tab_id_for_test(), second);
        assert_eq!(
            application.queue_controller_presentation(
                channel,
                registration,
                enrollment,
                std::time::Duration::from_secs(2),
                &context,
            ),
            Err(Error::Unavailable)
        );
        assert_eq!(application.primary_mut().active_tab_id_for_test(), second);

        let (held_send, held) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let holder = std::thread::spawn(move || {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            held_send.send(()).unwrap();
            released
                .recv_timeout(std::time::Duration::from_secs(2))
                .is_ok()
        });
        held.recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            application.queue_controller_presentation_if(
                channel,
                registration,
                enrollment,
                std::time::Duration::from_secs(2),
                &context,
                |_| true,
            ),
            Err(Error::Busy)
        );
        release.send(()).unwrap();
        assert!(holder.join().unwrap());
        assert_eq!(application.primary_mut().active_tab_id_for_test(), second);

        let attempt = crate::automation::retry_automation_busy(|| {
            application.queue_controller_presentation_if(
                channel,
                registration,
                enrollment,
                std::time::Duration::from_secs(2),
                &context,
                |_| true,
            )
        })
        .unwrap();
        assert_eq!(application.primary_mut().active_tab_id_for_test(), first);
        assert_eq!(attempt.target(), target);
        let revision = application.presentation_workspace.current().unwrap();
        let (principal, broker) = application.automation.presentation_broker_for_test(channel);
        assert_eq!(
            broker.finish_presentation(principal, attempt, None, std::time::Instant::now()),
            Ok(None)
        );
        let simulated = broker
            .observe_presentation(
                monitor,
                NativePresentationState::Foreground {
                    window: attempt.window(),
                    selected: Some(target),
                    workspace_revision: revision,
                },
            )
            .unwrap()
            .unwrap();
        let confirmed = broker
            .finish_presentation(
                principal,
                attempt,
                Some(simulated),
                std::time::Instant::now(),
            )
            .unwrap()
            .unwrap();
        let requirement =
            PresentationRequirement::new(registration, enrollment, confirmed.revision);
        application
            .primary_mut()
            .dispatch_for_test(AppCommand::ActivateTab(second), &context);
        application
            .primary_mut()
            .dispatch_for_test(AppCommand::ActivateTab(first), &context);
        let new_revision = application.presentation_workspace.current().unwrap();
        assert!(new_revision > revision);
        let (principal, broker) = application.automation.presentation_broker_for_test(channel);
        let same_window_and_tab = broker
            .observe_presentation(
                monitor,
                NativePresentationState::Foreground {
                    window: attempt.window(),
                    selected: Some(target),
                    workspace_revision: new_revision,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            broker.revalidate_presentation(
                principal,
                requirement,
                target,
                same_window_and_tab,
                std::time::Instant::now(),
            ),
            Err(Error::Revoked)
        );
    }

    #[test]
    #[allow(deprecated)]
    fn presentation_control_review_rejects_egui_ime_preedit_and_loss_regain() {
        use festerm_automation::{
            Digest, Error, GrantRequest, Invocation, InvocationBinding, Scope,
        };
        use std::{collections::BTreeSet, time::Duration};

        for regain_before_decision in [false, true] {
            let context = egui::Context::default();
            let (app, tab) = FesTermApp::automation_fixture();
            let mut application = FesTermApplication::new(app);
            let ime = application.ime_admission.epoch.clone();
            application.windows[0].app.set_automation_ime_epoch(&ime);
            let (channel, _, _, _, target) = application
                .automation
                .setup_presentation_for_test(&application.windows[0].app, tab);
            let request = GrantRequest {
                scope: Scope {
                    control: BTreeSet::from([target]),
                    invoke_workflows: true,
                    ..Scope::default()
                },
                bindings: BTreeSet::from([InvocationBinding {
                    api_version: 1,
                    invocation: Invocation::Workflow {
                        bundle: Digest([1; 32]),
                        manifest: Digest([2; 32]),
                    },
                    parameters: Digest([3; 32]),
                }]),
                direct_read_calls: BTreeSet::new(),
            };
            let review = crate::automation::retry_automation_busy(|| {
                application.automation.prepare_control_handoff_review(
                    channel,
                    target,
                    request.clone(),
                    Duration::from_secs(60),
                    &mut application.windows[0]
                        .app
                        .automation_session_mut(tab)
                        .controller,
                )
            })
            .unwrap();
            let decision = review.accept_from_native();
            let mut preedit = egui::RawInput {
                events: vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "pending".to_owned(),
                    active_range_chars: None,
                })],
                ..Default::default()
            };
            eframe::App::raw_input_hook(&mut application, &context, &mut preedit);
            assert!(!application.ime_admission.finish_pass());

            if regain_before_decision {
                let mut commit = egui::RawInput {
                    events: vec![egui::Event::Ime(egui::ImeEvent::Commit("x".into()))],
                    ..Default::default()
                };
                eframe::App::raw_input_hook(&mut application, &context, &mut commit);
                assert!(application.ime_admission.finish_pass());
            }
            assert_eq!(
                crate::automation::retry_automation_busy(|| {
                    application
                        .automation
                        .approve_control_handoff(channel, &review, &decision)
                }),
                Err(Error::Revoked)
            );
            if !regain_before_decision {
                assert!(matches!(
                    crate::automation::retry_automation_busy(|| {
                        application.automation.prepare_control_handoff_review(
                            channel,
                            target,
                            request.clone(),
                            Duration::from_secs(60),
                            &mut application.windows[0]
                                .app
                                .automation_session_mut(tab)
                                .controller,
                        )
                    }),
                    Err(Error::Unavailable)
                ));
                let mut commit = egui::RawInput {
                    events: vec![egui::Event::Ime(egui::ImeEvent::Commit("x".into()))],
                    ..Default::default()
                };
                eframe::App::raw_input_hook(&mut application, &context, &mut commit);
                assert!(application.ime_admission.finish_pass());
            }
            let fresh = crate::automation::retry_automation_busy(|| {
                application.automation.prepare_control_handoff_review(
                    channel,
                    target,
                    request.clone(),
                    Duration::from_secs(60),
                    &mut application.windows[0]
                        .app
                        .automation_session_mut(tab)
                        .controller,
                )
            })
            .unwrap();
            let fresh_decision = fresh.accept_from_native();
            crate::automation::retry_automation_busy(|| {
                application
                    .automation
                    .approve_control_handoff(channel, &fresh, &fresh_decision)
            })
            .unwrap();
        }
    }

    #[test]
    fn presentation_workspace_revision_tracks_all_window_selection_and_topology_changes() {
        let (mut application, context) = application();
        let initial_tab = application.primary_mut().active_tab_id_for_test();
        let initial = application.presentation_workspace.current().unwrap();
        application
            .primary_mut()
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application
            .primary_mut()
            .dispatch_for_test(AppCommand::ActivateTab(initial_tab), &context);
        let returned = application.presentation_workspace.current().unwrap();
        assert!(returned >= initial + 2);
        let index = application.open_window(&context, None);
        let opened = application.presentation_workspace.current().unwrap();
        assert!(opened > returned);
        application.windows[index]
            .app
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        assert!(application.presentation_workspace.current().unwrap() > opened);
    }

    #[test]
    fn ime_admission_observes_raw_input_before_same_frame_shortcuts() {
        let (mut application, context) = application();
        let captured = application.ime_admission.epoch.capture().unwrap();
        let mut input = egui::RawInput {
            events: vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "pending".to_owned(),
                    active_range_chars: None,
                }),
                egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::CTRL,
                },
            ],
            ..Default::default()
        };
        eframe::App::raw_input_hook(&mut application, &context, &mut input);
        {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            assert!(!application.ime_admission.epoch.is_current(captured));
        }
        assert_eq!(input.events.len(), 2);
        assert!(!application.ime_admission.finish_pass());
        assert!(application.ime_admission.epoch.capture().is_none());
    }

    #[test]
    #[allow(deprecated)]
    fn ime_admission_waits_for_each_window_and_processes_commit_before_reopening() {
        let mut admission = ImeAdmission::new();
        let original = admission.epoch.capture().unwrap();
        let first = WindowId::PRIMARY;
        let second = WindowId(1);
        admission.observe(first, &[egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert!(!admission.finish_pass());
        {
            let _guard = festerm_session::automation_admission_lock().unwrap();
            assert!(admission.epoch.is_current(original));
        }
        let preedit = egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "pending".to_owned(),
            active_range_chars: None,
        });
        admission.observe(first, std::slice::from_ref(&preedit));
        admission.observe(second, &[preedit]);
        assert!(!admission.finish_pass());

        admission.observe(
            first,
            &[
                egui::Event::Text("ordinary".to_owned()),
                egui::Event::WindowFocused(false),
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }),
            ],
        );
        assert!(!admission.finish_pass());
        assert!(admission.epoch.capture().is_none());

        admission.observe(
            first,
            &[egui::Event::Ime(egui::ImeEvent::Commit("x".into()))],
        );
        assert!(!admission.finish_pass());
        admission.observe(second, &[egui::Event::Ime(egui::ImeEvent::Disabled)]);
        assert!(admission.epoch.capture().is_none());
        assert!(admission.finish_pass());
        assert!(admission.epoch.capture().is_some());
        let _guard = festerm_session::automation_admission_lock().unwrap();
        assert!(!admission.epoch.is_current(original));
    }

    #[test]
    fn automation_channel_updates_schedule_a_followup_frame() {
        let (mut application, context) = application();
        application.window_mut(0).set_automation_settings(
            crate::automation::SettingsSnapshot::with_pending_channel_for_test(7),
        );
        for _ in 0..4 {
            context
                .run_ui(egui::RawInput::default(), |_| {})
                .drop_without_applying_deltas();
        }
        assert!(!context.has_requested_repaint());
        application.settle_windows_and_drive_automation(&context);
        assert!(context.has_requested_repaint());
    }

    #[test]
    fn raw_input_hook_forwards_numeric_diagnostics_without_changing_input() {
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        for level in [tracing::Level::INFO, tracing::Level::DEBUG] {
            let output = Arc::new(Mutex::new(Vec::<u8>::new()));
            let captured = Arc::clone(&output);
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_ansi(false)
                .without_time()
                .with_writer(move || Writer(Arc::clone(&captured)))
                .finish();
            let _subscriber = tracing::subscriber::set_default(subscriber);
            let (mut application, context) = application();
            let position = egui::pos2(987.0, 654.0);
            let mut input = egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Line,
                        delta: egui::vec2(0.0, -3.0),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::Text("owned-diagnostic-text-marker".to_owned()),
                ],
                ..Default::default()
            };
            let before = format!("{input:?}");

            eframe::App::raw_input_hook(&mut application, &context, &mut input);

            assert_eq!(format!("{input:?}"), before);
            let log = String::from_utf8(output.lock().unwrap().clone()).unwrap();
            if level == tracing::Level::INFO {
                assert!(!log.contains("delivering native pointer batch"));
            } else {
                assert!(log.contains("delivering native pointer batch"));
                for count in ["frame=0", "movements=1", "buttons=2", "wheels=1"] {
                    assert!(log.contains(count), "{log}");
                }
            }
            for excluded in ["owned-diagnostic-text-marker", "987", "654"] {
                assert!(!log.contains(excluded), "{log}");
            }
        }
    }

    #[test]
    fn session_alias_multi_window_move_save_restore_and_reset_use_one_configuration() {
        let context = egui::Context::default();
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Screen, "build")
        .unwrap()])
        .unwrap();
        let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.adopt_broadcast_configuration(configuration.clone());
        app.set_session_metadata_for_test(tab, Some("remote"), None);
        app.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        let mut application = FesTermApplication::new(app);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        application.window_mut(0).dispatch_for_test(
            AppCommand::RenameTab(tab, "Window two name".into()),
            &context,
        );
        let target = application.windows[1].id.viewport_id();
        application.window_mut(0).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved: tab,
                target: Some(target),
                before: None,
                screen_position: egui::pos2(900.0, 40.0),
            },
            &context,
        );
        application.settle_windows(&context);
        assert_eq!(
            application.window_mut(1).session_label_for_test(tab),
            "Window two name"
        );
        let saved = Configuration::load_from_path(&fixture.path).unwrap();
        assert_eq!(saved.profiles(), configuration.profiles());
        for window in &application.windows {
            assert_eq!(window.app.shared_application_services().0, saved);
        }
        let restored_app = FesTermApp::with_restored_workspace_for_test(&context, saved.clone());
        let mut restored = FesTermApplication::new(restored_app);
        restored.restore_windows(&context);
        assert_eq!(restored.window_count(), 2);
        let restored_tab = restored.window_mut(1).active_tab_id_for_test();
        assert_eq!(
            restored.window_mut(1).chip_primary_for_test(restored_tab),
            "Window two name"
        );
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::UseDefaultSessionName(tab), &context);
        application.settle_windows(&context);
        assert!(application
            .window_mut(1)
            .session_alias_for_test(tab)
            .is_none());
        let reset = Configuration::load_from_path(&fixture.path).unwrap();
        assert!(!reset.to_toml().unwrap().contains("alias"));
        assert_eq!(reset.profiles(), configuration.profiles());
        assert!(transport.sent().is_empty());
        assert!(transport.operations().is_empty());
    }

    #[test]
    fn session_alias_native_metadata_saves_without_workspace_restore_or_terminal_contents() {
        let context = egui::Context::default();
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                false,
                false,
                false,
            ))
            .unwrap();
        let identity =
            festerm_config::DurableSessionIdentity::native("build", 42, 100, "owned-native-42-100")
                .unwrap();
        let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.adopt_broadcast_configuration(configuration);
        app.set_session_metadata_for_test(tab, None, Some(identity.clone()));
        app.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        let mut application = FesTermApplication::new(app);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::RenameTab(tab, "Durable name".into()), &context);
        application.settle_windows(&context);
        let saved = Configuration::load_from_path(&fixture.path).unwrap();
        assert!(saved.workspace().is_none());
        assert_eq!(
            saved.durable_session_alias(&identity).unwrap().as_str(),
            "Durable name"
        );
        assert!(saved.profiles().is_empty());
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::UseDefaultSessionName(tab), &context);
        application.settle_windows(&context);
        assert!(Configuration::load_from_path(&fixture.path)
            .unwrap()
            .durable_session_alias(&identity)
            .is_none());
        assert!(transport.sent().is_empty());
        assert!(transport.operations().is_empty());
    }

    #[test]
    fn session_alias_native_seed_broadcast_keeps_window_views_independent_and_latest_edit_wins() {
        let context = egui::Context::default();
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                false,
                false,
                false,
            ))
            .unwrap();
        let identity =
            festerm_config::DurableSessionIdentity::native("build", 42, 100, "owned-native-42-100")
                .unwrap();
        let (mut primary, first, first_transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        primary.adopt_broadcast_configuration(configuration.clone());
        primary.set_session_metadata_for_test(first, None, Some(identity.clone()));
        primary.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        let (mut secondary, second, second_transport) =
            FesTermApp::for_test_with_fake_ssh_session([]);
        secondary.adopt_broadcast_configuration(configuration);
        secondary.set_session_metadata_for_test(second, None, Some(identity.clone()));
        let mut application = FesTermApplication::new(primary);
        application.windows.push(Window {
            id: WindowId(1),
            app: secondary,
            placement: None,
        });
        application.next_window_id = 2;
        application.window_mut(1).dispatch_for_test(
            AppCommand::RenameTab(second, "Second window".into()),
            &context,
        );
        application.window_mut(0).dispatch_for_test(
            AppCommand::RenameTab(first, "Latest first window".into()),
            &context,
        );
        application.settle_windows(&context);
        let saved = Configuration::load_from_path(&fixture.path).unwrap();
        assert!(saved.workspace().is_none());
        assert_eq!(
            saved.durable_session_alias(&identity).unwrap().as_str(),
            "Latest first window"
        );
        assert_eq!(
            application.window_mut(0).session_label_for_test(first),
            "Latest first window"
        );
        assert_eq!(
            application.window_mut(1).session_label_for_test(second),
            "Second window"
        );
        for window in &application.windows {
            assert_eq!(window.app.shared_application_services().0, saved);
        }
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::UseDefaultSessionName(first), &context);
        application.settle_windows(&context);
        assert!(Configuration::load_from_path(&fixture.path)
            .unwrap()
            .durable_session_alias(&identity)
            .is_none());
        assert!(application
            .window_mut(0)
            .session_alias_for_test(first)
            .is_none());
        assert_eq!(
            application.window_mut(1).session_label_for_test(second),
            "Second window"
        );
        assert!(first_transport.sent().is_empty());
        assert!(first_transport.operations().is_empty());
        assert!(second_transport.sent().is_empty());
        assert!(second_transport.operations().is_empty());
    }

    #[test]
    fn session_alias_workspace_opt_out_keeps_mux_name_live_only_without_writing_metadata() {
        let context = egui::Context::default();
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        let profile = Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()
        .with_persistence(festerm_config::PersistenceProviderKind::Tmux, "build")
        .unwrap();
        let configuration = Configuration::new(vec![profile])
            .unwrap()
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                false,
                false,
                false,
            ))
            .unwrap();
        let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.adopt_broadcast_configuration(configuration.clone());
        app.set_session_metadata_for_test(tab, Some("remote"), None);
        app.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        let mut application = FesTermApplication::new(app);
        application.window_mut(0).dispatch_for_test(
            AppCommand::RenameTab(tab, "Only this view".into()),
            &context,
        );
        application.settle_windows(&context);
        assert_eq!(
            application.window_mut(0).session_label_for_test(tab),
            "Only this view"
        );
        assert_eq!(
            application.window_mut(0).shared_application_services().0,
            configuration
        );
        assert!(!application.window_mut(0).restores_workspace());
        assert!(!fixture.path.exists());
        assert!(application
            .window_mut(0)
            .take_configuration_broadcast()
            .is_none());
        assert!(application
            .window_mut(0)
            .session_name_notice_for_test()
            .unwrap()
            .contains("open tab only"));
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::UseDefaultSessionName(tab), &context);
        application.settle_windows(&context);
        assert!(application
            .window_mut(0)
            .session_alias_for_test(tab)
            .is_none());
        assert!(!fixture.path.exists());
        assert!(transport.sent().is_empty());
        assert!(transport.operations().is_empty());
    }

    #[test]
    fn session_alias_actual_save_failure_is_visible_and_never_broadcasts_unsaved_metadata() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()])
        .unwrap();
        let (mut app, tab, _) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.adopt_broadcast_configuration(configuration.clone());
        app.set_session_metadata_for_test(tab, Some("remote"), None);
        // A directory is not a valid target file. The real atomic-save path
        // refuses it before creating anything, without touching user data.
        app.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                std::env::current_dir().unwrap(),
            ),
        );
        let mut application = FesTermApplication::new(app);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::RenameTab(tab, "Applies now".into()), &context);
        application.settle_windows(&context);
        assert_eq!(
            application.window_mut(0).session_label_for_test(tab),
            "Applies now"
        );
        for window in &mut application.windows {
            assert_eq!(window.app.shared_application_services().0, configuration);
            assert!(window.app.take_configuration_broadcast().is_none());
            assert!(window
                .app
                .session_name_notice_for_test()
                .unwrap()
                .contains("could not be saved"));
        }
        assert!(matches!(
            application.window_mut(0).shared_application_services().1,
            crate::configuration_startup::ConfigurationStartupStatus::SessionNamesSaveFailure(_)
        ));
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        application.window_mut(0).set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::RenameTab(tab, "Applies now".into()), &context);
        application.settle_windows(&context);
        let saved = Configuration::load_from_path(&fixture.path).unwrap();
        assert_eq!(saved.profiles(), configuration.profiles());
        let festerm_config::WorkspaceTab::SshSession(saved_tab) =
            &saved.workspace().unwrap().tabs()[0]
        else {
            panic!("saved SSH view");
        };
        assert_eq!(saved_tab.alias().unwrap().as_str(), "Applies now");
        for window in &application.windows {
            assert_eq!(window.app.shared_application_services().0, saved);
        }
    }

    #[test]
    fn session_alias_rejected_secret_edit_is_visible_without_mutating_or_saving() {
        const REJECTED_ALIAS: &str = "-----BEGIN PRIVATE KEY-----";
        let context = egui::Context::default();
        let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()])
        .unwrap();
        let identity =
            festerm_config::DurableSessionIdentity::native("build", 42, 100, "owned-native-42-100")
                .unwrap();
        let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.adopt_broadcast_configuration(configuration);
        app.set_session_metadata_for_test(tab, Some("remote"), Some(identity.clone()));
        app.set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        let mut application = FesTermApplication::new(app);
        application.window_mut(0).dispatch_for_test(
            AppCommand::RenameTab(tab, "Previous alias".into()),
            &context,
        );
        application.settle_windows(&context);
        let saved = Configuration::load_from_path(&fixture.path).unwrap();
        let saved_bytes = std::fs::read(&fixture.path).unwrap();
        let saved_status = application.window_mut(0).shared_application_services().1;
        assert_eq!(
            saved.durable_session_alias(&identity).unwrap().as_str(),
            "Previous alias"
        );
        let notice = application
            .window_mut(0)
            .session_name_notice_for_test()
            .map(str::to_owned);
        let refused_target = crate::configuration_startup::SessionNameConfigurationFixture::new();
        std::fs::create_dir(&refused_target.path).unwrap();
        application.window_mut(0).set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                refused_target.path.clone(),
            ),
        );
        for cancelled in ["", " \r\n ", "\u{202e}\u{2028}\0"] {
            application
                .window_mut(0)
                .dispatch_for_test(AppCommand::RenameTab(tab, cancelled.into()), &context);
            application.settle_windows(&context);
            assert_eq!(
                application.window_mut(0).session_name_notice_for_test(),
                notice.as_deref()
            );
            let (current, status, ..) = application.window_mut(0).shared_application_services();
            assert_eq!(current, saved);
            assert_eq!(status, saved_status);
        }
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::RenameTab(tab, REJECTED_ALIAS.into()), &context);
        application.settle_windows(&context);
        let refusal = application
            .window_mut(0)
            .session_name_notice_for_test()
            .unwrap();
        assert!(refusal.contains("Session name was not changed"));
        assert!(refusal.len() <= 200);
        assert!(!refusal.contains(REJECTED_ALIAS));
        assert_eq!(
            application.window_mut(0).session_label_for_test(tab),
            "Previous alias"
        );
        assert_eq!(
            application.window_mut(0).session_alias_for_test(tab),
            Some("Previous alias")
        );
        let (current, status, ..) = application.window_mut(0).shared_application_services();
        assert_eq!(current, saved);
        assert_eq!(status, saved_status);
        assert_eq!(std::fs::read(&fixture.path).unwrap(), saved_bytes);
        assert_eq!(Configuration::load_from_path(&fixture.path).unwrap(), saved);
        assert!(application
            .window_mut(0)
            .take_configuration_broadcast()
            .is_none());
        assert!(transport.sent().is_empty());
        assert!(transport.operations().is_empty());
        application.window_mut(0).set_reloader_for_test(
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                fixture.path.clone(),
            ),
        );
        application.window_mut(0).dispatch_for_test(
            AppCommand::RenameTab(tab, " \nBuild\u{202e}\t bench\r ".into()),
            &context,
        );
        application.settle_windows(&context);
        let sanitized = Configuration::load_from_path(&fixture.path).unwrap();
        assert_eq!(sanitized.profiles(), saved.profiles());
        assert_eq!(
            sanitized.durable_session_alias(&identity).unwrap().as_str(),
            "Build bench"
        );
        let festerm_config::WorkspaceTab::SshSession(saved_tab) =
            &sanitized.workspace().unwrap().tabs()[0]
        else {
            panic!("saved SSH view");
        };
        assert_eq!(saved_tab.alias().unwrap().as_str(), "Build bench");
        assert_eq!(
            application.window_mut(0).session_label_for_test(tab),
            "Build bench"
        );
        assert!(transport.sent().is_empty());
        assert!(transport.operations().is_empty());
    }

    #[test]
    fn session_alias_restored_authentication_chips_display_saved_view_aliases() {
        let context = egui::Context::default();
        let configuration = Configuration::new(vec![Profile::ssh(
            "remote",
            "ssh.example.test",
            22,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .unwrap()])
        .unwrap();
        for sftp in [false, true] {
            for explicit in [false, true] {
                let alias = if sftp { "Saved SFTP" } else { "Saved SSH" };
                let descriptor = if sftp {
                    festerm_config::WorkspaceTab::sftp_session("saved", "remote")
                } else {
                    festerm_config::WorkspaceTab::ssh_session("saved", "remote")
                }
                .unwrap()
                .with_session_alias(
                    explicit.then(|| festerm_config::SessionAlias::new(alias).unwrap()),
                )
                .unwrap();
                let saved = configuration
                    .clone()
                    .with_workspace(
                        festerm_config::WorkspaceConfiguration::new(vec![descriptor], None)
                            .unwrap(),
                    )
                    .unwrap();
                let saved = Configuration::parse(&saved.to_toml().unwrap()).unwrap();
                let app = FesTermApp::with_restored_workspace_for_test(&context, saved);
                let tab = app.active_tab_id_for_test();
                assert_eq!(
                    app.chip_primary_for_test(tab),
                    if explicit { alias } else { "remote" }
                );
                assert_eq!(
                    app.shared_application_services().0.profiles(),
                    configuration.profiles()
                );
            }
        }
    }

    #[test]
    fn session_alias_failed_reset_remains_retryable_until_metadata_is_really_removed() {
        let context = egui::Context::default();
        let identity =
            festerm_config::DurableSessionIdentity::native("build", 42, 100, "owned-native-42-100")
                .unwrap();
        for native in [false, true] {
            let fixture = crate::configuration_startup::SessionNameConfigurationFixture::new();
            let configuration = if native {
                Configuration::empty()
            } else {
                Configuration::new(vec![Profile::ssh(
                    "remote",
                    "ssh.example.test",
                    22,
                    "deploy",
                    "xterm-256color",
                    80,
                    24,
                )
                .unwrap()])
                .unwrap()
            };
            let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
            app.adopt_broadcast_configuration(configuration);
            app.set_session_metadata_for_test(
                tab,
                if native { None } else { Some("remote") },
                native.then(|| identity.clone()),
            );
            app.set_reloader_for_test(
                crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                    fixture.path.clone(),
                ),
            );
            let mut application = FesTermApplication::new(app);
            application
                .window_mut(0)
                .dispatch_for_test(AppCommand::RenameTab(tab, "Saved alias".into()), &context);
            application.settle_windows(&context);
            let saved = Configuration::load_from_path(&fixture.path).unwrap();
            if native {
                assert_eq!(
                    saved.durable_session_alias(&identity).unwrap().as_str(),
                    "Saved alias"
                );
            } else {
                let festerm_config::WorkspaceTab::SshSession(saved_tab) =
                    &saved.workspace().unwrap().tabs()[0]
                else {
                    panic!("saved workspace view");
                };
                assert_eq!(saved_tab.alias().unwrap().as_str(), "Saved alias");
            }
            application.window_mut(0).set_reloader_for_test(
                crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                    std::env::current_dir().unwrap(),
                ),
            );
            application
                .window_mut(0)
                .dispatch_for_test(AppCommand::UseDefaultSessionName(tab), &context);
            application.settle_windows(&context);
            assert!(application
                .window_mut(0)
                .session_alias_for_test(tab)
                .is_none());
            assert!(application.window_mut(0).can_use_default_name_for_test(tab));
            assert!(application
                .window_mut(0)
                .session_name_notice_for_test()
                .unwrap()
                .contains("could not be saved"));
            assert_eq!(Configuration::load_from_path(&fixture.path).unwrap(), saved);
            assert_eq!(
                application.window_mut(0).shared_application_services().0,
                saved
            );
            application.settle_windows(&context);
            assert!(application.window_mut(0).can_use_default_name_for_test(tab));
            application.window_mut(0).set_reloader_for_test(
                crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                    fixture.path.clone(),
                ),
            );
            application
                .window_mut(0)
                .dispatch_for_test(AppCommand::UseDefaultSessionName(tab), &context);
            application.settle_windows(&context);
            assert!(!application.window_mut(0).can_use_default_name_for_test(tab));
            let reset = Configuration::load_from_path(&fixture.path).unwrap();
            if native {
                assert!(reset.durable_session_alias(&identity).is_none());
            } else {
                let festerm_config::WorkspaceTab::SshSession(saved_tab) =
                    &reset.workspace().unwrap().tabs()[0]
                else {
                    panic!("reset workspace view");
                };
                assert!(saved_tab.alias().is_none());
            }
            assert!(transport.sent().is_empty());
            assert!(transport.operations().is_empty());
        }
    }

    #[test]
    fn external_documents_open_in_last_active_window_and_reuse_dirty_views() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        application.last_active_window = application.windows[1].id;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes with spaces.md");
        std::fs::write(&path, "# Original\n").unwrap();
        application.open_external_document(path.clone(), &context);
        assert_eq!(application.window_mut(0).tab_count_for_test(), 1);
        assert_eq!(application.window_mut(1).tab_count_for_test(), 2);
        let tab = application.window_mut(1).active_tab_id_for_test();
        let documents = application.window_mut(1).documents_for_test().clone();
        let id = documents.borrow().find_local(&path).unwrap();
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .text_mut()
            .replace(0..0, "Unsaved\n")
            .unwrap();

        application.last_active_window = WindowId::PRIMARY;
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenSettings, &context);
        application.open_external_document(path.clone(), &context);
        assert_eq!(application.last_active_window, application.windows[1].id);
        assert_eq!(application.window_mut(1).active_tab_id_for_test(), tab);
        assert_eq!(application.window_mut(1).tab_count_for_test(), 3);
        assert_eq!(application.window_mut(0).tab_count_for_test(), 1);
        let documents = documents.borrow();
        assert_eq!(documents.len(), 1);
        let document = documents.get(id).unwrap();
        assert_eq!(document.views(), 1);
        assert!(document.text().is_dirty());
        assert_eq!(document.text().text(), "Unsaved\n# Original\n");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "# Original\n");
    }

    #[test]
    fn external_document_missing_file_preserves_tabs_and_blocks_later_requests() {
        let (mut application, context) = application();
        let directory = tempfile::tempdir().unwrap();
        let before = application.primary_mut().active_tab_id_for_test();
        application.open_external_document(directory.path().join("missing.md"), &context);
        assert_eq!(application.primary_mut().active_tab_id_for_test(), before);
        assert_eq!(application.primary_mut().tab_count_for_test(), 1);
        assert!(application.primary_mut().document_activation_blocked());
    }

    #[test]
    fn document_activation_queue_waits_behind_errors_without_losing_later_files() {
        let (mut application, context) = application();
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.md");
        let later = directory.path().join("later.md");
        std::fs::write(&later, "# Later\n").unwrap();
        application
            .document_activation
            .enqueue([missing, later.clone()])
            .unwrap();
        application.process_document_activation(&context);
        assert!(application.primary_mut().document_activation_blocked());
        assert_eq!(application.document_activation.metrics().queue_depth, 1);
        application.process_document_activation(&context);
        assert_eq!(application.document_activation.metrics().queue_depth, 1);
        assert!(!application.primary_mut().has_local_document(&later));
    }

    #[test]
    fn document_activation_batch_opens_one_document_per_pass_in_order() {
        let (mut application, context) = application();
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.markdown");
        std::fs::write(&first, "# First\n").unwrap();
        std::fs::write(&second, "# Second\n").unwrap();
        application
            .document_activation
            .enqueue([first.clone(), second.clone()])
            .unwrap();
        application.process_document_activation(&context);
        assert!(application.primary_mut().has_local_document(&first));
        assert!(!application.primary_mut().has_local_document(&second));
        application.process_document_activation(&context);
        assert!(application.primary_mut().has_local_document(&second));
        assert_eq!(application.primary_mut().tab_count_for_test(), 3);
        assert_eq!(application.document_activation.metrics().queue_depth, 0);
    }

    /// The first window is eframe's root viewport. Anything else leaves it
    /// unable to publish a footprint, so it can neither receive a dragged tab
    /// nor detach one of its own (ADR 0033).
    #[test]
    fn the_first_window_is_the_root_viewport() {
        assert_eq!(WindowId::PRIMARY.viewport_id(), egui::ViewportId::ROOT);
        assert_ne!(WindowId(1).viewport_id(), egui::ViewportId::ROOT);
    }

    /// An additional window wears the same chrome as the first: on macOS the
    /// native titlebar stays hidden so the chip row owns that band.
    #[test]
    fn an_additional_window_is_built_with_the_same_chrome_as_the_first() {
        let (application, _context) = application();
        let window = Window {
            id: WindowId(1),
            app: FesTermApp::for_test_with_configuration(Configuration::empty()),
            placement: None,
        };
        drop(application);

        let additional = FesTermApplication::viewport_builder(&window);
        let first = crate::window_viewport_builder(
            crate::APPLICATION_TITLE,
            egui::vec2(crate::DEFAULT_WINDOW_WIDTH, crate::DEFAULT_WINDOW_HEIGHT),
        );

        assert_eq!(additional.decorations, first.decorations);
        assert_eq!(additional.titlebar_shown, first.titlebar_shown);
        assert_eq!(additional.title_shown, first.title_shown);
        assert_eq!(
            additional.fullsize_content_view,
            first.fullsize_content_view
        );
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

    /// ADR 0034 §2: a file opened in two windows is one document. Two
    /// registries would mean two undo histories and two chances to overwrite
    /// the other window's work.
    #[test]
    fn every_window_shares_one_document_registry() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);

        let directory = std::env::temp_dir().join(format!(
            "festerm-shared-documents-{}-{}",
            std::process::id(),
            application.window_count()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("notes.md");
        std::fs::write(&path, "alpha\n").unwrap();

        let first = application
            .window_mut(0)
            .documents_for_test()
            .borrow_mut()
            .open_local(&path)
            .unwrap();
        let second = application
            .window_mut(1)
            .documents_for_test()
            .borrow_mut()
            .open_local(&path)
            .unwrap();

        assert_eq!(first, second);
        let documents = application.window_mut(1).documents_for_test().borrow();
        assert_eq!(documents.len(), 1);
        assert_eq!(documents.get(second).unwrap().views(), 2);
        drop(documents);
        let _ = std::fs::remove_dir_all(&directory);
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
        assert!(application.window_mut(1).compact_launcher_grid_for_test());

        application
            .window_mut(0)
            .broadcast_for_test(configuration_without_compact_launcher_grid());
        application.settle_windows(&context);

        assert!(!application.window_mut(1).compact_launcher_grid_for_test());
    }

    #[test]
    fn terminal_font_broadcast_reaches_stale_sibling_before_its_next_reconciliation() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
        assert_eq!(
            application
                .window_mut(1)
                .terminal_font_preference_for_test(),
            TerminalFontPreference::JetBrainsMono
        );

        let configuration = Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT.with_terminal_typography(
                TerminalFontPreference::MapleMono,
                InterfaceSettings::DEFAULT.terminal_ligatures(),
            ))
            .unwrap();
        application.window_mut(0).broadcast_for_test(configuration);
        festerm_ui_egui::reconcile_terminal_font_preference(
            &context,
            TerminalFontFamily::MapleMono,
        );

        application.settle_windows(&context);
        assert_eq!(
            application
                .window_mut(1)
                .terminal_font_preference_for_test(),
            TerminalFontPreference::MapleMono
        );
        application
            .window_mut(1)
            .reinstall_terminal_font_for_test(&context);
        assert!(
            festerm_ui_egui::terminal_font_family_is_desired(
                &context,
                TerminalFontFamily::MapleMono
            ),
            "the same-pass broadcast must replace the stale window preference before its next reconciliation"
        );
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
            .broadcast_for_test(configuration_without_compact_launcher_grid());
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

    #[test]
    fn cancelled_recovery_close_keeps_the_secondary_window_alive() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        application.window_mut(1).queue_recovery_notice_for_test(
            std::path::PathBuf::from("/tmp/.festerm-save-recovery.stage"),
            festerm_document::SaveError::new("Recovery required", "Recover the retained bytes."),
        );
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let mut output = context.run_ui(input, |ui| {
            application.window_mut(1).frame_logic(ui.ctx());
        });
        output.textures_delta.clear();

        application.close_finished_windows(&context);

        assert_eq!(application.window_count(), 2);
        assert!(!application.window_mut(1).window_close_accepted());
    }

    #[test]
    fn primary_quit_refuses_a_dirty_document_owned_by_a_secondary_window() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secondary-only.txt");
        std::fs::write(&path, "before\n").unwrap();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        let documents = application.window_mut(0).documents_for_test().clone();
        let document = documents.borrow().find_local(&path).unwrap();
        documents
            .borrow_mut()
            .get_mut(document)
            .unwrap()
            .text_mut()
            .sync_from_view("unsaved\n")
            .unwrap();
        application.refresh_application_close_context();

        assert!(!application
            .window_mut(0)
            .evaluate_close_request_for_test(&context));

        assert!(!application.window_mut(0).window_close_accepted());
        assert_eq!(
            application.window_mut(0).open_refusal_headline_for_test(),
            Some("Quitting is waiting for unsaved documents")
        );
    }

    #[test]
    fn primary_quit_counts_sessions_owned_by_secondary_windows() {
        let primary = FesTermApp::for_test_with_configuration(Configuration::empty());
        let (secondary, _tab, _transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        let mut application = FesTermApplication::new(primary);
        application.windows.push(Window {
            id: WindowId(1),
            app: secondary,
            placement: None,
        });
        let context = egui::Context::default();
        application.refresh_application_close_context();

        assert!(!application
            .window_mut(0)
            .evaluate_close_request_for_test(&context));

        let counts = application
            .window_mut(0)
            .pending_quit_counts_for_test()
            .expect("the secondary SSH session must be included in primary quit");
        assert_eq!(counts.ssh, 1);
    }

    #[test]
    fn secondary_window_close_refuses_all_dirty_views_it_owns() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secondary-views.txt");
        std::fs::write(&path, "before\n").unwrap();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenAnotherEditorView, &context);
        let documents = application.window_mut(0).documents_for_test().clone();
        let document = documents.borrow().find_local(&path).unwrap();
        documents
            .borrow_mut()
            .get_mut(document)
            .unwrap()
            .text_mut()
            .sync_from_view("unsaved\n")
            .unwrap();

        assert!(!application
            .window_mut(1)
            .evaluate_close_request_for_test(&context));

        assert_eq!(
            application.window_mut(1).open_refusal_headline_for_test(),
            Some("Closing this window is waiting for unsaved documents")
        );
        assert_eq!(documents.borrow().get(document).unwrap().views(), 2);
        assert_eq!(application.window_count(), 2);
    }

    #[test]
    fn accepted_window_close_releases_its_last_document_views() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.txt");
        let second = directory.path().join("second.txt");
        std::fs::write(&first, "first\n").unwrap();
        std::fs::write(&second, "second\n").unwrap();
        application.window_mut(1).dispatch_for_test(
            AppCommand::OpenTextEditor {
                path: first.clone(),
            },
            &context,
        );
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenAnotherEditorView, &context);
        application.window_mut(1).dispatch_for_test(
            AppCommand::OpenTextEditor {
                path: second.clone(),
            },
            &context,
        );
        let documents = application.window_mut(0).documents_for_test().clone();
        let first_id = documents.borrow().find_local(&first).unwrap();
        assert_eq!(documents.borrow().get(first_id).unwrap().views(), 2);
        assert_eq!(documents.borrow().len(), 2);

        application.window_mut(1).accept_window_close_for_test();
        application.settle_windows(&context);

        assert_eq!(application.window_count(), 1);
        assert!(documents.borrow().is_empty());
        assert!(documents.borrow().find_local(&first).is_none());
        assert!(documents.borrow().find_local(&second).is_none());
    }

    #[test]
    fn accepted_window_close_preserves_a_sibling_document_and_its_undo() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.txt");
        std::fs::write(&path, "original\n").unwrap();
        for index in 0..2 {
            application
                .window_mut(index)
                .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        }
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenAnotherEditorView, &context);
        let documents = application.window_mut(0).documents_for_test().clone();
        let id = documents.borrow().find_local(&path).unwrap();
        assert_eq!(documents.borrow().get(id).unwrap().views(), 3);
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .text_mut()
            .replace(0..0, "unsaved ")
            .unwrap();

        application.window_mut(1).accept_window_close_for_test();
        application.settle_windows(&context);

        let mut registry = documents.borrow_mut();
        let document = registry.get_mut(id).unwrap();
        assert_eq!(document.views(), 1);
        assert_eq!(document.text().text(), "unsaved original\n");
        assert!(document.text().is_dirty());
        assert!(document.text_mut().undo());
        assert_eq!(document.text().text(), "original\n");
        assert!(document.text_mut().redo());
        assert_eq!(document.text().text(), "unsaved original\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
    }

    #[test]
    fn window_teardown_does_not_release_already_closed_document_views() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("shared.txt");
        std::fs::write(&path, "original\n").unwrap();
        for index in 0..2 {
            application
                .window_mut(index)
                .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        }
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenAnotherEditorView, &context);
        let documents = application.window_mut(0).documents_for_test().clone();
        let id = documents.borrow().find_local(&path).unwrap();
        let closed = application.window_mut(1).active_tab_id_for_test();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::CloseTab(closed), &context);
        assert_eq!(documents.borrow().get(id).unwrap().views(), 2);

        application.window_mut(1).accept_window_close_for_test();
        application.settle_windows(&context);

        assert_eq!(documents.borrow().get(id).unwrap().views(), 1);
        let last = application.window_mut(0).active_tab_id_for_test();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::CloseTab(last), &context);
        assert!(documents.borrow().is_empty());
    }

    #[test]
    fn moving_an_editor_out_of_a_closing_window_keeps_its_document_registered() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let launcher = application.window_mut(1).active_tab_id_for_test();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("moved.txt");
        std::fs::write(&path, "moved\n").unwrap();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        let moved = application.window_mut(1).active_tab_id_for_test();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::CloseTab(launcher), &context);
        let documents = application.window_mut(0).documents_for_test().clone();
        let id = documents.borrow().find_local(&path).unwrap();
        let target = application.windows[0].id.viewport_id();
        application.window_mut(1).dispatch_for_test(
            AppCommand::MoveTabToWindow {
                moved,
                target: Some(target),
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
        assert_eq!(documents.borrow().get(id).unwrap().views(), 1);
        assert_eq!(documents.borrow().get(id).unwrap().text().text(), "moved\n");
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::CloseTab(moved), &context);
        assert!(documents.borrow().is_empty());
    }

    #[test]
    fn application_teardown_releases_all_remaining_document_views() {
        let (mut application, context) = application();
        application.open_window(&context, None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quit.txt");
        std::fs::write(&path, "quit\n").unwrap();
        for index in 0..2 {
            application
                .window_mut(index)
                .dispatch_for_test(AppCommand::OpenTextEditor { path: path.clone() }, &context);
        }
        let documents = application.window_mut(0).documents_for_test().clone();
        let id = documents.borrow().find_local(&path).unwrap();
        assert_eq!(documents.borrow().get(id).unwrap().views(), 2);

        drop(application);

        assert!(documents.borrow().is_empty());
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
            .broadcast_for_test(configuration_without_compact_launcher_grid());
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

    /// Opens a tab that may legitimately move between windows (ADR 0033).
    ///
    /// Launcher, Settings, and Profiles are per-window singletons that stay
    /// put, so a move test needs real per-tab content; a text document is the
    /// one such tab that needs neither a PTY nor a network.
    fn open_movable_tab(
        application: &mut FesTermApplication,
        window: usize,
        path: &str,
        context: &egui::Context,
    ) -> crate::tabs::TabId {
        let file = movable_tab_file(path);
        application
            .window_mut(window)
            .dispatch_for_test(AppCommand::OpenTextEditor { path: file }, context);
        application.window_mut(window).active_tab_id_for_test()
    }

    /// A real file for `open_movable_tab`, because a document that cannot be
    /// read opens no tab to move.
    fn movable_tab_file(path: &str) -> std::path::PathBuf {
        let name = std::path::Path::new(path)
            .file_name()
            .expect("a file name")
            .to_owned();
        let directory = std::env::temp_dir().join(format!(
            "festerm-movable-tab-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join(name);
        std::fs::write(&file, "# Moved\n").unwrap();
        file
    }

    /// The whole point of ADR 0033: the dragged tab itself - with whatever
    /// session it owns - ends up in the other window, rather than a new tab
    /// being opened there.
    #[test]
    fn a_tab_dropped_on_another_window_moves_into_it() {
        let (mut application, context) = application();
        let moved = open_movable_tab(&mut application, 0, "/docs/moved.md", &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenWindow, &context);
        application.settle_windows(&context);
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
        let launcher = application.window_mut(1).active_tab_id_for_test();
        let moved = open_movable_tab(&mut application, 1, "/docs/only.md", &context);
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::CloseTab(launcher), &context);
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
        let launcher = application.window_mut(0).active_tab_id_for_test();
        let moved = open_movable_tab(&mut application, 0, "/docs/last.md", &context);
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::CloseTab(launcher), &context);
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
        let moved = open_movable_tab(&mut application, 0, "/docs/detached.md", &context);
        application.window_mut(0).set_window_geometry_for_test(
            festerm_config::WorkspaceWindowGeometry::new(0.0, 0.0, 640.0, 480.0),
        );

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
        let launcher = application.window_mut(1).active_tab_id_for_test();
        let moved = open_movable_tab(&mut application, 1, "/docs/only.md", &context);
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::CloseTab(launcher), &context);

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

    /// Launcher, Settings, and Profiles are per-window singletons every
    /// window opens for itself (ADR 0033): moving one would only strip its
    /// source window of the surface it was showing, so the move is refused
    /// wherever it is requested from.
    #[test]
    fn a_singleton_application_surface_never_moves_between_windows() {
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
            application
                .window_mut(0)
                .tab_ids_for_test()
                .contains(&moved),
            "Settings must stay in the window that opened it"
        );
        assert!(!application
            .window_mut(1)
            .tab_ids_for_test()
            .contains(&moved));
    }

    /// The same refusal covers detaching one into a window of its own.
    #[test]
    fn a_singleton_application_surface_never_detaches_into_a_new_window() {
        let (mut application, context) = application();
        application
            .window_mut(0)
            .dispatch_for_test(AppCommand::OpenProfiles, &context);
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

        assert_eq!(application.window_count(), 1);
        assert!(application
            .window_mut(0)
            .tab_ids_for_test()
            .contains(&moved));
    }

    /// A window may only *request* a move, and only for a tab it owns; a
    /// stale or foreign identifier must not disturb anyone's tabs.
    #[test]
    fn a_move_to_a_window_that_no_longer_exists_keeps_the_tab_where_it_is() {
        let (mut application, context) = application();
        let moved = open_movable_tab(&mut application, 0, "/docs/stale.md", &context);

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
        let revision = application.presentation_workspace.current().unwrap();
        let active = application.window_mut(1).active_tab_id_for_test();
        let other = application
            .window_mut(1)
            .tab_ids_for_test()
            .into_iter()
            .find(|tab| *tab != active)
            .unwrap();
        application
            .window_mut(1)
            .dispatch_for_test(AppCommand::ActivateTab(other), &context);
        assert!(application.presentation_workspace.current().unwrap() > revision);
    }

    /// The compact Launcher layout is on by default, so the change a sibling
    /// window has to notice is turning it *off*.
    fn configuration_without_compact_launcher_grid() -> Configuration {
        Configuration::empty()
            .with_interface_settings(InterfaceSettings::DEFAULT.with_compact_launcher_grid(false))
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
