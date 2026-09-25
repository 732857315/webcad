//! Bulge helpers and the polyline segment iterator.
//!
//! A polyline segment from `p0` to `p1` with bulge `b` is an arc with signed sweep `4·atan(b)`
//! (positive = counter-clockwise); `b = 0` is a straight segment.

use wcad_math::{DVec2, cross2, perp};

use crate::curves::{Arc2, Line2, PolyVertex, Polyline2};

/// Bulges smaller than this are treated as straight segments.
pub const BULGE_EPS: f64 = 1e-12;

/// Arc geometry of a bulge segment: center, radius, start angle and signed sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BulgeArc {
    pub c: DVec2,
    pub r: f64,
    pub start: f64,
    /// Signed sweep, positive = counter-clockwise.
    pub sweep: f64,
}

impl BulgeArc {
    pub fn point_at(&self, u: f64) -> DVec2 {
        let a = self.start + self.sweep * u;
        self.c + DVec2::new(a.cos(), a.sin()) * self.r
    }
    /// The same point set as a counter-clockwise [`Arc2`].
    pub fn to_arc(&self) -> Arc2 {
        if self.sweep >= 0.0 {
            Arc2::new(self.c, self.r, self.start, self.start + self.sweep)
        } else {
            Arc2::new(self.c, self.r, self.start + self.sweep, self.start)
        }
    }
    pub fn is_ccw(&self) -> bool {
        self.sweep >= 0.0
    }
}

/// Arc for the bulge segment `p0 → p1`, `None` for straight (or degenerate) segments.
pub fn bulge_to_arc(p0: DVec2, p1: DVec2, bulge: f64) -> Option<BulgeArc> {
    if bulge.abs() < BULGE_EPS || !bulge.is_finite() {
        return None;
    }
    let chord = p1 - p0;
    let c_len = chord.length();
    if c_len < 1e-300 {
        return None;
    }
    let sweep = 4.0 * bulge.atan();
    // Distance from chord midpoint to center along the left normal: d = (c/2) * (1 - b²) / (2b).
    let mid = (p0 + p1) * 0.5;
    let n = perp(chord) / c_len;
    let d = 0.5 * c_len * (1.0 - bulge * bulge) / (2.0 * bulge);
    let c = mid + n * d;
    let r = c.distance(p0);
    if !c.is_finite() || !r.is_finite() {
        return None;
    }
    let start = (p0 - c).y.atan2((p0 - c).x);
    Some(BulgeArc { c, r, start, sweep })
}

/// Bulge value for a signed sweep angle.
pub fn sweep_to_bulge(sweep: f64) -> f64 {
    (sweep / 4.0).tan()
}

/// Bulge of the arc through `p0`, `pm`, `p1` (in that order); 0 if collinear.
pub fn bulge_from_three_points(p0: DVec2, pm: DVec2, p1: DVec2) -> f64 {
    // The inscribed angle theorem: sweep/2 = π - angle(p0, pm, p1) with orientation sign.
    let a = p0 - pm;
    let b = p1 - pm;
    let cr = cross2(a, b);
    if cr.abs() <= 1e-15 * a.length() * b.length() {
        return 0.0;
    }
    let ang = cr.abs().atan2(a.dot(b)); // interior angle at pm in (0, π)
    let half = std::f64::consts::PI - ang;
    // pm on the right of p0→p1 (cr > 0 means a×b > 0: p0 → pm → p1 turns...) decide via side.
    let side = cross2(p1 - p0, pm - p0);
    let sweep = if side < 0.0 { 2.0 * half } else { -2.0 * half };
    sweep_to_bulge(sweep)
}

/// One polyline segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PolySegment {
    Line(Line2),
    Arc(BulgeArc),
}

impl PolySegment {
    pub fn from_bulge(p0: DVec2, p1: DVec2, bulge: f64) -> Self {
        match bulge_to_arc(p0, p1, bulge) {
            Some(a) => PolySegment::Arc(a),
            None => PolySegment::Line(Line2::new(p0, p1)),
        }
    }
    /// Point at local parameter `u ∈ [0, 1]`.
    pub fn point_at(&self, u: f64) -> DVec2 {
        match self {
            PolySegment::Line(l) => l.at(u),
            PolySegment::Arc(a) => a.point_at(u),
        }
    }
    /// Derivative with respect to `u`.
    pub fn deriv_at(&self, u: f64) -> DVec2 {
        match self {
            PolySegment::Line(l) => l.b - l.a,
            PolySegment::Arc(a) => {
                let ang = a.start + a.sweep * u;
                DVec2::new(-ang.sin(), ang.cos()) * (a.r * a.sweep)
            }
        }
    }
    /// Second derivative with respect to `u`.
    pub fn deriv2_at(&self, u: f64) -> DVec2 {
        match self {
            PolySegment::Line(_) => DVec2::ZERO,
            PolySegment::Arc(a) => {
                let ang = a.start + a.sweep * u;
                -DVec2::new(ang.cos(), ang.sin()) * (a.r * a.sweep * a.sweep)
            }
        }
    }
    pub fn start(&self) -> DVec2 {
        self.point_at(0.0)
    }
    pub fn end(&self) -> DVec2 {
        self.point_at(1.0)
    }
    pub fn length(&self) -> f64 {
        match self {
            PolySegment::Line(l) => l.length(),
            PolySegment::Arc(a) => a.r * a.sweep.abs(),
        }
    }
    pub fn bulge(&self) -> f64 {
        match self {
            PolySegment::Line(_) => 0.0,
            PolySegment::Arc(a) => sweep_to_bulge(a.sweep),
        }
    }
    /// Local parameter of the counter-clockwise arc angle `ang` on an arc segment (unclamped).
    pub(crate) fn arc_angle_to_u(a: &BulgeArc, ang: f64) -> f64 {
        let ccw = a.to_arc();
        let sw = a.sweep.abs();
        if sw <= 0.0 {
            return 0.0;
        }
        let d = ang - ccw.start;
        if a.is_ccw() { d / sw } else { 1.0 - d / sw }
    }
}

