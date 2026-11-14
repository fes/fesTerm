//! Foreground-only HOST consent and authorized inventory. Public recipes enter
//! as untrusted selection hints, never as native credential references.

use super::FesTermApp;
use crate::tabs::{AppCommand, AppState, TabContent, TabId};
use eframe::egui;
use festerm_config::{
    host_recipes::{
        AccountReference, ConnectionBehavior, HostHint, HostReference, Lifetime, PublicRecipe,
        Recipe,
    },
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
    EnsureProfile {
        logical: host_client::LogicalSessionId,
        revision: festerm_host_protocol::Epoch,
        name: String,
    },
    Create,
}

/// Reuse only an unambiguous same-account, exact-locator public identity.
/// The protected receipt and live host pin still have to verify before inventory.
fn known_host_reference(
    context: &egui::Context,
    configuration: &festerm_config::Configuration,
    account_id: u64,
    locator: &str,
) -> Option<HostReference> {
    let remembered = crate::screens::host_workspace::remembered_hosts(context);
    let candidates = remembered
        .iter()
        .filter(|host| host.hint.locator == locator)
        .map(|host| &host.reference)
        .chain(
            configuration
                .host_recipes()
                .recipes()
                .iter()
                .filter_map(|recipe| {
                    recipe
                        .host_hint
                        .as_ref()
                        .filter(|hint| hint.locator == locator)
                        .map(|_| &recipe.host)
                }),
        )
        .filter(|reference| reference.account == AccountReference::Github { account_id });
    let mut known = None;
    for reference in candidates {
        if known.as_ref().is_some_and(|known| known != reference) {
            return None;
        }
        known = Some(reference.clone());
    }
    known
}

