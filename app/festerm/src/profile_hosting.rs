//! Composition-owned desktop hosting. Config is intent; private consent is
//! authority, and background replies never change either.

#[path = "profile_hosting_account.rs"]
mod account;
#[path = "profile_hosting_stage.rs"]
mod staging;

use super::{screens, FesTermApp};
use crate::tabs::{LocalHostAction, TabId};
use eframe::egui;
use festerm_config::{Configuration, LocalTunnelHosting, PersistenceProviderKind, Profile};
use festerm_tunnel_bridge::{
    profile_hosting::{
        self, BindExistingRequest, Consent, Failure, HostedLaunch, ManagedHostStatus,
        ManagementOperation, ManagementReply, ManagementRequest, Origin, Phase, Store, Target,
    },
    setup_storage,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::mpsc,
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const QUEUE_LIMIT: usize = 64;

pub(super) struct LocalProfileEdit {
    pub original_id: Option<String>,
    pub profile: Profile,
    pub expose: bool,
    pub explicit_enable: bool,
    pub account_selection: Option<egui::Id>,
}

#[derive(Clone)]
enum Work {
    Ensure(Box<Profile>, usize),
    Restart(Box<Profile>, usize, Target),
    Remove(LocalTunnelHosting),
    Status(LocalTunnelHosting),
    Login(Box<Work>),
}

impl Work {
    fn is_removal(&self) -> bool {
        match self {
            Self::Remove(_) => true,
            Self::Login(next) => next.is_removal(),
            _ => false,
        }
    }

    fn same_operation(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ensure(..), Self::Ensure(..))
            | (Self::Restart(..), Self::Restart(..))
            | (Self::Remove(_), Self::Remove(_))
            | (Self::Status(_), Self::Status(_)) => true,
            (Self::Login(next), Self::Login(other)) => next.same_operation(other),
            _ => false,
        }
    }

    fn binding(&self) -> Option<&LocalTunnelHosting> {
        match self {
            Self::Ensure(profile, _) | Self::Restart(profile, ..) => binding(profile),
            Self::Login(next) => next.binding(),
            Self::Remove(binding) | Self::Status(binding) => Some(binding),
        }
    }
}

type Completion = (Result<ManagementReply, String>, Option<staging::Helpers>);

struct Pending {
    work: Work,
    receiver: mpsc::Receiver<Completion>,
    cancel: CancellationToken,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Default)]
pub(super) struct Controller {
    accounts: account::Chooser,
    enabled: bool,
    initialized: bool,
    queue: VecDeque<Work>,
    pending: Option<Pending>,
    helpers: Option<staging::Helpers>,
    rows: BTreeMap<String, ManagedHostStatus>,
    errors: BTreeMap<String, String>,
    refresh: Option<Instant>,
    published: Option<Instant>,
    confirmation: Option<(String, LocalTunnelHosting, LocalHostAction)>,
    cleanup_confirmation: Option<LocalTunnelHosting>,
    focus_confirmation: bool,
    inventory: LocalInventory,
    inventory_pending: Option<mpsc::Receiver<LocalInventory>>,
    inventory_refreshed: Option<Instant>,
    inventory_updated: Option<Instant>,
    cleanup_initialized: bool,
}

fn binding(profile: &Profile) -> Option<&LocalTunnelHosting> {
    profile.as_local()?.tunnel_hosting()
}

fn validate_hosted_names(
    configuration: &Configuration,
    original_id: Option<&str>,
    candidate: &Profile,
    expose: bool,
) -> Result<(), &'static str> {
    fn native_name(profile: &Profile) -> Option<&str> {
        profile
            .as_local()?
            .persistence()
            .filter(|persistence| {
                persistence.provider() == PersistenceProviderKind::FestermSessiond
            })
            .map(|persistence| persistence.session_name())
    }
    let Some(name) = native_name(candidate) else {
        return Ok(());
    };
    if configuration.profiles().iter().any(|other| {
        Some(other.identifier()) != original_id
            && native_name(other) == Some(name)
            && (expose || binding(other).is_some_and(LocalTunnelHosting::enabled))
    }) {
        return Err("Dev Tunnel hosting requires a unique native persistent session name. Rename the conflicting session before saving.");
    }
    Ok(())
}

fn store(binding: &LocalTunnelHosting) -> io::Result<Store> {
    Store::open(
        &profile_hosting::installation_path(binding.installation())?,
        binding.installation(),
        false,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GrantReadFailure {
    Missing,
    ForeignConsent,
    Unavailable,
}

fn grant(binding: &LocalTunnelHosting) -> Result<Consent, GrantReadFailure> {
    read_grant(binding, true)
}

fn grant_view(binding: &LocalTunnelHosting) -> Result<Consent, GrantReadFailure> {
    read_grant(binding, false)
}

fn read_grant(binding: &LocalTunnelHosting, wait: bool) -> Result<Consent, GrantReadFailure> {
    let grant = store(binding)
        .and_then(|store| if wait { store.read() } else { store.try_read() })
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                GrantReadFailure::Missing
            } else {
                GrantReadFailure::Unavailable
            }
        })?;
    if grant.consent != binding.consent() {
        return Err(GrantReadFailure::ForeignConsent);
    }
    Ok(grant)
}

fn revocation_grant(
    read: Result<Consent, GrantReadFailure>,
) -> Result<Option<Consent>, &'static str> {
    match read {
        Ok(grant) => Ok(Some(grant)),
        Err(GrantReadFailure::Missing | GrantReadFailure::ForeignConsent) => Ok(None),
        Err(GrantReadFailure::Unavailable) => Err("Private hosting state could not be verified. No profile change was saved; retry before disabling or deleting it."),
    }
}

fn fingerprint(profile: &Profile) -> Result<String, &'static str> {
    let local = profile.as_local().ok_or("Select a local profile.")?;
    let local = crate::environment::with_corrected_local_path(local.to_local_profile());
    festerm_sessiond::spawn_fingerprint(&local).map_err(|_| {
        "Check the executable, working directory and effective PATH before enabling hosting."
    })
}

fn selected(target: &Target) -> festerm_sessiond::NativeSessionIdentity {
    festerm_sessiond::NativeSessionIdentity {
        name: target.name.clone(),
        pid: target.pid,
        created_at_unix_ms: target.generation,
        endpoint: target.endpoint.clone(),
    }
}

fn deferred_restart(grant: &Consent) -> Option<&Target> {
    grant.hosted_launch.as_ref()?.legacy.as_ref()
}

fn legacy_session(name: &str) -> Result<Option<Target>, String> {
    let root = festerm_sessiond::runtime_root().map_err(|_| "profile_host.runtime_namespace")?;
    let sessions = festerm_sessiond::list_running_sessions_strict_in(&root)
        .map_err(|_| "profile_host.restart_identity")?;
    Ok(sessions
        .into_iter()
        .find(|running| running.session.name == name)
        .map(|running| {
            let session = running.session;
            Target {
                runtime_root: root,
                name: session.name,
                pid: session.pid,
                generation: session.created_at_unix_ms,
                endpoint: session.endpoint,
            }
        }))
}

