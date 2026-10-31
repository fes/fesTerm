use eframe::egui;
use festerm_markdown::{
    Block, ListKind, LocalMarkdownSource, MarkdownCancellation, MarkdownDocument, MarkdownLoader,
    TaskState,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Workspace {
    #[default]
    Terminal,
    Files,
    Markdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LayoutTier {
    Wide,
    Compact,
    Minimal,
}

impl LayoutTier {
    pub(crate) fn for_size(size: egui::Vec2) -> Self {
        if size.y < 430.0 {
            Self::Minimal
        } else if size.x >= 700.0 && size.y >= 500.0 {
            Self::Wide
        } else {
            Self::Compact
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkdownMode {
    Preview,
    Source,
}

pub(crate) struct MarkdownWorkspace {
    name: String,
    location: String,
    document: MarkdownDocument,
    mode: MarkdownMode,
    contents_open: bool,
    selected_heading: usize,
    pending_heading: Option<usize>,
}

impl Default for MarkdownWorkspace {
    fn default() -> Self {
        let source_text = synthetic_markdown();
        let source = LocalMarkdownSource::new("/Documents/Nimbus-Runbook.md")
            .expect("fixed mobile Markdown path");
        let document = MarkdownLoader::default()
            .load(
                source.into(),
                source_text.len(),
                source_text.as_bytes(),
                &MarkdownCancellation::new(),
            )
            .expect("fixed mobile Markdown fixture");
        Self {
            name: "Nimbus-Runbook.md".to_owned(),
            location: "LOCAL snapshot".to_owned(),
            document,
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
        }
    }
}

impl MarkdownWorkspace {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Renders a file the user opened; `location` says where it came from.
    pub(crate) fn from_text(name: &str, location: &str, text: &str) -> Result<Self, String> {
        let source = LocalMarkdownSource::new(format!("/Documents/{name}"))
            .map_err(|_| "This file name cannot be previewed".to_owned())?;
        let document = MarkdownLoader::default()
            .load(
                source.into(),
                text.len(),
                text.as_bytes(),
                &MarkdownCancellation::new(),
            )
            .map_err(|error| format!("Could not render this file: {error}"))?;
        Ok(Self {
            name: name.to_owned(),
            location: location.to_owned(),
            document,
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
        })
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let tier = LayoutTier::for_size(ui.available_size());
        ui.horizontal(|ui| {
            ui.strong(&self.name);
            ui.small(&self.location);
        });
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, MarkdownMode::Preview, "Preview");
            ui.selectable_value(&mut self.mode, MarkdownMode::Source, "Source");
            if tier != LayoutTier::Wide
                && ui
                    .selectable_label(self.contents_open, "Contents")
                    .clicked()
            {
                self.contents_open = !self.contents_open;
            }
        });
        ui.separator();

        match self.mode {
            MarkdownMode::Source => self.show_source(ui),
            MarkdownMode::Preview if tier == LayoutTier::Wide => {
                let contents_width = 210.0;
                let height = ui.available_height();
                let (row, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), height),
                    egui::Sense::hover(),
                );
                let gap = ui.spacing().item_spacing.x;
                let contents =
                    egui::Rect::from_min_size(row.min, egui::vec2(contents_width, row.height()));
                let preview = egui::Rect::from_min_max(
                    egui::pos2(contents.right() + gap, row.top()),
                    row.max,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(contents), |ui| {
                    ui.set_clip_rect(contents);
                    self.show_contents(ui);
                });
                ui.scope_builder(egui::UiBuilder::new().max_rect(preview), |ui| {
                    ui.set_clip_rect(preview);
                    self.show_preview(ui);
                });
            }
            MarkdownMode::Preview => {
                if self.contents_open {
                    let height = (ui.available_height() * 0.34).clamp(110.0, 220.0);
                    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                        self.show_contents(ui);
                    });
                    ui.separator();
                }
                self.show_preview(ui);
            }
        }
    }

    fn show_contents(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.strong("CONTENTS");
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("mobile-markdown-contents")
                .show(ui, |ui| {
                    for (index, heading) in self.document.headings().iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.add_space(f32::from(heading.level().saturating_sub(1)) * 10.0);
                            if ui
                                .selectable_label(self.selected_heading == index, heading.text())
                                .clicked()
                            {
                                self.selected_heading = index;
                                self.pending_heading = Some(index);
                                self.contents_open = false;
                            }
                        });
                    }
                });
        });
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_heading;
        let pending = &mut self.pending_heading;
        let reading_width = ui.available_width().min(760.0);
        egui::ScrollArea::vertical()
            .id_salt("mobile-markdown-preview")
            .show(ui, |ui| {
                ui.set_max_width(reading_width);
                for block in self.document.blocks() {
                    show_markdown_block(ui, block, selected, pending, 0);
                    ui.add_space(8.0);
                }
            });
    }

    fn show_source(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::both()
            .id_salt("mobile-markdown-source")
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(self.document.source_text()).monospace())
                        .selectable(true),
                );
            });
    }
}

