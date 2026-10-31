//! Tiny Simulator/device test host for the app lock. It is not the product UI:
//! green = sensitive content drawn, orange = lock screen. State changes are
//! appended to `$HOME/Documents/lock-host.log`.
//!
//! Environment (Simulator: prefix with `SIMCTL_CHILD_`):
//! `LOCK_HOST_GRACE` = always|1m|5m|15m|never (default 1m),
//! `LOCK_HOST_AUTOUNLOCK=1` starts one unlock per lock episode.
#![cfg_attr(not(target_os = "ios"), allow(unused))]

#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("lock_host runs on iOS only");
}

#[cfg(target_os = "ios")]
fn main() {
    host::run();
}

#[cfg(target_os = "ios")]
mod host {
    use block2::RcBlock;
    use festerm_ios_lock::{
        AppLock, BackgroundGrace, LaContextLocker, LockBridge, LockSettings, LockState,
        SettingsStore, SystemClock,
    };
    use objc2::{define_class, rc::Retained, ClassType, MainThreadOnly};
    use objc2_foundation::{
        MainThreadMarker, NSDictionary, NSObject, NSObjectProtocol, NSString, NSTimer,
    };
    use objc2_ui_kit::{
        UIApplication, UIApplicationDelegate, UIColor, UILabel, UIScreen, UIView,
        UIViewAutoresizing, UIViewController, UIWindow,
    };
    use raw_window_handle::{RawWindowHandle, UiKitWindowHandle, WindowHandle};
    use std::{
        cell::{Cell, RefCell},
        io::Write,
        path::PathBuf,
        ptr::NonNull,
        sync::Arc,
    };

    struct Host {
        _window: Retained<UIWindow>,
        _bridge: std::rc::Rc<LockBridge>,
        _timer: Retained<NSTimer>,
    }

    thread_local! {
        static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "LockHostDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UIApplicationDelegate for Delegate {
            #[unsafe(method(application:didFinishLaunchingWithOptions:))]
            fn did_finish_launching(
                &self,
                _application: &UIApplication,
                _options: Option<&NSDictionary>,
            ) -> bool {
                let mtm = self.mtm();
                HOST.with(|host| *host.borrow_mut() = Some(start(mtm)));
                true
            }
        }
    );

    fn documents() -> PathBuf {
        let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Documents");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn log(line: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(documents().join("lock-host.log"))
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn label(
        mtm: MainThreadMarker,
        parent: &UIView,
        color: &UIColor,
        text: &str,
    ) -> Retained<UIView> {
        let view = UIView::initWithFrame(mtm.alloc(), parent.bounds());
        view.setBackgroundColor(Some(color));
        view.setAutoresizingMask(
            UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight,
        );
        let text_view = UILabel::initWithFrame(mtm.alloc(), view.bounds());
        text_view.setText(Some(&NSString::from_str(text)));
        text_view.setTextAlignment(objc2_ui_kit::NSTextAlignment::Center);
        view.addSubview(&text_view);
        parent.addSubview(&view);
        view
    }

    #[allow(deprecated)]
    fn start(mtm: MainThreadMarker) -> Host {
        let grace = match std::env::var("LOCK_HOST_GRACE").as_deref() {
            Ok("always") => BackgroundGrace::Always,
            Ok("5m") => BackgroundGrace::Minutes5,
            Ok("15m") => BackgroundGrace::Minutes15,
            Ok("never") => BackgroundGrace::Never,
            _ => BackgroundGrace::Minutes1,
        };
        let store = SettingsStore::new(documents().join("lock.json"));
        let settings = LockSettings {
            enabled: true,
            background_grace: grace,
            ..LockSettings::default()
        };
        let _ = store.save(&settings);
        // Constructed before any view exists: cold launch is already Locked.
        let lock = AppLock::new(
            Some(store),
            Arc::new(LaContextLocker),
            Arc::new(SystemClock),
            || {},
        );
        log(&format!(
            "launch state={:?} draw={} availability={:?}",
            lock.state(),
            lock.should_draw_content(),
            lock.availability()
        ));

        let window = UIWindow::initWithFrame(mtm.alloc(), UIScreen::mainScreen(mtm).bounds());
        let controller = UIViewController::new(mtm);
        window.setRootViewController(Some(&controller));
        let root = controller.view().expect("root view");
        let content = label(
            mtm,
            &root,
            &UIColor::greenColor(),
            "SENSITIVE TERMINAL CONTENT",
        );
        let lock_screen = label(mtm, &root, &UIColor::orangeColor(), "LOCKED");
        window.makeKeyAndVisible();

        let handle = UiKitWindowHandle::new(
            NonNull::new(Retained::as_ptr(&root).cast_mut().cast()).expect("view pointer"),
        );
        // SAFETY: `root` outlives the handle's use inside `LockBridge::new`.
        let window_handle = unsafe { WindowHandle::borrow_raw(RawWindowHandle::UiKit(handle)) };
        let bridge =
            std::rc::Rc::new(LockBridge::new(window_handle, lock.clone()).expect("lock bridge"));
        let tick_bridge = bridge.clone();

        let autounlock = std::env::var("LOCK_HOST_AUTOUNLOCK").as_deref() == Ok("1");
        let last = RefCell::new(String::new());
        let attempted = Cell::new(false);
        let timer_lock = lock.clone();
        // The bridge is also synced by its own notification observers; the
        // timer plays the role of the host's per-frame callback.
        let tick = RcBlock::new(move |_timer: NonNull<NSTimer>| {
            let draw = timer_lock.should_draw_content();
            content.setHidden(!draw);
            lock_screen.setHidden(draw);
            timer_lock.note_frame_presented();
            tick_bridge.sync();
            let state = timer_lock.state();
            if state == LockState::Unlocked {
                attempted.set(false);
            }
            if autounlock && state == LockState::Locked && !attempted.get() {
                attempted.set(true);
                timer_lock.begin_unlock();
            }
            let line = format!(
                "state={state:?} draw={draw} cover={} failure={:?}",
                timer_lock.shield_should_cover(),
                timer_lock.last_failure()
            );
            if *last.borrow() != line {
                log(&line);
                *last.borrow_mut() = line;
            }
        });
        // SAFETY: the timer fires on the main run loop, where this block lives.
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &tick) };
        Host {
            _window: window,
            _bridge: bridge,
            _timer: timer,
        }
    }

    pub fn run() {
        let mtm = MainThreadMarker::new().expect("main thread");
        let delegate = NSString::from_class(Delegate::class());
        UIApplication::main(None, Some(&delegate), mtm);
    }
}
