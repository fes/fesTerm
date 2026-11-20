use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Cursor, Write},
    sync::{Arc, Mutex},
};

#[cfg(test)]
use egui::Vec2;
use egui::{Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};
use festerm_core::kitty::{KittyFormat, KittyImage, KittyPlacement, MAX_RGBA_BYTES};

use crate::{geometry::CellPosition, renderer::GridLayout, TerminalSnapshot};

const MAX_AXIS: u32 = 8192;
const MAX_PIXELS: u32 = 16_777_216;
pub(crate) const UNDER_BACKGROUND_Z_LIMIT: i32 = -1_073_741_824;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KittyLayer {
    UnderBackground,
    BetweenBackgroundAndText,
    AboveText,
}

pub(crate) const fn classify_layer(z: i32) -> KittyLayer {
    if z < UNDER_BACKGROUND_Z_LIMIT {
        KittyLayer::UnderBackground
    } else if z < 0 {
        KittyLayer::BetweenBackgroundAndText
    } else {
        KittyLayer::AboveText
    }
}

/// Fixed render-side graphics reservation shared across terminal views.
/// UI texture pressure here only affects texture materialization, never
/// `festerm-core` protocol admission, IDs, or replies.
#[derive(Clone, Debug)]
pub struct KittyGraphicsReservation(Arc<Mutex<ReservationState>>);

#[derive(Debug)]
struct ReservationState {
    limit_bytes: usize,
    used_bytes: usize,
}

impl KittyGraphicsReservation {
    pub fn new(limit_bytes: usize) -> Self {
        Self(Arc::new(Mutex::new(ReservationState {
            limit_bytes,
            used_bytes: 0,
        })))
    }

    pub fn limit_bytes(&self) -> usize {
        self.0
            .lock()
            .expect("kitty graphics reservation lock poisoned")
            .limit_bytes
    }

    pub fn used_bytes(&self) -> usize {
        self.0
            .lock()
            .expect("kitty graphics reservation lock poisoned")
            .used_bytes
    }

    pub(crate) fn set_limit_bytes(&self, limit_bytes: usize) {
        self.0
            .lock()
            .expect("kitty graphics reservation lock poisoned")
            .limit_bytes = limit_bytes;
    }

    fn try_reserve(&self, bytes: usize) -> Option<KittyTextureCharge> {
        let mut state = self
            .0
            .lock()
            .expect("kitty graphics reservation lock poisoned");
        let next = state.used_bytes.checked_add(bytes)?;
        if next > state.limit_bytes {
            return None;
        }
        state.used_bytes = next;
        Some(KittyTextureCharge {
            reservation: Self(Arc::clone(&self.0)),
            bytes,
        })
    }
}

impl Default for KittyGraphicsReservation {
    fn default() -> Self {
        Self::new(MAX_RGBA_BYTES)
    }
}

struct KittyTextureCharge {
    reservation: KittyGraphicsReservation,
    bytes: usize,
}

impl Drop for KittyTextureCharge {
    fn drop(&mut self) {
        let mut state = self
            .reservation
            .0
            .lock()
            .expect("kitty graphics reservation lock poisoned");
        state.used_bytes = state.used_bytes.saturating_sub(self.bytes);
    }
}

/// Deterministic, bounded PNG validation callback for `festerm_core::kitty::KittyPngValidator`.
pub fn validate_kitty_png(data: &[u8], expected_width: u32, expected_height: u32) -> bool {
    decode_png_rgba(
        data,
        Some((expected_width, expected_height)),
        MAX_RGBA_BYTES,
    )
    .is_some()
}

fn checked_rgba_bytes(width: u32, height: u32, budget_bytes: usize) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_AXIS || height > MAX_AXIS {
        return None;
    }
    let pixels = width.checked_mul(height)?;
    if pixels > MAX_PIXELS {
        return None;
    }
    let rgba_bytes = (pixels as usize).checked_mul(4)?;
    (rgba_bytes <= budget_bytes).then_some(rgba_bytes)
}

fn expected_png_idat_len(info: &png::Info<'_>) -> Option<usize> {
    let bits_per_pixel = info
        .color_type
        .samples()
        .checked_mul(info.bit_depth as usize)?;
    let row_bytes = |w: u32| -> Option<usize> {
        usize::try_from(w)
            .ok()?
            .checked_mul(bits_per_pixel)?
            .checked_add(7)
            .map(|bits| bits / 8)
    };
    if !info.interlaced {
        let stride = row_bytes(info.width)?;
        let line = stride.checked_add(1)?;
        return usize::try_from(info.height).ok()?.checked_mul(line);
    }
    const ADAM7_X_ORIGIN: [u32; 7] = [0, 4, 0, 2, 0, 1, 0];
    const ADAM7_Y_ORIGIN: [u32; 7] = [0, 0, 4, 0, 2, 0, 1];
    const ADAM7_X_STEP: [u32; 7] = [8, 8, 4, 4, 2, 2, 1];
    const ADAM7_Y_STEP: [u32; 7] = [8, 8, 8, 4, 4, 2, 2];
    let mut total = 0usize;
    for pass in 0..7 {
        let pw = info
            .width
            .saturating_sub(ADAM7_X_ORIGIN[pass])
            .div_ceil(ADAM7_X_STEP[pass]);
        let ph = info
            .height
            .saturating_sub(ADAM7_Y_ORIGIN[pass])
            .div_ceil(ADAM7_Y_STEP[pass]);
        if pw > 0 && ph > 0 {
            let line = row_bytes(pw)?.checked_add(1)?;
            total = total.checked_add(usize::try_from(ph).ok()?.checked_mul(line)?)?;
        }
    }
    Some(total)
}

fn validate_png_idat_zlib_stream(data: &[u8], expected_raw_len: usize) -> bool {
    let Some(mut remaining) = data.strip_prefix(b"\x89PNG\r\n\x1a\n") else {
        return false;
    };
    let mut decompress = flate2::Decompress::new(true);
    let mut scratch = [0u8; 8192];
    let mut stream_ended = false;
    let mut saw_idat = false;

    while let Some(header) = remaining.get(..8) {
        let length = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let kind = &header[4..8];
        let Some(end) = length.checked_add(8) else {
            return false;
        };
        let Some(crc_end) = end.checked_add(4) else {
            return false;
        };
        let Some(chunk) = remaining.get(..crc_end) else {
            return false;
        };
        let payload = &chunk[8..end];
        remaining = &remaining[crc_end..];

        if kind == b"IDAT" {
            saw_idat = true;
            if stream_ended && !payload.is_empty() {
                return false;
            }
            let mut offset = 0usize;
            while offset < payload.len() {
                if stream_ended {
                    return false;
                }
                let before_in = decompress.total_in();
                let before_out = decompress.total_out();
                let out_room = expected_raw_len
                    .saturating_sub(before_out as usize)
                    .min(scratch.len());
                // Provide 1 extra byte if out_room == 0 so excess decompressed output
                // is detected rather than stalling with 0-byte output buffer.
                let out_slice = if out_room > 0 {
                    &mut scratch[..out_room]
                } else {
                    &mut scratch[..1]
                };
                let Ok(status) = decompress.decompress(
                    &payload[offset..],
                    out_slice,
                    flate2::FlushDecompress::None,
                ) else {
                    return false;
                };
                let consumed = (decompress.total_in() - before_in) as usize;
                let produced = (decompress.total_out() - before_out) as usize;
                if decompress.total_out() as usize > expected_raw_len {
                    return false;
                }
                offset += consumed;
                match status {
                    flate2::Status::StreamEnd => {
                        stream_ended = true;
                        if offset != payload.len() {
                            return false;
                        }
                    }
                    flate2::Status::Ok => {
                        if consumed == 0 && produced == 0 {
                            return false;
                        }
                    }
                    flate2::Status::BufError => return false,
                }
            }
        } else if kind == b"IEND" {
            return saw_idat
                && stream_ended
                && remaining.is_empty()
                && decompress.total_out() as usize == expected_raw_len;
        }
    }
    false
}

