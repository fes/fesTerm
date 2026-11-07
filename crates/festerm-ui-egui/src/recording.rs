use std::{collections::HashMap, fmt};

use egui::{
    epaint::{ClippedPrimitive, ImageData, ImageDelta, Primitive, Vertex},
    Color32, Context, Pos2, RawInput, Rect, TextureFilter, TextureId, TextureOptions,
    TextureWrapMode,
};
use festerm_core::Terminal;

use crate::{
    geometry::CellMetrics,
    renderer::{paint_grid, FontSettings, GlyphCache, GridLayout, GridPaint},
    Selection, TerminalFontFamily, TerminalFontSet, TerminalRenderCache, TerminalSnapshot,
    DEFAULT_BACKGROUND, DEFAULT_FOREGROUND,
};

const MAX_TEXTURE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WIDTH: usize = 3840;
const MAX_HEIGHT: usize = 2160;

/// Fixed, even-aligned dimensions for an offline video frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingFrameSize {
    width: usize,
    height: usize,
}

impl RecordingFrameSize {
    pub fn new(width: usize, height: usize) -> Result<Self, RecordingRenderError> {
        if width < 2
            || height < 2
            || width > MAX_WIDTH
            || height > MAX_HEIGHT
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
        {
            return Err(RecordingRenderError::new(
                "video dimensions must be even and at most 3840 by 2160",
            ));
        }
        Ok(Self { width, height })
    }

    pub const fn width(self) -> usize {
        self.width
    }

    pub const fn height(self) -> usize {
        self.height
    }
}

/// An offline rendering failure; no live terminal is affected.
#[derive(Debug)]
pub struct RecordingRenderError(String);

impl RecordingRenderError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for RecordingRenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RecordingRenderError {}

struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
    options: TextureOptions,
}

/// A terminal-only CPU renderer using fesTerm's bundled JetBrains Mono and
/// default terminal theme. It does not create an input sink or a GPU context.
pub struct RecordingRenderer {
    context: Context,
    size: RecordingFrameSize,
    font_size: f32,
    fonts: FontSettings,
    cache: TerminalRenderCache,
    glyphs: GlyphCache,
    selection: Selection,
    textures: HashMap<TextureId, Texture>,
    texture_bytes: usize,
    pixels: Vec<u8>,
}

impl RecordingRenderer {
    pub fn new(size: RecordingFrameSize, font_size: f32) -> Result<Self, RecordingRenderError> {
        if !font_size.is_finite() || !(8.0..=64.0).contains(&font_size) {
            return Err(RecordingRenderError::new(
                "recording font size must be between 8 and 64 points",
            ));
        }
        let context = Context::default();
        let generation = crate::install_terminal_fonts(&context);
        let mut fonts = FontSettings::default();
        fonts.set_font_set(TerminalFontSet::new(
            TerminalFontFamily::JetBrainsMono,
            false,
            generation,
        ));
        let byte_count = size.width * size.height * 4;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(byte_count).map_err(|error| {
            RecordingRenderError::new(format!("frame allocation failed: {error}"))
        })?;
        pixels.resize(byte_count, 0);
        Ok(Self {
            context,
            size,
            font_size,
            fonts,
            cache: TerminalRenderCache::default(),
            glyphs: GlyphCache::default(),
            selection: Selection::default(),
            textures: HashMap::new(),
            texture_bytes: 0,
            pixels,
        })
    }

