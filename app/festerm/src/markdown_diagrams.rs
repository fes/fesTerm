use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, TryRecvError},
        Arc, OnceLock,
    },
};

use eframe::egui;

const MAX_SOURCE_BYTES: usize = 8 * 1024;
const MAX_SOURCE_ITEMS: usize = 512;
const MAX_DIAGRAMS: usize = 16;
const MAX_WORKERS: usize = 2;
const MAX_SVG_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGE_AXIS: u32 = 2048;
const MAX_IMAGE_PIXELS: u32 = 1024 * 1024;
static WORKERS: AtomicUsize = AtomicUsize::new(0);

struct Worker;

impl Worker {
    fn acquire() -> Option<Self> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_WORKERS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

enum Diagram {
    Pending(Receiver<Result<egui::ColorImage, &'static str>>),
    Ready(egui::TextureHandle),
    Failed(&'static str),
}

struct Entry {
    diagram: Diagram,
    source_visible: bool,
}

#[derive(Default)]
pub(crate) struct MermaidDiagrams {
    entries: BTreeMap<usize, Entry>,
}

impl MermaidDiagrams {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns whether the ordinary selectable source should also be drawn.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        start: usize,
        source: &str,
        reveal_source: bool,
    ) -> bool {
        if !self.entries.contains_key(&start) {
            if self.entries.len() >= MAX_DIAGRAMS {
                ui.label("Mermaid preview limit reached; showing source.");
                return true;
            }
            let diagram = if let Err(error) = validate_source(source) {
                Diagram::Failed(error)
            } else if let Some(worker) = Worker::acquire() {
                let source = source.to_owned();
                let context = ui.ctx().clone();
                let (sender, receiver) = mpsc::sync_channel(1);
                let spawned = std::thread::Builder::new()
                    .name("mermaid-preview".into())
                    .spawn(move || {
                        let result = std::panic::catch_unwind(|| rasterize(&source))
                            .unwrap_or(Err("Mermaid rendering failed; showing source."));
                        let _ = sender.send(result);
                        drop(worker);
                        context.request_repaint();
                    });
                if spawned.is_ok() {
                    Diagram::Pending(receiver)
                } else {
                    Diagram::Failed("Mermaid worker unavailable; showing source.")
                }
            } else {
                ui.label("Waiting to render Mermaid diagram...");
                // A worker's completion may wake a different window.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                return true;
            };
            self.entries.insert(
                start,
                Entry {
                    diagram,
                    source_visible: false,
                },
            );
        }
        let entry = self.entries.get_mut(&start).unwrap();
        if let Diagram::Pending(receiver) = &entry.diagram {
            match receiver.try_recv() {
                Ok(Ok(image)) => {
                    entry.diagram = Diagram::Ready(ui.ctx().load_texture(
                        "Mermaid diagram",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                Ok(Err(error)) => entry.diagram = Diagram::Failed(error),
                Err(TryRecvError::Disconnected) => {
                    entry.diagram = Diagram::Failed("Mermaid rendering failed; showing source.");
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        match &entry.diagram {
            Diagram::Ready(texture) => {
                ui.add(
                    egui::Image::new(texture)
                        .max_width(ui.available_width())
                        .alt_text("Mermaid diagram; use Show Mermaid source for text"),
                );
                ui.checkbox(&mut entry.source_visible, "Show Mermaid source");
                entry.source_visible || reveal_source
            }
            Diagram::Pending(_) => {
                ui.label("Rendering Mermaid diagram...");
                true
            }
            Diagram::Failed(error) => {
                ui.label(*error);
                true
            }
        }
    }
}

fn validate_source(source: &str) -> Result<(), &'static str> {
    if source.len() > MAX_SOURCE_BYTES
        || source.lines().count() > 128
        || source
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .filter(|s| !s.is_empty())
            .count()
            > MAX_SOURCE_ITEMS
    {
        return Err("Mermaid source exceeds preview limits; showing source.");
    }
    // Bound recursive syntax and numeric layout controls before entering the
    // third-party parser (including compact, single-line diagrams).
    let mut depth = 0usize;
    for ch in source.chars() {
        match ch {
            '[' | '{' | '(' => {
                depth += 1;
                if depth > 16 {
                    return Err("Mermaid nesting exceeds preview limits; showing source.");
                }
            }
            ']' | '}' | ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if source
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .any(|s| s.parse::<u32>().map_or(true, |n| n > 4096))
    {
        return Err("Mermaid numeric value exceeds preview limits; showing source.");
    }
    Ok(())
}

fn font_database() -> Arc<resvg::usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut fonts = resvg::usvg::fontdb::Database::new();
            fonts.load_font_data(
                include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
                    .to_vec(),
            );
            fonts.set_sans_serif_family("JetBrains Mono");
            fonts.set_serif_family("JetBrains Mono");
            fonts.set_monospace_family("JetBrains Mono");
            Arc::new(fonts)
        })
        .clone()
}

fn rasterize(source: &str) -> Result<egui::ColorImage, &'static str> {
    validate_source(source)?;
    let svg = mermaid_svg::render_with(
        source,
        &mermaid_svg::Theme::default_theme().with_font("JetBrains Mono"),
    )
    .map_err(|_| "Invalid or unsupported Mermaid syntax; showing source.")?;
    if svg.len() > MAX_SVG_BYTES {
        return Err("Mermaid output exceeds preview limits; showing source.");
    }
    let options = resvg::usvg::Options {
        fontdb: font_database(),
        // Generated SVG is rasterized only. Never resolve images, including
        // data URLs or filesystem references, or scan installed fonts.
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_str(&svg, &options)
        .map_err(|_| "Mermaid image could not be rendered; showing source.")?;
    let size = tree.size();
    let scale = (MAX_IMAGE_AXIS as f32 / size.width())
        .min(MAX_IMAGE_AXIS as f32 / size.height())
        .min((MAX_IMAGE_PIXELS as f32 / (size.width() * size.height())).sqrt())
        .min(1.0);
    let width = (size.width() * scale).floor().max(1.0) as u32;
    let height = (size.height() * scale).floor().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or("Mermaid image exceeds preview limits; showing source.")?;
    pixmap.fill(resvg::tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(egui::ColorImage::from_rgba_premultiplied(
        [width as usize, height as usize],
        pixmap.data(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable, Harness};

    #[test]
    fn mermaid_rasterizes_flowcharts_sequences_and_charts_offline() {
        for source in [
            "flowchart LR\n A[Start] --> B[Finish]",
            "sequenceDiagram\n Alice->>Bob: Hello",
            "pie\n \"Cats\" : 2\n \"Dogs\" : 3",
            "classDiagram\n Animal <|-- Duck",
            "stateDiagram-v2\n [*] --> Ready",
            "mindmap\n root\n  Child",
        ] {
            let image = rasterize(source).unwrap_or_else(|e| panic!("{source}: {e}"));
            assert!(image.width() > 1 && image.height() > 1);
            assert!(image.pixels.len() <= MAX_IMAGE_PIXELS as usize);
            assert!(image
                .pixels
                .iter()
                .any(|pixel| *pixel != egui::Color32::WHITE));
        }
    }

    #[test]
    fn mermaid_rejects_bad_syntax_and_excessive_work_without_echoing_source() {
        assert!(rasterize("not-a-diagram SECRET").is_err());
        for source in [
            "x".repeat(MAX_SOURCE_BYTES + 1),
            "A ".repeat(MAX_SOURCE_ITEMS + 1),
            "[".repeat(17),
            "packet-beta\n 0-99999999999999999999: huge".into(),
        ] {
            assert!(validate_source(&source).is_err());
        }
    }

    #[test]
    fn mermaid_generated_links_and_html_remain_inert_pixels() {
        let image =
            rasterize("flowchart LR\n A[Start] --> B[End]\n click A \"https://example.invalid/\"")
                .unwrap();
        assert!(!image.pixels.is_empty());
    }

    #[test]
    fn mermaid_preview_reuses_pixels_and_exposes_source_on_request_and_find() {
        let mut harness = Harness::builder().build_ui_state(
            |ui, (diagrams, reveal, source_visible): &mut (MermaidDiagrams, bool, bool)| {
                if diagrams.entries.is_empty() {
                    let image = rasterize("flowchart LR\n A --> B").unwrap();
                    diagrams.entries.insert(
                        0,
                        Entry {
                            diagram: Diagram::Ready(ui.ctx().load_texture(
                                "controlled diagram",
                                image,
                                Default::default(),
                            )),
                            source_visible: false,
                        },
                    );
                }
                *source_visible = diagrams.show(ui, 0, "flowchart LR\n A --> B", *reveal);
            },
            (MermaidDiagrams::default(), false, false),
        );
        harness.run();
        assert!(!harness.state().2);
        let Diagram::Ready(texture) = &harness.state().0.entries[&0].diagram else {
            panic!("expected prepared diagram");
        };
        let id = texture.id();
        harness.get_by_label("Show Mermaid source").click();
        harness.run();
        assert!(harness.state().2);
        harness.get_by_label("Show Mermaid source").click();
        harness.run();
        assert!(!harness.state().2);
        harness.state_mut().1 = true;
        harness.run();
        assert!(harness.state().2);
        let Diagram::Ready(texture) = &harness.state().0.entries[&0].diagram else {
            panic!("expected retained diagram");
        };
        assert_eq!(texture.id(), id);
    }
}
