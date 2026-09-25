//! Closed regions of a planar curve arrangement (sketch profiles, hatch boundaries, "pick inside").
//!
//! Algorithm: split every curve at all mutual intersections (and where an endpoint lies within
//! `tol` of another curve), merge endpoints within `tol`, drop duplicate and dangling edges, build
//! a half-edge structure with the outgoing edges at each vertex sorted by tangent angle then
//! curvature, walk the faces (face on the left), and keep the positive (counter-clockwise) ones.
//! Clockwise faces are the outer boundaries of connected components; they become holes of the
//! smallest face of another component that contains them.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wcad_math::{BBox2, DVec2, cross2};

use crate::bulge::PolySegment;
use crate::curve::Curve;
use crate::curves::{Arc2, Curve2};
use crate::intersect::{default_tol, intersect_tol};
use crate::numeric::SafeClamp;
use crate::numeric::integrate;

/// A closed, head-to-tail chain of curves. Outer loops are counter-clockwise, holes clockwise.
/// `sources[i]` is the index (into the input slice given to [`find_regions`]) of the curve that
/// `curves[i]` was cut from, so callers can name generated faces after their source entities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loop {
    pub curves: Vec<Curve2>,
    pub sources: Vec<usize>,
}

/// A face of the arrangement: one outer loop plus zero or more hole loops strictly inside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Region {
    pub outer: Loop,
    pub holes: Vec<Loop>,
}

impl AsRef<[Curve2]> for Loop {
    fn as_ref(&self) -> &[Curve2] {
        &self.curves
    }
}

impl Loop {
    /// Exact signed area (positive = counter-clockwise), arcs and ellipses in closed form.
    pub fn signed_area(&self) -> f64 {
        signed_area(&self.curves)
    }
    pub fn bbox(&self) -> BBox2 {
        self.curves
            .iter()
            .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()))
    }
    /// Polygon approximation (closing point not repeated).
    pub fn to_polygon(&self, tol: f64) -> Vec<DVec2> {
        loop_polygon(&self.curves, tol)
    }
    /// Even-odd containment test (boundary points are unspecified).
    pub fn contains(&self, p: DVec2) -> bool {
        point_in_loop(&self.curves, p)
    }
}

impl Region {
    /// Area of the outer loop minus the holes.
    pub fn area(&self) -> f64 {
        self.outer.signed_area().abs()
            - self
                .holes
                .iter()
                .map(|h| h.signed_area().abs())
                .sum::<f64>()
    }
    /// Area centroid.
    pub fn centroid(&self) -> DVec2 {
        let o = self
            .outer
            .curves
            .first()
            .map(|c| c.start())
            .unwrap_or(DVec2::ZERO);
        let (mut a, mut m) = moments(&self.outer.curves, o);
        if a < 0.0 {
            a = -a;
            m = -m;
        }
        for h in &self.holes {
            let (ha, hm) = moments(&h.curves, o);
            let (ha, hm) = if ha < 0.0 { (-ha, -hm) } else { (ha, hm) };
            a -= ha;
            m -= hm;
        }
        if a.abs() < 1e-300 { o } else { o + m / a }
    }
    /// `true` if `p` is inside the outer loop and outside every hole.
    pub fn contains(&self, p: DVec2) -> bool {
        self.outer.contains(p) && !self.holes.iter().any(|h| h.contains(p))
    }
    pub fn bbox(&self) -> BBox2 {
        self.outer.bbox()
    }
    /// All loops (outer first).
    pub fn loops(&self) -> impl Iterator<Item = &Loop> {
        std::iter::once(&self.outer).chain(self.holes.iter())
    }
}

// ------------------------------------------------------------------------------------------------
// area / moments / containment helpers

