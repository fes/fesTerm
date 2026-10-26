use eframe::egui;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use super::{Inline, InlineRenderStyle, MarkdownDocument, SourceSpan};

const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;
static NEXT_SURFACE_OWNER: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
thread_local! {
    static CODE_SOURCE_MAPPINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Default)]
struct Selection {
    widgets: HashSet<egui::Id>,
    source: Option<Range<usize>>,
    html_bytes: usize,
    last_code: Option<usize>,
    last_code_source_end: usize,
    pieces: Vec<(Range<usize>, String)>,
    rich: bool,
    overflow: bool,
    invalid: bool,
    source_inexact: bool,
}

#[derive(Clone)]
struct MenuSelection {
    revision: DocumentRevision,
    html: String,
    markdown: Option<String>,
    text: String,
    include_source: bool,
    source_inexact: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct DocumentRevision {
    owner: u64,
    generation: u64,
}

#[derive(Default)]
struct ClipboardSelection {
    current: Selection,
    copies: HashMap<egui::ViewportId, (String, String)>,
    menus: HashMap<egui::ViewportId, (egui::Id, Arc<MenuSelection>)>,
    seen_menus: HashSet<egui::ViewportId>,
    ending_viewport: Option<egui::ViewportId>,
    capturing: bool,
}

impl egui::Plugin for ClipboardSelection {
    fn debug_name(&self) -> &'static str {
        "MarkdownClipboardSelection"
    }

    fn on_begin_pass(&mut self, ui: &mut egui::Ui) {
        let viewport = ui.ctx().viewport_id();
        self.copies.remove(&viewport);
        self.seen_menus.remove(&viewport);
        if viewport == egui::ViewportId::ROOT {
            let active: HashSet<_> =
                ui.input(|input| input.raw.viewports.keys().copied().collect());
            let mut closed = Vec::new();
            self.menus.retain(|viewport, (id, _)| {
                let keep = *viewport == egui::ViewportId::ROOT || active.contains(viewport);
                if !keep {
                    closed.push(*id);
                }
                keep
            });
            for id in closed {
                egui::Popup::close_id(ui.ctx(), id);
            }
        }
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        self.ending_viewport = Some(ui.ctx().viewport_id());
    }

