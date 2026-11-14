//! Settings application-surface presentation, split out of `screens.rs`.

use std::{collections::BTreeSet, path::PathBuf};

use eframe::egui::{self, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{
    EmojiPresentationPreference, ImageMemoryBudgetPreference, ScrollSpeedPreference,
    ScrollbackLimitPreference, SftpPaneOrderPreference, TerminalFontPreference,
};
use festerm_ui_egui::{
    chrome::ChipLayout,
    controls::{action_button, ActionButtonRole},
    icon::{self, Icon},
    theme,
};

use super::{
    ssh_back_button, ssh_paragraph, ssh_section_heading, AppCommand, CONTENT_SCROLLBAR_LANE,
};

/// Renders the Settings application surface.
///
/// `chip_layout`, `status_bar_visible`, and `show_session_details` reflect
/// the current interface preferences (`docs/gui-design.md` "Wrapping must
/// remain user-configurable"). Unlike profiles/workspace metadata, these
/// preferences are saved automatically by the composition root as soon as
/// they change; there is no separate explicit save step for them. Returns
/// commands for Settings actions; the application composition root owns
/// configuration I/O and applies successful replacements to `AppState`.
#[derive(Clone)]
pub(crate) struct SettingsViewModel {
    #[cfg(feature = "local-host-settings")]
    pub local_remote_access: crate::app::desktop_remote_access::View,
    #[cfg(feature = "local-host-settings")]
    pub local_share_targets: Vec<crate::app::desktop_remote_access::ShareTarget>,
    pub keyboard_bindings: festerm_config::KeyboardBindings,
    pub chip_layout: ChipLayout,
    pub status_bar_visible: bool,
    pub show_session_details: bool,
    pub show_pane_handles: bool,
    pub pane_high_contrast: bool,
    pub confirm_session_close: bool,
    pub prefer_powershell: bool,
    pub extend_dev_tunnel_lifetime: bool,
    pub show_running_dev_tunnels: bool,
    pub dev_tunnel_extension_days: u32,
    /// Public account metadata and observations from explicit account operations.
    #[cfg(feature = "experimental-dev-tunnels")]
    pub dev_tunnel_accounts: Vec<(
        String,
        String,
        usize,
        crate::app::tunnel_browser::AccountStatus,
    )>,
    #[cfg(not(feature = "experimental-dev-tunnels"))]
    pub dev_tunnel_accounts: Vec<(String, String, usize)>,
    pub customize_local_shell: bool,
    pub restore_workspace: bool,
    pub terminal_font: TerminalFontPreference,
    pub font_download_status: Option<String>,
    pub font_download_failed: bool,
    pub terminal_ligatures: bool,
    pub emoji_presentation: EmojiPresentationPreference,
    pub scroll_speed: ScrollSpeedPreference,
    pub rectangle_copy_trim_trailing_spaces: bool,
    pub scrollback_limit: ScrollbackLimitPreference,
    pub image_memory_budget: ImageMemoryBudgetPreference,
    pub image_memory_over_budget: bool,
    pub quick_switch_overlay: bool,
    pub compact_launcher_grid: bool,
    pub show_resumable_sessions: bool,
    pub show_durable_session_in_status_bar: bool,
    pub automatic_update_checks: bool,
    pub mcp_enabled: bool,
    pub javascript_enabled: bool,
    pub automation_mcp_enabled: bool,
    pub automation_javascript_enabled: bool,
    pub automation_mcp_ui_revision: u64,
    pub automation_javascript_ui_revision: u64,
    pub automation_preference_feedback: Option<&'static str>,
    pub automation_preference_retry_available: bool,
    pub automation_window: crate::application::WindowId,
    pub automation: crate::automation::SettingsSnapshot,
    pub default_sftp_local_directory: Option<String>,
    pub sftp_pane_order: SftpPaneOrderPreference,
    pub session_recording_enabled: bool,
    pub recording_preference_unsaved: bool,
}

#[derive(Clone, Default)]
struct SettingsState {
    default_sftp_local_directory: String,
    sftp_pane_order: Option<SftpPaneOrderPreference>,
    synced_value: Option<String>,
    feedback: Option<String>,
}

fn settings_sftp_directory_field_id(ui: &Ui) -> egui::Id {
    ui.make_persistent_id("settings_default_sftp_local_directory")
}

/// The drill-in pages Settings hosts inside its own surface. Each keeps the
/// Settings singleton identity; Back (or Escape) returns to the main page and
/// to the row that opened the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum SettingsPage {
    #[default]
    Main,
    KeyboardShortcuts,
    #[cfg(feature = "local-host-settings")]
    RemoteAccess,
    AutomationActivity,
}

impl SettingsPage {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Main => "Settings",
            Self::KeyboardShortcuts => "Keyboard shortcuts",
            #[cfg(feature = "local-host-settings")]
            Self::RemoteAccess => "Remote access",
            Self::AutomationActivity => "Automation activity",
        }
    }
}

/// Per-window Settings navigation, kept in egui temp memory like the other
/// Settings view state.
#[derive(Clone, Copy, Default)]
struct SettingsNavigation {
    page: SettingsPage,
    /// The drill-in row to refocus once the main page is drawn again.
    return_to: Option<SettingsPage>,
    /// Focus the sub-page's Back control on its first frame.
    focus_back: bool,
    /// Whether a text field had focus when the last frame ended. egui drops
    /// focus on Escape before any widget runs, so the field that Escape
    /// should leave has to be remembered from the previous frame.
    text_focused: bool,
}

/// Main-page section headings, top to bottom. Drill-in sections are rows
/// rather than headed cards and are named by their row title.
#[cfg(test)]
const SETTINGS_SECTION_ORDER: &[&str] = &[
    "Terminal",
    "Startup & sessions",
    "Window & tabs",
    "Keyboard shortcuts",
    "Files & documents",
    #[cfg(feature = "experimental-dev-tunnels")]
    "Dev Tunnels",
    #[cfg(feature = "local-host-settings")]
    "Remote access",
    "Recording",
    "Automation (Beta)",
    "Updates & privacy",
    "Reset",
];

/// The vertical gap between two Settings cards; `settings_card` owns it so
/// no call site can forget or double it.
const SETTINGS_CARD_GAP: f32 = 12.0;
/// The space either side of the rule between two rows inside a card.
const SETTINGS_ROW_GAP: f32 = 10.0;

fn settings_navigation_id(ui: &Ui) -> egui::Id {
    ui.id()
        .with(("settings_navigation", ui.ctx().viewport_id()))
}

fn settings_drill_in_id(navigation_id: egui::Id, page: SettingsPage) -> egui::Id {
    navigation_id.with(("drill_in", page))
}

fn settings_row_separator(ui: &mut Ui) {
    ui.add_space(SETTINGS_ROW_GAP);
    ui.separator();
    ui.add_space(SETTINGS_ROW_GAP);
}

/// A secondary title inside a merged card ("Font", "Panes", …).
fn settings_group_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .strong()
            .color(theme::TEXT_SECONDARY),
    );
    ui.add_space(4.0);
}

