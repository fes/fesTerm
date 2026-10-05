//! SSH local and remote TCP/IP port-forward configuration and runtime.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use tokio::io::AsyncWriteExt;

use festerm_session::{
    SessionError, SessionErrorKind, SessionEvent, SshPortForwardDirection, SshPortForwardRuntime,
    SshPortForwardSource, SshPortForwardState,
};

use crate::{
    SshClientHandler, WorkerShared, COMMAND_POLL_INTERVAL, CONNECT_TIMEOUT,
    PORT_FORWARD_PENDING_CONNECTION_CAPACITY,
};

/// Secret-free metadata that the SSH backend can consume as a port-forward request.
///
/// `festerm-config::SshPortForwardConfiguration` implements this trait so
/// saved profile metadata can flow directly into `festerm-ssh` without
/// duplicating the configuration struct.
pub trait SshPortForwardSpec {
    fn direction(&self) -> SshPortForwardDirection;
    fn bind_host(&self) -> &str;
    fn bind_port(&self) -> u16;
    fn destination_host(&self) -> &str;
    fn destination_port(&self) -> u16;
}

/// Combined profile, pending, active, and failed mappings per connected SSH session.
pub const MAX_SSH_PORT_FORWARD_ENTRIES: usize = 128;

impl<T: SshPortForwardSpec + ?Sized> SshPortForwardSpec for &T {
    fn direction(&self) -> SshPortForwardDirection {
        (**self).direction()
    }

    fn bind_host(&self) -> &str {
        (**self).bind_host()
    }

    fn bind_port(&self) -> u16 {
        (**self).bind_port()
    }

    fn destination_host(&self) -> &str {
        (**self).destination_host()
    }

    fn destination_port(&self) -> u16 {
        (**self).destination_port()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPortForwardConfigurationError {
    EmptyHost,
    ControlCharacter,
    ZeroPort,
    DuplicateBinding,
    InventoryLimit,
}

impl fmt::Display for SshPortForwardConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHost => formatter.write_str("SSH port-forward host must not be empty"),
            Self::ControlCharacter => {
                formatter.write_str("SSH port-forward host must not contain control characters")
            }
            Self::ZeroPort => formatter.write_str("SSH port-forward port must not be zero"),
            Self::DuplicateBinding => formatter.write_str(
                "SSH port-forward bindings must not repeat the same direction and bind address",
            ),
            Self::InventoryLimit => write!(
                formatter,
                "SSH session supports at most {MAX_SSH_PORT_FORWARD_ENTRIES} port-forward mappings"
            ),
        }
    }
}

impl std::error::Error for SshPortForwardConfigurationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPortForwardRequestError {
    NotRunning,
    QueueFull,
    Closed,
    InventoryFull,
    InvalidConfiguration(SshPortForwardConfigurationError),
}

