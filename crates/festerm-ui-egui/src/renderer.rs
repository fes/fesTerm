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
use festerm_core::{Attributes, Color, CursorStyle, Dimensions};
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
            let byte_size = image.width() * image.height() * 4;
            self.prepare_for_insert(byte_size);
            let aspect_ratio = image.width() as f32 / image.height() as f32;
            let texture_name = format!(
                "festerm-color-emoji-{}-{}",
                stable_text_hash(text),
                pixel_size
            );
            let texture = painter
                .ctx()
                .load_texture(texture_name, image, TextureOptions::LINEAR);
            self.textures.insert(
                key.clone(),
                ColorEmojiTexture {
                    texture,
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
    let Some(dimensions) = paint.cache.dimensions() else {
        return GridPaintStats::default();
    };
    let mut stats = GridPaintStats::default();
    let selection_range = paint.selection.range_in_snapshot(paint.snapshot);
    painter.rect_filled(paint.layout.rect, 0.0, DEFAULT_BACKGROUND);
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
                let cell_painter = painter.with_clip_rect(rect);
                let outcome = glyphs.paint_color_emoji(
                    &cell_painter,
                    &cell.text,
                    rect,
                    cell.attributes,
                    paint.fonts.font_set().color_emoji(),
                );
                stats.record_color_emoji(outcome);
                if !outcome.painted() {
                    let galley = glyphs.layout(
                        &cell_painter,
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
                    cell_painter.galley(text_position, galley, foreground);
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
                let run_painter = painter.with_clip_rect(rect);
                let outcome = glyphs.paint_color_emoji(
                    &run_painter,
                    &run.text,
                    rect,
                    run.attributes,
                    paint.fonts.font_set().color_emoji(),
                );
                stats.record_color_emoji(outcome);
                if !outcome.painted() {
                    let galley = glyphs.layout(
                        &run_painter,
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
                    run_painter.galley(text_position, galley, run.foreground);
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
pub fn resolve_color(color: Color, default: Color32) -> Color32 {
    match color {
        Color::Default => default,
        Color::Rgb { red, green, blue } => Color32::from_rgb(red, green, blue),
        Color::Indexed(index) if index < 16 => ansi_color(index),
        Color::Indexed(index @ 16..=231) => {
            let value = index - 16;
            let levels = [0, 95, 135, 175, 215, 255];
            Color32::from_rgb(
                levels[(value / 36) as usize],
                levels[((value / 6) % 6) as usize],
                levels[(value % 6) as usize],
            )
        }
        Color::Indexed(index) => {
            let level = 8 + (index - 232) * 10;
            Color32::from_gray(level)
        }
    }
}

fn ansi_color(index: u8) -> Color32 {
    const COLORS: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    let (red, green, blue) = COLORS[index as usize];
    Color32::from_rgb(red, green, blue)
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
    use super::*;
    use icu_properties::{
        props::{Emoji, EmojiPresentation},
        CodePointSetData,
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
}
