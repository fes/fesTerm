//! Foreground-only, same-tab ordinary attachment recovery. No mutation operation.
use super::*;
use festerm_session::SessionId;
use festerm_tunnel_bridge::ordinary_auth::OrdinaryHostInfo;

#[derive(Clone)]
pub(super) struct Seed {
    locator: String,
    binding: Binding,
    info: OrdinaryHostInfo,
    #[cfg(not(test))]
    prior: host_client::ordinary::OrdinaryContext,
    target: host_client::Target,
    session: SessionId,
}

impl Seed {
    pub(super) fn new(
        ordinary: &host_client::ordinary::OrdinaryContext,
        session: &HostSession,
        locator: &str,
    ) -> Option<Self> {
        service::locator(locator).ok()?;
        (ordinary.binding() == session.binding()).then_some(Self {
            locator: locator.to_owned(),
            binding: session.binding().clone(),
            info: ordinary.host_info().clone(),
            #[cfg(not(test))]
            prior: ordinary.clone(),
            target: session.target()?.clone(),
            session: session.id(),
        })
    }

    fn validate(
        &self,
        context: &host_client::ordinary::OrdinaryContext,
        inventory: &Inventory,
    ) -> Result<(), String> {
        if context.binding() != &self.binding || context.host_info() != &self.info {
            return Err("The original protected host changed; no attachment was attempted.".into());
        }
        if inventory
            .unknown
            .iter()
            .any(|target| target.logical == self.target.logical)
        {
            return Err(
                "The original session state is unknown. No attachment was attempted.".into(),
            );
        }
        if !inventory
            .sessions
            .iter()
            .any(|session| session.target == self.target)
        {
            return Err("The exact session is not available. No replacement was started.".into());
        }
        Ok(())
    }
}

struct Ready {
    session: Arc<HostSession>,
}

pub(super) struct PendingReconnect {
    tab: u64,
    seed: Seed,
    pub(super) account: TunnelAccount,
    epoch: u64,
    attempt: u64,
    request: Arc<CancellationToken>,
    lifetime: Arc<CancellationToken>,
    job: Job<Result<Ready, String>>,
    installed: bool,
}

impl Drop for PendingReconnect {
    fn drop(&mut self) {
        self.request.cancel();
        if !self.installed {
            self.lifetime.cancel();
        }
    }
}

impl MobileApp {
    fn host_reconnect_visible(&self) -> bool {
        self.lifecycle.get().active
            && self
                .lock
                .as_ref()
                .is_none_or(|lock| lock.content_available())
            && matches!(self.surface, Surface::Terminal)
            && self.workspace == super::super::workspaces::Workspace::Terminal
            && !self.hosts.open
            && self.active_is_host()
    }

    fn reconnect_account_current(&self, account: &TunnelAccount) -> bool {
        self.profiles.as_ref().is_some_and(|profiles| {
            profiles
                .store_handle()
                .lock()
                .is_ok_and(|store| store.accounts().contains(account))
        })
    }

    fn reconnect_current(&self, pending: &PendingReconnect) -> bool {
        self.host_reconnect_visible()
            && self.active_tab == Some(pending.tab)
            && self.sink.live.host.epoch() == pending.epoch
            && self.hosts.generation == pending.attempt
            && !pending.request.is_cancelled()
            && !pending.lifetime.is_cancelled()
            && self.reconnect_account_current(&pending.account)
            && self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .and_then(live_ssh::LiveSsh::host_session_id)
                == Some(pending.seed.session)
            && self.hosts.session_accounts.iter().any(|owner| {
                owner.tab == pending.tab
                    && owner.account == pending.account
                    && owner
                        .reconnect
                        .as_ref()
                        .is_some_and(|seed| seed.session == pending.seed.session)
            })
    }

    pub(crate) fn host_reconnect_pending(&self) -> bool {
        self.hosts
            .reconnect
            .as_ref()
            .is_some_and(|pending| self.active_tab == Some(pending.tab))
            || self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .is_some_and(live_ssh::LiveSsh::host_connecting)
    }

    pub(crate) fn host_reconnect_notice(&self) -> Option<&str> {
        self.hosts
            .reconnect_notice
            .as_ref()
            .filter(|(tab, _)| Some(*tab) == self.active_tab)
            .map(|(_, notice)| notice.as_str())
    }

