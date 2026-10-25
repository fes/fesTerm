use eframe::egui;
use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

use super::{Inline, InlineRenderStyle, MarkdownDocument, SourceSpan};

const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
thread_local! {
    static CODE_SOURCE_MAPPINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Default)]
struct Selection {
    widgets: HashSet<egui::Id>,
    source: Option<Range<usize>>,
    html_bytes: usize,
    text: String,
    last_rect: Option<egui::Rect>,
    last_code: Option<usize>,
    last_code_source_end: usize,
    pieces: Vec<(Range<usize>, String)>,
    rich: bool,
    overflow: bool,
    source_inexact: bool,
}

#[derive(Clone)]
struct MenuSelection {
    revision: u64,
    html: String,
    markdown: String,
    text: String,
    include_source: bool,
    source_inexact: bool,
}

#[derive(Default)]
struct ClipboardSelection {
    current: Selection,
    copies: HashMap<egui::ViewportId, (String, String)>,
    menus: HashMap<egui::ViewportId, (egui::Id, Arc<MenuSelection>)>,
    seen_menus: HashSet<egui::ViewportId>,
    capturing: bool,
}

impl egui::Plugin for ClipboardSelection {
    fn debug_name(&self) -> &'static str {
        "MarkdownClipboardSelection"
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        self.copies.remove(&ui.ctx().viewport_id());
        self.seen_menus.remove(&ui.ctx().viewport_id());
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        let viewport = ctx.viewport_id();
        if !self.seen_menus.remove(&viewport) {
            if let Some((id, _)) = self.menus.remove(&viewport) {
                egui::Popup::close_id(ctx, id);
            }
        }
        let Some((html, expected_text)) = self.copies.remove(&viewport) else {
            return;
        };
        // Label selection emits its Copy command at end-of-pass, after widgets.
        if let Some(command) = output.platform_output.commands.last_mut() {
            if let egui::OutputCommand::CopyText(text) = command {
                if *text != expected_text {
                    return;
                }
                *command = egui::OutputCommand::CopyHtml {
                    html,
                    text: std::mem::take(text),
                };
            }
        }
    }
}

pub(super) fn begin(ui: &egui::Ui) {
    let plugin = ui.ctx().plugin_or_default::<ClipboardSelection>();
    let mut state = plugin.lock();
    state.current = Selection::default();
    state.capturing = ui.input(|i| {
        i.events.contains(&egui::Event::Copy)
            || i.pointer.button_clicked(egui::PointerButton::Secondary)
    });
}

fn escape(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        result.push_str(match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\'' => "&#39;",
            _ => {
                result.push(character);
                continue;
            }
        });
    }
    result
}

fn styled(text: &str, style: InlineRenderStyle) -> String {
    let mut html = escape(text);
    for (enabled, tag) in [
        (style.code_like, "code"),
        (style.strikethrough, "s"),
        (style.italics, "em"),
        (style.strong, "strong"),
    ] {
        if enabled {
            html = format!("<{tag}>{html}</{tag}>");
        }
    }
    html
}

fn byte_at(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(index, _)| index)
}

fn source_range(
    document: &MarkdownDocument,
    text: &str,
    span: SourceSpan,
    selected: Range<usize>,
) -> Option<Range<usize>> {
    let source = span.byte_range();
    let raw = &document.source_text()[source.clone()];
    // Text events retain their exact encoded span. A decoded entity/escape is
    // indivisible in source; never search the document for a duplicate string.
    if raw == text {
        Some(
            source.start + byte_at(text, selected.start)
                ..source.start + byte_at(text, selected.end),
        )
    } else if let Some(prefix) = raw
        .strip_suffix(text)
        .filter(|prefix| prefix.chars().all(|ch| ch == ' '))
    {
        Some(
            source.start + prefix.len() + byte_at(text, selected.start)
                ..source.start + prefix.len() + byte_at(text, selected.end),
        )
    } else if selected.start == 0 && selected.end == text.chars().count() {
        Some(source)
    } else {
        None
    }
}

