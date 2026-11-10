use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

struct Outgoing {
    kind: u8,
    bytes: Vec<u8>,
    _terminal: Option<ClientBytesPermit>,
    _control: Option<BudgetPermit>,
}

struct ClientState {
    binding: LiveBinding,
    permit: Arc<ConnectionPermit>,
    detach: Mutex<Option<(Envelope<Request>, tokio::time::Instant)>>,
    detach_started: tokio::sync::Notify,
    closed: AtomicBool,
    failure: Mutex<Option<Error>>,
    cancel: CancellationToken,
}

/// Full-duplex portable transport. Queued frames retain their aggregate byte
/// charges; no reconnect or replay task exists. Event availability is
/// asynchronous and does not poll the terminal.
pub struct TerminalHandle {
    commands: tokio::sync::mpsc::Sender<Outgoing>,
    events: tokio::sync::mpsc::Receiver<Frame>,
    state: Arc<ClientState>,
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        self.state.cancel.cancel();
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send + 'static> LiveTerminal<S> {
    pub fn spawn(self) -> Result<TerminalHandle, Error> {
        if !self.usable {
            return Err(Error::Closed);
        }
        let state = Arc::new(ClientState {
            binding: self.binding,
            permit: self.permit,
            detach: Mutex::new(None),
            detach_started: tokio::sync::Notify::new(),
            closed: AtomicBool::new(false),
            failure: Mutex::new(None),
            cancel: CancellationToken::new(),
        });
        let (commands, mut command_rx) = tokio::sync::mpsc::channel::<Outgoing>(8);
        let (event_tx, events) = tokio::sync::mpsc::channel(4);
        let task_state = state.clone();
        tokio::spawn(async move {
            let (mut read, mut output) = tokio::io::split(self.stream);
            let reader = async {
                loop {
                    let frame = read_frame(&mut read, &task_state.permit, false).await?;
                    let finished = if frame.kind == HOST_REPLY {
                        let reply: Envelope<Response> =
                            festerm_host_protocol::decode(&frame.bytes)?;
                        let pending = task_state.detach.lock().map_err(|_| Error::Closed)?;
                        reply.validate_for(&pending.as_ref().ok_or(Error::Closed)?.0)?;
                        true
                    } else {
                        if !matches!(frame.kind, OUTPUT | RESIZED | SYNC | EXITED | DISPLACED) {
                            return Err(festerm_host_protocol::ContractError::Malformed.into());
                        }
                        matches!(frame.kind, EXITED | DISPLACED)
                    };
                    event_tx.send(frame).await.map_err(|_| Error::Closed)?;
                    if finished {
                        return Ok::<(), Error>(());
                    }
                }
            };
            let writer = async {
                while let Some(frame) = command_rx.recv().await {
                    write(&mut output, frame.kind, &frame.bytes).await?;
                }
                Err::<(), Error>(Error::Closed)
            };
            let detach_deadline = async {
                task_state.detach_started.notified().await;
                let deadline = task_state
                    .detach
                    .lock()
                    .map_err(|_| Error::Closed)?
                    .as_ref()
                    .ok_or(Error::Closed)?
                    .1;
                tokio::time::sleep_until(deadline).await;
                Err::<(), Error>(Error::Deadline)
            };
            let result = tokio::select! {
                biased;
                _ = task_state.cancel.cancelled() => Err(Error::Cancelled),
                result = detach_deadline => result,
                result = reader => result,
                result = writer => result,
            };
            task_state.closed.store(true, Ordering::Release);
            if let Err(error) = result {
                if let Ok(mut failure) = task_state.failure.lock() {
                    *failure = Some(error);
                }
            }
        });
        Ok(TerminalHandle {
            commands,
            events,
            state,
        })
    }
}

impl TerminalHandle {
    pub fn binding(&self) -> &LiveBinding {
        &self.state.binding
    }

    fn enqueue(&self, frame: Outgoing) -> Result<(), Error> {
        if self.state.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        self.commands.try_send(frame).map_err(|error| match error {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                festerm_host_protocol::ContractError::Busy.into()
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => Error::Closed,
        })
    }

