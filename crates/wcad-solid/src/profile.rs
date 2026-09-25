//! Planar profiles: `wcad_geom2d::Region` on a `Plane` → monstertruck planar face.
//!
//! Lines, arcs and circles become exact edges (arcs are split into pieces of at most 180°),
//! polylines are expanded into line/bulge-arc segments, ellipses are exact rational arcs (an affine
//! image of a circle arc) and splines become B-spline/NURBS edges. Outer loops are made
//! counter-clockwise and holes clockwise in plane coordinates, so the face normal is the plane
//! normal; sweeps then orient the face along the sweep direction.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};

use monstertruck_modeling::{
    BsplineCurve, Curve, Edge, Face, Invertible, KnotVector, NurbsCurve, Point3, Surface,
    Transformed, Vector4, Vertex, Wire, builder,
};
use wcad_geom2d::{Curve2, Loop, Nurbs2, Region};
use wcad_math::{DAffine3, DMat3, DVec2, DVec3, Plane};
use wcad_sketch::SkEntityId;

use crate::conv::{mat4, p3};
use crate::{Error, Result};

/// Where a boundary edge of a profile face came from.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileEdgeTag {
    pub entity: SkEntityId,
    /// Piece counter per entity (split arcs and entities that appear in several loops).
    pub index: u32,
    /// Midpoint of the piece in plane coordinates.
    pub mid: DVec2,
}

/// A planar face ready for sweeping, with naming information.
#[derive(Clone, Debug)]
pub struct ProfileFace {
    pub(crate) face: Face,
    pub plane: Plane,
    pub tags: Vec<ProfileEdgeTag>,
    /// Flattened loops (outer first) in plane coordinates, for point-in-region tests.
    pub polygons: Vec<Vec<DVec2>>,
}

impl ProfileFace {
    /// `true` if `p` (plane coordinates) is inside the outer loop and outside every hole.
    pub fn contains(&self, p: DVec2) -> bool {
        let Some((outer, holes)) = self.polygons.split_first() else {
            return false;
        };
        point_in_polygon(outer, p) && !holes.iter().any(|h| point_in_polygon(h, p))
    }
}

/// A primitive 2D segment in traversal order.
#[derive(Clone, Debug)]
enum Seg {
    Line(DVec2, DVec2),
    /// Circular arc from angle `a0` sweeping `sweep` (signed; negative = clockwise).
    Arc {
        c: DVec2,
        r: f64,
        a0: f64,
        sweep: f64,
    },
    /// Elliptical arc: `c + major cos t + minor sin t`, from `t0` sweeping `dt` (signed).
    Ellipse {
        c: DVec2,
        major: DVec2,
        minor: DVec2,
        t0: f64,
        dt: f64,
    },
    Spline {
        spline: Nurbs2,
        reversed: bool,
    },
}

impl Seg {
    fn start(&self) -> DVec2 {
        self.at(0.0)
    }
    fn end(&self) -> DVec2 {
        self.at(1.0)
    }
    /// Point at normalized parameter `s` in `[0, 1]` along the traversal.
    fn at(&self, s: f64) -> DVec2 {
        match self {
            Seg::Line(a, b) => a.lerp(*b, s),
            Seg::Arc { c, r, a0, sweep } => {
                let a = a0 + sweep * s;
                *c + DVec2::new(a.cos(), a.sin()) * *r
            }
            Seg::Ellipse {
                c,
                major,
                minor,
                t0,
                dt,
            } => {
                let t = t0 + dt * s;
                *c + *major * t.cos() + *minor * t.sin()
            }
            Seg::Spline { spline, reversed } => {
                let s = if *reversed { 1.0 - s } else { s };
                eval_nurbs2(spline, s).unwrap_or(DVec2::ZERO)
            }
        }
    }
    fn samples(&self) -> usize {
        match self {
            Seg::Line(..) => 1,
            Seg::Arc { sweep, .. } => ((sweep.abs() / (PI / 18.0)).ceil() as usize).clamp(2, 72),
            Seg::Ellipse { dt, .. } => ((dt.abs() / (PI / 18.0)).ceil() as usize).clamp(2, 72),
            Seg::Spline { spline, .. } => (spline.ctrl.len() * 8).clamp(8, 256),
        }
    }
    fn reversed(&self) -> Seg {
        match self.clone() {
            Seg::Line(a, b) => Seg::Line(b, a),
            Seg::Arc { c, r, a0, sweep } => Seg::Arc {
                c,
                r,
                a0: a0 + sweep,
                sweep: -sweep,
            },
            Seg::Ellipse {
                c,
                major,
                minor,
                t0,
                dt,
            } => Seg::Ellipse {
                c,
                major,
                minor,
                t0: t0 + dt,
                dt: -dt,
            },
            Seg::Spline { spline, reversed } => Seg::Spline {
                spline,
                reversed: !reversed,
            },
        }
    }
}