struct Run<'a> {
    text: &'a str,
    span: SourceSpan,
    style: InlineRenderStyle,
    href: Option<&'a str>,
}

fn collect<'a>(
    inlines: &'a [Inline],
    document: &'a MarkdownDocument,
    style: InlineRenderStyle,
    runs: &mut Vec<Run<'a>>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) => runs.push(Run {
                text: text.text(),
                span: text.text_span(),
                style: if matches!(inline, Inline::Code(_)) {
                    style.as_inline_code()
                } else {
                    style
                },
                href: None,
            }),
            Inline::Emphasis(container)
            | Inline::Strong(container)
            | Inline::Strikethrough(container) => {
                let style = match inline {
                    Inline::Emphasis(_) => style.with_italics(),
                    Inline::Strong(_) => style.with_strong(),
                    _ => style.with_strikethrough(),
                };
                collect(container.inlines(), document, style, runs);
            }
            Inline::Link(link) => {
                let start = runs.len();
                collect(link.inlines(), document, style, runs);
                let reference = &document.resource_references()[link.reference_index()];
                if reference.class() == festerm_markdown::ResourceReferenceClass::HttpsAbsolute
                    && reference.target().starts_with("https://")
                    && !reference.target().chars().any(char::is_control)
                {
                    for run in &mut runs[start..] {
                        run.href = Some(reference.target());
                    }
                }
            }
            Inline::Image(image) => runs.push(Run {
                text: image.alt_text(),
                span: image.alt_text_span(),
                style,
                href: None,
            }),
            Inline::RawHtml(html) => runs.push(Run {
                text: html.literal(),
                span: html.span(),
                style: style.as_inline_code(),
                href: None,
            }),
            Inline::SoftBreak { span } => runs.push(Run {
                text: " ",
                span: *span,
                style,
                href: None,
            }),
            Inline::HardBreak { span } => runs.push(Run {
                text: "\n",
                span: *span,
                style,
                href: None,
            }),
        }
    }
}

pub(super) fn text(
    ui: &egui::Ui,
    response: &egui::Response,
    document: &MarkdownDocument,
    text: &str,
    span: SourceSpan,
    style: InlineRenderStyle,
) {
    register(
        ui,
        response,
        document,
        &[Run {
            text,
            span,
            style,
            href: None,
        }],
        None,
        None,
    );
}

pub(super) fn code_line(
    ui: &egui::Ui,
    response: &egui::Response,
    document: &MarkdownDocument,
    block: &festerm_markdown::CodeBlock,
    text: &str,
    offset: usize,
) {
    if selected_range(ui, response).is_none() {
        return;
    }
    #[cfg(test)]
    CODE_SOURCE_MAPPINGS.set(CODE_SOURCE_MAPPINGS.get() + 1);
    let Some(span) = block
        .code_source_range(offset..offset + text.len(), document.source_text())
        .and_then(|range| document.source_span(range))
    else {
        return;
    };
    register(
        ui,
        response,
        document,
        &[Run {
            text,
            span,
            style: InlineRenderStyle::body().as_inline_code(),
            href: None,
        }],
        None,
        Some(block.span().start().byte_offset()),
    );
}

pub(super) fn inlines(
    ui: &egui::Ui,
    response: &egui::Response,
    document: &MarkdownDocument,
    inlines: &[Inline],
    style: InlineRenderStyle,
    whole: Option<SourceSpan>,
) {
    if selected_range(ui, response).is_none() {
        return;
    }
    let mut runs = Vec::new();
    collect(inlines, document, style, &mut runs);
    register(ui, response, document, &runs, whole, None);
}

