//! Foreground HOST commands. Metadata, discovery and native approval stay distinct.
use super::{
    live_ssh,
    profiles::Profiles,
    tunnel_link::{Job, Poll},
    MobileApp, Surface,
};
use eframe::egui;
use festerm_host_protocol::{AttachmentId, Creation, LogicalSessionId, OperationId, Recipe};
use festerm_mobile_profiles::{
    host_recipes::{
        AccountReference, ConnectionBehavior, HostReference, MetadataCommand, PublicRecipe,
    },
    TunnelAccount,
};
use festerm_tunnel_bridge::{
    host_client::{
        self,
        approvals::{Receipt, ReceiptIndex, ReviewedOwner},
        ApprovedAttachment, ApprovedContext, Budgets, CreationIntent, CreationMode, HostSession,
        Inventory, Outcome, Scope, Worker,
    },
    owner_auth::{owner_key_fingerprint, OwnerBinding},
};
use festerm_tunnel_client::{
    host_control::{Binding, ConnectionPermit},
    identity::{self, Identity},
    service::{self, ResolvedTunnel, TunnelSummary},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Default)]
pub(super) struct Privacy {
    epoch: AtomicU64,
    owners: Mutex<Owners>,
}

#[derive(Default)]
struct Owners {
    tokens: Vec<Weak<CancellationToken>>,
    workers: Vec<Weak<Worker>>,
    sessions: Vec<Weak<HostSession>>,
}

impl Privacy {
    pub(super) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    pub(super) fn invalidate(&self) {
        let _ = self
            .epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            });
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for token in owners.tokens.drain(..).filter_map(|token| token.upgrade()) {
            token.cancel();
        }
        for worker in owners
            .workers
            .drain(..)
            .filter_map(|worker| worker.upgrade())
        {
            worker.cancel();
        }
        for session in owners
            .sessions
            .drain(..)
            .filter_map(|session| session.upgrade())
        {
            session.cancel();
        }
    }

    fn token(&self, epoch: u64) -> Arc<CancellationToken> {
        let token = Arc::new(CancellationToken::new());
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        owners.tokens.retain(|owner| owner.strong_count() > 0);
        if self.epoch() != epoch || epoch == u64::MAX {
            token.cancel();
        } else {
            owners.tokens.push(Arc::downgrade(&token));
        }
        token
    }

    fn worker(&self, epoch: u64, worker: &Arc<Worker>) {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        owners.workers.retain(|owner| owner.strong_count() > 0);
        if self.epoch() != epoch || epoch == u64::MAX {
            worker.cancel();
        } else {
            owners.workers.push(Arc::downgrade(worker));
        }
    }

    pub(super) fn session(&self, epoch: u64, session: &Arc<HostSession>) -> bool {
        let mut owners = self
            .owners
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        owners.sessions.retain(|owner| owner.strong_count() > 0);
        if self.epoch() != epoch || epoch == u64::MAX {
            session.cancel();
            false
        } else {
            owners.sessions.push(Arc::downgrade(session));
            true
        }
    }
}

enum ResultEvent {
    Hosts(Vec<TunnelSummary>),
    Receipt(Box<Receipt>),
    Resolved(Box<ResolvedTunnel>),
}

struct Pending {
    account: TunnelAccount,
    recipe: Option<PublicRecipe>,
    epoch: u64,
    token: Arc<CancellationToken>,
    job: Job<Result<ResultEvent, String>>,
}

struct Original {
    recipe: PublicRecipe,
    account: TunnelAccount,
    intent: Arc<CreationIntent>,
}

#[derive(Default)]
pub(super) struct HostUi {
    pub(super) open: bool,
    account: String,
    selected: Option<PublicRecipe>,
    saved: bool,
    locator: String,
    prerequisite: Zeroizing<String>,
    fingerprint: String,
    trust: bool,
    enrolled: bool,
    launch: bool,
    import_reviewed: bool,
    import: String,
    hosts: Vec<TunnelSummary>,
    receipt: Option<Receipt>,
    approval_account: Option<TunnelAccount>,
    resolved: Option<ResolvedTunnel>,
    inventory: Option<Inventory>,
    pending: Option<Pending>,
    worker: Option<Arc<Worker>>,
    worker_account: Option<TunnelAccount>,
    session_accounts: Vec<(Weak<HostSession>, TunnelAccount)>,
    originals: Vec<Original>,
    budgets: Budgets,
    epoch: u64,
    generation: u64,
    error: Option<String>,
}