/// Evaluate a 2D NURBS at normalized parameter `s` (de Boor on the valid knot span).
fn eval_nurbs2(n: &Nurbs2, s: f64) -> Option<DVec2> {
    let p = n.degree as usize;
    let k = &n.knots;
    let m = n.ctrl.len();
    if p == 0 || m <= p || k.len() != m + p + 1 {
        return None;
    }
    let (u0, u1) = (k[p], k[m]);
    let u = u0 + (u1 - u0) * s.clamp(0.0, 1.0);
    let mut span = p;
    while span + 1 < m && k[span + 1] <= u {
        span += 1;
    }
    let w = |i: usize| {
        if n.weights.len() == m {
            n.weights[i]
        } else {
            1.0
        }
    };
    let mut d: Vec<(DVec2, f64)> = (0..=p)
        .map(|j| (n.ctrl[span - p + j] * w(span - p + j), w(span - p + j)))
        .collect();
    for r in 1..=p {
        for j in (r..=p).rev() {
            let i = span - p + j;
            let den = k[i + p + 1 - r] - k[i];
            let a = if den.abs() < 1e-300 {
                0.0
            } else {
                (u - k[i]) / den
            };
            d[j] = (
                d[j - 1].0 * (1.0 - a) + d[j].0 * a,
                d[j - 1].1 * (1.0 - a) + d[j].1 * a,
            );
        }
    }
    let (pt, wt) = d[p];
    (wt.abs() > 1e-300).then(|| pt / wt)
}

fn arc_seg(c: DVec2, r: f64, start: f64, end: f64) -> Seg {
    Seg::Arc {
        c,
        r,
        a0: start,
        sweep: wcad_math::ccw_sweep(start, end),
    }
}

fn bulge_seg(a: DVec2, b: DVec2, bulge: f64) -> Seg {
    if bulge.abs() < 1e-12 {
        return Seg::Line(a, b);
    }
    let sweep = 4.0 * bulge.atan();
    let chord = b - a;
    let l = chord.length();
    let r = l / (2.0 * (sweep / 2.0).sin()).abs();
    // Center lies to the left of the chord for CCW (positive) arcs with |sweep| < π.
    let mid = (a + b) * 0.5;
    let h = r * (sweep / 2.0).cos(); // signed distance from chord midpoint to center (negative if |sweep| > π)
    let left = wcad_math::perp(chord / l);
    let c = mid + left * h * sweep.signum();
    let a0 = (a - c).to_angle();
    Seg::Arc { c, r, a0, sweep }
}

