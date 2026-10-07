//! Windows SDK interop isolated from application and terminal semantics.
//!
//! Each rendered surface is immutable after publication. A later frame never
//! mutates a texture that a caller may still reference in a command buffer.

mod texture_limits;
pub use texture_limits::texture_dimensions_supported;

#[cfg(all(windows, target_arch = "x86_64"))]
mod renderer;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use renderer::{
    process_cpu_time, CachedRenderer, CachedSurface, Error, RenderTimings, Renderer, Surface,
};
