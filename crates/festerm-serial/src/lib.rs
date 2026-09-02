//! Local serial-port session backend built on [`serialport`].
//!
//! A serial port is a platform-exclusive character device, not a spawned
//! process or a network channel: there is no "connected" handshake, no
//! multiplexed channels, and no way to notify a peer that the terminal grid
//! resized (see ADR 0023). This crate never imports or mutates
//! `festerm-core`; callers drain [`SessionEvent`] values and decide when to
//! ingest terminal bytes, exactly like `festerm-pty` and `festerm-ssh`.

use std::{
    fmt,
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

use festerm_session::{
    noop_session_event_notifier, Session, SessionError, SessionErrorKind, SessionEvent,
    SessionEventNotifier, SessionId, SessionLifecycle, SessionMetrics, SessionOperation,
    SessionSendError, SessionTryReceiveError, ShutdownError, ShutdownResult, TerminalSize,
    DEFAULT_COMMAND_QUEUE_CAPACITY, DEFAULT_EVENT_QUEUE_CAPACITY, MAX_IO_CHUNK_BYTES,
};

/// How often the reader thread's blocking read times out to check for
/// shutdown. This is purely an internal responsiveness knob; it never
/// produces a `SessionEvent` or counts as backend-observed traffic.
const READ_POLL_TIMEOUT: Duration = Duration::from_millis(200);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const READER_STOP_TIMEOUT: Duration = Duration::from_secs(2);

/// A discovered serial device's exact system identifier and, when the OS
/// supplies one, a friendly description.
///
/// `identifier` is always the exact string needed to reopen this device
/// (`COM3`, `/dev/ttyUSB0`, `/dev/cu.usbserial-1410`, ...). fesTerm never
/// fabricates or reformats it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredPort {
    identifier: String,
    description: Option<String>,
}

impl DiscoveredPort {
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

/// Failure to enumerate available serial devices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialDiscoveryError {
    message: String,
}

impl fmt::Display for SerialDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SerialDiscoveryError {}

/// Lists discoverable serial devices with their exact system identifiers.
///
/// A device missing from this list can still be opened by typing its exact
/// identifier explicitly (`docs/gui-design.md`'s device picker allows both).
/// This function never fabricates an identifier; on error it reports a
/// concise, content-free failure rather than an empty list, so the UI can
/// distinguish "no devices found" from "could not enumerate devices".
pub fn discover_ports() -> Result<Vec<DiscoveredPort>, SerialDiscoveryError> {
    serialport::available_ports()
        .map(|ports| {
            ports
                .into_iter()
                .map(|port| DiscoveredPort {
                    identifier: port.port_name,
                    description: port_description(&port.port_type),
                })
                .collect()
        })
        .map_err(|error| SerialDiscoveryError {
            message: format!("could not enumerate serial devices: {error}"),
        })
}

fn port_description(port_type: &serialport::SerialPortType) -> Option<String> {
    match port_type {
        serialport::SerialPortType::UsbPort(info) => {
            let product = info.product.as_deref().unwrap_or("USB serial device");
            match &info.manufacturer {
                Some(manufacturer) => Some(format!("{manufacturer} {product}")),
                None => Some(product.to_owned()),
            }
        }
        serialport::SerialPortType::BluetoothPort => Some("Bluetooth serial device".to_owned()),
        serialport::SerialPortType::PciPort => Some("PCI serial device".to_owned()),
        serialport::SerialPortType::Unknown => None,
    }
}

/// Number of data bits transmitted per character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataBits {
    Five,
    Six,
    Seven,
    Eight,
}

/// Parity checking applied to each character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Parity {
    None,
    Odd,
    Even,
}

/// Number of stop bits terminating each character.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopBits {
    One,
    Two,
}

/// Flow-control strategy between fesTerm and the attached device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

impl From<DataBits> for serialport::DataBits {
    fn from(value: DataBits) -> Self {
        match value {
            DataBits::Five => Self::Five,
            DataBits::Six => Self::Six,
            DataBits::Seven => Self::Seven,
            DataBits::Eight => Self::Eight,
        }
    }
}