fn decode_png_rgba(
    data: &[u8],
    expected_dimensions: Option<(u32, u32)>,
    budget_bytes: usize,
) -> Option<ColorImage> {
    let limits = png::Limits {
        bytes: budget_bytes.min(MAX_RGBA_BYTES),
    };
    let mut cursor = Cursor::new(data);
    let mut decoder = png::Decoder::new_with_limits(&mut cursor, limits);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info().ok()?;
    let info = reader.info();
    if info.frame_control().is_some() || info.animation_control().is_some() {
        return None;
    }
    let expected_idat_len = expected_png_idat_len(info)?;
    let (width, height) = (info.width, info.height);
    if let Some((expected_w, expected_h)) = expected_dimensions {
        if width != expected_w || height != expected_h {
            return None;
        }
    }
    let rgba_bytes = checked_rgba_bytes(width, height, budget_bytes)?;
    if !validate_png_idat_zlib_stream(data, expected_idat_len) {
        return None;
    }
    let (color_type, bit_depth) = reader.output_color_type();
    if bit_depth != png::BitDepth::Eight {
        return None;
    }
    let channels = match color_type {
        png::ColorType::Rgba => 4usize,
        png::ColorType::Rgb => 3usize,
        png::ColorType::GrayscaleAlpha => 2usize,
        png::ColorType::Grayscale => 1usize,
        png::ColorType::Indexed => return None,
    };
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    let expected_bytes = pixel_count.checked_mul(channels)?;
    let output_len = reader.output_buffer_size()?;
    if output_len != expected_bytes || output_len > budget_bytes.max(rgba_bytes) {
        return None;
    }
    let mut buffer = vec![0u8; output_len];
    let frame_info = reader.next_frame(&mut buffer).ok()?;
    if frame_info.width != width
        || frame_info.height != height
        || frame_info.buffer_size() != expected_bytes
    {
        return None;
    }
    // Finish reading to IEND so trailing IDAT/chunk CRCs are validated and
    // truncated or trailing data is rejected.
    reader.finish().ok()?;
    if cursor.position() as usize != data.len() {
        return None;
    }
    let size = [width as usize, height as usize];
    match color_type {
        png::ColorType::Rgba => Some(ColorImage::from_rgba_unmultiplied(size, &buffer)),
        png::ColorType::Rgb => {
            let pixels = buffer
                .as_chunks::<3>()
                .0
                .iter()
                .map(|rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                .collect();
            Some(ColorImage::new(size, pixels))
        }
        png::ColorType::GrayscaleAlpha => {
            let pixels = buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|ga| Color32::from_rgba_unmultiplied(ga[0], ga[0], ga[0], ga[1]))
                .collect();
            Some(ColorImage::new(size, pixels))
        }
        png::ColorType::Grayscale => {
            let pixels = buffer.iter().map(|&g| Color32::from_rgb(g, g, g)).collect();
            Some(ColorImage::new(size, pixels))
        }
        png::ColorType::Indexed => None,
    }
}

