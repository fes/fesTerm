use eframe::egui;
use festerm_host_protocol::{IssuedOperation, LogicalSessionId, OperationId, Target};
use festerm_tunnel_bridge::host_client::originals::OriginalId;

pub(super) fn creation_original(
    ui: &mut egui::Ui,
    name: &str,
    logical: &LogicalSessionId,
    issued: Option<IssuedOperation>,
    can_forget: bool,
) -> (bool, bool) {
    ui.label(format!("Retained original {name} · logical {logical} · issued {issued:?}; explicit original-action review only"));
    let review = ui
        .button(format!("Review retained original: {name}"))
        .clicked();
    let forget = ui
        .add_enabled(
            can_forget,
            egui::Button::new("Forget this local Create retry history..."),
        )
        .clicked();
    (review, forget)
}

pub(super) fn end_original(
    ui: &mut egui::Ui,
    target: &Target,
    operation: &OperationId,
    issued: bool,
    resolved: bool,
    can_forget: bool,
) -> (bool, bool) {
    ui.label(format!("Original End {} · generation {} · operation {} · issued={} confirmed-ended={}; protected capture required before sending",
        target.logical, target.generation.get(), operation, issued, resolved));
    let review = ui
        .add_enabled(!resolved, egui::Button::new("Review original End / retry"))
        .clicked();
    let forget = ui
        .add_enabled(
            can_forget,
            egui::Button::new("Forget this local End retry history..."),
        )
        .clicked();
    (review, forget)
}

pub(super) struct ForgetDisplay<'a> {
    pub account: u64,
    pub id: &'a OriginalId,
    pub logical: Option<&'a LogicalSessionId>,
    pub target: Option<&'a Target>,
    pub epoch: u64,
    pub generation: u64,
}

pub(super) struct EndDisplay<'a> {
    pub name: &'a str,
    pub host: &'a str,
}

pub(super) fn forget_review(
    ui: &mut egui::Ui,
    display: ForgetDisplay<'_>,
    confirmed: &mut bool,
) -> (bool, bool) {
    ui.separator();
    ui.heading("Forget local retry history - NOT End session");
    ui.label(format!(
        "Account github:{} · original {:?}",
        display.account, display.id
    ));
    if let Some(logical) = display.logical {
        ui.label(format!("Logical session {logical}"));
    }
    if let Some(target) = display.target {
        ui.label(format!(
            "Captured native generation {}",
            target.generation.get()
        ));
    }
    ui.label(format!(
        "Privacy epoch {} · request generation {}",
        display.epoch, display.generation
    ));
    ui.label("The protected host/installation/principal above are fixed. Only this local checkpoint is selected. Other originals are preserved.");
    ui.label("The remote session may still be running and its action may still complete. Forget does NOT End, detach, revoke, prove absence, retry or authorize a replacement. Other clients are not revoked.");
    ui.checkbox(
        confirmed,
        "I explicitly abandon only this original's local retry history",
    );
    let confirm = ui
        .add_enabled(*confirmed, egui::Button::new("Confirm local Forget once"))
        .clicked();
    let cancel = ui.button("Cancel local Forget (keep history)").clicked();
    (confirm, cancel)
}

pub(super) fn end_review(
    ui: &mut egui::Ui,
    display: EndDisplay<'_>,
    confirmed: &mut bool,
    cancel_focused: &mut bool,
) -> (bool, bool) {
    use crate::widgets::{button, ButtonKind};
    ui.set_max_width((ui.ctx().content_rect().width() - 48.0).clamp(160.0, 400.0));
    ui.spacing_mut().item_spacing.y = 12.0;
    ui.heading(format!("Terminate {} on {}?", display.name, display.host));
    ui.label("This ends the shell and stops programs running in it.");
    let cancel_response = button(ui, ButtonKind::Secondary, "Cancel", true);
    if !*cancel_focused {
        cancel_response.request_focus();
        *cancel_focused = true;
    }
    let cancel =
        cancel_response.clicked() || ui.input(|input| input.key_pressed(egui::Key::Escape));
    let confirm = button(ui, ButtonKind::Destructive, "Terminate", true).clicked();
    *confirmed = confirm && !cancel;
    (confirm && !cancel, cancel)
}
