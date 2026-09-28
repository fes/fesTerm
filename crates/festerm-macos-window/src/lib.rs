//! AppKit integration kept outside the cross-platform application crate.

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Semantic application command emitted by the native macOS menu. The app
/// translates these through the same command paths used by chrome, shortcuts,
/// and the command palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeMenuCommand {
    Paste,
    NewSession,
    NewWindow,
    StartLocalShell,
    OpenSettings,
    CloseActiveSurface,
    ToggleCommandPalette,
    ToggleSessionInspector,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
}

/// Accelerator metadata only; application policy remains in the composition root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeShortcut {
    pub command: NativeMenuCommand,
    pub key: String,
    pub control: bool,
    pub command_modifier: bool,
    pub option: bool,
    pub shift: bool,
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeMenuAction {
    Paste,
    NewSession,
    NewWindow,
    StartLocalShell,
    OpenSettings,
    CloseActiveSurface,
    ToggleCommandPalette,
    ToggleSessionInspector,
    ClearTerminal,
    ResetTerminal,
    ToggleFocusMode,
}

#[cfg(any(target_os = "macos", test))]
impl NativeMenuAction {
    const fn command(self) -> NativeMenuCommand {
        match self {
            Self::Paste => NativeMenuCommand::Paste,
            Self::NewSession => NativeMenuCommand::NewSession,
            Self::NewWindow => NativeMenuCommand::NewWindow,
            Self::StartLocalShell => NativeMenuCommand::StartLocalShell,
            Self::OpenSettings => NativeMenuCommand::OpenSettings,
            Self::CloseActiveSurface => NativeMenuCommand::CloseActiveSurface,
            Self::ToggleCommandPalette => NativeMenuCommand::ToggleCommandPalette,
            Self::ToggleSessionInspector => NativeMenuCommand::ToggleSessionInspector,
            Self::ClearTerminal => NativeMenuCommand::ClearTerminal,
            Self::ResetTerminal => NativeMenuCommand::ResetTerminal,
            Self::ToggleFocusMode => NativeMenuCommand::ToggleFocusMode,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Eq, PartialEq)]
struct NativeMenuState<'a> {
    close_label: &'a str,
    inspector_enabled: bool,
    inspector_label: &'static str,
}

#[cfg(any(target_os = "macos", test))]
const fn native_menu_state(
    close_label: &str,
    inspector_enabled: bool,
    inspector_open: bool,
) -> NativeMenuState<'_> {
    NativeMenuState {
        close_label,
        inspector_enabled,
        inspector_label: if inspector_open {
            "Hide Session Inspector"
        } else {
            "Show Session Inspector"
        },
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct OwnedNativeMenuState {
    close_label: String,
    inspector_enabled: bool,
    inspector_label: &'static str,
}

#[cfg(any(target_os = "macos", test))]
impl From<NativeMenuState<'_>> for OwnedNativeMenuState {
    fn from(state: NativeMenuState<'_>) -> Self {
        Self {
            close_label: state.close_label.to_owned(),
            inspector_enabled: state.inspector_enabled,
            inspector_label: state.inspector_label,
        }
    }
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug)]
struct NativeMenuSync<T> {
    applied: Option<T>,
    pending: bool,
}

#[cfg(any(target_os = "macos", test))]
impl<T: Clone + Eq> NativeMenuSync<T> {
    fn pending() -> Self {
        Self {
            applied: None,
            pending: true,
        }
    }

    fn should_apply(&self, requested: &T) -> bool {
        self.pending || self.applied.as_ref() != Some(requested)
    }

    fn mark_pending(&mut self) {
        self.pending = true;
    }

    fn mark_applied(&mut self, requested: T) {
        self.applied = Some(requested);
        self.pending = false;
    }
}

#[cfg(any(target_os = "macos", test))]
impl<T> Default for NativeMenuSync<T> {
    fn default() -> Self {
        Self {
            applied: None,
            pending: false,
        }
    }
}

/// Maximum file-open requests retained before the app drains Finder document
/// open events. The bound protects cold start and inactive-window bursts from
/// unbounded memory growth.
pub const OPEN_DOCUMENT_REQUEST_CAPACITY: usize = 64;

/// Maximum parse/overflow errors retained before the app drains Finder
/// document open events.
pub const OPEN_DOCUMENT_ERROR_CAPACITY: usize = 16;

/// Maximum filesystem-representation bytes accepted for one Finder document
/// URL. This avoids retaining unexpectedly large event payloads.
pub const OPEN_DOCUMENT_PATH_BYTE_CAPACITY: usize = 4096;

/// Typed non-path details for a native document-open event that could not be
/// converted into a usable filesystem path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenDocumentError {
    MissingDirectObject,
    EmptyDocumentList,
    InvalidDocumentDescriptor,
    NonFileUrl,
    MissingFilePath,
    PathTooLong { bytes: usize, max_bytes: usize },
    DocumentListTruncated { discarded: usize },
    RequestQueueFull { dropped: usize },
    ErrorQueueFull { dropped: usize },
}

impl fmt::Display for OpenDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDirectObject => {
                write!(formatter, "open-document event did not include documents")
            }
            Self::EmptyDocumentList => {
                write!(
                    formatter,
                    "open-document event did not include any documents"
                )
            }
            Self::InvalidDocumentDescriptor => {
                write!(
                    formatter,
                    "open-document event included an unsupported document item"
                )
            }
            Self::NonFileUrl => write!(formatter, "open-document event included a non-file URL"),
            Self::MissingFilePath => {
                write!(
                    formatter,
                    "open-document event included a file URL without a path"
                )
            }
            Self::PathTooLong { bytes, max_bytes } => write!(
                formatter,
                "open-document path was too large ({bytes} bytes, maximum {max_bytes})"
            ),
            Self::DocumentListTruncated { discarded } => write!(
                formatter,
                "open-document event exceeded the document limit; discarded {discarded} item(s)"
            ),
            Self::RequestQueueFull { dropped } => write!(
                formatter,
                "open-document request queue was full; discarded {dropped} document(s)"
            ),
            Self::ErrorQueueFull { dropped } => write!(
                formatter,
                "open-document error queue was full; discarded {dropped} error(s)"
            ),
        }
    }
}

impl std::error::Error for OpenDocumentError {}

/// Buffered Finder document-open work for one-shot fallback draining. Prefer
/// [`OpenDocumentBridge::register_callbacks`] so native events hand off to the
/// application queue without a per-frame polling path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpenDocumentDrain {
    pub paths: Vec<PathBuf>,
    pub errors: Vec<OpenDocumentError>,
}

/// Result from an application-owned open-document enqueue callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDocumentEnqueueResult {
    Accepted,
    Backpressured,
}

impl From<bool> for OpenDocumentEnqueueResult {
    fn from(accepted: bool) -> Self {
        if accepted {
            Self::Accepted
        } else {
            Self::Backpressured
        }
    }
}

/// Application callbacks used by the native AppleEvent bridge to hand off
/// document-open events without requiring a per-frame polling path.
#[derive(Clone)]
pub struct OpenDocumentCallbacks {
    enqueue_path: Arc<dyn Fn(PathBuf) -> OpenDocumentEnqueueResult + Send + Sync>,
    enqueue_error: Arc<dyn Fn(OpenDocumentError) -> OpenDocumentEnqueueResult + Send + Sync>,
}

impl OpenDocumentCallbacks {
    pub fn new(
        enqueue_path: Arc<dyn Fn(PathBuf) -> OpenDocumentEnqueueResult + Send + Sync>,
        enqueue_error: Arc<dyn Fn(OpenDocumentError) -> OpenDocumentEnqueueResult + Send + Sync>,
    ) -> Self {
        Self {
            enqueue_path,
            enqueue_error,
        }
    }
}

/// Remaining buffered native document-open work after attempting callback
/// delivery.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OpenDocumentPending {
    pub paths: usize,
    pub errors: usize,
}

/// Error returned when installing the native macOS open-document bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDocumentBridgeInstallError {
    NotMainThread,
    AlreadyInstalled,
}

