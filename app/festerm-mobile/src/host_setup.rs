//! "Set up a host" sheets: the reviewable Dev Tunnel installer command and the
//! exact embedded script. Nothing here runs the script or touches the network.

use super::theme::{color, radius, space, type_scale};
use super::widgets::{
    bottom_sheet, button, caption_label, card, footnote, segmented, sheet_title, switch_row,
    ButtonKind,
};
use eframe::egui::{self, CornerRadius, Rect, Stroke, Ui};
use festerm_host_setup::{HostPlatform, InstallOptions};

#[derive(Debug)]
pub struct HostSetupState {
    pub open: bool,
    pub platform_index: usize,
    pub takeover: bool,
    pub install_dependencies: bool,
    /// The read-only script viewer is showing instead of the command sheet.
    pub viewing: bool,
    pub notice: Option<&'static str>,
}

impl Default for HostSetupState {
    fn default() -> Self {
        Self {
            open: false,
            platform_index: 0,
            takeover: InstallOptions::default().takeover,
            install_dependencies: false,
            viewing: false,
            notice: None,
        }
    }
}

impl HostSetupState {
    pub fn open(&mut self) {
        self.open = true;
        self.viewing = false;
        self.notice = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.viewing = false;
        self.notice = None;
    }

    pub fn platform(&self) -> HostPlatform {
        HostPlatform::ALL[self.platform_index.min(HostPlatform::ALL.len() - 1)]
    }

    pub fn options(&self) -> InstallOptions {
        InstallOptions {
            takeover: self.takeover,
            install_dependencies: self.install_dependencies
                && self.platform() == HostPlatform::Linux,
        }
    }

    pub fn command(&self) -> String {
        festerm_host_setup::install_command(self.platform(), self.options())
    }

    pub fn script(&self) -> &'static str {
        self.platform().script()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostSetupAction {
    None,
    Close,
    CopyCommand,
    ViewScript,
    CopyScript,
    BackToCommand,
}

/// Draws whichever of the two sheets is current.
pub fn show(ctx: &egui::Context, bounds: Rect, state: &mut HostSetupState) -> HostSetupAction {
    if !state.open {
        return HostSetupAction::None;
    }
    if state.viewing {
        script_sheet(ctx, bounds, state)
    } else {
        command_sheet(ctx, bounds, state)
    }
}

fn mono_block(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(color::FIELD)
        .stroke(Stroke::new(1.0, color::BORDER))
        .corner_radius(CornerRadius::same(radius::FIELD))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .monospace()
                        .size(type_scale::CAPTION + 0.5)
                        .color(color::TEXT),
                )
                .wrap(),
            );
        });
}

fn command_sheet(ctx: &egui::Context, bounds: Rect, state: &mut HostSetupState) -> HostSetupAction {
    let mut action = HostSetupAction::None;
    let outcome = bottom_sheet(ctx, "host-setup", bounds, |ui| {
        if sheet_title(ui, "Set up a host", Some("Done")) {
            action = HostSetupAction::Close;
        }
        footnote(
            ui,
            "Run one line on the computer you want to reach. It installs the fesTerm host and \
             publishes it as a Dev Tunnel.",
        );
        let labels = HostPlatform::ALL.map(HostPlatform::label);
        let items: Vec<_> = labels.iter().map(|label| (None, *label)).collect();
        segmented(ui, &items, &mut state.platform_index, false);
        card(ui, |ui| {
            switch_row(
                ui,
                "Allow remote takeover",
                Some("Lets another device take over a held session."),
                &mut state.takeover,
                false,
            );
            if state.platform() == HostPlatform::Linux {
                switch_row(
                    ui,
                    "Install dependencies",
                    Some("Installs missing packages with sudo."),
                    &mut state.install_dependencies,
                    true,
                );
            }
        });
        caption_label(ui, "Command");
        mono_block(ui, &state.command());
        footnote(
            ui,
            &format!(
                "The script you can view is the exact installer shipped with fesTerm {}; the \
                 command downloads that release's copy and pins the host bundle to this version.",
                festerm_host_setup::VERSION
            ),
        );
        if let Some(notice) = state.notice {
            ui.label(
                egui::RichText::new(notice)
                    .size(type_scale::CAPTION)
                    .color(color::SUCCESS),
            );
        }
        if button(ui, ButtonKind::Primary, "Copy command", true).clicked() {
            action = HostSetupAction::CopyCommand;
        }
        if button(ui, ButtonKind::Secondary, "View script", true).clicked() {
            action = HostSetupAction::ViewScript;
        }
    });
    if outcome.dismissed {
        action = HostSetupAction::Close;
    }
    action
}

