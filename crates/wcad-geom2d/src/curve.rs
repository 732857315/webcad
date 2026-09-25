//! Curve evaluation: the [`Curve`] trait and its implementations for every curve type.
//!
//! Parameter domains (all methods clamp parameters into the domain):
//! - [`Line2`]: `t ∈ [0, 1]`, `a + t·(b − a)`.
//! - [`Circle2`]: `t ∈ [0, 2π]`, angle from +X.
//! - [`Arc2`]: `t ∈ [start, start + sweep]`, angle from +X (not normalized).
//! - [`EllipseArc2`]: `t ∈ [start, start + sweep]`, parametric angle.
//! - [`Polyline2`]: `t ∈ [0, segment_count]`; segment `i` covers `[i, i+1]`, linear in length for
//!   straight segments and in angle for arc segments.
//! - [`Nurbs2`]: `t ∈ [knots[p], knots[n]]`.

use std::f64::consts::TAU;

use wcad_math::{BBox2, DAffine2, DVec2, ccw_sweep, cross2, normalize_0_2pi};

use crate::bulge::{BulgeArc, PolySegment};
use crate::curves::{Arc2, Circle2, Curve2, EllipseArc2, Line2, Nurbs2, PolyVertex, Polyline2};
use crate::numeric::SafeClamp;
use crate::numeric::integrate;

/// Common curve interface. Methods returning curves return [`Curve2`] because some operations
/// change the curve kind (e.g. a non-uniformly scaled circle becomes an ellipse, a reversed arc
/// becomes a single-segment polyline with a negative bulge).
pub trait Curve {
    /// Parameter domain `(t0, t1)`, `t0 <= t1`.
    fn domain(&self) -> (f64, f64);
    /// Point at parameter `t`.
    fn point_at(&self, t: f64) -> DVec2;
    /// First derivative `dC/dt`.
    fn deriv_at(&self, t: f64) -> DVec2;
    /// Second derivative `d²C/dt²`.
    fn deriv2_at(&self, t: f64) -> DVec2;
    /// `true` for closed curves (circle, full ellipse, closed polyline/spline).
    fn is_closed(&self) -> bool;
    /// Exact (lines, arcs, ellipses) or tight (splines) axis-aligned bounds.
    fn bbox(&self) -> BBox2;
    /// Arc length between two parameters (`t0 <= t1`).
    fn length_between(&self, t0: f64, t1: f64) -> f64;
    /// Closest point on the curve: `(parameter, point)`.
    fn closest(&self, p: DVec2) -> (f64, DVec2);
    /// Polyline approximation with parameters, chord error `<= tol`. Includes both ends.
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)>;
    /// The piece between `t0` and `t1`. For closed curves `t1 < t0` wraps around the seam.
    /// `None` when the piece would be degenerate.
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2>;
    /// The same point set traversed in the opposite direction.
    fn reversed(&self) -> Curve2;
    /// Affine image.
    fn transformed(&self, m: &DAffine2) -> Curve2;

    /// Unit tangent (zero at degenerate points).
    fn tangent_at(&self, t: f64) -> DVec2 {
        self.deriv_at(t).normalize_or_zero()
    }
    /// Signed curvature (positive = turning left).
    fn curvature_at(&self, t: f64) -> f64 {
        let d1 = self.deriv_at(t);
        let d2 = self.deriv2_at(t);
        let l = d1.length();
        if l < 1e-300 {
            0.0
        } else {
            cross2(d1, d2) / (l * l * l)
        }
    }
    /// Alias of [`Curve::point_at`].
    fn eval(&self, t: f64) -> DVec2 {
        self.point_at(t)
    }
    /// Alias of [`Curve::tangent_at`].
    fn tangent(&self, t: f64) -> DVec2 {
        self.tangent_at(t)
    }
    fn start(&self) -> DVec2 {
        self.point_at(self.domain().0)
    }
    fn end(&self) -> DVec2 {
        self.point_at(self.domain().1)
    }
    fn length(&self) -> f64 {
        let (a, b) = self.domain();
        self.length_between(a, b)
    }
    /// Polyline approximation with chord error `<= tol`.
    fn flatten(&self, tol: f64) -> Vec<DVec2> {
        self.flatten_params(tol)
            .into_iter()
            .map(|(_, p)| p)
            .collect()
    }
    /// Split an open curve at `t` (strictly inside the domain).
    fn split(&self, t: f64) -> Option<(Curve2, Curve2)> {
        let (a, b) = self.domain();
        if self.is_closed() || !(t > a && t < b) {
            return None;
        }
        Some((self.sub_curve(a, t)?, self.sub_curve(t, b)?))
    }
    /// Parameter at arc length `s` from the start (clamped).
    fn param_at_length(&self, s: f64) -> f64 {
        let (a, b) = self.domain();
        let total = self.length_between(a, b);
        if !(s > 0.0) || total <= 0.0 {
            return a;
        }
        if s >= total {
            return b;
        }
        // Newton with bisection safeguard on L(t) - s.
        let (mut lo, mut hi) = (a, b);
        let mut t = a + (b - a) * s / total;
        for _ in 0..60 {
            let f = self.length_between(a, t) - s;
            if f.abs() <= 1e-12 * total {
                return t;
            }
            if f > 0.0 {
                hi = t;
            } else {
                lo = t;
            }
            let d = self.deriv_at(t).length();
            let nt = if d > 1e-300 { t - f / d } else { f64::NAN };
            t = if nt > lo && nt < hi {
                nt
            } else {
                0.5 * (lo + hi)
            };
            if hi - lo <= 1e-15 * (1.0 + b.abs()) {
                break;
            }
        }
        t
    }
    /// Clamp `t` into the domain.
    fn clamp_param(&self, t: f64) -> f64 {
        let (a, b) = self.domain();
        if t.is_finite() { t.sclamp(a, b) } else { a }
    }
}

// ------------------------------------------------------------------------------------------------
// helpers

/// Segment count so that a circular arc of radius `r` and sweep `sweep` has chord error `<= tol`.
pub(crate) fn arc_segments(r: f64, sweep: f64, tol: f64) -> usize {
    let sweep = sweep.abs();
    if !(r > 0.0) || !sweep.is_finite() {
        return 1;
    }
    let tol = if tol > 0.0 { tol } else { r * 1e-3 };
    let step = if tol >= r {
        std::f64::consts::FRAC_PI_2
    } else {
        2.0 * (1.0 - tol / r).acos()
    };
    let step = step.clamp(1e-4, std::f64::consts::FRAC_PI_2);
    ((sweep / step).ceil() as usize).clamp(1, 16384)
}

