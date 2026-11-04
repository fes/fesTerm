//! Windows resume-from-suspend wake notification support for fesTerm.
//!
//! Creates a hidden message-only window on a dedicated thread solely to
//! receive `WM_POWERBROADCAST` and calls back on resume, matching ADR 0018's
//! "resume from system sleep" wake trigger for an on-demand SSH liveness
//! probe. Network-interface/route-change detection is deliberately out of
//! scope here; see issue #48 for that follow-up.
//!
//! The separate [`PrivacyMonitor`] requires native session/suspend
//! subscriptions and reports unavailable platforms/services as errors rather
//! than adopting [`WakeMonitor`]'s best-effort behavior.

#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WakeEvent {
    Wake,
    Ignore,
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
struct WindowsPowerEventCodes {
    power_broadcast: u32,
    resume_automatic: usize,
    resume_suspend: usize,
}

#[cfg(any(windows, test))]
const fn classify_windows_power_event(
    message: u32,
    wparam: usize,
    codes: WindowsPowerEventCodes,
) -> WakeEvent {
    if message == codes.power_broadcast
        && (wparam == codes.resume_automatic || wparam == codes.resume_suspend)
    {
        WakeEvent::Wake
    } else {
        WakeEvent::Ignore
    }
}

#[cfg(any(windows, test))]
struct PrivacyRevocation {
    callback: std::sync::Arc<dyn Fn() + Send + Sync>,
    revoked: std::sync::atomic::AtomicBool,
}

