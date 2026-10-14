use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, OnceLock},
};

use eframe::egui;
use resvg::{tiny_skia, usvg};

use crate::markdown_images::{
    DecodedImage, ImageLoadFailure, ImageReservation, RETAINED_IMAGE_OVERHEAD,
};

const MAX_XML_NODES: u32 = 16_384;
const MAX_DEPTH: usize = 64;
const MAX_EXPANDED_NODES: u64 = 65_536;
const MAX_EXPANDED_BYTES: u64 = 4 * 1024 * 1024;
const MAX_GLYPHS: u64 = 16_384;
const MAX_CSS_BYTES: usize = 16 * 1024;
const MAX_CSS_RULES: usize = 128;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;

fn refusal(message: &str) -> ImageLoadFailure {
    ImageLoadFailure::Permanent(format!("SVG: {message}"))
}

#[derive(Clone, Copy, Default)]
struct Cost {
    nodes: u64,
    bytes: u64,
    glyphs: u64,
    depth: usize,
    clips: u64,
}

impl Cost {
    fn add(&mut self, other: Self) -> Result<(), ImageLoadFailure> {
        self.nodes += other.nodes;
        self.bytes += other.bytes;
        self.glyphs += other.glyphs;
        self.clips += other.clips;
        self.depth = self.depth.max(other.depth + 1);
        if self.nodes > MAX_EXPANDED_NODES
            || self.bytes > MAX_EXPANDED_BYTES
            || self.glyphs > MAX_GLYPHS
            || self.depth > MAX_DEPTH
        {
            return Err(refusal(
                "XML/reference expansion exceeds the complexity limit.",
            ));
        }
        Ok(())
    }

    fn parser_bytes(self) -> u64 {
        self.bytes * 64 + self.nodes * 1024 + self.glyphs * 8192
    }
}

fn urls(value: &str) -> Result<Vec<String>, ImageLoadFailure> {
    if value.contains('\\') || value.contains('@') || value.contains("/*") {
        return Err(refusal(
            "CSS escapes, imports and external styles are not supported.",
        ));
    }
    let lower = value.to_ascii_lowercase();
    let mut rest = lower.as_str();
    let mut offset = 0;
    let mut references = Vec::new();
    while let Some(start) = rest.find("url") {
        offset += start + 3;
        rest = &lower[offset..];
        let whitespace = rest.len() - rest.trim_start().len();
        if !rest[whitespace..].starts_with('(') {
            continue;
        }
        offset += whitespace + 1;
        let end = lower[offset..]
            .find(')')
            .ok_or_else(|| refusal("Malformed CSS resource reference."))?
            + offset;
        let target = value[offset..end].trim().trim_matches(['\'', '"']);
        let Some(id) = target.strip_prefix('#').filter(|id| !id.is_empty()) else {
            return Err(refusal("Only internal fragment references are supported; external/data resources are blocked."));
        };
        references.push(id.to_owned());
        offset = end + 1;
        rest = &lower[offset..];
    }
    Ok(references)
}