#[derive(Default)]
struct OpenDocumentState {
    paths: VecDeque<PathBuf>,
    errors: VecDeque<OpenDocumentError>,
    dropped_errors: usize,
    callbacks: Option<OpenDocumentCallbacks>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl OpenDocumentState {
    fn register_wake_callback(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.wake = Some(wake);
    }

    fn clear_wake_callback(&mut self) {
        self.wake = None;
    }

    fn register_callbacks(&mut self, callbacks: OpenDocumentCallbacks) -> OpenDocumentPending {
        self.callbacks = Some(callbacks);
        self.flush_pending_to_callbacks()
    }

    fn clear_callbacks(&mut self) {
        self.callbacks = None;
    }

    #[cfg(any(target_os = "macos", test))]
    fn enqueue_path(&mut self, path: PathBuf) {
        if self.paths.is_empty() {
            if let Some(callbacks) = &self.callbacks {
                if (callbacks.enqueue_path)(path.clone()) == OpenDocumentEnqueueResult::Accepted {
                    return;
                }
            }
        }
        if self.paths.len() < OPEN_DOCUMENT_REQUEST_CAPACITY {
            self.paths.push_back(path);
        } else {
            self.enqueue_error(OpenDocumentError::RequestQueueFull { dropped: 1 });
        }
    }

    #[cfg(any(target_os = "macos", test))]
    fn enqueue_error(&mut self, error: OpenDocumentError) {
        if self.errors.is_empty() && self.dropped_errors == 0 {
            if let Some(callbacks) = &self.callbacks {
                if (callbacks.enqueue_error)(error.clone()) == OpenDocumentEnqueueResult::Accepted {
                    return;
                }
            }
        }
        if self.errors.len() < OPEN_DOCUMENT_ERROR_CAPACITY {
            self.errors.push_back(error);
        } else {
            self.dropped_errors = self.dropped_errors.saturating_add(1);
        }
    }

    fn flush_pending_to_callbacks(&mut self) -> OpenDocumentPending {
        let Some(callbacks) = &self.callbacks else {
            return self.pending();
        };

        while let Some(path) = self.paths.front().cloned() {
            if (callbacks.enqueue_path)(path) == OpenDocumentEnqueueResult::Backpressured {
                return self.pending();
            }
            self.paths.pop_front();
        }

        while let Some(error) = self.errors.front().cloned() {
            if (callbacks.enqueue_error)(error) == OpenDocumentEnqueueResult::Backpressured {
                return self.pending();
            }
            self.errors.pop_front();
        }

        if self.dropped_errors > 0 {
            let error = OpenDocumentError::ErrorQueueFull {
                dropped: self.dropped_errors,
            };
            if (callbacks.enqueue_error)(error) == OpenDocumentEnqueueResult::Accepted {
                self.dropped_errors = 0;
            }
        }

        self.pending()
    }

    fn pending(&self) -> OpenDocumentPending {
        OpenDocumentPending {
            paths: self.paths.len(),
            errors: self
                .errors
                .len()
                .saturating_add(usize::from(self.dropped_errors > 0)),
        }
    }

    fn drain(&mut self) -> OpenDocumentDrain {
        let mut errors = self.errors.drain(..).collect::<Vec<_>>();
        if self.dropped_errors > 0 {
            push_bounded_error(
                &mut errors,
                OpenDocumentError::ErrorQueueFull {
                    dropped: std::mem::take(&mut self.dropped_errors),
                },
            );
        }
        OpenDocumentDrain {
            paths: self.paths.drain(..).collect(),
            errors,
        }
    }
}

fn push_bounded_error(errors: &mut Vec<OpenDocumentError>, error: OpenDocumentError) {
    if errors.len() < OPEN_DOCUMENT_ERROR_CAPACITY {
        errors.push(error);
    } else if let OpenDocumentError::ErrorQueueFull { dropped } = error {
        let replacement = OpenDocumentError::ErrorQueueFull {
            dropped: dropped.saturating_add(1),
        };
        let _ = errors.pop();
        errors.push(replacement);
    }
}

#[cfg(any(target_os = "macos", test))]
fn wake_callback_from(
    state: &Arc<Mutex<OpenDocumentState>>,
) -> Option<Arc<dyn Fn() + Send + Sync>> {
    state
        .lock()
        .expect("open-document bridge mutex poisoned")
        .wake
        .clone()
}

#[cfg(any(target_os = "macos", test))]
fn wake_after_enqueue(state: &Arc<Mutex<OpenDocumentState>>) {
    if let Some(wake) = wake_callback_from(state) {
        wake();
    }
}

/// Handle for native Finder document-open AppleEvents.
///
/// Install this before starting eframe so launch-time `kAEOpenDocuments`
/// events can be buffered. After the application activation queue exists,
/// register callbacks; buffered work is then retried without converting these
/// native events into egui dropped-files.
pub struct OpenDocumentBridge {
    state: Arc<Mutex<OpenDocumentState>>,
    installed: bool,
    #[cfg(target_os = "macos")]
    handler: Option<objc2::rc::Retained<objc2::runtime::AnyObject>>,
}

impl OpenDocumentBridge {
    pub fn unavailable() -> Self {
        Self {
            state: Arc::new(Mutex::new(OpenDocumentState::default())),
            installed: false,
            #[cfg(target_os = "macos")]
            handler: None,
        }
    }

    pub fn is_installed(&self) -> bool {
        self.installed
    }

    pub fn register_ui_wake_callback(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        self.reassert_open_document_handler();
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .register_wake_callback(wake);
    }

    pub fn clear_ui_wake_callback(&self) {
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .clear_wake_callback();
    }

    pub fn register_callbacks(&self, callbacks: OpenDocumentCallbacks) -> OpenDocumentPending {
        self.reassert_open_document_handler();
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .register_callbacks(callbacks)
    }

    pub fn clear_callbacks(&self) {
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .clear_callbacks();
    }

    /// Attempts to deliver buffered cold-start events to registered callbacks.
    /// The bridge peeks one buffered item at a time and removes it only after
    /// the application callback reports capacity.
    pub fn flush_pending_to_callbacks(&self) -> OpenDocumentPending {
        self.reassert_open_document_handler();
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .flush_pending_to_callbacks()
    }

    pub fn pending(&self) -> OpenDocumentPending {
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .pending()
    }

    pub fn drain(&self) -> OpenDocumentDrain {
        self.state
            .lock()
            .expect("open-document bridge mutex poisoned")
            .drain()
    }

    #[cfg(target_os = "macos")]
    fn reassert_open_document_handler(&self) {
        if let Some(handler) = &self.handler {
            open_documents::register_apple_event_handler(handler);
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn reassert_open_document_handler(&self) {}
}

/// Installs the macOS Finder open-document AppleEvent bridge.
///
/// On non-macOS platforms this is a compile-safe no-op that returns an
/// unavailable bridge.
#[cfg(target_os = "macos")]
pub fn install_open_document_bridge() -> Result<OpenDocumentBridge, OpenDocumentBridgeInstallError>
{
    open_documents::install()
}

#[cfg(not(target_os = "macos"))]
pub fn install_open_document_bridge() -> Result<OpenDocumentBridge, OpenDocumentBridgeInstallError>
{
    Ok(OpenDocumentBridge::unavailable())
}

#[cfg(target_os = "macos")]
mod open_documents {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, Ordering};
    use std::sync::{Arc, Mutex};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::NSApplicationDidFinishLaunchingNotification;
    use objc2_foundation::{
        MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager,
        NSAppleEventManagerWillProcessFirstEventNotification, NSNotification, NSNotificationCenter,
        NSObject, NSObjectProtocol, NSURL,
    };

    use super::{
        wake_after_enqueue, OpenDocumentBridge, OpenDocumentBridgeInstallError, OpenDocumentError,
        OpenDocumentState, OPEN_DOCUMENT_ERROR_CAPACITY, OPEN_DOCUMENT_PATH_BYTE_CAPACITY,
        OPEN_DOCUMENT_REQUEST_CAPACITY,
    };

    const K_CORE_EVENT_CLASS: u32 = 0x6165_7674; // 'aevt'
    const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63; // 'odoc'
    const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d; // '----'

    static ACTIVE_OPEN_DOCUMENT_HANDLER: AtomicPtr<AnyObject> =
        AtomicPtr::new(std::ptr::null_mut());
    static ORIGINAL_FINISH_LAUNCHING: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
    static INSTALL_FINISH_LAUNCHING_HOOK: std::sync::Once = std::sync::Once::new();

    struct OpenDocumentHandlerIvars {
        state: Arc<Mutex<OpenDocumentState>>,
    }

    define_class!(
        // SAFETY: NSObject imposes no additional subclassing invariants. The
        // handler is installed on the main thread and only touches bounded
        // Rust queues behind a mutex.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = OpenDocumentHandlerIvars]
        struct OpenDocumentHandler;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for OpenDocumentHandler {}

        impl OpenDocumentHandler {
            #[unsafe(method(handleOpenDocuments:withReplyEvent:))]
            fn handle_open_documents(
                &self,
                event: &NSAppleEventDescriptor,
                _reply: &NSAppleEventDescriptor,
            ) {
                enqueue_event(&self.ivars().state, event);
                wake_after_enqueue(&self.ivars().state);
            }

            #[unsafe(method(applicationDidFinishLaunching:))]
            fn application_did_finish_launching(&self, _notification: &NSNotification) {
                register_apple_event_handler(self);
            }

            #[unsafe(method(appleEventManagerWillProcessFirstEvent:))]
            fn apple_event_manager_will_process_first_event(&self, _notification: &NSNotification) {
                register_apple_event_handler(self);
            }
        }
    );

    impl OpenDocumentHandler {
        fn new(state: Arc<Mutex<OpenDocumentState>>, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(OpenDocumentHandlerIvars { state });
            // SAFETY: NSObject's init signature is correct.
            unsafe { msg_send![super(this), init] }
        }
    }

    pub fn install() -> Result<OpenDocumentBridge, OpenDocumentBridgeInstallError> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(OpenDocumentBridgeInstallError::NotMainThread);
        };
        if !ACTIVE_OPEN_DOCUMENT_HANDLER
            .load(Ordering::SeqCst)
            .is_null()
        {
            return Err(OpenDocumentBridgeInstallError::AlreadyInstalled);
        }
        let state = Arc::new(Mutex::new(OpenDocumentState::default()));
        let handler = OpenDocumentHandler::new(state.clone(), mtm);
        install_finish_launching_hook();
        ACTIVE_OPEN_DOCUMENT_HANDLER.store(
            (&*handler as *const OpenDocumentHandler)
                .cast_mut()
                .cast::<AnyObject>(),
            Ordering::SeqCst,
        );
        register_apple_event_handler(&handler);
        let center = NSNotificationCenter::defaultCenter();
        // SAFETY: `handler` implements `handleOpenDocuments:withReplyEvent:`
        // with the NSAppleEventManager handler signature and
        // `applicationDidFinishLaunching:` with the NSNotification observer
        // signature. The bridge retains it until Drop removes both
        // registrations.
        unsafe {
            center.addObserver_selector_name_object(
                &handler,
                sel!(applicationDidFinishLaunching:),
                Some(NSApplicationDidFinishLaunchingNotification),
                None,
            );
            center.addObserver_selector_name_object(
                &handler,
                sel!(appleEventManagerWillProcessFirstEvent:),
                Some(NSAppleEventManagerWillProcessFirstEventNotification),
                None,
            );
        }
        Ok(OpenDocumentBridge {
            state,
            installed: true,
            handler: Some(handler.into()),
        })
    }