    /// Returns opaque RGBA pixels. `None` renders the neutral pre-enrollment frame.
    pub fn render(&mut self, terminal: Option<&Terminal>) -> Result<&[u8], RecordingRenderError> {
        for pixel in self.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&DEFAULT_BACKGROUND.to_array());
        }
        let Some(terminal) = terminal else {
            return Ok(&self.pixels);
        };
        let rect = Rect::from_min_size(
            Pos2::ZERO,
            egui::vec2(self.size.width as f32, self.size.height as f32),
        );
        let mut input = RawInput {
            screen_rect: Some(rect),
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .expect("raw input has a root viewport")
            .native_pixels_per_point = Some(1.0);
        let context = self.context.clone();
        let mut error = None;
        let mut output = context.run_ui(input, |ui| {
            let painter = ui.painter().with_clip_rect(rect);
            let dimensions = terminal.dimensions();
            let mut fonts = self.fonts.clone();
            fonts.size_points = self.font_size;
            let glyph =
                painter.layout_no_wrap("M".to_owned(), fonts.regular_font_id(), DEFAULT_FOREGROUND);
            let scale = (rect.width() / (glyph.size().x * dimensions.columns() as f32))
                .min(rect.height() / (glyph.size().y * dimensions.rows() as f32))
                .min(1.0);
            if !scale.is_finite() || self.font_size * scale < 4.0 {
                error = Some(RecordingRenderError::new(
                    "the terminal grid is too large to render legibly on this canvas",
                ));
                return;
            }
            fonts.size_points *= scale;
            let glyph =
                painter.layout_no_wrap("M".to_owned(), fonts.regular_font_id(), DEFAULT_FOREGROUND);
            let Some(metrics) = CellMetrics::new(glyph.size().x, glyph.size().y) else {
                error = Some(RecordingRenderError::new(
                    "recording font has invalid metrics",
                ));
                return;
            };
            let grid_size = egui::vec2(
                metrics.width * dimensions.columns() as f32,
                metrics.height * dimensions.rows() as f32,
            );
            if grid_size.x > rect.width() + 1.0 || grid_size.y > rect.height() + 1.0 {
                error = Some(RecordingRenderError::new(
                    "the terminal grid does not fit the recording canvas",
                ));
                return;
            }
            let layout = GridLayout {
                rect: Rect::from_center_size(rect.center(), grid_size),
                dimensions,
                metrics,
            };
            let snapshot = TerminalSnapshot::from_terminal(terminal);
            self.cache
                .update(snapshot, &(0..dimensions.rows()).collect::<Vec<_>>());
            paint_grid(
                painter,
                GridPaint {
                    layout,
                    snapshot,
                    cache: &self.cache,
                    selection: &self.selection,
                    fonts: &fonts,
                    shape_cell_runs: false,
                    focused: true,
                    suppress_cursor: false,
                    full_redraw: true,
                },
                &mut self.glyphs,
            );
        });
        if let Some(error) = error {
            output.drop_without_applying_deltas();
            return Err(error);
        }
        let updates = std::mem::take(&mut output.textures_delta.set);
        let released = std::mem::take(&mut output.textures_delta.free);
        for (id, updates) in updates {
            for update in updates {
                self.update_texture(id, update)?;
            }
        }
        let primitives = context.tessellate(output.shapes, output.pixels_per_point);
        rasterize(&mut self.pixels, self.size, &self.textures, primitives)?;
        for id in released {
            if let Some(texture) = self.textures.remove(&id) {
                self.texture_bytes -= texture.pixels.len() * 4;
            }
        }
        Ok(&self.pixels)
    }

    /// Returns the pixels from the last successful render.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn update_texture(
        &mut self,
        id: TextureId,
        delta: ImageDelta,
    ) -> Result<(), RecordingRenderError> {
        if delta.options.wrap_mode != TextureWrapMode::ClampToEdge
            || delta.options.mipmap_mode.is_some()
            || delta.options.magnification != delta.options.minification
        {
            return Err(RecordingRenderError::new(
                "recording renderer cannot use these texture options",
            ));
        }
        let ImageData::Color(image) = delta.image;
        let count = image.size[0]
            .checked_mul(image.size[1])
            .ok_or_else(|| RecordingRenderError::new("recording texture size overflows"))?;
        if count != image.pixels.len() || image.size.contains(&0) {
            return Err(RecordingRenderError::new("invalid recording texture"));
        }
        if let Some([x, y]) = delta.pos {
            let texture = self
                .textures
                .get_mut(&id)
                .ok_or_else(|| RecordingRenderError::new("recording texture patch has no base"))?;
            if x.checked_add(image.size[0])
                .is_none_or(|right| right > texture.size[0])
                || y.checked_add(image.size[1])
                    .is_none_or(|bottom| bottom > texture.size[1])
            {
                return Err(RecordingRenderError::new(
                    "recording texture patch is out of bounds",
                ));
            }
            for row in 0..image.size[1] {
                let source = row * image.size[0];
                let destination = (y + row) * texture.size[0] + x;
                texture.pixels[destination..destination + image.size[0]]
                    .copy_from_slice(&image.pixels[source..source + image.size[0]]);
            }
            texture.options = delta.options;
        } else {
            let old = self
                .textures
                .get(&id)
                .map_or(0, |texture| texture.pixels.len() * 4);
            let next = self
                .texture_bytes
                .checked_sub(old)
                .and_then(|bytes| {
                    count
                        .checked_mul(4)
                        .and_then(|added| bytes.checked_add(added))
                })
                .ok_or_else(|| RecordingRenderError::new("recording texture size overflows"))?;
            if next > MAX_TEXTURE_BYTES {
                return Err(RecordingRenderError::new(
                    "recording texture budget is exhausted",
                ));
            }
            self.textures.insert(
                id,
                Texture {
                    size: image.size,
                    pixels: image.pixels.clone(),
                    options: delta.options,
                },
            );
            self.texture_bytes = next;
        }
        Ok(())
    }
}

