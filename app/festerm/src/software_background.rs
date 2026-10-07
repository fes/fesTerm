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
    paint_namespace: Arc<()>,
    #[cfg(test)]
    paints: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    palette_frames: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    palette_fills: std::sync::atomic::AtomicUsize,
}

struct PanelPaint {
    renderer: Arc<PanelRenderer>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    uniform: wgpu::Buffer,
    bindings: wgpu::BindGroup,
    geometry: egui::Mesh,
}

fn panel_renderer_id() -> egui::Id {
    egui::Id::new("festerm::software-panel-background")
}

fn white_mesh_geometry(mesh: &egui::Mesh) -> bool {
    mesh.texture_id == egui::TextureId::default()
        && mesh
            .vertices
            .iter()
            .all(|vertex| vertex.uv == egui::epaint::WHITE_UV)
        && !mesh.indices.is_empty()
        && mesh.is_valid()
}

pub(crate) fn opaque_window_frame(shape: &egui::Shape) -> bool {
    matches!(shape, egui::Shape::Vec(shapes) if matches!(
        shapes.as_slice(),
        [egui::Shape::Rect(shadow), egui::Shape::Rect(frame)]
            if shadow.brush.is_none() && frame.brush.is_none() && frame.fill.is_opaque()
    ))
}

pub(crate) fn complete_opaque_window_frame(
    shape: &egui::epaint::ClippedShape,
    viewport: egui::Rect,
) -> bool {
    if !viewport.is_finite()
        || viewport.min != egui::Pos2::ZERO
        || !viewport.is_positive()
        || !opaque_window_frame(&shape.shape)
    {
        return false;
    }
    let egui::Shape::Vec(parts) = &shape.shape else {
        unreachable!("matched a shadow/frame pair");
    };
    let [egui::Shape::Rect(shadow), egui::Shape::Rect(frame)] = parts.as_slice() else {
        unreachable!("matched two rectangles");
    };
    frame.rect.is_finite()
        && frame.rect.is_positive()
        && shadow.rect.is_finite()
        && shadow.rect.is_positive()
        && viewport.contains_rect(frame.rect)
        && shape.clip_rect.contains_rect(frame.rect)
        && shadow.fill.r() == 0
        && shadow.fill.g() == 0
        && shadow.fill.b() == 0
        && shadow.fill.a() != 0
        && !shadow.fill.is_opaque()
        && shadow.stroke == egui::Stroke::NONE
        && shadow.blur_width.is_finite()
}

fn opaque_palette_fill(shape: &egui::Shape) -> bool {
    matches!(shape, egui::Shape::Rect(rect) if rect.brush.is_none() && rect.fill.is_opaque())
}

fn palette_frame_layer() -> egui::LayerId {
    egui::LayerId::new(
        egui::Order::Middle,
        festerm_ui_egui::palette::PaletteState::window_id(),
    )
}

fn supports_palette_frame(ui: &egui::Ui) -> bool {
    supports_panel_painter(ui)
        && ui
            .ctx()
            .layer_transform_to_global(palette_frame_layer())
            .is_none()
}

struct PaletteFrameBackground;

impl egui::Plugin for PaletteFrameBackground {
    fn debug_name(&self) -> &'static str {
        "festerm palette frame background"
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        if !supports_palette_frame(ui) {
            return;
        }
        let context = ui.ctx();
        let layer = palette_frame_layer();
        #[cfg(test)]
        if context.data(|data| {
            data.get_temp::<bool>(palette_frame_disabled_id())
                .unwrap_or(false)
        }) {
            return;
        }
        let Some(renderer) =
            context.data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
        else {
            return;
        };
        let fills_enabled = palette_fills_enabled(context);
        let backgrounds = context.graphics(|graphics| {
            Some(
                graphics
                    .get(layer)?
                    .all_entries()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        let window_frame = opaque_window_frame(&entry.shape);
                        (window_frame || fills_enabled && opaque_palette_fill(&entry.shape))
                            .then(|| (egui::layers::ShapeIdx(index), entry.clone(), window_frame))
                    })
                    .collect::<Vec<_>>(),
            )
        });
        let Some(backgrounds) = backgrounds else {
            return;
        };
        if !backgrounds.iter().any(|(_, _, frame)| *frame) {
            return;
        }
        // Tessellation takes the context lock; never do it while editing graphics.
        for (index, background, window_frame) in backgrounds {
            let shape = renderer.shape_for_clip(context, background.clip_rect, background.shape);
            #[cfg(test)]
            if matches!(shape, egui::Shape::Callback(_)) {
                let counter = if window_frame {
                    &renderer.palette_frames
                } else {
                    &renderer.palette_fills
                };
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            #[cfg(not(test))]
            let _ = window_frame;
            context.graphics_mut(|graphics| {
                graphics
                    .get_mut(layer)
                    .expect("captured palette paint list remains present in the same pass")
                    .set(index, background.clip_rect, shape);
            });
        }
    }
}

fn palette_fills_enabled(_context: &egui::Context) -> bool {
    #[cfg(test)]
    {
        _context.data(|data| {
            !data
                .get_temp::<bool>(palette_fills_disabled_id())
                .unwrap_or(false)
        })
    }
    #[cfg(not(test))]
    {
        true
    }
}

