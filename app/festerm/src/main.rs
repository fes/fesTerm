// Hide the console window a Windows binary otherwise defaults to (the
// "parent shell" behind the GUI window on launch). Kept for debug builds so
// `tracing`/`println!` diagnostics stay visible on the console while
// iterating locally.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod application;
mod configuration_startup;
mod diagnostics;
mod discovery;
// Every entry point here is exercised by its own tests and is called for real
// once the editor tab lands; the allowance goes with it.
#[allow(dead_code)]
mod document_store;
#[allow(dead_code)]
mod documents;
mod environment;
mod inspector;
mod keyboard;
mod local_command;
mod markdown_viewer;
mod multiplexer_sessions;
mod native_smoke;
mod overlay_state;
mod port_forward_draft;
mod save_as;
mod screens;
mod search;
pub mod session_controller;
mod sftp_file_manager;
mod software_background;
mod tabs;
mod terminal_paths;
mod text_compare;
mod text_editor;
#[cfg(test)]
mod ui_gallery;
mod updates;
mod vi_command;

use app::FesTermApp;
use application::FesTermApplication;
use configuration_startup::load as load_startup_configuration;

pub(crate) const APPLICATION_TITLE: &str = "fesTerm";
const APPLICATION_ICON_PNG: &[u8] = include_bytes!("../../../assets/app-icon/app-icon-256.png");

/// The window width fesTerm opens at, sized to an 80-column terminal at the
/// default 14pt monospace font (~9px cells) plus the 16px inset on each side.
///
/// Every surface has to be usable at this width, not just the terminal, so
/// `screens` tests its settings layout against this constant rather than
/// against a comfortable width nobody actually starts at.
pub(crate) const DEFAULT_WINDOW_WIDTH: f32 = 80.0 * 9.0 + 16.0 * 2.0;

/// The window height fesTerm opens at: ~25 terminal rows at the default 14pt
/// monospace font (~18px cells), plus the chrome band above (top inset and
/// compact chip row) and the status bar below.
///
/// Shared with `application`, so an additional window (ADR 0032) opens at the
/// same size as the first rather than at an arbitrary default.
pub(crate) const DEFAULT_WINDOW_HEIGHT: f32 = 25.0 * 18.0 + (8.0 + 34.0) + 24.0;

pub(crate) fn application_icon_data() -> eframe::egui::IconData {
    eframe::icon_data::from_png_bytes(APPLICATION_ICON_PNG)
        .expect("the committed fesTerm application icon must be a valid PNG")
}

/// The chrome every fesTerm window is built with, whether it is the first one
/// or an additional one (ADR 0032).
///
/// On macOS the native titlebar is hidden but the window keeps its decorations,
/// so the traffic lights stay while the chip row occupies the transparent
/// titlebar's content area. Other platforms drop decorations entirely and use
/// the integrated custom controls in that same row. Shared rather than
/// duplicated, so a second window cannot come up wearing a native titlebar the
/// first one does not have.
pub(crate) fn window_viewport_builder(
    title: &str,
    size: eframe::egui::Vec2,
) -> eframe::egui::ViewportBuilder {
    eframe::egui::ViewportBuilder::default()
        .with_decorations(cfg!(target_os = "macos"))
        .with_fullsize_content_view(cfg!(target_os = "macos"))
        .with_title_shown(!cfg!(target_os = "macos"))
        .with_titlebar_shown(!cfg!(target_os = "macos"))
        .with_title(title)
        .with_icon(application_icon_data())
        .with_inner_size(size)
        .with_min_inner_size([360.0, 240.0])
}

/// Builds the first window's viewport, at its restored geometry when a saved
/// workspace has one and at `default_size` otherwise. Smoke runs never inherit
/// a saved position or size.
fn primary_viewport_builder(
    restored: Option<festerm_config::WorkspaceWindowGeometry>,
    default_size: eframe::egui::Vec2,
    native_smoke: bool,
) -> eframe::egui::ViewportBuilder {
    let restored = restored.filter(|_| !native_smoke);
    let size = restored.map_or(default_size, |geometry| {
        let (width, height) = geometry.size();
        eframe::egui::vec2(width, height)
    });
    let mut builder = window_viewport_builder(APPLICATION_TITLE, size);
    if let Some(geometry) = restored {
        let (x, y) = geometry.position();
        builder = builder.with_position(eframe::egui::pos2(x, y));
    }
    builder
}

