use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{
    text::{LayoutJob, TextFormat},
    Color32, ColorImage, FontFamily, FontId, Pos2, Rect, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Vec2,
};
use festerm_core::{Attributes, Color, ColorScheme, CursorStyle, Dimensions, Rgb};
use swash::{
    scale::{image::Content, Render, ScaleContext, Source, StrikeWith},
    shape::ShapeContext,
    text::Script,
    FontRef,
};

use crate::{
    cache::{RenderedCell, TerminalRenderCache},
    fonts::{
        is_color_emoji, TerminalFontSet, BOLD_FAMILY, BOLD_ITALIC_FAMILY, COLOR_EMOJI_BYTES,
        EMOJI_FAMILY, ITALIC_FAMILY, LIGATURE_BOLD_FAMILY, LIGATURE_BOLD_ITALIC_FAMILY,
        LIGATURE_ITALIC_FAMILY, LIGATURE_REGULAR_FAMILY, REGULAR_FAMILY,
    },
    geometry::{CellGeometry, CellPosition, CellRange},
    selection::Selection,
    TerminalSnapshot, DEFAULT_BACKGROUND, DEFAULT_FOREGROUND, GLYPH_CACHE_CAPACITY,
    SELECTION_BACKGROUND,
};

mod clip_batching;

use clip_batching::{GridGlyphClipPolicy, GridTextClipOverride, GridTextClipPainter};

/// Font configuration for the initial cell renderer.
#[derive(Clone, Debug, PartialEq)]
pub struct FontSettings {
    pub size_points: f32,
    font_set: TerminalFontSet,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            size_points: 14.0,
            font_set: TerminalFontSet::default(),
        }
    }
}

impl FontSettings {
    pub(crate) fn regular_font_id(&self) -> FontId {
        let family = if self.font_set.ligatures() {
            LIGATURE_REGULAR_FAMILY
        } else {
            REGULAR_FAMILY
        };
        FontId::new(self.size_points, FontFamily::Name(family.into()))
    }

    pub(crate) const fn font_set(&self) -> TerminalFontSet {
        self.font_set
    }

    pub(crate) fn set_font_set(&mut self, font_set: TerminalFontSet) {
        self.font_set = font_set;
    }

    fn font_id(&self, attributes: Attributes) -> FontId {
        let family = match (
            self.font_set.ligatures(),
            attributes.contains(Attributes::BOLD),
            attributes.contains(Attributes::ITALIC),
        ) {
            (true, true, true) => LIGATURE_BOLD_ITALIC_FAMILY,
            (true, true, false) => LIGATURE_BOLD_FAMILY,
            (true, false, true) => LIGATURE_ITALIC_FAMILY,
            (true, false, false) => LIGATURE_REGULAR_FAMILY,
            (false, true, true) => BOLD_ITALIC_FAMILY,
            (false, true, false) => BOLD_FAMILY,
            (false, false, true) => ITALIC_FAMILY,
            (false, false, false) => REGULAR_FAMILY,
        };
        FontId::new(self.size_points, FontFamily::Name(family.into()))
    }

