//! Settings application-surface presentation, split out of `screens.rs`.

use std::path::PathBuf;

use eframe::egui::{self, Sense, Stroke, TextEdit, Ui, WidgetInfo, WidgetType};
use festerm_config::{
    EmojiPresentationPreference, ScrollSpeedPreference, ScrollbackLimitPreference,
    SftpPaneOrderPreference, TerminalFontPreference,
};
use festerm_ui_egui::{chrome::ChipLayout, theme};

use super::{ssh_paragraph, ssh_section_heading, AppCommand, CONTENT_SCROLLBAR_LANE};

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
    pub keyboard_bindings: festerm_config::KeyboardBindings,
    pub chip_layout: ChipLayout,
    pub status_bar_visible: bool,
    pub show_session_details: bool,
    pub confirm_session_close: bool,
    pub prefer_powershell: bool,
    pub customize_local_shell: bool,
    pub restore_workspace: bool,
    pub terminal_font: TerminalFontPreference,
    pub terminal_ligatures: bool,
    pub emoji_presentation: EmojiPresentationPreference,
    pub scroll_speed: ScrollSpeedPreference,
    pub scrollback_limit: ScrollbackLimitPreference,
    pub quick_switch_overlay: bool,
    pub compact_launcher_grid: bool,
    pub pulse_new_output_dot: bool,
    pub show_resumable_sessions: bool,
    pub show_durable_session_in_status_bar: bool,
    pub automatic_update_checks: bool,
    pub default_sftp_local_directory: Option<String>,
    pub sftp_pane_order: SftpPaneOrderPreference,
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