    fn install_finish_launching_hook() {
        INSTALL_FINISH_LAUNCHING_HOOK.call_once(|| {
            let method = class!(NSApplication)
                .instance_method(sel!(finishLaunching))
                .expect("NSApplication finishLaunching method");
            // SAFETY: The replacement has the Objective-C instance method
            // ABI for `-[NSApplication finishLaunching]` and calls the
            // previous implementation before reasserting only this
            // process-wide AppleEvent handler.
            let previous = unsafe {
                method.set_implementation(std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel),
                    objc2::runtime::Imp,
                >(festerm_finish_launching))
            };
            ORIGINAL_FINISH_LAUNCHING.store(previous as *mut (), Ordering::SeqCst);
        });
    }

    unsafe extern "C-unwind" fn festerm_finish_launching(this: &AnyObject, selector: Sel) {
        let original = ORIGINAL_FINISH_LAUNCHING.load(Ordering::SeqCst);
        assert!(
            !original.is_null(),
            "original AppKit launch method is installed"
        );
        // SAFETY: Stored from `Method::set_implementation` for the same
        // selector and class, so the implementation has the
        // `finishLaunching` method ABI.
        let original: unsafe extern "C-unwind" fn(&AnyObject, Sel) =
            unsafe { std::mem::transmute(original) };
        unsafe { original(this, selector) };
        let handler = ACTIVE_OPEN_DOCUMENT_HANDLER.load(Ordering::SeqCst);
        if !handler.is_null() {
            // SAFETY: The bridge retains the active handler while this global
            // pointer is set, and Drop clears it before releasing.
            let handler = unsafe { &*handler };
            register_apple_event_handler(handler);
        }
    }

    pub(super) fn register_apple_event_handler(handler: &AnyObject) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: `handler` implements `handleOpenDocuments:withReplyEvent:`
        // with the NSAppleEventManager handler signature, is retained by the
        // returned bridge, and the selector is registered only for the
        // process-wide open-documents AppleEvent pair. AppKit's launch path may
        // install its default open-documents handler at finishLaunching, so the
        // finish-launching observer re-applies this registration without
        // replacing the application delegate.
        unsafe {
            let _: () = msg_send![
                &*manager,
                setEventHandler: handler,
                andSelector: sel!(handleOpenDocuments:withReplyEvent:),
                forEventClass: K_CORE_EVENT_CLASS,
                andEventID: K_AE_OPEN_DOCUMENTS,
            ];
        }
    }

    fn enqueue_event(state: &Arc<Mutex<OpenDocumentState>>, event: &NSAppleEventDescriptor) {
        let mut parsed = parse_event(event);
        let mut state = state.lock().expect("open-document bridge mutex poisoned");
        for path in parsed.paths.drain(..) {
            state.enqueue_path(path);
        }
        for error in parsed.errors.drain(..) {
            state.enqueue_error(error);
        }
    }

    pub(super) struct ParsedOpenDocuments {
        pub(super) paths: Vec<PathBuf>,
        pub(super) errors: Vec<OpenDocumentError>,
    }

    pub(super) fn parse_event(event: &NSAppleEventDescriptor) -> ParsedOpenDocuments {
        // SAFETY: `event` is supplied by NSAppleEventManager to this handler;
        // the selector returns an optional NSAppleEventDescriptor for the
        // direct-object keyword.
        let direct: Option<Retained<NSAppleEventDescriptor>> =
            unsafe { msg_send![event, paramDescriptorForKeyword: KEY_DIRECT_OBJECT] };
        let Some(direct) = direct else {
            return ParsedOpenDocuments {
                paths: Vec::new(),
                errors: vec![OpenDocumentError::MissingDirectObject],
            };
        };
        parse_document_descriptor(&direct)
    }

    fn parse_document_descriptor(descriptor: &NSAppleEventDescriptor) -> ParsedOpenDocuments {
        let item_count = descriptor.numberOfItems();
        if item_count <= 0 {
            return parse_one_document_descriptor(descriptor).unwrap_or_else(|| {
                ParsedOpenDocuments {
                    paths: Vec::new(),
                    errors: vec![OpenDocumentError::EmptyDocumentList],
                }
            });
        }

        let mut paths = Vec::new();
        let mut errors = Vec::new();
        let mut dropped_errors = 0usize;
        let item_count = item_count as usize;
        let processed_count = item_count.min(OPEN_DOCUMENT_REQUEST_CAPACITY);
        if item_count > processed_count {
            push_parse_error(
                &mut errors,
                &mut dropped_errors,
                OpenDocumentError::DocumentListTruncated {
                    discarded: item_count - processed_count,
                },
            );
        }
        for index in 1..=processed_count {
            match descriptor.descriptorAtIndex(index as isize) {
                Some(item) => match file_url_path(&item) {
                    Ok(path) => paths.push(path),
                    Err(error) => push_parse_error(&mut errors, &mut dropped_errors, error),
                },
                None => push_parse_error(
                    &mut errors,
                    &mut dropped_errors,
                    OpenDocumentError::InvalidDocumentDescriptor,
                ),
            }
        }
        finish_parse_errors(&mut errors, dropped_errors);
        ParsedOpenDocuments { paths, errors }
    }

    fn parse_one_document_descriptor(
        descriptor: &NSAppleEventDescriptor,
    ) -> Option<ParsedOpenDocuments> {
        match file_url_path(descriptor) {
            Ok(path) => Some(ParsedOpenDocuments {
                paths: vec![path],
                errors: Vec::new(),
            }),
            Err(OpenDocumentError::InvalidDocumentDescriptor) => None,
            Err(error) => Some(ParsedOpenDocuments {
                paths: Vec::new(),
                errors: vec![error],
            }),
        }
    }

    fn file_url_path(descriptor: &NSAppleEventDescriptor) -> Result<PathBuf, OpenDocumentError> {
        let Some(url) = descriptor.fileURLValue() else {
            return Err(OpenDocumentError::InvalidDocumentDescriptor);
        };
        path_from_file_url(&url)
    }

    fn path_from_file_url(url: &NSURL) -> Result<PathBuf, OpenDocumentError> {
        if !url.isFileURL() {
            return Err(OpenDocumentError::NonFileUrl);
        }
        let representation = url.fileSystemRepresentation();
        // SAFETY: NSURL returns a process-owned NUL-terminated filesystem
        // representation pointer valid for immediate use.
        let path = unsafe { CStr::from_ptr(representation.as_ptr()) };
        let bytes = path.to_bytes();
        if bytes.is_empty() {
            return Err(OpenDocumentError::MissingFilePath);
        }
        if bytes.len() > OPEN_DOCUMENT_PATH_BYTE_CAPACITY {
            return Err(OpenDocumentError::PathTooLong {
                bytes: bytes.len(),
                max_bytes: OPEN_DOCUMENT_PATH_BYTE_CAPACITY,
            });
        }
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }

    fn push_parse_error(
        errors: &mut Vec<OpenDocumentError>,
        dropped_errors: &mut usize,
        error: OpenDocumentError,
    ) {
        if errors.len() < OPEN_DOCUMENT_ERROR_CAPACITY {
            errors.push(error);
        } else {
            *dropped_errors = dropped_errors.saturating_add(1);
        }
    }

    fn finish_parse_errors(errors: &mut Vec<OpenDocumentError>, dropped_errors: usize) {
        if dropped_errors == 0 {
            return;
        }
        let summary = OpenDocumentError::ErrorQueueFull {
            dropped: dropped_errors,
        };
        if errors.len() < OPEN_DOCUMENT_ERROR_CAPACITY {
            errors.push(summary);
        } else {
            let _ = errors.pop();
            errors.push(OpenDocumentError::ErrorQueueFull {
                dropped: dropped_errors.saturating_add(1),
            });
        }
    }

    impl Drop for super::OpenDocumentBridge {
        fn drop(&mut self) {
            if self.installed {
                let Some(handler) = self.handler.take() else {
                    return;
                };
                let handler_ptr = (&*handler as *const AnyObject).cast_mut();
                let _ = ACTIVE_OPEN_DOCUMENT_HANDLER.compare_exchange(
                    handler_ptr,
                    std::ptr::null_mut(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                let manager = NSAppleEventManager::sharedAppleEventManager();
                let center = NSNotificationCenter::defaultCenter();
                // SAFETY: This unregisters the process-wide handler for the
                // exact AppleEvent class/id installed above, then removes the
                // retained NSObject from its finish-launching observer
                // registration.
                unsafe {
                    let _: () = msg_send![
                        &*manager,
                        removeEventHandlerForEventClass: K_CORE_EVENT_CLASS,
                        andEventID: K_AE_OPEN_DOCUMENTS,
                    ];
                    center.removeObserver(&handler);
                }
            }
        }
    }
}

/// Observes macOS resume-from-sleep (ADR 0018: "resume from system sleep" is
/// the primary wake/network-change trigger for an on-demand SSH liveness
/// probe). Network-interface/route-change detection is deliberately out of
/// scope here; see issue #48 for that follow-up.
#[cfg(target_os = "macos")]
mod wake {
    use std::sync::Arc;

    use objc2::rc::Retained;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{MainThreadMarker, NSNotification, NSObject, NSObjectProtocol};

    struct WakeObserverIvars {
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    define_class!(
        // SAFETY: NSObject imposes no additional subclassing invariants. The
        // observer is main-thread-only and its Rust ivars are dropped
        // normally.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = WakeObserverIvars]
        struct WakeObserver;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for WakeObserver {}

        impl WakeObserver {
            #[unsafe(method(didWake:))]
            fn did_wake(&self, _notification: Option<&NSNotification>) {
                (self.ivars().wake)();
            }
        }
    );

    impl WakeObserver {
        fn new(wake: Arc<dyn Fn() + Send + Sync>, mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(WakeObserverIvars { wake });
            // SAFETY: NSObject's init signature is correct.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// Registers `wake` to run once per resume-from-sleep for as long as the
    /// returned `WakeMonitor` stays alive; dropping it unregisters the
    /// observer.
    pub struct WakeMonitor {
        // The observer is retained for its NSNotificationCenter registration;
        // dropping it removes that registration in `Drop` below.
        observer: Option<Retained<WakeObserver>>,
    }

    impl WakeMonitor {
        pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
            let mtm =
                MainThreadMarker::new().expect("wake-monitor installation requires main thread");
            let observer = WakeObserver::new(wake, mtm);
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            // SAFETY: `observer` is a valid, retained NSObject subclass
            // implementing `didWake:`, and it outlives this registration
            // (removed in `Drop` before the observer is deallocated).
            unsafe {
                center.addObserver_selector_name_object(
                    &observer,
                    sel!(didWake:),
                    Some(objc2_app_kit::NSWorkspaceDidWakeNotification),
                    None,
                );
            }
            Self {
                observer: Some(observer),
            }
        }
    }

    impl Drop for WakeMonitor {
        fn drop(&mut self) {
            if let Some(observer) = self.observer.take() {
                let center = NSWorkspace::sharedWorkspace().notificationCenter();
                // SAFETY: `observer` was registered above and is still a
                // valid NSObject at this point.
                unsafe {
                    center.removeObserver(&observer);
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use wake::WakeMonitor;

#[cfg(not(target_os = "macos"))]
pub struct WakeMonitor;

#[cfg(not(target_os = "macos"))]
impl WakeMonitor {
    pub fn install(_wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }
}

#[cfg(target_os = "macos")]
mod menu {
    use std::sync::{mpsc, Arc};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
    use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};

    use super::{
        native_menu_state, NativeMenuAction, NativeMenuCommand, NativeMenuSync, NativeShortcut,
        OwnedNativeMenuState,
    };

    struct MenuTargetIvars {
        sender: mpsc::Sender<NativeMenuCommand>,
        wake: Arc<dyn Fn() + Send + Sync>,
    }

    define_class!(
        // SAFETY: NSObject imposes no additional subclassing invariants. The
        // target is main-thread-only and its Rust ivars are dropped normally.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = MenuTargetIvars]
        struct MenuTarget;

        // SAFETY: NSObjectProtocol has no additional safety requirements.
        unsafe impl NSObjectProtocol for MenuTarget {}

        impl MenuTarget {
            #[unsafe(method(pasteFromClipboard:))]
            fn paste_from_clipboard(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::Paste);
            }
            #[unsafe(method(newSession:))]
            fn new_session(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::NewSession);
            }

            #[unsafe(method(newWindow:))]
            fn new_window(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::NewWindow);
            }

            #[unsafe(method(startLocalShell:))]
            fn start_local_shell(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::StartLocalShell);
            }

            #[unsafe(method(openSettings:))]
            fn open_settings(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::OpenSettings);
            }

            #[unsafe(method(closeActiveSurface:))]
            fn close_active_surface(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::CloseActiveSurface);
            }

            #[unsafe(method(toggleCommandPalette:))]
            fn toggle_command_palette(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleCommandPalette);
            }

            #[unsafe(method(toggleSessionInspector:))]
            fn toggle_session_inspector(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleSessionInspector);
            }

            #[unsafe(method(clearTerminal:))]
            fn clear_terminal(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ClearTerminal);
            }

            #[unsafe(method(resetTerminal:))]
            fn reset_terminal(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ResetTerminal);
            }

            #[unsafe(method(toggleFocusMode:))]
            fn toggle_focus_mode(&self, _sender: Option<&AnyObject>) {
                self.emit(NativeMenuAction::ToggleFocusMode);
            }
        }
    );

    impl MenuTarget {
        fn new(
            sender: mpsc::Sender<NativeMenuCommand>,
            wake: Arc<dyn Fn() + Send + Sync>,
            mtm: MainThreadMarker,
        ) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(MenuTargetIvars { sender, wake });
            // SAFETY: NSObject's init signature is correct.
            unsafe { msg_send![super(this), init] }
        }

        fn emit(&self, action: NativeMenuAction) {
            let mtm = MainThreadMarker::new().expect("native menu callback is main-thread-only");
            if NSApplication::sharedApplication(mtm)
                .currentEvent()
                .is_some_and(|event| {
                    event.r#type() == objc2_app_kit::NSEventType::KeyDown && event.isARepeat()
                })
            {
                return;
            }
            let _ = self.ivars().sender.send(action.command());
            (self.ivars().wake)();
        }
    }

    struct ShortcutItem {
        command: NativeMenuCommand,
        item: Retained<NSMenuItem>,
    }

    fn command_for_selector(selector: objc2::runtime::Sel) -> Option<NativeMenuCommand> {
        if selector == sel!(newSession:) {
            Some(NativeMenuCommand::NewSession)
        } else if selector == sel!(newWindow:) {
            Some(NativeMenuCommand::NewWindow)
        } else if selector == sel!(startLocalShell:) {
            Some(NativeMenuCommand::StartLocalShell)
        } else if selector == sel!(openSettings:) {
            Some(NativeMenuCommand::OpenSettings)
        } else if selector == sel!(closeActiveSurface:) {
            Some(NativeMenuCommand::CloseActiveSurface)
        } else if selector == sel!(toggleCommandPalette:) {
            Some(NativeMenuCommand::ToggleCommandPalette)
        } else if selector == sel!(clearTerminal:) {
            Some(NativeMenuCommand::ClearTerminal)
        } else if selector == sel!(resetTerminal:) {
            Some(NativeMenuCommand::ResetTerminal)
        } else if selector == sel!(toggleFocusMode:) {
            Some(NativeMenuCommand::ToggleFocusMode)
        } else {
            None
        }
    }

    pub struct NativeMenu {
        main: Option<Retained<NSMenu>>,
        receiver: Option<mpsc::Receiver<NativeMenuCommand>>,
        // NSMenuItem targets are weak; retain the target for the menu lifetime.
        _target: Option<Retained<MenuTarget>>,
        close_item: Option<Retained<NSMenuItem>>,
        inspector_item: Option<Retained<NSMenuItem>>,
        shortcut_items: Vec<ShortcutItem>,
        state_sync: NativeMenuSync<OwnedNativeMenuState>,
        shortcut_sync: NativeMenuSync<Vec<NativeShortcut>>,
    }

    impl NativeMenu {
        pub fn unavailable() -> Self {
            Self {
                main: None,
                receiver: None,
                _target: None,
                close_item: None,
                inspector_item: None,
                shortcut_items: Vec::new(),
                state_sync: NativeMenuSync::default(),
                shortcut_sync: NativeMenuSync::default(),
            }
        }

        pub fn try_recv(&self) -> Option<NativeMenuCommand> {
            self.receiver
                .as_ref()
                .and_then(|receiver| receiver.try_recv().ok())
        }

        pub fn shortcuts_need_update(&self) -> bool {
            self.shortcut_sync.pending
        }

        pub fn update(&mut self, close_label: &str, inspector_enabled: bool, inspector_open: bool) {
            let requested = OwnedNativeMenuState::from(native_menu_state(
                close_label,
                inspector_enabled,
                inspector_open,
            ));
            if !self.state_sync.should_apply(&requested) {
                return;
            }
            self.ensure_bound_items();
            let (Some(close_item), Some(inspector_item)) = (&self.close_item, &self.inspector_item)
            else {
                self.state_sync.mark_pending();
                return;
            };
            close_item.setTitle(&NSString::from_str(&requested.close_label));
            inspector_item.setEnabled(requested.inspector_enabled);
            inspector_item.setTitle(&NSString::from_str(requested.inspector_label));
            self.state_sync.mark_applied(requested);
        }

        pub fn update_shortcuts(&mut self, shortcuts: &[NativeShortcut]) {
            let requested = shortcuts.to_vec();
            if !self.shortcut_sync.should_apply(&requested) {
                return;
            }
            self.ensure_bound_items();
            if self.shortcut_items.is_empty() {
                self.shortcut_sync.mark_pending();
                return;
            }
            for shortcut_item in &self.shortcut_items {
                let binding = requested
                    .iter()
                    .find(|binding| binding.command == shortcut_item.command);
                shortcut_item.item.setKeyEquivalent(&NSString::from_str(
                    binding.map_or("", |binding| binding.key.as_str()),
                ));
                let mut flags = NSEventModifierFlags::empty();
                if let Some(binding) = binding {
                    if binding.control {
                        flags |= NSEventModifierFlags::Control;
                    }
                    if binding.command_modifier {
                        flags |= NSEventModifierFlags::Command;
                    }
                    if binding.option {
                        flags |= NSEventModifierFlags::Option;
                    }
                    if binding.shift {
                        flags |= NSEventModifierFlags::Shift;
                    }
                }
                shortcut_item.item.setKeyEquivalentModifierMask(flags);
            }
            self.shortcut_sync.mark_applied(requested);
        }

        fn ensure_bound_items(&mut self) {
            if self.close_item.is_some()
                && self.inspector_item.is_some()
                && !self.shortcut_items.is_empty()
            {
                return;
            }
            if self.main.is_none() && self.receiver.is_none() && self._target.is_none() {
                return;
            }
            let fallback_main = self.main.is_none().then(|| {
                let mtm = MainThreadMarker::new()
                    .expect("native menu synchronization requires main thread");
                NSApplication::sharedApplication(mtm).mainMenu()
            });
            let main = match self.main.as_ref() {
                Some(main) => Some(main),
                None => fallback_main.as_ref().and_then(|main| main.as_ref()),
            };
            let Some(main) = main else {
                return;
            };
            let mut close_item = None;
            let mut inspector_item = None;
            let mut shortcut_items = Vec::new();
            for root in main.itemArray() {
                let Some(menu) = root.submenu() else { continue };
                for item in menu.itemArray() {
                    let Some(selector) = item.action() else {
                        continue;
                    };
                    if selector == sel!(closeActiveSurface:) {
                        close_item = Some(item.clone());
                    } else if selector == sel!(toggleSessionInspector:) {
                        inspector_item = Some(item.clone());
                    }
                    if let Some(command) = command_for_selector(selector) {
                        shortcut_items.push(ShortcutItem {
                            command,
                            item: item.clone(),
                        });
                    }
                }
            }
            self.close_item = close_item;
            self.inspector_item = inspector_item;
            self.shortcut_items = shortcut_items;
        }
    }

    pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> NativeMenu {
        let mtm = MainThreadMarker::new().expect("AppKit menu installation requires main thread");
        let (sender, receiver) = mpsc::channel();
        let target = MenuTarget::new(sender, wake, mtm);
        let app = NSApplication::sharedApplication(mtm);

        let main = menu(mtm, "Main");
        let app_menu = menu(mtm, "fesTerm");
        let app_root = submenu_root(mtm, "fesTerm", &app_menu);
        main.addItem(&app_root);

        app_menu.addItem(&custom_item(
            mtm,
            "Settings…",
            ",",
            NSEventModifierFlags::Command,
            sel!(openSettings:),
            &target,
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        let services = menu(mtm, "Services");
        let services_item = submenu_root(mtm, "Services", &services);
        app_menu.addItem(&services_item);
        app.setServicesMenu(Some(&services));
        app_menu.addItem(&responder_item(mtm, "Hide fesTerm", "h", sel!(hide:)));
        let hide_others = responder_item(mtm, "Hide Others", "h", sel!(hideOtherApplications:));
        hide_others.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
        );
        app_menu.addItem(&hide_others);
        app_menu.addItem(&responder_item(
            mtm,
            "Show All",
            "",
            sel!(unhideAllApplications:),
        ));
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        app_menu.addItem(&responder_item(mtm, "Quit fesTerm", "q", sel!(terminate:)));

        let file = menu(mtm, "File");
        main.addItem(&submenu_root(mtm, "File", &file));
        file.addItem(&custom_item(
            mtm,
            "New Session…",
            "t",
            NSEventModifierFlags::Command,
            sel!(newSession:),
            &target,
        ));
        file.addItem(&custom_item(
            mtm,
            "New Window",
            "n",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(newWindow:),
            &target,
        ));
        file.addItem(&custom_item(
            mtm,
            "Start Local Shell",
            "n",
            NSEventModifierFlags::Command,
            sel!(startLocalShell:),
            &target,
        ));
        file.addItem(&NSMenuItem::separatorItem(mtm));
        let close_item = custom_item(
            mtm,
            "Close Session",
            "w",
            NSEventModifierFlags::Command,
            sel!(closeActiveSurface:),
            &target,
        );
        file.addItem(&close_item);
        let close_window = responder_item(mtm, "Close Window", "w", sel!(performClose:));
        close_window.setKeyEquivalentModifierMask(
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
        );
        file.addItem(&close_window);

        let edit = menu(mtm, "Edit");
        main.addItem(&submenu_root(mtm, "Edit", &edit));
        edit.addItem(&responder_item(mtm, "Copy", "", sel!(copy:)));
        edit.addItem(&custom_item(
            mtm,
            "Paste",
            "",
            NSEventModifierFlags::empty(),
            sel!(pasteFromClipboard:),
            &target,
        ));
        edit.addItem(&NSMenuItem::separatorItem(mtm));
        edit.addItem(&custom_item(
            mtm,
            "Clear Terminal",
            "k",
            NSEventModifierFlags::Command,
            sel!(clearTerminal:),
            &target,
        ));

        let shell = menu(mtm, "Shell");
        main.addItem(&submenu_root(mtm, "Shell", &shell));
        shell.addItem(&custom_item(
            mtm,
            "Reset Terminal",
            "r",
            NSEventModifierFlags::Command | NSEventModifierFlags::Option,
            sel!(resetTerminal:),
            &target,
        ));

        let view = menu(mtm, "View");
        main.addItem(&submenu_root(mtm, "View", &view));
        let palette = custom_item(
            mtm,
            "Command Palette…",
            "p",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(toggleCommandPalette:),
            &target,
        );
        view.addItem(&palette);
        view.addItem(&custom_item(
            mtm,
            "Toggle Focus Mode",
            "f",
            NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            sel!(toggleFocusMode:),
            &target,
        ));
        let inspector_item = custom_item(
            mtm,
            "Show Session Inspector",
            "",
            NSEventModifierFlags::empty(),
            sel!(toggleSessionInspector:),
            &target,
        );
        view.addItem(&inspector_item);

        let window = menu(mtm, "Window");
        main.addItem(&submenu_root(mtm, "Window", &window));
        window.addItem(&responder_item(
            mtm,
            "Minimize",
            "m",
            sel!(performMiniaturize:),
        ));
        window.addItem(&responder_item(mtm, "Zoom", "", sel!(performZoom:)));
        app.setWindowsMenu(Some(&window));

        app.setMainMenu(Some(&main));
        NativeMenu {
            main: Some(main),
            receiver: Some(receiver),
            _target: Some(target),
            close_item: Some(close_item.clone()),
            inspector_item: Some(inspector_item),
            shortcut_items: vec![
                ShortcutItem {
                    command: NativeMenuCommand::NewSession,
                    item: file.itemAtIndex(0).expect("new session menu item").clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::NewWindow,
                    item: file.itemAtIndex(1).expect("new window menu item").clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::StartLocalShell,
                    item: file
                        .itemAtIndex(2)
                        .expect("start local shell menu item")
                        .clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::CloseActiveSurface,
                    item: close_item.clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::OpenSettings,
                    item: app_menu.itemAtIndex(0).expect("settings menu item").clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::ClearTerminal,
                    item: edit
                        .itemAtIndex(3)
                        .expect("clear terminal menu item")
                        .clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::ResetTerminal,
                    item: shell
                        .itemAtIndex(0)
                        .expect("reset terminal menu item")
                        .clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::ToggleCommandPalette,
                    item: palette.clone(),
                },
                ShortcutItem {
                    command: NativeMenuCommand::ToggleFocusMode,
                    item: view
                        .itemAtIndex(1)
                        .expect("toggle focus mode menu item")
                        .clone(),
                },
            ],
            state_sync: NativeMenuSync::pending(),
            shortcut_sync: NativeMenuSync::pending(),
        }
    }

    fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
        NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title))
    }

    fn submenu_root(mtm: MainThreadMarker, title: &str, submenu: &NSMenu) -> Retained<NSMenuItem> {
        let item = responder_item(mtm, title, "", sel!(noop:));
        item.setSubmenu(Some(submenu));
        item
    }

    fn responder_item(
        mtm: MainThreadMarker,
        title: &str,
        key: &str,
        selector: objc2::runtime::Sel,
    ) -> Retained<NSMenuItem> {
        // SAFETY: selectors are compile-time AppKit responder selectors.
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                Some(selector),
                &NSString::from_str(key),
            )
        }
    }

    fn custom_item(
        mtm: MainThreadMarker,
        title: &str,
        key: &str,
        modifiers: NSEventModifierFlags,
        selector: objc2::runtime::Sel,
        target: &MenuTarget,
    ) -> Retained<NSMenuItem> {
        let item = responder_item(mtm, title, key, selector);
        item.setKeyEquivalentModifierMask(modifiers);
        // SAFETY: target implements every selector supplied to this helper and
        // is retained by NativeMenu for at least as long as the item.
        unsafe { item.setTarget(Some(target)) };
        item
    }
}

