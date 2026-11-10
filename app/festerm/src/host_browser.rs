//! Foreground-only HOST consent and authorized inventory. Public recipes enter
//! as untrusted selection hints, never as native credential references.

use super::FesTermApp;
use crate::tabs::{AppCommand, AppState, TabContent, TabId};
use eframe::egui;
use festerm_config::{
    host_recipes::{AccountReference, ConnectionBehavior, Lifetime, PublicRecipe},
    DevTunnelAccount,
};
use festerm_secret_store::{SecretReference, SecretStore};
use festerm_tunnel_bridge::host_client::{
    self,
    approvals::{Receipt, ReceiptIndex, ReviewedOwner},
    ApprovedAttachment, ApprovedContext, CreationIntent, CreationMode, Inventory, Outcome, Scope,
    Target, Worker,
};
use festerm_tunnel_client::{
    host_control::{Binding, ConnectionPermit},
    identity::{self, Identity},
    service::{self, ResolvedTunnel},
};
use std::{
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

enum ResultEvent {
    Prepared(Receipt),
    Resolved(Box<(Receipt, ResolvedTunnel)>),
}

enum Action {
    Load,
    Prepare,
    Inventory,
    Attach(Target),
    Create,
}

pub(super) struct Browser {
    tab: TabId,
    request: egui::Id,
    recipe: PublicRecipe,
    account: DevTunnelAccount,
    account_reference: SecretReference,
    cancel: CancellationToken,
    prerequisite: zeroize::Zeroizing<String>,
    fingerprint: String,
    consent: bool,
    host_approved: bool,
    creation_confirmed: bool,
    locator: String,
    receipt: Option<Receipt>,
    resolved: Option<ResolvedTunnel>,
    receiver: Option<mpsc::Receiver<Result<ResultEvent, String>>>,
    pending_deadline: Option<(Instant, CancellationToken)>,
    worker: Option<Worker>,
    inventory: Option<Inventory>,
    generation: u64,
    feedback: Option<String>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Browser {
    fn current(&self, state: &AppState, context: &egui::Context) -> bool {
        state
            .tabs()
            .iter()
            .any(|tab| tab.id == self.tab && matches!(tab.content, TabContent::Profiles))
            && crate::screens::host_recipe_request_is_current(context, self.tab, self.request)
            && state.configuration().host_recipes().get(&self.recipe.id) == Some(&self.recipe)
            && state.configuration().accounts().iter().any(|account| {
                account.account_id() == self.account.account_id()
                    && account.credential_reference() == &self.account_reference
            })
    }

    fn busy(&self) -> bool {
        self.receiver.is_some() || self.worker.is_some()
    }

    fn context(&self) -> Result<ApprovedContext, String> {
        let receipt = self
            .receipt
            .as_ref()
            .ok_or("Load or prepare the protected receipt first.")?;
        let resolved = self
            .resolved
            .as_ref()
            .ok_or("Explicitly find authorized sessions first.")?;
        let binding = Binding::new(
            receipt.binding().host.host.clone(),
            receipt.binding().account.clone(),
            format!(
                "{}.{}/{}/{}",
                resolved.binding.tunnel_id,
                resolved.binding.cluster,
                resolved.binding.host_id,
                resolved.binding.port
            ),
        )
        .map_err(|_| "The selected endpoint binding is invalid.")?;
        ApprovedContext::from_protected_approval(
            receipt.binding().clone(),
            receipt.reference(),
            resolved.endpoint.clone(),
            resolved.binding.port,
            resolved.connect_token.clone(),
            binding,
        )
        .map_err(|error| error.message().to_owned())
    }

    fn scope(&self, context: &ApprovedContext) -> Scope {
        Scope {
            request: uuid::Uuid::new_v4()
                .to_string()
                .try_into()
                .expect("UUID operation"),
            generation: self.generation,
            binding: context.binding().clone(),
        }
    }

    fn allows_target(&self, target: &Target) -> bool {
        match &self.recipe.behavior {
            ConnectionBehavior::ChooseSession => true,
            ConnectionBehavior::ResumeOrCreate => target.logical == self.recipe.logical_session,
            ConnectionBehavior::AttachOnly { generation } => {
                target.logical == self.recipe.logical_session && target.generation == *generation
            }
            ConnectionBehavior::AlwaysCreate => false,
        }
    }

    fn creation_mode(&self) -> Option<CreationMode> {
        match self.recipe.behavior {
            ConnectionBehavior::ResumeOrCreate => Some(CreationMode::Ensure),
            ConnectionBehavior::AlwaysCreate | ConnectionBehavior::ChooseSession => {
                Some(CreationMode::Create)
            }
            ConnectionBehavior::AttachOnly { .. } => None,
        }
    }

    fn allows_creation(&self) -> bool {
        self.host_approved
            && self.creation_confirmed
            && self.inventory.as_ref().is_some_and(|inventory| {
                let caps = inventory.capabilities;
                caps.attach
                    && (matches!(self.recipe.lifetime, Lifetime::Persistent {}) || caps.temporary)
                    && match self.creation_mode() {
                        Some(CreationMode::Create) => caps.create && caps.creation_allowed,
                        Some(CreationMode::Ensure) => caps.ensure,
                        None => false,
                    }
            })
    }

    fn creation_request(&self) -> Option<(CreationMode, host_client::Creation)> {
        if !self.allows_creation() {
            return None;
        }
        let mode = self.creation_mode()?;
        Some((
            mode,
            host_client::Creation {
                logical: if mode == CreationMode::Ensure {
                    self.recipe.logical_session.clone()
                } else {
                    host_client::LogicalSessionId::new()
                },
                attachment: host_client::AttachmentId::new(),
                name: Some(self.recipe.name.clone()),
                recipe: self.recipe.launch.clone(),
                lifetime: self.recipe.lifetime,
            },
        ))
    }

    fn expire_pending(&mut self, now: Instant) {
        if self
            .pending_deadline
            .as_ref()
            .is_some_and(|(deadline, _)| now >= *deadline)
        {
            let (_, cancel) = self.pending_deadline.take().expect("expired deadline");
            cancel.cancel();
            self.receiver = None;
            self.feedback = Some("The protected owner worker timed out. Native preparation may have completed locally; reload the same protected receipt. No host session is known to be absent.".into());
        }
    }
}

impl crate::tabs::AppState {
    fn host_creation_intent(
        &mut self,
        recipe: &PublicRecipe,
        reference: &SecretReference,
        approved: &ApprovedContext,
        make: impl FnOnce() -> Option<(CreationMode, host_client::Creation)>,
    ) -> Result<Arc<CreationIntent>, String> {
        self.host_creation_intents
            .retain(|(_, _, intent)| !intent.is_presented());
        if let Some((original, credential, intent)) = self
            .host_creation_intents
            .iter()
            .find(|(original, _, _)| original.id == recipe.id)
        {
            if original != recipe || credential != reference {
                return Err("The original action remains unresolved under its original recipe/account credential. No replacement action was issued.".into());
            }
            return Ok(intent.clone());
        }
        if self.host_creation_intents.len() >= 16 {
            return Err("The bounded original-operation registry is full. Unresolved actions were retained; no new action was issued.".into());
        }
        let (mode, creation) = make().ok_or("Explicit creation approval is no longer current.")?;
        let intent = Arc::new(
            CreationIntent::new(approved, mode, creation)
                .map_err(|error| error.message().to_owned())?,
        );
        self.host_creation_intents.push((
            recipe.clone(),
            reference.duplicate_for_transport(),
            intent.clone(),
        ));
        Ok(intent)
    }
}

impl FesTermApp {
    pub(super) fn review_host(
        &mut self,
        tab: TabId,
        request: egui::Id,
        recipe: PublicRecipe,
        context: &egui::Context,
    ) {
        if !self.state.tabs().iter().any(|candidate| {
            candidate.id == tab && matches!(candidate.content, TabContent::Profiles)
        }) || !crate::screens::host_recipe_request_is_current(context, tab, request)
            || self.state.configuration().host_recipes().get(&recipe.id) != Some(&recipe)
        {
            return;
        }
        let AccountReference::Github { account_id } = recipe.host.account;
        let Some(account) = self
            .state
            .configuration()
            .accounts()
            .iter()
            .find(|account| account.account_id() == account_id)
            .cloned()
        else {
            crate::screens::report_host_recipe_outcome(context, tab, request,
                Err("This recipe's account is not signed in locally. Add or select that account explicitly in Profiles; no sign-in was started."));
            return;
        };
        if let Some(previous) = self.host_browser.take() {
            crate::screens::report_host_recipe_outcome(
                context,
                previous.tab,
                previous.request,
                Err("The previous foreground host review was cancelled."),
            );
        }
        self.host_browser = Some(Browser {
            tab,
            request,
            recipe,
            account_reference: account.credential_reference().duplicate_for_transport(),
            account,
            cancel: CancellationToken::new(),
            prerequisite: zeroize::Zeroizing::new(String::new()),
            fingerprint: String::new(),
            consent: false,
            host_approved: false,
            creation_confirmed: false,
            locator: String::new(),
            receipt: None,
            resolved: None,
            receiver: None,
            pending_deadline: None,
            worker: None,
            inventory: None,
            generation: 0,
            feedback: None,
        });
    }

    pub(super) fn poll_host_browser(&mut self, context: &egui::Context) {
        if self
            .host_browser
            .as_ref()
            .is_some_and(|browser| !browser.current(&self.state, context))
        {
            let browser = self.host_browser.take().expect("checked browser");
            crate::screens::report_host_recipe_outcome(context, browser.tab, browser.request,
                Err("Host review cancelled because its editor, recipe or account credential changed. No result was installed."));
            return;
        }
        let Some(browser) = self.host_browser.as_mut() else {
            return;
        };
        browser.expire_pending(Instant::now());
        let event = browser.receiver.as_ref().and_then(|receiver| match receiver.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("The bounded owner worker closed without a result. Its outcome is not known to be absent.".into())),
            Err(mpsc::TryRecvError::Empty) => None,
        });
        if let Some(event) = event {
            browser.receiver = None;
            browser.pending_deadline = None;
            match event {
                Ok(ResultEvent::Prepared(receipt)) => {
                    browser.receipt = Some(receipt);
                    browser.feedback = Some("Protected local key ready. This does not enroll or authorize it on the host. Review its public key below.".into());
                }
                Ok(ResultEvent::Resolved(pair)) => {
                    let (receipt, resolved) = *pair;
                    browser.receipt = Some(receipt);
                    browser.resolved = Some(resolved);
                    let result = browser.context().and_then(|approved| {
                        let scope = browser.scope(&approved);
                        let wake = context.clone();
                        Worker::start(
                            approved,
                            scope,
                            host_client::Operation::Inventory,
                            &self.state.host_client_budgets,
                            Arc::new(move || wake.request_repaint()),
                        )
                        .map_err(|error| error.message().to_owned())
                    });
                    match result {
                        Ok(worker) => browser.worker = Some(worker),
                        Err(error) => browser.feedback = Some(error),
                    }
                }
                Err(error) => browser.feedback = Some(error),
            }
        }
        let completion = browser
            .worker
            .as_ref()
            .and_then(|worker| worker.try_receive(worker.scope()));
        if let Some(completion) = completion {
            let completed_scope = completion.scope;
            browser.worker = None;
            match completion.outcome {
                Outcome::Inventory(inventory) => {
                    browser.inventory = Some(inventory);
                    browser.feedback = None;
                }
                Outcome::Failed(error) => browser.feedback = Some(error.message().into()),
                Outcome::Created(created) => {
                    let result = browser.context().and_then(|approved| {
                        ApprovedAttachment::from_created(approved, completed_scope, *created)
                            .map_err(|error| error.message().to_owned())
                    });
                    match result {
                        Ok(attachment) => {
                            let tab_id = browser.tab;
                            let request = browser.request;
                            self.state.dispatch(
                                AppCommand::OpenApprovedHostFromProfiles {
                                    tab_id,
                                    request,
                                    attachment: Box::new(attachment),
                                },
                                context,
                            );
                            self.host_browser = None;
                        }
                        Err(error) => browser.feedback = Some(error),
                    }
                }
                _ => {
                    browser.feedback =
                        Some("Unexpected inventory completion; no session was attached.".into())
                }
            }
        }
    }

    pub(super) fn show_host_browser(&mut self, context: &egui::Context, escape: bool) {
        let Some(browser) = self.host_browser.as_mut() else {
            return;
        };
        let mut dismiss = escape;
        let mut action = None;
        egui::Modal::new(egui::Id::new("foreground_host_review")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 620.0);
            ui.heading("Review selected HOST");
            super::confirmations::confirmation_body(ui, |ui| {
                ui.label(format!("Host: {}", browser.recipe.host.id));
                ui.label(format!("Account: github:{} · {}", browser.account.account_id(), browser.account.login()));
                ui.label("The recipe is an unverified hint. No discovery, sign-in or connection starts until you explicitly act here.");
                if let Some(message) = &browser.feedback { ui.label(message); }
                if browser.busy() { ui.spinner(); ui.label("Working on the selected host only…"); }
                ui.add_enabled_ui(!browser.busy(), |ui| {
                    if let Some(receipt) = &browser.receipt {
                        ui.separator();
                        ui.label(format!("Approved installation: {}", receipt.binding().installation));
                        ui.label(format!("Approved principal: {}", receipt.binding().principal));
                        ui.label(format!("Pinned host key: {}", receipt.binding().host.host_key_fingerprint));
                        let public_key: String = receipt.public_key().iter().map(|byte| format!("{byte:02x}")).collect();
                        let fingerprint = festerm_tunnel_bridge::owner_auth::owner_key_fingerprint(&receipt.public_key());
                        ui.label("Client owner PUBLIC key (not a private key or service token):");
                        ui.monospace(&public_key);
                        if let Ok(fingerprint) = fingerprint {
                            ui.monospace(&fingerprint);
                            if ui.button("Copy owner fingerprint").clicked() { ui.ctx().copy_text(fingerprint); }
                        }
                        if ui.button("Copy owner public key").clicked() { ui.ctx().copy_text(public_key); }
                        ui.label("On this exact host, run festerm-host approve-owner with this public key and its owner fingerprint. Choose each permission explicitly; nothing here grants host access.");
                        if ui.checkbox(&mut browser.host_approved, "I reviewed this full binding and completed the separate host-local owner approval").changed()
                            && !browser.host_approved
                        {
                            browser.inventory = None;
                            browser.resolved = None;
                            browser.creation_confirmed = false;
                        }
                        ui.label("Selected host's Dev Tunnel locator (id.cluster):");
                        if ui.add(egui::TextEdit::singleline(&mut browser.locator).char_limit(201)).changed() {
                            browser.inventory = None;
                            browser.resolved = None;
                            browser.creation_confirmed = false;
                        }
                        if ui.add_enabled(browser.host_approved && service::locator(&browser.locator).is_ok(),
                            egui::Button::new("Find authorized sessions on this host")).clicked() {
                            action = Some(Action::Inventory);
                        }
                    } else {
                        if ui.button("Load existing protected owner receipt").clicked() { action = Some(Action::Load); }
                        ui.separator();
                        ui.label("First connection: paste ONLY the full binding object from this host's owner_approval_prerequisite. Do not paste credentials or a secret reference.");
                        ui.add(egui::TextEdit::multiline(&mut *browser.prerequisite).char_limit(4096).desired_rows(5));
                        ui.label("Independently verify and enter the SHA256 host-key fingerprint from the host:");
                        ui.add(egui::TextEdit::singleline(&mut browser.fingerprint).char_limit(128));
                        ui.checkbox(&mut browser.consent, "I verified the installation, host, account, principal and host-key fingerprint on the selected host");
                        if ui.add_enabled(browser.consent && !browser.fingerprint.is_empty(),
                            egui::Button::new("Trust this exact binding and prepare native owner key")).clicked() {
                            action = Some(Action::Prepare);
                        }
                    }
                    if let Some(inventory) = &browser.inventory {
                        ui.separator();
                        ui.label("Authorized host inventory");
                        if !inventory.unknown.is_empty() {
                            ui.label(format!("{} target(s) remain unknown. Unknown never means absent.", inventory.unknown.len()));
                        }
                        if inventory.sessions.is_empty() {
                            ui.label("No authorized sessions returned. This is not permission or authority to create a shell.");
                        }
                        ui.label(format!("Host permissions: create {} · ensure {} · creation grant {} · temporary {}",
                            inventory.capabilities.create, inventory.capabilities.ensure,
                            inventory.capabilities.creation_allowed, inventory.capabilities.temporary));
                        ui.label(match browser.recipe.behavior {
                            ConnectionBehavior::ChooseSession => "Recipe: choose an existing session, or explicitly create a fresh logical session.",
                            ConnectionBehavior::ResumeOrCreate => "Recipe: explicit ensure lets the host resume this logical session or authorize creation under its journal. Client inventory is never absence authority.",
                            ConnectionBehavior::AlwaysCreate => "Recipe: explicitly create one fresh logical session. An uncertain reply retains this same action, never another shell.",
                            ConnectionBehavior::AttachOnly { .. } => "Recipe: attach only to its exact logical session and generation.",
                        });
                        if browser.creation_mode().is_some() {
                            ui.monospace(format!("shell {:?}\nargv {:?}\ncwd {:?}\nlifetime {:?}",
                                browser.recipe.launch.shell, browser.recipe.launch.args,
                                browser.recipe.launch.cwd, browser.recipe.lifetime));
                            ui.checkbox(&mut browser.creation_confirmed,
                                "I approve executing these exact launch options and lifetime if the host authorizes creation");
                            if let Some((_, _, intent)) = self.state.host_creation_intents.iter()
                                .find(|(recipe, _, _)| recipe.id == browser.recipe.id) {
                                ui.monospace(format!("Original action · logical {} · attachment {} · issued {:?}",
                                    intent.creation().logical, intent.creation().attachment,
                                    intent.issued_operation()));
                                if intent.is_resolved() && !intent.is_presented() {
                                    ui.label("The host confirmed this action, but it was not admitted for presentation. Retry retains this same action and cannot create a replacement.");
                                }
                            }
                            if ui.add_enabled(browser.allows_creation(),
                                egui::Button::new("Execute / retry the original create or ensure action")).clicked() {
                                action = Some(Action::Create);
                            }
                            ui.label("A failed/unknown action is retained in this app process, even after this review closes. Retry reuses its original logical ID, attachment and issued operation. Process restart is not durable reconciliation.");
                            if !inventory.capabilities.creation_allowed {
                                ui.label("Creation permission is not granted. Ensure may only resume an authorized existing session; absence must be refused by the host.");
                            }
                        }
                        for session in &inventory.sessions {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(format!("{} · logical {} · generation {} · {}", session.name, session.target.logical,
                                    session.target.generation.get(), if session.attached { "already attached / busy" } else { "available" }));
                                ui.label(match session.lifetime {
                                    Lifetime::Persistent {} => "Persistent: disconnect keeps the shell running.",
                                    Lifetime::Temporary { .. } => "Temporary: intentional disconnect ends this shell; transport loss has a 60-second grace.",
                                });
                                if ui.add_enabled(browser.host_approved && inventory.capabilities.attach && !session.attached && browser.allows_target(&session.target),
                                    egui::Button::new("Attach this existing session")).clicked() {
                                    action = Some(Action::Attach(session.target.clone()));
                                }
                            });
                        }
                    }
                });
            });
            if ui.button("Cancel").clicked() { dismiss = true; }
        });
        if dismiss {
            let browser = self.host_browser.take().expect("visible browser");
            crate::screens::report_host_recipe_outcome(
                context,
                browser.tab,
                browser.request,
                Err("Foreground host review cancelled. Local key preparation may have completed; reload the same protected receipt. No host enrollment or input replay was performed."),
            );
            return;
        }
        if let Some(action) = action {
            self.apply_host_review_action(action, context);
        }
    }

    fn apply_host_review_action(&mut self, action: Action, context: &egui::Context) {
        if !self
            .host_browser
            .as_ref()
            .is_some_and(|browser| browser.current(&self.state, context) && !browser.busy())
        {
            return;
        }
        let browser = self.host_browser.as_mut().expect("current browser");
        browser.feedback = None;
        if matches!(action, Action::Create) {
            if !browser.allows_creation() {
                return;
            }
            let approved = match browser.context() {
                Ok(approved) => approved,
                Err(error) => {
                    browser.feedback = Some(error);
                    return;
                }
            };
            let intent = match self.state.host_creation_intent(
                &browser.recipe,
                &browser.account_reference,
                &approved,
                || browser.creation_request(),
            ) {
                Ok(intent) => intent,
                Err(error) => {
                    browser.feedback = Some(error);
                    return;
                }
            };
            let Some(generation) = browser.generation.checked_add(1) else {
                return;
            };
            browser.generation = generation;
            let scope = browser.scope(&approved);
            let wake = context.clone();
            match Worker::start(
                approved,
                scope,
                host_client::Operation::Create(intent),
                &self.state.host_client_budgets,
                Arc::new(move || wake.request_repaint()),
            ) {
                Ok(worker) => browser.worker = Some(worker),
                Err(error) => browser.feedback = Some(error.message().into()),
            }
            return;
        }
        if let Action::Attach(target) = action {
            let selected = browser.inventory.as_ref().is_some_and(|inventory| {
                browser.host_approved
                    && inventory.capabilities.attach
                    && browser.allows_target(&target)
                    && inventory
                        .sessions
                        .iter()
                        .any(|session| session.target == target && !session.attached)
            });
            if !selected {
                return;
            }
            let attachment = browser.context().and_then(|approved| {
                let scope = browser.scope(&approved);
                ApprovedAttachment::new(approved, scope, target)
                    .map_err(|error| error.message().into())
            });
            match attachment {
                Ok(attachment) => {
                    let tab_id = browser.tab;
                    let request = browser.request;
                    self.state.dispatch(
                        AppCommand::OpenApprovedHostFromProfiles {
                            tab_id,
                            request,
                            attachment: Box::new(attachment),
                        },
                        context,
                    );
                    self.host_browser = None;
                }
                Err(error) => browser.feedback = Some(error),
            }
            return;
        }
        let Ok(permit) = ConnectionPermit::reserve(&self.state.host_client_budgets) else {
            browser.feedback = Some(
                "The aggregate HOST worker budget is busy. No vault or network work started."
                    .into(),
            );
            return;
        };
        let reviewed = if matches!(action, Action::Prepare) {
            if !browser.consent {
                return;
            }
            match ReviewedOwner::confirm(
                &browser.prerequisite,
                &browser.recipe.host.id,
                &format!("github:{}", browser.account.account_id()),
                &browser.fingerprint,
            ) {
                Ok(reviewed) => Some(reviewed),
                Err(error) => {
                    browser.feedback = Some(error.message().into());
                    return;
                }
            }
        } else {
            None
        };
        if matches!(action, Action::Inventory)
            && (!browser.host_approved || browser.receipt.is_none())
        {
            return;
        }
        if matches!(action, Action::Inventory) && service::locator(&browser.locator).is_err() {
            browser.feedback = Some("Enter the selected host's valid Dev Tunnel locator. No vault or network work started.".into());
            return;
        }
        let store = match &self.secret_store {
            Ok(store) => Arc::clone(store),
            Err(_) => {
                browser.feedback =
                    Some("Native secure storage is unavailable. No fallback is allowed.".into());
                return;
            }
        };
        let account = browser.account.clone();
        let host = browser.recipe.host.id.clone();
        let locator = browser.locator.clone();
        let cancel = browser.cancel.clone();
        let wake = context.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        browser.inventory = None;
        browser.resolved = None;
        browser.host_approved = matches!(action, Action::Inventory);
        let Some(generation) = browser.generation.checked_add(1) else {
            browser.feedback = Some(
                "The foreground operation generation is exhausted. Reopen this review.".into(),
            );
            return;
        };
        browser.generation = generation;
        let cancel = cancel.child_token();
        let pending_cancel = cancel.clone();
        let duration = Duration::from_secs(if matches!(action, Action::Inventory) {
            45
        } else {
            15
        });
        let spawned = std::thread::Builder::new().name("festerm-host-approval".into()).spawn(move || {
            let _permit = permit;
            let result = (|| {
                if cancel.is_cancelled() { return Err("Host review cancelled.".into()); }
                let index = ReceiptIndex::native().map_err(|error| error.message().to_owned())?;
                let receipt = match reviewed {
                    Some(reviewed) => index.prepare(reviewed),
                    None => index.load(&host, &format!("github:{}", account.account_id())),
                }.map_err(|error| error.message().to_owned())?;
                if cancel.is_cancelled() { return Err("Host review cancelled; protected preparation may have completed locally. No connection was authorized.".into()); }
                if !matches!(action, Action::Inventory) { return Ok(ResultEvent::Prepared(receipt)); }
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(|_| "The selected-host resolver worker could not start.".to_owned())?;
                let resolved = runtime.block_on(async {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => Err("Host resolution cancelled.".into()),
                        value = tokio::time::timeout(Duration::from_secs(30), resolve_host(&account, store.as_ref(), &locator, &cancel)) =>
                            value.map_err(|_| "Host resolution timed out. No session is known to be absent.".to_owned())?,
                    }
                })?;
                Ok(ResultEvent::Resolved(Box::new((receipt, resolved))))
            })();
            if !cancel.is_cancelled() { let _ = sender.try_send(result); wake.request_repaint(); }
        });
        match spawned {
            Ok(_) => {
                browser.receiver = Some(receiver);
                browser.pending_deadline = Some((Instant::now() + duration, pending_cancel));
                context.request_repaint_after(duration);
            }
            Err(_) => {
                browser.feedback =
                    Some("The bounded protected owner worker could not start.".into())
            }
        }
    }
}

async fn resolve_host(
    account: &DevTunnelAccount,
    store: &dyn SecretStore,
    locator: &str,
    cancel: &CancellationToken,
) -> Result<ResolvedTunnel, String> {
    let client = identity::registered_client_id()
        .ok_or("Account sign-in is not configured in this build.")?;
    let secret = store.get(account.credential_reference()).map_err(|_| "The selected account's native credential is unavailable. No sign-in or fallback was attempted.")?;
    let identity = secret
        .with_bytes(|bytes| Identity::decode_private(bytes, account.account_id(), client))
        .map_err(|error| error.to_string())?;
    if identity.needs_sign_in() {
        return Err("The selected account requires foreground sign-in in Profiles. No background sign-in or refresh was started.".into());
    }
    service::resolve_host(&identity, locator, cancel).await.map_err(|error| {
        if error == festerm_tunnel_client::Error::Tunnel {
            "The selected endpoint does not advertise the HOST protocol. No legacy bridge fallback was attempted.".into()
        } else { format!("{} No session absence was inferred.", error) }
    })
}

#[cfg(test)]
#[path = "host_browser_tests.rs"]
mod tests;
