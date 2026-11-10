//! Presentation adapter for an explicitly selected, approved HOST attachment.

use super::*;
use crate::{
    host_terminal::HOST_REPLY,
    protocol::{DISPLACED, EXITED, OUTPUT, RESIZED, SYNC},
};
use festerm_host_protocol::Envelope;
use festerm_session::{
    Session, SessionError, SessionErrorKind, SessionEvent, SessionEventNotifier, SessionId,
    SessionLifecycle, SessionMetrics, SessionOperation, SessionSendError, SessionTryReceiveError,
    ShutdownError, ShutdownResult, TerminalSize,
};
use std::sync::Mutex;

struct State {
    worker: Worker,
    adoption: Option<Adoption>,
    terminal: Option<TerminalHandle>,
    lifecycle: SessionLifecycle,
    lifecycle_pending: bool,
    detach: Option<OperationId>,
    metrics: SessionMetrics,
}

/// One frontend-owned attachment. All terminal mutations still belong to the
/// composition root; receiving recovery never acknowledges its installation.
pub struct HostSession {
    id: SessionId,
    scope: Scope,
    state: Mutex<State>,
    notifier: Arc<dyn SessionEventNotifier>,
}

impl HostSession {
    pub fn start(
        context: ApprovedContext,
        scope: Scope,
        target: Target,
        budgets: &Budgets,
        notifier: Arc<dyn SessionEventNotifier>,
    ) -> Result<Self, Failure> {
        let notify = notifier.clone();
        let worker = Worker::start(
            context,
            scope,
            Operation::Attach { target },
            budgets,
            Arc::new(move || notify.notify()),
        )?;
        Ok(Self::from_worker(worker, notifier))
    }

    fn from_worker(worker: Worker, notifier: Arc<dyn SessionEventNotifier>) -> Self {
        Self {
            id: SessionId::next(),
            scope: worker.scope().clone(),
            state: Mutex::new(State {
                worker,
                adoption: None,
                terminal: None,
                lifecycle: SessionLifecycle::Starting,
                lifecycle_pending: true,
                detach: None,
                metrics: SessionMetrics {
                    event_queue_capacity: 4,
                    ..SessionMetrics::default()
                },
            }),
            notifier,
        }
    }

    pub fn has_pending_snapshot(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        self.refresh(&mut state);
        state.adoption.is_some()
    }

    pub fn account(&self) -> &str {
        self.scope.binding.account()
    }

    /// Called only on the originating installed frontend. It cannot admit
    /// input; Running still requires the subsequent host activation receipt.
    pub fn adopt_into(&self, terminal: &mut Terminal) -> Result<bool, Failure> {
        let mut state = self.state.lock().map_err(|_| Failure::WorkerUnavailable)?;
        self.refresh(&mut state);
        let Some(adoption) = state.adoption.take() else {
            return Ok(false);
        };
        if state.worker.cancel.is_cancelled() {
            return Err(Error::Cancelled.into());
        }
        adoption.adopt_into(&self.scope, terminal)?;
        Ok(true)
    }
}