impl fmt::Display for SshPortForwardRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => {
                formatter.write_str("SSH port forwarding is only available while connected")
            }
            Self::QueueFull => formatter.write_str("SSH port-forward request queue is full"),
            Self::Closed => {
                formatter.write_str("SSH port-forward request was rejected: session closed")
            }
            Self::InventoryFull => write!(
                formatter,
                "SSH port-forward inventory is full ({MAX_SSH_PORT_FORWARD_ENTRIES} mappings); remove an active or failed mapping before adding another"
            ),
            Self::InvalidConfiguration(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SshPortForwardRequestError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RequestedSshPortForward {
    pub(crate) direction: SshPortForwardDirection,
    pub(crate) bind_host: String,
    pub(crate) bind_port: u16,
    pub(crate) destination_host: String,
    pub(crate) destination_port: u16,
    pub(crate) source: SshPortForwardSource,
}

impl RequestedSshPortForward {
    fn binding_key(&self) -> PortForwardBindingKey {
        PortForwardBindingKey {
            direction: self.direction,
            bind_host: self.bind_host.clone(),
            bind_port: self.bind_port,
        }
    }

    pub(crate) fn runtime(
        &self,
        state: SshPortForwardState,
        failure_reason: Option<String>,
    ) -> SshPortForwardRuntime {
        SshPortForwardRuntime::new(
            self.direction,
            self.bind_host.clone(),
            self.bind_port,
            self.destination_host.clone(),
            self.destination_port,
            self.source,
            state,
            failure_reason,
        )
    }
}

pub(crate) fn collect_requested_port_forwards<I>(
    port_forwards: I,
    source: SshPortForwardSource,
) -> Result<Vec<RequestedSshPortForward>, SshPortForwardConfigurationError>
where
    I: IntoIterator,
    I::Item: SshPortForwardSpec,
{
    let mut collected = Vec::new();
    let mut bindings = HashSet::new();
    for forward in port_forwards {
        if collected.len() == MAX_SSH_PORT_FORWARD_ENTRIES {
            return Err(SshPortForwardConfigurationError::InventoryLimit);
        }
        let requested = requested_port_forward(forward, source)?;
        if !bindings.insert(requested.binding_key()) {
            return Err(SshPortForwardConfigurationError::DuplicateBinding);
        }
        collected.push(requested);
    }
    Ok(collected)
}

pub(crate) fn requested_port_forward(
    forward: impl SshPortForwardSpec,
    source: SshPortForwardSource,
) -> Result<RequestedSshPortForward, SshPortForwardConfigurationError> {
    let bind_host = forward.bind_host().trim();
    let destination_host = forward.destination_host().trim();
    if bind_host.is_empty() || destination_host.is_empty() {
        return Err(SshPortForwardConfigurationError::EmptyHost);
    }
    if bind_host.chars().any(char::is_control) || destination_host.chars().any(char::is_control) {
        return Err(SshPortForwardConfigurationError::ControlCharacter);
    }
    if forward.bind_port() == 0 || forward.destination_port() == 0 {
        return Err(SshPortForwardConfigurationError::ZeroPort);
    }
    Ok(RequestedSshPortForward {
        direction: forward.direction(),
        bind_host: bind_host.to_owned(),
        bind_port: forward.bind_port(),
        destination_host: destination_host.to_owned(),
        destination_port: forward.destination_port(),
        source,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortForwardBindingKey {
    pub(crate) direction: SshPortForwardDirection,
    pub(crate) bind_host: String,
    pub(crate) bind_port: u16,
}

impl Hash for PortForwardBindingKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&self.direction).hash(state);
        self.bind_host.hash(state);
        self.bind_port.hash(state);
    }
}

#[derive(Default)]
struct PortForwardAdmissionState {
    generation: u64,
    bindings: HashSet<PortForwardBindingKey>,
}

#[derive(Default)]
pub(crate) struct PortForwardAdmissions {
    state: Arc<Mutex<PortForwardAdmissionState>>,
}

impl PortForwardAdmissions {
    pub(crate) fn reset(&self, generation: u64) {
        *self
            .state
            .lock()
            .expect("forward admission lock is not poisoned") = PortForwardAdmissionState {
            generation,
            bindings: HashSet::new(),
        };
    }

    pub(crate) fn reserve(
        &self,
        requested: RequestedSshPortForward,
        generation: u64,
    ) -> Result<AdmittedPortForward, SshPortForwardRequestError> {
        let mut state = self
            .state
            .lock()
            .expect("forward admission lock is not poisoned");
        if generation != state.generation {
            return Err(SshPortForwardRequestError::NotRunning);
        }
        let key = requested.binding_key();
        if state.bindings.contains(&key) {
            return Err(SshPortForwardRequestError::InvalidConfiguration(
                SshPortForwardConfigurationError::DuplicateBinding,
            ));
        }
        if state.bindings.len() == MAX_SSH_PORT_FORWARD_ENTRIES {
            return Err(SshPortForwardRequestError::InventoryFull);
        }
        state.bindings.insert(key.clone());
        Ok(AdmittedPortForward {
            requested,
            reservation: PortForwardReservation {
                state: Arc::clone(&self.state),
                key,
                generation,
            },
        })
    }
}

pub(crate) struct AdmittedPortForward {
    requested: RequestedSshPortForward,
    reservation: PortForwardReservation,
}

struct PortForwardReservation {
    state: Arc<Mutex<PortForwardAdmissionState>>,
    key: PortForwardBindingKey,
    generation: u64,
}

impl PortForwardReservation {
    fn is_current(&self) -> bool {
        self.state
            .lock()
            .expect("forward admission lock is not poisoned")
            .generation
            == self.generation
    }
}

impl Drop for PortForwardReservation {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .expect("forward admission lock is not poisoned");
        if state.generation == self.generation {
            state.bindings.remove(&self.key);
            if state.bindings.is_empty() {
                state.bindings = HashSet::new();
            } else if state.bindings.capacity() > 32
                && state.bindings.len() <= state.bindings.capacity() / 4
            {
                let minimum = state.bindings.len().max(16);
                state.bindings.shrink_to(minimum);
            }
        }
    }
}

