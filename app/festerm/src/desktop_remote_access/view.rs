use super::*;
use crate::screens::{settings_note, settings_toggle_row_enabled};
use festerm_ui_egui::{
    controls::{action_button, ActionButtonRole},
    theme,
};

pub(crate) fn show(
    ui: &mut egui::Ui,
    view: &View,
    share_targets: &[ShareTarget],
) -> Option<Action> {
    ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(24.0);
    let mut action = None;
    let mut requested = view
        .access_requested
        .unwrap_or_else(|| view.access_enabled());
    if settings_toggle_row_enabled(
        ui,
        "Allow remote access to this computer",
        "Lets a signed-in GitHub account reach shells on this computer. Nothing is enabled \
         until you review Enable.",
        requested,
        !view.busy,
    ) {
        requested = !requested;
        action = Some(Action::RequestAccess(requested));
    }
    if requested && !view.access_enabled() {
        ui.label("Requested — not yet enabled.");
    }
    if !requested && view.access_enabled() {
        ui.colored_label(
            theme::STATUS_ERROR,
            "Access has not been disabled yet. Confirm Disable and wait for revocation to finish.",
        );
    }
    if let Some(failure) = view.failure {
        ui.colored_label(theme::STATUS_ERROR, failure.notice());
    }
    if let Some(code) = &view.device_code {
        ui.label("Complete the explicit sign-in at https://github.com/login/device");
        ui.monospace(code.0.as_str());
        settings_note(
            ui,
            "No browser opens automatically. Do not share this one-time code.",
        );
    }
    let mut draft = view.draft.clone();
    ui.add_enabled_ui(!view.busy, |ui| {
        if requested {
            ui.label("GitHub account");
            let bound = view.bound_account();
            let selected = view.accounts.iter().find(|account| {
                account.account_id.to_string() == draft.account
            });
            let selected_label = selected.map(|account| format!("@{}", account.login))
                .unwrap_or_else(|| match bound {
                    Some(account) => format!("Host account · {account}"),
                    None => "Choose a signed-in account".into(),
                });
            egui::ComboBox::from_id_salt("local_host_github_account")
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for account in &view.accounts {
                        if account.account_id == 0 { continue; }
                        let identity = account.account_id.to_string();
                        ui.add_enabled_ui(bound.is_none_or(|bound| bound == identity), |ui| {
                            ui.selectable_value(&mut draft.account, identity,
                                format!("@{} · {}", account.login, account.account_id));
                        });
                    }
                });
            if view.accounts.is_empty()
                && action_button(ui, ActionButtonRole::Secondary, "Sign in with GitHub").clicked() {
                action = Some(Action::SignInAccount);
            }
            if bound.is_some() {
                settings_note(ui, "This installation keeps its verified account. Disable and complete removal before choosing another account.");
            }
            if settings_toggle_row_enabled(
                ui,
                "Allow remote clients to create shell sessions",
                "Off by default. Otherwise remote clients only list, attach to, and detach \
                 from existing shells.",
                draft.allow_creation,
                true,
            ) {
                draft.allow_creation = !draft.allow_creation;
            }
            let disable_to_configure = matches!(&view.scope, Scope::Present(snapshot)
                if snapshot.phase == Phase::Provisioned && snapshot.settings != draft.settings());
            if disable_to_configure {
                settings_note(ui, "Changes are not applied. Disable access, then re-enable to review the retained preferences. Durable shells stay running.");
                if action_button(ui, ActionButtonRole::DangerOutline, "Review Disable to change host settings…").clicked() {
                    action = Some(Action::Review(Operation::Disable));
                }
            } else if action_button(ui, ActionButtonRole::Accent, "Review Enable…").clicked() {
                action = Some(Action::Review(Operation::Enable));
            }
        }
        ui.collapsing("Advanced / Recovery", |ui| {
        ui.label(view.observation_label());
        settings_note(ui, "Selecting an account is not host authentication. Enable separately verifies the selected account and native-vault consent.");
            if let Some(failure) = view.failure {
                settings_note(ui, failure.message());
            }
        field(ui, "Friendly name", &mut draft.friendly_name);
        if settings_toggle_row_enabled(
            ui,
            "Start at my sign-in (only where the native policy is qualified)",
            "Default: manual startup; creation off. No pre-login option.",
            draft.startup_at_sign_in,
            true,
        ) {
            draft.startup_at_sign_in = !draft.startup_at_sign_in;
        }
        ui.label(format!(
            "Official host bundle · {} · {} / {}",
            festerm_host_setup::VERSION, std::env::consts::OS, std::env::consts::ARCH
        ));
        settings_note(ui, view.bundle_status.label());
        if let Scope::Present(snapshot) = &view.scope {
            ui.monospace(format!("Installation {} · epoch {} · {:?}",
                snapshot.installation, snapshot.epoch, snapshot.phase));
        }
        if let Some(process) = &view.process {
            settings_note(ui, &format!("Native process: {process}. Not endpoint readiness."));
        }
        ui.label("Enable acquires only this exact build's official native host bundle. No installer script runs from Settings.");
        ui.collapsing("Advanced · explicit signed local bundle", |ui| {
            ui.checkbox(&mut draft.manual_bundle, "Use manually selected signed files instead of official acquisition");
            settings_note(ui, "An explicit alternative, never a download, sign-in or vault failure fallback.");
            ui.add_enabled_ui(draft.manual_bundle, |ui| {
                field(ui, "Manifest · absolute path", &mut draft.manifest);
                field(ui, "Signature · absolute path", &mut draft.signature);
                field(ui, "Archive · absolute path", &mut draft.archive);
            });
        });
        ui.horizontal_wrapped(|ui| {
            if action_button(ui, ActionButtonRole::Secondary, "Refresh local status").clicked() {
                action = Some(Action::Refresh);
            }
            for (operation, label) in [
                (Operation::ObserveNative, "Observe native process…"),
                (Operation::InspectOwner, "Review native owner context…"),
                (Operation::Reconcile, "Review interrupted setup recovery…"),
                (Operation::Start, "Review Start…"),
                (Operation::Disable, "Review Disable…"),
                (Operation::Uninstall, "Review Uninstall / retry cleanup…"),
            ] {
                let role = if matches!(operation, Operation::Disable | Operation::Uninstall) {
                    ActionButtonRole::DangerOutline
                } else {
                    ActionButtonRole::Secondary
                };
                if action_button(ui, role, label).clicked() {
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
            settings_note(ui, "First review native owner context to pin its current policy revision. Changed or revoked policy requires a new review.");
            field(ui, "Owner public key · 64 hex characters", &mut draft.owner_public_key);
            field(
                ui,
                "Independently confirmed owner fingerprint",
                &mut draft.owner_fingerprint,
            );
            ui.checkbox(&mut draft.approve_creation, "Also approve creation for this owner key");
            settings_note(ui, "Inventory and existing attach/detach only unless separately approved. No explicit-end or temporary grant.");
            if action_button(ui, ActionButtonRole::Secondary, "Review owner approval…").clicked() {
                action = Some(Action::Review(Operation::ApproveOwner));
            }
        });
        });
        if matches!(view.scope, Scope::Unobserved)
            && action_button(ui, ActionButtonRole::Secondary, "Refresh local status").clicked()
        {
            action = Some(Action::Refresh);
        }
        if let Some(operation) = view.review {
            ui.group(|ui| {
                ui.label(format!("Confirm {operation:?} for the exact observed installation and epoch?"));
                match operation {
                    Operation::Enable => {
                        ui.label(format!("Name: {} · creation {} · startup {:?}", draft.friendly_name, draft.allow_creation, draft.settings().startup));
                        ui.label(if draft.manual_bundle {
                            "Explicit Advanced choice: verify the selected signed local bundle."
                        } else {
                            "Acquire and authenticate this exact current-version native host bundle from the official release. Missing assets block setup; no older/latest retry."
                        });
                        ui.label("Explicit foreground sign-in may create a cloud resource and register a signed native helper. No shells are created or shared; owner access requires separate approval.");
                    }
                    Operation::ApproveOwner => {
                        ui.label("Approve this exact public key/fingerprint and selected permissions? Native vault errors never select file storage.");
                        settings_note(ui, "Cancelling in-flight permission work fences this host; persistent shells remain untouched.");
                    }
                    Operation::Disable => {
                        ui.label("Revoke first, drain host leases and stop remote exposure. Persistent shells are NOT stopped or restarted.");
                    }
                    Operation::Uninstall => {
                        ui.label("Fence and remove only this exact native registration. Cloud cleanup can remain pending and require independent manual provider review. No unconditional DELETE is issued.");
                    }
                    Operation::Share(_) => {
                        ui.label("Use the exact-session sharing review below. This installation review cannot authorize sharing.");
                    }
                    _ => {
                        ui.label("A running process or successful command is not proof of endpoint readiness.");
                    }
                }
                ui.horizontal(|ui| {
                    let role = if matches!(operation, Operation::Disable | Operation::Uninstall) {
                        ActionButtonRole::DangerOutline
                    } else {
                        ActionButtonRole::Accent
                    };
                    if action_button(ui, role, "Confirm explicitly").clicked() {
                        action = Some(Action::Confirm);
                    }
                    if action_button(ui, ActionButtonRole::Secondary, "Not now").clicked() {
                        action = Some(Action::Dismiss);
                    }
                });
            });
        }
        if !share_targets.is_empty() || view.share_review.is_some() || view.sharing.is_some() {
        ui.collapsing("Session sharing / Recovery", |ui| {
        ui.label("Share one existing local persistent shell");
        settings_note(ui, "Private by default. No share-all, profile metadata grant, persistent conversion or restart.");
        for target in share_targets {
            if action_button(ui, ActionButtonRole::Secondary, &format!(
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
            settings_note(ui, "No eligible attached local native persistent shells. Nonpersistent shells are not converted.");
        }
        if let Some(next) = sharing_review(ui, view) {
            action = Some(next);
        }
        });
        }
    });
    if draft != view.draft {
        action = Some(Action::Edit(draft));
    }
    if view.busy
        && action_button(
            ui,
            ActionButtonRole::Secondary,
            "Cancel and fence mutating work",
        )
        .clicked()
    {
        action = Some(Action::Cancel);
    }
    if view.cancelling {
        ui.label("Revoking / draining. New work remains blocked until actual workers finish.");
    }
    if view.cleanup.is_some()
        || matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::CleanupPending)
    {
        ui.collapsing("Cloud cleanup / Recovery", |ui| {
            cleanup(ui, view);
        });
    }
    action
}

pub(crate) fn show_session_sharing(
    ui: &mut egui::Ui,
    view: &View,
    target: Option<&ShareTarget>,
) -> Option<Action> {
    let mut scoped = view.clone();
    scoped.share_review = view
        .share_review
        .clone()
        .filter(|review| Some(review) == target);
    scoped.sharing = view
        .sharing
        .clone()
        .filter(|sharing| Some(&sharing.target) == target);
    let mut action = None;
    ui.add_enabled_ui(!view.busy, |ui| action = sharing_review(ui, &scoped));
    action
}

fn sharing_review(ui: &mut egui::Ui, view: &View) -> Option<Action> {
    let mut action = None;
    if let Some(target) = &view.share_review {
        ui.group(|ui| {
                ui.label(format!(
                    "Exact local session {} · created {} · generation {}",
                    target.identity.name,
                    target.identity.created_at_unix_ms,
                    target.lifecycle_generation
                ));
                if target.source.is_none() {
                    ui.label(Failure::ShareCapability.notice());
                } else {
                    let retained = view.sharing.as_ref().filter(|sharing| &sharing.target == target);
                    if !retained.is_some_and(|sharing| matches!(sharing.phase, SharePhase::Active | SharePhase::Revoked)) {
                        let label = if retained.is_some_and(|sharing| matches!(sharing.phase, SharePhase::Prepared | SharePhase::Uncertain)) {
                            settings_note(ui, "Retry only the original Bind. This does not revoke any prior Switch or create a replacement operation.");
                            "Resume original Bind · no new Switch"
                        } else {
                            settings_note(ui, "Bind prepares this exact generation privately. A separate explicit Switch authorizes exposure. The local shell and frontend stay untouched.");
                            "Confirm Bind privately · no exposure"
                        };
                        if action_button(ui, ActionButtonRole::Accent, label).clicked() {
                            action = Some(Action::Share(target.clone()));
                        }
                    }
                    if action_button(ui, ActionButtonRole::Secondary, "Cancel review").clicked() {
                        action = Some(Action::Dismiss);
                    }
                }
                ui.collapsing("Reviewed session / Recovery", |ui| {
                let mut original = view.share_operation_input.clone();
                field(ui, "Recover ORIGINAL sharing operation · optional", &mut original);
                if original != view.share_operation_input {
                    action = Some(Action::RecoverShare(original));
                }
                if !view.share_operation_input.is_empty() {
                    settings_note(ui, "These recovery controls use this reviewed session and the typed ORIGINAL operation, not the status group below.");
                    for (step, label) in [
                        (ShareStep::Resume, "Resume original protected status · no switch"),
                        (ShareStep::Release, "Release reviewed original operation · keep shell"),
                        (ShareStep::Rollback, "Rollback reviewed original operation · irreversible unshare"),
                    ] {
                        if action_button(ui, ActionButtonRole::Secondary, label).clicked() {
                            action = Some(Action::RecoverReviewedShare(step));
                        }
                    }
                }
                if view.share_operation_input.is_empty()
                    && view.sharing.as_ref().is_some_and(|sharing| &sharing.target == target)
                    && action_button(ui, ActionButtonRole::DangerOutline, "Release this reviewed generation · keep shell").clicked()
                {
                    action = Some(Action::Unshare(target.clone()));
                }
                if action_button(ui, ActionButtonRole::Secondary, "Set up host first · still private…").clicked() {
                    action = Some(Action::Review(Operation::Enable));
                }
                });
            });
    }
    if let Some(sharing) = &view.sharing {
        ui.group(|ui| {
                ui.label(format!(
                    "Last protected response: {:?}. Not endpoint readiness or cached permission.",
                    sharing.phase
                ));
                if sharing.phase == SharePhase::Prepared {
                    settings_note(ui, "PRIVATE: explicitly review the same session and current owner context before Switch.");
                    if action_button(ui, ActionButtonRole::Accent, "Confirm Switch · expose this exact generation").clicked() {
                        action = Some(Action::SwitchShare);
                    }
                }
                if sharing.phase == SharePhase::Uncertain {
                    settings_note(ui, "Outcome uncertain. Keep this ORIGINAL operation. Do not retry with a replacement session or operation.");
                }
                ui.collapsing("Sharing operation / Recovery", |ui| {
                ui.monospace(format!(
                    "Original sharing operation {}\nSession {} · PID {} · created {}",
                    sharing.operation,
                    sharing.target.identity.name,
                    sharing.target.identity.pid,
                    sharing.target.identity.created_at_unix_ms,
                ));
                ui.horizontal_wrapped(|ui| {
                    if action_button(ui, ActionButtonRole::Secondary, "Resume original status · no switch").clicked() {
                        action = Some(Action::ResumeShare);
                    }
                    if action_button(ui, ActionButtonRole::DangerOutline, "Confirm Release · keep shell/frontend").clicked() {
                        action = Some(Action::ReleaseShare);
                    }
                    if action_button(ui, ActionButtonRole::DangerOutline, "Confirm Rollback · irreversible unshare only").clicked() {
                        action = Some(Action::RollbackShare);
                    }
                });
                });
            });
    }
    action
}

fn cleanup(ui: &mut egui::Ui, view: &View) {
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
        settings_note(ui, "Keep CleanupPending while present or uncertain. Only an authenticated verified-absence retry commits Removed. Nothing opens or deletes automatically.");
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).char_limit(4096));
}