#[cfg(any(windows, test))]
impl PrivacyRevocation {
    fn new(callback: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            callback,
            revoked: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn revoke(&self) {
        if !self.revoked.swap(true, std::sync::atomic::Ordering::AcqRel) {
            (self.callback)();
        }
    }
}

#[cfg(any(windows, test))]
mod privacy_lifecycle;

#[cfg(windows)]
pub use privacy_lifecycle::{
    privacy_monitor_ownership, PrivacyCleanupState, PrivacyCleanupStatus, PrivacyInstallFailure,
    PrivacyOwnershipState, RejectedPrivacyInstall,
};

#[cfg(any(windows, test))]
const fn session_is_available(
    expected_id: u32,
    reported_id: u32,
    active: bool,
    flags: i32,
    unlocked_flag: i32,
) -> bool {
    expected_id == reported_id && active && flags == unlocked_flag
}

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread::{self, JoinHandle};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostMessageW, PostQuitMessage, RegisterClassExW, TranslateMessage, HWND_MESSAGE, MSG,
        PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, WM_CLOSE, WM_DESTROY, WM_POWERBROADCAST,
        WNDCLASSEXW,
    };

    use super::{classify_windows_power_event, WakeEvent, WindowsPowerEventCodes};

    const POWER_EVENT_CODES: WindowsPowerEventCodes = WindowsPowerEventCodes {
        power_broadcast: WM_POWERBROADCAST,
        resume_automatic: PBT_APMRESUMEAUTOMATIC as usize,
        resume_suspend: PBT_APMRESUMESUSPEND as usize,
    };

    /// UTF-16, NUL-terminated `"fesTermWakeMonitorWindowClass"`.
    const CLASS_NAME: &[u16] = &[
        0x0066, 0x0065, 0x0073, 0x0054, 0x0065, 0x0072, 0x006D, 0x0057, 0x0061, 0x006B, 0x0065,
        0x004D, 0x006F, 0x006E, 0x0069, 0x0074, 0x006F, 0x0072, 0x0057, 0x0069, 0x006E, 0x0064,
        0x006F, 0x0077, 0x0043, 0x006C, 0x0061, 0x0073, 0x0073, 0x0000,
    ];

    thread_local! {
        // The window procedure is a plain `extern "system" fn` with no
        // closure-capture support. Each `WakeMonitor` owns exactly one
        // message-only window on its own dedicated thread, so a
        // thread-local slot (set once before the message loop starts) is
        // simpler than `GWLP_USERDATA` and equally correct here.
        static WAKE: RefCell<Option<Arc<dyn Fn() + Send + Sync>>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn window_procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if classify_windows_power_event(message, wparam, POWER_EVENT_CODES) == WakeEvent::Wake {
            WAKE.with(|wake| {
                if let Some(wake) = wake.borrow().as_ref() {
                    wake();
                }
            });
            return 1;
        }
        if message == WM_DESTROY {
            // SAFETY: no preconditions; this only posts WM_QUIT to this
            // thread's own message queue so `GetMessageW` below returns.
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        if message == WM_CLOSE {
            // SAFETY: `window` is this message-only window, still valid on
            // its own message-loop thread.
            unsafe { DestroyWindow(window) };
            return 0;
        }
        // SAFETY: `window`, `message`, `wparam`, and `lparam` are exactly the
        // parameters the message loop received for this window procedure.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    /// Runs a hidden message-only window on a dedicated thread for as long as
    /// the `WakeMonitor` stays alive, invoking `wake` once per resume from
    /// suspend (`WM_POWERBROADCAST` / `PBT_APMRESUME*`). Dropping it closes
    /// the window and joins the thread.
    pub struct WakeMonitor {
        thread: Option<JoinHandle<()>>,
        window: HWND,
    }

    // The only cross-thread use of `window` is `PostMessageW`, which the
    // Win32 documentation states is safe to call from any thread.
    unsafe impl Send for WakeMonitor {}

    impl WakeMonitor {
        pub fn install(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
            let (window_sender, window_receiver) = mpsc::channel::<Option<usize>>();
            let thread = thread::Builder::new()
                .name("festerm-wake-monitor".to_owned())
                .spawn(move || {
                    WAKE.with(|slot| *slot.borrow_mut() = Some(wake));
                    // SAFETY: a null module name resolves to this process's
                    // own executable module, which outlives the class and
                    // window registered against it below.
                    let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
                    let class = WNDCLASSEXW {
                        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                        style: 0,
                        lpfnWndProc: Some(window_procedure),
                        cbClsExtra: 0,
                        cbWndExtra: 0,
                        hInstance: hinstance,
                        hIcon: std::ptr::null_mut(),
                        hCursor: std::ptr::null_mut(),
                        hbrBackground: std::ptr::null_mut(),
                        lpszMenuName: std::ptr::null(),
                        lpszClassName: CLASS_NAME.as_ptr(),
                        hIconSm: std::ptr::null_mut(),
                    };
                    // SAFETY: `class` is a fully initialized `WNDCLASSEXW`
                    // with a valid, NUL-terminated `lpszClassName` and a
                    // valid window-procedure pointer.
                    if unsafe { RegisterClassExW(&class) } == 0 {
                        let _ = window_sender.send(None);
                        return;
                    }
                    // SAFETY: `CLASS_NAME` was just registered on this
                    // thread above; `HWND_MESSAGE` creates a message-only
                    // window with no visible surface or taskbar presence.
                    let window = unsafe {
                        CreateWindowExW(
                            0,
                            CLASS_NAME.as_ptr(),
                            std::ptr::null(),
                            0,
                            0,
                            0,
                            0,
                            0,
                            HWND_MESSAGE,
                            std::ptr::null_mut(),
                            hinstance,
                            std::ptr::null(),
                        )
                    };
                    if window.is_null() {
                        let _ = window_sender.send(None);
                        return;
                    }
                    let _ = window_sender.send(Some(window as usize));

                    let mut message = MSG::default();
                    loop {
                        // SAFETY: `message` is exclusively owned by this
                        // thread for the duration of the call.
                        let result =
                            unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
                        if result <= 0 {
                            break;
                        }
                        // SAFETY: `message` was just populated by the
                        // `GetMessageW` call above.
                        unsafe {
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                })
                .expect("wake-monitor thread should spawn");

            let window = window_receiver
                .recv()
                .ok()
                .flatten()
                .map(|raw| raw as HWND)
                .unwrap_or(std::ptr::null_mut());

            Self {
                thread: Some(thread),
                window,
            }
        }
    }

    impl Drop for WakeMonitor {
        fn drop(&mut self) {
            if !self.window.is_null() {
                // SAFETY: `self.window` is the message-only window created
                // in `install`, still valid until its own thread destroys
                // it in response to this message.
                unsafe {
                    PostMessageW(self.window, WM_CLOSE, 0, 0);
                }
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[cfg(windows)]
pub use imp::WakeMonitor;

#[cfg(not(windows))]
pub struct WakeMonitor;

#[cfg(not(windows))]
impl WakeMonitor {
    pub fn install(_wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }
}

/// Windows privacy-loss notification monitor; native qualification is pending.
///
/// Requires an active, unlocked WTS session, working WTS service, and native
/// suspend registration (Windows 8+). A dedicated message-only window receives
/// session lock/logoff, console/remote disconnection (including user switch),
/// and `PBT_APMSUSPEND`. Suspend is explicitly registered: message-only windows
/// do not receive ordinary broadcast messages. Service stop and message-loop
/// failure also request shutdown and attempt a loss callback. A loss does not
/// itself establish revocation; resume and unlock never grant authority.
///
/// The callback runs outside native dispatch, on the listener or, on a
/// supervisor failure, the reaper. It and its captured destructors must not
/// block or panic, but a callback can block on the application's admission
/// lock. A caught panic can still reach the process-wide panic hook. Native
/// message payloads are never logged or dereferenced. Synthetic loss messages
/// cannot grant authority. A loss can precede `install` returning; neither
/// callback entry nor completion proves admission closed.
///
/// Installation can block and must run off the UI thread. Dropping or closing
/// the monitor only requests shutdown; it never waits for the listener. The
/// owner must independently close admission before requesting shutdown and
/// retain a [`PrivacyCleanupStatus`] until cleanup completes. An installation
/// error can contain [`PrivacyInstallFailure`] with the same status. Pending
/// or unknown cleanup prevents another monitor from being installed. A single
/// process-lifetime reaper owns the listener through actual thread join,
/// including TLS destruction; uncertain native cancellation is quarantined.
/// Inputs rejected before acquiring that slot are returned in the installation
/// error; dropping those inputs may block or panic and remains the caller's
/// responsibility.
/// At its own deadline the owner can permanently quarantine a still-pending
/// cleanup using [`PrivacyCleanupStatus::quarantine_if_unresolved`].
/// Any non-active state is unavailable. Neither cleanup status nor callback
/// delivery qualifies admission, privacy, or a physical final-recipient fence.
/// No monitor state authorizes disclosure; MCP bootstrap remains closed.
#[cfg(windows)]
pub use privacy::PrivacyMonitor;

#[cfg(not(windows))]
pub struct PrivacyMonitor;

#[cfg(not(windows))]
impl PrivacyMonitor {
    pub fn install(_callback: std::sync::Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "WTS privacy monitoring requires Windows",
        ))
    }
}

#[cfg(windows)]
mod privacy {
    use std::cell::{Cell, RefCell, UnsafeCell};
    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, OnceLock};
    use std::thread;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        HANDLE, HWND, LPARAM, LRESULT, WAIT_IO_COMPLETION, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Power::{
        RegisterSuspendResumeNotification, UnregisterSuspendResumeNotification, HPOWERNOTIFY,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        ProcessIdToSessionId, WTSActive, WTSFreeMemory, WTSQuerySessionInformationW,
        WTSRegisterSessionNotification, WTSSessionInfoEx, WTSUnRegisterSessionNotification,
        NOTIFY_FOR_THIS_SESSION, WTSINFOEXW, WTS_CURRENT_SERVER_HANDLE, WTS_SESSIONSTATE_UNLOCK,
    };
    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, NotifyServiceStatusChangeW, OpenSCManagerW, OpenServiceW,
        QueryServiceStatusEx, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
        SERVICE_NOTIFY_2W, SERVICE_NOTIFY_DELETE_PENDING, SERVICE_NOTIFY_PAUSED,
        SERVICE_NOTIFY_PAUSE_PENDING, SERVICE_NOTIFY_STOPPED, SERVICE_NOTIFY_STOP_PENDING,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
    };
    use windows_sys::Win32::System::Threading::SleepEx;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        MsgWaitForMultipleObjectsEx, PeekMessageW, RegisterClassExW, TranslateMessage,
        UnregisterClassW, DEVICE_NOTIFY_WINDOW_HANDLE, HWND_MESSAGE, MSG, MWMO_ALERTABLE,
        MWMO_INPUTAVAILABLE, PBT_APMSUSPEND, PM_REMOVE, QS_ALLINPUT, WM_CLOSE, WM_DESTROY,
        WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT, WM_WTSSESSION_CHANGE,
        WNDCLASSEXW, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
        WTS_SESSION_LOGOFF,
    };

    use super::privacy_lifecycle::{self, invoke_revocation, Lifecycle, PrivacyCleanupStatus};
    use super::{session_is_available, PrivacyRevocation};

    const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
    const STARTUP_MESSAGE_LIMIT: usize = 64;
    const SHUTDOWN_POLL_MILLIS: u32 = 50;

    static NEXT_CLASS: AtomicU64 = AtomicU64::new(0);

    struct Callback {
        lifecycle: Arc<Lifecycle>,
    }

    thread_local! {
        static CALLBACK: RefCell<Option<Callback>> = const { RefCell::new(None) };
        static FAILED: Cell<bool> = const { Cell::new(false) };
    }

    fn note_loss() {
        let _ = FAILED.try_with(|failed| failed.set(true));
        let _ = CALLBACK.try_with(|slot| {
            let Ok(callback) = slot.try_borrow() else {
                return;
            };
            if let Some(callback) = callback.as_ref() {
                let _ = callback.lifecycle.stop();
            }
        });
    }

    unsafe extern "system" fn service_failed(_notification: *const core::ffi::c_void) {
        note_loss();
    }

    fn loss(message: u32, wparam: usize) -> bool {
        (message == WM_POWERBROADCAST && wparam == PBT_APMSUSPEND as usize)
            || (message == WM_WTSSESSION_CHANGE
                && matches!(
                    wparam as u32,
                    WTS_SESSION_LOCK
                        | WTS_SESSION_LOGOFF
                        | WTS_CONSOLE_DISCONNECT
                        | WTS_REMOTE_DISCONNECT
                ))
            || message == WM_QUERYENDSESSION
            || (message == WM_ENDSESSION && wparam != 0)
    }

    unsafe extern "system" fn window_procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if loss(message, wparam) {
            note_loss();
            return 1;
        }
        if message == WM_CLOSE || message == WM_DESTROY {
            note_loss();
            // Only our orderly teardown destroys the window. An unsolicited
            // WM_CLOSE must not silently disable the listener.
            return 0;
        }
        // SAFETY: the OS supplied these unchanged window-procedure arguments.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    struct NotificationBuffer(UnsafeCell<SERVICE_NOTIFY_2W>);

    // The single process slot permits access only on its listener. Reuse
    // requires successful service cancellation and the listener's actual
    // join; APCs already queued to that thread are lost when it terminates.
    // The storage itself persists even if cancellation cannot be established.
    unsafe impl Send for NotificationBuffer {}
    unsafe impl Sync for NotificationBuffer {}

    static SERVICE_NOTIFICATION: OnceLock<NotificationBuffer> = OnceLock::new();

    fn service_notification() -> *const SERVICE_NOTIFY_2W {
        let buffer = SERVICE_NOTIFICATION
            .get_or_init(|| NotificationBuffer(UnsafeCell::new(SERVICE_NOTIFY_2W::default())));
        // SAFETY: this listener exclusively holds the process slot. A previous
        // listener cannot release that slot before its join and cancellation.
        unsafe {
            buffer.0.get().write(SERVICE_NOTIFY_2W {
                dwVersion: 2,
                pfnNotifyCallback: Some(service_failed),
                ..Default::default()
            });
        }
        buffer.0.get()
    }

    struct Registration {
        lifecycle: Arc<Lifecycle>,
        revocation: Arc<PrivacyRevocation>,
        class: Vec<u16>,
        class_registered: bool,
        instance: HANDLE,
        window: HWND,
        power: HPOWERNOTIFY,
        wts: bool,
        manager: SC_HANDLE,
        service: SC_HANDLE,
    }

    impl Registration {
        fn quarantine(&self) -> ! {
            self.lifecycle.unknown();
            invoke_revocation(&self.revocation, &self.lifecycle);
            loop {
                std::hint::black_box(self);
                thread::park();
            }
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            if self.lifecycle.stop() || FAILED.get() {
                invoke_revocation(&self.revocation, &self.lifecycle);
            }
            // SAFETY: the handles and window belong to this listener thread.
            // Any uncertain cancellation retains this entire registration;
            // the service notification buffer is process-lifetime storage.
            unsafe {
                if !self.service.is_null() && CloseServiceHandle(self.service) == 0 {
                    self.quarantine();
                }
                self.service = std::ptr::null_mut();
                if !self.manager.is_null() && CloseServiceHandle(self.manager) == 0 {
                    self.quarantine();
                }
                self.manager = std::ptr::null_mut();
                if self.power != 0 && UnregisterSuspendResumeNotification(self.power) == 0 {
                    self.quarantine();
                }
                self.power = 0;
                if self.wts && WTSUnRegisterSessionNotification(self.window) == 0 {
                    self.quarantine();
                }
                self.wts = false;
                if !self.window.is_null() && DestroyWindow(self.window) == 0 {
                    self.quarantine();
                }
                self.window = std::ptr::null_mut();
                if self.class_registered
                    && UnregisterClassW(self.class.as_ptr(), self.instance) == 0
                {
                    self.quarantine();
                }
                self.class_registered = false;
            }
        }
    }

    fn unavailable() -> io::Error {
        io::Error::new(io::ErrorKind::NotConnected, "native Windows privacy monitoring unavailable (WTS session/service or suspend registration)")
    }

    fn active_session() -> io::Result<()> {
        let mut session_id = 0;
        let mut buffer = std::ptr::null_mut();
        let mut bytes = 0;
        // SAFETY: output pointers are writable and the WTS result is released
        // using the matching WTS allocator after checking its size and level.
        unsafe {
            if ProcessIdToSessionId(std::process::id(), &mut session_id) == 0
                || WTSQuerySessionInformationW(
                    WTS_CURRENT_SERVER_HANDLE,
                    session_id,
                    WTSSessionInfoEx,
                    &mut buffer,
                    &mut bytes,
                ) == 0
            {
                if !buffer.is_null() {
                    WTSFreeMemory(buffer.cast());
                }
                return Err(unavailable());
            }
            let active = if !buffer.is_null() && bytes as usize >= std::mem::size_of::<WTSINFOEXW>()
            {
                let info = &*buffer.cast::<WTSINFOEXW>();
                if info.Level == 1 {
                    let session = &info.Data.WTSInfoExLevel1;
                    session_is_available(
                        session_id,
                        session.SessionId,
                        session.SessionState == WTSActive,
                        session.SessionFlags,
                        WTS_SESSIONSTATE_UNLOCK as i32,
                    )
                } else {
                    false
                }
            } else {
                false
            };
            if !buffer.is_null() {
                WTSFreeMemory(buffer.cast());
            }
            if active {
                Ok(())
            } else {
                Err(unavailable())
            }
        }
    }

    impl Registration {
        fn install(
            lifecycle: Arc<Lifecycle>,
            revocation: Arc<PrivacyRevocation>,
        ) -> io::Result<Self> {
            let mut class_id = NEXT_CLASS.load(Ordering::Relaxed);
            loop {
                let next = class_id.checked_add(1).ok_or_else(unavailable)?;
                match NEXT_CLASS.compare_exchange_weak(
                    class_id,
                    next,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(current) => class_id = current,
                }
            }
            let class: Vec<u16> = format!(
                "fesTermPrivacyMonitor-{}-{}\0",
                std::process::id(),
                class_id
            )
            .encode_utf16()
            .collect();
            // SAFETY: null requests the current executable's module handle.
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            if instance.is_null() {
                return Err(unavailable());
            }
            let mut registration = Self {
                lifecycle,
                revocation,
                class,
                class_registered: false,
                instance,
                window: std::ptr::null_mut(),
                power: 0,
                wts: false,
                manager: std::ptr::null_mut(),
                service: std::ptr::null_mut(),
            };
            let descriptor = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_procedure),
                hInstance: instance,
                lpszClassName: registration.class.as_ptr(),
                ..Default::default()
            };
            // SAFETY: the class has a valid procedure and NUL-terminated name.
            if unsafe { RegisterClassExW(&descriptor) } == 0 {
                registration.quarantine();
            }
            registration.class_registered = true;
            // SAFETY: class/instance remain alive through window destruction.
            registration.window = unsafe {
                CreateWindowExW(
                    0,
                    registration.class.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                )
            };
            if registration.window.is_null() {
                return Err(unavailable());
            }
            // SAFETY: a valid message-only window is the native notification
            // recipient. Explicit registration covers non-broadcast windows.
            unsafe {
                if WTSRegisterSessionNotification(registration.window, NOTIFY_FOR_THIS_SESSION) == 0
                {
                    registration.quarantine();
                }
                registration.wts = true;
                registration.power = RegisterSuspendResumeNotification(
                    registration.window,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                );
                if registration.power == 0 {
                    registration.quarantine();
                }
                registration.manager =
                    OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
                if registration.manager.is_null() {
                    return Err(unavailable());
                }
                let service_name: Vec<u16> = "TermService\0".encode_utf16().collect();
                registration.service = OpenServiceW(
                    registration.manager,
                    service_name.as_ptr(),
                    SERVICE_QUERY_STATUS,
                );
                if CloseServiceHandle(registration.manager) == 0 {
                    registration.quarantine();
                }
                registration.manager = std::ptr::null_mut();
                if registration.service.is_null() {
                    return Err(unavailable());
                }
                // This service-only mask excludes CREATED/DELETED, whose
                // notification would allocate a service-name list.
                if NotifyServiceStatusChangeW(
                    registration.service,
                    SERVICE_NOTIFY_STOPPED
                        | SERVICE_NOTIFY_STOP_PENDING
                        | SERVICE_NOTIFY_DELETE_PENDING
                        | SERVICE_NOTIFY_PAUSED
                        | SERVICE_NOTIFY_PAUSE_PENDING,
                    service_notification(),
                ) != 0
                {
                    return Err(unavailable());
                }
                let mut status = SERVICE_STATUS_PROCESS::default();
                let mut needed = 0;
                if QueryServiceStatusEx(
                    registration.service,
                    SC_STATUS_PROCESS_INFO,
                    (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
                    std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
                    &mut needed,
                ) == 0
                    || status.dwCurrentState != SERVICE_RUNNING
                {
                    return Err(unavailable());
                }
            }
            active_session()?;
            Ok(registration)
        }
    }

    fn drain_startup_messages(lifecycle: &Lifecycle) -> bool {
        for _ in 0..STARTUP_MESSAGE_LIMIT {
            if lifecycle.cancelled() {
                return false;
            }
            // SAFETY: an alertable zero-duration wait dispatches any already
            // queued service notification APC on this listener thread.
            let result = unsafe { SleepEx(0, 1) };
            if FAILED.get() {
                return false;
            }
            if result == WAIT_IO_COMPLETION {
                continue;
            }
            if result != 0 {
                return false;
            }
            let mut message = MSG::default();
            // SAFETY: the message is writable, thread-local storage.
            if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
                return !FAILED.get();
            }
            if message.message == WM_QUIT {
                return false;
            }
            // SAFETY: PeekMessageW initialized this message.
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        false
    }

    pub struct PrivacyMonitor {
        cleanup: PrivacyCleanupStatus,
    }

    struct FailureGuard {
        revocation: Arc<PrivacyRevocation>,
        lifecycle: Arc<Lifecycle>,
        armed: bool,
    }

    impl FailureGuard {
        fn should_stop(&mut self, failed: bool) -> bool {
            if failed {
                return true;
            }
            if self.lifecycle.cancelled() {
                self.armed = false;
                return true;
            }
            false
        }
    }

    impl Drop for FailureGuard {
        fn drop(&mut self) {
            let _ = self.lifecycle.stop();
            if self.armed {
                invoke_revocation(&self.revocation, &self.lifecycle);
            }
        }
    }

    fn listen(
        lifecycle: Arc<Lifecycle>,
        revocation: Arc<PrivacyRevocation>,
        ready: mpsc::SyncSender<bool>,
    ) -> bool {
        CALLBACK.with(|slot| {
            *slot.borrow_mut() = Some(Callback {
                lifecycle: lifecycle.clone(),
            });
        });
        let mut failure = FailureGuard {
            revocation: revocation.clone(),
            lifecycle: lifecycle.clone(),
            armed: true,
        };
        if lifecycle.cancelled() {
            failure.armed = false;
            return true;
        }
        let registration = match Registration::install(lifecycle.clone(), revocation) {
            Ok(registration) => registration,
            Err(_) => {
                let _ = ready.try_send(false);
                return true;
            }
        };
        // On every later return, report loss before native teardown can stall.
        let mut failure = failure;
        if !drain_startup_messages(&lifecycle) || !lifecycle.activate() {
            return true;
        }
        if ready.try_send(true).is_err() {
            return true;
        }
        'listen: loop {
            if failure.should_stop(FAILED.get()) {
                break;
            }
            // SAFETY: the alertable wait dispatches this thread's service
            // APCs. The finite timeout observes shutdown without a UI-side
            // native handle operation.
            let result = unsafe {
                MsgWaitForMultipleObjectsEx(
                    0,
                    std::ptr::null(),
                    SHUTDOWN_POLL_MILLIS,
                    QS_ALLINPUT,
                    MWMO_ALERTABLE | MWMO_INPUTAVAILABLE,
                )
            };
            if failure.should_stop(FAILED.get()) {
                break;
            }
            if result == WAIT_IO_COMPLETION || result == WAIT_TIMEOUT {
                continue;
            }
            if result != WAIT_OBJECT_0 {
                break;
            }
            let mut message = MSG::default();
            // Process one message per wait so shutdown and service failure
            // cannot be starved by a message flood.
            // SAFETY: message is valid writable local storage.
            if unsafe { PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                if message.message == WM_QUIT {
                    break 'listen;
                }
                // SAFETY: PeekMessageW initialized the message.
                unsafe {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        drop(failure);
        CALLBACK.with(|slot| *slot.borrow_mut() = None);
        drop(registration);
        true
    }

    impl PrivacyMonitor {
        /// Starts monitor ownership off the UI thread. Neither its status nor
        /// callback delivery qualifies admission, privacy, or recipient
        /// fencing; MCP bootstrap remains closed.
        pub fn install(callback: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
            let revocation = Arc::new(PrivacyRevocation::new(callback));
            let (ready, started) = mpsc::sync_channel(1);
            let cleanup = privacy_lifecycle::launch(revocation, move |lifecycle, revocation| {
                listen(lifecycle, revocation, ready)
            })?;
            match started.recv_timeout(STARTUP_TIMEOUT) {
                Ok(true) if cleanup.is_active() => Ok(Self { cleanup }),
                _ => {
                    cleanup.stop();
                    Err(privacy_lifecycle::unavailable(Some(cleanup)))
                }
            }
        }

        /// Observes monitor cleanup only. Neither this status nor callback
        /// delivery qualifies admission, privacy, or recipient fencing; MCP
        /// bootstrap remains closed.
        pub fn cleanup_status(&self) -> PrivacyCleanupStatus {
            self.cleanup.clone()
        }

        /// Requests shutdown without waiting; retain the returned status until
        /// cleanup is complete or treated as unknown. Neither that status nor
        /// callback delivery qualifies admission, privacy, or physical
        /// recipient fencing; MCP bootstrap remains closed.
        pub fn close(self) -> PrivacyCleanupStatus {
            let cleanup = self.cleanup_status();
            drop(self);
            cleanup
        }
    }

    impl Drop for PrivacyMonitor {
        fn drop(&mut self) {
            self.cleanup.stop();
        }
    }

    #[cfg(test)]
    mod tests {
        // These simulated notifications do not qualify admission, privacy, or
        // final-recipient fencing. Callback delivery is not a fence; bootstrap
        // remains closed.
        use super::*;

        #[test]
        fn native_loss_dispatch_attempts_callback_once_after_dispatch() {
            use std::sync::atomic::AtomicUsize;

            FAILED.set(false);
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let lifecycle = privacy_lifecycle::test_lifecycle();
            let revocation = Arc::new(PrivacyRevocation::new(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            })));
            CALLBACK.with(|slot| {
                *slot.borrow_mut() = Some(Callback {
                    lifecycle: lifecycle.clone(),
                });
            });
            CALLBACK.with(|slot| {
                let _borrow = slot.borrow_mut();
                // SAFETY: the service callback ignores its native argument.
                unsafe { service_failed(std::ptr::null()) };
            });
            assert!(FAILED.get());
            assert!(!lifecycle.cancelled());
            FAILED.set(false);
            // SAFETY: these callbacks do not dereference their native
            // arguments and use the listener thread's test-local state.
            unsafe {
                service_failed(std::ptr::null());
                window_procedure(
                    std::ptr::null_mut(),
                    WM_WTSSESSION_CHANGE,
                    WTS_SESSION_LOCK as usize,
                    0,
                );
                window_procedure(
                    std::ptr::null_mut(),
                    WM_POWERBROADCAST,
                    PBT_APMSUSPEND as usize,
                    0,
                );
            }
            assert!(FAILED.get());
            assert!(lifecycle.cancelled());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let mut failure = FailureGuard {
                revocation: revocation.clone(),
                lifecycle: lifecycle.clone(),
                armed: true,
            };
            assert!(failure.should_stop(FAILED.get()));
            drop(failure);
            invoke_revocation(&revocation, &lifecycle);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            CALLBACK.with(|slot| *slot.borrow_mut() = None);
            FAILED.set(false);
        }

        #[test]
        fn ordinary_active_shutdown_disarms_the_failure_guard() {
            use std::sync::atomic::AtomicUsize;

            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            let lifecycle = privacy_lifecycle::test_lifecycle();
            assert!(lifecycle.activate());
            let mut failure = FailureGuard {
                revocation: Arc::new(PrivacyRevocation::new(Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }))),
                lifecycle: lifecycle.clone(),
                armed: true,
            };
            assert!(!failure.should_stop(false));
            assert!(lifecycle.stop());
            assert!(failure.should_stop(false));
            drop(failure);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }

        #[test]
        fn loss_event_classification_excludes_resume_and_unlock() {
            use windows_sys::Win32::UI::WindowsAndMessaging::{
                PBT_APMRESUMEAUTOMATIC, WTS_SESSION_UNLOCK,
            };
            assert!(loss(WM_POWERBROADCAST, PBT_APMSUSPEND as usize));
            for event in [
                WTS_SESSION_LOCK,
                WTS_SESSION_LOGOFF,
                WTS_CONSOLE_DISCONNECT,
                WTS_REMOTE_DISCONNECT,
            ] {
                assert!(loss(WM_WTSSESSION_CHANGE, event as usize));
            }
            assert!(!loss(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize));
            assert!(!loss(WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize));
            assert!(!loss(0, WTS_SESSION_LOCK as usize));
            assert!(!loss(WM_ENDSESSION, 0));
            assert!(loss(WM_ENDSESSION, 1));
        }
    }
}

