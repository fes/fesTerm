//! Confirmation and modal-dialog subsystem for [`FesTermApp`]: the aggregate
//! quit confirmation, per-tab close confirmation, large-paste confirmation,
//! file-drop insertion confirmation, and interface-settings reset
//! confirmation. Each dialog follows the same shape (check pending overlay
//! state, render an `egui::Modal`, mutate overlay state, dispatch a
//! follow-up action) but has distinct button sets and side effects, so they
//! are kept as siblings rather than forced through one generic renderer.

use eframe::egui;
use festerm_config::InterfaceSettings;
use festerm_ui_egui::theme;

use crate::overlay_state::{
    AfterDocumentClose, CloseConsequence, PendingCloseConfirmation,
    PendingDocumentCloseConfirmation, PendingQuitConfirmation, QuitConfirmationPurpose,
};
use crate::tabs::{AppCommand, InspectorTransport, TabContent, TabId};

use super::{bounded_paste_preview, confirmation_width, paste_line_count, FesTermApp};

/// The height of a dialog's row of buttons. Stated rather than measured so a
/// centred layout has something to centre against.
const DIALOG_BUTTON_HEIGHT: f32 = 28.0;

impl FesTermApp {
    /// Applies the one close policy shared by chrome, shortcuts, the command
    /// palette, native menus, and session overlays. Non-live surfaces close
    /// immediately; a live transport is confirmed when that preference is
    /// enabled and closes directly otherwise.
    pub(super) fn request_close_tab(&mut self, id: TabId, context: &egui::Context) {
        // Unsaved text is asked about before anything else and is never gated
        // by the session-close preference: ending a session the user asked to
        // end is not the same as throwing away what they typed.
        if self.request_document_close(id, AfterDocumentClose::CloseTab) {
            return;
        }
        let confirmation = if self.state.confirm_session_close() {
            self.state
                .tabs()
                .iter()
                .find(|tab| tab.id == id)
                .and_then(|tab| {
                    let TabContent::Session(session) = &tab.content else {
                        return None;
                    };
                    session
                        .close_requires_confirmation()
                        .then(|| PendingCloseConfirmation {
                            tab: id,
                            identity: session.label.clone(),
                            consequence: match session.inspector_transport {
                                InspectorTransport::Local { .. } => {
                                    CloseConsequence::TerminateLocalProcess
                                }
                                InspectorTransport::Ssh { .. }
                                | InspectorTransport::Sftp { .. } => {
                                    CloseConsequence::DisconnectSsh
                                }
                                InspectorTransport::Serial { .. } => {
                                    CloseConsequence::TerminateLocalProcess
                                }
                            },
                            lifecycle_generation: session.controller.lifecycle_generation(),
                            restore_tab: self.state.active(),
                            cancel_focus_requested: false,
                        })
                })
        } else {
            None
        };
        if let Some(confirmation) = confirmation {
            self.palette.close();
            self.overlays.pending_close = Some(confirmation);
        } else {
            self.state.dispatch(AppCommand::CloseTab(id), context);
        }
    }

