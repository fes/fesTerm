use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

use eframe::egui;
use festerm_config::{DevTunnelHost, Profile};
use festerm_secret_store::{SecretBytes, SecretReference, SecretStore};
use festerm_tunnel_bridge::{
    relay::{self, Destination},
    transport,
};
use festerm_tunnel_client::{
    Error, Https,
    identity::{self, Account, DevicePrompt, Identity},
    service,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::FesTermApp;
use crate::{tabs::TunnelConnectToken, tunnel_activation::Launch};

static ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
#[path = "tunnel_workflow_tests.rs"]
mod tests;

struct WorkerSlot {
    identity_lease: OnceLock<std::fs::File>,
}

fn acquire_identity_lease(
    root: &std::path::Path,
    reference: &SecretReference,
) -> Result<std::fs::File, Error> {
    std::fs::create_dir_all(root).map_err(|_| Error::Store)?;
    let metadata = std::fs::symlink_metadata(root).map_err(|_| Error::Store)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Store);
    }
    let path = root.join(format!("{}.lock", reference.to_persisted_string()));
    if std::fs::symlink_metadata(&path)
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err(Error::Store);
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| Error::Store)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
            Error::Busy
        } else {
            Error::Store
        }
    })?;
    Ok(file)
}

fn profile_identity_lease(
    expected: &Profile,
    remember: bool,
) -> Result<Option<std::fs::File>, Error> {
    let identity = expected
        .as_dev_tunnel()
        .and_then(|profile| profile.identity());
    if !remember || identity.is_none() {
        return Ok(None);
    }
    let root = directories::ProjectDirs::from("com", "fes", "fesTerm")
        .ok_or(Error::Store)?
        .cache_dir()
        .join("native-tunnel-identity-locks");
    acquire_identity_lease(&root, identity.ok_or(Error::Store)?.credential_reference()).map(Some)
}
impl Drop for WorkerSlot {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

struct Stored {
    store: Arc<dyn SecretStore>,
    references: Vec<SecretReference>,
}
impl Drop for Stored {
    fn drop(&mut self) {
        for reference in &self.references {
            let _ = self.store.delete(reference);
        }
    }
}

struct Prepared {
    profile: Option<Profile>,
    launch: Launch,
    stored: Stored,
}

enum Event {
    Device(DevicePrompt),
    Refresh {
        bytes: zeroize::Zeroizing<Vec<u8>>,
        published: oneshot::Sender<Result<(), Error>>,
    },
    Host {
        account: Account,
        host: DevTunnelHost,
        approval: oneshot::Sender<()>,
    },
    Complete(Result<Box<Prepared>, Error>),
}
enum Stage {
    Working,
    Device(DevicePrompt),
    Host {
        account: Account,
        host: DevTunnelHost,
        approval: Option<oneshot::Sender<()>>,
    },
    Error(Error),
}

pub(super) struct Workflow {
    expected: Profile,
    cancel: CancellationToken,
    receiver: mpsc::Receiver<Event>,
    stage: Stage,
    connect: bool,
    remember: bool,
    focus_cancel: bool,
    ready: Option<Box<Prepared>>,
    _slot: Option<Arc<WorkerSlot>>,
}
impl Drop for Workflow {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

fn host_metadata(binding: &service::HostBinding) -> DevTunnelHost {
    DevTunnelHost {
        tunnel_id: binding.tunnel_id.clone(),
        cluster: binding.cluster.clone(),
        host_id: binding.host_id.clone(),
        public_key: binding.public_key.clone(),
        port: binding.port,
    }
}

impl Workflow {
    fn start(
        expected: Profile,
        store: Arc<dyn SecretStore>,
        sign_in: bool,
        remember: bool,
        connect: bool,
        context: &egui::Context,
    ) -> Self {
        let cancel = CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut workflow = Self {
            expected: expected.clone(),
            cancel: cancel.clone(),
            receiver,
            stage: Stage::Working,
            connect,
            remember,
            focus_cancel: true,
            ready: None,
            _slot: None,
        };
        let Some(client_id) = identity::registered_client_id() else {
            workflow.stage = Stage::Error(Error::Registration);
            return workflow;
        };
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            workflow.stage = Stage::Error(Error::Busy);
            return workflow;
        }
        let slot = Arc::new(WorkerSlot {
            identity_lease: OnceLock::new(),
        });
        workflow._slot = Some(Arc::clone(&slot));
        let context = context.clone();
        let spawned = std::thread::Builder::new().name("native-tunnel-sign-in".into())
            .spawn(move || {
                let _slot = slot;
                let notify = |event| {
                    let sent = sender.try_send(event).is_ok();
                    context.request_repaint();
                    sent
                };
                let result = profile_identity_lease(&expected, remember).and_then(|lease| {
                    if let Some(lease) = lease {
                        _slot.identity_lease.set(lease).map_err(|_| Error::Busy)?;
                    }
                    tokio::runtime::Builder::new_current_thread().enable_all().build()
                        .map_err(|_| Error::Network)
                })
                    .and_then(|runtime| runtime.block_on(async {
                        tokio::select! {
                            _ = cancel.cancelled() => Err(Error::Cancelled),
                            _ = tokio::time::sleep(Duration::from_secs(1800)) => Err(Error::Expired),
                            value = prepare(expected, store, client_id, sign_in, remember, &cancel, &notify) => value.map(Box::new),
                        }
                    }));
                notify(Event::Complete(result));
            });
        if spawned.is_err() {
            workflow._slot = None;
            workflow.stage = Stage::Error(Error::Network);
        }
        workflow
    }
}

async fn prepare(
    expected: Profile,
    store: Arc<dyn SecretStore>,
    client_id: &str,
    force_sign_in: bool,
    remember: bool,
    cancel: &CancellationToken,
    notify: &impl Fn(Event) -> bool,
) -> Result<Prepared, Error> {
    let profile = expected.as_dev_tunnel().ok_or(Error::Tunnel)?;
    let name = profile.tunnel_name().ok_or(Error::Tunnel)?;
    let http = Https::new()?;
    let new_account = force_sign_in || profile.identity().is_none() || !remember;
    let identity = if new_account {
        identity::sign_in(&http, client_id, None, cancel, |prompt| {
            if !notify(Event::Device(prompt)) {
                cancel.cancel();
            }
        })
        .await?
    } else {
        let binding = profile.identity().ok_or(Error::AccountChanged)?;
        let secret = store
            .get(binding.credential_reference())
            .map_err(|_| Error::Store)?;
        let identity = secret
            .with_bytes(|bytes| Identity::decode_private(bytes, binding.account_id, client_id))?;
        if identity.needs_sign_in() {
            let refreshed = identity.refresh(&http, client_id, cancel).await?;
            let (published, publication) = oneshot::channel();
            if !notify(Event::Refresh {
                bytes: refreshed.encode_private()?,
                published,
            }) {
                return Err(Error::Cancelled);
            }
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::Cancelled),
                result = publication => result.map_err(|_| Error::Cancelled)??,
            };
            refreshed
        } else {
            identity
        }
    };
    let resolved = service::resolve(&identity, name, cancel)
        .await
        .inspect_err(|error| {
            tracing::warn!(stage = "tunnel-lookup", error = ?error, "Native Dev Tunnel setup failed");
        })?;
    let host = host_metadata(&resolved.binding);
    relay::host_fingerprint(&host.public_key).map_err(|_| Error::Tunnel)?;
    if new_account || profile.trusted_host() != Some(&host) {
        let (approval, approved) = oneshot::channel();
        if !notify(Event::Host {
            account: identity.account.clone(),
            host: host.clone(),
            approval,
        }) {
            return Err(Error::Cancelled);
        }
        tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(300)) => return Err(Error::Expired),
            result = approved => result.map_err(|_| Error::Cancelled)?,
        };
    }
    let description = tokio::time::timeout(
        Duration::from_secs(30),
        transport::discover(
            &resolved.endpoint,
            resolved.binding.port,
            &resolved.connect_token,
        ),
    )
    .await
    .map_err(|_| Error::Network)?
    .map_err(|_| Error::Tunnel)?;
    let destination = Destination {
        version: 1,
        tunnel_id: resolved.binding.tunnel_id,
        cluster: resolved.binding.cluster,
        port: resolved.binding.port,
        target: description.target,
        endpoint: resolved.endpoint,
        file_read: description.files.read,
        path_style: if description.files.windows_paths {
            festerm_tunnel_bridge::files::PathStyle::Windows
        } else {
            festerm_tunnel_bridge::files::PathStyle::Posix
        },
    };
    let mut stored = Stored {
        store,
        references: Vec::new(),
    };
    let updated = if remember {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let identity_ref = if new_account {
            let encoded = identity.encode_private()?;
            let reference = stored
                .store
                .put(&SecretBytes::copy_from_slice(&encoded))
                .map_err(|_| Error::Store)?;
            stored.references.push(reference.duplicate_for_transport());
            reference
        } else {
            profile
                .identity()
                .ok_or(Error::AccountChanged)?
                .credential_reference()
                .duplicate_for_transport()
        };
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let token =
            TunnelConnectToken::new(resolved.connect_token.to_string()).into_stored_secret();
        let connect_ref = stored.store.put(&token).map_err(|_| Error::Store)?;
        stored
            .references
            .push(connect_ref.duplicate_for_transport());
        Some(
            expected
                .clone()
                .with_tunnel_identity(
                    identity.account.id,
                    identity.account.login.clone(),
                    identity_ref,
                    connect_ref,
                    host,
                )
                .map_err(|_| Error::Response)?,
        )
    } else {
        None
    };
    if cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    Ok(Prepared {
        profile: updated,
        launch: Launch::from_destination(destination, resolved.connect_token)
            .with_account(identity.account.clone()),
        stored,
    })
}

