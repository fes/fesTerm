use super::*;
use serde::Serialize;
use std::sync::Arc;

pub(crate) const PHYSICAL_SIZE: [u32; 2] = [2058, 1658];

#[derive(Debug, Default, Serialize)]
pub(crate) struct Frame {
    pub native_calls: u64,
    pub updated_pixels: u64,
    pub surface_pixels: u64,
    pub host_copy: bool,
    pub retained_reused: bool,
    pub retained_rebuilt: bool,
    pub retained_texture_bytes: u64,
    pub retained_signature_bytes: usize,
    pub font_atlas_bytes: usize,
    pub font_atlas_cloned_bytes: usize,
    pub font_atlas_reused: bool,
    pub uploaded_textures: usize,
}

pub(crate) struct Renderer {
    state: egui_wgpu::RenderState,
    renderer: WgpuTestRenderer,
    texture: wgpu::Texture,
    status: Arc<super::super::native::Status>,
    retained: egui_wgpu::RetainedUi,
}

impl Renderer {
    pub(crate) fn instance(&self) -> wgpu::Instance {
        self.state.instance.clone()
    }

    pub(crate) fn new(context: &egui::Context) -> Self {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let mut state = create_render_state(setup, Default::default());
        let info = state.adapter.get_info();
        assert_eq!(info.backend, wgpu::Backend::Dx12);
        assert_eq!(info.device_type, wgpu::DeviceType::Cpu);
        state.target_format = wgpu::TextureFormat::Bgra8Unorm;
        *state.renderer.write() =
            egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
        crate::software_background::install(context, &state);
        let status = super::super::native::install_with_painter_options(
            context,
            &state,
            true,
            festerm_ui_egui::NativePainterOptions {
                capture_font_atlas_timings: true,
                ..Default::default()
            },
        )
        .unwrap();
        state.renderer.write().retained_composition_enabled = true;
        let texture = state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("six-session aging target"),
            size: wgpu::Extent3d {
                width: PHYSICAL_SIZE[0],
                height: PHYSICAL_SIZE[1],
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
        Self {
            renderer: WgpuTestRenderer::from_render_state(state.clone()),
            state,
            texture,
            status,
            retained: Default::default(),
        }
    }

    pub(crate) fn calls(&self) -> u64 {
        self.status.frames.load(Ordering::Relaxed)
            + self.status.reused_frames.load(Ordering::Relaxed)
    }

    pub(crate) fn draw(
        &mut self,
        context: &egui::Context,
        mut output: egui::FullOutput,
        calls_before: u64,
    ) -> Frame {
        self.renderer.handle_delta(&mut output.textures_delta);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: PHYSICAL_SIZE,
            pixels_per_point: output.pixels_per_point,
        };
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let (host_copy, retained) = draw_composed(
            &self.state,
            &self.texture,
            &screen,
            &primitives,
            [0.0; 4],
            Some(&mut self.retained),
        );
        assert!(
            self.status.active.load(Ordering::Relaxed),
            "native painting failed: {:?}",
            self.status.first_failure.get()
        );
        let native_calls = self.calls() - calls_before;
        let samples = std::mem::take(&mut *self.status.font_atlas_samples.lock().unwrap());
        assert!(samples.len() <= 1, "one root terminal per frame");
        let stats = self.retained.stats();
        let mut frame = Frame {
            native_calls,
            updated_pixels: if native_calls > 0 {
                self.status.last_updated_pixels.load(Ordering::Relaxed)
            } else {
                0
            },
            surface_pixels: if native_calls > 0 {
                self.status.last_surface_pixels.load(Ordering::Relaxed)
            } else {
                0
            },
            host_copy,
            retained_reused: retained == Some(true),
            retained_rebuilt: retained == Some(false),
            retained_texture_bytes: stats.texture_bytes,
            retained_signature_bytes: stats.signature_bytes,
            ..Default::default()
        };
        if let Some((capture, uploaded)) = samples.into_iter().next() {
            frame.font_atlas_bytes = capture.atlas_bytes;
            frame.font_atlas_cloned_bytes = capture.cloned_bytes;
            frame.font_atlas_reused = capture.reused;
            frame.uploaded_textures = uploaded;
        }
        frame
    }

    pub(crate) fn image(&self) -> image::RgbaImage {
        read_image(&self.state, &self.texture)
    }
}

pub(crate) fn resource_snapshot(instance: &wgpu::Instance) -> serde_json::Value {
    let report = instance
        .generate_report()
        .expect("DX12 must support wgpu registry reports");
    let registries = [
        ("surfaces", &report.surfaces),
        ("adapters", &report.hub.adapters),
        ("devices", &report.hub.devices),
        ("queues", &report.hub.queues),
        ("pipeline_layouts", &report.hub.pipeline_layouts),
        ("shader_modules", &report.hub.shader_modules),
        ("bind_group_layouts", &report.hub.bind_group_layouts),
        ("bind_groups", &report.hub.bind_groups),
        ("command_encoders", &report.hub.command_encoders),
        ("command_buffers", &report.hub.command_buffers),
        ("render_bundles", &report.hub.render_bundles),
        ("render_pipelines", &report.hub.render_pipelines),
        ("compute_pipelines", &report.hub.compute_pipelines),
        ("pipeline_caches", &report.hub.pipeline_caches),
        ("query_sets", &report.hub.query_sets),
        ("buffers", &report.hub.buffers),
        ("textures", &report.hub.textures),
        ("texture_views", &report.hub.texture_views),
        ("external_textures", &report.hub.external_textures),
        ("samplers", &report.hub.samplers),
        ("render_passes", &report.hub.render_passes),
        ("compute_passes", &report.hub.compute_passes),
        ("render_bundle_encoders", &report.hub.render_bundle_encoders),
    ];
    serde_json::Value::Object(
        registries
            .into_iter()
            .map(|(name, registry)| {
                (
                    name.to_owned(),
                    serde_json::json!({
                        "num_allocated": registry.num_allocated,
                        "num_kept_from_user": registry.num_kept_from_user,
                        "num_released_from_user": registry.num_released_from_user,
                        "element_size": registry.element_size,
                    }),
                )
            })
            .collect(),
    )
}
