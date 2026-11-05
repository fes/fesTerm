//! Portable, connection-owned observation. No credentials or transport handle escape.
pub use crate::observation_protocol::{
    Capabilities, Correlation, DirectoryCursor, Entry, Kind, Metadata, Page, ReadOutcome, Root,
    WatchEvent, WatchNotice,
};
use crate::{
    observation_protocol::{self as rpc, Operation, Result as Reply},
    protocol::{invalid, Target},
    relay::ForwardingBinding,
    wire::{read_frame, write_frame, DEADLINE},
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

struct Call {
    root: String,
    operation: Operation,
    reply: flume::Sender<io::Result<CallResult>>,
}
enum CallResult {
    Reply(Reply, Option<Vec<u8>>),
    Watch(ObservationWatch),
}
struct Shared {
    capabilities: Mutex<Option<Capabilities>>,
    binding: Mutex<Option<ForwardingBinding>>,
    expires: Mutex<Option<tokio::time::Instant>>,
    busy: AtomicBool,
    stopped: CancellationToken,
}

#[derive(Clone)]
pub struct ObservationRequestor {
    shared: Arc<Shared>,
    sender: flume::Sender<Call>,
}
impl std::fmt::Debug for ObservationRequestor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservationRequestor(<bound live attachment>)")
    }
}