    /// Intercepts the OS "close this window" request and, if any session
    /// still has something to lose, cancels it and shows one aggregate
    /// confirmation instead of letting the window disappear silently
    /// (`docs/gui-design.md` "Closing sessions and quitting",
    /// `docs/gui-action-graph.md` `QUIT-01`/`QUIT-02`). fesTerm has exactly
    /// one native window, so the same path covers both the window's close
    /// button and "Quit fesTerm" - both arrive here as the same
    /// `close_requested` viewport event.
    ///
    /// Split from `logic()`'s real `ctx.input` read so tests can drive it
    /// directly without needing a way to fabricate a genuine close-request
    /// input event on a headless `egui::Context`.
    pub(super) fn evaluate_close_request(&mut self, context: &egui::Context) {
        if self.native_smoke.is_some() {
            // Native smoke owns the window lifecycle and writes its result
            // before requesting deterministic teardown. Interactive quit
            // confirmation must not cancel that automation-owned close.
            return;
        }
        if self.quit_confirmed {
            // Already deliberately confirmed: let the follow-up close proceed.
            return;
        }
        if let Some(pending) = self.overlays.pending_quit {
            // A second close while the ordinary Quit dialog is showing is the
            // platform's follow-up teardown request. An update-consent dialog
            // has not authorized quitting, so preserve the sessions instead.
            if pending.purpose == QuitConfirmationPurpose::InstallUpdate {
                context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
            return;
        }
        // Unsaved text goes first: a session's scrollback can be reproduced,
        // a document's unsaved changes cannot. Each dirty document is asked
        // about in turn, and the close resumes only once every one of them has
        // been answered for (ADR 0034 §7).
        if let Some((tab, _)) = self.state.dirty_document_closes().into_iter().next() {
            let purpose = if self.role == crate::app::WindowRole::Secondary {
                QuitConfirmationPurpose::CloseWindow
            } else {
                QuitConfirmationPurpose::Quit
            };
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_document_close(tab, AfterDocumentClose::ResumeClose(purpose));
            return;
        }
        let counts = self.state.live_session_counts();
        if counts.total() == 0 {
            // Nothing would be lost: let the close proceed untouched.
            return;
        }
        // Closing an additional window (ADR 0033) ends that window's own
        // sessions and nothing else, which is the very action the
        // "confirm before closing a live session" preference governs - so
        // it is honoured here. Closing the primary window quits the
        // application outright and keeps its unconditional confirmation:
        // that discards every window's work at once, which no per-session
        // preference was ever asked about.
        let purpose = if self.role == crate::app::WindowRole::Secondary {
            if !self.state.confirm_session_close() {
                return;
            }
            QuitConfirmationPurpose::CloseWindow
        } else {
            QuitConfirmationPurpose::Quit
        };
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.overlays.pending_quit = Some(PendingQuitConfirmation {
            counts,
            cancel_focus_requested: false,
            purpose,
        });
    }

    pub(super) fn request_update_install(&mut self) {
        let counts = self.state.live_session_counts();
        if counts.total() == 0 {
            self.update_restart_authorized = true;
            self.updates.begin_install();
            return;
        }
        self.overlays.pending_quit = Some(PendingQuitConfirmation {
            counts,
            cancel_focus_requested: false,
            purpose: QuitConfirmationPurpose::InstallUpdate,
        });
    }

    pub(super) fn show_close_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.overlays.pending_close.as_ref().cloned() else {
            return;
        };
        let still_live = self
            .state
            .tabs()
            .iter()
            .find(|tab| tab.id == pending.tab)
            .is_some_and(|tab| {
                matches!(&tab.content, TabContent::Session(session)
                if session.close_requires_confirmation()
                    && session.controller.lifecycle_generation() == pending.lifecycle_generation
                    && matches!(
                        (&session.inspector_transport, pending.consequence),
                        (InspectorTransport::Local { persistence: None }, CloseConsequence::TerminateLocalProcess)
                            | (InspectorTransport::Ssh { .. }, CloseConsequence::DisconnectSsh)
                            | (InspectorTransport::Sftp { .. }, CloseConsequence::DisconnectSsh)
                            | (InspectorTransport::Serial { .. }, CloseConsequence::TerminateLocalProcess)
                    ))
            });
        if !still_live {
            self.cancel_close_confirmation();
            return;
        }

        let mut cancel = escape;
        let mut confirm = false;
        egui::Modal::new(egui::Id::new("close_session_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 360.0));
                ui.heading(format!("Close \u{201c}{}\u{201d}?", pending.identity));
                ui.add_space(6.0);
                ui.label(pending.consequence.message());
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let cancel_button = ui.button("Cancel");
                    if !pending.cancel_focus_requested {
                        cancel_button.request_focus();
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Close Session").color(theme::STATUS_ERROR),
                        ))
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        if let Some(current) = self.overlays.pending_close.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_close_confirmation();
        } else if confirm {
            self.overlays.pending_close = None;
            self.state
                .dispatch(AppCommand::CloseTab(pending.tab), context);
        }
    }

