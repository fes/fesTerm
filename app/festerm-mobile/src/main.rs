//! Own the native event loop: iOS cannot use desktop run-on-demand loops.

use festerm_mobile::{Lifecycle, MobileApp};
use raw_window_handle::HasWindowHandle;
use std::{cell::Cell, rc::Rc};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
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
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    log::set_logger(&STARTUP_LOGGER).expect("single mobile logger");
    log::set_max_level(log::LevelFilter::Warn);
    eprintln!("festerm-mobile: starting native host");
    let event_loop = EventLoop::<eframe::UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let lifecycle = Rc::new(Cell::new(Lifecycle::default()));
    let app_lifecycle = lifecycle.clone();
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
            let keyboard = cc.window_handle().ok().and_then(|handle| {
                festerm_ios_window::KeyboardBridge::new(handle, move || context.request_repaint())
            });
            Ok(Box::new(
                MobileApp::new(app_lifecycle).with_keyboard(keyboard),
            ))
        }),
        &event_loop,
    );
    event_loop.run_app(&mut MobileHost { app, lifecycle })?;
    Ok(())
}