fn show_markdown_block(
    ui: &mut egui::Ui,
    block: &Block,
    selected_heading: usize,
    pending_heading: &mut Option<usize>,
    depth: usize,
) {
    match block {
        Block::Paragraph(paragraph) => {
            ui.label(paragraph.plain_text());
        }
        Block::Heading(heading) => {
            let size = match heading.level() {
                1 => 28.0,
                2 => 22.0,
                3 => 18.0,
                _ => 15.0,
            };
            let response = ui.label(
                egui::RichText::new(heading.plain_text())
                    .size(size)
                    .strong()
                    .color(if heading.heading_index() == selected_heading {
                        ui.visuals().hyperlink_color
                    } else {
                        ui.visuals().text_color()
                    }),
            );
            if *pending_heading == Some(heading.heading_index()) {
                response.scroll_to_me(Some(egui::Align::TOP));
                *pending_heading = None;
            }
        }
        Block::BlockQuote(quote) => {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    for nested in quote.blocks() {
                        show_markdown_block(
                            ui,
                            nested,
                            selected_heading,
                            pending_heading,
                            depth + 1,
                        );
                    }
                });
        }
        Block::List(list) => {
            for (index, item) in list.items().iter().enumerate() {
                ui.horizontal_top(|ui| {
                    ui.add_space(depth as f32 * 10.0);
                    let marker = match item.task_state() {
                        Some(TaskState::Checked) => "[x]".to_owned(),
                        Some(TaskState::Unchecked) => "[ ]".to_owned(),
                        None => match list.kind() {
                            ListKind::Bullet => "-".to_owned(),
                            ListKind::Ordered { first_item_number } => {
                                format!("{}.", first_item_number + index as u64)
                            }
                        },
                    };
                    ui.monospace(marker);
                    ui.vertical(|ui| {
                        for nested in item.blocks() {
                            show_markdown_block(
                                ui,
                                nested,
                                selected_heading,
                                pending_heading,
                                depth + 1,
                            );
                        }
                    });
                });
            }
        }
        Block::Table(table) => {
            egui::Grid::new(("mobile-markdown-table", block.span().start().byte_offset()))
                .striped(true)
                .show(ui, |ui| {
                    for row in table.rows() {
                        for cell in row.cells() {
                            if row.is_header() {
                                ui.strong(cell.plain_text());
                            } else {
                                ui.label(cell.plain_text());
                            }
                        }
                        ui.end_row();
                    }
                });
        }
        Block::CodeBlock(code) => {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    if let Some(language) = code.language() {
                        ui.small(language.to_ascii_uppercase());
                    }
                    ui.monospace(code.code_text());
                });
        }
        Block::Html(html) => {
            ui.monospace(html.literal());
        }
        Block::Rule { .. } => {
            ui.separator();
        }
    }
}

fn synthetic_markdown() -> &'static str {
    "# Nimbus Relay Runbook\n\n\
     This repository-owned fixture demonstrates the mobile Markdown reader. \
     It never opens a real file or network resource.\n\n\
     ## Before deployment\n\n\
     - [x] Run the test suite\n\
     - [ ] Confirm the release checksum\n\
     - [ ] Notify the on-call engineer\n\n\
     ## Deploy\n\n\
     Upload `release-2024.09.tar.gz`, then run:\n\n\
     ```sh\n\
     ./deploy.sh --environment staging\n\
     ```\n\n\
     ## Verify\n\n\
     | Check | Expected |\n\
     | --- | --- |\n\
     | Health | HTTP 200 |\n\
     | Queue depth | Below 100 |\n\n\
     ## Rollback\n\n\
     Restore the previous release and attach the deployment log to the incident."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_tiers_follow_measured_space_not_device_identity() {
        assert_eq!(
            LayoutTier::for_size(egui::vec2(1_194.0, 834.0)),
            LayoutTier::Wide
        );
        assert_eq!(
            LayoutTier::for_size(egui::vec2(834.0, 1_194.0)),
            LayoutTier::Wide
        );
        assert_eq!(
            LayoutTier::for_size(egui::vec2(390.0, 844.0)),
            LayoutTier::Compact
        );
        assert_eq!(
            LayoutTier::for_size(egui::vec2(844.0, 390.0)),
            LayoutTier::Minimal
        );
    }

    #[test]
    fn markdown_fixture_uses_shared_parser_and_has_navigable_contents() {
        let markdown = MarkdownWorkspace::default();
        let headings: Vec<_> = markdown
            .document
            .headings()
            .iter()
            .map(|heading| heading.text())
            .collect();
        assert_eq!(
            headings,
            [
                "Nimbus Relay Runbook",
                "Before deployment",
                "Deploy",
                "Verify",
                "Rollback"
            ]
        );
        assert_eq!(markdown.mode, MarkdownMode::Preview);
    }

    #[test]
    fn markdown_workspace_renders_opened_text_and_rejects_oversize_input() {
        let opened = MarkdownWorkspace::from_text("notes.md", "Remote · /srv", "# One\n\n## Two\n")
            .expect("renders");
        assert_eq!(opened.name(), "notes.md");
        assert_eq!(opened.document.headings().len(), 2);
        let huge = "x".repeat(festerm_markdown::MAX_SOURCE_BYTES + 1);
        assert!(MarkdownWorkspace::from_text("big.md", "x", &huge).is_err());
    }
}