pub(crate) fn decode_kitty_image(image: &KittyImage, budget_bytes: usize) -> Option<ColorImage> {
    let rgba_bytes = checked_rgba_bytes(image.width, image.height, budget_bytes)?;
    let size = [image.width as usize, image.height as usize];
    let pixel_count = (image.width as usize).checked_mul(image.height as usize)?;
    match image.format {
        KittyFormat::Rgba => {
            if image.data().len() != rgba_bytes {
                return None;
            }
            Some(ColorImage::from_rgba_unmultiplied(size, image.data()))
        }
        KittyFormat::Rgb => {
            if image.data().len() != pixel_count.checked_mul(3)? {
                return None;
            }
            let pixels = image
                .data()
                .as_chunks::<3>()
                .0
                .iter()
                .map(|rgb| Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                .collect();
            Some(ColorImage::new(size, pixels))
        }
        KittyFormat::Png => decode_png_rgba(
            image.data(),
            Some((image.width, image.height)),
            budget_bytes,
        ),
    }
}

struct AlphaPlane {
    width: usize,
    height: usize,
    alpha: Arc<[u8]>,
}

impl AlphaPlane {
    fn from_color_image(image: &ColorImage) -> Self {
        let alpha: Vec<u8> = image.pixels.iter().map(|pixel| pixel.a()).collect();
        Self {
            width: image.width(),
            height: image.height(),
            alpha: Arc::from(alpha),
        }
    }

    fn sample_linear_alpha(&self, tex_u: f32, tex_v: f32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let fx =
            (tex_u.clamp(0.0, 1.0) * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let fy =
            (tex_v.clamp(0.0, 1.0) * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let x0 = fx.floor() as usize;
        let y0 = fy.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let wx = fx - x0 as f32;
        let wy = fy - y0 as f32;
        let a = |x: usize, y: usize| f32::from(self.alpha[y * self.width + x]);
        let top = a(x0, y0) * (1.0 - wx) + a(x1, y0) * wx;
        let bottom = a(x0, y1) * (1.0 - wx) + a(x1, y1) * wx;
        top * (1.0 - wy) + bottom * wy
    }
}

struct CachedKittyImage {
    serial: u64,
    alpha: Option<AlphaPlane>,
    pending_image: Option<Arc<ColorImage>>,
    texture: Option<TextureHandle>,
    _charge: Option<KittyTextureCharge>,
    failed: bool,
}

/// Render-side decoded image and GPU texture cache, bounded by `reservation`.
#[derive(Default)]
pub(crate) struct KittyTextureCache {
    entries: HashMap<u32, CachedKittyImage>,
    recency: VecDeque<u32>,
    reservation: KittyGraphicsReservation,
    drawable_ids: HashSet<u32>,
    decode_count: usize,
}

impl KittyTextureCache {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.drawable_ids.clear();
    }

    pub(crate) fn set_budget_bytes(&mut self, budget_bytes: usize) {
        self.reservation.set_limit_bytes(budget_bytes);
        while self.reservation.used_bytes() > budget_bytes {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    pub(crate) fn set_reservation(&mut self, reservation: KittyGraphicsReservation) {
        self.clear();
        self.reservation = reservation;
    }

    pub(crate) fn decode_count(&self) -> usize {
        self.decode_count
    }

    #[cfg(test)]
    pub(crate) fn used_bytes(&self) -> usize {
        self.reservation.used_bytes()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Drops entries whose core image was evicted or replaced with a new serial.
    pub(crate) fn sync(&mut self, snapshot: TerminalSnapshot<'_>) {
        let images = snapshot.kitty_images();
        let mut stale = Vec::new();
        for (&id, entry) in &self.entries {
            let still_current = images
                .iter()
                .find(|image| image.id == id)
                .is_some_and(|image| image.serial == entry.serial);
            if !still_current {
                stale.push(id);
            }
        }
        for id in stale {
            self.remove_entry(id);
        }
    }

    fn remove_entry(&mut self, id: u32) {
        self.entries.remove(&id);
        if let Some(pos) = self.recency.iter().position(|&item| item == id) {
            self.recency.remove(pos);
        }
    }

    fn touch(&mut self, id: u32) {
        if let Some(pos) = self.recency.iter().position(|&item| item == id) {
            self.recency.remove(pos);
        }
        self.recency.push_back(id);
    }

    fn reserve_without_evicting_protected(
        &mut self,
        bytes: usize,
        protected: &HashSet<u32>,
    ) -> Option<KittyTextureCharge> {
        if bytes > self.reservation.limit_bytes() {
            return None;
        }
        loop {
            if let Some(charge) = self.reservation.try_reserve(bytes) {
                return Some(charge);
            }
            let evict_pos = self.recency.iter().position(|id| !protected.contains(id))?;
            let id = self.recency.remove(evict_pos)?;
            self.entries.remove(&id);
        }
    }

    /// Selects and caches the bounded set of drawable images for this frame once,
    /// without evicting earlier visible images of the same frame.
    pub(crate) fn prepare_frame(
        &mut self,
        snapshot: TerminalSnapshot<'_>,
        layout: GridLayout,
        pixels_per_point: f32,
    ) {
        self.sync(snapshot);
        self.drawable_ids.clear();
        let placements = snapshot.kitty_placements();
        if placements.is_empty() {
            return;
        }
        let images = snapshot.kitty_images();
        let viewport_offset_rows = snapshot.viewport_offset_rows();
        let alternate_screen = snapshot.modes().alternate_screen();

        let mut visible_ids = Vec::new();
        let mut visible_set = HashSet::new();
        for placement in placements {
            if !visible_set.insert(placement.image_id) {
                continue;
            }
            let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
                continue;
            };
            if placement_geometry(
                layout,
                viewport_offset_rows,
                alternate_screen,
                pixels_per_point,
                placement,
                image,
            )
            .is_some()
            {
                visible_ids.push(image.id);
            } else {
                visible_set.remove(&image.id);
            }
        }

        // First mark already-cached visible images as drawable so we never evict them.
        let mut protected = HashSet::new();
        for &id in &visible_ids {
            if let Some(existing) = self.entries.get(&id) {
                if !existing.failed {
                    protected.insert(id);
                    self.drawable_ids.insert(id);
                }
            }
        }

        for id in visible_ids {
            if protected.contains(&id) {
                self.touch(id);
                continue;
            }
            let Some(image) = images.iter().find(|img| img.id == id) else {
                continue;
            };
            if self.ensure_decoded_protected(image, &protected).is_some() {
                protected.insert(id);
                self.drawable_ids.insert(id);
            }
        }
    }

    pub(crate) fn is_drawable(&self, image_id: u32) -> bool {
        self.drawable_ids.contains(&image_id)
    }

    fn ensure_decoded_protected(
        &mut self,
        image: &KittyImage,
        protected: &HashSet<u32>,
    ) -> Option<()> {
        if let Some(existing) = self.entries.get(&image.id) {
            if existing.serial == image.serial {
                let failed = existing.failed;
                self.touch(image.id);
                return (!failed).then_some(());
            }
            self.remove_entry(image.id);
        }

        let limit = self.reservation.limit_bytes();
        let Some(rgba_bytes) = checked_rgba_bytes(image.width, image.height, limit) else {
            self.entries.insert(
                image.id,
                CachedKittyImage {
                    serial: image.serial,
                    alpha: None,
                    pending_image: None,
                    texture: None,
                    _charge: None,
                    failed: true,
                },
            );
            self.touch(image.id);
            return None;
        };
        let alpha_bytes = (image.width as usize).saturating_mul(image.height as usize);
        // Charge both CPU (alpha/decode) and GPU (RGBA texture) bytes.
        let total_charge = rgba_bytes.checked_add(alpha_bytes)?;
        let charge = self.reserve_without_evicting_protected(total_charge, protected)?;

        self.decode_count += 1;
        let Some(decoded) = decode_kitty_image(image, limit) else {
            self.entries.insert(
                image.id,
                CachedKittyImage {
                    serial: image.serial,
                    alpha: None,
                    pending_image: None,
                    texture: None,
                    _charge: None,
                    failed: true,
                },
            );
            self.touch(image.id);
            return None;
        };

        let alpha = AlphaPlane::from_color_image(&decoded);
        self.entries.insert(
            image.id,
            CachedKittyImage {
                serial: image.serial,
                alpha: Some(alpha),
                pending_image: Some(Arc::new(decoded)),
                texture: None,
                _charge: Some(charge),
                failed: false,
            },
        );
        self.touch(image.id);
        Some(())
    }

    #[cfg(test)]
    fn ensure_decoded(&mut self, image: &KittyImage) -> Option<()> {
        let empty = HashSet::new();
        self.ensure_decoded_protected(image, &empty)
    }

    fn alpha_plane(&self, image: &KittyImage) -> Option<&AlphaPlane> {
        self.entries
            .get(&image.id)
            .filter(|entry| entry.serial == image.serial && !entry.failed)
            .and_then(|entry| entry.alpha.as_ref())
    }

    pub(crate) fn ensure_texture(
        &mut self,
        ctx: &egui::Context,
        image: &KittyImage,
    ) -> Option<&TextureHandle> {
        if !self.is_drawable(image.id) {
            return None;
        }
        let entry = self.entries.get_mut(&image.id)?;
        if entry.serial != image.serial || entry.failed {
            return None;
        }
        if entry.texture.is_none() {
            let color_image = entry.pending_image.take()?;
            let name = format!("kitty-image-{}-{}", image.id, image.serial);
            entry.texture = Some(ctx.load_texture(name, color_image, TextureOptions::LINEAR));
        }
        entry.texture.as_ref()
    }

    pub(crate) fn image_at_point(
        &self,
        snapshot: TerminalSnapshot<'_>,
        layout: GridLayout,
        pixels_per_point: f32,
        pos: Pos2,
    ) -> Option<KittyImage> {
        if !layout.rect.contains(pos) {
            return None;
        }
        let placements = snapshot.kitty_placements();
        let images = snapshot.kitty_images();
        let viewport_offset_rows = snapshot.viewport_offset_rows();
        let alternate_screen = snapshot.modes().alternate_screen();
        for placement in placements.iter().rev() {
            if !self.is_drawable(placement.image_id) {
                continue;
            }
            let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
                continue;
            };
            let Some(entry) = self
                .entries
                .get(&placement.image_id)
                .filter(|entry| entry.serial == image.serial && !entry.failed)
            else {
                continue;
            };
            let Some(alpha) = entry.alpha.as_ref() else {
                continue;
            };
            let Some((dest_rect, uv)) = placement_geometry(
                layout,
                viewport_offset_rows,
                alternate_screen,
                pixels_per_point,
                placement,
                image,
            ) else {
                continue;
            };
            if !dest_rect.contains(pos) {
                continue;
            }
            let u = ((pos.x - dest_rect.left()) / dest_rect.width()).clamp(0.0, 1.0);
            let v = ((pos.y - dest_rect.top()) / dest_rect.height()).clamp(0.0, 1.0);
            let tex_u = uv.left() + u * uv.width();
            let tex_v = uv.top() + v * uv.height();
            if alpha.sample_linear_alpha(tex_u, tex_v) > 0.0 {
                return Some(image.clone());
            }
        }
        None
    }
}

pub fn encode_color_image_png(image: &ColorImage) -> Result<Vec<u8>, String> {
    let width = u32::try_from(image.width()).map_err(|_| "Image width is too large".to_owned())?;
    let height =
        u32::try_from(image.height()).map_err(|_| "Image height is too large".to_owned())?;
    if width == 0
        || height == 0
        || image
            .width()
            .checked_mul(image.height())
            .is_none_or(|pixels| pixels != image.pixels.len())
    {
        return Err("Image dimensions are invalid".to_owned());
    }

    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("Could not start PNG encoding: {error}"))?;
    let mut stream = writer
        .stream_writer()
        .map_err(|error| format!("Could not stream PNG pixels: {error}"))?;
    let mut row = Vec::with_capacity(image.width().saturating_mul(4));
    for pixels in image.pixels.chunks_exact(image.width()) {
        row.clear();
        for pixel in pixels {
            row.extend_from_slice(&pixel.to_srgba_unmultiplied());
        }
        stream
            .write_all(&row)
            .map_err(|error| format!("Could not encode PNG pixels: {error}"))?;
    }
    stream
        .finish()
        .map_err(|error| format!("Could not finish PNG encoding: {error}"))?;
    writer
        .finish()
        .map_err(|error| format!("Could not finish PNG file: {error}"))?;
    Ok(output)
}

pub fn decode_kitty_image_for_export(image: &KittyImage) -> Result<ColorImage, String> {
    decode_kitty_image(image, MAX_RGBA_BYTES)
        .ok_or_else(|| "The terminal image could not be decoded for export".to_owned())
}

/// Computes the viewport-clipped destination rect and corresponding normalized UV
/// rect for a placement in device-pixel-aligned egui points.
pub(crate) fn placement_geometry(
    layout: GridLayout,
    viewport_offset_rows: usize,
    alternate_screen: bool,
    pixels_per_point: f32,
    placement: &KittyPlacement,
    image: &KittyImage,
) -> Option<(Rect, Rect)> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let src = placement.source();
    if src.width == 0 || src.height == 0 {
        return None;
    }
    let display_row = if viewport_offset_rows == 0 || alternate_screen {
        placement.anchor.row()
    } else {
        placement.anchor.row().checked_add(viewport_offset_rows)?
    };
    if display_row >= layout.dimensions.rows()
        || placement.anchor.column() >= layout.dimensions.columns()
        || placement.columns == 0
        || placement.rows == 0
    {
        return None;
    }
    let ppp = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let (extent_w, extent_h) = placement.pixel_extent();
    if extent_w == 0 || extent_h == 0 {
        return None;
    }
    let cell_left = layout.rect.left() + placement.anchor.column() as f32 * layout.metrics.width;
    let cell_top = layout.rect.top() + display_row as f32 * layout.metrics.height;

    let offset_x = f32::from(placement.pixel_offset.0) / ppp;
    let offset_y = f32::from(placement.pixel_offset.1) / ppp;

    let min_x = ((cell_left + offset_x) * ppp).round() / ppp;
    let min_y = ((cell_top + offset_y) * ppp).round() / ppp;
    let max_x = min_x + (extent_w as f32 / ppp);
    let max_y = min_y + (extent_h as f32 / ppp);

    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    let unclipped_dest = Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y));
    let clipped_dest = unclipped_dest.intersect(layout.rect);
    if !clipped_dest.is_positive() {
        return None;
    }

    let img_w = image.width as f32;
    let img_h = image.height as f32;
    let base_u0 = src.x as f32 / img_w;
    let base_v0 = src.y as f32 / img_h;
    let base_u1 = src.x.saturating_add(src.width) as f32 / img_w;
    let base_v1 = src.y.saturating_add(src.height) as f32 / img_h;

    let clip_u0 = (clipped_dest.left() - unclipped_dest.left()) / unclipped_dest.width();
    let clip_v0 = (clipped_dest.top() - unclipped_dest.top()) / unclipped_dest.height();
    let clip_u1 = (clipped_dest.right() - unclipped_dest.left()) / unclipped_dest.width();
    let clip_v1 = (clipped_dest.bottom() - unclipped_dest.top()) / unclipped_dest.height();

    let uv = Rect::from_min_max(
        Pos2::new(
            base_u0 + clip_u0 * (base_u1 - base_u0),
            base_v0 + clip_v0 * (base_v1 - base_v0),
        ),
        Pos2::new(
            base_u0 + clip_u1 * (base_u1 - base_u0),
            base_v0 + clip_v1 * (base_v1 - base_v0),
        ),
    );
    Some((clipped_dest, uv))
}

