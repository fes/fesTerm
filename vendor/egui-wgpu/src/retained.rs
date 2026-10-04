use std::{mem::size_of, sync::Arc};

use epaint::{ClippedPrimitive, Mesh, Primitive, emath::Rect};

use crate::{Callback, Renderer, ScreenDescriptor};

const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024 * 1024;

/// Exact immutable paint inputs within one renderer-defined identity namespace.
///
/// Equal keys promise identical paint for the same screen, viewport and clip.
/// All other inputs, including pipeline/resource identity, must be covered.
/// Paint must have no side effects; prepare and finish_prepare still run.
#[derive(Clone, Debug)]
pub struct CallbackPaintKey {
    pub namespace: Arc<()>,
    pub bytes: Arc<[u8]>,
}

impl PartialEq for CallbackPaintKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.namespace, &other.namespace) && self.bytes == other.bytes
    }
}

impl Eq for CallbackPaintKey {}

#[derive(PartialEq)]
enum PaintSignature {
    Mesh(Mesh),
    Callback { rect: Rect, key: CallbackPaintKey },
}

#[derive(PartialEq)]
struct JobSignature {
    clip_rect: Rect,
    paint: PaintSignature,
}

struct Signature {
    size: [u32; 2],
    scale: f32,
    format: wgpu::TextureFormat,
    clear: [f32; 4],
    textures: Arc<()>,
    jobs: Vec<JobSignature>,
    bytes: usize,
}

fn fits_size([width, height]: [u32; 2]) -> bool {
    width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

fn add_bytes(bytes: &mut usize, count: usize, stride: usize) -> Result<(), &'static str> {
    *bytes = count
        .checked_mul(stride)
        .and_then(|added| bytes.checked_add(added))
        .filter(|total| *total <= MAX_SIGNATURE_BYTES)
        .ok_or("paint signature exceeds retention budget")?;
    Ok(())
}

impl Signature {
    fn capture(
        renderer: &Renderer,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        format: wgpu::TextureFormat,
        clear: [f32; 4],
    ) -> Result<Self, &'static str> {
        if !fits_size(screen.size_in_pixels) || !clear.iter().all(|value| value.is_finite()) {
            return Err("target size or clear color is ineligible");
        }
        let textures = renderer
            .retained_texture_epoch()
            .ok_or("managed texture ownership is not exclusive")?;
        let mut bytes = size_of::<Self>();
        add_bytes(&mut bytes, jobs.len(), size_of::<JobSignature>())?;
        let mut signatures = Vec::with_capacity(jobs.len());
        for job in jobs {
            if !job.clip_rect.is_finite() {
                return Err("nonfinite paint clip");
            }
            let paint = match &job.primitive {
                Primitive::Mesh(mesh) => {
                    if !renderer.retains_texture(mesh.texture_id) {
                        return Err("mesh does not use a renderer-owned managed texture");
                    }
                    add_bytes(&mut bytes, mesh.vertices.len(), size_of::<epaint::Vertex>())?;
                    add_bytes(&mut bytes, mesh.indices.len(), size_of::<u32>())?;
                    PaintSignature::Mesh(mesh.clone())
                }
                Primitive::Callback(callback) => {
                    if !callback.rect.is_finite() {
                        return Err("nonfinite callback viewport");
                    }
                    let key = callback
                        .callback
                        .downcast_ref::<Callback>()
                        .and_then(|callback| callback.0.paint_key())
                        .ok_or("callback has no immutable paint key")?;
                    add_bytes(&mut bytes, key.bytes.len(), 1)?;
                    PaintSignature::Callback {
                        rect: callback.rect,
                        key,
                    }
                }
            };
            signatures.push(JobSignature {
                clip_rect: job.clip_rect,
                paint,
            });
        }
        Ok(Self {
            size: screen.size_in_pixels,
            scale: screen.pixels_per_point,
            format,
            clear,
            textures: Arc::clone(textures),
            jobs: signatures,
            bytes,
        })
    }

    fn matches(&self, other: &Self) -> bool {
        self.size == other.size
            && self.scale == other.scale
            && self.format == other.format
            && self.clear == other.clear
            && Arc::ptr_eq(&self.textures, &other.textures)
            && self.jobs == other.jobs
    }
}

struct CachedPrefix {
    signature: Signature,
    texture: wgpu::Texture,
}

/// Cumulative outcomes and current cache-owned storage, excluding GPU in-flight work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedUiStats {
    pub reused_frames: u64,
    pub rebuilt_frames: u64,
    pub texture_bytes: u64,
    pub signature_bytes: usize,
    pub decline_reason: Option<&'static str>,
}

