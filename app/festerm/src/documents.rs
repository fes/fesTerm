//! The one place a document lives while it is open (ADR 0034 §1, §2, §6).
//!
//! Opening the same file in a second view — another tab, a Preview, another
//! window — must not produce a second copy of the text with its own undo
//! history and its own idea of whether there are unsaved changes. The registry
//! is what makes "the same file" mean one buffer: it is owned by the
//! application rather than by any window, keyed by origin, and reference
//! counted by view, so the last view closing is what forgets the document.
//!
//! Every window runs in the same pass on the same thread, so shared ownership
//! here is `Rc<RefCell<…>>` rather than a lock: there is no second thread to
//! contend with, and a lock would only hide re-entrancy bugs that a `RefCell`
//! panics on loudly.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{atomic::Ordering, Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use eframe::egui;

mod local_io;

use festerm_document::{
    AutoSaveControl, Availability, DocumentBounds, DocumentId, DocumentKey, DocumentOrigin,
    DocumentStatus, LocalOrigin, OriginError, SaveError, SaveOutcome, SaveProgress, StatusInputs,
    TextDocument, UnavailableReason, UntitledOrigin,
};

use festerm_syntax::{DocumentSyntax, SyntaxStatus};

use crate::document_store::{
    self, ConfirmedDestination, DestinationExpectation, Freshness, Generation, LoadFailure,
    LocalSourceAuthority, RecoveryAuthority, SaveExpectation, SaveFailure,
};

/// How often an open local document is re-checked against its file. Short
/// enough that a `git checkout` in the next window is noticed while the user
/// is still looking at it, long enough that a handful of open files cost
/// nothing measurable.
const POLL_INTERVAL: Duration = Duration::from_millis(1_500);

/// How long "Reloaded from disk" stays up. Long enough to read, short enough
/// that it is gone before it becomes stale news.
const RELOAD_NOTICE: Duration = Duration::from_secs(8);

/// How long a document must sit unchanged before Auto-save writes it.
///
/// Auto-save is idle-debounced rather than periodic (ADR 0034 §7): a write per
/// keystroke would put a partial word on disk, wake every other view's poll,
/// and, on a remote origin, spend a round trip on text the user is still in
/// the middle of typing.
const AUTO_SAVE_IDLE: Duration = Duration::from_millis(900);
pub(crate) const MAX_PENDING_RECOVERY_NOTICES: usize = 16;
pub(crate) const MAX_OVERFLOW_RECOVERY_NOTICES: usize = 64;
const MAX_RECOVERY_PROTECTIONS: usize = 64;

/// The application-scoped registry handle every window holds.
pub(crate) type SharedDocuments = Rc<RefCell<DocumentRegistry>>;

/// One open document and everything true about it that is not presentation.
#[derive(Debug)]
pub(crate) struct OpenDocument {
    origin: DocumentOrigin,
    text: TextDocument,
    generation: Option<Generation>,
    source_authority: Option<LocalSourceAuthority>,
    views: usize,
    read_only: bool,
    availability: Availability,
    conflict: Option<festerm_document::ConflictState>,
    save: SaveProgress,
    auto_save_requested: bool,
    last_error: Option<SaveError>,
    recovery_error: Option<SaveError>,
    recovery_path: Option<PathBuf>,
    recovery_notice_queued: bool,
    /// When this document's source was last checked, so metadata queries run
    /// once per interval, not per frame. Windows also opens a file handle to
    /// retrieve its stable identity; Unix obtains identity from the stat.
    checked: Instant,
    /// When an outside change was last taken up, so the notice can fade
    /// instead of sitting there claiming news that is minutes old.
    reloaded: Option<Instant>,
    /// The content revision last seen by Auto-save and when it was seen, which
    /// is what turns a stream of keystrokes into one write once typing stops.
    settled: Option<(u64, Instant)>,
    /// The parse tree and span cache for this document's text, living beside
    /// the bytes so two views — and a Split's two panes — parse it once
    /// (ADR 0035 §1).
    syntax: DocumentSyntax,
    /// The revision an Auto-save last failed at. Auto-save does not try that
    /// same content again: a failing write retried on a timer would bury the
    /// error under its own repetition (ADR 0034 §7).
    auto_save_blocked_at: Option<u64>,
    io_epoch: u64,
    background: Option<Registration>,
    background_error: Option<SaveError>,
    background_load_error: Option<LoadFailure>,
}

impl Drop for OpenDocument {
    fn drop(&mut self) {
        self.cancel_background();
    }
}

#[derive(Debug)]
struct Registration {
    revision: u64,
    auto_save: bool,
    control: Arc<local_io::Control>,
}

impl OpenDocument {
    fn cancel_background(&mut self) {
        if let Some(background) = &self.background {
            background.control.cancel();
        }
    }

    fn invalidate_background(&mut self) {
        self.cancel_background();
        self.background = None;
        self.io_epoch += 1;
    }
}

impl OpenDocument {
    pub(crate) const fn origin(&self) -> &DocumentOrigin {
        &self.origin
    }

    pub(crate) const fn text(&self) -> &TextDocument {
        &self.text
    }

    pub(crate) fn text_mut(&mut self) -> &mut TextDocument {
        self.cancel_background();
        &mut self.text
    }

    pub(crate) fn local_source_generation(&self) -> Option<(&LocalSourceAuthority, Generation)> {
        self.source_authority
            .as_ref()
            .zip(self.generation)
            .filter(|_| {
                matches!(self.origin, DocumentOrigin::Local(_))
                    && matches!(self.availability, Availability::Available)
            })
    }

    pub(crate) const fn views(&self) -> usize {
        self.views
    }

    #[cfg(test)]
    pub(crate) fn recovery_path(&self) -> Option<&Path> {
        self.recovery_path.as_deref()
    }

    pub(crate) const fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) const fn auto_save_requested(&self) -> bool {
        self.auto_save_requested
    }

    pub(crate) fn set_auto_save_requested(&mut self, requested: bool) {
        if self.auto_save_requested != requested {
            self.cancel_background();
        }
        self.auto_save_requested = requested;
    }

    /// Dismisses a conflict banner without writing anything: the user has
    /// decided their version is the one they want to keep for now (ADR 0034
    /// §6).
    pub(crate) fn keep_my_version(&mut self) {
        self.invalidate_background();
        self.conflict = None;
    }

    pub(crate) fn conflict(&self) -> Option<&festerm_document::ConflictState> {
        self.conflict.as_ref()
    }

    /// The derived status every surface reads from, so the banner, the Save
    /// button, the chip, and the status bar cannot disagree.
    pub(crate) fn status(&self) -> DocumentStatus {
        self.status_with_error(
            self.last_error
                .clone()
                .or_else(|| self.background_error.clone())
                .or_else(|| {
                    self.background_load_error
                        .as_ref()
                        .map(|error| SaveError::new(error.headline(), error.detail()))
                }),
        )
    }

    fn status_with_error(&self, last_error: Option<SaveError>) -> DocumentStatus {
        DocumentStatus::derive(&StatusInputs {
            dirty: self.text.is_dirty(),
            save: if self
                .background
                .as_ref()
                .is_some_and(|registration| registration.control.saving.load(Ordering::Acquire))
            {
                SaveProgress::InFlight
            } else {
                self.save
            },
            availability: self.availability.clone(),
            conflict: self.conflict.clone(),
            auto_save_requested: self.auto_save_requested,
            last_error,
            recovery_error: self.recovery_error.clone(),
            remote: self.origin.is_remote(),
            has_save_target: !matches!(self.origin, DocumentOrigin::Untitled(_)),
            recently_reloaded: self.reloaded.is_some_and(|at| at.elapsed() < RELOAD_NOTICE),
        })
    }

    pub(crate) const fn requires_save_as(&self) -> bool {
        matches!(self.origin, DocumentOrigin::Untitled(_))
    }

    /// The spans covering `range`, reparsing only if the text has moved on.
    ///
    /// Read-only over the text: this produces colour, never bytes, so it
    /// cannot dirty the document or enter its undo history (ADR 0035 §1).
    pub(crate) fn syntax_spans(
        &mut self,
        range: std::ops::Range<usize>,
    ) -> &[festerm_syntax::Span] {
        let Self { text, syntax, .. } = self;
        syntax.spans(text.text(), text.revision(), range)
    }

    #[cfg(test)]
    pub(crate) fn syntax_spans_without_budget_for_test(
        &mut self,
        range: std::ops::Range<usize>,
    ) -> &[festerm_syntax::Span] {
        let Self { text, syntax, .. } = self;
        syntax.spans_without_budget_for_test(text.text(), text.revision(), range)
    }

    /// Why this document has no colour, when it has none.
    pub(crate) fn syntax_status(&self) -> SyntaxStatus {
        self.syntax.status()
    }

    /// Whether a debounced auto-save should actually write right now.
    pub(crate) fn auto_save_should_write(&self) -> bool {
        // Admission's own warning must not prevent a later eligible admission.
        // Genuine save/availability errors still pause writes.
        self.text.is_dirty()
            && self.status_with_error(self.last_error.clone()).auto_save() == AutoSaveControl::On
    }
}

/// What a freshness check decided to do about a document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RefreshOutcome {
    /// The source is unchanged; nothing happened.
    Unchanged,
    /// The buffer was clean, so it now holds what the source holds.
    Reloaded,
    /// The buffer had unsaved changes and the source changed too.
    Conflict,
    /// The source can no longer be used; the buffer is kept as it is.
    Unavailable(UnavailableReason),
    /// The source changed but could not be read, so nothing was decided.
    Unreadable,
}

/// Why a document could not be opened.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OpenFailure {
    Origin(OriginError),
    Load(LoadFailure),
}

impl OpenFailure {
    pub(crate) fn headline(&self) -> String {
        match self {
            Self::Origin(_) => "This path cannot be opened".to_owned(),
            Self::Load(failure) => failure.headline(),
        }
    }

    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Origin(_) => {
                "A document needs a file name; a bare folder or root path has none.".to_owned()
            }
            Self::Load(failure) => failure.detail(),
        }
    }
}

#[derive(Debug)]
struct RecoveryProtection {
    source: PathBuf,
    authority: RecoveryAuthority,
    breadcrumb: PathBuf,
    error: SaveError,
}

impl RecoveryProtection {
    fn matches(&self, path: &Path, authority: Option<&LocalSourceAuthority>) -> bool {
        self.source == path
            || self.authority.canonical_path() == path
            || authority.is_some_and(|authority| {
                self.authority.canonical_path() == authority.canonical_path()
                    || self.authority.matches(authority)
            })
    }
}

/// Every open document, owned by the application rather than by a window.
#[derive(Debug, Default)]
pub(crate) struct DocumentRegistry {
    documents: HashMap<DocumentId, OpenDocument>,
    by_key: HashMap<DocumentKey, DocumentId>,
    next_id: u64,
    next_untitled_by_prefix: HashMap<String, u64>,
    pending_recovery_notices: VecDeque<(PathBuf, SaveError)>,
    overflow_recovery_notices: VecDeque<(PathBuf, SaveError)>,
    recovery_notice_overflow_summary: Option<(usize, PathBuf, SaveError)>,
    active_recovery_notice: Option<(PathBuf, SaveError)>,
    recovery_protections: Vec<RecoveryProtection>,
    recovery_protection_overflow: Option<(PathBuf, SaveError)>,
    bounds: DocumentBounds,
    worker: Option<local_io::Worker>,
    worker_start_error: Option<SaveError>,
    write_gate: Arc<Mutex<()>>,
}