pub(crate) fn connect_profile(
    profile: &Profile,
    size: festerm_session::TerminalSize,
    scrollback: usize,
    notifier: std::sync::Arc<dyn festerm_session::SessionEventNotifier>,
    allow_start: bool,
) -> Result<festerm_sessiond::PersistentSession, String> {
    let binding = binding(profile).ok_or("profile_host.binding")?;
    let local = profile.as_local().ok_or("profile_host.profile")?;
    let persistence = local.persistence().ok_or("profile_host.profile")?;
    let native = crate::environment::with_corrected_local_path(local.to_local_profile());
    let mut grant = match grant(binding) {
        Ok(grant) => grant,
        Err(GrantReadFailure::Missing | GrantReadFailure::ForeignConsent) => {
            return connect_unbound_profile(
                persistence.session_name(),
                &native,
                size,
                scrollback,
                notifier,
                allow_start,
                None,
            )
        }
        Err(GrantReadFailure::Unavailable) => {
            return Err(
                "Private hosting state is unavailable; no fallback shell was started.".into(),
            )
        }
    };
    let launch = match (
        &grant.hosted_launch,
        binding.enabled() && grant.phase.allows_hosting(),
    ) {
        (Some(launch), true) => launch,
        _ => {
            return connect_unbound_profile(
                persistence.session_name(),
                &native,
                size,
                scrollback,
                notifier,
                allow_start,
                grant
                    .target
                    .as_ref()
                    .map(|target| target.runtime_root.as_path()),
            )
        }
    };
    let root = launch.runtime_root.clone();
    let restart = deferred_restart(&grant).cloned();
    if root != profile_hosting::hosted_runtime().map_err(|_| "profile_host.runtime_namespace")?
        || fingerprint(profile)? != grant.spawn_fingerprint
    {
        return Err(
            "The hosted profile command or runtime changed; no fallback shell was started.".into(),
        );
    }
    if let Some(expected) = deferred_restart(&grant) {
        if let Some(current) = legacy_session(&grant.session_name)? {
            if &current != expected {
                return Err(
                    "The legacy session was replaced; no session was attached or restarted.".into(),
                );
            }
            return attach_target(&current, notifier);
        }
        if !allow_start {
            return Err(
                "Hosting is pending a manual session restart; restoration cannot start it.".into(),
            );
        }
        grant = store(binding)
            .and_then(|store| store.authorize_hosted_start(binding.consent(), expected))
            .map_err(|_| {
                "The old session has not definitively ended; no replacement was started."
            })?;
    }
    if !grant.phase.allows_hosting() {
        return Err("Hosting consent is not active; no replacement was started.".into());
    }
    if let Some(target) = &grant.target {
        return attach_target(target, notifier);
    }
    if grant.target.is_none() && restart.is_none() {
        if let Some(legacy) = legacy_session(&grant.session_name)? {
            store(binding)
                .and_then(|store| store.defer_hosted_start(binding.consent(), legacy))
                .map_err(|_| "profile_host.stale_consent")?;
            return Err("The existing session needs a restart. Edit the profile to restart it, or end it and open the profile again.".into());
        }
    }
    let target = if allow_start {
        let identity = festerm_sessiond::ensure_detached_in(
            &root,
            festerm_sessiond::DetachedSessionRequest {
                name: persistence.session_name(),
                profile: &native,
                size,
                scrollback_limit_bytes: scrollback,
                launch_request: binding.consent(),
            },
        )
        .map_err(|error| error.to_string())?;
        Target {
            runtime_root: root,
            name: identity.name,
            pid: identity.pid,
            generation: identity.created_at_unix_ms,
            endpoint: identity.endpoint,
        }
    } else {
        let running = festerm_sessiond::list_running_sessions_in(&root)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|running| running.session.name == persistence.session_name())
            .ok_or("No hosted session exists. Restoration never starts a replacement shell.")?;
        Target {
            runtime_root: root,
            name: running.session.name,
            pid: running.session.pid,
            generation: running.session.created_at_unix_ms,
            endpoint: running.session.endpoint,
        }
    };
    if allow_start {
        store(binding)
            .and_then(|store| match restart {
                Some(expected) => {
                    store.bind_restarted_target(binding.consent(), &expected, target.clone())
                }
                None => store.bind_target(binding.consent(), target.clone()),
            })
            .map_err(|_| "The hosting consent changed. The shell was retained without exposure.")?;
    }
    attach_target(&target, notifier)
}

fn attach_target(
    target: &Target,
    notifier: std::sync::Arc<dyn festerm_session::SessionEventNotifier>,
) -> Result<festerm_sessiond::PersistentSession, String> {
    let running = festerm_sessiond::list_running_sessions_in(&target.runtime_root)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|running| {
            let session = &running.session;
            session.name == target.name
                && session.pid == target.pid
                && session.created_at_unix_ms == target.generation
                && session.endpoint == target.endpoint
        })
        .ok_or(
            "The selected hosted generation is unavailable. No replacement shell was started.",
        )?;
    festerm_sessiond::PersistentSession::resume_discovered_in(
        &running.session,
        &target.runtime_root,
        notifier,
    )
    .map_err(|error| error.to_string())
}