    pub fn input(&self, bytes: &[u8]) -> Result<(), Error> {
        let detach = self.state.detach.lock().map_err(|_| Error::Closed)?;
        if detach.is_some() {
            return Err(Error::Closed);
        }
        if bytes.is_empty() || bytes.len() > CHUNK_LIMIT {
            return Err(festerm_host_protocol::ContractError::Limit.into());
        }
        let charge = self.state.permit.reserve_terminal_bytes(bytes.len())?;
        self.enqueue(Outgoing {
            kind: INPUT,
            bytes: bytes.to_vec(),
            _terminal: Some(charge),
            _control: None,
        })
    }

    pub fn resize(&self, size: festerm_session::TerminalSize) -> Result<(), Error> {
        let detach = self.state.detach.lock().map_err(|_| Error::Closed)?;
        if detach.is_some() {
            return Err(Error::Closed);
        }
        let charge = self.state.permit.reserve_terminal_bytes(8)?;
        self.enqueue(Outgoing {
            kind: RESIZE,
            bytes: crate::wire::encode_size(size),
            _terminal: Some(charge),
            _control: None,
        })
    }

    pub fn sync(&self, sync: &Sync) -> Result<(), Error> {
        let detach = self.state.detach.lock().map_err(|_| Error::Closed)?;
        if detach.is_some() {
            return Err(Error::Closed);
        }
        match sync {
            Sync::Scrollback(limit) if *limit > HOST_SNAPSHOT_LIMIT => {
                return Err(festerm_host_protocol::ContractError::Limit.into())
            }
            Sync::Bytes(bytes) if bytes.len() > CHUNK_LIMIT => {
                return Err(festerm_host_protocol::ContractError::Limit.into())
            }
            _ => {}
        }
        let charge = self.state.permit.reserve_terminal_bytes(CHUNK_LIMIT)?;
        let bytes = encode_json_bounded(sync, CHUNK_LIMIT)?;
        self.enqueue(Outgoing {
            kind: SYNC,
            bytes,
            _terminal: Some(charge),
            _control: None,
        })
    }

    /// Returns only an operation ID, not Detached success. Observe its validated
    /// HOST_REPLY through `next_event`; a timeout/close never means detached.
    pub fn request_detach(&self) -> Result<OperationId, Error> {
        if !self.state.binding.capabilities.detach {
            return Err(Error::Refused(festerm_host_protocol::Refusal::Unsupported));
        }
        let mut pending = self.state.detach.lock().map_err(|_| Error::Closed)?;
        if pending.is_some() {
            return Err(Error::Closed);
        }
        let binding = &self.state.binding;
        let request = Envelope::new(
            binding.host.host().clone(),
            OperationId::new(),
            Request::Detach {
                target: binding.target.clone(),
                attachment: binding.attachment.clone(),
                lease: binding.lease.as_ref().map(|lease| lease.lease.clone()),
                lease_epoch: binding.lease.as_ref().map(|lease| lease.epoch),
                reason: festerm_host_protocol::DetachKind::Intentional,
            },
        );
        let charge = self
            .state
            .permit
            .budgets()
            .reserve(Resource::ControlBytes, CONTROL_FRAME_BYTES)?;
        let bytes = festerm_host_protocol::encode(&request)?;
        let operation = request.operation.clone();
        self.enqueue(Outgoing {
            kind: HOST_REQUEST,
            bytes,
            _terminal: None,
            _control: Some(charge),
        })?;
        *pending = Some((request, tokio::time::Instant::now() + DEADLINE));
        self.state.detach_started.notify_one();
        Ok(operation)
    }

    pub async fn next_event(&mut self) -> Result<Frame, Error> {
        self.events.recv().await.ok_or_else(|| {
            self.state
                .failure
                .lock()
                .ok()
                .and_then(|failure| *failure)
                .unwrap_or(Error::Closed)
        })
    }
}
