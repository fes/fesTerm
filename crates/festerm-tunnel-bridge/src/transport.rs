use std::{
    io::{self, Read, Write},
    sync::Arc,
    time::Duration,
};

use festerm_session::{Session, SessionEvent, SessionEventNotifier, SessionLifecycle};
use festerm_sessiond::{NativeSessionIdentity, PersistentSession, RecoverySyncCommand};
#[cfg(test)]
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

pub(crate) use crate::wire::{
    decode_size, encode_size, read_client_frame, read_frame, write_frame, DEADLINE,
};
#[cfg(test)]
pub(crate) use crate::wire::{discover_stream, read_frame_inner};
use crate::{
    files::{FileCall, FileRequestor, FileService, PendingFile, ReadRequest},
    protocol::*,
    relay::{self, Destination},
};

// Desktop callers reach discovery through `transport`; the implementation is portable.
pub use crate::wire::discover;

const QUEUE: usize = 16;

pub fn read_token(stdin: bool) -> io::Result<Zeroizing<String>> {
    if stdin {
        return read_token_line(&mut io::stdin().lock());
    }
    let token = Zeroizing::new(rpassword::prompt_password(
        "Dedicated tunnel-scoped token (hidden): ",
    )?);
    validate_token(&token)
}

fn read_token_line(input: &mut impl Read) -> io::Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new(Vec::new());
    loop {
        let mut byte = [0];
        if input.read(&mut byte)? == 0 || byte[0] == b'\n' {
            break;
        }
        if bytes.len() == 16 * 1024 + 1 {
            return Err(invalid());
        }
        bytes.push(byte[0]);
    }
    validate_token(std::str::from_utf8(&bytes).map_err(|_| invalid())?)
}

fn validate_token(token: &str) -> io::Result<Zeroizing<String>> {
    let token = Zeroizing::new(token.trim().to_owned());
    if token.is_empty() || token.len() > 16 * 1024 || token.bytes().any(|b| !b.is_ascii_graphic()) {
        return Err(invalid());
    }
    Ok(token)
}

pub fn read_destination(path: &std::path::Path) -> io::Result<Destination> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes).map_err(|_| invalid())
}

struct ValidatedStream {
    input: flume::Sender<Vec<u8>>,
    output: flume::Receiver<Vec<u8>>,
    pending: io::Cursor<Vec<u8>>,
    cancel: CancellationToken,
}
impl Drop for ValidatedStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Read for ValidatedStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = Read::read(&mut self.pending, bytes)?;
        if count != 0 {
            return Ok(count);
        }
        let next = match self.output.recv_timeout(Duration::from_millis(20)) {
            Ok(bytes) => bytes,
            Err(flume::RecvTimeoutError::Timeout) => return Err(io::ErrorKind::TimedOut.into()),
            Err(flume::RecvTimeoutError::Disconnected) => return Ok(0),
        };
        self.pending = io::Cursor::new(next);
        Read::read(&mut self.pending, bytes)
    }
}
impl Write for ValidatedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let count = bytes.len().min(CHUNK_LIMIT);
        self.input
            .try_send(bytes[..count].to_vec())
            .map_err(|error| match error {
                flume::TrySendError::Full(_) => io::ErrorKind::WouldBlock,
                flume::TrySendError::Disconnected(_) => io::ErrorKind::BrokenPipe,
            })?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct ClientSession {
    pub session: PersistentSession,
    pub files: Option<FileRequestor>,
}

