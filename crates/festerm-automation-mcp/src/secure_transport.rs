//! Owned, bounded I/O for an experimental direct local MCP channel.
//!
//! Noise NN authenticates neither endpoint. The supplied native verifier is a
//! trust boundary, not a wire protocol: it must independently display this
//! endpoint's *complete* fingerprint and obtain the peer's complete value
//! through a trusted human comparison. Values obtained from MCP, stdio, this
//! socket, or this endpoint's own display cannot satisfy that contract. No
//! native verifier is qualified by this module.
//!
//! After each local comparison, the endpoints exchange encrypted, role-bound
//! confirmations and receipts. Application frames cannot be sent or returned
//! until both confirmations have been received and the local receipt was
//! physically written. This proves neither executable identity nor that a
//! malicious peer actually used a trusted human interface.
//!
//! The stream must be exclusively owned and configured for nonblocking reads
//! and writes, and its destructor must not block. A successful write means OS
//! acceptance, not peer receipt or proof that the intended client is still
//! alive. The admission callback must return a guard held through the
//! physical write, and must validate the exact connection, grant, sources,
//! privacy state, generations and deadline applicable to the pending frame. A
//! busy guard may return `WouldBlock`; denial, cancellation, failure or panic
//! consumes the stream. If its destructor fails, disposal is uncertain.
//!
//! Catching an unwind does not suppress the process-wide panic hook, which
//! runs first and may record or print its payload. Error values contain no
//! panic payload, but this module cannot guarantee hook-path confidentiality.
//! Supplied callbacks, guards and destructors must not panic with sensitive
//! messages; their interaction with the installed hook remains unqualified.

use std::{
    fmt, io,
    io::{Read, Write},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::{
    secure_channel::UnverifiedChannel,
    secure_handshake_io::{HandshakeIo, Interest, ReadProgress},
    secure_record_io::{RecordIo, WriteProgress},
    MAX_MESSAGE_BYTES,
};

pub const FINGERPRINT_TEXT_BYTES: usize = 64;
pub const MAX_BOOTSTRAP_DURATION: Duration = Duration::from_secs(60);
pub const MAX_FRAME_DURATION: Duration = Duration::from_secs(2);
pub const MAX_TRANSPORT_LIFETIME: Duration = Duration::from_secs(600);
const MAX_BOOTSTRAP_POLLS: usize = 65_536;
const MAX_TOTAL_POLLS: usize = 4_194_304;
const MAX_FRAME_ATTEMPTS: usize = MAX_MESSAGE_BYTES * 4;
const READ_CHUNK: usize = 4096;

const CLIENT_CONFIRM: &[u8] = b"fesTerm direct MCP v1 client confirmed";
const APP_CONFIRM: &[u8] = b"fesTerm direct MCP v1 application confirmed";
const CLIENT_RECEIPT: &[u8] = b"fesTerm direct MCP v1 client received confirmation";
const APP_RECEIPT: &[u8] = b"fesTerm direct MCP v1 application received confirmation";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Client,
    Application,
}

impl Role {
    fn confirmation(self) -> &'static [u8] {
        match self {
            Self::Client => CLIENT_CONFIRM,
            Self::Application => APP_CONFIRM,
        }
    }

    fn peer_confirmation(self) -> &'static [u8] {
        match self {
            Self::Client => APP_CONFIRM,
            Self::Application => CLIENT_CONFIRM,
        }
    }

    fn receipt(self) -> &'static [u8] {
        match self {
            Self::Client => CLIENT_RECEIPT,
            Self::Application => APP_RECEIPT,
        }
    }

    fn peer_receipt(self) -> &'static [u8] {
        match self {
            Self::Client => APP_RECEIPT,
            Self::Application => CLIENT_RECEIPT,
        }
    }
}

/// A failure to display a fingerprint on the trusted native surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeUnavailable;

/// Native confirmation cleanup could not be completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeCleanupFailed;

