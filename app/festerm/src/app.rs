use std::{
    sync::{mpsc, Arc},
    thread,
    time::Duration,
};

use eframe::egui;
use festerm_config::Configuration;
use festerm_pty::LocalProfile;
#[cfg(test)]
use festerm_secret_store::MemorySecretStore;
use festerm_secret_store::{native_store, SecretReference, SecretStore, SecretStoreError};
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};
use festerm_ui_egui::overlay::{self, OverlayAction};
use festerm_ui_egui::palette::{self, PaletteItem, PaletteState};
use festerm_ui_egui::theme;

use crate::configuration_startup::{
    ConfigurationReloader, ConfigurationStartupStatus, StartupConfiguration,
};
use crate::inspector::{InspectorAction, InspectorContent, TransportFacts};
use crate::native_smoke::NativeWindowSmoke;
use crate::screens;
use crate::tabs::{
    AppCommand, AppState, HostKeyTrustDecision, InspectorTransport, TabContent, TabId,
};

const APPLICATION_TITLE: &str = "fesTerm";
const LARGE_PASTE_CHARACTER_THRESHOLD: usize = 4_096;
const LARGE_PASTE_LINE_THRESHOLD: usize = 100;
const PASTE_PREVIEW_CHARACTER_LIMIT: usize = 800;
const PASTE_PREVIEW_LINE_LIMIT: usize = 8;

#[derive(Clone, Copy)]
enum ApplicationShortcut {
    CommandPalette,
    NewSession,
    CloseActiveSurface,
    NextSession,
    PreviousSession,
    Settings,
}

impl ApplicationShortcut {
    fn chord(self) -> Option<(egui::Modifiers, egui::Key)> {
        match self {
            Self::CommandPalette => Some((
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::P,
            )),
            Self::NewSession => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
                },
                egui::Key::T,
            )),
            Self::CloseActiveSurface => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
                },
                egui::Key::W,
            )),
            // Ctrl+Tab remains fesTerm session navigation on every platform;
            // Cmd+Tab belongs to the macOS application switcher.
            Self::NextSession => Some((egui::Modifiers::CTRL, egui::Key::Tab)),
            Self::PreviousSession => Some((
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                egui::Key::Tab,
            )),
            Self::Settings if cfg!(target_os = "macos") => {
                Some((egui::Modifiers::COMMAND, egui::Key::Comma))
            }
            Self::Settings => None,
        }
    }

    const fn label(self) -> Option<&'static str> {
        match self {
            Self::CommandPalette if cfg!(target_os = "macos") => Some("Cmd+Shift+P"),
            Self::CommandPalette => Some("Ctrl+Shift+P"),
            Self::NewSession if cfg!(target_os = "macos") => Some("Cmd+T"),
            Self::NewSession => Some("Ctrl+Shift+T"),
            Self::CloseActiveSurface if cfg!(target_os = "macos") => Some("Cmd+W"),
            Self::CloseActiveSurface => Some("Ctrl+Shift+W"),
            Self::NextSession => Some("Ctrl+Tab"),
            Self::PreviousSession => Some("Ctrl+Shift+Tab"),
            Self::Settings if cfg!(target_os = "macos") => Some("Cmd+,"),
            Self::Settings => None,
        }
    }

    fn consume(self, context: &egui::Context) -> bool {
        self.chord().is_some_and(|(modifiers, key)| {
            context.input_mut(|input| input.consume_key(modifiers, key))
        })
    }
}

/// Composition root.
///
/// `AppState` owns the always-nonempty tab collection and session/command
/// policy (`docs/application-command-model.md`); this struct wires it to the
/// `eframe` event loop, the top-of-window chrome
/// (`crates/festerm-ui-egui/src/chrome.rs`), and the native-window smoke
/// driver.
pub struct FesTermApp {
    state: AppState,
    /// The deterministic local tab created only for native-window smoke. The
    /// ordinary no-workspace product path starts at Launcher instead.
    primary_tab: Option<TabId>,
    window_title: String,
    native_smoke: Option<NativeWindowSmoke>,
    palette: PaletteState,
    configuration_status: ConfigurationStartupStatus,
    configuration_reloader: ConfigurationReloader,
    /// Composition-owned native-store factory result. Failure is retained as a
    /// content-free status so local sessions and the rest of the app stay
    /// available.
    secret_store: Result<Arc<dyn SecretStore>, SecretStoreError>,
    pending_password_store: Option<PendingPasswordStore>,
    secure_storage_feedback: Option<&'static str>,
    /// Widget that owned focus immediately before Inspector opened, when it
    /// remains a meaningful restoration target.
    inspector_restore_focus: Option<egui::Id>,
    rename_restore_focus: Option<egui::Id>,
    rename_restore_tab: Option<TabId>,
    pending_close: Option<PendingCloseConfirmation>,
    pending_paste: Option<PendingPasteConfirmation>,
    native_menu: festerm_macos_window::NativeMenu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseConsequence {
    TerminateLocalProcess,
    DisconnectSsh,
}

impl CloseConsequence {
    const fn message(self) -> &'static str {
        match self {
            Self::TerminateLocalProcess => {
                "The local process will be terminated and its terminal history discarded."
            }
            Self::DisconnectSsh => {
                "The SSH connection will be disconnected and its terminal history discarded."
            }
        }
    }
}

#[derive(Clone, Debug)]
struct PendingCloseConfirmation {
    tab: TabId,
    identity: String,
    consequence: CloseConsequence,
    lifecycle_generation: u64,
    restore_tab: TabId,
    cancel_focus_requested: bool,
}

#[derive(Clone, Debug)]
struct PendingPasteConfirmation {
    tab: TabId,
    identity: String,
    text: String,
    transport_state: &'static str,
    lifecycle_generation: u64,
    bracketed_paste: bool,
    cancel_focus_requested: bool,
}

struct PendingPasswordStore {
    receiver: mpsc::Receiver<Result<SecretReference, SecretStoreError>>,
    profile_id: String,
    options: festerm_ssh::SshSessionOptions,
    store: Arc<dyn SecretStore>,
}

fn native_secret_store() -> Result<Arc<dyn SecretStore>, SecretStoreError> {
    native_store().map(Arc::<dyn SecretStore>::from)
}

fn secret_store_message(error: SecretStoreError) -> &'static str {
    match error {
        SecretStoreError::LockedOrUnavailable | SecretStoreError::Unsupported => {
            "Native secure storage is unavailable or locked. Unlock or enable it to use saved SSH passwords."
        }
        SecretStoreError::BackendFailure => {
            "Native secure storage failed. Saved SSH passwords are unavailable; try again after checking the platform service."
        }
        SecretStoreError::Missing | SecretStoreError::InvalidReference => {
            "Native secure storage could not use the requested saved SSH password."
        }
    }
}

fn normalize_paste_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn paste_line_count(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count() + 1
}

fn confirmation_width(viewport_width: f32, preferred: f32) -> f32 {
    (viewport_width - 32.0).clamp(240.0, preferred)
}

fn bounded_paste_preview(text: &str) -> (String, usize, usize) {
    let mut preview = String::new();
    let mut shown_characters = 0;
    let mut shown_lines = 1;
    for character in text.chars() {
        if shown_characters == PASTE_PREVIEW_CHARACTER_LIMIT {
            break;
        }
        if character == '\n' && shown_lines == PASTE_PREVIEW_LINE_LIMIT {
            break;
        }
        match character {
            '\n' => {
                preview.push('\n');
                shown_lines += 1;
            }
            '\t' => preview.push('\t'),
            control if control.is_control() => {
                preview.push_str(&format!("\\u{{{:04x}}}", control as u32));
            }
            visible => preview.push(visible),
        }
        shown_characters += 1;
    }
    (preview, shown_lines, shown_characters)
}

impl FesTermApp {
    /// Builds the application around explicitly supplied, already-validated
    /// profile and optional workspace metadata.
    pub fn with_configuration(context: &egui::Context, configuration: Configuration) -> Self {
        Self::with_configuration_status(context, configuration, ConfigurationStartupStatus::Missing)
    }

