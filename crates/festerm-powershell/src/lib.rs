#![forbid(unsafe_code)]

use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use festerm_secret_store::SecretBytes;
use psrp_rs::{
    AuthMethod, Pipeline, PipelineEvent, PipelineState, PsObject, PsValue, RunspacePool,
    WinrmClient, WinrmConfig, WinrmCredentials, WinrmError, WinrmPsrpTransport,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PowerShellEndpoint {
    host: String,
    port: u16,
    trusted_ca_pem: Option<String>,
    local_bind_address: Option<IpAddr>,
}

impl PowerShellEndpoint {
    pub fn https(host: impl Into<String>) -> Result<Self, PowerShellSessionError> {
        let host = validate_host(host.into())?;
        Ok(Self {
            host,
            port: 5986,
            trusted_ca_pem: None,
            local_bind_address: None,
        })
    }

    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub fn with_trusted_ca_pem(mut self, pem: String) -> Result<Self, PowerShellSessionError> {
        if pem.trim().is_empty() {
            return Err(PowerShellSessionError::InvalidConfiguration(
                "trusted CA PEM must not be empty",
            ));
        }
        self.trusted_ca_pem = Some(pem);
        Ok(self)
    }

    pub fn with_local_bind_address(
        mut self,
        address: Option<IpAddr>,
    ) -> Result<Self, PowerShellSessionError> {
        validate_local_bind_address(address)?;
        self.local_bind_address = address;
        Ok(self)
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn trusted_ca_pem(&self) -> Option<&str> {
        self.trusted_ca_pem.as_deref()
    }

    #[must_use]
    pub fn local_bind_address(&self) -> Option<IpAddr> {
        self.local_bind_address
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerShellAuthMethod {
    Ntlm,
    Basic,
    Kerberos,
    Certificate,
    CredSsp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PowerShellOptions {
    pub auth: PowerShellAuthMethod,
    pub connect_timeout: Duration,
    pub operation_timeout: Duration,
    pub max_envelope_size: u32,
    pub max_output_bytes: usize,
    pub max_response_body_bytes: usize,
    pub max_error_body_bytes: usize,
    pub event_channel_capacity: usize,
    pub max_events_per_command: usize,
    pub max_expanded_value_nodes: usize,
    pub max_expanded_value_bytes: usize,
    pub cancel_drain_timeout: Duration,
    pub shutdown_timeout: Duration,
}

impl Default for PowerShellOptions {
    fn default() -> Self {
        Self {
            auth: PowerShellAuthMethod::Ntlm,
            connect_timeout: Duration::from_secs(10),
            operation_timeout: Duration::from_secs(30),
            max_envelope_size: 153_600,
            max_output_bytes: 4 * 1024 * 1024,
            max_response_body_bytes: 4 * 1024 * 1024,
            max_error_body_bytes: 64 * 1024,
            event_channel_capacity: 32,
            max_events_per_command: 512,
            max_expanded_value_nodes: 16_384,
            max_expanded_value_bytes: 2 * 1024 * 1024,
            cancel_drain_timeout: Duration::from_secs(5),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}

pub struct PowerShellCredentials {
    username: String,
    domain: Option<String>,
    password: SecretBytes,
}

impl PowerShellCredentials {
    pub fn new(
        username: impl Into<String>,
        password: SecretBytes,
    ) -> Result<Self, PowerShellSessionError> {
        Self::with_domain(username, None::<String>, password)
    }

    pub fn with_domain(
        username: impl Into<String>,
        domain: impl Into<Option<String>>,
        password: SecretBytes,
    ) -> Result<Self, PowerShellSessionError> {
        let username = username.into();
        if username.trim().is_empty() {
            return Err(PowerShellSessionError::InvalidConfiguration(
                "username must not be empty",
            ));
        }
        Ok(Self {
            username,
            domain: domain.into().filter(|value| !value.trim().is_empty()),
            password,
        })
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }
}

impl fmt::Debug for PowerShellCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PowerShellCredentials")
            .field("username", &self.username)
            .field("domain", &self.domain)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum PowerShellSessionError {
    #[error("invalid PowerShell configuration: {0}")]
    InvalidConfiguration(&'static str),
    #[error("unsupported PowerShell authentication mode: {0:?}")]
    UnsupportedAuth(PowerShellAuthMethod),
    #[error("PowerShell endpoint rejected the request")]
    AuthenticationRejected,
    #[error("PowerShell transport timed out")]
    TimedOut,
    #[error("PowerShell response exceeded the configured {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("PowerShell session was invalidated")]
    Invalidated,
    #[error("PowerShell session is closed")]
    Closed,
    #[error("PowerShell transport failed")]
    TransportFailure,
    #[error(
        "PowerShell connection failed with the configured local source address; check address availability and routing (no unbound fallback was attempted)"
    )]
    LocalBindConnectionFailed,
    #[error("PowerShell protocol failed")]
    ProtocolFailure,
    #[error("PowerShell worker failed to start")]
    WorkerStartFailed,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PowerShellCommandFailureKind {
    Cancelled,
    Stopped,
    PipelineFailed,
    BudgetExceeded,
    ChannelBackpressure,
    TimedOut,
    Invalidated,
    TransportFailure,
    ProtocolFailure,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PowerShellCommandFailure {
    pub kind: PowerShellCommandFailureKind,
    pub state: Option<PipelineState>,
    pub errors: Vec<PsValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PowerShellCommandMessage {
    Event(PipelineEvent),
    Completed { state: PipelineState },
    Failed(PowerShellCommandFailure),
}

impl PowerShellCommandMessage {
    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed { .. } | Self::Failed(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PowerShellSessionStatus {
    Connecting,
    Ready,
    Invalidated,
    Closed,
}

pub struct PowerShellCommand {
    receiver: Receiver<PowerShellCommandMessage>,
    cancel: CancellationToken,
    finished: bool,
}

impl PowerShellCommand {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn recv_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<PowerShellCommandMessage>, PowerShellSessionError> {
        if self.finished {
            return Ok(None);
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(message) => {
                if message.is_terminal() {
                    self.finished = true;
                }
                Ok(Some(message))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(PowerShellSessionError::TimedOut),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(PowerShellSessionError::Invalidated),
        }
    }
}

#[derive(Debug)]
pub struct PowerShellSession {
    sender: Mutex<Option<mpsc::Sender<SessionRequest>>>,
    status: Arc<Mutex<PowerShellSessionStatus>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    options: PowerShellOptions,
}

impl PowerShellSession {
    pub fn connect(
        endpoint: PowerShellEndpoint,
        options: PowerShellOptions,
        credentials: PowerShellCredentials,
    ) -> Result<Self, PowerShellSessionError> {
        validate_options(&options)?;
        if options.auth != PowerShellAuthMethod::Ntlm {
            return Err(PowerShellSessionError::UnsupportedAuth(options.auth));
        }
        let explicitly_bound = endpoint.local_bind_address.is_some();
        let status = Arc::new(Mutex::new(PowerShellSessionStatus::Connecting));
        let (request_tx, request_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_status = Arc::clone(&status);
        let worker_options = options.clone();
        let join = thread::Builder::new()
            .name("festerm-powershell".into())
            .spawn(move || {
                run_worker(
                    endpoint,
                    worker_options,
                    credentials,
                    request_rx,
                    ready_tx,
                    worker_status,
                )
            })
            .map_err(|_| PowerShellSessionError::WorkerStartFailed)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                sender: Mutex::new(Some(request_tx)),
                status,
                worker: Mutex::new(Some(join)),
                options,
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(
                    if explicitly_bound && error == PowerShellSessionError::TransportFailure {
                        PowerShellSessionError::LocalBindConnectionFailed
                    } else {
                        error
                    },
                )
            }
            Err(_) => {
                let _ = join.join();
                Err(PowerShellSessionError::WorkerStartFailed)
            }
        }
    }

    pub fn status(&self) -> PowerShellSessionStatus {
        *self.status.lock().expect("status mutex poisoned")
    }

    pub fn start_script(
        &self,
        script: impl Into<String>,
    ) -> Result<PowerShellCommand, PowerShellSessionError> {
        let script = script.into();
        if script.trim().is_empty() {
            return Err(PowerShellSessionError::InvalidConfiguration(
                "script must not be empty",
            ));
        }
        if self.status() != PowerShellSessionStatus::Ready {
            return Err(match self.status() {
                PowerShellSessionStatus::Invalidated => PowerShellSessionError::Invalidated,
                PowerShellSessionStatus::Closed => PowerShellSessionError::Closed,
                PowerShellSessionStatus::Connecting | PowerShellSessionStatus::Ready => {
                    PowerShellSessionError::TransportFailure
                }
            });
        }
        let (message_tx, message_rx) = mpsc::sync_channel(self.options.event_channel_capacity + 1);
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        let cancel = CancellationToken::new();
        let request = SessionRequest::RunScript {
            script,
            message_tx,
            cancel: cancel.clone(),
            ack_tx,
        };
        self.sender()
            .send(request)
            .map_err(|_| PowerShellSessionError::Invalidated)?;
        ack_rx
            .recv()
            .map_err(|_| PowerShellSessionError::Invalidated)??;
        Ok(PowerShellCommand {
            receiver: message_rx,
            cancel,
            finished: false,
        })
    }

    pub fn close(&self) -> Result<(), PowerShellSessionError> {
        let mut guard = self.sender.lock().expect("sender mutex poisoned");
        let Some(sender) = guard.take() else {
            if self.status() == PowerShellSessionStatus::Closed {
                return Ok(());
            }
            return Err(PowerShellSessionError::Closed);
        };
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        sender
            .send(SessionRequest::Close { ack_tx })
            .map_err(|_| PowerShellSessionError::Invalidated)?;
        let result = ack_rx
            .recv()
            .map_err(|_| PowerShellSessionError::Invalidated)?;
        if let Some(join) = self.worker.lock().expect("worker mutex poisoned").take() {
            let _ = join.join();
        }
        result
    }

    fn sender(&self) -> mpsc::Sender<SessionRequest> {
        self.sender
            .lock()
            .expect("sender mutex poisoned")
            .as_ref()
            .expect("sender available")
            .clone()
    }
}

enum SessionRequest {
    RunScript {
        script: String,
        message_tx: SyncSender<PowerShellCommandMessage>,
        cancel: CancellationToken,
        ack_tx: SyncSender<Result<(), PowerShellSessionError>>,
    },
    Close {
        ack_tx: SyncSender<Result<(), PowerShellSessionError>>,
    },
}

fn run_worker(
    endpoint: PowerShellEndpoint,
    options: PowerShellOptions,
    credentials: PowerShellCredentials,
    request_rx: Receiver<SessionRequest>,
    ready_tx: SyncSender<Result<(), PowerShellSessionError>>,
    status: Arc<Mutex<PowerShellSessionStatus>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            let _ = ready_tx.send(Err(PowerShellSessionError::WorkerStartFailed));
            *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Invalidated;
            return;
        }
    };

    let client = match build_client(&endpoint, &options, credentials) {
        Ok(client) => client,
        Err(error) => {
            let _ = ready_tx.send(Err(error.clone()));
            *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Invalidated;
            return;
        }
    };
    let open_result = runtime.block_on(async {
        let (rpid, creation) =
            RunspacePool::<WinrmPsrpTransport<'_>>::build_creation_fragments(1, 1)
                .map_err(|error| map_psrp_error(&error))?;
        let transport = WinrmPsrpTransport::open(&client, endpoint.host(), &creation)
            .await
            .map_err(|error| map_psrp_error(&error))?;
        RunspacePool::open_from_transport(transport, rpid, 1, 1)
            .await
            .map_err(|error| map_psrp_error(&error))
    });
    let mut pool = match open_result {
        Ok(pool) => pool,
        Err(error) => {
            let _ = ready_tx.send(Err(error.clone()));
            *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Invalidated;
            return;
        }
    };
    *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Ready;
    let _ = ready_tx.send(Ok(()));

    while let Ok(request) = request_rx.recv() {
        match request {
            SessionRequest::RunScript {
                script,
                message_tx,
                cancel,
                ack_tx,
            } => {
                let started =
                    runtime.block_on(async { Pipeline::new(script).start(&mut pool).await });
                match started {
                    Ok(handle) => {
                        let _ = ack_tx.send(Ok(()));
                        let health = runtime.block_on(drive_pipeline(
                            handle, message_tx, cancel, &options, &status,
                        ));
                        if matches!(health, WorkerHealth::Invalidated) {
                            *status.lock().expect("status mutex poisoned") =
                                PowerShellSessionStatus::Invalidated;
                            break;
                        }
                    }
                    Err(error) => {
                        let mapped = map_psrp_error(&error);
                        let _ = ack_tx.send(Err(mapped.clone()));
                        if invalidates_psrp(&error) {
                            *status.lock().expect("status mutex poisoned") =
                                PowerShellSessionStatus::Invalidated;
                            break;
                        }
                    }
                }
            }
            SessionRequest::Close { ack_tx } => {
                let result = runtime.block_on(async {
                    match tokio::time::timeout(options.shutdown_timeout, pool.close()).await {
                        Ok(Ok(())) => Ok(()),
                        Ok(Err(error)) => Err(map_psrp_error(&error)),
                        Err(_) => Err(PowerShellSessionError::TimedOut),
                    }
                });
                *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Closed;
                let _ = ack_tx.send(result);
                return;
            }
        }
    }

    let _ = runtime
        .block_on(async { tokio::time::timeout(options.shutdown_timeout, pool.close()).await });
    *status.lock().expect("status mutex poisoned") = PowerShellSessionStatus::Invalidated;
}

fn build_client(
    endpoint: &PowerShellEndpoint,
    options: &PowerShellOptions,
    credentials: PowerShellCredentials,
) -> Result<WinrmClient, PowerShellSessionError> {
    let mut password = credentials
        .password
        .with_bytes(|bytes| String::from_utf8(bytes.to_vec()))
        .map_err(|_| {
            PowerShellSessionError::InvalidConfiguration("password must be valid UTF-8")
        })?;
    let client = WinrmClient::new(
        WinrmConfig {
            port: endpoint.port,
            use_tls: true,
            accept_invalid_certs: false,
            trusted_ca_pem: endpoint.trusted_ca_pem.clone(),
            local_bind_address: endpoint.local_bind_address,
            connect_timeout_secs: options.connect_timeout.as_secs(),
            operation_timeout_secs: options.operation_timeout.as_secs(),
            auth_method: AuthMethod::Ntlm,
            max_envelope_size: options.max_envelope_size,
            max_output_bytes: Some(options.max_output_bytes),
            max_response_body_bytes: options.max_response_body_bytes,
            max_error_body_bytes: options.max_error_body_bytes,
            ..WinrmConfig::default()
        },
        WinrmCredentials::new(
            credentials.username,
            std::mem::take(&mut password),
            credentials.domain.unwrap_or_default(),
        ),
    )
    .map_err(|error| map_winrm_error(&error))?;
    password.zeroize();
    Ok(client)
}

enum WorkerHealth {
    Healthy,
    Invalidated,
}

async fn drive_pipeline(
    mut handle: psrp_rs::PipelineHandle<'_, WinrmPsrpTransport<'_>>,
    message_tx: SyncSender<PowerShellCommandMessage>,
    cancel: CancellationToken,
    options: &PowerShellOptions,
    status: &Arc<Mutex<PowerShellSessionStatus>>,
) -> WorkerHealth {
    let mut budget = CommandBudget::new(options);
    let mut pipeline_errors = Vec::new();
    let mut deferred_failure: Option<PowerShellCommandFailureKind> = None;
    let mut deadline: Option<Instant> = None;

    loop {
        let event = {
            let mut next_event = Box::pin(handle.next_event_with_cancel(cancel.clone()));
            match wait_for_pipeline_event(next_event.as_mut(), &cancel, &mut deadline, options)
                .await
            {
                NextPipelineEvent::Event(Ok(event)) => event,
                NextPipelineEvent::Event(Err(error)) => {
                    let cancel_requested = cancel.is_cancelled();
                    let kind = failure_kind_for_pipeline_error(&error, cancel_requested);
                    let invalidates = pipeline_error_invalidates_worker(&error, cancel_requested);
                    if invalidates {
                        *status.lock().expect("status mutex poisoned") =
                            PowerShellSessionStatus::Invalidated;
                    }
                    let _ = message_tx.try_send(PowerShellCommandMessage::Failed(
                        PowerShellCommandFailure {
                            kind,
                            state: None,
                            errors: pipeline_errors,
                        },
                    ));
                    return if invalidates {
                        WorkerHealth::Invalidated
                    } else {
                        WorkerHealth::Healthy
                    };
                }
                NextPipelineEvent::DeadlineElapsed => {
                    *status.lock().expect("status mutex poisoned") =
                        PowerShellSessionStatus::Invalidated;
                    let _ = message_tx.try_send(PowerShellCommandMessage::Failed(
                        PowerShellCommandFailure {
                            kind: timeout_failure_kind(
                                cancel.is_cancelled(),
                                deferred_failure.as_ref(),
                            ),
                            state: None,
                            errors: pipeline_errors,
                        },
                    ));
                    return WorkerHealth::Invalidated;
                }
            }
        };

        if let Err(kind) = budget.note_event(&event) {
            deferred_failure = Some(kind);
            let _ = handle.stop().await;
            cancel.cancel();
            note_cancel_deadline(&mut deadline, options.cancel_drain_timeout);
            continue;
        }

        if let PipelineEvent::Error(value) = &event {
            pipeline_errors.push(value.clone());
        }

        if send_event(&message_tx, PowerShellCommandMessage::Event(event.clone())).is_err() {
            let _ = handle.stop().await;
            return WorkerHealth::Invalidated;
        }

        if let PipelineEvent::State(state) = event {
            if !state.is_terminal() {
                continue;
            }
            if let Some(kind) = deferred_failure {
                let _ = message_tx.try_send(PowerShellCommandMessage::Failed(
                    PowerShellCommandFailure {
                        kind,
                        state: Some(state),
                        errors: pipeline_errors,
                    },
                ));
                return WorkerHealth::Healthy;
            }
            let terminal = match state {
                PipelineState::Completed => PowerShellCommandMessage::Completed { state },
                PipelineState::Failed => {
                    PowerShellCommandMessage::Failed(PowerShellCommandFailure {
                        kind: PowerShellCommandFailureKind::PipelineFailed,
                        state: Some(state),
                        errors: pipeline_errors,
                    })
                }
                PipelineState::Stopped if cancel.is_cancelled() => {
                    PowerShellCommandMessage::Failed(PowerShellCommandFailure {
                        kind: PowerShellCommandFailureKind::Cancelled,
                        state: Some(state),
                        errors: pipeline_errors,
                    })
                }
                PipelineState::Stopped => {
                    PowerShellCommandMessage::Failed(PowerShellCommandFailure {
                        kind: PowerShellCommandFailureKind::Stopped,
                        state: Some(state),
                        errors: pipeline_errors,
                    })
                }
                PipelineState::Disconnected | PipelineState::Unknown => {
                    PowerShellCommandMessage::Failed(PowerShellCommandFailure {
                        kind: PowerShellCommandFailureKind::Invalidated,
                        state: Some(state),
                        errors: pipeline_errors,
                    })
                }
                PipelineState::NotStarted | PipelineState::Running | PipelineState::Stopping => {
                    continue;
                }
            };
            let invalidated = matches!(
                terminal,
                PowerShellCommandMessage::Failed(PowerShellCommandFailure {
                    kind: PowerShellCommandFailureKind::Invalidated,
                    ..
                })
            );
            if invalidated {
                *status.lock().expect("status mutex poisoned") =
                    PowerShellSessionStatus::Invalidated;
            }
            let _ = message_tx.try_send(terminal);
            return if invalidated {
                WorkerHealth::Invalidated
            } else {
                WorkerHealth::Healthy
            };
        }
    }
}

enum NextPipelineEvent {
    Event(Result<PipelineEvent, psrp_rs::PsrpError>),
    DeadlineElapsed,
}

async fn wait_for_pipeline_event<F>(
    mut next_event: Pin<&mut F>,
    cancel: &CancellationToken,
    deadline: &mut Option<Instant>,
    options: &PowerShellOptions,
) -> NextPipelineEvent
where
    F: Future<Output = Result<PipelineEvent, psrp_rs::PsrpError>>,
{
    loop {
        if cancel.is_cancelled() {
            note_cancel_deadline(deadline, options.cancel_drain_timeout);
        }

        let wait = deadline
            .map(|limit| limit.saturating_duration_since(Instant::now()))
            .unwrap_or(options.operation_timeout + options.cancel_drain_timeout);
        if wait.is_zero() {
            return NextPipelineEvent::DeadlineElapsed;
        }

        let sleep = tokio::time::sleep(wait);
        tokio::pin!(sleep);

        tokio::select! {
            biased;
            () = cancel.cancelled(), if deadline.is_none() => {
                note_cancel_deadline(deadline, options.cancel_drain_timeout);
            }
            result = next_event.as_mut() => return NextPipelineEvent::Event(result),
            _ = &mut sleep => return NextPipelineEvent::DeadlineElapsed,
        }
    }
}

fn note_cancel_deadline(deadline: &mut Option<Instant>, cancel_drain_timeout: Duration) {
    if deadline.is_none() {
        *deadline = Some(Instant::now() + cancel_drain_timeout);
    }
}

fn timeout_failure_kind(
    cancel_requested: bool,
    deferred_failure: Option<&PowerShellCommandFailureKind>,
) -> PowerShellCommandFailureKind {
    if cancel_requested {
        PowerShellCommandFailureKind::Invalidated
    } else {
        deferred_failure
            .cloned()
            .unwrap_or(PowerShellCommandFailureKind::TimedOut)
    }
}

fn send_event(
    sender: &SyncSender<PowerShellCommandMessage>,
    message: PowerShellCommandMessage,
) -> Result<(), ()> {
    sender.try_send(message).map_err(|error| match error {
        TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
    })
}

struct CommandBudget {
    max_events: usize,
    max_nodes: usize,
    max_bytes: usize,
    events: usize,
    nodes: usize,
    bytes: usize,
}

impl CommandBudget {
    fn new(options: &PowerShellOptions) -> Self {
        Self {
            max_events: options.max_events_per_command,
            max_nodes: options.max_expanded_value_nodes,
            max_bytes: options.max_expanded_value_bytes,
            events: 0,
            nodes: 0,
            bytes: 0,
        }
    }

    fn note_event(&mut self, event: &PipelineEvent) -> Result<(), PowerShellCommandFailureKind> {
        self.events = self
            .events
            .checked_add(1)
            .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
        if self.events > self.max_events {
            return Err(PowerShellCommandFailureKind::BudgetExceeded);
        }
        if let Some(value) = event_value(event) {
            let cost = measure_value(value)?;
            self.nodes = self
                .nodes
                .checked_add(cost.nodes)
                .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
            self.bytes = self
                .bytes
                .checked_add(cost.bytes)
                .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
            if self.nodes > self.max_nodes || self.bytes > self.max_bytes {
                return Err(PowerShellCommandFailureKind::BudgetExceeded);
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct ValueMeasure {
    nodes: usize,
    bytes: usize,
}

fn measure_value(value: &PsValue) -> Result<ValueMeasure, PowerShellCommandFailureKind> {
    fn add(
        total: &mut ValueMeasure,
        next: ValueMeasure,
    ) -> Result<(), PowerShellCommandFailureKind> {
        total.nodes = total
            .nodes
            .checked_add(next.nodes)
            .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
        total.bytes = total
            .bytes
            .checked_add(next.bytes)
            .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
        Ok(())
    }
    let mut total = ValueMeasure { nodes: 1, bytes: 0 };
    match value {
        PsValue::Null
        | PsValue::Bool(_)
        | PsValue::I8(_)
        | PsValue::U8(_)
        | PsValue::I16(_)
        | PsValue::U16(_)
        | PsValue::I32(_)
        | PsValue::U32(_)
        | PsValue::I64(_)
        | PsValue::U64(_)
        | PsValue::F32(_)
        | PsValue::Double(_)
        | PsValue::Char(_)
        | PsValue::Guid(_) => {}
        PsValue::Decimal(text)
        | PsValue::String(text)
        | PsValue::DateTime(text)
        | PsValue::Duration(text)
        | PsValue::Version(text)
        | PsValue::Uri(text)
        | PsValue::Xml(text)
        | PsValue::ScriptBlock(text)
        | PsValue::SecureString(text) => total.bytes = text.len(),
        PsValue::Bytes(bytes) => total.bytes = bytes.len(),
        PsValue::List(values) => {
            for value in values {
                add(&mut total, measure_value(value)?)?;
            }
        }
        PsValue::Dict(entries) => {
            for (key, value) in entries {
                add(&mut total, measure_value(key)?)?;
                add(&mut total, measure_value(value)?)?;
            }
        }
        PsValue::Object(object) => {
            total.bytes = object.type_names.iter().map(String::len).sum::<usize>()
                + object.to_string.as_ref().map_or(0, String::len);
            for (name, value) in &object.properties {
                total.bytes = total
                    .bytes
                    .checked_add(name.len())
                    .ok_or(PowerShellCommandFailureKind::BudgetExceeded)?;
                add(&mut total, measure_value(value)?)?;
            }
        }
    }
    Ok(total)
}

fn event_value(event: &PipelineEvent) -> Option<&PsValue> {
    match event {
        PipelineEvent::Output(value)
        | PipelineEvent::Error(value)
        | PipelineEvent::Warning(value)
        | PipelineEvent::Verbose(value)
        | PipelineEvent::Debug(value)
        | PipelineEvent::Information(value)
        | PipelineEvent::Progress(value) => Some(value),
        PipelineEvent::State(_) => None,
    }
}

fn validate_host(host: String) -> Result<String, PowerShellSessionError> {
    if host.trim().is_empty() || host.contains('/') || host.contains('@') || host.contains("://") {
        return Err(PowerShellSessionError::InvalidConfiguration(
            "endpoint host must be a bare hostname or IP literal",
        ));
    }
    Ok(host)
}

fn validate_local_bind_address(address: Option<IpAddr>) -> Result<(), PowerShellSessionError> {
    let Some(address) = address else {
        return Ok(());
    };
    if address.is_unspecified() {
        return Err(PowerShellSessionError::InvalidConfiguration(
            "local bind address must not be unspecified",
        ));
    }
    if address.is_multicast() {
        return Err(PowerShellSessionError::InvalidConfiguration(
            "local bind address must not be multicast",
        ));
    }
    if let IpAddr::V6(address) = address {
        if address.to_ipv4_mapped().is_some() {
            return Err(PowerShellSessionError::InvalidConfiguration(
                "IPv4-mapped IPv6 local bind addresses are unsupported",
            ));
        }
        if (address.segments()[0] & 0xffc0) == 0xfe80 {
            return Err(PowerShellSessionError::InvalidConfiguration(
                "IPv6 link-local local bind addresses require a scope ID, which is unsupported",
            ));
        }
    }
    Ok(())
}

fn validate_options(options: &PowerShellOptions) -> Result<(), PowerShellSessionError> {
    if options.event_channel_capacity == 0
        || options.max_events_per_command == 0
        || options.max_expanded_value_nodes == 0
        || options.max_expanded_value_bytes == 0
    {
        return Err(PowerShellSessionError::InvalidConfiguration(
            "bounds must be greater than zero",
        ));
    }
    Ok(())
}

fn map_failure_kind(error: &psrp_rs::PsrpError) -> PowerShellCommandFailureKind {
    match map_psrp_error(error) {
        PowerShellSessionError::TimedOut => PowerShellCommandFailureKind::TimedOut,
        PowerShellSessionError::Invalidated => PowerShellCommandFailureKind::Invalidated,
        PowerShellSessionError::TransportFailure
        | PowerShellSessionError::LocalBindConnectionFailed => {
            PowerShellCommandFailureKind::TransportFailure
        }
        PowerShellSessionError::ProtocolFailure => PowerShellCommandFailureKind::ProtocolFailure,
        PowerShellSessionError::AuthenticationRejected
        | PowerShellSessionError::Closed
        | PowerShellSessionError::InvalidConfiguration(_)
        | PowerShellSessionError::ResponseTooLarge { .. }
        | PowerShellSessionError::UnsupportedAuth(_)
        | PowerShellSessionError::WorkerStartFailed => {
            PowerShellCommandFailureKind::ProtocolFailure
        }
    }
}

fn failure_kind_for_pipeline_error(
    error: &psrp_rs::PsrpError,
    cancel_requested: bool,
) -> PowerShellCommandFailureKind {
    if cancel_requested {
        return match error {
            psrp_rs::PsrpError::Cancelled | psrp_rs::PsrpError::Stopped => {
                PowerShellCommandFailureKind::Cancelled
            }
            _ => PowerShellCommandFailureKind::Invalidated,
        };
    }
    map_failure_kind(error)
}

fn pipeline_error_invalidates_worker(error: &psrp_rs::PsrpError, cancel_requested: bool) -> bool {
    if cancel_requested {
        !matches!(
            error,
            psrp_rs::PsrpError::Cancelled | psrp_rs::PsrpError::Stopped
        )
    } else {
        invalidates_psrp(error)
    }
}

fn invalidates_psrp(error: &psrp_rs::PsrpError) -> bool {
    matches!(
        error,
        psrp_rs::PsrpError::Winrm(WinrmError::Timeout(_)) | psrp_rs::PsrpError::Cancelled
    )
}

fn map_psrp_error(error: &psrp_rs::PsrpError) -> PowerShellSessionError {
    match error {
        psrp_rs::PsrpError::Winrm(error) => map_winrm_error(error),
        psrp_rs::PsrpError::Cancelled => PowerShellSessionError::TimedOut,
        psrp_rs::PsrpError::Stopped
        | psrp_rs::PsrpError::BadState { .. }
        | psrp_rs::PsrpError::Protocol(_)
        | psrp_rs::PsrpError::Clixml(_)
        | psrp_rs::PsrpError::Fragment(_)
        | psrp_rs::PsrpError::PipelineFailed(_) => PowerShellSessionError::ProtocolFailure,
    }
}

fn map_winrm_error(error: &WinrmError) -> PowerShellSessionError {
    match error {
        WinrmError::AuthFailed(_) => PowerShellSessionError::AuthenticationRejected,
        WinrmError::Timeout(_) | WinrmError::Cancelled => PowerShellSessionError::TimedOut,
        WinrmError::ResponseTooLarge { limit } => {
            PowerShellSessionError::ResponseTooLarge { limit: *limit }
        }
        WinrmError::Http(_) | WinrmError::Transfer(_) => PowerShellSessionError::TransportFailure,
        WinrmError::Soap(_) | WinrmError::Ntlm(_) | WinrmError::CredSsp(_) => {
            PowerShellSessionError::ProtocolFailure
        }
    }
}

pub fn value_to_display_text(value: &PsValue) -> String {
    match value {
        PsValue::Null => String::from("$null"),
        PsValue::Bool(value) => value.to_string(),
        PsValue::I8(value) => value.to_string(),
        PsValue::U8(value) => value.to_string(),
        PsValue::I16(value) => value.to_string(),
        PsValue::U16(value) => value.to_string(),
        PsValue::I32(value) => value.to_string(),
        PsValue::U32(value) => value.to_string(),
        PsValue::I64(value) => value.to_string(),
        PsValue::U64(value) => value.to_string(),
        PsValue::F32(value) => value.to_string(),
        PsValue::Double(value) => value.to_string(),
        PsValue::Decimal(value)
        | PsValue::String(value)
        | PsValue::DateTime(value)
        | PsValue::Duration(value)
        | PsValue::Version(value)
        | PsValue::Uri(value)
        | PsValue::Xml(value)
        | PsValue::ScriptBlock(value)
        | PsValue::SecureString(value) => value.clone(),
        PsValue::Char(value) => value.to_string(),
        PsValue::Bytes(bytes) => format!("<{} bytes>", bytes.len()),
        PsValue::Guid(value) => value.to_string(),
        PsValue::List(values) => format!("{:?}", values),
        PsValue::Dict(values) => format!("{:?}", values),
        PsValue::Object(object) => object
            .to_string
            .clone()
            .or_else(|| {
                object
                    .get("ToString")
                    .and_then(PsValue::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("{:?}", object.properties)),
    }
}

pub fn host_object(name: &str, value: &str) -> PsValue {
    PsValue::Object(PsObject::new().with(name, PsValue::String(value.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_not_impl_any;
    use std::future::poll_fn;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn credentials_debug_redacts_password() {
        let creds =
            PowerShellCredentials::new("alice", SecretBytes::copy_from_slice(b"super-secret"))
                .unwrap();
        let rendered = format!("{creds:?}");
        assert!(rendered.contains("[REDACTED]"));
        assert!(!rendered.contains("super-secret"));
    }

    #[test]
    fn unsupported_auth_is_rejected_before_network_use() {
        let endpoint = PowerShellEndpoint::https("win.local").unwrap();
        let error = PowerShellSession::connect(
            endpoint,
            PowerShellOptions {
                auth: PowerShellAuthMethod::Kerberos,
                ..PowerShellOptions::default()
            },
            PowerShellCredentials::new("alice", SecretBytes::copy_from_slice(b"pw")).unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            PowerShellSessionError::UnsupportedAuth(PowerShellAuthMethod::Kerberos)
        );
    }

    #[test]
    fn endpoint_validation_rejects_url_shaped_hosts() {
        assert!(PowerShellEndpoint::https("https://win.local/wsman").is_err());
        assert!(PowerShellEndpoint::https("user@win.local").is_err());
    }

    #[test]
    fn endpoint_local_bind_address_defaults_to_none_and_can_be_set() {
        let endpoint = PowerShellEndpoint::https("win.local").unwrap();
        assert_eq!(endpoint.local_bind_address(), None);

        let endpoint = endpoint
            .with_local_bind_address(Some(IpAddr::from([127, 0, 0, 2])))
            .unwrap();
        assert_eq!(
            endpoint.local_bind_address(),
            Some(IpAddr::from([127, 0, 0, 2]))
        );

        let endpoint = endpoint.with_local_bind_address(None).unwrap();
        assert_eq!(endpoint.local_bind_address(), None);
    }

    #[test]
    fn endpoint_local_bind_address_rejects_unsupported_sources() {
        for address in [
            IpAddr::from([0, 0, 0, 0]),
            IpAddr::from([224, 0, 0, 1]),
            IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
            IpAddr::V6(std::net::Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(std::net::Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V6(std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0)),
            IpAddr::V6(std::net::Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 1)),
        ] {
            assert!(
                PowerShellEndpoint::https("win.local")
                    .unwrap()
                    .with_local_bind_address(Some(address))
                    .is_err(),
                "{address} should be rejected"
            );
        }
    }

    #[test]
    fn endpoint_local_bind_failure_is_explicit_and_never_connects_unbound() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = PowerShellEndpoint::https("127.0.0.1")
            .unwrap()
            .with_port(listener.local_addr().unwrap().port())
            .with_local_bind_address(Some(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)))
            .unwrap();
        let credentials =
            PowerShellCredentials::new("fixture", SecretBytes::copy_from_slice(b"fixture"))
                .unwrap();
        let options = PowerShellOptions {
            connect_timeout: Duration::from_secs(1),
            operation_timeout: Duration::from_secs(1),
            ..PowerShellOptions::default()
        };
        let error = PowerShellSession::connect(endpoint, options, credentials).unwrap_err();
        assert_eq!(error, PowerShellSessionError::LocalBindConnectionFailed);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn empty_trusted_ca_pem_is_rejected() {
        let endpoint = PowerShellEndpoint::https("win.local").unwrap();
        assert!(endpoint.with_trusted_ca_pem("   ".into()).is_err());
    }

    #[test]
    fn zero_bounds_are_rejected_before_worker_start() {
        let error = PowerShellSession::connect(
            PowerShellEndpoint::https("win.local").unwrap(),
            PowerShellOptions {
                event_channel_capacity: 0,
                ..PowerShellOptions::default()
            },
            PowerShellCredentials::new("alice", SecretBytes::copy_from_slice(b"pw")).unwrap(),
        )
        .unwrap_err();
        assert_eq!(
            error,
            PowerShellSessionError::InvalidConfiguration("bounds must be greater than zero")
        );
    }

    #[test]
    fn command_budget_rejects_reference_amplification_like_growth() {
        let mut budget = CommandBudget::new(&PowerShellOptions {
            max_expanded_value_nodes: 4,
            max_expanded_value_bytes: 32,
            ..PowerShellOptions::default()
        });
        let value = PsValue::List(vec![
            PsValue::String("one".into()),
            PsValue::String("two".into()),
        ]);
        assert!(budget
            .note_event(&PipelineEvent::Output(value.clone()))
            .is_ok());
        assert_eq!(
            budget
                .note_event(&PipelineEvent::Output(value))
                .unwrap_err(),
            PowerShellCommandFailureKind::BudgetExceeded
        );
    }

    #[test]
    fn map_winrm_error_redacts_response_details() {
        let mapped = map_winrm_error(&WinrmError::AuthFailed("bad password for alice".into()));
        assert_eq!(mapped, PowerShellSessionError::AuthenticationRejected);
        assert_eq!(
            map_winrm_error(&WinrmError::ResponseTooLarge { limit: 1024 }),
            PowerShellSessionError::ResponseTooLarge { limit: 1024 }
        );
        assert_not_impl_any!(PowerShellCredentials: Clone);
    }

    #[test]
    fn value_to_display_text_prefers_object_to_string_property() {
        let object =
            PsObject::new().with("ToString", PsValue::String("formatted output".to_string()));
        assert_eq!(
            value_to_display_text(&PsValue::Object(object)),
            "formatted output"
        );
    }

    #[test]
    fn cancel_drain_deadline_applies_to_in_flight_wait() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PowerShellOptions {
                operation_timeout: Duration::from_millis(250),
                cancel_drain_timeout: Duration::from_millis(40),
                ..PowerShellOptions::default()
            };
            let cancel = CancellationToken::new();
            let started = Arc::new(AtomicBool::new(false));
            let started_for_future = Arc::clone(&started);
            let mut deadline = None;
            let mut next_event = Box::pin(poll_fn(move |_| {
                started_for_future.store(true, Ordering::SeqCst);
                std::task::Poll::<Result<PipelineEvent, psrp_rs::PsrpError>>::Pending
            }));

            let cancel_task = {
                let started = Arc::clone(&started);
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    while !started.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    cancel.cancel();
                })
            };

            let start = Instant::now();
            let result = tokio::time::timeout(
                Duration::from_millis(200),
                wait_for_pipeline_event(next_event.as_mut(), &cancel, &mut deadline, &options),
            )
            .await
            .expect("cancel drain regression must stay bounded");
            let elapsed = start.elapsed();
            cancel_task.await.unwrap();

            assert!(matches!(result, NextPipelineEvent::DeadlineElapsed));
            assert!(deadline.is_some());
            assert!(
                elapsed < Duration::from_millis(150),
                "cancel drain waited too long: {elapsed:?}"
            );
        });
    }

    #[test]
    fn cancellation_timeout_becomes_invalidated() {
        assert_eq!(
            timeout_failure_kind(true, None),
            PowerShellCommandFailureKind::Invalidated
        );
    }

    #[test]
    fn cancellation_protocol_error_invalidates_session() {
        let error = psrp_rs::PsrpError::Protocol("receive aborted after signal".into());
        assert_eq!(
            failure_kind_for_pipeline_error(&error, true),
            PowerShellCommandFailureKind::Invalidated
        );
        assert!(pipeline_error_invalidates_worker(&error, true));
    }

    #[test]
    fn acknowledged_cancellation_does_not_invalidate_session() {
        let error = psrp_rs::PsrpError::Stopped;
        assert_eq!(
            failure_kind_for_pipeline_error(&error, true),
            PowerShellCommandFailureKind::Cancelled
        );
        assert!(!pipeline_error_invalidates_worker(&error, true));
    }
}