fn preflight(document: &roxmltree::Document<'_>) -> Result<Cost, ImageLoadFailure> {
    let root = document.root_element();
    if root.tag_name().name() != "svg"
        || root
            .tag_name()
            .namespace()
            .is_some_and(|namespace| namespace != "http://www.w3.org/2000/svg")
    {
        return Err(refusal("The XML root must be an SVG element."));
    }
    let mut ids = BTreeMap::new();
    let mut stylesheet_references = Vec::new();
    let mut css_bytes = 0;
    let mut css_rules = 0;
    for node in document.descendants() {
        if node.is_pi() {
            return Err(refusal("XML processing instructions are not supported."));
        }
        if !node.is_element() {
            continue;
        }
        if node.ancestors().count() > MAX_DEPTH {
            return Err(refusal("XML nesting exceeds the depth limit."));
        }
        if node
            .tag_name()
            .namespace()
            .is_some_and(|namespace| namespace != "http://www.w3.org/2000/svg")
        {
            return Err(refusal("Foreign XML/HTML content is not supported."));
        }
        if !matches!(
            node.tag_name().name(),
            "svg"
                | "g"
                | "defs"
                | "symbol"
                | "use"
                | "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "text"
                | "tspan"
                | "textPath"
                | "title"
                | "desc"
                | "metadata"
                | "linearGradient"
                | "radialGradient"
                | "stop"
                | "clipPath"
                | "marker"
                | "style"
                | "a"
                | "switch"
        ) {
            return Err(refusal("Scripts, images, foreignObject, animation, filters, masks and patterns are not supported."));
        }
        if let Some(id) = node.attribute("id") {
            if ids.insert(id.to_owned(), node).is_some() {
                return Err(refusal("Duplicate XML ids are not supported."));
            }
        }
        if node.tag_name().name() == "style" {
            let css = node.text().unwrap_or_default();
            css_bytes += css.len();
            css_rules += css.bytes().filter(|byte| *byte == b'{').count();
            if css_bytes > MAX_CSS_BYTES || css_rules > MAX_CSS_RULES {
                return Err(refusal("Stylesheet exceeds the CSS complexity limit."));
            }
            stylesheet_references.extend(urls(css)?);
        }
        for attribute in node.attributes() {
            if attribute.name().to_ascii_lowercase().starts_with("on") {
                return Err(refusal("Event handlers are not supported."));
            }
            if attribute.name() == "href" && !attribute.value().starts_with('#') {
                return Err(refusal("External and data href resources are blocked."));
            }
            if matches!(attribute.name(), "filter" | "mask") && attribute.value() != "none" {
                return Err(refusal("Filters and masks are not supported."));
            }
            urls(attribute.value())?;
        }
    }
    let mut memo = HashMap::new();
    expansion(
        root,
        &ids,
        &stylesheet_references,
        &mut Vec::new(),
        &mut memo,
    )
}

fn expansion<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    ids: &BTreeMap<String, roxmltree::Node<'a, 'input>>,
    stylesheet_references: &[String],
    visiting: &mut Vec<roxmltree::NodeId>,
    memo: &mut HashMap<roxmltree::NodeId, Cost>,
) -> Result<Cost, ImageLoadFailure> {
    if visiting.len() >= MAX_DEPTH || visiting.contains(&node.id()) {
        return Err(refusal(
            "Cyclic or excessively deep SVG references are blocked.",
        ));
    }
    if let Some(cost) = memo.get(&node.id()) {
        return Ok(*cost);
    }
    visiting.push(node.id());
    let text = node.text().unwrap_or_default();
    let mut cost = Cost {
        nodes: 1,
        bytes: if node.is_text() {
            text.len() as u64
        } else {
            node.attributes()
                .map(|attribute| attribute.value().len() as u64 + 16)
                .sum()
        },
        glyphs: if node.is_text()
            && node.parent_element().is_some_and(|parent| {
                matches!(parent.tag_name().name(), "text" | "tspan" | "textPath")
            }) {
            text.chars().count() as u64
        } else {
            0
        },
        depth: 1,
        clips: u64::from(node.is_element() && node.tag_name().name() == "clipPath"),
    };
    for child in node.children() {
        cost.add(expansion(
            child,
            ids,
            stylesheet_references,
            visiting,
            memo,
        )?)?;
    }
    if node.is_element() {
        let mut references = Vec::new();
        for attribute in node.attributes() {
            if attribute.name() == "href" {
                references.push(attribute.value().trim_start_matches('#').to_owned());
            }
            references.extend(urls(attribute.value())?);
        }
        if matches!(
            node.tag_name().name(),
            "path"
                | "rect"
                | "circle"
                | "ellipse"
                | "line"
                | "polyline"
                | "polygon"
                | "text"
                | "g"
                | "use"
        ) {
            references.extend_from_slice(stylesheet_references);
        }
        for reference in references {
            let target = ids
                .get(&reference)
                .ok_or_else(|| refusal("An internal SVG reference has no target."))?;
            cost.add(expansion(
                *target,
                ids,
                stylesheet_references,
                visiting,
                memo,
            )?)?;
        }
    }
    visiting.pop();
    memo.insert(node.id(), cost);
    Ok(cost)
}