pub(crate) fn paint_kitty_layer(
    painter: &egui::Painter,
    layout: GridLayout,
    snapshot: TerminalSnapshot<'_>,
    cache: &mut KittyTextureCache,
    layer: KittyLayer,
) {
    let placements = snapshot.kitty_placements();
    if placements.is_empty() {
        return;
    }
    let clip_rect = layout.rect.intersect(painter.clip_rect());
    if !clip_rect.is_positive() {
        return;
    }
    let clipped_painter = painter.with_clip_rect(clip_rect);
    let ppp = painter.ctx().pixels_per_point();
    let images = snapshot.kitty_images();
    let viewport_offset_rows = snapshot.viewport_offset_rows();
    let alternate_screen = snapshot.modes().alternate_screen();

    // `snapshot.kitty_placements()` is already sorted by `(z, image_id)`.
    for placement in placements {
        if classify_layer(placement.z) != layer {
            continue;
        }
        let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
            continue;
        };
        let Some((dest_rect, uv)) = placement_geometry(
            layout,
            viewport_offset_rows,
            alternate_screen,
            ppp,
            placement,
            image,
        ) else {
            continue;
        };
        let Some(texture) = cache.ensure_texture(painter.ctx(), image) else {
            continue;
        };
        clipped_painter.image(texture.id(), dest_rect, uv, Color32::WHITE);
    }
}

/// Returns `true` when `pos` (in egui points) lands on a non-transparent pixel
/// (sampled with linear filtering matching `TextureOptions::LINEAR`) of any
/// above-text (`z >= 0`) placement inside the terminal grid.
pub(crate) fn is_point_occluded_by_above_text(
    cache: &KittyTextureCache,
    snapshot: TerminalSnapshot<'_>,
    layout: GridLayout,
    pixels_per_point: f32,
    pos: Pos2,
) -> bool {
    if !layout.rect.contains(pos) {
        return false;
    }
    let placements = snapshot.kitty_placements();
    if placements.is_empty() {
        return false;
    }
    let images = snapshot.kitty_images();
    let viewport_offset_rows = snapshot.viewport_offset_rows();
    let alternate_screen = snapshot.modes().alternate_screen();

    for placement in placements {
        if classify_layer(placement.z) != KittyLayer::AboveText
            || !cache.is_drawable(placement.image_id)
        {
            continue;
        }
        let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
            continue;
        };
        let Some(plane) = cache.alpha_plane(image) else {
            continue;
        };
        let Some((dest_rect, uv)) = placement_geometry(
            layout,
            viewport_offset_rows,
            alternate_screen,
            pixels_per_point,
            placement,
            image,
        ) else {
            continue;
        };
        if !dest_rect.contains(pos) || dest_rect.width() <= 0.0 || dest_rect.height() <= 0.0 {
            continue;
        }
        let u = ((pos.x - dest_rect.left()) / dest_rect.width()).clamp(0.0, 1.0);
        let v = ((pos.y - dest_rect.top()) / dest_rect.height()).clamp(0.0, 1.0);
        let tex_u = uv.left() + u * uv.width();
        let tex_v = uv.top() + v * uv.height();
        if plane.sample_linear_alpha(tex_u, tex_v) > 0.0 {
            return true;
        }
    }
    false
}

/// Precomputed per-cell above-text occlusion map built once from budgeted,
/// already-decoded drawable images in `KittyTextureCache` (never decoding per cell).
#[derive(Clone, Debug, Default)]
pub(crate) struct KittyCellOcclusionMap {
    columns: usize,
    rows: usize,
    cells: Vec<bool>,
    row_content_ids: Vec<Option<u64>>,
}

impl KittyCellOcclusionMap {
    pub(crate) fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub(crate) fn is_content_occluded(&self, content: festerm_core::ContentPosition) -> bool {
        if self.cells.is_empty() {
            return false;
        }
        let Some(row) = self
            .row_content_ids
            .iter()
            .position(|&id| id == Some(content.absolute_row))
        else {
            return false;
        };
        self.is_occluded(CellPosition {
            column: content.column,
            row,
        })
    }

