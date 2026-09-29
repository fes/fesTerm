use std::{
    collections::HashMap,
    ffi::{c_char, c_void, CStr},
    fmt,
    ptr::NonNull,
    sync::Arc,
    time::{Duration, Instant},
};

use egui::{
    epaint::{ClippedPrimitive, Primitive},
    Color32, ColorImage, Pos2, Rect, TextureId,
};
use windows_core::Interface;

#[derive(Clone, Copy)]
#[repr(C)]
struct Color([u8; 4]);

#[repr(C)]
struct Vertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: Color,
}

#[repr(C)]
struct MeshInput {
    texture: u64,
    vertices: *const Vertex,
    vertex_count: usize,
    indices: *const u32,
    index_count: usize,
    clip: [f32; 4],
}

unsafe extern "C" {
    fn festerm_d2d_process_cpu_ticks(output: *mut u64) -> i32;
    fn festerm_d2d_create(device: *mut c_void, queue: *mut c_void, output: *mut *mut c_void)
        -> i32;
    fn festerm_d2d_destroy(bridge: *mut c_void);
    fn festerm_d2d_error(bridge: *const c_void) -> *const c_char;
    fn festerm_d2d_texture(
        bridge: *mut c_void,
        id: u64,
        width: u32,
        height: u32,
        pixels: *const Color,
        pixel_count: usize,
    ) -> i32;
    fn festerm_d2d_prune(bridge: *mut c_void, ids: *const u64, count: usize) -> i32;
    fn festerm_d2d_prepare(
        bridge: *mut c_void,
        width: u32,
        height: u32,
        clear: Color,
        normalized_positions: bool,
        meshes: *const MeshInput,
        count: usize,
    ) -> i32;
    fn festerm_d2d_draw(
        bridge: *mut c_void,
        width: u32,
        height: u32,
        clip: *const f32,
        output: *mut *mut c_void,
    ) -> i32;
}

/// An explicit native failure or unsupported frame; callers must retain ordinary painting.
#[derive(Debug)]
pub struct Error {
    operation: &'static str,
    code: i32,
    message: String,
}

impl Error {
    fn unsupported(message: &str) -> Self {
        Self {
            operation: "frame validation",
            code: 0x80004001u32 as i32,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} (HRESULT 0x{:08x})",
            self.operation, self.message, self.code as u32
        )
    }
}

impl std::error::Error for Error {}

/// Cumulative kernel and user CPU time across all threads of this process.
/// Optional rendering probes must complete queued work before sampling a boundary.
pub fn process_cpu_time() -> Result<Duration, Error> {
    let mut ticks = 0;
    // The SDK writes a single initialized u64; no handles cross this boundary.
    let code = unsafe { festerm_d2d_process_cpu_ticks(&mut ticks) };
    if code < 0 {
        return Err(Error {
            operation: "process CPU timing",
            code,
            message: "GetProcessTimes failed".into(),
        });
    }
    Ok(Duration::from_secs(ticks / 10_000_000) + Duration::from_nanos((ticks % 10_000_000) * 100))
}

/// Native drawing is queued before subsequent wgpu use. Published pixels are
/// never overwritten; unchanged frames may share the same texture.
#[derive(Clone)]
pub struct Surface {
    pub texture: wgpu::Texture,
    pub rect: Rect,
    pub origin: [u32; 2],
}

#[derive(Default)]
pub struct RenderTimings {
    pub analysis: Duration,
    pub texture_upload: Duration,
    pub geometry_prepare: Duration,
    pub geometry_prepare_calls: usize,
    pub native_draw: Duration,
    pub mesh_count: usize,
    pub vertex_count: usize,
    pub index_count: usize,
    pub texture_count: usize,
    pub uploaded_texture_count: usize,
    pub surface_width: u32,
    pub surface_height: u32,
}

impl RenderTimings {
    fn record_analysis(
        &mut self,
        started: Instant,
        meshes: usize,
        vertices: usize,
        indices: usize,
    ) {
        self.analysis = started.elapsed();
        self.mesh_count = meshes;
        self.vertex_count = vertices;
        self.index_count = indices;
    }
}

const MAX_PRIMITIVES: usize = 250_000;
const MAX_FRAME_VERTICES: usize = 2_000_000;
const MAX_FRAME_INDICES: usize = 6_000_000;
const MAX_MESH_VERTICES: usize = 1_000_000;
const MAX_MESH_INDICES: usize = 3_000_000;
const MAX_TEXTURE_BYTES: usize = 96 * 1024 * 1024;
const MAX_TEXTURE_DIMENSION: usize = 8_192;
const MAX_TEXTURE_PIXELS: usize = 16_777_216;
const MAX_SURFACE_DIMENSION: u32 = 4_096;
const MAX_RETAINED_TEXTURE_UPLOAD_PIXELS: usize = MAX_TEXTURE_PIXELS;
const MAX_RETAINED_FRAME_VERTICES: usize = MAX_FRAME_VERTICES;
const MAX_RETAINED_MESH_INPUTS: usize = MAX_PRIMITIVES;

/// Serial ownership of a multithread-capable Direct2D/D3D11-on-12 context.
pub struct Renderer {
    native: NonNull<c_void>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    textures: HashMap<u64, Arc<ColorImage>>,
    used_texture_ids: Vec<u64>,
    texture_upload: Vec<Color>,
    mesh_vertices: Vec<Vertex>,
    mesh_inputs: Vec<MeshInput>,
}

// Native operations require &mut self. The SDK factory is multithread-capable,
// and every published wgpu surface is immutable from this renderer's perspective.
unsafe impl Send for Renderer {}