impl DocumentRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn shared() -> SharedDocuments {
        Rc::new(RefCell::new(Self::new()))
    }

    #[cfg(test)]
    pub(crate) fn with_bounds(bounds: DocumentBounds) -> Self {
        Self {
            bounds,
            ..Self::default()
        }
    }

    /// Opens a local file, or hands back the document that is already open for
    /// it. Aliasing is by origin, so two views of one path share one buffer,
    /// one undo history, and one dirty flag.
    pub(crate) fn open_local(&mut self, path: &Path) -> Result<DocumentId, OpenFailure> {
        self.drain_background();
        let origin = LocalOrigin::new(path)
            .map(DocumentOrigin::Local)
            .map_err(OpenFailure::Origin)?;
        if let Some(id) = self.current_keyed_local(&origin) {
            self.retain(id);
            return Ok(id);
        }

        let loaded = document_store::load(path, self.bounds).map_err(OpenFailure::Load)?;
        if let Some(existing) = self
            .documents
            .iter()
            .filter(|(_, document)| {
                document
                    .source_authority
                    .as_ref()
                    .is_some_and(|authority| authority.matches(&loaded.source_authority))
                    && document.generation.is_some_and(|generation| {
                        document_store::source_authority_is_current(
                            document.source_authority.as_ref().expect("checked above"),
                            generation,
                        )
                    })
            })
            .map(|(id, _)| *id)
            .min()
        {
            self.by_key.insert(origin.key(), existing);
            self.retain(existing);
            return Ok(existing);
        }
        Ok(self.insert(
            origin,
            loaded.document,
            Some(loaded.generation),
            Some(loaded.source_authority),
            loaded.read_only,
        ))
    }

    pub(crate) fn open_terminal_link(&mut self, path: &Path) -> Result<DocumentId, OpenFailure> {
        let origin = LocalOrigin::new(path)
            .map(DocumentOrigin::Local)
            .map_err(OpenFailure::Origin)?;
        let loaded =
            document_store::load_terminal_link(path, self.bounds).map_err(OpenFailure::Load)?;
        let existing = self
            .documents
            .iter()
            .filter(|(_, document)| {
                document.origin.key() == origin.key()
                    || document
                        .source_authority
                        .as_ref()
                        .is_some_and(|authority| authority.matches(&loaded.source_authority))
            })
            .map(|(id, _)| *id)
            .min();
        if let Some(id) = existing {
            let document = self.documents.get_mut(&id).expect("matched document");
            document.invalidate_background();
            if let Some(authority) = &mut document.source_authority {
                authority.restrict_terminal_links();
            }
            self.retain(id);
            return Ok(id);
        }
        Ok(self.insert(
            origin,
            loaded.document,
            Some(loaded.generation),
            Some(loaded.source_authority),
            loaded.read_only,
        ))
    }

    fn current_keyed_local(&mut self, origin: &DocumentOrigin) -> Option<DocumentId> {
        let key = origin.key();
        let id = self.by_key.get(&key).copied()?;
        let document = self.documents.get(&id)?;
        if document.origin.key() == key {
            return Some(id);
        }
        let authority = document.source_authority.clone();
        let generation = document.generation;
        let path = match origin {
            DocumentOrigin::Local(origin) => origin.path(),
            _ => return None,
        };
        let current = document_store::resolve_source_authority(path).ok();
        let valid = authority
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(authority, current)| authority.matches(current))
            && authority
                .zip(generation)
                .is_some_and(|(authority, generation)| {
                    document_store::source_authority_is_current(&authority, generation)
                });
        if valid {
            Some(id)
        } else {
            self.by_key.remove(&key);
            None
        }
    }

    /// Adds a document whose bytes were fetched by something other than the
    /// local filesystem, which is how remote origins arrive.
    pub(crate) fn adopt(
        &mut self,
        origin: DocumentOrigin,
        text: TextDocument,
        read_only: bool,
    ) -> DocumentId {
        if let Some(id) = self.by_key.get(&origin.key()).copied() {
            self.retain(id);
            return id;
        }
        self.insert(origin, text, None, None, read_only)
    }

    /// Replaces the bytes for an already-open remote document snapshot, or
    /// adopts it if this is the first view of that verified origin.
    pub(crate) fn adopt_remote_snapshot(
        &mut self,
        origin: DocumentOrigin,
        text: TextDocument,
        read_only: bool,
    ) -> DocumentId {
        if let Some(id) = self.by_key.get(&origin.key()).copied() {
            if let Some(document) = self.documents.get_mut(&id) {
                document.syntax = DocumentSyntax::new(origin.file_name(), text.text());
                document.origin = origin;
                document.text = text;
                document.read_only = read_only;
                document.availability = Availability::Available;
                document.last_error = None;
            }
            self.retain(id);
            return id;
        }
        self.insert(origin, text, None, None, read_only)
    }

    fn insert(
        &mut self,
        origin: DocumentOrigin,
        text: TextDocument,
        generation: Option<Generation>,
        source_authority: Option<LocalSourceAuthority>,
        read_only: bool,
    ) -> DocumentId {
        self.next_id += 1;
        let id = DocumentId::from_raw(self.next_id);
        let syntax = DocumentSyntax::new(origin.file_name(), text.text());
        self.by_key.insert(origin.key(), id);
        let recovery = match &origin {
            DocumentOrigin::Local(origin) => {
                self.recovery_protection(origin.path(), source_authority.as_ref())
            }
            _ => None,
        };
        self.documents.insert(
            id,
            OpenDocument {
                syntax,
                origin,
                text,
                generation,
                source_authority,
                views: 1,
                read_only,
                availability: Availability::Available,
                conflict: None,
                save: SaveProgress::Idle,
                auto_save_requested: false,
                last_error: None,
                recovery_error: recovery.as_ref().map(|(_, error)| error.clone()),
                recovery_path: recovery.as_ref().map(|(path, _)| path.clone()),
                recovery_notice_queued: recovery.is_some(),
                checked: Instant::now(),
                reloaded: None,
                settled: None,
                auto_save_blocked_at: None,
                io_epoch: 0,
                background: None,
                background_error: None,
                background_load_error: None,
            },
        );
        id
    }

    fn recovery_protection(
        &self,
        path: &Path,
        authority: Option<&LocalSourceAuthority>,
    ) -> Option<(PathBuf, SaveError)> {
        self.recovery_protections
            .iter()
            .find(|protection| protection.matches(path, authority))
            .map(|protection| (protection.breadcrumb.clone(), protection.error.clone()))
            .or_else(|| self.recovery_protection_overflow.clone())
    }

    fn retain_recovery_protection(
        &mut self,
        source: PathBuf,
        authority: RecoveryAuthority,
        breadcrumb: PathBuf,
        error: SaveError,
    ) {
        if self
            .recovery_protections
            .iter()
            .any(|protection| protection.breadcrumb == breadcrumb)
        {
            return;
        }
        if self.recovery_protections.len() < MAX_RECOVERY_PROTECTIONS {
            self.recovery_protections.push(RecoveryProtection {
                source,
                authority,
                breadcrumb: breadcrumb.clone(),
                error: error.clone(),
            });
        } else if self.recovery_protection_overflow.is_none() {
            self.recovery_protection_overflow = Some((
                breadcrumb.clone(),
                SaveError::new(
                    "Local recovery protection capacity exceeded",
                    format!("The 64 unresolved local source protections are occupied. All local writes are refused for this application lifetime; acknowledgement does not resolve recovery. Preserve unsaved text and manually recover retained versions, including {}, before restarting fesTerm.", breadcrumb.display()),
                ),
            ));
        }
        // Protection is application-lifetime state, not a notification or view.
        for document in self.documents.values_mut() {
            let DocumentOrigin::Local(origin) = &document.origin else {
                continue;
            };
            let recovery = self
                .recovery_protections
                .iter()
                .find(|protection| {
                    protection.matches(origin.path(), document.source_authority.as_ref())
                })
                .map(|protection| (protection.breadcrumb.clone(), protection.error.clone()))
                .or_else(|| self.recovery_protection_overflow.clone());
            if let Some((path, error)) = recovery {
                document.invalidate_background();
                document.recovery_path = Some(path);
                document.recovery_error = Some(error);
                document.recovery_notice_queued = true;
            }
        }
        self.queue_recovery_notice_record(breadcrumb, error);
    }

    /// Records one more view of an already-open document.
    pub(crate) fn retain(&mut self, id: DocumentId) {
        if let Some(document) = self.documents.get_mut(&id) {
            document.views += 1;
        }
    }

    /// Adds a new untitled document whose bytes are already in memory.
    ///
    /// The bounds grow to fit the snapshot being adopted so an export can open
    /// whatever bounded history the terminal retained without relaxing the
    /// ordinary file-on-disk editor limits for unrelated documents.
    pub(crate) fn create_untitled(
        &mut self,
        name_prefix: &str,
        qualified_label: &str,
        bytes: &[u8],
    ) -> DocumentId {
        let next = self
            .next_untitled_by_prefix
            .entry(name_prefix.to_owned())
            .and_modify(|next| *next += 1)
            .or_insert(1);
        let key = format!("{name_prefix}-{next}");
        let file_name = format!("{name_prefix}-{next}.txt");
        let origin = UntitledOrigin::new(key, file_name, qualified_label)
            .expect("generated untitled origins are valid");
        let text = TextDocument::from_unsaved_bytes(bytes, self.bounds)
            .expect("untitled snapshots are preflighted against editor bounds");
        self.insert(DocumentOrigin::from(origin), text, None, None, false)
    }

    /// Records one fewer view. The document is forgotten when the last view
    /// goes, which is also when a watcher would be released.
    ///
    /// Returns true when the document was dropped. Callers are responsible for
    /// having satisfied the dirty-close policy first: the registry does not
    /// second-guess a decision the user has already been asked to make.
    pub(crate) fn release(&mut self, id: DocumentId) -> bool {
        let Some(document) = self.documents.get_mut(&id) else {
            return false;
        };
        document.views = document.views.saturating_sub(1);
        if document.views > 0 {
            return false;
        }
        document.cancel_background();
        let recovery = (!document.recovery_notice_queued)
            .then(|| {
                document
                    .recovery_path
                    .clone()
                    .zip(document.recovery_error.clone())
            })
            .flatten();
        self.documents.remove(&id);
        self.by_key.retain(|_, document| *document != id);
        if let Some(recovery) = recovery {
            self.queue_recovery_notice_record(recovery.0, recovery.1);
        }
        true
    }

    pub(crate) fn take_recovery_notice(&mut self) -> Option<(PathBuf, SaveError)> {
        if self.active_recovery_notice.is_some() {
            return None;
        }
        let notice = self
            .pending_recovery_notices
            .pop_front()
            .or_else(|| self.overflow_recovery_notices.pop_front())
            .or_else(|| {
                self.recovery_notice_overflow_summary
                    .take()
                    .map(|(count, path, error)| {
                        (
                            path.clone(),
                            SaveError::new(
                                "Additional saves need manual recovery",
                                format!(
                                "{count} additional recovery notice(s) exceeded the visible queue. \
                                 The latest retained path is {}. {} Check the application logs for \
                                 every retained path.",
                                path.display(),
                                error.detail()
                            ),
                            ),
                        )
                    })
            });
        self.active_recovery_notice.clone_from(&notice);
        notice
    }

    pub(crate) fn acknowledge_recovery_notice(&mut self, path: &Path) -> bool {
        let Some((active, _)) = self.active_recovery_notice.as_ref() else {
            return false;
        };
        if active != path {
            tracing::warn!(
                active = %active.display(),
                acknowledged = %path.display(),
                "a recovery notice acknowledgement key did not match the active path"
            );
        }
        self.active_recovery_notice = None;
        true
    }

    pub(crate) fn has_recovery_notices(&self) -> bool {
        !self.pending_recovery_notices.is_empty()
            || !self.overflow_recovery_notices.is_empty()
            || self.recovery_notice_overflow_summary.is_some()
            || self.active_recovery_notice.is_some()
    }

    pub(crate) fn first_dirty_document(&self) -> Option<(DocumentId, String, String)> {
        self.documents.iter().find_map(|(id, document)| {
            document.text.is_dirty().then(|| {
                (
                    *id,
                    document.origin.file_name().to_owned(),
                    document.origin.qualified_label(),
                )
            })
        })
    }

    #[cfg(test)]
    pub(crate) fn queue_recovery_notice(&mut self, path: PathBuf, error: SaveError) {
        self.queue_recovery_notice_record(path, error);
    }

    fn queue_recovery_notice_record(&mut self, path: PathBuf, error: SaveError) {
        if self.pending_recovery_notices.len() < MAX_PENDING_RECOVERY_NOTICES {
            self.pending_recovery_notices.push_back((path, error));
            return;
        }
        if self.overflow_recovery_notices.len() < MAX_OVERFLOW_RECOVERY_NOTICES {
            self.overflow_recovery_notices.push_back((path, error));
            return;
        }
        tracing::error!(
            path = %path.display(),
            detail = %error.detail(),
            "a save recovery notice exceeded the visible queue; its path remains in the logs"
        );
        match &mut self.recovery_notice_overflow_summary {
            Some((count, latest_path, latest_error)) => {
                *count += 1;
                *latest_path = path;
                *latest_error = error;
            }
            summary @ None => *summary = Some((1, path, error)),
        }
    }

    pub(crate) fn get(&self, id: DocumentId) -> Option<&OpenDocument> {
        self.documents.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: DocumentId) -> Option<&mut OpenDocument> {
        self.documents.get_mut(&id)
    }

    pub(crate) fn is_open(&self, id: DocumentId) -> bool {
        self.documents.contains_key(&id)
    }

    /// The document already open for a path, if any. This is what lets Open
    /// bind a second view to an existing buffer instead of reading the file
    /// again.
    pub(crate) fn find_local(&self, path: &Path) -> Option<DocumentId> {
        let origin = LocalOrigin::new(path).ok().map(DocumentOrigin::Local)?;
        self.by_key.get(&origin.key()).copied()
    }

    pub(crate) fn find_current_local(&mut self, path: &Path) -> Option<DocumentId> {
        let origin = LocalOrigin::new(path).ok().map(DocumentOrigin::Local)?;
        self.current_keyed_local(&origin)
    }

    /// Every open document, for callers that must act on all of them.
    pub(crate) fn open_ids(&self) -> impl Iterator<Item = DocumentId> + '_ {
        self.documents.keys().copied()
    }

    pub(crate) fn len(&self) -> usize {
        self.documents.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }

    pub(crate) const fn bounds(&self) -> DocumentBounds {
        self.bounds
    }

    /// Writes a local document back to its origin, revalidating first.
    ///
    /// A conflict is not an error the user has to interpret: the source's text
    /// is captured with it so Compare can open immediately and Reload can
    /// apply exactly the bytes the conflict was raised for.
    pub(crate) fn save(&mut self, id: DocumentId) -> Option<SaveOutcome> {
        let document = self.documents.get_mut(&id)?;
        match &document.origin {
            DocumentOrigin::Remote(_) => return None,
            DocumentOrigin::Untitled(_) => {
                let error = SaveError::new(
                    "Choose a destination first",
                    "This document is not on disk yet. Use Save As to choose where to write it.",
                );
                document.last_error = Some(error.clone());
                return Some(SaveOutcome::Failed(error));
            }
            DocumentOrigin::Local(_) => {}
        }
        self.drain_background();
        let gate = Arc::clone(&self.write_gate);
        for document in self.documents.values_mut() {
            document.cancel_background();
        }
        let _guard = match gate.try_lock() {
            Ok(guard) => guard,
            Err(error) => {
                return self
                    .save_admission_error(id, matches!(error, TryLockError::Poisoned(_)))
                    .map(SaveOutcome::Failed)
            }
        };
        self.drain_background();
        for document in self.documents.values_mut() {
            document.invalidate_background();
        }
        let protection = self.documents.get(&id).and_then(|document| {
            let DocumentOrigin::Local(origin) = &document.origin else {
                return None;
            };
            self.recovery_protection(origin.path(), document.source_authority.as_ref())
        });
        if let Some((path, error)) = protection {
            let document = self.documents.get_mut(&id)?;
            document.recovery_path = Some(path);
            document.recovery_error = Some(error.clone());
            document.recovery_notice_queued = true;
            return Some(SaveOutcome::Failed(error));
        }
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        if let Some(error) = document.recovery_error.clone() {
            return Some(SaveOutcome::Failed(error));
        }
        let DocumentOrigin::Local(origin) = &document.origin else {
            unreachable!("local save preflight");
        };
        let path = origin.path().to_path_buf();
        let bytes = document.text.to_bytes();

        let expectation = match (document.generation, document.source_authority.as_ref()) {
            (Some(generation), Some(authority)) => SaveExpectation::Loaded {
                generation,
                authority,
            },
            _ => {
                let error = SaveError::new(
                    "Saving did not complete",
                    "The document no longer has exact authority for its source. Use Save As… to choose a destination.",
                );
                document.last_error = Some(error.clone());
                return Some(SaveOutcome::Failed(error));
            }
        };
        let outcome = match document_store::save(&path, &bytes, expectation) {
            Ok(saved) => {
                document.generation = Some(saved.generation);
                document.background_load_error = None;
                document.source_authority = Some(saved.source_authority);
                document.read_only = saved.read_only;
                document.text.mark_saved();
                document.conflict = None;
                document.last_error = None;
                document.availability = Availability::Available;
                SaveOutcome::Saved
            }
            Err(SaveFailure::Conflict(_)) => {
                let conflict = conflict_for(
                    &path,
                    document
                        .source_authority
                        .as_ref()
                        .expect("loaded local source"),
                    bounds,
                );
                document.conflict = Some(conflict.clone());
                SaveOutcome::Conflict(conflict)
            }
            Err(SaveFailure::Gone) => {
                document.availability = Availability::Unavailable(UnavailableReason::Missing);
                SaveOutcome::Unavailable(UnavailableReason::Missing)
            }
            Err(failure @ SaveFailure::PermissionDenied) => {
                document.availability =
                    Availability::Unavailable(UnavailableReason::PermissionDenied);
                let _ = failure;
                SaveOutcome::Unavailable(UnavailableReason::PermissionDenied)
            }
            Err(failure @ SaveFailure::RecoveryRequired(_)) => {
                let error = SaveError::new(failure.headline(), failure.detail());
                if let SaveFailure::RecoveryRequired(path) = &failure {
                    document.recovery_path = Some(path.clone());
                    document.recovery_notice_queued = true;
                }
                document.recovery_error = Some(error.clone());
                SaveOutcome::Failed(error)
            }
            Err(failure) => {
                let error = SaveError::new(failure.headline(), failure.detail());
                document.last_error = Some(error.clone());
                SaveOutcome::Failed(error)
            }
        };
        document.save = SaveProgress::Idle;
        let recovery = if document.recovery_notice_queued {
            document
                .recovery_path
                .clone()
                .zip(document.recovery_error.clone())
        } else {
            None
        };
        if let Some((path, error)) = recovery {
            let authority = document
                .source_authority
                .clone()
                .expect("loaded save authority");
            let DocumentOrigin::Local(origin) = &document.origin else {
                unreachable!()
            };
            let source = origin.path().to_path_buf();
            self.retain_recovery_protection(source, authority.recovery_authority(), path, error);
        }
        Some(outcome)
    }

    /// Writes a document's text to a new local destination and hands back the
    /// document identity the saving view should follow.
    ///
    /// The new location is a different document, not a rename: other views of
    /// the original are still looking at the original file and must not be
    /// moved out from under themselves (ADR 0034 §3). The caller is
    /// responsible for releasing the view's old document once it has rebound.
    ///
    /// If the destination is already open, that document is what the view
    /// binds to. Two buffers for one file is precisely what the registry
    /// exists to prevent (§1), so the open document adopts the saving buffer
    /// and its undo history rather than a second copy being made.
    pub(crate) fn save_as(
        &mut self,
        id: DocumentId,
        path: &Path,
        confirmed: &ConfirmedDestination,
    ) -> Option<(SaveOutcome, Option<DocumentId>)> {
        self.drain_background();
        let gate = Arc::clone(&self.write_gate);
        for document in self.documents.values_mut() {
            document.cancel_background();
        }
        let _guard = match gate.try_lock() {
            Ok(guard) => guard,
            Err(error) => {
                return self
                    .save_admission_error(id, matches!(error, TryLockError::Poisoned(_)))
                    .map(|error| (SaveOutcome::Failed(error), None))
            }
        };
        self.drain_background();
        for document in self.documents.values_mut() {
            document.invalidate_background();
        }
        let protected = self
            .recovery_protections
            .iter()
            .find(|protection| {
                protection.source == path || protection.authority.matches_destination(confirmed)
            })
            .map(|protection| protection.error.clone())
            .or_else(|| {
                self.recovery_protection_overflow
                    .as_ref()
                    .map(|(_, error)| error.clone())
            });
        if let Some(error) = protected {
            self.documents.get_mut(&id)?.last_error = Some(error.clone());
            return Some((SaveOutcome::Failed(error), None));
        }
        if !confirmed.matches_requested_path(path) {
            let error = SaveError::new(
                "That destination changed after confirmation",
                "Review the destination and confirm Save As again. Nothing was written.",
            );
            if let Some(source) = self.documents.get_mut(&id) {
                source.last_error = Some(error.clone());
            }
            return Some((SaveOutcome::Failed(error), None));
        }
        let source_is_exact_destination = self.documents.get(&id).is_some_and(|source| {
            matches!(
                source.origin(),
                DocumentOrigin::Local(local)
                    if confirmed.matches_requested_path(local.path())
            )
        });
        if let Some(source) = self.documents.get(&id) {
            if let Some(error) = source.recovery_error.clone() {
                if source_is_exact_destination
                    || source
                        .source_authority
                        .as_ref()
                        .is_some_and(|authority| confirmed.matches_source_authority(authority))
                {
                    return Some((SaveOutcome::Failed(error), None));
                }
            }
        }
        let origin = match LocalOrigin::new(path).map(DocumentOrigin::Local) {
            Ok(origin) => origin,
            Err(_) => {
                let error = SaveError::new(
                    "That destination cannot be used",
                    "The chosen path is not a file this host can write to.",
                );
                if let Some(source) = self.documents.get_mut(&id) {
                    source.last_error = Some(error.clone());
                }
                return Some((SaveOutcome::Failed(error), None));
            }
        };
        let keyed = self
            .current_keyed_local(&origin)
            .filter(|candidate| *candidate != id || source_is_exact_destination);
        let mut matching = Vec::new();
        if let Some(keyed) = keyed {
            matching.push(keyed);
        }
        for (candidate, document) in &self.documents {
            if *candidate == id && !source_is_exact_destination {
                continue;
            }
            let authority_matches = document
                .source_authority
                .as_ref()
                .zip(document.generation)
                .is_some_and(|(authority, generation)| {
                    confirmed.matches_source_authority(authority)
                        && document_store::source_authority_is_current(authority, generation)
                });
            let origin_matches = matches!(
                document.origin(),
                DocumentOrigin::Local(local)
                    if confirmed.matches_requested_path(local.path())
            );
            if (authority_matches || origin_matches) && !matching.contains(candidate) {
                matching.push(*candidate);
            }
        }
        matching.sort_unstable();
        if let Some(error) = matching
            .iter()
            .filter(|candidate| **candidate != id)
            .filter_map(|candidate| self.documents.get(candidate))
            .find_map(|destination| destination.recovery_error.clone())
        {
            if let Some(source) = self.documents.get_mut(&id) {
                source.last_error = Some(error.clone());
            }
            return Some((SaveOutcome::Failed(error), None));
        }
        if matching
            .iter()
            .filter(|candidate| **candidate != id)
            .any(|candidate| {
                self.documents.get(candidate).is_some_and(|destination| {
                    destination.text.is_dirty() || destination.conflict.is_some()
                })
            })
        {
            let error = SaveError::new(
                "That destination has unsaved work",
                "Switch to the already-open destination, save or resolve its changes, then try Save As again. Nothing was written.",
            );
            if let Some(source) = self.documents.get_mut(&id) {
                source.last_error = Some(error.clone());
            }
            return Some((SaveOutcome::Failed(error), None));
        }
        let existing = keyed.or_else(|| matching.first().copied());
        if let Some(existing) = existing {
            let Some(destination) = self.documents.get(&existing) else {
                let error = SaveError::new(
                    "That destination cannot be verified",
                    "Refresh the open documents and try Save As again. Nothing was written.",
                );
                if let Some(source) = self.documents.get_mut(&id) {
                    source.last_error = Some(error.clone());
                }
                return Some((SaveOutcome::Failed(error), None));
            };
            let expectation_matches = match confirmed.expectation() {
                DestinationExpectation::Existing(expected) => {
                    destination.generation == Some(expected)
                }
                DestinationExpectation::Absent => matches!(
                    destination.availability,
                    Availability::Unavailable(UnavailableReason::Missing)
                ),
            };
            if !expectation_matches && destination.generation.is_none() {
                let error = SaveError::new(
                    "That destination cannot be verified",
                    "Refresh or reopen the already-open destination, then try Save As again. Nothing was written.",
                );
                if let Some(source) = self.documents.get_mut(&id) {
                    source.last_error = Some(error.clone());
                }
                return Some((SaveOutcome::Failed(error), None));
            }
            if !expectation_matches {
                let error = SaveError::new(
                    "That destination changed after confirmation",
                    "Review the already-open destination and confirm Save As again. Nothing was written.",
                );
                if let Some(source) = self.documents.get_mut(&id) {
                    source.last_error = Some(error.clone());
                }
                return Some((SaveOutcome::Failed(error), None));
            }
        }
        let Some(document) = self.documents.get(&id) else {
            let error = SaveError::new(
                "This document cannot be verified",
                "Refresh the open documents and try Save As again. Nothing was written.",
            );
            return Some((SaveOutcome::Failed(error), None));
        };
        let bytes = document.text.to_bytes();
        let text = document.text.clone();

        let saved =
            match document_store::save(path, &bytes, SaveExpectation::Destination(confirmed)) {
                Ok(saved) => saved,
                Err(failure @ SaveFailure::RecoveryRequired(_)) => {
                    let error = SaveError::new(failure.headline(), failure.detail());
                    if let Some(document) = self.documents.get_mut(&id) {
                        if let SaveFailure::RecoveryRequired(path) = &failure {
                            document.recovery_path = Some(path.clone());
                            document.recovery_notice_queued = true;
                        }
                        document.recovery_error = Some(error.clone());
                    }
                    if let SaveFailure::RecoveryRequired(path) = failure {
                        let DocumentOrigin::Local(origin) = &origin else {
                            unreachable!()
                        };
                        self.retain_recovery_protection(
                            origin.path().to_path_buf(),
                            confirmed.recovery_authority(),
                            path,
                            error.clone(),
                        );
                    }
                    return Some((SaveOutcome::Failed(error), None));
                }
                Err(failure) => {
                    let error = SaveError::new(failure.headline(), failure.detail());
                    if let Some(document) = self.documents.get_mut(&id) {
                        document.last_error = Some(error.clone());
                    }
                    return Some((SaveOutcome::Failed(error), None));
                }
            };

        if let Some(existing) = existing {
            // The file the user chose is open elsewhere, and it now holds the
            // bytes just written, so that document is brought up to date
            // from the exact save result rather than reopening a pathname that
            // may already name a different parent.
            let mut reloaded = text;
            reloaded.mark_saved();
            self.by_key.retain(|_, document| *document != existing);
            self.by_key.insert(origin.key(), existing);
            if let Some(document) = self.documents.get_mut(&existing) {
                document.syntax = DocumentSyntax::new(origin.file_name(), reloaded.text());
                document.origin = origin;
                document.text = reloaded;
                document.background_load_error = None;
                document.generation = Some(saved.generation);
                document.source_authority = Some(saved.source_authority);
                document.read_only = saved.read_only;
                document.availability = Availability::Available;
                document.conflict = None;
                document.last_error = None;
                document.recovery_error = None;
                document.recovery_path = None;
                document.recovery_notice_queued = false;
                document.save = SaveProgress::Idle;
                document.reloaded = Some(Instant::now());
                document.checked = Instant::now();
                document.settled = None;
                document.auto_save_blocked_at = None;
            }
            if existing != id {
                self.retain(existing);
            }
            return Some((SaveOutcome::Saved, Some(existing)));
        }

        let mut text = text;
        text.mark_saved();
        let new_id = self.insert(
            origin,
            text,
            Some(saved.generation),
            Some(saved.source_authority),
            saved.read_only,
        );
        Some((SaveOutcome::Saved, Some(new_id)))
    }

    /// Legacy explicit-save transition oracle, not the production scheduler.
    /// Real-worker scheduling is covered by the `background_*` regressions.
    ///
    /// Writes every document whose Auto-save is on and whose typing has
    /// settled, and returns what happened to each one that was attempted.
    ///
    /// This is the whole of Auto-save's scheduling: one debounce per document,
    /// coalesced by construction because a write is only considered once the
    /// content has stopped changing. Failures are not retried on the clock --
    /// the document stays dirty, keeps its error, and is only reconsidered
    /// once the user edits again, which is the only new information there is
    /// (ADR 0034 §7).
    #[cfg(test)]
    pub(crate) fn auto_save(&mut self, now: Instant) -> Vec<(DocumentId, SaveOutcome)> {
        let mut due = Vec::new();
        for (id, document) in &mut self.documents {
            let revision = document.text.revision();
            let changed = document.settled.is_none_or(|(seen, _)| seen != revision);
            if changed {
                document.settled = Some((revision, now));
                // New content is new information, so an earlier failure stops
                // standing in the way.
                if document
                    .auto_save_blocked_at
                    .is_some_and(|at| at != revision)
                {
                    document.auto_save_blocked_at = None;
                }
                continue;
            }
            if document.auto_save_blocked_at == Some(revision) {
                continue;
            }
            if !document.auto_save_should_write() || document.save != SaveProgress::Idle {
                continue;
            }
            let Some((_, since)) = document.settled else {
                continue;
            };
            if now.duration_since(since) >= AUTO_SAVE_IDLE {
                due.push(*id);
            }
        }

        let mut outcomes = Vec::new();
        for id in due {
            let Some(outcome) = self.save(id) else {
                continue;
            };
            if let Some(document) = self.documents.get_mut(&id) {
                if outcome != SaveOutcome::Saved {
                    document.auto_save_blocked_at = Some(document.text.revision());
                }
            }
            outcomes.push((id, outcome));
        }
        outcomes
    }

    /// Legacy explicit-refresh transition oracle, not the production scheduler.
    ///
    /// Re-checks every local document whose turn has come round, which is how
    /// an outside change is noticed without the user pressing anything.
    ///
    /// fesTerm does not install a filesystem watcher for this. ADR 0034 §6
    /// makes watcher events *hints* that must be re-stat'ed anyway — atomic
    /// saves arrive as create/rename/replace storms, and watch services drop
    /// events under load — so a watcher would buy earlier notice at the cost
    /// of a platform-specific dependency and three sets of edge cases, while
    /// the check it triggers is exactly the one below. A bounded poll of the
    /// handful of files that are actually open behaves identically on every
    /// platform, and Refresh remains there for anyone who will not wait.
    #[cfg(test)]
    pub(crate) fn poll(&mut self, now: Instant) -> Vec<(DocumentId, RefreshOutcome)> {
        let due: Vec<DocumentId> = self
            .documents
            .iter()
            .filter(|(_, document)| {
                matches!(document.origin, DocumentOrigin::Local(_))
                    && document.save == SaveProgress::Idle
                    // A conflict is the user's to resolve; re-checking would
                    // only replace their banner with the same banner.
                    && document.conflict.is_none()
                    && document.recovery_error.is_none()
                    && now.duration_since(document.checked) >= POLL_INTERVAL
            })
            .map(|(id, _)| *id)
            .collect();

        let mut changed = Vec::new();
        for id in due {
            if let Some(outcome) = self.refresh(id) {
                if outcome != RefreshOutcome::Unchanged {
                    changed.push((id, outcome));
                }
            }
        }
        changed
    }

    /// Legacy focus/explicit-refresh oracle, not the production scheduler.
    ///
    /// Re-checks everything now, whatever the poll clock says. Used when a
    /// window regains focus: the user has just come back from whatever
    /// changed the file.
    #[cfg(test)]
    pub(crate) fn revalidate_all(&mut self) -> Vec<(DocumentId, RefreshOutcome)> {
        let ids: Vec<DocumentId> = self
            .documents
            .iter()
            .filter(|(_, document)| {
                document.save == SaveProgress::Idle
                    && document.conflict.is_none()
                    && document.recovery_error.is_none()
            })
            .map(|(id, _)| *id)
            .collect();
        let mut changed = Vec::new();
        for id in ids {
            if let Some(outcome) = self.refresh(id) {
                if outcome != RefreshOutcome::Unchanged {
                    changed.push((id, outcome));
                }
            }
        }
        changed
    }

    /// Re-checks a local document against its source and applies ADR 0034 §6's
    /// table: a clean buffer follows the file, a dirty one never does silently.
    pub(crate) fn refresh(&mut self, id: DocumentId) -> Option<RefreshOutcome> {
        if !matches!(self.documents.get(&id)?.origin, DocumentOrigin::Local(_)) {
            return None;
        }
        self.drain_background();
        let gate = Arc::clone(&self.write_gate);
        let _guard = match gate.try_lock() {
            Ok(guard) => guard,
            Err(error) => {
                self.save_admission_error(id, matches!(error, TryLockError::Poisoned(_)))?;
                return Some(RefreshOutcome::Unreadable);
            }
        };
        self.drain_background();
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        document.invalidate_background();
        let DocumentOrigin::Local(origin) = &document.origin else {
            return None;
        };
        let path = origin.path().to_path_buf();
        let Some(known) = document.generation else {
            return Some(RefreshOutcome::Unchanged);
        };

        document.checked = Instant::now();
        let authority = document
            .source_authority
            .as_ref()
            .expect("loaded local source");
        let outcome = match document_store::freshness_for_source(&path, authority, known) {
            Freshness::Unchanged => {
                let authority_current =
                    document.source_authority.as_ref().is_some_and(|authority| {
                        document_store::source_authority_identity_is_current(authority, known)
                    });
                if authority_current {
                    document.background_load_error = None;
                    document.availability = Availability::Available;
                    RefreshOutcome::Unchanged
                } else {
                    let newly_unavailable =
                        matches!(document.availability, Availability::Available);
                    document.availability = Availability::Unavailable(UnavailableReason::Missing);
                    if newly_unavailable {
                        RefreshOutcome::Unavailable(UnavailableReason::Missing)
                    } else {
                        RefreshOutcome::Unchanged
                    }
                }
            }
            Freshness::Changed(_) => match document_store::load_source(&path, authority, bounds) {
                Ok(loaded) if !document.text.is_dirty() => {
                    document.background_load_error = None;
                    document.text = loaded.document;
                    document.generation = Some(loaded.generation);
                    document.source_authority = Some(loaded.source_authority);
                    document.read_only = loaded.read_only;
                    document.availability = Availability::Available;
                    document.conflict = None;
                    document.reloaded = Some(Instant::now());
                    RefreshOutcome::Reloaded
                }
                Ok(loaded) => {
                    document.background_load_error = None;
                    document.conflict = Some(
                        festerm_document::ConflictState::new(
                            "This file changed somewhere else while you were editing it.",
                        )
                        .with_source_text(loaded.document.text()),
                    );
                    RefreshOutcome::Conflict
                }
                Err(LoadFailure::NotFound) => {
                    document.availability = Availability::Unavailable(UnavailableReason::Missing);
                    RefreshOutcome::Unavailable(UnavailableReason::Missing)
                }
                Err(LoadFailure::PermissionDenied) => {
                    document.availability =
                        Availability::Unavailable(UnavailableReason::PermissionDenied);
                    RefreshOutcome::Unavailable(UnavailableReason::PermissionDenied)
                }
                Err(LoadFailure::NotAFile | LoadFailure::Refused(_)) => {
                    document.availability = Availability::Unavailable(UnavailableReason::NotAFile);
                    RefreshOutcome::Unavailable(UnavailableReason::NotAFile)
                }
                Err(LoadFailure::Unreadable) => RefreshOutcome::Unreadable,
            },
            Freshness::Gone(LoadFailure::PermissionDenied) => {
                document.availability =
                    Availability::Unavailable(UnavailableReason::PermissionDenied);
                RefreshOutcome::Unavailable(UnavailableReason::PermissionDenied)
            }
            Freshness::Gone(LoadFailure::NotAFile) => {
                document.availability = Availability::Unavailable(UnavailableReason::NotAFile);
                RefreshOutcome::Unavailable(UnavailableReason::NotAFile)
            }
            Freshness::Gone(_) => {
                document.availability = Availability::Unavailable(UnavailableReason::Missing);
                RefreshOutcome::Unavailable(UnavailableReason::Missing)
            }
        };
        Some(outcome)
    }

    /// Replaces the buffer with what the source holds, discarding unsaved
    /// changes. Only ever reached through an explicit Reload the user pressed
    /// on a conflict banner.
    pub(crate) fn reload_from_source(&mut self, id: DocumentId) -> Option<RefreshOutcome> {
        if !matches!(self.documents.get(&id)?.origin, DocumentOrigin::Local(_)) {
            return None;
        }
        self.drain_background();
        let gate = Arc::clone(&self.write_gate);
        let _guard = match gate.try_lock() {
            Ok(guard) => guard,
            Err(error) => {
                self.save_admission_error(id, matches!(error, TryLockError::Poisoned(_)))?;
                return Some(RefreshOutcome::Unreadable);
            }
        };
        self.drain_background();
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        document.invalidate_background();
        let DocumentOrigin::Local(origin) = &document.origin else {
            return None;
        };
        let path = origin.path().to_path_buf();
        match document_store::load_source(
            &path,
            document
                .source_authority
                .as_ref()
                .expect("loaded local source"),
            bounds,
        ) {
            Ok(loaded) => {
                document.text = loaded.document;
                document.background_load_error = None;
                document.generation = Some(loaded.generation);
                document.source_authority = Some(loaded.source_authority);
                document.read_only = loaded.read_only;
                document.availability = Availability::Available;
                document.conflict = None;
                document.last_error = None;
                Some(RefreshOutcome::Reloaded)
            }
            Err(LoadFailure::NotFound) => {
                document.availability = Availability::Unavailable(UnavailableReason::Missing);
                Some(RefreshOutcome::Unavailable(UnavailableReason::Missing))
            }
            Err(_) => Some(RefreshOutcome::Unreadable),
        }
    }

    fn save_admission_error(&mut self, id: DocumentId, poisoned: bool) -> Option<SaveError> {
        let error = if poisoned {
            SaveError::new(
                "Local save coordination stopped",
                "An unexpected local write failure stopped save coordination. Nothing new was written by this command. Preserve unsaved text and any recovery directory, then restart fesTerm before saving again.",
            )
        } else {
            SaveError::new(
                "A local save is still finishing",
                "Nothing new was written by this command. Wait for the background save to finish, then try again.",
            )
        };
        self.documents.get_mut(&id)?.last_error = Some(error.clone());
        Some(error)
    }

    #[cfg(test)]
    pub(crate) fn hold_next_background_io_for_test(
        &mut self,
        context: &egui::Context,
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
        completed: std::sync::mpsc::SyncSender<()>,
    ) {
        let context = context.clone();
        let release = Mutex::new(release);
        let first = std::sync::atomic::AtomicBool::new(true);
        self.worker = Some(
            local_io::Worker::start_with(
                Arc::new(move || {
                    context.request_repaint_of(egui::ViewportId::ROOT);
                    let _ = completed.try_send(());
                }),
                Arc::clone(&self.write_gate),
                Arc::new(move |request, save| {
                    if first.swap(false, Ordering::AcqRel) {
                        entered.send(()).unwrap();
                        release
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(10))
                            .unwrap();
                    }
                    local_io::execute(request, save)
                }),
            )
            .unwrap(),
        );
    }

    /// Reconciles immutable worker registrations, not files. The worker owns
    /// its poll/debounce clock and wakes the UI only for observable results.
    pub(crate) fn background_tick(
        &mut self,
        context: &egui::Context,
        now: Instant,
        regained_focus: bool,
    ) -> bool {
        let mut changed = self.drain_background();
        if self.worker.is_none()
            && self.worker_start_error.is_none()
            && self.documents.values().any(Self::background_eligible)
        {
            let context = context.clone();
            match local_io::Worker::start(
                Arc::new(move || context.request_repaint_of(egui::ViewportId::ROOT)),
                Arc::clone(&self.write_gate),
            ) {
                Ok(worker) => self.worker = Some(worker),
                Err(error) => self.worker_start_error = Some(error),
            }
        }
        for (id, document) in &mut self.documents {
            if !Self::background_eligible(document) {
                document.cancel_background();
                document.background = None;
                continue;
            }
            let revision = document.text.revision();
            if document.settled.is_none_or(|(seen, _)| seen != revision) {
                document.settled = Some((revision, now));
            }
            if document
                .auto_save_blocked_at
                .is_some_and(|blocked| blocked != revision)
            {
                document.auto_save_blocked_at = None;
                document.last_error = None;
            }
            let auto_save = document.auto_save_should_write()
                && document.background_load_error.is_none()
                && document.auto_save_blocked_at != Some(revision);
            if document.background.as_ref().is_some_and(|registration| {
                registration.revision == revision
                    && registration.auto_save == auto_save
                    && !registration.control.cancelled.load(Ordering::Acquire)
            }) {
                if regained_focus {
                    document
                        .background
                        .as_ref()
                        .expect("checked above")
                        .control
                        .force
                        .store(true, Ordering::Release);
                }
                continue;
            }
            document.cancel_background();
            document.background = None;
            let result = if let Some(worker) = &self.worker {
                let DocumentOrigin::Local(origin) = &document.origin else {
                    unreachable!()
                };
                let control = Arc::new(local_io::Control::default());
                control.force.store(regained_focus, Ordering::Release);
                worker
                    .register(|| {
                        let mut request = local_io::Request::new(
                            *id,
                            document.io_epoch,
                            revision,
                            document.generation.expect("eligible generation"),
                            document
                                .source_authority
                                .clone()
                                .expect("eligible authority"),
                            origin.path().to_path_buf(),
                            document.availability.clone(),
                            document.background_load_error.clone(),
                            auto_save.then(|| document.text.to_bytes()),
                            if regained_focus {
                                now
                            } else {
                                now + POLL_INTERVAL
                            },
                            auto_save.then(|| {
                                document.settled.expect("settled above").1 + AUTO_SAVE_IDLE
                            }),
                            self.bounds,
                            Arc::clone(&control),
                        );
                        request.save_point = auto_save.then(|| document.text.save_point());
                        request
                    })
                    .map(|()| {
                        document.background = Some(Registration {
                            revision,
                            auto_save,
                            control,
                        });
                    })
            } else {
                Err(self
                    .worker_start_error
                    .clone()
                    .expect("worker start attempted"))
            };
            let error = result.err();
            changed |= document.background_error != error;
            document.background_error = error;
        }
        if regained_focus {
            if let Some(worker) = &self.worker {
                worker.force();
            }
        }
        changed
    }

    fn background_eligible(document: &OpenDocument) -> bool {
        matches!(document.origin, DocumentOrigin::Local(_))
            && document.generation.is_some()
            && document.source_authority.is_some()
            && document.save == SaveProgress::Idle
            && document.conflict.is_none()
            && document.recovery_error.is_none()
    }

    /// Only a short-lived notice needs a UI timer. Filesystem and debounce
    /// deadlines belong to the worker, including when every view is idle.
    pub(crate) fn notice_repaint_after(&self, now: Instant) -> Option<Duration> {
        self.documents
            .values()
            .filter_map(|document| {
                document
                    .reloaded
                    .map(|at| at + RELOAD_NOTICE)
                    .filter(|deadline| *deadline > now)
                    .map(|deadline| deadline.duration_since(now))
            })
            .min()
    }

    fn drain_background(&mut self) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        let completions = match worker.drain() {
            Ok(completions) => completions,
            Err(error) => {
                let mut changed = false;
                for document in self
                    .documents
                    .values_mut()
                    .filter(|doc| Self::background_eligible(doc))
                {
                    document.cancel_background();
                    document.background = None;
                    changed |= document.background_error.as_ref() != Some(&error);
                    document.background_error = Some(error.clone());
                }
                self.worker_start_error = Some(error);
                self.worker = None;
                return changed;
            }
        };
        let mut changed = false;
        for completion in completions {
            changed |= self.apply_background(completion);
        }
        changed
    }

    fn apply_background(&mut self, completion: local_io::Completion) -> bool {
        use local_io::{Check, Result};
        let local_io::Completion { request, result } = completion;
        // Recovery breadcrumbs survive closing, cancelling, or rebinding the
        // view; an ambiguous publication is never an ignorable stale result.
        let recovery = if let Result::Saved(Err(SaveFailure::RecoveryRequired(path)), _) = &result {
            let failure = SaveFailure::RecoveryRequired(path.clone());
            let error = SaveError::new(failure.headline(), failure.detail());
            self.retain_recovery_protection(
                request.path.clone(),
                request.authority.recovery_authority(),
                path.clone(),
                error,
            );
            true
        } else {
            false
        };
        let Some(document) = self.documents.get_mut(&request.id) else {
            return recovery;
        };
        if document.io_epoch != request.epoch
            || document.generation != Some(request.generation)
            || !matches!(&document.origin, DocumentOrigin::Local(origin) if origin.path() == request.path)
        {
            return recovery;
        }
        if document
            .background
            .as_ref()
            .is_some_and(|registration| Arc::ptr_eq(&registration.control, &request.control))
        {
            document.background = None;
        }
        document.checked = Instant::now();
        match result {
            Result::Checked(_) if request.control.cancelled.load(Ordering::Acquire) => {
                return false
            }
            Result::Checked(Check::Current(current)) => {
                document.background_load_error = None;
                document.availability = if current {
                    Availability::Available
                } else {
                    Availability::Unavailable(UnavailableReason::Missing)
                };
            }
            Result::Checked(Check::Loaded(loaded))
                if !document.text.is_dirty() && document.text.revision() == request.revision =>
            {
                document.invalidate_background();
                document.text = loaded.document;
                document.background_load_error = None;
                document.generation = Some(loaded.generation);
                document.source_authority = Some(loaded.source_authority);
                document.read_only = loaded.read_only;
                document.availability = Availability::Available;
                document.conflict = None;
                document.reloaded = Some(Instant::now());
            }
            Result::Checked(Check::Loaded(loaded)) => {
                document.background_load_error = None;
                document.conflict = Some(
                    festerm_document::ConflictState::new(
                        "This file changed somewhere else while you were editing it.",
                    )
                    .with_source_text(loaded.document.text()),
                );
            }
            Result::Checked(Check::Failed(error)) => {
                let reason = match error {
                    LoadFailure::NotFound => UnavailableReason::Missing,
                    LoadFailure::PermissionDenied => UnavailableReason::PermissionDenied,
                    LoadFailure::NotAFile | LoadFailure::Refused(_) => UnavailableReason::NotAFile,
                    LoadFailure::Unreadable => {
                        document.background_load_error = Some(error);
                        return true;
                    }
                };
                document.availability = Availability::Unavailable(reason);
            }
            Result::Saved(Ok(saved), _) => {
                document.invalidate_background();
                document.generation = Some(saved.generation);
                document.background_load_error = None;
                document.source_authority = Some(saved.source_authority);
                document.read_only = saved.read_only;
                document
                    .text
                    .mark_saved_at(request.save_point.expect("save request history point"));
                document.conflict = None;
                document.last_error = None;
                document.availability = Availability::Available;
            }
            Result::Saved(Err(SaveFailure::Interrupted), _)
                if request.control.cancelled.load(Ordering::Acquire) =>
            {
                return false
            }
            Result::Saved(Err(failure), conflict) => {
                document.auto_save_blocked_at = Some(request.revision);
                match failure {
                    SaveFailure::Conflict(_) => {
                        let mut state = festerm_document::ConflictState::new(
                            "This file changed somewhere else while you were editing it.",
                        );
                        if let Some(loaded) = conflict {
                            state = state.with_source_text(loaded.document.text());
                        }
                        document.conflict = Some(state);
                    }
                    SaveFailure::Gone => {
                        document.availability =
                            Availability::Unavailable(UnavailableReason::Missing)
                    }
                    SaveFailure::PermissionDenied => {
                        document.availability =
                            Availability::Unavailable(UnavailableReason::PermissionDenied)
                    }
                    SaveFailure::RecoveryRequired(ref path) => {
                        document.recovery_path = Some(path.clone());
                        document.recovery_notice_queued = true;
                        document.recovery_error =
                            Some(SaveError::new(failure.headline(), failure.detail()));
                    }
                    _ => {
                        document.last_error =
                            Some(SaveError::new(failure.headline(), failure.detail()))
                    }
                }
            }
        }
        true
    }
}

