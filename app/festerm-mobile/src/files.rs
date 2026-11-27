//! SFTP browsing state for the Files screen: two stacked panes (remote and
//! this iPhone), selection, transfers and the pending conflict decision.
//!
//! Everything here is UI-free and runs against `festerm_sftp::PaneFs`, so the
//! gallery and tests use an in-memory file system while production plugs in
//! `SftpClient` and the sandboxed `LocalFs` through a [`Connector`].

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use eframe::egui;
use festerm_sftp::{
    ConflictPolicy, Direction, Entry, EntryKind, ListOptions, Listing, PaneFs, SftpError, SortKey,
    SortOrder, TransferId, TransferManager, TransferRequest, TransferState,
};
use festerm_ssh::HostIdentity;

use super::model::{HostDecision, HostVerification};
use super::password_buffer::PasswordBuffer;

/// Hidden directory under the local root that holds remote files opened for
/// reading; it is emptied when the Files session closes.
pub(crate) const CACHE_DIR: &str = ".festerm-cache";
const MAX_MARKDOWN_BYTES: u64 = festerm_markdown::MAX_SOURCE_BYTES as u64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RowState {
    Queued,
    Active,
    Awaiting,
    Done,
    Skipped,
    Failed,
    Cancelled,
}

impl RowState {
    pub(crate) const fn finished(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Skipped | Self::Failed | Self::Cancelled
        )
    }
}

/// One side of a conflict: what the file looks like there today.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FileFacts {
    pub size: Option<u64>,
    pub modified: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConflictView {
    pub direction: Direction,
    pub allowed: Vec<ConflictPolicy>,
    pub can_remember: bool,
    pub iphone: FileFacts,
    pub remote: FileFacts,
    pub suggested_name: String,
    pub existing_is_directory: bool,
    pub likely_identical: bool,
}

/// A transfer as the UI shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransferRow {
    pub id: u64,
    pub name: String,
    pub direction: Direction,
    /// Where it is going, e.g. `/srv/app/releases` or `This iPhone`.
    pub target: String,
    pub state: RowState,
    pub done: u64,
    pub total: Option<u64>,
    pub rate: f64,
    pub conflict: Option<ConflictView>,
    pub error: Option<String>,
}

impl TransferRow {
    pub(crate) fn eta_seconds(&self) -> Option<u64> {
        let total = self.total?;
        if self.rate < 1.0 || self.done >= total {
            return None;
        }
        Some(((total - self.done) as f64 / self.rate).ceil() as u64)
    }
}

/// The transfer engine behind the Files screen.
#[cfg(feature = "host-clients")]
pub(crate) mod deletion;

/// Presentation metadata only. The remote worker retains authority and every
/// queued operation keeps its original complete virtual path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteLocation {
    pub path: String,
    pub label: String,
    pub available: bool,
    pub mutations_available: bool,
}
impl RemoteLocation {
    pub(crate) fn contains(&self, path: &str) -> bool {
        path.strip_prefix(&self.path)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with('/'))
    }
    pub(crate) fn display_path(&self, path: &str) -> String {
        let tail = path.strip_prefix(&self.path).unwrap_or("");
        if tail.is_empty() {
            return self.label.clone();
        }
        let separator = if self.label.contains('\\') { "\\" } else { "/" };
        format!(
            "{}{}{}",
            self.label.trim_end_matches(['/', '\\']),
            separator,
            festerm_sftp::path::display_name(tail.trim_start_matches('/')).replace('/', separator)
        )
    }
}

