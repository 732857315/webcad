//! Editing primitives: trim, extend, fillet, chamfer, break, join.
//!
//! All functions are pure: they return new curves and never panic on degenerate input (they
//! return `None` / the input unchanged instead).

use std::f64::consts::{PI, TAU};

use wcad_math::{BBox2, DVec2, cross2, normalize_0_2pi, perp};

use crate::bulge::{BulgeArc, PolySegment, sweep_to_bulge};
use crate::curve::Curve;
use crate::curves::{Arc2, Circle2, Curve2, EllipseArc2, Line2, PolyVertex, Polyline2};
use crate::intersect::intersect;

/// Parameters on `target` where any cutter intersects it, sorted and de-duplicated.
fn cut_params(target: &Curve2, cutters: &[Curve2]) -> Vec<f64> {
    let (a, b) = target.domain();
    let closed = target.is_closed();
    let span = (b - a).max(1e-300);
    let mut ts: Vec<f64> = Vec::new();
    for c in cutters {
        for h in intersect(target, c) {
            let mut t = h.ta;
            if closed {
                t = a + (t - a).rem_euclid(span);
                if b - t < 1e-12 * span {
                    t = a;
                }
            } else if t <= a + 1e-9 * span || t >= b - 1e-9 * span {
                continue; // touching at an end does not cut
            }
            ts.push(t);
        }
    }
    ts.sort_by(|x, y| x.total_cmp(y));
    ts.dedup_by(|x, y| (*x - *y).abs() <= 1e-9 * span);
    ts
}

/// Trim `target` with `cutters`: remove the piece between the two cut points around `pick_point`
/// (or from a cut point to the curve end). Returns the remaining pieces (possibly empty), or `None`
/// when no cutter intersects the target (nothing to trim). Closed curves need two cut points.
pub fn trim(target: &Curve2, cutters: &[Curve2], pick_point: DVec2) -> Option<Vec<Curve2>> {
    let ts = cut_params(target, cutters);
    if ts.is_empty() {
        return None;
    }
    let (a, b) = target.domain();
    let (tp, _) = target.closest(pick_point);
    if target.is_closed() {
        if ts.len() < 2 {
            return None;
        }
        let lo = ts
            .iter()
            .copied()
            .rfind(|t| *t <= tp)
            .or_else(|| ts.last().copied())?;
        let hi = ts
            .iter()
            .copied()
            .find(|t| *t > tp)
            .or_else(|| ts.first().copied())?;
        return Some(target.sub_curve(hi, lo).into_iter().collect());
    }
    let lo = ts.iter().copied().rfind(|t| *t <= tp);
    let hi = ts.iter().copied().find(|t| *t > tp);
    let mut out = Vec::new();
    if let Some(lo) = lo {
        out.extend(target.sub_curve(a, lo));
    }
    if let Some(hi) = hi {
        out.extend(target.sub_curve(hi, b));
    }
    Some(out)
}