    fn output_hook(&mut self, ctx: &egui::Context, output: &mut egui::FullOutput) {
        let Some(viewport) = self.ending_viewport.take() else {
            return;
        };
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

pub(super) fn install(context: &egui::Context) {
    context.plugin_or_default::<ClipboardSelection>();
}

pub(super) fn new_surface_owner() -> u64 {
    NEXT_SURFACE_OWNER
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |owner| {
            owner.checked_add(1)
        })
        .expect("Markdown clipboard surface owner exhausted")
}

pub(super) fn begin(ui: &egui::Ui) {
    let plugin = ui.ctx().plugin_or_default::<ClipboardSelection>();
    let (copying, opening_menu) = ui.input(|i| {
        (
            i.events.contains(&egui::Event::Copy),
            i.pointer.button_clicked(egui::PointerButton::Secondary),
        )
    });
    {
        let mut state = plugin.lock();
        state.current = Selection::default();
        state.capturing = copying || opening_menu;
    }
    ui.ctx()
        .plugin::<egui::text_selection::LabelSelectionState>()
        .lock()
        .begin_preserve_selection_on_secondary_press(ui.ctx().viewport_id());
    if opening_menu {
        ui.ctx()
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .capture_selection_text(ui.ctx().viewport_id());
    }
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
    if code_block.is_some() && state.current.last_code == code_block {
        let Some(gap) = document
            .source_text()
            .get(state.current.last_code_source_end..raw.start)
        else {
            tracing::warn!(
                previous = state.current.last_code_source_end,
                next = raw.start,
                "Markdown code source ranges are not monotonic; using plain clipboard fallback"
            );
            state.current.invalid = true;
            return;
        };
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
    }
    state.current.last_code = code_block;
    state.current.last_code_source_end = raw.end;
    if state.current.html_bytes.saturating_add(html.len()) > MAX_COPY_BYTES {
        state.current.overflow = true;
        return;
    }
    state.current.rich |= rich;
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

fn inline_breaks(inlines: &[Inline], breaks: &mut Vec<(Range<usize>, char)>) {
    for inline in inlines {
        match inline {
            Inline::SoftBreak { span } => breaks.push((span.byte_range(), ' ')),
            Inline::HardBreak { span } => breaks.push((span.byte_range(), '\n')),
            Inline::Emphasis(container)
            | Inline::Strong(container)
            | Inline::Strikethrough(container) => inline_breaks(container.inlines(), breaks),
            Inline::Link(link) => inline_breaks(link.inlines(), breaks),
            _ => {}
        }
    }
}

fn rich_inline_body(inlines: &[Inline], pieces: &[&(Range<usize>, String)]) -> String {
    let mut breaks = Vec::new();
    inline_breaks(inlines, &mut breaks);
    let mut html = String::new();
    let mut previous_end = None;
    let mut next_break = 0;
    for (source, fragment) in pieces {
        if let Some(previous_end) = previous_end {
            while next_break < breaks.len() && breaks[next_break].0.end <= previous_end {
                next_break += 1;
            }
            while next_break < breaks.len() && breaks[next_break].0.start < source.start {
                let (span, separator) = &breaks[next_break];
                if previous_end <= span.start && span.end <= source.start {
                    html.push(*separator);
                }
                next_break += 1;
            }
        }
        html.push_str(fragment);
        previous_end = Some(source.end);
    }
    html
}

fn overlapping_pieces<'a, 'b>(
    pieces: &'a [&'b (Range<usize>, String)],
    span: Range<usize>,
) -> &'a [&'b (Range<usize>, String)] {
    let first = pieces.partition_point(|(source, _)| source.end <= span.start);
    let end = first + pieces[first..].partition_point(|(source, _)| source.start < span.end);
    &pieces[first..end]
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
        let selected = overlapping_pieces(pieces, span);
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
            super::Block::Paragraph(paragraph) => html.push_str(&format!(
                "<p>{}</p>",
                rich_inline_body(paragraph.inlines(), selected)
            )),
            super::Block::Heading(heading) => html.push_str(&format!(
                "<h{0}>{1}</h{0}>",
                heading.level(),
                rich_inline_body(heading.inlines(), selected)
            )),
            super::Block::BlockQuote(quote) => html.push_str(&format!(
                "<blockquote>{}</blockquote>",
                rich_blocks(quote.blocks(), selected)
            )),
            super::Block::List(list) => {
                let selected_items: Vec<_> = list
                    .items()
                    .iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let content = rich_blocks(item.blocks(), selected);
                        (!content.is_empty()).then_some((index, content))
                    })
                    .collect();
                let Some((first_selected, _)) = selected_items.first() else {
                    continue;
                };
                let (open, close) = match list.kind() {
                    festerm_markdown::ListKind::Bullet => ("<ul>".to_owned(), "</ul>"),
                    festerm_markdown::ListKind::Ordered { first_item_number } => (
                        format!(
                            "<ol start=\"{}\">",
                            first_item_number.saturating_add(*first_selected as u64)
                        ),
                        "</ol>",
                    ),
                };
                html.push_str(&open);
                for (_, content) in selected_items {
                    html.push_str(&format!("<li>{content}</li>"));
                }
                html.push_str(close);
            }
            super::Block::Table(table) => {
                html.push_str("<table>");
                for row in table.rows() {
                    let row_selected = overlapping_pieces(selected, row.span().byte_range());
                    if row_selected.is_empty() {
                        continue;
                    }
                    let mut cells = String::new();
                    for cell in row.cells() {
                        let pieces = overlapping_pieces(row_selected, cell.span().byte_range());
                        let body = rich_inline_body(cell.inlines(), pieces);
                        let tag = if row.is_header() { "th" } else { "td" };
                        cells.push_str(&format!("<{tag}>{body}</{tag}>"));
                    }
                    html.push_str(&format!("<tr>{cells}</tr>"));
                }
                html.push_str("</table>");
            }
            _ => html.push_str(&format!("<pre>{}</pre>", body())),
        }
    }
    html
}

