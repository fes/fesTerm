use eframe::{egui, egui_wgpu, wgpu};

pub(crate) fn install_from_environment(
    context: &egui::Context,
    state: Option<&egui_wgpu::RenderState>,
) {
    match std::env::var("FESTERM_EXPERIMENTAL_DIRECT2D") {
        Err(std::env::VarError::NotPresent) => return,
        Ok(value) if value == "0" => return,
        Ok(value) if value == "1" => {}
        _ => {
            tracing::warn!(target: "festerm::rendering",
                "FESTERM_EXPERIMENTAL_DIRECT2D expects 0 or 1; retaining egui-wgpu");
            return;
        }
    }
    let Some(state) = state else {
        tracing::warn!(target: "festerm::rendering", "Direct2D requires wgpu; retaining the current renderer");
        return;
    };
    let info = state.adapter.get_info();
    if !eligible(info.device_type, info.backend, state.target_format) {
        tracing::info!(target: "festerm::rendering",
            "Direct2D requires a DX12 CPU adapter and 8-bit gamma target; retaining egui-wgpu");
        return;
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    match native::install(context, state) {
        Ok(_) => {
            tracing::info!(target: "festerm::rendering", "experimental Direct2D terminal painter enabled")
        }
        Err(error) => tracing::warn!(target: "festerm::rendering", %error,
            "Direct2D initialization failed; retaining egui-wgpu"),
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    {
        let _ = context;
        tracing::warn!(target: "festerm::rendering",
            "experimental Direct2D currently supports Windows x64 only; retaining egui-wgpu");
    }
}

fn eligible(device: wgpu::DeviceType, backend: wgpu::Backend, format: wgpu::TextureFormat) -> bool {
    device == wgpu::DeviceType::Cpu
        && backend == wgpu::Backend::Dx12
        && matches!(
            format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
        )
}

#[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TimingConfig {
    Disabled,
    EveryFrame,
    EveryNthFrame(u64),
}

#[cfg_attr(not(all(windows, target_arch = "x86_64")), allow(dead_code))]
impl TimingConfig {
    fn from_environment(value: Option<&str>) -> Result<Self, &'static str> {
        match value {
            None | Some("") | Some("0") => Ok(Self::Disabled),
            Some("1") => Ok(Self::EveryFrame),
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|interval| *interval > 1)
                .map(Self::EveryNthFrame)
                .ok_or("FESTERM_DIRECT2D_TIMINGS expects 0, 1, or an integer interval >= 2"),
        }
    }

    fn enabled(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    fn should_log(self, frame_number: u64) -> bool {
        match self {
            Self::Disabled => false,
            Self::EveryFrame => true,
            Self::EveryNthFrame(interval) => frame_number.is_multiple_of(interval),
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod native {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    };
    use std::time::Instant;
    use wgpu::util::DeviceExt;

    pub(super) struct Status {
        pub(super) active: AtomicBool,
        pub(super) frames: AtomicU64,
        pub(super) first_failure: OnceLock<String>,
    }

    struct Paint {
        pipeline: Arc<wgpu::RenderPipeline>,
        bindings: wgpu::BindGroup,
        _texture: wgpu::Texture,
    }

    impl egui_wgpu::CallbackTrait for Paint {
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

    const SHADER: &str = r"
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var<uniform> origin: vec4<u32>;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let points = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(points[index], 0.0, 1.0);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(image, vec2<i32>(position.xy) - vec2<i32>(origin.xy), 0);
}
";

    pub(super) fn install(
        context: &egui::Context,
        state: &egui_wgpu::RenderState,
    ) -> Result<Arc<Status>, festerm_windows_direct2d::Error> {
        let timings = match TimingConfig::from_environment(
            std::env::var("FESTERM_DIRECT2D_TIMINGS").ok().as_deref(),
        ) {
            Ok(config) => config,
            Err(message) => {
                tracing::warn!(target: "festerm::rendering", "{message}; timing logs disabled");
                TimingConfig::Disabled
            }
        };
        let renderer = Mutex::new(festerm_windows_direct2d::Renderer::new(
            state.device.clone(),
            state.queue.clone(),
        )?);
        let device = state.device.clone();
        let bindings_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("festerm Direct2D composite"),
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
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: std::num::NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("festerm Direct2D composite"),
            bind_group_layouts: &[Some(&bindings_layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("festerm Direct2D composite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = Arc::new(
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("festerm Direct2D composite"),
                layout: Some(&layout),
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
            }),
        );
        let status = Arc::new(Status {
            active: AtomicBool::new(true),
            frames: AtomicU64::new(0),
            first_failure: OnceLock::new(),
        });
        let observed = status.clone();
        festerm_ui_egui::install_root_terminal_painter(context, move |context, frame| {
            let total_started = timings.enabled().then(Instant::now);
            let mut render_timings = timings
                .enabled()
                .then(festerm_windows_direct2d::RenderTimings::default);
            let result = match renderer.lock() {
                Ok(mut renderer) => renderer.render(
                    frame.rect,
                    frame.pixels_per_point,
                    festerm_ui_egui::theme::SURFACE_TERMINAL,
                    &frame.primitives,
                    &frame.textures,
                    render_timings.as_mut(),
                ),
                Err(_) => {
                    observed.active.store(false, Ordering::Relaxed);
                    festerm_ui_egui::remove_root_terminal_painter(context);
                    tracing::error!(target: "festerm::rendering",
                        "Direct2D state poisoned; retaining egui-wgpu");
                    return None;
                }
            };
            let surface = match result {
                Ok(Some(surface)) => surface,
                Ok(None) => return None,
                Err(error) => {
                    let _ = observed.first_failure.set(error.to_string());
                    observed.active.store(false, Ordering::Relaxed);
                    festerm_ui_egui::remove_root_terminal_painter(context);
                    tracing::warn!(target: "festerm::rendering", %error,
                        "disabling experimental Direct2D; retaining egui-wgpu");
                    return None;
                }
            };
            let mut offset = [0u8; 16];
            offset[0..4].copy_from_slice(&surface.origin[0].to_le_bytes());
            offset[4..8].copy_from_slice(&surface.origin[1].to_le_bytes());
            let composite_started = timings.enabled().then(Instant::now);
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("festerm Direct2D origin"),
                contents: &offset,
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let view = surface.texture.create_view(&Default::default());
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("festerm Direct2D composite"),
                layout: &bindings_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
            let number = observed.frames.fetch_add(1, Ordering::Relaxed) + 1;
            if timings.should_log(number) {
                let render_timings = render_timings.as_ref().expect("timing capture enabled");
                tracing::info!(
                    target: "festerm::rendering",
                    direct2d_frame_number = number,
                    surface_width = render_timings.surface_width,
                    surface_height = render_timings.surface_height,
                    mesh_count = render_timings.mesh_count,
                    vertex_count = render_timings.vertex_count,
                    index_count = render_timings.index_count,
                    texture_count = render_timings.texture_count,
                    uploaded_texture_count = render_timings.uploaded_texture_count,
                    analysis_ms = render_timings.analysis.as_secs_f64() * 1000.0,
                    texture_upload_ms = render_timings.texture_upload.as_secs_f64() * 1000.0,
                    geometry_prepare_ms = render_timings.geometry_prepare.as_secs_f64() * 1000.0,
                    native_draw_ms = render_timings.native_draw.as_secs_f64() * 1000.0,
                    composite_ms = composite_started
                        .map(|started| started.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or_default(),
                    total_ms = total_started
                        .map(|started| started.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or_default(),
                    "Direct2D production frame timings"
                );
            } else {
                tracing::debug!(target: "festerm::rendering", direct2d_frame_number = number,
                    "built Direct2D terminal surface");
            }
            Some(egui_wgpu::Callback::new_paint_callback(
                surface.rect,
                Paint {
                    pipeline: pipeline.clone(),
                    bindings,
                    _texture: surface.texture,
                },
            ))
        });
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct2d_selection_preserves_hardware_other_backends_and_srgb() {
        for device in [
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::VirtualGpu,
            wgpu::DeviceType::Other,
        ] {
            assert!(!eligible(
                device,
                wgpu::Backend::Dx12,
                wgpu::TextureFormat::Bgra8Unorm
            ));
        }
        for backend in [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Gl,
            wgpu::Backend::Metal,
        ] {
            assert!(!eligible(
                wgpu::DeviceType::Cpu,
                backend,
                wgpu::TextureFormat::Bgra8Unorm
            ));
        }
        for (format, supported) in [
            (wgpu::TextureFormat::Bgra8Unorm, true),
            (wgpu::TextureFormat::Rgba8Unorm, true),
            (wgpu::TextureFormat::Bgra8UnormSrgb, false),
            (wgpu::TextureFormat::Rgba16Float, false),
        ] {
            assert_eq!(
                eligible(wgpu::DeviceType::Cpu, wgpu::Backend::Dx12, format),
                supported
            );
        }
    }

    #[test]
    fn direct2d_timing_gate_requires_explicit_valid_values() {
        assert_eq!(
            TimingConfig::from_environment(None),
            Ok(TimingConfig::Disabled)
        );
        assert_eq!(
            TimingConfig::from_environment(Some("0")),
            Ok(TimingConfig::Disabled)
        );
        assert_eq!(
            TimingConfig::from_environment(Some("1")),
            Ok(TimingConfig::EveryFrame)
        );
        assert_eq!(
            TimingConfig::from_environment(Some("120")),
            Ok(TimingConfig::EveryNthFrame(120))
        );
        assert!(TimingConfig::from_environment(Some("2"))
            .unwrap()
            .should_log(4));
        assert!(!TimingConfig::from_environment(Some("2"))
            .unwrap()
            .should_log(3));
        assert!(TimingConfig::from_environment(Some("abc")).is_err());
        assert!(TimingConfig::from_environment(Some("-1")).is_err());
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    fn fixture(
        native: bool,
        scale: f32,
        clipped: bool,
        disabled: bool,
        palette: bool,
    ) -> image::RgbaImage {
        use egui_kittest::{
            wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
            Harness,
        };
        use festerm_core::{Dimensions, Terminal};
        use festerm_ui_egui::{EncodedInputSink, TerminalView};
        struct Sink;
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, _: &[u8]) {}
        }
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let test_renderer = WgpuTestRenderer::from_render_state(state.clone());
        let mut terminal = Terminal::new(Dimensions::new(40, 10).unwrap()).unwrap();
        if palette {
            terminal.ingest(b"\x1b[?25l");
        } else {
            terminal.ingest(
                "Plain \u{754c} e\u{301}\r\n\x1b[41;97m Colored \x1b[0m\r\n\
             \x1b[4mUnderlined\x1b[0m\r\n\u{1f916} \u{1f469}\u{200d}\u{1f52c}\r\n\
             \x1b[2m\u{1f916}\x1b[0m == ->"
                    .as_bytes(),
            );
        }
        let mut view = TerminalView::default();
        let mut status = None;
        let mut configured = false;
        let mut palette_seeded = false;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(321.0, 193.0))
            .with_pixels_per_point(scale)
            .with_max_steps(16)
            .renderer(test_renderer)
            .build_ui(|ui| {
                if !configured {
                    let generation = festerm_ui_egui::install_terminal_fonts(ui.ctx());
                    view.set_font_set(festerm_ui_egui::TerminalFontSet::new(
                        Default::default(),
                        true,
                        generation,
                    ));
                    crate::software_background::install(ui.ctx(), &state);
                    if native {
                        status = Some(super::native::install(ui.ctx(), &state).unwrap());
                    }
                    configured = true;
                    ui.ctx().request_repaint();
                    return;
                }
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(70, 25, 80));
                ui.add_enabled_ui(!disabled, |ui| {
                    if clipped {
                        ui.set_clip_rect(egui::Rect::from_min_max(
                            egui::pos2(11.0, 13.0),
                            egui::pos2(310.0, 180.0),
                        ));
                    }
                    view.show(ui, &mut terminal, &mut Sink);
                    if palette && !palette_seeded {
                        let dimensions = terminal.dimensions();
                        assert!(dimensions.columns() * dimensions.rows() > 256);
                        for row in 0..dimensions.rows() {
                            for column in 0..dimensions.columns() {
                                let color = row * dimensions.columns() + column;
                                terminal.ingest(
                                    format!(
                                        "\x1b[{};{}H\x1b[48;2;{};{};64m ",
                                        row + 1,
                                        column + 1,
                                        color % 256,
                                        color / 256
                                    )
                                    .as_bytes(),
                                );
                            }
                        }
                        palette_seeded = true;
                        ui.ctx().request_repaint();
                    }
                });
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::pos2(15.0, 100.0), egui::vec2(180.0, 25.0)),
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(200, 100, 50, 96),
                );
            });
        harness.run();
        let image = harness.render().unwrap();
        drop(harness);
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("target")
            .join("direct2d-snapshots");
        std::fs::create_dir_all(&directory).unwrap();
        image
            .save(directory.join(format!(
                "{}-scale-{scale}-clip-{clipped}-disabled-{disabled}-palette-{palette}.png",
                if native { "direct2d" } else { "wgpu" }
            )))
            .unwrap();
        if let Some(status) = status {
            use std::sync::atomic::Ordering;
            assert_eq!(
                status.active.load(Ordering::Relaxed),
                !palette,
                "unexpected fallback: {:?}",
                status.first_failure.get()
            );
            if palette {
                assert!(status
                    .first_failure
                    .get()
                    .unwrap()
                    .contains("256 feathered colors"));
            } else {
                assert_eq!(status.frames.load(Ordering::Relaxed) > 0, !disabled);
            }
        }
        image
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn integrated_direct2d_matches_terminal_pixels_and_translucent_fallback() {
        for scale in [1.0, 1.25, 2.0] {
            for clipped in [false, true] {
                for disabled in [false, true] {
                    let reference = fixture(false, scale, clipped, disabled, false);
                    let actual = fixture(true, scale, clipped, disabled, false);
                    assert_eq!(reference.dimensions(), actual.dimensions());
                    if !disabled {
                        assert!(
                            reference
                                .pixels()
                                .filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200)
                                .count()
                                > 10,
                            "comparison must contain visible terminal text"
                        );
                    }
                    let mismatches = reference
                        .pixels()
                        .zip(actual.pixels())
                        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                        .count();
                    assert_eq!(
                        mismatches, 0,
                        "scale={scale}, clipped={clipped}, disabled={disabled}"
                    );
                }
            }
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn unsupported_native_palette_keeps_the_current_frame_pixels() {
        assert_eq!(
            fixture(false, 1.0, false, false, true),
            fixture(true, 1.0, false, false, true)
        );
    }
}
