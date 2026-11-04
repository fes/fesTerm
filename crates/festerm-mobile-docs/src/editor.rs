//! Mobile ownership over the shared document engine (ADR 0067).
//! Native file authorities stay outside this registry; display paths grant no access.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use festerm_document::{
    normalise_line_endings, DocumentBounds, DocumentId, DocumentKey, DocumentOrigin, Encoding,
    LineEnding, RefusalReason, SavePoint, TextDocument,
};

pub const MAX_DOCUMENTS: usize = 8;
pub const MAX_VIEWS: usize = 24;
pub const MOBILE_DOCUMENT_BOUNDS: DocumentBounds =
    DocumentBounds::new(crate::MOBILE_MAX_SOURCE_BYTES, 16_384, 16 * 1024);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ViewId(u64);

impl ViewId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Supplied by an authorized source, over its identity and exact encoded bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceVersion(pub [u8; 32]);

pub struct SourceSnapshot {
    pub bytes: Vec<u8>,
    pub version: SourceVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentError {
    Closed,
    ReadOnly,
    Unavailable,
    Stale,
    InvalidSelection,
    Conflict,
    Busy,
    Capacity,
    Refused(RefusalReason),
}

impl DocumentError {
    pub fn message(&self) -> String {
        match self {
            Self::Closed => "This document view is closed. Open the document again.".to_owned(),
            Self::ReadOnly => {
                "This source is read-only. Save an editable local copy first.".to_owned()
            }
            Self::Unavailable => {
                "Access to this file is unavailable. Reopen it through Files or save a local copy."
                    .to_owned()
            }
            Self::Stale => {
                "The document, selection or source changed. Review it and try again.".to_owned()
            }
            Self::InvalidSelection => "This selection no longer names valid text.".to_owned(),
            Self::Conflict => {
                "The file changed outside fesTerm. Compare, reload, keep your edits or save a copy."
                    .to_owned()
            }
            Self::Busy => "A save is still finishing. Wait and try again.".to_owned(),
            Self::Capacity => {
                "The mobile limit is eight documents and twenty-four views. Close a view and retry."
                    .to_owned()
            }
            Self::Refused(reason) => format!("{}. {}", reason.headline(), reason.detail()),
        }
    }
}

impl From<RefusalReason> for DocumentError {
    fn from(reason: RefusalReason) -> Self {
        Self::Refused(reason)
    }
}

pub struct ExternalChange {
    snapshot: SourceSnapshot,
    kept_edits: bool,
}

impl ExternalChange {
    pub fn bytes(&self) -> &[u8] {
        &self.snapshot.bytes
    }

    pub fn kept_edits(&self) -> bool {
        self.kept_edits
    }
}

pub struct OpenDocument {
    origin: DocumentOrigin,
    text: TextDocument,
    version: Option<SourceVersion>,
    writable: bool,
    available: bool,
    source_epoch: u64,
    access_epoch: u64,
    conflict: Option<ExternalChange>,
    saving: Option<u64>,
}

impl OpenDocument {
    pub fn origin(&self) -> &DocumentOrigin {
        &self.origin
    }

    pub fn text(&self) -> &TextDocument {
        &self.text
    }

    pub fn dirty(&self) -> bool {
        self.text.is_dirty() || self.conflict.is_some()
    }

    pub fn writable(&self) -> bool {
        self.writable && self.available && !self.origin.is_remote()
    }

    pub fn available(&self) -> bool {
        self.available
    }

    pub fn conflict(&self) -> Option<&ExternalChange> {
        self.conflict.as_ref()
    }

    pub fn saving(&self) -> bool {
        self.saving.is_some()
    }

    fn require_writable(&self) -> Result<(), DocumentError> {
        if !self.available {
            Err(DocumentError::Unavailable)
        } else if !self.writable() {
            Err(DocumentError::ReadOnly)
        } else {
            Ok(())
        }
    }
}

/// Presentation supplies both the captured and current target before delayed commits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditTarget {
    pub view: ViewId,
    pub document: DocumentId,
    pub revision: u64,
    pub selection: Range<usize>,
    origin: DocumentKey,
    source_epoch: u64,
    access_epoch: u64,
}

#[derive(Clone, Debug)]
pub struct ReadTicket {
    document: DocumentId,
    source_epoch: u64,
    access_epoch: u64,
    revision: u64,
    version: Option<SourceVersion>,
}

#[derive(Clone, Debug)]
pub struct SaveTicket {
    document: DocumentId,
    source_epoch: u64,
    operation: u64,
    point: SavePoint,
    pub expected: SourceVersion,
}