#[cfg(target_os = "macos")]
pub use menu::{install as install_application_menu, NativeMenu};

#[cfg(not(target_os = "macos"))]
pub struct NativeMenu;

#[cfg(not(target_os = "macos"))]
pub fn install_application_menu(_: std::sync::Arc<dyn Fn() + Send + Sync>) -> NativeMenu {
    NativeMenu
}

#[cfg(not(target_os = "macos"))]
impl NativeMenu {
    pub const fn unavailable() -> Self {
        Self
    }

    pub const fn try_recv(&self) -> Option<NativeMenuCommand> {
        None
    }

    pub const fn shortcuts_need_update(&self) -> bool {
        false
    }

    pub fn update(&mut self, _: &str, _: bool, _: bool) {}
    pub fn update_shortcuts(&mut self, _: &[NativeShortcut]) {}
}

#[cfg(target_os = "macos")]
use std::ptr::NonNull;

#[cfg(any(target_os = "macos", test))]
fn traffic_light_origin_y(
    superview_height: f64,
    band_center_from_top: f64,
    button_height: f64,
) -> f64 {
    superview_height - band_center_from_top - button_height / 2.0
}

/// Applies fesTerm's native window chrome to *every* window the process
/// owns: the traffic lights are aligned with the chip band, and AppKit's
/// own window dragging is disabled.
///
/// Every window is swept rather than one addressed by handle because only
/// eframe's root viewport exposes a `raw-window-handle`; the additional
/// windows fesTerm opens (ADR 0033) are egui viewports with no handle of
/// their own, and leaving them out left AppKit dragging those windows
/// whenever a tab chip in them was dragged - which made moving tabs out of
/// any window but the first impossible. Windows without traffic lights
/// (the tab-drag ghost) simply have nothing to align.
///
/// Callers are expected to call this every frame: it is idempotent (it
/// only ever assigns the exact target position, and only disables movement
/// that is still enabled) and windows opened later need the same treatment
/// as soon as they exist.
#[cfg(target_os = "macos")]
pub fn sync_window_chrome(band_center_from_top: f64) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let application = NSApplication::sharedApplication(main_thread);
    for window in application.windows() {
        offset_traffic_lights(&window, band_center_from_top);
        disable_native_window_movement(&window);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn sync_window_chrome(_: f64) {}

/// Vertically places macOS's standard traffic lights so their center sits
/// `band_center_from_top` points below the window's top edge, matching
/// fesTerm's integrated chrome band (`festerm_ui_egui::chrome::
/// chrome_band_center_from_top`).
///
/// This computes an absolute position from the window's own current height
/// rather than nudging AppKit's default placement by a fixed empirical
/// delta: an assumed default titlebar height can drift across macOS
/// versions, and a fixed one-time delta would go stale the moment the chip
/// row's height itself becomes runtime-configurable.
#[cfg(target_os = "macos")]
fn offset_traffic_lights(ns_window: &objc2_app_kit::NSWindow, band_center_from_top: f64) {
    use objc2_app_kit::NSWindowButton;
    use objc2_foundation::NSPoint;

    for button_kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let Some(button) = ns_window.standardWindowButton(button_kind) else {
            continue;
        };
        // The button's frame is relative to its immediate superview (a
        // small titlebar-container view living in the top-right corner of
        // the window chrome), not the window's own frame, so the "distance
        // from the top edge" must be computed against that superview's
        // height, not `ns_window.frame().size.height`.
        let Some(superview) = (unsafe { button.superview() }) else {
            continue;
        };
        let superview_height = superview.bounds().size.height;
        let frame = button.frame();
        // AppKit's window coordinate space has a bottom-left origin;
        // convert the desired distance from the top edge into that space.
        let target_origin_y =
            traffic_light_origin_y(superview_height, band_center_from_top, frame.size.height);
        if (frame.origin.y - target_origin_y).abs() > f64::EPSILON {
            button.setFrameOrigin(NSPoint::new(frame.origin.x, target_origin_y));
        }
    }
}

