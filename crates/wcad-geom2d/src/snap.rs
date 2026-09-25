//! Object snap candidates.
//!
//! Point-set snaps ([`snap_points`]): endpoint, midpoint, center, quadrant, node. Reference-point
//! snaps: [`nearest`], [`perpendicular_feet`], [`tangent_points`]. Intersection snaps use
//! [`crate::intersect`]. `Point` entities (the usual node targets) are handled by the caller.

use std::f64::consts::{FRAC_PI_2, TAU};

use wcad_math::{DVec2, cross2, perp};

use crate::bulge::PolySegment;
use crate::curve::{Curve, arc_param};
use crate::curves::{Arc2, Curve2, EllipseArc2, Line2, Nurbs2};
use crate::numeric::find_roots;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapKind {
    Endpoint,
    Midpoint,
    Center,
    Quadrant,
    /// Spline fit points (for curves; point entities are snapped by the caller).
    Node,
}

/// Snap candidates of `kind` on `curve`.
pub fn snap_points(curve: &Curve2, kind: SnapKind) -> Vec<DVec2> {
    match kind {
        SnapKind::Endpoint => endpoints(curve),
        SnapKind::Midpoint => midpoints(curve),
        SnapKind::Center => centers(curve),
        SnapKind::Quadrant => quadrants(curve),
        SnapKind::Node => match curve {
            Curve2::Spline(s) => s.fit_points.clone(),
            _ => Vec::new(),
        },
    }
}

/// Alias used by the architecture document.
pub fn snap_candidates(curve: &Curve2, kind: SnapKind) -> Vec<DVec2> {
    snap_points(curve, kind)
}

fn endpoints(c: &Curve2) -> Vec<DVec2> {
    match c {
        Curve2::Circle(_) => Vec::new(),
        Curve2::Ellipse(e) if e.is_full() => Vec::new(),
        Curve2::Polyline(p) => p.verts.iter().map(|v| v.p).collect(),
        _ if c.is_closed() => vec![c.start()],
        _ => vec![c.start(), c.end()],
    }
}

fn midpoints(c: &Curve2) -> Vec<DVec2> {
    match c {
        Curve2::Line(l) => vec![l.midpoint()],
        Curve2::Arc(a) => vec![a.mid_point()],
        Curve2::Circle(_) => Vec::new(),
        Curve2::Ellipse(e) if e.is_full() => Vec::new(),
        Curve2::Polyline(p) => p.segments().map(|s| s.point_at(0.5)).collect(),
        _ => {
            let l = c.length();
            vec![c.point_at(c.param_at_length(0.5 * l))]
        }
    }
}

