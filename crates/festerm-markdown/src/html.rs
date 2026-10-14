use super::*;
use quick_xml::{events::Event as XmlEvent, Reader};

#[derive(Clone, Debug)]
pub(super) struct SvgFragment {
    range: Range<usize>,
    xml: Arc<str>,
}

pub(super) fn svg_fragments(
    source: &str,
    options: Options,
    bounds: MarkdownBounds,
    cancellation: &MarkdownCancellation,
) -> Result<Vec<SvgFragment>, MarkdownLoadError> {
    let mut fragments = Vec::new();
    if !source.contains("<svg") {
        return Ok(fragments);
    }
    let mut consumed = 0;
    let mut first_html = true;
    let mut svg_html_block = false;
    for (index, (event, range)) in Parser::new_ext(source, options)
        .into_offset_iter()
        .enumerate()
    {
        if index % 128 == 0 {
            ensure_not_cancelled(cancellation)?;
        }
        if matches!(event, Event::Start(Tag::HtmlBlock)) {
            first_html = true;
        }
        if matches!(event, Event::Html(_)) {
            if first_html {
                svg_html_block = starts_svg(source[range.clone()].trim_start());
                first_html = false;
            }
            if !svg_html_block {
                continue;
            }
        }
        if range.start < consumed || !matches!(event, Event::Html(_) | Event::InlineHtml(_)) {
            continue;
        }
        let mut start = range.start;
        loop {
            let whitespace =
                source[start..range.end].len() - source[start..range.end].trim_start().len();
            start += whitespace;
            if start >= range.end {
                break;
            }
            let text = &source[start..];
            if !starts_svg(text) {
                break;
            }
            let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
            let quote_depth = source[line_start..start]
                .chars()
                .take_while(|character| character.is_whitespace() || *character == '>')
                .filter(|character| *character == '>')
                .count();
            // Keep byte offsets identical while removing only Markdown container
            // prefixes. XML whitespace itself is harmless.
            let input = &text[..text.floor_char_boundary(MAX_INLINE_SVG_BYTES.min(text.len()))];
            let mut reader = Reader::from_str(input);
            let mut depth = 0usize;
            let mut end = None;
            let mut events = 0;
            loop {
                if events % 128 == 0 {
                    ensure_not_cancelled(cancellation)?;
                }
                events += 1;
                match reader.read_event() {
                    Ok(XmlEvent::Start(_)) => {
                        depth += 1;
                        if depth > 64 {
                            break;
                        }
                    }
                    Ok(XmlEvent::Empty(_)) if depth == 0 => {
                        end = Some(reader.buffer_position() as usize);
                        break;
                    }
                    Ok(XmlEvent::End(_)) => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = Some(reader.buffer_position() as usize);
                            break;
                        }
                    }
                    Ok(XmlEvent::Eof) | Err(_) => break,
                    _ => {}
                }
            }
            consumed = start + reader.buffer_position() as usize;
            if let Some(end) = end {
                if fragments.len() >= bounds.max_resource_references {
                    return Err(MarkdownLoadError::TooManyResourceReferences {
                        limit: bounds.max_resource_references,
                        actual: fragments.len() + 1,
                    });
                }
                fragments.push(SvgFragment {
                    range: start..start + end,
                    xml: if quote_depth == 0 {
                        Arc::from(&input[..end])
                    } else {
                        Arc::from(
                            String::from_utf8(quote_mask(&input[..end], quote_depth))
                                .expect("ASCII quote masking preserves UTF-8"),
                        )
                    },
                });
                start += end;
                if start >= range.end {
                    break;
                }
            } else {
                break;
            }
        }
    }
    Ok(fragments)
}

fn starts_svg(text: &str) -> bool {
    text.starts_with("<svg")
        && text
            .as_bytes()
            .get(4)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/')
}

fn quote_mask(xml: &str, depth: usize) -> Vec<u8> {
    let mut bytes = xml.as_bytes().to_vec();
    let mut start = 0;
    for line in xml.split_inclusive('\n') {
        if start != 0 {
            let mut cursor = 0;
            for _ in 0..depth {
                while line
                    .as_bytes()
                    .get(cursor)
                    .is_some_and(|byte| *byte == b' ' || *byte == b'\t')
                {
                    cursor += 1;
                }
                if line.as_bytes().get(cursor) != Some(&b'>') {
                    break;
                }
                bytes[start + cursor] = b' ';
                cursor += 1;
            }
        }
        start += line.len();
    }
    bytes
}

pub(super) fn mask_svg(source: &str, fragments: &[SvgFragment]) -> Option<String> {
    if fragments.is_empty() {
        return None;
    }
    let mut bytes = source.as_bytes().to_vec();
    for fragment in fragments {
        // Keep XML atomic across Markdown paragraph breaks without changing source byte offsets.
        bytes[fragment.range.clone()].fill(b' ');
        bytes[fragment.range.start..fragment.range.start + 3].copy_from_slice(b"<?s");
        bytes[fragment.range.end - 2..fragment.range.end].copy_from_slice(b"?>");
    }
    Some(String::from_utf8(bytes).expect("ASCII SVG masking preserves UTF-8"))
}

