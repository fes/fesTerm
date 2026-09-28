use super::{profile, *};
use egui::epaint::{ClippedPrimitive, Primitive};
use egui_kittest::{
    wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
    TestRenderer,
};
use festerm_core::{Dimensions, Terminal};
use festerm_ui_egui::{EncodedInputSink, TerminalView};

struct Sink;
impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, _: &[u8]) {}
    fn terminal_resizes_owned_by_backend(&self) -> bool {
        true
    }
}

fn target(
    state: &egui_wgpu::RenderState,
    size: [u32; 2],
    format: wgpu::TextureFormat,
    copy_dst: bool,
) -> wgpu::Texture {
    state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("host-copy regression"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | if copy_dst {
                wgpu::TextureUsages::COPY_DST
            } else {
                wgpu::TextureUsages::empty()
            },
        view_formats: &[],
    })
}

fn render_state() -> egui_wgpu::RenderState {
    let mut setup = default_wgpu_setup();
    let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
        unreachable!()
    };
    options.instance_descriptor.backends = wgpu::Backends::DX12;
    let mut state = create_render_state(setup, Default::default());
    assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
    state.target_format = wgpu::TextureFormat::Bgra8Unorm;
    *state.renderer.write() =
        egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
    state
}

#[test]
fn host_copy_preserves_pixels_dpi_resize_overlays_and_fallback() {
    let state = render_state();
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    crate::software_background::install(&context, &state);
    let status = super::native::install_with_host_copy(&context, &state, true).unwrap();
    let mut terminal = Terminal::new(Dimensions::new(48, 24).unwrap()).unwrap();
    terminal.ingest(
        "\x1b[?25l\x1b[2J\x1b[HHost copy: \u{754c} e\u{301} \u{1f916}\r\n\x1b[31mred\x1b[0m"
            .as_bytes(),
    );
    let mut view = TerminalView::default();
    let mut copied_frames = 0;
    let mut retained = None;
    for scale in [1.0, 1.25, 2.0, 1.0] {
        for frame in 0..8 {
            let size = if frame >= 6 { [520, 400] } else { [480, 440] };
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(size[0] as f32, size[1] as f32),
                )),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            terminal.ingest(format!("\x1b[4;1H\x1b[2KUpdate {frame}").as_bytes());
            let overlay = frame == 4;
            let disabled = frame == 5;
            let mut output = context.run_ui(input, |ui| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(70, 25, 80));
                ui.add_enabled_ui(!disabled, |ui| {
                    if frame == 3 {
                        ui.set_clip_rect(egui::Rect::from_min_max(
                            egui::pos2(11.25, 13.5),
                            egui::pos2(460.5, 409.75),
                        ));
                    }
                    view.show(ui, &mut terminal, &mut Sink);
                });
                if overlay {
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(egui::pos2(15.0, 20.0), egui::vec2(180.0, 25.0)),
                        6.0,
                        egui::Color32::from_rgba_unmultiplied(200, 100, 50, 96),
                    );
                }
            });
            uploader.handle_delta(&mut output.textures_delta);
            let scale = context.pixels_per_point();
            let screen = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [
                    (size[0] as f32 * scale).round() as u32,
                    (size[1] as f32 * scale).round() as u32,
                ],
                pixels_per_point: scale,
            };
            let primitives = context.tessellate(output.shapes, scale);
            let texture = target(&state, screen.size_in_pixels, state.target_format, true);
            state.renderer.write().final_callback_copy_enabled = false;
            assert!(!profile::draw(&state, &texture, &screen, &primitives));
            let reference = profile::read_image(&state, &texture);
            state.renderer.write().final_callback_copy_enabled = true;
            let copied = profile::draw(&state, &texture, &screen, &primitives);
            copied_frames += usize::from(copied);
            let actual = profile::read_image(&state, &texture);
            profile::assert_same_pixels(
                &reference,
                &actual,
                &format!("scale={scale}, frame={frame}, copied={copied}"),
            );
            if overlay || disabled {
                assert!(!copied, "overlay/opacity must retain ordinary paint");
            } else if frame >= 2 {
                assert!(copied, "eligible native frame must exercise the copy");
                verify_rejections(&state, &texture, &screen, &primitives);
                if retained.is_none() {
                    verify_renderer_options(&state, &texture, &screen, &primitives);
                    let mut capture =
                        egui_wgpu::capture::CaptureState::new(&state.device, &texture);
                    assert!(profile::draw(
                        &state,
                        &capture.texture,
                        &screen,
                        &primitives
                    ));
                    profile::assert_same_pixels(
                        &reference,
                        &profile::read_image(&state, &capture.texture),
                        "capture target must use the same host copy",
                    );
                    capture.update(
                        &state.device,
                        &target(&state, screen.size_in_pixels, state.target_format, false),
                    );
                    assert!(!profile::draw(
                        &state,
                        &capture.texture,
                        &screen,
                        &primitives
                    ));
                    profile::assert_same_pixels(
                        &reference,
                        &profile::read_image(&state, &capture.texture),
                        "capture target without copy support must preserve shader pixels",
                    );
                    retained = Some((screen, primitives, reference));
                }
            }
            assert!(status.active.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
    assert!(
        copied_frames >= 16,
        "must exercise copies across DPI/resize changes"
    );
    let (screen, primitives, reference) = retained.unwrap();
    let texture = target(&state, screen.size_in_pixels, state.target_format, true);
    assert!(profile::draw(&state, &texture, &screen, &primitives));
    profile::assert_same_pixels(
        &reference,
        &profile::read_image(&state, &texture),
        "published callback must remain immutable after later updates, DPI and resize",
    );
}