pub(crate) fn show_settings(ui: &mut Ui, settings: SettingsViewModel) -> Option<AppCommand> {
    let SettingsViewModel {
        #[cfg(feature = "local-host-settings")]
        local_remote_access,
        #[cfg(feature = "local-host-settings")]
        local_share_targets,
        keyboard_bindings,
        chip_layout,
        status_bar_visible,
        show_session_details,
        show_pane_handles,
        pane_high_contrast,
        confirm_session_close,
        prefer_powershell,
        extend_dev_tunnel_lifetime,
        show_running_dev_tunnels,
        dev_tunnel_extension_days,
        dev_tunnel_accounts,
        customize_local_shell,
        restore_workspace,
        terminal_font,
        font_download_status,
        font_download_failed,
        terminal_ligatures,
        emoji_presentation,
        scroll_speed,
        rectangle_copy_trim_trailing_spaces,
        scrollback_limit,
        image_memory_budget,
        image_memory_over_budget,
        quick_switch_overlay,
        compact_launcher_grid,
        show_resumable_sessions,
        show_durable_session_in_status_bar,
        automatic_update_checks,
        mcp_enabled,
        javascript_enabled,
        automation_mcp_enabled,
        automation_javascript_enabled,
        automation_mcp_ui_revision,
        automation_javascript_ui_revision,
        automation_preference_feedback,
        automation_preference_retry_available,
        automation_window,
        automation,
        default_sftp_local_directory,
        sftp_pane_order,
        session_recording_enabled,
        recording_preference_unsaved,
    } = settings;
    #[cfg(not(windows))]
    let _ = prefer_powershell;
    #[cfg(not(feature = "experimental-dev-tunnels"))]
    let _ = (
        extend_dev_tunnel_lifetime,
        show_running_dev_tunnels,
        dev_tunnel_extension_days,
        &dev_tunnel_accounts,
    );
    let state_id = ui.id().with("settings_state");
    let field_id = settings_sftp_directory_field_id(ui);
    let mut state = ui.data(|data| data.get_temp::<SettingsState>(state_id).unwrap_or_default());
    let model_value = default_sftp_local_directory.unwrap_or_default();
    let field_focused = ui.memory(|memory| memory.has_focus(field_id));
    if state.synced_value.as_deref() != Some(model_value.as_str()) && !field_focused {
        state.default_sftp_local_directory = model_value.clone();
        state.synced_value = Some(model_value);
        state.feedback = None;
    }
    state.sftp_pane_order.get_or_insert(sftp_pane_order);

    let navigation_id = settings_navigation_id(ui);
    let mut navigation = ui.data(|data| {
        data.get_temp::<SettingsNavigation>(navigation_id)
            .unwrap_or_default()
    });
    // The fixed Ctrl+Shift+F12 recovery chord asks the keyboard editor to
    // focus its search; the editor lives on its own sub-page, so go there.
    // The editor itself consumes the request.
    if ui.data(|data| {
        data.get_temp::<bool>(egui::Id::new("keyboard-settings-recovery"))
            .unwrap_or(false)
    }) {
        navigation.page = SettingsPage::KeyboardShortcuts;
        navigation.return_to = None;
        navigation.focus_back = false;
    }
    // Escape steps back out of a sub-page, but only once nothing nearer owns
    // it: an open dropdown, a focused text field, or a chord capture in the
    // keyboard editor (which treats Escape as "cancel capture").
    if navigation.page != SettingsPage::Main {
        let ctx = ui.ctx().clone();
        if !egui::Popup::is_any_open(&ctx)
            && !navigation.text_focused
            && !ctx.text_edit_focused()
            && !crate::keyboard::capture_owns_keys(&ctx)
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            navigation.return_to = Some(navigation.page);
            navigation.page = SettingsPage::Main;
        }
    }
    let page = navigation.page;
    let mut open_page = None;
    let mut go_back = false;

    let mut command = None;
    let mut automation_command = SettingsAutomationCommands::default();
    ui.horizontal(|ui| {
        ui.add_space(26.0);
        // Bound Settings' own height to whatever room is actually left
        // above the status bar (queried from its persisted panel state,
        // the same technique the SSH profile editor panel uses): the
        // card-based layout is taller than the old flat button list, and
        // without this it can paint straight into - or past - the status
        // bar instead of stopping short of it.
        let panel_top = ui.cursor().top();
        let mut viewport_bottom = ui.ctx().content_rect().bottom();
        if let Some(status_bar) =
            egui::containers::panel::PanelState::load(ui.ctx(), egui::Id::new("status_bar"))
        {
            viewport_bottom = viewport_bottom.min(status_bar.outer_rect.top());
        }
        let available_height = (viewport_bottom - panel_top).max(0.0);
        // `ScrollArea` computes its own available space via
        // `ui.available_rect_before_wrap()`. Handing it a `ui` whose
        // `max_rect` isn't already a real, bounded rect (as is the case
        // here, directly inside a `ui.horizontal`) leads to a degenerate
        // sizing pass that -- besides being wrong for layout -- also
        // breaks click routing for widgets painted via `egui::Frame`
        // inside the scroll area. Giving the scroll area its own child
        // `Ui` with an explicit, non-degenerate `max_rect` (the same
        // technique the SSH profile editor uses) avoids both problems.
        let scroll_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), available_height),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(scroll_rect), |ui| {
            // egui's default floating scroll style reveals the bar for
            // *any* hover inside the scroll area's content, not just when
            // the pointer is actually near the bar - unlike the terminal
            // view's own history scrollbar, which stays hidden until it's
            // scrolled away from rest or the pointer is right over it.
            // Zeroing the "active" (any-content-hover) opacities while
            // keeping "interact" (hovering/dragging the bar itself) at
            // full strength reproduces that same narrower reveal condition
            // here.
            let mut scroll_style = egui::style::ScrollStyle::floating();
            scroll_style.active_handle_opacity = 0.0;
            scroll_style.active_background_opacity = 0.0;
            ui.spacing_mut().scroll = scroll_style;
            // One scroll position per page, so returning from a sub-page
            // lands where the user left the main page.
            egui::ScrollArea::vertical()
                .id_salt(("settings_scroll", page))
                .max_height(available_height)
                .show(ui, |ui| {
                    // The scroll bar itself belongs to this scroll *frame*,
                    // not to Settings' own content: it is given its own
                    // reserved lane on the right, by keeping the cards
                    // narrower than the frame instead of shrinking the
                    // frame itself. That way the (invisible until needed)
                    // scroll bar never has to sit on top of the cards' own
                    // right edge.
                    ui.set_max_width((ui.available_width() - CONTENT_SCROLLBAR_LANE).max(0.0));
                    ui.vertical(|ui| {
                        ui.add_space(24.0);
                        if page != SettingsPage::Main {
                            ui.horizontal(|ui| {
                                let back = ssh_back_button(ui);
                                if navigation.focus_back {
                                    back.request_focus();
                                    navigation.focus_back = false;
                                }
                                go_back = back.clicked();
                                ui.add_space(8.0);
                                ui.heading(page.title());
                            });
                            ui.add_space(SETTINGS_CARD_GAP);
                        }
                        match page {
                            SettingsPage::Main => {
                        ui.heading("Settings");
                        ui.add_space(2.0);

                        settings_card(ui, "Terminal", |ui| {
                            settings_group_label(ui, "Font");
                            let mut selected_font = terminal_font;
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width(ui.available_width() - 190.0);
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("Terminal font")
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        ssh_paragraph(
                                            ui,
                                            "JetBrains Mono is included. Other families download automatically \
                                             when selected, including after an upgrade restores your preference. \
                                             Verified fonts work offline. GitHub receives the font asset request, \
                                             your IP address and fesTerm version, never terminal content. \
                                             Application text and update-download consent are unchanged.",
                                        );
                                    });
                                },
                                |ui| {
                                    egui::ComboBox::from_id_salt("terminal-font-family")
                                        .selected_text(terminal_font_label(selected_font))
                                        .width(160.0)
                                        .show_ui(ui, |ui| {
                                            for font in [
                                                TerminalFontPreference::JetBrainsMono,
                                                TerminalFontPreference::IosevkaTerm,
                                                TerminalFontPreference::JuliaMono,
                                                TerminalFontPreference::MapleMono,
                                            ] {
                                                ui.selectable_value(
                                                    &mut selected_font,
                                                    font,
                                                    terminal_font_label(font),
                                                );
                                            }
                                        });
                                },
                            );
                            if selected_font != terminal_font {
                                command = Some(AppCommand::SetTerminalFont(selected_font));
                            }
                            if let Some(status) = &font_download_status {
                                ssh_paragraph(ui, status);
                            }
                            if font_download_failed
                                && action_button(ui, ActionButtonRole::Secondary, "Retry font download").clicked()
                            {
                                command = Some(AppCommand::RetryTerminalFontDownload);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Programming ligatures",
                                "Shape eligible adjacent cells together while preserving cursor, \
                                 selection, and terminal grid ownership.",
                                terminal_ligatures,
                            ) {
                                command = Some(AppCommand::ToggleTerminalLigatures);
                            }
                            settings_row_separator(ui);
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Emoji presentation",
                                "Use bundled color artwork or deterministic monochrome fallback. \
                                 Terminal cell geometry is unchanged.",
                                &[
                                    (
                                        "Color",
                                        emoji_presentation == EmojiPresentationPreference::Color,
                                    ),
                                    (
                                        "Monochrome",
                                        emoji_presentation
                                            == EmojiPresentationPreference::Monochrome,
                                    ),
                                ],
                            ) {
                                command =
                                    Some(AppCommand::SetEmojiPresentation(if selected == 0 {
                                        EmojiPresentationPreference::Color
                                    } else {
                                        EmojiPresentationPreference::Monochrome
                                    }));
                            }
                            ui.add_space(SETTINGS_CARD_GAP);
                            settings_group_label(ui, "History & scrolling");
                            if let Some(selected) = settings_segmented_row(
                                ui,
                                "Scrollback limit",
                                "Maximum retained history for newly created sessions. \
                                 Existing sessions keep their current limit.",
                                &ScrollbackLimitPreference::ALL
                                    .map(|limit| (limit.label(), limit == scrollback_limit)),
                            ) {
                                command = Some(AppCommand::SetScrollbackLimit(
                                    ScrollbackLimitPreference::ALL[selected],
                                ));
                            }
                            settings_row_separator(ui);
                            let mut selected_speed = scroll_speed;
                            if let Some(new_speed) = settings_clickstop_row(
                                ui,
                                "Scroll speed",
                                "How far one trackpad or mouse wheel scroll step moves \
                                 through scrollback history.",
                                &ScrollSpeedPreference::ALL,
                                selected_speed,
                            ) {
                                selected_speed = new_speed;
                            }
                            if selected_speed != scroll_speed {
                                command = Some(AppCommand::SetScrollSpeed(selected_speed));
                            }
                            ui.add_space(SETTINGS_CARD_GAP);
                            settings_group_label(ui, "Mouse & selection");
                            if settings_toggle_row(
                                ui,
                                "Trim trailing spaces in rectangle copy",
                                "Bounded rectangle copy freezes the visible terminal while output \
                                 continues. Choose two corners or drag a rectangle, then Enter \
                                 copies and resumes, or right-click opens Copy and path/link \
                                 actions; Escape cancels. Turn this off to preserve the \
                                 selected width, including trailing spaces on every row.",
                                rectangle_copy_trim_trailing_spaces,
                            ) {
                                command = Some(AppCommand::ToggleRectangleCopyTrimTrailingSpaces);
                            }
                            ui.add_space(SETTINGS_ROW_GAP);
                            ui.label(
                                egui::RichText::new("Shift+right-click opens the fesTerm menu")
                                    .size(12.0)
                                    .color(theme::TEXT_SECONDARY),
                            );
                            ssh_paragraph(
                                ui,
                                "Use Shift+right-click for fesTerm's Copy/Paste menu even \
                                 when a terminal program handles mouse input. Plain right-click \
                                 stays with that program. Shift+drag selects terminal text. \
                                 These overrides work in local and SSH sessions.",
                            );
                        });

                        settings_card(ui, "Startup & sessions", |ui| {
                            if settings_toggle_row(
                                ui,
                                "Workspace restore",
                                "Reopen your previously open tabs and the active tab \
                                 automatically on launch. On by default.",
                                restore_workspace,
                            ) {
                                command = Some(AppCommand::ToggleRestoreWorkspace);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Confirm before closing live sessions",
                                "Ask before terminating a running local process or \
                                 disconnecting an active remote session.",
                                confirm_session_close,
                            ) {
                                command = Some(AppCommand::ToggleConfirmSessionClose);
                            }
                            ui.add_space(SETTINGS_CARD_GAP);
                            settings_group_label(ui, "Local shell");
                            #[cfg(windows)]
                            {
                                if settings_toggle_row(
                                    ui,
                                    "Prefer PowerShell when available",
                                    "Use pwsh.exe (the Windows app-execution alias, PATH or \
                                     Program Files) when it exists. Turn this off to use \
                                     COMSPEC for new default local sessions.",
                                    prefer_powershell,
                                ) {
                                    command = Some(AppCommand::TogglePreferPowershell);
                                }
                                settings_row_separator(ui);
                            }
                            if settings_toggle_row(
                                ui,
                                "Customize local shell before launch",
                                "Show executable, arguments, and working-directory fields, \
                                 prefilled with the defaults, after choosing Local Shell. \
                                 When off, start the default shell in your home directory \
                                 immediately. Off by default.",
                                customize_local_shell,
                            ) {
                                command = Some(AppCommand::ToggleCustomizeLocalShell);
                            }
                            ui.add_space(SETTINGS_CARD_GAP);
                            settings_group_label(ui, "New Session tab");
                            if settings_toggle_row(
                                ui,
                                "Compact New Session layout",
                                "Shrink the New Session tab's launch cards so the \
                                 saved-profile and running-session panels start higher \
                                 up the window. Card descriptions are kept. On by \
                                 default.",
                                compact_launcher_grid,
                            ) {
                                command = Some(AppCommand::ToggleCompactLauncherGrid);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Resume unattached local sessions from New Session",
                                "Surface locally running festerm-sessiond persistence \
                                 sessions that have no attached window, plus locally \
                                 running tmux and GNU screen sessions, as one-click \
                                 \"Resume\" entries in their own labeled widgets on the \
                                 New Session tab. On by default.",
                                show_resumable_sessions,
                            ) {
                                command = Some(AppCommand::ToggleShowResumableSessions);
                            }
                        });

                        settings_card(ui, "Window & tabs", |ui| {
                            if settings_segmented_row(
                                ui,
                                "Session chip layout",
                                "Keep terminal height stable with one scrolling row.",
                                &[
                                    ("Single row", !matches!(chip_layout, ChipLayout::Wrap)),
                                    ("Wrap", matches!(chip_layout, ChipLayout::Wrap)),
                                ],
                            )
                            .is_some()
                            {
                                command = Some(AppCommand::ToggleChipLayout);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Show session details in chips",
                                "Show the terminal title or launch context beneath the \
                                 session name. Off makes every chip compact and single-line, \
                                 moving the active session's detail to the status bar.",
                                show_session_details,
                            ) {
                                command = Some(AppCommand::ToggleShowSessionDetails);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Show quick-switch numbers",
                                "While the quick-switch modifier is held, briefly overlay each \
                                 eligible chip's number (1-9) in place of its usual status \
                                 presentation.",
                                quick_switch_overlay,
                            ) {
                                command = Some(AppCommand::ToggleQuickSwitchOverlay);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Show status bar",
                                "Display sourced session state, terminal dimensions, and \
                                 the active session detail when compact chips require it.",
                                status_bar_visible,
                            ) {
                                command = Some(AppCommand::ToggleStatusBar);
                            }
                            ui.add_space(SETTINGS_ROW_GAP);
                            ui.indent("durable_session_name", |ui| {
                                if settings_toggle_row_enabled(
                                    ui,
                                    "Show durable session name in status bar",
                                    if status_bar_visible {
                                        "Name the tmux, GNU screen, or fesTerm session daemon \
                                         session the active terminal is attached to, as \
                                         \"provider · session name\". This is the durable \
                                         session's stable identity, which the terminal-provided \
                                         title cannot supply; it is separate from \"Show session \
                                         details in chips\" and unaffected by it. Ordinary \
                                         sessions show nothing. Off by default."
                                    } else {
                                        "Requires the status bar. Turn on Show status bar to \
                                         name the durable tmux, GNU screen, or fesTerm session \
                                         daemon session the active terminal is attached to."
                                    },
                                    show_durable_session_in_status_bar,
                                    status_bar_visible,
                                ) {
                                    command = Some(AppCommand::ToggleDurableSessionInStatusBar);
                                }
                            });
                            ui.add_space(SETTINGS_CARD_GAP);
                            settings_group_label(ui, "Panes");
                            if settings_toggle_row(
                                ui,
                                "Show pane handles on hover",
                                "Reveal a compact handle near each pane's top edge. \
                                 Rearrange panes always shows handles, even when this is off.",
                                show_pane_handles,
                            ) {
                                command = Some(AppCommand::TogglePaneHandles);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "High-contrast pane focus",
                                "Use a stronger outline around the focused pane.",
                                pane_high_contrast,
                            ) {
                                command = Some(AppCommand::TogglePaneHighContrast);
                            }
                        });

                        settings_drill_in_card(
                            ui,
                            settings_drill_in_id(navigation_id, SettingsPage::KeyboardShortcuts),
                            SettingsPage::KeyboardShortcuts,
                            &keyboard_shortcuts_summary(&keyboard_bindings),
                            &mut navigation,
                            &mut open_page,
                        );

                        settings_card(ui, "Files & documents", |ui| {
                            let mut selected_order =
                                *state.sftp_pane_order.get_or_insert(sftp_pane_order);
                            let order_labels = [
                                (SftpPaneOrderPreference::LocalLeft, "Local left · Remote right"),
                                (SftpPaneOrderPreference::RemoteLeft, "Remote left · Local right"),
                            ];
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width(
                                        (ui.available_width() - 190.0)
                                            .max(SETTINGS_MIN_DESCRIPTION_WIDTH),
                                    );
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new("SFTP pane order")
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        ssh_paragraph(
                                            ui,
                                            "Visual order for the GUI SFTP file manager. Commands and accessibility labels still refer to Local and Remote, never Left and Right.",
                                        );
                                    });
                                },
                                |ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(180.0, ui.spacing().interact_size.y * 2.0),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            for (value, label) in order_labels {
                                                if ui
                                                    .radio_value(&mut selected_order, value, label)
                                                    .changed()
                                                {
                                                    command =
                                                        Some(AppCommand::SetSftpPaneOrder(value));
                                                }
                                            }
                                        },
                                    );
                                },
                            );
                            state.sftp_pane_order = Some(selected_order);
                            settings_row_separator(ui);
                            let label_id = std::cell::Cell::new(None);
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width(
                                        (ui.available_width() - 190.0)
                                            .max(SETTINGS_MIN_DESCRIPTION_WIDTH),
                                    );
                                    ui.vertical(|ui| {
                                        let label = ui.label(
                                            egui::RichText::new("Default local SFTP directory")
                                                .color(theme::TEXT_PRIMARY),
                                        );
                                        label_id.set(Some(label.id));
                                        ssh_paragraph(
                                            ui,
                                            "Starting local directory for new SFTP tabs. \
                                             Changing it updates only future sessions; `lcd` \
                                             affects the live session only.",
                                        );
                                    });
                                },
                                |ui| {
                                    let response = ui.add(
                                        TextEdit::singleline(
                                            &mut state.default_sftp_local_directory,
                                        )
                                        .id(field_id)
                                        .hint_text("Path to local directory")
                                        .desired_width(180.0),
                                    );
                                    let response = response
                                        .labelled_by(
                                        label_id.get().expect("label should be rendered"),
                                    );
                                    if response.changed() {
                                        let trimmed = state.default_sftp_local_directory.trim();
                                        if trimmed.is_empty() {
                                            state.feedback = None;
                                            state.synced_value = Some(String::new());
                                            command =
                                                Some(AppCommand::SetDefaultSftpLocalDirectory(
                                                    None,
                                                ));
                                        } else if trimmed.chars().any(char::is_control) {
                                            state.feedback = Some(
                                                "Default local SFTP directory must not contain control characters."
                                                    .to_owned(),
                                            );
                                        } else {
                                            state.feedback = None;
                                            state.synced_value = Some(trimmed.to_owned());
                                            command = Some(
                                                AppCommand::SetDefaultSftpLocalDirectory(Some(
                                                    PathBuf::from(trimmed),
                                                )),
                                            );
                                        }
                                    }
                                },
                            );
                            if let Some(feedback) = &state.feedback {
                                ui.add_space(6.0);
                                ui.colored_label(theme::STATUS_ERROR, feedback);
                            }
                            settings_row_separator(ui);
                            let mut selected = image_memory_budget;
                            egui::Sides::new().show(
                                ui,
                                |ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(SETTINGS_MIN_DESCRIPTION_WIDTH));
                                    ui.vertical(|ui| {
                                        ui.label(egui::RichText::new("Markdown image memory budget").color(theme::TEXT_PRIMARY));
                                        ssh_paragraph(ui, "Shared image storage and load reservations across all windows. \
                                            Lowering the limit keeps existing images but blocks new loads \
                                            until space is available. Decoder-private and renderer memory are not capped.");
                                    });
                                },
                                |ui| {
                                    egui::ComboBox::from_id_salt("image-memory-budget")
                                        .selected_text(selected.label())
                                        .width(160.0)
                                        .show_ui(ui, |ui| {
                                            for budget in ImageMemoryBudgetPreference::ALL {
                                                ui.selectable_value(&mut selected, budget, budget.label());
                                            }
                                        })
                                        .response
                                        .widget_info(|| WidgetInfo::labeled(WidgetType::ComboBox, ui.is_enabled(), "Image memory budget"));
                                },
                            );
                            if selected != image_memory_budget {
                                command = Some(AppCommand::SetImageMemoryBudget(selected));
                            }
                            if image_memory_over_budget {
                                ui.add_space(6.0);
                                ssh_paragraph(ui, "Current image reservations exceed this limit. \
                                    Existing images are kept; new image loads are refused until space is released.");
                            }
                        });

                        #[cfg(feature = "experimental-dev-tunnels")]
                        settings_card(ui, "Dev Tunnels", |ui| {
                            settings_group_label(ui, "GitHub accounts");
                            ssh_paragraph(ui, "Shared by Dev Tunnel connections.");
                            if dev_tunnel_accounts.is_empty() {
                                ssh_paragraph(ui, "No accounts yet.");
                            }
                            for (key, login, used_by, status) in &dev_tunnel_accounts {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(format!(
                                        "@{login} · used by {used_by} profile{}",
                                        if *used_by == 1 { "" } else { "s" }
                                    ));
                                    if status.needs_sign_in() {
                                        ui.colored_label(theme::STATUS_ERROR, status.label());
                                    } else {
                                        ui.weak(status.label());
                                    }
                                    if action_button(ui, ActionButtonRole::Secondary, "Sign in again").clicked() {
                                        command = Some(AppCommand::ReauthenticateDevTunnelAccount {
                                            account: key.clone(),
                                        });
                                    }
                                    if action_button(ui, ActionButtonRole::Secondary, &format!("Sign out @{login}")).clicked() {
                                        command = Some(AppCommand::RemoveDevTunnelAccount {
                                            account: key.clone(),
                                        });
                                    }
                                });
                            }
                            let sign_in_label = if dev_tunnel_accounts.is_empty() {
                                "Sign in with GitHub"
                            } else {
                                "Add account"
                            };
                            if action_button(ui, ActionButtonRole::Secondary, sign_in_label).clicked() {
                                command = Some(AppCommand::AddDevTunnelAccount);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Show my running tunnels in New Session",
                                "List your account's running fesTerm bridge tunnels that \
                                 have no saved profile, so you can connect without \
                                 setting one up first. Asks the Dev Tunnels service when \
                                 New Session opens, at most once a minute. Off by default.",
                                show_running_dev_tunnels,
                            ) {
                                command = Some(AppCommand::ToggleShowRunningDevTunnels);
                            }
                            settings_row_separator(ui);
                            if settings_toggle_row(
                                ui,
                                "Extend tunnel lifetime on sign-in",
                                "When you sign in to or connect a Dev Tunnel you own, \
                                 ask the service to push its expiry forward. A failed \
                                 extension never blocks the connection. Off by default.",
                                extend_dev_tunnel_lifetime,
                            ) {
                                command = Some(AppCommand::ToggleExtendDevTunnelLifetime);
                            }
                            if extend_dev_tunnel_lifetime {
                                ui.add_space(SETTINGS_ROW_GAP);
                                ui.indent("dev_tunnel_extension_length", |ui| {
                                    const DAYS: [u32; 6] = [7, 14, 30, 60, 90, 180];
                                    let labels = DAYS.map(|days| format!("{days} days"));
                                    let options: Vec<(&str, bool)> = DAYS
                                        .iter()
                                        .zip(&labels)
                                        .map(|(days, label)| {
                                            (label.as_str(), *days == dev_tunnel_extension_days)
                                        })
                                        .collect();
                                    if let Some(selected) = settings_segmented_row(
                                        ui,
                                        "Extension length",
                                        "How far from now the tunnel's expiry is moved each \
                                         time. Other values (1-365 days) can be set in the \
                                         configuration file.",
                                        &options,
                                    ) {
                                        command = Some(AppCommand::SetDevTunnelExtensionDays(
                                            DAYS[selected],
                                        ));
                                    }
                                });
                            }
                        });

                        #[cfg(feature = "local-host-settings")]
                        settings_drill_in_card(
                            ui,
                            settings_drill_in_id(navigation_id, SettingsPage::RemoteAccess),
                            SettingsPage::RemoteAccess,
                            &remote_access_summary(&local_remote_access),
                            &mut navigation,
                            &mut open_page,
                        );

                        settings_card(ui, "Recording", |ui| {
                            if settings_toggle_row(
                                ui,
                                "Allow session recording",
                                "Off by default. Turning this off stops an active recording; existing recordings remain available in the library.",
                                session_recording_enabled,
                            ) {
                                command = Some(AppCommand::SetSessionRecordingAllowed(
                                    !session_recording_enabled,
                                ));
                            }
                            if recording_preference_unsaved {
                                ui.add_space(SETTINGS_ROW_GAP);
                                ssh_paragraph(ui, "Recording is off for this run, but the saved preference is unconfirmed and may permit recording after restart. Retry saving, or reconcile the configuration if saves are blocked.");
                                if action_button(ui, ActionButtonRole::Secondary, "Retry saving recording opt-out").clicked() {
                                    command = Some(AppCommand::SetSessionRecordingAllowed(false));
                                }
                            }
                            if session_recording_enabled {
                                settings_row_separator(ui);
                                if settings_drill_in_row(
                                    ui,
                                    ui.id().with("recording_settings"),
                                    "Recording settings",
                                    Some("Capture, naming, and playback defaults."),
                                )
                                .clicked()
                                {
                                    command = Some(AppCommand::OpenRecordingSettings);
                                }
                            }
                            settings_row_separator(ui);
                            if settings_drill_in_row(
                                ui,
                                ui.id().with("recording_library"),
                                "Recording library",
                                Some("Play, export, or delete saved recordings."),
                            )
                            .clicked()
                            {
                                command = Some(AppCommand::OpenRecordingLibrary);
                            }
                        });

                        settings_card(ui, "Automation (Beta)", |ui| {
                            if let Some(feedback) = automation_preference_feedback {
                                ssh_paragraph(ui, feedback);
                                if automation_preference_retry_available
                                    && action_button(ui, ActionButtonRole::Secondary, "Retry saving automation preferences (stay stopped)").clicked()
                                {
                                    automation_command.record(AppCommand::RetryAutomationSettings);
                                }
                                ui.add_space(SETTINGS_ROW_GAP);
                            }
                            let viewport = ui.ctx().viewport_id();
                            if ui.push_id(("mcp_preference", viewport, mcp_enabled, automation_mcp_ui_revision), |ui| {
                                settings_toggle_row(
                                    ui,
                                    "Enable local MCP (Beta)",
                                    "Allow a local assistant adapter to request access. Each connection \
                                     still needs a separate native grant for exact terminals. Terminal \
                                     output can contain secrets. Off by default.",
                                    mcp_enabled,
                                )
                            }).inner {
                                automation_command.record(AppCommand::ToggleMcpEnabled);
                            }
                            ssh_paragraph(ui, "Beta: any process running as your user can connect, so always match the verification code before approving. Access stops when the screen locks or the computer sleeps, but an action already in flight may still complete. Enabling grants no terminal access by itself.");
                            settings_row_separator(ui);
                            if ui.push_id(("javascript_preference", viewport, javascript_enabled, automation_javascript_ui_revision), |ui| {
                                settings_toggle_row(
                                    ui,
                                    "Enable JavaScript workflows (Beta)",
                                    "Run a selected workflow in an isolated, resource-limited worker \
                                     after you approve it for one local terminal. Every input it sends \
                                     needs a separate approval. Off by default.",
                                    javascript_enabled,
                                )
                            }).inner {
                                automation_command.record(AppCommand::ToggleJavascriptEnabled);
                            }
                            settings_row_separator(ui);
                            ssh_paragraph(ui, automation.status);
                            ui.add_space(4.0);
                            let summary = automation_activity_summary(&automation);
                            let response = settings_drill_in_row(
                                ui,
                                settings_drill_in_id(navigation_id, SettingsPage::AutomationActivity),
                                SettingsPage::AutomationActivity.title(),
                                Some(&summary),
                            );
                            settings_drill_in_focus(ui, &response, SettingsPage::AutomationActivity, &mut navigation);
                            if response.clicked() {
                                open_page = Some(SettingsPage::AutomationActivity);
                            }
                        });

                        settings_card(ui, "Updates & privacy", |ui| {
                            if settings_toggle_row(
                                ui,
                                "Check for fesTerm updates automatically",
                                "Contact GitHub about once a day to see whether a newer \
                                 fesTerm has been released, and mark the \"More actions\" \
                                 control when one has. Nothing is downloaded or installed \
                                 without your say-so, and no profile, session, terminal, \
                                 device, or configuration data is sent. On by default; turn \
                                 it off to make fesTerm check only when you ask it to from \
                                 About fesTerm.",
                                automatic_update_checks,
                            ) {
                                command = Some(AppCommand::ToggleAutomaticUpdateChecks);
                            }
                        });

                        settings_card(ui, "Reset", |ui| {
                            ssh_paragraph(ui, RESET_ALL_SETTINGS_SUMMARY);
                            ui.add_space(SETTINGS_ROW_GAP);
                            if action_button(
                                ui,
                                ActionButtonRole::DangerOutline,
                                RESET_ALL_SETTINGS_LABEL,
                            )
                            .clicked()
                            {
                                automation_command.record(AppCommand::ResetAllSettings);
                            }
                        });
                            }
                            SettingsPage::KeyboardShortcuts => {
                                settings_panel(ui, |ui| {
                                    if let Some(action) =
                                        crate::keyboard::show_editor(ui, &keyboard_bindings)
                                    {
                                        command = Some(action);
                                    }
                                });
                            }
                            #[cfg(feature = "local-host-settings")]
                            SettingsPage::RemoteAccess => {
                                settings_panel(ui, |ui| {
                                    if let Some(action) = crate::app::desktop_remote_access::show(
                                        ui,
                                        &local_remote_access,
                                        &local_share_targets,
                                    ) {
                                        command = Some(match action {
                                            crate::app::desktop_remote_access::Action::SignInAccount => AppCommand::AddDevTunnelAccount,
                                            action => AppCommand::LocalRemoteAccess(action),
                                        });
                                    }
                                });
                            }
                            SettingsPage::AutomationActivity => {
                                settings_card(ui, "JavaScript workflows", |ui| {
                                    if let Some(action) =
                                        workflow_settings(ui, &automation, automation_javascript_enabled)
                                    {
                                        if matches!(&action, AppCommand::OpenWorkflowPicker) {
                                            command = Some(AppCommand::OpenWorkflowPicker);
                                        }
                                        automation_command.record(action);
                                    }
                                });
                                settings_card(ui, "Local MCP", |ui| {
                                    if let Some(action) = automation_settings(ui, &automation, automation_mcp_enabled, automation_window) {
                                        automation_command.record(AppCommand::Automation(action));
                                    }
                                });
                            }
                        }
                    });
                });
        });
    });
    if go_back {
        navigation.return_to = Some(page);
        navigation.page = SettingsPage::Main;
    } else if let Some(next) = open_page {
        navigation.page = next;
        navigation.return_to = None;
        navigation.focus_back = true;
    }
    if navigation.page != page {
        ui.ctx().request_repaint();
    }
    navigation.text_focused = ui.ctx().text_edit_focused();
    ui.data_mut(|data| {
        data.insert_temp(state_id, state);
        data.insert_temp(navigation_id, navigation);
    });
    automation_command.resolve(command)
}

/// The label shared by every surface that invokes `AppCommand::ResetAllSettings`.
pub(crate) const RESET_ALL_SETTINGS_LABEL: &str = "Reset all settings…";
/// What the main-page Reset card says before anyone opens the confirmation.
const RESET_ALL_SETTINGS_SUMMARY: &str = "Return the preferences on this page, including \
     keyboard shortcuts, to their defaults. You confirm first, with the full list.";
/// Every Settings group `AppCommand::ResetAllSettings` returns to its
/// defaults; the confirmation dialog lists exactly these.
pub(crate) const RESET_ALL_SETTINGS_CATEGORIES: &[&str] = &[
    "Terminal: font, ligatures, emoji, scrollback, scroll speed, and selection",
    "Startup & sessions: workspace restore, close confirmation, local shell, and New Session",
    "Window & tabs: chips, status bar, quick-switch numbers, and panes",
    "Keyboard shortcuts: every customized binding",
    "Files & documents: SFTP pane order and folder, editor, and image memory",
    "Dev Tunnels options",
    "Automation: Local MCP and JavaScript workflows turn off",
    "Updates & privacy",
];
/// What `AppCommand::ResetAllSettings` deliberately leaves alone.
pub(crate) const RESET_ALL_SETTINGS_NOT_AFFECTED: &str = "Recording settings, signed-in Dev \
     Tunnels accounts, remote access, saved profiles, and stored credentials are not affected.";

fn keyboard_shortcuts_summary(bindings: &festerm_config::KeyboardBindings) -> String {
    let customized = crate::keyboard::customized_count(bindings);
    let customized = match customized {
        0 => "All defaults".to_owned(),
        1 => "1 customized".to_owned(),
        count => format!("{count} customized"),
    };
    format!("{customized} · Ctrl+Shift+F12 always reopens Settings")
}

#[cfg(feature = "local-host-settings")]
fn remote_access_summary(view: &crate::app::desktop_remote_access::View) -> String {
    let state = match view.access_status() {
        None => "Not observed",
        Some(false) => "Off",
        Some(true) => "On",
    };
    let mut summary = format!("Let remote clients reach this computer · {state}");
    if view.review.is_some() || view.device_code.is_some() || view.share_review.is_some() {
        summary.push_str(" · review waiting");
    } else if view.busy {
        summary.push_str(" · working");
    }
    if view.failure.is_some() {
        summary.push_str(" · needs attention");
    }
    summary
}

fn automation_activity_summary(snapshot: &crate::automation::SettingsSnapshot) -> String {
    let mut parts = Vec::new();
    let connections = snapshot.channels.len();
    if connections > 0 {
        parts.push(format!(
            "{connections} MCP connection{}",
            if connections == 1 { "" } else { "s" }
        ));
    }
    let controllers = snapshot.controllers.len();
    if controllers > 0 {
        parts.push(format!(
            "{controllers} controller{}",
            if controllers == 1 { "" } else { "s" }
        ));
    }
    if snapshot.workflow.busy() || snapshot.javascript.run.is_some() {
        parts.push("workflow active".to_owned());
    }
    let waiting = snapshot
        .channels
        .iter()
        .filter(|channel| !channel.approved)
        .count()
        + usize::from(snapshot.direct_verification.is_some())
        + usize::from(
            snapshot
                .javascript
                .run
                .as_ref()
                .and_then(|run| run.input.as_ref())
                .is_some_and(|input| !input.approved),
        );
    if waiting > 0 {
        parts.push(format!("{waiting} waiting for your review"));
    }
    if parts.is_empty() {
        "Connections, controllers, and workflow runs. Nothing active.".to_owned()
    } else {
        parts.join(" · ")
    }
}

