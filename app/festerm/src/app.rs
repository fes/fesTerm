use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use eframe::egui;
use festerm_config::{
    Configuration, InterfaceSettings, Profile, SerialDataBits, SerialFlowControl, SerialParity,
    SerialStopBits, TerminalFontPreference,
};
use festerm_pty::LocalProfile;
#[cfg(test)]
use festerm_secret_store::MemorySecretStore;
use festerm_secret_store::{native_store, SecretStore, SecretStoreError};
use festerm_session::HostKeyPrompt;
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};
use festerm_ui_egui::overlay::{self, OverlayAction};
use festerm_ui_egui::palette::{self, PaletteItem, PaletteState};
use festerm_ui_egui::theme;
use festerm_ui_egui::{TerminalFontFamily, TerminalFontGeneration, TerminalFontSet};

use crate::configuration_startup::{
    ConfigurationReloader, ConfigurationStartupStatus, StartupConfiguration,
};
use crate::inspector::{InspectorAction, InspectorContent, TransportFacts};
use crate::native_smoke::NativeWindowSmoke;
use crate::overlay_state::{
    CloseConsequence, OverlayState, PendingCloseConfirmation, PendingPasswordStore,
    PendingPasteConfirmation, PendingSettingsResetConfirmation,
};
use crate::screens;
use crate::tabs::{
    AppCommand, AppState, HostKeyTrustDecision, InspectorTransport, TabContent, TabId,
};
use crate::updates::{UpdateController, UpdateStatus};

const APPLICATION_TITLE: &str = "fesTerm";
const AI_AUTHORSHIP_SUMMARY: &str = "Entirely AI-written with human guidance.";
const AI_AUTHORSHIP_DETAIL: &str =
    "GitHub Copilot produced the code, tests, documentation, and first-party assets; \
     the project owner directs and accepts the work.";
const LARGE_PASTE_CHARACTER_THRESHOLD: usize = 4_096;
const LARGE_PASTE_LINE_THRESHOLD: usize = 100;
const PASTE_PREVIEW_CHARACTER_LIMIT: usize = 800;
const PASTE_PREVIEW_LINE_LIMIT: usize = 8;

#[derive(Clone, Copy)]
enum ApplicationShortcut {
    CommandPalette,
    NewSession,
    StartLocalShell,
    CloseActiveSurface,
    NextSession,
    PreviousSession,
    /// The macOS-only `Cmd+,` "Preferences" convention. Also bound as a
    /// native `fesTerm` app-menu accelerator
    /// (`festerm-macos-window`'s `install`), so this egui-level chord is a
    /// redundant safety net there; it has no non-mac equivalent (comma
    /// carries no such convention on Windows/Linux).
    Settings,
    /// A cross-platform, discoverable Settings shortcut that doesn't rely on
    /// finding the macOS-only app menu or knowing the `Cmd+,` convention -
    /// this is the one presented in Settings' own Keyboard card.
    SettingsHotkey,
    ZoomOut,
    ZoomReset,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
}

#[derive(Clone, Copy)]
enum ZoomCommand {
    In,
    Out,
    Reset,
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
            Self::StartLocalShell => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                egui::Key::N,
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
            Self::SettingsHotkey => Some((
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::S,
            )),
            Self::ZoomOut => Some((egui::Modifiers::COMMAND, egui::Key::Minus)),
            Self::ZoomReset => Some((egui::Modifiers::COMMAND, egui::Key::Num0)),
            Self::ClearTerminal => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                egui::Key::K,
            )),
            Self::ResetTerminal => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND | egui::Modifiers::ALT
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                egui::Key::R,
            )),
            Self::ToggleFocusMode => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                if cfg!(target_os = "macos") {
                    egui::Key::F
                } else {
                    egui::Key::F11
                },
            )),
        }
    }

    const fn label(self) -> Option<&'static str> {
        match self {
            Self::CommandPalette if cfg!(target_os = "macos") => Some("\u{2318}+Shift+P"),
            Self::CommandPalette => Some("Ctrl+Shift+P"),
            Self::NewSession if cfg!(target_os = "macos") => Some("\u{2318}+T"),
            Self::NewSession => Some("Ctrl+Shift+T"),
            Self::StartLocalShell if cfg!(target_os = "macos") => Some("\u{2318}+N"),
            Self::StartLocalShell => Some("Ctrl+Shift+N"),
            Self::CloseActiveSurface if cfg!(target_os = "macos") => Some("\u{2318}+W"),
            Self::CloseActiveSurface => Some("Ctrl+Shift+W"),
            Self::NextSession => Some("Ctrl+Tab"),
            Self::PreviousSession => Some("Ctrl+Shift+Tab"),
            Self::Settings if cfg!(target_os = "macos") => Some("\u{2318}+,"),
            Self::Settings => None,
            Self::SettingsHotkey if cfg!(target_os = "macos") => Some("\u{2318}+Shift+S"),
            Self::SettingsHotkey => Some("Ctrl+Shift+S"),
            Self::ZoomOut if cfg!(target_os = "macos") => Some("\u{2318}+-"),
            Self::ZoomOut => Some("Ctrl+-"),
            Self::ZoomReset if cfg!(target_os = "macos") => Some("\u{2318}+0"),
            Self::ZoomReset => Some("Ctrl+0"),
            Self::ClearTerminal if cfg!(target_os = "macos") => Some("\u{2318}+K"),
            Self::ClearTerminal => Some("Ctrl+Shift+K"),
            Self::ResetTerminal if cfg!(target_os = "macos") => Some("Option+\u{2318}+R"),
            Self::ResetTerminal => Some("Ctrl+Shift+R"),
            Self::ToggleFocusMode if cfg!(target_os = "macos") => Some("\u{2318}+Shift+F"),
            Self::ToggleFocusMode => Some("Ctrl+Shift+F11"),
        }
    }

    fn consume(self, context: &egui::Context) -> bool {
        self.chord().is_some_and(|(modifiers, key)| {
            context.input_mut(|input| input.consume_key(modifiers, key))
        })
    }
}

/// The first several open tabs, in tab-bar order, get a quick-switch
/// keystroke (`Cmd+1`..`Cmd+9` on macOS, `Ctrl+1`..`Ctrl+9` elsewhere) that
/// jumps directly to that tab, mirroring the browser/terminal convention of
/// numbering only the first nine positions. Shared by `palette_items` (to
/// display the keystroke) and `handle_shortcuts` (to act on it), so the two
/// never drift out of sync.
const MAX_QUICK_SWITCH_TABS: usize = 9;

/// The physical key for the Nth (0-based) quick-switch slot, or `None` past
/// `MAX_QUICK_SWITCH_TABS`.
fn quick_switch_key(index: usize) -> Option<egui::Key> {
    [
        egui::Key::Num1,
        egui::Key::Num2,
        egui::Key::Num3,
        egui::Key::Num4,
        egui::Key::Num5,
        egui::Key::Num6,
        egui::Key::Num7,
        egui::Key::Num8,
        egui::Key::Num9,
    ]
    .get(index)
    .copied()
}

/// Pre-formatted display text for the Nth (0-based) quick-switch slot (e.g.
/// `"\u{2318} 1"`), or `None` past `MAX_QUICK_SWITCH_TABS`.
fn quick_switch_label(index: usize) -> Option<String> {
    if index >= MAX_QUICK_SWITCH_TABS {
        return None;
    }

    let n = index + 1;
    Some(if cfg!(target_os = "macos") {
        format!("\u{2318} {n}")
    } else {
        format!("Ctrl+{n}")
    })
}

const fn terminal_font_family(preference: TerminalFontPreference) -> TerminalFontFamily {
    match preference {
        TerminalFontPreference::JetBrainsMono => TerminalFontFamily::JetBrainsMono,
        TerminalFontPreference::IosevkaTerm => TerminalFontFamily::IosevkaTerm,
        TerminalFontPreference::JuliaMono => TerminalFontFamily::JuliaMono,
        TerminalFontPreference::MapleMono => TerminalFontFamily::MapleMono,
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
    secure_storage_feedback: Option<&'static str>,
    /// Widget that owned focus immediately before Inspector opened, when it
    /// remains a meaningful restoration target.
    inspector_restore_focus: Option<egui::Id>,
    rename_restore_focus: Option<egui::Id>,
    rename_restore_tab: Option<TabId>,
    /// Confirmation prompts, in-flight secure-storage lookup, and transient
    /// status banner (see `overlay_state`); grouped into one type so the
    /// several call sites that need "is anything blocking terminal input"
    /// have one shared answer.
    overlays: OverlayState,
    native_menu: festerm_macos_window::NativeMenu,
    /// Resume-from-sleep notifier (see `install_wake_monitor`). `None` until
    /// installed by the real composition root; headless tests never call
    /// `install_wake_monitor`, so they simply never receive wake signals.
    wake_monitor: Option<PlatformWakeMonitor>,
    /// Set from the wake-monitor's OS-thread callback, and drained on the
    /// main thread once per frame (`logic`). A plain flag, not a channel:
    /// coalescing repeated wake signals into a single liveness pass is
    /// correct and avoids unbounded queuing while the app is backgrounded.
    wake_requested: Arc<AtomicBool>,
    focus_mode: bool,
    terminal_fonts_installed: bool,
    terminal_font_generation: TerminalFontGeneration,
    about_icon: Option<egui::TextureHandle>,
    updates: UpdateController,
}

#[cfg(target_os = "macos")]
type PlatformWakeMonitor = festerm_macos_window::WakeMonitor;
#[cfg(target_os = "windows")]
type PlatformWakeMonitor = festerm_windows_power::WakeMonitor;
#[cfg(target_os = "linux")]
type PlatformWakeMonitor = festerm_linux_power::WakeMonitor;

/// Platforms with no wake-notification hook yet still work correctly: they
/// simply rely on ordinary transport-error detection and probe cadence, per
/// ADR 0018 ("wake/network events optimize detection, they do not gate
/// correctness").
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
struct PlatformWakeMonitor;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
impl PlatformWakeMonitor {
    fn install(_wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }
}

fn native_secret_store() -> Result<Arc<dyn SecretStore>, SecretStoreError> {
    native_store().map(Arc::<dyn SecretStore>::from)
}

fn load_application_icon(context: &egui::Context) -> egui::TextureHandle {
    let icon = crate::application_icon_data();
    context.load_texture(
        "fesTerm application icon",
        egui::ColorImage::from_rgba_unmultiplied(
            [icon.width as usize, icon.height as usize],
            &icon.rgba,
        ),
        egui::TextureOptions::LINEAR,
    )
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

const fn serial_data_bits_label(value: SerialDataBits) -> &'static str {
    match value {
        SerialDataBits::Five => "5",
        SerialDataBits::Six => "6",
        SerialDataBits::Seven => "7",
        SerialDataBits::Eight => "8",
    }
}

const fn serial_parity_label(value: SerialParity) -> &'static str {
    match value {
        SerialParity::None => "None",
        SerialParity::Odd => "Odd",
        SerialParity::Even => "Even",
    }
}

const fn serial_stop_bits_label(value: SerialStopBits) -> &'static str {
    match value {
        SerialStopBits::One => "1",
        SerialStopBits::Two => "2",
    }
}

