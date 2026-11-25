//! Read-only ordinary HOST Files composition. This never attaches a shell.
use super::files::{Connector, LinkEvent, Ready, RowState, TransferRow, Transfers};
use super::model::HostDecision;
use eframe::egui;
use festerm_sftp::{
    finish_listing, path, ConflictPolicy, Direction, Entry, EntryKind, ListOptions, Listing,
    LocalFs, PaneFs, PaneKind, SftpError,
};
use festerm_tunnel_bridge::{file_client, file_protocol as rpc, host_client::ordinary::Files};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::{mpsc, Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Runtime,
};
use tokio_util::sync::CancellationToken;

mod local_download;

const QUEUE: usize = 8;
const HISTORY: usize = 32;
const MAX_DOWNLOAD: u64 = 16 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);
type Reply<T> = mpsc::SyncSender<Result<T, SftpError>>;

enum Command {
    Ping,
    List(String, ListOptions, Reply<Listing>),
    Stat(String, Reply<Entry>),
    Download(u64, String, String, CancellationToken),
}
#[derive(Default)]
struct State {
    next: u64,
    rows: Vec<TransferRow>,
    cancels: HashMap<u64, CancellationToken>,
    failed: bool,
}
struct Lease {
    commands: tokio::sync::mpsc::Sender<Command>,
    cancel: CancellationToken,
    state: Arc<Mutex<State>>,
    wake: egui::Context,
    current: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Lease {
    fn send(&self, command: Command) -> Result<(), SftpError> {
        if self.cancel.is_cancelled() || (self.current)().is_err() {
            self.cancel.cancel();
            return Err(SftpError::Cancelled);
        }
        self.commands
            .try_send(command)
            .map_err(|_| SftpError::Disconnected("Files worker unavailable or busy".into()))
    }
    fn request<T>(&self, build: impl FnOnce(Reply<T>) -> Command) -> Result<T, SftpError> {
        let (send, receive) = mpsc::sync_channel(1);
        self.send(build(send))?;
        receive
            .recv_timeout(DEADLINE + Duration::from_secs(2))
            .map_err(|_| SftpError::Timeout("Files worker".into()))?
    }
}
struct Remote(Arc<Lease>);
fn denied() -> SftpError {
    SftpError::Unsupported("Read-only: this operation is unavailable".into())
}
impl PaneFs for Remote {
    fn kind(&self) -> PaneKind {
        PaneKind::Remote
    }
    fn home(&self) -> String {
        "/".into()
    }
    fn list(&self, p: &str, o: &ListOptions) -> Result<Listing, SftpError> {
        let p = rooted(p)?;
        let options = ListOptions {
            max_scan: o.max_scan.min(festerm_sftp::DEFAULT_MAX_SCAN),
            max_entries: o.max_entries.min(festerm_sftp::DEFAULT_MAX_ENTRIES),
            show_hidden: o.show_hidden,
            filter: o.filter.clone(),
            sort: o.sort,
            order: o.order,
            directories_first: o.directories_first,
            cancel: o.cancel.clone(),
        };
        self.0.request(|reply| Command::List(p, options, reply))
    }
    fn stat(&self, p: &str) -> Result<Entry, SftpError> {
        {
            let p = rooted(p)?;
            self.0.request(|reply| Command::Stat(p, reply))
        }
    }
    fn mkdir(&self, _: &str) -> Result<(), SftpError> {
        Err(denied())
    }
    fn rename(&self, _: &str, _: &str) -> Result<(), SftpError> {
        Err(denied())
    }
    fn delete(&self, _: &str, _: bool) -> Result<(), SftpError> {
        Err(denied())
    }
    fn join(&self, d: &str, n: &str) -> Result<String, SftpError> {
        path::validate_name(n)?;
        rooted(&path::join_remote(d, n)?)
    }
    fn parent(&self, p: &str) -> Option<String> {
        let p = rooted(p).ok()?;
        if p == "/" {
            None
        } else {
            path::parent_remote(&p)
        }
    }
}
fn rooted(p: &str) -> Result<String, SftpError> {
    if p != "/"
        && (!p.starts_with('/')
            || p.len() > 4096
            || p[1..]
                .split('/')
                .any(|part| path::validate_name(part).is_err()))
    {
        return Err(SftpError::InvalidPath(
            "A valid rooted file path is required".into(),
        ));
    }
    Ok(p.into())
}
fn entry(name: String, m: rpc::Metadata) -> Result<Entry, SftpError> {
    let size = m
        .size
        .parse()
        .map_err(|_| SftpError::Remote("Invalid metadata".into()))?;
    Ok(Entry {
        display_name: path::display_name(&name),
        hidden: name.starts_with('.'),
        name_safe: path::validate_name(&name).is_ok(),
        name,
        kind: match m.kind {
            rpc::Kind::File => EntryKind::File,
            rpc::Kind::Directory => EntryKind::Directory,
            rpc::Kind::Unsupported => EntryKind::Other,
        },
        size: Some(size),
        modified: m
            .modified_ms
            .and_then(|v| v.parse::<i64>().ok())
            .map(|v| v / 1000),
        permissions: None,
        symlink: None,
    })
}
fn error(e: file_client::Error) -> SftpError {
    match e {
        file_client::Error::Cancelled => SftpError::Cancelled,
        file_client::Error::Remote {
            code: rpc::ErrorCode::NotFound,
            ..
        } => SftpError::NotFound("File".into()),
        file_client::Error::Remote {
            code: rpc::ErrorCode::Denied,
            ..
        } => SftpError::PermissionDenied("File access".into()),
        _ => SftpError::Disconnected("Connection closed. Reconnect to browse files.".into()),
    }
}
async fn listing<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut file_client::Client<S>,
    p: &str,
    o: &ListOptions,
) -> Result<Listing, SftpError> {
    let h = client.open_directory(p).await.map_err(error)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    let result = async {
        loop {
            if o.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
                return Err(SftpError::Cancelled);
            }
            let page = client
                .read_directory(&h, client.capabilities().page_limit)
                .await
                .map_err(error)?;
            for e in page.entries {
                if entries.len() >= o.max_scan {
                    truncated = true;
                    break;
                }
                let kind = match e.kind {
                    rpc::Kind::File => EntryKind::File,
                    rpc::Kind::Directory => EntryKind::Directory,
                    rpc::Kind::Unsupported => EntryKind::Other,
                };
                entries.push(Entry {
                    display_name: path::display_name(&e.name),
                    hidden: e.name.starts_with('.'),
                    name_safe: path::validate_name(&e.name).is_ok(),
                    name: e.name,
                    kind,
                    size: None,
                    modified: None,
                    permissions: None,
                    symlink: None,
                });
            }
            if page.done || truncated {
                break;
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        client.cancel();
        return Err(error);
    }
    client.close(&h).await.map_err(error)?;
    Ok(finish_listing(p.into(), entries, truncated, o))
}
struct HostTransfers(Arc<Lease>);
impl Transfers for HostTransfers {
    fn rows(&self) -> Vec<TransferRow> {
        self.0.state.lock().unwrap().rows.clone()
    }
    fn enqueue(&self, d: Direction, source: &str, target: &str) -> Result<u64, String> {
        if d != Direction::Download {
            return Err("Read-only: uploads are unavailable".into());
        }
        let source = rooted(source).map_err(|_| "Invalid file path")?;
        let name = path::file_name_remote(&source).map_err(|_| "Invalid file name")?;
        path::validate_name(&name).map_err(|_| "Unsafe file name")?;
        path::normalize_local(target).map_err(|_| "Invalid local folder")?;
        let mut state = self.0.state.lock().map_err(|_| "Files queue unavailable")?;
        if state.failed || state.rows.len() >= HISTORY {
            return Err("Clear finished transfers or reconnect".into());
        }
        state.next = state.next.checked_add(1).ok_or("Files queue exhausted")?;
        let id = state.next;
        let cancel = CancellationToken::new();
        self.0
            .send(Command::Download(id, source, target.into(), cancel.clone()))
            .map_err(|_| "Files queue is busy")?;
        state.cancels.insert(id, cancel);
        state.rows.push(TransferRow {
            id,
            name,
            direction: d,
            target: target.into(),
            state: RowState::Queued,
            done: 0,
            total: None,
            rate: 0.0,
            conflict: None,
            error: None,
        });
        Ok(id)
    }
    fn cancel(&self, id: u64) {
        if let Some(c) = self.0.state.lock().unwrap().cancels.get(&id) {
            c.cancel();
        }
        self.0.wake.request_repaint();
    }
    fn resolve(&self, _: u64, _: ConflictPolicy, _: bool) {}
    fn clear_finished(&self) {
        self.0
            .state
            .lock()
            .unwrap()
            .rows
            .retain(|r| !r.state.finished());
    }
    fn connection_error(&self) -> Option<String> {
        if (self.0.current)().is_err() {
            self.0.cancel.cancel();
        }
        (self.0.cancel.is_cancelled() || self.0.state.lock().unwrap().failed)
            .then(|| "Connection closed. Reconnect to browse files.".into())
    }
}
struct Progress {
    state: Arc<Mutex<State>>,
    id: u64,
    wake: egui::Context,
    started: std::time::Instant,
    last_repaint: std::time::Instant,
}
struct LimitedSink<'a, W> {
    file: &'a mut W,
    written: u64,
    cancel: CancellationToken,
    progress: Option<Progress>,
}
impl<W: AsyncWrite + Unpin> AsyncWrite for LimitedSink<'_, W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.cancel.is_cancelled() {
            return Poll::Ready(Err(std::io::ErrorKind::Interrupted.into()));
        }
        if buf.len() as u64 > MAX_DOWNLOAD.saturating_sub(self.written) {
            return Poll::Ready(Err(std::io::ErrorKind::FileTooLarge.into()));
        }
        let n = std::task::ready!(Pin::new(&mut *self.file).poll_write(cx, buf))?;
        self.written += n as u64;
        let written = self.written;
        if let Some(progress) = &mut self.progress {
            if let Some(row) = progress
                .state
                .lock()
                .unwrap()
                .rows
                .iter_mut()
                .find(|row| row.id == progress.id)
            {
                row.done = written;
                row.rate = written as f64 / progress.started.elapsed().as_secs_f64().max(0.001);
                if row.total.is_some_and(|total| total < written) {
                    row.total = None;
                }
            }
            if progress.last_repaint.elapsed() >= Duration::from_millis(50) {
                progress.wake.request_repaint();
                progress.last_repaint = std::time::Instant::now();
            }
        }
        Poll::Ready(Ok(n))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.file).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut *self.file).poll_shutdown(cx)
    }
}
fn download<S: AsyncRead + AsyncWrite + Unpin>(
    rt: &Runtime,
    client: &mut file_client::Client<S>,
    destination: (&LocalFs, &str),
    source: &str,
    cancel: &CancellationToken,
    publish_guard: &(dyn Fn() -> Result<(), SftpError> + Send + Sync),
    progress: Option<Progress>,
) -> Result<u64, SftpError> {
    let cancelled = if let Some(progress) = &progress {
        // HostTransfers::cancel takes this same lock: cancellation that wins
        // while Queued cannot slip between this gate and the Active claim.
        let mut state = progress.state.lock().unwrap();
        let cancelled = cancel.is_cancelled();
        if let Some(row) = state.rows.iter_mut().find(|row| row.id == progress.id) {
            row.state = if cancelled {
                RowState::Cancelled
            } else {
                RowState::Active
            };
        }
        cancelled
    } else {
        cancel.is_cancelled()
    };
    if cancelled {
        return Err(SftpError::Cancelled);
    }
    let (local, target) = destination;
    let metadata_result = rt.block_on(async {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SftpError::Cancelled),
            result = tokio::time::timeout(DEADLINE, client.stat(source)) => result.map_err(|_| SftpError::Timeout("Connection timed out".into()))?.map_err(error),
        }
    });
    if cancel.is_cancelled() || matches!(metadata_result, Err(SftpError::Timeout(_))) {
        client.cancel();
    }
    let metadata = metadata_result?;
    let total = metadata
        .size
        .parse::<u64>()
        .map_err(|_| SftpError::Remote("Invalid metadata".into()))?;
    if !matches!(metadata.kind, rpc::Kind::File) || total > MAX_DOWNLOAD {
        return Err(SftpError::LimitExceeded(
            "Downloads are limited to 16 MiB".into(),
        ));
    }
    if let Some(progress) = &progress {
        if let Some(row) = progress
            .state
            .lock()
            .unwrap()
            .rows
            .iter_mut()
            .find(|row| row.id == progress.id)
        {
            row.total = Some(total);
        }
    }
    let name = path::file_name_remote(source)?;
    let mut stage = local_download::Stage::create(local, target)?;
    let mut file = stage.writer()?;
    let result = rt.block_on(async {
        let mut sink = LimitedSink { file: &mut file, written: 0, cancel: cancel.clone(), progress };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SftpError::Cancelled),
            result = tokio::time::timeout(DEADLINE, client.download(source, &mut sink)) => result.map_err(|_| SftpError::Timeout("Connection timed out".into()))?.map_err(error),
        };
        tokio::io::AsyncWriteExt::flush(&mut file).await.map_err(|_| SftpError::Io("Download could not be flushed".into()))?;
        result
    });
    drop(file);
    if cancel.is_cancelled() || matches!(result, Err(SftpError::Timeout(_))) {
        client.cancel();
    }
    let result = result.and_then(|bytes| {
        client.check_current().map_err(error)?;
        stage.publish(&name, || {
            if cancel.is_cancelled() {
                return Err(SftpError::Cancelled);
            }
            publish_guard()
        })?;
        Ok(bytes)
    });
    let cleanup = stage.cleanup();
    match (result, cleanup) {
        (_, Err(error)) => Err(error),
        (result, Ok(())) => result,
    }
}