impl ObservationRequestor {
    pub(crate) fn pending(parent: &CancellationToken) -> (Self, ObservationDriver) {
        let (sender, receiver) = flume::bounded(1);
        let shared = Arc::new(Shared {
            capabilities: Mutex::new(None),
            binding: Mutex::new(None),
            expires: Mutex::new(None),
            busy: AtomicBool::new(false),
            stopped: parent.child_token(),
        });
        (
            Self {
                shared: shared.clone(),
                sender,
            },
            ObservationDriver { shared, receiver },
        )
    }
    pub fn available(&self) -> bool {
        !self.shared.stopped.is_cancelled()
            && self.shared.expires.lock().is_ok_and(|expiry| {
                expiry.is_some_and(|deadline| deadline > tokio::time::Instant::now())
            })
            && !self.sender.is_disconnected()
            && self
                .shared
                .binding
                .lock()
                .ok()
                .and_then(|b| b.clone())
                .is_some_and(|b| b.available())
            && self.shared.capabilities.lock().is_ok_and(|c| c.is_some())
    }
    pub fn capabilities(&self) -> Option<Capabilities> {
        if !self.available() {
            return None;
        }
        self.shared.capabilities.lock().ok()?.clone()
    }
    pub fn cancel(&self) {
        self.shared.stopped.cancel();
    }
    fn call(&self, root: &str, operation: Operation) -> io::Result<CallResult> {
        if !self.available()
            || !self
                .capabilities()
                .is_some_and(|c| c.roots.iter().any(|r| r.id == root))
        {
            return Err(invalid());
        }
        self.shared
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| invalid())?;
        struct Busy<'a>(&'a AtomicBool);
        impl Drop for Busy<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _busy = Busy(&self.shared.busy);
        let (reply, receiver) = flume::bounded(1);
        self.sender
            .try_send(Call {
                root: root.into(),
                operation,
                reply,
            })
            .map_err(|_| invalid())?;
        let result = match receiver.recv_timeout(DEADLINE) {
            Ok(result) => result?,
            Err(_) => {
                self.cancel();
                return Err(invalid());
            }
        };
        if !self.available() {
            return Err(invalid());
        }
        Ok(result)
    }
    fn reply(&self, root: &str, operation: Operation) -> io::Result<Reply> {
        match self.call(root, operation)? {
            CallResult::Reply(reply, None) => Ok(reply),
            _ => Err(invalid()),
        }
    }
    pub fn stat(&self, root: &str, path: &str) -> io::Result<Metadata> {
        path_check(path)?;
        match self.reply(root, Operation::Stat { path: path.into() })? {
            Reply::Stat { metadata } => {
                metadata.validate()?;
                Ok(metadata)
            }
            _ => Err(invalid()),
        }
    }
    pub fn open_directory(&self, root: &str, path: &str) -> io::Result<DirectoryCursor> {
        path_check(path)?;
        match self.reply(root, Operation::OpenDirectory { path: path.into() })? {
            Reply::Directory { cursor }
                if cursor.len() == 32 && cursor.bytes().all(|c| c.is_ascii_hexdigit()) =>
            {
                Ok(cursor)
            }
            _ => Err(invalid()),
        }
    }
    pub fn read_directory(&self, root: &str, cursor: &str, limit: u16) -> io::Result<Page> {
        if cursor.len() != 32 || limit == 0 || limit > rpc::PAGE_LIMIT {
            return Err(invalid());
        }
        match self.reply(
            root,
            Operation::ReadDirectory {
                cursor: cursor.into(),
                limit,
            },
        )? {
            Reply::Page { page } => {
                if page.entries.len() + usize::from(page.skipped) > usize::from(limit) {
                    return Err(invalid());
                }
                for entry in &page.entries {
                    path_check(&format!("/{}", entry.name))?;
                    if entry.name.contains('/') {
                        return Err(invalid());
                    }
                    entry.metadata.validate()?;
                }
                Ok(page)
            }
            _ => Err(invalid()),
        }
    }
    pub fn close_directory(&self, root: &str, cursor: &str) -> io::Result<()> {
        if cursor.len() != 32 {
            return Err(invalid());
        }
        match self.reply(
            root,
            Operation::CloseDirectory {
                cursor: cursor.into(),
            },
        )? {
            Reply::Closed => Ok(()),
            _ => Err(invalid()),
        }
    }
    pub fn read(
        &self,
        root: &str,
        path: &str,
        offset: u64,
        length: usize,
        expected: &Metadata,
    ) -> io::Result<ReadOutcome> {
        path_check(path)?;
        expected.validate()?;
        if length > rpc::CHUNK_LIMIT {
            return Err(invalid());
        }
        match self.call(
            root,
            Operation::Read {
                path: path.into(),
                offset: offset.to_string(),
                length: length as u32,
                expected: expected.clone(),
            },
        )? {
            CallResult::Reply(Reply::Changed { metadata }, None) => {
                metadata.validate()?;
                Ok(ReadOutcome::Changed { metadata })
            }
            CallResult::Reply(
                Reply::Data {
                    metadata,
                    length: count,
                },
                Some(bytes),
            ) => {
                metadata.validate()?;
                if &metadata != expected || bytes.len() != count as usize || bytes.len() > length {
                    return Err(invalid());
                }
                Ok(ReadOutcome::Data { metadata, bytes })
            }
            _ => Err(invalid()),
        }
    }
    pub fn correlation(&self, root: &str) -> io::Result<Option<Correlation>> {
        match self.reply(root, Operation::Correlation {})? {
            Reply::Correlation { correlation } => {
                if correlation.as_ref().is_some_and(|c| {
                    c.value.is_empty()
                        || c.value.len() > 128
                        || c.value.chars().any(char::is_control)
                }) {
                    return Err(invalid());
                }
                Ok(correlation)
            }
            _ => Err(invalid()),
        }
    }
    pub fn watch(&self, root: &str) -> io::Result<ObservationWatch> {
        match self.call(root, Operation::Watch {})? {
            CallResult::Watch(watch) => Ok(watch),
            _ => Err(invalid()),
        }
    }
}
fn path_check(path: &str) -> io::Result<()> {
    if rpc::valid_path(path) {
        Ok(())
    } else {
        Err(invalid())
    }
}