/// Disables AppKit's own default window-dragging behavior entirely (both
/// from the native title bar and from clicking the window background),
/// leaving 100% of window movement under fesTerm's own explicit
/// `egui::ViewportCommand::StartDrag` calls
/// (`festerm_ui_egui::chrome::show`'s row-level drag region).
///
/// This matters because `with_decorations(true)` (kept so the native
/// traffic lights keep working) plus `with_fullsize_content_view(true)`
/// still leaves a real, if visually blank, native title-bar strip across
/// the *entire* top of the window - AppKit drags the window from a
/// press-drag anywhere in that strip by default, before the event ever
/// reaches egui's own hit-testing. Since fesTerm's chip row paints its
/// title text inside that same strip, a drag started on a chip's title
/// used to move the whole window instead of reordering the chip - or
/// dragging it to another window - regardless of how egui's own widgets
/// resolved the same gesture. Disabling native movement removes that
/// OS-level shortcut entirely, so only the explicit drag regions fesTerm
/// itself defines can ever move the window.
#[cfg(target_os = "macos")]
fn disable_native_window_movement(ns_window: &objc2_app_kit::NSWindow) {
    if ns_window.isMovable() {
        ns_window.setMovable(false);
    }
}

/// Forces winit's own content `NSView` back to first responder so key events
/// resume being delivered after the OS window regains key/main status.
///
/// This works around a real AppKit gap (winit itself hits the same issue
/// internally - see its `set_style_mask`'s "If we don't do this, key
/// handling will break" comment): `windowDidBecomeKey`/egui's own
/// `WindowFocused(true)` fire reliably whenever the *window* becomes key
/// again, but AppKit does not always restore first-responder status to the
/// content view as part of that - most notably when the activating click
/// lands in the blank native title-bar strip above fesTerm's own chip row
/// (`disable_native_window_movement`'s doc comment explains why that strip
/// exists) rather than on any interactive view. In that case the window is
/// key, egui's own focus bookkeeping still thinks the terminal is focused,
/// but no keyDown ever reaches the app until the user separately clicks
/// inside real view content. Explicitly reclaiming first-responder status on
/// every regained-focus event closes that gap regardless of where the
/// activating click landed.
#[cfg(target_os = "macos")]
pub fn reclaim_first_responder(ns_view: NonNull<std::ffi::c_void>) {
    use objc2_app_kit::NSView;

    // SAFETY: winit supplies a live NSView pointer for the root window
    // handle; this function runs on the main thread while that window is
    // alive.
    let ns_view = unsafe { ns_view.cast::<NSView>().as_ref() };
    let Some(ns_window) = ns_view.window() else {
        return;
    };
    let _ = ns_window.makeFirstResponder(Some(ns_view));
}

