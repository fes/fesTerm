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
        AccountReference, ConnectionBehavior, HostHint, HostReference, MetadataCommand,
        PublicRecipe,
    },
    TunnelAccount,
};
use festerm_session::{Session, ShutdownError, ShutdownResult};
use festerm_tunnel_bridge::{
    host_client::{
        self,
        approvals::{Receipt, ReceiptIndex, ReviewedOwner},
        ApprovedAttachment, ApprovedContext, Budgets, CreationIntent, CreationMode, EndIntent,
        HostSession, Inventory, Outcome, Scope, Worker,
    },
    owner_auth::{owner_key_fingerprint, OwnerBinding},
};
use festerm_tunnel_client::{
    host_control::{Binding, ConnectionPermit},
    identity::{self, Identity},
    service::{self, ResolvedTunnel, TunnelSummary},
};
use host_client::originals::{
    ForgetOutcome, ForgottenOriginal, OriginalId, OriginalStore, RecoveredIntent,
    RecoveredOriginal, ReviewedForget,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[cfg(all(test, unix))]
#[derive(Default)]
struct GalleryDisplay {
    originals: bool,
    forget_end: Option<bool>,
    end: bool,
    busy: bool,
}

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
        let mut current = self.epoch.load(Ordering::Acquire);
        while let Some(next) = current.checked_add(1) {
            match self.epoch.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
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
    Originals(Arc<OriginalStore>, Vec<RecoveredOriginal>),
    ForgetReview(Box<ReviewedForget>, u64),
    Forgotten(ForgottenOriginal, u64),
}

struct OriginalReview {
    account: TunnelAccount,
    recipe: Option<PublicRecipe>,
}

impl OriginalReview {
    fn encode(&self) -> Result<String, String> {
        serde_json::to_string(&serde_json::json!({
            "account": &self.account, "recipe": &self.recipe,
        }))
        .map_err(|_| "Original review could not be protected.".into())
    }

    fn decode(value: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(value).map_err(|_| "Protected original review is malformed.")?;
        let object = value
            .as_object()
            .ok_or("Protected original review is malformed.")?;
        if object.len() != 2 || !object.contains_key("account") || !object.contains_key("recipe") {
            return Err("Protected original review is malformed.".into());
        }
        Ok(Self {
            account: serde_json::from_value(object["account"].clone())
                .map_err(|_| "Protected original account is malformed.")?,
            recipe: serde_json::from_value(object["recipe"].clone())
                .map_err(|_| "Protected original recipe is malformed.")?,
        })
    }
}

struct Pending {
    account: TunnelAccount,
    recipe: Option<PublicRecipe>,
    epoch: u64,
    token: Arc<CancellationToken>,
    job: Job<Result<ResultEvent, String>>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SavedConnect {
    Receipt,
    Resolve,
    Inventory,
    Originals,
}

struct QuickCheck {
    recipe: PublicRecipe,
    account: TunnelAccount,
    epoch: u64,
    generation: u64,
    owner: OwnerBinding,
    reference: festerm_secret_store::SecretReference,
}

struct Original {
    recipe: PublicRecipe,
    account: TunnelAccount,
    intent: Arc<CreationIntent>,
}

struct SessionAccount {
    session: Weak<HostSession>,
    account: TunnelAccount,
    tab: u64,
    owner: Option<OwnerBinding>,
}

struct OriginalEnd {
    host: festerm_host_protocol::HostId,
    account: TunnelAccount,
    intent: Arc<EndIntent>,
}

struct EndReview {
    account: TunnelAccount,
    intent: Arc<EndIntent>,
    epoch: u64,
    generation: u64,
    confirmed: bool,
    source: Option<(u64, festerm_session::SessionId)>,
}

impl EndReview {
    fn authorize(
        &self,
        account: &TunnelAccount,
        context: &ApprovedContext,
        epoch: u64,
        generation: u64,
    ) -> Result<Arc<EndIntent>, String> {
        if !self.confirmed
            || &self.account != account
            || self.epoch != epoch
            || self.generation != generation
            || !self.intent.matches_context(context)
        {
            return Err("End confirmation expired or is incomplete; no operation issued.".into());
        }
        Ok(self.intent.clone())
    }
}

struct Retirement {
    tab: u64,
    job: Job<Result<ShutdownResult, ShutdownError>>,
}

struct ForgetReview {
    record: ReviewedForget,
    account: TunnelAccount,
    epoch: u64,
    generation: u64,
    confirmed: bool,
}

impl ForgetReview {
    fn authorize(
        &self,
        account: &TunnelAccount,
        context: &ApprovedContext,
        epoch: u64,
        generation: u64,
    ) -> Result<(), String> {
        if !self.confirmed
            || &self.account != account
            || self.epoch != epoch
            || self.generation != generation
            || !self.record.matches_context(context)
        {
            return Err(
                "Local Forget confirmation expired or is incomplete; no deletion confirmed.".into(),
            );
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct HostUi {
    #[cfg(all(test, unix))]
    gallery_display: Option<GalleryDisplay>,
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
    session_accounts: Vec<SessionAccount>,
    originals: Vec<Original>,
    original_store: Option<Arc<OriginalStore>>,
    selected_original: Option<String>,
    ends: Vec<OriginalEnd>,
    end_review: Option<EndReview>,
    forget_review: Option<ForgetReview>,
    retirements: Vec<Retirement>,
    budgets: Budgets,
    epoch: u64,
    generation: u64,
    error: Option<String>,
    discovered: bool,
    quick_new: bool,
    quick_name: String,
    editing: bool,
    profile_name: String,
    session_name: String,
    create_if_missing: bool,
    existing_session: Option<usize>,
    host_collapsed: bool,
    saved_connect: Option<SavedConnect>,
    connection_notice: Option<(host_client::EnsureDisposition, std::time::Instant)>,
    quick_check: Option<QuickCheck>,
}

impl HostUi {
    fn finish_quick_check(&mut self, account: &TunnelAccount) -> Result<Action, String> {
        let review = self
            .quick_check
            .take()
            .ok_or("Create review is no longer current.")?;
        let receipt = self
            .receipt
            .as_ref()
            .ok_or("Protected owner approval changed.")?;
        if !self.quick_new
            || self.selected.as_ref() != Some(&review.recipe)
            || account != &review.account
            || self.epoch != review.epoch
            || self.generation != review.generation
            || receipt.binding() != &review.owner
            || receipt.reference() != review.reference
            || self.quick_name.trim() != review.recipe.session_name()
            || !self.quick_creation_allowed()
            || self.original_store.is_none()
        {
            return Err("Create review changed. Review the session and try again.".into());
        }
        self.context()?;
        if self.originals.iter().any(|original| {
            original.recipe.id == review.recipe.id && !original.intent.is_presented()
        }) {
            return Err(
                "An original action is retained. Review it in Details & recovery before retrying."
                    .into(),
            );
        }
        self.launch = true;
        Ok(Action::Create)
    }

    fn saved_receipt_loaded(&mut self) -> Result<Option<Action>, String> {
        if self.saved_connect != Some(SavedConnect::Receipt) {
            return Ok(None);
        }
        let recipe = self
            .selected
            .as_ref()
            .ok_or("Saved profile selection changed.")?;
        let account = self
            .approval_account
            .as_ref()
            .ok_or("Original account no longer current.")?;
        let receipt = self
            .receipt
            .as_ref()
            .ok_or(host_client::ENROLLMENT_PREREQUISITE)?;
        if self.locator.trim().is_empty() {
            return Err("Choose this profile's host endpoint before connecting.".into());
        }
        if receipt.binding().host.host != recipe.host.id
            || receipt.binding().account != format!("github:{}", account.account_id)
            || recipe.host.account
                != (AccountReference::Github {
                    account_id: account.account_id,
                })
            || self.fingerprint != receipt.binding().host.host_key_fingerprint
        {
            return Err(
                "The protected host or account approval does not match this profile.".into(),
            );
        }
        self.saved_connect = Some(SavedConnect::Resolve);
        Ok(Some(Action::Resolve))
    }

    fn busy(&self) -> bool {
        #[cfg(all(test, unix))]
        if self
            .gallery_display
            .as_ref()
            .is_some_and(|display| display.busy)
        {
            return true;
        }
        self.pending.is_some() || self.worker.is_some()
    }

    fn quick_creation_allowed(&self) -> bool {
        self.inventory.as_ref().is_some_and(|inventory| {
            inventory.capabilities.create
                && inventory.capabilities.creation_allowed
                && inventory.capabilities.attach
        })
    }

    fn valid_quick_name(&self) -> bool {
        let name = self.quick_name.trim();
        !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
    }

    fn begin_profile_edit(&mut self) {
        if let Some(recipe) = &self.selected {
            self.profile_name = recipe.name.clone();
            self.session_name = recipe.session_name().to_owned();
            self.create_if_missing = matches!(recipe.behavior, ConnectionBehavior::ResumeOrCreate);
            self.existing_session = self.inventory.as_ref().and_then(|inventory| {
                inventory.sessions.iter().position(|session| {
                    session.target.logical == recipe.logical_session
                        && match recipe.behavior {
                            ConnectionBehavior::AttachOnly { generation } => {
                                session.target.generation == generation
                            }
                            _ => true,
                        }
                })
            });
        }
        self.editing = true;
        self.quick_new = false;
    }

    fn profile_draft(&self) -> Result<PublicRecipe, String> {
        let mut recipe = self.selected.clone().ok_or("Choose a host first.")?;
        recipe.name = self.profile_name.trim().to_owned();
        let existing_session = self
            .existing_session
            .and_then(|index| self.inventory.as_ref()?.sessions.get(index));
        let preserve_logical_resume = existing_session.is_some_and(|session| {
            session.target.logical == recipe.logical_session
                && matches!(
                    recipe.behavior,
                    ConnectionBehavior::ResumeOnly {} | ConnectionBehavior::ResumeOrCreate
                )
        });
        if let Some(session) = existing_session {
            recipe.logical_session = session.target.logical.clone();
        }
        if self.create_if_missing {
            recipe.session_name = Some(self.session_name.trim().to_owned());
            recipe.behavior = ConnectionBehavior::ResumeOrCreate;
            recipe.lifetime = festerm_host_protocol::Lifetime::Persistent {};
        } else if preserve_logical_resume {
            recipe.behavior = ConnectionBehavior::ResumeOnly {};
        } else if let Some(session) = existing_session {
            recipe.logical_session = session.target.logical.clone();
            recipe.behavior = ConnectionBehavior::AttachOnly {
                generation: session.target.generation,
            };
            recipe.session_name = Some(session.name.clone());
            recipe.lifetime = session.lifetime;
        } else if !matches!(recipe.behavior, ConnectionBehavior::AttachOnly { .. }) {
            if matches!(recipe.behavior, ConnectionBehavior::ChooseSession) {
                return Err(
                    "Choose an existing session, or enable Create session if missing.".into(),
                );
            }
            recipe.behavior = ConnectionBehavior::ResumeOnly {};
        }
        if !self.locator.trim().is_empty() {
            recipe.host_hint = Some(HostHint {
                locator: self.locator.trim().to_owned(),
                name: self
                    .hosts
                    .iter()
                    .find(|host| host.locator == self.locator)
                    .and_then(|host| host.name.clone())
                    .unwrap_or_else(|| self.locator.clone()),
            });
        }
        recipe.validate().map_err(|_| {
            "Enter a profile name and a valid session name (up to 128 bytes).".to_owned()
        })?;
        Ok(recipe)
    }

    fn freeze_quick_recipe(&mut self, account: &TunnelAccount) -> Result<(), String> {
        if !self.quick_new || !self.quick_creation_allowed() {
            return Err("This host does not currently permit new sessions.".into());
        }
        let name = self.quick_name.trim();
        if !self.valid_quick_name() {
            return Err("Enter a session name (up to 128 bytes).".into());
        }
        let current = self.selected.as_ref().ok_or("Choose a host first.")?;
        let frozen = self
            .originals
            .iter()
            .any(|original| original.recipe.id == current.id);
        if frozen {
            if current.session_name() != name
                || current.behavior != ConnectionBehavior::AlwaysCreate
            {
                return Err(
                    "Retry the original session name; its result is not yet reconciled.".into(),
                );
            }
        } else {
            let mut recipe = current.clone();
            recipe.name = name.to_owned();
            recipe.session_name = Some(name.to_owned());
            recipe.behavior = ConnectionBehavior::AlwaysCreate;
            recipe.lifetime = festerm_host_protocol::Lifetime::Persistent {};
            recipe
                .validate()
                .map_err(|_| "Enter a valid session name.")?;
            if recipe.host.account
                != (AccountReference::Github {
                    account_id: account.account_id,
                })
            {
                return Err("The selected host belongs to another account.".into());
            }
            self.selected = Some(recipe);
            self.saved = false;
        }
        self.launch = true;
        Ok(())
    }

    fn has_forget_selection(
        &self,
        id: &OriginalId,
        account: &TunnelAccount,
        context: &ApprovedContext,
    ) -> bool {
        let same_account = |original: &TunnelAccount| {
            original.provider == account.provider && original.account_id == account.account_id
        };
        match id {
            OriginalId::Creation(id) => self.originals.iter().any(|original| {
                &original.intent.creation().attachment == id
                    && same_account(&original.account)
                    && original.intent.matches_context(context)
            }),
            OriginalId::End(id) => self.ends.iter().any(|original| {
                original.intent.operation() == id
                    && same_account(&original.account)
                    && original.intent.matches_context(context)
            }),
        }
    }

    fn accept_forget_review(
        &mut self,
        record: ReviewedForget,
        account: &TunnelAccount,
        generation: u64,
    ) -> Result<(), String> {
        let context = self.context()?;
        if self.pending.is_some()
            || self.worker.is_some()
            || generation != self.generation
            || !record.matches_context(&context)
            || !self.has_forget_selection(record.id(), account, &context)
        {
            return Err("Local Forget review is no longer current; no deletion confirmed.".into());
        }
        self.launch = false;
        self.end_review = None;
        self.forget_review = Some(ForgetReview {
            record,
            account: account.clone(),
            epoch: self.epoch,
            generation,
            confirmed: false,
        });
        Ok(())
    }

    fn accept_forgotten(
        &mut self,
        forgotten: ForgottenOriginal,
        account: &TunnelAccount,
        generation: u64,
    ) -> Result<(), String> {
        let context = self.context()?;
        if generation != self.generation
            || !forgotten.matches_context(&context)
            || !self.has_forget_selection(forgotten.id(), account, &context)
        {
            return Err("Local Forget completion is no longer current; no remote absence inferred. Explicitly reopen protected history.".into());
        }
        match forgotten.id() {
            OriginalId::Creation(id) => {
                self.originals
                    .retain(|original| &original.intent.creation().attachment != id);
                if self.selected_original.as_ref() == Some(&id.to_string()) {
                    self.selected_original = None;
                }
            }
            OriginalId::End(id) => self
                .ends
                .retain(|original| original.intent.operation() != id),
        }
        self.launch = false;
        self.end_review = None;
        self.forget_review = None;
        self.error = Some(match forgotten.outcome() {
            ForgetOutcome::Removed => "Selected local retry checkpoint forgotten. Remote session/action unchanged; no End, absence, replacement or retry.".into(),
            ForgetOutcome::AlreadyMissing => "Selected local retry history abandoned; no native checkpoint remained in the validated payload. This says nothing about the remote action.".into(),
        });
        Ok(())
    }

    fn accept_recovered(
        &mut self,
        store: Arc<OriginalStore>,
        recovered: Vec<RecoveredOriginal>,
        account: &TunnelAccount,
    ) -> Result<(), String> {
        self.prune_confirmed_ends();
        let context = self.context()?;
        let mut creations = Vec::new();
        let mut ends = Vec::new();
        for original in recovered {
            let review = OriginalReview::decode(&original.review)?;
            if review.account.provider != account.provider
                || review.account.account_id != account.account_id
            {
                return Err(
                    "The original authenticated account differs; original retained, no replacement."
                        .into(),
                );
            }
            match original.intent {
                RecoveredIntent::Creation(intent) => {
                    let recipe = review
                        .recipe
                        .ok_or("Original creation recipe is missing.")?;
                    let creation = intent.creation();
                    if !intent.is_protected_original()
                        || !intent.matches_context(&context)
                        || &recipe.host.id != context.binding().host()
                        || recipe.host.account
                            != (AccountReference::Github {
                                account_id: account.account_id,
                            })
                        || creation.recipe != recipe.launch
                        || creation.lifetime != recipe.lifetime
                        || creation.name.as_deref() != Some(recipe.session_name())
                        || (intent.mode() == CreationMode::Ensure
                            && (recipe.behavior != ConnectionBehavior::ResumeOrCreate
                                || creation.logical != recipe.logical_session))
                        || (intent.mode() == CreationMode::Create
                            && matches!(
                                recipe.behavior,
                                ConnectionBehavior::ResumeOrCreate
                                    | ConnectionBehavior::ResumeOnly {}
                                    | ConnectionBehavior::AttachOnly { .. }
                            ))
                    {
                        return Err(
                            "Protected original recipe/context changed; no action was started."
                                .into(),
                        );
                    }
                    creations.push(Original {
                        recipe,
                        account: account.clone(),
                        intent,
                    });
                }
                RecoveredIntent::End(intent) => {
                    if review.recipe.is_some() || !intent.matches_context(&context) {
                        return Err("Protected original End context changed.".into());
                    }
                    ends.push(OriginalEnd {
                        host: context.binding().host().clone(),
                        account: account.clone(),
                        intent,
                    });
                }
            }
        }
        for restored in &creations {
            if let Some(old) = self.originals.iter().find(|old| {
                old.intent.creation().attachment == restored.intent.creation().attachment
            }) {
                if old.account != restored.account
                    || old.recipe != restored.recipe
                    || old.intent.creation() != restored.intent.creation()
                    || old.intent.mode() != restored.intent.mode()
                    || old.intent.issued_operation() != restored.intent.issued_operation()
                    || !old.intent.matches_context(&context)
                {
                    return Err("Original identity collision; no original was replaced.".into());
                }
            }
        }
        for restored in &ends {
            if let Some(old) = self
                .ends
                .iter()
                .find(|old| old.intent.operation() == restored.intent.operation())
            {
                if old.account != restored.account
                    || old.intent.target() != restored.intent.target()
                    || !old.intent.matches_context(&context)
                {
                    return Err("Original End identity collision; no original was replaced.".into());
                }
            }
        }
        let new_creations: Vec<_> = creations
            .into_iter()
            .filter(|restored| {
                !self.originals.iter().any(|old| {
                    old.intent.creation().attachment == restored.intent.creation().attachment
                })
            })
            .collect();
        let new_ends: Vec<_> = ends
            .into_iter()
            .filter(|restored| {
                !self
                    .ends
                    .iter()
                    .any(|old| old.intent.operation() == restored.intent.operation())
            })
            .collect();
        if self.originals.len() + new_creations.len() > 16 || self.ends.len() + new_ends.len() > 16
        {
            return Err("Original registry full; every unresolved action remains retained.".into());
        }
        self.originals.extend(new_creations);
        self.ends.extend(new_ends);
        self.original_store = Some(store);
        self.launch = false;
        self.end_review = None;
        Ok(())
    }

    fn prune_confirmed_ends(&mut self) {
        self.originals.retain(|original| {
            !self
                .ends
                .iter()
                .any(|end| end.intent.definitively_settles(&original.intent))
        });
        self.ends.retain(|original| !original.intent.is_resolved());
    }

    fn retain_end(
        &mut self,
        context: &ApprovedContext,
        account: TunnelAccount,
        intent: Arc<EndIntent>,
    ) -> Result<(), String> {
        self.prune_confirmed_ends();
        if intent.is_resolved() {
            return Err(
                "This exact End is already confirmed; refresh authorized inventory.".into(),
            );
        }
        if !intent.matches_context(context) {
            return Err("The original protected End context changed.".into());
        }
        if self
            .ends
            .iter()
            .any(|original| Arc::ptr_eq(&original.intent, &intent))
        {
            return Ok(());
        }
        if self.ends.len() >= 16 {
            return Err("Original End registry full; uncertain actions retained.".into());
        }
        self.ends.push(OriginalEnd {
            host: context.binding().host().clone(),
            account,
            intent,
        });
        Ok(())
    }

    fn end_intent(
        &mut self,
        context: &ApprovedContext,
        target: host_client::Target,
        account: &TunnelAccount,
    ) -> Result<Arc<EndIntent>, String> {
        self.prune_confirmed_ends();
        if let Some(original) = self.ends.iter().find(|original| {
            &original.host == context.binding().host() && original.intent.target() == &target
        }) {
            if &original.account != account || !original.intent.matches_context(context) {
                return Err("Original End retained under its original protected host/account; no replacement operation issued.".into());
            }
            return Ok(original.intent.clone());
        }
        Ok(Arc::new(EndIntent::new(context, target)))
    }

    fn cancel(&mut self) {
        self.quick_check = None;
        self.saved_connect = None;
        self.forget_review = None;
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
        self.end_review = None;
        self.original_store = None;
    }

    fn clear_review(&mut self) {
        self.clear_approval();
        self.selected = None;
        self.saved = false;
        self.locator.clear();
        self.fingerprint.clear();
        self.prerequisite = Zeroizing::new(String::new());
        self.selected_original = None;
        self.quick_new = false;
        self.quick_name.clear();
        self.editing = false;
        self.existing_session = None;
        self.host_collapsed = false;
    }

    fn context(&self) -> Result<ApprovedContext, String> {
        if !self.enrolled && self.saved_connect.is_none() {
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
            ConnectionBehavior::AttachOnly { .. } | ConnectionBehavior::ResumeOnly {} => {
                return Err("Attach-only cannot create.".into());
            }
        };
        self.originals.retain(|original| {
            !original.intent.is_presented() || original.intent.is_protected_original()
        });
        if self.selected_original.is_none()
            && self
                .originals
                .iter()
                .filter(|original| {
                    original.recipe.id == recipe.id && !original.intent.is_presented()
                })
                .count()
                > 1
        {
            return Err("Multiple originals exist for this recipe; explicitly review the exact original first.".into());
        }
        if let Some(original) = self.originals.iter().find(|original| {
            original.recipe.id == recipe.id
                && self.selected_original.as_ref().map_or_else(
                    || !original.intent.is_presented(),
                    |selected| &original.intent.creation().attachment.to_string() == selected,
                )
        }) {
            if original.recipe != recipe || original.account != account {
                return Err("Original action retained under its original recipe/account; no replacement was issued.".into());
            }
            return Ok(original.intent.clone());
        }
        if self.selected_original.is_some() {
            return Err(
                "The selected original no longer matches this action; no new action was issued."
                    .into(),
            );
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
            name: Some(recipe.session_name().to_owned()),
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
    RecoverOriginals,
    Resolve,
    Create,
    Attach(usize),
    ReviewEnd(usize),
    ReviewOriginalEnd(usize),
    ConfirmEnd,
    CancelEnd,
    ReviewForgetCreation(usize),
    ReviewForgetEnd(usize),
    ConfirmForget,
    CancelForget,
    #[cfg(test)]
    Save,
    Import,
    Close,
    FindAccount(usize),
    SelectHost(usize),
    Settings,
    QuickNew,
    QuickCreate,
    EditProfile,
    SaveProfile,
    CancelEdit,
    NewProfile,
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
        Some((ConnectionBehavior::ResumeOnly {}, logical)) if logical != &target.logical => {
            Err("Resume must keep this profile's saved logical session.".into())
        }
        Some((ConnectionBehavior::AttachOnly { generation }, logical))
            if logical != &target.logical || generation != &target.generation =>
        {
            Err("Exact logical session/generation required; no retargeting.".into())
        }
        _ => Ok(()),
    }
}

fn requires_launch_review(recipe: &PublicRecipe) -> bool {
    recipe.launch.shell.is_some()
        || !recipe.launch.args.is_empty()
        || recipe.launch.cwd.is_some()
        || !matches!(
            recipe.lifetime,
            festerm_host_protocol::Lifetime::Persistent {}
        )
}

impl MobileApp {
    pub(super) fn connect_saved_host_profile(&mut self, ctx: &egui::Context, id: &str) -> bool {
        let recipe = self.profiles.as_ref().and_then(|profiles| {
            profiles
                .store_handle()
                .lock()
                .ok()?
                .host_recipes()
                .recipes()
                .iter()
                .find(|recipe| recipe.id.as_str() == id)
                .cloned()
        });
        let Some(recipe) = recipe else {
            return false;
        };
        if self.hosts.busy() {
            self.notice = Some("Finish or cancel the current host action first.".into());
            return true;
        }
        self.open_hosts();
        self.select_host_recipe(recipe);
        self.hosts.connection_notice = None;
        self.hosts.saved_connect = Some(SavedConnect::Receipt);
        self.hosts.launch = !self
            .hosts
            .selected
            .as_ref()
            .is_some_and(requires_launch_review);
        if let Err(error) = self.host_action(ctx, Action::Load) {
            self.hosts.saved_connect = None;
            self.hosts.error = Some(error);
        }
        true
    }

    fn continue_saved_inventory(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let recipe = self
            .hosts
            .selected
            .as_ref()
            .ok_or("Saved profile selection changed.")?;
        let inventory = self
            .hosts
            .inventory
            .as_ref()
            .ok_or("Authorized sessions unavailable.")?;
        match recipe.behavior {
            ConnectionBehavior::AttachOnly { .. } | ConnectionBehavior::ResumeOnly {} => {
                let index = inventory
                    .sessions
                    .iter()
                    .position(|session| selected_target(Some(recipe), &session.target).is_ok())
                    .ok_or_else(|| {
                        if inventory
                            .unknown
                            .iter()
                            .any(|target| target.logical == recipe.logical_session)
                        {
                            "Session status is unknown. Refresh to try again."
                        } else {
                            "Session not found or not authorized."
                        }
                    })?;
                self.hosts.saved_connect = None;
                self.host_action(ctx, Action::Attach(index))
            }
            ConnectionBehavior::ResumeOrCreate | ConnectionBehavior::AlwaysCreate => {
                self.hosts.saved_connect = Some(SavedConnect::Originals);
                self.host_action(ctx, Action::RecoverOriginals)
            }
            ConnectionBehavior::ChooseSession => {
                self.hosts.saved_connect = None;
                Ok(())
            }
        }
    }

    fn continue_saved_originals(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.hosts.saved_connect = None;
        let recipe = self
            .hosts
            .selected
            .as_ref()
            .ok_or("Saved profile selection changed.")?;
        if self
            .hosts
            .originals
            .iter()
            .any(|original| original.recipe.id == recipe.id && !original.intent.is_presented())
        {
            return Err(
                "An original action is retained. Review it in Details & recovery before retrying."
                    .into(),
            );
        }
        if requires_launch_review(recipe) {
            self.hosts.launch = false;
            return Ok(());
        }
        self.hosts.launch = true;
        self.host_action(ctx, Action::Create)
    }

    fn review_end(&mut self, account: TunnelAccount, intent: Arc<EndIntent>) {
        self.hosts.end_review = Some(EndReview {
            account,
            intent,
            epoch: self.hosts.epoch,
            generation: self.hosts.generation,
            confirmed: false,
            source: None,
        });
        self.hosts.open = true;
    }

    pub(super) fn review_host_end(&mut self, tab: u64) -> Result<(), String> {
        self.hosts.open = true;
        if !self.host_foreground()
            || !self
                .tabs
                .iter()
                .any(|current| current.id == tab && current.host)
        {
            return Err("The original HOST tab is no longer current or foreground.".into());
        }
        let owner = self
            .hosts
            .session_accounts
            .iter()
            .find(|owner| owner.tab == tab)
            .ok_or("Refresh the original host's authorized inventory to review End.")?;
        let session = owner
            .session
            .upgrade()
            .ok_or("The attachment retired; refresh authorized inventory before End.")?;
        let context = self.hosts.context()?;
        if self.host_account().as_ref() != Some(&owner.account)
            || owner.owner.as_ref() != self.hosts.receipt.as_ref().map(Receipt::binding)
            || context.binding() != session.binding()
        {
            return Err("Review the original protected host, principal and account before End; no tab metadata grants authority.".into());
        }
        let target = session
            .target()
            .ok_or("The exact adopted generation is unavailable; refresh inventory.")?
            .clone();
        let account = owner.account.clone();
        let intent = self.hosts.end_intent(&context, target, &account)?;
        self.review_end(account, intent);
        self.hosts.end_review.as_mut().expect("new review").source = Some((tab, session.id()));
        Ok(())
    }

    pub(super) fn retire_host_tab(&mut self, tab: u64) -> bool {
        if !self
            .tabs
            .iter()
            .any(|current| current.id == tab && current.host)
        {
            return true;
        }
        if self.hosts.retirements.len() >= 16 {
            self.notice = Some(
                "HOST detach workers are busy; wait before closing another attachment.".into(),
            );
            return false;
        }
        let retiring = if self.active_tab == Some(tab) {
            self.sink
                .live
                .connection
                .borrow_mut()
                .as_mut()
                .and_then(live_ssh::LiveSsh::take_host_for_retirement)
        } else {
            self.sink
                .live
                .background
                .borrow_mut()
                .iter_mut()
                .find(|(id, _)| *id == tab)
                .and_then(|(_, live)| live.take_host_for_retirement())
        };
        if let Some((session, ctx)) = retiring {
            let job = Job::spawn(&ctx, move |_| {
                let result = session.shutdown(std::time::Duration::from_secs(2));
                if result.is_err() {
                    session.cancel();
                }
                result
            });
            self.hosts.retirements.push(Retirement { tab, job });
        }
        true
    }

    pub(super) fn open_hosts(&mut self) {
        self.hosts.clear_approval();
        self.hosts.epoch = self.sink.live.host.epoch();
        self.hosts.open = true;
    }

    pub(super) fn select_host_profile(&mut self, id: &str, edit: bool) -> bool {
        let Some(recipe) = self.profiles.as_ref().and_then(|profiles| {
            profiles
                .store_handle()
                .lock()
                .ok()?
                .host_recipes()
                .recipes()
                .iter()
                .find(|recipe| recipe.id.as_str() == id)
                .cloned()
        }) else {
            return false;
        };
        self.open_hosts();
        self.select_host_recipe(recipe);
        if edit {
            self.hosts.begin_profile_edit();
        }
        true
    }

    fn select_host_recipe(&mut self, recipe: PublicRecipe) {
        self.hosts.clear_review();
        let AccountReference::Github { account_id } = recipe.host.account;
        let account = self
            .profiles
            .as_ref()
            .and_then(|profiles| {
                profiles
                    .store_handle()
                    .lock()
                    .ok()?
                    .accounts()
                    .iter()
                    .find(|account| account.account_id == account_id)
                    .map(|account| account.id.clone())
            })
            .unwrap_or_default();
        if self.hosts.account != account {
            self.hosts.hosts.clear();
            self.hosts.discovered = false;
        }
        self.hosts.account = account;
        self.hosts.locator = recipe
            .host_hint
            .as_ref()
            .map(|hint| hint.locator.clone())
            .unwrap_or_default();
        self.hosts.selected = Some(recipe);
        self.hosts.saved = true;
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
        self.hosts.retirements.retain(|retirement| {
            match retirement.job.poll() {
                Poll::Pending => true,
                Poll::Done(result) => {
                    self.notice = Some(match result {
                        Ok(ShutdownResult::Stopped) => format!(
                            "HOST tab {}: correlated attachment detach confirmed; no End was issued.",
                            retirement.tab
                        ),
                        Ok(ShutdownResult::AlreadyStopped) => format!(
                            "HOST tab {}: attachment already stopped; no detach receipt or End inferred.",
                            retirement.tab
                        ),
                        Err(_) => format!(
                            "HOST tab {}: detach unconfirmed; daemon owns transport-loss grace. No End or retry.",
                            retirement.tab
                        ),
                    });
                    false
                }
                Poll::Lost => {
                    self.notice = Some("HOST retirement worker lost; no detach or End success inferred.".into());
                    false
                }
            }
        });
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
            .retain(|owner| owner.session.strong_count() > 0);
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
            .any(|owner| !current_accounts.contains(&owner.account))
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
                Ok(ResultEvent::Hosts(hosts)) => {
                    self.hosts.hosts = hosts;
                    self.hosts.discovered = true;
                }
                Ok(ResultEvent::Receipt(receipt)) => {
                    self.hosts.enrolled = false;
                    if self.hosts.saved_connect.is_none() {
                        self.hosts.launch = false;
                    }
                    if self.hosts.fingerprint.is_empty() {
                        self.hosts.fingerprint =
                            receipt.binding().host.host_key_fingerprint.clone();
                    }
                    self.hosts.receipt = Some(*receipt);
                    self.hosts.approval_account = account.clone();
                    if self.hosts.selected.is_none() {
                        if let Some(account) = &account {
                            let receipt = self.hosts.receipt.as_ref().expect("installed receipt");
                            if let Ok(mut recipe) = PublicRecipe::persistent(
                                "Host session",
                                HostReference {
                                    id: receipt.binding().host.host.clone(),
                                    account: AccountReference::Github {
                                        account_id: account.account_id,
                                    },
                                },
                                Recipe {
                                    shell: None,
                                    args: Vec::new(),
                                    cwd: None,
                                },
                            ) {
                                recipe.behavior = ConnectionBehavior::ChooseSession;
                                self.hosts.selected = Some(recipe);
                                self.hosts.saved = false;
                            }
                        }
                    }
                    match self.hosts.saved_receipt_loaded() {
                        Ok(Some(action)) => {
                            if let Err(error) = self.host_action(ctx, action) {
                                self.hosts.saved_connect = None;
                                self.hosts.error = Some(error);
                            }
                        }
                        Err(error) => {
                            self.hosts.saved_connect = None;
                            self.hosts.error = Some(error);
                        }
                        Ok(None) => {}
                    }
                }
                Ok(ResultEvent::Resolved(resolved)) => {
                    self.hosts.resolved = Some(*resolved);
                    if self.hosts.saved_connect == Some(SavedConnect::Resolve) {
                        self.hosts.saved_connect = Some(SavedConnect::Inventory);
                    }
                    if let Err(error) =
                        self.start_host_operation(ctx, host_client::Operation::Inventory)
                    {
                        self.hosts.saved_connect = None;
                        self.hosts.error = Some(error);
                    }
                }
                Ok(ResultEvent::Originals(store, originals)) => match account.as_ref() {
                    Some(account) => {
                        if let Err(error) = self.hosts.accept_recovered(store, originals, account) {
                            self.hosts.saved_connect = None;
                            self.hosts.quick_check = None;
                            self.hosts.error = Some(error);
                        } else if self.hosts.saved_connect == Some(SavedConnect::Originals) {
                            if let Err(error) = self.continue_saved_originals(ctx) {
                                self.hosts.error = Some(error);
                            }
                        } else if self.hosts.quick_check.is_some() {
                            let next = self.hosts.finish_quick_check(account);
                            match next.and_then(|action| self.host_action(ctx, action)) {
                                Ok(()) => {}
                                Err(error) => self.hosts.error = Some(error),
                            }
                        }
                    }
                    None => self.hosts.error = Some("Original account no longer current.".into()),
                },
                Ok(ResultEvent::ForgetReview(record, generation)) => {
                    let result = account
                        .as_ref()
                        .ok_or("Original account no longer current.".into())
                        .and_then(|account| {
                            self.hosts
                                .accept_forget_review(*record, account, generation)
                        });
                    if let Err(error) = result {
                        self.hosts.error = Some(error);
                    }
                }
                Ok(ResultEvent::Forgotten(forgotten, generation)) => {
                    let result = account
                        .as_ref()
                        .ok_or("Original account no longer current.".into())
                        .and_then(|account| {
                            self.hosts.accept_forgotten(forgotten, account, generation)
                        });
                    if let Err(error) = result {
                        self.hosts.error = Some(error);
                    }
                }
                Err(error) => {
                    self.hosts.saved_connect = None;
                    self.hosts.quick_check = None;
                    self.hosts.error = Some(error);
                }
            }
        }
        let result = self
            .hosts
            .worker
            .as_ref()
            .and_then(|worker| worker.try_receive(worker.scope()));
        if let Some(result) = result {
            self.hosts.prune_confirmed_ends();
            self.hosts.worker = None;
            self.hosts.worker_account = None;
            match result.outcome {
                Outcome::Inventory(inventory) => {
                    self.hosts.inventory = Some(inventory);
                    if self.hosts.saved_connect == Some(SavedConnect::Inventory) {
                        self.hosts.enrolled = true;
                        if let Err(error) = self.continue_saved_inventory(ctx) {
                            self.hosts.saved_connect = None;
                            self.hosts.error = Some(error);
                        }
                    }
                }
                Outcome::Created(created) => {
                    let disposition = created.disposition();
                    let attachment = self.hosts.context().and_then(|context| {
                        ApprovedAttachment::from_created(context, result.scope, *created)
                            .map_err(|error| error.message().into())
                    });
                    match attachment {
                        Ok(attachment) => {
                            self.hosts.connection_notice =
                                Some((disposition, std::time::Instant::now()));
                            self.install_host(ctx, attachment);
                        }
                        Err(error) => self.hosts.error = Some(error),
                    }
                }
                Outcome::Failed(error) => {
                    self.hosts.saved_connect = None;
                    self.hosts.error = Some(error.message().into());
                }
                Outcome::Ended { target } => {
                    self.hosts.inventory = None;
                    self.hosts.error = Some(format!(
                        "Host confirmed End for logical {} generation {}. No replacement was touched.",
                        target.logical,
                        target.generation.get()
                    ));
                }
                _ => {
                    self.hosts.error =
                        Some("Unexpected HOST control result; no terminal was installed.".into())
                }
            }
        }
        self.show_host_connection_notice(ctx);
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
        self.park_active();
        let id = self.push_tab(title, None);
        self.hosts.session_accounts.push(SessionAccount {
            session: Arc::downgrade(&session),
            account,
            tab: id,
            owner: self
                .hosts
                .receipt
                .as_ref()
                .map(|receipt| receipt.binding().clone()),
        });
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
        #[cfg(all(test, unix))]
        if self.hosts.gallery_display.is_some() {
            return Err("Synthetic HOST gallery actions are disabled.".into());
        }
        if matches!(action, Action::Settings) {
            self.hosts.cancel();
            self.hosts.open = false;
            self.shell.tab = super::shell::Tab::Settings;
            self.surface = Surface::Tabs;
            return Ok(());
        }
        if matches!(action, Action::CancelForget) {
            self.hosts.forget_review = None;
            return Ok(());
        }
        if matches!(action, Action::CancelEnd) {
            self.hosts.end_review = None;
            return Ok(());
        }
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
            let selected_original = original.intent.creation().attachment.to_string();
            self.hosts.clear_review();
            if self.hosts.account != account {
                self.hosts.hosts.clear();
            }
            self.hosts.account = account;
            self.hosts.selected = Some(recipe);
            self.hosts.selected_original = Some(selected_original);
            return Ok(());
        }
        if !self.host_foreground() || !self.host_recipe_current() {
            return Err(
                "Unlock and foreground the app, then explicitly review the original host again."
                    .into(),
            );
        }
        if matches!(action, Action::CancelEdit) {
            self.hosts.editing = false;
            self.hosts.quick_new = false;
            return Ok(());
        }
        if matches!(action, Action::EditProfile) {
            self.hosts.begin_profile_edit();
            return Ok(());
        }
        if matches!(action, Action::NewProfile) {
            let previous = self
                .hosts
                .selected
                .as_ref()
                .ok_or("Choose a saved host first.")?;
            let mut recipe =
                PublicRecipe::persistent("New profile", previous.host.clone(), Recipe::default())
                    .map_err(|_| "The saved host is invalid.")?;
            recipe.host_hint = previous.host_hint.clone();
            recipe.behavior = ConnectionBehavior::ChooseSession;
            self.hosts.selected = Some(recipe);
            self.hosts.saved = false;
            self.hosts.selected_original = None;
            self.hosts.launch = false;
            self.hosts.begin_profile_edit();
            self.hosts.profile_name.clear();
            self.hosts.session_name.clear();
            self.hosts.create_if_missing = false;
            return Ok(());
        }
        if let Action::FindAccount(index) = action {
            let account = self
                .profiles
                .as_ref()
                .and_then(|profiles| {
                    profiles
                        .store_handle()
                        .lock()
                        .ok()?
                        .accounts()
                        .get(index)
                        .cloned()
                })
                .ok_or("Account selection expired. Open Settings to sign in.")?;
            if self.hosts.account != account.id {
                self.hosts.clear_review();
                self.hosts.hosts.clear();
                self.hosts.discovered = false;
                self.hosts.account = account.id;
            }
            return self.host_action(ctx, Action::Find);
        }
        if let Action::SelectHost(index) = action {
            let locator = self
                .hosts
                .hosts
                .get(index)
                .ok_or("Host selection expired.")?
                .locator
                .clone();
            if self.hosts.locator != locator {
                self.hosts.clear_review();
                self.hosts.locator = locator;
                self.hosts.host_collapsed = false;
            } else {
                self.hosts.host_collapsed = !self.hosts.host_collapsed;
            }
            return Ok(());
        }
        if matches!(action, Action::QuickNew) {
            if !self.hosts.quick_creation_allowed() {
                return Err("This host does not currently permit new sessions.".into());
            }
            let previous = self.hosts.selected.as_ref().ok_or("Choose a host first.")?;
            let mut recipe =
                PublicRecipe::persistent("New session", previous.host.clone(), Recipe::default())
                    .map_err(|_| "The host recipe is invalid.")?;
            recipe.host_hint = previous.host_hint.clone();
            recipe.behavior = ConnectionBehavior::ChooseSession;
            self.hosts.selected = Some(recipe);
            self.hosts.saved = false;
            self.hosts.selected_original = None;
            self.hosts.launch = false;
            self.hosts.quick_new = true;
            self.hosts.quick_name.clear();
            self.hosts.editing = false;
            return Ok(());
        }
        if matches!(action, Action::QuickCreate) {
            if self.hosts.busy() {
                return Err("A host action is already running.".into());
            }
            if !self.hosts.launch
                && self.hosts.selected.as_ref().is_some_and(|recipe| {
                    self.hosts.originals.iter().any(|original| {
                        original.recipe.id == recipe.id && !original.intent.is_presented()
                    })
                })
            {
                return Err(
                    "Review the retained original in Details & recovery before retrying.".into(),
                );
            }
            let account = self.host_account().ok_or("Sign in from Settings first.")?;
            self.hosts.freeze_quick_recipe(&account)?;
            if self.hosts.original_store.is_none() {
                self.hosts.context()?;
                let receipt = self
                    .hosts
                    .receipt
                    .as_ref()
                    .ok_or("Protected owner approval unavailable.")?;
                self.hosts.quick_check = Some(QuickCheck {
                    recipe: self.hosts.selected.clone().expect("frozen quick recipe"),
                    account,
                    epoch: self.hosts.epoch,
                    generation: self.hosts.generation,
                    owner: receipt.binding().clone(),
                    reference: receipt.reference(),
                });
                self.hosts.launch = false;
                let result = self.host_action(ctx, Action::RecoverOriginals);
                if result.is_err() {
                    self.hosts.quick_check = None;
                }
                return result;
            }
            return self.host_action(ctx, Action::Create);
        }
        if matches!(action, Action::SaveProfile) {
            let recipe = self.hosts.profile_draft()?;
            let command = if self.hosts.saved {
                MetadataCommand::Edit(recipe.clone())
            } else {
                MetadataCommand::Save(recipe.clone())
            };
            self.profiles
                .as_ref()
                .ok_or("Profile storage unavailable.")?
                .store_handle()
                .lock()
                .map_err(|_| "Profile store busy.")?
                .apply_host_recipe_command(command)
                .map_err(|error| error.to_string())?;
            self.hosts.selected = Some(recipe);
            self.hosts.saved = true;
            self.hosts.editing = false;
            self.hosts.launch = false;
            self.hosts.error = None;
            return Ok(());
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
        #[cfg(test)]
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
        let account = self.host_account().ok_or("Sign in from Settings first.")?;
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
        let forget_id = match action {
            Action::ReviewForgetCreation(index) => Some(OriginalId::Creation(
                self.hosts
                    .originals
                    .get(index)
                    .ok_or("Original selection expired.")?
                    .intent
                    .creation()
                    .attachment
                    .clone(),
            )),
            Action::ReviewForgetEnd(index) => Some(OriginalId::End(
                self.hosts
                    .ends
                    .get(index)
                    .ok_or("Original End selection expired.")?
                    .intent
                    .operation()
                    .clone(),
            )),
            _ => None,
        };
        if let Some(id) = &forget_id {
            if !self
                .hosts
                .has_forget_selection(id, &account, &self.hosts.context()?)
            {
                return Err("Review the original protected host and authenticated account before local Forget.".into());
            }
            if self.hosts.original_store.is_none() {
                return Err("Explicitly reopen protected originals before local Forget.".into());
            }
            self.hosts.forget_review = None;
        }
        let forget_commit = if matches!(action, Action::ConfirmForget) {
            let review = self
                .hosts
                .forget_review
                .as_ref()
                .ok_or("Review local Forget first.")?;
            let context = self.hosts.context()?;
            review.authorize(&account, &context, self.hosts.epoch, self.hosts.generation)?;
            if !self
                .hosts
                .has_forget_selection(review.record.id(), &account, &context)
            {
                return Err("The selected original changed; local Forget was not started.".into());
            }
            Some(
                self.hosts
                    .forget_review
                    .take()
                    .expect("authorized review")
                    .record,
            )
        } else {
            None
        };
        match action {
            Action::ReviewEnd(index) => {
                let inventory = self
                    .hosts
                    .inventory
                    .as_ref()
                    .ok_or("Refresh authorized inventory first.")?;
                if !inventory.capabilities.explicit_end {
                    return Err("This host does not advertise explicit End.".into());
                }
                let target = inventory
                    .sessions
                    .get(index)
                    .ok_or("End selection expired.")?
                    .target
                    .clone();
                let context = self.hosts.context()?;
                let intent = self.hosts.end_intent(&context, target, &account)?;
                self.review_end(account, intent);
                return Ok(());
            }
            Action::ReviewOriginalEnd(index) => {
                let original = self
                    .hosts
                    .ends
                    .get(index)
                    .ok_or("Original End selection expired.")?;
                if original.account != account
                    || !original.intent.matches_context(&self.hosts.context()?)
                {
                    return Err(
                        "Review the original protected host and account before retrying End."
                            .into(),
                    );
                }
                let intent = original.intent.clone();
                self.review_end(account, intent);
                return Ok(());
            }
            Action::ConfirmEnd => {
                let review = self
                    .hosts
                    .end_review
                    .as_ref()
                    .ok_or("Review the exact End target first.")?;
                if let Some((tab, session)) = review.source {
                    if !self
                        .tabs
                        .iter()
                        .any(|current| current.id == tab && current.host)
                        || !self.hosts.session_accounts.iter().any(|owner| {
                            owner.tab == tab
                                && owner
                                    .session
                                    .upgrade()
                                    .is_some_and(|current| current.id() == session)
                        })
                    {
                        return Err("The original adopted HOST tab retired; review authorized inventory again.".into());
                    }
                }
                let context = self.hosts.context()?;
                let protected = self.hosts.original_store.clone()
                    .ok_or("Explicitly reopen protected originals before any Create or End; no operation issued.")?;
                let intent = review.authorize(
                    &account,
                    &context,
                    self.hosts.epoch,
                    self.hosts.generation,
                )?;
                intent
                    .protect_original(
                        protected,
                        OriginalReview {
                            account: account.clone(),
                            recipe: None,
                        }
                        .encode()?,
                    )
                    .map_err(|error| error.message().to_owned())?;
                self.hosts.retain_end(&context, account, intent.clone())?;
                self.start_host_operation(ctx, host_client::Operation::End(intent))?;
                self.hosts.end_review = None;
                return Ok(());
            }
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
                let protected = self.hosts.original_store.clone()
                    .ok_or("Explicitly reopen protected originals before any Create or End; no operation issued.")?;
                let review = OriginalReview {
                    account: account.clone(),
                    recipe: Some(recipe.clone()),
                }
                .encode()?;
                let intent = self.hosts.creation_intent(&context, recipe, account)?;
                intent
                    .protect_original(protected, review)
                    .map_err(|error| error.message().to_owned())?;
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
        #[cfg(not(target_os = "android"))]
        let root = profiles.host_approval_directory();
        let recovery_context = if matches!(action, Action::RecoverOriginals)
            || forget_id.is_some()
            || forget_commit.is_some()
        {
            if matches!(action, Action::RecoverOriginals) {
                self.hosts.original_store = None;
            }
            Some(self.hosts.context()?)
        } else {
            None
        };
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
            && ((!self.hosts.enrolled && self.hosts.saved_connect != Some(SavedConnect::Resolve))
                || self.hosts.receipt.is_none())
        {
            return Err(host_client::ENROLLMENT_PREREQUISITE.into());
        }
        let snapshot = account.clone();
        let snapshot_recipe = selected.clone();
        let original_store = self.hosts.original_store.clone();
        let generation = self.hosts.generation;
        let job = Job::spawn(ctx, move |legacy_cancel| {
            let _permit = permit;
            if cancel.is_cancelled() || legacy_cancel.is_cancelled() {
                return Err("HOST action cancelled.".into());
            }
            let index = || {
                #[cfg(target_os = "android")]
                {
                    festerm_secret_store::NativeAppPrivateRoot::current()
                        .map(|root| ReceiptIndex::in_native_app_sandbox(Arc::new(root)))
                        .map_err(|_| {
                            "Native application storage unavailable; no path fallback.".to_owned()
                        })
                }
                #[cfg(not(target_os = "android"))]
                {
                    ReceiptIndex::in_app_sandbox(&root).map_err(|error| error.message().to_owned())
                }
            };
            let result = match action {
                Action::ReviewForgetCreation(_) | Action::ReviewForgetEnd(_) => original_store
                    .ok_or("Protected original store no longer current.")?
                    .review_forget(
                        recovery_context.as_ref().expect("explicit local review"),
                        forget_id.expect("selected local original"),
                        &cancel,
                    )
                    .map(|record| ResultEvent::ForgetReview(Box::new(record), generation))
                    .map_err(|error| error.message().to_owned()),
                Action::ConfirmForget => forget_commit
                    .expect("confirmed local review")
                    .forget(
                        recovery_context
                            .as_ref()
                            .expect("explicit local confirmation"),
                        &cancel,
                    )
                    .map(|forgotten| ResultEvent::Forgotten(forgotten, generation))
                    .map_err(|error| error.message().to_owned()),
                Action::RecoverOriginals => {
                    let context = recovery_context
                        .as_ref()
                        .expect("explicit protected recovery");
                    #[cfg(target_os = "android")]
                    let result = {
                        let root = festerm_secret_store::NativeAppPrivateRoot::current().map_err(
                            |_| {
                                "Native application storage unavailable; no path fallback."
                                    .to_owned()
                            },
                        )?;
                        OriginalStore::recover_in_native_app_sandbox(Arc::new(root), context)
                    };
                    #[cfg(not(target_os = "android"))]
                    let result = OriginalStore::recover(&root, context);
                    result
                        .map(|(store, originals)| ResultEvent::Originals(store, originals))
                        .map_err(|error| error.message().to_owned())
                }
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
                        .map_err(|_| "Sign in again from Settings.")?;
                    if identity.needs_sign_in() {
                        return Err("Sign in again from Settings.".into());
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

    fn show_host_recovery(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        if self.hosts.resolved.is_some()
            && self.hosts.enrolled
            && ui
                .button("Explicitly reopen protected original actions (no network retry)")
                .clicked()
        {
            *action = Some(Action::RecoverOriginals);
        }
        if self.hosts.original_store.is_none() {
            ui.label("Create/End requires an explicit protected-original check under the current owner/account. A missing, locked or corrupt checkpoint never grants a replacement action.");
        }
        for (index, original) in self.hosts.originals.iter().enumerate() {
            let (review, forget) = display::creation_original(
                ui,
                &original.recipe.name,
                &original.intent.creation().logical,
                original.intent.issued_operation(),
                self.hosts.original_store.is_some()
                    && self.hosts.pending.is_none()
                    && self.hosts.worker.is_none(),
            );
            if review {
                *action = Some(Action::ReviewOriginal(index));
            }
            if forget {
                *action = Some(Action::ReviewForgetCreation(index));
            }
        }
        for (index, original) in self.hosts.ends.iter().enumerate() {
            let (review, forget) = display::end_original(
                ui,
                original.intent.target(),
                original.intent.operation(),
                original.intent.is_issued(),
                original.intent.is_resolved(),
                self.hosts.original_store.is_some()
                    && self.hosts.pending.is_none()
                    && self.hosts.worker.is_none(),
            );
            if review {
                *action = Some(Action::ReviewOriginalEnd(index));
            }
            if forget {
                *action = Some(Action::ReviewForgetEnd(index));
            }
        }
        #[cfg(all(test, unix))]
        self.show_host_gallery_recovery(ui);
        if let Some(review) = &mut self.hosts.forget_review {
            let (confirm, cancel) = display::forget_review(
                ui,
                display::ForgetDisplay {
                    account: review.account.account_id,
                    id: review.record.id(),
                    logical: review.record.logical(),
                    target: review.record.target(),
                    epoch: review.epoch,
                    generation: review.generation,
                },
                &mut review.confirmed,
            );
            if confirm {
                *action = Some(Action::ConfirmForget);
            }
            if cancel {
                *action = Some(Action::CancelForget);
            }
        }
        if let Some(review) = &mut self.hosts.end_review {
            let (confirm, cancel) = display::end_review(
                ui,
                display::EndDisplay {
                    account: review.account.account_id,
                    target: review.intent.target(),
                    operation: review.intent.operation(),
                    epoch: review.epoch,
                    generation: review.generation,
                },
                &mut review.confirmed,
            );
            if confirm {
                *action = Some(Action::ConfirmEnd);
            }
            if cancel {
                *action = Some(Action::CancelEnd);
            }
        }
        ui.label("Originals are protected in the native vault before a mutation can be sent. After restart explicitly approve this same owner/account, reopen originals, review and confirm the exact original again. No automatic retry, input replay or background reconnect. Pre-checkpoint-version lost actions cannot be reconstructed.");
    }
}

#[path = "host_clients_display.rs"]
mod display;

#[path = "host_clients_view.rs"]
mod view;

#[cfg(test)]
#[path = "host_clients_tests.rs"]
mod tests;