/// Adaptive sampling of `f` over the parameter breaks `breaks`, each interval first cut into `init`
/// pieces. Checks chord deviation at the 1/4, 1/2 and 3/4 points.
pub(crate) fn adaptive_flatten(
    f: &dyn Fn(f64) -> DVec2,
    breaks: &[f64],
    init: usize,
    tol: f64,
) -> Vec<(f64, DVec2)> {
    let mut out: Vec<(f64, DVec2)> = Vec::new();
    if breaks.len() < 2 {
        if let Some(&t) = breaks.first() {
            out.push((t, f(t)));
        }
        return out;
    }
    let init = init.clamp(1, 1024);
    let budget = 200_000usize;
    out.push((breaks[0], f(breaks[0])));
    for w in breaks.windows(2) {
        let (a, b) = (w[0], w[1]);
        if !(b > a) {
            continue;
        }
        let h = (b - a) / init as f64;
        for i in 0..init {
            let t0 = a + h * i as f64;
            let t1 = if i + 1 == init { b } else { t0 + h };
            let p0 = out.last().map(|x| x.1).unwrap_or_else(|| f(t0));
            let p1 = f(t1);
            subdivide(f, t0, p0, t1, p1, tol, 0, &mut out, budget);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn subdivide(
    f: &dyn Fn(f64) -> DVec2,
    t0: f64,
    p0: DVec2,
    t1: f64,
    p1: DVec2,
    tol: f64,
    depth: u32,
    out: &mut Vec<(f64, DVec2)>,
    budget: usize,
) {
    let tm = 0.5 * (t0 + t1);
    let pm = f(tm);
    let flat = depth >= 14 || out.len() >= budget || {
        let q1 = f(0.5 * (t0 + tm));
        let q3 = f(0.5 * (tm + t1));
        dist_to_segment(pm, p0, p1) <= tol
            && dist_to_segment(q1, p0, p1) <= tol
            && dist_to_segment(q3, p0, p1) <= tol
    };
    if flat {
        out.push((t1, p1));
    } else {
        subdivide(f, t0, p0, tm, pm, tol, depth + 1, out, budget);
        subdivide(f, tm, pm, t1, p1, tol, depth + 1, out, budget);
    }
}

pub(crate) fn dist_to_segment(p: DVec2, a: DVec2, b: DVec2) -> f64 {
    let d = b - a;
    let l2 = d.length_squared();
    if l2 < 1e-300 {
        return p.distance(a);
    }
    let t = ((p - a).dot(d) / l2).clamp(0.0, 1.0);
    p.distance(a + d * t)
}

/// Map angle `ang` onto the arc parameter range `[start, start + sweep]`; `None` if outside by
/// more than `eps` radians.
pub(crate) fn arc_param(start: f64, sweep: f64, ang: f64, eps: f64) -> Option<f64> {
    let d = normalize_0_2pi(ang - start);
    if d <= sweep + eps {
        Some(start + d.min(sweep))
    } else if d >= TAU - eps {
        Some(start)
    } else {
        None
    }
}

/// Newton refinement of the closest-point condition `(C(t) − p)·C'(t) = 0` inside `[a, b]`.
pub(crate) fn refine_closest(c: &dyn Curve, p: DVec2, mut t: f64, a: f64, b: f64) -> f64 {
    for _ in 0..40 {
        let q = c.point_at(t) - p;
        let d1 = c.deriv_at(t);
        let d2 = c.deriv2_at(t);
        let g = q.dot(d1);
        let dg = d1.dot(d1) + q.dot(d2);
        if dg.abs() < 1e-300 {
            break;
        }
        let mut nt = (t - g / dg).sclamp(a, b);
        // guard: never accept a step that increases the distance much
        if (c.point_at(nt) - p).length_squared() > q.length_squared() * (1.0 + 1e-12) {
            nt = (t + nt) * 0.5;
            if (c.point_at(nt) - p).length_squared() > q.length_squared() {
                break;
            }
        }
        if (nt - t).abs() <= 1e-15 * (1.0 + t.abs()) {
            t = nt;
            break;
        }
        t = nt;
    }
    t
}

/// Generic closest point by sampling `n` intervals over the breaks plus Newton refinement.
pub(crate) fn closest_by_sampling(
    c: &dyn Curve,
    p: DVec2,
    breaks: &[f64],
    per: usize,
) -> (f64, DVec2) {
    let mut samples: Vec<(f64, f64)> = Vec::new();
    for w in breaks.windows(2) {
        let (a, b) = (w[0], w[1]);
        for i in 0..=per {
            let t = a + (b - a) * i as f64 / per as f64;
            samples.push((t, c.point_at(t).distance_squared(p)));
        }
    }
    let (d0, d1) = c.domain();
    if samples.is_empty() {
        return (d0, c.point_at(d0));
    }
    // refine the best few local minima
    let mut cand: Vec<usize> = (0..samples.len())
        .filter(|&i| {
            let l = if i > 0 {
                samples[i - 1].1
            } else {
                f64::INFINITY
            };
            let r = if i + 1 < samples.len() {
                samples[i + 1].1
            } else {
                f64::INFINITY
            };
            samples[i].1 <= l && samples[i].1 <= r
        })
        .collect();
    cand.sort_by(|a, b| samples[*a].1.total_cmp(&samples[*b].1));
    cand.truncate(4);
    let mut best = (samples[0].0, f64::INFINITY);
    for i in cand {
        let lo = if i > 0 { samples[i - 1].0 } else { d0 };
        let hi = if i + 1 < samples.len() {
            samples[i + 1].0
        } else {
            d1
        };
        let t = refine_closest(c, p, samples[i].0, lo.min(hi), hi.max(lo));
        let d = c.point_at(t).distance_squared(p);
        if d < best.1 {
            best = (t, d);
        }
    }
    (best.0, c.point_at(best.0))
}

/// Similarity decomposition of the linear part: `Some((scale, rotation, reflected))`.
pub(crate) fn similarity(m: &DAffine2) -> Option<(f64, f64, bool)> {
    let x = m.matrix2.x_axis;
    let y = m.matrix2.y_axis;
    let (lx, ly) = (x.length(), y.length());
    if lx < 1e-300 || ly < 1e-300 {
        return None;
    }
    if (lx - ly).abs() > 1e-12 * lx.max(ly) || x.dot(y).abs() > 1e-12 * lx * ly {
        return None;
    }
    let det = cross2(x, y);
    Some((lx, x.y.atan2(x.x), det < 0.0))
}

fn default_tol(bb: &BBox2) -> f64 {
    (bb.size().length() * 1e-4).max(1e-9)
}

fn sane_tol(tol: f64, fallback: impl FnOnce() -> f64) -> f64 {
    if tol > 0.0 && tol.is_finite() {
        tol
    } else {
        fallback()
    }
}

// ------------------------------------------------------------------------------------------------
// Line2

impl Curve for Line2 {
    fn domain(&self) -> (f64, f64) {
        (0.0, 1.0)
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.at(t)
    }
    fn deriv_at(&self, _t: f64) -> DVec2 {
        self.b - self.a
    }
    fn deriv2_at(&self, _t: f64) -> DVec2 {
        DVec2::ZERO
    }
    fn is_closed(&self) -> bool {
        false
    }
    fn bbox(&self) -> BBox2 {
        BBox2::new(self.a, self.b)
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        (t1 - t0).abs() * self.length()
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let d = self.b - self.a;
        let l2 = d.length_squared();
        let t = if l2 < 1e-300 {
            0.0
        } else {
            ((p - self.a).dot(d) / l2).clamp(0.0, 1.0)
        };
        (t, self.at(t))
    }
    fn flatten_params(&self, _tol: f64) -> Vec<(f64, DVec2)> {
        vec![(0.0, self.a), (1.0, self.b)]
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let (t0, t1) = (t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0));
        (t1 > t0).then(|| Curve2::Line(Line2::new(self.at(t0), self.at(t1))))
    }
    fn reversed(&self) -> Curve2 {
        Curve2::Line(Line2::new(self.b, self.a))
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        Curve2::Line(Line2::new(
            m.transform_point2(self.a),
            m.transform_point2(self.b),
        ))
    }
}

// ------------------------------------------------------------------------------------------------
// Circle2

impl Circle2 {
    /// Clockwise traversal as a closed two-segment bulge polyline.
    pub fn to_polyline(&self, ccw: bool) -> Polyline2 {
        let b = if ccw { 1.0 } else { -1.0 };
        Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(self.c + DVec2::new(self.r, 0.0), b),
                PolyVertex::with_bulge(self.c - DVec2::new(self.r, 0.0), b),
            ],
            closed: true,
        }
    }
    fn as_ellipse(&self) -> EllipseArc2 {
        EllipseArc2 {
            c: self.c,
            major: DVec2::new(self.r, 0.0),
            ratio: 1.0,
            start: 0.0,
            end: TAU,
        }
    }
}

