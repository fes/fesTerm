use std::{path::PathBuf, time::Instant};

use eframe::egui;
use festerm_markdown::{Block, LocalMarkdownSource, MarkdownCancellation, MarkdownLoader};
use festerm_ssh::{
    SftpDirectoryItem, SftpDirectorySnapshot, SftpEntryType, SftpLocation, SftpPath,
};
use festerm_syntax::{DocumentSyntax, Language, Role};
use serde::Serialize;

use crate::{
    documents::DocumentRegistry,
    markdown_viewer::{MarkdownPreviewPane, MarkdownViewerTab},
    performance_fixtures::{self, PerformanceFixtures},
    sftp_file_manager::SftpFileManagerTab,
    tabs::TabId,
    text_editor::{EditorMode, TextEditorTab},
};

const WARMUP_FRAMES: usize = 8;
const MEASURED_FRAMES: usize = 40;

#[derive(Serialize)]
struct Sample {
    name: &'static str,
    items: usize,
    viewport_points: [f32; 2],
    preparation_ms: Option<f64>,
    readiness_ms: Option<f64>,
    preparation_ui_ms: Vec<f64>,
    first_ui_ms: f64,
    first_tessellation_ms: f64,
    fixture_state_verified: bool,
    warmup_ui_ms: Vec<f64>,
    warmup_tessellation_ms: Vec<f64>,
    steady_ui_ms: Vec<f64>,
    steady_tessellation_ms: Vec<f64>,
    ui_median_ms: f64,
    ui_p95_ms: f64,
    tessellation_median_ms: f64,
    tessellation_p95_ms: f64,
    last_shape_count: usize,
    last_vertex_count: usize,
}

fn percentile(samples: &mut [f64], percent: usize) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[(samples.len() - 1) * percent / 100]
}

fn measure_prepared(
    name: &'static str,
    items: usize,
    preparation_started: Instant,
    show: impl FnMut(&mut egui::Ui),
) -> Sample {
    let preparation_ms = preparation_started.elapsed().as_secs_f64() * 1000.0;
    let mut sample = measure(name, items, show);
    sample.preparation_ms = Some(preparation_ms);
    sample
}

fn measure(name: &'static str, items: usize, mut show: impl FnMut(&mut egui::Ui)) -> Sample {
    let context = egui::Context::default();
    context.set_theme(egui::ThemePreference::Dark);
    context.set_visuals(festerm_ui_egui::theme::default_visuals());
    let mut ui_times = Vec::with_capacity(MEASURED_FRAMES);
    let mut tessellation_times = Vec::with_capacity(MEASURED_FRAMES);
    let mut warmup_ui_ms = Vec::with_capacity(WARMUP_FRAMES);
    let mut warmup_tessellation_ms = Vec::with_capacity(WARMUP_FRAMES);
    let mut shapes = 0;
    let mut vertices = 0;
    let mut first_ui_ms = 0.0;
    let mut first_tessellation_ms = 0.0;
    for frame in 0..1 + WARMUP_FRAMES + MEASURED_FRAMES {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1180.0, 760.0),
            )),
            time: Some(frame as f64 / 60.0),
            ..Default::default()
        };
        let start = Instant::now();
        let mut output = context.run_ui(input, &mut show);
        let ui_ms = start.elapsed().as_secs_f64() * 1000.0;
        output.textures_delta.clear();
        shapes = output.shapes.len();
        let start = Instant::now();
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        let tessellation_ms = start.elapsed().as_secs_f64() * 1000.0;
        vertices = primitives
            .iter()
            .map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum();
        std::hint::black_box(primitives);
        if frame == 0 {
            first_ui_ms = ui_ms;
            first_tessellation_ms = tessellation_ms;
        } else if frame > WARMUP_FRAMES {
            ui_times.push(ui_ms);
            tessellation_times.push(tessellation_ms);
        } else {
            warmup_ui_ms.push(ui_ms);
            warmup_tessellation_ms.push(tessellation_ms);
        }
    }
    Sample {
        name,
        items,
        viewport_points: [1180.0, 760.0],
        preparation_ms: None,
        readiness_ms: None,
        preparation_ui_ms: Vec::new(),
        first_ui_ms,
        first_tessellation_ms,
        fixture_state_verified: false,
        warmup_ui_ms,
        warmup_tessellation_ms,
        steady_ui_ms: ui_times.clone(),
        steady_tessellation_ms: tessellation_times.clone(),
        ui_median_ms: percentile(&mut ui_times, 50),
        ui_p95_ms: percentile(&mut ui_times, 95),
        tessellation_median_ms: percentile(&mut tessellation_times, 50),
        tessellation_p95_ms: percentile(&mut tessellation_times, 95),
        last_shape_count: shapes,
        last_vertex_count: vertices,
    }
}

