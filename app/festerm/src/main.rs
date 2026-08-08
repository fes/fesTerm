mod app;
mod diagnostics;
mod native_smoke;
mod screens;
pub mod session_controller;
mod tabs;

use app::FesTermApp;

const APPLICATION_TITLE: &str = "fesTerm";

fn main() -> eframe::Result<()> {
    diagnostics::init();
    tracing::info!(target: "festerm::app", "starting fesTerm");

    // The session-chip chrome row lives in the same band as the window's
    // own minimize/maximize/close controls (`docs/gui-design.md` "native
    // min/max/close window buttons directly in the same band as the
    // chips (no separate OS title bar at all)"), so native decorations are
    // disabled here and `app.rs` paints its own custom title-bar controls
    // and drag region instead.
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_decorations(false)
        .with_title(APPLICATION_TITLE)
        .with_min_inner_size([360.0, 240.0]);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        APPLICATION_TITLE,
        options,
        Box::new(|creation_context| Ok(Box::new(FesTermApp::new(&creation_context.egui_ctx)))),
    )
}
