//! Bounded Kitty direct-transport authority. No textures or host resources.

use std::{
    fmt,
    io::Write as _,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use miniz_oxide::inflate::{
    core::{
        decompress,
        inflate_flags::{TINFL_FLAG_PARSE_ZLIB_HEADER, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF},
        DecompressorOxide,
    },
    TINFLStatus,
};
use smallvec::SmallVec;

use crate::{Cursor, Dimensions, TRANSPORT_QUEUE_HIGH_WATERMARK};

pub(crate) const MAX_CONTROL_BYTES: usize = 512;
const MAX_CHUNK_BYTES: usize = 4096;
const MAX_DATA_BYTES: usize = 4 * 1024 * 1024;
const MAX_FINAL_BYTES: usize = MAX_DATA_BYTES.div_ceil(3) * 4;
pub(crate) const MAX_KITTY_PACKET_BYTES: usize = MAX_CONTROL_BYTES + 1 + MAX_FINAL_BYTES;
pub(crate) const MAX_KITTY_REPLY_BYTES: usize = MAX_CONTROL_BYTES + 96;
const MAX_AXIS: u32 = 8192;
const MAX_PIXELS: u32 = 16_777_216;
const MAX_IMAGES: usize = 64;
const MAX_LIVE_IMAGES: usize = 2 * MAX_IMAGES;
const MAX_PLACEMENTS: usize = 256;
/// Global ceiling for the fixed decoded-image reservation of one terminal.
pub const MAX_RGBA_BYTES: usize = MAX_PIXELS as usize * 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KittyPacket {
    pub bytes: Vec<u8>,
    pub abort: Option<KittyAbort>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KittyAbort {
    Interrupted,
    Limit,
}

pub(crate) fn reserve_staging<T>(limit: usize) -> Option<Vec<T>> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(limit).ok()?;
    Some(buffer)
}

pub(crate) fn clone_staging(buffer: &Vec<u8>) -> Vec<u8> {
    let mut copy = Vec::with_capacity(buffer.capacity());
    copy.extend_from_slice(buffer);
    copy
}

/// Trusted grid-cell geometry in device pixels, independent of the renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KittyGeometry {
    cell_width: u16,
    cell_height: u16,
}

impl KittyGeometry {
    pub fn new(cell_width: u16, cell_height: u16) -> Option<Self> {
        (cell_width != 0 && cell_height != 0).then_some(Self {
            cell_width,
            cell_height,
        })
    }

    pub const fn cell_pixels(self) -> (u16, u16) {
        (self.cell_width, self.cell_height)
    }

    pub(crate) fn grid_pixels(self, dimensions: Dimensions) -> Option<(usize, usize)> {
        Some((
            dimensions.columns().checked_mul(self.cell_width.into())?,
            dimensions.rows().checked_mul(self.cell_height.into())?,
        ))
    }
}

type ValidatePng = dyn Fn(&[u8], u32, u32) -> bool + Send + Sync;

/// A trusted, deterministic PNG validation boundary. It receives structurally
/// checked PNGs with pre-admitted dimensions and must validate the complete
/// compressed pixel stream with bounded work and allocation before returning.
/// For the same inputs, the result must not depend on time or queue pressure.
#[derive(Clone)]
pub struct KittyPngValidator(Arc<ValidatePng>);

impl KittyPngValidator {
    pub fn new(validate: impl Fn(&[u8], u32, u32) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(validate))
    }
}

impl fmt::Debug for KittyPngValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KittyPngValidator")
    }
}

impl PartialEq for KittyPngValidator {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for KittyPngValidator {}

/// Fixed at opt-in; pending uploads are separately bounded to 4 MiB per buffer.
/// Outer zlib inflation is capped by the RGBA reservation for raw pixels or
/// by the 4 MiB file limit for PNG, before its pixel stream is validated.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KittyConfig {
    pub rgba_budget: usize,
    pub png_validator: Option<KittyPngValidator>,
}

impl KittyConfig {
    /// Core payload bytes to charge to the owner's protocol pool at opt-in:
    /// retained sources plus parser, upload, inflation and reply reservations.
    /// This does not include app validation or materialized textures.
    pub fn protocol_memory_bytes(&self) -> Option<usize> {
        (1..=MAX_RGBA_BYTES).contains(&self.rgba_budget).then(|| {
            self.retained_source_limit()
                + self.inflate_limit()
                + self.retained_source_limit() // one validated source awaiting publication
                + 3 * MAX_DATA_BYTES
                + MAX_KITTY_PACKET_BYTES
                + TRANSPORT_QUEUE_HIGH_WATERMARK
        })
    }

    fn inflate_limit(&self) -> usize {
        self.rgba_budget.max(MAX_DATA_BYTES)
    }

    fn retained_source_limit(&self) -> usize {
        self.rgba_budget.max(MAX_DATA_BYTES)
    }
}