pub(crate) fn timing_distribution(samples: &[f64]) -> serde_json::Value {
    assert!(!samples.is_empty());
    let mut sorted = samples.to_vec();
    let median = percentile(&mut sorted, 50);
    let p95 = percentile(&mut sorted, 95);
    serde_json::json!({
        "samples_ms": samples,
        "median_ms": median,
        "p95_ms": p95,
        "min_ms": sorted[0],
        "max_ms": sorted[sorted.len() - 1],
        "percentile_rule": "sorted[(len - 1) * percentile / 100]",
    })
}

fn measure_surface(scene: crate::ui_gallery::SurfaceScene, directory: &std::path::Path) -> Sample {
    let mut probe = crate::ui_gallery::SurfaceProbe::new(scene, directory, 1.0);
    let started = Instant::now();
    let primitives = probe.context.tessellate(
        probe.first_output.shapes.clone(),
        probe.first_output.pixels_per_point,
    );
    let first_tessellation_ms = started.elapsed().as_secs_f64() * 1000.0;
    std::hint::black_box(primitives);
    probe.prepare(scene.kind, |delta| delta.clear());
    let mut warmup_ui_ms = Vec::new();
    let mut warmup_tessellation_ms = Vec::new();
    let mut steady_ui_ms = Vec::new();
    let mut steady_tessellation_ms = Vec::new();
    let mut last_shape_count = 0;
    let mut last_vertex_count = 0;
    for frame in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let mut output = probe.frame();
        let ui_ms = probe.last_ui_ms;
        output.textures_delta.clear();
        last_shape_count = output.shapes.len();
        let started = Instant::now();
        let primitives = probe
            .context
            .tessellate(output.shapes, output.pixels_per_point);
        let tessellation_ms = started.elapsed().as_secs_f64() * 1000.0;
        last_vertex_count = primitives
            .iter()
            .map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum();
        std::hint::black_box(primitives);
        if frame < WARMUP_FRAMES {
            warmup_ui_ms.push(ui_ms);
            warmup_tessellation_ms.push(tessellation_ms);
        } else {
            steady_ui_ms.push(ui_ms);
            steady_tessellation_ms.push(tessellation_ms);
        }
        probe.assert_state(scene.kind);
    }
    Sample {
        name: scene.id,
        items: scene.items(),
        viewport_points: [scene.size().x, scene.size().y],
        preparation_ms: Some(probe.preparation_ms),
        readiness_ms: Some(probe.readiness_ms),
        preparation_ui_ms: probe.preparation_ui_ms,
        first_ui_ms: probe.first_ui_ms,
        first_tessellation_ms,
        fixture_state_verified: true,
        warmup_ui_ms,
        warmup_tessellation_ms,
        ui_median_ms: percentile(&mut steady_ui_ms.clone(), 50),
        ui_p95_ms: percentile(&mut steady_ui_ms.clone(), 95),
        tessellation_median_ms: percentile(&mut steady_tessellation_ms.clone(), 50),
        tessellation_p95_ms: percentile(&mut steady_tessellation_ms.clone(), 95),
        steady_ui_ms,
        steady_tessellation_ms,
        last_shape_count,
        last_vertex_count,
    }
}

#[test]
fn surface_timing_distributions_preserve_order_and_tail_without_budgets() {
    let samples = [9.0, 1.0, 2.0, 4.0, 30.0];
    let report = timing_distribution(&samples);
    assert_eq!(report["samples_ms"], serde_json::json!(samples));
    assert_eq!(report["median_ms"], 4.0);
    assert_eq!(report["p95_ms"], 9.0);
    assert_eq!(report["max_ms"], 30.0);
}