/// Extend the end of `target` nearest to `pick_point` to the nearest boundary. Lines, arcs,
/// elliptical arcs and open polylines (last/first segment) are supported. `None` if nothing is hit.
pub fn extend(target: &Curve2, boundaries: &[Curve2], pick_point: DVec2) -> Option<Curve2> {
    if target.is_closed() {
        return None;
    }
    let at_end = pick_point.distance(target.end()) <= pick_point.distance(target.start());
    let world = boundaries
        .iter()
        .fold(target.bbox(), |b, c| b.union(&c.bbox()));
    let reach = world.size().length() * 2.0 + 1.0;
    match target {
        Curve2::Line(l) => {
            let (p, d) = if at_end {
                (l.b, l.dir())
            } else {
                (l.a, -l.dir())
            };
            let hit = ray_hit(p, d, reach, boundaries)?;
            Some(Curve2::Line(if at_end {
                Line2::new(l.a, hit)
            } else {
                Line2::new(hit, l.b)
            }))
        }
        Curve2::Arc(arc) => {
            let delta = circle_extension(arc.c, arc.r, arc.start, arc.sweep(), at_end, boundaries)?;
            let sw = arc.sweep();
            Some(Curve2::Arc(if at_end {
                Arc2::new(arc.c, arc.r, arc.start, arc.start + sw + delta)
            } else {
                Arc2::new(arc.c, arc.r, arc.start - delta, arc.start + sw)
            }))
        }
        Curve2::Ellipse(e) => {
            let full = Curve2::Ellipse(EllipseArc2 {
                start: 0.0,
                end: TAU,
                ..*e
            });
            let sw = e.sweep();
            let mut best: Option<f64> = None;
            for b in boundaries {
                for h in intersect(&full, b) {
                    let d = if at_end {
                        normalize_0_2pi(h.ta - (e.start + sw))
                    } else {
                        normalize_0_2pi(e.start - h.ta)
                    };
                    if d > 1e-9 && d < TAU - sw - 1e-9 && best.is_none_or(|x| d < x) {
                        best = Some(d);
                    }
                }
            }
            let d = best?;
            Some(Curve2::Ellipse(if at_end {
                EllipseArc2 {
                    end: e.start + sw + d,
                    ..*e
                }
            } else {
                EllipseArc2 {
                    start: e.start - d,
                    end: e.start + sw,
                    ..*e
                }
            }))
        }
        Curve2::Polyline(pl) => {
            let n = pl.segment_count();
            if n == 0 {
                return None;
            }
            let idx = if at_end { n - 1 } else { 0 };
            let seg = pl.segment(idx)?;
            let mut verts = pl.verts.clone();
            match seg {
                PolySegment::Line(l) => {
                    let (p, d) = if at_end {
                        (l.b, l.dir())
                    } else {
                        (l.a, -l.dir())
                    };
                    let hit = ray_hit(p, d, reach, boundaries)?;
                    if at_end {
                        verts.last_mut()?.p = hit;
                    } else {
                        verts[0].p = hit;
                    }
                }
                PolySegment::Arc(ba) => {
                    let ccw = ba.to_arc();
                    // extending the segment end means going further in its own direction
                    let forward_end = at_end == ba.is_ccw();
                    let delta = circle_extension(
                        ccw.c,
                        ccw.r,
                        ccw.start,
                        ccw.sweep(),
                        forward_end,
                        boundaries,
                    )?;
                    let new_sweep = ba.sweep + delta * ba.sweep.signum();
                    let nb = BulgeArc {
                        sweep: new_sweep,
                        start: if at_end {
                            ba.start
                        } else {
                            ba.start - delta * ba.sweep.signum()
                        },
                        ..ba
                    };
                    if at_end {
                        verts[idx].bulge = sweep_to_bulge(new_sweep);
                        verts.last_mut()?.p = nb.point_at(1.0);
                    } else {
                        verts[0].p = nb.point_at(0.0);
                        verts[0].bulge = sweep_to_bulge(new_sweep);
                    }
                }
            }
            Some(Curve2::Polyline(Polyline2 {
                verts,
                closed: false,
            }))
        }
        Curve2::Circle(_) | Curve2::Spline(_) => None,
    }
}

/// Nearest boundary hit along the ray `p + d·s`, `s > 0`.
fn ray_hit(p: DVec2, d: DVec2, reach: f64, boundaries: &[Curve2]) -> Option<DVec2> {
    if d == DVec2::ZERO {
        return None;
    }
    let ray = Curve2::Line(Line2::new(p, p + d * reach));
    let eps = 1e-9 * reach;
    let mut best: Option<(f64, DVec2)> = None;
    for b in boundaries {
        for h in intersect(&ray, b) {
            let s = h.ta * reach;
            if s > eps && best.is_none_or(|x| s < x.0) {
                best = Some((s, h.p));
            }
        }
    }
    best.map(|x| x.1)
}

/// Smallest positive angular extension of a CCW arc beyond its end (`at_end`) or before its start.
fn circle_extension(
    c: DVec2,
    r: f64,
    start: f64,
    sweep: f64,
    at_end: bool,
    boundaries: &[Curve2],
) -> Option<f64> {
    let full = Curve2::Circle(Circle2::new(c, r));
    let end = start + sweep;
    let mut best: Option<f64> = None;
    for b in boundaries {
        for h in intersect(&full, b) {
            let d = if at_end {
                normalize_0_2pi(h.ta - end)
            } else {
                normalize_0_2pi(start - h.ta)
            };
            if d > 1e-9 && d < TAU - sweep - 1e-9 && best.is_none_or(|x| d < x) {
                best = Some(d);
            }
        }
    }
    best
}

// ------------------------------------------------------------------------------------------------
// fillet / chamfer

/// Result of [`fillet`]: the trimmed/extended inputs (`None` when a curve vanishes) and the
/// fillet arc (`None` for radius 0).
#[derive(Clone, Debug, PartialEq)]
pub struct FilletResult {
    pub a: Option<Curve2>,
    pub b: Option<Curve2>,
    pub arc: Option<Arc2>,
}

#[derive(Clone, Copy, Debug)]
enum Carrier {
    Line { p: DVec2, d: DVec2 },
    Circle { c: DVec2, r: f64 },
}

