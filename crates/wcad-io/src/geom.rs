//! Path geometry shared by the SVG and PDF writers.
//!
//! Curves become [`Seg`] paths that keep arcs exact (SVG has native elliptical arcs; PDF converts
//! them to cubics), splines are decomposed into Bézier segments, and everything can be transformed
//! by an affine map (block inserts) without losing exactness.

use std::f64::consts::{FRAC_PI_2, TAU};

use wcad_geom2d::{Curve2, Nurbs2, PolyVertex, Polyline2};
#[cfg(test)]
use wcad_math::cross2;
use wcad_math::{BBox2, DAffine2, DVec2, perp};

/// One path command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Seg {
    Move(DVec2),
    Line(DVec2),
    /// Elliptical arc through `c + u·cos t + v·sin t` for `t` from `t0` to `t0 + dt` (`dt` signed).
    /// `u`/`v` are conjugate semi-diameters (orthogonal for untransformed ellipses).
    Arc {
        c: DVec2,
        u: DVec2,
        v: DVec2,
        t0: f64,
        dt: f64,
    },
    Cubic(DVec2, DVec2, DVec2),
    Close,
}

pub(crate) type Path = Vec<Seg>;

#[inline]
pub(crate) fn arc_point(c: DVec2, u: DVec2, v: DVec2, t: f64) -> DVec2 {
    c + u * t.cos() + v * t.sin()
}

#[inline]
fn arc_deriv(u: DVec2, v: DVec2, t: f64) -> DVec2 {
    -u * t.sin() + v * t.cos()
}

fn finite(p: DVec2) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

/// Circular arc for a polyline bulge segment `a → b`. `None` for a straight segment.
pub(crate) fn bulge_arc(a: DVec2, b: DVec2, bulge: f64) -> Option<Seg> {
    if !bulge.is_finite() || bulge.abs() < 1e-12 {
        return None;
    }
    let chord = b - a;
    if chord.length_squared() < 1e-24 {
        return None;
    }
    let center = (a + b) * 0.5 + perp(chord) * ((1.0 - bulge * bulge) / (4.0 * bulge));
    let r = (a - center).length();
    let t0 = (a.y - center.y).atan2(a.x - center.x);
    let dt = 4.0 * bulge.atan();
    if !finite(center) || !r.is_finite() {
        return None;
    }
    Some(Seg::Arc { c: center, u: DVec2::new(r, 0.0), v: DVec2::new(0.0, r), t0, dt })
}

/// Segments of a polyline after its initial `Move` (closing segment included when closed).
pub(crate) fn polyline_segs(p: &Polyline2) -> Option<(DVec2, Vec<Seg>)> {
    let first = p.verts.first()?;
    let n = p.verts.len();
    let mut segs = Vec::with_capacity(n + 1);
    let seg_count = p.segment_count();
    for i in 0..seg_count {
        let v0: &PolyVertex = &p.verts[i];
        let v1 = &p.verts[(i + 1) % n];
        match bulge_arc(v0.p, v1.p, v0.bulge) {
            Some(arc) => segs.push(arc),
            None => segs.push(Seg::Line(v1.p)),
        }
    }
    Some((first.p, segs))
}

/// Start point and segments (after the initial `Move`) of any curve. `None` for degenerate data.
pub(crate) fn curve_segs(curve: &Curve2) -> Option<(DVec2, Vec<Seg>)> {
    match curve {
        Curve2::Line(l) => Some((l.a, vec![Seg::Line(l.b)])),
        Curve2::Circle(c) => {
            if !(c.r > 0.0) {
                return None;
            }
            let u = DVec2::new(c.r, 0.0);
            let v = DVec2::new(0.0, c.r);
            Some((c.c + u, vec![Seg::Arc { c: c.c, u, v, t0: 0.0, dt: TAU }]))
        }
        Curve2::Arc(a) => {
            if !(a.r > 0.0) {
                return None;
            }
            let u = DVec2::new(a.r, 0.0);
            let v = DVec2::new(0.0, a.r);
            Some((a.start_point(), vec![Seg::Arc { c: a.c, u, v, t0: a.start, dt: a.sweep() }]))
        }
        Curve2::Ellipse(e) => {
            if e.major.length_squared() < 1e-24 || !(e.ratio > 0.0) {
                return None;
            }
            let dt = if e.is_full() { TAU } else { wcad_math::ccw_sweep(e.start, e.end) };
            let u = e.major;
            let v = e.minor();
            Some((e.at_param(e.start), vec![Seg::Arc { c: e.c, u, v, t0: e.start, dt }]))
        }
        Curve2::Polyline(p) => polyline_segs(p),
        Curve2::Spline(s) => nurbs_segs(s).or_else(|| spline_fallback(s)),
    }
}

