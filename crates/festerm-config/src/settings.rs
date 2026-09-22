use serde::{Deserialize, Serialize};

use crate::{
    is_false, is_true, validate_stored_path_setting, ConfigError, ConfigErrorKind, KeyboardBindings,
};
use std::path::Path;

/// User-adjustable interface preferences that apply immediately in the UI and
/// are intended to be saved automatically as they change
/// (`docs/gui-design.md` "Wrapping must remain user-configurable"). Unlike
/// profiles and workspace metadata, there is deliberately no separate
/// explicit save step for this slice: Settings applies each change live and
/// the application persists the same replacement immediately afterward.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceSettings {
    #[serde(default, skip_serializing_if = "KeyboardBindings::is_empty")]
    keyboard_bindings: KeyboardBindings,
    #[serde(default)]
    chip_layout: ChipLayoutPreference,
    #[serde(default = "default_status_bar_visible")]
    status_bar_visible: bool,
    #[serde(default = "default_show_session_details")]
    show_session_details: bool,
    /// Whether closing a live session asks for confirmation before
    /// terminating or disconnecting it. On by default to preserve the safe
    /// close behavior; users who prefer one-click closing may opt out.
    #[serde(default = "default_confirm_session_close")]
    confirm_session_close: bool,
    /// Whether new default local sessions on Windows prefer the per-user
    /// `WindowsApps\pwsh.exe` app-execution alias over `%COMSPEC%`.
    #[serde(default = "default_prefer_powershell", skip_serializing_if = "is_true")]
    prefer_powershell: bool,
    /// Whether choosing Local Shell in New Session opens the executable,
    /// arguments, and working-directory form before launch. Off by default:
    /// the ordinary path starts the platform shell in the user's home
    /// directory immediately.
    #[serde(default, skip_serializing_if = "is_false")]
    customize_local_shell: bool,
    /// Whether the open-tab list and active tab persist across restarts.
    /// On by default: reopening where the last session left off is what a
    /// terminal that holds long-lived work is expected to do, and the
    /// alternative - discarding the tab list on every quit - is the more
    /// surprising of the two once a user keeps more than one session open.
    /// Turning it off is one click in Settings
    /// (`docs/gui-design.md` "Workspace restore").
    #[serde(default = "default_restore_workspace", skip_serializing_if = "is_true")]
    restore_workspace: bool,
    /// The bundled primary face used for terminal cells. Application chrome
    /// typography remains independent.
    #[serde(default, skip_serializing_if = "TerminalFontPreference::is_default")]
    terminal_font: TerminalFontPreference,
    /// Whether eligible adjacent terminal cells may be shaped as a run.
    /// On by default: the bundled faces are designed around their coding
    /// ligatures, and the renderer preserves cell ownership across them.
    #[serde(
        default = "default_terminal_ligatures",
        skip_serializing_if = "is_true"
    )]
    terminal_ligatures: bool,
    /// Whether eligible emoji use the bundled color raster path or the
    /// deterministic monochrome fallback chain.
    #[serde(
        default,
        skip_serializing_if = "EmojiPresentationPreference::is_default"
    )]
    emoji_presentation: EmojiPresentationPreference,
    /// How many scrollback rows a single trackpad/wheel scroll step moves,
    /// relative to fesTerm's original fixed pixel-to-row mapping.
    #[serde(default, skip_serializing_if = "ScrollSpeedPreference::is_default")]
    scroll_speed: ScrollSpeedPreference,
    /// Retained primary-history payload budget for newly created sessions.
    #[serde(default, skip_serializing_if = "ScrollbackLimitPreference::is_default")]
    scrollback_limit: ScrollbackLimitPreference,
    /// Whether holding the quick-switch modifier (Cmd on macOS, Ctrl
    /// elsewhere) temporarily overlays each eligible chip's quick-switch
    /// number in place of its usual status presentation (feature request
    /// #69). On by default: the numbers appear only while the modifier is
    /// held, so they cost nothing until the shortcut is already being used
    /// and they teach it to a user who does not know it.
    #[serde(
        default = "default_quick_switch_overlay",
        skip_serializing_if = "is_true"
    )]
    quick_switch_overlay: bool,
    /// Whether the Launcher's New Session list uses a responsive
    /// multi-column layout for saved profiles when the window is wide
    /// enough (feature request #64). On by default: the shorter launch cards
    /// let the saved-profile and running-session panels start higher, which
    /// is what a user opening New Session is usually reaching for.
    #[serde(
        default = "default_compact_launcher_grid",
        skip_serializing_if = "is_true"
    )]
    compact_launcher_grid: bool,
    /// Whether a background session tab's chip status dot slow-pulses when
    /// that session has emitted output since the tab was last active
    /// (feature request #68). On by default: a background session that has
    /// produced something is the one fact a tab strip cannot otherwise
    /// convey, and the pulse is slow enough not to compete for attention.
    #[serde(
        default = "default_pulse_new_output_dot",
        skip_serializing_if = "is_true"
    )]
    pulse_new_output_dot: bool,
    /// Whether the New Session/Launcher screen surfaces locally running,
    /// unattached `festerm-sessiond` persistence sessions as one-click
    /// "Resume" entries (feature request #70). On by default: a session that
    /// survived the last quit is exactly what a user opening New Session
    /// after a restart is looking for, and hiding it invites starting a
    /// second copy of work that is already running.
    #[serde(
        default = "default_show_resumable_sessions",
        skip_serializing_if = "is_true"
    )]
    show_resumable_sessions: bool,
    /// Whether the status bar names the durable session the active terminal
    /// is attached to, as `provider · session name` (feature request #168).
    /// Off by default: the status bar keeps its current fields exactly.
    /// Independent of `show_session_details`, which governs the chip's
    /// launch/title detail rather than durable-session identity.
    #[serde(default, skip_serializing_if = "is_false")]
    show_durable_session_in_status_bar: bool,
    /// Whether the GUI SFTP file manager shows the Local or Remote pane on
    /// the left. This is a global habit preference, not per-tab state.
    #[serde(default, skip_serializing_if = "SftpPaneOrderPreference::is_default")]
    sftp_pane_order: SftpPaneOrderPreference,
    /// The default starting local directory for new SFTP sessions. When
    /// present it must resolve to an existing local directory so SFTP tabs
    /// never start with a broken `lpwd` baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_sftp_local_directory: Option<String>,
    /// Whether fesTerm occasionally asks GitHub Releases whether a newer
    /// version exists and shows a quiet badge when one does. On by default so
    /// a user learns about a security fix without going looking for it;
    /// turning it off stops the poll itself, not merely the badge.
    #[serde(
        default = "default_automatic_update_checks",
        skip_serializing_if = "is_true"
    )]
    automatic_update_checks: bool,
    /// How a text editor view starts out. These are per-view settings while
    /// the view is open (ADR 0034 §9), but the way a reader likes to work does
    /// not change between one file and the next, so the last answer is the
    /// next view's starting point.
    #[serde(default, skip_serializing_if = "EditorSettings::is_default")]
    editor: EditorSettings,
}

