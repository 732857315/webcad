//! Parallel offset. Exact for lines, circles, arcs and bulge polylines (the latter through
//! cavalier_contours, which also removes self-intersections); ellipses and splines are flattened
//! first and offset as polylines (approximate, result is a polyline).

use cavalier_contours::polyline::{
    PlineCreation, PlineSource, PlineSourceMut, PlineVertex, Polyline,
};
use wcad_math::{DVec2, cross2};

use crate::bulge::PolySegment;
use crate::curve::Curve;
use crate::curves::{Arc2, Circle2, Curve2, Line2, PolyVertex, Polyline2};
use crate::regions::point_in_polygon;

/// Offset with a signed distance: positive = to the left of the curve direction (inward for
/// counter-clockwise closed curves, towards the center for circles and arcs).
pub fn offset_signed(c: &Curve2, d: f64) -> Vec<Curve2> {
    if !d.is_finite() {
        return Vec::new();
    }
    if d == 0.0 {
        return vec![c.clone()];
    }
    match c {
        Curve2::Line(l) => {
            let n = wcad_math::perp(l.dir());
            if n == DVec2::ZERO {
                return Vec::new();
            }
            vec![Curve2::Line(Line2::new(l.a + n * d, l.b + n * d))]
        }
        Curve2::Circle(ci) => {
            let r = ci.r - d;
            if r > 1e-12 * ci.r.abs().max(1.0) {
                vec![Curve2::Circle(Circle2::new(ci.c, r))]
            } else {
                Vec::new()
            }
        }
        Curve2::Arc(a) => {
            let r = a.r - d;
            if r > 1e-12 * a.r.abs().max(1.0) {
                vec![Curve2::Arc(Arc2::new(a.c, r, a.start, a.end))]
            } else {
                Vec::new()
            }
        }
        Curve2::Polyline(p) => offset_polyline(p, d)
            .into_iter()
            .map(Curve2::Polyline)
            .collect(),
        Curve2::Ellipse(_) | Curve2::Spline(_) => {
            let bb = c.bbox();
            let tol = (bb.size().length() * 2e-5).max(1e-9);
            let pts = c.flatten(tol);
            let closed = c.is_closed();
            let mut pl = Polyline2::from_points(pts, closed);
            if closed && pl.verts.len() > 2 {
                pl.verts.pop(); // last point duplicates the first
            }
            offset_polyline(&pl, d)
                .into_iter()
                .map(Curve2::Polyline)
                .collect()
        }
    }
}

/// Offset by `distance` (> 0) towards `side_point`: for closed curves the side is inside/outside,
/// for open curves the side of the curve closest to the point.
pub fn offset(c: &Curve2, distance: f64, side_point: DVec2) -> Vec<Curve2> {
    let d = distance.abs();
    offset_signed(c, d * offset_side(c, side_point))
}

/// `+1.0` if `side_point` is on the left of `c` (or inside a counter-clockwise closed curve),
/// `-1.0` otherwise.
pub fn offset_side(c: &Curve2, side_point: DVec2) -> f64 {
    if c.is_closed() {
        let bb = c.bbox();
        let tol = (bb.size().length() * 1e-5).max(1e-9);
        let poly = c.flatten(tol);
        let inside = point_in_polygon(&poly, side_point);
        let ccw = signed_area_pts(&poly) >= 0.0;
        return if inside == ccw { 1.0 } else { -1.0 };
    }
    let (t, q) = c.closest(side_point);
    let (a, b) = c.domain();
    let h = (b - a) * 1e-7;
    let t1 = c.tangent_at((t - h).max(a));
    let t2 = c.tangent_at((t + h).min(b));
    let tan = (t1 + t2).normalize_or(t1);
    let v = side_point - q;
    let s = cross2(tan, v);
    if s != 0.0 {
        return s.signum();
    }
    // side point on the curve's extension: use the end tangent
    if cross2(c.tangent_at(b), side_point - c.end()) >= 0.0 {
        1.0
    } else {
        -1.0
    }
}

pub(crate) fn signed_area_pts(p: &[DVec2]) -> f64 {
    if p.len() < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..p.len() {
        a += cross2(p[i], p[(i + 1) % p.len()]);
    }
    0.5 * a
}

/// Offset a bulge polyline with sharp (extended) corners between straight segments, like
/// AutoCAD's default `OFFSETGAPTYPE = 0`. See [`offset_polyline_round`] for round joins.
pub fn offset_polyline(p: &Polyline2, d: f64) -> Vec<Polyline2> {
    let src: Vec<DVec2> = p.verts.iter().map(|v| v.p).collect();
    let ext = wcad_math::BBox2::from_points(src.iter().copied())
        .size()
        .length();
    offset_polyline_round(p, d)
        .into_iter()
        .map(|o| sharpen_joins(&o, &src, d, ext))
        .collect()
}