/// A full path (`Move` + segments) for a curve; closed curves end with `Close`.
pub(crate) fn curve_path(curve: &Curve2) -> Path {
    let Some((start, segs)) = curve_segs(curve) else { return Vec::new() };
    let mut path = Vec::with_capacity(segs.len() + 2);
    path.push(Seg::Move(start));
    path.extend(segs);
    let closed = match curve {
        Curve2::Circle(_) => true,
        Curve2::Ellipse(e) => e.is_full(),
        Curve2::Polyline(p) => p.closed,
        _ => false,
    };
    if closed {
        path.push(Seg::Close);
    }
    path
}

/// End point of a segment list starting at `start`.
pub(crate) fn segs_end(start: DVec2, segs: &[Seg]) -> DVec2 {
    let mut cur = start;
    for s in segs {
        match *s {
            Seg::Move(p) | Seg::Line(p) | Seg::Cubic(_, _, p) => cur = p,
            Seg::Arc { c, u, v, t0, dt } => cur = arc_point(c, u, v, t0 + dt),
            Seg::Close => {}
        }
    }
    cur
}

/// Reverse a segment list: returns the new start point and the reversed segments.
pub(crate) fn reverse_segs(start: DVec2, segs: &[Seg]) -> (DVec2, Vec<Seg>) {
    // Points before each segment.
    let mut starts = Vec::with_capacity(segs.len());
    let mut cur = start;
    for s in segs {
        starts.push(cur);
        cur = segs_end(cur, std::slice::from_ref(s));
    }
    let mut out = Vec::with_capacity(segs.len());
    for (s, &from) in segs.iter().zip(&starts).rev() {
        match *s {
            Seg::Line(_) | Seg::Move(_) => out.push(Seg::Line(from)),
            Seg::Cubic(c1, c2, _) => out.push(Seg::Cubic(c2, c1, from)),
            Seg::Arc { c, u, v, t0, dt } => out.push(Seg::Arc { c, u, v, t0: t0 + dt, dt: -dt }),
            Seg::Close => {}
        }
    }
    (cur, out)
}

/// Chain a closed loop of curves into one closed sub-path, flipping curves whose orientation does
/// not continue from the previous end point (hatch boundaries are not always head-to-tail).
pub(crate) fn loop_path(curves: &[Curve2], out: &mut Path) {
    let mut first = true;
    let mut cur = DVec2::ZERO;
    for c in curves {
        let Some((s, segs)) = curve_segs(c) else { continue };
        let e = segs_end(s, &segs);
        let (s, segs) = if !first && cur.distance_squared(e) < cur.distance_squared(s) {
            reverse_segs(s, &segs)
        } else {
            (s, segs)
        };
        if first {
            out.push(Seg::Move(s));
            first = false;
        } else if cur.distance_squared(s) > 1e-18 {
            out.push(Seg::Line(s));
        }
        cur = segs_end(s, &segs);
        out.extend(segs);
    }
    if !first {
        out.push(Seg::Close);
    }
}

/// Apply an affine transform to a path (exact for arcs and Béziers).
pub(crate) fn transform_path(path: &mut Path, m: &DAffine2) {
    for s in path.iter_mut() {
        match s {
            Seg::Move(p) | Seg::Line(p) => *p = m.transform_point2(*p),
            Seg::Cubic(a, b, p) => {
                *a = m.transform_point2(*a);
                *b = m.transform_point2(*b);
                *p = m.transform_point2(*p);
            }
            Seg::Arc { c, u, v, .. } => {
                *c = m.transform_point2(*c);
                *u = m.transform_vector2(*u);
                *v = m.transform_vector2(*v);
            }
            Seg::Close => {}
        }
    }
}