/// A decision obtained only from the trusted native human-comparison path.
///
/// The fixed-size value is the independently obtained *peer* fingerprint,
/// not a value supplied by the channel or copied from the local display.
pub enum NativeDecision {
    Pending,
    Denied,
    ComparedPeer([u8; FINGERPRINT_TEXT_BYTES]),
}

/// Nonblocking interface to a separately qualified native human channel.
///
/// Implementations must not block, self-confirm, accept a remotely supplied
/// decision, or use an MCP/stdio/public pairing code as proof of comparison.
/// Cancellation must be nonblocking, work even before a fingerprint was
/// displayed, and report failure unless all outstanding decisions are
/// dismissed. `begin_retirement` runs before channel and stream teardown; a
/// verifier with deferred or shared UI must use it to invalidate pending
/// displays and reserve its surface until cleanup finishes. It must not
/// activate UI, and it does not replace `cancel`. Dropping the verifier must
/// not block and must also release its native resources. The owner must check
/// revocation at the actual UI dispatch; this generic trait alone does not
/// provide that fence. The transport cannot establish the provenance of an
/// implementation or preempt a callback or destructor that blocks.
pub trait TrustedNativeVerification {
    fn show_local_fingerprint(&mut self, fingerprint: &str) -> Result<(), NativeUnavailable>;
    fn poll_decision(&mut self) -> NativeDecision;
    fn begin_retirement(&mut self) -> Result<(), NativeCleanupFailed> {
        Ok(())
    }
    fn cancel(&mut self) -> Result<(), NativeCleanupFailed>;
}

/// A persistent, one-way cleanup failure signal, including failures in `Drop`.
///
/// Retain this handle before transferring or dropping the transport, wait for
/// cleanup to finish, and inspect it before reusing a native confirmation
/// surface. `false` does not mean cleanup has completed. A failed or panicking
/// cancellation does not prove that the prompt was dismissed: its owner must
/// quarantine the surface and never reuse an actionable prompt.
#[derive(Clone, Debug)]
pub struct CleanupStatus(Arc<AtomicBool>);

impl CleanupStatus {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn mark_failed(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn failed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Closed,
    NotEstablished,
    Busy,
    Denied,
    Deadline,
    WorkLimit,
    Protocol,
    PeerClosed,
    Io(io::ErrorKind),
    CallbackPanicked,
    /// Cleanup failed; a process-wide panic hook may already have seen a payload.
    CleanupFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("secure transport closed"),
            Self::NotEstablished => formatter.write_str("secure transport not confirmed"),
            Self::Busy => formatter.write_str("secure transport output is pending"),
            Self::Denied => formatter.write_str("secure transport admission refused"),
            Self::Deadline => formatter.write_str("secure transport deadline expired"),
            Self::WorkLimit => formatter.write_str("secure transport work limit exceeded"),
            Self::Protocol => formatter.write_str("secure transport protocol refused"),
            Self::PeerClosed => formatter.write_str("secure transport peer closed"),
            Self::Io(kind) => write!(formatter, "secure transport I/O failed: {kind:?}"),
            Self::CallbackPanicked => formatter.write_str("secure transport callback failed"),
            Self::CleanupFailed => formatter.write_str("secure transport cleanup failed"),
        }
    }
}

impl std::error::Error for Error {}

pub enum Progress {
    /// No operation can currently make progress without waiting or a decision.
    Blocked,
    /// Retry promptly, subject to the caller's fairness budget.
    Advanced,
    /// Both local confirmations have been cryptographically exchanged.
    Established,
    /// The pending application frame has been physically written.
    Sent,
    /// One complete authenticated application frame.
    Frame(Vec<u8>),
}

/// Socket readiness to await after `Progress::Blocked`.
///
/// Native decisions and write-admission changes need separate wakeups. Retry
/// immediately after any other progress, subject to a bounded fairness budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoInterest {
    pub readable: bool,
    pub writable: bool,
}

