//! Own the native event loop: iOS cannot use desktop run-on-demand loops.

#[cfg(feature = "live-ssh")]
use festerm_mobile::SuspensionHandle;
use festerm_mobile::{HardwareKeys, Lifecycle, MobileApp};
use raw_window_handle::HasWindowHandle;
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, OnceLock},
};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, ElementState, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::Key,
    window::WindowId,
};

// Capture startup failures even when eframe cannot construct the application.
// This offline probe never logs terminal input; only host/renderer warnings.
struct StartupLogger;

impl log::Log for StartupLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

static STARTUP_LOGGER: StartupLogger = StartupLogger;

struct MobileHost<'a> {
    app: eframe::EframeWinitApplication<'a>,
    lifecycle: Rc<Cell<Lifecycle>>,
    keys: HardwareKeys,
    repaint: Arc<OnceLock<eframe::egui::Context>>,
    #[cfg(feature = "live-ssh")]
    suspension: SuspensionHandle,
}

impl MobileHost<'_> {
    fn update_lifecycle(&self, update: impl FnOnce(&mut Lifecycle)) {
        let mut state = self.lifecycle.get();
        update(&mut state);
        self.lifecycle.set(state);
    }
}

impl ApplicationHandler<eframe::UserEvent> for MobileHost<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.update_lifecycle(Lifecycle::resume);
        self.app.resumed(event_loop);
        festerm_ios_window::attach_scene_windows();
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(feature = "live-ssh")]
        self.suspension.suspend();
        self.update_lifecycle(Lifecycle::suspend);
        self.app.suspended(event_loop);
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.update_lifecycle(Lifecycle::memory_warning);
        self.app.memory_warning(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Never submit Metal drawables while UIKit has suspended the app.
        // Forward other events so resize/focus state remains current.
        if !self.lifecycle.get().active && matches!(event, WindowEvent::RedrawRequested) {
            return;
        }
        if let WindowEvent::KeyboardInput { event: key, .. } = &event {
            if key.state == ElementState::Pressed {
                if let Key::Character(text) = &key.logical_key {
                    if self.keys.push_text(text) {
                        if let Some(context) = self.repaint.get() {
                            context.request_repaint();
                        }
                    }
                }
            }
        }
        self.app.window_event(event_loop, id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if self.lifecycle.get().active {
            self.app.new_events(event_loop, cause);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        if self.lifecycle.get().active {
            self.app.user_event(event_loop, event);
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        if self.lifecycle.get().active {
            self.app.device_event(event_loop, id, event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.lifecycle.get().active {
            self.app.about_to_wait(event_loop);
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.app.exiting(event_loop);
    }
}

fn build_lock(repaint: Arc<OnceLock<eframe::egui::Context>>) -> festerm_ios_lock::AppLock {
    let store = festerm_mobile::Profiles::default_directory()
        .map(|directory| festerm_ios_lock::SettingsStore::new(directory.join("app-lock.json")));
    #[cfg(target_os = "ios")]
    let locker: Arc<dyn festerm_ios_lock::Locker> = Arc::new(festerm_ios_lock::LaContextLocker);
    // Desktop harness builds have no owner-authentication API.
    #[cfg(not(target_os = "ios"))]
    let locker: Arc<dyn festerm_ios_lock::Locker> =
        festerm_ios_lock::FakeLocker::new(festerm_ios_lock::Availability::Unavailable);
    festerm_ios_lock::AppLock::new(
        store,
        locker,
        Arc::new(festerm_ios_lock::SystemClock),
        move || {
            if let Some(context) = repaint.get() {
                context.request_repaint();
            }
        },
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    log::set_logger(&STARTUP_LOGGER).expect("single mobile logger");
    log::set_max_level(log::LevelFilter::Warn);
    eprintln!("festerm-mobile: starting native host");
    // iOS 27 terminates apps without a UIScene delegate; the class must exist
    // before UIApplicationMain reads the Info.plist scene manifest.
    festerm_ios_window::register_scene_delegate();
    let event_loop = EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let lifecycle = Rc::new(Cell::new(Lifecycle::default()));
    let app_lifecycle = lifecycle.clone();
    #[cfg(feature = "live-ssh")]
    let suspension = SuspensionHandle::default();
    #[cfg(feature = "live-ssh")]
    let app_suspension = suspension.clone();
    // The lock exists before the window so a cold launch never draws content.
    let repaint: Arc<OnceLock<eframe::egui::Context>> = Arc::new(OnceLock::new());
    let lock = build_lock(repaint.clone());
    let keys = HardwareKeys::default();
    let app_keys = keys.clone();
    let host_repaint = repaint.clone();
    let viewport = eframe::egui::ViewportBuilder::default();
    #[cfg(not(target_os = "ios"))]
    let viewport = viewport.with_inner_size([390.0, 844.0]);
    let app = eframe::create_native(
        "fesTerm Mobile Spike",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Wgpu,
            wgpu_options: festerm_mobile::mobile_wgpu_configuration(),
            run_and_return: false,
            viewport,
            ..Default::default()
        },
        Box::new(move |cc| {
            eprintln!("festerm-mobile: renderer initialized");
            let context = cc.egui_ctx.clone();
            let _ = repaint.set(context.clone());
            let bridge = cc
                .window_handle()
                .ok()
                .and_then(|handle| festerm_ios_lock::LockBridge::new(handle, lock.clone()));
            let keyboard = cc.window_handle().ok().and_then(|handle| {
                festerm_ios_window::KeyboardBridge::new(handle, move || context.request_repaint())
            });
            let app = MobileApp::new(app_lifecycle)
                .with_keyboard(keyboard)
                .with_hardware_keys(app_keys)
                .with_lock(lock, bridge);
            let app = match festerm_mobile::Profiles::open_default() {
                Ok(profiles) => app.with_profiles(profiles),
                Err(error) => {
                    eprintln!("festerm-mobile: saved profiles unavailable: {error}");
                    app
                }
            };
            #[cfg(feature = "live-ssh")]
            let app = app.with_suspension(app_suspension);
            Ok(Box::new(app))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut MobileHost {
        app,
        lifecycle,
        keys,
        repaint: host_repaint,
        #[cfg(feature = "live-ssh")]
        suspension,
    })?;
    Ok(())
}
