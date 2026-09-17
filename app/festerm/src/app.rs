mod confirmations;

use std::{
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::documents::SharedDocuments;
use eframe::egui;
use festerm_config::{
    Configuration, EmojiPresentationPreference, InterfaceSettings, PersistenceProviderKind,
    Profile, SerialDataBits, SerialFlowControl, SerialParity, SerialStopBits,
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
    LivePortForwardManager, OverlayState, PendingFileDropConfirmation, PendingPasswordStore,
    PendingPasteConfirmation, PendingSettingsResetConfirmation, StoredCredentialLaunch,
};
use crate::screens;
use crate::sftp_file_manager::{
    self, local_home_directory, MarkdownFilePicker, MarkdownPickerOutcome,
};
use crate::tabs::{
    AppCommand, AppState, ExternalLinkTarget, HostKeyTrustDecision, InspectorTransport, TabContent,
    TabId,
};
use crate::updates::{UpdateController, UpdateStatus};

/// How often a window wakes purely to re-check the files it has open. Slower
/// than the registry's own interval on purpose: waking is cheap, but waking
/// more often than there is anything new to find is just heat.
const DOCUMENT_POLL_REPAINT: Duration = Duration::from_millis(750);

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
    /// Opens one additional fesTerm window (ADR 0032). Window-scoped only in
    /// that the request originates from the focused window; the new window is
    /// created by the composition root.
    NewWindow,
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
    /// Opens the "Open Markdown File…" picker (`Ctrl+O`/`Cmd+O`), the
    /// near-universal "open a document" chord. Inside a Markdown viewer the
    /// picked file replaces that viewer's document; anywhere else it opens a
    /// new tab.
    ///
    /// On Windows/Linux this deliberately yields to a focused terminal
    /// session, because there the chord *is* `^O` (0x0F): nano binds it to
    /// "Write Out", so claiming it would silently swallow a save and cost the
    /// user their edits, and readline binds it to `operate-and-get-next`.
    /// That matches the rule the rest of this table follows - a plain
    /// `Ctrl+<letter>` belongs to the program inside the terminal. From a
    /// terminal the picker is still one click away under "More actions".
    /// macOS is unaffected: `Cmd+O` never reaches the terminal (see
    /// `festerm_ui_egui::input::control_key`, which ignores `mac_cmd`), so
    /// there the chord stays available on every surface.
    OpenMarkdownFile,
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
    fn action(self) -> festerm_config::KeyboardAction {
        use festerm_config::KeyboardAction as A;
        match self {
            Self::CommandPalette => A::CommandPalette,
            Self::NewSession => A::NewSession,
            Self::NewWindow => A::NewWindow,
            Self::StartLocalShell => A::StartLocalShell,
            Self::CloseActiveSurface => A::CloseActiveSurface,
            Self::NextSession => A::NextSession,
            Self::PreviousSession => A::PreviousSession,
            Self::Settings => A::Settings,
            Self::SettingsHotkey => A::SettingsHotkey,
            Self::ZoomOut => A::ZoomOut,
            Self::ZoomReset => A::ZoomReset,
            Self::ClearTerminal => A::ClearTerminal,
            Self::ResetTerminal => A::ResetTerminal,
            Self::ToggleFocusMode => A::ToggleFocusMode,
            Self::PortForwardManager => A::PortForwardManager,
            Self::MarkdownFind => A::MarkdownFind,
            Self::MarkdownReload => A::MarkdownReload,
            Self::MarkdownPreviewSource => A::MarkdownPreviewSource,
            Self::MarkdownOutline => A::MarkdownOutline,
            Self::OpenMarkdownFile => A::OpenMarkdownFile,
            Self::Find => A::Find,
        }
    }
    #[cfg(test)]
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
            Self::NewWindow if cfg!(target_os = "macos") => Some((
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::N,
            )),
            Self::NewWindow => None,
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
            Self::OpenMarkdownFile => Some((egui::Modifiers::COMMAND, egui::Key::O)),
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

    #[cfg(test)]
    const fn label(self) -> Option<&'static str> {
        match self {
            Self::CommandPalette if cfg!(target_os = "macos") => Some("\u{2318}+Shift+P"),
            Self::CommandPalette => Some("Ctrl+Shift+P"),
            Self::NewSession if cfg!(target_os = "macos") => Some("\u{2318}+T"),
            Self::NewSession => Some("Ctrl+Shift+T"),
            Self::NewWindow if cfg!(target_os = "macos") => Some("\u{2318}+Shift+N"),
            Self::NewWindow => None,
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
            Self::OpenMarkdownFile if cfg!(target_os = "macos") => Some("\u{2318}+O"),
            Self::OpenMarkdownFile => Some("Ctrl+O"),
            Self::Find if cfg!(target_os = "macos") => Some("\u{2318}+F"),
            Self::Find => Some("Ctrl+Shift+F"),
        }
    }

    fn consume(self, context: &egui::Context, scope: crate::keyboard::ShortcutContext) -> bool {
        let bindings = context.data(|data| {
            data.get_temp::<festerm_config::KeyboardBindings>(egui::Id::new(
                "effective-keyboard-bindings",
            ))
            .unwrap_or_default()
        });
        scope.consume(context, &bindings, self.action())
    }
}

/// The first several open tabs, in tab-bar order, get a quick-switch
/// keystroke (`Cmd+1`..`Cmd+9` on macOS, `Ctrl+1`..`Ctrl+9` elsewhere) that
/// jumps directly to that tab, mirroring the browser/terminal convention of
/// numbering only the first nine positions. Shared by `palette_items` (to
/// display the keystroke) and `handle_shortcuts` (to act on it), so the two
/// never drift out of sync.
const MAX_QUICK_SWITCH_TABS: usize = 9;

/// Pre-formatted display text for the Nth (0-based) quick-switch slot (e.g.
/// `"\u{2318} 1"`), or `None` past `MAX_QUICK_SWITCH_TABS`.
#[cfg(test)]
fn quick_switch_label(index: usize) -> Option<String> {
    crate::keyboard::QUICK_ACTIONS
        .get(index)
        .and_then(|action| crate::keyboard::label(&Default::default(), *action))
}

const fn terminal_font_family(preference: TerminalFontPreference) -> TerminalFontFamily {
    match preference {
        TerminalFontPreference::JetBrainsMono => TerminalFontFamily::JetBrainsMono,
        TerminalFontPreference::IosevkaTerm => TerminalFontFamily::IosevkaTerm,
        TerminalFontPreference::JuliaMono => TerminalFontFamily::JuliaMono,
        TerminalFontPreference::MapleMono => TerminalFontFamily::MapleMono,
    }
}

#[derive(Clone, Copy)]
struct ClipboardPasteOrigin {
    token: u64,
    tab: TabId,
    generation: u64,
    ownership_epoch: u64,
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
    clipboard_paste: Option<ClipboardPasteOrigin>,
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
    /// Whether this window had focus last frame, so regaining it can be told
    /// from merely having it.
    window_was_focused: bool,
    terminal_font_generation: TerminalFontGeneration,
    about_icon: Option<egui::TextureHandle>,
    updates: UpdateController,
    /// A verified updater install has handed replacement and relaunch off to
    /// cargo-packager. Request the normal application close exactly once so
    /// the updater can replace the running bundle.
    update_exit_requested: bool,
    /// The user has consented to ending every live session after a verified
    /// update is installed. Kept separate from `quit_confirmed` so an install
    /// failure cannot disable ordinary quit protection.
    update_restart_authorized: bool,
    /// Set once the aggregate quit confirmation has been deliberately
    /// confirmed, so the follow-up OS close request that actually tears
    /// down the window is let through instead of being intercepted again
    /// (`docs/gui-action-graph.md` `QUIT-03`).
    quit_confirmed: bool,
    /// Which window this is (ADR 0032). The primary window owns the native
    /// menu bar, the wake monitor, native window chrome, workspace
    /// persistence, and the application quit path; a secondary window owns
    /// only its own tabs.
    role: WindowRole,
    /// A configuration document this window has just committed to disk, held
    /// for the composition root to broadcast to sibling windows (ADR 0032).
    /// Set only after a successful save, so a sibling can never adopt a
    /// document that is not on disk.
    pending_configuration_broadcast: Option<Configuration>,
    /// A secondary window's close request that survived confirmation. The
    /// composition root drains this and drops the window; the primary
    /// window's close is the ordinary application quit instead.
    window_close_accepted: bool,
    /// This window's tab list changed and the workspace needs saving. The
    /// save itself spans every window, so the composition root performs it
    /// (ADR 0033).
    workspace_save_requested: bool,
    /// Where the platform last reported this window, saved with the
    /// workspace so restore can reopen it in place (ADR 0033).
    window_geometry: Option<festerm_config::WorkspaceWindowGeometry>,
    /// Additional windows a restored workspace asks for, drained once by the
    /// composition root at startup because only it can create windows.
    pending_restored_windows: Vec<festerm_config::WorkspaceWindow>,
}

/// Distinguishes the one window that owns application-scoped host
/// integration from the additional windows that do not (ADR 0032).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowRole {
    Primary,
    Secondary,
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