fn directory(location: SftpLocation, path: SftpPath, count: usize) -> SftpDirectorySnapshot {
    SftpDirectorySnapshot {
        location,
        entries: (0..count)
            .map(|index| {
                let name = format!("entry-{index:06}.txt");
                SftpDirectoryItem {
                    path: path.join_child(&name),
                    name,
                    file_type: SftpEntryType::File,
                    size: Some(index as u64 * 128),
                    modified_at: Some(std::time::UNIX_EPOCH),
                    permissions: Some(0o644),
                }
            })
            .collect(),
        path,
        loaded_at: std::time::UNIX_EPOCH,
    }
}

fn measure_fenced_loading(path: PathBuf, entries: usize) -> serde_json::Value {
    let rows: String = (0..entries)
        .map(|index| {
            let comma = if index + 1 < entries { "," } else { "" };
            format!("  \"entry_{index}\": {index}{comma}\n")
        })
        .collect();
    let code = format!("{{\n{rows}}}\n");
    assert!(code.len() < festerm_markdown::MAX_CODE_BLOCK_BYTES);
    let text = format!("```json\n{code}```\n");
    let source = LocalMarkdownSource::new(path).unwrap();
    let cancellation = MarkdownCancellation::new();
    let loader = MarkdownLoader::default();
    let mut times = Vec::with_capacity(MEASURED_FRAMES);
    let mut warmup_ms = Vec::with_capacity(WARMUP_FRAMES);
    let mut expected_pieces = None;
    for iteration in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let started = Instant::now();
        let document = loader
            .load(
                source.clone().into(),
                text.len(),
                text.as_bytes(),
                &cancellation,
            )
            .unwrap();
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(document.source_text(), text);
        let [Block::CodeBlock(block)] = document.blocks() else {
            panic!("the loading fixture must remain one fenced block");
        };
        assert_eq!(block.code_text(), code);
        let lines = block.highlighted_lines();
        assert_eq!(lines.len(), entries + 2);
        for (line_number, line) in lines[1..=entries].iter().enumerate() {
            assert!(
                line.spans()
                    .iter()
                    .any(|span| span.role() == Some(Role::StringLiteral))
                    && line
                        .spans()
                        .iter()
                        .any(|span| span.role() == Some(Role::Number)),
                "JSON fence ({entries} entries), iteration {iteration}, entry {line_number}: \
                 every entry must remain highlighted, not silently use a fallback"
            );
        }
        let pieces: usize = lines.iter().map(|line| line.spans().len()).sum();
        assert_eq!(*expected_pieces.get_or_insert(pieces), pieces);
        std::hint::black_box(document);
        if iteration >= WARMUP_FRAMES {
            times.push(elapsed);
        } else {
            warmup_ms.push(elapsed);
        }
    }
    serde_json::json!({
        "name": format!("json-fence-{entries}-entries"),
        "source_bytes": text.len(),
        "code_lines": entries + 2,
        "highlighted_pieces": expected_pieces.unwrap(),
        "warmup_load_ms": warmup_ms,
        "measured_loads": MEASURED_FRAMES,
        "load_median_ms": percentile(&mut times, 50),
        "load_p95_ms": percentile(&mut times, 95),
        "scope": "full MarkdownLoader projection from synthetic in-memory bytes; no disk, UI or native window"
    })
}