pub(super) fn finish(
    ui: &mut egui::Ui,
    document: &MarkdownDocument,
    owner: u64,
    generation: u64,
    rect: egui::Rect,
) {
    let (secondary_pressed, pointer_pos) = ui.input(|input| {
        (
            input.pointer.button_pressed(egui::PointerButton::Secondary),
            input.pointer.interact_pos(),
        )
    });
    let preserve_background_press = secondary_pressed
        && pointer_pos.is_some_and(|pos| {
            rect.contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        });
    ui.ctx()
        .plugin::<egui::text_selection::LabelSelectionState>()
        .lock()
        .end_preserve_selection_on_secondary_press(
            ui.ctx().viewport_id(),
            preserve_background_press,
        );
    let plugin = ui.ctx().plugin::<ClipboardSelection>();
    let id = ui.id().with(("markdown-selection-copy", owner));
    let viewport = ui.ctx().viewport_id();
    if !plugin.lock().capturing && !egui::Popup::is_id_open(ui.ctx(), id) {
        return;
    }
    let capturing = plugin.lock().capturing;
    let selection = std::mem::take(&mut plugin.lock().current);
    let owns_selection = if capturing && !selection.overflow && !selection.invalid {
        let ranges = ui
            .ctx()
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .selected_label_ranges(ui.ctx().viewport_id());
        !ranges.is_empty() && ranges.iter().all(|(id, _)| selection.widgets.contains(id))
    } else {
        false
    };
    let plain_text = if capturing {
        ui.ctx()
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .pending_copy_text(ui.ctx().viewport_id())
            .map(str::to_owned)
    } else {
        None
    };
    let html = if owns_selection {
        let mut pieces: Vec<_> = selection.pieces.iter().collect();
        pieces.sort_unstable_by_key(|(source, _)| source.start);
        format!(
            "<div style=\"white-space:pre-wrap\">{}</div>",
            rich_blocks(document.blocks(), &pieces)
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
        if let Some(expected) = plain_text.clone() {
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
    let revision = DocumentRevision { owner, generation };
    if opening {
        let mut source = selection.source;
        let payload = if owns_selection && html.len() <= MAX_COPY_BYTES {
            source.as_mut().zip(plain_text).map(|(source, text)| {
                expand_blocks(document.blocks(), source);
                let include_source = ui.input(|i| i.modifiers.shift);
                MenuSelection {
                    revision,
                    html,
                    markdown: (include_source && !selection.source_inexact)
                        .then(|| document.source_text()[source.clone()].to_owned()),
                    text,
                    include_source,
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
            if payload.include_source
                && ui
                    .add_enabled(
                        !payload.source_inexact,
                        egui::Button::new("Copy Markdown source"),
                    )
                    .on_disabled_hover_text("This partial transformed text has no exact Markdown source range. Select the complete text run or copy from Source mode.")
                    .clicked()
            {
                if let Some(markdown) = &payload.markdown {
                    ui.ctx().copy_text(markdown.clone());
                    ui.close();
                }
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

        fn select_between(&mut self, start_text: &str, end_text: &str) -> egui::Pos2 {
            self.step(Vec::new());
            let output = self.step(Vec::new());
            let point = |text: &str, end: bool| {
                let label = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(label) if label.galley.job.text == text => Some(label),
                        _ => None,
                    })
                    .unwrap();
                label.pos
                    + label
                        .galley
                        .pos_from_cursor(if end {
                            label.galley.end()
                        } else {
                            label.galley.begin()
                        })
                        .center()
                        .to_vec2()
            };
            let start = point(start_text, false);
            let end = point(end_text, true);
            self.step(vec![
                egui::Event::PointerMoved(start),
                button(start, true, egui::PointerButton::Primary),
            ]);
            self.step(vec![egui::Event::PointerMoved(end)]);
            self.step(vec![button(end, false, egui::PointerButton::Primary)]);
            end
        }

        fn select(&mut self, text: &str) -> egui::Pos2 {
            self.select_between(text, text)
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

    fn viewport_input(viewport: egui::ViewportId) -> egui::RawInput {
        viewport_input_with_active(viewport, &[viewport])
    }

    fn viewport_input_with_active(
        viewport: egui::ViewportId,
        active: &[egui::ViewportId],
    ) -> egui::RawInput {
        let mut input = egui::RawInput {
            viewport_id: viewport,
            ..Default::default()
        };
        for active_viewport in active {
            input.viewports.insert(
                *active_viewport,
                egui::ViewportInfo {
                    parent: (*active_viewport != egui::ViewportId::ROOT)
                        .then_some(egui::ViewportId::ROOT),
                    ..Default::default()
                },
            );
        }
        input
    }

    #[test]
    fn markdown_clipboard_output_and_menu_lifecycle_use_the_ended_viewport() {
        let context = egui::Context::default();
        let plugin = context.plugin_or_default::<ClipboardSelection>();
        let child = egui::ViewportId::from_hash_of("markdown-copy-child");

        for (viewport, text) in [(egui::ViewportId::ROOT, "root"), (child, "child")] {
            let output = context.run_ui(viewport_input(viewport), |ui| {
                ui.ctx()
                    .plugin::<ClipboardSelection>()
                    .lock()
                    .copies
                    .insert(viewport, (format!("<b>{text}</b>"), text.to_owned()));
                ui.ctx().copy_text(text.to_owned());
            });
            assert!(output.platform_output.commands.iter().any(|command| {
                matches!(command, egui::OutputCommand::CopyHtml {html, text: fallback}
                    if html == &format!("<b>{text}</b>") && fallback == text)
            }));
            output.drop_without_applying_deltas();
        }
        assert!(plugin.lock().copies.is_empty());

        let menu = |label: &str| {
            Arc::new(MenuSelection {
                revision: DocumentRevision {
                    owner: 1,
                    generation: label.len() as u64,
                },
                html: format!("<b>{label}</b>"),
                markdown: None,
                text: label.to_owned(),
                include_source: false,
                source_inexact: false,
            })
        };
        {
            let mut state = plugin.lock();
            state.menus.insert(
                egui::ViewportId::ROOT,
                (egui::Id::new("root-menu"), menu("root")),
            );
            state
                .menus
                .insert(child, (egui::Id::new("child-menu"), menu("child")));
        }
        let output = context.run_ui(
            viewport_input_with_active(egui::ViewportId::ROOT, &[egui::ViewportId::ROOT, child]),
            |ui| {
                ui.ctx()
                    .plugin::<ClipboardSelection>()
                    .lock()
                    .seen_menus
                    .insert(egui::ViewportId::ROOT);
            },
        );
        output.drop_without_applying_deltas();
        assert_eq!(plugin.lock().menus.len(), 2);

        let output = context.run_ui(viewport_input(egui::ViewportId::ROOT), |ui| {
            ui.ctx()
                .plugin::<ClipboardSelection>()
                .lock()
                .seen_menus
                .insert(egui::ViewportId::ROOT);
        });
        output.drop_without_applying_deltas();
        let state = plugin.lock();
        assert!(state.menus.contains_key(&egui::ViewportId::ROOT));
        assert!(!state.menus.contains_key(&child));
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
        assert!(harness
            .context
            .plugin::<ClipboardSelection>()
            .lock()
            .menus
            .values()
            .all(|(_, payload)| payload.markdown.is_none()));
        let copy = menu_button(&output, "Copy rich text").unwrap();
        harness.step(vec![
            egui::Event::PointerMoved(copy),
            button(copy, true, egui::PointerButton::Primary),
        ]);
        let output = harness.step(vec![button(copy, false, egui::PointerButton::Primary)]);
        assert!(output.platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyHtml {html, text} if html.contains("<strong>café 漢字</strong>") && text == "café 漢字")));
    }

    #[test]
    fn markdown_selection_keeps_secondary_but_not_middle_click_behavior() {
        let mut harness = CopyHarness::new("selected text\n");
        harness.select("selected text");
        let outside = egui::pos2(850.0, 550.0);
        harness.step(vec![
            egui::Event::PointerMoved(outside),
            button(outside, true, egui::PointerButton::Middle),
        ]);
        harness.step(vec![button(outside, false, egui::PointerButton::Middle)]);
        let output = harness.step(vec![egui::Event::Copy]);
        assert!(output.platform_output.commands.is_empty());
    }

    #[test]
    fn markdown_selection_survives_secondary_press_on_document_whitespace() {
        let mut harness = CopyHarness::new("selected text\n");
        harness.select("selected text");
        let output = harness.menu(egui::pos2(850.0, 550.0), false);
        assert!(menu_button(&output, "Copy rich text").is_some());
        assert!(harness
            .context
            .plugin::<egui::text_selection::LabelSelectionState>()
            .lock()
            .has_selection());
    }

    #[test]
    fn markdown_rich_copy_uses_parser_breaks_not_widget_geometry_for_separators() {
        for (source, separator) in [
            ("**bold**_italic_\n", ""),
            ("**bold**\n_italic_\n", " "),
            ("**bold**  \n_italic_\n", "\n"),
        ] {
            let mut harness = CopyHarness::new(source);
            harness.select_between("bold", "italic");
            let output = harness.step(vec![egui::Event::Copy]);
            let html = output
                .platform_output
                .commands
                .iter()
                .find_map(|command| match command {
                    egui::OutputCommand::CopyHtml { html, .. } => Some(html),
                    _ => None,
                })
                .unwrap();
            assert!(
                html.contains(&format!("<strong>bold</strong>{separator}<em>italic</em>")),
                "{source:?}: {html}"
            );
        }
    }

    #[test]
    fn markdown_rich_copy_preserves_the_first_selected_ordered_list_number() {
        let mut harness = CopyHarness::new("1. first\n2. second\n3. third\n");
        harness.select("third");
        let output = harness.step(vec![egui::Event::Copy]);
        let html = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyHtml { html, .. } => Some(html),
                _ => None,
            })
            .unwrap();
        assert!(
            html.contains("<ol start=\"3\"><li><p>third</p></li></ol>"),
            "{html}"
        );
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
                harness.pane.parse("**new secret selection**\n".to_owned());
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
    fn replacing_markdown_surface_retires_prior_menu_payload() {
        let mut harness = CopyHarness::new("**old secret selection**\n");
        let end = harness.select("old secret selection");
        let output = harness.menu(end, true);
        assert!(menu_button(&output, "Copy Markdown source").is_some());

        harness.pane = MarkdownPreviewPane::new(
            LocalMarkdownSource::new("/owned/replacement.md")
                .unwrap()
                .into(),
            "**new public selection**\n",
        );
        let output = harness.step(Vec::new());
        assert!(menu_button(&output, "Copy Markdown source").is_none());
        assert!(harness
            .context
            .plugin::<ClipboardSelection>()
            .lock()
            .menus
            .is_empty());
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