impl fmt::Debug for Progress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Blocked => "Blocked",
            Self::Advanced => "Advanced",
            Self::Established => "Established",
            Self::Sent => "Sent",
            Self::Frame(_) => "Frame(<redacted>)",
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Handshake,
    NativeDecision,
    Confirming,
    Active,
    Closed,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Output {
    Confirmation,
    Receipt,
    Frame,
}

enum Attempt {
    Idle,
    Blocked,
    Progress(Progress),
}

fn catch_callback<T>(operation: impl FnOnce() -> T) -> Result<T, ()> {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(value) => Ok(value),
        Err(payload) => {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                // A second panicking payload cannot be safely dropped again.
                std::mem::forget(payload);
            }
            Err(())
        }
    }
}

fn dispose_owned<S, N: TrustedNativeVerification>(
    stream: &mut Option<S>,
    native: &mut Option<N>,
    cleanup: &CleanupStatus,
) {
    if let Some(stream) = stream.take() {
        if catch_callback(|| drop(stream)).is_err() {
            cleanup.mark_failed();
        }
    }
    if let Some(mut native) = native.take() {
        if !matches!(catch_callback(|| native.cancel()), Ok(Ok(()))) {
            cleanup.mark_failed();
        }
        if catch_callback(|| drop(native)).is_err() {
            cleanup.mark_failed();
        }
    }
}

fn begin_native_retirement<N: TrustedNativeVerification>(
    native: &mut Option<N>,
    cleanup: &CleanupStatus,
) {
    if let Some(native) = native.as_mut() {
        if !matches!(catch_callback(|| native.begin_retirement()), Ok(Ok(()))) {
            cleanup.mark_failed();
        }
    }
}

fn with_guard<G, T>(guard: G, operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let result = catch_callback(operation);
    let dropped = catch_callback(|| drop(guard));
    match (result, dropped) {
        (Ok(result), Ok(())) => result,
        _ => Err(Error::CallbackPanicked),
    }
}

/// One stream, one native comparison, and at most one frame per direction.
pub struct SecureTransport<S: Read + Write, N: TrustedNativeVerification> {
    stream: Option<S>,
    native: Option<N>,
    cleanup: CleanupStatus,
    role: Role,
    phase: Phase,
    handshake: Option<HandshakeIo>,
    unverified: Option<UnverifiedChannel>,
    records: Option<RecordIo>,
    bootstrap_deadline: Instant,
    lifetime_deadline: Instant,
    frame_duration: Duration,
    total_polls: usize,
    bootstrap_polls: usize,
    read_deadline: Option<Instant>,
    write_deadline: Option<Instant>,
    read_attempts: usize,
    write_attempts: usize,
    input: [u8; READ_CHUNK],
    input_start: usize,
    input_end: usize,
    input_received_at: Option<Instant>,
    output: Option<Output>,
    confirmation_sent: bool,
    confirmation_received: bool,
    receipt_sent: bool,
    receipt_received: bool,
    prefer_write: bool,
}