impl Default for KittyConfig {
    fn default() -> Self {
        Self {
            rgba_budget: MAX_RGBA_BYTES,
            png_validator: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KittyFormat {
    Rgb,
    Rgba,
    Png,
}

/// Validated direct image source. The serial changes on every accepted upload.
#[derive(Clone, Eq, PartialEq)]
pub struct KittyImage {
    pub id: u32,
    pub number: Option<u32>,
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub format: KittyFormat,
    data: Arc<[u8]>,
    charge: usize,
    transient: bool,
}

impl KittyImage {
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for KittyImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KittyImage")
            .field("id", &self.id)
            .field("serial", &self.serial)
            .field("dimensions", &(self.width, self.height))
            .field("format", &self.format)
            .field("data_len", &self.data.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KittySourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: Option<u32>,
    pub anchor: Cursor,
    pub columns: usize,
    pub rows: usize,
    pub pixel_offset: (u16, u16),
    pub z: i32,
    source: KittySourceRect,
    pixel_width: u32,
    original_extent: (u32, u32),
    original_y_offset: u16,
    cell_height: u16,
    clipped_top: usize,
}

impl KittyPlacement {
    /// Visible destination width and height in device pixels, excluding offsets.
    pub fn pixel_extent(&self) -> (u32, u32) {
        let (top, bottom) = self.visible_y_edges();
        (self.pixel_width, bottom - top)
    }

    fn visible_y_edges(&self) -> (u32, u32) {
        let edge = |row: usize| self.cell_y_edge(row).min(self.original_extent.1);
        (edge(self.clipped_top), edge(self.clipped_top + self.rows))
    }

    fn cell_y_edge(&self, row: usize) -> u32 {
        ((row as u32) * u32::from(self.cell_height)).saturating_sub(self.original_y_offset.into())
    }

    pub fn source(&self) -> KittySourceRect {
        let (top_pixels, bottom_pixels) = self.visible_y_edges();
        let (width, height) = self.original_extent;
        let edge = |pixels| u64::from(self.source.height) * u64::from(pixels);
        let top = (edge(top_pixels) / u64::from(height)) as u32;
        let bottom = edge(bottom_pixels).div_ceil(u64::from(height)) as u32;
        let visible_width = (u64::from(self.source.width) * u64::from(self.pixel_width))
            .div_ceil(u64::from(width)) as u32;
        KittySourceRect {
            y: self.source.y + top,
            width: visible_width,
            height: bottom - top,
            ..self.source
        }
    }

    fn within_scroll_region(&self, surface: KittySurface) -> bool {
        self.anchor.row() >= surface.top
            && self.anchor.row() + self.rows - 1 <= surface.bottom
            && self.anchor.column() >= surface.left
            && self.anchor.column() + self.columns - 1 <= surface.right
    }

    fn scrolls_off_top(&self, surface: KittySurface, count: usize) -> bool {
        let clip = count.saturating_sub(self.anchor.row() - surface.top);
        clip >= self.rows || self.cell_y_edge(self.clipped_top + clip) >= self.original_extent.1
    }

    fn is_visible(&self) -> bool {
        self.rows != 0 && self.cell_y_edge(self.clipped_top) < self.original_extent.1
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct KittySurface {
    pub dimensions: Dimensions,
    pub cursor: Cursor,
    pub top: usize,
    pub bottom: usize,
    pub left: usize,
    pub right: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct KittyMove {
    pub cursor: Cursor,
    pub scroll: usize,
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    Invalid,
    Unsupported,
    Missing,
    Limit,
}

impl Failure {
    const fn message(self) -> &'static str {
        match self {
            Self::Invalid => "EINVAL:invalid graphics command",
            Self::Unsupported => "ENOTSUP:unsupported graphics command",
            Self::Missing => "ENOENT:image not found",
            Self::Limit => "ENOSPC:graphics limit exceeded",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Controls([Option<i64>; 128]);

impl Controls {
    fn get(&self, key: u8) -> Option<i64> {
        self.0[key as usize]
    }

    fn number(&self, key: u8) -> Option<u32> {
        self.get(key).map(|value| value as u32)
    }

    fn code(&self, key: u8, default: u8) -> u8 {
        self.get(key).map_or(default, |value| value as u8)
    }

    fn uses_only(&self, keys: &[u8]) -> bool {
        self.0
            .iter()
            .enumerate()
            .all(|(key, value)| value.is_none() || keys.contains(&(key as u8)))
    }

    fn reply(
        &self,
        id: Option<u32>,
        result: Result<(), Failure>,
        enqueue: impl FnOnce(&[u8]) -> bool,
    ) -> bool {
        self.write_reply(id, self.number(b'p'), result, |_| {}, enqueue)
    }

    fn write_reply(
        &self,
        id: Option<u32>,
        placement: Option<u32>,
        result: Result<(), Failure>,
        fields: impl FnOnce(&mut &mut [u8]),
        enqueue: impl FnOnce(&[u8]) -> bool,
    ) -> bool {
        let quiet = self.number(b'q').unwrap_or(0);
        let number = self.number(b'I');
        let id = self.number(b'i').or(id);
        if (self.get(b'i').is_none() && number.is_none())
            || quiet == 2
            || (quiet == 1 && result.is_ok())
        {
            return true;
        }
        let mut bytes = [0; MAX_KITTY_REPLY_BYTES];
        let mut buffer = bytes.as_mut_slice();
        buffer.write_all(b"\x1b_G").expect("fixed reply prefix");
        let mut separator = "";
        for (key, value) in [('i', id), ('I', number), ('p', placement)] {
            if let Some(value) = value {
                write!(buffer, "{separator}{key}={value}").expect("bounded reply fields");
                separator = ",";
            }
        }
        fields(&mut buffer);
        let message = result.map_or_else(Failure::message, |()| "OK");
        write!(buffer, ";{message}\x1b\\").expect("bounded reply message");
        let len = MAX_KITTY_REPLY_BYTES - buffer.len();
        enqueue(&bytes[..len])
    }

    fn feature_reply(
        &self,
        config: &KittyConfig,
        parsed: Result<&[u8], Failure>,
        enqueue: impl FnOnce(&[u8]) -> bool,
    ) {
        if self.number(b'i').is_none() {
            return;
        }
        let action = self.code(b'e', 0);
        self.write_reply(
            None,
            None,
            parsed.map(drop),
            |buffer| {
                if parsed.is_err() || !b"qtTpd".contains(&action) {
                    return;
                }
                write!(buffer, ",e={}", char::from(action)).expect("bounded feature action");
                let upload = b"qtT".contains(&action);
                let placement = b"Tp".contains(&action);
                for key in b"dfosvSNpcrxywhXYCz" {
                    let Some(value) = self.get(*key) else {
                        continue;
                    };
                    let supported = match *key {
                        b'd' => action == b'd' && b"AiInN".contains(&(value as u8)),
                        b'f' => {
                            upload
                                && (value == 24 || (value == 100 && config.png_validator.is_some()))
                        }
                        b'o' => upload && value == i64::from(b'z'),
                        b's' | b'v' => upload && (1..=i64::from(MAX_AXIS)).contains(&value),
                        b'S' => {
                            let limit =
                                if self.code(b'o', 0) == b'z' && self.number(b'f') != Some(100) {
                                    config.rgba_budget
                                } else {
                                    MAX_DATA_BYTES
                                };
                            upload && (1..=limit as i64).contains(&value)
                        }
                        b'N' => upload && value == 1,
                        b'p' => value != 0,
                        b'c' | b'r' => placement && (1..=i64::from(MAX_AXIS)).contains(&value),
                        b'X' | b'Y' => placement && (1..=i64::from(u16::MAX)).contains(&value),
                        b'C' => placement && value == 1,
                        _ => placement && value != 0,
                    };
                    if supported {
                        let key = char::from(*key);
                        if matches!(key, 'd' | 'o') {
                            write!(buffer, ",{key}={}", char::from(value as u8))
                                .expect("bounded feature value");
                        } else {
                            write!(buffer, ",{key}={value}").expect("bounded feature value");
                        }
                    }
                }
            },
            enqueue,
        );
    }
}

fn parse_packet(packet: &[u8]) -> (Controls, Result<&[u8], Failure>) {
    let mut controls = Controls([None; 128]);
    let separator = packet
        .iter()
        .take(MAX_CONTROL_BYTES + 1)
        .position(|&byte| byte == b';')
        .unwrap_or(packet.len());
    if separator > MAX_CONTROL_BYTES {
        return (controls, Err(Failure::Limit));
    }
    let mut result = Ok(packet.get(separator + 1..).unwrap_or_default());
    let mut unknown = false;
    if separator == 0 {
        return (controls, result);
    }
    for (index, item) in packet[..separator].split(|&byte| byte == b',').enumerate() {
        if index >= 28 {
            result = Err(Failure::Limit);
            break;
        }
        let [key, b'=', value @ ..] = item else {
            result = Err(Failure::Invalid);
            continue;
        };
        let parsed = match key {
            b'a' | b't' | b'd' | b'o' | b'e'
                if value.len() == 1 && value[0].is_ascii_alphabetic() =>
            {
                Some(i64::from(value[0]))
            }
            b'z' => std::str::from_utf8(value)
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .map(i64::from),
            b'i' | b'I' | b'p' | b'q' | b'f' | b's' | b'v' | b'S' | b'm' | b'N' | b'c' | b'r'
            | b'x' | b'y' | b'w' | b'h' | b'X' | b'Y' | b'C' | b'U'
                if !value.is_empty() && value.iter().all(u8::is_ascii_digit) =>
            {
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|value| value.parse::<u32>().ok())
                    .map(i64::from)
            }
            _ if key.is_ascii_alphabetic() && !b"atdoeziIpqfsvSmNcrxywhXYCU".contains(key) => {
                unknown = true;
                continue;
            }
            _ => None,
        };
        if let Some(value) = parsed {
            let slot = &mut controls.0[*key as usize];
            if slot.is_some() {
                result = Err(Failure::Invalid);
            } else {
                *slot = Some(value);
            }
        } else {
            result = Err(Failure::Invalid);
        }
    }
    let feature = controls.code(b'a', b't') == b'q' && controls.get(b'e').is_some();
    if (unknown && !feature)
        || (controls.get(b'i').is_some() && controls.get(b'I').is_some())
        || b"iIp".iter().any(|&key| controls.get(key) == Some(0))
        || controls.number(b'q').unwrap_or(0) > 2
        || (!feature
            && b"mNC"
                .iter()
                .any(|&key| controls.number(key).unwrap_or(0) > 1))
    {
        result = Err(Failure::Invalid);
    }
    (controls, result)
}

#[derive(Eq, PartialEq)]
pub(crate) struct KittyBuffer {
    pub images: Vec<KittyImage>,
    pub placements: Vec<KittyPlacement>,
    upload: Option<Controls>,
    staging: Vec<u8>,
}

impl Clone for KittyBuffer {
    fn clone(&self) -> Self {
        let mut images = Vec::with_capacity(MAX_IMAGES);
        images.extend_from_slice(&self.images);
        let mut placements = Vec::with_capacity(MAX_PLACEMENTS);
        placements.extend_from_slice(&self.placements);
        Self {
            images,
            placements,
            upload: self.upload.clone(),
            staging: clone_reserved(&self.staging, MAX_DATA_BYTES),
        }
    }
}

impl KittyBuffer {
    fn new() -> Option<Self> {
        Some(Self {
            images: reserve_staging(MAX_IMAGES)?,
            placements: reserve_staging(MAX_PLACEMENTS)?,
            upload: None,
            staging: reserve_staging(MAX_DATA_BYTES)?,
        })
    }

    pub(crate) fn reset(&mut self) {
        self.images.clear();
        self.placements.clear();
        self.abort_upload();
    }

    fn abort_upload(&mut self) {
        self.upload = None;
        self.staging.clear();
    }

    fn resolve(&self, controls: &Controls) -> Option<u32> {
        controls.number(b'i').or_else(|| {
            let number = controls.number(b'I')?;
            self.images
                .iter()
                .rfind(|image| image.number == Some(number))
                .map(|image| image.id)
        })
    }

    fn remove_image(&mut self, id: u32) {
        self.images.retain(|image| image.id != id);
        self.placements.retain(|placement| placement.image_id != id);
    }

    fn replace_placement(&mut self, placement: KittyPlacement) {
        if let Some(id) = placement.placement_id {
            self.placements.retain(|existing| {
                existing.image_id != placement.image_id || existing.placement_id != Some(id)
            });
        }
        if placement.is_visible() {
            let position = self.placements.partition_point(|other| {
                (other.z, other.image_id) <= (placement.z, placement.image_id)
            });
            self.placements.insert(position, placement);
        }
    }

    fn adds_placement(
        &self,
        surface: KittySurface,
        (planned, movement): &(KittyPlacement, Option<KittyMove>),
    ) -> bool {
        let scroll = movement.map_or(0, |movement| movement.scroll);
        planned.is_visible()
            && !self.placements.iter().any(|existing| {
                (planned.placement_id.is_some()
                    && existing.image_id == planned.image_id
                    && existing.placement_id == planned.placement_id)
                    || (scroll != 0
                        && existing.within_scroll_region(surface)
                        && existing.scrolls_off_top(surface, scroll))
            })
    }

    pub(crate) fn scroll(&mut self, surface: KittySurface, count: usize, down: bool) {
        self.placements.retain_mut(|placement| {
            let row = placement.anchor.row();
            if !placement.within_scroll_region(surface) {
                return true;
            }
            if down {
                if count > surface.bottom - row {
                    return false;
                }
                placement.anchor.row += count;
                placement.rows = placement
                    .rows
                    .min(surface.bottom + 1 - placement.anchor.row);
            } else {
                if placement.scrolls_off_top(surface, count) {
                    return false;
                }
                let clip = count.saturating_sub(row - surface.top);
                placement.anchor.row = (row - (count - clip)).max(surface.top);
                placement.rows -= clip;
                placement.clipped_top += clip;
                if clip != 0 {
                    placement.pixel_offset.1 = 0;
                }
            }
            placement.is_visible()
        });
    }
}

#[derive(Eq, PartialEq)]
pub(crate) struct KittyAuthority {
    config: KittyConfig,
    pub geometry: Option<KittyGeometry>,
    pub buffers: [KittyBuffer; 2],
    query_staging: Vec<u8>,
    inflate_staging: Vec<u8>,
    next_id: u32,
    serial: u64,
    handoff: bool,
    pending: Option<ValidationBarrier>,
}

impl Clone for KittyAuthority {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            geometry: self.geometry,
            buffers: self.buffers.clone(),
            query_staging: clone_reserved(&self.query_staging, MAX_DATA_BYTES),
            inflate_staging: clone_reserved(&self.inflate_staging, self.config.inflate_limit()),
            next_id: self.next_id,
            serial: self.serial,
            handoff: self.handoff,
            // A terminal copy never owns the original terminal's worker job.
            pending: None,
        }
    }
}

impl fmt::Debug for KittyAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KittyAuthority")
            .field("geometry", &self.geometry)
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Eviction {
    images: SmallVec<[(usize, u32); MAX_LIVE_IMAGES]>,
    placement: Option<(usize, u32)>,
}

#[derive(Clone, Copy, Default)]
struct ImageCharge {
    rgba: usize,
    source: usize,
}

struct EvictionCandidate {
    priority: u8,
    serial: u64,
    buffer: usize,
    id: u32,
    charge: ImageCharge,
    placements: usize,
}

#[derive(Eq, PartialEq)]
struct ValidationBarrier {
    id: u64,
    work: Option<KittyValidation>,
    reset_requested: bool,
}

/// One complete Kitty upload or data query awaiting off-owner validation.
/// The request owns the pre-reserved parser and decode buffers until returned.
#[derive(Eq, PartialEq)]
#[must_use = "validate the job and return its result to the terminal owner"]
pub struct KittyValidation {
    id: u64,
    index: usize,
    controls: Controls,
    payload_start: usize,
    encoding: UploadEncoding,
    config: KittyConfig,
    packet: KittyPacket,
    data: Vec<u8>,
    inflated: Vec<u8>,
}

/// The result of validating a [`KittyValidation`]. Only its original terminal
/// can publish the result, once, in PTY stream order.
#[must_use = "return the result to the originating terminal to release its barrier"]
pub struct KittyValidationResult {
    work: KittyValidation,
    validation: Result<ValidatedUpload, Failure>,
}

type ImageAdmission = (KittyFormat, u32, u32, usize);

struct ValidatedUpload {
    image: ImageAdmission,
    source: Option<Arc<[u8]>>,
}

impl KittyValidation {
    /// Performs bounded base64, zlib, and image validation. Call on the worker.
    pub fn validate(mut self) -> KittyValidationResult {
        let payload = &self.packet.bytes[self.payload_start..];
        let validation = decode_chunk(payload, false, self.encoding.input_limit(), &mut self.data)
            .and_then(|()| {
                let data = self.encoding.decode(&self.data, &mut self.inflated)?;
                let image = admitted_image(&self.controls, data, &self.config)?;
                let source = (self.controls.code(b'a', b't') != b'q').then(|| Arc::from(data));
                Ok(ValidatedUpload { image, source })
            });
        KittyValidationResult {
            work: self,
            validation,
        }
    }
}

fn clone_reserved(buffer: &Vec<u8>, minimum: usize) -> Vec<u8> {
    if buffer.capacity() < minimum {
        // The original allocation is loaned to a worker.
        debug_assert!(buffer.is_empty());
        Vec::with_capacity(minimum)
    } else {
        clone_staging(buffer)
    }
}

impl KittyAuthority {
    pub(crate) fn new(config: KittyConfig) -> Option<Self> {
        config.protocol_memory_bytes()?;
        Some(Self {
            inflate_staging: reserve_staging(config.inflate_limit())?,
            config,
            geometry: None,
            buffers: [KittyBuffer::new()?, KittyBuffer::new()?],
            query_staging: reserve_staging(MAX_DATA_BYTES)?,
            next_id: 1,
            serial: 0,
            handoff: false,
            pending: None,
        })
    }

    pub(crate) fn reset(&mut self) {
        debug_assert!(self.pending.is_none());
        for buffer in &mut self.buffers {
            buffer.reset();
        }
    }

    pub(crate) fn defer_reset(&mut self) -> bool {
        if let Some(pending) = &mut self.pending {
            pending.reset_requested = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn clear_placements(&mut self) {
        for buffer in &mut self.buffers {
            buffer.placements.clear();
        }
    }

    pub(crate) fn abort_uploads(&mut self) {
        for buffer in &mut self.buffers {
            buffer.abort_upload();
        }
    }

    pub(crate) fn enable_worker_handoff(&mut self) {
        self.handoff = true;
    }

    pub(crate) fn validation_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn take_validation(&mut self) -> Option<KittyValidation> {
        self.pending.as_mut()?.work.take()
    }

    pub(crate) fn accepts_validation(&self, result: &KittyValidationResult) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|job| job.id == result.work.id)
    }

    pub(crate) fn defer_validation(
        &mut self,
        index: usize,
        packet: KittyPacket,
    ) -> Result<(), KittyPacket> {
        if !self.handoff || self.geometry.is_none() || packet.abort.is_some() {
            return Err(packet);
        }
        let (controls, payload) = parse_packet(&packet.bytes);
        let Ok(payload) = payload else {
            return Err(packet);
        };
        let payload_start = packet.bytes.len() - payload.len();
        let query = controls.code(b'a', b't') == b'q';
        let effective = self
            .continuation_controls(index, &controls)
            .unwrap_or_else(|| controls.clone());
        if !b"qtT".contains(&effective.code(b'a', b't'))
            || effective.get(b'e').is_some()
            || effective.number(b'm') == Some(1)
            || effective.code(b't', b'd') != b'd'
        {
            return Err(packet);
        }
        let Ok(encoding) = upload_parameters(&effective, &self.config) else {
            return Err(packet);
        };
        let data = self.take_upload_staging(index, query, &controls);
        static NEXT_VALIDATION_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_VALIDATION_ID.fetch_add(1, Ordering::Relaxed);
        debug_assert!(self.pending.is_none());
        self.pending = Some(ValidationBarrier {
            id,
            work: Some(KittyValidation {
                id,
                index,
                controls: effective,
                payload_start,
                encoding,
                config: self.config.clone(),
                packet,
                data,
                inflated: std::mem::take(&mut self.inflate_staging),
            }),
            reset_requested: false,
        });
        Ok(())
    }

    pub(crate) fn finish_validation(
        &mut self,
        result: KittyValidationResult,
        surface: KittySurface,
        mut enqueue: impl FnMut(&[u8]) -> bool,
    ) -> (KittyPacket, Option<KittyMove>, bool) {
        debug_assert!(self.accepts_validation(&result));
        let reset_requested = self.pending.take().expect("checked job").reset_requested;
        let KittyValidationResult {
            mut work,
            validation,
        } = result;
        let query = work.controls.code(b'a', b't') == b'q';
        work.inflated.clear();
        self.inflate_staging = work.inflated;
        self.return_upload_staging(work.index, query, work.data, None);
        let controls = &work.controls;
        let reply_id = controls
            .number(b'I')
            .and_then(|_| self.buffers[work.index].resolve(controls));
        let mut reply = |id, status| controls.reply(id, status, &mut enqueue);
        let result = validation.and_then(|ValidatedUpload { image, source }| {
            if let Some(source) = source {
                self.publish_upload(
                    work.index,
                    surface,
                    controls,
                    image,
                    (source.len(), || source),
                    &mut reply,
                )
            } else {
                reply(reply_id, Ok(()));
                Ok(None)
            }
        });
        let movement = match result {
            Ok(movement) => movement,
            Err(error) => {
                reply(reply_id, Err(error));
                None
            }
        };
        (work.packet, movement, reset_requested)
    }

    fn continuation_controls(&self, index: usize, controls: &Controls) -> Option<Controls> {
        if !controls.uses_only(b"mq") {
            return None;
        }
        let mut pending = self.buffers[index].upload.clone()?;
        pending.0[b'm' as usize] = controls.get(b'm');
        if controls.get(b'q').is_some() {
            pending.0[b'q' as usize] = controls.get(b'q');
        }
        Some(pending)
    }

    fn take_upload_staging(&mut self, index: usize, query: bool, controls: &Controls) -> Vec<u8> {
        if query {
            return std::mem::take(&mut self.query_staging);
        }
        let buffer = &mut self.buffers[index];
        let mut data = std::mem::take(&mut buffer.staging);
        if buffer.upload.take().is_none() || !controls.uses_only(b"mq") {
            data.clear();
        }
        data
    }

    fn return_upload_staging(
        &mut self,
        index: usize,
        query: bool,
        mut data: Vec<u8>,
        upload: Option<Controls>,
    ) {
        if upload.is_none() {
            data.clear();
        }
        if query {
            self.query_staging = data;
        } else {
            self.buffers[index].upload = upload;
            self.buffers[index].staging = data;
        }
    }

    pub(crate) fn handle(
        &mut self,
        index: usize,
        surface: KittySurface,
        packet: &KittyPacket,
        mut enqueue: impl FnMut(&[u8]) -> bool,
    ) -> Option<KittyMove> {
        let geometry = self.geometry?;
        let (mut controls, payload) = parse_packet(&packet.bytes);
        let query = controls.code(b'a', b't') == b'q';
        if query && controls.get(b'e').is_some() && packet.abort.is_none() {
            controls.feature_reply(&self.config, payload, enqueue);
            return None;
        }
        let continuation = self.continuation_controls(index, &controls);
        let mut data = self.take_upload_staging(index, query, &controls);
        if let Some(continuation) = continuation {
            controls = continuation;
        }
        let reply_id = controls
            .number(b'I')
            .and_then(|_| self.buffers[index].resolve(&controls));
        let controls = &controls;
        let payload = if let Some(reason) = packet.abort {
            Err(match reason {
                KittyAbort::Interrupted => Failure::Invalid,
                KittyAbort::Limit => Failure::Limit,
            })
        } else if controls.code(b't', b'd') != b'd' {
            Err(Failure::Unsupported)
        } else {
            payload
        };
        let mut reply = |id, result| controls.reply(id, result, &mut enqueue);
        let mut inflated = std::mem::take(&mut self.inflate_staging);
        let result = payload.and_then(|payload| match controls.code(b'a', b't') {
            b'q' | b't' | b'T' => {
                let more = controls.number(b'm') == Some(1);
                let encoding = upload_parameters(controls, &self.config)?;
                decode_chunk(payload, more, encoding.input_limit(), &mut data)?;
                if more {
                    return Ok(None);
                }
                let image_data = encoding.decode(&data, &mut inflated)?;
                let image = admitted_image(controls, image_data, &self.config)?;
                if query {
                    reply(reply_id, Ok(()));
                    return Ok(Some(None));
                }
                let source = (image_data.len(), || Arc::from(image_data));
                self.publish_upload(index, surface, controls, image, source, &mut reply)
                    .map(Some)
            }
            b'p' => {
                if !payload.is_empty() || !controls.uses_only(b"aiIpqcrxywhXYCzU") {
                    return Err(Failure::Invalid);
                }
                if controls.number(b'U').unwrap_or(0) != 0 {
                    return Err(Failure::Unsupported);
                }
                let buffer = &self.buffers[index];
                let image = buffer
                    .resolve(controls)
                    .and_then(|id| buffer.images.iter().find(|image| image.id == id))
                    .ok_or(Failure::Missing)?;
                let id = image.id;
                let placement =
                    plan_placement(controls, id, image.width, image.height, geometry, surface)?;
                let eviction = self.plan_eviction(
                    index,
                    id,
                    None,
                    buffer.adds_placement(surface, &placement),
                )?;
                if !reply(Some(id), Ok(())) {
                    return Ok(Some(None));
                }
                self.apply_eviction(eviction);
                Ok(Some(commit_placement(
                    &mut self.buffers[index],
                    surface,
                    Some(placement),
                )))
            }
            b'd' => {
                if !payload.is_empty() || !controls.uses_only(b"adiIpq") {
                    return Err(Failure::Invalid);
                }
                let delete = plan_delete(&self.buffers[index], controls)?;
                if reply(delete.id, Ok(())) {
                    delete.apply(&mut self.buffers[index]);
                }
                Ok(Some(None))
            }
            _ => Err(Failure::Unsupported),
        });

        inflated.clear();
        self.inflate_staging = inflated;
        self.return_upload_staging(
            index,
            query,
            data,
            matches!(result, Ok(None)).then(|| controls.clone()),
        );
        match result {
            Ok(movement) => movement.flatten(),
            Err(error) => {
                reply(reply_id, Err(error));
                None
            }
        }
    }

    fn publish_upload(
        &mut self,
        index: usize,
        surface: KittySurface,
        controls: &Controls,
        (format, width, height, charge): ImageAdmission,
        (source_len, source): (usize, impl FnOnce() -> Arc<[u8]>),
        reply: &mut impl FnMut(Option<u32>, Result<(), Failure>) -> bool,
    ) -> Result<Option<KittyMove>, Failure> {
        let geometry = self.geometry.ok_or(Failure::Invalid)?;
        let (id, advance_id) = controls
            .number(b'i')
            .map_or_else(|| (self.available_id(), true), |id| (id, false));
        let placement = if controls.code(b'a', b't') == b'T' {
            Some(plan_placement(
                controls, id, width, height, geometry, surface,
            )?)
        } else {
            None
        };
        let eviction = self.plan_eviction(
            index,
            id,
            Some(ImageCharge {
                rgba: charge,
                source: source_len,
            }),
            placement
                .as_ref()
                .is_some_and(|planned| self.buffers[index].adds_placement(surface, planned)),
        )?;
        let serial = self.serial.checked_add(1).ok_or(Failure::Limit)?;
        if !reply(Some(id), Ok(())) {
            return Ok(None);
        }
        self.apply_eviction(eviction);
        let buffer = &mut self.buffers[index];
        buffer.images.push(KittyImage {
            id,
            number: controls.number(b'I'),
            serial,
            width,
            height,
            format,
            data: source(),
            charge,
            transient: controls.number(b'N') == Some(1),
        });
        self.serial = serial;
        if advance_id {
            self.next_id = next_id(id);
        }
        Ok(commit_placement(buffer, surface, placement))
    }

    fn available_id(&self) -> u32 {
        let mut id = self.next_id;
        while self
            .buffers
            .iter()
            .any(|buffer| buffer.images.iter().any(|image| image.id == id))
        {
            id = next_id(id);
        }
        id
    }

    fn plan_eviction(
        &self,
        active: usize,
        image_id: u32,
        new_charge: Option<ImageCharge>,
        add_placement: bool,
    ) -> Result<Eviction, Failure> {
        let mut eviction = Eviction::default();
        let mut active_placements =
            self.buffers[active].placements.len() + usize::from(add_placement);
        if new_charge.is_none() && active_placements <= MAX_PLACEMENTS {
            return Ok(eviction);
        }
        let mut usage = new_charge.unwrap_or_default();
        let mut active_images = usize::from(new_charge.is_some());
        let mut candidates = SmallVec::<[EvictionCandidate; MAX_LIVE_IMAGES]>::new();
        for (index, buffer) in self.buffers.iter().enumerate() {
            for image in &buffer.images {
                let placements = buffer
                    .placements
                    .iter()
                    .filter(|placement| placement.image_id == image.id)
                    .count();
                if index == active && image.id == image_id && new_charge.is_some() {
                    eviction.images.push((index, image.id));
                    active_placements -= placements;
                    continue;
                }
                let charge = ImageCharge {
                    rgba: image.charge,
                    source: image.data.len(),
                };
                usage.rgba += charge.rgba;
                usage.source += charge.source;
                active_images += usize::from(index == active);
                if index == active && image.id == image_id {
                    continue;
                }
                let priority = match (placements, image.transient) {
                    (0, true) => 0,
                    (0, false) => 1,
                    _ => 2,
                };
                candidates.push(EvictionCandidate {
                    priority,
                    serial: image.serial,
                    buffer: index,
                    id: image.id,
                    charge,
                    placements,
                });
            }
        }
        candidates
            .sort_unstable_by_key(|image| (image.priority, image.serial, image.buffer, image.id));
        while active_images > MAX_IMAGES
            || usage.rgba > self.config.rgba_budget
            || usage.source > self.config.retained_source_limit()
        {
            let candidate = candidates
                .iter()
                .position(|image| active_images <= MAX_IMAGES || image.buffer == active)
                .ok_or(Failure::Limit)?;
            let image = candidates.remove(candidate);
            usage.rgba -= image.charge.rgba;
            usage.source -= image.charge.source;
            if image.buffer == active {
                active_images -= 1;
                active_placements -= image.placements;
            }
            eviction.images.push((image.buffer, image.id));
        }
        if active_placements > MAX_PLACEMENTS {
            if let Some(image) = candidates
                .iter()
                .find(|image| image.buffer == active && image.placements != 0)
            {
                eviction.images.push((image.buffer, image.id));
            } else {
                // Every placement belongs to the image being placed. Keep its
                // data and remove its first placement in stable paint order.
                eviction.placement = Some((active, image_id));
            }
        }
        Ok(eviction)
    }

    fn apply_eviction(&mut self, eviction: Eviction) {
        for (buffer, image) in eviction.images {
            self.buffers[buffer].remove_image(image);
        }
        if let Some((buffer, image)) = eviction.placement {
            let placements = &mut self.buffers[buffer].placements;
            if let Some(index) = placements
                .iter()
                .position(|placement| placement.image_id == image)
            {
                placements.remove(index);
            }
        }
    }
}

const fn next_id(id: u32) -> u32 {
    if id == u32::MAX {
        1
    } else {
        id + 1
    }
}

struct Delete {
    id: Option<u32>,
    placement_id: Option<u32>,
    free_image: bool,
}

impl Delete {
    fn apply(self, buffer: &mut KittyBuffer) {
        if let Some(id) = self.id {
            buffer.placements.retain(|placement| {
                placement.image_id != id
                    || self
                        .placement_id
                        .is_some_and(|id| placement.placement_id != Some(id))
            });
            if self.free_image
                && !buffer
                    .placements
                    .iter()
                    .any(|placement| placement.image_id == id)
            {
                buffer.remove_image(id);
            }
        } else {
            buffer.placements.clear();
            if self.free_image {
                buffer.images.clear();
            }
        }
    }
}

fn plan_delete(buffer: &KittyBuffer, controls: &Controls) -> Result<Delete, Failure> {
    let selector = controls.code(b'd', b'a');
    let free_image = selector.is_ascii_uppercase();
    let id = match selector.to_ascii_lowercase() {
        b'a' if controls.get(b'i').is_none()
            && controls.get(b'I').is_none()
            && controls.get(b'p').is_none() =>
        {
            None
        }
        key @ (b'i' | b'n')
            if controls
                .get(if key == b'i' { b'i' } else { b'I' })
                .is_some() =>
        {
            Some(
                buffer
                    .resolve(controls)
                    .filter(|&id| buffer.images.iter().any(|image| image.id == id))
                    .ok_or(Failure::Missing)?,
            )
        }
        _ => return Err(Failure::Unsupported),
    };
    Ok(Delete {
        id,
        placement_id: controls.number(b'p'),
        free_image,
    })
}

fn plan_placement(
    controls: &Controls,
    id: u32,
    width: u32,
    height: u32,
    geometry: KittyGeometry,
    surface: KittySurface,
) -> Result<(KittyPlacement, Option<KittyMove>), Failure> {
    let x = controls.number(b'x').unwrap_or(0);
    let y = controls.number(b'y').unwrap_or(0);
    let remaining_width = width
        .checked_sub(x)
        .filter(|&value| value != 0)
        .ok_or(Failure::Invalid)?;
    let remaining_height = height
        .checked_sub(y)
        .filter(|&value| value != 0)
        .ok_or(Failure::Invalid)?;
    let w = controls
        .number(b'w')
        .filter(|&value| value != 0)
        .unwrap_or(remaining_width);
    let h = controls
        .number(b'h')
        .filter(|&value| value != 0)
        .unwrap_or(remaining_height);
    let (cell_width, cell_height) = geometry.cell_pixels();
    let offset_x = controls.number(b'X').unwrap_or(0);
    let offset_y = controls.number(b'Y').unwrap_or(0);
    if w > remaining_width
        || h > remaining_height
        || offset_x >= u32::from(cell_width)
        || offset_y >= u32::from(cell_height)
    {
        return Err(Failure::Invalid);
    }
    let columns_requested = controls.number(b'c').filter(|&value| value != 0);
    let rows_requested = controls.number(b'r').filter(|&value| value != 0);
    if columns_requested.is_some_and(|columns| columns > MAX_AXIS)
        || rows_requested.is_some_and(|rows| rows > MAX_AXIS)
    {
        return Err(Failure::Limit);
    }
    let cell_width = u64::from(cell_width);
    let cell_height = u64::from(cell_height);
    let box_width = columns_requested.map(|n| u64::from(n) * cell_width - u64::from(offset_x));
    let box_height = rows_requested.map(|n| u64::from(n) * cell_height - u64::from(offset_y));
    let (pixel_width, pixel_height) =
        fit_placement(u64::from(w), u64::from(h), box_width, box_height);
    let columns = columns_requested.map_or_else(
        || (pixel_width + u64::from(offset_x)).div_ceil(cell_width),
        u64::from,
    );
    let rows = rows_requested.map_or_else(
        || (pixel_height + u64::from(offset_y)).div_ceil(cell_height),
        u64::from,
    );
    if columns > u64::from(MAX_AXIS) || rows > u64::from(MAX_AXIS) {
        return Err(Failure::Limit);
    }
    let cursor = surface.cursor;
    let inside = (surface.top..=surface.bottom).contains(&cursor.row())
        && (surface.left..=surface.right).contains(&cursor.column());
    let (top, bottom, left, right) = if inside {
        (surface.top, surface.bottom, surface.left, surface.right)
    } else {
        (
            0,
            surface.dimensions.rows() - 1,
            0,
            surface.dimensions.columns() - 1,
        )
    };
    let columns = columns as usize;
    let rows = (rows as usize).min(bottom + 1 - top);
    let visible_columns = columns.min(right + 1 - cursor.column());
    // Both pixel axes fit their checked, bounded cell spans.
    let visible_width = pixel_width.min(visible_columns as u64 * cell_width - u64::from(offset_x));
    let mut placement = KittyPlacement {
        image_id: id,
        placement_id: controls.number(b'p'),
        anchor: cursor,
        columns: visible_columns,
        rows,
        pixel_offset: (offset_x as u16, offset_y as u16),
        z: controls.get(b'z').unwrap_or(0) as i32,
        source: KittySourceRect {
            x,
            y,
            width: w,
            height: h,
        },
        pixel_width: visible_width as u32,
        original_extent: (pixel_width as u32, pixel_height as u32),
        original_y_offset: offset_y as u16,
        cell_height: cell_height as u16,
        clipped_top: 0,
    };
    if controls.number(b'C') == Some(1) {
        placement.rows = placement.rows.min(bottom + 1 - cursor.row());
        return Ok((placement, None));
    }
    let mut column = cursor.column() + visible_columns;
    let mut row = cursor.row() + rows - 1;
    if column > right {
        column = left;
        row += 1;
    }
    let scroll = if inside {
        row.saturating_sub(bottom)
    } else {
        0
    };
    placement.clipped_top = scroll.saturating_sub(cursor.row() - top).min(rows);
    placement.anchor.row = cursor.row() - scroll.min(cursor.row() - top);
    placement.rows = (rows - placement.clipped_top).min(bottom + 1 - placement.anchor.row());
    if placement.clipped_top != 0 {
        placement.pixel_offset.1 = 0;
    }
    Ok((
        placement,
        Some(KittyMove {
            cursor: Cursor {
                column,
                row: row.min(bottom),
            },
            scroll,
        }),
    ))
}

fn fit_placement(
    width: u64,
    height: u64,
    box_width: Option<u64>,
    box_height: Option<u64>,
) -> (u64, u64) {
    let width_scaled_height = |w: u64| (w * height / width).max(1);
    let height_scaled_width = |h: u64| (h * width / height).max(1);
    match (box_width, box_height) {
        (None, None) => (width, height),
        (Some(w), Some(h)) if w * height > h * width => (height_scaled_width(h), h),
        (Some(w), _) => (w, width_scaled_height(w)),
        (None, Some(h)) => (height_scaled_width(h), h),
    }
}

fn commit_placement(
    buffer: &mut KittyBuffer,
    surface: KittySurface,
    planned: Option<(KittyPlacement, Option<KittyMove>)>,
) -> Option<KittyMove> {
    let (placement, movement) = planned?;
    if let Some(movement) = movement {
        if movement.scroll != 0 {
            buffer.scroll(surface, movement.scroll, false);
        }
    }
    buffer.replace_placement(placement);
    movement
}

fn decode_chunk(
    payload: &[u8],
    more: bool,
    limit: usize,
    data: &mut Vec<u8>,
) -> Result<(), Failure> {
    if (more && payload.len() > MAX_CHUNK_BYTES) || payload.len() > MAX_FINAL_BYTES {
        return Err(Failure::Limit);
    }
    let padding = payload
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'=')
        .take(3)
        .count();
    let text = &payload[..payload.len() - padding];
    if padding > 2
        || (padding != 0 && (more || !payload.len().is_multiple_of(4)))
        || (more && (text.is_empty() || !text.len().is_multiple_of(4)))
        || text.len() % 4 == 1
    {
        return Err(Failure::Invalid);
    }
    let length = data.len() + text.len() * 6 / 8;
    if length > limit {
        return Err(Failure::Limit);
    }
    debug_assert!(length <= data.capacity());
    let mut bits = 0u32;
    let mut pending = 0;
    for &byte in text {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(Failure::Invalid),
        };
        bits = (bits << 6) | u32::from(value);
        pending += 6;
        if pending >= 8 {
            pending -= 8;
            data.push((bits >> pending) as u8);
        }
    }
    if bits & ((1 << pending) - 1) != 0 {
        return Err(Failure::Invalid);
    }
    Ok(())
}

fn pixel_charge(width: u32, height: u32, budget: usize) -> Result<usize, Failure> {
    if width == 0 || height == 0 {
        return Err(Failure::Invalid);
    }
    if width > MAX_AXIS || height > MAX_AXIS {
        return Err(Failure::Limit);
    }
    let pixels = width
        .checked_mul(height)
        .filter(|&value| value <= MAX_PIXELS)
        .ok_or(Failure::Limit)?;
    let charge = pixels as usize * 4;
    (charge <= budget).then_some(charge).ok_or(Failure::Limit)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum UploadEncoding {
    Plain(Option<usize>),
    Zlib(usize),
}

impl UploadEncoding {
    fn input_limit(&self) -> usize {
        match *self {
            Self::Plain(Some(expected)) => expected,
            _ => MAX_DATA_BYTES,
        }
    }

    fn decode<'a>(self, input: &'a [u8], output: &'a mut Vec<u8>) -> Result<&'a [u8], Failure> {
        match self {
            Self::Plain(expected) if expected.is_none_or(|expected| input.len() == expected) => {
                Ok(input)
            }
            Self::Plain(_) => Err(Failure::Invalid),
            Self::Zlib(expected) => inflate_exact(input, output, expected),
        }
    }
}

fn inflate_exact<'a>(
    mut input: &[u8],
    output: &'a mut Vec<u8>,
    expected: usize,
) -> Result<&'a [u8], Failure> {
    const OUTPUT_STEP: usize = 64 * 1024;
    debug_assert!(expected <= output.capacity());
    let mut decompressor = DecompressorOxide::new();
    let mut written = 0;
    loop {
        // Grow only the initialized slice. Its pre-reserved storage doubles
        // as the history window and never grows beyond the advertised size.
        output.resize((output.len() + OUTPUT_STEP).min(expected), 0);
        let (status, consumed, produced) = decompress(
            &mut decompressor,
            input,
            output,
            written,
            TINFL_FLAG_PARSE_ZLIB_HEADER | TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF,
        );
        input = &input[consumed..];
        written += produced;
        match status {
            TINFLStatus::Done if input.is_empty() && written == expected => return Ok(output),
            TINFLStatus::HasMoreOutput if output.len() < expected => {}
            _ => return Err(Failure::Invalid),
        }
    }
}

fn upload_encoding(controls: &Controls, config: &KittyConfig) -> Result<UploadEncoding, Failure> {
    let compressed = match controls.code(b'o', 0) {
        0 => false,
        b'z' => true,
        _ => return Err(Failure::Unsupported),
    };
    let advertised = controls.number(b'S').map(|value| value as usize);
    let format = controls.number(b'f').unwrap_or(32);
    let output_limit = match format {
        24 | 32 if compressed => config.rgba_budget,
        24 | 32 | 100 => MAX_DATA_BYTES,
        _ => return Err(Failure::Unsupported),
    };
    if compressed {
        let size = advertised
            .filter(|&size| size != 0)
            .ok_or(Failure::Invalid)?;
        if size > output_limit {
            return Err(Failure::Limit);
        }
    }
    let expected = if format == 100 {
        // S refers to the PNG file size, not the size of its decoded pixels.
        advertised
    } else {
        let width = controls.number(b's').ok_or(Failure::Invalid)?;
        let height = controls.number(b'v').ok_or(Failure::Invalid)?;
        let charge = pixel_charge(width, height, config.rgba_budget)?;
        let bytes = if format == 24 { charge / 4 * 3 } else { charge };
        if advertised.is_some_and(|size| size != bytes) {
            return Err(Failure::Invalid);
        }
        Some(bytes)
    };
    if expected.is_some_and(|size| size > output_limit) {
        return Err(Failure::Limit);
    }
    Ok(if compressed {
        UploadEncoding::Zlib(expected.ok_or(Failure::Invalid)?)
    } else {
        UploadEncoding::Plain(expected)
    })
}

fn upload_parameters(controls: &Controls, config: &KittyConfig) -> Result<UploadEncoding, Failure> {
    let action = controls.code(b'a', b't');
    let keys = if action == b'T' {
        &b"atfsvoSiIpqmNcrxywhXYCzU"[..]
    } else {
        &b"atfsvoSiIpqmN"[..]
    };
    if !controls.uses_only(keys) || (action == b'q' && controls.number(b'm') == Some(1)) {
        return Err(Failure::Invalid);
    }
    if controls.number(b'U').unwrap_or(0) != 0 {
        return Err(Failure::Unsupported);
    }
    upload_encoding(controls, config)
}

fn admitted_image(
    controls: &Controls,
    data: &[u8],
    config: &KittyConfig,
) -> Result<ImageAdmission, Failure> {
    let format = match controls.number(b'f').unwrap_or(32) {
        24 => KittyFormat::Rgb,
        32 => KittyFormat::Rgba,
        100 => KittyFormat::Png,
        _ => return Err(Failure::Unsupported),
    };
    if format == KittyFormat::Png {
        let (width, height, charge) = validate_png(data, config.rgba_budget)?;
        if controls.number(b's').is_some_and(|value| value != width)
            || controls.number(b'v').is_some_and(|value| value != height)
        {
            return Err(Failure::Invalid);
        }
        let validate = config.png_validator.as_ref().ok_or(Failure::Unsupported)?;
        if !(validate.0)(data, width, height) {
            return Err(Failure::Invalid);
        }
        return Ok((format, width, height, charge));
    }
    let width = controls.number(b's').ok_or(Failure::Invalid)?;
    let height = controls.number(b'v').ok_or(Failure::Invalid)?;
    let charge = pixel_charge(width, height, config.rgba_budget)?;
    let length = if format == KittyFormat::Rgb {
        charge / 4 * 3
    } else {
        charge
    };
    if data.len() != length {
        return Err(Failure::Invalid);
    }
    Ok((format, width, height, charge))
}

const fn png_crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb8_8320 & (0u32.wrapping_sub(value & 1)));
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const PNG_CRC: [u32; 256] = png_crc_table();

fn png_word(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().expect("fixed PNG field"))
}

fn validate_png(data: &[u8], budget: usize) -> Result<(u32, u32, usize), Failure> {
    let mut remaining = data
        .strip_prefix(b"\x89PNG\r\n\x1a\n")
        .ok_or(Failure::Invalid)?;
    let mut dimensions = None;
    let mut color = 0;
    let mut depth = 0;
    let mut palette = false;
    let mut idat = false;
    let mut idat_closed = false;
    let mut idat_size = 0;
    for index in 0..4096 {
        let header = remaining.get(..8).ok_or(Failure::Invalid)?;
        let length = png_word(&header[..4]) as usize;
        let kind = &header[4..];
        let end = length.checked_add(8).ok_or(Failure::Invalid)?;
        let crc_end = end.checked_add(4).ok_or(Failure::Invalid)?;
        let chunk = remaining.get(..crc_end).ok_or(Failure::Invalid)?;
        let payload = &chunk[8..end];
        if !kind.iter().all(u8::is_ascii_alphabetic) || !kind[2].is_ascii_uppercase() {
            return Err(Failure::Invalid);
        }
        let crc = !chunk[4..end].iter().fold(u32::MAX, |crc, &byte| {
            PNG_CRC[((crc as u8) ^ byte) as usize] ^ (crc >> 8)
        });
        if crc != png_word(&chunk[end..]) || (index == 0 && kind != b"IHDR") {
            return Err(Failure::Invalid);
        }
        remaining = &remaining[crc_end..];
        if kind != b"IDAT" && idat {
            idat_closed = true;
        }
        match kind {
            b"IHDR" if index == 0 && length == 13 => {
                let width = png_word(&payload[..4]);
                let height = png_word(&payload[4..8]);
                depth = payload[8];
                color = payload[9];
                let valid_depth = match color {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    _ => false,
                };
                if !valid_depth || payload[10..12] != [0, 0] || payload[12] > 1 {
                    return Err(Failure::Invalid);
                }
                dimensions = Some((width, height, pixel_charge(width, height, budget)?));
            }
            b"PLTE"
                if !idat
                    && !palette
                    && !matches!(color, 0 | 4)
                    && (3..=768).contains(&length)
                    && length.is_multiple_of(3)
                    && (color != 3 || length / 3 <= 1 << depth) =>
            {
                palette = true;
            }
            b"IDAT" if !idat_closed && (color != 3 || palette) => {
                idat = true;
                idat_size += length;
            }
            b"IEND" if idat_size != 0 && length == 0 && remaining.is_empty() => {
                return dimensions.ok_or(Failure::Invalid);
            }
            b"acTL" | b"fcTL" | b"fdAT" => return Err(Failure::Unsupported),
            _ if kind[0].is_ascii_lowercase() => {}
            _ => return Err(Failure::Invalid),
        }
    }
    Err(Failure::Limit)
}