fn carrier(c: &Curve2) -> Option<Carrier> {
    match c {
        Curve2::Line(l) => {
            let d = l.dir();
            (d != DVec2::ZERO).then_some(Carrier::Line { p: l.a, d })
        }
        Curve2::Arc(a) => Some(Carrier::Circle { c: a.c, r: a.r }),
        Curve2::Circle(ci) => Some(Carrier::Circle { c: ci.c, r: ci.r }),
        _ => None,
    }
}

impl Carrier {
    /// Offset towards the side containing `side` by `r` (`None` if the circle vanishes).
    fn offset_towards(&self, side: DVec2, r: f64) -> Option<Carrier> {
        match *self {
            Carrier::Line { p, d } => {
                let s = cross2(d, side - p).signum();
                let s = if s == 0.0 { 1.0 } else { s };
                Some(Carrier::Line {
                    p: p + perp(d) * (r * s),
                    d,
                })
            }
            Carrier::Circle { c, r: cr } => {
                let inside = side.distance(c) < cr;
                let nr = if inside { cr - r } else { cr + r };
                (nr > 0.0).then_some(Carrier::Circle { c, r: nr })
            }
        }
    }
    fn foot(&self, q: DVec2) -> DVec2 {
        match *self {
            Carrier::Line { p, d } => p + d * (q - p).dot(d),
            Carrier::Circle { c, r } => c + (q - c).normalize_or(DVec2::X) * r,
        }
    }
}

fn carrier_hits(a: &Carrier, b: &Carrier) -> Vec<DVec2> {
    match (*a, *b) {
        (Carrier::Line { p: p1, d: d1 }, Carrier::Line { p: p2, d: d2 }) => {
            let den = cross2(d1, d2);
            if den.abs() < 1e-12 {
                return Vec::new();
            }
            let s = cross2(p2 - p1, d2) / den;
            vec![p1 + d1 * s]
        }
        (Carrier::Line { p, d }, Carrier::Circle { c, r })
        | (Carrier::Circle { c, r }, Carrier::Line { p, d }) => {
            let foot = p + d * (c - p).dot(d);
            let h = foot.distance(c);
            if h > r * (1.0 + 1e-12) {
                return Vec::new();
            }
            let half = (r * r - h * h).max(0.0).sqrt();
            if half <= 1e-12 * r {
                return vec![foot];
            }
            vec![foot - d * half, foot + d * half]
        }
        (Carrier::Circle { c: c1, r: r1 }, Carrier::Circle { c: c2, r: r2 }) => {
            let v = c2 - c1;
            let dist = v.length();
            if dist < 1e-300
                || dist > r1 + r2 + 1e-12 * (r1 + r2)
                || dist < (r1 - r2).abs() - 1e-12 * (r1 + r2)
            {
                return Vec::new();
            }
            let dir = v / dist;
            let a = (dist * dist + r1 * r1 - r2 * r2) / (2.0 * dist);
            let h = (r1 * r1 - a * a).max(0.0).sqrt();
            let base = c1 + dir * a;
            if h <= 1e-12 * r1 {
                return vec![base];
            }
            vec![base + perp(dir) * h, base - perp(dir) * h]
        }
    }
}

/// Trim/extend `c` (line or arc) so it ends at `t` (a point on its carrier), keeping the part on
/// the side of `pick`. Circles are returned unchanged.
fn trim_to(c: &Curve2, t: DVec2, pick: DVec2) -> Option<Curve2> {
    match c {
        Curve2::Line(l) => {
            let d = l.b - l.a;
            let l2 = d.length_squared();
            if l2 < 1e-300 {
                return None;
            }
            let st = (t - l.a).dot(d) / l2;
            let sp = (pick - l.a).dot(d) / l2;
            let nl = if sp >= st {
                Line2::new(t, l.b)
            } else {
                Line2::new(l.a, t)
            };
            (nl.length() > 1e-12 * l2.sqrt()).then_some(Curve2::Line(nl))
        }
        Curve2::Arc(a) => {
            let phi = (t - a.c).y.atan2((t - a.c).x);
            let sw = a.sweep();
            let end = a.start + sw;
            let na = if let Some(tp) = a.param_of_angle(phi, 1e-12) {
                let pp = a.closest(pick).0;
                if pp >= tp {
                    Arc2::new(a.c, a.r, tp, end)
                } else {
                    Arc2::new(a.c, a.r, a.start, tp)
                }
            } else {
                let de = normalize_0_2pi(phi - end);
                let ds = normalize_0_2pi(a.start - phi);
                if de <= ds {
                    Arc2::new(a.c, a.r, a.start, end + de)
                } else {
                    Arc2::new(a.c, a.r, a.start - ds, end)
                }
            };
            (na.sweep() * na.r > 1e-12 * a.r && na.sweep() < TAU - 1e-12).then_some(Curve2::Arc(na))
        }
        Curve2::Circle(_) => Some(c.clone()),
        _ => None,
    }
}

