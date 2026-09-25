//! Curve/curve intersection.
//!
//! Closed forms for line/circle/arc pairs, ellipses by mapping one ellipse to the unit circle
//! (line → quadratic, conic → trigonometric root isolation), splines by flattening + Newton
//! refinement (with alternating projection for tangential contacts). Polylines are decomposed
//! into their line/arc segments. Collinear overlapping lines and coincident arcs report the
//! overlap endpoints.

use std::f64::consts::TAU;

use wcad_math::{BBox2, DVec2, cross2, normalize_0_2pi, perp};

use crate::bulge::{BulgeArc, PolySegment};
use crate::curve::{Curve, arc_param, refine_closest};
use crate::curves::{Curve2, EllipseArc2, Line2};
use crate::numeric::SafeClamp;
use crate::numeric::{find_roots, solve2};

/// One intersection point with the parameters on both curves (see [`crate::curve`] for domains).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Intersection {
    pub p: DVec2,
    pub ta: f64,
    pub tb: f64,
}

/// Default absolute tolerance for a pair of curves: `1e-9 × max(1, extent)`.
pub fn default_tol(a: &Curve2, b: &Curve2) -> f64 {
    let bb = a.bbox().union(&b.bbox());
    1e-9 * bb.size().length().max(1.0)
}

/// All intersection points of `a` and `b`, sorted by `ta`.
pub fn intersect(a: &Curve2, b: &Curve2) -> Vec<Intersection> {
    intersect_tol(a, b, default_tol(a, b))
}

/// [`intersect`] with an explicit tolerance (tangency threshold, range slack and merge distance).
pub fn intersect_tol(a: &Curve2, b: &Curve2, tol: f64) -> Vec<Intersection> {
    let tol = if tol > 0.0 && tol.is_finite() {
        tol
    } else {
        default_tol(a, b)
    };
    if !a.bbox().expanded(tol).intersects(&b.bbox().expanded(tol)) {
        return Vec::new();
    }
    let mut out = if is_generic(a) || is_generic(b) {
        a.with_dyn(|ca| b.with_dyn(|cb| generic(ca, cb, tol)))
    } else {
        let pa = parts(a);
        let pb = parts(b);
        let mut v = Vec::new();
        for x in &pa {
            for y in &pb {
                if !x.bbox.expanded(tol).intersects(&y.bbox.expanded(tol)) {
                    continue;
                }
                for (p, na, nb) in part_hits(&x.part, &y.part, tol) {
                    if let (Some(ta), Some(tb)) = (x.accept(na, tol), y.accept(nb, tol)) {
                        v.push(Intersection { p, ta, tb });
                    }
                }
            }
        }
        v
    };
    out.sort_by(|p, q| p.ta.total_cmp(&q.ta));
    dedup(&mut out, tol * 16.0);
    out
}

fn dedup(v: &mut Vec<Intersection>, d: f64) {
    let mut keep: Vec<Intersection> = Vec::with_capacity(v.len());
    for x in v.drain(..) {
        if !keep.iter().any(|k| k.p.distance(x.p) <= d) {
            keep.push(x);
        }
    }
    *v = keep;
}

fn is_generic(c: &Curve2) -> bool {
    matches!(c, Curve2::Spline(s) if s.is_valid())
}

// ------------------------------------------------------------------------------------------------
// decomposition

#[derive(Clone, Debug)]
enum Part {
    Line(Line2),
    /// Circle with optional CCW range `(start, sweep)`.
    Circ {
        c: DVec2,
        r: f64,
        range: Option<(f64, f64)>,
    },
    Ell(EllipseArc2),
}

#[derive(Clone, Copy, Debug)]
enum Map {
    /// Native parameter is the parent parameter.
    Same,
    /// Line segment `i` of a polyline: parent = off + u.
    Seg(f64),
    /// Arc segment of a polyline: native angle → parent = off + u(angle).
    Bulge(f64, BulgeArc),
}

