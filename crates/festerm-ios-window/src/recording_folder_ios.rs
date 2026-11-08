use std::{
    cell::RefCell,
    ffi::{CStr, OsStr},
    fs::{self, File},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
    sync::{
        mpsc::{self, Receiver, TryRecvError},
        Arc,
    },
    thread,
};

use block2::RcBlock;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{Bool, ProtocolObject},
    DeclaredClass, MainThreadOnly,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSFileCoordinator, NSFileCoordinatorWritingOptions,
    NSObject, NSObjectProtocol, NSString, NSURLBookmarkCreationOptions,
    NSURLBookmarkResolutionOptions, NSURL,
};
use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController, UIView};
use objc2_uniform_type_identifiers::UTTypeFolder;

const MAX_BOOKMARK_BYTES: usize = 65_536;
const MAX_DISPLAY_NAME_BYTES: usize = 1024;
type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Eq, PartialEq)]
pub struct RecordingFolderSelection {
    pub bookmark: Vec<u8>,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingFolderError {
    Busy,
    Cancelled,
    Unavailable,
    Invalid,
}

impl RecordingFolderError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Busy => "Another Files operation is active. Finish it and try again.",
            Self::Cancelled => "Folder selection cancelled.",
            Self::Unavailable => "The Files folder is unavailable. Select it again.",
            Self::Invalid => "The selected Files folder could not be authorized.",
        }
    }
}

#[derive(Default)]
struct PickerState {
    active: Option<Retained<UIDocumentPickerViewController>>,
    result: Option<Result<RecordingFolderSelection, RecordingFolderError>>,
    pending: Option<Receiver<Result<RecordingFolderSelection, RecordingFolderError>>>,
}

struct PickerIvars {
    state: Rc<RefCell<PickerState>>,
    repaint: Repaint,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "FesTermRecordingFolderPickerDelegate"]
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
            let result = (|| {
                if urls.count() != 1 {
                    return Err(RecordingFolderError::Invalid);
                }
                let url = urls.objectAtIndex(0);
                if !url.isFileURL() {
                    return Err(RecordingFolderError::Invalid);
                }
                let scope = SecurityScope::start(url)?;
                let (send, receive) = mpsc::sync_channel(1);
                let repaint = self.ivars().repaint.clone();
                thread::Builder::new()
                    .name("festerm-recording-folder".to_owned())
                    .spawn(move || {
                        let result = objc2::rc::autoreleasepool(|_| selected_folder(scope));
                        let _ = send.send(result);
                        repaint();
                    })
                    .map_err(|_| RecordingFolderError::Unavailable)?;
                self.ivars().state.borrow_mut().pending = Some(receive);
                Ok(())
            })();
            if let Err(error) = result {
                self.ivars().state.borrow_mut().result = Some(Err(error));
            }
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
                self.ivars().state.borrow_mut().result = Some(Err(RecordingFolderError::Cancelled));
                (self.ivars().repaint)();
            }
        }
    }
);

/// Main-thread owner of the explicit iOS Files folder picker.
pub struct RecordingFolderPicker {
    view: Retained<UIView>,
    delegate: Retained<PickerDelegate>,
}

impl RecordingFolderPicker {
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

    pub fn open(&self) -> Result<(), RecordingFolderError> {
        if self.delegate.ivars().state.borrow().active.is_some() {
            return Err(RecordingFolderError::Busy);
        }
        let controller = self
            .view
            .window()
            .and_then(|window| window.rootViewController())
            .ok_or(RecordingFolderError::Unavailable)?;
        if controller.presentedViewController().is_some() {
            return Err(RecordingFolderError::Busy);
        }
        // SAFETY: UTTypeFolder is an immutable system constant.
        let types = NSArray::from_slice(&[unsafe { UTTypeFolder }]);
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
            UIDocumentPickerViewController::alloc(MainThreadMarker::from(&*self.view)),
            &types,
            false,
        );
        picker.setAllowsMultipleSelection(false);
        picker.setTitle(Some(&NSString::from_str("Choose recording folder")));
        picker.setDelegate(Some(ProtocolObject::from_ref(&*self.delegate)));
        self.delegate.ivars().state.borrow_mut().active = Some(picker.clone());
        controller.presentViewController_animated_completion(&picker, true, None);
        Ok(())
    }

    pub fn take_result(&self) -> Option<Result<RecordingFolderSelection, RecordingFolderError>> {
        let mut state = self.delegate.ivars().state.borrow_mut();
        if let Some(pending) = state.pending.as_ref() {
            match pending.try_recv() {
                Ok(result) => {
                    state.pending = None;
                    state.result = Some(result);
                }
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    state.pending = None;
                    state.result = Some(Err(RecordingFolderError::Unavailable));
                }
            }
        }
        let result = state.result.take()?;
        state.active = None;
        Some(result)
    }
}

impl Drop for RecordingFolderPicker {
    fn drop(&mut self) {
        if let Some(active) = self.delegate.ivars().state.borrow_mut().active.take() {
            active.setDelegate(None);
            active.dismissViewControllerAnimated_completion(false, None);
        }
    }
}

struct SecurityScope(Retained<NSURL>);

impl SecurityScope {
    fn start(url: Retained<NSURL>) -> Result<Self, RecordingFolderError> {
        // SAFETY: a successful access is paired with stop in Drop.
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            return Err(RecordingFolderError::Unavailable);
        }
        Ok(Self(url))
    }
}

