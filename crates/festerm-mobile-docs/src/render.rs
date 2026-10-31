//! Headless plain-text projection of the Markdown model. It is the
//! accessibility/text fallback and the deterministic target of render tests;
//! the egui host draws the same `Block` tree natively.

use festerm_markdown::{Block, Inline, ListKind, MarkdownDocument, TableAlignment, TaskState};

pub fn render_plain_text(document: &MarkdownDocument) -> String {
    let mut out = String::new();
    blocks(document, document.blocks(), 0, &mut out);
    out
}

fn inlines(document: &MarkdownDocument, items: &[Inline], out: &mut String) {
    for inline in items {
        match inline {
            Inline::Text(t) => out.push_str(t.text()),
            Inline::Code(t) => {
                out.push('`');
                out.push_str(t.text());
                out.push('`');
            }
            Inline::Emphasis(c) => wrap(document, c.inlines(), "_", out),
            Inline::Strong(c) => wrap(document, c.inlines(), "**", out),
            Inline::Strikethrough(c) => wrap(document, c.inlines(), "~~", out),
            Inline::Link(l) => {
                inlines(document, l.inlines(), out);
                if let Some(r) = document.resource_references().get(l.reference_index()) {
                    out.push_str(" <");
                    out.push_str(r.target());
                    out.push('>');
                }
            }
            Inline::Image(i) => {
                out.push_str("[image: ");
                out.push_str(i.alt_text());
                out.push(']');
            }
            // Raw HTML is inert and never interpreted.
            Inline::RawHtml(_) => {}
            Inline::SoftBreak { .. } => out.push(' '),
            Inline::HardBreak { .. } => out.push('\n'),
        }
    }
}

fn wrap(document: &MarkdownDocument, items: &[Inline], mark: &str, out: &mut String) {
    out.push_str(mark);
    inlines(document, items, out);
    out.push_str(mark);
}

fn blocks(document: &MarkdownDocument, items: &[Block], depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    for block in items {
        match block {
            Block::Paragraph(p) => {
                out.push_str(&pad);
                inlines(document, p.inlines(), out);
                out.push_str("\n\n");
            }
            Block::Heading(h) => {
                out.push_str(&"#".repeat(usize::from(h.level())));
                out.push(' ');
                inlines(document, h.inlines(), out);
                out.push_str("\n\n");
            }
            Block::BlockQuote(q) => {
                let mut inner = String::new();
                blocks(document, q.blocks(), 0, &mut inner);
                for line in inner.trim_end().lines() {
                    out.push_str(&format!("{pad}> {line}\n"));
                }
                out.push('\n');
            }
            Block::List(list) => {
                for (n, item) in list.items().iter().enumerate() {
                    let marker = match list.kind() {
                        ListKind::Bullet => "-".to_owned(),
                        ListKind::Ordered { first_item_number } => {
                            format!("{}.", first_item_number + n as u64)
                        }
                    };
                    let task = match item.task_state() {
                        Some(TaskState::Checked) => " [x]",
                        Some(TaskState::Unchecked) => " [ ]",
                        None => "",
                    };
                    let mut inner = String::new();
                    blocks(document, item.blocks(), 0, &mut inner);
                    let mut lines = inner.trim_end().lines();
                    out.push_str(&format!(
                        "{pad}{marker}{task} {}\n",
                        lines.next().unwrap_or("")
                    ));
                    for line in lines {
                        out.push_str(&format!("{pad}  {line}\n"));
                    }
                }
                out.push('\n');
            }
            Block::Table(table) => {
                for (r, row) in table.rows().iter().enumerate() {
                    let cells: Vec<&str> = row.cells().iter().map(|c| c.plain_text()).collect();
                    out.push_str(&format!("{pad}| {} |\n", cells.join(" | ")));
                    if r == 0 {
                        let rule: Vec<&str> = table
                            .alignments()
                            .iter()
                            .map(|a| match a {
                                TableAlignment::Left => ":--",
                                TableAlignment::Center => ":-:",
                                TableAlignment::Right => "--:",
                                TableAlignment::None => "---",
                            })
                            .collect();
                        out.push_str(&format!("{pad}| {} |\n", rule.join(" | ")));
                    }
                }
                out.push('\n');
            }
            Block::CodeBlock(code) => {
                out.push_str(&format!("{pad}```{}\n", code.language().unwrap_or("")));
                out.push_str(code.code_text());
                if !code.code_text().ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!("{pad}```\n\n"));
            }
            Block::Html(_) => {}
            Block::Rule { .. } => out.push_str(&format!("{pad}---\n\n")),
        }
    }
}