/// Replace the round joins cavalier_contours inserts at convex corners (arcs of radius `|d|`
/// centered on a source vertex, between two straight segments) by the extended line intersection.
fn sharpen_joins(pl: &Polyline2, src: &[DVec2], d: f64, ext: f64) -> Polyline2 {
    let mut segs: Vec<PolySegment> = pl.segments().collect();
    let m = segs.len();
    if m < 3 {
        return pl.clone();
    }
    let eps = 1e-7 * ext.max(1e-300);
    let closed = pl.is_closed();
    let mut removed = vec![false; m];
    for i in 0..m {
        let PolySegment::Arc(a) = segs[i] else {
            continue;
        };
        if (a.r - d.abs()).abs() > eps || !src.iter().any(|v| v.distance(a.c) <= eps) {
            continue;
        }
        let (p, q) = match (i, closed) {
            (0, false) => continue,
            (i, false) if i + 1 == m => continue,
            (i, _) => ((i + m - 1) % m, (i + 1) % m),
        };
        if removed[p] || removed[q] {
            continue;
        }
        let (PolySegment::Line(lp), PolySegment::Line(lq)) = (segs[p], segs[q]) else {
            continue;
        };
        let (dp, dq) = (lp.b - lp.a, lq.b - lq.a);
        let den = cross2(dp, dq);
        if den.abs() <= 1e-9 * dp.length() * dq.length() {
            continue;
        }
        let t = cross2(lq.a - lp.a, dq) / den;
        let x = lp.a + dp * t;
        segs[p] = PolySegment::Line(Line2::new(lp.a, x));
        segs[q] = PolySegment::Line(Line2::new(x, lq.b));
        removed[i] = true;
    }
    if !removed.iter().any(|r| *r) {
        return pl.clone();
    }
    let kept: Vec<PolySegment> = segs
        .into_iter()
        .zip(removed)
        .filter(|(_, r)| !r)
        .map(|(s, _)| s)
        .collect();
    Polyline2::from_segments(&kept, closed)
}

/// Offset a bulge polyline with cavalier_contours (round joins at convex corners). Coordinates are
/// normalized (translated to the bbox center and scaled to an extent of ~1000) so cavalier's
/// absolute epsilons behave the same at every drawing scale.
pub fn offset_polyline_round(p: &Polyline2, d: f64) -> Vec<Polyline2> {
    let mut verts: Vec<PolyVertex> = p
        .verts
        .iter()
        .copied()
        .filter(|v| v.p.is_finite() && v.bulge.is_finite())
        .collect();
    if verts.len() < 2 {
        return Vec::new();
    }
    let bb = wcad_math::BBox2::from_points(verts.iter().map(|v| v.p));
    let ext = bb.size().length();
    if !(ext > 0.0) {
        return Vec::new();
    }
    let center = bb.center();
    let s = 1000.0 / ext;
    let eps = 1e-9 * ext;
    // drop consecutive duplicates (and a closing duplicate)
    verts.dedup_by(|b, a| a.p.distance(b.p) <= eps);
    if p.closed && verts.len() > 2 && verts[0].p.distance(verts[verts.len() - 1].p) <= eps {
        verts.pop();
    }
    if verts.len() < 2 {
        return Vec::new();
    }
    // bulges our own geometry treats as straight (non-finite arc) must be straight for cavalier too
    let m = verts.len();
    for i in 0..m {
        let next = verts[(i + 1) % m].p;
        if crate::bulge::bulge_to_arc(verts[i].p, next, verts[i].bulge).is_none() {
            verts[i].bulge = 0.0;
        }
    }
    if !p.closed
        && let Some(last) = verts.last_mut()
    {
        last.bulge = 0.0;
    }
    let mut cp: Polyline<f64> = Polyline::with_capacity(verts.len(), p.closed);
    for v in &verts {
        let q = (v.p - center) * s;
        cp.add_vertex(PlineVertex::new(q.x, q.y, v.bulge));
    }
    let Some(res) = guarded(|| cp.parallel_offset(d * s)) else {
        return Vec::new();
    };
    res.iter()
        .filter(|o| o.vertex_count() >= 2)
        .map(|o| Polyline2 {
            verts: o
                .iter_vertexes()
                .map(|v| PolyVertex::with_bulge(DVec2::new(v.x, v.y) / s + center, v.bulge))
                .collect(),
            closed: o.is_closed(),
        })
        .collect()
}