fn fonts() -> &'static (Arc<usvg::fontdb::Database>, String) {
    static FONTS: OnceLock<(Arc<usvg::fontdb::Database>, String)> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut database = usvg::fontdb::Database::new();
        database.load_font_data(
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Regular.ttf")
                .to_vec(),
        );
        database.load_font_data(
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Bold.ttf").to_vec(),
        );
        database.load_font_data(
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-Italic.ttf")
                .to_vec(),
        );
        database.load_font_data(
            include_bytes!("../../../assets/fonts/jetbrains-mono/JetBrainsMono-BoldItalic.ttf")
                .to_vec(),
        );
        let family = database
            .faces()
            .next()
            .expect("validated bundled SVG font exists")
            .families[0]
            .0
            .clone();
        database.set_sans_serif_family(&family);
        database.set_serif_family(&family);
        database.set_monospace_family(&family);
        (Arc::new(database), family)
    })
}

fn layer_bytes(
    group: &usvg::Group,
    width: u32,
    height: u32,
    clips: u64,
    depth: usize,
) -> Result<u64, ImageLoadFailure> {
    if depth > MAX_DEPTH {
        return Err(refusal("Rendered tree exceeds the depth limit."));
    }
    let mut bytes = 0;
    if group.should_isolate() {
        let bbox = group.abs_layer_bounding_box();
        let layer_width = layer_axis(bbox.width(), width)?;
        let layer_height = layer_axis(bbox.height(), height)?;
        if layer_width * layer_height > MAX_PIXELS {
            return Err(refusal(
                "A temporary SVG layer exceeds the raster-area limit.",
            ));
        }
        bytes += layer_width
            * layer_height
            * 4
            * if group.clip_path().is_some() {
                1 + 5 * clips
            } else {
                1
            };
    }
    for node in group.children() {
        match node {
            usvg::Node::Group(group) => {
                bytes += layer_bytes(group, width, height, clips, depth + 1)?
            }
            usvg::Node::Text(text) => {
                bytes += layer_bytes(text.flattened(), width, height, clips, depth + 1)?
            }
            _ => {}
        }
    }
    Ok(bytes)
}

fn layer_axis(extent: f32, canvas: u32) -> Result<u64, ImageLoadFailure> {
    if !extent.is_finite() || extent < 0.0 {
        return Err(refusal("A temporary SVG layer has invalid bounds."));
    }
    Ok((f64::from(extent).ceil() + 4.0).min(f64::from(canvas) * 5.0) as u64)
}