impl Renderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, Error> {
        let mut pointer = std::ptr::null_mut();
        // Borrowed native interfaces stay alive through creation; the SDK retains
        // its own references and verifies the queue belongs to this device.
        let code = unsafe {
            let native_device = device
                .as_hal::<wgpu::hal::api::Dx12>()
                .ok_or_else(|| Error::unsupported("DX12 device required"))?;
            let native_queue = queue
                .as_hal::<wgpu::hal::api::Dx12>()
                .ok_or_else(|| Error::unsupported("DX12 queue required"))?;
            festerm_d2d_create(
                native_device.raw_device().as_raw(),
                native_queue.as_raw().as_raw(),
                &mut pointer,
            )
        };
        if code < 0 {
            return Err(Error {
                operation: "initialization",
                code,
                message: "Direct2D/D3D11-on-12 initialization failed".into(),
            });
        }
        let native =
            NonNull::new(pointer).ok_or_else(|| Error::unsupported("native renderer missing"))?;
        Ok(Self {
            native,
            device,
            queue,
            textures: HashMap::new(),
            used_texture_ids: Vec::new(),
            texture_upload: Vec::new(),
            mesh_vertices: Vec::new(),
            mesh_inputs: Vec::new(),
        })
    }

    fn checked(&self, operation: &'static str, code: i32) -> Result<(), Error> {
        if code >= 0 {
            return Ok(());
        }
        // The bridge owns a NUL-terminated error buffer valid until its next call.
        let message = unsafe { CStr::from_ptr(festerm_d2d_error(self.native.as_ptr())) }
            .to_string_lossy()
            .into_owned();
        Err(Error {
            operation,
            code,
            message,
        })
    }

    /// Draws only the visible primitive bounds. The surrounding terminal background
    /// remains with egui, avoiding a full-window composite for a short changing line.
    pub fn render(
        &mut self,
        rect: Rect,
        pixels_per_point: f32,
        background: Color32,
        primitives: &[ClippedPrimitive],
        textures: &[(TextureId, Arc<ColorImage>)],
        timings: Option<&mut RenderTimings>,
    ) -> Result<Option<Surface>, Error> {
        let mut timings = timings;
        let Some(bounds) = self.prepare(
            rect,
            pixels_per_point,
            background,
            primitives,
            textures,
            timings.as_deref_mut(),
        )?
        else {
            return Ok(None);
        };
        self.draw_prepared(bounds, pixels_per_point, None, timings)
            .map(Some)
    }

    fn prepare(
        &mut self,
        rect: Rect,
        pixels_per_point: f32,
        background: Color32,
        primitives: &[ClippedPrimitive],
        textures: &[(TextureId, Arc<ColorImage>)],
        timings: Option<&mut RenderTimings>,
    ) -> Result<Option<Rect>, Error> {
        let mut timings = timings;
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || background.a() != 255 {
            return Err(Error::unsupported(
                "finite scale and opaque background required",
            ));
        }
        let analysis_started = timings.as_ref().map(|_| Instant::now());
        let analysis = analyze_frame(
            rect,
            pixels_per_point,
            primitives,
            &mut self.used_texture_ids,
        )?;
        if let (Some(timings), Some(started)) = (timings.as_deref_mut(), analysis_started) {
            timings.record_analysis(
                started,
                primitives.len(),
                analysis.vertex_count,
                analysis.index_count,
            );
            timings.texture_count = self.used_texture_ids.len();
        }
        let Some(bounds) = analysis.bounds else {
            return Ok(None);
        };
        let width = bounds.width() as u32;
        let height = bounds.height() as u32;
        if let Some(timings) = timings.as_deref_mut() {
            timings.surface_width = width;
            timings.surface_height = height;
        }
        if width > MAX_SURFACE_DIMENSION || height > MAX_SURFACE_DIMENSION {
            return Err(Error::unsupported("native frame exceeds supported bounds"));
        }

        let texture_upload_started = timings.as_ref().map(|_| Instant::now());
        let mut texture_bytes = 0usize;
        let mut uploaded_texture_count = 0usize;
        for &id in &self.used_texture_ids {
            let image = textures
                .iter()
                .find_map(|(texture_id, image)| {
                    (*texture_id == TextureId::Managed(id)).then_some(image)
                })
                .ok_or_else(|| Error::unsupported("captured texture pixels missing"))?;
            texture_bytes = texture_bytes.saturating_add(image.pixels.len().saturating_mul(4));
            if texture_bytes > MAX_TEXTURE_BYTES {
                return Err(Error::unsupported("native frame texture budget exceeded"));
            }
            if self
                .textures
                .get(&id)
                .is_some_and(|previous| previous.as_ref() == image.as_ref())
            {
                continue;
            }
            let [w, h] = image.size;
            if w == 0
                || h == 0
                || w > MAX_TEXTURE_DIMENSION
                || h > MAX_TEXTURE_DIMENSION
                || w * h > MAX_TEXTURE_PIXELS
                || image.pixels.len() != w * h
            {
                return Err(Error::unsupported("invalid texture dimensions"));
            }
            self.texture_upload.clear();
            self.texture_upload
                .extend(image.pixels.iter().map(|pixel| Color(pixel.to_array())));
            // Pixel storage remains alive for the synchronous SDK copy.
            let code = unsafe {
                festerm_d2d_texture(
                    self.native.as_ptr(),
                    id,
                    w as u32,
                    h as u32,
                    self.texture_upload.as_ptr(),
                    self.texture_upload.len(),
                )
            };
            self.checked("texture upload", code)?;
            uploaded_texture_count = uploaded_texture_count.saturating_add(1);
            self.textures.insert(id, image.clone());
        }
        // Prepared geometry and region draws share the validated frame's textures.
        let code = unsafe {
            festerm_d2d_prune(
                self.native.as_ptr(),
                self.used_texture_ids.as_ptr(),
                self.used_texture_ids.len(),
            )
        };
        self.checked("texture retirement", code)?;
        let used = &self.used_texture_ids;
        self.textures.retain(|id, _| used.binary_search(id).is_ok());
        if let (Some(timings), Some(started)) = (timings.as_deref_mut(), texture_upload_started) {
            timings.texture_upload = started.elapsed();
            timings.uploaded_texture_count = uploaded_texture_count;
        }

        let geometry_prepare_started = timings.as_ref().map(|_| Instant::now());
        self.mesh_vertices.clear();
        self.mesh_vertices.reserve(analysis.vertex_count);
        self.mesh_inputs.clear();
        self.mesh_inputs.reserve(primitives.len());
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                unreachable!()
            };
            let TextureId::Managed(id) = mesh.texture_id else {
                unreachable!()
            };
            if mesh.vertices.len() > MAX_MESH_VERTICES || mesh.indices.len() > MAX_MESH_INDICES {
                return Err(Error::unsupported("mesh exceeds supported bounds"));
            }
            let start = self.mesh_vertices.len();
            self.mesh_vertices
                .extend(mesh.vertices.iter().map(|vertex| Vertex {
                    position: [
                        raster_relative_position(vertex.pos.x * pixels_per_point, bounds.min.x),
                        raster_relative_position(vertex.pos.y * pixels_per_point, bounds.min.y),
                    ],
                    uv: [vertex.uv.x, vertex.uv.y],
                    color: Color(vertex.color.to_array()),
                }));
            let clip = pixel_rect(primitive.clip_rect, pixels_per_point)
                .intersect(pixel_rect(rect, pixels_per_point))
                .intersect(bounds);
            self.mesh_inputs.push(MeshInput {
                texture: id,
                vertices: self.mesh_vertices[start..].as_ptr(),
                vertex_count: mesh.vertices.len(),
                indices: mesh.indices.as_ptr(),
                index_count: mesh.indices.len(),
                clip: [
                    clip.min.x - bounds.min.x,
                    clip.min.y - bounds.min.y,
                    clip.max.x - bounds.min.x,
                    clip.max.y - bounds.min.y,
                ],
            });
        }
        // Inner Vec allocations and borrowed index slices stay alive through
        // preparation. The bridge copies/converts all geometry before returning.
        if let Some(timings) = timings.as_deref_mut() {
            timings.geometry_prepare_calls += 1;
        }
        let code = unsafe {
            festerm_d2d_prepare(
                self.native.as_ptr(),
                width,
                height,
                Color(background.to_array()),
                true,
                self.mesh_inputs.as_ptr(),
                self.mesh_inputs.len(),
            )
        };
        self.checked("geometry preparation", code)?;
        trim_vec_capacity(&mut self.texture_upload, MAX_RETAINED_TEXTURE_UPLOAD_PIXELS);
        trim_vec_capacity(&mut self.mesh_vertices, MAX_RETAINED_FRAME_VERTICES);
        trim_vec_capacity(&mut self.mesh_inputs, MAX_RETAINED_MESH_INPUTS);
        if let (Some(timings), Some(started)) = (timings, geometry_prepare_started) {
            timings.geometry_prepare = started.elapsed();
        }
        Ok(Some(bounds))
    }

    fn draw_prepared(
        &mut self,
        bounds: Rect,
        pixels_per_point: f32,
        clip: Option<Rect>,
        timings: Option<&mut RenderTimings>,
    ) -> Result<Surface, Error> {
        let clip = clip.unwrap_or(bounds);
        if !clip.is_finite() || !clip.is_positive() || !bounds.contains_rect(clip) {
            return Err(Error::unsupported("invalid prepared-frame draw clip"));
        }
        // Reuse the complete prepared frame at its original raster origin.
        // Only the draw clip and padded target extent change between patches.
        let bounds = Rect::from_min_max(bounds.min, clip.max);
        let origin = [bounds.min.x as u32, bounds.min.y as u32];
        let width = bounds.width() as u32;
        let height = bounds.height() as u32;
        let clip = [
            clip.min.x - bounds.min.x,
            clip.min.y - bounds.min.y,
            clip.max.x - bounds.min.x,
            clip.max.y - bounds.min.y,
        ];
        let descriptor = wgpu::TextureDescriptor {
            label: Some("festerm immutable Direct2D frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        };
        let mut pointer = std::ptr::null_mut();
        // The SDK owns a fresh committed resource, clears/draws its full extent,
        // releases it to ALL_SHADER_RESOURCE, and flushes on this graphics queue.
        let native_draw_started = timings.as_ref().map(|_| Instant::now());
        let code = unsafe {
            festerm_d2d_draw(
                self.native.as_ptr(),
                width,
                height,
                clip.as_ptr(),
                &mut pointer,
            )
        };
        self.checked("native drawing", code)?;
        if let (Some(timings), Some(started)) = (timings, native_draw_started) {
            timings.native_draw = started.elapsed();
        }
        let pointer =
            NonNull::new(pointer).ok_or_else(|| Error::unsupported("native surface missing"))?;
        // Transfer the owned COM reference into wgpu without transferring a
        // suballocator allocation. The descriptor matches native creation.
        // The initialized state prevents wgpu from clearing the imported pixels.
        let texture = unsafe {
            let resource = Interface::from_raw(pointer.as_ptr());
            let native = wgpu::hal::dx12::Device::texture_from_raw(
                resource,
                descriptor.format,
                descriptor.dimension,
                descriptor.size,
                1,
                1,
            );
            self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                native,
                &descriptor,
                wgpu::TextureUses::RESOURCE,
            )
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.transition_resources(
            std::iter::empty(),
            [wgpu::TextureTransition {
                texture: &texture,
                selector: None,
                state: wgpu::TextureUses::RESOURCE,
            }]
            .into_iter(),
        );
        self.checked_submit(encoder.finish())?;
        Ok(Surface {
            texture,
            rect: Rect::from_min_max(bounds.min / pixels_per_point, bounds.max / pixels_per_point),
            origin,
        })
    }

    fn checked_submit(&self, commands: wgpu::CommandBuffer) -> Result<(), Error> {
        // Validate the submission without synchronously waiting for rasterization.
        let submitted = self.queue.submit([commands]);
        match self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: Some(std::time::Duration::ZERO),
        }) {
            Ok(_) | Err(wgpu::PollError::Timeout) => Ok(()),
            Err(error) => Err(Error::unsupported(&format!(
                "wgpu rejected native submission: {error}"
            ))),
        }
    }
}