/// Expand a curve into segments in its natural direction. `closed` is true for closed curves.
fn curve_segs(c: &Curve2) -> (Vec<Seg>, bool) {
    match c {
        Curve2::Line(l) => (vec![Seg::Line(l.a, l.b)], false),
        Curve2::Arc(a) => (vec![arc_seg(a.c, a.r, a.start, a.end)], false),
        Curve2::Circle(ci) => (
            vec![Seg::Arc {
                c: ci.c,
                r: ci.r,
                a0: 0.0,
                sweep: TAU,
            }],
            true,
        ),
        Curve2::Ellipse(e) => {
            let full = e.is_full();
            let dt = if full {
                TAU
            } else {
                wcad_math::ccw_sweep(e.start, e.end)
            };
            (
                vec![Seg::Ellipse {
                    c: e.c,
                    major: e.major,
                    minor: e.minor(),
                    t0: e.start,
                    dt,
                }],
                full,
            )
        }
        Curve2::Polyline(p) => {
            let n = p.verts.len();
            let mut segs = Vec::new();
            for i in 0..p.segment_count() {
                let a = p.verts[i];
                let b = p.verts[(i + 1) % n];
                segs.push(bulge_seg(a.p, b.p, a.bulge));
            }
            (segs, p.closed)
        }
        Curve2::Spline(s) => (
            vec![Seg::Spline {
                spline: s.clone(),
                reversed: false,
            }],
            s.closed,
        ),
    }
}

/// Segments of a loop in traversal order, each with its source index.
fn loop_segs(lp: &Loop, tol: f64) -> Result<Vec<(Seg, usize)>> {
    let n = lp.curves.len();
    if n == 0 {
        return Err(Error::Invalid("empty profile loop".into()));
    }
    let src = |i: usize| lp.sources.get(i).copied().unwrap_or(i);
    let expanded: Vec<(Vec<Seg>, bool)> = lp.curves.iter().map(curve_segs).collect();
    if n == 1 {
        let (segs, closed) = &expanded[0];
        let first = segs
            .first()
            .ok_or_else(|| Error::Invalid("empty curve".into()))?;
        let last = segs.last().unwrap_or(first);
        if !*closed && first.start().distance(last.end()) > tol {
            return Err(Error::Invalid("profile loop is not closed".into()));
        }
        return Ok(segs.iter().map(|s| (s.clone(), src(0))).collect());
    }
    let ends: Vec<(DVec2, DVec2)> = expanded
        .iter()
        .map(|(s, _)| {
            (
                s.first().map_or(DVec2::ZERO, Seg::start),
                s.last().map_or(DVec2::ZERO, Seg::end),
            )
        })
        .collect();
    // Starting point of curve 0: its endpoint shared with the last curve.
    let (s0, e0) = ends[0];
    let (sl, el) = ends[n - 1];
    let d_start = s0.distance(sl).min(s0.distance(el));
    let d_end = e0.distance(sl).min(e0.distance(el));
    let mut p = if d_start <= d_end { s0 } else { e0 };
    let mut out = Vec::new();
    for (i, (segs, _)) in expanded.iter().enumerate() {
        let (s, e) = ends[i];
        let fwd = s.distance(p) <= e.distance(p);
        let gap = if fwd { s.distance(p) } else { e.distance(p) };
        if gap > tol {
            return Err(Error::Invalid(format!(
                "profile loop has a gap of {gap:.3e} at curve {i}"
            )));
        }
        if fwd {
            out.extend(segs.iter().map(|s| (s.clone(), src(i))));
            p = e;
        } else {
            out.extend(segs.iter().rev().map(|s| (s.reversed(), src(i))));
            p = s;
        }
    }
    Ok(out)
}

fn signed_area(poly: &[DVec2]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| wcad_math::cross2(poly[i], poly[(i + 1) % n]))
        .sum::<f64>()
        * 0.5
}

fn segs_polygon(segs: &[(Seg, usize)]) -> Vec<DVec2> {
    let mut poly = Vec::new();
    for (s, _) in segs {
        let k = s.samples();
        for j in 0..k {
            poly.push(s.at(j as f64 / k as f64));
        }
    }
    poly
}