impl HostUi {
    fn cancel(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.token.cancel();
        }
        if let Some(worker) = self.worker.take() {
            worker.cancel();
        }
        self.worker_account = None;
    }

    fn clear_approval(&mut self) {
        self.cancel();
        self.receipt = None;
        self.approval_account = None;
        self.resolved = None;
        self.inventory = None;
        self.trust = false;
        self.enrolled = false;
        self.launch = false;
    }

    fn clear_review(&mut self) {
        self.clear_approval();
        self.selected = None;
        self.saved = false;
        self.locator.clear();
        self.fingerprint.clear();
        self.prerequisite = Zeroizing::new(String::new());
    }

    fn context(&self) -> Result<ApprovedContext, String> {
        if !self.enrolled {
            return Err(host_client::ENROLLMENT_PREREQUISITE.into());
        }
        let receipt = self
            .receipt
            .as_ref()
            .ok_or(host_client::ENROLLMENT_PREREQUISITE)?;
        let resolved = self
            .resolved
            .as_ref()
            .ok_or("Explicitly resolve this selected HOST endpoint first.")?;
        if self.fingerprint != receipt.binding().host.host_key_fingerprint {
            return Err("The independently confirmed host fingerprint changed.".into());
        }
        let binding = Binding::new(
            receipt.binding().host.host.clone(),
            receipt.binding().account.clone(),
            format!(
                "{}.{}",
                resolved.binding.tunnel_id, resolved.binding.cluster
            ),
        )
        .map_err(|error| error.to_string())?;
        ApprovedContext::from_protected_approval(
            receipt.binding().clone(),
            receipt.reference(),
            resolved.endpoint.clone(),
            resolved.binding.port,
            resolved.connect_token.clone(),
            binding,
        )
        .map_err(|error| error.message().into())
    }

    fn creation_intent(
        &mut self,
        context: &ApprovedContext,
        recipe: PublicRecipe,
        account: TunnelAccount,
    ) -> Result<Arc<CreationIntent>, String> {
        let mode = match recipe.behavior {
            ConnectionBehavior::ResumeOrCreate => CreationMode::Ensure,
            ConnectionBehavior::AlwaysCreate | ConnectionBehavior::ChooseSession => {
                CreationMode::Create
            }
            ConnectionBehavior::AttachOnly { .. } => {
                return Err("Attach-only cannot create.".into())
            }
        };
        self.originals
            .retain(|original| !original.intent.is_presented());
        if let Some(original) = self
            .originals
            .iter()
            .find(|original| original.recipe.id == recipe.id)
        {
            if original.recipe != recipe || original.account != account {
                return Err("Original action retained under its original recipe/account; no replacement was issued.".into());
            }
            return Ok(original.intent.clone());
        }
        if self.originals.len() >= 16 {
            return Err("Original-action registry full; unresolved actions retained.".into());
        }
        let creation = Creation {
            logical: if mode == CreationMode::Ensure {
                recipe.logical_session.clone()
            } else {
                LogicalSessionId::new()
            },
            attachment: AttachmentId::new(),
            recipe: recipe.launch.clone(),
            lifetime: recipe.lifetime,
            name: Some(recipe.name.clone()),
        };
        let intent = Arc::new(
            CreationIntent::new(context, mode, creation)
                .map_err(|error| error.message().to_owned())?,
        );
        self.originals.push(Original {
            recipe,
            account,
            intent: intent.clone(),
        });
        Ok(intent)
    }
}

#[derive(Clone, Copy)]
enum Action {
    NewReview,
    ReviewOriginal(usize),
    Find,
    Prepare,
    Load,
    Resolve,
    Create,
    Attach(usize),
    Save,
    Import,
    Close,
}

fn selected_target(
    recipe: Option<&PublicRecipe>,
    target: &host_client::Target,
) -> Result<(), String> {
    match recipe.map(|recipe| (&recipe.behavior, &recipe.logical_session)) {
        Some((ConnectionBehavior::AlwaysCreate, _)) => {
            Err("Always-create cannot substitute an existing attachment.".into())
        }
        Some((ConnectionBehavior::ResumeOrCreate, logical)) if logical != &target.logical => Err(
            "Resume must keep this recipe's logical session; use its explicit Ensure action."
                .into(),
        ),
        Some((ConnectionBehavior::AttachOnly { generation }, logical))
            if logical != &target.logical || generation != &target.generation =>
        {
            Err("Exact logical session/generation required; no retargeting.".into())
        }
        _ => Ok(()),
    }
}