pub(crate) fn show_settings(ui: &mut Ui, settings: SettingsViewModel) -> Option<AppCommand> {
    let SettingsViewModel {
        keyboard_bindings,
        chip_layout,
        status_bar_visible,
        show_session_details,
        confirm_session_close,
        prefer_powershell,
        customize_local_shell,
        restore_workspace,
        terminal_font,
        terminal_ligatures,
        emoji_presentation,
        scroll_speed,
        scrollback_limit,
        quick_switch_overlay,
        compact_launcher_grid,
        pulse_new_output_dot,
        show_resumable_sessions,
        show_durable_session_in_status_bar,
        automatic_update_checks,
        default_sftp_local_directory,
        sftp_pane_order,
    } = settings;
    #[cfg(not(windows))]
    let _ = prefer_powershell;
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
    let mut command = None;
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
            egui::ScrollArea::vertical()
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
                        ui.heading("Settings");
                        ui.add_space(2.0);

                        settings_card(ui, "Interface", |ui| {
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
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
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
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show status bar",
                                "Display sourced session state, terminal dimensions, and \
                                 the active session detail when compact chips require it.",
                                status_bar_visible,
                            ) {
                                command = Some(AppCommand::ToggleStatusBar);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Confirm before closing live sessions",
                                "Ask before terminating a running local process or \
                                 disconnecting an active remote session.",
                                confirm_session_close,
                            ) {
                                command = Some(AppCommand::ToggleConfirmSessionClose);
                            }
                            #[cfg(windows)]
                            {
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(10.0);
                                if settings_toggle_row(
                                    ui,
                                    "Prefer PowerShell when available",
                                    "Use the current user's standard Windows app-execution \
                                     alias for pwsh.exe when it exists. Turn this off to use \
                                     COMSPEC for new default local sessions.",
                                    prefer_powershell,
                                ) {
                                    command = Some(AppCommand::TogglePreferPowershell);
                                }
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
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
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Workspace restore",
                                "Reopen your previously open tabs and the active tab \
                                 automatically on launch. Off by default.",
                                restore_workspace,
                            ) {
                                command = Some(AppCommand::ToggleRestoreWorkspace);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Compact New Session layout",
                                "Shrink the New Session tab's launch cards so the \
                                 saved-profile and running-session panels start higher \
                                 up the window. Card descriptions are kept. Off by \
                                 default.",
                                compact_launcher_grid,
                            ) {
                                command = Some(AppCommand::ToggleCompactLauncherGrid);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Pulse status dot on new background output",
                                "Slow-pulse a background tab's chip status dot when that \
                                 session has produced output since you last looked at it, \
                                 so it can quietly draw your eye without changing its \
                                 connection-state color. The active tab's own chip never \
                                 pulses. Off by default.",
                                pulse_new_output_dot,
                            ) {
                                command = Some(AppCommand::TogglePulseNewOutputDot);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Resume unattached local sessions from New Session",
                                "Surface locally running festerm-sessiond persistence \
                                 sessions that have no attached window, plus locally \
                                 running tmux and GNU screen sessions, as one-click \
                                 \"Resume\" entries in their own labeled widgets on the \
                                 New Session tab. Off by default.",
                                show_resumable_sessions,
                            ) {
                                command = Some(AppCommand::ToggleShowResumableSessions);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Show durable session name in status bar",
                                "Name the tmux, GNU screen, or fesTerm session daemon \
                                 session the active terminal is attached to, as \
                                 \"provider · session name\". This is the durable \
                                 session's stable identity, which the terminal-provided \
                                 title cannot supply; it is separate from \"Show session \
                                 details in chips\" and unaffected by it. Ordinary \
                                 sessions show nothing. Off by default.",
                                show_durable_session_in_status_bar,
                            ) {
                                command = Some(AppCommand::ToggleDurableSessionInStatusBar);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
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
                            ui.add_space(10.0);
                            if ui.button("Reset interface settings to defaults").clicked() {
                                command = Some(AppCommand::ResetInterfaceSettings);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Terminal mouse", |ui| {
                            ui.label("Shift+right-click opens the fesTerm menu");
                            ssh_paragraph(
                                ui,
                                "Use Shift+right-click for fesTerm's Copy/Paste menu even \
                                 when a terminal program handles mouse input. Plain right-click \
                                 stays with that program. Shift+drag selects terminal text. \
                                 These overrides work in local and SSH sessions.",
                            );
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Scrolling", |ui| {
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
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
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
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Terminal typography", |ui| {
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
                                            "Choose the bundled primary face used by terminal \
                                             cells. Application text is unchanged.",
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
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
                            if settings_toggle_row(
                                ui,
                                "Programming ligatures",
                                "Shape eligible adjacent cells together while preserving cursor, \
                                 selection, and terminal grid ownership.",
                                terminal_ligatures,
                            ) {
                                command = Some(AppCommand::ToggleTerminalLigatures);
                            }
                            ui.add_space(10.0);
                            ui.separator();
                            ui.add_space(10.0);
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
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Quick switch", |ui| {
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
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "SFTP", |ui| {
                            ui.horizontal_top(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    ui.label(
                                        egui::RichText::new("SFTP pane order")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    ssh_paragraph(
                                        ui,
                                        "Visual order for the GUI SFTP file manager. Commands and accessibility labels still refer to Local and Remote, never Left and Right.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    for (value, label) in [
                                        (
                                            SftpPaneOrderPreference::LocalLeft,
                                            "Local left · Remote right",
                                        ),
                                        (
                                            SftpPaneOrderPreference::RemoteLeft,
                                            "Remote left · Local right",
                                        ),
                                    ] {
                                        let response = ui.radio_value(
                                            state
                                                .sftp_pane_order
                                                .get_or_insert(sftp_pane_order),
                                            value,
                                            label,
                                        );
                                        if response.changed() {
                                            command = Some(AppCommand::SetSftpPaneOrder(value));
                                        }
                                    }
                                });
                            });
                            ui.add_space(10.0);
                            ui.horizontal_top(|ui| {
                                let mut label_id = None;
                                ui.vertical(|ui| {
                                    ui.set_max_width((ui.available_width() - 190.0).max(0.0));
                                    let label = ui.label(
                                        egui::RichText::new("Default local SFTP directory")
                                            .color(theme::TEXT_PRIMARY),
                                    );
                                    label_id = Some(label.id);
                                    ssh_paragraph(
                                        ui,
                                        "Starting local directory for new SFTP tabs. \
                                         Changing it updates only future sessions; `lcd` \
                                         affects the live session only.",
                                    );
                                });
                                ui.add_space(16.0);
                                ui.vertical(|ui| {
                                    let response = ui.add(
                                        TextEdit::singleline(
                                            &mut state.default_sftp_local_directory,
                                        )
                                        .id(field_id)
                                        .hint_text("Path to local directory")
                                        .desired_width(180.0),
                                    );
                                    let response = response
                                        .labelled_by(label_id.expect("label should be rendered"));
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
                                });
                            });
                            if let Some(feedback) = &state.feedback {
                                ui.add_space(6.0);
                                ui.colored_label(theme::STATUS_ERROR, feedback);
                            }
                        });

                        ui.add_space(12.0);

                        settings_card(ui, "Keyboard bindings", |ui| {
                            if let Some(action) =
                                crate::keyboard::show_editor(ui, &keyboard_bindings)
                            {
                                command = Some(action);
                            }
                        });
                    });
                });
        });
    });
    ui.data_mut(|data| data.insert_temp(state_id, state));
    command
}