pub(super) struct HostConnector {
    events: mpsc::Receiver<LinkEvent>,
    lease: Arc<Lease>,
}
impl HostConnector {
    pub(super) fn start(
        local_root: std::path::PathBuf,
        cancel: CancellationToken,
        wake: &egui::Context,
        publish_guard: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        prepare: impl FnOnce(&Runtime, &CancellationToken) -> Result<Files, String> + Send + 'static,
    ) -> Self {
        let (events_send, events) = mpsc::sync_channel(1);
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let state = Arc::new(Mutex::new(State::default()));
        let lease = Arc::new(Lease {
            commands: tx,
            cancel: cancel.clone(),
            state: state.clone(),
            wake: wake.clone(),
            current: publish_guard.clone(),
        });
        let weak = Arc::downgrade(&lease);
        let wake = wake.clone();
        let spawn = std::thread::Builder::new()
            .name("mobile-host-files".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| "Files worker unavailable")?;
                    let files = prepare(&rt, &cancel)?;
                    files
                        .scope()
                        .validate()
                        .map_err(|_| "Invalid Files scope")?;
                    let mut client = files.into_client();
                    let local =
                        LocalFs::new(&local_root).map_err(|_| "App Documents unavailable")?;
                    let Some(lease) = weak.upgrade() else {
                        return Ok(());
                    };
                    let ready = Ready {
                        remote: Arc::new(Remote(lease.clone())),
                        local: Arc::new(local.clone()),
                        local_root,
                        transfers: Arc::new(HostTransfers(lease)),
                        start_dir: Some("/".into()),
                        remote_write: false,
                    };
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    events_send
                        .send(LinkEvent::Ready(Box::new(ready)))
                        .map_err(|_| "Files view closed")?;
                    wake.request_repaint();
                    loop {
                        let next = rt.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => None,
                            command = rx.recv() => command,
                            _ = tokio::time::sleep(Duration::from_secs(10)) => Some(Command::Ping),
                        }
                    });
                        let Some(command) = next else {
                            break;
                        };
                        match command {
                            Command::Ping => {
                                let result = rt.block_on(async {
                                    tokio::time::timeout(DEADLINE, client.ping()).await
                                });
                                if !matches!(result, Ok(Ok(()))) {
                                    client.cancel();
                                }
                            }
                            Command::List(path, options, reply) => {
                                let result = rt.block_on(async {
                                    tokio::time::timeout(
                                        DEADLINE,
                                        listing(&mut client, &path, &options),
                                    )
                                    .await
                                });
                                let result = match result {
                                    Ok(result) => result,
                                    Err(_) => {
                                        client.cancel();
                                        Err(SftpError::Timeout("Connection timed out".into()))
                                    }
                                };
                                let _ = reply.send(result);
                            }
                            Command::Stat(path, reply) => {
                                let result = rt.block_on(async {
                                    tokio::time::timeout(DEADLINE, client.stat(&path)).await
                                });
                                let result = match result {
                                    Ok(result) => result.map_err(error),
                                    Err(_) => {
                                        client.cancel();
                                        Err(SftpError::Timeout("Connection timed out".into()))
                                    }
                                }
                                .and_then(|metadata| {
                                    entry(
                                        path::file_name_remote(&path).unwrap_or_default(),
                                        metadata,
                                    )
                                });
                                let _ = reply.send(result);
                            }
                            Command::Download(id, source, target, job_cancel) => {
                                wake.request_repaint();
                                let progress = Progress {
                                    state: state.clone(),
                                    id,
                                    wake: wake.clone(),
                                    started: std::time::Instant::now(),
                                    last_repaint: std::time::Instant::now(),
                                };
                                let result = download(
                                    &rt,
                                    &mut client,
                                    (&local, &target),
                                    &source,
                                    &job_cancel,
                                    publish_guard.as_ref(),
                                    Some(progress),
                                );
                                let mut s = state.lock().unwrap();
                                s.cancels.remove(&id);
                                if let Some(row) = s.rows.iter_mut().find(|row| row.id == id) {
                                    match result {
                                        Ok(n) => {
                                            row.done = n;
                                            row.total = Some(n);
                                            row.state = RowState::Done;
                                        }
                                        Err(e) => {
                                            row.state = if job_cancel.is_cancelled() {
                                                RowState::Cancelled
                                            } else {
                                                RowState::Failed
                                            };
                                            row.error = Some(super::files::describe_error(&e));
                                        }
                                    }
                                }
                            }
                        }
                        wake.request_repaint();
                        if !client.available() {
                            break;
                        }
                    }
                    client.cancel();
                    Ok(())
                };
                let result = run();
                if let Err(message) = result {
                    let _ = events_send.send(LinkEvent::Failed(message));
                }
                let mut s = state.lock().unwrap();
                s.failed = true;
                for row in &mut s.rows {
                    if !row.state.finished() {
                        row.state = RowState::Failed;
                        row.error = Some("Connection closed".into());
                    }
                }
                drop(s);
                wake.request_repaint();
            });
        if spawn.is_err() {
            lease.cancel.cancel();
        }
        Self { events, lease }
    }
}
impl Connector for HostConnector {
    fn poll(&mut self) -> Vec<LinkEvent> {
        if (self.lease.current)().is_err() {
            self.lease.cancel.cancel();
        }
        if self.lease.cancel.is_cancelled() {
            vec![LinkEvent::Failed("Connection closed".into())]
        } else {
            self.events.try_iter().collect()
        }
    }
    fn resolve_host(&mut self, _: HostDecision) {}
    fn submit_password(&mut self, _: String) {}
    fn cancel(&mut self) {
        self.lease.cancel.cancel();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
    struct CountedStream {
        stream: DuplexStream,
        written: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl AsyncRead for CountedStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.stream).poll_read(cx, buf)
        }
    }
    impl AsyncWrite for CountedStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let n = std::task::ready!(Pin::new(&mut self.stream).poll_write(cx, buf))?;
            self.written
                .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
            Poll::Ready(Ok(n))
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.stream).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }
    fn fixture() -> (
        Runtime,
        tempfile::TempDir,
        tempfile::TempDir,
        file_client::Client<CountedStream>,
    ) {
        let (runtime, remote, local, client, _) = counted_fixture();
        (runtime, remote, local, client)
    }
    fn counted_fixture() -> (
        Runtime,
        tempfile::TempDir,
        tempfile::TempDir,
        file_client::Client<CountedStream>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let remote = tempfile::tempdir().unwrap();
        let local = tempfile::tempdir().unwrap();
        std::fs::write(remote.path().join("public.md"), b"# public canary\n").unwrap();
        std::fs::create_dir(remote.path().join("folder")).unwrap();
        let filesystem = festerm_tunnel_bridge::filesystem::Filesystem::open(&rpc::Policy {
            root: remote.path().canonicalize().unwrap(),
            write: false,
            delete: false,
        })
        .unwrap();
        let (stream, mut peer) = tokio::io::duplex(8192);
        runtime.spawn(async move {
            // Only the initial public OPEN dispatch; every subsequent framed
            // operation is served by the real existing FileService.
            assert_eq!(peer.read_u8().await.unwrap(), rpc::OPEN);
            let length = peer.read_u32().await.unwrap() as usize;
            assert!(length <= rpc::JSON_LIMIT);
            let mut bytes = vec![0; length];
            peer.read_exact(&mut bytes).await.unwrap();
            let open: rpc::Open = serde_json::from_slice(&bytes).unwrap();
            let _ = festerm_tunnel_bridge::filesystem::serve(peer, filesystem, open.target, &bytes)
                .await;
        });
        let open:rpc::Open = serde_json::from_value(serde_json::json!({"version":rpc::VERSION,"target":{"name":"mobile-files-fixture","pid":std::process::id(),"generation":"1"}})).unwrap();
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stream = CountedStream {
            stream,
            written: written.clone(),
        };
        let client = runtime
            .block_on(file_client::Client::open(
                stream,
                open.target,
                CancellationToken::new(),
            ))
            .unwrap();
        (runtime, remote, local, client, written)
    }
    #[test]
    fn host_files_real_framed_read_only_listing_download_and_preview_bytes() {
        let (rt, _remote, local, mut client) = fixture();
        let page = rt
            .block_on(listing(&mut client, "/", &ListOptions::default()))
            .unwrap();
        assert!(page
            .entries
            .iter()
            .any(|e| e.name == "folder" && e.is_dir()));
        assert!(page
            .entries
            .iter()
            .any(|e| e.name == "public.md" && e.kind == EntryKind::File));
        let bytes = download(
            &rt,
            &mut client,
            (&LocalFs::new(local.path()).unwrap(), ""),
            "/public.md",
            &CancellationToken::new(),
            &|| Ok(()),
            None,
        )
        .unwrap();
        assert_eq!(bytes, 16);
        assert_eq!(
            std::fs::read(local.path().join("public.md")).unwrap(),
            b"# public canary\n"
        );
        assert_eq!(std::fs::read_dir(local.path()).unwrap().count(), 1);
    }
    #[test]
    fn host_files_download_publication_guard_cutover_keeps_original_and_no_final() {
        let (rt, _remote, local, mut client) = fixture();
        let result = download(
            &rt,
            &mut client,
            (&LocalFs::new(local.path()).unwrap(), ""),
            "/public.md",
            &CancellationToken::new(),
            &|| Err(SftpError::Cancelled),
            None,
        );
        assert!(matches!(result, Err(SftpError::Cancelled)));
        assert_eq!(std::fs::read_dir(local.path()).unwrap().count(), 0);
    }
    #[test]
    fn host_files_queued_cancel_sends_no_request_and_next_download_uses_same_client() {
        let (rt, _remote, local, mut client, written) = counted_fixture();
        let before = written.load(std::sync::atomic::Ordering::SeqCst);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut row = transfer_row();
        row.state = RowState::Queued;
        let state = Arc::new(Mutex::new(State {
            rows: vec![row],
            ..State::default()
        }));
        let progress = Progress {
            state: state.clone(),
            id: 1,
            wake: egui::Context::default(),
            started: std::time::Instant::now(),
            last_repaint: std::time::Instant::now(),
        };
        let local = LocalFs::new(local.path()).unwrap();
        assert!(matches!(
            download(
                &rt,
                &mut client,
                (&local, ""),
                "/public.md",
                &cancel,
                &|| Ok(()),
                Some(progress)
            ),
            Err(SftpError::Cancelled)
        ));
        assert_eq!(
            written.load(std::sync::atomic::Ordering::SeqCst),
            before,
            "pre-cancelled queued item must send no request"
        );
        assert!(
            client.available(),
            "unstarted job must not retire other queued work"
        );
        assert_eq!(state.lock().unwrap().rows[0].state, RowState::Cancelled);
        assert_eq!(std::fs::read_dir(local.root()).unwrap().count(), 0);
        assert_eq!(
            download(
                &rt,
                &mut client,
                (&local, ""),
                "/public.md",
                &CancellationToken::new(),
                &|| Ok(()),
                None
            )
            .unwrap(),
            16
        );
        assert_eq!(
            std::fs::read(local.root().join("public.md")).unwrap(),
            b"# public canary\n"
        );
        assert!(client.available());
    }
    fn transfer_row() -> TransferRow {
        TransferRow {
            id: 1,
            name: "public".into(),
            direction: Direction::Download,
            target: "".into(),
            state: RowState::Active,
            done: 0,
            total: Some(8),
            rate: 0.0,
            conflict: None,
            error: None,
        }
    }
    #[test]
    fn host_files_actual_partial_writes_update_progress_and_cancel_preserves_count() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (mut writer, mut reader) = tokio::io::duplex(4);
            let cancel = CancellationToken::new();
            let state = Arc::new(Mutex::new(State {
                rows: vec![transfer_row()],
                ..State::default()
            }));
            let progress = Progress {
                state: state.clone(),
                id: 1,
                wake: egui::Context::default(),
                started: std::time::Instant::now(),
                last_repaint: std::time::Instant::now(),
            };
            let mut sink = LimitedSink {
                file: &mut writer,
                written: 0,
                cancel: cancel.clone(),
                progress: Some(progress),
            };
            assert_eq!(sink.write(b"public!!").await.unwrap(), 4);
            assert_eq!(state.lock().unwrap().rows[0].done, 4);
            assert_eq!(state.lock().unwrap().rows[0].total, Some(8));
            cancel.cancel();
            assert_eq!(
                sink.write(b"next").await.unwrap_err().kind(),
                std::io::ErrorKind::Interrupted
            );
            assert_eq!(state.lock().unwrap().rows[0].done, 4);
            let mut bytes = [0; 4];
            reader.read_exact(&mut bytes).await.unwrap();
            assert_eq!(&bytes, b"publ");
        });
    }
    #[test]
    fn host_files_late_ready_after_account_retirement_is_not_disclosed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let valid = Arc::new(AtomicBool::new(true));
        let check = valid.clone();
        let (commands, _rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            current: Arc::new(move || {
                if check.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(SftpError::Cancelled)
                }
            }),
        });
        let root = tempfile::tempdir().unwrap();
        let local = LocalFs::new(root.path()).unwrap();
        let (sender, events) = mpsc::sync_channel(1);
        sender
            .send(LinkEvent::Ready(Box::new(Ready {
                remote_write: false,
                remote: Arc::new(Remote(lease.clone())),
                local: Arc::new(local),
                local_root: root.path().into(),
                transfers: Arc::new(HostTransfers(lease.clone())),
                start_dir: Some("/".into()),
            })))
            .unwrap_or_else(|_| panic!("owned Ready fixture channel"));
        let mut connector = HostConnector {
            events,
            lease: lease.clone(),
        };
        valid.store(false, Ordering::SeqCst);
        assert!(matches!(
            connector.poll().as_slice(),
            [LinkEvent::Failed(_)]
        ));
        assert!(lease.cancel.is_cancelled());
    }

    #[test]
    fn host_files_mutations_and_invalid_paths_never_reach_worker() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands: tx,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            current: Arc::new(|| Ok(())),
        });
        let remote = Remote(lease.clone());
        assert!(remote.mkdir("/new").is_err());
        assert!(remote.rename("/a", "/b").is_err());
        assert!(remote.delete("/a", true).is_err());
        assert!(remote.stat("/../outside").is_err());
        assert!(remote.join("/", "../bad").is_err());
        let transfers = HostTransfers(lease);
        assert!(transfers.enqueue(Direction::Upload, "public", "/").is_err());
        assert!(rx.try_recv().is_err());
    }
}