/// Fillet two curves (lines, arcs, circles) with radius `radius`. `pick_a`/`pick_b` are points on
/// the parts of `a`/`b` to keep. Radius 0 trims/extends both to their intersection.
pub fn fillet(
    a: &Curve2,
    b: &Curve2,
    radius: f64,
    pick_a: DVec2,
    pick_b: DVec2,
) -> Option<FilletResult> {
    if !(radius >= 0.0) || !radius.is_finite() {
        return None;
    }
    let ca = carrier(a)?;
    let cb = carrier(b)?;
    let pa = a.closest(pick_a).1;
    let pb = b.closest(pick_b).1;
    if radius == 0.0 {
        let x = carrier_hits(&ca, &cb).into_iter().min_by(|p, q| {
            (p.distance(pa) + p.distance(pb)).total_cmp(&(q.distance(pa) + q.distance(pb)))
        })?;
        return Some(FilletResult {
            a: trim_to(a, x, pa),
            b: trim_to(b, x, pb),
            arc: None,
        });
    }
    // candidate centers: offset carriers towards the other pick, plus all other sign combinations
    let mut cands: Vec<(DVec2, bool)> = Vec::new();
    let preferred = (ca.offset_towards(pb, radius), cb.offset_towards(pa, radius));
    if let (Some(oa), Some(ob)) = preferred {
        cands.extend(carrier_hits(&oa, &ob).into_iter().map(|p| (p, true)));
    }
    for sa in all_offsets(&ca, radius) {
        for sb in all_offsets(&cb, radius) {
            cands.extend(carrier_hits(&sa, &sb).into_iter().map(|p| (p, false)));
        }
    }
    let score = |o: DVec2| ca.foot(o).distance(pa) + cb.foot(o).distance(pb);
    let best = cands
        .iter()
        .filter(|c| c.1)
        .min_by(|x, y| score(x.0).total_cmp(&score(y.0)))
        .or_else(|| {
            cands
                .iter()
                .min_by(|x, y| score(x.0).total_cmp(&score(y.0)))
        })?
        .0;
    let ta = ca.foot(best);
    let tb = cb.foot(best);
    let a0 = (ta - best).y.atan2((ta - best).x);
    let b0 = (tb - best).y.atan2((tb - best).x);
    let arc = if normalize_0_2pi(b0 - a0) <= PI {
        Arc2::new(best, radius, a0, a0 + normalize_0_2pi(b0 - a0))
    } else {
        Arc2::new(best, radius, b0, b0 + normalize_0_2pi(a0 - b0))
    };
    Some(FilletResult {
        a: trim_to(a, ta, pa),
        b: trim_to(b, tb, pb),
        arc: Some(arc),
    })
}

fn all_offsets(c: &Carrier, r: f64) -> Vec<Carrier> {
    match *c {
        Carrier::Line { p, d } => vec![
            Carrier::Line {
                p: p + perp(d) * r,
                d,
            },
            Carrier::Line {
                p: p - perp(d) * r,
                d,
            },
        ],
        Carrier::Circle { c, r: cr } => {
            let mut v = vec![Carrier::Circle { c, r: cr + r }];
            if cr > r {
                v.push(Carrier::Circle { c, r: cr - r });
            }
            if r > cr {
                v.push(Carrier::Circle { c, r: r - cr });
            }
            v
        }
    }
}

/// Result of [`chamfer`].
#[derive(Clone, Debug, PartialEq)]
pub struct ChamferResult {
    pub a: Option<Line2>,
    pub b: Option<Line2>,
    /// The chamfer segment (`None` when both distances are 0).
    pub line: Option<Line2>,
}

/// Chamfer two lines: cut `d1` along `a` and `d2` along `b` from their intersection, keeping the
/// parts on the side of the picks.
pub fn chamfer(
    a: &Line2,
    b: &Line2,
    d1: f64,
    d2: f64,
    pick_a: DVec2,
    pick_b: DVec2,
) -> Option<ChamferResult> {
    if !(d1 >= 0.0 && d2 >= 0.0 && d1.is_finite() && d2.is_finite()) {
        return None;
    }
    let (da, db) = (a.dir(), b.dir());
    let x = carrier_hits(
        &Carrier::Line { p: a.a, d: da },
        &Carrier::Line { p: b.a, d: db },
    )
    .into_iter()
    .next()?;
    let side = |l: &Line2, d: DVec2, pick: DVec2| {
        let s = (pick - x).dot(d);
        if s.abs() > 1e-12 {
            d * s.signum()
        } else {
            // pick at the corner: keep the longer side
            let far = if l.a.distance(x) >= l.b.distance(x) {
                l.a
            } else {
                l.b
            };
            (far - x).normalize_or(d)
        }
    };
    let ua = side(a, da, pick_a);
    let ub = side(b, db, pick_b);
    let ca = x + ua * d1;
    let cb = x + ub * d2;
    let cut = |l: &Line2, u: DVec2, c: DVec2| -> Option<Line2> {
        // keep direction: the endpoint further along u stays
        let (sa, sb) = ((l.a - x).dot(u), (l.b - x).dot(u));
        let nl = if sa >= sb {
            Line2::new(l.a, c)
        } else {
            Line2::new(c, l.b)
        };
        (nl.length() > 1e-12 * (1.0 + l.length())).then_some(nl)
    };
    let line = (ca.distance(cb) > 0.0).then(|| Line2::new(ca, cb));
    Some(ChamferResult {
        a: cut(a, ua, ca),
        b: cut(b, ub, cb),
        line,
    })
}