struct PartRef {
    part: Part,
    map: Map,
    bbox: BBox2,
}

impl PartRef {
    /// Range filter + map to the parent parameter.
    fn accept(&self, native: f64, tol: f64) -> Option<f64> {
        match &self.part {
            Part::Line(l) => {
                let len = l.length().max(1e-300);
                let e = tol / len;
                if native < -e || native > 1.0 + e {
                    return None;
                }
                let u = native.clamp(0.0, 1.0);
                match self.map {
                    Map::Seg(off) => Some(off + u),
                    _ => Some(u),
                }
            }
            Part::Circ { r, range, .. } => {
                let e = tol / r.max(1e-300);
                let ang = match range {
                    None => normalize_0_2pi(native),
                    Some((s, sw)) => arc_param(*s, *sw, native, e)?,
                };
                match self.map {
                    Map::Bulge(off, a) => {
                        Some(off + PolySegment::arc_angle_to_u(&a, ang).clamp(0.0, 1.0))
                    }
                    _ => Some(ang),
                }
            }
            Part::Ell(e) => {
                let eps = tol / e.minor().length().max(1e-300);
                e.param_of_angle(native, eps)
            }
        }
    }
}

fn parts(c: &Curve2) -> Vec<PartRef> {
    let mk = |part: Part, map: Map| {
        let bbox = match &part {
            Part::Line(l) => l.bbox(),
            Part::Circ { c, r, range: None } => crate::Circle2::new(*c, *r).bbox(),
            Part::Circ {
                c,
                r,
                range: Some((s, sw)),
            } => crate::Arc2::new(*c, *r, *s, s + sw).bbox(),
            Part::Ell(e) => e.bbox(),
        };
        PartRef { part, map, bbox }
    };
    match c {
        Curve2::Line(l) => vec![mk(Part::Line(*l), Map::Same)],
        Curve2::Circle(ci) => vec![mk(
            Part::Circ {
                c: ci.c,
                r: ci.r,
                range: None,
            },
            Map::Same,
        )],
        Curve2::Arc(a) => vec![mk(
            Part::Circ {
                c: a.c,
                r: a.r,
                range: Some((a.start, a.sweep())),
            },
            Map::Same,
        )],
        Curve2::Ellipse(e) => vec![mk(Part::Ell(*e), Map::Same)],
        Curve2::Polyline(p) => p
            .segments()
            .enumerate()
            .map(|(i, s)| match s {
                PolySegment::Line(l) => mk(Part::Line(l), Map::Seg(i as f64)),
                PolySegment::Arc(a) => {
                    let ccw = a.to_arc();
                    mk(
                        Part::Circ {
                            c: ccw.c,
                            r: ccw.r,
                            range: Some((ccw.start, ccw.sweep())),
                        },
                        Map::Bulge(i as f64, a),
                    )
                }
            })
            .collect(),
        Curve2::Spline(s) => {
            // invalid spline: its control polygon
            parts(&Curve2::Polyline(s.fallback_polyline()))
        }
    }
}

/// Raw hits between two parts: `(point, native_a, native_b)`.
fn part_hits(a: &Part, b: &Part, tol: f64) -> Vec<(DVec2, f64, f64)> {
    match (a, b) {
        (Part::Line(x), Part::Line(y)) => line_line(x, y, tol),
        (Part::Line(x), Part::Circ { c, r, range }) => line_circle(x, *c, *r, *range, tol),
        (Part::Circ { c, r, range }, Part::Line(y)) => swap(line_circle(y, *c, *r, *range, tol)),
        (
            Part::Circ {
                c: c1,
                r: r1,
                range: g1,
            },
            Part::Circ {
                c: c2,
                r: r2,
                range: g2,
            },
        ) => circle_circle(*c1, *r1, *g1, *c2, *r2, *g2, tol),
        (Part::Ell(e), _) => ellipse_part(e, b, tol),
        (_, Part::Ell(e)) => swap(ellipse_part(e, a, tol)),
    }
}