pub fn start_client(
    destination: Destination,
    token: Zeroizing<String>,
    notifier: Arc<dyn SessionEventNotifier>,
) -> io::Result<ClientSession> {
    let (input, commands) = flume::bounded(QUEUE);
    let (output, events) = flume::bounded(QUEUE);
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let (file_tx, file_rx) = flume::bounded(1);
    static NEXT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let generation = crate::reserve_sequence(&NEXT_GENERATION)?;
    let files = destination.file_read.then(|| FileRequestor {
        sender: file_tx,
        busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        next: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        stopped: cancel.clone(),
        path_style: destination.path_style,
        endpoint_identity: format!(
            "dev-tunnel:{}:{}:{}:{}",
            destination.endpoint.host_id,
            destination
                .endpoint
                .host_public_keys
                .first()
                .map_or("", String::as_str),
            destination.target.pid,
            destination.target.generation
        ),
        generation,
    });
    std::thread::Builder::new()
        .name("festerm-dev-tunnel".into())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                tokio::select! {
                    _ = stopped.cancelled() => {}
                    _ = client_session(destination, token, commands, output, file_rx) => {}
                }
                stopped.cancel();
            });
        })?;
    let session = PersistentSession::from_validated_transport(
        ValidatedStream {
            input,
            output: events,
            pending: io::Cursor::new(Vec::new()),
            cancel,
        },
        notifier,
    )
    .map_err(|_| invalid())?;
    Ok(ClientSession { session, files })
}

async fn client_session(
    destination: Destination,
    token: Zeroizing<String>,
    commands: flume::Receiver<Vec<u8>>,
    output: flume::Sender<Vec<u8>>,
    files: flume::Receiver<FileCall>,
) -> io::Result<()> {
    let mut connection = tokio::time::timeout(DEADLINE, relay::connect(&destination, &token))
        .await
        .map_err(|_| invalid())??;
    drop(token);
    client_io_with_files(
        &mut connection.stream,
        destination.target,
        commands,
        output,
        files,
        FileAccess {
            read: destination.file_read,
            windows_paths: destination.path_style == crate::files::PathStyle::Windows,
        },
    )
    .await
}

async fn client_io_with_files(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    target: Target,
    commands: flume::Receiver<Vec<u8>>,
    output: flume::Sender<Vec<u8>>,
    files: flume::Receiver<FileCall>,
    file_access: FileAccess,
) -> io::Result<()> {
    let hello = serde_json::to_vec(&Hello {
        takeover: true,
        files: file_access,
        version: VERSION,
        snapshot_schema: 3,
        target,
    })
    .map_err(|_| invalid())?;
    write_frame(&mut stream, HELLO, &hello).await?;
    let (kind, capabilities) = tokio::time::timeout(DEADLINE, read_frame(&mut stream))
        .await
        .map_err(|_| invalid())??;
    if kind != CAPABILITIES
        || capabilities.len() != 2
        || capabilities[0] != u8::from(file_access.read)
        || (file_access.read && capabilities[1] != u8::from(file_access.windows_paths))
        || capabilities[1] > 1
    {
        return Err(invalid());
    }
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_frame(&mut stream))
        .await
        .map_err(|_| invalid())??;
    if kind != SNAPSHOT {
        return Err(invalid());
    }
    let terminal = decode_snapshot(&bytes)?;
    drop(bytes);
    let snapshot = festerm_sessiond::encode_recovery_snapshot_with_limit(&terminal, SNAPSHOT_LIMIT)
        .map_err(|_| invalid())?;
    drop(terminal);
    for chunk in snapshot.chunks(CHUNK_LIMIT) {
        output
            .send_async(chunk.to_vec())
            .await
            .map_err(|_| invalid())?;
    }
    drop(snapshot);
    let (read, write) = tokio::io::split(&mut stream);
    let pending = Arc::new(std::sync::Mutex::new(None));
    tokio::try_join!(
        client_send(write, commands, files, pending.clone()),
        client_receive(read, output, pending)
    )?;
    Ok(())
}