// ------------------------------------------------------------------------------------------------
// break / join

/// Remove the part of `curve` between (the projections of) `p1` and `p2`. If the points coincide,
/// an open curve is split in two there. For closed curves the part from `p1` counter-clockwise
/// (along the curve direction) to `p2` is removed.
pub fn break_at(curve: &Curve2, p1: DVec2, p2: DVec2) -> Vec<Curve2> {
    let (a, b) = curve.domain();
    let (t1, _) = curve.closest(p1);
    let (t2, _) = curve.closest(p2);
    let span = (b - a).max(1e-300);
    if curve.is_closed() {
        if (t1 - t2).abs() <= 1e-12 * span {
            return vec![curve.clone()];
        }
        return curve.sub_curve(t2, t1).into_iter().collect();
    }
    let (lo, hi) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
    let mut out = Vec::new();
    out.extend(curve.sub_curve(a, lo));
    out.extend(curve.sub_curve(if hi - lo <= 1e-12 * span { lo } else { hi }, b));
    out
}

fn to_segments(c: &Curve2) -> Option<Vec<PolySegment>> {
    match c {
        Curve2::Line(l) => Some(vec![PolySegment::Line(*l)]),
        Curve2::Arc(a) => Some(vec![PolySegment::Arc(BulgeArc {
            c: a.c,
            r: a.r,
            start: a.start,
            sweep: a.sweep(),
        })]),
        Curve2::Polyline(p) if !p.closed => Some(p.segments().collect()),
        _ => None,
    }
}

fn reverse_segments(v: &[PolySegment]) -> Vec<PolySegment> {
    v.iter()
        .rev()
        .map(|s| match s {
            PolySegment::Line(l) => PolySegment::Line(Line2::new(l.b, l.a)),
            PolySegment::Arc(a) => PolySegment::Arc(BulgeArc {
                start: a.start + a.sweep,
                sweep: -a.sweep,
                ..*a
            }),
        })
        .collect()
}