pub(crate) struct ObservationDriver {
    shared: Arc<Shared>,
    receiver: flume::Receiver<Call>,
}
impl ObservationDriver {
    pub(crate) fn fence_expiry(&self, deadline: Option<tokio::time::Instant>) {
        let Some(deadline) = deadline.filter(|deadline| *deadline > tokio::time::Instant::now())
        else {
            self.shared.stopped.cancel();
            return;
        };
        if let Ok(mut expiry) = self.shared.expires.lock() {
            *expiry = Some(deadline);
        } else {
            self.shared.stopped.cancel();
            return;
        }
        let cancel = self.shared.stopped.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {}
                _ = tokio::time::sleep_until(deadline) => cancel.cancel(),
            }
        });
    }
    pub(crate) async fn run(self, binding: ForwardingBinding, target: Target) -> io::Result<()> {
        let cancel = self.shared.stopped.clone();
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(invalid()),
            result = self.run_inner(binding, target) => result,
        };
        cancel.cancel();
        result
    }

    pub(crate) fn token_expiry(token: &str) -> Option<tokio::time::Instant> {
        if token.len() > 16 * 1024 {
            return None;
        }
        let mut parts = token.split('.');
        parts.next().filter(|s| !s.is_empty())?;
        let payload = parts.next().filter(|s| !s.is_empty())?;
        parts.next().filter(|s| !s.is_empty())?;
        if parts.next().is_some() {
            return None;
        }
        // Read only the bounded exp claim of an already service-validated token.
        // This is a conservative lifetime fence, not token authentication.
        let mut decoded = Vec::with_capacity(payload.len() * 3 / 4);
        let mut bits = 0u32;
        let mut count = 0u32;
        for byte in payload.bytes() {
            let value = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'-' => 62,
                b'_' => 63,
                _ => return None,
            };
            bits = (bits << 6) | u32::from(value);
            count += 6;
            if count >= 8 {
                count -= 8;
                decoded.push((bits >> count) as u8);
                bits &= (1 << count) - 1;
            }
        }
        if count >= 6 || bits != 0 {
            return None;
        }
        #[derive(serde::Deserialize)]
        struct Expiry {
            exp: u64,
        }
        let claims: Expiry = serde_json::from_slice(&decoded).ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        let until = Duration::from_secs(claims.exp).checked_sub(now)?;
        if until.is_zero() {
            return None;
        }
        tokio::time::Instant::now().checked_add(until)
    }
    async fn run_inner(&self, binding: ForwardingBinding, target: Target) -> io::Result<()> {
        let mut stream = tokio::time::timeout(DEADLINE, binding.open())
            .await
            .map_err(|_| invalid())??;
        let capabilities = open(&mut stream, &target).await?;
        *self.shared.binding.lock().map_err(|_| invalid())? = Some(binding.clone());
        *self.shared.capabilities.lock().map_err(|_| invalid())? = Some(capabilities.clone());
        let watches = Arc::new(tokio::sync::Semaphore::new(2));
        let mut next = 0u32;
        let mut ping = tokio::time::interval(Duration::from_secs(10));
        ping.tick().await;
        loop {
            let call = tokio::select! {
                call = self.receiver.recv_async() => Some(call.map_err(|_| invalid())?),
                _ = ping.tick() => None,
            };
            if !binding.available() || self.shared.stopped.is_cancelled() {
                return Err(invalid());
            }
            let Some(call) = call else {
                next = next.checked_add(1).ok_or_else(invalid)?;
                let result = exchange(
                    &mut stream,
                    next,
                    &capabilities.roots[0].id,
                    Operation::Ping {},
                )
                .await?;
                if !matches!(result, CallResult::Reply(Reply::Pong, None)) {
                    return Err(invalid());
                }
                continue;
            };
            if matches!(call.operation, Operation::Watch {}) {
                let permit = match watches.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        let _ = call.reply.try_send(Err(io::ErrorKind::WouldBlock.into()));
                        continue;
                    }
                };
                let cancel = self.shared.stopped.child_token();
                let binding = binding.clone();
                let target = target.clone();
                let expiry = *self.shared.expires.lock().map_err(|_| invalid())?;
                tokio::spawn(async move {
                    let result = tokio::select! {
                        _ = cancel.cancelled() => Err(invalid()),
                        result = start_watch(binding, target, call.root, cancel.clone(), permit, expiry) => result,
                    };
                    let _ = call.reply.try_send(result.map(CallResult::Watch));
                });
                continue;
            }
            next = next.checked_add(1).ok_or_else(invalid)?;
            let result = exchange(&mut stream, next, &call.root, call.operation).await;
            if self.shared.stopped.is_cancelled() || !binding.available() {
                return Err(invalid());
            }
            let fatal = result
                .as_ref()
                .is_err_and(|e| e.kind() != io::ErrorKind::PermissionDenied);
            let _ = call.reply.try_send(result);
            if fatal {
                return Err(invalid());
            }
        }
    }
}