fn connect_unbound_profile(
    name: &str,
    profile: &festerm_pty::LocalProfile,
    size: festerm_session::TerminalSize,
    scrollback: usize,
    notifier: std::sync::Arc<dyn festerm_session::SessionEventNotifier>,
    allow_start: bool,
    preferred: Option<&Path>,
) -> Result<festerm_sessiond::PersistentSession, String> {
    let hosted = profile_hosting::hosted_runtime().map_err(|_| "profile_host.runtime_namespace")?;
    let legacy = festerm_sessiond::runtime_root().map_err(|_| "profile_host.runtime_namespace")?;
    let mut roots = preferred
        .into_iter()
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    for root in [hosted, legacy] {
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    if let Some((root, session)) = unbound_existing_session_in(&roots, name)? {
        return festerm_sessiond::PersistentSession::resume_discovered_in(
            &session, &root, notifier,
        )
        .map_err(|error| error.to_string());
    }
    if allow_start {
        festerm_sessiond::PersistentSession::start_with_notifier(
            name, profile, size, scrollback, notifier,
        )
        .map_err(|error| error.to_string())
    } else {
        Err(
            "No existing session is available. Restoration never starts an unbound hosted profile."
                .into(),
        )
    }
}

fn unbound_existing_session_in(
    roots: &[PathBuf],
    name: &str,
) -> Result<Option<(PathBuf, festerm_sessiond::UnattachedSession)>, String> {
    for root in roots {
        if let Some(running) = festerm_sessiond::list_running_sessions_strict_in(root)
            .map_err(|error| error.to_string())?
            .into_iter()
            .find(|running| running.session.name == name)
        {
            return Ok(Some((root.clone(), running.session)));
        }
    }
    Ok(None)
}

impl Controller {
    pub(super) fn native() -> Self {
        Self {
            enabled: !cfg!(test),
            ..Self::default()
        }
    }

    fn enqueue(&mut self, work: Work) {
        let Some(binding) = work.binding() else {
            return;
        };
        let id = binding.installation().to_owned();
        if !work.is_removal()
            && self.queue.iter().any(|queued| {
                queued.is_removal()
                    && queued
                        .binding()
                        .is_some_and(|binding| binding.installation() == id)
            })
        {
            return;
        }
        if matches!(work, Work::Status(_))
            && self.queue.iter().any(|queued| {
                queued
                    .binding()
                    .is_some_and(|binding| binding.installation() == id)
            })
        {
            return;
        }
        self.published = None;
        if matches!(work, Work::Remove(_)) {
            if let Some(pending) = &self.pending {
                if pending
                    .work
                    .binding()
                    .is_some_and(|binding| binding.installation() == id)
                {
                    pending.cancel.cancel();
                }
            }
        } else if self.pending.as_ref().is_some_and(|pending| {
            pending
                .work
                .binding()
                .is_some_and(|pending| pending == binding)
                && pending.work.same_operation(&work)
        }) {
            return;
        }
        self.queue.retain(|queued| {
            queued
                .binding()
                .is_none_or(|binding| binding.installation() != id)
        });
        if self.queue.len() < QUEUE_LIMIT {
            self.errors.remove(&id);
            self.queue.push_back(work);
        } else {
            self.errors.insert(id, "profile_host.queue_full".into());
        }
    }

    /// Called before configuration mutation, including writes that later fail.
    pub(super) fn fence_changes(
        &mut self,
        before: &Configuration,
        after: &Configuration,
    ) -> Result<(), &'static str> {
        for previous in before.profiles() {
            let Some(old) = binding(previous) else {
                continue;
            };
            if after
                .profiles()
                .iter()
                .any(|new| preserves_binding(previous, new))
            {
                continue;
            }
            if let Some(grant) = revocation_grant(grant(old))? {
                store(old)
                    .and_then(|store| store.revoke(&grant.consent))
                    .map_err(|_| {
                        "Exposure could not be revoked. Retry before changing this profile."
                    })?;
                self.enqueue(Work::Remove(old.clone()));
            }
        }
        Ok(())
    }

    fn prepare(
        &mut self,
        profile: Profile,
        previous: Option<&Profile>,
        expose: bool,
        explicit_enable: bool,
        selected_account: Option<&str>,
    ) -> Result<(Profile, bool), &'static str> {
        if !self.enabled {
            return Err("Hosting is unavailable in this application context.");
        }
        if !expose {
            let metadata = previous
                .and_then(binding)
                .filter(|_| {
                    profile
                        .as_local()
                        .and_then(|local| local.persistence())
                        .is_some_and(|persistence| {
                            persistence.provider() == PersistenceProviderKind::FestermSessiond
                        })
                })
                .and_then(|old| {
                    LocalTunnelHosting::new(old.installation(), old.consent(), false).ok()
                });
            return profile
                .with_local_tunnel_hosting(metadata)
                .map(|profile| (profile, false))
                .map_err(|_| "Invalid local profile.");
        }
        let local = profile.as_local().ok_or("Select a local profile.")?;
        let persistence = local
            .persistence()
            .filter(|persistence| {
                persistence.provider() == PersistenceProviderKind::FestermSessiond
            })
            .ok_or("Dev Tunnel hosting requires the fesTerm session daemon.")?;
        let fingerprint = fingerprint(&profile)?;
        if let Some(existing) = previous.and_then(binding) {
            if let Ok(grant) = grant(existing) {
                if grant.phase.allows_hosting()
                    || matches!(grant.phase, Phase::Prepared | Phase::CloudMissing)
                {
                    if grant.spawn_fingerprint != fingerprint
                        || grant.session_name != persistence.session_name()
                    {
                        return Err("Disable exposure and Save before changing the command or persistent session name.");
                    }
                    if let Some(selected) = selected_account {
                        let recorded = grant.selected_account.as_deref().or_else(|| {
                            grant
                                .cloud_reservation
                                .as_ref()
                                .map(|reservation| reservation.account.as_str())
                        });
                        if recorded.is_some_and(|recorded| recorded != selected) {
                            return Err("This exposure belongs to its recorded GitHub account. Restore that sign-in or disable and remove it before choosing another account.");
                        }
                        if recorded.is_none() {
                            let _account = profile_hosting::account_lease(false)
                                .map_err(|_| "Account sign-in is busy. Retry shortly.")?;
                            store(existing)
                                .and_then(|store| {
                                    store.select_account(existing.consent(), selected)
                                })
                                .map_err(|_| {
                                    "The hosting account changed. Verify the account again."
                                })?;
                        }
                    }
                    return profile
                        .with_local_tunnel_hosting(Some(existing.clone()))
                        .map(|profile| (profile, grant.phase == Phase::Prepared))
                        .map_err(|_| "Invalid hosting metadata.");
                }
            }
        }
        if !explicit_enable {
            return Err("This exposure is not authorized on this host. Turn the checkbox off and on, then Save.");
        }
        staging::initialize().map_err(|_| "Private hosting storage is unavailable.")?;
        let account = selected_account.ok_or("Choose a GitHub account before enabling hosting.")?;
        let _account = profile_hosting::account_lease(false)
            .map_err(|_| "Account sign-in is busy. Retry shortly.")?;
        let installation = uuid::Uuid::new_v4().to_string();
        let consent = uuid::Uuid::new_v4().to_string();
        let state = profile_hosting::installation_path(&installation)
            .map_err(|_| "Private hosting storage is unavailable.")?;
        Store::open(&state, &installation, true)
            .and_then(|store| {
                store.prepare_hosted(
                    &consent,
                    &fingerprint,
                    persistence.session_name(),
                    HostedLaunch {
                        runtime_root: profile_hosting::hosted_runtime()?,
                        legacy: None,
                    },
                )?;
                store.select_account(&consent, account)
            })
            .map_err(|_| "The hosting consent could not be prepared.")?;
        let metadata = LocalTunnelHosting::new(installation, consent, true)
            .map_err(|_| "Invalid hosting metadata.")?;
        profile
            .with_local_tunnel_hosting(Some(metadata))
            .map(|profile| (profile, true))
            .map_err(|_| "Invalid hosting metadata.")
    }

    fn start_pending(&mut self, context: &egui::Context) {
        if self.pending.is_some() {
            return;
        }
        let Some(work) = self.queue.pop_front() else {
            return;
        };
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let context = context.clone();
        let helpers = self.helpers.clone();
        let worker_work = work.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = std::thread::Builder::new().name("profile-tunnel-hosting".into()).spawn(move || {
            let helpers = helpers.map(Ok).unwrap_or_else(staging::stage);
            let completion = match helpers {
                Ok(helpers) => {
                    let result = tokio::runtime::Builder::new_current_thread()
                        .enable_all().build()
                        .map_err(|_| "profile_host.runtime".to_owned())
                        .and_then(|runtime| runtime.block_on(async {
                            tokio::select! {
                                _ = worker_cancel.cancelled() => Err("profile_host.cancelled".into()),
                                _ = tokio::time::sleep(Duration::from_secs(360)) => Err("profile_host.timeout".into()),
                                result = execute(&helpers, worker_work, &worker_cancel) => result,
                            }
                        }));
                    (result, Some(helpers))
                }
                Err(error) => (Err(festerm_tunnel_bridge::worker_log::code(&error.to_string()).to_owned()), None),
            };
            let _ = sender.try_send(completion);
            context.request_repaint();
        });
        self.pending = Some(Pending {
            work,
            receiver,
            cancel,
        });
    }

    fn poll(&mut self, configuration: &Configuration, context: &egui::Context) {
        if !self.enabled {
            return;
        }
        if !self.initialized {
            self.initialized = true;
            for profile in configuration.profiles() {
                if binding(profile).is_some_and(|binding| {
                    binding.enabled()
                        && grant_view(binding).is_ok_and(|grant| {
                            grant.phase.allows_hosting() && deferred_restart(&grant).is_none()
                        })
                }) {
                    self.enqueue(Work::Ensure(
                        Box::new(profile.clone()),
                        configuration
                            .interface_settings()
                            .scrollback_limit()
                            .bytes(),
                    ));
                }
            }
        }
        self.poll_inventory(context);
        if let Some(pending) = &self.pending {
            let outcome = match pending.receiver.try_recv() {
                Ok(completion) => Some(completion),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some((Err("profile_host.worker_unavailable".into()), None))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some((result, helpers)) = outcome {
                self.published = None;
                let pending = self.pending.take().expect("pending completion");
                if let Some(helpers) = helpers {
                    self.helpers = Some(helpers);
                }
                if let Some(binding) = pending
                    .work
                    .binding()
                    .filter(|binding| !pending.cancel.is_cancelled() && grant_view(binding).is_ok())
                {
                    let id = binding.installation().to_owned();
                    match result {
                        Ok(mut reply) if reply.version == 1 && reply.hosts.len() == 1 => {
                            let row = reply.hosts.pop().expect("one host");
                            if row.origin == Origin::DesktopProfile
                                && row.installation == id
                                && row.consent.as_deref() == Some(binding.consent())
                                && grant_view(binding).is_ok()
                            {
                                if let Some(error) = reply.error {
                                    self.errors.insert(id.clone(), error);
                                }
                                self.rows.insert(id, row);
                            }
                        }
                        Err(error) => {
                            if error == "profile_host.restart_confirmation_required" {
                                if let Work::Ensure(profile, _) = &pending.work {
                                    self.confirmation = Some((
                                        profile.identifier().to_owned(),
                                        binding.clone(),
                                        LocalHostAction::Restart,
                                    ));
                                    self.focus_confirmation = true;
                                }
                            }
                            self.rows.remove(&id);
                            self.errors.insert(id, error);
                        }
                        _ => {
                            self.rows.remove(&id);
                            self.errors.insert(id, "profile_host.reply_unknown".into());
                        }
                    }
                }
            }
        }
        if self
            .refresh
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30))
        {
            self.refresh = Some(Instant::now());
            for metadata in configuration.profiles().iter().filter_map(binding) {
                if grant_view(metadata).is_ok_and(|grant| deferred_restart(&grant).is_none()) {
                    self.enqueue(Work::Status(metadata.clone()));
                }
            }
        }
        self.start_pending(context);
        self.publish(configuration, context);
        if self.pending.is_some() {
            context.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn queue_cleanup(&mut self) {
        for (id, grant) in self.inventory.entries.clone() {
            let Some(grant) = grant else { continue };
            if matches!(grant.phase, Phase::Revoking | Phase::CleanupPending) {
                if self.pending.as_ref().is_some_and(|pending| {
                    pending.work.is_removal()
                        && pending
                            .work
                            .binding()
                            .is_some_and(|binding| binding.installation() == id)
                }) {
                    continue;
                }
                if let Ok(binding) = LocalTunnelHosting::new(id, grant.consent, false) {
                    self.enqueue(Work::Remove(binding));
                }
            }
        }
    }

    fn poll_inventory(&mut self, context: &egui::Context) {
        if let Some(receiver) = &self.inventory_pending {
            match receiver.try_recv() {
                Ok(inventory) => {
                    self.inventory = inventory;
                    self.inventory_updated = Some(Instant::now());
                    self.inventory_pending = None;
                    self.published = None;
                    if !self.cleanup_initialized {
                        self.cleanup_initialized = true;
                        self.queue_cleanup();
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.inventory_pending = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.inventory_pending.is_none()
            && self
                .inventory_refreshed
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            self.inventory_refreshed = Some(Instant::now());
            let (sender, receiver) = mpsc::sync_channel(1);
            let context = context.clone();
            let _ = std::thread::Builder::new()
                .name("profile-tunnel-inventory".into())
                .spawn(move || {
                    let _ = sender.try_send(local_inventory());
                    context.request_repaint();
                });
            self.inventory_pending = Some(receiver);
        }
    }

    fn publish(&mut self, configuration: &Configuration, context: &egui::Context) {
        if self
            .published
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.published = Some(Instant::now());
        let mut views = BTreeMap::new();
        for profile in configuration.profiles() {
            let Some(binding) = binding(profile) else {
                continue;
            };
            let grant = match grant_view(binding) {
                Ok(grant) => grant,
                Err(failure) => {
                    views.insert(
                    profile.identifier().to_owned(),
                    screens::LocalHostView {
                        message: if failure == GrantReadFailure::Unavailable {
                            "Private hosting state is unavailable. Exposure is unknown, not disabled; retry when it can be verified."
                        } else {
                            "Not authorized on this host. Enable and Save to grant access."
                        }.into(),
                        ..Default::default()
                    },
                );
                    continue;
                }
            };
            let row = self
                .rows
                .get(binding.installation())
                .filter(|row| row.consent.as_deref() == Some(binding.consent()));
            let busy = self.pending.as_ref().is_some_and(|pending| {
                pending
                    .work
                    .binding()
                    .is_some_and(|pending| pending.installation() == binding.installation())
            });
            let mut view = screens::LocalHostView {
                enabled: binding.enabled()
                    && (grant.phase.allows_hosting()
                        || matches!(grant.phase, Phase::Prepared | Phase::CloudMissing)),
                busy,
                login_busy: busy
                    && self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| matches!(pending.work, Work::Login(_))),
                sign_in: grant.failure == Some(Failure::SignInRequired),
                recreate: grant.phase == Phase::CloudMissing,
                new_shell: grant.failure == Some(Failure::NewShellApprovalRequired),
                restart: deferred_restart(&grant).is_some(),
                message: phase_message(&grant).into(),
                ..Default::default()
            };
            if let Some(code) = self.errors.get(binding.installation()) {
                view.message = failure_message(code).into();
            }
            if grant.phase == Phase::Exposed {
                view.share = row
                    .filter(|row| row.phase == Phase::Exposed && row.failure.is_none())
                    .and_then(|row| row.destination.as_ref())
                    .and_then(|destination| serde_json::to_string_pretty(destination).ok());
                if view.share.is_none()
                    && grant.failure.is_none()
                    && !self.errors.contains_key(binding.installation())
                {
                    view.message =
                        "Checking service readiness. Connection details are not yet verified."
                            .into();
                }
            }
            views.insert(profile.identifier().to_owned(), view);
        }
        let mut cleanup = BTreeMap::new();
        for (installation, grant) in &self.inventory.entries {
            let Some(grant) = grant else {
                cleanup.insert(installation.clone(), screens::LocalHostView {
                    message: "Unknown local hosting state. Automatic deletion is refused; retain this record for diagnosis.".into(),
                    busy: true,
                    ..Default::default()
                });
                continue;
            };
            let unreferenced = unreferenced_active(configuration, installation, grant);
            if !unreferenced && !matches!(grant.phase, Phase::Revoking | Phase::CleanupPending) {
                continue;
            }
            let busy = self.pending.as_ref().is_some_and(|pending| {
                pending
                    .work
                    .binding()
                    .is_some_and(|binding| binding.installation() == installation)
            });
            cleanup.insert(
                installation.clone(),
                screens::LocalHostView {
                    message: if unreferenced {
                        format!("May belong to another fesTerm configuration. {} No adoption or teardown occurs without explicit confirmation.", phase_message(grant))
                    } else { self
                        .errors
                        .get(installation)
                        .map(|code| failure_message(code))
                        .unwrap_or_else(|| phase_message(grant))
                        .into() },
                    busy,
                    login_busy: busy
                        && self
                            .pending
                            .as_ref()
                            .is_some_and(|pending| matches!(pending.work, Work::Login(_))),
                    sign_in: !unreferenced && grant.failure == Some(Failure::SignInRequired),
                    unreferenced,
                    ..Default::default()
                },
            );
        }
        if self.inventory.truncated {
            cleanup.insert("Inventory truncated".into(), screens::LocalHostView {
                message: "Inventory incomplete: at most 1000 local records are displayed, and busy storage may shorten the scan. Other records were not inspected or modified.".into(),
                busy: true,
                ..Default::default()
            });
        }
        if self
            .inventory_updated
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(30))
        {
            cleanup.insert("Inventory not refreshed".into(), screens::LocalHostView {
                message: "Local inventory has not been refreshed yet; cached rows may be stale. Destructive actions revalidate protected ownership rather than trusting this display.".into(),
                busy: true,
                ..Default::default()
            });
        }
        screens::set_local_host_views(context, views, cleanup);
    }
}

#[derive(Default)]
struct LocalInventory {
    entries: Vec<(String, Option<Consent>)>,
    truncated: bool,
}

impl LocalInventory {
    fn cloud_grant(&self, locator: &str) -> Result<Option<(&str, &Consent)>, &'static str> {
        for (installation, grant) in &self.entries {
            if let Some(grant) = grant
                .as_ref()
                .filter(|grant| cloud_candidate(grant, locator))
            {
                return Ok(Some((installation, grant)));
            }
        }
        if self.truncated || self.entries.iter().any(|(_, grant)| grant.is_none()) {
            return Err("Local tunnel ownership could not be fully verified; deletion was refused. Resolve the unknown local hosting records first.");
        }
        Ok(None)
    }
}