impl InterfaceSettings {
    /// What a fresh installation starts with, and the target of an explicit
    /// Settings reset. Every one of these is reachable in one click from the
    /// Settings screen, so the bar for being a default is which answer suits
    /// more people rather than which is the more conservative.
    pub const DEFAULT: Self = Self {
        keyboard_bindings: KeyboardBindings(Vec::new()),
        chip_layout: ChipLayoutPreference::SingleRowScroll,
        status_bar_visible: true,
        show_session_details: false,
        confirm_session_close: false,
        prefer_powershell: true,
        customize_local_shell: false,
        restore_workspace: true,
        terminal_font: TerminalFontPreference::JetBrainsMono,
        terminal_ligatures: true,
        emoji_presentation: EmojiPresentationPreference::Color,
        scroll_speed: ScrollSpeedPreference::Normal,
        scrollback_limit: ScrollbackLimitPreference::MiB64,
        quick_switch_overlay: true,
        compact_launcher_grid: true,
        pulse_new_output_dot: true,
        show_resumable_sessions: true,
        show_durable_session_in_status_bar: false,
        sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
        default_sftp_local_directory: None,
        automatic_update_checks: true,
        editor: EditorSettings::DEFAULT,
    };

    pub fn new(
        chip_layout: ChipLayoutPreference,
        status_bar_visible: bool,
        show_session_details: bool,
        confirm_session_close: bool,
        restore_workspace: bool,
    ) -> Self {
        Self {
            chip_layout,
            status_bar_visible,
            show_session_details,
            confirm_session_close,
            restore_workspace,
            ..Self::DEFAULT
        }
    }