fn conflict_for(
    path: &Path,
    authority: &LocalSourceAuthority,
    bounds: DocumentBounds,
) -> festerm_document::ConflictState {
    let conflict = festerm_document::ConflictState::new(
        "This file changed somewhere else while you were editing it.",
    );
    match document_store::load_source(path, authority, bounds) {
        Ok(loaded) => conflict.with_source_text(loaded.document.text()),
        Err(_) => conflict,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::Instant;

    use festerm_document::Severity;

    use super::*;

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join("document-io-fixtures")
                .join(format!(
                    "festerm-documents-{}-{label}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.path.join(name);
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn registry_length(registry: &DocumentRegistry, id: DocumentId) -> usize {
        registry.get(id).unwrap().text().text().len()
    }

    fn type_into(registry: &mut DocumentRegistry, id: DocumentId, text: &str) {
        let document = registry.get_mut(id).unwrap();
        let end = document.text().text().len();
        document.text_mut().replace(end..end, text).unwrap();
    }

    #[test]
    fn terminal_file_link_refresh_and_save_preserve_no_follow_policy() {
        let directory = TemporaryDirectory::new("terminal-link-refresh");
        let path = fs::canonicalize(directory.file("report.md", "first\n")).unwrap();
        let mut registry = DocumentRegistry::new();
        let id = registry.open_terminal_link(&path).unwrap();
        let replacement = directory.file("replacement.md", "second\n");
        #[cfg(windows)]
        fs::remove_file(&path).unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_eq!(registry.refresh(id), Some(RefreshOutcome::Reloaded));
        type_into(&mut registry, id, "mine\n");
        assert_eq!(registry.save(id), Some(SaveOutcome::Saved));
        assert_eq!(
            registry.reload_from_source(id),
            Some(RefreshOutcome::Reloaded)
        );
        assert_eq!(registry.get(id).unwrap().text().text(), "second\nmine\n");
        #[cfg(unix)]
        {
            let other = directory.file("other.md", "private target\n");
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(other, &path).unwrap();
            assert_terminal_link_replacement_refused(&mut registry, id, "second\nmine\n");
        }
    }

    #[test]
    fn terminal_file_link_dirty_refresh_keeps_edits_and_bounded_conflict_source() {
        let directory = TemporaryDirectory::new("terminal-link-dirty-refresh");
        let path = fs::canonicalize(directory.file("report.md", "first\n")).unwrap();
        let mut registry = DocumentRegistry::new();
        let id = registry.open_terminal_link(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        let replacement = directory.file("replacement.md", "theirs\n");
        #[cfg(windows)]
        fs::remove_file(&path).unwrap();
        fs::rename(replacement, &path).unwrap();
        assert_eq!(registry.refresh(id), Some(RefreshOutcome::Conflict));
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "first\nmine\n");
        assert_eq!(document.conflict().unwrap().source_text(), Some("theirs\n"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_file_link_refresh_never_follows_replaced_file_or_parent() {
        for replacement in 0..3 {
            for reuse in [false, true] {
                let directory = TemporaryDirectory::new("terminal-link-replacement");
                let root = fs::canonicalize(&directory.path).unwrap();
                let source = root.join("source");
                let other = root.join("other");
                fs::create_dir(&source).unwrap();
                fs::create_dir(&other).unwrap();
                let path = source.join("report.md");
                fs::write(&path, "original\n").unwrap();
                fs::write(other.join("report.md"), "private target\n").unwrap();
                let mut registry = DocumentRegistry::new();
                let existing = reuse.then(|| registry.open_local(&path).unwrap());
                let id = registry.open_terminal_link(&path).unwrap();
                if let Some(existing) = existing {
                    assert_eq!(existing, id);
                }
                if replacement == 1 {
                    fs::rename(&source, root.join("retired")).unwrap();
                    std::os::unix::fs::symlink(&other, &source).unwrap();
                } else if replacement == 2 {
                    let retired = root.join("retired");
                    fs::rename(&source, &retired).unwrap();
                    fs::create_dir(&source).unwrap();
                    fs::hard_link(retired.join("report.md"), &path).unwrap();
                } else {
                    fs::remove_file(&path).unwrap();
                    std::os::unix::fs::symlink(other.join("report.md"), &path).unwrap();
                }
                assert_terminal_link_replacement_refused(&mut registry, id, "original\n");
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn terminal_file_link_refresh_never_follows_replaced_parent_junction() {
        let directory = TemporaryDirectory::new("terminal-link-junction-replacement");
        let root = fs::canonicalize(&directory.path).unwrap();
        let source = root.join("source");
        let other = root.join("other");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&other).unwrap();
        let path = source.join("report.md");
        fs::write(&path, "original\n").unwrap();
        fs::write(other.join("report.md"), "private target\n").unwrap();
        let mut registry = DocumentRegistry::new();
        let id = registry.open_terminal_link(&path).unwrap();
        // Unlink the open child so Windows permits renaming its parent.
        fs::remove_file(&path).unwrap();
        fs::rename(&source, root.join("retired")).unwrap();
        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&source)
            .arg(&other)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "owned junction fixture creation failed");
        assert_terminal_link_replacement_refused(&mut registry, id, "original\n");
        fs::remove_dir(&source).unwrap();
    }

    fn assert_terminal_link_replacement_refused(
        registry: &mut DocumentRegistry,
        id: DocumentId,
        expected: &str,
    ) {
        let document = registry.get(id).unwrap();
        let request = local_io::Request::new(
            id,
            document.io_epoch,
            document.text.revision(),
            document.generation.unwrap(),
            document.source_authority.clone().unwrap(),
            match &document.origin {
                DocumentOrigin::Local(origin) => origin.path().to_path_buf(),
                _ => unreachable!(),
            },
            document.availability.clone(),
            None,
            None,
            Instant::now(),
            None,
            registry.bounds(),
            Arc::new(local_io::Control::default()),
        );
        assert!(matches!(
            local_io::execute(&request, false),
            local_io::Result::Checked(local_io::Check::Failed(_))
        ));
        assert!(
            conflict_for(&request.path, &request.authority, registry.bounds())
                .source_text()
                .is_none()
        );
        assert!(matches!(
            registry.refresh(id),
            Some(RefreshOutcome::Unavailable(_))
        ));
        assert_eq!(
            registry.reload_from_source(id),
            Some(RefreshOutcome::Unreadable)
        );
        assert_eq!(registry.get(id).unwrap().text().text(), expected);
        assert!(registry.get(id).unwrap().conflict().is_none());
    }

    #[test]
    fn opening_one_path_twice_shares_a_single_buffer() {
        let directory = TemporaryDirectory::new("alias");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();

        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&path).unwrap();

        assert_eq!(first, second);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(first).unwrap().views(), 2);

        type_into(&mut registry, first, "beta\n");
        assert_eq!(registry.get(second).unwrap().text().text(), "alpha\nbeta\n");
        assert!(registry.get(second).unwrap().text().is_dirty());
    }

    #[cfg(unix)]
    #[test]
    fn opening_a_symlink_alias_reuses_the_loaded_source_authority() {
        let directory = TemporaryDirectory::new("symlink-alias");
        let path = directory.file("notes.md", "alpha\n");
        let alias = directory.path.join("alias.md");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();

        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&alias).unwrap();

        assert_eq!(first, second);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(first).unwrap().views(), 2);
    }

    #[test]
    fn opening_a_hard_link_alias_reuses_the_loaded_file_identity() {
        let directory = TemporaryDirectory::new("hard-link-alias");
        let path = directory.file("notes.md", "alpha\n");
        let alias = directory.path.join("alias.md");
        fs::hard_link(&path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();

        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&alias).unwrap();

        assert_eq!(first, second);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(first).unwrap().views(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn retargeted_cached_symlink_alias_does_not_reopen_the_old_document() {
        let directory = TemporaryDirectory::new("retargeted-cached-alias");
        let first_path = directory.file("first.md", "first\n");
        let second_path = directory.file("second.md", "second\n");
        let alias = directory.path.join("alias.md");
        std::os::unix::fs::symlink(&first_path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();

        let first = registry.open_local(&first_path).unwrap();
        assert_eq!(registry.open_local(&alias).unwrap(), first);
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&second_path, &alias).unwrap();
        assert_eq!(registry.find_current_local(&alias), None);
        assert_eq!(registry.find_local(&alias), None);

        let reopened = registry.open_local(&alias).unwrap();

        assert_ne!(reopened, first);
        assert_eq!(registry.get(reopened).unwrap().text().text(), "second\n");
    }

    #[test]
    fn stale_source_generation_cannot_establish_a_new_hard_link_alias() {
        let directory = TemporaryDirectory::new("stale-hard-link-alias");
        let path = directory.file("notes.md", "before\n");
        let alias = directory.path.join("alias.md");
        fs::hard_link(&path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();

        let stale = registry.open_local(&path).unwrap();
        fs::write(&path, "after with a different size\n").unwrap();
        let current = registry.open_local(&alias).unwrap();

        assert_ne!(current, stale);
        assert_eq!(
            registry.get(current).unwrap().text().text(),
            "after with a different size\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unlinked_source_cannot_reuse_its_retained_identity_for_a_replacement() {
        let directory = TemporaryDirectory::new("unlinked-retained-identity");
        let path = directory.file("notes.md", "before\n");
        let alias = directory.path.join("alias.md");
        fs::hard_link(&path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();
        let stale = registry.open_local(&path).unwrap();
        assert_eq!(registry.open_local(&alias).unwrap(), stale);

        fs::remove_file(&alias).unwrap();
        fs::write(&alias, "replacement\n").unwrap();
        let replacement = registry.open_local(&alias).unwrap();

        assert_ne!(replacement, stale);
        assert_eq!(
            registry.get(replacement).unwrap().text().text(),
            "replacement\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_symlink_aliases_are_revalidated_after_retargeting() {
        use std::os::windows::fs::symlink_file;

        let directory = TemporaryDirectory::new("windows-symlink-alias");
        let first_path = directory.file("first.md", "first\n");
        let second_path = directory.file("second.md", "second\n");
        let alias = directory.path.join("alias.md");
        if let Err(error) = symlink_file(&first_path, &alias) {
            if error.raw_os_error() == Some(1314)
                && std::env::var_os("FESTERM_REQUIRE_WINDOWS_SYMLINKS")
                    .is_none_or(|value| value.is_empty())
            {
                return;
            }
            panic!("Windows CI must permit the owned file-symlink fixture: {error}");
        }
        let mut registry = DocumentRegistry::new();

        let first = registry.open_local(&first_path).unwrap();
        assert_eq!(registry.open_local(&alias).unwrap(), first);
        fs::remove_file(&alias).unwrap();
        symlink_file(&second_path, &alias).unwrap();

        assert_eq!(registry.find_current_local(&alias), None);
        let reopened = registry.open_local(&alias).unwrap();
        assert_ne!(reopened, first);
        assert_eq!(registry.get(reopened).unwrap().text().text(), "second\n");
    }

    #[test]
    fn highlighting_is_read_only_over_the_document_it_describes() {
        let directory = TemporaryDirectory::new("syntax-read-only");
        let path = directory.file("main.rs", "fn main() { let x = 1; }\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();

        let before = registry.get(id).unwrap().text().clone();
        let spans = registry
            .get_mut(id)
            .unwrap()
            .syntax_spans(0..before.text().len())
            .to_vec();

        assert!(!spans.is_empty(), "a Rust file is coloured");
        let after = registry.get(id).unwrap().text();
        assert_eq!(after.revision(), before.revision());
        assert_eq!(after.text(), before.text());
        assert_eq!(after.to_bytes(), before.to_bytes());
        assert!(!after.is_dirty(), "colour is not a change");
        assert!(!after.can_undo(), "colour is not in the undo history");
    }

    #[test]
    fn two_views_of_one_file_share_the_one_parse() {
        let directory = TemporaryDirectory::new("syntax-shared");
        let path = directory.file("main.rs", "fn main() { let x = 1; }\n");
        let mut registry = DocumentRegistry::new();
        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&path).unwrap();
        assert_eq!(first, second, "one document, two views");

        let length = registry.get(first).unwrap().text().text().len();
        let from_first = registry
            .get_mut(first)
            .unwrap()
            .syntax_spans(0..length)
            .to_vec();
        let from_second = registry
            .get_mut(second)
            .unwrap()
            .syntax_spans(0..length)
            .to_vec();

        assert_eq!(
            from_first, from_second,
            "the second view reads the tree the first one paid for"
        );

        type_into(&mut registry, first, "fn other() {}\n");
        let grown = registry_length(&registry, second);
        let after = registry
            .get_mut(second)
            .unwrap()
            .syntax_spans(0..grown)
            .to_vec();
        assert!(
            after.len() > from_second.len(),
            "an edit in one view recolours the other"
        );
    }

    #[test]
    fn a_file_without_a_grammar_is_simply_not_coloured() {
        let directory = TemporaryDirectory::new("syntax-unknown");
        let path = directory.file("notes.unknownext", "alpha beta\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();

        assert!(registry.get_mut(id).unwrap().syntax_spans(0..11).is_empty());
        assert_eq!(
            registry.get(id).unwrap().syntax_status(),
            festerm_syntax::SyntaxStatus::UnknownLanguage
        );
        assert_eq!(
            registry.get(id).unwrap().syntax_status().note(),
            None,
            "no grammar is not a failure, so there is nothing to explain"
        );
    }

    #[test]
    fn a_document_survives_until_its_last_view_closes() {
        let directory = TemporaryDirectory::new("release");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.open_local(&path).unwrap();

        assert!(!registry.release(id));
        assert!(registry.is_open(id));
        assert!(registry.release(id));
        assert!(!registry.is_open(id));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn reopening_after_the_last_view_reads_the_file_again() {
        let directory = TemporaryDirectory::new("reopen");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let first = registry.open_local(&path).unwrap();
        type_into(&mut registry, first, "typed\n");
        registry.release(first);

        let second = registry.open_local(&path).unwrap();

        assert_ne!(first, second);
        assert_eq!(registry.get(second).unwrap().text().text(), "alpha\n");
    }

    #[test]
    fn saving_writes_the_file_and_clears_the_unsaved_marker() {
        let directory = TemporaryDirectory::new("save");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "beta\n");

        assert_eq!(registry.save(id), Some(SaveOutcome::Saved));

        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nbeta\n");
        assert!(!registry.get(id).unwrap().text().is_dirty());
        assert_eq!(
            registry.get(id).unwrap().status().severity(),
            Severity::Informational
        );
    }

    #[test]
    fn unchanged_refresh_restores_a_temporarily_unavailable_source() {
        let directory = TemporaryDirectory::new("refresh-restores-authority");
        let path = directory.file("notes.md", "alpha\n");
        let held = directory.path.join("held.md");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();

        fs::rename(&path, &held).unwrap();
        assert!(matches!(
            registry.refresh(id),
            Some(RefreshOutcome::Unavailable(_))
        ));
        assert!(registry
            .get(id)
            .unwrap()
            .local_source_generation()
            .is_none());

        fs::rename(&held, &path).unwrap();
        assert_eq!(registry.refresh(id), Some(RefreshOutcome::Unchanged));
        assert!(
            registry
                .get(id)
                .unwrap()
                .local_source_generation()
                .is_some(),
            "restoring the exact source and parent must restore Preview authority"
        );
    }

    #[test]
    fn unchanged_refresh_rejects_a_hard_link_in_a_replacement_parent() {
        let directory = TemporaryDirectory::new("refresh-parent");
        let retained = TemporaryDirectory::new("refresh-parent-retained");
        fs::remove_dir(&retained.path).unwrap();
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();

        #[cfg(windows)]
        {
            let error = fs::rename(&directory.path, &retained.path).unwrap_err();
            assert!(matches!(error.raw_os_error(), Some(5) | Some(32)));
            registry
                .get_mut(id)
                .unwrap()
                .source_authority
                .as_mut()
                .unwrap()
                .release_identity_pin_for_parent_rebind_test();
        }
        fs::rename(&directory.path, &retained.path).unwrap();
        fs::create_dir(&directory.path).unwrap();
        fs::hard_link(retained.path.join("notes.md"), &path).unwrap();

        assert!(matches!(
            registry.refresh(id),
            Some(RefreshOutcome::Unavailable(_))
        ));
        assert!(
            registry
                .get(id)
                .unwrap()
                .local_source_generation()
                .is_none(),
            "the same file generation must not transfer authority to a replacement parent"
        );
    }

    #[test]
    fn saving_somewhere_else_writes_there_and_leaves_the_original_alone() {
        let directory = TemporaryDirectory::new("save-as");
        let path = directory.file("notes.md", "alpha\n");
        let destination = directory.path.join("copy.md");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "beta\n");
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        let moved = moved.expect("the view has somewhere to follow");
        assert_ne!(
            moved, id,
            "a new destination is a new document, not a rename"
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "alpha\nbeta\n");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "alpha\n",
            "the file that was open is not what was written to"
        );
        assert!(!registry.get(moved).unwrap().text().is_dirty());
        assert!(
            registry.get(id).unwrap().text().is_dirty(),
            "the original still holds the typing nobody has saved to it"
        );
    }

    #[test]
    fn saving_onto_an_already_open_file_binds_to_that_document_rather_than_a_second_copy() {
        let directory = TemporaryDirectory::new("save-as-open");
        let source = directory.file("notes.md", "alpha\n");
        let destination = directory.file("other.md", "something else\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "beta\n");
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(
            moved,
            Some(existing),
            "one file is one buffer, however it came to be written"
        );
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            "alpha\nbeta\n",
            "and the document already open on it has to show what is now there"
        );
        assert!(
            registry.get(existing).unwrap().text().can_undo(),
            "rebinding must preserve the source buffer's edit history"
        );
        assert!(registry.get_mut(existing).unwrap().text_mut().undo());
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            "alpha\n",
            "the source edit remains undoable after the view rebinds"
        );
    }

    #[test]
    fn save_as_to_a_hard_link_rebinds_only_the_saving_view_to_a_new_document() {
        let directory = TemporaryDirectory::new("save-as-hard-link");
        let source = directory.file("source.md", "source\n");
        let destination = directory.path.join("destination.md");
        fs::hard_link(&source, &destination).unwrap();
        let mut registry = DocumentRegistry::new();
        let source_id = registry.open_local(&source).unwrap();
        assert_eq!(registry.open_local(&destination).unwrap(), source_id);
        type_into(&mut registry, source_id, "edited\n");
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry
            .save_as(source_id, &destination, &expectation)
            .unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        let destination_id = moved.expect("saving view destination");
        assert_ne!(destination_id, source_id);
        assert_eq!(fs::read_to_string(&source).unwrap(), "source\n");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "source\nedited\n"
        );
        assert!(registry.get(source_id).unwrap().text().is_dirty());
        assert!(!registry.get(destination_id).unwrap().text().is_dirty());
        assert_eq!(
            registry.get(source_id).unwrap().origin().file_name(),
            "source.md"
        );
        assert_eq!(
            registry.get(destination_id).unwrap().origin().file_name(),
            "destination.md"
        );
    }

    #[test]
    fn save_as_refuses_a_hard_link_alias_of_a_recovery_pending_source() {
        let directory = TemporaryDirectory::new("save-as-hard-link-recovery");
        let source = directory.file("source.md", "source\n");
        let destination = directory.path.join("destination.md");
        fs::hard_link(&source, &destination).unwrap();
        let mut registry = DocumentRegistry::new();
        let source_id = registry.open_local(&source).unwrap();
        type_into(&mut registry, source_id, "edited\n");
        #[cfg(not(windows))]
        {
            let target = source.clone();
            let replacement = directory.path.join("replacement.md");
            document_store::set_after_save_replacement_hook(move || {
                fs::write(&replacement, "later\n").unwrap();
                fs::rename(&replacement, &target).unwrap();
            });
        }
        #[cfg(windows)]
        {
            let target = source.clone();
            document_store::set_after_save_replacement_hook(move || {
                let mut permissions = fs::metadata(&target).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&target, permissions).unwrap();
            });
        }
        assert!(matches!(
            registry.save(source_id),
            Some(SaveOutcome::Failed(_))
        ));
        let recovery = registry
            .get(source_id)
            .unwrap()
            .recovery_path
            .clone()
            .expect("real save failure retained recovery");
        let error = registry
            .get(source_id)
            .unwrap()
            .recovery_error
            .clone()
            .expect("real save failure retained its error");
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&source).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&source, permissions).unwrap();
        }
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry
            .save_as(source_id, &destination, &expectation)
            .unwrap();

        assert_eq!(outcome, SaveOutcome::Failed(error));
        assert!(moved.is_none());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "source\n");
        assert_eq!(
            registry.get(source_id).unwrap().recovery_path.as_ref(),
            Some(&recovery)
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_as_revalidates_an_old_alias_key_after_rebinding_the_destination() {
        let directory = TemporaryDirectory::new("save-as-rekey-alias");
        let source = directory.file("source.md", "source\n");
        let destination = directory.file("destination.md", "destination\n");
        let alias = directory.path.join("alias.md");
        std::os::unix::fs::symlink(&destination, &alias).unwrap();
        let mut registry = DocumentRegistry::new();
        let source_id = registry.open_local(&source).unwrap();
        let destination_id = registry.open_local(&alias).unwrap();
        type_into(&mut registry, source_id, "edited\n");
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry
            .save_as(source_id, &destination, &expectation)
            .unwrap();
        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(moved, Some(destination_id));

        fs::remove_file(&alias).unwrap();
        fs::write(&alias, "unrelated\n").unwrap();
        let unrelated = registry.open_local(&alias).unwrap();

        assert_ne!(unrelated, destination_id);
        assert_eq!(
            registry.get(unrelated).unwrap().text().text(),
            "unrelated\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_as_revalidates_a_retargeted_secondary_alias_before_destination_reuse() {
        let directory = TemporaryDirectory::new("save-as-retargeted-secondary-alias");
        let source = directory.file("source.md", "source\n");
        let original = directory.file("original.md", "original\n");
        let alias = directory.path.join("alias.md");
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let mut registry = DocumentRegistry::new();
        let original_id = registry.open_local(&original).unwrap();
        assert_eq!(registry.open_local(&alias).unwrap(), original_id);
        let source_id = registry.open_local(&source).unwrap();
        type_into(&mut registry, source_id, "edited\n");

        fs::remove_file(&alias).unwrap();
        fs::write(&alias, "replacement\n").unwrap();
        let expectation = document_store::observe_destination(&alias).unwrap();
        let (outcome, moved) = registry.save_as(source_id, &alias, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert!(moved.is_some_and(|destination| destination != original_id));
        assert_eq!(fs::read_to_string(&alias).unwrap(), "source\nedited\n");
        assert_eq!(fs::read_to_string(&original).unwrap(), "original\n");
        assert_eq!(
            registry.get(original_id).unwrap().text().text(),
            "original\n"
        );
    }

    #[test]
    fn save_as_does_not_rebind_a_stale_primary_hard_link_document() {
        let directory = TemporaryDirectory::new("save-as-stale-primary-hard-link");
        let primary = directory.file("primary.md", "primary\n");
        let destination = directory.path.join("destination.md");
        fs::hard_link(&primary, &destination).unwrap();
        let source = directory.file("source.md", "source\n");
        let mut registry = DocumentRegistry::new();
        let stale = registry.open_local(&primary).unwrap();
        assert_eq!(registry.open_local(&destination).unwrap(), stale);
        let source_id = registry.open_local(&source).unwrap();
        type_into(&mut registry, source_id, "edited\n");

        let replacement = directory.file("replacement.md", "replacement\n");
        #[cfg(windows)]
        fs::remove_file(&primary).unwrap();
        fs::rename(&replacement, &primary).unwrap();
        let expectation = document_store::observe_destination(&destination).unwrap();
        let (outcome, moved) = registry
            .save_as(source_id, &destination, &expectation)
            .unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert!(moved.is_some_and(|destination_id| destination_id != stale));
        assert_eq!(registry.get(stale).unwrap().text().text(), "primary\n");
        assert_eq!(
            registry.get(stale).unwrap().origin().file_name(),
            "primary.md"
        );
        assert_eq!(fs::read_to_string(&primary).unwrap(), "replacement\n");
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "source\nedited\n"
        );
    }

    #[test]
    fn save_as_refuses_a_destination_with_pending_manual_recovery() {
        let directory = TemporaryDirectory::new("save-as-recovery");
        let source = directory.file("notes.md", "alpha\n");
        let destination = directory.file("other.md", "protected\n");
        let recovery = directory.path.join(".festerm-save-recovery.stage");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "beta\n");
        let error = SaveError::new(
            "Saving needs manual recovery",
            format!("Recover retained versions from {}.", recovery.display()),
        );
        let destination_document = registry.get_mut(existing).unwrap();
        destination_document.recovery_path = Some(recovery);
        destination_document.recovery_error = Some(error.clone());
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Failed(error));
        assert_eq!(moved, None);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "protected\n");
        assert!(registry.get(existing).unwrap().recovery_error.is_some());
    }

    #[test]
    fn save_as_recovery_is_promoted_while_the_source_view_remains_open() {
        let directory = TemporaryDirectory::new("save-as-promotes-recovery");
        let source = directory.file("source.md", "source\n");
        let destination = directory.file("destination.md", "destination\n");
        let alias = directory.path.join("destination-alias.md");
        fs::hard_link(&destination, &alias).unwrap();
        let mut registry = DocumentRegistry::new();
        let source_id = registry.open_local(&source).unwrap();
        type_into(&mut registry, source_id, "edited\n");
        let expectation = document_store::observe_destination(&destination).unwrap();
        #[cfg(not(windows))]
        {
            let target = destination.clone();
            let replacement = directory.path.join("replacement.md");
            document_store::set_after_save_replacement_hook(move || {
                fs::write(&replacement, "later\n").unwrap();
                fs::rename(&replacement, &target).unwrap();
            });
        }
        #[cfg(windows)]
        {
            let target = destination.clone();
            document_store::set_after_save_replacement_hook(move || {
                let mut permissions = fs::metadata(&target).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&target, permissions).unwrap();
            });
        }

        let (outcome, moved) = registry
            .save_as(source_id, &destination, &expectation)
            .unwrap();

        assert!(matches!(outcome, SaveOutcome::Failed(_)));
        assert!(moved.is_none());
        let recovery = registry
            .get(source_id)
            .unwrap()
            .recovery_path()
            .expect("save as retained exact recovery")
            .to_owned();
        let (notice_path, _) = registry
            .take_recovery_notice()
            .expect("save as immediately promoted its recovery notice");
        assert_eq!(notice_path, recovery);
        assert_eq!(registry.get(source_id).unwrap().views(), 1);
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&destination).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&destination, permissions).unwrap();
        }
        assert!(registry.acknowledge_recovery_notice(&notice_path));
        assert!(registry.release(source_id));
        let reopened_destination = registry.open_local(&destination).unwrap();
        assert_eq!(
            registry.get(reopened_destination).unwrap().recovery_path(),
            Some(recovery.as_path())
        );
        assert!(matches!(
            registry.save(reopened_destination),
            Some(SaveOutcome::Failed(_))
        ));
        assert!(registry.release(reopened_destination));
        let reopened_source = registry.open_local(&source).unwrap();
        let confirmed = document_store::observe_destination(&destination).unwrap();
        assert!(matches!(
            registry.save_as(reopened_source, &destination, &confirmed),
            Some((SaveOutcome::Failed(_), None))
        ));
        let aliased = registry.open_local(&alias).unwrap();
        assert_eq!(
            registry.get(aliased).unwrap().recovery_path(),
            Some(recovery.as_path())
        );
        assert!(registry.release(aliased));
        let confirmed_alias = document_store::observe_destination(&alias).unwrap();
        assert!(matches!(
            registry.save_as(reopened_source, &alias, &confirmed_alias),
            Some((SaveOutcome::Failed(_), None))
        ));
    }

    #[test]
    fn save_as_refuses_a_dirty_open_destination_without_mutating_either_buffer_or_disk() {
        let directory = TemporaryDirectory::new("save-as-dirty-open");
        let source = directory.file("notes.md", "source\n");
        let destination = directory.file("other.md", "destination\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "source edit\n");
        type_into(&mut registry, existing, "destination edit\n");
        let expectation = document_store::observe_destination(&destination).unwrap();
        let source_before = registry.get(id).unwrap().text().text().to_owned();
        let destination_before = registry.get(existing).unwrap().text().text().to_owned();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        let error = match outcome {
            SaveOutcome::Failed(error) => error,
            other => panic!("expected visible refusal, got {other:?}"),
        };
        assert_eq!(moved, None);
        assert_eq!(error.headline(), "That destination has unsaved work");
        assert!(error.detail().contains("Nothing was written"));
        assert_eq!(fs::read_to_string(&destination).unwrap(), "destination\n");
        assert_eq!(registry.get(id).unwrap().text().text(), source_before);
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            destination_before
        );
        assert!(registry.get(id).unwrap().text().is_dirty());
        assert!(registry.get(id).unwrap().text().can_undo());
        assert!(registry.get(existing).unwrap().text().is_dirty());
        assert!(registry.get(existing).unwrap().text().can_undo());
        assert_eq!(registry.get(id).unwrap().status().detail(), error.detail());
    }

    #[test]
    fn save_as_refuses_a_conflicted_open_destination_without_mutation() {
        let directory = TemporaryDirectory::new("save-as-conflicted-open");
        let source = directory.file("notes.md", "source\n");
        let destination = directory.file("other.md", "destination\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "source edit\n");
        type_into(&mut registry, existing, "destination edit\n");
        fs::write(&destination, b"outside\n").unwrap();
        assert!(matches!(
            registry.refresh(existing),
            Some(RefreshOutcome::Conflict)
        ));
        let expectation = document_store::observe_destination(&destination).unwrap();
        let source_before = registry.get(id).unwrap().text().text().to_owned();
        let destination_before = registry.get(existing).unwrap().text().text().to_owned();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert!(matches!(outcome, SaveOutcome::Failed(_)));
        assert_eq!(moved, None);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "outside\n");
        assert_eq!(registry.get(id).unwrap().text().text(), source_before);
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            destination_before
        );
        assert!(registry.get(existing).unwrap().conflict().is_some());
    }

    #[test]
    fn save_as_uses_the_clean_open_destinations_recorded_generation() {
        let directory = TemporaryDirectory::new("save-as-clean-generation");
        let source = directory.file("notes.md", "source\n");
        let destination = directory.file("other.md", "destination\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "source edit\n");
        let expectation = document_store::observe_destination(&destination).unwrap();
        fs::write(&destination, b"outside and newer\n").unwrap();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert!(matches!(outcome, SaveOutcome::Failed(_)));
        assert_eq!(moved, None);
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "outside and newer\n"
        );
        assert!(registry.get(id).unwrap().text().is_dirty());
        assert!(!registry.get(existing).unwrap().text().is_dirty());
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            "destination\n"
        );
    }

    #[test]
    fn save_as_can_recreate_a_clean_open_destination_confirmed_absent() {
        let directory = TemporaryDirectory::new("save-as-clean-missing");
        let source = directory.file("notes.md", "source\n");
        let destination = directory.file("other.md", "destination\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let existing = registry.open_local(&destination).unwrap();
        type_into(&mut registry, id, "source edit\n");
        fs::remove_file(&destination).unwrap();
        assert!(matches!(
            registry.refresh(existing),
            Some(RefreshOutcome::Unavailable(UnavailableReason::Missing))
        ));
        let expectation = document_store::observe_destination(&destination).unwrap();
        assert_eq!(expectation.expectation(), DestinationExpectation::Absent);

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(moved, Some(existing));
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "source\nsource edit\n"
        );
        assert_eq!(
            registry.get(existing).unwrap().text().text(),
            "source\nsource edit\n"
        );
    }

    #[test]
    fn save_as_chooses_the_lowest_matching_document_id_deterministically() {
        let directory = TemporaryDirectory::new("save-as-deterministic-match");
        let source = directory.file("notes.md", "source\n");
        let destination = directory.file("other.md", "destination\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&source).unwrap();
        let first = registry.open_local(&destination).unwrap();
        let (origin, text, generation, source_authority, read_only) = {
            let document = registry.documents.get(&first).unwrap();
            (
                document.origin.clone(),
                document.text.clone(),
                document.generation,
                document.source_authority.clone(),
                document.read_only,
            )
        };
        let second = registry.insert(origin, text, generation, source_authority, read_only);
        registry.by_key.clear();
        type_into(&mut registry, id, "source edit\n");
        let expectation = document_store::observe_destination(&destination).unwrap();

        let (outcome, moved) = registry.save_as(id, &destination, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(moved, Some(first.min(second)));
    }

    #[test]
    fn clean_save_as_to_the_same_document_does_not_retain_a_phantom_view() {
        let directory = TemporaryDirectory::new("save-as-same-clean");
        let path = directory.file("notes.md", "source\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        let expectation = document_store::observe_destination(&path).unwrap();

        let (outcome, moved) = registry.save_as(id, &path, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(moved, Some(id));
        assert_eq!(registry.get(id).unwrap().views(), 1);
    }

    #[test]
    fn dirty_save_as_to_the_same_document_uses_source_authority_without_self_conflict() {
        let directory = TemporaryDirectory::new("save-as-same-dirty");
        let path = directory.file("notes.md", "source\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "edit\n");
        let expectation = document_store::observe_destination(&path).unwrap();

        let (outcome, moved) = registry.save_as(id, &path, &expectation).unwrap();

        assert_eq!(outcome, SaveOutcome::Saved);
        assert_eq!(moved, Some(id));
        assert_eq!(fs::read_to_string(&path).unwrap(), "source\nedit\n");
        assert!(!registry.get(id).unwrap().text().is_dirty());
        assert!(registry.get(id).unwrap().text().can_undo());
        assert_eq!(registry.get(id).unwrap().views(), 1);
    }

    #[test]
    fn create_untitled_snapshot_starts_dirty_and_saveable() {
        let mut registry = DocumentRegistry::new();

        let id = registry.create_untitled(
            "terminal-history",
            "Local Shell · terminal history snapshot",
            b"alpha\nbeta",
        );

        let document = registry.get(id).unwrap();
        assert!(matches!(
            document.origin(),
            DocumentOrigin::Untitled(origin)
                if origin.file_name() == "terminal-history-1.txt"
                    && origin.qualified_label()
                        == "Local Shell · terminal history snapshot"
        ));
        assert_eq!(document.text().text(), "alpha\nbeta");
        assert!(document.text().is_dirty());
        assert_eq!(document.text().revision(), 0);
        assert!(!document.text().can_undo());
        assert!(!document.text().can_redo());
        assert!(document.status().can_save());
        assert!(document.status().can_save_as());
        assert_eq!(document.status().auto_save(), AutoSaveControl::Unavailable);
    }

    #[test]
    fn untitled_save_as_failure_preserves_the_snapshot_and_reports_the_error() {
        let directory = TemporaryDirectory::new("untitled-save-error");
        let mut registry = DocumentRegistry::new();
        let id = registry.create_untitled(
            "terminal-history",
            "Local Shell · terminal history snapshot",
            b"alpha\n",
        );
        let confirmed =
            document_store::observe_destination(&directory.path.join("snapshot.txt")).unwrap();

        let (outcome, moved) = registry
            .save_as(id, directory.path.as_path(), &confirmed)
            .unwrap();
        let error = match outcome {
            SaveOutcome::Failed(error) => error,
            other => panic!("expected save failure, got {other:?}"),
        };

        assert_eq!(moved, None);
        let document = registry.get(id).unwrap();
        assert!(matches!(document.origin(), DocumentOrigin::Untitled(_)));
        assert_eq!(document.text().text(), "alpha\n");
        assert!(document.text().is_dirty());
        assert_eq!(document.status().detail(), error.detail());
    }

    #[test]
    fn saving_an_untitled_snapshot_requires_save_as_instead_of_succeeding_silently() {
        let mut registry = DocumentRegistry::new();
        let id = registry.create_untitled(
            "terminal-history",
            "Local Shell · terminal history snapshot",
            b"alpha\n",
        );

        let outcome = registry.save(id).unwrap();

        let error = match outcome {
            SaveOutcome::Failed(error) => error,
            other => panic!("expected save failure, got {other:?}"),
        };
        assert_eq!(error.headline(), "Choose a destination first");
        assert_eq!(
            error.detail(),
            "This document is not on disk yet. Use Save As to choose where to write it."
        );
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn untitled_names_are_sequential_per_creation_kind() {
        let directory = TemporaryDirectory::new("untitled-names");
        let path = directory.file("existing.txt", "existing\n");
        let mut registry = DocumentRegistry::new();
        registry.open_local(&path).unwrap();

        let first = registry.create_untitled("Untitled", "New unsaved file", b"");
        let history = registry.create_untitled(
            "terminal-history",
            "Local Shell · terminal history snapshot",
            b"history",
        );
        let second = registry.create_untitled("Untitled", "New unsaved file", b"");

        assert_eq!(
            registry.get(first).unwrap().origin().file_name(),
            "Untitled-1.txt"
        );
        assert_eq!(
            registry.get(history).unwrap().origin().file_name(),
            "terminal-history-1.txt"
        );
        assert_eq!(
            registry.get(second).unwrap().origin().file_name(),
            "Untitled-2.txt"
        );
    }

    #[test]
    fn untitled_snapshots_never_enter_auto_save() {
        let mut registry = DocumentRegistry::new();
        let id = registry.create_untitled(
            "terminal-history",
            "Local Shell · terminal history snapshot",
            b"alpha\n",
        );
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let start = Instant::now();

        assert!(!registry.get(id).unwrap().auto_save_should_write());
        assert!(registry.auto_save(start + AUTO_SAVE_IDLE * 4).is_empty());
        assert_eq!(
            registry.get(id).unwrap().status().auto_save(),
            AutoSaveControl::Unavailable
        );
    }

    #[test]
    fn a_clean_document_follows_the_file_when_it_changes_elsewhere() {
        let directory = TemporaryDirectory::new("reload");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();

        fs::write(&path, "written by somebody else\n").unwrap();

        assert_eq!(registry.refresh(id), Some(RefreshOutcome::Reloaded));
        assert_eq!(
            registry.get(id).unwrap().text().text(),
            "written by somebody else\n"
        );
        assert!(registry.get(id).unwrap().conflict().is_none());
    }

    #[test]
    fn a_dirty_document_conflicts_rather_than_losing_what_was_typed() {
        let directory = TemporaryDirectory::new("conflict");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");

        fs::write(&path, "theirs\n").unwrap();

        assert_eq!(registry.refresh(id), Some(RefreshOutcome::Conflict));
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "alpha\nmine\n");
        assert_eq!(document.conflict().unwrap().source_text(), Some("theirs\n"));
        assert!(document.conflict().unwrap().can_compare());
        assert_eq!(document.status().severity(), Severity::Blocking);
        assert!(!document.status().can_save());
    }

    #[test]
    fn manual_recovery_notice_survives_polling_and_names_the_recovery_directory() {
        let directory = TemporaryDirectory::new("manual-recovery");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        #[cfg(not(windows))]
        {
            let target = path.clone();
            let replacement = directory.path.join("replacement.md");
            document_store::set_after_save_replacement_hook(move || {
                fs::write(&replacement, "later\n").unwrap();
                fs::rename(&replacement, &target).unwrap();
            });
        }
        #[cfg(windows)]
        {
            let target = path.clone();
            document_store::set_after_save_replacement_hook(move || {
                let mut permissions = fs::metadata(&target).unwrap().permissions();
                permissions.set_readonly(true);
                fs::set_permissions(&target, permissions).unwrap();
            });
        }

        assert!(matches!(registry.save(id), Some(SaveOutcome::Failed(_))));
        let recovery = registry
            .get(id)
            .unwrap()
            .recovery_path
            .clone()
            .expect("exact retained recovery path");
        assert!(
            registry.has_recovery_notices(),
            "an open document's retained recovery must block application close and update actions"
        );
        let (retained_path, retained_error) = registry
            .take_recovery_notice()
            .expect("the open document immediately promotes its application notice");
        assert_eq!(retained_path, recovery);
        assert!(retained_error
            .detail()
            .contains(&recovery.display().to_string()));
        assert!(registry.acknowledge_recovery_notice(&retained_path));
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            fs::set_permissions(&path, permissions).unwrap();
        }

        assert!(registry.poll(Instant::now() + POLL_INTERVAL).is_empty());
        assert!(matches!(registry.save(id), Some(SaveOutcome::Failed(_))));

        let document = registry.get(id).unwrap();
        assert!(document.conflict().is_none());
        assert!(document
            .status()
            .detail()
            .contains(&recovery.display().to_string()));
        assert_eq!(document.status().severity(), Severity::Blocking);
        assert!(!document.status().can_save());
        assert!(registry.release(id));
        assert!(
            registry.take_recovery_notice().is_none(),
            "final-view release must not duplicate an acknowledged notice"
        );
        let reopened = registry.open_local(&path).unwrap();
        assert_eq!(
            registry.get(reopened).unwrap().recovery_path(),
            Some(recovery.as_path())
        );
        assert!(matches!(
            registry.save(reopened),
            Some(SaveOutcome::Failed(_))
        ));
    }

    #[test]
    fn saving_into_a_file_that_changed_first_conflicts_and_writes_nothing() {
        let directory = TemporaryDirectory::new("saveconflict");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        fs::write(&path, "theirs\n").unwrap();

        match registry.save(id) {
            Some(SaveOutcome::Conflict(conflict)) => {
                assert_eq!(conflict.source_text(), Some("theirs\n"));
            }
            other => panic!("expected a conflict, got {other:?}"),
        }

        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn keeping_my_version_dismisses_the_banner_without_writing() {
        let directory = TemporaryDirectory::new("keep");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        fs::write(&path, "theirs\n").unwrap();
        registry.refresh(id);

        registry.get_mut(id).unwrap().keep_my_version();

        assert!(registry.get(id).unwrap().conflict().is_none());
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn reloading_from_the_source_replaces_the_buffer_in_every_view() {
        let directory = TemporaryDirectory::new("reloadexplicit");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&path).unwrap();
        type_into(&mut registry, first, "mine\n");
        fs::write(&path, "theirs\n").unwrap();

        assert_eq!(
            registry.reload_from_source(first),
            Some(RefreshOutcome::Reloaded)
        );

        assert_eq!(registry.get(second).unwrap().text().text(), "theirs\n");
        assert!(!registry.get(second).unwrap().text().is_dirty());
    }

    #[test]
    fn a_deleted_file_keeps_the_buffer_and_says_the_source_is_gone() {
        let directory = TemporaryDirectory::new("deleted");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        fs::remove_file(&path).unwrap();

        assert_eq!(
            registry.refresh(id),
            Some(RefreshOutcome::Unavailable(UnavailableReason::Missing))
        );
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "alpha\nmine\n");
        assert!(!document.status().can_save());
        assert!(document.status().can_save_as());
        assert_eq!(document.status().auto_save(), AutoSaveControl::Unavailable);
    }

    #[test]
    fn auto_save_writes_only_while_the_source_is_usable() {
        let directory = TemporaryDirectory::new("autosave");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");

        assert!(registry.get(id).unwrap().auto_save_should_write());

        fs::write(&path, "theirs\n").unwrap();
        registry.refresh(id);

        assert!(!registry.get(id).unwrap().auto_save_should_write());
        assert_eq!(
            registry.get(id).unwrap().status().auto_save(),
            AutoSaveControl::Paused
        );
    }

    #[test]
    fn auto_save_waits_for_typing_to_settle_and_then_writes_once() {
        let directory = TemporaryDirectory::new("autosave-debounce");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let start = Instant::now();

        // Three keystrokes, each one arriving before the debounce expires.
        for (step, text) in ["m", "i", "n"].iter().enumerate() {
            type_into(&mut registry, id, text);
            let now = start + AUTO_SAVE_IDLE.mul_f32(0.6) * (step as u32 + 1);
            assert!(
                registry.auto_save(now).is_empty(),
                "a document still being typed into is not written"
            );
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\n");

        let settled = start + AUTO_SAVE_IDLE * 4;
        assert_eq!(
            registry.auto_save(settled),
            vec![(id, SaveOutcome::Saved)],
            "one write, once the typing stopped"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\nmin");
        assert!(!registry.get(id).unwrap().text().is_dirty());

        assert!(
            registry.auto_save(settled + AUTO_SAVE_IDLE * 2).is_empty(),
            "a clean document is not written again and again"
        );
    }

    #[test]
    fn auto_save_left_off_never_writes_however_long_it_waits() {
        let directory = TemporaryDirectory::new("autosave-off");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "mine\n");
        let start = Instant::now();

        assert!(registry.auto_save(start).is_empty());
        assert!(registry.auto_save(start + AUTO_SAVE_IDLE * 10).is_empty());
        assert!(registry.auto_save(start + AUTO_SAVE_IDLE * 20).is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\n");
        assert!(
            registry.get(id).unwrap().text().is_dirty(),
            "and nothing about the document was quietly changed"
        );
    }

    #[test]
    fn auto_save_pauses_on_conflict_rather_than_overwriting_the_other_version() {
        let directory = TemporaryDirectory::new("autosave-conflict");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        change_outside(&path, "theirs\n");
        registry.refresh(id);

        let start = Instant::now();
        assert!(registry.auto_save(start).is_empty());
        assert!(registry.auto_save(start + AUTO_SAVE_IDLE * 4).is_empty());

        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");
        assert_eq!(
            registry.get(id).unwrap().status().auto_save(),
            AutoSaveControl::Paused
        );
    }

    #[test]
    fn a_failed_auto_save_is_not_retried_until_the_document_changes_again() {
        let directory = TemporaryDirectory::new("autosave-failure");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        // The file is replaced from outside *without* the document noticing,
        // so the write itself is what discovers the clash.
        change_outside(&path, "theirs\n");

        // The first call is the frame that notices the edit; the debounce is
        // measured from there, exactly as the application's per-frame call
        // does it.
        let start = Instant::now();
        assert!(registry.auto_save(start).is_empty());
        let first = registry.auto_save(start + AUTO_SAVE_IDLE * 2);
        assert_eq!(first.len(), 1, "the write was attempted once");
        assert!(matches!(first[0].1, SaveOutcome::Conflict(_)));
        assert_eq!(fs::read_to_string(&path).unwrap(), "theirs\n");

        assert!(
            registry.auto_save(start + AUTO_SAVE_IDLE * 8).is_empty(),
            "the same failing content is not attempted again on a timer"
        );
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn auto_save_belongs_to_the_document_so_every_view_agrees() {
        let directory = TemporaryDirectory::new("autosave-shared");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();
        let first = registry.open_local(&path).unwrap();
        let second = registry.open_local(&path).unwrap();
        assert_eq!(first, second, "two views, one document");

        registry
            .get_mut(first)
            .unwrap()
            .set_auto_save_requested(true);

        assert_eq!(
            registry.get(second).unwrap().status().auto_save(),
            AutoSaveControl::On,
            "the second view cannot hold a different answer"
        );
    }

    #[test]
    fn a_file_too_large_to_edit_is_refused_with_its_limit() {
        let directory = TemporaryDirectory::new("bounds");
        let path = directory.file("big.txt", &"x".repeat(512));
        let mut registry = DocumentRegistry::with_bounds(DocumentBounds::new(64, 16, 64));

        let failure = registry.open_local(&path).unwrap_err();

        assert_eq!(failure.headline(), "This file is too large to edit");
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn a_file_that_is_not_text_is_refused_by_name_rather_than_shown_as_rubble() {
        // The Open File sheet lists every file, because an extension cannot
        // tell a `Makefile` from a `.png`. What keeps that honest is this: a
        // file that is not text is refused when it is read, in words.
        let directory = TemporaryDirectory::new("binary");
        let path = directory.file("image.bin", "PNG\u{0}\u{0}\u{1}rubble");
        let mut registry = DocumentRegistry::new();

        let failure = registry.open_local(&path).unwrap_err();

        assert_eq!(failure.headline(), "This file appears to be binary");
        assert_eq!(registry.len(), 0, "a refused file opens no document");
    }

    #[test]
    fn an_already_open_path_can_be_found_without_reading_the_file() {
        let directory = TemporaryDirectory::new("find");
        let path = directory.file("notes.md", "alpha\n");
        let mut registry = DocumentRegistry::new();

        assert_eq!(registry.find_local(&path), None);
        let id = registry.open_local(&path).unwrap();
        assert_eq!(registry.find_local(&path), Some(id));
        assert_eq!(registry.get(id).unwrap().views(), 1);
    }

    /// Writes a file in a way an outside program would, and makes sure the
    /// change is actually detectable: same-size writes inside one filesystem
    /// timestamp tick are exactly the case `Generation` exists for, so the
    /// tests must not accidentally depend on the clock.
    fn change_outside(path: &std::path::Path, contents: &str) {
        std::thread::sleep(Duration::from_millis(10));
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_clean_document_follows_a_change_made_outside_festerm() {
        let directory = TemporaryDirectory::new("poll-clean");
        let path = directory.file("notes.md", "first\n");
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(&path).unwrap();

        change_outside(&path, "second\n");
        // Before the interval is up, nothing is even looked at.
        assert!(documents.borrow_mut().poll(Instant::now()).is_empty());
        assert_eq!(documents.borrow().get(id).unwrap().text().text(), "first\n");

        let changed = documents.borrow_mut().poll(Instant::now() + POLL_INTERVAL);
        assert_eq!(changed, vec![(id, RefreshOutcome::Reloaded)]);
        assert_eq!(
            documents.borrow().get(id).unwrap().text().text(),
            "second\n"
        );
        // And it says so, rather than the text changing under the reader in
        // silence.
        let status = documents.borrow().get(id).unwrap().status();
        assert_eq!(status.headline(), "Reloaded from disk");
        assert_eq!(status.severity(), Severity::Informational);
    }

    #[test]
    fn a_dirty_document_never_follows_a_change_made_outside_festerm() {
        let directory = TemporaryDirectory::new("poll-dirty");
        let path = directory.file("notes.md", "first\n");
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(&path).unwrap();
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .text_mut()
            .sync_from_view("mine\n")
            .unwrap();

        change_outside(&path, "theirs\n");
        let changed = documents.borrow_mut().poll(Instant::now() + POLL_INTERVAL);

        assert_eq!(changed, vec![(id, RefreshOutcome::Conflict)]);
        // The user's text is untouched and the other version is in hand for
        // Compare, without a second read of the file.
        let registry = documents.borrow();
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "mine\n");
        assert_eq!(document.conflict().unwrap().source_text(), Some("theirs\n"));
        assert_eq!(document.status().severity(), Severity::Blocking);
    }

    #[test]
    fn a_conflict_is_left_alone_until_the_user_resolves_it() {
        let directory = TemporaryDirectory::new("poll-conflicted");
        let path = directory.file("notes.md", "first\n");
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(&path).unwrap();
        documents
            .borrow_mut()
            .get_mut(id)
            .unwrap()
            .text_mut()
            .sync_from_view("mine\n")
            .unwrap();
        change_outside(&path, "theirs\n");
        documents.borrow_mut().poll(Instant::now() + POLL_INTERVAL);

        change_outside(&path, "theirs again\n");
        let changed = documents
            .borrow_mut()
            .poll(Instant::now() + POLL_INTERVAL * 4);

        // Replacing a conflict banner with the same conflict banner tells the
        // user nothing and would discard what Compare is already showing.
        assert!(changed.is_empty());
        assert_eq!(
            documents
                .borrow()
                .get(id)
                .unwrap()
                .conflict()
                .unwrap()
                .source_text(),
            Some("theirs\n")
        );
    }

    #[test]
    fn regaining_focus_re_checks_without_waiting_for_the_interval() {
        let directory = TemporaryDirectory::new("focus-revalidate");
        let path = directory.file("notes.md", "first\n");
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(&path).unwrap();

        change_outside(&path, "second\n");
        let changed = documents.borrow_mut().revalidate_all();

        assert_eq!(changed, vec![(id, RefreshOutcome::Reloaded)]);
    }

    #[test]
    fn a_file_that_disappears_keeps_its_buffer_and_says_the_source_is_gone() {
        let directory = TemporaryDirectory::new("poll-deleted");
        let path = directory.file("notes.md", "first\n");
        let documents = DocumentRegistry::shared();
        let id = documents.borrow_mut().open_local(&path).unwrap();

        fs::remove_file(&path).unwrap();
        let changed = documents.borrow_mut().poll(Instant::now() + POLL_INTERVAL);

        assert_eq!(
            changed,
            vec![(id, RefreshOutcome::Unavailable(UnavailableReason::Missing))]
        );
        assert_eq!(documents.borrow().get(id).unwrap().text().text(), "first\n");
    }

    fn controlled_worker(
        registry: &mut DocumentRegistry,
        executor: impl Fn(&local_io::Request, bool) -> local_io::Result + Send + Sync + 'static,
    ) -> std::sync::mpsc::Receiver<()> {
        let (wake, woken) = std::sync::mpsc::sync_channel(64);
        registry.worker = Some(
            local_io::Worker::start_with(
                Arc::new(move || {
                    wake.try_send(()).unwrap();
                }),
                Arc::clone(&registry.write_gate),
                Arc::new(executor),
            )
            .unwrap(),
        );
        woken
    }

    fn wait_completion(
        registry: &mut DocumentRegistry,
        woken: &std::sync::mpsc::Receiver<()>,
    ) -> bool {
        woken.recv_timeout(Duration::from_secs(10)).unwrap();
        registry.drain_background()
    }

    fn controlled_saved(request: &local_io::Request) -> local_io::Result {
        // Simulates completed publication on an owned fixture, not the
        // platform security protocol (which has separate store tests).
        fs::write(&request.path, request.bytes.as_ref().unwrap()).unwrap();
        let loaded = document_store::load(&request.path, request.bounds).unwrap();
        local_io::Result::Saved(
            Ok(document_store::SavedDocument {
                generation: loaded.generation,
                source_authority: loaded.source_authority,
                read_only: loaded.read_only,
            }),
            None,
        )
    }

    #[test]
    fn background_worker_fairly_services_overdue_freshness_and_autosave() {
        let directory = TemporaryDirectory::new("background-queue-fairness");
        let mut registry = DocumentRegistry::new();
        let ids: Vec<_> = ["a.md", "b.md", "c.md", "d.md", "bootstrap.md"]
            .into_iter()
            .map(|name| {
                registry
                    .open_local(&directory.file(name, "owned\n"))
                    .unwrap()
            })
            .collect();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let bootstrap = ids[4];
        let woken = controlled_worker(&mut registry, move |request, save| {
            entered.send((request.id, save)).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            if request.id == bootstrap {
                local_io::Result::Checked(local_io::Check::Current(false))
            } else if save {
                controlled_saved(request)
            } else {
                // Deterministically model checks remaining due after slow I/O,
                // without depending on wall-clock sleeps or executor speed.
                request.control.force.store(true, Ordering::Release);
                local_io::Result::Checked(local_io::Check::Current(true))
            }
        });
        let now = Instant::now();
        let controls: Vec<_> = ids
            .iter()
            .map(|_| Arc::new(local_io::Control::default()))
            .collect();
        let worker = registry.worker.as_ref().unwrap();
        let register = |index: usize| {
            let document = registry.get(ids[index]).unwrap();
            let DocumentOrigin::Local(origin) = &document.origin else {
                unreachable!()
            };
            worker
                .register(|| {
                    local_io::Request::new(
                        ids[index],
                        0,
                        document.text.revision(),
                        document.generation.unwrap(),
                        document.source_authority.clone().unwrap(),
                        origin.path().to_path_buf(),
                        Availability::Available,
                        None,
                        (index == 2).then(|| b"saved\n".to_vec()),
                        now,
                        (index == 2).then_some(now),
                        registry.bounds,
                        Arc::clone(&controls[index]),
                    )
                })
                .unwrap();
        };
        register(4);
        assert_eq!(
            waiting.recv_timeout(Duration::from_secs(10)).unwrap(),
            (bootstrap, false)
        );
        for index in 0..4 {
            register(index);
        }
        release.send(()).unwrap();
        let mut seen = vec![waiting.recv_timeout(Duration::from_secs(10)).unwrap()];
        for _ in 1..4 {
            release.send(()).unwrap();
            seen.push(waiting.recv_timeout(Duration::from_secs(10)).unwrap());
        }
        for control in &controls {
            control.cancel();
        }
        release.send(()).unwrap();
        // Disconnect before asserting so a failing regression cannot leave a
        // continuously due worker behind.
        registry.worker = None;
        assert_eq!(
            seen,
            vec![
                (ids[0], false),
                (ids[1], false),
                (ids[2], true),
                (ids[3], false)
            ]
        );
        assert!(woken.recv_timeout(Duration::from_secs(10)).is_ok());
        assert_eq!(
            fs::read_to_string(directory.path.join("c.md")).unwrap(),
            "saved\n"
        );
    }

    #[test]
    fn background_worker_accepts_commands_while_existing_freshness_is_continuously_due() {
        let directory = TemporaryDirectory::new("background-command-fairness");
        let mut registry = DocumentRegistry::new();
        let first = registry
            .open_local(&directory.file("first.md", "first\n"))
            .unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let (checked, next_checked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let first_check = std::sync::atomic::AtomicBool::new(true);
        let second_seen = std::sync::atomic::AtomicBool::new(false);
        let woken = controlled_worker(&mut registry, move |request, save| {
            assert!(!save);
            if request.id == first {
                if first_check.swap(false, Ordering::AcqRel) {
                    entered.send(()).unwrap();
                    blocked
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10))
                        .unwrap();
                }
                request
                    .control
                    .force
                    .store(!second_seen.load(Ordering::Acquire), Ordering::Release);
            } else {
                second_seen.store(true, Ordering::Release);
                checked.send(()).unwrap();
            }
            local_io::Result::Checked(local_io::Check::Current(true))
        });
        let context = egui::Context::default();
        assert!(!registry.background_tick(&context, Instant::now(), true));
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let second = registry
            .open_local(&directory.file("second.md", "second\n"))
            .unwrap();
        assert!(!registry.background_tick(&context, Instant::now(), true));
        release.send(()).unwrap();
        next_checked.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(registry.get(second).unwrap().text().text(), "second\n");
        assert!(woken.try_recv().is_err());
    }

    #[test]
    fn background_slow_freshness_and_reload_leave_frame_nonblocking() {
        let directory = TemporaryDirectory::new("background-slow-read");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let woken = controlled_worker(&mut registry, move |request, save| {
            assert!(!save);
            entered.send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            local_io::Result::Checked(local_io::Check::Loaded(Box::new(
                document_store::load(&request.path, request.bounds).unwrap(),
            )))
        });
        fs::write(&path, "external longer text\n").unwrap();
        let context = egui::Context::default();
        assert!(!registry.background_tick(&context, Instant::now(), true));
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        for _ in 0..5 {
            assert!(!registry.background_tick(&context, Instant::now(), true));
        }
        assert_eq!(registry.get(id).unwrap().text().text(), "original\n");
        assert!(woken.try_recv().is_err());
        release.send(()).unwrap();
        assert!(wait_completion(&mut registry, &woken));
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "external longer text\n");
        assert!(!document.text().is_dirty());
        assert!(document.local_source_generation().is_some());
        assert!(registry.notice_repaint_after(Instant::now()).is_some());
    }

    #[test]
    fn background_unchanged_and_ineligible_documents_do_not_wake_or_time_repaint() {
        let directory = TemporaryDirectory::new("background-idle");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        let (checked, checks) = std::sync::mpsc::sync_channel(8);
        let woken = controlled_worker(&mut registry, move |_, save| {
            assert!(!save);
            checked.send(()).unwrap();
            local_io::Result::Checked(local_io::Check::Current(true))
        });
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now(), true);
        for _ in 0..3 {
            checks.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(woken.try_recv().is_err());
            assert!(registry.notice_repaint_after(Instant::now()).is_none());
            assert!(!registry.background_tick(&context, Instant::now(), true));
        }
        registry.get_mut(id).unwrap().conflict =
            Some(festerm_document::ConflictState::new("owned conflict"));
        registry.create_untitled("fixture", "owned fixture", b"untitled\n");
        registry.background_tick(&context, Instant::now(), false);
        assert!(registry.get(id).unwrap().background.is_none());
        assert!(registry.notice_repaint_after(Instant::now()).is_none());
        let mut untitled_only = DocumentRegistry::new();
        untitled_only.create_untitled("fixture", "owned fixture", b"untitled\n");
        assert!(!untitled_only.background_tick(&context, Instant::now(), true));
        assert!(untitled_only.worker.is_none());
        let remote = festerm_document::RemoteOrigin::new(
            "fixture.invalid",
            22,
            festerm_document::RemoteOwner::username("fixture").unwrap(),
            "SHA256:owned-fixture",
            "owned-fixture.md",
            1,
        )
        .unwrap();
        untitled_only.adopt(
            DocumentOrigin::Remote(remote),
            TextDocument::from_bytes(b"owned remote fixture\n", DocumentBounds::DEFAULT).unwrap(),
            false,
        );
        assert!(!untitled_only.background_tick(&context, Instant::now(), true));
        assert!(untitled_only.worker.is_none());
    }

    #[test]
    fn background_reload_cancelled_by_edit_rechecks_into_conflict_without_losing_text() {
        let directory = TemporaryDirectory::new("background-edit-race");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        fs::write(&path, "external version\n").unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let first = std::sync::atomic::AtomicBool::new(true);
        let woken = controlled_worker(&mut registry, move |request, _| {
            let loaded = document_store::load(&request.path, request.bounds).unwrap();
            if first.swap(false, Ordering::AcqRel) {
                entered.send(()).unwrap();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
            local_io::Result::Checked(local_io::Check::Loaded(Box::new(loaded)))
        });
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now(), true);
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        type_into(&mut registry, id, "mine\n");
        release.send(()).unwrap();
        assert!(!wait_completion(&mut registry, &woken));
        assert_eq!(registry.get(id).unwrap().text().text(), "original\nmine\n");
        registry.background_tick(&context, Instant::now(), true);
        assert!(wait_completion(&mut registry, &woken));
        let document = registry.get(id).unwrap();
        assert_eq!(document.text().text(), "original\nmine\n");
        assert_eq!(
            document.conflict().unwrap().source_text(),
            Some("external version\n")
        );
        assert!(!document.auto_save_should_write());
    }

    #[test]
    fn background_late_reload_cannot_mutate_closed_reopened_or_explicitly_reloaded_document() {
        for reopen in [false, true] {
            let directory = TemporaryDirectory::new("background-identity-race");
            let path = directory.file("notes.md", "original\n");
            let mut registry = DocumentRegistry::new();
            let id = registry.open_local(&path).unwrap();
            let (entered, waiting) = std::sync::mpsc::sync_channel(1);
            let (release, blocked) = std::sync::mpsc::sync_channel(1);
            let blocked = Mutex::new(blocked);
            let woken = controlled_worker(&mut registry, move |request, _| {
                let loaded = document_store::load(&request.path, request.bounds).unwrap();
                entered.send(()).unwrap();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                local_io::Result::Checked(local_io::Check::Loaded(Box::new(loaded)))
            });
            let context = egui::Context::default();
            registry.background_tick(&context, Instant::now(), true);
            waiting.recv_timeout(Duration::from_secs(10)).unwrap();
            fs::write(&path, "new identity bytes\n").unwrap();
            let current = if reopen {
                assert!(registry.release(id));
                let reopened = registry.open_local(&path).unwrap();
                assert_ne!(id, reopened);
                reopened
            } else {
                assert_eq!(
                    registry.reload_from_source(id),
                    Some(RefreshOutcome::Reloaded)
                );
                id
            };
            release.send(()).unwrap();
            assert!(!wait_completion(&mut registry, &woken));
            assert_eq!(
                registry.get(current).unwrap().text().text(),
                "new identity bytes\n"
            );
        }
    }

    #[test]
    fn background_capacity_covers_queued_running_and_completed_work_and_recovers_on_close() {
        let directory = TemporaryDirectory::new("background-bound");
        let mut registry = DocumentRegistry::new();
        let mut ids = Vec::new();
        for index in 0..=local_io::CAPACITY {
            ids.push(
                registry
                    .open_local(&directory.file(&format!("{index}.txt"), "owned\n"))
                    .unwrap(),
            );
        }
        let woken = controlled_worker(&mut registry, |request, save| {
            if save {
                controlled_saved(request)
            } else {
                local_io::Result::Checked(local_io::Check::Current(true))
            }
        });
        let context = egui::Context::default();
        assert!(registry.background_tick(&context, Instant::now(), false));
        assert_eq!(
            registry.worker.as_ref().unwrap().admitted(),
            local_io::CAPACITY
        );
        let refused = ids
            .iter()
            .copied()
            .find(|id| registry.get(*id).unwrap().background_error.is_some())
            .unwrap();
        assert!(registry
            .get(refused)
            .unwrap()
            .status()
            .detail()
            .contains("16"));
        type_into(&mut registry, refused, "later\n");
        registry
            .get_mut(refused)
            .unwrap()
            .set_auto_save_requested(true);
        let now = Instant::now();
        let revision = registry.get(refused).unwrap().text().revision();
        registry.get_mut(refused).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        let admitted = ids.into_iter().find(|id| *id != refused).unwrap();
        registry.release(admitted);
        registry.worker.as_ref().unwrap().force();
        woken.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.background_tick(&context, now, false));
        assert!(registry.get(refused).unwrap().background.is_some());
        assert!(
            registry
                .get(refused)
                .unwrap()
                .background
                .as_ref()
                .unwrap()
                .auto_save,
            "a capacity warning cannot create a paused-admission retry loop"
        );
        assert!(registry.get(refused).unwrap().background_error.is_none());
        assert_eq!(
            registry.worker.as_ref().unwrap().admitted(),
            local_io::CAPACITY
        );
        assert!(wait_completion(&mut registry, &woken));
        assert!(!registry.get(refused).unwrap().text().is_dirty());
    }

    #[test]
    fn background_completed_slots_stay_bounded_until_app_consumption_even_after_close() {
        let directory = TemporaryDirectory::new("background-completed-bound");
        let mut registry = DocumentRegistry::new();
        let mut ids = Vec::new();
        for index in 0..=local_io::CAPACITY {
            ids.push(
                registry
                    .open_local(&directory.file(&format!("{index}.txt"), "owned\n"))
                    .unwrap(),
            );
        }
        let woken = controlled_worker(&mut registry, |_, _| {
            local_io::Result::Checked(local_io::Check::Current(false))
        });
        let context = egui::Context::default();
        assert!(registry.background_tick(&context, Instant::now(), true));
        for _ in 0..local_io::CAPACITY {
            woken.recv_timeout(Duration::from_secs(10)).unwrap();
        }
        let refused = ids
            .iter()
            .copied()
            .find(|id| registry.get(*id).unwrap().background_error.is_some())
            .unwrap();
        for id in ids.into_iter().filter(|id| *id != refused) {
            assert!(registry.release(id));
        }
        let document = registry.get(refused).unwrap();
        let DocumentOrigin::Local(origin) = &document.origin else {
            unreachable!()
        };
        let request = local_io::Request::new(
            refused,
            document.io_epoch,
            document.text.revision(),
            document.generation.unwrap(),
            document.source_authority.clone().unwrap(),
            origin.path().to_path_buf(),
            document.availability.clone(),
            None,
            None,
            Instant::now() + POLL_INTERVAL,
            None,
            registry.bounds,
            Arc::new(local_io::Control::default()),
        );
        let worker = registry.worker.as_ref().unwrap();
        assert_eq!(worker.admitted(), local_io::CAPACITY);
        let built = std::sync::atomic::AtomicBool::new(false);
        assert!(
            worker
                .register(|| {
                    built.store(true, Ordering::Release);
                    request
                })
                .is_err(),
            "completed slots remain charged even after all originating views close"
        );
        assert!(
            !built.load(Ordering::Acquire),
            "refusal must not clone document bytes"
        );
        assert!(registry.background_tick(&context, Instant::now(), false));
        assert!(registry.get(refused).unwrap().background_error.is_none());
        assert_eq!(registry.worker.as_ref().unwrap().admitted(), 1);
    }

    #[test]
    fn background_autosave_is_nonblocking_debounced_and_preserves_newer_edit_save_point() {
        let directory = TemporaryDirectory::new("background-save-race");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "first\n");
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let woken = controlled_worker(&mut registry, move |request, save| {
            assert!(save);
            let result = controlled_saved(request);
            entered.send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            result
        });
        let context = egui::Context::default();
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        assert!(!registry.background_tick(&context, now, false));
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!registry.background_tick(&context, Instant::now(), false));
        assert!(registry
            .get(id)
            .unwrap()
            .status()
            .headline()
            .contains("Saving"));
        type_into(&mut registry, id, "second\n");
        assert!(matches!(registry.save(id), Some(SaveOutcome::Failed(_))));
        assert!(registry
            .get(id)
            .unwrap()
            .status()
            .detail()
            .contains("try again"));
        assert!(registry
            .get(id)
            .unwrap()
            .status()
            .actions()
            .contains(&festerm_document::BannerAction::Retry));
        let destination = directory.path.join("retry.txt");
        let confirmed = document_store::observe_destination(&destination).unwrap();
        assert!(matches!(
            registry.save_as(id, &destination, &confirmed),
            Some((SaveOutcome::Failed(_), None))
        ));
        assert!(!destination.exists());
        release.send(()).unwrap();
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\nfirst\n");
        let document = registry.get_mut(id).unwrap();
        assert_eq!(document.text().text(), "original\nfirst\nsecond\n");
        assert!(document.text().is_dirty());
        assert!(document.text_mut().undo());
        assert!(
            !document.text().is_dirty(),
            "undo reaches the exact published save point"
        );
        assert!(document.text_mut().undo());
        assert!(
            document.text().is_dirty(),
            "the original baseline is no longer what disk holds"
        );
        let confirmed = document_store::observe_destination(&destination).unwrap();
        assert!(matches!(
            registry.save_as(id, &destination, &confirmed),
            Some((SaveOutcome::Saved, Some(_)))
        ));
        assert_eq!(fs::read_to_string(destination).unwrap(), "original\n");
    }

    #[test]
    fn background_autosave_error_is_truthful_and_not_retried_for_the_same_revision() {
        let directory = TemporaryDirectory::new("background-save-error");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executed = Arc::clone(&calls);
        let woken = controlled_worker(&mut registry, move |_, save| {
            if save {
                executed.fetch_add(1, Ordering::AcqRel);
                local_io::Result::Saved(Err(SaveFailure::MetadataPreservation), None)
            } else {
                local_io::Result::Checked(local_io::Check::Current(true))
            }
        });
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        let context = egui::Context::default();
        registry.background_tick(&context, now, false);
        assert!(wait_completion(&mut registry, &woken));
        assert!(registry.get(id).unwrap().text().is_dirty());
        assert!(registry
            .get(id)
            .unwrap()
            .status()
            .detail()
            .contains("Save As"));
        registry.background_tick(&context, now + AUTO_SAVE_IDLE * 20, false);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        type_into(&mut registry, id, "new information\n");
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        // The production tick normally notices the new revision itself.
        registry.get_mut(id).unwrap().settled = Some((revision - 1, now - AUTO_SAVE_IDLE));
        registry.background_tick(&context, now - AUTO_SAVE_IDLE, false);
        // The worker owns the deadline; another tick could consume its
        // already-published error before this test drains the completion.
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(calls.load(Ordering::Acquire), 2);
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn background_late_save_error_does_not_pause_already_registered_newer_edits() {
        let directory = TemporaryDirectory::new("background-late-error");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "first\n");
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let woken = controlled_worker(&mut registry, move |request, save| {
            assert!(save);
            let attempt = calls.fetch_add(1, Ordering::AcqRel);
            entered.send(attempt).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            if attempt == 0 {
                local_io::Result::Saved(Err(SaveFailure::MetadataPreservation), None)
            } else {
                assert!(!request.control.cancelled.load(Ordering::Acquire));
                controlled_saved(request)
            }
        });
        let context = egui::Context::default();
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&context, now, false);
        assert_eq!(waiting.recv_timeout(Duration::from_secs(10)).unwrap(), 0);
        type_into(&mut registry, id, "newer\n");
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&context, now, false);
        release.send(()).unwrap();
        assert_eq!(waiting.recv_timeout(Duration::from_secs(10)).unwrap(), 1);
        woken.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.background_tick(&context, Instant::now(), false));
        let document = registry.get(id).unwrap();
        assert!(document.last_error.is_none());
        assert!(document.auto_save_blocked_at.is_none());
        assert!(!document
            .background
            .as_ref()
            .unwrap()
            .control
            .cancelled
            .load(Ordering::Acquire));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        release.send(()).unwrap();
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "original\nfirst\nnewer\n"
        );
        assert!(!registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn background_completed_save_wakes_after_releasing_write_admission() {
        let directory = TemporaryDirectory::new("background-save-wake");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        let (wake, woken) = std::sync::mpsc::sync_channel(8);
        let gate = Arc::clone(&registry.write_gate);
        registry.worker = Some(
            local_io::Worker::start_with(
                Arc::new(move || {
                    wake.try_send(gate.try_lock().is_ok()).unwrap();
                }),
                Arc::clone(&registry.write_gate),
                Arc::new(|request, save| {
                    assert!(save);
                    controlled_saved(request)
                }),
            )
            .unwrap(),
        );
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&egui::Context::default(), now, false);
        assert!(
            woken.recv_timeout(Duration::from_secs(10)).unwrap(),
            "a completed save must release write admission before waking the app"
        );
        assert!(registry.drain_background());
        assert!(!registry.get(id).unwrap().text().is_dirty());
        assert_eq!(fs::read_to_string(path).unwrap(), "original\nmine\n");
    }

    #[test]
    fn background_real_missing_source_reports_once_and_unchanged_missing_does_not_wake() {
        let directory = TemporaryDirectory::new("background-gone");
        let path = directory.file("notes.md", "owned\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        let (wake, woken) = std::sync::mpsc::sync_channel(8);
        registry.worker = Some(
            local_io::Worker::start(
                Arc::new(move || {
                    wake.try_send(()).unwrap();
                }),
                Arc::clone(&registry.write_gate),
            )
            .unwrap(),
        );
        fs::remove_file(&path).unwrap();
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now(), true);
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(
            registry.get(id).unwrap().availability,
            Availability::Unavailable(UnavailableReason::Missing)
        );
        assert_eq!(registry.get(id).unwrap().text().text(), "owned\n");
        assert!(!registry.background_tick(&context, Instant::now(), true));
        // A subsequent real change, rather than a timer repaint, wakes again.
        fs::write(&path, "returned longer\n").unwrap();
        // The preceding tick registered the missing source. Let its real
        // worker publish the change before the app drains that completion.
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(registry.get(id).unwrap().text().text(), "returned longer\n");
    }

    #[test]
    fn background_recovery_drained_before_reopen_still_refuses_all_same_source_writes() {
        let directory = TemporaryDirectory::new("background-closed-recovery");
        let path = directory.file("notes.md", "original\n");
        let alias = directory.path.join("original-alias.md");
        fs::hard_link(&path, &alias).unwrap();
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Arc::new(Mutex::new(blocked));
        let target = path.clone();
        let replacement = directory.path.join("replacement.md");
        let woken = controlled_worker(&mut registry, move |request, save| {
            assert!(save);
            let entered = entered.clone();
            let blocked = Arc::clone(&blocked);
            let target = target.clone();
            let replacement = replacement.clone();
            document_store::set_after_save_replacement_hook(move || {
                entered.send(()).unwrap();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
                #[cfg(not(windows))]
                {
                    fs::write(&replacement, "later\n").unwrap();
                    fs::rename(&replacement, &target).unwrap();
                }
                #[cfg(windows)]
                {
                    let _ = replacement;
                    let mut permissions = fs::metadata(&target).unwrap().permissions();
                    permissions.set_readonly(true);
                    fs::set_permissions(&target, permissions).unwrap();
                }
            });
            local_io::execute(request, save)
        });
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&egui::Context::default(), now, false);
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.release(id));
        release.send(()).unwrap();
        assert!(wait_completion(&mut registry, &woken));
        assert!(registry.is_empty());
        let (recovery, error) = registry.take_recovery_notice().unwrap();
        assert!(recovery.is_dir());
        let original = fs::read(recovery.join("original")).unwrap();
        let prepared = fs::read(recovery.join("prepared")).unwrap();
        let reopened = registry.open_local(&path).unwrap();
        assert!(registry.acknowledge_recovery_notice(&recovery));
        assert!(!registry.has_recovery_notices());
        type_into(&mut registry, reopened, "newer\n");
        let before = fs::read(&path).unwrap();
        assert_eq!(
            registry.get(reopened).unwrap().recovery_path(),
            Some(recovery.as_path())
        );
        assert_eq!(
            registry.save(reopened),
            Some(SaveOutcome::Failed(error.clone()))
        );
        let confirmed = document_store::observe_destination(&path).unwrap();
        assert_eq!(
            registry.save_as(reopened, &path, &confirmed),
            Some((SaveOutcome::Failed(error.clone()), None))
        );
        registry
            .get_mut(reopened)
            .unwrap()
            .set_auto_save_requested(true);
        let revision = registry.get(reopened).unwrap().text().revision();
        registry.get_mut(reopened).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&egui::Context::default(), now, true);
        assert!(registry.get(reopened).unwrap().background.is_none());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert_eq!(registry.get(reopened).unwrap().recovery_error, Some(error));
        let aliased = registry.open_local(&alias).unwrap();
        assert_eq!(
            registry.get(aliased).unwrap().recovery_path(),
            Some(recovery.as_path())
        );
        assert!(matches!(
            registry.save(aliased),
            Some(SaveOutcome::Failed(_))
        ));
        assert!(registry.release(aliased));
        let confirmed_alias = document_store::observe_destination(&alias).unwrap();
        assert!(matches!(
            registry.save_as(reopened, &alias, &confirmed_alias),
            Some((SaveOutcome::Failed(_), None))
        ));
        assert_eq!(fs::read(recovery.join("original")).unwrap(), original);
        assert_eq!(fs::read(recovery.join("prepared")).unwrap(), prepared);
        assert!(recovery.is_dir());
    }

    #[test]
    fn unresolved_recovery_protection_is_bounded_and_overflow_refuses_all_local_writes() {
        let directory = TemporaryDirectory::new("recovery-protection-bound");
        let unrelated = directory.file("unrelated.md", "untouched\n");
        let mut registry = DocumentRegistry::new();
        let existing = registry.open_local(&unrelated).unwrap();
        for index in 0..=MAX_RECOVERY_PROTECTIONS {
            let path = directory.file(&format!("source-{index}.md"), "owned\n");
            let loaded = document_store::load(&path, registry.bounds()).unwrap();
            let breadcrumb = directory.path.join(format!("recovery-{index}"));
            let failure = SaveFailure::RecoveryRequired(breadcrumb.clone());
            registry.retain_recovery_protection(
                path,
                loaded.source_authority.recovery_authority(),
                breadcrumb.clone(),
                SaveError::new(failure.headline(), failure.detail()),
            );
            assert_eq!(registry.take_recovery_notice().unwrap().0, breadcrumb);
            assert!(registry.acknowledge_recovery_notice(&breadcrumb));
        }
        assert_eq!(
            registry.recovery_protections.len(),
            MAX_RECOVERY_PROTECTIONS
        );
        assert!(!registry.has_recovery_notices());
        let overflow = registry.recovery_protection_overflow.clone().unwrap();
        assert_eq!(overflow.0, directory.path.join("recovery-64"));
        assert!(overflow.1.detail().contains("All local writes are refused"));
        type_into(&mut registry, existing, "new\n");
        assert_eq!(
            registry.save(existing),
            Some(SaveOutcome::Failed(overflow.1.clone()))
        );
        registry
            .get_mut(existing)
            .unwrap()
            .set_auto_save_requested(true);
        registry.background_tick(
            &egui::Context::default(),
            Instant::now() + AUTO_SAVE_IDLE,
            true,
        );
        assert!(registry.get(existing).unwrap().background.is_none());
        assert!(registry.release(existing));
        let reopened = registry.open_local(&unrelated).unwrap();
        assert_eq!(
            registry.get(reopened).unwrap().recovery_error,
            Some(overflow.1.clone())
        );
        let destination = directory.path.join("new.md");
        let confirmed = document_store::observe_destination(&destination).unwrap();
        assert_eq!(
            registry.save_as(reopened, &destination, &confirmed),
            Some((SaveOutcome::Failed(overflow.1), None))
        );
        assert!(!destination.exists());
        assert_eq!(fs::read_to_string(&unrelated).unwrap(), "untouched\n");
        let protected = registry
            .open_local(&directory.path.join("source-0.md"))
            .unwrap();
        assert_eq!(
            registry.get(protected).unwrap().recovery_path(),
            Some(directory.path.join("recovery-0").as_path())
        );
    }

    #[test]
    fn background_recovery_completion_survives_close_and_is_not_silently_discarded() {
        let directory = TemporaryDirectory::new("background-recovery");
        let path = directory.file("notes.md", "original\n");
        let recovery = directory.path.join("owned-recovery");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        type_into(&mut registry, id, "mine\n");
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let returned_recovery = recovery.clone();
        let woken = controlled_worker(&mut registry, move |_, save| {
            assert!(save);
            entered.send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            local_io::Result::Saved(
                Err(SaveFailure::RecoveryRequired(returned_recovery.clone())),
                None,
            )
        });
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&egui::Context::default(), now, false);
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.release(id));
        let reopened = registry.open_local(&path).unwrap();
        release.send(()).unwrap();
        assert!(wait_completion(&mut registry, &woken));
        assert_eq!(registry.take_recovery_notice().unwrap().0, recovery);
        assert!(registry.get(reopened).unwrap().recovery_error.is_some());
        assert!(!registry.get(reopened).unwrap().status().can_save());
    }

    #[test]
    fn background_poll_does_not_refuse_save_as_and_cancelled_queued_autosave_never_writes() {
        let directory = TemporaryDirectory::new("background-manual");
        let path = directory.file("blocking.txt", "read fixture\n");
        let mut registry = DocumentRegistry::new();
        registry.open_local(&path).unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executed = Arc::clone(&writes);
        let woken = controlled_worker(&mut registry, move |_, save| {
            if save {
                executed.fetch_add(1, Ordering::AcqRel);
            }
            assert!(!save, "cancelled queued work must not reach the executor");
            entered.send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            local_io::Result::Checked(local_io::Check::Failed(LoadFailure::Unreadable))
        });
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now(), true);
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        let source = directory.file("source.txt", "original\n");
        let id = registry.open_local(&source).unwrap();
        type_into(&mut registry, id, "latest\n");
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&context, now, false);
        let destination = directory.path.join("new.txt");
        let confirmed = document_store::observe_destination(&destination).unwrap();
        let (_, new_id) = registry.save_as(id, &destination, &confirmed).unwrap();
        assert!(new_id.is_some());
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "original\nlatest\n"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "original\n");
        release.send(()).unwrap();
        assert!(!wait_completion(&mut registry, &woken));
        assert_eq!(writes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn background_autosave_debounce_coalesces_edits_and_disable_cancels_a_running_prepare() {
        let directory = TemporaryDirectory::new("background-cancel-save");
        let path = directory.file("notes.md", "original\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "first\n");
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let (checked, checks) = std::sync::mpsc::sync_channel(2);
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::sync_channel(1);
        let blocked = Mutex::new(blocked);
        let woken = controlled_worker(&mut registry, move |request, save| {
            if !save {
                checked.send(()).unwrap();
                return local_io::Result::Checked(local_io::Check::Current(true));
            }
            assert_eq!(
                request.bytes.as_deref(),
                Some(&b"original\nfirst\nlatest\n"[..])
            );
            entered.send(()).unwrap();
            blocked
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            assert!(request.control.cancelled.load(Ordering::Acquire));
            local_io::Result::Saved(Err(SaveFailure::Interrupted), None)
        });
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now() + Duration::from_secs(3600), true);
        checks.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            waiting.try_recv().is_err(),
            "focus freshness does not bypass the typing debounce"
        );
        type_into(&mut registry, id, "latest\n");
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&context, now, false);
        waiting.recv_timeout(Duration::from_secs(10)).unwrap();
        registry.get_mut(id).unwrap().set_auto_save_requested(false);
        release.send(()).unwrap();
        assert!(!wait_completion(&mut registry, &woken));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        assert!(registry.get(id).unwrap().text().is_dirty());
        assert!(registry.get(id).unwrap().last_error.is_none());
    }

    #[test]
    fn background_stable_unavailable_and_read_errors_do_not_repeat_repaints() {
        for unreadable in [false, true] {
            let directory = TemporaryDirectory::new("background-stable-error");
            let path = directory.file("notes.md", "owned\n");
            let mut registry = DocumentRegistry::new();
            let id = registry.open_local(&path).unwrap();
            if unreadable {
                registry.get_mut(id).unwrap().background_load_error = Some(LoadFailure::Unreadable);
            } else {
                registry.get_mut(id).unwrap().availability =
                    Availability::Unavailable(UnavailableReason::Missing);
            }
            let (checked, checks) = std::sync::mpsc::sync_channel(4);
            let woken = controlled_worker(&mut registry, move |_, _| {
                checked.send(()).unwrap();
                local_io::Result::Checked(local_io::Check::Failed(if unreadable {
                    LoadFailure::Unreadable
                } else {
                    LoadFailure::NotFound
                }))
            });
            let context = egui::Context::default();
            registry.background_tick(&context, Instant::now(), true);
            for _ in 0..3 {
                checks.recv_timeout(Duration::from_secs(10)).unwrap();
                assert!(woken.try_recv().is_err());
                assert!(!registry.background_tick(&context, Instant::now(), true));
            }
        }
    }

    #[test]
    fn background_write_panic_refuses_explicit_save_with_truthful_coordination_error() {
        let directory = TemporaryDirectory::new("background-write-panic");
        let path = directory.file("notes.md", "owned\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "edited\n");
        registry.get_mut(id).unwrap().set_auto_save_requested(true);
        let woken = controlled_worker(&mut registry, |_, save| {
            assert!(save);
            panic!("controlled publication failure");
        });
        let now = Instant::now();
        let revision = registry.get(id).unwrap().text().revision();
        registry.get_mut(id).unwrap().settled = Some((revision, now - AUTO_SAVE_IDLE));
        registry.background_tick(&egui::Context::default(), now, false);
        woken.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.drain_background());
        assert!(matches!(registry.save(id), Some(SaveOutcome::Failed(_))));
        assert!(registry
            .get(id)
            .unwrap()
            .last_error
            .as_ref()
            .unwrap()
            .detail()
            .contains("restart"));
        let destination = directory.path.join("new.txt");
        let confirmed = document_store::observe_destination(&destination).unwrap();
        assert!(matches!(
            registry.save_as(id, &destination, &confirmed),
            Some((SaveOutcome::Failed(_), None))
        ));
        assert!(!destination.exists());
        assert_eq!(fs::read_to_string(&path).unwrap(), "owned\n");
        assert_eq!(registry.get(id).unwrap().text().text(), "owned\nedited\n");
        assert!(registry.get(id).unwrap().text().is_dirty());
    }

    #[test]
    fn background_worker_failure_wakes_and_surfaces_an_error_instead_of_default_success() {
        let directory = TemporaryDirectory::new("background-worker-failure");
        let path = directory.file("notes.md", "owned\n");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        let woken = controlled_worker(&mut registry, |_, _| panic!("controlled worker failure"));
        let context = egui::Context::default();
        registry.background_tick(&context, Instant::now(), true);
        woken.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(registry.drain_background());
        assert!(registry
            .get(id)
            .unwrap()
            .status()
            .detail()
            .contains("worker stopped"));
        assert_eq!(registry.get(id).unwrap().text().text(), "owned\n");
        assert!(!registry.background_tick(&context, Instant::now(), false));
    }
}