    pub(crate) fn with_startup_configuration(
        context: &egui::Context,
        startup_configuration: StartupConfiguration,
    ) -> Self {
        let (configuration, status, configuration_reloader) = startup_configuration.into_parts();
        let mut app = Self::with_configuration(context, configuration);
        app.configuration_status = status;
        app.configuration_reloader = configuration_reloader;
        app
    }

    fn with_configuration_status(
        context: &egui::Context,
        configuration: Configuration,
        configuration_status: ConfigurationStartupStatus,
    ) -> Self {
        Self::with_configuration_status_and_secret_store(
            context,
            configuration,
            configuration_status,
            native_secret_store(),
        )
    }

    fn with_configuration_status_and_secret_store(
        context: &egui::Context,
        configuration: Configuration,
        configuration_status: ConfigurationStartupStatus,
        secret_store: Result<Arc<dyn SecretStore>, SecretStoreError>,
    ) -> Self {
        // One semantic blue-graphite default for application surfaces and
        // widgets. Terminal ANSI and explicit RGB colors remain independent.
        context.set_visuals(theme::default_visuals());
        let native_smoke = NativeWindowSmoke::from_environment();
        let smoke_profile = native_smoke.as_ref().map(|smoke| {
            LocalProfile::new(smoke.test_child_path()).with_arguments(smoke.test_child_arguments())
        });
        let (state, primary_tab) = if let Some(workspace) = configuration.workspace().cloned() {
            (
                AppState::with_restored_workspace(context, configuration, &workspace),
                None,
            )
        } else if smoke_profile.is_some() {
            let (state, primary_tab) =
                AppState::with_primary_session(context, smoke_profile, configuration);
            (state, Some(primary_tab))
        } else {
            (AppState::with_launcher(configuration), None)
        };
        Self {
            state,
            primary_tab,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke,
            palette: PaletteState::default(),
            configuration_status,
            configuration_reloader: ConfigurationReloader::unavailable(),
            secret_store,
            pending_password_store: None,
            secure_storage_feedback: None,
            inspector_restore_focus: None,
            rename_restore_focus: None,
            rename_restore_tab: None,
            pending_close: None,
            pending_paste: None,
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
        }
    }

    pub(crate) fn install_native_menu(&mut self, context: &egui::Context) {
        let context = context.clone();
        self.native_menu =
            festerm_macos_window::install_application_menu(std::sync::Arc::new(move || {
                context.request_repaint()
            }));
    }

    fn handle_native_menu_commands(&mut self, context: &egui::Context) {
        if self.pending_close.is_some() || self.pending_paste.is_some() {
            return;
        }
        while let Some(command) = self.native_menu.try_recv() {
            use festerm_macos_window::NativeMenuCommand;
            match command {
                NativeMenuCommand::NewSession => {
                    self.state.dispatch(AppCommand::OpenLauncher, context)
                }
                NativeMenuCommand::StartLocalShell => {
                    self.state.dispatch(AppCommand::StartLocalSession, context)
                }
                NativeMenuCommand::OpenSettings => {
                    self.state.dispatch(AppCommand::OpenSettings, context)
                }
                NativeMenuCommand::CloseActiveSurface => {
                    let active = self.state.active();
                    self.request_close_tab(active, context);
                }
                NativeMenuCommand::ToggleCommandPalette => self.palette.toggle(),
                NativeMenuCommand::ToggleSessionInspector => {
                    self.toggle_inspector_from_current_focus(context)
                }
            }
        }
    }

    fn update_native_menu(&self) {
        let close_label = match self.state.active_tab().content {
            TabContent::Launcher => "Close Launcher",
            TabContent::Settings => "Close Settings",
            TabContent::SshAuthenticationRequired(_) | TabContent::Session(_) => "Close Session",
        };
        self.native_menu.update(
            close_label,
            matches!(self.state.active_tab().content, TabContent::Session(_)),
            self.state.inspector_open(),
        );
    }