impl MobileApp {
    pub(super) fn open_hosts(&mut self) {
        self.hosts.clear_approval();
        self.hosts.epoch = self.sink.live.host.epoch();
        self.hosts.open = true;
    }

    pub(super) fn review_host_reconnection(&mut self) {
        self.open_hosts();
        self.hosts.clear_review();
        self.hosts.error = Some("Explicitly select the original host and exact session again. No legacy fallback, new shell or input replay was started.".into());
    }

    fn host_account(&self) -> Option<TunnelAccount> {
        let store = self.profiles.as_ref()?.store_handle();
        let store = store.lock().ok()?;
        store
            .accounts()
            .iter()
            .find(|account| account.id == self.hosts.account)
            .cloned()
    }

    fn host_foreground(&self) -> bool {
        self.lifecycle.get().active
            && self
                .lock
                .as_ref()
                .is_none_or(|lock| lock.content_available())
            && self.hosts.epoch == self.sink.live.host.epoch()
            && self.hosts.epoch != u64::MAX
    }

    fn host_recipe_current(&self) -> bool {
        if !self.hosts.saved {
            return true;
        }
        let Some(recipe) = self.hosts.selected.as_ref() else {
            return false;
        };
        let Some(profiles) = self.profiles.as_ref() else {
            return false;
        };
        let store = profiles.store_handle();
        let Ok(store) = store.lock() else {
            return false;
        };
        store.host_recipes().get(&recipe.id) == Some(recipe)
    }

    pub(super) fn poll_hosts(&mut self, ctx: &egui::Context) {
        if let Some(error) = self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .and_then(live_ssh::LiveSsh::host_error)
        {
            self.notice = Some(error.to_owned());
        }
        self.hosts
            .session_accounts
            .retain(|(session, _)| session.strong_count() > 0);
        let current_accounts = self
            .profiles
            .as_ref()
            .map(Profiles::store_handle)
            .and_then(|store| store.lock().ok().map(|store| store.accounts().to_vec()))
            .unwrap_or_default();
        if self
            .hosts
            .session_accounts
            .iter()
            .any(|(_, expected)| !current_accounts.contains(expected))
        {
            self.sink.live.host.invalidate();
            self.hosts.session_accounts.clear();
        }
        if !self.host_foreground() || !self.host_recipe_current() {
            self.hosts.clear_approval();
            self.hosts.epoch = self.sink.live.host.epoch();
            self.hosts.error = Some("HOST work cancelled by lock, suspension, account or recipe change. Original actions remain; no input is replayed.".into());
            return;
        }
        let account = self.host_account();
        if account.is_none() {
            self.hosts.hosts.clear();
        }
        let stale = self.hosts.pending.as_ref().is_some_and(|pending| {
            account.as_ref() != Some(&pending.account)
                || pending.epoch != self.hosts.epoch
                || pending.recipe != self.hosts.selected
        }) || self
            .hosts
            .approval_account
            .as_ref()
            .is_some_and(|expected| account.as_ref() != Some(expected))
            || self
                .hosts
                .worker_account
                .as_ref()
                .is_some_and(|expected| account.as_ref() != Some(expected));
        if stale {
            self.hosts.clear_approval();
            self.hosts.error =
                Some("The original account credential changed; no result was installed.".into());
            return;
        }
        let completed = self
            .hosts
            .pending
            .as_ref()
            .and_then(|pending| match pending.job.poll() {
                Poll::Pending => None,
                Poll::Done(result) => Some(result),
                Poll::Lost => Some(Err(
                    "HOST worker closed; no absence or success was inferred.".into(),
                )),
            });
        if let Some(result) = completed {
            self.hosts.pending = None;
            match result {
                Ok(ResultEvent::Hosts(hosts)) => self.hosts.hosts = hosts,
                Ok(ResultEvent::Receipt(receipt)) => {
                    self.hosts.enrolled = false;
                    self.hosts.launch = false;
                    self.hosts.receipt = Some(*receipt);
                    self.hosts.approval_account = account.clone();
                }
                Ok(ResultEvent::Resolved(resolved)) => {
                    self.hosts.resolved = Some(*resolved);
                    if let Err(error) =
                        self.start_host_operation(ctx, host_client::Operation::Inventory)
                    {
                        self.hosts.error = Some(error);
                    }
                }
                Err(error) => self.hosts.error = Some(error),
            }
        }
        let result = self
            .hosts
            .worker
            .as_ref()
            .and_then(|worker| worker.try_receive(worker.scope()));
        if let Some(result) = result {
            self.hosts.worker = None;
            self.hosts.worker_account = None;
            match result.outcome {
                Outcome::Inventory(inventory) => self.hosts.inventory = Some(inventory),
                Outcome::Created(created) => {
                    let attachment = self.hosts.context().and_then(|context| {
                        ApprovedAttachment::from_created(context, result.scope, *created)
                            .map_err(|error| error.message().into())
                    });
                    match attachment {
                        Ok(attachment) => self.install_host(ctx, attachment),
                        Err(error) => self.hosts.error = Some(error),
                    }
                }
                Outcome::Failed(error) => self.hosts.error = Some(error.message().into()),
                _ => {
                    self.hosts.error =
                        Some("Unexpected HOST control result; no terminal was installed.".into())
                }
            }
        }
    }

