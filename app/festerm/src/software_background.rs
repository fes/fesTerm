use eframe::{egui, egui_wgpu, wgpu};

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