#[derive(Default)]
struct AnchorTag {
    href: Option<String>,
    title: Option<String>,
    names: Vec<String>,
    empty: bool,
}

fn anchor_open(text: &str) -> bool {
    let bytes = text.trim().as_bytes();
    bytes.first() == Some(&b'<')
        && bytes
            .get(1)
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'a'))
        && bytes
            .get(2)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/')
}

fn anchor_close(text: &str) -> bool {
    text.trim().strip_prefix("</").is_some_and(|rest| {
        rest.as_bytes()
            .first()
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'a'))
            && rest[1..].trim() == ">"
    })
}

fn anchor_tag(literal: &str) -> Option<AnchorTag> {
    let text = literal.trim();
    let mut cursor = 2;
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'<')
        || !bytes.get(1)?.eq_ignore_ascii_case(&b'a')
        || !bytes
            .get(2)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/')
    {
        return None;
    }

    let mut tag = AnchorTag::default();
    let mut attributes = std::collections::BTreeSet::new();
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if text.get(cursor..) == Some(">") {
            return Some(tag);
        }
        if text.get(cursor..) == Some("/>") {
            tag.empty = true;
            return Some(tag);
        }
        let start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            cursor += 1;
        }
        if cursor == start {
            return None;
        }
        let name = text[start..cursor].to_ascii_lowercase();
        if !attributes.insert(name.clone())
            || !matches!(name.as_str(), "href" | "id" | "name" | "title")
        {
            return None;
        }
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'=') {
            return None;
        }
        cursor += 1;
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let quote = *bytes.get(cursor)?;
        let value = if quote == b'\'' || quote == b'"' {
            cursor += 1;
            let start = cursor;
            while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
                cursor += 1;
            }
            bytes.get(cursor)?;
            let value = &text[start..cursor];
            cursor += 1;
            value
        } else {
            let start = cursor;
            while bytes
                .get(cursor)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
            {
                cursor += 1;
            }
            if start == cursor {
                return None;
            }
            &text[start..cursor]
        };
        let value = quick_xml::escape::unescape(value).ok()?.into_owned();
        match name.as_str() {
            "href" => tag.href = Some(value),
            "title" => tag.title = Some(value),
            _ if !value.is_empty() => tag.names.push(value),
            _ => {}
        }
    }
}