    fn start_host_operation(
        &mut self,
        ctx: &egui::Context,
        operation: host_client::Operation,
    ) -> Result<(), String> {
        if !self.host_foreground() || self.hosts.worker.is_some() {
            return Err("HOST action is not current or already busy.".into());
        }
        let account = self
            .host_account()
            .ok_or("Select an explicitly signed-in account.")?;
        let context = self.hosts.context()?;
        self.hosts.generation = self
            .hosts
            .generation
            .checked_add(1)
            .ok_or("HOST request generation exhausted.")?;
        let scope = Scope {
            request: OperationId::new(),
            generation: self.hosts.generation,
            binding: context.binding().clone(),
        };
        let repaint = ctx.clone();
        let token = self.sink.live.host.token(self.hosts.epoch);
        let worker = Arc::new(
            Worker::start_with_cancellation(
                context,
                scope,
                operation,
                &self.hosts.budgets,
                Arc::new(move || repaint.request_repaint()),
                token.as_ref().clone(),
            )
            .map_err(|error| error.message().to_owned())?,
        );
        self.sink.live.host.worker(self.hosts.epoch, &worker);
        self.hosts.worker_account = Some(account);
        self.hosts.worker = Some(worker);
        Ok(())
    }

    fn install_host(&mut self, ctx: &egui::Context, attachment: ApprovedAttachment) {
        if !self.host_foreground() {
            return;
        }
        let title = attachment.description();
        let token = self.sink.live.host.token(self.hosts.epoch);
        let session = match attachment.start_with_cancellation(
            &self.hosts.budgets,
            Arc::new(live_ssh::Repaint(ctx.clone())),
            token.as_ref().clone(),
        ) {
            Ok(session) => Arc::new(session),
            Err(error) => {
                self.hosts.error = Some(error.message().into());
                return;
            }
        };
        self.install_host_session(ctx, title, session);
    }

    fn install_host_session(
        &mut self,
        ctx: &egui::Context,
        title: String,
        session: Arc<HostSession>,
    ) {
        if !self.host_foreground() {
            session.cancel();
            return;
        }
        if !self.sink.live.host.session(self.hosts.epoch, &session) {
            return;
        }
        let Some(account) = self.host_account() else {
            session.cancel();
            return;
        };
        if self.hosts.approval_account.as_ref() != Some(&account)
            || session.account() != format!("github:{}", account.account_id)
        {
            session.cancel();
            self.hosts.error =
                Some("The original protected account changed before terminal installation.".into());
            return;
        }
        self.hosts
            .session_accounts
            .push((Arc::downgrade(&session), account));
        self.park_active();
        let id = self.push_tab(title, None);
        self.tabs.last_mut().expect("new tab").host = true;
        self.terminal = self.blank_terminal();
        self.view = self.fresh_view();
        self.active_tab = Some(id);
        *self.sink.live.connection.borrow_mut() = Some(live_ssh::LiveSsh::from_host(session, ctx));
        self.surface = Surface::Terminal;
        self.workspace = super::workspaces::Workspace::Terminal;
        self.clear_sticky_modifiers();
        self.hosts.cancel();
        self.hosts.open = false;
    }