    pub(crate) fn cancel_host_reconnect_for_tab(&mut self, tab: u64) {
        if self
            .hosts
            .reconnect
            .as_ref()
            .is_some_and(|pending| pending.tab == tab)
        {
            self.hosts.reconnect = None;
        }
        if self
            .hosts
            .reconnect_notice
            .as_ref()
            .is_some_and(|(id, _)| *id == tab)
        {
            self.hosts.reconnect_notice = None;
        }
    }

    /// Explicit Retry and the single foreground attempt share only this Attach path.
    pub(crate) fn begin_host_reconnect(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let result = self.begin_host_reconnect_inner(ctx);
        if let (Some(tab), Err(error)) = (self.active_tab, &result) {
            self.hosts.reconnect_notice = Some((tab, error.clone()));
        }
        result
    }

    fn begin_host_reconnect_inner(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if !self.host_reconnect_visible()
            || self.hosts.reconnect.is_some()
            || self.hosts.busy()
            || self.host_reconnect_pending()
        {
            return Err("Unlock and return to the original terminal before reconnecting.".into());
        }
        let tab = self
            .active_tab
            .ok_or("The original terminal is unavailable.")?;
        if self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(live_ssh::LiveSsh::input_enabled)
        {
            return Err("This session is already connected.".into());
        }
        let owner = self
            .hosts
            .session_accounts
            .iter()
            .find(|owner| owner.tab == tab)
            .ok_or("The original attachment is unavailable.")?;
        let seed = owner
            .reconnect
            .clone()
            .ok_or("This attachment requires explicit host review; no fallback was attempted.")?;
        let account = owner.account.clone();
        if !self.reconnect_account_current(&account)
            || self
                .sink
                .live
                .connection
                .borrow()
                .as_ref()
                .and_then(live_ssh::LiveSsh::host_session_id)
                != Some(seed.session)
        {
            return Err("The original account or terminal changed.".into());
        }
        let profiles = self
            .profiles
            .as_ref()
            .ok_or("Account storage unavailable.")?;
        let store = profiles.store_handle();
        #[cfg(all(target_os = "ios", not(test)))]
        let root = profiles.host_approval_directory();
        let permit =
            ConnectionPermit::reserve(&self.hosts.budgets).map_err(|error| error.to_string())?;
        let identity_client = self
            .tunnel_client()
            .ok_or("Account preparation unavailable.")?;
        let epoch = self.sink.live.host.epoch();
        if epoch == u64::MAX {
            return Err("HOST privacy state is unavailable.".into());
        }
        let attempt = self
            .hosts
            .generation
            .checked_add(1)
            .ok_or("HOST request generation exhausted.")?;
        self.hosts.generation = attempt;
        if let Some(tab) = self.tabs.iter_mut().find(|current| current.id == tab) {
            tab.reconnect_armed = false;
        }
        if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
            live.take_host_resume_arm();
        }
        let request = self.sink.live.host.token(epoch);
        let lifetime = self.sink.live.host.token(epoch);
        let guard_store = store.clone();
        let guard_account = account.clone();
        let guard = host_client::ordinary::OrdinaryAccountGuard::new(
            lifetime.as_ref().clone(),
            Arc::new(move || {
                if guard_store
                    .lock()
                    .map_err(|_| festerm_host_protocol::ContractError::Stale)?
                    .accounts()
                    .contains(&guard_account)
                {
                    Ok(())
                } else {
                    Err(festerm_host_protocol::ContractError::Stale)
                }
            }),
        );
        let cancel = request.clone();
        let session_cancel = lifetime.clone();
        let snapshot = account.clone();
        let expected = seed.clone();
        let budgets = self.hosts.budgets.clone();
        let repaint = ctx.clone();
        #[cfg(test)]
        let ordinary_service = self.hosts.ordinary_service.clone();
        let job = Job::spawn(ctx, move |legacy_cancel| {
            let check = || {
                if cancel.is_cancelled() || legacy_cancel.is_cancelled() {
                    return Err(festerm_mobile_tunnels::TunnelError::Cancelled);
                }
                guard
                    .check()
                    .map_err(|_| festerm_mobile_tunnels::TunnelError::Cancelled)
            };
            check().map_err(|_| "Reconnect cancelled.".to_owned())?;
            let identity = identity_client
                .prepare_account_identity(&snapshot, &cancel, &check)
                .map_err(|_| {
                    "The original account could not be prepared. Reconnect or close this terminal."
                        .to_owned()
                })?;
            drop(permit);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| "HOST client worker unavailable.".to_owned())?;
            #[cfg(not(test))]
            let (ordinary, inventory) = {
                #[cfg(any(target_os = "android", target_os = "ios"))]
                let _root = native_host_approval_root(
                    #[cfg(target_os = "ios")]
                    &root,
                )?;
                runtime
                    .block_on(host_client::ordinary::rediscover_existing(
                        &identity,
                        &expected.locator,
                        &expected.prior,
                        &budgets,
                        guard.clone(),
                        &cancel,
                    ))
                    .map_err(|error| error.message().to_owned())?
            };
            #[cfg(test)]
            let (ordinary, inventory) = {
                drop(identity);
                runtime
                    .block_on(ordinary_service
                        .ok_or("Owned ordinary reconnect fixture unavailable.")?(
                        snapshot.account_id,
                        expected.locator.clone(),
                        cancel.as_ref().clone(),
                    ))
                    .map_err(|error| error.message().to_owned())?
            };
            check().map_err(|_| "Reconnect cancelled.".to_owned())?;
            expected.validate(&ordinary, &inventory)?;
            let scope = Scope {
                request: OperationId::new(),
                generation: attempt,
                binding: ordinary.binding().clone(),
            };
            // New attachment only: this constructor cannot Ensure/Create or take over a writer.
            let attachment = ApprovedAttachment::new(ordinary, scope, expected.target.clone())
                .map_err(|error| error.message().to_owned())?;
            let session = Arc::new(
                attachment
                    .start_with_cancellation(
                        &budgets,
                        Arc::new(live_ssh::Repaint(repaint)),
                        session_cancel.as_ref().clone(),
                    )
                    .map_err(|error| error.message().to_owned())?,
            );
            if check().is_err() {
                session.cancel();
                return Err("Reconnect cancelled.".into());
            }
            Ok(Ready { session })
        });
        self.hosts.reconnect_notice = None;
        self.hosts.reconnect = Some(PendingReconnect {
            tab,
            seed,
            account,
            epoch,
            attempt,
            request,
            lifetime,
            job,
            installed: false,
        });
        Ok(())
    }

    pub(crate) fn poll_host_reconnect(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.hosts.reconnect.as_ref() {
            if !self.reconnect_current(pending) {
                self.hosts.reconnect = None;
            }
        }
        let result = self
            .hosts
            .reconnect
            .as_ref()
            .and_then(|pending| match pending.job.poll() {
                Poll::Pending => None,
                Poll::Done(result) => Some(result),
                Poll::Lost => Some(Err(
                    "Reconnect worker ended; no session outcome inferred.".into()
                )),
            });
        if let Some(result) = result {
            let mut pending = self.hosts.reconnect.take().expect("completed reconnect");
            if !self.reconnect_current(&pending) {
                if let Ok(ready) = result {
                    ready.session.cancel();
                }
                return;
            }
            match result {
                Err(error) => self.hosts.reconnect_notice = Some((pending.tab, error)),
                Ok(ready) => {
                    if !self.sink.live.host.session(pending.epoch, &ready.session) {
                        return;
                    }
                    let owner = self
                        .hosts
                        .session_accounts
                        .iter_mut()
                        .find(|owner| owner.tab == pending.tab)
                        .expect("current owner checked");
                    pending.seed.session = ready.session.id();
                    owner.session = Arc::downgrade(&ready.session);
                    owner.ordinary_lifetime = Some(pending.lifetime.clone());
                    owner.reconnect = Some(pending.seed.clone());
                    let mut replacement = live_ssh::LiveSsh::from_host(ready.session, ctx);
                    if let Some(previous) = self.sink.live.connection.borrow().as_ref() {
                        replacement.retain_host_adoption(previous);
                    }
                    *self.sink.live.connection.borrow_mut() = Some(replacement);
                    // Terminal history stays visible until the authenticated snapshot is adopted.
                    // No old queued input or resize is transferred to the new worker.
                    self.cancel_dictation();
                    self.terminal_keyboard.cancel();
                    self.gestures.cancel();
                    self.clear_sticky_modifiers();
                    pending.installed = true;
                }
            }
        }
        if !self.host_reconnect_visible() || self.hosts.reconnect.is_some() {
            return;
        }
        let Some(tab) = self.active_tab else {
            return;
        };
        if !self
            .tabs
            .iter()
            .any(|current| current.id == tab && current.reconnect_armed)
        {
            return;
        }
        if self
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .is_some_and(live_ssh::LiveSsh::input_enabled)
        {
            if let Some(live) = self.sink.live.connection.borrow_mut().as_mut() {
                live.take_host_resume_arm();
            }
            if let Some(current) = self.tabs.iter_mut().find(|current| current.id == tab) {
                current.reconnect_armed = false;
            }
            return;
        }
        let armed = self
            .sink
            .live
            .connection
            .borrow_mut()
            .as_mut()
            .is_some_and(live_ssh::LiveSsh::take_host_resume_arm);
        if armed {
            // Consume before attempting, including a refusal; repeated frames cannot loop.
            if let Some(current) = self.tabs.iter_mut().find(|current| current.id == tab) {
                current.reconnect_armed = false;
            }
            if let Err(error) = self.begin_host_reconnect(ctx) {
                self.hosts.reconnect_notice = Some((tab, error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        client_inventory, ordinary_fixture_context, ordinary_mobile_ui_fixture,
    };
    use super::*;
    use festerm_host_protocol::{GenerationId, Lifetime, SessionSummary};
    use festerm_session::SessionLifecycle;
    use std::time::{Duration, Instant};

    fn fixture() -> (tempfile::TempDir, MobileApp, Seed, Inventory) {
        let (root, ordinary) = ordinary_fixture_context(7);
        let target = host_client::Target {
            logical: LogicalSessionId::new(),
            generation: GenerationId::new(3).unwrap(),
        };
        let mut inventory = client_inventory(false);
        inventory.sessions.push(SessionSummary {
            target: target.clone(),
            name: "Existing".into(),
            lifetime: Lifetime::Persistent {},
            attached: false,
        });
        let (mut app, _) =
            ordinary_mobile_ui_fixture(ordinary.clone(), inventory.clone(), root.path());
        app.hosts.approval_account = app.host_account();
        app.hosts.ordinary = Some(ordinary.clone());
        app.hosts.locator = "build.fixture".into();
        let scope = Scope {
            request: OperationId::new(),
            generation: 1,
            binding: ordinary.binding().clone(),
        };
        let ctx = egui::Context::default();
        let session = Arc::new(
            ApprovedAttachment::new(ordinary.clone(), scope, target)
                .unwrap()
                .start(&app.hosts.budgets, Arc::new(live_ssh::Repaint(ctx.clone())))
                .unwrap(),
        );
        app.install_host_session(&ctx, "Existing".into(), session.clone());
        let seed = app
            .hosts
            .session_accounts
            .last()
            .unwrap()
            .reconnect
            .clone()
            .unwrap();
        session.cancel();
        let until = Instant::now() + Duration::from_secs(2);
        while session.lifecycle() == SessionLifecycle::Starting && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        (root, app, seed, inventory)
    }

    #[test]
    fn exact_host_identity_and_running_generation_are_required() {
        let (_root, app, seed, mut inventory) = fixture();
        let context = app.hosts.ordinary.as_ref().unwrap();
        assert!(seed.validate(context, &inventory).is_ok());
        inventory.unknown.push(seed.target.clone());
        assert!(seed.validate(context, &inventory).is_err());
        inventory.unknown.clear();
        let mut changed = seed.clone();
        changed.info.policy_epoch = festerm_host_protocol::Epoch::new(2).unwrap();
        assert!(changed.validate(context, &inventory).is_err());
        changed = seed.clone();
        changed.info.installation = festerm_host_protocol::HostId::new();
        assert!(changed.validate(context, &inventory).is_err());
        changed = seed.clone();
        changed.info.principal = festerm_host_protocol::PrincipalId::new();
        assert!(changed.validate(context, &inventory).is_err());
        changed = seed.clone();
        changed.info.route.host.host_key_fingerprint.push('x');
        assert!(changed.validate(context, &inventory).is_err());
        inventory.sessions[0].target.generation = GenerationId::new(4).unwrap();
        assert!(seed.validate(context, &inventory).is_err());
        inventory.sessions.clear();
        assert!(seed.validate(context, &inventory).is_err());
    }

    #[test]
    fn reconnect_is_same_tab_and_fails_closed_on_context_changes() {
        for change in [
            "background",
            "chooser",
            "settings",
            "tab",
            "epoch",
            "attempt",
            "account",
        ] {
            let (_root, mut app, _seed, _) = fixture();
            let ctx = egui::Context::default();
            app.terminal.ingest(b"retained history");
            let tab = app.active_tab;
            app.begin_host_reconnect(&ctx).unwrap();
            assert!(app.host_reconnect_pending());
            let pending = app.hosts.reconnect.as_ref().unwrap();
            let lifetime = pending.lifetime.clone();
            assert!(app.reconnect_current(pending));
            match change {
                "background" => {
                    let mut state = app.lifecycle.get();
                    state.active = false;
                    app.lifecycle.set(state);
                }
                "chooser" => app.hosts.open = true,
                "settings" => app.surface = Surface::Tabs,
                "tab" => app.active_tab = None,
                "epoch" => app.sink.live.host.invalidate(),
                "attempt" => app.hosts.generation += 1,
                "account" => {
                    let id = app.hosts.reconnect.as_ref().unwrap().account.id.clone();
                    app.invalidate_host_account(&id);
                }
                _ => unreachable!(),
            }
            app.poll_host_reconnect(&ctx);
            assert!(app.hosts.reconnect.is_none(), "{change}");
            assert!(lifetime.is_cancelled(), "{change}");
            assert!(app
                .terminal
                .text_snapshot()
                .text()
                .contains("retained history"));
            assert_eq!(app.tabs.iter().filter(|t| Some(t.id) == tab).count(), 1);
            assert!(app.hosts.worker.is_none() && app.hosts.pending.is_none());
        }
    }

    #[test]
    fn completed_attach_installs_only_same_tab_and_preserves_history_until_snapshot() {
        let (_root, mut app, seed, _) = fixture();
        let ctx = egui::Context::default();
        app.terminal.ingest(b"original history");
        let tab = app.active_tab;
        let tabs = app.tabs.len();
        app.begin_host_reconnect(&ctx).unwrap();
        let (stream, _peer) = tokio::io::duplex(8192);
        let scope = Scope {
            request: OperationId::new(),
            generation: app.hosts.generation,
            binding: seed.binding.clone(),
        };
        let replacement = host_client::testing::session_from_stream(
            stream,
            ConnectionPermit::reserve(&app.hosts.budgets).unwrap(),
            scope,
            seed.target.clone(),
            Arc::new(live_ssh::Repaint(ctx.clone())),
        )
        .unwrap();
        let installed = replacement.clone();
        let pending = app.hosts.reconnect.as_mut().unwrap();
        // The discarded job can cancel its own session lifetime during cleanup.
        // Give the injected result fresh tokens under the same privacy epoch.
        let original_request = std::mem::replace(
            &mut pending.request,
            app.sink.live.host.token(pending.epoch),
        );
        let original_lifetime = std::mem::replace(
            &mut pending.lifetime,
            app.sink.live.host.token(pending.epoch),
        );
        pending.job = Job::spawn(&ctx, move |_| {
            Ok(Ready {
                session: replacement,
            })
        });
        original_request.cancel();
        // Model the superseded worker cancelling its session after handoff.
        original_lifetime.cancel();
        assert!(app.reconnect_current(app.hosts.reconnect.as_ref().unwrap()));
        let until = Instant::now() + Duration::from_secs(2);
        while app.hosts.reconnect.is_some() && Instant::now() < until {
            app.poll_host_reconnect(&ctx);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.hosts.reconnect.is_none());
        assert_eq!(app.active_tab, tab);
        assert_eq!(app.tabs.len(), tabs);
        assert_eq!(
            app.sink
                .live
                .connection
                .borrow()
                .as_ref()
                .unwrap()
                .host_session_id(),
            Some(installed.id()),
            "synthetic attach did not install: {:?}",
            app.host_reconnect_notice()
        );
        assert!(app
            .terminal
            .text_snapshot()
            .text()
            .contains("original history"));
        assert!(!app
            .sink
            .live
            .connection
            .borrow()
            .as_ref()
            .unwrap()
            .input_enabled());
        assert_eq!(
            app.hosts
                .session_accounts
                .last()
                .unwrap()
                .reconnect
                .as_ref()
                .unwrap()
                .target,
            seed.target
        );
        assert!(!app.hosts.open);
        installed.cancel();
    }

    #[test]
    fn manual_retry_keeps_exact_seed_and_never_opens_chooser() {
        let (_root, mut app, seed, _) = fixture();
        let ctx = egui::Context::default();
        for _ in 0..2 {
            app.begin_host_reconnect(&ctx).unwrap();
            let pending = app.hosts.reconnect.as_ref().unwrap();
            assert_eq!(pending.seed.target, seed.target);
            assert_eq!(pending.seed.binding, seed.binding);
            let tab = pending.tab;
            app.cancel_host_reconnect_for_tab(tab);
            assert!(!app.hosts.open);
        }
        assert!(app.hosts.worker.is_none() && app.hosts.pending.is_none());
    }
}
