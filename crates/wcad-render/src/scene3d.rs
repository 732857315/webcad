//! CPU-side 3D display lists: triangle meshes and screen-space-width line sets, plus their per-instance
//! styles and the plane grid description.

use std::ops::Range;

use wcad_math::{BBox3, DMat4, DVec3, Plane};

use crate::camera::Ray3;
use crate::{Rgba, pack_rgba};

/// Triangle mesh in local coordinates (placed by [`MeshStyle::transform`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals. If the length differs from `positions`, smooth normals are computed on upload.
    pub normals: Vec<[f32; 3]>,
    /// Triangle list.
    pub indices: Vec<u32>,
}

impl MeshData {
    /// Local bounds of all vertices.
    pub fn bbox(&self) -> BBox3 {
        BBox3::from_points(
            self.positions.iter().filter(|p| p.iter().all(|c| c.is_finite())).map(|p| DVec3::from(p.map(f64::from))),
        )
    }

    /// Smooth vertex normals from the triangles, weighted by the corner angle (independent of how
    /// the faces are triangulated).
    pub fn compute_normals(&self) -> Vec<[f32; 3]> {
        let mut acc = vec![DVec3::ZERO; self.positions.len()];
        let n = self.positions.len();
        let p = |i: u32| DVec3::from(self.positions[i as usize].map(f64::from));
        for t in self.indices.as_chunks::<3>().0 {
            if t.iter().any(|&i| i as usize >= n) {
                continue;
            }
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let Some(fnrm) = (b - a).cross(c - a).try_normalize() else { continue };
            for (i, prev, next) in [(t[0], c, b), (t[1], a, c), (t[2], b, a)] {
                let corner = p(i);
                let angle = (prev - corner).angle_between(next - corner);
                if angle.is_finite() {
                    acc[i as usize] += fnrm * angle;
                }
            }
        }
        acc.into_iter().map(|v| v.normalize_or(DVec3::Z).as_vec3().to_array()).collect()
    }

    /// Nearest triangle hit by `ray` with `t >= t_min`: `(triangle index, t)`. `transform` places the mesh.
    pub fn pick(&self, ray: &Ray3, transform: &DMat4, t_min: f64) -> Option<(usize, f64)> {
        let n = self.positions.len();
        let world: Vec<DVec3> =
            self.positions.iter().map(|p| transform.transform_point3(DVec3::from(p.map(f64::from)))).collect();
        let mut best: Option<(usize, f64)> = None;
        for (ti, t) in self.indices.as_chunks::<3>().0.iter().enumerate() {
            if t.iter().any(|&i| i as usize >= n) {
                continue;
            }
            if let Some(hit) = ray.intersect_triangle(world[t[0] as usize], world[t[1] as usize], world[t[2] as usize])
                && hit >= t_min
                && best.is_none_or(|(_, b)| hit < b)
            {
                best = Some((ti, hit));
            }
        }
        best
    }
}

/// Per-instance style of an uploaded mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshStyle {
    /// Local → world placement (`f64`; combined with the view matrix in `f64` before the `f32` cast).
    pub transform: DMat4,
    /// Base color. Alpha < 1 draws the mesh transparent (after the opaque ones, without depth writes).
    pub color: Rgba,
    /// Index ranges (into `MeshData::indices`, rounded to whole triangles) drawn in another color, e.g.
    /// a hovered or selected face. The highlight color's alpha is the blend factor over the base color.
    pub highlight_ranges: Vec<(Range<u32>, Rgba)>,
    pub visible: bool,
}

impl Default for MeshStyle {
    fn default() -> Self {
        Self { transform: DMat4::IDENTITY, color: [0.72, 0.74, 0.78, 1.0], highlight_ranges: Vec::new(), visible: true }
    }
}

/// One 3D segment with screen-space width.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineSegment3D {
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub color_a: [u8; 4],
    pub color_b: [u8; 4],
}

/// A set of 3D segments in local coordinates (placed by [`LineStyle3D::transform`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LineSet3D {
    pub segments: Vec<LineSegment3D>,
}

impl LineSet3D {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn push_segment(&mut self, a: [f32; 3], b: [f32; 3], color: Rgba) {
        let c = pack_rgba(color);
        self.segments.push(LineSegment3D { a, b, color_a: c, color_b: c });
    }

    /// Adds a segment from `f64` points (cast to `f32`; keep them near the local origin).
    pub fn push_segment_f64(&mut self, a: DVec3, b: DVec3, color: Rgba) {
        if a.is_finite() && b.is_finite() {
            self.push_segment(a.as_vec3().to_array(), b.as_vec3().to_array(), color);
        }
    }