/// `½∮(x dy − y dx)` along `c`, with coordinates taken relative to `o` (exact for lines, arcs,
/// circles, ellipses and bulge polylines; numeric for splines).
pub fn area_term(c: &Curve2, o: DVec2) -> f64 {
    match c {
        Curve2::Line(l) => 0.5 * cross2(l.a - o, l.b - o),
        Curve2::Circle(ci) => std::f64::consts::PI * ci.r * ci.r,
        Curve2::Arc(a) => arc_term(a.c - o, a.r, a.start, a.sweep()),
        Curve2::Ellipse(e) => {
            let (t0, t1) = e.domain();
            let (p0, p1) = (e.at_param(t0), e.at_param(t1));
            0.5 * (cross2(e.c - o, p1 - p0) + cross2(e.major, e.minor()) * (t1 - t0))
        }
        Curve2::Polyline(p) => {
            let mut s = 0.0;
            for seg in p.segments() {
                s += match seg {
                    PolySegment::Line(l) => 0.5 * cross2(l.a - o, l.b - o),
                    PolySegment::Arc(a) => arc_term(a.c - o, a.r, a.start, a.sweep),
                };
            }
            s
        }
        Curve2::Spline(_) => c.with_dyn(|cd| {
            let (a, b) = cd.domain();
            let pieces = 16usize;
            0.5 * integrate(
                &|t| cross2(cd.point_at(t) - o, cd.deriv_at(t)),
                a,
                b,
                pieces,
                1e-12,
            )
        }),
    }
}

fn arc_term(c: DVec2, r: f64, start: f64, sweep: f64) -> f64 {
    let p0 = DVec2::new(start.cos(), start.sin()) * r;
    let p1 = DVec2::new((start + sweep).cos(), (start + sweep).sin()) * r;
    0.5 * (r * r * sweep + cross2(c, p1 - p0))
}

/// Exact signed area of a closed chain of curves (positive = counter-clockwise).
pub fn signed_area(curves: &[Curve2]) -> f64 {
    let o = curves.first().map(|c| c.start()).unwrap_or(DVec2::ZERO);
    curves.iter().map(|c| area_term(c, o)).sum()
}

/// Area and first moments `(A, ∫∫(p − o) dA)` of a closed chain (signed by orientation).
fn moments(curves: &[Curve2], o: DVec2) -> (f64, DVec2) {
    let mut a = 0.0;
    let mut m = DVec2::ZERO;
    for c in curves {
        a += area_term(c, o);
        c.with_dyn(|cd| {
            let (t0, t1) = cd.domain();
            let pieces = match c {
                Curve2::Line(_) => 1,
                Curve2::Polyline(p) => p.segment_count().max(1) * 4,
                _ => 16,
            };
            let mx = integrate(
                &|t| {
                    let p = cd.point_at(t) - o;
                    0.5 * p.x * p.x * cd.deriv_at(t).y
                },
                t0,
                t1,
                pieces,
                1e-12,
            );
            let my = integrate(
                &|t| {
                    let p = cd.point_at(t) - o;
                    -0.5 * p.y * p.y * cd.deriv_at(t).x
                },
                t0,
                t1,
                pieces,
                1e-12,
            );
            m += DVec2::new(mx, my);
        });
    }
    (a, m)
}

/// Area centroid of a closed chain of curves.
pub fn centroid(curves: &[Curve2]) -> DVec2 {
    let o = curves.first().map(|c| c.start()).unwrap_or(DVec2::ZERO);
    let (a, m) = moments(curves, o);
    if a.abs() < 1e-300 { o } else { o + m / a }
}

fn loop_polygon(curves: &[Curve2], tol: f64) -> Vec<DVec2> {
    let mut pts: Vec<DVec2> = Vec::new();
    for c in curves {
        let f = c.flatten(tol);
        let skip = usize::from(!pts.is_empty());
        pts.extend(f.into_iter().skip(skip));
    }
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

/// Even-odd containment of `p` in the closed chain `curves` (flattened with a tolerance relative
/// to the chain's size).
pub fn point_in_loop(curves: &[Curve2], p: DVec2) -> bool {
    let bb = curves.iter().fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()));
    if !bb.contains(p) {
        return false;
    }
    let tol = (bb.size().length() * 1e-6).max(1e-12);
    point_in_polygon(&loop_polygon(curves, tol), p)
}

// ------------------------------------------------------------------------------------------------
// arrangement

struct Edge {
    curve: Curve2,
    src: usize,
    v0: usize,
    v1: usize,
    alive: bool,
}

struct VertexGrid {
    cell: f64,
    map: HashMap<(i64, i64), Vec<usize>>,
    pts: Vec<DVec2>,
    tol: f64,
}