    /// Applies the one close policy shared by chrome, shortcuts, the command
    /// palette, native menus, and session overlays. Non-live surfaces close
    /// immediately; a live transport is bound to an explicit confirmation.
    fn request_close_tab(&mut self, id: TabId, context: &egui::Context) {
        let confirmation = self
            .state
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
                            InspectorTransport::Local => CloseConsequence::TerminateLocalProcess,
                            InspectorTransport::Ssh { .. } => CloseConsequence::DisconnectSsh,
                        },
                        lifecycle_generation: session.controller.lifecycle_generation(),
                        restore_tab: self.state.active(),
                        cancel_focus_requested: false,
                    })
            });
        if let Some(confirmation) = confirmation {
            self.palette.close();
            self.pending_close = Some(confirmation);
        } else {
            self.state.dispatch(AppCommand::CloseTab(id), context);
        }
    }

    fn handle_paste_request(&mut self, tab: TabId, text: String) {
        let text = normalize_paste_line_endings(&text);
        let Some(session) = self.state.session_tab_mut(tab) else {
            return;
        };
        if !session.accepts_input() {
            return;
        }
        let bracketed_paste = session.terminal.modes().bracketed_paste();
        let line_count = paste_line_count(&text);
        let character_count = text.chars().count();
        let requires_confirmation = character_count >= LARGE_PASTE_CHARACTER_THRESHOLD
            || line_count >= LARGE_PASTE_LINE_THRESHOLD
            || (!bracketed_paste && line_count > 1);
        if !requires_confirmation {
            let _ = festerm_ui_egui::route_input(
                &mut session.terminal,
                festerm_core::InputEvent::Paste(text),
                &mut session.controller,
            );
            return;
        }
        self.pending_paste = Some(PendingPasteConfirmation {
            tab,
            identity: session.label.clone(),
            text,
            transport_state: session.status_bar_label(),
            lifecycle_generation: session.controller.lifecycle_generation(),
            bracketed_paste,
            cancel_focus_requested: false,
        });
    }

    fn show_close_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.pending_close.as_ref().cloned() else {
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
                        (InspectorTransport::Local, CloseConsequence::TerminateLocalProcess)
                            | (InspectorTransport::Ssh { .. }, CloseConsequence::DisconnectSsh)
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
        if let Some(current) = self.pending_close.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_close_confirmation();
        } else if confirm {
            self.pending_close = None;
            self.state
                .dispatch(AppCommand::CloseTab(pending.tab), context);
        }
    }

    fn cancel_close_confirmation(&mut self) {
        let Some(pending) = self.pending_close.take() else {
            return;
        };
        // Popup/menu widget IDs can disappear in the frame that opens the
        // dialog. Restore the active surface, not a stale invoker node.
        if let Some(session) = self.state.session_tab_mut(pending.restore_tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    fn cancel_paste_confirmation(&mut self) {
        let Some(pending) = self.pending_paste.take() else {
            return;
        };
        if let Some(session) = self.state.session_tab_mut(pending.tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    fn show_paste_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.pending_paste.as_ref().cloned() else {
            return;
        };
        let valid_target = self.state.active() == pending.tab
            && self
                .state
                .session_tab_mut(pending.tab)
                .is_some_and(|session| {
                    session.accepts_input()
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
                    if !pending.cancel_focus_requested {
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
        if let Some(current) = self.pending_paste.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_paste_confirmation();
        } else if paste {
            self.pending_paste = None;
            if let Some(session) = self.state.session_tab_mut(pending.tab) {
                let _ = festerm_ui_egui::route_input(
                    &mut session.terminal,
                    festerm_core::InputEvent::Paste(pending.text),
                    &mut session.controller,
                );
            }
        }
    }

    /// Handles the only user-triggered configuration I/O. The reloader keeps
    /// the selected path private; `AppState` receives a complete immutable
    /// replacement only after successful validation. Session state is not
    /// involved, so existing transports continue unchanged.
    fn reload_configuration(&mut self) {
        let (replacement, status) = self.configuration_reloader.reload();
        if let Some(configuration) = replacement {
            self.state.replace_configuration(configuration);
        }
        self.configuration_status = status;
    }

    /// Captures a metadata-only workspace and saves it only for an explicit
    /// Settings action. The current configuration changes only after the
    /// atomic file replacement has succeeded.
    fn save_workspace(&mut self) {
        let replacement = match self.state.capture_workspace_configuration() {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status = ConfigurationStartupStatus::WorkspaceSaveFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                );
                return;
            }
        };
        let status = self.configuration_reloader.save_workspace(&replacement);
        if matches!(status, ConfigurationStartupStatus::WorkspaceSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    fn native_store_available(&self) -> bool {
        self.secret_store.is_ok()
    }

    fn secure_storage_status_message(&self) -> Option<&'static str> {
        self.secure_storage_feedback.or_else(|| {
            self.secret_store
                .as_ref()
                .err()
                .copied()
                .map(secret_store_message)
        })
    }

    fn start_stored_password_profile(&mut self, profile_id: String, context: &egui::Context) {
        self.start_stored_password_profile_with_options(
            profile_id,
            festerm_ssh::SshSessionOptions::new(),
            context,
        );
    }

    fn start_stored_password_profile_with_options(
        &mut self,
        profile_id: String,
        options: festerm_ssh::SshSessionOptions,
        context: &egui::Context,
    ) {
        let Ok(store) = self.secret_store.as_ref() else {
            self.secure_storage_feedback = self
                .secret_store
                .as_ref()
                .err()
                .copied()
                .map(secret_store_message);
            return;
        };
        if !self.state.start_stored_password_ssh_profile(
            &profile_id,
            Arc::clone(store),
            options,
            context,
        ) {
            self.secure_storage_feedback =
                Some("This saved SSH profile has no stored password. Enter and remember a password first.");
        }
    }

    fn store_password_for_profile(
        &mut self,
        profile_id: String,
        password: crate::tabs::PasswordToStore,
        options: festerm_ssh::SshSessionOptions,
        context: &egui::Context,
    ) {
        let Ok(store) = self.secret_store.as_ref() else {
            self.secure_storage_feedback = self
                .secret_store
                .as_ref()
                .err()
                .copied()
                .map(secret_store_message);
            return;
        };
        if self.pending_password_store.is_some() {
            self.secure_storage_feedback =
                Some("A saved SSH password update is already in progress. Please wait.");
            return;
        }
        let store = Arc::clone(store);
        let worker_store = Arc::clone(&store);
        let (sender, receiver) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("festerm-store-ssh-password".to_owned())
            .spawn(move || {
                let secret = password.into_secret_bytes();
                let _ = sender.send(worker_store.put(&secret));
            }) {
            Ok(_) => {
                self.pending_password_store = Some(PendingPasswordStore {
                    receiver,
                    profile_id,
                    options,
                    store,
                });
                self.secure_storage_feedback =
                    Some("Saving SSH password in native secure storage…");
                context.request_repaint();
            }
            Err(_) => {
                self.secure_storage_feedback = Some(
                    "Native secure storage could not start a password-save worker. Try again.",
                );
            }
        }
    }

    fn process_pending_password_store(&mut self, context: &egui::Context) {
        let Some(pending) = self.pending_password_store.take() else {
            return;
        };
        match pending.receiver.try_recv() {
            Ok(Ok(reference)) => {
                let previous_reference = self
                    .state
                    .configuration()
                    .profile(&pending.profile_id)
                    .and_then(festerm_config::Profile::credential_reference)
                    .map(festerm_secret_store::SecretReference::duplicate_for_transport);
                let replacement = self.state.configuration().with_ssh_password_credential(
                    &pending.profile_id,
                    reference.duplicate_for_transport(),
                );
                let saved = replacement.as_ref().ok().and_then(|configuration| {
                    self.configuration_reloader
                        .save_configuration(configuration)
                        .ok()
                        .map(|_| configuration.clone())
                });
                if let Some(configuration) = saved {
                    self.state.replace_configuration(configuration);
                    self.configuration_status = ConfigurationStartupStatus::PasswordCredentialSaved;
                    self.secure_storage_feedback = match previous_reference {
                        Some(previous) => match pending.store.delete(&previous) {
                            Ok(_) => Some("SSH password saved in native secure storage."),
                            Err(_) => Some(
                                "SSH password saved, but the previous native password could not be removed.",
                            ),
                        },
                        None => Some("SSH password saved in native secure storage."),
                    };
                    self.start_stored_password_profile_with_options(
                        pending.profile_id,
                        pending.options,
                        context,
                    );
                } else {
                    let cleanup = pending.store.delete(&reference);
                    self.configuration_status =
                        ConfigurationStartupStatus::PasswordCredentialSaveFailure(
                            crate::configuration_startup::ConfigurationLoadFailure::Unreadable,
                        );
                    self.secure_storage_feedback = match cleanup {
                        Ok(_) => Some(
                            "SSH password was not linked because configuration could not be saved; the new native secret was removed.",
                        ),
                        Err(_) => Some(
                            "SSH password was not linked because configuration could not be saved; native-secret cleanup also failed.",
                        ),
                    };
                }
            }
            Ok(Err(error)) => {
                self.secure_storage_feedback = Some(secret_store_message(error));
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.pending_password_store = Some(pending);
                context.request_repaint_after(Duration::from_millis(20));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.secure_storage_feedback =
                    Some("Native secure storage did not complete the password save. Try again.");
            }
        }
    }

    fn update_window_title(&mut self, context: &egui::Context) {
        let title = Self::window_title();
        if self.window_title != title {
            context.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn window_title() -> String {
        APPLICATION_TITLE.to_owned()
    }

    /// Reduces a terminal-provided OSC title to just its final path
    /// component for the chip row's secondary text, so a chip shows
    /// `cmd.exe` rather than `C:\WINDOWS\system32\cmd.exe`
    /// (`docs/gui-design.md` "Identity precedence": the stable label leads,
    /// and secondary terminal metadata should stay compact rather than
    /// forcing the chip to grow to fit a full path). Falls back to the
    /// original string when it has no path-like structure to extract a
    /// final component from.
    fn display_secondary(terminal_title: &str) -> String {
        std::path::Path::new(terminal_title)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| terminal_title.to_owned())
    }

    /// Drains every open session's bounded backend queues, independent of
    /// which tab is active: each session chip represents "a persistent
    /// object with its own identity and state" (`docs/gui-design.md`) that
    /// must keep making progress while another tab is focused.
    fn pump_all_sessions(&mut self, context: &egui::Context) {
        let mut needs_repaint = false;
        for session in self.state.session_tabs_mut() {
            if session.controller.pump_events(&mut session.terminal) {
                needs_repaint = true;
            }
            session
                .controller
                .forward_terminal_replies(&mut session.terminal);
            session.controller.flush_pending_writes();
            session.controller.flush_pending_resize();
        }
        if needs_repaint {
            context.request_repaint();
        }
    }

    fn tab_id_for_chip(&self, chip_id: ChipId) -> Option<TabId> {
        self.state
            .tabs()
            .iter()
            .find(|tab| tab.id.chip_id() == chip_id.0)
            .map(|tab| tab.id)
    }

    /// Translates chrome gestures into `AppCommand`s and dispatches them
    /// through the single command-handling path
    /// (`docs/application-command-model.md`).
    fn dispatch_chrome_actions(&mut self, actions: Vec<ChromeAction>, context: &egui::Context) {
        for action in actions {
            match action {
                ChromeAction::NewTab => self.state.dispatch(AppCommand::OpenLauncher, context),
                ChromeAction::OpenSettings => {
                    self.state.dispatch(AppCommand::OpenSettings, context)
                }
                ChromeAction::ToggleInspector => self.toggle_inspector_from_current_focus(context),
                ChromeAction::TogglePalette => self.palette.toggle(),
                ChromeAction::ToggleChipLayout => {
                    self.state.dispatch(AppCommand::ToggleChipLayout, context)
                }
                ChromeAction::Activate(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::ActivateTab(id), context);
                        if self.state.inspector_open() {
                            self.inspector_restore_focus = None;
                        }
                        // Re-claim keyboard focus for the now-active
                        // session's terminal (`TerminalView::
                        // request_focus_on_next_frame`): selecting a chip
                        // otherwise left focus on the chrome row until the
                        // user clicked inside the terminal themselves.
                        if let Some(tab) = self.state.session_tab_mut(id) {
                            tab.view.request_focus_on_next_frame();
                        }
                    }
                }
                ChromeAction::Close(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.request_close_tab(id, context);
                    }
                }
                ChromeAction::Reorder { moved, before } => {
                    let Some(moved) = self.tab_id_for_chip(moved) else {
                        continue;
                    };
                    let before = before.and_then(|chip_id| self.tab_id_for_chip(chip_id));
                    self.state
                        .dispatch(AppCommand::ReorderTab { moved, before }, context);
                }
                ChromeAction::MoveLeft(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::MoveTabLeft(id), context);
                    }
                }
                ChromeAction::MoveRight(chip_id) => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state.dispatch(AppCommand::MoveTabRight(id), context);
                    }
                }
                ChromeAction::RenameStarted { restore_focus } => {
                    self.rename_restore_focus = restore_focus;
                    self.rename_restore_tab = Some(self.state.active());
                }
                ChromeAction::RenameFinished => {
                    let restore_tab = self.rename_restore_tab.take();
                    if let Some(tab) = restore_tab.and_then(|id| self.state.session_tab_mut(id)) {
                        tab.view.request_focus_on_next_frame();
                        self.rename_restore_focus = None;
                    } else if let Some(target) = self.rename_restore_focus.take() {
                        context.memory_mut(|memory| memory.request_focus(target));
                    }
                }
                ChromeAction::Rename { id: chip_id, name } => {
                    if let Some(id) = self.tab_id_for_chip(chip_id) {
                        self.state
                            .dispatch(AppCommand::RenameTab(id, name), context);
                    }
                }
            }
        }
    }

    fn toggle_inspector_from_current_focus(&mut self, context: &egui::Context) {
        if !self.state.inspector_open() {
            self.inspector_restore_focus = context.memory(|memory| memory.focused());
        }
        self.state
            .dispatch(AppCommand::ToggleSessionInspector, context);
    }

    /// Builds the current frame's command-palette items: every dispatchable
    /// application action, plus one "Activate" entry per open tab so the
    /// palette also serves as the searchable session switcher required by
    /// `docs/gui-design.md` ("a searchable session switcher keyed primarily
    /// by stable identity").
    fn palette_items(&self) -> Vec<PaletteItem> {
        const NEW_LAUNCHER_TAB: u64 = 1;
        const OPEN_SETTINGS: u64 = 2;
        const START_LOCAL_SESSION: u64 = 3;
        const TOGGLE_INSPECTOR: u64 = 4;
        const CLOSE_ACTIVE_TAB: u64 = 5;
        // Tab-scoped palette ids are offset well past the fixed action ids so
        // they never collide with a real `TabId::chip_id()` value.
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;

        let mut items = vec![
            PaletteItem {
                id: NEW_LAUNCHER_TAB,
                label: "New Session…".to_owned(),
                hint: ApplicationShortcut::NewSession.label().map(str::to_owned),
            },
            PaletteItem {
                id: START_LOCAL_SESSION,
                label: "Start Local Shell".to_owned(),
                hint: None,
            },
            PaletteItem {
                id: OPEN_SETTINGS,
                label: "Open Settings".to_owned(),
                hint: ApplicationShortcut::Settings.label().map(str::to_owned),
            },
        ];
        if matches!(self.state.active_tab().content, TabContent::Session(_)) {
            items.push(PaletteItem {
                id: TOGGLE_INSPECTOR,
                label: if self.state.inspector_open() {
                    "Hide Session Inspector".to_owned()
                } else {
                    "Show Session Inspector".to_owned()
                },
                hint: None,
            });
        }
        items.push(PaletteItem {
            id: CLOSE_ACTIVE_TAB,
            label: match &self.state.active_tab().content {
                TabContent::Launcher => "Close Launcher".to_owned(),
                TabContent::Settings => "Close Settings".to_owned(),
                TabContent::SshAuthenticationRequired(_) | TabContent::Session(_) => {
                    "Close Session…".to_owned()
                }
            },
            hint: None,
        });
        for tab in self.state.tabs() {
            let (label, hint) = match &tab.content {
                TabContent::Launcher => ("Launcher".to_owned(), None),
                TabContent::Settings => ("Settings".to_owned(), None),
                TabContent::SshAuthenticationRequired(tab) => (
                    tab.profile.identifier().to_owned(),
                    Some(format!(
                        "SSH authentication required · {}:{}",
                        tab.profile.host(),
                        tab.profile.port()
                    )),
                ),
                TabContent::Session(session) => {
                    let dynamic_title = session.terminal.title();
                    let hint = (!dynamic_title.is_empty())
                        .then(|| dynamic_title.to_owned())
                        .or_else(|| session.launch_secondary.clone());
                    (session.label.clone(), hint)
                }
            };
            items.push(PaletteItem {
                id: TAB_ACTIVATE_OFFSET + tab.id.chip_id(),
                label: format!("Activate: {label}"),
                hint,
            });
        }
        items
    }

    /// Applies a selected command-palette item id, translating it back into
    /// the same `AppCommand` path used by chrome gestures and shortcuts.
    fn dispatch_palette_selection(&mut self, id: u64, context: &egui::Context) {
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;
        match id {
            1 => self.state.dispatch(AppCommand::OpenLauncher, context),
            2 => self.state.dispatch(AppCommand::OpenSettings, context),
            3 => self.state.dispatch(AppCommand::StartLocalSession, context),
            4 => {
                // The palette closes as its command is selected, so its text
                // field is not a viable focus-restoration target.
                self.inspector_restore_focus = None;
                self.state
                    .dispatch(AppCommand::ToggleSessionInspector, context);
            }
            5 => {
                let active = self.state.active();
                self.request_close_tab(active, context);
            }
            id if id >= TAB_ACTIVATE_OFFSET => {
                let chip_id = ChipId(id - TAB_ACTIVATE_OFFSET);
                if let Some(target) = self.tab_id_for_chip(chip_id) {
                    self.state
                        .dispatch(AppCommand::ActivateTab(target), context);
                }
            }
            _ => {}
        }
    }

    /// Recognized global shortcuts (`docs/gui-design.md` "Interaction
    /// Conventions"). Tab creation/closure deliberately use Command on macOS
    /// and Ctrl+Shift on Windows/Linux, leaving plain Ctrl+T and Ctrl+W
    /// available to terminal applications such as Vim and Emacs. All bindings
    /// dispatch through the same `AppCommand` path as chip clicks and the
    /// palette.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.pending_close.is_some() || self.pending_paste.is_some() {
            return;
        }
        let open_palette = ApplicationShortcut::CommandPalette.consume(ctx);
        if open_palette {
            self.palette.toggle();
        }
        // While the palette is open, it owns Enter/Escape/arrow keys; avoid
        // also acting on tab-management shortcuts this frame.
        if self.palette.is_open() {
            return;
        }
        let new_tab = ApplicationShortcut::NewSession.consume(ctx);
        let close_tab = ApplicationShortcut::CloseActiveSurface.consume(ctx);
        let next_tab = ApplicationShortcut::NextSession.consume(ctx);
        let previous_tab = ApplicationShortcut::PreviousSession.consume(ctx);
        let settings = ApplicationShortcut::Settings.consume(ctx);

        if new_tab {
            self.state.dispatch(AppCommand::OpenLauncher, ctx);
        }
        if close_tab {
            let active = self.state.active();
            self.request_close_tab(active, ctx);
        }
        if next_tab {
            self.state.dispatch(AppCommand::ActivateNextTab, ctx);
        }
        if previous_tab {
            self.state.dispatch(AppCommand::ActivatePreviousTab, ctx);
        }
        if settings {
            self.state.dispatch(AppCommand::OpenSettings, ctx);
        }
    }

    /// Builds this frame's chip view models. Stable tab identity (label)
    /// always leads; a non-empty terminal-provided title is shown only as
    /// secondary metadata (`docs/gui-design.md` "Identity precedence").
    fn chip_view_models(&self) -> (Vec<ChipViewModel>, ChipId) {
        let chips = self
            .state
            .tabs()
            .iter()
            .map(|tab| {
                let (primary, secondary, status) = match &tab.content {
                    TabContent::Launcher => (
                        "New Session".to_owned(),
                        Some("Launcher".to_owned()),
                        ChipStatus::Neutral,
                    ),
                    TabContent::Settings => (
                        "Settings".to_owned(),
                        Some("Application".to_owned()),
                        ChipStatus::Neutral,
                    ),
                    TabContent::SshAuthenticationRequired(tab) => (
                        tab.profile.identifier().to_owned(),
                        Some(format!(
                            "SSH authentication required · {}:{}",
                            tab.profile.host(),
                            tab.profile.port()
                        )),
                        ChipStatus::Neutral,
                    ),
                    TabContent::Session(session) => {
                        let dynamic_title = session.terminal.title();
                        let secondary = (!dynamic_title.is_empty())
                            .then(|| Self::display_secondary(dynamic_title))
                            .or_else(|| session.launch_secondary.clone());
                        (session.label.clone(), secondary, session.chip_status())
                    }
                };
                let renamable = matches!(tab.content, TabContent::Session(_));
                ChipViewModel {
                    id: ChipId(tab.id.chip_id()),
                    primary,
                    secondary,
                    status,
                    closable: true,
                    renamable,
                }
            })
            .collect();
        (chips, ChipId(self.state.active().chip_id()))
    }

    /// Right-side session inspector (`docs/gui-design.md` "Application chrome
    /// and session context"): hidden by default, and shows only content-free
    /// connection state and diagnostics for the active session. It never
    /// hosts Settings.
    fn show_session_inspector(
        &self,
        context: &egui::Context,
        content_rect: egui::Rect,
        close_requested: bool,
    ) -> Option<InspectorAction> {
        let TabContent::Session(session) = &self.state.active_tab().content else {
            return None;
        };
        let tab = self.state.active();
        let diagnostics = session.controller.diagnostics_line();
        let grid = session.view.dimensions_label();
        let terminal_title =
            (!session.terminal.title().is_empty()).then(|| session.terminal.title());
        let status = session.status_bar_label();
        let chip_status = session.chip_status();
        let transport = match &session.inspector_transport {
            InspectorTransport::Local => TransportFacts::Local,
            InspectorTransport::Ssh {
                username,
                host,
                port,
            } => TransportFacts::Ssh {
                username,
                host,
                port: *port,
            },
        };
        let type_label = match session.inspector_transport {
            InspectorTransport::Local => "Local shell",
            InspectorTransport::Ssh { .. } => "SSH",
        };
        let state_message = match chip_status {
            ChipStatus::Failed => Some(match session.inspector_transport {
                InspectorTransport::Local => {
                    "The local shell could not start. Review Diagnostics for the failure detail."
                }
                InspectorTransport::Ssh { .. } => {
                    "The SSH session could not start. Review Diagnostics for the failure detail."
                }
            }),
            ChipStatus::Disconnected => Some("The connection has been lost."),
            ChipStatus::Exited => Some("The session has exited."),
            ChipStatus::Reconnecting => Some("Attempting to reconnect to the host."),
            ChipStatus::AuthRequired => Some("Authentication is required to continue."),
            ChipStatus::Starting | ChipStatus::Connected | ChipStatus::Neutral => None,
        };
        crate::inspector::show(
            context,
            content_rect,
            InspectorContent {
                subject_id: tab.chip_id(),
                identity: &session.label,
                type_label,
                state: status,
                state_message,
                state_color: chip_status.color(),
                grid: grid.as_deref(),
                terminal_title,
                profile: session.profile_identifier.as_deref(),
                transport,
                trust_fingerprint: session
                    .host_key_prompt()
                    .map(|prompt| prompt.sha256_fingerprint()),
                diagnostics: &diagnostics,
                reconnect_available: session.reconnect_available(),
            },
            close_requested,
        )
    }

    /// Bottom application status bar. Session identity remains in the chip;
    /// this footer shows only sourced grid/locality facts and transport state.
    /// Application surfaces keep the same 24 px geometry with empty content.
    fn show_status_bar(&self, ui: &mut egui::Ui) {
        let (dimensions, system, status, status_label) = match &self.state.active_tab().content {
            TabContent::Launcher
            | TabContent::Settings
            | TabContent::SshAuthenticationRequired(_) => (None, None, ChipStatus::Neutral, ""),
            TabContent::Session(session) => {
                let status = session.chip_status();
                (
                    session.view.dimensions_label(),
                    Some(session.system_label()),
                    status,
                    session.status_bar_label(),
                )
            }
        };
        egui::Panel::bottom("status_bar")
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(theme::SURFACE_WINDOW))
            .show(ui, |ui| {
                festerm_ui_egui::statusbar::show(
                    ui,
                    festerm_ui_egui::statusbar::StatusBarContent {
                        dimensions: dimensions.as_deref(),
                        system,
                        status,
                        status_label,
                    },
                );
            });
    }

    /// Shows the M7 host-trust decision only for the active SSH tab. The
    /// returned command is dispatched after UI construction, so clicking a
    /// control only signals the SSH worker and never waits for network I/O on
    /// the GUI thread.
    fn show_host_key_prompt(&self, ui: &mut egui::Ui) -> Option<AppCommand> {
        let tab = self.state.active_tab();
        let TabContent::Session(session) = &tab.content else {
            return None;
        };
        let prompt = session.host_key_prompt()?;
        let host_port = Self::canonical_host_port(prompt.host(), prompt.port());
        let fingerprint = prompt.sha256_fingerprint();
        let mut decision = None;

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.heading("Verify SSH Host Key");
            ui.label("Verify this host before connecting:");
            ui.horizontal(|ui| {
                ui.label("Host:");
                ui.monospace(&host_port);
            });
            ui.horizontal(|ui| {
                ui.label("SHA-256 fingerprint:");
                ui.monospace(fingerprint);
            });
            ui.horizontal(|ui| {
                if ui.button("Reject").clicked() {
                    decision = Some(HostKeyTrustDecision::Reject);
                }
                if ui.button("Accept Once").clicked() {
                    decision = Some(HostKeyTrustDecision::AcceptOnce);
                }
            });
        });

        decision.map(|decision| AppCommand::ResolveHostKeyTrust {
            tab: tab.id,
            decision,
        })
    }

    fn canonical_host_port(host: &str, port: u16) -> String {
        if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        }
    }
}