    pub(crate) fn build(
        cache: &KittyTextureCache,
        snapshot: TerminalSnapshot<'_>,
        layout: GridLayout,
        pixels_per_point: f32,
    ) -> Self {
        let columns = layout.dimensions.columns();
        let rows = layout.dimensions.rows();
        let placements = snapshot.kitty_placements();
        if placements.is_empty()
            || !placements.iter().any(|p| {
                classify_layer(p.z) == KittyLayer::AboveText && cache.is_drawable(p.image_id)
            })
        {
            return Self {
                columns,
                rows,
                cells: Vec::new(),
                row_content_ids: Vec::new(),
            };
        }
        let mut cells = vec![false; columns.saturating_mul(rows)];
        let images = snapshot.kitty_images();
        let viewport_offset_rows = snapshot.viewport_offset_rows();
        let alternate_screen = snapshot.modes().alternate_screen();
        let mut any_occluded = false;

        for placement in placements {
            if classify_layer(placement.z) != KittyLayer::AboveText
                || !cache.is_drawable(placement.image_id)
            {
                continue;
            }
            let Some(image) = images.iter().find(|img| img.id == placement.image_id) else {
                continue;
            };
            // Only actually drawable (budget-admitted, decoded) images can occlude.
            let Some(plane) = cache.alpha_plane(image) else {
                continue;
            };
            let Some((dest_rect, uv)) = placement_geometry(
                layout,
                viewport_offset_rows,
                alternate_screen,
                pixels_per_point,
                placement,
                image,
            ) else {
                continue;
            };
            let col_start = (((dest_rect.left() - layout.rect.left()) / layout.metrics.width)
                .floor() as isize)
                .clamp(0, columns as isize) as usize;
            let col_end = (((dest_rect.right() - layout.rect.left()) / layout.metrics.width).ceil()
                as isize)
                .clamp(0, columns as isize) as usize;
            let row_start = (((dest_rect.top() - layout.rect.top()) / layout.metrics.height).floor()
                as isize)
                .clamp(0, rows as isize) as usize;
            let row_end = (((dest_rect.bottom() - layout.rect.top()) / layout.metrics.height).ceil()
                as isize)
                .clamp(0, rows as isize) as usize;
            for row in row_start..row_end {
                for column in col_start..col_end {
                    let idx = row * columns + column;
                    if cells[idx] {
                        continue;
                    }
                    let Some(cell_rect) = layout
                        .cell_geometry()
                        .cell_rect(CellPosition { column, row }, 1)
                    else {
                        continue;
                    };
                    let pos = cell_rect.center();
                    if !dest_rect.contains(pos) {
                        continue;
                    }
                    let u = ((pos.x - dest_rect.left()) / dest_rect.width()).clamp(0.0, 1.0);
                    let v = ((pos.y - dest_rect.top()) / dest_rect.height()).clamp(0.0, 1.0);
                    let tex_u = uv.left() + u * uv.width();
                    let tex_v = uv.top() + v * uv.height();
                    if plane.sample_linear_alpha(tex_u, tex_v) > 0.0 {
                        cells[idx] = true;
                        any_occluded = true;
                    }
                }
            }
        }
        if !any_occluded {
            return Self {
                columns,
                rows,
                cells: Vec::new(),
                row_content_ids: Vec::new(),
            };
        }
        let row_content_ids = (0..rows)
            .map(|row| snapshot.content_row_for_viewport_row(row))
            .collect();
        Self {
            columns,
            rows,
            cells,
            row_content_ids,
        }
    }

    pub(crate) fn is_occluded(&self, cell: CellPosition) -> bool {
        if self.cells.is_empty() || cell.column >= self.columns || cell.row >= self.rows {
            return false;
        }
        self.cells[cell.row * self.columns + cell.column]
    }
}