    pub const fn with_terminal_typography(
        mut self,
        terminal_font: TerminalFontPreference,
        terminal_ligatures: bool,
    ) -> Self {
        self.terminal_font = terminal_font;
        self.terminal_ligatures = terminal_ligatures;
        self
    }

    pub const fn with_prefer_powershell(mut self, prefer_powershell: bool) -> Self {
        self.prefer_powershell = prefer_powershell;
        self
    }

    pub const fn with_customize_local_shell(mut self, customize_local_shell: bool) -> Self {
        self.customize_local_shell = customize_local_shell;
        self
    }

    pub const fn with_emoji_presentation(
        mut self,
        emoji_presentation: EmojiPresentationPreference,
    ) -> Self {
        self.emoji_presentation = emoji_presentation;
        self
    }

    /// Sets the scrollback scroll-speed clickstop (feature request #67).
    pub const fn with_scroll_speed(mut self, scroll_speed: ScrollSpeedPreference) -> Self {
        self.scroll_speed = scroll_speed;
        self
    }

    /// Sets the retained primary-history budget for newly created sessions.
    pub const fn with_scrollback_limit(
        mut self,
        scrollback_limit: ScrollbackLimitPreference,
    ) -> Self {
        self.scrollback_limit = scrollback_limit;
        self
    }

    /// Sets whether holding the quick-switch modifier overlays chip numbers
    /// (feature request #69).
    pub const fn with_quick_switch_overlay(mut self, quick_switch_overlay: bool) -> Self {
        self.quick_switch_overlay = quick_switch_overlay;
        self
    }

    /// Sets whether the Launcher's New Session list uses a responsive
    /// multi-column layout for saved profiles (feature request #64).
    pub const fn with_compact_launcher_grid(mut self, compact_launcher_grid: bool) -> Self {
        self.compact_launcher_grid = compact_launcher_grid;
        self
    }

    /// Sets whether a background tab's chip status dot slow-pulses when new
    /// output has arrived since it was last active (feature request #68).
    pub const fn with_pulse_new_output_dot(mut self, pulse_new_output_dot: bool) -> Self {
        self.pulse_new_output_dot = pulse_new_output_dot;
        self
    }

    /// Sets whether the Launcher surfaces locally running, unattached
    /// `festerm-sessiond` sessions as one-click "Resume" entries (feature
    /// request #70).
    pub const fn with_show_resumable_sessions(mut self, show_resumable_sessions: bool) -> Self {
        self.show_resumable_sessions = show_resumable_sessions;
        self
    }

    /// Sets whether the status bar names the durable session the active
    /// terminal is attached to (feature request #168).
    pub const fn with_show_durable_session_in_status_bar(
        mut self,
        show_durable_session_in_status_bar: bool,
    ) -> Self {
        self.show_durable_session_in_status_bar = show_durable_session_in_status_bar;
        self
    }

    /// Sets the visual left/right order for the GUI SFTP panes.
    pub const fn with_sftp_pane_order(mut self, sftp_pane_order: SftpPaneOrderPreference) -> Self {
        self.sftp_pane_order = sftp_pane_order;
        self
    }