#[test]
fn host_copy_capture_tracks_usage_size_and_format() {
    let state = render_state();
    let make_target = |size, format, copy| target(&state, size, format, copy);
    let original = make_target([64, 48], state.target_format, false);
    let mut capture = egui_wgpu::capture::CaptureState::new(&state.device, &original);
    let first = capture.texture.clone();
    assert!(!first.usage().contains(wgpu::TextureUsages::COPY_DST));
    capture.update(&state.device, &original);
    assert_eq!(capture.texture, first, "unchanged captures reuse resources");
    for (size, format, copy) in [
        ([64, 48], state.target_format, true),
        ([64, 48], state.target_format, false),
        ([80, 60], state.target_format, true),
        ([80, 60], wgpu::TextureFormat::Rgba8Unorm, true),
    ] {
        let previous = capture.texture.clone();
        capture.update(&state.device, &make_target(size, format, copy));
        assert_ne!(capture.texture, previous);
        assert_eq!([capture.texture.width(), capture.texture.height()], size);
        assert_eq!(capture.texture.format(), format);
        assert_eq!(
            capture
                .texture
                .usage()
                .contains(wgpu::TextureUsages::COPY_DST),
            copy
        );
        assert!(capture.texture.usage().contains(
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
        ));
    }
}

fn verify_renderer_options(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
) {
    for options in [
        egui_wgpu::RendererOptions {
            msaa_samples: 4,
            ..Default::default()
        },
        egui_wgpu::RendererOptions {
            depth_stencil_format: Some(wgpu::TextureFormat::Depth32Float),
            ..Default::default()
        },
    ] {
        let mut renderer = egui_wgpu::Renderer::new(&state.device, state.target_format, options);
        assert!(!renderer.final_callback_copy_enabled);
        renderer.final_callback_copy_enabled = true;
        assert!(renderer
            .final_callback_copy(primitives, screen, texture)
            .is_none());
    }
}

fn verify_rejections(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
) {
    let renderer = state.renderer.read();
    let rejected = |paint: &[ClippedPrimitive], target: &wgpu::Texture| {
        assert!(renderer
            .final_callback_copy(paint, screen, target)
            .is_none());
    };
    let mut clipped = primitives.to_vec();
    let last = clipped.last_mut().unwrap();
    let Primitive::Callback(callback) = &last.primitive else {
        panic!("expected native callback");
    };
    last.clip_rect = callback.rect.shrink(1.0);
    rejected(&clipped, texture);
    rejected(
        primitives,
        &target(
            state,
            screen.size_in_pixels,
            wgpu::TextureFormat::Rgba8Unorm,
            true,
        ),
    );
    rejected(
        primitives,
        &target(state, screen.size_in_pixels, state.target_format, false),
    );
    rejected(
        primitives,
        &target(state, [32, 32], state.target_format, true),
    );
}
