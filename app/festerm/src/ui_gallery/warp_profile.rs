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

pub(super) fn picker_backdrop_attribution(
    value: Option<&OsStr>,
    selected: SceneSet,
) -> Result<bool, &'static str> {
    match value {
        None => Ok(false),
        Some(value)
            if value == OsStr::new("textureless-black")
                && selected == SceneSet::PickerControls
                && cfg!(all(windows, target_arch = "x86_64")) =>
        {
            Ok(true)
        }
        Some(_) => Err(
            "FESTERM_WARP_UI_PICKER_BACKDROP requires textureless-black, picker-controls and Windows x64",
        ),
    }
}

pub(super) fn picker_frame_attribution(
    value: Option<&OsStr>,
    selected: SceneSet,
    backdrop_enabled: bool,
) -> Result<bool, &'static str> {
    match value {
        None => Ok(false),
        Some(value)
            if value == OsStr::new("textureless-frame")
                && selected == SceneSet::PickerControls
                && !backdrop_enabled
                && cfg!(all(windows, target_arch = "x86_64")) =>
        {
            Ok(true)
        }
        Some(_) => Err(
            "FESTERM_WARP_UI_PICKER_FRAME requires textureless-frame, picker-controls and Windows x64 without backdrop attribution",
        ),
    }
}

fn opaque_frame_index(
    shapes: &[egui::epaint::ClippedShape],
    viewport: egui::Rect,
) -> Result<usize, &'static str> {
    if !viewport.is_finite() || viewport.min != egui::Pos2::ZERO || !viewport.is_positive() {
        return Err("frame attribution requires a finite positive root viewport");
    }
    let mut matched = None;
    for (index, shape) in shapes.iter().enumerate() {
        if !crate::software_background::complete_opaque_window_frame(shape, viewport) {
            continue;
        }
        if matched.replace(index).is_some() {
            return Err("frame attribution found multiple opaque shadow/frame pairs");
        }
    }
    matched.ok_or("frame attribution requires exactly one complete opaque shadow/frame pair")
}

fn black_backdrop_index(
    shapes: &[egui::epaint::ClippedShape],
    viewport: egui::Rect,
) -> Result<usize, &'static str> {
    if !viewport.is_finite() || viewport.min != egui::Pos2::ZERO || !viewport.is_positive() {
        return Err("backdrop attribution requires a finite positive root viewport");
    }
    let mut matched = None;
    for (index, shape) in shapes.iter().enumerate() {
        if !crate::software_background::full_root_black_backdrop(shape, viewport) {
            continue;
        }
        if matched.replace(index).is_some() {
            return Err(
                "backdrop attribution found multiple full-root translucent black rectangles",
            );
        }
    }
    matched.ok_or("backdrop attribution requires exactly one full-root translucent black rectangle")
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

