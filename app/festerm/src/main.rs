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
    // A reasonable default terminal size (~80 columns x 25 rows at the
    // default 14pt monospace font) rather than an arbitrary/oversized
    // window: approximated from typical monospace cell metrics (~9px
    // wide, ~18px tall at 14pt), plus room for the chrome band above
    // (top/bottom inset + chip row) and the status bar below.
    const APPROX_CELL_WIDTH: f32 = 9.0;
    const APPROX_CELL_HEIGHT: f32 = 18.0;
    const DEFAULT_COLUMNS: f32 = 80.0;
    const DEFAULT_ROWS: f32 = 25.0;
    const CHROME_HEIGHT: f32 = 8.0 + 38.0 + 8.0; // top inset + chip row + bottom inset
    const STATUS_BAR_HEIGHT: f32 = 24.0;
    const SIDE_INSET: f32 = 16.0 * 2.0;
    let default_width = DEFAULT_COLUMNS * APPROX_CELL_WIDTH + SIDE_INSET;
    let default_height = DEFAULT_ROWS * APPROX_CELL_HEIGHT + CHROME_HEIGHT + STATUS_BAR_HEIGHT;

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_decorations(false)
        .with_title(APPLICATION_TITLE)
        .with_inner_size([default_width, default_height])
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