fn local_inventory() -> LocalInventory {
    let Ok(namespace) = profile_hosting::namespace() else {
        return LocalInventory {
            truncated: true,
            ..Default::default()
        };
    };
    let entries = match std::fs::read_dir(namespace.join("installations")) {
        Ok(entries) => entries,
        Err(error) => {
            return LocalInventory {
                truncated: error.kind() != io::ErrorKind::NotFound,
                ..Default::default()
            }
        }
    };
    let mut inventory = LocalInventory::default();
    let started = Instant::now();
    for (index, entry) in entries.enumerate() {
        if index == 1000 || started.elapsed() >= Duration::from_secs(5) {
            inventory.truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            inventory.truncated = true;
            continue;
        };
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            inventory.truncated = true;
            continue;
        };
        let Ok(path) = profile_hosting::installation_path(&id) else {
            inventory.truncated = true;
            continue;
        };
        let grant = Store::open(&path, &id, false)
            .and_then(|store| store.read())
            .ok();
        inventory.entries.push((id, grant));
    }
    inventory
}

fn references_grant(configuration: &Configuration, installation: &str, consent: &str) -> bool {
    configuration
        .profiles()
        .iter()
        .filter_map(binding)
        .any(|binding| binding.installation() == installation && binding.consent() == consent)
}

fn unreferenced_active(configuration: &Configuration, installation: &str, grant: &Consent) -> bool {
    (grant.phase.allows_hosting() || grant.phase == Phase::Prepared)
        && !references_grant(configuration, installation, &grant.consent)
}

fn cloud_candidate(grant: &Consent, locator: &str) -> bool {
    let Some((name, cluster)) = locator.split_once('.') else {
        return false;
    };
    if cluster.is_empty() || cluster.contains('.') {
        return false;
    }
    match &grant.cloud {
        Some(cloud) => cloud.qualified_id == locator,
        None => grant
            .cloud_reservation
            .as_ref()
            .is_some_and(|reservation| name == reservation.name),
    }
}

fn preserves_binding(previous: &Profile, next: &Profile) -> bool {
    let (Some(previous), Some(next)) = (previous.as_local(), next.as_local()) else {
        return false;
    };
    previous.tunnel_hosting() == next.tunnel_hosting()
        && previous.executable() == next.executable()
        && previous.arguments() == next.arguments()
        && previous.working_directory() == next.working_directory()
        && previous.persistence() == next.persistence()
}