    fn host_action(&mut self, ctx: &egui::Context, action: Action) -> Result<(), String> {
        if matches!(action, Action::Close) {
            self.hosts.cancel();
            self.hosts.open = false;
            return Ok(());
        }
        if matches!(action, Action::NewReview) {
            self.hosts.clear_review();
            return Ok(());
        }
        if let Action::ReviewOriginal(index) = action {
            let original = self
                .hosts
                .originals
                .get(index)
                .ok_or("Original action selection expired.")?;
            let (recipe, account) = (original.recipe.clone(), original.account.id.clone());
            self.hosts.clear_review();
            if self.hosts.account != account {
                self.hosts.hosts.clear();
            }
            self.hosts.account = account;
            self.hosts.selected = Some(recipe);
            return Ok(());
        }
        if !self.host_foreground() || !self.host_recipe_current() {
            return Err(
                "Unlock and foreground the app, then explicitly review the original host again."
                    .into(),
            );
        }
        let profiles = self
            .profiles
            .as_ref()
            .ok_or("Native mobile profile storage is unavailable.")?;
        let store = profiles.store_handle();
        if matches!(action, Action::Import) {
            if !self.hosts.import_reviewed {
                return Err(
                    "Review the public recipe before import; it grants no execution approval."
                        .into(),
                );
            }
            store
                .lock()
                .map_err(|_| "Profile store busy.")?
                .apply_host_recipe_command(MetadataCommand::Import(self.hosts.import.clone()))
                .map_err(|error| error.to_string())?;
            self.hosts.clear_approval();
            self.hosts.import_reviewed = false;
            return Ok(());
        }
        if matches!(action, Action::Save) {
            let recipe = self
                .hosts
                .selected
                .clone()
                .ok_or("Review a host before saving its recipe.")?;
            store
                .lock()
                .map_err(|_| "Profile store busy.")?
                .apply_host_recipe_command(MetadataCommand::Save(recipe))
                .map_err(|error| error.to_string())?;
            self.hosts.saved = true;
            return Ok(());
        }
        if self.hosts.pending.is_some() || self.hosts.worker.is_some() {
            return Err("One foreground HOST action is already running.".into());
        }
        let account = self
            .host_account()
            .ok_or("Choose a signed-in account in Profiles; no background sign-in was started.")?;
        if self
            .hosts
            .approval_account
            .as_ref()
            .is_some_and(|expected| expected != &account)
        {
            self.hosts.clear_approval();
            return Err(
                "The approved account credential changed; review the original host again.".into(),
            );
        }
        if self.hosts.selected.as_ref().is_some_and(|recipe| {
            recipe.host.account
                != (AccountReference::Github {
                    account_id: account.account_id,
                })
        }) {
            return Err("This recipe belongs to another account. No operation was issued.".into());
        }
        match action {
            Action::Create => {
                if !self.hosts.launch {
                    return Err("Confirm the exact recipe and lifetime separately.".into());
                }
                let recipe = self
                    .hosts
                    .selected
                    .clone()
                    .ok_or("Choose a public recipe first.")?;
                let context = self.hosts.context()?;
                let intent = self.hosts.creation_intent(&context, recipe, account)?;
                return self.start_host_operation(ctx, host_client::Operation::Create(intent));
            }
            Action::Attach(index) => {
                let inventory = self
                    .hosts
                    .inventory
                    .as_ref()
                    .ok_or("Explicitly fetch authorized inventory first.")?;
                let target = inventory
                    .sessions
                    .get(index)
                    .ok_or("Session selection expired.")?
                    .target
                    .clone();
                selected_target(self.hosts.selected.as_ref(), &target)?;
                let context = self.hosts.context()?;
                self.hosts.generation = self
                    .hosts
                    .generation
                    .checked_add(1)
                    .ok_or("HOST request generation exhausted.")?;
                let scope = Scope {
                    request: OperationId::new(),
                    generation: self.hosts.generation,
                    binding: context.binding().clone(),
                };
                let attachment = ApprovedAttachment::new(context, scope, target)
                    .map_err(|error| error.message().to_owned())?;
                self.install_host(ctx, attachment);
                return Ok(());
            }
            _ => {}
        }
        let permit =
            ConnectionPermit::reserve(&self.hosts.budgets).map_err(|error| error.to_string())?;
        let epoch = self.hosts.epoch;
        let token = self.sink.live.host.token(epoch);
        let cancel = token.clone();
        let selected = self.hosts.selected.clone();
        let root = profiles.host_approval_directory();
        let locator = self.hosts.locator.clone();
        let fingerprint = self.hosts.fingerprint.clone();
        let reviewed = if matches!(action, Action::Prepare) {
            if !self.hosts.trust {
                return Err(
                    "Independently verify and confirm the complete host-local prerequisite first."
                        .into(),
                );
            }
            let binding: OwnerBinding = serde_json::from_str(&self.hosts.prerequisite)
                .map_err(|_| "Invalid owner prerequisite; no vault action started.")?;
            let expected = selected
                .as_ref()
                .map(|recipe| &recipe.host.id)
                .unwrap_or(&binding.host.host);
            Some(
                ReviewedOwner::confirm(
                    &self.hosts.prerequisite,
                    expected,
                    &format!("github:{}", account.account_id),
                    &fingerprint,
                )
                .map_err(|error| error.message().to_owned())?,
            )
        } else {
            None
        };
        if matches!(action, Action::Resolve)
            && (!self.hosts.enrolled || self.hosts.receipt.is_none())
        {
            return Err(host_client::ENROLLMENT_PREREQUISITE.into());
        }
        let snapshot = account.clone();
        let snapshot_recipe = selected.clone();
        let job = Job::spawn(ctx, move |legacy_cancel| {
            let _permit = permit;
            if cancel.is_cancelled() || legacy_cancel.is_cancelled() {
                return Err("HOST action cancelled.".into());
            }
            let index =
                || ReceiptIndex::in_app_sandbox(&root).map_err(|error| error.message().to_owned());
            let result = match action {
                Action::Prepare => index()?
                    .prepare(reviewed.expect("reviewed"))
                    .map(|receipt| ResultEvent::Receipt(Box::new(receipt)))
                    .map_err(|error| error.message().into()),
                Action::Load => {
                    let host = selected
                        .as_ref()
                        .ok_or("Choose a saved host to load its protected receipt.")?;
                    index()?
                        .load(&host.host.id, &format!("github:{}", snapshot.account_id))
                        .map(|receipt| ResultEvent::Receipt(Box::new(receipt)))
                        .map_err(|error| error.message().into())
                }
                Action::Find | Action::Resolve => {
                    let (credential, secrets) = {
                        let store = store.lock().map_err(|_| "Account store unavailable.")?;
                        if store
                            .accounts()
                            .iter()
                            .find(|current| current.id == snapshot.id)
                            != Some(&snapshot)
                        {
                            return Err("Account credential changed.".into());
                        }
                        (snapshot.credential.to_reference(), store.secrets().clone())
                    };
                    let native = secrets
                        .get(&credential)
                        .map_err(|_| "Native account vault unavailable; no fallback.")?;
                    let client_id = identity::registered_client_id()
                        .ok_or("Foreground account sign-in is not configured.")?;
                    let identity = native
                        .with_bytes(|bytes| {
                            Identity::decode_private(bytes, snapshot.account_id, client_id)
                        })
                        .map_err(|_| "Foreground sign-in required in Profiles.")?;
                    if identity.needs_sign_in() {
                        return Err("Foreground sign-in required in Profiles; no refresh or sign-in was started.".into());
                    }
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "HOST directory worker unavailable.")?;
                    runtime.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Err("HOST directory action cancelled.".into()),
                            result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                                if matches!(action, Action::Find) {
                                    service::list_hosts(&identity, &cancel).await.map(|hosts| ResultEvent::Hosts(hosts.into_iter().take(32).collect()))
                                } else {
                                    service::resolve_host(&identity, &locator, &cancel).await.map(|resolved| ResultEvent::Resolved(Box::new(resolved)))
                                }
                            }) => result.map_err(|_| "HOST directory timed out; no absence inferred.".to_owned())?.map_err(|error| format!("{error}; no absence or creation authority inferred.")),
                        }
                    })
                }
                _ => unreachable!(),
            };
            if cancel.is_cancelled() || legacy_cancel.is_cancelled() {
                return Err("HOST action cancelled; local preparation may have completed. No result was installed.".into());
            }
            result
        });
        self.hosts.pending = Some(Pending {
            account,
            recipe: snapshot_recipe,
            epoch,
            token,
            job,
        });
        Ok(())
    }

    pub(super) fn show_hosts(&mut self, ctx: &egui::Context) {
        let accounts = self
            .profiles
            .as_ref()
            .map(Profiles::store_handle)
            .and_then(|store| store.lock().ok().map(|store| store.accounts().to_vec()))
            .unwrap_or_default();
        let recipes = self
            .profiles
            .as_ref()
            .map(Profiles::store_handle)
            .and_then(|store| {
                store
                    .lock()
                    .ok()
                    .map(|store| store.host_recipes().recipes().to_vec())
            })
            .unwrap_or_default();
        let old_account = self.hosts.account.clone();
        let old_locator = self.hosts.locator.clone();
        let old_fingerprint = self.hosts.fingerprint.clone();
        let old_prerequisite = self.hosts.prerequisite.clone();
        let old_consent = (self.hosts.trust, self.hosts.enrolled, self.hosts.launch);
        let mut action = None;
        let mut selection = None;
        egui::Window::new("Dev Tunnel hosts — foreground only").id(egui::Id::new("mobile-hosts"))
            .resizable(true).vscroll(true).max_height(ctx.content_rect().height().max(120.0)).show(ctx, |ui| {
            ui.label("Private by default. Opening/saving/importing does not connect, sign in, trust, enroll or create.");
            if ui.button("Review another host (local only)").clicked() { action = Some(Action::NewReview); }
            egui::ComboBox::from_id_salt("host-account").selected_text(&self.hosts.account).show_ui(ui, |ui| {
                for account in &accounts { ui.selectable_value(&mut self.hosts.account, account.id.clone(), format!("{} · github:{}", account.login, account.account_id)); }
            });
            if ui.button("Find hosts / refresh account").clicked() { action = Some(Action::Find); }
            ui.label("Service metadata only (up to 32 results); no online/session-absence claim.");
            for host in &self.hosts.hosts {
                if ui.button(format!("{} · {}", host.name.as_deref().unwrap_or("HOST"), host.locator)).clicked() { self.hosts.locator = host.locator.clone(); }
            }
            ui.separator();
            ui.label("Locally saved public recipes");
            for recipe in &recipes {
                if ui.button(format!("{} · {}", recipe.name, recipe.host.id)).clicked() { selection = Some(recipe.clone()); }
            }
            ui.collapsing("Reviewed public import (never execution approval)", |ui| {
                ui.add(egui::TextEdit::multiline(&mut self.hosts.import).char_limit(64 * 1024));
                ui.checkbox(&mut self.hosts.import_reviewed, "I reviewed the public host/account/launch metadata");
                if ui.button("Import metadata only").clicked() { action = Some(Action::Import); }
            });
            ui.label("Selected HOST locator"); ui.text_edit_singleline(&mut self.hosts.locator);
            ui.label("Independently confirmed SSH host fingerprint"); ui.text_edit_singleline(&mut self.hosts.fingerprint);
            ui.collapsing("Protected owner prerequisite / first execution", |ui| {
                ui.add(egui::TextEdit::multiline(&mut *self.hosts.prerequisite).char_limit(4096));
                ui.checkbox(&mut self.hosts.trust, "I independently verified this installation, host fingerprint, account and principal");
                if ui.button("Prepare native owner key (does NOT enroll on host)").clicked() { action = Some(Action::Prepare); }
                if ui.button("Load existing protected local receipt").clicked() { action = Some(Action::Load); }
            });
            if let Some(receipt) = &self.hosts.receipt {
                ui.label(format!("Host {} · principal {} · installation {}", receipt.binding().host.host, receipt.binding().principal, receipt.binding().installation));
                ui.label(receipt.public_key().iter().map(|byte| format!("{byte:02x}")).collect::<String>());
                if let Ok(fingerprint) = owner_key_fingerprint(&receipt.public_key()) { ui.label(fingerprint); }
                ui.label("Have this public key explicitly approved on the selected host. Imported metadata and this local key preparation do not enroll it.");
                if self.hosts.selected.is_none() {
                    if let Ok(mut recipe) = PublicRecipe::persistent("New host shell", HostReference { id: receipt.binding().host.host.clone(), account: AccountReference::Github { account_id: accounts.iter().find(|account| account.id == self.hosts.account).map_or(0, |account| account.account_id) } }, Recipe { shell: None, args: Vec::new(), cwd: None }) {
                        recipe.behavior = ConnectionBehavior::ChooseSession;
                        self.hosts.selected = Some(recipe);
                        self.hosts.saved = false;
                    }
                }
            }
            ui.add_enabled(self.hosts.receipt.is_some(), egui::Checkbox::new(&mut self.hosts.enrolled, "I confirm this exact host fingerprint and that the displayed owner public key was approved on this host"));
            if ui.button("Connect / refresh authorized sessions").clicked() { action = Some(Action::Resolve); }
            if let Some(inventory) = &self.hosts.inventory {
                ui.label(format!("Capabilities: attach={} create={} ensure={} · protected creation={} temporary={}", inventory.capabilities.attach, inventory.capabilities.create, inventory.capabilities.ensure, inventory.capabilities.creation_allowed, inventory.capabilities.temporary));
                if !inventory.unknown.is_empty() { ui.label("Some sessions are unknown, NOT absent."); }
                for (index, session) in inventory.sessions.iter().enumerate() {
                    let allowed = inventory.capabilities.attach && selected_target(self.hosts.selected.as_ref(), &session.target).is_ok();
                    if ui.add_enabled(allowed, egui::Button::new(format!("Attach {} · generation {}", session.target.logical, session.target.generation.get()))).clicked() { action = Some(Action::Attach(index)); }
                }
            }
            if let Some(recipe) = &self.hosts.selected {
                ui.label(format!("Logical session {}", recipe.logical_session));
                ui.label(format!("{} · {:?} · shell={:?} argv={:?} cwd={:?} · {:?}", recipe.name, recipe.behavior, recipe.launch.shell, recipe.launch.args, recipe.launch.cwd, recipe.lifetime));
                ui.checkbox(&mut self.hosts.launch, "Confirm this exact logical recipe, launch and lifetime");
                let allowed = self.hosts.inventory.as_ref().is_some_and(|inventory| match recipe.behavior {
                    ConnectionBehavior::ResumeOrCreate => inventory.capabilities.ensure && inventory.capabilities.attach,
                    ConnectionBehavior::AttachOnly { .. } => false,
                    _ => inventory.capabilities.create && inventory.capabilities.creation_allowed && inventory.capabilities.attach,
                });
                if ui.add_enabled(allowed && self.hosts.launch, egui::Button::new("Explicit Create / resume-or-create (or original-action retry)")).clicked() { action = Some(Action::Create); }
                if !self.hosts.saved && ui.button("Save public recipe only").clicked() { action = Some(Action::Save); }
            }
            for (index, original) in self.hosts.originals.iter().enumerate() {
                ui.label(format!("Retained original {} · logical {} · issued {:?}; in-process only, never automatic retry", original.recipe.name, original.intent.creation().logical, original.intent.issued_operation()));
                if ui.button(format!("Review retained original: {}", original.recipe.name)).clicked() { action = Some(Action::ReviewOriginal(index)); }
            }
            ui.label("After restart, reconcile uncertain actions on the host before approving another creation. No input replay or background reconnect.");
            if self.hosts.pending.is_some() || self.hosts.worker.is_some() { ui.label("Bounded foreground HOST work in progress…"); }
            if let Some(error) = &self.hosts.error { ui.label(error); }
            if ui.button("Cancel / close host review").clicked() { action = Some(Action::Close); }
        });
        if let Some(recipe) = selection {
            self.hosts.clear_approval();
            let AccountReference::Github { account_id } = recipe.host.account;
            self.hosts.account = accounts
                .iter()
                .find(|account| account.account_id == account_id)
                .map(|account| account.id.clone())
                .unwrap_or_default();
            self.hosts.selected = Some(recipe);
            self.hosts.saved = true;
            action = None;
        } else if old_account != self.hosts.account
            || old_locator != self.hosts.locator
            || old_fingerprint != self.hosts.fingerprint
            || old_prerequisite != self.hosts.prerequisite
            || (old_consent.0 && !self.hosts.trust)
            || (old_consent.1 && !self.hosts.enrolled)
            || ((self.hosts.pending.is_some() || self.hosts.worker.is_some())
                && old_consent != (self.hosts.trust, self.hosts.enrolled, self.hosts.launch))
        {
            self.hosts.clear_approval();
            if old_account != self.hosts.account {
                self.hosts.selected = None;
                self.hosts.saved = false;
                self.hosts.hosts.clear();
            }
            action = None;
        }
        if let Some(action) = action {
            self.hosts.error = None;
            if let Err(error) = self.host_action(ctx, action) {
                self.hosts.error = Some(error);
            }
        }
    }
}

#[cfg(test)]
#[path = "host_clients_tests.rs"]
mod tests;
