//! SSH local and remote TCP/IP port-forward configuration and runtime.

use std::{fmt, sync::Arc};

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
        }
    }
}

impl std::error::Error for SshPortForwardConfigurationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SshPortForwardRequestError {
    NotRunning,
    QueueFull,
    Closed,
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
    for forward in port_forwards {
        let requested = requested_port_forward(forward, source)?;
        if port_forward_bindings_collide(&collected, &requested) {
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

fn port_forward_bindings_collide(
    existing: &[RequestedSshPortForward],
    candidate: &RequestedSshPortForward,
) -> bool {
    existing.iter().any(|forward| {
        forward.direction == candidate.direction
            && forward.bind_host == candidate.bind_host
            && forward.bind_port == candidate.bind_port
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PortForwardBindingKey {
    pub(crate) direction: SshPortForwardDirection,
    pub(crate) bind_host: String,
    pub(crate) bind_port: u16,
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

pub(crate) struct ActivePortForward {
    requested: RequestedSshPortForward,
    runtime: SshPortForwardRuntime,
    handle: ActivePortForwardHandle,
}

impl ActivePortForward {
    fn key(&self) -> PortForwardBindingKey {
        PortForwardBindingKey {
            direction: self.requested.direction,
            bind_host: self.requested.bind_host.clone(),
            bind_port: self.requested.bind_port,
        }
    }
}

pub(crate) fn emit_port_forward_snapshot(
    shared: &WorkerShared,
    active_port_forwards: &[ActivePortForward],
) {
    let snapshot = active_port_forwards
        .iter()
        .map(|forward| forward.runtime.clone())
        .collect();
    let _ = shared.try_emit(SessionEvent::PortForwardsUpdated(snapshot));
}

pub(crate) fn report_port_forward_error(shared: &WorkerShared, message: impl Into<String>) {
    let _ = shared.try_emit(SessionEvent::Error(SessionError::new(
        SessionErrorKind::Internal,
        message,
    )));
}

pub(crate) async fn apply_port_forward(
    handle: &russh::client::Handle<SshClientHandler>,
    requested: RequestedSshPortForward,
    active_port_forwards: &mut Vec<ActivePortForward>,
    local_forward_sender: &tokio::sync::mpsc::Sender<AcceptedLocalForwardConnection>,
    shared: &Arc<WorkerShared>,
) {
    if active_port_forwards.iter().any(|forward| {
        forward.key()
            == PortForwardBindingKey {
                direction: requested.direction,
                bind_host: requested.bind_host.clone(),
                bind_port: requested.bind_port,
            }
    }) {
        report_port_forward_error(
            shared,
            format!(
                "SSH {} port forward on {}:{} already exists",
                port_forward_direction_label(requested.direction),
                requested.bind_host,
                requested.bind_port,
            ),
        );
        emit_port_forward_snapshot(shared, active_port_forwards);
        return;
    }

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
        },
        Err(reason) => ActivePortForward {
            runtime: requested.runtime(SshPortForwardState::Failed, Some(reason)),
            requested,
            handle: ActivePortForwardHandle::Inactive,
        },
    };
    active_port_forwards.push(entry);
    emit_port_forward_snapshot(shared, active_port_forwards);
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
    active_port_forwards: &mut Vec<ActivePortForward>,
    shared: &WorkerShared,
) {
    let Some(index) = active_port_forwards
        .iter()
        .position(|forward| forward.key() == *key)
    else {
        report_port_forward_error(
            shared,
            format!(
                "SSH {} port forward on {}:{} does not exist",
                port_forward_direction_label(key.direction),
                key.bind_host,
                key.bind_port,
            ),
        );
        emit_port_forward_snapshot(shared, active_port_forwards);
        return;
    };
    let forward = active_port_forwards.remove(index);
    stop_active_port_forward(Some(handle), forward).await;
    emit_port_forward_snapshot(shared, active_port_forwards);
}

pub(crate) async fn teardown_port_forwards(
    handle: Option<&russh::client::Handle<SshClientHandler>>,
    active_port_forwards: &mut Vec<ActivePortForward>,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
    shared: &WorkerShared,
) {
    while let Some(forward) = active_port_forwards.pop() {
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
    active_port_forwards: &mut [ActivePortForward],
    shared: &Arc<WorkerShared>,
    attempt_limiter: &PortForwardAttemptLimiter,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
) {
    let Some(forward) = active_port_forwards.iter_mut().find(|forward| {
        forward.runtime.state() == SshPortForwardState::Active && forward.key() == accepted.key
    }) else {
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
    active_port_forwards: &mut [ActivePortForward],
    shared: &Arc<WorkerShared>,
    attempt_limiter: &PortForwardAttemptLimiter,
    port_forward_attempts: &mut tokio::task::JoinSet<()>,
) {
    let Some(forward) = active_port_forwards.iter_mut().find(|forward| {
        forward.runtime.state() == SshPortForwardState::Active && forward.key() == forwarded.key
    }) else {
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