impl From<Parity> for serialport::Parity {
    fn from(value: Parity) -> Self {
        match value {
            Parity::None => Self::None,
            Parity::Odd => Self::Odd,
            Parity::Even => Self::Even,
        }
    }
}

impl From<StopBits> for serialport::StopBits {
    fn from(value: StopBits) -> Self {
        match value {
            StopBits::One => Self::One,
            StopBits::Two => Self::Two,
        }
    }
}

impl From<FlowControl> for serialport::FlowControl {
    fn from(value: FlowControl) -> Self {
        match value {
            FlowControl::None => Self::None,
            FlowControl::Software => Self::Software,
            FlowControl::Hardware => Self::Hardware,
        }
    }
}

/// Line settings applied when opening a serial device.
///
/// Defaults match `docs/gui-design.md`'s Serial connection form: 115200
/// baud, 8 data bits, no parity, 1 stop bit, no flow control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineSettings {
    device: String,
    baud_rate: u32,
    data_bits: DataBits,
    parity: Parity,
    stop_bits: StopBits,
    flow_control: FlowControl,
}

/// A line setting with a value the backend cannot use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LineSettingsError {
    EmptyDevice,
    ZeroBaudRate,
}

impl fmt::Display for LineSettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDevice => formatter.write_str("a serial device identifier is required"),
            Self::ZeroBaudRate => formatter.write_str("baud rate must be greater than zero"),
        }
    }
}

impl std::error::Error for LineSettingsError {}

impl LineSettings {
    pub fn new(
        device: impl Into<String>,
        baud_rate: u32,
        data_bits: DataBits,
        parity: Parity,
        stop_bits: StopBits,
        flow_control: FlowControl,
    ) -> Result<Self, LineSettingsError> {
        let device = device.into();
        if device.trim().is_empty() {
            return Err(LineSettingsError::EmptyDevice);
        }
        if baud_rate == 0 {
            return Err(LineSettingsError::ZeroBaudRate);
        }
        Ok(Self {
            device,
            baud_rate,
            data_bits,
            parity,
            stop_bits,
            flow_control,
        })
    }

    /// The application's documented default line settings: 115200 baud, 8
    /// data bits, no parity, 1 stop bit, no flow control.
    pub fn with_defaults(device: impl Into<String>) -> Result<Self, LineSettingsError> {
        Self::new(
            device,
            115_200,
            DataBits::Eight,
            Parity::None,
            StopBits::One,
            FlowControl::None,
        )
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub const fn baud_rate(&self) -> u32 {
        self.baud_rate
    }

    pub const fn data_bits(&self) -> DataBits {
        self.data_bits
    }

    pub const fn parity(&self) -> Parity {
        self.parity
    }

    pub const fn stop_bits(&self) -> StopBits {
        self.stop_bits
    }

    pub const fn flow_control(&self) -> FlowControl {
        self.flow_control
    }
}

/// Errors returned while opening a serial session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SerialSessionError {
    message: String,
}

impl SerialSessionError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SerialSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SerialSessionError {}

enum SessionCommand {
    Input(Vec<u8>),
    Shutdown,
}

struct Shared {
    id: SessionId,
    lifecycle: Mutex<SessionLifecycle>,
    metrics: Mutex<SessionMetrics>,
    event_sender: SyncSender<SessionEvent>,
    event_notifier: Arc<dyn SessionEventNotifier>,
    cancel: AtomicBool,
    completion_receiver: Mutex<Receiver<Result<ShutdownResult, SessionError>>>,
    completion: Mutex<Option<Result<ShutdownResult, SessionError>>>,
}

impl Shared {
    fn lifecycle(&self) -> SessionLifecycle {
        self.lifecycle
            .lock()
            .expect("session lifecycle lock is not poisoned")
            .clone()
    }

    fn set_lifecycle(&self, lifecycle: SessionLifecycle) {
        *self
            .lifecycle
            .lock()
            .expect("session lifecycle lock is not poisoned") = lifecycle.clone();
        let _ = self.try_emit(SessionEvent::Lifecycle(lifecycle));
    }

    fn record_error(&self, error: SessionError) {
        {
            let mut metrics = self
                .metrics
                .lock()
                .expect("session metrics lock is not poisoned");
            metrics.error_count = metrics.error_count.saturating_add(1);
        }
        let _ = self.try_emit(SessionEvent::Error(error));
    }