pub(crate) struct AcceptedLocalForwardConnection {
    key: PortForwardBindingKey,
    stream: tokio::net::TcpStream,
    originator_address: String,
    originator_port: u16,
}

pub(crate) struct ForwardedTcpIpConnection {
    pub(crate) key: PortForwardBindingKey,
    pub(crate) channel: russh::Channel<russh::client::Msg>,
}

#[derive(Clone)]
pub(crate) struct PortForwardAttemptLimiter {
    permits: Arc<tokio::sync::Semaphore>,
    max_in_flight: usize,
}

impl PortForwardAttemptLimiter {
    pub(crate) fn new(max_in_flight: usize) -> Self {
        assert!(
            max_in_flight > 0,
            "port-forward attempt limit must be nonzero"
        );
        Self {
            permits: Arc::new(tokio::sync::Semaphore::new(max_in_flight)),
            max_in_flight,
        }
    }

    fn max_in_flight(&self) -> usize {
        self.max_in_flight
    }

    pub(crate) fn try_acquire(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }
}

enum ActivePortForwardHandle {
    Inactive,
    Local {
        shutdown: tokio::sync::watch::Sender<bool>,
        listener: tokio::task::JoinHandle<()>,
    },
    Remote {
        shutdown: tokio::sync::watch::Sender<bool>,
    },
}

impl ActivePortForwardHandle {
    fn shutdown_receiver(&self) -> Option<tokio::sync::watch::Receiver<bool>> {
        match self {
            Self::Local { shutdown, .. } | Self::Remote { shutdown } => Some(shutdown.subscribe()),
            Self::Inactive => None,
        }
    }
}

struct ActivePortForward {
    requested: RequestedSshPortForward,
    runtime: SshPortForwardRuntime,
    handle: ActivePortForwardHandle,
    reservation: PortForwardReservation,
}

impl ActivePortForward {
    fn key(&self) -> &PortForwardBindingKey {
        &self.reservation.key
    }
}

pub(crate) struct PortForwardInventory {
    entries: Vec<ActivePortForward>,
    bindings: HashMap<PortForwardBindingKey, usize>,
    dirty: bool,
    pending_snapshot: Option<SessionEvent>,
    #[cfg(test)]
    snapshot_preparations: usize,
}

impl Default for PortForwardInventory {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            bindings: HashMap::new(),
            dirty: true,
            pending_snapshot: None,
            #[cfg(test)]
            snapshot_preparations: 0,
        }
    }
}

impl PortForwardInventory {
    fn insert(&mut self, forward: ActivePortForward) {
        assert!(self.entries.len() < MAX_SSH_PORT_FORWARD_ENTRIES);
        assert!(!self.bindings.contains_key(forward.key()));
        self.bindings
            .insert(forward.key().clone(), self.entries.len());
        self.entries.push(forward);
        self.dirty = true;
    }

    fn get(&self, key: &PortForwardBindingKey) -> Option<&ActivePortForward> {
        self.bindings.get(key).map(|index| &self.entries[*index])
    }

    fn remove(&mut self, key: &PortForwardBindingKey) -> Option<ActivePortForward> {
        let index = self.bindings.remove(key)?;
        let forward = self.entries.remove(index);
        for (index, entry) in self.entries.iter().enumerate().skip(index) {
            *self
                .bindings
                .get_mut(entry.key())
                .expect("indexed forward exists") = index;
        }
        self.dirty = true;
        if self.entries.is_empty() {
            self.entries = Vec::new();
            self.bindings = HashMap::new();
        } else if self.entries.capacity() > 32 && self.entries.len() <= self.entries.capacity() / 4
        {
            let minimum = self.entries.len().max(16);
            self.entries.shrink_to(minimum);
        }
        if self.bindings.capacity() > 32 && self.bindings.len() <= self.bindings.capacity() / 4 {
            self.bindings.shrink_to(self.entries.len().max(16));
        }
        Some(forward)
    }
}

pub(crate) fn emit_port_forward_snapshot(
    shared: &WorkerShared,
    inventory: &mut PortForwardInventory,
) {
    if inventory.dirty {
        inventory.pending_snapshot = Some(SessionEvent::PortForwardsUpdated(
            inventory
                .entries
                .iter()
                .map(|forward| forward.runtime.clone())
                .collect(),
        ));
        inventory.dirty = false;
        #[cfg(test)]
        {
            inventory.snapshot_preparations += 1;
        }
    }
    if let Some(event) = inventory.pending_snapshot.take() {
        inventory.pending_snapshot = shared.try_emit_retaining(event).err();
    }
}