const fn terminal_font_label(font: TerminalFontPreference) -> &'static str {
    match font {
        TerminalFontPreference::JetBrainsMono => "JetBrains Mono",
        TerminalFontPreference::IosevkaTerm => "Iosevka Term",
        TerminalFontPreference::JuliaMono => "JuliaMono",
        TerminalFontPreference::MapleMono => "Maple Mono",
    }
}

/// A titled card matching the launcher/profile-editor "quiet section" visual
/// language (`ssh_section_heading` + a bordered, rounded surface), so
/// Settings groups related controls the same way the rest of the app does
/// instead of a flat, plain list of buttons.
fn settings_card(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    let _ = settings_card_response(ui, title, body);
}

fn settings_card_response(
    ui: &mut Ui,
    title: &str,
    body: impl FnOnce(&mut Ui),
) -> egui::InnerResponse<()> {
    egui::Frame::new()
        .fill(theme::SURFACE_TAB_INACTIVE)
        .stroke(Stroke::new(1.0, theme::BORDER_SUBTLE))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ssh_section_heading(ui, title);
            ui.add_space(6.0);
            body(ui);
        })
}

/// One labeled on/off preference row: a fixed title and state-independent
/// description on the left, and a pill-shaped toggle switch on the right
/// (`docs/images/gui-mockups/settings.png`) - replacing a plain text button
/// whose entire label used to flip between "shown"/"hidden" copy. Returns
/// whether the switch was clicked this frame; the caller still owns
/// dispatching the actual `AppCommand`, matching every other control here.
fn settings_toggle_row(ui: &mut Ui, title: &str, description: &str, value: bool) -> bool {
    let mut clicked = false;
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
    clicked
}