pub(crate) fn point_in_polygon(poly: &[DVec2], p: DVec2) -> bool {
    let n = poly.len();
    let mut inside = false;
    let mut j = n.wrapping_sub(1);
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

fn validate_seg(s: &Seg, tol: f64) -> Result<()> {
    let ok = match s {
        Seg::Line(a, b) => a.is_finite() && b.is_finite(),
        Seg::Arc { c, r, a0, sweep } => {
            c.is_finite() && r.is_finite() && *r > tol && a0.is_finite() && sweep.is_finite()
        }
        Seg::Ellipse {
            c,
            major,
            minor,
            t0,
            dt,
        } => {
            c.is_finite()
                && major.length() > tol
                && minor.length() > tol
                && t0.is_finite()
                && dt.is_finite()
        }
        Seg::Spline { spline, .. } => {
            spline.ctrl.iter().all(|p| p.is_finite()) && spline.knots.iter().all(|k| k.is_finite())
        }
    };
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(
            "degenerate or non-finite profile curve".into(),
        ))
    }
}

/// Split long arcs so each piece sweeps at most 180°.
fn split_long(s: Seg) -> Vec<Seg> {
    match s {
        Seg::Arc { c, r, a0, sweep } if sweep.abs() > PI + 1e-9 => {
            let n = (sweep.abs() / PI).ceil() as usize;
            let d = sweep / n as f64;
            (0..n)
                .map(|i| Seg::Arc {
                    c,
                    r,
                    a0: a0 + d * i as f64,
                    sweep: d,
                })
                .collect()
        }
        Seg::Ellipse {
            c,
            major,
            minor,
            t0,
            dt,
        } if dt.abs() > PI + 1e-9 => {
            let n = (dt.abs() / PI).ceil() as usize;
            let d = dt / n as f64;
            (0..n)
                .map(|i| Seg::Ellipse {
                    c,
                    major,
                    minor,
                    t0: t0 + d * i as f64,
                    dt: d,
                })
                .collect()
        }
        other => vec![other],
    }
}

/// Build the 3D edge of a segment between shared vertices.
fn seg_edge(s: &Seg, plane: &Plane, v0: &Vertex, v1: &Vertex) -> Result<Edge> {
    let w = |p: DVec2| p3(plane.to_world(p));
    match s {
        Seg::Line(..) => Ok(builder::line(v0, v1)),
        Seg::Arc { .. } => {
            let mid = w(s.at(0.5));
            builder::try_circle_arc(v0, v1, mid).map_err(|e| Error::Kernel(format!("arc: {e}")))
        }
        Seg::Ellipse {
            c,
            major,
            minor,
            t0,
            dt,
        } => {
            // Unit-circle arc mapped by the affine ellipse frame (NURBS are affine invariant).
            let (a, b, m) = (t0, t0 + dt, t0 + dt * 0.5);
            let u = |t: f64| Point3::new(t.cos(), t.sin(), 0.0);
            let (u0, u1) = (builder::vertex(u(*a)), builder::vertex(u(b)));
            let arc: Edge = builder::try_circle_arc(&u0, &u1, u(m))
                .map_err(|e| Error::Kernel(format!("ellipse: {e}")))?;
            let frame = DAffine3::from_mat3_translation(
                DMat3::from_cols(major.extend(0.0), minor.extend(0.0), DVec3::Z),
                c.extend(0.0),
            );
            let to_world = plane.to_world_affine() * frame;
            let curve = arc.oriented_curve().transformed(mat4(&to_world));
            Ok(Edge::new(v0, v1, curve))
        }
        Seg::Spline { spline, reversed } => {
            let p = spline.degree as usize;
            let m = spline.ctrl.len();
            if p == 0 || m <= p || spline.knots.len() != m + p + 1 {
                return Err(Error::Invalid("malformed spline in profile".into()));
            }
            let knots = KnotVector::try_from(spline.knots.clone())
                .map_err(|e| Error::Invalid(format!("spline knots: {e}")))?;
            let mut curve: Curve = if spline.weights.len() == m {
                let ctrl: Vec<Vector4> = spline
                    .ctrl
                    .iter()
                    .zip(&spline.weights)
                    .map(|(c, wt)| {
                        let q = plane.to_world(*c);
                        Vector4::new(q.x * wt, q.y * wt, q.z * wt, *wt)
                    })
                    .collect();
                let bs = BsplineCurve::try_new(knots, ctrl)
                    .map_err(|e| Error::Invalid(format!("spline: {e}")))?;
                Curve::NurbsCurve(NurbsCurve::new(bs))
            } else {
                let ctrl: Vec<Point3> = spline.ctrl.iter().map(|c| w(*c)).collect();
                Curve::BsplineCurve(
                    BsplineCurve::try_new(knots, ctrl)
                        .map_err(|e| Error::Invalid(format!("spline: {e}")))?,
                )
            };
            if *reversed {
                curve.invert();
            }
            Ok(Edge::new(v0, v1, curve))
        }
    }
}