fn swap(v: Vec<(DVec2, f64, f64)>) -> Vec<(DVec2, f64, f64)> {
    v.into_iter().map(|(p, a, b)| (p, b, a)).collect()
}

fn angle_of(p: DVec2, c: DVec2) -> f64 {
    let d = p - c;
    d.y.atan2(d.x)
}

// ------------------------------------------------------------------------------------------------
// closed forms

fn line_line(l1: &Line2, l2: &Line2, tol: f64) -> Vec<(DVec2, f64, f64)> {
    let d1 = l1.b - l1.a;
    let d2 = l2.b - l2.a;
    let (n1, n2) = (d1.length(), d2.length());
    if n1 < 1e-300 || n2 < 1e-300 {
        return Vec::new();
    }
    let den = cross2(d1, d2);
    let w = l2.a - l1.a;
    if den.abs() > 1e-12 * n1 * n2 {
        let u1 = cross2(w, d2) / den;
        let u2 = cross2(w, d1) / den;
        return vec![(l1.a + d1 * u1, u1, u2)];
    }
    // parallel: collinear within tol?
    let dist = cross2(d1, w).abs() / n1;
    if dist > tol {
        return Vec::new();
    }
    let proj1 = |p: DVec2| (p - l1.a).dot(d1) / (n1 * n1);
    let proj2 = |p: DVec2| (p - l2.a).dot(d2) / (n2 * n2);
    vec![
        (l2.a, proj1(l2.a), 0.0),
        (l2.b, proj1(l2.b), 1.0),
        (l1.a, 0.0, proj2(l1.a)),
        (l1.b, 1.0, proj2(l1.b)),
    ]
}

fn line_circle(
    l: &Line2,
    c: DVec2,
    r: f64,
    _range: Option<(f64, f64)>,
    tol: f64,
) -> Vec<(DVec2, f64, f64)> {
    let d = l.b - l.a;
    let dd = d.length_squared();
    if dd < 1e-300 {
        return Vec::new();
    }
    let uf = (c - l.a).dot(d) / dd;
    let foot = l.a + d * uf;
    let h = foot.distance(c);
    if (h - r).abs() <= tol {
        return vec![(foot, uf, angle_of(foot, c))];
    }
    if h > r {
        return Vec::new();
    }
    let half = ((r - h) * (r + h)).max(0.0).sqrt() / dd.sqrt();
    [uf - half, uf + half]
        .into_iter()
        .map(|u| {
            let p = l.a + d * u;
            // snap onto the circle to limit drift
            (p, u, angle_of(p, c))
        })
        .collect()
}

fn circle_circle(
    c1: DVec2,
    r1: f64,
    g1: Option<(f64, f64)>,
    c2: DVec2,
    r2: f64,
    g2: Option<(f64, f64)>,
    tol: f64,
) -> Vec<(DVec2, f64, f64)> {
    let v = c2 - c1;
    let d = v.length();
    if d <= tol && (r1 - r2).abs() <= tol {
        // coincident circles: report arc endpoints lying on the other arc
        let mut out = Vec::new();
        for (s, sw) in [g1, g2].into_iter().flatten() {
            for ang in [s, s + sw] {
                let p = c1 + DVec2::new(ang.cos(), ang.sin()) * r1;
                out.push((p, ang, ang));
            }
        }
        return out;
    }
    if d < 1e-300 {
        return Vec::new();
    }
    let dir = v / d;
    if (d - (r1 + r2)).abs() <= tol {
        let p = c1 + dir * r1;
        return vec![(p, angle_of(p, c1), angle_of(p, c2))];
    }
    if (d - (r1 - r2).abs()).abs() <= tol {
        let p = if r1 >= r2 {
            c1 + dir * r1
        } else {
            c2 - dir * r2
        };
        return vec![(p, angle_of(p, c1), angle_of(p, c2))];
    }
    if d > r1 + r2 || d < (r1 - r2).abs() {
        return Vec::new();
    }
    let a = (d * d + r1 * r1 - r2 * r2) / (2.0 * d);
    let h = (r1 * r1 - a * a).max(0.0).sqrt();
    let base = c1 + dir * a;
    let n = perp(dir);
    [base + n * h, base - n * h]
        .into_iter()
        .map(|p| (p, angle_of(p, c1), angle_of(p, c2)))
        .collect()
}