const fn serial_flow_control_label(value: SerialFlowControl) -> &'static str {
    match value {
        SerialFlowControl::None => "None",
        SerialFlowControl::Software => "Software (XON/XOFF)",
        SerialFlowControl::Hardware => "Hardware (RTS/CTS)",
    }
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
        // Workspace restoration is an explicit opt-in
        // (`docs/gui-design.md` "Workspace restore"), off by default: any
        // saved tab list is ignored - and dropped from the in-memory
        // configuration outright, so it can't resurface later just because
        // an unrelated settings change gets saved - whenever the preference
        // reads false, regardless of what a previous run (or an earlier
        // version of fesTerm, before this preference existed) left on disk.
        let configuration = if configuration.interface_settings().restore_workspace() {
            configuration
        } else {
            configuration.without_workspace()
        };
        // One semantic blue-graphite default for application surfaces and
        // widgets. Terminal ANSI and explicit RGB colors remain independent.
        context.set_visuals(theme::default_visuals());
        let terminal_font_generation = festerm_ui_egui::install_terminal_font_family(
            context,
            terminal_font_family(configuration.interface_settings().terminal_font()),
        );
        // fesTerm owns the standard zoom chords as per-session terminal
        // commands. Letting egui also process them at end-of-frame would scale
        // application chrome and violate the documented zoom boundary.
        context.options_mut(|options| options.zoom_with_keyboard = false);
        let native_smoke = NativeWindowSmoke::from_environment();
        let about_icon = load_application_icon(context);
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
            secure_storage_feedback: None,
            inspector_restore_focus: None,
            rename_restore_focus: None,
            rename_restore_tab: None,
            overlays: OverlayState::default(),
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
            wake_monitor: None,
            wake_requested: Arc::new(AtomicBool::new(false)),
            focus_mode: false,
            terminal_fonts_installed: true,
            terminal_font_generation,
            about_icon: Some(about_icon),
            updates: UpdateController::from_build(),
        }
    }

    pub(crate) fn install_native_menu(&mut self, context: &egui::Context) {
        let context = context.clone();
        self.native_menu =
            festerm_macos_window::install_application_menu(std::sync::Arc::new(move || {
                context.request_repaint()
            }));
    }

    /// Starts the platform wake-notification hook (resume-from-sleep on
    /// macOS/Windows, `PrepareForSleep` over D-Bus on Linux; a no-op on any
    /// other platform). The callback only sets a flag and asks for a
    /// repaint; the actual liveness probe runs later on the main thread from
    /// `logic`, since `AppState`'s tabs are not safe to touch from the
    /// monitor's own OS thread.
    pub(crate) fn install_wake_monitor(&mut self, context: &egui::Context) {
        let context = context.clone();
        let wake_requested = Arc::clone(&self.wake_requested);
        self.wake_monitor = Some(PlatformWakeMonitor::install(Arc::new(move || {
            wake_requested.store(true, Ordering::Release);
            context.request_repaint();
        })));
    }

    /// Keeps the native macOS traffic lights vertically centered against the
    /// chip row. Re-applied every frame from the current chrome geometry
    /// (`festerm_ui_egui::chrome::chrome_band_center_from_top`) rather than
    /// assumed once at window creation, so it stays correct across a future
    /// runtime chip-height change with no further wiring.
    #[cfg(target_os = "macos")]
    fn sync_native_window_chrome(&self, frame: &eframe::Frame) {
        use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

        let Ok(window_handle) = frame.window_handle() else {
            return;
        };
        let RawWindowHandle::AppKit(appkit_handle) = window_handle.as_raw() else {
            return;
        };
        festerm_macos_window::offset_traffic_lights(
            appkit_handle.ns_view,
            f64::from(festerm_ui_egui::chrome::chrome_band_center_from_top(
                self.state.show_session_details(),
            )),
        );
        festerm_macos_window::disable_native_window_movement(appkit_handle.ns_view);
    }

    #[cfg(not(target_os = "macos"))]
    fn sync_native_window_chrome(&self, _frame: &eframe::Frame) {}

    fn handle_native_menu_commands(&mut self, context: &egui::Context) {
        if self.overlays.blocks_terminal_input() {
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
                NativeMenuCommand::ClearTerminal => self.clear_active_terminal(context),
                NativeMenuCommand::ResetTerminal => self.reset_active_terminal(context),
                NativeMenuCommand::ToggleFocusMode => self.toggle_focus_mode(context),
            }
        }
    }

    fn update_native_menu(&self) {
        let close_label = match self.state.active_tab().content {
            TabContent::Launcher => "Close Launcher",
            TabContent::Settings => "Close Settings",
            TabContent::Profiles => "Close Profiles",
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
    /// immediately; a live transport is confirmed when that preference is
    /// enabled and closes directly otherwise.
    fn request_close_tab(&mut self, id: TabId, context: &egui::Context) {
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
                                InspectorTransport::Ssh { .. } => CloseConsequence::DisconnectSsh,
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
        self.overlays.pending_paste = Some(PendingPasteConfirmation {
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

    fn cancel_close_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_close.take() else {
            return;
        };
        // Popup/menu widget IDs can disappear in the frame that opens the
        // dialog. Restore the active surface, not a stale invoker node.
        if let Some(session) = self.state.session_tab_mut(pending.restore_tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    fn cancel_paste_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_paste.take() else {
            return;
        };
        if let Some(session) = self.state.session_tab_mut(pending.tab) {
            session.view.request_focus_on_next_frame();
        }
    }

    fn show_paste_confirmation(&mut self, context: &egui::Context, escape: bool) {
        let Some(pending) = self.overlays.pending_paste.as_ref().cloned() else {
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
        if let Some(current) = self.overlays.pending_paste.as_mut() {
            current.cancel_focus_requested = true;
        }
        if cancel {
            self.cancel_paste_confirmation();
        } else if paste {
            self.overlays.pending_paste = None;
            if let Some(session) = self.state.session_tab_mut(pending.tab) {
                let _ = festerm_ui_egui::route_input(
                    &mut session.terminal,
                    festerm_core::InputEvent::Paste(pending.text),
                    &mut session.controller,
                );
            }
        }
    }

    /// Captures a metadata-only workspace snapshot and saves it immediately
    /// (`docs/gui-design.md` "Configuration": open/closed tabs, their order,
    /// and the active tab autosave on every change - there is no manual
    /// Save action). The current configuration changes only after the
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

    /// Scrubs any previously saved workspace snapshot from disk after the
    /// "Workspace restore" preference is turned off (`docs/gui-design.md`
    /// "Workspace restore"). Infallible on the in-memory side - clearing a
    /// workspace can never fail validation - so this only reports a status
    /// if the write-through itself fails.
    fn clear_saved_workspace(&mut self) {
        let replacement = self.state.configuration().without_workspace();
        let status = self.configuration_reloader.save_workspace(&replacement);
        if matches!(status, ConfigurationStartupStatus::WorkspaceSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Writes through the current chip-layout/status-bar preferences
    /// immediately after a toggle or reset. The in-memory `AppState` change
    /// applies regardless of whether the write succeeds
    /// (`docs/gui-design.md` "apply immediately"); a failed write only means
    /// the change will not survive a restart.
    fn persist_interface_settings(&mut self) {
        let replacement = match self
            .state
            .configuration()
            .with_interface_settings(self.state.interface_settings())
        {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status =
                    ConfigurationStartupStatus::InterfaceSettingsSaveFailure(
                        crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                    );
                return;
            }
        };
        let status = self
            .configuration_reloader
            .save_interface_settings(&replacement);
        if matches!(status, ConfigurationStartupStatus::InterfaceSettingsSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Persists the host key currently displayed for `tab` as trusted (ADR
    /// 0020), reading the pending prompt before any dispatch can clear it.
    /// The current in-memory configuration is only replaced after the
    /// atomic file write succeeds, matching [`Self::save_workspace`]'s
    /// commit-only-on-success rule; the SSH-level accept proceeds
    /// regardless, since a save failure only means this host will prompt
    /// again on a future connection rather than being remembered.
    fn persist_known_host_trust(&mut self, tab: TabId) {
        let Some(prompt) = self
            .state
            .session_tab_mut(tab)
            .and_then(|session| session.host_key_prompt())
        else {
            return;
        };
        let host = prompt.host().to_owned();
        let port = prompt.port();
        let fingerprint = prompt.sha256_fingerprint().to_owned();
        let replacement =
            match self
                .state
                .configuration()
                .with_known_host_trust(&host, port, &fingerprint)
            {
                Ok(replacement) => replacement,
                Err(_) => {
                    self.configuration_status =
                        ConfigurationStartupStatus::KnownHostTrustSaveFailure(
                            crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                        );
                    return;
                }
            };
        let status = self
            .configuration_reloader
            .save_known_host_trust(&replacement);
        if matches!(status, ConfigurationStartupStatus::KnownHostTrustSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Creates or edits a profile from the Profiles surface (both are the
    /// same upsert-by-identifier write, `docs/gui-design.md` "Profile
    /// editing"). The in-memory configuration only changes after the atomic
    /// file write succeeds, matching every other automatic-save path here.
    fn save_profile(&mut self, profile: festerm_config::Profile) {
        let replacement = match self.state.configuration().with_profile(profile) {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status = ConfigurationStartupStatus::ProfileSaveFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                );
                return;
            }
        };
        let status = self.configuration_reloader.save_profile(&replacement);
        if matches!(status, ConfigurationStartupStatus::ProfileSaved) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Deletes a profile the user has already confirmed in the Profiles
    /// surface. `Configuration::without_profile` itself rejects deletion of
    /// a profile still referenced by a saved workspace tab, surfacing that
    /// as an ordinary save failure rather than silently orphaning the tab.
    fn delete_profile(&mut self, identifier: &str) {
        let replacement = match self.state.configuration().without_profile(identifier) {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status = ConfigurationStartupStatus::ProfileDeleteFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                );
                return;
            }
        };
        let status = self.configuration_reloader.delete_profile(&replacement);
        if matches!(status, ConfigurationStartupStatus::ProfileDeleted) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Reorders a saved profile after a drag-to-reorder gesture on the
    /// Profiles surface (`Configuration::with_reordered_profiles`); the
    /// Launcher's own profile ordering reflects this immediately since both
    /// surfaces read the same persisted `Configuration::profiles` order.
    fn reorder_profiles(&mut self, moved: &str, before: Option<&str>) {
        let replacement = match self
            .state
            .configuration()
            .with_reordered_profiles(moved, before)
        {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status = ConfigurationStartupStatus::ProfilesReorderFailure(
                    crate::configuration_startup::ConfigurationLoadFailure::Invalid,
                );
                return;
            }
        };
        let status = self.configuration_reloader.reorder_profiles(&replacement);
        if matches!(status, ConfigurationStartupStatus::ProfilesReordered) {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
    }

    /// Applies the reset-interface-settings policy shared by chrome, the
    /// command palette, and shortcuts: nothing to confirm when settings
    /// already equal defaults, otherwise a destructive-adjacent confirmation
    /// (`docs/gui-action-graph.md` SET-02).
    fn request_reset_interface_settings(&mut self, context: &egui::Context) {
        if self.state.interface_settings() == InterfaceSettings::DEFAULT {
            self.state
                .dispatch(AppCommand::ResetInterfaceSettings, context);
            return;
        }
        self.palette.close();
        self.overlays.pending_settings_reset = Some(PendingSettingsResetConfirmation {
            cancel_focus_requested: false,
        });
    }

    fn reinstall_terminal_font(&mut self, context: &egui::Context) {
        self.terminal_font_generation = festerm_ui_egui::install_terminal_font_family(
            context,
            terminal_font_family(self.state.terminal_font()),
        );
        context.request_repaint();
    }

    fn show_settings_reset_confirmation(&mut self, context: &egui::Context, escape: bool) {
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

    fn cancel_settings_reset_confirmation(&mut self) {
        self.overlays.pending_settings_reset = None;
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
        // Derive a manual-recovery strategy from the saved profile's
        // durable-session configuration (ADR 0018), so clicking a saved
        // profile's "use stored password" action attaches/creates its
        // configured tmux/screen session instead of always opening a plain
        // shell.
        let strategy = self
            .state
            .configuration()
            .profile(&profile_id)
            .and_then(Profile::as_ssh)
            .and_then(|profile| profile.session_strategy().ok())
            .unwrap_or(festerm_ssh::SessionStrategy::PlainShell);
        self.start_stored_password_profile_with_options(
            profile_id,
            festerm_ssh::SshSessionOptions::manual_recovery(strategy),
            context,
        );
    }

    /// Resolves whether a saved SSH profile has a stored native-secret
    /// credential and starts it accordingly: with one, exactly like
    /// `StartStoredPasswordSshProfile` (needs the composition root's secret
    /// store); without one, `AppState::start_configured_ssh_profile_interactive`
    /// handles the whole launch itself (openssh-style in-terminal prompt),
    /// no composition-root resource required.
    fn start_configured_ssh_profile(&mut self, profile_id: String, context: &egui::Context) {
        let has_credential = self
            .state
            .configuration()
            .profile(&profile_id)
            .and_then(Profile::as_ssh)
            .is_some_and(|profile| profile.credential_reference().is_some());
        if has_credential {
            self.start_stored_password_profile(profile_id, context);
        } else {
            self.state
                .start_configured_ssh_profile_interactive(&profile_id, context);
        }
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
        launch_after_store: bool,
        context: &egui::Context,
    ) {
        self.store_credential_for_profile(
            profile_id,
            festerm_config::CredentialKind::Password,
            move || password.into_secret_bytes(),
            options,
            launch_after_store,
            context,
        );
    }

    fn store_private_key_for_profile(
        &mut self,
        profile_id: String,
        private_key: crate::tabs::PrivateKeyToStore,
        options: festerm_ssh::SshSessionOptions,
        launch_after_store: bool,
        context: &egui::Context,
    ) {
        self.store_credential_for_profile(
            profile_id,
            festerm_config::CredentialKind::PrivateKey,
            move || private_key.into_secret_bytes(),
            options,
            launch_after_store,
            context,
        );
    }

    /// Shared background-worker storage path for both password and
    /// private-key profile credentials: the secret is converted to
    /// [`SecretBytes`] only inside the spawned worker thread and never
    /// touches the UI thread, mirroring the existing password-only
    /// implementation this replaces.
    fn store_credential_for_profile(
        &mut self,
        profile_id: String,
        credential_kind: festerm_config::CredentialKind,
        make_secret: impl FnOnce() -> festerm_secret_store::SecretBytes + Send + 'static,
        options: festerm_ssh::SshSessionOptions,
        launch_after_store: bool,
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
        if self.overlays.pending_password_store.is_some() {
            self.secure_storage_feedback =
                Some("A saved SSH credential update is already in progress. Please wait.");
            return;
        }
        let store = Arc::clone(store);
        let worker_store = Arc::clone(&store);
        let (sender, receiver) = mpsc::sync_channel(1);
        match thread::Builder::new()
            .name("festerm-store-ssh-credential".to_owned())
            .spawn(move || {
                let secret = make_secret();
                let _ = sender.send(worker_store.put(&secret));
            }) {
            Ok(_) => {
                self.overlays.pending_password_store = Some(PendingPasswordStore {
                    receiver,
                    profile_id,
                    options,
                    store,
                    launch_after_store,
                    credential_kind,
                });
                self.secure_storage_feedback =
                    Some("Saving SSH credential in native secure storage…");
                context.request_repaint();
            }
            Err(_) => {
                self.secure_storage_feedback = Some(
                    "Native secure storage could not start a credential-save worker. Try again.",
                );
            }
        }
    }

    fn process_pending_password_store(&mut self, context: &egui::Context) {
        let Some(pending) = self.overlays.pending_password_store.take() else {
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
                let replacement = self.state.configuration().with_ssh_credential(
                    &pending.profile_id,
                    reference.duplicate_for_transport(),
                    pending.credential_kind,
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
                            Ok(_) => Some("SSH credential saved in native secure storage."),
                            Err(_) => Some(
                                "SSH credential saved, but the previous native secret could not be removed.",
                            ),
                        },
                        None => Some("SSH credential saved in native secure storage."),
                    };
                    if pending.launch_after_store {
                        self.start_stored_password_profile_with_options(
                            pending.profile_id,
                            pending.options,
                            context,
                        );
                    }
                } else {
                    let cleanup = pending.store.delete(&reference);
                    self.configuration_status =
                        ConfigurationStartupStatus::PasswordCredentialSaveFailure(
                            crate::configuration_startup::ConfigurationLoadFailure::Unreadable,
                        );
                    self.secure_storage_feedback = match cleanup {
                        Ok(_) => Some(
                            "SSH credential was not linked because configuration could not be saved; the new native secret was removed.",
                        ),
                        Err(_) => Some(
                            "SSH credential was not linked because configuration could not be saved; native-secret cleanup also failed.",
                        ),
                    };
                }
            }
            Ok(Err(error)) => {
                self.secure_storage_feedback = Some(secret_store_message(error));
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.overlays.pending_password_store = Some(pending);
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

    /// Runs a liveness probe across every open SSH session once, if the wake
    /// monitor's OS thread signaled a resume since the previous frame. Runs
    /// on the main thread deliberately: `AppState`'s tabs are not safe to
    /// reach from the monitor's own background thread.
    fn check_wake_monitor_signal(&mut self) {
        if self.wake_requested.swap(false, Ordering::AcqRel) {
            self.state.request_liveness_check_on_all_sessions();
        }
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
        self.show_active_eviction_notice_if_needed(context);
    }

    /// M9 eviction notice: the first time the *active* tab's retained
    /// scrollback discards a logical line to stay within its memory bound,
    /// surface a one-shot transient notice so the user learns why history
    /// they scroll back to may stop short, instead of silently truncating
    /// with no visible signal. Latched per tab (`eviction_notice_shown`) so
    /// a sustained-output workload that keeps evicting doesn't repaint a
    /// notice every frame. Deliberately scoped to the active tab only:
    /// background tabs evicting quietly should not steal the single shared
    /// transient-notice slot from whatever the user is looking at.
    fn show_active_eviction_notice_if_needed(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        if session.eviction_notice_shown {
            return;
        }
        if session.terminal.scrollback_stats().evicted_lines() == 0 {
            return;
        }
        session.eviction_notice_shown = true;
        self.overlays.transient_notice = Some((
            "Scrollback limit reached — oldest history discarded".to_owned(),
            Instant::now() + Duration::from_millis(2_500),
        ));
        context.request_repaint();
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
                ChromeAction::OpenProfiles => {
                    self.state.dispatch(AppCommand::OpenProfiles, context)
                }
                ChromeAction::ToggleInspector => self.toggle_inspector_from_current_focus(context),
                ChromeAction::TogglePalette => self.palette.toggle(),
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
        const START_LOCAL_SESSION: u64 = 3;
        const TOGGLE_INSPECTOR: u64 = 4;
        const CLOSE_ACTIVE_TAB: u64 = 5;
        const TOGGLE_FOCUS_MODE: u64 = 6;
        const ZOOM_IN: u64 = 7;
        const ZOOM_OUT: u64 = 8;
        const RESET_ZOOM: u64 = 9;
        const ABOUT: u64 = 10;
        const RESET_TERMINAL: u64 = 11;
        const CLEAR_TERMINAL_HISTORY: u64 = 12;
        const COPY: u64 = 13;
        const PASTE: u64 = 14;
        // Tab-scoped palette ids are offset well past the fixed action ids so
        // they never collide with a real `TabId::chip_id()` value.
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;

        let mut items = vec![
            PaletteItem {
                id: NEW_LAUNCHER_TAB,
                label: "New Session…".to_owned(),
                hint: ApplicationShortcut::NewSession.label().map(str::to_owned),
                is_tab: false,
                shortcut_label: None,
            },
            PaletteItem {
                id: START_LOCAL_SESSION,
                label: "Start Local Shell".to_owned(),
                hint: ApplicationShortcut::StartLocalShell
                    .label()
                    .map(str::to_owned),
                is_tab: false,
                shortcut_label: None,
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
                is_tab: false,
                shortcut_label: None,
            });
            items.extend([
                PaletteItem {
                    id: TOGGLE_FOCUS_MODE,
                    label: if self.focus_mode {
                        "Exit Focus Mode".to_owned()
                    } else {
                        "Enter Focus Mode".to_owned()
                    },
                    hint: ApplicationShortcut::ToggleFocusMode
                        .label()
                        .map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: ZOOM_IN,
                    label: "Zoom In".to_owned(),
                    hint: Some(if cfg!(target_os = "macos") {
                        "\u{2318}++".to_owned()
                    } else {
                        "Ctrl++".to_owned()
                    }),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: ZOOM_OUT,
                    label: "Zoom Out".to_owned(),
                    hint: ApplicationShortcut::ZoomOut.label().map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: RESET_ZOOM,
                    label: "Reset Zoom".to_owned(),
                    hint: ApplicationShortcut::ZoomReset.label().map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: COPY,
                    label: "Copy".to_owned(),
                    // Handled by egui's built-in clipboard shortcut, which
                    // binds to Cmd/Ctrl+C regardless of platform (unlike the
                    // app-level `ApplicationShortcut`s, which shift to
                    // Ctrl+Shift on Windows/Linux to leave Ctrl+C free for
                    // terminal interrupt).
                    hint: Some(if cfg!(target_os = "macos") {
                        "\u{2318}+C".to_owned()
                    } else {
                        "Ctrl+C".to_owned()
                    }),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: PASTE,
                    label: "Paste".to_owned(),
                    hint: Some(if cfg!(target_os = "macos") {
                        "\u{2318}+V".to_owned()
                    } else {
                        "Ctrl+V".to_owned()
                    }),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: RESET_TERMINAL,
                    label: "Reset Terminal".to_owned(),
                    hint: ApplicationShortcut::ResetTerminal
                        .label()
                        .map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: CLEAR_TERMINAL_HISTORY,
                    label: "Clear Terminal".to_owned(),
                    hint: ApplicationShortcut::ClearTerminal
                        .label()
                        .map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
            ]);
        }
        items.push(PaletteItem {
            id: CLOSE_ACTIVE_TAB,
            label: match &self.state.active_tab().content {
                TabContent::Launcher => "Close Launcher".to_owned(),
                TabContent::Settings => "Close Settings".to_owned(),
                TabContent::Profiles => "Close Profiles".to_owned(),
                TabContent::SshAuthenticationRequired(_) | TabContent::Session(_) => {
                    "Close Session…".to_owned()
                }
            },
            hint: ApplicationShortcut::CloseActiveSurface
                .label()
                .map(str::to_owned),
            is_tab: false,
            shortcut_label: None,
        });
        for (index, tab) in self.state.tabs().iter().enumerate() {
            let (label, hint) = match &tab.content {
                TabContent::Launcher => ("Launcher".to_owned(), None),
                TabContent::Settings => ("Settings".to_owned(), None),
                TabContent::Profiles => ("Profiles".to_owned(), None),
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
                label,
                hint,
                is_tab: true,
                shortcut_label: quick_switch_label(index),
            });
        }
        // "About fesTerm" is deliberately the very last entry, after every
        // tab, so the palette's action items (which people reach for far
        // more often) aren't pushed down by it.
        items.push(PaletteItem {
            id: ABOUT,
            label: "About fesTerm".to_owned(),
            hint: None,
            is_tab: false,
            shortcut_label: None,
        });
        items
    }

    /// Applies a selected command-palette item id, translating it back into
    /// the same `AppCommand` path used by chrome gestures and shortcuts.
    fn dispatch_palette_selection(&mut self, id: u64, context: &egui::Context) {
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;
        match id {
            1 => self.state.dispatch(AppCommand::OpenLauncher, context),
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
            6 => self.toggle_focus_mode(context),
            7 => self.zoom_active_session(ZoomCommand::In, context),
            8 => self.zoom_active_session(ZoomCommand::Out, context),
            9 => self.zoom_active_session(ZoomCommand::Reset, context),
            10 => {
                self.overlays.about_open = true;
                self.overlays.about_licenses_open = false;
                context.request_repaint();
            }
            11 => self.reset_active_terminal(context),
            12 => self.clear_active_terminal(context),
            13 => self.copy_active_selection(context),
            14 => self.paste_into_active_session(context),
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
        if self.overlays.blocks_terminal_input() {
            return;
        }
        let open_palette = ApplicationShortcut::CommandPalette.consume(ctx);
        if open_palette {
            self.palette.toggle();
        }
        // Quick-switch to one of the first `MAX_QUICK_SWITCH_TABS` tabs by
        // position (`Cmd+1`..`Cmd+9`/`Ctrl+1`..`Ctrl+9`), matching the
        // keystroke shown next to each tab row in the command palette
        // (`palette_items`). Checked before the palette-open early return
        // below so the same keystroke works whether or not the palette
        // happens to be open.
        for index in 0..MAX_QUICK_SWITCH_TABS {
            let Some(key) = quick_switch_key(index) else {
                break;
            };
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, key)) {
                if let Some(tab) = self.state.tabs().get(index) {
                    let target = tab.id;
                    self.state.dispatch(AppCommand::ActivateTab(target), ctx);
                }
                self.palette.close();
                break;
            }
        }
        // While the palette is open, it owns Enter/Escape/arrow keys; avoid
        // also acting on tab-management shortcuts this frame.
        if self.palette.is_open() {
            return;
        }
        let new_tab = ApplicationShortcut::NewSession.consume(ctx);
        let start_local_shell = ApplicationShortcut::StartLocalShell.consume(ctx);
        let close_tab = ApplicationShortcut::CloseActiveSurface.consume(ctx);
        let next_tab = ApplicationShortcut::NextSession.consume(ctx);
        let previous_tab = ApplicationShortcut::PreviousSession.consume(ctx);
        // Both bindings open Settings: the legacy macOS-only `Cmd+,`
        // convention (also present as a native app-menu accelerator) and the
        // cross-platform `SettingsHotkey` shown in Settings' own Keyboard
        // card. Both `consume` calls run unconditionally so neither chord is
        // ever left un-consumed by short-circuiting.
        let settings_legacy = ApplicationShortcut::Settings.consume(ctx);
        let settings_hotkey = ApplicationShortcut::SettingsHotkey.consume(ctx);
        let settings = settings_legacy || settings_hotkey;
        let zoom_in = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::COMMAND, egui::Key::Plus)
                    || input.consume_key(egui::Modifiers::COMMAND, egui::Key::Equals)
            });
        let zoom_out = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::ZoomOut.consume(ctx);
        let reset_zoom = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::ZoomReset.consume(ctx);
        let clear_terminal = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::ClearTerminal.consume(ctx);
        let reset_terminal = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::ResetTerminal.consume(ctx);
        let toggle_focus_mode = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::ToggleFocusMode.consume(ctx);

        if new_tab {
            self.state.dispatch(AppCommand::OpenLauncher, ctx);
        }
        if start_local_shell {
            self.state.dispatch(AppCommand::StartLocalSession, ctx);
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
        if zoom_in {
            self.zoom_active_session(ZoomCommand::In, ctx);
        }
        if zoom_out {
            self.zoom_active_session(ZoomCommand::Out, ctx);
        }
        if reset_zoom {
            self.zoom_active_session(ZoomCommand::Reset, ctx);
        }
        if clear_terminal {
            self.clear_active_terminal(ctx);
        }
        if reset_terminal {
            self.reset_active_terminal(ctx);
        }
        if toggle_focus_mode {
            self.toggle_focus_mode(ctx);
        }
    }

    fn zoom_active_session(&mut self, command: ZoomCommand, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        let changed = match command {
            ZoomCommand::In => session.view.zoom_in(),
            ZoomCommand::Out => session.view.zoom_out(),
            ZoomCommand::Reset => session.view.reset_zoom(),
        };
        if changed {
            self.overlays.transient_notice = Some((
                format!("Terminal zoom: {:.0} pt", session.view.font_size_points()),
                Instant::now() + Duration::from_millis(1_500),
            ));
            context.request_repaint();
        }
    }

    /// Resets the active session's terminal display state (screen, cursor,
    /// colors/attributes, modes) without touching its retained scrollback
    /// history. Mirrors what a real terminal does on `ESC c` (RIS) or a
    /// shell's `reset` command, but works even if the running program can't
    /// be asked to emit that sequence itself (e.g. it's wedged).
    fn reset_active_terminal(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        session.terminal.reset_to_initial_state();
        self.overlays.transient_notice = Some((
            "Terminal reset".to_owned(),
            Instant::now() + Duration::from_millis(1_500),
        ));
        // The palette closes as this command is dispatched; without
        // explicitly restoring focus the terminal is left unfocused and
        // the next keystroke is swallowed until the user presses Escape.
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    /// Clears the visible display and retained scrollback while preserving
    /// terminal modes and attributes, matching the conventional Cmd+K action.
    fn clear_active_terminal(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        session.terminal.ingest(b"\x1b[2J\x1b[3J\x1b[H");
        self.overlays.transient_notice = Some((
            "Terminal cleared".to_owned(),
            Instant::now() + Duration::from_millis(1_500),
        ));
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    /// Copies the active session's current selection to the system
    /// clipboard, mirroring what pressing the OS copy shortcut
    /// (`egui::Event::Copy`, handled in `route_egui_events`) would do while
    /// the terminal has focus and text is selected.
    fn copy_active_selection(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        if let Some(text) = session.view.selected_text(&session.terminal) {
            context.copy_text(text);
        }
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    /// Requests the OS deliver the clipboard's contents as a paste event,
    /// which the existing `egui::Event::Paste` handling in
    /// `route_egui_events` then routes into the focused terminal, the same
    /// path the OS paste shortcut uses.
    fn paste_into_active_session(&mut self, context: &egui::Context) {
        if !matches!(self.state.active_tab().content, TabContent::Session(_)) {
            return;
        }
        self.restore_active_terminal_focus();
        context.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        context.request_repaint();
    }

    fn toggle_focus_mode(&mut self, context: &egui::Context) {
        if !matches!(self.state.active_tab().content, TabContent::Session(_)) {
            return;
        }
        self.focus_mode = !self.focus_mode;
        self.overlays.transient_notice = Some((
            if self.focus_mode {
                format!(
                    "Focus Mode · {} → Exit Focus Mode",
                    ApplicationShortcut::CommandPalette
                        .label()
                        .expect("command palette always has a platform binding")
                )
            } else {
                "Focus Mode exited".to_owned()
            },
            Instant::now() + Duration::from_millis(1_500),
        ));
        let active = self.state.active();
        if let Some(session) = self.state.session_tab_mut(active) {
            session.view.request_focus_on_next_frame();
        }
        context.request_repaint();
    }

    fn show_transient_notice(&mut self, context: &egui::Context) {
        let Some((text, deadline)) = self.overlays.transient_notice.as_ref() else {
            return;
        };
        if Instant::now() >= *deadline {
            self.overlays.transient_notice = None;
            return;
        }
        let text = text.clone();
        egui::Area::new(egui::Id::new("fesTerm transient mode notice"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .interactable(false)
            .show(context, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(text).color(theme::TEXT_SECONDARY));
                    });
            });
        context.request_repaint_after(Duration::from_millis(100));
    }

    fn version_information() -> String {
        let mut information = format!(
            "fesTerm {}\nOS: {}\nArchitecture: {}\nUI: egui/eframe 0.36\nAuthorship: {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            AI_AUTHORSHIP_SUMMARY,
        );
        if let Some(commit) = option_env!("FESTERM_BUILD_COMMIT") {
            information.push_str("\nBuild: ");
            information.push_str(commit);
        }
        information
    }

    fn show_about(&mut self, context: &egui::Context, escape: bool) {
        if !self.overlays.about_open {
            return;
        }
        if escape {
            self.overlays.about_open = false;
            self.overlays.about_licenses_open = false;
            self.restore_active_terminal_focus();
            return;
        }

        #[derive(Clone, Copy)]
        enum UpdateAction {
            Check,
            Download,
            Install,
        }

        let mut close = false;
        let mut update_action = None;
        let update_status = self.updates.status().clone();
        let installation_kind = self.updates.installation_kind();
        let width = (context.content_rect().width() - 32.0).clamp(280.0, 420.0);
        egui::Modal::new(egui::Id::new("fesTerm about dialog"))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(context, |ui| {
                ui.set_width(width);
                ui.heading("About fesTerm");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if let Some(about_icon) = &self.about_icon {
                        ui.add(
                            egui::Image::new(about_icon)
                                .fit_to_exact_size(egui::vec2(48.0, 48.0))
                                .alt_text("fesTerm application icon"),
                        );
                    }
                    ui.vertical(|ui| {
                        ui.heading("fesTerm");
                        ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
                    });
                });
                ui.add_space(6.0);
                ui.label("A compact local, SSH, and serial terminal.");
                ui.label(egui::RichText::new(AI_AUTHORSHIP_SUMMARY).strong());
                ui.label(
                    egui::RichText::new(AI_AUTHORSHIP_DETAIL)
                        .small()
                        .color(theme::TEXT_SECONDARY),
                );
                ui.hyperlink_to("github.com/fes/fesTerm", "https://github.com/fes/fesTerm");
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Updates").strong());
                match &update_status {
                    UpdateStatus::Unavailable(message) => {
                        ui.label(*message);
                    }
                    UpdateStatus::Idle => {
                        if ui.button("Check for Updates").clicked() {
                            update_action = Some(UpdateAction::Check);
                        }
                    }
                    UpdateStatus::Checking => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Checking GitHub Releases…");
                        });
                    }
                    UpdateStatus::Current => {
                        ui.label("fesTerm is up to date.");
                        if ui.button("Check Again").clicked() {
                            update_action = Some(UpdateAction::Check);
                        }
                    }
                    UpdateStatus::Available(summary) => {
                        ui.label(format!("fesTerm {} is available.", summary.version));
                        if let Some(notes) = summary.notes.as_deref() {
                            let bounded = notes.chars().take(600).collect::<String>();
                            if !bounded.trim().is_empty() {
                                ui.label(
                                    egui::RichText::new(bounded)
                                        .small()
                                        .color(theme::TEXT_SECONDARY),
                                );
                            }
                        }
                        if installation_kind.can_install() {
                            if ui.button("Download Update").clicked() {
                                update_action = Some(UpdateAction::Download);
                            }
                        } else {
                            ui.label(
                                "This installation is package-managed. Use its package manager to update.",
                            );
                            ui.hyperlink_to(
                                "Open Releases",
                                "https://github.com/fes/fesTerm/releases",
                            );
                        }
                    }
                    UpdateStatus::Downloading(summary) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!(
                                "Downloading and verifying fesTerm {}…",
                                summary.version
                            ));
                        });
                    }
                    UpdateStatus::ReadyToInstall(summary) => {
                        ui.label(format!(
                            "fesTerm {} is downloaded and verified.",
                            summary.version
                        ));
                        if ui.button("Install and Restart").clicked() {
                            update_action = Some(UpdateAction::Install);
                        }
                    }
                    UpdateStatus::Installing(summary) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Installing fesTerm {}…", summary.version));
                        });
                    }
                    UpdateStatus::Installed(summary) => {
                        ui.label(format!(
                            "fesTerm {} was installed. Restart fesTerm if it remains open.",
                            summary.version
                        ));
                    }
                    UpdateStatus::Failed {
                        message,
                        retry_check,
                    } => {
                        ui.colored_label(theme::STATUS_ERROR, *message);
                        if *retry_check && ui.button("Try Again").clicked() {
                            update_action = Some(UpdateAction::Check);
                        }
                    }
                }
                if !matches!(update_status, UpdateStatus::Unavailable(_)) {
                    ui.label(
                        egui::RichText::new(
                            "Checks fesTerm’s public GitHub Releases only when requested. No profile, session, terminal, or device data is sent.",
                        )
                        .small()
                        .color(theme::TEXT_MUTED),
                    );
                    ui.hyperlink_to("Update endpoint", UpdateController::endpoint());
                }
                ui.add_space(6.0);
                if self.overlays.about_licenses_open {
                    egui::ScrollArea::vertical()
                        .id_salt("fesTerm license text")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "fesTerm\nMIT License\n\nThe workspace declares MIT licensing. The canonical source repository is linked above.",
                                )
                                    .monospace()
                                    .small(),
                            );
                            ui.label(
                                egui::RichText::new(
                                    "JetBrains Mono 2.304, Iosevka Term 34.8.1, JuliaMono 0.63.2, and Maple Mono 7.9 are bundled under the SIL Open Font License 1.1. Complete licenses, attribution, source archives, and checksums are stored under assets/fonts. JuliaMono and Maple Mono retain their upstream Reserved Font Names. Inter is not yet bundled.",
                                )
                                .small()
                                .color(theme::TEXT_MUTED),
                            );
                        });
                }
                ui.horizontal(|ui| {
                    if ui.button("Copy Version Information").clicked() {
                        context.copy_text(Self::version_information());
                    }
                    if self.overlays.about_licenses_open {
                        if ui.button("Hide Licenses").clicked() {
                            self.overlays.about_licenses_open = false;
                        }
                    } else if ui.button("Licenses").clicked() {
                        self.overlays.about_licenses_open = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        match update_action {
            Some(UpdateAction::Check) => self.updates.begin_check(),
            Some(UpdateAction::Download) => self.updates.begin_download(),
            Some(UpdateAction::Install) => self.updates.begin_install(),
            None => {}
        }
        if close {
            self.overlays.about_open = false;
            self.overlays.about_licenses_open = false;
            self.restore_active_terminal_focus();
        }
    }

    fn restore_active_terminal_focus(&mut self) {
        let active = self.state.active();
        if let Some(session) = self.state.session_tab_mut(active) {
            session.view.request_focus_on_next_frame();
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
                    TabContent::Profiles => (
                        "Profiles".to_owned(),
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
            InspectorTransport::Local { .. } => TransportFacts::Local,
            InspectorTransport::Ssh {
                username,
                host,
                port,
                ..
            } => TransportFacts::Ssh {
                username,
                host,
                port: *port,
            },
            InspectorTransport::Serial {
                device,
                baud_rate,
                data_bits,
                parity,
                stop_bits,
                flow_control,
            } => TransportFacts::Serial {
                device,
                baud_rate: *baud_rate,
                data_bits: serial_data_bits_label(*data_bits),
                parity: serial_parity_label(*parity),
                stop_bits: serial_stop_bits_label(*stop_bits),
                flow_control: serial_flow_control_label(*flow_control),
            },
        };
        let persistent_session = match &session.inspector_transport {
            InspectorTransport::Local {
                persistence: Some(persistence),
            }
            | InspectorTransport::Ssh {
                persistence: Some(persistence),
                ..
            } => Some(crate::inspector::PersistentSessionFacts {
                provider_label: persistence.provider_label,
                session_name: &persistence.session_name,
            }),
            InspectorTransport::Local { .. }
            | InspectorTransport::Ssh { .. }
            | InspectorTransport::Serial { .. } => None,
        };
        let type_label = match session.inspector_transport {
            InspectorTransport::Local { .. } => "Local shell",
            InspectorTransport::Ssh { .. } => "SSH",
            InspectorTransport::Serial { .. } => "Serial",
        };
        let state_message = match chip_status {
            ChipStatus::Failed => Some(match session.inspector_transport {
                InspectorTransport::Local { .. } => {
                    "The local shell could not start. Review Diagnostics for the failure detail."
                }
                InspectorTransport::Ssh { .. } => {
                    "The SSH session could not start. Review Diagnostics for the failure detail."
                }
                InspectorTransport::Serial { .. } => {
                    "The serial session could not start. Review Diagnostics for the failure detail."
                }
            }),
            ChipStatus::Disconnected => Some("The connection has been lost."),
            ChipStatus::Exited => Some("The session has exited."),
            ChipStatus::Reconnecting => Some(if persistent_session.is_some() {
                "Attempting to resume the durable remote session."
            } else {
                "Attempting to reconnect to the host."
            }),
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
                persistent_session,
            },
            close_requested,
        )
    }

    /// Bottom application status bar. Session identity remains in the chip;
    /// this footer shows only sourced grid/locality facts and transport state.
    /// Application surfaces keep the same 24 px geometry with empty content.
    fn show_status_bar(&self, ui: &mut egui::Ui) {
        let show_session_details = self.state.show_session_details();
        let (dimensions, system, status, status_label, detail) =
            match &self.state.active_tab().content {
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::SshAuthenticationRequired(_) => {
                    (None, None, ChipStatus::Neutral, "", None)
                }
                TabContent::Session(session) => {
                    let status = session.chip_status();
                    // Only relocate the detail here while chips are compact
                    // (`docs/gui-design.md` "Show session details in
                    // chips"): when chips already show it, repeating it in
                    // the status bar would duplicate stable session
                    // identity/title instead of merely relocating it.
                    let detail = (!show_session_details)
                        .then(|| {
                            let dynamic_title = session.terminal.title();
                            (!dynamic_title.is_empty())
                                .then(|| Self::display_secondary(dynamic_title))
                                .or_else(|| session.launch_secondary.clone())
                        })
                        .flatten();
                    (
                        session.view.dimensions_label(),
                        Some(session.system_label()),
                        status,
                        session.status_bar_label(),
                        detail,
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
                        detail: detail.as_deref(),
                    },
                );
            });
    }

    /// Shows the openssh-style host-key verification prompt only for the
    /// active SSH tab, rendered pty-styled (monospace, terminal background)
    /// in place of the terminal content — there is no real PTY output yet at
    /// this point in the connection — with keyboard-driven `[y/N]` capture
    /// instead of buttons, mirroring `ssh`'s own
    /// "Are you sure you want to continue connecting (yes/no)?" prompt. The
    /// returned command is dispatched after UI construction, so pressing a
    /// key only signals the SSH worker and never waits for network I/O on
    /// the GUI thread.
    fn show_host_key_prompt_ui(
        ui: &mut egui::Ui,
        tab_id: TabId,
        prompt: &HostKeyPrompt,
    ) -> Option<AppCommand> {
        if prompt.is_key_change() {
            return Self::show_changed_host_key_prompt_ui(ui, tab_id, prompt);
        }
        if !festerm_ui_egui::terminal_fonts_installed(ui.ctx()) {
            // Mirrors `TerminalView`'s own guard: the named terminal font
            // family only becomes usable after egui rebuilds its atlas at
            // the next pass boundary, so skip laying out text with it here.
            festerm_ui_egui::install_terminal_fonts(ui.ctx());
            ui.ctx().request_repaint();
            return None;
        }
        let host_port = Self::canonical_host_port(prompt.host(), prompt.port());
        let fingerprint = prompt.sha256_fingerprint();
        let mut decision = None;
        let font = festerm_ui_egui::terminal_font(festerm_ui_egui::DEFAULT_TERMINAL_FONT_SIZE);
        let mono = |text: String, color: egui::Color32| {
            egui::RichText::new(text).font(font.clone()).color(color)
        };

        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.label(mono(
                    format!("The authenticity of host '{host_port}' can't be established."),
                    theme::TEXT_PRIMARY,
                ));
                ui.label(mono(
                    format!("ED25519 key fingerprint is {fingerprint}."),
                    theme::TEXT_PRIMARY,
                ));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(mono(
                        "Are you sure you want to continue connecting (yes/no)? [y/N] ".to_owned(),
                        theme::TEXT_PRIMARY,
                    ));
                    ui.label(mono(
                        screens::pty_cursor_glyph(ui).to_owned(),
                        theme::TEXT_PRIMARY,
                    ));
                });
                ui.add_space(4.0);
                ui.label(mono(
                    "Press 'a' to accept and remember this host for future connections.".to_owned(),
                    theme::TEXT_SECONDARY,
                ));
            });

        // Keyboard-driven, matching the "feel" of a real pty prompt: no
        // terminal view is shown this frame to compete for these keys, so
        // consuming them here is sufficient to capture input without a
        // dedicated focus target.
        ui.ctx().input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Y) {
                decision = Some(HostKeyTrustDecision::AcceptOnce);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::A) {
                decision = Some(HostKeyTrustDecision::AcceptAndPersist);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::N)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
            {
                decision = Some(HostKeyTrustDecision::Reject);
            }
        });

        decision.map(|decision| AppCommand::ResolveHostKeyTrust {
            tab: tab_id,
            decision,
        })
    }

    /// The changed-key warning path (ADR 0020, `docs/gui-action-graph.md`
    /// `TRUST-04`): a persistent trust record already names a different
    /// fingerprint for this host, so this never offers an ordinary
    /// low-friction Accept Once. The only way to proceed is to type the
    /// literal word `yes` and press Enter, mirroring the raw, unechoed
    /// keystroke capture used by the live SSH password prompt but echoing
    /// what is typed here (it is not secret) so the deliberate act is
    /// visible. Anything else, or Escape, cancels the connection.
    fn show_changed_host_key_prompt_ui(
        ui: &mut egui::Ui,
        tab_id: TabId,
        prompt: &HostKeyPrompt,
    ) -> Option<AppCommand> {
        if !festerm_ui_egui::terminal_fonts_installed(ui.ctx()) {
            festerm_ui_egui::install_terminal_fonts(ui.ctx());
            ui.ctx().request_repaint();
            return None;
        }
        let host_port = Self::canonical_host_port(prompt.host(), prompt.port());
        let fingerprint = prompt.sha256_fingerprint();
        let previous_fingerprint = prompt.previously_trusted_fingerprint().unwrap_or_default();
        let state_id = ui.id().with(("changed_host_key_prompt", tab_id));
        let mut typed: String = ui.data_mut(|data| data.get_temp(state_id).unwrap_or_default());
        let mut decision = None;
        let font = festerm_ui_egui::terminal_font(festerm_ui_egui::DEFAULT_TERMINAL_FONT_SIZE);
        let mono = |text: String, color: egui::Color32| {
            egui::RichText::new(text).font(font.clone()).color(color)
        };

        egui::Frame::new()
            .fill(theme::SURFACE_TERMINAL)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.label(mono(
                    "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@".to_owned(),
                    theme::STATUS_ERROR,
                ));
                ui.label(mono(
                    "@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @".to_owned(),
                    theme::STATUS_ERROR,
                ));
                ui.label(mono(
                    "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@".to_owned(),
                    theme::STATUS_ERROR,
                ));
                ui.add_space(4.0);
                ui.label(mono(
                    format!(
                        "The key previously trusted for '{host_port}' was {previous_fingerprint}."
                    ),
                    theme::TEXT_PRIMARY,
                ));
                ui.label(mono(
                    format!("The server now presents a different key: {fingerprint}."),
                    theme::TEXT_PRIMARY,
                ));
                ui.label(mono(
                    "This could mean someone is intercepting this connection, or the host's key was legitimately changed.".to_owned(),
                    theme::TEXT_PRIMARY,
                ));
                ui.add_space(4.0);
                ui.label(mono(
                    "Type 'yes' and press Enter to replace the trusted key and continue, or press Escape to cancel.".to_owned(),
                    theme::TEXT_SECONDARY,
                ));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.label(mono(format!("> {typed}"), theme::TEXT_PRIMARY));
                    ui.label(mono(
                        screens::pty_cursor_glyph(ui).to_owned(),
                        theme::TEXT_PRIMARY,
                    ));
                });
            });

        let mut submitted = false;
        let mut cancelled = false;
        ui.ctx().input_mut(|input| {
            input.events.retain(|event| match event {
                egui::Event::Text(text) => {
                    typed.push_str(text);
                    false
                }
                egui::Event::Key {
                    key: egui::Key::Backspace,
                    pressed: true,
                    ..
                } => {
                    typed.pop();
                    false
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                } => {
                    submitted = true;
                    false
                }
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                } => {
                    cancelled = true;
                    false
                }
                _ => true,
            });
        });

        if cancelled {
            decision = Some(HostKeyTrustDecision::Reject);
        } else if submitted {
            decision = Some(if typed.trim() == "yes" {
                HostKeyTrustDecision::AcceptAndPersist
            } else {
                HostKeyTrustDecision::Reject
            });
        }

        if decision.is_some() {
            ui.data_mut(|data| data.remove_temp::<String>(state_id));
        } else {
            ui.data_mut(|data| data.insert_temp(state_id, typed));
        }

        decision.map(|decision| AppCommand::ResolveHostKeyTrust {
            tab: tab_id,
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
    fn logic(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.sync_native_window_chrome(frame);
        self.process_pending_password_store(context);
        self.check_wake_monitor_signal();
        self.pump_all_sessions(context);
        self.state.reprompt_rejected_ssh_passwords(context);
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
        if !self.terminal_fonts_installed {
            self.terminal_font_generation = festerm_ui_egui::install_terminal_font_family(
                ui.ctx(),
                terminal_font_family(self.state.terminal_font()),
            );
            self.terminal_fonts_installed = true;
            // `set_fonts` rebuilds egui's atlas after this pass. A test app
            // can begin directly on a terminal surface (unlike production,
            // which installs in its constructor), so do not request a named
            // family until the following repaint.
            ui.ctx().request_repaint();
            return;
        }
        self.process_pending_password_store(ui.ctx());
        self.updates.poll();
        if self.updates.status().is_busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.handle_native_menu_commands(ui.ctx());
        self.handle_shortcuts(ui.ctx());
        self.update_native_menu();
        if self.focus_mode && !matches!(self.state.active_tab().content, TabContent::Session(_)) {
            self.focus_mode = false;
        }

        // A destructive confirmation or the About modal owns Escape before
        // the terminal input adapter sees raw events. Backdrop clicks are
        // intentionally ignored. The key is only consumed when some modal
        // overlay is actually open, so other Escape handlers (inspector,
        // chip rename) still see the event when neither applies.
        let modal_owns_escape = self.overlays.blocks_terminal_input();
        let escape_pressed = modal_owns_escape
            && ui
                .ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        let confirmation_escape = escape_pressed
            && (self.overlays.pending_close.is_some()
                || self.overlays.pending_paste.is_some()
                || self.overlays.pending_settings_reset.is_some());
        let about_escape = escape_pressed && self.overlays.about_open;

        if !self.focus_mode {
            let (chips, active_chip) = self.chip_view_models();
            let inspector_open = self.state.inspector_open();
            let inspector_available =
                matches!(self.state.active_tab().content, TabContent::Session(_));
            let actions = chrome::show(
                ui,
                &chips,
                active_chip,
                inspector_open,
                inspector_available,
                self.state.chip_layout(),
                self.state.show_session_details(),
            );
            self.dispatch_chrome_actions(actions, &ui.ctx().clone());
        }
        let inspector_open = self.state.inspector_open();
        // Consume Escape before `TerminalView::show` routes raw input so the
        // dismissal key can never leak into Vim, Emacs, or another TUI.
        let inspector_escape = inspector_open
            && ui
                .ctx()
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));

        if self.state.status_bar_visible() && !self.focus_mode {
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
        let paste_was_pending = self.overlays.pending_paste.is_some();
        let mut deferred_pastes = Vec::new();
        let chip_layout = self.state.chip_layout();
        let native_store_available = self.native_store_available();
        let secure_storage_status = self.secure_storage_status_message();
        let active_tab_id = self.state.active();
        let scroll_speed_multiplier = self.state.scroll_speed().multiplier();
        let terminal_font_set = TerminalFontSet::new(
            terminal_font_family(self.state.terminal_font()),
            self.state.terminal_ligatures(),
            self.terminal_font_generation,
        );
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
                        screens::SettingsViewModel {
                            chip_layout,
                            status_bar_visible: self.state.status_bar_visible(),
                            show_session_details: self.state.show_session_details(),
                            confirm_session_close: self.state.confirm_session_close(),
                            restore_workspace: self.state.restore_workspace(),
                            terminal_font: self.state.terminal_font(),
                            terminal_ligatures: self.state.terminal_ligatures(),
                            scroll_speed: self.state.scroll_speed(),
                        },
                        ApplicationShortcut::CommandPalette
                            .label()
                            .unwrap_or("(unbound)"),
                        ApplicationShortcut::SettingsHotkey
                            .label()
                            .unwrap_or("(unbound)"),
                    );
                }
                TabContent::Profiles => {
                    let pending_edit = self.state.take_pending_profile_edit();
                    screen_command = screens::show_profiles(
                        ui,
                        active_tab_id,
                        self.state.configuration(),
                        pending_edit,
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
                    session.view.set_font_set(terminal_font_set);
                    let host_key_prompt = session.host_key_prompt().cloned();
                    let password_prompt = host_key_prompt
                        .is_none()
                        .then(|| session.password_prompt().cloned())
                        .flatten();
                    if let Some(prompt) = &host_key_prompt {
                        screen_command = Self::show_host_key_prompt_ui(ui, active_tab_id, prompt);
                    } else if let Some(prompt) = &password_prompt {
                        screen_command =
                            screens::show_ssh_live_password_prompt(ui, active_tab_id, prompt);
                    } else {
                        let options = festerm_ui_egui::TerminalViewOptions {
                            paste_available: session.accepts_input(),
                            terminal_input_enabled: !self.overlays.blocks_terminal_input()
                                && !self.palette.is_open(),
                            keyboard_input_enabled: session.accepts_typed_input(),
                            defer_paste_to_application: true,
                            scroll_speed_multiplier,
                        };
                        session.view.show_with_options(
                            ui,
                            &mut session.terminal,
                            &mut session.controller,
                            options,
                        );
                        deferred_pastes = session.view.take_paste_requests();
                    }
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
        } else if !self.overlays.blocks_terminal_input() && deferred_pastes.len() == 1 {
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
                AppCommand::StartStoredPasswordSshProfile { profile_id } => {
                    self.start_stored_password_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StartConfiguredSshProfile { profile_id } => {
                    self.start_configured_ssh_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StoreSshPassword {
                    profile_id,
                    password,
                    options,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    options,
                    true,
                    &ui.ctx().clone(),
                ),
                AppCommand::StoreProfilePassword {
                    profile_id,
                    password,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    festerm_ssh::SshSessionOptions::new(),
                    false,
                    &ui.ctx().clone(),
                ),
                AppCommand::StoreProfilePrivateKey {
                    profile_id,
                    private_key,
                } => self.store_private_key_for_profile(
                    profile_id,
                    private_key,
                    festerm_ssh::SshSessionOptions::new(),
                    false,
                    &ui.ctx().clone(),
                ),
                AppCommand::SaveProfile { profile } => self.save_profile(profile),
                AppCommand::DeleteProfile { identifier } => self.delete_profile(&identifier),
                AppCommand::ReorderProfiles { moved, before } => {
                    self.reorder_profiles(&moved, before.as_deref());
                }
                AppCommand::CloseTab(id) => {
                    self.request_close_tab(id, &ui.ctx().clone());
                }
                command @ (AppCommand::ToggleChipLayout
                | AppCommand::ToggleStatusBar
                | AppCommand::ToggleShowSessionDetails
                | AppCommand::ToggleConfirmSessionClose
                | AppCommand::ToggleTerminalLigatures
                | AppCommand::SetScrollSpeed(_)) => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(command, &context);
                    self.persist_interface_settings();
                }
                AppCommand::SetTerminalFont(font) => {
                    let context = ui.ctx().clone();
                    self.state
                        .dispatch(AppCommand::SetTerminalFont(font), &context);
                    self.reinstall_terminal_font(&context);
                    self.persist_interface_settings();
                }
                AppCommand::ToggleRestoreWorkspace => {
                    let context = ui.ctx().clone();
                    self.state
                        .dispatch(AppCommand::ToggleRestoreWorkspace, &context);
                    self.persist_interface_settings();
                    // Turning the preference off scrubs any previously
                    // saved tab list from disk immediately, rather than
                    // leaving it dormant until the next unrelated write -
                    // "explicit" means no stale snapshot can resurface
                    // later just by re-enabling the toggle.
                    if !self.state.restore_workspace() {
                        self.clear_saved_workspace();
                    }
                }
                AppCommand::ResetInterfaceSettings => {
                    self.request_reset_interface_settings(&ui.ctx().clone());
                }
                AppCommand::ResolveHostKeyTrust { tab, decision } => {
                    let context = ui.ctx().clone();
                    if matches!(decision, HostKeyTrustDecision::AcceptAndPersist) {
                        self.persist_known_host_trust(tab);
                    }
                    self.state
                        .dispatch(AppCommand::ResolveHostKeyTrust { tab, decision }, &context);
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
            }
        }

        if self.overlays.pending_close.is_some() {
            self.show_close_confirmation(ui.ctx(), confirmation_escape);
        } else if self.overlays.pending_settings_reset.is_some() {
            self.show_settings_reset_confirmation(ui.ctx(), confirmation_escape);
        } else {
            self.show_paste_confirmation(ui.ctx(), confirmation_escape);
        }

        self.show_about(ui.ctx(), about_escape);

        self.show_transient_notice(ui.ctx());

        // Autosave the workspace exactly once per frame that actually
        // changed it, but only when the user has explicitly opted into
        // workspace restore (`docs/gui-design.md` "Workspace restore" -
        // unlike chip-layout/status-bar preferences, tab contents are not
        // meant to persist implicitly). `take_workspace_dirty` still runs
        // every frame regardless, so the flag never piles up while the
        // preference is off.
        if self.state.take_workspace_dirty() && self.state.restore_workspace() {
            self.save_workspace();
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
            secure_storage_feedback: None,
            inspector_restore_focus: None,
            rename_restore_focus: None,
            rename_restore_tab: None,
            overlays: OverlayState::default(),
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
            wake_monitor: None,
            wake_requested: Arc::new(AtomicBool::new(false)),
            focus_mode: false,
            terminal_fonts_installed: false,
            terminal_font_generation: TerminalFontGeneration::default(),
            about_icon: None,
            updates: UpdateController::unavailable_for_test(),
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
    fn wake_monitor_signal_drives_a_liveness_pass_and_then_clears_itself() {
        // No real wake monitor is installed by `for_test_with_live_session`
        // (that only happens via `install_wake_monitor`, called from the
        // real composition root); this simulates the OS-thread callback
        // firing directly, the same way any of the three platform monitors
        // would signal it.
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.wake_requested.store(true, Ordering::Release);

        app.check_wake_monitor_signal();

        assert!(!app.wake_requested.load(Ordering::Acquire));

        // A second pass with nothing pending is a harmless no-op.
        app.check_wake_monitor_signal();
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
    fn resetting_already_default_interface_settings_needs_no_confirmation() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());

        app.request_reset_interface_settings(&context);

        assert!(app.overlays.pending_settings_reset.is_none());
        assert_eq!(app.state.interface_settings(), InterfaceSettings::DEFAULT);
    }

    #[test]
    fn resetting_changed_interface_settings_is_safe_by_default_and_confirmed_deliberately() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.state.dispatch(AppCommand::ToggleChipLayout, &context);
        app.state.dispatch(AppCommand::ToggleStatusBar, &context);
        assert_ne!(app.state.interface_settings(), InterfaceSettings::DEFAULT);

        app.request_reset_interface_settings(&context);
        assert!(app.overlays.pending_settings_reset.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 400.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Cancel").is_focused());

        harness.get_by_label("Cancel").click();
        harness.step();
        assert!(harness.state().overlays.pending_settings_reset.is_none());
        assert_ne!(
            harness.state().state.interface_settings(),
            InterfaceSettings::DEFAULT
        );

        harness
            .state_mut()
            .request_reset_interface_settings(&context);
        harness.step();
        harness.get_by_label("Reset").click();
        harness.step();
        assert!(harness.state().overlays.pending_settings_reset.is_none());
        assert_eq!(
            harness.state().state.interface_settings(),
            InterfaceSettings::DEFAULT
        );
    }

    #[test]
    fn selecting_a_terminal_font_reinstalls_the_atlas_with_a_new_generation() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.terminal_font_generation = festerm_ui_egui::install_terminal_fonts(&context);
        let previous = app.terminal_font_generation;

        app.state.dispatch(
            AppCommand::SetTerminalFont(TerminalFontPreference::JuliaMono),
            &context,
        );
        app.reinstall_terminal_font(&context);

        assert_ne!(app.terminal_font_generation, previous);
        assert_eq!(
            app.state.interface_settings().terminal_font(),
            TerminalFontPreference::JuliaMono
        );
    }

    #[test]
    fn live_close_confirmation_is_safe_by_default_and_confirmed_deliberately() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.request_close_tab(tab, &context);
        assert!(app.overlays.pending_close.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Cancel").is_focused());

        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(harness.state().overlays.pending_close.is_some());
        assert_eq!(harness.state().state.active(), tab);

        harness.get_by_label("Close Session").click();
        harness.step();
        assert!(harness.state().overlays.pending_close.is_none());
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
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().overlays.pending_close.is_none());
        assert_eq!(harness.state().state.active(), tab);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));
    }

    #[test]
    fn disabled_close_confirmation_closes_live_session_immediately() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.state
            .dispatch(AppCommand::ToggleConfirmSessionClose, &context);

        app.request_close_tab(tab, &context);

        assert!(app.overlays.pending_close.is_none());
        assert!(matches!(
            app.state.active_tab().content,
            TabContent::Launcher
        ));
    }

    #[test]
    fn focus_mode_is_explicit_terminal_only_and_escape_does_not_exit() {
        let context = egui::Context::default();
        let (app, tab) = FesTermApp::for_test_with_live_session(&context);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.state_mut().dispatch_palette_selection(6, &context);
        harness.step();
        assert!(harness.state().focus_mode);
        assert_eq!(harness.state().state.active(), tab);
        assert!(harness.query_by_label("Local Shell chip").is_none());
        assert!(harness.query_by_label_contains("Focus Mode ·").is_some());

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().focus_mode, "Escape belongs to the terminal");

        harness.state_mut().dispatch_palette_selection(6, &context);
        harness.step();
        assert!(!harness.state().focus_mode);
        assert!(harness.query_by_label("Local Shell chip").is_some());

        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenLauncher, &context);
        harness.state_mut().focus_mode = true;
        harness.step();
        assert!(!harness.state().focus_mode);
    }

    #[test]
    fn about_festerm_is_always_the_last_palette_entry() {
        let context = egui::Context::default();
        let (app, _tab) = FesTermApp::for_test_with_live_session(&context);
        let items = app.palette_items();
        assert_eq!(
            items.last().map(|item| item.label.as_str()),
            Some("About fesTerm"),
            "About fesTerm must sort after every tab and action entry"
        );
    }

    #[test]
    fn zoom_palette_commands_change_only_the_active_session_and_reset() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "Zoom In"));
        assert!(items.iter().any(|item| item.label == "Zoom Out"));
        assert!(items.iter().any(|item| item.label == "Reset Zoom"));

        app.dispatch_palette_selection(7, &context);
        assert_eq!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .view
                .font_size_points(),
            15.0
        );
        app.dispatch_palette_selection(8, &context);
        assert_eq!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .view
                .font_size_points(),
            14.0
        );
        app.dispatch_palette_selection(7, &context);
        app.dispatch_palette_selection(9, &context);
        assert_eq!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .view
                .font_size_points(),
            14.0
        );
    }

    #[test]
    fn close_copy_and_paste_palette_entries_show_their_keystrokes() {
        let context = egui::Context::default();
        let (app, _tab) = FesTermApp::for_test_with_live_session(&context);
        let items = app.palette_items();

        let close = items
            .iter()
            .find(|item| item.label == "Close Session…")
            .expect("a live session tab has a close entry");
        assert_eq!(
            close.hint.as_deref(),
            ApplicationShortcut::CloseActiveSurface.label()
        );

        let copy = items
            .iter()
            .find(|item| item.label == "Copy")
            .expect("a session tab offers a Copy entry");
        assert_eq!(
            copy.hint.as_deref(),
            Some(if cfg!(target_os = "macos") {
                "\u{2318}+C"
            } else {
                "Ctrl+C"
            })
        );

        let paste = items
            .iter()
            .find(|item| item.label == "Paste")
            .expect("a session tab offers a Paste entry");
        assert_eq!(
            paste.hint.as_deref(),
            Some(if cfg!(target_os = "macos") {
                "\u{2318}+V"
            } else {
                "Ctrl+V"
            })
        );
    }

    #[test]
    fn paste_palette_command_requests_an_os_clipboard_paste() {
        const PASTE: u64 = 14;
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);

        app.dispatch_palette_selection(PASTE, &context);
        context.viewport(|viewport| {
            assert!(
                viewport
                    .commands
                    .contains(&egui::ViewportCommand::RequestPaste),
                "selecting Paste must ask the OS to deliver clipboard contents"
            );
        });
    }

    #[test]
    fn reset_terminal_palette_command_clears_screen_but_keeps_scrollback() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "Reset Terminal"));
        assert!(items.iter().any(|item| item.label == "Clear Terminal"));

        {
            let session = app
                .state
                .session_tab_mut(tab)
                .expect("active terminal session");
            for line in 1..=60 {
                session
                    .terminal
                    .ingest(format!("line{line}\r\n").as_bytes());
            }
            session.terminal.ingest(b"\x1b[1;31mred bold");
        }
        let scrollback_before = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session")
            .terminal
            .scrollback_stats()
            .logical_lines();
        assert!(scrollback_before > 0);

        const RESET_TERMINAL: u64 = 11;
        app.dispatch_palette_selection(RESET_TERMINAL, &context);

        let session = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session");
        assert_eq!(
            session.terminal.attributes(),
            festerm_core::Attributes::NONE
        );
        assert_eq!(
            session.terminal.scrollback_stats().logical_lines(),
            scrollback_before,
            "reset must not clear scrollback"
        );
    }

    #[test]
    fn clear_terminal_clears_display_and_scrollback_without_resetting_attributes() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        {
            let session = app
                .state
                .session_tab_mut(tab)
                .expect("active terminal session");
            for line in 1..=60 {
                session
                    .terminal
                    .ingest(format!("line{line}\r\n").as_bytes());
            }
            session.terminal.ingest(b"\x1b[1;31mred bold");
            assert!(session.terminal.scrollback_stats().logical_lines() > 0);
            assert_ne!(
                session.terminal.attributes(),
                festerm_core::Attributes::NONE
            );
        }

        app.clear_active_terminal(&context);

        let session = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session");
        assert_eq!(session.terminal.scrollback_stats().logical_lines(), 0);
        assert_eq!(
            session
                .terminal
                .row_text(0)
                .expect("first terminal row")
                .trim(),
            ""
        );
        assert_ne!(
            session.terminal.attributes(),
            festerm_core::Attributes::NONE,
            "clear must not perform a terminal reset"
        );
    }

    #[test]
    fn terminal_shortcuts_follow_platform_conventions() {
        if cfg!(target_os = "macos") {
            assert_eq!(
                ApplicationShortcut::ClearTerminal.label(),
                Some("\u{2318}+K")
            );
            assert_eq!(
                ApplicationShortcut::ResetTerminal.label(),
                Some("Option+\u{2318}+R")
            );
        } else {
            assert_eq!(
                ApplicationShortcut::ClearTerminal.label(),
                Some("Ctrl+Shift+K")
            );
            assert_eq!(
                ApplicationShortcut::ResetTerminal.label(),
                Some("Ctrl+Shift+R")
            );
        }
    }

    #[test]
    fn local_shell_and_focus_mode_shortcuts_dispatch_their_commands() {
        let mut harness = harness();
        harness.run();

        let (local_modifiers, local_key) = ApplicationShortcut::StartLocalShell
            .chord()
            .expect("local-shell shortcut");
        harness.key_press_modifiers(local_modifiers, local_key);
        harness.step();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));

        let (focus_modifiers, focus_key) = ApplicationShortcut::ToggleFocusMode
            .chord()
            .expect("focus-mode shortcut");
        harness.key_press_modifiers(focus_modifiers, focus_key);
        harness.step();
        assert!(harness.state().focus_mode);

        harness.key_press_modifiers(focus_modifiers, focus_key);
        harness.step();
        assert!(!harness.state().focus_mode);
    }

    #[test]
    fn active_tab_eviction_shows_a_one_shot_transient_notice() {
        // M9 eviction notices: the first time the active tab's retained
        // scrollback discards a logical line to stay within its configured
        // memory bound, the user must see a visible signal rather than
        // history silently getting shorter than expected.
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        {
            let session = app
                .state
                .session_tab_mut(tab)
                .expect("active terminal session");
            // A tiny viewport (mirrors festerm-core's own eviction test)
            // scrolls almost every ingested line into history immediately,
            // and a tiny scrollback budget forces eviction after only a few
            // of those lines instead of needing megabytes of filler.
            let tiny_dimensions = festerm_core::Dimensions::new(4, 2)
                .expect("4x2 is a valid terminal size for forcing eviction");
            session.terminal = festerm_core::Terminal::with_scrollback_limit(tiny_dimensions, 1024)
                .expect("small scrollback limit is valid");
            for line in 1..=64 {
                session
                    .terminal
                    .ingest(format!("line{line}\r\n").as_bytes());
            }
        }
        assert!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .terminal
                .scrollback_stats()
                .evicted_lines()
                > 0,
            "the tiny scrollback limit must have forced at least one eviction"
        );

        app.pump_all_sessions(&context);

        assert!(
            app.overlays
                .transient_notice
                .as_ref()
                .is_some_and(|(text, _)| text.contains("Scrollback limit reached")),
            "eviction must surface a transient notice"
        );
        assert!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .eviction_notice_shown,
            "the notice must latch so it isn't re-triggered every frame"
        );

        // Dismiss the notice and pump again: continued eviction from
        // sustained output must not re-show it.
        app.overlays.transient_notice = None;
        app.pump_all_sessions(&context);
        assert!(
            app.overlays.transient_notice.is_none(),
            "a latched eviction notice must not repeat on every frame"
        );
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn exited_session_becomes_read_only_and_stops_delivering_typed_input() {
        // M9 disconnected-history lifecycle: once a session's process has
        // exited, `docs/gui-action-graph.md`'s `HIST-06`/`SSH-02` invariant
        // says scrollback/selection/copy remain available but typed input
        // must be absent/ignored rather than attempted against a dead
        // transport.
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.state
            .session_tab_mut(tab)
            .expect("active terminal session")
            .controller
            .set_lifecycle_for_test(festerm_session::SessionLifecycle::Exited(
                festerm_session::SessionExit::with_exit_code(0),
            ));

        let history_before = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session")
            .terminal
            .row_text(0);

        let session = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session");
        assert!(
            !session.accepts_typed_input(),
            "an exited session must stop accepting typed input"
        );

        // History remains readable/unchanged; nothing was corrupted by the
        // (correctly refused) input attempt.
        assert_eq!(
            app.state
                .session_tab_mut(tab)
                .expect("active terminal session")
                .terminal
                .row_text(0),
            history_before,
            "read-only history must remain intact after the session exits"
        );
    }

    #[test]
    fn reset_terminal_click_in_the_palette_does_not_select_or_swallow_terminal_input() {
        // Regression test: clicking a command-palette row (e.g. "Reset
        // Terminal") over a live terminal session must not leak that same
        // click through to the terminal grid as a text selection, and must
        // not leave any stray egui focus/interaction state that swallows
        // the next keystroke until the user presses Escape.
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        let selection_before = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.selection().clone(),
            _ => panic!("Enter must start a local session"),
        };

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::P,
        );
        harness.step();
        harness.get_by_label_contains("Reset Terminal").click();
        // The transient "Terminal reset" toast keeps requesting repaints
        // until it expires; one step is enough to observe the result of
        // this frame's click without waiting on that timer.
        harness.step();

        assert!(
            !harness.state().palette.is_open(),
            "selecting a command must close the palette"
        );
        let selection_after = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session.view.selection().clone(),
            _ => panic!("the session tab must remain active"),
        };
        assert_eq!(
            selection_after, selection_before,
            "the dismissing click must not leave a stray terminal selection"
        );

        let bytes_before = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => unreachable!("checked above"),
        };
        harness.event(egui::Event::Text("x".to_owned()));
        harness.step();
        let bytes_after = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => unreachable!("checked above"),
        };
        assert!(
            bytes_after > bytes_before,
            "a keystroke right after dismissing the palette must reach the terminal without an extra Escape"
        );
    }

    #[test]
    fn clear_terminal_palette_command_clears_display_and_scrollback() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);

        {
            let session = app
                .state
                .session_tab_mut(tab)
                .expect("active terminal session");
            session
                .terminal
                .ingest(b"one\r\ntwo\r\nthree\r\nfour\r\nprompt$ ");
        }
        const CLEAR_TERMINAL: u64 = 12;
        app.dispatch_palette_selection(CLEAR_TERMINAL, &context);

        let session = app
            .state
            .session_tab_mut(tab)
            .expect("active terminal session");
        assert_eq!(
            session.terminal.scrollback_stats().logical_lines(),
            0,
            "history should be cleared"
        );
        assert!(
            session
                .terminal
                .row_text(0)
                .expect("first terminal row")
                .trim()
                .is_empty(),
            "the visible screen should be cleared"
        );
    }

    #[test]
    fn about_dialog_is_bounded_truthful_and_escape_returns_to_prior_surface() {
        let context = egui::Context::default();
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.state_mut().dispatch_palette_selection(10, &context);
        harness.step();
        assert!(harness.state().overlays.about_open);
        harness.get_by_label("About fesTerm");
        let version_label = format!("Version {}", env!("CARGO_PKG_VERSION"));
        harness.get_by_label(&version_label);
        harness.get_by_label("A compact local, SSH, and serial terminal.");
        harness.get_by_label(AI_AUTHORSHIP_SUMMARY);
        harness.get_by_label(AI_AUTHORSHIP_DETAIL);
        harness.get_by_label("Copy Version Information");
        harness.get_by_label("Licenses");
        harness.state_mut().overlays.about_licenses_open = true;
        harness.step();
        assert!(harness.state().overlays.about_licenses_open);
        assert!(harness.query_by_label_contains("MIT License").is_some());

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(!harness.state().overlays.about_open);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));

        let version = FesTermApp::version_information();
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.contains(std::env::consts::OS));
        assert!(version.contains(std::env::consts::ARCH));
        assert!(version.contains(AI_AUTHORSHIP_SUMMARY));
        assert!(!version.contains("HOME="));
    }

    #[test]
    fn about_exposes_updates_only_for_a_configured_packaged_build() {
        let mut unavailable = FesTermApp::for_test_with_configuration(Configuration::empty());
        unavailable.overlays.about_open = true;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), unavailable);
        harness.run();
        harness.get_by_label("Update checks are available in packaged releases.");
        assert!(harness.query_by_label("Check for Updates").is_none());

        harness.state_mut().updates = UpdateController::configured_for_test();
        harness.step();
        harness.get_by_label("Check for Updates");
        harness
            .get_by_label_contains("Checks fesTerm’s public GitHub Releases only when requested");
    }

    fn harness() -> Harness<'static, FesTermApp> {
        harness_with_configuration(Configuration::empty())
    }

    fn harness_with_configuration(configuration: Configuration) -> Harness<'static, FesTermApp> {
        Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(
                |ui, app: &mut FesTermApp| {
                    ui.ctx().set_visuals(theme::default_visuals());
                    app.ui_content(ui);
                },
                FesTermApp::for_test_with_configuration(configuration),
            )
    }

    fn test_host_key_prompt() -> HostKeyPrompt {
        HostKeyPrompt::new(
            "ssh.example.test",
            22,
            "SHA256:UCUiLr7Pjs9wFFJMDByLgc3NrtdU344OgUM45wZPcIQ",
        )
    }

    #[test]
    fn host_key_prompt_ui_accepts_and_remembers_a_first_seen_host_on_a_key() {
        struct PromptHarnessState {
            prompt: HostKeyPrompt,
            tab_id: TabId,
            command: Option<AppCommand>,
        }

        let tab_id = AppState::for_test().active();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    if let Some(command) =
                        FesTermApp::show_host_key_prompt_ui(ui, state.tab_id, &state.prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt: test_host_key_prompt(),
                    tab_id,
                    command: None,
                },
            );
        harness.run();
        harness.run();

        harness.key_press(egui::Key::A);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ResolveHostKeyTrust {
                decision: HostKeyTrustDecision::AcceptAndPersist,
                ..
            })
        ));
    }

    #[test]
    fn changed_host_key_prompt_ui_requires_typing_yes_to_replace_trust() {
        struct PromptHarnessState {
            prompt: HostKeyPrompt,
            tab_id: TabId,
            command: Option<AppCommand>,
        }

        let tab_id = AppState::for_test().active();
        let prompt = test_host_key_prompt()
            .with_previously_trusted_fingerprint("SHA256:previouslyTrustedButDifferent");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    if let Some(command) =
                        FesTermApp::show_host_key_prompt_ui(ui, state.tab_id, &state.prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt,
                    tab_id,
                    command: None,
                },
            );
        harness.run();
        harness.run();

        // A bare Enter (nothing typed, so not "yes") must reject rather
        // than offer any low-friction accept for a changed key.
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ResolveHostKeyTrust {
                decision: HostKeyTrustDecision::Reject,
                ..
            })
        ));
    }

    #[test]
    fn changed_host_key_prompt_ui_accepts_and_persists_only_after_typing_the_literal_word_yes() {
        struct PromptHarnessState {
            prompt: HostKeyPrompt,
            tab_id: TabId,
            command: Option<AppCommand>,
        }

        let tab_id = AppState::for_test().active();
        let prompt = test_host_key_prompt()
            .with_previously_trusted_fingerprint("SHA256:previouslyTrustedButDifferent");
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 200.0))
            .build_ui_state(
                |ui, state: &mut PromptHarnessState| {
                    if let Some(command) =
                        FesTermApp::show_host_key_prompt_ui(ui, state.tab_id, &state.prompt)
                    {
                        state.command = Some(command);
                    }
                },
                PromptHarnessState {
                    prompt,
                    tab_id,
                    command: None,
                },
            );
        harness.run();
        harness.run();

        for character in "yes".chars() {
            harness.event(egui::Event::Text(character.to_string()));
        }
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ResolveHostKeyTrust {
                decision: HostKeyTrustDecision::AcceptAndPersist,
                ..
            })
        ));
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
    fn stored_ssh_password_path_uses_injected_memory_store_and_persists_only_reference() {
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
        let context = egui::Context::default();
        harness.state_mut().store_password_for_profile(
            "production".to_owned(),
            crate::tabs::PasswordToStore::new("memory-only-password".to_owned()),
            festerm_ssh::SshSessionOptions::manual_recovery(
                festerm_ssh::SessionStrategy::PlainShell,
            ),
            true,
            &context,
        );
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
        harness.step();
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
        .expect("configuration is valid")
        // Workspace restore is off by default; this test exercises the
        // restore path, so it must opt in explicitly.
        .with_interface_settings(InterfaceSettings::new(
            festerm_config::ChipLayoutPreference::SingleRowScroll,
            true,
            true,
            true,
            true,
        ))
        .expect("configuration with restore_workspace enabled is valid");

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
    fn a_saved_workspace_is_ignored_at_startup_when_restore_workspace_is_off() {
        // Regression test: "Workspace restore" defaults to off, so a
        // workspace saved by an earlier run (or an older fesTerm build
        // that always persisted the tab list) must not silently resurface
        // just because the file still contains one.
        let workspace =
            festerm_config::WorkspaceConfiguration::new(
                vec![festerm_config::WorkspaceTab::settings("settings")
                    .expect("settings tab is valid")],
                Some("settings".to_owned()),
            )
            .expect("workspace is valid");
        let configuration = Configuration::new_with_workspace(Vec::new(), workspace)
            .expect("configuration is valid");
        assert!(!configuration.interface_settings().restore_workspace());

        let app = FesTermApp::with_configuration(&egui::Context::default(), configuration);

        assert_eq!(app.state.tabs().len(), 1);
        assert!(matches!(
            app.state.active_tab().content,
            TabContent::Launcher
        ));
        assert!(
            !app.state.configuration().workspace_enabled(),
            "the stale on-disk workspace must also be dropped from the in-memory \
             configuration, so it can't resurface via an unrelated settings save"
        );
    }

    #[test]
    fn turning_off_restore_workspace_clears_any_previously_saved_workspace_from_disk() {
        // Regression test: disabling the explicit "Workspace restore"
        // preference must scrub the on-disk tab list immediately, not just
        // stop updating it - otherwise re-enabling the toggle later would
        // resurrect a stale, forgotten snapshot instead of starting clean.
        let workspace =
            festerm_config::WorkspaceConfiguration::new(
                vec![festerm_config::WorkspaceTab::launcher("launcher")
                    .expect("launcher tab is valid")],
                None,
            )
            .expect("workspace is valid");
        let configuration = Configuration::new_with_workspace(Vec::new(), workspace)
            .expect("configuration is valid")
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                true,
                true,
                true,
            ))
            .expect("configuration with restore_workspace enabled is valid");
        let mut app = FesTermApp::for_test_with_configuration(configuration);
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-restore-workspace-toggle-off-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone());
        assert!(app.state.restore_workspace());

        let context = egui::Context::default();
        app.state
            .dispatch(AppCommand::ToggleRestoreWorkspace, &context);
        app.persist_interface_settings();
        assert!(!app.state.restore_workspace());
        app.clear_saved_workspace();

        assert!(!app.state.configuration().workspace_enabled());
        let saved = Configuration::load_from_path(&path).expect("saved configuration loads");
        assert!(!saved.workspace_enabled());
        assert!(saved.workspace().is_none());
        fs::remove_dir_all(directory).unwrap();
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
    fn workspace_autosaves_after_a_tab_mutating_frame_with_no_manual_save_action() {
        // Regression test: Settings used to require an explicit "Save
        // workspace" button; the workspace now saves itself automatically
        // whenever a frame changes the tab list (`AppState::workspace_dirty`
        // / `take_workspace_dirty`), so opening a new tab alone - with no
        // Settings action at all - must persist it to disk.
        let configuration = Configuration::empty()
            // Autosave is now gated on the "Workspace restore" preference;
            // this test exercises the autosave path, so it must opt in.
            .with_interface_settings(InterfaceSettings::new(
                festerm_config::ChipLayoutPreference::SingleRowScroll,
                true,
                true,
                true,
                true,
            ))
            .expect("configuration with restore_workspace enabled is valid");
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(configuration);
        let directory = std::env::current_dir().unwrap().join(format!(
            ".festerm-app-workspace-autosave-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone());

        app.state.dispatch(AppCommand::StartLocalSession, &context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 400.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        assert_eq!(
            harness.state().configuration_status,
            ConfigurationStartupStatus::WorkspaceSaved
        );
        assert!(
            path.exists(),
            "starting a session should autosave the workspace without a manual Save action"
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
            Some("\u{2318}+T")
        );
    }

    #[test]
    fn settings_hotkey_opens_settings_on_every_platform() {
        // Unlike the legacy `Cmd+,` binding (macOS-only), this cross-platform
        // shortcut works everywhere and is the one presented in Settings'
        // own Keyboard card.
        let mut harness = harness();
        harness.run();

        harness.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::S,
        );
        harness.run();

        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
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
        harness.step();
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
        harness.step();

        assert_eq!(harness.state().state.tabs().len(), before - 1);
    }

    #[test]
    fn command_palette_selection_activates_the_chosen_tab_end_to_end() {
        let mut harness = harness();
        harness.run();

        // Settings is opened directly here (rather than via the command
        // palette or its macOS-only Cmd+, shortcut) since `Open Settings`/
        // `Open Profiles` were removed from the palette as both are already
        // reachable from the three-dot menu or by closing the tab's X.
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenSettings, &context);
        harness.run();
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
        assert!(harness.state().palette.is_open());
        // The Launcher tab is first, so its accessible label includes the
        // platform-specific right-aligned quick-switch chord.
        let launcher_label = format!(
            "Launcher, {}",
            quick_switch_label(0).expect("the first quick-switch label exists")
        );
        harness
            .get_by_role_and_label(accesskit::Role::Button, &launcher_label)
            .click();
        harness.run();
        assert!(!harness.state().palette.is_open());

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
        harness.step();

        harness.get_by_label("Local Shell chip").click_secondary();
        harness.step();
        harness.get_by_label("Rename session").click();
        harness.step();
        let before_escape = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => panic!("rename must not change the active session"),
        };
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
        // Escape must remain application-owned: canceling the rename must
        // not leak a stray escape byte into the terminal. Focus reporting
        // (DECSET ?1004) defaults to off in our own terminal core, but this
        // test drives a *real* local shell session, and on Windows ConPTY's
        // documented startup sequence unconditionally sends `\x1b[?1004h`
        // to enable focus tracking for the hosted process (see
        // https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences
        // and ConPTY's own init-sequence behavior). Once that mode is on,
        // regaining terminal focus legitimately encodes a single `\x1b[I`
        // focus-in report (3 bytes) - that is correct VT protocol behavior,
        // not a leak, and only happens on real Windows sessions. A genuine
        // leaked Escape byte would show up as some *other*, smaller delta
        // (a lone 0x1b is 1 byte), so only accept exactly 0 or exactly the
        // legitimate focus-report length here.
        const FOCUS_IN_REPORT_BYTES: u64 = 3; // encoded length of "\x1b[I"
        let escape_delta = after_escape.saturating_sub(before_escape);
        assert!(
            escape_delta == 0 || escape_delta == FOCUS_IN_REPORT_BYTES,
            "Escape must remain application-owned: observed {escape_delta} unexpected \
             session-bound bytes (only a legitimate {FOCUS_IN_REPORT_BYTES}-byte DECSET \
             ?1004 focus-in report is expected when the real backend has focus reporting \
             enabled)"
        );

        harness.event(egui::Event::Text("Q".to_owned()));
        harness.step();
        let after_text = match &harness.state().state.active_tab().content {
            TabContent::Session(session) => session
                .view
                .diagnostics()
                .input_sink
                .map_or(0, |diagnostics| diagnostics.byte_count),
            _ => panic!("session must remain active"),
        };
        assert_eq!(
            after_text,
            before_escape + escape_delta + 1,
            "the keystroke right after cancelling rename must reach the terminal directly, confirming focus was restored"
        );
    }
}
