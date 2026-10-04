use super::*;
use egui::epaint::{ClippedPrimitive, Primitive};
use egui_kittest::{
    wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
    TestRenderer,
};
use festerm_core::{Dimensions, Terminal};
use festerm_test_support::tui_workload::Workload;
use festerm_ui_egui::{EncodedInputSink, TerminalView};
use festerm_windows_direct2d::{process_cpu_time, Surface};
use std::{
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

pub(super) struct Sink;
impl EncodedInputSink for Sink {
    fn record_encoded_input(&mut self, _: &[u8]) {}
    fn terminal_resizes_owned_by_backend(&self) -> bool {
        true
    }
}

fn complete(state: &egui_wgpu::RenderState) {
    state
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("complete renderer work");
}

pub(super) fn draw(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
) -> bool {
    draw_composed(state, texture, screen, primitives, [0.0; 4], None).0
}

pub(super) fn draw_composed(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
    clear: [f32; 4],
    retained: Option<&mut egui_wgpu::RetainedUi>,
) -> (bool, Option<bool>) {
    let mut renderer = state.renderer.write();
    let mut encoder = state.device.create_command_encoder(&Default::default());
    let extra = renderer.update_buffers(
        &state.device,
        &state.queue,
        &mut encoder,
        primitives,
        screen,
    );
    let copy = renderer.final_callback_copy(primitives, screen, texture);
    let paint = if copy.is_some() {
        &primitives[..primitives.len() - 1]
    } else {
        primitives
    };
    let retained = retained.and_then(|retained| {
        if copy.is_some() {
            retained.try_render(
                &renderer,
                &state.device,
                &mut encoder,
                primitives,
                screen,
                texture,
                clear,
            )
        } else {
            retained.clear();
            None
        }
    });
    let target = texture.create_view(&Default::default());
    if retained.is_none() {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("residual CPU probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(clear[0]),
                            g: f64::from(clear[1]),
                            b: f64::from(clear[2]),
                            a: f64::from(clear[3]),
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, paint, screen);
    }
    if let Some(copy) = copy {
        copy.encode(&mut encoder, texture);
    }
    let copied = copy.is_some();
    state
        .queue
        .submit(extra.into_iter().chain([encoder.finish()]));
    drop(renderer);
    complete(state);
    (copied, retained)
}

struct CompositePaint {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

impl egui_wgpu::CallbackTrait for CompositePaint {
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _: &egui_wgpu::CallbackResources,
    ) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[derive(Clone, Copy)]
enum CompositeProbe {
    Sampled,
    InterpolatedLoad,
}

fn composite_probe(
    state: &egui_wgpu::RenderState,
    surface: &Surface,
    probe: CompositeProbe,
) -> egui::PaintCallback {
    let mut entries = vec![wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }];
    if matches!(probe, CompositeProbe::Sampled) {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 1,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
            count: None,
        });
    }
    let layout = state
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite probe"),
            entries: &entries,
        });
    let pipeline_layout = state
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite probe"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let module = state
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite probe"),
            source: wgpu::ShaderSource::Wgsl(
                match probe {
                    CompositeProbe::Sampled => {
                        r"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var nearest: sampler;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    let points = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var output: VertexOutput;
    output.position = vec4(points[index], 0.0, 1.0);
    output.uv = points[index] * vec2(0.5, -0.5) + vec2(0.5);
    return output;
}
@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureSampleLevel(image, nearest, input.uv, 0.0);
}
"
                    }
                    CompositeProbe::InterpolatedLoad => {
                        r"
@group(0) @binding(0) var image: texture_2d<f32>;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) texel: vec2<f32>,
}
@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    let points = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var output: VertexOutput;
    output.position = vec4(points[index], 0.0, 1.0);
    output.texel = (points[index] * vec2(0.5, -0.5) + vec2(0.5))
        * vec2<f32>(textureDimensions(image));
    return output;
}
@fragment
fn fragment(input: VertexOutput) -> @location(0) vec4<f32> {
    return textureLoad(image, vec2<i32>(input.texel), 0);
}
"
                    }
                }
                .into(),
            ),
        });
    let pipeline = state
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite probe"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: state.target_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
    let view = surface.texture.create_view(&Default::default());
    let sampler = state.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("nearest composite probe"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let mut entries = vec![wgpu::BindGroupEntry {
        binding: 0,
        resource: wgpu::BindingResource::TextureView(&view),
    }];
    if matches!(probe, CompositeProbe::Sampled) {
        entries.push(wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::Sampler(&sampler),
        });
    }
    let bindings = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composite probe"),
        layout: &layout,
        entries: &entries,
    });
    egui_wgpu::Callback::new_paint_callback(
        surface.rect,
        CompositePaint {
            pipeline,
            bindings,
            _texture: surface.texture.clone(),
        },
    )
}