async fn open(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    target: &Target,
) -> io::Result<Capabilities> {
    write_frame(
        stream,
        rpc::OPEN,
        &serde_json::to_vec(&rpc::Open {
            version: rpc::VERSION,
            target: target.clone(),
        })?,
    )
    .await?;
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_frame(stream))
        .await
        .map_err(|_| invalid())??;
    if kind != rpc::RESPONSE {
        return Err(invalid());
    }
    let response: rpc::Response = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    match response {
        rpc::Response {
            id: 0,
            result: Some(Reply::Ready { capabilities }),
            error: None,
        } => {
            capabilities.validate()?;
            Ok(capabilities)
        }
        _ => Err(invalid()),
    }
}

async fn exchange(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    id: u32,
    root: &str,
    operation: Operation,
) -> io::Result<CallResult> {
    write_frame(
        stream,
        rpc::REQUEST,
        &serde_json::to_vec(&rpc::Request {
            id,
            root: root.into(),
            operation,
        })?,
    )
    .await?;
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_frame(stream))
        .await
        .map_err(|_| invalid())??;
    if kind != rpc::RESPONSE {
        return Err(invalid());
    }
    let response: rpc::Response = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if response.id != id {
        return Err(invalid());
    }
    let result = match (response.result, response.error) {
        (Some(result), None) => result,
        (None, Some(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "observation refused",
            ))
        }
        _ => return Err(invalid()),
    };
    let data = if let Reply::Data { length, .. } = &result {
        if *length as usize > rpc::CHUNK_LIMIT {
            return Err(invalid());
        }
        let (kind, bytes) = tokio::time::timeout(DEADLINE, read_frame(stream))
            .await
            .map_err(|_| invalid())??;
        if kind != rpc::DATA
            || bytes.len() != *length as usize + 4
            || bytes[..4] != id.to_be_bytes()
        {
            return Err(invalid());
        }
        Some(bytes[4..].to_vec())
    } else {
        None
    };
    Ok(CallResult::Reply(result, data))
}

