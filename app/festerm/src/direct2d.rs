use eframe::{egui, egui_wgpu, wgpu};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Preference {
    Automatic,
    Disabled,
    Enabled,
}

impl Preference {
    fn from_environment(value: Option<&std::ffi::OsStr>) -> Result<Self, &'static str> {
        match value {
            None => Ok(Self::Automatic),
            Some(value) if value == "0" => Ok(Self::Disabled),
            Some(value) if value == "1" => Ok(Self::Enabled),
            _ => Err("FESTERM_EXPERIMENTAL_DIRECT2D expects 0 or 1; retaining egui-wgpu"),
        }
    }

    fn selects(
        self,
        windows_x64: bool,
        device: wgpu::DeviceType,
        backend: wgpu::Backend,
        format: wgpu::TextureFormat,
    ) -> bool {
        self != Self::Disabled && windows_x64 && eligible(device, backend, format)
    }
}

pub(crate) fn install_from_environment(
    context: &egui::Context,
    state: Option<&egui_wgpu::RenderState>,
) {
    let host_copy_value = std::env::var_os("FESTERM_EXPERIMENTAL_HOST_COPY");
    let host_copy_explicit = host_copy_value.as_deref() == Some(std::ffi::OsStr::new("1"));
    let host_copy = match host_copy_requested(host_copy_value.as_deref()) {
        Ok(requested) => requested,
        Err(message) => {
            tracing::warn!(target: "festerm::rendering", "{message}");
            false
        }
    };
    let retained_composition = match retained_composition_requested(
        std::env::var_os("FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION").as_deref(),
        host_copy,
    ) {
        Ok(requested) => requested,
        Err(message) => {
            tracing::warn!(target: "festerm::rendering", "{message}");
            false
        }
    };
    if retained_composition && !host_copy {
        tracing::warn!(target: "festerm::rendering",
            "retained composition requires FESTERM_EXPERIMENTAL_HOST_COPY=1; retaining existing rendering");
    }
    let preference = match Preference::from_environment(
        std::env::var_os("FESTERM_EXPERIMENTAL_DIRECT2D").as_deref(),
    ) {
        Ok(Preference::Disabled) => {
            if host_copy_explicit {
                tracing::info!(target: "festerm::rendering",
                    "host copy requires Direct2D; retaining ordinary rendering");
            }
            return;
        }
        Ok(preference) => preference,
        Err(message) => {
            tracing::warn!(target: "festerm::rendering", "{message}");
            return;
        }
    };
    let Some(state) = state else {
        if preference == Preference::Enabled || host_copy_explicit {
            tracing::warn!(target: "festerm::rendering", "Direct2D requires wgpu; retaining the current renderer");
        }
        return;
    };
    let info = state.adapter.get_info();
    if !preference.selects(
        cfg!(all(windows, target_arch = "x86_64")),
        info.device_type,
        info.backend,
        state.target_format,
    ) {
        if preference == Preference::Enabled || host_copy_explicit {
            tracing::info!(target: "festerm::rendering",
                "Direct2D requires Windows x64, a DX12 CPU adapter and 8-bit gamma target; retaining egui-wgpu");
        }
        return;
    }
    if host_copy && state.target_format != wgpu::TextureFormat::Bgra8Unorm {
        tracing::info!(target: "festerm::rendering",
            "host copy requires a BGRA target; retaining shader composition");
    }
    let composition = CompositionSelection {
        host_copy,
        retained_composition,
    }
    .selects(
        preference,
        cfg!(all(windows, target_arch = "x86_64")),
        info.device_type,
        info.backend,
        state.target_format,
    );
    #[cfg(all(windows, target_arch = "x86_64"))]
    match native::install_with_host_copy(context, state, composition.host_copy) {
        Ok(_) => {
            let retained = composition.retained_composition;
            state.renderer.write().retained_composition_enabled = retained;
            if retained {
                tracing::info!(target: "festerm::rendering",
                    "experimental retained window prefix enabled; ineligible frames retain ordinary composition");
            }
            tracing::info!(target: "festerm::rendering", ?preference, "Direct2D terminal painter enabled")
        }
        Err(error) => tracing::warn!(target: "festerm::rendering", %error,
            "Direct2D initialization failed; retaining egui-wgpu"),
    }
    #[cfg(not(all(windows, target_arch = "x86_64")))]
    let _ = (context, composition);
}