/// Wire for one loop, oriented CCW (`ccw = true`) or CW in plane coordinates.
fn loop_wire(
    lp: &Loop,
    plane: &Plane,
    ccw: bool,
    tol: f64,
    entity_of: &dyn Fn(usize) -> SkEntityId,
    counters: &mut HashMap<SkEntityId, u32>,
    tags: &mut Vec<ProfileEdgeTag>,
) -> Result<(Wire, Vec<DVec2>)> {
    let mut segs = loop_segs(lp, tol)?;
    for (s, _) in &segs {
        validate_seg(s, tol)?;
    }
    let poly = segs_polygon(&segs);
    let area = signed_area(&poly);
    if area.is_nan() || area.abs() <= tol * tol {
        return Err(Error::Invalid("profile loop encloses no area".into()));
    }
    if (area > 0.0) != ccw {
        segs = segs
            .into_iter()
            .rev()
            .map(|(s, i)| (s.reversed(), i))
            .collect();
    }
    let segs: Vec<(Seg, usize)> = segs
        .into_iter()
        .flat_map(|(s, i)| split_long(s).into_iter().map(move |p| (p, i)))
        .filter(|(s, _)| !matches!(s, Seg::Line(a, b) if a.distance(*b) <= tol))
        .collect();
    if segs.is_empty() || (segs.len() == 1 && matches!(segs[0].0, Seg::Line(..))) {
        return Err(Error::Invalid("degenerate profile loop".into()));
    }
    let verts: Vec<Vertex> = segs
        .iter()
        .map(|(s, _)| builder::vertex(p3(plane.to_world(s.start()))))
        .collect();
    let n = verts.len();
    let mut edges = Vec::with_capacity(n);
    for (k, (s, src)) in segs.iter().enumerate() {
        edges.push(seg_edge(s, plane, &verts[k], &verts[(k + 1) % n])?);
        let entity = entity_of(*src);
        let c = counters.entry(entity).or_insert(0);
        tags.push(ProfileEdgeTag {
            entity,
            index: *c,
            mid: s.at(0.5),
        });
        *c += 1;
    }
    let poly = segs_polygon(&segs);
    Ok((Wire::from(edges), poly))
}

/// Build a planar face for `region` on `plane`. `entity_of` maps loop source indices to sketch
/// entities (for face naming). The face normal equals `plane.normal()`.
pub fn build_profile_face(
    plane: &Plane,
    region: &Region,
    entity_of: &dyn Fn(usize) -> SkEntityId,
) -> Result<ProfileFace> {
    if !(plane.origin.is_finite() && plane.x_axis.is_finite() && plane.y_axis.is_finite()) {
        return Err(Error::Invalid("non-finite plane".into()));
    }
    let size = region_size(region);
    let tol = (size * 1e-7).max(1e-9);
    let mut counters = HashMap::new();
    let mut tags = Vec::new();
    let mut wires = Vec::new();
    let mut polygons = Vec::new();
    let (w, p) = loop_wire(
        &region.outer,
        plane,
        true,
        tol,
        entity_of,
        &mut counters,
        &mut tags,
    )?;
    wires.push(w);
    polygons.push(p);
    for h in &region.holes {
        let (w, p) = loop_wire(h, plane, false, tol, entity_of, &mut counters, &mut tags)?;
        wires.push(w);
        polygons.push(p);
    }
    let face: Face = crate::guard("attach plane", || {
        builder::try_attach_plane(wires).map_err(|e| Error::Kernel(format!("profile face: {e}")))
    })?;
    let face = orient_face(face, plane.normal());
    Ok(ProfileFace {
        face,
        plane: *plane,
        tags,
        polygons,
    })
}