    fn font_id_for_text(&self, attributes: Attributes, text: &str) -> FontId {
        if is_color_emoji(text) {
            FontId::new(self.size_points, FontFamily::Name(EMOJI_FAMILY.into()))
        } else {
            self.font_id(attributes)
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GlyphKey {
    text: String,
    foreground: Color32,
    attributes: u16,
    font_size_bits: u32,
    layout_width_bits: u32,
    font_generation: crate::TerminalFontGeneration,
}

/// Cache laid-out cell glyphs. `egui` owns the underlying font atlas; this
/// cache avoids rebuilding a one-cell layout job for unchanged text styling.
#[derive(Default)]
pub(crate) struct GlyphCache {
    layouts: HashMap<GlyphKey, Arc<egui::Galley>>,
    color_emoji: ColorEmojiCache,
}

impl GlyphCache {
    pub(crate) fn clear(&mut self) {
        self.layouts.clear();
        self.color_emoji.clear();
    }

    pub(crate) fn layout(
        &mut self,
        painter: &egui::Painter,
        text: &str,
        attributes: Attributes,
        foreground: Color32,
        font: &FontSettings,
        layout_width: f32,
    ) -> Arc<egui::Galley> {
        let key = GlyphKey {
            text: text.to_owned(),
            foreground,
            attributes: attributes.bits(),
            font_size_bits: font.size_points.to_bits(),
            layout_width_bits: layout_width.to_bits(),
            font_generation: font.font_set().generation(),
        };
        if let Some(layout) = self.layouts.get(&key) {
            return layout.clone();
        }
        if self.layouts.len() >= GLYPH_CACHE_CAPACITY {
            self.layouts.clear();
        }

        let mut job = LayoutJob::default();
        job.wrap.max_width = layout_width;
        job.break_on_newline = false;
        job.append(
            text,
            0.0,
            TextFormat {
                font_id: font.font_id_for_text(attributes, text),
                color: foreground,
                // Italic terminal cells use the bundled italic face rather
                // than synthetic skewing. This keeps metrics deterministic.
                italics: false,
                ..Default::default()
            },
        );
        let layout = painter.layout_job(job);
        self.layouts.insert(key, layout.clone());
        layout
    }

    fn paint_color_emoji(
        &mut self,
        painter: &egui::Painter,
        text: &str,
        rect: Rect,
        attributes: Attributes,
        color_emoji: bool,
    ) -> ColorEmojiPaintOutcome {
        if !color_emoji || attributes.contains(Attributes::CONCEALED) || !is_color_emoji(text) {
            return ColorEmojiPaintOutcome::NotPainted;
        }
        self.color_emoji
            .paint(painter, text, rect, attributes.contains(Attributes::FAINT))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColorEmojiPaintOutcome {
    NotPainted,
    TextureCacheHit,
    TextureCacheMiss,
    NegativeCacheHit,
    RasterizationFailed,
}

impl ColorEmojiPaintOutcome {
    const fn painted(self) -> bool {
        matches!(self, Self::TextureCacheHit | Self::TextureCacheMiss)
    }
}

const COLOR_EMOJI_CACHE_CAPACITY: usize = 512;
const COLOR_EMOJI_CACHE_BYTE_CAPACITY: usize = 32 * 1024 * 1024;
const MAX_COLOR_EMOJI_INPUT_BYTES: usize = 256;
const MAX_COLOR_EMOJI_LAYERS: usize = 64;
const MAX_COLOR_EMOJI_PIXELS: u32 = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ColorEmojiKey {
    text: String,
    pixel_size: u16,
}

struct ColorEmojiTexture {
    texture: TextureHandle,
    image: Option<Arc<ColorImage>>,
    aspect_ratio: f32,
    byte_size: usize,
}

struct ColorEmojiCache {
    textures: HashMap<ColorEmojiKey, ColorEmojiTexture>,
    failed: HashSet<ColorEmojiKey>,
    recency: VecDeque<ColorEmojiKey>,
    texture_bytes: usize,
    shape_context: ShapeContext,
    scale_context: ScaleContext,
}

impl Default for ColorEmojiCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            failed: HashSet::new(),
            recency: VecDeque::new(),
            texture_bytes: 0,
            shape_context: ShapeContext::new(),
            scale_context: ScaleContext::new(),
        }
    }
}

impl ColorEmojiCache {
    fn clear(&mut self) {
        self.textures.clear();
        self.failed.clear();
        self.recency.clear();
        self.texture_bytes = 0;
    }

    fn paint(
        &mut self,
        painter: &egui::Painter,
        text: &str,
        rect: Rect,
        faint: bool,
    ) -> ColorEmojiPaintOutcome {
        let pixels_per_point = painter.ctx().pixels_per_point();
        let pixel_size = (rect.height() * pixels_per_point)
            .round()
            .clamp(1.0, MAX_COLOR_EMOJI_PIXELS as f32) as u16;
        let key = ColorEmojiKey {
            text: text.to_owned(),
            pixel_size,
        };
        let outcome = if self.textures.contains_key(&key) {
            self.touch(&key);
            ColorEmojiPaintOutcome::TextureCacheHit
        } else if self.failed.contains(&key) {
            self.touch(&key);
            return ColorEmojiPaintOutcome::NegativeCacheHit;
        } else {
            let Some(image) = self.rasterize(text, pixel_size) else {
                self.prepare_for_insert(0);
                self.failed.insert(key.clone());
                self.touch(&key);
                return ColorEmojiPaintOutcome::RasterizationFailed;
            };
            let retain_image = crate::native_painter::installed(painter.ctx());
            let byte_size = image.width() * image.height() * 4 * if retain_image { 2 } else { 1 };
            self.prepare_for_insert(byte_size);
            let aspect_ratio = image.width() as f32 / image.height() as f32;
            let texture_name = format!(
                "festerm-color-emoji-{}-{}",
                stable_text_hash(text),
                pixel_size
            );
            let image = Arc::new(image);
            let retained_image = retain_image.then(|| image.clone());
            let texture = painter.ctx().load_texture(
                texture_name,
                egui::ImageData::Color(image),
                TextureOptions::LINEAR,
            );
            self.textures.insert(
                key.clone(),
                ColorEmojiTexture {
                    texture,
                    image: retained_image,
                    aspect_ratio,
                    byte_size,
                },
            );
            self.texture_bytes += byte_size;
            self.touch(&key);
            ColorEmojiPaintOutcome::TextureCacheMiss
        };
        let Some(entry) = self.textures.get(&key) else {
            return ColorEmojiPaintOutcome::RasterizationFailed;
        };
        if let Some(image) = &entry.image {
            crate::native_painter::record_texture(painter.ctx(), entry.texture.id(), image);
        }
        let max_size = rect.size() * 0.92;
        let size = if max_size.x / max_size.y > entry.aspect_ratio {
            Vec2::new(max_size.y * entry.aspect_ratio, max_size.y)
        } else {
            Vec2::new(max_size.x, max_size.x / entry.aspect_ratio)
        };
        let destination = Rect::from_center_size(rect.center(), size);
        painter.image(
            entry.texture.id(),
            destination,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            if faint {
                Color32::from_white_alpha(128)
            } else {
                Color32::WHITE
            },
        );
        outcome
    }

    fn prepare_for_insert(&mut self, byte_size: usize) {
        while self.textures.len().saturating_add(self.failed.len()) >= COLOR_EMOJI_CACHE_CAPACITY
            || self.texture_bytes.saturating_add(byte_size) > COLOR_EMOJI_CACHE_BYTE_CAPACITY
        {
            let Some(oldest) = self.recency.pop_front() else {
                self.clear();
                break;
            };
            if let Some(texture) = self.textures.remove(&oldest) {
                self.texture_bytes = self.texture_bytes.saturating_sub(texture.byte_size);
            } else {
                self.failed.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, key: &ColorEmojiKey) {
        if let Some(position) = self.recency.iter().position(|candidate| candidate == key) {
            self.recency.remove(position);
        }
        self.recency.push_back(key.clone());
    }

    fn rasterize(&mut self, text: &str, pixel_size: u16) -> Option<ColorImage> {
        if text.len() > MAX_COLOR_EMOJI_INPUT_BYTES {
            return None;
        }
        let font = FontRef::from_index(COLOR_EMOJI_BYTES, 0)?;
        let mut glyphs = Vec::new();
        let mut pen_x = 0.0;
        let mut too_many_layers = false;
        // This font exposes keycaps as foreground/background bitmap layers
        // instead of a single substituted glyph through Swash.
        let is_keycap = text.contains('\u{20e3}');
        {
            let mut shaper = self
                .shape_context
                .builder(font)
                .size(f32::from(pixel_size))
                .script(Script::Common)
                .build();
            shaper.add_str(text);
            shaper.shape_with(|cluster| {
                for glyph in cluster.glyphs {
                    if glyph.id != 0 {
                        if glyphs.len() >= MAX_COLOR_EMOJI_LAYERS {
                            too_many_layers = true;
                        } else {
                            let x = if is_keycap { glyph.x } else { pen_x + glyph.x };
                            glyphs.push((glyph.id, x, glyph.y));
                        }
                        pen_x += glyph.advance;
                    }
                }
            });
        }
        if glyphs.is_empty() || too_many_layers {
            return None;
        }
        let mut scaler = self
            .scale_context
            .builder(font)
            .size(f32::from(pixel_size))
            .hint(true)
            .build();
        let renderer = Render::new(&[
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::ColorOutline(0),
        ]);
        let mut layers = Vec::with_capacity(glyphs.len());
        let mut bounds = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for (glyph_id, x, y) in glyphs {
            let image = renderer.render(&mut scaler, glyph_id)?;
            if image.content != Content::Color
                || image.placement.width == 0
                || image.placement.height == 0
                || image.placement.width > u32::from(pixel_size) * 4
                || image.placement.height > u32::from(pixel_size) * 4
            {
                return None;
            }
            let left = x.round() as i32 + image.placement.left;
            let top = -(y.round() as i32 + image.placement.top);
            let right = left + image.placement.width as i32;
            let bottom = top + image.placement.height as i32;
            bounds.0 = bounds.0.min(left);
            bounds.1 = bounds.1.min(top);
            bounds.2 = bounds.2.max(right);
            bounds.3 = bounds.3.max(bottom);
            layers.push((image, left, top));
        }
        let width = bounds.2.checked_sub(bounds.0)? as usize;
        let height = bounds.3.checked_sub(bounds.1)? as usize;
        if width == 0
            || height == 0
            || width > usize::from(pixel_size) * 4
            || height > usize::from(pixel_size) * 4
        {
            return None;
        }
        if is_keycap {
            layers.reverse();
        }
        let mut pixels = vec![Color32::TRANSPARENT; width * height];
        for (image, left, top) in layers {
            let offset_x = (left - bounds.0) as usize;
            let offset_y = (top - bounds.1) as usize;
            for source_y in 0..image.placement.height as usize {
                for source_x in 0..image.placement.width as usize {
                    let source_index = (source_y * image.placement.width as usize + source_x) * 4;
                    let source = Color32::from_rgba_unmultiplied(
                        image.data[source_index],
                        image.data[source_index + 1],
                        image.data[source_index + 2],
                        image.data[source_index + 3],
                    );
                    let destination_index = (offset_y + source_y) * width + offset_x + source_x;
                    pixels[destination_index] = pixels[destination_index].blend(source);
                }
            }
        }
        Some(ColorImage::new([width, height], pixels))
    }
}

fn stable_text_hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[derive(Clone, Copy)]
pub(crate) struct GridLayout {
    pub(crate) rect: Rect,
    pub(crate) dimensions: Dimensions,
    pub(crate) metrics: crate::geometry::CellMetrics,
}

impl GridLayout {
    pub(crate) fn cell_geometry(self) -> CellGeometry {
        CellGeometry::new(self.rect.min, self.dimensions, self.metrics)
    }
}

pub(crate) struct GridPaint<'a> {
    pub(crate) layout: GridLayout,
    pub(crate) snapshot: TerminalSnapshot<'a>,
    pub(crate) cache: &'a TerminalRenderCache,
    pub(crate) selection: &'a Selection,
    pub(crate) fonts: &'a FontSettings,
    pub(crate) shape_cell_runs: bool,
    pub(crate) focused: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct GridPaintStats {
    pub(crate) color_emoji_paints: usize,
    pub(crate) color_emoji_cache_hits: usize,
    pub(crate) color_emoji_cache_misses: usize,
    pub(crate) color_emoji_rasterization_attempts: usize,
    pub(crate) color_emoji_rasterization_failures: usize,
    pub(crate) color_emoji_negative_cache_hits: usize,
}

impl GridPaintStats {
    fn record_color_emoji(&mut self, outcome: ColorEmojiPaintOutcome) {
        match outcome {
            ColorEmojiPaintOutcome::NotPainted => {}
            ColorEmojiPaintOutcome::TextureCacheHit => {
                self.color_emoji_paints += 1;
                self.color_emoji_cache_hits += 1;
            }
            ColorEmojiPaintOutcome::TextureCacheMiss => {
                self.color_emoji_paints += 1;
                self.color_emoji_cache_misses += 1;
                self.color_emoji_rasterization_attempts += 1;
            }
            ColorEmojiPaintOutcome::NegativeCacheHit => {
                self.color_emoji_negative_cache_hits += 1;
            }
            ColorEmojiPaintOutcome::RasterizationFailed => {
                self.color_emoji_rasterization_attempts += 1;
                self.color_emoji_rasterization_failures += 1;
            }
        }
    }
}

pub(crate) fn grid_cell_rect(layout: GridLayout, position: CellPosition, columns: usize) -> Rect {
    layout
        .cell_geometry()
        .cell_rect(position, columns)
        .expect("renderer requests an in-bounds leading-cell span")
}

pub(crate) fn rendered_cell_columns(
    cell: &RenderedCell,
    dimensions: Dimensions,
    column: usize,
) -> usize {
    cell.width
        .columns()
        .max(1)
        .min(dimensions.columns().saturating_sub(column))
}

pub(crate) fn rendered_cell_is_selected(
    selection: Option<CellRange>,
    position: CellPosition,
    columns: usize,
) -> bool {
    selection.is_some_and(|range| {
        (0..columns).any(|offset| {
            range.contains(CellPosition {
                column: position.column + offset,
                row: position.row,
            })
        })
    })
}

pub(crate) fn cell_needs_background_paint(cell: &RenderedCell, selected: bool) -> bool {
    selected || cell.background != Color::Default || cell.attributes.contains(Attributes::INVERSE)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GlyphRun {
    position: CellPosition,
    columns: usize,
    text: String,
    foreground: Color32,
    attributes: Attributes,
    selected: bool,
    has_hyperlink: bool,
    single_width_only: bool,
}

impl GlyphRun {
    #[cfg(test)]
    pub(crate) const fn position(&self) -> CellPosition {
        self.position
    }

    #[cfg(test)]
    pub(crate) const fn columns(&self) -> usize {
        self.columns
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    fn can_extend(&self, cell: &RenderedCell, foreground: Color32, selected: bool) -> bool {
        !self.selected
            && !selected
            && !self.has_hyperlink
            && self.single_width_only
            && !self.text.is_empty()
            && self.text.is_ascii()
            && cell.width == festerm_core::CellWidth::Single
            && !cell.text.is_empty()
            && cell.text.is_ascii()
            && self.foreground == foreground
            && self.attributes == cell.attributes
            && cell.hyperlink.is_none()
    }
}

/// Produces shaping runs without changing terminal-cell ownership.
///
/// Every run starts at a leading terminal cell and owns an explicit count of
/// physical columns. Wide cells, selections, style changes, and hyperlinks
/// create hard boundaries. The renderer may shape a run as one visual glyph
/// sequence, but cursor, selection, and hit testing keep using `CellGeometry`.
pub(crate) fn glyph_runs(
    cells: &[RenderedCell],
    row: usize,
    dimensions: Dimensions,
    selection: Option<CellRange>,
) -> Vec<GlyphRun> {
    let mut runs = Vec::new();
    for (column, cell) in cells.iter().enumerate() {
        if cell.width == festerm_core::CellWidth::Continuation {
            continue;
        }
        let position = CellPosition { column, row };
        let columns = rendered_cell_columns(cell, dimensions, column);
        let (foreground, _) = cell_colors(cell);
        let selected = rendered_cell_is_selected(selection, position, columns);
        let can_extend = runs.last().is_some_and(|run: &GlyphRun| {
            run.position.row == row
                && run.position.column + run.columns == column
                && run.can_extend(cell, foreground, selected)
        });
        if can_extend {
            let run = runs.last_mut().expect("run existence was checked");
            run.columns += columns;
            run.text.push_str(&cell.text);
        } else {
            runs.push(GlyphRun {
                position,
                columns,
                text: cell.text.clone(),
                foreground,
                attributes: cell.attributes,
                selected,
                has_hyperlink: cell.hyperlink.is_some(),
                single_width_only: cell.width == festerm_core::CellWidth::Single,
            });
        }
    }
    runs
}

pub(crate) fn paint_grid(
    painter: egui::Painter,
    paint: GridPaint<'_>,
    glyphs: &mut GlyphCache,
) -> GridPaintStats {
    paint_grid_with_clip_override(painter, paint, glyphs, GridTextClipOverride::Auto)
}

fn paint_grid_with_clip_override(
    painter: egui::Painter,
    paint: GridPaint<'_>,
    glyphs: &mut GlyphCache,
    clip_override: GridTextClipOverride,
) -> GridPaintStats {
    let Some(dimensions) = paint.cache.dimensions() else {
        return GridPaintStats::default();
    };
    let mut stats = GridPaintStats::default();
    let selection_range = paint.selection.range_in_snapshot(paint.snapshot);
    let clip_policy = GridGlyphClipPolicy::new(&painter, clip_override);
    crate::background::paint_background(&painter, paint.layout.rect);
    let native = crate::native_painter::Batch::begin(&painter, painter.clip_rect());
    for row in 0..dimensions.rows() {
        let Some(cells) = paint.cache.row(row) else {
            continue;
        };
        for (column, cell) in cells.iter().enumerate() {
            if cell.width == festerm_core::CellWidth::Continuation {
                continue;
            }
            let position = CellPosition { column, row };
            let columns = rendered_cell_columns(cell, dimensions, column);
            let rect = grid_cell_rect(paint.layout, position, columns);
            let (foreground, background) = cell_colors(cell);
            let selected = rendered_cell_is_selected(selection_range, position, columns);
            if cell_needs_background_paint(cell, selected) {
                painter.rect_filled(
                    rect,
                    0.0,
                    if selected {
                        SELECTION_BACKGROUND
                    } else {
                        background
                    },
                );
            }
            if !paint.shape_cell_runs && !cell.text.is_empty() {
                // Clip to this cell's rect. Some glyphs (notably box-drawing
                // corners/dots in certain bundled faces) can measure taller
                // than the "M"-derived cell height, so an unclipped paint can
                // bleed into an adjacent row; that row's later background
                // fill then overwrites part of the bled glyph, leaving only
                // a flat sliver visible. The run-shaping path below already
                // clips for the same reason.
                let clipped_painter = painter.with_clip_rect(rect);
                let outcome = glyphs.paint_color_emoji(
                    &clipped_painter,
                    &cell.text,
                    rect,
                    cell.attributes,
                    paint.fonts.font_set().color_emoji(),
                );
                stats.record_color_emoji(outcome);
                if !outcome.painted() {
                    let galley = glyphs.layout(
                        &painter,
                        &cell.text,
                        cell.attributes,
                        foreground,
                        paint.fonts,
                        rect.width(),
                    );
                    let text_position = Pos2::new(
                        rect.left(),
                        rect.top()
                            + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                    );
                    match clip_policy.galley_painter(rect, text_position, galley.as_ref()) {
                        GridTextClipPainter::SharedParent => {
                            painter.galley(text_position, galley, foreground);
                        }
                        GridTextClipPainter::ClippedCell => {
                            clipped_painter.galley(text_position, galley, foreground);
                        }
                    }
                }
            }
            let double_underline = cell.attributes.contains(Attributes::DOUBLE_UNDERLINE);
            if cell.attributes.contains(Attributes::UNDERLINE) || double_underline {
                let underline_y = rect.bottom() - if double_underline { 3.0 } else { 2.0 };
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), underline_y),
                        Pos2::new(rect.right(), underline_y),
                    ],
                    Stroke::new(1.0_f32, foreground),
                );
                if double_underline {
                    let underline_y = rect.bottom() - 1.0;
                    painter.line_segment(
                        [
                            Pos2::new(rect.left(), underline_y),
                            Pos2::new(rect.right(), underline_y),
                        ],
                        Stroke::new(1.0_f32, foreground),
                    );
                }
            }
            if cell.attributes.contains(Attributes::STRIKETHROUGH) {
                let strikethrough_y = rect.center().y;
                painter.line_segment(
                    [
                        Pos2::new(rect.left(), strikethrough_y),
                        Pos2::new(rect.right(), strikethrough_y),
                    ],
                    Stroke::new(1.0_f32, foreground),
                );
            }
        }
        if paint.shape_cell_runs {
            for run in glyph_runs(cells, row, dimensions, selection_range) {
                if run.text.is_empty() {
                    continue;
                }
                let rect = grid_cell_rect(paint.layout, run.position, run.columns);
                let clipped_painter = painter.with_clip_rect(rect);
                let outcome = glyphs.paint_color_emoji(
                    &clipped_painter,
                    &run.text,
                    rect,
                    run.attributes,
                    paint.fonts.font_set().color_emoji(),
                );
                stats.record_color_emoji(outcome);
                if !outcome.painted() {
                    let galley = glyphs.layout(
                        &painter,
                        &run.text,
                        run.attributes,
                        run.foreground,
                        paint.fonts,
                        rect.width(),
                    );
                    let text_position = Pos2::new(
                        rect.left(),
                        rect.top()
                            + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                    );
                    match clip_policy.galley_painter(rect, text_position, galley.as_ref()) {
                        GridTextClipPainter::SharedParent => {
                            painter.galley(text_position, galley, run.foreground);
                        }
                        GridTextClipPainter::ClippedCell => {
                            clipped_painter.galley(text_position, galley, run.foreground);
                        }
                    }
                }
            }
        }
    }

    if paint.snapshot.modes().cursor_visible() {
        let cursor = paint.snapshot.cursor_in_viewport();
        if let Some((cursor_column, cursor_row)) = cursor
            .filter(|(column, row)| *column < dimensions.columns() && *row < dimensions.rows())
        {
            let cell_rect = Rect::from_min_size(
                Pos2::new(
                    paint.layout.rect.left() + cursor_column as f32 * paint.layout.metrics.width,
                    paint.layout.rect.top() + cursor_row as f32 * paint.layout.metrics.height,
                ),
                Vec2::new(paint.layout.metrics.width, paint.layout.metrics.height),
            );
            let color = if paint.focused {
                DEFAULT_FOREGROUND
            } else {
                DEFAULT_FOREGROUND.gamma_multiply(0.5)
            };
            // Until the running program explicitly requests a cursor shape
            // via DECSCUSR, render a vertical bar rather than the
            // spec-mandated blinking-block reset state: a full hollow box
            // reads as "unfocused" even when it isn't, and a bar is the
            // more typical default cursor appearance for a fresh session.
            // `cursor_style()` itself is untouched and still reports the
            // spec-accurate value to anything that queries it.
            let style = if paint.snapshot.cursor_style_requested_by_program() {
                paint.snapshot.cursor_style()
            } else {
                CursorStyle::SteadyBar
            };
            let focused_block = paint.focused
                && matches!(style, CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock);
            paint_cursor(painter.clone(), cell_rect, style, color, paint.focused);
            if focused_block {
                // A filled block would otherwise fully hide the character
                // underneath; redraw it inverted (background-colored) on
                // top, matching every other terminal emulator's filled
                // block-cursor convention.
                if let Some(cell) = paint
                    .cache
                    .row(cursor_row)
                    .and_then(|row| row.get(cursor_column).filter(|cell| !cell.text.is_empty()))
                {
                    let galley = glyphs.layout(
                        &painter,
                        &cell.text,
                        cell.attributes,
                        DEFAULT_BACKGROUND,
                        paint.fonts,
                        cell_rect.width(),
                    );
                    let text_position = Pos2::new(
                        cell_rect.left(),
                        cell_rect.top()
                            + ((paint.layout.metrics.height - galley.size().y) / 2.0).max(0.0),
                    );
                    painter.galley(text_position, galley, DEFAULT_BACKGROUND);
                }
            }
        }
    }
    if let Some(native) = native {
        native.finish(&painter);
    }
    stats
}

fn paint_cursor(
    painter: egui::Painter,
    cell: Rect,
    style: CursorStyle,
    color: Color32,
    focused: bool,
) {
    match style {
        CursorStyle::BlinkingBlock | CursorStyle::SteadyBlock => {
            // Filled when focused (the conventional "this pane has the
            // keyboard" look shared by other terminal emulators), hollow
            // when not, so shape alone communicates focus state rather
            // than always drawing a hollow box regardless of focus.
            if focused {
                painter.rect_filled(cell, 0.0, color);
            } else {
                painter.rect_stroke(
                    cell.shrink(0.5),
                    0.0,
                    Stroke::new(1.0_f32, color),
                    StrokeKind::Inside,
                );
            }
        }
        CursorStyle::BlinkingUnderline | CursorStyle::SteadyUnderline => {
            painter.line_segment(
                [
                    Pos2::new(cell.left(), cell.bottom() - 1.0),
                    Pos2::new(cell.right(), cell.bottom() - 1.0),
                ],
                Stroke::new(1.0_f32, color),
            );
        }
        CursorStyle::BlinkingBar | CursorStyle::SteadyBar => {
            painter.line_segment(
                [
                    Pos2::new(cell.left() + 0.5, cell.top()),
                    Pos2::new(cell.left() + 0.5, cell.bottom()),
                ],
                Stroke::new(1.0_f32, color),
            );
        }
    }
}

pub(crate) fn cell_colors(cell: &RenderedCell) -> (Color32, Color32) {
    let mut foreground = resolve_color(cell.foreground, DEFAULT_FOREGROUND);
    let mut background = resolve_color(cell.background, DEFAULT_BACKGROUND);
    if cell.attributes.contains(Attributes::INVERSE) {
        std::mem::swap(&mut foreground, &mut background);
    }
    if cell.attributes.contains(Attributes::CONCEALED) {
        foreground = background;
    }
    if cell.attributes.contains(Attributes::FAINT) {
        foreground = foreground.gamma_multiply(0.6);
    }
    (foreground, background)
}

/// Resolves terminal colors using the xterm-style ANSI/256-color palette.
///
/// The palette itself lives in `festerm-core` because the core has to report
/// the same values through `OSC 4/10/11/12`. Painting and reporting resolve
/// through one table so a program cannot be told one color and shown another.
pub fn resolve_color(color: Color, default: Color32) -> Color32 {
    match color {
        Color::Default => default,
        other => {
            let resolved = terminal_color_scheme().resolve(other, Rgb::new(0, 0, 0));
            Color32::from_rgb(resolved.red, resolved.green, resolved.blue)
        }
    }
}

/// The colors this front end paints, in the form the core reports them.
///
/// The composition root hands this to each `Terminal` so that a color query
/// is answered with the shade actually on screen.
pub fn terminal_color_scheme() -> ColorScheme {
    ColorScheme::new(
        rgb_of(DEFAULT_FOREGROUND),
        rgb_of(DEFAULT_BACKGROUND),
        // `paint_terminal` draws the cursor in the default foreground color.
        rgb_of(DEFAULT_FOREGROUND),
        ColorScheme::DEFAULT_ANSI,
    )
}

fn rgb_of(color: Color32) -> Rgb {
    Rgb::new(color.r(), color.g(), color.b())
}

/// Measures through execution of `submit`, which is the point at which grid
/// shapes have been handed to egui rather than presented by the OS.
pub(crate) fn measure_input_to_paint_submission<T>(
    input_observed: Option<Instant>,
    submit: impl FnOnce() -> T,
) -> (T, Option<Duration>) {
    let submitted = submit();
    (submitted, input_observed.map(|started| started.elapsed()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_reported_scheme_is_the_palette_the_renderer_paints() {
        // A color query is only useful if its answer matches the pixels. The
        // two paths share `ColorScheme`, and this proves the sharing holds
        // for every entry, including the defaults the theme owns.
        let scheme = terminal_color_scheme();
        for index in 0..=255u8 {
            let reported = scheme.palette(index);
            assert_eq!(
                resolve_color(Color::Indexed(index), DEFAULT_BACKGROUND),
                Color32::from_rgb(reported.red, reported.green, reported.blue),
                "palette entry {index} is reported differently than it is painted"
            );
        }
        assert_eq!(
            scheme.foreground(),
            Rgb::new(
                DEFAULT_FOREGROUND.r(),
                DEFAULT_FOREGROUND.g(),
                DEFAULT_FOREGROUND.b()
            )
        );
        assert_eq!(
            scheme.background(),
            Rgb::new(
                DEFAULT_BACKGROUND.r(),
                DEFAULT_BACKGROUND.g(),
                DEFAULT_BACKGROUND.b()
            )
        );
        assert_eq!(
            scheme.cursor(),
            scheme.foreground(),
            "the cursor is drawn in the default foreground, so that is what OSC 12 must report"
        );
    }

    #[test]
    fn the_cores_stand_in_scheme_still_matches_this_theme() {
        // `festerm-core` carries its own defaults so headless callers answer
        // sensibly without an embedder. They are only honest while they agree
        // with the theme this front end actually paints.
        assert_eq!(ColorScheme::default(), terminal_color_scheme());
    }

    use std::{
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };

    use egui::epaint::Primitive;
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    use egui_kittest::Harness;
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    use egui_kittest::SnapshotResults;
    use egui_kittest::TestRenderer;
    use festerm_core::{
        Attributes, CellWidth, Color, Dimensions, InputEvent, InputEventOutcome, Key, Terminal,
    };
    use festerm_test_support::load_fixture;
    use icu_properties::{
        props::{Emoji, EmojiPresentation},
        CodePointSetData,
    };

    use super::*;
    use crate::{
        geometry::{dimensions_from_viewport, viewport_layout},
        input::route_input,
        CellMetrics, CellPosition, CellRange, EncodedInputSink, RenderedCell, ResizeOutcome,
        ResizeTracker, TerminalRenderCache, TerminalSnapshot, TerminalView, ViewSize,
        DEFAULT_BACKGROUND,
    };

    #[test]
    fn terminal_attributes_select_real_bundled_faces() {
        let font = FontSettings::default();
        for (attributes, expected) in [
            (Attributes::from_bits(0), REGULAR_FAMILY),
            (Attributes::BOLD, BOLD_FAMILY),
            (Attributes::ITALIC, ITALIC_FAMILY),
            (
                Attributes::from_bits(Attributes::BOLD.bits() | Attributes::ITALIC.bits()),
                BOLD_ITALIC_FAMILY,
            ),
        ] {
            assert_eq!(font.font_id(attributes).family.to_string(), expected);
        }
    }

    #[test]
    fn agency_emoji_use_the_owned_family_and_rasterize_as_color() {
        let font = FontSettings::default();
        let mut cache = ColorEmojiCache::default();
        for emoji in [
            "🤖",
            "🧹",
            "🧠",
            "🧩",
            "🟢",
            "🗑️",
            "⚠️",
            "ℹ️",
            "👩‍🔬",
            "1️⃣",
            "🇺🇸",
        ] {
            assert_eq!(
                font.font_id_for_text(Attributes::NONE, emoji)
                    .family
                    .to_string(),
                EMOJI_FAMILY
            );
            let image = cache
                .rasterize(emoji, 32)
                .unwrap_or_else(|| panic!("failed to rasterize {emoji}"));
            assert!(image.width() > 0);
            assert!(image.height() > 0);
            let visible_colors = image
                .pixels
                .iter()
                .filter(|pixel| pixel.a() != 0)
                .map(|pixel| (pixel.r(), pixel.g(), pixel.b()))
                .collect::<std::collections::HashSet<_>>();
            assert!(
                visible_colors.len() > 1,
                "{emoji} did not retain intrinsic color"
            );
        }
        assert_ne!(
            font.font_id_for_text(Attributes::NONE, "⚠︎")
                .family
                .to_string(),
            EMOJI_FAMILY
        );
    }

    #[test]
    fn complex_emoji_sequences_rasterize_at_supported_sizes() {
        let mut cache = ColorEmojiCache::default();
        let keycaps = ['#', '*']
            .into_iter()
            .chain('0'..='9')
            .map(|base| format!("{base}\u{fe0f}\u{20e3}"))
            .collect::<Vec<_>>();
        for emoji in crate::fonts::COMPLEX_COLOR_EMOJI_TEST_CASES
            .iter()
            .copied()
            .map(str::to_owned)
            .chain(keycaps)
        {
            for pixel_size in [8, 16, 32, 64, 128, 256] {
                let image = cache.rasterize(&emoji, pixel_size).unwrap_or_else(|| {
                    panic!("failed to rasterize {emoji} at {pixel_size} pixels")
                });
                assert!(image.width() > 0, "{emoji} at {pixel_size}");
                assert!(image.height() > 0, "{emoji} at {pixel_size}");
                assert!(
                    image.width() <= usize::from(pixel_size) * 4,
                    "{emoji} at {pixel_size}"
                );
                assert!(
                    image.height() <= usize::from(pixel_size) * 4,
                    "{emoji} at {pixel_size}"
                );
                assert!(
                    image.pixels.iter().any(|pixel| pixel.a() != 0),
                    "{emoji} at {pixel_size}"
                );
            }
        }
    }

    #[test]
    fn every_unicode_15_1_rgi_emoji_classifies_and_rasterizes() {
        let mut cache = ColorEmojiCache::default();
        for emoji in crate::fonts::unicode_emoji_15_1_fully_qualified() {
            assert!(
                is_color_emoji(&emoji),
                "{emoji} was not classified for color"
            );
            let image = cache
                .rasterize(&emoji, 16)
                .unwrap_or_else(|| panic!("failed to rasterize Unicode 15.1 RGI emoji {emoji}"));
            assert!(
                image.pixels.iter().any(|pixel| pixel.a() != 0),
                "{emoji} rendered transparently"
            );
        }
    }

    #[test]
    fn every_default_color_emoji_scalar_rasterizes_from_the_pinned_font() {
        let mut cache = ColorEmojiCache::default();
        for range in CodePointSetData::new::<EmojiPresentation>().iter_ranges() {
            for code_point in range {
                let text = char::from_u32(code_point).unwrap().to_string();
                let image = cache
                    .rasterize(&text, 16)
                    .unwrap_or_else(|| panic!("failed to rasterize U+{code_point:04X} {text}"));
                assert!(
                    image.pixels.iter().any(|pixel| pixel.a() != 0),
                    "U+{code_point:04X} rendered transparently"
                );
            }
        }
    }

    #[test]
    fn every_emoji_property_scalar_rasterizes_with_explicit_emoji_presentation() {
        let mut cache = ColorEmojiCache::default();
        for range in CodePointSetData::new::<Emoji>().iter_ranges() {
            for code_point in range {
                let character = char::from_u32(code_point).unwrap();
                let text = format!("{character}\u{fe0f}");
                let image = cache
                    .rasterize(&text, 16)
                    .unwrap_or_else(|| panic!("failed to rasterize U+{code_point:04X} with VS16"));
                assert!(
                    image.pixels.iter().any(|pixel| pixel.a() != 0),
                    "U+{code_point:04X} with VS16 rendered transparently"
                );
            }
        }
    }

    #[test]
    fn color_emoji_paint_reuses_textures_and_honors_concealment() {
        let context = egui::Context::default();
        let mut glyphs = GlyphCache::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(32.0, 16.0));

        let mut outcome = ColorEmojiPaintOutcome::TextureCacheHit;
        let mut output = context.run_ui(Default::default(), |context| {
            let painter = context.layer_painter(egui::LayerId::background());
            outcome = glyphs.paint_color_emoji(&painter, "🤖", rect, Attributes::CONCEALED, true);
        });
        output.textures_delta.clear();
        assert_eq!(outcome, ColorEmojiPaintOutcome::NotPainted);
        assert!(glyphs.color_emoji.textures.is_empty());

        for (attributes, expected) in [
            (Attributes::NONE, ColorEmojiPaintOutcome::TextureCacheMiss),
            (Attributes::FAINT, ColorEmojiPaintOutcome::TextureCacheHit),
        ] {
            let mut output = context.run_ui(Default::default(), |context| {
                let painter = context.layer_painter(egui::LayerId::background());
                outcome = glyphs.paint_color_emoji(&painter, "🤖", rect, attributes, true);
            });
            output.textures_delta.clear();
            assert_eq!(outcome, expected);
        }
        assert_eq!(glyphs.color_emoji.textures.len(), 1);
        assert!(glyphs.color_emoji.texture_bytes > 0);
    }

    #[test]
    fn monochrome_policy_skips_color_emoji_textures() {
        let context = egui::Context::default();
        let mut glyphs = GlyphCache::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(32.0, 16.0));

        let mut outcome = ColorEmojiPaintOutcome::TextureCacheHit;
        let mut output = context.run_ui(Default::default(), |context| {
            let painter = context.layer_painter(egui::LayerId::background());
            outcome = glyphs.paint_color_emoji(&painter, "🤖", rect, Attributes::NONE, false);
        });
        output.textures_delta.clear();

        assert_eq!(outcome, ColorEmojiPaintOutcome::NotPainted);
        assert!(glyphs.color_emoji.textures.is_empty());
        assert_eq!(glyphs.color_emoji.texture_bytes, 0);
    }

    #[test]
    fn failed_color_emoji_rasterization_is_negative_cached() {
        let context = egui::Context::default();
        let mut glyphs = GlyphCache::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(32.0, 16.0));
        let text = format!("1{}", "\u{20e3}".repeat(MAX_COLOR_EMOJI_LAYERS));

        let mut outcome = ColorEmojiPaintOutcome::NotPainted;
        for expected in [
            ColorEmojiPaintOutcome::RasterizationFailed,
            ColorEmojiPaintOutcome::NegativeCacheHit,
        ] {
            let mut output = context.run_ui(Default::default(), |context| {
                let painter = context.layer_painter(egui::LayerId::background());
                outcome = glyphs.paint_color_emoji(&painter, &text, rect, Attributes::NONE, true);
            });
            output.textures_delta.clear();
            assert_eq!(outcome, expected);
        }
        assert_eq!(glyphs.color_emoji.failed.len(), 1);
        assert!(glyphs.color_emoji.textures.is_empty());
    }

    #[test]
    fn color_emoji_rasterizer_rejects_missing_and_excessive_inputs() {
        let mut cache = ColorEmojiCache::default();
        assert!(cache.rasterize("\u{e000}", 16).is_none());
        assert!(cache
            .rasterize(
                &format!("1{}", "\u{20e3}".repeat(MAX_COLOR_EMOJI_LAYERS)),
                16
            )
            .is_none());
        assert!(cache
            .rasterize(&"🤖".repeat(MAX_COLOR_EMOJI_INPUT_BYTES), 16)
            .is_none());
    }

    #[test]
    fn every_keycap_raster_has_distinct_visible_pixels() {
        let mut cache = ColorEmojiCache::default();
        let mut hashes = std::collections::HashSet::new();
        for base in ['#', '*'].into_iter().chain('0'..='9') {
            let emoji = format!("{base}\u{fe0f}\u{20e3}");
            let image = cache
                .rasterize(&emoji, 32)
                .unwrap_or_else(|| panic!("failed to rasterize {emoji}"));
            let hash = image.pixels.iter().fold(
                (image.width() as u64) << 32 | image.height() as u64,
                |hash, pixel| {
                    pixel.to_array().into_iter().fold(hash, |hash, byte| {
                        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
                    })
                },
            );
            assert!(hashes.insert(hash), "duplicate keycap raster for {emoji}");
        }
    }

    #[test]
    fn color_emoji_texture_cache_stays_bounded() {
        let context = egui::Context::default();
        let image = ColorImage::new([1, 1], vec![Color32::WHITE]);
        let texture = context.load_texture("emoji-cache-test", image, TextureOptions::LINEAR);
        let mut cache = ColorEmojiCache::default();
        for index in 0..COLOR_EMOJI_CACHE_CAPACITY {
            let byte_size = 4;
            cache.textures.insert(
                ColorEmojiKey {
                    text: index.to_string(),
                    pixel_size: 16,
                },
                ColorEmojiTexture {
                    texture: texture.clone(),
                    image: None,
                    aspect_ratio: 1.0,
                    byte_size,
                },
            );
            cache.texture_bytes += byte_size;
            cache.touch(&ColorEmojiKey {
                text: index.to_string(),
                pixel_size: 16,
            });
        }
        cache.prepare_for_insert(4);
        assert_eq!(cache.textures.len(), COLOR_EMOJI_CACHE_CAPACITY - 1);
        assert_eq!(cache.texture_bytes, (COLOR_EMOJI_CACHE_CAPACITY - 1) * 4);
        assert!(!cache.textures.contains_key(&ColorEmojiKey {
            text: "0".to_owned(),
            pixel_size: 16,
        }));

        cache.clear();
        cache.textures.insert(
            ColorEmojiKey {
                text: "🤖".to_owned(),
                pixel_size: 16,
            },
            ColorEmojiTexture {
                texture,
                image: None,
                aspect_ratio: 1.0,
                byte_size: COLOR_EMOJI_CACHE_BYTE_CAPACITY,
            },
        );
        cache.texture_bytes = COLOR_EMOJI_CACHE_BYTE_CAPACITY;
        cache.touch(&ColorEmojiKey {
            text: "🤖".to_owned(),
            pixel_size: 16,
        });
        cache.prepare_for_insert(1);
        assert!(cache.textures.is_empty());
        assert_eq!(cache.texture_bytes, 0);

        for index in 0..COLOR_EMOJI_CACHE_CAPACITY {
            let key = ColorEmojiKey {
                text: index.to_string(),
                pixel_size: 16,
            };
            cache.failed.insert(key.clone());
            cache.touch(&key);
        }
        cache.prepare_for_insert(0);
        assert_eq!(cache.failed.len(), COLOR_EMOJI_CACHE_CAPACITY - 1);
        assert!(!cache.failed.contains(&ColorEmojiKey {
            text: "0".to_owned(),
            pixel_size: 16,
        }));

        assert!(cache
            .rasterize(&"🤖".repeat(MAX_COLOR_EMOJI_INPUT_BYTES), 16)
            .is_none());
    }

    #[test]
    fn capacity_eviction_preserves_newly_visible_emoji_reuse() {
        let context = egui::Context::default();
        let image = ColorImage::new([1, 1], vec![Color32::WHITE]);
        let texture = context.load_texture("emoji-capacity-test", image, TextureOptions::LINEAR);
        let mut glyphs = GlyphCache::default();
        for index in 0..COLOR_EMOJI_CACHE_CAPACITY - 1 {
            let key = ColorEmojiKey {
                text: index.to_string(),
                pixel_size: 16,
            };
            glyphs.color_emoji.textures.insert(
                key.clone(),
                ColorEmojiTexture {
                    texture: texture.clone(),
                    image: None,
                    aspect_ratio: 1.0,
                    byte_size: 4,
                },
            );
            glyphs.color_emoji.texture_bytes += 4;
            glyphs.color_emoji.touch(&key);
        }

        let mut outcomes = Vec::new();
        let mut output = context.run_ui(Default::default(), |context| {
            let painter = context.layer_painter(egui::LayerId::background());
            for height in [16.0, 17.0, 16.0] {
                outcomes.push(glyphs.paint_color_emoji(
                    &painter,
                    "🤖",
                    Rect::from_min_size(Pos2::ZERO, Vec2::new(32.0, height)),
                    Attributes::NONE,
                    true,
                ));
            }
        });
        output.textures_delta.clear();

        assert_eq!(
            outcomes,
            [
                ColorEmojiPaintOutcome::TextureCacheMiss,
                ColorEmojiPaintOutcome::TextureCacheMiss,
                ColorEmojiPaintOutcome::TextureCacheHit,
            ]
        );
        assert_eq!(
            glyphs.color_emoji.textures.len() + glyphs.color_emoji.failed.len(),
            COLOR_EMOJI_CACHE_CAPACITY
        );
    }

    #[test]
    fn ligature_policy_selects_shaped_faces_and_collapses_standard_operators() {
        for family in [
            crate::TerminalFontFamily::JetBrainsMono,
            crate::TerminalFontFamily::IosevkaTerm,
            crate::TerminalFontFamily::JuliaMono,
            crate::TerminalFontFamily::MapleMono,
        ] {
            let context = egui::Context::default();
            let generation = crate::install_terminal_font_family(&context, family);
            let mut ligature_spans = Vec::new();

            let mut output = context.run_ui(Default::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    for text in ["!=", "==", "->", "=>", "::", "<=", "==="] {
                        let mut font = FontSettings::default();
                        font.set_font_set(crate::TerminalFontSet::new(family, true, generation));
                        let galley = ui.painter().layout_no_wrap(
                            text.to_owned(),
                            font.regular_font_id(),
                            Color32::WHITE,
                        );
                        let spans_multiple_cells = galley
                            .rows
                            .iter()
                            .flat_map(|row| &row.glyphs)
                            .any(|glyph| glyph.uv_rect.size.x > glyph.advance_width + 0.5);
                        ligature_spans.push((text, spans_multiple_cells));
                    }
                });
            });
            output.textures_delta.clear();