/// Intersections of ellipse `e` with another part, in `e`'s parametric angle (native a).
fn ellipse_part(e: &EllipseArc2, other: &Part, tol: f64) -> Vec<(DVec2, f64, f64)> {
    let um = e.unit_map();
    if um.matrix2.determinant().abs() < 1e-300 {
        return Vec::new();
    }
    let inv = um.inverse();
    let minor = e.minor().length().max(1e-300);
    let utol = tol / minor;
    match other {
        Part::Line(l) => {
            let ul = Line2::new(inv.transform_point2(l.a), inv.transform_point2(l.b));
            line_circle(&ul, DVec2::ZERO, 1.0, None, utol)
                .into_iter()
                .map(|(_, u, th)| (e.at_param(th), th, u))
                .collect()
        }
        Part::Circ { c, r, .. } => {
            let cc = inv.transform_point2(*c);
            let p = inv.transform_vector2(DVec2::new(*r, 0.0));
            let q = inv.transform_vector2(DVec2::new(0.0, *r));
            conic_roots(cc, p, q, utol)
                .into_iter()
                .map(|th| {
                    let pt = e.at_param(th);
                    (pt, th, angle_of(pt, *c))
                })
                .collect()
        }
        Part::Ell(e2) => {
            let cc = inv.transform_point2(e2.c);
            let p = inv.transform_vector2(e2.major);
            let q = inv.transform_vector2(e2.minor());
            conic_roots(cc, p, q, utol)
                .into_iter()
                .map(|th| {
                    let pt = e.at_param(th);
                    (pt, th, e2.angle_of_point(pt))
                })
                .collect()
        }
    }
}

/// Angles θ where the unit circle meets the ellipse `c + p cos s + q sin s`.
fn conic_roots(c: DVec2, p: DVec2, q: DVec2, utol: f64) -> Vec<f64> {
    let m = wcad_math::DMat2::from_cols(p, q);
    if m.determinant().abs() < 1e-300 {
        return Vec::new();
    }
    let mi = m.inverse();
    let f = |th: f64| {
        let u = DVec2::new(th.cos(), th.sin());
        (mi * (u - c)).length_squared() - 1.0
    };
    let df = |th: f64| {
        let u = DVec2::new(th.cos(), th.sin());
        let du = DVec2::new(-th.sin(), th.cos());
        2.0 * (mi * (u - c)).dot(mi * du)
    };
    // |f| scale: gradient of |M⁻¹x|² is ~2|M⁻¹|, so a distance tolerance maps to this f tolerance
    let g = (mi.x_axis.length() + mi.y_axis.length()).max(1e-300);
    let zero_eps = (utol * 2.0 * g * (1.0 + g * (1.0 + c.length()))).max(1e-13);
    let mut r = find_roots(&f, &df, 0.0, TAU, 180, zero_eps, 1e-9);
    // merge seam duplicates (0 and 2π)
    if r.len() >= 2
        && let (Some(first), Some(last)) = (r.first().copied(), r.last().copied())
        && (last - first - TAU).abs() < 1e-7
    {
        r.pop();
    }
    r
}

// ------------------------------------------------------------------------------------------------
// generic numeric intersection (splines)