const fn terminal_font_label(font: TerminalFontPreference) -> &'static str {
    match font {
        TerminalFontPreference::JetBrainsMono => "JetBrains Mono",
        TerminalFontPreference::IosevkaTerm => "Iosevka Term",
        TerminalFontPreference::JuliaMono => "JuliaMono",
        TerminalFontPreference::MapleMono => "Maple Mono",
    }
}

const WORKFLOW_SOURCE_PAGE_BYTES: usize = 2048;

#[derive(Clone, Default)]
struct WorkflowSourceCursor {
    digest: String,
    module: String,
    page: usize,
}

fn workflow_source_page(source: &str, requested: usize) -> (usize, usize, std::ops::Range<usize>) {
    let pages = source.len().div_ceil(WORKFLOW_SOURCE_PAGE_BYTES).max(1);
    let page = requested.min(pages - 1);
    let boundary = |offset: usize| {
        let mut offset = offset.min(source.len());
        while !source.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    let start = boundary(page * WORKFLOW_SOURCE_PAGE_BYTES);
    let end = boundary((page + 1) * WORKFLOW_SOURCE_PAGE_BYTES);
    (page, pages, start..end)
}

fn escape_workflow_source(source: &str) -> String {
    let mut escaped = String::new();
    for character in source.chars() {
        match character {
            '\n' => escaped.push_str("\\n\n"),
            '\\' => escaped.push_str("\\\\"),
            ' '..='~' => escaped.push(character),
            _ => escaped.extend(character.escape_unicode()),
        }
    }
    escaped
}

fn workflow_source_review(ui: &mut Ui, bundle: &festerm_automation_js::snapshot::ApprovedBundle) {
    let snapshot = bundle.snapshot();
    ui.collapsing("Review captured source (not approval)", |ui| {
        ssh_paragraph(
            ui,
            "This is the captured source, not a live file. Backslashes are doubled; \
             non-ASCII and control characters are shown as Unicode escapes. \
             Each newline is marked \\n before the line break. Pages may split a line.",
        );
        let id = ui.id().with("workflow-source-review");
        let mut cursor = ui
            .data(|data| data.get_temp::<WorkflowSourceCursor>(id))
            .unwrap_or_default();
        if cursor.digest != bundle.digest() || !snapshot.modules.contains_key(&cursor.module) {
            cursor = WorkflowSourceCursor {
                digest: bundle.digest().to_owned(),
                module: snapshot.manifest.entry.clone(),
                page: 0,
            };
        }
        egui::ComboBox::from_label("Source module")
            .selected_text(&cursor.module)
            .show_ui(ui, |ui| {
                for module in &snapshot.manifest.modules {
                    if ui
                        .selectable_label(cursor.module == *module, module)
                        .clicked()
                    {
                        cursor.module.clone_from(module);
                        cursor.page = 0;
                    }
                }
            });
        let source = &snapshot.modules[&cursor.module];
        let (page, pages, _) = workflow_source_page(source, cursor.page);
        cursor.page = page;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(cursor.page > 0, egui::Button::new("Previous source page"))
                .clicked()
            {
                cursor.page -= 1;
            }
            if ui
                .add_enabled(
                    cursor.page + 1 < pages,
                    egui::Button::new("Next source page"),
                )
                .clicked()
            {
                cursor.page += 1;
            }
        });
        let (page, pages, range) = workflow_source_page(source, cursor.page);
        let first = if range.is_empty() { 0 } else { range.start + 1 };
        ui.label(format!(
            "Page {} of {} - source bytes {}-{} of {}",
            page + 1,
            pages,
            first,
            range.end,
            source.len()
        ));
        ui.label(egui::RichText::new(escape_workflow_source(&source[range])).monospace());
        ui.data_mut(|data| data.insert_temp(id, cursor));
    });
}

fn workflow_settings(
    ui: &mut Ui,
    snapshot: &crate::automation::SettingsSnapshot,
    enabled: bool,
) -> Option<AppCommand> {
    use crate::automation::{NativeCommand, WorkflowStatus};
    use festerm_automation_js::snapshot::{Capability, ParameterType};

    ssh_paragraph(
        ui,
        "Beta: selecting a workflow captures a frozen preview and grants no access. Running \
         it lets the script read the one terminal you choose for up to 5 minutes; terminal \
         output can contain secrets. Each text or key input is shown here and waits for \
         your approval. Access stops when the screen locks or the computer sleeps.",
    );
    if !enabled {
        return None;
    }

    let mut command = SettingsAutomationCommands::default();
    if ui
        .add_enabled(
            !snapshot.workflow.busy(),
            egui::Button::new("Select workflow JSON"),
        )
        .clicked()
    {
        command.record(AppCommand::OpenWorkflowPicker);
    }
    match &snapshot.workflow.status {
        WorkflowStatus::Empty => {}
        WorkflowStatus::Capturing => {
            ssh_paragraph(ui, "Capturing the selected workflow in the background.");
        }
        WorkflowStatus::Waiting => {
            ssh_paragraph(
                ui,
                "The newer selection will be captured after the previous file read exits.",
            );
        }
        WorkflowStatus::Cancelling => {
            ssh_paragraph(
                ui,
                "The cancelled file read is still finishing. Select another workflow after it exits.",
            );
        }
        WorkflowStatus::Failed(error) => {
            ui.colored_label(theme::STATUS_ERROR, error);
        }
        WorkflowStatus::Captured => {
            if let Some(bundle) = snapshot.workflow.current_bundle() {
                let manifest = bundle.manifest();
                ui.label(format!("Workflow: {}", manifest.name));
                ui.label(format!("Captured snapshot SHA-256: {}", bundle.digest()));
                ssh_paragraph(
                    ui,
                    "This snapshot has not been approved. The selected file is not proof \
                     of publisher identity.",
                );
                ui.collapsing("Requested capabilities (not granted)", |ui| {
                    if manifest.capabilities.is_empty() {
                        ui.label("None");
                    }
                    for capability in &manifest.capabilities {
                        ui.label(match capability {
                            Capability::Observe => "Discover approved terminal attachments",
                            Capability::ReadScreen => "Read terminal screens",
                            Capability::ReadScrollback => "Read retained terminal history",
                            Capability::GetSelection => "Read terminal selections",
                            Capability::WaitForChange => "Wait for terminal changes",
                            Capability::SendText => "Send terminal text",
                            Capability::SendKeys => "Send terminal keys",
                            Capability::Open => "Open terminal sessions",
                            Capability::InvokeWorkflow => "Invoke other workflows",
                        });
                    }
                });
                ui.collapsing("Parameters and modules", |ui| {
                    for (name, kind) in &manifest.parameters {
                        let description = match kind {
                            ParameterType::Text { max_bytes } => {
                                format!("text, at most {max_bytes} bytes")
                            }
                            ParameterType::Integer { min, max } => {
                                format!("integer from {min} to {max}")
                            }
                            ParameterType::Boolean {} => "boolean".to_owned(),
                            ParameterType::Target {} => "terminal attachment".to_owned(),
                        };
                        ui.label(format!("{name}: {description}"));
                    }
                    for module in &manifest.modules {
                        let suffix = if module == &manifest.entry {
                            " (entry)"
                        } else {
                            ""
                        };
                        ui.label(format!("{module}{suffix}"));
                    }
                });
                workflow_source_review(ui, bundle);
                workflow_run_settings(ui, snapshot, bundle, &mut command);
            }
        }
    }
    if snapshot.workflow.current_bundle().is_none() {
        if let Some(run) = &snapshot.javascript.run {
            workflow_run_status(ui, run, &mut command);
        }
    }
    if (snapshot.workflow.busy() || snapshot.workflow.current_bundle().is_some())
        && ui.button("Clear workflow preview").clicked()
    {
        command.record(AppCommand::Automation(NativeCommand::ClearWorkflow));
    }
    command.resolve(None)
}

#[derive(Clone, Default)]
struct WorkflowRunForm {
    target: Option<festerm_automation::TargetRef>,
    text: std::collections::BTreeMap<String, String>,
    booleans: std::collections::BTreeMap<String, bool>,
    reviewed: bool,
}

fn workflow_run_settings(
    ui: &mut Ui,
    snapshot: &crate::automation::SettingsSnapshot,
    bundle: &festerm_automation_js::snapshot::ApprovedBundle,
    command: &mut SettingsAutomationCommands,
) {
    use crate::automation::{javascript::Monitoring, NativeCommand};
    use festerm_automation_js::snapshot::{ParameterType, ParameterValue};

    ui.add_space(8.0);
    ui.label(egui::RichText::new("Run workflow").strong());
    let view = &snapshot.javascript;
    if let Some(run) = &view.run {
        workflow_run_status(ui, run, command);
        return;
    }
    if let Some(outcome) = &view.outcome {
        ui.label(outcome);
    }
    match view.monitoring {
        Monitoring::Off => {
            ssh_paragraph(
                ui,
                "Workflows run only while privacy monitoring is active. Starting monitoring \
                 grants no terminal access.",
            );
            if ui.button("Start privacy monitoring").clicked() {
                command.record(AppCommand::Automation(
                    NativeCommand::StartWorkflowMonitoring,
                ));
            }
            return;
        }
        Monitoring::Starting => {
            ssh_paragraph(ui, "Starting privacy monitoring.");
            return;
        }
        Monitoring::Ready => {}
    }
    let manifest = bundle.manifest();
    let id = egui::Id::new(("workflow-run-form", bundle.digest()));
    let mut form: WorkflowRunForm = ui.data_mut(|data| data.get_temp(id)).unwrap_or_default();
    if form
        .target
        .is_some_and(|target| !view.targets.iter().any(|(item, _)| *item == target))
    {
        form.target = None;
        form.reviewed = false;
    }
    let selected = form
        .target
        .and_then(|target| view.targets.iter().find(|(item, _)| *item == target))
        .map_or("Choose a local terminal", |(_, label)| label.as_str());
    egui::ComboBox::from_id_salt(("workflow-run-target", bundle.digest()))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (target, label) in &view.targets {
                if ui
                    .selectable_label(form.target == Some(*target), label)
                    .clicked()
                {
                    form.target = Some(*target);
                    form.reviewed = false;
                }
            }
        });
    if view.targets.is_empty() {
        ssh_paragraph(ui, "No local shell terminal is open.");
    }
    let mut parameters = festerm_automation_js::snapshot::Parameters::new();
    let mut valid = true;
    for (name, kind) in &manifest.parameters {
        ui.horizontal(|ui| {
            ui.label(format!("{name}:"));
            match kind {
                ParameterType::Text { max_bytes } => {
                    let value = form.text.entry(name.clone()).or_default();
                    if ui.text_edit_singleline(value).changed() {
                        form.reviewed = false;
                    }
                    if value.len() > *max_bytes as usize {
                        valid = false;
                        ui.colored_label(theme::STATUS_ERROR, "too long");
                    }
                    parameters.insert(name.clone(), ParameterValue::Text(value.clone()));
                }
                ParameterType::Integer { min, max } => {
                    let value = form
                        .text
                        .entry(name.clone())
                        .or_insert_with(|| min.to_string());
                    if ui.text_edit_singleline(value).changed() {
                        form.reviewed = false;
                    }
                    match value.trim().parse::<i64>() {
                        Ok(number) if (*min..=*max).contains(&number) => {
                            parameters.insert(name.clone(), ParameterValue::Integer(number));
                        }
                        _ => {
                            valid = false;
                            ui.colored_label(theme::STATUS_ERROR, format!("{min} to {max}"));
                        }
                    }
                }
                ParameterType::Boolean {} => {
                    let value = form.booleans.entry(name.clone()).or_default();
                    if ui.checkbox(value, "").changed() {
                        form.reviewed = false;
                    }
                    parameters.insert(name.clone(), ParameterValue::Boolean(*value));
                }
                ParameterType::Target {} => {
                    ui.label("the selected terminal");
                    parameters.insert(
                        name.clone(),
                        ParameterValue::Target(crate::automation::javascript::TARGET_ID.into()),
                    );
                }
            }
        });
    }
    ui.checkbox(
        &mut form.reviewed,
        "I reviewed this snapshot, its capabilities and parameters for the selected terminal",
    );
    let ready = valid && form.reviewed && form.target.is_some();
    if ui
        .add_enabled(ready, egui::Button::new("Run workflow"))
        .clicked()
    {
        command.record(AppCommand::Automation(NativeCommand::StartWorkflowRun {
            digest: bundle.digest().to_owned(),
            target: form.target,
            parameters,
        }));
        form.reviewed = false;
    }
    ui.data_mut(|data| data.insert_temp(id, form));
}

fn workflow_run_status(
    ui: &mut Ui,
    run: &crate::automation::javascript::RunView,
    command: &mut SettingsAutomationCommands,
) {
    use crate::automation::NativeCommand;

    ui.label(format!("Running: {}", run.name));
    ui.label(format!("Terminal: {}", run.target));
    if run.starting {
        ssh_paragraph(ui, "Starting the isolated worker.");
    }
    if let Some(notice) = &run.notice {
        ui.colored_label(ui.visuals().warn_fg_color, notice);
    }
    if let Some(input) = &run.input {
        ui.group(|ui| {
            ui.label(egui::RichText::new("Workflow input review").strong());
            ui.label(format!("Send to {}:", input.target));
            ui.label(egui::RichText::new(&input.description).monospace());
            ui.label(format!("{} bytes", input.bytes));
            if run.queued_inputs > 1 {
                ui.label(format!("{} more inputs waiting", run.queued_inputs - 1));
            }
            if input.approved {
                ssh_paragraph(ui, "Sending when the terminal is idle.");
            } else {
                ui.horizontal(|ui| {
                    ui.push_id(("workflow-input", input.token), |ui| {
                        if ui.button("Send this input").clicked() {
                            command.record(AppCommand::Automation(
                                NativeCommand::ApproveWorkflowInput { token: input.token },
                            ));
                        }
                        if ui.button("Deny").clicked() {
                            command.record(AppCommand::Automation(
                                NativeCommand::DenyWorkflowInput { token: input.token },
                            ));
                        }
                    });
                });
            }
        });
    }
    if ui.button("Stop workflow").clicked() {
        command.record(AppCommand::Automation(NativeCommand::CancelWorkflowRun {
            token: run.token,
        }));
    }
}

// Settings emits one AppCommand per frame, so conflicts must not discard a stop.
#[derive(Default)]
struct SettingsAutomationCommands(Option<AppCommand>);

impl SettingsAutomationCommands {
    fn record(&mut self, action: AppCommand) {
        self.0 = Some(if self.0.is_some() {
            AppCommand::Automation(crate::automation::NativeCommand::SafetyStop)
        } else {
            action
        });
    }

    fn resolve(self, other: Option<AppCommand>) -> Option<AppCommand> {
        match self.0 {
            Some(AppCommand::OpenWorkflowPicker) => other.or(Some(AppCommand::OpenWorkflowPicker)),
            action => action.or(other),
        }
    }
}

fn record_automation_action(
    slot: &mut Option<crate::automation::NativeCommand>,
    action: crate::automation::NativeCommand,
) {
    *slot = Some(if slot.is_some() {
        crate::automation::NativeCommand::SafetyStop
    } else {
        action
    });
}

fn cancel_controller_input_button(
    ui: &mut Ui,
    command: &mut Option<crate::automation::NativeCommand>,
    channel: u64,
    registration: festerm_automation::ControllerRegistrationId,
    token: u64,
) {
    ui.push_id(("controller-input-cancel", token), |ui| {
        if ui.button("Cancel requested input").clicked() {
            record_automation_action(
                command,
                crate::automation::NativeCommand::CancelControllerInput {
                    channel,
                    registration,
                    input: token,
                },
            );
        }
    });
}