async fn client_send(
    mut writer: impl AsyncWrite + Unpin,
    commands: flume::Receiver<Vec<u8>>,
    files: flume::Receiver<FileCall>,
    file_pending: PendingFile,
) -> io::Result<()> {
    let mut pending = Vec::new();
    let mut files_open = true;
    loop {
        let bytes = tokio::select! {
            call = files.recv_async(), if files_open => {
                let Ok(call) = call else { files_open = false; continue; };
                let bytes = serde_json::to_vec(&call.request).map_err(|_| invalid())?;
                {
                    let mut slot = file_pending.lock().map_err(|_| invalid())?;
                    if slot.is_some() { let _ = call.reply.try_send(Err(invalid())); continue; }
                    *slot = Some(call);
                }
                write_frame(&mut writer, FILE_READ, &bytes).await?;
                continue;
            }
            bytes = commands.recv_async() => bytes.map_err(|_| invalid())?,
        };
        if pending.len() + bytes.len() > 2 * CHUNK_LIMIT + 9 {
            return Err(invalid());
        }
        pending.extend(bytes);
        while pending.len() >= 9 {
            if &pending[..4] != b"FSD1" {
                return Err(invalid());
            }
            let length =
                u32::from_be_bytes(pending[5..9].try_into().map_err(|_| invalid())?) as usize;
            if length > CHUNK_LIMIT {
                return Err(invalid());
            }
            if pending.len() < 9 + length {
                break;
            }
            let kind = match pending[4] {
                1 => INPUT,
                2 => RESIZE,
                3 => SYNC,
                4 => ADOPTED,
                _ => return Err(invalid()),
            };
            let bytes = if kind == SYNC {
                let command =
                    festerm_sessiond::decode_recovery_sync_command(&pending[9..9 + length])
                        .map_err(|_| invalid())?;
                serde_json::to_vec(&Sync::from(command)).map_err(|_| invalid())?
            } else {
                pending[9..9 + length].to_vec()
            };
            write_frame(&mut writer, kind, &bytes).await?;
            pending.drain(..9 + length);
        }
    }
}

async fn client_receive(
    mut reader: impl AsyncRead + Unpin,
    output: flume::Sender<Vec<u8>>,
    pending: PendingFile,
) -> io::Result<()> {
    loop {
        let (kind, bytes) = read_frame(&mut reader).await?;
        if kind == FILE_RESULT {
            if bytes.len() < 9 {
                return Err(invalid());
            }
            let id = u64::from_be_bytes(bytes[..8].try_into().map_err(|_| invalid())?);
            let call = pending
                .lock()
                .map_err(|_| invalid())?
                .take()
                .ok_or_else(invalid)?;
            if call.request.id != id || bytes[8] > 1 || (bytes[8] == 1 && bytes.len() != 9) {
                return Err(invalid());
            }
            let result = if bytes[8] == 0 {
                Ok(bytes[9..].to_vec())
            } else {
                Err(invalid())
            };
            let _ = call.reply.try_send(result);
            continue;
        }
        let frame = match kind {
            OUTPUT => festerm_sessiond::encode_server_output_frame(&bytes),
            RESIZED => {
                decode_size(&bytes)?;
                festerm_sessiond::encode_server_resize_applied_frame(&bytes)
            }
            SYNC => {
                let sync: Sync = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                let encoded = festerm_sessiond::encode_recovery_sync_command(&sync.try_into()?)
                    .map_err(|_| invalid())?;
                festerm_sessiond::encode_server_recovery_sync_frame(&encoded)
            }
            EXITED if bytes.is_empty() => festerm_sessiond::encode_server_exited_frame(),
            _ => return Err(invalid()),
        };
        output.send_async(frame).await.map_err(|_| invalid())?;
        if kind == EXITED {
            return Err(invalid());
        }
    }
}

/// Who holds the daemon session now, from the registry alone (no daemon IPC).
fn attached_by(selected: &NativeSessionIdentity) -> Option<Displacer> {
    let root = festerm_sessiond::runtime_root().ok()?;
    festerm_sessiond::list_running_sessions_in(&root)
        .ok()?
        .into_iter()
        .find(|running| {
            running.session.name == selected.name
                && running.session.pid == selected.pid
                && running.session.created_at_unix_ms == selected.created_at_unix_ms
        })?
        .attached_by
        .map(|kind| match kind {
            festerm_sessiond::AttachKind::Local => Displacer::Local,
            festerm_sessiond::AttachKind::Remote => Displacer::Remote,
        })
}