/// Run a cavalier_contours call; on native targets a panic inside the library (it has debug
/// assertions for degenerate input) is caught and reported as `None`. On wasm (`panic = abort`)
/// inputs are sanitized beforehand instead.
fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Some(f())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::{EllipseArc2, Nurbs2};
    use std::f64::consts::PI;

    #[test]
    fn exact_offsets() {
        let l = Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0)));
        let o = offset(&l, 2.0, DVec2::new(5.0, -3.0));
        assert_eq!(
            o,
            vec![Curve2::Line(Line2::new(
                DVec2::new(0.0, -2.0),
                DVec2::new(10.0, -2.0)
            ))]
        );
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0));
        assert_eq!(
            offset(&c, 1.0, DVec2::new(1.0, 0.0)),
            vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 4.0))]
        );
        assert_eq!(
            offset(&c, 1.0, DVec2::new(9.0, 0.0)),
            vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 6.0))]
        );
        assert!(offset(&c, 6.0, DVec2::ZERO).is_empty());
        let a = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, PI));
        assert_eq!(
            offset(&a, 1.0, DVec2::new(0.0, 10.0)),
            vec![Curve2::Arc(Arc2::new(DVec2::ZERO, 6.0, 0.0, PI))]
        );
    }

    #[test]
    fn polyline_offsets() {
        // rounded shape from the research notes: square 10x10 with a semicircular right side
        let p = Polyline2 {
            verts: vec![
                PolyVertex::new(DVec2::new(0.0, 0.0)),
                PolyVertex::with_bulge(DVec2::new(10.0, 0.0), 1.0),
                PolyVertex::new(DVec2::new(10.0, 10.0)),
                PolyVertex::new(DVec2::new(0.0, 10.0)),
            ],
            closed: true,
        };
        let area = p.signed_area();
        assert!((area - (100.0 + 12.5 * PI)).abs() < 1e-9);
        let inward = offset(&Curve2::Polyline(p.clone()), 1.0, DVec2::new(5.0, 5.0));
        assert_eq!(inward.len(), 1);
        let Curve2::Polyline(ip) = &inward[0] else {
            panic!()
        };
        assert!(ip.signed_area().abs() < area);
        let outward = offset(&Curve2::Polyline(p.clone()), 1.0, DVec2::new(-5.0, 5.0));
        let Curve2::Polyline(op) = &outward[0] else {
            panic!()
        };
        // two sharp corners on the left: round result + 2·(1 − π/4)
        let round = offset_polyline_round(&p, -1.0);
        assert!(
            (round[0].signed_area().abs() - 188.12).abs() < 0.05,
            "{}",
            round[0].signed_area()
        );
        assert!(
            (op.signed_area().abs() - round[0].signed_area().abs() - 2.0 * (1.0 - PI / 4.0)).abs()
                < 1e-9,
            "{}",
            op.signed_area()
        );
        assert_eq!(op.verts.len(), 4);
        // open L-shaped polyline: sharp corner
        let l = Polyline2::from_points(
            [DVec2::new(0.0, 10.0), DVec2::ZERO, DVec2::new(10.0, 0.0)],
            false,
        );
        let o = offset_signed(&Curve2::Polyline(l), -1.0);
        let Curve2::Polyline(ol) = &o[0] else {
            panic!()
        };
        assert_eq!(ol.verts.len(), 3, "{ol:?}");
        assert!((ol.verts[1].p - DVec2::new(-1.0, -1.0)).length() < 1e-9);
        // U-shape with a narrow slot splits into two loops inward
        let u = Polyline2::from_points(
            [
                (0.0, 0.0),
                (10.0, 0.0),
                (10.0, 10.0),
                (6.0, 10.0),
                (6.0, 2.0),
                (4.0, 2.0),
                (4.0, 10.0),
                (0.0, 10.0),
            ]
            .map(|(x, y)| DVec2::new(x, y)),
            true,
        );
        assert_eq!(offset_signed(&Curve2::Polyline(u.clone()), 1.5).len(), 2);
        assert_eq!(offset_signed(&Curve2::Polyline(u), -1.5).len(), 1);
        // scale independence: same shape at 1e-3 scale and far from the origin
        let small = Polyline2::from_points(
            [
                DVec2::new(1e6, 1e6),
                DVec2::new(1e6 + 0.01, 1e6),
                DVec2::new(1e6 + 0.01, 1e6 + 0.01),
                DVec2::new(1e6, 1e6 + 0.01),
            ],
            true,
        );
        let o = offset_signed(&Curve2::Polyline(small), 0.001);
        assert_eq!(o.len(), 1);
        let Curve2::Polyline(op) = &o[0] else {
            panic!()
        };
        assert!(
            (op.signed_area() - 0.008 * 0.008).abs() < 1e-10,
            "{}",
            op.signed_area()
        );
    }

    #[test]
    fn approximate_offsets() {
        let e = Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(5.0, 0.0),
            ratio: 0.6,
            start: 0.0,
            end: std::f64::consts::TAU,
        });
        let o = offset(&e, 0.5, DVec2::new(10.0, 0.0));
        assert_eq!(o.len(), 1);
        // every offset vertex is at distance 0.5 from the ellipse
        let Curve2::Polyline(p) = &o[0] else { panic!() };
        for v in &p.verts {
            let (_, q) = e.closest(v.p);
            assert!((q.distance(v.p) - 0.5).abs() < 1e-3, "{}", q.distance(v.p));
        }
        let s = Curve2::Spline(
            Nurbs2::from_fit_points(
                &[DVec2::ZERO, DVec2::new(2.0, 1.0), DVec2::new(4.0, 0.0)],
                3,
            )
            .unwrap(),
        );
        let o = offset_signed(&s, 0.2);
        assert_eq!(o.len(), 1);
    }
}
