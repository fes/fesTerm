#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use festerm_macos_window::{
        install_open_document_bridge, OpenDocumentCallbacks, OpenDocumentEnqueueResult,
        OpenDocumentError, OpenDocumentPending,
    };
    use objc2::encode::{Encode, Encoding, RefEncode};
    use objc2::rc::Retained;
    use objc2::{class, msg_send};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{
        MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSString, NSURL,
    };

    const K_CORE_EVENT_CLASS: u32 = 0x6165_7674;
    const K_AE_OPEN_DOCUMENTS: u32 = 0x6f64_6f63;
    const KEY_DIRECT_OBJECT: u32 = 0x2d2d_2d2d;

    #[repr(C)]
    struct OpaqueAeDataStorageType {
        _private: [u8; 0],
    }

    unsafe impl RefEncode for OpaqueAeDataStorageType {
        const ENCODING_REF: Encoding =
            Encoding::Pointer(&Encoding::Struct("OpaqueAEDataStorageType", &[]));
    }

    type AeDataStorage = *mut *mut OpaqueAeDataStorageType;

    #[repr(C, packed(2))]
    struct AeDesc {
        descriptor_type: u32,
        data_handle: AeDataStorage,
    }

    unsafe impl Encode for AeDesc {
        const ENCODING: Encoding =
            Encoding::Struct("AEDesc", &[<u32>::ENCODING, <AeDataStorage>::ENCODING]);
    }

    unsafe impl RefEncode for AeDesc {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
    }

    pub fn run() {
        let bridge = install_open_document_bridge()
            .expect("open-document bridge must install on test binary main thread");
        assert!(bridge.is_installed());
        assert!(matches!(
            install_open_document_bridge(),
            Err(festerm_macos_window::OpenDocumentBridgeInstallError::AlreadyInstalled)
        ));

        let cold = PathBuf::from("/Users/example/cold open.txt");
        dispatch_open_documents([file_descriptor(&cold)]);
        assert_eq!(
            bridge.pending(),
            OpenDocumentPending {
                paths: 1,
                errors: 0
            }
        );

        let mtm = MainThreadMarker::new().expect("test binary main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.finishLaunching();
        let launched = PathBuf::from("/Users/example/during launch.md");
        dispatch_open_documents([file_descriptor(&launched)]);
        assert_eq!(
            bridge.pending(),
            OpenDocumentPending {
                paths: 2,
                errors: 0
            },
            "AppKit launch must not steal document events before the UI attaches"
        );

        let capacity = Arc::new(AtomicUsize::new(0));
        let delivered_paths = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let delivered_errors = Arc::new(Mutex::new(Vec::<OpenDocumentError>::new()));
        let path_capacity = capacity.clone();
        let path_sink = delivered_paths.clone();
        let error_sink = delivered_errors.clone();
        let pending = bridge.register_callbacks(OpenDocumentCallbacks::new(
            Arc::new(move |path| {
                if path_capacity
                    .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                        value.checked_sub(1)
                    })
                    .is_ok()
                {
                    path_sink.lock().expect("delivered path mutex").push(path);
                    OpenDocumentEnqueueResult::Accepted
                } else {
                    OpenDocumentEnqueueResult::Backpressured
                }
            }),
            Arc::new(move |error| {
                error_sink
                    .lock()
                    .expect("delivered error mutex")
                    .push(error);
                OpenDocumentEnqueueResult::Accepted
            }),
        ));
        assert_eq!(
            pending,
            OpenDocumentPending {
                paths: 2,
                errors: 0
            }
        );
        assert!(delivered_paths
            .lock()
            .expect("delivered path mutex")
            .is_empty());

        let warm = PathBuf::from("/Users/example/warm open.md");
        dispatch_open_documents([file_descriptor(&warm)]);
        assert!(
            delivered_paths
                .lock()
                .expect("delivered path mutex")
                .is_empty(),
            "warm event must not bypass the backpressured cold event"
        );
        assert_eq!(
            bridge.pending(),
            OpenDocumentPending {
                paths: 3,
                errors: 0
            }
        );

        capacity.store(3, Ordering::SeqCst);
        assert_eq!(
            bridge.flush_pending_to_callbacks(),
            OpenDocumentPending::default()
        );
        assert_eq!(
            delivered_paths
                .lock()
                .expect("delivered path mutex")
                .as_slice(),
            &[cold, launched, warm]
        );

        dispatch_open_documents([non_file_descriptor()]);
        assert_eq!(
            delivered_errors
                .lock()
                .expect("delivered error mutex")
                .as_slice(),
            &[OpenDocumentError::NonFileUrl]
        );
    }

    fn dispatch_open_documents(
        descriptors: impl IntoIterator<Item = Retained<NSAppleEventDescriptor>>,
    ) {
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
        for (index, descriptor) in descriptors.into_iter().enumerate() {
            list.insertDescriptor_atIndex(&descriptor, (index + 1) as isize);
        }
        unsafe {
            let _: () = msg_send![
                &*event,
                setParamDescriptor: &*list,
                forKeyword: KEY_DIRECT_OBJECT,
            ];
        }

        let reply = NSAppleEventDescriptor::nullDescriptor();
        let event_desc: *mut AeDesc = unsafe { msg_send![&*event, aeDesc] };
        let reply_desc: *mut AeDesc = unsafe { msg_send![&*reply, aeDesc] };
        let err: i16 = unsafe {
            msg_send![
                &*NSAppleEventManager::sharedAppleEventManager(),
                dispatchRawAppleEvent: event_desc,
                withRawReply: reply_desc,
                handlerRefCon: std::ptr::null_mut::<core::ffi::c_void>(),
            ]
        };
        assert_eq!(err, 0, "NSAppleEventManager dispatch failed");
    }

    fn file_descriptor(path: &Path) -> Retained<NSAppleEventDescriptor> {
        let url = NSURL::fileURLWithPath(&NSString::from_str(
            path.to_str().expect("test path must be UTF-8"),
        ));
        NSAppleEventDescriptor::descriptorWithFileURL(&url)
    }

    fn non_file_descriptor() -> Retained<NSAppleEventDescriptor> {
        let url = NSURL::URLWithString(&NSString::from_str("https://example.invalid/file.txt"))
            .expect("valid non-file URL");
        NSAppleEventDescriptor::descriptorWithFileURL(&url)
    }
}