    /// Sets the default starting local directory for new SFTP sessions.
    pub fn with_default_sftp_local_directory(
        mut self,
        default_sftp_local_directory: Option<String>,
    ) -> Self {
        self.default_sftp_local_directory = default_sftp_local_directory;
        self
    }

    pub const fn chip_layout(&self) -> ChipLayoutPreference {
        self.chip_layout
    }

    pub const fn status_bar_visible(&self) -> bool {
        self.status_bar_visible
    }

    pub const fn show_session_details(&self) -> bool {
        self.show_session_details
    }

    pub const fn confirm_session_close(&self) -> bool {
        self.confirm_session_close
    }

    pub const fn prefer_powershell(&self) -> bool {
        self.prefer_powershell
    }

    pub const fn customize_local_shell(&self) -> bool {
        self.customize_local_shell
    }

    pub const fn restore_workspace(&self) -> bool {
        self.restore_workspace
    }

    pub const fn terminal_font(&self) -> TerminalFontPreference {
        self.terminal_font
    }

    pub const fn terminal_ligatures(&self) -> bool {
        self.terminal_ligatures
    }

    pub const fn emoji_presentation(&self) -> EmojiPresentationPreference {
        self.emoji_presentation
    }

    pub const fn scroll_speed(&self) -> ScrollSpeedPreference {
        self.scroll_speed
    }

    pub const fn scrollback_limit(&self) -> ScrollbackLimitPreference {
        self.scrollback_limit
    }

    pub const fn quick_switch_overlay(&self) -> bool {
        self.quick_switch_overlay
    }

    pub const fn compact_launcher_grid(&self) -> bool {
        self.compact_launcher_grid
    }

    pub const fn pulse_new_output_dot(&self) -> bool {
        self.pulse_new_output_dot
    }

    pub const fn show_resumable_sessions(&self) -> bool {
        self.show_resumable_sessions
    }

    pub const fn show_durable_session_in_status_bar(&self) -> bool {
        self.show_durable_session_in_status_bar
    }

    pub const fn automatic_update_checks(&self) -> bool {
        self.automatic_update_checks
    }

    pub const fn with_automatic_update_checks(mut self, automatic_update_checks: bool) -> Self {
        self.automatic_update_checks = automatic_update_checks;
        self
    }

    pub const fn sftp_pane_order(&self) -> SftpPaneOrderPreference {
        self.sftp_pane_order
    }

    pub fn default_sftp_local_directory(&self) -> Option<&Path> {
        self.default_sftp_local_directory.as_deref().map(Path::new)
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == Self::DEFAULT
    }

    /// Sets how a text editor view starts out.
    pub const fn with_editor(mut self, editor: EditorSettings) -> Self {
        self.editor = editor;
        self
    }

    pub const fn editor(&self) -> EditorSettings {
        self.editor
    }

    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.keyboard_bindings
            .validate(cfg!(target_os = "macos"))
            .map_err(|_| ConfigError::new(ConfigErrorKind::InvalidInterfaceSettings))?;
        if let Some(directory) = &self.default_sftp_local_directory {
            validate_stored_path_setting(directory)?;
        }
        Ok(())
    }

    pub fn keyboard_bindings(&self) -> &KeyboardBindings {
        &self.keyboard_bindings
    }

    pub fn with_keyboard_bindings(mut self, bindings: KeyboardBindings) -> Self {
        self.keyboard_bindings = bindings;
        self
    }
}

/// Bundled primary terminal families. The serialized names are stable
/// configuration values rather than platform font-discovery names.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalFontPreference {
    #[default]
    JetBrainsMono,
    IosevkaTerm,
    JuliaMono,
    MapleMono,
}

impl TerminalFontPreference {
    const fn is_default(&self) -> bool {
        matches!(self, Self::JetBrainsMono)
    }
}

/// Selects which repository-owned emoji presentation path terminal cells use.
///
/// Both choices preserve the same core-owned grapheme and cell geometry.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmojiPresentationPreference {
    #[default]
    Color,
    Monochrome,
}