impl HostSession {
    /// App-lock/account invalidation abandons the transport, not intentional
    /// detach success. The daemon owns temporary-session loss grace.
    pub fn cancel(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.worker.cancel.is_cancelled() {
                return;
            }
            state.worker.cancel();
            state.adoption = None;
            state.terminal = None;
            Self::transition(
                &mut state,
                SessionLifecycle::Disconnected(Self::error(Error::Cancelled.into())),
            );
        }
        self.notifier.notify();
    }

    pub fn sync(&self, sync: &crate::protocol::Sync) -> Result<(), SessionSendError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Self::closed(SessionOperation::Input))?;
        self.refresh(&mut state);
        let terminal = Self::running(&state, SessionOperation::Input)?;
        terminal
            .sync(sync)
            .map_err(|error| Self::send_error(error, SessionOperation::Input))
    }

    fn refresh(&self, state: &mut State) {
        let Some(completion) = state.worker.try_receive(&self.scope) else {
            return;
        };
        match completion.outcome {
            Outcome::Snapshot(adoption)
                if state.lifecycle == SessionLifecycle::Starting && state.adoption.is_none() =>
            {
                state.adoption = Some(*adoption);
            }
            Outcome::Attached(terminal) if state.lifecycle == SessionLifecycle::Starting => {
                terminal.set_event_notifier(self.notifier.clone());
                state.terminal = Some(terminal);
                Self::transition(state, SessionLifecycle::Running);
            }
            Outcome::Failed(failure) => {
                state.metrics.error_count += 1;
                Self::transition(state, SessionLifecycle::Failed(Self::error(failure)));
            }
            _ => {
                state.worker.cancel();
                Self::transition(
                    state,
                    SessionLifecycle::Failed(Self::error(ContractError::Stale.into())),
                );
            }
        }
    }

    fn transition(state: &mut State, lifecycle: SessionLifecycle) {
        state.lifecycle = lifecycle;
        state.lifecycle_pending = true;
    }

    fn finish(state: &mut State, lifecycle: SessionLifecycle) {
        state.lifecycle = lifecycle;
        state.terminal = None;
        state.adoption = None;
        state.worker.cancel();
    }

    fn error(failure: Failure) -> SessionError {
        SessionError::new(SessionErrorKind::Output, failure.message())
    }

    fn closed(operation: SessionOperation) -> SessionSendError {
        SessionSendError::Closed { operation }
    }

    fn running(
        state: &State,
        operation: SessionOperation,
    ) -> Result<&TerminalHandle, SessionSendError> {
        if state.lifecycle != SessionLifecycle::Running {
            return Err(Self::closed(operation));
        }
        state
            .terminal
            .as_ref()
            .ok_or_else(|| Self::closed(operation))
    }

    fn send_error(error: Error, operation: SessionOperation) -> SessionSendError {
        match error {
            Error::Contract(ContractError::Busy) => SessionSendError::Full {
                operation,
                capacity: 8,
            },
            _ => Self::closed(operation),
        }
    }

    fn event(state: &mut State, frame: host_terminal::Frame) -> Result<SessionEvent, Failure> {
        match frame.kind() {
            OUTPUT => {
                state.metrics.output_bytes += frame.payload().len() as u64;
                Ok(SessionEvent::Output(frame.payload().to_vec()))
            }
            RESIZED => Ok(SessionEvent::ResizeApplied(
                crate::wire::decode_size(frame.payload()).map_err(|_| ContractError::Malformed)?,
            )),
            SYNC => Ok(SessionEvent::RecoverySync(frame.payload().to_vec())),
            HOST_REPLY => {
                let reply: Envelope<Response> = festerm_host_protocol::decode(frame.payload())?;
                if state.detach.as_ref() != Some(&reply.operation) {
                    return Err(ContractError::Stale.into());
                }
                match reply.body {
                    Response::Detached { .. } => {
                        Self::finish(state, SessionLifecycle::Stopped);
                        Ok(SessionEvent::Lifecycle(SessionLifecycle::Stopped))
                    }
                    Response::Refused { reason } => Err(Error::Refused(reason).into()),
                    _ => Err(ContractError::Malformed.into()),
                }
            }
            EXITED => {
                // This wire event carries no exit status; do not invent zero.
                Self::finish(state, SessionLifecycle::Stopped);
                Ok(SessionEvent::Lifecycle(SessionLifecycle::Stopped))
            }
            DISPLACED => {
                let lifecycle = SessionLifecycle::Disconnected(SessionError::new(
                    SessionErrorKind::Output,
                    "This attachment was displaced by another client. No automatic reconnect or input replay was attempted.",
                ));
                Self::finish(state, lifecycle.clone());
                Ok(SessionEvent::Lifecycle(lifecycle))
            }
            _ => Err(ContractError::Malformed.into()),
        }
    }
}

impl Session for HostSession {
    fn id(&self) -> SessionId {
        self.id
    }

    fn lifecycle(&self) -> SessionLifecycle {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        self.refresh(&mut state);
        state.lifecycle.clone()
    }