pub(super) fn read_image(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
) -> image::RgbaImage {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("untimed profile screenshot"),
        size: u64::from(stride) * u64::from(texture.height()),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = state.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    state.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap();
        });
    complete(state);
    receiver
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    let mut bytes: Vec<_> = mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect();
    match texture.format() {
        wgpu::TextureFormat::Bgra8Unorm => {
            for pixel in bytes.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
        }
        wgpu::TextureFormat::Rgba8Unorm => {}
        format => panic!("unsupported screenshot format: {format:?}"),
    }
    drop(mapped);
    buffer.unmap();
    image::RgbaImage::from_raw(texture.width(), texture.height(), bytes).unwrap()
}

pub(super) fn assert_same_pixels(
    reference: &image::RgbaImage,
    actual: &image::RgbaImage,
    label: &str,
) {
    assert_eq!(reference.dimensions(), actual.dimensions(), "{label}");
    assert_eq!(
        reference
            .pixels()
            .zip(actual.pixels())
            .filter(|(a, b)| a != b)
            .count(),
        0,
        "{label}"
    );
}

fn draw_with_copy(
    state: &egui_wgpu::RenderState,
    texture: &wgpu::Texture,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
    surface: &Surface,
) {
    let (native, preceding) = primitives.split_last().expect("native image callback");
    assert!(
        matches!(&native.primitive, Primitive::Callback(callback) if callback.rect == surface.rect),
        "copy probe supports only a final native callback"
    );
    assert!(native.clip_rect.contains_rect(surface.rect));
    assert_eq!(surface.texture.format(), texture.format());
    assert!(surface.origin[0] + surface.texture.width() <= texture.width());
    assert!(surface.origin[1] + surface.texture.height() <= texture.height());
    // This test owns the target. An ordinary egui callback cannot do this copy.
    draw(state, texture, screen, preceding);
    let mut encoder = state.device.create_command_encoder(&Default::default());
    let mut destination = texture.as_image_copy();
    destination.origin = wgpu::Origin3d {
        x: surface.origin[0],
        y: surface.origin[1],
        z: 0,
    };
    encoder.copy_texture_to_texture(
        surface.texture.as_image_copy(),
        destination,
        surface.texture.size(),
    );
    state.queue.submit([encoder.finish()]);
    complete(state);
}

fn select_cases(
    cases: &mut Vec<(String, Vec<ClippedPrimitive>)>,
    requested: &str,
) -> Result<(), &'static str> {
    let requested: Vec<_> = requested.split(',').map(str::trim).collect();
    if !requested
        .iter()
        .all(|name| cases.iter().any(|case| case.0 == *name))
    {
        return Err("unknown or empty profile case");
    }
    if requested.contains(&"retained-validation-only") && !requested.contains(&"localized-ui-only")
    {
        return Err("retained validation requires the UI-only snapshot case");
    }
    cases.retain(|case| requested.contains(&case.0.as_str()));
    Ok(())
}

#[test]
fn profile_copy_modes_are_test_only_and_preserve_diagnostic_exclusivity() {
    use std::ffi::OsStr;
    for (direct, host, retained, expected) in [
        (false, None, None, (true, true)),
        (false, Some("0"), None, (false, false)),
        (false, Some("1"), Some("0"), (true, false)),
        (true, None, None, (false, false)),
    ] {
        assert_eq!(
            profile_copy_modes(direct, host.map(OsStr::new), retained.map(OsStr::new)),
            Ok(expected)
        );
    }
    for (direct, host, retained) in [
        (false, Some("0"), Some("1")),
        (true, Some("1"), None),
        (false, Some("invalid"), None),
        (false, None, Some("invalid")),
    ] {
        assert!(
            profile_copy_modes(direct, host.map(OsStr::new), retained.map(OsStr::new)).is_err()
        );
    }
}