pub(crate) trait Transfers: Send + Sync {
    fn remote_locations(&self) -> Option<Vec<RemoteLocation>> {
        None
    }
    fn remote_path_label(&self, path: &str) -> String {
        if let Some(locations) = self.remote_locations() {
            return locations
                .iter()
                .find(|location| location.contains(path))
                .map_or_else(
                    || "Remote folder".into(),
                    |location| location.display_path(path),
                );
        }
        let root = self.remote_root_label();
        if path == "/" {
            root
        } else {
            format!(
                "{}/{}",
                root.trim_end_matches('/'),
                festerm_sftp::path::display_name(path.trim_start_matches('/'))
            )
        }
    }
    fn remote_root_label(&self) -> String {
        "/".into()
    }
    #[cfg(feature = "host-clients")]
    fn delete_available(&self) -> bool {
        false
    }
    #[cfg(feature = "host-clients")]
    fn review_delete(
        &self,
        _: Vec<String>,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<deletion::Reply<deletion::Review>, String> {
        Err("Deletion is unavailable".into())
    }
    #[cfg(feature = "host-clients")]
    fn commit_delete(&self, _: u64) -> Result<deletion::Reply<deletion::Outcome>, String> {
        Err("Deletion is unavailable".into())
    }
    #[cfg(feature = "host-clients")]
    fn cancel_delete(&self) {}
    #[cfg(feature = "host-clients")]
    fn local_delete_available(&self) -> bool {
        false
    }
    #[cfg(feature = "host-clients")]
    fn review_local_delete(
        &self,
        _: Vec<String>,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<deletion::Reply<deletion::Review>, String> {
        Err("Deletion is unavailable".into())
    }
    #[cfg(feature = "host-clients")]
    fn commit_local_delete(&self, _: u64) -> Result<deletion::Reply<deletion::Outcome>, String> {
        Err("Deletion is unavailable".into())
    }
    #[cfg(feature = "host-clients")]
    fn cancel_local_delete(&self) {}
    /// Ordinary Files keeps private preview copies; SSH retains its historical
    /// preparation, read and cleanup path through these default hooks.
    fn retains_preview_copies(&self) -> bool {
        false
    }
    fn prepare_preview_cache(&self) -> Option<Result<String, SftpError>> {
        None
    }
    fn read_local_markdown(&self, _: &str, _: usize) -> Option<Result<String, String>> {
        None
    }
    fn connection_error(&self) -> Option<String> {
        None
    }
    fn rows(&self) -> Vec<TransferRow>;
    fn enqueue(
        &self,
        direction: Direction,
        source: &str,
        destination_dir: &str,
    ) -> Result<u64, String>;
    fn cancel(&self, id: u64);
    fn resolve(&self, id: u64, policy: ConflictPolicy, remember: bool);
    fn clear_finished(&self);
}

/// Production transfers on a connected [`TransferManager`].
pub(crate) struct ManagerTransfers {
    manager: TransferManager,
}

impl ManagerTransfers {
    pub(crate) fn new(manager: TransferManager) -> Self {
        Self { manager }
    }
}

fn facts_of(fingerprint: festerm_sftp::Fingerprint) -> FileFacts {
    FileFacts {
        size: fingerprint.size,
        modified: fingerprint.modified,
    }
}

impl Transfers for ManagerTransfers {
    fn rows(&self) -> Vec<TransferRow> {
        // The queue of events is only a wake-up signal; snapshot() is the truth.
        drop(self.manager.poll_events());
        self.manager
            .snapshot()
            .into_iter()
            .map(|info| {
                let mut error = None;
                let mut conflict = None;
                let state = match &info.state {
                    TransferState::Queued => RowState::Queued,
                    TransferState::Running => RowState::Active,
                    TransferState::AwaitingConflict(found) => {
                        let (iphone, remote) = match info.direction {
                            Direction::Upload => (found.incoming, found.existing),
                            Direction::Download => (found.existing, found.incoming),
                        };
                        conflict = Some(ConflictView {
                            direction: info.direction,
                            allowed: if found.existing_is_directory {
                                vec![ConflictPolicy::KeepBoth, ConflictPolicy::Skip]
                            } else {
                                vec![
                                    ConflictPolicy::KeepBoth,
                                    ConflictPolicy::Skip,
                                    ConflictPolicy::Replace,
                                ]
                            },
                            can_remember: true,
                            iphone: facts_of(iphone),
                            remote: facts_of(remote),
                            suggested_name: found.suggested_name.clone(),
                            existing_is_directory: found.existing_is_directory,
                            likely_identical: found.likely_identical,
                        });
                        RowState::Awaiting
                    }
                    TransferState::Completed => RowState::Done,
                    TransferState::Skipped => RowState::Skipped,
                    TransferState::Cancelled => RowState::Cancelled,
                    TransferState::Failed(failure) => {
                        error = Some(describe_error(failure));
                        RowState::Failed
                    }
                };
                if let Some(temp) = &info.leftover_remote_temp {
                    error = Some(format!(
                        "{} The server still holds a partial file: {temp}",
                        error.unwrap_or_default()
                    ));
                }
                TransferRow {
                    id: info.id,
                    name: info.name,
                    direction: info.direction,
                    target: match info.direction {
                        Direction::Upload => info.destination_dir,
                        Direction::Download => {
                            super::platform_text("This iPhone", "This device").to_owned()
                        }
                    },
                    state,
                    done: info.progress.bytes_done,
                    total: info.progress.bytes_total,
                    rate: info.progress.rate_bytes_per_sec,
                    conflict,
                    error,
                }
            })
            .collect()
    }

    fn enqueue(
        &self,
        direction: Direction,
        source: &str,
        destination_dir: &str,
    ) -> Result<u64, String> {
        let request = match direction {
            Direction::Upload => TransferRequest::upload(source, destination_dir),
            Direction::Download => TransferRequest::download(source, destination_dir),
        };
        self.manager
            .enqueue(request)
            .map_err(|error| describe_error(&error))
    }

    fn cancel(&self, id: u64) {
        self.manager.cancel(id as TransferId);
    }

    fn resolve(&self, id: u64, policy: ConflictPolicy, remember: bool) {
        // A conflict that already resolved itself is not an error for the UI.
        let _ = self.manager.resolve_conflict(id, policy, remember);
    }

    fn clear_finished(&self) {
        self.manager.clear_finished();
    }
}

pub(crate) fn describe_error(error: &SftpError) -> String {
    match error {
        SftpError::PermissionDenied(_) => "Permission denied".to_owned(),
        SftpError::NotFound(_) => "Not found".to_owned(),
        SftpError::Disconnected(_) => "The connection was lost".to_owned(),
        SftpError::Timeout(_) => "The server stopped answering".to_owned(),
        SftpError::Cancelled => "Cancelled".to_owned(),
        SftpError::LimitExceeded(detail) => format!("Too large or too many: {detail}"),
        other => {
            let text = other.to_string();
            let mut chars = text.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        }
    }
}

type ListingReply = (u64, Result<Listing, SftpError>);

/// One browsable directory with sort, filter and a multi-selection.
pub(crate) struct Pane {
    pub(crate) fs: Arc<dyn PaneFs>,
    pub(crate) path: String,
    pub(crate) listing: Option<Listing>,
    pub(crate) error: Option<String>,
    pub(crate) selected: BTreeSet<String>,
    pub(crate) sort: SortKey,
    pub(crate) order: SortOrder,
    pub(crate) filter: String,
    pub(crate) filter_open: bool,
    pub(crate) show_hidden: bool,
    generation: u64,
    pending: Option<(String, mpsc::Receiver<ListingReply>)>,
}

impl Pane {
    pub(crate) fn new(fs: Arc<dyn PaneFs>, path: String) -> Self {
        Self {
            fs,
            path,
            listing: None,
            error: None,
            selected: BTreeSet::new(),
            sort: SortKey::Name,
            order: SortOrder::Ascending,
            filter: String::new(),
            filter_open: false,
            show_hidden: false,
            generation: 0,
            pending: None,
        }
    }

    pub(crate) fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Lists `path` on a worker thread; the pane keeps its old contents until
    /// the answer arrives.
    pub(crate) fn load(&mut self, path: String, ctx: &egui::Context) {
        self.generation += 1;
        let generation = self.generation;
        let (sender, receiver) = mpsc::channel();
        let fs = Arc::clone(&self.fs);
        let options = ListOptions {
            show_hidden: self.show_hidden,
            sort: self.sort,
            order: self.order,
            ..ListOptions::default()
        };
        let wake = ctx.clone();
        let target = path.clone();
        std::thread::spawn(move || {
            let result = fs.list(&target, &options);
            let _ = sender.send((generation, result));
            wake.request_repaint();
        });
        self.pending = Some((path, receiver));
    }

    pub(crate) fn refresh(&mut self, ctx: &egui::Context) {
        self.load(self.path.clone(), ctx);
    }

    pub(crate) fn open_dir(&mut self, name: &str, ctx: &egui::Context) {
        if let Ok(path) = self.fs.join(&self.path, name) {
            self.selected.clear();
            self.load(path, ctx);
        }
    }

    pub(crate) fn go_up(&mut self, ctx: &egui::Context) {
        if let Some(parent) = self.fs.parent(&self.path) {
            self.selected.clear();
            self.load(parent, ctx);
        }
    }

    pub(crate) fn go_to(&mut self, path: &str, ctx: &egui::Context) {
        self.selected.clear();
        self.load(path.to_owned(), ctx);
    }

    pub(crate) fn can_go_up(&self) -> bool {
        self.fs.parent(&self.path).is_some()
    }

    /// Applies a finished listing. Returns whether anything changed.
    pub(crate) fn poll(&mut self) -> bool {
        let Some((target, receiver)) = &self.pending else {
            return false;
        };
        let Ok((generation, result)) = receiver.try_recv() else {
            return false;
        };
        if generation != self.generation {
            return false;
        }
        let target = target.clone();
        self.pending = None;
        match result {
            Ok(listing) => {
                self.path = target;
                self.error = None;
                let names: HashSet<&str> = listing
                    .entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect();
                self.selected.retain(|name| names.contains(name.as_str()));
                self.listing = Some(listing);
            }
            Err(error) => {
                self.listing = None;
                self.selected.clear();
                self.error = Some(describe_error(&error));
            }
        }
        true
    }

    pub(crate) fn set_sort(&mut self, key: SortKey, ctx: &egui::Context) {
        if self.sort == key {
            self.order = match self.order {
                SortOrder::Ascending => SortOrder::Descending,
                SortOrder::Descending => SortOrder::Ascending,
            };
        } else {
            self.sort = key;
            self.order = SortOrder::Ascending;
        }
        self.refresh(ctx);
    }

    /// Entries after the filter, in listing order.
    pub(crate) fn visible(&self) -> Vec<&Entry> {
        let needle = self.filter.trim().to_lowercase();
        self.listing
            .iter()
            .flat_map(|listing| listing.entries.iter())
            .filter(|entry| {
                needle.is_empty() || entry.display_name.to_lowercase().contains(&needle)
            })
            .collect()
    }

    pub(crate) fn toggle(&mut self, name: &str) {
        if !self.selected.remove(name) {
            self.selected.insert(name.to_owned());
        }
    }

    /// Selected regular files (directories are not transferred).
    pub(crate) fn selected_files(&self) -> Vec<&Entry> {
        self.listing
            .iter()
            .flat_map(|listing| listing.entries.iter())
            .filter(|entry| {
                self.selected.contains(&entry.name)
                    && entry.name_safe
                    && entry.effective_kind() == EntryKind::File
            })
            .collect()
    }

    pub(crate) fn selected_bytes(&self) -> u64 {
        self.selected_files()
            .iter()
            .map(|entry| entry.size.unwrap_or(0))
            .sum()
    }

    /// `(label, path)` for each breadcrumb, root first.
    pub(crate) fn crumbs(&self, root_label: &str) -> Vec<(String, String)> {
        let remote = self.fs.kind() == festerm_sftp::PaneKind::Remote;
        self.crumbs_from(root_label, if remote { "/" } else { "" })
    }

    pub(crate) fn crumbs_from(&self, root_label: &str, root: &str) -> Vec<(String, String)> {
        let remote = self.fs.kind() == festerm_sftp::PaneKind::Remote;
        let mut crumbs = vec![(root_label.to_owned(), root.to_owned())];
        let relative = if root.is_empty() || root == "/" {
            self.path.trim_start_matches('/')
        } else if self.path == root {
            ""
        } else {
            self.path.strip_prefix(&format!("{root}/")).unwrap_or("")
        };
        let mut path = root.trim_end_matches('/').to_owned();
        for part in relative.split('/').filter(|part| !part.is_empty()) {
            if remote || !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            let full = if remote && !path.starts_with('/') {
                format!("/{path}")
            } else {
                path.clone()
            };
            crumbs.push((part.to_owned(), full));
        }
        crumbs
    }
}

pub(crate) fn is_markdown(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// Where the Files screen is in connecting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Link {
    Connecting(String),
    HostKey(HostVerification),
    Password { target: String, retry: bool },
    Ready,
    Failed(String),
}

/// What a connector hands over once the SFTP channel is open.
pub(crate) struct Ready {
    pub remote_write: bool,
    pub remote: Arc<dyn PaneFs>,
    pub local: Arc<dyn PaneFs>,
    pub local_root: PathBuf,
    pub transfers: Arc<dyn Transfers>,
    pub start_dir: Option<String>,
    /// Already admitted first listing for a namespaced HOST start.
    pub initial_remote_listing: Option<Listing>,
}

pub(crate) enum LinkEvent {
    Status(String),
    HostKey(HostVerification),
    NeedPassword { target: String, retry: bool },
    Trust(HostIdentity, String),
    Ready(Box<Ready>),
    Failed(String),
}

/// Opens the SFTP channel off the UI thread. Implemented over
/// `festerm-ssh` in `sftp_link`; tests use a scripted fake.
pub(crate) trait Connector {
    fn poll(&mut self) -> Vec<LinkEvent>;
    fn resolve_host(&mut self, decision: HostDecision);
    fn submit_password(&mut self, password: String);
    fn cancel(&mut self);
}

/// Something for the app to act on after [`FilesSession::poll`].
pub(crate) enum FilesEvent {
    Trust(HostIdentity, String),
    OpenMarkdown {
        name: String,
        location: String,
        text: String,
    },
}

pub(crate) struct FolderDraft {
    pub remote: bool,
    pub path: String,
    pub name: String,
    pub error: Option<String>,
    pending: Option<mpsc::Receiver<Result<(), SftpError>>>,
}
impl FolderDraft {
    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

pub(crate) struct FilesSession {
    #[cfg(feature = "host-clients")]
    pub(crate) deletion: Option<deletion::Draft>,
    #[cfg(feature = "host-clients")]
    delete_result: Option<String>,
    #[cfg(feature = "host-clients")]
    delete_recovery: Option<String>,
    pub(crate) remote_write: bool,
    pub(crate) folder: Option<FolderDraft>,
    pub(crate) title: String,
    /// Protocol and target, e.g. `SFTP · dev@example.net`.
    pub(crate) subtitle: String,
    pub(crate) link: Link,
    pub(crate) remote: Option<Pane>,
    pub(crate) local: Option<Pane>,
    pub(crate) transfers: Option<Arc<dyn Transfers>>,
    pub(crate) rows: Vec<TransferRow>,
    pub(crate) transfers_sheet: bool,
    pub(crate) remember: bool,
    pub(crate) notice: Option<String>,
    pub(crate) password: PasswordBuffer,
    connector: Option<Box<dyn Connector>>,
    local_root: Option<PathBuf>,
    finished_seen: HashSet<u64>,
    opening: Option<(u64, String, String, String)>,
    cache_counter: u32,
}

impl FilesSession {
    pub(crate) fn remote_locations(&self) -> Option<Vec<RemoteLocation>> {
        self.transfers
            .as_ref()
            .and_then(|transfers| transfers.remote_locations())
    }
    pub(crate) fn remote_location(&self) -> Option<RemoteLocation> {
        let path = &self.remote.as_ref()?.path;
        self.remote_locations()?
            .into_iter()
            .find(|location| location.contains(path))
    }
    pub(crate) fn remote_mutations_available(&self) -> bool {
        let Some(pane) = self.remote.as_ref().filter(|pane| pane.error.is_none()) else {
            return false;
        };
        let Some(locations) = self.remote_locations() else {
            return true;
        };
        !pane.loading()
            && pane.listing.as_ref().is_some_and(|listing| {
                listing.path == pane.path && listing.directory_read_only != Some(true)
            })
            && locations.iter().any(|location| {
                location.contains(&pane.path) && location.available && location.mutations_available
            })
    }
    pub(crate) fn can_upload(&self) -> bool {
        self.remote_write && self.remote_mutations_available()
    }
    pub(crate) fn remote_root_label(&self) -> String {
        if self.remote_locations().is_some() {
            return self
                .remote_location()
                .map_or_else(|| "Allowed locations".into(), |location| location.label);
        }
        self.transfers
            .as_ref()
            .map_or_else(|| "/".into(), |transfers| transfers.remote_root_label())
    }

    pub(crate) fn connecting(
        title: String,
        subtitle: String,
        connector: Box<dyn Connector>,
    ) -> Self {
        Self {
            title,
            subtitle,
            link: Link::Connecting("Connecting".to_owned()),
            #[cfg(feature = "host-clients")]
            deletion: None,
            #[cfg(feature = "host-clients")]
            delete_result: None,
            #[cfg(feature = "host-clients")]
            delete_recovery: None,
            remote_write: false,
            folder: None,
            remote: None,
            local: None,
            transfers: None,
            rows: Vec::new(),
            transfers_sheet: false,
            remember: false,
            notice: None,
            password: PasswordBuffer::default(),
            connector: Some(connector),
            local_root: None,
            finished_seen: HashSet::new(),
            opening: None,
            cache_counter: 0,
        }
    }

    /// A session that is already connected (gallery and tests).
    #[cfg(test)]
    pub(crate) fn ready(
        title: String,
        subtitle: String,
        ready: Ready,
        ctx: &egui::Context,
    ) -> Self {
        let mut session = Self::connecting(title, subtitle, Box::new(NoConnector));
        session.connector = None;
        session.install(ready, ctx);
        session
    }

    fn install(&mut self, ready: Ready, ctx: &egui::Context) {
        #[cfg(feature = "host-clients")]
        {
            self.delete_recovery = None;
        }
        self.remote_write = ready.remote_write;
        let remote_start = ready
            .start_dir
            .clone()
            .unwrap_or_else(|| ready.remote.home());
        let mut remote = Pane::new(ready.remote, remote_start.clone());
        if let Some(listing) = ready.initial_remote_listing {
            if listing.path != remote_start {
                self.link = Link::Failed("Files starting folder changed".into());
                return;
            }
            remote.listing = Some(listing);
        } else {
            remote.load(remote_start, ctx);
        }
        let local_start = ready.local.home();
        let mut local = Pane::new(ready.local, local_start.clone());
        local.load(local_start, ctx);
        self.remote = Some(remote);
        self.local = Some(local);
        self.transfers = Some(ready.transfers);
        self.local_root = Some(ready.local_root);
        self.link = Link::Ready;
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.link == Link::Ready
    }

    pub(crate) fn resolve_host(&mut self, decision: HostDecision) {
        if let Some(connector) = self.connector.as_mut() {
            connector.resolve_host(decision);
        }
        self.link = match decision {
            HostDecision::Trust => Link::Connecting("Authenticating".to_owned()),
            HostDecision::Cancel => Link::Failed("Host key rejected".to_owned()),
        };
    }

    pub(crate) fn submit_password(&mut self) {
        let password = self.password.take();
        if let Some(connector) = self.connector.as_mut() {
            connector.submit_password(password);
        }
        self.link = Link::Connecting("Authenticating".to_owned());
    }

    pub(crate) fn cancel_connect(&mut self) {
        if let Some(connector) = self.connector.as_mut() {
            connector.cancel();
        }
        self.password.clear();
        self.link = Link::Failed("Cancelled".to_owned());
    }

    /// Pumps the connector, listings and transfers once per frame.
    pub(crate) fn poll(&mut self, ctx: &egui::Context) -> Vec<FilesEvent> {
        let mut events = Vec::new();
        if let Some(connector) = self.connector.as_mut() {
            for event in connector.poll() {
                match event {
                    LinkEvent::Status(status) => self.link = Link::Connecting(status),
                    LinkEvent::HostKey(model) => self.link = Link::HostKey(model),
                    LinkEvent::NeedPassword { target, retry } => {
                        self.link = Link::Password { target, retry }
                    }
                    LinkEvent::Trust(identity, fingerprint) => {
                        events.push(FilesEvent::Trust(identity, fingerprint));
                    }
                    LinkEvent::Ready(ready) => {
                        self.install(*ready, ctx);
                        self.connector = None;
                        break;
                    }
                    LinkEvent::Failed(message) => {
                        self.link = Link::Failed(message);
                        self.connector = None;
                        break;
                    }
                }
            }
        }
        let mut changed = false;
        for pane in [self.remote.as_mut(), self.local.as_mut()]
            .into_iter()
            .flatten()
        {
            changed |= pane.poll();
            if pane.loading() {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let _ = changed;
        if let Some(transfers) = self.transfers.clone() {
            self.rows = transfers.rows();
            if let Some(message) = transfers.connection_error() {
                self.link = Link::Failed(message);
            }
            if matches!(self.link, Link::Ready) {
                self.after_transfers(ctx, &mut events);
            } else if self.opening.take().is_some() {
                // A completed remote preview is still an automatic disclosure.
                // Retirement drops that intent, not the downloaded local file.
                self.notice = None;
            }
        }
        #[cfg(feature = "host-clients")]
        self.poll_delete(ctx);
        if self.is_ready() {
            self.poll_folder(ctx);
        } else {
            self.folder = None;
        }
        events
    }

    pub(crate) fn new_folder(&mut self, remote: bool) {
        if !self.is_ready() || (remote && !self.can_upload()) {
            return;
        }
        let pane = if remote {
            self.remote.as_ref()
        } else {
            self.local.as_ref()
        };
        if let Some(pane) = pane {
            self.folder = Some(FolderDraft {
                remote,
                path: pane.path.clone(),
                name: String::new(),
                error: None,
                pending: None,
            });
        }
    }
    pub(crate) fn create_folder(&mut self, ctx: &egui::Context) {
        if !self.is_ready() {
            self.folder = None;
            return;
        }
        let remote_write = self.can_upload();
        let Some(draft) = &mut self.folder else {
            return;
        };
        if draft.busy() || (draft.remote && !remote_write) {
            return;
        }
        if draft.name.len() > 255 || festerm_sftp::path::validate_name(&draft.name).is_err() {
            draft.error = Some("Enter one folder name without separators".into());
            return;
        }
        let pane = if draft.remote {
            self.remote.as_ref()
        } else {
            self.local.as_ref()
        };
        let Some(pane) = pane else {
            return;
        };
        if draft.remote && pane.path != draft.path {
            return;
        }
        let path = match pane.fs.join(&draft.path, &draft.name) {
            Ok(path) => path,
            Err(e) => {
                draft.error = Some(describe_error(&e));
                return;
            }
        };
        let fs = pane.fs.clone();
        let (send, receive) = mpsc::sync_channel(1);
        let wake = ctx.clone();
        if std::thread::Builder::new()
            .name("files-new-folder".into())
            .spawn(move || {
                let _ = send.send(fs.mkdir(&path));
                wake.request_repaint();
            })
            .is_err()
        {
            draft.error = Some("Could not start folder creation".into());
            return;
        }
        draft.pending = Some(receive);
        draft.error = None;
    }
    fn poll_folder(&mut self, ctx: &egui::Context) {
        let Some(draft) = &mut self.folder else {
            return;
        };
        let Some(reply) = &draft.pending else {
            return;
        };
        let result = match reply.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(SftpError::Disconnected("Folder worker closed".into()))
            }
        };
        draft.pending = None;
        if let Err(e) = result {
            draft.error = Some(describe_error(&e));
            return;
        }
        let pane = if draft.remote {
            self.remote.as_mut()
        } else {
            self.local.as_mut()
        };
        if let Some(pane) = pane.filter(|pane| pane.path == draft.path) {
            pane.refresh(ctx);
        }
        self.folder = None;
    }

    fn after_transfers(&mut self, ctx: &egui::Context, events: &mut Vec<FilesEvent>) {
        let mut refresh = false;
        let mut opened = None;
        for row in &self.rows {
            if row.state.finished() && self.finished_seen.insert(row.id) {
                refresh |= row.state == RowState::Done;
                if let Some((id, name, location, path)) = &self.opening {
                    if *id == row.id {
                        opened = Some((row.state, name.clone(), location.clone(), path.clone()));
                    }
                }
            }
        }
        if self.rows.iter().any(|row| !row.state.finished()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if refresh {
            for pane in [self.remote.as_mut(), self.local.as_mut()]
                .into_iter()
                .flatten()
            {
                if !pane.loading() {
                    pane.refresh(ctx);
                }
            }
        }
        if let Some((state, name, location, path)) = opened {
            self.opening = None;
            self.notice = None;
            if state == RowState::Done {
                match self.read_local(&path) {
                    Ok(text) => events.push(FilesEvent::OpenMarkdown {
                        name,
                        location,
                        text,
                    }),
                    Err(message) => self.notice = Some(message),
                }
            } else {
                self.notice = Some("Could not download the document".to_owned());
            }
        }
    }

    fn read_local(&self, relative: &str) -> Result<String, String> {
        if let Some(result) = self.transfers.as_ref().and_then(|transfers| {
            transfers.read_local_markdown(relative, MAX_MARKDOWN_BYTES as usize)
        }) {
            return result;
        }
        let root = self.local_root.as_ref().ok_or_else(|| {
            super::platform_text(
                "This iPhone storage is unavailable",
                "Local app storage is unavailable",
            )
            .to_owned()
        })?;
        let path = root.join(relative);
        let size = std::fs::metadata(&path)
            .map_err(|_| "Could not read the document".to_owned())?
            .len();
        if size > MAX_MARKDOWN_BYTES {
            return Err("Documents over 4 MB cannot be previewed".to_owned());
        }
        let bytes = std::fs::read(&path).map_err(|_| "Could not read the document".to_owned())?;
        String::from_utf8(bytes).map_err(|_| "The document is not UTF-8 text".to_owned())
    }

    /// Opens the single selected Markdown file of a pane.
    pub(crate) fn open_markdown(&mut self, remote_side: bool) -> Option<FilesEvent> {
        let pane = if remote_side {
            self.remote.as_ref()?
        } else {
            self.local.as_ref()?
        };
        let entry = pane
            .selected_files()
            .into_iter()
            .find(|entry| is_markdown(&entry.name))?
            .clone();
        let path = pane.fs.join(&pane.path, &entry.name).ok()?;
        if entry.size.unwrap_or(0) > MAX_MARKDOWN_BYTES {
            self.notice = Some("Documents over 4 MB cannot be previewed".to_owned());
            return None;
        }
        if remote_side {
            let transfers = self.transfers.clone()?;
            self.cache_counter = match self.cache_counter.checked_add(1) {
                Some(next) => next,
                None => {
                    self.notice = Some("Reconnect before opening another preview".into());
                    return None;
                }
            };
            let local = self.local.as_ref()?.fs.clone();
            let cache = match transfers.prepare_preview_cache() {
                Some(Ok(path)) => path,
                Some(Err(_)) => {
                    self.notice = Some("Could not prepare private preview storage".into());
                    return None;
                }
                None => {
                    if local.mkdir(CACHE_DIR).is_err() && local.stat(CACHE_DIR).is_err() {
                        self.notice = Some("Could not prepare storage for the document".to_owned());
                        return None;
                    }
                    CACHE_DIR.to_owned()
                }
            };
            let directory = format!("{cache}/{}", self.cache_counter);
            if local.mkdir(&directory).is_err() {
                self.notice = Some("Could not prepare storage for the document".to_owned());
                return None;
            }
            match transfers.enqueue(Direction::Download, &path, &directory) {
                Ok(id) => {
                    self.opening = Some((
                        id,
                        entry.name.clone(),
                        transfers.remote_path_label(&path),
                        format!("{directory}/{}", entry.name),
                    ));
                    self.notice = Some(format!("Opening {}…", entry.display_name));
                }
                Err(error) => self.notice = Some(error),
            }
            None
        } else {
            match self.read_local(&path) {
                Ok(text) => Some(FilesEvent::OpenMarkdown {
                    name: entry.name.clone(),
                    location: format!(
                        "{} · {path}",
                        super::platform_text("This iPhone", "This device")
                    ),
                    text,
                }),
                Err(message) => {
                    self.notice = Some(message);
                    None
                }
            }
        }
    }

    /// Queues the selected files of one pane toward the other.
    pub(crate) fn start_transfer(&mut self, upload: bool) {
        if upload && !self.can_upload() {
            self.notice = Some("Upload is unavailable in this read-only Files view".into());
            return;
        }
        let Some(transfers) = self.transfers.clone() else {
            return;
        };
        let (source, target) = if upload {
            (self.local.as_ref(), self.remote.as_ref())
        } else {
            (self.remote.as_ref(), self.local.as_ref())
        };
        let (Some(source), Some(target)) = (source, target) else {
            return;
        };
        let destination = target.path.clone();
        let mut queued = 0;
        let mut failure = None;
        for entry in source.selected_files() {
            let Ok(path) = source.fs.join(&source.path, &entry.name) else {
                continue;
            };
            let direction = if upload {
                Direction::Upload
            } else {
                Direction::Download
            };
            match transfers.enqueue(direction, &path, &destination) {
                Ok(_) => queued += 1,
                Err(error) => failure = Some(error),
            }
        }
        if let Some(pane) = if upload {
            self.local.as_mut()
        } else {
            self.remote.as_mut()
        } {
            pane.selected.clear();
        }
        self.notice = failure;
        if queued > 0 {
            self.transfers_sheet = false;
        }
    }

    /// First transfer waiting for a conflict decision.
    pub(crate) fn conflict(&self) -> Option<&TransferRow> {
        self.rows
            .iter()
            .find(|row| row.state == RowState::Awaiting && row.conflict.is_some())
    }

    pub(crate) fn active_count(&self) -> usize {
        self.rows.iter().filter(|row| !row.state.finished()).count()
    }

    /// Removes opened-document copies; called when the session closes.
    pub(crate) fn cleanup(&mut self) {
        #[cfg(feature = "host-clients")]
        {
            self.delete_recovery = None;
        }
        #[cfg(feature = "host-clients")]
        self.cancel_delete();
        if self
            .transfers
            .as_ref()
            .is_none_or(|transfers| !transfers.retains_preview_copies())
        {
            if let Some(root) = &self.local_root {
                let _ = std::fs::remove_dir_all(root.join(CACHE_DIR));
            }
        }
        if let Some(transfers) = &self.transfers {
            for row in &self.rows {
                if !row.state.finished() {
                    transfers.cancel(row.id);
                }
            }
        }
        if let Some(connector) = self.connector.as_mut() {
            connector.cancel();
        }
    }
}

impl Drop for FilesSession {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
struct NoConnector;

#[cfg(test)]
impl Connector for NoConnector {
    fn poll(&mut self) -> Vec<LinkEvent> {
        Vec::new()
    }
    fn resolve_host(&mut self, _: HostDecision) {}
    fn submit_password(&mut self, _: String) {}
    fn cancel(&mut self) {}
}

pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `2026-10-31` from Unix seconds (UTC), without a calendar dependency.
pub(crate) fn format_date(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_format_sizes_and_dates_for_rows() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(38_400_000), "38.4 MB");
        assert_eq!(format_size(1_200_000_000), "1.2 GB");
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(1_782_864_000), "2026-07-01");
        assert!(is_markdown("README.MD") && !is_markdown("notes.txt"));
    }

    #[test]
    fn files_transfer_eta_needs_a_rate_and_remaining_bytes() {
        let mut row = TransferRow {
            id: 1,
            name: "a".into(),
            direction: Direction::Upload,
            target: "/".into(),
            state: RowState::Active,
            done: 29_400_000,
            total: Some(38_400_000),
            rate: 3_200_000.0,
            conflict: None,
            error: None,
        };
        assert_eq!(row.eta_seconds(), Some(3));
        row.rate = 0.0;
        assert_eq!(row.eta_seconds(), None);
        row.rate = 1_000.0;
        row.done = 38_400_000;
        assert_eq!(row.eta_seconds(), None);
    }

    use crate::files_fixture::{fixture_entry, FixtureFs, FixtureTransfers};
    use festerm_sftp::PaneKind;
    use std::{cell::RefCell, collections::VecDeque, rc::Rc};

    fn fixture_fs(kind: PaneKind, home: &'static str) -> Arc<FixtureFs> {
        Arc::new(FixtureFs {
            kind,
            home,
            entries: vec![
                fixture_entry("logs", true, 0, 10),
                fixture_entry("README.md", false, 4_200, 20),
                fixture_entry("Archive.TAR.gz", false, 9_000, 30),
            ],
        })
    }

    fn settle(session: &mut FilesSession, ctx: &egui::Context) -> Vec<FilesEvent> {
        let mut events = Vec::new();
        for _ in 0..400 {
            events.extend(session.poll(ctx));
            let loading = [session.remote.as_ref(), session.local.as_ref()]
                .into_iter()
                .flatten()
                .any(Pane::loading);
            if !loading {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        events
    }

    fn ready(transfers: Arc<FixtureTransfers>) -> Ready {
        Ready {
            remote_write: true,
            remote: fixture_fs(PaneKind::Remote, "/srv"),
            local: fixture_fs(PaneKind::Local, ""),
            local_root: PathBuf::from("/nonexistent-festerm-files-test"),
            transfers,
            start_dir: None,
            initial_remote_listing: None,
        }
    }

    #[test]
    fn files_admitted_initial_listing_is_used_once_and_mismatched_path_refuses_ready() {
        let ctx = egui::Context::default();
        for matching in [true, false] {
            let mut data = ready(Arc::new(FixtureTransfers::new(Vec::new())));
            data.start_dir = Some("/already-admitted".into());
            data.initial_remote_listing = Some(festerm_sftp::finish_listing(
                if matching {
                    "/already-admitted"
                } else {
                    "/different"
                }
                .into(),
                Vec::new(),
                false,
                &ListOptions::default(),
            ));
            let mut session = FilesSession::ready("Files".into(), "Host".into(), data, &ctx);
            assert_eq!(session.is_ready(), matching);
            if matching {
                let remote = session.remote.as_ref().unwrap();
                assert!(
                    !remote.loading(),
                    "no second directory request after admission"
                );
                assert_eq!(remote.listing.as_ref().unwrap().path, "/already-admitted");
            } else {
                assert!(session.remote.is_none());
                assert!(session.transfers.is_none());
            }
            session.cleanup();
        }
    }

    #[test]
    fn files_start_directory_initializes_child_but_keeps_allowed_root_up_fence() {
        let ctx = egui::Context::default();
        let mut data = ready(Arc::new(FixtureTransfers::new(Vec::new())));
        data.start_dir = Some("/child".into());
        let mut session = FilesSession::ready("Files".into(), "Host".into(), data, &ctx);
        settle(&mut session, &ctx);
        let remote = session.remote.as_mut().unwrap();
        assert_eq!(remote.path, "/child");
        assert!(remote.can_go_up());
        assert_eq!(
            remote.crumbs("/home/example")[0],
            ("/home/example".into(), "/".into())
        );
        remote.go_up(&ctx);
        settle(&mut session, &ctx);
        let remote = session.remote.as_ref().unwrap();
        assert_eq!(remote.path, "/");
        assert!(!remote.can_go_up());
    }

    #[test]
    fn files_allowed_locations_choose_without_exposing_selectors_or_enabling_unavailable_writes() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        for width in [360.0, 375.0] {
            let ctx = egui::Context::default();
            let mut transfers = FixtureTransfers::new(Vec::new());
            let selector = "/11111111111111111111111111111111";
            transfers.locations = Some(vec![
                RemoteLocation {
                    path: selector.into(),
                    label: "/Users/example".into(),
                    available: true,
                    mutations_available: false,
                },
                RemoteLocation {
                    path: "/22222222222222222222222222222222".into(),
                    label: "/Volumes/offline".into(),
                    available: false,
                    mutations_available: false,
                },
            ]);
            let mut data = ready(Arc::new(transfers));
            data.start_dir = Some("/".into());
            let mut session = FilesSession::ready("Files".into(), "Host".into(), data, &ctx);
            settle(&mut session, &ctx);
            assert!(!session.can_upload());
            let mut harness = Harness::builder()
                .with_size(egui::vec2(width, 853.0))
                .build_ui_state(
                    |ui, session: &mut FilesSession| {
                        crate::files_ui::show(ui, session);
                    },
                    session,
                );
            harness.run();
            harness.get(
                By::new()
                    .label("/Volumes/offline — Unavailable")
                    .predicate(|node| node.is_disabled()),
            );
            harness.get_by_label("/Users/example").click();
            // Dispatch the actual click, then await the existing bounded
            // listing worker before asking the UI harness to become idle.
            harness.step();
            settle(harness.state_mut(), &ctx);
            assert!(!harness.state().remote.as_ref().unwrap().loading());
            harness.run();
            assert_eq!(harness.state().remote.as_ref().unwrap().path, selector);
            assert_eq!(harness.state().remote_root_label(), "/Users/example");
            assert!(!harness.state().can_upload());
            harness.get(
                By::new()
                    .label("New remote folder")
                    .predicate(|node| node.is_disabled()),
            );
            harness.get(
                By::new()
                    .label("New local folder")
                    .predicate(|node| !node.is_disabled()),
            );
            assert_eq!(
                harness
                    .get_all(
                        By::new()
                            .label("Parent folder")
                            .predicate(|node| node.is_disabled())
                    )
                    .count(),
                2
            );
            assert!(harness.query_by_label(selector).is_none());
            let pane = harness.state_mut().remote.as_mut().unwrap();
            pane.path = format!("{selector}/projects");
            assert_eq!(
                pane.crumbs_from("/Users/example", selector),
                vec![
                    ("/Users/example".into(), selector.into()),
                    ("projects".into(), format!("{selector}/projects")),
                ]
            );
            let mut retired = FixtureTransfers::new(Vec::new());
            retired.locations = Some(Vec::new());
            harness.state_mut().transfers = Some(Arc::new(retired));
            harness.run();
            harness.get_by_label("Allowed locations");
            assert!(harness
                .query_by_label(selector.trim_start_matches('/'))
                .is_none());
            assert!(!harness.state().can_upload());
        }
    }

    #[test]
    fn files_location_display_paths_and_namespace_matching_do_not_mix_roots() {
        for (label, expected) in [
            ("/", "/projects"),
            ("/Users/example", "/Users/example/projects"),
            ("C:\\Users\\example", "C:\\Users\\example\\projects"),
        ] {
            let location = RemoteLocation {
                path: "/11111111111111111111111111111111".into(),
                label: label.into(),
                available: true,
                mutations_available: true,
            };
            assert!(location.contains(&location.path));
            assert!(location.contains(&format!("{}/projects", location.path)));
            assert!(!location.contains(&format!("{}0/projects", location.path)));
            assert_eq!(location.display_path(&location.path), label);
            assert_eq!(
                location.display_path(&format!("{}/projects", location.path)),
                expected
            );
        }
    }

    #[test]
    fn files_failed_view_offers_explicit_location_choice_only_for_host_context() {
        use egui_kittest::{kittest::Queryable, Harness};
        for host_context in [false, true] {
            let mut session = FilesSession::connecting(
                "Files".into(),
                "Dev Tunnels Files".into(),
                Box::new(NoConnector),
            );
            session.link = Link::Failed("Starting folder unavailable".into());
            let mut harness = Harness::builder()
                .with_size(egui::vec2(375.0, 853.0))
                .build_ui_state(
                    move |ui, state: &mut (FilesSession, bool, bool)| {
                        let outcome = crate::files_ui::show_with_location_choice(
                            ui,
                            &mut state.0,
                            host_context,
                        );
                        state.1 |= outcome.choose_location;
                        state.2 |= outcome.retry;
                    },
                    (session, false, false),
                );
            harness.run();
            if host_context {
                harness.get_by_label("Choose location").click();
                harness.run();
                assert!(harness.state().1);
                assert!(
                    !harness.state().2,
                    "choice is distinct from retrying the default start"
                );
            } else {
                assert!(
                    harness.query_by_label("Choose location").is_none(),
                    "a protocol caption cannot authorize HOST routing"
                );
            }
            assert!(matches!(harness.state().0.link, Link::Failed(_)));
            assert!(harness.state().0.remote.is_none());
        }
    }

    #[test]
    fn files_pane_filters_selects_and_only_offers_regular_files() {
        let ctx = egui::Context::default();
        let mut session = FilesSession::ready(
            "x".into(),
            "SFTP".into(),
            ready(Arc::new(FixtureTransfers::new(Vec::new()))),
            &ctx,
        );
        settle(&mut session, &ctx);
        let pane = session.remote.as_mut().unwrap();
        assert_eq!(pane.path, "/srv");
        pane.filter = "tar".into();
        assert_eq!(pane.visible().len(), 1, "filter is case-insensitive");
        pane.filter.clear();
        pane.toggle("logs");
        pane.toggle("README.md");
        assert_eq!(
            pane.selected_files().len(),
            1,
            "folders are not transferred"
        );
        assert_eq!(pane.selected_bytes(), 4_200);
        pane.toggle("README.md");
        assert!(pane.selected_files().is_empty());
        assert_eq!(pane.crumbs("Documents")[1].0, "srv");
    }

    #[test]
    fn files_failed_refresh_clears_only_failed_pane_and_allows_up() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };

        let ctx = egui::Context::default();
        let mut data = ready(Arc::new(FixtureTransfers::new(Vec::new())));
        data.local = Arc::new(FixtureFs {
            kind: PaneKind::Local,
            home: "",
            entries: vec![fixture_entry("local.md", false, 31, 0)],
        });
        let mut session = FilesSession::ready("Files".into(), "Host".into(), data, &ctx);
        settle(&mut session, &ctx);
        session.remote.as_mut().unwrap().toggle("README.md");
        session.local.as_mut().unwrap().toggle("local.md");

        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 853.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run();
        harness.get(
            By::new()
                .label("Download 1")
                .predicate(|node| !node.is_disabled()),
        );

        let session = harness.state_mut();
        let remote = session.remote.as_mut().unwrap();
        let (send, receive) = mpsc::channel();
        remote.generation += 1;
        send.send((remote.generation, Err(SftpError::NotFound("/srv".into()))))
            .unwrap();
        remote.pending = Some((remote.path.clone(), receive));
        session.poll(&ctx);

        let remote = session.remote.as_ref().unwrap();
        assert_eq!(remote.path, "/srv");
        assert!(remote.listing.is_none() && remote.selected.is_empty());
        assert!(session.is_ready());
        assert!(session.open_markdown(true).is_none());
        assert_eq!(session.cache_counter, 0, "no stale preview preparation");
        session.start_transfer(false);
        assert!(
            session.notice.is_none(),
            "no stale Download reached enqueue"
        );
        assert_eq!(
            session.local.as_ref().unwrap().selected_files()[0].name,
            "local.md"
        );
        harness.run();
        harness.get_by_label("Not found");
        assert!(harness.query_by_label("README.md").is_none());
        harness.get(
            By::new()
                .label("Download")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("Upload 1")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New remote folder")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New local folder")
                .predicate(|node| !node.is_disabled()),
        );
        harness.get_by_label("Open");

        harness
            .get(
                By::new()
                    .label("Parent folder")
                    .predicate(|node| !node.is_disabled()),
            )
            .click();
        harness.step();
        settle(harness.state_mut(), &ctx);
        harness.run();
        let remote = harness.state().remote.as_ref().unwrap();
        assert_eq!(remote.path, "/");
        assert!(remote.error.is_none());
        assert!(remote.selected.is_empty());
        assert!(remote
            .visible()
            .iter()
            .any(|entry| entry.name == "README.md"));
        assert!(harness.state().can_upload());
    }

