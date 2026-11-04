use super::{
    DocumentFileError, DocumentFileEvent, DocumentFileSnapshot, MAX_DOCUMENT_BYTES,
    MAX_NATIVE_DOCUMENTS,
};
use block2::RcBlock;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, ProtocolObject},
    DeclaredClass, MainThreadOnly,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSError, NSObject, NSObjectProtocol, NSString, NSURL,
};
use objc2_ui_kit::{
    UIDocument, UIDocumentPickerDelegate, UIDocumentPickerViewController, UIDocumentSaveOperation,
    UIDocumentState, UIView,
};
use objc2_uniform_type_identifiers::UTTypeData;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

type Repaint = Arc<dyn Fn() + Send + Sync>;
static LIVE_DOCUMENTS: AtomicUsize = AtomicUsize::new(0);

struct DocumentLease;

impl Drop for DocumentLease {
    fn drop(&mut self) {
        LIVE_DOCUMENTS.fetch_sub(1, Ordering::AcqRel);
    }
}

struct FrozenSave {
    operation: u64,
    expected: [u8; 32],
    bytes: Vec<u8>,
    wrote: bool,
    _permit: crate::DocumentWritePermit,
}

#[derive(Default)]
struct FileState {
    path: Option<PathBuf>,
    started: bool,
    opened: bool,
    working: bool,
    cancelled: bool,
    read_epoch: u64,
    loaded: Option<DocumentFileSnapshot>,
    save: Option<FrozenSave>,
    completed: Option<DocumentFileEvent>,
    error: Option<DocumentFileError>,
    unavailable: Option<DocumentFileError>,
}

struct FileIvars {
    state: Arc<Mutex<FileState>>,
    repaint: Repaint,
    _lease: DocumentLease,
}

define_class!(
    #[unsafe(super(UIDocument))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermBoundedDocument"]
    #[ivars = FileIvars]
    struct BoundedDocument;

    unsafe impl NSObjectProtocol for BoundedDocument {}

    impl BoundedDocument {
        // UIKit invokes serialization on its file-access queue. Only the
        // synchronized Rust staging state is accessed here, not main-thread UI.
        #[unsafe(method(readFromURL:error:))]
        fn read_from_url(&self, url: &NSURL, error: *mut *mut NSError) -> bool {
            self.read_contents(url, error)
        }

        #[unsafe(method_id(contentsForType:error:))]
        fn contents_for_type(
            &self,
            _file_type: &NSString,
            error: *mut *mut NSError,
        ) -> Option<Retained<AnyObject>> {
            self.serialized_contents(error)
        }

        #[unsafe(method(hasUnsavedChanges))]
        fn has_unsaved_changes(&self) -> bool {
            false
        }

        #[unsafe(method(writeContents:toURL:forSaveOperation:originalContentsURL:error:))]
        fn write_contents(
            &self,
            contents: &AnyObject,
            destination: &NSURL,
            operation: UIDocumentSaveOperation,
            original: Option<&NSURL>,
            error: *mut *mut NSError,
        ) -> bool {
            self.write_verified(contents, destination, operation, original, error)
        }
    }
);

impl BoundedDocument {
    fn read_contents(&self, url: &NSURL, error: *mut *mut NSError) -> bool {
        let epoch = {
            let state = self.ivars().state.lock().expect("native document state");
            if state.cancelled {
                // SAFETY: UIKit supplies this optional NSError out pointer.
                unsafe { set_error(DocumentFileError::Cancelled, error) };
                return false;
            }
            state.read_epoch
        };
        let result = read_snapshot(url);
        let mut state = self.ivars().state.lock().expect("native document state");
        if state.cancelled || state.read_epoch != epoch {
            // SAFETY: UIKit supplies this optional NSError out pointer.
            unsafe { set_error(DocumentFileError::Cancelled, error) };
            return false;
        }
        match result {
            Ok(snapshot)
                if state
                    .path
                    .as_ref()
                    .is_none_or(|path| *path == snapshot.path) =>
            {
                state.path = Some(snapshot.path.clone());
                state.loaded = Some(snapshot);
                state.unavailable = None;
                drop(state);
                (self.ivars().repaint)();
                true
            }
            result => {
                let failure = result.err().unwrap_or(DocumentFileError::Unavailable);
                state.error = Some(failure);
                state.unavailable = Some(failure);
                drop(state);
                // SAFETY: UIKit supplies this optional NSError out pointer.
                unsafe { set_error(failure, error) };
                (self.ivars().repaint)();
                false
            }
        }
    }