pub(crate) fn report_port_forward_error(shared: &WorkerShared, message: impl Into<String>) {
    let message = message.into();
    tracing::warn!(target: "festerm::ssh", %message, "SSH port-forward operation failed");
    let _ = shared.try_emit(SessionEvent::Error(SessionError::new(
        SessionErrorKind::Internal,
        message,
    )));
}

pub(crate) async fn apply_port_forward(
    handle: &russh::client::Handle<SshClientHandler>,
    admitted: AdmittedPortForward,
    active_port_forwards: &mut PortForwardInventory,
    local_forward_sender: &tokio::sync::mpsc::Sender<AcceptedLocalForwardConnection>,
    shared: &Arc<WorkerShared>,
) {
    if !admitted.reservation.is_current() {
        report_port_forward_error(
            shared,
            "SSH port-forward request was canceled because its connection ended",
        );
        return;
    }
    let AdmittedPortForward {
        requested,
        reservation,
    } = admitted;

    let runtime_result = match requested.direction {
        SshPortForwardDirection::Local => {
            start_local_port_forward(&requested, local_forward_sender.clone(), Arc::clone(shared))
                .await
        }
        SshPortForwardDirection::Remote => start_remote_port_forward(handle, &requested).await,
    };

    let entry = match runtime_result {
        Ok(handle) => ActivePortForward {
            runtime: requested.runtime(SshPortForwardState::Active, None),
            requested,
            handle,
            reservation,
        },
        Err(reason) => ActivePortForward {
            runtime: requested.runtime(SshPortForwardState::Failed, Some(reason)),
            requested,
            handle: ActivePortForwardHandle::Inactive,
            reservation,
        },
    };
    active_port_forwards.insert(entry);
}

async fn start_local_port_forward(
    requested: &RequestedSshPortForward,
    local_forward_sender: tokio::sync::mpsc::Sender<AcceptedLocalForwardConnection>,
    shared: Arc<WorkerShared>,
) -> Result<ActivePortForwardHandle, String> {
    let listener =
        tokio::net::TcpListener::bind((requested.bind_host.as_str(), requested.bind_port))
            .await
            .map_err(|error| format!("could not bind local SSH port forward: {error}"))?;
    let key = PortForwardBindingKey {
        direction: requested.direction,
        bind_host: requested.bind_host.clone(),
        bind_port: requested.bind_port,
    };
    let (shutdown_sender, mut shutdown_receiver) = tokio::sync::watch::channel(false);
    let listener = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = shutdown_receiver.changed() => {
                    if changed.is_err() || *shutdown_receiver.borrow() {
                        break;
                    }
                }
                accepted = listener.accept() => match accepted {
                    Ok((stream, address)) => {
                        match local_forward_sender.try_send(AcceptedLocalForwardConnection {
                            key: key.clone(),
                            stream,
                            originator_address: address.ip().to_string(),
                            originator_port: address.port(),
                        }) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                report_port_forward_error(
                                    &shared,
                                    format!(
                                        "SSH local port forward on {}:{} rejected an accepted connection because {} connections were already pending",
                                        key.bind_host,
                                        key.bind_port,
                                        PORT_FORWARD_PENDING_CONNECTION_CAPACITY,
                                    ),
                                );
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    Err(_) => tokio::time::sleep(COMMAND_POLL_INTERVAL).await,
                }
            }
        }
    });
    Ok(ActivePortForwardHandle::Local {
        shutdown: shutdown_sender,
        listener,
    })
}

async fn start_remote_port_forward(
    handle: &russh::client::Handle<SshClientHandler>,
    requested: &RequestedSshPortForward,
) -> Result<ActivePortForwardHandle, String> {
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        handle.tcpip_forward(requested.bind_host.clone(), u32::from(requested.bind_port)),
    )
    .await
    {
        Ok(Ok(_)) => {
            let (shutdown_sender, _) = tokio::sync::watch::channel(false);
            Ok(ActivePortForwardHandle::Remote {
                shutdown: shutdown_sender,
            })
        }
        Ok(Err(error)) => Err(format!(
            "could not request remote SSH port forward: {error}"
        )),
        Err(_) => Err("requesting the remote SSH port forward timed out".to_owned()),
    }
}

pub(crate) async fn remove_port_forward(
    handle: &russh::client::Handle<SshClientHandler>,
    key: &PortForwardBindingKey,
    active_port_forwards: &mut PortForwardInventory,
    shared: &WorkerShared,
) {
    let Some(forward) = active_port_forwards.remove(key) else {
        report_port_forward_error(
            shared,
            format!(
                "SSH {} port forward on {}:{} does not exist",
                port_forward_direction_label(key.direction),
                key.bind_host,
                key.bind_port,
            ),
        );
        return;
    };
    stop_active_port_forward(Some(handle), forward).await;
}

