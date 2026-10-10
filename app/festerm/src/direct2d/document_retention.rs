use super::{
    host_copy::{render_state, target},
    profile, *,
};
use egui::epaint::{ClippedPrimitive, Primitive};
use egui_kittest::{wgpu::WgpuTestRenderer, TestRenderer};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn assert_frame(
    state: &egui_wgpu::RenderState,
    screen: &egui_wgpu::ScreenDescriptor,
    jobs: &[ClippedPrimitive],
    clear: [f32; 4],
    cache: &mut egui_wgpu::RetainedUi,
    expected: Option<bool>,
) -> image::RgbaImage {
    let texture = target(state, screen.size_in_pixels, state.target_format, true);
    assert_eq!(
        profile::draw_composed(state, &texture, screen, jobs, clear, None),
        (false, None),
    );
    let reference = profile::read_image(state, &texture);
    assert_eq!(
        profile::draw_composed(state, &texture, screen, jobs, clear, Some(cache)),
        (false, expected),
        "{:?}",
        cache.stats(),
    );
    profile::assert_same_pixels(
        &reference,
        &profile::read_image(state, &texture),
        "complete-frame retention must preserve ordinary pixels",
    );
    reference
}

fn fixture() -> (
    egui_wgpu::RenderState,
    egui_wgpu::ScreenDescriptor,
    Vec<ClippedPrimitive>,
) {
    let state = render_state();
    state.renderer.write().retained_frame_enabled = true;
    state.renderer.write().update_texture(
        &state.device,
        &state.queue,
        egui::TextureId::Managed(77),
        &egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ),
    );
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(128.0, 96.0));
    let mut mesh = egui::Mesh::with_texture(egui::TextureId::Managed(77));
    mesh.add_rect_with_uv(
        rect.shrink(12.0),
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        egui::Color32::from_rgb(70, 100, 150),
    );
    (
        state,
        egui_wgpu::ScreenDescriptor {
            size_in_pixels: [128, 96],
            pixels_per_point: 1.0,
        },
        vec![ClippedPrimitive {
            clip_rect: rect,
            primitive: Primitive::Mesh(mesh),
        }],
    )
}

fn document_frames(
    state: &egui_wgpu::RenderState,
    uploader: &mut WgpuTestRenderer,
    path: &std::path::Path,
    mode: crate::text_editor::EditorMode,
    scales: &[f32],
    mut observe: impl FnMut(&egui_wgpu::ScreenDescriptor, &[ClippedPrimitive]),
) {
    use crate::{documents::DocumentRegistry, tabs::TabId, text_editor::TextEditorTab};
    let documents = DocumentRegistry::shared();
    let document = documents.borrow_mut().open_local(path).unwrap();
    let mut editor = TextEditorTab::new(document, &documents);
    editor.set_mode_for_gallery(mode);
    let tab = TabId::next_for_test();
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    context.set_visuals(festerm_ui_egui::theme::default_visuals());
    context.all_styles_mut(|style| style.animation_time = 0.0);
    crate::software_background::install(&context, state);
    for &scale in scales {
        let mut jobs = Vec::new();
        for _ in 0..8 {
            let mut input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 400.0),
                )),
                time: Some(10.0),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .native_pixels_per_point = Some(scale);
            let mut output = context.run_ui(input, |ui| {
                assert!(editor.show(ui, tab, &documents).is_none());
            });
            uploader.handle_delta(&mut output.textures_delta);
            jobs = context.tessellate(output.shapes, scale);
        }
        observe(
            &egui_wgpu::ScreenDescriptor {
                size_in_pixels: [(640.0 * scale) as u32, (400.0 * scale) as u32],
                pixels_per_point: scale,
            },
            &jobs,
        );
    }
}