    fn serialized_contents(&self, error: *mut *mut NSError) -> Option<Retained<AnyObject>> {
        let bytes = {
            let state = self.ivars().state.lock().expect("native document state");
            if state.cancelled {
                // SAFETY: UIKit supplies this optional NSError out pointer.
                unsafe { set_error(DocumentFileError::Cancelled, error) };
                return None;
            }
            state.save.as_ref().map(|save| save.bytes.clone())
        };
        match bytes {
            Some(bytes) => Some(NSData::with_bytes(&bytes).into_super().into_super()),
            None => {
                // SAFETY: UIKit supplies this optional NSError out pointer.
                unsafe { set_error(DocumentFileError::Busy, error) };
                None
            }
        }
    }

    fn write_verified(
        &self,
        contents: &AnyObject,
        destination: &NSURL,
        operation: UIDocumentSaveOperation,
        original: Option<&NSURL>,
        error: *mut *mut NSError,
    ) -> bool {
        let expected = {
            let state = self.ivars().state.lock().expect("native document state");
            if state.cancelled {
                Err(DocumentFileError::Cancelled)
            } else {
                state
                    .save
                    .as_ref()
                    .map(|save| (save.expected, state.path.clone()))
                    .ok_or(DocumentFileError::Busy)
            }
        };
        let verified = expected.and_then(|(expected, path)| {
            if operation != UIDocumentSaveOperation::ForOverwriting {
                return Err(DocumentFileError::Unavailable);
            }
            let snapshot = read_snapshot(original.ok_or(DocumentFileError::Unavailable)?)?;
            if path.as_ref() != Some(&snapshot.path) || snapshot.version != expected {
                return Err(DocumentFileError::Changed);
            }
            if !snapshot.writable {
                return Err(DocumentFileError::ReadOnly);
            }
            Ok(())
        });
        if let Err(failure) = verified {
            self.ivars()
                .state
                .lock()
                .expect("native document state")
                .error = Some(failure);
            // SAFETY: UIKit supplies this optional NSError out pointer.
            unsafe { set_error(failure, error) };
            return false;
        }
        // SAFETY: contents came from this class's NSData serializer.
        // UIDocument owns coordination, security scope and safe publication.
        let written: bool = unsafe {
            msg_send![super(self),
                writeContents: contents,
                toURL: destination,
                forSaveOperation: operation,
                originalContentsURL: original,
                error: error
            ]
        };
        if written {
            let mut state = self.ivars().state.lock().expect("native document state");
            if let Some(save) = state.save.as_mut() {
                save.wrote = true;
            }
        }
        written
    }
}

/// UIKit owns the URL scope and presenter for this exact user-selected file.
/// Only frozen, bounded serialization payloads cross into its I/O queue.
pub struct DocumentFile {
    document: Retained<BoundedDocument>,
}

