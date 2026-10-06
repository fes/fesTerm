use eframe::{egui, egui_wgpu, wgpu};
use serde::Serialize;
use std::{ffi::OsStr, time::Instant};

use super::SurfaceKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SceneSet {
    All,
    PickerControls,
    MarkdownControls,
}

impl SceneSet {
    pub(super) fn parse(value: Option<&OsStr>) -> Result<Self, &'static str> {
        match value {
            None => Ok(Self::All),
            Some(value) => match value.to_str() {
                Some("all") => Ok(Self::All),
                Some("picker-controls") => Ok(Self::PickerControls),
                Some("markdown-controls") => Ok(Self::MarkdownControls),
                _ => {
                    Err("FESTERM_WARP_UI_SCENES must be all, picker-controls or markdown-controls")
                }
            },
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::PickerControls => "picker-controls",
            Self::MarkdownControls => "markdown-controls",
        }
    }

    pub(super) fn includes(self, kind: SurfaceKind) -> bool {
        match self {
            Self::All => true,
            Self::PickerControls => matches!(
                kind,
                SurfaceKind::OpenSmall
                    | SurfaceKind::OpenError
                    | SurfaceKind::SaveSmall
                    | SurfaceKind::SaveError
            ),
            Self::MarkdownControls => matches!(
                kind,
                SurfaceKind::MarkdownPreview | SurfaceKind::MarkdownSource
            ),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct DrawSample {
    tessellation_ms: f64,
    prepare_encode_ms: f64,
    submit_ms: f64,
    draw_wait_ms: f64,
    readback_prepare_submit_ms: f64,
    readback_wait_ms: f64,
    image_copy_ms: f64,
    pub(super) total_ms: f64,
    pub(super) work: Work,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct Work {
    meshes: usize,
    vertices: usize,
    indices: usize,
    callbacks: usize,
    callback_command_buffers: usize,
    executed_panel_paints: Option<usize>,
    target_bytes: u64,
    readback_buffer_bytes: u64,
    image_bytes: usize,
}

impl Work {
    fn from_primitives(primitives: &[egui::ClippedPrimitive]) -> Self {
        let mut work = Self::default();
        for primitive in primitives {
            match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => {
                    work.meshes += 1;
                    work.vertices += mesh.vertices.len();
                    work.indices += mesh.indices.len();
                }
                egui::epaint::Primitive::Callback(_) => work.callbacks += 1,
            }
        }
        work
    }
}

fn milliseconds(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn complete(state: &egui_wgpu::RenderState, submission: Option<wgpu::SubmissionIndex>) {
    state
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: submission,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
        .expect("complete profiled surface operation");
}

pub(super) fn render(
    state: &egui_wgpu::RenderState,
    context: &egui::Context,
    output: &egui::FullOutput,
) -> (image::RgbaImage, DrawSample) {
    let total = Instant::now();
    let panel_before = crate::software_background::PanelTestProbe::existing_paints(context);
    let mut renderer = state.renderer.write();
    let size = context.content_rect().size() * context.pixels_per_point();
    let dimensions = [size.x.round() as u32, size.y.round() as u32];
    assert!(dimensions.iter().all(|side| *side > 0));
    assert!(matches!(
        state.target_format,
        wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Rgba8UnormSrgb
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Bgra8UnormSrgb
    ));
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: dimensions,
        pixels_per_point: context.pixels_per_point(),
    };
    let started = Instant::now();
    let primitives = context.tessellate(output.shapes.clone(), context.pixels_per_point());
    let tessellation_ms = milliseconds(started);
    let mut work = Work::from_primitives(&primitives);
    let started = Instant::now();
    let mut encoder = state.device.create_command_encoder(&Default::default());
    let extra = renderer.update_buffers(
        &state.device,
        &state.queue,
        &mut encoder,
        &primitives,
        &screen,
    );
    work.callback_command_buffers = extra.len();
    let target = state.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("profiled surface target"),
        size: wgpu::Extent3d {
            width: dimensions[0],
            height: dimensions[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: state.target_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    work.target_bytes = u64::from(dimensions[0]) * u64::from(dimensions[1]) * 4;
    let view = target.create_view(&Default::default());
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("profiled surface draw"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
        renderer.render(&mut pass, &primitives, &screen);
    }
    let command = encoder.finish();
    let prepare_encode_ms = milliseconds(started);
    let started = Instant::now();
    state.queue.submit(extra.into_iter().chain([command]));
    let submit_ms = milliseconds(started);
    let started = Instant::now();
    complete(state, None);
    let draw_wait_ms = milliseconds(started);

    let started = Instant::now();
    let row_bytes = dimensions[0] * 4;
    let stride =
        row_bytes.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    work.readback_buffer_bytes = u64::from(stride) * u64::from(dimensions[1]);
    let buffer = state.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("profiled surface readback"),
        size: work.readback_buffer_bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = state.device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: None,
            },
        },
        target.size(),
    );
    let submission = state.queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            drop(sender.send(result));
        });
    let readback_prepare_submit_ms = milliseconds(started);
    let started = Instant::now();
    complete(state, Some(submission));
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("profiled readback callback completed")
        .expect("profiled surface readback mapped");
    let readback_wait_ms = milliseconds(started);
    let started = Instant::now();
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .expect("profiled surface readback range");
    let bytes: Vec<u8> = mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..row_bytes as usize].iter().copied())
        .collect();
    work.image_bytes = bytes.len();
    let image = image::RgbaImage::from_raw(dimensions[0], dimensions[1], bytes)
        .expect("profiled framebuffer dimensions");
    drop(mapped);
    buffer.unmap();
    let image_copy_ms = milliseconds(started);
    let panel_after = crate::software_background::PanelTestProbe::existing_paints(context);
    work.executed_panel_paints = match (panel_before, panel_after) {
        (Some(before), Some(after)) => Some(
            after
                .checked_sub(before)
                .expect("panel paint counter remains monotonic"),
        ),
        (None, None) => None,
        _ => panic!("panel renderer changed during an unchanged-frame draw"),
    };
    let sample = DrawSample {
        tessellation_ms,
        prepare_encode_ms,
        submit_ms,
        draw_wait_ms,
        readback_prepare_submit_ms,
        readback_wait_ms,
        image_copy_ms,
        total_ms: milliseconds(total),
        work,
    };
    (image, sample)
}