pub async fn serve(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    selected: NativeSessionIdentity,
) -> io::Result<()> {
    serve_with_files(stream, selected, FileService::new(false)).await
}

pub async fn serve_with_files(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    selected: NativeSessionIdentity,
    file_service: FileService,
) -> io::Result<()> {
    serve_with_takeover(stream, selected, file_service, false).await
}

/// `takeover` lets a remote connection displace whichever client holds the session.
pub async fn serve_with_takeover(
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
    selected: NativeSessionIdentity,
    file_service: FileService,
    takeover: bool,
) -> io::Result<()> {
    let target = Target {
        name: selected.name.clone(),
        pid: selected.pid,
        generation: selected.created_at_unix_ms.to_string(),
    };
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
        .await
        .map_err(|_| invalid())??;
    if kind == DISCOVER {
        if bytes != VERSION.to_be_bytes() {
            return Err(invalid());
        }
        let attached_by = attached_by(&selected);
        let description = Description {
            attached_by,
            takeover_allowed: takeover,
            version: VERSION,
            target,
            files: FileAccess {
                read: file_service.allows_read(),
                windows_paths: cfg!(windows),
            },
        };
        let bytes = serde_json::to_vec(&description).map_err(|_| invalid())?;
        write_frame(&mut stream, DESCRIPTION, &bytes).await?;
        return Ok(());
    }
    if kind != HELLO {
        return Err(invalid());
    }
    let hello: Hello = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if hello.version != VERSION
        || hello.snapshot_schema != 3
        || hello.target != target
        || (hello.files.read
            && (!file_service.allows_read() || hello.files.windows_paths != cfg!(windows)))
    {
        write_frame(&mut stream, REFUSED, &[]).await?;
        return Err(invalid());
    }
    let takeover = takeover && hello.takeover;
    let displacement_probe = selected.clone();
    let file_read_admitted = hello.files.read;
    write_frame(
        &mut stream,
        CAPABILITIES,
        &[u8::from(file_read_admitted), u8::from(cfg!(windows))],
    )
    .await?;
    let notify = Arc::new(tokio::sync::Notify::new());
    struct Wake(Arc<tokio::sync::Notify>);
    impl SessionEventNotifier for Wake {
        fn notify(&self) {
            self.0.notify_one();
        }
    }
    let notifier = Arc::new(Wake(notify.clone()));
    let session =
        match tokio::task::spawn_blocking(move || match festerm_sessiond::runtime_root() {
            Ok(root) => PersistentSession::resume_remote_in(
                &selected,
                &root,
                takeover,
                SNAPSHOT_LIMIT as u32,
                notifier,
            ),
            Err(error) => Err(error),
        })
        .await
        .map_err(|_| invalid())?
        {
            Ok(session) => session,
            Err(_) => {
                write_frame(&mut stream, REFUSED, &[]).await?;
                return Err(invalid());
            }
        };
    let terminal = tokio::time::timeout(DEADLINE, async {
        loop {
            let notified = notify.notified();
            if let Some(terminal) = session.take_recovered_terminal_deferred() {
                return Ok(terminal);
            }
            if matches!(
                session.lifecycle(),
                SessionLifecycle::Disconnected(_)
                    | SessionLifecycle::Failed(_)
                    | SessionLifecycle::Stopped
            ) {
                return Err(invalid());
            }
            notified.await;
        }
    })
    .await
    .map_err(|_| invalid())??;
    let snapshot = encode_snapshot(&terminal)?;
    drop(terminal);
    write_frame(&mut stream, SNAPSHOT, &snapshot).await?;
    drop(snapshot);
    let (kind, bytes) = tokio::time::timeout(DEADLINE, read_client_frame(&mut stream))
        .await
        .map_err(|_| invalid())??;
    if kind != ADOPTED || !bytes.is_empty() {
        return Err(invalid());
    }
    session.acknowledge_recovery_adoption();
    let (read, mut write) = tokio::io::split(stream);
    let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
    let reader = async {
        let mut read = read;
        loop {
            let frame = read_client_frame(&mut read).await?;
            tx.send(frame).await.map_err(|_| invalid())?;
        }
        #[allow(unreachable_code)]
        Ok::<(), io::Error>(())
    };
    let process = async {
        type FileFuture =
            std::pin::Pin<Box<dyn std::future::Future<Output = (u64, io::Result<Vec<u8>>)> + Send>>;
        let mut file_job: Option<FileFuture> = None;
        loop {
            let notified = notify.notified();
            for _ in 0..QUEUE {
                let Ok(event) = session.try_recv_event() else {
                    break;
                };
                match event {
                    SessionEvent::Output(bytes) => write_frame(&mut write, OUTPUT, &bytes).await?,
                    SessionEvent::RecoverySync(bytes) => {
                        let sync = festerm_sessiond::decode_recovery_sync_command(&bytes)
                            .map_err(|_| invalid())?;
                        let bytes = serde_json::to_vec(&Sync::from(sync)).map_err(|_| invalid())?;
                        write_frame(&mut write, SYNC, &bytes).await?;
                    }
                    SessionEvent::ResizeApplied(size) => {
                        write_frame(&mut write, RESIZED, &encode_size(size)).await?
                    }
                    SessionEvent::Lifecycle(SessionLifecycle::Exited(_)) => {
                        write_frame(&mut write, EXITED, &[]).await?;
                        return Ok(());
                    }
                    SessionEvent::Lifecycle(SessionLifecycle::Disconnected(error))
                        if error.message() == festerm_sessiond::DISPLACED_REASON =>
                    {
                        let by = attached_by(&displacement_probe).unwrap_or(Displacer::Unknown);
                        write_frame(&mut write, DISPLACED, &[by.code()]).await?;
                        return Ok(());
                    }
                    SessionEvent::Lifecycle(
                        SessionLifecycle::Disconnected(_)
                        | SessionLifecycle::Failed(_)
                        | SessionLifecycle::Stopped,
                    )
                    | SessionEvent::Error(_) => return Err(invalid()),
                    _ => {}
                }
            }
            tokio::select! {
                (id, result) = async { match file_job.as_mut() { Some(job) => job.await, None => std::future::pending().await } } => {
                    file_job = None;
                    write_file_result(&mut write, id, result).await?;
                }
                _ = notified => {}
                frame = rx.recv() => {
                    let (kind, bytes) = frame.ok_or_else(invalid)?;
                    match kind {
                        FILE_READ => {
                            let request: ReadRequest = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                            if !file_read_admitted || file_job.is_some() {
                                write_file_result(&mut write, request.id, Err(invalid())).await?;
                            } else {
                                let files = file_service.clone();
                                file_job = Some(Box::pin(async move { (request.id, files.read(request.path).await) }));
                            }
                        }
                        INPUT => session.try_send_input(&bytes).map_err(|_| invalid())?,
                        RESIZE => session.try_resize(decode_size(&bytes)?).map_err(|_| invalid())?,
                        SYNC => {
                            let sync: Sync = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
                            match RecoverySyncCommand::try_from(sync)? {
                                RecoverySyncCommand::SetScrollbackLimit { limit_bytes } => session.sync_recovery_scrollback_limit(limit_bytes),
                                RecoverySyncCommand::SetColorScheme(colors) => session.sync_recovery_color_scheme(colors),
                                RecoverySyncCommand::MirrorBytes(bytes) => session.sync_recovery_bytes(&bytes),
                                RecoverySyncCommand::ResetToInitialState => session.sync_recovery_reset(),
                            }.map_err(|_| invalid())?;
                        }
                        DETACH if bytes.is_empty() => return Ok(()),
                        _ => return Err(invalid()),
                    }
                }
            }
        }
    };
    tokio::pin!(reader);
    tokio::pin!(process);
    tokio::select! { result = &mut reader => result, result = &mut process => result }
}