fn document_cases() -> [(&'static str, crate::text_editor::EditorMode, String); 3] {
    use crate::text_editor::EditorMode;
    [
        ("fixture.txt", EditorMode::Edit, "Owned small editor line.\n".repeat(200)),
        ("fixture.md", EditorMode::Preview, "# Owned preview\n\nA **selectable** paragraph.\n\n| Name | Value |\n| --- | --- |\n| Item | 1 |\n\n".repeat(8)),
        ("split.md", EditorMode::Split, "# Owned split\n\nEditable and selectable text.\n\n".repeat(8)),
    ]
}

#[test]
fn retained_document_frames_preserve_editor_preview_and_split_pixels() {
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("document-retention-fixtures");
    std::fs::create_dir_all(&base).unwrap();
    let owned = tempfile::tempdir_in(base).unwrap();
    let state = render_state();
    state.renderer.write().retained_frame_enabled = true;
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    for (name, mode, text) in document_cases() {
        let path = owned.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut cache = egui_wgpu::RetainedUi::default();
        document_frames(
            &state,
            &mut uploader,
            &path,
            mode,
            &[1.0, 1.25, 2.0, 1.0],
            |screen, jobs| {
                assert_frame(&state, screen, jobs, [0.0; 4], &mut cache, Some(false));
                assert_frame(&state, screen, jobs, [0.0; 4], &mut cache, Some(true));
                let texture = target(&state, screen.size_in_pixels, state.target_format, true);
                let capture = egui_wgpu::capture::CaptureState::new(&state.device, &texture);
                assert_eq!(
                    profile::draw_composed(
                        &state,
                        &capture.texture,
                        screen,
                        jobs,
                        [0.0; 4],
                        Some(&mut cache)
                    ),
                    (false, Some(true)),
                );
                let ordinary = assert_frame(&state, screen, jobs, [0.0; 4], &mut cache, Some(true));
                profile::assert_same_pixels(
                    &ordinary,
                    &profile::read_image(&state, &capture.texture),
                    "capture targets preserve complete document pixels",
                );
            },
        );
        assert!(cache.stats().reused_frames >= 12);
        assert!(cache.stats().texture_bytes <= 64 * 1024 * 1024);
        assert!(cache.stats().signature_bytes <= 1024 * 1024);
    }
}

#[test]
#[ignore = "opt-in completed WARP all-miss control; not native input/display latency"]
fn profile_document_retention_changing_frames() {
    use festerm_windows_direct2d::process_cpu_time;
    use std::time::Instant;

    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("FESTERM_DOCUMENT_RETENTION_PROFILE_OUT").expect("fresh output directory"),
    );
    assert!(
        output.is_absolute() && !output.exists(),
        "preserve previous evidence"
    );
    std::fs::create_dir_all(&output).unwrap();
    let state = render_state();
    let adapter = state.adapter.get_info();
    assert_eq!(adapter.device_type, wgpu::DeviceType::Cpu);
    assert_eq!(adapter.backend, wgpu::Backend::Dx12);
    state.renderer.write().retained_frame_enabled = true;
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    let owned = tempfile::tempdir_in(&output).unwrap();
    let mut records = Vec::new();
    const FRAMES: u32 = 32;
    for (name, mode, text) in document_cases() {
        let path = owned.path().join(name);
        std::fs::write(&path, text).unwrap();
        let mut prepared = None;
        document_frames(
            &state,
            &mut uploader,
            &path,
            mode,
            &[2.0],
            |screen, jobs| {
                prepared = Some((
                    egui_wgpu::ScreenDescriptor {
                        size_in_pixels: screen.size_in_pixels,
                        pixels_per_point: screen.pixels_per_point,
                    },
                    jobs.to_vec(),
                ));
            },
        );
        let (screen, jobs) = prepared.unwrap();
        let texture = target(&state, screen.size_in_pixels, state.target_format, true);
        let mut samples = Vec::new();
        for (order, enabled) in [false, true, true, false].into_iter().enumerate() {
            let mut cache = egui_wgpu::RetainedUi::default();
            let frame = |index: u32, cache: &mut egui_wgpu::RetainedUi| {
                let clear = [
                    if index.is_multiple_of(2) { 0.01 } else { 0.02 },
                    0.0,
                    0.0,
                    1.0,
                ];
                assert_eq!(
                    profile::draw_composed(
                        &state,
                        &texture,
                        &screen,
                        &jobs,
                        clear,
                        enabled.then_some(cache),
                    ),
                    (false, enabled.then_some(false)),
                    "every enabled changing signature must rebuild",
                );
            };
            for index in 0..4 {
                frame(index, &mut cache);
            }
            let before = cache.stats();
            let cpu_start = process_cpu_time().unwrap();
            let started = Instant::now();
            for index in 0..FRAMES {
                frame(index, &mut cache);
            }
            let elapsed = started.elapsed();
            let cpu = process_cpu_time().unwrap() - cpu_start;
            samples.push((cpu.as_secs_f64(), elapsed.as_secs_f64()));
            let after = cache.stats();
            assert_eq!(after.reused_frames - before.reused_frames, 0);
            assert_eq!(
                after.rebuilt_frames - before.rebuilt_frames,
                if enabled { u64::from(FRAMES) } else { 0 }
            );
            let retained_pixels = profile::read_image(&state, &texture);
            profile::draw_composed(
                &state,
                &texture,
                &screen,
                &jobs,
                [0.02, 0.0, 0.0, 1.0],
                None,
            );
            profile::assert_same_pixels(
                &retained_pixels,
                &profile::read_image(&state, &texture),
                "changing frame pixels",
            );
            let record = serde_json::json!({
                "scene":name, "order":order, "enabled":enabled, "frames":FRAMES,
                "cpu_ms_per_frame":cpu.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                "completed_ms_per_frame":elapsed.as_secs_f64() * 1000.0 / f64::from(FRAMES),
                "hits":after.reused_frames - before.reused_frames,
                "rebuilds":after.rebuilt_frames - before.rebuilt_frames,
            });
            println!("{record}");
            records.push(record);
            std::fs::write(output.join("profile.json"), serde_json::to_vec_pretty(&serde_json::json!({
                        "adapter":format!("{adapter:?}"), "records":records,
                        "maximum_on_off_ratio":1.10,
                        "limitations":"Prepared real document jobs, alternating exact clear color, completed GPU work per frame, readback outside timing. All-miss overhead control, not native latency or an active-input workload.",
                    })).unwrap()).unwrap();
        }
        let cpu_ratio = (samples[1].0 + samples[2].0) / (samples[0].0 + samples[3].0);
        let elapsed_ratio = (samples[1].1 + samples[2].1) / (samples[0].1 + samples[3].1);
        println!(
            "{name}: all-miss CPU ratio={cpu_ratio:.4}, completed-render ratio={elapsed_ratio:.4}"
        );
        assert!(
            cpu_ratio <= 1.10 && elapsed_ratio <= 1.10,
            "{name}: all-miss overhead exceeds the bounded 10% regression ceiling"
        );
    }
}
#[test]
fn retained_document_frames_invalidate_exact_inputs_and_texture_ownership() {
    let (mut state, mut screen, mut jobs) = fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    let initial = assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(true));
    for change in 0..6 {
        match change {
            0 => {
                let Primitive::Mesh(mesh) = &mut jobs[0].primitive else {
                    unreachable!()
                };
                mesh.vertices[0].color = egui::Color32::RED;
            }
            1 => jobs[0].clip_rect = jobs[0].clip_rect.shrink(20.0),
            2 => screen.pixels_per_point = 1.25,
            3 => screen.size_in_pixels = [144, 100],
            4 => jobs.reverse(),
            5 => state.target_format = wgpu::TextureFormat::Rgba8Unorm,
            _ => unreachable!(),
        }
        if change == 4 {
            let mut overlay = jobs[0].clone();
            let Primitive::Mesh(mesh) = &mut overlay.primitive else {
                unreachable!()
            };
            for vertex in &mut mesh.vertices {
                vertex.color = egui::Color32::GREEN;
            }
            jobs.push(overlay);
        }
        if change == 5 {
            let mut replacement =
                egui_wgpu::Renderer::new(&state.device, state.target_format, Default::default());
            replacement.retained_frame_enabled = true;
            replacement.update_texture(
                &state.device,
                &state.queue,
                egui::TextureId::Managed(77),
                &egui::epaint::ImageDelta::full(
                    egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                    egui::TextureOptions::NEAREST,
                ),
            );
            *state.renderer.write() = replacement;
        }
        assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
        assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(true));
    }
    assert_frame(
        &state,
        &screen,
        &jobs,
        [0.1, 0.2, 0.3, 1.0],
        &mut cache,
        Some(false),
    );
    jobs.reverse();
    assert_frame(
        &state,
        &screen,
        &jobs,
        [0.1, 0.2, 0.3, 1.0],
        &mut cache,
        Some(false),
    );
    let id = egui::TextureId::Managed(77);
    for delta in [
        egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([2, 2], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        egui::epaint::ImageDelta::partial(
            [0, 0],
            egui::ColorImage::filled([1, 1], egui::Color32::GREEN),
            egui::TextureOptions::LINEAR,
        ),
    ] {
        state
            .renderer
            .write()
            .update_texture(&state.device, &state.queue, id, &delta);
        let changed = assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
        assert_ne!(initial, changed);
        assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(true));
    }
    state.renderer.write().free_texture(&id);
    let texture = target(&state, screen.size_in_pixels, state.target_format, true);
    let mut encoder = state.device.create_command_encoder(&Default::default());
    assert_eq!(
        cache.try_render_frame(
            &state.renderer.read(),
            &state.device,
            &mut encoder,
            &jobs,
            &screen,
            &texture,
            [0.0; 4]
        ),
        None
    );
    assert_eq!(cache.stats().texture_bytes, 0);
    state.renderer.write().update_texture(
        &state.device,
        &state.queue,
        id,
        &egui::epaint::ImageDelta::full(
            egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
            egui::TextureOptions::NEAREST,
        ),
    );
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    let external = state
        .renderer
        .read()
        .texture(&id)
        .unwrap()
        .texture
        .clone()
        .unwrap();
    let view = external.create_view(&Default::default());
    let user = state.renderer.write().register_native_texture(
        &state.device,
        &view,
        wgpu::FilterMode::Nearest,
    );
    let mut user_jobs = jobs.clone();
    let Primitive::Mesh(mesh) = &mut user_jobs[0].primitive else {
        unreachable!()
    };
    mesh.texture_id = user;
    assert_frame(&state, &screen, &user_jobs, [0.0; 4], &mut cache, None);
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, None);
    assert_eq!(
        cache.stats().decline_reason,
        Some("managed texture ownership is not exclusive")
    );
    state
        .renderer
        .write()
        .update_egui_texture_from_wgpu_texture(&state.device, &view, wgpu::FilterMode::Nearest, id);
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, None);
}