impl Curve for Circle2 {
    fn domain(&self) -> (f64, f64) {
        (0.0, TAU)
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.at_angle(t)
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        DVec2::new(-t.sin(), t.cos()) * self.r
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        -DVec2::new(t.cos(), t.sin()) * self.r
    }
    fn is_closed(&self) -> bool {
        true
    }
    fn bbox(&self) -> BBox2 {
        BBox2::new(self.c - DVec2::splat(self.r), self.c + DVec2::splat(self.r))
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        (t1 - t0).abs() * self.r
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let d = p - self.c;
        let t = if d.length_squared() < 1e-300 {
            0.0
        } else {
            normalize_0_2pi(d.y.atan2(d.x))
        };
        (t, self.at_angle(t))
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        let n = arc_segments(self.r, TAU, tol).max(8);
        (0..=n)
            .map(|i| {
                let t = if i == n {
                    TAU
                } else {
                    TAU * i as f64 / n as f64
                };
                (
                    t,
                    if i == n {
                        self.at_angle(0.0)
                    } else {
                        self.at_angle(t)
                    },
                )
            })
            .collect()
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let sw = ccw_sweep(t0, t1);
        if !(sw * self.r > 1e-12 * self.r.max(1.0)) || (t1 - t0).abs() < 1e-15 {
            return None;
        }
        Some(Curve2::Arc(Arc2::new(self.c, self.r, t0, t1)))
    }
    fn reversed(&self) -> Curve2 {
        Curve2::Polyline(self.to_polyline(false))
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        match similarity(m) {
            Some((s, _, _)) => Curve2::Circle(Circle2::new(m.transform_point2(self.c), self.r * s)),
            None => self.as_ellipse().transformed(m),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Arc2

impl Arc2 {
    fn as_ellipse(&self) -> EllipseArc2 {
        EllipseArc2 {
            c: self.c,
            major: DVec2::new(self.r, 0.0),
            ratio: 1.0,
            start: self.start,
            end: self.start + self.sweep(),
        }
    }
    /// Arc parameter (angle within the domain) of the angle `ang`, if on the arc (`eps` radians).
    pub fn param_of_angle(&self, ang: f64, eps: f64) -> Option<f64> {
        arc_param(self.start, self.sweep(), ang, eps)
    }
    /// Bulge-polyline representation (1 segment), in the given direction.
    pub fn to_polyline(&self, ccw: bool) -> Polyline2 {
        let b = crate::bulge::sweep_to_bulge(self.sweep());
        if ccw {
            Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(self.start_point(), b),
                    PolyVertex::new(self.end_point()),
                ],
                closed: false,
            }
        } else {
            Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(self.end_point(), -b),
                    PolyVertex::new(self.start_point()),
                ],
                closed: false,
            }
        }
    }
}