#[test]
fn warp_scene_selection_preserves_full_default_and_bounds_picker_controls() {
    assert_eq!(SceneSet::parse(None), Ok(SceneSet::All));
    assert_eq!(SceneSet::parse(Some(OsStr::new("all"))), Ok(SceneSet::All));
    let selected = SceneSet::parse(Some(OsStr::new("picker-controls"))).unwrap();
    let scenes = super::bounded_surface_scenes();
    let selected: Vec<_> = scenes
        .iter()
        .filter(|scene| selected.includes(scene.kind))
        .map(|scene| scene.id)
        .collect();
    assert_eq!(
        selected,
        [
            "open-file-small-ready",
            "open-file-small-ready-narrow",
            "open-file-error",
            "open-file-error-narrow",
            "save-as-small-ready",
            "save-as-small-ready-narrow",
            "save-as-error",
            "save-as-error-narrow",
        ]
    );
    let selected = SceneSet::parse(Some(OsStr::new("markdown-controls"))).unwrap();
    let scenes = super::markdown_surface_scenes();
    assert_eq!(
        scenes
            .iter()
            .filter(|scene| selected.includes(scene.kind))
            .map(|scene| scene.id)
            .collect::<Vec<_>>(),
        [
            "markdown-large-preview",
            "markdown-large-preview-narrow",
            "markdown-large-source",
            "markdown-large-source-narrow",
        ],
    );
    assert!(scenes
        .iter()
        .all(|scene| !SceneSet::PickerControls.includes(scene.kind)));
    assert!(super::bounded_surface_scenes()
        .iter()
        .all(|scene| !selected.includes(scene.kind)));
    for invalid in [
        "",
        "picker-controls,all",
        "markdown-controls,all",
        "markdown-controls ",
        "unknown",
        "ALL",
    ] {
        assert!(SceneSet::parse(Some(OsStr::new(invalid))).is_err());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_wide(&[0xd800]);
        assert!(SceneSet::parse(Some(&invalid)).is_err());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let invalid = std::ffi::OsString::from_vec(vec![0xff]);
        assert!(SceneSet::parse(Some(&invalid)).is_err());
    }
}

#[test]
fn profiled_surface_render_matches_original_renderer_at_two_scales() {
    use egui_kittest::{wgpu::WgpuTestRenderer, TestRenderer};
    for scale in [1.0, 1.25] {
        let state = egui_kittest::wgpu::create_render_state(
            egui_kittest::wgpu::default_wgpu_setup(),
            egui_wgpu::RendererOptions::default(),
        );
        let mut reference = WgpuTestRenderer::from_render_state(state.clone());
        let context = egui::Context::default();
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(137.0, 103.0),
            )),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        let mut output = context.run_ui(input, |ui| {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(egui::pos2(10.0, 12.0), egui::vec2(53.0, 47.0)),
                3.0,
                egui::Color32::from_rgba_unmultiplied(30, 80, 170, 129),
            );
            ui.label("Glyph pixels");
        });
        reference.handle_delta(&mut output.textures_delta);
        let expected = reference.render(&context, &output).unwrap();
        let (actual, sample) = render(&state, &context, &output);
        assert_eq!(actual, expected);
        assert!(sample.work.meshes > 0);
        assert!(sample.work.vertices > 0);
        assert!(sample.work.indices > 0);
        assert_eq!(sample.work.target_bytes, actual.as_raw().len() as u64);
        assert!(sample.work.readback_buffer_bytes > sample.work.target_bytes);
        let timed = sample.tessellation_ms
            + sample.prepare_encode_ms
            + sample.submit_ms
            + sample.draw_wait_ms
            + sample.readback_prepare_submit_ms
            + sample.readback_wait_ms
            + sample.image_copy_ms;
        assert!(timed <= sample.total_ms);
    }
}