    fn fail(&self, error: SessionError) {
        self.record_error(error.clone());
        self.set_lifecycle(SessionLifecycle::Failed(error));
    }

    fn try_emit(&self, event: SessionEvent) -> bool {
        let mut metrics = self
            .metrics
            .lock()
            .expect("session metrics lock is not poisoned");
        match self.event_sender.try_send(event) {
            Ok(()) => {
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_add(1);
                metrics.event_queue_high_watermark = metrics
                    .event_queue_high_watermark
                    .max(metrics.event_queue_depth);
                drop(metrics);
                self.event_notifier.notify();
                true
            }
            Err(TrySendError::Full(_)) => {
                metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
                false
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    fn emit_output(&self, bytes: Vec<u8>) -> bool {
        let output_len = bytes.len();
        let mut output_event = SessionEvent::Output(bytes);
        let mut backpressure_pending = false;
        let mut backpressure_event_sent = false;
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return false;
            }

            if backpressure_pending && !backpressure_event_sent {
                let metrics = self.metrics();
                if self.try_emit(SessionEvent::Backpressure {
                    direction: festerm_session::FlowDirection::Output,
                    queued: metrics.event_queue_depth,
                    capacity: metrics.event_queue_capacity,
                }) {
                    backpressure_event_sent = true;
                    continue;
                }
            }

            let mut metrics = self
                .metrics
                .lock()
                .expect("session metrics lock is not poisoned");
            match self.event_sender.try_send(output_event) {
                Ok(()) => {
                    metrics.output_bytes = metrics.output_bytes.saturating_add(output_len as u64);
                    metrics.event_queue_depth = metrics.event_queue_depth.saturating_add(1);
                    metrics.event_queue_high_watermark = metrics
                        .event_queue_high_watermark
                        .max(metrics.event_queue_depth);
                    drop(metrics);
                    self.event_notifier.notify();
                    return true;
                }
                Err(TrySendError::Full(event)) => {
                    output_event = event;
                    metrics.backpressure_count = metrics.backpressure_count.saturating_add(1);
                    drop(metrics);
                    backpressure_pending = true;
                    thread::sleep(Duration::from_millis(5));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }

    fn metrics(&self) -> SessionMetrics {
        *self
            .metrics
            .lock()
            .expect("session metrics lock is not poisoned")
    }

    fn request_stop(&self) {
        let lifecycle = self.lifecycle();
        if matches!(lifecycle, SessionLifecycle::Stopped) {
            return;
        }
        if matches!(
            lifecycle,
            SessionLifecycle::Starting | SessionLifecycle::Running
        ) {
            self.set_lifecycle(SessionLifecycle::Stopping);
        }
        self.cancel.store(true, Ordering::Release);
    }

    fn complete(
        &self,
        result: Result<ShutdownResult, SessionError>,
        sender: &SyncSender<Result<ShutdownResult, SessionError>>,
    ) {
        let _ = sender.send(result);
    }
}

/// A native serial-port session driven by bounded worker queues.
///
/// Unlike `festerm-pty` and `festerm-ssh`, there is no child process or
/// remote peer: opening the device is the entire "start" step, and an
/// unexpected close is reported as `Failed`, never `Disconnected` (ADR
/// 0023) since a serial port has no liveness/reconnect protocol to recover.
pub struct SerialSession {
    shared: Arc<Shared>,
    command_sender: SyncSender<SessionCommand>,
    event_receiver: Mutex<Receiver<SessionEvent>>,
}

impl SerialSession {
    pub fn open(settings: LineSettings) -> Result<Self, SerialSessionError> {
        Self::open_with_notifier(settings, noop_session_event_notifier())
    }

    /// Opens a serial session and wakes `notifier` whenever an event arrives.
    pub fn open_with_notifier(
        settings: LineSettings,
        event_notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, SerialSessionError> {
        let port = serialport::new(settings.device.clone(), settings.baud_rate)
            .data_bits(settings.data_bits.into())
            .parity(settings.parity.into())
            .stop_bits(settings.stop_bits.into())
            .flow_control(settings.flow_control.into())
            .timeout(READ_POLL_TIMEOUT)
            .open()
            .map_err(|error| {
                SerialSessionError::new(format!(
                    "could not open serial device {}: {error}",
                    settings.device
                ))
            })?;
        let reader = port.try_clone().map_err(|error| {
            SerialSessionError::new(format!("could not open serial device reader: {error}"))
        })?;

        let (event_sender, event_receiver) = mpsc::sync_channel(DEFAULT_EVENT_QUEUE_CAPACITY);
        let (command_sender, command_receiver) = mpsc::sync_channel(DEFAULT_COMMAND_QUEUE_CAPACITY);
        let (completion_sender, completion_receiver) = mpsc::sync_channel(1);
        let (reader_done_sender, reader_done_receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            id: SessionId::next(),
            lifecycle: Mutex::new(SessionLifecycle::Starting),
            metrics: Mutex::new(SessionMetrics {
                event_queue_capacity: DEFAULT_EVENT_QUEUE_CAPACITY,
                ..SessionMetrics::default()
            }),
            event_sender,
            event_notifier,
            cancel: AtomicBool::new(false),
            completion_receiver: Mutex::new(completion_receiver),
            completion: Mutex::new(None),
        });
        shared.set_lifecycle(SessionLifecycle::Starting);
        shared.set_lifecycle(SessionLifecycle::Running);

        let control_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("festerm-serial-control-{}", shared.id))
            .spawn(move || {
                control_worker(
                    control_shared,
                    command_receiver,
                    port,
                    reader_done_receiver,
                    completion_sender,
                );
            })
            .map_err(|error| {
                shared.request_stop();
                SerialSessionError::new(format!("could not start serial control worker: {error}"))
            })?;

        let reader_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name(format!("festerm-serial-reader-{}", shared.id))
            .spawn(move || reader_worker(reader_shared, reader, reader_done_sender))
            .map_err(|error| {
                shared.request_stop();
                SerialSessionError::new(format!("could not start serial reader worker: {error}"))
            })?;

        Ok(Self {
            shared,
            command_sender,
            event_receiver: Mutex::new(event_receiver),
        })
    }
}

impl Session for SerialSession {
    fn id(&self) -> SessionId {
        self.shared.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        self.shared.lifecycle()
    }

    fn metrics(&self) -> SessionMetrics {
        self.shared.metrics()
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > MAX_IO_CHUNK_BYTES {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: MAX_IO_CHUNK_BYTES,
                actual: bytes.len(),
            });
        }
        send_command(
            &self.command_sender,
            SessionCommand::Input(bytes.to_vec()),
            SessionOperation::Input,
        )
    }

    /// A serial port has no way to inform its peer of terminal dimensions
    /// (ADR 0023): this always succeeds immediately as a local-only fact
    /// without touching the device.
    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        {
            let mut metrics = self
                .shared
                .metrics
                .lock()
                .expect("session metrics lock is not poisoned");
            metrics.resize_count = metrics.resize_count.saturating_add(1);
        }
        let _ = self.shared.try_emit(SessionEvent::ResizeApplied(size));
        Ok(())
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        self.shared.request_stop();
        send_command(
            &self.command_sender,
            SessionCommand::Shutdown,
            SessionOperation::Shutdown,
        )
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        match self
            .event_receiver
            .lock()
            .expect("session event receiver lock is not poisoned")
            .try_recv()
        {
            Ok(event) => {
                let mut metrics = self
                    .shared
                    .metrics
                    .lock()
                    .expect("session metrics lock is not poisoned");
                metrics.event_queue_depth = metrics.event_queue_depth.saturating_sub(1);
                Ok(event)
            }
            Err(TryRecvError::Empty) => Err(SessionTryReceiveError::Empty),
            Err(TryRecvError::Disconnected) => Err(SessionTryReceiveError::Closed),
        }
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        let _ = self.try_shutdown();
        let mut completion = self
            .shared
            .completion
            .lock()
            .expect("session completion lock is not poisoned");
        if let Some(result) = completion.clone() {
            return result.map_err(ShutdownError::Failed);
        }
        match self
            .shared
            .completion_receiver
            .lock()
            .expect("session completion receiver lock is not poisoned")
            .recv_timeout(timeout)
        {
            Ok(result) => {
                *completion = Some(result.clone());
                result.map_err(ShutdownError::Failed)
            }
            Err(RecvTimeoutError::Timeout) => Err(ShutdownError::TimedOut { timeout }),
            Err(RecvTimeoutError::Disconnected) => Err(ShutdownError::Failed(SessionError::new(
                SessionErrorKind::Shutdown,
                "serial worker ended without reporting shutdown completion",
            ))),
        }
    }
}

