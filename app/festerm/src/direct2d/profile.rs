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

struct Sink;
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

fn draw(
    state: &egui_wgpu::RenderState,
    target: &wgpu::TextureView,
    screen: &egui_wgpu::ScreenDescriptor,
    primitives: &[ClippedPrimitive],
) {
    let mut renderer = state.renderer.write();
    let mut encoder = state.device.create_command_encoder(&Default::default());
    let extra = renderer.update_buffers(
        &state.device,
        &state.queue,
        &mut encoder,
        primitives,
        screen,
    );
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("residual CPU probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                ..Default::default()
            })
            .forget_lifetime();
        renderer.render(&mut pass, primitives, screen);
    }
    state
        .queue
        .submit(extra.into_iter().chain([encoder.finish()]));
    drop(renderer);
    complete(state);
}

struct SampledPaint {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroup,
    _texture: wgpu::Texture,
}

impl egui_wgpu::CallbackTrait for SampledPaint {
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

// Diagnostic alternative only: compare integer loads with nearest sampling.
fn sampled_callback(state: &egui_wgpu::RenderState, surface: &Surface) -> egui::PaintCallback {
    let layout = state
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sampled composite probe"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
    let pipeline_layout = state
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sampled composite probe"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
    let module = state
        .device
        .create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sampled composite probe"),
            source: wgpu::ShaderSource::Wgsl(
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
                .into(),
            ),
        });
    let pipeline = state
        .device
        .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sampled composite probe"),
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
    let bindings = state.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sampled composite probe"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
        ],
    });
    egui_wgpu::Callback::new_paint_callback(
        surface.rect,
        SampledPaint {
            pipeline,
            bindings,
            _texture: surface.texture.clone(),
        },
    )
}