#[cfg(all(windows, target_arch = "x86_64"))]
pub(super) fn attribute_picker_backdrop(
    state: &egui_wgpu::RenderState,
    context: &egui::Context,
    output: &egui::FullOutput,
    expected: &image::RgbaImage,
) -> serde_json::Value {
    let viewport = context.viewport_rect();
    let index = black_backdrop_index(&output.shapes, viewport)
        .expect("actual picker frame has one attributable black backdrop");
    let source = &output.shapes[index];
    let egui::Shape::Rect(backdrop) = &source.shape else {
        unreachable!("matched a rectangle");
    };
    let mut report = attribute_picker_shape(
        state,
        context,
        output,
        expected,
        index,
        "textureless-black",
        "textureless_black",
    );
    report["schema"] = serde_json::json!("festerm-picker-backdrop-attribution-v1");
    report["backdrop_color_rgba"] = serde_json::json!(backdrop.fill.to_array());
    report["backdrop_vertices"] = report["geometry_vertices"].clone();
    report["backdrop_indices"] = report["geometry_indices"].clone();
    report["scope"] = serde_json::json!(
        "Same unchanged actual frame and white-UV geometry; only its unique full-root translucent black backdrop uses the existing installed panel shader. Six balanced ordered pairs and every exact pixel retained. Callback construction is outside render timing. Not a shipping optimization, isolated GPU timestamp, native presentation, physical latency or total-resource claim."
    );
    report
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub(super) fn attribute_picker_frame(
    state: &egui_wgpu::RenderState,
    context: &egui::Context,
    output: &egui::FullOutput,
    expected: &image::RgbaImage,
) -> serde_json::Value {
    let viewport = context.viewport_rect();
    let index = opaque_frame_index(&output.shapes, viewport)
        .expect("actual picker has one complete opaque shadow/frame pair");
    let egui::Shape::Vec(parts) = &output.shapes[index].shape else {
        unreachable!("matched a shadow/frame pair");
    };
    let [egui::Shape::Rect(shadow), egui::Shape::Rect(frame)] = parts.as_slice() else {
        unreachable!("matched two rectangles");
    };
    let mut report = attribute_picker_shape(
        state,
        context,
        output,
        expected,
        index,
        "textureless-frame",
        "textureless_frame",
    );
    report["schema"] = serde_json::json!("festerm-picker-frame-attribution-v1");
    report["frame_fill_rgba"] = serde_json::json!(frame.fill.to_array());
    report["frame_rect"] = serde_json::json!([
        frame.rect.min.x,
        frame.rect.min.y,
        frame.rect.max.x,
        frame.rect.max.y
    ]);
    report["frame_stroke_rgba"] = serde_json::json!(frame.stroke.color.to_array());
    report["frame_stroke_width"] = serde_json::json!(frame.stroke.width);
    report["frame_corner_radii"] = serde_json::json!([
        frame.corner_radius.nw,
        frame.corner_radius.ne,
        frame.corner_radius.sw,
        frame.corner_radius.se
    ]);
    report["shadow_rgba"] = serde_json::json!(shadow.fill.to_array());
    report["shadow_rect"] = serde_json::json!([
        shadow.rect.min.x,
        shadow.rect.min.y,
        shadow.rect.max.x,
        shadow.rect.max.y
    ]);
    report["shadow_blur_width"] = serde_json::json!(shadow.blur_width);
    report
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn attribute_picker_shape(
    state: &egui_wgpu::RenderState,
    context: &egui::Context,
    output: &egui::FullOutput,
    expected: &image::RgbaImage,
    index: usize,
    mode: &str,
    key: &str,
) -> serde_json::Value {
    let viewport = context.viewport_rect();
    let source = &output.shapes[index];
    let primitives = context.tessellate(vec![source.clone()], context.pixels_per_point());
    let [egui::ClippedPrimitive {
        primitive: egui::epaint::Primitive::Mesh(mesh),
        ..
    }] = primitives.as_slice()
    else {
        panic!("attributed picker shape must tessellate to exactly one mesh");
    };
    let mut converted = output.clone();
    converted.shapes[index].shape = crate::software_background::PanelTestProbe::existing(context)
        .white_mesh_shape(viewport, mesh)
        .expect("attributed picker shape has valid white-UV geometry and eligible pipeline");
    for frame in [output, &converted] {
        let (image, _) = render(state, context, frame);
        assert_eq!(&image, expected, "picker attribution warmup pixels");
    }
    let mut pairs = Vec::new();
    let mut ordinary_times = Vec::new();
    let mut converted_times = Vec::new();
    for pair in 0..6 {
        let converted_first = pair % 2 != 0;
        let (first, second) = if converted_first {
            (&converted, output)
        } else {
            (output, &converted)
        };
        let (first_image, first_sample) = render(state, context, first);
        let (second_image, second_sample) = render(state, context, second);
        assert_eq!(&first_image, expected, "first measured picker pixels");
        assert_eq!(&second_image, expected, "second measured picker pixels");
        let (ordinary, textureless) = if converted_first {
            (second_sample, first_sample)
        } else {
            (first_sample, second_sample)
        };
        assert_eq!(
            textureless.work.executed_panel_paints,
            ordinary
                .work
                .executed_panel_paints
                .map(|count| count.checked_add(1).expect("bounded panel paint count")),
            "the attributed picker callback must execute exactly once",
        );
        assert!(ordinary.work.executed_panel_paints.is_some());
        ordinary_times.push(ordinary.total_ms);
        converted_times.push(textureless.total_ms);
        let mut sample = serde_json::json!({
            "order": if converted_first { [mode, "ordinary"] } else { ["ordinary", mode] },
            "ordinary": ordinary,
            "pixels_equal": true,
        });
        sample[key] = serde_json::to_value(textureless).unwrap();
        pairs.push(sample);
    }
    let mut report = serde_json::json!({
        "shape_index": index,
        "viewport_points": [viewport.width(), viewport.height()],
        "pixels_per_point": context.pixels_per_point(),
        "geometry_vertices": mesh.vertices.len(),
        "geometry_indices": mesh.indices.len(),
        "paired_samples": pairs,
        "ordinary_completed_draw_readback": crate::surface_performance::timing_distribution(&ordinary_times),
        "pixels_equal": true,
        "scope": "Same unchanged actual picker and original white-UV shadow/frame geometry; only one exact opaque shadow/frame pair uses the existing installed panel shader. Six balanced ordered pairs preserve every pixel and require one additional callback. Construction excluded from draw timing; not isolated GPU timestamps, native latency, production enablement or resource acceptance.",
    });
    report[format!("{key}_completed_draw_readback")] = serde_json::to_value(
        crate::surface_performance::timing_distribution(&converted_times),
    )
    .unwrap();
    report
}

#[test]
fn picker_frame_attribution_requires_exclusive_bounded_selection() {
    for selected in [
        SceneSet::All,
        SceneSet::PickerControls,
        SceneSet::MarkdownControls,
    ] {
        assert_eq!(picker_frame_attribution(None, selected, false), Ok(false));
        for value in ["1", "textureless-frame ", "textureless-black"] {
            assert!(picker_frame_attribution(Some(OsStr::new(value)), selected, false).is_err());
        }
        assert!(
            picker_frame_attribution(Some(OsStr::new("textureless-frame")), selected, true)
                .is_err()
        );
    }
    assert!(
        picker_frame_attribution(Some(OsStr::new("textureless-frame")), SceneSet::All, false)
            .is_err()
    );
    assert!(picker_frame_attribution(
        Some(OsStr::new("textureless-frame")),
        SceneSet::MarkdownControls,
        false
    )
    .is_err());
    assert_eq!(
        picker_frame_attribution(
            Some(OsStr::new("textureless-frame")),
            SceneSet::PickerControls,
            false
        )
        .is_ok(),
        cfg!(all(windows, target_arch = "x86_64"))
    );
}

#[test]
fn picker_frame_attribution_requires_unique_complete_shadow_frame() {
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(240.0, 180.0));
    let rect = egui::Rect::from_min_max(egui::pos2(20.0, 20.0), egui::pos2(210.0, 150.0));
    let original = egui::epaint::ClippedShape {
        clip_rect: viewport,
        shape: egui::Frame::window(&egui::Style::default()).paint(rect),
    };
    assert_eq!(
        opaque_frame_index(std::slice::from_ref(&original), viewport),
        Ok(0)
    );
    assert!(opaque_frame_index(&[], viewport).is_err());
    assert!(opaque_frame_index(&[original.clone(), original.clone()], viewport).is_err());
    for case in [
        "clip",
        "outside",
        "translucent",
        "colored-shadow",
        "invisible-shadow",
        "nonfinite",
    ] {
        let mut invalid = original.clone();
        if case == "clip" {
            invalid.clip_rect.max.x = rect.center().x;
        }
        let egui::Shape::Vec(parts) = &mut invalid.shape else {
            unreachable!()
        };
        let [egui::Shape::Rect(shadow), egui::Shape::Rect(frame)] = parts.as_mut_slice() else {
            unreachable!()
        };
        match case {
            "outside" => frame.rect.min.x = -1.0,
            "translucent" => frame.fill = egui::Color32::from_black_alpha(100),
            "colored-shadow" => {
                shadow.fill = egui::Color32::from_rgba_unmultiplied(31, 43, 61, 128)
            }
            "invisible-shadow" => shadow.fill = egui::Color32::TRANSPARENT,
            "nonfinite" => shadow.blur_width = f32::NAN,
            _ => {}
        }
        assert!(
            opaque_frame_index(std::slice::from_ref(&invalid), viewport).is_err(),
            "{case}"
        );
    }
    assert!(opaque_frame_index(
        std::slice::from_ref(&original),
        viewport.translate(egui::vec2(1.0, 0.0))
    )
    .is_err());
}

#[test]
fn picker_backdrop_attribution_requires_explicit_bounded_supported_selection() {
    for selected in [
        SceneSet::All,
        SceneSet::PickerControls,
        SceneSet::MarkdownControls,
    ] {
        assert_eq!(picker_backdrop_attribution(None, selected), Ok(false));
        assert!(picker_backdrop_attribution(Some(OsStr::new("1")), selected).is_err());
        assert!(
            picker_backdrop_attribution(Some(OsStr::new("textureless-black ")), selected).is_err()
        );
    }
    assert!(
        picker_backdrop_attribution(Some(OsStr::new("textureless-black")), SceneSet::All).is_err()
    );
    assert!(picker_backdrop_attribution(
        Some(OsStr::new("textureless-black")),
        SceneSet::MarkdownControls
    )
    .is_err());
    assert_eq!(
        picker_backdrop_attribution(
            Some(OsStr::new("textureless-black")),
            SceneSet::PickerControls
        )
        .is_ok(),
        cfg!(all(windows, target_arch = "x86_64")),
    );
}

#[test]
fn picker_backdrop_attribution_rejects_missing_ambiguous_or_nonexact_geometry() {
    let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 480.0));
    let backdrop = egui::epaint::ClippedShape {
        clip_rect: viewport,
        shape: egui::Shape::rect_filled(viewport, 0.0, egui::Color32::from_black_alpha(128)),
    };
    assert_eq!(
        black_backdrop_index(std::slice::from_ref(&backdrop), viewport),
        Ok(0)
    );
    assert!(black_backdrop_index(&[], viewport).is_err());
    assert!(black_backdrop_index(&[backdrop.clone(), backdrop.clone()], viewport).is_err());
    for shape in [
        egui::Shape::rect_filled(viewport, 0.0, egui::Color32::BLACK),
        egui::Shape::rect_filled(viewport, 0.0, egui::Color32::TRANSPARENT),
        egui::Shape::rect_filled(
            viewport,
            0.0,
            egui::Color32::from_rgba_premultiplied(1, 0, 0, 128),
        ),
        egui::Shape::rect_filled(viewport, 2.0, egui::Color32::from_black_alpha(128)),
        egui::Shape::rect_filled(
            viewport.shrink(1.0),
            0.0,
            egui::Color32::from_black_alpha(128),
        ),
    ] {
        assert!(black_backdrop_index(
            &[egui::epaint::ClippedShape {
                clip_rect: viewport,
                shape
            }],
            viewport
        )
        .is_err());
    }
    let mut clipped = backdrop.clone();
    clipped.clip_rect = viewport.shrink(1.0);
    assert!(black_backdrop_index(&[clipped], viewport).is_err());
    let mut blurred = backdrop.clone();
    let egui::Shape::Rect(rect) = &mut blurred.shape else {
        unreachable!()
    };
    rect.blur_width = 1.0;
    assert!(black_backdrop_index(&[blurred], viewport).is_err());
    let mut stroked = backdrop.clone();
    let egui::Shape::Rect(rect) = &mut stroked.shape else {
        unreachable!()
    };
    rect.stroke = egui::Stroke::new(1.0, egui::Color32::WHITE);
    assert!(black_backdrop_index(&[stroked], viewport).is_err());
    let mut textured = backdrop.clone();
    let egui::Shape::Rect(rect) = &mut textured.shape else {
        unreachable!()
    };
    rect.brush = Some(std::sync::Arc::new(egui::epaint::Brush {
        fill_texture_id: egui::TextureId::Managed(1),
        uv: viewport,
    }));
    assert!(black_backdrop_index(&[textured], viewport).is_err());
    assert!(black_backdrop_index(&[backdrop], viewport.translate(egui::vec2(1.0, 0.0))).is_err());
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

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn picker_frame_attribution_preserves_pixels_and_balanced_order() {
    use egui_kittest::{wgpu::WgpuTestRenderer, TestRenderer};
    for scale in [1.0, 1.25] {
        let mut setup = egui_kittest::wgpu::default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state =
            egui_kittest::wgpu::create_render_state(setup, egui_wgpu::RendererOptions::default());
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::dark());
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let _probe = crate::software_background::PanelTestProbe::install(&context, &state);
        let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(224.0, 160.0));
        let mut input = egui::RawInput {
            screen_rect: Some(viewport),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        let mut frame = egui::FullOutput::default();
        for _ in 0..5 {
            frame = context.run_ui(input.clone(), |ui| {
                ui.label("Underlying content");
                egui::Modal::new(egui::Id::new("frame-attribution-modal")).show(&context, |ui| {
                    ui.set_min_size(egui::vec2(100.0, 50.0));
                    ui.label("Original picker");
                    ui.button("Choose").on_hover_text("Live response");
                });
            });
            renderer.handle_delta(&mut frame.textures_delta);
        }
        let expected = renderer.render(&context, &frame).unwrap();
        let report = attribute_picker_frame(&state, &context, &frame, &expected);
        assert_eq!(report["schema"], "festerm-picker-frame-attribution-v1");
        let pairs = report["paired_samples"].as_array().unwrap();
        assert_eq!(pairs.len(), 6);
        assert_eq!(
            pairs
                .iter()
                .filter(|pair| pair["order"][0] == "ordinary")
                .count(),
            3
        );
        for pair in pairs {
            assert_eq!(pair["pixels_equal"], true);
            assert_eq!(pair["ordinary"]["work"]["executed_panel_paints"], 0);
            assert_eq!(
                pair["textureless_frame"]["work"]["executed_panel_paints"],
                1
            );
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[test]
fn picker_backdrop_attribution_preserves_full_frame_pixels_and_balanced_order() {
    use egui_kittest::{wgpu::WgpuTestRenderer, TestRenderer};
    for scale in [1.0, 1.25] {
        let state = egui_kittest::wgpu::create_render_state(
            egui_kittest::wgpu::default_wgpu_setup(),
            egui_wgpu::RendererOptions::default(),
        );
        let context = egui::Context::default();
        let _probe = crate::software_background::PanelTestProbe::install(&context, &state);
        let mut renderer = WgpuTestRenderer::from_render_state(state.clone());
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(128.0, 96.0));
        let mut input = egui::RawInput {
            screen_rect: Some(viewport),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(scale);
        let mut frame = egui::FullOutput::default();
        for _ in 0..3 {
            frame = context.run_ui(input.clone(), |ui| {
                ui.painter()
                    .rect_filled(viewport, 0.0, egui::Color32::from_rgb(31, 43, 61));
                ui.label("Underlying content");
                let painter = context.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("attribution-backdrop"),
                ));
                painter.rect_filled(viewport, 0.0, egui::Color32::from_black_alpha(128));
                painter.text(
                    egui::pos2(15.0, 45.0),
                    egui::Align2::LEFT_TOP,
                    "Picker stays live",
                    egui::FontId::proportional(12.0),
                    egui::Color32::WHITE,
                );
            });
            renderer.handle_delta(&mut frame.textures_delta);
        }
        let expected = renderer.render(&context, &frame).unwrap();
        let report = attribute_picker_backdrop(&state, &context, &frame, &expected);
        let pairs = report["paired_samples"].as_array().unwrap();
        assert_eq!(pairs.len(), 6);
        assert_eq!(
            pairs
                .iter()
                .filter(|pair| pair["order"][0] == "ordinary")
                .count(),
            3,
        );
        for pair in pairs {
            assert_eq!(pair["pixels_equal"], true);
            assert_eq!(pair["ordinary"]["work"]["executed_panel_paints"], 0);
            assert_eq!(
                pair["textureless_black"]["work"]["executed_panel_paints"],
                1
            );
        }
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
