//! Small, matched frontend workloads; never opens a window or installs a clipboard.
use super::{
    host_copy::{render_state, target},
    profile,
};
use crate::markdown_viewer::MarkdownPreviewPane;
use egui::epaint::Primitive;
use egui_kittest::{wgpu::WgpuTestRenderer, TestRenderer};
use festerm_markdown::LocalMarkdownSource;
use sha2::{Digest, Sha256};
use std::{io::Read, time::Instant};

const FRAMES: usize = 32;
const WARMUP: usize = 8;
const SCALE: f32 = 1.5;
const SIZE: [u32; 2] = [960, 600];

fn table(rows: usize) -> String {
    let mut text = "| Name | Value | Status |\n| --- | ---: | --- |\n".to_owned();
    for row in 0..rows {
        text.push_str(&format!("| Row {row} | {} | ready |\n", row * 7));
    }
    text
}

fn code(lines: usize, language: &str) -> String {
    let mut text = format!("```{language}\n");
    for line in 0..lines {
        text.push_str(&format!(
            "let value_{line} = {line} + 1; // owned payload\n"
        ));
    }
    text.push_str("```\n");
    text
}

fn fixtures() -> Vec<(String, String)> {
    let mut cases = vec![(
        "prose".to_owned(),
        "A small **owned** paragraph with a [local label](#end) and ordinary words.\n\n".repeat(16),
    )];
    for rows in [5, 15, 30] {
        cases.push((format!("table-{rows}"), table(rows)));
    }
    for lines in [20, 100, 300] {
        for language in ["", "rust"] {
            cases.push((
                format!(
                    "code-{}-{lines}",
                    if language.is_empty() {
                        "plain"
                    } else {
                        language
                    }
                ),
                code(lines, language),
            ));
        }
    }
    cases.push((
        "mixed".to_owned(),
        format!(
            "A **small** mixed preview.\n\n{}\n{}",
            table(5),
            code(20, "rust")
        ),
    ));
    cases
        .into_iter()
        .map(|(name, body)| (name, format!("# Start\n\n{body}\n# End\n\nOwned tail.\n")))
        .collect()
}

