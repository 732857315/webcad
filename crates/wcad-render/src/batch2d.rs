//! CPU-side 2D display list: line segments, filled triangles and point markers in `f32` relative to an
//! `f64` origin.

use wcad_math::{BBox2, DVec2};

use crate::{Rgba, pack_rgba};

/// One screen-space-width line segment (drawn as an anti-aliased capsule, so polylines get round joins).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineSegment2D {
    /// Start, relative to the batch origin.
    pub a: [f32; 2],
    /// End, relative to the batch origin.
    pub b: [f32; 2],
    /// 8-bit sRGB color at `a` (straight alpha).
    pub color_a: [u8; 4],
    /// 8-bit sRGB color at `b`.
    pub color_b: [u8; 4],
    /// Width in pixels (before `Frame2D::pixel_scale`). `<= 0` draws a 1-pixel hairline; widths below
    /// one pixel are drawn one pixel wide with reduced opacity.
    pub width: f32,
}

/// Vertex of a filled triangle.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FillVertex {
    /// Position relative to the batch origin.
    pub pos: [f32; 2],
    /// 8-bit sRGB color (straight alpha).
    pub color: [u8; 4],
}

/// Marker shapes for [`PointMarker`].
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PointShape {
    #[default]
    Square = 0,
    SquareOutline = 1,
    /// `+`
    Cross = 2,
    /// `×`
    XCross = 3,
    Circle = 4,
    CircleOutline = 5,
}

/// A screen-space point marker.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PointMarker {
    /// Position relative to the batch origin.
    pub pos: [f32; 2],
    pub color: [u8; 4],
    /// Full marker size in pixels (before `Frame2D::pixel_scale`).
    pub size: f32,
    /// A [`PointShape`] discriminant.
    pub shape: u32,
}

/// A 2D display list. Coordinates are stored in `f32` relative to [`Batch2D::origin`]; choose the origin
/// near the batch's content (e.g. its bounding-box center) and keep batches spatially compact — the
/// `f32` precision is relative to the batch extent, not to the absolute coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch2D {
    pub origin: DVec2,
    pub lines: Vec<LineSegment2D>,
    pub fill_vertices: Vec<FillVertex>,
    /// Triangle list indices into `fill_vertices`.
    pub fill_indices: Vec<u32>,
    pub points: Vec<PointMarker>,
}

impl Batch2D {
    pub fn new(origin: DVec2) -> Self {
        Self {
            origin,
            ..Default::default()
        }
    }

    /// World point → batch-local `f32`.
    #[inline]
    pub fn local(&self, p: DVec2) -> [f32; 2] {
        let d = p - self.origin;
        [d.x as f32, d.y as f32]
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.fill_indices.is_empty() && self.points.is_empty()
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.fill_vertices.clear();
        self.fill_indices.clear();
        self.points.clear();
    }

    /// Adds a segment with one color.
    pub fn push_line(&mut self, a: DVec2, b: DVec2, width_px: f32, color: Rgba) {
        self.push_line_colors(a, b, width_px, color, color);
    }

    /// Adds a segment with a color gradient from `a` to `b`.
    pub fn push_line_colors(
        &mut self,
        a: DVec2,
        b: DVec2,
        width_px: f32,
        color_a: Rgba,
        color_b: Rgba,
    ) {
        if !a.is_finite() || !b.is_finite() {
            return;
        }
        self.lines.push(LineSegment2D {
            a: self.local(a),
            b: self.local(b),
            color_a: pack_rgba(color_a),
            color_b: pack_rgba(color_b),
            width: if width_px.is_finite() { width_px } else { 1.0 },
        });
    }

    /// Adds consecutive segments through `pts` (and back to the first point when `closed`).
    pub fn push_polyline(&mut self, pts: &[DVec2], closed: bool, width_px: f32, color: Rgba) {
        for w in pts.windows(2) {
            self.push_line(w[0], w[1], width_px, color);
        }
        if closed && pts.len() > 2 {
            self.push_line(pts[pts.len() - 1], pts[0], width_px, color);
        }
    }