impl Curve for Arc2 {
    fn domain(&self) -> (f64, f64) {
        (self.start, self.start + self.sweep())
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.at_angle(self.clamp_param(t))
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        let t = self.clamp_param(t);
        DVec2::new(-t.sin(), t.cos()) * self.r
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        let t = self.clamp_param(t);
        -DVec2::new(t.cos(), t.sin()) * self.r
    }
    fn is_closed(&self) -> bool {
        false
    }
    fn bbox(&self) -> BBox2 {
        let mut b = BBox2::from_points([self.start_point(), self.end_point()]);
        let sweep = self.sweep();
        for k in 0..4 {
            let ang = k as f64 * std::f64::consts::FRAC_PI_2;
            if normalize_0_2pi(ang - self.start) <= sweep {
                b.include(self.at_angle(ang));
            }
        }
        b
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        (self.clamp_param(t1) - self.clamp_param(t0)).abs() * self.r
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let d = p - self.c;
        let (a, b) = self.domain();
        if d.length_squared() > 1e-300
            && let Some(t) = arc_param(self.start, b - a, d.y.atan2(d.x), 0.0)
        {
            return (t, self.at_angle(t));
        }
        let (ps, pe) = (self.at_angle(a), self.at_angle(b));
        if ps.distance_squared(p) <= pe.distance_squared(p) {
            (a, ps)
        } else {
            (b, pe)
        }
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        let (a, b) = self.domain();
        let n = arc_segments(self.r, b - a, tol);
        (0..=n)
            .map(|i| {
                let t = if i == n {
                    b
                } else {
                    a + (b - a) * i as f64 / n as f64
                };
                (t, self.at_angle(t))
            })
            .collect()
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let (t0, t1) = (self.clamp_param(t0), self.clamp_param(t1));
        ((t1 - t0) * self.r > 1e-12 * self.r.max(1.0))
            .then(|| Curve2::Arc(Arc2::new(self.c, self.r, t0, t1)))
    }
    fn reversed(&self) -> Curve2 {
        Curve2::Polyline(self.to_polyline(false))
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        match similarity(m) {
            Some((s, rot, false)) => {
                let sw = self.sweep();
                Curve2::Arc(Arc2::new(
                    m.transform_point2(self.c),
                    self.r * s,
                    self.start + rot,
                    self.start + rot + sw,
                ))
            }
            Some((s, rot, true)) => {
                let end = self.start + self.sweep();
                Curve2::Arc(Arc2::new(
                    m.transform_point2(self.c),
                    self.r * s,
                    rot - end,
                    rot - self.start,
                ))
            }
            None => self.as_ellipse().transformed(m),
        }
    }
}

// ------------------------------------------------------------------------------------------------
// EllipseArc2

impl EllipseArc2 {
    /// Parametric sweep in `(0, 2π]`.
    pub fn sweep(&self) -> f64 {
        if self.is_full() {
            TAU
        } else {
            ccw_sweep(self.start, self.end)
        }
    }
    /// Map from the unit circle to this ellipse: `u ↦ c + major·u.x + minor·u.y`.
    pub fn unit_map(&self) -> DAffine2 {
        DAffine2::from_cols(self.major, self.minor(), self.c)
    }
    /// Parameter (within the domain) of the parametric angle `t`, if on the arc.
    pub fn param_of_angle(&self, t: f64, eps: f64) -> Option<f64> {
        if self.is_full() {
            return Some(normalize_0_2pi(t - self.start) + self.start);
        }
        arc_param(self.start, self.sweep(), t, eps)
    }
    /// Parametric angle of a point (projected through the inverse unit map).
    pub fn angle_of_point(&self, p: DVec2) -> f64 {
        let inv = self.unit_map();
        if inv.matrix2.determinant().abs() < 1e-300 {
            return self.start;
        }
        let q = inv.inverse().transform_point2(p);
        q.y.atan2(q.x)
    }
}