/// Iterator over the segments of a polyline (including the closing segment when closed).
pub struct PolySegments<'a> {
    pl: &'a Polyline2,
    i: usize,
    n: usize,
}

impl Iterator for PolySegments<'_> {
    type Item = PolySegment;
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.n {
            return None;
        }
        let v = &self.pl.verts;
        let a = v[self.i];
        let b = v[(self.i + 1) % v.len()];
        self.i += 1;
        Some(PolySegment::from_bulge(a.p, b.p, a.bulge))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let r = self.n - self.i;
        (r, Some(r))
    }
}

impl ExactSizeIterator for PolySegments<'_> {}

impl Polyline2 {
    /// Iterate over the segments (lines and bulge arcs).
    pub fn segments(&self) -> PolySegments<'_> {
        PolySegments {
            pl: self,
            i: 0,
            n: self.segment_count(),
        }
    }

    /// Segment `i` (`None` when out of range).
    pub fn segment(&self, i: usize) -> Option<PolySegment> {
        if i >= self.segment_count() {
            return None;
        }
        let a = self.verts[i];
        let b = self.verts[(i + 1) % self.verts.len()];
        Some(PolySegment::from_bulge(a.p, b.p, a.bulge))
    }

    /// Build an open or closed polyline from a list of segments that are head-to-tail.
    pub fn from_segments(segs: &[PolySegment], closed: bool) -> Self {
        let mut verts: Vec<PolyVertex> = Vec::with_capacity(segs.len() + 1);
        for s in segs {
            verts.push(PolyVertex::with_bulge(s.start(), s.bulge()));
        }
        if !closed && let Some(last) = segs.last() {
            verts.push(PolyVertex::new(last.end()));
        }
        Polyline2 { verts, closed }
    }

    /// Signed area enclosed by a closed polyline (positive = counter-clockwise). Open polylines are
    /// treated as closed by their chord.
    pub fn signed_area(&self) -> f64 {
        let n = self.verts.len();
        if n < 2 {
            return 0.0;
        }
        // relative to the first vertex to avoid cancellation far from the origin
        let o = self.verts[0].p;
        let mut a = 0.0;
        for i in 0..n {
            if i + 1 == n && !self.closed {
                break; // implicit straight closing chord contributes cross(v, o - o) = 0
            }
            let v0 = self.verts[i];
            let v1 = self.verts[(i + 1) % n];
            a += cross2(v0.p - o, v1.p - o);
            if let Some(arc) = bulge_to_arc(v0.p, v1.p, v0.bulge) {
                // circular segment area between chord and arc
                let seg = 0.5 * arc.r * arc.r * (arc.sweep - arc.sweep.sin());
                a += 2.0 * seg;
            }
        }
        0.5 * a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    #[test]
    fn bulge_semicircle() {
        let a = bulge_to_arc(DVec2::new(-1.0, 0.0), DVec2::new(1.0, 0.0), 1.0).unwrap();
        assert!(a.c.length() < 1e-15);
        assert!((a.r - 1.0).abs() < 1e-15);
        assert!((a.sweep - PI).abs() < 1e-15);
        // CCW from (-1,0) by π goes through (0,-1)
        assert!((a.point_at(0.5) - DVec2::new(0.0, -1.0)).length() < 1e-12);
        let b = bulge_to_arc(
            DVec2::new(1.0, 0.0),
            DVec2::new(0.0, 1.0),
            sweep_to_bulge(-3.0 * FRAC_PI_2),
        )
        .unwrap();
        assert!(b.c.length() < 1e-12, "{:?}", b.c);
        assert!((b.point_at(0.5) - DVec2::new(-1.0, -1.0).normalize()).length() < 1e-12);
    }

    #[test]
    fn three_point_bulge() {
        let b = bulge_from_three_points(
            DVec2::new(-1.0, 0.0),
            DVec2::new(0.0, -1.0),
            DVec2::new(1.0, 0.0),
        );
        assert!((b - 1.0).abs() < 1e-12, "{b}");
        let b = bulge_from_three_points(
            DVec2::new(-1.0, 0.0),
            DVec2::new(0.0, 1.0),
            DVec2::new(1.0, 0.0),
        );
        assert!((b + 1.0).abs() < 1e-12, "{b}");
        let p0 = DVec2::new(1.0, 0.0);
        let pm = DVec2::new(0.5f64.sqrt(), 0.5f64.sqrt());
        let p1 = DVec2::new(0.0, 1.0);
        let b = bulge_from_three_points(p0, pm, p1);
        assert!((b - sweep_to_bulge(FRAC_PI_2)).abs() < 1e-12);
    }

    #[test]
    fn area_with_bulges() {
        // circle r=1 from two semicircles
        let pl = Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::new(-1.0, 0.0), 1.0),
                PolyVertex::with_bulge(DVec2::new(1.0, 0.0), 1.0),
            ],
            closed: true,
        };
        assert!((pl.signed_area() - PI).abs() < 1e-12);
        let sq = Polyline2::from_points([DVec2::ZERO, DVec2::X, DVec2::ONE, DVec2::Y], true);
        assert!((sq.signed_area() - 1.0).abs() < 1e-15);
        assert_eq!(sq.segments().count(), 4);
    }
}
