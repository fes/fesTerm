use eframe::{egui, egui_wgpu, wgpu};
use std::sync::Arc;
use wgpu::util::DeviceExt;

const PANEL_SHADER: &str = r"
struct Locals { screen_size: vec4<f32> };
@group(0) @binding(0) var<uniform> locals: Locals;
override dithering: bool;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vertex(@location(0) position: vec2<f32>, @location(1) color: u32) -> VertexOutput {
    var out: VertexOutput;
    out.position = vec4<f32>(
        2.0 * position.x / locals.screen_size.x - 1.0,
        1.0 - 2.0 * position.y / locals.screen_size.y,
        0.0, 1.0,
    );
    out.color = vec4<f32>(
        f32(color & 255u), f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u), f32((color >> 24u) & 255u),
    ) / 255.0;
    return out;
}

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    var color = in.color;
    if dithering {
        let f = 0.06711056 * in.position.x + 0.00583715 * in.position.y;
        let noise = (fract(52.9829189 * fract(f)) - 0.5) * 0.95;
        color = vec4<f32>(color.rgb + vec3<f32>(noise / 255.0), color.a);
    }
    return color;
}
";

struct PanelRenderer {
    device: wgpu::Device,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    #[cfg(test)]
    paints: std::sync::atomic::AtomicUsize,
}

struct PanelPaint {
    renderer: Arc<PanelRenderer>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    uniform: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

fn panel_renderer_id() -> egui::Id {
    egui::Id::new("festerm::software-panel-background")
}

fn use_panel_pipeline(
    windows: bool,
    device_type: wgpu::DeviceType,
    backend: wgpu::Backend,
    format: wgpu::TextureFormat,
) -> bool {
    windows
        && device_type == wgpu::DeviceType::Cpu
        && backend == wgpu::Backend::Dx12
        && supported_format(format)
}

impl PanelRenderer {
    fn new(state: &egui_wgpu::RenderState, dithering: bool) -> Option<Arc<Self>> {
        if !supported_format(state.target_format) {
            return None;
        }
        let device = &state.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("festerm solid panel background"),
            source: wgpu::ShaderSource::Wgsl(PANEL_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("festerm panel uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("festerm panel pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("festerm solid panel background"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<egui::epaint::Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: std::mem::offset_of!(egui::epaint::Vertex, pos) as u64,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Uint32,
                            offset: std::mem::offset_of!(egui::epaint::Vertex, color) as u64,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: Default::default(),
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: state.target_format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                            dst_factor: wgpu::BlendFactor::One,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("dithering", f64::from(u32::from(dithering)))],
                    ..Default::default()
                },
            }),
            multiview_mask: None,
            cache: None,
        });
        Some(Arc::new(Self {
            device: device.clone(),
            pipeline,
            layout,
            #[cfg(test)]
            paints: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    fn shape(self: &Arc<Self>, ui: &egui::Ui, shape: egui::Shape) -> egui::Shape {
        let primitives = ui.ctx().tessellate(
            vec![egui::epaint::ClippedShape {
                clip_rect: ui.clip_rect(),
                shape: shape.clone(),
            }],
            ui.ctx().pixels_per_point(),
        );
        let mesh = match primitives.as_slice() {
            [egui::ClippedPrimitive {
                primitive: egui::epaint::Primitive::Mesh(mesh),
                ..
            }] if mesh.texture_id == egui::TextureId::default()
                && mesh.vertices.iter().all(|v| v.uv == egui::epaint::WHITE_UV)
                && !mesh.indices.is_empty()
                && mesh.is_valid() =>
            {
                mesh
            }
            [] => return egui::Shape::Noop,
            _ => {
                tracing::warn!(target: "festerm::rendering", "retaining standard panel painting for unexpected geometry");
                return shape;
            }
        };
        let Ok(index_count) = u32::try_from(mesh.indices.len()) else {
            tracing::warn!(target: "festerm::rendering", "retaining standard panel painting for oversized geometry");
            return shape;
        };
        let vertices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("festerm panel vertices"),
                contents: bytemuck::cast_slice(&mesh.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let indices = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("festerm panel indices"),
                contents: bytemuck::cast_slice(&mesh.indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        let uniform = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("festerm panel screen size"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("festerm panel bindings"),
            layout: &self.layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
            ui.ctx().viewport_rect(),
            PanelPaint {
                renderer: Arc::clone(self),
                vertices,
                indices,
                index_count,
                uniform,
                bindings,
            },
        ))
    }
}

impl egui_wgpu::CallbackTrait for PanelPaint {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        _resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        // Use the framebuffer's rounded physical size, not the UI's requested size.
        let size = [
            screen.size_in_pixels[0] as f32 / screen.pixels_per_point,
            screen.size_in_pixels[1] as f32 / screen.pixels_per_point,
            0.0,
            0.0,
        ];
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&size));
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        #[cfg(test)]
        self.renderer
            .paints
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        pass.set_pipeline(&self.renderer.pipeline);
        pass.set_bind_group(0, &self.bindings, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, 0..1);
    }
}