impl DocumentFile {
    fn selected(
        url: &NSURL,
        repaint: Repaint,
        mtm: MainThreadMarker,
    ) -> Result<Self, DocumentFileError> {
        if !url.isFileURL() {
            return Err(DocumentFileError::NotAFile);
        }
        LIVE_DOCUMENTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_NATIVE_DOCUMENTS).then_some(count + 1)
            })
            .map_err(|_| DocumentFileError::Busy)?;
        let allocated = BoundedDocument::alloc(mtm).set_ivars(FileIvars {
            state: Arc::new(Mutex::new(FileState::default())),
            repaint,
            _lease: DocumentLease,
        });
        // SAFETY: designated UIDocument initializer with the picker-granted URL.
        let document = unsafe { msg_send![super(allocated), initWithFileURL: url] };
        Ok(Self { document })
    }

    pub fn open(&self) -> Result<(), DocumentFileError> {
        self.begin(false)?;
        let document = self.document.clone();
        let state = self.document.ivars().state.clone();
        let repaint = self.document.ivars().repaint.clone();
        let completed = RcBlock::new(move |success: Bool| {
            let mut state = state.lock().expect("native document state");
            state.working = false;
            state.opened = success.as_bool();
            if !success.as_bool() {
                state.loaded = None;
                state.unavailable =
                    Some(state.error.take().unwrap_or(DocumentFileError::Unavailable));
            }
            let cancelled = state.cancelled;
            drop(state);
            if cancelled {
                close_idle_document(document.clone());
            }
            repaint();
        });
        self.document.openWithCompletionHandler(Some(&completed));
        Ok(())
    }

    fn begin(&self, require_open: bool) -> Result<(), DocumentFileError> {
        let closed = self
            .document
            .documentState()
            .contains(UIDocumentState::Closed);
        if require_open && closed {
            return Err(DocumentFileError::Unavailable);
        }
        if !require_open && !closed {
            return Err(DocumentFileError::Busy);
        }
        let mut state = self
            .document
            .ivars()
            .state
            .lock()
            .expect("native document state");
        if state.cancelled || (require_open && !state.opened) {
            return Err(DocumentFileError::Unavailable);
        }
        if state.working || state.completed.is_some() {
            return Err(DocumentFileError::Busy);
        }
        if !require_open && state.started {
            return Err(DocumentFileError::Unavailable);
        }
        if !require_open {
            state.started = true;
        }
        state.working = true;
        state.read_epoch += 1;
        state.loaded = None;
        state.error = None;
        state.unavailable = None;
        Ok(())
    }

    pub fn refresh(&self) -> Result<(), DocumentFileError> {
        self.begin(true)?;
        let document = self.document.clone();
        let state = self.document.ivars().state.clone();
        let repaint = self.document.ivars().repaint.clone();
        let completed = RcBlock::new(move |success: Bool| {
            let mut state = state.lock().expect("native document state");
            state.working = false;
            if !success.as_bool() {
                state.loaded = None;
                state.unavailable =
                    Some(state.error.take().unwrap_or(DocumentFileError::Unavailable));
            }
            let cancelled = state.cancelled;
            drop(state);
            if cancelled {
                close_idle_document(document.clone());
            }
            repaint();
        });
        self.document
            .revertToContentsOfURL_completionHandler(&self.document.fileURL(), Some(&completed));
        Ok(())
    }

    pub fn save(
        &self,
        operation: u64,
        expected: [u8; 32],
        bytes: Vec<u8>,
    ) -> Result<(), DocumentFileError> {
        if bytes.len() > MAX_DOCUMENT_BYTES {
            return Err(DocumentFileError::TooLarge);
        }
        let native_state = self.document.documentState();
        if native_state.contains(UIDocumentState::Closed) {
            return Err(DocumentFileError::Unavailable);
        }
        if native_state.contains(UIDocumentState::EditingDisabled) {
            return Err(DocumentFileError::ReadOnly);
        }
        let permit =
            crate::document_writer::try_document_save(self.document.ivars().repaint.clone())?;
        self.begin(true)?;
        {
            let mut state = self
                .document
                .ivars()
                .state
                .lock()
                .expect("native document state");
            state.save = Some(FrozenSave {
                operation,
                expected,
                bytes,
                wrote: false,
                _permit: permit,
            });
        }
        let document = self.document.clone();
        let state = self.document.ivars().state.clone();
        let repaint = self.document.ivars().repaint.clone();
        let completed = RcBlock::new(move |success: Bool| {
            let mut state = state.lock().expect("native document state");
            state.working = false;
            if let Some(save) = state.save.take() {
                state.completed = Some(if success.as_bool() && save.wrote {
                    match state.path.as_ref() {
                        Some(path) => DocumentFileEvent::Saved {
                            operation: save.operation,
                            version: version(path, &save.bytes),
                        },
                        None => DocumentFileEvent::SaveFailed {
                            operation: save.operation,
                            error: DocumentFileError::Unavailable,
                        },
                    }
                } else {
                    DocumentFileEvent::SaveFailed {
                        operation: save.operation,
                        error: state.error.take().unwrap_or(DocumentFileError::Io),
                    }
                });
            }
            let cancelled = state.cancelled;
            drop(state);
            if cancelled {
                close_idle_document(document.clone());
            }
            repaint();
        });
        self.document.saveToURL_forSaveOperation_completionHandler(
            &self.document.fileURL(),
            UIDocumentSaveOperation::ForOverwriting,
            Some(&completed),
        );
        Ok(())
    }

    pub fn take_events(&self) -> Vec<DocumentFileEvent> {
        let editing_disabled = self
            .document
            .documentState()
            .contains(UIDocumentState::EditingDisabled);
        let mut state = self
            .document
            .ivars()
            .state
            .lock()
            .expect("native document state");
        if state.working || state.cancelled {
            return Vec::new();
        }
        let mut events = Vec::with_capacity(3);
        if let Some(completed) = state.completed.take() {
            events.push(completed);
        }
        if let Some(mut loaded) = state.loaded.take() {
            loaded.writable &= !editing_disabled;
            events.push(DocumentFileEvent::Loaded(loaded));
        }
        if let Some(error) = state.unavailable.take() {
            events.push(DocumentFileEvent::Unavailable(error));
        }
        events
    }
}