const REGION_HEIGHT: u32 = 64;

struct Region {
    rect: Rect,
    primitives: Vec<ClippedPrimitive>,
}

struct CachedFrame {
    rect: Rect,
    scale: f32,
    background: Color32,
    textures: HashMap<u64, Arc<ColorImage>>,
    regions: Vec<Region>,
    surface: Surface,
}

/// One immutable result; unchanged pixels may be copied from the preceding frame.
pub struct CachedSurface {
    pub surface: Surface,
    pub updated_regions: usize,
    /// Pixels replaced in the result, excluding padding in temporary raster images.
    pub updated_pixels: u64,
}

/// Retains only the last frame's exact presentation snapshots and immutable pixels.
pub struct CachedRenderer {
    renderer: Renderer,
    previous: Option<CachedFrame>,
}

impl CachedRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, Error> {
        Ok(Self {
            renderer: Renderer::new(device, queue)?,
            previous: None,
        })
    }

    pub fn render(
        &mut self,
        rect: Rect,
        pixels_per_point: f32,
        background: Color32,
        primitives: &[ClippedPrimitive],
        textures: &[(TextureId, Arc<ColorImage>)],
        timings: Option<&mut RenderTimings>,
    ) -> Result<Option<CachedSurface>, Error> {
        let mut timings = timings;
        // Validate the complete original frame, including unused vertices and
        // aggregate palette/texture/geometry budgets, before considering reuse.
        let Some(bounds) = self.renderer.prepare(
            rect,
            pixels_per_point,
            background,
            primitives,
            textures,
            timings.as_deref_mut(),
        )?
        else {
            self.previous = None;
            return Ok(None);
        };
        let current_textures = self.renderer.textures.clone();
        let regions = if current_textures.contains_key(&0) {
            partition_regions(bounds, pixels_per_point, primitives)
        } else {
            None
        };
        let Some(regions) = regions else {
            self.previous = None;
            return self
                .renderer
                .draw_prepared(bounds, pixels_per_point, None, timings)
                .map(|surface| {
                    Some(CachedSurface {
                        updated_pixels: u64::from(surface.texture.width())
                            * u64::from(surface.texture.height()),
                        updated_regions: 1,
                        surface,
                    })
                });
        };
        let compatible = self.previous.as_ref().filter(|previous| {
            previous.rect == rect
                && previous.scale == pixels_per_point
                && previous.background == background
                && previous.surface.origin == [bounds.min.x as u32, bounds.min.y as u32]
                && previous.surface.texture.width() == bounds.width() as u32
                && previous.surface.texture.height() == bounds.height() as u32
                && previous.textures == current_textures
                && previous.regions.len() == regions.len()
        });
        let changed: Vec<_> = regions
            .iter()
            .enumerate()
            .filter_map(|(index, region)| {
                match compatible {
                    Some(previous) => {
                        region_damage(&previous.regions[index], region, pixels_per_point)
                    }
                    None => Some(region.rect),
                }
                .map(|damage| (index, damage))
            })
            .collect();
        if changed.is_empty() {
            return Ok(Some(CachedSurface {
                surface: compatible
                    .expect("unchanged compatible frame")
                    .surface
                    .clone(),
                updated_regions: 0,
                updated_pixels: 0,
            }));
        }
        let total_pixels = u64::from(bounds.width() as u32) * u64::from(bounds.height() as u32);
        let patches: Vec<_> = changed
            .chunk_by(|left, right| right.0 == left.0 + 1)
            .map(|adjacent| {
                let damage = adjacent
                    .iter()
                    .fold(Rect::NOTHING, |rect, (_, damage)| rect.union(*damage));
                pixel_rect(damage, pixels_per_point).intersect(bounds)
            })
            .collect();
        let patch_fits = patches
            .iter()
            .map(|rect| {
                let size = rect.max - bounds.min;
                size.x as u64 * size.y as u64
            })
            .sum::<u64>()
            <= total_pixels;
        // A full redraw is cheaper than copying a frame and replacing most of it.
        let (surface, updated_regions, updated_pixels) = match compatible
            .filter(|_| changed.len() * 2 < regions.len() && patch_fits)
        {
            None => (
                self.renderer.draw_prepared(
                    bounds,
                    pixels_per_point,
                    None,
                    timings.as_deref_mut(),
                )?,
                regions.len(),
                total_pixels,
            ),
            Some(previous) => {
                let previous = &previous.surface;
                let texture = self
                    .renderer
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("festerm immutable retained terminal frame"),
                        size: previous.texture.size(),
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Bgra8Unorm,
                        usage: wgpu::TextureUsages::COPY_SRC
                            | wgpu::TextureUsages::COPY_DST
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    });
                let mut encoder = self
                    .renderer
                    .device
                    .create_command_encoder(&Default::default());
                encoder.copy_texture_to_texture(
                    previous.texture.as_image_copy(),
                    texture.as_image_copy(),
                    texture.size(),
                );
                let mut updated_pixels = 0;
                for damage in patches {
                    let mut region_timings = timings.as_ref().map(|_| RenderTimings::default());
                    let surface = self.renderer.draw_prepared(
                        bounds,
                        pixels_per_point,
                        Some(damage),
                        region_timings.as_mut(),
                    )?;
                    if let (Some(total), Some(region)) = (timings.as_deref_mut(), region_timings) {
                        total.native_draw += region.native_draw;
                    }
                    let extent = wgpu::Extent3d {
                        width: damage.width() as u32,
                        height: damage.height() as u32,
                        depth_or_array_layers: 1,
                    };
                    updated_pixels += u64::from(extent.width) * u64::from(extent.height);
                    let mut source = surface.texture.as_image_copy();
                    source.origin = wgpu::Origin3d {
                        x: damage.min.x as u32 - surface.origin[0],
                        y: damage.min.y as u32 - surface.origin[1],
                        z: 0,
                    };
                    let mut destination = texture.as_image_copy();
                    destination.origin = wgpu::Origin3d {
                        x: damage.min.x as u32 - previous.origin[0],
                        y: damage.min.y as u32 - previous.origin[1],
                        z: 0,
                    };
                    encoder.copy_texture_to_texture(source, destination, extent);
                }
                self.renderer.checked_submit(encoder.finish())?;
                (
                    Surface {
                        texture,
                        rect: previous.rect,
                        origin: previous.origin,
                    },
                    changed.len(),
                    updated_pixels,
                )
            }
        };
        self.previous = Some(CachedFrame {
            rect,
            scale: pixels_per_point,
            background,
            textures: current_textures,
            regions,
            surface: surface.clone(),
        });
        Ok(Some(CachedSurface {
            surface,
            updated_regions,
            updated_pixels,
        }))
    }
}