pub(crate) fn decode(
    bytes: &[u8],
    scratch: &mut ImageReservation,
    base_bytes: u64,
    texture_side: usize,
) -> Result<DecodedImage, ImageLoadFailure> {
    if bytes.len() > festerm_markdown::MAX_INLINE_SVG_BYTES {
        return Err(refusal("Encoded SVG must not exceed 1 MiB."));
    }
    let xml = std::str::from_utf8(bytes).map_err(|_| refusal("SVG must be UTF-8 XML."))?;
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            ..Default::default()
        },
    )
    .map_err(|_| refusal("Invalid or over-complex XML; DTD/entity declarations are blocked."))?;
    let cost = preflight(&document)?;
    scratch.resize(base_bytes + cost.parser_bytes())?;
    let (fontdb, family) = fonts();
    let options = usvg::Options {
        resources_dir: None,
        font_family: family.clone(),
        fontdb: fontdb.clone(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_xmltree(&document, &options).map_err(|error| {
        ImageLoadFailure::Permanent(format!("SVG could not be parsed: {error}"))
    })?;
    let size = tree.size().to_int_size();
    let (width, height) = (size.width(), size.height());
    let count = u64::from(width) * u64::from(height);
    if width as usize > texture_side || height as usize > texture_side || count > MAX_PIXELS {
        return Err(refusal(
            "Output exceeds the texture-axis or raster-area limit.",
        ));
    }
    let rgba_bytes = count * 4;
    let retained_bytes = RETAINED_IMAGE_OVERHEAD + 2 * rgba_bytes;
    scratch.resize(
        base_bytes
            + cost.parser_bytes()
            + rgba_bytes
            + retained_bytes
            + layer_bytes(tree.root(), width, height, cost.clips, 0)?,
    )?;
    let reservation = scratch.split(retained_bytes);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| refusal("Pixel storage could not be allocated."))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(count as usize)
        .map_err(|_| refusal("Pixel storage could not be allocated."))?;
    pixels.extend(
        pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|rgba| egui::Color32::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3])),
    );
    Ok(DecodedImage {
        pixels: egui::ColorImage::new([width as usize, height as usize], pixels),
        reservation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown_images::ImageMemoryBudget;

    fn render(xml: &str, side: usize) -> Result<DecodedImage, ImageLoadFailure> {
        let context = egui::Context::default();
        let budget = ImageMemoryBudget::for_context(&context, Default::default());
        let mut scratch = budget.reserve(1024, None)?;
        decode(xml.as_bytes(), &mut scratch, 1024, side)
    }

    #[test]
    fn svg_decodes_static_pixels_transparency_and_view_box() {
        let image = render(r#"<svg width="2" height="2" viewBox="0 0 2 2"><rect width="1" height="1" fill="red" fill-opacity=".5"/></svg>"#, 32)
            .unwrap_or_else(|error| panic!("{}", error.message()));
        assert_eq!(image.pixels.size, [2, 2]);
        assert_eq!(
            image.pixels.pixels[0],
            egui::Color32::from_rgba_premultiplied(128, 0, 0, 128)
        );
        assert_eq!(image.pixels.pixels[3], egui::Color32::TRANSPARENT);
    }

    #[test]
    fn svg_uses_bundled_fonts_and_internal_references() {
        for xml in [
            r##"<svg width="8" height="8"><defs><rect id="box" width="8" height="8" fill="red"/></defs><use href="#box"/></svg>"##,
            r#"<svg width="128" height="32"><text x="0" y="24" font-size="24" font-family="Not Installed">Hello</text></svg>"#,
            r##"<svg width="8" height="8"><defs><clipPath id="clip"><rect width="4" height="8"/></clipPath></defs><g clip-path="url(#clip)"><rect width="8" height="8" fill="red"/></g></svg>"##,
            r##"<svg width="8" height="8"><defs><linearGradient id="grad"><stop stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs><style>.shape{fill:url(#grad)}</style><rect class="shape" width="8" height="8"/></svg>"##,
        ] {
            let image =
                render(xml, 256).unwrap_or_else(|error| panic!("{}: {xml}", error.message()));
            assert!(image.pixels.pixels.iter().any(|pixel| pixel.a() != 0));
        }
    }

    #[test]
    fn svg_rejects_executable_external_and_unsupported_content() {
        for content in [
            "<script>evil()</script>",
            "<foreignObject/>",
            "<animate/>",
            "<filter/>",
            "<mask/>",
            "<pattern/>",
            "<image href=\"data:image/png;base64,AA==\"/>",
            "<use href=\"file:///secret\"/>",
            "<a href=\"https://example.com\"><rect/></a>",
            "<rect onclick=\"evil()\"/>",
            "<rect fill=\"url(https://example.com/p)\"/>",
            "<style>@import 'https://example.com/style';</style>",
            "<style>.x{fill:u\\72l(file:///secret)}</style>",
            "<?xml-stylesheet href=\"file:///secret\"?>",
        ] {
            let xml = format!("<svg width=\"2\" height=\"2\">{content}</svg>");
            assert!(
                matches!(render(&xml, 32), Err(ImageLoadFailure::Permanent(_))),
                "{xml}"
            );
        }
        assert!(render("<!DOCTYPE svg [<!ENTITY x SYSTEM 'file:///secret'>]><svg width='2' height='2'>&x;</svg>", 32).is_err());
        assert!(render("<svg><broken></svg>", 32).is_err());
    }

    #[test]
    fn svg_bounds_xml_references_stylesheets_and_raster_dimensions() {
        assert!(render("<svg width='33' height='2'/>", 32).is_err());
        assert!(render("<svg width='8192' height='8192'/>", 8192).is_err());
        assert!(render(
            &format!(
                "<svg>{}</svg>",
                " ".repeat(festerm_markdown::MAX_INLINE_SVG_BYTES)
            ),
            32
        )
        .is_err());
        assert!(render(
            &format!(
                "<svg width='2' height='2'>{}<rect/>{}</svg>",
                "<g>".repeat(MAX_DEPTH),
                "</g>".repeat(MAX_DEPTH)
            ),
            32
        )
        .is_err());
        assert!(render(
            "<svg width='2' height='2'><g id='a'><use href='#a'/></g></svg>",
            32
        )
        .is_err());
        let mut definitions = "<g id='n0'><rect width='2' height='2'/></g>".to_owned();
        for index in 1..20 {
            definitions.push_str(&format!(
                "<g id='n{index}'><use href='#n{}'/><use href='#n{}'/></g>",
                index - 1,
                index - 1
            ));
        }
        assert!(render(
            &format!(
                "<svg width='2' height='2'><defs>{definitions}</defs><use href='#n19'/></svg>"
            ),
            32
        )
        .is_err());
        let styles = format!(
            "<style>{}</style><style>{}</style>",
            " ".repeat(MAX_CSS_BYTES / 2 + 1),
            " ".repeat(MAX_CSS_BYTES / 2 + 1)
        );
        assert!(render(&format!("<svg width='2' height='2'>{styles}</svg>"), 32).is_err());
        let styles = ".shape{fill:red}".repeat(MAX_CSS_RULES + 1);
        assert!(render(
            &format!("<svg width='2' height='2'><style>{styles}</style></svg>"),
            32
        )
        .is_err());
        assert_eq!(layer_axis(f32::MAX, 2).unwrap(), 10);
        assert!(layer_axis(f32::INFINITY, 2).is_err());
        assert!(render("<svg width='2' height='2'><g opacity='.5'><rect width='1e30' height='1e30'/></g></svg>", 32).is_ok());
    }

    #[test]
    fn svg_admits_exact_encoded_xml_node_css_and_texture_limits() {
        let root = "<svg width='2' height='2'>";
        let end = "</svg>";
        let xml = format!(
            "{root}{}{end}",
            " ".repeat(festerm_markdown::MAX_INLINE_SVG_BYTES - root.len() - end.len())
        );
        assert!(render(&xml, 32).is_ok());
        assert!(render(&(xml + " "), 32).is_err());
        let exact = format!(
            "{root}{}{end}",
            "<rect/>".repeat(MAX_XML_NODES as usize - 2)
        );
        assert!(render(&exact, 32).is_ok());
        let too_many = format!(
            "{root}{}{end}",
            "<rect/>".repeat(MAX_XML_NODES as usize - 1)
        );
        assert!(render(&too_many, 32).is_err());
        let style = " ".repeat(MAX_CSS_BYTES);
        assert!(render(&format!("{root}<style>{style}</style>{end}"), 32).is_ok());
        assert!(render(
            &format!("{root}<style>{style}</style><style> </style>{end}"),
            32
        )
        .is_err());
        let rules = ".x{fill:red}".repeat(MAX_CSS_RULES);
        assert!(render(&format!("{root}<style>{rules}</style>{end}"), 32).is_ok());
        assert!(render(
            &format!("{root}<style>{rules}</style><style>.y{{fill:red}}</style>{end}"),
            32
        )
        .is_err());
        assert!(render("<svg width='32' height='32'/>", 32).is_ok());
        assert!(render("<svg width='33' height='32'/>", 32).is_err());
    }

    #[test]
    fn svg_budget_failure_can_retry_and_retained_owner_releases_memory() {
        let context = egui::Context::default();
        let budget = ImageMemoryBudget::for_context(&context, Default::default());
        let mut scratch = budget.reserve(1024, None).unwrap();
        let blocker = budget.reserve(budget.limit() - 1024, None).unwrap();
        let xml = b"<svg width='2' height='2'><rect width='2' height='2'/></svg>";
        assert!(matches!(
            decode(xml, &mut scratch, 1024, 32),
            Err(ImageLoadFailure::Budget { .. })
        ));
        drop(blocker);
        let image = decode(xml, &mut scratch, 1024, 32)
            .unwrap_or_else(|error| panic!("{}", error.message()));
        drop(scratch);
        assert_eq!(
            budget.usage_for_test().0,
            RETAINED_IMAGE_OVERHEAD + 2 * 2 * 2 * 4
        );
        drop(image);
        assert_eq!(budget.usage_for_test(), (0, 0));
    }
}
