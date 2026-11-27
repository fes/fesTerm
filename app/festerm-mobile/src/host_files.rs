//! Ordinary Files composition. This never attaches a shell.
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
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
#[cfg(unix)]
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    runtime::Runtime,
};
use tokio_util::sync::CancellationToken;

mod deletion;
mod local_delete;
#[cfg(unix)]
mod local_download;
mod local_download_queue;
mod local_root;
mod local_upload;
mod uploads;

const QUEUE: usize = 8;
const HISTORY: usize = 32;
const MAX_DOWNLOAD: u64 = 16 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);
type Reply<T> = mpsc::SyncSender<Result<T, SftpError>>;

enum Command {
    Ping,
    Delete(deletion::Command),
    LocalDelete(deletion::Command),
    List(String, ListOptions, Reply<Listing>),
    Stat(String, Reply<Entry>),
    Mkdir(String, Reply<()>),
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
    write: std::sync::atomic::AtomicBool,
    delete: std::sync::atomic::AtomicBool,
    uploads: Mutex<Option<Arc<uploads::Uploads>>>,
    root: Mutex<Option<Arc<local_root::Root>>>,
    display_root: Mutex<Option<festerm_tunnel_bridge::file_protocol::DisplayRoot>>,
    locations: Mutex<Option<Vec<rpc::FileLocation>>>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Lease {
    fn location(&self, path: &str) -> Option<rpc::FileLocation> {
        self.locations
            .lock()
            .unwrap()
            .as_ref()?
            .iter()
            .find(|location| location.contains(path))
            .cloned()
    }
    fn mutation_path_available(&self, path: &str) -> bool {
        self.locations
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|locations| {
                locations.iter().any(|location| {
                    location.contains(path) && location.available && location.mutations_available
                })
            })
    }
    fn send_current(&self) -> Result<(), SftpError> {
        if self.cancel.is_cancelled()
            || (self.current)().is_err()
            || self
                .root
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|root| root.check().is_err())
        {
            self.cancel.cancel();
            return Err(SftpError::Cancelled);
        }
        Ok(())
    }
    fn send(&self, command: Command) -> Result<(), SftpError> {
        self.send_current()?;
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
    SftpError::Unsupported("This operation is unavailable".into())
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
        self.0.send_current()?;
        if p == "/" && self.0.locations.lock().unwrap().is_some() {
            return Ok(finish_listing(p, Vec::new(), false, o));
        }
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
    fn mkdir(&self, value: &str) -> Result<(), SftpError> {
        if !self.0.write.load(std::sync::atomic::Ordering::SeqCst)
            || !self.0.mutation_path_available(value)
        {
            return Err(denied());
        }
        let value = rooted(value)?;
        self.0.request(|reply| Command::Mkdir(value, reply))
    }
    fn rename(&self, _: &str, _: &str) -> Result<(), SftpError> {
        Err(denied())
    }
    fn delete(&self, _: &str, _: bool) -> Result<(), SftpError> {
        Err(denied())
    }
    fn join(&self, d: &str, n: &str) -> Result<String, SftpError> {
        if d == "/" && self.0.locations.lock().unwrap().is_some() {
            return Err(denied());
        }
        path::validate_name(n)?;
        rooted(&path::join_remote(d, n)?)
    }
    fn parent(&self, p: &str) -> Option<String> {
        let p = rooted(p).ok()?;
        if p == "/"
            || self
                .0
                .location(&p)
                .is_some_and(|location| location.virtual_root() == p)
        {
            None
        } else {
            path::parent_remote(&p)
        }
    }
}
/// Validate optional server metadata before the worker can publish Ready.
async fn initial_directory<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut file_client::Client<S>,
    navigation: file_client::NavigationPreference,
) -> Result<String, &'static str> {
    client
        .check_current()
        .map_err(|_| "Files connection cancelled")?;
    let locations = client
        .locations()
        .await
        .map_err(|_| "Files locations unavailable")?;
    client
        .check_current()
        .map_err(|_| "Files connection cancelled")?;
    if navigation == file_client::NavigationPreference::ChooseLocation {
        if locations.is_none() {
            return Err("This host does not support choosing an allowed location");
        }
        // Explicit user intent, fresh metadata, and no filesystem request.
        // Remote::list handles this synthetic chooser locally.
        return Ok("/".into());
    }
    let start = client
        .start_directory()
        .await
        .map_err(|_| "Files starting folder unavailable")?;
    if locations.as_ref().is_some_and(|locations| {
        start != "/"
            && !locations
                .iter()
                .any(|location| location.available && location.contains(&start))
    }) {
        return Err("Files starting folder unavailable");
    }
    rooted(&start).map_err(|_| "Files starting folder unavailable")
}

/// Namespaced default starts are not Ready until the actual first directory
/// listing succeeds. Carry that snapshot into the pane rather than reissuing
/// the same request after admission. The explicit chooser stays entirely local.
async fn initial_listing<S: AsyncRead + AsyncWrite + Unpin>(
    client: &mut file_client::Client<S>,
    start: &str,
) -> Result<Option<Listing>, &'static str> {
    if client.location_metadata().is_none() {
        return Ok(None);
    }
    client
        .check_current()
        .map_err(|_| "Files connection cancelled")?;
    let options = ListOptions::default();
    let snapshot = if start == "/" {
        finish_listing(start.into(), Vec::new(), false, &options)
    } else {
        match tokio::time::timeout(DEADLINE, listing(client, start, &options)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(_)) => return Err("Files starting folder unavailable"),
            Err(_) => {
                client.cancel();
                return Err("Files starting folder unavailable");
            }
        }
    };
    client
        .check_current()
        .map_err(|_| "Files connection cancelled")?;
    Ok(Some(snapshot))
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
fn permissions(read_only: bool) -> u32 {
    if read_only {
        0o500
    } else {
        0o700
    }
}
fn entry(name: String, m: rpc::Metadata, permissions: Option<u32>) -> Result<Entry, SftpError> {
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
        permissions,
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
    let h = client.open_readable_directory(p).await.map_err(error)?;
    let mut entries = Vec::new();
    let mut truncated = false;
    let result = async {
        let directory_read_only = client
            .directory_metadata(&h)
            .map_err(error)?
            .map(|opened| opened.read_only);
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
                    permissions: directory_read_only.map(|_| permissions(e.read_only)),
                    symlink: None,
                });
            }
            if page.done || truncated {
                break;
            }
        }
        Ok(directory_read_only)
    }
    .await;
    let directory_read_only = match result {
        Ok(value) => value,
        Err(error) => {
            client.cancel();
            return Err(error);
        }
    };
    client.close(&h).await.map_err(error)?;
    let mut snapshot = finish_listing(p.into(), entries, truncated, o);
    snapshot.directory_read_only = directory_read_only;
    Ok(snapshot)
}
struct HostTransfers(Arc<Lease>);
impl Transfers for HostTransfers {
    fn remote_locations(&self) -> Option<Vec<super::files::RemoteLocation>> {
        let locations = self.0.locations.lock().unwrap();
        let locations = locations.as_ref()?;
        // Preserve the new-scope distinction after retirement without disclosing
        // its former labels or silently adopting the legacy namespace.
        if self.0.cancel.is_cancelled() || (self.0.current)().is_err() {
            return Some(Vec::new());
        }
        Some(
            locations
                .iter()
                .map(|location| super::files::RemoteLocation {
                    path: location.virtual_root(),
                    label: path::display_name(location.label.as_str()),
                    available: location.available,
                    mutations_available: location.mutations_available,
                })
                .collect(),
        )
    }
    fn remote_root_label(&self) -> String {
        // Paint may run while lifecycle polling is paused. This is a local
        // authority check only: no Documents stat or remote liveness request.
        if self.0.cancel.is_cancelled() || (self.0.current)().is_err() {
            return "Shared folder".into();
        }
        self.0.display_root.lock().unwrap().as_ref().map_or_else(
            || "Shared folder".into(),
            |root| festerm_sftp::path::display_name(root.as_str()),
        )
    }