impl EmojiPresentationPreference {
    const fn is_default(&self) -> bool {
        matches!(self, Self::Color)
    }
}

/// A discrete clickstop scaling how many scrollback rows one trackpad/wheel
/// scroll step moves, relative to fesTerm's original fixed pixel-to-row
/// mapping (`crates/festerm-ui-egui/src/view.rs`). Deliberately a small,
/// fixed set of named steps rather than a free-form numeric multiplier,
/// matching how the rest of Settings favors discrete, previewable choices
/// over open-ended numeric entry.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollSpeedPreference {
    VerySlow,
    Slow,
    #[default]
    Normal,
    Fast,
    VeryFast,
}

/// Global visual ordering preference for the GUI SFTP file manager panes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SftpPaneOrderPreference {
    #[default]
    LocalLeft,
    RemoteLeft,
}

impl SftpPaneOrderPreference {
    const fn is_default(&self) -> bool {
        matches!(self, Self::LocalLeft)
    }
}

impl ScrollSpeedPreference {
    /// All clickstops in slowest-to-fastest order, for rendering a slider.
    pub const ALL: [Self; 5] = [
        Self::VerySlow,
        Self::Slow,
        Self::Normal,
        Self::Fast,
        Self::VeryFast,
    ];

    /// A short, user-displayable name for this clickstop.
    pub const fn label(self) -> &'static str {
        match self {
            Self::VerySlow => "Very slow",
            Self::Slow => "Slow",
            Self::Normal => "Normal",
            Self::Fast => "Fast",
            Self::VeryFast => "Very fast",
        }
    }

    /// The multiplier applied to the rows a scroll step would otherwise
    /// move. `Normal` is `1.0` and preserves fesTerm's original behavior.
    pub const fn multiplier(self) -> f32 {
        match self {
            Self::VerySlow => 0.05,
            Self::Slow => 0.35,
            Self::Normal => 1.0,
            Self::Fast => 1.75,
            Self::VeryFast => 2.5,
        }
    }

    /// This clickstop's position in [`Self::ALL`], for driving a slider by
    /// index.
    pub const fn index(self) -> usize {
        match self {
            Self::VerySlow => 0,
            Self::Slow => 1,
            Self::Normal => 2,
            Self::Fast => 3,
            Self::VeryFast => 4,
        }
    }

    /// The clickstop at `index` in [`Self::ALL`], saturating to the nearest
    /// valid end rather than panicking on an out-of-range slider position.
    pub const fn from_index(index: usize) -> Self {
        match index {
            0 => Self::VerySlow,
            1 => Self::Slow,
            3 => Self::Fast,
            n if n >= 4 => Self::VeryFast,
            _ => Self::Normal,
        }
    }

    const fn is_default(&self) -> bool {
        matches!(self, Self::Normal)
    }
}

/// A bounded retained primary-history budget for newly created sessions.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScrollbackLimitPreference {
    Disabled,
    #[serde(rename = "16-mib")]
    MiB16,
    #[default]
    #[serde(rename = "64-mib")]
    MiB64,
    #[serde(rename = "256-mib")]
    MiB256,
}

impl ScrollbackLimitPreference {
    pub const ALL: [Self; 4] = [Self::Disabled, Self::MiB16, Self::MiB64, Self::MiB256];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::MiB16 => "16 MiB",
            Self::MiB64 => "64 MiB",
            Self::MiB256 => "256 MiB",
        }
    }

    pub const fn bytes(self) -> usize {
        match self {
            Self::Disabled => 0,
            Self::MiB16 => 16 * 1024 * 1024,
            Self::MiB64 => 64 * 1024 * 1024,
            Self::MiB256 => 256 * 1024 * 1024,
        }
    }

    const fn is_default(&self) -> bool {
        matches!(self, Self::MiB64)
    }
}