impl Drop for SerialSession {
    fn drop(&mut self) {
        // Never block destructors. Explicit `Session::shutdown` performs the
        // bounded wait; this wake-up keeps a detached worker from retaining
        // an open port after the application drops its last handle.
        let _ = self.try_shutdown();
    }
}

fn send_command(
    sender: &SyncSender<SessionCommand>,
    command: SessionCommand,
    operation: SessionOperation,
) -> Result<(), SessionSendError> {
    match sender.try_send(command) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(_)) => Err(SessionSendError::Full {
            operation,
            capacity: DEFAULT_COMMAND_QUEUE_CAPACITY,
        }),
        Err(TrySendError::Disconnected(_)) => Err(SessionSendError::Closed { operation }),
    }
}

/// Whether a blocking read's error is only the configured poll timeout
/// firing with no bytes available, rather than a real device failure (e.g.
/// the adapter was unplugged).
fn is_read_timeout(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::TimedOut
}

fn reader_worker(
    shared: Arc<Shared>,
    mut reader: Box<dyn serialport::SerialPort>,
    done_sender: SyncSender<Result<(), SessionError>>,
) {
    let mut buffer = vec![0_u8; MAX_IO_CHUNK_BYTES.min(16 * 1024)];
    let result = loop {
        if shared.cancel.load(Ordering::Acquire) {
            break Ok(());
        }
        match reader.read(&mut buffer) {
            Ok(0) => {
                if shared.cancel.load(Ordering::Acquire) {
                    break Ok(());
                }
                let error = SessionError::new(
                    SessionErrorKind::Output,
                    "serial device closed unexpectedly",
                );
                shared.record_error(error.clone());
                shared.cancel.store(true, Ordering::Release);
                break Err(error);
            }
            Ok(read) => {
                if !shared.emit_output(buffer[..read].to_vec()) {
                    break Ok(());
                }
            }
            Err(error) if is_read_timeout(&error) => {
                // Only a responsiveness poll; not a device fact.
                continue;
            }
            Err(error) => {
                if shared.cancel.load(Ordering::Acquire) {
                    break Ok(());
                }
                let error = SessionError::new(
                    SessionErrorKind::Output,
                    format!("serial read failed: {error}"),
                );
                shared.record_error(error.clone());
                shared.cancel.store(true, Ordering::Release);
                break Err(error);
            }
        }
    };
    // Close this thread's cloned file descriptor *before* signaling
    // completion. `serialport` opens ports exclusively by default (TIOCEXCL
    // plus an advisory flock), and that lock is only released once every fd
    // referencing the device is closed. `control_worker` forwards this
    // signal straight into `SerialSession::shutdown`'s return value, so if
    // `reader` were dropped after sending (i.e. implicitly at function
    // return), a caller reopening the same device immediately after
    // `shutdown()` returns could race the OS into still reporting the
    // device busy. Dropping here makes the fd closure a strict
    // happens-before of shutdown's completion.
    drop(reader);
    let _ = done_sender.send(result);
}

