use eframe::egui;
use festerm_markdown::{
    Block, ListKind, LocalMarkdownSource, MarkdownCancellation, MarkdownDocument, MarkdownLoader,
    TaskState,
};

const MAX_TRANSFER_HISTORY: usize = 8;
const TRANSFER_SECONDS: f64 = 2.5;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Workspace {
    #[default]
    Terminal,
    Files,
    Markdown,
}

impl Workspace {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Markdown => "README",
        }
    }
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

    const fn label(self) -> &'static str {
        match self {
            Self::Wide => "Wide",
            Self::Compact => "Compact",
            Self::Minimal => "Minimal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Location {
    Local,
    Remote,
}

impl Location {
    const fn label(self) -> &'static str {
        match self {
            Self::Local => "LOCAL",
            Self::Remote => "REMOTE",
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileEntry {
    name: &'static str,
    kind: EntryKind,
    size: Option<u64>,
}

impl FileEntry {
    const fn directory(name: &'static str) -> Self {
        Self {
            name,
            kind: EntryKind::Directory,
            size: None,
        }
    }

    const fn file(name: &'static str, size: u64) -> Self {
        Self {
            name,
            kind: EntryKind::File,
            size: Some(size),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Transfer {
    name: String,
    from: Location,
    progress: f32,
}

impl Transfer {
    fn complete(&self) -> bool {
        self.progress >= 1.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Collision {
    name: &'static str,
    from: Location,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CollisionDecision {
    Replace,
    KeepBoth,
    Skip,
}

pub(crate) struct FilesWorkspace {
    local: Vec<FileEntry>,
    remote: Vec<FileEntry>,
    selected_location: Location,
    local_selected: Option<usize>,
    remote_selected: Option<usize>,
    focused_pane: Location,
    transfers: Vec<Transfer>,
    collision: Option<Collision>,
    last_tick: Option<f64>,
    last_local_rect: Option<egui::Rect>,
    last_remote_rect: Option<egui::Rect>,
    last_collision_rect: Option<egui::Rect>,
    last_queue_rect: Option<egui::Rect>,
}

impl Default for FilesWorkspace {
    fn default() -> Self {
        Self {
            local: vec![
                FileEntry::directory("src"),
                FileEntry::directory("target"),
                FileEntry::file("Cargo.toml", 612),
                FileEntry::file("deploy.sh", 1_875),
                FileEntry::file("README.md", 4_204),
            ],
            remote: vec![
                FileEntry::directory("logs"),
                FileEntry::file("config.yaml", 512),
                FileEntry::file("README.md", 3_810),
                FileEntry::file("release-2024.09.tar.gz", 4_831_201),
            ],
            selected_location: Location::Local,
            local_selected: Some(3),
            remote_selected: None,
            focused_pane: Location::Local,
            transfers: Vec::new(),
            collision: None,
            last_tick: None,
            last_local_rect: None,
            last_remote_rect: None,
            last_collision_rect: None,
            last_queue_rect: None,
        }
    }
}

impl FilesWorkspace {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let tier = LayoutTier::for_size(ui.available_size());
        self.tick(ui.input(|input| input.time));
        self.last_local_rect = None;
        self.last_remote_rect = None;
        self.last_collision_rect = None;
        self.last_queue_rect = None;

        ui.horizontal(|ui| {
            ui.strong("SFTP workspace");
            ui.small(format!("{} layout", tier.label()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small("Offline facsimile");
            });
        });
        ui.separator();

        let collision_height = if self.collision.is_some() { 76.0 } else { 0.0 };
        let queue_height = if self.transfers.is_empty() {
            0.0
        } else {
            28.0 + self.transfers.len().min(3) as f32 * 28.0
        };
        let pane_height = (ui.available_height() - collision_height - queue_height).max(48.0);
        match tier {
            LayoutTier::Wide => {
                let (row, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), pane_height),
                    egui::Sense::hover(),
                );
                let gap = ui.spacing().item_spacing.x;
                let rail_width = 190.0;
                let pane_width = ((row.width() - rail_width - gap * 2.0) / 2.0).max(180.0);
                let local =
                    egui::Rect::from_min_size(row.min, egui::vec2(pane_width, row.height()));
                let rail = egui::Rect::from_min_size(
                    egui::pos2(local.right() + gap, row.top()),
                    egui::vec2(rail_width, row.height()),
                );
                let remote = egui::Rect::from_min_size(
                    egui::pos2(rail.right() + gap, row.top()),
                    egui::vec2(pane_width, row.height()),
                );
                self.last_local_rect = Some(
                    ui.scope_builder(egui::UiBuilder::new().max_rect(local), |ui| {
                        ui.set_clip_rect(local);
                        self.show_pane(ui, Location::Local)
                    })
                    .inner,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(rail), |ui| {
                    ui.set_clip_rect(rail);
                    self.show_transfer_controls(ui, false);
                });
                self.last_remote_rect = Some(
                    ui.scope_builder(egui::UiBuilder::new().max_rect(remote), |ui| {
                        ui.set_clip_rect(remote);
                        self.show_pane(ui, Location::Remote)
                    })
                    .inner,
                );
            }
            LayoutTier::Compact => {
                let gaps = ui.spacing().item_spacing.y * 2.0;
                let pane_height = ((pane_height - 44.0 - gaps) / 2.0).max(70.0);
                ui.allocate_ui(egui::vec2(ui.available_width(), pane_height), |ui| {
                    self.last_remote_rect = Some(self.show_pane(ui, Location::Remote));
                });
                self.show_transfer_controls(ui, true);
                ui.allocate_ui(egui::vec2(ui.available_width(), pane_height), |ui| {
                    self.last_local_rect = Some(self.show_pane(ui, Location::Local));
                });
            }
            LayoutTier::Minimal => {
                ui.horizontal(|ui| {
                    ui.label("Focused pane:");
                    for location in [Location::Local, Location::Remote] {
                        if ui
                            .selectable_label(self.focused_pane == location, location.label())
                            .clicked()
                        {
                            self.focused_pane = location;
                        }
                    }
                    if self.selected_file().is_some()
                        && ui.button(self.transfer_label(false)).clicked()
                    {
                        self.start_transfer();
                    }
                });
                ui.allocate_ui(egui::vec2(ui.available_width(), pane_height), |ui| {
                    let rect = self.show_pane(ui, self.focused_pane);
                    match self.focused_pane {
                        Location::Local => self.last_local_rect = Some(rect),
                        Location::Remote => self.last_remote_rect = Some(rect),
                    }
                });
            }
        }
        self.last_collision_rect = self.show_collision(ui);
        self.last_queue_rect = self.show_queue(ui);
    }

    fn show_pane(&mut self, ui: &mut egui::Ui, location: Location) -> egui::Rect {
        let selected = match location {
            Location::Local => self.local_selected,
            Location::Remote => self.remote_selected,
        };
        let entries = match location {
            Location::Local => self.local.clone(),
            Location::Remote => self.remote.clone(),
        };
        let path = match location {
            Location::Local => "~/projects/nimbus-relay",
            Location::Remote => "/srv/releases",
        };
        egui::Frame::group(ui.style())
            .show(ui, |ui| {
                ui.set_min_height(ui.available_height());
                ui.horizontal(|ui| {
                    ui.strong(location.label());
                    ui.monospace(path);
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt(("mobile-files", location.label()))
                    .show(ui, |ui| {
                        for (index, entry) in entries.iter().enumerate() {
                            let icon = match entry.kind {
                                EntryKind::Directory => "[DIR]",
                                EntryKind::File => "[FILE]",
                            };
                            let size = entry.size.map(format_size).unwrap_or_default();
                            if ui
                                .selectable_label(
                                    selected == Some(index),
                                    format!("{icon}  {:<27} {size}", entry.name),
                                )
                                .clicked()
                            {
                                self.select(location, index);
                            }
                        }
                    });
            })
            .response
            .rect
    }

    fn show_transfer_controls(&mut self, ui: &mut egui::Ui, horizontal: bool) {
        let enabled = self.selected_file().is_some() && self.collision.is_none();
        let label = self.transfer_label(horizontal);
        let mut clicked = false;
        if horizontal {
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), 44.0),
                egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                |ui| {
                    clicked = ui.add_enabled(enabled, egui::Button::new(label)).clicked();
                },
            );
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                clicked = ui.add_enabled(enabled, egui::Button::new(label)).clicked();
                ui.small("Select either pane");
            });
        }
        if clicked {
            self.start_transfer();
        }
    }

    fn show_collision(&mut self, ui: &mut egui::Ui) -> Option<egui::Rect> {
        let collision = self.collision.clone()?;
        Some(
            egui::Frame::group(ui.style())
                .show(ui, |ui| {
                    ui.strong(format!(
                        "{} already exists at the destination",
                        collision.name
                    ));
                    ui.horizontal_wrapped(|ui| {
                        for (label, decision) in [
                            ("Replace", CollisionDecision::Replace),
                            ("Keep both", CollisionDecision::KeepBoth),
                            ("Skip", CollisionDecision::Skip),
                        ] {
                            if ui.button(label).clicked() {
                                self.resolve_collision(decision);
                            }
                        }
                    });
                })
                .response
                .rect,
        )
    }

    fn show_queue(&mut self, ui: &mut egui::Ui) -> Option<egui::Rect> {
        if self.transfers.is_empty() {
            return None;
        }
        Some(
            ui.scope(|ui| {
                ui.separator();
                ui.strong("Transfers");
                for transfer in self.transfers.iter().rev().take(3) {
                    let direction = match transfer.from {
                        Location::Local => "Upload",
                        Location::Remote => "Download",
                    };
                    ui.horizontal(|ui| {
                        ui.label(format!("{direction}: {}", transfer.name));
                        ui.add(
                            egui::ProgressBar::new(transfer.progress)
                                .desired_width(ui.available_width())
                                .text(if transfer.complete() {
                                    "Complete".to_owned()
                                } else {
                                    format!("{:.0}%", transfer.progress * 100.0)
                                }),
                        );
                    });
                }
                if self.transfers.iter().any(|transfer| !transfer.complete()) {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(50));
                }
            })
            .response
            .rect,
        )
    }

    fn select(&mut self, location: Location, index: usize) {
        self.selected_location = location;
        self.focused_pane = location;
        match location {
            Location::Local => {
                self.local_selected = Some(index);
                self.remote_selected = None;
            }
            Location::Remote => {
                self.remote_selected = Some(index);
                self.local_selected = None;
            }
        }
    }

    fn selected_entry(&self) -> Option<&FileEntry> {
        match self.selected_location {
            Location::Local => self.local_selected.and_then(|index| self.local.get(index)),
            Location::Remote => self
                .remote_selected
                .and_then(|index| self.remote.get(index)),
        }
    }

    fn selected_file(&self) -> Option<&FileEntry> {
        self.selected_entry()
            .filter(|entry| entry.kind == EntryKind::File)
    }

    fn transfer_label(&self, horizontal: bool) -> String {
        let (action, arrow) = match (self.selected_location, horizontal) {
            (Location::Local, true) => ("Upload to Remote", "up"),
            (Location::Remote, true) => ("Download to Local", "down"),
            (Location::Local, false) => ("Upload to Remote", "right"),
            (Location::Remote, false) => ("Download to Local", "left"),
        };
        format!("{action} ({arrow})")
    }

    fn start_transfer(&mut self) {
        let Some(entry) = self.selected_file() else {
            return;
        };
        let name = entry.name;
        let from = self.selected_location;
        let destination = match from.opposite() {
            Location::Local => &self.local,
            Location::Remote => &self.remote,
        };
        if destination.iter().any(|entry| entry.name == name) {
            self.collision = Some(Collision { name, from });
            return;
        }
        self.enqueue(name.to_owned(), from);
    }

    fn resolve_collision(&mut self, decision: CollisionDecision) {
        let Some(collision) = self.collision.take() else {
            return;
        };
        match decision {
            CollisionDecision::Replace => self.enqueue(collision.name.to_owned(), collision.from),
            CollisionDecision::KeepBoth => {
                let (stem, extension) = collision
                    .name
                    .rsplit_once('.')
                    .map_or((collision.name, ""), |(stem, extension)| (stem, extension));
                let name = if extension.is_empty() {
                    format!("{stem} copy")
                } else {
                    format!("{stem} copy.{extension}")
                };
                self.enqueue(name, collision.from);
            }
            CollisionDecision::Skip => {}
        }
    }

    fn enqueue(&mut self, name: String, from: Location) {
        if self.transfers.len() == MAX_TRANSFER_HISTORY {
            let Some(completed) = self.transfers.iter().position(Transfer::complete) else {
                return;
            };
            self.transfers.remove(completed);
        }
        self.transfers.push(Transfer {
            name,
            from,
            progress: 0.0,
        });
    }

    fn tick(&mut self, now: f64) {
        let previous = self.last_tick.replace(now).unwrap_or(now);
        let delta = (now - previous).clamp(0.0, 0.25) as f32;
        for transfer in &mut self.transfers {
            if !transfer.complete() {
                transfer.progress = (transfer.progress + delta / TRANSFER_SECONDS as f32).min(1.0);
            }
        }
    }
}

fn format_size(size: u64) -> String {
    if size >= 1024 * 1024 {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    } else if size >= 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{size} B")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MarkdownMode {
    Preview,
    Source,
}

pub(crate) struct MarkdownWorkspace {
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
            document,
            mode: MarkdownMode::Preview,
            contents_open: false,
            selected_heading: 0,
            pending_heading: None,
        }
    }
}