struct WatchState {
    terminal: Mutex<Option<WatchNotice>>,
    cancel: CancellationToken,
    _permit: tokio::sync::OwnedSemaphorePermit,
    last: AtomicU64,
    expires: Option<tokio::time::Instant>,
}
pub struct ObservationWatch {
    receiver: flume::Receiver<WatchNotice>,
    state: Arc<WatchState>,
}
impl std::fmt::Debug for ObservationWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObservationWatch(<bounded subscription>)")
    }
}
impl ObservationWatch {
    fn terminal(&self) -> Option<WatchNotice> {
        if self
            .state
            .expires
            .is_some_and(|deadline| deadline <= tokio::time::Instant::now())
        {
            self.state.cancel.cancel();
        }
        if self.state.cancel.is_cancelled() {
            while self.receiver.try_recv().is_ok() {}
            return Some(
                self.state
                    .terminal
                    .lock()
                    .ok()?
                    .clone()
                    .unwrap_or(WatchNotice {
                        sequence: self
                            .state
                            .last
                            .load(Ordering::Acquire)
                            .saturating_add(1)
                            .to_string(),
                        event: WatchEvent::Ended,
                    }),
            );
        }
        self.state.terminal.lock().ok()?.clone()
    }
    pub fn try_recv(&self) -> io::Result<Option<WatchNotice>> {
        if let Some(terminal) = self.terminal() {
            return Ok(Some(terminal));
        }
        let notice = match self.receiver.try_recv() {
            Ok(notice) => Some(notice),
            Err(flume::TryRecvError::Empty) => None,
            Err(flume::TryRecvError::Disconnected) => Some(WatchNotice {
                sequence: self
                    .state
                    .last
                    .load(Ordering::Acquire)
                    .saturating_add(1)
                    .to_string(),
                event: WatchEvent::Ended,
            }),
        };
        if let Some(terminal) = self.terminal() {
            return Ok(Some(terminal));
        }
        Ok(notice)
    }
    pub fn recv_timeout(&self, timeout: Duration) -> io::Result<WatchNotice> {
        let deadline = std::time::Instant::now()
            .checked_add(timeout)
            .ok_or_else(invalid)?;
        loop {
            if let Some(notice) = self.try_recv()? {
                return Ok(notice);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self
                .receiver
                .recv_timeout(remaining.min(Duration::from_millis(20)))
            {
                Ok(notice) => return Ok(self.terminal().unwrap_or(notice)),
                Err(flume::RecvTimeoutError::Timeout) => {}
                Err(flume::RecvTimeoutError::Disconnected) => {
                    return Ok(self.terminal().unwrap_or(WatchNotice {
                        sequence: "1".into(),
                        event: WatchEvent::Ended,
                    }))
                }
            }
        }
    }
    pub fn close(&self) {
        self.state.cancel.cancel();
    }
}
impl Drop for ObservationWatch {
    fn drop(&mut self) {
        self.close();
    }
}

fn retire(state: &WatchState, sequence: u64, event: WatchEvent) {
    if let Ok(mut terminal) = state.terminal.lock() {
        *terminal = Some(WatchNotice {
            sequence: sequence.saturating_add(1).to_string(),
            event,
        });
    }
    state.cancel.cancel();
}
async fn start_watch(
    binding: ForwardingBinding,
    target: Target,
    root: String,
    cancel: CancellationToken,
    permit: tokio::sync::OwnedSemaphorePermit,
    expiry: Option<tokio::time::Instant>,
) -> io::Result<ObservationWatch> {
    let mut stream = tokio::time::timeout(DEADLINE, binding.open())
        .await
        .map_err(|_| invalid())??;
    let caps = open(&mut stream, &target).await?;
    if !caps.watch || !caps.roots.iter().any(|r| r.id == root) {
        return Err(invalid());
    }
    let result = exchange(&mut stream, 1, &root, Operation::Watch {}).await?;
    if !matches!(result, CallResult::Reply(Reply::Watching, None)) {
        return Err(invalid());
    }
    let (sender, receiver) = flume::bounded(rpc::NOTICE_LIMIT);
    let state = Arc::new(WatchState {
        terminal: Mutex::new(None),
        cancel,
        _permit: permit,
        last: AtomicU64::new(0),
        expires: expiry,
    });
    let task_state = state.clone();
    tokio::spawn(async move {
        let mut previous = 0u64;
        loop {
            let frame = tokio::select! {
                _ = task_state.cancel.cancelled() => { retire(&task_state, previous, WatchEvent::Ended); return; }
                frame = read_frame(&mut stream) => frame,
            };
            let notice = match frame.and_then(|(kind, bytes)| {
                if kind != rpc::NOTICE {
                    return Err(invalid());
                }
                let notice: WatchNotice = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                if rpc::decimal(&notice.sequence)? != previous.checked_add(1).ok_or_else(invalid)? {
                    return Err(invalid());
                }
                match &notice.event {
                    WatchEvent::Created { path }
                    | WatchEvent::Modified { path }
                    | WatchEvent::Removed { path } => path_check(path)?,
                    WatchEvent::Renamed { from, to } => {
                        path_check(from)?;
                        path_check(to)?;
                    }
                    _ => {}
                }
                Ok(notice)
            }) {
                Ok(notice) => notice,
                Err(error) => {
                    let event = if error.kind() == io::ErrorKind::InvalidData {
                        WatchEvent::RescanRequired
                    } else {
                        WatchEvent::Ended
                    };
                    retire(&task_state, previous, event);
                    return;
                }
            };
            if task_state.cancel.is_cancelled() || !binding.available() {
                retire(&task_state, previous, WatchEvent::Ended);
                return;
            }
            if matches!(notice.event, WatchEvent::Ended | WatchEvent::RescanRequired) {
                retire(&task_state, previous, notice.event);
                return;
            }
            previous += 1;
            task_state.last.store(previous, Ordering::Release);
            if sender.try_send(notice).is_err() {
                retire(&task_state, previous, WatchEvent::RescanRequired);
                return;
            }
        }
    });
    Ok(ObservationWatch { receiver, state })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> Metadata {
        Metadata {
            kind: Kind::File,
            identity: "native:1".into(),
            size: "0".into(),
            modified_ms: None,
            revision: "a".repeat(32),
        }
    }

    #[tokio::test]
    async fn observation_portable_wire_empty_eof_and_changed_are_distinct() {
        let (mut client, mut server) = tokio::io::duplex(128 * 1024);
        let expected = metadata();
        let task = tokio::spawn(async move {
            for id in 1..=2u32 {
                let (kind, bytes) = crate::wire::read_frame(&mut server).await.unwrap();
                assert_eq!(kind, rpc::REQUEST);
                let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(request.id, id);
                let result = if id == 1 {
                    Reply::Data {
                        metadata: metadata(),
                        length: 0,
                    }
                } else {
                    let mut value = metadata();
                    value.revision = "b".repeat(32);
                    Reply::Changed { metadata: value }
                };
                let response = rpc::Response {
                    id,
                    result: Some(result),
                    error: None,
                };
                write_frame(
                    &mut server,
                    rpc::RESPONSE,
                    &serde_json::to_vec(&response).unwrap(),
                )
                .await
                .unwrap();
                if id == 1 {
                    write_frame(&mut server, rpc::DATA, &id.to_be_bytes())
                        .await
                        .unwrap();
                }
            }
        });
        let reply = exchange(
            &mut client,
            1,
            &"a".repeat(32),
            Operation::Read {
                path: "/file".into(),
                offset: "0".into(),
                length: 1,
                expected: expected.clone(),
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(reply, CallResult::Reply(Reply::Data { length:0,.. }, Some(data)) if data.is_empty())
        );
        let reply = exchange(
            &mut client,
            2,
            &"a".repeat(32),
            Operation::Read {
                path: "/file".into(),
                offset: "0".into(),
                length: 1,
                expected,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            reply,
            CallResult::Reply(Reply::Changed { .. }, None)
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn observation_token_expiry_is_independent_and_unknown_expiry_refuses() {
        assert!(
            ObservationDriver::token_expiry("header.eyJleHAiOjQyOTQ5NjcyOTV9.signature").is_some()
        );
        for token in [
            "opaque",
            "header.e30.signature",
            "header.eyJleHAiOjF9.signature",
            "header.eyJleHAiOjQyOTQ5NjcyOTV9.signature.extra",
            "header.!!!!.signature",
        ] {
            assert!(ObservationDriver::token_expiry(token).is_none());
        }
        let parent = CancellationToken::new();
        let (provider, driver) = ObservationRequestor::pending(&parent);
        driver.fence_expiry(Some(
            tokio::time::Instant::now() + Duration::from_millis(30),
        ));
        tokio::time::timeout(Duration::from_secs(1), provider.shared.stopped.cancelled())
            .await
            .unwrap();
        assert!(!parent.is_cancelled());
        assert!(!provider.available());
        assert!(provider.stat(&"a".repeat(32), "/file").is_err());
        assert!(driver.receiver.try_recv().is_err());
        let (provider, driver) = ObservationRequestor::pending(&parent);
        driver.fence_expiry(None);
        assert!(provider.shared.stopped.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn observation_watch_terminal_status_survives_full_buffer_and_parent_cancellation() {
        for event in [WatchEvent::Ended, WatchEvent::RescanRequired] {
            let parent = CancellationToken::new();
            let (sender, receiver) = flume::bounded(rpc::NOTICE_LIMIT);
            let slot = Arc::new(tokio::sync::Semaphore::new(1));
            let state = Arc::new(WatchState {
                terminal: Mutex::new(None),
                cancel: parent.child_token(),
                _permit: slot.try_acquire_owned().unwrap(),
                last: AtomicU64::new(64),
                expires: None,
            });
            for sequence in 1..=rpc::NOTICE_LIMIT {
                sender
                    .try_send(WatchNotice {
                        sequence: sequence.to_string(),
                        event: WatchEvent::Modified {
                            path: "/file".into(),
                        },
                    })
                    .unwrap();
            }
            let watch = ObservationWatch {
                receiver,
                state: state.clone(),
            };
            retire(&state, 64, event.clone());
            let terminal = watch.try_recv().unwrap().unwrap();
            assert_eq!(terminal.event, event);
            assert_eq!(terminal.sequence, "65");
            assert!(watch.receiver.is_empty());
            assert!(!parent.is_cancelled());
        }
        let parent = CancellationToken::new();
        let (_, receiver) = flume::bounded(1);
        let slot = Arc::new(tokio::sync::Semaphore::new(1));
        let watch = ObservationWatch {
            receiver,
            state: Arc::new(WatchState {
                terminal: Mutex::new(None),
                cancel: parent.child_token(),
                _permit: slot.try_acquire_owned().unwrap(),
                last: AtomicU64::new(7),
                expires: None,
            }),
        };
        parent.cancel();
        assert_eq!(
            watch.recv_timeout(Duration::from_secs(1)).unwrap().event,
            WatchEvent::Ended
        );
    }
}