fn region_damage(left: &Region, right: &Region, scale: f32) -> Option<Rect> {
    if left.rect != right.rect || left.primitives.len() != right.primitives.len() {
        return Some(right.rect);
    }
    let mut damage = Rect::NOTHING;
    for (old_primitive, new_primitive) in left.primitives.iter().zip(&right.primitives) {
        let (Primitive::Mesh(left_mesh), Primitive::Mesh(right_mesh)) =
            (&old_primitive.primitive, &new_primitive.primitive)
        else {
            return Some(right.rect);
        };
        if old_primitive.clip_rect != new_primitive.clip_rect
            || left_mesh.texture_id != right_mesh.texture_id
        {
            return Some(right.rect);
        }
        // Partitioned meshes contain sequential, explicit triangles. Keep whole
        // triangles at both ends so a changed corner cannot hide old coverage.
        let old = left_mesh.vertices.as_chunks::<3>().0;
        let new = right_mesh.vertices.as_chunks::<3>().0;
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let mut changed = Rect::NOTHING;
        for vertex in old[prefix..old.len() - suffix]
            .iter()
            .chain(&new[prefix..new.len() - suffix])
            .flatten()
        {
            changed.extend_with(vertex.pos * scale);
        }
        if changed.is_positive() {
            // Include raster rounding and edge coverage before clipping to the
            // original primitive. Removed geometry must be cleared too.
            let changed = Rect::from_min_max(
                changed.min.floor() - egui::vec2(1.0, 1.0),
                changed.max.ceil() + egui::vec2(1.0, 1.0),
            )
            .intersect(pixel_rect(new_primitive.clip_rect, scale));
            if changed.is_positive() {
                damage = damage.union(changed);
            }
        }
    }
    let damage = damage.intersect(pixel_rect(right.rect, scale));
    damage
        .is_positive()
        .then(|| Rect::from_min_max(damage.min / scale, damage.max / scale))
}

fn partition_regions(
    bounds: Rect,
    scale: f32,
    primitives: &[ClippedPrimitive],
) -> Option<Vec<Region>> {
    let count = (bounds.height() as u32).div_ceil(REGION_HEIGHT) as usize;
    let mut regions: Vec<_> = (0..count)
        .map(|index| {
            let top = bounds.min.y + index as f32 * REGION_HEIGHT as f32;
            Region {
                rect: Rect::from_min_max(
                    Pos2::new(bounds.min.x, top) / scale,
                    Pos2::new(bounds.max.x, (top + REGION_HEIGHT as f32).min(bounds.max.y)) / scale,
                ),
                primitives: Vec::new(),
            }
        })
        .collect();
    let mut meshes: Vec<Option<egui::Mesh>> = vec![None; count];
    let mut vertex_count = count * 4;
    let mut primitive_count = count;
    for primitive in primitives {
        let Primitive::Mesh(mesh) = &primitive.primitive else {
            return None;
        };
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let vertices = [
                mesh.vertices[triangle[0] as usize],
                mesh.vertices[triangle[1] as usize],
                mesh.vertices[triangle[2] as usize],
            ];
            let mut visible = Rect::NOTHING;
            for vertex in vertices {
                visible.extend_with(vertex.pos * scale);
            }
            visible = visible
                .intersect(pixel_rect(primitive.clip_rect, scale))
                .intersect(bounds);
            if !visible.is_positive() {
                continue;
            }
            let first = ((visible.min.y - bounds.min.y) as u32 / REGION_HEIGHT) as usize;
            let last = (((visible.max.y - bounds.min.y).ceil() as u32).saturating_sub(1)
                / REGION_HEIGHT) as usize;
            for mesh in meshes.iter_mut().take(last + 1).skip(first) {
                let mesh = mesh.get_or_insert_with(|| egui::Mesh {
                    texture_id: match &primitive.primitive {
                        Primitive::Mesh(mesh) => mesh.texture_id,
                        _ => unreachable!(),
                    },
                    ..Default::default()
                });
                vertex_count += 3;
                if vertex_count > MAX_FRAME_VERTICES || mesh.vertices.len() + 3 > MAX_MESH_VERTICES
                {
                    return None;
                }
                let start = mesh.vertices.len() as u32;
                mesh.vertices.extend(vertices);
                mesh.indices.extend([start, start + 1, start + 2]);
            }
        }
        for (region, mesh) in regions.iter_mut().zip(&mut meshes) {
            if let Some(mesh) = mesh.take() {
                primitive_count += 1;
                if primitive_count > MAX_PRIMITIVES {
                    return None;
                }
                region.primitives.push(ClippedPrimitive {
                    clip_rect: primitive.clip_rect.intersect(region.rect),
                    primitive: Primitive::Mesh(mesh),
                });
            }
        }
    }
    Some(regions)
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The bridge owns all COM references. Published textures are owned by
        // wgpu and their recorded last submission follows the native flush.
        unsafe { festerm_d2d_destroy(self.native.as_ptr()) };
    }
}