#[derive(Debug, Eq, PartialEq)]
pub enum CloseOutcome {
    NeedsConfirmation,
    Closed { released: Option<DocumentId> },
}

#[derive(Default)]
pub struct Documents {
    documents: BTreeMap<DocumentId, OpenDocument>,
    origins: HashMap<DocumentKey, DocumentId>,
    views: BTreeMap<ViewId, DocumentId>,
    next_document: u64,
    next_view: u64,
    next_operation: u64,
}

impl Documents {
    pub fn open(
        &mut self,
        origin: DocumentOrigin,
        bytes: &[u8],
        version: Option<SourceVersion>,
        writable: bool,
    ) -> Result<ViewId, DocumentError> {
        if let Some(document) = self.origins.get(&origin.key()).copied() {
            return self.duplicate_document(document);
        }
        if self.documents.len() >= MAX_DOCUMENTS || self.views.len() >= MAX_VIEWS {
            return Err(DocumentError::Capacity);
        }
        let text = if matches!(origin, DocumentOrigin::Untitled(_)) {
            TextDocument::from_unsaved_bytes(bytes, MOBILE_DOCUMENT_BOUNDS)?
        } else {
            TextDocument::from_bytes(bytes, MOBILE_DOCUMENT_BOUNDS)?
        };
        let document = DocumentId::from_raw(
            self.next_document
                .checked_add(1)
                .ok_or(DocumentError::Capacity)?,
        );
        let next_view = self
            .next_view
            .checked_add(1)
            .ok_or(DocumentError::Capacity)?;
        self.next_document = document.raw();
        self.origins.insert(origin.key(), document);
        self.documents.insert(
            document,
            OpenDocument {
                writable: writable && !origin.is_remote(),
                origin,
                text,
                version,
                available: true,
                source_epoch: 0,
                access_epoch: 0,
                conflict: None,
                saving: None,
            },
        );
        self.next_view = next_view;
        let view = ViewId(next_view);
        self.views.insert(view, document);
        Ok(view)
    }

    pub fn get(&self, document: DocumentId) -> Result<&OpenDocument, DocumentError> {
        self.documents.get(&document).ok_or(DocumentError::Closed)
    }

    pub fn document_for(&self, view: ViewId) -> Result<DocumentId, DocumentError> {
        self.views.get(&view).copied().ok_or(DocumentError::Closed)
    }

    pub fn view(&self, view: ViewId) -> Result<&OpenDocument, DocumentError> {
        self.get(self.document_for(view)?)
    }