/// A titled card matching the launcher/profile-editor "quiet section" visual
/// language (`ssh_section_heading` + a bordered, rounded surface), so
/// Settings groups related controls the same way the rest of the app does
/// instead of a flat, plain list of buttons.
fn automation_settings(
    ui: &mut Ui,
    snapshot: &crate::automation::SettingsSnapshot,
    enabled: bool,
    window: crate::application::WindowId,
) -> Option<crate::automation::NativeCommand> {
    use crate::automation::{
        presentation::DirectActionDisplay, ControllerInputView, ControllerMetadataView,
        ControllerSelectionView, NativeCommand,
    };
    use festerm_automation::TargetRef;
    ui.add_space(10.0);
    ssh_paragraph(ui, snapshot.status);
    if !enabled {
        clear_direct_comparison_input(ui, direct_comparison_state_id(ui, window));
        return None;
    }
    let mut command = None;
    if ui
        .button("Restart local MCP (revokes MCP access)")
        .clicked()
    {
        record_automation_action(&mut command, NativeCommand::RestartMcp);
    }
    if let Some(action) = direct_verification_settings(ui, snapshot, window) {
        record_automation_action(&mut command, action);
    }
    ssh_paragraph(
        ui,
        "Connect your MCP client using this installed executable with --mcp. \
         Compare the public verification code from festerm_pairing_status below. \
         This code is not the direct-channel fingerprint; connecting never grants access.",
    );
    ssh_paragraph(
        ui,
        "For a signed client identity, generate one key per client with fesTerm --mcp-keygen \
         and provide it to that client's --mcp process as FESTERM_MCP_CLIENT_KEY. Keep the \
         key private and out of command-line arguments. Without it, pairing is connection-scoped.",
    );
    for channel in &snapshot.channels {
        ui.push_id(("automation_channel", channel.channel), |ui| {
            ui.separator();
            ui.label(format!("Client verification code: {}", channel.verification_code));
            if let Some(fingerprint) = &channel.identity_fingerprint {
                ui.label(format!("Authenticated client key: {fingerprint}"));
            } else {
                ssh_paragraph(
                    ui,
                    "This client has no authenticated identity; approvals remain connection-scoped.",
                );
            }
            if channel.approved {
                ssh_paragraph(ui, "Observation granted for at most 5 minutes. No input or execution permission.");
                for (target, label) in &snapshot.targets {
                    if channel.approved_targets.contains(target) {
                        ui.label(format!("Shared: {label}"));
                    }
                }
                if ui.button("Revoke this MCP connection").clicked() {
                    record_automation_action(&mut command, NativeCommand::Revoke { channel: channel.channel });
                }
                return;
            }
            if ui.button("Disconnect this MCP connection").clicked() {
                record_automation_action(&mut command, NativeCommand::Revoke { channel: channel.channel });
            }
            ssh_paragraph(ui, "Choose exact attachments. This discloses their labels, live screen, retained history and selections, including any secrets they contain. \
                At most 1 MiB and 256 operations per grant; no shell input. Lock, suspend, disconnect, replacement, expiry or disabling MCP revokes access.");
            let id = ui.id().with("observation_approval");
            let (mut verified, mut selected) = ui.data(|data| {
                data.get_temp::<(bool, BTreeSet<TargetRef>)>(id).unwrap_or_default()
            });
            selected.retain(|reference| snapshot.targets.iter().any(|(target, _)| target == reference));
            ui.checkbox(&mut verified, "I verified this exact code in my MCP client");
            for (target, label) in &snapshot.targets {
                ui.push_id((target.session(), target.generation()), |ui| {
                    let mut checked = selected.contains(target);
                    if ui.checkbox(&mut checked, label).changed() {
                        if checked {
                            selected.insert(*target);
                        } else {
                            selected.remove(target);
                        }
                    }
                });
            }
            if selected.len() > 32 {
                ssh_paragraph(ui, "Select at most 32 exact attachments.");
            }
            let allowed = verified && !selected.is_empty() && selected.len() <= 32;
            if ui.add_enabled(allowed, egui::Button::new("Approve observation for 5 minutes")).clicked() {
                record_automation_action(&mut command, NativeCommand::ApproveObservation {
                    channel: channel.channel, targets: selected.iter().copied().collect(),
                });
                verified = false;
                selected.clear();
            }
            ui.data_mut(|data| data.insert_temp(id, (verified, selected)));
        });
    }
    ui.separator();
    ui.label("Controller registrations");
    ssh_paragraph(
        ui,
        "Beta: client identifiers and terminal names are untrusted. Registration does not choose a terminal, \
         open a prompt or grant access. Choose and confirm a local terminal in this window, then review \
         each requested input before it is sent. Metadata delivery and presentation are unavailable.",
    );
    if snapshot.controller_opted_in {
        ssh_paragraph(
            ui,
            "Controller registration is opted in for new connections. Restart or disable MCP to end the opt-in.",
        );
    } else if ui
        .add_enabled(
            snapshot.controller_opt_in_available,
            egui::Button::new("Enable controller registration for new connections"),
        )
        .clicked()
    {
        record_automation_action(&mut command, NativeCommand::EnableControllerEnrollment);
    }
    if snapshot.controllers.is_empty() {
        ssh_paragraph(ui, "No controller registrations on active connections.");
    }
    for row in &snapshot.controllers {
        ui.push_id(
            ("controller_registration", row.channel, row.public_id.to_wire()),
            |ui| {
                ui.separator();
                let code = snapshot
                    .channels
                    .iter()
                    .find(|channel| channel.channel == row.channel)
                    .map_or("unavailable", |channel| channel.verification_code.as_str());
                ui.label(format!(
                    "Client {code}; untrusted identifier: \"{}\"",
                    row.label
                ));
                if let Some(target) = row.associated_target {
                    if let Some(candidate) = snapshot
                        .controller_targets
                        .iter()
                        .find(|candidate| candidate.reference == target && candidate.window == window)
                    {
                        ui.label(format!("Associated in this window: {}", candidate.label));
                    } else {
                        ssh_paragraph(ui, "Associated with a local terminal outside this view.");
                    }
                    ssh_paragraph(ui, "This association grants no terminal access.");
                } else {
                    ssh_paragraph(ui, "No terminal associated.");
                }
                if ui.button("Choose local terminal").clicked() {
                    record_automation_action(
                        &mut command,
                        NativeCommand::ChooseController {
                            channel: row.channel,
                            registration: row.registration,
                        },
                    );
                }
                if ui.button("Unregister controller").clicked() {
                    record_automation_action(
                        &mut command,
                        NativeCommand::UnregisterController {
                            channel: row.channel,
                            registration: row.registration,
                        },
                    );
                }
                match row.selection {
                    ControllerSelectionView::Idle => {}
                    ControllerSelectionView::Choosing {
                        token,
                        window: chooser_window,
                    } => {
                        if chooser_window != window {
                            ssh_paragraph(ui, "Continue this selection in its originating window, or cancel it.");
                        } else if snapshot.controller_targets_incomplete {
                            ssh_paragraph(ui, "The local terminal list is incomplete or over its limit. Close terminals and try again.");
                        } else if !snapshot.controller_targets.iter().any(|target| target.window == window) {
                            ssh_paragraph(ui, "No live local terminal is available in this window.");
                        } else {
                            ssh_paragraph(
                                ui,
                                "Select one exact local terminal in this window. Positions count all chips from the start of this window's tab row. Nothing is selected by default.",
                            );
                            for candidate in snapshot.controller_targets.iter().filter(|target| target.window == window) {
                                ui.push_id(
                                    (token, candidate.reference.session(), candidate.reference.generation()),
                                    |ui| {
                                        if ui.button(format!("This window — {}", candidate.label)).clicked() {
                                            record_automation_action(
                                                &mut command,
                                                NativeCommand::SelectControllerTarget {
                                                    channel: row.channel,
                                                    registration: row.registration,
                                                    selection: token,
                                                    target: candidate.reference,
                                                },
                                            );
                                        }
                                    },
                                );
                            }
                        }
                        if ui.button("Cancel controller selection").clicked() {
                            record_automation_action(
                                &mut command,
                                NativeCommand::CancelController {
                                    channel: row.channel,
                                    registration: row.registration,
                                },
                            );
                        }
                    }
                    ControllerSelectionView::Reviewing {
                        token,
                        target,
                        window: review_window,
                    } => {
                        let candidate = snapshot.controller_targets.iter().find(|candidate| {
                            candidate.reference == target && candidate.window == window
                        });
                        if review_window != window {
                            ssh_paragraph(ui, "Confirm in the originating window, or cancel.");
                        } else if let Some(candidate) = candidate {
                            ui.label(format!("Review this window — {}", candidate.label));
                            ui.push_id(
                                (token, target.session(), target.generation()),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !snapshot.controller_targets_incomplete,
                                            egui::Button::new("Confirm association (no access granted)"),
                                        )
                                        .clicked()
                                    {
                                        record_automation_action(
                                            &mut command,
                                            NativeCommand::ConfirmController {
                                                channel: row.channel,
                                                registration: row.registration,
                                                selection: token,
                                            },
                                        );
                                    }
                                },
                            );
                        } else {
                            ssh_paragraph(ui, "The selected local terminal is no longer available.");
                        }
                        if ui.button("Cancel controller selection").clicked() {
                            record_automation_action(
                                &mut command,
                                NativeCommand::CancelController {
                                    channel: row.channel,
                                    registration: row.registration,
                                },
                            );
                        }
                    }
                }
                match row.metadata {
                    ControllerMetadataView::Unavailable => {}
                    ControllerMetadataView::Idle {
                        enrollment,
                        window: association_window,
                        target,
                        granted,
                    } => {
                        if association_window == window
                            && snapshot.controller_targets.iter().any(|candidate| {
                                candidate.reference == target && candidate.window == window
                            })
                        {
                            if granted {
                                ssh_paragraph(
                                    ui,
                                    "Metadata access is approved for this association; delivery remains unavailable.",
                                );
                                if ui.button("Revoke this metadata grant").clicked() {
                                    record_automation_action(
                                        &mut command,
                                        NativeCommand::RevokeControllerMetadata {
                                            channel: row.channel,
                                            registration: row.registration,
                                            enrollment,
                                        },
                                    );
                                }
                            } else if ui
                                .add_enabled(
                                    !snapshot.controller_targets_incomplete,
                                    egui::Button::new("Review metadata access"),
                                )
                                .clicked()
                            {
                                record_automation_action(
                                    &mut command,
                                    NativeCommand::ReviewControllerMetadata {
                                        channel: row.channel,
                                        registration: row.registration,
                                        enrollment,
                                    },
                                );
                            }
                        }
                    }
                    ControllerMetadataView::Reviewing {
                        token,
                        window: review_window,
                        target,
                    } => {
                        let candidate = snapshot.controller_targets.iter().find(|candidate| {
                            candidate.reference == target && candidate.window == window
                        });
                        if review_window != window {
                            ssh_paragraph(ui, "Confirm metadata in the originating window, or cancel.");
                        } else if let Some(candidate) = candidate {
                            if snapshot.controller_metadata_review_is_current(row) {
                                ui.label(format!(
                                    "Review metadata for client {code}: {}",
                                    candidate.label
                                ));
                                ssh_paragraph(
                                    ui,
                                    "Approve only the terminal label and lifecycle status for at most 5 minutes. \
                                     This grants no screen, history, selection or input access and does not require JavaScript. \
                                     Metadata delivery remains unavailable.",
                                );
                                ui.push_id(
                                    (token, target.session(), target.generation()),
                                    |ui| {
                                        if ui
                                            .add_enabled(
                                                !snapshot.controller_targets_incomplete,
                                                egui::Button::new("Approve this metadata grant"),
                                            )
                                            .clicked()
                                        {
                                            record_automation_action(
                                                &mut command,
                                                NativeCommand::ConfirmControllerMetadata {
                                                    channel: row.channel,
                                                    registration: row.registration,
                                                    review: token,
                                                },
                                            );
                                        }
                                    },
                                );
                            } else {
                                ssh_paragraph(ui, "Metadata review was cancelled.");
                            }
                        } else {
                            ssh_paragraph(ui, "The selected local terminal is no longer available.");
                        }
                        if ui.button("Cancel metadata review").clicked() {
                            record_automation_action(
                                &mut command,
                                NativeCommand::CancelControllerMetadata {
                                    channel: row.channel,
                                    registration: row.registration,
                                    review: token,
                                },
                            );
                        }
                    }
                    ControllerMetadataView::Cancelling => {
                        ssh_paragraph(ui, "Cancelling metadata review.");
                    }
                    ControllerMetadataView::Revoking => {
                        ssh_paragraph(ui, "Revoking metadata access.");
                    }
                }
                match &row.input {
                    ControllerInputView::Unavailable => {}
                    ControllerInputView::Requested {
                        token,
                        window: input_window,
                        target,
                    } => {
                        if *input_window == window
                            && snapshot.controller_input_review_is_current(row)
                        {
                            if let Some(candidate) = snapshot.controller_targets.iter().find(|candidate| {
                                candidate.reference == *target && candidate.window == window
                            }) {
                                ui.label(format!("Client {code} requested input for {}", candidate.label));
                                ui.push_id(("controller-input-review", *token), |ui| {
                                    if ui
                                        .add_enabled(
                                            !snapshot.controller_targets_incomplete,
                                            egui::Button::new("Review requested input"),
                                        )
                                        .clicked()
                                    {
                                        record_automation_action(
                                            &mut command,
                                            NativeCommand::ReviewControllerInput {
                                                channel: row.channel,
                                                registration: row.registration,
                                                input: *token,
                                            },
                                        );
                                    }
                                });
                            }
                        } else {
                            ssh_paragraph(ui, "Review this input in the associated window, or cancel.");
                        }
                        cancel_controller_input_button(
                            ui, &mut command, row.channel, row.registration, *token,
                        );
                    }
                    ControllerInputView::Preparing { token, window: input_window } => {
                        if *input_window == window {
                            ssh_paragraph(ui, "Preparing the exact input and existing metadata disclosures.");
                        }
                        cancel_controller_input_button(
                            ui, &mut command, row.channel, row.registration, *token,
                        );
                    }
                    ControllerInputView::Reviewing {
                        token,
                        window: input_window,
                        target,
                        action,
                        metadata_sources,
                    } => {
                        let candidate = snapshot.controller_targets.iter().find(|candidate| {
                            candidate.reference == *target && candidate.window == window
                        });
                        if *input_window != window {
                            ssh_paragraph(ui, "Review this input in the associated window, or cancel.");
                        } else if let Some(candidate) = candidate.filter(|_| {
                            snapshot.controller_input_review_is_current(row)
                        }) {
                            ui.label(format!(
                                "Client {code}; input target: {} (session {}, attachment {})",
                                candidate.label,
                                target.session(),
                                target.generation(),
                            ));
                            ssh_paragraph(
                                ui,
                                "Review the complete escaped input below. Text can execute even without Enter.",
                            );
                            egui::ScrollArea::vertical()
                                .id_salt(("controller-input", row.channel, *token))
                                .max_height(240.0)
                                .show(ui, |ui| match action {
                                    DirectActionDisplay::Text { escaped, submit } => {
                                        ui.add(egui::Label::new(format!("Text: \"{escaped}\"")).wrap());
                                        ui.label(if *submit {
                                            "Enter will be sent after the text."
                                        } else {
                                            "No extra Enter will be sent."
                                        });
                                    }
                                    DirectActionDisplay::Keys(keys) => {
                                        for (index, key) in keys.iter().enumerate() {
                                            ui.add(
                                                egui::Label::new(format!("Key {}: \"{}\"", index + 1, key))
                                                    .wrap(),
                                            );
                                        }
                                    }
                                });
                            if metadata_sources.is_empty() {
                                ssh_paragraph(ui, "No independently approved metadata sources are included.");
                            } else {
                                ssh_paragraph(
                                    ui,
                                    "Information available from each metadata source below can influence input at this target:",
                                );
                                for source in metadata_sources {
                                    ui.add(egui::Label::new(source).wrap());
                                }
                            }
                            ssh_paragraph(
                                ui,
                                "Approval sends this exact input to the terminal queue and permits \
                                 new intentional requests for the same action while armed, for at \
                                 most five minutes and subject to shared operation budgets. Queue \
                                 admission does not mean a command completed. Never automatically \
                                 replay after an unknown or lost response.",
                            );
                            ui.push_id(("controller-input-confirm", *token), |ui| {
                                if ui
                                    .add_enabled(
                                        !snapshot.controller_targets_incomplete,
                                        egui::Button::new("Approve and send this input"),
                                    )
                                    .clicked()
                                {
                                    record_automation_action(
                                        &mut command,
                                        NativeCommand::ConfirmControllerInput {
                                            channel: row.channel,
                                            registration: row.registration,
                                            input: *token,
                                        },
                                    );
                                }
                            });
                        } else {
                            ssh_paragraph(ui, "This input review is no longer current.");
                        }
                        cancel_controller_input_button(
                            ui, &mut command, row.channel, row.registration, *token,
                        );
                    }
                    ControllerInputView::Submitting {
                        token,
                        window: input_window,
                        may_be_admitted,
                    } => {
                        if *input_window == window {
                            ssh_paragraph(
                                ui,
                                if *may_be_admitted {
                                    "The input may already have been admitted; cancellation cannot undo it."
                                } else {
                                    "Submitting the approved input."
                                },
                            );
                        }
                        cancel_controller_input_button(
                            ui, &mut command, row.channel, row.registration, *token,
                        );
                    }
                    ControllerInputView::Cancelling => {
                        ssh_paragraph(ui, "Cancelling requested input.");
                    }
                }
                if let Some(control) = row.control.filter(|control| control.window == window) {
                    ui.label(format!(
                        "Control handoff armed for session {} (attachment {}).",
                        control.target.session(),
                        control.target.generation(),
                    ));
                    ssh_paragraph(
                        ui,
                        "The same approved action may be requested again until the original \
                         expiry or shared budgets are reached. Disarming stops later input but \
                         cannot undo input already admitted.",
                    );
                    ui.push_id(("controller-input-disarm", row.channel, control.token), |ui| {
                        if ui.button("Disarm terminal control").clicked() {
                            record_automation_action(
                                &mut command,
                                NativeCommand::DisarmControllerInput {
                                    channel: row.channel,
                                    registration: row.registration,
                                    input: control.token,
                                },
                            );
                        }
                    });
                }
            },
        );
    }
    command
}

#[cfg(test)]
impl crate::automation::SettingsSnapshot {
    pub(crate) fn gallery_show(ui: &mut Ui, scene: &str) {
        use crate::application::WindowId;
        use crate::automation::{
            javascript::{InputView, Monitoring, RunView},
            presentation::DirectActionDisplay,
            ControllerArmedControl, ControllerInputView, ControllerMetadataView,
            ControllerSelectionView, WorkflowStatus, WorkflowView,
        };
        let window = WindowId::PRIMARY;
        let (mut snapshot, _, _, target, _, enrollment) = Self::with_controller_for_test();
        snapshot.status = "Synthetic local MCP presentation. No adapter is listening.";
        snapshot.targets = vec![(target, "Synthetic build · exact terminal attachment".into())];
        if !scene.starts_with("controller-") {
            snapshot.controllers.clear();
        } else {
            let row = &mut snapshot.controllers[0];
            row.label = "Synthetic untrusted controller".into();
            let controller_scene = match scene {
                "controller-choose-empty"
                | "controller-choose-incomplete"
                | "controller-choose-foreign" => "controller-choose",
                "controller-associate-missing" | "controller-associate-foreign" => {
                    "controller-associate"
                }
                "controller-metadata-stale"
                | "controller-metadata-missing"
                | "controller-metadata-foreign" => "controller-metadata-review",
                "controller-input-stale"
                | "controller-input-missing"
                | "controller-input-foreign"
                | "controller-input-submit" => "controller-input-review",
                "controller-submitting-not-admitted" => "controller-submitting",
                _ => scene,
            };
            match controller_scene {
                "controller-choose" => {
                    row.selection = ControllerSelectionView::Choosing { token: 17, window }
                }
                "controller-associate" => {
                    row.selection = ControllerSelectionView::Reviewing {
                        token: 17,
                        window,
                        target,
                    }
                }
                "controller-metadata" | "controller-metadata-granted" => {
                    row.associated_target = Some(target);
                    row.metadata = ControllerMetadataView::Idle {
                        enrollment,
                        window,
                        target,
                        granted: scene == "controller-metadata-granted",
                    };
                }
                "controller-metadata-review" => {
                    row.associated_target = Some(target);
                    row.set_metadata_review_for_test(31, window, target);
                }
                "controller-input-request" => {
                    row.associated_target = Some(target);
                    row.set_input_requested_for_test(53, window, target);
                }
                "controller-input-review" => {
                    row.associated_target = Some(target);
                    row.set_input_review_for_test(
                        53,
                        window,
                        target,
                        DirectActionDisplay::Text {
                            escaped: "printf 'synthetic review\\\\n'".into(),
                            submit: scene == "controller-input-submit",
                        },
                        vec!["Explicit metadata source · synthetic build".into()],
                    );
                }
                "controller-keys-review" => {
                    row.associated_target = Some(target);
                    row.set_input_review_for_test(
                        53,
                        window,
                        target,
                        DirectActionDisplay::Keys(vec!["Ctrl+C".into()]),
                        Vec::new(),
                    );
                }
                "controller-preparing" => {
                    row.input = ControllerInputView::Preparing { token: 53, window }
                }
                "controller-submitting" => {
                    row.input = ControllerInputView::Submitting {
                        token: 53,
                        window,
                        may_be_admitted: scene != "controller-submitting-not-admitted",
                    }
                }
                "controller-cancelling" => row.input = ControllerInputView::Cancelling,
                "controller-metadata-cancelling" => {
                    row.metadata = ControllerMetadataView::Cancelling
                }
                "controller-metadata-revoking" => row.metadata = ControllerMetadataView::Revoking,
                "controller-armed" => {
                    row.associated_target = Some(target);
                    row.control = Some(ControllerArmedControl {
                        token: 53,
                        window,
                        target,
                    });
                }
                "controller-idle" | "controller-optin" => {}
                _ => unreachable!("unknown controller scene"),
            }
            if matches!(
                scene,
                "controller-choose-empty"
                    | "controller-associate-missing"
                    | "controller-metadata-missing"
                    | "controller-input-missing"
            ) {
                snapshot.controller_targets.clear();
            }
            if scene == "controller-choose-incomplete" {
                snapshot.controller_targets_incomplete = true;
            }
            let other = WindowId::for_test(2);
            match scene {
                "controller-optin" => {
                    snapshot.controllers.clear();
                    snapshot.controller_opted_in = false;
                    snapshot.controller_opt_in_available = true;
                }
                "controller-choose-foreign" => {
                    snapshot.controllers[0].selection = ControllerSelectionView::Choosing {
                        token: 17,
                        window: other,
                    }
                }
                "controller-associate-foreign" => {
                    snapshot.controllers[0].selection = ControllerSelectionView::Reviewing {
                        token: 17,
                        window: other,
                        target,
                    }
                }
                "controller-metadata-foreign" => {
                    snapshot.controllers[0].set_metadata_review_for_test(31, other, target)
                }
                "controller-input-foreign" => snapshot.controllers[0].set_input_review_for_test(
                    53,
                    other,
                    target,
                    DirectActionDisplay::Keys(vec!["Ctrl+C".into()]),
                    Vec::new(),
                ),
                "controller-metadata-stale" | "controller-input-stale" => snapshot.disable(),
                _ => {}
            }
        }
        if scene == "direct-verify" {
            snapshot.direct_verification =
                Some(crate::automation::verification::View::for_test(window));
        }
        if scene == "observation-granted" {
            snapshot.channels[0].approved = true;
            snapshot.channels[0].approved_targets = vec![target];
        }
        if scene.starts_with("workflow-") {
            use festerm_automation_js::snapshot::{
                ApprovedBundle, Capability, Manifest, SourceSnapshot,
            };
            use std::collections::{BTreeMap, BTreeSet};
            snapshot.channels.clear();
            match scene {
                "workflow-capturing" => snapshot.workflow.status = WorkflowStatus::Capturing,
                "workflow-waiting" => snapshot.workflow.status = WorkflowStatus::Waiting,
                "workflow-failed" => snapshot.workflow.status = WorkflowStatus::Failed(
                    "Synthetic preview refusal: the selected bundle failed validation. No access granted.".into(),
                ),
                "workflow-cancelling" => snapshot.workflow.status = WorkflowStatus::Cancelling,
                "workflow-preview" | "workflow-source" | "workflow-monitoring" | "workflow-ready"
                | "workflow-capabilities" | "workflow-parameters" | "workflow-source-modules"
                | "workflow-source-page2" | "workflow-source-last" | "workflow-parameters-invalid" | "workflow-parameters-reviewed"
                | "workflow-ready-no-target" => {
                    use festerm_automation_js::snapshot::ParameterType;
                    let rich = matches!(scene, "workflow-capabilities" | "workflow-parameters" | "workflow-source-modules"
                        | "workflow-source-page2" | "workflow-source-last" | "workflow-parameters-invalid" | "workflow-parameters-reviewed");
                    let modules = if rich {
                        BTreeMap::from([
                            ("helper.js".into(), "export const synthetic = true;".into()),
                            ("main.js".into(), format!("import 'helper.js';\n{}export default async function(ctx) {{ return 'synthetic'; }}", "// synthetic captured source line\n".repeat(150))),
                        ])
                    } else {
                        BTreeMap::from([("main.js".into(), "export default async function(ctx) { return 'synthetic'; }".into())])
                    };
                    let parameters = if rich {
                        BTreeMap::from([
                            ("count".into(), ParameterType::Integer { min: 1, max: 10 }),
                            ("confirm".into(), ParameterType::Boolean {}),
                            ("label".into(), ParameterType::Text { max_bytes: 32 }),
                            ("terminal".into(), ParameterType::Target {}),
                        ])
                    } else { BTreeMap::new() };
                    let bundle = ApprovedBundle::new(SourceSnapshot {
                        manifest: Manifest {
                            version: 1, api_version: 1, name: "synthetic-build-review".into(),
                            entry: "main.js".into(), modules: modules.keys().cloned().collect(),
                            parameters,
                            capabilities: BTreeSet::from([Capability::ReadScreen, Capability::SendText]),
                        },
                        modules,
                    }).unwrap();
                    if rich {
                        let form = WorkflowRunForm {
                            target: Some(target), reviewed: scene == "workflow-parameters-reviewed",
                            text: BTreeMap::from([
                                ("count".into(), if scene == "workflow-parameters-invalid" { "99".into() } else { "3".into() }),
                                ("label".into(), if scene == "workflow-parameters-invalid" { "x".repeat(40) } else { "synthetic".into() }),
                            ]),
                            booleans: BTreeMap::from([("confirm".into(), true)]),
                        };
                        ui.data_mut(|data| data.insert_temp(egui::Id::new(("workflow-run-form", bundle.digest())), form));
                    }
                    snapshot.workflow = WorkflowView::captured_for_test(bundle);
                    if scene == "workflow-ready" || scene == "workflow-ready-no-target" || rich {
                        snapshot.javascript.monitoring = Monitoring::Ready;
                        if scene != "workflow-ready-no-target" {
                            snapshot.javascript.targets = vec![(target, "Synthetic build · exact attachment".into())];
                        }
                    }
                    if scene == "workflow-monitoring" {
                        snapshot.javascript.monitoring = Monitoring::Starting;
                    }
                }
                "workflow-running" | "workflow-input" | "workflow-sending" | "workflow-starting" | "workflow-input-queued" => {
                    snapshot.javascript.run = Some(RunView {
                        token: 17, name: "Synthetic build review".into(),
                        target: "Synthetic build · exact attachment".into(), starting: scene == "workflow-starting",
                        input: matches!(scene, "workflow-input" | "workflow-sending" | "workflow-input-queued").then(|| InputView {
                            token: 53, target: "Synthetic build".into(),
                            description: "Text: printf 'synthetic review\\\\n' · do not submit".into(),
                            bytes: 29, approved: scene == "workflow-sending",
                        }),
                        queued_inputs: if scene == "workflow-input-queued" { 3 } else { 1 },
                        notice: (scene == "workflow-input-queued").then(|| "Synthetic input refusal: review the retained exact input before any new admission.".into()),
                    });
                }
                "workflow-empty" => {}
                _ => unreachable!("unknown workflow scene"),
            }
            let _ = workflow_settings(ui, &snapshot, true);
        } else {
            let _ = automation_settings(ui, &snapshot, scene != "disabled", window);
        }
    }
}