fn rasterize(
    pixels: &mut [u8],
    size: RecordingFrameSize,
    textures: &HashMap<TextureId, Texture>,
    primitives: Vec<ClippedPrimitive>,
) -> Result<(), RecordingRenderError> {
    for clipped in primitives {
        let Primitive::Mesh(mesh) = clipped.primitive else {
            return Err(RecordingRenderError::new(
                "recording renderer encountered a native paint callback",
            ));
        };
        if !mesh.is_valid() || !mesh.indices.len().is_multiple_of(3) {
            return Err(RecordingRenderError::new("invalid recording paint mesh"));
        }
        let texture = textures
            .get(&mesh.texture_id)
            .ok_or_else(|| RecordingRenderError::new("recording paint texture is unavailable"))?;
        for indices in mesh.indices.chunks_exact(3) {
            let vertices = [
                mesh.vertices[indices[0] as usize],
                mesh.vertices[indices[1] as usize],
                mesh.vertices[indices[2] as usize],
            ];
            rasterize_triangle(pixels, size, clipped.clip_rect, texture, vertices);
        }
    }
    Ok(())
}

fn edge(a: Pos2, b: Pos2, point: Pos2) -> f32 {
    (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x)
}

fn top_left(a: Pos2, b: Pos2) -> bool {
    let delta = b - a;
    delta.y < 0.0 || (delta.y == 0.0 && delta.x > 0.0)
}