impl Drop for DocumentFile {
    fn drop(&mut self) {
        let idle = {
            let mut state = self
                .document
                .ivars()
                .state
                .lock()
                .expect("native document state");
            if state.cancelled {
                return;
            }
            state.cancelled = true;
            !state.working
        };
        if idle {
            close_idle_document(self.document.clone());
        }
    }
}

fn close_idle_document(document: Retained<BoundedDocument>) {
    if document.documentState().contains(UIDocumentState::Closed) {
        return;
    }
    let held = document.clone();
    let completed = RcBlock::new(move |success: Bool| {
        let _ = &held;
        if !success.as_bool() {
            eprintln!("festerm-mobile: native document close failed");
        }
    });
    document.closeWithCompletionHandler(Some(&completed));
}

#[cfg(all(feature = "native-document-smoke", target_abi = "sim"))]
#[path = "document_smoke.rs"]
mod smoke;
#[cfg(all(feature = "native-document-smoke", target_abi = "sim"))]
pub use smoke::DocumentSmoke;

fn read_snapshot(url: &NSURL) -> Result<DocumentFileSnapshot, DocumentFileError> {
    use std::{
        ffi::{CStr, OsStr},
        os::unix::ffi::OsStrExt,
        ptr::NonNull,
    };
    if !url.isFileURL() {
        return Err(DocumentFileError::NotAFile);
    }
    let mut representation = [0u8; 4096];
    // SAFETY: this live buffer has exactly the advertised writable capacity.
    if !unsafe {
        url.getFileSystemRepresentation_maxLength(
            NonNull::from(&mut representation[0]).cast(),
            representation.len(),
        )
    } {
        return Err(DocumentFileError::Unavailable);
    }
    let path =
        CStr::from_bytes_until_nul(&representation).map_err(|_| DocumentFileError::Unavailable)?;
    let path = PathBuf::from(OsStr::from_bytes(path.to_bytes()));
    let path = std::fs::canonicalize(path).map_err(|_| DocumentFileError::Unavailable)?;
    let file = File::open(&path).map_err(|_| DocumentFileError::Unavailable)?;
    let metadata = file.metadata().map_err(|_| DocumentFileError::Io)?;
    if !metadata.is_file() {
        return Err(DocumentFileError::NotAFile);
    }
    if metadata.len() > MAX_DOCUMENT_BYTES as u64 {
        return Err(DocumentFileError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DocumentFileError::Io)?;
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(DocumentFileError::TooLarge);
    }
    Ok(DocumentFileSnapshot {
        version: version(&path, &bytes),
        path,
        bytes,
        writable: !metadata.permissions().readonly(),
    })
}

fn version(path: &Path, bytes: &[u8]) -> [u8; 32] {
    use std::os::unix::ffi::OsStrExt;
    let mut digest = Sha256::new();
    digest.update(b"fesTerm document source v1\0");
    let path = path.as_os_str().as_bytes();
    digest.update((path.len() as u64).to_le_bytes());
    digest.update(path);
    digest.update(bytes);
    digest.finalize().into()
}

