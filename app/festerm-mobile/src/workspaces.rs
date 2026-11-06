use eframe::egui;
use festerm_markdown::{
    Block, Inline, ListKind, LocalMarkdownSource, MarkdownCancellation, MarkdownDocument,
    MarkdownLoader, TaskState,
};
use std::sync::Arc;

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
    document: Arc<MarkdownDocument>,
    mode: MarkdownMode,
    contents_open: bool,
    selected_heading: usize,
    pending_heading: Option<usize>,
    preview_select_all: bool,
    preview_copy: Option<String>,
    preview_revision_changed: bool,
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
            document: Arc::new(document),
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
            preview_select_all: false,
            preview_copy: None,
            preview_revision_changed: true,
        }
    }
}

impl MarkdownWorkspace {
    pub(crate) fn from_shared(name: &str, location: &str, document: Arc<MarkdownDocument>) -> Self {
        Self {
            name: name.to_owned(),
            location: location.to_owned(),
            document,
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
            preview_select_all: false,
            preview_copy: None,
            preview_revision_changed: true,
        }
    }

    pub(crate) fn update_shared(&mut self, document: Arc<MarkdownDocument>) {
        if !Arc::ptr_eq(&self.document, &document) {
            self.document = document;
            self.selected_heading = 0;
            self.pending_heading = None;
            self.preview_select_all = false;
            self.preview_copy = None;
            self.preview_revision_changed = true;
        }
    }

