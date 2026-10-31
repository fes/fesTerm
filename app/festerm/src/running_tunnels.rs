//! "My running Dev Tunnels" in New Session: the default account's running
//! bridge tunnels that have no saved profile. Opt-in through
//! `show_running_dev_tunnels`; the list is cached so opening New Session does
//! not hit the Dev Tunnels service more than once a minute.

use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use eframe::egui;
use festerm_config::Profile;
use festerm_tunnel_client::{service::TunnelSummary, Error};
use tokio_util::sync::CancellationToken;

use super::{screens::RunningTunnelRow, FesTermApp};

const CACHE: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Default)]
pub(super) struct RunningTunnels {
    tunnels: Vec<TunnelSummary>,
    fetched: Option<Instant>,
    pending: Option<Pending>,
}

struct Pending {
    receiver: mpsc::Receiver<Result<Vec<TunnelSummary>, Error>>,
    cancel: CancellationToken,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl RunningTunnels {
    /// Forgets the cache so the next frame lists again.
    pub(super) fn invalidate(&mut self) {
        self.fetched = None;
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

impl FesTermApp {
    /// The rows New Session shows. Starts or collects a listing as needed.
    pub(super) fn running_tunnel_rows(&mut self, context: &egui::Context) -> Vec<RunningTunnelRow> {
        let configuration = self.state.configuration();
        let account = configuration.accounts().first().cloned();
        let (true, Some(account)) = (self.state.show_running_dev_tunnels(), account) else {
            self.running_tunnels.clear();
            return Vec::new();
        };
        let running = &mut self.running_tunnels;
        if let Some(pending) = &running.pending {
            match pending.receiver.try_recv() {
                Ok(result) => {
                    running.pending = None;
                    running.fetched = Some(Instant::now());
                    running.tunnels = result.unwrap_or_default();
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    running.pending = None;
                    running.fetched = Some(Instant::now());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let stale = running.fetched.is_none_or(|at| at.elapsed() >= CACHE);
        if stale && running.pending.is_none() && self.native_tunnel.is_none() {
            if let Ok(store) = &self.secret_store {
                // A failed attempt is cached like a success, so an offline
                // machine retries once a minute rather than every frame.
                running.fetched = Some(Instant::now());
                if let Ok(lease) = super::tunnel_workflow::account_lease(Some(&account)) {
                    running.pending = Some(spawn(account, Arc::clone(store), lease, context));
                }
            }
        }
        let saved = |locator: &str| {
            configuration.profiles().iter().any(|profile| {
                matches!(profile, Profile::DevTunnel(tunnel) if tunnel.tunnel_name() == Some(locator))
            })
        };
        running
            .tunnels
            .iter()
            .filter(|tunnel| tunnel.active && !saved(&tunnel.locator))
            .map(|tunnel| RunningTunnelRow {
                locator: tunnel.locator.clone(),
                label: super::tunnel_browser::display_name(tunnel),
            })
            .collect()
    }

    /// Creates a profile for a listed running tunnel and connects to it.
    pub(super) fn save_and_connect_running_tunnel(
        &mut self,
        locator: &str,
        context: &egui::Context,
    ) {
        let Some(tunnel) = self
            .running_tunnels
            .tunnels
            .iter()
            .find(|tunnel| tunnel.locator == locator)
            .cloned()
        else {
            return;
        };
        let account = self
            .state
            .configuration()
            .accounts()
            .first()
            .map(|account| account.identifier().to_owned());
        if let Some(id) = self.create_profile_from_summary(&tunnel, account) {
            self.begin_native_tunnel(id, false, true, true, context);
        }
    }
}

fn spawn(
    account: festerm_config::DevTunnelAccount,
    store: Arc<dyn festerm_secret_store::SecretStore>,
    lease: Option<std::fs::File>,
    context: &egui::Context,
) -> Pending {
    let cancel = CancellationToken::new();
    let (sender, receiver) = mpsc::sync_channel(1);
    let (worker_cancel, context) = (cancel.clone(), context.clone());
    let spawned = std::thread::Builder::new()
        .name("native-tunnel-running".into())
        .spawn(move || {
            let _lease = lease;
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Error::Network)
                .and_then(|runtime| {
                    runtime.block_on(async {
                        tokio::select! {
                            _ = worker_cancel.cancelled() => Err(Error::Cancelled),
                            _ = tokio::time::sleep(TIMEOUT) => Err(Error::Expired),
                            value = super::tunnel_browser::list_for_account(account, &store, &worker_cancel) => value,
                        }
                    })
                });
            let _ = sender.try_send(result);
            context.request_repaint();
        });
    if spawned.is_err() {
        cancel.cancel();
    }
    Pending { receiver, cancel }
}