impl VertexGrid {
    fn new(tol: f64) -> Self {
        Self {
            cell: (tol * 2.0).max(1e-300),
            map: HashMap::new(),
            pts: Vec::new(),
            tol,
        }
    }
    fn key(&self, p: DVec2) -> (i64, i64) {
        let k = |v: f64| {
            let q = (v / self.cell).floor();
            if q.is_finite() {
                q.clamp(-9e15, 9e15) as i64
            } else {
                0
            }
        };
        (k(p.x), k(p.y))
    }
    fn get_or_add(&mut self, p: DVec2) -> usize {
        let (kx, ky) = self.key(p);
        let mut best: Option<(usize, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(ids) = self.map.get(&(kx + dx, ky + dy)) {
                    for &i in ids {
                        let d = self.pts[i].distance(p);
                        if d <= self.tol && best.is_none_or(|b| d < b.1) {
                            best = Some((i, d));
                        }
                    }
                }
            }
        }
        if let Some((i, _)) = best {
            return i;
        }
        let id = self.pts.len();
        self.pts.push(p);
        self.map.entry((kx, ky)).or_default().push(id);
        id
    }
}

/// Split input curves into arrangement pieces `(curve, source)`: polylines become segments.
fn primitive_pieces(curves: &[Curve2]) -> Vec<(Curve2, usize)> {
    let mut out = Vec::new();
    for (i, c) in curves.iter().enumerate() {
        match c {
            Curve2::Polyline(p) => {
                for s in p.segments() {
                    match s {
                        PolySegment::Line(l) => out.push((Curve2::Line(l), i)),
                        PolySegment::Arc(a) => out.push((Curve2::Arc(a.to_arc()), i)),
                    }
                }
            }
            Curve2::Spline(s) if !s.is_valid() => {
                for seg in s.fallback_polyline().segments() {
                    if let PolySegment::Line(l) = seg {
                        out.push((Curve2::Line(l), i));
                    }
                }
            }
            _ => out.push((c.clone(), i)),
        }
    }
    out.retain(|(c, _)| c.length() > 0.0 && c.bbox().size().is_finite());
    out
}