/// Painter-drawn pill-shaped toggle switch matching the mockup's on/off
/// control (`docs/images/gui-mockups/settings.png`): a rounded track that
/// fills with the accent color when on, and a circular knob that slides to
/// the matching side - instead of a text button whose whole label changes
/// between "shown"/"hidden" copy. An explicit accessible label is set (like
/// `paint_close_button`'s pattern) since the switch has no text of its own
/// for screen readers or headless-test queries to find.
pub(super) fn toggle_switch(ui: &mut Ui, value: bool, accessible_label: &str) -> egui::Response {
    let desired_size = egui::vec2(40.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Checkbox, true, accessible_label));

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
            ui.horizontal(|ui| {
                for (index, (label, selected)) in options.iter().enumerate() {
                    if ui.selectable_label(*selected, *label).clicked() && !*selected {
                        clicked = Some(index);
                    }
                }
            });
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
    use egui_kittest::{kittest::Queryable, Harness};

    struct SettingsHarnessState {
        command: Option<AppCommand>,
        keyboard_bindings: festerm_config::KeyboardBindings,
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
                            keyboard_bindings: state.keyboard_bindings.clone(),
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            confirm_session_close: true,
                            prefer_powershell: true,
                            customize_local_shell: false,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            show_durable_session_in_status_bar: false,
                            automatic_update_checks: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
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
                },
            )
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

    #[test]
    fn settings_presents_each_shortcut_exactly_once() {
        // Regression test: a read-only "Keyboard" card used to restate the
        // command palette and Settings shortcuts that the bindings editor
        // already lists, so a customized chord could be shown twice with two
        // different values.
        let mut harness = wide_settings_harness();
        harness.run();

        assert!(harness.query_by_label("QUICK SWITCH").is_some());
        assert_eq!(
            harness.query_all_by_label("Open Settings").count(),
            1,
            "each action must appear once, in the bindings editor"
        );
        assert_eq!(harness.query_all_by_label("Command palette").count(), 1);
    }

    #[test]
    fn settings_keyboard_bindings_card_is_last() {
        let mut harness = wide_settings_harness();
        harness.run();

        let keyboard_top = harness.get_by_label("KEYBOARD BINDINGS").rect().top();
        for earlier_card in [
            "INTERFACE",
            "SCROLLING",
            "TERMINAL TYPOGRAPHY",
            "QUICK SWITCH",
            "SFTP",
        ] {
            assert!(
                harness.get_by_label(earlier_card).rect().top() < keyboard_top,
                "{earlier_card} must appear before Keyboard bindings"
            );
        }
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
        // (feature request #64): off by default, with its own explicit
        // toggle in the Interface card.
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
    fn settings_toggle_pulse_new_output_dot_control_returns_the_toggle_command() {
        // Regression test for the "Pulse status dot on new background
        // output" preference (feature request #68): off by default, with
        // its own explicit toggle in the Interface card.
        let mut harness = settings_harness();
        harness.run();

        assert!(harness
            .query_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output"
            )
            .is_some());

        harness
            .get_by_role_and_label(
                accesskit::Role::CheckBox,
                "Pulse status dot on new background output",
            )
            .click();
        harness.run();

        assert!(matches!(
            harness.state().command,
            Some(AppCommand::TogglePulseNewOutputDot)
        ));
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
        // New Session" preference (feature request #70): off by default,
        // with its own explicit toggle in the Interface card.
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
                            keyboard_bindings: Default::default(),
                            chip_layout: ChipLayout::Wrap,
                            status_bar_visible: true,
                            show_session_details: true,
                            confirm_session_close: true,
                            prefer_powershell: true,
                            customize_local_shell: false,
                            restore_workspace: false,
                            terminal_font: TerminalFontPreference::JetBrainsMono,
                            terminal_ligatures: false,
                            emoji_presentation: EmojiPresentationPreference::Color,
                            scroll_speed: ScrollSpeedPreference::Normal,
                            scrollback_limit: ScrollbackLimitPreference::MiB64,
                            quick_switch_overlay: false,
                            compact_launcher_grid: false,
                            pulse_new_output_dot: false,
                            show_resumable_sessions: false,
                            show_durable_session_in_status_bar: false,
                            automatic_update_checks: false,
                            default_sftp_local_directory: None,
                            sftp_pane_order: SftpPaneOrderPreference::LocalLeft,
                        },
                    ) {
                        state.command = Some(command);
                    }
                },
                SettingsHarnessState {
                    command: None,
                    keyboard_bindings: Default::default(),
                },
            );
        harness.run();
        harness.run();

        let status_bar_top =
            egui::containers::panel::PanelState::load(&harness.ctx, egui::Id::new("status_bar"))
                .expect("status bar panel state should be recorded")
                .outer_rect
                .top();
        let command_palette_rect = harness.get_by_label("Command palette").rect();
        assert!(
            command_palette_rect.max.y <= status_bar_top,
            "Settings content must stay above the status bar rather than overlapping it"
        );
    }
}