    pub fn views(&self) -> impl Iterator<Item = (ViewId, DocumentId)> + '_ {
        self.views.iter().map(|(view, document)| (*view, *document))
    }

    pub fn duplicate(&mut self, view: ViewId) -> Result<ViewId, DocumentError> {
        self.duplicate_document(self.document_for(view)?)
    }

    fn duplicate_document(&mut self, document: DocumentId) -> Result<ViewId, DocumentError> {
        if self.views.len() >= MAX_VIEWS {
            return Err(DocumentError::Capacity);
        }
        self.next_view = self
            .next_view
            .checked_add(1)
            .ok_or(DocumentError::Capacity)?;
        let view = ViewId(self.next_view);
        self.views.insert(view, document);
        Ok(view)
    }

    pub fn target(
        &self,
        view: ViewId,
        selection: Range<usize>,
    ) -> Result<EditTarget, DocumentError> {
        let document = self.document_for(view)?;
        let entry = self.get(document)?;
        let text = entry.text.text();
        if selection.start > selection.end
            || selection.end > text.len()
            || !text.is_char_boundary(selection.start)
            || !text.is_char_boundary(selection.end)
        {
            return Err(DocumentError::InvalidSelection);
        }
        Ok(EditTarget {
            view,
            document,
            revision: entry.text.revision(),
            selection,
            origin: entry.origin.key(),
            source_epoch: entry.source_epoch,
            access_epoch: entry.access_epoch,
        })
    }

    fn editable_target(&mut self, target: &EditTarget) -> Result<&mut OpenDocument, DocumentError> {
        if &self.target(target.view, target.selection.clone())? != target {
            return Err(DocumentError::Stale);
        }
        let entry = self
            .documents
            .get_mut(&target.document)
            .ok_or(DocumentError::Closed)?;
        entry.require_writable()?;
        Ok(entry)
    }

    pub fn sync(&mut self, target: &EditTarget, updated: &str) -> Result<bool, DocumentError> {
        let updated = normalized_input(updated)?;
        let entry = self.editable_target(target)?;
        check_encoded_change(&entry.text, entry.text.text(), &updated)?;
        Ok(entry.text.sync_from_view(&updated)?)
    }

    /// One non-coalesced transaction, guarded against selection-only changes too.
    pub fn replace(
        &mut self,
        captured: &EditTarget,
        current: &EditTarget,
        replacement: &str,
    ) -> Result<(), DocumentError> {
        if captured != current {
            return Err(DocumentError::Stale);
        }
        let replacement = normalized_input(replacement)?;
        let entry = self.editable_target(captured)?;
        check_encoded_change(
            &entry.text,
            &entry.text.text()[captured.selection.clone()],
            &replacement,
        )?;
        entry
            .text
            .replace(captured.selection.clone(), &replacement)?;
        Ok(())
    }

    pub fn undo(&mut self, view: ViewId) -> Result<bool, DocumentError> {
        let document = self.document_for(view)?;
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        entry.require_writable()?;
        Ok(entry.text.undo())
    }

    pub fn redo(&mut self, view: ViewId) -> Result<bool, DocumentError> {
        let document = self.document_for(view)?;
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        entry.require_writable()?;
        Ok(entry.text.redo())
    }

    pub fn end_typing(&mut self, view: ViewId) -> Result<(), DocumentError> {
        let document = self.document_for(view)?;
        self.documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?
            .text
            .close_transaction();
        Ok(())
    }

    pub fn read_ticket(&self, view: ViewId) -> Result<ReadTicket, DocumentError> {
        let document = self.document_for(view)?;
        let entry = self.get(document)?;
        Ok(ReadTicket {
            document,
            source_epoch: entry.source_epoch,
            access_epoch: entry.access_epoch,
            revision: entry.text.revision(),
            version: entry.version,
        })
    }

    pub fn observe(
        &mut self,
        ticket: &ReadTicket,
        snapshot: SourceSnapshot,
    ) -> Result<bool, DocumentError> {
        let entry = self
            .documents
            .get_mut(&ticket.document)
            .ok_or(DocumentError::Closed)?;
        if entry.source_epoch != ticket.source_epoch
            || entry.access_epoch != ticket.access_epoch
            || entry.text.revision() != ticket.revision
            || entry.version != ticket.version
        {
            return Err(DocumentError::Stale);
        }
        if entry.saving() {
            return Err(DocumentError::Busy);
        }
        TextDocument::from_bytes(&snapshot.bytes, MOBILE_DOCUMENT_BOUNDS)?;
        if entry.version == Some(snapshot.version) {
            if entry.conflict.take().is_some() {
                entry.source_epoch += 1;
                return Ok(true);
            }
            return Ok(false);
        }
        if entry
            .conflict
            .as_ref()
            .is_some_and(|change| change.snapshot.version == snapshot.version)
        {
            return Ok(false);
        }
        if entry.dirty() {
            entry.conflict = Some(ExternalChange {
                snapshot,
                kept_edits: false,
            });
            entry.source_epoch += 1;
        } else {
            entry.text.reload_from(&snapshot.bytes)?;
            entry.version = Some(snapshot.version);
            entry.source_epoch += 1;
        }
        Ok(true)
    }

    pub fn keep_edits(&mut self, view: ViewId) -> Result<(), DocumentError> {
        let document = self.document_for(view)?;
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        entry.require_writable()?;
        entry
            .conflict
            .as_mut()
            .ok_or(DocumentError::Conflict)?
            .kept_edits = true;
        Ok(())
    }

    pub fn reload(&mut self, view: ViewId) -> Result<(), DocumentError> {
        let document = self.document_for(view)?;
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        if entry.saving() {
            return Err(DocumentError::Busy);
        }
        if !entry.available {
            return Err(DocumentError::Unavailable);
        }
        let conflict = entry.conflict.as_ref().ok_or(DocumentError::Conflict)?;
        entry.text.reload_from(&conflict.snapshot.bytes)?;
        entry.version = Some(conflict.snapshot.version);
        entry.conflict = None;
        entry.source_epoch += 1;
        Ok(())
    }

    pub fn set_access(
        &mut self,
        document: DocumentId,
        available: bool,
        writable: bool,
    ) -> Result<(), DocumentError> {
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        if entry.saving() && available {
            return Err(DocumentError::Busy);
        }
        entry.access_epoch += 1;
        entry.available = available;
        entry.writable = writable && !entry.origin.is_remote();
        Ok(())
    }

    pub fn prepare_save(
        &mut self,
        view: ViewId,
        automatic: bool,
    ) -> Result<(SaveTicket, Vec<u8>), DocumentError> {
        let document = self.document_for(view)?;
        let entry = self
            .documents
            .get_mut(&document)
            .ok_or(DocumentError::Closed)?;
        entry.require_writable()?;
        if !matches!(entry.origin, DocumentOrigin::Local(_)) {
            return Err(DocumentError::Unavailable);
        }
        if entry.saving() {
            return Err(DocumentError::Busy);
        }
        let expected = match &entry.conflict {
            Some(conflict) if !automatic && conflict.kept_edits => conflict.snapshot.version,
            Some(_) => return Err(DocumentError::Conflict),
            None => entry.version.ok_or(DocumentError::Unavailable)?,
        };
        let bytes = entry.text.to_bytes();
        MOBILE_DOCUMENT_BOUNDS.check_declared_size(bytes.len())?;
        self.next_operation = self
            .next_operation
            .checked_add(1)
            .ok_or(DocumentError::Capacity)?;
        entry.text.close_transaction();
        let ticket = SaveTicket {
            document,
            source_epoch: entry.source_epoch,
            operation: self.next_operation,
            point: entry.text.save_point(),
            expected,
        };
        entry.saving = Some(ticket.operation);
        Ok((ticket, bytes))
    }

    fn saving_entry(&mut self, ticket: &SaveTicket) -> Result<&mut OpenDocument, DocumentError> {
        let entry = self
            .documents
            .get_mut(&ticket.document)
            .ok_or(DocumentError::Closed)?;
        if entry.source_epoch != ticket.source_epoch || entry.saving != Some(ticket.operation) {
            return Err(DocumentError::Stale);
        }
        Ok(entry)
    }

    pub fn finish_save(
        &mut self,
        ticket: &SaveTicket,
        version: SourceVersion,
    ) -> Result<(), DocumentError> {
        let entry = self.saving_entry(ticket)?;
        entry.text.mark_saved_at(ticket.point);
        entry.version = Some(version);
        entry.conflict = None;
        entry.saving = None;
        Ok(())
    }

    pub fn fail_save(&mut self, ticket: &SaveTicket) -> Result<(), DocumentError> {
        self.saving_entry(ticket)?.saving = None;
        Ok(())
    }

    pub fn close(&mut self, view: ViewId, discard: bool) -> Result<CloseOutcome, DocumentError> {
        let document = self.document_for(view)?;
        let last = self.views.values().filter(|id| **id == document).count() == 1;
        let entry = self.get(document)?;
        if last && entry.saving() {
            return Err(DocumentError::Busy);
        }
        if last && entry.dirty() && !discard {
            return Ok(CloseOutcome::NeedsConfirmation);
        }
        self.views.remove(&view);
        let released = if last {
            let entry = self
                .documents
                .remove(&document)
                .ok_or(DocumentError::Closed)?;
            self.origins.remove(&entry.origin.key());
            Some(document)
        } else {
            None
        };
        Ok(CloseOutcome::Closed { released })
    }
}