impl eframe::App for FesTermApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_pending_password_store(context);
        self.pump_all_sessions(context);
        self.update_window_title(context);
        if let Some(smoke) = self.native_smoke.as_mut() {
            if let Some(primary_tab) = self.primary_tab {
                if let Some(primary) = self.state.session_tab_mut(primary_tab) {
                    smoke.drive(context, &mut primary.terminal, &mut primary.controller);
                }
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_content(ui);
    }
}

impl FesTermApp {
    /// The full chrome/palette/session UI for one frame. Split out from
    /// [`eframe::App::ui`] so headless `egui_kittest` tests can drive it
    /// directly without constructing an `eframe::Frame` (whose fields are
    /// private to `eframe` and not test-constructible).
    fn ui_content(&mut self, ui: &mut egui::Ui) {
        self.process_pending_password_store(ui.ctx());
        self.handle_native_menu_commands(ui.ctx());
        self.handle_shortcuts(ui.ctx());
        self.update_native_menu();

        // A destructive confirmation owns Escape before the terminal input
        // adapter sees raw events. Backdrop clicks are intentionally ignored.
        let confirmation_escape = (self.pending_close.is_some() || self.pending_paste.is_some())
            && ui
                .ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

        let (chips, active_chip) = self.chip_view_models();
        let inspector_open = self.state.inspector_open();
        let inspector_available = matches!(self.state.active_tab().content, TabContent::Session(_));
        let actions = chrome::show(
            ui,
            &chips,
            active_chip,
            inspector_open,
            inspector_available,
            self.state.chip_layout(),
        );
        // No explicit separator line here: the chrome band now paints its
        // own lighter `CHROME_BACKGROUND` fill, and the natural color
        // contrast between that band and the darker terminal content below
        // it reads as the boundary (mockup: a near-invisible seam, not a
        // bright rule).
        self.dispatch_chrome_actions(actions, &ui.ctx().clone());
        let inspector_open = self.state.inspector_open();
        // Consume Escape before `TerminalView::show` routes raw input so the
        // dismissal key can never leak into Vim, Emacs, or another TUI.
        let inspector_escape = inspector_open
            && ui
                .ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

        if self.state.status_bar_visible() {
            self.show_status_bar(ui);
        }

        if let Some(decision) = {
            let items = self.palette_items();
            palette::show(ui.ctx(), &mut self.palette, &items)
        } {
            self.palette.close();
            if let Some(id) = decision {
                let context = ui.ctx().clone();
                self.dispatch_palette_selection(id, &context);
            }
        }

        let host_key_command = self.show_host_key_prompt(ui);
        let content_rect = ui.available_rect_before_wrap();
        // Intercept outside pointer-button events before TerminalView reads
        // them. A foreground Area can paint above the terminal, but it cannot
        // retroactively undo input already routed earlier in the frame.
        let inspector_outside_click = if inspector_open {
            let inspector_rect = crate::inspector::overlay_rect(content_rect);
            ui.ctx().input_mut(|input| {
                let mut outside_click = false;
                input.events.retain(|event| {
                    let egui::Event::PointerButton { pos, .. } = event else {
                        return true;
                    };
                    if content_rect.contains(*pos) && !inspector_rect.contains(*pos) {
                        outside_click = true;
                        false
                    } else {
                        true
                    }
                });
                outside_click
            })
        } else {
            false
        };
        let mut screen_command = None;
        let mut overlay_action = None;
        let paste_was_pending = self.pending_paste.is_some();
        let mut deferred_pastes = Vec::new();
        let chip_layout = self.state.chip_layout();
        let native_store_available = self.native_store_available();
        let secure_storage_status = self.secure_storage_status_message();
        let active_tab_id = self.state.active();
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    screen_command = screens::show_launcher(
                        ui,
                        active_tab_id,
                        self.state.configuration().profiles(),
                        native_store_available,
                        secure_storage_status,
                    );
                }
                TabContent::Settings => {
                    screen_command = screens::show_settings(
                        ui,
                        chip_layout,
                        self.state.status_bar_visible(),
                        self.configuration_status,
                        secure_storage_status,
                    );
                }
                TabContent::SshAuthenticationRequired(tab) => {
                    screen_command = screens::show_ssh_authentication_required(
                        ui,
                        active_tab_id,
                        &tab.profile,
                        native_store_available,
                    );
                }
                TabContent::Session(session) => {
                    let options = festerm_ui_egui::TerminalViewOptions {
                        paste_available: session.accepts_input(),
                        terminal_input_enabled: self.pending_close.is_none()
                            && self.pending_paste.is_none(),
                        defer_paste_to_application: true,
                    };
                    session.view.show_with_options(
                        ui,
                        &mut session.terminal,
                        &mut session.controller,
                        options,
                    );
                    deferred_pastes = session.view.take_paste_requests();
                    session
                        .controller
                        .observe_resize_probe_terminal_state(&session.terminal);
                    session
                        .controller
                        .forward_terminal_replies(&mut session.terminal);
                    session.controller.flush_pending_writes();
                    session.controller.flush_pending_resize();
                    if session.controller.pump_events(&mut session.terminal) {
                        ui.ctx().request_repaint();
                    }
                    overlay_action = overlay::show(ui.ctx(), session.chip_status());
                }
            }
        }
        if paste_was_pending && !deferred_pastes.is_empty() {
            // A later clipboard-delivery event invalidates the captured
            // operation. Never replace an open dialog or route a second paste.
            self.cancel_paste_confirmation();
        } else if self.pending_close.is_none()
            && self.pending_paste.is_none()
            && deferred_pastes.len() == 1
        {
            self.handle_paste_request(active_tab_id, deferred_pastes.remove(0));
        }
        let inspector_action = inspector_open
            .then(|| {
                self.show_session_inspector(
                    ui.ctx(),
                    content_rect,
                    inspector_escape || inspector_outside_click,
                )
            })
            .flatten();
        if let Some(command) = host_key_command {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }
        if let Some(action) = inspector_action {
            let context = ui.ctx().clone();
            match action {
                InspectorAction::Close => {
                    if let Some(target) = self.inspector_restore_focus.take() {
                        context.memory_mut(|memory| memory.request_focus(target));
                    } else if let Some(session) = self.state.session_tab_mut(active_tab_id) {
                        session.view.request_focus_on_next_frame();
                    }
                    self.state
                        .dispatch(AppCommand::ToggleSessionInspector, &context);
                }
                InspectorAction::Reconnect => self
                    .state
                    .dispatch(AppCommand::ReconnectSession(active_tab_id), &context),
            }
        }
        if let Some(command) = screen_command {
            match command {
                AppCommand::ReloadConfiguration => self.reload_configuration(),
                AppCommand::SaveWorkspace => self.save_workspace(),
                AppCommand::StartStoredPasswordSshProfile { profile_id } => {
                    self.start_stored_password_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StoreSshPassword {
                    profile_id,
                    password,
                    options,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    options,
                    &ui.ctx().clone(),
                ),
                AppCommand::CloseTab(id) => {
                    self.request_close_tab(id, &ui.ctx().clone());
                }
                command => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(command, &context);
                }
            }
        }
        if let Some(action) = overlay_action {
            let context = ui.ctx().clone();
            match action {
                OverlayAction::OpenDiagnostics => {
                    self.inspector_restore_focus = None;
                    if !self.state.inspector_open() {
                        self.state
                            .dispatch(AppCommand::ToggleSessionInspector, &context);
                    }
                }
                OverlayAction::CloseTab => {
                    self.request_close_tab(active_tab_id, &context);
                }
            }
        }

        if self.pending_close.is_some() {
            self.show_close_confirmation(ui.ctx(), confirmation_escape);
        } else {
            self.show_paste_confirmation(ui.ctx(), confirmation_escape);
        }

        if self.native_smoke.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