fn phase_message(grant: &Consent) -> &'static str {
    if grant.creation_pending && grant.creation_submitted {
        return "Cloud creation was submitted but is unconfirmed. Retry only reconciles this reservation. Disable/Save and explicitly re-enable to request a separate tunnel; this old reservation stays tracked for cleanup.";
    }
    match grant.failure {
        Some(Failure::SignInRequired) => return "Sign-in required. Use the recorded GitHub account; no credentials are stored in the profile.",
        Some(Failure::NewShellApprovalRequired) => return "New shell approval required. Retry cannot replace the recorded persistent shell.",
        Some(Failure::CommandMismatch) => return "The persistent command could not be verified. No running shell was replaced.",
        Some(Failure::CreationUncertain) => return "Creation outcome uncertain. Cleanup remains tracked; Retry only looks for the reserved owned tunnel.",
        Some(Failure::HelperUnavailable) => return "Hosting helpers are unavailable. Install a complete signed fesTerm build, then Retry.",
        Some(Failure::RestartRequired) => return "Hosting is pending a session restart. The current shell is unchanged. Restart it explicitly here, or end it and open this profile again; Retry never restarts it.",
        _ => {}
    }
    match grant.phase {
        Phase::Prepared => "Prepared, not exposed. Save again to authorize starting.",
        Phase::Requested | Phase::Provisioning => "Starting. Connection details appear only after verified readiness.",
        Phase::Exposed => "Exposed. Closing the tab or app does not disable this service.",
        Phase::Revoking => "Stopping exposure. The persistent shell is retained.",
        Phase::CleanupPending => "Local exposure stopped; cloud cleanup is pending. Sign in with the recorded account and Retry.",
        Phase::Revoked => "Exposure revoked. A new explicit enable is required.",
        Phase::CloudMissing => "The recorded cloud tunnel is missing. Recreate requires separate approval and never starts another shell.",
    }
}

fn failure_message(code: &str) -> &'static str {
    match code {
        "profile_host.restart_required" | "profile_host.restart_confirmation_required" =>
            "Hosting is pending a session restart. The current shell is unchanged. Restart it explicitly here, or end it and open this profile again; Retry never restarts it.",
        "profile_host.cancelled" => "Sign-in or the pending operation was cancelled. Saved consent was not renewed.",
        "profile_host.new_shell_approval_required" => "New shell approval required. Retry cannot replace a dead or uncertain shell.",
        "profile_host.signature_required" | "profile_host.package_location" => "Install a complete signed fesTerm build with its tunnel helpers, then Retry.",
        "profile_host.archive_pin_mismatch" | "profile_host.cli_pin_mismatch" | "profile_host.cached_helper_mismatch" => "Hosting helper verification failed. Existing services were not replaced; reinstall the reviewed release.",
        "profile_host.download_failed" | "profile_host.download_unavailable" => "The pinned Dev Tunnel CLI could not be downloaded. Check connectivity, then Retry.",
        "profile_host.command_mismatch" => "The exact persistent command could not be verified. Disable exposure before changing its command or session name.",
        _ => "Hosting could not complete. The protected operation is retained; check the saved profile and Retry. Cleanup may require browser sign-in.",
    }
}

async fn capture(
    mut command: tokio::process::Command,
    deadline: Duration,
) -> Result<Zeroizing<Vec<u8>>, String> {
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .map_err(|_| "profile_host.helper_unavailable".to_owned())?;
    capture_child(&mut child, deadline).await
}

async fn capture_child(
    child: &mut tokio::process::Child,
    deadline: Duration,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let stdout = child.stdout.take().ok_or("profile_host.helper_output")?;
    let stderr = child.stderr.take().ok_or("profile_host.helper_output")?;
    async fn drain(reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Zeroizing<Vec<u8>>> {
        let mut bytes = Zeroizing::new(Vec::with_capacity(1024 * 1024 + 1));
        reader.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("profile_host.helper_output"));
        }
        Ok(bytes)
    }
    let result = tokio::time::timeout(deadline, async {
        let (stdout, _stderr, status) =
            tokio::try_join!(drain(stdout), drain(stderr), child.wait())?;
        if !status.success() {
            return Err(io::Error::other("profile_host.helper_failed"));
        }
        Ok(stdout)
    })
    .await;
    result
        .map_err(|_| "profile_host.timeout".to_owned())?
        .map_err(|_| "profile_host.helper_failed".to_owned())
}

async fn execute(
    helpers: &staging::Helpers,
    work: Work,
    cancel: &CancellationToken,
) -> Result<ManagementReply, String> {
    execute_in(helpers, work, cancel, None).await
}