/// Join curves that connect head-to-tail within `tol` into one curve: collinear lines → line,
/// co-circular arcs → arc (or circle), otherwise an (open or closed) bulge polyline. `None` if the
/// curves do not form a single chain or contain curves that cannot be joined (ellipses, splines,
/// circles, closed polylines).
pub fn join(curves: &[Curve2], tol: f64) -> Option<Curve2> {
    if curves.is_empty() {
        return None;
    }
    if curves.len() == 1 {
        return Some(curves[0].clone());
    }
    let tol = if tol > 0.0 { tol } else { 1e-9 };
    let mut pieces: Vec<Vec<PolySegment>> = Vec::with_capacity(curves.len());
    for c in curves {
        let s = to_segments(c)?;
        if s.is_empty() {
            return None;
        }
        pieces.push(s);
    }
    let mut used = vec![false; pieces.len()];
    let mut chain: Vec<PolySegment> = pieces[0].clone();
    used[0] = true;
    let mut remaining = pieces.len() - 1;
    while remaining > 0 {
        let head = chain.first()?.start();
        let tail = chain.last()?.end();
        let mut progressed = false;
        for i in 0..pieces.len() {
            if used[i] {
                continue;
            }
            let (s, e) = (pieces[i].first()?.start(), pieces[i].last()?.end());
            if s.distance(tail) <= tol {
                chain.extend_from_slice(&pieces[i]);
            } else if e.distance(tail) <= tol {
                chain.extend(reverse_segments(&pieces[i]));
            } else if e.distance(head) <= tol {
                let mut v = pieces[i].clone();
                v.extend_from_slice(&chain);
                chain = v;
            } else if s.distance(head) <= tol {
                let mut v = reverse_segments(&pieces[i]);
                v.extend_from_slice(&chain);
                chain = v;
            } else {
                continue;
            }
            used[i] = true;
            remaining -= 1;
            progressed = true;
        }
        if !progressed {
            return None;
        }
    }
    let first = chain.first()?.start();
    let last = chain.last()?.end();
    let closed = first.distance(last) <= tol && chain.len() > 1;
    // all collinear lines → one line
    if !closed && chain.iter().all(|s| matches!(s, PolySegment::Line(_))) {
        let dir = (last - first).normalize_or_zero();
        let straight = dir != DVec2::ZERO
            && chain.iter().all(|s| {
                let (p, q) = (s.start(), s.end());
                cross2(dir, p - first).abs() <= tol
                    && cross2(dir, q - first).abs() <= tol
                    && (q - p).dot(dir) > 0.0
            });
        if straight {
            return Some(Curve2::Line(Line2::new(first, last)));
        }
    }
    // co-circular arcs in one direction → one arc / circle
    if let PolySegment::Arc(a0) = chain[0]
        && chain.iter().all(|s| matches!(s, PolySegment::Arc(a) if a.c.distance(a0.c) <= tol && (a.r - a0.r).abs() <= tol && a.sweep.signum() == a0.sweep.signum()))
    {
        let total: f64 = chain.iter().map(|s| if let PolySegment::Arc(a) = s { a.sweep } else { 0.0 }).sum();
        if total.abs() >= TAU - 1e-9 {
            return Some(Curve2::Circle(Circle2::new(a0.c, a0.r)));
        }
        if total.abs() < TAU {
            let ba = BulgeArc { sweep: total, ..a0 };
            if ba.is_ccw() {
                return Some(Curve2::Arc(ba.to_arc()));
            }
        }
    }
    let mut verts: Vec<PolyVertex> = chain
        .iter()
        .map(|s| PolyVertex::with_bulge(s.start(), s.bulge()))
        .collect();
    if !closed {
        verts.push(PolyVertex::new(last));
    }
    Some(Curve2::Polyline(Polyline2 { verts, closed }))
}

