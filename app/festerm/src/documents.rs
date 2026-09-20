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
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use festerm_document::{
    AutoSaveControl, Availability, DocumentBounds, DocumentId, DocumentKey, DocumentOrigin,
    DocumentStatus, LocalOrigin, OriginError, SaveError, SaveOutcome, SaveProgress, StatusInputs,
    TextDocument, UnavailableReason,
};

use festerm_syntax::{DocumentSyntax, SyntaxStatus};

use crate::document_store::{self, Freshness, Generation, LoadFailure, SaveFailure};

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

/// The application-scoped registry handle every window holds.
pub(crate) type SharedDocuments = Rc<RefCell<DocumentRegistry>>;

/// One open document and everything true about it that is not presentation.
#[derive(Debug)]
pub(crate) struct OpenDocument {
    origin: DocumentOrigin,
    text: TextDocument,
    generation: Option<Generation>,
    views: usize,
    read_only: bool,
    availability: Availability,
    conflict: Option<festerm_document::ConflictState>,
    save: SaveProgress,
    auto_save_requested: bool,
    last_error: Option<SaveError>,
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
}

impl OpenDocument {
    pub(crate) const fn origin(&self) -> &DocumentOrigin {
        &self.origin
    }

    pub(crate) const fn text(&self) -> &TextDocument {
        &self.text
    }

    pub(crate) const fn text_mut(&mut self) -> &mut TextDocument {
        &mut self.text
    }

    pub(crate) const fn views(&self) -> usize {
        self.views
    }

    pub(crate) const fn read_only(&self) -> bool {
        self.read_only
    }

    pub(crate) const fn auto_save_requested(&self) -> bool {
        self.auto_save_requested
    }

    pub(crate) const fn set_auto_save_requested(&mut self, requested: bool) {
        self.auto_save_requested = requested;
    }

    /// Dismisses a conflict banner without writing anything: the user has
    /// decided their version is the one they want to keep for now (ADR 0034
    /// §6).
    pub(crate) fn keep_my_version(&mut self) {
        self.conflict = None;
    }

    pub(crate) fn conflict(&self) -> Option<&festerm_document::ConflictState> {
        self.conflict.as_ref()
    }