async fn execute_in(
    helpers: &staging::Helpers,
    work: Work,
    cancel: &CancellationToken,
    restart: Option<Target>,
) -> Result<ManagementReply, String> {
    let operation = match work {
        Work::Restart(profile, scrollback, expected) => {
            let binding = binding(&profile).ok_or("profile_host.binding")?;
            let store = store(binding).map_err(|_| "profile_host.binding")?;
            let grant = grant(binding).map_err(|_| "profile_host.binding")?;
            if !grant.phase.allows_hosting()
                || deferred_restart(&grant) != Some(&expected)
                || fingerprint(&profile).map_err(|_| "profile_host.command_mismatch")?
                    != grant.spawn_fingerprint
                || cancel.is_cancelled()
            {
                return Err("profile_host.stale_consent".into());
            }
            let checked = request_management(
                helpers,
                ManagementOperation::CheckRestartTarget {
                    target: expected.clone(),
                },
                cancel,
            )
            .await?;
            if checked.error.is_some() {
                return Err("The selected session is already hosted, or hosting ownership could not be verified. No session was stopped.".into());
            }
            if festerm_sessiond::process_alive(expected.pid) {
                let command = festerm_sessiond::terminate_selected_session_command_in(
                    &expected.runtime_root,
                    &selected(&expected),
                )
                .map_err(|_| "profile_host.restart_identity")?;
                let mut command = tokio::process::Command::from(command);
                command
                    .kill_on_drop(true)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                #[cfg(windows)]
                command.creation_flags(0x0800_0000);
                let mut child = store
                    .with_restart_permit(binding.consent(), &expected, || {
                        if cancel.is_cancelled() {
                            return Err(io::Error::other("profile_host.cancelled"));
                        }
                        command.spawn()
                    })
                    .map_err(|_| "profile_host.stale_consent")?;
                capture_child(&mut child, Duration::from_secs(15)).await?;
                let deadline = Instant::now() + Duration::from_secs(5);
                while festerm_sessiond::process_alive(expected.pid) {
                    if cancel.is_cancelled() || Instant::now() >= deadline {
                        return Err("profile_host.restart_required".into());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            store
                .authorize_hosted_start(binding.consent(), &expected)
                .map_err(|_| "profile_host.restart_required")?;
            return Box::pin(execute_in(
                helpers,
                Work::Ensure(profile, scrollback),
                cancel,
                Some(expected),
            ))
            .await;
        }
        Work::Login(next) => {
            let _account =
                profile_hosting::account_lease(true).map_err(|_| "profile_host.account_busy")?;
            let binding = next.binding().ok_or("profile_host.binding")?;
            if grant(binding).map_err(|_| "profile_host.binding")?.failure
                != Some(Failure::SignInRequired)
            {
                return Err("profile_host.sign_in_not_required".into());
            }
            let verified = festerm_tunnel_bridge::cli_pins::verify(&helpers.cli)
                .map_err(|_| "profile_host.cli_pin_mismatch")?;
            let mut command = tokio::process::Command::new(verified.path());
            command.args(["user", "login", "--github", "--use-browser-auth"]);
            let _output = capture(command, Duration::from_secs(300)).await?;
            drop(_account);
            return Box::pin(execute(helpers, *next, cancel)).await;
        }
        Work::Ensure(profile, scrollback) => {
            let binding = binding(&profile).ok_or("profile_host.binding")?;
            let store = store(binding).map_err(|_| "profile_host.binding")?;
            let grant = grant(binding).map_err(|_| "profile_host.binding")?;
            if !grant.phase.allows_hosting() {
                return Err("profile_host.consent_not_active".into());
            }
            if deferred_restart(&grant) != restart.as_ref() {
                return Err("profile_host.restart_required".into());
            }
            if let Some(expected) = &restart {
                store
                    .authorize_hosted_start(binding.consent(), expected)
                    .map_err(|_| "profile_host.restart_required")?;
            }
            let local = profile.as_local().ok_or("profile_host.profile")?;
            let persistence = local.persistence().ok_or("profile_host.profile")?;
            if persistence.session_name() != grant.session_name
                || fingerprint(&profile).map_err(|_| "profile_host.command_mismatch")?
                    != grant.spawn_fingerprint
            {
                let _ = store.report_failure(binding.consent(), Failure::CommandMismatch);
                return Err("profile_host.command_mismatch".into());
            }
            let native_profile =
                crate::environment::with_corrected_local_path(local.to_local_profile());
            let target = if let Some(target) = grant.target {
                if festerm_sessiond::session_correlation_in(
                    &target.runtime_root,
                    &selected(&target),
                )
                .is_err()
                {
                    let _ =
                        store.report_failure(binding.consent(), Failure::NewShellApprovalRequired);
                    return Err("profile_host.new_shell_approval_required".into());
                }
                target
            } else {
                let root = grant
                    .hosted_launch
                    .as_ref()
                    .map(|launch| Ok(launch.runtime_root.clone()))
                    .unwrap_or_else(festerm_sessiond::runtime_root)
                    .map_err(|_| "profile_host.runtime_namespace")?;
                if grant.hosted_launch.is_some() && restart.is_none() {
                    if let Some(legacy) = legacy_session(&grant.session_name)? {
                        store
                            .defer_hosted_start(binding.consent(), legacy)
                            .map_err(|_| "profile_host.stale_consent")?;
                        return Err("profile_host.restart_confirmation_required".into());
                    }
                }
                let identity = festerm_sessiond::ensure_detached_in(
                    &root,
                    festerm_sessiond::DetachedSessionRequest {
                        name: persistence.session_name(),
                        profile: &native_profile,
                        size: festerm_session::TerminalSize::new(80, 24)
                            .expect("default terminal dimensions"),
                        scrollback_limit_bytes: scrollback,
                        launch_request: binding.consent(),
                    },
                )
                .map_err(|_| {
                    let _ =
                        store.report_failure(binding.consent(), Failure::NewShellApprovalRequired);
                    "profile_host.new_shell_approval_required"
                })?;
                Target {
                    runtime_root: root,
                    name: identity.name,
                    pid: identity.pid,
                    generation: identity.created_at_unix_ms,
                    endpoint: identity.endpoint,
                }
            };
            match restart {
                Some(expected) => {
                    store.bind_restarted_target(binding.consent(), &expected, target.clone())
                }
                None => store.bind_target(binding.consent(), target.clone()),
            }
            .map_err(|_| "profile_host.stale_consent")?;
            ManagementOperation::BindExisting(Box::new(BindExistingRequest {
                installation: binding.installation().to_owned(),
                consent: binding.consent().to_owned(),
                cli: helpers.cli.clone(),
                target,
                spawn_fingerprint: grant.spawn_fingerprint,
                description: profile.identifier().chars().take(100).collect(),
                upgrade: true,
            }))
        }
        Work::Remove(binding) => ManagementOperation::Remove {
            installation: binding.installation().to_owned(),
            consent: Some(binding.consent().to_owned()),
        },
        Work::Status(binding) => ManagementOperation::Status {
            installation: binding.installation().to_owned(),
        },
    };
    request_management(helpers, operation, cancel).await
}

async fn request_management(
    helpers: &staging::Helpers,
    operation: ManagementOperation,
    cancel: &CancellationToken,
) -> Result<ManagementReply, String> {
    if cancel.is_cancelled() {
        return Err("profile_host.cancelled".into());
    }
    let root = profile_hosting::namespace()
        .map_err(|_| "profile_host.runtime_namespace")?
        .join("requests");
    let directory = setup_storage::private_directory(&root, false)
        .map_err(|_| "profile_host.request_storage")?;
    let name = format!("{}.json", uuid::Uuid::new_v4());
    let request = ManagementRequest {
        version: 1,
        operation,
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| "profile_host.request_encode")?;
    if bytes.len() > 64 * 1024 {
        return Err("profile_host.request_size".into());
    }
    setup_storage::replace_private(&directory, Path::new(&name), &bytes)
        .map_err(|_| "profile_host.request_storage")?;
    struct RequestFile(PathBuf);
    impl Drop for RequestFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let path = RequestFile(root.join(name));
    let mut command = tokio::process::Command::new(&helpers.setup);
    command
        .arg("--profile-host")
        .arg("--request-json")
        .arg(&path.0);
    let output = capture(command, Duration::from_secs(180)).await;
    serde_json::from_slice(&output?).map_err(|_| "profile_host.reply_unknown".into())
}

pub(super) fn revoke_cloud_browser_host(
    account: u64,
    locator: &str,
    cancel: &CancellationToken,
) -> Result<Option<LocalTunnelHosting>, &'static str> {
    let inventory = local_inventory();
    if let Some((installation, grant)) = inventory.cloud_grant(locator)? {
        if grant
            .cloud_reservation
            .as_ref()
            .is_none_or(|reservation| reservation.account != account.to_string())
        {
            return Err("The local host's recorded account does not match this browser. Use its recorded account to clean it up.");
        }
        let binding = LocalTunnelHosting::new(installation, &grant.consent, false)
            .map_err(|_| "The local hosting record could not be verified.")?;
        if cancel.is_cancelled() {
            return Err("Deletion was cancelled before local revocation.");
        }
        store(&binding)
            .and_then(|store| store.revoke(binding.consent()))
            .map_err(|_| "Local exposure could not be revoked; deletion was refused.")?;
        return Ok(Some(binding));
    }
    Ok(None)
}

impl FesTermApp {
    pub(super) fn queue_cloud_browser_host_removal(&mut self, binding: LocalTunnelHosting) {
        self.local_hosting.enqueue(Work::Remove(binding));
        self.secure_storage_feedback = Some("Local exposure revoked; owned teardown is queued. The persistent shell is retained. Pending cleanup is available in Profiles.");
    }

    pub(super) fn save_local_hosted_profile(
        &mut self,
        tab_id: TabId,
        request: egui::Id,
        edit: LocalProfileEdit,
        context: &egui::Context,
    ) {
        let LocalProfileEdit {
            original_id,
            profile,
            expose,
            explicit_enable,
            account_selection,
        } = edit;
        let previous = original_id
            .as_deref()
            .and_then(|id| self.state.configuration().profile(id))
            .cloned();
        let plan = validate_hosted_names(
            self.state.configuration(),
            original_id.as_deref(),
            &profile,
            expose,
        )
        .and_then(|_| {
            let account = if expose && explicit_enable {
                Some(self.local_hosting.accounts.take(
                    context,
                    tab_id,
                    account_selection,
                    original_id.as_deref(),
                )?)
            } else {
                None
            };
            self.local_hosting.prepare(
                profile,
                previous.as_ref(),
                expose,
                explicit_enable,
                account.as_deref(),
            )
        });
        let outcome = match plan {
            Err(message) => Err(message.to_owned()),
            Ok((profile, prepared)) => {
                let replacement = self
                    .state
                    .configuration()
                    .with_edited_profile(original_id.as_deref(), profile.clone());
                let saved = self.apply_configuration_save(
                    replacement,
                    super::ConfigurationStartupStatus::ProfileSaveFailure,
                    super::ConfigurationReloader::save_profile,
                );
                if saved {
                    let commit = binding(&profile)
                        .map(|binding| {
                            if prepared {
                                store(binding)
                                    .and_then(|store| store.commit_requested(binding.consent()))
                                    .map(|_| ())
                            } else {
                                Ok(())
                            }
                        })
                        .unwrap_or(Ok(()));
                    if commit.is_ok() {
                        if expose {
                            self.local_hosting.enqueue(Work::Ensure(
                                Box::new(profile),
                                self.state.scrollback_limit().bytes(),
                            ));
                        }
                        Ok(())
                    } else {
                        Err("The profile was saved, but exposure is not authorized yet. Save again to complete the request.".into())
                    }
                } else {
                    if prepared {
                        if let Some(binding) = binding(&profile) {
                            let _ = store(binding)
                                .and_then(|store| store.cancel_prepared(binding.consent()));
                        }
                    }
                    Err(self.configuration_status.settings_message().to_owned())
                }
            }
        };
        screens::report_local_profile_save_outcome(context, tab_id, request, outcome);
    }

    pub(super) fn poll_local_hosts(&mut self, context: &egui::Context) {
        let selecting_account = self
            .local_hosting
            .accounts
            .poll(context, self.state.active());
        if let Some(profile) = self.state.take_completed_hosted_profile() {
            if self.state.configuration().profile(profile.identifier()) == Some(&profile)
                && binding(&profile).is_some_and(LocalTunnelHosting::enabled)
            {
                self.local_hosting.enqueue(Work::Ensure(
                    Box::new(profile),
                    self.state.scrollback_limit().bytes(),
                ));
            }
        }
        self.local_hosting.poll(self.state.configuration(), context);
        if selecting_account {
            return;
        }
        let focus_cancel = std::mem::take(&mut self.local_hosting.focus_confirmation);
        if let Some(binding) = self.local_hosting.cleanup_confirmation.clone() {
            let mut confirmed = false;
            let mut cancelled = context.input(|input| input.key_pressed(egui::Key::Escape));
            egui::Modal::new(egui::Id::new("local_host_cleanup_confirmation"))
                .show(context, |ui| {
                    super::confirmations::prepare_confirmation(ui, context, 480.0);
                    ui.heading("Revoke unreferenced local exposure?");
                    ui.label("May belong to another fesTerm configuration. This stops local exposure and removes only its verified owned cloud tunnel. The persistent shell is kept.");
                    ui.label(binding.installation());
                    ui.horizontal(|ui| {
                        confirmed = ui.button("Revoke and remove").clicked();
                        let cancel = ui.button("Cancel");
                        if focus_cancel { cancel.request_focus(); }
                        cancelled |= cancel.clicked();
                    });
                });
            if cancelled || confirmed {
                self.local_hosting.cleanup_confirmation = None;
            }
            if confirmed {
                let result = if references_grant(
                    self.state.configuration(),
                    binding.installation(),
                    binding.consent(),
                ) {
                    Err("A profile now references this grant. Disable and Save that profile instead.")
                } else {
                    grant(&binding).map_err(|_| "The hosting grant changed or is unavailable; nothing was removed.")
                        .and_then(|_| store(&binding).and_then(|store| store.revoke(binding.consent())).map_err(|_| "The hosting grant changed or could not be revoked; nothing was removed."))
                        .map(|_| self.local_hosting.enqueue(Work::Remove(binding)))
                };
                if let Err(message) = result {
                    self.secure_storage_feedback = Some(message);
                }
            }
            return;
        }
        let Some((profile_id, binding, action)) = self.local_hosting.confirmation.clone() else {
            return;
        };
        let mut confirmed = false;
        let mut cancelled = context.input(|input| input.key_pressed(egui::Key::Escape));
        let new_shell = action == LocalHostAction::NewShell;
        let restart = action == LocalHostAction::Restart;
        egui::Modal::new(egui::Id::new("local_host_confirmation"))
            .show(context, |ui| {
                super::confirmations::prepare_confirmation(ui, context, 480.0);
                ui.heading(if restart { "Restart this session to enable Dev Tunnel?" } else if new_shell { "Start a new persistent shell?" } else { "Recreate the missing tunnel?" });
                ui.label(if restart {
                    "Enabling hosting requires restarting this persistent session. Yes stops only the selected session and starts the configured command; running programs and terminal state are lost, and any attached local tabs or remote clients disconnect. No keeps it running and saves hosting as pending until you end the session and open this profile again. Other sessions are not restarted."
                } else if new_shell {
                    "This starts a new configured persistent shell and exposes it. Old programs and terminal state are not recovered. No live daemon is terminated."
                } else {
                    "This creates a new cloud tunnel and connection details for the exact recorded persistent shell. It cannot start or replace that shell."
                });
                ui.horizontal(|ui| {
                    confirmed = ui.button(if restart { "Yes, restart and enable" } else if new_shell { "Start new shell and expose" } else { "Recreate tunnel" }).clicked();
                    let cancel = ui.button(if restart { "No, keep running" } else { "Cancel" });
                    if focus_cancel { cancel.request_focus(); }
                    cancelled |= cancel.clicked();
                });
            });
        if cancelled || confirmed {
            self.local_hosting.confirmation = None;
        }
        if confirmed {
            if let Err(message) = self.confirm_local_host_action(&profile_id, &binding, action) {
                self.secure_storage_feedback = Some(message);
            }
        }
    }

    pub(super) fn begin_local_host_account(
        &mut self,
        tab: TabId,
        request: egui::Id,
        context: &egui::Context,
    ) {
        self.local_hosting.accounts.begin(context, tab, request);
    }

    fn confirm_local_host_action(
        &mut self,
        profile_id: &str,
        expected: &LocalTunnelHosting,
        action: LocalHostAction,
    ) -> Result<(), &'static str> {
        let profile = self
            .state
            .configuration()
            .profile(profile_id)
            .cloned()
            .ok_or("The profile changed. Open it again.")?;
        let binding = binding(&profile)
            .ok_or("The hosting binding changed. Open the profile again.")?
            .clone();
        if &binding != expected {
            return Err(
                "The hosting binding changed. Review the current profile before confirming.",
            );
        }
        let grant =
            grant(&binding).map_err(|_| "The hosting consent changed. Open the profile again.")?;
        let store = store(&binding).map_err(|_| "Private hosting storage is unavailable.")?;
        match action {
            LocalHostAction::Restart => {
                let expected = deferred_restart(&grant)
                    .ok_or("The pending restart changed. Open the profile again.")?
                    .clone();
                self.local_hosting.enqueue(Work::Restart(
                    Box::new(profile),
                    self.state.scrollback_limit().bytes(),
                    expected,
                ));
            }
            LocalHostAction::RecreateMissing => {
                store.request_recreation(binding.consent()).map_err(|_| {
                    "The tunnel is not definitively missing, or its consent was revoked."
                })?;
                self.local_hosting.enqueue(Work::Ensure(
                    Box::new(profile),
                    self.state.scrollback_limit().bytes(),
                ));
            }
            LocalHostAction::NewShell => {
                if grant.target.as_ref().is_some_and(|target| {
                    festerm_sessiond::session_correlation_in(
                        &target.runtime_root,
                        &selected(target),
                    )
                    .is_ok()
                }) {
                    return Err("The recorded shell is still live. It was not replaced.");
                }
                let fresh = uuid::Uuid::new_v4().to_string();
                store.prepare_new_shell(binding.consent(), &fresh)
                    .map_err(|_| "A fresh shell cannot be approved in this hosting state. Resolve pending creation or cleanup first.")?;
                let metadata = LocalTunnelHosting::new(binding.installation(), fresh, true)
                    .map_err(|_| "Invalid hosting consent.")?;
                let profile = profile
                    .with_local_tunnel_hosting(Some(metadata.clone()))
                    .map_err(|_| "Invalid hosting profile.")?;
                if !self.apply_configuration_save(
                    self.state.configuration().with_profile(profile.clone()),
                    super::ConfigurationStartupStatus::ProfileSaveFailure,
                    super::ConfigurationReloader::save_profile,
                ) {
                    let _ = store.revoke(metadata.consent());
                    self.local_hosting.enqueue(Work::Remove(metadata));
                    return Err("The profile could not be saved. Exposure remains revoked; the old shell was not terminated.");
                }
                store.commit_requested(metadata.consent()).map_err(|_| {
                    "The profile was saved, but another explicit Save is required before exposure."
                })?;
                self.local_hosting.enqueue(Work::Ensure(
                    Box::new(profile),
                    self.state.scrollback_limit().bytes(),
                ));
            }
            _ => return Err("This hosting action requires a different confirmation."),
        }
        Ok(())
    }

    pub(super) fn local_host_cleanup_action(
        &mut self,
        installation: &str,
        action: LocalHostAction,
    ) {
        let Ok(path) = profile_hosting::installation_path(installation) else {
            return;
        };
        let Ok(grant) = Store::open(&path, installation, false).and_then(|store| store.read())
        else {
            return;
        };
        if action == LocalHostAction::RevokeUnreferenced {
            if unreferenced_active(self.state.configuration(), installation, &grant) {
                self.local_hosting.cleanup_confirmation =
                    LocalTunnelHosting::new(installation, grant.consent, false).ok();
                self.local_hosting.focus_confirmation = true;
            }
            return;
        }
        if !matches!(grant.phase, Phase::Revoking | Phase::CleanupPending) {
            return;
        }
        let Ok(binding) = LocalTunnelHosting::new(installation, grant.consent, false) else {
            return;
        };
        match action {
            LocalHostAction::Retry => self.local_hosting.enqueue(Work::Remove(binding)),
            LocalHostAction::SignIn => self
                .local_hosting
                .enqueue(Work::Login(Box::new(Work::Remove(binding)))),
            LocalHostAction::CancelSignIn => {
                if let Some(pending) = &self.local_hosting.pending {
                    if matches!(pending.work, Work::Login(_))
                        && pending
                            .work
                            .binding()
                            .is_some_and(|pending| pending.installation() == installation)
                    {
                        pending.cancel.cancel();
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn local_host_action(&mut self, profile_id: &str, action: LocalHostAction) {
        if action == LocalHostAction::CancelSignIn {
            if let Some(pending) = &self.local_hosting.pending {
                if matches!(pending.work, Work::Login(..)) {
                    pending.cancel.cancel();
                }
            }
            return;
        }
        let Some(profile) = self.state.configuration().profile(profile_id).cloned() else {
            return;
        };
        let Some(binding) = binding(&profile) else {
            return;
        };
        let Ok(grant) = grant(binding) else { return };
        match action {
            LocalHostAction::Retry => {
                if matches!(
                    grant.phase,
                    Phase::Revoking | Phase::CleanupPending | Phase::Revoked
                ) {
                    self.local_hosting.enqueue(Work::Remove(binding.clone()));
                } else if grant.phase.allows_hosting() && deferred_restart(&grant).is_none() {
                    self.local_hosting.enqueue(Work::Ensure(
                        Box::new(profile),
                        self.state.scrollback_limit().bytes(),
                    ));
                }
            }
            LocalHostAction::SignIn => {
                let next = if matches!(
                    grant.phase,
                    Phase::Revoking | Phase::CleanupPending | Phase::Revoked
                ) {
                    Work::Remove(binding.clone())
                } else {
                    Work::Ensure(Box::new(profile), self.state.scrollback_limit().bytes())
                };
                self.local_hosting.enqueue(Work::Login(Box::new(next)));
            }
            LocalHostAction::RecreateMissing
            | LocalHostAction::NewShell
            | LocalHostAction::Restart => {
                self.local_hosting.confirmation =
                    Some((profile_id.to_owned(), binding.clone(), action));
                self.local_hosting.focus_confirmation = true;
            }
            LocalHostAction::CancelSignIn | LocalHostAction::RevokeUnreferenced => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbound_profile_prefers_existing_hosted_then_legacy_and_refuses_unknown() {
        let fixture = tempfile::tempdir().unwrap();
        let hosted = fixture.path().join("hosted");
        let legacy = fixture.path().join("legacy");
        let roots = [hosted.clone(), legacy.clone()];
        assert!(unbound_existing_session_in(&roots, "shell")
            .unwrap()
            .is_none());
        std::fs::create_dir(&hosted).unwrap();
        std::fs::create_dir(&legacy).unwrap();
        let endpoint = fixture.path().join("legacy.sock");
        std::fs::write(&endpoint, b"").unwrap();
        let document = serde_json::json!({ "sessions": { "shell": {
            "name": "shell", "pid": std::process::id(), "socket": endpoint,
            "created_at_unix_ms": 42
        } } });
        std::fs::write(
            legacy.join("registry.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        assert_eq!(
            unbound_existing_session_in(&roots, "shell")
                .unwrap()
                .unwrap()
                .0,
            legacy
        );
        std::fs::write(
            hosted.join("registry.json"),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
        assert_eq!(
            unbound_existing_session_in(&roots, "shell")
                .unwrap()
                .unwrap()
                .0,
            hosted
        );
        std::fs::write(
            hosted.join("registry.json"),
            br#"{"sessions":{"shell":{"socket":42,"protocol_version":9}}}"#,
        )
        .unwrap();
        assert!(unbound_existing_session_in(&roots, "shell").is_err());
    }

    fn fixture() -> (LocalTunnelHosting, Consent, Profile) {
        let binding = LocalTunnelHosting::new(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            true,
        )
        .unwrap();
        let grant = serde_json::from_value(serde_json::json!({
            "version": 1,
            "installation": binding.installation(),
            "consent": binding.consent(),
            "spawn_fingerprint": "spawn-v1:fixture",
            "phase": "exposed",
            "session_name": "fixture",
            "target": null,
            "cloud_reservation": {
                "account": "42",
                "name": "festerm-aaaaaaaaaaaa4aaa8aaaaaaaaaaaaaaa",
                "ownership_label": "festerm-owned-bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
            },
            "cloud": null,
            "creation_pending": false,
            "creation_submitted": false,
            "worker_stopped": false,
            "failure": null
        }))
        .unwrap();
        let profile = Profile::local("fixture", "shell", Vec::new(), None)
            .unwrap()
            .with_persistence(PersistenceProviderKind::FestermSessiond, "fixture")
            .unwrap()
            .with_local_tunnel_hosting(Some(binding.clone()))
            .unwrap();
        (binding, grant, profile)
    }

    #[test]
    fn a_status_poll_cannot_swallow_explicit_hosting_work() {
        let (binding, _, profile) = fixture();
        let (_, receiver) = mpsc::channel();
        let mut controller = Controller {
            pending: Some(Pending {
                work: Work::Status(binding),
                receiver,
                cancel: CancellationToken::new(),
            }),
            ..Default::default()
        };
        controller.enqueue(Work::Ensure(Box::new(profile.clone()), 1024));
        assert!(matches!(controller.queue.front(), Some(Work::Ensure(..))));
        controller.enqueue(Work::Login(Box::new(Work::Ensure(Box::new(profile), 1024))));
        assert!(matches!(controller.queue.front(), Some(Work::Login(_))));
        assert_eq!(controller.queue.len(), 1);
    }

    #[test]
    fn unverifiable_consent_blocks_profile_removal_instead_of_skipping_revocation() {
        assert!(revocation_grant(Err(GrantReadFailure::Unavailable)).is_err());
        assert!(revocation_grant(Err(GrantReadFailure::Missing))
            .unwrap()
            .is_none());
        assert!(revocation_grant(Err(GrantReadFailure::ForeignConsent))
            .unwrap()
            .is_none());
        let (_, grant, _) = fixture();
        assert_eq!(revocation_grant(Ok(grant.clone())).unwrap(), Some(grant));
    }

    #[test]
    fn queued_removal_cannot_be_replaced_by_ensure_or_login_for_hosting() {
        let (binding, _, profile) = fixture();
        let mut controller = Controller::default();
        controller.enqueue(Work::Remove(binding));
        controller.enqueue(Work::Ensure(Box::new(profile.clone()), 1024));
        controller.enqueue(Work::Login(Box::new(Work::Ensure(Box::new(profile), 1024))));
        assert_eq!(controller.queue.len(), 1);
        assert!(matches!(controller.queue.front(), Some(Work::Remove(_))));
    }

    #[test]
    fn an_unreferenced_host_cannot_be_removed_after_its_profile_reappears() {
        let (binding, mut grant, profile) = fixture();
        let loaded = Configuration::new(vec![profile]).unwrap();
        for phase in [Phase::Exposed, Phase::Prepared] {
            grant.phase = phase;
            assert!(unreferenced_active(
                &Configuration::empty(),
                binding.installation(),
                &grant
            ));
            assert!(!unreferenced_active(
                &loaded,
                binding.installation(),
                &grant
            ));
        }
        assert!(references_grant(
            &loaded,
            binding.installation(),
            binding.consent()
        ));
    }

    #[test]
    fn uncertain_creation_remains_visible_without_a_transient_failure_code() {
        let (_, mut grant, _) = fixture();
        grant.creation_pending = true;
        grant.creation_submitted = true;
        grant.phase = Phase::CleanupPending;
        grant.failure = None;
        let message = phase_message(&grant);
        assert!(message.contains("unconfirmed"));
        assert!(message.contains("old reservation stays tracked"));
    }

    #[test]
    fn cloud_browser_removal_requires_a_qualified_reserved_candidate() {
        let (_, mut grant, _) = fixture();
        let name = grant.cloud_reservation.as_ref().unwrap().name.clone();
        assert!(!cloud_candidate(&grant, &name));
        assert!(!cloud_candidate(&grant, &format!("{name}.")));
        assert!(!cloud_candidate(&grant, "unrelated.usw3"));
        assert!(cloud_candidate(&grant, &format!("{name}.usw3")));
        grant.cloud = Some(profile_hosting::CloudIdentity {
            qualified_id: format!("{name}.use2"),
            incarnation: None,
        });
        assert!(!cloud_candidate(&grant, &format!("{name}.usw3")));
        assert!(cloud_candidate(&grant, &format!("{name}.use2")));
    }

    #[test]
    fn unknown_inventory_cannot_fall_back_to_unmanaged_cloud_deletion() {
        let mut inventory = LocalInventory::default();
        assert!(inventory.cloud_grant("foreign.usw3").unwrap().is_none());
        inventory.truncated = true;
        assert!(inventory.cloud_grant("foreign.usw3").is_err());
        inventory.truncated = false;
        inventory
            .entries
            .push((uuid::Uuid::new_v4().to_string(), None));
        assert!(inventory.cloud_grant("foreign.usw3").is_err());
    }
}