    fn metrics(&self) -> SessionMetrics {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let depth = state
            .terminal
            .as_ref()
            .map_or(0, TerminalHandle::event_queue_depth);
        state.metrics.event_queue_depth = depth;
        state.metrics.event_queue_high_watermark =
            state.metrics.event_queue_high_watermark.max(depth);
        state.metrics
    }

    fn try_send_input(&self, bytes: &[u8]) -> Result<(), SessionSendError> {
        if bytes.len() > crate::protocol::CHUNK_LIMIT {
            return Err(SessionSendError::TooLarge {
                operation: SessionOperation::Input,
                maximum: crate::protocol::CHUNK_LIMIT,
                actual: bytes.len(),
            });
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| Self::closed(SessionOperation::Input))?;
        self.refresh(&mut state);
        let terminal = Self::running(&state, SessionOperation::Input)?;
        if !bytes.is_empty() {
            terminal
                .input(bytes)
                .map_err(|error| Self::send_error(error, SessionOperation::Input))?;
            state.metrics.input_bytes += bytes.len() as u64;
        }
        Ok(())
    }

    fn try_resize(&self, size: TerminalSize) -> Result<(), SessionSendError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Self::closed(SessionOperation::Resize))?;
        self.refresh(&mut state);
        Self::running(&state, SessionOperation::Resize)?
            .resize(size)
            .map_err(|error| Self::send_error(error, SessionOperation::Resize))?;
        state.metrics.resize_count += 1;
        Ok(())
    }

    fn try_shutdown(&self) -> Result<(), SessionSendError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Self::closed(SessionOperation::Shutdown))?;
        self.refresh(&mut state);
        if state.lifecycle.is_terminal() || state.detach.is_some() {
            return Ok(());
        }
        let terminal = Self::running(&state, SessionOperation::Shutdown)?;
        state.detach = Some(
            terminal
                .request_detach()
                .map_err(|error| Self::send_error(error, SessionOperation::Shutdown))?,
        );
        Self::transition(&mut state, SessionLifecycle::Stopping);
        self.notifier.notify();
        Ok(())
    }

    fn try_recv_event(&self) -> Result<SessionEvent, SessionTryReceiveError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionTryReceiveError::Closed)?;
        self.refresh(&mut state);
        if state.lifecycle_pending {
            state.lifecycle_pending = false;
            return Ok(SessionEvent::Lifecycle(state.lifecycle.clone()));
        }
        if state.lifecycle.is_terminal()
            || matches!(state.lifecycle, SessionLifecycle::Disconnected(_))
        {
            return Err(SessionTryReceiveError::Closed);
        }
        let Some(terminal) = state.terminal.as_mut() else {
            return Err(SessionTryReceiveError::Empty);
        };
        let result = match terminal.try_next_event() {
            Ok(Some(frame)) => Self::event(&mut state, frame),
            Ok(None) => return Err(SessionTryReceiveError::Empty),
            Err(error) => Err(error.into()),
        };
        match result {
            Ok(event) => Ok(event),
            Err(failure) => {
                state.metrics.error_count += 1;
                Self::finish(
                    &mut state,
                    SessionLifecycle::Disconnected(Self::error(failure)),
                );
                Ok(SessionEvent::Lifecycle(state.lifecycle.clone()))
            }
        }
    }

    fn terminal_replies_owned_by_backend(&self) -> bool {
        true
    }

    fn shutdown(&self, timeout: Duration) -> Result<ShutdownResult, ShutdownError> {
        if self.lifecycle().is_terminal() {
            return Ok(ShutdownResult::AlreadyStopped);
        }
        self.try_shutdown().map_err(ShutdownError::Request)?;
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let _ = self.try_recv_event();
            match self.lifecycle() {
                SessionLifecycle::Stopped => return Ok(ShutdownResult::Stopped),
                SessionLifecycle::Disconnected(error) | SessionLifecycle::Failed(error) => {
                    return Err(ShutdownError::Failed(error));
                }
                _ => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        self.cancel();
        Err(ShutdownError::TimedOut { timeout })
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
