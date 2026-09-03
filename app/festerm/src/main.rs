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
mod markdown_viewer;
mod native_smoke;
mod overlay_state;
mod port_forward_draft;
mod screens;
mod search;
pub mod session_controller;
mod sftp_file_manager;
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
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: windows_drag_friendly_surface_config(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        APPLICATION_TITLE,
        options,
        Box::new(|creation_context| {
            log_wgpu_adapter(creation_context);
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

/// Chooses the wgpu surface present mode, avoiding vsync-locked presentation
/// on Windows.
///
/// Windows' native window-drag interaction runs its own modal message loop
/// (entered on `WM_ENTERSIZEMOVE`) that pumps a `WM_MOVING` message, and
/// expects the application to present a repainted frame synchronously for
/// each one to keep the window's content tracking the mouse. eframe's
/// default surface configuration
/// (`egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT`) uses
/// `wgpu::PresentMode::AutoVsync`, which on Windows' common DX12/Vulkan
/// backends resolves to `Fifo`: each present call blocks until the
/// display's next vsync interval. Inside that per-message modal loop, that
/// wait makes the window's redrawn content visibly lag behind the actual
/// window frame the OS is already moving, which is what shows up as jank
/// or jitter while dragging.
///
/// `AutoNoVsync` (falling back to `Fifo` only if a backend truly has no
/// alternative) removes that wait, at the cost of allowing tearing when
/// the frame rate exceeds the display's refresh rate — an acceptable
/// trade for a GUI that is idle almost all the time between user input.
/// macOS is left on the default: its Metal/`CAMetalLayer` presentation
/// path does not couple window-drag responsiveness to the app's own
/// present timing the way Windows' DX12/Vulkan swapchain does, so it does
/// not show the same symptom and keeps the smoother default vsync
/// behavior.
fn windows_drag_friendly_surface_config() -> eframe::egui_wgpu::SurfaceConfig {
    if cfg!(target_os = "windows") {
        eframe::egui_wgpu::SurfaceConfig {
            present_mode: eframe::wgpu::PresentMode::AutoNoVsync,
            ..eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
        }
    } else {
        eframe::egui_wgpu::SurfaceConfig::HIGH_THROUGHPUT
    }
}

/// Logs the GPU adapter `wgpu` actually selected for rendering (name,
/// backend, device type) so a slow-rendering report can be diagnosed
/// without guessing whether the OS handed us an integrated GPU, a software
/// (WARP/CPU) adapter, or fell back to the GL translation backend instead
/// of a native accelerated one.
fn log_wgpu_adapter(creation_context: &eframe::CreationContext<'_>) {
    match creation_context.wgpu_render_state.as_ref() {
        Some(render_state) => {
            let info = render_state.adapter.get_info();
            tracing::info!(
                target: "festerm::app",
                adapter_name = %info.name,
                backend = ?info.backend,
                device_type = ?info.device_type,
                driver = %info.driver,
                driver_info = %info.driver_info,
                "wgpu selected a rendering adapter"
            );
        }
        None => {
            tracing::warn!(
                target: "festerm::app",
                "no wgpu render state available (a non-wgpu backend is active)"
            );
        }
    }
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