fn script_sheet(ctx: &egui::Context, bounds: Rect, state: &mut HostSetupState) -> HostSetupAction {
    let mut action = HostSetupAction::None;
    let platform = state.platform();
    let text = platform.script();
    let outcome = bottom_sheet(ctx, "host-setup-script", bounds, |ui| {
        if sheet_title(ui, platform.script_name(), Some("Back")) {
            action = HostSetupAction::BackToCommand;
        }
        footnote(
            ui,
            &format!(
                "Read-only. Save it and run it yourself with: {}",
                festerm_host_setup::local_command(platform, state.options())
            ),
        );
        script_viewer(ui, text, (bounds.height() * 0.5).clamp(200.0, 520.0));
        if let Some(notice) = state.notice {
            ui.label(
                egui::RichText::new(notice)
                    .size(type_scale::CAPTION)
                    .color(color::SUCCESS),
            );
        }
        if button(ui, ButtonKind::Primary, "Copy script", true).clicked() {
            action = HostSetupAction::CopyScript;
        }
    });
    if outcome.dismissed {
        action = HostSetupAction::BackToCommand;
    }
    action
}

/// Read-only monospace text with a line gutter; only visible rows are laid out.
fn script_viewer(ui: &mut Ui, text: &str, height: f32) {
    let lines: Vec<&str> = text.lines().collect();
    let font = type_scale::mono(type_scale::CAPTION);
    let row_height = ui.fonts_mut(|fonts| fonts.row_height(&font)) + 2.0;
    let gutter = format!("{}", lines.len()).len();
    egui::Frame::new()
        .fill(color::FIELD)
        .stroke(Stroke::new(1.0, color::BORDER))
        .corner_radius(CornerRadius::same(radius::FIELD))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both()
                .id_salt("host-setup-script-text")
                .max_height(height)
                .auto_shrink([false, false])
                .show_rows(ui, row_height, lines.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for index in range {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = space::SM;
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{:>gutter$}", index + 1))
                                        .font(font.clone())
                                        .color(color::TEXT_FAINT),
                                )
                                .selectable(false),
                            );
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(lines[index])
                                        .font(font.clone())
                                        .color(color::TEXT),
                                )
                                .extend(),
                            );
                        });
                    }
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_defaults_on_and_off_adds_the_flag() {
        let mut state = HostSetupState::default();
        assert!(state
            .command()
            .ends_with(&format!("bash -s -- {}", festerm_host_setup::VERSION)));
        state.takeover = false;
        assert!(state.command().ends_with("--no-takeover"));
        state.platform_index = 2;
        assert!(state.command().ends_with("-NoTakeover"));
    }

    #[test]
    fn dependencies_apply_to_linux_only() {
        let mut state = HostSetupState {
            install_dependencies: true,
            ..HostSetupState::default()
        };
        assert!(!state.command().contains("--install-dependencies"));
        state.platform_index = 1;
        assert!(state.command().contains("--install-dependencies"));
        state.platform_index = 2;
        assert!(!state.command().contains("dependencies"));
    }

    #[test]
    fn viewer_shows_the_exact_embedded_script() {
        let mut state = HostSetupState::default();
        assert_eq!(state.script(), festerm_host_setup::BASH_INSTALLER);
        state.platform_index = 2;
        assert_eq!(state.script(), festerm_host_setup::POWERSHELL_INSTALLER);
    }
}