#[cfg(test)]
mod tests {
    // Test callbacks and their completion do not qualify admission, privacy,
    // or final-recipient fencing; MCP bootstrap remains closed.
    use super::{
        classify_windows_power_event, session_is_available, PrivacyRevocation, WakeEvent,
        WindowsPowerEventCodes,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    const CODES: WindowsPowerEventCodes = WindowsPowerEventCodes {
        power_broadcast: 10,
        resume_automatic: 20,
        resume_suspend: 30,
    };

    #[test]
    fn automatic_and_user_visible_resume_events_wake() {
        assert_eq!(classify_windows_power_event(10, 20, CODES), WakeEvent::Wake);
        assert_eq!(classify_windows_power_event(10, 30, CODES), WakeEvent::Wake);
    }

    #[test]
    fn unrelated_messages_and_power_events_are_ignored() {
        assert_eq!(
            classify_windows_power_event(11, 20, CODES),
            WakeEvent::Ignore
        );
        assert_eq!(
            classify_windows_power_event(10, 40, CODES),
            WakeEvent::Ignore
        );
    }

    #[test]
    fn initial_session_must_match_and_be_known_active_and_unlocked() {
        assert!(session_is_available(7, 7, true, 1, 1));
        assert!(!session_is_available(7, 8, true, 1, 1));
        assert!(!session_is_available(7, 7, false, 1, 1));
        assert!(!session_is_available(7, 7, true, 0, 1));
        assert!(!session_is_available(7, 7, true, -1, 1));
        assert!(!session_is_available(7, 7, true, 3, 1));
    }

    #[test]
    fn privacy_loss_invokes_test_callback_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let revocation = PrivacyRevocation::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        revocation.revoke();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        revocation.revoke();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_privacy_losses_invoke_callback_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let revocation = Arc::new(PrivacyRevocation::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        let start = Arc::new(Barrier::new(5));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let revocation = revocation.clone();
                let start = start.clone();
                scope.spawn(move || {
                    start.wait();
                    revocation.revoke();
                });
            }
            start.wait();
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn privacy_monitor_refuses_other_operating_systems() {
        let error = super::PrivacyMonitor::install(std::sync::Arc::new(|| {}))
            .err()
            .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}