impl FesTermApp {
    pub(super) fn retire_native_tunnel_credentials(&self, previous: Option<&Profile>) {
        let Some(previous) = previous else { return };
        let Some(old) = previous
            .as_dev_tunnel()
            .filter(|profile| profile.tunnel_name().is_some())
        else {
            return;
        };
        let Ok(store) = &self.secret_store else {
            return;
        };
        let current = self
            .state
            .configuration()
            .profile(previous.identifier())
            .and_then(Profile::as_dev_tunnel);
        for reference in old
            .identity()
            .map(|identity| identity.credential_reference())
            .into_iter()
            .chain(old.credential_reference())
        {
            let retained = current.is_some_and(|profile| {
                profile.credential_reference() == Some(reference)
                    || profile
                        .identity()
                        .is_some_and(|identity| identity.credential_reference() == reference)
            });
            if !retained {
                let _ = store.delete(reference);
            }
        }
    }

    pub(super) fn begin_native_tunnel(
        &mut self,
        profile_id: String,
        sign_in: bool,
        remember: bool,
        connect: bool,
        context: &egui::Context,
    ) {
        if self.native_tunnel.is_some() {
            return;
        }
        let Some(profile) = self
            .state
            .configuration()
            .profile(&profile_id)
            .filter(|profile| {
                profile
                    .as_dev_tunnel()
                    .is_some_and(|profile| profile.tunnel_name().is_some())
            })
            .cloned()
        else {
            return;
        };
        let store = match &self.secret_store {
            Ok(store) => Arc::clone(store),
            Err(_) if !remember => Arc::new(festerm_secret_store::MemorySecretStore::new()),
            Err(_) => {
                self.secure_storage_feedback =
                    Some("Secure storage unavailable. Use Connect once.");
                return;
            }
        };
        self.native_tunnel = Some(Workflow::start(
            profile, store, sign_in, remember, connect, context,
        ));
        self.overlays.native_tunnel = true;
        context.request_repaint();
    }

