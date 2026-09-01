// Hide the console window a Windows binary otherwise defaults to (the
// "parent shell" behind the GUI window on launch). Kept for debug builds so
// `tracing`/`println!` diagnostics stay visible on the console while
// iterating locally.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod configuration_startup;
mod diagnostics;
mod environment;
mod inspector;
mod native_smoke;
mod overlay_state;
mod screens;
pub mod session_controller;
mod tabs;
mod updates;

use app::FesTermApp;
use configuration_startup::load as load_startup_configuration;

const APPLICATION_TITLE: &str = "fesTerm";
const APPLICATION_ICON_PNG: &[u8] = include_bytes!("../../../assets/app-icon/app-icon-256.png");

pub(crate) fn application_icon_data() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(APPLICATION_ICON_PNG)
        .expect("the committed fesTerm application icon must be a valid PNG")
}

fn main() -> eframe::Result<()> {
    diagnostics::init();
    tracing::info!(target: "festerm::app", "starting fesTerm");
    let startup_configuration = load_startup_configuration();

    // On macOS, retain the native traffic-light controls while allowing the
    // chip row to occupy the transparent titlebar's content area. Other
    // platforms keep the integrated custom controls in that same row.
    // A reasonable default terminal size (~80 columns x 25 rows at the
    // default 14pt monospace font) rather than an arbitrary/oversized
    // window: approximated from typical monospace cell metrics (~9px
    // wide, ~18px tall at 14pt), plus room for the chrome band above
    // (top inset + chip row; the terminal owns the one shared gap below)
    // and the status bar below.
    const APPROX_CELL_WIDTH: f32 = 9.0;
    const APPROX_CELL_HEIGHT: f32 = 18.0;
    const DEFAULT_COLUMNS: f32 = 80.0;
    const DEFAULT_ROWS: f32 = 25.0;
    const CHROME_HEIGHT: f32 = 8.0 + 34.0; // top inset + compact chip row
    const STATUS_BAR_HEIGHT: f32 = 24.0;
    const SIDE_INSET: f32 = 16.0 * 2.0;
    let default_width = DEFAULT_COLUMNS * APPROX_CELL_WIDTH + SIDE_INSET;
    let default_height = DEFAULT_ROWS * APPROX_CELL_HEIGHT + CHROME_HEIGHT + STATUS_BAR_HEIGHT;

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_decorations(cfg!(target_os = "macos"))
        .with_fullsize_content_view(cfg!(target_os = "macos"))
        .with_title_shown(!cfg!(target_os = "macos"))
        .with_titlebar_shown(!cfg!(target_os = "macos"))
        .with_title(APPLICATION_TITLE)
        .with_icon(application_icon_data())
        .with_inner_size([default_width, default_height])
        .with_min_inner_size([360.0, 240.0]);
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        APPLICATION_TITLE,
        options,
        Box::new(|creation_context| {
            let mut app = FesTermApp::with_startup_configuration(
                &creation_context.egui_ctx,
                startup_configuration,
            );
            app.install_native_menu(&creation_context.egui_ctx);
            app.install_wake_monitor(&creation_context.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_window_uses_the_committed_festerm_application_icon() {
        let icon = super::application_icon_data();
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
        assert!(
            icon.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[2] > pixel[0]),
            "the branded icon must retain its cyan prompt treatment"
        );
    }
}