fn profile_copy_modes(
    direct_copy: bool,
    host: Option<&std::ffi::OsStr>,
    retained: Option<&std::ffi::OsStr>,
) -> Result<(bool, bool), &'static str> {
    let host = match host {
        None => !direct_copy,
        Some(value) if value == "0" => false,
        Some(value) if value == "1" => true,
        _ => return Err("FESTERM_TUI_PROFILE_HOST_COPY expects 0, 1 or unset"),
    };
    let retained = match retained {
        None => host,
        Some(value) if value == "0" => false,
        Some(value) if value == "1" => true,
        _ => return Err("FESTERM_TUI_PROFILE_RETAINED_COMPOSITION expects 0, 1 or unset"),
    };
    if retained && !host {
        return Err("retained composition requires the host-copy probe");
    }
    if direct_copy && (host || retained) {
        return Err("legacy direct-copy and host-copy probes are exclusive");
    }
    Ok((host, retained))
}

#[test]
fn profile_case_filter_preserves_order_and_rejects_incomplete_inputs() {
    let all = vec![
        ("localized-all".to_owned(), Vec::new()),
        ("localized-ui-only".to_owned(), Vec::new()),
        ("retained-validation-only".to_owned(), Vec::new()),
    ];
    for requested in [
        "",
        " ",
        "unknown",
        "localized-all,",
        "retained-validation-only",
    ] {
        let mut cases = all.clone();
        assert!(select_cases(&mut cases, requested).is_err(), "{requested}");
        assert_eq!(
            cases.len(),
            all.len(),
            "invalid selection must not mutate cases"
        );
    }
    let mut cases = all;
    select_cases(&mut cases, "retained-validation-only, localized-ui-only").unwrap();
    assert_eq!(
        cases.iter().map(|case| case.0.as_str()).collect::<Vec<_>>(),
        ["localized-ui-only", "retained-validation-only"]
    );
}

fn application_profile_scene(scene: &str) -> Result<bool, &'static str> {
    match scene {
        "terminal" => Ok(false),
        "application" | "application-palette" => Ok(true),
        _ => Err("FESTERM_TUI_PROFILE_SCENE expects terminal, application, or application-palette"),
    }
}

#[test]
fn residual_profile_scene_preserves_controls_and_accepts_production_palette() {
    assert_eq!(application_profile_scene("terminal"), Ok(false));
    assert_eq!(application_profile_scene("application"), Ok(true));
    assert_eq!(application_profile_scene("application-palette"), Ok(true));
    for invalid in ["", "palette", "Application", "application-palette,terminal"] {
        assert!(application_profile_scene(invalid).is_err(), "{invalid}");
    }
}