fn host_copy_requested(value: Option<&std::ffi::OsStr>) -> Result<bool, &'static str> {
    match value {
        None => Ok(true),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        _ => Err("FESTERM_EXPERIMENTAL_HOST_COPY expects 0 or 1; retaining shader composition"),
    }
}

fn retained_composition_requested(
    value: Option<&std::ffi::OsStr>,
    host_copy: bool,
) -> Result<bool, &'static str> {
    match value {
        None => Ok(host_copy),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        _ => Err("FESTERM_EXPERIMENTAL_RETAINED_COMPOSITION expects 0 or 1; retaining existing composition"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompositionSelection {
    host_copy: bool,
    retained_composition: bool,
}

impl CompositionSelection {
    fn selects(
        self,
        preference: Preference,
        windows_x64: bool,
        device: wgpu::DeviceType,
        backend: wgpu::Backend,
        format: wgpu::TextureFormat,
    ) -> Self {
        let host_copy = self.host_copy
            && preference.selects(windows_x64, device, backend, format)
            && format == wgpu::TextureFormat::Bgra8Unorm;
        Self {
            host_copy,
            retained_composition: host_copy && self.retained_composition,
        }
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

#[cfg(all(test, windows, target_arch = "x86_64"))]
mod profile;

#[cfg(all(test, windows, target_arch = "x86_64"))]
mod font_atlas_profile;

#[cfg(all(test, windows, target_arch = "x86_64"))]
mod host_copy;

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
        pub(super) reused_frames: AtomicU64,
        pub(super) first_failure: OnceLock<String>,
        unsupported_frame: AtomicBool,
        #[cfg(test)]
        pub(super) last_updated_pixels: AtomicU64,
        #[cfg(test)]
        pub(super) last_surface_pixels: AtomicU64,
        #[cfg(test)]
        pub(super) last_surface: Mutex<Option<festerm_windows_direct2d::Surface>>,
        #[cfg(test)]
        pub(super) font_atlas_samples: Mutex<Vec<(festerm_ui_egui::FontAtlasCapture, usize)>>,
    }

    struct Paint {
        pipeline: Arc<wgpu::RenderPipeline>,
        bindings: wgpu::BindGroup,
        copy: egui_wgpu::CallbackTextureCopy,
    }

    impl egui_wgpu::CallbackTrait for Paint {
        fn texture_copy(&self) -> Option<&egui_wgpu::CallbackTextureCopy> {
            Some(&self.copy)
        }

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

    #[cfg(test)]
    pub(super) fn install(
        context: &egui::Context,
        state: &egui_wgpu::RenderState,
    ) -> Result<Arc<Status>, festerm_windows_direct2d::Error> {
        install_with_host_copy(context, state, false)
    }

    pub(super) fn install_with_host_copy(
        context: &egui::Context,
        state: &egui_wgpu::RenderState,
        host_copy: bool,
    ) -> Result<Arc<Status>, festerm_windows_direct2d::Error> {
        install_with_painter_options(
            context,
            state,
            host_copy,
            festerm_ui_egui::NativePainterOptions::default(),
        )
    }

    pub(super) fn install_with_painter_options(
        context: &egui::Context,
        state: &egui_wgpu::RenderState,
        host_copy: bool,
        mut painter_options: festerm_ui_egui::NativePainterOptions,
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
        let renderer = Mutex::new(festerm_windows_direct2d::CachedRenderer::new(
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
            reused_frames: AtomicU64::new(0),
            first_failure: OnceLock::new(),
            unsupported_frame: AtomicBool::new(false),
            #[cfg(test)]
            last_updated_pixels: AtomicU64::new(0),
            #[cfg(test)]
            last_surface_pixels: AtomicU64::new(0),
            #[cfg(test)]
            last_surface: Mutex::new(None),
            #[cfg(test)]
            font_atlas_samples: Mutex::new(Vec::new()),
        });
        let observed = status.clone();
        painter_options.capture_font_atlas_timings |= timings.enabled();
        let capture_timings = painter_options.capture_font_atlas_timings;
        let paint = move |context: &egui::Context, frame: festerm_ui_egui::TerminalPaintFrame| {
            let total_started = timings.enabled().then(Instant::now);
            let mut render_timings =
                capture_timings.then(festerm_windows_direct2d::RenderTimings::default);
            let result = match renderer.lock() {
                Ok(mut renderer) => {
                    if frame.full_redraw {
                        renderer.invalidate();
                    }
                    let result = renderer.render(
                        frame.rect,
                        frame.pixels_per_point,
                        festerm_ui_egui::theme::SURFACE_TERMINAL,
                        &frame.primitives,
                        &frame.textures,
                        render_timings.as_mut(),
                    );
                    if result
                        .as_ref()
                        .is_err_and(|error| error.is_unsupported_frame())
                    {
                        renderer.invalidate();
                    }
                    result
                }
                Err(_) => {
                    observed.active.store(false, Ordering::Relaxed);
                    festerm_ui_egui::remove_root_terminal_painter(context);
                    tracing::error!(target: "festerm::rendering",
                        "Direct2D state poisoned; retaining egui-wgpu");
                    return None;
                }
            };
            let rendered = match result {
                Ok(Some(surface)) => surface,
                Ok(None) => return None,
                Err(error) if error.is_unsupported_frame() => {
                    let _ = observed.first_failure.set(error.to_string());
                    if !observed.unsupported_frame.swap(true, Ordering::Relaxed) {
                        tracing::warn!(target: "festerm::rendering", %error,
                            "unsupported Direct2D frame; retaining egui-wgpu for this frame");
                    }
                    return None;
                }
                Err(error) => {
                    let _ = observed.first_failure.set(error.to_string());
                    observed.active.store(false, Ordering::Relaxed);
                    festerm_ui_egui::remove_root_terminal_painter(context);
                    tracing::warn!(target: "festerm::rendering", %error,
                        "disabling experimental Direct2D; retaining egui-wgpu");
                    return None;
                }
            };
            if observed.unsupported_frame.swap(false, Ordering::Relaxed) {
                tracing::info!(target: "festerm::rendering",
                    "Direct2D terminal painting resumed after unsupported content");
            }
            let updated_regions = rendered.updated_regions;
            let updated_pixels = rendered.updated_pixels;
            let surface = rendered.surface;
            #[cfg(test)]
            {
                if capture_timings {
                    observed.font_atlas_samples.lock().unwrap().push((
                        frame.font_atlas_capture,
                        render_timings
                            .as_ref()
                            .expect("capture timing enabled")
                            .uploaded_texture_count,
                    ));
                }
                *observed.last_surface.lock().unwrap() = Some(surface.clone());
                observed
                    .last_updated_pixels
                    .store(updated_pixels, Ordering::Relaxed);
                observed.last_surface_pixels.store(
                    u64::from(surface.texture.width()) * u64::from(surface.texture.height()),
                    Ordering::Relaxed,
                );
            }

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
            let number = if updated_regions == 0 {
                let reused = observed.reused_frames.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::debug!(target: "festerm::rendering", direct2d_reused_frames = reused,
                    "reused unchanged Direct2D terminal frame");
                observed.frames.load(Ordering::Relaxed)
            } else {
                observed.frames.fetch_add(1, Ordering::Relaxed) + 1
            };
            if updated_regions > 0 && timings.should_log(number) {
                let render_timings = render_timings.as_ref().expect("timing capture enabled");
                tracing::info!(
                    target: "festerm::rendering",
                    direct2d_frame_number = number,
                    updated_regions,
                    updated_pixels,
                    surface_width = render_timings.surface_width,
                    surface_height = render_timings.surface_height,
                    mesh_count = render_timings.mesh_count,
                    vertex_count = render_timings.vertex_count,
                    index_count = render_timings.index_count,
                    texture_count = render_timings.texture_count,
                    uploaded_texture_count = render_timings.uploaded_texture_count,
                    font_atlas_capture_ms = frame.font_atlas_capture.elapsed.as_secs_f64() * 1000.0,
                    font_atlas_bytes = frame.font_atlas_capture.atlas_bytes,
                    font_atlas_cloned_bytes = frame.font_atlas_capture.cloned_bytes,
                    font_atlas_reused = frame.font_atlas_capture.reused,
                    analysis_ms = render_timings.analysis.as_secs_f64() * 1000.0,
                    texture_upload_ms = render_timings.texture_upload.as_secs_f64() * 1000.0,
                    geometry_prepare_ms = render_timings.geometry_prepare.as_secs_f64() * 1000.0,
                    geometry_prepare_calls = render_timings.geometry_prepare_calls,
                    native_draw_ms = render_timings.native_draw.as_secs_f64() * 1000.0,
                    composite_ms = composite_started
                        .map(|started| started.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or_default(),
                    total_ms = total_started
                        .map(|started| started.elapsed().as_secs_f64() * 1000.0)
                        .unwrap_or_default(),
                    "Direct2D production frame timings"
                );
            } else if updated_regions > 0 {
                tracing::debug!(target: "festerm::rendering", direct2d_frame_number = number,
                    "built Direct2D terminal surface");
            }
            Some(egui_wgpu::Callback::new_paint_callback(
                surface.rect,
                Paint {
                    pipeline: pipeline.clone(),
                    bindings,
                    copy: egui_wgpu::CallbackTextureCopy {
                        texture: surface.texture,
                        origin: surface.origin,
                    },
                },
            ))
        };
        festerm_ui_egui::install_root_terminal_painter_with_options(
            context,
            painter_options,
            paint,
        );
        state.renderer.write().final_callback_copy_enabled = host_copy;
        if host_copy {
            tracing::info!(target: "festerm::rendering",
                "experimental final-target host copy enabled; ineligible frames retain shader composition");
        }
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_copy_defaults_on_and_rejects_invalid_values() {
        for (value, expected) in [(None, true), (Some("0"), false), (Some("1"), true)] {
            assert_eq!(
                host_copy_requested(value.map(std::ffi::OsStr::new)).unwrap(),
                expected
            );
        }
        for value in ["", "true", "2", " 1"] {
            assert!(host_copy_requested(Some(std::ffi::OsStr::new(value))).is_err());
        }
    }

    #[test]
    fn retained_composition_defaults_follow_host_copy_and_preserve_explicit_overrides() {
        for host_copy in [false, true] {
            for (value, expected) in [(None, host_copy), (Some("0"), false), (Some("1"), true)] {
                assert_eq!(
                    retained_composition_requested(value.map(std::ffi::OsStr::new), host_copy)
                        .unwrap(),
                    expected
                );
            }
            for value in ["", "true", "2", " 1"] {
                assert!(retained_composition_requested(
                    Some(std::ffi::OsStr::new(value)),
                    host_copy
                )
                .is_err());
            }
        }
    }

    #[test]
    fn composition_defaults_and_opt_outs_never_force_unsupported_routing() {
        for host_value in [None, Some("0"), Some("1")] {
            let host_copy = host_copy_requested(host_value.map(std::ffi::OsStr::new)).unwrap();
            for retained_value in [None, Some("0"), Some("1")] {
                let requested = CompositionSelection {
                    host_copy,
                    retained_composition: retained_composition_requested(
                        retained_value.map(std::ffi::OsStr::new),
                        host_copy,
                    )
                    .unwrap(),
                };
                for preference in [
                    Preference::Automatic,
                    Preference::Disabled,
                    Preference::Enabled,
                ] {
                    for windows_x64 in [false, true] {
                        for device in [
                            wgpu::DeviceType::Cpu,
                            wgpu::DeviceType::DiscreteGpu,
                            wgpu::DeviceType::IntegratedGpu,
                            wgpu::DeviceType::VirtualGpu,
                            wgpu::DeviceType::Other,
                        ] {
                            for backend in [
                                wgpu::Backend::Dx12,
                                wgpu::Backend::Metal,
                                wgpu::Backend::Vulkan,
                                wgpu::Backend::Gl,
                            ] {
                                for format in [
                                    wgpu::TextureFormat::Bgra8Unorm,
                                    wgpu::TextureFormat::Rgba8Unorm,
                                    wgpu::TextureFormat::Bgra8UnormSrgb,
                                    wgpu::TextureFormat::Rgba8UnormSrgb,
                                    wgpu::TextureFormat::Rgba16Float,
                                ] {
                                    let selected = requested.selects(
                                        preference,
                                        windows_x64,
                                        device,
                                        backend,
                                        format,
                                    );
                                    let eligible = preference != Preference::Disabled
                                        && windows_x64
                                        && device == wgpu::DeviceType::Cpu
                                        && backend == wgpu::Backend::Dx12
                                        && format == wgpu::TextureFormat::Bgra8Unorm;
                                    assert_eq!(selected.host_copy, host_copy && eligible);
                                    assert_eq!(
                                        selected.retained_composition,
                                        requested.retained_composition && selected.host_copy
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn direct2d_default_and_overrides_preserve_platform_adapter_and_format_policy() {
        for (value, preference) in [
            (None, Preference::Automatic),
            (Some("0"), Preference::Disabled),
            (Some("1"), Preference::Enabled),
        ] {
            let parsed = Preference::from_environment(value.map(std::ffi::OsStr::new)).unwrap();
            assert_eq!(parsed, preference);
            for windows_x64 in [false, true] {
                for device in [
                    wgpu::DeviceType::Cpu,
                    wgpu::DeviceType::DiscreteGpu,
                    wgpu::DeviceType::IntegratedGpu,
                    wgpu::DeviceType::VirtualGpu,
                    wgpu::DeviceType::Other,
                ] {
                    for backend in [
                        wgpu::Backend::Dx12,
                        wgpu::Backend::Metal,
                        wgpu::Backend::Vulkan,
                        wgpu::Backend::Gl,
                    ] {
                        for format in [
                            wgpu::TextureFormat::Bgra8Unorm,
                            wgpu::TextureFormat::Rgba8Unorm,
                            wgpu::TextureFormat::Bgra8UnormSrgb,
                            wgpu::TextureFormat::Rgba8UnormSrgb,
                            wgpu::TextureFormat::Rgba16Float,
                        ] {
                            assert_eq!(
                                parsed.selects(windows_x64, device, backend, format),
                                preference != Preference::Disabled
                                    && windows_x64
                                    && device == wgpu::DeviceType::Cpu
                                    && backend == wgpu::Backend::Dx12
                                    && matches!(
                                        format,
                                        wgpu::TextureFormat::Bgra8Unorm
                                            | wgpu::TextureFormat::Rgba8Unorm
                                    ),
                                "{value:?}, {windows_x64}, {device:?}, {backend:?}, {format:?}",
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn direct2d_invalid_overrides_do_not_enable_the_default() {
        for value in ["", "true", "false", "auto", "2", " 1", "0 "] {
            assert!(Preference::from_environment(Some(std::ffi::OsStr::new(value))).is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(
                Preference::from_environment(Some(std::ffi::OsStr::from_bytes(&[0xff]))).is_err()
            );
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let value = std::ffi::OsString::from_wide(&[0xd800]);
            assert!(Preference::from_environment(Some(&value)).is_err());
        }
    }

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
        recover_palette: bool,
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
        let mut frame_number = 0;
        let mut frames_before_recovery = 0;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(321.0, 193.0))
            .with_pixels_per_point(scale)
            .with_max_steps(16)
            .renderer(test_renderer)
            .build_ui(|ui| {
                frame_number += 1;
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
                if recover_palette && frame_number == 4 {
                    if let Some(status) = &status {
                        use std::sync::atomic::Ordering;
                        frames_before_recovery = status.frames.load(Ordering::Relaxed);
                        assert!(status.first_failure.get().is_some());
                    }
                    terminal.ingest(b"\x1b[0m\x1b[2J\x1b[Hnative painting resumes");
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
                if recover_palette && frame_number == 3 {
                    ui.ctx().request_repaint();
                }
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
                "{}-scale-{scale}-clip-{clipped}-disabled-{disabled}-palette-{palette}{}.png",
                if native { "direct2d" } else { "wgpu" },
                if recover_palette { "-recovered" } else { "" },
            )))
            .unwrap();
        if let Some(status) = status {
            use std::sync::atomic::Ordering;
            assert!(
                status.active.load(Ordering::Relaxed),
                "unexpected fallback: {:?}",
                status.first_failure.get()
            );
            if palette {
                assert!(status
                    .first_failure
                    .get()
                    .unwrap()
                    .contains("256 feathered colors"));
                if recover_palette {
                    assert!(
                        status.frames.load(Ordering::Relaxed) > frames_before_recovery,
                        "eligible frames must return to native painting"
                    );
                }
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
                    let reference = fixture(false, scale, clipped, disabled, false, false);
                    let actual = fixture(true, scale, clipped, disabled, false, false);
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
            fixture(false, 1.0, false, false, true, false),
            fixture(true, 1.0, false, false, true, false)
        );
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn unsupported_native_palette_returns_to_native_painting() {
        let reference = fixture(false, 1.0, false, false, true, true);
        let actual = fixture(true, 1.0, false, false, true, true);
        assert_eq!(reference.dimensions(), actual.dimensions());
        assert!(
            reference
                .pixels()
                .filter(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] > 200)
                .count()
                > 10,
            "recovery comparison must contain terminal text"
        );
        assert_eq!(
            reference
                .pixels()
                .zip(actual.pixels())
                .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                .count(),
            0,
            "recovered native pixels must match the ordinary-renderer tolerance"
        );
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn native_full_redraw_replaces_identical_terminal_pixels_once() {
        use egui_kittest::{
            wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
            TestRenderer,
        };
        use festerm_core::{Dimensions, Terminal};
        use festerm_ui_egui::{EncodedInputSink, TerminalView};
        use std::sync::atomic::Ordering;

        struct Sink;
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, _: &[u8]) {
                panic!("local redraw must not send terminal input");
            }
            fn terminal_resizes_owned_by_backend(&self) -> bool {
                true
            }
        }
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
        let context = egui::Context::default();
        let status = super::native::install(&context, &state).unwrap();
        let mut terminal = Terminal::new(Dimensions::new(48, 24).unwrap()).unwrap();
        terminal.ingest(b"\x1b[?25lunchanged ASCII terminal");
        let mut view = TerminalView::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(480.0, 480.0),
            )),
            time: Some(0.0),
            ..Default::default()
        };
        let mut reference = None;
        for index in 0..8 {
            if index == 3 {
                view.request_full_redraw();
            }
            if index == 5 {
                terminal.ingest(b"\x1b[2J\x1b[H");
                view.request_full_redraw();
            }
            if index == 6 {
                terminal.ingest(b"unchanged ASCII terminal");
            }
            let mut output = context.run_ui(input.clone(), |ui| {
                view.show(ui, &mut terminal, &mut Sink);
            });
            renderer.handle_delta(&mut output.textures_delta);
            let image = renderer.render(&context, &output).unwrap();
            assert!(status.active.load(Ordering::Relaxed));
            if index == 2 || index == 4 || index == 7 {
                assert_eq!(status.last_updated_pixels.load(Ordering::Relaxed), 0);
            }
            if index == 2 {
                assert!(status.last_surface_pixels.load(Ordering::Relaxed) > 0);
                reference = Some(image.clone());
            }
            if index == 3 {
                assert_eq!(
                    status.last_updated_pixels.load(Ordering::Relaxed),
                    status.last_surface_pixels.load(Ordering::Relaxed),
                    "identical native pixels must all be replaced on explicit redraw"
                );
                assert_eq!(Some(image), reference);
            }
            if index == 6 {
                assert_eq!(
                    status.last_updated_pixels.load(Ordering::Relaxed),
                    status.last_surface_pixels.load(Ordering::Relaxed),
                    "a redraw while blank must also discard older retained pixels"
                );
            }
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn retained_terminal_updates_preserve_pixels_across_dpi_and_clipping() {
        use egui_kittest::{
            wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
            TestRenderer,
        };
        use festerm_core::{Dimensions, Terminal};
        use festerm_ui_egui::{EncodedInputSink, TerminalView};
        use std::sync::atomic::Ordering;

        struct Sink;
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, _: &[u8]) {}
            fn terminal_resizes_owned_by_backend(&self) -> bool {
                true
            }
        }
        let sequence = |native: bool, scale: f32, clipped: bool| {
            let mut setup = default_wgpu_setup();
            let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
                unreachable!()
            };
            options.instance_descriptor.backends = wgpu::Backends::DX12;
            let state = create_render_state(setup, Default::default());
            let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
            let context = egui::Context::default();
            crate::software_background::install(&context, &state);
            let status = native.then(|| super::native::install(&context, &state).unwrap());
            let mut terminal = Terminal::new(Dimensions::new(48, 24).unwrap()).unwrap();
            terminal.ingest(b"\x1b[?25l");
            for row in 1..=24 {
                terminal.ingest(
                    format!("\x1b[{row};1HRow {row:02} 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ")
                        .as_bytes(),
                );
            }
            terminal.ingest(
                "\x1b[3;1H\u{754c} e\u{301} \u{1f916} \u{1f469}\u{200d}\u{1f52c}".as_bytes(),
            );
            let mut view = TerminalView::default();
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(480.0, 480.0),
                )),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let updates = [
                "",
                "",
                "",
                "\x1b[9;3H\x1b[31mPATCH\x1b[0m",
                "\x1b[9;1H\x1b[2K",
                "\x1b[9;3H\x1b[4mABCD\x1b[0m",
                "\x1b[10;5H\x1b[?25h",
                "\x1b[?25l\x1b[3;1H\u{1f916}\x1b[K",
                "",
            ];
            let mut images = Vec::new();
            let mut saw_partial = false;
            for (index, bytes) in updates.iter().enumerate() {
                terminal.ingest(bytes.as_bytes());
                let mut output = context.run_ui(input.clone(), |ui| {
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        0.0,
                        egui::Color32::from_rgb(70, 25, 80),
                    );
                    ui.add_enabled_ui(index != 5, |ui| {
                        if clipped {
                            ui.set_clip_rect(egui::Rect::from_min_max(
                                egui::pos2(11.25, 13.5),
                                egui::pos2(460.5, 469.75),
                            ));
                        }
                        view.show(ui, &mut terminal, &mut Sink);
                    });
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(egui::pos2(15.0, 100.0), egui::vec2(180.0, 25.0)),
                        6.0,
                        egui::Color32::from_rgba_unmultiplied(200, 100, 50, 96),
                    );
                });
                renderer.handle_delta(&mut output.textures_delta);
                images.push(renderer.render(&context, &output).unwrap());
                if let Some(status) = &status {
                    assert!(
                        status.active.load(Ordering::Relaxed),
                        "{:?}",
                        status.first_failure.get()
                    );
                    let changed = status.last_updated_pixels.load(Ordering::Relaxed);
                    let full = status.last_surface_pixels.load(Ordering::Relaxed);
                    saw_partial |= index >= 3 && index != 5 && changed > 0 && changed < full;
                }
            }
            if native {
                assert!(saw_partial, "fixture must exercise retained updates");
            }
            images
        };
        for scale in [1.0, 1.25, 2.0] {
            for clipped in [false, true] {
                let reference = sequence(false, scale, clipped);
                let actual = sequence(true, scale, clipped);
                for (index, (reference, actual)) in reference.iter().zip(&actual).enumerate() {
                    assert_eq!(reference.dimensions(), actual.dimensions());
                    let mismatches = reference
                        .pixels()
                        .zip(actual.pixels())
                        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                        .count();
                    assert_eq!(
                        mismatches, 0,
                        "frame={index}, scale={scale}, clipped={clipped}"
                    );
                }
            }
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    #[ignore = "optional completed-render TUI profiling; not native presentation latency"]
    fn replay_terminal_tui_workloads() {
        use egui_kittest::{
            wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
            TestRenderer,
        };
        use festerm_core::{Dimensions, Terminal};
        use festerm_test_support::{captures, tui_workload::Workload};
        use festerm_ui_egui::{EncodedInputSink, TerminalView};
        use std::{path::PathBuf, sync::atomic::Ordering, time::Instant};

        struct Sink;
        impl EncodedInputSink for Sink {
            fn record_encoded_input(&mut self, _: &[u8]) {}
            fn terminal_resizes_owned_by_backend(&self) -> bool {
                true
            }
        }

        assert_eq!(
            std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
            Ok("1")
        );
        let directory = PathBuf::from(
            std::env::var_os("FESTERM_TUI_RENDER_OUT").expect("set FESTERM_TUI_RENDER_OUT"),
        );
        std::fs::create_dir_all(&directory).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let cases = [
            (
                "copilot",
                captures::frame_worth_rendering(captures::COPILOT).to_vec(),
                None,
            ),
            (
                "vim",
                captures::frame_worth_rendering(captures::VIM).to_vec(),
                None,
            ),
            (
                "htop",
                captures::frame_worth_rendering(captures::HTOP).to_vec(),
                None,
            ),
            (
                "tmux",
                captures::frame_worth_rendering(captures::TMUX).to_vec(),
                None,
            ),
            ("quiet", Workload::Quiet.setup(), Some(Workload::Quiet)),
            (
                "localized",
                Workload::Localized.setup(),
                Some(Workload::Localized),
            ),
            (
                "streaming",
                Workload::Streaming.setup(),
                Some(Workload::Streaming),
            ),
            (
                "full-redraw",
                Workload::FullRedraw.setup(),
                Some(Workload::FullRedraw),
            ),
        ];
        let dimensions = Dimensions::new(captures::COLUMNS, captures::ROWS).unwrap();
        let mut results = Vec::new();
        for (name, setup_bytes, workload) in cases {
            let mut reference: Option<image::RgbaImage> = None;
            for native in [false, true] {
                let mut setup = default_wgpu_setup();
                let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
                    unreachable!()
                };
                options.instance_descriptor.backends = wgpu::Backends::DX12;
                let state = create_render_state(setup, Default::default());
                assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
                let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
                let context = egui::Context::default();
                context.set_theme(egui::ThemePreference::Dark);
                context.set_visuals(festerm_ui_egui::theme::default_visuals());
                crate::software_background::install(&context, &state);
                let status = native.then(|| super::native::install(&context, &state).unwrap());
                let mut terminal = Terminal::new(dimensions).unwrap();
                terminal.ingest(&setup_bytes);
                let mut view = TerminalView::default();
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1440.0, 852.0),
                    )),
                    ..Default::default()
                };
                input
                    .viewports
                    .get_mut(&egui::ViewportId::ROOT)
                    .unwrap()
                    .native_pixels_per_point = Some(2.0);
                let step = |terminal: &mut Terminal, view: &mut TerminalView| {
                    context.run_ui(input.clone(), |ui| {
                        view.show(ui, terminal, &mut Sink);
                    })
                };
                for frame in 0..5 {
                    if let Some(workload) = workload {
                        terminal.ingest(&workload.update(frame));
                    }
                    let mut output = step(&mut terminal, &mut view);
                    renderer.handle_delta(&mut output.textures_delta);
                    renderer
                        .render(&context, &output)
                        .expect("TUI warmup render");
                }
                assert_eq!(terminal.dimensions(), dimensions);
                assert!(context
                    .viewport_rect()
                    .contains_rect(view.diagnostics().grid_rect.unwrap()));
                let mut timings = Vec::new();
                let mut image = None;
                for frame in 5..15 {
                    let bytes = workload
                        .map(|workload| workload.update(frame))
                        .unwrap_or_default();
                    let started = Instant::now();
                    terminal.ingest(&bytes);
                    let parse_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let started = Instant::now();
                    let mut output = step(&mut terminal, &mut view);
                    renderer.handle_delta(&mut output.textures_delta);
                    let ui_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let started = Instant::now();
                    image = Some(
                        renderer
                            .render(&context, &output)
                            .expect("completed TUI render"),
                    );
                    let draw_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let updated_pixels = status.as_ref().map(|status| {
                        let updated = status.last_updated_pixels.load(Ordering::Relaxed);
                        let full = status.last_surface_pixels.load(Ordering::Relaxed);
                        if name == "localized" && frame >= 10 {
                            assert!(
                                updated * 4 < full,
                                "localized update redrew most of the terminal"
                            );
                        }
                        updated
                    });
                    timings.push(serde_json::json!({
                        "frame": frame, "input_bytes": bytes.len(), "parse_ms": parse_ms,
                        "ui_ms": ui_ms, "draw_readback_ms": draw_ms,
                        "updated_pixels": updated_pixels,
                    }));
                }
                let image = image.unwrap();
                image
                    .save(directory.join(format!(
                        "{name}-{}.png",
                        if native { "direct2d" } else { "wgpu" }
                    )))
                    .unwrap();
                if let Some(status) = status {
                    assert!(
                        status.active.load(Ordering::Relaxed),
                        "{name}: native fallback: {:?}",
                        status.first_failure.get()
                    );
                    assert!(
                        status.frames.load(Ordering::Relaxed) > 0,
                        "{name}: no native frames"
                    );
                }
                if let Some(reference) = &reference {
                    assert_eq!(reference.dimensions(), image.dimensions());
                    let changed = reference
                        .pixels()
                        .zip(image.pixels())
                        .filter(|(a, b)| a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 2))
                        .count();
                    assert_eq!(changed, 0, "{name}: native rendering changed TUI pixels");
                } else {
                    reference = Some(image);
                }
                let average = |key: &str| {
                    timings
                        .iter()
                        .map(|value| value[key].as_f64().unwrap())
                        .sum::<f64>()
                        / timings.len() as f64
                };
                eprintln!("tui-replay case={name} native={native} parse_ms={:.3} ui_ms={:.3} draw_readback_ms={:.3}",
                    average("parse_ms"), average("ui_ms"), average("draw_readback_ms"));
                results.push(serde_json::json!({"case":name,"native":native,"frames":timings}));
                std::fs::write(
                    directory.join("timings.json"),
                    serde_json::to_vec_pretty(&results).unwrap(),
                )
                .unwrap();
            }
        }
    }
}