/// Bounding box of a set of curves (helper for tools).
pub fn bbox_of(curves: &[Curve2]) -> BBox2 {
    curves.iter().fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::*;
    use std::f64::consts::FRAC_PI_2;

    fn line(x0: f64, y0: f64, x1: f64, y1: f64) -> Curve2 {
        Curve2::Line(Line2::new(DVec2::new(x0, y0), DVec2::new(x1, y1)))
    }

    #[test]
    fn trim_line_and_circle() {
        let t = line(0.0, 0.0, 10.0, 0.0);
        let cutters = [line(3.0, -1.0, 3.0, 1.0), line(7.0, -1.0, 7.0, 1.0)];
        let r = trim(&t, &cutters, DVec2::new(5.0, 0.1)).unwrap();
        assert_eq!(r, vec![line(0.0, 0.0, 3.0, 0.0), line(7.0, 0.0, 10.0, 0.0)]);
        let r = trim(&t, &cutters, DVec2::new(9.0, 0.0)).unwrap();
        assert_eq!(r, vec![line(0.0, 0.0, 7.0, 0.0)]);
        assert!(trim(&t, &[line(20.0, -1.0, 20.0, 1.0)], DVec2::ZERO).is_none());
        // circle trimmed by a line through it: keep the far side
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0));
        let r = trim(&c, &[line(-10.0, 3.0, 10.0, 3.0)], DVec2::new(0.0, 5.0)).unwrap();
        assert_eq!(r.len(), 1);
        let Curve2::Arc(a) = r[0] else {
            panic!("{:?}", r[0])
        };
        assert!(a.mid_point().distance(DVec2::new(0.0, -5.0)) < 1e-9);
        // closed polyline
        let sq = Curve2::Polyline(Polyline2::from_points(
            [
                DVec2::ZERO,
                DVec2::new(4.0, 0.0),
                DVec2::new(4.0, 4.0),
                DVec2::new(0.0, 4.0),
            ],
            true,
        ));
        let r = trim(&sq, &[line(2.0, -1.0, 2.0, 5.0)], DVec2::new(4.0, 2.0)).unwrap();
        assert_eq!(r.len(), 1);
        assert!((r[0].length() - 8.0).abs() < 1e-9);
        assert!(r[0].start().distance(DVec2::new(2.0, 4.0)) < 1e-9);
    }

    #[test]
    fn extend_curves() {
        let t = line(0.0, 0.0, 5.0, 0.0);
        let b = [line(8.0, -1.0, 8.0, 1.0), line(12.0, -1.0, 12.0, 1.0)];
        assert_eq!(
            extend(&t, &b, DVec2::new(4.0, 0.0)).unwrap(),
            line(0.0, 0.0, 8.0, 0.0)
        );
        assert!(extend(&t, &b, DVec2::new(1.0, 0.0)).is_none());
        let a = Curve2::Arc(Arc2::new(DVec2::ZERO, 5.0, 0.0, FRAC_PI_2 * 0.5));
        let wall = [line(-10.0, 4.0, 10.0, 4.0)];
        let Curve2::Arc(ea) = extend(&a, &wall, DVec2::new(3.0, 3.0)).unwrap() else {
            panic!()
        };
        assert!((ea.end_point().y - 4.0).abs() < 1e-9);
        assert!(ea.end_point().x > 0.0);
        // polyline end segment
        let pl = Curve2::Polyline(Polyline2::from_points(
            [DVec2::new(0.0, 5.0), DVec2::ZERO, DVec2::new(5.0, 0.0)],
            false,
        ));
        let Curve2::Polyline(ep) = extend(&pl, &b, DVec2::new(5.0, 0.0)).unwrap() else {
            panic!()
        };
        assert_eq!(ep.verts.last().unwrap().p, DVec2::new(8.0, 0.0));
        // polyline ending with an arc segment (CW)
        let pa = Curve2::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::new(DVec2::new(-5.0, 0.0)),
                PolyVertex::with_bulge(DVec2::new(0.0, 5.0), sweep_to_bulge(-FRAC_PI_2 * 0.5)),
                PolyVertex::new(DVec2::new(5.0 * (0.25 * PI).cos(), 5.0 * (0.25 * PI).sin())),
            ],
            closed: false,
        });
        let wall2 = [line(-10.0, 2.0, 10.0, 2.0)];
        let Curve2::Polyline(ep) = extend(&pa, &wall2, pa.end()).unwrap() else {
            panic!()
        };
        let end = ep.verts.last().unwrap().p;
        assert!((end.y - 2.0).abs() < 1e-9 && end.x > 0.0, "{end}");
        assert!((end.length() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn fillet_lines() {
        let a = line(0.0, 0.0, 10.0, 0.0);
        let b = line(10.0, -2.0, 10.0, 10.0);
        let r = fillet(&a, &b, 2.0, DVec2::new(3.0, 0.0), DVec2::new(10.0, 5.0)).unwrap();
        let arc = r.arc.unwrap();
        assert!(arc.c.distance(DVec2::new(8.0, 2.0)) < 1e-9);
        assert!((arc.sweep() - FRAC_PI_2).abs() < 1e-9);
        assert_eq!(r.a.unwrap(), line(0.0, 0.0, 8.0, 0.0));
        assert_eq!(r.b.unwrap(), line(10.0, 2.0, 10.0, 10.0));
        // lines that don't meet: extended
        let b2 = line(12.0, 3.0, 12.0, 10.0);
        let r = fillet(&a, &b2, 1.0, DVec2::new(3.0, 0.0), DVec2::new(12.0, 5.0)).unwrap();
        assert!(r.arc.unwrap().c.distance(DVec2::new(11.0, 1.0)) < 1e-9);
        assert_eq!(r.a.unwrap(), line(0.0, 0.0, 11.0, 0.0));
        // radius 0: corner
        let r = fillet(&a, &b2, 0.0, DVec2::new(3.0, 0.0), DVec2::new(12.0, 5.0)).unwrap();
        assert_eq!(r.a.unwrap(), line(0.0, 0.0, 12.0, 0.0));
        assert_eq!(r.b.unwrap(), line(12.0, 0.0, 12.0, 10.0));
        assert!(r.arc.is_none());
    }

    #[test]
    fn fillet_line_arc_and_arcs() {
        let l = line(-10.0, 0.0, 10.0, 0.0);
        let c = Curve2::Arc(Arc2::new(DVec2::new(0.0, 5.0), 3.0, PI, TAU));
        let r = fillet(&l, &c, 2.0, DVec2::new(-6.0, 0.0), DVec2::new(-3.0, 5.0)).unwrap();
        let arc = r.arc.unwrap();
        // tangent to the line (center at y=2) and externally to the circle (distance 5)
        assert!((arc.c.y - 2.0).abs() < 1e-9);
        assert!((arc.c.distance(DVec2::new(0.0, 5.0)) - 5.0).abs() < 1e-9);
        assert!(arc.c.x < 0.0);
        let na = r.a.unwrap();
        assert!(
            na.end().distance(DVec2::new(arc.c.x, 0.0)) < 1e-9
                || na.start().distance(DVec2::new(arc.c.x, 0.0)) < 1e-9
        );
        // two arcs
        let a1 = Curve2::Arc(Arc2::new(DVec2::new(-4.0, 0.0), 3.0, -FRAC_PI_2, FRAC_PI_2));
        let a2 = Curve2::Arc(Arc2::new(DVec2::new(4.0, 0.0), 3.0, FRAC_PI_2, 1.5 * PI));
        let r = fillet(&a1, &a2, 2.0, DVec2::new(-1.0, 0.5), DVec2::new(1.0, 0.5)).unwrap();
        let arc = r.arc.unwrap();
        assert!((arc.c.distance(DVec2::new(-4.0, 0.0)) - 5.0).abs() < 1e-9);
        assert!((arc.c.distance(DVec2::new(4.0, 0.0)) - 5.0).abs() < 1e-9);
        assert!(arc.c.y > 0.0);
        for (orig, new) in [(&a1, r.a.unwrap()), (&a2, r.b.unwrap())] {
            let (Curve2::Arc(o), Curve2::Arc(n)) = (orig, new) else {
                panic!()
            };
            assert!(n.c == o.c && n.r == o.r);
        }
    }

    #[test]
    fn chamfer_lines() {
        let a = Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0));
        let b = Line2::new(DVec2::new(10.0, 0.0), DVec2::new(10.0, 10.0));
        let r = chamfer(
            &a,
            &b,
            2.0,
            3.0,
            DVec2::new(1.0, 0.0),
            DVec2::new(10.0, 9.0),
        )
        .unwrap();
        assert_eq!(r.a.unwrap(), Line2::new(DVec2::ZERO, DVec2::new(8.0, 0.0)));
        assert_eq!(
            r.b.unwrap(),
            Line2::new(DVec2::new(10.0, 3.0), DVec2::new(10.0, 10.0))
        );
        assert_eq!(
            r.line.unwrap(),
            Line2::new(DVec2::new(8.0, 0.0), DVec2::new(10.0, 3.0))
        );
        assert!(
            chamfer(
                &a,
                &Line2::new(DVec2::Y, DVec2::new(5.0, 1.0)),
                1.0,
                1.0,
                DVec2::ZERO,
                DVec2::Y
            )
            .is_none()
        );
    }

    #[test]
    fn break_and_join() {
        let l = line(0.0, 0.0, 10.0, 0.0);
        let r = break_at(&l, DVec2::new(3.0, 1.0), DVec2::new(6.0, -1.0));
        assert_eq!(r, vec![line(0.0, 0.0, 3.0, 0.0), line(6.0, 0.0, 10.0, 0.0)]);
        let r = break_at(&l, DVec2::new(4.0, 0.0), DVec2::new(4.0, 0.0));
        assert_eq!(r, vec![line(0.0, 0.0, 4.0, 0.0), line(4.0, 0.0, 10.0, 0.0)]);
        let c = Curve2::Circle(Circle2::new(DVec2::ZERO, 1.0));
        let r = break_at(&c, DVec2::new(1.0, 0.0), DVec2::new(0.0, 1.0));
        let Curve2::Arc(a) = r[0] else { panic!() };
        assert!((a.sweep() - 1.5 * PI).abs() < 1e-9);
        // join: collinear lines
        assert_eq!(
            join(&[line(0.0, 0.0, 2.0, 0.0), line(5.0, 0.0, 2.0, 0.0)], 1e-9).unwrap(),
            line(0.0, 0.0, 5.0, 0.0)
        );
        // join: arcs of one circle
        let a1 = Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, 0.0, 1.0));
        let a2 = Curve2::Arc(Arc2::new(DVec2::ZERO, 1.0, 1.0, 2.5));
        let Curve2::Arc(j) = join(&[a2.clone(), a1.clone()], 1e-9).unwrap() else {
            panic!()
        };
        assert!((j.sweep() - 2.5).abs() < 1e-12);
        // join: rectangle of lines + an arc → closed polyline
        let parts = [
            line(0.0, 0.0, 4.0, 0.0),
            line(4.0, 4.0, 0.0, 4.0),
            line(0.0, 4.0, 0.0, 0.0),
            Curve2::Arc(Arc2::new(DVec2::new(4.0, 2.0), 2.0, -FRAC_PI_2, FRAC_PI_2)),
        ];
        let Curve2::Polyline(p) = join(&parts, 1e-9).unwrap() else {
            panic!()
        };
        assert!(p.closed);
        assert_eq!(p.verts.len(), 4);
        assert!((p.signed_area().abs() - (16.0 + 2.0 * PI)).abs() < 1e-9);
        // not connected
        assert!(join(&[line(0.0, 0.0, 1.0, 0.0), line(3.0, 0.0, 4.0, 0.0)], 1e-9).is_none());
        let pts = p.verts.iter().map(|v| v.p).collect::<Vec<_>>();
        assert!(pts.contains(&DVec2::new(4.0, 4.0)));
    }
}