fn main() -> eframe::Result<()> {
    let diagnostics = diagnostics::init();
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
    let default_width = DEFAULT_WINDOW_WIDTH;
    let default_height = DEFAULT_WINDOW_HEIGHT;

    // A restored workspace reopens the first window where and how the user
    // left it; without one (or with workspace restore off) fesTerm opens at
    // the default size the platform places wherever it likes.
    let restored_geometry = startup_configuration.restored_window_geometry();
    let viewport = primary_viewport_builder(
        restored_geometry,
        eframe::egui::vec2(default_width, default_height),
        native_smoke::NativeWindowSmoke::requested(),
    );
    let options = eframe::NativeOptions {
        viewport,
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: windows_drag_friendly_surface_config(),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = eframe::run_native(
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
            let mut application = FesTermApplication::new(app);
            application.restore_windows(&creation_context.egui_ctx);
            Ok(Box::new(application))
        }),
    );
    diagnostics.finish(result.is_ok());
    result
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
            configure_renderer_animations(&creation_context.egui_ctx, info.device_type);
            software_background::install(&creation_context.egui_ctx, render_state);
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

fn configure_renderer_animations(
    context: &eframe::egui::Context,
    device_type: eframe::wgpu::DeviceType,
) {
    if device_type == eframe::wgpu::DeviceType::Cpu {
        context.all_styles_mut(|style| style.animation_time = 0.0);
        tracing::info!(
            target: "festerm::app",
            "using static animation indicators on a software rendering adapter"
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn software_renderers_disable_animation_without_changing_gpu_defaults() {
        use eframe::{egui::Context, wgpu::DeviceType};

        for device_type in [
            DeviceType::Other,
            DeviceType::IntegratedGpu,
            DeviceType::DiscreteGpu,
            DeviceType::VirtualGpu,
            DeviceType::Cpu,
        ] {
            let context = Context::default();
            context.all_styles_mut(|style| style.animation_time = 0.5);
            super::configure_renderer_animations(&context, device_type);
            let expected = if device_type == DeviceType::Cpu {
                0.0
            } else {
                0.5
            };
            for theme in [eframe::egui::Theme::Dark, eframe::egui::Theme::Light] {
                assert_eq!(context.style_of(theme).animation_time, expected);
            }
        }
    }

    /// A restored workspace reopens the first window at the size and position
    /// it was left at, rather than at the default size the platform places
    /// wherever it likes.
    #[test]
    fn a_restored_workspace_reopens_the_first_window_where_it_was_left() {
        let restored = festerm_config::WorkspaceWindowGeometry::new(120.0, 80.0, 1440.0, 900.0);

        let builder = super::primary_viewport_builder(
            Some(restored),
            eframe::egui::vec2(super::DEFAULT_WINDOW_WIDTH, super::DEFAULT_WINDOW_HEIGHT),
            false,
        );

        assert_eq!(builder.inner_size, Some(eframe::egui::vec2(1440.0, 900.0)));
        assert_eq!(builder.position, Some(eframe::egui::pos2(120.0, 80.0)));
    }

    /// Without saved geometry - no workspace, workspace restore off, or a
    /// platform that will not report a window's own position - the first
    /// window opens at fesTerm's default size and is not placed at all.
    #[test]
    fn without_saved_geometry_the_first_window_opens_at_the_default_size() {
        let default_size =
            eframe::egui::vec2(super::DEFAULT_WINDOW_WIDTH, super::DEFAULT_WINDOW_HEIGHT);

        let builder = super::primary_viewport_builder(None, default_size, false);

        assert_eq!(builder.inner_size, Some(default_size));
        assert_eq!(builder.position, None);
    }

    #[test]
    fn native_smoke_ignores_saved_window_geometry() {
        let restored =
            festerm_config::WorkspaceWindowGeometry::new(-20_000.0, -20_000.0, 1440.0, 900.0);
        let default_size =
            eframe::egui::vec2(super::DEFAULT_WINDOW_WIDTH, super::DEFAULT_WINDOW_HEIGHT);

        let builder = super::primary_viewport_builder(Some(restored), default_size, true);

        assert_eq!(builder.inner_size, Some(default_size));
        assert_eq!(builder.position, None);
    }

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
