//! GPU renderer. Knows nothing about documents: it draws display lists built by the app.
//! See `docs/ARCHITECTURE.md` §5.7.
//!
//! # Model
//!
//! The renderer is *retained*: the app uploads display lists once ([`Renderer::upload_batch2d`],
//! [`Renderer::upload_mesh`], [`Renderer::upload_lines3d`]) and re-uploads only what changed. Every frame
//! that actually needs a new image (see [`Renderer::needs_render_2d`] / [`Renderer::needs_render_3d`])
//! is drawn into an offscreen [`Viewport`] (color `Rgba8Unorm` + depth + optional MSAA). The app shows the
//! viewport's [`Viewport::color_view`] as an egui native texture; egui repaints never re-render the scene.
//!
//! # Use from egui (sketch)
//!
//! ```ignore
//! // once
//! let mut renderer = wcad_render::Renderer::new(&rs.device, &rs.queue);
//! let samples = wcad_render::choose_sample_count(&rs.adapter, 4);
//! let mut vp = wcad_render::Viewport::new(&rs.device, size_px, samples);
//! let tex = rs.renderer.write().register_native_texture(&rs.device, vp.color_view(), wgpu::FilterMode::Linear);
//! // every egui frame
//! if vp.resize(&rs.device, size_px) {
//!     rs.renderer.write().update_egui_texture_from_wgpu_texture(&rs.device, vp.color_view(),
//!         wgpu::FilterMode::Linear, tex);
//! }
//! if renderer.needs_render_2d(&vp, &camera, &frame) {
//!     let mut enc = rs.device.create_command_encoder(&Default::default());
//!     renderer.render_2d(&rs.device, &rs.queue, &mut enc, &vp, &camera, &frame);
//!     rs.queue.submit([enc.finish()]);
//! }
//! ui.image((tex, rect.size()));
//! ```
//!
//! # Conventions
//!
//! - Colors ([`Rgba`]) are **sRGB-encoded, straight (non-premultiplied) alpha** in `0..=1`: the values you
//!   would type into a color picker. The output texture is gamma-space (egui target formats are
//!   non-sRGB), so 2D colors are written as-is and 3D lighting is done in linear space and re-encoded.
//! - All pixel quantities (line widths, point sizes, viewport sizes, screen coordinates) are **physical
//!   pixels of the viewport texture** with the origin at the top-left corner and Y down. Multiply egui
//!   points by `pixels_per_point`, or set `pixel_scale` in [`Frame2D`]/[`Frame3D`] for widths.
//! - 2D world: right-handed, Y up. 3D world: right-handed, **Z up**.
//! - Geometry is uploaded as `f32` relative to an `f64` origin/transform; the renderer combines the
//!   origin with the camera in `f64`, so coordinates like `1e6` stay precise on screen.

mod batch2d;
mod camera;
mod renderer;
mod scene3d;
mod viewport;

pub use batch2d::{Batch2D, FillVertex, LineSegment2D, PointMarker, PointShape};
pub use camera::{Camera2D, Camera3D, Ray3, StandardView};
pub use renderer::{Batch2DStyle, BatchId, Frame2D, Frame3D, LineSetId, MeshId, Renderer};
pub use scene3d::{Grid3D, LineSegment3D, LineSet3D, LineStyle3D, MeshData, MeshStyle};
pub use viewport::{COLOR_FORMAT, DEPTH_FORMAT, Viewport, choose_sample_count};

pub use wcad_math::{BBox2, BBox3, DMat4, DVec2, DVec3, Plane};

/// sRGB-encoded RGBA color with straight alpha, each channel in `0..=1`.
pub type Rgba = [f32; 4];

/// Builds an [`Rgba`] from 8-bit sRGB channels.
#[inline]
pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Rgba {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}

/// Opaque [`Rgba`] from 8-bit sRGB channels.
#[inline]
pub fn rgb8(r: u8, g: u8, b: u8) -> Rgba {
    rgba8(r, g, b, 255)
}

/// Quantizes an [`Rgba`] to the 8-bit form stored in GPU vertex data (clamped, NaN → 0).
#[inline]
pub(crate) fn pack_rgba(c: Rgba) -> [u8; 4] {
    let q = |v: f32| if v.is_nan() { 0 } else { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
    [q(c[0]), q(c[1]), q(c[2]), q(c[3])]
}

/// WGSL helpers shared by every shader module (prepended to each module's source).
pub(crate) const SHADER_COMMON: &str = include_str!("shaders/common.wgsl");
/// 3D per-render uniforms, prepended to the 3D modules.
pub(crate) const SHADER_FRAME3D: &str = include_str!("shaders/frame3d.wgsl");
pub(crate) const SHADER_2D: &str = include_str!("shaders/shader2d.wgsl");
pub(crate) const SHADER_3D: &str = include_str!("shaders/shader3d.wgsl");
pub(crate) const SHADER_GRID: &str = include_str!("shaders/grid3d.wgsl");

/// Concatenates WGSL snippets into one module source.
pub(crate) fn shader_source(parts: &[&str]) -> String {
    let mut s = String::with_capacity(parts.iter().map(|p| p.len() + 1).sum());
    for p in parts {
        s.push_str(p);
        s.push('\n');
    }
    s
}

/// Full sources of the three shader modules: (label, WGSL).
pub(crate) fn shader_modules() -> [(&'static str, String); 3] {
    [
        ("wcad_shader2d", shader_source(&[SHADER_COMMON, SHADER_2D])),
        ("wcad_shader3d", shader_source(&[SHADER_COMMON, SHADER_FRAME3D, SHADER_3D])),
        ("wcad_grid3d", shader_source(&[SHADER_COMMON, SHADER_FRAME3D, SHADER_GRID])),
    ]
}

#[cfg(test)]
mod tests;
