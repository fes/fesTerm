//! Windows resume-from-suspend wake notification support for fesTerm.
//!
//! Creates a hidden message-only window on a dedicated thread solely to
//! receive `WM_POWERBROADCAST` and calls back on resume, matching ADR 0018's
//! "resume from system sleep" wake trigger for an on-demand SSH liveness
//! probe. Network-interface/route-change detection is deliberately out of
//! scope here; see issue #48 for that follow-up.

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

#[cfg(test)]
mod tests {
    use super::{classify_windows_power_event, WakeEvent, WindowsPowerEventCodes};

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
}