async fn write_file_result(
    writer: &mut (impl AsyncWrite + Unpin),
    id: u64,
    result: io::Result<Vec<u8>>,
) -> io::Result<()> {
    let mut response = Vec::with_capacity(9);
    response.extend_from_slice(&id.to_be_bytes());
    match result {
        Ok(bytes) if bytes.len() <= FILE_LIMIT => {
            response.push(0);
            response.extend(bytes);
        }
        _ => response.push(1),
    }
    write_frame(writer, FILE_RESULT, &response).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_core::{Dimensions, Terminal};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn private_token_line_preserves_bound_with_lf_crlf_and_invalid_input() {
        for ending in ["", "\n", "\r\n"] {
            let line = format!("{}{ending}", "a".repeat(16 * 1024));
            let token = read_token_line(&mut line.as_bytes()).unwrap();
            assert_eq!(token.len(), 16 * 1024);
        }
        for line in [
            format!("{}\r\n", "a".repeat(16 * 1024 + 1)),
            "a b\n".to_owned(),
            "é\n".to_owned(),
            "\r\n".to_owned(),
        ] {
            assert!(read_token_line(&mut line.as_bytes()).is_err());
        }
        assert!(read_token_line(&mut &[0xff, b'\n'][..]).is_err());
    }

    struct Wake(AtomicUsize);
    #[tokio::test]
    async fn bridge_discovery_is_bounded_read_only_and_never_opens_daemon_ipc() {
        let (mut client, host) = tokio::io::duplex(8192);
        let selected = NativeSessionIdentity {
            name: "test-owned".into(),
            pid: 7,
            created_at_unix_ms: 9,
            endpoint: "must-not-open-this-local-endpoint".into(),
        };
        let task = tokio::spawn(serve_with_files(host, selected, FileService::new(true)));
        let description = discover_stream(&mut client).await.unwrap();
        assert_eq!(description.target.name, "test-owned");
        assert_eq!(description.target.generation, "9");
        assert!(description.files.read);
        assert_eq!(description.files.windows_paths, cfg!(windows));
        assert!(task.await.unwrap().is_ok());
        for (kind, length, client_direction) in [
            (DISCOVER, 3, true),
            (DESCRIPTION, 4097, false),
            (DESCRIPTION, 1, true),
        ] {
            let mut header = vec![kind];
            header.extend_from_slice(&(length as u32).to_be_bytes());
            assert!(
                read_frame_inner(&mut std::io::Cursor::new(header), client_direction)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn bridge_discovery_refuses_incompatible_or_nonbridge_responses_without_admission() {
        for response in [
            serde_json::json!({"version":999,"target":{"name":"owned","pid":7,"generation":"9"},"files":{"read":false,"windows_paths":false}}),
            serde_json::json!({"version":1,"target":{"name":"owned","pid":7,"generation":"bad"},"files":{"read":false,"windows_paths":false}}),
            serde_json::json!({"version":1,"target":{"name":"owned","pid":7,"generation":"9"},"files":{"read":false,"windows_paths":false},"endpoint":"private-local-path"}),
        ] {
            let (mut client, mut host) = tokio::io::duplex(8192);
            let task = tokio::spawn(async move {
                assert_eq!(read_client_frame(&mut host).await.unwrap().0, DISCOVER);
                write_frame(
                    &mut host,
                    DESCRIPTION,
                    &serde_json::to_vec(&response).unwrap(),
                )
                .await
                .unwrap();
            });
            assert!(discover_stream(&mut client).await.is_err());
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn bridge_refuses_incompatible_hello_before_daemon_admission() {
        for (version, schema, generation) in [
            (VERSION + 1, 3, "9"),
            (VERSION, 2, "9"),
            (VERSION, 3, "wrong"),
        ] {
            let (mut client, host) = tokio::io::duplex(8192);
            let selected = NativeSessionIdentity {
                name: "test-owned".into(),
                pid: 7,
                created_at_unix_ms: 9,
                endpoint: "never-opened".into(),
            };
            let task = tokio::spawn(serve(host, selected));
            let hello = serde_json::to_vec(&Hello {
                takeover: true,
                files: FileAccess::default(),
                version,
                snapshot_schema: schema,
                target: Target {
                    name: "test-owned".into(),
                    pid: 7,
                    generation: generation.into(),
                },
            })
            .unwrap();
            write_frame(&mut client, HELLO, &hello).await.unwrap();
            assert_eq!(read_frame(&mut client).await.unwrap(), (REFUSED, vec![]));
            assert!(task.await.unwrap().is_err());
        }
    }

    #[tokio::test]
    async fn bridge_rejects_wrong_direction_and_oversized_file_frames_before_body_allocation() {
        for (kind, length, client_direction) in [
            (FILE_RESULT, FILE_LIMIT + 10, false),
            (FILE_RESULT, FILE_LIMIT + 9, true),
        ] {
            let mut bytes = vec![kind];
            bytes.extend_from_slice(&(length as u32).to_be_bytes());
            let mut reader = std::io::Cursor::new(bytes);
            assert!(read_frame_inner(&mut reader, client_direction)
                .await
                .is_err());
        }
    }
    impl SessionEventNotifier for Wake {
        fn notify(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn mismatched_file_response_cannot_satisfy_another_request_or_become_terminal_output() {
        let (reply, result) = flume::bounded(1);
        let pending = Arc::new(std::sync::Mutex::new(Some(FileCall {
            request: ReadRequest {
                id: 41,
                path: "/owned/report.md".into(),
            },
            reply,
        })));
        let (output, terminal) = flume::bounded(1);
        let mut frame = vec![FILE_RESULT];
        frame.extend_from_slice(&10_u32.to_be_bytes());
        frame.extend_from_slice(&42_u64.to_be_bytes());
        frame.extend_from_slice(&[0, b'x']);
        assert!(client_receive(std::io::Cursor::new(frame), output, pending)
            .await
            .is_err());
        assert!(result.recv().is_err());
        assert!(terminal.is_empty());
    }

    #[tokio::test]
    async fn remote_session_adopts_before_live_output_and_never_owns_query_replies() {
        let (client, mut host) = tokio::io::duplex(1024);
        let (input, commands) = flume::bounded(QUEUE);
        let (output, events) = flume::bounded(QUEUE);
        let cancel = CancellationToken::new();
        let wake = Arc::new(Wake(AtomicUsize::new(0)));
        let session = PersistentSession::from_validated_transport(
            ValidatedStream {
                input,
                output: events,
                pending: io::Cursor::new(Vec::new()),
                cancel: cancel.clone(),
            },
            wake.clone(),
        )
        .unwrap();
        let target = Target {
            name: "test-owned".into(),
            pid: 7,
            generation: "9".into(),
        };
        let (file_tx, file_rx) = flume::bounded(1);
        let files = FileRequestor {
            sender: file_tx,
            busy: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            stopped: cancel.clone(),
            path_style: crate::files::PathStyle::Posix,
            endpoint_identity: "test-owned-host-key-and-session".into(),
            generation: 1,
        };
        let worker = tokio::spawn(client_io_with_files(
            client,
            target,
            commands,
            output,
            file_rx,
            FileAccess {
                read: true,
                windows_paths: false,
            },
        ));
        let (kind, bytes) = read_frame(&mut host).await.unwrap();
        assert_eq!(kind, HELLO);
        let hello: Hello = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(hello.target.pid, 7);
        write_frame(&mut host, CAPABILITIES, &[1, 0]).await.unwrap();
        let mut terminal = Terminal::new(Dimensions::new(80, 24).unwrap()).unwrap();
        terminal.ingest(b"same retained shell\r\n");
        let snapshot = encode_snapshot(&terminal.recovery_clone()).unwrap();
        write_frame(&mut host, SNAPSHOT, &snapshot).await.unwrap();
        // Before app adoption there is no acknowledgement or replayed input.
        assert!(
            tokio::time::timeout(Duration::from_millis(80), read_frame(&mut host))
                .await
                .is_err()
        );
        let recovered = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(terminal) = session.take_recovered_terminal() {
                    return terminal;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(recovered
            .row_text(0)
            .unwrap()
            .starts_with("same retained shell"));
        assert!(wake.0.load(Ordering::SeqCst) > 0);
        assert!(session.terminal_replies_owned_by_backend());
        assert_eq!(read_frame(&mut host).await.unwrap(), (ADOPTED, vec![]));
        write_frame(&mut host, OUTPUT, b"live\x1b[6n")
            .await
            .unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Ok(SessionEvent::Output(bytes)) = session.try_recv_event() {
                    return bytes;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(bytes, b"live\x1b[6n");
        session.try_send_input(b"one explicit input").unwrap();
        assert_eq!(
            read_frame(&mut host).await.unwrap(),
            (INPUT, b"one explicit input".to_vec())
        );
        let requestor = files.clone();
        let read =
            tokio::task::spawn_blocking(move || requestor.read("/remote/report.md", FILE_LIMIT));
        let (kind, bytes) = read_frame(&mut host).await.unwrap();
        assert_eq!(kind, FILE_READ);
        let request: ReadRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(request.path, "/remote/report.md");
        write_file_result(
            &mut host,
            request.id,
            Ok(b"file snapshot, not terminal output".to_vec()),
        )
        .await
        .unwrap();
        assert_eq!(
            read.await.unwrap().unwrap(),
            b"file snapshot, not terminal output"
        );
        assert!(!matches!(
            session.try_recv_event(),
            Ok(SessionEvent::Output(_))
        ));
        session.try_shutdown().unwrap();
        // Cancellation is independent of queued input/output and closes the
        // attachment, without any daemon terminate operation in this protocol.
        drop(session);
        tokio::time::timeout(Duration::from_secs(2), cancel.cancelled())
            .await
            .unwrap();
        assert!(!files.available());
        assert!(files.read("/remote/report.md", FILE_LIMIT).is_err());
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn malformed_network_lengths_and_unsupported_actions_refuse_before_allocation() {
        let (mut client, mut host) = tokio::io::duplex(32);
        host.write_all(&[INPUT, 0xff, 0xff, 0xff, 0xff])
            .await
            .unwrap();
        assert!(read_frame(&mut client).await.is_err());
        let (mut client, mut host) = tokio::io::duplex(32);
        host.write_all(&[99, 0, 0, 0, 0]).await.unwrap();
        assert!(read_frame(&mut client).await.is_err());
    }

    #[test]
    fn saturated_transport_is_bounded_and_drop_cancels_without_retry() {
        let (input, commands) = flume::bounded(1);
        let (_output, events) = flume::bounded(1);
        let cancel = CancellationToken::new();
        let mut stream = ValidatedStream {
            input,
            output: events,
            pending: io::Cursor::new(Vec::new()),
            cancel: cancel.clone(),
        };
        assert_eq!(stream.write(b"first").unwrap(), 5);
        assert_eq!(
            stream.write(b"never queued").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(stream);
        assert!(cancel.is_cancelled());
        assert_eq!(commands.try_recv().unwrap(), b"first");
        assert!(commands.try_recv().is_err());
    }
}
