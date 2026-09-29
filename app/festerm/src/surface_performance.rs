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
    preparation_ms: Option<f64>,
    warmup_ui_ms: Vec<f64>,
    warmup_tessellation_ms: Vec<f64>,
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
    for frame in 0..WARMUP_FRAMES + MEASURED_FRAMES {
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
        if frame >= WARMUP_FRAMES {
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
        preparation_ms: None,
        warmup_ui_ms,
        warmup_tessellation_ms,
        ui_median_ms: percentile(&mut ui_times, 50),
        ui_p95_ms: percentile(&mut ui_times, 95),
        tessellation_median_ms: percentile(&mut tessellation_times, 50),
        tessellation_p95_ms: percentile(&mut tessellation_times, 95),
        last_shape_count: shapes,
        last_vertex_count: vertices,
    }
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
    let fixtures = tempfile::tempdir().unwrap();
    let mut samples = Vec::new();
    let tab = TabId::next_for_test();

    let source: String = (0..2000)
        .map(|index| {
            format!("fn entry_{index}() {{ let value = {index}; println!(\"{{value}}\"); }}\n")
        })
        .collect();
    let source_path = fixtures.path().join("fixture.rs");
    std::fs::write(&source_path, &source).unwrap();
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
    }

    let markdown: String = (0..400)
        .map(|index| {
            format!(
                "## Section {index}\n\nA **synthetic** paragraph with `inline code` and ordinary text.\n\n\
                 ```rust\nfn example() {{ let value = {index}; }}\n```\n\n\
                 | Name | Value |\n| --- | --- |\n| Item | {index} |\n\n"
            )
        })
        .collect();
    let markdown_path = fixtures.path().join("fixture.md");
    std::fs::write(&markdown_path, &markdown).unwrap();
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

    let matches = viewer.set_find_query_for_test("e");
    assert_eq!(matches, markdown.match_indices("e").count());
    samples.push(measure("markdown-source-find", matches, |ui| {
        assert!(viewer.show(ui, tab).is_none());
    }));
    viewer.toggle_mode();
    samples.push(measure("markdown-preview-find", matches, |ui| {
        assert!(viewer.show(ui, tab).is_none());
    }));

    for name in [
        "markdown-preview-headings",
        "markdown-preview-prose",
        "markdown-preview-code",
        "markdown-preview-tables",
    ] {
        let text: String = (0..400)
            .map(|index| match name {
                "markdown-preview-headings" => format!("## Section {index}\n\n"),
                "markdown-preview-prose" => {
                    "A **synthetic** paragraph with `inline code` and ordinary text.\n\n".into()
                }
                "markdown-preview-code" => {
                    format!("```rust\nfn example() {{ let value = {index}; }}\n```\n\n")
                }
                "markdown-preview-tables" => {
                    format!("| Name | Value |\n| --- | --- |\n| Item | {index} |\n\n")
                }
                _ => unreachable!(),
            })
            .collect();
        let source = LocalMarkdownSource::new(fixtures.path().join(format!("{name}.md"))).unwrap();
        let mut pane = MarkdownPreviewPane::new(source.into(), &text);
        samples.push(measure(name, 400, |ui| pane.show(ui)));
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
    std::fs::write(&long_path, &long_line).unwrap();
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

    let report = serde_json::json!({
        "schema": "festerm-interactive-surface-profile-v1",
        "package_version": env!("CARGO_PKG_VERSION"),
        "release": !cfg!(debug_assertions),
        "viewport_points": [1180, 760],
        "pixels_per_point": 1,
        "warmup_frames": WARMUP_FRAMES,
        "measured_frames": MEASURED_FRAMES,
        "scope": "forced steady-state UI construction and tessellation; no GPU draw or presentation",
        "preparation_scope": "fixture reads and widget construction after writing synthetic input; warmup frames recorded separately; no native window",
        "samples": samples,
        "fenced_loading": fenced_loading,
        "syntax_preparation": syntax_preparation,
        "find_model": find_model,
    });
    let json = serde_json::to_string_pretty(&report).unwrap();
    std::fs::write(output.join("profile.json"), &json).unwrap();
    println!("{json}");
}
