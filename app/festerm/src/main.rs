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

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        APPLICATION_TITLE,
        options,
        Box::new(|creation_context| Ok(Box::new(FesTermApp::new(&creation_context.egui_ctx)))),
    )
}