            assert!(
                ligature_spans
                    .iter()
                    .any(|(_, spans_multiple_cells)| *spans_multiple_cells),
                "{family:?} exposes no standard programming ligature"
            );
        }
    }

    #[derive(Default)]
    struct Sink(Vec<Vec<u8>>);

    impl EncodedInputSink for Sink {
        fn record_encoded_input(&mut self, bytes: &[u8]) {
            self.0.push(bytes.to_vec());
        }
    }

    fn terminal(columns: usize, rows: usize) -> Terminal {
        Terminal::new(Dimensions::new(columns, rows).expect("valid test size"))
            .expect("test terminal allocation")
    }

    fn grid_layout(columns: usize, rows: usize) -> GridLayout {
        GridLayout {
            rect: Rect::from_min_size(
                Pos2::new(5.0, 7.0),
                Vec2::new(columns as f32 * 10.0, rows as f32 * 20.0),
            ),
            dimensions: Dimensions::new(columns, rows).expect("valid test size"),
            metrics: CellMetrics::new(10.0, 20.0).expect("valid test cell metrics"),
        }
    }

    #[derive(Clone, Copy)]
    struct RenderGridScenario {
        family: crate::TerminalFontFamily,
        ligatures: bool,
        pixels_per_point: f32,
        origin: Pos2,
        viewport_clip: Option<Rect>,
        metrics: CellMetrics,
        shape_cell_runs: bool,
        transform: Option<egui::emath::TSTransform>,
        debug_clip_rects: bool,
        debug_text_rects: bool,
        debug_ignore_clip_rects: bool,
    }

    impl RenderGridScenario {
        fn screen_size(self, dimensions: Dimensions) -> Vec2 {
            let clip = self.viewport_clip.unwrap_or_else(|| {
                Rect::from_min_size(
                    self.origin,
                    Vec2::new(
                        dimensions.columns() as f32 * self.metrics.width,
                        dimensions.rows() as f32 * self.metrics.height,
                    ),
                )
            });
            clip.max.to_vec2() + Vec2::splat(16.0)
        }
    }

    impl Default for RenderGridScenario {
        fn default() -> Self {
            Self {
                family: crate::TerminalFontFamily::JetBrainsMono,
                ligatures: false,
                pixels_per_point: 1.0,
                origin: Pos2::new(5.0, 7.0),
                viewport_clip: None,
                metrics: CellMetrics::new(10.0, 20.0).expect("valid test metrics"),
                shape_cell_runs: false,
                transform: None,
                debug_clip_rects: false,
                debug_text_rects: false,
                debug_ignore_clip_rects: false,
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct RenderCounts {
        primitives: usize,
        mesh_primitives: usize,
    }

    struct RenderGridCapture {
        output: egui::FullOutput,
        context: egui::Context,
        counts: RenderCounts,
    }

    impl Drop for RenderGridCapture {
        fn drop(&mut self) {
            self.output.textures_delta.clear();
        }
    }

    fn render_grid_capture(
        dimensions: Dimensions,
        content: &[u8],
        scenario: RenderGridScenario,
        clip_override: GridTextClipOverride,
    ) -> RenderGridCapture {
        let context = egui::Context::default();
        let generation = crate::install_terminal_font_family(&context, scenario.family);
        let mut fonts = FontSettings::default();
        fonts.set_font_set(crate::TerminalFontSet::new(
            scenario.family,
            scenario.ligatures,
            generation,
        ));

        let mut terminal = Terminal::new(dimensions).expect("test terminal allocation");
        terminal.ingest(content);
        let dirty_rows = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let mut cache = TerminalRenderCache::default();
        cache.update(snapshot, &dirty_rows);
        let layout = GridLayout {
            rect: Rect::from_min_size(
                scenario.origin,
                Vec2::new(
                    dimensions.columns() as f32 * scenario.metrics.width,
                    dimensions.rows() as f32 * scenario.metrics.height,
                ),
            ),
            dimensions,
            metrics: scenario.metrics,
        };
        let viewport_clip = scenario.viewport_clip.unwrap_or(layout.rect);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, scenario.screen_size(dimensions));
        let mut glyphs = GlyphCache::default();
        let selection = Selection::default();

        let mut input = egui::RawInput {
            screen_rect: Some(screen_rect),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("root viewport exists")
            .native_pixels_per_point = Some(scenario.pixels_per_point);
        let output = context.run_ui(input, |context| {
            context.tessellation_options_mut(|options| {
                options.debug_paint_clip_rects = scenario.debug_clip_rects;
                options.debug_paint_text_rects = scenario.debug_text_rects;
                options.debug_ignore_clip_rects = scenario.debug_ignore_clip_rects;
            });
            let painter = context.layer_painter(egui::LayerId::background());
            if let Some(transform) = scenario.transform {
                context.set_transform_layer(painter.layer_id(), transform);
            }
            paint_grid_with_clip_override(
                painter.with_clip_rect(viewport_clip),
                GridPaint {
                    layout,
                    snapshot,
                    cache: &cache,
                    selection: &selection,
                    fonts: &fonts,
                    shape_cell_runs: scenario.shape_cell_runs,
                    focused: false,
                },
                &mut glyphs,
                clip_override,
            );
        });
        let primitives = context.tessellate(output.shapes.clone(), context.pixels_per_point());
        let counts = RenderCounts {
            primitives: primitives.len(),
            mesh_primitives: primitives
                .iter()
                .filter(|primitive| matches!(primitive.primitive, Primitive::Mesh(_)))
                .count(),
        };
        RenderGridCapture {
            output,
            context,
            counts,
        }
    }

    fn render_grid_pixels(
        dimensions: Dimensions,
        content: &[u8],
        scenario: RenderGridScenario,
        clip_override: GridTextClipOverride,
    ) -> Vec<u8> {
        let mut capture = render_grid_capture(dimensions, content, scenario, clip_override);
        let mut renderer = egui_kittest::wgpu::WgpuTestRenderer::new();
        renderer.handle_delta(&mut capture.output.textures_delta);
        renderer
            .render(&capture.context, &capture.output)
            .expect("grid render succeeds")
            .into_raw()
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    struct HeadlessViewState {
        view: TerminalView,
        terminal: Terminal,
        sink: Sink,
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    impl HeadlessViewState {
        fn with_terminal(terminal: Terminal) -> Self {
            Self {
                view: TerminalView::default(),
                terminal,
                sink: Sink::default(),
            }
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn visual_harness(terminal: Terminal) -> Harness<'static, HeadlessViewState> {
        sized_visual_harness(terminal, Vec2::new(640.0, 360.0))
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn sized_visual_harness(terminal: Terminal, size: Vec2) -> Harness<'static, HeadlessViewState> {
        Harness::builder()
            .with_size(size)
            .with_pixels_per_point(1.0)
            .with_theme(egui::Theme::Dark)
            .wgpu()
            .build_ui_state(
                |ui, state: &mut HeadlessViewState| {
                    state.view.show(ui, &mut state.terminal, &mut state.sink);
                },
                HeadlessViewState::with_terminal(terminal),
            )
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn assert_snapshot_invariants(harness: &Harness<'_, HeadlessViewState>) {
        let state = harness.state();
        assert_eq!(
            state.view.diagnostics().calculated_dimensions,
            Some(state.terminal.dimensions())
        );
        assert_eq!(
            state.view.cache.dimensions(),
            Some(state.terminal.dimensions())
        );
        assert!(state
            .view
            .diagnostics()
            .grid_rect
            .is_some_and(|grid| grid.is_finite() && grid.width() > 0.0 && grid.height() > 0.0));
        for row in 0..state.terminal.dimensions().rows() {
            assert_eq!(
                state.view.cache.row(row).map(<[RenderedCell]>::len),
                Some(state.terminal.dimensions().columns())
            );
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn snapshot_after_structural_assertions(
        harness: &mut Harness<'_, HeadlessViewState>,
        name: &str,
        snapshots: &mut SnapshotResults,
    ) {
        assert_snapshot_invariants(harness);
        let platform_name = if cfg!(target_os = "windows") {
            format!("{name}-windows")
        } else {
            name.to_owned()
        };
        snapshots.add(harness.try_snapshot(&platform_name));
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    fn focus_terminal_grid(harness: &mut Harness<'_, HeadlessViewState>) {
        let grid = harness
            .state()
            .view
            .diagnostics()
            .grid_rect
            .expect("rendered frame records grid geometry");
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.event(egui::Event::PointerButton {
            pos: grid.center(),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
    }

    #[test]
    fn p6_glyph_runs_preserve_terminal_cell_boundaries() {
        let single = |text: &str| RenderedCell {
            text: text.to_owned(),
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            attributes: Attributes::NONE,
            hyperlink: None,
        };
        let wide = RenderedCell {
            text: "界".to_owned(),
            width: CellWidth::Double,
            ..single("")
        };
        let continuation = RenderedCell {
            width: CellWidth::Continuation,
            ..single("")
        };
        let linked = RenderedCell {
            hyperlink: Some(Arc::<str>::from("https://example.com")),
            ..single("x")
        };
        let fallback = single("\u{1f980}");
        let styled = RenderedCell {
            attributes: Attributes::BOLD,
            ..single("z")
        };
        let cells = vec![
            single("="),
            single("="),
            wide,
            continuation,
            single("e\u{301}"),
            fallback,
            linked,
            single("y"),
            styled,
            single("w"),
        ];
        let dimensions = Dimensions::new(cells.len(), 1).unwrap();
        let runs = glyph_runs(&cells, 0, dimensions, None);

        assert_eq!(runs.len(), 8);
        assert_eq!(runs[0].position(), CellPosition { column: 0, row: 0 });
        assert_eq!(runs[0].columns(), 2);
        assert_eq!(runs[0].text(), "==");
        assert_eq!(runs[1].position(), CellPosition { column: 2, row: 0 });
        assert_eq!(runs[1].columns(), 2);
        assert_eq!(runs[1].text(), "界");
        assert_eq!(runs[2].position(), CellPosition { column: 4, row: 0 });
        assert_eq!(runs[2].columns(), 1);
        assert_eq!(runs[2].text(), "e\u{301}");
        assert_eq!(runs[3].position(), CellPosition { column: 5, row: 0 });
        assert_eq!(runs[4].position(), CellPosition { column: 6, row: 0 });
        assert_eq!(runs[5].position(), CellPosition { column: 7, row: 0 });
        assert_eq!(runs[6].position(), CellPosition { column: 8, row: 0 });
        assert_eq!(runs[7].position(), CellPosition { column: 9, row: 0 });

        let selected = glyph_runs(
            &cells[..2],
            0,
            Dimensions::new(2, 1).unwrap(),
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 1, row: 0 },
            )),
        );
        assert_eq!(
            selected.len(),
            2,
            "selection remains a hard shaping boundary for future selected-text styling"
        );

        let separated = glyph_runs(
            &[single("="), single(""), single("=")],
            0,
            Dimensions::new(3, 1).unwrap(),
            None,
        );
        assert_eq!(separated.len(), 3);
        assert_eq!(separated[2].position(), CellPosition { column: 2, row: 0 });
    }

    #[test]
    fn glyph_clip_batching_reduces_meshes_for_dense_ascii_without_changing_pixels() {
        let dimensions = Dimensions::new(80, 6).unwrap();
        let content = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?\r\n\
                        0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?\r\n\
                        0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?\r\n\
                        0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?\r\n\
                        0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?\r\n\
                        0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!?";
        let scenario = RenderGridScenario::default();

        let baseline = render_grid_capture(
            dimensions,
            content,
            scenario,
            GridTextClipOverride::AlwaysCell,
        );
        let optimized =
            render_grid_capture(dimensions, content, scenario, GridTextClipOverride::Auto);

        assert!(
            optimized.counts.primitives < baseline.counts.primitives,
            "expected fewer clipped primitives, baseline {:?} optimized {:?}",
            baseline.counts,
            optimized.counts
        );
        assert!(
            optimized.counts.mesh_primitives + dimensions.columns()
                < baseline.counts.mesh_primitives,
            "dense ASCII should materially reduce text meshes, baseline {:?} optimized {:?}",
            baseline.counts,
            optimized.counts
        );
        assert_eq!(
            render_grid_pixels(
                dimensions,
                content,
                scenario,
                GridTextClipOverride::AlwaysCell,
            ),
            render_grid_pixels(dimensions, content, scenario, GridTextClipOverride::Auto)
        );
    }

    #[test]
    fn glyph_clip_batching_preserves_pixels_across_fonts_styles_and_dpi() {
        let dimensions = Dimensions::new(32, 3).unwrap();
        let families = [
            crate::TerminalFontFamily::JetBrainsMono,
            crate::TerminalFontFamily::IosevkaTerm,
            crate::TerminalFontFamily::JuliaMono,
            crate::TerminalFontFamily::MapleMono,
        ];
        let scenarios = [
            (
                "regular",
                "ASCII text 12345\r\nnext line".as_bytes(),
                RenderGridScenario {
                    pixels_per_point: 1.5,
                    origin: Pos2::new(5.25, 7.125),
                    ..RenderGridScenario::default()
                },
            ),
            (
                "bold",
                "\x1b[1mBOLD ASCII 12345\x1b[0m".as_bytes(),
                RenderGridScenario {
                    pixels_per_point: 2.0,
                    origin: Pos2::new(1.5, 2.5),
                    ..RenderGridScenario::default()
                },
            ),
            (
                "italic",
                "\x1b[3mitalic ASCII 12345\x1b[0m".as_bytes(),
                RenderGridScenario {
                    pixels_per_point: 1.0,
                    origin: Pos2::new(3.5, 4.0),
                    ..RenderGridScenario::default()
                },
            ),
            (
                "ligatures",
                "== != -> => :: <= ===".as_bytes(),
                RenderGridScenario {
                    ligatures: true,
                    shape_cell_runs: true,
                    pixels_per_point: 1.5,
                    origin: Pos2::new(5.25, 7.125),
                    ..RenderGridScenario::default()
                },
            ),
            (
                "unicode",
                "wide 界 combining e\u{301}\r\nbox ┌─┐\r\n└─┘".as_bytes(),
                RenderGridScenario {
                    pixels_per_point: 2.0,
                    origin: Pos2::new(4.25, 6.5),
                    ..RenderGridScenario::default()
                },
            ),
        ];

        for family in families {
            for (name, content, scenario) in scenarios {
                let scenario = RenderGridScenario { family, ..scenario };
                assert_eq!(
                    render_grid_pixels(
                        dimensions,
                        content,
                        scenario,
                        GridTextClipOverride::AlwaysCell,
                    ),
                    render_grid_pixels(dimensions, content, scenario, GridTextClipOverride::Auto),
                    "pixel output changed for {family:?} / {name}"
                );
            }
        }
    }

    #[test]
    fn glyph_clip_batching_falls_back_for_overflow_transforms_debug_and_viewport_edges() {
        let dimensions = Dimensions::new(8, 2).unwrap();

        let overflow = RenderGridScenario {
            metrics: CellMetrics::new(4.0, 20.0).unwrap(),
            pixels_per_point: 1.5,
            origin: Pos2::new(5.25, 7.125),
            ..RenderGridScenario::default()
        };
        let overflow_baseline = render_grid_capture(
            dimensions,
            "\x1b[3mWWWWWWWW\x1b[0m".as_bytes(),
            overflow,
            GridTextClipOverride::AlwaysCell,
        );
        let overflow_optimized = render_grid_capture(
            dimensions,
            "\x1b[3mWWWWWWWW\x1b[0m".as_bytes(),
            overflow,
            GridTextClipOverride::Auto,
        );
        assert_eq!(overflow_baseline.counts, overflow_optimized.counts);
        assert_eq!(
            render_grid_pixels(
                dimensions,
                "\x1b[3mWWWWWWWW\x1b[0m".as_bytes(),
                overflow,
                GridTextClipOverride::AlwaysCell,
            ),
            render_grid_pixels(
                dimensions,
                "\x1b[3mWWWWWWWW\x1b[0m".as_bytes(),
                overflow,
                GridTextClipOverride::Auto,
            )
        );

        let viewport_edge = RenderGridScenario {
            pixels_per_point: 2.0,
            origin: Pos2::new(2.5, 3.5),
            viewport_clip: Some(Rect::from_min_max(
                Pos2::new(2.5, 3.5),
                Pos2::new(2.5 + 6.25 * 10.0, 3.5 + 2.0 * 20.0),
            )),
            ..RenderGridScenario::default()
        };
        assert_eq!(
            render_grid_pixels(
                dimensions,
                "ABCDEFZH".as_bytes(),
                viewport_edge,
                GridTextClipOverride::AlwaysCell,
            ),
            render_grid_pixels(
                dimensions,
                "ABCDEFZH".as_bytes(),
                viewport_edge,
                GridTextClipOverride::Auto,
            )
        );

        let transformed = RenderGridScenario {
            transform: Some(egui::emath::TSTransform::from_translation(Vec2::new(
                1.0, 0.0,
            ))),
            ..RenderGridScenario::default()
        };
        let transformed_baseline = render_grid_capture(
            dimensions,
            "transformed".as_bytes(),
            transformed,
            GridTextClipOverride::AlwaysCell,
        );
        let transformed_optimized = render_grid_capture(
            dimensions,
            "transformed".as_bytes(),
            transformed,
            GridTextClipOverride::Auto,
        );
        assert_eq!(transformed_baseline.counts, transformed_optimized.counts);

        for scenario in [
            RenderGridScenario {
                debug_clip_rects: true,
                ..RenderGridScenario::default()
            },
            RenderGridScenario {
                debug_text_rects: true,
                ..RenderGridScenario::default()
            },
            RenderGridScenario {
                debug_ignore_clip_rects: true,
                ..RenderGridScenario::default()
            },
        ] {
            let baseline = render_grid_capture(
                dimensions,
                "debug view".as_bytes(),
                scenario,
                GridTextClipOverride::AlwaysCell,
            );
            let optimized = render_grid_capture(
                dimensions,
                "debug view".as_bytes(),
                scenario,
                GridTextClipOverride::Auto,
            );
            assert_eq!(baseline.counts, optimized.counts);
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn terminal_view_emoji_policy_switches_color_texture_submission() {
        let mut emoji_terminal = terminal(20, 2);
        emoji_terminal.ingest("🤖 aligned".as_bytes());
        let mut harness = visual_harness(emoji_terminal);
        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default().with_color_emoji(false));

        harness.run();
        harness.run();
        assert_eq!(harness.state().view.diagnostics().color_emoji_paints, 0);
        assert_eq!(
            harness.state().view.diagnostics().color_emoji_cache_misses,
            0
        );
        let terminal_text = harness.state().terminal.row_text(0);

        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default());
        harness.run();
        assert_eq!(harness.state().view.diagnostics().color_emoji_paints, 1);
        assert_eq!(
            harness.state().view.diagnostics().color_emoji_cache_misses,
            1
        );
        assert_eq!(harness.state().terminal.row_text(0), terminal_text);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn repeated_emoji_frame_rasterizes_once_then_uses_only_cache_hits() {
        let mut emoji_terminal = terminal(20, 2);
        emoji_terminal.ingest("🤖 🤖 🤖".as_bytes());
        let mut harness = visual_harness(emoji_terminal);

        harness.run();
        harness.run();
        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default().with_color_emoji(false));
        harness.run();
        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default());
        harness.run();
        let cold = harness.state().view.diagnostics();
        assert_eq!(cold.color_emoji_paints, 3);
        assert_eq!(cold.color_emoji_cache_misses, 1);
        assert_eq!(cold.color_emoji_cache_hits, 2);
        assert_eq!(cold.color_emoji_rasterization_attempts, 1);
        assert_eq!(cold.color_emoji_rasterization_failures, 0);
        assert_eq!(cold.color_emoji_negative_cache_hits, 0);

        harness.run();
        let warm = harness.state().view.diagnostics();
        assert_eq!(warm.color_emoji_paints, 3);
        assert_eq!(warm.color_emoji_cache_misses, 0);
        assert_eq!(warm.color_emoji_cache_hits, 3);
        assert_eq!(warm.color_emoji_rasterization_attempts, 0);
        assert_eq!(warm.color_emoji_rasterization_failures, 0);
        assert_eq!(warm.color_emoji_negative_cache_hits, 0);
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn failed_emoji_rasterization_attempts_once_then_uses_negative_cache() {
        let mut emoji_terminal = terminal(10, 2);
        let excessive_keycap = format!("1{}", "\u{20e3}".repeat(64));
        emoji_terminal.ingest(excessive_keycap.as_bytes());
        let mut harness = visual_harness(emoji_terminal);

        harness.run();
        harness.run();
        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default().with_color_emoji(false));
        harness.run();
        harness
            .state_mut()
            .view
            .set_font_set(TerminalFontSet::default());
        harness.run();
        let failed = harness.state().view.diagnostics();
        assert_eq!(failed.color_emoji_paints, 0);
        assert_eq!(failed.color_emoji_rasterization_attempts, 1);
        assert_eq!(failed.color_emoji_rasterization_failures, 1);
        assert_eq!(failed.color_emoji_negative_cache_hits, 0);

        harness.run();
        let cached = harness.state().view.diagnostics();
        assert_eq!(cached.color_emoji_paints, 0);
        assert_eq!(cached.color_emoji_rasterization_attempts, 0);
        assert_eq!(cached.color_emoji_rasterization_failures, 0);
        assert_eq!(cached.color_emoji_negative_cache_hits, 1);
    }

    #[test]
    fn diagnostics_summary_reports_content_free_emoji_cache_work() {
        let mut view = TerminalView::default();
        view.diagnostics.color_emoji_paints = 7;
        view.diagnostics.color_emoji_cache_hits = 6;
        view.diagnostics.color_emoji_cache_misses = 1;
        view.diagnostics.color_emoji_rasterization_attempts = 2;
        view.diagnostics.color_emoji_rasterization_failures = 1;
        view.diagnostics.color_emoji_negative_cache_hits = 3;

        let summary = view.diagnostics_summary("session running");

        assert!(summary.contains(
            "emoji paints 7; cache hits 6; misses 1; raster attempts 2; failures 1; negative hits 3"
        ));
        assert!(!summary.contains('🤖'));
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn rendered_terminal_frames_match_reviewed_snapshots() {
        let mut snapshots = SnapshotResults::new();
        let mut empty = visual_harness(terminal(80, 24));
        snapshot_after_structural_assertions(&mut empty, "terminal-empty", &mut snapshots);

        let mut attributes_terminal = terminal(80, 24);
        attributes_terminal.ingest(
            b"\x1b[31mred \x1b[38;5;39mindexed \x1b[38;2;70;150;240mrgb \
              \x1b[7minverse \x1b[4munderline \x1b[9mstrike\x1b[0m",
        );
        let mut attributes = visual_harness(attributes_terminal);
        snapshot_after_structural_assertions(
            &mut attributes,
            "terminal-attributes",
            &mut snapshots,
        );

        for (name, style) in [
            ("terminal-cursor-block", b"\x1b[2 q".as_slice()),
            ("terminal-cursor-underline", b"\x1b[4 q".as_slice()),
            ("terminal-cursor-bar", b"\x1b[6 q".as_slice()),
        ] {
            let mut cursor_terminal = terminal(80, 24);
            cursor_terminal.ingest(style);
            cursor_terminal.ingest(b"cursor");
            let mut cursor = visual_harness(cursor_terminal);
            focus_terminal_grid(&mut cursor);
            snapshot_after_structural_assertions(&mut cursor, name, &mut snapshots);
        }

        let mut unicode_terminal = terminal(80, 24);
        unicode_terminal.ingest("wide \u{754c} combining e\u{301}".as_bytes());
        let mut unicode = visual_harness(unicode_terminal);
        unicode
            .state_mut()
            .view
            .selection
            .begin(CellPosition { column: 5, row: 0 });
        unicode
            .state_mut()
            .view
            .selection
            .extend(CellPosition { column: 10, row: 0 });
        unicode.state_mut().view.selection.finish();
        unicode.step();
        snapshot_after_structural_assertions(
            &mut unicode,
            "terminal-unicode-selection",
            &mut snapshots,
        );

        let mut emoji_terminal = terminal(80, 24);
        emoji_terminal.ingest(
            "🤖 bot  🗑️ clean  ⚠️ warn  ℹ️ info  👩‍🔬 lab  1️⃣ key  🇺🇸 flag  aligned".as_bytes(),
        );
        let mut emoji = visual_harness(emoji_terminal);
        emoji
            .state_mut()
            .view
            .selection
            .begin(CellPosition { column: 0, row: 0 });
        emoji
            .state_mut()
            .view
            .selection
            .extend(CellPosition { column: 5, row: 0 });
        emoji.state_mut().view.selection.finish();
        focus_terminal_grid(&mut emoji);
        snapshot_after_structural_assertions(&mut emoji, "terminal-agency-emoji", &mut snapshots);

        let mut shaping_terminal = terminal(80, 24);
        shaping_terminal.ingest("== != -> wide \u{754c} combining e\u{301}".as_bytes());
        let mut shaping = visual_harness(shaping_terminal);
        shaping.state_mut().view.enable_cell_run_shaping_for_test();
        focus_terminal_grid(&mut shaping);
        snapshot_after_structural_assertions(
            &mut shaping,
            "terminal-cell-run-shaping",
            &mut snapshots,
        );

        // Regression coverage for a bug where merged ASCII glyph runs
        // containing digits (dates, byte counts, hex-looking names) were
        // misrouted to the emoji font and rendered with visible spacing
        // gaps against the fixed monospace cell width, because digits carry
        // Unicode's loose `Emoji` property. This mirrors a real `dir /s`
        // directory listing, the original repro.
        let mut digits_terminal = terminal(80, 24);
        digits_terminal.ingest(
            b"08/13/2026  12:03 PM    <DIR>          fbad1\r\n\
              08/13/2026  12:03 PM       2,275,096,628 eaa2e0c142ea2bd7\r\n\
              #1 file*2.txt 0123456789",
        );
        let mut digits = visual_harness(digits_terminal);
        digits.state_mut().view.enable_cell_run_shaping_for_test();
        focus_terminal_grid(&mut digits);
        snapshot_after_structural_assertions(
            &mut digits,
            "terminal-cell-run-shaping-digits",
            &mut snapshots,
        );

        let mut alternate_terminal = terminal(80, 24);
        alternate_terminal.ingest(b"primary\x1b[?1049h\x1b[6 qalternate screen");
        let mut alternate = visual_harness(alternate_terminal);
        snapshot_after_structural_assertions(
            &mut alternate,
            "terminal-alternate-screen",
            &mut snapshots,
        );

        let mut resize_terminal = terminal(80, 24);
        resize_terminal.ingest(b"banner\r\nprompt> ");
        let mut resize = visual_harness(resize_terminal);
        for (name, size, output) in [
            (
                "terminal-resize-narrow",
                Vec2::new(370.0, 300.0),
                b"\x1b[2;1Hpartial narrow".as_slice(),
            ),
            (
                "terminal-resize-wide",
                Vec2::new(730.0, 560.0),
                b"\x1b[3;1Hpartial wide".as_slice(),
            ),
            (
                "terminal-resize-medium",
                Vec2::new(500.0, 400.0),
                b"\x1b[4;1Hpartial medium".as_slice(),
            ),
            (
                "terminal-resize-wide-repeat",
                Vec2::new(730.0, 560.0),
                b"\x1b[5;1Hpartial wide repeat".as_slice(),
            ),
        ] {
            resize.set_size(size);
            resize.state_mut().terminal.ingest(output);
            resize.step();
            snapshot_after_structural_assertions(&mut resize, name, &mut snapshots);
        }
        snapshots.unwrap();
    }

    #[cfg(any(target_os = "windows", target_os = "linux"))]
    #[test]
    fn compact_colon_background_padding_matches_reviewed_snapshot() {
        let mut terminal = terminal(80, 24);
        terminal.ingest(b"Before \x1b[38:2:110:190:255;48:2:48:48:48m TCPTracking \x1b[0m, after.");
        for column in 7..20 {
            assert_eq!(
                terminal.cell(column, 0).unwrap().background(),
                Color::Rgb {
                    red: 48,
                    green: 48,
                    blue: 48,
                }
            );
        }
        assert_eq!(terminal.cell(20, 0).unwrap().background(), Color::Default);

        let mut snapshots = SnapshotResults::new();
        let mut harness = visual_harness(terminal);
        snapshot_after_structural_assertions(
            &mut harness,
            "terminal-compact-colon-background-padding",
            &mut snapshots,
        );
        snapshots.unwrap();
    }

    /// The window the capture corpus needs: every fixture was recorded at
    /// 120x40, and replaying into anything else reflows it.
    ///
    /// The grid sits inside 16px of padding and the cell is neither a round
    /// number of pixels nor stable across font changes, so this size was
    /// measured rather than calculated: 120x40 holds for windows from roughly
    /// 1041x752 to 1049x770, and this sits in the middle of that. The test
    /// asserts the result rather than trusting the number, because
    /// `assert_snapshot_invariants` compares the view's computed size against
    /// the terminal's *own* size - if the window were too small the view would
    /// quietly resize the terminal and the invariants would still hold, while
    /// the snapshot captured reflowed rubbish.
    #[cfg(target_os = "linux")]
    const CAPTURE_WINDOW: Vec2 = Vec2::new(1045.0, 761.0);

    /// Replays a capture from the corpus and draws its final interesting frame.
    ///
    /// These are the same byte streams `festerm-core`'s `tui_capture.rs`
    /// replays, asked the one question a cell-grid assertion cannot answer: a
    /// cell can hold the right character, colour and attributes and still be
    /// drawn wrong. The compact-colon defect in #188 shipped past a green
    /// suite; background *extent*, glyph substitution and run splitting are
    /// only visible in pixels.
    ///
    /// **Linux only, deliberately.** The other snapshot scenarios are built
    /// for Windows and Linux both, but a Windows baseline has to be generated
    /// on Windows and reviewed by eye to be worth anything, and an unreviewed
    /// baseline just freezes whatever happened to be drawn. The behaviour
    /// under test is platform-independent; the existing fifteen scenarios
    /// continue to cover the Windows font stack.
    ///
    /// Regenerate with `UPDATE_SNAPSHOTS=1 cargo test -p festerm-ui-egui`, on
    /// Linux, then *look at the PNGs* before committing them.
    #[cfg(target_os = "linux")]
    #[test]
    fn captured_programs_match_reviewed_snapshots() {
        use festerm_test_support::captures;

        // One capture per rendering risk, rather than the whole corpus:
        // snapshots cost repo size and human review, and `less` and `nano`
        // exercise the same classes as `vim` and `fzf` with weaker signal.
        let scenarios: [(&str, &[u8]); 5] = [
            // Where #188 was found: inline backgrounds that must stop exactly
            // where the span does. The only capture that never takes the
            // alternate screen.
            ("copilot", captures::COPILOT),
            // Full-width reverse-video status line, and a line-number gutter
            // whose alignment a grid assertion cannot see.
            ("vim", captures::VIM),
            // DEC special graphics pane dividers - a cell can hold the right
            // code point and still render as tofu.
            ("tmux", captures::TMUX),
            // Dense indexed and truecolour palette: meters and a reverse-video
            // header painted across the full width.
            ("htop", captures::HTOP),
            // A selected row whose background runs past the end of the text,
            // from the 256-colour palette.
            ("fzf", captures::FZF),
        ];

        let mut snapshots = SnapshotResults::new();
        for (name, capture) in scenarios {
            let mut replayed = terminal(captures::COLUMNS, captures::ROWS);
            replayed.ingest(captures::frame_worth_rendering(capture));

            // Structure, not values: these recordings carry one machine's
            // clock, PIDs and percentages, so the assertions here only
            // establish that there is something to get wrong - a painted
            // background or a reverse-video run, the two things the renderer
            // has to size correctly. The reviewed baseline is what pins down
            // how it is actually drawn.
            assert!(
                (0..captures::ROWS).any(|row| (0..captures::COLUMNS).any(|column| replayed
                    .cell(column, row)
                    .is_some_and(|cell| cell.background() != Color::Default
                        || cell.attributes().contains(Attributes::INVERSE)))),
                "{name} draws no highlight at all, so it cannot regress one"
            );

            let mut harness = sized_visual_harness(replayed, CAPTURE_WINDOW);
            harness.step();
            assert_eq!(
                harness.state().terminal.dimensions(),
                Dimensions::new(captures::COLUMNS, captures::ROWS).expect("corpus geometry"),
                "the {name} window reflowed the capture instead of drawing it"
            );

            snapshot_after_structural_assertions(
                &mut harness,
                &format!("terminal-capture-{name}"),
                &mut snapshots,
            );
        }
        snapshots.unwrap();
    }

    #[test]
    fn undersized_viewport_does_not_shrink_the_terminal_or_lose_cached_content() {
        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        let mut terminal = terminal(80, 24);
        terminal.ingest(b"Windows banner\r\nC:\\Users\\fes>");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );
        let mut resize = ResizeTracker::default();

        for viewport in [
            ViewSize {
                width: 370.0,
                height: 260.0,
            },
            ViewSize {
                width: 0.0,
                height: 0.0,
            },
            ViewSize {
                width: 8.0,
                height: 19.0,
            },
            ViewSize {
                width: 730.0,
                height: 520.0,
            },
        ] {
            resize.apply_viewport_with_content_positions(&mut terminal, viewport, cell, &[]);
            let dirty_rows = terminal.take_dirty_rows();
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

            assert!(terminal
                .row_text(0)
                .is_some_and(|row| row.starts_with("Windows banner")));
            assert!(cache
                .row(0)
                .is_some_and(|row| row.first().is_some_and(|cell| cell.text() == "W")));
        }
        assert_eq!(terminal.dimensions(), Dimensions::new(73, 26).unwrap());
    }

    #[test]
    fn viewport_replay_preserves_cache_geometry_during_output_resizes() {
        enum Step {
            Output(&'static [u8]),
            Viewport(ViewSize),
        }

        let cell = CellMetrics::new(10.0, 20.0).unwrap();
        let mut terminal = terminal(80, 24);
        let mut cache = TerminalRenderCache::default();
        let mut resize = ResizeTracker::default();
        let sink = Sink::default();

        for step in [
            Step::Output(b"Windows banner\r\nC:\\Users\\fes>"),
            Step::Viewport(ViewSize {
                width: 370.0,
                height: 260.0,
            }),
            Step::Output(b"\x1b[2;1Hactive output"),
            Step::Viewport(ViewSize {
                width: 0.0,
                height: 0.0,
            }),
            Step::Output(b"\x1b[3;1Hpartial"),
            Step::Viewport(ViewSize {
                width: 500.0,
                height: 360.0,
            }),
            Step::Viewport(ViewSize {
                width: 730.0,
                height: 520.0,
            }),
        ] {
            match step {
                Step::Output(bytes) => terminal.ingest(bytes),
                Step::Viewport(viewport) => {
                    let (_outcome, positions) = resize.apply_viewport_with_content_positions(
                        &mut terminal,
                        viewport,
                        cell,
                        &[],
                    );
                    assert!(positions.is_empty());
                    let layout =
                        viewport_layout(Pos2::new(0.0, 0.0), viewport, cell, terminal.dimensions());
                    assert_eq!(layout.dimensions, terminal.dimensions());
                    assert_eq!(layout.viewport.min, layout.grid.min);
                    if dimensions_from_viewport(viewport, cell).is_some() {
                        assert!(
                            layout.viewport.contains_rect(layout.grid),
                            "accepted terminal dimensions must fit the allocated viewport"
                        );
                    }
                    let cursor = terminal.cursor();
                    assert!(cursor.column() < terminal.dimensions().columns());
                    assert!(cursor.row() < terminal.dimensions().rows());
                    let cursor_rect = grid_cell_rect(
                        GridLayout {
                            rect: layout.grid,
                            dimensions: layout.dimensions,
                            metrics: cell,
                        },
                        CellPosition {
                            column: cursor.column(),
                            row: cursor.row(),
                        },
                        1,
                    );
                    assert!(cursor_rect.is_finite());
                    if dimensions_from_viewport(viewport, cell).is_some() {
                        assert!(layout.viewport.contains_rect(cursor_rect));
                    }
                }
            }

            let dirty_rows = terminal.take_dirty_rows();
            assert!(dirty_rows
                .iter()
                .all(|row| *row < terminal.dimensions().rows()));
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
            assert_eq!(cache.dimensions(), Some(terminal.dimensions()));
            for row in 0..terminal.dimensions().rows() {
                assert_eq!(
                    cache.row(row).map(<[RenderedCell]>::len),
                    Some(terminal.dimensions().columns())
                );
            }
        }

        assert!(terminal
            .row_text(0)
            .is_some_and(|row| row.starts_with("Windows banner")));
        assert!(terminal
            .row_text(1)
            .is_some_and(|row| row.starts_with("active output")));
        assert!(terminal
            .row_text(2)
            .is_some_and(|row| row.starts_with("partial")));
        assert!(sink.0.is_empty());
    }

    #[test]
    fn wide_cells_use_one_two_column_paint_and_selection_span() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[4;38;2;1;2;3;48;5;196m\xe7\x95\x8c");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cells = cache.row(0).expect("cached row");
        let leading = &cells[0];
        let continuation = &cells[1];

        assert_eq!(leading.width(), CellWidth::Double);
        assert_eq!(continuation.width(), CellWidth::Continuation);
        assert_eq!(leading.attributes(), continuation.attributes());
        assert_eq!(leading.foreground(), continuation.foreground());
        assert_eq!(leading.background(), continuation.background());

        let layout = grid_layout(4, 1);
        let rect = grid_cell_rect(layout, CellPosition { column: 0, row: 0 }, 2);
        assert_eq!(rect.min, Pos2::new(5.0, 7.0));
        assert_eq!(rect.size(), Vec2::new(20.0, 20.0));
        assert!(rendered_cell_is_selected(
            Some(CellRange::new(
                CellPosition { column: 1, row: 0 },
                CellPosition { column: 1, row: 0 },
            )),
            CellPosition { column: 0, row: 0 },
            rendered_cell_columns(leading, layout.dimensions, 0),
        ));
    }

    #[test]
    fn input_latency_finishes_after_paint_submission_work() {
        let observed = Instant::now();
        let (_, elapsed) = measure_input_to_paint_submission(Some(observed), || {
            std::thread::sleep(Duration::from_millis(2));
        });

        assert!(
            elapsed.is_some_and(|duration| duration >= Duration::from_millis(2)),
            "the measurement must include the submitted grid paint work"
        );
        assert_eq!(measure_input_to_paint_submission::<()>(None, || ()).1, None);
    }

    #[test]
    fn cache_updates_dirty_rows_without_full_grid_copies() {
        let mut terminal = terminal(4, 2);
        terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        terminal.ingest(b"A");
        let dirty_rows = terminal.take_dirty_rows();

        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(update.updated_rows, vec![0, 1]);
        terminal.ingest(b"B");
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        assert!(!update.full_refresh);
        assert_eq!(update.updated_rows, vec![0]);
        assert_eq!(cache.row(0).unwrap()[1].text(), "B");
    }

    #[test]
    fn recorded_fixture_state_preserves_renderer_cell_structure() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/m3/unicode-cells.fixture");
        let fixture = load_fixture(&path).expect("fixture parses");
        let mut terminal = Terminal::new(fixture.dimensions).unwrap();
        terminal.ingest(&fixture.input);
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        let row = cache.row(0).unwrap();
        assert_eq!(row[1].text(), "界");
        assert_eq!(row[1].width(), CellWidth::Double);
        assert_eq!(row[2].width(), CellWidth::Continuation);
        assert_eq!(row[3].text(), "e\u{301}");
    }

    #[test]
    fn cache_preserves_passive_hyperlink_metadata() {
        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b]8;;https://example.com\x1b\\go\x1b]8;;\x1b\\");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();

        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert_eq!(
            cache.row(0).unwrap()[0].hyperlink(),
            Some("https://example.com/")
        );
        assert_eq!(cache.row(0).unwrap()[2].hyperlink(), None);
    }

    #[test]
    fn renderer_resolves_terminal_colors_and_basic_attributes() {
        assert_eq!(
            resolve_color(Color::Indexed(196), DEFAULT_BACKGROUND),
            Color32::from_rgb(255, 0, 0)
        );
        assert_eq!(
            resolve_color(
                Color::Rgb {
                    red: 1,
                    green: 2,
                    blue: 3
                },
                DEFAULT_BACKGROUND
            ),
            Color32::from_rgb(1, 2, 3)
        );

        let mut terminal = terminal(4, 1);
        terminal.ingest(b"\x1b[1;3;4;7;8;9;38;2;1;2;3;48;5;196mX");
        let dirty_rows = terminal.take_dirty_rows();
        let mut cache = TerminalRenderCache::default();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        let cell = &cache.row(0).unwrap()[0];

        assert!(cell.attributes.contains(Attributes::BOLD));
        assert!(cell.attributes.contains(Attributes::ITALIC));
        assert!(cell.attributes.contains(Attributes::UNDERLINE));
        assert!(cell.attributes.contains(Attributes::INVERSE));
        assert!(cell.attributes.contains(Attributes::CONCEALED));
        assert!(cell.attributes.contains(Attributes::STRIKETHROUGH));
        let (foreground, background) = cell_colors(cell);
        assert_eq!(
            foreground, background,
            "conceal uses the effective background"
        );
        assert_eq!(background, Color32::from_rgb(1, 2, 3));
    }

    #[test]
    fn default_cells_share_the_grid_background_without_individual_paints() {
        let default = RenderedCell {
            text: String::new(),
            width: CellWidth::Single,
            foreground: Color::Default,
            background: Color::Default,
            attributes: Attributes::NONE,
            hyperlink: None,
        };
        let colored = RenderedCell {
            background: Color::Indexed(4),
            ..default.clone()
        };
        let inverse = RenderedCell {
            attributes: Attributes::INVERSE,
            ..default.clone()
        };

        assert!(!cell_needs_background_paint(&default, false));
        assert!(cell_needs_background_paint(&default, true));
        assert!(cell_needs_background_paint(&colored, false));
        assert!(cell_needs_background_paint(&inverse, false));
    }

    #[test]
    fn output_ingested_after_resize_becomes_visible_in_the_cache() {
        // Regression test: after a live resize reflows the primary screen,
        // subsequent PTY output must still mark rows dirty normally so the
        // cache (and therefore the renderer) picks it up.
        let mut terminal = terminal(80, 24);
        terminal.ingest(b"$ ");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );

        let mut resize = ResizeTracker::default();
        assert_eq!(
            resize.apply(&mut terminal, Dimensions::new(100, 30).unwrap()),
            ResizeOutcome::Resized(Dimensions::new(100, 30).unwrap())
        );
        let dirty_rows = terminal.take_dirty_rows();
        cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        terminal.ingest(b"echo hello\r\nhello\r\n$ ");
        let dirty_rows = terminal.take_dirty_rows();
        assert!(
            !dirty_rows.is_empty(),
            "new output after resize must mark rows dirty"
        );
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        assert!(!update.updated_rows.is_empty());
        assert_eq!(cache.row(0).unwrap()[0].text(), "$");
        assert_eq!(cache.row(1).unwrap()[0].text(), "h");
    }

    #[test]
    fn sustained_output_keeps_cache_input_and_resize_paths_usable() {
        let started = Instant::now();
        let mut terminal = terminal(120, 40);
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );

        for _ in 0..1_000 {
            terminal.ingest(
                b"representative output line exercises terminal scrolling and dirty rows\r\n",
            );
            let dirty_rows = terminal.take_dirty_rows();
            cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);
        }

        let mut sink = Sink::default();
        assert_eq!(
            route_input(&mut terminal, InputEvent::Key(Key::ArrowDown), &mut sink).outcome,
            InputEventOutcome::Encoded { bytes: 3 }
        );
        let mut resize = ResizeTracker::default();
        assert_eq!(
            resize.apply(&mut terminal, Dimensions::new(100, 30).unwrap()),
            ResizeOutcome::Resized(Dimensions::new(100, 30).unwrap())
        );
        let dirty_rows = terminal.take_dirty_rows();
        let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

        assert!(update.full_refresh);
        assert_eq!(cache.dimensions(), Some(Dimensions::new(100, 30).unwrap()));
        assert_eq!(sink.0, vec![b"\x1b[B".to_vec()]);
        // This is a pure-CPU regression watchdog (no I/O or subprocess), so
        // it normally completes in well under a second; the generous
        // ceiling exists only to catch a genuine multiple-orders-of-
        // magnitude algorithmic regression, not to enforce a tight budget.
        // GitHub's hosted `windows-latest` runners are documented to run
        // noticeably slower/noisier than `ubuntu-latest`/`macos-latest`
        // (particularly for unoptimized debug builds under load), so give
        // it enough headroom to avoid CI-only false positives.
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "representative output path became unexpectedly slow"
        );
    }

    #[test]
    fn repeated_resize_refreshes_cached_banner_and_prompt_cells() {
        let mut terminal = terminal(12, 4);
        terminal.ingest(b"Windows cmd\r\nCopyright\r\nC:\\Users\\fes>");
        let mut cache = TerminalRenderCache::default();
        let initial_dirty_rows = terminal.take_dirty_rows();
        cache.update(
            TerminalSnapshot::from_terminal(&terminal),
            &initial_dirty_rows,
        );
        let mut resize = ResizeTracker::default();

        // Under reflow (ADR 0017), shrinking the row count can push older
        // hard-broken lines into retained history rather than always
        // keeping them clipped in place at the top; growing back to a
        // taller size pulls them back onto the visible screen unchanged.
        // Expected top-of-row text per step, verified against the
        // equivalent festerm-core reflow test.
        let expectations = [
            ["W", "C", "C"],
            ["C", "C", ">"],
            ["C", "C", "s"],
            ["C", "C", ">"],
            ["W", "C", "C"],
        ];

        for (dimensions, expected) in [
            Dimensions::new(11, 4).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 3).unwrap(),
            Dimensions::new(12, 3).unwrap(),
            Dimensions::new(11, 4).unwrap(),
        ]
        .into_iter()
        .zip(expectations)
        {
            assert_eq!(
                resize.apply(&mut terminal, dimensions),
                ResizeOutcome::Resized(dimensions)
            );
            let dirty_rows = terminal.take_dirty_rows();
            let update = cache.update(TerminalSnapshot::from_terminal(&terminal), &dirty_rows);

            assert!(update.full_refresh);
            assert_eq!(cache.row(0).unwrap()[0].text(), expected[0]);
            assert_eq!(cache.row(1).unwrap()[0].text(), expected[1]);
            assert_eq!(cache.row(2).unwrap()[0].text(), expected[2]);
        }
    }
}