/// Planar arrangement edges after splitting, merging and pruning.
fn build_edges(curves: &[Curve2], tol: f64) -> (Vec<Edge>, Vec<DVec2>) {
    let pieces = primitive_pieces(curves);
    let n = pieces.len();
    let boxes: Vec<BBox2> = pieces.iter().map(|(c, _)| c.bbox().expanded(tol)).collect();
    let mut params: Vec<Vec<f64>> = pieces.iter().map(|_| Vec::new()).collect();
    for i in 0..n {
        for j in i + 1..n {
            if !boxes[i].intersects(&boxes[j]) {
                continue;
            }
            let (a, b) = (&pieces[i].0, &pieces[j].0);
            let itol = default_tol(a, b) * 10.0;
            for h in intersect_tol(a, b, itol) {
                params[i].push(h.ta);
                params[j].push(h.tb);
            }
        }
    }
    // endpoints lying within tol of another piece (T-junctions with small gaps)
    for i in 0..n {
        let c = &pieces[i].0;
        if c.is_closed() {
            continue;
        }
        for p in [c.start(), c.end()] {
            for j in 0..n {
                if j == i || !boxes[j].contains(p) {
                    continue;
                }
                let (t, q) = pieces[j].0.closest(p);
                if q.distance(p) <= tol {
                    params[j].push(t);
                }
            }
        }
    }
    let mut grid = VertexGrid::new(tol);
    let mut edges: Vec<Edge> = Vec::new();
    for (k, (c, src)) in pieces.iter().enumerate() {
        let (a, b) = c.domain();
        let closed = c.is_closed();
        let mut ts: Vec<f64> = params[k]
            .iter()
            .copied()
            .filter(|t| t.is_finite())
            .map(|t| t.sclamp(a, b))
            .collect();
        if !closed {
            ts.push(a);
            ts.push(b);
        } else if ts.contains(&b) {
            // the seam parameter b is the same point as a
            for t in &mut ts {
                if *t == b {
                    *t = a;
                }
            }
        }
        ts.sort_by(|x, y| x.total_cmp(y));
        // merge parameters whose points coincide within tol
        let mut uniq: Vec<f64> = Vec::new();
        for t in ts {
            if uniq
                .last()
                .is_none_or(|l| c.point_at(*l).distance(c.point_at(t)) > tol)
            {
                uniq.push(t);
            }
        }
        if closed {
            if uniq.len() >= 2
                && let (Some(f), Some(l)) = (uniq.first().copied(), uniq.last().copied())
                && c.point_at(f).distance(c.point_at(l)) <= tol
            {
                uniq.pop();
            }
            if uniq.is_empty() {
                uniq.push(a);
            }
            if uniq.len() == 1 {
                let t = uniq[0];
                let mid = if t + 0.5 * (b - a) <= b {
                    t + 0.5 * (b - a)
                } else {
                    t - 0.5 * (b - a)
                };
                uniq.push(mid);
                uniq.sort_by(|x, y| x.total_cmp(y));
            }
        }
        let mut spans: Vec<(f64, f64)> = uniq.windows(2).map(|w| (w[0], w[1])).collect();
        if closed && let (Some(f), Some(l)) = (uniq.first().copied(), uniq.last().copied()) {
            spans.push((l, f));
        }
        for (t0, t1) in spans {
            let Some(sub) = c.sub_curve(t0, t1) else {
                continue;
            };
            if sub.length() <= tol {
                continue;
            }
            let v0 = grid.get_or_add(sub.start());
            let v1 = grid.get_or_add(sub.end());
            if v0 == v1 && sub.length() <= 4.0 * tol {
                continue;
            }
            edges.push(Edge {
                curve: sub,
                src: *src,
                v0,
                v1,
                alive: true,
            });
        }
    }
    // duplicates: same end vertices and same midpoint
    let mids: Vec<DVec2> = edges.iter().map(|e| mid_point(&e.curve)).collect();
    let mut by_ends: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        by_ends
            .entry((e.v0.min(e.v1), e.v0.max(e.v1)))
            .or_default()
            .push(i);
    }
    for ids in by_ends.values() {
        for (x, &i) in ids.iter().enumerate() {
            for &j in &ids[..x] {
                if edges[j].alive && mids[i].distance(mids[j]) <= tol * 4.0 {
                    edges[i].alive = false;
                    break;
                }
            }
        }
    }
    // prune dangling edges
    let nv = grid.pts.len();
    let mut deg = vec![0usize; nv];
    for e in edges.iter().filter(|e| e.alive) {
        deg[e.v0] += 1;
        deg[e.v1] += 1;
    }
    loop {
        let mut changed = false;
        for e in edges.iter_mut().filter(|e| e.alive) {
            if deg[e.v0] <= 1 || deg[e.v1] <= 1 {
                e.alive = false;
                deg[e.v0] -= 1;
                deg[e.v1] -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    edges.retain(|e| e.alive);
    (edges, grid.pts)
}

fn mid_point(c: &Curve2) -> DVec2 {
    let (a, b) = c.domain();
    c.point_at(0.5 * (a + b))
}

/// Outgoing direction and curvature of a curve at its start (robust to vanishing derivatives).
fn start_dir(c: &Curve2) -> (DVec2, f64) {
    let (a, b) = c.domain();
    let mut d = c.deriv_at(a);
    if d.length_squared() < 1e-24 * (1.0 + c.bbox().size().length_squared()) {
        d = c.point_at(a + (b - a) * 1e-6) - c.start();
    }
    (d.normalize_or_zero(), c.curvature_at(a))
}

fn end_dir_reversed(c: &Curve2) -> (DVec2, f64) {
    let (a, b) = c.domain();
    let mut d = c.deriv_at(b);
    if d.length_squared() < 1e-24 * (1.0 + c.bbox().size().length_squared()) {
        d = c.end() - c.point_at(b - (b - a) * 1e-6);
    }
    (-d.normalize_or_zero(), -c.curvature_at(b))
}

struct Face {
    halfedges: Vec<usize>,
    area: f64,
}

fn walk_faces(edges: &[Edge], nv: usize) -> Vec<Face> {
    let nh = edges.len() * 2;
    // half-edge h: edge h/2, forward if h even
    let origin = |h: usize| {
        if h.is_multiple_of(2) {
            edges[h / 2].v0
        } else {
            edges[h / 2].v1
        }
    };
    let mut out_by_v: Vec<Vec<(usize, f64, f64)>> = vec![Vec::new(); nv];
    for h in 0..nh {
        let e = &edges[h / 2];
        let (d, k) = if h.is_multiple_of(2) {
            start_dir(&e.curve)
        } else {
            end_dir_reversed(&e.curve)
        };
        let mut ang = d.y.atan2(d.x);
        if ang < 0.0 {
            ang += std::f64::consts::TAU;
        }
        if ang >= std::f64::consts::TAU - 1e-10 {
            ang = 0.0;
        }
        out_by_v[origin(h)].push((h, ang, k));
    }
    let mut pos = vec![0usize; nh];
    for list in &mut out_by_v {
        list.sort_by(|x, y| {
            if (x.1 - y.1).abs() <= 1e-9 {
                x.2.total_cmp(&y.2)
            } else {
                x.1.total_cmp(&y.1)
            }
        });
        for (i, (h, _, _)) in list.iter().enumerate() {
            pos[*h] = i;
        }
    }
    let next = |h: usize| -> usize {
        let twin = h ^ 1;
        let v = origin(twin);
        let list = &out_by_v[v];
        let i = pos[twin];
        list[(i + list.len() - 1) % list.len()].0
    };
    let mut visited = vec![false; nh];
    let mut faces = Vec::new();
    for start in 0..nh {
        if visited[start] {
            continue;
        }
        let mut hs = Vec::new();
        let mut h = start;
        let mut guard = 0;
        while !visited[h] && guard <= nh {
            visited[h] = true;
            hs.push(h);
            h = next(h);
            guard += 1;
        }
        if h != start {
            continue; // malformed walk (should not happen)
        }
        faces.push(Face {
            halfedges: hs,
            area: 0.0,
        });
    }
    faces
}

fn half_curve(edges: &[Edge], h: usize) -> Curve2 {
    let e = &edges[h / 2];
    if h.is_multiple_of(2) {
        e.curve.clone()
    } else {
        e.curve.reversed()
    }
}

/// All bounded faces of the arrangement formed by `curves` (split at intersections, dangling
/// pieces ignored), with holes nested by containment. `tol` is the merge distance for endpoints.
pub fn find_regions(curves: &[Curve2], tol: f64) -> Vec<Region> {
    let ext = curves
        .iter()
        .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()))
        .size()
        .length();
    if !ext.is_finite() || ext <= 0.0 {
        return Vec::new();
    }
    let tol = if tol > 0.0 && tol.is_finite() {
        tol
    } else {
        (ext * 1e-9).max(1e-12)
    };
    let (edges, verts) = build_edges(curves, tol);
    if edges.is_empty() {
        return Vec::new();
    }
    let mut faces = walk_faces(&edges, verts.len());
    let loops: Vec<Loop> = faces
        .iter_mut()
        .map(|f| {
            let curves: Vec<Curve2> = f.halfedges.iter().map(|&h| half_curve(&edges, h)).collect();
            let sources = f.halfedges.iter().map(|&h| edges[h / 2].src).collect();
            f.area = signed_area(&curves);
            Loop { curves, sources }
        })
        .collect();
    let area_eps = (ext * ext * 1e-14).max(1e-300);
    let pos: Vec<usize> = (0..faces.len())
        .filter(|&i| faces[i].area > area_eps)
        .collect();
    let neg: Vec<usize> = (0..faces.len())
        .filter(|&i| faces[i].area < -area_eps)
        .collect();
    let mut holes: Vec<Vec<usize>> = vec![Vec::new(); faces.len()];
    let poly_tol = (ext * 1e-6).max(1e-12);
    let polys: HashMap<usize, (Vec<DVec2>, BBox2)> = pos
        .iter()
        .map(|&i| {
            let p = loops[i].to_polygon(poly_tol);
            let b = BBox2::from_points(p.iter().copied());
            (i, (p, b))
        })
        .collect();
    let half_to_face: HashMap<usize, usize> = faces
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| f.halfedges.iter().map(move |&h| (h, fi)))
        .collect();
    for &ni in &neg {
        // a sample point on the component boundary
        let c0 = &loops[ni].curves[0];
        let sample = mid_point(c0);
        // faces bounded by this component's own edges cannot contain it
        let own: std::collections::HashSet<usize> = faces[ni]
            .halfedges
            .iter()
            .filter_map(|h| half_to_face.get(&(h ^ 1)).copied())
            .collect();
        let mut best: Option<(usize, f64)> = None;
        for &pi in &pos {
            if own.contains(&pi) {
                continue;
            }
            let (poly, bb) = &polys[&pi];
            if !bb.contains(sample) || !point_in_polygon(poly, sample) {
                continue;
            }
            let a = faces[pi].area;
            if best.is_none_or(|b| a < b.1) {
                best = Some((pi, a));
            }
        }
        if let Some((pi, _)) = best {
            holes[pi].push(ni);
        }
    }
    pos.iter()
        .map(|&pi| Region {
            outer: loops[pi].clone(),
            holes: holes[pi].iter().map(|&h| loops[h].clone()).collect(),
        })
        .collect()
}