/// Cubic Bézier pieces `(c1, c2, end)` approximating an elliptical arc (≤ 90° per piece).
pub(crate) fn arc_to_cubics(c: DVec2, u: DVec2, v: DVec2, t0: f64, dt: f64) -> Vec<(DVec2, DVec2, DVec2)> {
    if !dt.is_finite() || dt == 0.0 {
        return Vec::new();
    }
    let n = ((dt.abs() / FRAC_PI_2).ceil() as usize).clamp(1, 64);
    let d = dt / n as f64;
    let k = 4.0 / 3.0 * (d / 4.0).tan();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let a = t0 + d * i as f64;
        let b = a + d;
        let p0 = arc_point(c, u, v, a);
        let p3 = arc_point(c, u, v, b);
        out.push((p0 + arc_deriv(u, v, a) * k, p3 - arc_deriv(u, v, b) * k, p3));
    }
    out
}

/// Principal radii and rotation of the ellipse spanned by conjugate semi-diameters `u`, `v`.
/// Returns `(rx, ry, angle)` with `rx >= ry`, `angle` of the major axis in radians.
pub(crate) fn ellipse_axes(u: DVec2, v: DVec2) -> (f64, f64, f64) {
    let a = u.x * u.x + v.x * v.x;
    let b = u.x * u.y + v.x * v.y;
    let d = u.y * u.y + v.y * v.y;
    let mean = (a + d) * 0.5;
    let rad = (((a - d) * 0.5).powi(2) + b * b).sqrt();
    let l1 = (mean + rad).max(0.0);
    let l2 = (mean - rad).max(0.0);
    let angle = 0.5 * (2.0 * b).atan2(a - d);
    (l1.sqrt(), l2.sqrt(), angle)
}

/// Polylines approximating each sub-path (closed sub-paths repeat their first point at the end).
pub(crate) fn flatten(path: &[Seg], tol: f64) -> Vec<Vec<DVec2>> {
    let tol = if tol.is_finite() && tol > 0.0 { tol } else { 1e-3 };
    let mut out: Vec<Vec<DVec2>> = Vec::new();
    let mut cur_poly: Vec<DVec2> = Vec::new();
    let mut cur = DVec2::ZERO;
    let mut start = DVec2::ZERO;
    for s in path {
        match *s {
            Seg::Move(p) => {
                if cur_poly.len() > 1 {
                    out.push(std::mem::take(&mut cur_poly));
                }
                cur_poly.clear();
                cur_poly.push(p);
                cur = p;
                start = p;
            }
            Seg::Line(p) => {
                if cur_poly.is_empty() {
                    cur_poly.push(cur);
                }
                cur_poly.push(p);
                cur = p;
            }
            Seg::Arc { c, u, v, t0, dt } => {
                if cur_poly.is_empty() {
                    cur_poly.push(cur);
                }
                let r = u.length().max(v.length());
                let step = if r > tol { 2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos() } else { dt.abs() };
                let n = if step > 1e-9 { (dt.abs() / step).ceil() as usize } else { 1 }.clamp(1, 4096);
                for i in 1..=n {
                    cur_poly.push(arc_point(c, u, v, t0 + dt * i as f64 / n as f64));
                }
                cur = arc_point(c, u, v, t0 + dt);
            }
            Seg::Cubic(c1, c2, p) => {
                if cur_poly.is_empty() {
                    cur_poly.push(cur);
                }
                let len = cur.distance(c1) + c1.distance(c2) + c2.distance(p);
                let n = ((len / tol).sqrt().ceil() as usize).clamp(1, 1024);
                for i in 1..=n {
                    let t = i as f64 / n as f64;
                    let mt = 1.0 - t;
                    cur_poly.push(
                        cur * (mt * mt * mt) + c1 * (3.0 * mt * mt * t) + c2 * (3.0 * mt * t * t) + p * (t * t * t),
                    );
                }
                cur = p;
            }
            Seg::Close => {
                if !cur_poly.is_empty() {
                    if cur_poly.last().is_some_and(|l| l.distance_squared(start) > 0.0) {
                        cur_poly.push(start);
                    }
                    out.push(std::mem::take(&mut cur_poly));
                }
                cur = start;
            }
        }
    }
    if cur_poly.len() > 1 {
        out.push(cur_poly);
    }
    out
}