impl Curve for EllipseArc2 {
    fn domain(&self) -> (f64, f64) {
        (self.start, self.start + self.sweep())
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.at_param(t)
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        -self.major * t.sin() + self.minor() * t.cos()
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        -(self.major * t.cos() + self.minor() * t.sin())
    }
    fn is_closed(&self) -> bool {
        self.is_full()
    }
    fn bbox(&self) -> BBox2 {
        let (a, b) = self.domain();
        let mut bb = BBox2::from_points([self.at_param(a), self.at_param(b)]);
        let (m, n) = (self.major, self.minor());
        let tx = n.x.atan2(m.x);
        let ty = n.y.atan2(m.y);
        for base in [tx, ty] {
            for k in 0..2 {
                let t = base + k as f64 * std::f64::consts::PI;
                if let Some(tt) = arc_param(a, b - a, t, 0.0) {
                    bb.include(self.at_param(tt));
                }
            }
        }
        bb
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
        let pieces = ((hi - lo) / (std::f64::consts::PI / 8.0)).ceil().max(1.0) as usize;
        integrate(&|t| self.deriv_at(t).length(), lo, hi, pieces, 1e-12)
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let (a, b) = self.domain();
        let per = ((b - a) / TAU * 48.0).ceil().max(8.0) as usize;
        closest_by_sampling(self, p, &[a, b], per)
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        let (a, b) = self.domain();
        let tol = sane_tol(tol, || self.major.length() * 1e-3);
        let init = ((b - a) / (std::f64::consts::PI / 4.0)).ceil().max(2.0) as usize;
        let mut v = adaptive_flatten(&|t| self.at_param(t), &[a, b], init, tol);
        if self.is_full()
            && let (Some(first), Some(last)) = (v.first().copied(), v.last_mut())
        {
            last.1 = first.1;
        }
        v
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let (a, b) = self.domain();
        let (t0, t1) = if self.is_full() {
            (t0, if t1 <= t0 { t1 + TAU } else { t1 })
        } else {
            (t0.sclamp(a, b), t1.sclamp(a, b))
        };
        if !(t1 - t0 > 1e-12) || t1 - t0 > TAU {
            return None;
        }
        Some(Curve2::Ellipse(EllipseArc2 {
            start: t0,
            end: t1,
            ..*self
        }))
    }
    fn reversed(&self) -> Curve2 {
        Curve2::Spline(self.to_nurbs().reversed())
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        let c = m.transform_point2(self.c);
        let p = m.transform_vector2(self.major);
        let q = m.transform_vector2(self.minor());
        // principal axes of c + p cos t + q sin t
        let t0 = 0.5 * (2.0 * p.dot(q)).atan2(p.dot(p) - q.dot(q));
        let mut maj = p * t0.cos() + q * t0.sin();
        let mut min = -p * t0.sin() + q * t0.cos();
        let mut shift = t0;
        if min.length_squared() > maj.length_squared() {
            // rotate the parametrization by another quarter turn
            let nm = min;
            min = -maj;
            maj = nm;
            shift += std::f64::consts::FRAC_PI_2;
        }
        let ml = maj.length();
        if ml < 1e-300 {
            return Curve2::Line(Line2::new(c, c));
        }
        let ratio = (min.length() / ml).clamp(1e-12, 1.0);
        let full = self.is_full();
        // new parameter s = t − shift if `min` points to the left of `maj`, otherwise mirrored
        if cross2(maj, min) >= 0.0 {
            let (s0, s1) = if full {
                (self.start - shift, self.start - shift + TAU)
            } else {
                (self.start - shift, self.start + self.sweep() - shift)
            };
            Curve2::Ellipse(EllipseArc2 {
                c,
                major: maj,
                ratio,
                start: s0,
                end: s1,
            })
        } else {
            let (s0, s1) = if full {
                (shift - self.start, shift - self.start + TAU)
            } else {
                (-(self.start + self.sweep() - shift), -(self.start - shift))
            };
            Curve2::Ellipse(EllipseArc2 {
                c,
                major: maj,
                ratio,
                start: s0,
                end: s1,
            })
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Polyline2

impl Polyline2 {
    /// Segment index and local parameter for polyline parameter `t`.
    pub fn locate(&self, t: f64) -> (usize, f64) {
        let n = self.segment_count();
        if n == 0 {
            return (0, 0.0);
        }
        let t = if t.is_finite() {
            t.clamp(0.0, n as f64)
        } else {
            0.0
        };
        let i = (t.floor() as usize).min(n - 1);
        (i, t - i as f64)
    }

    fn seg_sub(seg: &PolySegment, u0: f64, u1: f64) -> PolySegment {
        match seg {
            PolySegment::Line(l) => PolySegment::Line(Line2::new(l.at(u0), l.at(u1))),
            PolySegment::Arc(a) => PolySegment::Arc(BulgeArc {
                c: a.c,
                r: a.r,
                start: a.start + a.sweep * u0,
                sweep: a.sweep * (u1 - u0),
            }),
        }
    }

    fn sub_range_segments(&self, t0: f64, t1: f64, out: &mut Vec<PolySegment>) {
        let (i0, u0) = self.locate(t0);
        let (i1, u1) = self.locate(t1);
        let (i1, u1) = if u1 == 0.0 && i1 > i0 {
            (i1 - 1, 1.0)
        } else {
            (i1, u1)
        };
        for i in i0..=i1 {
            let Some(seg) = self.segment(i) else { continue };
            let a = if i == i0 { u0 } else { 0.0 };
            let b = if i == i1 { u1 } else { 1.0 };
            if b > a + 1e-15 {
                out.push(Self::seg_sub(&seg, a, b));
            }
        }
    }
}

impl Curve for Polyline2 {
    fn domain(&self) -> (f64, f64) {
        (0.0, self.segment_count() as f64)
    }
    fn point_at(&self, t: f64) -> DVec2 {
        if self.segment_count() == 0 {
            return self.verts.first().map(|v| v.p).unwrap_or(DVec2::ZERO);
        }
        let (i, u) = self.locate(t);
        self.segment(i)
            .map(|s| s.point_at(u))
            .unwrap_or(DVec2::ZERO)
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        let (i, u) = self.locate(t);
        self.segment(i)
            .map(|s| s.deriv_at(u))
            .unwrap_or(DVec2::ZERO)
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        let (i, u) = self.locate(t);
        self.segment(i)
            .map(|s| s.deriv2_at(u))
            .unwrap_or(DVec2::ZERO)
    }
    fn is_closed(&self) -> bool {
        self.closed && self.verts.len() >= 2
    }
    fn bbox(&self) -> BBox2 {
        let mut b = BBox2::from_points(self.verts.iter().map(|v| v.p));
        for s in self.segments() {
            if let PolySegment::Arc(a) = s {
                b = b.union(&a.to_arc().bbox());
            }
        }
        b
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        let (t0, t1) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
        let mut segs = Vec::new();
        self.sub_range_segments(t0, t1, &mut segs);
        segs.iter().map(|s| s.length()).sum()
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let mut best = (0.0, self.point_at(0.0), f64::INFINITY);
        for (i, s) in self.segments().enumerate() {
            let (u, q) = match s {
                PolySegment::Line(l) => l.closest(p),
                PolySegment::Arc(a) => {
                    let arc = a.to_arc();
                    let (ang, q) = arc.closest(p);
                    (PolySegment::arc_angle_to_u(&a, ang).clamp(0.0, 1.0), q)
                }
            };
            let d = q.distance_squared(p);
            if d < best.2 {
                best = (i as f64 + u, q, d);
            }
        }
        (best.0, best.1)
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        let mut out: Vec<(f64, DVec2)> = Vec::new();
        if let Some(v) = self.verts.first() {
            out.push((0.0, v.p));
        }
        for (i, s) in self.segments().enumerate() {
            match s {
                PolySegment::Line(l) => out.push((i as f64 + 1.0, l.b)),
                PolySegment::Arc(a) => {
                    let n = arc_segments(a.r, a.sweep, tol);
                    for k in 1..=n {
                        let u = k as f64 / n as f64;
                        out.push((i as f64 + u, if k == n { s.end() } else { a.point_at(u) }));
                    }
                }
            }
        }
        if self.is_closed()
            && let (Some(first), Some(last)) = (out.first().copied(), out.last_mut())
        {
            last.1 = first.1;
        }
        out
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let (a, b) = self.domain();
        if b <= a {
            return None;
        }
        let mut segs = Vec::new();
        if self.is_closed() && t1 <= t0 {
            self.sub_range_segments(t0.sclamp(a, b), b, &mut segs);
            self.sub_range_segments(a, t1.sclamp(a, b), &mut segs);
        } else {
            let (t0, t1) = (t0.sclamp(a, b), t1.sclamp(a, b));
            if !(t1 > t0) {
                return None;
            }
            self.sub_range_segments(t0, t1, &mut segs);
        }
        segs.retain(|s| s.length() > 1e-14);
        if segs.is_empty() {
            return None;
        }
        Some(Curve2::Polyline(Polyline2::from_segments(&segs, false)))
    }
    fn reversed(&self) -> Curve2 {
        let segs: Vec<PolySegment> = self
            .segments()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|s| match s {
                PolySegment::Line(l) => PolySegment::Line(Line2::new(l.b, l.a)),
                PolySegment::Arc(a) => PolySegment::Arc(BulgeArc {
                    c: a.c,
                    r: a.r,
                    start: a.start + a.sweep,
                    sweep: -a.sweep,
                }),
            })
            .collect();
        if segs.is_empty() {
            return Curve2::Polyline(self.clone());
        }
        Curve2::Polyline(Polyline2::from_segments(&segs, self.closed))
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        let has_arcs = self
            .verts
            .iter()
            .any(|v| v.bulge.abs() >= crate::bulge::BULGE_EPS);
        match similarity(m) {
            Some((_, _, refl)) => {
                let sign = if refl { -1.0 } else { 1.0 };
                Curve2::Polyline(Polyline2 {
                    verts: self
                        .verts
                        .iter()
                        .map(|v| PolyVertex::with_bulge(m.transform_point2(v.p), v.bulge * sign))
                        .collect(),
                    closed: self.closed,
                })
            }
            None if !has_arcs => Curve2::Polyline(Polyline2 {
                verts: self
                    .verts
                    .iter()
                    .map(|v| PolyVertex::new(m.transform_point2(v.p)))
                    .collect(),
                closed: self.closed,
            }),
            None => match self.to_nurbs() {
                Some(n) => Curve2::Spline(n.transformed(m)),
                None => Curve2::Polyline(self.clone()),
            },
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Nurbs2 (callers must check `is_valid()`; `Curve2` does this and falls back to the control polygon)

impl Curve for Nurbs2 {
    fn domain(&self) -> (f64, f64) {
        Nurbs2::domain(self)
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.eval_derivs(t)[0]
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        self.eval_derivs(t)[1]
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        self.eval_derivs(t)[2]
    }
    fn is_closed(&self) -> bool {
        let (a, b) = Nurbs2::domain(self);
        self.closed
            || self.point_at(a).distance(self.point_at(b))
                <= 1e-10 * (1.0 + self.ctrl_bbox().size().length())
    }
    fn bbox(&self) -> BBox2 {
        let hull = self.ctrl_bbox();
        let tol = default_tol(&hull) * 0.1;
        let mut b = BBox2::from_points(self.flatten_params(tol).into_iter().map(|x| x.1));
        b = b.expanded(tol);
        // never larger than the control hull (valid for positive weights)
        if self.weights.iter().all(|w| *w > 0.0) {
            b.min = b.min.max(hull.min);
            b.max = b.max.min(hull.max);
        }
        b
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        let (lo, hi) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
        let (d0, d1) = Nurbs2::domain(self);
        let (lo, hi) = (lo.sclamp(d0, d1), hi.sclamp(d0, d1));
        let mut total = 0.0;
        for w in self.span_breaks().windows(2) {
            let (a, b) = (w[0].max(lo), w[1].min(hi));
            if b > a {
                total += integrate(&|t| self.deriv_at(t).length(), a, b, 2, 1e-11);
            }
        }
        total
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        let per = 4 * (self.degree as usize + 1);
        closest_by_sampling(self, p, &self.span_breaks(), per)
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        let tol = sane_tol(tol, || default_tol(&self.ctrl_bbox()));
        let init = (self.degree as usize).clamp(2, 8);
        let mut v = adaptive_flatten(&|t| self.point_at(t), &self.span_breaks(), init, tol);
        if self.closed
            && let (Some(first), Some(last)) = (v.first().copied(), v.last_mut())
            && first.1.distance(last.1) <= tol
        {
            last.1 = first.1;
        }
        v
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        let (a, b) = Nurbs2::domain(self);
        if Curve::is_closed(self) && t1 <= t0 {
            let first = Nurbs2::sub_curve(self, t0, b);
            let second = Nurbs2::sub_curve(self, a, t1);
            return match (first, second) {
                (Some(f), Some(s)) => f.concat(&s).map(Curve2::Spline),
                (Some(f), None) => Some(Curve2::Spline(f)),
                (None, Some(s)) => Some(Curve2::Spline(s)),
                (None, None) => None,
            };
        }
        Nurbs2::sub_curve(self, t0, t1).map(Curve2::Spline)
    }
    fn reversed(&self) -> Curve2 {
        Curve2::Spline(Nurbs2::reversed(self))
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        Curve2::Spline(Nurbs2::transformed(self, m))
    }
}

impl Nurbs2 {
    fn ctrl_bbox(&self) -> BBox2 {
        BBox2::from_points(self.ctrl.iter().copied())
    }
    /// Control polygon (or fit points) as a polyline: the fallback geometry of invalid splines.
    pub fn fallback_polyline(&self) -> Polyline2 {
        let pts = if self.ctrl.len() >= 2 {
            &self.ctrl
        } else {
            &self.fit_points
        };
        Polyline2::from_points(pts.iter().copied().filter(|p| p.is_finite()), self.closed)
    }
}

// ------------------------------------------------------------------------------------------------
// Curve2 dispatch

impl Curve2 {
    /// Run `f` with the curve as a trait object (invalid splines use their control polygon).
    pub fn with_dyn<R>(&self, f: impl FnOnce(&dyn Curve) -> R) -> R {
        match self {
            Curve2::Line(c) => f(c),
            Curve2::Circle(c) => f(c),
            Curve2::Arc(c) => f(c),
            Curve2::Ellipse(c) => f(c),
            Curve2::Polyline(c) => f(c),
            Curve2::Spline(c) => {
                if c.is_valid() {
                    f(c)
                } else {
                    f(&c.fallback_polyline())
                }
            }
        }
    }

    /// Closed-form offset where exact (see [`crate::offset`]); positive `d` = left of the direction.
    pub fn offset(&self, d: f64) -> Vec<Curve2> {
        crate::offset::offset_signed(self, d)
    }

    /// If the curve is a circular arc in either direction (an [`Arc2`] or a single-segment bulge
    /// polyline), return it as a counter-clockwise arc plus a `ccw` flag.
    pub fn as_directed_arc(&self) -> Option<(Arc2, bool)> {
        match self {
            Curve2::Arc(a) => Some((*a, true)),
            Curve2::Polyline(p) if p.segment_count() == 1 => match p.segment(0)? {
                PolySegment::Arc(a) => Some((a.to_arc(), a.is_ccw())),
                PolySegment::Line(_) => None,
            },
            _ => None,
        }
    }

    /// Kind name for diagnostics.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Curve2::Line(_) => "line",
            Curve2::Circle(_) => "circle",
            Curve2::Arc(_) => "arc",
            Curve2::Ellipse(_) => "ellipse",
            Curve2::Polyline(_) => "polyline",
            Curve2::Spline(_) => "spline",
        }
    }
}

impl Curve for Curve2 {
    fn domain(&self) -> (f64, f64) {
        self.with_dyn(|c| c.domain())
    }
    fn point_at(&self, t: f64) -> DVec2 {
        self.with_dyn(|c| c.point_at(t))
    }
    fn deriv_at(&self, t: f64) -> DVec2 {
        self.with_dyn(|c| c.deriv_at(t))
    }
    fn deriv2_at(&self, t: f64) -> DVec2 {
        self.with_dyn(|c| c.deriv2_at(t))
    }
    fn is_closed(&self) -> bool {
        self.with_dyn(|c| c.is_closed())
    }
    fn bbox(&self) -> BBox2 {
        self.with_dyn(|c| c.bbox())
    }
    fn length_between(&self, t0: f64, t1: f64) -> f64 {
        self.with_dyn(|c| c.length_between(t0, t1))
    }
    fn closest(&self, p: DVec2) -> (f64, DVec2) {
        self.with_dyn(|c| c.closest(p))
    }
    fn flatten_params(&self, tol: f64) -> Vec<(f64, DVec2)> {
        self.with_dyn(|c| c.flatten_params(tol))
    }
    fn sub_curve(&self, t0: f64, t1: f64) -> Option<Curve2> {
        self.with_dyn(|c| c.sub_curve(t0, t1))
    }
    fn reversed(&self) -> Curve2 {
        self.with_dyn(|c| c.reversed())
    }
    fn transformed(&self, m: &DAffine2) -> Curve2 {
        self.with_dyn(|c| c.transformed(m))
    }
    fn curvature_at(&self, t: f64) -> f64 {
        self.with_dyn(|c| c.curvature_at(t))
    }
    fn param_at_length(&self, s: f64) -> f64 {
        self.with_dyn(|c| c.param_at_length(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn all_curves() -> Vec<Curve2> {
        vec![
            Curve2::Line(Line2::new(DVec2::new(1.0, 2.0), DVec2::new(5.0, -1.0))),
            Curve2::Circle(Circle2::new(DVec2::new(1.0, 1.0), 2.0)),
            Curve2::Arc(Arc2::new(DVec2::new(0.0, 0.0), 3.0, 5.5, 1.0)),
            Curve2::Ellipse(EllipseArc2 {
                c: DVec2::new(2.0, 1.0),
                major: DVec2::new(3.0, 4.0),
                ratio: 0.4,
                start: 0.5,
                end: 4.0,
            }),
            Curve2::Ellipse(EllipseArc2 {
                c: DVec2::ZERO,
                major: DVec2::new(2.0, 0.0),
                ratio: 0.5,
                start: 0.0,
                end: TAU,
            }),
            Curve2::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::new(0.0, 0.0), 0.0),
                    PolyVertex::with_bulge(DVec2::new(4.0, 0.0), 0.5),
                    PolyVertex::with_bulge(DVec2::new(4.0, 3.0), -0.3),
                    PolyVertex::new(DVec2::new(0.0, 3.0)),
                ],
                closed: false,
            }),
            Curve2::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::new(0.0, 0.0), 0.4),
                    PolyVertex::new(DVec2::new(4.0, 0.0)),
                    PolyVertex::new(DVec2::new(2.0, 3.0)),
                ],
                closed: true,
            }),
            Curve2::Spline(
                Nurbs2::from_fit_points(
                    &[
                        DVec2::new(0.0, 0.0),
                        DVec2::new(1.0, 2.0),
                        DVec2::new(3.0, 1.0),
                        DVec2::new(5.0, 3.0),
                    ],
                    3,
                )
                .unwrap(),
            ),
        ]
    }

    #[test]
    fn derivatives_consistent() {
        for c in all_curves() {
            let (a, b) = c.domain();
            for i in 1..40 {
                let t = a + (b - a) * (i as f64 + 0.37) / 41.0;
                let h = 1e-6 * (b - a);
                let fd = (c.point_at(t + h) - c.point_at(t - h)) / (2.0 * h);
                let d = c.deriv_at(t);
                assert!(
                    (fd - d).length() < 1e-5 * (1.0 + d.length()),
                    "{} t={t} {fd} {d}",
                    c.kind_name()
                );
            }
        }
    }

    #[test]
    fn length_matches_flatten() {
        for c in all_curves() {
            let l = c.length();
            let fl: f64 = c
                .flatten(1e-6)
                .windows(2)
                .map(|w| w[0].distance(w[1]))
                .sum();
            assert!((l - fl).abs() < 1e-4 * l, "{} {l} {fl}", c.kind_name());
            // param_at_length round trip
            let t = c.param_at_length(0.3 * l);
            let (a, _) = c.domain();
            assert!(
                (c.length_between(a, t) - 0.3 * l).abs() < 1e-8 * l,
                "{}",
                c.kind_name()
            );
        }
        let e = EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(5.0, 0.0),
            ratio: 0.6,
            start: 0.0,
            end: TAU,
        };
        // Ramanujan II for a=5, b=3
        let (a, b) = (5.0f64, 3.0f64);
        let h = ((a - b) / (a + b)).powi(2);
        let ram = PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt()));
        assert!((e.length() - ram).abs() < 1e-6, "{} {ram}", e.length());
    }

    #[test]
    fn bbox_contains_samples_and_is_tight() {
        for c in all_curves() {
            let bb = c.bbox();
            let pts = c.flatten(1e-5);
            let fb = BBox2::from_points(pts.iter().copied());
            for p in &pts {
                assert!(bb.expanded(1e-9).contains(*p), "{} {p}", c.kind_name());
            }
            assert!(
                (bb.min - fb.min).length() < 1e-3 && (bb.max - fb.max).length() < 1e-3,
                "{} {bb:?} {fb:?}",
                c.kind_name()
            );
        }
    }

    #[test]
    fn closest_is_minimal() {
        for c in all_curves() {
            let pts = c.flatten(1e-4);
            for q in [
                DVec2::new(0.3, 0.2),
                DVec2::new(6.0, 6.0),
                DVec2::new(-3.0, 1.0),
                DVec2::new(2.0, 1.5),
            ] {
                let (t, p) = c.closest(q);
                assert!((c.point_at(t) - p).length() < 1e-9);
                let brute = pts
                    .iter()
                    .map(|x| x.distance(q))
                    .fold(f64::INFINITY, f64::min);
                assert!(
                    p.distance(q) <= brute + 1e-6,
                    "{} q={q} {} vs {brute}",
                    c.kind_name(),
                    p.distance(q)
                );
            }
        }
    }

    #[test]
    fn flatten_respects_tolerance() {
        for c in all_curves() {
            let tol = 1e-3;
            let fp = c.flatten_params(tol);
            for w in fp.windows(2) {
                for k in 1..4 {
                    let t = w[0].0 + (w[1].0 - w[0].0) * k as f64 / 4.0;
                    let d = dist_to_segment(c.point_at(t), w[0].1, w[1].1);
                    assert!(d <= tol * 1.01, "{} d={d}", c.kind_name());
                }
            }
            assert!(fp.first().unwrap().1.distance(c.start()) < 1e-12);
            assert!(fp.last().unwrap().1.distance(c.end()) < 1e-12);
        }
    }

    #[test]
    fn split_and_sub_curve() {
        for c in all_curves() {
            let (a, b) = c.domain();
            let t = a + 0.4 * (b - a);
            if c.is_closed() {
                assert!(c.split(t).is_none());
                // wrapped piece
                let s = c.sub_curve(a + 0.7 * (b - a), a + 0.2 * (b - a)).unwrap();
                assert!(
                    s.start().distance(c.point_at(a + 0.7 * (b - a))) < 1e-9,
                    "{}",
                    c.kind_name()
                );
                assert!(
                    s.end().distance(c.point_at(a + 0.2 * (b - a))) < 1e-9,
                    "{}",
                    c.kind_name()
                );
                let tot = c.length();
                assert!(
                    (s.length() - 0.5 * tot).abs() < 1e-3 * tot
                        || !matches!(c, Curve2::Circle(_) | Curve2::Ellipse(_)),
                    "{}",
                    c.kind_name()
                );
                continue;
            }
            let (p, q) = c.split(t).unwrap();
            assert!(p.start().distance(c.start()) < 1e-9, "{}", c.kind_name());
            assert!(p.end().distance(c.point_at(t)) < 1e-9, "{}", c.kind_name());
            assert!(
                q.start().distance(c.point_at(t)) < 1e-9,
                "{}",
                c.kind_name()
            );
            assert!(q.end().distance(c.end()) < 1e-9, "{}", c.kind_name());
            assert!(
                (p.length() + q.length() - c.length()).abs() < 1e-7 * c.length(),
                "{}",
                c.kind_name()
            );
        }
    }

    #[test]
    fn reversed_swaps_ends() {
        for c in all_curves() {
            let r = c.reversed();
            assert!(r.start().distance(c.end()) < 1e-9, "{}", c.kind_name());
            assert!(r.end().distance(c.start()) < 1e-9, "{}", c.kind_name());
            assert!(
                (r.length() - c.length()).abs() < 1e-7 * c.length(),
                "{}",
                c.kind_name()
            );
            // mid points coincide
            let pm = c.point_at(c.param_at_length(0.5 * c.length()));
            let rm = r.point_at(r.param_at_length(0.5 * r.length()));
            assert!(pm.distance(rm) < 1e-6, "{} {pm} {rm}", c.kind_name());
        }
    }

    #[test]
    fn transforms() {
        let ms = [
            DAffine2::from_scale_angle_translation(DVec2::splat(2.0), 0.7, DVec2::new(3.0, -1.0)),
            DAffine2::from_scale_angle_translation(
                DVec2::new(2.0, -2.0),
                0.3,
                DVec2::new(1.0, 1.0),
            ),
            DAffine2::from_scale_angle_translation(
                DVec2::new(1.0, 0.5),
                0.4,
                DVec2::new(-2.0, 0.0),
            ),
            DAffine2::from_cols(
                DVec2::new(1.0, 0.3),
                DVec2::new(0.2, 1.5),
                DVec2::new(0.5, 0.5),
            ),
        ];
        for c in all_curves() {
            for m in &ms {
                let tc = c.transformed(m);
                if !matches!(c, Curve2::Circle(_)) {
                    // mirrored arcs/ellipses swap their ends (they must stay counter-clockwise)
                    let (s, e) = (m.transform_point2(c.start()), m.transform_point2(c.end()));
                    let same = tc.start().distance(s) < 1e-9
                        && (tc.end().distance(e) < 1e-9 || tc.is_closed());
                    let swapped = tc.start().distance(e) < 1e-9 && tc.end().distance(s) < 1e-9;
                    assert!(same || swapped, "{} -> {}", c.kind_name(), tc.kind_name());
                }
                // every transformed sample lies on the new curve
                let (a, b) = c.domain();
                for i in 0..=10 {
                    let p = m.transform_point2(c.point_at(a + (b - a) * i as f64 / 10.0));
                    let (_, q) = tc.closest(p);
                    assert!(
                        p.distance(q) < 1e-7,
                        "{} -> {} i={i} {}",
                        c.kind_name(),
                        tc.kind_name(),
                        p.distance(q)
                    );
                }
            }
        }
        // circle under non-uniform scale is an ellipse
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 1.0));
        assert!(matches!(
            c.transformed(&DAffine2::from_scale(DVec2::new(2.0, 1.0))),
            Curve2::Ellipse(_)
        ));
        let a = Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, 0.0, FRAC_PI_2));
        let m = a.transformed(&DAffine2::from_scale(DVec2::new(-1.0, 1.0)));
        let Curve2::Arc(ma) = m else { panic!() };
        assert!((ma.sweep() - FRAC_PI_2).abs() < 1e-12);
        assert!((ma.mid_point() - DVec2::new(-0.5f64.sqrt(), 0.5f64.sqrt())).length() < 1e-12);
    }

    #[test]
    fn serde_round_trip() {
        for c in all_curves() {
            let s = serde_json::to_string(&c).unwrap();
            let back: Curve2 = serde_json::from_str(&s).unwrap();
            assert_eq!(back.kind_name(), c.kind_name());
            assert!(
                back.start().distance(c.start()) < 1e-12 && back.end().distance(c.end()) < 1e-12
            );
            assert!((back.length() - c.length()).abs() < 1e-12);
        }
    }

    #[test]
    fn invalid_spline_does_not_panic() {
        let s = Curve2::Spline(Nurbs2 {
            degree: 3,
            ctrl: vec![DVec2::ZERO, DVec2::ONE],
            weights: vec![],
            knots: vec![0.0],
            fit_points: vec![],
            closed: false,
        });
        let _ = s.length();
        let _ = s.bbox();
        let _ = s.flatten(0.1);
        let _ = s.closest(DVec2::X);
        let empty = Curve2::Spline(Nurbs2::default());
        let _ = empty.flatten(0.1);
        let _ = empty.point_at(0.5);
        let _ = empty.bbox();
        let pl = Curve2::Polyline(Polyline2::default());
        let _ = pl.flatten(0.1);
        let _ = pl.closest(DVec2::ONE);
        let _ = pl.length();
    }
}