impl<S: Read + Write, N: TrustedNativeVerification> SecureTransport<S, N> {
    /// The caller must pass a uniquely owned, already nonblocking stream.
    /// Durations may only lower the fixed protocol ceilings.
    pub fn new(
        stream: S,
        role: Role,
        native: N,
        bootstrap_duration: Duration,
        frame_duration: Duration,
    ) -> Result<Self, Error> {
        let mut stream = Some(stream);
        let mut native = Some(native);
        let cleanup = CleanupStatus::new();
        let setup = catch_callback(|| {
            if bootstrap_duration.is_zero()
                || bootstrap_duration > MAX_BOOTSTRAP_DURATION
                || frame_duration.is_zero()
                || frame_duration > MAX_FRAME_DURATION
            {
                return Err(Error::Protocol);
            }
            let bootstrap_deadline = Instant::now()
                .checked_add(bootstrap_duration)
                .ok_or(Error::Deadline)?;
            let lifetime_deadline = Instant::now()
                .checked_add(MAX_TRANSPORT_LIFETIME)
                .ok_or(Error::Deadline)?;
            let handshake = match role {
                Role::Client => HandshakeIo::direct_initiator().map_err(|_| Error::Protocol)?,
                Role::Application => HandshakeIo::direct_responder(),
            };
            Ok((bootstrap_deadline, lifetime_deadline, handshake))
        });
        let (bootstrap_deadline, lifetime_deadline, handshake) = match setup {
            Ok(Ok(values)) => values,
            failure => {
                let error = match failure {
                    Ok(Err(error)) => error,
                    Err(()) => Error::CallbackPanicked,
                    Ok(Ok(_)) => unreachable!(),
                };
                begin_native_retirement(&mut native, &cleanup);
                dispose_owned(&mut stream, &mut native, &cleanup);
                return Err(if cleanup.failed() {
                    Error::CleanupFailed
                } else {
                    error
                });
            }
        };
        Ok(Self {
            stream,
            native,
            cleanup,
            role,
            phase: Phase::Handshake,
            handshake: Some(handshake),
            unverified: None,
            records: None,
            bootstrap_deadline,
            lifetime_deadline,
            frame_duration,
            total_polls: 0,
            bootstrap_polls: 0,
            read_deadline: None,
            write_deadline: None,
            read_attempts: 0,
            write_attempts: 0,
            input: [0; READ_CHUNK],
            input_start: 0,
            input_end: 0,
            input_received_at: None,
            output: None,
            confirmation_sent: false,
            confirmation_received: false,
            receipt_sent: false,
            receipt_received: false,
            prefer_write: true,
        })
    }

    pub fn is_established(&self) -> bool {
        self.phase == Phase::Active
    }

    pub fn has_pending_output(&self) -> bool {
        self.output.is_some()
    }

    pub fn io_interest(&self) -> IoInterest {
        match self.phase {
            Phase::Handshake => match self.handshake.as_ref().map(HandshakeIo::interest) {
                Some(Interest::Read) => IoInterest {
                    readable: true,
                    writable: false,
                },
                Some(Interest::Write) => IoInterest {
                    readable: false,
                    writable: true,
                },
                _ => IoInterest {
                    readable: false,
                    writable: false,
                },
            },
            Phase::NativeDecision => IoInterest {
                readable: true,
                writable: false,
            },
            Phase::Confirming => IoInterest {
                readable: !self.receipt_received,
                writable: self.output.is_some(),
            },
            Phase::Active => IoInterest {
                readable: true,
                writable: self.output.is_some(),
            },
            Phase::Closed => IoInterest {
                readable: false,
                writable: false,
            },
        }
    }