#[test]
#[ignore = "optional release UI construction/tessellation probe; not GPU or native latency"]
fn profile_interactive_surfaces() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let output = PathBuf::from(
        std::env::var_os("FESTERM_SURFACE_PROFILE_OUT").expect("set FESTERM_SURFACE_PROFILE_OUT"),
    );
    assert!(
        !output.exists(),
        "use a fresh performance evidence directory"
    );
    std::fs::create_dir_all(&output).unwrap();
    let mut fixtures = PerformanceFixtures::from_environment().unwrap();
    let mut samples = Vec::new();
    let tab = TabId::next_for_test();

    let source = performance_fixtures::rust_input();
    let source_path = fixtures.path().join("fixture.rs");
    fixtures.write_input(&source_path, &source).unwrap();
    let documents = DocumentRegistry::shared();
    let preparation_started = Instant::now();
    let document = documents.borrow_mut().open_local(&source_path).unwrap();
    let mut editor = TextEditorTab::new(document, &documents);
    samples.push(measure_prepared(
        "editor-syntax-2000-lines",
        2000,
        preparation_started,
        |ui| {
            assert!(editor.show(ui, tab, &documents).is_none());
        },
    ));
    fixtures
        .observe("editor-syntax-2000-lines", &source_path)
        .unwrap();
    let syntax_status = documents.borrow().get(document).unwrap().syntax_status();
    assert!(
        syntax_status.is_highlighted(),
        "the syntax fixture must not silently measure a fallback: {syntax_status:?}"
    );
    let preparation_started = Instant::now();
    editor.open_find_for_gallery("value", None);
    samples.push(measure_prepared(
        "editor-find-2000-capped-matches",
        2000,
        preparation_started,
        |ui| {
            assert!(editor.show(ui, tab, &documents).is_none());
        },
    ));
    fixtures
        .observe("editor-find-2000-capped-matches", &source_path)
        .unwrap();
    assert_eq!(editor.find_match_count_for_test(), 2000);

    // Keep controls before Markdown query work: its CPU duration must not
    // condition the timings of otherwise unchanged widgets later in the probe.
    for (name, count) in [
        ("sftp-100-rows-per-pane", 100),
        ("sftp-5000-rows-per-pane", 5000),
    ] {
        let context = egui::Context::default();
        let preparation_started = Instant::now();
        let mut browser = SftpFileManagerTab::for_gallery(
            "Performance fixture".into(),
            "test".into(),
            "fixture.example.com".into(),
            22,
            directory(SftpLocation::Local, SftpPath::local(fixtures.path()), count),
            directory(SftpLocation::Remote, SftpPath::remote("/fixture"), count),
            None,
            None,
            Default::default(),
            &context,
        );
        samples.push(measure_prepared(
            name,
            count * 2,
            preparation_started,
            |ui| {
                assert!(browser.show(ui, tab).is_none());
            },
        ));
        let actual_path = fixtures.path().to_owned();
        fixtures.observe(name, &actual_path).unwrap();
    }

    let markdown = performance_fixtures::markdown_input();
    let markdown_path = fixtures.path().join("fixture.md");
    fixtures.write_input(&markdown_path, &markdown).unwrap();
    let preparation_started = Instant::now();
    let document = documents.borrow_mut().open_local(&markdown_path).unwrap();
    let mut preview = TextEditorTab::new(document, &documents);
    preview.set_mode_for_gallery(EditorMode::Preview);
    samples.push(measure_prepared(
        "markdown-preview-400-sections",
        400,
        preparation_started,
        |ui| {
            assert!(preview.show(ui, tab, &documents).is_none());
        },
    ));
    fixtures
        .observe("markdown-preview-400-sections", &markdown_path)
        .unwrap();
    let viewer_path = markdown_path.clone();
    let preparation_started = Instant::now();
    let mut viewer = MarkdownViewerTab::open_local(markdown_path);
    viewer.toggle_mode();
    samples.push(measure_prepared(
        "markdown-source-400-sections",
        markdown.lines().count(),
        preparation_started,
        |ui| {
            assert!(viewer.show(ui, tab).is_none());
        },
    ));
    fixtures
        .observe("markdown-source-400-sections", &viewer_path)
        .unwrap();

    let matches = viewer.set_find_query_for_test("e");
    assert_eq!(matches, markdown.match_indices("e").count());
    samples.push(measure("markdown-source-find", matches, |ui| {
        assert!(viewer.show(ui, tab).is_none());
    }));
    fixtures
        .observe("markdown-source-find", &viewer_path)
        .unwrap();
    viewer.toggle_mode();
    samples.push(measure("markdown-preview-find", matches, |ui| {
        assert!(viewer.show(ui, tab).is_none());
    }));
    fixtures
        .observe("markdown-preview-find", &viewer_path)
        .unwrap();

    for name in [
        "markdown-preview-headings",
        "markdown-preview-prose",
        "markdown-preview-code",
        "markdown-preview-tables",
    ] {
        let text = performance_fixtures::fragment_input(name);
        let path = fixtures.path().join(format!("{name}.md"));
        fixtures.verify_fragment(&path, &text).unwrap();
        let source = LocalMarkdownSource::new(path.clone()).unwrap();
        let mut pane = MarkdownPreviewPane::new(source.into(), &text);
        samples.push(measure(name, 400, |ui| pane.show(ui)));
        fixtures.observe(name, &path).unwrap();
    }

    let fenced_loading: Vec<_> = [200, 2000, 4000]
        .map(|entries| measure_fenced_loading(fixtures.path().join("fence.md"), entries))
        .into();

    let mut syntax_times = Vec::with_capacity(MEASURED_FRAMES);
    for iteration in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let started = Instant::now();
        let syntax = DocumentSyntax::for_language(Language::Rust);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        assert!(syntax.status().is_highlighted());
        std::hint::black_box(syntax);
        if iteration >= WARMUP_FRAMES {
            syntax_times.push(elapsed);
        }
    }
    let syntax_preparation = serde_json::json!({
        "name": "rust-syntax-construction",
        "instances": MEASURED_FRAMES,
        "median_ms": percentile(&mut syntax_times, 50),
        "p95_ms": percentile(&mut syntax_times, 95),
        "scope": "fresh DocumentSyntax instances after widget scenes; parser/query setup, no source parsing"
    });

    let long_line = "e\u{301} ".repeat(20_000);
    let long_path = fixtures.path().join("long-line.md");
    fixtures.write_input(&long_path, &long_line).unwrap();
    let mut long_viewer = MarkdownViewerTab::open_local(long_path);
    let mut find_times = Vec::with_capacity(MEASURED_FRAMES);
    for iteration in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let started = Instant::now();
        let matches = long_viewer.set_find_query_for_test("e\u{301}");
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(matches, 20_000);
        if iteration >= WARMUP_FRAMES {
            find_times.push(elapsed);
        }
    }
    let find_model = serde_json::json!({
        "name": "markdown-find-long-unicode-line",
        "source_bytes": long_line.len(),
        "matches": 20_000,
        "median_ms": percentile(&mut find_times, 50),
        "p95_ms": percentile(&mut find_times, 95),
        "scope": "Find query and source-position construction; no UI or parsing"
    });

    // Append the new matrix after all original controls/model probes so they
    // retain their workload order and are not preconditioned by picker I/O.
    for scene in crate::ui_gallery::bounded_surface_scenes() {
        let scene_output = output.join(scene.id);
        std::fs::create_dir(&scene_output).unwrap();
        std::fs::write(scene_output.join("status.json"), r#"{"status":"running"}"#).unwrap();
        let sample = measure_surface(scene, &scene_output.join("fixtures"));
        std::fs::write(
            scene_output.join("profile.json"),
            serde_json::to_string_pretty(&sample).unwrap(),
        )
        .unwrap();
        std::fs::write(scene_output.join("status.json"), r#"{"status":"complete"}"#).unwrap();
        samples.push(sample);
    }

    let report = serde_json::json!({
        "schema": "festerm-interactive-surface-profile-v2",
        "package_version": env!("CARGO_PKG_VERSION"),
        "release": !cfg!(debug_assertions),
        "viewport_points": [1180, 760],
        "pixels_per_point": 1,
        "warmup_frames": WARMUP_FRAMES,
        "measured_frames": MEASURED_FRAMES,
        "scope": "forced first UI call, warmup and steady UI construction/tessellation; no GPU draw, native scheduling, input-to-display or presentation",
        "preparation_scope": "original controls retain model preparation after synthetic input writes; added matrix scenes include owned fixture creation/model setup; readiness separately includes interaction frames, bounded chip settle/scroll reveal, and real picker worker wait",
        "first_ui_scope": "one fresh-context UI call per scene, possibly loading/font-atlas setup; not cold-process start or first-ready latency; shared process caches can already be warm",
        "expanded_accessibility_scope": "expanded scenes enable AccessKit for semantic fixture guards; query-tree updates are excluded from UI timings; original controls retain their original context policy",
        "cold_process_start_ms": serde_json::Value::Null,
        "cold_process_start_status": "not measured by an already-running test binary; requires separately staged native process-start evidence",
        "provenance": crate::ui_gallery::surface_probe_provenance(),
        "samples": samples,
        "fenced_loading": fenced_loading,
        "syntax_preparation": syntax_preparation,
        "find_model": find_model,
    });
    let json = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(output.join("profile.json"), &json).unwrap();
    fixtures.finish(&output, Some(&report)).unwrap();
    println!("{json}");
}