    fn retains_preview_copies(&self) -> bool {
        true
    }
    fn prepare_preview_cache(&self) -> Option<Result<String, SftpError>> {
        Some(self.0.send_current().and_then(|()| {
            self.0
                .root
                .lock()
                .unwrap()
                .as_ref()
                .ok_or_else(denied)?
                .prepare_preview()
        }))
    }
    fn read_local_markdown(&self, path: &str, limit: usize) -> Option<Result<String, String>> {
        Some(
            self.0
                .root
                .lock()
                .unwrap()
                .as_ref()
                .ok_or_else(|| "Local preview is unavailable".to_owned())
                .and_then(|root| root.read_text(path, limit)),
        )
    }
    fn delete_available(&self) -> bool {
        self.0.delete.load(std::sync::atomic::Ordering::SeqCst) && self.0.send_current().is_ok()
    }
    fn review_delete(
        &self,
        paths: Vec<String>,
        stop: CancellationToken,
    ) -> Result<super::files::deletion::Reply<super::files::deletion::Review>, String> {
        if !self.delete_available()
            || paths
                .iter()
                .any(|path| !self.0.mutation_path_available(path))
        {
            return Err("Deletion is unavailable".into());
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.0
            .send(Command::Delete(deletion::Command::Prepare(
                paths, stop, send,
            )))
            .map_err(|_| "Connection closed")?;
        Ok(receive)
    }
    fn commit_delete(
        &self,
        id: u64,
    ) -> Result<super::files::deletion::Reply<super::files::deletion::Outcome>, String> {
        if !self.delete_available() {
            return Err("Deletion is unavailable".into());
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.0
            .send(Command::Delete(deletion::Command::Commit(id, send)))
            .map_err(|_| "Connection closed")?;
        Ok(receive)
    }
    fn cancel_delete(&self) {
        // The UI cancels the shared stop token synchronously before this
        // best-effort close request; a full queue cannot permit a stale commit.
        let _ = self.0.send(Command::Delete(deletion::Command::Cancel));
    }
    fn local_delete_available(&self) -> bool {
        cfg!(unix)
            && !self.0.cancel.is_cancelled()
            && (self.0.current)().is_ok()
            && self.0.root.lock().unwrap().is_some()
            && !self.0.cancel.is_cancelled()
    }
    fn review_local_delete(
        &self,
        paths: Vec<String>,
        stop: CancellationToken,
    ) -> Result<super::files::deletion::Reply<super::files::deletion::Review>, String> {
        if !self.local_delete_available() {
            return Err("Deletion is unavailable".into());
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.0
            .commands
            .try_send(Command::LocalDelete(deletion::Command::Prepare(
                paths, stop, send,
            )))
            .map_err(|_| "Files is busy")?;
        Ok(receive)
    }
    fn commit_local_delete(
        &self,
        id: u64,
    ) -> Result<super::files::deletion::Reply<super::files::deletion::Outcome>, String> {
        if !self.local_delete_available() {
            return Err("Deletion is unavailable".into());
        }
        let (send, receive) = mpsc::sync_channel(1);
        self.0
            .commands
            .try_send(Command::LocalDelete(deletion::Command::Commit(id, send)))
            .map_err(|_| "Files is busy")?;
        Ok(receive)
    }
    fn cancel_local_delete(&self) {
        let _ = self
            .0
            .commands
            .try_send(Command::LocalDelete(deletion::Command::Cancel));
    }
    fn rows(&self) -> Vec<TransferRow> {
        if let Some(uploads) = self.0.uploads.lock().unwrap().clone() {
            uploads.poll();
        }
        let mut rows = self.0.state.lock().unwrap().rows.clone();
        for row in &mut rows {
            if row.direction == Direction::Upload {
                row.target = self.remote_path_label(&row.target);
            }
        }
        rows
    }
    fn enqueue(&self, d: Direction, source: &str, target: &str) -> Result<u64, String> {
        if d == Direction::Upload {
            self.0.send_current().map_err(|_| "Connection closed")?;
            if !self.0.write.load(std::sync::atomic::Ordering::SeqCst)
                || !self.0.mutation_path_available(target)
            {
                return Err("Uploads are unavailable".into());
            }
            let uploads = self
                .0
                .uploads
                .lock()
                .unwrap()
                .clone()
                .ok_or("Uploads are unavailable")?;
            let target = rooted(target).map_err(|_| "Invalid upload destination")?;
            let name = path::local_components(source)
                .map_err(|_| "Invalid local file")?
                .last()
                .ok_or("Invalid local file")?
                .to_string();
            let id = {
                let mut state = self.0.state.lock().unwrap();
                if state.failed || state.rows.len() >= HISTORY {
                    return Err("Clear finished transfers or reconnect".into());
                }
                state.next = state.next.checked_add(1).ok_or("Files queue exhausted")?;
                let id = state.next;
                state.rows.push(TransferRow {
                    id,
                    name,
                    direction: d,
                    target: target.clone(),
                    state: RowState::Queued,
                    done: 0,
                    total: None,
                    rate: 0.0,
                    conflict: None,
                    error: None,
                });
                id
            };
            if let Err(message) = uploads.enqueue(id, source, &target) {
                self.0.state.lock().unwrap().rows.retain(|row| row.id != id);
                return Err(message);
            }
            self.0.wake.request_repaint();
            return Ok(id);
        }
        self.0.send_current().map_err(|_| "Connection closed")?;
        let source = rooted(source).map_err(|_| "Invalid file path")?;
        let name = path::file_name_remote(&source).map_err(|_| "Invalid file name")?;
        path::validate_name(&name).map_err(|_| "Unsafe file name")?;
        let target = path::normalize_local(target).map_err(|_| "Invalid local folder")?;
        let root = self
            .0
            .root
            .lock()
            .unwrap()
            .clone()
            .ok_or("Downloads are unavailable")?;
        let directory = local_download_queue::directory(&root, &target)?;
        let queue = self
            .0
            .uploads
            .lock()
            .unwrap()
            .clone()
            .ok_or("Downloads are unavailable")?;
        let id = {
            let mut state = self.0.state.lock().map_err(|_| "Files queue unavailable")?;
            if state.failed || state.rows.len() >= HISTORY {
                return Err("Clear finished transfers or reconnect".into());
            }
            state.next = state.next.checked_add(1).ok_or("Files queue exhausted")?;
            let id = state.next;
            state.rows.push(TransferRow {
                id,
                name,
                direction: d,
                target: target.clone(),
                state: RowState::Queued,
                done: 0,
                total: None,
                rate: 0.0,
                conflict: None,
                error: None,
            });
            id
        };
        if let Err(message) = queue.enqueue_download(id, &source, directory) {
            self.0.state.lock().unwrap().rows.retain(|row| row.id != id);
            return Err(message);
        }
        self.0.wake.request_repaint();
        Ok(id)
    }

    fn cancel(&self, id: u64) {
        if self
            .0
            .uploads
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|uploads| uploads.cancel(id))
        {
            return;
        }
        if let Some(c) = self.0.state.lock().unwrap().cancels.get(&id) {
            c.cancel();
        }
        self.0.wake.request_repaint();
    }
    fn resolve(&self, id: u64, policy: ConflictPolicy, remember: bool) {
        if let Some(uploads) = self.0.uploads.lock().unwrap().as_ref() {
            uploads.resolve(id, policy, remember);
        }
    }
    fn clear_finished(&self) {
        if let Some(uploads) = self.0.uploads.lock().unwrap().as_ref() {
            uploads.clear_finished();
        }
        self.0
            .state
            .lock()
            .unwrap()
            .rows
            .retain(|r| !r.state.finished());
    }
    fn connection_error(&self) -> Option<String> {
        if let Some(message) = self
            .0
            .uploads
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|uploads| uploads.failure())
        {
            self.0.cancel.cancel();
            return Some(message);
        }
        if self.0.send_current().is_err() {
            self.0.cancel.cancel();
        }
        (self.0.cancel.is_cancelled() || self.0.state.lock().unwrap().failed)
            .then(|| "Connection closed. Reconnect to browse files.".into())
    }
}
#[cfg(all(test, unix))]
struct Progress {
    state: Arc<Mutex<State>>,
    id: u64,
    wake: egui::Context,
    started: std::time::Instant,
    last_repaint: std::time::Instant,
}
#[cfg(all(test, unix))]
struct LimitedSink<'a, W> {
    file: &'a mut W,
    written: u64,
    cancel: CancellationToken,
    progress: Option<Progress>,
}
#[cfg(all(test, unix))]
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
#[cfg(all(test, unix))]
fn download<S: AsyncRead + AsyncWrite + Unpin>(
    rt: &Runtime,
    client: &mut file_client::Client<S>,
    destination: (&Arc<local_root::Root>, &str),
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
            result = tokio::time::timeout(DEADLINE, client.stat_readable_path(source)) => result.map_err(|_| SftpError::Timeout("Connection timed out".into()))?.map_err(error),
        }
    });
    if cancel.is_cancelled() || matches!(metadata_result, Err(SftpError::Timeout(_))) {
        client.cancel();
    }
    let metadata = metadata_result?.metadata;
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
            result = tokio::time::timeout(DEADLINE, client.download_readable_path(source, &mut sink)) => result.map_err(|_| SftpError::Timeout("Connection timed out".into()))?.map_err(error),
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

fn run_files_worker(run: impl FnOnce(&Runtime) -> Result<(), String>) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Files worker unavailable")?;
    // The russh channel destructor spawns its close task during synchronous
    // cleanup, including returns before the Files session becomes ready.
    let _entered = runtime.enter();
    run(&runtime)
}

