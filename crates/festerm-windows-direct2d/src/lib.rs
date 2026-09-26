//! Windows SDK interop isolated from application and terminal semantics.
//!
//! Each rendered surface is immutable after publication. A later frame never
//! mutates a texture that a caller may still reference in a command buffer.

#[cfg(all(windows, target_arch = "x86_64"))]
mod renderer;
#[cfg(all(windows, target_arch = "x86_64"))]
pub use renderer::{Error, Renderer, Surface};