fn valid_session_name(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

pub(super) const MAX_PARALLEL_DISCOVERIES: usize = 3;
const MAX_QUEUED_DISCOVERIES: usize = 32;

/// Reserves a discovery permit only when another permit would still remain,
/// so background host refreshes never take the last connection slot.
fn reserve_with_headroom(budgets: &host_client::Budgets) -> Option<ConnectionPermit> {
    let permit = ConnectionPermit::reserve(budgets).ok()?;
    ConnectionPermit::reserve(budgets).ok()?;
    Some(permit)
}

pub(super) struct QueuedDiscovery {
    tab: TabId,
    request: egui::Id,
    account: DevTunnelAccount,
}

pub(super) struct Discovery {
    tab: TabId,
    request: egui::Id,
    account: DevTunnelAccount,
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Result<Vec<service::TunnelSummary>, String>>,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
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
    #[cfg(test)]
    gallery_owner: Option<(festerm_tunnel_bridge::owner_auth::OwnerBinding, [u8; 32])>,
    #[cfg(test)]
    gallery_original: Option<(String, bool)>,
    resolved: Option<ResolvedTunnel>,
    receiver: Option<mpsc::Receiver<Result<ResultEvent, String>>>,
    pending_deadline: Option<(Instant, CancellationToken)>,
    worker: Option<Worker>,
    inventory: Option<Inventory>,
    generation: u64,
    feedback: Option<String>,
    workspace: bool,
    discovered: bool,
    auto_connect: bool,
    new_session: bool,
    session_name: String,
    profile_name: String,
    unbound: bool,
    resolve_after_load: bool,
    profile_target: Option<Target>,
    profile_create: bool,
    profile_label: Option<String>,
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
impl FesTermApp {
    pub(crate) fn gallery_host_browser(scene: &str) -> Self {
        let requested_scene = scene;
        let scene = match scene {
            "retry-original" | "resolved-original" | "always-create" | "attach-only" => "create",
            _ => scene,
        };
        use festerm_config::{
            host_recipes::{HostReference, Recipe},
            AccountProvider, Configuration,
        };
        let account = DevTunnelAccount::new(
            "synthetic-github",
            AccountProvider::Github,
            42,
            "synthetic-devuser",
            SecretReference::generate(),
        )
        .unwrap();
        let mut recipe = PublicRecipe::persistent(
            "Synthetic build host",
            HostReference {
                id: "11111111-1111-4111-8111-111111111111".parse().unwrap(),
                account: AccountReference::Github { account_id: 42 },
            },
            Recipe {
                shell: Some("/bin/sh".into()),
                args: vec!["-l".into()],
                cwd: Some("/work/synthetic".into()),
            },
        )
        .unwrap();
        recipe.id =
            festerm_config::host_recipes::RecipeId::parse("22222222-2222-4222-8222-222222222222")
                .unwrap();
        recipe.logical_session = "33333333-3333-4333-8333-333333333333".parse().unwrap();
        let (configuration, _) = Configuration::empty()
            .with_account(account.clone())
            .unwrap();
        let mut app = Self::for_test_with_configuration(configuration);
        let context = egui::Context::default();
        app.state.dispatch(AppCommand::OpenProfiles, &context);
        let mut browser = Browser {
            tab: app.state.active(),
            request: egui::Id::new("gallery-host-review"),
            recipe,
            account_reference: account.credential_reference().duplicate_for_transport(),
            account,
            cancel: CancellationToken::new(),
            prerequisite: zeroize::Zeroizing::new(String::new()),
            fingerprint: String::new(),
            consent: false,
            host_approved: false,
            creation_confirmed: false,
            locator: "review-build.use".into(),
            receipt: None,
            gallery_owner: None,
            gallery_original: None,
            resolved: None,
            receiver: None,
            pending_deadline: None,
            worker: None,
            inventory: None,
            generation: 1,
            feedback: None,
            workspace: false,
            discovered: false,
            auto_connect: false,
            new_session: false,
            session_name: String::new(),
            profile_name: String::new(),
            unbound: false,
            resolve_after_load: false,
            profile_target: None,
            profile_create: false,
            profile_label: None,
        };
        if scene == "error" {
            browser.feedback = Some("Unlock the native credential store and retry explicitly. No file fallback, host enrollment or session creation occurred.".into());
        }
        if scene == "working" {
            let (_, receiver) = mpsc::channel();
            browser.receiver = Some(receiver);
        }
        if matches!(
            scene,
            "owner" | "inventory" | "unknown" | "sessions" | "create" | "temporary"
        ) {
            use festerm_tunnel_bridge::owner_auth::{OwnerBinding, PinnedHost};
            browser.gallery_owner = Some((
                OwnerBinding {
                    installation: "44444444-4444-4444-8444-444444444444".into(),
                    host: PinnedHost {
                        host: browser.recipe.host.id.clone(),
                        host_key_fingerprint: "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
                            .into(),
                    },
                    account: "github:42".into(),
                    principal: "55555555-5555-4555-8555-555555555555".parse().unwrap(),
                },
                [0; 32],
            ));
            browser.host_approved = scene != "owner";
        }
        if matches!(
            scene,
            "inventory" | "unknown" | "sessions" | "create" | "temporary"
        ) {
            browser.inventory = Some(Inventory {
                capabilities: Default::default(),
                sessions: Vec::new(),
                profiles: Vec::new(),
                unknown: if scene == "unknown" {
                    vec![Target {
                        logical: browser.recipe.logical_session.clone(),
                        generation: "7".to_owned().try_into().unwrap(),
                    }]
                } else {
                    Vec::new()
                },
            });
            let inventory = browser.inventory.as_mut().unwrap();
            inventory.capabilities.attach = true;
            inventory.capabilities.inventory = true;
            if matches!(scene, "sessions" | "create" | "temporary") {
                inventory.sessions = serde_json::from_value(serde_json::json!([
                    {
                        "target": { "logical": "33333333-3333-4333-8333-333333333333", "generation": "7" },
                        "name": "Synthetic build", "lifetime": { "kind": "persistent" }, "attached": false
                    },
                    {
                        "target": { "logical": "66666666-6666-4666-8666-666666666666", "generation": "8" },
                        "name": "Synthetic occupied shell", "lifetime": { "kind": "persistent" }, "attached": true
                    }
                ])).expect("synthetic authorized session summaries");
            }
            if scene == "create" || scene == "temporary" {
                inventory.capabilities.create = true;
                inventory.capabilities.ensure = true;
                inventory.capabilities.creation_allowed = true;
                browser.creation_confirmed = true;
            }
            if scene == "temporary" {
                inventory.capabilities.temporary = true;
                browser.recipe.lifetime = Lifetime::Temporary {
                    reconnect_grace_seconds: 60,
                };
            }
        }
        match requested_scene {
            "retry-original" | "resolved-original" => {
                browser.gallery_original = Some((
                    format!("Original action · logical {} · attachment 77777777-7777-4777-8777-777777777777 · issued None", browser.recipe.logical_session),
                    requested_scene == "resolved-original",
                ));
            }
            "always-create" => browser.recipe.behavior = ConnectionBehavior::AlwaysCreate,
            "attach-only" => {
                browser.recipe.behavior = ConnectionBehavior::AttachOnly {
                    generation: "7".to_owned().try_into().unwrap(),
                }
            }
            _ => {}
        }
        app.host_browser = Some(browser);
        app
    }
}

impl Browser {
    fn current(&self, state: &AppState, context: &egui::Context) -> bool {
        let origin_current = state.tabs().iter().any(|tab| {
            tab.id == self.tab
                && if self.workspace {
                    matches!(tab.content, TabContent::Launcher | TabContent::Profiles)
                        && crate::screens::host_workspace::request_is_current(
                            context,
                            self.tab,
                            self.request,
                        )
                } else {
                    matches!(tab.content, TabContent::Profiles)
                        && crate::screens::host_recipe_request_is_current(
                            context,
                            self.tab,
                            self.request,
                        )
                }
        });
        origin_current
            && (self.discovered
                || state.configuration().host_recipes().get(&self.recipe.id) == Some(&self.recipe))
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
            ConnectionBehavior::ResumeOrCreate | ConnectionBehavior::ResumeOnly {} => {
                target.logical == self.recipe.logical_session
            }
            ConnectionBehavior::AttachOnly { generation } => {
                target.logical == self.recipe.logical_session && target.generation == *generation
            }
            ConnectionBehavior::AlwaysCreate => false,
        }
    }

    fn automatic_target(&self) -> Option<&Target> {
        if matches!(self.recipe.behavior, ConnectionBehavior::ChooseSession) {
            return None;
        }
        self.inventory
            .as_ref()?
            .sessions
            .iter()
            .find(|session| self.allows_target(&session.target))
            .map(|session| &session.target)
    }

    fn creation_mode(&self) -> Option<CreationMode> {
        match self.recipe.behavior {
            ConnectionBehavior::ResumeOrCreate => Some(CreationMode::Ensure),
            ConnectionBehavior::AlwaysCreate | ConnectionBehavior::ChooseSession => {
                Some(CreationMode::Create)
            }
            ConnectionBehavior::AttachOnly { .. } | ConnectionBehavior::ResumeOnly {} => None,
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
                name: Some(self.recipe.session_name().to_owned()),
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
        let locator = recipe
            .host_hint
            .as_ref()
            .map(|hint| hint.locator.clone())
            .unwrap_or_default();
        let resume_saved = !locator.is_empty();
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
            locator,
            receipt: None,
            #[cfg(test)]
            gallery_owner: None,
            #[cfg(test)]
            gallery_original: None,
            resolved: None,
            receiver: None,
            pending_deadline: None,
            worker: None,
            inventory: None,
            generation: 0,
            feedback: None,
            workspace: false,
            discovered: false,
            auto_connect: resume_saved,
            new_session: false,
            session_name: String::new(),
            profile_name: String::new(),
            unbound: false,
            resolve_after_load: resume_saved,
            profile_target: None,
            profile_create: false,
            profile_label: None,
        });
        if resume_saved {
            self.apply_host_review_action(Action::Load, context);
        }
    }

    pub(super) fn find_host_tunnels(
        &mut self,
        tab: TabId,
        request: egui::Id,
        account_key: &str,
        context: &egui::Context,
    ) {
        let Some(account) = self.state.configuration().account(account_key).cloned() else {
            return;
        };
        if !self.host_discovery_is_current(tab, request, &account, context) {
            return;
        }
        // A newer refresh for the same account supersedes older work; other
        // accounts keep running in parallel.
        self.host_discovery
            .retain(|previous| !(previous.tab == tab && previous.account == account));
        self.host_discovery_queue
            .retain(|previous| !(previous.tab == tab && previous.account == account));
        if self.host_discovery_queue.len() >= MAX_QUEUED_DISCOVERIES {
            crate::screens::host_workspace::report_discovery(
                context,
                tab,
                request,
                account_key,
                Err("Too many host refreshes are waiting. Try again.".into()),
            );
            return;
        }
        self.host_discovery_queue.push_back(QueuedDiscovery {
            tab,
            request,
            account,
        });
        self.start_queued_host_discoveries(context);
    }

    fn host_discovery_is_current(
        &self,
        tab: TabId,
        request: egui::Id,
        account: &DevTunnelAccount,
        context: &egui::Context,
    ) -> bool {
        self.state.tabs().iter().any(|candidate| {
            candidate.id == tab
                && matches!(
                    candidate.content,
                    TabContent::Launcher | TabContent::Profiles
                )
        }) && crate::screens::host_workspace::discovery_is_current(
            context,
            tab,
            request,
            account.identifier(),
        ) && self
            .state
            .configuration()
            .account(account.identifier())
            .is_some_and(|configured| configured == account)
    }

    /// Starts queued discoveries while at most [`MAX_PARALLEL_DISCOVERIES`]
    /// run and one shared connection permit stays free for interactive
    /// connects. Queued work waits for the next UI pass; it is never polled
    /// on a timer.
    fn start_queued_host_discoveries(&mut self, context: &egui::Context) {
        while self.host_discovery.len() < MAX_PARALLEL_DISCOVERIES {
            let Some(front) = self.host_discovery_queue.front() else {
                return;
            };
            if !self.host_discovery_is_current(front.tab, front.request, &front.account, context) {
                self.host_discovery_queue.pop_front();
                continue;
            }
            let store = match &self.secret_store {
                Ok(store) => Arc::clone(store),
                Err(_) => {
                    let next = self.host_discovery_queue.pop_front().expect("queued");
                    crate::screens::host_workspace::report_discovery(
                        context,
                        next.tab,
                        next.request,
                        next.account.identifier(),
                        Err("Unlock secure storage and try again.".into()),
                    );
                    continue;
                }
            };
            let Some(permit) = reserve_with_headroom(&self.state.host_client_budgets) else {
                return;
            };
            let next = self.host_discovery_queue.pop_front().expect("queued");
            let cancel = CancellationToken::new();
            let work_cancel = cancel.clone();
            let work_account = next.account.clone();
            let wake = context.clone();
            let (sender, receiver) = mpsc::sync_channel(1);
            let spawned = std::thread::Builder::new().name("festerm-find-hosts".into()).spawn(move || {
            let _permit = permit;
            let result = (|| {
                let _lease = super::tunnel_workflow::account_lease(Some(&work_account))
                    .map_err(|error| error.to_string())?;
                let client = identity::registered_client_id().ok_or("GitHub sign-in is unavailable.")?;
                let secret = store.get(work_account.credential_reference())
                    .map_err(|_| "Unlock secure storage and try again.")?;
                let identity = secret.with_bytes(|bytes| Identity::decode_private(bytes,
                    work_account.account_id(), client)).map_err(|error| error.to_string())?;
                if identity.needs_sign_in() {
                    return Err("GitHub sign-in expired. Sign in again in Settings.".into());
                }
                let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(|_| "Host discovery could not start.")?;
                runtime.block_on(async {
                    tokio::select! {
                        _ = work_cancel.cancelled() => Err("Discovery cancelled.".into()),
                        result = tokio::time::timeout(Duration::from_secs(30),
                            service::list_hosts(&identity, &work_cancel)) => {
                            let mut hosts = result.map_err(|_| "Host discovery timed out. Try again.")?
                                .map_err(|error| error.to_string())?;
                            hosts.truncate(32);
                            Ok(hosts)
                        }
                    }
                })
            })();
            if !work_cancel.is_cancelled() { let _ = sender.try_send(result); wake.request_repaint(); }
        });
            if spawned.is_err() {
                crate::screens::host_workspace::report_discovery(
                    context,
                    next.tab,
                    next.request,
                    next.account.identifier(),
                    Err("Host discovery could not start.".into()),
                );
            } else {
                self.host_discovery.push(Discovery {
                    tab: next.tab,
                    request: next.request,
                    account: next.account,
                    cancel,
                    receiver,
                });
            }
        }
    }

    pub(super) fn poll_host_discovery(&mut self, context: &egui::Context) {
        let mut index = 0;
        while index < self.host_discovery.len() {
            let discovery = &self.host_discovery[index];
            if !self.host_discovery_is_current(
                discovery.tab,
                discovery.request,
                &discovery.account,
                context,
            ) {
                let discovery = self.host_discovery.remove(index);
                crate::screens::host_workspace::report_discovery(
                    context,
                    discovery.tab,
                    discovery.request,
                    discovery.account.identifier(),
                    Err(
                        "Discovery cancelled because its account or screen changed. Refresh again."
                            .into(),
                    ),
                );
                continue;
            }
            let result = match discovery.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Host discovery stopped. Try again.".into())
                }
            };
            let discovery = self.host_discovery.remove(index);
            crate::screens::host_workspace::report_discovery(
                context,
                discovery.tab,
                discovery.request,
                discovery.account.identifier(),
                result,
            );
        }
        self.start_queued_host_discoveries(context);
    }

    pub(super) fn review_discovered_host(
        &mut self,
        tab: TabId,
        request: egui::Id,
        host: crate::screens::host_workspace::HostChoice,
        context: &egui::Context,
    ) {
        let Some(account) = self.state.configuration().account(&host.account) else {
            return;
        };
        let known = host.reference.or_else(|| {
            known_host_reference(
                context,
                self.state.configuration(),
                account.account_id(),
                &host.locator,
            )
        });
        let unbound = known.is_none();
        let reference = known.unwrap_or_else(|| HostReference {
            id: uuid::Uuid::new_v4()
                .to_string()
                .try_into()
                .expect("UUID host hint"),
            account: AccountReference::Github {
                account_id: account.account_id(),
            },
        });
        let Ok(mut recipe) = PublicRecipe::persistent(
            host.name.clone(),
            reference,
            Recipe {
                shell: None,
                args: Vec::new(),
                cwd: None,
            },
        ) else {
            crate::screens::host_workspace::report_outcome(
                context,
                tab,
                request,
                "The host metadata is invalid.",
            );
            return;
        };
        recipe.behavior = ConnectionBehavior::ChooseSession;
        recipe.host_hint = Some(HostHint {
            locator: host.locator,
            name: host.name,
        });
        self.start_workspace_host(tab, request, recipe, true, unbound, false, context);
    }

    pub(super) fn review_workspace_recipe(
        &mut self,
        tab: TabId,
        request: egui::Id,
        recipe: PublicRecipe,
        context: &egui::Context,
    ) {
        if self.state.configuration().host_recipes().get(&recipe.id) != Some(&recipe) {
            return;
        }
        self.start_workspace_host(tab, request, recipe, false, false, true, context);
    }

    #[allow(clippy::too_many_arguments)]
    fn start_workspace_host(
        &mut self,
        tab: TabId,
        request: egui::Id,
        recipe: PublicRecipe,
        discovered: bool,
        unbound: bool,
        auto_connect: bool,
        context: &egui::Context,
    ) {
        if recipe.validate().is_err() {
            crate::screens::host_workspace::report_outcome(
                context,
                tab,
                request,
                "The saved host or session metadata is invalid.",
            );
            return;
        }
        if !self.state.tabs().iter().any(|candidate| {
            candidate.id == tab
                && matches!(
                    candidate.content,
                    TabContent::Launcher | TabContent::Profiles
                )
        }) || !crate::screens::host_workspace::request_is_current(context, tab, request)
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
            crate::screens::host_workspace::report_outcome(
                context,
                tab,
                request,
                "Sign in to this account in Settings.",
            );
            return;
        };
        let locator = recipe
            .host_hint
            .as_ref()
            .map(|hint| hint.locator.clone())
            .unwrap_or_default();
        let session_name = recipe.session_name().to_owned();
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
            locator,
            receipt: None,
            #[cfg(test)]
            gallery_owner: None,
            #[cfg(test)]
            gallery_original: None,
            resolved: None,
            receiver: None,
            pending_deadline: None,
            worker: None,
            inventory: None,
            generation: 0,
            feedback: None,
            workspace: true,
            discovered,
            auto_connect,
            new_session: false,
            session_name,
            profile_name: String::new(),
            unbound,
            resolve_after_load: !unbound,
            profile_target: None,
            profile_create: false,
            profile_label: None,
        });
        if !unbound {
            self.apply_host_review_action(Action::Load, context);
        }
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
            crate::screens::host_workspace::report_outcome(
                context,
                browser.tab,
                browser.request,
                "Connection cancelled: the host, profile or account changed.",
            );
            return;
        }
        let mut next_action = None;
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
                    if browser.resolve_after_load && service::locator(&browser.locator).is_ok() {
                        browser.resolve_after_load = false;
                        browser.host_approved = true;
                        next_action = Some(Action::Inventory);
                    }
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
                    if let Some(hint) = &browser.recipe.host_hint {
                        crate::screens::host_workspace::remember_host(
                            context,
                            crate::screens::host_workspace::HostSessions {
                                reference: browser.recipe.host.clone(),
                                hint: hint.clone(),
                                sessions: inventory
                                    .sessions
                                    .iter()
                                    .map(|session| {
                                        crate::screens::host_workspace::SavedSessionChoice {
                                            target: session.target.clone(),
                                            name: session.name.clone(),
                                            lifetime: session.lifetime,
                                        }
                                    })
                                    .collect(),
                            },
                        );
                    }
                    browser.inventory = Some(inventory);
                    browser.feedback = None;
                    if browser.auto_connect {
                        browser.auto_connect = false;
                        let inventory = browser.inventory.as_ref().expect("published inventory");
                        if let Some(session) = browser.automatic_target().and_then(|target| {
                            inventory
                                .sessions
                                .iter()
                                .find(|session| &session.target == target)
                        }) {
                            if session.attached {
                                browser.feedback = Some("This session is already attached.".into());
                            } else {
                                next_action = Some(Action::Attach(session.target.clone()));
                            }
                        } else if matches!(
                            browser.recipe.behavior,
                            ConnectionBehavior::ResumeOrCreate
                        ) && browser.recipe.launch.shell.is_none()
                            && browser.recipe.launch.args.is_empty()
                            && browser.recipe.launch.cwd.is_none()
                            && matches!(browser.recipe.lifetime, Lifetime::Persistent {})
                        {
                            browser.creation_confirmed = true;
                            next_action = Some(Action::Create);
                        } else if matches!(
                            browser.recipe.behavior,
                            ConnectionBehavior::AttachOnly { .. }
                                | ConnectionBehavior::ResumeOnly {}
                        ) {
                            browser.feedback = Some(
                                if inventory.unknown.is_empty() {
                                    "Session not found."
                                } else {
                                    "Session status is unknown. Refresh to try again."
                                }
                                .into(),
                            );
                        }
                    }
                }
                Outcome::Failed(error) => browser.feedback = Some(error.message().into()),
                Outcome::Created(created) => {
                    if created.disposition() == host_client::EnsureDisposition::Recreated {
                        self.overlays.transient_notice = Some((
                            "Previous session ended. Started a new shell.".into(),
                            Instant::now() + Duration::from_secs(12),
                        ));
                    }
                    let result = browser.context().and_then(|approved| {
                        ApprovedAttachment::from_created(approved, completed_scope, *created)
                            .map_err(|error| error.message().to_owned())
                    });
                    match result {
                        Ok(attachment) => {
                            let tab_id = browser.tab;
                            let request = browser.request;
                            crate::screens::host_workspace::set_connection_label(
                                context,
                                tab_id,
                                request,
                                if browser.discovered {
                                    browser.recipe.session_name().to_owned()
                                } else {
                                    browser.recipe.name.clone()
                                },
                            );
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
                Outcome::ProfileEnsured {
                    session,
                    disposition,
                    attachment,
                } => {
                    if disposition == host_client::EnsureDisposition::Recreated {
                        self.overlays.transient_notice = Some((
                            "Previous session ended. Started a new shell.".into(),
                            Instant::now() + Duration::from_secs(12),
                        ));
                    }
                    let result = browser.context().and_then(|approved| {
                        ApprovedAttachment::from_profile_ensured(
                            approved,
                            completed_scope,
                            session,
                            disposition,
                            attachment,
                        )
                        .map_err(|error| error.message().to_owned())
                    });
                    match result {
                        Ok(attachment) => {
                            let tab_id = browser.tab;
                            let request = browser.request;
                            let label = browser
                                .profile_label
                                .take()
                                .unwrap_or_else(|| "Shared profile".into());
                            crate::screens::host_workspace::set_connection_label(
                                context, tab_id, request, label,
                            );
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
        if let Some(action) = next_action {
            self.apply_host_review_action(action, context);
        }
    }

    pub(super) fn show_host_browser(&mut self, context: &egui::Context, escape: bool) {
        use festerm_ui_egui::controls::{action_button_enabled, ActionButtonRole};

        if self
            .host_browser
            .as_ref()
            .is_some_and(|browser| browser.workspace || browser.recipe.host_hint.is_some())
        {
            self.show_workspace_host(context, escape);
            return;
        }
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
                    let owner = browser.receipt.as_ref().map(|receipt| (receipt.binding(), receipt.public_key()));
                    #[cfg(test)]
                    let owner = owner.or_else(|| browser.gallery_owner.as_ref().map(|(binding, public)| (binding, *public)));
                    if let Some((binding, public)) = owner {
                        ui.separator();
                        ui.label(format!("Approved installation: {}", binding.installation));
                        ui.label(format!("Approved principal: {}", binding.principal));
                        ui.label(format!("Pinned host key: {}", binding.host.host_key_fingerprint));
                        let public_key: String = public.iter().map(|byte| format!("{byte:02x}")).collect();
                        let fingerprint = festerm_tunnel_bridge::owner_auth::owner_key_fingerprint(&public);
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
                        ui.add(egui::TextEdit::singleline(&mut browser.fingerprint).min_size(egui::vec2(0.0, 24.0)).char_limit(128));
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
                            ConnectionBehavior::ResumeOnly {} => "Recipe: resume only this logical session. Never create a replacement.",
                            ConnectionBehavior::AlwaysCreate => "Recipe: explicitly create one fresh logical session. An uncertain reply retains this same action, never another shell.",
                            ConnectionBehavior::AttachOnly { .. } => "Recipe: attach only to its exact logical session and generation.",
                        });
                        if browser.creation_mode().is_some() {
                            ui.monospace(format!("shell {:?}\nargv {:?}\ncwd {:?}\nlifetime {:?}",
                                browser.recipe.launch.shell, browser.recipe.launch.args,
                                browser.recipe.launch.cwd, browser.recipe.lifetime));
                            ui.checkbox(&mut browser.creation_confirmed,
                                "I approve executing these exact launch options and lifetime if the host authorizes creation");
                            let original = self.state.host_creation_intents.iter()
                                .find(|(recipe, _, _)| recipe.id == browser.recipe.id)
                                .map(|(_, _, intent)| (
                                    format!("Original action · logical {} · attachment {} · issued {:?}",
                                        intent.creation().logical, intent.creation().attachment, intent.issued_operation()),
                                    intent.is_resolved() && !intent.is_presented(),
                                ));
                            #[cfg(test)]
                            let original = original.or_else(|| browser.gallery_original.clone());
                            if let Some((text, resolved_without_presentation)) = original {
                                ui.monospace(text);
                                if resolved_without_presentation {
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
                        if inventory.capabilities.profiles {
                            ui.separator();
                            ui.strong("Shared profiles");
                            if inventory.profiles.is_empty() {
                                ui.label("No profiles are enabled for this host.");
                            }
                            for profile in &inventory.profiles {
                                ui.push_id(profile.logical.as_str(), |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(&profile.name);
                                        if action_button_enabled(
                                            ui,
                                            inventory.capabilities.ensure_profile
                                                && inventory.capabilities.attach,
                                            ActionButtonRole::Accent,
                                            "Connect / start",
                                        )
                                        .clicked()
                                        {
                                            action = Some(Action::EnsureProfile {
                                                logical: profile.logical.clone(),
                                                revision: profile.revision,
                                                name: profile.name.clone(),
                                            });
                                        }
                                    });
                                });
                            }
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

    fn show_workspace_host(&mut self, context: &egui::Context, escape: bool) {
        use festerm_ui_egui::controls::{action_button, action_button_enabled, ActionButtonRole};
        let selection_mode = self.host_browser.as_ref().is_some_and(|browser| {
            crate::screens::host_workspace::profile_request_is_current(
                context,
                browser.tab,
                browser.request,
            )
        });
        let Some(browser) = self.host_browser.as_mut() else {
            return;
        };
        let mut dismiss = escape;
        let mut action = None;
        let mut save = None;
        let mut cancel_profile = false;
        let mut quick_create = false;
        let mut selection: Option<Option<Target>> = None;
        egui::Modal::new(egui::Id::new("foreground_host_review")).show(context, |ui| {
            ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(24.0);
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading(browser.recipe.host_hint.as_ref().map(|hint| hint.name.as_str())
                .unwrap_or("Dev Tunnel host"));
            ui.small(format!("@{}", browser.account.login()));
            super::confirmations::confirmation_body(ui, |ui| {
                if browser.busy() {
                    ui.horizontal(|ui| { ui.spinner(); ui.label("Connecting…"); });
                }
                if let Some(message) = &browser.feedback {
                    ui.label(
                        if message.len() > 140 { "The connection needs attention." } else { message });
                    if message.len() > 140 { ui.collapsing("Details", |ui| { ui.label(message); }); }
                }
                ui.add_enabled_ui(!browser.busy(), |ui| {
                    if browser.receipt.is_none() {
                        ui.label("Verify this host");
                        ui.label("Host details");
                        ui.add(egui::TextEdit::multiline(&mut *browser.prerequisite)
                            .char_limit(4096).desired_rows(3));
                        ui.label("Host fingerprint");
                        ui.add(egui::TextEdit::singleline(&mut browser.fingerprint).char_limit(128));
                        ui.checkbox(&mut browser.consent, "I independently verified this host and account");
                        ui.collapsing("Where to find these details", |ui| {
                            ui.label("On the host, open Settings → Dev Tunnels → Remote access → Connect another device. Copy its host details and fingerprint. Never paste a private key or token.");
                        });
                        if action_button_enabled(ui, browser.consent && !browser.fingerprint.is_empty(),
                            ActionButtonRole::Accent, "Prepare client key").clicked() {
                            action = Some(Action::Prepare);
                        }
                        if !browser.unbound && action_button(ui, ActionButtonRole::Secondary,
                            "Retry saved approval").clicked() {
                            browser.resolve_after_load = true;
                            action = Some(Action::Load);
                        }
                    } else if browser.inventory.is_none() {
                        if browser.host_approved
                            && action_button(ui, ActionButtonRole::Secondary, "Review owner approval…").clicked() {
                            browser.host_approved = false;
                            browser.resolved = None;
                        }
                        if !browser.host_approved {
                            ui.label("Approve this client on the host");
                            if let Some(receipt) = &browser.receipt {
                                let public_key: String = receipt.public_key().iter()
                                    .map(|byte| format!("{byte:02x}")).collect();
                                if action_button(ui, ActionButtonRole::Secondary, "Copy public key").clicked() {
                                    context.copy_text(public_key.clone());
                                }
                                if let Ok(fingerprint) = festerm_tunnel_bridge::owner_auth::owner_key_fingerprint(&receipt.public_key()) {
                                    if action_button(ui, ActionButtonRole::Secondary, "Copy owner fingerprint").clicked() {
                                        context.copy_text(fingerprint.clone());
                                    }
                                    ui.collapsing("Approval details", |ui| {
                                        ui.monospace(public_key);
                                        ui.monospace(fingerprint);
                                        ui.label("On the host, open Settings → Dev Tunnels → Remote access → Approve a client. Paste this public key and fingerprint, then confirm the selected permissions.");
                                    });
                                }
                            }
                            ui.checkbox(&mut browser.host_approved, "I completed host-side owner approval");
                        }
                        if browser.locator.is_empty() {
                            ui.label("Tunnel name or id");
                            ui.add(egui::TextEdit::singleline(&mut browser.locator).min_size(egui::vec2(0.0, 24.0)).char_limit(201));
                        }
                        if action_button_enabled(ui, browser.host_approved
                            && service::locator(&browser.locator).is_ok(), ActionButtonRole::Accent,
                            "Show sessions").clicked() {
                            action = Some(Action::Inventory);
                        }
                    } else if let Some(inventory) = browser.inventory.clone() {
                        if selection_mode && action_button(ui, ActionButtonRole::Accent, "Use this host").clicked() {
                            selection = Some(None);
                        }
                        ui.horizontal(|ui| {
                            ui.strong("Sessions");
                            if action_button(ui, ActionButtonRole::Secondary, "Refresh").clicked() {
                                action = Some(Action::Inventory);
                            }
                        });
                        if inventory.sessions.is_empty() { ui.label("No authorized sessions."); }
                        if !inventory.unknown.is_empty() {
                            ui.label("Some session states are unknown.");
                        }
                        for session in &inventory.sessions {
                            ui.push_id(session.target.logical.as_str(), |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(&session.name);
                                    if session.attached { ui.small("Already attached"); }
                                    if selection_mode {
                                        if action_button(ui, ActionButtonRole::Accent, "Select session").clicked() {
                                            selection = Some(Some(session.target.clone()));
                                        }
                                    } else {
                                    if action_button_enabled(ui, !session.attached
                                        && inventory.capabilities.attach
                                        && browser.allows_target(&session.target),
                                        ActionButtonRole::Accent, "Connect").clicked() {
                                        action = Some(Action::Attach(session.target.clone()));
                                    }
                                    if action_button(ui, ActionButtonRole::Secondary, "Save as profile…").clicked() {
                                        browser.profile_target = Some(session.target.clone());
                                        browser.profile_name = session.name.clone();
                                    }
                                    }
                                });
                            });
                        }
                        if inventory.capabilities.profiles {
                            ui.separator();
                            ui.strong("Shared profiles");
                            if inventory.profiles.is_empty() {
                                ui.label("No profiles are enabled for this host.");
                            }
                            for profile in &inventory.profiles {
                                ui.push_id(profile.logical.as_str(), |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(&profile.name);
                                        if !selection_mode
                                            && action_button_enabled(
                                                ui,
                                                inventory.capabilities.ensure_profile
                                                    && inventory.capabilities.attach,
                                                ActionButtonRole::Accent,
                                                "Connect / start",
                                            )
                                            .clicked()
                                        {
                                            action = Some(Action::EnsureProfile {
                                                logical: profile.logical.clone(),
                                                revision: profile.revision,
                                                name: profile.name.clone(),
                                            });
                                        }
                                    });
                                });
                            }
                        }
                        if let Some(target) = &browser.profile_target {
                            ui.separator();
                            ui.label("Profile name");
                            ui.add(egui::TextEdit::singleline(&mut browser.profile_name).min_size(egui::vec2(0.0, 24.0)).char_limit(200));
                            ui.checkbox(&mut browser.profile_create, "Create session if missing");
                            ui.horizontal(|ui| {
                                if action_button(ui, ActionButtonRole::Secondary, "Cancel profile").clicked() {
                                    cancel_profile = true;
                                }
                                if action_button_enabled(ui, !browser.profile_name.trim().is_empty(),
                                    ActionButtonRole::Accent, "Save profile").clicked() {
                                    save = Some(target.clone());
                                }
                            });
                        }
                        let caps = inventory.capabilities;
                        if !selection_mode && browser.discovered && caps.create && caps.creation_allowed && caps.attach {
                            if !browser.new_session {
                                if action_button(ui, ActionButtonRole::Secondary, "+ New session").clicked() {
                                    browser.new_session = true;
                                    browser.session_name.clear();
                                }
                            } else {
                                ui.separator();
                                ui.label("Session name");
                                ui.add(egui::TextEdit::singleline(&mut browser.session_name).min_size(egui::vec2(0.0, 24.0)).char_limit(128));
                                ui.horizontal(|ui| {
                                    if action_button(ui, ActionButtonRole::Secondary, "Cancel new session").clicked() {
                                        browser.new_session = false;
                                    }
                                    if action_button_enabled(ui, valid_session_name(&browser.session_name),
                                        ActionButtonRole::Accent, "Create and connect").clicked() {
                                        browser.recipe.session_name = Some(browser.session_name.trim().to_owned());
                                        browser.recipe.lifetime = Lifetime::Persistent {};
                                        browser.recipe.behavior = ConnectionBehavior::AlwaysCreate;
                                        browser.creation_confirmed = true;
                                        quick_create = true;
                                        action = Some(Action::Create);
                                    }
                                });
                            }
                        } else if !selection_mode && !browser.discovered && browser.creation_mode().is_some() {
                            ui.separator();
                            ui.label(format!("Session: {}", browser.recipe.session_name()));
                            let custom_launch = browser.recipe.launch.shell.is_some()
                                || !browser.recipe.launch.args.is_empty()
                                || browser.recipe.launch.cwd.is_some()
                                || !matches!(browser.recipe.lifetime, Lifetime::Persistent {});
                            if custom_launch {
                                ui.monospace(format!("shell {:?}\narguments {:?}\ndirectory {:?}\nlifetime {:?}",
                                    browser.recipe.launch.shell, browser.recipe.launch.args,
                                    browser.recipe.launch.cwd, browser.recipe.lifetime));
                                ui.checkbox(&mut browser.creation_confirmed, "I reviewed this launch and session policy");
                            }
                            let supported = match browser.creation_mode() {
                                Some(CreationMode::Ensure) => caps.ensure,
                                Some(CreationMode::Create) => caps.create && caps.creation_allowed,
                                None => false,
                            } && caps.attach && (matches!(browser.recipe.lifetime, Lifetime::Persistent {}) || caps.temporary);
                            if action_button_enabled(ui, supported && (!custom_launch || browser.creation_confirmed),
                                ActionButtonRole::Accent, "Connect / retry original action").clicked() {
                                browser.creation_confirmed = true;
                                action = Some(Action::Create);
                            }
                            if !caps.creation_allowed { ui.small("Creation is off. Existing authorized sessions can still resume."); }
                        }
                    }
                });
            });
            if action_button(ui, ActionButtonRole::Secondary, "Cancel").clicked() { dismiss = true; }
        });
        if dismiss {
            let browser = self.host_browser.take().expect("visible browser");
            crate::screens::host_workspace::report_outcome(
                context,
                browser.tab,
                browser.request,
                "Connection cancelled. Any issued action is retained for explicit retry.",
            );
            crate::screens::report_host_recipe_outcome(
                context,
                browser.tab,
                browser.request,
                Err("Connection cancelled. Any issued action is retained for explicit retry."),
            );
            return;
        }
        if let Some(target) = selection {
            let browser = self.host_browser.as_mut().expect("visible browser");
            let Some(host) = crate::screens::host_workspace::remembered_hosts(context)
                .into_iter()
                .find(|host| host.reference == browser.recipe.host)
            else {
                browser.feedback = Some("Refresh sessions before selecting this host.".into());
                return;
            };
            self.state.dispatch(
                AppCommand::SelectHostForProfile {
                    tab_id: browser.tab,
                    request: browser.request,
                    host,
                    target,
                },
                context,
            );
            self.host_browser = None;
            return;
        }
        if cancel_profile {
            self.host_browser
                .as_mut()
                .expect("visible browser")
                .profile_target = None;
        }
        if quick_create {
            let browser = self.host_browser.as_mut().expect("visible browser");
            if let Some((original, _, _)) =
                self.state
                    .host_creation_intents
                    .iter()
                    .find(|(original, _, intent)| {
                        !intent.is_presented()
                            && original.host == browser.recipe.host
                            && original.session_name() == browser.recipe.session_name()
                            && original.launch == browser.recipe.launch
                            && original.lifetime == browser.recipe.lifetime
                            && matches!(original.behavior, ConnectionBehavior::AlwaysCreate)
                    })
            {
                browser.recipe = original.clone();
            }
        }
        if let Some(target) = save {
            let browser = self.host_browser.as_ref().expect("visible browser");
            let Some(session) = browser.inventory.as_ref().and_then(|inventory| {
                inventory
                    .sessions
                    .iter()
                    .find(|session| session.target == target)
            }) else {
                return;
            };
            let mut recipe = browser.recipe.clone();
            recipe.id = festerm_config::host_recipes::RecipeId::new();
            recipe.name = browser.profile_name.trim().to_owned();
            recipe.session_name = Some(session.name.clone());
            recipe.logical_session = target.logical;
            recipe.lifetime = session.lifetime;
            recipe.behavior = if browser.profile_create {
                ConnectionBehavior::ResumeOrCreate
            } else {
                ConnectionBehavior::AttachOnly {
                    generation: target.generation,
                }
            };
            let saved_id = recipe.id.clone();
            self.apply_host_recipe_command(festerm_config::host_recipes::MetadataCommand::Save(
                recipe,
            ));
            let saved = self
                .state
                .configuration()
                .host_recipes()
                .get(&saved_id)
                .is_some();
            let browser = self.host_browser.as_mut().expect("visible browser");
            browser.feedback = Some(
                if saved {
                    "Profile saved."
                } else {
                    "Profile could not be saved. Try again."
                }
                .into(),
            );
            if saved {
                browser.profile_target = None;
            }
        } else if let Some(action) = action {
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
        if let Action::EnsureProfile {
            logical,
            revision,
            name,
        } = &action
        {
            let selected = browser.inventory.as_ref().is_some_and(|inventory| {
                browser.host_approved
                    && inventory.capabilities.ensure_profile
                    && inventory.capabilities.attach
                    && inventory
                        .profiles
                        .iter()
                        .any(|profile| &profile.logical == logical && profile.revision == *revision)
            });
            if !selected {
                return;
            }
            let approved = match browser.context() {
                Ok(approved) => approved,
                Err(error) => {
                    browser.feedback = Some(error);
                    return;
                }
            };
            let Some(generation) = browser.generation.checked_add(1) else {
                browser.feedback = Some(
                    "The foreground operation generation is exhausted. Reopen this review.".into(),
                );
                return;
            };
            browser.generation = generation;
            browser.profile_label = Some(name.clone());
            let scope = browser.scope(&approved);
            let wake = context.clone();
            match Worker::start(
                approved,
                scope,
                host_client::Operation::EnsureProfile {
                    logical: logical.clone(),
                    revision: *revision,
                    attachment: host_client::AttachmentId::new(),
                },
                &self.state.host_client_budgets,
                Arc::new(move || wake.request_repaint()),
            ) {
                Ok(worker) => browser.worker = Some(worker),
                Err(error) => browser.feedback = Some(error.message().into()),
            }
            return;
        }
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
            let label = if browser.discovered {
                browser
                    .inventory
                    .as_ref()
                    .and_then(|inventory| {
                        inventory
                            .sessions
                            .iter()
                            .find(|session| session.target == target)
                    })
                    .map(|session| session.name.clone())
                    .expect("validated selected target")
            } else {
                browser.recipe.name.clone()
            };
            let attachment = browser.context().and_then(|approved| {
                let scope = browser.scope(&approved);
                ApprovedAttachment::new(approved, scope, target)
                    .map_err(|error| error.message().into())
            });
            match attachment {
                Ok(attachment) => {
                    let tab_id = browser.tab;
                    let request = browser.request;
                    crate::screens::host_workspace::set_connection_label(
                        context, tab_id, request, label,
                    );
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
            let candidate_host = if browser.unbound {
                if browser.prerequisite.len() > 4096 {
                    browser.feedback = Some("The host binding is too large.".into());
                    return;
                }
                match serde_json::from_str::<festerm_tunnel_bridge::owner_auth::OwnerBinding>(
                    &browser.prerequisite,
                ) {
                    Ok(binding) => binding.host.host,
                    Err(_) => {
                        browser.feedback =
                            Some("Paste the public owner binding from this host.".into());
                        return;
                    }
                }
            } else {
                browser.recipe.host.id.clone()
            };
            match ReviewedOwner::confirm(
                &browser.prerequisite,
                &candidate_host,
                &format!("github:{}", browser.account.account_id()),
                &browser.fingerprint,
            ) {
                Ok(reviewed) => {
                    browser.recipe.host.id = candidate_host;
                    browser.unbound = false;
                    Some(reviewed)
                }
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