#[derive(Clone, Default)]
struct DirectComparisonInput {
    generation: u64,
    peer: String,
    verified: bool,
}

fn direct_comparison_state_id(ui: &Ui, window: crate::application::WindowId) -> egui::Id {
    ui.id().with(("direct_mcp_comparison_state", window))
}

fn clear_direct_comparison_input(ui: &Ui, state_id: egui::Id) {
    ui.data_mut(|data| {
        if let Some(previous) = data.remove_temp::<DirectComparisonInput>(state_id) {
            data.remove::<egui::text_edit::TextEditState>(
                state_id.with(("entry", previous.generation)),
            );
        }
    });
}

fn direct_verification_settings(
    ui: &mut Ui,
    snapshot: &crate::automation::SettingsSnapshot,
    window: crate::application::WindowId,
) -> Option<crate::automation::NativeCommand> {
    use crate::automation::{verification::valid_fingerprint, NativeCommand};

    let state_id = direct_comparison_state_id(ui, window);
    let Some(view) = snapshot
        .direct_verification
        .as_ref()
        .filter(|view| view.window == window && view.is_current())
    else {
        clear_direct_comparison_input(ui, state_id);
        return None;
    };
    let previous = ui.data(|data| data.get_temp::<DirectComparisonInput>(state_id));
    let mut state = match previous {
        Some(state) if state.generation == view.generation => state,
        _ => {
            clear_direct_comparison_input(ui, state_id);
            DirectComparisonInput {
                generation: view.generation,
                ..DirectComparisonInput::default()
            }
        }
    };
    let entry_id = state_id.with(("entry", view.generation));
    let mut command = None;
    ui.push_id(("direct_mcp_comparison", view.generation), |ui| {
        ui.separator();
        ui.label("Compare the direct MCP connection");
        ssh_paragraph(
            ui,
            "Read the complete fingerprint independently in your MCP client and enter it here. \
             Do not copy the fingerprint shown below. This comparison alone does not grant access.",
        );
        ui.add(egui::Label::new(egui::RichText::new(&view.fingerprint).monospace()).wrap());
        let label = ui.label("Fingerprint read from the MCP client");
        let input = ui
            .add(
                egui::TextEdit::singleline(&mut state.peer)
                    .id(entry_id)
                    .char_limit(64)
                    .hint_text("64 hexadecimal characters"),
            )
            .labelled_by(label.id);
        if input.changed() {
            state.verified = false;
        }
        let valid = valid_fingerprint(&state.peer);
        ui.add_enabled(
            valid && !input.changed(),
            egui::Checkbox::new(
                &mut state.verified,
                "I read this complete fingerprint independently in my MCP client",
            ),
        );
        if ui
            .add_enabled(
                state.verified && valid && view.is_current(),
                egui::Button::new("Compare fingerprints"),
            )
            .clicked()
        {
            command = Some(NativeCommand::CompareDirectPeer {
                generation: view.generation,
                fingerprint: state.peer.clone(),
            });
            state.peer.clear();
            state.verified = false;
        }
        if ui.button("Reject direct MCP connection").clicked() {
            record_automation_action(
                &mut command,
                NativeCommand::DenyDirectPeer {
                    generation: view.generation,
                },
            );
            state.peer.clear();
            state.verified = false;
        }
    });
    if command.is_some() {
        ui.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(entry_id));
    }
    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

/// A headed Settings card followed by the shared card gap. Returns the
/// card's frame rect.
fn settings_card(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) -> egui::Rect {
    let rect = settings_card_response(ui, title, body).response.rect;
    ui.add_space(SETTINGS_CARD_GAP);
    rect
}

fn settings_card_response(
    ui: &mut Ui,
    title: &str,
    body: impl FnOnce(&mut Ui),
) -> egui::InnerResponse<()> {
    settings_frame(ui, |ui| {
        ssh_section_heading(ui, title);
        ui.add_space(6.0);
        body(ui);
    })
}

/// A Settings card without a heading, for a sub-page whose own title
/// already names its content.
fn settings_panel(ui: &mut Ui, body: impl FnOnce(&mut Ui)) -> egui::Rect {
    let rect = settings_frame(ui, body).response.rect;
    ui.add_space(SETTINGS_CARD_GAP);
    rect
}

fn settings_frame(ui: &mut Ui, body: impl FnOnce(&mut Ui)) -> egui::InnerResponse<()> {
    crate::software_background::show_frame(
        ui,
        egui::Frame::new()
            .fill(theme::SURFACE_TAB_INACTIVE)
            .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::same(16)),
        |ui| {
            ui.set_min_width(ui.available_width());
            body(ui);
        },
    )
}

/// A main-page section that is only a way into its own sub-page.
fn settings_drill_in_card(
    ui: &mut Ui,
    id: egui::Id,
    page: SettingsPage,
    summary: &str,
    navigation: &mut SettingsNavigation,
    open_page: &mut Option<SettingsPage>,
) {
    settings_panel(ui, |ui| {
        let response = settings_drill_in_row(ui, id, page.title(), Some(summary));
        settings_drill_in_focus(ui, &response, page, navigation);
        if response.clicked() {
            *open_page = Some(page);
        }
    });
}

/// Returns keyboard focus to the row a sub-page was opened from.
fn settings_drill_in_focus(
    ui: &Ui,
    response: &egui::Response,
    page: SettingsPage,
    navigation: &mut SettingsNavigation,
) {
    if navigation.return_to == Some(page) {
        response.request_focus();
        ui.scroll_to_rect(response.rect, None);
        navigation.return_to = None;
    }
}

/// A full-width row that opens another page: title, optional one-line
/// summary, and the `OpenSubpage` chevron from the icon system (never a
/// typed ">"). The whole row is one focusable button named by its title;
/// the summary stays readable text for assistive technology.
fn settings_drill_in_row(
    ui: &mut Ui,
    id: egui::Id,
    title: &str,
    summary: Option<&str>,
) -> egui::Response {
    const CHEVRON: f32 = 14.0;
    let background = ui.painter().add(egui::Shape::Noop);
    let row = ui.scope_builder(
        egui::UiBuilder::new().id_salt(id).sense(Sense::click()),
        |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    egui::Sides::new().show(
                        ui,
                        |ui| {
                            ui.set_max_width(
                                (ui.available_width() - CHEVRON - 16.0)
                                    .max(SETTINGS_MIN_DESCRIPTION_WIDTH),
                            );
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(title).color(theme::TEXT_PRIMARY),
                                    )
                                    .selectable(false),
                                );
                                if let Some(summary) = summary {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(summary)
                                                .size(12.0)
                                                .color(theme::TEXT_SECONDARY),
                                        )
                                        .selectable(false),
                                    );
                                }
                            });
                        },
                        |ui| {
                            let (chevron, _) =
                                ui.allocate_exact_size(egui::Vec2::splat(CHEVRON), Sense::hover());
                            icon::paint(
                                ui.painter(),
                                Icon::OpenSubpage,
                                chevron,
                                theme::TEXT_MUTED,
                            );
                        },
                    );
                });
        },
    );
    let response = row.response;
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), title));
    if response.hovered() || response.has_focus() {
        ui.painter().set(
            background,
            egui::Shape::rect_filled(response.rect, 6.0, theme::SURFACE_TAB_ACTIVE),
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            6.0,
            Stroke::new(1.0, theme::BORDER_ACTIVE),
            egui::StrokeKind::Inside,
        );
    }
    response
}

/// One labeled on/off preference row: a fixed title and state-independent
/// description on the left, and a pill-shaped toggle switch on the right
/// (`docs/images/gui-mockups/settings.png`) - replacing a plain text button
/// whose entire label used to flip between "shown"/"hidden" copy. Returns
/// whether the switch was clicked this frame; the caller still owns
/// dispatching the actual `AppCommand`, matching every other control here.
pub(crate) fn settings_toggle_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    value: bool,
) -> bool {
    settings_toggle_row_enabled(ui, title, description, value, true)
}

/// `settings_toggle_row` for a preference that only applies while another
/// one is on: disabled rows keep their place but cannot be toggled.
pub(crate) fn settings_toggle_row_enabled(
    ui: &mut Ui,
    title: &str,
    description: &str,
    value: bool,
    enabled: bool,
) -> bool {
    let mut clicked = false;
    ui.add_enabled_ui(enabled, |ui| {
        egui::Sides::new().show(
            ui,
            |ui| {
                // Reserve room for the switch itself (and the `Sides` gap) so
                // the description wraps at measurement time instead of laying
                // out as one long unwrapped line that pushes the switch off
                // the right edge of the card.
                ui.set_max_width(ui.available_width() - 60.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                    ssh_paragraph(ui, description);
                });
            },
            |ui| {
                clicked = toggle_switch(ui, value, title).clicked();
            },
        );
    });
    clicked && enabled
}

/// Secondary explanatory copy in the shared Settings text role, for surfaces
/// hosted inside Settings that live outside this module.
#[cfg(feature = "local-host-settings")]
pub(crate) fn settings_note(ui: &mut Ui, text: &str) {
    ssh_paragraph(ui, text);
}

/// Painter-drawn pill-shaped toggle switch matching the mockup's on/off
/// control (`docs/images/gui-mockups/settings.png`): a rounded track that
/// fills with the accent color when on, and a circular knob that slides to
/// the matching side - instead of a text button whose whole label changes
/// between "shown"/"hidden" copy. An explicit accessible label is set (like
/// `paint_close_button`'s pattern) since the switch has no text of its own
/// for screen readers or headless-test queries to find.
pub(crate) fn toggle_switch(ui: &mut Ui, value: bool, accessible_label: &str) -> egui::Response {
    let desired_size = egui::vec2(40.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Checkbox,
            ui.is_enabled(),
            value,
            accessible_label,
        )
    });

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool(response.id, value);
        let rounding = rect.height() / 2.0;
        let track_fill = theme::SURFACE_TAB_INACTIVE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        let track_stroke = theme::BORDER_SUBTLE.lerp_to_gamma(theme::ACCENT_PRIMARY, how_on);
        ui.painter().rect_filled(rect, rounding, track_fill);
        ui.painter().rect_stroke(
            rect,
            rounding,
            Stroke::new(1.0, track_stroke),
            egui::StrokeKind::Inside,
        );
        let knob_radius = rounding - 3.0;
        let knob_x = egui::lerp((rect.left() + rounding)..=(rect.right() - rounding), how_on);
        ui.painter().circle_filled(
            egui::pos2(knob_x, rect.center().y),
            knob_radius,
            egui::Color32::WHITE,
        );
    }

    response.on_hover_text(accessible_label)
}

/// One labeled multi-choice preference row: a fixed title/description on the
/// left and a segmented button group on the right
/// (`docs/images/gui-mockups/settings.png`'s "Session chip layout" row),
/// replacing a single text button whose label flipped to name the *other*
/// choice. Returns the index of a newly selected (previously inactive)
/// option; clicking the already-active option is a no-op, matching ordinary
/// segmented-control behavior.
/// The width a row of `selectable_label`s will occupy.
///
/// `egui::Sides` gives its left closure whatever width that closure claims,
/// so a settings row reserves the right-hand control's width up front and
/// lets the description wrap into the remainder. Reserving a fixed guess
/// works only until a control outgrows it, and the failure is not a clipped
/// button: egui grows the enclosing card to fit instead, so the card spills
/// past the window's right edge *and* every later card inherits the wider
/// content width and spills with it. Measuring the control removes the guess.
fn segmented_control_width(ui: &Ui, options: &[(&str, bool)]) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let padding = ui.spacing().button_padding.x * 2.0;
    let labels: f32 = options
        .iter()
        .map(|(label, _)| {
            ui.painter()
                .layout_no_wrap(
                    (*label).to_owned(),
                    font.clone(),
                    egui::Color32::PLACEHOLDER,
                )
                .size()
                .x
                + padding
        })
        .sum();
    labels + ui.spacing().item_spacing.x * options.len().saturating_sub(1) as f32
}

/// The narrowest a settings row's description column is allowed to become
/// while making room for its control, so a wide control cannot squeeze the
/// prose into a one-word-per-line ribbon.
const SETTINGS_MIN_DESCRIPTION_WIDTH: f32 = 180.0;

