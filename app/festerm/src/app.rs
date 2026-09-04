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
    Configuration, EmojiPresentationPreference, InterfaceSettings, Profile, SerialDataBits,
    SerialFlowControl, SerialParity, SerialStopBits,
    SshPortForwardDirection as ConfigPortForwardDirection, TerminalFontPreference,
};
use festerm_pty::LocalProfile;
#[cfg(test)]
use festerm_secret_store::MemorySecretStore;
use festerm_secret_store::{native_store, SecretStore, SecretStoreError};
use festerm_session::{
    HostKeyPrompt, SshPortForwardDirection, SshPortForwardRuntime, SshPortForwardSource,
    SshPortForwardState,
};
use festerm_ui_egui::chrome::{self, ChipId, ChipStatus, ChipViewModel, ChromeAction};
use festerm_ui_egui::overlay::{self, OverlayAction};
use festerm_ui_egui::palette::{self, PaletteItem, PaletteState};
use festerm_ui_egui::theme;
use festerm_ui_egui::{TerminalFontFamily, TerminalFontGeneration, TerminalFontSet};

use crate::configuration_startup::{
    ConfigurationReloader, ConfigurationStartupStatus, StartupConfiguration,
};
use crate::inspector::{InspectorAction, InspectorContent, TransportFacts};
use crate::markdown_viewer::take_viewer_commands;
use crate::native_smoke::NativeWindowSmoke;
use crate::overlay_state::{
    CloseConsequence, LivePortForwardManager, OverlayState, PendingCloseConfirmation,
    PendingFileDropConfirmation, PendingPasswordStore, PendingPasteConfirmation,
    PendingQuitConfirmation, PendingSettingsResetConfirmation, StoredCredentialLaunch,
};
use crate::screens;
use crate::sftp_file_manager;
use crate::tabs::{
    AppCommand, AppState, ExternalLinkTarget, HostKeyTrustDecision, InspectorTransport, TabContent,
    TabId,
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
    PortForwardManager,
    MarkdownFind,
    MarkdownReload,
    MarkdownPreviewSource,
    MarkdownOutline,
    /// Terminal-content search (`docs/gui-design.md` "Terminal-content
    /// search"). `Ctrl+Shift+F` on Windows/Linux; macOS uses plain `Cmd+F`
    /// since `Cmd+Shift+F` is already `ToggleFocusMode` there.
    Find,
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
            Self::PortForwardManager => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                egui::Key::M,
            )),
            Self::MarkdownFind => Some((egui::Modifiers::COMMAND, egui::Key::F)),
            Self::MarkdownReload => Some((egui::Modifiers::COMMAND, egui::Key::R)),
            Self::MarkdownPreviewSource => Some((
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::V,
            )),
            Self::MarkdownOutline => Some((
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::O,
            )),
            Self::Find => Some((
                if cfg!(target_os = "macos") {
                    egui::Modifiers::COMMAND
                } else {
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT
                },
                egui::Key::F,
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
            Self::PortForwardManager if cfg!(target_os = "macos") => Some("\u{2318}+Shift+M"),
            Self::PortForwardManager => Some("Ctrl+Shift+M"),
            Self::MarkdownFind if cfg!(target_os = "macos") => Some("\u{2318}+F"),
            Self::MarkdownFind => Some("Ctrl+F"),
            Self::MarkdownReload if cfg!(target_os = "macos") => Some("\u{2318}+R"),
            Self::MarkdownReload => Some("Ctrl+R"),
            Self::MarkdownPreviewSource if cfg!(target_os = "macos") => Some("\u{2318}+Shift+V"),
            Self::MarkdownPreviewSource => Some("Ctrl+Shift+V"),
            Self::MarkdownOutline if cfg!(target_os = "macos") => Some("\u{2318}+Shift+O"),
            Self::MarkdownOutline => Some("Ctrl+Shift+O"),
            Self::Find if cfg!(target_os = "macos") => Some("\u{2318}+F"),
            Self::Find => Some("Ctrl+Shift+F"),
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
    /// Set once the aggregate quit confirmation has been deliberately
    /// confirmed, so the follow-up OS close request that actually tears
    /// down the window is let through instead of being intercepted again
    /// (`docs/gui-action-graph.md` `QUIT-03`).
    quit_confirmed: bool,
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
        // fesTerm only ships a single dark theme. Pin egui's active theme
        // explicitly rather than trusting `ThemePreference::System`: on
        // Windows the OS frequently reports a light system theme, which
        // would otherwise make `set_visuals` below write into the *inactive*
        // dark style slot while egui keeps rendering with the untouched
        // (light-on-light-background text) default light style.
        context.set_theme(egui::ThemePreference::Dark);
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
            quit_confirmed: false,
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
    fn sync_native_window_chrome(&self, context: &egui::Context, frame: &eframe::Frame) {
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

        // AppKit doesn't reliably hand first-responder status back to
        // winit's content view just because the window became key again -
        // see `reclaim_first_responder`'s doc comment for why a plain click
        // on the blank native title-bar strip can regain window focus
        // without ever restoring real keyboard delivery. Re-assert it on
        // every regained-focus event regardless of where the activating
        // click landed.
        let window_just_focused = context.input(|i| {
            i.events
                .iter()
                .any(|event| matches!(event, egui::Event::WindowFocused(true)))
        });
        if window_just_focused {
            festerm_macos_window::reclaim_first_responder(appkit_handle.ns_view);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn sync_native_window_chrome(&self, _context: &egui::Context, _frame: &eframe::Frame) {}

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
            TabContent::MarkdownViewer(_) => "Close Markdown Viewer",
            TabContent::SshAuthenticationRequired(_)
            | TabContent::SftpAuthenticationRequired(_)
            | TabContent::SftpFileManagerAuthenticationRequired(_)
            | TabContent::SftpFileManager(_)
            | TabContent::Session(_) => "Close Session",
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

    fn open_markdown_file_picker_with(
        &mut self,
        context: &egui::Context,
        pick_file: impl FnOnce() -> Option<std::path::PathBuf>,
    ) {
        let picked = pick_file();
        if let Some(path) = picked {
            self.state
                .dispatch(AppCommand::OpenLocalMarkdownFile { path }, context);
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
    fn evaluate_close_request(&mut self, context: &egui::Context) {
        if self.native_smoke.is_some() {
            // Native smoke owns the window lifecycle and writes its result
            // before requesting deterministic teardown. Interactive quit
            // confirmation must not cancel that automation-owned close.
            return;
        }
        if self.quit_confirmed || self.overlays.pending_quit.is_some() {
            // Already deliberately confirmed, or a second close-requested
            // event arrived while the confirmation is already showing -
            // either way, do not open (or reopen) another dialog.
            return;
        }
        let counts = self.state.live_session_counts();
        if counts.total() == 0 {
            // Nothing would be lost: let the close proceed untouched.
            return;
        }
        context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        self.overlays.pending_quit = Some(PendingQuitConfirmation {
            counts,
            cancel_focus_requested: false,
        });
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

    /// Renders the one aggregate confirmation for closing the window while
    /// live sessions remain (`docs/gui-design.md` "Closing sessions and
    /// quitting"). Revalidates the live counts every frame rather than
    /// trusting the snapshot captured when the dialog opened, so a session
    /// that exits on its own while the dialog is showing is reflected
    /// immediately, and the dialog closes itself if none remain.
    fn show_quit_confirmation(&mut self, context: &egui::Context, escape: bool) {
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
                ui.heading("Quit fesTerm?");
                ui.add_space(6.0);
                ui.label(pending.summary_message());
                ui.label("Unsaved terminal history will be discarded.");
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
                            egui::RichText::new("Quit fesTerm").color(theme::STATUS_ERROR),
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
            self.quit_confirmed = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
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

    /// Inspects this frame's OS file drops (`docs/gui-design.md`
    /// "Drag-and-drop input") and, for a local live session that still
    /// accepts input, stages a bounded insertion preview instead of ever
    /// silently guessing shell-specific quoting. Drops onto anything else
    /// (SSH/serial sessions, Launcher/Settings, an already-blocked overlay,
    /// or a session that no longer accepts input) are rejected with a
    /// factual transient notice rather than a misleading client-local path
    /// insertion.
    fn handle_dropped_files(&mut self, context: &egui::Context) {
        let dropped = context.input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        if self.overlays.blocks_terminal_input() {
            return;
        }
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            self.reject_file_drop(context, "File drop needs an active session.");
            return;
        };
        if !matches!(
            session.inspector_transport,
            InspectorTransport::Local { .. }
        ) {
            self.reject_file_drop(
                context,
                "File drop only inserts paths into a local session, never a remote or serial one.",
            );
            return;
        }
        if !session.accepts_input() {
            self.reject_file_drop(context, "File drop needs a running session.");
            return;
        }
        let paths: Vec<String> = dropped
            .iter()
            .map(|file| file.path().to_string_lossy().into_owned())
            .collect();
        if paths.is_empty() {
            return;
        }
        let identity = session.label.clone();
        let lifecycle_generation = session.controller.lifecycle_generation();
        let path_count = paths.len();
        self.overlays.pending_file_drop = Some(PendingFileDropConfirmation {
            tab: active,
            identity,
            text: paths.join(" "),
            path_count,
            lifecycle_generation,
            cancel_focus_requested: false,
        });
    }

    fn reject_file_drop(&mut self, context: &egui::Context, message: &str) {
        self.overlays.transient_notice = Some((
            message.to_owned(),
            Instant::now() + Duration::from_millis(2_500),
        ));
        context.request_repaint();
    }

    fn show_file_drop_confirmation(&mut self, context: &egui::Context, escape: bool) {
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

    fn cancel_file_drop_confirmation(&mut self) {
        let Some(pending) = self.overlays.pending_file_drop.take() else {
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

    /// Shared automatic-save orchestration for the "workspace/configuration"
    /// candidate seam (#53): every write-through path here computes a
    /// validated replacement `Configuration`, asks the reloader to persist
    /// it, and only commits that replacement into `self.state` if the write
    /// actually succeeded (`ConfigurationStartupStatus::was_saved`) -
    /// otherwise the in-memory configuration is left untouched and the
    /// caller only learns about the failure via `self.configuration_status`.
    ///
    /// Centralizing this removes seven independently hand-written copies of
    /// the same "commit only on success" check (previously each a
    /// `matches!(status, ...)` written against a different specific success
    /// variant) into one place, so a future ninth call site can't
    /// accidentally omit the guard and commit an unsaved change. This is a
    /// data/control-flow extraction only, scoped to the three fields it
    /// already reads (`self.state`, `self.configuration_reloader`,
    /// `self.configuration_status`); it does not become a new "manager"
    /// type, matching the constraint in #53's issue body.
    fn apply_configuration_save(
        &mut self,
        replacement: Result<festerm_config::Configuration, festerm_config::ConfigError>,
        on_invalid: impl FnOnce(
            crate::configuration_startup::ConfigurationLoadFailure,
        ) -> ConfigurationStartupStatus,
        save: impl FnOnce(
            &crate::configuration_startup::ConfigurationReloader,
            &festerm_config::Configuration,
        ) -> ConfigurationStartupStatus,
    ) -> bool {
        let replacement = match replacement {
            Ok(replacement) => replacement,
            Err(_) => {
                self.configuration_status =
                    on_invalid(crate::configuration_startup::ConfigurationLoadFailure::Invalid);
                return false;
            }
        };
        let status = save(&self.configuration_reloader, &replacement);
        let was_saved = status.was_saved();
        if was_saved {
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
        was_saved
    }

    /// Captures a metadata-only workspace snapshot and saves it immediately
    /// (`docs/gui-design.md` "Configuration": open/closed tabs, their order,
    /// and the active tab autosave on every change - there is no manual
    /// Save action). The current configuration changes only after the
    /// atomic file replacement has succeeded.
    fn save_workspace(&mut self) {
        self.apply_configuration_save(
            self.state.capture_workspace_configuration(),
            ConfigurationStartupStatus::WorkspaceSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_workspace,
        );
    }

    /// Scrubs any previously saved workspace snapshot from disk after the
    /// "Workspace restore" preference is turned off (`docs/gui-design.md`
    /// "Workspace restore"). Infallible on the in-memory side - clearing a
    /// workspace can never fail validation - so this only reports a status
    /// if the write-through itself fails.
    fn clear_saved_workspace(&mut self) {
        self.apply_configuration_save(
            Ok(self.state.configuration().without_workspace()),
            ConfigurationStartupStatus::WorkspaceSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_workspace,
        );
    }

    /// Writes through the current chip-layout/status-bar preferences
    /// immediately after a toggle or reset. The in-memory `AppState` change
    /// applies regardless of whether the write succeeds
    /// (`docs/gui-design.md` "apply immediately"); a failed write only means
    /// the change will not survive a restart.
    fn persist_interface_settings(&mut self) {
        self.apply_configuration_save(
            self.state
                .configuration()
                .with_interface_settings(self.state.interface_settings()),
            ConfigurationStartupStatus::InterfaceSettingsSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_interface_settings,
        );
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
        self.apply_configuration_save(
            self.state
                .configuration()
                .with_known_host_trust(&host, port, &fingerprint),
            ConfigurationStartupStatus::KnownHostTrustSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_known_host_trust,
        );
    }

    /// Creates or edits a profile from the Profiles surface (both are the
    /// same upsert-by-identifier write, `docs/gui-design.md` "Profile
    /// editing"). The in-memory configuration only changes after the atomic
    /// file write succeeds, matching every other automatic-save path here.
    fn save_profile(&mut self, profile: festerm_config::Profile) -> bool {
        self.apply_configuration_save(
            self.state.configuration().with_profile(profile),
            ConfigurationStartupStatus::ProfileSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_profile,
        )
    }

    /// Deletes a profile the user has already confirmed in the Profiles
    /// surface. `Configuration::without_profile` itself rejects deletion of
    /// a profile still referenced by a saved workspace tab, surfacing that
    /// as an ordinary save failure rather than silently orphaning the tab.
    fn delete_profile(&mut self, identifier: &str) {
        self.apply_configuration_save(
            self.state.configuration().without_profile(identifier),
            ConfigurationStartupStatus::ProfileDeleteFailure,
            crate::configuration_startup::ConfigurationReloader::delete_profile,
        );
    }

    /// Reorders a saved profile after a drag-to-reorder gesture on the
    /// Profiles surface (`Configuration::with_reordered_profiles`); the
    /// Launcher's own profile ordering reflects this immediately since both
    /// surfaces read the same persisted `Configuration::profiles` order.
    fn reorder_profiles(&mut self, moved: &str, before: Option<&str>) {
        self.apply_configuration_save(
            self.state
                .configuration()
                .with_reordered_profiles(moved, before),
            ConfigurationStartupStatus::ProfilesReorderFailure,
            crate::configuration_startup::ConfigurationReloader::reorder_profiles,
        );
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
        self.apply_terminal_font_policy();
        context.request_repaint();
    }

    fn terminal_font_set(&self) -> TerminalFontSet {
        TerminalFontSet::new(
            terminal_font_family(self.state.terminal_font()),
            self.state.terminal_ligatures(),
            self.terminal_font_generation,
        )
        .with_color_emoji(
            self.state.emoji_presentation() == EmojiPresentationPreference::Color
                || self
                    .native_smoke
                    .as_ref()
                    .is_some_and(NativeWindowSmoke::requires_color_emoji),
        )
    }

    fn save_profile_with_credential(
        &mut self,
        profile: festerm_config::Profile,
        credential: crate::tabs::ProfileCredentialToStore,
        context: &egui::Context,
    ) {
        let profile_id = profile.identifier().to_owned();
        if !self.save_profile(profile) {
            return;
        }
        match credential {
            crate::tabs::ProfileCredentialToStore::Password(password) => self
                .store_password_for_profile(
                    profile_id,
                    password,
                    festerm_ssh::SshSessionOptions::new(),
                    None,
                    context,
                ),
            crate::tabs::ProfileCredentialToStore::PrivateKey(private_key) => self
                .store_private_key_for_profile(
                    profile_id,
                    private_key,
                    festerm_ssh::SshSessionOptions::new(),
                    None,
                    context,
                ),
        }
    }

    fn apply_terminal_font_policy(&mut self) {
        let font_set = self.terminal_font_set();
        self.state.apply_terminal_font_set(font_set);
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

    fn request_external_link(&mut self, target: &str, context: &egui::Context) {
        let Some(target) = festerm_core::normalize_external_web_url(target) else {
            self.overlays.transient_notice = Some((
                "This link is not a valid web address.".to_owned(),
                Instant::now() + Duration::from_secs(3),
            ));
            context.request_repaint();
            return;
        };
        self.state.dispatch(
            AppCommand::OpenExternalLink {
                target: ExternalLinkTarget::new(target),
            },
            context,
        );
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
        let options = festerm_ssh::SshSessionOptions::manual_recovery(strategy);
        let options = self
            .state
            .configuration()
            .profile(&profile_id)
            .and_then(Profile::as_ssh)
            .and_then(|profile| {
                options
                    .with_profile_port_forwards(profile.port_forwards().iter())
                    .ok()
            });
        let Some(options) = options else {
            self.secure_storage_feedback =
                Some("This saved SSH profile has invalid port-forward settings.");
            return;
        };
        self.start_stored_password_profile_with_options(profile_id, options, context);
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

    fn start_configured_sftp_profile(&mut self, profile_id: String, context: &egui::Context) {
        let Some(profile) = self
            .state
            .configuration()
            .profile(&profile_id)
            .and_then(Profile::as_ssh)
        else {
            return;
        };
        if profile.sftp_gui_mode() {
            self.state.dispatch(
                AppCommand::OpenConfiguredSftpFileManagerProfile { profile_id },
                context,
            );
            return;
        }
        let has_credential = profile.credential_reference().is_some();
        if has_credential {
            self.start_stored_sftp_profile(profile_id, context);
        } else {
            self.state
                .start_configured_sftp_profile_interactive(&profile_id, context);
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

    fn start_stored_sftp_profile(&mut self, profile_id: String, context: &egui::Context) {
        let Ok(store) = self.secret_store.as_ref() else {
            self.secure_storage_feedback = self
                .secret_store
                .as_ref()
                .err()
                .copied()
                .map(secret_store_message);
            return;
        };
        if !self
            .state
            .start_stored_password_sftp_profile(&profile_id, Arc::clone(store), context)
        {
            self.secure_storage_feedback =
                Some("This saved SFTP destination has no stored password. Enter and remember a password first.");
        }
    }

    fn start_stored_sftp_file_manager_profile(
        &mut self,
        profile_id: String,
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
        if !self.state.start_stored_sftp_file_manager_profile(
            &profile_id,
            Arc::clone(store),
            context,
        ) {
            self.secure_storage_feedback =
                Some("This saved SFTP destination has no stored credential.");
        }
    }

    fn store_password_for_profile(
        &mut self,
        profile_id: String,
        password: crate::tabs::PasswordToStore,
        _options: festerm_ssh::SshSessionOptions,
        launch_after_store: Option<StoredCredentialLaunch>,
        context: &egui::Context,
    ) {
        self.store_credential_for_profile(
            profile_id,
            festerm_config::CredentialKind::Password,
            move || password.into_secret_bytes(),
            launch_after_store,
            context,
        );
    }

    fn store_private_key_for_profile(
        &mut self,
        profile_id: String,
        private_key: crate::tabs::PrivateKeyToStore,
        _options: festerm_ssh::SshSessionOptions,
        launch_after_store: Option<StoredCredentialLaunch>,
        context: &egui::Context,
    ) {
        self.store_credential_for_profile(
            profile_id,
            festerm_config::CredentialKind::PrivateKey,
            move || private_key.into_secret_bytes(),
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
        launch_after_store: Option<StoredCredentialLaunch>,
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
                    if let Some(launch) = pending.launch_after_store {
                        match launch {
                            StoredCredentialLaunch::Ssh(options) => {
                                self.start_stored_password_profile_with_options(
                                    pending.profile_id,
                                    options,
                                    context,
                                );
                            }
                            StoredCredentialLaunch::Sftp => {
                                self.start_stored_sftp_profile(pending.profile_id, context);
                            }
                        }
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
        let active = self.state.active();
        for (id, session) in self.state.session_tabs_with_id_mut() {
            let hit_limit = session.controller.pump_events(&mut session.terminal);
            // `hit_limit` alone only reports whether the bounded per-frame
            // drain was exhausted (backpressure) - a normal, modest burst of
            // output drains well under the per-frame cap and reports
            // `false` there even though real output was ingested. Use
            // `last_pump_output_received()` for "did output actually
            // arrive", both for scheduling a repaint and for the
            // background-tab "new output" chip pulse (feature request #68);
            // relying on `hit_limit` for the latter meant it almost never
            // fired for ordinary output.
            let output_received = session.controller.last_pump_output_received();
            if hit_limit || output_received {
                needs_repaint = true;
                if output_received && id != active {
                    // Only mark a *background* tab as having new output; the
                    // active tab is already visible, so there is nothing to
                    // notify the user of.
                    session.has_new_output_since_active = true;
                }
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
        const FIND_IN_TERMINAL: u64 = 15;
        const PORT_FORWARD_MANAGER: u64 = 16;
        const OPEN_MARKDOWN_FILE: u64 = 17;
        const RELOAD_MARKDOWN: u64 = 18;
        const TOGGLE_MARKDOWN_MODE: u64 = 19;
        const FIND_IN_MARKDOWN: u64 = 20;
        const TOGGLE_MARKDOWN_OUTLINE: u64 = 21;
        const OPEN_SFTP_FILE_MANAGER: u64 = 22;
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
            PaletteItem {
                id: OPEN_MARKDOWN_FILE,
                label: "Open Markdown File…".to_owned(),
                hint: None,
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
                    id: FIND_IN_TERMINAL,
                    label: "Find in Terminal…".to_owned(),
                    hint: ApplicationShortcut::Find.label().map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: PORT_FORWARD_MANAGER,
                    label: if self.port_forward_manager_open_for_active_tab() {
                        "Hide Port Forward Manager".to_owned()
                    } else {
                        "Manage Port Forwards…".to_owned()
                    },
                    hint: ApplicationShortcut::PortForwardManager
                        .label()
                        .map(str::to_owned),
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
            if let TabContent::Session(session) = &self.state.active_tab().content {
                if !session.live_port_forwarding_available() {
                    items.retain(|item| item.id != PORT_FORWARD_MANAGER);
                }
                if matches!(session.inspector_transport, InspectorTransport::Ssh { .. }) {
                    items.push(PaletteItem {
                        id: OPEN_SFTP_FILE_MANAGER,
                        label: "Open SFTP".to_owned(),
                        hint: Some(
                            "Open the GUI SFTP file manager for this SSH session".to_owned(),
                        ),
                        is_tab: false,
                        shortcut_label: None,
                    });
                }
            }
        }
        if matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        ) {
            items.extend([
                PaletteItem {
                    id: RELOAD_MARKDOWN,
                    label: "Reload Markdown".to_owned(),
                    hint: ApplicationShortcut::MarkdownReload
                        .label()
                        .map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: TOGGLE_MARKDOWN_MODE,
                    label: "Toggle Preview/Source".to_owned(),
                    hint: ApplicationShortcut::MarkdownPreviewSource
                        .label()
                        .map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: FIND_IN_MARKDOWN,
                    label: "Find in Markdown…".to_owned(),
                    hint: ApplicationShortcut::MarkdownFind.label().map(str::to_owned),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: TOGGLE_MARKDOWN_OUTLINE,
                    label: "Toggle Markdown Outline".to_owned(),
                    hint: ApplicationShortcut::MarkdownOutline
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
                TabContent::MarkdownViewer(_) => "Close Markdown Viewer".to_owned(),
                TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_)
                | TabContent::Session(_) => "Close Session…".to_owned(),
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
                TabContent::MarkdownViewer(tab) => (
                    tab.title().to_owned(),
                    Some(tab.chip_secondary().to_owned()),
                ),
                TabContent::SshAuthenticationRequired(tab) => (
                    tab.profile.identifier().to_owned(),
                    Some(format!(
                        "SSH authentication required · {}:{}",
                        tab.profile.host(),
                        tab.profile.port()
                    )),
                ),
                TabContent::SftpAuthenticationRequired(tab) => (
                    tab.profile.identifier().to_owned(),
                    Some(format!(
                        "SFTP authentication required · {}:{}",
                        tab.profile.host(),
                        tab.profile.port()
                    )),
                ),
                TabContent::SftpFileManagerAuthenticationRequired(tab) => (
                    tab.target.label.clone(),
                    Some(format!(
                        "GUI SFTP authentication required · {}:{}",
                        tab.target.host, tab.target.port
                    )),
                ),
                TabContent::SftpFileManager(tab) => {
                    (tab.label.clone(), Some("GUI SFTP file manager".to_owned()))
                }
                TabContent::Session(session) => {
                    let dynamic_title = session.terminal.title();
                    let hint = session
                        .dynamic_secondary()
                        .or_else(|| (!dynamic_title.is_empty()).then(|| dynamic_title.to_owned()))
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
        self.dispatch_palette_selection_with_picker(id, context, || {
            rfd::FileDialog::new()
                .add_filter("Markdown", &["md", "markdown"])
                .pick_file()
        });
    }

    fn dispatch_palette_selection_with_picker(
        &mut self,
        id: u64,
        context: &egui::Context,
        pick_markdown_file: impl FnOnce() -> Option<std::path::PathBuf>,
    ) {
        const TAB_ACTIVATE_OFFSET: u64 = 1 << 32;
        match id {
            1 => self.state.dispatch(AppCommand::OpenLauncher, context),
            3 => self.state.dispatch(AppCommand::StartLocalSession, context),
            17 => self.open_markdown_file_picker_with(context, pick_markdown_file),
            18 => self.state.dispatch(AppCommand::ReloadMarkdown, context),
            19 => self
                .state
                .dispatch(AppCommand::ToggleMarkdownPreviewSource, context),
            20 => self.state.dispatch(AppCommand::OpenMarkdownFind, context),
            21 => self
                .state
                .dispatch(AppCommand::ToggleMarkdownOutline, context),
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
            15 => self.open_terminal_search(context),
            16 => self.toggle_port_forward_manager(context),
            22 => {
                let active = self.state.active();
                if let Some(target) = self.state.sftp_file_manager_target_for_tab(active) {
                    self.state
                        .dispatch(AppCommand::OpenSftpFileManager { target }, context);
                }
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
        let open_port_forward_manager = matches!(&self.state.active_tab().content, TabContent::Session(session) if session.live_port_forwarding_available())
            && ApplicationShortcut::PortForwardManager.consume(ctx);
        let open_find = matches!(self.state.active_tab().content, TabContent::Session(_))
            && ApplicationShortcut::Find.consume(ctx);
        let markdown_find = matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        ) && ApplicationShortcut::MarkdownFind.consume(ctx);
        let markdown_reload = matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        ) && ApplicationShortcut::MarkdownReload.consume(ctx);
        let markdown_toggle_mode = matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        ) && ApplicationShortcut::MarkdownPreviewSource.consume(ctx);
        let markdown_toggle_outline = matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        ) && ApplicationShortcut::MarkdownOutline.consume(ctx);

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
        if open_port_forward_manager {
            self.toggle_port_forward_manager(ctx);
        }
        if open_find {
            self.open_terminal_search(ctx);
        }
        if markdown_find {
            self.state.dispatch(AppCommand::OpenMarkdownFind, ctx);
        }
        if markdown_reload {
            self.state.dispatch(AppCommand::ReloadMarkdown, ctx);
        }
        if markdown_toggle_mode {
            self.state
                .dispatch(AppCommand::ToggleMarkdownPreviewSource, ctx);
        }
        if markdown_toggle_outline {
            self.state.dispatch(AppCommand::ToggleMarkdownOutline, ctx);
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

    /// Opens (or refocuses) the terminal-content find bar for the active
    /// session (`docs/gui-design.md` "Terminal-content search"). The palette
    /// entry shares this same entry point.
    fn open_terminal_search(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        session.search.open();
        session.search.rescan(&session.terminal);
        if let Some(row) = session.search.current_match_row() {
            session.view.reveal_document_row(&session.terminal, row);
        }
        context.request_repaint();
    }

    /// Escape/close-button path: clears the transient query/highlights and
    /// restores terminal focus, matching Copy/Paste's ordinary focus
    /// restoration behavior after an overlay-ish surface dismisses.
    fn close_terminal_search(&mut self, context: &egui::Context) {
        let active = self.state.active();
        if let Some(session) = self.state.session_tab_mut(active) {
            session.search.close();
        }
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    fn sync_port_forward_manager(&mut self) {
        let Some(manager) = self.overlays.port_forward_manager.as_ref() else {
            return;
        };
        let active = self.state.active();
        let keep_open = manager.tab == active
            && matches!(
                self.state.active_tab().content,
                TabContent::Session(ref session) if session.is_ssh_session()
            );
        if !keep_open {
            self.overlays.port_forward_manager = None;
        }
    }

    fn port_forward_manager_open_for_active_tab(&self) -> bool {
        self.overlays
            .port_forward_manager
            .as_ref()
            .is_some_and(|manager| manager.tab == self.state.active())
    }

    fn close_port_forward_manager(&mut self, context: &egui::Context) {
        if self.overlays.port_forward_manager.take().is_some() {
            self.restore_active_terminal_focus();
            context.request_repaint();
        }
    }

    fn open_port_forward_manager(&mut self, context: &egui::Context) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        let available = session.live_port_forwarding_available();
        let search_open = session.search.is_open();
        if !available {
            return;
        }
        if search_open {
            self.close_terminal_search(context);
        }
        let query_result = self
            .state
            .session_tab_mut(active)
            .expect("active session tab still exists")
            .query_port_forwards();
        let mut manager = LivePortForwardManager::new(active);
        if let Err(error) = query_result {
            manager.error = Some(error.to_string());
        }
        self.overlays.port_forward_manager = Some(manager);
        context.request_repaint();
    }

    fn toggle_port_forward_manager(&mut self, context: &egui::Context) {
        if self.port_forward_manager_open_for_active_tab() {
            self.close_port_forward_manager(context);
        } else {
            self.open_port_forward_manager(context);
        }
    }

    fn port_forward_direction_label(direction: SshPortForwardDirection) -> &'static str {
        match direction {
            SshPortForwardDirection::Local => "Local",
            SshPortForwardDirection::Remote => "Remote",
        }
    }

    fn port_forward_source_label(source: SshPortForwardSource) -> &'static str {
        match source {
            SshPortForwardSource::Profile => "Profile",
            SshPortForwardSource::Ephemeral => "Ephemeral",
        }
    }

    fn port_forward_state_label(state: SshPortForwardState) -> &'static str {
        match state {
            SshPortForwardState::Active => "Active",
            SshPortForwardState::Failed => "Failed",
        }
    }

    fn port_forward_count_label(count: usize) -> Option<String> {
        (count > 0).then(|| {
            if count == 1 {
                "1 active forward".to_owned()
            } else {
                format!("{count} active forwards")
            }
        })
    }

    fn show_port_forward_manager(&mut self, ctx: &egui::Context, content_rect: egui::Rect) {
        let Some(manager) = self.overlays.port_forward_manager.as_mut() else {
            return;
        };
        let Some(session) = self.state.session_tab_mut(manager.tab) else {
            self.overlays.port_forward_manager = None;
            return;
        };
        let available = session.live_port_forwarding_available();
        let forwards = session.port_forwards().to_vec();
        let mut close_requested = false;
        let mut add_requested = false;
        let mut remove_requested: Option<SshPortForwardRuntime> = None;
        let width = (content_rect.width() - 32.0).clamp(360.0, 520.0);
        let height = (content_rect.height() - 24.0).clamp(280.0, 520.0);
        egui::Modal::new(egui::Id::new(("port_forward_manager", manager.tab)))
            .backdrop_color(egui::Color32::from_black_alpha(128))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(width);
                        ui.set_max_width(width);
                        ui.set_max_height(height);
                        ui.horizontal(|ui| {
                            ui.heading("Port Forward Manager");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Close").clicked() {
                                        close_requested = true;
                                    }
                                },
                            );
                        });
                        ui.label(
                            egui::RichText::new(
                                "Live-only SSH forwards for this tab. Overlay-added mappings never change the saved profile.",
                            )
                            .small()
                            .color(theme::TEXT_SECONDARY),
                        );
                        if let Some(error) = &manager.error {
                            ui.add_space(6.0);
                            ui.colored_label(theme::STATUS_ERROR, error);
                        }
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("CURRENT FORWARDS")
                                .size(10.0)
                                .color(theme::TEXT_MUTED),
                        );
                        ui.add_space(4.0);
                        if !available {
                            ui.label(
                                egui::RichText::new(
                                    "Port forwarding is available only while this SSH session is connected.",
                                )
                                .color(theme::TEXT_SECONDARY),
                            );
                        } else if forwards.is_empty() {
                            ui.label(
                                egui::RichText::new("No live port forwards yet.")
                                    .color(theme::TEXT_SECONDARY),
                            );
                        } else {
                            egui::ScrollArea::vertical()
                                .id_salt(("port_forward_manager_list", manager.tab))
                                .max_height((height * 0.45).max(140.0))
                                .show(ui, |ui| {
                                    for (index, forward) in forwards.iter().enumerate() {
                                        if index > 0 {
                                            ui.add_space(8.0);
                                        }
                                        egui::Frame::new()
                                            .fill(theme::SURFACE_TAB_ACTIVE)
                                            .stroke(egui::Stroke::new(
                                                1.0,
                                                theme::BORDER_SUBTLE,
                                            ))
                                            .corner_radius(6.0)
                                            .inner_margin(egui::Margin::same(12))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            Self::port_forward_direction_label(
                                                                forward.direction(),
                                                            ),
                                                        )
                                                        .strong(),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{}:{}",
                                                            forward.bind_host(),
                                                            forward.bind_port()
                                                        ))
                                                        .color(theme::TEXT_SECONDARY),
                                                    );
                                                    ui.label("→");
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "{}:{}",
                                                            forward.destination_host(),
                                                            forward.destination_port()
                                                        ))
                                                        .color(theme::TEXT_SECONDARY),
                                                    );
                                                    ui.with_layout(
                                                        egui::Layout::right_to_left(
                                                            egui::Align::Center,
                                                        ),
                                                        |ui| {
                                                            if ui
                                                                .button(format!(
                                                                    "Remove forward {}:{}",
                                                                    forward.bind_host(),
                                                                    forward.bind_port()
                                                                ))
                                                                .clicked()
                                                            {
                                                                remove_requested =
                                                                    Some(forward.clone());
                                                            }
                                                        },
                                                    );
                                                });
                                                ui.add_space(4.0);
                                                ui.horizontal_wrapped(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            Self::port_forward_source_label(
                                                                forward.source(),
                                                            ),
                                                        )
                                                        .size(11.0)
                                                        .color(theme::TEXT_MUTED),
                                                    );
                                                    let state_color = match forward.state() {
                                                        SshPortForwardState::Active => {
                                                            theme::STATUS_RUNNING
                                                        }
                                                        SshPortForwardState::Failed => {
                                                            theme::STATUS_ERROR
                                                        }
                                                    };
                                                    ui.colored_label(
                                                        state_color,
                                                        Self::port_forward_state_label(
                                                            forward.state(),
                                                        ),
                                                    );
                                                    if let Some(reason) = forward.failure_reason() {
                                                        ui.label(
                                                            egui::RichText::new(reason)
                                                                .size(11.0)
                                                                .color(theme::TEXT_SECONDARY),
                                                        );
                                                    }
                                                });
                                            });
                                    }
                                });
                        }
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new("ADD LIVE FORWARD")
                                .size(10.0)
                                .color(theme::TEXT_MUTED),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "New rows default to loopback (127.0.0.1). Edit the bind host explicitly to widen exposure.",
                            )
                            .size(11.0)
                            .color(theme::TEXT_SECONDARY),
                        );
                        ui.add_space(8.0);
                        ui.add_enabled_ui(available, |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Direction");
                                ui.radio_value(
                                    &mut manager.draft.direction,
                                    ConfigPortForwardDirection::Local,
                                    "Local",
                                );
                                ui.radio_value(
                                    &mut manager.draft.direction,
                                    ConfigPortForwardDirection::Remote,
                                    "Remote",
                                );
                            });
                            ui.horizontal(|ui| {
                                let label = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Bind host")
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .selectable(false),
                                );
                                let field = ui.add(
                                    egui::TextEdit::singleline(&mut manager.draft.bind_host)
                                        .id_salt(("port_forward_bind_host", manager.tab))
                                        .desired_width(180.0),
                                );
                                let field = field.labelled_by(label.id);
                                if manager.request_focus {
                                    field.request_focus();
                                    manager.request_focus = false;
                                }
                            });
                            ui.horizontal(|ui| {
                                let label = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Bind port")
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .selectable(false),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut manager.draft.bind_port)
                                        .id_salt(("port_forward_bind_port", manager.tab))
                                        .desired_width(120.0),
                                )
                                .labelled_by(label.id);
                            });
                            ui.horizontal(|ui| {
                                let label = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Destination host")
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .selectable(false),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut manager.draft.destination_host,
                                    )
                                    .id_salt(("port_forward_destination_host", manager.tab))
                                    .desired_width(180.0),
                                )
                                .labelled_by(label.id);
                            });
                            ui.horizontal(|ui| {
                                let label = ui.add(
                                    egui::Label::new(
                                        egui::RichText::new("Destination port")
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .selectable(false),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(
                                        &mut manager.draft.destination_port,
                                    )
                                    .id_salt(("port_forward_destination_port", manager.tab))
                                    .desired_width(120.0),
                                )
                                .labelled_by(label.id);
                            });
                            ui.add_space(6.0);
                            if ui.button("Add live forward").clicked() {
                                add_requested = true;
                            }
                        });
                    });
            });

        if close_requested {
            self.close_port_forward_manager(ctx);
            return;
        }
        if let Some(forward) = remove_requested {
            manager.error = session
                .remove_port_forward(
                    forward.direction(),
                    forward.bind_host(),
                    forward.bind_port(),
                )
                .err()
                .map(|error| error.to_string());
            ctx.request_repaint();
        }
        if add_requested {
            match manager.draft.build() {
                Ok(forward) => match session.add_port_forward(&forward) {
                    Ok(()) => {
                        manager.error = None;
                        manager.draft.reset();
                    }
                    Err(error) => manager.error = Some(error.to_string()),
                },
                Err(error) => manager.error = Some(error),
            }
            ctx.request_repaint();
        }
    }

    /// Renders the find bar as a foreground overlay above the terminal
    /// viewport (`content_rect`) without altering `CentralPanel` layout or
    /// grid dimensions.
    fn show_terminal_find_bar(&mut self, ctx: &egui::Context, content_rect: egui::Rect) {
        let active = self.state.active();
        let Some(session) = self.state.session_tab_mut(active) else {
            return;
        };
        if !session.search.is_open() {
            return;
        }
        session.search.refresh_if_stale(&session.terminal);
        let area_id = egui::Id::new(("terminal_find_bar", active));
        let query_id = egui::Id::new(("terminal_find_query", active));
        let mut close_requested = false;
        egui::Area::new(area_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                content_rect.right() - 320.0,
                content_rect.top() + 8.0,
            ))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.horizontal(|ui| {
                        let label = ui.add(egui::Label::new("Find:").selectable(false));
                        let mut query = session.search.query().to_owned();
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut query)
                                .id(query_id)
                                .hint_text("Search terminal…")
                                .desired_width(140.0),
                        );
                        let response = response.labelled_by(label.id);
                        if session.search.take_focus_request() {
                            response.request_focus();
                        }
                        let mut jump_to: Option<usize> = None;
                        if response.changed() {
                            session.search.set_query(&session.terminal, query);
                            jump_to = session.search.current_match_row();
                        }
                        let enter_pressed = response.has_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        if enter_pressed {
                            if ui.input(|input| input.modifiers.shift) {
                                session.search.retreat();
                            } else {
                                session.search.advance();
                            }
                            jump_to = session.search.current_match_row();
                        }
                        if let Some(row) = jump_to {
                            session.view.reveal_document_row(&session.terminal, row);
                        }
                        if response.has_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Escape))
                        {
                            close_requested = true;
                        }
                        ui.label(if session.search.has_query() {
                            match session.search.current_position() {
                                Some(position) => {
                                    format!("{position} of {}", session.search.match_count())
                                }
                                None => "No matches".to_owned(),
                            }
                        } else {
                            String::new()
                        });
                        if ui.add(egui::Button::new("\u{2191}")).clicked() {
                            session.search.retreat();
                            jump_to = session.search.current_match_row();
                        }
                        if ui.add(egui::Button::new("\u{2193}")).clicked() {
                            session.search.advance();
                            jump_to = session.search.current_match_row();
                        }
                        if let Some(row) = jump_to {
                            session.view.reveal_document_row(&session.terminal, row);
                        }
                        if ui.button("\u{2715}").clicked() {
                            close_requested = true;
                        }
                    });
                });
            });
        if close_requested {
            self.close_terminal_search(ctx);
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
            .enumerate()
            .map(|(index, tab)| {
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
                    TabContent::MarkdownViewer(tab) => (
                        tab.title().to_owned(),
                        Some(tab.chip_secondary().to_owned()),
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
                    TabContent::SftpAuthenticationRequired(tab) => (
                        tab.profile.identifier().to_owned(),
                        Some(format!(
                            "SFTP authentication required · {}:{}",
                            tab.profile.host(),
                            tab.profile.port()
                        )),
                        ChipStatus::Neutral,
                    ),
                    TabContent::SftpFileManagerAuthenticationRequired(tab) => (
                        tab.target.label.clone(),
                        Some(format!(
                            "GUI SFTP authentication required · {}:{}",
                            tab.target.host, tab.target.port
                        )),
                        ChipStatus::Neutral,
                    ),
                    TabContent::SftpFileManager(tab) => (
                        tab.label.clone(),
                        Some("GUI SFTP file manager".to_owned()),
                        ChipStatus::Neutral,
                    ),
                    TabContent::Session(session) => {
                        let dynamic_title = session.terminal.title();
                        let secondary = session
                            .dynamic_secondary()
                            .or_else(|| {
                                (!dynamic_title.is_empty())
                                    .then(|| Self::display_secondary(dynamic_title))
                            })
                            .or_else(|| session.launch_secondary.clone());
                        (session.label.clone(), secondary, session.chip_status())
                    }
                };
                let renamable = matches!(tab.content, TabContent::Session(_));
                // Feature request #68: only a background session tab with
                // unseen output pulses, and only when the preference is on.
                // The active tab's own chip never pulses - there is nothing
                // to notify the user of while they're already looking at it.
                let pulse_new_output = self.state.pulse_new_output_dot()
                    && tab.id != self.state.active()
                    && matches!(
                        &tab.content,
                        TabContent::Session(session) if session.has_new_output_since_active
                    );
                ChipViewModel {
                    id: ChipId(tab.id.chip_id()),
                    primary,
                    secondary,
                    status,
                    closable: true,
                    renamable,
                    quick_switch_number: (index < MAX_QUICK_SWITCH_TABS).then(|| (index + 1) as u8),
                    pulse_new_output,
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
        // Append the terminal view's own per-frame render diagnostics
        // (frame time, input->paint submission latency, dirty rows) after
        // the session/PTY line, so a slow-rendering report can be
        // diagnosed from the Inspector alone rather than needing extra
        // instrumentation.
        let diagnostics = session
            .view
            .diagnostics_summary(&session.controller.diagnostics_line());
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
            InspectorTransport::Sftp {
                username,
                host,
                port,
            } => TransportFacts::Sftp {
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
            | InspectorTransport::Sftp { .. }
            | InspectorTransport::Serial { .. } => None,
        };
        let type_label = match session.inspector_transport {
            InspectorTransport::Local { .. } => "Local shell",
            InspectorTransport::Ssh { .. } => "SSH",
            InspectorTransport::Sftp { .. } => "SFTP",
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
                InspectorTransport::Sftp { .. } => {
                    "The SFTP session could not start. Review Diagnostics for the failure detail."
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
                open_sftp_available: matches!(
                    session.inspector_transport,
                    InspectorTransport::Ssh { .. }
                ),
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
        let (dimensions, system, status, status_label, detail, port_forwards) =
            match &self.state.active_tab().content {
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::MarkdownViewer(_)
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_) => {
                    (None, None, ChipStatus::Neutral, "", None, None)
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
                            session
                                .dynamic_secondary()
                                .or_else(|| {
                                    (!dynamic_title.is_empty())
                                        .then(|| Self::display_secondary(dynamic_title))
                                })
                                .or_else(|| session.launch_secondary.clone())
                        })
                        .flatten();
                    (
                        session.view.dimensions_label(),
                        Some(session.system_label()),
                        status,
                        session.status_bar_label(),
                        detail,
                        Self::port_forward_count_label(session.active_port_forward_count()),
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
                        port_forwards: port_forwards.as_deref(),
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
        if context.input(|i| i.viewport().close_requested()) {
            self.evaluate_close_request(context);
        }
        self.handle_dropped_files(context);
        self.sync_native_window_chrome(context, frame);
        self.process_pending_password_store(context);
        self.check_wake_monitor_signal();
        self.pump_all_sessions(context);
        self.state.reprompt_rejected_ssh_passwords(context);
        self.update_window_title(context);
        if let Some(smoke) = self.native_smoke.as_mut() {
            if let Some(primary_tab) = self.primary_tab {
                if let Some(primary) = self.state.session_tab_mut(primary_tab) {
                    let color_emoji_paints = primary.view.diagnostics().color_emoji_paints;
                    smoke.drive(
                        context,
                        &mut primary.terminal,
                        &mut primary.controller,
                        color_emoji_paints,
                    );
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
        // Native decorations (and the OS resize border they carry) are only
        // enabled on macOS (`main.rs`); every other platform needs this
        // painter-drawn substitute. Runs first, over the full window rect,
        // before any nested layout below narrows `ui`'s `max_rect`.
        if !cfg!(target_os = "macos") {
            chrome::handle_resize_border(ui);
        }
        self.handle_native_menu_commands(ui.ctx());
        self.handle_shortcuts(ui.ctx());
        self.update_native_menu();
        self.sync_port_forward_manager();
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
                || self.overlays.pending_file_drop.is_some()
                || self.overlays.pending_settings_reset.is_some()
                || self.overlays.pending_quit.is_some());
        let port_forward_manager_escape =
            escape_pressed && self.overlays.port_forward_manager.is_some();
        let about_escape = escape_pressed && self.overlays.about_open;

        if !self.focus_mode {
            let (chips, active_chip) = self.chip_view_models();
            let inspector_open = self.state.inspector_open();
            let inspector_available =
                matches!(self.state.active_tab().content, TabContent::Session(_));
            // Feature request #69: only overlays chip quick-switch numbers
            // while the same modifier the quick-switch shortcut itself uses
            // is currently held, so the visual cue always matches the live
            // shortcut, not just at the moment a chord completes.
            let quick_switch_overlay_active = self.state.quick_switch_overlay()
                && ui.ctx().input(|input| input.modifiers.command);
            let actions = chrome::show(
                ui,
                &chips,
                active_chip,
                inspector_open,
                inspector_available,
                self.state.chip_layout(),
                self.state.show_session_details(),
                quick_switch_overlay_active,
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
        // Search's own `TextEdit` consumes Escape itself while focused; this
        // covers Escape pressed after the user clicked back into the
        // terminal without closing the find bar first.
        let search_escape = !inspector_escape
            && matches!(&self.state.active_tab().content, TabContent::Session(session) if session.search.is_open())
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
        // Rendered before `TerminalView::show_with_options` below: while the
        // find bar is open that call marks `terminal_input_enabled = false`
        // and strips this frame's keyboard/text events from the shared
        // input queue for a full modal-style blackout. The find bar's own
        // `TextEdit` must see its `Text`/`Key` events before that happens,
        // so it renders first even though it paints above the terminal.
        self.show_terminal_find_bar(&ui.ctx().clone(), content_rect);
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
        let mut deferred_links = Vec::new();
        let chip_layout = self.state.chip_layout();
        let native_store_available = self.native_store_available();
        let secure_storage_status = self.secure_storage_status_message();
        let active_tab_id = self.state.active();
        let scroll_speed_multiplier = self.state.scroll_speed().multiplier();
        let terminal_font_set = self.terminal_font_set();
        let sftp_pane_order = self.state.sftp_pane_order();
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    let resumable_sessions = if self.state.show_resumable_sessions() {
                        festerm_sessiond::list_unattached_local_sessions()
                    } else {
                        Vec::new()
                    };
                    screen_command = screens::show_launcher(
                        ui,
                        active_tab_id,
                        self.state.configuration().profiles(),
                        native_store_available,
                        secure_storage_status,
                        self.state.compact_launcher_grid(),
                        &resumable_sessions,
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
                            emoji_presentation: self.state.emoji_presentation(),
                            scroll_speed: self.state.scroll_speed(),
                            scrollback_limit: self.state.scrollback_limit(),
                            quick_switch_overlay: self.state.quick_switch_overlay(),
                            compact_launcher_grid: self.state.compact_launcher_grid(),
                            pulse_new_output_dot: self.state.pulse_new_output_dot(),
                            show_resumable_sessions: self.state.show_resumable_sessions(),
                            default_sftp_local_directory: self
                                .state
                                .default_sftp_local_directory()
                                .map(|path| path.to_string_lossy().into_owned()),
                            sftp_pane_order: self.state.sftp_pane_order(),
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
                TabContent::MarkdownViewer(tab) => {
                    screen_command = tab.show(ui, active_tab_id);
                }
                TabContent::SshAuthenticationRequired(tab) => {
                    screen_command = screens::show_ssh_authentication_required(
                        ui,
                        active_tab_id,
                        &tab.profile,
                        native_store_available,
                    );
                }
                TabContent::SftpAuthenticationRequired(tab) => {
                    screen_command = screens::show_sftp_authentication_required(
                        ui,
                        active_tab_id,
                        &tab.profile,
                        native_store_available,
                    );
                }
                TabContent::SftpFileManagerAuthenticationRequired(tab) => {
                    screen_command = sftp_file_manager::show_authentication_required(
                        ui,
                        active_tab_id,
                        &tab.target,
                    );
                }
                TabContent::SftpFileManager(tab) => {
                    tab.set_pane_order(sftp_pane_order);
                    tab.show(ui);
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
                                && !self.palette.is_open()
                                && !session.search.is_open(),
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
                        deferred_links = session.view.take_link_requests();
                    }
                    session
                        .controller
                        .observe_resize_probe_terminal_state(&session.terminal);
                    session
                        .controller
                        .forward_terminal_replies(&mut session.terminal);
                    session.controller.flush_pending_writes();
                    session.controller.flush_pending_resize();
                    session.controller.pump_events(&mut session.terminal);
                    if session.controller.last_pump_output_received() {
                        ui.ctx().request_repaint();
                    }
                    overlay_action = overlay::show(ui.ctx(), session.chip_status());
                }
            }
        }
        for command in take_viewer_commands(ui.ctx()) {
            let context = ui.ctx().clone();
            self.state.dispatch(command, &context);
        }
        if search_escape {
            self.close_terminal_search(&ui.ctx().clone());
        }
        if paste_was_pending && !deferred_pastes.is_empty() {
            // A later clipboard-delivery event invalidates the captured
            // operation. Never replace an open dialog or route a second paste.
            self.cancel_paste_confirmation();
        } else if !self.overlays.blocks_terminal_input() && deferred_pastes.len() == 1 {
            self.handle_paste_request(active_tab_id, deferred_pastes.remove(0));
        }
        for link in deferred_links {
            self.request_external_link(link.as_ref(), ui.ctx());
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
                InspectorAction::OpenSftp => {
                    if let Some(target) = self.state.sftp_file_manager_target_for_tab(active_tab_id)
                    {
                        self.state
                            .dispatch(AppCommand::OpenSftpFileManager { target }, &context);
                    }
                }
            }
        }
        if let Some(command) = screen_command {
            match command {
                AppCommand::OpenLocalMarkdownFile { path } => {
                    let context = ui.ctx().clone();
                    self.state
                        .dispatch(AppCommand::OpenLocalMarkdownFile { path }, &context);
                }
                AppCommand::StartStoredPasswordSshProfile {
                    profile_id,
                    options,
                } => {
                    self.start_stored_password_profile_with_options(
                        profile_id,
                        options,
                        &ui.ctx().clone(),
                    );
                }
                AppCommand::StartConfiguredSshProfile { profile_id } => {
                    self.start_configured_ssh_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StartConfiguredSftpProfile { profile_id } => {
                    self.start_configured_sftp_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StoreSshPassword {
                    profile_id,
                    password,
                    options,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    festerm_ssh::SshSessionOptions::new(),
                    Some(StoredCredentialLaunch::Ssh(options)),
                    &ui.ctx().clone(),
                ),
                AppCommand::StartStoredPasswordSftpProfile { profile_id } => {
                    self.start_stored_sftp_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StartStoredSftpFileManagerProfile { profile_id } => {
                    self.start_stored_sftp_file_manager_profile(profile_id, &ui.ctx().clone());
                }
                AppCommand::StoreSftpPassword {
                    profile_id,
                    password,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    festerm_ssh::SshSessionOptions::new(),
                    Some(StoredCredentialLaunch::Sftp),
                    &ui.ctx().clone(),
                ),
                AppCommand::StoreProfilePassword {
                    profile_id,
                    password,
                } => self.store_password_for_profile(
                    profile_id,
                    password,
                    festerm_ssh::SshSessionOptions::new(),
                    None,
                    &ui.ctx().clone(),
                ),
                AppCommand::StoreProfilePrivateKey {
                    profile_id,
                    private_key,
                } => self.store_private_key_for_profile(
                    profile_id,
                    private_key,
                    festerm_ssh::SshSessionOptions::new(),
                    None,
                    &ui.ctx().clone(),
                ),
                AppCommand::SaveProfile { profile } => {
                    self.save_profile(profile);
                }
                AppCommand::SaveProfileWithCredential {
                    profile,
                    credential,
                } => self.save_profile_with_credential(profile, credential, &ui.ctx().clone()),
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
                | AppCommand::SetScrollSpeed(_)
                | AppCommand::SetScrollbackLimit(_)
                | AppCommand::SetDefaultSftpLocalDirectory(_)
                | AppCommand::SetSftpPaneOrder(_)) => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(command, &context);
                    self.persist_interface_settings();
                }
                command @ (AppCommand::ToggleTerminalLigatures
                | AppCommand::SetEmojiPresentation(_)) => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(command, &context);
                    self.apply_terminal_font_policy();
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

        self.sync_port_forward_manager();
        if self.overlays.pending_close.is_some() {
            self.show_close_confirmation(ui.ctx(), confirmation_escape);
        } else if self.overlays.pending_quit.is_some() {
            self.show_quit_confirmation(ui.ctx(), confirmation_escape);
        } else if self.overlays.pending_settings_reset.is_some() {
            self.show_settings_reset_confirmation(ui.ctx(), confirmation_escape);
        } else if self.overlays.pending_file_drop.is_some() {
            self.show_file_drop_confirmation(ui.ctx(), confirmation_escape);
        } else {
            self.show_paste_confirmation(ui.ctx(), confirmation_escape);
        }

        if port_forward_manager_escape {
            self.close_port_forward_manager(ui.ctx());
        } else {
            self.show_port_forward_manager(ui.ctx(), content_rect);
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
            quit_confirmed: false,
        }
    }

    fn for_test_with_live_session(context: &egui::Context) -> (Self, TabId) {
        let (state, tab) = AppState::with_primary_session(context, None, Configuration::empty());
        let mut app = Self::for_test_with_configuration(Configuration::empty());
        app.state = state;
        (app, tab)
    }

    fn for_test_with_fake_ssh_session(
        events: impl IntoIterator<Item = festerm_session::SessionEvent>,
    ) -> (Self, TabId, crate::session_controller::fake::FakeSshSession) {
        let mut app = Self::for_test_with_configuration(Configuration::empty());
        let session = crate::session_controller::fake::FakeSshSession::new(events);
        let tab = app.state.replace_active_with_test_ssh_session(
            session.clone(),
            "test-user",
            "ssh.example.test",
            22,
        );
        (app, tab, session)
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
    fn closing_with_no_live_sessions_needs_no_quit_confirmation() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());

        app.evaluate_close_request(&context);

        assert!(app.overlays.pending_quit.is_none());
    }

    #[test]
    fn closing_with_a_live_session_shows_aggregate_quit_confirmation() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);

        app.evaluate_close_request(&context);

        let pending = app.overlays.pending_quit.expect("should be pending");
        assert_eq!(pending.counts.local, 1);
        assert_eq!(pending.counts.ssh, 0);
        assert_eq!(pending.counts.serial, 0);
    }

    #[test]
    fn native_smoke_close_bypasses_live_session_quit_confirmation() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.native_smoke = Some(NativeWindowSmoke::finished_for_test());

        app.evaluate_close_request(&context);

        assert!(app.overlays.pending_quit.is_none());
    }

    #[test]
    fn a_second_close_request_does_not_reopen_the_quit_confirmation() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);

        app.evaluate_close_request(&context);
        app.overlays
            .pending_quit
            .as_mut()
            .unwrap()
            .cancel_focus_requested = true;
        app.evaluate_close_request(&context);

        // Still the same pending confirmation (focus flag untouched by a
        // second, redundant close-requested event), not a fresh one.
        assert!(
            app.overlays
                .pending_quit
                .expect("should still be pending")
                .cancel_focus_requested
        );
    }

    #[test]
    fn aggregate_quit_confirmation_is_safe_by_default_and_confirmed_deliberately() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.evaluate_close_request(&context);
        assert!(app.overlays.pending_quit.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Cancel").is_focused());

        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(harness.state().overlays.pending_quit.is_some());
        assert!(!harness.state().quit_confirmed);

        harness.get_by_label("Quit fesTerm").click();
        harness.step();
        assert!(harness.state().overlays.pending_quit.is_none());
        assert!(harness.state().quit_confirmed);
    }

    #[test]
    fn escape_cancels_the_aggregate_quit_confirmation_without_quitting() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.evaluate_close_request(&context);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().overlays.pending_quit.is_none());
        assert!(!harness.state().quit_confirmed);
    }

    #[derive(Debug)]
    struct FakeDroppedFile(std::path::PathBuf);

    impl egui::DroppedFile for FakeDroppedFile {
        fn path(&self) -> &std::path::Path {
            &self.0
        }

        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("test file drops are never read".to_owned())
        }
    }

    fn simulate_file_drop(context: &egui::Context, paths: &[&str]) {
        context.input_mut(|input| {
            for path in paths {
                input
                    .raw
                    .dropped_files
                    .push(std::sync::Arc::new(FakeDroppedFile(path.into())));
            }
        });
    }

    #[test]
    fn dropping_a_file_on_a_live_local_session_stages_a_bounded_preview() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        simulate_file_drop(&context, &["/tmp/example.txt"]);

        app.handle_dropped_files(&context);

        let pending = app
            .overlays
            .pending_file_drop
            .expect("should stage a preview");
        assert_eq!(pending.tab, tab);
        assert_eq!(pending.text, "/tmp/example.txt");
        assert_eq!(pending.path_count, 1);
    }

    #[test]
    fn dropping_multiple_files_preserves_drop_order_space_joined() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        simulate_file_drop(&context, &["/tmp/b.txt", "/tmp/a.txt"]);

        app.handle_dropped_files(&context);

        let pending = app
            .overlays
            .pending_file_drop
            .expect("should stage a preview");
        assert_eq!(pending.text, "/tmp/b.txt /tmp/a.txt");
        assert_eq!(pending.path_count, 2);
    }

    #[test]
    fn dropping_a_file_on_launcher_is_rejected_with_a_transient_notice() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        simulate_file_drop(&context, &["/tmp/example.txt"]);

        app.handle_dropped_files(&context);

        assert!(app.overlays.pending_file_drop.is_none());
        assert!(app.overlays.transient_notice.is_some());
    }

    #[test]
    fn file_drop_confirmation_is_safe_by_default_and_confirmed_deliberately() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        simulate_file_drop(&context, &["/tmp/example.txt"]);
        app.handle_dropped_files(&context);
        assert!(app.overlays.pending_file_drop.is_some());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(440.0, 400.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Cancel").is_focused());

        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(harness.state().overlays.pending_file_drop.is_some());

        harness.get_by_label("Insert Path").click();
        harness.step();
        assert!(harness.state().overlays.pending_file_drop.is_none());
        // Confirming routes the path text through the same `Paste` input
        // path as an ordinary paste; verifying overlay/session identity
        // stays intact here (rather than asserting on real PTY echo
        // timing, which content assertions elsewhere in this file avoid
        // for the same reason) is the deterministic part of this contract.
        assert_eq!(harness.state().state.active(), tab);
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Session(_)
        ));
    }

    #[test]
    fn escape_cancels_file_drop_without_inserting_anything() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        simulate_file_drop(&context, &["/tmp/example.txt"]);
        app.handle_dropped_files(&context);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().overlays.pending_file_drop.is_none());
    }

    #[test]
    fn open_terminal_search_opens_find_bar_and_focuses_query() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.open_terminal_search(&context);
        assert!(app.state.session_tab_mut(tab).unwrap().search.is_open());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.get_by_label("Find:").is_focused());
    }

    #[test]
    fn terminal_search_finds_and_navigates_matches() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        {
            let session = app.state.session_tab_mut(tab).unwrap();
            session
                .terminal
                .ingest(b"alpha line\r\nbeta line\r\nalpha again\r\n");
        }
        app.open_terminal_search(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness.get_by_label("Find:").type_text("alpha");
        harness.run();

        {
            let search = &harness
                .state_mut()
                .state
                .session_tab_mut(tab)
                .unwrap()
                .search;
            assert_eq!(search.match_count(), 2);
            assert_eq!(search.current_position(), Some(1));
        }

        harness.get_by_label("\u{2193}").click();
        harness.step();
        assert_eq!(
            harness
                .state_mut()
                .state
                .session_tab_mut(tab)
                .unwrap()
                .search
                .current_position(),
            Some(2)
        );
    }

    #[test]
    fn escape_closes_terminal_search_and_restores_terminal_focus() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        app.open_terminal_search(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(!harness
            .state_mut()
            .state
            .session_tab_mut(tab)
            .unwrap()
            .search
            .is_open());
    }

    #[test]
    fn find_in_terminal_palette_command_opens_the_find_bar() {
        let context = egui::Context::default();
        let (mut app, tab) = FesTermApp::for_test_with_live_session(&context);
        const FIND_IN_TERMINAL: u64 = 15;
        app.dispatch_palette_selection(FIND_IN_TERMINAL, &context);
        assert!(app.state.session_tab_mut(tab).unwrap().search.is_open());
    }

    fn sample_port_forwards() -> Vec<SshPortForwardRuntime> {
        vec![
            SshPortForwardRuntime::new(
                SshPortForwardDirection::Local,
                "127.0.0.1",
                15432,
                "db.internal",
                5432,
                SshPortForwardSource::Profile,
                SshPortForwardState::Active,
                None,
            ),
            SshPortForwardRuntime::new(
                SshPortForwardDirection::Remote,
                "127.0.0.1",
                18080,
                "127.0.0.1",
                8080,
                SshPortForwardSource::Ephemeral,
                SshPortForwardState::Failed,
                Some("remote bind denied".to_owned()),
            ),
        ]
    }

    #[test]
    fn opening_the_port_forward_manager_shows_an_empty_state() {
        let context = egui::Context::default();
        let (mut app, _tab, session) = FesTermApp::for_test_with_fake_ssh_session([]);

        app.open_port_forward_manager(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        assert!(harness.query_by_label("Port Forward Manager").is_some());
        assert!(harness
            .query_by_label("No live port forwards yet.")
            .is_some());
        assert_eq!(
            session.operations(),
            vec![crate::session_controller::fake::FakeSshOperation::Query]
        );
    }

    #[test]
    fn port_forward_manager_renders_runtime_snapshots_and_failure_details() {
        let context = egui::Context::default();
        let (mut app, _tab, _session) = FesTermApp::for_test_with_fake_ssh_session([
            festerm_session::SessionEvent::PortForwardsUpdated(sample_port_forwards()),
        ]);
        app.open_port_forward_manager(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        for label in [
            "127.0.0.1:15432",
            "db.internal:5432",
            "Profile",
            "Active",
            "127.0.0.1:18080",
            "127.0.0.1:8080",
            "Ephemeral",
            "Failed",
            "remote bind denied",
        ] {
            assert!(
                harness.query_by_label(label).is_some(),
                "expected {label:?} in overlay"
            );
        }
        assert!(harness.query_by_label("2 active forwards").is_none());
        assert!(harness.query_by_label("1 active forward").is_some());
    }

    #[test]
    fn port_forward_manager_submit_dispatches_an_add_request() {
        let context = egui::Context::default();
        let (mut app, _tab, session) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.open_port_forward_manager(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        {
            let manager = harness
                .state_mut()
                .overlays
                .port_forward_manager
                .as_mut()
                .expect("overlay should stay open");
            manager.draft.bind_port = "15432".to_owned();
            manager.draft.destination_host = "db.internal".to_owned();
            manager.draft.destination_port = "5432".to_owned();
        }
        harness.run();
        harness.get_by_label("Add live forward").click();
        harness.run();

        assert_eq!(
            session.operations(),
            vec![
                crate::session_controller::fake::FakeSshOperation::Query,
                crate::session_controller::fake::FakeSshOperation::Add {
                    direction: SshPortForwardDirection::Local,
                    bind_host: "127.0.0.1".to_owned(),
                    bind_port: 15432,
                    destination_host: "db.internal".to_owned(),
                    destination_port: 5432,
                },
            ]
        );
    }

    #[test]
    fn port_forward_manager_remove_dispatches_a_remove_request() {
        let context = egui::Context::default();
        let (mut app, _tab, session) = FesTermApp::for_test_with_fake_ssh_session([
            festerm_session::SessionEvent::PortForwardsUpdated(vec![
                sample_port_forwards()[0].clone()
            ]),
        ]);
        app.open_port_forward_manager(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness
            .get_by_label("Remove forward 127.0.0.1:15432")
            .click();
        harness.run();

        assert_eq!(
            session.operations(),
            vec![
                crate::session_controller::fake::FakeSshOperation::Query,
                crate::session_controller::fake::FakeSshOperation::Remove {
                    direction: SshPortForwardDirection::Local,
                    bind_host: "127.0.0.1".to_owned(),
                    bind_port: 15432,
                },
            ]
        );
    }

    #[test]
    fn port_forward_manager_shortcut_and_palette_entry_open_the_overlay() {
        let context = egui::Context::default();
        let (app, _tab, _session) = FesTermApp::for_test_with_fake_ssh_session([]);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.key_press_modifiers(
            ApplicationShortcut::PortForwardManager.chord().unwrap().0,
            ApplicationShortcut::PortForwardManager.chord().unwrap().1,
        );
        harness.run();
        assert!(harness.query_by_label("Port Forward Manager").is_some());

        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.query_by_label("Port Forward Manager").is_none());

        let items = harness.state().palette_items();
        assert!(items
            .iter()
            .any(|item| item.label == "Manage Port Forwards…"));
        harness.state_mut().dispatch_palette_selection(16, &context);
        harness.run();
        assert!(harness.query_by_label("Port Forward Manager").is_some());
    }

    #[test]
    fn port_forward_manager_is_gated_to_live_ssh_tabs_only() {
        let context = egui::Context::default();
        let (app, _tab) = FesTermApp::for_test_with_live_session(&context);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        let items = harness.state().palette_items();
        assert!(
            items
                .iter()
                .all(|item| item.label != "Manage Port Forwards…"),
            "non-SSH tabs must not advertise the port-forward manager"
        );
        harness.key_press_modifiers(
            ApplicationShortcut::PortForwardManager.chord().unwrap().0,
            ApplicationShortcut::PortForwardManager.chord().unwrap().1,
        );
        harness.run();
        assert!(harness.query_by_label("Port Forward Manager").is_none());
    }

    #[test]
    fn disconnecting_clears_the_port_forward_manager_list() {
        let context = egui::Context::default();
        let (mut app, _tab, session) = FesTermApp::for_test_with_fake_ssh_session([
            festerm_session::SessionEvent::PortForwardsUpdated(sample_port_forwards()),
        ]);
        app.open_port_forward_manager(&context);

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        assert!(harness.query_by_label("127.0.0.1:15432").is_some());

        session.push_event(festerm_session::SessionEvent::Lifecycle(
            festerm_session::SessionLifecycle::Disconnected(festerm_session::SessionError::new(
                festerm_session::SessionErrorKind::Spawn,
                "network lost",
            )),
        ));
        harness.run();

        assert!(harness.query_by_label("127.0.0.1:15432").is_none());
        assert!(harness
            .query_by_label(
                "Port forwarding is available only while this SSH session is connected."
            )
            .is_some());
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
    fn chip_pulses_only_for_a_background_session_with_new_output_and_the_preference_on() {
        // Feature request #68: `chip_view_models` gates the pulse flag on
        // (a) the "pulse on new output" preference being on, (b) the tab
        // being a session with unseen output, and (c) that tab NOT being
        // the currently active one - the active tab's own chip must never
        // pulse even if its flag happens to be set.
        let context = egui::Context::default();
        let (mut app, first) = FesTermApp::for_test_with_live_session(&context);
        app.state.dispatch(AppCommand::StartLocalSession, &context);
        let second = app.state.active();

        // Preference off (default): even a flagged background tab does not
        // pulse.
        if let Some(session) = app.state.session_tab_mut(first) {
            session.has_new_output_since_active = true;
        }
        let (chips, _) = app.chip_view_models();
        assert!(
            chips.iter().all(|chip| !chip.pulse_new_output),
            "no chip should pulse while the preference is off"
        );

        // Preference on: the flagged background tab (`first`) pulses, but
        // the active tab (`second`) never does, even if its own flag were
        // also set.
        app.state
            .dispatch(AppCommand::TogglePulseNewOutputDot, &context);
        if let Some(session) = app.state.session_tab_mut(second) {
            session.has_new_output_since_active = true;
        }
        let (chips, active_chip) = app.chip_view_models();
        let first_chip = chips
            .iter()
            .find(|chip| chip.id != active_chip)
            .expect("expected a background chip");
        let second_chip = chips
            .iter()
            .find(|chip| chip.id == active_chip)
            .expect("expected the active chip");
        assert!(
            first_chip.pulse_new_output,
            "a flagged background tab must pulse when the preference is on"
        );
        assert!(
            !second_chip.pulse_new_output,
            "the active tab's own chip must never pulse"
        );
    }

    #[test]
    fn modest_background_session_output_sets_the_new_output_flag_via_real_pump() {
        // Regression test for a bug (user-reported: the pulse never showed
        // despite the preference being on and background tabs producing
        // output) where `pump_all_sessions` gated `has_new_output_since_active`
        // on `SessionController::pump_events`'s own `bool` return value -
        // which only reports whether the bounded per-frame drain hit
        // `MAX_SESSION_EVENTS_PER_FRAME` (a backpressure signal), not "did
        // output arrive". A real local shell's startup banner/prompt is a
        // few dozen bytes across a handful of events - nowhere near that
        // cap - so the flag was essentially never set for ordinary output.
        // Exercises a real spawned local session end-to-end through
        // `pump_all_sessions` rather than manually setting the flag (unlike
        // `chip_pulses_only_for_a_background_session_with_new_output_and_the_preference_on`
        // above, which only covers `chip_view_models`'s gating logic once
        // the flag is already set).
        let context = egui::Context::default();
        let (mut app, first) = FesTermApp::for_test_with_live_session(&context);
        app.state
            .dispatch(AppCommand::TogglePulseNewOutputDot, &context);
        // Starting a second local session makes it active, leaving `first`
        // in the background while its real shell process starts up and
        // prints its initial prompt.
        app.state.dispatch(AppCommand::StartLocalSession, &context);
        assert_ne!(app.state.active(), first, "the new session must be active");

        let deadline = Instant::now() + Duration::from_millis(2_500);
        let mut pulsing = false;
        while Instant::now() < deadline {
            app.pump_all_sessions(&context);
            let (chips, _) = app.chip_view_models();
            if chips
                .iter()
                .find(|chip| chip.id == ChipId(first.chip_id()))
                .is_some_and(|chip| chip.pulse_new_output)
            {
                pulsing = true;
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            pulsing,
            "a background tab's real (modest) shell startup output must set the pulse flag"
        );
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
    fn command_palette_always_offers_open_markdown_file() {
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "Open Markdown File…"));
    }

    #[test]
    fn markdown_palette_picker_selection_opens_the_selected_file() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.dispatch_palette_selection_with_picker(17, &context, || {
            Some(std::path::PathBuf::from("/docs/readme.md"))
        });

        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "Reload Markdown"));
    }

    #[test]
    fn saved_sftp_profile_launches_its_configured_surface_mode() {
        let context = egui::Context::default();
        for (gui_mode, expected_gui_surface) in [(true, true), (false, false)] {
            let configuration = Configuration::new(vec![festerm_config::Profile::sftp(
                "files",
                "sftp.example.test",
                22,
                "deploy",
                gui_mode,
            )
            .unwrap()])
            .unwrap();
            let mut app = FesTermApp::for_test_with_configuration(configuration);

            app.start_configured_sftp_profile("files".to_owned(), &context);

            assert_eq!(
                app.state.tabs().iter().any(|tab| matches!(
                    tab.content,
                    TabContent::SftpFileManagerAuthenticationRequired(_)
                )),
                expected_gui_surface
            );
        }
    }

    #[test]
    fn markdown_viewer_palette_items_follow_the_active_viewer() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.state.dispatch(
            AppCommand::OpenLocalMarkdownFile {
                path: std::path::PathBuf::from("/docs/readme.md"),
            },
            &context,
        );

        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "Reload Markdown"));
        assert!(items
            .iter()
            .any(|item| item.label == "Toggle Preview/Source"));
        assert!(items.iter().any(|item| item.label == "Find in Markdown…"));
        assert!(items
            .iter()
            .any(|item| item.label == "Toggle Markdown Outline"));
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
            Some(StoredCredentialLaunch::Ssh(
                festerm_ssh::SshSessionOptions::manual_recovery(
                    festerm_ssh::SessionStrategy::PlainShell,
                ),
            )),
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
    fn startup_workspace_restores_sftp_tabs_as_authentication_required_surfaces() {
        let workspace = festerm_config::WorkspaceConfiguration::new(
            vec![
                festerm_config::WorkspaceTab::sftp_session("remote", "production")
                    .expect("SFTP tab is valid"),
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
        .with_interface_settings(InterfaceSettings::new(
            festerm_config::ChipLayoutPreference::SingleRowScroll,
            true,
            true,
            true,
            true,
        ))
        .expect("configuration with restore_workspace enabled is valid");

        let app = FesTermApp::with_configuration(&egui::Context::default(), configuration);

        assert_eq!(app.state.tabs().len(), 1);
        assert!(matches!(
            app.state.tabs()[0].content,
            TabContent::SftpAuthenticationRequired(_)
        ));
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
    fn clicking_a_background_chip_gives_its_terminal_keyboard_focus() {
        // User-reported bug: clicking a session chip to bring it back into
        // focus activated the tab, but the chrome row (not the terminal)
        // kept egui's own keyboard focus, so typed keys went nowhere until
        // the user separately clicked inside the terminal viewport.
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        harness.key_press_modifiers(tab_management_modifiers(), egui::Key::T);
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        // Two local sessions now exist; the second (just started) is active
        // and already holds terminal keyboard focus by construction.
        let chips = harness
            .get_all_by_label_contains(" chip")
            .collect::<Vec<_>>();
        assert_eq!(chips.len(), 2, "expected exactly two session chips");
        assert!(
            harness.get_by_label("Terminal viewport").is_focused(),
            "the freshly started session's terminal should already hold focus"
        );

        // Click the first (background) chip.
        harness
            .get_all_by_label_contains(" chip")
            .next()
            .expect("first chip")
            .click();
        harness.run();

        assert!(
            harness.get_by_label("Terminal viewport").is_focused(),
            "activating a background chip must hand keyboard focus to its terminal, \
             not leave it stranded on the chrome row"
        );
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