/// Flip a planar face so its normal points along `dir` (sweeps are inside-out otherwise).
pub(crate) fn orient_face(mut face: Face, dir: DVec3) -> Face {
    if let Surface::Plane(p) = face.oriented_surface() {
        let n = p.normal();
        if n.x * dir.x + n.y * dir.y + n.z * dir.z < 0.0 {
            face.invert();
        }
    }
    face
}

fn region_size(r: &Region) -> f64 {
    let mut b = wcad_math::BBox2::EMPTY;
    for c in r.outer.curves.iter() {
        for (s, _) in curve_segs(c).0.iter().map(|s| (s, ())) {
            b.include(s.start());
            b.include(s.end());
            b.include(s.at(0.5));
        }
    }
    b.size().length().max(1e-9)
}

/// Flattened outer loop of a region (plane coordinates, CCW or as given).
pub(crate) fn loop_polygon(lp: &Loop) -> Vec<DVec2> {
    match loop_segs(lp, f64::INFINITY) {
        Ok(segs) => segs_polygon(&segs),
        Err(_) => Vec::new(),
    }
}

/// A point strictly inside the region (inside the outer loop, outside holes), if one is found.
pub(crate) fn interior_point(region: &Region) -> Option<DVec2> {
    let mut polys = vec![loop_polygon(&region.outer)];
    polys.extend(region.holes.iter().map(loop_polygon));
    interior_of_polygons(&polys)
}

/// A point strictly inside a profile face.
pub(crate) fn interior_point_of(face: &ProfileFace) -> Option<DVec2> {
    interior_of_polygons(&face.polygons)
}