#[cfg(test)]
pub(crate) fn visible_placement_rects(
    layout: GridLayout,
    snapshot: TerminalSnapshot<'_>,
    pixels_per_point: f32,
) -> Vec<(u32, i32, Rect)> {
    let images = snapshot.kitty_images();
    snapshot
        .kitty_placements()
        .iter()
        .filter_map(|p| {
            let image = images.iter().find(|img| img.id == p.image_id)?;
            let (dest, _) = placement_geometry(
                layout,
                snapshot.viewport_offset_rows(),
                snapshot.modes().alternate_screen(),
                pixels_per_point,
                p,
                image,
            )?;
            Some((p.image_id, p.z, dest))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::CellMetrics,
        input::EncodedInputSink,
        selection::{selection_text_with_occlusion, Selection},
        smart_selection::{infer_with_occlusion, SmartSelectionScope},
        TerminalView, TerminalViewOptions,
    };
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use festerm_core::{
        kitty::{KittyConfig, KittyGeometry, KittyPngValidator},
        Dimensions, Terminal,
    };

    #[derive(Default)]
    struct TestSink;
    impl EncodedInputSink for TestSink {
        fn record_encoded_input(&mut self, _bytes: &[u8]) {}
    }

    fn base64_encode(input: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            out.push(TABLE[(b0 >> 2) as usize] as char);
            out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            out.push(if chunk.len() > 1 {
                TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                TABLE[(b2 & 0x3f) as usize] as char
            } else {
                '='
            });
        }
        out
    }

    fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(rgba).unwrap();
        drop(writer);
        out
    }

    fn kitty_terminal(columns: usize, rows: usize) -> Terminal {
        let mut terminal = Terminal::new(Dimensions::new(columns, rows).unwrap()).unwrap();
        terminal
            .enable_kitty_graphics(KittyConfig {
                rgba_budget: MAX_RGBA_BYTES,
                png_validator: Some(KittyPngValidator::new(validate_kitty_png)),
            })
            .unwrap();
        terminal
            .set_kitty_geometry(KittyGeometry::new(10, 20))
            .unwrap();
        terminal
    }

    #[allow(clippy::too_many_arguments)]
    fn upload_rgba(
        terminal: &mut Terminal,
        id: u32,
        width: u32,
        height: u32,
        rgba: &[u8],
        z: i32,
        cols: usize,
        rows: usize,
    ) {
        let b64 = base64_encode(rgba);
        let cmd = format!(
            "\x1b_Ga=T,f=32,s={width},v={height},i={id},z={z},c={cols},r={rows},C=1,q=2;{b64}\x1b\\"
        );
        terminal.ingest(cmd.as_bytes());
    }

    #[test]
    fn layer_classification_matches_kitty_z_thresholds() {
        assert_eq!(classify_layer(i32::MIN), KittyLayer::UnderBackground);
        assert_eq!(
            classify_layer(UNDER_BACKGROUND_Z_LIMIT - 1),
            KittyLayer::UnderBackground
        );
        assert_eq!(
            classify_layer(UNDER_BACKGROUND_Z_LIMIT),
            KittyLayer::BetweenBackgroundAndText
        );
        assert_eq!(classify_layer(-1), KittyLayer::BetweenBackgroundAndText);
        assert_eq!(classify_layer(0), KittyLayer::AboveText);
        assert_eq!(classify_layer(100), KittyLayer::AboveText);
    }

    #[test]
    fn png_validation_and_decode_are_bounded_and_exact() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 0];
        let png_bytes = encode_png_rgba(2, 1, &rgba);
        assert!(validate_kitty_png(&png_bytes, 2, 1));
        assert!(!validate_kitty_png(&png_bytes, 1, 2));
        assert!(!validate_kitty_png(b"not-a-png", 2, 1));

        let decoded = decode_png_rgba(&png_bytes, Some((2, 1)), 8).unwrap();
        assert_eq!(decoded.size, [2, 1]);
        assert_eq!(
            decoded[(0, 0)],
            Color32::from_rgba_unmultiplied(255, 0, 0, 255)
        );
        assert_eq!(decoded[(1, 0)], Color32::TRANSPARENT);

        assert!(decode_png_rgba(&png_bytes, Some((2, 1)), 7).is_none());

        // Trailing bytes after IEND must be rejected.
        let mut with_trailing = png_bytes.clone();
        with_trailing.extend_from_slice(b"extra");
        assert!(!validate_kitty_png(&with_trailing, 2, 1));

        // Missing IEND (truncated) must be rejected.
        assert!(!validate_kitty_png(
            &png_bytes[..png_bytes.len() - 12],
            2,
            1
        ));

        // Corrupted CRC on IDAT/IEND must be rejected.
        let mut bad_crc = png_bytes.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0xff;
        assert!(!validate_kitty_png(&bad_crc, 2, 1));
    }

    #[test]
    fn exported_png_round_trips_the_exact_decoded_pixels() {
        let image = ColorImage::from_rgba_unmultiplied([2, 1], &[255, 0, 0, 255, 0, 255, 0, 127]);
        let encoded = encode_color_image_png(&image).unwrap();
        let decoded = decode_png_rgba(&encoded, Some((2, 1)), 8).unwrap();
        assert_eq!(decoded.pixels, image.pixels);

        let mut terminal = kitty_terminal(2, 1);
        upload_rgba(
            &mut terminal,
            1,
            2,
            1,
            &[255, 0, 0, 255, 0, 255, 0, 127],
            0,
            1,
            1,
        );
        let decoded_source = decode_kitty_image_for_export(
            &TerminalSnapshot::from_terminal(&terminal).kitty_images()[0],
        )
        .unwrap();
        assert_eq!(decoded_source.pixels, image.pixels);
    }

    #[test]
    fn texture_cache_enforces_budget_lru_and_invalidates_on_serial_change() {
        let mut terminal = kitty_terminal(10, 4);
        let px1 = [255, 0, 0, 255, 255, 0, 0, 255]; // 8 bytes
        let px2 = [0, 255, 0, 255, 0, 255, 0, 255]; // 8 bytes
        let px3 = [0, 0, 255, 255, 0, 0, 255, 255]; // 8 bytes
        upload_rgba(&mut terminal, 1, 2, 1, &px1, 0, 1, 1);
        upload_rgba(&mut terminal, 2, 2, 1, &px2, 0, 1, 1);

        let mut cache = KittyTextureCache::default();
        cache.set_budget_bytes(20);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.sync(snapshot);
        assert!(cache.ensure_decoded(&snapshot.kitty_images()[0]).is_some());
        assert!(cache.ensure_decoded(&snapshot.kitty_images()[1]).is_some());
        assert_eq!(cache.used_bytes(), 20);
        assert_eq!(cache.len(), 2);

        // Adding a 3rd 10-byte (8 RGBA + 2 alpha) image evicts image 1 (LRU).
        upload_rgba(&mut terminal, 3, 2, 1, &px3, 0, 1, 1);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.sync(snapshot);
        let img3 = snapshot.kitty_images().iter().find(|i| i.id == 3).unwrap();
        assert!(cache.ensure_decoded(img3).is_some());
        assert_eq!(cache.used_bytes(), 20);
        assert!(cache.alpha_plane(img3).is_some());

        // Re-uploading image 2 bumps its serial and invalidates the old entry.
        upload_rgba(&mut terminal, 2, 2, 1, &px1, 0, 1, 1);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.sync(snapshot);
        assert_eq!(cache.used_bytes(), 10);
    }

    #[test]
    fn device_pixel_geometry_and_viewport_clipping() {
        let mut terminal = kitty_terminal(10, 4);
        let png = encode_png_rgba(1, 1, &[10, 20, 30, 255]);
        let b64 = base64_encode(&png);
        // Place at (1, 1) with pixel offset (3, 5) spanning 2x2 cells.
        terminal.ingest(
            format!("\x1b[2;2H\x1b_Ga=T,f=100,i=1,X=3,Y=5,c=2,r=2,C=1,q=2;{b64}\x1b\\").as_bytes(),
        );
        let layout = GridLayout {
            rect: Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(100.0, 80.0)),
            dimensions: Dimensions::new(10, 4).unwrap(),
            metrics: CellMetrics::new(10.0, 20.0).unwrap(),
        };
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let rects_2x = visible_placement_rects(layout, snapshot, 2.0);
        assert_eq!(rects_2x.len(), 1);
        assert_eq!(rects_2x[0].2.left(), 11.5); // 10.0 + 3/2
        assert_eq!(rects_2x[0].2.top(), 22.5); // 20.0 + 5/2
                                               // Core pixel_extent preserves 1:1 aspect inside (17, 35) -> (17, 17) device px = (8.5, 8.5) pt.
        assert_eq!(rects_2x[0].2.right(), 20.0);
        assert_eq!(rects_2x[0].2.bottom(), 31.0);

        // Scroll up 3 rows in history so row 1 moves to row 4 (off-screen).
        for _ in 0..5 {
            terminal.ingest(b"\n");
        }
        let scrolled = TerminalSnapshot::from_terminal_viewport(&terminal, 3);
        assert!(visible_placement_rects(layout, scrolled, 1.0).is_empty());
    }

    #[test]
    fn above_text_non_transparent_pixels_suppress_interactions_while_transparent_and_lower_retain_them(
    ) {
        let mut terminal = kitty_terminal(20, 4);
        terminal.ingest(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07 word");
        // Image 1: z=0 (above text), 4x2 pixels (2:1 aspect matching 2x1 cells):
        // left half opaque, right half transparent.
        let mut split_rgba = Vec::with_capacity(4 * 2 * 4);
        for _y in 0..2 {
            split_rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
        }
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 1, 4, 2, &split_rgba, 0, 2, 1);

        // Image 2: z=-1 (between bg and text), opaque, placed at col 5..9, row 0 ("word").
        let opaque_rgba = [0, 255, 0, 255];
        terminal.ingest(b"\x1b[1;6H");
        upload_rgba(&mut terminal, 2, 1, 1, &opaque_rgba, -1, 4, 1);

        let layout = GridLayout {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 80.0)),
            dimensions: Dimensions::new(20, 4).unwrap(),
            metrics: CellMetrics::new(10.0, 20.0).unwrap(),
        };
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let mut cache = KittyTextureCache::default();
        cache.prepare_frame(snapshot, layout, 1.0);
        let decode_count_before = cache.decode_count();
        let map = KittyCellOcclusionMap::build(&cache, snapshot, layout, 1.0);
        assert_eq!(cache.decode_count(), decode_count_before);

        // Col 0 is under the opaque left half of image 1 -> occluded.
        assert!(map.is_occluded(CellPosition { column: 0, row: 0 }));
        // Col 1 is under the transparent right half of image 1 -> NOT occluded.
        assert!(!map.is_occluded(CellPosition { column: 1, row: 0 }));
        // Col 5 ("word") is over z=-1 image 2 -> NOT occluded.
        assert!(!map.is_occluded(CellPosition { column: 5, row: 0 }));

        assert_eq!(
            cache
                .image_at_point(snapshot, layout, 1.0, Pos2::new(5.0, 5.0))
                .map(|image| image.id),
            Some(1)
        );
        assert!(cache
            .image_at_point(snapshot, layout, 1.0, Pos2::new(15.0, 5.0))
            .is_none());
        assert_eq!(
            cache
                .image_at_point(snapshot, layout, 1.0, Pos2::new(55.0, 5.0))
                .map(|image| image.id),
            Some(2)
        );

        // Copy omits hidden col 0 ("l"), retains transparent col 1 ("i") and z=-1 ("word").
        let mut sel = Selection::default();
        sel.begin(CellPosition { column: 0, row: 0 });
        sel.extend(CellPosition { column: 8, row: 0 });
        sel.finish();
        let copied = selection_text_with_occlusion(snapshot, &sel, &mut |content| {
            map.is_occluded(CellPosition {
                column: content.column,
                row: content.absolute_row as usize,
            })
        });
        assert_eq!(copied.as_deref(), Some("ink word"));

        // Smart selection is refused at occluded col 0, succeeds at z=-1 col 6.
        assert!(infer_with_occlusion(
            snapshot,
            CellPosition { column: 0, row: 0 },
            SmartSelectionScope::Sentence,
            &mut |cell| map.is_occluded(cell),
        )
        .is_none());
        assert!(infer_with_occlusion(
            snapshot,
            CellPosition { column: 6, row: 0 },
            SmartSelectionScope::Sentence,
            &mut |cell| map.is_occluded(cell),
        )
        .is_some());
    }

    #[test]
    fn image_hit_prefers_topmost_non_transparent_drawable_placement() {
        let mut terminal = kitty_terminal(2, 1);
        upload_rgba(&mut terminal, 1, 1, 1, &[255, 0, 0, 255], 0, 1, 1);
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 2, 1, 1, &[0, 0, 255, 255], 1, 1, 1);
        let layout = GridLayout {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(20.0, 20.0)),
            dimensions: Dimensions::new(2, 1).unwrap(),
            metrics: CellMetrics::new(10.0, 20.0).unwrap(),
        };
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let mut cache = KittyTextureCache::default();
        cache.prepare_frame(snapshot, layout, 1.0);
        let image = cache
            .image_at_point(snapshot, layout, 1.0, Pos2::new(5.0, 5.0))
            .unwrap();
        assert_eq!(image.id, 2);

        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 3, 1, 1, &[0, 0, 0, 0], 2, 1, 1);
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.prepare_frame(snapshot, layout, 1.0);
        let image = cache
            .image_at_point(snapshot, layout, 1.0, Pos2::new(5.0, 5.0))
            .unwrap();
        assert_eq!(image.id, 2);
    }

    #[test]
    fn view_exposes_bounded_image_accessibility_label_and_suppresses_occluded_link() {
        let terminal = kitty_terminal(20, 4);

        struct State {
            terminal: Terminal,
            view: TerminalView,
            sink: TestSink,
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(240.0, 100.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    state.view.show_with_options(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        TerminalViewOptions::default(),
                    );
                },
                State {
                    terminal,
                    view: TerminalView::default(),
                    sink: TestSink,
                },
            );
        harness.run();
        {
            let terminal = &mut harness.state_mut().terminal;
            terminal
                .set_kitty_geometry(KittyGeometry::new(10, 20))
                .unwrap();
            terminal.ingest(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
            terminal.ingest(b"\x1b[1;1H");
            upload_rgba(terminal, 1, 1, 1, &[255, 0, 0, 255], 0, 2, 1);
            assert_eq!(terminal.kitty_placements().len(), 1);
        }
        harness.run();
        assert_eq!(harness.state().terminal.kitty_placements().len(), 1);

        let images = harness
            .query_all_by_role(egui::accesskit::Role::Image)
            .collect::<Vec<_>>();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].accesskit_node().label().as_deref(), Some("image"));
    }

    #[test]
    fn view_pointer_selection_and_link_activation_respect_above_text_alpha_and_lower_layers() {
        let terminal = kitty_terminal(20, 4);

        struct State {
            terminal: Terminal,
            view: TerminalView,
            sink: TestSink,
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(240.0, 100.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    state.view.show_with_options(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        TerminalViewOptions::default(),
                    );
                },
                State {
                    terminal,
                    view: TerminalView::default(),
                    sink: TestSink,
                },
            );
        harness.run();
        {
            let terminal = &mut harness.state_mut().terminal;
            terminal
                .set_kitty_geometry(KittyGeometry::new(10, 20))
                .unwrap();
            terminal.ingest(
                b"\x1b]8;;https://blocked.test\x07ab\x1b]8;;\x07  \x1b]8;;https://open.test\x07cd\x1b]8;;\x07",
            );
            // Image 1: z=0 (above text), 4x4 pixels (1:1 aspect -> full cell height):
            // left 2 cols opaque (col 0), right 2 cols transparent (col 1).
            terminal.ingest(b"\x1b[1;1H");
            let mut rgba = Vec::with_capacity(4 * 4 * 4);
            for _y in 0..4 {
                rgba.extend_from_slice(&[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]);
            }
            upload_rgba(terminal, 1, 4, 4, &rgba, 0, 2, 1);
            // Image 2: z=-1 (between bg & text), opaque over cols 4..6 ("cd").
            terminal.ingest(b"\x1b[1;5H");
            upload_rgba(terminal, 2, 1, 1, &[0, 255, 0, 255], -1, 2, 1);
        }
        harness.run();

        let rect = harness.state().view.diagnostics().grid_rect.unwrap();
        let metrics = harness.state().view.diagnostics().cell_metrics.unwrap();
        let cell_center = |col: usize| {
            Pos2::new(
                rect.left() + (col as f32 + 0.5) * metrics.width,
                rect.top() + 0.5 * metrics.height,
            )
        };

        // Command+Click on opaque col 0 -> link suppressed.
        let p0 = cell_center(0);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();
        assert!(harness.state_mut().view.take_link_requests().is_empty());

        // Command+Click on lower-layer col 4 -> link activated.
        let p4 = cell_center(4);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p4,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p4,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();
        assert_eq!(
            harness
                .state_mut()
                .view
                .take_link_requests()
                .iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>(),
            vec!["https://open.test/"]
        );

        // Primary press at opaque col 0 -> selection suppressed.
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        assert!(!harness.state().view.selection.is_active());
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        // Primary press at transparent col 1 -> selection begins.
        let p1 = cell_center(1);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p1,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        assert!(harness.state().view.selection.is_active());
    }

    #[test]
    fn renderer_paints_kitty_layers_and_equal_z_image_id_in_protocol_order() {
        let mut terminal = kitty_terminal(10, 2);
        // Cell (0,0) has red background and text 'A'.
        terminal.ingest(b"\x1b[41mA\x1b[0m");
        // Under-background (z = -2^30 - 1)
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(
            &mut terminal,
            10,
            1,
            1,
            &[10, 20, 30, 255],
            UNDER_BACKGROUND_Z_LIMIT - 1,
            1,
            1,
        );
        // Between-background-and-text (z = -1)
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 20, 1, 1, &[40, 50, 60, 255], -1, 1, 1);
        // Above-text (z = 0, id = 31 and id = 30 -> id 30 must paint before id 31)
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 31, 1, 1, &[70, 80, 90, 255], 0, 1, 1);
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 30, 1, 1, &[100, 110, 120, 255], 0, 1, 1);

        let mut cache = crate::TerminalRenderCache::default();
        let mut glyphs = crate::renderer::GlyphCache::default();
        let dirty = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.update(snapshot, &dirty);

        let ctx = egui::Context::default();
        crate::install_terminal_fonts(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let layout = GridLayout {
                rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 40.0)),
                dimensions: terminal.dimensions(),
                metrics: CellMetrics::new(10.0, 20.0).unwrap(),
            };
            crate::renderer::paint_grid(
                ui.painter().clone(),
                crate::renderer::GridPaint {
                    layout,
                    snapshot,
                    cache: &cache,
                    selection: &Selection::default(),
                    fonts: &crate::FontSettings::default(),
                    color_scheme: None,
                    shape_cell_runs: false,
                    focused: false,
                    suppress_cursor: true,
                    full_redraw: true,
                },
                &mut glyphs,
            );
        });

        let texture_id_for = |id: u32| {
            glyphs
                .kitty
                .entries
                .get(&id)
                .and_then(|e| e.texture.as_ref())
                .unwrap()
                .id()
        };
        let t_under = texture_id_for(10);
        let t_between = texture_id_for(20);
        let t_above_30 = texture_id_for(30);
        let t_above_31 = texture_id_for(31);

        let mut order = Vec::new();
        for clipped in &output.shapes {
            match &clipped.shape {
                egui::epaint::Shape::Mesh(mesh) => {
                    if mesh.texture_id == t_under {
                        order.push("under");
                    } else if mesh.texture_id == t_between {
                        order.push("between");
                    } else if mesh.texture_id == t_above_30 {
                        order.push("above_30");
                    } else if mesh.texture_id == t_above_31 {
                        order.push("above_31");
                    }
                }
                egui::epaint::Shape::Text(_) => {
                    order.push("text");
                }
                _ => {}
            }
        }

        output.textures_delta.clear();
        assert_eq!(
            order,
            vec!["under", "between", "text", "above_30", "above_31"]
        );
    }

    #[test]
    fn selection_highlight_paints_after_above_text_graphics() {
        let mut terminal = kitty_terminal(10, 2);
        terminal.ingest(b"hello");
        terminal.ingest(b"\x1b[1;1H");
        upload_rgba(&mut terminal, 1, 1, 1, &[255, 0, 0, 255], 0, 2, 1);

        let mut cache = crate::TerminalRenderCache::default();
        let mut glyphs = crate::renderer::GlyphCache::default();
        let dirty = terminal.take_dirty_rows();
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        cache.update(snapshot, &dirty);

        let mut selection = Selection::default();
        selection.begin(CellPosition { column: 0, row: 0 });
        selection.extend(CellPosition { column: 2, row: 0 });
        selection.finish();

        let ctx = egui::Context::default();
        crate::install_terminal_fonts(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let layout = GridLayout {
                rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 40.0)),
                dimensions: terminal.dimensions(),
                metrics: CellMetrics::new(10.0, 20.0).unwrap(),
            };
            crate::renderer::paint_grid(
                ui.painter().clone(),
                crate::renderer::GridPaint {
                    layout,
                    snapshot,
                    cache: &cache,
                    selection: &selection,
                    fonts: &crate::FontSettings::default(),
                    color_scheme: None,
                    shape_cell_runs: false,
                    focused: false,
                    suppress_cursor: true,
                    full_redraw: true,
                },
                &mut glyphs,
            );
        });

        let t_above = glyphs
            .kitty
            .entries
            .get(&1)
            .and_then(|e| e.texture.as_ref())
            .unwrap()
            .id();
        let mut above_idx = None;
        let mut sel_idx = None;
        for (idx, clipped) in output.shapes.iter().enumerate() {
            match &clipped.shape {
                egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == t_above => {
                    above_idx = Some(idx);
                }
                egui::epaint::Shape::Rect(rect) if rect.fill.a() == 140 => {
                    sel_idx = Some(idx);
                }
                _ => {}
            }
        }
        output.textures_delta.clear();
        assert!(above_idx.is_some());
        assert!(sel_idx.is_some());
        assert!(sel_idx.unwrap() > above_idx.unwrap());
    }

    #[test]
    fn command_click_revalidates_above_text_occlusion_on_release() {
        let terminal = kitty_terminal(20, 4);

        struct State {
            terminal: Terminal,
            view: TerminalView,
            sink: TestSink,
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(240.0, 100.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    state.view.show_with_options(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        TerminalViewOptions::default(),
                    );
                },
                State {
                    terminal,
                    view: TerminalView::default(),
                    sink: TestSink,
                },
            );
        harness.run();
        {
            let terminal = &mut harness.state_mut().terminal;
            terminal
                .set_kitty_geometry(KittyGeometry::new(10, 20))
                .unwrap();
            terminal.ingest(b"\x1b]8;;https://example.test\x07link\x1b]8;;\x07");
        }
        harness.run();

        let rect = harness.state().view.diagnostics().grid_rect.unwrap();
        let metrics = harness.state().view.diagnostics().cell_metrics.unwrap();
        let p0 = Pos2::new(
            rect.left() + 0.5 * metrics.width,
            rect.top() + 0.5 * metrics.height,
        );
        // Press while unoccluded.
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();

        // Now an above-text graphic appears over col 0 before release.
        {
            let terminal = &mut harness.state_mut().terminal;
            terminal.ingest(b"\x1b[1;1H");
            upload_rgba(terminal, 1, 1, 1, &[255, 0, 0, 255], 0, 2, 1);
        }
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p0,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        harness.run();
        assert!(harness.state_mut().view.take_link_requests().is_empty());
    }

    #[test]
    fn frozen_rectangle_copy_uses_capture_time_occlusion_snapshot() {
        let terminal = kitty_terminal(20, 4);

        struct State {
            terminal: Terminal,
            view: TerminalView,
            sink: TestSink,
        }
        let mut harness = Harness::builder()
            .with_size(egui::vec2(240.0, 100.0))
            .build_ui_state(
                |ui, state: &mut State| {
                    state.view.show_with_options(
                        ui,
                        &mut state.terminal,
                        &mut state.sink,
                        TerminalViewOptions::default(),
                    );
                },
                State {
                    terminal,
                    view: TerminalView::default(),
                    sink: TestSink,
                },
            );
        harness.run();
        {
            let terminal = &mut harness.state_mut().terminal;
            terminal
                .set_kitty_geometry(KittyGeometry::new(10, 20))
                .unwrap();
            terminal.ingest(b"abcd");
            terminal.ingest(b"\x1b[1;1H");
            upload_rgba(terminal, 1, 1, 1, &[255, 0, 0, 255], 0, 1, 1);
        }
        harness.run();

        {
            let state = harness.state_mut();
            state
                .view
                .begin_rectangle_copy(&state.terminal, true)
                .unwrap();
            // Remove the live graphic after rectangle copy froze.
            state.terminal.ingest(b"\x1b_Ga=d,d=a,q=2;\x1b\\");
            assert!(state.terminal.kitty_placements().is_empty());
        }

        let rect = harness.state().view.diagnostics().grid_rect.unwrap();
        let metrics = harness.state().view.diagnostics().cell_metrics.unwrap();
        let col_center = |col: usize| {
            Pos2::new(
                rect.left() + (col as f32 + 0.5) * metrics.width,
                rect.top() + 0.5 * metrics.height,
            )
        };
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: col_center(0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        harness
            .input_mut()
            .events
            .push(egui::Event::PointerMoved(col_center(3)));
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos: col_center(3),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();

        let copied = {
            let state = harness.state();
            state.view.selected_text(&state.terminal)
        };
        // Col 0 ('a') remains omitted by the frozen occlusion snapshot.
        assert_eq!(copied.as_deref(), Some("bcd"));
    }

    #[test]
    fn linear_sampling_and_reservation_pressure_do_not_alter_protocol_state() {
        let plane = AlphaPlane {
            width: 2,
            height: 1,
            alpha: Arc::from(vec![255, 0]),
        };
        // Midway between texel 0 (0.25) and texel 1 (0.75) has linear alpha > 0.
        assert!(plane.sample_linear_alpha(0.5, 0.5) > 0.0);
        // Right texel center (0.75) has alpha == 0.
        assert_eq!(plane.sample_linear_alpha(0.75, 0.5), 0.0);

        // Reservation pressure only affects render-side cache, never core store.
        let mut terminal = kitty_terminal(10, 2);
        upload_rgba(
            &mut terminal,
            1,
            2,
            1,
            &[255, 0, 0, 255, 0, 0, 0, 255],
            0,
            1,
            1,
        );
        let reservation = KittyGraphicsReservation::new(4);
        let mut cache = KittyTextureCache::default();
        cache.set_reservation(reservation.clone());
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        assert!(cache.ensure_decoded(&snapshot.kitty_images()[0]).is_none());
        assert_eq!(reservation.used_bytes(), 0);
        assert_eq!(terminal.kitty_images().len(), 1);
        assert_eq!(terminal.kitty_placements().len(), 1);
    }

    #[test]
    fn frame_drawable_set_avoids_thrash_across_two_large_images_and_matches_occlusion() {
        // Two 3200x2500 images: each charges 32M GPU + 8M alpha = 40M > half of 64 MiB.
        let mut terminal = kitty_terminal(20, 4);
        let width = 3200u32;
        let height = 2500u32;
        // Build compressed zlib RGBA stream for 3200x2500 (mostly zeros, tiny compressed size).
        let raw_len = (width as usize) * (height as usize) * 4;
        let raw = vec![255u8; raw_len];
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &raw).unwrap();
        let compressed = encoder.finish().unwrap();
        let b64 = base64_encode(&compressed);

        for (id, col) in [(1u32, 1usize), (2u32, 4usize)] {
            terminal.ingest(format!("\x1b[1;{col}H").as_bytes());
            let prefix = format!(
                "\x1b_Ga=T,f=32,o=z,S={raw_len},s={width},v={height},i={id},z=0,c=2,r=1,C=1,q=2;"
            );
            let mut packet = prefix.into_bytes();
            packet.extend_from_slice(b64.as_bytes());
            packet.extend_from_slice(b"\x1b\\");
            terminal.ingest(&packet);
        }
        assert_eq!(terminal.kitty_placements().len(), 2);

        let layout = GridLayout {
            rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 80.0)),
            dimensions: Dimensions::new(20, 4).unwrap(),
            metrics: CellMetrics::new(10.0, 20.0).unwrap(),
        };
        let snapshot = TerminalSnapshot::from_terminal(&terminal);
        let reservation = KittyGraphicsReservation::new(MAX_RGBA_BYTES);
        let mut cache = KittyTextureCache::default();
        cache.set_reservation(reservation.clone());

        for _frame in 0..3 {
            cache.prepare_frame(snapshot, layout, 1.0);
        }
        // Only image 1 fits in 64 MiB (40M <= 67.1M); image 2 is skipped without thrashing.
        assert_eq!(cache.decode_count(), 1);
        assert!(cache.is_drawable(1));
        assert!(!cache.is_drawable(2));
        assert_eq!(reservation.used_bytes(), 40_000_000);
        assert!(reservation.used_bytes() <= MAX_RGBA_BYTES);

        let map = KittyCellOcclusionMap::build(&cache, snapshot, layout, 1.0);
        assert!(map.is_occluded(CellPosition { column: 0, row: 0 }));
        assert!(!map.is_occluded(CellPosition { column: 3, row: 0 }));
    }
}