    pub(crate) fn show_projection(&mut self, ui: &mut egui::Ui) {
        self.show_projection_tier(ui, LayoutTier::for_size(ui.available_size()));
    }

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
            document: Arc::new(document),
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
            preview_select_all: false,
            preview_copy: None,
            preview_revision_changed: true,
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
            MarkdownMode::Preview => self.show_projection_tier(ui, tier),
        }
    }

    fn show_projection_tier(&mut self, ui: &mut egui::Ui, tier: LayoutTier) {
        match tier {
            LayoutTier::Wide => {
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
                    self.show_contents(ui, true);
                });
                ui.scope_builder(egui::UiBuilder::new().max_rect(preview), |ui| {
                    ui.set_clip_rect(preview);
                    self.show_preview(ui);
                });
            }
            LayoutTier::Compact | LayoutTier::Minimal => {
                if self.contents_open {
                    let height = (ui.available_height() * 0.34).clamp(110.0, 220.0);
                    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                        self.show_contents(ui, true);
                    });
                    ui.separator();
                }
                self.show_preview(ui);
            }
        }
    }

    pub(crate) fn show_contents(&mut self, ui: &mut egui::Ui, heading: bool) -> bool {
        let mut navigated = false;
        egui::Frame::group(ui.style()).show(ui, |ui| {
            if heading {
                ui.strong("CONTENTS");
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .id_salt("mobile-markdown-contents")
                .show(ui, |ui| {
                    for (index, heading) in self.document.headings().iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().interact_size.y = super::theme::space::TOUCH;
                            ui.add_space(f32::from(heading.level().saturating_sub(1)) * 10.0);
                            if ui
                                .selectable_label(self.selected_heading == index, heading.text())
                                .clicked()
                            {
                                self.selected_heading = index;
                                self.pending_heading = Some(index);
                                self.contents_open = false;
                                navigated = true;
                            }
                        });
                    }
                });
        });
        navigated
    }

    fn show_preview(&mut self, ui: &mut egui::Ui) {
        let selected = self.selected_heading;
        let pending = &mut self.pending_heading;
        let reading_width = ui.available_width().min(760.0);
        let plugin = ui
            .ctx()
            .plugin::<egui::text_selection::LabelSelectionState>();
        let viewport = ui.ctx().viewport_id();
        {
            let mut labels = plugin.lock();
            if std::mem::take(&mut self.preview_revision_changed) {
                labels.clear_selection();
            }
            labels.begin_preserve_selection_on_secondary_press(viewport);
            labels.preserve_selection_on_touch(viewport);
            labels.capture_selection_text(viewport);
        }
        let mut selection = PreviewSelection {
            all: self.preview_select_all,
            ..Default::default()
        };
        let preview = ui.scope(|ui| {
            egui::ScrollArea::vertical()
                .id_salt("mobile-markdown-preview")
                .show(ui, |ui| {
                    ui.set_max_width(reading_width);
                    for block in self.document.blocks() {
                        show_markdown_block(ui, block, selected, pending, 0, &mut selection);
                        ui.add_space(8.0);
                    }
                });
        });
        {
            let mut labels = plugin.lock();
            labels.end_preserve_selection_on_secondary_press(viewport, selection.menu);
            if selection.menu {
                let ranges = labels.selected_label_ranges(viewport);
                self.preview_copy = if selection.all {
                    Some(preview_text(self.document.blocks()))
                } else if !ranges.is_empty()
                    && ranges.iter().all(|(id, _)| selection.widgets.contains(id))
                {
                    labels.pending_copy_text(viewport).map(str::to_owned)
                } else {
                    None
                };
            }
        }
        self.preview_select_all = selection.all;
        let popup = egui::Popup::context_menu(&preview.response)
            .open_memory(selection.menu.then_some(egui::SetOpenCommand::Bool(true)))
            .show(|ui| {
                ui.spacing_mut().interact_size.y = super::theme::space::TOUCH;
                if ui
                    .add_enabled(self.preview_copy.is_some(), egui::Button::new("Copy"))
                    .clicked()
                {
                    ui.copy_text(self.preview_copy.as_ref().expect("copy selection").clone());
                    ui.close();
                }
                if ui.button("Select All").clicked() {
                    plugin.lock().clear_selection();
                    self.preview_select_all = true;
                    ui.close();
                }
            });
        if popup.is_none() {
            self.preview_copy = None;
        }
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

#[derive(Default)]
struct PreviewSelection {
    all: bool,
    widgets: Vec<egui::Id>,
    menu: bool,
}

fn preview_label(
    ui: &mut egui::Ui,
    text: egui::RichText,
    wrap: bool,
    selection: &mut PreviewSelection,
) -> egui::Response {
    let text = if selection.all {
        text.background_color(ui.visuals().selection.bg_fill)
            .color(ui.visuals().selection.stroke.color)
    } else {
        text
    };
    let response = ui.add(
        egui::Label::new(text)
            .wrap_mode(if wrap {
                egui::TextWrapMode::Wrap
            } else {
                egui::TextWrapMode::Extend
            })
            .selectable(true),
    );
    selection.widgets.push(response.id);
    selection.menu |= response.secondary_clicked();
    if !response.secondary_clicked() && (response.clicked() || response.drag_started()) {
        selection.all = false;
    }
    response
}

fn show_markdown_block(
    ui: &mut egui::Ui,
    block: &Block,
    selected_heading: usize,
    pending_heading: &mut Option<usize>,
    depth: usize,
    selection: &mut PreviewSelection,
) {
    match block {
        Block::Paragraph(paragraph) => {
            preview_label(
                ui,
                egui::RichText::new(flowing_text(paragraph.inlines())),
                true,
                selection,
            );
        }
        Block::Heading(heading) => {
            let size = match heading.level() {
                1 => 28.0,
                2 => 22.0,
                3 => 18.0,
                _ => 15.0,
            };
            let response = preview_label(
                ui,
                egui::RichText::new(flowing_text(heading.inlines()))
                    .size(size)
                    .strong()
                    .color(if heading.heading_index() == selected_heading {
                        ui.visuals().hyperlink_color
                    } else {
                        ui.visuals().text_color()
                    }),
                true,
                selection,
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
                            selection,
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
                    preview_label(
                        ui,
                        egui::RichText::new(marker).monospace(),
                        false,
                        selection,
                    );
                    ui.vertical(|ui| {
                        for nested in item.blocks() {
                            show_markdown_block(
                                ui,
                                nested,
                                selected_heading,
                                pending_heading,
                                depth + 1,
                                selection,
                            );
                        }
                    });
                });
            }
        }
        Block::Table(table) => {
            let mut widths = Vec::<f32>::new();
            for row in table.rows() {
                for (column, cell) in row.cells().iter().enumerate() {
                    if widths.len() <= column {
                        widths.push(112.0);
                    }
                    let natural = cell
                        .plain_text()
                        .lines()
                        .map(|line| line.chars().count().min(40))
                        .max()
                        .unwrap_or(0) as f32
                        * 8.0;
                    widths[column] = widths[column].max(natural.clamp(112.0, 320.0));
                }
            }
            wide_block(ui, block, |ui| {
                egui::Grid::new(("mobile-markdown-table", block.span().start().byte_offset()))
                    .striped(true)
                    .show(ui, |ui| {
                        for row in table.rows() {
                            for (column, cell) in row.cells().iter().enumerate() {
                                ui.allocate_ui(egui::vec2(widths[column], 0.0), |ui| {
                                    let text = egui::RichText::new(flowing_text(cell.inlines()));
                                    preview_label(
                                        ui,
                                        if row.is_header() { text.strong() } else { text },
                                        true,
                                        selection,
                                    );
                                });
                            }
                            ui.end_row();
                        }
                    });
            });
        }
        Block::CodeBlock(code) => {
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    if let Some(language) = code.language() {
                        ui.small(language.to_ascii_uppercase());
                    }
                    wide_block(ui, block, |ui| {
                        preview_label(
                            ui,
                            egui::RichText::new(code.code_text()).monospace(),
                            false,
                            selection,
                        );
                    });
                });
        }
        Block::Html(html) => {
            preview_label(
                ui,
                egui::RichText::new(html.literal()).monospace(),
                true,
                selection,
            );
        }
        Block::Rule { .. } => {
            ui.separator();
        }
    }
}