/// The smallest region containing `p`, if any.
pub fn region_at(curves: &[Curve2], p: DVec2, tol: f64) -> Option<Region> {
    find_regions(curves, tol)
        .into_iter()
        .filter(|r| r.contains(p))
        .min_by(|a, b| a.area().total_cmp(&b.area()))
}

/// Even-odd point-in-polygon test (closing edge implied).
pub fn point_in_polygon(poly: &[DVec2], p: DVec2) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Convert an arc given in either direction into loop-friendly form (helper for callers that
/// build loops by hand).
pub fn directed_arc(a: Arc2, ccw: bool) -> Curve2 {
    if ccw { Curve2::Arc(a) } else { a.reversed() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::*;
    use std::f64::consts::PI;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Curve2> {
        let p = [
            DVec2::new(x0, y0),
            DVec2::new(x1, y0),
            DVec2::new(x1, y1),
            DVec2::new(x0, y1),
        ];
        (0..4)
            .map(|i| Curve2::Line(Line2::new(p[i], p[(i + 1) % 4])))
            .collect()
    }

    fn check_loop(l: &Loop) {
        for i in 0..l.curves.len() {
            let a = &l.curves[i];
            let b = &l.curves[(i + 1) % l.curves.len()];
            assert!(
                a.end().distance(b.start()) < 1e-7,
                "loop not head-to-tail: {:?} -> {:?}",
                a.end(),
                b.start()
            );
        }
        assert_eq!(l.curves.len(), l.sources.len());
    }

    #[test]
    fn rectangle() {
        let r = find_regions(&rect(0.0, 0.0, 4.0, 3.0), 1e-9);
        assert_eq!(r.len(), 1);
        assert!((r[0].area() - 12.0).abs() < 1e-12);
        assert!(r[0].outer.signed_area() > 0.0);
        check_loop(&r[0].outer);
        assert!((r[0].centroid() - DVec2::new(2.0, 1.5)).length() < 1e-9);
        // sources name the input lines
        let mut s = r[0].outer.sources.clone();
        s.sort();
        assert_eq!(s, vec![0, 1, 2, 3]);
    }

    #[test]
    fn circle_inside_rectangle() {
        let mut c = rect(0.0, 0.0, 10.0, 10.0);
        c.push(Curve2::Circle(Circle2::new(DVec2::new(5.0, 5.0), 2.0)));
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 2);
        let big = r
            .iter()
            .find(|x| x.holes.len() == 1)
            .expect("rect with hole");
        assert!((big.area() - (100.0 - 4.0 * PI)).abs() < 1e-9);
        assert!(big.holes[0].signed_area() < 0.0);
        check_loop(&big.holes[0]);
        assert!(big.holes[0].sources.iter().all(|s| *s == 4));
        let disk = r.iter().find(|x| x.holes.is_empty()).unwrap();
        assert!((disk.area() - 4.0 * PI).abs() < 1e-9);
        assert!((disk.centroid() - DVec2::new(5.0, 5.0)).length() < 1e-9);
        let at = region_at(&c, DVec2::new(1.0, 1.0), 1e-9).unwrap();
        assert_eq!(at.holes.len(), 1);
        let at = region_at(&c, DVec2::new(5.0, 5.5), 1e-9).unwrap();
        assert!(at.holes.is_empty());
        assert!(region_at(&c, DVec2::new(20.0, 5.0), 1e-9).is_none());
    }

    #[test]
    fn overlapping_circles() {
        let c = vec![
            Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0)),
            Curve2::Circle(Circle2::new(DVec2::new(6.0, 0.0), 5.0)),
        ];
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 3);
        let mut areas: Vec<f64> = r.iter().map(|x| x.area()).collect();
        areas.sort_by(|a, b| a.total_cmp(b));
        // lens area (research notes: AND = 22.3648, NOT = 56.1751)
        assert!((areas[0] - 22.364_8).abs() < 1e-3, "{areas:?}");
        assert!((areas[1] - 56.175_1).abs() < 1e-3);
        assert!((areas[2] - 56.175_1).abs() < 1e-3);
        for x in &r {
            check_loop(&x.outer);
        }
    }

    #[test]
    fn slot_with_dangling_lines() {
        // slot: two lines + two semicircles
        let mut c = vec![
            Curve2::Line(Line2::new(DVec2::new(0.0, 0.0), DVec2::new(10.0, 0.0))),
            Curve2::Arc(Arc2::new(DVec2::new(10.0, 2.0), 2.0, -PI / 2.0, PI / 2.0)),
            Curve2::Line(Line2::new(DVec2::new(10.0, 4.0), DVec2::new(0.0, 4.0))),
            Curve2::Arc(Arc2::new(DVec2::new(0.0, 2.0), 2.0, PI / 2.0, 1.5 * PI)),
        ];
        // dangling lines: one crossing into the slot, one free-floating
        c.push(Curve2::Line(Line2::new(
            DVec2::new(5.0, 2.0),
            DVec2::new(5.0, 8.0),
        )));
        c.push(Curve2::Line(Line2::new(
            DVec2::new(20.0, 20.0),
            DVec2::new(30.0, 25.0),
        )));
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(
            (r[0].area() - (40.0 + 4.0 * PI)).abs() < 1e-9,
            "{}",
            r[0].area()
        );
        check_loop(&r[0].outer);
        // a chord splits the slot into two regions
        c.push(Curve2::Line(Line2::new(
            DVec2::new(3.0, -1.0),
            DVec2::new(3.0, 5.0),
        )));
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 2);
        let total: f64 = r.iter().map(|x| x.area()).sum();
        assert!((total - (40.0 + 4.0 * PI)).abs() < 1e-9);
    }

    #[test]
    fn polyline_and_nested_components() {
        let outer = Curve2::Polyline(Polyline2::from_points(
            [
                DVec2::ZERO,
                DVec2::new(20.0, 0.0),
                DVec2::new(20.0, 20.0),
                DVec2::new(0.0, 20.0),
            ],
            true,
        ));
        let mut c = vec![outer];
        c.extend(rect(2.0, 2.0, 8.0, 8.0));
        c.push(Curve2::Circle(Circle2::new(DVec2::new(5.0, 5.0), 1.0)));
        c.extend(rect(12.0, 12.0, 18.0, 18.0));
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 4);
        let big = r
            .iter()
            .max_by(|a, b| a.outer.signed_area().total_cmp(&b.outer.signed_area()))
            .unwrap();
        assert_eq!(big.holes.len(), 2);
        assert!((big.area() - (400.0 - 72.0)).abs() < 1e-9);
        let mid = r
            .iter()
            .find(|x| (x.outer.signed_area() - 36.0).abs() < 1e-9)
            .unwrap();
        assert_eq!(mid.holes.len(), 1);
        // T-junction with a small gap merges within tol
        let mut g = rect(0.0, 0.0, 10.0, 10.0);
        g.push(Curve2::Line(Line2::new(
            DVec2::new(5.0, 1e-5),
            DVec2::new(5.0, 10.0 - 1e-5),
        )));
        assert_eq!(find_regions(&g, 1e-4).len(), 2);
        assert_eq!(find_regions(&g, 1e-7).len(), 1);
    }

    #[test]
    fn tangent_circles_and_ellipse() {
        // two circles touching internally at one point + an ellipse
        let c = vec![
            Curve2::Circle(Circle2::new(DVec2::ZERO, 4.0)),
            Curve2::Circle(Circle2::new(DVec2::new(2.0, 0.0), 2.0)),
            Curve2::Ellipse(EllipseArc2 {
                c: DVec2::new(20.0, 0.0),
                major: DVec2::new(3.0, 0.0),
                ratio: 0.5,
                start: 0.0,
                end: std::f64::consts::TAU,
            }),
        ];
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 3, "{r:?}");
        let mut areas: Vec<f64> = r.iter().map(|x| x.area()).collect();
        areas.sort_by(|a, b| a.total_cmp(b));
        assert!((areas[0] - 4.0 * PI).abs() < 1e-9);
        assert!((areas[1] - 4.5 * PI).abs() < 1e-9);
        assert!((areas[2] - 12.0 * PI).abs() < 1e-9);
    }

    #[test]
    fn shared_edges_and_crossing_circle() {
        let mut c = rect(0.0, 0.0, 4.0, 4.0);
        c.extend(rect(4.0, 0.0, 8.0, 4.0));
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|x| (x.area() - 16.0).abs() < 1e-9));
        let mut c = rect(0.0, 0.0, 4.0, 4.0);
        c.extend(rect(4.0, 1.0, 8.0, 3.0));
        let r = find_regions(&c, 1e-9);
        let mut a: Vec<f64> = r.iter().map(|x| x.area()).collect();
        a.sort_by(|x, y| x.total_cmp(y));
        assert_eq!(a.len(), 2);
        assert!((a[0] - 8.0).abs() < 1e-9 && (a[1] - 16.0).abs() < 1e-9);
        let mut c = rect(0.0, 0.0, 10.0, 10.0);
        c.push(Curve2::Circle(Circle2::new(DVec2::new(10.0, 5.0), 2.0)));
        let r = find_regions(&c, 1e-9);
        let mut a: Vec<f64> = r.iter().map(|x| x.area()).collect();
        a.sort_by(|x, y| x.total_cmp(y));
        assert_eq!(a.len(), 3);
        assert!((a[0] - 2.0 * PI).abs() < 1e-9 && (a[1] - 2.0 * PI).abs() < 1e-9);
        assert!((a[2] - (100.0 - 2.0 * PI)).abs() < 1e-9);
        for x in &r {
            check_loop(&x.outer);
            // outer loops are counter-clockwise
            assert!(x.outer.signed_area() > 0.0);
        }
    }

    #[test]
    fn bulge_polyline_and_spline_regions() {
        let p = Polyline2 {
            verts: vec![
                PolyVertex::new(DVec2::new(0.0, 0.0)),
                PolyVertex::with_bulge(DVec2::new(10.0, 0.0), 1.0),
                PolyVertex::new(DVec2::new(10.0, 10.0)),
                PolyVertex::with_bulge(DVec2::new(0.0, 10.0), -0.5),
            ],
            closed: true,
        };
        let r = find_regions(&[Curve2::Polyline(p.clone())], 1e-9);
        assert_eq!(r.len(), 1);
        assert!((r[0].area() - p.signed_area().abs()).abs() < 1e-9);
        check_loop(&r[0].outer);
        // closed spline (circle as rational NURBS) cut by a line → 2 half disks
        let s = Circle2::new(DVec2::ZERO, 3.0).to_nurbs();
        let c = vec![
            Curve2::Spline(s),
            Curve2::Line(Line2::new(DVec2::new(-5.0, 0.0), DVec2::new(5.0, 0.0))),
        ];
        let r = find_regions(&c, 1e-9);
        assert_eq!(r.len(), 2, "{r:?}");
        for x in &r {
            assert!((x.area() - 4.5 * PI).abs() < 1e-6, "{}", x.area());
            check_loop(&x.outer);
        }
        // centroid of the upper half disk: 4r/(3π)
        let up = r.iter().find(|x| x.centroid().y > 0.0).unwrap();
        assert!((up.centroid().y - 4.0 / PI).abs() < 1e-6);
    }

    #[test]
    fn grid_property() {
        // n x n grid of lines -> (n-1)^2 cells
        for n in 2..6 {
            let mut c = Vec::new();
            for i in 0..n {
                let v = i as f64;
                c.push(Curve2::Line(Line2::new(
                    DVec2::new(v, -0.5),
                    DVec2::new(v, n as f64 - 0.5),
                )));
                c.push(Curve2::Line(Line2::new(
                    DVec2::new(-0.5, v),
                    DVec2::new(n as f64 - 0.5, v),
                )));
            }
            let r = find_regions(&c, 1e-9);
            assert_eq!(r.len(), (n - 1) * (n - 1));
            for x in &r {
                assert!((x.area() - 1.0).abs() < 1e-12);
                check_loop(&x.outer);
            }
        }
    }
}
