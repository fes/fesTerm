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

impl JobSignature {
    fn matches(&self, job: &ClippedPrimitive, whole_frame: bool) -> bool {
        self.clip_rect == job.clip_rect
            && match (&self.paint, &job.primitive) {
                (PaintSignature::Mesh(saved), Primitive::Mesh(mesh)) => saved == mesh,
                (PaintSignature::Callback { rect, key }, Primitive::Callback(callback)) => {
                    *rect == callback.rect
                        && callback
                            .callback
                            .downcast_ref::<Callback>()
                            .is_some_and(|painter| {
                                (!whole_frame || painter.0.texture_copy().is_none())
                                    && painter.0.paint_key().as_ref() == Some(key)
                            })
                }
                _ => false,
            }
    }
}

struct Signature {
    whole_frame: bool,
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
        whole_frame: bool,
    ) -> Result<Self, &'static str> {
        if !fits_size(screen.size_in_pixels)
            || !screen.pixels_per_point.is_finite()
            || screen.pixels_per_point <= 0.0
            || !clear.iter().all(|value| value.is_finite())
        {
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
                    let painter = callback
                        .callback
                        .downcast_ref::<Callback>()
                        .ok_or("callback has no immutable paint key")?;
                    if whole_frame && painter.0.texture_copy().is_some() {
                        return Err("image-copy callback cannot be retained in a complete frame");
                    }
                    let key = painter
                        .0
                        .paint_key()
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
            whole_frame,
            size: screen.size_in_pixels,
            scale: screen.pixels_per_point,
            format,
            clear,
            textures: Arc::clone(textures),
            jobs: signatures,
            bytes,
        })
    }

    fn matches_inputs(
        &self,
        renderer: &Renderer,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        format: wgpu::TextureFormat,
        clear: [f32; 4],
        whole_frame: bool,
    ) -> bool {
        renderer.retained_texture_epoch().is_some_and(|textures| {
            self.matches_frame(screen, format, clear, whole_frame, textures)
                && self.jobs.len() == jobs.len()
                && self.jobs.iter().zip(jobs).all(|(saved, job)| {
                    if let Primitive::Mesh(mesh) = &job.primitive
                        && !renderer.retains_texture(mesh.texture_id)
                    {
                        return false;
                    }
                    saved.matches(job, whole_frame)
                })
        })
    }

    fn matches_frame(
        &self,
        screen: &ScreenDescriptor,
        format: wgpu::TextureFormat,
        clear: [f32; 4],
        whole_frame: bool,
        textures: &Arc<()>,
    ) -> bool {
        self.whole_frame == whole_frame
            && self.size == screen.size_in_pixels
            && self.scale == screen.pixels_per_point
            && self.format == format
            && self.clear == clear
            && Arc::ptr_eq(&self.textures, textures)
    }
}

struct CachedPrefix {
    signature: Signature,
    texture: wgpu::Texture,
    painted_jobs: usize,
}

enum RetainedPlan {
    Reuse,
    Rebuild {
        signature: Signature,
        painted_jobs: usize,
    },
    ReusePrefix(Signature),
}

/// Cumulative outcomes and current cache-owned storage, excluding GPU in-flight work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedUiStats {
    pub reused_frames: u64,
    pub rebuilt_frames: u64,
    /// Image reuse that still paints a changing complete-frame suffix.
    pub partial_reused_frames: u64,
    pub cached_prefix_jobs: usize,
    pub cached_signature_jobs: usize,
    /// Attempts to materialize an owned signature, including declined candidates.
    pub signature_captures: u64,
    pub texture_bytes: u64,
    pub signature_bytes: usize,
    pub decline_reason: Option<&'static str>,
}