struct FrameAnalysis {
    bounds: Option<Rect>,
    vertex_count: usize,
    index_count: usize,
}

fn analyze_frame(
    rect: Rect,
    scale: f32,
    primitives: &[ClippedPrimitive],
    used_texture_ids: &mut Vec<u64>,
) -> Result<FrameAnalysis, Error> {
    if primitives.len() > MAX_PRIMITIVES {
        return Err(Error::unsupported("too many paint primitives"));
    }
    let canvas = pixel_rect(rect, scale);
    if !canvas.is_finite() || canvas.min.x < 0.0 || canvas.min.y < 0.0 {
        return Err(Error::unsupported("invalid canvas bounds"));
    }

    let mut bounds = Rect::NOTHING;
    let mut vertex_count = 0usize;
    let mut index_count = 0usize;
    used_texture_ids.clear();

    for primitive in primitives {
        if [
            primitive.clip_rect.min.x,
            primitive.clip_rect.min.y,
            primitive.clip_rect.max.x,
            primitive.clip_rect.max.y,
        ]
        .into_iter()
        .any(f32::is_nan)
        {
            return Err(Error::unsupported("invalid primitive clip"));
        }
        let Primitive::Mesh(mesh) = &primitive.primitive else {
            return Err(Error::unsupported("nested paint callback"));
        };
        let TextureId::Managed(id) = mesh.texture_id else {
            return Err(Error::unsupported("external texture"));
        };
        vertex_count = vertex_count.saturating_add(mesh.vertices.len());
        index_count = index_count.saturating_add(mesh.indices.len());
        if vertex_count > MAX_FRAME_VERTICES || index_count > MAX_FRAME_INDICES {
            return Err(Error::unsupported("native frame geometry budget exceeded"));
        }
        used_texture_ids.push(id);
        let clipped = visible_mesh_bounds(mesh, primitive.clip_rect, scale, canvas)?;
        if clipped.is_positive() {
            bounds = bounds.union(clipped);
        }
    }
    used_texture_ids.sort_unstable();
    used_texture_ids.dedup();

    Ok(FrameAnalysis {
        bounds: bounds
            .is_positive()
            .then(|| Rect::from_min_max(bounds.min.floor(), bounds.max.ceil()).intersect(canvas)),
        vertex_count,
        index_count,
    })
}

fn pixel_rect(rect: Rect, scale: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new((rect.min.x * scale).round(), (rect.min.y * scale).round()),
        Pos2::new((rect.max.x * scale).round(), (rect.max.y * scale).round()),
    )
}

fn visible_mesh_bounds(
    mesh: &egui::Mesh,
    clip_rect: Rect,
    scale: f32,
    canvas: Rect,
) -> Result<Rect, Error> {
    let mut mesh_bounds = Rect::NOTHING;
    for index in &mesh.indices {
        let vertex = mesh
            .vertices
            .get(*index as usize)
            .ok_or_else(|| Error::unsupported("invalid mesh index"))?;
        let point = vertex.pos * scale;
        if !point.is_finite() {
            return Err(Error::unsupported("nonfinite vertex"));
        }
        mesh_bounds.extend_with(point);
    }
    Ok(mesh_bounds
        .intersect(pixel_rect(clip_rect, scale))
        .intersect(canvas))
}

fn raster_position(value: f32) -> f32 {
    const SCALE: f32 = (1 << 8) as f32;
    (value * SCALE).round() / SCALE
}

fn raster_relative_position(value: f32, origin: f32) -> f32 {
    // Quantize before changing origins so damage boundaries cannot change
    // half-subpixel rounding on triangles that cross the boundary.
    raster_position(value) - origin
}