/// Bounding box of a path (arcs sampled, Béziers by their control hull).
pub(crate) fn path_bbox(path: &[Seg], bbox: &mut Option<BBox2>) {
    let mut add = |p: DVec2| {
        if finite(p) {
            match bbox {
                Some(b) => {
                    b.min = b.min.min(p);
                    b.max = b.max.max(p);
                }
                None => *bbox = Some(BBox2 { min: p, max: p }),
            }
        }
    };
    for s in path {
        match *s {
            Seg::Move(p) | Seg::Line(p) => add(p),
            Seg::Cubic(a, b, p) => {
                add(a);
                add(b);
                add(p);
            }
            Seg::Arc { c, u, v, t0, dt } => {
                let n = ((dt.abs() / TAU * 64.0).ceil() as usize).clamp(2, 64);
                for i in 0..=n {
                    add(arc_point(c, u, v, t0 + dt * i as f64 / n as f64));
                }
            }
            Seg::Close => {}
        }
    }
}

pub(crate) fn bbox_add_point(bbox: &mut Option<BBox2>, p: DVec2) {
    path_bbox(&[Seg::Move(p)], bbox);
}

// ---------------------------------------------------------------------------------------------
// NURBS → Bézier decomposition
// ---------------------------------------------------------------------------------------------

type H = [f64; 3]; // homogeneous (x·w, y·w, w)