fn preview_text(blocks: &[Block]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        let text = match block {
            Block::Paragraph(value) => flowing_text(value.inlines()),
            Block::Heading(value) => flowing_text(value.inlines()),
            Block::BlockQuote(value) => preview_text(value.blocks()),
            Block::List(value) => value
                .items()
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let marker = match item.task_state() {
                        Some(TaskState::Checked) => "[x]".to_owned(),
                        Some(TaskState::Unchecked) => "[ ]".to_owned(),
                        None => match value.kind() {
                            ListKind::Bullet => "-".to_owned(),
                            ListKind::Ordered { first_item_number } => {
                                format!("{}.", first_item_number + index as u64)
                            }
                        },
                    };
                    format!("{marker} {}", preview_text(item.blocks()))
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Block::Table(value) => value
                .rows()
                .iter()
                .map(|row| {
                    row.cells()
                        .iter()
                        .map(|cell| flowing_text(cell.inlines()))
                        .collect::<Vec<_>>()
                        .join("\t")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Block::CodeBlock(value) => value.code_text().to_owned(),
            Block::Html(value) => value.literal().to_owned(),
            Block::Rule { .. } => continue,
        };
        parts.push(text);
    }
    parts.join("\n\n")
}

fn flowing_text(inlines: &[Inline]) -> String {
    let mut text = String::new();
    for inline in inlines {
        match inline {
            Inline::Text(value) | Inline::Code(value) => text.push_str(value.text()),
            Inline::Emphasis(value) | Inline::Strong(value) | Inline::Strikethrough(value) => {
                text.push_str(&flowing_text(value.inlines()))
            }
            Inline::Link(value) => text.push_str(&flowing_text(value.inlines())),
            Inline::Image(value) => text.push_str(value.alt_text()),
            Inline::RawHtml(value) => text.push_str(value.literal()),
            Inline::SoftBreak { .. } => text.push(' '),
            Inline::HardBreak { .. } => text.push('\n'),
        }
    }
    text
}

fn wide_block(ui: &mut egui::Ui, block: &Block, content: impl FnOnce(&mut egui::Ui)) {
    let output = egui::ScrollArea::horizontal()
        .id_salt(("markdown-overflow", block.span().start().byte_offset()))
        .min_scrolled_width(0.0)
        .auto_shrink([false, true])
        .show(ui, |ui| ui.scope(content).response.rect);
    if output.state.offset.x + output.inner_rect.width() + 1.0 < output.content_size.x {
        let visible = output
            .inner
            .intersect(output.inner_rect)
            .intersect(ui.clip_rect());
        if visible.is_positive() {
            super::widgets::paint_chevron(
                ui.painter(),
                visible.right_center() - egui::vec2(4.0, 0.0),
                3.0,
                ui.visuals().weak_text_color(),
            );
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
    fn preview_partial_selection_survives_touch_copy_but_not_projection_replacement() {
        use egui_kittest::{kittest::Queryable, Harness};
        let workspace =
            MarkdownWorkspace::from_text("copy.md", "synthetic", "# Heading\n\nAlpha soft")
                .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, state: &mut (MarkdownWorkspace, Vec<String>)| {
                    state.0.show_projection(ui);
                    ui.ctx().output(|output| {
                        for command in &output.commands {
                            if let egui::OutputCommand::CopyText(text) = command {
                                state.1.push(text.clone());
                            }
                        }
                    });
                },
                (workspace, Vec::new()),
            );
        harness.run();
        let label = harness.get_by_label("Alpha soft").rect();
        let pos = egui::pos2(label.left() + 10.0, label.center().y);
        let now = harness.ctx.input(|input| input.time);
        for click in 0..2 {
            harness.input_mut().time = Some(now + 0.1 * f64::from(click + 1));
            for pressed in [true, false] {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::PointerMoved(pos));
                harness.input_mut().events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
        }
        harness.run();
        let plugin = harness
            .ctx
            .plugin::<egui::text_selection::LabelSelectionState>();
        assert!(plugin.lock().has_selection());
        assert!(plugin
            .lock()
            .selected_label_ranges(harness.ctx.viewport_id())
            .iter()
            .any(|(_, range)| !range.is_empty()));
        let now = harness.ctx.input(|input| input.time);
        for (elapsed, phase, pressed) in [
            (0.01, egui::TouchPhase::Start, Some(true)),
            (0.3, egui::TouchPhase::Move, None),
            (0.95, egui::TouchPhase::Move, None),
            (1.05, egui::TouchPhase::End, Some(false)),
        ] {
            let input = harness.input_mut();
            input.time = Some(now + elapsed);
            input.events.push(egui::Event::Touch {
                device_id: egui::TouchDeviceId(0),
                id: egui::TouchId(1),
                phase,
                pos,
                force: None,
            });
            input.events.push(egui::Event::PointerMoved(pos));
            if let Some(pressed) = pressed {
                input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
        }
        harness.run();
        assert_eq!(harness.state().0.preview_copy.as_deref(), Some("Alpha"),);
        harness.get_by_label("Copy").click();
        harness.run();
        assert_eq!(harness.state().1, ["Alpha"]);
        let now = harness.ctx.input(|input| input.time);
        for click in 0..2 {
            harness.input_mut().time = Some(now + 0.1 * f64::from(click + 1));
            for pressed in [true, false] {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::PointerMoved(pos));
                harness.input_mut().events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.step();
        }
        harness.run();
        assert!(plugin.lock().has_selection());
        let replacement =
            MarkdownWorkspace::from_text("copy.md", "synthetic", "# Changed\n\nNewer text")
                .unwrap();
        harness.state_mut().0.update_shared(replacement.document);
        harness.run();
        assert!(!plugin.lock().has_selection());
        assert!(harness.state().0.preview_copy.is_none());
    }

    #[test]
    fn preview_code_blocks_scroll_independently_inside_the_viewport() {
        use egui_kittest::{kittest::Queryable, Harness};
        let first =
            "first_unbroken_code_line_that_must_scroll_horizontally_without_expanding_the_viewport";
        let second =
            "second_unbroken_code_line_that_must_have_an_independent_horizontal_scroll_offset";
        let workspace = MarkdownWorkspace::from_text(
            "code.md",
            "synthetic",
            &format!("```text\n{first}\n```\n\n```text\n{second}\n```\n"),
        )
        .unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(320.0, 568.0))
            .build_ui_state(
                |ui, workspace: &mut MarkdownWorkspace| workspace.show_projection(ui),
                workspace,
            );
        harness.run();
        let first = format!("{first}\n");
        let second = format!("{second}\n");
        let before = harness.get_by_label(&first).rect();
        let second_before = harness.get_by_label(&second).rect().left();
        harness.event(egui::Event::PointerMoved(egui::pos2(
            150.0,
            before.center().y,
        )));
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-100.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        assert!(harness.get_by_label(&first).rect().left() < before.left());
        assert_eq!(harness.get_by_label(&second).rect().left(), second_before);
        assert_eq!(harness.ctx.content_rect().width(), 320.0);
    }

    #[test]
    fn preview_touch_menu_selects_and_copies_all_rendered_text_without_markdown() {
        use egui_kittest::{kittest::Queryable, Harness};
        let workspace = MarkdownWorkspace::from_text(
            "copy.md", "synthetic",
            "# Heading\n\nAlpha\n**soft**\n\n- item\n\n| A | B |\n| --- | --- |\n| one | two |\n\n```sh\necho ok\n```\n",
        ).unwrap();
        let expected = "Heading\n\nAlpha soft\n\n- item\n\nA\tB\none\ttwo\n\necho ok\n";
        assert_eq!(preview_text(workspace.document.blocks()), expected);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(390.0, 844.0))
            .build_ui_state(
                |ui, state: &mut (MarkdownWorkspace, Vec<String>)| {
                    state.0.show_projection(ui);
                    ui.ctx().output(|output| {
                        for command in &output.commands {
                            if let egui::OutputCommand::CopyText(text) = command {
                                state.1.push(text.clone());
                            }
                        }
                    });
                },
                (workspace, Vec::new()),
            );
        harness.run();
        for label in ["Select All", "Copy"] {
            let pos = harness.get_by_label("Alpha soft").rect().center();
            if label == "Select All" {
                let now = harness.ctx.input(|input| input.time);
                for (elapsed, phase, pressed) in [
                    (0.01, egui::TouchPhase::Start, Some(true)),
                    (0.3, egui::TouchPhase::Move, None),
                    (0.95, egui::TouchPhase::Move, None),
                    (1.05, egui::TouchPhase::End, Some(false)),
                ] {
                    let input = harness.input_mut();
                    input.time = Some(now + elapsed);
                    input.events.push(egui::Event::Touch {
                        device_id: egui::TouchDeviceId(0),
                        id: egui::TouchId(1),
                        phase,
                        pos,
                        force: None,
                    });
                    input.events.push(egui::Event::PointerMoved(pos));
                    if let Some(pressed) = pressed {
                        input.events.push(egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        });
                    }
                    harness.step();
                }
            } else {
                for pressed in [true, false] {
                    harness.event(egui::Event::PointerMoved(pos));
                    harness.event(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Secondary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                    harness.step();
                }
            }
            harness.run();
            harness.get_by_label(label).click();
            harness.run();
        }
        assert_eq!(harness.state().1, [expected]);
        assert!(harness.output().platform_output.ime.is_none());
        let replacement = MarkdownWorkspace::from_text("new.md", "synthetic", "changed").unwrap();
        harness.state_mut().0.update_shared(replacement.document);
        assert!(!harness.state().0.preview_select_all);
        assert!(harness.state().0.preview_copy.is_none());
    }

    #[test]
    fn preview_wide_tables_scroll_independently_without_expanding_the_viewport() {
        use egui_kittest::{kittest::Queryable, Harness};
        let workspace = MarkdownWorkspace::from_text(
            "tables.md", "synthetic",
            "| First A | First B | First C |\n| --- | --- | --- |\n| A long column with readable wrapping and bounded width | Another long column with readable wrapping and bounded width | Third long column with readable wrapping and bounded width |\n\n| Second A | Second B | Second C |\n| --- | --- | --- |\n| A long second table column with wrapping | Another long second table column with wrapping | Third long second table column with wrapping |\n\n```text\nan_unbroken_code_line_that_must_scroll_horizontally_instead_of_wrapping_or_expanding_the_viewport\n```\n",
        ).unwrap();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(320.0, 568.0))
            .build_ui_state(
                |ui, workspace: &mut MarkdownWorkspace| workspace.show_projection(ui),
                workspace,
            );
        harness.run();
        let first = harness.get_by_label("First A").rect();
        let second = harness.get_by_label("Second A").rect();
        harness.event(egui::Event::PointerMoved(first.center()));
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-100.0, 0.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        assert!(harness.get_by_label("First A").rect().left() < first.left());
        assert_eq!(
            harness.get_by_label("Second A").rect().left(),
            second.left()
        );
        assert_eq!(harness.ctx.content_rect().width(), 320.0);
    }

    #[test]
    fn preview_soft_newlines_flow_but_explicit_breaks_and_blocks_remain() {
        let workspace = MarkdownWorkspace::from_text(
            "breaks.md", "synthetic",
            "# A *heading*\n\nRevision 5 is not an\n**implementation** commitment.  \nHard break.\n\n- first\n  continuation\n\n```text\nkeep\nnewlines\n```\n",
        ).unwrap();
        let blocks = workspace.document.blocks();
        let Block::Paragraph(paragraph) = &blocks[1] else {
            panic!("paragraph")
        };
        assert_eq!(
            flowing_text(paragraph.inlines()),
            "Revision 5 is not an implementation commitment.\nHard break."
        );
        let Block::Heading(heading) = &blocks[0] else {
            panic!("heading")
        };
        assert_eq!(flowing_text(heading.inlines()), "A heading");
        let Block::CodeBlock(code) = &blocks[3] else {
            panic!("code")
        };
        assert_eq!(code.code_text(), "keep\nnewlines\n");
    }

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

    #[test]
    fn shared_markdown_views_reuse_projection_but_keep_navigation_independent() {
        let mut left = MarkdownWorkspace::default();
        let mut right = MarkdownWorkspace::from_shared("notes.md", "Local", left.document.clone());
        assert!(Arc::ptr_eq(&left.document, &right.document));
        left.selected_heading = 2;
        left.pending_heading = Some(2);
        assert_eq!(right.selected_heading, 0);
        assert_eq!(right.pending_heading, None);
        right.update_shared(left.document.clone());
        assert_eq!(left.pending_heading, Some(2));
        let next = MarkdownWorkspace::from_text("notes.md", "Local", "# Changed\n").unwrap();
        left.update_shared(next.document.clone());
        right.update_shared(next.document.clone());
        assert!(Arc::ptr_eq(&left.document, &right.document));
        assert_eq!(left.selected_heading, 0);
        assert_eq!(left.pending_heading, None);
        assert_eq!(left.document.source_text(), "# Changed\n");
    }
}