    #[test]
    fn files_physical_root_is_display_only_and_unsupported_entries_are_unavailable() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        for root in ["/Users/example", "C:\\Users\\example", "Shared folder"] {
            for width in [360.0, 375.0] {
                let ctx = egui::Context::default();
                let mut transfers = FixtureTransfers::new(Vec::new());
                transfers.display_root = Some(root.into());
                let mut data = ready(Arc::new(transfers));
                data.start_dir = Some("/".into());
                data.remote_write = false;
                let mut session = FilesSession::ready("Files".into(), "Host".into(), data, &ctx);
                settle(&mut session, &ctx);
                let pane = session.remote.as_mut().unwrap();
                let mut entry = crate::files_fixture::fixture_entry("src", false, 0, 1);
                entry.kind = EntryKind::Other;
                pane.listing.as_mut().unwrap().entries.push(entry);
                assert_eq!(pane.crumbs(root)[0], (root.into(), "/".into()));
                assert!(!pane.can_go_up());
                let mut harness = Harness::builder()
                    .with_size(egui::vec2(width, 853.0))
                    .build_ui_state(
                        |ui, session: &mut FilesSession| {
                            crate::files_ui::show(ui, session);
                        },
                        session,
                    );
                harness.run();
                harness.get_by_label(root);
                harness.get(
                    By::new()
                        .label("src — Unavailable")
                        .predicate(|node| node.is_disabled()),
                );
                harness.get(
                    By::new()
                        .label("New remote folder")
                        .predicate(|node| node.is_disabled()),
                );
                harness.get(
                    By::new()
                        .label("New local folder")
                        .predicate(|node| !node.is_disabled()),
                );
                assert_eq!(
                    harness
                        .get_all(
                            By::new()
                                .label("Parent folder")
                                .predicate(|node| node.is_disabled())
                        )
                        .count(),
                    2,
                    "both the virtual remote root and Documents root disable Up"
                );
                assert!(harness
                    .state()
                    .remote
                    .as_ref()
                    .unwrap()
                    .selected_files()
                    .is_empty());
                assert_eq!(harness.state().remote.as_ref().unwrap().path, "/");
            }
        }
    }

    #[test]
    fn files_read_only_capability_disables_upload_and_rejects_stale_dispatch() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        let ctx = egui::Context::default();
        let selector = "/11111111111111111111111111111111";
        let mut transfers = FixtureTransfers::new(Vec::new());
        transfers.locations = Some(vec![RemoteLocation {
            path: selector.into(),
            label: "/owned".into(),
            available: true,
            mutations_available: true,
        }]);
        let mut data = ready(Arc::new(transfers));
        data.start_dir = Some(format!("{selector}/child"));
        data.remote_write = false;
        let mut session = FilesSession::ready("Files".into(), "Read-only".into(), data, &ctx);
        settle(&mut session, &ctx);
        session.local.as_mut().unwrap().toggle("README.md");
        session.remote.as_mut().unwrap().toggle("README.md");
        session.start_transfer(true);
        assert_eq!(
            session.notice.as_deref(),
            Some("Upload is unavailable in this read-only Files view")
        );
        session
            .remote
            .as_mut()
            .unwrap()
            .listing
            .as_mut()
            .unwrap()
            .entries
            .iter_mut()
            .find(|entry| entry.name == "README.md")
            .unwrap()
            .size = None;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, session: &mut FilesSession| {
                    crate::files_ui::show(ui, session);
                },
                session,
            );
        harness.run();
        harness.get_by_label("3 items · 1 selected");
        assert!(
            harness
                .query_by_label("3 items · 1 selected · 0 B")
                .is_none(),
            "an unreported HOST listing size is not a zero-byte file"
        );
        harness
            .state_mut()
            .remote
            .as_mut()
            .unwrap()
            .listing
            .as_mut()
            .unwrap()
            .entries
            .iter_mut()
            .find(|entry| entry.name == "README.md")
            .unwrap()
            .size = Some(0);
        harness.run();
        harness.get_by_label("3 items · 1 selected · 0 B");
        harness.get(
            By::new()
                .label("Upload 1")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("Download 1")
                .predicate(|node| !node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New remote folder")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New local folder")
                .predicate(|node| !node.is_disabled()),
        );
        let session = harness.state_mut();
        session.remote_write = true;
        assert!(
            session.can_upload(),
            "legacy location listings allow mutations"
        );
        let pane = session.remote.as_mut().unwrap();
        pane.listing.as_mut().unwrap().directory_read_only = Some(true);
        session.start_transfer(true);
        session.new_folder(true);
        assert_eq!(
            session.notice.as_deref(),
            Some("Upload is unavailable in this read-only Files view")
        );
        assert!(session.folder.is_none());
        harness.run();
        harness.get(
            By::new()
                .label("Upload 1")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New remote folder")
                .predicate(|node| node.is_disabled()),
        );
        let session = harness.state_mut();
        let pane = session.remote.as_mut().unwrap();
        let mut snapshot = pane.listing.take().unwrap();
        assert!(!session.can_upload());
        snapshot.directory_read_only = Some(false);
        let pane = session.remote.as_mut().unwrap();
        let current_path = pane.path.clone();
        snapshot.path = selector.into();
        pane.listing = Some(snapshot);
        assert!(!session.can_upload());
        let pane = session.remote.as_mut().unwrap();
        pane.listing.as_mut().unwrap().path = current_path.clone();
        pane.error = Some("Refresh failed".into());
        assert!(!session.can_upload());
        let pane = session.remote.as_mut().unwrap();
        pane.error = None;
        let (_send, receive) = mpsc::channel();
        pane.pending = Some((current_path, receive));
        assert!(!session.can_upload());
        session.remote.as_mut().unwrap().pending = None;
        harness.run();
        harness.get(
            By::new()
                .label("Upload 1")
                .predicate(|node| !node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("New remote folder")
                .predicate(|node| !node.is_disabled()),
        );
    }

    #[test]
    fn files_retired_connection_never_opens_completed_remote_preview_but_keeps_local_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct RetirableTransfers {
            fixture: FixtureTransfers,
            retired: AtomicBool,
        }
        impl Transfers for RetirableTransfers {
            fn connection_error(&self) -> Option<String> {
                self.retired
                    .load(Ordering::SeqCst)
                    .then(|| "Connection closed".into())
            }
            fn rows(&self) -> Vec<TransferRow> {
                self.fixture.rows()
            }
            fn enqueue(&self, _: Direction, _: &str, _: &str) -> Result<u64, String> {
                Err("Not used".into())
            }
            fn cancel(&self, _: u64) {}
            fn resolve(&self, _: u64, _: ConflictPolicy, _: bool) {}
            fn clear_finished(&self) {}
        }
        let ctx = egui::Context::default();
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join(CACHE_DIR).join("1");
        std::fs::create_dir_all(&cache).unwrap();
        let bytes = b"# Public preview\n";
        std::fs::write(cache.join("README.md"), bytes).unwrap();
        std::fs::write(root.path().join("README.md"), bytes).unwrap();
        let row = crate::files_fixture::transfer_rows(false)
            .into_iter()
            .find(|row| row.state == RowState::Done)
            .unwrap();
        let id = row.id;
        let engine = Arc::new(RetirableTransfers {
            fixture: FixtureTransfers::new(vec![row]),
            retired: AtomicBool::new(false),
        });
        let make_session = || {
            let mut data = ready(Arc::new(FixtureTransfers::new(Vec::new())));
            data.transfers = engine.clone();
            data.local_root = root.path().to_owned();
            data.local = Arc::new(festerm_sftp::LocalFs::new(root.path()).unwrap());
            let mut session = FilesSession::ready("Files".into(), "Read-only".into(), data, &ctx);
            session.cache_counter = 1;
            session.opening = Some((
                id,
                "README.md".into(),
                "/README.md".into(),
                format!("{CACHE_DIR}/1/README.md"),
            ));
            session
        };
        let mut active = make_session();
        assert!(matches!(
            active.poll(&ctx).as_slice(),
            [FilesEvent::OpenMarkdown { .. }]
        ));
        let mut retired = make_session();
        engine.retired.store(true, Ordering::SeqCst);
        assert!(
            retired.poll(&ctx).is_empty(),
            "retirement must suppress automatic disclosure"
        );
        assert!(matches!(retired.link, Link::Failed(_)));
        assert!(retired.opening.is_none());
        assert_eq!(std::fs::read(cache.join("README.md")).unwrap(), bytes);
        engine.retired.store(false, Ordering::SeqCst);
        assert!(
            retired.poll(&ctx).is_empty(),
            "pending preview must not replay"
        );
        settle(&mut retired, &ctx);
        retired.local.as_mut().unwrap().toggle("README.md");
        assert!(
            matches!(
                retired.open_markdown(false),
                Some(FilesEvent::OpenMarkdown { .. })
            ),
            "explicit local preview is independent of retired remote authority"
        );
    }

    #[test]
    fn files_new_folder_uses_bounded_name_and_current_pane_without_remote_write_authority() {
        let ctx = egui::Context::default();
        let root = tempfile::tempdir().unwrap();
        let mut data = ready(Arc::new(FixtureTransfers::new(Vec::new())));
        data.remote_write = false;
        data.local = Arc::new(festerm_sftp::LocalFs::new(root.path()).unwrap());
        data.local_root = root.path().to_owned();
        let mut session = FilesSession::ready("Files".into(), "Read-only".into(), data, &ctx);
        session.new_folder(true);
        assert!(
            session.folder.is_none(),
            "read-only remote must not admit a mutation draft"
        );
        session.new_folder(false);
        session.folder.as_mut().unwrap().name = "../outside".into();
        session.create_folder(&ctx);
        assert!(session.folder.as_ref().unwrap().error.is_some());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        session.folder.as_mut().unwrap().name = "Public folder α".into();
        session.create_folder(&ctx);
        for _ in 0..400 {
            session.poll(&ctx);
            if session.folder.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(session.folder.is_none());
        assert!(root.path().join("Public folder α").is_dir());
        session.new_folder(false);
        session.folder.as_mut().unwrap().name = "never-created".into();
        session.link = Link::Failed("Connection closed".into());
        session.create_folder(&ctx);
        assert!(session.folder.is_none());
        assert!(!root.path().join("never-created").exists());
    }

    #[test]
    fn files_conflict_projects_only_supported_actions_and_no_cross_batch_remember() {
        use egui_kittest::{
            kittest::{By, Queryable},
            Harness,
        };
        let row = crate::files_fixture::transfer_rows(true)
            .into_iter()
            .find(|row| row.conflict.is_some())
            .unwrap();
        let mut view = row.conflict.unwrap();
        view.existing_is_directory = false;
        view.allowed = vec![ConflictPolicy::Skip];
        view.can_remember = false;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, state: &mut (ConflictView, bool, Option<ConflictPolicy>)| {
                    if let Some(choice) = crate::files_ui::conflict_sheet_content(
                        ui,
                        "public.txt",
                        "/",
                        &state.0,
                        &mut state.1,
                    ) {
                        state.2 = Some(choice);
                    }
                },
                (view, true, None),
            );
        harness.run();
        harness.get(
            By::new()
                .label("Keep both")
                .predicate(|node| node.is_disabled()),
        );
        harness.get(
            By::new()
                .label("Replace remote file")
                .predicate(|node| node.is_disabled()),
        );
        assert!(harness
            .query(By::new().label("Use this choice for remaining conflicts"))
            .is_none());
        assert!(!harness.state().1);
        harness.get(By::new().label("Skip file")).click();
        harness.run();
        assert_eq!(harness.state().2, Some(ConflictPolicy::Skip));
    }

    #[test]
    fn files_conflict_waits_for_a_choice_and_hands_it_to_the_engine() {
        let ctx = egui::Context::default();
        let engine = Arc::new(FixtureTransfers::new(Vec::new()));
        let mut session =
            FilesSession::ready("x".into(), "SFTP".into(), ready(engine.clone()), &ctx);
        settle(&mut session, &ctx);
        assert!(session.conflict().is_none());
        let mut rows = crate::files_fixture::transfer_rows(true);
        rows.retain(|row| row.state == RowState::Awaiting);
        let engine = Arc::new(FixtureTransfers::new(rows));
        let mut session =
            FilesSession::ready("x".into(), "SFTP".into(), ready(engine.clone()), &ctx);
        settle(&mut session, &ctx);
        let waiting = session.conflict().expect("awaiting transfer").id;
        engine.resolve(waiting, ConflictPolicy::Replace, true);
        assert_eq!(
            engine.resolved.lock().unwrap().as_slice(),
            [(waiting, ConflictPolicy::Replace, true)]
        );
    }

    struct Script {
        events: Rc<RefCell<VecDeque<LinkEvent>>>,
        resolved: Rc<RefCell<Vec<HostDecision>>>,
        passwords: Rc<RefCell<Vec<String>>>,
    }

    impl Connector for Script {
        fn poll(&mut self) -> Vec<LinkEvent> {
            self.events.borrow_mut().drain(..).collect()
        }
        fn resolve_host(&mut self, decision: HostDecision) {
            self.resolved.borrow_mut().push(decision);
        }
        fn submit_password(&mut self, password: String) {
            self.passwords.borrow_mut().push(password);
        }
        fn cancel(&mut self) {}
    }

    #[test]
    fn files_link_walks_trust_password_and_failure_without_leaking_the_password() {
        let ctx = egui::Context::default();
        let events = Rc::new(RefCell::new(VecDeque::new()));
        let resolved = Rc::new(RefCell::new(Vec::new()));
        let passwords = Rc::new(RefCell::new(Vec::new()));
        let mut session = FilesSession::connecting(
            "x".into(),
            "SFTP".into(),
            Box::new(Script {
                events: events.clone(),
                resolved: resolved.clone(),
                passwords: passwords.clone(),
            }),
        );
        let host = HostVerification {
            name: "h".into(),
            target: "h:22".into(),
            algorithm: "SHA-256".into(),
            fingerprint: "SHA256:abc".into(),
            changed_from: None,
        };
        events
            .borrow_mut()
            .push_back(LinkEvent::HostKey(host.clone()));
        session.poll(&ctx);
        assert_eq!(session.link, Link::HostKey(host));
        session.resolve_host(HostDecision::Trust);
        assert_eq!(resolved.borrow().as_slice(), [HostDecision::Trust]);
        assert!(matches!(session.link, Link::Connecting(_)));

        events.borrow_mut().push_back(LinkEvent::NeedPassword {
            target: "u@h".into(),
            retry: true,
        });
        session.poll(&ctx);
        assert!(matches!(session.link, Link::Password { retry: true, .. }));
        session.password.0.push_str("secret");
        session.submit_password();
        assert_eq!(passwords.borrow().as_slice(), ["secret"]);
        assert!(
            session.password.0.is_empty(),
            "the buffer is emptied on submit"
        );

        events
            .borrow_mut()
            .push_back(LinkEvent::Failed("Authentication failed".into()));
        session.poll(&ctx);
        assert_eq!(session.link, Link::Failed("Authentication failed".into()));
    }

    #[test]
    fn files_link_rejecting_the_host_key_fails_the_session() {
        let events = Rc::new(RefCell::new(VecDeque::new()));
        let resolved = Rc::new(RefCell::new(Vec::new()));
        let mut session = FilesSession::connecting(
            "x".into(),
            "SFTP".into(),
            Box::new(Script {
                events,
                resolved: resolved.clone(),
                passwords: Rc::default(),
            }),
        );
        session.resolve_host(HostDecision::Cancel);
        assert_eq!(resolved.borrow().as_slice(), [HostDecision::Cancel]);
        assert!(matches!(session.link, Link::Failed(_)));
    }
}