impl MarkdownWorkspace {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        let tier = LayoutTier::for_size(ui.available_size());
        ui.horizontal(|ui| {
            ui.strong("Nimbus-Runbook.md");
            ui.small("LOCAL snapshot");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.small("Offline facsimile");
            });
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
    fn transfer_direction_follows_the_selected_source_pane() {
        let mut files = FilesWorkspace::default();
        assert!(files.transfer_label(true).contains("Remote"));
        assert!(files.transfer_label(true).contains("up"));
        files.select(Location::Remote, 3);
        assert!(files.transfer_label(true).contains("Local"));
        assert!(files.transfer_label(true).contains("down"));
        files.start_transfer();
        assert_eq!(files.transfers[0].from, Location::Remote);
    }

    #[test]
    fn collision_requires_an_explicit_decision_and_queue_is_bounded() {
        let mut files = FilesWorkspace::default();
        files.select(Location::Local, 4);
        files.start_transfer();
        assert_eq!(files.collision.as_ref().unwrap().name, "README.md");
        assert!(files.transfers.is_empty());
        files.resolve_collision(CollisionDecision::KeepBoth);
        assert_eq!(files.transfers[0].name, "README copy.md");
        for index in 0..MAX_TRANSFER_HISTORY + 3 {
            files.enqueue(format!("{index}.txt"), Location::Local);
        }
        assert_eq!(files.transfers.len(), MAX_TRANSFER_HISTORY);
    }