pub(crate) async fn teardown_port_forwards(
    handle: Option<&russh::client::Handle<SshClientHandler>>,
    active_port_forwards: &mut PortForwardInventory,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
    shared: &WorkerShared,
) {
    while let Some(key) = active_port_forwards
        .entries
        .last()
        .map(|forward| forward.key().clone())
    {
        let forward = active_port_forwards
            .remove(&key)
            .expect("last forward exists");
        stop_active_port_forward(handle, forward).await;
    }
    port_forward_attempts.abort_all();
    while port_forward_attempts.join_next().await.is_some() {}
    emit_port_forward_snapshot(shared, active_port_forwards);
}

async fn stop_active_port_forward(
    handle: Option<&russh::client::Handle<SshClientHandler>>,
    forward: ActivePortForward,
) {
    match forward.handle {
        ActivePortForwardHandle::Inactive => {}
        ActivePortForwardHandle::Local { shutdown, listener } => {
            let _ = shutdown.send(true);
            let _ = listener.await;
        }
        ActivePortForwardHandle::Remote { shutdown } => {
            let _ = shutdown.send(true);
            if let Some(handle) = handle {
                let _ = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    handle.cancel_tcpip_forward(
                        forward.requested.bind_host,
                        u32::from(forward.requested.bind_port),
                    ),
                )
                .await;
            }
        }
    }
}

pub(crate) fn handle_local_forward_connection(
    handle: &Arc<russh::client::Handle<SshClientHandler>>,
    accepted: AcceptedLocalForwardConnection,
    active_port_forwards: &PortForwardInventory,
    shared: &Arc<WorkerShared>,
    attempt_limiter: &PortForwardAttemptLimiter,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
) {
    let Some(forward) = active_port_forwards
        .get(&accepted.key)
        .filter(|forward| forward.runtime.state() == SshPortForwardState::Active)
    else {
        return;
    };
    let Some(shutdown_receiver) = forward.handle.shutdown_receiver() else {
        return;
    };
    let Some(_permit) = attempt_limiter.try_acquire() else {
        report_port_forward_error(
            shared,
            format!(
                "SSH local port forward {}:{} -> {}:{} rejected a connection because {} forwarded connections were already in flight",
                forward.requested.bind_host,
                forward.requested.bind_port,
                forward.requested.destination_host,
                forward.requested.destination_port,
                attempt_limiter.max_in_flight(),
            ),
        );
        return;
    };
    let requested = forward.requested.clone();
    let handle = Arc::clone(handle);
    let shared = Arc::clone(shared);
    port_forward_attempts.spawn(async move {
        run_local_forward_connection(
            handle,
            accepted,
            requested,
            shutdown_receiver,
            shared,
            _permit,
        )
        .await;
    });
}

async fn run_local_forward_connection(
    handle: Arc<russh::client::Handle<SshClientHandler>>,
    accepted: AcceptedLocalForwardConnection,
    requested: RequestedSshPortForward,
    mut shutdown_receiver: tokio::sync::watch::Receiver<bool>,
    shared: Arc<WorkerShared>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let channel = tokio::select! {
        changed = shutdown_receiver.changed() => {
            let _ = changed;
            return;
        }
        result = tokio::time::timeout(
            CONNECT_TIMEOUT,
            handle.channel_open_direct_tcpip(
                requested.destination_host.clone(),
                u32::from(requested.destination_port),
                accepted.originator_address,
                u32::from(accepted.originator_port),
            ),
        ) => match result {
            Ok(Ok(channel)) => channel,
            Ok(Err(error)) => {
                report_port_forward_error(
                    &shared,
                    format!(
                        "SSH local port forward {}:{} -> {}:{} could not open a channel: {error}",
                        requested.bind_host,
                        requested.bind_port,
                        requested.destination_host,
                        requested.destination_port,
                    ),
                );
                return;
            }
            Err(_) => {
                report_port_forward_error(
                    &shared,
                    format!(
                        "SSH local port forward {}:{} -> {}:{} timed out while opening a channel",
                        requested.bind_host,
                        requested.bind_port,
                        requested.destination_host,
                        requested.destination_port,
                    ),
                );
                return;
            }
        }
    };
    bridge_tcp_and_ssh(accepted.stream, channel, shutdown_receiver).await;
}