pub(super) struct HostConnector {
    events: mpsc::Receiver<LinkEvent>,
    lease: Arc<Lease>,
}
impl HostConnector {
    pub(super) fn start(
        local_root: std::path::PathBuf,
        navigation: file_client::NavigationPreference,
        cancel: CancellationToken,
        wake: &egui::Context,
        publish_guard: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync>,
        prepare: impl FnOnce(&Runtime, &CancellationToken) -> Result<(Files, Option<Files>), String>
            + Send
            + 'static,
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
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(None),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
        });
        let weak = Arc::downgrade(&lease);
        let wake = wake.clone();
        let spawn = std::thread::Builder::new()
            .name("mobile-host-files".into())
            .spawn(move || {
                let result = run_files_worker(|rt| {
                    let (files, transfer) = prepare(rt, &cancel)?;
                    files
                        .scope()
                        .validate()
                        .map_err(|_| "Invalid Files scope")?;
                    let mut endpoint = festerm_tunnel_bridge::files_endpoint::FilesEndpoint::new(
                        files,
                        cancel.clone(),
                        Default::default(),
                        festerm_tunnel_bridge::files_endpoint::FilesChannel::Browsing,
                    );
                    let client = endpoint
                        .client_without_reviews()
                        .map_err(|_| "Files connection closed")?;
                    let root = local_root::Root::open(&local_root)
                        .map_err(|_| "App Documents unavailable or changed")?;
                    let local = root
                        .local_fs()
                        .map_err(|_| "App Documents unavailable or changed")?;
                    let start_dir = rt.block_on(initial_directory(client, navigation))?;
                    let initial_remote_listing = rt.block_on(initial_listing(client, &start_dir))?;
                    let locations = client.location_metadata().map(|locations| locations.to_vec());
                    let display_root = rt
                        .block_on(client.display_root())
                        .map_err(|_| "Files folder information unavailable")?;
                    let write = client.capabilities().write;
                    let delete = client.delete_advertised();
                    let Some(lease) = weak.upgrade() else {
                        return Ok(());
                    };
                    *lease.display_root.lock().unwrap() = display_root;
                    *lease.locations.lock().unwrap() = locations;
                    *lease.root.lock().unwrap() = Some(root.clone());
                    lease
                        .delete
                        .store(delete, std::sync::atomic::Ordering::SeqCst);
                    lease
                        .write
                        .store(write, std::sync::atomic::Ordering::SeqCst);
                    let sources = Arc::new(local_upload::Sources::new(
                        root.clone(),
                        publish_guard.clone(),
                    ));
                    if let Some(files) = transfer {
                        let uploads = rt.block_on(async {
                            uploads::Uploads::new(
                                files,
                                sources.clone(),
                                local_download_queue::Destinations::new(root.clone(), publish_guard.clone(), cancel.clone()),
                                state.clone(),
                                cancel.child_token(),
                            )
                        });
                        *lease.uploads.lock().unwrap() = Some(Arc::new(uploads));
                    }
                    let ready = Ready {
                        remote: Arc::new(Remote(lease.clone())),
                        local: Arc::new(local_upload::Documents {
                            local: local.clone(),
                            sources,
                        }),
                        local_root,
                        transfers: Arc::new(HostTransfers(lease)),
                        start_dir: Some(start_dir),
                        initial_remote_listing,
                        remote_write: write,
                    };
                    if cancel.is_cancelled() || (publish_guard)().is_err() {
                        return Ok(());
                    }
                    events_send
                        .send(LinkEvent::Ready(Box::new(ready)))
                        .map_err(|_| "Files view closed")?;
                    wake.request_repaint();
                    let repaint = wake.clone();
                    let mut local_delete = local_delete::Owner::new(root.clone(), publish_guard.clone(), cancel.clone(), Arc::new(move || repaint.request_repaint()));
                    loop {
                        let local_stop = local_delete.stop();
                        let next = rt.block_on(async {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => None,
                            _ = local_stop.cancelled() => Some(Command::LocalDelete(deletion::Command::Cancel)),
                            command = rx.recv() => command,
                            _ = tokio::time::sleep(Duration::from_secs(10)) => Some(Command::Ping),
                        }
                    });
                        let Some(command) = next else {
                            break;
                        };
                        if (publish_guard)().is_err() || root.check().is_err() {
                            break;
                        }
                        if let Command::LocalDelete(command) = command {
                            local_delete.execute(command);
                            wake.request_repaint();
                            continue;
                        }
                        if !matches!(command, Command::Ping) { local_delete.retire(); }
                        if let Command::Delete(command) = command {
                            if !rt.block_on(deletion::execute(&mut endpoint, command)) {
                                break;
                            }
                            wake.request_repaint();
                            continue;
                        }
                        if matches!(command, Command::Ping) {
                            if !matches!(
                                rt.block_on(async {
                                    tokio::time::timeout(DEADLINE, endpoint.ping()).await
                                }),
                                Ok(Ok(()))
                            ) {
                                break;
                            }
                            continue;
                        }
                        // Navigation and transfers retire the old review before
                        // exposing the raw serialized client again.
                        if !matches!(
                            rt.block_on(async {
                                tokio::time::timeout(DEADLINE, endpoint.cancel_delete_review())
                                    .await
                            }),
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                        let Ok(client) = endpoint.client_without_reviews() else {
                            break;
                        };
                        match command {
                            Command::LocalDelete(_) | Command::Delete(_) => unreachable!("handled before client access"),
                            Command::Mkdir(path, reply) => {
                                let result = rt.block_on(async {
                                    tokio::time::timeout(DEADLINE, client.mkdir(&path)).await
                                });
                                let result = match result {
                                    Ok(result) => result.map_err(error),
                                    Err(_) => {
                                        client.cancel();
                                        Err(SftpError::Timeout("Connection timed out".into()))
                                    }
                                };
                                let _ = reply.send(result);
                            }
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
                                    tokio::time::timeout(DEADLINE, listing(client, &path, &options))
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
                                let tagged = client.read_links_advertised();
                                let result = rt.block_on(async {
                                    tokio::time::timeout(DEADLINE, client.stat_readable_path(&path)).await
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
                                        metadata.metadata,
                                        tagged.then(|| permissions(metadata.read_only)),
                                    )
                                });
                                let _ = reply.send(result);
                            }

                        }
                        wake.request_repaint();
                        if !client.available() {
                            break;
                        }
                    }
                    cancel.cancel();
                    drop(endpoint);
                    if let Some(lease) = weak.upgrade() {
                        let uploads = lease.uploads.lock().unwrap().clone();
                        if let Some(uploads) = uploads {
                            rt.block_on(uploads.shutdown());
                        }
                    }
                    Ok(())
                });
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
        if self.lease.send_current().is_err() {
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
    #[derive(Default)]
    struct WriteGate {
        writes: std::sync::atomic::AtomicUsize,
        entered: std::sync::atomic::AtomicBool,
        open: std::sync::atomic::AtomicBool,
        waker: Mutex<Option<std::task::Waker>>,
    }
    struct CountedStream {
        stream: DuplexStream,
        written: Arc<std::sync::atomic::AtomicUsize>,
        gate: Option<Arc<WriteGate>>,
        capture: Option<Arc<Mutex<Vec<u8>>>>,
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
            if let Some(gate) = &self.gate {
                if buf.len() >= 1024
                    && gate.writes.load(std::sync::atomic::Ordering::SeqCst) >= rpc::CHUNK_LIMIT
                    && !gate.open.load(std::sync::atomic::Ordering::SeqCst)
                {
                    *gate.waker.lock().unwrap() = Some(cx.waker().clone());
                    gate.entered
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    return Poll::Pending;
                }
            }
            let n = std::task::ready!(Pin::new(&mut self.stream).poll_write(cx, buf))?;
            if n > 0 && buf.len() >= 1024 {
                if let Some(gate) = &self.gate {
                    gate.writes
                        .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
                }
            }
            self.written
                .fetch_add(n, std::sync::atomic::Ordering::SeqCst);
            if let Some(capture) = &self.capture {
                let mut capture = capture.lock().unwrap();
                assert!(capture.len() + n <= rpc::JSON_LIMIT);
                capture.extend_from_slice(&buf[..n]);
            }
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
        policy_fixture(false)
    }
    fn policy_fixture(
        write: bool,
    ) -> (
        Runtime,
        tempfile::TempDir,
        tempfile::TempDir,
        file_client::Client<CountedStream>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        gated_policy_fixture(write, None)
    }
    fn gated_policy_fixture(
        write: bool,
        gate: Option<Arc<WriteGate>>,
    ) -> (
        Runtime,
        tempfile::TempDir,
        tempfile::TempDir,
        file_client::Client<CountedStream>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        captured_policy_fixture(write, gate, None)
    }
    fn captured_policy_fixture(
        write: bool,
        gate: Option<Arc<WriteGate>>,
        capture: Option<Arc<Mutex<Vec<u8>>>>,
    ) -> (
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
            start_in: None,
            locations: None,
            root: remote.path().canonicalize().unwrap(),
            write,
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
            gate,
            capture,
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
    fn host_files_mobile_download_choices_use_read_only_host_and_retained_local_publication() {
        use festerm_tunnel_bridge::files_endpoint::{FilesChannel, FilesEndpoint, FirstFailure};
        use std::sync::atomic::{AtomicBool, Ordering};
        for (policy, changed, retired, cancelled) in [
            (ConflictPolicy::Skip, false, false, false),
            (ConflictPolicy::KeepBoth, false, false, false),
            (ConflictPolicy::Replace, false, false, false),
            (ConflictPolicy::Replace, true, false, false),
            (ConflictPolicy::Replace, false, true, false),
            (ConflictPolicy::Replace, false, false, true),
        ] {
            let (rt, remote, local, client, _) = policy_fixture(false);
            let root = local_root::Root::open(local.path()).unwrap();
            let target = root.root().to_owned();
            std::fs::write(target.join("public.md"), b"old local").unwrap();
            std::fs::write(target.join("public (copy).md"), b"keep existing copy").unwrap();
            let current = Arc::new(AtomicBool::new(true));
            let checked = current.clone();
            let guard: Arc<dyn Fn() -> Result<(), SftpError> + Send + Sync> = Arc::new(move || {
                if checked.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(SftpError::Cancelled)
                }
            });
            let sources = Arc::new(local_upload::Sources::new(root.clone(), guard.clone()));
            let mut row = transfer_row();
            row.id = 1;
            row.direction = Direction::Download;
            row.state = RowState::Queued;
            let state = Arc::new(Mutex::new(State {
                rows: vec![row],
                ..State::default()
            }));
            rt.block_on(async {
                let failure = FirstFailure::default();
                let endpoint = FilesEndpoint::from_client(
                    client,
                    CancellationToken::new(),
                    failure.clone(),
                    FilesChannel::Transfer,
                );
                let downloads =
                    local_download_queue::Destinations::new(root, guard, CancellationToken::new());
                let manager =
                    festerm_ssh::SftpTransferManager::from_remote_endpoint_with_local_files(
                        endpoint,
                        sources.clone(),
                        downloads,
                    );
                let transfers =
                    uploads::Uploads::from_manager(manager, sources, state.clone(), failure);
                transfers
                    .enqueue_download(1, "/public.md", target.clone())
                    .unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        transfers.poll();
                        let status = state.lock().unwrap().rows[0].state;
                        if status == RowState::Awaiting {
                            break;
                        }
                        assert!(
                            !status.finished(),
                            "download failed before review: {:?}",
                            state.lock().unwrap().rows
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                {
                    let state = state.lock().unwrap();
                    let view = state.rows[0].conflict.as_ref().unwrap();
                    assert_eq!(view.direction, Direction::Download);
                    assert_eq!(view.iphone.size, Some(9));
                    assert_eq!(view.remote.size, Some(b"# public canary\n".len() as u64));
                    // The suggestion is not a reservation; Keep Both must recheck names.
                    assert_eq!(view.suggested_name, "public (copy).md");
                    assert!(view.allowed.contains(&ConflictPolicy::Replace));
                }
                if changed {
                    std::fs::rename(target.join("public.md"), target.join("held-original.md"))
                        .unwrap();
                    std::fs::write(target.join("public.md"), b"unreviewed replacement").unwrap();
                }
                if retired {
                    current.store(false, Ordering::SeqCst);
                }
                if cancelled {
                    transfers.cancel(1);
                } else {
                    transfers.resolve(1, policy, false);
                }
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        transfers.poll();
                        if state.lock().unwrap().rows[0].state.finished() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                let expected = if changed || retired {
                    RowState::Failed
                } else if cancelled {
                    RowState::Cancelled
                } else if policy == ConflictPolicy::Skip {
                    RowState::Skipped
                } else {
                    RowState::Done
                };
                {
                    let state = state.lock().unwrap();
                    assert_eq!(state.rows[0].state, expected, "{:?}", state.rows);
                }
                if expected == RowState::Done {
                    let state = state.lock().unwrap();
                    assert_eq!(state.rows[0].done, b"# public canary\n".len() as u64);
                    assert_eq!(state.rows[0].total, Some(b"# public canary\n".len() as u64));
                }
                transfers.shutdown().await;
            });
            assert_eq!(
                std::fs::read(remote.path().join("public.md")).unwrap(),
                b"# public canary\n"
            );
            assert_eq!(
                std::fs::read(target.join("public (copy).md")).unwrap(),
                b"keep existing copy"
            );
            assert_eq!(
                std::fs::read(target.join("public.md")).unwrap(),
                if changed {
                    b"unreviewed replacement".as_slice()
                } else if policy == ConflictPolicy::Replace && !retired && !cancelled {
                    b"# public canary\n".as_slice()
                } else {
                    b"old local".as_slice()
                }
            );
            if changed {
                assert_eq!(
                    std::fs::read(target.join("held-original.md")).unwrap(),
                    b"old local"
                );
            }
            let entries = std::fs::read_dir(&target)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect::<Vec<_>>();
            assert_eq!(
                entries.len(),
                if changed || policy == ConflictPolicy::KeepBoth {
                    3
                } else {
                    2
                }
            );
            assert!(entries.iter().all(|p| !p
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains(".part")));
            if policy == ConflictPolicy::KeepBoth {
                let copy = entries
                    .iter()
                    .find(|p| {
                        p.file_name().unwrap() != "public.md"
                            && p.file_name().unwrap() != "public (copy).md"
                    })
                    .unwrap();
                assert_eq!(std::fs::read(copy).unwrap(), b"# public canary\n");
            }
        }
    }

    #[test]
    fn host_files_mobile_upload_maps_skip_keep_both_and_reviewed_replace_through_shared_queue() {
        use festerm_tunnel_bridge::files_endpoint::{FilesChannel, FilesEndpoint, FirstFailure};
        for policy in [
            ConflictPolicy::Skip,
            ConflictPolicy::KeepBoth,
            ConflictPolicy::Replace,
        ] {
            let (rt, remote, local_root, client, _) = policy_fixture(true);
            let local = LocalFs::new(local_root.path()).unwrap();
            std::fs::write(local.root().join("public.md"), b"new public upload").unwrap();
            let sources = Arc::new(local_upload::Sources::new(
                local_root::Root::open(local.root()).unwrap(),
                Arc::new(|| Ok(())),
            ));
            let mut row = transfer_row();
            row.id = 1;
            row.state = RowState::Queued;
            row.direction = Direction::Upload;
            let state = Arc::new(Mutex::new(State {
                rows: vec![row],
                ..State::default()
            }));
            rt.block_on(async {
                let failure = FirstFailure::default();
                let endpoint = FilesEndpoint::from_client(
                    client,
                    CancellationToken::new(),
                    failure.clone(),
                    FilesChannel::Transfer,
                );
                // Exercise the same optional local capability used in production
                // for both directions, rather than the upload-only constructor.
                let downloads = local_download_queue::Destinations::new(
                    local_root::Root::open(local.root()).unwrap(),
                    Arc::new(|| Ok(())),
                    CancellationToken::new(),
                );
                let manager =
                    festerm_ssh::SftpTransferManager::from_remote_endpoint_with_local_files(
                        endpoint,
                        sources.clone(),
                        downloads,
                    );
                let uploads =
                    uploads::Uploads::from_manager(manager, sources, state.clone(), failure);
                uploads.enqueue(1, "public.md", "/").unwrap();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        uploads.poll();
                        {
                            let state = state.lock().unwrap();
                            let row = &state.rows[0];
                            if row.state == RowState::Awaiting {
                                break;
                            }
                            assert!(
                                !row.state.finished(),
                                "upload stopped before collision: {:?} {:?}",
                                row.state,
                                row.error
                            );
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                assert!(state.lock().unwrap().rows[0].conflict.is_some());
                uploads.resolve(1, policy, false);
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        uploads.poll();
                        if state.lock().unwrap().rows[0].state.finished() {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "collision resolution stalled for {policy:?}: {:?}",
                        state.lock().unwrap().rows
                    )
                });
                {
                    let locked = state.lock().unwrap();
                    let row = &locked.rows[0];
                    assert_eq!(
                        row.state,
                        if policy == ConflictPolicy::Skip {
                            RowState::Skipped
                        } else {
                            RowState::Done
                        }
                    );
                    if policy != ConflictPolicy::Skip {
                        assert_eq!(row.done, 17);
                        assert_eq!(row.total, Some(17));
                    }
                }
                uploads.shutdown().await;
            });
            if policy == ConflictPolicy::Replace {
                assert_eq!(
                    std::fs::read(remote.path().join("public.md")).unwrap(),
                    b"new public upload"
                );
            } else {
                assert_eq!(
                    std::fs::read(remote.path().join("public.md")).unwrap(),
                    b"# public canary\n"
                );
            }
            if policy == ConflictPolicy::KeepBoth {
                let files = std::fs::read_dir(remote.path())
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_name() != "public.md" && entry.path().is_file())
                    .collect::<Vec<_>>();
                assert_eq!(files.len(), 1);
                assert_eq!(
                    std::fs::read(files[0].path()).unwrap(),
                    b"new public upload"
                );
            }
        }
    }
    #[test]
    fn host_files_upload_queued_source_cutover_never_sends_begin_or_publishes_replacement() {
        use festerm_tunnel_bridge::files_endpoint::{FilesChannel, FilesEndpoint, FirstFailure};
        use std::os::unix::fs::MetadataExt;
        let capture = Arc::new(Mutex::new(Vec::new()));
        let (rt, remote, local_root, client, _) =
            captured_policy_fixture(true, None, Some(capture.clone()));
        let remote_names = || {
            std::fs::read_dir(remote.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let original_remote_names = remote_names();
        let local = LocalFs::new(local_root.path()).unwrap();
        std::fs::write(local.root().join("new.txt"), b"public admitted source").unwrap();
        let original_source = std::fs::metadata(local.root().join("new.txt")).unwrap();
        let sources = Arc::new(local_upload::Sources::new(
            local_root::Root::open(local.root()).unwrap(),
            Arc::new(|| Ok(())),
        ));
        let mut row = transfer_row();
        row.id = 1;
        row.state = RowState::Queued;
        row.direction = Direction::Upload;
        let state = Arc::new(Mutex::new(State {
            rows: vec![row],
            ..State::default()
        }));
        rt.block_on(async {
            let failure = FirstFailure::default();
            let endpoint = FilesEndpoint::from_client(
                client,
                CancellationToken::new(),
                failure.clone(),
                FilesChannel::Transfer,
            );
            let downloads = local_download_queue::Destinations::new(
                local_root::Root::open(local.root()).unwrap(),
                Arc::new(|| Ok(())),
                CancellationToken::new(),
            );
            let manager = festerm_ssh::SftpTransferManager::from_remote_endpoint_with_local_files(
                endpoint,
                sources.clone(),
                downloads,
            );
            let uploads = uploads::Uploads::from_manager(manager, sources, state.clone(), failure);
            uploads.enqueue(1, "new.txt", "/").unwrap();
            // No runtime yield: replace the admitted name before the queue runs.
            std::fs::rename(
                local.root().join("new.txt"),
                local.root().join("retained.txt"),
            )
            .unwrap();
            std::fs::write(local.root().join("new.txt"), b"unadmitted bytes").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    uploads.poll();
                    if state.lock().unwrap().rows[0].state.finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(state.lock().unwrap().rows[0].state, RowState::Failed);
            assert_eq!(
                state.lock().unwrap().rows[0].error.as_deref(),
                Some("This file changed or is no longer available. Select it again.")
            );
            // Production planning may stat the remote destination before the
            // retained source is opened. Audit actual typed requests rather
            // than treating all transport traffic as an upload mutation.
            let transcript = capture.lock().unwrap().clone();
            let mut frames = transcript.as_slice();
            let mut operations = Vec::new();
            while !frames.is_empty() {
                let (kind, bytes) = festerm_tunnel_bridge::wire::read_frame(&mut frames)
                    .await
                    .unwrap();
                match kind {
                    rpc::OPEN => {
                        let _: rpc::Open = serde_json::from_slice(&bytes).unwrap();
                    }
                    rpc::REQUEST => {
                        let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                        operations.push(request.operation);
                    }
                    _ => panic!("unexpected client frame: {kind}"),
                }
            }
            assert!(
                !operations.is_empty(),
                "production destination preflight must be exercised"
            );
            assert!(
                operations.iter().all(|operation| matches!(
                    operation,
                    rpc::Operation::Stat { path } | rpc::Operation::StatReadable { path }
                        if path == "/" || path == "/new.txt"
                )),
                "only read-only destination metadata is allowed; no BeginUpload: {operations:?}"
            );
            assert_eq!(
                remote_names(),
                original_remote_names,
                "no destination or partial may be created"
            );
            uploads.shutdown().await;
        });
        assert!(!remote.path().join("new.txt").exists());
        assert_eq!(remote_names(), original_remote_names);
        assert_eq!(
            std::fs::read(remote.path().join("public.md")).unwrap(),
            b"# public canary\n"
        );
        assert_eq!(
            std::fs::read_dir(remote.path().join("folder"))
                .unwrap()
                .count(),
            0
        );
        let retained = std::fs::metadata(local.root().join("retained.txt")).unwrap();
        assert_eq!(
            (retained.dev(), retained.ino()),
            (original_source.dev(), original_source.ino())
        );
        assert_eq!(
            std::fs::read(local.root().join("new.txt")).unwrap(),
            b"unadmitted bytes"
        );
        assert_eq!(
            std::fs::read(local.root().join("retained.txt")).unwrap(),
            b"public admitted source"
        );
    }
    #[test]
    fn host_files_upload_inflight_source_cutover_settles_and_next_upload_is_usable() {
        use festerm_tunnel_bridge::files_endpoint::{FilesChannel, FilesEndpoint, FirstFailure};
        use std::sync::atomic::Ordering;
        let gate = Arc::new(WriteGate::default());
        let (rt, remote, local_dir, client, _) = gated_policy_fixture(true, Some(gate.clone()));
        let root = local_root::Root::open(local_dir.path()).unwrap();
        let local = root.local_fs().unwrap();
        std::fs::write(local.root().join("large.txt"), vec![b'p'; 256 * 1024]).unwrap();
        let sources = Arc::new(local_upload::Sources::new(root, Arc::new(|| Ok(()))));
        let mut row = transfer_row();
        row.id = 1;
        row.direction = Direction::Upload;
        row.state = RowState::Queued;
        let state = Arc::new(Mutex::new(State {
            rows: vec![row.clone()],
            ..State::default()
        }));
        rt.block_on(async {
            let failure = FirstFailure::default();
            let lifetime = CancellationToken::new();
            let endpoint = FilesEndpoint::from_client(
                client,
                lifetime.clone(),
                failure.clone(),
                FilesChannel::Transfer,
            );
            let uploads =
                uploads::Uploads::from_endpoint(endpoint, sources, state.clone(), failure);
            uploads.enqueue(1, "large.txt", "/").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !gate.entered.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            for _ in 0..2 {
                uploads.poll();
                let state = state.lock().unwrap();
                let row = &state.rows[0];
                assert!(
                    row.done > 0,
                    "actual active source bytes must reach the existing progress row"
                );
                assert_eq!(
                    row.state,
                    RowState::Active,
                    "stale snapshots must not downgrade known active progress"
                );
                assert_eq!(row.total, Some(256 * 1024));
            }
            std::fs::write(local.root().join("large.txt"), b"fresh selected source").unwrap();
            gate.open.store(true, Ordering::SeqCst);
            if let Some(waker) = gate.waker.lock().unwrap().take() {
                waker.wake();
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    uploads.poll();
                    if state.lock().unwrap().rows[0].state.finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(state.lock().unwrap().rows[0].state, RowState::Failed);
            assert!(!remote.path().join("large.txt").exists());
            assert!(
                uploads.failure().is_none(),
                "acknowledged abort leaves the transfer stream usable"
            );
            row.id = 2;
            state.lock().unwrap().rows.push(row);
            uploads.enqueue(2, "large.txt", "/").unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    uploads.poll();
                    if state.lock().unwrap().rows[1].state.finished() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(state.lock().unwrap().rows[1].state, RowState::Done);
            assert_eq!(
                std::fs::read(remote.path().join("large.txt")).unwrap(),
                b"fresh selected source"
            );
            // An actually closed worker must be observed after buffered events,
            // unlike the nonterminal Empty state or an ordinary queued cancel.
            lifetime.cancel();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    uploads.poll();
                    if uploads.failure().is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
            })
            .await
            .unwrap();
            uploads.shutdown().await;
        });
    }
    #[test]
    fn host_files_real_framed_read_only_listing_download_and_preview_bytes() {
        let (rt, _remote, local, mut client) = fixture();
        let page = rt
            .block_on(listing(&mut client, "/", &ListOptions::default()))
            .unwrap();
        assert_eq!(page.directory_read_only, None);
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
            (&local_root::Root::open(local.path()).unwrap(), ""),
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
            (&local_root::Root::open(local.path()).unwrap(), ""),
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
        let local = local_root::Root::open(local.path()).unwrap();
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
    fn host_files_location_chooser_is_local_and_retirement_hides_namespaced_metadata() {
        let selector = "11111111111111111111111111111111";
        let virtual_root = format!("/{selector}");
        let (commands, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let mut row = transfer_row();
        row.direction = Direction::Upload;
        row.target = format!("{virtual_root}/projects");
        let lease = Arc::new(Lease {
            commands,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State {
                rows: vec![row],
                ..State::default()
            })),
            wake: egui::Context::default(),
            current: Arc::new(|| Ok(())),
            write: std::sync::atomic::AtomicBool::new(true),
            delete: std::sync::atomic::AtomicBool::new(true),
            uploads: Mutex::new(None),
            root: Mutex::new(None),
            display_root: Mutex::new(None),
            locations: Mutex::new(Some(vec![rpc::FileLocation {
                selector: selector.into(),
                label: "/Users/example".to_owned().try_into().unwrap(),
                available: true,
                mutations_available: false,
            }])),
        });
        let remote = Remote(lease.clone());
        let transfers = HostTransfers(lease.clone());
        assert!(remote
            .list("/", &ListOptions::default())
            .unwrap()
            .entries
            .is_empty());
        assert!(remote.join("/", "unrouted").is_err());
        assert_eq!(remote.parent(&virtual_root), None);
        assert_eq!(
            remote.parent(&format!("{virtual_root}/child")),
            Some(virtual_root.clone())
        );
        assert!(remote.mkdir(&format!("{virtual_root}/child")).is_err());
        assert!(transfers
            .enqueue(Direction::Upload, "public.md", &virtual_root)
            .is_err());
        assert!(transfers
            .review_delete(
                vec![format!("{virtual_root}/public.md")],
                CancellationToken::new()
            )
            .is_err());
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(transfers.rows()[0].target, "/Users/example/projects");
        assert_eq!(
            lease.state.lock().unwrap().rows[0].target,
            format!("{virtual_root}/projects"),
            "presentation must never retarget retained queued work"
        );
        lease.cancel.cancel();
        assert!(transfers.remote_locations().unwrap().is_empty());
        assert_eq!(transfers.rows()[0].target, "Remote folder");
        assert!(remote.list("/", &ListOptions::default()).is_err());
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
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(None),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
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
                initial_remote_listing: None,
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
    fn host_files_root_replacement_retires_ready_and_connection_before_disclosure() {
        let owner = tempfile::tempdir().unwrap();
        let selected = owner.path().join("Documents");
        std::fs::create_dir(&selected).unwrap();
        let root = local_root::Root::open(&selected).unwrap();
        let (commands, _rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            current: Arc::new(|| Ok(())),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(Some(root)),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
        });
        let (send, events) = mpsc::sync_channel(1);
        send.send(LinkEvent::Status("prepared".into())).unwrap();
        let mut connector = HostConnector {
            events,
            lease: lease.clone(),
        };
        std::fs::rename(&selected, owner.path().join("retained")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert!(matches!(
            connector.poll().as_slice(),
            [LinkEvent::Failed(_)]
        ));
        assert!(HostTransfers(lease).connection_error().is_some());
    }
    #[test]
    fn host_files_preview_cleanup_preserves_preexisting_and_replaced_cache() {
        let owner = tempfile::tempdir().unwrap();
        let selected = owner.path().join("Documents");
        std::fs::create_dir(&selected).unwrap();
        let root = local_root::Root::open(&selected).unwrap();
        let local = root.local_fs().unwrap();
        let sources = Arc::new(local_upload::Sources::new(
            root.clone(),
            Arc::new(|| Ok(())),
        ));
        let (commands, _rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            current: Arc::new(|| Ok(())),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(Some(root.clone())),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
        });
        let mut session = super::super::files::FilesSession::ready(
            "Files".into(),
            "Read-only".into(),
            Ready {
                remote_write: false,
                remote: Arc::new(local.clone()),
                local: Arc::new(local_upload::Documents { local, sources }),
                local_root: selected.clone(),
                transfers: Arc::new(HostTransfers(lease)),
                start_dir: None,
                initial_remote_listing: None,
            },
            &egui::Context::default(),
        );
        let preexisting = selected.join(super::super::files::CACHE_DIR);
        std::fs::create_dir(&preexisting).unwrap();
        let prior = preexisting.join("prior.txt");
        std::fs::write(&prior, b"preexisting public marker").unwrap();
        // No preview was requested: close must not claim this existing folder.
        session.cleanup();
        assert_eq!(std::fs::read(&prior).unwrap(), b"preexisting public marker");
        let owned = root.prepare_preview().unwrap();
        let owned_path = selected.join(&owned);
        std::fs::write(owned_path.join("owned.txt"), b"private preview copy").unwrap();
        std::fs::rename(&owned_path, selected.join("retained-preview")).unwrap();
        std::fs::create_dir(&owned_path).unwrap();
        let replacement = owned_path.join("replacement.txt");
        std::fs::write(&replacement, b"unowned cache replacement").unwrap();
        assert!(root.prepare_preview().is_err());
        session.cleanup();
        assert_eq!(
            std::fs::read(&replacement).unwrap(),
            b"unowned cache replacement"
        );
        assert_eq!(
            std::fs::read(selected.join("retained-preview/owned.txt")).unwrap(),
            b"private preview copy"
        );
        std::fs::rename(&selected, owner.path().join("retained")).unwrap();
        std::fs::create_dir_all(selected.join(super::super::files::CACHE_DIR)).unwrap();
        let marker = selected
            .join(super::super::files::CACHE_DIR)
            .join("replacement.txt");
        std::fs::write(&marker, b"replacement public marker").unwrap();
        session.cleanup();
        assert_eq!(std::fs::read(marker).unwrap(), b"replacement public marker");
    }
    #[test]
    fn host_files_worker_drops_ssh_channel_on_close_and_startup_error() {
        use russh::{
            client,
            keys::{key::safe_rng, Algorithm, PrivateKey, PublicKey},
            server, Channel,
        };

        struct Host;
        impl server::Handler for Host {
            type Error = russh::Error;

            async fn auth_none(&mut self, _: &str) -> Result<server::Auth, Self::Error> {
                Ok(server::Auth::Accept)
            }

            async fn channel_open_session(
                &mut self,
                _: Channel<server::Msg>,
                reply: server::ChannelOpenHandle,
                _: &mut server::Session,
            ) -> Result<(), Self::Error> {
                reply.accept().await;
                Ok(())
            }
        }

        struct PinnedHost(PublicKey);
        impl client::Handler for PinnedHost {
            type Error = russh::Error;

            async fn check_server_key(&mut self, key: &PublicKey) -> Result<bool, Self::Error> {
                Ok(key == &self.0)
            }
        }

        for expected in [Err("Files starting folder unavailable".to_owned()), Ok(())] {
            let actual = run_files_worker(|runtime| {
                let (_session, stream) = runtime.block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), async {
                        let key = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap();
                        let pinned_host = PinnedHost(key.public_key().clone());
                        let config = Arc::new(server::Config {
                            keys: vec![key],
                            ..Default::default()
                        });
                        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
                        let server = tokio::spawn(async move {
                            server::run_stream(config, server_io, Host)
                                .await
                                .unwrap()
                                .await
                        });
                        let mut client = client::connect_stream(
                            Arc::new(client::Config::default()),
                            client_io,
                            pinned_host,
                        )
                        .await
                        .unwrap();
                        assert!(client.authenticate_none("fixture").await.unwrap().success());
                        let channel = client.channel_open_session().await.unwrap();
                        ((client, server), channel.into_stream())
                    })
                    .await
                    .unwrap()
                });
                // Exercise the real russh destructor outside block_on, with
                // both a live channel returning an error and ordinary close.
                expected.clone()?;
                drop(stream);
                Ok(())
            });
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn host_files_start_directory_metadata_refuses_unsupported_paths_before_ready() {
        use festerm_tunnel_bridge::wire::{read_frame, write_frame};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            for (path, accepted) in [("/عربي", true), ("/\u{202e}hidden", false), ("/bad//child", false)] {
                let scope = rpc::HostScope { host: Default::default(), installation: Default::default(), epoch: "1".into() };
                let expected = scope.clone();
                let (stream, mut peer) = tokio::io::duplex(8192);
                let server = tokio::spawn(async move {
                    let limits = rpc::Capabilities { version: rpc::VERSION, write: false, chunk_limit: rpc::CHUNK_LIMIT, page_limit: rpc::PAGE_LIMIT, handle_limit: rpc::HANDLE_LIMIT };
                    write_frame(&mut peer, rpc::HOST_DESCRIPTION, &serde_json::to_vec(&rpc::HostDescription { version: rpc::HOST_VERSION, scope: expected, filesystem: Some(limits) }).unwrap()).await.unwrap();
                    assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::OPEN);
                    let ready = serde_json::json!({"id":0,"result":{"type":"ready_v3","capabilities":{"version":rpc::HOST_REVIEWED_VERSION,"write":false,"operations":["start_directory"],"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}}});
                    write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&ready).unwrap()).await.unwrap();
                    let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                    assert_eq!(kind, rpc::REQUEST);
                    let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                    assert!(matches!(request.operation, rpc::Operation::StartDirectory {}));
                    let reply = serde_json::json!({"id":request.id,"result":{"type":"start_directory","path":path}});
                    write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&reply).unwrap()).await.unwrap();
                });
                let (mut client, _) = file_client::Client::open_host_reviewed(stream, &scope.host, &scope.installation, CancellationToken::new()).await.unwrap().unwrap();
                // This is the worker's prerequisite for constructing/publishing Ready;
                // it also covers a manually configured server, independent of owner GUI validation.
                let start = initial_directory(&mut client, file_client::NavigationPreference::DefaultStart).await;
                assert_eq!(start.is_ok(), accepted);
                if accepted { assert_eq!(start.unwrap(), path); }
                client.cancel();
                tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
            }
        });
    }

    #[test]
    fn host_files_choose_location_requires_new_valid_metadata_and_never_opens_a_folder() {
        use festerm_tunnel_bridge::wire::{read_frame, write_frame};
        use file_client::NavigationPreference::{ChooseLocation, DefaultStart};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            // Each attempt has a NEW stream/client; the failed default cannot
            // donate cached metadata, an opening, or a selector to recovery.
            for (navigation, legacy, malformed, cancel_before, cancel_after) in [
                (DefaultStart, false, false, false, false),
                (ChooseLocation, false, false, false, false),
                (ChooseLocation, true, false, false, false),
                (ChooseLocation, false, true, false, false),
                (ChooseLocation, false, false, true, false),
                (ChooseLocation, false, false, false, true),
            ] {
                let scope = rpc::HostScope { host: Default::default(), installation: Default::default(), epoch: "1".into() };
                let expected = scope.clone();
                let cancel = CancellationToken::new();
                let retire = cancel.clone();
                let (stream, mut peer) = tokio::io::duplex(8192);
                let server = tokio::spawn(async move {
                    let limits = rpc::Capabilities { version: rpc::VERSION, write: false, chunk_limit: rpc::CHUNK_LIMIT, page_limit: rpc::PAGE_LIMIT, handle_limit: rpc::HANDLE_LIMIT };
                    write_frame(&mut peer, rpc::HOST_DESCRIPTION, &serde_json::to_vec(&rpc::HostDescription { version: if legacy { rpc::HOST_VERSION } else { rpc::HOST_LOCATIONS_VERSION }, scope: expected, filesystem: Some(limits) }).unwrap()).await.unwrap();
                    assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::OPEN);
                    let operations = if legacy { vec!["start_directory"] } else { vec!["locations", "start_directory"] };
                    let ready = serde_json::json!({"id":0,"result":{"type":"ready_v3","capabilities":{"version":rpc::HOST_REVIEWED_VERSION,"write":false,"operations":operations,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}}});
                    write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&ready).unwrap()).await.unwrap();
                    if !legacy && !cancel_before {
                        let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                        assert_eq!(kind, rpc::REQUEST);
                        let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                        assert!(matches!(request.operation, rpc::Operation::Locations {}));
                        let reply = serde_json::json!({"id":request.id,"result":{"type":"locations","locations":[{"selector":if malformed {"invalid"}else{"22222222222222222222222222222222"},"label":"/owned/current","available":true,"mutations_available":false}]}});
                        write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&reply).unwrap()).await.unwrap();
                        if cancel_after { retire.cancel(); }
                        if navigation == DefaultStart {
                            let (_, bytes) = read_frame(&mut peer).await.unwrap();
                            let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                            assert!(matches!(request.operation, rpc::Operation::StartDirectory {}));
                            let reply = serde_json::json!({"id":request.id,"error":"not_found"});
                            write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&reply).unwrap()).await.unwrap();
                        }
                    }
                    // Dropping the client must be the NEXT event. Any Start,
                    // stat, directory open, mutation, or retry fails this check.
                    assert!(read_frame(&mut peer).await.is_err());
                });
                let (mut client, _) = file_client::Client::open_host_reviewed(stream, &scope.host, &scope.installation, cancel.clone()).await.unwrap().unwrap();
                if cancel_before { cancel.cancel(); }
                let result = initial_directory(&mut client, navigation).await;
                let accepted = navigation == ChooseLocation && !legacy && !malformed && !cancel_before && !cancel_after;
                assert_eq!(result.is_ok(), accepted);
                if accepted {
                    assert_eq!(result.unwrap(), "/");
                    let snapshot = initial_listing(&mut client, "/").await.unwrap().unwrap();
                    assert_eq!(snapshot.path, "/");
                    assert!(snapshot.entries.is_empty());
                }
                if navigation == DefaultStart {
                    assert!(client.available(), "a missing start is not a malformed-metadata recovery or an authentication downgrade");
                }
                drop(client);
                tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
            }
        });
    }

    #[test]
    fn host_files_initial_listing_is_required_before_ready() {
        use festerm_tunnel_bridge::wire::{read_frame, write_frame};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for (tagged, failure) in [
            (false, Some("not_found")),
            (false, Some("denied")),
            (false, None),
            (true, Some("denied")),
            (true, None),
        ] {
            let (mut client, server, alias) = rt.block_on(async {
                let scope = rpc::HostScope { host: Default::default(), installation: Default::default(), epoch: "1".into() };
                let expected = scope.clone();
                let (stream, mut peer) = tokio::io::duplex(8192);
                let server = tokio::spawn(async move {
                    let limits = rpc::Capabilities { version: rpc::VERSION, write: tagged, chunk_limit: rpc::CHUNK_LIMIT, page_limit: rpc::PAGE_LIMIT, handle_limit: rpc::HANDLE_LIMIT };
                    write_frame(&mut peer, rpc::HOST_DESCRIPTION, &serde_json::to_vec(&rpc::HostDescription { version: rpc::HOST_LOCATIONS_VERSION, scope: expected, filesystem: Some(limits) }).unwrap()).await.unwrap();
                    assert_eq!(read_frame(&mut peer).await.unwrap().0, rpc::OPEN);
                    let mut operations = vec!["locations", "start_directory"];
                    if tagged {
                        operations.push("read_links");
                    }
                    let ready = serde_json::json!({"id":0,"result":{"type":"ready_v3","capabilities":{"version":rpc::HOST_REVIEWED_VERSION,"write":tagged,"operations":operations,"chunk_limit":rpc::CHUNK_LIMIT,"page_limit":rpc::PAGE_LIMIT,"handle_limit":rpc::HANDLE_LIMIT}}});
                    write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&ready).unwrap()).await.unwrap();
                    let steps = match (tagged, failure) {
                        (_, Some(_)) => 3,
                        (false, None) => 5,
                        (true, None) => 12,
                    };
                    const START: &str = "/22222222222222222222222222222222/child";
                    const ALIAS: &str = "/22222222222222222222222222222222/child/public-alias.md";
                    for step in 0..steps {
                        let (kind, bytes) = read_frame(&mut peer).await.unwrap();
                        assert_eq!(kind, rpc::REQUEST);
                        let request: rpc::Request = serde_json::from_slice(&bytes).unwrap();
                        let reply = match step {
                            0 => {
                                assert!(matches!(request.operation, rpc::Operation::Locations {}));
                                serde_json::json!({"id":request.id,"result":{"type":"locations","locations":[{"selector":"22222222222222222222222222222222","label":"/owned/current","available":true,"mutations_available":tagged}]}})
                            }
                            1 => {
                                assert!(matches!(request.operation, rpc::Operation::StartDirectory {}));
                                serde_json::json!({"id":request.id,"result":{"type":"start_directory","path":START}})
                            }
                            2 | 5 => {
                                if tagged {
                                    assert!(matches!(request.operation, rpc::Operation::OpenReadableDirectory { ref path } if path == START));
                                } else {
                                    assert!(matches!(request.operation, rpc::Operation::OpenDirectory { ref path } if path == START));
                                }
                                if let Some(failure) = failure {
                                    serde_json::json!({"id":request.id,"error":failure})
                                } else if tagged {
                                    serde_json::json!({"id":request.id,"result":{"type":"readable_directory","handle":7,"metadata":{"kind":"directory","size":"0","modified_ms":null},"read_only":true}})
                                } else {
                                    serde_json::json!({"id":request.id,"result":{"type":"directory","handle":7}})
                                }
                            }
                            3 | 6 => {
                                assert!(matches!(request.operation, rpc::Operation::ReadDirectory { handle:7, .. }));
                                if tagged {
                                    serde_json::json!({"id":request.id,"result":{"type":"readable_entries","entries":[{"name":"public-alias.md","kind":"file","read_only":true}],"done":true,"skipped":0}})
                                } else {
                                    serde_json::json!({"id":request.id,"result":{"type":"entries","entries":[],"done":true,"skipped":0}})
                                }
                            }
                            4 | 7 => {
                                assert!(matches!(request.operation, rpc::Operation::Close { handle:7 }));
                                serde_json::json!({"id":request.id,"result":{"type":"closed"}})
                            }
                            8 => {
                                assert!(matches!(request.operation, rpc::Operation::StatReadable { ref path } if path == ALIAS));
                                serde_json::json!({"id":request.id,"result":{"type":"readable_stat","metadata":{"kind":"file","size":"16","modified_ms":null},"read_only":true}})
                            }
                            9 => {
                                assert!(matches!(request.operation, rpc::Operation::OpenReadableFile { ref path } if path == ALIAS));
                                serde_json::json!({"id":request.id,"result":{"type":"file","handle":8,"metadata":{"kind":"file","size":"16","modified_ms":null}}})
                            }
                            10 => {
                                assert!(matches!(request.operation, rpc::Operation::Read {handle:8, ..}));
                                serde_json::json!({"id":request.id,"result":{"type":"read","offset":"0","length":16,"eof":true}})
                            }
                            11 => {
                                assert!(matches!(request.operation, rpc::Operation::Close { handle:8 }));
                                serde_json::json!({"id":request.id,"result":{"type":"closed"}})
                            }
                            _ => unreachable!(),
                        };
                        write_frame(&mut peer, rpc::RESPONSE, &serde_json::to_vec(&reply).unwrap()).await.unwrap();
                        if step == 10 {
                            let mut data = request.id.to_be_bytes().to_vec();
                            data.extend_from_slice(b"# public canary\n");
                            write_frame(&mut peer, rpc::DATA, &data).await.unwrap();
                        }
                    }
                    // No fallback directory, mutation or retry follows the refusal.
                    assert!(read_frame(&mut peer).await.is_err());
                });
                let (mut client, _) = file_client::Client::open_host_reviewed(stream, &scope.host, &scope.installation, CancellationToken::new()).await.unwrap().unwrap();
                let start = initial_directory(&mut client, file_client::NavigationPreference::DefaultStart).await.unwrap();
                let first = initial_listing(&mut client, &start).await;
                if failure.is_some() {
                    assert_eq!(first.unwrap_err(), "Files starting folder unavailable");
                } else {
                    let snapshot = first.unwrap().unwrap();
                    assert_eq!(snapshot.path, start);
                    if tagged {
                        assert_eq!(snapshot.directory_read_only, Some(true));
                        assert_eq!(snapshot.entries[0].name, "public-alias.md");
                        assert_eq!(snapshot.entries[0].permissions, Some(0o500));
                        assert_eq!(
                            listing(&mut client, &start, &ListOptions::default()).await.unwrap(),
                            snapshot,
                        );
                    } else {
                        assert_eq!(snapshot.directory_read_only, None);
                        assert!(snapshot.entries.is_empty());
                    }
                }
                (client, server, format!("{start}/public-alias.md"))
            });
            if tagged && failure.is_none() {
                let local = tempfile::tempdir().unwrap();
                let count = download(
                    &rt,
                    &mut client,
                    (&local_root::Root::open(local.path()).unwrap(), ""),
                    &alias,
                    &CancellationToken::new(),
                    &|| Ok(()),
                    None,
                )
                .unwrap();
                assert_eq!(count, 16);
                assert_eq!(
                    std::fs::read(local.path().join("public-alias.md")).unwrap(),
                    b"# public canary\n",
                );
            }
            drop(client);
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(3), server)
                    .await
                    .unwrap()
                    .unwrap();
            });
        }
    }

    #[test]
    fn host_files_root_presentation_sanitizes_formatters_and_masks_retired_inactive_view() {
        use super::super::{
            files::FilesSession, files_fixture::FixtureFs, workspaces::Workspace, Lifecycle,
            MobileApp, Surface,
        };
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{
            cell::Cell,
            rc::Rc,
            sync::atomic::{AtomicBool, Ordering},
        };
        for raw in ["/Users/عربي", "/Users/\u{202e}example\u{2066}"] {
            for retired in [None, Some(true), Some(false)] {
                let current = Arc::new(AtomicBool::new(true));
                let check = current.clone();
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
                    write: std::sync::atomic::AtomicBool::new(false),
                    delete: std::sync::atomic::AtomicBool::new(false),
                    uploads: Mutex::new(None),
                    root: Mutex::new(None),
                    locations: Mutex::new(None),
                    display_root: Mutex::new(Some(raw.to_owned().try_into().unwrap())),
                });
                let fs = |kind| {
                    Arc::new(FixtureFs {
                        kind,
                        home: "/",
                        entries: vec![],
                    })
                };
                let mut app = MobileApp::new_terminal(Rc::new(Cell::new(Lifecycle {
                    active: false,
                    ..Default::default()
                })));
                app.files = Some(Box::new(FilesSession::ready(
                    "Files".into(),
                    "Host".into(),
                    Ready {
                        remote: fs(festerm_sftp::PaneKind::Remote),
                        local: fs(festerm_sftp::PaneKind::Local),
                        local_root: "/nonexistent-public-fixture".into(),
                        transfers: Arc::new(HostTransfers(lease.clone())),
                        start_dir: Some("/".into()),
                        initial_remote_listing: None,
                        remote_write: false,
                    },
                    &egui::Context::default(),
                )));
                app.workspace = Workspace::Files;
                app.surface = Surface::Terminal;
                app.files_ctx.only = true;
                match retired {
                    Some(true) => lease.cancel.cancel(),
                    Some(false) => current.store(false, Ordering::SeqCst),
                    None => (),
                }
                let mut harness = Harness::builder()
                    .with_size(egui::vec2(375.0, 853.0))
                    .build_ui_state(
                        |ui, app: &mut MobileApp| app.show_with_occlusion(ui, 0.0),
                        app,
                    );
                harness.run_steps(3);
                let label = if retired.is_some() {
                    "Shared folder".into()
                } else {
                    festerm_sftp::path::display_name(raw)
                };
                harness.get_by_label(&label);
                if label != raw {
                    assert!(harness.query_by_label(raw).is_none());
                }
                assert!(
                    harness.state().files.as_ref().unwrap().is_ready(),
                    "inactive paint did not poll retirement"
                );
                assert_eq!(
                    lease
                        .display_root
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .as_str(),
                    raw
                );
                assert!(!format!("{:?}", lease.display_root.lock().unwrap()).contains(raw));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn host_files_delete_local_availability_rechecks_lifetime_after_callback() {
        let local = tempfile::tempdir().unwrap();
        let root = local_root::Root::open(local.path()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let cancel = CancellationToken::new();
        let retired = cancel.clone();
        let lease = Arc::new(Lease {
            commands: tx,
            cancel,
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(Some(root)),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
            current: Arc::new(move || {
                retired.cancel();
                Ok(())
            }),
        });
        let transfers = HostTransfers(lease);
        assert!(!transfers.local_delete_available());
        assert!(transfers
            .review_local_delete(vec!["canary".into()], CancellationToken::new())
            .is_err());
        assert!(rx.try_recv().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn host_files_delete_local_pre_review_file_to_folder_shows_current_kind_before_confirm() {
        use crate::files::{FilesSession, Ready};
        use egui_kittest::{kittest::Queryable, Harness};
        let local = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join("chosen"), b"old file").unwrap();
        let root = local_root::Root::open(local.path()).unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands: tx,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(Some(root.clone())),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
            current: Arc::new(|| Ok(())),
        });
        let ctx = egui::Context::default();
        let mut session = FilesSession::ready(
            "Files".into(),
            "Dev Tunnels Files".into(),
            Ready {
                remote: Arc::new(crate::files_fixture::FixtureFs {
                    kind: festerm_sftp::PaneKind::Remote,
                    home: "/",
                    entries: vec![],
                }),
                local: Arc::new(festerm_sftp::LocalFs::new(local.path()).unwrap()),
                local_root: local.path().into(),
                transfers: Arc::new(HostTransfers(lease.clone())),
                start_dir: Some("/".into()),
                initial_remote_listing: None,
                remote_write: false,
            },
            &ctx,
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while session.local.as_ref().unwrap().loading() {
            session.poll(&ctx);
            assert!(
                std::time::Instant::now() < deadline,
                "initial owned listing did not finish"
            );
            std::thread::yield_now();
        }
        assert_eq!(
            session
                .local
                .as_ref()
                .unwrap()
                .listing
                .as_ref()
                .unwrap()
                .entries[0]
                .kind,
            festerm_sftp::EntryKind::File
        );
        session
            .local
            .as_mut()
            .unwrap()
            .selected
            .insert("chosen".into());
        std::fs::remove_file(local.path().join("chosen")).unwrap();
        std::fs::create_dir(local.path().join("chosen")).unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    session.poll_delete(ui.ctx());
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run_steps(3);
        harness
            .get_all_by_label("Sort and view options")
            .nth(1)
            .unwrap()
            .click();
        harness.run_steps(3);
        harness.get_by_label("Delete selected").click();
        harness.run_steps(3);
        let Command::LocalDelete(command) = rx.try_recv().unwrap() else {
            panic!("local preparation must not use remote RPC")
        };
        let mut owner = local_delete::Owner::new(
            root,
            lease.current.clone(),
            lease.cancel.clone(),
            Arc::new(|| {}),
        );
        owner.execute(command);
        harness.run_steps(3);
        harness.get_by_label("Empty folder");
        assert!(harness.query_by_label("File · 8 B").is_none());
        assert!(local.path().join("chosen").is_dir());
        assert!(rx.try_recv().is_err());
        harness.get_by_label("Delete reviewed entries").click();
        harness.run_steps(3);
        assert!(
            local.path().join("chosen").is_dir(),
            "only worker confirmation may mutate"
        );
        let Command::LocalDelete(command) = rx.try_recv().unwrap() else {
            panic!("local confirmation must not use remote RPC")
        };
        owner.execute(command);
        harness.run_steps(3);
        assert!(!local.path().join("chosen").exists());
    }
    #[cfg(unix)]
    #[test]
    fn host_files_delete_local_commands_do_not_require_remote_write_delete_or_rpc() {
        let local = tempfile::tempdir().unwrap();
        std::fs::write(local.path().join("canary"), b"original").unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let root = local_root::Root::open(local.path()).unwrap();
        let lease = Arc::new(Lease {
            commands: tx,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(Some(root.clone())),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
            current: Arc::new(|| Ok(())),
        });
        let transfers = HostTransfers(lease.clone());
        assert!(!transfers.delete_available());
        assert!(transfers.local_delete_available());
        let stop = CancellationToken::new();
        let reply = transfers
            .review_local_delete(vec!["canary".into()], stop.clone())
            .unwrap();
        let Command::LocalDelete(command) = rx.try_recv().unwrap() else {
            panic!("local command must never enter remote dispatcher")
        };
        let mut owner = local_delete::Owner::new(
            root,
            lease.current.clone(),
            lease.cancel.clone(),
            Arc::new(|| {}),
        );
        owner.execute(command);
        let review = reply.recv().unwrap().unwrap();
        assert_eq!(
            std::fs::read(local.path().join("canary")).unwrap(),
            b"original"
        );
        let result = transfers.commit_local_delete(review.id).unwrap();
        let Command::LocalDelete(command) = rx.try_recv().unwrap() else {
            panic!("local commit must not use Files RPC")
        };
        owner.execute(command);
        assert_eq!(result.recv().unwrap().unwrap().confirmed, 1);
        assert!(rx.try_recv().is_err());
        assert!(!local.path().join("canary").exists());
    }
    #[test]
    fn host_files_mutations_and_invalid_paths_never_reach_worker() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE);
        let lease = Arc::new(Lease {
            commands: tx,
            cancel: CancellationToken::new(),
            state: Arc::new(Mutex::new(State::default())),
            wake: egui::Context::default(),
            write: std::sync::atomic::AtomicBool::new(false),
            delete: std::sync::atomic::AtomicBool::new(false),
            uploads: Mutex::new(None),
            root: Mutex::new(None),
            locations: Mutex::new(None),
            display_root: Mutex::new(None),
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