fn centers(c: &Curve2) -> Vec<DVec2> {
    match c {
        Curve2::Circle(ci) => vec![ci.c],
        Curve2::Arc(a) => vec![a.c],
        Curve2::Ellipse(e) => vec![e.c],
        Curve2::Polyline(p) => p
            .segments()
            .filter_map(|s| match s {
                PolySegment::Arc(a) => Some(a.c),
                PolySegment::Line(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn quadrants(c: &Curve2) -> Vec<DVec2> {
    let circ = |cc: DVec2, r: f64, range: Option<(f64, f64)>| -> Vec<DVec2> {
        (0..4)
            .filter_map(|k| {
                let ang = k as f64 * FRAC_PI_2;
                match range {
                    None => Some(ang),
                    Some((s, sw)) => arc_param(s, sw, ang, 1e-12),
                }
                .map(|a| cc + DVec2::new(a.cos(), a.sin()) * r)
            })
            .collect()
    };
    match c {
        Curve2::Circle(ci) => circ(ci.c, ci.r, None),
        Curve2::Arc(a) => circ(a.c, a.r, Some((a.start, a.sweep()))),
        Curve2::Ellipse(e) => {
            // ends of the principal axes
            let (s, sw) = (e.start, e.sweep());
            (0..4)
                .filter_map(|k| {
                    let t = k as f64 * FRAC_PI_2;
                    if e.is_full() {
                        Some(t)
                    } else {
                        arc_param(s, sw, t, 1e-12)
                    }
                    .map(|t| e.at_param(t))
                })
                .collect()
        }
        Curve2::Polyline(p) => p
            .segments()
            .flat_map(|s| match s {
                PolySegment::Arc(a) => {
                    let ccw = a.to_arc();
                    circ(ccw.c, ccw.r, Some((ccw.start, ccw.sweep())))
                }
                PolySegment::Line(_) => Vec::new(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Nearest point on the curve.
pub fn nearest(curve: &Curve2, p: DVec2) -> DVec2 {
    curve.closest(p).1
}

/// Feet of perpendiculars from `p` onto the curve (points where `p − C(t)` is normal to the
/// curve). For lines the foot on the infinite line is returned only when it lies on the segment.
pub fn perpendicular_feet(curve: &Curve2, p: DVec2) -> Vec<DVec2> {
    match curve {
        Curve2::Line(l) => line_foot(l, p).into_iter().collect(),
        Curve2::Circle(ci) => {
            let d = p - ci.c;
            if d.length_squared() < 1e-300 {
                return Vec::new();
            }
            let u = d.normalize();
            vec![ci.c + u * ci.r, ci.c - u * ci.r]
        }
        Curve2::Arc(a) => arc_feet(a, p),
        Curve2::Polyline(pl) => {
            let mut out = Vec::new();
            for s in pl.segments() {
                match s {
                    PolySegment::Line(l) => out.extend(line_foot(&l, p)),
                    PolySegment::Arc(a) => out.extend(arc_feet(&a.to_arc(), p)),
                }
            }
            out
        }
        _ => curve.with_dyn(|c| {
            // roots of g(t) = (C(t) − p)·C'(t)
            let (a, b) = c.domain();
            let g = |t: f64| (c.point_at(t) - p).dot(c.deriv_at(t));
            let dg = |t: f64| {
                let d1 = c.deriv_at(t);
                d1.dot(d1) + (c.point_at(t) - p).dot(c.deriv2_at(t))
            };
            let n = sample_count(curve);
            let scale = c.bbox().size().length().max(1e-300);
            let mut out: Vec<DVec2> = Vec::new();
            for t in find_roots(&g, &dg, a, b, n, 1e-12 * scale * scale, 1e-9 * (b - a)) {
                let q = c.point_at(t);
                if !out.iter().any(|o| o.distance(q) <= 1e-9 * scale) {
                    out.push(q);
                }
            }
            out
        }),
    }
}

fn line_foot(l: &Line2, p: DVec2) -> Option<DVec2> {
    let d = l.b - l.a;
    let l2 = d.length_squared();
    if l2 < 1e-300 {
        return None;
    }
    let t = (p - l.a).dot(d) / l2;
    (-1e-12..=1.0 + 1e-12)
        .contains(&t)
        .then(|| l.at(t.clamp(0.0, 1.0)))
}

fn arc_feet(a: &Arc2, p: DVec2) -> Vec<DVec2> {
    let d = p - a.c;
    if d.length_squared() < 1e-300 {
        return Vec::new();
    }
    let ang = d.y.atan2(d.x);
    [ang, ang + std::f64::consts::PI]
        .into_iter()
        .filter_map(|x| arc_param(a.start, a.sweep(), x, 1e-12))
        .map(|t| a.at_angle(t))
        .collect()
}

fn sample_count(c: &Curve2) -> usize {
    match c {
        Curve2::Spline(s) => (s.ctrl.len() * 8).clamp(32, 4096),
        _ => 128,
    }
}

/// Points of tangency on the curve for lines through the external point `p` (circles, arcs,
/// ellipses, arc segments of polylines, splines).
pub fn tangent_points(curve: &Curve2, p: DVec2) -> Vec<DVec2> {
    let circ = |c: DVec2, r: f64, range: Option<(f64, f64)>| -> Vec<DVec2> {
        let d = p - c;
        let dist = d.length();
        if dist <= r * (1.0 + 1e-12) {
            return Vec::new();
        }
        let base = d.y.atan2(d.x);
        let off = (r / dist).acos();
        [base + off, base - off]
            .into_iter()
            .filter_map(|ang| match range {
                None => Some(ang),
                Some((s, sw)) => arc_param(s, sw, ang, 1e-12),
            })
            .map(|ang| c + DVec2::new(ang.cos(), ang.sin()) * r)
            .collect()
    };
    match curve {
        Curve2::Line(_) => Vec::new(),
        Curve2::Circle(c) => circ(c.c, c.r, None),
        Curve2::Arc(a) => circ(a.c, a.r, Some((a.start, a.sweep()))),
        Curve2::Ellipse(e) => ellipse_tangents(e, p),
        Curve2::Polyline(pl) => pl
            .segments()
            .flat_map(|s| match s {
                PolySegment::Arc(a) => {
                    let c = a.to_arc();
                    circ(c.c, c.r, Some((c.start, c.sweep())))
                }
                PolySegment::Line(_) => Vec::new(),
            })
            .collect(),
        Curve2::Spline(s) => spline_tangents(s, p, curve),
    }
}

/// Tangency is affine invariant: map the ellipse to the unit circle, solve there, map back.
fn ellipse_tangents(e: &EllipseArc2, p: DVec2) -> Vec<DVec2> {
    let m = e.unit_map();
    if m.matrix2.determinant().abs() < 1e-300 {
        return Vec::new();
    }
    let q = m.inverse().transform_point2(p);
    let dist = q.length();
    if dist <= 1.0 + 1e-12 {
        return Vec::new();
    }
    let base = q.y.atan2(q.x);
    let off = (1.0 / dist).acos();
    [base + off, base - off]
        .into_iter()
        .filter_map(|t| {
            if e.is_full() {
                Some(t)
            } else {
                arc_param(e.start, e.sweep(), t, 1e-12)
            }
        })
        .map(|t| e.at_param(t))
        .collect()
}

fn spline_tangents(s: &Nurbs2, p: DVec2, curve: &Curve2) -> Vec<DVec2> {
    curve.with_dyn(|c| {
        let (a, b) = c.domain();
        // roots of cross(C(t) − p, C'(t))
        let f = |t: f64| cross2(c.point_at(t) - p, c.deriv_at(t));
        let df = |t: f64| cross2(c.point_at(t) - p, c.deriv2_at(t));
        let scale = c.bbox().size().length().max(1e-300);
        let n = (s.ctrl.len() * 8).clamp(32, 4096);
        find_roots(&f, &df, a, b, n, 1e-12 * scale * scale, 1e-9 * (b - a))
            .into_iter()
            .map(|t| c.point_at(t))
            .filter(|q| q.distance(p) > 1e-9 * scale)
            .collect()
    })
}

/// Tangent line directions helper: unit normal at the closest point (used by the "perpendicular"
/// snap marker orientation).
pub fn normal_at_closest(curve: &Curve2, p: DVec2) -> DVec2 {
    let (t, _) = curve.closest(p);
    perp(curve.tangent_at(t))
}

/// Parameter angle helper used by grips: angle of `p` around `c` in `[0, 2π)`.
pub fn angle_around(c: DVec2, p: DVec2) -> f64 {
    let d = p - c;
    d.y.atan2(d.x).rem_euclid(TAU)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::*;
    use std::f64::consts::PI;

    #[test]
    fn point_snaps() {
        let l = Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(4.0, 2.0)));
        assert_eq!(
            snap_points(&l, SnapKind::Endpoint),
            vec![DVec2::ZERO, DVec2::new(4.0, 2.0)]
        );
        assert_eq!(
            snap_points(&l, SnapKind::Midpoint),
            vec![DVec2::new(2.0, 1.0)]
        );
        let a = Curve2::Arc(Arc2::new(DVec2::ZERO, 2.0, -0.5, 2.0));
        let q = snap_points(&a, SnapKind::Quadrant);
        assert_eq!(q.len(), 2); // 0 and π/2
        assert!((q[0] - DVec2::new(2.0, 0.0)).length() < 1e-12);
        assert_eq!(snap_points(&a, SnapKind::Center), vec![DVec2::ZERO]);
        let c = Curve2::Circle(Circle2::new(DVec2::ONE, 1.0));
        assert_eq!(snap_points(&c, SnapKind::Quadrant).len(), 4);
        assert!(snap_points(&c, SnapKind::Endpoint).is_empty());
        let e = Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(0.0, 3.0),
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        });
        let q = snap_points(&e, SnapKind::Quadrant);
        assert_eq!(q.len(), 4);
        assert!((q[0] - DVec2::new(0.0, 3.0)).length() < 1e-12);
        assert!((q[1] - DVec2::new(-1.5, 0.0)).length() < 1e-12);
        let pl = Curve2::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::new(-1.0, 0.0), 1.0),
                PolyVertex::new(DVec2::new(1.0, 0.0)),
                PolyVertex::new(DVec2::new(1.0, 3.0)),
            ],
            closed: false,
        });
        let m = snap_points(&pl, SnapKind::Midpoint);
        assert!((m[0] - DVec2::new(0.0, -1.0)).length() < 1e-12);
        assert!((m[1] - DVec2::new(1.0, 1.5)).length() < 1e-12);
        assert_eq!(snap_points(&pl, SnapKind::Center), vec![DVec2::ZERO]);
        let sp = Nurbs2::from_fit_points(
            &[DVec2::ZERO, DVec2::new(1.0, 1.0), DVec2::new(2.0, 0.0)],
            3,
        )
        .unwrap();
        let s = Curve2::Spline(sp);
        let mid = snap_points(&s, SnapKind::Midpoint)[0];
        assert!((mid.x - 1.0).abs() < 1e-6, "{mid}"); // symmetric curve
        assert_eq!(snap_points(&s, SnapKind::Node).len(), 3);
    }

    #[test]
    fn perpendicular_and_tangent() {
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0));
        let p = DVec2::new(10.0, 0.0);
        let t = tangent_points(&c, p);
        assert_eq!(t.len(), 2);
        for q in &t {
            assert!(((*q - p).dot(*q)).abs() < 1e-9); // radius ⟂ tangent line
        }
        let f = perpendicular_feet(&c, p);
        assert_eq!(f.len(), 2);
        assert!((f[0] - DVec2::new(5.0, 0.0)).length() < 1e-12);
        let l = Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0)));
        assert_eq!(
            perpendicular_feet(&l, DVec2::new(3.0, 4.0)),
            vec![DVec2::new(3.0, 0.0)]
        );
        assert!(perpendicular_feet(&l, DVec2::new(13.0, 4.0)).is_empty());
        // ellipse tangents: tangent line through p touches the ellipse
        let e = EllipseArc2 {
            c: DVec2::new(1.0, 1.0),
            major: DVec2::new(3.0, 1.0),
            ratio: 0.4,
            start: 0.0,
            end: TAU,
        };
        let ec = Curve2::Ellipse(e);
        let p = DVec2::new(8.0, 5.0);
        let t = tangent_points(&ec, p);
        assert_eq!(t.len(), 2);
        for q in &t {
            let (tt, _) = ec.closest(*q);
            let dir = ec.tangent_at(tt);
            assert!(cross2(dir, (p - *q).normalize()).abs() < 1e-9);
        }
        let f = perpendicular_feet(&ec, p);
        assert!(!f.is_empty());
        for q in &f {
            let (tt, _) = ec.closest(*q);
            assert!(ec.tangent_at(tt).dot((p - *q).normalize()).abs() < 1e-6);
        }
        // arc restricts tangents
        let a = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, PI));
        assert_eq!(tangent_points(&a, DVec2::new(10.0, 0.0)).len(), 1);
        // spline tangents from an external point
        let s = Curve2::Spline(
            Nurbs2::from_fit_points(
                &[
                    DVec2::new(-3.0, 0.0),
                    DVec2::new(0.0, 2.0),
                    DVec2::new(3.0, 0.0),
                ],
                3,
            )
            .unwrap(),
        );
        let t = tangent_points(&s, DVec2::new(0.0, 2.5));
        assert_eq!(t.len(), 2, "{t:?}");
    }
}