#[test]
#[ignore = "optional paced process-CPU decomposition; not native presentation latency"]
fn profile_terminal_residual_cpu() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let directory = PathBuf::from(
        std::env::var_os("FESTERM_TUI_PROFILE_OUT").expect("set FESTERM_TUI_PROFILE_OUT"),
    );
    assert!(!directory.exists(), "use a new profiling output directory");
    std::fs::create_dir_all(&directory).unwrap();
    let mut setup = default_wgpu_setup();
    let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
        unreachable!()
    };
    options.instance_descriptor.backends = wgpu::Backends::DX12;
    let mut state = create_render_state(setup, Default::default());
    let direct_copy = match std::env::var_os("FESTERM_TUI_PROFILE_COPY") {
        None => false,
        Some(value) if value == "1" => true,
        _ => panic!("FESTERM_TUI_PROFILE_COPY expects 1 or unset"),
    };
    let (host_copy, retained_composition) = profile_copy_modes(
        direct_copy,
        std::env::var_os("FESTERM_TUI_PROFILE_HOST_COPY").as_deref(),
        std::env::var_os("FESTERM_TUI_PROFILE_RETAINED_COMPOSITION").as_deref(),
    )
    .expect("valid test-only copy modes");
    assert!(
        !retained_composition || host_copy,
        "retained composition requires the host-copy probe"
    );
    assert!(!(direct_copy && host_copy), "select only one copy probe");
    if direct_copy || host_copy {
        state.target_format = wgpu::TextureFormat::Bgra8Unorm;
        *state.renderer.write() =
            egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
    }
    let info = state.adapter.get_info();
    assert_eq!(info.device_type, wgpu::DeviceType::Cpu);
    let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    context.set_visuals(festerm_ui_egui::theme::default_visuals());
    crate::software_background::install(&context, &state);
    let palette_frame_textureless = match std::env::var_os("FESTERM_TUI_PROFILE_PALETTE_FRAME") {
        None => true,
        Some(value) if value == "0" => false,
        Some(value) if value == "1" => true,
        _ => panic!("FESTERM_TUI_PROFILE_PALETTE_FRAME expects 0, 1, or unset"),
    };
    crate::software_background::set_palette_frame_enabled(&context, palette_frame_textureless);
    let status = super::native::install_with_host_copy(&context, &state, host_copy).unwrap();
    state.renderer.write().retained_composition_enabled = retained_composition;
    let mut retained_ui = egui_wgpu::RetainedUi::default();
    let scene = std::env::var_os("FESTERM_TUI_PROFILE_SCENE")
        .map(|value| value.into_string().expect("profile scene must be UTF-8"))
        .unwrap_or_else(|| "terminal".into());
    let application_scene = application_profile_scene(&scene).expect("valid profile scene");
    let mut application = application_scene.then(|| {
        let (mut app, _, transport) = crate::app::FesTermApp::for_test_with_fake_ssh_session([]);
        if scene == "application-palette" {
            app.open_palette_for_gallery(&context);
        }
        (app, transport)
    });
    let dimensions = Dimensions::new(120, 40).unwrap();
    let mut terminal = Terminal::new(dimensions).unwrap();
    let mut view = TerminalView::default();
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1029.0, 829.0),
        )),
        ..Default::default()
    };
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .native_pixels_per_point = Some(2.0);
    let mut step = |bytes: &[u8]| {
        if let Some((app, transport)) = &mut application {
            if !bytes.is_empty() {
                transport.push_event(festerm_session::SessionEvent::Output(bytes.to_vec()));
            }
            let output = context.run_ui(input.clone(), |ui| {
                app.frame_logic(ui.ctx());
                app.ui_content(ui);
            });
            (output, app.active_terminal_dimensions_for_test())
        } else {
            terminal.ingest(bytes);
            let output = context.run_ui(input.clone(), |ui| {
                view.show(ui, &mut terminal, &mut Sink);
            });
            (output, terminal.dimensions())
        }
    };
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [2058, 1658],
        pixels_per_point: 2.0,
    };
    let texture = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("persistent offscreen probe target"),
        size: wgpu::Extent3d {
            width: screen.size_in_pixels[0],
            height: screen.size_in_pixels[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for _ in 0..8 {
        let (mut output, _) = step(&[]);
        renderer.handle_delta(&mut output.textures_delta);
        let paint = context.tessellate(output.shapes, context.pixels_per_point());
        draw(&state, &texture, &screen, &paint);
    }
    let (mut output, actual_dimensions) = step(&Workload::Localized.setup());
    renderer.handle_delta(&mut output.textures_delta);
    assert_eq!(actual_dimensions, dimensions);
    let mut primitives = Vec::new();
    for frame in 0..8 {
        let (mut output, actual_dimensions) = step(&Workload::Localized.update(frame));
        assert_eq!(actual_dimensions, dimensions);
        renderer.handle_delta(&mut output.textures_delta);
        primitives = context.tessellate(output.shapes, context.pixels_per_point());
        draw(&state, &texture, &screen, &primitives);
    }
    assert!(
        status.active.load(Ordering::Relaxed),
        "{:?}",
        status.first_failure.get()
    );
    if scene == "application-palette" {
        assert!(
            context
                .memory(|memory| memory.area_rect(egui::Id::new("festerm_command_palette")))
                .is_some(),
            "profile requires the actual production command palette"
        );
    }
    let surface = status.last_surface.lock().unwrap().clone().unwrap();
    let native_index = primitives
        .iter()
        .position(|primitive| matches!(&primitive.primitive, Primitive::Callback(callback) if callback.rect == surface.rect))
        .expect("locate native image callback");
    let mut sampled = primitives.clone();
    sampled[native_index].primitive =
        Primitive::Callback(composite_probe(&state, &surface, CompositeProbe::Sampled));
    let mut interpolated = primitives.clone();
    interpolated[native_index].primitive = Primitive::Callback(composite_probe(
        &state,
        &surface,
        CompositeProbe::InterpolatedLoad,
    ));
    draw(&state, &texture, &screen, &primitives);
    let original_image = read_image(&state, &texture);
    if retained_composition {
        for expected in [Some(false), Some(true)] {
            assert_eq!(
                draw_composed(
                    &state,
                    &texture,
                    &screen,
                    &primitives,
                    [0.0; 4],
                    Some(&mut retained_ui),
                ),
                (true, expected),
                "{:?}",
                retained_ui.stats(),
            );
            assert_same_pixels(
                &original_image,
                &read_image(&state, &texture),
                "retained prefix changed initial application pixels",
            );
        }
    }
    draw(&state, &texture, &screen, &sampled);
    let sampled_image = read_image(&state, &texture);
    draw(&state, &texture, &screen, &interpolated);
    let interpolated_image = read_image(&state, &texture);
    original_image.save(directory.join("original.png")).unwrap();
    sampled_image.save(directory.join("sampled.png")).unwrap();
    assert_same_pixels(
        &original_image,
        &sampled_image,
        "sampled compositor changed pixels",
    );
    assert_same_pixels(
        &original_image,
        &interpolated_image,
        "compositor coordinates changed pixels",
    );
    if direct_copy {
        draw_with_copy(&state, &texture, &screen, &primitives, &surface);
        let copied_image = read_image(&state, &texture);
        copied_image.save(directory.join("copied.png")).unwrap();
        assert_same_pixels(
            &original_image,
            &copied_image,
            "direct copying changed pixels",
        );
    }
    if let Some(reference) = std::env::var_os("FESTERM_TUI_PROFILE_REFERENCE") {
        let reference = image::open(reference).unwrap().into_rgba8();
        assert_same_pixels(
            &reference,
            &original_image,
            "full application pixels changed",
        );
    }
    let textureless_mesh = std::env::var_os("FESTERM_TUI_PROFILE_TEXTURELESS_MESH").map(|value| {
        value
            .into_string()
            .expect("textureless mesh index must be UTF-8")
            .parse::<usize>()
            .expect("textureless mesh probe expects a nonnegative primitive index")
    });
    if let Some(index) = textureless_mesh {
        let primitive = primitives.get_mut(index).expect("textureless mesh exists");
        let Primitive::Mesh(mesh) = &primitive.primitive else {
            panic!("textureless probe requires an ordinary mesh");
        };
        let probe = crate::software_background::PanelTestProbe::install(&context, &state);
        let shape = probe
            .white_mesh_shape(context.viewport_rect(), mesh)
            .expect("supported exact-geometry textureless mesh");
        let egui::Shape::Callback(callback) = shape else {
            unreachable!()
        };
        primitive.primitive = Primitive::Callback(callback);
        draw(&state, &texture, &screen, &primitives);
        let candidate = read_image(&state, &texture);
        candidate.save(directory.join("textureless.png")).unwrap();
        assert_same_pixels(
            &original_image,
            &candidate,
            "textureless diagnostic changed full-frame pixels",
        );
    }
    let metadata = primitives
        .iter()
        .enumerate()
        .map(|(index, primitive)| match &primitive.primitive {
            Primitive::Callback(callback) => serde_json::json!({
                "index":index, "kind":"callback", "rect":format!("{:?}", callback.rect),
                "clip":format!("{:?}", primitive.clip_rect),
                "callback_viewport_pixels":callback.rect.area() * 4.0,
                "native_image":index == native_index,
            }),
            Primitive::Mesh(mesh) => serde_json::json!({
                "index":index, "kind":"mesh", "vertices":mesh.vertices.len(),
                "indices":mesh.indices.len(), "clip":format!("{:?}", primitive.clip_rect),
                "bounds":format!("{:?}", mesh.calc_bounds()),
                "opaque_white_triangles":mesh.indices.as_chunks::<3>().0.iter().filter(|triangle| {
                    let first = mesh.vertices[triangle[0] as usize];
                    triangle.iter().all(|index| {
                        let vertex = mesh.vertices[*index as usize];
                        vertex.uv == egui::epaint::WHITE_UV
                            && vertex.color == first.color
                            && vertex.color.is_opaque()
                    })
                }).count(),
            }),
        })
        .collect::<Vec<_>>();
    let mut cases = vec![
        ("sleep-only".to_owned(), Vec::new()),
        ("clear-only".to_owned(), Vec::new()),
        ("frozen-all".to_owned(), primitives.clone()),
        ("interpolated-load-frozen".to_owned(), interpolated.clone()),
        (
            "interpolated-load-native".to_owned(),
            vec![interpolated[native_index].clone()],
        ),
    ];
    for (index, primitive) in primitives.iter().enumerate() {
        cases.push((format!("primitive-{index}"), vec![primitive.clone()]));
    }

    let meshes = primitives
        .iter()
        .filter(|primitive| matches!(primitive.primitive, Primitive::Mesh(_)))
        .cloned()
        .collect();
    let mut without_fills = primitives.clone();
    let mut removed_fill_triangles = 0;
    for primitive in &mut without_fills {
        if let Primitive::Mesh(mesh) = &mut primitive.primitive {
            let mut indices = Vec::new();
            for triangle in mesh.indices.as_chunks::<3>().0 {
                let first = mesh.vertices[triangle[0] as usize];
                if triangle.iter().all(|index| {
                    let vertex = mesh.vertices[*index as usize];
                    vertex.uv == egui::epaint::WHITE_UV
                        && vertex.color == first.color
                        && vertex.color.is_opaque()
                }) {
                    removed_fill_triangles += 1;
                } else {
                    indices.extend_from_slice(triangle);
                }
            }
            mesh.indices = indices;
        }
    }
    if std::env::var("FESTERM_TUI_PROFILE_SAMPLER").as_deref() == Ok("1") {
        cases.push(("sampled-frozen-all".to_owned(), sampled));
    }
    if direct_copy {
        cases.extend([
            ("copy-frozen-all".to_owned(), primitives.clone()),
            ("localized-copy-all".to_owned(), Vec::new()),
        ]);
    }
    cases.extend([
        ("meshes-only".to_owned(), meshes),
        ("without-solid-mesh-fills".to_owned(), without_fills),
        ("localized-all".to_owned(), Vec::new()),
        ("localized-without-composition".to_owned(), Vec::new()),
        ("native-copy-only".to_owned(), Vec::new()),
        ("native-allocate-copy".to_owned(), Vec::new()),
        ("native-solid-patch".to_owned(), Vec::new()),
        ("interpolated-load-frozen-repeat".to_owned(), interpolated),
        ("frozen-all-repeat".to_owned(), primitives.clone()),
        ("localized-ui-only".to_owned(), Vec::new()),
        ("retained-validation-only".to_owned(), Vec::new()),
    ]);
    if let Some(requested) = std::env::var_os("FESTERM_TUI_PROFILE_CASES") {
        let requested = requested.into_string().expect("case names must be UTF-8");
        select_cases(&mut cases, &requested).expect("valid profile selection");
    }
    if textureless_mesh.is_some() {
        assert!(
            cases
                .iter()
                .all(|(name, _)| matches!(name.as_str(), "frozen-all" | "frozen-all-repeat")),
            "textureless diagnostic supports only explicit frozen whole-frame controls"
        );
    }
    let copy_descriptor = wgpu::TextureDescriptor {
        label: Some("native image copy probe"),
        size: surface.texture.size(),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: surface.texture.format(),
        usage: wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    };
    let copy_target = state.device.create_texture(&copy_descriptor);
    let captured =
        std::sync::Arc::new(std::sync::OnceLock::<festerm_ui_egui::TerminalPaintFrame>::new());
    let mut validation_renderer =
        festerm_windows_direct2d::CachedRenderer::new(state.device.clone(), state.queue.clone())
            .unwrap();
    let mut solid_renderer =
        festerm_windows_direct2d::Renderer::new(state.device.clone(), state.queue.clone()).unwrap();
    let patch_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(surface.texture.width() as f32, 128.0),
    );
    let mut patch = egui::Mesh::default();
    patch.add_colored_rect(patch_rect, festerm_ui_egui::theme::SURFACE_TERMINAL);
    let patch = [ClippedPrimitive {
        clip_rect: patch_rect,
        primitive: Primitive::Mesh(patch),
    }];
    let patch_textures = [(
        egui::TextureId::Managed(0),
        std::sync::Arc::new(egui::ColorImage::new([1, 1], vec![egui::Color32::WHITE])),
    )];
    let logical_processors = std::thread::available_parallelism().unwrap().get();
    let mut measurements = Vec::new();
    const FRAMES: u32 = 100;
    const INTERVAL: Duration = Duration::from_millis(100);
    let mut update_index = 8;
    for (name, frozen) in cases {
        if name == "localized-ui-only" {
            let Primitive::Callback(callback) = &primitives[native_index].primitive else {
                unreachable!()
            };
            let callback = callback.clone();
            let captured = captured.clone();
            // Diagnostic only: capture the normal UI snapshot but freeze native pixels.
            festerm_ui_egui::install_root_terminal_painter(&context, move |_, frame| {
                let _ = captured.set(frame);
                Some(callback.clone())
            });
        }
        let requires_retention = matches!(
            name.as_str(),
            "frozen-all" | "localized-all" | "frozen-all-repeat"
        );
        let mut last_composited_frame = None;
        let mut frame = |retained_ui: &mut egui_wgpu::RetainedUi| {
            if name == "sleep-only" {
                return;
            }
            if name == "copy-frozen-all" {
                draw_with_copy(&state, &texture, &screen, &frozen, &surface);
            } else if name == "retained-validation-only" {
                let frame = captured
                    .get()
                    .expect("UI-only case must capture a frame first");
                validation_renderer
                    .render(
                        frame.rect,
                        frame.pixels_per_point,
                        festerm_ui_egui::theme::SURFACE_TERMINAL,
                        &frame.primitives,
                        &frame.textures,
                        None,
                    )
                    .unwrap()
                    .expect("retained native frame");
                complete(&state);
            } else if name == "native-solid-patch" {
                solid_renderer
                    .render(
                        patch_rect,
                        1.0,
                        festerm_ui_egui::theme::SURFACE_TERMINAL,
                        &patch,
                        &patch_textures,
                        None,
                    )
                    .unwrap()
                    .expect("solid native patch");
                state.queue.submit([]);
                complete(&state);
            } else if name.starts_with("native-") {
                let target = if name == "native-allocate-copy" {
                    state.device.create_texture(&copy_descriptor)
                } else {
                    copy_target.clone()
                };
                let mut encoder = state.device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_texture(
                    surface.texture.as_image_copy(),
                    target.as_image_copy(),
                    target.size(),
                );
                state.queue.submit([encoder.finish()]);
                complete(&state);
            } else if name.starts_with("localized-") {
                let (mut output, actual_dimensions) =
                    step(&Workload::Localized.update(update_index));
                assert_eq!(actual_dimensions, dimensions);
                update_index += 1;
                renderer.handle_delta(&mut output.textures_delta);
                if name == "localized-without-composition" || name == "localized-ui-only" {
                    state.queue.submit([]);
                    complete(&state);
                } else {
                    let paint = context.tessellate(output.shapes, context.pixels_per_point());
                    if name == "localized-copy-all" {
                        let current = status.last_surface.lock().unwrap().clone().unwrap();
                        draw_with_copy(&state, &texture, &screen, &paint, &current);
                    } else {
                        let (copied, retained) = draw_composed(
                            &state,
                            &texture,
                            &screen,
                            &paint,
                            [0.0; 4],
                            Some(retained_ui),
                        );
                        if host_copy {
                            assert!(copied, "host-copy probe silently fell back");
                        }
                        if retained_composition && requires_retention {
                            assert!(
                                retained.is_some(),
                                "retained prefix silently fell back: {:?}",
                                retained_ui.stats(),
                            );
                        }
                    }
                    last_composited_frame = Some(paint);
                }
            } else {
                let (copied, retained) = draw_composed(
                    &state,
                    &texture,
                    &screen,
                    &frozen,
                    [0.0; 4],
                    Some(retained_ui),
                );
                if retained_composition && requires_retention {
                    assert!(
                        copied && retained.is_some(),
                        "frozen retained prefix silently fell back: {:?}",
                        retained_ui.stats(),
                    );
                }
            }
        };
        for _ in 0..5 {
            frame(&mut retained_ui);
            std::thread::sleep(INTERVAL);
        }
        complete(&state);
        let retained_before = retained_ui.stats();
        let cpu_start = process_cpu_time().unwrap();
        let started = Instant::now();
        let mut completed_draw = Duration::ZERO;
        for index in 0..FRAMES {
            let draw_started = Instant::now();
            frame(&mut retained_ui);
            completed_draw += draw_started.elapsed();
            std::thread::sleep((INTERVAL * (index + 1)).saturating_sub(started.elapsed()));
        }
        let elapsed = started.elapsed();
        let cpu = process_cpu_time().unwrap() - cpu_start;
        let retained_after = retained_ui.stats();
        if retained_composition && requires_retention {
            assert!(
                retained_after.reused_frames > retained_before.reused_frames,
                "the measured interval reused no prefix: {retained_after:?}",
            );
        }
        if let Some(paint) = last_composited_frame.as_deref().or_else(|| {
            matches!(name.as_str(), "frozen-all" | "frozen-all-repeat").then_some(frozen.as_slice())
        }) {
            let copied = read_image(&state, &texture);
            draw(&state, &texture, &screen, paint);
            let reference = read_image(&state, &texture);
            assert_same_pixels(
                &reference,
                &copied,
                "composition changed final application pixels",
            );
        }
        assert!(
            status.active.load(Ordering::Relaxed),
            "{:?}",
            status.first_failure.get()
        );
        let terminal_damage = matches!(
            name.as_str(),
            "localized-all" | "localized-copy-all" | "localized-without-composition"
        )
        .then(|| {
            serde_json::json!({
                "last_updated_pixels":status.last_updated_pixels.load(Ordering::Relaxed),
                "last_surface_pixels":status.last_surface_pixels.load(Ordering::Relaxed),
            })
        });
        let measurement = serde_json::json!({
            "case":name, "frames":FRAMES, "wall_ms":elapsed.as_secs_f64() * 1000.0,
            "cpu_ms":cpu.as_secs_f64() * 1000.0,
            "cpu_ms_per_frame":cpu.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            "completed_draw_ms_per_frame":completed_draw.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            "cpu_percent":100.0 * cpu.as_secs_f64() / elapsed.as_secs_f64() / logical_processors as f64,
            "frames_per_second":f64::from(FRAMES) / elapsed.as_secs_f64(),
            "terminal_damage":terminal_damage,
            "retained_prefix":{
                "reused_frames":retained_after.reused_frames-retained_before.reused_frames,
                "rebuilt_frames":retained_after.rebuilt_frames-retained_before.rebuilt_frames,
                "texture_bytes":retained_after.texture_bytes,
                "signature_bytes":retained_after.signature_bytes,
                "decline_reason":retained_after.decline_reason,
            },
        });
        eprintln!("residual-profile {measurement}");
        measurements.push(measurement);
        std::fs::write(
            directory.join("profile.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "adapter":format!("{info:?}"), "logical_processors":logical_processors,
                "target_format":format!("{:?}", state.target_format),
                "physical_size":screen.size_in_pixels, "pixels_per_point":screen.pixels_per_point,
                "grid":[120,40], "interval_ms":100, "exact_sampled_pixels":true,
                "exact_interpolated_pixels":true,
                "direct_copy_probe":direct_copy,
                "host_copy_probe":host_copy,
                "textureless_mesh_probe":textureless_mesh,
                "palette_frame_textureless":palette_frame_textureless,
                "retained_composition_probe":retained_composition,
                "scene":scene, "removed_fill_triangles":removed_fill_triangles,
                "primitives":metadata, "measurements":measurements,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
