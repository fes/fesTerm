use std::{
    collections::{HashMap, HashSet},
    ffi::{c_char, c_void, CStr},
    fmt,
    ptr::NonNull,
    sync::Arc,
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
        meshes: *const MeshInput,
        count: usize,
    ) -> i32;
    fn festerm_d2d_draw(bridge: *mut c_void, output: *mut *mut c_void) -> i32;
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

/// Native drawing is queued before subsequent wgpu use; the texture is never
/// reused for another frame.
pub struct Surface {
    pub texture: wgpu::Texture,
    pub rect: Rect,
    pub origin: [u32; 2],
}

/// Serial ownership of a multithread-capable Direct2D/D3D11-on-12 context.
pub struct Renderer {
    native: NonNull<c_void>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    textures: HashMap<u64, Arc<ColorImage>>,
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
    ) -> Result<Option<Surface>, Error> {
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || background.a() != 255 {
            return Err(Error::unsupported(
                "finite scale and opaque background required",
            ));
        }
        if primitives.len() > 250_000 {
            return Err(Error::unsupported("too many paint primitives"));
        }
        let mut vertices = 0usize;
        let mut indices = 0usize;
        for primitive in primitives {
            if let Primitive::Mesh(mesh) = &primitive.primitive {
                vertices = vertices.saturating_add(mesh.vertices.len());
                indices = indices.saturating_add(mesh.indices.len());
                if vertices > 2_000_000 || indices > 6_000_000 {
                    return Err(Error::unsupported("native frame geometry budget exceeded"));
                }
            }
        }
        let Some(bounds) = visible_bounds(rect, pixels_per_point, primitives)? else {
            return Ok(None);
        };
        let origin = [bounds.min.x as u32, bounds.min.y as u32];
        let width = bounds.width() as u32;
        let height = bounds.height() as u32;
        if width > 4096 || height > 4096 || primitives.len() > 250_000 {
            return Err(Error::unsupported("native frame exceeds supported bounds"));
        }
        let used = primitives
            .iter()
            .map(|primitive| match &primitive.primitive {
                Primitive::Mesh(mesh) => match mesh.texture_id {
                    TextureId::Managed(id) => Ok(id),
                    TextureId::User(_) => Err(Error::unsupported("external texture")),
                },
                Primitive::Callback(_) => Err(Error::unsupported("nested paint callback")),
            })
            .collect::<Result<HashSet<_>, _>>()?;
        let mut texture_bytes = 0usize;
        for &id in &used {
            let image = textures
                .iter()
                .find_map(|(texture_id, image)| {
                    (*texture_id == TextureId::Managed(id)).then_some(image)
                })
                .ok_or_else(|| Error::unsupported("captured texture pixels missing"))?;
            texture_bytes = texture_bytes.saturating_add(image.pixels.len().saturating_mul(4));
            if texture_bytes > 96 * 1024 * 1024 {
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
                || w > 8192
                || h > 8192
                || w * h > 16_777_216
                || image.pixels.len() != w * h
            {
                return Err(Error::unsupported("invalid texture dimensions"));
            }
            let pixels = image
                .pixels
                .iter()
                .map(|pixel| Color(pixel.to_array()))
                .collect::<Vec<_>>();
            // Pixel storage remains alive for the synchronous SDK copy.
            let code = unsafe {
                festerm_d2d_texture(
                    self.native.as_ptr(),
                    id,
                    w as u32,
                    h as u32,
                    pixels.as_ptr(),
                    pixels.len(),
                )
            };
            self.checked("texture upload", code)?;
            self.textures.insert(id, image.clone());
        }
        let retained = used.iter().copied().collect::<Vec<_>>();
        // The SDK copies the IDs before returning; no Rust storage is retained.
        let code =
            unsafe { festerm_d2d_prune(self.native.as_ptr(), retained.as_ptr(), retained.len()) };
        self.checked("texture retirement", code)?;
        self.textures.retain(|id, _| used.contains(id));

        let mut storage = Vec::with_capacity(primitives.len());
        let mut inputs = Vec::with_capacity(primitives.len());
        for primitive in primitives {
            let Primitive::Mesh(mesh) = &primitive.primitive else {
                unreachable!()
            };
            let TextureId::Managed(id) = mesh.texture_id else {
                unreachable!()
            };
            if mesh.vertices.len() > 1_000_000 || mesh.indices.len() > 3_000_000 {
                return Err(Error::unsupported("mesh exceeds supported bounds"));
            }
            storage.push(
                mesh.vertices
                    .iter()
                    .map(|vertex| Vertex {
                        position: [
                            vertex.pos.x * pixels_per_point - bounds.min.x,
                            vertex.pos.y * pixels_per_point - bounds.min.y,
                        ],
                        uv: [vertex.uv.x, vertex.uv.y],
                        color: Color(vertex.color.to_array()),
                    })
                    .collect::<Vec<_>>(),
            );
            let vertices = storage.last().expect("just inserted vertices");
            let clip = pixel_rect(primitive.clip_rect, pixels_per_point).intersect(bounds);
            inputs.push(MeshInput {
                texture: id,
                vertices: vertices.as_ptr(),
                vertex_count: vertices.len(),
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
        let code = unsafe {
            festerm_d2d_prepare(
                self.native.as_ptr(),
                width,
                height,
                Color(background.to_array()),
                inputs.as_ptr(),
                inputs.len(),
            )
        };
        self.checked("geometry preparation", code)?;

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
        let code = unsafe { festerm_d2d_draw(self.native.as_ptr(), &mut pointer) };
        self.checked("native drawing", code)?;
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
        // Record use after the native flush. A zero-time wait validates the
        // submission index without waiting for rasterization to complete.
        let submitted = self.queue.submit([encoder.finish()]);
        match self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: Some(std::time::Duration::ZERO),
        }) {
            Ok(_) | Err(wgpu::PollError::Timeout) => {}
            Err(error) => {
                return Err(Error::unsupported(&format!(
                    "wgpu rejected native submission: {error}"
                )))
            }
        }
        Ok(Some(Surface {
            texture,
            rect: Rect::from_min_max(bounds.min / pixels_per_point, bounds.max / pixels_per_point),
            origin,
        }))
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // The bridge owns all COM references. Published textures are owned by
        // wgpu and their recorded last submission follows the native flush.
        unsafe { festerm_d2d_destroy(self.native.as_ptr()) };
    }
}

fn pixel_rect(rect: Rect, scale: f32) -> Rect {
    Rect::from_min_max(
        Pos2::new((rect.min.x * scale).round(), (rect.min.y * scale).round()),
        Pos2::new((rect.max.x * scale).round(), (rect.max.y * scale).round()),
    )
}

fn visible_bounds(
    rect: Rect,
    scale: f32,
    primitives: &[ClippedPrimitive],
) -> Result<Option<Rect>, Error> {
    let canvas = pixel_rect(rect, scale);
    if !canvas.is_finite() || canvas.min.x < 0.0 || canvas.min.y < 0.0 {
        return Err(Error::unsupported("invalid canvas bounds"));
    }
    let mut bounds = Rect::NOTHING;
    for primitive in primitives {
        let Primitive::Mesh(mesh) = &primitive.primitive else {
            return Err(Error::unsupported("nested paint callback"));
        };
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
        let clipped = mesh_bounds
            .intersect(pixel_rect(primitive.clip_rect, scale))
            .intersect(canvas);
        if clipped.is_positive() {
            bounds = bounds.union(clipped);
        }
    }
    if !bounds.is_positive() {
        return Ok(None);
    }
    Ok(Some(
        Rect::from_min_max(bounds.min.floor(), bounds.max.ceil()).intersect(canvas),
    ))
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
        assert_eq!(
            visible_bounds(canvas, 1.25, &frame(Color32::RED))
                .unwrap()
                .unwrap(),
            Rect::from_min_max(egui::pos2(12.0, 25.0), egui::pos2(33.0, 45.0))
        );
        let mut invalid = frame(Color32::RED);
        let Primitive::Mesh(mesh) = &mut invalid[0].primitive else {
            unreachable!()
        };
        mesh.indices.push(999);
        assert!(visible_bounds(canvas, 1.0, &invalid).is_err());
    }

    fn first_pixel(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> [u8; 4] {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
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
        let result = mapped[..4].try_into().unwrap();
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
            .render(canvas, 1.0, Color32::BLACK, &frame(Color32::RED), &textures)
            .unwrap()
            .unwrap();
        let blue = renderer
            .render(
                canvas,
                1.0,
                Color32::BLACK,
                &frame(Color32::BLUE),
                &textures,
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
}