impl Default for InterfaceSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn default_status_bar_visible() -> bool {
    InterfaceSettings::DEFAULT.status_bar_visible
}

fn default_show_session_details() -> bool {
    InterfaceSettings::DEFAULT.show_session_details
}

fn default_automatic_update_checks() -> bool {
    InterfaceSettings::DEFAULT.automatic_update_checks
}

fn default_confirm_session_close() -> bool {
    InterfaceSettings::DEFAULT.confirm_session_close
}

fn default_restore_workspace() -> bool {
    InterfaceSettings::DEFAULT.restore_workspace
}

fn default_terminal_ligatures() -> bool {
    InterfaceSettings::DEFAULT.terminal_ligatures
}

fn default_quick_switch_overlay() -> bool {
    InterfaceSettings::DEFAULT.quick_switch_overlay
}

fn default_compact_launcher_grid() -> bool {
    InterfaceSettings::DEFAULT.compact_launcher_grid
}

fn default_pulse_new_output_dot() -> bool {
    InterfaceSettings::DEFAULT.pulse_new_output_dot
}

fn default_show_resumable_sessions() -> bool {
    InterfaceSettings::DEFAULT.show_resumable_sessions
}

fn default_prefer_powershell() -> bool {
    InterfaceSettings::DEFAULT.prefer_powershell
}

/// The persisted chip-wrapping preference
/// (`docs/gui-design.md` "Tab overflow and wrapping"). This mirrors
/// `festerm_ui_egui::chrome::ChipLayout` without introducing a UI-crate
/// dependency into this configuration crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChipLayoutPreference {
    /// Many chips wrap onto additional rows.
    Wrap,
    /// Chips stay on a single row; overflow scrolls horizontally instead of
    /// wrapping.
    #[default]
    SingleRowScroll,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_interface_settings_document_deserializes_to_default() {
        let settings: InterfaceSettings = toml::from_str("").unwrap();

        assert_eq!(settings, InterfaceSettings::DEFAULT);
    }

    #[test]
    fn scroll_speed_clickstop_index_mapping_is_stable_and_saturating() {
        for (index, expected) in ScrollSpeedPreference::ALL.into_iter().enumerate() {
            assert_eq!(expected.index(), index);
            assert_eq!(ScrollSpeedPreference::from_index(index), expected);
        }
        // Out-of-range indices saturate to the fastest clickstop rather than
        // panicking, since a slider position should never be able to select
        // a nonexistent step.
        assert_eq!(
            ScrollSpeedPreference::from_index(99),
            ScrollSpeedPreference::VeryFast
        );
    }

    #[test]
    fn editor_settings_round_trip_and_read_a_stored_zero_as_a_fluid_width() {
        let settings = InterfaceSettings::DEFAULT.with_editor(EditorSettings::new(
            false,
            Some(80),
            true,
            true,
            false,
        ));
        let written = toml::to_string(&settings).unwrap();
        let read: InterfaceSettings = toml::from_str(&written).unwrap();

        assert_eq!(read.editor(), settings.editor());
        assert!(
            !read.editor().syntax(),
            "highlighting turned off stays off across a restart (ADR 0035 §7)"
        );
        assert!(
            written.contains("[editor]"),
            "the block has to be written where it can be read back: {written}"
        );

        let zero: InterfaceSettings = toml::from_str("[editor]\nfixed_columns = 0\n").unwrap();
        assert_eq!(
            zero.editor().fixed_columns(),
            None,
            "a settings file is not a place to argue with the reader"
        );
    }

    #[test]
    fn syntax_highlighting_is_on_unless_the_file_says_otherwise() {
        let quiet: InterfaceSettings = toml::from_str("[editor]\nvi_keys = true\n").unwrap();
        assert!(
            quiet.editor().syntax(),
            "a file written before the option existed still gets colour"
        );

        let off: InterfaceSettings = toml::from_str("[editor]\nsyntax = false\n").unwrap();
        assert!(!off.editor().syntax());
    }

    #[test]
    fn a_default_editor_block_is_not_written_at_all() {
        let written = toml::to_string(&InterfaceSettings::DEFAULT).unwrap();

        assert!(
            !written.contains("[editor]"),
            "settings nobody changed do not belong in the file: {written}"
        );
    }

    #[test]
    fn scrollback_limit_clickstops_have_stable_labels_and_byte_values() {
        assert_eq!(
            ScrollbackLimitPreference::ALL.map(ScrollbackLimitPreference::label),
            ["Disabled", "16 MiB", "64 MiB", "256 MiB"]
        );
        assert_eq!(
            ScrollbackLimitPreference::ALL.map(ScrollbackLimitPreference::bytes),
            [0, 16 * 1024 * 1024, 64 * 1024 * 1024, 256 * 1024 * 1024]
        );
    }
}