/// The default durable-session provider offered to a newly created Local
/// profile, detected once per frame from what's actually installed on the
/// local `PATH`.
///
/// This is the composition-root call site for
/// [`PersistenceProviderKind::default_for_local_session`]: production code
/// performs the real `PATH` scan here so `screens::show_profiles` and its
/// drafts stay pure/parameterized and deterministically testable.
fn detect_default_local_persistence_provider() -> PersistenceProviderKind {
    PersistenceProviderKind::default_for_local_session(
        festerm_pty::is_executable_on_path("tmux"),
        festerm_pty::is_executable_on_path("screen"),
    )
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

    /// Builds one additional window (ADR 0032), sharing the application's
    /// secret-store handle and configuration source with the window it was
    /// opened from and starting on the Launcher with the current
    /// configuration.
    ///
    /// A new window deliberately does not clone the originating window's
    /// tabs: a live PTY, SSH transport, or SFTP client has exactly one owner,
    /// so a session cannot be duplicated into a second window.
    pub(crate) fn secondary_window(
        context: &egui::Context,
        configuration: Configuration,
        configuration_status: ConfigurationStartupStatus,
        configuration_reloader: ConfigurationReloader,
        secret_store: Result<Arc<dyn SecretStore>, SecretStoreError>,
        documents: SharedDocuments,
    ) -> Self {
        let mut window = Self::with_configuration_status_and_secret_store(
            context,
            // A new window starts on the Launcher. Workspace restore is the
            // primary window's startup behaviour, not something every later
            // window repeats.
            configuration.without_workspace(),
            configuration_status,
            secret_store,
        );
        window.configuration_reloader = configuration_reloader;
        // The registry is application-scoped: a second window joins the one
        // that already exists rather than starting its own.
        window.state.adopt_documents(documents);
        window.role = WindowRole::Secondary;
        // The native menu bar, the wake monitor, and native-window smoke stay
        // with the primary window; a secondary window shares the process and
        // therefore already benefits from the primary window's liveness pass.
        window.native_smoke = None;
        window.primary_tab = None;
        window
    }

    /// Fills a freshly created window with the tabs a saved workspace
    /// recorded for it (ADR 0033), in place of the Launcher it started on.
    pub(crate) fn restore_window_tabs(
        &mut self,
        context: &egui::Context,
        window: &festerm_config::WorkspaceWindow,
    ) {
        self.state = AppState::with_restored_window(
            context,
            self.state.configuration().clone().without_workspace(),
            window.tabs(),
            window.focused_tab_id(),
        );
        self.window_geometry = window.geometry().copied();
    }

    /// Fills a freshly created window with the tab dragged out to create it.
    pub(crate) fn adopt_detached_tab(&mut self, tab: crate::tabs::Tab) {
        self.state.adopt_detached_tab(tab);
    }

    /// Additional windows a restored workspace asked for, drained once.
    pub(crate) fn take_restored_windows(&mut self) -> Vec<festerm_config::WorkspaceWindow> {
        std::mem::take(&mut self.pending_restored_windows)
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
        let mut pending_restored_windows = Vec::new();
        let (state, primary_tab) = if let Some(workspace) = configuration.workspace().cloned() {
            // The saved workspace's own tabs are this, the primary, window;
            // its additional windows are opened by the composition root once
            // this one exists (ADR 0033).
            pending_restored_windows = workspace.windows().to_vec();
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
            window_was_focused: true,
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
            clipboard_paste: None,
            overlays: OverlayState::default(),
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
            wake_monitor: None,
            wake_requested: Arc::new(AtomicBool::new(false)),
            focus_mode: false,
            terminal_fonts_installed: true,
            terminal_font_generation,
            about_icon: Some(about_icon),
            updates: UpdateController::from_build(),
            update_exit_requested: false,
            update_restart_authorized: false,
            quit_confirmed: false,
            role: WindowRole::Primary,
            pending_configuration_broadcast: None,
            window_close_accepted: false,
            workspace_save_requested: false,
            window_geometry: None,
            pending_restored_windows,
        }
    }

    pub(crate) fn install_native_menu(&mut self, context: &egui::Context) {
        let context = context.clone();
        self.native_menu =
            festerm_macos_window::install_application_menu(std::sync::Arc::new(move || {
                context.request_repaint()
            }));
        self.update_native_menu();
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
        // Every window the process owns is swept, not just the one this
        // handle addresses: only eframe's root viewport exposes a
        // `raw-window-handle`, and the additional windows fesTerm opens
        // (ADR 0033) need the very same chrome - without it AppKit drags
        // *them* whenever one of their chips is dragged, so tabs could
        // never be moved out of any window but the first.
        festerm_macos_window::sync_window_chrome(f64::from(
            festerm_ui_egui::chrome::chrome_band_center_from_top(self.state.show_session_details()),
        ));

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
        while let Some(command) = self.native_menu.try_recv() {
            self.dispatch_native_menu_command(command, context);
            self.cancel_invalid_clipboard_paste(context);
        }
    }

    fn dispatch_native_menu_command(
        &mut self,
        command: festerm_macos_window::NativeMenuCommand,
        context: &egui::Context,
    ) {
        if self.overlays.blocks_terminal_input()
            && command != festerm_macos_window::NativeMenuCommand::Paste
        {
            return;
        }
        use festerm_config::KeyboardAction as A;
        use festerm_macos_window::NativeMenuCommand;
        let action = match command {
            NativeMenuCommand::Paste => None,
            NativeMenuCommand::NewSession => Some(A::NewSession),
            NativeMenuCommand::NewWindow => Some(A::NewWindow),
            NativeMenuCommand::StartLocalShell => Some(A::StartLocalShell),
            NativeMenuCommand::OpenSettings => Some(A::Settings),
            NativeMenuCommand::CloseActiveSurface => Some(A::CloseActiveSurface),
            NativeMenuCommand::ToggleCommandPalette => Some(A::CommandPalette),
            NativeMenuCommand::ClearTerminal => Some(A::ClearTerminal),
            NativeMenuCommand::ResetTerminal => Some(A::ResetTerminal),
            NativeMenuCommand::ToggleFocusMode => Some(A::ToggleFocusMode),
            NativeMenuCommand::ToggleSessionInspector => None,
        };
        if let Some(action) = action {
            let bindings = self.state.interface_settings().keyboard_bindings().clone();
            // Some native integrations also surface the accelerator key.
            // Remove that counterpart before egui dispatch, not by frame time.
            crate::keyboard::consume(context, &bindings, action);
            if let TabContent::Session(session) = &self.state.active_tab().content {
                let mut recorder = session
                    .controller
                    .input_recorder
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                recorder.set_target(
                    self.state.active().chip_id(),
                    session.controller.lifecycle_generation(),
                );
                recorder.record(
                    "native-menu",
                    festerm_ui_egui::routing_trace::Metadata {
                        class: action.title(),
                        modifiers: 0,
                    },
                    "menu-command-dispatched-key-provenance-unknown",
                    "no-delivery",
                    0,
                );
            }
        }
        match command {
            NativeMenuCommand::Paste => {
                if self.terminal_owns_input() {
                    self.paste_into_active_session(context);
                } else {
                    context.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
            }
            NativeMenuCommand::NewSession => self.state.dispatch(AppCommand::OpenLauncher, context),
            NativeMenuCommand::NewWindow => self.state.dispatch(AppCommand::OpenWindow, context),
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

    fn update_native_menu(&self) {
        let close_label = match self.state.active_tab().content {
            TabContent::Launcher => "Close Launcher",
            TabContent::Settings => "Close Settings",
            TabContent::Profiles => "Close Profiles",
            TabContent::MarkdownViewer(_) => "Close Markdown Viewer",
            TabContent::TextEditor(_) => "Close Editor",
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
        use festerm_config::KeyboardAction as A;
        use festerm_macos_window::NativeMenuCommand as N;
        let settings = self.state.interface_settings();
        let shortcuts = [
            (N::NewSession, A::NewSession),
            (N::NewWindow, A::NewWindow),
            (N::StartLocalShell, A::StartLocalShell),
            (N::OpenSettings, A::Settings),
            (N::CloseActiveSurface, A::CloseActiveSurface),
            (N::ToggleCommandPalette, A::CommandPalette),
            (N::ClearTerminal, A::ClearTerminal),
            (N::ResetTerminal, A::ResetTerminal),
            (N::ToggleFocusMode, A::ToggleFocusMode),
        ]
        .into_iter()
        .filter_map(|(command, action)| {
            if self.overlays.blocks_terminal_input()
                || (self.palette.is_open() && action != A::CommandPalette)
            {
                return None;
            }
            if action.scope() == festerm_config::KeyboardScope::Terminal
                && !self.terminal_owns_input()
            {
                return None;
            }
            let parsed = festerm_config::Chord::parse(
                settings
                    .keyboard_bindings()
                    .effective(action, cfg!(target_os = "macos")),
                cfg!(target_os = "macos"),
            )
            .ok()??;
            let key = match parsed.key {
                "Comma" => ",".into(),
                "Period" => ".".into(),
                "Tab" => "\t".into(),
                "Minus" => "-".into(),
                "Plus" => "+".into(),
                "Equals" => "=".into(),
                key if key.starts_with('F') && key.len() > 1 => {
                    char::from_u32(0xf704 + key[1..].parse::<u32>().ok()? - 1)?.to_string()
                }
                key => key.to_lowercase(),
            };
            Some(festerm_macos_window::NativeShortcut {
                command,
                key,
                control: parsed.ctrl,
                command_modifier: parsed.command,
                option: parsed.alt,
                shift: parsed.shift,
            })
        })
        .collect::<Vec<_>>();
        self.native_menu.update_shortcuts(&shortcuts);
    }

    fn handle_paste_request(
        &mut self,
        tab: TabId,
        text: String,
        clipboard_token: Option<u64>,
        context: &egui::Context,
    ) {
        let input_ownership_epoch = self.state.input_ownership_epoch();
        let text = normalize_paste_line_endings(&text);
        let Some(session) = self.state.session_tab_mut(tab) else {
            return;
        };
        if !session.accepts_input() {
            if let Some(token) = clipboard_token {
                session
                    .controller
                    .cancel_clipboard_input(token, "discarded-clipboard-cancelled");
            }
            self.show_clipboard_discard_notice(tab);
            return;
        }
        let bracketed_paste = session.terminal.modes().bracketed_paste();
        let line_count = paste_line_count(&text);
        let character_count = text.chars().count();
        let requires_confirmation = character_count >= LARGE_PASTE_CHARACTER_THRESHOLD
            || line_count >= LARGE_PASTE_LINE_THRESHOLD
            || (!bracketed_paste && line_count > 1);
        if !requires_confirmation {
            self.deliver_ordered_paste(tab, text, clipboard_token, context);
            return;
        }
        self.overlays.pending_paste = Some(PendingPasteConfirmation {
            clipboard_token,
            opened_frame: context.cumulative_frame_nr(),
            tab,
            identity: session.label.clone(),
            text,
            transport_state: session.status_bar_label(),
            lifecycle_generation: session.controller.lifecycle_generation(),
            input_ownership_epoch,
            bracketed_paste,
            cancel_focus_requested: false,
        });
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
        if self.state.sftp_file_manager_tab_mut(active).is_some() {
            self.handle_dropped_files_on_sftp_tab(context, active, &dropped);
            return;
        }
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

    /// The SFTP-file-manager half of `handle_dropped_files` (issue #137):
    /// external OS drops onto the *remote* pane upload into its current
    /// directory; drops onto the local pane (or anywhere else in the tab,
    /// e.g. the transfer rail) are explicitly unsupported, matching the
    /// terminal-session path's "never silently guess" stance. This tab's
    /// body hasn't rendered yet this frame, so the target pane is resolved
    /// from wherever `show_pane` last drew each pane, cached on the tab.
    fn handle_dropped_files_on_sftp_tab(
        &mut self,
        context: &egui::Context,
        active: crate::tabs::TabId,
        dropped: &[egui::DroppedFileHandle],
    ) {
        let Some(tab) = self.state.sftp_file_manager_tab_mut(active) else {
            return;
        };
        let drop_pos = context.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let over_remote_pane = drop_pos.is_some_and(|pos| {
            tab.last_remote_pane_rect
                .is_some_and(|rect| rect.contains(pos))
        });
        if !over_remote_pane {
            self.reject_file_drop(
                context,
                "Drop files onto the remote pane to upload them; the local pane doesn't accept file drops.",
            );
            return;
        }
        let paths: Vec<std::path::PathBuf> = dropped
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect();
        if paths.is_empty() {
            return;
        }
        match tab.enqueue_external_drop_upload(paths) {
            Ok(0) => {}
            Ok(_) => {
                context.request_repaint();
            }
            Err(message) => self.reject_file_drop(context, &message),
        }
    }

    fn reject_file_drop(&mut self, context: &egui::Context, message: &str) {
        self.overlays.transient_notice = Some((
            message.to_owned(),
            Instant::now() + Duration::from_millis(2_500),
        ));
        context.request_repaint();
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
            // The single choke point every configuration write passes
            // through, and therefore the only place sibling windows need to
            // learn about (ADR 0032). Broadcasting the committed document
            // rather than the attempted one preserves
            // commit-only-on-success: a failed save reaches no sibling.
            self.pending_configuration_broadcast = Some(replacement.clone());
            self.state.replace_configuration(replacement);
        }
        self.configuration_status = status;
        was_saved
    }

    /// The configuration source, secret-store handle and last configuration
    /// status a newly opened window should inherit (ADR 0032): these are
    /// application-scoped services, so a second window shares them rather
    /// than opening its own (which would contend for the sessiond registry
    /// lock and re-prompt the OS keychain).
    pub(crate) fn shared_application_services(
        &self,
    ) -> (
        Configuration,
        ConfigurationStartupStatus,
        ConfigurationReloader,
        Result<Arc<dyn SecretStore>, SecretStoreError>,
        SharedDocuments,
    ) {
        (
            self.state.configuration().clone(),
            self.configuration_status,
            self.configuration_reloader.clone(),
            self.secret_store.clone(),
            Rc::clone(self.state.documents()),
        )
    }

    /// Hands the composition root any configuration this window has just
    /// committed, so it can be applied to every sibling window (ADR 0032).
    pub(crate) fn take_configuration_broadcast(&mut self) -> Option<Configuration> {
        self.pending_configuration_broadcast.take()
    }

    /// Adopts a configuration a sibling window committed. Deliberately narrow:
    /// it swaps the immutable document consulted for preference reads and
    /// future Launcher choices and refreshes the derived interface state, and
    /// touches no tab, focus, scroll offset, selection, or in-progress text
    /// entry (ADR 0032).
    pub(crate) fn adopt_broadcast_configuration(&mut self, configuration: Configuration) {
        self.state.adopt_configuration(configuration);
    }

    /// True when this window's close request has been accepted and the
    /// composition root should drop it. Meaningful only for secondary
    /// windows; the primary window's close is the application quit path.
    pub(crate) const fn window_close_accepted(&self) -> bool {
        self.window_close_accepted || self.quit_confirmed
    }

    /// One-shot consumption of this window's request for another window.
    pub(crate) fn take_window_open_request(&mut self) -> bool {
        self.state.take_window_open_request()
    }

    /// Captures a metadata-only workspace snapshot and saves it immediately
    /// (`docs/gui-design.md` "Configuration": open/closed tabs, their order,
    /// and the active tab autosave on every change - there is no manual
    /// Save action). The current configuration changes only after the
    /// atomic file replacement has succeeded.
    ///
    /// Called only on the primary window and only by the composition root,
    /// which alone can see every window's tabs; `additional_windows` carries
    /// the siblings' already-captured state (ADR 0033).
    pub(crate) fn save_workspace(
        &mut self,
        additional_windows: Vec<festerm_config::WorkspaceWindow>,
        next_identifier: &mut usize,
    ) {
        if self.role == WindowRole::Secondary {
            // Every window contributes its tabs, but exactly one write
            // happens, through the same choke point as every other
            // configuration write (ADR 0015).
            return;
        }
        self.apply_configuration_save(
            self.state
                .capture_workspace_configuration(additional_windows, next_identifier),
            ConfigurationStartupStatus::WorkspaceSaveFailure,
            crate::configuration_startup::ConfigurationReloader::save_workspace,
        );
    }

    /// Captures this window's restorable tabs, focus, and geometry as one
    /// additional-window entry, or `None` when it has nothing restorable.
    pub(crate) fn capture_additional_window(
        &self,
        next_identifier: &mut usize,
    ) -> Option<festerm_config::WorkspaceWindow> {
        let (tabs, focused_tab_id) = self
            .state
            .capture_window_workspace_tabs(next_identifier)
            .ok()?;
        if tabs.is_empty() {
            return None;
        }
        Some(festerm_config::WorkspaceWindow::new(
            tabs,
            focused_tab_id,
            self.window_geometry,
        ))
    }

    /// One-shot consumption of this window's "my tab list changed" flag, so
    /// the composition root can save one workspace covering every window.
    pub(crate) fn take_workspace_save_request(&mut self) -> bool {
        std::mem::take(&mut self.workspace_save_requested)
    }

    /// Whether workspace restore is enabled, which is a configuration
    /// preference and therefore identical in every window.
    pub(crate) const fn restores_workspace(&self) -> bool {
        self.state.restore_workspace()
    }

    /// Records where the platform says this window currently is, so a saved
    /// workspace can reopen it there (ADR 0033). Platforms that refuse to
    /// report a window's own position (Wayland) leave this `None`, and the
    /// window restores at the default size wherever the platform puts it.
    fn record_window_geometry(&mut self, context: &egui::Context) {
        let (position, size) = context.input(|input| {
            let viewport = input.viewport();
            (
                viewport.outer_rect.or(viewport.inner_rect).map(|r| r.min),
                viewport.inner_rect.map(|rect| rect.size()),
            )
        });
        let (Some(position), Some(size)) = (position, size) else {
            return;
        };
        let geometry =
            festerm_config::WorkspaceWindowGeometry::new(position.x, position.y, size.x, size.y);
        if self.window_geometry != Some(geometry) {
            self.window_geometry = Some(geometry);
            // Geometry is saved with the workspace rather than on its own, so
            // a move or resize alone does not write the file; the next tab
            // change carries the new position with it.
        }
    }

    /// This window's last reported size, used to size a window detached from
    /// it like the one the tab left.
    pub(crate) fn window_size(&self) -> Option<egui::Vec2> {
        self.window_geometry.map(|geometry| {
            let (width, height) = geometry.size();
            egui::vec2(width, height)
        })
    }

    /// Moves one of this window's tabs out, with its live session intact.
    pub(crate) fn detach_tab(&mut self, id: crate::tabs::TabId) -> Option<crate::tabs::Tab> {
        self.state.detach_tab(id)
    }

    /// Takes ownership of a tab dropped onto this window.
    pub(crate) fn adopt_tab(&mut self, tab: crate::tabs::Tab, before: Option<crate::tabs::TabId>) {
        self.state.adopt_tab(tab, before);
    }

    pub(crate) fn tab_count(&self) -> usize {
        self.state.tabs().len()
    }

    pub(crate) fn has_no_tabs(&self) -> bool {
        self.state.is_empty()
    }

    pub(crate) fn open_launcher_if_empty(&mut self, context: &egui::Context) {
        self.state.open_launcher_if_empty(context);
    }

    /// One-shot consumption of a pending cross-window tab move (ADR 0033).
    pub(crate) fn take_tab_move_request(&mut self) -> Option<crate::tabs::TabMoveRequest> {
        self.state.take_tab_move_request()
    }

    /// Stamps a saved profile's last-used time and writes it through, so the
    /// launcher's "Last Used" column and its recently-used ordering survive a
    /// restart.
    ///
    /// Unlike every other configuration write this one is silent in both
    /// directions: the user did not ask for it, so neither success nor
    /// failure belongs in the status line, and a failed write costs only an
    /// ordering hint. The in-memory configuration still changes only after
    /// the atomic file replacement succeeds, matching
    /// [`Self::save_workspace`]'s commit-only-on-success rule.
    fn record_profile_launch(&mut self, profile_id: &str) {
        let Some(now) = crate::screens::unix_now_seconds() else {
            return;
        };
        let Ok(replacement) = self
            .state
            .configuration()
            .with_profile_last_used(profile_id, now)
        else {
            return;
        };
        if self
            .configuration_reloader
            .save_configuration(&replacement)
            .is_ok()
        {
            self.state.replace_configuration(replacement);
        }
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
        let prompt = if let Some(session) = self.state.session_tab_mut(tab) {
            session.host_key_prompt().cloned()
        } else if let Some(file_manager) = self.state.sftp_file_manager_tab_mut(tab) {
            file_manager.host_key_prompt().cloned()
        } else {
            None
        };
        let Some(prompt) = prompt else {
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
                ChromeAction::OpenMarkdownFile => self.open_markdown_file_picker(context),
                ChromeAction::OpenSettings => {
                    self.state.dispatch(AppCommand::OpenSettings, context)
                }
                ChromeAction::OpenProfiles => {
                    self.state.dispatch(AppCommand::OpenProfiles, context)
                }
                ChromeAction::ToggleInspector => self.toggle_inspector_from_current_focus(context),
                ChromeAction::OpenAbout => {
                    self.overlays.about_open = true;
                    self.overlays.about_licenses_open = false;
                    context.request_repaint();
                }
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
                ChromeAction::MoveToWindow {
                    moved,
                    target,
                    before,
                    screen_position,
                } => {
                    let Some(moved) = self.tab_id_for_chip(moved) else {
                        continue;
                    };
                    // `before` names a chip in the *destination* window, which
                    // this window cannot look up; identifiers are
                    // process-wide unique, so it is carried across and
                    // resolved where it means something (ADR 0033).
                    let before = before.map(|chip_id| TabId::from_chip_id(chip_id.0));
                    self.state.dispatch(
                        AppCommand::MoveTabToWindow {
                            moved,
                            target,
                            before,
                            screen_position,
                        },
                        context,
                    );
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
            self.cancel_invalid_clipboard_paste(context);
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
        use festerm_config::KeyboardAction as A;
        let bindings = self.state.interface_settings().keyboard_bindings().clone();
        let binding_label = |action| crate::keyboard::label(&bindings, action);
        const NEW_LAUNCHER_TAB: u64 = 1;
        const START_LOCAL_SESSION: u64 = 3;
        const NEW_WINDOW: u64 = 23;
        const CLOSE_ACTIVE_TAB: u64 = 5;
        const TOGGLE_FOCUS_MODE: u64 = 6;
        const ZOOM_IN: u64 = 7;
        const ZOOM_OUT: u64 = 8;
        const RESET_ZOOM: u64 = 9;
        const RESET_TERMINAL: u64 = 11;
        const CLEAR_TERMINAL_HISTORY: u64 = 12;
        const COPY: u64 = 13;
        const PASTE: u64 = 14;
        const FIND_IN_TERMINAL: u64 = 15;
        const PORT_FORWARD_MANAGER: u64 = 16;
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
                hint: binding_label(A::NewSession),
                is_tab: false,
                shortcut_label: None,
            },
            PaletteItem {
                id: START_LOCAL_SESSION,
                label: "Start Local Shell".to_owned(),
                hint: binding_label(A::StartLocalShell),
                is_tab: false,
                shortcut_label: None,
            },
            PaletteItem {
                id: NEW_WINDOW,
                label: "New Window".to_owned(),
                hint: binding_label(A::NewWindow),
                is_tab: false,
                shortcut_label: None,
            },
        ];
        if matches!(self.state.active_tab().content, TabContent::Session(_)) {
            items.extend([
                PaletteItem {
                    id: TOGGLE_FOCUS_MODE,
                    label: if self.focus_mode {
                        "Exit Focus Mode".to_owned()
                    } else {
                        "Enter Focus Mode".to_owned()
                    },
                    hint: binding_label(A::ToggleFocusMode),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: ZOOM_IN,
                    label: "Zoom In".to_owned(),
                    hint: binding_label(A::ZoomIn),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: ZOOM_OUT,
                    label: "Zoom Out".to_owned(),
                    hint: binding_label(A::ZoomOut),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: RESET_ZOOM,
                    label: "Reset Zoom".to_owned(),
                    hint: binding_label(A::ZoomReset),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: COPY,
                    label: "Copy".to_owned(),
                    hint: binding_label(A::Copy),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: PASTE,
                    label: "Paste".to_owned(),
                    hint: binding_label(A::Paste),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: FIND_IN_TERMINAL,
                    label: "Find in Terminal…".to_owned(),
                    hint: binding_label(A::Find),
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
                    hint: binding_label(A::PortForwardManager),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: RESET_TERMINAL,
                    label: "Reset Terminal".to_owned(),
                    hint: binding_label(A::ResetTerminal),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: CLEAR_TERMINAL_HISTORY,
                    label: "Clear Terminal".to_owned(),
                    hint: binding_label(A::ClearTerminal),
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
                    hint: binding_label(A::MarkdownReload),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: TOGGLE_MARKDOWN_MODE,
                    label: "Toggle Preview/Source".to_owned(),
                    hint: binding_label(A::MarkdownPreviewSource),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: FIND_IN_MARKDOWN,
                    label: "Find in Markdown…".to_owned(),
                    hint: binding_label(A::MarkdownFind),
                    is_tab: false,
                    shortcut_label: None,
                },
                PaletteItem {
                    id: TOGGLE_MARKDOWN_OUTLINE,
                    label: "Toggle Markdown Outline".to_owned(),
                    hint: binding_label(A::MarkdownOutline),
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
                TabContent::TextEditor(_) => "Close Editor".to_owned(),
                TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_)
                | TabContent::SftpFileManager(_)
                | TabContent::Session(_) => "Close Session…".to_owned(),
            },
            hint: binding_label(A::CloseActiveSurface),
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
                TabContent::TextEditor(tab) => {
                    (tab.title().to_owned(), Some(tab.origin_label().to_owned()))
                }
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
                shortcut_label: crate::keyboard::QUICK_ACTIONS
                    .get(index)
                    .and_then(|action| binding_label(*action)),
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
            3 => self.state.dispatch(AppCommand::StartLocalSession, context),
            23 => self.state.dispatch(AppCommand::OpenWindow, context),
            18 => self.state.dispatch(AppCommand::ReloadMarkdown, context),
            19 => self
                .state
                .dispatch(AppCommand::ToggleMarkdownPreviewSource, context),
            20 => self.state.dispatch(AppCommand::OpenMarkdownFind, context),
            21 => self
                .state
                .dispatch(AppCommand::ToggleMarkdownOutline, context),
            5 => {
                let active = self.state.active();
                self.request_close_tab(active, context);
            }
            6 => self.toggle_focus_mode(context),
            7 => self.zoom_active_session(ZoomCommand::In, context),
            8 => self.zoom_active_session(ZoomCommand::Out, context),
            9 => self.zoom_active_session(ZoomCommand::Reset, context),
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
    fn terminal_owns_input(&self) -> bool {
        self.terminal_surface_owns_input() && !self.overlays.blocks_terminal_input()
    }

    fn terminal_surface_owns_input(&self) -> bool {
        matches!(&self.state.active_tab().content,
            TabContent::Session(session) if !session.search.is_open()
                && session.host_key_prompt().is_none() && session.password_prompt().is_none())
            && !self.palette.is_open()
            && !self.state.inspector_open()
            && self.rename_restore_tab.is_none()
    }

    fn shortcut_context(
        &self,
        context: &egui::Context,
        before_opening_confirmation: bool,
    ) -> crate::keyboard::ShortcutContext {
        let blocked = if before_opening_confirmation && self.clipboard_confirmation_opening(context)
        {
            self.overlays.blocks_terminal_input_except_paste()
        } else {
            self.overlays.blocks_terminal_input()
        };
        crate::keyboard::ShortcutContext {
            blocked: blocked
                || crate::keyboard::composition_active(context, self.state.active().chip_id()),
            palette_open: self.palette.is_open(),
            terminal_input: self.terminal_surface_owns_input(),
            markdown_viewer: matches!(
                self.state.active_tab().content,
                TabContent::MarkdownViewer(_)
            ),
            open_markdown: cfg!(target_os = "macos")
                || !matches!(self.state.active_tab().content, TabContent::Session(_)),
            port_forward_available: matches!(&self.state.active_tab().content,
                TabContent::Session(session) if session.live_port_forwarding_available()),
            tab_count: self.state.tabs().len(),
        }
    }

    fn clipboard_confirmation_opening(&self, context: &egui::Context) -> bool {
        self.overlays.pending_paste.as_ref().is_some_and(|pending| {
            pending.clipboard_token.is_some()
                && pending.opened_frame == context.cumulative_frame_nr()
        })
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        self.cancel_invalid_clipboard_paste(ctx);
        let deferred_id = egui::Id::new("ordered-keyboard-events");
        let mut events = ctx
            .data_mut(|data| data.remove_temp::<Vec<egui::Event>>(deferred_id))
            .unwrap_or_default();
        events.extend(ctx.input_mut(|input| std::mem::take(&mut input.events)));
        // The keyboard editor is capturing a chord: hand it the keys rather
        // than dispatching them, or assigning a shortcut would also fire it.
        if crate::keyboard::recording(ctx) {
            crate::keyboard::stash_recorded_events(ctx, events);
            return;
        }
        if events
            .iter()
            .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        {
            let cancelling = self.clipboard_paste.is_some();
            self.cancel_clipboard_paste(ctx);
            if cancelling {
                let bindings = self.state.interface_settings().keyboard_bindings().clone();
                let scope = self.shortcut_context(ctx, false);
                let before = events.len();
                events.retain(|event| {
                    scope.activates(event, &bindings, true)
                        || !matches!(
                            event,
                            egui::Event::Key { .. }
                                | egui::Event::Text(_)
                                | egui::Event::Ime(_)
                                | egui::Event::Copy
                                | egui::Event::Cut
                                | egui::Event::Paste(_)
                        )
                });
                if events.len() != before {
                    self.overlays.transient_notice = Some((
                        "Clipboard operation cancelled on focus loss; pending input was not sent."
                            .into(),
                        Instant::now() + Duration::from_secs(5),
                    ));
                }
            }
        }
        // The native adapter publishes ready reads before this frame's input.
        // Resolve their queue position before any later key/text is routed.
        self.associate_input_recorder(ctx);
        self.complete_clipboard_paste(ctx);
        let mut pending = std::collections::VecDeque::from(events);
        let mut remaining = Vec::new();
        if pending.is_empty() {
            self.handle_shortcut_packet(ctx);
            self.cancel_invalid_clipboard_paste(ctx);
        }
        while let Some(event) = pending.pop_front() {
            if matches!(event, egui::Event::WindowFocused(false)) {
                self.cancel_clipboard_paste(ctx);
            }
            let new_confirmation_action = self.clipboard_confirmation_opening(ctx)
                && self.shortcut_context(ctx, true).activates(
                    &event,
                    self.state.interface_settings().keyboard_bindings(),
                    false,
                );
            if new_confirmation_action
                || (self.clipboard_paste.is_some()
                    && self.shortcut_context(ctx, false).activates(
                        &event,
                        self.state.interface_settings().keyboard_bindings(),
                        true,
                    ))
            {
                let before = remaining.len();
                remaining.retain(|event| {
                    !matches!(
                        event,
                        egui::Event::Key { .. }
                            | egui::Event::Text(_)
                            | egui::Event::Ime(_)
                            | egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                    )
                });
                let discarded = before - remaining.len();
                self.cancel_clipboard_paste(ctx);
                if new_confirmation_action {
                    self.cancel_paste_confirmation();
                }
                if discarded > 0 {
                    self.overlays.transient_notice = Some((
                        format!("Clipboard operation cancelled: pending input was not sent ({discarded} current-frame input events)."),
                        Instant::now() + Duration::from_secs(5)));
                }
            }
            // Deliver earlier widget/terminal input before a later shortcut
            // can change its owner. Retain only the unprocessed suffix.
            if remaining.iter().any(|event| {
                matches!(
                    event,
                    egui::Event::Text(_)
                        | egui::Event::Paste(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Ime(_)
                        | egui::Event::Key { pressed: true, .. }
                )
            }) && self.shortcut_context(ctx, false).activates(
                &event,
                self.state.interface_settings().keyboard_bindings(),
                false,
            ) {
                pending.push_front(event);
                ctx.data_mut(|data| {
                    data.insert_temp(deferred_id, pending.into_iter().collect::<Vec<_>>())
                });
                ctx.request_repaint();
                break;
            }
            let mut packet = vec![event];
            // The native adapter's adjacent semantic clipboard event belongs
            // to this key, not to the next frame or newly selected surface.
            if matches!(packet[0], egui::Event::Key { .. })
                && pending.front().is_some_and(|event| {
                    matches!(
                        event,
                        egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_)
                    )
                })
            {
                packet.push(pending.pop_front().unwrap());
            }
            ctx.input_mut(|input| input.events = packet);
            self.handle_shortcut_packet(ctx);
            self.cancel_invalid_clipboard_paste(ctx);
            remaining.extend(ctx.input_mut(|input| std::mem::take(&mut input.events)));
        }
        ctx.input_mut(|input| input.events = remaining);
        self.associate_input_recorder(ctx);
    }

    fn associate_input_recorder(&self, ctx: &egui::Context) {
        let recorder_id = egui::Id::new(festerm_ui_egui::routing_trace::CONTEXT_ID);
        if let TabContent::Session(session) = &self.state.active_tab().content {
            let recorder = session.controller.input_recorder.clone();
            recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .set_target(
                    self.state.active().chip_id(),
                    session.controller.lifecycle_generation(),
                );
            recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .set_focus(if self.overlays.blocks_terminal_input() {
                    "modal"
                } else if self.rename_restore_tab.is_some() {
                    "chip-rename"
                } else if self.palette.is_open() {
                    "palette"
                } else if self.state.inspector_open() {
                    "inspector"
                } else if session.search.is_open() {
                    "terminal-search"
                } else if session.host_key_prompt().is_some() || session.password_prompt().is_some()
                {
                    "authentication-widget"
                } else {
                    "terminal"
                });
            ctx.data_mut(|data| data.insert_temp(recorder_id, recorder));
        } else {
            ctx.data_mut(|data| {
                data.remove::<festerm_ui_egui::routing_trace::SharedRecorder>(recorder_id)
            });
        }
    }

    fn handle_shortcut_packet(&mut self, ctx: &egui::Context) {
        self.associate_input_recorder(ctx);
        let bindings = self.state.interface_settings().keyboard_bindings().clone();
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("effective-keyboard-bindings"),
                bindings.clone(),
            )
        });
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("command-palette-shortcut-label"),
                crate::keyboard::label(&bindings, festerm_config::KeyboardAction::CommandPalette)
                    .unwrap_or_else(|| "Unbound".into()),
            )
        });
        if crate::keyboard::composition_owns_keys(ctx, self.state.active().chip_id()) {
            return;
        }
        let scope = self.shortcut_context(ctx, false);
        if scope.blocked {
            // Opening-frame input waits behind the confirmation. Suppressed
            // app-key repeats/releases are neither actions nor terminal input.
            let opening_scope = self.shortcut_context(ctx, true);
            if !opening_scope.blocked
                && ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            egui::Event::Key { pressed: false, .. }
                                | egui::Event::Key { repeat: true, .. }
                        )
                    })
                })
            {
                for action in festerm_config::KeyboardAction::ALL {
                    opening_scope.consume(ctx, &bindings, action);
                }
                crate::keyboard::consume_exact(
                    ctx,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::F12,
                );
            }
            return;
        }
        let consume = |action| scope.consume(ctx, &bindings, action);
        let terminal_owns_input = self.terminal_owns_input();
        let paired_paste = terminal_owns_input
            .then(|| crate::keyboard::paired_paste(ctx))
            .flatten();
        let paste = consume(festerm_config::KeyboardAction::Paste)
            | consume(festerm_config::KeyboardAction::PasteAlternate);
        if terminal_owns_input {
            crate::keyboard::prepare_terminal_events(ctx);
        }
        if crate::keyboard::consume_exact(
            ctx,
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            egui::Key::F12,
        ) {
            self.palette.close();
            self.state.dispatch(AppCommand::OpenSettings, ctx);
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new("keyboard-settings-recovery"), true)
            });
        }
        let open_palette = ApplicationShortcut::CommandPalette.consume(ctx, scope);
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
            if consume(crate::keyboard::QUICK_ACTIONS[index]) {
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
        let new_tab = ApplicationShortcut::NewSession.consume(ctx, scope);
        let new_window = ApplicationShortcut::NewWindow.consume(ctx, scope);
        let start_local_shell = ApplicationShortcut::StartLocalShell.consume(ctx, scope);
        let close_tab = ApplicationShortcut::CloseActiveSurface.consume(ctx, scope);
        let next_tab = ApplicationShortcut::NextSession.consume(ctx, scope);
        let previous_tab = ApplicationShortcut::PreviousSession.consume(ctx, scope);
        // Both bindings open Settings: the legacy macOS-only `Cmd+,`
        // convention (also present as a native app-menu accelerator) and the
        // cross-platform `SettingsHotkey` shown in Settings' own Keyboard
        // card. Both `consume` calls run unconditionally so neither chord is
        // ever left un-consumed by short-circuiting.
        let settings_legacy = ApplicationShortcut::Settings.consume(ctx, scope);
        let settings_hotkey = ApplicationShortcut::SettingsHotkey.consume(ctx, scope);
        let settings = settings_legacy || settings_hotkey;
        let zoom_in = consume(festerm_config::KeyboardAction::ZoomIn)
            | consume(festerm_config::KeyboardAction::ZoomInAlternate);
        let zoom_out = ApplicationShortcut::ZoomOut.consume(ctx, scope);
        let reset_zoom = ApplicationShortcut::ZoomReset.consume(ctx, scope);
        let clear_terminal = ApplicationShortcut::ClearTerminal.consume(ctx, scope);
        let reset_terminal = ApplicationShortcut::ResetTerminal.consume(ctx, scope);
        let toggle_focus_mode = ApplicationShortcut::ToggleFocusMode.consume(ctx, scope);
        let open_port_forward_manager = ApplicationShortcut::PortForwardManager.consume(ctx, scope);
        let open_find = ApplicationShortcut::Find.consume(ctx, scope);
        let markdown_find = ApplicationShortcut::MarkdownFind.consume(ctx, scope);
        let markdown_reload = ApplicationShortcut::MarkdownReload.consume(ctx, scope);
        let markdown_toggle_mode = ApplicationShortcut::MarkdownPreviewSource.consume(ctx, scope);
        let markdown_toggle_outline = ApplicationShortcut::MarkdownOutline.consume(ctx, scope);
        let open_markdown_file = ApplicationShortcut::OpenMarkdownFile.consume(ctx, scope);
        let copy = consume(festerm_config::KeyboardAction::Copy)
            | consume(festerm_config::KeyboardAction::CopyAlternate);
        if copy {
            self.copy_active_selection(ctx);
        }
        if paste {
            self.paste_into_active_session_with_text(ctx, paired_paste);
        }

        if new_tab {
            self.state.dispatch(AppCommand::OpenLauncher, ctx);
        }
        if new_window {
            self.state.dispatch(AppCommand::OpenWindow, ctx);
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
        if open_markdown_file {
            self.open_markdown_file_picker(ctx);
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
            session.view.clear_selection();
            festerm_ui_egui::routing_trace::record_local(
                context,
                "copy-request",
                "selection-copied-and-cleared",
                0,
            );
        } else {
            festerm_ui_egui::routing_trace::record_local(
                context,
                "copy-request",
                "no-selection-no-input",
                0,
            );
        }
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    fn paste_into_active_session(&mut self, context: &egui::Context) {
        self.paste_into_active_session_with_text(context, None);
    }

    fn paste_into_active_session_with_text(
        &mut self,
        context: &egui::Context,
        text: Option<String>,
    ) {
        if !self.terminal_owns_input() {
            return;
        }
        self.cancel_clipboard_paste(context);
        let tab = self.state.active();
        let Some(session) = self
            .state
            .session_tab(tab)
            .filter(|session| session.accepts_input())
        else {
            return;
        };
        let generation = session.controller.lifecycle_generation();
        if let Some(text) = text {
            self.state
                .session_tab_mut(tab)
                .unwrap()
                .controller
                .finish_failed_clipboard_input();
            self.handle_paste_request(tab, text, None, context);
        } else if let Some(token) =
            egui_winit::clipboard_requests::request(context, egui::ViewportId::ROOT)
        {
            if !self
                .state
                .session_tab_mut(tab)
                .unwrap()
                .controller
                .begin_clipboard_input(token)
            {
                egui_winit::clipboard_requests::cancel(context, egui::ViewportId::ROOT, token);
                return;
            }
            self.clipboard_paste = Some(ClipboardPasteOrigin {
                token,
                tab,
                generation,
                ownership_epoch: self.state.input_ownership_epoch(),
            });
        }
        self.restore_active_terminal_focus();
        context.request_repaint();
    }

    fn cancel_clipboard_paste(&mut self, context: &egui::Context) {
        if let Some(origin) = self.clipboard_paste.take() {
            egui_winit::clipboard_requests::cancel(context, egui::ViewportId::ROOT, origin.token);
            if let Some(session) = self.state.session_tab_mut(origin.tab) {
                session
                    .controller
                    .fail_clipboard_input(origin.token, "discarded-clipboard-cancelled");
            }
            self.show_clipboard_discard_notice(origin.tab);
            let bindings = self.state.interface_settings().keyboard_bindings().clone();
            let scope = self.shortcut_context(context, false);
            let discarded = context.input_mut(|input| {
                let before = input.events.len();
                input.events.retain(|event| {
                    scope.activates(event, &bindings, true)
                        || !matches!(
                            event,
                            egui::Event::Key { .. }
                                | egui::Event::Text(_)
                                | egui::Event::Ime(_)
                                | egui::Event::Copy
                                | egui::Event::Cut
                                | egui::Event::Paste(_)
                        )
                });
                before - input.events.len()
            });
            if discarded > 0 {
                self.overlays.transient_notice = Some((
                    format!("Clipboard operation cancelled: pending input was not sent ({discarded} current-frame input events)."),
                    Instant::now() + Duration::from_secs(5)));
            }
        }
    }

    fn cancel_invalid_clipboard_paste(&mut self, context: &egui::Context) {
        let Some(origin) = self.clipboard_paste else {
            return;
        };
        if !self.terminal_owns_input()
            || self.state.active() != origin.tab
            || self.state.input_ownership_epoch() != origin.ownership_epoch
            || !self.state.session_tab(origin.tab).is_some_and(|session| {
                session.accepts_input()
                    && session.controller.clipboard_input_pending(origin.token)
                    && session.controller.lifecycle_generation() == origin.generation
            })
        {
            self.cancel_clipboard_paste(context);
        }
    }

    fn complete_clipboard_paste(&mut self, context: &egui::Context) {
        self.cancel_invalid_clipboard_paste(context);
        if let Some((token, text)) =
            egui_winit::clipboard_requests::take_response(context, egui::ViewportId::ROOT)
        {
            if self
                .clipboard_paste
                .is_some_and(|origin| origin.token == token)
            {
                let origin = self.clipboard_paste.unwrap();
                if let Some(text) = text {
                    self.clipboard_paste = None;
                    self.handle_paste_request(origin.tab, text, Some(origin.token), context);
                    if self
                        .state
                        .session_tab(origin.tab)
                        .is_some_and(|session| session.controller.clipboard_input_failed(token))
                    {
                        self.clipboard_paste = Some(origin);
                    }
                } else {
                    if let Some(session) = self.state.session_tab_mut(origin.tab) {
                        session
                            .controller
                            .fail_clipboard_input(origin.token, "discarded-clipboard-read-failed");
                    }
                    self.show_clipboard_discard_notice(origin.tab);
                }
            }
        }
    }

    fn deliver_ordered_paste(
        &mut self,
        tab: TabId,
        text: String,
        token: Option<u64>,
        context: &egui::Context,
    ) {
        if let Some(session) = self.state.session_tab_mut(tab) {
            let prepared =
                token.is_none_or(|token| session.controller.prepare_clipboard_input(token));
            if prepared {
                let _ = festerm_ui_egui::route_input(
                    &mut session.terminal,
                    festerm_core::InputEvent::Paste(text),
                    &mut session.controller,
                );
            }
            // Encoding or queue admission may have failed without filling the
            // reserved position. Never release the suffix in that case.
            if let Some(token) = token {
                if session.controller.clipboard_input_pending(token) {
                    session
                        .controller
                        .fail_clipboard_input(token, "discarded-clipboard-write-failed");
                }
            }
        }
        self.show_clipboard_discard_notice(tab);
        context.request_repaint();
    }

    fn show_clipboard_discard_notice(&mut self, tab: TabId) {
        let count = self.state.session_tab_mut(tab).map_or(0, |session| {
            session.controller.take_clipboard_discarded_bytes()
        });
        if count > 0 {
            self.overlays.transient_notice = Some((
                        format!("Clipboard operation cancelled or failed: {count} following input bytes were not sent. Re-enter any required input."),
                        Instant::now() + Duration::from_secs(5)));
        }
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
                    crate::keyboard::label(
                        self.state.interface_settings().keyboard_bindings(),
                        festerm_config::KeyboardAction::CommandPalette
                    )
                    .unwrap_or_else(|| "Ctrl+Shift+F12 (Settings recovery)".into())
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
            Some(UpdateAction::Install) => self.request_update_install(),
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

    /// Opens the "Open Markdown File…" picker (#132), reusing the SFTP
    /// file manager's local-pane browsing widget instead of the OS-native
    /// `rfd::FileDialog` previously used here.
    ///
    /// When the active tab is already a Markdown viewer the picked file
    /// replaces *that* document instead of opening another tab, so `Ctrl+O`
    /// behaves the way it does in every other document viewer.
    fn open_markdown_file_picker(&mut self, context: &egui::Context) {
        let active = self.state.active();
        self.overlays.markdown_file_picker_replaces = matches!(
            self.state.active_tab().content,
            TabContent::MarkdownViewer(_)
        )
        .then_some(active);
        self.overlays.markdown_file_picker = Some(MarkdownFilePicker::new(
            self.markdown_file_picker_start_directory(),
            context.clone(),
        ));
        context.request_repaint();
    }

    /// Where a freshly opened picker starts browsing: the directory the last
    /// picker was left in (if it still exists), otherwise the user's home
    /// directory.
    fn markdown_file_picker_start_directory(&self) -> std::path::PathBuf {
        self.overlays
            .markdown_file_picker_directory
            .as_ref()
            .filter(|path| path.is_dir())
            .cloned()
            .unwrap_or_else(local_home_directory)
    }

    /// Remembers where the picker was browsing so the next one resumes
    /// there. Called on every way the picker can close, not just a
    /// successful pick, because navigating to a folder and then cancelling
    /// is still the user telling us where they are working.
    fn remember_markdown_file_picker_directory(&mut self) {
        if let Some(directory) = self
            .overlays
            .markdown_file_picker
            .as_ref()
            .and_then(MarkdownFilePicker::current_directory)
        {
            self.overlays.markdown_file_picker_directory = Some(directory);
        }
    }

    fn close_markdown_file_picker(&mut self, context: &egui::Context) {
        self.remember_markdown_file_picker_directory();
        if self.overlays.markdown_file_picker.take().is_some() {
            self.overlays.markdown_file_picker_replaces = None;
            self.restore_active_terminal_focus();
            context.request_repaint();
        }
    }

    fn show_markdown_file_picker(&mut self, ctx: &egui::Context, content_rect: egui::Rect) {
        let Some(picker) = self.overlays.markdown_file_picker.as_mut() else {
            return;
        };
        picker.poll();
        let width = (content_rect.width() - 32.0).clamp(420.0, 640.0);
        let height = (content_rect.height() - 24.0).clamp(360.0, 560.0);
        let mut outcome = None;
        egui::Modal::new(egui::Id::new("markdown_file_picker")).show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_max_width(width);
                    ui.set_max_height(height);
                    ui.heading("Open Markdown File");
                    ui.add_space(6.0);
                    outcome = Some(picker.ui(ui));
                });
        });
        match outcome {
            Some(MarkdownPickerOutcome::Open(path)) => {
                self.remember_markdown_file_picker_directory();
                let replacing = self.overlays.markdown_file_picker_replaces.take();
                self.overlays.markdown_file_picker = None;
                self.restore_active_terminal_focus();
                self.state
                    .dispatch(AppCommand::OpenLocalMarkdownFile { path, replacing }, ctx);
            }
            Some(MarkdownPickerOutcome::Cancelled) => {
                self.close_markdown_file_picker(ctx);
            }
            Some(MarkdownPickerOutcome::Pending) | None => {}
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
                    // The chip is where an editor with unsaved changes is
                    // noticed from another tab, so it carries the document's
                    // severity rather than a flat neutral dot (ADR 0034 §8).
                    TabContent::TextEditor(tab) => (
                        tab.title().to_owned(),
                        Some(tab.origin_label().to_owned()),
                        tab.chip_status(self.state.documents()),
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
                // ADR 0033: the singleton application surfaces stay in the
                // window that opened them - every window can open its own
                // Launcher/Settings/Profiles, so moving one between windows
                // would only take a surface away from its source window.
                let movable_across_windows = tab.content.movable_across_windows();
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
                    movable_across_windows,
                    quick_switch_number: crate::keyboard::QUICK_ACTIONS
                        .get(index)
                        .filter(|action| {
                            self.state
                                .interface_settings()
                                .keyboard_bindings()
                                .effective(**action, cfg!(target_os = "macos"))
                                == action.default_chord(cfg!(target_os = "macos"))
                        })
                        .map(|_| (index + 1) as u8),
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
            // The Session Inspector only ever describes sessions, so a
            // document's states cannot arrive here.
            ChipStatus::Starting
            | ChipStatus::Connected
            | ChipStatus::Neutral
            | ChipStatus::DocumentSaved
            | ChipStatus::DocumentUnsaved
            | ChipStatus::DocumentConflict => None,
        };
        let recorder = session
            .controller
            .input_recorder
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let input_recording = recorder.recording();
        let input_report = recorder.report();
        drop(recorder);
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
                input_recording,
                input_report: &input_report,
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
        let show_durable_session = self.state.show_durable_session_in_status_bar();
        let mut durable_session = None;
        let (context, dimensions, system, status, status_label, detail, port_forwards) =
            match &self.state.active_tab().content {
                TabContent::Launcher
                | TabContent::Settings
                | TabContent::Profiles
                | TabContent::SshAuthenticationRequired(_)
                | TabContent::SftpAuthenticationRequired(_)
                | TabContent::SftpFileManagerAuthenticationRequired(_) => {
                    (None, None, None, ChipStatus::Neutral, "", None, None)
                }
                // Same reasoning as the file manager below: the viewer has
                // source locality, encoding, and a read-only/stale state to
                // report, which is exactly what the mockup's `.fmd-status`
                // shows. Reporting it here instead of in a viewer-owned
                // footer keeps one status band in the window.
                // The editor reports the same kinds of fact as the viewer,
                // plus where the caret is and how large the buffer is, which
                // is what the mockup's status band shows (ADR 0034 §8).
                TabContent::TextEditor(tab) => (
                    Some(tab.language_label()),
                    Some(tab.status_bar_position()),
                    Some(std::borrow::Cow::Owned(
                        tab.status_bar_encoding(self.state.documents()),
                    )),
                    tab.chip_status(self.state.documents()),
                    tab.status_bar_label(self.state.documents()),
                    Some(tab.status_bar_size(self.state.documents())),
                    None,
                ),
                TabContent::MarkdownViewer(tab) => (
                    Some(tab.status_bar_context()),
                    None,
                    Some(std::borrow::Cow::Borrowed(tab.status_bar_encoding())),
                    tab.status_bar_status(),
                    tab.status_bar_label(),
                    None,
                    None,
                ),
                // The file manager has no terminal grid to report, but it
                // does have two endpoints and a transport state, so it fills
                // the bar the way the mockup's `.fsftp-statusline` does
                // rather than leaving 24px of empty chrome under the panes.
                TabContent::SftpFileManager(tab) => (
                    Some("SFTP"),
                    None,
                    Some(std::borrow::Cow::Owned(tab.status_bar_endpoints())),
                    tab.chip_status(),
                    tab.status_bar_label(),
                    None,
                    None,
                ),
                TabContent::Session(session) => {
                    let status = session.chip_status();
                    // Durable identity is a separate question from the
                    // chip's detail, so this is gated on its own preference
                    // and may appear alongside `detail` rather than
                    // replacing it (feature request #168).
                    if show_durable_session {
                        durable_session = session.durable_session_label();
                    }
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
                        None,
                        session.view.dimensions_label(),
                        Some(std::borrow::Cow::Borrowed(session.system_label())),
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
                        context,
                        dimensions: dimensions.as_deref(),
                        system: system.as_deref(),
                        status,
                        status_label,
                        detail: detail.as_deref(),
                        durable_session: durable_session.as_deref(),
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
        self.frame_logic(context);
        self.sync_native_window_chrome(context, frame);
        self.drive_native_smoke(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_content(ui);
    }
}

impl FesTermApp {
    /// The per-frame, pre-paint work every window does, regardless of whether
    /// it is the root viewport or an additional one (ADR 0032). Split out of
    /// [`eframe::App::logic`] because a secondary window has no
    /// `eframe::Frame` of its own - `Frame`'s fields are private to `eframe`
    /// and it is only ever handed to the root viewport - and because the two
    /// pieces it excludes, native window chrome and native-window smoke, are
    /// deliberately primary-window-only.
    pub(crate) fn frame_logic(&mut self, context: &egui::Context) {
        if context.input(|i| i.viewport().close_requested()) {
            self.evaluate_close_request(context);
            // Nothing cancelled the close, so this window really is going
            // away. The primary window's teardown is eframe's; a secondary
            // window's is the composition root's.
            if self.overlays.pending_quit.is_none() {
                self.window_close_accepted = true;
            }
        }
        self.handle_dropped_files(context);
        self.process_pending_password_store(context);
        self.check_wake_monitor_signal();
        self.pump_all_sessions(context);
        self.state.reprompt_rejected_ssh_passwords(context);
        self.update_window_title(context);
        self.record_window_geometry(context);
        self.check_open_documents(context);
    }

    /// Notices outside changes to open files without anybody pressing
    /// Refresh. Every window runs this against the one shared registry; the
    /// registry's own interval means the second window's call is a no-op
    /// rather than a second stat.
    fn check_open_documents(&mut self, context: &egui::Context) {
        let documents = Rc::clone(self.state.documents());
        if documents.borrow().is_empty() {
            return;
        }
        let regained_focus = context.input(|i| i.viewport().focused).unwrap_or(true);
        let changed = if regained_focus && !self.window_was_focused {
            // The user has just come back from whatever changed the file.
            documents.borrow_mut().revalidate_all()
        } else {
            documents.borrow_mut().poll(Instant::now())
        };
        self.window_was_focused = regained_focus;
        // Auto-save runs on the same beat as the freshness poll, after it, so
        // a document that has just been found in conflict is not written a
        // frame later by the debounce that was already counting down.
        // A failure leaves its error on the document, where every view's
        // banner already reads from, so there is nothing to route here beyond
        // making the frame that shows it happen.
        let written = documents.borrow_mut().auto_save(Instant::now());
        if !changed.is_empty() || !written.is_empty() {
            context.request_repaint();
        }
        // Polling has to keep happening while the window sits idle, or an
        // outside change would only be noticed the next time something else
        // caused a repaint.
        context.request_repaint_after(DOCUMENT_POLL_REPAINT);
    }

    fn drive_native_smoke(&mut self, context: &egui::Context) {
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

    /// The full chrome/palette/session UI for one frame. Split out from
    /// [`eframe::App::ui`] so headless `egui_kittest` tests can drive it
    /// directly without constructing an `eframe::Frame` (whose fields are
    /// private to `eframe` and not test-constructible).
    pub(crate) fn ui_content(&mut self, ui: &mut egui::Ui) {
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
        if matches!(self.updates.status(), UpdateStatus::Failed { .. }) {
            self.update_restart_authorized = false;
        }
        if matches!(self.updates.status(), UpdateStatus::Installed(_))
            && self.update_restart_authorized
            && !self.update_exit_requested
        {
            self.update_exit_requested = true;
            self.quit_confirmed = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
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
        let opening_clipboard_confirmation = self.clipboard_confirmation_opening(ui.ctx());
        let terminal_input_target = (self.terminal_owns_input() || opening_clipboard_confirmation)
            .then_some(self.state.active());
        if let Some(smoke) = &mut self.native_smoke {
            smoke.observe_palette(self.palette.is_open());
        }
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
                || self.overlays.pending_document_close.is_some()
                || self.overlays.pending_quit.is_some());
        let port_forward_manager_escape =
            escape_pressed && self.overlays.port_forward_manager.is_some();
        let markdown_file_picker_escape =
            escape_pressed && self.overlays.markdown_file_picker.is_some();
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
        // Foreground widgets must see keyboard events before the terminal's
        // blackout removes them from the shared input stream.
        let inspector_action = inspector_open
            .then(|| {
                self.show_session_inspector(
                    ui.ctx(),
                    content_rect,
                    inspector_escape || inspector_outside_click,
                )
            })
            .flatten();
        let mut screen_command = None;
        let mut overlay_action = None;
        let paste_was_pending = self.overlays.pending_paste.is_some();
        let mut deferred_pastes = Vec::new();
        let mut clipboard_read_requested = false;
        let mut deferred_links = Vec::new();
        let chip_layout = self.state.chip_layout();
        let native_store_available = self.native_store_available();
        let secure_storage_status = self.secure_storage_status_message();
        let active_tab_id = self.state.active();
        let scroll_speed_multiplier = self.state.scroll_speed().multiplier();
        let terminal_font_set = self.terminal_font_set();
        let sftp_pane_order = self.state.sftp_pane_order();
        // Matches the guard used above when the bar is actually drawn.
        let status_bar_visible = self.state.status_bar_visible() && !self.focus_mode;
        self.state.update_running_sessions(ui.ctx());
        // Taken before the active tab is borrowed mutably: an editor tab reads
        // and writes the shared registry while it draws.
        let documents = Rc::clone(self.state.documents());
        {
            let tab = self.state.active_tab_mut();
            match &mut tab.content {
                TabContent::Launcher => {
                    for error in &self.state.discovery.inventory.errors {
                        ui.colored_label(theme::TEXT_SECONDARY, error);
                    }
                    if let Some(error) = &self.state.resume_error {
                        ui.colored_label(theme::TEXT_SECONDARY, error);
                    }
                    screen_command = screens::show_launcher(
                        ui,
                        active_tab_id,
                        self.state.configuration(),
                        native_store_available,
                        secure_storage_status,
                        self.state.compact_launcher_grid(),
                        &self.state.discovery.inventory.native,
                        &self.state.discovery.inventory.tmux,
                        &self.state.discovery.inventory.screen,
                    );
                }
                TabContent::Settings => {
                    screen_command = screens::show_settings(
                        ui,
                        screens::SettingsViewModel {
                            keyboard_bindings: self
                                .state
                                .interface_settings()
                                .keyboard_bindings()
                                .clone(),
                            chip_layout,
                            status_bar_visible: self.state.status_bar_visible(),
                            show_session_details: self.state.show_session_details(),
                            confirm_session_close: self.state.confirm_session_close(),
                            prefer_powershell: self.state.prefer_powershell(),
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
                            show_durable_session_in_status_bar: self
                                .state
                                .show_durable_session_in_status_bar(),
                            default_sftp_local_directory: self
                                .state
                                .default_sftp_local_directory()
                                .map(|path| path.to_string_lossy().into_owned()),
                            sftp_pane_order: self.state.sftp_pane_order(),
                        },
                    );
                }
                TabContent::Profiles => {
                    let pending_edit = self.state.take_pending_profile_edit();
                    let pending_create = self.state.take_pending_profile_create();
                    screen_command = screens::show_profiles(
                        ui,
                        active_tab_id,
                        self.state.configuration(),
                        pending_edit,
                        pending_create,
                        detect_default_local_persistence_provider(),
                    );
                }
                TabContent::MarkdownViewer(tab) => {
                    tab.set_status_bar_visible(status_bar_visible);
                    // A preview opened from an editor follows the document
                    // rather than the file; the viewer has no handle on the
                    // registry, so the text is handed to it here.
                    if let Some(document) = tab.live_document() {
                        let text = documents
                            .borrow()
                            .get(document)
                            .map(|open| open.text().text().to_owned());
                        if let Some(text) = text {
                            tab.sync_live(ui.ctx(), &text);
                        }
                    }
                    screen_command = tab.show(ui, active_tab_id);
                }
                TabContent::TextEditor(tab) => {
                    tab.set_status_bar_visible(status_bar_visible);
                    screen_command = tab.show(ui, active_tab_id, &documents);
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
                    tab.set_status_bar_visible(status_bar_visible);
                    screen_command = tab.show(ui, active_tab_id);
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
                            terminal_input_enabled: terminal_input_target == Some(active_tab_id)
                                && (!self.overlays.blocks_terminal_input()
                                    || opening_clipboard_confirmation)
                                && !self.palette.is_open()
                                && !inspector_open
                                && self.rename_restore_tab.is_none()
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
                        clipboard_read_requested = session.view.take_clipboard_read_request();
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
                    overlay_action = overlay::show(
                        ui.ctx(),
                        session.chip_status(),
                        session.reconnect_available(),
                    );
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
        }
        // Untagged Paste events may belong to an old widget request. They
        // never authorize terminal delivery; native key payloads were handled
        // in order above, and explicit terminal reads have identified replies.
        if clipboard_read_requested && terminal_input_target == Some(active_tab_id) {
            self.paste_into_active_session(ui.ctx());
        }
        for link in deferred_links {
            self.request_external_link(link.as_ref(), ui.ctx());
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
                InspectorAction::OpenSftp => {
                    if let Some(target) = self.state.sftp_file_manager_target_for_tab(active_tab_id)
                    {
                        self.state
                            .dispatch(AppCommand::OpenSftpFileManager { target }, &context);
                    }
                }
                InspectorAction::ToggleInputRecording => {
                    if let Some(session) = self.state.session_tab(active_tab_id) {
                        let enabled = !session
                            .controller
                            .input_recorder
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .recording();
                        self.state.dispatch(
                            AppCommand::SetInputRecording {
                                tab: active_tab_id,
                                enabled,
                            },
                            &context,
                        );
                    }
                }
                InspectorAction::ClearInputRecording => self
                    .state
                    .dispatch(AppCommand::ClearInputRecording(active_tab_id), &context),
                InspectorAction::CopyInputRecording => self
                    .state
                    .dispatch(AppCommand::CopyInputRecording(active_tab_id), &context),
            }
        }
        if let Some(command) = screen_command {
            match command {
                AppCommand::OpenMarkdownWorkspace => {
                    self.open_markdown_file_picker(&ui.ctx().clone());
                }
                AppCommand::OpenLocalMarkdownFile { path, replacing } => {
                    let context = ui.ctx().clone();
                    self.state.dispatch(
                        AppCommand::OpenLocalMarkdownFile { path, replacing },
                        &context,
                    );
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
                | AppCommand::TogglePreferPowershell
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
                AppCommand::SetKeyboardBindings(bindings) => {
                    if let Err(error) = bindings.validate(cfg!(target_os = "macos")) {
                        self.overlays.transient_notice =
                            Some((error.to_owned(), Instant::now() + Duration::from_secs(5)));
                    } else {
                        self.state
                            .dispatch(AppCommand::SetKeyboardBindings(bindings), ui.ctx());
                        self.persist_interface_settings();
                        self.update_native_menu();
                    }
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
                OverlayAction::Reconnect => {
                    self.state
                        .dispatch(AppCommand::ReconnectSession(self.state.active()), &context);
                }
                OverlayAction::OpenDiagnostics => {
                    self.inspector_restore_focus = None;
                    if !self.state.inspector_open() {
                        self.state
                            .dispatch(AppCommand::ToggleSessionInspector, &context);
                    }
                }
            }
        }

        self.cancel_invalid_clipboard_paste(ui.ctx());
        let tabs = self
            .state
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect::<Vec<_>>();
        for tab in tabs {
            if let Some(session) = self.state.session_tab_mut(tab) {
                session.controller.finish_failed_clipboard_input();
            }
            self.show_clipboard_discard_notice(tab);
        }
        self.sync_port_forward_manager();
        if self.overlays.pending_close.is_some() {
            self.show_close_confirmation(ui.ctx(), confirmation_escape);
        } else if self.overlays.pending_document_close.is_some() {
            self.show_document_close_confirmation(ui.ctx(), confirmation_escape);
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

        if markdown_file_picker_escape {
            self.close_markdown_file_picker(ui.ctx());
        } else {
            self.show_markdown_file_picker(ui.ctx(), content_rect);
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
            // One workspace covers every window (ADR 0033), so the window
            // that changed only reports it; the composition root gathers the
            // other windows' tabs and performs the single write.
            self.workspace_save_requested = true;
        }
        if let Some(profile_id) = self.state.take_pending_profile_usage() {
            self.record_profile_launch(&profile_id);
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
    /// Dispatches one application command as a window would, for
    /// Application-level tests that need a window in a particular state.
    pub(crate) fn dispatch_for_test(&mut self, command: AppCommand, context: &egui::Context) {
        self.state.dispatch(command, context);
    }

    pub(crate) fn tab_count_for_test(&self) -> usize {
        self.state.tabs().len()
    }

    /// Builds a primary window from a configuration that carries a saved
    /// workspace, including the additional windows the composition root then
    /// reopens (ADR 0033).
    pub(crate) fn with_restored_workspace_for_test(
        context: &egui::Context,
        configuration: Configuration,
    ) -> Self {
        let workspace = configuration
            .workspace()
            .cloned()
            .expect("a configuration with a saved workspace");
        let mut window = Self::for_test_with_configuration(configuration.clone());
        window.pending_restored_windows = workspace.windows().to_vec();
        window.state = AppState::with_restored_workspace(context, configuration, &workspace);
        window
    }

    /// Stands in for the frame that notices a changed tab list, so a test
    /// can drive the composition root's workspace save without rendering.
    pub(crate) const fn request_workspace_save_for_test(&mut self) {
        self.workspace_save_requested = true;
    }

    pub(crate) fn set_reloader_for_test(
        &mut self,
        reloader: crate::configuration_startup::ConfigurationReloader,
    ) {
        self.configuration_reloader = reloader;
    }

    pub(crate) fn set_window_geometry_for_test(
        &mut self,
        geometry: festerm_config::WorkspaceWindowGeometry,
    ) {
        self.window_geometry = Some(geometry);
    }

    pub(crate) fn tab_ids_for_test(&self) -> Vec<TabId> {
        self.state.tabs().iter().map(|tab| tab.id).collect()
    }

    pub(crate) fn active_tab_id_for_test(&self) -> TabId {
        self.state.active()
    }

    pub(crate) fn active_tab_is_launcher_for_test(&self) -> bool {
        matches!(self.state.active_tab().content, TabContent::Launcher)
    }

    pub(crate) const fn compact_launcher_grid_for_test(&self) -> bool {
        self.state.compact_launcher_grid()
    }

    pub(crate) fn profile_count_for_test(&self) -> usize {
        self.state.configuration().profiles().len()
    }

    pub(crate) fn keyboard_binding_for_test(
        &self,
        action: festerm_config::KeyboardAction,
    ) -> String {
        self.state
            .interface_settings()
            .keyboard_bindings()
            .effective(action, cfg!(target_os = "macos"))
            .to_owned()
    }

    /// Stands in for a successful configuration save, which is the only thing
    /// that queues a cross-window broadcast. Tests use this instead of a real
    /// save so they do not need a writable configuration file.
    pub(crate) fn broadcast_for_test(&mut self, configuration: Configuration) {
        self.pending_configuration_broadcast = Some(configuration.clone());
        self.state.replace_configuration(configuration);
    }

    #[cfg(test)]
    pub(crate) fn documents_for_test(&self) -> &SharedDocuments {
        self.state.documents()
    }

    /// Lets the screenshot gallery arrange a real application state -- an
    /// open document, a real close request -- rather than drawing a modal by
    /// hand that nothing else in the product would ever produce.
    pub(crate) fn dispatch_for_gallery(&mut self, command: AppCommand, context: &egui::Context) {
        self.state.dispatch(command, context);
    }

    pub(crate) fn request_active_tab_close_for_gallery(&mut self, context: &egui::Context) {
        let active = self.state.active();
        self.request_close_tab(active, context);
    }

    pub(crate) fn active_tab_for_gallery(&self) -> crate::tabs::TabId {
        self.state.active()
    }

    /// Re-reads every open document's source, which is what the per-frame
    /// freshness poll does on its own schedule; the gallery needs it to happen
    /// now so a fixture changed underneath an editor is in conflict by the
    /// time the frame is captured.
    pub(crate) fn refresh_documents_for_gallery(&mut self) {
        let documents = self.state.documents().clone();
        let ids: Vec<_> = documents.borrow().open_ids().collect();
        for id in ids {
            documents.borrow_mut().refresh(id);
        }
    }

    pub(crate) const fn accept_window_close_for_test(&mut self) {
        self.window_close_accepted = true;
    }

    pub(crate) fn for_test_with_configuration(configuration: Configuration) -> Self {
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
            clipboard_paste: None,
            overlays: OverlayState::default(),
            native_menu: festerm_macos_window::NativeMenu::unavailable(),
            wake_monitor: None,
            wake_requested: Arc::new(AtomicBool::new(false)),
            focus_mode: false,
            terminal_fonts_installed: false,
            window_was_focused: true,
            terminal_font_generation: TerminalFontGeneration::default(),
            about_icon: None,
            updates: UpdateController::unavailable_for_test(),
            update_exit_requested: false,
            update_restart_authorized: false,
            quit_confirmed: false,
            role: WindowRole::Primary,
            pending_configuration_broadcast: None,
            window_close_accepted: false,
            workspace_save_requested: false,
            window_geometry: None,
            pending_restored_windows: Vec::new(),
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
    use crate::overlay_state::{CloseConsequence, QuitConfirmationPurpose};
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness, SnapshotOptions,
    };

    fn keyboard_harness() -> (
        Harness<'static, FesTermApp>,
        crate::session_controller::fake::FakeSshSession,
    ) {
        let (app, _, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        (harness, transport)
    }

    #[test]
    fn the_status_bar_names_the_active_tabs_durable_session_only_when_asked() {
        // Feature request #168: off by default, shown for the active tab
        // when enabled, and absent again on a surface that has no durable
        // session -- without touching the session itself.
        let context = egui::Context::default();
        let (mut app, tab, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        app.state
            .session_tab_mut(tab)
            .expect("the test tab is a session")
            .inspector_transport = crate::tabs::InspectorTransport::Ssh {
            username: "deploy".to_owned(),
            host: "web-1.example.test".to_owned(),
            port: 22,
            persistence: Some(crate::tabs::InspectorPersistence {
                provider_label: festerm_config::PersistenceProviderKind::Tmux.label(),
                session_name: "deploy-watch".to_owned(),
            }),
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        assert!(
            harness.query_by_label("tmux · deploy-watch").is_none(),
            "the durable-session item must stay off until it is asked for"
        );

        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ToggleDurableSessionInStatusBar, &context);
        harness.run();
        assert!(
            harness.query_by_label("tmux · deploy-watch").is_some(),
            "an attached session must name itself once the preference is on"
        );

        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenLauncher, &context);
        harness.run();
        assert!(
            harness.query_by_label("tmux · deploy-watch").is_none(),
            "an application surface has no durable session to report"
        );
        assert!(
            transport.sent().is_empty(),
            "reporting identity must not send anything to the session"
        );
    }

    fn keyboard_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            modifiers,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
        }
    }

    fn bound_event(action: festerm_config::KeyboardAction) -> egui::Event {
        let (modifiers, key) =
            crate::keyboard::chord(action.default_chord(cfg!(target_os = "macos"))).unwrap();
        keyboard_event(key, modifiers)
    }

    fn keyboard_clipboard_request(harness: &mut Harness<'static, FesTermApp>) -> u64 {
        let context = harness.ctx.clone();
        harness.state_mut().paste_into_active_session(&context);
        egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap()
    }

    fn keyboard_clipboard_reply(context: &egui::Context, token: u64, text: &str) {
        egui_winit::clipboard_requests::complete(
            context,
            egui::ViewportId::ROOT,
            token,
            Some(text.into()),
        );
    }

    fn keyboard_second_session(
        harness: &mut Harness<'static, FesTermApp>,
    ) -> (TabId, crate::session_controller::fake::FakeSshSession) {
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenLauncher, &context);
        let transport = crate::session_controller::fake::FakeSshSession::new([]);
        let tab = harness
            .state_mut()
            .state
            .replace_active_with_test_ssh_session(
                transport.clone(),
                "controlled",
                "second.example.test",
                22,
            );
        (tab, transport)
    }

    #[test]
    fn keyboard_ready_clipboard_precedes_current_frame_enter_and_text() {
        for (event, suffix) in [
            (
                keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
                "\r",
            ),
            (egui::Event::Text("following".into()), "following"),
        ] {
            let (mut harness, transport) = keyboard_harness();
            let context = harness.ctx.clone();
            let token = keyboard_clipboard_request(&mut harness);
            keyboard_clipboard_reply(&context, token, "controlled-marker");
            harness.input_mut().events.push(event);
            harness.step();
            assert_eq!(
                transport.sent().concat(),
                format!("controlled-marker{suffix}").as_bytes()
            );
        }
    }

    #[test]
    fn keyboard_unresolved_paste_orders_same_batch_and_later_session_input() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let tab = harness.state().state.active();
        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(
            festerm_config::KeyboardAction::Paste,
            Some("Ctrl+Shift+F8".into()),
        );
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        harness.state_mut().state.dispatch(
            AppCommand::SetInputRecording { tab, enabled: true },
            &context,
        );
        harness.input_mut().events.extend([
            keyboard_event(
                egui::Key::F8,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            ),
            egui::Event::Text("controlled-following-token".into()),
            keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
        ]);
        harness.step();
        assert!(transport.sent().is_empty());
        harness
            .input_mut()
            .events
            .push(egui::Event::Text("later".into()));
        harness.step();
        assert!(transport.sent().is_empty());
        let report = harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .controller
            .input_recorder
            .lock()
            .unwrap()
            .report();
        assert!(report.contains("queued-clipboard"));
        assert!(!report.contains("controlled-following-token"));
        let token =
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap();
        keyboard_clipboard_reply(&context, token, "controlled-marker");
        harness.step();
        assert_eq!(
            transport.sent().concat(),
            b"controlled-markercontrolled-following-token\rlater"
        );
    }

    #[test]
    fn keyboard_cancelled_or_failed_clipboard_never_releases_following_input() {
        for cause in ["read-failed", "generation", "switch", "recovery"] {
            let (mut harness, transport) = keyboard_harness();
            let context = harness.ctx.clone();
            let tab = harness.state().state.active();
            let token = keyboard_clipboard_request(&mut harness);
            harness.input_mut().events.extend([
                egui::Event::Text("held-secret".into()),
                keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
            ]);
            harness.step();
            assert!(transport.sent().is_empty());
            let mut other = None;
            match cause {
                "read-failed" => {
                    egui_winit::clipboard_requests::complete(
                        &context,
                        egui::ViewportId::ROOT,
                        token,
                        None,
                    );
                    harness
                        .input_mut()
                        .events
                        .push(keyboard_event(egui::Key::Enter, egui::Modifiers::NONE));
                }
                "generation" => {
                    harness
                        .state_mut()
                        .state
                        .session_tab_mut(tab)
                        .unwrap()
                        .controller
                        .advance_lifecycle_generation();
                    keyboard_clipboard_reply(&context, token, "stale");
                    harness
                        .input_mut()
                        .events
                        .push(egui::Event::Text("old-generation".into()));
                }
                "switch" => {
                    other = Some(keyboard_second_session(&mut harness).1);
                    keyboard_clipboard_reply(&context, token, "stale");
                }
                _ => harness.input_mut().events.push(keyboard_event(
                    egui::Key::F12,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                )),
            }
            harness.step();
            assert!(transport.sent().is_empty(), "{cause}");
            assert!(other.is_none_or(|transport| transport.sent().is_empty()));
            assert!(
                harness
                    .state()
                    .overlays
                    .transient_notice
                    .as_ref()
                    .is_some_and(|(message, _)| message.contains("not sent")
                        && !message.contains("held-secret")),
                "{cause}"
            );
            assert!(harness.state().clipboard_paste.is_none());
        }
    }

    #[test]
    fn keyboard_same_batch_recovery_cancels_unencoded_following_input() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(
            festerm_config::KeyboardAction::Paste,
            Some("Ctrl+Shift+F8".into()),
        );
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        harness.input_mut().events.extend([
            keyboard_event(
                egui::Key::F8,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            ),
            egui::Event::Text("not-for-settings".into()),
            keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
            keyboard_event(
                egui::Key::F12,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            ),
        ]);
        harness.step();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
        assert!(transport.sent().is_empty());
        assert!(harness.state().overlays.transient_notice.is_some());
        assert!(harness.state().clipboard_paste.is_none());
    }

    #[test]
    fn keyboard_confirmation_opening_frame_holds_unseen_prompt_input() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
        harness.input_mut().events.extend([
            egui::Event::Text("following".into()),
            keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
        ]);
        harness.step();
        assert!(transport.sent().is_empty());
        assert!(harness.state().overlays.pending_paste.is_some());
        harness.run();
        harness.get_by_label("Paste").click();
        harness.run();
        assert_eq!(
            transport.sent().concat(),
            b"controlled-one\ncontrolled-twofollowing\r"
        );
    }

    #[test]
    fn keyboard_inactive_markdown_chord_cannot_cancel_paste_confirmation() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let mut bindings = festerm_config::KeyboardBindings::default();
        // Exercise the non-macOS default collision on every host.
        bindings.set(
            festerm_config::KeyboardAction::MarkdownFind,
            Some("Ctrl+F".into()),
        );
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
        harness.input_mut().events.extend([
            keyboard_event(egui::Key::F, egui::Modifiers::CTRL),
            keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
        ]);
        harness.step();
        assert!(transport.sent().is_empty());
        assert!(harness.state().overlays.pending_paste.is_some());
        harness.run();
        harness.get_by_label("Paste").click();
        harness.run();
        assert_eq!(
            transport.sent().concat(),
            b"controlled-one\ncontrolled-two\x06\r"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn keyboard_inactive_document_default_waits_behind_unresolved_paste() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let token = keyboard_clipboard_request(&mut harness);
        harness.input_mut().events.push(bound_event(
            festerm_config::KeyboardAction::OpenMarkdownFile,
        ));
        harness.step();
        assert!(transport.sent().is_empty());
        assert_eq!(harness.state().clipboard_paste.unwrap().token, token);
        assert!(harness.state().overlays.markdown_file_picker.is_none());
        keyboard_clipboard_reply(&context, token, "controlled-marker");
        harness.step();
        assert_eq!(transport.sent().concat(), b"controlled-marker\x0f");
    }

    #[test]
    fn keyboard_unbound_global_and_missing_tab_cannot_cancel_paste_confirmation() {
        for action in [
            festerm_config::KeyboardAction::SettingsHotkey,
            festerm_config::KeyboardAction::Quick9,
        ] {
            let (mut harness, transport) = keyboard_harness();
            let context = harness.ctx.clone();
            let mut bindings = festerm_config::KeyboardBindings::default();
            let event = if action == festerm_config::KeyboardAction::SettingsHotkey {
                bindings.set(action, Some(String::new()));
                bound_event(action)
            } else {
                bindings.set(action, Some("Ctrl+G".into()));
                keyboard_event(egui::Key::G, egui::Modifiers::CTRL)
            };
            harness
                .state_mut()
                .state
                .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
            let token = keyboard_clipboard_request(&mut harness);
            keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
            harness.input_mut().events.extend([
                event,
                keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
            ]);
            harness.step();
            assert!(transport.sent().is_empty(), "{action:?}");
            assert!(
                harness.state().overlays.pending_paste.is_some(),
                "{action:?}"
            );
            harness.run();
            harness.get_by_label("Paste").click();
            harness.run();
            let suffix: &[u8] = if action == festerm_config::KeyboardAction::Quick9 {
                b"\x07\r"
            } else if cfg!(target_os = "macos") {
                b"\r"
            } else {
                b"\x13\r"
            };
            assert_eq!(
                transport.sent().concat(),
                [b"controlled-one\ncontrolled-two".as_slice(), suffix].concat()
            );
        }
    }

    #[test]
    fn keyboard_applicable_globals_and_recovery_cancel_clipboard_barriers() {
        for ready in [false, true] {
            for action in ["settings", "switch", "palette", "recovery"] {
                let (mut harness, first) = keyboard_harness();
                let context = harness.ctx.clone();
                let first_tab = harness.state().state.active();
                let (second_tab, second) = keyboard_second_session(&mut harness);
                harness
                    .state_mut()
                    .state
                    .dispatch(AppCommand::ActivateTab(first_tab), &context);
                let token = keyboard_clipboard_request(&mut harness);
                if ready {
                    keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
                }
                let event = match action {
                    "settings" => bound_event(festerm_config::KeyboardAction::SettingsHotkey),
                    "switch" => bound_event(festerm_config::KeyboardAction::Quick2),
                    "palette" => bound_event(festerm_config::KeyboardAction::CommandPalette),
                    _ => keyboard_event(
                        egui::Key::F12,
                        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    ),
                };
                harness
                    .input_mut()
                    .events
                    .extend([egui::Event::Text("discard-with-old-read".into()), event]);
                harness.step();
                assert!(
                    harness.state().clipboard_paste.is_none(),
                    "{ready} {action}"
                );
                assert!(
                    harness.state().overlays.pending_paste.is_none(),
                    "{ready} {action}"
                );
                assert!(
                    first.sent().is_empty() && second.sent().is_empty(),
                    "{ready} {action}"
                );
                match action {
                    "settings" | "recovery" => assert!(matches!(
                        harness.state().state.active_tab().content,
                        TabContent::Settings
                    )),
                    "switch" => assert_eq!(harness.state().state.active(), second_tab),
                    _ => assert!(harness.state().palette.is_open()),
                }
                keyboard_clipboard_reply(&context, token, "stale-response");
                harness.step();
                assert!(
                    first.sent().is_empty() && second.sent().is_empty(),
                    "{ready} {action}"
                );
            }
        }
    }

    #[test]
    fn keyboard_app_repeats_and_releases_do_not_cancel_or_enter_confirmation_buffer() {
        for release in [false, true] {
            let (mut harness, transport) = keyboard_harness();
            let context = harness.ctx.clone();
            // egui derives repeat from its held-key state, not the supplied flag.
            harness
                .input_mut()
                .events
                .push(keyboard_event(egui::Key::J, egui::Modifiers::NONE));
            harness.step();
            assert!(transport.sent().is_empty());
            let mut bindings = festerm_config::KeyboardBindings::default();
            bindings.set(
                festerm_config::KeyboardAction::SettingsHotkey,
                Some("Ctrl+Shift+J".into()),
            );
            harness
                .state_mut()
                .state
                .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
            let token = keyboard_clipboard_request(&mut harness);
            keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
            let mut event =
                keyboard_event(egui::Key::J, egui::Modifiers::CTRL | egui::Modifiers::SHIFT);
            if let egui::Event::Key {
                pressed, repeat, ..
            } = &mut event
            {
                *pressed = !release;
                *repeat = !release;
            }
            harness.input_mut().events.extend([
                event,
                keyboard_event(egui::Key::Enter, egui::Modifiers::NONE),
            ]);
            harness.step();
            assert!(transport.sent().is_empty());
            assert!(harness.state().overlays.pending_paste.is_some());
            harness.run();
            harness.get_by_label("Paste").click();
            harness.run();
            assert_eq!(
                transport.sent().concat(),
                b"controlled-one\ncontrolled-two\r"
            );
        }
    }

    #[test]
    fn keyboard_opening_confirmation_bypass_does_not_bypass_another_modal() {
        let (mut harness, _) = keyboard_harness();
        let context = harness.ctx.clone();
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
        harness.step();
        let app = harness.state_mut();
        app.overlays.pending_paste.as_mut().unwrap().opened_frame = context.cumulative_frame_nr();
        assert!(!app.shortcut_context(&context, true).blocked);
        app.overlays.about_open = true;
        assert!(app.shortcut_context(&context, true).blocked);
        app.overlays.about_open = false;
        assert!(app.shortcut_context(&context, false).blocked);
    }

    #[test]
    fn keyboard_buffered_enter_waits_for_deliberate_paste_confirmation() {
        for confirm in [false, true] {
            let (mut harness, transport) = keyboard_harness();
            let context = harness.ctx.clone();
            let token = keyboard_clipboard_request(&mut harness);
            harness
                .input_mut()
                .events
                .push(keyboard_event(egui::Key::Enter, egui::Modifiers::NONE));
            harness.step();
            keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
            harness.run();
            assert!(transport.sent().is_empty());
            assert!(
                harness.state().overlays.pending_paste.is_some(),
                "buffered Enter cannot dismiss or submit the dialog"
            );
            harness
                .get_by_label(if confirm { "Paste" } else { "Cancel" })
                .click();
            // The click driver spans several frames; cancellation reports the
            // discarded input in the existing transient notification.
            for _ in 0..4 {
                harness.step();
            }
            if confirm {
                assert_eq!(
                    transport.sent().concat(),
                    b"controlled-one\ncontrolled-two\r"
                );
            } else {
                assert!(transport.sent().is_empty());
                assert!(harness.state().overlays.transient_notice.is_some());
            }
            assert!(harness.state().overlays.pending_paste.is_none());
        }
    }

    #[test]
    fn keyboard_delayed_paste_cancels_on_tab_generation_and_owner_changes() {
        for change in ["tab", "generation", "inspector", "round-trip", "window"] {
            let (mut harness, first_transport) = keyboard_harness();
            let first = harness.state().state.active();
            let context = harness.ctx.clone();
            let (second, second_transport) = keyboard_second_session(&mut harness);
            harness
                .state_mut()
                .state
                .dispatch(AppCommand::ActivateTab(first), &context);
            harness.run();
            let token = keyboard_clipboard_request(&mut harness);
            match change {
                "tab" => harness
                    .state_mut()
                    .state
                    .dispatch(AppCommand::ActivateTab(second), &context),
                "generation" => harness
                    .state_mut()
                    .state
                    .session_tab_mut(first)
                    .unwrap()
                    .controller
                    .advance_lifecycle_generation(),
                "round-trip" => {
                    harness
                        .state_mut()
                        .state
                        .dispatch(AppCommand::ActivateTab(second), &context);
                    harness
                        .state_mut()
                        .state
                        .dispatch(AppCommand::ActivateTab(first), &context);
                }
                "window" => harness.event(egui::Event::WindowFocused(false)),
                _ => harness
                    .state_mut()
                    .state
                    .dispatch(AppCommand::ToggleSessionInspector, &context),
            }
            keyboard_clipboard_reply(&context, token, "controlled-stale-marker");
            harness.run();
            assert!(harness.state().clipboard_paste.is_none());
            assert!(
                first_transport.sent().is_empty() && second_transport.sent().is_empty(),
                "{change}"
            );
            assert!(harness.state().overlays.pending_paste.is_none());
        }
    }

    #[test]
    fn keyboard_late_or_duplicate_clipboard_callbacks_cannot_fulfil_new_requests() {
        let (mut harness, first_transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let first = keyboard_clipboard_request(&mut harness);
        let replacement = keyboard_clipboard_request(&mut harness);
        assert_ne!(first, replacement);
        keyboard_clipboard_reply(&context, first, "discard-old");
        harness.run();
        assert!(first_transport.sent().is_empty());
        keyboard_clipboard_reply(&context, replacement, "accepted-first");
        harness.run();
        keyboard_clipboard_reply(&context, replacement, "discard-duplicate");
        harness.run();
        assert_eq!(first_transport.sent().concat(), b"accepted-first");
        let cancelled = keyboard_clipboard_request(&mut harness);
        let (_, second_transport) = keyboard_second_session(&mut harness);
        harness.run();
        let second = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, cancelled, "discard-old-session");
        harness.run();
        assert!(second_transport.sent().is_empty());
        assert_eq!(harness.state().clipboard_paste.unwrap().token, second);
        keyboard_clipboard_reply(&context, second, "accepted-second");
        harness.run();
        assert_eq!(second_transport.sent().concat(), b"accepted-second");
    }

    #[test]
    fn keyboard_paired_paste_uses_original_payload_before_same_batch_switch() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, first_transport) = keyboard_harness();
        let first = harness.state().state.active();
        let context = harness.ctx.clone();
        let (second, second_transport) = keyboard_second_session(&mut harness);
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ActivateTab(first), &context);
        harness.run();
        harness.input_mut().events.extend([
            bound_event(A::Paste),
            egui::Event::Paste("controlled-original-clipboard".into()),
            bound_event(A::Quick2),
        ]);
        harness.step();
        assert_eq!(harness.state().state.active(), second);
        assert_eq!(
            first_transport.sent().concat(),
            b"controlled-original-clipboard"
        );
        assert!(second_transport.sent().is_empty());
        assert!(
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT)
                .is_none()
        );
        harness.event(egui::Event::Paste("unauthorized-widget-callback".into()));
        harness.run();
        assert!(second_transport.sent().is_empty());
    }

    #[test]
    fn keyboard_async_shortcut_then_same_batch_switch_cancels_before_native_read() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, first_transport) = keyboard_harness();
        let first = harness.state().state.active();
        let context = harness.ctx.clone();
        let (second, second_transport) = keyboard_second_session(&mut harness);
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ActivateTab(first), &context);
        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(A::Paste, Some("Ctrl+Shift+F8".into()));
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        harness.run();
        harness.input_mut().events.extend([
            keyboard_event(
                egui::Key::F8,
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            ),
            bound_event(A::Quick2),
        ]);
        harness.step();
        assert_eq!(harness.state().state.active(), second);
        assert!(harness.state().clipboard_paste.is_none());
        assert!(
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT)
                .is_none()
        );
        assert!(first_transport.sent().is_empty() && second_transport.sent().is_empty());
    }

    #[test]
    fn keyboard_identified_paste_preserves_confirmation_and_generation_safety() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "controlled-one\ncontrolled-two");
        harness.run();
        assert!(transport.sent().is_empty());
        assert!(harness.state().overlays.pending_paste.is_some());
        harness.get_by_label("Paste").click();
        harness.run();
        assert_eq!(transport.sent().concat(), b"controlled-one\ncontrolled-two");
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "discard-one\ndiscard-two");
        harness.run();
        let tab = harness.state().state.active();
        harness
            .state_mut()
            .state
            .session_tab_mut(tab)
            .unwrap()
            .controller
            .advance_lifecycle_generation();
        harness.run();
        assert!(harness.state().overlays.pending_paste.is_none());
        assert_eq!(transport.sent().concat(), b"controlled-one\ncontrolled-two");
        let token = keyboard_clipboard_request(&mut harness);
        keyboard_clipboard_reply(&context, token, "discard-round\ntrip");
        harness.run();
        keyboard_second_session(&mut harness);
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ActivateTab(tab), &context);
        harness.run();
        assert!(
            harness.state().overlays.pending_paste.is_none(),
            "returning to the origin does not revive a confirmation"
        );
        assert_eq!(transport.sent().concat(), b"controlled-one\ncontrolled-two");
    }

    #[test]
    fn keyboard_native_menu_and_local_gestures_request_identified_clipboard_reads() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .dispatch_native_menu_command(festerm_macos_window::NativeMenuCommand::Paste, &context);
        let token =
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap();
        keyboard_clipboard_reply(&context, token, "native-menu");
        harness.run();
        let grid = harness.get_by_label("Terminal viewport").rect();
        harness.event(egui::Event::PointerMoved(grid.center()));
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Middle,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Middle,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        let token =
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap();
        keyboard_clipboard_reply(&context, token, "middle-click");
        harness.run();
        harness.get_by_label("Terminal viewport").click_secondary();
        harness.run();
        harness.get_by_label("Paste").click();
        harness.run();
        let token =
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap();
        keyboard_clipboard_reply(&context, token, "context-menu");
        harness.run();
        assert_eq!(
            transport.sent().concat(),
            b"native-menumiddle-clickcontext-menu"
        );
    }

    #[test]
    fn keyboard_rename_owns_raw_and_semantic_clipboard_without_terminal_delivery() {
        let (mut harness, transport) = keyboard_harness();
        let chip = format!("{} chip", harness.state().chip_view_models().0[0].primary);
        harness.get_by_label(&chip).click_secondary();
        harness.run();
        harness.get_by_label("Rename session").click();
        harness.run();
        assert!(harness.state().rename_restore_tab.is_some());
        for native in [true, false] {
            harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            harness.run();
            if native {
                harness
                    .input_mut()
                    .events
                    .push(keyboard_event(egui::Key::V, egui::Modifiers::COMMAND));
            }
            harness
                .input_mut()
                .events
                .push(egui::Event::Paste("controlled-rename".into()));
            harness.step();
            assert_eq!(
                harness
                    .get_by_role(accesskit::Role::TextInput)
                    .value()
                    .as_deref(),
                Some("controlled-rename")
            );
            harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            harness.run();
            for (key, semantic) in [
                (egui::Key::C, egui::Event::Copy),
                (egui::Key::X, egui::Event::Cut),
            ] {
                if native {
                    harness
                        .input_mut()
                        .events
                        .push(keyboard_event(key, egui::Modifiers::COMMAND));
                }
                harness.input_mut().events.push(semantic);
                harness.step();
                assert!(harness
                    .output()
                    .platform_output
                    .commands
                    .iter()
                    .any(|command| matches!(command,
                    egui::OutputCommand::CopyText(text) if text == "controlled-rename")));
            }
            assert_eq!(
                harness
                    .get_by_role(accesskit::Role::TextInput)
                    .value()
                    .as_deref(),
                Some("")
            );
            assert!(transport.sent().is_empty());
        }
        harness
            .input_mut()
            .events
            .push(bound_event(festerm_config::KeyboardAction::SettingsHotkey));
        harness.step();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn keyboard_same_batch_switch_recomputes_terminal_scope_and_recording_target() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, first_transport) = keyboard_harness();
        let context = harness.ctx.clone();
        let first = harness.state().state.active();
        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(A::ClearTerminal, Some("Ctrl+Shift+K".into()));
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenSettings, &context);
        harness.run();
        harness.input_mut().events.extend([
            bound_event(A::Quick1),
            keyboard_event(egui::Key::K, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
        ]);
        harness.step();
        assert_eq!(harness.state().state.active(), first);
        assert!(harness.state().overlays.transient_notice.is_some());
        assert!(first_transport.sent().is_empty());
        harness.state_mut().overlays.transient_notice = None;
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenLauncher, &context);
        let second_transport = crate::session_controller::fake::FakeSshSession::new([]);
        let second = harness
            .state_mut()
            .state
            .replace_active_with_test_ssh_session(
                second_transport.clone(),
                "controlled",
                "second.example.test",
                22,
            );
        for tab in [first, second] {
            harness.state_mut().state.dispatch(
                AppCommand::SetInputRecording { tab, enabled: true },
                &context,
            );
        }
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ActivateTab(first), &context);
        harness.run();
        let second_index = harness
            .state()
            .state
            .tabs()
            .iter()
            .position(|tab| tab.id == second)
            .unwrap();
        harness.input_mut().events.extend([
            bound_event(crate::keyboard::QUICK_ACTIONS[second_index]),
            bound_event(A::Copy),
        ]);
        harness.step();
        assert_eq!(harness.state().state.active(), second);
        let report = |tab| {
            harness
                .state()
                .state
                .session_tab(tab)
                .unwrap()
                .controller
                .input_recorder
                .lock()
                .unwrap()
                .report()
        };
        assert!(!report(first).contains("class=Copy terminal selection"));
        assert!(report(second).contains("class=Copy terminal selection"));
        assert!(report(second).contains(&format!("target={} generation=1", second.chip_id())));
        assert!(first_transport.sent().is_empty() && second_transport.sent().is_empty());
    }

    #[test]
    fn keyboard_input_before_a_same_batch_switch_stays_with_original_session() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, transport) = keyboard_harness();
        harness.input_mut().events.extend([
            egui::Event::Text("controlled-before-switch".into()),
            bound_event(A::SettingsHotkey),
        ]);
        harness.run();
        assert_eq!(transport.sent().concat(), b"controlled-before-switch");
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
    }

    #[test]
    fn keyboard_ime_cancellation_by_tab_click_releases_shortcuts_and_recovery() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, transport) = keyboard_harness();
        let chip = format!("{} chip", harness.state().chip_view_models().0[0].primary);
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenSettings, &context);
        harness.run();
        harness.get_by_label("Search actions…").click();
        harness.run();
        harness.event(egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "controlled-preedit".into(),
            active_range_chars: None,
        }));
        harness.run();
        harness
            .input_mut()
            .events
            .push(bound_event(A::CommandPalette));
        harness.step();
        assert!(
            !harness.state().palette.is_open(),
            "live composition owns shortcuts"
        );
        harness.get_by_label(&chip).click();
        harness.run();
        harness
            .input_mut()
            .events
            .push(bound_event(A::SettingsHotkey));
        harness.step();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
        harness.get_by_label(&chip).click();
        harness.run();
        harness.key_press_modifiers(
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            egui::Key::F12,
        );
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn keyboard_capture_unbind_and_repeat_use_actual_application_dispatch() {
        use festerm_config::KeyboardAction as A;
        let (mut harness, transport) = keyboard_harness();
        let mut bindings = festerm_config::KeyboardBindings::default();
        bindings.set(A::ClearTerminal, Some("Ctrl+Shift+K".into()));
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings.clone()), &context);
        harness.key_down_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::K);
        harness.step();
        assert!(
            transport.sent().is_empty(),
            "captured command must not also encode ^K"
        );
        assert!(harness
            .state()
            .overlays
            .transient_notice
            .as_ref()
            .unwrap()
            .0
            .contains("cleared"));
        harness.state_mut().overlays.transient_notice = None;
        harness.event(egui::Event::Key {
            key: egui::Key::K,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        });
        harness.step();
        assert!(
            transport.sent().is_empty(),
            "suppressed repeats remain consumed"
        );
        assert!(harness.state().overlays.transient_notice.is_none());
        harness.key_up(egui::Key::K);
        harness.step();
        bindings.set(A::ClearTerminal, Some(String::new()));
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::SetKeyboardBindings(bindings), &context);
        harness.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::K);
        harness.run();
        assert_eq!(transport.sent().concat(), b"\x0b");
    }

    #[test]
    fn keyboard_native_menu_counterpart_is_not_dispatched_twice() {
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let context = egui::Context::default();
        let (modifiers, key) = crate::keyboard::chord(
            festerm_config::KeyboardAction::CommandPalette.default_chord(cfg!(target_os = "macos")),
        )
        .unwrap();
        let mut output = context.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    modifiers,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                }],
                ..Default::default()
            },
            |ui| {
                app.dispatch_native_menu_command(
                    festerm_macos_window::NativeMenuCommand::ToggleCommandPalette,
                    ui.ctx(),
                );
                app.handle_shortcuts(ui.ctx());
            },
        );
        output.textures_delta.clear();
        assert!(
            app.palette.is_open(),
            "native command plus key counterpart must toggle only once"
        );
        app.palette.close();
        app.overlays.about_open = true;
        app.dispatch_native_menu_command(
            festerm_macos_window::NativeMenuCommand::ToggleCommandPalette,
            &context,
        );
        assert!(
            !app.palette.is_open(),
            "blocked menu intent must not be replayed later"
        );
    }

    #[test]
    fn keyboard_native_clipboard_provenance_does_not_leak_paste_or_swallow_control_keys() {
        let (mut harness, transport) = keyboard_harness();
        for (key, semantic) in [
            (egui::Key::C, egui::Event::Copy),
            (egui::Key::X, egui::Event::Cut),
            (
                egui::Key::V,
                egui::Event::Paste("controlled-not-for-terminal".into()),
            ),
        ] {
            harness.input_mut().events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            });
            if !cfg!(target_os = "macos") {
                harness.input_mut().events.push(semantic);
            }
            harness.input_mut().events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            });
        }
        harness.run();
        assert_eq!(transport.sent().concat(), b"\x03\x18\x16");
        harness.event(egui::Event::Copy);
        harness.run();
        assert_eq!(
            transport.sent().concat(),
            b"\x03\x18\x16",
            "explicit Copy is never an interrupt"
        );
        harness.event(egui::Event::Ime(egui::ImeEvent::Commit("é".into())));
        harness.run();
        assert_eq!(transport.sent().concat(), b"\x03\x18\x16\xc3\xa9");
    }

    #[test]
    fn keyboard_recovery_and_widget_focus_block_terminal_delivery() {
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        harness.state_mut().open_terminal_search(&context);
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::C);
        harness.event(egui::Event::Text("controlled-query".into()));
        harness.run();
        assert!(transport.sent().is_empty());
        harness.key_press_modifiers(
            egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
            egui::Key::F12,
        );
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Settings
        ));
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn keyboard_inspector_recording_controls_do_not_send_widget_input_to_terminal() {
        let (app, _, transport) = FesTermApp::for_test_with_fake_ssh_session([]);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 1000.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::ToggleSessionInspector, &context);
        harness.run();
        harness.get_by_label("Diagnostics").click();
        harness.run();
        harness.get_by_label("Record input routing").click();
        harness.run();
        let tab = harness.state().state.active();
        assert!(harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .controller
            .input_recorder
            .lock()
            .unwrap()
            .recording());
        harness.get_by_label("Stop input recording").click();
        harness.run();
        assert!(!harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .controller
            .input_recorder
            .lock()
            .unwrap()
            .recording());
        harness.get_by_label("Record input routing").click();
        harness.run();
        harness.event(egui::Event::Text("fake-inspector-token".into()));
        harness.event(egui::Event::Paste("fake-inspector-paste".into()));
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(
            transport.sent().is_empty(),
            "foreground widget input must not reach the terminal"
        );
        harness.get_by_label("Clear input recording").click();
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(!harness.state().state.inspector_open());
        assert!(transport.sent().is_empty());
    }

    #[test]
    fn keyboard_copy_of_fake_auth_url_never_submits_or_cancels_waiting_prompt() {
        let (mut harness, transport) = keyboard_harness();
        let tab = harness.state().state.active();
        let context = harness.ctx.clone();
        harness.state_mut().state.dispatch(
            AppCommand::SetInputRecording { tab, enabled: true },
            &context,
        );
        harness
            .state_mut()
            .state
            .session_tab_mut(tab)
            .unwrap()
            .terminal
            .ingest(b"\x1b[2J\x1b[Hhttps://auth.example.invalid/fake\r\nWaiting for fake token: ");
        harness.step();
        let grid = harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .view
            .diagnostics()
            .grid_rect
            .unwrap();
        let start = grid.left_top() + egui::vec2(2.0, 2.0);
        let end = start + egui::vec2(295.0, 0.0);
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
        harness.step();
        assert!(harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .view
            .selection()
            .range()
            .is_some());
        let (modifiers, key) = crate::keyboard::chord(
            festerm_config::KeyboardAction::Copy.default_chord(cfg!(target_os = "macos")),
        )
        .unwrap();
        harness.input_mut().events.push(egui::Event::Key {
            key,
            modifiers,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
        });
        harness.input_mut().events.push(egui::Event::Copy);
        harness.input_mut().events.push(egui::Event::Copy); // Native/toolkit duplicate after selection clearing.
        harness.step();
        assert!(harness.output().platform_output.commands.iter().any(|command| matches!(
            command, egui::OutputCommand::CopyText(text) if text.starts_with("https://auth.example.invalid"))),
            "controlled fixture copy output: {:?}", harness.output().platform_output.commands);
        assert!(
            transport.sent().is_empty(),
            "copy must not emit token input, Enter, EOF, or interrupt"
        );
        harness.key_up(key);
        harness.step();
        harness.event(egui::Event::Copy);
        harness.step();
        assert!(
            transport.sent().is_empty(),
            "an empty-selection menu Copy is also inert"
        );
        harness.event(egui::Event::Text("fake-token-154".into()));
        harness.key_press(egui::Key::Enter);
        harness.step();
        assert_eq!(
            transport.sent().concat(),
            b"fake-token-154\r",
            "the prompt remains able to accept its token"
        );
        harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::C);
        harness.step();
        assert!(
            transport.sent().concat().ends_with(b"\x03"),
            "actual Control-C remains available"
        );
        let recorder = harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .controller
            .input_recorder
            .lock()
            .unwrap();
        let report = recorder.report();
        assert!(report.contains("selection-copied"));
        assert!(report.contains("control-interrupt"));
        assert!(!report.contains("fake-token-154"));
        assert!(!report.contains("auth.example.invalid"));
    }

    #[test]
    fn keyboard_settings_ui_save_reload_preserves_profiles_and_preferences() {
        let configuration = Configuration::parse("schema_version = 1\n[[profiles]]\nkind = 'local'\nid = 'controlled'\nexecutable = 'controlled-unused'\n[settings]\nstatus_bar_visible = false\nterminal_font = 'julia-mono'\n").unwrap();
        let directory = std::env::current_dir()
            .unwrap()
            .join(format!(".keyboard-save-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("config.toml");
        fs::write(&path, configuration.to_toml().unwrap()).unwrap();
        let mut app = FesTermApp::for_test_with_configuration(configuration.clone());
        app.configuration_reloader = ConfigurationReloader::from_path_for_test(path.clone());
        app.state
            .dispatch(AppCommand::OpenSettings, &egui::Context::default());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(752.0, 5200.0))
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        harness.get_by_label("Clear binding").click();
        harness.run();
        let loaded = Configuration::parse(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded.profiles(), configuration.profiles());
        assert!(!loaded.interface_settings().status_bar_visible());
        assert_eq!(
            loaded.interface_settings().terminal_font(),
            TerminalFontPreference::JuliaMono
        );
        assert_eq!(
            loaded.interface_settings().keyboard_bindings().effective(
                festerm_config::KeyboardAction::NewSession,
                cfg!(target_os = "macos")
            ),
            ""
        );
        harness.get_by_label("Restore default").click();
        harness.run();
        assert!(Configuration::parse(&fs::read_to_string(&path).unwrap())
            .unwrap()
            .interface_settings()
            .keyboard_bindings()
            .is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

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

    /// Closing an additional window (ADR 0033) ends only that window's own
    /// sessions, which is exactly what the "confirm before closing a live
    /// session" preference governs - so with the preference off the window
    /// closes without interrupting the user, the same way its tabs do.
    #[test]
    fn closing_a_secondary_window_honours_the_close_confirmation_preference() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.role = WindowRole::Secondary;
        app.state
            .dispatch(AppCommand::ToggleConfirmSessionClose, &context);

        app.evaluate_close_request(&context);

        assert!(app.overlays.pending_quit.is_none());
    }

    /// With the preference on, an additional window still confirms - but as
    /// a window close, not as quitting the application, which would misstate
    /// what the user is about to lose.
    #[test]
    fn closing_a_secondary_window_confirms_as_a_window_rather_than_a_quit() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.role = WindowRole::Secondary;

        app.evaluate_close_request(&context);

        assert!(app
            .overlays
            .pending_quit
            .is_some_and(|pending| pending.purpose == QuitConfirmationPurpose::CloseWindow));
    }

    /// The primary window's close quits the application and discards every
    /// window's work at once, which no per-session preference was asked
    /// about, so it keeps confirming regardless.
    #[test]
    fn closing_the_primary_window_confirms_even_with_the_preference_off() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.state
            .dispatch(AppCommand::ToggleConfirmSessionClose, &context);

        app.evaluate_close_request(&context);

        assert!(app
            .overlays
            .pending_quit
            .is_some_and(|pending| pending.purpose == QuitConfirmationPurpose::Quit));
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
    fn window_close_during_update_consent_preserves_live_sessions() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.updates = UpdateController::ready_to_install_for_test();
        app.request_update_install();

        app.evaluate_close_request(&context);
        let mut output = context.end_pass();

        assert!(app
            .overlays
            .pending_quit
            .is_some_and(|pending| { pending.purpose == QuitConfirmationPurpose::InstallUpdate }));
        assert!(output
            .viewport_output
            .values()
            .flat_map(|viewport| &viewport.commands)
            .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        output.textures_delta.clear();
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

    /// A directory with one Markdown file in it, opened in an editor tab, with
    /// the application ready to render.
    fn app_with_open_editor(
        context: &egui::Context,
        contents: &str,
    ) -> (FesTermApp, std::path::PathBuf, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "festerm-dirty-close-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("NOTES.md");
        std::fs::write(&path, contents).unwrap();

        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.state.dispatch(
            AppCommand::OpenTextEditor {
                path: path.clone(),
            },
            context,
        );
        (app, directory, path)
    }

    fn editor_harness(app: FesTermApp) -> Harness<'static, FesTermApp> {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 640.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness
    }

    /// Types into the editor body through real key events, which is the only
    /// way a test can be sure the path the user takes is the path that dirties
    /// the document.
    fn type_into_editor(harness: &mut Harness<'static, FesTermApp>, text: &str) {
        let body = harness.get_by_role(egui::accesskit::Role::MultilineTextInput);
        body.focus();
        body.type_text(text);
        harness.run();
    }

    /// The chip's accessible name, which is where a screen reader hears the
    /// state the shape is showing (ADR 0034 §8).
    fn chip_name(harness: &Harness<'static, FesTermApp>, file: &str) -> String {
        harness
            .query_all_by_label_contains(file)
            .filter_map(|node| node.accesskit_node().label())
            .find(|label| label.ends_with("chip"))
            .unwrap_or_else(|| panic!("no chip for {file}"))
    }

    #[test]
    fn the_chip_says_its_state_in_its_accessible_name() {
        let context = egui::Context::default();
        let (app, directory, _path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);

        assert_eq!(chip_name(&harness, "NOTES.md"), "NOTES.md, saved chip");

        type_into_editor(&mut harness, "typed");

        assert_eq!(
            chip_name(&harness, "NOTES.md"),
            "NOTES.md, unsaved chip",
            "the state has to be in the name, not only in the shape"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_file_changed_underneath_turns_the_chip_into_a_conflict() {
        let context = egui::Context::default();
        let (app, directory, path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");

        fs::write(&path, "somebody else\n").unwrap();
        let documents = harness.state().state.documents().clone();
        let id = documents.borrow().find_local(&path).expect("the open document");
        documents.borrow_mut().refresh(id);
        harness.run();

        assert_eq!(
            chip_name(&harness, "NOTES.md"),
            "NOTES.md, in conflict chip",
            "a conflict is its own state, not a louder kind of unsaved"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn closing_a_clean_editor_asks_nothing_and_forgets_the_document() {
        let context = egui::Context::default();
        let (mut app, directory, _path) = app_with_open_editor(&context, "alpha\n");
        let tab = app.state.active();

        app.request_close_tab(tab, &context);

        assert!(app.overlays.pending_document_close.is_none());
        assert!(
            app.state.documents().borrow().is_empty(),
            "the last view going is what forgets the document"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn closing_the_final_view_of_a_dirty_document_asks_first() {
        let context = egui::Context::default();
        let (app, directory, _path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");

        let tab = harness.state().state.active();
        harness.state_mut().request_close_tab(tab, &context);
        harness.run();

        let pending = harness
            .state()
            .overlays
            .pending_document_close
            .clone()
            .expect("the final view of a dirty document is asked about");
        assert_eq!(pending.title, "NOTES.md");
        assert!(
            pending.origin.ends_with("NOTES.md"),
            "the prompt names the fully qualified origin: {}",
            pending.origin
        );
        // Named in the prompt itself, not only in the state behind it.
        harness.get_by_label("Save changes to NOTES.md?");
        assert!(
            harness.query_all_by_label(pending.origin.as_str()).count() > 0,
            "the prompt shows where the file lives"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn closing_one_of_two_views_of_a_dirty_document_asks_nothing() {
        let context = egui::Context::default();
        let (app, directory, path) = app_with_open_editor(&context, "# alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");

        // A live Markdown preview is a second view of the same document.
        harness
            .state_mut()
            .state
            .dispatch(AppCommand::OpenTextDocumentInMarkdown, &context);
        harness.run();
        let editor_tab = harness
            .state()
            .state
            .tabs()
            .iter()
            .find(|tab| matches!(tab.content, TabContent::TextEditor(_)))
            .expect("the editor tab is still open")
            .id;

        harness.state_mut().request_close_tab(editor_tab, &context);
        harness.run();

        assert!(
            harness.state().overlays.pending_document_close.is_none(),
            "closing one of two views takes nothing away"
        );
        let documents = harness.state().state.documents().borrow();
        assert_eq!(documents.len(), 1, "the document outlives the view");
        let id = documents.find_local(&path).expect("still open");
        assert!(
            documents.get(id).unwrap().text().is_dirty(),
            "and it still holds what was typed"
        );
        drop(documents);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn the_dirty_close_prompt_focuses_save_and_escape_cancels() {
        let context = egui::Context::default();
        let (app, directory, path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");
        let tab = harness.state().state.active();
        harness.state_mut().request_close_tab(tab, &context);
        harness.run();

        // Save is the default action and is what the Return key reaches.
        assert!(
            harness
                .query_all_by_label("Save")
                .any(|save| save.is_focused()),
            "Save is focused, so Return saves rather than discarding"
        );

        harness.key_press(egui::Key::Escape);
        harness.step();

        assert!(harness.state().overlays.pending_document_close.is_none());
        assert!(
            harness
                .state()
                .state
                .tabs()
                .iter()
                .any(|open| open.id == tab),
            "cancelling leaves the tab open"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\n",
            "and writes nothing"
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn saving_from_the_dirty_close_prompt_writes_the_file_and_closes_the_tab() {
        let context = egui::Context::default();
        let (app, directory, path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");
        let tab = harness.state().state.active();
        harness.state_mut().request_close_tab(tab, &context);
        harness.run();

        // Return, because Save is the focused action.
        harness.key_press(egui::Key::Enter);
        harness.step();
        harness.run();

        assert!(harness.state().overlays.pending_document_close.is_none());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\ntyped");
        assert!(
            !harness
                .state()
                .state
                .tabs()
                .iter()
                .any(|open| open.id == tab),
            "the tab closes once the write succeeded"
        );
        assert!(harness.state().state.documents().borrow().is_empty());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn discarding_from_the_dirty_close_prompt_closes_without_writing() {
        let context = egui::Context::default();
        let (app, directory, path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");
        let tab = harness.state().state.active();
        harness.state_mut().request_close_tab(tab, &context);
        harness.run();

        harness.get_by_label("Discard changes").click();
        harness.step();
        harness.run();

        assert!(harness.state().overlays.pending_document_close.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "alpha\n",
            "discarding writes nothing"
        );
        assert!(!harness
            .state()
            .state
            .tabs()
            .iter()
            .any(|open| open.id == tab));
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn quitting_with_unsaved_text_asks_about_the_document_before_anything_else() {
        let context = egui::Context::default();
        let (app, directory, _path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");

        harness.state_mut().evaluate_close_request(&context);
        harness.run();

        let pending = harness
            .state()
            .overlays
            .pending_document_close
            .clone()
            .expect("a quit asks about unsaved text");
        assert_eq!(pending.title, "NOTES.md");
        assert_eq!(
            pending.then,
            crate::overlay_state::AfterDocumentClose::ResumeClose(QuitConfirmationPurpose::Quit),
            "answering it resumes the quit rather than ending there"
        );
        assert!(!harness.state().quit_confirmed);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_saved_document_is_no_longer_asked_about_when_quitting() {
        let context = egui::Context::default();
        let (app, directory, _path) = app_with_open_editor(&context, "alpha\n");
        let mut harness = editor_harness(app);
        type_into_editor(&mut harness, "typed");
        harness.get_by_label("Save").click();
        harness.run();

        harness.state_mut().evaluate_close_request(&context);
        harness.run();

        assert!(
            harness.state().overlays.pending_document_close.is_none(),
            "nothing is unsaved, so nothing is asked"
        );
        let _ = std::fs::remove_dir_all(&directory);
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
    fn disconnected_overlay_reconnects_the_same_tab_through_application_command() {
        let (app, tab, session) = FesTermApp::for_test_with_fake_ssh_session([
            festerm_session::SessionEvent::Output(b"retained content".to_vec()),
            festerm_session::SessionEvent::Lifecycle(
                festerm_session::SessionLifecycle::Disconnected(
                    festerm_session::SessionError::new(
                        festerm_session::SessionErrorKind::Output,
                        "test disconnect",
                    ),
                ),
            ),
        ]);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();
        harness.get_by_label("Open Diagnostics");
        let generation = harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .controller
            .lifecycle_generation();
        harness.get_by_label("Reconnect").click();
        harness.run();
        assert_eq!(harness.state().state.active(), tab);
        assert_eq!(
            session.operations(),
            vec![crate::session_controller::fake::FakeSshOperation::Reconnect]
        );
        assert_eq!(
            harness
                .state()
                .state
                .session_tab(tab)
                .unwrap()
                .controller
                .lifecycle_generation(),
            generation + 1
        );
        assert!(harness.query_by_label("Reconnect").is_none());
        assert!(harness.get_by_label("Terminal viewport").is_focused());
        assert!(harness
            .state()
            .state
            .session_tab(tab)
            .unwrap()
            .terminal
            .row_text(0)
            .expect("first terminal row")
            .contains("retained content"));
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
    fn command_palette_omits_more_actions_entries() {
        let context = egui::Context::default();
        let (app, _tab) = FesTermApp::for_test_with_live_session(&context);
        let items = app.palette_items();
        assert!(
            !items.iter().any(|item| matches!(
                item.label.as_str(),
                "About fesTerm" | "Show Session Inspector" | "Hide Session Inspector"
            )),
            "More actions entries must not be duplicated in the command palette"
        );
    }

    #[test]
    fn command_palette_omits_open_markdown_file() {
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let items = app.palette_items();
        assert!(!items.iter().any(|item| item.label == "Open Markdown File…"));
    }

    #[test]
    fn the_markdown_picker_chord_works_from_a_non_terminal_surface() {
        let mut harness = harness();
        harness.run();
        assert!(matches!(
            harness.state().state.active_tab().content,
            TabContent::Launcher
        ));

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        harness.run();

        assert!(
            harness.state().overlays.markdown_file_picker.is_some(),
            "the document-open chord should still work where no terminal owns it"
        );
    }

    /// On Windows/Linux the picker's chord is literally `^O`, which nano
    /// binds to "Write Out". Swallowing it would turn a save into a file
    /// dialog and lose the user's edits, so a focused terminal keeps it.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn a_focused_terminal_keeps_plain_control_o_for_the_program_inside_it() {
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(
            matches!(
                harness.state().state.active_tab().content,
                TabContent::Session(_)
            ),
            "this test needs a terminal session focused"
        );

        // How the platform actually reports a held Ctrl on Windows/Linux:
        // egui sets `command` alongside `ctrl`, and it is precisely that
        // overlap which makes `^O` collide with the app's `Cmd/Ctrl+O`
        // binding. Pressing `Modifiers::CTRL` alone would never match the
        // binding and so would pass no matter what this test is guarding.
        let control_o = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        harness.key_press_modifiers(control_o, egui::Key::O);
        harness.run();

        assert!(
            harness.state().overlays.markdown_file_picker.is_none(),
            "Ctrl+O was taken from the terminal, so nano's Write Out would be swallowed"
        );
    }

    /// The macOS chord is `Cmd+O`, which never reaches the terminal, so it
    /// stays available on every surface including a live session.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_macos_markdown_picker_chord_survives_a_focused_terminal() {
        let mut harness = harness();
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.step();

        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::O);
        harness.run();

        assert!(harness.state().overlays.markdown_file_picker.is_some());
    }

    #[test]
    fn more_actions_open_markdown_file_opens_the_local_file_picker() {
        let mut harness = harness();
        harness.run();

        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Open Markdown File…").click();
        harness.run();

        assert!(harness.state().overlays.markdown_file_picker.is_some());
        harness.get_by_label("Open Markdown File");
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
                replacing: None,
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

    /// A fresh picker starts at the user's home directory (not `/`), and a
    /// later one resumes where the previous one was left.
    #[test]
    fn the_markdown_file_picker_starts_at_home_and_then_resumes_the_last_directory() {
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());

        assert_eq!(
            app.markdown_file_picker_start_directory(),
            local_home_directory()
        );

        let remembered = std::env::temp_dir();
        app.overlays.markdown_file_picker_directory = Some(remembered.clone());
        assert_eq!(app.markdown_file_picker_start_directory(), remembered);

        // A directory that has since been deleted must not strand the picker
        // somewhere it cannot list.
        app.overlays.markdown_file_picker_directory =
            Some(remembered.join("festerm-directory-that-does-not-exist"));
        assert_eq!(
            app.markdown_file_picker_start_directory(),
            local_home_directory()
        );
    }

    /// `Ctrl+O` from inside a Markdown viewer retargets that viewer; from any
    /// other surface it opens a new tab.
    #[test]
    fn the_markdown_file_picker_targets_the_active_viewer_only_from_a_viewer() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());

        app.open_markdown_file_picker(&context);
        assert_eq!(app.overlays.markdown_file_picker_replaces, None);
        app.close_markdown_file_picker(&context);

        app.state.dispatch(
            AppCommand::OpenLocalMarkdownFile {
                path: std::path::PathBuf::from("/docs/readme.md"),
                replacing: None,
            },
            &context,
        );
        let viewer = app.state.active();

        app.open_markdown_file_picker(&context);
        assert_eq!(app.overlays.markdown_file_picker_replaces, Some(viewer));

        // Dismissing the picker must not leave the target armed for the next
        // (possibly unrelated) open.
        app.close_markdown_file_picker(&context);
        assert_eq!(app.overlays.markdown_file_picker_replaces, None);
    }

    #[test]
    fn the_command_palette_offers_new_window_and_only_requests_it() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let items = app.palette_items();
        assert!(items.iter().any(|item| item.label == "New Window"));

        let tabs_before = app.state.tabs().len();
        app.dispatch_palette_selection(23, &context);

        // Window creation belongs to the composition root, so the palette
        // must not have opened a tab or otherwise changed this window.
        assert_eq!(app.state.tabs().len(), tabs_before);
        assert!(app.take_window_open_request());
        assert!(!app.take_window_open_request(), "the request is one-shot");
    }

    /// macOS users reach New Window from the File menu, which travels a
    /// different path than the palette and must request a window rather than
    /// acting on this one.
    #[test]
    fn the_native_menu_new_window_command_requests_a_window() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());

        let tabs_before = app.state.tabs().len();
        app.dispatch_native_menu_command(
            festerm_macos_window::NativeMenuCommand::NewWindow,
            &context,
        );

        assert_eq!(app.state.tabs().len(), tabs_before);
        assert!(app.take_window_open_request());
    }

    /// The persisted workspace is still a single tab list (ADR 0032), so a
    /// second window must not overwrite it with its own tabs.
    #[test]
    fn a_secondary_window_does_not_persist_its_workspace() {
        let context = egui::Context::default();
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.configuration_reloader =
            crate::configuration_startup::ConfigurationReloader::from_path_for_test(
                std::path::PathBuf::from("/festerm-nonexistent/config.toml"),
            );
        app.role = WindowRole::Secondary;
        app.configuration_status = ConfigurationStartupStatus::Loaded;
        app.state.dispatch(AppCommand::OpenSettings, &context);

        app.save_workspace(Vec::new(), &mut 1);

        // A real save to that unwritable path would have recorded a failure
        // status and queued a broadcast; a skipped save records neither.
        assert!(app.take_configuration_broadcast().is_none());
        assert!(matches!(
            app.configuration_status,
            ConfigurationStartupStatus::Loaded
        ));
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
                "Ctrl+Shift+C"
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
                "Ctrl+Shift+V"
            })
        );
    }

    #[test]
    fn paste_palette_command_requests_an_os_clipboard_paste() {
        const PASTE: u64 = 14;
        let (mut harness, transport) = keyboard_harness();
        let context = harness.ctx.clone();
        harness
            .state_mut()
            .dispatch_palette_selection(PASTE, &context);
        let token =
            egui_winit::clipboard_requests::take_request(&context, egui::ViewportId::ROOT).unwrap();
        keyboard_clipboard_reply(&context, token, "controlled-palette");
        harness.run();
        assert_eq!(transport.sent().concat(), b"controlled-palette");
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
        let app = FesTermApp::for_test_with_configuration(Configuration::empty());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(360.0, 516.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("About fesTerm").click();
        harness.run();
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

    #[test]
    fn successful_update_install_requests_one_normal_application_close() {
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.updates = UpdateController::installed_for_test();
        app.update_restart_authorized = true;
        let context = egui::Context::default();
        let mut first_frame = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.ui_content(ui));
        });
        assert!(
            first_frame
                .viewport_output
                .values()
                .flat_map(|viewport| &viewport.commands)
                .all(|command| !matches!(command, egui::ViewportCommand::Close)),
            "font installation must not request an update exit"
        );
        first_frame.textures_delta.clear();

        let mut installed_frame = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.ui_content(ui));
        });
        let close_requests = installed_frame
            .viewport_output
            .values()
            .flat_map(|viewport| &viewport.commands)
            .filter(|command| matches!(command, egui::ViewportCommand::Close))
            .count();
        assert_eq!(close_requests, 1);
        assert!(app.update_exit_requested);
        assert!(app.quit_confirmed);
        installed_frame.textures_delta.clear();

        let mut next_frame = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.ui_content(ui));
        });
        let close_requests = next_frame
            .viewport_output
            .values()
            .flat_map(|viewport| &viewport.commands)
            .filter(|command| matches!(command, egui::ViewportCommand::Close))
            .count();
        assert_eq!(close_requests, 0, "the close request must be one-shot");
        next_frame.textures_delta.clear();
    }

    #[test]
    fn update_install_waits_for_live_session_restart_consent() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.updates = UpdateController::ready_to_install_for_test();

        app.request_update_install();

        let pending = app
            .overlays
            .pending_quit
            .expect("restart consent should be pending");
        assert_eq!(pending.purpose, QuitConfirmationPurpose::InstallUpdate);
        assert!(matches!(
            app.updates.status(),
            UpdateStatus::ReadyToInstall(_)
        ));
        assert!(!app.update_restart_authorized);
    }

    #[test]
    fn cancelling_update_restart_consent_keeps_the_update_installable() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.updates = UpdateController::ready_to_install_for_test();
        app.request_update_install();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.get_by_label("Cancel").click();
        harness.step();

        assert!(harness.state().overlays.pending_quit.is_none());
        assert!(!harness.state().update_restart_authorized);
        assert!(matches!(
            harness.state().updates.status(),
            UpdateStatus::ReadyToInstall(_)
        ));
    }

    #[test]
    fn confirmed_update_restart_installs_then_closes_without_reprompting() {
        let context = egui::Context::default();
        let (mut app, _tab) = FesTermApp::for_test_with_live_session(&context);
        app.updates = UpdateController::ready_to_install_for_test();
        app.request_update_install();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(420.0, 600.0))
            .with_max_steps(16)
            .build_ui_state(|ui, app: &mut FesTermApp| app.ui_content(ui), app);
        harness.run();

        harness.get_by_label("Install and Restart").click();
        harness.step();
        harness.step();

        assert!(harness.state().update_restart_authorized);
        assert!(harness.state().update_exit_requested);
        assert!(harness.state().quit_confirmed);
        assert!(harness.state().overlays.pending_quit.is_none());
    }

    #[test]
    fn failed_update_install_restores_ordinary_quit_protection() {
        let mut app = FesTermApp::for_test_with_configuration(Configuration::empty());
        app.updates = UpdateController::ready_to_fail_install_for_test();
        app.request_update_install();
        assert!(app.update_restart_authorized);

        let context = egui::Context::default();
        let mut first_frame = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.ui_content(ui));
        });
        first_frame.textures_delta.clear();
        let mut failed_frame = context.run_ui(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| app.ui_content(ui));
        });
        failed_frame.textures_delta.clear();

        assert!(matches!(app.updates.status(), UpdateStatus::Failed { .. }));
        assert!(!app.update_restart_authorized);
        assert!(!app.update_exit_requested);
        assert!(!app.quit_confirmed);
    }

    fn harness() -> Harness<'static, FesTermApp> {
        harness_with_configuration(Configuration::empty())
    }

    fn harness_with_configuration(configuration: Configuration) -> Harness<'static, FesTermApp> {
        Harness::builder()
            .with_size(egui::vec2(1240.0, 880.0))
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
            .get_by_label(
                "development — Local · festerm-inspector-test-command-that-does-not-exist",
            )
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
        let configuration = Configuration::new(vec![
            festerm_config::Profile::local("dev-shell", "/bin/zsh", Vec::new(), None)
                .expect("capture profile is valid"),
            festerm_config::Profile::ssh(
                "prod-web-01",
                "10.0.4.21",
                22,
                "deploy",
                "xterm-256color",
                100,
                40,
            )
            .expect("capture profile is valid"),
            festerm_config::Profile::sftp("build-artifacts", "artifacts.internal", 22, "ci", true)
                .expect("capture profile is valid"),
            festerm_config::Profile::serial(
                "usb-console",
                "/dev/tty.usbserial-1420",
                115_200,
                festerm_config::SerialDataBits::Eight,
                festerm_config::SerialParity::None,
                festerm_config::SerialStopBits::One,
                festerm_config::SerialFlowControl::None,
            )
            .expect("capture profile is valid"),
        ])
        .expect("capture configuration is valid");
        let mut snapshots = egui_kittest::SnapshotResults::new();
        for (compact, name) in [
            (false, "festerm-launcher-actual"),
            (true, "festerm-launcher-compact"),
        ] {
            let configuration = configuration
                .with_interface_settings(
                    festerm_config::InterfaceSettings::DEFAULT.with_compact_launcher_grid(compact),
                )
                .expect("capture interface settings are valid");
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1239.0, 877.0))
                .build_ui_state(
                    |ui, app: &mut FesTermApp| {
                        ui.ctx().set_visuals(theme::default_visuals());
                        app.ui_content(ui);
                    },
                    FesTermApp::for_test_with_configuration(configuration),
                );
            harness.run();
            harness.snapshot_options(name, &SnapshotOptions::default().output_path(&output_path));
            snapshots.extend(harness.take_snapshot_results());
        }
        snapshots.unwrap();
    }

    /// Produces the Session Inspector overlay with production widgets while
    /// keeping the capture independent of a live process or network service.
    #[test]
    #[ignore = "manual GUI mockup review capture"]
    fn capture_session_inspector_for_mockup_review() {
        let output_path = std::env::temp_dir().join("festerm-gui-review");
        let mut harness = failed_local_profile_harness();
        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Session inspector").click();
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

        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Session inspector").click();
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
        harness.get_by_label("More actions").click();
        harness.run();
        harness.get_by_label("Session inspector").click();
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

        app.save_workspace(Vec::new(), &mut 1);

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
        // `Harness::run` repaints until the UI goes quiet and panics after
        // four frames if it has not. This test owns a real local session, and
        // `ui_content` requests another repaint on every frame that received
        // shell output (see `last_pump_output_received`), so whether the UI
        // settles within four frames depends entirely on how quickly the
        // spawned shell finishes printing its banner - which made this test
        // fail intermittently on CI. Autosave is driven by the first frame
        // that observes `workspace_dirty`, so stepping a fixed number of
        // frames tests the same thing without racing the shell.
        harness.step();
        harness.step();

        // The write itself spans every window, so the frame only *asks* for
        // it and the composition root performs it (ADR 0033).
        let app = harness.state_mut();
        assert!(
            app.take_workspace_save_request(),
            "starting a session should request a workspace save with no manual Save action"
        );
        app.save_workspace(Vec::new(), &mut 1);

        assert_eq!(
            app.configuration_status,
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

        app.save_workspace(Vec::new(), &mut 1);

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
            .get_by_label("development — Local · festerm-profile-test-command-that-does-not-exist")
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