fn generic(a: &dyn Curve, b: &dyn Curve, tol: f64) -> Vec<Intersection> {
    let bb = a.bbox().union(&b.bbox());
    let ext = bb.size().length().max(1e-300);
    let ftol = ext * 2e-4;
    let fa = a.flatten_params(ftol);
    let fb = b.flatten_params(ftol);
    let mut cands: Vec<(f64, f64)> = Vec::new();
    let near = ftol * 3.0;
    let boxes_b: Vec<BBox2> = fb
        .windows(2)
        .map(|w| BBox2::new(w[0].1, w[1].1).expanded(near))
        .collect();
    for wa in fa.windows(2) {
        let ba = BBox2::new(wa[0].1, wa[1].1);
        for (j, wb) in fb.windows(2).enumerate() {
            if !ba.intersects(&boxes_b[j]) {
                continue;
            }
            let (p0, p1, q0, q1) = (wa[0].1, wa[1].1, wb[0].1, wb[1].1);
            let (s, t, dist) = seg_seg_closest(p0, p1, q0, q1);
            if dist <= near {
                cands.push((
                    wa[0].0 + (wa[1].0 - wa[0].0) * s,
                    wb[0].0 + (wb[1].0 - wb[0].0) * t,
                ));
            }
        }
    }
    let (a0, a1) = a.domain();
    let (b0, b1) = b.domain();
    let accept = (tol * 100.0).max(ext * 1e-9);
    let mut out: Vec<Intersection> = Vec::new();
    for (s0, t0) in cands {
        let (mut s, mut t) = (s0, t0);
        let mut ok = false;
        for _ in 0..40 {
            let f = a.point_at(s) - b.point_at(t);
            if f.length() <= tol * 0.01 {
                ok = true;
                break;
            }
            let da = a.deriv_at(s);
            let db = b.deriv_at(t);
            let Some((ds, dt)) = solve2(da.x, -db.x, da.y, -db.y, -f.x, -f.y) else {
                break;
            };
            let ns = (s + ds).sclamp(a0, a1);
            let nt = (t + dt).sclamp(b0, b1);
            if (ns - s).abs() + (nt - t).abs() < 1e-16 {
                break;
            }
            s = ns;
            t = nt;
        }
        if !ok {
            // tangential contact: alternating projections, then accept if the gap closed
            let (mut s2, mut t2) = (s0, t0);
            for _ in 0..200 {
                let ls = (s2 - (a1 - a0) * 0.05).max(a0);
                let hs = (s2 + (a1 - a0) * 0.05).min(a1);
                let lt = (t2 - (b1 - b0) * 0.05).max(b0);
                let ht = (t2 + (b1 - b0) * 0.05).min(b1);
                let ns = refine_closest(a, b.point_at(t2), s2, ls, hs);
                let nt = refine_closest(b, a.point_at(ns), t2, lt, ht);
                let moved = (ns - s2).abs() + (nt - t2).abs();
                s2 = ns;
                t2 = nt;
                if moved < 1e-15 * (1.0 + s2.abs() + t2.abs()) {
                    break;
                }
            }
            let gap = a.point_at(s2).distance(b.point_at(t2));
            if gap <= accept {
                s = s2;
                t = t2;
                ok = true;
            }
        }
        if ok {
            let p = (a.point_at(s) + b.point_at(t)) * 0.5;
            out.push(Intersection { p, ta: s, tb: t });
        }
    }
    out
}