pub(crate) fn handle_forwarded_tcpip_connection(
    forwarded: ForwardedTcpIpConnection,
    active_port_forwards: &PortForwardInventory,
    shared: &Arc<WorkerShared>,
    attempt_limiter: &PortForwardAttemptLimiter,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
) {
    let Some(forward) = active_port_forwards
        .get(&forwarded.key)
        .filter(|forward| forward.runtime.state() == SshPortForwardState::Active)
    else {
        port_forward_attempts.spawn(async move {
            let _ = forwarded.channel.close().await;
        });
        return;
    };
    let Some(shutdown_receiver) = forward.handle.shutdown_receiver() else {
        port_forward_attempts.spawn(async move {
            let _ = forwarded.channel.close().await;
        });
        return;
    };
    let Some(_permit) = attempt_limiter.try_acquire() else {
        report_port_forward_error(
            shared,
            format!(
                "SSH remote port forward {}:{} -> {}:{} rejected a connection because {} forwarded connections were already in flight",
                forward.requested.bind_host,
                forward.requested.bind_port,
                forward.requested.destination_host,
                forward.requested.destination_port,
                attempt_limiter.max_in_flight(),
            ),
        );
        port_forward_attempts.spawn(async move {
            let _ = forwarded.channel.close().await;
        });
        return;
    };
    let requested = forward.requested.clone();
    let shared = Arc::clone(shared);
    port_forward_attempts.spawn(async move {
        run_forwarded_tcpip_connection(forwarded, requested, shutdown_receiver, shared, _permit)
            .await;
    });
}

async fn run_forwarded_tcpip_connection(
    forwarded: ForwardedTcpIpConnection,
    requested: RequestedSshPortForward,
    mut shutdown_receiver: tokio::sync::watch::Receiver<bool>,
    shared: Arc<WorkerShared>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let stream = tokio::select! {
        changed = shutdown_receiver.changed() => {
            let _ = changed;
            let _ = forwarded.channel.close().await;
            return;
        }
        result = tokio::time::timeout(
            CONNECT_TIMEOUT,
            tokio::net::TcpStream::connect((
                requested.destination_host.as_str(),
                requested.destination_port,
            )),
        ) => match result {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                report_port_forward_error(
                    &shared,
                    format!(
                        "SSH remote port forward {}:{} -> {}:{} could not connect locally: {error}",
                        requested.bind_host,
                        requested.bind_port,
                        requested.destination_host,
                        requested.destination_port,
                    ),
                );
                let _ = forwarded.channel.close().await;
                return;
            }
            Err(_) => {
                report_port_forward_error(
                    &shared,
                    format!(
                        "SSH remote port forward {}:{} -> {}:{} timed out while connecting locally",
                        requested.bind_host,
                        requested.bind_port,
                        requested.destination_host,
                        requested.destination_port,
                    ),
                );
                let _ = forwarded.channel.close().await;
                return;
            }
        }
    };
    bridge_tcp_and_ssh(stream, forwarded.channel, shutdown_receiver).await;
}

async fn bridge_tcp_and_ssh(
    stream: tokio::net::TcpStream,
    channel: russh::Channel<russh::client::Msg>,
    mut shutdown_receiver: tokio::sync::watch::Receiver<bool>,
) {
    let mut stream = stream;
    let mut channel_stream = channel.into_stream();
    tokio::select! {
        result = tokio::io::copy_bidirectional(&mut stream, &mut channel_stream) => {
            let _ = result;
        }
        changed = shutdown_receiver.changed() => {
            let _ = changed;
        }
    }
    let _ = channel_stream.shutdown().await;
    let _ = stream.shutdown().await;
}