/// How a text editor view starts out: the presentation options the reader last
/// chose (`docs/text-editor-design.md` "Per-view options").
///
/// A fixed column count of `None` is a fluid width. Zero is not a column
/// count, so a stored zero is read as no fixed width at all rather than
/// refused: a settings file is not a place to argue with the reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorSettings {
    #[serde(default = "default_line_numbers", skip_serializing_if = "is_true")]
    line_numbers: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fixed_columns: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    vi_keys: bool,
    /// The outline pane, on by default: a document large enough to need an
    /// editor is usually large enough to need navigating, and the pane
    /// collapses in one click when it is not wanted.
    #[serde(default = "default_outline", skip_serializing_if = "is_true")]
    outline: bool,
    /// Syntax highlighting, on by default: an editor that has grammars and
    /// does not use them is surprising in a way the reverse is not
    /// (ADR 0035 §7).
    #[serde(default = "default_syntax", skip_serializing_if = "is_true")]
    syntax: bool,
}

const fn default_syntax() -> bool {
    true
}

const fn default_line_numbers() -> bool {
    true
}

const fn default_outline() -> bool {
    EditorSettings::DEFAULT.outline
}

impl EditorSettings {
    pub const DEFAULT: Self = Self {
        line_numbers: true,
        fixed_columns: None,
        vi_keys: false,
        outline: true,
        syntax: true,
    };

    pub const fn new(
        line_numbers: bool,
        fixed_columns: Option<u32>,
        vi_keys: bool,
        outline: bool,
        syntax: bool,
    ) -> Self {
        Self {
            line_numbers,
            fixed_columns,
            vi_keys,
            outline,
            syntax,
        }
    }

    #[must_use]
    pub const fn with_line_numbers(mut self, line_numbers: bool) -> Self {
        self.line_numbers = line_numbers;
        self
    }

    #[must_use]
    pub const fn with_vi_keys(mut self, vi_keys: bool) -> Self {
        self.vi_keys = vi_keys;
        self
    }

    #[must_use]
    pub const fn with_outline(mut self, outline: bool) -> Self {
        self.outline = outline;
        self
    }

    #[must_use]
    pub const fn with_syntax(mut self, syntax: bool) -> Self {
        self.syntax = syntax;
        self
    }

    pub const fn line_numbers(&self) -> bool {
        self.line_numbers
    }

    /// The stored column count, with zero read as a fluid width.
    pub const fn fixed_columns(&self) -> Option<u32> {
        match self.fixed_columns {
            Some(0) | None => None,
            Some(columns) => Some(columns),
        }
    }

    pub const fn vi_keys(&self) -> bool {
        self.vi_keys
    }

    pub const fn outline(&self) -> bool {
        self.outline
    }

    pub const fn syntax(&self) -> bool {
        self.syntax
    }

    const fn is_default(&self) -> bool {
        self.line_numbers == Self::DEFAULT.line_numbers
            && self.fixed_columns.is_none()
            && self.vi_keys == Self::DEFAULT.vi_keys
            && self.outline == Self::DEFAULT.outline
            && self.syntax == Self::DEFAULT.syntax
    }
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self::DEFAULT
    }
}