fn sha256(mut input: impl Read) -> String {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn build_receipt() -> serde_json::Value {
    let path = std::env::var_os("FESTERM_PREVIEW_NAVIGATION_BUILD_RECEIPT").expect(
        "supply an external source-bound build receipt; runtime HEAD is not build provenance",
    );
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    for (field, length) in [
        ("source_revision", 40),
        ("source_tree", 40),
        ("test_binary_sha256", 64),
        ("benchmark_sha256", 64),
    ] {
        let value = receipt[field].as_str().expect("receipt hash field");
        assert!(
            value.len() == length
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "invalid {field}"
        );
    }
    let binary = sha256(std::fs::File::open(std::env::current_exe().unwrap()).unwrap());
    let benchmark = sha256(&include_bytes!("preview_navigation.rs")[..]);
    assert_eq!(receipt["test_binary_sha256"], binary);
    assert_eq!(receipt["benchmark_sha256"], benchmark);
    // Whitelist fields: no executable/source paths or arbitrary receipt text in output.
    serde_json::json!({
        "source_revision": receipt["source_revision"],
        "source_tree": receipt["source_tree"],
        "test_binary_sha256": binary,
        "benchmark_sha256": benchmark,
        "source_attribution": "external-build-receipt; source/build procedure must be audited separately",
    })
}

#[derive(Clone, Copy)]
enum Navigation {
    Stationary,
    Wheel,
    PageWheel,
    Heading,
}

impl Navigation {
    fn label(self) -> &'static str {
        match self {
            Self::Stationary => "stationary",
            Self::Wheel => "wheel",
            Self::PageWheel => "page-wheel",
            Self::Heading => "heading",
        }
    }

    fn apply(
        self,
        frame: usize,
        pane: &mut MarkdownPreviewPane,
        input: &mut egui::RawInput,
    ) -> bool {
        match self {
            Self::Stationary => false,
            Self::Wheel | Self::PageWheel => {
                let down = (frame / 8).is_multiple_of(2);
                let (unit, distance) = if matches!(self, Self::Wheel) {
                    (egui::MouseWheelUnit::Point, 36.0)
                } else {
                    (egui::MouseWheelUnit::Page, 1.0)
                };
                input.events.push(egui::Event::MouseWheel {
                    unit,
                    delta: egui::vec2(0.0, if down { -distance } else { distance }),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                });
                true
            }
            Self::Heading => {
                if frame.is_multiple_of(8) {
                    pane.scroll_to_heading(usize::from((frame / 8).is_multiple_of(2)));
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[test]
#[ignore = "explicit opt-in small frontend navigation + completed offscreen DX12 CPU/WARP replay; not display latency"]
fn profile_small_preview_navigation() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let release = !cfg!(debug_assertions);
    assert!(release, "use a release test executable");
    let receipt = build_receipt();
    let state = render_state();
    let adapter = state.adapter.get_info();
    assert_eq!(adapter.backend, wgpu::Backend::Dx12);
    assert_eq!(adapter.device_type, wgpu::DeviceType::Cpu);
    assert_eq!(
        adapter.name, "Microsoft Basic Render Driver",
        "require WARP"
    );
    assert_eq!(adapter.vendor, 0x1414, "require Microsoft WARP vendor");
    assert_eq!(state.target_format, wgpu::TextureFormat::Bgra8Unorm);
    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: SIZE,
        pixels_per_point: SCALE,
    };
    let texture = target(&state, SIZE, state.target_format, true);
    let mut uploader = WgpuTestRenderer::from_render_state(state.clone());
    println!(
        "\n{}",
        serde_json::json!({
            "kind": "small-preview-navigation-metadata", "schema": 1,
            "build_receipt": receipt,
            "adapter": {"name": adapter.name, "backend": "Dx12", "device_type": "Cpu"},
            "os": "windows", "architecture": "x86_64", "release": release,
            "package_version": env!("CARGO_PKG_VERSION"),
            "physical_size": SIZE, "logical_size": [640, 400], "pixels_per_point": SCALE,
            "frames": FRAMES, "warmup_frames": WARMUP, "fixtures": 11,
            "route": "production MarkdownPreviewPane -> egui mesh -> completed offscreen WARP",
            "retained_frame_cache": false, "native_direct2d_terminal": false,
            "limitations": "Synthetic input, no native window/presentation/clipboard; not physical input-to-display latency. External balanced source controls required. Stationary controls include full UI construction/tessellation and ordinary replay, not an idle-event-loop measurement."
        })
    );
    let campaign_started = Instant::now();
    let mut moving_changes = [0_usize; 3];
    for (fixture, text) in fixtures() {
        for navigation in [
            Navigation::Stationary,
            Navigation::Wheel,
            Navigation::PageWheel,
            Navigation::Heading,
        ] {
            // A fresh pane/context for every mode gives identical initial layout and font state.
            let context = egui::Context::default();
            context.set_theme(egui::ThemePreference::Dark);
            context.set_visuals(festerm_ui_egui::theme::default_visuals());
            context.all_styles_mut(|style| {
                style.animation_time = 0.0;
                style.scroll_animation = egui::style::ScrollAnimation::none();
            });
            let mut pane = MarkdownPreviewPane::new(
                LocalMarkdownSource::new("owned-navigation-fixture.md")
                    .unwrap()
                    .into(),
                &text,
            );
            assert_eq!(pane.headings().len(), 2);
            let mut previous_offset = 0.0_f32;
            let mut real_scroll_changes = 0;
            let mut requests = 0;
            for step in 0..WARMUP + FRAMES {
                assert!(
                    campaign_started.elapsed().as_secs() < 180,
                    "three-minute workload budget exceeded; incomplete evidence is not a pass"
                );
                let measured = step >= WARMUP;
                let frame = step.saturating_sub(WARMUP);
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 400.0),
                    )),
                    time: Some(step as f64 / 60.0),
                    events: vec![egui::Event::PointerMoved(egui::pos2(320.0, 200.0))],
                    ..Default::default()
                };
                input
                    .viewports
                    .get_mut(&egui::ViewportId::ROOT)
                    .unwrap()
                    .native_pixels_per_point = Some(SCALE);
                let requested = measured && navigation.apply(frame, &mut pane, &mut input);
                let mut offset = None;
                let started = Instant::now();
                let mut output = context.run_ui(input, |ui| {
                    let id = ui.make_persistent_id("markdown-preview-pane");
                    pane.show(ui);
                    offset =
                        egui::scroll_area::State::load(ui.ctx(), id).map(|state| state.offset.y);
                });
                let build_ns = started.elapsed().as_nanos();
                assert_eq!(output.pixels_per_point, SCALE, "fixed viewport DPI");
                let offset = offset.expect("observe actual production ScrollArea state");
                assert!(offset.is_finite());
                let changed = (offset - previous_offset).abs() > 0.01;
                previous_offset = offset;
                let texture_sets = output.textures_delta.set.len();
                let texture_frees = output.textures_delta.free.len();
                let texture_bytes: usize = output
                    .textures_delta
                    .set
                    .iter()
                    .map(|(_, delta)| {
                        let [width, height] = delta.image.size();
                        width * height * 4
                    })
                    .sum();
                let started = Instant::now();
                let jobs = context.tessellate(output.shapes, SCALE);
                let tessellation_ns = started.elapsed().as_nanos();
                let mut vertices = 0;
                let mut indices = 0;
                let mut clips = Vec::new();
                for job in &jobs {
                    assert!(job.clip_rect.is_finite(), "finite clip");
                    if !clips.contains(&job.clip_rect) {
                        clips.push(job.clip_rect);
                    }
                    let Primitive::Mesh(mesh) = &job.primitive else {
                        panic!("unexpected callback: ordinary preview mesh route required")
                    };
                    vertices += mesh.vertices.len();
                    indices += mesh.indices.len();
                }
                assert!(
                    vertices > 0 && indices > 0,
                    "preview must emit real geometry"
                );
                let started = Instant::now();
                uploader.handle_delta(&mut output.textures_delta);
                // Include completed texture uploads, not just queue-write host return.
                state.queue.submit([]);
                state
                    .device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(std::time::Duration::from_secs(10)),
                    })
                    .unwrap();
                let texture_upload_completed_ns = started.elapsed().as_nanos();
                let before = state.renderer.read().mesh_upload_bytes();
                let cpu = festerm_windows_direct2d::process_cpu_time().unwrap();
                let started = Instant::now();
                assert_eq!(
                    profile::draw_composed(&state, &texture, &screen, &jobs, [0.0; 4], None),
                    (false, None),
                    "ordinary mesh route, no host-copy callback or retained-frame shortcut"
                );
                let render_completed_ns = started.elapsed().as_nanos();
                let render_process_cpu_ns =
                    (festerm_windows_direct2d::process_cpu_time().unwrap() - cpu).as_nanos();
                let staged_mesh_bytes = state.renderer.read().mesh_upload_bytes() - before;
                assert!(
                    staged_mesh_bytes > 0,
                    "ordinary replay must stage real meshes"
                );
                if measured {
                    requests += usize::from(requested);
                    real_scroll_changes += usize::from(changed);
                    println!(
                        "{}",
                        serde_json::json!({
                            "kind": "small-preview-navigation-frame", "fixture": fixture,
                            "navigation": navigation.label(), "frame": frame,
                            "requested": requested, "scroll_offset_y": offset, "scroll_changed": changed,
                            "ui_build_ns": build_ns, "tessellation_ns": tessellation_ns,
                            "texture_upload_completed_ns": texture_upload_completed_ns,
                            "render_completed_ns": render_completed_ns,
                            "render_process_cpu_ns": render_process_cpu_ns,
                            "jobs": jobs.len(), "unique_clip_rects": clips.len(),
                            "vertices": vertices, "indices": indices,
                            "texture_sets": texture_sets, "texture_frees": texture_frees,
                            "texture_delta_rgba_bytes": texture_bytes,
                            "staged_mesh_bytes": staged_mesh_bytes,
                        })
                    );
                }
            }
            if matches!(navigation, Navigation::Stationary) {
                assert_eq!(real_scroll_changes, 0, "stationary control moved");
            } else {
                let index = match navigation {
                    Navigation::Wheel => 0,
                    Navigation::PageWheel => 1,
                    Navigation::Heading => 2,
                    Navigation::Stationary => unreachable!(),
                };
                moving_changes[index] += real_scroll_changes;
            }
            println!(
                "{}",
                serde_json::json!({
                    "kind": "small-preview-navigation-summary", "fixture": fixture,
                    "navigation": navigation.label(), "frames": FRAMES,
                    "requests": requests, "real_scroll_changes": real_scroll_changes,
                    "no_effect_requests_possible": true,
                })
            );
        }
    }
    assert!(
        moving_changes.into_iter().all(|count| count > 0),
        "every navigation route must change real scroll offsets somewhere in the campaign"
    );
    println!(
        "{}",
        serde_json::json!({
            "kind": "small-preview-navigation-complete", "status": "pass",
            "measured_frames": 11 * 4 * FRAMES, "completed_draws": 11 * 4 * (WARMUP + FRAMES),
            "real_scroll_changes_by_route": moving_changes,
        })
    );
}