    pub(super) fn cancel_close_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_close.take() else {
            return;
        };
        // Popup/menu widget IDs can disappear in the frame that opens the
        // dialog. Restore the active surface, not a stale invoker node.
        if let Some(session) = self.state.session_tab_mut(pending.restore_tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    /// Raises the final-view dirty-close prompt for a tab, and reports
    /// whether it did. A tab that is not a view of a document, or is one of
    /// several views, or whose document has nothing unsaved, closes without
    /// being asked about (ADR 0034 §7).
    fn request_document_close(&mut self, id: TabId, then: AfterDocumentClose) -> bool {
        let Some(consequence) = self.state.document_close_consequence(id) else {
            return false;
        };
        self.palette.close();
        self.overlays.pending_document_close = Some(PendingDocumentCloseConfirmation {
            tab: id,
            document: consequence.document,
            title: consequence.title,
            origin: consequence.origin,
            restore_tab: self.state.active(),
            save_focus_requested: false,
            then,
        });
        true
    }

    /// The final-view dirty-close prompt (ADR 0034 §7).
    ///
    /// Save is the default and the focused action, Escape is Cancel, and
    /// Discard is worded as what it does and never takes the Return key. The
    /// document's fully qualified origin is shown under the question, because
    /// this prompt can be raised from a background window or by a quit closing
    /// several documents at once, and a prompt that only said "Save changes?"
    /// could be answered for the wrong file.
    pub(super) fn show_document_close_confirmation(
        &mut self,
        context: &egui::Context,
        escape: bool,
    ) {
        let Some(pending) = self.overlays.pending_document_close.as_ref().cloned() else {
            return;
        };
        // Revalidated every frame rather than trusted from when it opened: a
        // sibling view saving the document, or the tab going away underneath
        // the prompt, means there is nothing left to ask about.
        if self.state.document_close_consequence(pending.tab).as_ref()
            != Some(&crate::tabs::DirtyDocumentClose {
                document: pending.document,
                title: pending.title.clone(),
                origin: pending.origin.clone(),
            })
        {
            self.overlays.pending_document_close = None;
            self.state
                .dispatch(AppCommand::CloseTab(pending.tab), context);
            self.continue_document_close(pending.then, context);
            return;
        }

        let mut cancel = escape;
        let mut discard = false;
        let mut save = false;
        egui::Modal::new(egui::Id::new("document_close_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(160))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 560.0));
                ui.add_space(4.0);
                ui.heading(format!("Save changes to {}?", pending.title));
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(12.0);
                ui.label("This is the last view of this document. Unsaved changes will be lost.");
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new(&pending.origin)
                        .monospace()
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(24.0);
                // The row is given its height rather than asked for one. A
                // vertically centred right-to-left layout inside a dialog
                // that has no height of its own has nothing to centre
                // against, and grows by a few points every frame: the prompt
                // creeps taller until its question has scrolled off the top
                // of the screen and only the buttons are left.
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), DIALOG_BUTTON_HEIGHT),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let save_button = ui.add(
                            egui::Button::new(
                                egui::RichText::new("Save")
                                    .strong()
                                    .color(theme::TEXT_ON_ACCENT),
                            )
                            .fill(theme::ACCENT_ACTION)
                            .min_size(egui::vec2(84.0, 28.0)),
                        );
                        if !pending.save_focus_requested {
                            save_button.request_focus();
                        }
                        // Which action Return reaches is drawn as a ring, not as
                        // a fill colour, so the default is still obvious to a
                        // reader who cannot tell the two buttons apart by hue.
                        if save_button.has_focus() {
                            ui.painter().rect_stroke(
                                save_button.rect.expand(2.0),
                                6.0,
                                egui::Stroke::new(2.0, theme::BORDER_ACTIVE),
                                egui::StrokeKind::Outside,
                            );
                        }
                        if save_button.clicked() {
                            save = true;
                        }
                        ui.add_space(8.0);
                        // Bordered rather than bare, so the destructive action is
                        // as plainly a button as the one beside it. Its weight and
                        // its heavier border, not its colour, are what mark it as
                        // the one that throws work away.
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Discard changes")
                                        .strong()
                                        .color(theme::STATUS_ERROR),
                                )
                                .stroke(egui::Stroke::new(1.5, theme::STATUS_ERROR))
                                .min_size(egui::vec2(132.0, 28.0)),
                            )
                            .clicked()
                        {
                            discard = true;
                        }
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .stroke(egui::Stroke::new(1.0, theme::BORDER_SUBTLE))
                                    .min_size(egui::vec2(84.0, 28.0)),
                            )
                            .clicked()
                        {
                            cancel = true;
                        }
                    },
                );
                ui.add_space(4.0);
            });
        if let Some(current) = self.overlays.pending_document_close.as_mut() {
            current.save_focus_requested = true;
        }

        if cancel {
            self.overlays.pending_document_close = None;
            // Popup and menu widget IDs can disappear in the frame that opens
            // a dialog, so focus is put back on the surface that was active
            // rather than left on a stale node.
            self.state
                .dispatch(AppCommand::ActivateTab(pending.restore_tab), context);
            return;
        }
        if save {
            self.overlays.pending_document_close = None;
            match self.state.save_document(pending.document) {
                Some(festerm_document::SaveOutcome::Saved) => {
                    self.state
                        .dispatch(AppCommand::CloseTab(pending.tab), context);
                    self.continue_document_close(pending.then, context);
                }
                // The write did not happen, so neither does the close: the
                // banner now says why, and the user still has their text.
                _ => self
                    .state
                    .dispatch(AppCommand::ActivateTab(pending.tab), context),
            }
            return;
        }
        if discard {
            self.overlays.pending_document_close = None;
            self.state
                .dispatch(AppCommand::CloseTab(pending.tab), context);
            self.continue_document_close(pending.then, context);
        }
    }

    /// Says why a file the reader picked could not be opened. Without it the
    /// picker simply closes on a binary or oversized file and the click looks
    /// like it missed (ADR 0034 §2).
    pub(super) fn show_open_refusal_notice(&mut self, context: &egui::Context, escape: bool) {
        let Some(notice) = self.overlays.open_refusal.as_ref() else {
            return;
        };
        let (name, path, headline, detail) = (
            notice.name.clone(),
            notice.path.clone(),
            notice.headline.clone(),
            notice.detail.clone(),
        );
        let mut dismiss = escape;
        egui::Modal::new(egui::Id::new("open_refusal_notice"))
            .backdrop_color(egui::Color32::from_black_alpha(160))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 560.0));
                ui.add_space(4.0);
                ui.heading(format!("Cannot open {name}"));
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(12.0);
                ui.label(headline);
                ui.add_space(8.0);
                ui.label(egui::RichText::new(detail).color(theme::TEXT_SECONDARY));
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new(path)
                        .monospace()
                        .size(12.0)
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(24.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), DIALOG_BUTTON_HEIGHT),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        let button = ui.add(
                            egui::Button::new(
                                egui::RichText::new("OK")
                                    .strong()
                                    .color(theme::TEXT_ON_ACCENT),
                            )
                            .fill(theme::ACCENT_ACTION)
                            .min_size(egui::vec2(84.0, DIALOG_BUTTON_HEIGHT)),
                        );
                        if !self.overlays.open_refusal_focused {
                            button.request_focus();
                        }
                        if button.clicked() {
                            dismiss = true;
                        }
                    },
                );
                ui.add_space(4.0);
            });
        self.overlays.open_refusal_focused = true;
        if dismiss {
            self.overlays.open_refusal = None;
            self.overlays.open_refusal_focused = false;
        }
    }

    /// Moves a window close or quit on to the next document holding unsaved
    /// changes, and lets the close proceed once none are left.
    fn continue_document_close(&mut self, then: AfterDocumentClose, context: &egui::Context) {
        let AfterDocumentClose::ResumeClose(purpose) = then else {
            return;
        };
        if let Some((tab, _)) = self.state.dirty_document_closes().into_iter().next() {
            self.request_document_close(tab, AfterDocumentClose::ResumeClose(purpose));
            return;
        }
        // Re-requesting the close rather than confirming it here: the session
        // confirmation this interrupted has still not been asked, and the
        // close request is the one place that decides whether it needs to be.
        context.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Renders the one aggregate confirmation for closing the window while
    /// live sessions remain (`docs/gui-design.md` "Closing sessions and
    /// quitting"). Revalidates the live counts every frame rather than
    /// trusting the snapshot captured when the dialog opened, so a session
    /// that exits on its own while the dialog is showing is reflected
    /// immediately, and the dialog closes itself if none remain.
    pub(super) fn show_quit_confirmation(&mut self, context: &egui::Context, escape: bool) {
        if self.overlays.pending_quit.is_none() {
            return;
        }
        let counts = self.state.live_session_counts();
        if counts.total() == 0 {
            self.overlays.pending_quit = None;
            return;
        }
        if let Some(pending) = self.overlays.pending_quit.as_mut() {
            pending.counts = counts;
        }
        let pending = *self.overlays.pending_quit.as_ref().expect("checked above");

        let mut cancel = escape;
        let mut confirm = false;
        egui::Modal::new(egui::Id::new("quit_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 360.0));
                let (heading, consequence, confirm_label) = match pending.purpose {
                    QuitConfirmationPurpose::Quit => (
                        "Quit fesTerm?",
                        "Unsaved terminal history will be discarded.",
                        "Quit fesTerm",
                    ),
                    QuitConfirmationPurpose::CloseWindow => (
                        "Close this window?",
                        "This window's unsaved terminal history will be discarded.",
                        "Close Window",
                    ),
                    QuitConfirmationPurpose::InstallUpdate => (
                        "Install update and restart fesTerm?",
                        "The update will close every session after installation succeeds.",
                        "Install and Restart",
                    ),
                };
                ui.heading(heading);
                ui.add_space(6.0);
                ui.label(pending.summary_message());
                ui.label(consequence);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let cancel_button = ui.button("Cancel");
                    if !pending.cancel_focus_requested {
                        cancel_button.request_focus();
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new(confirm_label).color(theme::STATUS_ERROR),
                        ))
                        .clicked()
                    {
                        confirm = true;
                    }
                });
            });
        if let Some(current) = self.overlays.pending_quit.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.overlays.pending_quit = None;
        } else if confirm {
            self.overlays.pending_quit = None;
            match pending.purpose {
                QuitConfirmationPurpose::Quit | QuitConfirmationPurpose::CloseWindow => {
                    self.quit_confirmed = true;
                    context.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                QuitConfirmationPurpose::InstallUpdate => {
                    self.update_restart_authorized = true;
                    self.updates.begin_install();
                }
            }
        }
    }

    pub(super) fn cancel_paste_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_paste.take() else {
            return;
        };
        if let Some(session) = self.state.session_tab_mut(pending.tab) {
            if let Some(token) = pending.clipboard_token {
                session
                    .controller
                    .cancel_clipboard_input(token, "discarded-clipboard-cancelled");
            }
            session.view.request_focus_on_next_frame();
        }
        self.show_clipboard_discard_notice(pending.tab);
    }

    pub(super) fn show_file_drop_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.overlays.pending_file_drop.as_ref().cloned() else {
            return;
        };
        let valid_target = self.state.active() == pending.tab
            && self
                .state
                .session_tab_mut(pending.tab)
                .is_some_and(|session| {
                    session.accepts_input()
                        && session.controller.lifecycle_generation() == pending.lifecycle_generation
                        && matches!(
                            session.inspector_transport,
                            InspectorTransport::Local { .. }
                        )
                });
        if !valid_target {
            self.cancel_file_drop_confirmation();
            return;
        }

        let (preview, _shown_lines, shown_characters) = bounded_paste_preview(&pending.text);
        let omitted_characters = pending
            .text
            .chars()
            .count()
            .saturating_sub(shown_characters);
        let mut cancel = escape;
        let mut insert = false;
        let noun = if pending.path_count == 1 {
            "path"
        } else {
            "paths"
        };
        egui::Modal::new(egui::Id::new("file_drop_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 440.0));
                ui.heading(format!(
                    "Insert {} {noun} into \u{201c}{}\u{201d}?",
                    pending.path_count, pending.identity
                ));
                ui.label(
                    "The exact path text below will be inserted as typed input; no Enter is sent \
                     and no file contents are read.",
                );
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(180.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(preview).monospace())
                                    .selectable(true)
                                    .wrap(),
                            );
                        });
                });
                if omitted_characters > 0 {
                    ui.label(format!("Preview omits {omitted_characters} characters."));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let cancel_button = ui.button("Cancel");
                    if !pending.cancel_focus_requested {
                        cancel_button.request_focus();
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                    if ui.button("Insert Path").clicked() {
                        insert = true;
                    }
                });
            });
        if let Some(current) = self.overlays.pending_file_drop.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_file_drop_confirmation();
        } else if insert {
            self.overlays.pending_file_drop = None;
            if let Some(session) = self.state.session_tab_mut(pending.tab) {
                let _ = festerm_ui_egui::route_input(
                    &mut session.terminal,
                    festerm_core::InputEvent::Paste(pending.text),
                    &mut session.controller,
                );
            }
        }
    }

    pub(super) fn cancel_file_drop_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_file_drop.take() else {
            return;
        };
        if let Some(session) = self.state.session_tab_mut(pending.tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    pub(super) fn show_paste_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.overlays.pending_paste.as_ref().cloned() else {
            return;
        };
        let opening_frame = pending.clipboard_token.is_some()
            && pending.opened_frame == context.cumulative_frame_nr();
        let valid_target = self.state.active() == pending.tab
            && self.state.input_ownership_epoch() == pending.input_ownership_epoch
            && self
                .state
                .session_tab_mut(pending.tab)
                .is_some_and(|session| {
                    session.accepts_input()
                        && pending
                            .clipboard_token
                            .is_none_or(|token| session.controller.clipboard_input_pending(token))
                        && session.controller.lifecycle_generation() == pending.lifecycle_generation
                        && session.terminal.modes().bracketed_paste() == pending.bracketed_paste
                        && session.status_bar_label() == pending.transport_state
                });
        if !valid_target {
            self.cancel_paste_confirmation();
            return;
        }

        let line_count = paste_line_count(&pending.text);
        let character_count = pending.text.chars().count();
        let (preview, shown_lines, shown_characters) = bounded_paste_preview(&pending.text);
        let omitted_lines = line_count.saturating_sub(shown_lines);
        let omitted_characters = character_count.saturating_sub(shown_characters);
        let mut cancel = escape;
        let mut paste = false;
        egui::Modal::new(egui::Id::new("paste_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                if opening_frame {
                    ui.disable();
                }
                ui.set_width(confirmation_width(context.content_rect().width(), 440.0));
                let unit = if line_count == 1 { "line" } else { "lines" };
                ui.heading(format!(
                    "Paste {line_count} {unit} into \u{201c}{}\u{201d}?",
                    pending.identity
                ));
                if !pending.bracketed_paste {
                    ui.label("Bracketed paste is not active; a line may execute immediately.");
                } else {
                    ui.label("This large paste will be sent as one bracketed input operation.");
                }
                ui.label(format!(
                    "Target state: {} \u{00b7} {line_count} {unit} \u{00b7} {character_count} characters",
                    pending.transport_state
                ));
                if pending.clipboard_token.is_some() {
                    ui.label("Waiting keyboard input follows Paste; Cancel discards it.");
                }
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(preview).monospace())
                                .selectable(true)
                                .wrap(),
                        );
                    });
                });
                if omitted_lines > 0 || omitted_characters > 0 {
                    ui.label(format!(
                        "Preview omits {omitted_lines} lines and {omitted_characters} characters."
                    ));
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let cancel_button = ui.button("Cancel");
                    if !pending.cancel_focus_requested && !opening_frame {
                        cancel_button.request_focus();
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                    if ui.button("Paste").clicked() {
                        paste = true;
                    }
                });
            });
        if let Some(current) = self.overlays.pending_paste.as_mut() {
            current.cancel_focus_requested = !opening_frame;
        }
        if opening_frame {
            context.request_repaint();
        }
        if cancel {
            self.cancel_paste_confirmation();
        } else if paste {
            self.overlays.pending_paste = None;
            self.deliver_ordered_paste(pending.tab, pending.text, pending.clipboard_token, context);
        }
    }

    pub(super) fn show_settings_reset_confirmation(
        &mut self,
        context: &egui::Context,
        escape: bool,
    ) {
        let Some(pending) = self.overlays.pending_settings_reset.as_ref().cloned() else {
            return;
        };
        if self.state.interface_settings() == InterfaceSettings::DEFAULT {
            self.cancel_settings_reset_confirmation();
            return;
        }

        let mut cancel = escape;
        let mut confirm = false;
        egui::Modal::new(egui::Id::new("reset_interface_settings_confirmation"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(confirmation_width(context.content_rect().width(), 360.0));
                ui.heading("Reset interface settings?");
                ui.add_space(6.0);
                ui.label(
                    "Interface layout, workspace behavior, and terminal typography will return \
                     to their defaults.",
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let cancel_button = ui.button("Cancel");
                    if !pending.cancel_focus_requested {
                        cancel_button.request_focus();
                    }
                    if cancel_button.clicked() {
                        cancel = true;
                    }
                    if ui.button("Reset").clicked() {
                        confirm = true;
                    }
                });
            });
        if let Some(current) = self.overlays.pending_settings_reset.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_settings_reset_confirmation();
        } else if confirm {
            self.overlays.pending_settings_reset = None;
            self.state
                .dispatch(AppCommand::ResetInterfaceSettings, context);
            self.reinstall_terminal_font(context);
            self.persist_interface_settings();
        }
    }

    pub(super) fn cancel_settings_reset_confirmation(&mut self) {
        self.overlays.pending_settings_reset = None;
    }
}