#[cfg(not(target_os = "macos"))]
pub fn reclaim_first_responder(_: ()) {}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[cfg(target_os = "macos")]
    use super::OPEN_DOCUMENT_PATH_BYTE_CAPACITY;
    use super::{
        native_menu_state, traffic_light_origin_y, wake_after_enqueue, NativeMenuAction,
        NativeMenuCommand, NativeMenuState, NativeMenuSync, OpenDocumentBridge,
        OpenDocumentCallbacks, OpenDocumentEnqueueResult, OpenDocumentError, OpenDocumentPending,
        OwnedNativeMenuState, OPEN_DOCUMENT_ERROR_CAPACITY, OPEN_DOCUMENT_REQUEST_CAPACITY,
    };

    #[test]
    fn native_menu_actions_map_to_shared_commands() {
        let cases = [
            (NativeMenuAction::Paste, NativeMenuCommand::Paste),
            (NativeMenuAction::NewSession, NativeMenuCommand::NewSession),
            (NativeMenuAction::NewWindow, NativeMenuCommand::NewWindow),
            (
                NativeMenuAction::StartLocalShell,
                NativeMenuCommand::StartLocalShell,
            ),
            (
                NativeMenuAction::OpenSettings,
                NativeMenuCommand::OpenSettings,
            ),
            (
                NativeMenuAction::CloseActiveSurface,
                NativeMenuCommand::CloseActiveSurface,
            ),
            (
                NativeMenuAction::ToggleCommandPalette,
                NativeMenuCommand::ToggleCommandPalette,
            ),
            (
                NativeMenuAction::ToggleSessionInspector,
                NativeMenuCommand::ToggleSessionInspector,
            ),
            (
                NativeMenuAction::ClearTerminal,
                NativeMenuCommand::ClearTerminal,
            ),
            (
                NativeMenuAction::ResetTerminal,
                NativeMenuCommand::ResetTerminal,
            ),
            (
                NativeMenuAction::ToggleFocusMode,
                NativeMenuCommand::ToggleFocusMode,
            ),
        ];

        for (action, expected) in cases {
            assert_eq!(action.command(), expected);
        }
    }

    #[test]
    fn native_menu_state_tracks_surface_and_inspector_context() {
        assert_eq!(
            native_menu_state("Close Settings", false, false),
            NativeMenuState {
                close_label: "Close Settings",
                inspector_enabled: false,
                inspector_label: "Show Session Inspector",
            }
        );
        assert_eq!(
            native_menu_state("Close Session", true, true),
            NativeMenuState {
                close_label: "Close Session",
                inspector_enabled: true,
                inspector_label: "Hide Session Inspector",
            }
        );
    }

    #[test]
    fn native_menu_sync_skips_repeated_updates_but_retries_pending_work() {
        let mut sync = NativeMenuSync::pending();
        let requested = OwnedNativeMenuState {
            close_label: "Close Session".to_owned(),
            inspector_enabled: true,
            inspector_label: "Show Session Inspector",
        };
        assert!(sync.should_apply(&requested));

        sync.mark_applied(requested.clone());
        assert!(
            !sync.should_apply(&requested),
            "an unchanged applied state should not be re-sent"
        );

        sync.mark_pending();
        assert!(
            sync.should_apply(&requested),
            "a deferred startup update must retry even when the request is unchanged"
        );
    }

    #[test]
    fn traffic_light_origin_centers_button_in_chrome_band() {
        assert_eq!(traffic_light_origin_y(40.0, 14.0, 12.0), 20.0);
        assert_eq!(traffic_light_origin_y(20.0, 24.0, 12.0), -10.0);
    }

    #[test]
    fn open_document_bridge_buffers_paths_and_errors_until_drained() {
        let bridge = OpenDocumentBridge::unavailable();
        {
            let mut state = bridge.state.lock().expect("open document state");
            state.enqueue_path(PathBuf::from("/Users/example/one.txt"));
            state.enqueue_error(OpenDocumentError::NonFileUrl);
        }

        let drain = bridge.drain();
        assert_eq!(drain.paths, vec![PathBuf::from("/Users/example/one.txt")]);
        assert_eq!(drain.errors, vec![OpenDocumentError::NonFileUrl]);
        assert!(bridge.drain().paths.is_empty());
        assert!(bridge.drain().errors.is_empty());
    }

    #[test]
    fn open_document_bridge_bounds_requests_without_exposing_dropped_paths() {
        let bridge = OpenDocumentBridge::unavailable();
        {
            let mut state = bridge.state.lock().expect("open document state");
            for index in 0..(OPEN_DOCUMENT_REQUEST_CAPACITY + 2) {
                state.enqueue_path(PathBuf::from(format!("/Users/example/{index}.txt")));
            }
        }

        let drain = bridge.drain();
        assert_eq!(drain.paths.len(), OPEN_DOCUMENT_REQUEST_CAPACITY);
        assert_eq!(
            drain
                .errors
                .iter()
                .filter(|error| matches!(error, OpenDocumentError::RequestQueueFull { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn open_document_bridge_summarizes_error_overflow() {
        let bridge = OpenDocumentBridge::unavailable();
        {
            let mut state = bridge.state.lock().expect("open document state");
            for _ in 0..(OPEN_DOCUMENT_ERROR_CAPACITY + 3) {
                state.enqueue_error(OpenDocumentError::InvalidDocumentDescriptor);
            }
        }

        let drain = bridge.drain();
        assert_eq!(drain.errors.len(), OPEN_DOCUMENT_ERROR_CAPACITY);
        assert_eq!(
            drain.errors.last(),
            Some(&OpenDocumentError::ErrorQueueFull { dropped: 4 })
        );
    }

    #[test]
    fn open_document_errors_have_user_facing_display_text() {
        assert_eq!(
            OpenDocumentError::DocumentListTruncated { discarded: 7 }.to_string(),
            "open-document event exceeded the document limit; discarded 7 item(s)"
        );
        assert!(
            !OpenDocumentError::NonFileUrl
                .to_string()
                .contains("NonFileUrl"),
            "display text should not expose debug enum formatting"
        );
    }

    #[test]
    fn open_document_callbacks_receive_warm_events_without_buffering() {
        let bridge = OpenDocumentBridge::unavailable();
        let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        let path_sink = paths.clone();
        let error_sink = errors.clone();
        bridge.register_callbacks(OpenDocumentCallbacks::new(
            Arc::new(move |path| {
                path_sink.lock().expect("path sink").push(path);
                OpenDocumentEnqueueResult::Accepted
            }),
            Arc::new(move |error| {
                error_sink.lock().expect("error sink").push(error);
                OpenDocumentEnqueueResult::Accepted
            }),
        ));

        {
            let mut state = bridge.state.lock().expect("open document state");
            state.enqueue_path(PathBuf::from("/Users/example/warm.txt"));
            state.enqueue_error(OpenDocumentError::NonFileUrl);
        }

        assert_eq!(
            paths.lock().expect("path sink").as_slice(),
            &[PathBuf::from("/Users/example/warm.txt")]
        );
        assert_eq!(
            errors.lock().expect("error sink").as_slice(),
            &[OpenDocumentError::NonFileUrl]
        );
        assert_eq!(bridge.pending(), OpenDocumentPending::default());
    }

    #[test]
    fn open_document_callbacks_peek_and_retry_buffered_work_on_capacity() {
        let bridge = OpenDocumentBridge::unavailable();
        {
            let mut state = bridge.state.lock().expect("open document state");
            state.enqueue_path(PathBuf::from("/Users/example/first.txt"));
            state.enqueue_path(PathBuf::from("/Users/example/second.txt"));
        }

        let capacity = Arc::new(AtomicUsize::new(1));
        let delivered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let capacity_for_callback = capacity.clone();
        let delivered_for_callback = delivered.clone();
        let pending = bridge.register_callbacks(OpenDocumentCallbacks::new(
            Arc::new(move |path| {
                if capacity_for_callback
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                        value.checked_sub(1)
                    })
                    .is_ok()
                {
                    delivered_for_callback
                        .lock()
                        .expect("delivered paths")
                        .push(path);
                    OpenDocumentEnqueueResult::Accepted
                } else {
                    OpenDocumentEnqueueResult::Backpressured
                }
            }),
            Arc::new(|_| OpenDocumentEnqueueResult::Accepted),
        ));

        assert_eq!(pending.paths, 1);
        assert_eq!(
            delivered.lock().expect("delivered paths").as_slice(),
            &[PathBuf::from("/Users/example/first.txt")]
        );

        capacity.store(1, Ordering::SeqCst);
        assert_eq!(
            bridge.flush_pending_to_callbacks(),
            OpenDocumentPending::default()
        );
        assert_eq!(
            delivered.lock().expect("delivered paths").as_slice(),
            &[
                PathBuf::from("/Users/example/first.txt"),
                PathBuf::from("/Users/example/second.txt")
            ]
        );
    }

    #[test]
    fn open_document_bridge_wake_callback_is_replaceable_and_clearable() {
        let bridge = OpenDocumentBridge::unavailable();
        let first = Arc::new(AtomicUsize::new(0));
        let first_wake = first.clone();
        bridge.register_ui_wake_callback(Arc::new(move || {
            first_wake.fetch_add(1, Ordering::SeqCst);
        }));
        wake_after_enqueue(&bridge.state);
        assert_eq!(first.load(Ordering::SeqCst), 1);

        let second = Arc::new(AtomicUsize::new(0));
        let second_wake = second.clone();
        bridge.register_ui_wake_callback(Arc::new(move || {
            second_wake.fetch_add(1, Ordering::SeqCst);
        }));
        wake_after_enqueue(&bridge.state);
        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);

        bridge.clear_ui_wake_callback();
        wake_after_enqueue(&bridge.state);
        assert_eq!(second.load(Ordering::SeqCst), 1);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_document_appleevent_descriptor_coerces_file_urls_to_paths() {
        use objc2::rc::Retained;
        use objc2::{class, msg_send};
        use objc2_foundation::{NSAppleEventDescriptor, NSString, NSURL};

        const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
        const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
        const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

        let target = NSAppleEventDescriptor::currentProcessDescriptor();
        let event: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![
                class!(NSAppleEventDescriptor),
                appleEventWithEventClass: K_CORE_EVENT_CLASS,
                eventID: K_AE_OPEN_DOCUMENTS,
                targetDescriptor: &*target,
                returnID: 0i16,
                transactionID: 0i32,
            ]
        };
        let list = NSAppleEventDescriptor::listDescriptor();
        let first = PathBuf::from("/Users/example/fesTerm open with.txt");
        let second = PathBuf::from("/Users/example/second.md");
        for (index, expected) in [first.clone(), second.clone()].into_iter().enumerate() {
            let url = NSURL::fileURLWithPath(&NSString::from_str(
                expected.to_str().expect("utf-8 test path"),
            ));
            let file = NSAppleEventDescriptor::descriptorWithFileURL(&url);
            list.insertDescriptor_atIndex(&file, (index + 1) as isize);
        }
        unsafe {
            let _: () = msg_send![
                &*event,
                setParamDescriptor: &*list,
                forKeyword: KEY_DIRECT_OBJECT,
            ];
        }

        let parsed = super::open_documents::parse_event(&event);
        assert_eq!(parsed.paths, vec![first, second]);
        assert!(parsed.errors.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_document_appleevent_descriptor_processing_is_bounded() {
        use objc2::rc::Retained;
        use objc2::{class, msg_send};
        use objc2_foundation::{NSAppleEventDescriptor, NSString, NSURL};

        const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
        const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
        const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

        let target = NSAppleEventDescriptor::currentProcessDescriptor();
        let event: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![
                class!(NSAppleEventDescriptor),
                appleEventWithEventClass: K_CORE_EVENT_CLASS,
                eventID: K_AE_OPEN_DOCUMENTS,
                targetDescriptor: &*target,
                returnID: 0i16,
                transactionID: 0i32,
            ]
        };
        let list = NSAppleEventDescriptor::listDescriptor();
        for index in 0..(OPEN_DOCUMENT_REQUEST_CAPACITY + 2) {
            let path = PathBuf::from(format!("/Users/example/bounded-{index}.txt"));
            let url = NSURL::fileURLWithPath(&NSString::from_str(
                path.to_str().expect("utf-8 test path"),
            ));
            let file = NSAppleEventDescriptor::descriptorWithFileURL(&url);
            list.insertDescriptor_atIndex(&file, (index + 1) as isize);
        }
        unsafe {
            let _: () = msg_send![
                &*event,
                setParamDescriptor: &*list,
                forKeyword: KEY_DIRECT_OBJECT,
            ];
        }

        let parsed = super::open_documents::parse_event(&event);
        assert_eq!(parsed.paths.len(), OPEN_DOCUMENT_REQUEST_CAPACITY);
        assert_eq!(
            parsed.errors,
            vec![OpenDocumentError::DocumentListTruncated { discarded: 2 }]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_document_appleevent_rejects_oversized_paths_without_retaining_them() {
        use objc2::rc::Retained;
        use objc2::{class, msg_send};
        use objc2_foundation::{NSAppleEventDescriptor, NSString, NSURL};

        const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
        const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
        const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

        let target = NSAppleEventDescriptor::currentProcessDescriptor();
        let event: Retained<NSAppleEventDescriptor> = unsafe {
            msg_send![
                class!(NSAppleEventDescriptor),
                appleEventWithEventClass: K_CORE_EVENT_CLASS,
                eventID: K_AE_OPEN_DOCUMENTS,
                targetDescriptor: &*target,
                returnID: 0i16,
                transactionID: 0i32,
            ]
        };
        let oversized = format!(
            "/Users/example/{}",
            "x".repeat(OPEN_DOCUMENT_PATH_BYTE_CAPACITY + 1)
        );
        let url = NSURL::fileURLWithPath(&NSString::from_str(&oversized));
        let file = NSAppleEventDescriptor::descriptorWithFileURL(&url);
        let list = NSAppleEventDescriptor::listDescriptor();
        list.insertDescriptor_atIndex(&file, 1);
        unsafe {
            let _: () = msg_send![
                &*event,
                setParamDescriptor: &*list,
                forKeyword: KEY_DIRECT_OBJECT,
            ];
        }

        let parsed = super::open_documents::parse_event(&event);
        assert!(parsed.paths.is_empty());
        assert_eq!(
            parsed.errors,
            vec![OpenDocumentError::PathTooLong {
                bytes: oversized.len(),
                max_bytes: OPEN_DOCUMENT_PATH_BYTE_CAPACITY
            }]
        );
    }
}