fn normalized_input(text: &str) -> Result<Cow<'_, str>, DocumentError> {
    if text.contains('\0') {
        return Err(DocumentError::Refused(RefusalReason::BinaryContent));
    }
    Ok(if text.contains('\r') {
        Cow::Owned(normalise_line_endings(text))
    } else {
        Cow::Borrowed(text)
    })
}

fn check_encoded_change(
    document: &TextDocument,
    removed: &str,
    inserted: &str,
) -> Result<(), DocumentError> {
    let encoded = |text: &str| {
        text.len()
            + if document.line_ending() == LineEnding::Crlf {
                text.bytes().filter(|byte| *byte == b'\n').count()
            } else {
                0
            }
    };
    let bytes = encoded(document.text()) - encoded(removed)
        + encoded(inserted)
        + if document.encoding() == Encoding::Utf8Bom {
            3
        } else {
            0
        };
    MOBILE_DOCUMENT_BOUNDS.check_declared_size(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use festerm_document::{LocalOrigin, RemoteOrigin, RemoteOwner, UntitledOrigin};

    fn origin(name: &str) -> DocumentOrigin {
        LocalOrigin::new(format!("/authorized/{name}"))
            .unwrap()
            .into()
    }

    fn version(value: u8) -> SourceVersion {
        SourceVersion([value; 32])
    }

    fn snapshot(text: &str, value: u8) -> SourceSnapshot {
        SourceSnapshot {
            bytes: text.as_bytes().to_vec(),
            version: version(value),
        }
    }

    fn local(documents: &mut Documents, text: &str) -> ViewId {
        documents
            .open(origin("notes.md"), text.as_bytes(), Some(version(1)), true)
            .unwrap()
    }

    fn edit(documents: &mut Documents, view: ViewId, text: &str) {
        let target = documents.target(view, 0..0).unwrap();
        documents.sync(&target, text).unwrap();
    }

    #[test]
    fn mobile_document_identity_and_undo_are_shared_without_shared_selection() {
        let mut documents = Documents::default();
        let first = local(&mut documents, "one two");
        let second = documents
            .open(
                origin("notes.md"),
                b"must not replace",
                Some(version(2)),
                true,
            )
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(
            documents.document_for(first),
            documents.document_for(second)
        );
        let first_target = documents.target(first, 0..3).unwrap();
        let second_target = documents.target(second, 4..7).unwrap();
        assert_ne!(first_target.selection, second_target.selection);
        documents
            .replace(&first_target, &first_target, "three")
            .unwrap();
        assert_eq!(documents.view(second).unwrap().text().text(), "three two");
        assert!(documents.view(second).unwrap().dirty());
        assert!(documents.undo(second).unwrap());
        assert_eq!(documents.view(first).unwrap().text().text(), "one two");
        assert!(!documents.view(first).unwrap().dirty());
        assert!(documents.redo(first).unwrap());
        assert_eq!(documents.view(second).unwrap().text().text(), "three two");
    }

    #[test]
    fn mobile_document_multiline_replacement_is_one_exact_encoded_undo() {
        let mut documents = Documents::default();
        let original = "\u{feff}title\r\ncafé 👩‍💻\r\nend\r\n";
        let view = local(&mut documents, original);
        edit(&mut documents, view, "title\ncafé 👩‍💻\nend!\n");
        let before = documents.view(view).unwrap().text().to_bytes();
        let text = documents.view(view).unwrap().text().text();
        let start = text.find("café").unwrap();
        let end = text.find("\nend").unwrap();
        let target = documents.target(view, start..end).unwrap();
        documents
            .replace(&target, &target, "first paragraph\n\n第二段 🦀")
            .unwrap();
        let applied = documents.view(view).unwrap().text().to_bytes();
        assert!(applied.starts_with(b"\xef\xbb\xbf"));
        assert!(std::str::from_utf8(&applied)
            .unwrap()
            .contains("first paragraph\r\n\r\n第二段 🦀"));
        assert!(documents.undo(view).unwrap());
        assert_eq!(documents.view(view).unwrap().text().to_bytes(), before);
        assert!(documents.undo(view).unwrap());
        assert_eq!(
            documents.view(view).unwrap().text().to_bytes(),
            original.as_bytes()
        );
    }

    #[test]
    fn mobile_document_rejects_stale_revision_selection_and_replayed_transactions() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "hello");
        let captured = documents.target(view, 0..0).unwrap();
        let moved = documents.target(view, 1..1).unwrap();
        assert_eq!(
            documents.replace(&captured, &moved, "no"),
            Err(DocumentError::Stale)
        );
        edit(&mut documents, view, "hello!");
        assert_eq!(
            documents.replace(&captured, &captured, "no"),
            Err(DocumentError::Stale)
        );
        let target = documents.target(view, 0..0).unwrap();
        documents.replace(&target, &target, "yes ").unwrap();
        assert_eq!(
            documents.replace(&target, &target, "yes "),
            Err(DocumentError::Stale)
        );
        assert_eq!(documents.view(view).unwrap().text().text(), "yes hello!");
    }

    #[test]
    fn mobile_document_rejects_invalid_unicode_ranges_without_clamping() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "é🦀");
        for range in [1..1, 0..3, 0..7, Range { start: 6, end: 0 }] {
            assert_eq!(
                documents.target(view, range),
                Err(DocumentError::InvalidSelection)
            );
        }
        assert!(documents.target(view, 2..6).is_ok());
    }

    #[test]
    fn mobile_document_remote_origin_refuses_every_mutation_beneath_ui() {
        let mut documents = Documents::default();
        let remote = RemoteOrigin::new(
            "example.invalid",
            22,
            RemoteOwner::username("fixture").unwrap(),
            "SHA256:fixture-only",
            "/notes.md",
            7,
        )
        .unwrap();
        let view = documents
            .open(remote.into(), b"remote", Some(version(1)), true)
            .unwrap();
        let target = documents.target(view, 0..0).unwrap();
        assert!(!documents.view(view).unwrap().writable());
        assert_eq!(
            documents.sync(&target, "changed"),
            Err(DocumentError::ReadOnly)
        );
        assert_eq!(
            documents.replace(&target, &target, "changed"),
            Err(DocumentError::ReadOnly)
        );
        assert_eq!(documents.undo(view), Err(DocumentError::ReadOnly));
        assert_eq!(documents.redo(view), Err(DocumentError::ReadOnly));
        assert!(matches!(
            documents.prepare_save(view, false),
            Err(DocumentError::ReadOnly)
        ));
        let id = documents.document_for(view).unwrap();
        documents.set_access(id, true, true).unwrap();
        assert!(!documents.view(view).unwrap().writable());
        assert_eq!(documents.view(view).unwrap().text().text(), "remote");
    }

    #[test]
    fn mobile_document_revocation_and_reauthorization_invalidate_old_targets() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "keep");
        let captured = documents.target(view, 0..0).unwrap();
        let document = documents.document_for(view).unwrap();
        documents.set_access(document, false, false).unwrap();
        assert!(!documents.view(view).unwrap().available());
        let revoked = documents.target(view, 0..0).unwrap();
        assert_eq!(
            documents.sync(&revoked, "no"),
            Err(DocumentError::Unavailable)
        );
        documents.set_access(document, true, true).unwrap();
        assert_eq!(documents.sync(&captured, "no"), Err(DocumentError::Stale));
        assert_eq!(documents.view(view).unwrap().text().text(), "keep");
    }

    #[test]
    fn mobile_document_external_change_never_silently_overwrites_dirty_text() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        edit(&mut documents, view, "my edits");
        let ticket = documents.read_ticket(view).unwrap();
        documents.observe(&ticket, snapshot("external", 2)).unwrap();
        let entry = documents.view(view).unwrap();
        assert_eq!(entry.text().text(), "my edits");
        assert_eq!(entry.conflict().unwrap().bytes(), b"external");
        assert!(!entry.conflict().unwrap().kept_edits());
        assert!(matches!(
            documents.prepare_save(view, false),
            Err(DocumentError::Conflict)
        ));
        documents.keep_edits(view).unwrap();
        assert_eq!(documents.view(view).unwrap().text().text(), "my edits");
        assert!(documents.view(view).unwrap().dirty());
        assert!(documents
            .view(view)
            .unwrap()
            .conflict()
            .unwrap()
            .kept_edits());
        assert!(matches!(
            documents.prepare_save(view, true),
            Err(DocumentError::Conflict)
        ));
        let (save, bytes) = documents.prepare_save(view, false).unwrap();
        assert_eq!(save.expected, version(2));
        assert_eq!(bytes, b"my edits");
        documents.finish_save(&save, version(3)).unwrap();
        assert!(!documents.view(view).unwrap().dirty());
        assert!(documents.view(view).unwrap().conflict().is_none());
    }

    #[test]
    fn mobile_document_clean_external_change_reloads_shared_identity() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        let sibling = documents.duplicate(view).unwrap();
        let id = documents.document_for(view).unwrap();
        let target = documents.target(view, 0..0).unwrap();
        let read = documents.read_ticket(view).unwrap();
        documents
            .observe(&read, snapshot("external\r\n", 2))
            .unwrap();
        assert_eq!(documents.document_for(sibling).unwrap(), id);
        assert_eq!(documents.view(sibling).unwrap().text().text(), "external\n");
        assert!(!documents.view(sibling).unwrap().dirty());
        assert_eq!(documents.sync(&target, "no"), Err(DocumentError::Stale));
    }

    #[test]
    fn mobile_document_late_reads_cannot_replace_edits_or_newer_conflicts() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        let old_read = documents.read_ticket(view).unwrap();
        edit(&mut documents, view, "mine");
        assert_eq!(
            documents.observe(&old_read, snapshot("old read", 2)),
            Err(DocumentError::Stale)
        );
        let read = documents.read_ticket(view).unwrap();
        documents
            .observe(&read, snapshot("first conflict", 2))
            .unwrap();
        assert_eq!(
            documents.observe(&read, snapshot("stale conflict", 3)),
            Err(DocumentError::Stale)
        );
        assert_eq!(
            documents.view(view).unwrap().conflict().unwrap().bytes(),
            b"first conflict"
        );
        let read = documents.read_ticket(view).unwrap();
        assert!(!documents
            .observe(&read, snapshot("first conflict", 2))
            .unwrap());
        assert!(documents.observe(&read, snapshot("original", 1)).unwrap());
        assert!(documents.view(view).unwrap().conflict().is_none());
        assert_eq!(documents.view(view).unwrap().text().text(), "mine");
    }

    #[test]
    fn mobile_document_reload_requires_conflict_and_discards_history_explicitly() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        assert_eq!(documents.reload(view), Err(DocumentError::Conflict));
        edit(&mut documents, view, "mine");
        let read = documents.read_ticket(view).unwrap();
        documents.observe(&read, snapshot("external", 2)).unwrap();
        documents.reload(view).unwrap();
        assert_eq!(documents.view(view).unwrap().text().text(), "external");
        assert!(!documents.view(view).unwrap().dirty());
        assert!(!documents.undo(view).unwrap());
    }

    #[test]
    fn mobile_document_save_completion_records_written_point_not_newer_edits() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        edit(&mut documents, view, "saved");
        let (save, bytes) = documents.prepare_save(view, false).unwrap();
        assert_eq!(bytes, b"saved");
        edit(&mut documents, view, "saved plus");
        documents.finish_save(&save, version(2)).unwrap();
        assert_eq!(documents.view(view).unwrap().text().text(), "saved plus");
        assert!(documents.view(view).unwrap().dirty());
        assert!(documents.undo(view).unwrap());
        assert_eq!(documents.view(view).unwrap().text().text(), "saved");
        assert!(!documents.view(view).unwrap().dirty());
        assert!(documents.undo(view).unwrap());
        assert!(documents.view(view).unwrap().dirty());
        assert_eq!(
            documents.finish_save(&save, version(2)),
            Err(DocumentError::Stale)
        );
    }

    #[test]
    fn mobile_document_failed_or_busy_save_cannot_claim_clean_or_lose_last_view() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        edit(&mut documents, view, "mine");
        let read = documents.read_ticket(view).unwrap();
        let (save, _) = documents.prepare_save(view, false).unwrap();
        assert!(documents.view(view).unwrap().saving());
        assert!(matches!(
            documents.prepare_save(view, false),
            Err(DocumentError::Busy)
        ));
        assert_eq!(documents.close(view, true), Err(DocumentError::Busy));
        assert_eq!(documents.reload(view), Err(DocumentError::Busy));
        assert_eq!(
            documents.observe(&read, snapshot("external", 2)),
            Err(DocumentError::Busy)
        );
        let id = documents.document_for(view).unwrap();
        documents.set_access(id, false, false).unwrap();
        assert!(!documents.view(view).unwrap().writable());
        documents.fail_save(&save).unwrap();
        assert!(documents.view(view).unwrap().dirty());
        assert_eq!(
            documents.close(view, false),
            Ok(CloseOutcome::NeedsConfirmation)
        );
    }

    #[test]
    fn mobile_document_only_last_dirty_view_prompts_and_handles_never_recycle() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        let sibling = documents.duplicate(view).unwrap();
        let document = documents.document_for(view).unwrap();
        edit(&mut documents, view, "mine");
        let old_read = documents.read_ticket(view).unwrap();
        assert_eq!(
            documents.close(view, false),
            Ok(CloseOutcome::Closed { released: None })
        );
        assert_eq!(
            documents.close(sibling, false),
            Ok(CloseOutcome::NeedsConfirmation)
        );
        assert_eq!(documents.views().count(), 1);
        assert_eq!(
            documents.close(sibling, true),
            Ok(CloseOutcome::Closed {
                released: Some(document)
            })
        );
        let reopened = local(&mut documents, "fresh");
        assert_ne!(reopened, view);
        assert_ne!(documents.document_for(reopened).unwrap(), document);
        assert_eq!(
            documents.observe(&old_read, snapshot("late", 2)),
            Err(DocumentError::Closed)
        );
        assert_eq!(documents.document_for(view), Err(DocumentError::Closed));
        assert_eq!(documents.view(reopened).unwrap().text().text(), "fresh");
    }

    #[test]
    fn mobile_document_untitled_copy_is_unsaved_and_cannot_write_original_source() {
        let mut documents = Documents::default();
        let source = local(&mut documents, "original");
        let copy_origin = UntitledOrigin::new("copy-1", "notes.md", "Unsaved local copy").unwrap();
        let copy = documents
            .open(copy_origin.into(), b"original", None, true)
            .unwrap();
        assert_ne!(documents.document_for(source), documents.document_for(copy));
        assert!(documents.view(copy).unwrap().dirty());
        assert!(matches!(
            documents.prepare_save(copy, false),
            Err(DocumentError::Unavailable)
        ));
        edit(&mut documents, copy, "local copy");
        assert_eq!(documents.view(source).unwrap().text().text(), "original");
        assert!(documents.undo(copy).unwrap());
        assert!(documents.view(copy).unwrap().dirty());
        documents.end_typing(copy).unwrap();
    }

    #[test]
    fn mobile_document_source_and_registry_limits_refuse_without_truncation() {
        let mut documents = Documents::default();
        let line = format!("{}\n", "a".repeat(1023));
        let limit_text = line.repeat(1024);
        assert_eq!(limit_text.len(), MOBILE_DOCUMENT_BOUNDS.max_bytes());
        let view = local(&mut documents, &limit_text);
        assert_eq!(
            documents.view(view).unwrap().text().text().len(),
            limit_text.len()
        );
        let mut oversized = limit_text.clone();
        oversized.push('a');
        let target = documents.target(view, 0..0).unwrap();
        assert!(matches!(
            documents.sync(&target, &oversized),
            Err(DocumentError::Refused(RefusalReason::TooLarge { .. }))
        ));
        assert_eq!(documents.view(view).unwrap().text().text(), limit_text);
        for index in 1..MAX_DOCUMENTS {
            documents
                .open(
                    origin(&format!("{index}.txt")),
                    b"small",
                    Some(version(1)),
                    true,
                )
                .unwrap();
        }
        assert!(matches!(
            documents.open(origin("overflow"), b"x", Some(version(1)), true),
            Err(DocumentError::Capacity)
        ));
        while documents.views().count() < MAX_VIEWS {
            documents.duplicate(view).unwrap();
        }
        assert_eq!(documents.duplicate(view), Err(DocumentError::Capacity));
    }

    #[test]
    fn mobile_document_normalizes_pasted_newlines_and_refuses_binary_edits() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "one\r\ntwo\r\n");
        edit(&mut documents, view, "one\r\ntwo\rthree\n");
        assert_eq!(
            documents.view(view).unwrap().text().text(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(
            documents.view(view).unwrap().text().to_bytes(),
            b"one\r\ntwo\r\nthree\r\n"
        );
        let target = documents.target(view, 0..0).unwrap();
        assert_eq!(
            documents.replace(&target, &target, "bad\0text"),
            Err(DocumentError::Refused(RefusalReason::BinaryContent))
        );
        assert_eq!(
            documents.sync(&target, "bad\0text"),
            Err(DocumentError::Refused(RefusalReason::BinaryContent))
        );
        assert_eq!(
            documents.view(view).unwrap().text().text(),
            "one\ntwo\nthree\n"
        );
    }

    #[test]
    fn mobile_document_edit_limits_include_original_encoded_line_endings() {
        let mut documents = Documents::default();
        let original = format!("{}\r\n", "a".repeat(115)).repeat(8900);
        let view = local(&mut documents, &original);
        let mut updated = documents.view(view).unwrap().text().text().to_owned();
        updated.push_str(&"b".repeat(8000));
        assert!(updated.len() < MOBILE_DOCUMENT_BOUNDS.max_bytes());
        let target = documents.target(view, 0..0).unwrap();
        assert!(matches!(
            documents.sync(&target, &updated),
            Err(DocumentError::Refused(RefusalReason::TooLarge { .. }))
        ));
        assert_eq!(
            documents.view(view).unwrap().text().to_bytes(),
            original.as_bytes()
        );
        assert!(!documents.view(view).unwrap().dirty());
    }

    #[test]
    fn mobile_document_revocation_gates_edits_while_a_save_finishes() {
        let mut documents = Documents::default();
        let view = local(&mut documents, "original");
        edit(&mut documents, view, "written");
        let (save, _) = documents.prepare_save(view, false).unwrap();
        let id = documents.document_for(view).unwrap();
        documents.set_access(id, false, false).unwrap();
        let target = documents.target(view, 0..0).unwrap();
        assert_eq!(
            documents.sync(&target, "no"),
            Err(DocumentError::Unavailable)
        );
        assert_eq!(
            documents.set_access(id, true, true),
            Err(DocumentError::Busy)
        );
        documents.finish_save(&save, version(2)).unwrap();
        assert!(!documents.view(view).unwrap().saving());
        assert!(!documents.view(view).unwrap().available());
        assert!(!documents.view(view).unwrap().dirty());
        assert!(matches!(
            documents.prepare_save(view, false),
            Err(DocumentError::Unavailable)
        ));
    }
}