#[cfg(test)]
fn palette_fills_disabled_id() -> egui::Id {
    egui::Id::new("festerm::test-ordinary-palette-fills")
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(crate) fn set_palette_fills_enabled(context: &egui::Context, enabled: bool) {
    context.data_mut(|data| data.insert_temp(palette_fills_disabled_id(), !enabled));
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(crate) fn palette_fill_conversions(context: &egui::Context) -> usize {
    context
        .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
        .expect("palette probe requires the installed eligible panel renderer")
        .palette_fills
        .load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
fn palette_frame_disabled_id() -> egui::Id {
    egui::Id::new("festerm::test-ordinary-palette-frame")
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(crate) fn set_palette_frame_enabled(context: &egui::Context, enabled: bool) {
    context.data_mut(|data| data.insert_temp(palette_frame_disabled_id(), !enabled));
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(crate) fn palette_frame_conversions(context: &egui::Context) -> usize {
    context
        .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
        .expect("palette probe requires the installed eligible panel renderer")
        .palette_frames
        .load(std::sync::atomic::Ordering::Relaxed)
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
            paint_namespace: Arc::new(()),
            #[cfg(test)]
            paints: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            palette_frames: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            palette_fills: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    fn shape(self: &Arc<Self>, ui: &egui::Ui, shape: egui::Shape) -> egui::Shape {
        self.shape_for_clip(ui.ctx(), ui.clip_rect(), shape)
    }

    fn shape_for_clip(
        self: &Arc<Self>,
        context: &egui::Context,
        clip_rect: egui::Rect,
        shape: egui::Shape,
    ) -> egui::Shape {
        let mut primitives = context.tessellate(
            vec![egui::epaint::ClippedShape {
                clip_rect,
                shape: shape.clone(),
            }],
            context.pixels_per_point(),
        );
        let mesh = match primitives.as_mut_slice() {
            [egui::ClippedPrimitive {
                primitive: egui::epaint::Primitive::Mesh(mesh),
                ..
            }] if white_mesh_geometry(mesh) => std::mem::take(mesh),
            [] => return egui::Shape::Noop,
            _ => {
                tracing::warn!(target: "festerm::rendering", "retaining standard panel painting for unexpected geometry");
                return shape;
            }
        };
        self.mesh_shape(context.viewport_rect(), mesh)
            .unwrap_or(shape)
    }

    fn mesh_shape(self: &Arc<Self>, viewport: egui::Rect, mesh: egui::Mesh) -> Option<egui::Shape> {
        let Ok(index_count) = u32::try_from(mesh.indices.len()) else {
            tracing::warn!(target: "festerm::rendering", "retaining standard panel painting for oversized geometry");
            return None;
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
        Some(egui::Shape::Callback(
            egui_wgpu::Callback::new_paint_callback(
                viewport,
                PanelPaint {
                    renderer: Arc::clone(self),
                    vertices,
                    indices,
                    index_count,
                    uniform,
                    bindings,
                    geometry: mesh,
                },
            ),
        ))
    }
}

impl egui_wgpu::CallbackTrait for PanelPaint {
    fn paint_key(&self) -> Option<egui_wgpu::CallbackPaintKey> {
        let vertices: &[u8] = bytemuck::cast_slice(&self.geometry.vertices);
        let indices: &[u8] = bytemuck::cast_slice(&self.geometry.indices);
        let mut bytes = Vec::with_capacity(8 + vertices.len() + indices.len());
        bytes.extend_from_slice(&(vertices.len() as u64).to_le_bytes());
        bytes.extend_from_slice(vertices);
        bytes.extend_from_slice(indices);
        Some(egui_wgpu::CallbackPaintKey {
            namespace: self.renderer.paint_namespace.clone(),
            bytes: bytes.into(),
        })
    }

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

fn supports_panel_painter(ui: &egui::Ui) -> bool {
    ui.painter().opacity() == 1.0
        && ui.painter().is_visible()
        && ui.ctx().viewport_id() == egui::ViewportId::ROOT
        && ui.ctx().viewport_rect().min == egui::Pos2::ZERO
        && ui.ctx().layer_transform_to_global(ui.layer_id()).is_none()
}

fn supports_panel_frame(ui: &egui::Ui, frame: &egui::Frame) -> bool {
    frame.fill.is_opaque() && frame.shadow == egui::Shadow::NONE && supports_panel_painter(ui)
}

pub(crate) fn full_root_black_backdrop(
    shape: &egui::epaint::ClippedShape,
    viewport: egui::Rect,
) -> bool {
    let egui::Shape::Rect(rect) = &shape.shape else {
        return false;
    };
    viewport.is_finite()
        && viewport.min == egui::Pos2::ZERO
        && viewport.is_positive()
        && rect.rect == viewport
        && shape.clip_rect.contains_rect(viewport)
        && rect.fill.r() == 0
        && rect.fill.g() == 0
        && rect.fill.b() == 0
        && rect.fill.a() != 0
        && !rect.fill.is_opaque()
        && rect.brush.is_none()
        && rect.corner_radius == egui::CornerRadius::ZERO
        && rect.stroke == egui::Stroke::NONE
        && rect.blur_width == 0.0
}

#[cfg(test)]
fn picker_backdrops_disabled_id() -> egui::Id {
    egui::Id::new("festerm::test-ordinary-picker-backdrop")
}

#[cfg(all(test, windows, target_arch = "x86_64"))]
pub(crate) fn set_picker_backdrops_enabled(context: &egui::Context, enabled: bool) {
    context.data_mut(|data| data.insert_temp(picker_backdrops_disabled_id(), !enabled));
}

fn picker_backdrops_enabled(_context: &egui::Context) -> bool {
    #[cfg(test)]
    if _context.data(|data| {
        data.get_temp::<bool>(picker_backdrops_disabled_id())
            .unwrap_or(false)
    }) {
        return false;
    }
    true
}

#[cfg(test)]
fn picker_frames_disabled_id() -> egui::Id {
    egui::Id::new("festerm::test-ordinary-picker-frame")
}

#[cfg(test)]
pub(crate) fn set_picker_frames_enabled(context: &egui::Context, enabled: bool) {
    context.data_mut(|data| data.insert_temp(picker_frames_disabled_id(), !enabled));
}

fn picker_frames_enabled(_context: &egui::Context) -> bool {
    #[cfg(test)]
    if _context.data(|data| {
        data.get_temp::<bool>(picker_frames_disabled_id())
            .unwrap_or(false)
    }) {
        return false;
    }
    true
}

fn picker_frame_background(
    context: &egui::Context,
    layer: egui::LayerId,
    start: egui::layers::ShapeIdx,
    owned_frame_rect: egui::Rect,
) -> Option<(egui::layers::ShapeIdx, egui::epaint::ClippedShape)> {
    let viewport = context.viewport_rect();
    context.graphics(|graphics| {
        let mut matched = None;
        for (index, shape) in graphics.get(layer)?.all_entries().enumerate().skip(start.0) {
            if complete_opaque_window_frame(shape, viewport)
                && matched.replace((egui::layers::ShapeIdx(index), shape.clone())).is_some()
            {
                tracing::warn!(target: "festerm::rendering", "retaining ordinary picker painting for ambiguous frame geometry");
                return None;
            }
        }
        matched.filter(|(_, shape)| {
            matches!(&shape.shape, egui::Shape::Vec(parts) if matches!(
                parts.as_slice(),
                [_, egui::Shape::Rect(frame)] if frame.rect == owned_frame_rect
            ))
        })
    })
}

pub(crate) fn show_picker_modal<R>(
    context: &egui::Context,
    modal: egui::Modal,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    let layer = modal.area.layer();
    let color = modal.backdrop_color;
    let requested_frame = modal.frame;
    let start = context.graphics(|graphics| {
        graphics
            .get(layer)
            .map_or(egui::layers::ShapeIdx(0), egui::layers::PaintList::next_idx)
    });
    let mut supported = false;
    let mut owned_frame_rect = egui::Rect::NOTHING;
    let response = modal.show(context, |ui| {
        supported = supports_panel_painter(ui);
        let frame = requested_frame.unwrap_or_else(|| egui::Frame::popup(ui.style()));
        let inner = contents(ui);
        owned_frame_rect = frame.widget_rect(ui.min_rect());
        inner
    });
    if !supported || context.layer_transform_to_global(layer).is_some() {
        return response;
    }
    let Some(renderer) =
        context.data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
    else {
        return response;
    };
    let viewport = context.viewport_rect();
    if picker_backdrops_enabled(context) && response.backdrop_response.rect == viewport {
        let backdrop = context.graphics(|graphics| {
            let mut matched = None;
            for (index, shape) in graphics.get(layer)?.all_entries().enumerate().skip(start.0) {
                if full_root_black_backdrop(shape, viewport)
                    && matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == color)
                    && matched.replace((egui::layers::ShapeIdx(index), shape.clone())).is_some()
                {
                    tracing::warn!(target: "festerm::rendering", "retaining ordinary picker painting for ambiguous backdrop geometry");
                    return None;
                }
            }
            matched
        });
        if let Some((index, backdrop)) = backdrop {
            let shape = renderer.shape_for_clip(context, backdrop.clip_rect, backdrop.shape);
            context.graphics_mut(|graphics| {
                graphics
                    .get_mut(layer)
                    .expect("picker paint list remains present in the same pass")
                    .set(index, backdrop.clip_rect, shape);
            });
        }
    }
    if picker_frames_enabled(context) {
        if let Some((index, frame)) =
            picker_frame_background(context, layer, start, owned_frame_rect)
        {
            let shape = renderer.shape_for_clip(context, frame.clip_rect, frame.shape);
            context.graphics_mut(|graphics| {
                graphics
                    .get_mut(layer)
                    .expect("picker paint list remains present in the same pass")
                    .set(index, frame.clip_rect, shape);
            });
        }
    }
    response
}

pub(crate) fn show_frame<R>(
    ui: &mut egui::Ui,
    frame: egui::Frame,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let renderer = ui
        .ctx()
        .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()));
    let Some(renderer) = renderer.filter(|_| supports_panel_frame(ui, &frame)) else {
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

#[cfg(test)]
pub(crate) struct PanelTestProbe(Option<Arc<PanelRenderer>>);

#[cfg(test)]
impl PanelTestProbe {
    #[cfg(all(windows, target_arch = "x86_64"))]
    pub(crate) fn existing(context: &egui::Context) -> Self {
        Self(context.data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id())))
    }

    pub(crate) fn existing_paints(context: &egui::Context) -> Option<usize> {
        context.data(|data| {
            data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id())
                .map(|renderer| renderer.paints.load(std::sync::atomic::Ordering::Relaxed))
        })
    }

    pub(crate) fn install(context: &egui::Context, state: &egui_wgpu::RenderState) -> Self {
        // Exercise geometry on CI adapters too; production install retains its adapter guards.
        let renderer = PanelRenderer::new(state, egui_wgpu::RendererOptions::default().dithering);
        if let Some(renderer) = &renderer {
            context.data_mut(|data| data.insert_temp(panel_renderer_id(), Arc::clone(renderer)));
        }
        Self(renderer)
    }

    pub(crate) fn paints(&self) -> usize {
        self.0.as_ref().map_or(0, |renderer| {
            renderer.paints.load(std::sync::atomic::Ordering::Relaxed)
        })
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    pub(crate) fn white_mesh_shape(
        &self,
        viewport: egui::Rect,
        mesh: &egui::Mesh,
    ) -> Result<egui::Shape, &'static str> {
        if !white_mesh_geometry(mesh) {
            return Err("textureless diagnostic requires valid, nonempty white-UV geometry");
        }
        self.0
            .as_ref()
            .ok_or("textureless diagnostic requires a supported framebuffer format")?
            .mesh_shape(viewport, mesh.clone())
            .ok_or("textureless diagnostic geometry exceeded the index budget")
    }
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
    paint_namespace: Arc<()>,
}

impl egui_wgpu::CallbackTrait for SolidBackground {
    fn paint_key(&self) -> Option<egui_wgpu::CallbackPaintKey> {
        Some(egui_wgpu::CallbackPaintKey {
            namespace: self.paint_namespace.clone(),
            bytes: Arc::from([]),
        })
    }

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
        SolidBackground {
            pipeline,
            paint_namespace: Arc::new(()),
        },
    ))
}

pub(crate) fn install(context: &egui::Context, render_state: &egui_wgpu::RenderState) {
    context.data_mut(|data| data.remove::<Arc<PanelRenderer>>(panel_renderer_id()));
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
                let fills = Arc::clone(&renderer);
                festerm_ui_egui::install_panel_fill_callback(context, move |ui, rect, fill| {
                    (fill.is_opaque() && supports_panel_painter(ui))
                        .then(|| fills.shape(ui, egui::Shape::rect_filled(rect, 0.0, fill)))
                });
                context.data_mut(|data| data.insert_temp(panel_renderer_id(), renderer));
                context.add_plugin(PaletteFrameBackground);
                tracing::info!(target: "festerm::app", "using textureless application panel backgrounds on Windows WARP");
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
    fn owned_picker_frame_selects_only_one_complete_new_modal_frame_at_both_widths() {
        for name in ["markdown_file_picker", "text_editor_save_as"] {
            for size in [egui::vec2(752.0, 516.0), egui::vec2(360.0, 240.0)] {
                let context = egui::Context::default();
                context.set_visuals(festerm_ui_egui::theme::default_visuals());
                context.all_styles_mut(|style| style.animation_time = 0.0);
                let id = egui::Id::new(name);
                let layer = egui::Modal::default_area(id).layer();
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                };
                for pass in 0..5 {
                    let mut output = context.run_ui(input.clone(), |_| {
                        let frame = egui::Frame::popup(&context.global_style())
                            .inner_margin(egui::Margin::same(14));
                        let painter = context.layer_painter(layer);
                        // A preceding same-layer frame is not owned by this invocation.
                        painter.add(frame.paint(egui::Rect::from_min_size(
                            egui::pos2(20.0, 20.0),
                            egui::vec2(60.0, 40.0),
                        )));
                        let start =
                            context.graphics(|graphics| graphics.get(layer).unwrap().next_idx());
                        let response =
                            show_picker_modal(&context, egui::Modal::new(id).frame(frame), |ui| {
                                ui.set_min_size(egui::vec2(size.x.min(600.0) - 60.0, 100.0));
                                ui.label(name);
                                ui.button("Choose").rect
                            });
                        if pass < 4 {
                            return;
                        }
                        assert!(context
                            .viewport_rect()
                            .contains_rect(response.response.rect));
                        let owned_rect = response.response.rect;
                        let (index, original) =
                            picker_frame_background(&context, layer, start, owned_rect)
                                .expect("the actual normal/narrow Modal emits one complete frame");
                        assert!(index.0 >= start.0);
                        assert!(complete_opaque_window_frame(
                            &original,
                            context.viewport_rect()
                        ));
                        let duplicate = painter.add(original.shape.clone());
                        assert!(
                            picker_frame_background(&context, layer, start, owned_rect).is_none()
                        );
                        painter.set(duplicate, egui::Shape::Noop);
                        context.graphics_mut(|graphics| {
                            graphics.get_mut(layer).unwrap().set(
                                index,
                                egui::Rect::from_min_size(
                                    response.response.rect.min,
                                    egui::vec2(1.0, 1.0),
                                ),
                                original.shape.clone(),
                            );
                        });
                        assert!(
                            picker_frame_background(&context, layer, start, owned_rect).is_none()
                        );
                        context.graphics_mut(|graphics| {
                            graphics.get_mut(layer).unwrap().set(
                                index,
                                original.clip_rect,
                                original.shape,
                            );
                        });
                        assert!(
                            picker_frame_background(&context, layer, start, owned_rect).is_some()
                        );
                        assert!(picker_frame_background(
                            &context,
                            layer,
                            start,
                            owned_rect.shrink(1.0)
                        )
                        .is_none());
                    });
                    output.textures_delta.clear();
                }
            }
        }
    }

    #[test]
    fn owned_picker_frame_diagnostic_control_leaves_backdrop_independent() {
        let context = egui::Context::default();
        assert!(picker_frames_enabled(&context));
        assert!(picker_backdrops_enabled(&context));
        set_picker_frames_enabled(&context, false);
        assert!(!picker_frames_enabled(&context));
        assert!(picker_backdrops_enabled(&context));
        // The legacy backdrop attribution selects both original shapes.
        context.data_mut(|data| data.insert_temp(picker_backdrops_disabled_id(), true));
        assert!(!picker_frames_enabled(&context));
        assert!(!picker_backdrops_enabled(&context));
        set_picker_frames_enabled(&context, true);
        assert!(picker_frames_enabled(&context));
        assert!(!picker_backdrops_enabled(&context));
        assert!(picker_frames_enabled(&egui::Context::default()));
    }

    #[test]
    fn owned_picker_frame_no_renderer_keeps_live_modal_shapes_input_and_responses() {
        let draw = |routed: bool, name: &str, case: &str, gesture: &str| {
            let context = egui::Context::default();
            context.set_visuals(festerm_ui_egui::theme::default_visuals());
            context.all_styles_mut(|style| style.animation_time = 0.0);
            let id = egui::Id::new(name);
            let mut frame = egui::Frame::popup(&context.global_style());
            if case == "shadowless" {
                frame.shadow = egui::Shadow::NONE;
            } else if case == "translucent" {
                frame.fill = egui::Color32::from_black_alpha(120);
            }
            let step = |events| {
                let mut observed = None;
                let mut output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(360.0, 240.0),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        ui.button("Underlying action")
                            .on_hover_text("Not the picker");
                        let modal = egui::Modal::new(id).frame(frame);
                        let contents = |ui: &mut egui::Ui| {
                            ui.label("Live picker content");
                            let button = ui.button("Choose");
                            (
                                button.id,
                                button.rect,
                                button.sense,
                                button.enabled(),
                                button.clicked(),
                            )
                        };
                        let response = if routed {
                            show_picker_modal(&context, modal, contents)
                        } else {
                            modal.show(&context, contents)
                        };
                        observed = Some((
                            response.response.id,
                            response.response.rect,
                            response.backdrop_response.id,
                            response.backdrop_response.rect,
                            response.backdrop_response.sense,
                            response.backdrop_response.clicked(),
                            response.is_top_modal,
                            response.any_popup_open,
                            response.inner,
                            response.should_close(),
                        ));
                    },
                );
                output.textures_delta.clear();
                (output, observed.unwrap())
            };
            let mut result = step(Vec::new());
            for _ in 0..4 {
                result = step(Vec::new());
            }
            if gesture == "escape" {
                result = step(vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]);
            } else {
                let pos = if gesture == "choose" {
                    result.1 .8 .1.center()
                } else {
                    egui::pos2(350.0, 230.0)
                };
                let pointer = |pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                step(vec![egui::Event::PointerMoved(pos), pointer(true)]);
                result = step(vec![pointer(false)]);
            }
            assert_eq!(result.1 .8 .4, gesture == "choose");
            assert_eq!(result.1 .5, gesture == "backdrop");
            assert_eq!(result.1 .9, gesture != "choose");
            (result.0.shapes, result.1)
        };
        for name in ["markdown_file_picker", "text_editor_save_as"] {
            for case in ["normal", "shadowless", "translucent"] {
                for gesture in ["choose", "backdrop", "escape"] {
                    assert_eq!(
                        draw(true, name, case, gesture),
                        draw(false, name, case, gesture),
                        "{name}, {case}, {gesture}",
                    );
                }
            }
        }
    }

    #[test]
    fn palette_frame_plugin_reinstallation_uses_current_renderer_and_declines_missing_renderer() {
        let state = create_render_state(default_wgpu_setup(), Default::default());
        let context = egui::Context::default();
        context.set_visuals(festerm_ui_egui::theme::default_visuals());
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let old = PanelRenderer::new(&state, true).unwrap();
        let current = PanelRenderer::new(&state, true).unwrap();
        assert!(!Arc::ptr_eq(&old.paint_namespace, &current.paint_namespace));
        context.add_plugin(PaletteFrameBackground);
        let mut palette = festerm_ui_egui::palette::PaletteState::default();
        palette.open();
        let items = [festerm_ui_egui::palette::PaletteItem {
            id: 1,
            label: "Settings".into(),
            hint: None,
            is_tab: false,
            shortcut_label: None,
        }];
        let mut draw = || {
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 480.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    let _ = festerm_ui_egui::palette::show(ui.ctx(), &mut palette, &items);
                },
            );
            output.textures_delta.clear();
        };
        context.data_mut(|data| data.insert_temp(panel_renderer_id(), Arc::clone(&old)));
        for _ in 0..4 {
            draw();
        }
        let old_count = old
            .palette_frames
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(old_count > 0);
        let old_fills = old.palette_fills.load(std::sync::atomic::Ordering::Relaxed);
        assert!(old_fills > 0);
        context.data_mut(|data| data.insert_temp(panel_renderer_id(), Arc::clone(&current)));
        context.add_plugin(PaletteFrameBackground);
        draw();
        assert_eq!(
            old.palette_frames
                .load(std::sync::atomic::Ordering::Relaxed),
            old_count
        );
        assert_eq!(
            old.palette_fills.load(std::sync::atomic::Ordering::Relaxed),
            old_fills
        );
        let current_count = current
            .palette_frames
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(current_count > 0);
        let current_fills = current
            .palette_fills
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(current_fills > 0);
        let mut unsupported = state.clone();
        unsupported.target_format = wgpu::TextureFormat::Bgra8UnormSrgb;
        install(&context, &unsupported);
        assert!(context
            .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
            .is_none());
        draw();
        assert_eq!(
            current
                .palette_frames
                .load(std::sync::atomic::Ordering::Relaxed),
            current_count
        );
        assert_eq!(
            current
                .palette_fills
                .load(std::sync::atomic::Ordering::Relaxed),
            current_fills
        );
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn picker_modal_preserves_pixels_responses_and_guarded_fallbacks() {
        use egui_kittest::TestRenderer;
        let render = |enabled: bool, scale: f32, case: &str| {
            let mut setup = default_wgpu_setup();
            let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
                unreachable!()
            };
            options.instance_descriptor.backends = wgpu::Backends::DX12;
            let state = create_render_state(setup, Default::default());
            assert_eq!(state.adapter.get_info().backend, wgpu::Backend::Dx12);
            assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
            let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
            let context = egui::Context::default();
            context.set_visuals(if case == "light" {
                egui::Visuals::light()
            } else {
                festerm_ui_egui::theme::default_visuals()
            });
            context.all_styles_mut(|style| style.animation_time = 0.0);
            if case != "no-renderer" {
                install(&context, &state);
            }
            set_picker_backdrops_enabled(&context, enabled);
            set_picker_frames_enabled(&context, false);
            let id = egui::Id::new("guarded-picker-modal");
            if case == "transform" {
                context.set_transform_layer(
                    egui::Modal::default_area(id).layer(),
                    egui::emath::TSTransform::from_translation(egui::vec2(1.25, 2.5)),
                );
            }
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    if case == "origin" {
                        egui::pos2(1.0, 2.0)
                    } else {
                        egui::Pos2::ZERO
                    },
                    egui::vec2(224.0, 144.0),
                )),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let color = if case == "colored" {
                egui::Color32::from_rgba_unmultiplied(31, 43, 61, 100)
            } else {
                egui::Color32::from_black_alpha(100)
            };
            let mut observed = None;
            let mut frame = egui::FullOutput::default();
            for _ in 0..5 {
                frame = context.run_ui(input.clone(), |ui| {
                    ui.label("Underlying content stays live");
                    let response = show_picker_modal(
                        &context,
                        egui::Modal::new(id).backdrop_color(color),
                        |ui| {
                            ui.label("Actual picker content");
                            let button = ui.button("Choose");
                            (button.id, button.rect, button.sense, button.enabled())
                        },
                    );
                    observed = Some((
                        response.response.id,
                        response.response.rect,
                        response.backdrop_response.id,
                        response.backdrop_response.rect,
                        response.backdrop_response.sense,
                        response.is_top_modal,
                        response.any_popup_open,
                        response.inner,
                    ));
                });
                renderer.handle_delta(&mut frame.textures_delta);
            }
            let before = PanelTestProbe::existing_paints(&context).unwrap_or(0);
            let image = renderer.render(&context, &frame).unwrap();
            let paints = PanelTestProbe::existing_paints(&context).unwrap_or(0) - before;
            (image, observed.unwrap(), paints)
        };
        for scale in [1.0, 1.25] {
            for case in [
                "normal",
                "light",
                "origin",
                "transform",
                "colored",
                "no-renderer",
            ] {
                let (ordinary, expected, ordinary_paints) = render(false, scale, case);
                let (converted, actual, paints) = render(true, scale, case);
                assert_eq!(converted, ordinary, "{case}, scale {scale}");
                assert_eq!(actual, expected, "{case}, scale {scale}");
                assert_eq!(ordinary_paints, 0);
                assert_eq!(paints, usize::from(matches!(case, "normal" | "light")));
            }
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn owned_picker_frame_preserves_pixels_responses_and_guarded_fallbacks() {
        use egui_kittest::TestRenderer;
        let render = |enabled: bool, scale: f32, case: &str| {
            let mut setup = default_wgpu_setup();
            let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
                unreachable!()
            };
            options.instance_descriptor.backends = wgpu::Backends::DX12;
            let state = create_render_state(setup, Default::default());
            assert_eq!(state.adapter.get_info().backend, wgpu::Backend::Dx12);
            assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
            let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
            let context = egui::Context::default();
            context.set_visuals(if case == "light" {
                egui::Visuals::light()
            } else {
                festerm_ui_egui::theme::default_visuals()
            });
            context.all_styles_mut(|style| style.animation_time = 0.0);
            if case != "no-renderer" {
                install(&context, &state);
            }
            set_picker_frames_enabled(&context, enabled);
            let id = egui::Id::new("text_editor_save_as");
            if case == "transform" {
                context.set_transform_layer(
                    egui::Modal::default_area(id).layer(),
                    egui::emath::TSTransform::from_translation(egui::vec2(1.25, 2.5)),
                );
            }
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    if case == "origin" {
                        egui::pos2(1.0, 2.0)
                    } else {
                        egui::Pos2::ZERO
                    },
                    if case == "normal" {
                        egui::vec2(513.0, 401.0)
                    } else {
                        egui::vec2(224.0, 144.0)
                    },
                )),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let mut popup = egui::Frame::popup(&context.global_style());
            match case {
                "shadowless" | "shadowless-with-child" => popup.shadow = egui::Shadow::NONE,
                "translucent-frame" => popup.fill = egui::Color32::from_black_alpha(120),
                "colored-shadow" => {
                    popup.shadow.color = egui::Color32::from_rgba_unmultiplied(31, 43, 61, 100)
                }
                _ => {}
            }
            let backdrop = if case == "colored-backdrop" {
                egui::Color32::from_rgba_unmultiplied(31, 43, 61, 100)
            } else {
                egui::Color32::from_black_alpha(100)
            };
            let mut observed = None;
            let mut output = egui::FullOutput::default();
            for _ in 0..5 {
                output = context.run_ui(input.clone(), |ui| {
                    ui.label("Underlying content stays live");
                    let contents = |ui: &mut egui::Ui| {
                        ui.set_min_size(egui::vec2(
                            if case == "outside-frame" {
                                400.0
                            } else {
                                100.0
                            },
                            50.0,
                        ));
                        if matches!(case, "ambiguous-frame" | "shadowless-with-child") {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.label("Another complete frame");
                            });
                        }
                        ui.label("Live picker content");
                        let button = ui.button("Choose");
                        (button.id, button.rect, button.sense, button.enabled())
                    };
                    let modal = egui::Modal::new(id).frame(popup).backdrop_color(backdrop);
                    let response = if case == "other-modal" {
                        modal.show(&context, contents)
                    } else {
                        show_picker_modal(&context, modal, contents)
                    };
                    observed = Some((
                        response.response.id,
                        response.response.rect,
                        response.backdrop_response.id,
                        response.backdrop_response.rect,
                        response.backdrop_response.sense,
                        response.is_top_modal,
                        response.any_popup_open,
                        response.inner,
                    ));
                });
                renderer.handle_delta(&mut output.textures_delta);
            }
            let before = PanelTestProbe::existing_paints(&context).unwrap_or(0);
            let image = renderer.render(&context, &output).unwrap();
            let paints = PanelTestProbe::existing_paints(&context).unwrap_or(0) - before;
            (image, observed.unwrap(), paints)
        };
        for scale in [1.0, 1.25] {
            for case in [
                "normal",
                "narrow",
                "light",
                "shadowless",
                "shadowless-with-child",
                "translucent-frame",
                "colored-shadow",
                "colored-backdrop",
                "outside-frame",
                "ambiguous-frame",
                "no-renderer",
                "origin",
                "transform",
                "other-modal",
            ] {
                let (ordinary, expected, ordinary_paints) = render(false, scale, case);
                let (converted, actual, paints) = render(true, scale, case);
                assert_eq!(converted, ordinary, "{case}, scale {scale}");
                assert_eq!(actual, expected, "{case}, scale {scale}");
                assert_eq!(
                    ordinary_paints,
                    usize::from(!matches!(
                        case,
                        "colored-backdrop" | "no-renderer" | "origin" | "transform" | "other-modal"
                    )),
                    "the ordinary frame keeps the existing default backdrop",
                );
                assert_eq!(
                    paints,
                    ordinary_paints
                        + usize::from(matches!(
                            case,
                            "normal" | "narrow" | "light" | "colored-backdrop"
                        )),
                    "{case}, scale {scale}",
                );
            }
        }
    }

    #[test]
    fn palette_frame_candidates_are_only_the_opaque_untextured_shadow_frame() {
        let rect = egui::Rect::from_min_size(egui::pos2(11.0, 17.0), egui::vec2(210.0, 130.0));
        let frame = egui::Frame::window(&egui::Style::default());
        assert_ne!(frame.shadow, egui::Shadow::NONE);
        assert!(opaque_window_frame(&frame.paint(rect)));
        assert!(!opaque_window_frame(
            &frame.fill(egui::Color32::from_black_alpha(120)).paint(rect)
        ));
        assert!(!opaque_window_frame(&egui::Shape::rect_filled(
            rect,
            5.0,
            egui::Color32::WHITE,
        )));
        assert!(!opaque_window_frame(&egui::Shape::Vec(vec![
            egui::Shape::Noop,
            egui::Shape::Noop,
        ])));
    }

    #[test]
    fn palette_fills_accept_only_opaque_untextured_rectangles() {
        let rect = egui::Rect::from_min_size(egui::pos2(11.0, 17.0), egui::vec2(210.0, 30.0));
        assert!(opaque_palette_fill(&egui::Shape::rect_filled(
            rect,
            3.0,
            egui::Color32::from_rgb(31, 43, 61),
        )));
        assert!(!opaque_palette_fill(&egui::Shape::rect_filled(
            rect,
            3.0,
            egui::Color32::from_black_alpha(160),
        )));
        assert!(!opaque_palette_fill(&egui::Shape::Noop));
        assert!(!opaque_palette_fill(
            &egui::Frame::window(&egui::Style::default()).paint(rect)
        ));
        let mut textured = egui::epaint::RectShape::filled(rect, 3.0, egui::Color32::WHITE);
        textured.brush = Some(Arc::new(egui::epaint::Brush {
            fill_texture_id: egui::TextureId::Managed(1),
            uv: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        }));
        assert!(!opaque_palette_fill(&egui::Shape::Rect(textured)));
    }

    #[test]
    fn palette_frame_retains_root_opacity_visibility_origin_and_layer_transform_guards() {
        for case in [
            "eligible",
            "opacity",
            "invisible",
            "origin",
            "root-transform",
            "palette-transform",
            "secondary",
        ] {
            let context = egui::Context::default();
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    if case == "origin" {
                        egui::pos2(1.25, 2.5)
                    } else {
                        egui::Pos2::ZERO
                    },
                    egui::vec2(360.0, 240.0),
                )),
                ..Default::default()
            };
            if case == "secondary" {
                input.viewport_id = egui::ViewportId::from_hash_of("secondary-palette-frame");
                input
                    .viewports
                    .insert(input.viewport_id, Default::default());
            }
            let mut output = context.run_ui(input, |ui| {
                match case {
                    "opacity" => ui.set_opacity(0.5),
                    "invisible" => ui.set_invisible(),
                    "root-transform" => context.set_transform_layer(
                        ui.layer_id(),
                        egui::emath::TSTransform::from_translation(egui::vec2(1.0, 2.0)),
                    ),
                    "palette-transform" => context.set_transform_layer(
                        palette_frame_layer(),
                        egui::emath::TSTransform::from_translation(egui::vec2(1.0, 2.0)),
                    ),
                    _ => {}
                }
                assert_eq!(supports_palette_frame(ui), case == "eligible", "{case}");
            });
            output.textures_delta.clear();
        }
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn textureless_palette_frame_preserves_shadow_pixels_across_dpi_and_fallback() {
        use egui_kittest::TestRenderer;
        let render = |enabled: bool, scale: f32, case: &str| {
            let mut setup = default_wgpu_setup();
            let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
                unreachable!()
            };
            options.instance_descriptor.backends = wgpu::Backends::DX12;
            let state = create_render_state(setup, Default::default());
            assert_eq!(state.adapter.get_info().device_type, wgpu::DeviceType::Cpu);
            let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
            let context = egui::Context::default();
            context.set_visuals(if case == "light" {
                egui::Visuals::light()
            } else {
                festerm_ui_egui::theme::default_visuals()
            });
            context.all_styles_mut(|style| style.animation_time = 0.0);
            if case == "translucent-frame" {
                context.all_styles_mut(|style| {
                    style.visuals.window_fill = egui::Color32::from_black_alpha(160)
                });
            }
            install(&context, &state);
            set_palette_frame_enabled(&context, enabled);
            if case == "palette-transform" {
                context.set_transform_layer(
                    palette_frame_layer(),
                    egui::emath::TSTransform::from_translation(egui::vec2(1.25, 2.5)),
                );
            }
            let mut palette = festerm_ui_egui::palette::PaletteState::default();
            palette.open();
            let items = [
                festerm_ui_egui::palette::PaletteItem {
                    id: 1,
                    label: "Local fixture".into(),
                    hint: Some("Active session".into()),
                    is_tab: true,
                    shortcut_label: Some("Ctrl+1".into()),
                },
                festerm_ui_egui::palette::PaletteItem {
                    id: 2,
                    label: "Open Settings".into(),
                    hint: Some("Ctrl+Shift+S".into()),
                    is_tab: false,
                    shortcut_label: None,
                },
            ];
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    if case == "origin" {
                        egui::pos2(1.25, 2.5)
                    } else {
                        egui::Pos2::ZERO
                    },
                    if case == "short" {
                        egui::vec2(360.0, 240.0)
                    } else {
                        egui::vec2(513.0, 401.0)
                    },
                )),
                time: Some(0.0),
                max_texture_side: Some(
                    usize::try_from(state.device.limits().max_texture_dimension_2d).unwrap(),
                ),
                ..Default::default()
            };
            if case == "secondary" {
                input.viewport_id = egui::ViewportId::from_hash_of("secondary-palette-pixels");
                input
                    .viewports
                    .insert(input.viewport_id, Default::default());
            }
            input
                .viewports
                .get_mut(&input.viewport_id)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            if case == "secondary" {
                // The offscreen renderer queries the root after the secondary pass.
                let mut root = input.clone();
                root.viewport_id = egui::ViewportId::ROOT;
                root.viewports
                    .entry(egui::ViewportId::ROOT)
                    .or_default()
                    .native_pixels_per_point = Some(scale);
                let mut output = context.run_ui(root, |_| {});
                renderer.handle_delta(&mut output.textures_delta);
            }
            let mut output = None;
            for frame in 0..8 {
                let mut raw = input.clone();
                if case == "query" && frame == 3 {
                    raw.events.push(egui::Event::Text("Settings".into()));
                }
                let mut current = context.run_ui(raw, |ui| {
                    if case == "opacity" {
                        ui.set_opacity(0.5);
                    }
                    ui.painter().rect_filled(
                        ui.max_rect(),
                        0.0,
                        egui::Color32::from_rgb(31, 43, 61),
                    );
                    assert!(
                        festerm_ui_egui::palette::show(ui.ctx(), &mut palette, &items).is_none()
                    );
                });
                renderer.handle_delta(&mut current.textures_delta);
                output = Some(current);
            }
            let image = renderer.render(&context, &output.unwrap()).unwrap();
            let panel = context
                .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
                .unwrap();
            assert_eq!(
                panel
                    .palette_frames
                    .load(std::sync::atomic::Ordering::Relaxed)
                    > 0,
                enabled && matches!(case, "dark" | "light" | "short" | "query"),
                "the pixel comparison must exercise the actual selected path: {case}, {scale}",
            );
            assert_eq!(
                panel
                    .palette_fills
                    .load(std::sync::atomic::Ordering::Relaxed)
                    > 0,
                enabled && matches!(case, "dark" | "light" | "short" | "query"),
                "the pixel comparison must exercise selected/search fills: {case}, {scale}",
            );
            image
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for case in ["dark", "light", "short", "query"] {
                let ordinary = render(false, scale, case);
                let actual = render(true, scale, case);
                assert_eq!(ordinary.dimensions(), actual.dimensions());
                assert_eq!(
                    ordinary
                        .pixels()
                        .zip(actual.pixels())
                        .filter(|(a, b)| a != b)
                        .count(),
                    0,
                    "scale={scale}, case={case}",
                );
            }
        }
        for case in [
            "opacity",
            "origin",
            "palette-transform",
            "secondary",
            "translucent-frame",
        ] {
            let ordinary = render(false, 1.25, case);
            let actual = render(true, 1.25, case);
            assert_eq!(ordinary.dimensions(), actual.dimensions());
            assert_eq!(
                ordinary
                    .pixels()
                    .zip(actual.pixels())
                    .filter(|(a, b)| a != b)
                    .count(),
                0,
                "{case}"
            );
        }
    }

    #[test]
    fn white_mesh_geometry_preserves_alpha_and_rejects_textures_or_invalid_indices() {
        let rect = egui::Rect::from_min_size(egui::pos2(2.5, 3.75), egui::vec2(18.0, 27.0));
        let mut mesh = egui::Mesh::default();
        mesh.add_colored_rect(rect, egui::Color32::LIGHT_BLUE);
        mesh.add_colored_rect(
            rect.translate(egui::vec2(4.0, 6.0)),
            egui::Color32::from_black_alpha(64),
        );
        assert!(white_mesh_geometry(&mesh));
        assert!(!white_mesh_geometry(&egui::Mesh::default()));
        for invalid in ["texture", "uv", "indices", "empty"] {
            let mut candidate = mesh.clone();
            match invalid {
                "texture" => candidate.texture_id = egui::TextureId::User(7),
                "uv" => candidate.vertices[0].uv = egui::pos2(0.25, 0.75),
                "indices" => candidate.indices[0] = candidate.vertices.len() as u32,
                "empty" => candidate.indices.clear(),
                _ => unreachable!(),
            }
            assert!(!white_mesh_geometry(&candidate), "{invalid}");
        }
    }

    #[test]
    fn panel_frames_retain_all_painter_and_frame_guards() {
        for case in [
            "eligible",
            "translucent-fill",
            "shadow",
            "opacity",
            "invisible",
            "transformed",
            "nonzero-origin",
            "secondary",
        ] {
            let context = egui::Context::default();
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    if case == "nonzero-origin" {
                        egui::pos2(1.25, 2.5)
                    } else {
                        egui::Pos2::ZERO
                    },
                    egui::vec2(320.0, 240.0),
                )),
                ..Default::default()
            };
            if case == "secondary" {
                input.viewport_id = egui::ViewportId::from_hash_of("secondary-panel-test");
                input
                    .viewports
                    .insert(input.viewport_id, Default::default());
            }
            let mut output = context.run_ui(input, |ui| {
                let mut frame = egui::Frame::new().fill(egui::Color32::DARK_BLUE);
                match case {
                    "translucent-fill" => frame.fill = egui::Color32::from_black_alpha(128),
                    "shadow" => {
                        frame.shadow = egui::Shadow {
                            offset: [1, 2],
                            blur: 4,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(64),
                        };
                    }
                    "opacity" => ui.set_opacity(0.5),
                    "invisible" => ui.set_invisible(),
                    "transformed" => context.set_transform_layer(
                        ui.layer_id(),
                        egui::emath::TSTransform::from_translation(egui::vec2(1.0, 0.0)),
                    ),
                    _ => {}
                }
                assert_eq!(
                    supports_panel_frame(ui, &frame),
                    case == "eligible",
                    "{case}"
                );
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn panel_frame_without_renderer_preserves_layout_and_child_order() {
        let draw = |shared| {
            let context = egui::Context::default();
            let mut geometry = None;
            let mut output = context.run_ui(Default::default(), |ui| {
                let frame = egui::Frame::new()
                    .fill(egui::Color32::DARK_BLUE)
                    .stroke(egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE))
                    .corner_radius(9)
                    .inner_margin(egui::Margin::same(7));
                let contents = |ui: &mut egui::Ui| ui.button("Unchanged child");
                let response = if shared {
                    show_frame(ui, frame, contents)
                } else {
                    frame.show(ui, contents)
                };
                geometry = Some((
                    response.response.rect,
                    response.inner.rect,
                    response.inner.id,
                ));
            });
            output.textures_delta.clear();
            let fill = output.shapes.iter().position(|shape| {
                matches!(&shape.shape, egui::Shape::Rect(rect) if rect.fill == egui::Color32::DARK_BLUE)
            }).expect("ordinary frame fill");
            let text = output
                .shapes
                .iter()
                .position(|shape| matches!(&shape.shape, egui::Shape::Text(_)))
                .expect("child text");
            assert!(fill < text);
            geometry.unwrap()
        };
        assert_eq!(draw(false), draw(true));
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn textureless_application_chrome_preserves_pixels_across_dpi_and_clipping() {
        use egui_kittest::TestRenderer;
        let render = |native: bool, scale: f32, opacity: f32, clipped: bool| {
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
            if native {
                install(&context, &state);
            }
            let (mut app, _, _) = crate::app::FesTermApp::for_test_with_fake_ssh_session([]);
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(513.0, 401.0),
                )),
                time: Some(0.0),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let mut output = None;
            for _ in 0..5 {
                let mut frame = context.run_ui(input.clone(), |ui| {
                    ui.set_opacity(opacity);
                    if clipped {
                        ui.set_clip_rect(egui::Rect::from_min_max(
                            egui::pos2(11.25, 13.5),
                            egui::pos2(501.5, 389.75),
                        ));
                    }
                    app.frame_logic(ui.ctx());
                    app.ui_content(ui);
                });
                renderer.handle_delta(&mut frame.textures_delta);
                output = Some(frame);
            }
            let image = renderer.render(&context, &output.unwrap()).unwrap();
            if native {
                let panel = context
                    .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()))
                    .unwrap();
                let paints = panel.paints.load(std::sync::atomic::Ordering::Relaxed);
                if opacity == 1.0 {
                    assert!(paints >= 2);
                } else {
                    assert_eq!(paints, 0);
                }
            }
            image
        };
        for scale in [1.0, 1.25, 2.0] {
            for opacity in [1.0, 0.5] {
                for clipped in [false, true] {
                    let reference = render(false, scale, opacity, clipped);
                    let actual = render(true, scale, opacity, clipped);
                    assert_eq!(reference.dimensions(), actual.dimensions());
                    assert_eq!(
                        reference
                            .pixels()
                            .zip(actual.pixels())
                            .filter(|(a, b)| a != b)
                            .count(),
                        0,
                        "scale={scale}, opacity={opacity}, clipped={clipped}",
                    );
                }
            }
        }
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
                opacity == 1.0,
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
    fn textureless_bordered_panels_match_pixels_across_dpi() {
        for scale in [1.0, 1.25, 2.0] {
            for clipped in [false, true] {
                for dithering in [false, true] {
                    let ordinary = panel_image(false, scale, 1.0, clipped, true, false, dithering);
                    let native = panel_image(true, scale, 1.0, clipped, true, false, dithering);
                    assert_eq!(
                        ordinary, native,
                        "scale={scale}, clipped={clipped}, dithering={dithering}"
                    );
                }
            }
        }
    }

    #[test]
    fn textureless_panel_fill_preserves_opacity_and_srgb_fallback() {
        for (opacity, bordered, srgb) in [
            (0.5, false, false),
            (0.5, true, false),
            (1.0, false, true),
            (1.0, true, true),
        ] {
            let ordinary = panel_image(false, 1.25, opacity, true, bordered, srgb, true);
            let native = panel_image(true, 1.25, opacity, true, bordered, srgb, true);
            assert_eq!(ordinary, native);
        }
    }

    #[test]
    fn textureless_frame_fallback_preserves_pixels_for_shadow_and_viewports() {
        use egui_kittest::TestRenderer;

        for case in [
            "translucent-fill",
            "shadow",
            "opacity",
            "invisible",
            "transformed",
            "nonzero-origin",
            "secondary",
            "srgb",
        ] {
            let render = |native: bool| {
                let mut state = create_render_state(default_wgpu_setup(), Default::default());
                if case == "srgb" {
                    state.target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
                    *state.renderer.write() = egui_wgpu::Renderer::new(
                        &state.device,
                        state.target_format,
                        Default::default(),
                    );
                }
                let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
                let context = egui::Context::default();
                context.set_theme(egui::ThemePreference::Dark);
                context.set_visuals(festerm_ui_egui::theme::default_visuals());
                let probe = native.then(|| PanelTestProbe::install(&context, &state));
                let mut input = egui::RawInput {
                    max_texture_side: Some(state.device.limits().max_texture_dimension_2d as usize),
                    screen_rect: Some(egui::Rect::from_min_size(
                        if case == "nonzero-origin" {
                            egui::pos2(1.25, 2.5)
                        } else {
                            egui::Pos2::ZERO
                        },
                        egui::vec2(257.0, 157.0),
                    )),
                    ..Default::default()
                };
                if case == "secondary" {
                    input
                        .viewports
                        .get_mut(&egui::ViewportId::ROOT)
                        .unwrap()
                        .native_pixels_per_point = Some(1.25);
                    // Readback uses the root screen descriptor outside a viewport pass.
                    let mut root = context.run_ui(input.clone(), |_ui| {});
                    renderer.handle_delta(&mut root.textures_delta);
                    input.viewport_id =
                        egui::ViewportId::from_hash_of("secondary-panel-pixel-test");
                    input
                        .viewports
                        .insert(input.viewport_id, Default::default());
                }
                input
                    .viewports
                    .get_mut(&input.viewport_id)
                    .unwrap()
                    .native_pixels_per_point = Some(1.25);
                let mut output = None;
                for _ in 0..5 {
                    let mut frame = context.run_ui(input.clone(), |ui| {
                        ui.set_clip_rect(egui::Rect::from_min_max(
                            egui::pos2(11.25, 13.5),
                            egui::pos2(241.75, 140.25),
                        ));
                        match case {
                            "opacity" => ui.set_opacity(0.5),
                            "invisible" => ui.set_invisible(),
                            "transformed" => context.set_transform_layer(
                                ui.layer_id(),
                                egui::emath::TSTransform::from_translation(egui::vec2(1.25, 2.5)),
                            ),
                            _ => {}
                        }
                        let mut frame = egui::Frame::new()
                            .fill(egui::Color32::DARK_BLUE)
                            .stroke(egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE))
                            .corner_radius(9)
                            .inner_margin(egui::Margin::same(7));
                        if case == "translucent-fill" {
                            frame.fill = egui::Color32::from_black_alpha(128);
                        } else if case == "shadow" {
                            frame.shadow = egui::Shadow {
                                offset: [1, 2],
                                blur: 4,
                                spread: 0,
                                color: egui::Color32::from_black_alpha(64),
                            };
                        }
                        let contents = |ui: &mut egui::Ui| {
                            ui.set_min_size(egui::vec2(200.0, 100.0));
                            ui.label("Fallback keeps the full widget");
                        };
                        if native {
                            show_frame(ui, frame, contents);
                        } else {
                            frame.show(ui, contents);
                        }
                    });
                    renderer.handle_delta(&mut frame.textures_delta);
                    output = Some(frame);
                }
                let image = renderer
                    .render(&context, &output.unwrap())
                    .expect("fallback framebuffer");
                if let Some(probe) = probe {
                    assert_eq!(probe.paints(), 0, "{case} must stay on ordinary painting");
                }
                image
            };
            assert_eq!(render(false), render(true), "{case}");
        }
    }

    #[test]
    fn unexpected_panel_geometry_retains_the_original_mesh() {
        let state = create_render_state(default_wgpu_setup(), Default::default());
        let renderer = PanelRenderer::new(&state, true).unwrap();
        let context = egui::Context::default();
        let mut output = context.run_ui(Default::default(), |ui| {
            let mut mesh = egui::Mesh::with_texture(egui::TextureId::User(17));
            mesh.add_rect_with_uv(
                egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(40.0, 30.0)),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            let original = Arc::new(mesh);
            let shape = renderer.shape(ui, egui::Shape::Mesh(Arc::clone(&original)));
            let egui::Shape::Mesh(retained) = shape else {
                panic!("unexpected geometry must not become a callback or blank shape");
            };
            assert!(Arc::ptr_eq(&original, &retained));
        });
        output.textures_delta.clear();
    }

    fn replay_sha256(mut input: impl std::io::Read) -> String {
        use sha2::{Digest, Sha256};

        let mut hash = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .expect("read replay provenance bytes");
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        hash.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn shared_panel_replay_hash_is_standard_sha256() {
        assert_eq!(
            replay_sha256(&b"abc"[..]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    #[test]
    #[ignore = "optional completed Windows WARP Inspector/SFTP replay; not native latency evidence"]
    fn replay_warp_ui_surfaces_shared_panels() {
        use egui_kittest::TestRenderer;
        use std::{
            fs::File,
            path::PathBuf,
            process::{Command, Stdio},
            time::Instant,
        };

        assert_eq!(
            std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
            Ok("1"),
            "set FESTERM_RUN_OPTIONAL_VALIDATION=1"
        );
        let output_root = PathBuf::from(
            std::env::var_os("FESTERM_WARP_UI_OUT").expect("set FESTERM_WARP_UI_OUT"),
        );
        std::fs::create_dir_all(&output_root).expect("create replay output root");
        let output = output_root.join("shared-panels");
        assert!(
            !output.try_exists().expect("check shared-panel replay output"),
            "shared-panels output already exists; retain the earlier attempt and choose a fresh FESTERM_WARP_UI_OUT",
        );
        std::fs::create_dir(&output).expect("reserve fresh shared-panel replay output");
        let revision = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("read source revision");
        assert!(revision.status.success());
        let source_status = Command::new("git")
            .args(["status", "--short", "--branch"])
            .output()
            .expect("read source status");
        assert!(source_status.status.success());
        std::fs::write(output.join("source-head.txt"), &revision.stdout)
            .expect("preserve replay source revision");
        std::fs::write(output.join("source-status.txt"), &source_status.stdout)
            .expect("preserve replay source status");
        let source_diff = output.join("candidate.diff");
        let diff_status = Command::new("git")
            .args([
                "--no-pager",
                "-c",
                "color.ui=false",
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "HEAD",
                "--",
            ])
            .stdout(Stdio::from(
                File::create(&source_diff).expect("create candidate diff"),
            ))
            .status()
            .expect("capture exact tracked candidate diff");
        assert!(diff_status.success(), "candidate diff capture failed");
        let source_diff_sha256 =
            replay_sha256(File::open(&source_diff).expect("open captured candidate diff"));
        let executable = std::env::current_exe().expect("identify actual replay test executable");
        let executable_sha256 =
            replay_sha256(File::open(&executable).expect("open actual replay test executable"));
        std::fs::write(
            output.join("candidate.diff.sha256"),
            format!("{source_diff_sha256}  candidate.diff\n"),
        )
        .expect("preserve candidate diff hash");
        std::fs::write(
            output.join("test-executable.sha256"),
            format!("{executable_sha256}  {}\n", executable.display()),
        )
        .expect("preserve actual test executable hash");
        std::fs::write(
            output.join("provenance.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "source_revision": String::from_utf8_lossy(&revision.stdout).trim(),
                "source_status": String::from_utf8_lossy(&source_status.stdout).trim(),
                "candidate_diff": "candidate.diff",
                "candidate_diff_sha256": source_diff_sha256,
                "test_executable": executable,
                "test_executable_sha256": executable_sha256,
                "measurement_kind": "same-executable ordinary-versus-textureless differential; not shipping before/after",
                "theme": "production Dark",
            }))
            .unwrap(),
        )
        .expect("preserve provenance before renderer setup and timing");
        let mut samples = Vec::new();
        for scene in [
            "inspector-collapsed",
            "inspector-expanded",
            "sftp-100",
            "sftp-5000",
            "unchanged-text-control",
        ] {
            for repeat in 0..4 {
                let mut reference = None;
                for native in if repeat % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let state = create_render_state(default_wgpu_setup(), Default::default());
                    let info = state.adapter.get_info();
                    assert!(
                        use_panel_pipeline(
                            cfg!(windows),
                            info.device_type,
                            info.backend,
                            state.target_format
                        ),
                        "replay requires Windows DX12 CPU/gamma, got {info:?}, {:?}",
                        state.target_format
                    );
                    let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
                    let context = egui::Context::default();
                    context.set_theme(egui::ThemePreference::Dark);
                    context.set_visuals(festerm_ui_egui::theme::default_visuals());
                    context.all_styles_mut(|style| {
                        style.animation_time = 0.0;
                        style.visuals.text_cursor.blink = false;
                    });
                    if native {
                        install(&context, &state);
                    }
                    let panel = context
                        .data(|data| data.get_temp::<Arc<PanelRenderer>>(panel_renderer_id()));
                    assert_eq!(
                        panel.is_some(),
                        native,
                        "no success-shaped renderer fallback"
                    );
                    let mut browser = crate::sftp_file_manager::tests::paint_fixture(
                        &context,
                        if scene == "sftp-5000" { 5000 } else { 100 },
                    );
                    let tab = crate::tabs::AppState::for_test().active();
                    let diagnostics =
                        "Synthetic renderer facts: bounded queues; completed rendering.\n"
                            .repeat(64);
                    let mut content = crate::inspector::tests::base_content(1);
                    content.diagnostics = &diagnostics;
                    content.input_report =
                        "Redacted synthetic routing report; no commands or credentials.";
                    let mut input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1774.0, 1075.0),
                        )),
                        time: Some(0.0),
                        ..Default::default()
                    };
                    input
                        .viewports
                        .get_mut(&egui::ViewportId::ROOT)
                        .unwrap()
                        .native_pixels_per_point = Some(2.0);
                    let mut step = |input: egui::RawInput| {
                        context.run_ui(input, |ui| {
                            if scene.starts_with("inspector") {
                                assert!(crate::inspector::show(
                                    ui.ctx(),
                                    ui.max_rect(),
                                    content.clone(),
                                    false
                                )
                                .is_none());
                            } else if scene.starts_with("sftp") {
                                assert!(browser.show(ui, tab).is_none());
                            } else {
                                ui.heading("Unchanged text control");
                            }
                        })
                    };
                    let mut frame = step(input.clone());
                    renderer.handle_delta(&mut frame.textures_delta);
                    for _ in 0..8 {
                        frame = step(input.clone());
                        renderer.handle_delta(&mut frame.textures_delta);
                    }
                    if scene == "inspector-expanded" {
                        let position = frame
                            .shapes
                            .iter()
                            .find_map(|shape| {
                                if let egui::Shape::Text(text) = &shape.shape {
                                    (text.galley.text() == "Diagnostics")
                                        .then(|| text.pos + text.galley.size() * 0.5)
                                } else {
                                    None
                                }
                            })
                            .expect("visible Diagnostics header");
                        for pressed in [true, false] {
                            let mut click = input.clone();
                            click.events = vec![
                                egui::Event::PointerMoved(position),
                                egui::Event::PointerButton {
                                    pos: position,
                                    button: egui::PointerButton::Primary,
                                    pressed,
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ];
                            frame = step(click);
                            renderer.handle_delta(&mut frame.textures_delta);
                        }
                        for _ in 0..8 {
                            frame = step(input.clone());
                            renderer.handle_delta(&mut frame.textures_delta);
                        }
                        assert!(frame.shapes.iter().any(|shape| {
                            matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("Synthetic renderer facts"))
                        }), "expanded replay must contain the diagnostic report");
                    }
                    let image = renderer
                        .render(&context, &frame)
                        .expect("completed replay warmup");
                    let callbacks_per_frame = if !native || scene == "unchanged-text-control" {
                        0
                    } else if scene.starts_with("inspector") {
                        1
                    } else {
                        4
                    };
                    let paints = || {
                        panel.as_ref().map_or(0, |panel| {
                            panel.paints.load(std::sync::atomic::Ordering::Relaxed)
                        })
                    };
                    assert_eq!(
                        paints(),
                        callbacks_per_frame,
                        "warmup must exercise every selected frame before timing"
                    );
                    assert_eq!(image.dimensions(), (3548, 2150));
                    assert!(
                        image
                            .pixels()
                            .filter(
                                |pixel| pixel.0 == festerm_ui_egui::theme::TEXT_PRIMARY.to_array()
                            )
                            .count()
                            > 20,
                        "visible text oracle"
                    );
                    image
                        .save(output.join(format!(
                            "shared-{scene}-{repeat}-{}.png",
                            if native { "textureless" } else { "ordinary" }
                        )))
                        .expect("save paired replay pixels");
                    if let Some(reference) = &reference {
                        let reference: &image::RgbaImage = reference;
                        assert_eq!(reference.dimensions(), image.dimensions());
                        assert!(
                            reference.pixels().eq(image.pixels()),
                            "{scene}, repeat={repeat} framebuffer differs; paired images retained",
                        );
                    } else {
                        reference = Some(image.clone());
                    }
                    let mut ui_ms = Vec::with_capacity(20);
                    let mut tessellate_ms = Vec::with_capacity(20);
                    let mut draw_readback_ms = Vec::with_capacity(5);
                    for _ in 0..20 {
                        let start = Instant::now();
                        frame = step(input.clone());
                        renderer.handle_delta(&mut frame.textures_delta);
                        ui_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    for _ in 0..20 {
                        let start = Instant::now();
                        std::hint::black_box(
                            context.tessellate(frame.shapes.clone(), context.pixels_per_point()),
                        );
                        tessellate_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    renderer
                        .render(&context, &frame)
                        .expect("completed settling frame");
                    for _ in 0..5 {
                        let start = Instant::now();
                        renderer
                            .render(&context, &frame)
                            .expect("completed measured draw/readback");
                        draw_readback_ms.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    let paints = paints();
                    assert_eq!(
                        paints,
                        callbacks_per_frame * 7,
                        "measured callback attribution"
                    );
                    let sample = serde_json::json!({
                        "scene": scene, "repeat": repeat, "native": native,
                        "adapter": format!("{info:?}"), "format": format!("{:?}", state.target_format),
                        "ui_ms": ui_ms, "tessellate_ms": tessellate_ms,
                        "draw_and_readback_ms": draw_readback_ms, "panel_paints": paints,
                    });
                    eprintln!("shared-panel-replay {sample}");
                    samples.push(sample);
                    std::fs::write(output.join("shared-paint-replay.json"), serde_json::to_vec_pretty(&serde_json::json!({
                        "source_revision": String::from_utf8_lossy(&revision.stdout).trim(),
                        "source_status": String::from_utf8_lossy(&source_status.stdout).trim(),
                        "provenance": "provenance.json",
                        "candidate_diff_sha256": source_diff_sha256,
                        "test_executable_sha256": executable_sha256,
                        "measurement_kind": "same-executable ordinary-versus-textureless differential; not shipping before/after",
                        "pixels": [3548, 2150], "scale": 2.0,
                        "boundary": "UI includes texture deltas; drawing includes tessellation, submission, synchronization and readback; not native presentation",
                        "samples": samples,
                    })).unwrap()).expect("preserve all completed samples");
                }
            }
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