/// One bounded, immutable image of a prefix or an eligible complete frame.
///
/// This never retains terminal pixels or relies on swap-chain contents. Hosts
/// must discard it on surface/lifecycle changes and restrict its use to their
/// eligible viewport. Complete frames reject image-copy callbacks as well as
/// unknown/mutable paint. Managed texture writes must use the renderer's APIs.
#[derive(Default)]
pub struct RetainedUi {
    cached: Option<CachedPrefix>,
    reused_frames: u64,
    rebuilt_frames: u64,
    partial_reused_frames: u64,
    signature_captures: u64,
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
            partial_reused_frames: self.partial_reused_frames,
            cached_prefix_jobs: self.cached.as_ref().map_or(0, |cached| cached.painted_jobs),
            cached_signature_jobs: self
                .cached
                .as_ref()
                .map_or(0, |cached| cached.signature.jobs.len()),
            signature_captures: self.signature_captures,
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
        let plan = self.plan_prefix(renderer, jobs, screen, target, clear)?;
        Some(self.encode_plan(
            renderer,
            device,
            encoder,
            &jobs[..jobs.len() - 1],
            screen,
            target,
            clear,
            plan,
        ))
    }

    fn plan_prefix(
        &mut self,
        renderer: &Renderer,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
    ) -> Option<RetainedPlan> {
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
        self.plan_jobs(renderer, prefix, screen, target, clear, false)
    }

    /// Paint/copy an eligible complete frame, without a final image-copy callback.
    ///
    /// Uses the same cache/budgets as prefix retention, after normal texture and
    /// buffer updates. Hosts must enforce opaque-root surface eligibility.
    #[allow(clippy::too_many_arguments)]
    pub fn try_render_frame(
        &mut self,
        renderer: &Renderer,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
    ) -> Option<bool> {
        let plan = self.plan_frame(renderer, jobs, screen, target, clear)?;
        Some(self.encode_plan(renderer, device, encoder, jobs, screen, target, clear, plan))
    }

    fn plan_frame(
        &mut self,
        renderer: &Renderer,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
    ) -> Option<RetainedPlan> {
        self.decline_reason = None;
        if !renderer.retained_frame_enabled {
            self.clear();
            return None;
        }
        if !renderer.retained_frame_target_eligible(screen, target) {
            self.clear();
            self.decline_reason = Some("ineligible complete-frame target");
            log::debug!(target: "egui_wgpu::retained_ui", "retained frame declined: ineligible target");
            return None;
        }
        self.plan_jobs(renderer, jobs, screen, target, clear, true)
    }

    /// Decide retention before staging meshes, using the same admission and encoding
    /// as [`Self::try_render`] and [`Self::try_render_frame`].
    ///
    /// Call after texture updates and [`Renderer::prepare_without_mesh_uploads`].
    /// `whole_frame` selects complete-frame retention rather than the prefix of an
    /// eligible final image copy. Rebuilds, partial hits and refusals upload all meshes;
    /// `None` therefore leaves the caller ready for ordinary same-frame rendering.
    /// Exact hits still require the caller to encode any final callback copy.
    #[allow(clippy::too_many_arguments)]
    pub fn try_render_with_deferred_mesh_uploads(
        &mut self,
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
        whole_frame: bool,
    ) -> Option<bool> {
        let plan = if whole_frame {
            self.plan_frame(renderer, jobs, screen, target, clear)
        } else {
            self.plan_prefix(renderer, jobs, screen, target, clear)
        };
        if !matches!(plan, Some(RetainedPlan::Reuse)) {
            renderer.update_mesh_buffers(device, queue, jobs);
        }
        plan.map(|plan| {
            let paint_jobs = if whole_frame {
                jobs
            } else {
                &jobs[..jobs.len() - 1]
            };
            self.encode_plan(
                renderer, device, encoder, paint_jobs, screen, target, clear, plan,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_jobs(
        &mut self,
        renderer: &Renderer,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
        whole_frame: bool,
    ) -> Option<RetainedPlan> {
        let complete_hit = self.cached.as_ref().is_some_and(|cached| {
            cached.painted_jobs == jobs.len()
                && cached.signature.matches_inputs(
                    renderer,
                    jobs,
                    screen,
                    target.format(),
                    clear,
                    whole_frame,
                )
        });
        if complete_hit {
            Some(RetainedPlan::Reuse)
        } else {
            self.signature_captures = self.signature_captures.saturating_add(1);
            let signature = match Signature::capture(
                renderer,
                jobs,
                screen,
                target.format(),
                clear,
                whole_frame,
            ) {
                Ok(signature) => signature,
                Err(reason) => {
                    self.clear();
                    self.decline_reason = Some(reason);
                    log::debug!(target: "egui_wgpu::retained_ui", "retained image declined: {reason}");
                    return None;
                }
            };
            let common_prefix = self
                .cached
                .as_ref()
                .filter(|cached| {
                    whole_frame
                        && cached.signature.matches_frame(
                            screen,
                            target.format(),
                            clear,
                            true,
                            &signature.textures,
                        )
                })
                .map_or(0, |cached| {
                    cached
                        .signature
                        .jobs
                        .iter()
                        .zip(&signature.jobs)
                        .take_while(|(saved, current)| saved == current)
                        .count()
                });
            if common_prefix == jobs.len() {
                // Restore complete-frame idle reuse after a moving suffix settles.
                Some(RetainedPlan::Rebuild {
                    signature,
                    painted_jobs: jobs.len(),
                })
            } else if common_prefix > 0
                && self
                    .cached
                    .as_ref()
                    .is_some_and(|cached| cached.painted_jobs <= common_prefix)
            {
                Some(RetainedPlan::ReusePrefix(signature))
            } else {
                Some(RetainedPlan::Rebuild {
                    signature,
                    painted_jobs: if common_prefix == 0 {
                        jobs.len()
                    } else {
                        common_prefix
                    },
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_plan(
        &mut self,
        renderer: &Renderer,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        jobs: &[ClippedPrimitive],
        screen: &ScreenDescriptor,
        target: &wgpu::Texture,
        clear: [f32; 4],
        plan: RetainedPlan,
    ) -> bool {
        let reused = matches!(plan, RetainedPlan::Reuse | RetainedPlan::ReusePrefix(_));
        if let RetainedPlan::Rebuild {
            signature,
            painted_jobs,
        } = plan
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("egui immutable retained image"),
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
                        label: Some("egui retained image rebuild"),
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
                renderer.render_job_range(&mut pass, jobs, screen, 0..painted_jobs);
            }
            self.cached = Some(CachedPrefix {
                signature,
                texture,
                painted_jobs,
            });
            self.rebuilt_frames = self.rebuilt_frames.saturating_add(1);
        } else {
            if let RetainedPlan::ReusePrefix(signature) = plan {
                self.cached
                    .as_mut()
                    .expect("retained prefix initialized")
                    .signature = signature;
                self.partial_reused_frames = self.partial_reused_frames.saturating_add(1);
            }
            self.reused_frames = self.reused_frames.saturating_add(1);
        }
        let cached = self.cached.as_ref().expect("retained image initialized");
        encoder.copy_texture_to_texture(
            cached.texture.as_image_copy(),
            target.as_image_copy(),
            target.size(),
        );
        if cached.painted_jobs < jobs.len() {
            let view = target.create_view(&Default::default());
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui retained prefix changing suffix"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    ..Default::default()
                })
                .forget_lifetime();
            renderer.render_job_range(&mut pass, jobs, screen, cached.painted_jobs..jobs.len());
        }
        reused
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

    #[test]
    fn retained_image_modes_and_renderer_epochs_are_distinct() {
        let mut prefix = Signature {
            whole_frame: false,
            size: [640, 400],
            scale: 1.0,
            format: wgpu::TextureFormat::Bgra8Unorm,
            clear: [0.0; 4],
            textures: Arc::new(()),
            jobs: Vec::new(),
            bytes: size_of::<Signature>(),
        };
        let mut frame = Signature {
            whole_frame: true,
            size: prefix.size,
            scale: prefix.scale,
            format: prefix.format,
            clear: prefix.clear,
            textures: Arc::clone(&prefix.textures),
            jobs: Vec::new(),
            bytes: prefix.bytes,
        };
        let screen = ScreenDescriptor {
            size_in_pixels: frame.size,
            pixels_per_point: frame.scale,
        };
        let matches = |prefix: &Signature, frame: &Signature| {
            prefix.matches_frame(
                &screen,
                frame.format,
                frame.clear,
                frame.whole_frame,
                &frame.textures,
            )
        };
        assert!(!matches(&prefix, &frame));
        prefix.whole_frame = true;
        assert!(matches(&prefix, &frame));
        frame.textures = Arc::new(());
        assert!(!matches(&prefix, &frame));
    }

    #[test]
    fn borrowed_mesh_matches_require_exact_geometry_clip_and_texture() {
        let rect = Rect::from_min_max(epaint::emath::Pos2::ZERO, epaint::emath::pos2(20.0, 20.0));
        let mut mesh = Mesh::with_texture(epaint::TextureId::Managed(7));
        mesh.add_rect_with_uv(rect, rect, epaint::Color32::WHITE);
        let saved = JobSignature {
            clip_rect: rect,
            paint: PaintSignature::Mesh(mesh.clone()),
        };
        let original = ClippedPrimitive {
            clip_rect: rect,
            primitive: Primitive::Mesh(mesh),
        };
        assert!(saved.matches(&original, true));
        for change in 0..6 {
            let mut job = original.clone();
            let Primitive::Mesh(mesh) = &mut job.primitive else {
                unreachable!()
            };
            match change {
                0 => job.clip_rect = rect.shrink(1.0),
                1 => mesh.texture_id = epaint::TextureId::Managed(8),
                2 => mesh.vertices[0].color = epaint::Color32::RED,
                3 => mesh.vertices[0].pos.x += 1.0,
                4 => mesh.vertices[0].uv.x += 1.0,
                5 => mesh.indices.swap(0, 1),
                _ => unreachable!(),
            }
            assert!(!saved.matches(&job, true), "change {change}");
        }
    }

    struct KeyedPaint(Option<CallbackPaintKey>);

    impl crate::CallbackTrait for KeyedPaint {
        fn paint_key(&self) -> Option<CallbackPaintKey> {
            self.0.clone()
        }

        fn paint(
            &self,
            _info: epaint::PaintCallbackInfo,
            _render_pass: &mut wgpu::RenderPass<'static>,
            _callback_resources: &crate::CallbackResources,
        ) {
            unreachable!("signature comparison must not paint")
        }
    }

    #[test]
    fn borrowed_callback_matches_require_exact_key_clip_and_viewport() {
        let rect = Rect::from_min_max(epaint::emath::Pos2::ZERO, epaint::emath::pos2(20.0, 20.0));
        let key = CallbackPaintKey {
            namespace: Arc::new(()),
            bytes: Arc::from([1, 2]),
        };
        let saved = JobSignature {
            clip_rect: rect,
            paint: PaintSignature::Callback {
                rect,
                key: key.clone(),
            },
        };
        let job = |rect, key| ClippedPrimitive {
            clip_rect: rect,
            primitive: Primitive::Callback(Callback::new_paint_callback(rect, KeyedPaint(key))),
        };
        assert!(saved.matches(&job(rect, Some(key.clone())), true));
        assert!(!saved.matches(&job(rect.shrink(1.0), Some(key.clone())), true));
        assert!(!saved.matches(&job(rect, None), true));
        assert!(!saved.matches(
            &job(
                rect,
                Some(CallbackPaintKey {
                    namespace: Arc::clone(&key.namespace),
                    bytes: Arc::from([1, 3]),
                })
            ),
            true
        ));
        assert!(!saved.matches(
            &job(
                rect,
                Some(CallbackPaintKey {
                    namespace: Arc::new(()),
                    bytes: Arc::clone(&key.bytes),
                })
            ),
            true
        ));
        let unknown = ClippedPrimitive {
            clip_rect: rect,
            primitive: Primitive::Callback(epaint::PaintCallback {
                rect,
                callback: Arc::new(()),
            }),
        };
        assert!(!saved.matches(&unknown, true));
    }
}
