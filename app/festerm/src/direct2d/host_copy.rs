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

#[test]
fn retained_prefix_matches_host_copy_across_panels_dpi_and_terminal_movement() {
    let state = render_state();
    assert!(!state.renderer.read().retained_composition_enabled);
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    crate::software_background::install(&context, &state);
    let status = super::native::install_with_host_copy(&context, &state, true).unwrap();
    state.renderer.write().retained_composition_enabled = true;
    let mut cache = egui_wgpu::RetainedUi::default();
    let mut terminal = Terminal::new(Dimensions::new(48, 24).unwrap()).unwrap();
    terminal.ingest(
        "\x1b[?25l\x1b[2J\x1b[HRetained: 0123456789 \u{754c} e\u{301}\r\n\x1b[31mred\x1b[0m"
            .as_bytes(),
    );
    let mut view = TerminalView::default();
    let mut reused_new_frames = 0;
    for scale in [1.0, 1.25, 2.0] {
        for frame in 0..12 {
            let size = if frame == 11 { [520, 400] } else { [480, 440] };
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
            let overlay = frame == 9;
            let disabled = frame == 10;
            let clear = if frame == 8 {
                [0.1, 0.2, 0.3, 1.0]
            } else {
                [0.0; 4]
            };
            let mut output = context.run_ui(input, |ui| {
                let fill = if frame == 5 {
                    egui::Color32::from_rgb(80, 110, 50)
                } else {
                    egui::Color32::from_rgb(70, 25, 80)
                };
                crate::software_background::show_frame(ui, egui::Frame::new().fill(fill), |ui| {
                    ui.label("Retained header 0123456789");
                });
                if frame == 7 {
                    ui.add_space(12.5);
                }
                ui.add_enabled_ui(!disabled, |ui| {
                    if frame == 6 {
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
            let (copied, _) =
                profile::draw_composed(&state, &texture, &screen, &primitives, clear, None);
            let reference = profile::read_image(&state, &texture);
            let candidate = profile::draw_composed(
                &state,
                &texture,
                &screen,
                &primitives,
                clear,
                Some(&mut cache),
            );
            assert_eq!(candidate.0, copied);
            profile::assert_same_pixels(
                &reference,
                &profile::read_image(&state, &texture),
                &format!("retained scale={scale}, frame={frame}, result={candidate:?}"),
            );
            reused_new_frames += usize::from(candidate.1 == Some(true));
            if overlay || disabled {
                assert_eq!(candidate, (false, None));
                assert_eq!(cache.stats().texture_bytes, 0);
            } else if frame >= 2 {
                assert!(
                    copied,
                    "eligible native frame must use its exact image copy"
                );
                assert!(
                    candidate.1.is_some(),
                    "panel keys must enable retention at scale={scale}, frame={frame}: {:?}",
                    cache.stats(),
                );
                let repeated = profile::draw_composed(
                    &state,
                    &texture,
                    &screen,
                    &primitives,
                    clear,
                    Some(&mut cache),
                );
                assert_eq!(repeated, (true, Some(true)));
                profile::assert_same_pixels(
                    &reference,
                    &profile::read_image(&state, &texture),
                    "a retained prefix must preserve every reference pixel",
                );
                let mut capture = egui_wgpu::capture::CaptureState::new(&state.device, &texture);
                assert_eq!(
                    profile::draw_composed(
                        &state,
                        &capture.texture,
                        &screen,
                        &primitives,
                        clear,
                        Some(&mut cache),
                    ),
                    (true, Some(true)),
                );
                profile::assert_same_pixels(
                    &reference,
                    &profile::read_image(&state, &capture.texture),
                    "capture and native-compatible targets must share retained pixels",
                );
                capture.update(
                    &state.device,
                    &target(&state, screen.size_in_pixels, state.target_format, false),
                );
                assert_eq!(
                    profile::draw_composed(
                        &state,
                        &capture.texture,
                        &screen,
                        &primitives,
                        clear,
                        Some(&mut egui_wgpu::RetainedUi::default()),
                    ),
                    (false, None),
                );
                profile::assert_same_pixels(
                    &reference,
                    &profile::read_image(&state, &capture.texture),
                    "a capture without copy support must retain ordinary pixels",
                );
            }
            assert!(cache.stats().texture_bytes <= 64 * 1024 * 1024);
            assert!(cache.stats().signature_bytes <= 1024 * 1024);
            assert!(status.active.load(std::sync::atomic::Ordering::Relaxed));
        }
    }
    assert!(
        reused_new_frames >= 6,
        "must reuse prefixes from separately built UI frames, not only frozen primitives",
    );
}

fn retained_fixture() -> (
    egui_wgpu::RenderState,
    egui_wgpu::ScreenDescriptor,
    Vec<ClippedPrimitive>,
) {
    let state = render_state();
    assert!(state
        .renderer
        .read()
        .texture(&egui::TextureId::Managed(0))
        .is_none());
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    crate::software_background::install(&context, &state);
    let status = super::native::install_with_host_copy(&context, &state, true).unwrap();
    state.renderer.write().retained_composition_enabled = true;
    let mut terminal = Terminal::new(Dimensions::new(48, 24).unwrap()).unwrap();
    terminal.ingest(b"\x1b[?25l\x1b[2J\x1b[HImmutable terminal image 0123456789");
    let mut view = TerminalView::default();
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [480, 440],
        pixels_per_point: 1.0,
    };
    let texture = target(&state, screen.size_in_pixels, state.target_format, true);
    let mut primitives = Vec::new();
    for _ in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 440.0),
            )),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            crate::software_background::show_frame(
                ui,
                egui::Frame::new().fill(egui::Color32::from_rgb(40, 50, 60)),
                |ui| {
                    ui.label("Retained header");
                },
            );
            view.show(ui, &mut terminal, &mut Sink);
        });
        uploader.handle_delta(&mut output.textures_delta);
        primitives = context.tessellate(output.shapes, 1.0);
        profile::draw(&state, &texture, &screen, &primitives);
    }
    assert!(status.active.load(std::sync::atomic::Ordering::Relaxed));
    assert!(profile::draw(&state, &texture, &screen, &primitives));
    (state, screen, primitives)
}

