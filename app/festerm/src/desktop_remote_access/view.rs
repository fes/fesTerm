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
    // Presentation only: reveal each newly opened Enable/Update review once.
    // One fixed entry is not consent or operation state; closing clears it.
    let identity = match (&view.scope, view.review) {
        (Scope::Absent, Some(Operation::Enable)) => Some((Operation::Enable, None)),
        (Scope::Present(snapshot), Some(operation @ (Operation::Enable | Operation::Update))) => {
            Some((
                operation,
                Some((snapshot.installation.clone(), snapshot.epoch)),
            ))
        }
        _ => None,
    };
    let reveal_review = ui.data_mut(|data| {
        let key = ui.id().with("host_review_reveal");
        let previous = data
            .get_temp::<Option<(Operation, Option<(String, u64)>)>>(key)
            .flatten();
        let reveal = identity.is_some() && previous != identity;
        data.insert_temp(key, identity);
        reveal
    });
    let mut requested = view
        .access_requested
        .unwrap_or_else(|| view.access_enabled());
    if settings_toggle_row_enabled(
        ui,
        "Allow remote access to this computer",
        "Share selected durable sessions through this host.",
        requested,
        !view.busy,
    ) {
        requested = !requested;
        action = Some(Action::RequestAccess(requested));
    }
    if matches!(view.scope, Scope::Unobserved) {
        ui.label("Remote access status is unavailable. Refresh local status.");
    } else if requested
        && !matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Provisioned)
    {
        ui.label("Setup pending. Remote access is not enabled yet.");
    }
    if requested
        && matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Provisioned)
    {
        ui.label(view.enabled_status());
        settings_note(ui, "Recorded status, not a current connection test.");
        if let Some(process) = &view.process {
            settings_note(ui, &format!("Last host process observation: {process}."));
        }
        ui.add_enabled_ui(!view.busy, |ui| {
            if action_button(ui, ActionButtonRole::Secondary, "Check host process…").clicked() {
                action = Some(Action::Review(Operation::ObserveNative));
            }
        });
    }
    if !requested && view.access_enabled() {
        ui.colored_label(
            theme::STATUS_ERROR,
            "Access has not been disabled yet. Confirm Disable and wait for revocation to finish.",
        );
    }
    if matches!(&view.scope, Scope::Present(snapshot) if snapshot.binding.is_some() && snapshot.phase != Phase::Removed)
    {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "Host components: {} → {}",
                view.release
                    .as_ref()
                    .map(|release| release.recorded_version.as_str())
                    .unwrap_or("unavailable"),
                festerm_host_setup::VERSION
            ));
            ui.add_enabled_ui(!view.busy && view.update_eligible(), |ui| {
                if action_button(ui, ActionButtonRole::Secondary, "Update host…").clicked() {
                    action = Some(Action::Review(Operation::Update));
                }
            });
        });
        let prerequisite = if !matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Disabled)
        {
            view.release
                .as_ref()
                .filter(|release| release.update_available(festerm_host_setup::VERSION))
                .map(|_| "Disable remote access before updating the host.")
        } else if let Some(release) = &view.release {
            match release.replacement.as_ref() {
                Some(update)
                    if matches!(
                        update.state,
                        festerm_host_setup::services::ReplacementState::Prepared
                            | festerm_host_setup::services::ReplacementState::Applying
                    ) =>
                {
                    Some(if update.to_version == festerm_host_setup::VERSION {
                        "Resume the interrupted update with its original signed bundle."
                    } else {
                        "Finish the pending update using its original app version and signed bundle."
                    })
                }
                Some(update)
                    if update.state
                        == festerm_host_setup::services::ReplacementState::Committed =>
                {
                    Some("Remote access is disabled. Enable it separately when ready.")
                }
                _ => None,
            }
        } else {
            Some("Refresh local status or review Recovery before updating.")
        };
        if let Some(prerequisite) = prerequisite {
            settings_note(ui, prerequisite);
        }
    }

    if let Some(failure) = view.failure {
        ui.colored_label(theme::STATUS_ERROR, failure.notice());
        if let Some(detail) = failure.observation_detail() {
            ui.small(detail);
            if matches!(view.scope, Scope::Present(_)) {
                settings_note(ui, "Protected phase is the last confirmed observation. Current endpoint readiness is unknown.");
            }
        }
    }
    diagnostics::show(ui, view.diagnostic, &view.runtime_history);
    if let Some(code) = &view.device_code {
        ui.monospace(code.0.as_str());
        match view.device_copied {
            Some(true) => {
                ui.label("Code copied. Paste it into GitHub.");
            }
            Some(false) => {
                ui.colored_label(
                    theme::STATUS_ERROR,
                    "Could not copy code. Copy it here or try again.",
                );
            }
            None => {
                ui.label("Enter this code on GitHub.");
            }
        }
        if view.device_opened == Some(false) {
            ui.colored_label(theme::STATUS_ERROR, "Could not open browser. Try again.");
        }
        ui.horizontal(|ui| {
            if action_button(ui, ActionButtonRole::Secondary, "Copy again").clicked() {
                action = Some(Action::CopySignInCode);
            }
            if action_button(ui, ActionButtonRole::Secondary, "Open browser").clicked() {
                action = Some(Action::OpenSignInBrowser);
            }
        });
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
                settings_note(ui, "To change accounts, disable access and remove this host first.");
            }
            if settings_toggle_row_enabled(
                ui,
                "Allow connections through my GitHub account",
                "Signed-in clients can discover this host and open shared sessions without device pairing.",
                draft.ordinary_access == OrdinaryAccess::ProviderOwner,
                true,
            ) {
                draft.ordinary_access = match draft.ordinary_access {
                    OrdinaryAccess::OwnerOnly => OrdinaryAccess::ProviderOwner,
                    OrdinaryAccess::ProviderOwner => OrdinaryAccess::OwnerOnly,
                };
            }
            if settings_toggle_row_enabled(
                ui,
                "Allow remote clients to create shell sessions",
                "Off by default. Existing shared sessions remain accessible.",
                draft.allow_creation,
                true,
            ) {
                draft.allow_creation = !draft.allow_creation;
            }
            if settings_toggle_row_enabled(
                ui,
                "Allow remote clients to terminate sessions",
                "Off by default. Each termination requires confirmation. Closing a tab only disconnects it.",
                draft.allow_session_termination,
                true,
            ) {
                draft.allow_session_termination = !draft.allow_session_termination;
            }
            ui.add_space(6.0);
            let files_editable = matches!(&view.scope, Scope::Absent)
                || matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Disabled);
            if settings_toggle_row_enabled(ui,
                "Allow remote file access to one folder",
                "Off by default. Share files separately from shell sessions.",
                draft.files.enabled,
                files_editable && (draft.ordinary_access == OrdinaryAccess::ProviderOwner || draft.files.enabled),
            ) { draft.files.enabled = !draft.files.enabled; }
            if draft.files.enabled {
                ui.add_enabled_ui(files_editable, |ui| {
                    ui.label("Folder on this computer");
                    let mut root = draft.files.root.to_string_lossy().into_owned();
                    if ui.add(egui::TextEdit::singleline(&mut root).char_limit(4096)
                        .hint_text("Absolute folder path")).changed() { draft.files.root = root.into(); }
                    ui.radio_value(&mut draft.files.write, false, "Read only — browse and download");
                    ui.radio_value(&mut draft.files.write, true, "Read and write — upload and create folders");
                    if settings_toggle_row_enabled(ui,
                        "Allow deleting files and empty folders",
                        "Off by default. Permanent deletion requires a fresh client review.",
                        draft.files.delete, draft.files.write || draft.files.delete,
                    ) { draft.files.delete = !draft.files.delete; }
                    if draft.files.delete && !draft.files.write {
                        settings_note(ui, "Turn deleting off before choosing read-only access.");
                    }
                });
                settings_note(ui, "File transfers stay inside this folder and its subfolders. Shared shells still have your user's file and command permissions.");
                if draft.ordinary_access != OrdinaryAccess::ProviderOwner {
                    settings_note(ui, "Folder access requires account-based connections. Turn folder access off or enable account-based connections explicitly.");
                } else if !draft.files.root.is_absolute() {
                    settings_note(ui, "Enter an absolute local folder path before reviewing Enable.");
                }
            }
            if !files_editable {
                settings_note(ui, "Disable remote access before changing folder access.");
            }
            ui.add_space(6.0);
            let disable_to_configure = matches!(&view.scope, Scope::Present(snapshot)
                if snapshot.phase == Phase::Provisioned && snapshot.settings != draft.settings());
            if disable_to_configure {
                settings_note(ui, "Changes are not applied. Disable access, then re-enable to review the retained preferences. Durable shells stay running.");
                if action_button(ui, ActionButtonRole::DangerOutline, "Review Disable to change host settings…").clicked() {
                    action = Some(Action::Review(Operation::Disable));
                }
            } else {
                let label = if draft.ordinary_access == OrdinaryAccess::ProviderOwner {
                    "Review Enable and start…"
                } else {
                    "Review Enable…"
                };
                ui.add_enabled_ui(draft.valid_files(), |ui| {
                    if action_button(ui, ActionButtonRole::Accent, label).clicked() {
                        action = Some(Action::Review(Operation::Enable));
                    }
                });
            }
        }
        ui.collapsing("Advanced / Recovery", |ui| {
        ui.label(view.observation_label());
        settings_note(ui, "Recorded host version is metadata; signed components are verified again when updating.");
        if let Some(update) = view.release.as_ref().and_then(|release| release.replacement.as_ref()) {
            let state = match update.state {
                festerm_host_setup::services::ReplacementState::Prepared => "ready to apply",
                festerm_host_setup::services::ReplacementState::Applying => "incomplete; retry the same update",
                festerm_host_setup::services::ReplacementState::Committed => "completed",
                festerm_host_setup::services::ReplacementState::Aborted => "aborted; review Recovery",
            };
            ui.label(format!("Update {} → {}: {state}", update.from_version, update.to_version));
        }
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
        settings_note(ui, if draft.manual_bundle {
            "Explicit Advanced choice: verify the selected signed local bundle."
        } else {
            "Acquire and authenticate this exact current-version native host bundle from the official release. Missing assets block setup; no older/latest retry."
        });
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
        // Each recovery action gets its own row. At native DPI, a wrapped
        // horizontal row can leave only a few pixels for the final button,
        // turning its label into a column of single characters.
        ui.vertical(|ui| {
            if action_button(ui, ActionButtonRole::Secondary, "Refresh local status").clicked() {
                action = Some(Action::Refresh);
            }
            if view.bound_enable_discard.is_some()
                && action_button(ui, ActionButtonRole::Secondary, "Review interrupted setup…").clicked()
            {
                action = Some(Action::Review(Operation::DiscardBoundEnable));
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
        });
        if matches!(&view.scope, Scope::Present(snapshot) if snapshot.phase == Phase::Provisioned) {
            ui.collapsing("Advanced · manually pair an owner device", |ui| {
                settings_note(ui, "Optional administrative pairing. Normal account-based connections do not require these details or a separate device approval.");
                if action_button(ui, ActionButtonRole::Secondary, "Get host approval details…").clicked() {
                    action = Some(Action::Review(Operation::InspectOwner));
                }
                if let Some(binding) = &view.owner_binding {
                    ui.horizontal_wrapped(|ui| {
                        if action_button(ui, ActionButtonRole::Secondary, "Copy host details").clicked() {
                            ui.ctx().copy_text(binding.clone());
                        }
                        if let Ok(owner) = serde_json::from_str::<festerm_tunnel_bridge::owner_auth::OwnerBinding>(binding) {
                            if action_button(ui, ActionButtonRole::Secondary, "Copy host fingerprint").clicked() {
                                ui.ctx().copy_text(owner.host.host_key_fingerprint);
                            }
                        }
                    });
                    ui.collapsing("Public owner binding for independent client review", |ui| {
                        ui.monospace(binding);
                    });
                }
                ui.strong("Approve a client");
                settings_note(ui, "Get the host details first. Then copy the public key and fingerprint shown on the connecting device.");
                field(ui, "Owner public key · 64 hex characters", &mut draft.owner_public_key);
                field(ui, "Independently confirmed owner fingerprint", &mut draft.owner_fingerprint);
                ui.checkbox(&mut draft.approve_creation, "Also approve creation for this owner key");
                if action_button(ui, ActionButtonRole::Secondary, "Review owner approval…").clicked() {
                    action = Some(Action::Review(Operation::ApproveOwner));
                }
            });
        }
        if matches!(view.scope, Scope::Unobserved)
            && action_button(ui, ActionButtonRole::Secondary, "Refresh local status").clicked()
        {
            action = Some(Action::Refresh);
        }
        if let Some(operation) = view.review {
            let review = ui.group(|ui| {
                let (title, confirm_label) = confirmation_labels(operation, &draft);
                ui.strong(title);
                if operation == Operation::Reconcile {
                    settings_note(ui, "Recovery can retire an unsupported request only after verifying that its cloud host is absent. Review Enable again afterward.");
                }
                if operation == Operation::DiscardBoundEnable {
                    if let Scope::Present(snapshot) = &view.scope {
                        ui.label(format!("Retained host: {}", snapshot.settings.friendly_name));
                        if let Some(binding) = &snapshot.binding {
                            ui.label(format!("Retained account: {}", binding.account));
                        }
                    }
                    settings_note(ui, "Keep remote access off and preserve this host and its sessions. This clears the interrupted setup; it does not stop the host or check its cloud status. Review Disable next, then Enable to sign in again.");
                }
                match operation {
                    Operation::Enable => {
                        ui.label(format!("Host: {}", draft.friendly_name));
                        let account = view.accounts.iter()
                            .find(|account| account.account_id.to_string() == draft.account)
                            .map(|account| format!("GitHub account: @{}", account.login))
                            .unwrap_or_else(|| {
                                let id = view.bound_account().unwrap_or(draft.account.as_str());
                                if id.is_empty() { "GitHub account: choose an account".into() }
                                else { format!("GitHub account ID: {id}") }
                            });
                        ui.label(account);
                        ui.label(if draft.allow_creation {
                            "Create additional shells: On"
                        } else {
                            "Create additional shells: Off"
                        });
                        ui.label(if draft.allow_session_termination {
                            "Terminate remote sessions: On — confirm each session"
                        } else { "Terminate remote sessions: Off" });
                        if draft.files.enabled {
                            ui.label(format!("File access folder: {}", draft.files.root.display()));
                            ui.label(if draft.files.write {
                                "File permissions: Read and write — upload and create folders"
                            } else { "File permissions: Read only — browse and download" });
                            ui.label(if draft.files.delete {
                                "Delete files and empty folders: On — permanent, reviewed individually"
                            } else { "Delete files and empty folders: Off" });
                            settings_note(ui, "Clients authorized through this host's GitHub account can access this folder. The folder restriction applies to file transfers, not shared shells.");
                        } else { ui.label("Remote file access: Off"); }
                        ui.label(if draft.startup_at_sign_in {
                            "Start automatically at sign-in: On"
                        } else {
                            "Start automatically at sign-in: Off"
                        });
                        ui.label(if draft.manual_bundle {
                            "Use the signed host component selected in Advanced."
                        } else {
                            "Set up fesTerm's signed host component for your user on this computer."
                        });
                        ui.label("You may be asked to sign in. Setup can create a Dev Tunnel. No shell sessions are created or shared by enabling access.");
                        match draft.ordinary_access {
                            OrdinaryAccess::ProviderOwner => {
                                ui.label("Enable and start listening now. Starting automatically at OS sign-in remains a separate preference.");
                                ui.label("Allow clients signed in to this host's GitHub account to list and open shared sessions without device pairing.");
                                ui.label("A shared shell runs as your user on this computer and has that user's file and command permissions. Anyone holding a valid tunnel connection credential can obtain this access. New shells require the separate creation setting.");
                            }
                            OrdinaryAccess::OwnerOnly => {
                                ui.label("Account-based connections are disabled. Only separately paired owner devices can connect.");
                            }
                        }
                    }
                    Operation::ObserveNative => {
                        ui.label("Check whether the host process is running. This does not start or stop it.");
                        settings_note(ui, "A running process does not guarantee that a remote connection will work.");
                    }
                    Operation::ApproveOwner => {
                        ui.label("Approve this exact public key/fingerprint and selected permissions? Native vault errors never select file storage.");
                        settings_note(ui, "Cancelling in-flight permission work fences this host; persistent shells remain untouched.");
                    }
                    Operation::Update => {
                        let from = view.release.as_ref().map(|release| release.from_version()).unwrap_or("unavailable");
                        ui.label(format!("Update host components: {from} → {}", festerm_host_setup::VERSION));
                        ui.label("Remote access must be off. The host stays disabled after updating; enable access separately when you are ready.");
                        ui.label("Keep the host identity, account, sharing and existing durable shells. Edited preferences are not applied by this update.");
                        ui.label(if draft.manual_bundle { "Verify the original signed bundle selected in Advanced." } else { "Download and verify the exact signed host bundle for this app version." });
                        settings_note(ui, "An interrupted update keeps its original operation and target; retrying cannot select a replacement intent.");
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
                let footer = ui.horizontal(|ui| {
                    let role = if matches!(operation, Operation::Disable | Operation::Uninstall) {
                        ActionButtonRole::DangerOutline
                    } else {
                        ActionButtonRole::Accent
                    };
                    if action_button(ui, role, confirm_label).clicked() {
                        action = Some(Action::Confirm);
                    }
                    let cancel = action_button(ui, ActionButtonRole::Secondary, "Not now");
                    if reveal_review { cancel.request_focus(); }
                    if cancel.clicked() {
                        action = Some(Action::Dismiss);
                    }
                });
                footer.response
            });
            if reveal_review {
                if operation == Operation::Enable {
                    // Enable's consent summary can exceed the viewport. Reveal
                    // its explicit actions rather than centering a too-tall group.
                    review.inner.scroll_to_me(Some(egui::Align::Center));
                } else {
                    review.response.scroll_to_me(Some(egui::Align::Center));
                }
            }
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
    if view.busy && action_button(ui, ActionButtonRole::Secondary, "Cancel operation").clicked() {
        action = Some(Action::Cancel);
    }
    if view.cancelling {
        ui.label("Canceling. Please wait before starting another operation.");
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
    // A private catalog preference does not suppress errors from an explicit
    // exact-session operation. Keep them scoped to the matching review/status.
    if scoped.share_review.is_some() || scoped.sharing.is_some() {
        if let Some(failure) = scoped.failure {
            ui.colored_label(theme::STATUS_ERROR, failure.notice());
        }
    }
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

// Presentation labels only. The reviewed operation and protected backend scope
// are carried unchanged by the existing Review/Confirm command path.
fn confirmation_labels(operation: Operation, draft: &Draft) -> (&'static str, &'static str) {
    match operation {
        Operation::Enable if draft.ordinary_access == OrdinaryAccess::ProviderOwner => {
            ("Enable remote access?", "Enable and start")
        }
        Operation::Enable => ("Enable remote access?", "Enable"),
        Operation::ObserveNative => ("Check host process?", "Check process"),
        Operation::Refresh => ("Refresh local status?", "Refresh status"),
        Operation::InspectOwner => ("Review device access?", "Review access"),
        Operation::Reconcile => ("Recover host setup?", "Recover setup"),
        Operation::DiscardBoundEnable => {
            ("Discard interrupted setup?", "Discard interrupted setup")
        }
        Operation::ApproveOwner => ("Approve this device?", "Approve device"),
        Operation::Start => ("Start remote access?", "Start host"),
        Operation::Update => ("Update host components?", "Update host"),
        Operation::Disable => ("Disable remote access?", "Disable access"),
        Operation::Uninstall => ("Remove this host?", "Remove host"),
        Operation::Share(_) => ("Review session sharing?", "Continue"),
    }
}
