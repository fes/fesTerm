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
    pub account: u64,
    pub target: &'a Target,
    pub operation: &'a OperationId,
    pub epoch: u64,
    pub generation: u64,
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
) -> (bool, bool) {
    ui.separator();
    ui.heading("Confirm End session — not Close tab");
    ui.label(format!(
        "Account github:{} · logical {} · native generation {} · original operation {}",
        display.account,
        display.target.logical,
        display.target.generation.get(),
        display.operation
    ));
    ui.label(format!(
        "Native privacy epoch {} · foreground request generation {}",
        display.epoch, display.generation
    ));
    ui.label("The protected host/installation/principal above and this exact generation are fixed. Native PID/namespace stay host-journal-owned; inventory does not expose a PID.");
    ui.label("End stops the remote process. Closing a persistent attachment does not. Stop-requested/timeout/denial is NOT confirmation that it ended or is absent.");
    ui.checkbox(
        confirmed,
        "I explicitly authorize End for this exact host and generation",
    );
    let confirm = ui
        .add_enabled(*confirmed, egui::Button::new("Confirm exact End once"))
        .clicked();
    let cancel = ui.button("Cancel End (no request)").clicked();
    (confirm, cancel)
}