/// One bounded, immutable image of the UI before an eligible final-copy callback.
///
/// This never retains terminal pixels or relies on swap-chain contents. Hosts
/// must discard it on surface/lifecycle changes and restrict its use to their
/// eligible viewport. Managed texture writes must use the renderer's APIs.
#[derive(Default)]
pub struct RetainedUi {
    cached: Option<CachedPrefix>,
    reused_frames: u64,
    rebuilt_frames: u64,
    decline_reason: Option<&'static str>,
}

impl RetainedUi {
    /// Discard retained pixels and signatures, keeping cumulative diagnostics.
    pub fn clear(&mut self) {
        self.cached = None;
        self.decline_reason = None;
    }

    pub fn stats(&self) -> RetainedUiStats {
        RetainedUiStats {
            reused_frames: self.reused_frames,
            rebuilt_frames: self.rebuilt_frames,
            texture_bytes: self.cached.as_ref().map_or(0, |cached| {
                u64::from(cached.texture.width()) * u64::from(cached.texture.height()) * 4
            }),
            signature_bytes: self
                .cached
                .as_ref()
                .map_or(0, |cached| cached.signature.bytes),
            decline_reason: self.decline_reason,
        }
    }

    /// Paint/copy only the prefix. The caller must then encode the final callback copy.
    ///
    /// Call after normal texture updates and `Renderer::update_buffers`. `Some`
    /// means the prefix is initialized in `target`; the value is true for reuse,
    /// false for a rebuild. `None` requires the existing ordinary paint path.
    /// This records into the caller's encoder, without submitting or waiting.
    #[allow(clippy::too_many_arguments)]
    pub fn try_render(
        &mut self,
        renderer: &Renderer,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
    ) -> Option<bool> {
        self.decline_reason = None;
        if !renderer.retained_composition_enabled {
            self.clear();
            return None;
        }
        if renderer.final_callback_copy(jobs, screen, target).is_none() {
            self.clear();
            self.decline_reason = Some("no eligible final image copy");
            log::debug!(target: "egui_wgpu::retained_ui",
                "retained prefix declined: no eligible final image copy");
            return None;
        }
        let prefix = &jobs[..jobs.len() - 1];
        let signature = match Signature::capture(renderer, prefix, screen, target.format(), clear) {
            Ok(signature) => signature,
            Err(reason) => {
                self.clear();
                self.decline_reason = Some(reason);
                log::debug!(target: "egui_wgpu::retained_ui", "retained prefix declined: {reason}");
                return None;
            }
        };
        let reused = self
            .cached
            .as_ref()
            .is_some_and(|cached| cached.signature.matches(&signature));
        if reused {
            self.reused_frames = self.reused_frames.saturating_add(1);
        } else {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("egui immutable retained prefix"),
                size: target.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: target.format(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui retained prefix rebuild"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color {
                                    r: f64::from(clear[0]),
                                    g: f64::from(clear[1]),
                                    b: f64::from(clear[2]),
                                    a: f64::from(clear[3]),
                                }),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        ..Default::default()
                    })
                    .forget_lifetime();
                renderer.render(&mut pass, prefix, screen);
            }
            self.cached = Some(CachedPrefix { signature, texture });
            self.rebuilt_frames = self.rebuilt_frames.saturating_add(1);
        }
        let cached = self.cached.as_ref().expect("retained prefix initialized");
        encoder.copy_texture_to_texture(
            cached.texture.as_image_copy(),
            target.as_image_copy(),
            target.size(),
        );
        Some(reused)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_prefix_budgets_are_inclusive_and_overflow_safe() {
        assert!(fits_size([4096, 4096]));
        assert!(!fits_size([4096, 4097]));
        assert!(!fits_size([0, 4096]));
        assert!(!fits_size([u32::MAX, u32::MAX]));
        let mut bytes = MAX_SIGNATURE_BYTES - 4;
        assert!(add_bytes(&mut bytes, 1, 4).is_ok());
        assert_eq!(bytes, MAX_SIGNATURE_BYTES);
        assert!(add_bytes(&mut bytes, 1, 1).is_err());
        assert!(add_bytes(&mut 0, usize::MAX, 2).is_err());
    }

    #[test]
    fn callback_paint_keys_require_namespace_identity_and_exact_bytes() {
        let key = CallbackPaintKey {
            namespace: Arc::new(()),
            bytes: Arc::from([1, 2, 3]),
        };
        assert_eq!(key, key.clone());
        assert_ne!(
            key,
            CallbackPaintKey {
                namespace: Arc::new(()),
                bytes: key.bytes.clone(),
            }
        );
        assert_ne!(
            key,
            CallbackPaintKey {
                namespace: key.namespace.clone(),
                bytes: Arc::from([1, 2, 4]),
            }
        );
    }
}