unsafe fn set_error(failure: DocumentFileError, out: *mut *mut NSError) {
    if out.is_null() {
        return;
    }
    let error = NSError::errorWithDomain_code_userInfo(
        &NSString::from_str("org.festerm.documents"),
        failure as isize + 1,
        None,
    );
    // SAFETY: the UIDocument caller supplied an NSError out parameter.
    unsafe { *out = Retained::autorelease_ptr(error) };
}

#[derive(Default)]
struct PickerState {
    active: Option<Retained<UIDocumentPickerViewController>>,
    result: Option<Result<DocumentFile, DocumentFileError>>,
}

struct PickerIvars {
    state: Rc<RefCell<PickerState>>,
    repaint: Repaint,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermDocumentPickerDelegate"]
    #[ivars = PickerIvars]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, controller: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            if !self
                .ivars()
                .state
                .borrow()
                .active
                .as_ref()
                .is_some_and(|active| **active == *controller)
            {
                return;
            }
            let result = if urls.count() == 1 {
                DocumentFile::selected(
                    &urls.objectAtIndex(0),
                    self.ivars().repaint.clone(),
                    MainThreadMarker::from(self),
                )
            } else {
                Err(DocumentFileError::NotAFile)
            };
            self.ivars().state.borrow_mut().result = Some(result);
            (self.ivars().repaint)();
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn was_cancelled(&self, controller: &UIDocumentPickerViewController) {
            if self
                .ivars()
                .state
                .borrow()
                .active
                .as_ref()
                .is_some_and(|active| **active == *controller)
            {
                self.ivars().state.borrow_mut().result = Some(Err(DocumentFileError::Cancelled));
                (self.ivars().repaint)();
            }
        }
    }
);

pub struct DocumentPicker {
    view: Retained<UIView>,
    delegate: Retained<PickerDelegate>,
}

impl DocumentPicker {
    pub(crate) fn new(view: Retained<UIView>, repaint: Repaint) -> Self {
        let allocated =
            PickerDelegate::alloc(MainThreadMarker::from(&*view)).set_ivars(PickerIvars {
                state: Rc::new(RefCell::new(PickerState::default())),
                repaint,
            });
        // SAFETY: NSObject initialization of the newly allocated main-thread delegate.
        let delegate = unsafe { msg_send![super(allocated), init] };
        Self { view, delegate }
    }

    pub fn open(&self) -> Result<(), DocumentFileError> {
        if self.delegate.ivars().state.borrow().active.is_some() {
            return Err(DocumentFileError::Busy);
        }
        let controller = self
            .view
            .window()
            .and_then(|window| window.rootViewController())
            .ok_or(DocumentFileError::Unavailable)?;
        if controller.presentedViewController().is_some() {
            return Err(DocumentFileError::Busy);
        }
        // SAFETY: UTTypeData is an immutable system constant. Unknown binary
        // files remain subject to bounded reading and the shared text decoder.
        let types = NSArray::from_slice(&[unsafe { UTTypeData }]);
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
            UIDocumentPickerViewController::alloc(MainThreadMarker::from(&*self.view)),
            &types,
            false,
        );
        picker.setAllowsMultipleSelection(false);
        picker.setShouldShowFileExtensions(true);
        picker.setTitle(Some(&NSString::from_str("Open text or Markdown")));
        picker.setDelegate(Some(ProtocolObject::from_ref(&*self.delegate)));
        self.delegate.ivars().state.borrow_mut().active = Some(picker.clone());
        controller.presentViewController_animated_completion(&picker, true, None);
        Ok(())
    }

    pub fn take_result(&self) -> Option<Result<DocumentFile, DocumentFileError>> {
        let mut state = self.delegate.ivars().state.borrow_mut();
        let result = state.result.take()?;
        state.active = None;
        Some(result)
    }
}

impl Drop for DocumentPicker {
    fn drop(&mut self) {
        if let Some(active) = self.delegate.ivars().state.borrow_mut().active.take() {
            active.setDelegate(None);
            active.dismissViewControllerAnimated_completion(false, None);
        }
    }
}