fn port_forward_direction_label(direction: SshPortForwardDirection) -> &'static str {
    match direction {
        SshPortForwardDirection::Local => "local",
        SshPortForwardDirection::Remote => "remote",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        noop_session_event_notifier, HostIdentity, SshConnectionProfile, SshWorkerFoundation,
        TerminalSize, WorkerCommandReceiver, DEFAULT_COMMAND_QUEUE_CAPACITY,
    };

    fn worker(event_capacity: usize) -> (SshWorkerFoundation, WorkerCommandReceiver) {
        let profile = SshConnectionProfile::new(
            HostIdentity::new("fixture.invalid", 22).unwrap(),
            "fixture",
            SshConnectionProfile::DEFAULT_TERMINAL_TYPE,
            TerminalSize::new(80, 24).unwrap(),
        )
        .unwrap();
        let (worker, receiver, _host_key, _password) = SshWorkerFoundation::new_with_capacities(
            profile,
            DEFAULT_COMMAND_QUEUE_CAPACITY,
            event_capacity,
            noop_session_event_notifier(),
        );
        (worker, receiver)
    }

    fn requested(bind_port: u16) -> RequestedSshPortForward {
        RequestedSshPortForward {
            direction: SshPortForwardDirection::Local,
            bind_host: "127.0.0.1".to_owned(),
            bind_port,
            destination_host: "127.0.0.1".to_owned(),
            destination_port: 9000,
            source: SshPortForwardSource::Ephemeral,
        }
    }

    fn insert_failed(
        inventory: &mut PortForwardInventory,
        shared: &WorkerShared,
        bind_port: u16,
    ) -> Result<(), SshPortForwardRequestError> {
        let AdmittedPortForward {
            requested,
            reservation,
        } = shared.port_forward_admissions.reserve(
            requested(bind_port),
            shared
                .transport_generation
                .load(std::sync::atomic::Ordering::Acquire),
        )?;
        inventory.insert(ActivePortForward {
            runtime: requested.runtime(
                SshPortForwardState::Failed,
                Some("fixture bind denied".into()),
            ),
            requested,
            handle: ActivePortForwardHandle::Inactive,
            reservation,
        });
        Ok(())
    }

    #[test]
    fn failed_port_forward_inventory_churn_plateaus_and_reclaims_capacity() {
        let (worker, _receiver) = worker(1);
        let mut inventory = PortForwardInventory::default();
        for port in 1..=128 {
            insert_failed(&mut inventory, &worker.shared, port).unwrap();
        }
        assert_eq!(
            insert_failed(&mut inventory, &worker.shared, 129),
            Err(SshPortForwardRequestError::InventoryFull)
        );
        assert!(inventory
            .entries
            .iter()
            .all(|entry| entry.runtime.state() == SshPortForwardState::Failed));
        for port in 129..=640 {
            let first = inventory.entries[0].key().clone();
            drop(inventory.remove(&first).unwrap());
            insert_failed(&mut inventory, &worker.shared, port).unwrap();
            assert_eq!(inventory.entries.len(), 128);
            assert_eq!(inventory.bindings.len(), 128);
        }
        while inventory.entries.len() > 1 {
            let key = inventory.entries[0].key().clone();
            drop(inventory.remove(&key).unwrap());
        }
        assert!(inventory.entries.capacity() <= 32);
        assert!(inventory.bindings.capacity() <= 32);
        assert!(
            worker
                .shared
                .port_forward_admissions
                .state
                .lock()
                .unwrap()
                .bindings
                .capacity()
                <= 32
        );
        while let Some(key) = inventory.entries.last().map(|entry| entry.key().clone()) {
            drop(inventory.remove(&key).unwrap());
        }
        assert_eq!(inventory.entries.capacity(), 0);
        assert_eq!(inventory.bindings.capacity(), 0);
        assert_eq!(
            worker
                .shared
                .port_forward_admissions
                .state
                .lock()
                .unwrap()
                .bindings
                .capacity(),
            0
        );
        insert_failed(&mut inventory, &worker.shared, 1).unwrap();
        assert!(inventory.entries.capacity() <= 16);
    }

    #[test]
    fn indexed_port_forward_removal_preserves_order_and_other_bindings() {
        let (worker, _receiver) = worker(1);
        let mut inventory = PortForwardInventory::default();
        for port in 1..=4 {
            insert_failed(&mut inventory, &worker.shared, port).unwrap();
        }
        drop(inventory.remove(&requested(2).binding_key()).unwrap());
        assert_eq!(
            inventory
                .entries
                .iter()
                .map(|entry| entry.requested.bind_port)
                .collect::<Vec<_>>(),
            vec![1, 3, 4]
        );
        for port in [1, 3, 4] {
            assert_eq!(
                inventory
                    .get(&requested(port).binding_key())
                    .unwrap()
                    .requested
                    .bind_port,
                port
            );
        }
        assert!(inventory.get(&requested(2).binding_key()).is_none());
    }

    #[test]
    fn stale_forward_reservation_cannot_release_a_new_generation_binding() {
        let admissions = PortForwardAdmissions::default();
        let old = admissions.reserve(requested(1), 0).unwrap();
        admissions.reset(1);
        assert!(!old.reservation.is_current());
        let current = admissions.reserve(requested(1), 1).unwrap();
        drop(old);
        assert!(current.reservation.is_current());
        assert!(matches!(
            admissions.reserve(requested(1), 1),
            Err(SshPortForwardRequestError::InvalidConfiguration(
                SshPortForwardConfigurationError::DuplicateBinding
            ))
        ));
        assert!(matches!(
            admissions.reserve(requested(2), 0),
            Err(SshPortForwardRequestError::NotRunning)
        ));
        drop(current);
        assert!(admissions.reserve(requested(1), 1).is_ok());
    }

    #[test]
    fn forward_reservation_is_released_only_after_local_owner_stops() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let admissions = PortForwardAdmissions::default();
                let AdmittedPortForward {
                    requested: request,
                    reservation,
                } = admissions.reserve(requested(1), 0).unwrap();
                let (shutdown, mut shutdown_receiver) = tokio::sync::watch::channel(false);
                let (observed, observation) = tokio::sync::oneshot::channel();
                let finish = Arc::new(tokio::sync::Notify::new());
                let listener_finish = Arc::clone(&finish);
                let listener = tokio::spawn(async move {
                    shutdown_receiver.changed().await.unwrap();
                    observed.send(()).unwrap();
                    listener_finish.notified().await;
                });
                let forward = ActivePortForward {
                    runtime: request.runtime(SshPortForwardState::Active, None),
                    requested: request,
                    handle: ActivePortForwardHandle::Local { shutdown, listener },
                    reservation,
                };
                let stop = tokio::spawn(stop_active_port_forward(None, forward));
                tokio::time::timeout(std::time::Duration::from_secs(3), observation)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(matches!(
                    admissions.reserve(requested(1), 0),
                    Err(SshPortForwardRequestError::InvalidConfiguration(
                        SshPortForwardConfigurationError::DuplicateBinding
                    ))
                ));
                assert!(!stop.is_finished());
                finish.notify_one();
                tokio::time::timeout(std::time::Duration::from_secs(3), stop)
                    .await
                    .unwrap()
                    .unwrap();
                assert!(admissions.reserve(requested(1), 0).is_ok());
            });
    }

    #[test]
    fn forward_snapshot_retries_without_recloning_and_coalesces_latest_state() {
        let (worker, _receiver) = worker(1);
        let mut inventory = PortForwardInventory::default();
        insert_failed(&mut inventory, &worker.shared, 1).unwrap();
        for _ in 0..1000 {
            emit_port_forward_snapshot(&worker.shared, &mut inventory);
        }
        assert_eq!(inventory.snapshot_preparations, 1);
        assert!(inventory.pending_snapshot.is_some());
        drop(inventory.remove(&requested(1).binding_key()).unwrap());
        insert_failed(&mut inventory, &worker.shared, 2).unwrap();
        emit_port_forward_snapshot(&worker.shared, &mut inventory);
        assert_eq!(inventory.snapshot_preparations, 2);
        assert!(matches!(
            worker.try_recv_event(),
            Ok(SessionEvent::Lifecycle(_))
        ));
        emit_port_forward_snapshot(&worker.shared, &mut inventory);
        let Ok(SessionEvent::PortForwardsUpdated(snapshot)) = worker.try_recv_event() else {
            panic!("the retained latest inventory must reach the frontend");
        };
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].bind_port(), 2);
        assert!(inventory.pending_snapshot.is_none());
        for _ in 0..1000 {
            emit_port_forward_snapshot(&worker.shared, &mut inventory);
        }
        assert_eq!(inventory.snapshot_preparations, 2);
        assert_eq!(
            worker.try_recv_event(),
            Err(crate::SessionTryReceiveError::Empty)
        );
        drop(inventory.remove(&requested(2).binding_key()).unwrap());
        emit_port_forward_snapshot(&worker.shared, &mut inventory);
        assert!(
            matches!(worker.try_recv_event(), Ok(SessionEvent::PortForwardsUpdated(snapshot)) if snapshot.is_empty())
        );
    }

    #[test]
    fn forward_snapshot_closed_receiver_does_not_rebuild_or_survive_owner_retirement() {
        let (worker, _receiver) = worker(1);
        let shared = Arc::clone(&worker.shared);
        let mut inventory = PortForwardInventory::default();
        insert_failed(&mut inventory, &shared, 1).unwrap();
        drop(worker);
        for _ in 0..1000 {
            emit_port_forward_snapshot(&shared, &mut inventory);
        }
        assert_eq!(inventory.snapshot_preparations, 1);
        assert!(inventory.pending_snapshot.is_some());
        drop(inventory);
        assert!(shared
            .port_forward_admissions
            .state
            .lock()
            .unwrap()
            .bindings
            .is_empty());
    }
}