#[test]
fn retained_document_frames_decline_targets_callbacks_and_budgets() {
    let (state, screen, jobs) = fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    state.renderer.write().retained_frame_enabled = false;
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, None);
    assert_eq!(cache.stats().texture_bytes, 0);
    state.renderer.write().retained_frame_enabled = true;
    let mut oversized = jobs.clone();
    let Primitive::Mesh(mesh) = &mut oversized[0].primitive else {
        unreachable!()
    };
    mesh.vertices.resize(
        1024 * 1024 / std::mem::size_of::<egui::epaint::Vertex>() + 1,
        mesh.vertices[0],
    );
    assert_frame(&state, &screen, &oversized, [0.0; 4], &mut cache, None);
    assert_eq!(
        cache.stats().decline_reason,
        Some("paint signature exceeds retention budget")
    );
    let texture = target(&state, screen.size_in_pixels, state.target_format, true);
    let mut encoder = state.device.create_command_encoder(&Default::default());
    for invalid in [
        target(&state, screen.size_in_pixels, state.target_format, false),
        target(&state, [64, 64], state.target_format, true),
        target(
            &state,
            screen.size_in_pixels,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            true,
        ),
        target(
            &state,
            screen.size_in_pixels,
            wgpu::TextureFormat::Rgba8Unorm,
            true,
        ),
    ] {
        assert_eq!(
            cache.try_render_frame(
                &state.renderer.read(),
                &state.device,
                &mut encoder,
                &jobs,
                &screen,
                &invalid,
                [0.0; 4]
            ),
            None
        );
    }
    for scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let invalid = egui_wgpu::ScreenDescriptor {
            size_in_pixels: screen.size_in_pixels,
            pixels_per_point: scale,
        };
        assert_eq!(
            cache.try_render_frame(
                &state.renderer.read(),
                &state.device,
                &mut encoder,
                &jobs,
                &invalid,
                &texture,
                [0.0; 4]
            ),
            None
        );
    }
    assert_eq!(
        cache.try_render_frame(
            &state.renderer.read(),
            &state.device,
            &mut encoder,
            &jobs,
            &screen,
            &texture,
            [f32::NAN; 4]
        ),
        None
    );
    for options in [
        egui_wgpu::RendererOptions {
            msaa_samples: 4,
            ..Default::default()
        },
        egui_wgpu::RendererOptions {
            depth_stencil_format: Some(wgpu::TextureFormat::Depth24Plus),
            ..Default::default()
        },
    ] {
        let mut renderer = egui_wgpu::Renderer::new(&state.device, state.target_format, options);
        renderer.retained_frame_enabled = true;
        assert_eq!(
            cache.try_render_frame(
                &renderer,
                &state.device,
                &mut encoder,
                &jobs,
                &screen,
                &texture,
                [0.0; 4]
            ),
            None
        );
    }
    let mut unknown = jobs.clone();
    unknown.push(observed_job(
        None,
        None,
        &Arc::new(AtomicUsize::new(0)),
        &Arc::new(AtomicUsize::new(0)),
        &Arc::new(AtomicUsize::new(0)),
    ));
    assert_frame(&state, &screen, &unknown, [0.0; 4], &mut cache, None);
    let mut copied = jobs.clone();
    copied.push(observed_job(
        Some(egui_wgpu::CallbackPaintKey {
            namespace: Arc::new(()),
            bytes: Arc::from([]),
        }),
        Some(egui_wgpu::CallbackTextureCopy {
            texture: texture.clone(),
            origin: [0, 0],
        }),
        &Arc::new(AtomicUsize::new(0)),
        &Arc::new(AtomicUsize::new(0)),
        &Arc::new(AtomicUsize::new(0)),
    ));
    assert_frame(&state, &screen, &copied, [0.0; 4], &mut cache, None);
    assert_eq!(
        cache.stats().decline_reason,
        Some("image-copy callback cannot be retained in a complete frame")
    );
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(true));
}