    /// Adds consecutive segments (e.g. a tessellated B-rep edge). Returns the segment index range.
    pub fn push_polyline(&mut self, pts: &[[f32; 3]], color: Rgba) -> Range<u32> {
        let start = self.segments.len() as u32;
        for w in pts.windows(2) {
            self.push_segment(w[0], w[1], color);
        }
        start..self.segments.len() as u32
    }

    /// Adds consecutive segments from `f64` points. Returns the segment index range.
    pub fn push_polyline_f64(&mut self, pts: &[DVec3], color: Rgba) -> Range<u32> {
        let start = self.segments.len() as u32;
        for w in pts.windows(2) {
            self.push_segment_f64(w[0], w[1], color);
        }
        start..self.segments.len() as u32
    }

    /// Adds a polyline given in 2D coordinates of `plane` (sketch curves), converted to world
    /// coordinates (use an identity transform, or subtract a local origin first for huge coordinates).
    pub fn push_plane_polyline(&mut self, plane: &Plane, pts: &[wcad_math::DVec2], color: Rgba) -> Range<u32> {
        let world: Vec<DVec3> = pts.iter().map(|&p| plane.to_world(p)).collect();
        self.push_polyline_f64(&world, color)
    }

    /// Local bounds of all segment endpoints.
    pub fn bbox(&self) -> BBox3 {
        let f = |p: &[f32; 3]| DVec3::from(p.map(f64::from));
        BBox3::from_points(self.segments.iter().flat_map(|s| [f(&s.a), f(&s.b)]).filter(|p| p.is_finite()))
    }
}

/// Per-instance style of an uploaded 3D line set.
#[derive(Clone, Debug, PartialEq)]
pub struct LineStyle3D {
    pub transform: DMat4,
    /// Width in pixels (before `Frame3D::pixel_scale`).
    pub width_px: f32,
    /// Replaces the per-segment colors when set.
    pub color: Option<Rgba>,
    /// Segment index ranges drawn in another color (hovered / selected edges).
    pub highlight_ranges: Vec<(Range<u32>, Rgba)>,
    /// Draw without depth test, after everything else (axes, previews).
    pub on_top: bool,
    /// Pull towards the eye as a fraction of the view distance, so edges win against their own faces.
    pub depth_bias: f32,
    pub visible: bool,
}

impl Default for LineStyle3D {
    fn default() -> Self {
        Self {
            transform: DMat4::IDENTITY,
            width_px: 1.5,
            color: None,
            highlight_ranges: Vec::new(),
            on_top: false,
            depth_bias: 2e-4,
            visible: true,
        }
    }
}

/// An anti-aliased grid drawn on a plane (in the fragment shader).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid3D {
    /// Grid plane; lines run along `x_axis`/`y_axis` through multiples of `spacing` from `origin`.
    pub plane: Plane,
    /// Minor line spacing in world units.
    pub spacing: f64,
    /// Every n-th line is a major line (0 or 1 = no major lines).
    pub major_every: u32,
    /// Half-size of the drawn square around the camera target's projection; the grid fades out towards it.
    pub radius: f64,
    pub minor_color: Rgba,
    pub major_color: Rgba,
    /// Colors of the plane's X and Y axis lines (alpha 0 hides them).
    pub x_axis_color: Rgba,
    pub y_axis_color: Rgba,
    /// Line width in pixels (before `Frame3D::pixel_scale`).
    pub line_width_px: f32,
}

impl Default for Grid3D {
    fn default() -> Self {
        Self {
            plane: Plane::XY,
            spacing: 10.0,
            major_every: 10,
            radius: 1000.0,
            minor_color: [0.55, 0.58, 0.63, 0.35],
            major_color: [0.45, 0.48, 0.53, 0.7],
            x_axis_color: [0.86, 0.26, 0.26, 0.9],
            y_axis_color: [0.30, 0.72, 0.30, 0.9],
            line_width_px: 1.0,
        }
    }
}

/// Clamps highlight ranges to `count` whole primitives (`unit` indices each), drops empty ones and sorts
/// them by start; overlapping ranges are trimmed so each index is drawn at most once.
pub(crate) fn normalize_ranges(ranges: &[(Range<u32>, Rgba)], count: u32, unit: u32) -> Vec<(Range<u32>, Rgba)> {
    let unit = unit.max(1);
    let mut out: Vec<(Range<u32>, Rgba)> = ranges
        .iter()
        .filter(|(r, _)| r.start < r.end)
        .filter_map(|(r, c)| {
            let s = (r.start / unit * unit).min(count);
            let e = (r.end.div_ceil(unit).saturating_mul(unit)).min(count);
            (s < e).then_some((s..e, *c))
        })
        .collect();
    out.sort_by_key(|(r, _)| r.start);
    let mut cursor = 0u32;
    out.retain_mut(|(r, _)| {
        r.start = r.start.max(cursor);
        if r.start >= r.end {
            return false;
        }
        cursor = r.end;
        true
    });
    out
}