    /// Earliest deadline the owner must wake for, including idle connections.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.phase == Phase::Closed {
            return None;
        }
        let mut deadline = self.lifetime_deadline;
        if self.phase != Phase::Active {
            deadline = deadline.min(self.bootstrap_deadline);
        }
        if let Some(read) = self.read_deadline {
            deadline = deadline.min(read);
        }
        if let Some(write) = self.write_deadline {
            deadline = deadline.min(write);
        }
        Some(deadline)
    }

    pub fn cleanup_status(&self) -> CleanupStatus {
        self.cleanup.clone()
    }

    /// Consume the stream and pending frames. Failure is sticky; never reuse
    /// the native surface if dismissal or resource cleanup is uncertain.
    pub fn close(&mut self) -> Result<(), Error> {
        begin_native_retirement(&mut self.native, &self.cleanup);
        self.phase = Phase::Closed;
        self.input.fill(0);
        self.input_start = 0;
        self.input_end = 0;
        self.input_received_at = None;
        self.output = None;
        self.read_deadline = None;
        self.write_deadline = None;
        if let Some(mut handshake) = self.handshake.take() {
            if catch_callback(|| handshake.close()).is_err() {
                self.cleanup.mark_failed();
            }
            if catch_callback(|| drop(handshake)).is_err() {
                self.cleanup.mark_failed();
            }
        }
        if let Some(channel) = self.unverified.take() {
            if catch_callback(|| drop(channel)).is_err() {
                self.cleanup.mark_failed();
            }
        }
        if let Some(mut records) = self.records.take() {
            if catch_callback(|| records.close()).is_err() {
                self.cleanup.mark_failed();
            }
            if catch_callback(|| drop(records)).is_err() {
                self.cleanup.mark_failed();
            }
        }
        dispose_owned(&mut self.stream, &mut self.native, &self.cleanup);
        if self.cleanup.failed() {
            Err(Error::CleanupFailed)
        } else {
            Ok(())
        }
    }

    fn closed_error(&self) -> Error {
        if self.cleanup.failed() {
            Error::CleanupFailed
        } else {
            Error::Closed
        }
    }

    fn close_after(&mut self, error: Error) -> Error {
        self.close().err().unwrap_or(error)
    }

    /// Encrypt one application frame after mutual confirmation. No plaintext
    /// is queued when the channel is not established or another frame is busy.
    pub fn prepare_send(&mut self, frame: &[u8], expires: Instant) -> Result<(), Error> {
        if self.phase == Phase::Closed {
            return Err(self.closed_error());
        }
        if self.phase != Phase::Active {
            return Err(Error::NotEstablished);
        }
        if let Err(error) = self.check_deadlines() {
            return Err(self.close_after(error));
        }
        if self.output.is_some() {
            return Err(Error::Busy);
        }
        let now = Instant::now();
        let Some(deadline) = now.checked_add(self.frame_duration) else {
            return Err(self.close_after(Error::Deadline));
        };
        let deadline = deadline.min(expires);
        if now >= deadline {
            return Err(self.close_after(Error::Deadline));
        }
        let result = catch_callback(|| {
            self.records
                .as_mut()
                .ok_or(Error::Closed)?
                .prepare_send(frame)
                .map_err(|_| Error::Protocol)
        });
        match result {
            Ok(Ok(())) => {
                self.output = Some(Output::Frame);
                self.write_deadline = Some(deadline);
                self.write_attempts = 0;
                Ok(())
            }
            Ok(Err(error)) => Err(self.close_after(error)),
            Err(()) => Err(self.close_after(Error::CallbackPanicked)),
        }
    }

    /// Perform bounded nonblocking work. The admission guard, when needed,
    /// remains held for the complete physical write. Any terminal error or
    /// panic closes the owned stream, including an unknown partial write.
    pub fn poll<G>(
        &mut self,
        mut admit_write: impl FnMut() -> io::Result<G>,
    ) -> Result<Progress, Error> {
        let result = catch_callback(|| self.poll_inner(&mut admit_write));
        let dropped = catch_callback(|| drop(admit_write));
        if dropped.is_err() {
            if let Ok(Ok(Progress::Frame(mut bytes))) = result {
                bytes.fill(0);
            }
            return Err(self.close_after(Error::CallbackPanicked));
        }
        self.finish_poll(result)
    }

    fn finish_poll(
        &mut self,
        result: Result<Result<Progress, Error>, ()>,
    ) -> Result<Progress, Error> {
        match result {
            Ok(Ok(progress)) => Ok(progress),
            Ok(Err(error)) => Err(self.close_after(error)),
            Err(()) => Err(self.close_after(Error::CallbackPanicked)),
        }
    }

    pub(crate) fn check_deadlines(&self) -> Result<(), Error> {
        if self.phase == Phase::Closed {
            return Err(self.closed_error());
        }
        let now = Instant::now();
        if now >= self.lifetime_deadline
            || (self.phase != Phase::Active && now >= self.bootstrap_deadline)
            || self.read_deadline.is_some_and(|deadline| now >= deadline)
            || self.write_deadline.is_some_and(|deadline| now >= deadline)
        {
            return Err(Error::Deadline);
        }
        Ok(())
    }

    fn poll_inner<G>(
        &mut self,
        admit_write: &mut impl FnMut() -> io::Result<G>,
    ) -> Result<Progress, Error> {
        self.check_deadlines()?;
        self.charge_poll()?;
        if self.phase != Phase::Active {
            self.bootstrap_polls += 1;
            if self.bootstrap_polls > MAX_BOOTSTRAP_POLLS {
                return Err(Error::WorkLimit);
            }
        }
        match self.phase {
            Phase::Handshake => self.poll_handshake(admit_write),
            Phase::NativeDecision => self.poll_native(),
            Phase::Confirming | Phase::Active => {
                if self.advance_confirmation()? {
                    return Ok(Progress::Established);
                }
                let write_first = self.prefer_write;
                self.prefer_write = !self.prefer_write;
                let first = if write_first {
                    self.write_records(admit_write)?
                } else {
                    self.read_records()?
                };
                let attempt = match first {
                    Attempt::Progress(progress) => Attempt::Progress(progress),
                    Attempt::Idle | Attempt::Blocked => {
                        if write_first {
                            self.read_records()?
                        } else {
                            self.write_records(admit_write)?
                        }
                    }
                };
                if self.advance_confirmation()? {
                    return Ok(Progress::Established);
                }
                Ok(match attempt {
                    Attempt::Idle | Attempt::Blocked => Progress::Blocked,
                    Attempt::Progress(progress) => progress,
                })
            }
            Phase::Closed => Err(self.closed_error()),
        }
    }

    fn charge_poll(&mut self) -> Result<(), Error> {
        self.total_polls += 1;
        if self.total_polls > MAX_TOTAL_POLLS {
            return Err(Error::WorkLimit);
        }
        Ok(())
    }

    fn poll_handshake<G>(
        &mut self,
        admit_write: &mut impl FnMut() -> io::Result<G>,
    ) -> Result<Progress, Error> {
        let interest = self.handshake.as_ref().ok_or(Error::Closed)?.interest();
        match interest {
            Interest::Read => {
                let handshake = self.handshake.as_mut().ok_or(Error::Closed)?;
                let stream = self.stream.as_mut().ok_or(Error::Closed)?;
                let result = handshake.read_pending(|bytes| stream.read(bytes));
                self.check_deadlines()?;
                match result {
                    Ok(ReadProgress::Blocked) => Ok(Progress::Blocked),
                    Ok(ReadProgress::Idle) => Err(Error::Protocol),
                    Ok(_) => Ok(Progress::Advanced),
                    Err(_) => Err(Error::Protocol),
                }
            }
            Interest::Write => {
                let guard = match admit_write() {
                    Ok(guard) => guard,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        return Ok(Progress::Blocked);
                    }
                    Err(_) => return Err(Error::Denied),
                };
                let result = with_guard(guard, || {
                    self.check_deadlines()?;
                    let handshake = self.handshake.as_mut().ok_or(Error::Closed)?;
                    let stream = self.stream.as_mut().ok_or(Error::Closed)?;
                    let result = handshake.write_pending(|bytes| stream.write(bytes));
                    self.check_deadlines()?;
                    Ok(result)
                })?;
                match result {
                    Ok(WriteProgress::Blocked) => Ok(Progress::Blocked),
                    Ok(WriteProgress::Idle) => Err(Error::Protocol),
                    Ok(_) => Ok(Progress::Advanced),
                    Err(_) => Err(Error::Protocol),
                }
            }
            Interest::Complete => {
                let channel = self
                    .handshake
                    .as_mut()
                    .ok_or(Error::Closed)?
                    .take_unverified()
                    .map_err(|_| Error::Protocol)?;
                self.handshake = None;
                self.native
                    .as_mut()
                    .ok_or(Error::Closed)?
                    .show_local_fingerprint(&channel.fingerprint().to_string())
                    .map_err(|_| Error::Denied)?;
                self.check_deadlines()?;
                self.unverified = Some(channel);
                self.phase = Phase::NativeDecision;
                Ok(Progress::Advanced)
            }
            Interest::Closed => Err(Error::Closed),
        }
    }

    fn poll_native(&mut self) -> Result<Progress, Error> {
        let decision = self.native.as_mut().ok_or(Error::Closed)?.poll_decision();
        self.check_deadlines()?;
        match decision {
            NativeDecision::Pending => {
                if self.input_end == self.input.len() {
                    let mut extra = [0];
                    let result = self.stream.as_mut().ok_or(Error::Closed)?.read(&mut extra);
                    self.check_deadlines()?;
                    return match result {
                        Ok(0) => Err(Error::PeerClosed),
                        Ok(_) => Err(Error::Protocol),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            Ok(Progress::Blocked)
                        }

                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            Ok(Progress::Advanced)
                        }
                        Err(error) => Err(Error::Io(error.kind())),
                    };
                }
                let received_at = Instant::now();
                let result = self
                    .stream
                    .as_mut()
                    .ok_or(Error::Closed)?
                    .read(&mut self.input[self.input_end..]);
                self.check_deadlines()?;
                match result {
                    Ok(0) => Err(Error::PeerClosed),
                    Ok(count) if count <= self.input.len() - self.input_end => {
                        if self.input_end == 0 {
                            self.input_received_at = Some(received_at);
                        }
                        self.input_end += count;
                        Ok(Progress::Advanced)
                    }
                    Ok(_) => Err(Error::Protocol),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        Ok(Progress::Blocked)
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                        Ok(Progress::Advanced)
                    }
                    Err(error) => Err(Error::Io(error.kind())),
                }
            }
            NativeDecision::Denied => Err(Error::Denied),
            NativeDecision::ComparedPeer(fingerprint) => {
                let fingerprint = std::str::from_utf8(&fingerprint).map_err(|_| Error::Denied)?;
                let channel = self
                    .unverified
                    .take()
                    .ok_or(Error::Closed)?
                    .confirm(fingerprint)
                    .map_err(|_| Error::Denied)?;
                let mut records = RecordIo::new(channel).map_err(|_| Error::Protocol)?;
                records
                    .prepare_send(self.role.confirmation())
                    .map_err(|_| Error::Protocol)?;
                self.records = Some(records);
                self.output = Some(Output::Confirmation);
                self.write_deadline = Some(self.bootstrap_deadline);
                self.phase = Phase::Confirming;
                Ok(Progress::Advanced)
            }
        }
    }

    fn advance_confirmation(&mut self) -> Result<bool, Error> {
        if self.phase != Phase::Confirming {
            return Ok(false);
        }
        self.check_deadlines()?;
        if self.confirmation_sent
            && self.confirmation_received
            && !self.receipt_sent
            && self.output.is_none()
        {
            self.records
                .as_mut()
                .ok_or(Error::Closed)?
                .prepare_send(self.role.receipt())
                .map_err(|_| Error::Protocol)?;
            self.output = Some(Output::Receipt);
            self.write_deadline = Some(self.bootstrap_deadline);
            self.write_attempts = 0;
        }
        if self.receipt_sent && self.receipt_received {
            self.phase = Phase::Active;
            self.read_deadline = self.buffered_deadline()?;
            self.read_attempts = 0;
            self.check_deadlines()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn write_records<G>(
        &mut self,
        admit_write: &mut impl FnMut() -> io::Result<G>,
    ) -> Result<Attempt, Error> {
        let Some(output) = self.output else {
            return Ok(Attempt::Idle);
        };
        self.write_attempts += 1;
        if self.write_attempts > MAX_FRAME_ATTEMPTS {
            return Err(Error::WorkLimit);
        }
        let guard = match admit_write() {
            Ok(guard) => guard,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(Attempt::Blocked);
            }
            Err(_) => return Err(Error::Denied),
        };
        let result = with_guard(guard, || {
            self.check_deadlines()?;
            let records = self.records.as_mut().ok_or(Error::Closed)?;
            let stream = self.stream.as_mut().ok_or(Error::Closed)?;
            let result = records.write_pending(|bytes| stream.write(bytes));
            self.check_deadlines()?;
            Ok(result)
        })?;
        match result {
            Ok(WriteProgress::Blocked) => Ok(Attempt::Blocked),
            Ok(WriteProgress::Retry | WriteProgress::Partial) => {
                Ok(Attempt::Progress(Progress::Advanced))
            }
            Ok(WriteProgress::Complete) => {
                self.output = None;
                self.write_deadline = None;
                self.write_attempts = 0;
                match output {
                    Output::Confirmation => self.confirmation_sent = true,
                    Output::Receipt => self.receipt_sent = true,
                    Output::Frame => return Ok(Attempt::Progress(Progress::Sent)),
                }
                Ok(Attempt::Progress(Progress::Advanced))
            }
            Ok(WriteProgress::Idle) => Err(Error::Protocol),
            Err(_) => Err(Error::Protocol),
        }
    }

    fn read_records(&mut self) -> Result<Attempt, Error> {
        if self.phase == Phase::Confirming && self.receipt_received {
            return Ok(Attempt::Idle);
        }
        if self.input_start == self.input_end {
            if self.read_deadline.is_some() {
                self.read_attempts += 1;
                if self.read_attempts > MAX_FRAME_ATTEMPTS {
                    return Err(Error::WorkLimit);
                }
            }
            let received_at = Instant::now();
            let stream = self.stream.as_mut().ok_or(Error::Closed)?;
            let result = stream.read(&mut self.input);
            self.check_deadlines()?;
            match result {
                Ok(0) => {
                    let _ = self.records.as_mut().ok_or(Error::Closed)?.end_input();
                    return Err(Error::PeerClosed);
                }
                Ok(count) if count <= self.input.len() => {
                    self.input_start = 0;
                    self.input_end = count;
                    self.input_received_at = Some(received_at);
                }
                Ok(_) => return Err(Error::Protocol),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(Attempt::Blocked);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Ok(Attempt::Progress(Progress::Advanced));
                }
                Err(error) => return Err(Error::Io(error.kind())),
            }
        }
        if self.phase == Phase::Active && self.read_deadline.is_none() {
            self.read_deadline = self.buffered_deadline()?;
            self.check_deadlines()?;
        }
        let (used, frame) = self
            .records
            .as_mut()
            .ok_or(Error::Closed)?
            .receive(&self.input[self.input_start..self.input_end])
            .map_err(|_| Error::Protocol)?;
        self.check_deadlines()?;
        if used == 0 || used > self.input_end - self.input_start {
            return Err(Error::Protocol);
        }
        self.input_start += used;
        if self.input_start == self.input_end {
            self.input.fill(0);
            self.input_start = 0;
            self.input_end = 0;
            self.input_received_at = None;
        }
        let Some(frame) = frame else {
            return Ok(Attempt::Progress(Progress::Advanced));
        };
        self.read_attempts = 0;
        if self.phase == Phase::Active {
            self.read_deadline = self.buffered_deadline()?;
            self.check_deadlines()?;
            return Ok(Attempt::Progress(Progress::Frame(frame)));
        }
        self.read_deadline = None;
        if !self.confirmation_received && frame == self.role.peer_confirmation() {
            self.confirmation_received = true;
        } else if self.confirmation_received
            && self.confirmation_sent
            && !self.receipt_received
            && frame == self.role.peer_receipt()
        {
            self.receipt_received = true;
        } else {
            return Err(Error::Protocol);
        }
        Ok(Attempt::Progress(Progress::Advanced))
    }

    fn buffered_deadline(&self) -> Result<Option<Instant>, Error> {
        if self.input_start == self.input_end {
            Ok(None)
        } else {
            Ok(Some(
                self.input_received_at
                    .ok_or(Error::Protocol)?
                    .checked_add(self.frame_duration)
                    .ok_or(Error::Deadline)?,
            ))
        }
    }
}

impl<S: Read + Write, N: TrustedNativeVerification> Drop for SecureTransport<S, N> {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