fn rasterize_triangle(
    pixels: &mut [u8],
    size: RecordingFrameSize,
    clip: Rect,
    texture: &Texture,
    mut vertices: [Vertex; 3],
) {
    let mut area = edge(vertices[0].pos, vertices[1].pos, vertices[2].pos);
    if !area.is_finite() || area == 0.0 {
        return;
    }
    if area < 0.0 {
        vertices.swap(1, 2);
        area = -area;
    }
    let min_x = vertices
        .iter()
        .map(|vertex| vertex.pos.x)
        .fold(f32::INFINITY, f32::min);
    let max_x = vertices
        .iter()
        .map(|vertex| vertex.pos.x)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_y = vertices
        .iter()
        .map(|vertex| vertex.pos.y)
        .fold(f32::INFINITY, f32::min);
    let max_y = vertices
        .iter()
        .map(|vertex| vertex.pos.y)
        .fold(f32::NEG_INFINITY, f32::max);
    let left = min_x.max(clip.min.x).floor().max(0.0) as usize;
    let right = max_x.min(clip.max.x).ceil().max(0.0).min(size.width as f32) as usize;
    let top = min_y.max(clip.min.y).floor().max(0.0) as usize;
    let bottom = max_y
        .min(clip.max.y)
        .ceil()
        .max(0.0)
        .min(size.height as f32) as usize;
    let colors = vertices.map(|vertex| vertex.color.to_array());
    for y in top..bottom {
        for x in left..right {
            let point = Pos2::new(x as f32 + 0.5, y as f32 + 0.5);
            if !clip.contains(point) {
                continue;
            }
            let weights = [
                edge(vertices[1].pos, vertices[2].pos, point),
                edge(vertices[2].pos, vertices[0].pos, point),
                edge(vertices[0].pos, vertices[1].pos, point),
            ];
            let included = [
                top_left(vertices[1].pos, vertices[2].pos),
                top_left(vertices[2].pos, vertices[0].pos),
                top_left(vertices[0].pos, vertices[1].pos),
            ];
            if weights
                .iter()
                .zip(included)
                .any(|(weight, included)| *weight < 0.0 || (*weight == 0.0 && !included))
            {
                continue;
            }
            let weights = weights.map(|weight| weight / area);
            let uv = Pos2::new(
                (0..3).map(|i| weights[i] * vertices[i].uv.x).sum(),
                (0..3).map(|i| weights[i] * vertices[i].uv.y).sum(),
            );
            let texel = sample_texture(texture, uv);
            let color: [f32; 4] = std::array::from_fn(|channel| {
                (0..3)
                    .map(|i| weights[i] * f32::from(colors[i][channel]))
                    .sum::<f32>()
                    * texel[channel]
                    / 255.0
            });
            let offset = (y * size.width + x) * 4;
            let remaining = 1.0 - color[3] / 255.0;
            for channel in 0..4 {
                pixels[offset + channel] = (color[channel]
                    + f32::from(pixels[offset + channel]) * remaining)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
}

fn sample_texture(texture: &Texture, uv: Pos2) -> [f32; 4] {
    let x = uv.x * texture.size[0] as f32 - 0.5;
    let y = uv.y * texture.size[1] as f32 - 0.5;
    let pixel = |x: f32, y: f32| {
        let x = x.clamp(0.0, (texture.size[0] - 1) as f32) as usize;
        let y = y.clamp(0.0, (texture.size[1] - 1) as f32) as usize;
        texture.pixels[y * texture.size[0] + x]
            .to_array()
            .map(f32::from)
    };
    if texture.options.magnification == TextureFilter::Nearest {
        return pixel(x.round(), y.round());
    }
    let left = x.floor();
    let top = y.floor();
    let dx = x - left;
    let dy = y - top;
    let corners = [
        pixel(left, top),
        pixel(left + 1.0, top),
        pixel(left, top + 1.0),
        pixel(left + 1.0, top + 1.0),
    ];
    std::array::from_fn(|channel| {
        let upper = corners[0][channel] * (1.0 - dx) + corners[1][channel] * dx;
        let lower = corners[2][channel] * (1.0 - dx) + corners[3][channel] * dx;
        upper * (1.0 - dy) + lower * dy
    })
}

#[cfg(test)]
mod tests {
    use festerm_core::{Dimensions, Terminal};

    use super::*;

    #[test]
    fn recording_renderer_draws_terminal_text_without_mutating_the_source() {
        let size = RecordingFrameSize::new(640, 360).unwrap();
        let mut renderer = RecordingRenderer::new(size, 18.0).unwrap();
        let mut terminal = Terminal::new(Dimensions::new(30, 4).unwrap()).unwrap();
        terminal.ingest("\x1b[?25l\x1b[31mHello 界\x1b[0m".as_bytes());
        let cursor = terminal.cursor();
        let first = renderer.render(Some(&terminal)).unwrap().to_vec();
        assert_eq!(first.len(), 640 * 360 * 4);
        assert!(first.chunks_exact(4).all(|pixel| pixel[3] == 255));
        assert!(first
            .chunks_exact(4)
            .any(|pixel| pixel[..3] != DEFAULT_BACKGROUND.to_array()[..3]));
        assert_eq!(renderer.render(Some(&terminal)).unwrap(), first);
        assert_eq!(terminal.cursor(), cursor);
        assert!(terminal.queued_input().is_empty());
        assert!(terminal.queued_replies().is_empty());

        terminal.resize(Dimensions::new(20, 6).unwrap()).unwrap();
        terminal.ingest(b"\r\nupdated");
        let resized = renderer.render(Some(&terminal)).unwrap();
        assert_eq!(resized.len(), first.len());
        assert_ne!(resized, first);
        let neutral = renderer.render(None).unwrap();
        assert!(neutral
            .chunks_exact(4)
            .all(|pixel| pixel == DEFAULT_BACKGROUND.to_array()));
    }

    #[test]
    fn adjacent_translucent_triangles_do_not_double_blend_their_shared_edge() {
        let size = RecordingFrameSize::new(4, 4).unwrap();
        let texture = Texture {
            size: [1, 1],
            pixels: vec![Color32::WHITE],
            options: TextureOptions::NEAREST,
        };
        let mut pixels = vec![0; 4 * 4 * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let vertex = |x, y| Vertex {
            pos: Pos2::new(x, y),
            uv: Pos2::ZERO,
            color: Color32::from_rgba_premultiplied(128, 0, 0, 128),
        };
        let clip = Rect::from_min_max(Pos2::ZERO, Pos2::new(4.0, 4.0));
        rasterize_triangle(
            &mut pixels,
            size,
            clip,
            &texture,
            [vertex(0.0, 0.0), vertex(4.0, 0.0), vertex(4.0, 4.0)],
        );
        rasterize_triangle(
            &mut pixels,
            size,
            clip,
            &texture,
            [vertex(0.0, 0.0), vertex(4.0, 4.0), vertex(0.0, 4.0)],
        );
        assert!(pixels
            .chunks_exact(4)
            .all(|pixel| pixel == [128, 0, 0, 255]));
    }

    #[test]
    fn recording_renderer_retains_color_emoji_and_texture_updates() {
        let size = RecordingFrameSize::new(320, 180).unwrap();
        let mut renderer = RecordingRenderer::new(size, 24.0).unwrap();
        let mut terminal = Terminal::new(Dimensions::new(12, 3).unwrap()).unwrap();
        terminal.ingest("\x1b[?25l🤖".as_bytes());
        let first = renderer.render(Some(&terminal)).unwrap().to_vec();
        assert!(first.chunks_exact(4).any(|pixel| {
            pixel[0] != pixel[1]
                && pixel[1] != pixel[2]
                && pixel[..3] != DEFAULT_BACKGROUND.to_array()[..3]
        }));
        terminal.ingest(" 🦊".as_bytes());
        let updated = renderer.render(Some(&terminal)).unwrap().to_vec();
        assert_ne!(updated, first);
        assert_eq!(renderer.render(Some(&terminal)).unwrap(), updated);
    }

    #[test]
    fn recording_renderer_rejects_invalid_canvas_sizes() {
        assert!(RecordingFrameSize::new(641, 360).is_err());
        assert!(RecordingFrameSize::new(640, 2162).is_err());
        assert!(RecordingFrameSize::new(0, 360).is_err());
    }
}