fn control_worker(
    shared: Arc<Shared>,
    command_receiver: Receiver<SessionCommand>,
    mut writer: Box<dyn serialport::SerialPort>,
    reader_done_receiver: Receiver<Result<(), SessionError>>,
    completion_sender: SyncSender<Result<ShutdownResult, SessionError>>,
) {
    let mut worker_failure = None;
    loop {
        if shared.cancel.load(Ordering::Acquire) {
            break;
        }

        match command_receiver.recv_timeout(POLL_INTERVAL) {
            Ok(SessionCommand::Input(bytes)) => {
                if let Err(error) = writer.write_all(&bytes).and_then(|()| writer.flush()) {
                    let error = SessionError::new(
                        SessionErrorKind::Input,
                        format!("serial write failed: {error}"),
                    );
                    shared.fail(error.clone());
                    worker_failure = Some(error);
                    shared.cancel.store(true, Ordering::Release);
                    break;
                }
                let mut metrics = shared
                    .metrics
                    .lock()
                    .expect("session metrics lock is not poisoned");
                metrics.input_bytes = metrics.input_bytes.saturating_add(bytes.len() as u64);
            }
            Ok(SessionCommand::Shutdown) => {
                shared.cancel.store(true, Ordering::Release);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                shared.cancel.store(true, Ordering::Release);
                break;
            }
        }
    }

    drop(writer);
    let reader_result = reader_done_receiver.recv_timeout(READER_STOP_TIMEOUT);
    let result = if let Some(error) = worker_failure {
        Err(error)
    } else {
        match reader_result {
            Ok(Ok(())) => {
                shared.set_lifecycle(SessionLifecycle::Stopped);
                Ok(ShutdownResult::Stopped)
            }
            Ok(Err(error)) => {
                // The reader records its failure before waking the control
                // worker, so only the terminal lifecycle remains to publish.
                shared.set_lifecycle(SessionLifecycle::Failed(error.clone()));
                Err(error)
            }
            Err(RecvTimeoutError::Timeout) => {
                let error = SessionError::new(
                    SessionErrorKind::Shutdown,
                    "serial reader did not stop within the bounded shutdown interval",
                );
                shared.fail(error.clone());
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let error = SessionError::new(
                    SessionErrorKind::Internal,
                    "serial reader ended without reporting completion",
                );
                shared.fail(error.clone());
                Err(error)
            }
        }
    };
    shared.complete(result, &completion_sender);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_settings_reject_an_empty_device_or_zero_baud() {
        assert_eq!(
            LineSettings::with_defaults("   ").unwrap_err(),
            LineSettingsError::EmptyDevice
        );
        assert_eq!(
            LineSettings::new(
                "/dev/ttyUSB0",
                0,
                DataBits::Eight,
                Parity::None,
                StopBits::One,
                FlowControl::None,
            )
            .unwrap_err(),
            LineSettingsError::ZeroBaudRate
        );
    }

    #[test]
    fn default_line_settings_match_the_documented_gui_defaults() {
        let settings = LineSettings::with_defaults("/dev/ttyUSB0").unwrap();
        assert_eq!(settings.device(), "/dev/ttyUSB0");
        assert_eq!(settings.baud_rate(), 115_200);
        assert_eq!(settings.data_bits(), DataBits::Eight);
        assert_eq!(settings.parity(), Parity::None);
        assert_eq!(settings.stop_bits(), StopBits::One);
        assert_eq!(settings.flow_control(), FlowControl::None);
    }

    #[test]
    fn opening_a_nonexistent_device_reports_a_concise_error() {
        let device = "/dev/festerm-serial-test-device-that-does-not-exist-0001";
        let settings = LineSettings::with_defaults(device).unwrap();
        let Err(error) = SerialSession::open(settings) else {
            panic!("opening a nonexistent serial device must fail");
        };
        let message = error.to_string();
        assert!(message.starts_with("could not open serial device"));
        assert!(message.contains(device));
    }

    #[cfg(unix)]
    #[test]
    fn opening_an_existing_non_tty_reports_an_open_failure() {
        let device = std::env::current_exe()
            .expect("the test executable has a path")
            .to_string_lossy()
            .into_owned();
        let settings = LineSettings::with_defaults(device.clone()).unwrap();
        let Err(error) = SerialSession::open(settings) else {
            panic!("opening a regular file as a serial device must fail");
        };
        let message = error.to_string();
        assert!(message.starts_with("could not open serial device"));
        assert!(message.contains(&device));
    }

    #[test]
    fn discovery_never_panics_and_reports_exact_identifiers() {
        // No assumption about what hardware is attached to the test runner:
        // this only proves discovery completes and, when it finds anything,
        // that identifiers are non-empty verbatim system strings rather than
        // a fabricated placeholder.
        let ports = discover_ports().expect("enumeration itself should not fail on CI runners");
        for port in ports {
            assert!(!port.identifier().is_empty());
        }
    }
}