fn settings_segmented_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[(&str, bool)],
) -> Option<usize> {
    let mut clicked = None;
    // Measured before the row is laid out, because the left closure runs
    // first and has to know how much to leave behind.
    let control_width = segmented_control_width(ui, options);
    egui::Sides::new().show(
        ui,
        |ui| {
            // Reserve exactly what the buttons need (plus the `Sides` gap) so
            // the description wraps at measurement time instead of laying out
            // as one long unwrapped line that pushes the segmented buttons
            // off the right edge of the card and out of click range.
            let reserved = control_width + ui.spacing().item_spacing.x;
            ui.set_max_width((ui.available_width() - reserved).max(SETTINGS_MIN_DESCRIPTION_WIDTH));
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            // `Sides` lays its right closure out right-to-left; a plain
            // `ui.horizontal` would inherit that and show (and Tab through)
            // the options backwards. Anchor a left-to-right block instead.
            ui.allocate_ui_with_layout(
                egui::vec2(control_width, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    for (index, (label, selected)) in options.iter().enumerate() {
                        let response = ui.selectable_label(*selected, *label);
                        if !*selected {
                            // Give unselected segments a visible boundary;
                            // only the selected one is filled.
                            ui.painter().rect_stroke(
                                response.rect,
                                ui.visuals().widgets.inactive.corner_radius,
                                Stroke::new(1.0, theme::BORDER_SUBTLE),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if response.clicked() && !*selected {
                            clicked = Some(index);
                        }
                    }
                },
            );
        },
    );
    clicked
}

/// Give a slider a rail that is actually visible inside a `settings_card`.
///
/// egui paints a slider's rail with `widgets.inactive.bg_fill` (see
/// `egui::Slider::slider_ui`), and `theme::default_visuals` sets that to
/// `SURFACE_TAB_INACTIVE` - which is exactly `settings_card`'s own fill. The
/// rail therefore vanished into the card and the only thing left on screen
/// was the handle's one-pixel outline, so the control read as a small empty
/// box floating in whitespace rather than as a slider. Lifting the rail one
/// surface step and filling the travelled portion with the accent color
/// matches `toggle_switch`'s vocabulary (inert track, accent for "how far
/// on") and makes the handle's position unmistakable.
fn style_settings_slider(ui: &mut Ui) {
    let visuals = &mut ui.style_mut().visuals;
    visuals.widgets.inactive.bg_fill = theme::SURFACE_TAB_ACTIVE;
    visuals.selection.bg_fill = theme::ACCENT_PRIMARY;
}

/// A labeled row with a discrete, clickstop-only slider: dragging or
/// clicking only ever lands on one of `options`' exact indices, unlike a
/// continuous `egui::Slider`, since a scroll-speed multiplier is meant to be
/// chosen from a small named set (mirroring `settings_segmented_row`'s
/// discrete-choice intent) rather than fine-tuned to an arbitrary numeric
/// value. Returns the newly selected value when the slider moves to a
/// different clickstop than `selected` this frame.
fn settings_clickstop_row(
    ui: &mut Ui,
    title: &str,
    description: &str,
    options: &[ScrollSpeedPreference],
    selected: ScrollSpeedPreference,
) -> Option<ScrollSpeedPreference> {
    const SLIDER_WIDTH: f32 = 160.0;

    let mut changed = None;
    // `egui::Sides` defaults its row height to a single `interact_size.y`
    // (matching the toggle/segmented rows' one-line right side), but this
    // row's right side stacks a slider *and* a value label underneath it.
    // Reserve enough height for both stacked lines up front.
    let row_height = ui.spacing().interact_size.y * 2.0 + 4.0;
    egui::Sides::new().height(row_height).show(
        ui,
        |ui| {
            // Same defensive width reservation as the other settings rows:
            // without it the description can measure as one long unwrapped
            // line and push the slider off the right edge of the card.
            ui.set_max_width(ui.available_width() - 190.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).color(theme::TEXT_PRIMARY));
                ssh_paragraph(ui, description);
            });
        },
        |ui| {
            // Unlike `ui.horizontal`, plain `ui.vertical` always lays out
            // its children with `Layout::top_down(Align::Min)` and does
            // not mirror the enclosing `Sides` right-to-left direction (see
            // `egui::Ui::horizontal`, which explicitly checks
            // `placer.prefer_right_to_left()` and `ui.vertical`, which
            // does not). A bare `ui.vertical(...)` here inherited the
            // *entire* remaining card width as its rect and then
            // left-aligned the slider and label inside it, so on any card
            // wider than description-text-plus-slider, the block rendered
            // immediately after the description paragraph instead of
            // pinned to the card's right edge - squeezing the slider down
            // to a sliver-sized hit target and spilling the value label
            // over the description (reported: "you can't tell it's
            // actually a slider" and "sliding the value doesn't seem to
            // change scroll speed"). Explicitly allocating a
            // `SLIDER_WIDTH`-wide block lets the *outer* right-to-left
            // cursor place it, matching how `toggle_switch` and
            // `settings_segmented_row`'s `ui.horizontal` already anchor to
            // the right edge.
            ui.allocate_ui_with_layout(
                egui::vec2(SLIDER_WIDTH, row_height),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    let max_index = options.len().saturating_sub(1);
                    let mut index = selected.index().min(max_index);
                    ui.style_mut().spacing.slider_width = SLIDER_WIDTH;
                    style_settings_slider(ui);
                    let response = ui.add(
                        egui::Slider::new(&mut index, 0..=max_index)
                            .step_by(1.0)
                            .trailing_fill(true)
                            .show_value(false),
                    );
                    if response.changed() {
                        let new_value = ScrollSpeedPreference::from_index(index);
                        if new_value != selected {
                            changed = Some(new_value);
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(selected.label())
                            .size(12.0)
                            .color(theme::TEXT_MUTED),
                    );
                },
            );
        },
    );
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };

    struct SettingsHarnessState {
        command: Option<AppCommand>,
        keyboard_bindings: festerm_config::KeyboardBindings,
        font_download_failed: bool,
        mcp_enabled: bool,
        javascript_enabled: bool,
        mcp_ui_revision: u64,
        javascript_ui_revision: u64,
        automation_stopped: bool,
        automation_preference_feedback: Option<&'static str>,
        automation_preference_retry_available: bool,
    }

    fn settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(520.0)
    }

    #[test]
    #[ignore = "production Settings keyboard editor visual review capture"]
    fn capture_keyboard_settings_normal_and_narrow() {
        let output = std::env::current_dir().unwrap().join(".keyboard-captures");
        let mut snapshots = egui_kittest::SnapshotResults::new();
        for width in [752.0, 360.0] {
            let mut harness = settings_harness_with_width(width);
            harness.run();
            open_settings_page(&mut harness, "Keyboard shortcuts");
            assert!(harness.query_by_label("Search actions…").is_some());
            assert!(
                harness.query_by_label("Reset all…").is_some(),
                "the all-bindings reset stays visible so users can see a reset exists"
            );

            // Capture a realistic working state rather than a pristine one:
            // one action customized, so the "Customized" badge and both reset
            // routes are actually visible in the review image.
            let mut customized = festerm_config::KeyboardBindings::default();
            customized.set(
                festerm_config::KeyboardAction::ClearTerminal,
                Some("Ctrl+Shift+F9".into()),
            );
            let mut harness = settings_harness_with_bindings(width, customized);
            harness.run();
            open_settings_page(&mut harness, "Keyboard shortcuts");
            harness
                .get_by_role_and_label(accesskit::Role::Button, "New Session")
                .click();
            harness.run();
            harness.get_by_label("Clear binding").click();
            harness.run();
            for label in [
                "Assign binding",
                "Clear binding",
                "Restore default",
                "Reset all…",
            ] {
                let rect = harness
                    .query_by_label(label)
                    .unwrap_or_else(|| panic!("{label} must be rendered at width {width}"))
                    .rect();
                assert!(
                    rect.left() >= 0.0 && rect.right() <= width,
                    "{label} at {rect:?} must fit the {width} editor"
                );
            }
            harness.snapshot_options(
                format!("keyboard-settings-{width}-{}", std::env::consts::OS),
                &egui_kittest::SnapshotOptions::default().output_path(&output),
            );
            snapshots.extend(harness.take_snapshot_results());
        }
        snapshots.unwrap();
    }

    /// A wider settings harness, matching a typical desktop window rather
    /// than the other settings tests' narrow fixed harness width. Needed to
    /// reproduce the "Scroll speed" slider mispositioning regression (see
    /// `scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop`):
    /// the bug only appears once the card is wider than the description
    /// text plus the slider's own width, which the narrow harness never is.
    fn wide_settings_harness() -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_width(1400.0)
    }

    fn settings_harness_with_width(width: f32) -> Harness<'static, SettingsHarnessState> {
        settings_harness_with_bindings(width, Default::default())
    }

    fn settings_harness_with_bindings(
        width: f32,
        bindings: festerm_config::KeyboardBindings,
    ) -> Harness<'static, SettingsHarnessState> {
        // Tall enough that every card, including the keyboard editor at the
        // bottom, is laid out inside the viewport and therefore interactive.
        Harness::builder()
            .with_size(egui::vec2(width, 5200.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
                            #[cfg(feature = "local-host-settings")]
                            local_remote_access: Default::default(),
                            #[cfg(feature = "local-host-settings")]
                            local_share_targets: Vec::new(),
                            keyboard_bindings: state.keyboard_bindings.clone(),
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            show_pane_handles: false,
                            pane_high_contrast: false,
                            confirm_session_close: true,
                            prefer_powershell: true,
                            extend_dev_tunnel_lifetime: false,
                            show_running_dev_tunnels: false,
                            dev_tunnel_extension_days: 30,
                            dev_tunnel_accounts: Vec::new(),
                            customize_local_shell: false,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            font_download_status: state.font_download_failed.then(|| {
                                "Using JetBrains Mono temporarily. Download failed; Retry.".into()
                            }),
                            font_download_failed: state.font_download_failed,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            rectangle_copy_trim_trailing_spaces: true,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            image_memory_budget: ImageMemoryBudgetPreference::MiB512,
                            image_memory_over_budget: false,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            show_resumable_sessions: false,
                            show_durable_session_in_status_bar: false,
                            automatic_update_checks: false,
                            mcp_enabled: state.mcp_enabled,
                            javascript_enabled: state.javascript_enabled,
                            automation_mcp_enabled: state.mcp_enabled && !state.automation_stopped,
                            automation_javascript_enabled: state.javascript_enabled
                                && !state.automation_stopped,
                            automation_mcp_ui_revision: state.mcp_ui_revision,
                            automation_javascript_ui_revision: state.javascript_ui_revision,
                            automation_preference_feedback: state.automation_preference_feedback,
                            automation_preference_retry_available: state
                                .automation_preference_retry_available,
                            automation_window: crate::application::WindowId::PRIMARY,
                            automation: Default::default(),
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                            session_recording_enabled: false,
                            recording_preference_unsaved: false,
                        },
                    ) {
                        // Applied here so the keyboard editor behaves like it
                        // does in the app instead of snapping back to the
                        // defaults on the next frame.
                        if let AppCommand::SetKeyboardBindings(next) = &command {
                            state.keyboard_bindings = next.clone();
                        }
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState {
                    command: None,
                    keyboard_bindings: bindings,
                    font_download_failed: false,
                    mcp_enabled: false,
                    javascript_enabled: false,
                    mcp_ui_revision: 0,
                    javascript_ui_revision: 0,
                    automation_stopped: false,
                    automation_preference_feedback: None,
                    automation_preference_retry_available: false,
                },
            )
    }

    #[test]
    fn settings_exposes_independent_native_automation_toggles() {
        let mut harness = settings_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Enable local MCP (Beta)")
            .click();
        harness.step();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleMcpEnabled)
        ));
        harness.state_mut().command = None;
        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Enable JavaScript workflows (Beta)",
            )
            .click();
        harness.step();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleJavascriptEnabled)
        ));
    }

    #[test]
    fn workflow_selection_is_available_only_when_javascript_is_enabled() {
        for enabled in [false, true] {
            let snapshot = crate::automation::SettingsSnapshot::default();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(800.0, 500.0))
                .build_ui_state(
                    move |ui, command: &mut Option<AppCommand>| {
                        if let Some(action) = workflow_settings(ui, &snapshot, enabled) {
                            *command = Some(action);
                        }
                    },
                    None,
                );
            harness.run();
            if enabled {
                harness.get_by_label("Select workflow JSON").click();
                harness.step();
                assert!(matches!(
                    harness.state(),
                    Some(AppCommand::OpenWorkflowPicker)
                ));
            } else {
                assert!(harness.query_by_label("Select workflow JSON").is_none());
            }
        }
    }

    #[test]
    fn workflow_source_pages_cover_utf8_without_splitting_characters() {
        let source = format!(
            "{}\u{1f512}{}",
            "a".repeat(WORKFLOW_SOURCE_PAGE_BYTES - 2),
            "b".repeat(WORKFLOW_SOURCE_PAGE_BYTES + 1)
        );
        let (_, pages, _) = workflow_source_page(&source, 0);
        let mut reconstructed = String::new();
        let mut previous_end = 0;
        for requested in 0..pages {
            let (page, count, range) = workflow_source_page(&source, requested);
            assert_eq!(page, requested);
            assert_eq!(count, pages);
            assert_eq!(range.start, previous_end);
            assert!(source.is_char_boundary(range.start));
            assert!(source.is_char_boundary(range.end));
            assert!(range.len() <= WORKFLOW_SOURCE_PAGE_BYTES + 3);
            previous_end = range.end;
            reconstructed.push_str(&source[range]);
        }
        assert_eq!(reconstructed, source);
        assert_eq!(
            workflow_source_page(&source, usize::MAX).0,
            pages.saturating_sub(1)
        );
        assert_eq!(workflow_source_page("", 7), (0, 1, 0..0));
    }

    #[test]
    fn workflow_source_display_escapes_spoofing_and_literal_backslashes() {
        assert_eq!(
            escape_workflow_source("say\\\t\u{1b}\u{202e}\u{e9}\n"),
            concat!(r"say\\\u{9}\u{1b}\u{202e}\u{e9}\n", "\n")
        );
    }

    #[test]
    fn workflow_source_review_pages_the_captured_snapshot_and_revokes_display() {
        use festerm_automation_js::snapshot::{ApprovedBundle, Manifest, SourceSnapshot};
        use std::collections::{BTreeMap, BTreeSet};

        let source = format!(
            "import 'helper.js';\n//{}\nexport default 1;",
            "a".repeat(WORKFLOW_SOURCE_PAGE_BYTES)
        );
        let bundle = ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "source-review".into(),
                entry: "main.js".into(),
                modules: vec!["helper.js".into(), "main.js".into()],
                parameters: BTreeMap::new(),
                capabilities: BTreeSet::new(),
            },
            modules: BTreeMap::from([
                ("helper.js".into(), "export const helper = 2;".into()),
                ("main.js".into(), source),
            ]),
        })
        .unwrap();
        let mut snapshot = crate::automation::SettingsSnapshot::default();
        snapshot.workflow = crate::automation::WorkflowView::captured_for_test(bundle);
        let other_window_view = snapshot.workflow.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 700.0))
            .build_ui_state(
                move |ui, _: &mut ()| {
                    workflow_settings(ui, &snapshot, true);
                },
                (),
            );
        harness.run();
        harness
            .get_by_label("Review captured source (not approval)")
            .click();
        harness.run();
        assert!(harness
            .query_by_label_contains("Page 1 of 2 - source bytes 1-2048")
            .is_some());
        harness.get_by_label("Next source page").click();
        harness.step();
        assert!(harness
            .query_by_label_contains("Page 2 of 2 - source bytes 2049-")
            .is_some());
        assert!(harness
            .query_by_label_contains("export default 1;")
            .is_some());
        assert!(harness
            .query_by_role_and_label(accesskit::Role::Button, "Run workflow")
            .is_none());
        assert!(harness.query_by_label("Start privacy monitoring").is_some());

        harness
            .get_by_role_and_label(accesskit::Role::ComboBox, "Source module")
            .click();
        harness.run();
        harness.get_by_label("helper.js").click();
        harness.run();
        assert!(harness.query_by_label_contains("Page 1 of 1").is_some());
        assert!(harness
            .query_by_label_contains("export const helper = 2;")
            .is_some());

        other_window_view.disable();
        harness.step();
        assert!(harness
            .query_by_label("Review captured source (not approval)")
            .is_none());
        assert!(harness
            .query_by_label_contains("export const helper = 2;")
            .is_none());
    }

    #[test]
    fn workflow_preview_disappears_when_another_window_disables_it() {
        use festerm_automation_js::snapshot::{ApprovedBundle, Manifest, SourceSnapshot};
        use std::collections::{BTreeMap, BTreeSet};

        let bundle = ApprovedBundle::new(SourceSnapshot {
            manifest: Manifest {
                version: 1,
                api_version: 1,
                name: "cancelled-preview".into(),
                entry: "main.js".into(),
                modules: vec!["main.js".into()],
                parameters: BTreeMap::new(),
                capabilities: BTreeSet::new(),
            },
            modules: BTreeMap::from([("main.js".into(), "export default 1;".into())]),
        })
        .unwrap();
        let mut snapshot = crate::automation::SettingsSnapshot::default();
        snapshot.workflow = crate::automation::WorkflowView::captured_for_test(bundle);
        let other_window_view = snapshot.workflow.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 500.0))
            .build_ui_state(
                move |ui, _: &mut ()| {
                    workflow_settings(ui, &snapshot, true);
                },
                (),
            );
        harness.run();
        assert!(harness
            .query_by_label("Workflow: cancelled-preview")
            .is_some());

        other_window_view.disable();
        harness.step();
        assert!(harness
            .query_by_label("Workflow: cancelled-preview")
            .is_none());
    }

    #[test]
    fn settings_can_disconnect_an_unapproved_mcp_channel() {
        let snapshot = crate::automation::SettingsSnapshot::with_pending_channel_for_test(7);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 1000.0))
            .build_ui_state(
                move |ui, command: &mut Option<crate::automation::NativeCommand>| {
                    if let Some(action) = automation_settings(
                        ui,
                        &snapshot,
                        true,
                        crate::application::WindowId::PRIMARY,
                    ) {
                        *command = Some(action);
                    }
                },
                None,
            );
        harness.run();
        harness
            .get_by_label("Disconnect this MCP connection")
            .click();
        harness.step();
        assert!(matches!(
            harness.state(),
            Some(crate::automation::NativeCommand::Revoke { channel: 7 })
        ));
    }

    #[test]
    fn direct_comparison_requires_complete_peer_entry_and_fresh_attestation() {
        use crate::application::WindowId;
        use crate::automation::{verification::View, NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            window: WindowId,
            command: Option<NativeCommand>,
        }

        let view = View::for_test(WindowId::PRIMARY);
        let mut snapshot = SettingsSnapshot::default();
        snapshot.direct_verification = Some(view.clone());
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    if let Some(command) =
                        direct_verification_settings(ui, &state.snapshot, state.window)
                    {
                        state.command = Some(command);
                    }
                },
                State {
                    snapshot,
                    window: WindowId::for_test(1),
                    command: None,
                },
            );
        harness.run();
        assert!(harness.query_by_label("Compare fingerprints").is_none());
        harness.state_mut().window = WindowId::PRIMARY;
        harness.run();

        let entry = "Fingerprint read from the MCP client";
        let attestation = "I read this complete fingerprint independently in my MCP client";
        assert!(harness
            .get_by_label(attestation)
            .accesskit_node()
            .is_disabled());
        assert!(harness
            .get_by_label("Compare fingerprints")
            .accesskit_node()
            .is_disabled());
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .focus();
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .type_text(&"B".repeat(64));
        harness.run();
        assert_eq!(
            harness
                .get_by_role_and_label(accesskit::Role::TextInput, entry)
                .value(),
            Some("B".repeat(64))
        );
        assert!(!harness
            .get_by_label(attestation)
            .accesskit_node()
            .is_disabled());
        harness.get_by_label(attestation).click();
        harness.run();
        assert_eq!(
            harness.get_by_label(attestation).accesskit_node().toggled(),
            Some(accesskit::Toggled::True)
        );
        assert!(!harness
            .get_by_label("Compare fingerprints")
            .accesskit_node()
            .is_disabled());

        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .click();
        harness.key_press(egui::Key::End);
        harness.key_press(egui::Key::Backspace);
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .type_text("c");
        harness.run();
        assert!(harness
            .get_by_label("Compare fingerprints")
            .accesskit_node()
            .is_disabled());
        harness.get_by_label(attestation).click();
        harness.run();
        harness.get_by_label("Compare fingerprints").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::CompareDirectPeer {
                generation: 1,
                fingerprint,
            }) if fingerprint == &format!("{}c", "B".repeat(63))
        ));

        view.retire_for_test();
        harness.step();
        assert!(harness.query_by_label("Compare fingerprints").is_none());
    }

    #[test]
    fn direct_comparison_reclaims_entry_state_when_prompt_changes() {
        use crate::application::WindowId;
        use crate::automation::{verification::View, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            enabled: bool,
        }

        let first = View::for_test(WindowId::PRIMARY);
        let mut snapshot = SettingsSnapshot::default();
        snapshot.direct_verification = Some(first);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 1000.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    let _ =
                        automation_settings(ui, &state.snapshot, state.enabled, WindowId::PRIMARY);
                },
                State {
                    snapshot,
                    enabled: true,
                },
            );
        harness.run();
        let entry = "Fingerprint read from the MCP client";
        let attestation = "I read this complete fingerprint independently in my MCP client";
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .focus();
        harness
            .get_by_role_and_label(accesskit::Role::TextInput, entry)
            .type_text(&"b".repeat(64));
        harness.run();
        assert_eq!(
            harness
                .get_by_role_and_label(accesskit::Role::TextInput, entry)
                .value(),
            Some("b".repeat(64))
        );
        harness.get_by_label(attestation).click();
        harness.run();
        assert_eq!(
            harness.get_by_label(attestation).accesskit_node().toggled(),
            Some(accesskit::Toggled::True)
        );

        let mut second = View::for_test(WindowId::PRIMARY);
        second.generation = 2;
        harness.state_mut().snapshot.direct_verification = Some(second);
        harness.run();
        assert!(harness
            .get_by_label(attestation)
            .accesskit_node()
            .is_disabled());
        assert!(harness
            .get_by_label("Compare fingerprints")
            .accesskit_node()
            .is_disabled());
        assert_eq!(
            harness
                .ctx
                .data(|data| data.count::<egui::text_edit::TextEditState>()),
            1
        );
        harness.state_mut().enabled = false;
        harness.step();
        assert_eq!(
            harness
                .ctx
                .data(|data| data.count::<egui::text_edit::TextEditState>()),
            0
        );
        assert_eq!(
            harness
                .ctx
                .data(|data| data.count::<DirectComparisonInput>()),
            0
        );
    }

    #[test]
    fn controller_settings_require_explicit_current_window_choice_and_confirmation() {
        use crate::application::WindowId;
        use crate::automation::{ControllerSelectionView, NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            window: WindowId,
            command: Option<NativeCommand>,
        }
        let (snapshot, registration, first, second, _, _) =
            SettingsSnapshot::with_controller_for_test();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 1800.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    if let Some(command) =
                        automation_settings(ui, &state.snapshot, true, state.window)
                    {
                        state.command = Some(command);
                    }
                },
                State {
                    snapshot,
                    window: WindowId::PRIMARY,
                    command: None,
                },
            );
        harness.run();
        assert!(harness
            .query_by_label("Confirm association (no access granted)")
            .is_none());
        assert!(harness
            .query_by_label("This window — tab position 2 — untrusted name \"Same\"")
            .is_none());
        harness.get_by_label("Choose local terminal").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ChooseController {
                channel: 7,
                registration: actual,
            }) if *actual == registration
        ));

        harness.state_mut().command = None;
        harness.state_mut().snapshot.controllers[0].selection = ControllerSelectionView::Choosing {
            token: 17,
            window: WindowId::PRIMARY,
        };
        harness.run();
        assert!(harness
            .query_by_label("This window — tab position 3 — untrusted name \"Same\"")
            .is_none());
        harness
            .get_by_label("This window — tab position 2 — untrusted name \"Same\"")
            .click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::SelectControllerTarget {
                channel: 7,
                registration: actual,
                selection: 17,
                target,
            }) if *actual == registration && *target == second && *target != first
        ));

        harness.state_mut().command = None;
        harness.state_mut().snapshot.controllers[0].selection =
            ControllerSelectionView::Reviewing {
                token: 18,
                target: second,
                window: WindowId::PRIMARY,
            };
        harness.state_mut().window = WindowId::for_test(2);
        harness.run();
        assert!(harness
            .query_by_label("Confirm association (no access granted)")
            .is_none());
        harness.state_mut().window = WindowId::PRIMARY;
        harness.run();
        harness
            .get_by_label("Confirm association (no access granted)")
            .click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ConfirmController {
                channel: 7,
                registration: actual,
                selection: 18,
            }) if *actual == registration
        ));
        harness.state_mut().command = None;
        harness.get_by_label("Cancel controller selection").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::CancelController {
                channel: 7,
                registration: actual,
            }) if *actual == registration
        ));
    }

    #[test]
    fn controller_metadata_settings_require_a_separate_same_window_review() {
        use crate::application::WindowId;
        use crate::automation::{ControllerMetadataView, NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            window: WindowId,
            command: Option<NativeCommand>,
        }
        let (mut snapshot, registration, _, target, _, enrollment) =
            SettingsSnapshot::with_controller_for_test();
        snapshot.controllers[0].associated_target = Some(target);
        snapshot.controllers[0].metadata = ControllerMetadataView::Idle {
            enrollment,
            window: WindowId::PRIMARY,
            target,
            granted: false,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 1800.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    if let Some(command) =
                        automation_settings(ui, &state.snapshot, true, state.window)
                    {
                        state.command = Some(command);
                    }
                },
                State {
                    snapshot,
                    window: WindowId::PRIMARY,
                    command: None,
                },
            );
        harness.run();
        assert!(harness
            .query_by_label("Approve this metadata grant")
            .is_none());
        harness.get_by_label("Review metadata access").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ReviewControllerMetadata {
                channel: 7,
                registration: actual,
                enrollment: actual_enrollment,
            }) if *actual == registration && *actual_enrollment == enrollment
        ));

        harness.state_mut().command = None;
        harness.state_mut().snapshot.controllers[0].set_metadata_review_for_test(
            42,
            WindowId::PRIMARY,
            target,
        );
        harness.state_mut().window = WindowId::for_test(2);
        harness.run();
        assert!(harness
            .query_by_label("Approve this metadata grant")
            .is_none());
        harness.state_mut().window = WindowId::PRIMARY;
        harness.run();
        assert!(harness
            .query_by_label_contains("Review metadata for client 0123456789abcdef: tab position 2",)
            .is_some());
        assert!(harness
            .query_by_label_contains("Approve only the terminal label and lifecycle status")
            .is_some());
        harness.get_by_label("Approve this metadata grant").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ConfirmControllerMetadata {
                channel: 7,
                registration: actual,
                review: 42,
            }) if *actual == registration
        ));

        harness.state_mut().command = None;
        let other_window_snapshot = harness.state().snapshot.clone();
        other_window_snapshot.cancel_controller_metadata_review(7, registration, 42);
        harness.run();
        assert!(harness
            .query_by_label("Approve this metadata grant")
            .is_none());
        assert!(harness
            .query_by_label("Metadata review was cancelled.")
            .is_some());
        harness.get_by_label("Cancel metadata review").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::CancelControllerMetadata {
                channel: 7,
                registration: actual,
                review: 42,
            }) if *actual == registration
        ));
    }

    #[test]
    fn controller_input_settings_show_the_review_and_cancel_stale_snapshots() {
        use crate::application::WindowId;
        use crate::automation::{
            presentation::DirectActionDisplay, NativeCommand, SettingsSnapshot,
        };

        struct State {
            snapshot: SettingsSnapshot,
            window: WindowId,
            command: Option<NativeCommand>,
        }
        let (mut snapshot, registration, _, target, _, _) =
            SettingsSnapshot::with_controller_for_test();
        snapshot.controllers[0].associated_target = Some(target);
        snapshot.controllers[0].set_input_requested_for_test(53, WindowId::PRIMARY, target);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 1800.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    if let Some(command) =
                        automation_settings(ui, &state.snapshot, true, state.window)
                    {
                        state.command = Some(command);
                    }
                },
                State {
                    snapshot,
                    window: WindowId::PRIMARY,
                    command: None,
                },
            );
        harness.run();
        harness.get_by_label("Review requested input").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ReviewControllerInput {
                channel: 7,
                registration: actual,
                input: 53,
            }) if *actual == registration
        ));
        harness.state_mut().command = None;
        harness.state_mut().snapshot.controllers[0].set_input_review_for_test(
            53,
            WindowId::PRIMARY,
            target,
            DirectActionDisplay::Text {
                escaped: "long escaped text\\nend".into(),
                submit: true,
            },
            vec!["Enrollment source: exact metadata terminal".into()],
        );
        harness.state_mut().window = WindowId::for_test(2);
        harness.run();
        assert!(harness
            .query_by_label_contains("Text: \"long escaped")
            .is_none());
        harness.state_mut().window = WindowId::PRIMARY;
        harness.run();
        assert!(harness
            .query_by_label_contains("Text: \"long escaped text\\nend\"")
            .is_some());
        assert!(harness
            .query_by_label("Enter will be sent after the text.")
            .is_some());
        assert!(harness
            .query_by_label("Enrollment source: exact metadata terminal")
            .is_some());
        assert!(harness
            .query_by_label_contains("new intentional requests for the same action")
            .is_some());
        assert!(harness
            .query_by_label_contains("Never automatically replay after an unknown or lost response")
            .is_some());
        harness.get_by_label("Approve and send this input").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::ConfirmControllerInput {
                channel: 7,
                registration: actual,
                input: 53,
            }) if *actual == registration
        ));
        harness.state_mut().command = None;
        let other_window_snapshot = harness.state().snapshot.clone();
        other_window_snapshot.cancel_controller_input_review(7, registration, 53);
        harness.run();
        assert!(harness
            .query_by_label_contains("Text: \"long escaped")
            .is_none());
        assert!(harness
            .query_by_label("Approve and send this input")
            .is_none());
        harness.get_by_label("Cancel requested input").click();
        harness.step();
        assert!(matches!(
            &harness.state().command,
            Some(NativeCommand::CancelControllerInput {
                channel: 7,
                registration: actual,
                input: 53,
            }) if *actual == registration
        ));
    }

    #[test]
    fn controller_metadata_review_ignores_stale_pointer_and_accessibility_actions() {
        use crate::application::WindowId;
        use crate::automation::{NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            command: Option<NativeCommand>,
        }
        for accessibility in [false, true] {
            let (mut snapshot, registration, _, target, _, _) =
                SettingsSnapshot::with_controller_for_test();
            snapshot.controllers[0].associated_target = Some(target);
            snapshot.controllers[0].set_metadata_review_for_test(42, WindowId::PRIMARY, target);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 1800.0))
                .build_ui_state(
                    |ui, state: &mut State| {
                        if let Some(command) =
                            automation_settings(ui, &state.snapshot, true, WindowId::PRIMARY)
                        {
                            state.command = Some(command);
                        }
                    },
                    State {
                        snapshot,
                        command: None,
                    },
                );
            harness.run();
            let label = "Approve this metadata grant";
            let position = harness.get_by_label(label).rect().center();
            if accessibility {
                harness.get_by_label(label).click_accesskit();
            } else {
                harness.event(egui::Event::PointerMoved(position));
                harness.event(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
            }
            harness.state_mut().snapshot.controllers[0].set_metadata_review_for_test(
                43,
                WindowId::PRIMARY,
                target,
            );
            if !accessibility {
                harness.event(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
            assert!(harness.state().command.is_none());
            harness.get_by_label(label).click();
            harness.step();
            assert!(matches!(
                &harness.state().command,
                Some(NativeCommand::ConfirmControllerMetadata {
                    channel: 7,
                    registration: actual,
                    review: 43,
                }) if *actual == registration
            ));
        }
    }

    #[test]
    fn controller_input_review_ignores_stale_pointer_and_accessibility_actions() {
        use crate::application::WindowId;
        use crate::automation::{NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            command: Option<NativeCommand>,
        }
        for (accessibility, cancel) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let (mut snapshot, registration, _, target, _, _) =
                SettingsSnapshot::with_controller_for_test();
            snapshot.controllers[0].associated_target = Some(target);
            snapshot.controllers[0].set_input_requested_for_test(53, WindowId::PRIMARY, target);
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 1800.0))
                .build_ui_state(
                    |ui, state: &mut State| {
                        if let Some(command) =
                            automation_settings(ui, &state.snapshot, true, WindowId::PRIMARY)
                        {
                            state.command = Some(command);
                        }
                    },
                    State {
                        snapshot,
                        command: None,
                    },
                );
            harness.run();
            let label = if cancel {
                "Cancel requested input"
            } else {
                "Review requested input"
            };
            let position = harness.get_by_label(label).rect().center();
            if accessibility {
                harness.get_by_label(label).click_accesskit();
            } else {
                harness.event(egui::Event::PointerMoved(position));
                harness.event(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
            }
            harness.state_mut().snapshot.controllers[0].set_input_requested_for_test(
                54,
                WindowId::PRIMARY,
                target,
            );
            if !accessibility {
                harness.event(egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
            assert!(harness.state().command.is_none());
            harness.get_by_label(label).click();
            harness.step();
            let correct = if cancel {
                matches!(
                    &harness.state().command,
                    Some(NativeCommand::CancelControllerInput {
                        channel: 7,
                        registration: actual,
                        input: 54,
                    }) if *actual == registration
                )
            } else {
                matches!(
                    &harness.state().command,
                    Some(NativeCommand::ReviewControllerInput {
                        channel: 7,
                        registration: actual,
                        input: 54,
                    }) if *actual == registration
                )
            };
            assert!(correct);
        }
    }

    #[test]
    fn controller_input_confirmation_and_disarm_ignore_stale_actions() {
        use crate::application::WindowId;
        use crate::automation::{
            presentation::DirectActionDisplay, ControllerArmedControl, NativeCommand,
            SettingsSnapshot,
        };

        struct State {
            snapshot: SettingsSnapshot,
            command: Option<NativeCommand>,
        }
        for disarm in [false, true] {
            for accessibility in [false, true] {
                let (mut snapshot, registration, _, target, _, _) =
                    SettingsSnapshot::with_controller_for_test();
                snapshot.controllers[0].associated_target = Some(target);
                if disarm {
                    snapshot.controllers[0].control = Some(ControllerArmedControl {
                        token: 53,
                        target,
                        window: WindowId::PRIMARY,
                    });
                } else {
                    snapshot.controllers[0].set_input_review_for_test(
                        53,
                        WindowId::PRIMARY,
                        target,
                        DirectActionDisplay::Text {
                            escaped: "input".into(),
                            submit: false,
                        },
                        vec!["source".into()],
                    );
                }
                let mut harness = Harness::builder()
                    .with_size(egui::vec2(900.0, 1800.0))
                    .build_ui_state(
                        |ui, state: &mut State| {
                            if let Some(command) =
                                automation_settings(ui, &state.snapshot, true, WindowId::PRIMARY)
                            {
                                state.command = Some(command);
                            }
                        },
                        State {
                            snapshot,
                            command: None,
                        },
                    );
                harness.run();
                let label = if disarm {
                    "Disarm terminal control"
                } else {
                    "Approve and send this input"
                };
                let position = harness.get_by_label(label).rect().center();
                if accessibility {
                    harness.get_by_label(label).click_accesskit();
                } else {
                    harness.event(egui::Event::PointerMoved(position));
                    harness.event(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                    harness.step();
                }
                if disarm {
                    harness.state_mut().snapshot.controllers[0]
                        .control
                        .as_mut()
                        .unwrap()
                        .token = 54;
                } else {
                    harness.state_mut().snapshot.controllers[0].set_input_review_for_test(
                        54,
                        WindowId::PRIMARY,
                        target,
                        DirectActionDisplay::Text {
                            escaped: "input".into(),
                            submit: false,
                        },
                        vec!["source".into()],
                    );
                }
                if !accessibility {
                    harness.event(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                harness.step();
                assert!(harness.state().command.is_none());
                harness.get_by_label(label).click();
                harness.step();
                if disarm {
                    assert!(matches!(
                        &harness.state().command,
                        Some(NativeCommand::DisarmControllerInput {
                            channel: 7,
                            registration: actual,
                            input: 54,
                        }) if *actual == registration
                    ));
                } else {
                    assert!(matches!(
                        &harness.state().command,
                        Some(NativeCommand::ConfirmControllerInput {
                            channel: 7,
                            registration: actual,
                            input: 54,
                        }) if *actual == registration
                    ));
                }
            }
        }
    }

    #[test]
    fn controller_confirmation_ignores_stale_accessibility_and_pointer_actions() {
        use crate::application::WindowId;
        use crate::automation::{ControllerSelectionView, NativeCommand, SettingsSnapshot};

        struct State {
            snapshot: SettingsSnapshot,
            command: Option<NativeCommand>,
        }

        for accessibility in [false, true] {
            for same_target in [false, true] {
                let (mut snapshot, registration, first, second, _, _) =
                    SettingsSnapshot::with_controller_for_test();
                snapshot.controllers[0].selection = ControllerSelectionView::Reviewing {
                    token: 18,
                    target: first,
                    window: WindowId::PRIMARY,
                };
                let mut harness = Harness::builder()
                    .with_size(egui::vec2(900.0, 1800.0))
                    .build_ui_state(
                        |ui, state: &mut State| {
                            if let Some(command) =
                                automation_settings(ui, &state.snapshot, true, WindowId::PRIMARY)
                            {
                                state.command = Some(command);
                            }
                        },
                        State {
                            snapshot,
                            command: None,
                        },
                    );
                harness.run();
                let label = "Confirm association (no access granted)";
                let position = harness.get_by_label(label).rect().center();
                if accessibility {
                    harness.get_by_label(label).click_accesskit();
                } else {
                    harness.event(egui::Event::PointerMoved(position));
                    harness.event(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    });
                    harness.step();
                }

                harness.state_mut().snapshot.controllers[0].selection =
                    ControllerSelectionView::Reviewing {
                        token: 20,
                        target: if same_target { first } else { second },
                        window: WindowId::PRIMARY,
                    };
                if !accessibility {
                    harness.event(egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                harness.step();
                assert!(
                    harness.state().command.is_none(),
                    "a stale action confirmed the replacement review"
                );

                harness.get_by_label(label).click_accesskit();
                harness.step();
                assert!(matches!(
                    &harness.state().command,
                    Some(NativeCommand::ConfirmController {
                        channel: 7,
                        registration: actual,
                        selection: 20,
                    }) if *actual == registration
                ));
            }
        }
    }

    #[test]
    fn automation_toggle_release_cannot_reenable_a_changed_preference() {
        for label in [
            "Enable local MCP (Beta)",
            "Enable JavaScript workflows (Beta)",
        ] {
            let mut harness = settings_harness();
            harness.state_mut().mcp_enabled = true;
            harness.state_mut().javascript_enabled = true;
            harness.run();
            let position = harness
                .get_by_role_and_label(accesskit::Role::CheckBox, label)
                .rect()
                .center();
            harness.event(egui::Event::PointerMoved(position));
            harness.event(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            if label == "Enable local MCP (Beta)" {
                harness.state_mut().mcp_enabled = false;
            } else {
                harness.state_mut().javascript_enabled = false;
            }
            harness.event(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            assert!(harness.state().command.is_none());

            harness
                .get_by_role_and_label(accesskit::Role::CheckBox, label)
                .click();
            harness.step();
            assert!(matches!(
                (&harness.state().command, label),
                (
                    Some(AppCommand::ToggleMcpEnabled),
                    "Enable local MCP (Beta)"
                ) | (
                    Some(AppCommand::ToggleJavascriptEnabled),
                    "Enable JavaScript workflows (Beta)"
                )
            ));
        }
    }

    #[test]
    fn automation_toggle_release_cannot_reenable_after_same_pass_aba() {
        use crate::automation::preferences::{Feature, Shared, Values};

        for (label, feature) in [
            ("Enable local MCP (Beta)", Feature::Mcp),
            ("Enable JavaScript workflows (Beta)", Feature::Javascript),
        ] {
            let preferences = Shared::new(Values {
                mcp: false,
                javascript: false,
            });
            let mut harness = settings_harness();
            harness.run();
            let position = harness
                .get_by_role_and_label(accesskit::Role::CheckBox, label)
                .rect()
                .center();
            harness.event(egui::Event::PointerMoved(position));
            harness.event(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();

            assert!(preferences.toggle(feature).is_some());
            assert!(preferences.toggle(feature).is_some());
            let revision = preferences.ui_revision(feature);
            match feature {
                Feature::Mcp => harness.state_mut().mcp_ui_revision = revision,
                Feature::Javascript => harness.state_mut().javascript_ui_revision = revision,
            }
            harness.event(egui::Event::PointerButton {
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.step();
            assert!(harness.state().command.is_none());

            harness
                .get_by_role_and_label(accesskit::Role::CheckBox, label)
                .click();
            harness.step();
            assert!(matches!(
                (&harness.state().command, feature),
                (Some(AppCommand::ToggleMcpEnabled), Feature::Mcp)
                    | (
                        Some(AppCommand::ToggleJavascriptEnabled),
                        Feature::Javascript
                    )
            ));
        }
    }

    #[test]
    fn settings_automation_conflicts_stop_before_approval_or_unrelated_actions() {
        use crate::automation::NativeCommand;

        #[derive(Clone, Copy, Debug)]
        enum Action {
            Javascript,
            Mcp,
            OpenWorkflow,
            ClearWorkflow,
            ResetSettings,
            Retry,
            Approve,
            Revoke,
            ConfirmController,
            CancelController,
        }
        let (_, registration, _, _, _, _) =
            crate::automation::SettingsSnapshot::with_controller_for_test();
        let make = |action| match action {
            Action::Javascript => AppCommand::ToggleJavascriptEnabled,
            Action::Mcp => AppCommand::ToggleMcpEnabled,
            Action::OpenWorkflow => AppCommand::OpenWorkflowPicker,
            Action::ClearWorkflow => AppCommand::Automation(NativeCommand::ClearWorkflow),
            Action::ResetSettings => AppCommand::ResetAllSettings,
            Action::Retry => AppCommand::RetryAutomationSettings,
            Action::Approve => AppCommand::Automation(NativeCommand::ApproveObservation {
                channel: 7,
                targets: Vec::new(),
            }),
            Action::Revoke => AppCommand::Automation(NativeCommand::Revoke { channel: 7 }),
            Action::ConfirmController => AppCommand::Automation(NativeCommand::ConfirmController {
                channel: 7,
                registration,
                selection: 9,
            }),
            Action::CancelController => AppCommand::Automation(NativeCommand::CancelController {
                channel: 7,
                registration,
            }),
        };
        for (first, second) in [
            (Action::Javascript, Action::Approve),
            (Action::Javascript, Action::ClearWorkflow),
            (Action::Javascript, Action::Mcp),
            (Action::ClearWorkflow, Action::Approve),
            (Action::OpenWorkflow, Action::ClearWorkflow),
            (Action::ResetSettings, Action::Approve),
            (Action::Retry, Action::Approve),
            (Action::Revoke, Action::Approve),
            (Action::Javascript, Action::ConfirmController),
            (Action::Mcp, Action::ConfirmController),
            (Action::CancelController, Action::ConfirmController),
        ] {
            for (first, second) in [(first, second), (second, first)] {
                let mut commands = SettingsAutomationCommands::default();
                commands.record(make(first));
                commands.record(make(second));
                commands.record(make(Action::Approve));
                assert!(
                    matches!(
                        commands.resolve(Some(AppCommand::ToggleStatusBar)),
                        Some(AppCommand::Automation(NativeCommand::SafetyStop))
                    ),
                    "{first:?} followed by {second:?} must stop, even with later actions"
                );
            }
        }
        for action in [
            Action::Javascript,
            Action::Mcp,
            Action::ClearWorkflow,
            Action::ResetSettings,
            Action::Retry,
            Action::Revoke,
        ] {
            let mut commands = SettingsAutomationCommands::default();
            commands.record(make(action));
            assert!(!matches!(
                commands.resolve(Some(AppCommand::ToggleStatusBar)),
                Some(AppCommand::ToggleStatusBar) | None
            ));
        }
        let mut commands = SettingsAutomationCommands::default();
        commands.record(make(Action::OpenWorkflow));
        assert!(matches!(
            commands.resolve(Some(AppCommand::ToggleStatusBar)),
            Some(AppCommand::ToggleStatusBar)
        ));
        assert!(matches!(
            SettingsAutomationCommands::default().resolve(Some(AppCommand::ToggleStatusBar)),
            Some(AppCommand::ToggleStatusBar)
        ));

        let mut actions = None;
        record_automation_action(&mut actions, NativeCommand::Revoke { channel: 7 });
        record_automation_action(
            &mut actions,
            NativeCommand::ApproveObservation {
                channel: 7,
                targets: Vec::new(),
            },
        );
        assert!(matches!(actions, Some(NativeCommand::SafetyStop)));
    }

    #[test]
    fn uncertain_automation_preferences_show_warning_and_safe_retry() {
        use crate::automation::preferences::{Shared, Values};

        let enabled = Values {
            mcp: true,
            javascript: true,
        };
        let preferences = Shared::new(enabled);
        preferences.save_failed(enabled);
        let mut harness = settings_harness();
        harness.state_mut().mcp_enabled = true;
        harness.state_mut().javascript_enabled = true;
        harness.state_mut().automation_stopped = true;
        harness.state_mut().automation_preference_feedback = preferences.feedback();
        harness.state_mut().automation_preference_retry_available = preferences.retry_available();
        harness.run();
        assert!(harness
            .query_by_label_contains("may remain on disk and be read after restart")
            .is_some());
        assert!(harness.query_by_label("Select workflow JSON").is_none());
        assert!(harness
            .query_by_label("Restart local MCP (revokes MCP access)")
            .is_none());
        harness
            .get_by_label("Retry saving automation preferences (stay stopped)")
            .click();
        harness.step();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::RetryAutomationSettings)
        ));
    }

    #[test]
    fn conflicting_automation_actions_are_visible_in_settings() {
        use crate::automation::preferences::{Shared, Values};

        let preferences = Shared::new(Values {
            mcp: true,
            javascript: true,
        });
        preferences.action_conflicted();
        let mut harness = settings_harness();
        harness.state_mut().automation_preference_feedback = preferences.feedback();
        harness.run();
        assert!(harness
            .query_by_label_contains("conflicted or overflowed")
            .is_some());
        assert!(harness
            .query_by_label_contains("workflow previews cancelled")
            .is_some());
    }

    #[test]
    fn settings_font_download_failure_offers_an_explicit_typed_retry() {
        let mut harness = settings_harness();
        harness.state_mut().font_download_failed = true;
        harness.run();
        assert!(harness
            .query_by_label_contains("Using JetBrains Mono temporarily")
            .is_some());
        harness.get_by_label("Retry font download").click();
        harness.step();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::RetryTerminalFontDownload)
        ));
    }

    #[test]
    fn settings_has_no_manual_reload_or_save_controls() {
        // Regression test: Settings used to offer explicit "Reload
        // configuration"/"Save workspace" buttons; configuration now
        // save/restores automatically, so neither control (nor their
        // explanatory copy) should be present any more.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Reload configuration").is_none());
        assert!(harness.query_by_label("Save workspace").is_none());
        assert!(harness
            .query_by_label("Configuration is never written automatically.")
            .is_none());
        assert!(harness
            .query_by_label("Chip layout and status bar visibility are saved automatically.")
            .is_none());
    }

    #[test]
    fn settings_has_no_configuration_card() {
        // Regression test: the "Configuration" card (startup/save status
        // copy plus native-secure-storage status) was removed from
        // Settings; that status is not shown here any more (secure storage
        // status already surfaces on the Launcher instead).
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Configuration").is_none());
        assert!(harness.query_by_label("Native secure storage").is_none());
    }

    #[test]
    fn settings_explains_the_fixed_terminal_mouse_override() {
        let mut harness = wide_settings_harness();
        harness.run();
        assert!(harness
            .query_by_label("Shift+right-click opens the fesTerm menu")
            .is_some());
        assert!(
            harness.state().command.is_none(),
            "help must not change settings"
        );
    }

    fn open_settings_page(harness: &mut Harness<'static, SettingsHarnessState>, title: &str) {
        harness
            .get_by_role_and_label(accesskit::Role::Button, title)
            .click();
        harness.run();
    }

    fn focused_button(harness: &Harness<'static, SettingsHarnessState>, label: &str) -> bool {
        harness
            .get_by_role_and_label(accesskit::Role::Button, label)
            .accesskit_node()
            .is_focused()
    }

    fn on_keyboard_shortcuts_page(harness: &Harness<'static, SettingsHarnessState>) -> bool {
        harness.query_by_label("Search actions…").is_some()
            && harness.query_by_label(RESET_ALL_SETTINGS_LABEL).is_none()
    }

    #[test]
    fn settings_presents_each_shortcut_exactly_once() {
        // Regression test: a read-only "Keyboard" card used to restate the
        // command palette and Settings shortcuts that the bindings editor
        // already lists, so a customized chord could be shown twice with two
        // different values. The editor now lives on its own sub-page and the
        // main page only summarizes it.
        let mut harness = wide_settings_harness();
        harness.run();
        assert_eq!(harness.query_all_by_label("Open Settings").count(), 0);
        assert!(harness
            .query_by_label_contains("All defaults · Ctrl+Shift+F12 always reopens Settings")
            .is_some());

        open_settings_page(&mut harness, "Keyboard shortcuts");
        assert_eq!(
            harness.query_all_by_label("Open Settings").count(),
            1,
            "each action must appear once, in the bindings editor"
        );
        assert_eq!(harness.query_all_by_label("Command palette").count(), 1);
    }

    #[test]
    fn settings_main_page_sections_follow_the_documented_order() {
        let mut harness = wide_settings_harness();
        harness.run();

        let mut previous: Option<(&str, f32)> = None;
        for section in SETTINGS_SECTION_ORDER {
            // Headed cards expose their uppercase heading; drill-in sections
            // are a single button named by the section.
            let top = harness
                .query_by_label(&section.to_uppercase())
                .map(|node| node.rect().top())
                .unwrap_or_else(|| {
                    harness
                        .get_by_role_and_label(accesskit::Role::Button, section)
                        .rect()
                        .top()
                });
            if let Some((earlier, earlier_top)) = previous {
                assert!(earlier_top < top, "{earlier} must appear before {section}");
            }
            previous = Some((section, top));
        }
        for retired in [
            "INTERFACE",
            "SCROLLING",
            "TERMINAL TYPOGRAPHY",
            "QUICK SWITCH",
            "SFTP",
            "KEYBOARD BINDINGS",
            "DEV TUNNEL ACCOUNTS",
            "DEV TUNNEL OPTIONS",
        ] {
            assert!(
                harness.query_by_label(retired).is_none(),
                "{retired} was merged into a named section"
            );
        }
    }

    #[test]
    fn settings_sub_pages_open_and_return_by_back_or_escape_to_their_row() {
        let mut harness = wide_settings_harness();
        harness.run();

        open_settings_page(&mut harness, "Keyboard shortcuts");
        assert!(on_keyboard_shortcuts_page(&harness));
        assert!(
            focused_button(&harness, "Back"),
            "Back takes focus on entry"
        );
        assert!(harness.query_by_label("Keyboard shortcuts").is_some());

        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.query_by_label("TERMINAL").is_some());
        assert!(
            focused_button(&harness, "Keyboard shortcuts"),
            "returning focuses the row the page was opened from"
        );

        open_settings_page(&mut harness, "Automation activity");
        assert!(harness.query_by_label("JAVASCRIPT WORKFLOWS").is_some());
        assert!(harness.query_by_label("LOCAL MCP").is_some());
        assert!(harness.query_by_label("TERMINAL").is_none());
        harness.get_by_label("Back").click();
        harness.run();
        assert!(harness.query_by_label("TERMINAL").is_some());
        assert!(focused_button(&harness, "Automation activity"));
        assert!(
            harness.state().command.is_none(),
            "navigation is view state"
        );
    }

    #[test]
    fn settings_recovery_chord_lands_on_the_shortcut_search() {
        // Ctrl+Shift+F12 opens Settings and asks the keyboard editor to focus
        // its search; with the editor on a sub-page, Settings has to go there.
        let mut harness = wide_settings_harness();
        harness.run();
        harness
            .ctx
            .data_mut(|data| data.insert_temp(egui::Id::new("keyboard-settings-recovery"), true));
        harness.run();
        harness.run();
        assert!(on_keyboard_shortcuts_page(&harness));
        assert!(harness
            .get_by_label("Search actions…")
            .accesskit_node()
            .is_focused());
    }

    #[test]
    fn settings_escape_first_belongs_to_a_focused_field_or_a_chord_capture() {
        let mut harness = wide_settings_harness();
        harness.run();
        open_settings_page(&mut harness, "Keyboard shortcuts");

        harness.get_by_label("Search actions…").click();
        harness.run();
        assert!(harness.ctx.text_edit_focused());
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(
            on_keyboard_shortcuts_page(&harness),
            "Escape leaves the search field first"
        );

        harness
            .get_by_role_and_label(accesskit::Role::Button, "New Session")
            .click();
        harness.run();
        harness.get_by_label("Record shortcut").click();
        harness.run();
        assert!(crate::keyboard::recording(&harness.ctx));
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(
            on_keyboard_shortcuts_page(&harness),
            "Escape during a chord capture never leaves the page"
        );
    }

    #[test]
    fn settings_reset_is_labelled_for_everything_it_resets() {
        let mut harness = wide_settings_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::Button, RESET_ALL_SETTINGS_LABEL)
            .click();
        harness.step();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ResetAllSettings)
        ));
        assert!(RESET_ALL_SETTINGS_CATEGORIES
            .iter()
            .any(|category| category.starts_with("Keyboard shortcuts")));
        assert!(RESET_ALL_SETTINGS_NOT_AFFECTED.contains("Recording settings"));
    }

    #[test]
    fn settings_default_on_preferences_say_so() {
        let defaults = festerm_config::InterfaceSettings::DEFAULT;
        assert!(defaults.restore_workspace());
        assert!(defaults.compact_launcher_grid());
        assert!(defaults.show_resumable_sessions());

        let mut harness = wide_settings_harness();
        harness.run();
        for sentence in [
            "the active tab automatically on launch. On by default.",
            "Card descriptions are kept. On by default.",
            "on the New Session tab. On by default.",
        ] {
            assert!(
                harness.query_by_label_contains(sentence).is_some(),
                "{sentence:?} must say it is on by default"
            );
        }
    }

    #[test]
    fn settings_dependent_rows_follow_their_parent_preference() {
        // The durable-session name only shows in the status bar, and the
        // extension length only matters while extension is on.
        let mut harness = wide_settings_harness();
        harness.run();
        assert!(!harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Show durable session name in status bar",
            )
            .accesskit_node()
            .is_disabled());
        assert!(harness.query_by_label("Extension length").is_none());

        let mut harness = Harness::builder()
            .with_size(egui::vec2(600.0, 200.0))
            .build_ui_state(
                |ui, clicked: &mut bool| {
                    *clicked |=
                        settings_toggle_row_enabled(ui, "Child", "Needs parent.", false, false);
                },
                false,
            );
        harness.run();
        let toggle = harness.get_by_role_and_label(accesskit::Role::CheckBox, "Child");
        assert!(toggle.accesskit_node().is_disabled());
        toggle.click();
        harness.run();
        assert!(!*harness.state(), "a disabled row cannot be toggled");
    }

    #[test]
    fn settings_segmented_options_read_and_tab_left_to_right() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(800.0, 200.0))
            .build_ui(|ui| {
                let _ = settings_segmented_row(
                    ui,
                    "Order",
                    "Options must read left to right.",
                    &[("First", true), ("Second", false), ("Third", false)],
                );
            });
        harness.run();
        let x = |label| harness.get_by_label(label).rect().left();
        assert!(x("First") < x("Second") && x("Second") < x("Third"));
    }

    #[test]
    fn settings_cards_and_panels_share_one_gap() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(600.0, 800.0))
            .build_ui_state(
                |ui, rects: &mut Vec<egui::Rect>| {
                    rects.clear();
                    rects.push(settings_card(ui, "One", |ui| {
                        ui.label("Body");
                    }));
                    rects.push(settings_panel(ui, |ui| {
                        ui.label("Panel");
                    }));
                    rects.push(settings_card(ui, "Two", |ui| {
                        ui.label("Body");
                    }));
                    rects.push(settings_card(ui, "Three", |ui| {
                        ui.label("Body");
                    }));
                },
                Vec::new(),
            );
        harness.run();
        let rects = harness.state();
        let gaps: Vec<f32> = rects
            .windows(2)
            .map(|pair| pair[1].top() - pair[0].bottom())
            .collect();
        assert!(gaps[0] >= SETTINGS_CARD_GAP);
        assert!(
            gaps.iter().all(|gap| (gap - gaps[0]).abs() < 0.5),
            "every card is followed by the same gap: {gaps:?}"
        );
    }

    #[test]
    fn settings_cards_fill_the_same_available_width() {
        let mut harness = Harness::builder()
            .with_size(egui::vec2(640.0, 320.0))
            .build_ui_state(
                |ui, widths: &mut Option<(f32, f32)>| {
                    let short = settings_card_response(ui, "Short", |ui| {
                        ui.label("Short content");
                    })
                    .response
                    .rect
                    .width();
                    ui.add_space(12.0);
                    let long = settings_card_response(ui, "Long", |ui| {
                        ui.label("Long content that would otherwise determine a wider card");
                    })
                    .response
                    .rect
                    .width();
                    *widths = Some((short, long));
                },
                None,
            );
        harness.run();

        let (short, long) = harness.state().expect("both settings cards render");
        assert!(
            (short - long).abs() < 0.1,
            "card widths differ: {short} vs {long}"
        );
    }

    #[test]
    fn settings_toggle_chip_layout_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        // The harness starts in `ChipLayout::Wrap`; clicking the *other*,
        // currently-inactive segmented option ("Single row") is what
        // selects a new value. Clicking the already-active "Wrap" option
        // is a no-op, matching ordinary segmented-control behavior.
        harness.get_by_label("Single row").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleChipLayout)
        ));
    }

    #[test]
    fn settings_toggle_restore_workspace_control_returns_the_toggle_command() {
        // Regression test for the "Workspace restore" preference: off by
        // default, with its own explicit toggle distinct from the
        // always-autosaving chip-layout/status-bar/session-detail toggles.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleRestoreWorkspace)
        ));
    }

    #[test]
    fn settings_toggle_compact_launcher_grid_control_returns_the_toggle_command() {
        // Regression test for the "Compact New Session layout" preference
        // (feature request #64): on by default, with its own explicit
        // toggle in the Startup & sessions card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, "Compact New Session layout")
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Compact New Session layout")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleCompactLauncherGrid)
        ));
    }

    #[test]
    fn settings_toggle_customize_local_shell_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Customize local shell before launch",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleCustomizeLocalShell)
        ));
    }

    #[test]
    fn settings_auto_applies_default_sftp_local_directory_edits() {
        let mut harness = settings_harness();
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(directory.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &directory
        ));
    }

    #[test]
    fn settings_sftp_pane_order_control_dispatches_the_selected_preference() {
        let mut harness = settings_harness();
        harness.run();

        harness
            .get_by_role_and_label(accesskit::Role::RadioButton, "Remote left · Local right")
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetSftpPaneOrder(
                SftpPaneOrderPreference::RemoteLeft
            ))
        ));
    }

    #[test]
    fn settings_accept_missing_default_sftp_local_directory_metadata() {
        let mut harness = settings_harness();
        let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("does-not-exist-default-sftp-local-directory");
        harness.run();

        harness.get_by_label("Default local SFTP directory").focus();
        harness
            .get_by_label("Default local SFTP directory")
            .type_text(missing.to_string_lossy().as_ref());
        harness.run();

        assert!(matches!(
            harness.state().command.as_ref(),
            Some(AppCommand::SetDefaultSftpLocalDirectory(Some(path))) if path == &missing
        ));
        assert!(
            harness
                .query_by_label("Default local SFTP directory must not contain control characters.")
                .is_none(),
            "ordinary path metadata must not show inline validation errors"
        );
    }

    #[test]
    fn settings_omit_the_retired_unread_pulse_toggle() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output"
            )
            .is_none());
    }

    /// The default window is `DEFAULT_WINDOW_WIDTH` wide (see `main.rs`), and
    /// every surface has to be usable there. `settings_segmented_row` used to
    /// reserve a fixed 170px for its buttons; "Scrollback limit"'s four
    /// options need roughly 240, and the overflow does not clip the buttons -
    /// egui grows the enclosing card instead. The Scrolling card was offered
    /// 684px and painted 754.5, and because each following card then inherited
    /// that wider content width, the Terminal font dropdown and the
    /// scroll-speed slider were pushed off the right edge of the window.
    #[test]
    fn settings_controls_stay_inside_their_card_at_the_default_window_width() {
        let mut harness = settings_harness_with_width(crate::DEFAULT_WINDOW_WIDTH);
        harness.run();

        // A toggle row's switch is pinned to the card's right edge and its
        // right side is narrow enough that it never forced the card wider,
        // so it marks where every other control should stop.
        let card_right = harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .rect()
            .right();

        let mut controls = vec![
            (
                "the scrollback-limit segmented control".to_owned(),
                harness.get_by_label("Disabled").rect().right(),
            ),
            (
                "the scroll-speed slider".to_owned(),
                harness.get_by_role(accesskit::Role::Slider).rect().right(),
            ),
        ];
        // Every dropdown, including the keyboard-binding filter, has to stay
        // inside the same card edge as the toggle rows.
        controls.extend(
            harness
                .query_all_by_role(accesskit::Role::ComboBox)
                .enumerate()
                .map(|(index, node)| (format!("dropdown {index}"), node.rect().right()))
                .collect::<Vec<_>>(),
        );
        assert!(
            controls.len() >= 4,
            "expected the font and keyboard-filter dropdowns to be present"
        );

        for (what, right) in controls {
            assert!(
                right <= card_right + 1.0,
                "{what} reaches {right}, past the {card_right} right edge the \
                 toggle rows line up on, so its card is wider than the window"
            );
        }
    }

    /// Regression test for the scroll-speed slider rendering as a small empty
    /// box with no visible track. egui paints a slider rail with
    /// `widgets.inactive.bg_fill`, and `theme::default_visuals` sets that to
    /// `SURFACE_TAB_INACTIVE` - byte-for-byte the fill `settings_card` uses -
    /// so the rail disappeared into the card and left only the handle's
    /// one-pixel outline on screen.
    #[test]
    fn a_settings_slider_rail_is_visible_against_the_card_it_sits_in() {
        let mut observed = None;
        egui::__run_test_ui(|ui| {
            ui.style_mut().visuals = theme::default_visuals();
            style_settings_slider(ui);
            observed = Some((
                ui.visuals().widgets.inactive.bg_fill,
                ui.visuals().selection.bg_fill,
            ));
        });

        let (rail, travelled) = observed.expect("the test ui body should have run");
        assert_ne!(
            rail,
            theme::SURFACE_TAB_INACTIVE,
            "the slider rail is painted in the same color as the settings card \
             around it, so the slider renders as a bare floating handle"
        );
        assert_eq!(
            travelled,
            theme::ACCENT_PRIMARY,
            "the travelled part of the rail should use the same accent the \
             toggle switches use for 'on'"
        );
    }

    #[test]
    fn scroll_speed_slider_is_reachable_and_dispatches_the_next_clickstop() {
        // Regression test for `settings_clickstop_row` rendering the
        // "Scroll speed" slider unusably: unlike `ui.horizontal` (used by
        // `settings_segmented_row`), plain `ui.vertical` does not mirror
        // `egui::Sides`' right-to-left direction (see `egui::Ui::horizontal`,
        // which checks `placer.prefer_right_to_left()`, versus `ui.vertical`,
        // which always lays out `Layout::top_down(Align::Min)`). A bare
        // `ui.vertical(...)` on the right side inherited the *entire*
        // remaining card width and then left-aligned the slider inside it,
        // so on any card wider than description-text-plus-slider the block
        // rendered immediately after the description paragraph instead of
        // pinned to the card's right edge like every other settings row -
        // squeezing the slider down to a tiny hit target and spilling the
        // value label over the description (reported: "you can't tell it's
        // actually a slider" and "sliding the value doesn't seem to change
        // scroll speed"). A width at least as wide as `docs/gui-mockups`'
        // settings card is required to reproduce this: the bug was invisible
        // at the narrow fixed-size harness width used by the other settings
        // tests here.
        let mut harness = wide_settings_harness();
        harness.run();

        let slider = harness.get_by_role(accesskit::Role::Slider);
        let card_right_edge = harness
            .get_by_role_and_label(accesskit::Role::CheckBox, "Workspace restore")
            .rect()
            .right();
        assert!(
            slider.rect().width() >= 100.0,
            "expected the clickstop slider to render at its configured width, got {:?}",
            slider.rect()
        );
        assert!(
            (slider.rect().right() - card_right_edge).abs() <= 40.0,
            "expected the slider to be pinned to the card's right edge like every \
             other settings control, but it rendered at {:?} while the card's \
             right edge is at {card_right_edge}",
            slider.rect()
        );

        slider.focus();
        harness.run();
        harness.key_press(egui::Key::ArrowRight);
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollSpeed(ScrollSpeedPreference::Fast))
        ));
    }

    #[test]
    fn settings_toggle_show_resumable_sessions_control_returns_the_toggle_command() {
        // Regression test for the "Resume unattached local sessions from
        // New Session" preference (feature request #70): on by default,
        // with its own explicit toggle in the Startup & sessions card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Resume unattached local sessions from New Session",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleShowResumableSessions)
        ));
    }

    #[test]
    fn settings_automatic_update_check_control_returns_the_toggle_command() {
        // The background release poll is a preference, not a hidden
        // behaviour: it is reachable and reversible from Settings.
        let mut harness = settings_harness();
        harness.run();

        let label = "Check for fesTerm updates automatically";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleAutomaticUpdateChecks)
        ));
    }

    #[test]
    fn settings_toggle_durable_session_status_bar_control_returns_the_toggle_command() {
        // Feature request #168: the durable-session status-bar item has its
        // own preference, deliberately not folded into "Show session details
        // in chips" -- identity and detail are different questions.
        let mut harness = settings_harness();
        harness.run();

        let label = "Show durable session name in status bar";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleDurableSessionInStatusBar)
        ));
    }

    #[test]
    fn settings_close_confirmation_control_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        let label = "Confirm before closing live sessions";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());

        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleConfirmSessionClose)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn settings_powershell_preference_returns_the_toggle_command() {
        let mut harness = settings_harness();
        harness.run();

        let label = "Prefer PowerShell when available";
        assert!(harness
            .query_by_role_and_label(accesskit::Role::CheckBox, label)
            .is_some());
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, label)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::TogglePreferPowershell)
        ));
    }

    #[test]
    fn settings_exposes_terminal_font_ligature_and_emoji_controls() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Terminal font").is_some());
        let ligatures = "Programming ligatures";
        harness
            .get_by_role_and_label(accesskit::Role::CheckBox, ligatures)
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::ToggleTerminalLigatures)
        ));
        assert!(harness.query_by_label("Emoji presentation").is_some());

        harness.get_by_label("Monochrome").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetEmojiPresentation(
                EmojiPresentationPreference::Monochrome
            ))
        ));
    }

    #[test]
    fn image_budget_settings_dropdown_dispatches_the_typed_bounded_preference() {
        let mut harness = settings_harness();
        harness.run();
        harness
            .get_by_role_and_label(accesskit::Role::ComboBox, "Image memory budget")
            .click();
        harness.run();
        harness.get_by_label("128 MiB").click();
        harness.run();
        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetImageMemoryBudget(
                ImageMemoryBudgetPreference::MiB128
            ))
        ));
    }

    #[test]
    fn settings_exposes_scrollback_limit_for_future_sessions() {
        let mut harness = settings_harness();
        harness.run();

        assert!(harness.query_by_label("Scrollback limit").is_some());
        harness.get_by_label("16 MiB").click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::SetScrollbackLimit(
                ScrollbackLimitPreference::MiB16
            ))
        ));
    }

    #[test]
    fn settings_panel_does_not_overlap_a_visible_bottom_status_bar() {
        // Regression test: the card-based Settings redesign is taller than
        // the old flat button list, so without a status-bar-aware height
        // clamp (mirroring the SSH profile editor panel's), its content
        // could paint into - or past - the bottom status bar strip. The
        // window here is tall enough for all of Settings' content to fit
        // without needing to scroll, so every widget's rect should stay
        // above the status bar; a shorter window would legitimately push
        // some content below the fold (inside the scrollable area) without
        // that being a bug, which a naive per-widget position check can't
        // distinguish from actually overlapping the status bar.
        //
        // The height has to track the content: this fixture is only 520
        // wide, and once `settings_segmented_row` began reserving its
        // buttons' real width the descriptions beside them wrap one line
        // further at that width, making the whole surface taller. Match
        // the full-content fixture above so the extra Windows controls and
        // fixed mouse-gesture help do not put this widget below the fold.
        let mut harness = Harness::builder()
            .with_size(egui::vec2(520.0, 5200.0))
            .build_ui_state(
                |ui, state: &mut SettingsHarnessState| {
                    egui::Panel::bottom("status_bar")
                        .resizable(false)
                        .show_separator_line(false)
                        .show(ui, |ui| {
                            ui.set_min_height(24.0);
                            ui.set_max_height(24.0);
                        });
                    if let Some(command) = show_settings(
                        ui,
                        SettingsViewModel {
                            #[cfg(feature = "local-host-settings")]
                            local_remote_access: Default::default(),
                            #[cfg(feature = "local-host-settings")]
                            local_share_targets: Vec::new(),
                            keyboard_bindings: Default::default(),
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            show_pane_handles: false,
                            pane_high_contrast: false,
                            confirm_session_close: true,
                            prefer_powershell: true,
                            extend_dev_tunnel_lifetime: false,
                            show_running_dev_tunnels: false,
                            dev_tunnel_extension_days: 30,
                            dev_tunnel_accounts: Vec::new(),
                            customize_local_shell: false,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            font_download_status: None,
                            font_download_failed: false,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            rectangle_copy_trim_trailing_spaces: true,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            image_memory_budget: ImageMemoryBudgetPreference::MiB512,
                            image_memory_over_budget: false,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            show_resumable_sessions: false,
                            show_durable_session_in_status_bar: false,
                            automatic_update_checks: false,
                            mcp_enabled: false,
                            javascript_enabled: false,
                            automation_mcp_enabled: false,
                            automation_javascript_enabled: false,
                            automation_mcp_ui_revision: 0,
                            automation_javascript_ui_revision: 0,
                            automation_preference_feedback: None,
                            automation_preference_retry_available: false,
                            automation_window: crate::application::WindowId::PRIMARY,
                            automation: Default::default(),
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                            session_recording_enabled: false,
                            recording_preference_unsaved: false,
                        },
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState {
                    command: None,
                    keyboard_bindings: Default::default(),
                    font_download_failed: false,
                    mcp_enabled: false,
                    javascript_enabled: false,
                    mcp_ui_revision: 0,
                    javascript_ui_revision: 0,
                    automation_stopped: false,
                    automation_preference_feedback: None,
                    automation_preference_retry_available: false,
                },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let last_control_rect = harness.get_by_label(RESET_ALL_SETTINGS_LABEL).rect();
        assert!(
            last_control_rect.max.y <= status_bar_top,
            "Settings content must stay above the status bar rather than overlapping it"
        );
    }
}