pub(super) fn link(
    ui: &egui::Ui,
    response: &egui::Response,
    document: &MarkdownDocument,
    link: &festerm_markdown::LinkInline,
    style: InlineRenderStyle,
) {
    if selected_range(ui, response).is_none() {
        return;
    }
    inlines(
        ui,
        response,
        document,
        &[Inline::Link(link.clone())],
        style,
        Some(link.span()),
    );
}

fn selected_range(ui: &egui::Ui, response: &egui::Response) -> Option<Range<usize>> {
    if !ui
        .ctx()
        .plugin_or_default::<ClipboardSelection>()
        .lock()
        .capturing
    {
        return None;
    }
    ui.ctx()
        .plugin::<egui::text_selection::LabelSelectionState>()
        .lock()
        .selected_label_range(ui.ctx().viewport_id(), response.id)
}

fn register(
    ui: &egui::Ui,
    response: &egui::Response,
    document: &MarkdownDocument,
    runs: &[Run<'_>],
    whole: Option<SourceSpan>,
    code_block: Option<usize>,
) {
    let rich = code_block.is_none();
    let selected = selected_range(ui, response);
    let Some(selected) = selected else { return };
    let mut source: Option<Range<usize>> = None;
    let mut html = String::new();
    let mut text = String::new();
    let mut cursor = 0;
    for run in runs {
        let count = run.text.chars().count();
        let start = selected.start.max(cursor);
        let end = selected.end.min(cursor + count);
        if start < end {
            let range = start - cursor..end - cursor;
            let raw =
                source_range(document, run.text, run.span, range.clone()).unwrap_or_else(|| {
                    ui.ctx()
                        .plugin::<ClipboardSelection>()
                        .lock()
                        .current
                        .source_inexact = true;
                    run.span.byte_range()
                });
            if let Some(source) = &mut source {
                source.start = source.start.min(raw.start);
                source.end = source.end.max(raw.end);
            } else {
                source = Some(raw);
            }
            let selected_text =
                &run.text[byte_at(run.text, range.start)..byte_at(run.text, range.end)];
            let content = styled(selected_text, run.style);
            if let Some(href) = run.href {
                html.push_str(&format!("<a href=\"{}\">{content}</a>", escape(href)));
            } else {
                html.push_str(&content);
            }
            if html.len() > MAX_COPY_BYTES {
                ui.ctx()
                    .plugin::<ClipboardSelection>()
                    .lock()
                    .current
                    .overflow = true;
                return;
            }
            text.push_str(selected_text);
        }
        cursor += count;
    }
    if selected.start == 0 && selected.end == cursor {
        if let Some(whole) = whole {
            source = Some(whole.byte_range());
        }
    }
    let Some(raw) = source else { return };
    let plugin = ui.ctx().plugin_or_default::<ClipboardSelection>();
    let mut state = plugin.lock();
    if state.current.overflow
        || state.current.html_bytes.saturating_add(html.len()) > MAX_COPY_BYTES
    {
        state.current.overflow = true;
        return;
    }
    if let Some(previous) = state.current.last_rect {
        let separator = !state.current.text.ends_with(char::is_whitespace)
            && !text.starts_with(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
        if code_block.is_some() && state.current.last_code == code_block {
            let gap = &document.source_text()[state.current.last_code_source_end..raw.start];
            let mut chars = gap.chars().peekable();
            let mut lines = 0;
            while let Some(ch) = chars.next() {
                if ch == '\r' {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    lines += 1;
                } else if ch == '\n' {
                    lines += 1;
                }
            }
            html.insert_str(0, &"\n".repeat(lines));
        } else if rich && separator && state.current.last_code.is_none() {
            html.insert(0, ' ');
        }
        if previous.bottom() <= response.rect.top() {
            state.current.text.push('\n');
            if response.rect.top() - previous.bottom() > response.rect.height() * 0.5 {
                state.current.text.push('\n');
            }
        } else if !state.current.text.ends_with(char::is_whitespace)
            && !text.starts_with(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        {
            state.current.text.push(' ');
        }
    }
    state.current.last_rect = Some(response.rect);
    state.current.last_code = code_block;
    state.current.last_code_source_end = raw.end;
    if state.current.html_bytes.saturating_add(html.len()) > MAX_COPY_BYTES {
        state.current.overflow = true;
        return;
    }
    state.current.rich |= rich;
    state.current.text.push_str(&text);
    state.current.widgets.insert(response.id);
    if let Some(source) = &mut state.current.source {
        source.start = source.start.min(raw.start);
        source.end = source.end.max(raw.end);
    } else {
        state.current.source = Some(raw.clone());
    }
    state.current.html_bytes += html.len();
    state.current.pieces.push((raw, html));
}

fn expand_inlines(inlines: &[Inline], selected: &mut Range<usize>) {
    for inline in inlines {
        if let Inline::Code(text) = inline {
            let display = text.text_span().byte_range();
            if selected.start <= display.start && selected.end >= display.end {
                let span = text.span().byte_range();
                selected.start = selected.start.min(span.start);
                selected.end = selected.end.max(span.end);
            }
        }
        let children = match inline {
            Inline::Emphasis(container)
            | Inline::Strong(container)
            | Inline::Strikethrough(container) => container.inlines(),
            Inline::Link(link) => link.inlines(),
            _ => continue,
        };
        expand_inlines(children, selected);
        if let (Some(first), Some(last)) = (children.first(), children.last()) {
            let first = match first {
                Inline::Text(text) | Inline::Code(text) => text.text_span(),
                _ => first.span(),
            };
            let last = match last {
                Inline::Text(text) | Inline::Code(text) => text.text_span(),
                _ => last.span(),
            };
            if selected.start <= first.byte_range().start && selected.end >= last.byte_range().end {
                let span = inline.span().byte_range();
                selected.start = selected.start.min(span.start);
                selected.end = selected.end.max(span.end);
            }
        }
    }
}

fn expand_blocks(blocks: &[super::Block], selected: &mut Range<usize>) {
    for block in blocks {
        match block {
            super::Block::Paragraph(block) => expand_inlines(block.inlines(), selected),
            super::Block::Heading(block) => expand_inlines(block.inlines(), selected),
            super::Block::BlockQuote(block) => expand_blocks(block.blocks(), selected),
            super::Block::List(block) => {
                for item in block.items() {
                    expand_blocks(item.blocks(), selected);
                }
            }
            super::Block::Table(block) => {
                for row in block.rows() {
                    for cell in row.cells() {
                        expand_inlines(cell.inlines(), selected);
                    }
                }
            }
            _ => {}
        }
    }
}

fn rich_blocks(blocks: &[super::Block], pieces: &[&(Range<usize>, String)]) -> String {
    let mut html = String::new();
    for block in blocks {
        let span = block.span().byte_range();
        let selected: Vec<_> = pieces
            .iter()
            .filter(|(source, _)| source.start < span.end && span.start < source.end)
            .copied()
            .collect();
        if selected.is_empty() {
            continue;
        }
        let body = || {
            selected
                .iter()
                .map(|(_, html)| html.as_str())
                .collect::<String>()
        };
        match block {
            super::Block::Paragraph(_) => html.push_str(&format!("<p>{}</p>", body())),
            super::Block::Heading(heading) => {
                html.push_str(&format!("<h{0}>{1}</h{0}>", heading.level(), body()))
            }
            super::Block::BlockQuote(quote) => html.push_str(&format!(
                "<blockquote>{}</blockquote>",
                rich_blocks(quote.blocks(), &selected)
            )),
            super::Block::List(list) => {
                let (open, close) = match list.kind() {
                    festerm_markdown::ListKind::Bullet => ("<ul>".to_owned(), "</ul>"),
                    festerm_markdown::ListKind::Ordered { first_item_number } => {
                        (format!("<ol start=\"{first_item_number}\">"), "</ol>")
                    }
                };
                html.push_str(&open);
                for item in list.items() {
                    let content = rich_blocks(item.blocks(), &selected);
                    if !content.is_empty() {
                        html.push_str(&format!("<li>{content}</li>"));
                    }
                }
                html.push_str(close);
            }
            super::Block::Table(table) => {
                html.push_str("<table>");
                for row in table.rows() {
                    let mut cells = String::new();
                    for cell in row.cells() {
                        let span = cell.span().byte_range();
                        let body: String = selected
                            .iter()
                            .filter(|(source, _)| {
                                source.start < span.end && span.start < source.end
                            })
                            .map(|(_, html)| html.as_str())
                            .collect();
                        let tag = if row.is_header() { "th" } else { "td" };
                        cells.push_str(&format!("<{tag}>{body}</{tag}>"));
                    }
                    if selected.iter().any(|(source, _)| {
                        source.start < row.span().byte_range().end
                            && row.span().byte_range().start < source.end
                    }) {
                        html.push_str(&format!("<tr>{cells}</tr>"));
                    }
                }
                html.push_str("</table>");
            }
            _ => html.push_str(&format!("<pre>{}</pre>", body())),
        }
    }
    html
}

pub(super) fn finish(ui: &mut egui::Ui, document: &MarkdownDocument, rect: egui::Rect) {
    let plugin = ui.ctx().plugin::<ClipboardSelection>();
    let id = ui.id().with("markdown-selection-copy");
    let viewport = ui.ctx().viewport_id();
    if !plugin.lock().capturing && !egui::Popup::is_id_open(ui.ctx(), id) {
        return;
    }
    let selection = std::mem::take(&mut plugin.lock().current);
    let ranges = ui
        .ctx()
        .plugin::<egui::text_selection::LabelSelectionState>()
        .lock()
        .selected_label_ranges(ui.ctx().viewport_id());
    let owns_selection = !selection.overflow
        && !ranges.is_empty()
        && ranges.iter().all(|(id, _)| selection.widgets.contains(id));
    let html = if owns_selection {
        format!(
            "<div style=\"white-space:pre-wrap\">{}</div>",
            rich_blocks(
                document.blocks(),
                &selection.pieces.iter().collect::<Vec<_>>()
            )
        )
    } else {
        String::new()
    };
    if selection.overflow || html.len() > MAX_COPY_BYTES {
        tracing::warn!("Markdown rich copy exceeds the clipboard output limit");
    }
    if owns_selection
        && selection.rich
        && html.len() <= MAX_COPY_BYTES
        && ui.input(|i| i.events.contains(&egui::Event::Copy))
    {
        let expected = ui
            .ctx()
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .pending_copy_text(ui.ctx().viewport_id())
            .map(str::to_owned);
        if let Some(expected) = expected {
            plugin
                .lock()
                .copies
                .insert(ui.ctx().viewport_id(), (html.clone(), expected));
        }
    }

    let opening = ui.input(|i| {
        i.pointer.button_clicked(egui::PointerButton::Secondary)
            && i.pointer
                .interact_pos()
                .is_some_and(|pos| rect.contains(pos))
    }) && ui
        .ctx()
        .layer_id_at(ui.ctx().pointer_interact_pos().unwrap_or_default())
        == Some(ui.layer_id());
    if !opening && !egui::Popup::is_id_open(ui.ctx(), id) {
        let mut state = plugin.lock();
        if state
            .menus
            .get(&viewport)
            .is_some_and(|(owner, _)| *owner == id)
        {
            state.menus.remove(&viewport);
        }
        return;
    }
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    document.source_text().hash(&mut revision);
    let revision = revision.finish();
    if opening {
        let mut source = selection.source;
        let payload = if owns_selection && html.len() <= MAX_COPY_BYTES {
            source.as_mut().map(|source| {
                expand_blocks(document.blocks(), source);
                MenuSelection {
                    revision,
                    html,
                    markdown: document.source_text()[source.clone()].to_owned(),
                    text: selection.text,
                    include_source: ui.input(|i| i.modifiers.shift),
                    source_inexact: selection.source_inexact,
                }
            })
        } else {
            None
        };
        let mut state = plugin.lock();
        state.menus.remove(&viewport);
        if let Some(payload) = payload {
            state.menus.insert(viewport, (id, Arc::new(payload)));
        }
    }
    let payload = {
        let mut state = plugin.lock();
        let payload = state
            .menus
            .get(&viewport)
            .filter(|(owner, payload)| *owner == id && payload.revision == revision)
            .map(|(_, payload)| payload.clone());
        if payload.is_some() {
            state.seen_menus.insert(viewport);
        }
        payload
    };
    let open = opening.then_some(egui::SetOpenCommand::Bool(payload.is_some()));
    egui::Popup::new(id, ui.ctx().clone(), egui::PopupAnchor::PointerFixed, ui.layer_id())
        .kind(egui::PopupKind::Menu)
        .open_memory(open)
        .show(|ui| {
            let Some(payload) = payload else {
                ui.close();
                return;
            };
            if ui.button("Copy rich text").clicked() {
                ui.ctx().send_cmd(egui::OutputCommand::CopyHtml {
                    html: payload.html.clone(),
                    text: payload.text.clone(),
                });
                ui.close();
            }
            if payload.include_source && ui.add_enabled(!payload.source_inexact, egui::Button::new("Copy Markdown source"))
                .on_disabled_hover_text("This partial transformed text has no exact Markdown source range. Select the complete text run or copy from Source mode.")
                .clicked() {
                ui.ctx().copy_text(payload.markdown.clone());
                ui.close();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::super::MarkdownPreviewPane;
    use super::*;
    use festerm_markdown::LocalMarkdownSource;

    struct CopyHarness {
        context: egui::Context,
        pane: MarkdownPreviewPane,
        frame: usize,
    }

    impl CopyHarness {
        fn new(source: &str) -> Self {
            Self {
                context: egui::Context::default(),
                pane: MarkdownPreviewPane::new(
                    LocalMarkdownSource::new("/owned/copy.md").unwrap().into(),
                    source,
                ),
                frame: 0,
            }
        }

        fn step(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            self.frame += 1;
            let mut output = self.context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    events,
                    time: Some(self.frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| self.pane.show(ui),
            );
            output.textures_delta.clear();
            output
        }

        fn select(&mut self, text: &str) -> egui::Pos2 {
            self.step(Vec::new());
            let output = self.step(Vec::new());
            let label = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(label) if label.galley.job.text == text => Some(label),
                    _ => None,
                })
                .unwrap();
            let start = label.pos
                + label
                    .galley
                    .pos_from_cursor(label.galley.begin())
                    .center()
                    .to_vec2();
            let end = label.pos
                + label
                    .galley
                    .pos_from_cursor(label.galley.end())
                    .center()
                    .to_vec2();
            self.step(vec![
                egui::Event::PointerMoved(start),
                button(start, true, egui::PointerButton::Primary),
            ]);
            self.step(vec![egui::Event::PointerMoved(end)]);
            self.step(vec![button(end, false, egui::PointerButton::Primary)]);
            end
        }

        fn menu(&mut self, pos: egui::Pos2, shift: bool) -> egui::FullOutput {
            let modifiers = if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            };
            self.step(vec![
                egui::Event::ModifiersChanged(modifiers),
                button(pos, true, egui::PointerButton::Secondary),
            ]);
            self.step(vec![button(pos, false, egui::PointerButton::Secondary)]);
            self.step(Vec::new())
        }
    }

    fn button(pos: egui::Pos2, pressed: bool, button: egui::PointerButton) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            pressed,
            button,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn menu_button(output: &egui::FullOutput, text: &str) -> Option<egui::Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(label) if label.galley.job.text == text => {
                Some(label.galley.rect.translate(label.pos.to_vec2()).center())
            }
            _ => None,
        })
    }

    #[test]
    fn markdown_copy_idle_crlf_code_performs_no_source_mapping() {
        let mut harness = CopyHarness::new(&format!("```text\r\n{}```\r\n", "x\r\n".repeat(1500)));
        CODE_SOURCE_MAPPINGS.set(0);
        for _ in 0..3 {
            harness.step(Vec::new());
        }
        assert_eq!(CODE_SOURCE_MAPPINGS.get(), 0);
    }

    #[test]
    fn markdown_copy_normal_menu_is_rich_with_plain_fallback_and_no_source_action() {
        let mut harness = CopyHarness::new("**café 漢字**\n");
        let end = harness.select("café 漢字");
        let output = harness.menu(end, false);
        assert!(menu_button(&output, "Copy Markdown source").is_none());
        let copy = menu_button(&output, "Copy rich text").unwrap();
        harness.step(vec![
            egui::Event::PointerMoved(copy),
            button(copy, true, egui::PointerButton::Primary),
        ]);
        let output = harness.step(vec![button(copy, false, egui::PointerButton::Primary)]);
        assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyHtml {html, text} if html.contains("<strong>café 漢字</strong>") && text == "café 漢字")));
    }

    #[test]
    fn markdown_copy_reparse_and_hidden_pane_retire_menu_payloads_before_activation() {
        for hidden in [false, true] {
            let mut harness = CopyHarness::new("**old secret selection**\n");
            let end = harness.select("old secret selection");
            let output = harness.menu(end, true);
            let copy = menu_button(&output, "Copy Markdown source").unwrap();
            assert_eq!(
                harness
                    .context
                    .plugin::<ClipboardSelection>()
                    .lock()
                    .menus
                    .len(),
                1
            );
            let output = if hidden {
                let mut output = harness.context.run_ui(Default::default(), |_| {});
                output.textures_delta.clear();
                output
            } else {
                harness.pane.parse("**replacement**\n".to_owned());
                harness.step(Vec::new())
            };
            assert!(menu_button(&output, "Copy Markdown source").is_none());
            assert!(harness
                .context
                .plugin::<ClipboardSelection>()
                .lock()
                .menus
                .is_empty());
            harness.step(vec![
                egui::Event::PointerMoved(copy),
                button(copy, true, egui::PointerButton::Primary),
            ]);
            let output = harness.step(vec![button(copy, false, egui::PointerButton::Primary)]);
            assert!(output.platform_output.commands.is_empty());
        }
    }

    #[test]
    fn markdown_rich_copy_escapes_literal_html_and_never_exports_local_resource_authority() {
        let text = "<script src='https://evil.test/x'>payload</script> & \"quotes\"";
        let html = styled(text, InlineRenderStyle::body().as_inline_code());
        assert!(html.contains("&lt;script"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("<img"));
        assert!(!html.contains("<svg"));
        let source = "[local](file:///private/secret) [remote](https://example.test/?x=%22&y=1)";
        let document = festerm_markdown::MarkdownLoader::default()
            .load(
                LocalMarkdownSource::new("/owned/copy.md").unwrap().into(),
                source.len(),
                source.as_bytes(),
                &Default::default(),
            )
            .unwrap();
        let super::super::Block::Paragraph(block) = &document.blocks()[0] else {
            panic!()
        };
        let mut runs = Vec::new();
        collect(
            block.inlines(),
            &document,
            InlineRenderStyle::body(),
            &mut runs,
        );
        assert!(runs
            .iter()
            .filter(|run| run.text == "local")
            .all(|run| run.href.is_none()));
        assert!(
            runs.iter()
                .any(|run| run.text == "remote"
                    && run.href == Some("https://example.test/?x=%22&y=1"))
        );
    }
}