pub(crate) fn show_frame<R>(
    ui: &mut egui::Ui,
    frame: egui::Frame,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let renderer = ui
        .ctx()
        .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()));
    let Some(renderer) = renderer.filter(|_| {
        frame.fill.is_opaque()
            && frame.stroke == egui::Stroke::NONE
            && frame.shadow == egui::Shadow::NONE
            && ui.painter().opacity() == 1.0
            && ui.painter().is_visible()
            && ui.ctx().viewport_id() == egui::ViewportId::ROOT
            && ui.ctx().viewport_rect().min == egui::Pos2::ZERO
            && ui.ctx().layer_transform_to_global(ui.layer_id()).is_none()
    }) else {
        return frame.show(ui, contents);
    };
    let background = ui.painter().add(egui::Shape::Noop);
    // Retain egui's frame layout and feathered geometry, behind the child widgets.
    let mut prepared = frame.begin(ui);
    let inner = contents(&mut prepared.content_ui);
    let content_rect = prepared.content_ui.min_rect();
    if ui.is_rect_visible(frame.widget_rect(content_rect)) {
        ui.painter()
            .set(background, renderer.shape(ui, frame.paint(content_rect)));
    }
    egui::InnerResponse::new(inner, prepared.allocate_space(ui))
}

const SHADER: &str = r"
override red: f32;
override green: f32;
override blue: f32;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

@fragment
fn fragment() -> @location(0) vec4<f32> {
    return vec4<f32>(red, green, blue, 1.0);
}
";

struct SolidBackground {
    pipeline: wgpu::RenderPipeline,
}

impl egui_wgpu::CallbackTrait for SolidBackground {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        _resources: &egui_wgpu::CallbackResources,
    ) {
        render_pass.set_pipeline(&self.pipeline);
        render_pass.draw(0..3, 0..1);
    }
}

fn supported_format(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
    )
}

fn create_callback(render_state: &egui_wgpu::RenderState) -> Option<egui::PaintCallback> {
    if !supported_format(render_state.target_format) {
        return None;
    }
    let device = &render_state.device;
    let color = festerm_ui_egui::theme::SURFACE_TERMINAL.to_normalized_gamma_f32();
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("festerm solid terminal background"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("festerm solid terminal background"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("festerm solid terminal background"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vertex"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fragment"),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[
                    ("red", f64::from(color[0])),
                    ("green", f64::from(color[1])),
                    ("blue", f64::from(color[2])),
                ],
                ..Default::default()
            },
        }),
        multiview_mask: None,
        cache: None,
    });
    Some(egui_wgpu::Callback::new_paint_callback(
        egui::Rect::NOTHING,
        SolidBackground { pipeline },
    ))
}