fn interior_of_polygons(polys: &[Vec<DVec2>]) -> Option<DVec2> {
    let (outer, holes) = polys.split_first()?;
    let inside =
        |p: DVec2| point_in_polygon(outer, p) && !holes.iter().any(|h| point_in_polygon(h, p));
    let b = wcad_math::BBox2::from_points(outer.iter().copied());
    if b.is_empty() {
        return None;
    }
    // Scan lines: widest gap between consecutive crossings of a horizontal line.
    let mut best: Option<(f64, DVec2)> = None;
    for k in 0..16 {
        let y = b.min.y + (b.max.y - b.min.y) * ((k as f64 + 0.5) / 16.0 + 0.0013);
        let mut xs = Vec::new();
        for poly in polys {
            let n = poly.len();
            for i in 0..n {
                let (a, c) = (poly[i], poly[(i + 1) % n]);
                if (a.y > y) != (c.y > y) {
                    xs.push(a.x + (y - a.y) / (c.y - a.y) * (c.x - a.x));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for w in xs.windows(2) {
            let p = DVec2::new((w[0] + w[1]) * 0.5, y);
            let gap = w[1] - w[0];
            if gap > 1e-9 && inside(p) && best.is_none_or(|(g, _)| gap > g) {
                best = Some((gap, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// `true` if `p` lies in the region (inside outer, outside holes).
pub(crate) fn region_contains(region: &Region, p: DVec2) -> bool {
    point_in_polygon(&loop_polygon(&region.outer), p)
        && !region
            .holes
            .iter()
            .any(|h| point_in_polygon(&loop_polygon(h), p))
}

/// Regions that are not enclosed by a hole of another region ("every closed outer region").
pub(crate) fn outer_regions(regions: &[Region]) -> Vec<usize> {
    (0..regions.len())
        .filter(|&i| {
            let Some(p) = interior_point(&regions[i]) else {
                return false;
            };
            !regions.iter().enumerate().any(|(j, q)| {
                j != i
                    && q.holes
                        .iter()
                        .any(|h| point_in_polygon(&loop_polygon(h), p))
            })
        })
        .collect()
}

/// Fallback region finder for simple profiles: closed curves and head-to-tail chains that close
/// without intersections, nested by containment (even depth = outer, odd depth = hole).
pub fn simple_regions(curves: &[Curve2], tol: f64) -> Vec<Region> {
    let mut loops: Vec<Loop> = Vec::new();
    let mut open: Vec<(usize, DVec2, DVec2)> = Vec::new();
    for (i, c) in curves.iter().enumerate() {
        let (segs, closed) = curve_segs(c);
        let (Some(f), Some(l)) = (segs.first(), segs.last()) else {
            continue;
        };
        if closed || f.start().distance(l.end()) <= tol {
            loops.push(Loop {
                curves: vec![c.clone()],
                sources: vec![i],
            });
        } else {
            open.push((i, f.start(), l.end()));
        }
    }
    // Chain open curves greedily by shared endpoints.
    let mut used = vec![false; open.len()];
    for s in 0..open.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let start = open[s].1;
        let mut end = open[s].2;
        let mut chain = vec![open[s].0];
        let mut closed = false;
        loop {
            if chain.len() > 1 && end.distance(start) <= tol {
                closed = true;
                break;
            }
            let next = (0..open.len()).find(|&k| {
                !used[k] && (open[k].1.distance(end) <= tol || open[k].2.distance(end) <= tol)
            });
            let Some(k) = next else { break };
            used[k] = true;
            end = if open[k].1.distance(end) <= tol {
                open[k].2
            } else {
                open[k].1
            };
            chain.push(open[k].0);
        }
        if closed {
            loops.push(Loop {
                curves: chain.iter().map(|&i| curves[i].clone()).collect(),
                sources: chain,
            });
        }
    }
    // Nesting by containment of a loop point.
    let polys: Vec<Vec<DVec2>> = loops.iter().map(loop_polygon).collect();
    let areas: Vec<f64> = polys.iter().map(|p| signed_area(p).abs()).collect();
    let contains = |outer: usize, inner: usize| {
        outer != inner
            && areas[outer] > areas[inner]
            && polys[inner]
                .first()
                .is_some_and(|p| point_in_polygon(&polys[outer], *p))
    };
    let depth: Vec<usize> = (0..loops.len())
        .map(|i| (0..loops.len()).filter(|&j| contains(j, i)).count())
        .collect();
    let mut regions = Vec::new();
    for i in 0..loops.len() {
        if !depth[i].is_multiple_of(2) || polys[i].is_empty() {
            continue;
        }
        let holes = (0..loops.len())
            .filter(|&j| depth[j] == depth[i] + 1 && contains(i, j))
            .map(|j| loops[j].clone())
            .collect();
        regions.push(Region {
            outer: loops[i].clone(),
            holes,
        });
    }
    regions
}

fn seg_to_curve(s: &Seg) -> Curve2 {
    use wcad_geom2d::{Arc2, Circle2, EllipseArc2, Line2};
    match s {
        Seg::Line(a, b) => Curve2::Line(Line2::new(*a, *b)),
        Seg::Arc { c, r, a0, sweep } => {
            let (s0, s1) = if *sweep >= 0.0 {
                (*a0, a0 + sweep)
            } else {
                (a0 + sweep, *a0)
            };
            if sweep.abs() >= TAU - 1e-12 {
                Curve2::Circle(Circle2::new(*c, *r))
            } else {
                Curve2::Arc(Arc2::new(
                    *c,
                    *r,
                    wcad_math::normalize_0_2pi(s0),
                    wcad_math::normalize_0_2pi(s1),
                ))
            }
        }
        Seg::Ellipse {
            c,
            major,
            minor,
            t0,
            dt,
        } => {
            // A negative sweep is stored as the CCW arc between the same ends.
            let (s0, s1) = if *dt >= 0.0 {
                (*t0, t0 + dt)
            } else {
                (t0 + dt, *t0)
            };
            let ratio = minor.length() / major.length().max(1e-300);
            Curve2::Ellipse(EllipseArc2 {
                c: *c,
                major: *major,
                ratio,
                start: s0,
                end: s1,
            })
        }
        Seg::Spline { spline, reversed } => {
            let mut sp = spline.clone();
            if *reversed {
                // Reverse control points and mirror the knot vector.
                sp.ctrl.reverse();
                sp.weights.reverse();
                let (k0, k1) = (
                    sp.knots.first().copied().unwrap_or(0.0),
                    sp.knots.last().copied().unwrap_or(1.0),
                );
                sp.knots = sp.knots.iter().rev().map(|k| k0 + k1 - k).collect();
            }
            Curve2::Spline(sp)
        }
    }
}

/// Merge adjacent regions by cancelling shared boundary pieces (traversed in opposite directions
/// by the two neighbours) and re-chaining the rest. Avoids coplanar unions of separately swept
/// regions. Returns `None` if the boundary cannot be re-chained.
pub(crate) fn merge_regions(regions: &[&Region]) -> Option<Vec<Region>> {
    let size = regions.iter().map(|r| region_size(r)).fold(0.0, f64::max);
    let tol = (size * 1e-6).max(1e-9);
    let mut segs: Vec<(Seg, usize)> = Vec::new();
    for r in regions {
        for (lp, ccw) in std::iter::once((&r.outer, true)).chain(r.holes.iter().map(|h| (h, false)))
        {
            let mut s = loop_segs(lp, tol).ok()?;
            let area = signed_area(&segs_polygon(&s));
            if (area > 0.0) != ccw {
                s = s
                    .into_iter()
                    .rev()
                    .map(|(s, i)| (s.reversed(), i))
                    .collect();
            }
            segs.extend(s);
        }
    }
    let close = |a: DVec2, b: DVec2| a.distance(b) <= tol;
    let n = segs.len();
    let mut alive = vec![true; n];
    for i in 0..n {
        if !alive[i] {
            continue;
        }
        for j in i + 1..n {
            let (a, b) = (&segs[i].0, &segs[j].0);
            if alive[j]
                && close(a.start(), b.end())
                && close(a.end(), b.start())
                && close(a.at(0.5), b.at(0.5))
            {
                alive[i] = false;
                alive[j] = false;
                break;
            }
        }
    }
    let rest: Vec<(Seg, usize)> = segs
        .into_iter()
        .zip(alive)
        .filter(|(_, a)| *a)
        .map(|(s, _)| s)
        .collect();
    let mut used = vec![false; rest.len()];
    let mut loops: Vec<Vec<(Seg, usize)>> = Vec::new();
    for s in 0..rest.len() {
        if used[s] {
            continue;
        }
        used[s] = true;
        let start = rest[s].0.start();
        let mut chain = vec![rest[s].clone()];
        let mut end = rest[s].0.end();
        while !close(end, start) {
            let k = (0..rest.len()).find(|&k| !used[k] && close(rest[k].0.start(), end))?;
            used[k] = true;
            end = rest[k].0.end();
            chain.push(rest[k].clone());
        }
        loops.push(chain);
    }
    let polys: Vec<Vec<DVec2>> = loops.iter().map(|l| segs_polygon(l)).collect();
    let areas: Vec<f64> = polys.iter().map(|p| signed_area(p)).collect();
    let to_loop = |l: &[(Seg, usize)]| Loop {
        curves: l.iter().map(|(s, _)| seg_to_curve(s)).collect(),
        sources: l.iter().map(|(_, i)| *i).collect(),
    };
    let outers: Vec<usize> = (0..loops.len()).filter(|&i| areas[i] > 0.0).collect();
    let mut out: Vec<Region> = outers
        .iter()
        .map(|&i| Region {
            outer: to_loop(&loops[i]),
            holes: Vec::new(),
        })
        .collect();
    for h in (0..loops.len()).filter(|&i| areas[i] < 0.0) {
        let p = polys[h].first().copied()?;
        let owner = outers
            .iter()
            .enumerate()
            .filter(|(_, o)| point_in_polygon(&polys[**o], p))
            .min_by(|a, b| areas[*a.1].total_cmp(&areas[*b.1]))
            .map(|(k, _)| k)?;
        out[owner].holes.push(to_loop(&loops[h]));
    }
    (!out.is_empty()).then_some(out)
}