fn trim_vec_capacity<T>(values: &mut Vec<T>, retained_capacity: usize) {
    if values.capacity() > retained_capacity {
        values.shrink_to(retained_capacity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::wgpu::{create_render_state, default_wgpu_setup};
    use std::time::Duration;

    fn frame(color: Color32) -> Vec<ClippedPrimitive> {
        let mut mesh = egui::Mesh::default();
        mesh.add_colored_rect(
            Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(26.0, 36.0)),
            color,
        );
        vec![ClippedPrimitive {
            clip_rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(64.0, 64.0)),
            primitive: Primitive::Mesh(mesh),
        }]
    }

    #[test]
    fn native_bounds_crop_sparse_paints_and_validate_indices() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(64.0, 64.0));
        let mut used = Vec::new();
        assert_eq!(
            analyze_frame(canvas, 1.25, &frame(Color32::RED), &mut used)
                .unwrap()
                .bounds
                .unwrap(),
            Rect::from_min_max(egui::pos2(12.0, 25.0), egui::pos2(33.0, 45.0))
        );
        assert_eq!(used, vec![0]);
        let mut invalid = frame(Color32::RED);
        let Primitive::Mesh(mesh) = &mut invalid[0].primitive else {
            unreachable!()
        };
        mesh.indices.push(999);
        assert!(analyze_frame(canvas, 1.0, &invalid, &mut used).is_err());
    }

    #[test]
    fn native_analysis_deduplicates_textures_and_rejects_large_frames() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(64.0, 64.0));
        let mut used = Vec::new();
        let mut meshes = frame(Color32::RED);
        meshes.extend(frame(Color32::BLUE));
        assert_eq!(
            analyze_frame(canvas, 1.0, &meshes, &mut used)
                .unwrap()
                .vertex_count,
            8
        );
        assert_eq!(used, vec![0]);

        let too_many = vec![meshes[0].clone(); MAX_PRIMITIVES + 1];
        assert!(analyze_frame(canvas, 1.0, &too_many, &mut used).is_err());
    }

    #[test]
    fn raster_grid_normalization_matches_native_rounding() {
        assert_eq!(raster_position(f32::from_bits(0x44a4_4001)), 1314.0);
        let aligned = f32::from_bits(0x446e_ed00);
        assert_eq!(raster_position(aligned), aligned);
    }

    #[test]
    fn raster_grid_normalization_is_independent_of_damage_origin() {
        for coordinate in [64.0 - 1.0 / 512.0, 64.0 + 1.0 / 512.0] {
            let expected = raster_relative_position(coordinate, 0.0);
            for origin in [0.0, 64.0, 128.0] {
                assert_eq!(
                    raster_relative_position(coordinate, origin) + origin,
                    expected
                );
            }
        }
    }

    #[test]
    fn narrow_damage_includes_removed_triangles_and_falls_back_for_clip_changes() {
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(256.0, 512.0));
        let mut before = frame(Color32::RED);
        let Primitive::Mesh(mesh) = &mut before[0].primitive else {
            unreachable!()
        };
        mesh.add_colored_rect(
            Rect::from_min_max(egui::pos2(40.0, 20.0), egui::pos2(56.0, 36.0)),
            Color32::GREEN,
        );
        let old = partition_regions(canvas, 1.0, &before).unwrap();
        let after = frame(Color32::RED);
        let new = partition_regions(canvas, 1.0, &after).unwrap();
        assert_eq!(region_damage(&old[0], &old[0], 1.0), None);
        let removed = region_damage(&old[0], &new[0], 1.0).unwrap();
        assert_eq!(
            removed,
            Rect::from_min_max(egui::pos2(39.0, 19.0), egui::pos2(57.0, 37.0))
        );
        assert_eq!(region_damage(&new[0], &old[0], 1.0), Some(removed));
        let mut corner = after;
        let Primitive::Mesh(mesh) = &mut corner[0].primitive else {
            unreachable!()
        };
        mesh.vertices[0].color = Color32::BLUE;
        let corner = partition_regions(canvas, 1.0, &corner).unwrap();
        assert_eq!(
            region_damage(&new[0], &corner[0], 1.0),
            Some(Rect::from_min_max(
                egui::pos2(9.0, 19.0),
                egui::pos2(27.0, 37.0)
            )),
            "unchanged corners of a changed triangle still contribute pixels"
        );
        let mut clipped = before.clone();
        clipped[0].clip_rect.max.x = 20.0;
        let clipped = partition_regions(canvas, 1.0, &clipped).unwrap();
        assert_eq!(
            region_damage(&old[0], &clipped[0], 1.0),
            Some(old[0].rect),
            "clip changes must erase old pixels outside the new clip"
        );
        let mut different_texture = before;
        let Primitive::Mesh(mesh) = &mut different_texture[0].primitive else {
            unreachable!()
        };
        mesh.texture_id = TextureId::Managed(1);
        let different_texture = partition_regions(canvas, 1.0, &different_texture).unwrap();
        assert_eq!(
            region_damage(&old[0], &different_texture[0], 1.0),
            Some(old[0].rect)
        );
    }

    fn first_pixel(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> [u8; 4] {
        pixel_at(device, queue, texture, wgpu::Origin3d::ZERO)
    }

    fn pixel_at(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        origin: wgpu::Origin3d,
    ) -> [u8; 4] {
        read_pixels(device, queue, texture, origin, [1, 1])
            .try_into()
            .unwrap()
    }

    fn read_pixels(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        origin: wgpu::Origin3d,
        size: [u32; 2],
    ) -> Vec<u8> {
        let stride = (size[0] * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(stride) * u64::from(size[1]),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut source = texture.as_image_copy();
        source.origin = origin;
        encoder.copy_texture_to_buffer(
            source,
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(size[1]),
                },
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                sender.send(result).unwrap();
            });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(30)),
            })
            .unwrap();
        receiver
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .unwrap();
        let mapped = buffer.slice(..).get_mapped_range().unwrap();
        let result = mapped
            .chunks_exact(stride as usize)
            .flat_map(|row| row[..size[0] as usize * 4].iter().copied())
            .collect();
        drop(mapped);
        buffer.unmap();
        result
    }

    #[test]
    fn shared_surfaces_preserve_pixels_and_previous_frame_ownership() {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let mut renderer = Renderer::new(state.device.clone(), state.queue.clone()).unwrap();
        let textures = vec![(
            TextureId::Managed(0),
            Arc::new(ColorImage::new([1, 1], vec![Color32::WHITE])),
        )];
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(64.0, 64.0));
        let red = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &frame(Color32::RED),
                &textures,
                None,
            )
            .unwrap()
            .unwrap();
        let blue = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &frame(Color32::BLUE),
                &textures,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(red.texture.width(), 16);
        assert_eq!(
            first_pixel(&state.device, &state.queue, &red.texture),
            [0, 0, 255, 255]
        );
        assert_eq!(
            first_pixel(&state.device, &state.queue, &blue.texture),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn narrow_retained_updates_match_full_pixels_with_overlap_erasure_and_dpi() {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let pixels = |surface: &Surface| {
            read_pixels(
                &state.device,
                &state.queue,
                &surface.texture,
                wgpu::Origin3d::ZERO,
                [surface.texture.width(), surface.texture.height()],
            )
        };
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let mut cached =
                CachedRenderer::new(state.device.clone(), state.queue.clone()).unwrap();
            let mut reference = Renderer::new(state.device.clone(), state.queue.clone()).unwrap();
            let base = Rect::from_min_size(egui::pos2(11.25, 9.75), egui::vec2(384.0, 512.0));
            let background = Color32::from_rgb(20, 30, 40);
            let mut textures = vec![
                (
                    TextureId::Managed(0),
                    Arc::new(ColorImage::new(
                        [8, 8],
                        (0..64)
                            .map(|index| Color32::from_white_alpha((255 - index * 29 % 256) as u8))
                            .collect(),
                    )),
                ),
                (
                    TextureId::Managed(1),
                    Arc::new(ColorImage::new(
                        [2, 2],
                        vec![Color32::RED, Color32::GREEN, Color32::BLUE, Color32::WHITE],
                    )),
                ),
            ];
            let mut original = None;
            for step in 0..8 {
                let mut canvas = base;
                if step == 7 {
                    canvas.max.x += 8.0;
                }
                let mut fill = egui::Mesh::default();
                fill.add_colored_rect(canvas, background);
                let mut glyphs = egui::Mesh::default();
                if step != 3 {
                    let offset = if step == 0 {
                        egui::vec2(80.25, 185.75)
                    } else {
                        egui::vec2(98.75, 202.25)
                    };
                    glyphs.add_rect_with_uv(
                        Rect::from_min_size(canvas.min + offset, egui::vec2(32.0, 26.0)),
                        Rect::from_min_max(
                            egui::pos2(if step >= 2 { 0.25 } else { 0.125 }, 0.125),
                            egui::pos2(0.875, 0.875),
                        ),
                        if step == 0 {
                            Color32::WHITE
                        } else {
                            Color32::from_rgb(200, 230, 160)
                        },
                    );
                }
                glyphs.add_rect_with_uv(
                    Rect::from_min_size(
                        canvas.min + egui::vec2(280.0, 185.75),
                        egui::vec2(38.0, 60.0),
                    ),
                    Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                let mut color = egui::Mesh::with_texture(TextureId::Managed(1));
                color.add_rect_with_uv(
                    Rect::from_min_size(
                        canvas.min + egui::vec2(250.5, 340.5),
                        egui::vec2(24.0, 24.0),
                    ),
                    Rect::from_min_max(
                        egui::pos2(if step >= 4 { 0.25 } else { 0.0 }, 0.0),
                        egui::pos2(1.0, 1.0),
                    ),
                    Color32::from_white_alpha(180),
                );
                let mut overlay = egui::Mesh::default();
                overlay.add_colored_rect(
                    Rect::from_min_size(
                        canvas.min + egui::vec2(90.0, 194.0),
                        egui::vec2(80.0, 40.0),
                    ),
                    Color32::from_rgba_unmultiplied(80, 170, 220, 80),
                );
                let start = overlay.vertices.len() as u32;
                let feathered_color = Color32::from_rgba_unmultiplied(160, 80, 200, 120);
                overlay.vertices.extend(
                    [
                        (egui::vec2(75.5, 180.25), Color32::TRANSPARENT),
                        (egui::vec2(170.75, 185.5), feathered_color),
                        (egui::vec2(115.25, 250.25), feathered_color),
                    ]
                    .map(|(offset, color)| egui::epaint::Vertex {
                        pos: canvas.min + offset,
                        uv: egui::epaint::WHITE_UV,
                        color,
                    }),
                );
                overlay.indices.extend([start, start + 1, start + 2]);
                let mut primitives: Vec<_> = [fill, glyphs, color, overlay]
                    .into_iter()
                    .map(|mesh| ClippedPrimitive {
                        clip_rect: canvas,
                        primitive: Primitive::Mesh(mesh),
                    })
                    .collect();
                if step == 5 {
                    primitives[1].clip_rect.max.x = canvas.left() + 105.0;
                }
                if step == 6 {
                    textures[1].1 = Arc::new(ColorImage::new([2, 2], vec![Color32::RED; 4]));
                }
                let expected = reference
                    .render(canvas, scale, background, &primitives, &textures, None)
                    .unwrap()
                    .unwrap();
                let actual = cached
                    .render(canvas, scale, background, &primitives, &textures, None)
                    .unwrap()
                    .unwrap();
                assert_eq!(actual.surface.origin, expected.origin);
                assert_eq!(actual.surface.texture.size(), expected.texture.size());
                let expected_pixels = pixels(&expected);
                let actual_pixels = pixels(&actual.surface);
                let different = actual_pixels
                    .iter()
                    .zip(&expected_pixels)
                    .position(|(a, b)| a != b);
                let first_difference = different.map(|offset| {
                    let pixel = offset / 4 * 4;
                    (
                        offset / 4 % expected.texture.width() as usize,
                        offset / 4 / expected.texture.width() as usize,
                        &actual_pixels[pixel..pixel + 4],
                        &expected_pixels[pixel..pixel + 4],
                    )
                });
                assert!(
                    different.is_none(),
                    "scale={scale}, step={step}, first different pixel={first_difference:?}"
                );
                let full_pixels =
                    u64::from(expected.texture.width()) * u64::from(expected.texture.height());
                if step == 1 {
                    assert!(
                        actual.updated_pixels > 0 && actual.updated_pixels < full_pixels / 32,
                        "localized glyph changes must not redraw full-width strips"
                    );
                }
                if step >= 6 {
                    assert_eq!(actual.updated_pixels, full_pixels);
                }
                if step == 0 {
                    original = Some((actual.surface, actual_pixels));
                }
            }
            let (original, expected) = original.unwrap();
            assert!(
                pixels(&original) == expected,
                "published pixels must stay immutable at scale={scale}"
            );
        }
    }

    #[test]
    fn scattered_retained_damage_prepares_full_geometry_only_once() {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let textures = [(
            TextureId::Managed(0),
            Arc::new(ColorImage::new(
                [8, 8],
                (0..64)
                    .map(|index| Color32::from_white_alpha((255 - index * 29 % 256) as u8))
                    .collect(),
            )),
        )];
        for (height, changed_regions, x) in [(1024, 4, 248.0), (4096, 8, 248.0), (4096, 31, 1.0)] {
            let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(256.0, height as f32));
            let frame = |changed| {
                let mut mesh = egui::Mesh::default();
                mesh.add_colored_rect(canvas, Color32::BLACK);
                for region in 0..height / REGION_HEIGHT {
                    mesh.add_rect_with_uv(
                        Rect::from_min_size(
                            egui::pos2(x, region as f32 * REGION_HEIGHT as f32 + 8.5),
                            egui::vec2(8.0, 16.0),
                        ),
                        Rect::from_min_max(egui::pos2(0.125, 0.125), egui::pos2(0.875, 0.875)),
                        if changed && region % 2 == 0 && region / 2 < changed_regions {
                            Color32::BLUE
                        } else {
                            Color32::RED
                        },
                    );
                }
                vec![ClippedPrimitive {
                    clip_rect: canvas,
                    primitive: Primitive::Mesh(mesh),
                }]
            };
            let pixels = |surface: &Surface| {
                read_pixels(
                    &state.device,
                    &state.queue,
                    &surface.texture,
                    wgpu::Origin3d::ZERO,
                    [surface.texture.width(), surface.texture.height()],
                )
            };
            let mut cached =
                CachedRenderer::new(state.device.clone(), state.queue.clone()).unwrap();
            let before = cached
                .render(canvas, 1.0, Color32::BLACK, &frame(false), &textures, None)
                .unwrap()
                .unwrap();
            let original = pixels(&before.surface);
            let mut timings = RenderTimings::default();
            let after = cached
                .render(
                    canvas,
                    1.0,
                    Color32::BLACK,
                    &frame(true),
                    &textures,
                    Some(&mut timings),
                )
                .unwrap()
                .unwrap();
            assert_eq!(after.updated_regions, changed_regions as usize);
            assert!(after.updated_pixels > 0 && after.updated_pixels < before.updated_pixels / 16);
            assert_eq!(
                timings.geometry_prepare_calls, 1,
                "height={height}, separated patches={changed_regions}, x={x}"
            );
            assert_eq!(
                timings.vertex_count,
                (1 + height / REGION_HEIGHT) as usize * 4
            );
            assert_eq!(
                timings.index_count,
                (1 + height / REGION_HEIGHT) as usize * 6
            );
            let mut reference = Renderer::new(state.device.clone(), state.queue.clone()).unwrap();
            let expected = reference
                .render(canvas, 1.0, Color32::BLACK, &frame(true), &textures, None)
                .unwrap()
                .unwrap();
            assert!(pixels(&after.surface) == pixels(&expected));
            assert!(pixels(&before.surface) == original);
        }
    }

    #[test]
    fn retained_geometry_and_raster_budgets_preserve_valid_frames() {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let mut renderer = CachedRenderer::new(state.device.clone(), state.queue.clone()).unwrap();
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(256.0, 512.0));
        let textures = [(
            TextureId::Managed(0),
            Arc::new(ColorImage::new([1, 1], vec![Color32::WHITE])),
        )];
        let mut background = egui::Mesh::default();
        background.add_colored_rect(canvas, Color32::BLACK);
        let mut primitives = frame(Color32::RED);
        primitives.push(ClippedPrimitive {
            clip_rect: canvas,
            primitive: Primitive::Mesh(background),
        });
        primitives.swap(0, 1);
        primitives.resize_with(MAX_PRIMITIVES, || ClippedPrimitive {
            clip_rect: canvas,
            primitive: Primitive::Mesh(egui::Mesh::default()),
        });
        let before = renderer
            .render(canvas, 1.0, Color32::BLACK, &primitives, &textures, None)
            .unwrap()
            .unwrap();
        let Primitive::Mesh(mesh) = &mut primitives[1].primitive else {
            unreachable!()
        };
        for vertex in &mut mesh.vertices {
            vertex.color = Color32::BLUE;
        }
        let mut timings = RenderTimings::default();
        let after = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &primitives,
                &textures,
                Some(&mut timings),
            )
            .unwrap()
            .unwrap();
        assert!(after.updated_pixels > 0 && after.updated_pixels < before.updated_pixels);
        assert_eq!(after.updated_regions, 1);
        assert_eq!(timings.geometry_prepare_calls, 1);
        assert_eq!(
            pixel_at(
                &state.device,
                &state.queue,
                &after.surface.texture,
                wgpu::Origin3d { x: 15, y: 25, z: 0 },
            ),
            [255, 0, 0, 255]
        );

        let mut separated = vec![primitives[0].clone()];
        let mut glyphs = egui::Mesh::default();
        for y in [200.0, 328.0, 456.0] {
            glyphs.add_colored_rect(
                Rect::from_min_size(egui::pos2(240.0, y), egui::vec2(8.0, 16.0)),
                Color32::RED,
            );
        }
        separated.push(ClippedPrimitive {
            clip_rect: canvas,
            primitive: Primitive::Mesh(glyphs),
        });
        let before = renderer
            .render(canvas, 1.0, Color32::BLACK, &separated, &textures, None)
            .unwrap()
            .unwrap();
        let Primitive::Mesh(mesh) = &mut separated[1].primitive else {
            unreachable!()
        };
        for vertex in &mut mesh.vertices {
            vertex.color = Color32::BLUE;
        }
        let after = renderer
            .render(canvas, 1.0, Color32::BLACK, &separated, &textures, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            after.updated_pixels, before.updated_pixels,
            "separated patches must not allocate more raster pixels than a full frame"
        );
        assert_eq!(after.updated_regions, before.updated_regions);
        assert_eq!(
            pixel_at(
                &state.device,
                &state.queue,
                &after.surface.texture,
                wgpu::Origin3d {
                    x: 244,
                    y: 464,
                    z: 0
                },
            ),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn retained_frames_update_only_changed_regions_and_preserve_older_pixels() {
        let mut setup = default_wgpu_setup();
        let egui_wgpu::WgpuSetup::CreateNew(options) = &mut setup else {
            unreachable!()
        };
        options.instance_descriptor.backends = wgpu::Backends::DX12;
        let state = create_render_state(setup, Default::default());
        let mut renderer = CachedRenderer::new(state.device.clone(), state.queue.clone()).unwrap();
        let canvas = Rect::from_min_size(Pos2::ZERO, egui::vec2(256.0, 512.0));
        let textures = vec![(
            TextureId::Managed(0),
            Arc::new(ColorImage::new([1, 1], vec![Color32::WHITE])),
        )];
        let scene = |changed: Option<Color32>| {
            let mut mesh = egui::Mesh::default();
            for row in 0..8 {
                let color = if row == 3 {
                    let Some(color) = changed else { continue };
                    color
                } else {
                    Color32::BLUE
                };
                mesh.add_colored_rect(
                    Rect::from_min_max(
                        egui::pos2(8.0, row as f32 * 64.0 + 8.0),
                        egui::pos2(248.0, row as f32 * 64.0 + 56.0),
                    ),
                    color,
                );
            }
            vec![ClippedPrimitive {
                clip_rect: canvas,
                primitive: Primitive::Mesh(mesh),
            }]
        };
        let original = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &scene(Some(Color32::BLUE)),
                &textures,
                None,
            )
            .unwrap()
            .unwrap();
        let unchanged = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &scene(Some(Color32::BLUE)),
                &textures,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.updated_regions, 0);
        assert_eq!(unchanged.updated_pixels, 0);
        let changed = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &scene(Some(Color32::GREEN)),
                &textures,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(changed.updated_regions, 1);
        assert!(changed.updated_pixels < original.updated_pixels / 4);
        let erased = renderer
            .render(canvas, 1.0, Color32::BLACK, &scene(None), &textures, None)
            .unwrap()
            .unwrap();
        assert_eq!(erased.updated_regions, 1);
        let at = wgpu::Origin3d { x: 1, y: 193, z: 0 };
        for (frame, expected) in [
            (&original, [255, 0, 0, 255]),
            (&unchanged, [255, 0, 0, 255]),
            (&changed, [0, 255, 0, 255]),
            (&erased, [0, 0, 0, 255]),
        ] {
            assert_eq!(
                pixel_at(&state.device, &state.queue, &frame.surface.texture, at),
                expected
            );
        }
        let mut invalid = scene(None);
        let Primitive::Mesh(mesh) = &mut invalid[0].primitive else {
            unreachable!()
        };
        let mut unused = mesh.vertices[0];
        unused.uv.x = -1.0;
        mesh.vertices.push(unused);
        assert!(renderer
            .render(canvas, 1.0, Color32::BLACK, &invalid, &textures, None)
            .is_err());

        let different_atlas = vec![(
            TextureId::Managed(0),
            Arc::new(ColorImage::new([2, 1], vec![Color32::WHITE; 2])),
        )];
        let replaced = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &scene(None),
                &different_atlas,
                None,
            )
            .unwrap()
            .unwrap();
        assert!(
            replaced.updated_regions > 1,
            "texture changes must invalidate retained regions"
        );
        let recolored = renderer
            .render(
                canvas,
                1.0,
                Color32::RED,
                &scene(None),
                &different_atlas,
                None,
            )
            .unwrap()
            .unwrap();
        assert!(recolored.updated_regions > 1);
        assert_eq!(
            pixel_at(&state.device, &state.queue, &recolored.surface.texture, at),
            [0, 0, 255, 255],
        );
        let scaled = renderer
            .render(
                canvas,
                1.25,
                Color32::RED,
                &scene(None),
                &different_atlas,
                None,
            )
            .unwrap()
            .unwrap();
        assert!(scaled.updated_regions > 1);
        let mut clipped = scene(None);
        clipped[0].clip_rect.max.x = 100.0;
        let narrowed = renderer
            .render(canvas, 1.25, Color32::RED, &clipped, &different_atlas, None)
            .unwrap()
            .unwrap();
        assert!(narrowed.updated_regions > 1);
        assert!(narrowed.surface.texture.width() < scaled.surface.texture.width());
        assert!(renderer
            .render(canvas, 1.0, Color32::BLACK, &[], &[], None)
            .unwrap()
            .is_none());
        assert!(
            renderer.previous.is_none(),
            "empty frames must release retained state"
        );
        clipped[0].clip_rect.min.x = f32::NAN;
        assert!(renderer
            .render(canvas, 1.0, Color32::BLACK, &clipped, &textures, None)
            .is_err());
    }
}