impl DocumentBuilder<'_> {
    pub(super) fn svg_inline(
        &mut self,
        range: Range<usize>,
    ) -> Result<Option<Inline>, MarkdownLoadError> {
        let index = self
            .svg_fragments
            .partition_point(|fragment| fragment.range.start < range.start);
        let Some(fragment) = self.svg_fragments.get(index).filter(|fragment| {
            fragment.range.start == range.start && fragment.range.end <= range.end
        }) else {
            return Ok(None);
        };
        let span = self.span(fragment.range.clone());
        let xml = fragment.xml.clone();
        let reference_index = self.push_resource_reference(ResourceReference {
            kind: ResourceReferenceKind::Image,
            class: ResourceReferenceClass::IntrinsicSvg,
            target: format!("Inline SVG at byte {}", range.start),
            title: None,
            label_text: None,
            alt_text: Some("SVG diagram".to_owned()),
            span,
            is_autolink: false,
            inline_svg: Some(xml),
        })?;
        Ok(Some(Inline::Image(ImageInline {
            span,
            reference_index,
            alt_text_span: span,
            alt_text: "SVG diagram".to_owned(),
        })))
    }

    pub(super) fn project_anchors(
        &mut self,
        inlines: &mut Vec<Inline>,
    ) -> Result<(), MarkdownLoadError> {
        for inline in inlines.iter_mut() {
            match inline {
                Inline::Emphasis(container)
                | Inline::Strong(container)
                | Inline::Strikethrough(container) => {
                    self.project_anchors(&mut container.inlines)?
                }
                _ => {}
            }
        }
        let mut pairs = BTreeMap::new();
        let mut opening = None;
        let mut depth = 0usize;
        for (index, inline) in inlines.iter().enumerate() {
            if let Inline::RawHtml(raw) = inline {
                let tag = anchor_tag(&raw.literal);
                if anchor_open(&raw.literal) && !tag.as_ref().is_some_and(|tag| tag.empty) {
                    opening = if depth == 0 && tag.is_some() {
                        Some(index)
                    } else {
                        None
                    };
                    depth += 1;
                } else if anchor_close(&raw.literal) {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        if let Some(start) = opening.take() {
                            pairs.insert(start, index);
                        }
                    }
                }
            }
        }
        let mut input = std::collections::VecDeque::from(std::mem::take(inlines));
        let mut index = 0;
        while let Some(inline) = input.pop_front() {
            let start_index = index;
            index += 1;
            let tag = match &inline {
                Inline::RawHtml(raw) => anchor_tag(&raw.literal),
                _ => None,
            };
            let Some(tag) = tag else {
                inlines.push(inline);
                continue;
            };
            let close = pairs.get(&start_index).copied();
            if !tag.empty && close.is_none() {
                inlines.push(inline);
                continue;
            }
            let mut children = Vec::new();
            let end = if tag.empty {
                inline.span().byte_end
            } else {
                let close = close.expect("closing anchor exists");
                for _ in index..close {
                    children.push(input.pop_front().expect("anchor child exists"));
                }
                index = close + 1;
                input
                    .pop_front()
                    .expect("closing anchor exists")
                    .span()
                    .byte_end
            };
            let span = self.span(inline.span().byte_start..end);
            for name in tag.names {
                if !self.named_anchors.contains_key(&name)
                    && self.named_anchors.len() >= self.bounds.max_resource_references
                {
                    return Err(MarkdownLoadError::TooManyResourceReferences {
                        limit: self.bounds.max_resource_references,
                        actual: self.named_anchors.len() + 1,
                    });
                }
                self.named_anchors.entry(name).or_insert(span);
            }
            if let Some(target) = tag.href {
                let plain_text = inline_plain_text(&children);
                let reference_index = self.push_resource_reference(ResourceReference {
                    kind: ResourceReferenceKind::Link,
                    class: classify_target(self.source, &target),
                    target,
                    title: tag.title,
                    label_text: option_string(plain_text.clone()),
                    alt_text: None,
                    span,
                    is_autolink: false,
                    inline_svg: None,
                })?;
                inlines.push(Inline::Link(LinkInline {
                    span,
                    reference_index,
                    plain_text,
                    inlines: children,
                }));
            } else {
                if children.is_empty() {
                    children.push(Inline::Text(TextInline {
                        span,
                        text_span: span,
                        text: String::new(),
                    }));
                }
                inlines.extend(children);
            }
        }
        Ok(())
    }

    pub(super) fn project_html_block(
        &mut self,
        range: Range<usize>,
        literal: String,
    ) -> Result<(), MarkdownLoadError> {
        let source = self.source_slice(range.clone()).trim_start();
        if !starts_svg(source) && !anchor_open(source) {
            return self.push_block(Block::Html(RawHtmlBlock {
                span: self.span(range),
                literal,
            }));
        }
        let mut inlines = Vec::new();
        let mut cursor = range.start;
        while cursor < range.end {
            if let Some(image) = self.svg_inline(cursor..range.end)? {
                cursor = image.span().byte_end;
                inlines.push(image);
                continue;
            }
            let rest = self.source_slice(cursor..range.end);
            let length = if rest.starts_with('<') {
                html_tag_length(rest)
            } else {
                rest.find('<').unwrap_or(rest.len())
            };
            let span = self.span(cursor..cursor + length);
            let text = self.source_slice(cursor..cursor + length).to_owned();
            inlines.push(if text.starts_with('<') {
                Inline::RawHtml(RawHtmlInline {
                    span,
                    literal: text,
                })
            } else {
                Inline::Text(TextInline {
                    span,
                    text_span: span,
                    text,
                })
            });
            cursor += length;
        }

        fn html_tag_length(text: &str) -> usize {
            if text.starts_with("<!--") {
                return text.find("-->").map_or(text.len(), |end| end + 3);
            }
            let name_end = 1 + text
                .as_bytes()
                .iter()
                .skip(1)
                .take_while(|byte| byte.is_ascii_alphabetic())
                .count();
            if [
                "script",
                "style",
                "textarea",
                "title",
                "xmp",
                "noembed",
                "noframes",
                "plaintext",
            ]
            .iter()
            .any(|name| text[1..name_end].eq_ignore_ascii_case(name))
            {
                return text.len();
            }
            let mut quote = None;
            for (index, byte) in text.bytes().enumerate() {
                match (quote, byte) {
                    (Some(expected), actual) if actual == expected => quote = None,
                    (None, b'\'' | b'"') => quote = Some(byte),
                    (None, b'>') => return index + 1,
                    _ => {}
                }
            }
            text.len()
        }
        let anchors_before = self.named_anchors.len();
        let resources_before = self.resource_references.len();
        let raw_before = inlines
            .iter()
            .filter(|inline| matches!(inline, Inline::RawHtml(_)))
            .count();
        self.project_anchors(&mut inlines)?;
        if self.named_anchors.len() != anchors_before
            || self.resource_references.len() != resources_before
            || inlines
                .iter()
                .filter(|inline| matches!(inline, Inline::RawHtml(_)))
                .count()
                != raw_before
            || inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Image(_)))
            || inlines
                .iter()
                .any(|inline| matches!(inline, Inline::Text(text) if text.text.is_empty()))
        {
            self.push_block(Block::Paragraph(TextBlock {
                span: self.span(range),
                plain_text: inline_plain_text(&inlines),
                inlines,
            }))
        } else {
            self.push_block(Block::Html(RawHtmlBlock {
                span: self.span(range),
                literal,
            }))
        }
    }
}