struct ObservedPaint {
    key: Option<egui_wgpu::CallbackPaintKey>,
    copy: Option<egui_wgpu::CallbackTextureCopy>,
    prepares: Arc<AtomicUsize>,
    finishes: Arc<AtomicUsize>,
    paints: Arc<AtomicUsize>,
}

impl egui_wgpu::CallbackTrait for ObservedPaint {
    fn paint_key(&self) -> Option<egui_wgpu::CallbackPaintKey> {
        self.key.clone()
    }
    fn texture_copy(&self) -> Option<&egui_wgpu::CallbackTextureCopy> {
        self.copy.as_ref()
    }
    fn prepare(
        &self,
        _: &wgpu::Device,
        _: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        _: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        self.prepares.fetch_add(1, Ordering::Relaxed);
        Vec::new()
    }
    fn finish_prepare(
        &self,
        _: &wgpu::Device,
        _: &wgpu::Queue,
        _: &mut wgpu::CommandEncoder,
        _: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        self.finishes.fetch_add(1, Ordering::Relaxed);
        Vec::new()
    }
    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        _: &mut wgpu::RenderPass<'static>,
        _: &egui_wgpu::CallbackResources,
    ) {
        self.paints.fetch_add(1, Ordering::Relaxed);
    }
}

fn observed_job(
    key: Option<egui_wgpu::CallbackPaintKey>,
    copy: Option<egui_wgpu::CallbackTextureCopy>,
    prepares: &Arc<AtomicUsize>,
    finishes: &Arc<AtomicUsize>,
    paints: &Arc<AtomicUsize>,
) -> ClippedPrimitive {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(128.0, 96.0));
    ClippedPrimitive {
        clip_rect: rect,
        primitive: Primitive::Callback(egui_wgpu::Callback::new_paint_callback(
            rect,
            ObservedPaint {
                key,
                copy,
                prepares: prepares.clone(),
                finishes: finishes.clone(),
                paints: paints.clone(),
            },
        )),
    }
}

