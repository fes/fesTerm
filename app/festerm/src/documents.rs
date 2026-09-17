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

use festerm_document::{
    AutoSaveControl, Availability, DocumentBounds, DocumentId, DocumentKey, DocumentOrigin,
    DocumentStatus, LocalOrigin, OriginError, SaveError, SaveOutcome, SaveProgress, StatusInputs,
    TextDocument, UnavailableReason,
};

use crate::document_store::{self, Freshness, Generation, LoadFailure, SaveFailure};

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
        })
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
        self.by_key.insert(origin.key(), id);
        self.documents.insert(
            id,
            OpenDocument {
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

    pub(crate) fn len(&self) -> usize {
        self.documents.len()
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

        let outcome = match document_store::freshness(&path, known) {
            Freshness::Unchanged => RefreshOutcome::Unchanged,
            Freshness::Changed(_) => match document_store::load(&path, bounds) {
                Ok(loaded) if !document.text.is_dirty() => {
                    document.text = loaded.document;
                    document.generation = Some(loaded.generation);
                    document.read_only = loaded.read_only;
                    document.availability = Availability::Available;
                    document.conflict = None;
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
    fn a_file_too_large_to_edit_is_refused_with_its_limit() {
        let directory = TemporaryDirectory::new("bounds");
        let path = directory.file("big.txt", &"x".repeat(512));
        let mut registry = DocumentRegistry::with_bounds(DocumentBounds::new(64, 16, 64));

        let failure = registry.open_local(&path).unwrap_err();

        assert_eq!(failure.headline(), "This file is too large to edit");
        assert_eq!(registry.len(), 0);
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
}