fn assert_retained_frame(
    state: &egui_wgpu::RenderState,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
    cache: &mut egui_wgpu::RetainedUi,
    expected: Option<bool>,
) -> image::RgbaImage {
    let texture = target(state, screen.size_in_pixels, state.target_format, true);
    let copied = profile::draw(state, &texture, screen, primitives);
    let reference = profile::read_image(state, &texture);
    let actual = profile::draw_composed(state, &texture, screen, primitives, [0.0; 4], Some(cache));
    assert_eq!(actual, (copied, expected), "{:?}", cache.stats());
    profile::assert_same_pixels(
        &reference,
        &profile::read_image(state, &texture),
        "retention must preserve current ordinary-composition pixels",
    );
    reference
}

#[test]
fn retained_prefix_keeps_queued_images_immutable_after_rebuild_and_clear() {
    let (state, screen, primitives) = retained_fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    let initial = assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    let delayed_target = target(&state, screen.size_in_pixels, state.target_format, true);
    let pending = {
        let mut renderer = state.renderer.write();
        let mut encoder = state.device.create_command_encoder(&Default::default());
        let extra = renderer.update_buffers(
            &state.device,
            &state.queue,
            &mut encoder,
            &primitives,
            &screen,
        );
        assert_eq!(
            cache.try_render(
                &renderer,
                &state.device,
                &mut encoder,
                &primitives,
                &screen,
                &delayed_target,
                [0.0; 4],
            ),
            Some(true),
        );
        renderer
            .final_callback_copy(&primitives, &screen, &delayed_target)
            .unwrap()
            .encode(&mut encoder, &delayed_target);
        extra
            .into_iter()
            .chain([encoder.finish()])
            .collect::<Vec<_>>()
    };

    let mut changed = primitives.clone();
    let mesh = changed
        .iter_mut()
        .find_map(|job| match &mut job.primitive {
            Primitive::Mesh(mesh) if !mesh.indices.is_empty() => Some(mesh),
            _ => None,
        })
        .expect("fixture must contain visible chrome geometry");
    for vertex in &mut mesh.vertices {
        vertex.color = egui::Color32::RED;
    }
    let newer = assert_retained_frame(&state, &screen, &changed, &mut cache, Some(false));
    assert_ne!(initial, newer, "the later prefix must visibly change");
    cache.clear();
    assert_eq!(cache.stats().texture_bytes, 0);
    assert_eq!(cache.stats().signature_bytes, 0);
    assert_eq!(cache.stats().rebuilt_frames, 2);
    assert_eq!(cache.stats().reused_frames, 1);
    drop(cache);

    // Submit the older copy only after replacing and releasing the cache-owned image.
    state.queue.submit(pending);
    profile::assert_same_pixels(
        &initial,
        &profile::read_image(&state, &delayed_target),
        "queued copies must retain their original immutable prefix after cache destruction",
    );
}