#[test]
fn retained_document_frames_keep_preparation_and_queued_images() {
    let (state, screen, mut jobs) = fixture();
    let mut cache = egui_wgpu::RetainedUi::default();
    let prepares = Arc::new(AtomicUsize::new(0));
    let finishes = Arc::new(AtomicUsize::new(0));
    let paints = Arc::new(AtomicUsize::new(0));
    let key = egui_wgpu::CallbackPaintKey {
        namespace: Arc::new(()),
        bytes: Arc::from([1]),
    };
    jobs.push(observed_job(
        Some(key.clone()),
        None,
        &prepares,
        &finishes,
        &paints,
    ));
    let initial = assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(true));
    assert_eq!(prepares.load(Ordering::Relaxed), 4);
    assert_eq!(finishes.load(Ordering::Relaxed), 4);
    assert_eq!(paints.load(Ordering::Relaxed), 3);
    for changed_key in [
        egui_wgpu::CallbackPaintKey {
            namespace: key.namespace.clone(),
            bytes: Arc::from([2]),
        },
        egui_wgpu::CallbackPaintKey {
            namespace: Arc::new(()),
            bytes: key.bytes.clone(),
        },
    ] {
        jobs[1] = observed_job(Some(changed_key), None, &prepares, &finishes, &paints);
        assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    }
    jobs[1].clip_rect = jobs[1].clip_rect.shrink(8.0);
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    let Primitive::Callback(callback) = &mut jobs[1].primitive else {
        unreachable!()
    };
    callback.rect = callback.rect.shrink(4.0);
    assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    let delayed_target = target(&state, screen.size_in_pixels, state.target_format, true);
    let pending = {
        let renderer = state.renderer.read();
        let mut encoder = state.device.create_command_encoder(&Default::default());
        assert_eq!(
            cache.try_render_frame(
                &renderer,
                &state.device,
                &mut encoder,
                &jobs,
                &screen,
                &delayed_target,
                [0.0; 4]
            ),
            Some(true)
        );
        encoder.finish()
    };
    let Primitive::Mesh(mesh) = &mut jobs[0].primitive else {
        unreachable!()
    };
    for vertex in &mut mesh.vertices {
        vertex.color = egui::Color32::RED;
    }
    let newer = assert_frame(&state, &screen, &jobs, [0.0; 4], &mut cache, Some(false));
    assert_ne!(initial, newer);
    cache.clear();
    assert_eq!(cache.stats().texture_bytes, 0);
    assert_eq!(cache.stats().signature_bytes, 0);
    drop(cache);
    state.queue.submit([pending]);
    profile::assert_same_pixels(
        &initial,
        &profile::read_image(&state, &delayed_target),
        "queued copies keep immutable images after rebuild and destruction",
    );
}