    /// Adds a dashed polyline. `pattern` is in world units, AutoCAD style: positive = dash, negative = gap,
    /// zero = dot. The pattern continues across vertices and starts `phase` units into the pattern.
    /// An empty or all-zero-length pattern draws a continuous line.
    pub fn push_dashed_polyline(
        &mut self,
        pts: &[DVec2],
        closed: bool,
        pattern: &[f64],
        phase: f64,
        width_px: f32,
        color: Rgba,
    ) {
        let total: f64 = pattern.iter().map(|d| d.abs()).sum();
        if pattern.is_empty() || !total.is_finite() || total <= 0.0 {
            self.push_polyline(pts, closed, width_px, color);
            return;
        }
        let mut path: Vec<DVec2> = pts.to_vec();
        if closed && pts.len() > 2 {
            path.push(pts[0]);
        }
        // Guard against absurd dash counts (pattern far smaller than the drawing): fall back to solid.
        let length: f64 = path.windows(2).map(|w| w[0].distance(w[1])).sum();
        if length / total > 1e6 {
            self.push_polyline(pts, closed, width_px, color);
            return;
        }
        let n = pattern.len();
        let (mut idx, mut remaining) = (0usize, pattern[0].abs());
        let mut ph = if phase.is_finite() {
            phase.rem_euclid(total)
        } else {
            0.0
        };
        while ph > 0.0 {
            if ph < remaining {
                remaining -= ph;
                break;
            }
            ph -= remaining;
            idx = (idx + 1) % n;
            remaining = pattern[idx].abs();
        }
        // Zero-length elements are dots: a zero-length capsule renders as a round dot of the line width.
        // `total > 0` guarantees these loops terminate.
        if let Some(&p) = path.first() {
            while pattern[idx] == 0.0 {
                self.push_line(p, p, width_px, color);
                idx = (idx + 1) % n;
                remaining = pattern[idx].abs();
            }
        }
        for w in path.windows(2) {
            let (a, b) = (w[0], w[1]);
            let seg_len = a.distance(b);
            if seg_len.is_nan() || seg_len <= 0.0 {
                continue;
            }
            let dir = (b - a) / seg_len;
            let mut pos = 0.0;
            while pos < seg_len {
                let step = remaining.min(seg_len - pos);
                if pattern[idx] > 0.0 {
                    self.push_line(a + dir * pos, a + dir * (pos + step), width_px, color);
                }
                pos += step;
                remaining -= step;
                if remaining <= 0.0 {
                    idx = (idx + 1) % n;
                    remaining = pattern[idx].abs();
                    while pattern[idx] == 0.0 {
                        let p = a + dir * pos;
                        self.push_line(p, p, width_px, color);
                        idx = (idx + 1) % n;
                        remaining = pattern[idx].abs();
                    }
                }
            }
        }
    }

    /// Adds one filled triangle.
    pub fn push_triangle(&mut self, a: DVec2, b: DVec2, c: DVec2, color: Rgba) {
        if !(a.is_finite() && b.is_finite() && c.is_finite()) {
            return;
        }
        let base = self.fill_vertices.len() as u32;
        let color = pack_rgba(color);
        for p in [a, b, c] {
            self.fill_vertices.push(FillVertex {
                pos: self.local(p),
                color,
            });
        }
        self.fill_indices
            .extend_from_slice(&[base, base + 1, base + 2]);
    }

    /// Adds an indexed triangle list (e.g. from lyon) with one color. Triangles referencing vertices
    /// outside `verts` are skipped.
    pub fn push_triangles(&mut self, verts: &[DVec2], indices: &[u32], color: Rgba) {
        let base = self.fill_vertices.len() as u32;
        let color = pack_rgba(color);
        for &p in verts {
            let pos = if p.is_finite() {
                self.local(p)
            } else {
                [0.0; 2]
            };
            self.fill_vertices.push(FillVertex { pos, color });
        }
        let n = verts.len() as u32;
        for tri in indices.as_chunks::<3>().0 {
            if tri.iter().all(|&i| i < n) {
                self.fill_indices.extend(tri.iter().map(|&i| base + i));
            }
        }
    }

    /// Adds a filled convex polygon (fan triangulation).
    pub fn push_convex_polygon(&mut self, pts: &[DVec2], color: Rgba) {
        if pts.len() < 3 {
            return;
        }
        let indices: Vec<u32> = (1..pts.len() as u32 - 1)
            .flat_map(|i| [0, i, i + 1])
            .collect();
        self.push_triangles(pts, &indices, color);
    }

    /// Adds a point marker.
    pub fn push_point(&mut self, p: DVec2, size_px: f32, shape: PointShape, color: Rgba) {
        if !p.is_finite() {
            return;
        }
        self.points.push(PointMarker {
            pos: self.local(p),
            color: pack_rgba(color),
            size: if size_px.is_finite() {
                size_px.max(0.0)
            } else {
                5.0
            },
            shape: shape as u32,
        });
    }

    /// World-space bounds of the content (line widths and marker sizes not included).
    pub fn bbox(&self) -> BBox2 {
        let o = self.origin;
        let w = |p: [f32; 2]| o + DVec2::new(p[0] as f64, p[1] as f64);
        let mut b = BBox2::EMPTY;
        for l in &self.lines {
            b.include(w(l.a));
            b.include(w(l.b));
        }
        for &i in &self.fill_indices {
            if let Some(v) = self.fill_vertices.get(i as usize) {
                b.include(w(v.pos));
            }
        }
        for p in &self.points {
            b.include(w(p.pos));
        }
        b
    }
}