fn screenshot(
    renderer: &mut WgpuTestRenderer,
    context: &egui::Context,
    primitives: &[ClippedPrimitive],
) -> image::RgbaImage {
    let output = egui::FullOutput {
        shapes: primitives
            .iter()
            .map(|primitive| egui::epaint::ClippedShape {
                clip_rect: primitive.clip_rect,
                shape: match &primitive.primitive {
                    Primitive::Mesh(mesh) => egui::Shape::Mesh(mesh.clone().into()),
                    Primitive::Callback(callback) => egui::Shape::Callback(callback.clone()),
                },
            })
            .collect(),
        pixels_per_point: context.pixels_per_point(),
        ..Default::default()
    };
    renderer.render(context, &output).expect("probe screenshot")
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
    let state = create_render_state(setup, Default::default());
    let info = state.adapter.get_info();
    assert_eq!(info.device_type, wgpu::DeviceType::Cpu);
    let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    context.set_visuals(festerm_ui_egui::theme::default_visuals());
    crate::software_background::install(&context, &state);
    let status = super::native::install(&context, &state).unwrap();
    let scene = std::env::var_os("FESTERM_TUI_PROFILE_SCENE")
        .map(|value| value.into_string().expect("profile scene must be UTF-8"))
        .unwrap_or_else(|| "terminal".into());
    assert!(matches!(scene.as_str(), "terminal" | "application"));
    let mut application = (scene == "application").then(|| {
        let (app, _, transport) = crate::app::FesTermApp::for_test_with_fake_ssh_session([]);
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target = texture.create_view(&Default::default());
    for _ in 0..8 {
        let (mut output, _) = step(&[]);
        renderer.handle_delta(&mut output.textures_delta);
        let paint = context.tessellate(output.shapes, context.pixels_per_point());
        draw(&state, &target, &screen, &paint);
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
        draw(&state, &target, &screen, &primitives);
    }
    assert!(
        status.active.load(Ordering::Relaxed),
        "{:?}",
        status.first_failure.get()
    );
    let surface = status.last_surface.lock().unwrap().clone().unwrap();
    let native_index = primitives
        .iter()
        .position(|primitive| matches!(&primitive.primitive, Primitive::Callback(callback) if callback.rect == surface.rect))
        .expect("locate native image callback");
    let mut sampled = primitives.clone();
    sampled[native_index].primitive = Primitive::Callback(sampled_callback(&state, &surface));
    let original_image = screenshot(&mut renderer, &context, &primitives);
    let sampled_image = screenshot(&mut renderer, &context, &sampled);
    original_image.save(directory.join("original.png")).unwrap();
    sampled_image.save(directory.join("sampled.png")).unwrap();
    assert_eq!(original_image.dimensions(), sampled_image.dimensions());
    assert_eq!(
        original_image
            .pixels()
            .zip(sampled_image.pixels())
            .filter(|(a, b)| a != b)
            .count(),
        0,
        "sampled compositor changed pixels"
    );
    if let Some(reference) = std::env::var_os("FESTERM_TUI_PROFILE_REFERENCE") {
        let reference = image::open(reference).unwrap().into_rgba8();
        assert_eq!(original_image.dimensions(), reference.dimensions());
        assert_eq!(
            original_image
                .pixels()
                .zip(reference.pixels())
                .filter(|(a, b)| a != b)
                .count(),
            0,
            "full application pixels changed"
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
            }),
        })
        .collect::<Vec<_>>();
    let mut cases = vec![
        ("sleep-only".to_owned(), Vec::new()),
        ("clear-only".to_owned(), Vec::new()),
        ("frozen-all".to_owned(), primitives.clone()),
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
    cases.extend([
        ("meshes-only".to_owned(), meshes),
        ("without-solid-mesh-fills".to_owned(), without_fills),
        ("localized-all".to_owned(), Vec::new()),
        ("localized-without-composition".to_owned(), Vec::new()),
        ("frozen-all-repeat".to_owned(), primitives),
    ]);
    let logical_processors = std::thread::available_parallelism().unwrap().get();
    let mut measurements = Vec::new();
    const FRAMES: u32 = 100;
    const INTERVAL: Duration = Duration::from_millis(100);
    let mut update_index = 8;
    for (name, frozen) in cases {
        let mut frame = || {
            if name == "sleep-only" {
                return;
            }
            if name.starts_with("localized-") {
                let (mut output, actual_dimensions) =
                    step(&Workload::Localized.update(update_index));
                assert_eq!(actual_dimensions, dimensions);
                update_index += 1;
                renderer.handle_delta(&mut output.textures_delta);
                if name == "localized-without-composition" {
                    state.queue.submit([]);
                    complete(&state);
                } else {
                    let paint = context.tessellate(output.shapes, context.pixels_per_point());
                    draw(&state, &target, &screen, &paint);
                }
            } else {
                draw(&state, &target, &screen, &frozen);
            }
        };
        for _ in 0..5 {
            frame();
            std::thread::sleep(INTERVAL);
        }
        complete(&state);
        let cpu_start = process_cpu_time().unwrap();
        let started = Instant::now();
        let mut completed_draw = Duration::ZERO;
        for index in 0..FRAMES {
            let draw_started = Instant::now();
            frame();
            completed_draw += draw_started.elapsed();
            std::thread::sleep((INTERVAL * (index + 1)).saturating_sub(started.elapsed()));
        }
        let elapsed = started.elapsed();
        let cpu = process_cpu_time().unwrap() - cpu_start;
        assert!(
            status.active.load(Ordering::Relaxed),
            "{:?}",
            status.first_failure.get()
        );
        let measurement = serde_json::json!({
            "case":name, "frames":FRAMES, "wall_ms":elapsed.as_secs_f64() * 1000.0,
            "cpu_ms":cpu.as_secs_f64() * 1000.0,
            "cpu_ms_per_frame":cpu.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            "completed_draw_ms_per_frame":completed_draw.as_secs_f64() * 1000.0 / f64::from(FRAMES),
            "cpu_percent":100.0 * cpu.as_secs_f64() / elapsed.as_secs_f64() / logical_processors as f64,
            "frames_per_second":f64::from(FRAMES) / elapsed.as_secs_f64(),
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
                "scene":scene, "removed_fill_triangles":removed_fill_triangles,
                "primitives":metadata, "measurements":measurements,
            }))
            .unwrap(),
        )
        .unwrap();
    }
}