/// Closest points of two segments: `(s, t, distance)` with `s, t ∈ [0, 1]`.
pub(crate) fn seg_seg_closest(p0: DVec2, p1: DVec2, q0: DVec2, q1: DVec2) -> (f64, f64, f64) {
    let d1 = p1 - p0;
    let d2 = q1 - q0;
    let den = cross2(d1, d2);
    if den.abs() > 1e-300 {
        let w = q0 - p0;
        let s = cross2(w, d2) / den;
        let t = cross2(w, d1) / den;
        if (0.0..=1.0).contains(&s) && (0.0..=1.0).contains(&t) {
            return (s, t, 0.0);
        }
    }
    // closest among endpoint projections
    let mut best = (0.0, 0.0, f64::INFINITY);
    let proj = |p: DVec2, a: DVec2, d: DVec2| {
        let l2 = d.length_squared();
        if l2 < 1e-300 {
            0.0
        } else {
            ((p - a).dot(d) / l2).clamp(0.0, 1.0)
        }
    };
    for (s, t) in [
        (0.0, proj(p0, q0, d2)),
        (1.0, proj(p1, q0, d2)),
        (proj(q0, p0, d1), 0.0),
        (proj(q1, p0, d1), 1.0),
    ] {
        let dist = (p0 + d1 * s).distance(q0 + d2 * t);
        if dist < best.2 {
            best = (s, t, dist);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn check(a: &Curve2, b: &Curve2, hits: &[Intersection]) {
        for h in hits {
            assert!(
                a.point_at(h.ta).distance(h.p) < 1e-7,
                "{} ta {h:?}",
                a.kind_name()
            );
            assert!(
                b.point_at(h.tb).distance(h.p) < 1e-7,
                "{} tb {h:?}",
                b.kind_name()
            );
        }
    }

    /// Brute force: flatten both finely and count proper segment crossings.
    fn brute_count(a: &Curve2, b: &Curve2) -> usize {
        let fa = a.flatten(1e-5);
        let fb = b.flatten(1e-5);
        let bbs: Vec<BBox2> = fb.windows(2).map(|w| BBox2::new(w[0], w[1])).collect();
        let mut pts: Vec<DVec2> = Vec::new();
        for wa in fa.windows(2) {
            let ba = BBox2::new(wa[0], wa[1]);
            for (j, wb) in fb.windows(2).enumerate() {
                if !ba.intersects(&bbs[j]) {
                    continue;
                }
                let (s, _, d) = seg_seg_closest(wa[0], wa[1], wb[0], wb[1]);
                if d == 0.0 {
                    let p = wa[0].lerp(wa[1], s);
                    if !pts.iter().any(|q| q.distance(p) < 1e-3) {
                        pts.push(p);
                    }
                }
            }
        }
        pts.len()
    }

    #[test]
    fn line_line_basic_and_overlap() {
        let a = Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(4.0, 4.0)));
        let b = Curve2::Line(Line2::new(DVec2::new(0.0, 4.0), DVec2::new(4.0, 0.0)));
        let h = intersect(&a, &b);
        assert_eq!(h.len(), 1);
        assert!((h[0].p - DVec2::new(2.0, 2.0)).length() < 1e-12);
        assert!((h[0].ta - 0.5).abs() < 1e-12 && (h[0].tb - 0.5).abs() < 1e-12);
        // touching at endpoint
        let c = Curve2::Line(Line2::new(DVec2::new(4.0, 4.0), DVec2::new(8.0, 0.0)));
        assert_eq!(intersect(&a, &c).len(), 1);
        // collinear overlap: endpoints of the overlap
        let d = Curve2::Line(Line2::new(DVec2::new(2.0, 2.0), DVec2::new(6.0, 6.0)));
        let h = intersect(&a, &d);
        assert_eq!(h.len(), 2, "{h:?}");
        check(&a, &d, &h);
        // disjoint parallel
        let e = Curve2::Line(Line2::new(DVec2::new(0.0, 1.0), DVec2::new(4.0, 5.0)));
        assert!(intersect(&a, &e).is_empty());
    }

    #[test]
    fn line_circle_arc() {
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0));
        let l = Curve2::Line(Line2::new(DVec2::new(-10.0, 3.0), DVec2::new(10.0, 3.0)));
        let h = intersect(&l, &c);
        assert_eq!(h.len(), 2);
        check(&l, &c, &h);
        assert!((h[0].p - DVec2::new(-4.0, 3.0)).length() < 1e-12);
        // tangent
        let t = Curve2::Line(Line2::new(DVec2::new(-10.0, 5.0), DVec2::new(10.0, 5.0)));
        let h = intersect(&t, &c);
        assert_eq!(h.len(), 1);
        assert!((h[0].p - DVec2::new(0.0, 5.0)).length() < 1e-12);
        // arc range filter: upper half only keeps both, lower half none
        let up = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, PI));
        assert_eq!(intersect(&l, &up).len(), 2);
        let down = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, PI, 0.0));
        assert_eq!(intersect(&l, &down).len(), 0);
        let quarter = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, FRAC_PI_2));
        let h = intersect(&quarter, &l);
        assert_eq!(h.len(), 1);
        check(&quarter, &l, &h);
    }

    #[test]
    fn circle_circle_cases() {
        let a = Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0));
        let b = Curve2::Circle(Circle2::new(DVec2::new(6.0, 0.0), 5.0));
        let h = intersect(&a, &b);
        assert_eq!(h.len(), 2);
        check(&a, &b, &h);
        let ext = Curve2::Circle(Circle2::new(DVec2::new(10.0, 0.0), 5.0));
        assert_eq!(intersect(&a, &ext).len(), 1);
        let int = Curve2::Circle(Circle2::new(DVec2::new(2.0, 0.0), 3.0));
        let h = intersect(&a, &int);
        assert_eq!(h.len(), 1);
        assert!((h[0].p - DVec2::new(5.0, 0.0)).length() < 1e-12);
        assert!(intersect(&a, &Curve2::Circle(Circle2::new(DVec2::ZERO, 2.0))).is_empty());
        // coincident arcs overlapping: overlap endpoints
        let a1 = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, 2.0));
        let a2 = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 1.0, 3.0));
        let h = intersect(&a1, &a2);
        assert_eq!(h.len(), 2, "{h:?}");
        check(&a1, &a2, &h);
    }

    #[test]
    fn ellipse_cases() {
        let e = Curve2::Ellipse(EllipseArc2 {
            c: DVec2::new(1.0, 1.0),
            major: DVec2::new(4.0, 0.0),
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        });
        let l = Curve2::Line(Line2::new(DVec2::new(-10.0, 1.0), DVec2::new(10.0, 1.0)));
        let h = intersect(&e, &l);
        assert_eq!(h.len(), 2);
        check(&e, &l, &h);
        assert!((h[0].p.x - 5.0).abs() < 1e-12 || (h[0].p.x + 3.0).abs() < 1e-12);
        // tangent line y = 3
        let t = Curve2::Line(Line2::new(DVec2::new(-10.0, 3.0), DVec2::new(10.0, 3.0)));
        assert_eq!(intersect(&e, &t).len(), 1);
        // circle through ellipse: 4 points
        let c = Curve2::Circle(Circle2::new(DVec2::new(1.0, 1.0), 3.0));
        let h = intersect(&e, &c);
        assert_eq!(h.len(), 4, "{h:?}");
        check(&e, &c, &h);
        // circle tangent to ellipse at the minor vertex (inside)
        let ct = Curve2::Circle(Circle2::new(DVec2::new(1.0, 1.0), 2.0));
        let h = intersect(&e, &ct);
        assert_eq!(h.len(), 2, "{h:?}");
        // ellipse / rotated ellipse
        let e2 = Curve2::Ellipse(EllipseArc2 {
            c: DVec2::new(1.0, 1.0),
            major: DVec2::new(0.0, 4.0),
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        });
        let h = intersect(&e, &e2);
        assert_eq!(h.len(), 4, "{h:?}");
        check(&e, &e2, &h);
        // partial ellipse arc
        let ea = Curve2::Ellipse(EllipseArc2 {
            c: DVec2::new(1.0, 1.0),
            major: DVec2::new(4.0, 0.0),
            ratio: 0.5,
            start: 0.0,
            end: PI,
        });
        let h = intersect(&ea, &e2);
        assert_eq!(h.len(), 2, "{h:?}");
        check(&ea, &e2, &h);
    }

    #[test]
    fn polyline_and_spline() {
        let pl = Curve2::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::new(-5.0, 0.0), 1.0),
                PolyVertex::with_bulge(DVec2::new(5.0, 0.0), 0.0),
                PolyVertex::new(DVec2::new(5.0, 5.0)),
            ],
            closed: false,
        });
        let l = Curve2::Line(Line2::new(DVec2::new(0.0, -10.0), DVec2::new(0.0, 10.0)));
        let h = intersect(&pl, &l);
        assert_eq!(h.len(), 1, "{h:?}");
        check(&pl, &l, &h);
        assert!((h[0].p - DVec2::new(0.0, -5.0)).length() < 1e-12);
        let sp = Curve2::Spline(
            Nurbs2::from_fit_points(
                &[
                    DVec2::new(-6.0, -6.0),
                    DVec2::new(-2.0, 3.0),
                    DVec2::new(2.0, -3.0),
                    DVec2::new(6.0, 6.0),
                ],
                3,
            )
            .unwrap(),
        );
        let h = intersect(&sp, &l);
        check(&sp, &l, &h);
        assert_eq!(h.len(), brute_count(&sp, &l));
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 4.0));
        let h = intersect(&sp, &c);
        check(&sp, &c, &h);
        assert_eq!(h.len(), brute_count(&sp, &c), "{h:?}");
        let h = intersect(&sp, &pl);
        check(&sp, &pl, &h);
        assert_eq!(h.len(), brute_count(&sp, &pl), "{h:?}");
        // spline tangent to a line: y = max of a symmetric bump
        let bump = Curve2::Spline(Nurbs2 {
            degree: 2,
            ctrl: vec![
                DVec2::new(-2.0, 0.0),
                DVec2::new(0.0, 2.0),
                DVec2::new(2.0, 0.0),
            ],
            weights: vec![],
            knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            fit_points: vec![],
            closed: false,
        });
        let top = Curve2::Line(Line2::new(DVec2::new(-3.0, 1.0), DVec2::new(3.0, 1.0)));
        let h = intersect(&bump, &top);
        assert_eq!(h.len(), 1, "{h:?}");
        assert!((h[0].p - DVec2::new(0.0, 1.0)).length() < 1e-6);
    }

    #[test]
    fn brute_force_property() {
        // pseudo-random lines/arcs/circles/ellipses: count must match brute force
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f64 / 10_000.0
        };
        let mut curves = Vec::new();
        for i in 0..24 {
            let c = DVec2::new(rnd() * 10.0, rnd() * 10.0);
            curves.push(match i % 4 {
                0 => Curve2::Line(Line2::new(c, DVec2::new(rnd() * 10.0, rnd() * 10.0))),
                1 => Curve2::Arc(Arc2::new(c, 1.0 + rnd() * 4.0, rnd() * 6.0, rnd() * 6.0)),
                2 => Curve2::Circle(Circle2::new(c, 0.5 + rnd() * 4.0)),
                _ => Curve2::Ellipse(EllipseArc2 {
                    c,
                    major: DVec2::from_angle(rnd() * 6.0) * (1.0 + rnd() * 4.0),
                    ratio: 0.2 + rnd() * 0.8,
                    start: rnd() * 6.0,
                    end: rnd() * 6.0,
                }),
            });
        }
        let mut total = 0;
        for i in 0..curves.len() {
            for j in i + 1..curves.len() {
                let h = intersect(&curves[i], &curves[j]);
                check(&curves[i], &curves[j], &h);
                let bc = brute_count(&curves[i], &curves[j]);
                assert_eq!(h.len(), bc, "{:?} x {:?}: {h:?}", curves[i], curves[j]);
                total += h.len();
            }
        }
        assert!(total > 30, "{total}");
    }
}