    pub(super) fn poll_native_tunnel(&mut self, context: &egui::Context) {
        let Some(mut workflow) = self.native_tunnel.take() else {
            return;
        };
        if self
            .state
            .configuration()
            .profile(workflow.expected.identifier())
            != Some(&workflow.expected)
        {
            self.secure_storage_feedback = Some("Profile changed. Sign-in cancelled.");
            self.overlays.native_tunnel = false;
            return;
        }
        loop {
            let event = match workflow.receiver.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if workflow.ready.is_none() && !matches!(workflow.stage, Stage::Error(_)) {
                        workflow.stage = Stage::Error(Error::Network);
                    }
                    break;
                }
            };
            match event {
                Event::Device(prompt) => workflow.stage = Stage::Device(prompt),
                Event::Refresh { bytes, published } => {
                    let result = workflow
                        .expected
                        .as_dev_tunnel()
                        .and_then(|profile| profile.identity())
                        .ok_or(Error::AccountChanged)
                        .and_then(|identity| {
                            self.secret_store
                                .as_ref()
                                .map_err(|_| Error::Store)?
                                .update(
                                    identity.credential_reference(),
                                    &SecretBytes::copy_from_slice(&bytes),
                                )
                                .map_err(|_| Error::Store)
                        });
                    let _ = published.send(result);
                }
                Event::Host {
                    account,
                    host,
                    approval,
                } => {
                    workflow.focus_cancel = true;
                    workflow.stage = Stage::Host {
                        account,
                        host,
                        approval: Some(approval),
                    };
                }
                Event::Complete(Ok(ready)) => workflow.ready = Some(ready),
                Event::Complete(Err(error)) => workflow.stage = Stage::Error(error),
            }
        }
        self.native_tunnel = Some(workflow);
        let _ = context;
    }

    fn finish_native_tunnel(&mut self, context: &egui::Context) {
        let Some(mut workflow) = self.native_tunnel.take() else {
            return;
        };
        if self
            .state
            .configuration()
            .profile(workflow.expected.identifier())
            != Some(&workflow.expected)
        {
            self.secure_storage_feedback = Some("Profile changed. Sign-in cancelled.");
            self.overlays.native_tunnel = false;
            return;
        }
        if let Some(mut ready) = workflow.ready.take() {
            if let Some(profile) = ready.profile.take() {
                if !self.save_profile(profile) {
                    self.secure_storage_feedback =
                        Some("Account could not be saved. Sign in again.");
                    self.overlays.native_tunnel = false;
                    return;
                }
                ready.stored.references.clear();
            }
            if workflow.connect {
                let profile_id = Some(workflow.expected.identifier().to_owned());
                self.state
                    .start_tunnel_launch(profile_id, move || Ok(ready.launch), context);
            }
            self.overlays.native_tunnel = false;
            return;
        }
        self.native_tunnel = Some(workflow);
    }

    pub(super) fn show_native_tunnel(&mut self, context: &egui::Context, escape: bool) {
        let Some(workflow) = self.native_tunnel.as_mut() else {
            return;
        };
        let mut dismiss = escape;
        egui::Modal::new(egui::Id::new("native_dev_tunnel")).show(context, |ui| {
            super::confirmations::prepare_confirmation(ui, context, 520.0);
            ui.heading("Dev Tunnel");
            match &mut workflow.stage {
                Stage::Working => {
                    ui.spinner();
                    ui.label("Connecting…");
                }
                Stage::Device(prompt) => {
                    ui.label("Sign in to Microsoft Dev Tunnels");
                    ui.label("Enter this code on GitHub:");
                    if ui
                        .button(egui::RichText::new(&prompt.user_code).monospace())
                        .on_hover_text("Copy code and open GitHub")
                        .clicked()
                    {
                        ui.ctx().copy_text(prompt.user_code.clone());
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(identity::VERIFICATION_URI));
                    }
                    ui.label("Waiting for sign-in…");
                }
                Stage::Host {
                    account,
                    host,
                    approval,
                } => {
                    ui.label(format!("GitHub · {}", account.login));
                    ui.label(format!(
                        "{}.{} · {}",
                        host.tunnel_id, host.cluster, host.port
                    ));
                    if let Some(previous) = workflow
                        .expected
                        .as_dev_tunnel()
                        .and_then(|profile| profile.trusted_host())
                    {
                        if previous != host {
                            ui.strong("Host identity changed.");
                            if let Ok(fingerprint) = relay::host_fingerprint(&previous.public_key) {
                                ui.small(format!("Previous: {fingerprint}"));
                            }
                        }
                    }
                    ui.label("Trust this host?");
                    if let Ok(fingerprint) = relay::host_fingerprint(&host.public_key) {
                        ui.monospace(fingerprint);
                    }
                    if workflow.remember {
                        ui.small("Uses native secure storage.");
                    }
                    if ui
                        .button(if workflow.connect {
                            "Connect"
                        } else {
                            "Save account"
                        })
                        .clicked()
                    {
                        if let Some(approval) = approval.take() {
                            let _ = approval.send(());
                        }
                        workflow.stage = Stage::Working;
                    }
                }
                Stage::Error(error) => {
                    ui.label(error.to_string());
                }
            }
            let cancel = ui.button(if matches!(workflow.stage, Stage::Error(_)) {
                "Close"
            } else {
                "Cancel"
            });
            if workflow.focus_cancel {
                cancel.request_focus();
                workflow.focus_cancel = false;
            }
            dismiss |= cancel.clicked();
        });
        if dismiss {
            self.native_tunnel = None;
            self.overlays.native_tunnel = false;
        } else if self
            .native_tunnel
            .as_ref()
            .is_some_and(|workflow| workflow.ready.is_some())
        {
            self.finish_native_tunnel(context);
        }
    }
}
