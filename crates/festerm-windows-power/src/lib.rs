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
const MAX_PRIVACY_LISTENERS: usize = 4;

#[cfg(any(windows, test))]
struct PrivacyListenerPermit<'a>(&'a std::sync::atomic::AtomicUsize);

#[cfg(any(windows, test))]
impl<'a> PrivacyListenerPermit<'a> {
    fn acquire(count: &'a std::sync::atomic::AtomicUsize) -> Option<Self> {
        count
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |current| {
                    if current < MAX_PRIVACY_LISTENERS {
                        Some(current + 1)
                    } else {
                        None
                    }
                },
            )
            .ok()
            .map(|_| Self(count))
    }
}

#[cfg(any(windows, test))]
impl Drop for PrivacyListenerPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

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

/// Fail-closed Windows privacy-loss notification guard.
///
/// Requires an active, unlocked WTS session, working WTS service, and native
/// suspend registration (Windows 8+). A dedicated message-only window receives
/// session lock/logoff, console/remote disconnection (including user switch),
/// and `PBT_APMSUSPEND`. Suspend is explicitly registered: message-only windows
/// do not receive ordinary broadcast messages. Service stop and message-loop
/// failure also revoke. The first loss permanently ends this guard; resume
/// and unlock never grant authority.
///
/// The callback runs synchronously on the listener thread and must not block,
/// panic, or drop this guard. Native message payloads are never logged or
/// dereferenced. Synthetic loss messages can only revoke, never grant.
/// Installation errors may invoke the callback on the installing thread.
/// Loss can precede `install` returning; latch it and retain this guard for
/// the entire time automation admission is open.
/// Reinstall before reopening admission: ordinary loss and fatal monitor
/// failure deliberately use the same callback, with no recovery notification.
/// Installation and teardown have bounded waits; a stalled native call may
/// leave a detached listener until that call returns. Outstanding listeners,
/// including stalled ones, are bounded per process.
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
    use std::cell::{Cell, RefCell};
    use std::io;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WAIT_IO_COMPLETION, WAIT_OBJECT_0, WPARAM,
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
    use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, SleepEx, INFINITE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        MsgWaitForMultipleObjectsEx, PeekMessageW, RegisterClassExW, TranslateMessage,
        UnregisterClassW, DEVICE_NOTIFY_WINDOW_HANDLE, HWND_MESSAGE, MSG, MWMO_ALERTABLE,
        MWMO_INPUTAVAILABLE, PBT_APMSUSPEND, PM_REMOVE, QS_ALLINPUT, WM_CLOSE, WM_DESTROY,
        WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_QUIT, WM_WTSSESSION_CHANGE,
        WNDCLASSEXW, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
        WTS_SESSION_LOGOFF,
    };

    use super::{session_is_available, PrivacyListenerPermit, PrivacyRevocation};

    const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
    const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
    const STARTUP_MESSAGE_LIMIT: usize = 64;

    static NEXT_CLASS: AtomicU64 = AtomicU64::new(0);
    static LIVE_LISTENERS: AtomicUsize = AtomicUsize::new(0);

    thread_local! {
        static CALLBACK: RefCell<Option<Arc<PrivacyRevocation>>> = const { RefCell::new(None) };
        static FAILED: Cell<bool> = const { Cell::new(false) };
    }

    fn revoke() {
        FAILED.set(true);
        CALLBACK.with(|slot| {
            if let Some(callback) = slot.borrow().as_ref() {
                callback.revoke();
            }
        });
    }

    unsafe extern "system" fn service_failed(_notification: *const core::ffi::c_void) {
        revoke();
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
            revoke();
            return 1;
        }
        if message == WM_CLOSE || message == WM_DESTROY {
            revoke();
            // Only our orderly teardown destroys the window. An unsolicited
            // WM_CLOSE must not silently disable the listener.
            return 0;
        }
        // SAFETY: the OS supplied these unchanged window-procedure arguments.
        unsafe { DefWindowProcW(window, message, wparam, lparam) }
    }

    struct Registration {
        class: Vec<u16>,
        instance: HANDLE,
        window: HWND,
        power: HPOWERNOTIFY,
        wts: bool,
        service: SC_HANDLE,
        service_notification: Option<Box<SERVICE_NOTIFY_2W>>,
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: all non-null handles belong to this thread. Closing the
            // service cancels its APC subscription before its buffer is freed.
            unsafe {
                if !self.service.is_null() && CloseServiceHandle(self.service) == 0 {
                    // A failed cancellation cannot prove that the OS has
                    // stopped using the notification buffer.
                    if let Some(buffer) = self.service_notification.take() {
                        let _ = Box::leak(buffer);
                    }
                }
                if self.power != 0 {
                    UnregisterSuspendResumeNotification(self.power);
                }
                if self.wts {
                    WTSUnRegisterSessionNotification(self.window);
                }
                if !self.window.is_null() {
                    DestroyWindow(self.window);
                }
                UnregisterClassW(self.class.as_ptr(), self.instance);
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
        fn install() -> io::Result<Self> {
            let class: Vec<u16> = format!(
                "fesTermPrivacyMonitor-{}-{}\0",
                std::process::id(),
                NEXT_CLASS.fetch_add(1, Ordering::Relaxed)
            )
            .encode_utf16()
            .collect();
            // SAFETY: null requests the current executable's module handle.
            let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let descriptor = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_procedure),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            // SAFETY: the class has a valid procedure and NUL-terminated name.
            if unsafe { RegisterClassExW(&descriptor) } == 0 {
                return Err(unavailable());
            }
            let mut registration = Self {
                class,
                instance,
                window: std::ptr::null_mut(),
                power: 0,
                wts: false,
                service: std::ptr::null_mut(),
                service_notification: Some(Box::new(SERVICE_NOTIFY_2W {
                    dwVersion: 2,
                    pfnNotifyCallback: Some(service_failed),
                    ..Default::default()
                })),
            };
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
                    return Err(unavailable());
                }
                registration.wts = true;
                registration.power = RegisterSuspendResumeNotification(
                    registration.window,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                );
                if registration.power == 0 {
                    return Err(unavailable());
                }
                let manager =
                    OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
                if manager.is_null() {
                    return Err(unavailable());
                }
                let service_name: Vec<u16> = "TermService\0".encode_utf16().collect();
                registration.service =
                    OpenServiceW(manager, service_name.as_ptr(), SERVICE_QUERY_STATUS);
                CloseServiceHandle(manager);
                if registration.service.is_null() {
                    return Err(unavailable());
                }
                if NotifyServiceStatusChangeW(
                    registration.service,
                    SERVICE_NOTIFY_STOPPED
                        | SERVICE_NOTIFY_STOP_PENDING
                        | SERVICE_NOTIFY_DELETE_PENDING
                        | SERVICE_NOTIFY_PAUSED
                        | SERVICE_NOTIFY_PAUSE_PENDING,
                    registration
                        .service_notification
                        .as_deref()
                        .expect("service notification buffer exists"),
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

    fn drain_startup_messages() -> bool {
        for _ in 0..STARTUP_MESSAGE_LIMIT {
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

    struct ShutdownEvent(usize);

    impl ShutdownEvent {
        fn signal(&self) {
            // SAFETY: the event stays open while this reference exists.
            unsafe { SetEvent(self.0 as HANDLE) };
        }
    }

    impl Drop for ShutdownEvent {
        fn drop(&mut self) {
            // SAFETY: this is the last owner of the event handle.
            unsafe { CloseHandle(self.0 as HANDLE) };
        }
    }

    struct CompletionSignal(mpsc::SyncSender<()>);

    impl Drop for CompletionSignal {
        fn drop(&mut self) {
            let _ = self.0.try_send(());
        }
    }

    pub struct PrivacyMonitor {
        shutdown: Arc<ShutdownEvent>,
        finished: mpsc::Receiver<()>,
        thread: Option<JoinHandle<()>>,
    }

    struct FailureGuard {
        revocation: Arc<PrivacyRevocation>,
        armed: bool,
    }

    impl Drop for FailureGuard {
        fn drop(&mut self) {
            if self.armed {
                self.revocation.revoke();
            }
        }
    }

    impl PrivacyMonitor {
        pub fn install(callback: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
            let revocation = Arc::new(PrivacyRevocation::new(callback));
            let Some(permit) = PrivacyListenerPermit::acquire(&LIVE_LISTENERS) else {
                revocation.revoke();
                return Err(unavailable());
            };
            // SAFETY: unnamed manual-reset event; no inherited/security state.
            let shutdown = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if shutdown.is_null() {
                revocation.revoke();
                return Err(unavailable());
            }
            let shutdown = Arc::new(ShutdownEvent(shutdown as usize));
            let thread_shutdown = shutdown.clone();
            let thread_revocation = revocation.clone();
            let (ready, started) = mpsc::sync_channel(1);
            let (done, finished) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("festerm-privacy-monitor".into())
                .spawn(move || {
                    let _completion = CompletionSignal(done);
                    let _permit = permit;
                    CALLBACK.with(|slot| *slot.borrow_mut() = Some(thread_revocation.clone()));
                    let registration = Registration::install();
                    let Ok(registration) = registration else {
                        thread_revocation.revoke();
                        CALLBACK.with(|slot| *slot.borrow_mut() = None);
                        let _ = ready.send(Err(unavailable()));
                        return;
                    };
                    // This guard is dropped before native subscriptions.
                    let mut failure = FailureGuard {
                        revocation: thread_revocation,
                        armed: true,
                    };
                    if !drain_startup_messages() {
                        return;
                    }
                    if ready.send(Ok(())).is_err() {
                        return;
                    }
                    let event = thread_shutdown.0 as HANDLE;
                    'listen: loop {
                        if FAILED.get() {
                            break;
                        }
                        // SAFETY: event outlives this thread. Alertable wait
                        // delivers the service-stop APC on this same thread.
                        let result = unsafe {
                            MsgWaitForMultipleObjectsEx(
                                1,
                                &event,
                                INFINITE,
                                QS_ALLINPUT,
                                MWMO_ALERTABLE | MWMO_INPUTAVAILABLE,
                            )
                        };
                        if result == WAIT_OBJECT_0 {
                            failure.armed = false;
                            break;
                        }
                        if FAILED.get() {
                            break;
                        }
                        if result == WAIT_IO_COMPLETION {
                            continue;
                        }
                        if result != WAIT_OBJECT_0 + 1 {
                            break;
                        }
                        let mut message = MSG::default();
                        // Process one message per wait so neither shutdown nor
                        // service failure can be starved by a message flood.
                        // SAFETY: message is valid writable local storage.
                        if unsafe {
                            PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE)
                        } != 0
                        {
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
                });
            let thread = match thread {
                Ok(thread) => thread,
                Err(error) => {
                    revocation.revoke();
                    return Err(error);
                }
            };
            let monitor = Self {
                shutdown,
                finished,
                thread: Some(thread),
            };
            match started.recv_timeout(STARTUP_TIMEOUT) {
                Ok(Ok(())) => Ok(monitor),
                _ => {
                    revocation.revoke();
                    Err(unavailable())
                }
            }
        }
    }

    impl Drop for PrivacyMonitor {
        fn drop(&mut self) {
            self.shutdown.signal();
            if let Some(thread) = self.thread.take() {
                let _ = self.finished.recv_timeout(SHUTDOWN_TIMEOUT);
                if thread.is_finished() {
                    let _ = thread.join();
                }
                // A stuck native call cannot hold the caller indefinitely.
                // The detached thread retains the event until it exits.
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn native_loss_dispatch_revokes_once_and_stops_the_listener() {
            use std::sync::atomic::AtomicUsize;

            FAILED.set(false);
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = calls.clone();
            CALLBACK.with(|slot| {
                *slot.borrow_mut() = Some(Arc::new(PrivacyRevocation::new(Arc::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                }))));
            });
            // SAFETY: both messages are handled without dereferencing hwnd or
            // lparam, and exercise the same dispatch as the native window.
            unsafe {
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
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            CALLBACK.with(|slot| *slot.borrow_mut() = None);
            FAILED.set(false);
        }

        #[test]
        fn loss_events_revoke_but_resume_and_unlock_do_not() {
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
    use super::{
        classify_windows_power_event, session_is_available, PrivacyListenerPermit,
        PrivacyRevocation, WakeEvent, WindowsPowerEventCodes, MAX_PRIVACY_LISTENERS,
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
    fn privacy_loss_synchronously_and_permanently_revokes() {
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

    #[test]
    fn stalled_listeners_cannot_exhaust_unbounded_slots() {
        let count = AtomicUsize::new(0);
        let mut permits: Vec<_> = (0..MAX_PRIVACY_LISTENERS)
            .map(|_| PrivacyListenerPermit::acquire(&count).unwrap())
            .collect();
        assert!(PrivacyListenerPermit::acquire(&count).is_none());
        drop(permits.pop());
        assert!(PrivacyListenerPermit::acquire(&count).is_some());
        drop(permits);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(PrivacyListenerPermit::acquire(&AtomicUsize::new(usize::MAX)).is_none());
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