    #[test]
    fn transfer_progress_is_monotonic_and_completes() {
        let mut files = FilesWorkspace::default();
        files.start_transfer();
        files.tick(1.0);
        files.tick(1.25);
        let first = files.transfers[0].progress;
        files.tick(2.0);
        assert!(files.transfers[0].progress >= first);
        for step in 3..20 {
            files.tick(f64::from(step));
        }
        assert!(files.transfers[0].complete());
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
    fn compact_files_layout_keeps_both_panes_on_screen() {
        let mut files = FilesWorkspace::default();
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(390.0, 844.0),
                )),
                ..Default::default()
            },
            |ui| files.show(ui),
        );
        output.drop_without_applying_deltas();
        let local = files.last_local_rect.expect("local pane");
        let remote = files.last_remote_rect.expect("remote pane");
        assert!(remote.bottom() < local.top());
        assert!(remote.bottom() <= 844.0);
        assert!(local.height() > 200.0);
        assert!(remote.height() > 200.0);
    }

    #[test]
    fn wide_files_layout_keeps_horizontal_panes_on_screen() {
        let mut files = FilesWorkspace::default();
        let context = egui::Context::default();
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_194.0, 834.0),
                )),
                ..Default::default()
            },
            |ui| files.show(ui),
        );
        output.drop_without_applying_deltas();
        let local = files.last_local_rect.expect("local pane");
        let remote = files.last_remote_rect.expect("remote pane");
        assert!(local.right() < remote.left());
        assert!(
            remote.right() <= 1_194.0,
            "local={local:?}, remote={remote:?}"
        );
        assert!((local.height() - remote.height()).abs() < 1.0);
    }

    #[test]
    fn compact_files_layout_keeps_collision_and_queue_actions_reachable() {
        let mut files = FilesWorkspace::default();
        files.select(Location::Local, 4);
        files.start_transfer();
        let context = egui::Context::default();
        let render = |files: &mut FilesWorkspace| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 844.0),
                    )),
                    ..Default::default()
                },
                |ui| files.show(ui),
            )
        };
        render(&mut files).drop_without_applying_deltas();
        assert!(files.last_collision_rect.unwrap().bottom() <= 844.0);

        files.resolve_collision(CollisionDecision::Replace);
        for index in 0..2 {
            files.enqueue(format!("{index}.txt"), Location::Local);
        }
        render(&mut files).drop_without_applying_deltas();
        assert!(files.last_queue_rect.unwrap().bottom() <= 844.0);
    }
}