fn hlerp(a: H, b: H, t: f64) -> H {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn hproject(h: H) -> Option<DVec2> {
    if h[2].abs() < 1e-300 {
        return None;
    }
    let p = DVec2::new(h[0] / h[2], h[1] / h[2]);
    finite(p).then_some(p)
}

/// Insert knot `u` once (NURBS Book A5.1 with r = 1). `None` if the data is inconsistent.
fn insert_knot(p: usize, knots: &mut Vec<f64>, pts: &mut Vec<H>, u: f64) -> Option<()> {
    let k = knots.iter().rposition(|&x| x <= u)?;
    let s = knots.iter().filter(|&&x| x == u).count();
    let n = pts.len();
    if k < p || s >= p || k - s >= n || k + p - s >= knots.len() {
        return None;
    }
    let mut q = Vec::with_capacity(n + 1);
    for i in 0..=n {
        if i + p <= k {
            q.push(pts[i]);
        } else if i > k - s {
            q.push(pts[i - 1]);
        } else {
            let denom = knots[i + p] - knots[i];
            let alpha = if denom.abs() > 0.0 { (u - knots[i]) / denom } else { 0.0 };
            q.push(hlerp(pts[i - 1], pts[i], alpha));
        }
    }
    knots.insert(k + 1, u);
    *pts = q;
    Some(())
}

fn de_casteljau(cps: &[H], t: f64) -> H {
    let mut tmp: Vec<H> = cps.to_vec();
    let n = tmp.len();
    for r in 1..n {
        for i in 0..n - r {
            tmp[i] = hlerp(tmp[i], tmp[i + 1], t);
        }
    }
    tmp.first().copied().unwrap_or([0.0, 0.0, 1.0])
}

/// Exact Bézier segments for non-rational splines of degree ≤ 3, sampled line segments otherwise.
pub(crate) fn nurbs_segs(s: &Nurbs2) -> Option<(DVec2, Vec<Seg>)> {
    let p = s.degree as usize;
    let n = s.ctrl.len();
    if p == 0 || p > 16 || n < p + 1 || s.knots.len() != n + p + 1 || n > 100_000 {
        return None;
    }
    let rational_data = !s.weights.is_empty();
    if rational_data && s.weights.len() != n {
        return None;
    }
    if s.knots.iter().any(|k| !k.is_finite()) || s.knots.windows(2).any(|w| w[1] < w[0]) {
        return None;
    }
    let mut pts: Vec<H> = Vec::with_capacity(n);
    for i in 0..n {
        let w = if rational_data { s.weights[i] } else { 1.0 };
        if !(w.is_finite() && w > 0.0) || !finite(s.ctrl[i]) {
            return None;
        }
        pts.push([s.ctrl[i].x * w, s.ctrl[i].y * w, w]);
    }
    let rational = rational_data && {
        let w0 = s.weights[0];
        s.weights.iter().any(|w| (w - w0).abs() > 1e-12 * w0.abs().max(1.0))
    };
    let mut knots = s.knots.clone();
    let a = knots[p];
    let b = knots[knots.len() - 1 - p];
    if !(b > a) {
        return None;
    }
    if n > MAX_EXACT_CTRL {
        return nurbs_sampled(p, &knots, &pts, a, b);
    }
    let mut values: Vec<f64> = knots.iter().copied().filter(|&k| k >= a && k <= b).collect();
    values.dedup();
    for u in values {
        let mult = knots.iter().filter(|&&k| k == u).count();
        for _ in mult..p {
            insert_knot(p, &mut knots, &mut pts, u)?;
        }
    }
    let m = knots.len() - 1;
    let mut start: Option<DVec2> = None;
    let mut segs = Vec::new();
    for j in p..m.saturating_sub(p) {
        if !(knots[j + 1] > knots[j]) || knots[j] < a || knots[j + 1] > b || j >= pts.len() {
            continue;
        }
        let cps = &pts[j - p..=j];
        let p0 = hproject(cps[0])?;
        if start.is_none() {
            start = Some(p0);
        }
        if !rational && p <= 3 {
            let q: Vec<DVec2> = cps.iter().map(|&h| hproject(h)).collect::<Option<Vec<_>>>()?;
            match p {
                1 => segs.push(Seg::Line(q[1])),
                2 => {
                    segs.push(Seg::Cubic(q[0] + (q[1] - q[0]) * (2.0 / 3.0), q[2] + (q[1] - q[2]) * (2.0 / 3.0), q[2]))
                }
                _ => segs.push(Seg::Cubic(q[1], q[2], q[3])),
            }
        } else {
            let samples = (8 * p).clamp(8, 64);
            for i in 1..=samples {
                let h = de_casteljau(cps, i as f64 / samples as f64);
                segs.push(Seg::Line(hproject(h)?));
            }
        }
    }
    Some((start?, segs))
}

/// Above this many control points, splines are sampled instead of decomposed (knot insertion is
/// quadratic in the control point count).
const MAX_EXACT_CTRL: usize = 2000;

/// de Boor evaluation on span `j` (`knots[j] <= u <= knots[j + 1]`).
fn de_boor(p: usize, knots: &[f64], pts: &[H], j: usize, u: f64) -> H {
    let mut d: Vec<H> = (0..=p).map(|r| pts[j - p + r]).collect();
    for r in 1..=p {
        for i in (r..=p).rev() {
            let idx = j - p + i;
            let denom = knots[idx + p + 1 - r] - knots[idx];
            let alpha = if denom != 0.0 { (u - knots[idx]) / denom } else { 0.0 };
            d[i] = hlerp(d[i - 1], d[i], alpha);
        }
    }
    d[p]
}

/// Line segments sampling every non-empty span (for very large splines).
fn nurbs_sampled(p: usize, knots: &[f64], pts: &[H], a: f64, b: f64) -> Option<(DVec2, Vec<Seg>)> {
    let m = knots.len() - 1;
    let mut start = None;
    let mut segs = Vec::new();
    for j in p..m.saturating_sub(p) {
        if !(knots[j + 1] > knots[j]) || knots[j] < a || knots[j + 1] > b || j >= pts.len() {
            continue;
        }
        const N: usize = 4;
        for i in 0..=N {
            let u = knots[j] + (knots[j + 1] - knots[j]) * i as f64 / N as f64;
            let q = hproject(de_boor(p, knots, pts, j, u))?;
            match start {
                None => start = Some(q),
                Some(_) if i > 0 => segs.push(Seg::Line(q)),
                Some(_) => {}
            }
        }
    }
    Some((start?, segs))
}

/// Polyline through fit points (or the control polygon) for splines with unusable knot data.
fn spline_fallback(s: &Nurbs2) -> Option<(DVec2, Vec<Seg>)> {
    let pts = if s.fit_points.len() >= 2 { &s.fit_points } else { &s.ctrl };
    let first = *pts.first()?;
    if pts.len() < 2 {
        return None;
    }
    Some((first, pts[1..].iter().map(|&p| Seg::Line(p)).collect()))
}

/// Signed area of a closed polygon (CCW positive).
#[cfg(test)]
pub(crate) fn polygon_area(pts: &[DVec2]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        a += cross2(pts[i], pts[(i + 1) % n]);
    }
    a * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_geom2d::{Arc2, Line2};

    fn eval_nurbs(s: &Nurbs2, u: f64) -> DVec2 {
        // Cox–de Boor, for testing only.
        let p = s.degree as usize;
        let n = s.ctrl.len();
        let basis = |i: usize, u: f64| -> f64 {
            fn nip(k: &[f64], i: usize, p: usize, u: f64) -> f64 {
                if p == 0 {
                    let last = k[k.len() - 1];
                    return if (k[i] <= u && u < k[i + 1]) || (u == last && k[i] < u && u <= k[i + 1]) {
                        1.0
                    } else {
                        0.0
                    };
                }
                let mut v = 0.0;
                let d1 = k[i + p] - k[i];
                if d1 > 0.0 {
                    v += (u - k[i]) / d1 * nip(k, i, p - 1, u);
                }
                let d2 = k[i + p + 1] - k[i + 1];
                if d2 > 0.0 {
                    v += (k[i + p + 1] - u) / d2 * nip(k, i + 1, p - 1, u);
                }
                v
            }
            nip(&s.knots, i, p, u)
        };
        let mut num = DVec2::ZERO;
        let mut den = 0.0;
        for i in 0..n {
            let w = if s.weights.is_empty() { 1.0 } else { s.weights[i] };
            let b = basis(i, u) * w;
            num += s.ctrl[i] * b;
            den += b;
        }
        num / den
    }

    #[test]
    fn bulge_semicircle() {
        let Some(Seg::Arc { c, u, t0, dt, .. }) = bulge_arc(DVec2::ZERO, DVec2::new(10.0, 0.0), 1.0) else {
            panic!("expected arc")
        };
        assert!(c.distance(DVec2::new(5.0, 0.0)) < 1e-12);
        assert!((u.x - 5.0).abs() < 1e-12);
        assert!((dt - std::f64::consts::PI).abs() < 1e-12);
        assert!((t0.abs() - std::f64::consts::PI).abs() < 1e-12);
    }

    #[test]
    fn cubic_spline_decomposes_exactly() {
        let s = Nurbs2 {
            degree: 3,
            ctrl: vec![
                DVec2::new(0.0, 0.0),
                DVec2::new(1.0, 2.0),
                DVec2::new(3.0, 3.0),
                DVec2::new(4.0, 0.0),
                DVec2::new(6.0, -1.0),
                DVec2::new(7.0, 2.0),
            ],
            weights: vec![],
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 4.0, 4.0, 4.0, 4.0],
            fit_points: vec![],
            closed: false,
        };
        let (start, segs) = nurbs_segs(&s).expect("valid spline");
        assert_eq!(segs.len(), 3);
        assert!(start.distance(DVec2::ZERO) < 1e-12);
        // Segment ends are the curve at the knots.
        let ends: Vec<DVec2> = segs.iter().map(|s| if let Seg::Cubic(_, _, p) = s { *p } else { DVec2::NAN }).collect();
        for (e, u) in ends.iter().zip([1.0, 2.5, 4.0]) {
            assert!(e.distance(eval_nurbs(&s, u)) < 1e-9, "{e:?} vs {:?}", eval_nurbs(&s, u));
        }
        // Mid-span point of the first Bézier equals the curve at u = 0.5.
        if let Seg::Cubic(c1, c2, p3) = segs[0] {
            let t: f64 = 0.5;
            let mt = 1.0 - t;
            let b = start * mt.powi(3) + c1 * 3.0 * mt * mt * t + c2 * 3.0 * mt * t * t + p3 * t.powi(3);
            assert!(b.distance(eval_nurbs(&s, 0.5)) < 1e-9);
        }
    }

    #[test]
    fn sampled_matches_exact() {
        let ctrl: Vec<DVec2> = (0..40).map(|i| DVec2::new(i as f64, ((i * 7) % 5) as f64)).collect();
        let mut knots = vec![0.0; 4];
        knots.extend((1..37).map(|i| i as f64));
        knots.extend([37.0; 4]);
        let s = Nurbs2 { degree: 3, ctrl, weights: vec![], knots, fit_points: vec![], closed: false };
        let pts: Vec<H> = s.ctrl.iter().map(|p| [p.x, p.y, 1.0]).collect();
        let (s1, segs) = nurbs_sampled(3, &s.knots, &pts, 0.0, 37.0).expect("sampled");
        let (s2, exact) = nurbs_segs(&s).expect("exact");
        assert!(s1.distance(s2) < 1e-9);
        assert!(segs_end(s1, &segs).distance(segs_end(s2, &exact)) < 1e-9);
        assert_eq!(segs.len(), 37 * 4);
    }

    #[test]
    fn unclamped_and_bad_splines() {
        let s = Nurbs2 {
            degree: 2,
            ctrl: vec![DVec2::new(0.0, 0.0), DVec2::new(1.0, 1.0), DVec2::new(2.0, 0.0), DVec2::new(3.0, 1.0)],
            weights: vec![],
            knots: vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            fit_points: vec![],
            closed: false,
        };
        let (start, segs) = nurbs_segs(&s).expect("unclamped spline");
        assert!(start.distance(eval_nurbs(&s, 2.0)) < 1e-9);
        assert!(segs_end(start, &segs).distance(eval_nurbs(&s, 4.0 - 1e-12)) < 1e-6);
        let bad = Nurbs2 { knots: vec![0.0, 1.0], ..s.clone() };
        assert!(nurbs_segs(&bad).is_none());
        assert!(curve_segs(&Curve2::Spline(bad)).is_some(), "falls back to the control polygon");
    }

    #[test]
    fn loop_path_flips_reversed_pieces() {
        let curves = vec![
            Curve2::Line(Line2::new(DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0))),
            Curve2::Arc(Arc2::new(DVec2::new(10.0, 5.0), 5.0, -FRAC_PI_2, FRAC_PI_2)),
            // reversed on purpose: goes from (0,10) to (10,10)
            Curve2::Line(Line2::new(DVec2::new(0.0, 10.0), DVec2::new(10.0, 10.0))),
            Curve2::Line(Line2::new(DVec2::new(0.0, 10.0), DVec2::new(0.0, 0.0))),
        ];
        let mut path = Vec::new();
        loop_path(&curves, &mut path);
        let polys = flatten(&path, 0.01);
        assert_eq!(polys.len(), 1);
        let area = polygon_area(&polys[0]).abs();
        let expected = 100.0 + std::f64::consts::PI * 25.0 / 2.0;
        assert!((area - expected).abs() < 0.2, "area {area} vs {expected}");
    }

    #[test]
    fn transformed_arc_axes() {
        let m = DAffine2::from_scale(DVec2::new(2.0, 1.0));
        let mut p = curve_path(&Curve2::Circle(wcad_geom2d::Circle2::new(DVec2::ZERO, 1.0)));
        transform_path(&mut p, &m);
        let Seg::Arc { u, v, .. } = p[1] else { panic!() };
        let (rx, ry, ang) = ellipse_axes(u, v);
        assert!((rx - 2.0).abs() < 1e-12 && (ry - 1.0).abs() < 1e-12 && ang.abs() < 1e-12);
        let cubics = arc_to_cubics(DVec2::ZERO, u, v, 0.0, TAU);
        assert_eq!(cubics.len(), 4);
    }
}