impl FesTermApp {
    /// Builds a `FesTermApp` around a launcher tab instead of a live local
    /// shell, so headless end-to-end UI tests do not need a real PTY and do
    /// not depend on `eframe::Frame`, which has no public/test constructor.
    fn for_test_with_configuration(configuration: Configuration) -> Self {
        let state = AppState::for_test_with_configuration(configuration);
        Self {
            state,
            primary_tab: None,
            window_title: APPLICATION_TITLE.to_owned(),
            native_smoke: None,
            palette: PaletteState::default(),
            configuration_status: ConfigurationStartupStatus::Missing,
            configuration_reloader: ConfigurationReloader::unavailable(),
            secret_store: Ok(Arc::new(MemorySecretStore::new())),
            pending_password_store: None,
            secure_storage_feedback: None,
            inspector_restore_focus: None,
            rename_restore_focus: None,
            rename_restore_tab: None,
            pending_close: None,
            pending_paste: None,
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
        }
    }

    fn for_test_with_live_session(context: &egui::Context) -> (Self, TabId) {
        let (state, tab) = AppState::with_primary_session(context, None, Configuration::empty());
        let mut app = Self::for_test_with_configuration(Configuration::empty());
        app.state = state;
        (app, tab)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use super::*;
    use egui_kittest::{kittest::Queryable, Harness, SnapshotOptions};

    #[test]
    fn paste_policy_normalizes_endings_and_counts_trailing_lines_exactly() {
        let normalized = normalize_paste_line_endings("first\r\nsecond\rthird\n");

        assert_eq!(normalized, "first\nsecond\nthird\n");
        assert_eq!(paste_line_count(&normalized), 4);
        assert_eq!(normalized.chars().count(), 19);
    }

    #[test]
    fn paste_preview_is_bounded_and_escapes_non_whitespace_controls() {
        let text = format!("one\ttwo\u{0007}\n{}", "x".repeat(900));
        let (preview, shown_lines, shown_characters) = bounded_paste_preview(&text);

        assert!(preview.starts_with("one\ttwo\\u{0007}\n"));
        assert_eq!(shown_lines, 2);
        assert_eq!(shown_characters, PASTE_PREVIEW_CHARACTER_LIMIT);
        assert!(shown_characters < text.chars().count());
    }

    #[test]
    fn close_confirmation_states_transport_specific_consequence() {
        assert!(CloseConsequence::TerminateLocalProcess
            .message()
            .contains("local process"));
        assert!(CloseConsequence::DisconnectSsh
            .message()
            .contains("SSH connection"));
    }

    #[test]
    fn confirmation_width_preserves_margins_at_minimum_window_size() {
        assert_eq!(confirmation_width(360.0, 440.0), 328.0);
        assert_eq!(confirmation_width(360.0, 360.0), 328.0);
        assert_eq!(confirmation_width(900.0, 440.0), 440.0);
    }

    #[test]
    fn live_close_confirmation_is_safe_by_default_and_confirmed_deliberately() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.request_close_tab(tab, &context);
        assert!(app.pending_close.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Cancel").is_focused());

        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(harness.state().pending_close.is_some());
        assert_eq!(harness.state().state.active(), tab);

        harness.get_by_label("Close Session").click();
        harness.step();
        assert!(harness.state().pending_close.is_none());
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));
    }

    #[test]
    fn escape_cancels_live_close_without_closing_session() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.request_close_tab(tab, &context);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().pending_close.is_none());
        assert_eq!(harness.state().state.active(), tab);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));
    }

    fn harness() -> Harness<'static, FesTermApp> {
        harness_with_configuration(Configuration::empty())
    }

    fn harness_with_configuration(configuration: Configuration) -> Harness<'static, FesTermApp> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(
                |ui, app: &mut FesTermApp| {
                    ui.ctx().set_visuals(theme::default_visuals());
                    app.ui_content(ui);
                },
                FesTermApp::for_test_with_configuration(configuration),
            )
    }

    fn failed_local_profile_harness() -> Harness<'static, FesTermApp> {
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "festerm-inspector-test-command-that-does-not-exist",
            Vec::new(),
            None,
        )
        .expect("test local profile is valid")])
        .expect("test configuration is valid");
        let mut harness = harness_with_configuration(configuration);
        harness.run();
        harness
            .get_by_label("development — Saved local profile")
            .click();
        harness.step();
        harness.run();
        harness
    }

    #[test]
    fn saved_password_launcher_path_uses_injected_memory_store_and_persists_only_reference() {
        let configuration = Configuration::new(vec![festerm_config::Profile::ssh(
            "production",
            "ssh.example.test",
            2200,
            "deploy",
            "xterm-256color",
            80,
            24,
        )
        .expect("test SSH profile is valid")])
        .expect("test configuration is valid");
        let directory = std::env::current_dir()
            .expect("test working directory is available")
            .join(format!(
                ".festerm-stored-password-test-{}",
                std::process::id()
            ));
        fs::create_dir(&directory).expect("test directory can be created");
        let path = directory.join("config.toml");
        let mut harness = harness_with_configuration(configuration);
        harness.state_mut().configuration_reloader =
            ConfigurationReloader::from_path_for_test(path.clone());
        harness.run();
        harness
            .get_by_label("Enter or replace password for production")
            .click();
        harness.step();
        harness.run();
        harness.get_by_label("Password").click();
        harness
            .get_by_label("Password")
            .type_text("memory-only-password");
        harness
            .get_by_label("Remember this password in native secure storage")
            .click();
        harness.get_by_label("Connect with password").click();
        harness.step();

        for _ in 0..50 {
            if harness
                .state()
                .state
                .configuration()
                .profile("production")
                .and_then(festerm_config::Profile::credential_reference)
                .is_some()
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
            harness.step();
        }

        let reference = harness
            .state()
            .state
            .configuration()
            .profile("production")
            .and_then(festerm_config::Profile::credential_reference)
            .expect("background worker must persist the opaque reference");
        let store = harness
            .state()
            .secret_store
            .as_ref()
            .expect("tests inject a memory store");
        assert_eq!(
            store
                .get(reference)
                .expect("stored password is available to the transport")
                .with_bytes(|bytes| bytes.to_vec()),
            b"memory-only-password"
        );
        let saved = fs::read_to_string(&path).expect("configuration was saved");
        assert!(saved.contains("credential_id"));
        assert!(!saved.contains("memory-only-password"));
        fs::remove_dir_all(directory).expect("test directory can be removed");
    }

    /// Produces a production-widget Launcher capture for explicit visual
    /// review without making it a platform-stable golden baseline.
    #[test]
    #[ignore = "manual GUI mockup review capture"]
    fn capture_launcher_for_mockup_review() {
        let output_path = std::env::temp_dir().join("festerm-gui-review");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(752.0, 516.0))
            .build_ui_state(
                |ui, app: &mut FesTermApp| {
                    ui.ctx().set_visuals(theme::default_visuals());
                    app.ui_content(ui);
                },
                FesTermApp::for_test_with_configuration(Configuration::empty()),
            );
        harness.run();
        harness.snapshot_options(
            "festerm-launcher-actual",
            &SnapshotOptions::default().output_path(output_path),
        );
    }

    /// Produces the Session Inspector overlay with production widgets while
    /// keeping the capture independent of a live process or network service.
    #[test]
    #[ignore = "manual GUI mockup review capture"]
    fn capture_session_inspector_for_mockup_review() {
        let output_path = std::env::temp_dir().join("festerm-gui-review");
        let mut harness = failed_local_profile_harness();
        harness
            .get_by_label_contains("Toggle session inspector")
            .click();
        harness.run();
        harness.snapshot_options(
            "festerm-session-inspector-actual",
            &SnapshotOptions::default().output_path(output_path),
        );
    }

    /// Captures the production terminal-local context menu with a real local
    /// session and deterministic application-owned selection.
    #[test]
    #[ignore = "manual GUI mockup review capture"]
    fn capture_terminal_context_menu_for_mockup_review() {
        let output_path = std::env::temp_dir().join("festerm-gui-review");
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        if let TabContent::Session(session) =
            &mut harness.state_mut().state.active_tab_mut().content
        {
            session.terminal.ingest(b"fesTerm context-menu review");
        }
        harness.run();
        let grid = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .grid_rect
                .expect("session grid is rendered"),
            _ => panic!("local launcher action must create a session"),
        };
        let start = grid.left_top() + egui::vec2(2.0, 2.0);
        let end = start + egui::vec2(52.0, 0.0);
        harness.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerMoved(end));
        harness.event(egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        harness.get_by_label("Terminal viewport").click_secondary();
        harness.run();
        harness.snapshot_options(
            "festerm-terminal-context-menu-actual",
            &SnapshotOptions::default().output_path(output_path),
        );
    }

    /// Captures a session-chip menu targeted at an inactive chip; the active
    /// Launcher remains visibly unchanged to prove context targeting does not
    /// activate the session.
    #[test]
    #[ignore = "manual GUI mockup review capture"]
    fn capture_session_chip_context_menu_for_mockup_review() {
        let output_path = std::env::temp_dir().join("festerm-gui-review");
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let session_id = harness.state().state.active();
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenSettings, &egui::Context::default());
        harness.state_mut().state.dispatch(
            AppCommand::MoveTabRight(session_id),
            &egui::Context::default(),
        );
        harness.run();
        harness.get_by_label("Local Shell chip").click_secondary();
        harness.run();
        harness.snapshot_options(
            "festerm-session-chip-context-menu-actual",
            &SnapshotOptions::default().output_path(output_path),
        );
    }

    #[test]
    fn session_inspector_opens_without_resizing_and_escape_restores_terminal_mode() {
        let mut harness = failed_local_profile_harness();
        let before = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.dimensions_label(),
            _ => panic!("the configured profile must produce a session surface"),
        };

        harness
            .get_by_label_contains("Toggle session inspector")
            .click();
        harness.run();

        assert!(harness.state().state.inspector_open());
        harness.get_by_label("Session Inspector");
        harness.get_by_label("Close Session Inspector");
        harness.get_by_label("PROCESS");
        let after = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.dimensions_label(),
            _ => panic!("the session surface must remain active"),
        };
        assert_eq!(after, before, "the overlay must not resize the terminal");

        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(!harness.state().state.inspector_open());
    }

    #[test]
    fn inspector_consumes_the_first_uncovered_terminal_click() {
        let mut harness = failed_local_profile_harness();
        harness
            .get_by_label_contains("Toggle session inspector")
            .click();
        harness.run();

        // An interaction inside the foreground panel must not hit the
        // click-catcher beneath it.
        harness.get_by_label("Diagnostics").click();
        harness.run();
        assert!(harness.state().state.inspector_open());

        let before = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.selection().clone(),
            _ => panic!("the configured profile must produce a session surface"),
        };
        let uncovered_terminal = egui::pos2(120.0, 300.0);
        harness.event(egui::Event::PointerMoved(uncovered_terminal));
        harness.event(egui::Event::PointerButton {
            pos: uncovered_terminal,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.event(egui::Event::PointerButton {
            pos: uncovered_terminal,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run();

        assert!(!harness.state().state.inspector_open());
        let after = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.selection().clone(),
            _ => panic!("the session surface must remain active"),
        };
        assert_eq!(after, before, "the dismissing click must not select text");
    }

    fn tab_management_modifiers() -> egui::Modifiers {
        if cfg!(target_os = "macos") {
            egui::Modifiers::COMMAND
        } else {
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
        }
    }

    #[test]
    fn native_window_title_is_fixed_for_privacy() {
        assert_eq!(FesTermApp::window_title(), APPLICATION_TITLE);
    }

    #[test]
    fn canonical_host_port_preserves_ssh_destination_boundaries() {
        assert_eq!(
            FesTermApp::canonical_host_port("ssh.example.test", 2222),
            "ssh.example.test:2222"
        );
        assert_eq!(
            FesTermApp::canonical_host_port("2001:db8::7", 22),
            "[2001:db8::7]:22"
        );
    }

    #[test]
    fn startup_workspace_replaces_the_default_local_session() {
        let workspace = festerm_config::WorkspaceConfiguration::new(
            vec![
                festerm_config::WorkspaceTab::launcher("launcher").expect("launcher tab is valid"),
                festerm_config::WorkspaceTab::ssh_session("remote", "production")
                    .expect("SSH tab is valid"),
            ],
            Some("remote".to_owned()),
        )
        .expect("workspace is valid");
        let configuration = Configuration::new_with_workspace(
            vec![festerm_config::Profile::ssh(
                "production",
                "ssh.example.test",
                2200,
                "deploy",
                "xterm-256color",
                80,
                24,
            )
            .expect("SSH profile is valid")],
            workspace,
        )
        .expect("configuration is valid");

        let app = FesTermApp::with_configuration(&egui::Context::default(), configuration);

        assert!(app.primary_tab.is_none());
        assert_eq!(app.state.tabs().len(), 2);
        assert!(matches!(app.state.tabs()[0].content, TabContent::Launcher));
        assert!(matches!(
            app.state.tabs()[1].content,
            TabContent::SshAuthenticationRequired(_)
        ));
        assert_eq!(app.state.active(), app.state.tabs()[1].id);
    }

    #[test]
    fn default_configuration_starts_at_the_launcher() {
        let app = FesTermApp::with_configuration(&egui::Context::default(), Configuration::empty());

        assert!(app.primary_tab.is_none());
        assert_eq!(app.state.tabs().len(), 1);
        assert!(matches!(
            app.state.active_tab().content,
            TabContent::Launcher
        ));
    }

    #[test]
    fn successful_workspace_save_replaces_configuration_after_writing() {
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "sh",
            Vec::new(),
            None,
        )
        .unwrap()])
        .unwrap();
        let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-workspace-save-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone());

        app.save_workspace();

        assert_eq!(
            app.configuration_status,
            ConfigurationStartupStatus::WorkspaceSaved
        );
        assert_eq!(
            app.state.configuration().profiles(),
            configuration.profiles()
        );
        assert!(app.state.configuration().workspace_enabled());
        assert_eq!(
            Configuration::load_from_path(&path).unwrap(),
            *app.state.configuration()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_workspace_save_retains_configuration_without_path_or_content_leakage() {
        let configuration = Configuration::empty();
        let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-workspace-save-failure-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(directory.clone());

        app.save_workspace();

        let diagnostic = app.configuration_status.settings_message();
        assert!(matches!(
            app.configuration_status,
            ConfigurationStartupStatus::WorkspaceSaveFailure(_)
        ));
        assert_eq!(app.state.configuration(), &configuration);
        assert!(!diagnostic.contains(directory.to_string_lossy().as_ref()));
        assert!(!diagnostic.contains("schema_version"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn platform_new_tab_shortcut_focuses_the_singleton_launcher_end_to_end() {
        let mut harness = harness();
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));
    }

    #[test]
    fn control_tab_switches_surfaces_on_every_platform() {
        let mut harness = harness();
        harness.run();
        let launcher = harness.state().state.active();
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenSettings, &context);
        harness.run();
        assert_ne!(harness.state().state.active(), launcher);

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Tab);
        harness.run();
        assert_eq!(harness.state().state.active(), launcher);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_settings_shortcut_and_presented_hints_match_the_contract() {
        let mut harness = harness();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Comma);
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));

        let items = harness.state().palette_items();
        assert_eq!(
            items
                .iter()
                .find(|item| item.label == "New Session…")
                .and_then(|item| item.hint.as_deref()),
            Some("Cmd+T")
        );
        assert_eq!(
            items
                .iter()
                .find(|item| item.label == "Open Settings")
                .and_then(|item| item.hint.as_deref()),
            Some("Cmd+,")
        );
    }

    #[test]
    fn plain_control_t_and_w_are_not_tab_management_shortcuts() {
        let mut harness = harness();
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::T);
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::W);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before);
    }

    #[test]
    fn platform_close_tab_shortcut_closes_the_active_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.step();
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        let before = harness.state().state.tabs().len();

        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::W);
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before - 1);
    }

    #[test]
    fn pressing_enter_on_the_launcher_starts_the_highlighted_option_end_to_end() {
        let mut harness = harness();
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));

        harness.key_press(egui::Key::Enter);
        // A freshly started local shell session keeps requesting repaints
        // as it pumps live process output, so `run()` (which loops to
        // quiescence) can never stabilize here; a single `step()` is enough
        // to apply the dispatched command and observe the tab-content
        // change.
        harness.step();

        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));
    }

    #[test]
    fn configured_local_profile_launcher_action_dispatches_end_to_end() {
        let configuration = Configuration::new(vec![festerm_config::Profile::local(
            "development",
            "festerm-profile-test-command-that-does-not-exist",
            Vec::new(),
            None,
        )
        .expect("test local profile is valid")])
        .expect("test configuration is valid");
        let mut harness = harness_with_configuration(configuration);
        harness.run();

        harness
            .get_by_label("development — Saved local profile")
            .click();
        harness.step();

        let TabContent::Session(session) = &harness.state().state.active_tab().content else {
            panic!("the configured profile launcher action must start a session tab");
        };
        assert_eq!(session.label, "development");
    }

    #[test]
    fn clicking_a_chip_close_button_closes_that_tab_end_to_end() {
        let mut harness = harness();
        harness.run();
        // Replace the startup Launcher with a session, then open the singleton
        // Launcher so closing it leaves the session behind.
        harness.key_press(egui::Key::Enter);
        harness.step();
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        let before = harness.state().state.tabs().len();

        harness
            .get_all_by_label("Close")
            .next()
            .expect("at least one closable chip")
            .click();
        harness.run();

        assert_eq!(harness.state().state.tabs().len(), before - 1);
    }

    #[test]
    fn command_palette_selection_activates_the_chosen_tab_end_to_end() {
        let mut harness = harness();
        harness.run();

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::P,
        );
        harness.run();
        assert!(harness.state().palette.is_open());

        let settings_label = ApplicationShortcut::Settings.label().map_or_else(
            || "Open Settings".to_owned(),
            |hint| format!("Open Settings  \u{2014}  {hint}"),
        );
        harness.get_by_label(&settings_label).click();
        harness.run();
        assert!(!harness.state().palette.is_open());
        let settings_tab = harness.state().state.active();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::P,
        );
        harness.run();
        harness.get_by_label("Activate: Launcher").click();
        harness.run();

        assert_ne!(harness.state().state.active(), settings_tab);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));
    }

    #[test]
    fn cancelling_chip_rename_restores_terminal_focus_without_leaking_escape() {
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let before = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => panic!("launcher action must start a session"),
        };

        harness.get_by_label("Local Shell chip").click_secondary();
        harness.run();
        harness.get_by_label("Rename session").click();
        harness.run();
        harness.key_press(egui::Key::Escape);
        // Text-selection visuals may request another frame after Escape; one
        // frame is sufficient to assert the application-owned focus result.
        harness.step();

        let after_escape = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => panic!("rename must not change the active session"),
        };
        assert_eq!(after_escape, before, "Escape must remain application-owned");

        harness.event(egui::Event::Text("Q".to_owned()));
        harness.run();
        let after_text = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => panic!("session must remain active"),
        };
        assert_eq!(after_text, before + 1);
    }
}