pub(crate) fn install(context: &egui::Context, render_state: &egui_wgpu::RenderState) {
    if render_state.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        let Some(callback) = create_callback(render_state) else {
            tracing::info!(
                target: "festerm::app",
                format = ?render_state.target_format,
                "retaining standard background painting for this framebuffer format"
            );
            return;
        };
        festerm_ui_egui::install_terminal_background_callback(context, callback);
        let info = render_state.adapter.get_info();
        if use_panel_pipeline(
            cfg!(target_os = "windows"),
            info.device_type,
            info.backend,
            render_state.target_format,
        ) {
            if let Some(renderer) = PanelRenderer::new(
                render_state,
                egui_wgpu::RendererOptions::default().dithering,
            ) {
                context.data_mut(|data| data.insert_temp(panel_renderer_id(), renderer));
                tracing::info!(target: "festerm::app", "using textureless Launcher panel backgrounds on Windows WARP");
            }
        }
        tracing::info!(
            target: "festerm::app",
            "using native solid terminal backgrounds on a software rendering adapter"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        wgpu::{create_render_state, default_wgpu_setup, WgpuTestRenderer},
        Harness,
    };
    use festerm_core::{Dimensions, Terminal};
    use festerm_ui_egui::{EncodedInputSink, TerminalView};

    struct Sink;

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, _bytes: &[u8]) {}
    }

    #[test]
    fn panel_pipeline_is_limited_to_windows_dx12_cpu_adapters() {
        for device in [
            wgpu::DeviceType::Cpu,
            wgpu::DeviceType::DiscreteGpu,
            wgpu::DeviceType::IntegratedGpu,
            wgpu::DeviceType::VirtualGpu,
            wgpu::DeviceType::Other,
        ] {
            for windows in [false, true] {
                for backend in [
                    wgpu::Backend::Dx12,
                    wgpu::Backend::Vulkan,
                    wgpu::Backend::Metal,
                ] {
                    for format in [
                        wgpu::TextureFormat::Bgra8Unorm,
                        wgpu::TextureFormat::Bgra8UnormSrgb,
                    ] {
                        assert_eq!(
                            use_panel_pipeline(windows, device, backend, format),
                            windows
                                && device == wgpu::DeviceType::Cpu
                                && backend == wgpu::Backend::Dx12
                                && format == wgpu::TextureFormat::Bgra8Unorm,
                        );
                    }
                }
            }
        }
    }

    fn panel_image(
        native: bool,
        scale: f32,
        opacity: f32,
        clipped: bool,
        bordered: bool,
        srgb: bool,
        dithering: bool,
    ) -> image::RgbaImage {
        let options = egui_wgpu::RendererOptions {
            dithering,
            ..Default::default()
        };
        let mut state = create_render_state(default_wgpu_setup(), options);
        if srgb {
            state.target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
            *state.renderer.write() =
                egui_wgpu::Renderer::new(&state.device, state.target_format, options);
        }
        let renderer = native
            .then(|| PanelRenderer::new(&state, dithering))
            .flatten();
        if native {
            assert_eq!(renderer.is_some(), !srgb);
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(257.0, 157.0))
            .with_pixels_per_point(scale)
            .renderer(WgpuTestRenderer::from_render_state(state))
            .build_ui(|ui| {
                if let Some(renderer) = &renderer {
                    ui.ctx().data_mut(|data| {
                        data.insert_temp(panel_renderer_id(), Arc::clone(renderer))
                    });
                }
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(70, 25, 80));
                ui.set_opacity(opacity);
                if clipped {
                    ui.set_clip_rect(egui::Rect::from_min_max(
                        egui::pos2(19.3, 16.7),
                        egui::pos2(232.1, 140.2),
                    ));
                }
                for (position, color) in [
                    (egui::pos2(7.3, 11.7), festerm_ui_egui::theme::SURFACE_PANEL),
                    (egui::pos2(122.1, 31.3), egui::Color32::from_rgb(32, 73, 91)),
                ] {
                    ui.scope_builder(
                        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                            position,
                            egui::vec2(110.0, 115.0),
                        )),
                        |ui| {
                            let frame = egui::Frame::new()
                                .fill(color)
                                .corner_radius(egui::CornerRadius {
                                    nw: 5,
                                    ne: 13,
                                    sw: 9,
                                    se: 17,
                                })
                                .inner_margin(egui::Margin::same(5))
                                .outer_margin(egui::Margin::same(2))
                                .stroke(if bordered {
                                    egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE)
                                } else {
                                    egui::Stroke::NONE
                                });
                            let contents = |ui: &mut egui::Ui| {
                                ui.set_min_size(egui::vec2(86.3, 76.7));
                                ui.label("Panel");
                            };
                            if native {
                                show_frame(ui, frame, contents);
                            } else {
                                frame.show(ui, contents);
                            }
                        },
                    );
                }
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::pos2(16.0, 101.0), egui::vec2(197.0, 22.0)),
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(180, 90, 50, 128),
                );
            });
        harness.run();
        let image = harness.render().expect("panel framebuffer");
        if let Some(renderer) = &renderer {
            assert_eq!(
                renderer.paints.load(std::sync::atomic::Ordering::Relaxed) > 0,
                opacity == 1.0 && !bordered,
                "the pixel comparison must exercise the selected rendering path",
            );
        }
        image
    }

    #[test]
    fn textureless_panel_fill_matches_rounded_clipped_pixels_across_dpi() {
        for scale in [1.0, 1.25, 2.0] {
            for clipped in [false, true] {
                for dithering in [false, true] {
                    let ordinary = panel_image(false, scale, 1.0, clipped, false, false, dithering);
                    let native = panel_image(true, scale, 1.0, clipped, false, false, dithering);
                    assert_eq!(ordinary.dimensions(), native.dimensions());
                    let mismatches = ordinary
                        .pixels()
                        .zip(native.pixels())
                        .filter(|(a, b)| a != b)
                        .count();
                    assert_eq!(
                        mismatches, 0,
                        "scale={scale}, clipped={clipped}, dithering={dithering}"
                    );
                }
            }
        }
    }

    #[test]
    fn textureless_panel_fill_preserves_opacity_stroke_and_srgb_fallback() {
        for (opacity, bordered, srgb) in
            [(0.5, false, false), (1.0, true, false), (1.0, false, true)]
        {
            let ordinary = panel_image(false, 1.25, opacity, true, bordered, srgb, true);
            let native = panel_image(true, 1.25, opacity, true, bordered, srgb, true);
            assert_eq!(ordinary, native);
        }
    }

    #[test]
    #[ignore = "optional Windows WARP draw/readback replay, not an idle CPU qualification"]
    fn replay_large_warp_panels() {
        assert_eq!(
            std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
            Ok("1"),
            "set FESTERM_RUN_OPTIONAL_VALIDATION=1",
        );
        let options = egui_wgpu::RendererOptions::default();
        let mut reference: Option<image::RgbaImage> = None;
        for native in [false, true] {
            let state = create_render_state(default_wgpu_setup(), options);
            let info = state.adapter.get_info();
            assert!(
                use_panel_pipeline(
                    cfg!(windows),
                    info.device_type,
                    info.backend,
                    state.target_format
                ),
                "this replay requires a Windows DX12 CPU adapter; got {info:?}",
            );
            let renderer = PanelRenderer::new(&state, options.dithering).unwrap();
            let mut harness = Harness::builder()
                .with_size(egui::vec2(1774.0, 1075.0))
                .with_pixels_per_point(2.0)
                .renderer(WgpuTestRenderer::from_render_state(state))
                .build_ui(|ui| {
                    if native {
                        ui.ctx().data_mut(|data| {
                            data.insert_temp(panel_renderer_id(), Arc::clone(&renderer));
                        });
                    }
                    for left in [16.0, 900.0] {
                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                                egui::pos2(left, 40.0),
                                egui::vec2(858.0, 1008.0),
                            )),
                            |ui| {
                                show_frame(
                                    ui,
                                    egui::Frame::new()
                                        .fill(festerm_ui_egui::theme::SURFACE_PANEL)
                                        .corner_radius(14)
                                        .inner_margin(egui::Margin::same(8)),
                                    |ui| {
                                        ui.set_min_size(egui::vec2(842.0, 976.0));
                                        ui.heading("Launcher panel");
                                    },
                                );
                            },
                        );
                    }
                });
            harness.run();
            let image = harness.render().expect("large panel warmup");
            assert_eq!(image.dimensions(), (3548, 2150));
            if let Some(reference) = &reference {
                let mismatches = reference
                    .pixels()
                    .zip(image.pixels())
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(mismatches, 0, "large-panel pixels changed");
            } else {
                let color = festerm_ui_egui::theme::SURFACE_PANEL.to_array();
                assert!(image.pixels().filter(|p| p.0 == color).count() > 2_000_000);
                reference = Some(image);
            }
            let started = std::time::Instant::now();
            for _ in 0..5 {
                harness.render().expect("large panel replay frame");
            }
            eprintln!(
                "warp-panel-replay native={native} frames=5 draw_and_readback_seconds={:.6}",
                started.elapsed().as_secs_f64(),
            );
            assert_eq!(
                renderer.paints.load(std::sync::atomic::Ordering::Relaxed) > 0,
                native,
            );
        }
    }

    fn terminal_image(
        native: bool,
        scale: f32,
        disabled: bool,
        clipped: bool,
        srgb: bool,
    ) -> image::RgbaImage {
        let mut render_state =
            create_render_state(default_wgpu_setup(), egui_wgpu::RendererOptions::default());
        if srgb {
            render_state.target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
            *render_state.renderer.write() = egui_wgpu::Renderer::new(
                &render_state.device,
                render_state.target_format,
                egui_wgpu::RendererOptions::default(),
            );
        }
        let callback = native.then(|| create_callback(&render_state)).flatten();
        if native {
            assert_eq!(callback.is_some(), !srgb);
        }
        let mut terminal = Terminal::new(Dimensions::new(40, 10).unwrap()).unwrap();
        terminal.ingest(
            "Plain \u{754c}\r\n\x1b[41;97m Colored \x1b[0m\r\n\x1b[4mUnderlined\x1b[0m".as_bytes(),
        );
        let mut view = TerminalView::default();
        let mut sink = Sink;
        let mut harness = Harness::builder()
            .with_size(egui::vec2(257.0, 157.0))
            .with_pixels_per_point(scale)
            .with_max_steps(16)
            .renderer(WgpuTestRenderer::from_render_state(render_state))
            .build_ui(|ui| {
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, egui::Color32::from_rgb(70, 25, 80));
                if let Some(callback) = &callback {
                    festerm_ui_egui::install_terminal_background_callback(
                        ui.ctx(),
                        callback.clone(),
                    );
                }
                ui.add_enabled_ui(!disabled, |ui| {
                    if clipped {
                        ui.set_clip_rect(egui::Rect::from_min_max(
                            egui::pos2(11.0, 13.0),
                            egui::pos2(249.0, 145.0),
                        ));
                    }
                    view.show(ui, &mut terminal, &mut sink);
                });
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(egui::pos2(15.0, 100.0), egui::vec2(100.0, 25.0)),
                    6.0,
                    egui::Color32::from_rgba_unmultiplied(200, 100, 50, 96),
                );
            });
        harness.run();
        harness.render().expect("terminal image")
    }

    #[test]
    fn native_solid_background_matches_terminal_pixels_across_dpi() {
        for scale in [1.0, 1.25, 2.0] {
            for disabled in [false, true] {
                for clipped in [false, true] {
                    let ordinary = terminal_image(false, scale, disabled, clipped, false);
                    let native = terminal_image(true, scale, disabled, clipped, false);
                    if !disabled {
                        let red = festerm_ui_egui::resolve_color(
                            festerm_core::Color::Indexed(1),
                            egui::Color32::BLACK,
                        )
                        .to_array();
                        assert!(
                            ordinary.pixels().filter(|pixel| pixel.0 == red).count() > 50,
                            "the comparison must contain the colored terminal fixture"
                        );
                    }
                    assert_eq!(ordinary.dimensions(), native.dimensions());
                    let mismatches = ordinary
                        .pixels()
                        .zip(native.pixels())
                        .filter(|(ordinary, native)| ordinary != native)
                        .count();
                    assert_eq!(
                        mismatches, 0,
                        "scale={scale}, disabled={disabled}, clipped={clipped}"
                    );
                }
            }
        }
    }

    #[test]
    fn srgb_framebuffers_keep_standard_background_pixels() {
        let ordinary = terminal_image(false, 1.25, false, true, true);
        let native = terminal_image(true, 1.25, false, true, true);
        assert_eq!(ordinary.dimensions(), native.dimensions());
        assert!(ordinary.pixels().eq(native.pixels()));
    }

    #[test]
    fn only_eight_bit_gamma_framebuffers_use_the_solid_background_pipeline() {
        for (format, supported) in [
            (wgpu::TextureFormat::Rgba8Unorm, true),
            (wgpu::TextureFormat::Bgra8Unorm, true),
            (wgpu::TextureFormat::Rgba8UnormSrgb, false),
            (wgpu::TextureFormat::Bgra8UnormSrgb, false),
            (wgpu::TextureFormat::Rgba16Float, false),
        ] {
            assert_eq!(supported_format(format), supported);
        }
    }
}