    /// The derived status every surface reads from, so the banner, the Save
    /// button, the chip, and the status bar cannot disagree.
    pub(crate) fn status(&self) -> DocumentStatus {
        DocumentStatus::derive(&StatusInputs {
            dirty: self.text.is_dirty(),
            save: self.save,
            availability: self.availability.clone(),
            conflict: self.conflict.clone(),
            auto_save_requested: self.auto_save_requested,
            last_error: self.last_error.clone(),
            remote: self.origin.is_remote(),
            recently_reloaded: self.reloaded.is_some_and(|at| at.elapsed() < RELOAD_NOTICE),
        })
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

    /// Why this document has no colour, when it has none.
    pub(crate) fn syntax_status(&self) -> SyntaxStatus {
        self.syntax.status()
    }

    /// Whether a debounced auto-save should actually write right now.
    pub(crate) fn auto_save_should_write(&self) -> bool {
        self.text.is_dirty() && self.status().auto_save() == AutoSaveControl::On
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

/// Every open document, owned by the application rather than by a window.
#[derive(Debug, Default)]
pub(crate) struct DocumentRegistry {
    documents: HashMap<DocumentId, OpenDocument>,
    by_key: HashMap<DocumentKey, DocumentId>,
    next_id: u64,
    bounds: DocumentBounds,
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
        let origin = LocalOrigin::new(path)
            .map(DocumentOrigin::Local)
            .map_err(OpenFailure::Origin)?;
        if let Some(id) = self.by_key.get(&origin.key()).copied() {
            self.retain(id);
            return Ok(id);
        }

        let loaded = document_store::load(path, self.bounds).map_err(OpenFailure::Load)?;
        Ok(self.insert(
            origin,
            loaded.document,
            Some(loaded.generation),
            loaded.read_only,
        ))
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
        self.insert(origin, text, None, read_only)
    }

    fn insert(
        &mut self,
        origin: DocumentOrigin,
        text: TextDocument,
        generation: Option<Generation>,
        read_only: bool,
    ) -> DocumentId {
        self.next_id += 1;
        let id = DocumentId::from_raw(self.next_id);
        let syntax = DocumentSyntax::new(origin.file_name(), text.text());
        self.by_key.insert(origin.key(), id);
        self.documents.insert(
            id,
            OpenDocument {
                syntax,
                origin,
                text,
                generation,
                views: 1,
                read_only,
                availability: Availability::Available,
                conflict: None,
                save: SaveProgress::Idle,
                auto_save_requested: false,
                last_error: None,
                checked: Instant::now(),
                reloaded: None,
                settled: None,
                auto_save_blocked_at: None,
            },
        );
        id
    }

    /// Records one more view of an already-open document.
    pub(crate) fn retain(&mut self, id: DocumentId) {
        if let Some(document) = self.documents.get_mut(&id) {
            document.views += 1;
        }
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
        let key = document.origin.key();
        self.documents.remove(&id);
        self.by_key.remove(&key);
        true
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

    /// Writes a local document back to its origin, revalidating first.
    ///
    /// A conflict is not an error the user has to interpret: the source's text
    /// is captured with it so Compare can open immediately and Reload can
    /// apply exactly the bytes the conflict was raised for.
    pub(crate) fn save(&mut self, id: DocumentId) -> Option<SaveOutcome> {
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        let DocumentOrigin::Local(origin) = &document.origin else {
            // Remote saves travel through the SFTP worker and complete
            // asynchronously; they do not run on this path.
            return None;
        };
        let path = origin.path().to_path_buf();
        let bytes = document.text.to_bytes();

        let outcome = match document_store::save(&path, &bytes, document.generation) {
            Ok(generation) => {
                document.generation = Some(generation);
                document.text.mark_saved();
                document.conflict = None;
                document.last_error = None;
                document.availability = Availability::Available;
                SaveOutcome::Saved
            }
            Err(SaveFailure::Conflict(_)) => {
                let conflict = conflict_for(&path, bounds);
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
            Err(failure) => {
                let error = SaveError::new(failure.headline(), failure.detail());
                document.last_error = Some(error.clone());
                SaveOutcome::Failed(error)
            }
        };
        document.save = SaveProgress::Idle;
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
    /// exists to prevent (§1), so the open document is reloaded from the bytes
    /// just written rather than a second copy being made.
    pub(crate) fn save_as(
        &mut self,
        id: DocumentId,
        path: &Path,
    ) -> Option<(SaveOutcome, Option<DocumentId>)> {
        let origin = match LocalOrigin::new(path).map(DocumentOrigin::Local) {
            Ok(origin) => origin,
            Err(_) => {
                let error = SaveError::new(
                    "That destination cannot be used",
                    "The chosen path is not a file this host can write to.",
                );
                return Some((SaveOutcome::Failed(error), None));
            }
        };
        let document = self.documents.get(&id)?;
        let bytes = document.text.to_bytes();
        let text = document.text.clone();

        let generation = match document_store::save(path, &bytes, None) {
            Ok(generation) => generation,
            Err(SaveFailure::Gone) => {
                return Some((SaveOutcome::Unavailable(UnavailableReason::Missing), None));
            }
            Err(SaveFailure::PermissionDenied) => {
                return Some((
                    SaveOutcome::Unavailable(UnavailableReason::PermissionDenied),
                    None,
                ));
            }
            Err(failure) => {
                let error = SaveError::new(failure.headline(), failure.detail());
                return Some((SaveOutcome::Failed(error), None));
            }
        };

        if let Some(existing) = self.by_key.get(&origin.key()).copied() {
            // The file the user chose is open elsewhere, and it now holds the
            // bytes just written, so that document is brought up to date
            // rather than shadowed.
            self.reload_from_source(existing);
            self.retain(existing);
            return Some((SaveOutcome::Saved, Some(existing)));
        }

        let mut text = text;
        text.mark_saved();
        let new_id = self.insert(origin, text, Some(generation), false);
        Some((SaveOutcome::Saved, Some(new_id)))
    }

    /// Writes every document whose Auto-save is on and whose typing has
    /// settled, and returns what happened to each one that was attempted.
    ///
    /// This is the whole of Auto-save's scheduling: one debounce per document,
    /// coalesced by construction because a write is only considered once the
    /// content has stopped changing. Failures are not retried on the clock --
    /// the document stays dirty, keeps its error, and is only reconsidered
    /// once the user edits again, which is the only new information there is
    /// (ADR 0034 §7).
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

    /// Re-checks everything now, whatever the poll clock says. Used when a
    /// window regains focus: the user has just come back from whatever
    /// changed the file.
    pub(crate) fn revalidate_all(&mut self) -> Vec<(DocumentId, RefreshOutcome)> {
        let ids: Vec<DocumentId> = self
            .documents
            .iter()
            .filter(|(_, document)| {
                document.save == SaveProgress::Idle && document.conflict.is_none()
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
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        let DocumentOrigin::Local(origin) = &document.origin else {
            return None;
        };
        let path = origin.path().to_path_buf();
        let Some(known) = document.generation else {
            return Some(RefreshOutcome::Unchanged);
        };

        document.checked = Instant::now();
        let outcome = match document_store::freshness(&path, known) {
            Freshness::Unchanged => RefreshOutcome::Unchanged,
            Freshness::Changed(_) => match document_store::load(&path, bounds) {
                Ok(loaded) if !document.text.is_dirty() => {
                    document.text = loaded.document;
                    document.generation = Some(loaded.generation);
                    document.read_only = loaded.read_only;
                    document.availability = Availability::Available;
                    document.conflict = None;
                    document.reloaded = Some(Instant::now());
                    RefreshOutcome::Reloaded
                }
                Ok(loaded) => {
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
        let bounds = self.bounds;
        let document = self.documents.get_mut(&id)?;
        let DocumentOrigin::Local(origin) = &document.origin else {
            return None;
        };
        let path = origin.path().to_path_buf();
        match document_store::load(&path, bounds) {
            Ok(loaded) => {
                document.text = loaded.document;
                document.generation = Some(loaded.generation);
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
}

fn conflict_for(path: &Path, bounds: DocumentBounds) -> festerm_document::ConflictState {
    let conflict = festerm_document::ConflictState::new(
        "This file changed somewhere else while you were editing it.",
    );
    match document_store::load(path, bounds) {
        Ok(loaded) => conflict.with_source_text(loaded.document.text()),
        Err(_) => conflict,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use festerm_document::Severity;

    use super::*;

    struct TemporaryDirectory {
        path: PathBuf,
    }

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
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
    fn saving_somewhere_else_writes_there_and_leaves_the_original_alone() {
        let directory = TemporaryDirectory::new("save-as");
        let path = directory.file("notes.md", "alpha\n");
        let destination = directory.path.join("copy.md");
        let mut registry = DocumentRegistry::new();
        let id = registry.open_local(&path).unwrap();
        type_into(&mut registry, id, "beta\n");

        let (outcome, moved) = registry.save_as(id, &destination).unwrap();

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

        let (outcome, moved) = registry.save_as(id, &destination).unwrap();

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
}