impl Drop for SecurityScope {
    fn drop(&mut self) {
        // SAFETY: this exact URL successfully started access in `start`.
        unsafe { self.0.stopAccessingSecurityScopedResource() };
    }
}

fn selected_folder(scope: SecurityScope) -> Result<RecordingFolderSelection, RecordingFolderError> {
    let data = scope
        .0
        .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::MinimalBookmark,
            None,
            None,
        )
        .map_err(|_| RecordingFolderError::Invalid)?;
    if data.length() == 0 || data.length() > MAX_BOOKMARK_BYTES {
        return Err(RecordingFolderError::Invalid);
    }
    let display_name = scope
        .0
        .lastPathComponent()
        .map(|name| name.to_string())
        .filter(|name| {
            !name.is_empty()
                && name.len() <= MAX_DISPLAY_NAME_BYTES
                && !name.chars().any(char::is_control)
        })
        .ok_or(RecordingFolderError::Invalid)?;
    Ok(RecordingFolderSelection {
        bookmark: data.to_vec(),
        display_name,
    })
}

/// Resolves a persisted picker grant and coordinates one destination operation.
/// The operation must not delete the private recording source: the caller may
/// do that only after coordination and publication have both succeeded.
pub fn with_recording_folder<T>(
    bookmark: &[u8],
    operation: impl FnOnce(&File) -> Result<T, String>,
) -> Result<T, String> {
    objc2::rc::autoreleasepool(|_| with_recording_folder_inner(bookmark, operation))
}

fn with_recording_folder_inner<T>(
    bookmark: &[u8],
    operation: impl FnOnce(&File) -> Result<T, String>,
) -> Result<T, String> {
    if bookmark.is_empty() || bookmark.len() > MAX_BOOKMARK_BYTES {
        return Err(RecordingFolderError::Invalid.message().to_owned());
    }
    let data = NSData::with_bytes(bookmark);
    let mut stale = Bool::NO;
    // SAFETY: `stale` is a valid out parameter for the synchronous resolution.
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            &data,
            NSURLBookmarkResolutionOptions::empty(),
            None,
            &mut stale,
        )
    }
    .map_err(|_| RecordingFolderError::Unavailable.message().to_owned())?;
    if stale.as_bool() || !url.isFileURL() {
        return Err(RecordingFolderError::Unavailable.message().to_owned());
    }
    let scope = SecurityScope::start(url).map_err(|error| error.message().to_owned())?;
    let coordinator = NSFileCoordinator::new();
    let operation = Rc::new(RefCell::new(Some(operation)));
    let result = Rc::new(RefCell::new(None));
    let run = operation.clone();
    let outcome = result.clone();
    let callback = RcBlock::new(move |url: NonNull<NSURL>| {
        let Some(operation) = run.borrow_mut().take() else {
            *outcome.borrow_mut() = Some(Err("Files repeated a recording operation".to_owned()));
            return;
        };
        // SAFETY: the coordinator supplies a live URL for this synchronous callback.
        let url = unsafe { url.as_ref() };
        let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let directory = open_coordinated_directory(url)?;
            operation(&directory)
        }))
        .unwrap_or_else(|_| Err("The recording export worker panicked".to_owned()));
        *outcome.borrow_mut() = Some(value);
    });
    let mut error = None;
    coordinator.coordinateWritingItemAtURL_options_error_byAccessor(
        &scope.0,
        NSFileCoordinatorWritingOptions::empty(),
        Some(&mut error),
        &callback,
    );
    if error.is_some() {
        return Err(
            "The Files provider could not coordinate the recording export; check the destination for an incomplete or published folder"
                .to_owned(),
        );
    }
    let value = result
        .borrow_mut()
        .take()
        .ok_or_else(|| "The Files provider did not authorize the recording export".to_owned())?;
    value
}

fn open_coordinated_directory(url: &NSURL) -> Result<File, String> {
    if !url.isFileURL() {
        return Err(RecordingFolderError::Invalid.message().to_owned());
    }
    let mut representation = [0u8; 4096];
    // SAFETY: this live buffer has exactly the advertised writable capacity.
    if !unsafe {
        url.getFileSystemRepresentation_maxLength(
            NonNull::from(&mut representation[0]).cast(),
            representation.len(),
        )
    } {
        return Err(RecordingFolderError::Unavailable.message().to_owned());
    }
    let bytes = CStr::from_bytes_until_nul(&representation)
        .map_err(|_| RecordingFolderError::Unavailable.message().to_owned())?;
    let path = PathBuf::from(OsStr::from_bytes(bytes.to_bytes()));
    let directory =
        File::open(&path).map_err(|_| RecordingFolderError::Unavailable.message().to_owned())?;
    let current = fs::symlink_metadata(&path)
        .map_err(|_| RecordingFolderError::Unavailable.message().to_owned())?;
    let pinned = directory
        .metadata()
        .map_err(|_| RecordingFolderError::Unavailable.message().to_owned())?;
    if !current.is_dir() || current.dev() != pinned.dev() || current.ino() != pinned.ino() {
        return Err(RecordingFolderError::Invalid.message().to_owned());
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn invalid_bookmarks_never_invoke_the_destination_operation() {
        let called = Cell::new(false);
        for bookmark in [Vec::new(), vec![1; MAX_BOOKMARK_BYTES + 1]] {
            assert!(with_recording_folder(&bookmark, |_| {
                called.set(true);
                Ok(())
            })
            .is_err());
        }
        assert!(!called.get());
    }
}
