use super::*;

pub(crate) fn show(
    ui: &mut egui::Ui,
    view: &View,
    share_targets: &[ShareTarget],
) -> Option<Action> {
    let mut action = None;
    ui.label(view.observation_label());
    ui.small("Settings affect this computer only. Profiles, imports and new shells never grant HOST access.");
    if let Scope::Present(snapshot) = &view.scope {
        ui.monospace(format!(
            "Installation {} · epoch {} · {:?}",
            snapshot.installation, snapshot.epoch, snapshot.phase
        ));
    }
    if let Some(process) = &view.process {
        ui.label(format!(
            "Last native process observation: {process}. This is not endpoint readiness."
        ));
    }
    if let Some(failure) = view.failure {
        ui.colored_label(ui.visuals().warn_fg_color, failure.message());
    }
    if let Some(code) = &view.device_code {
        ui.label("Complete the explicit sign-in at https://github.com/login/device");
        ui.monospace(code.0.as_str());
        ui.small("No browser opens automatically. Do not share this one-time code.");
    }
    let mut draft = view.draft.clone();
    ui.add_enabled_ui(!view.busy, |ui| {
        field(ui, "Friendly name", &mut draft.friendly_name);
        ui.checkbox(
            &mut draft.allow_creation,
            "Allow remote creation (separate owner approval still required)",
        );
        ui.checkbox(
            &mut draft.startup_at_sign_in,
            "Start at my sign-in (only where the native policy is qualified)",
        );
        ui.small("Default: manual startup; creation off. No pre-login option.");
        field(
            ui,
            "GitHub numeric account ID (verified during sign-in)",
            &mut draft.account,
        );
        ui.collapsing("Signed helper prerequisites", |ui| {
            ui.label("This build's trusted release files are required. No installer script runs from Settings.");
            field(ui, "Manifest · absolute path", &mut draft.manifest);
            field(ui, "Signature · absolute path", &mut draft.signature);
            field(ui, "Archive · absolute path", &mut draft.archive);
        });
        ui.horizontal_wrapped(|ui| {
            if ui.button("Refresh local status").clicked() {
                action = Some(Action::Refresh);
            }
            for (operation, label) in [
                (Operation::Enable, "Review Enable…"),
                (Operation::ObserveNative, "Observe native process…"),
                (Operation::InspectOwner, "Review native owner context…"),
                (Operation::Reconcile, "Review interrupted setup recovery…"),
                (Operation::Start, "Review Start…"),
                (Operation::Disable, "Review Disable…"),
                (Operation::Uninstall, "Review Uninstall / retry cleanup…"),
            ] {
                if ui.button(label).clicked() {
                    action = Some(Action::Review(operation));
                }
            }
        });
        if let Some(binding) = &view.owner_binding {
            ui.collapsing("Public owner binding for independent client review", |ui| {
                ui.monospace(binding);
            });
        }
        ui.collapsing("Explicit owner-key approval · native vault only", |ui| {
            ui.small("First review native owner context to pin its current policy revision. Changed or revoked policy requires a new review.");
            field(ui, "Owner public key · 64 hex characters", &mut draft.owner_public_key);
            field(
                ui,
                "Independently confirmed owner fingerprint",
                &mut draft.owner_fingerprint,
            );
            ui.checkbox(&mut draft.approve_creation, "Also approve creation for this owner key");
            ui.small("Inventory and existing attach/detach only unless separately approved. No explicit-end or temporary grant.");
            if ui.button("Review owner approval…").clicked() {
                action = Some(Action::Review(Operation::ApproveOwner));
            }
        });
        if let Some(operation) = view.review {
            ui.group(|ui| {
                ui.label(format!("Confirm {operation:?} for the exact observed installation and epoch?"));
                match operation {
                    Operation::Enable => {
                        ui.label(format!("Name: {} · creation {} · startup {:?}", draft.friendly_name, draft.allow_creation, draft.settings().startup));
                        ui.label("Explicit foreground sign-in may create a cloud resource and register a signed native helper. No shells are created or shared; owner access requires separate approval.");
                    }
                    Operation::ApproveOwner => {
                        ui.label("Approve this exact public key/fingerprint and selected permissions? Native vault errors never select file storage.");
                        ui.small("Cancelling in-flight permission work fences this host; persistent shells remain untouched.");
                    }
                    Operation::Disable => {
                        ui.label("Revoke first, drain host leases and stop remote exposure. Persistent shells are NOT stopped or restarted.");
                    }
                    Operation::Uninstall => {
                        ui.label("Fence and remove only this exact native registration. Cloud cleanup can remain pending and require independent manual provider review. No unconditional DELETE is issued.");
                    }
                    _ => {
                        ui.label("A running process or successful command is not proof of endpoint readiness.");
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Confirm explicitly").clicked() {
                        action = Some(Action::Confirm);
                    }
                    if ui.button("Not now").clicked() {
                        action = Some(Action::Dismiss);
                    }
                });
            });
        }
        ui.separator();
        ui.label("Share one existing local persistent shell");
        ui.small("Private by default. No share-all, profile metadata grant, persistent conversion or restart.");
        for target in share_targets {
            if ui
                .button(format!(
                    "Review {} · generation {}…",
                    target.profile.as_deref().unwrap_or("local shell"),
                    target.lifecycle_generation
                ))
                .clicked()
            {
                action = Some(Action::ReviewShare(target.clone()));
            }
        }
        if share_targets.is_empty() {
            ui.small("No eligible attached local native persistent shells. Nonpersistent shells are not converted.");
        }
        if let Some(target) = &view.share_review {
            ui.group(|ui| {
                ui.label(format!(
                    "Exact local session {} · created {} · generation {}",
                    target.identity.name,
                    target.identity.created_at_unix_ms,
                    target.lifecycle_generation
                ));
                ui.label(Failure::ShareCapability.message());
                if ui.button("Check explicit Share capability").clicked() {
                    action = Some(Action::Share(target.clone()));
                }
                if ui.button("Check exact Unshare capability").clicked() {
                    action = Some(Action::Unshare(target.clone()));
                }
                if ui.button("Set up host first · still private…").clicked() {
                    action = Some(Action::Review(Operation::Enable));
                }
            });
        }
    });
    if draft != view.draft {
        // An edited draft always invalidates an older confirmation.
        action = Some(Action::Edit(draft));
    }
    if view.busy && ui.button("Cancel and fence mutating work").clicked() {
        action = Some(Action::Cancel);
    }
    if view.cancelling {
        ui.label("Revoking / draining. New work remains blocked until actual workers finish.");
    }
    if matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::CleanupPending)
        && view.cleanup.is_none()
    {
        ui.label("The cleanup receipt is missing, foreign, unreadable or uncertain. There is no verified removal target. Do not remove anything from journal metadata; retry authenticated cleanup or review the protected evidence locally.");
    }
    if let Some(context) = &view.cleanup {
        ui.separator();
        if view.cleanup_verified_for_manual_review {
            ui.label("Last authenticated receipt-bound guidance, NOT continuous verification. Independently verify the CURRENT account, cluster, tunnel ID, ownership label and incarnation below before deciding to remove it in the provider's own UI. Stop on any mismatch or ambiguity.");
        } else {
            ui.label("Stored receipt ONLY — unverified. Do NOT remove a resource from this context. Retry authenticated Uninstall / cleanup to obtain current owner-bound guidance.");
        }
        ui.monospace(format!(
            "installation {} · epoch {}\naccount {} · cluster {} · tunnel {}\nownership {} · incarnation {}",
            context.installation, context.epoch, context.account_id, context.cluster,
            context.tunnel_id, context.ownership_label, context.incarnation
        ));
        ui.small("Keep CleanupPending while present or uncertain. Only an authenticated verified-absence retry commits Removed. Nothing opens or deletes automatically.");
    }
    action
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).char_limit(4096));
}