#[test]
fn retained_prefix_declines_signature_overflow_and_recovers() {
    let (state, screen, primitives) = retained_fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    let initial = assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    let mut oversized = primitives.clone();
    let mesh = oversized
        .iter_mut()
        .find_map(|job| match &mut job.primitive {
            Primitive::Mesh(mesh) if !mesh.vertices.is_empty() => Some(mesh),
            _ => None,
        })
        .expect("fixture must contain a chrome mesh");
    mesh.vertices.resize(
        1024 * 1024 / std::mem::size_of::<egui::epaint::Vertex>() + 1,
        mesh.vertices[0],
    );
    let fallback = assert_retained_frame(&state, &screen, &oversized, &mut cache, None);
    profile::assert_same_pixels(
        &initial,
        &fallback,
        "unused vertices must not change ordinary fallback pixels",
    );
    assert_eq!(cache.stats().texture_bytes, 0);
    assert_eq!(cache.stats().signature_bytes, 0);
    assert_eq!(
        cache.stats().decline_reason,
        Some("paint signature exceeds retention budget"),
    );
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));
}

#[test]
fn retained_prefix_invalidates_texture_updates_removal_renderer_and_external_access() {
    let (state, screen, primitives) = retained_fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    let initial = assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));
    let id = egui::TextureId::Managed(0);
    let red = egui::epaint::ImageDelta::full(
        egui::ColorImage::filled([2, 2], egui::Color32::RED),
        egui::TextureOptions::LINEAR,
    );
    state
        .renderer
        .write()
        .update_texture(&state.device, &state.queue, id, &red);
    let changed = assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_ne!(
        initial, changed,
        "the texture mutation must affect visible pixels"
    );
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));
    let green = egui::epaint::ImageDelta::partial(
        [0, 0],
        egui::ColorImage::filled([1, 1], egui::Color32::GREEN),
        egui::TextureOptions::NEAREST,
    );
    state
        .renderer
        .write()
        .update_texture(&state.device, &state.queue, id, &green);
    let partial = assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_ne!(
        changed, partial,
        "partial pixels/sampler changes must be visible"
    );
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));

    state.renderer.write().free_texture(&id);
    let texture = target(&state, screen.size_in_pixels, state.target_format, true);
    let mut encoder = state.device.create_command_encoder(&Default::default());
    assert_eq!(
        cache.try_render(
            &state.renderer.read(),
            &state.device,
            &mut encoder,
            &primitives,
            &screen,
            &texture,
            [0.0; 4],
        ),
        None,
    );
    assert_eq!(cache.stats().texture_bytes, 0);
    state
        .renderer
        .write()
        .update_texture(&state.device, &state.queue, id, &red);
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));

    let mut replacement =
        egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
    replacement.final_callback_copy_enabled = true;
    replacement.retained_composition_enabled = true;
    replacement.update_texture(&state.device, &state.queue, id, &red);
    *state.renderer.write() = replacement;
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));

    let external = state
        .renderer
        .read()
        .texture(&id)
        .unwrap()
        .texture
        .clone()
        .unwrap();
    state.queue.write_texture(
        external.as_image_copy(),
        &[0, 0, 255, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    assert_retained_frame(&state, &screen, &primitives, &mut cache, None);
    assert_eq!(cache.stats().texture_bytes, 0);
    assert_eq!(
        cache.stats().decline_reason,
        Some("managed texture ownership is not exclusive"),
    );
}

#[test]
fn retained_prefix_declines_unkeyed_callbacks_user_textures_and_external_bindings() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    struct ObservedPaint {
        key: Option<egui_wgpu::CallbackPaintKey>,
        prepares: Arc<AtomicUsize>,
        finishes: Arc<AtomicUsize>,
        paints: Arc<AtomicUsize>,
    }
    impl egui_wgpu::CallbackTrait for ObservedPaint {
        fn paint_key(&self) -> Option<egui_wgpu::CallbackPaintKey> {
            self.key.clone()
        }

        fn prepare(
            &self,
            _: &wgpu::Device,
            _: &wgpu::Queue,
            _: &egui_wgpu::ScreenDescriptor,
            _: &mut wgpu::CommandEncoder,
            _: &mut egui_wgpu::CallbackResources,
        ) -> Vec<wgpu::CommandBuffer> {
            self.prepares.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }

        fn finish_prepare(
            &self,
            _: &wgpu::Device,
            _: &wgpu::Queue,
            _: &mut wgpu::CommandEncoder,
            _: &mut egui_wgpu::CallbackResources,
        ) -> Vec<wgpu::CommandBuffer> {
            self.finishes.fetch_add(1, Ordering::Relaxed);
            Vec::new()
        }

        fn paint(
            &self,
            _: egui::PaintCallbackInfo,
            _: &mut wgpu::RenderPass<'static>,
            _: &egui_wgpu::CallbackResources,
        ) {
            self.paints.fetch_add(1, Ordering::Relaxed);
        }
    }

    let (state, screen, primitives) = retained_fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    state.renderer.write().retained_composition_enabled = false;
    assert_retained_frame(&state, &screen, &primitives, &mut cache, None);
    assert_eq!(cache.stats().texture_bytes, 0);
    state.renderer.write().retained_composition_enabled = true;
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));

    let prepares = Arc::new(AtomicUsize::new(0));
    let finishes = Arc::new(AtomicUsize::new(0));
    let paints = Arc::new(AtomicUsize::new(0));
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(480.0, 440.0));
    let observed = |key| ClippedPrimitive {
        clip_rect: rect,
        primitive: Primitive::Callback(egui_wgpu::Callback::new_paint_callback(
            rect,
            ObservedPaint {
                key,
                prepares: prepares.clone(),
                finishes: finishes.clone(),
                paints: paints.clone(),
            },
        )),
    };
    let mut keyed = primitives.clone();
    keyed.insert(
        0,
        observed(Some(egui_wgpu::CallbackPaintKey {
            namespace: Arc::new(()),
            bytes: Arc::from([]),
        })),
    );
    for expected in [Some(false), Some(true), Some(true)] {
        assert_retained_frame(&state, &screen, &keyed, &mut cache, expected);
    }
    assert_eq!(prepares.load(Ordering::Relaxed), 6);
    assert_eq!(finishes.load(Ordering::Relaxed), 6);
    assert_eq!(paints.load(Ordering::Relaxed), 4);
    for count in [&prepares, &finishes, &paints] {
        count.store(0, Ordering::Relaxed);
    }

    let mut unkeyed = primitives.clone();
    unkeyed.insert(0, observed(None));
    for _ in 0..2 {
        assert_retained_frame(&state, &screen, &unkeyed, &mut cache, None);
    }
    assert_eq!(prepares.load(Ordering::Relaxed), 4);
    assert_eq!(finishes.load(Ordering::Relaxed), 4);
    assert_eq!(paints.load(Ordering::Relaxed), 4);
    assert_eq!(cache.stats().texture_bytes, 0);

    let external = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mutable external texture rejection"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    state.queue.write_texture(
        external.as_image_copy(),
        &[0, 255, 0, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        external.size(),
    );
    let external_view = external.create_view(&Default::default());
    let user_id = state.renderer.write().register_native_texture(
        &state.device,
        &external_view,
        wgpu::FilterMode::Nearest,
    );
    let mut user_texture = primitives.clone();
    let mesh = user_texture
        .iter_mut()
        .find_map(|job| match &mut job.primitive {
            Primitive::Mesh(mesh) if !mesh.indices.is_empty() => Some(mesh),
            _ => None,
        })
        .expect("fixture must contain a textured chrome mesh");
    mesh.texture_id = user_id;
    assert_retained_frame(&state, &screen, &user_texture, &mut cache, None);
    assert_eq!(cache.stats().texture_bytes, 0);

    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(false));
    assert_retained_frame(&state, &screen, &primitives, &mut cache, Some(true));
    state
        .renderer
        .write()
        .update_egui_texture_from_wgpu_texture(
            &state.device,
            &external_view,
            wgpu::FilterMode::Nearest,
            egui::TextureId::Managed(0),
        );
    assert_retained_frame(&state, &screen, &primitives, &mut cache, None);
    assert_eq!(cache.stats().texture_bytes, 0);
    assert_eq!(
        cache.stats().decline_reason,
        Some("managed texture ownership is not exclusive"),
    );
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
