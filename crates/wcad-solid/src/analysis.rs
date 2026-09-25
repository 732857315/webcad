//! Per-face tessellation used for face classification, picking hints and name propagation.
//!
//! Every body keeps a coarse per-face triangle mesh ([`Analysis`]). It is used to
//! - classify faces (plane / cylinder / other) and compute centroids and normals,
//! - resolve geometric hints (nearest face / edge to a point),
//! - carry topological names through booleans and fillets: a face of an operation result inherits
//!   the name of the input face it lies on (sampled triangle centroids vs. input face meshes).

use std::collections::HashMap;

use monstertruck_meshing::prelude::{Invertible as _, MeshableShape};
use wcad_doc::{FeatureId, TopoName, TopoTag};
use wcad_math::{BBox3, DAffine3, DVec3, Plane};

use crate::body::{EdgeInfo, FaceInfo, MeshSolid, SurfaceKind};
use crate::conv::dp;

#[derive(Clone, Debug, Default)]
pub(crate) struct FaceMesh {
    pub positions: Vec<DVec3>,
    /// Per-vertex surface normals (may be zero when unknown, e.g. mesh-only bodies).
    pub normals: Vec<DVec3>,
    pub tris: Vec<[u32; 3]>,
    pub bbox: BBox3,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EdgePoly {
    pub points: Vec<DVec3>,
    pub faces: [u32; 2],
}

/// Coarse per-face tessellation of a body.
#[derive(Clone, Debug, Default)]
pub(crate) struct Analysis {
    pub faces: Vec<FaceMesh>,
    pub edges: Vec<EdgePoly>,
    /// Chord tolerance the faces were meshed with.
    pub tol: f64,
}

impl FaceMesh {
    fn finish(mut self) -> Self {
        self.bbox = BBox3::from_points(self.positions.iter().copied());
        self
    }

    fn tri(&self, t: &[u32; 3]) -> (DVec3, DVec3, DVec3) {
        (
            self.positions[t[0] as usize],
            self.positions[t[1] as usize],
            self.positions[t[2] as usize],
        )
    }

    /// Closest distance from `p` to the face mesh and the normal of the closest triangle.
    pub fn closest(&self, p: DVec3) -> Option<(f64, DVec3)> {
        let mut best: Option<(f64, DVec3)> = None;
        for t in &self.tris {
            let (a, b, c) = self.tri(t);
            let q = closest_point_triangle(p, a, b, c);
            let d = q.distance(p);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, (b - a).cross(c - a).normalize_or_zero()));
            }
        }
        best
    }

    /// Up to `n` sample points (triangle centroids spread over the face) with triangle normals.
    pub fn samples(&self, n: usize) -> Vec<(DVec3, DVec3)> {
        let valid: Vec<(DVec3, DVec3)> = self
            .tris
            .iter()
            .filter_map(|t| {
                let (a, b, c) = self.tri(t);
                let cr = (b - a).cross(c - a);
                let len = cr.length();
                (len > 1e-300 && len.is_finite()).then(|| ((a + b + c) / 3.0, cr / len))
            })
            .collect();
        if valid.len() <= n {
            return valid;
        }
        (0..n)
            .map(|k| valid[(k * valid.len() + valid.len() / (2 * n)) / n])
            .collect()
    }
}

impl Analysis {
    /// Tessellate an exact solid face by face.
    pub fn from_exact(solid: &monstertruck_modeling::Solid, tol: f64) -> Self {
        let meshed = solid.triangulation(tol);
        let mut faces = Vec::new();
        for face in meshed.face_iter() {
            let mut fm = FaceMesh::default();
            if let Some(mut poly) = face.surface() {
                if !face.orientation() {
                    poly.invert();
                }
                let pos = poly.positions();
                let nor = poly.normals();
                let mut map: HashMap<(usize, usize), u32> = HashMap::new();
                for tri in poly.faces().triangle_iter() {
                    let mut idx = [0u32; 3];
                    let mut ok = true;
                    for (k, v) in tri.iter().enumerate() {
                        let Some(p) = pos.get(v.pos) else {
                            ok = false;
                            break;
                        };
                        let key = (v.pos, v.nor.unwrap_or(usize::MAX));
                        let next = fm.positions.len() as u32;
                        let id = *map.entry(key).or_insert_with(|| {
                            fm.positions.push(dp(*p));
                            let n = v
                                .nor
                                .and_then(|n| nor.get(n))
                                .map_or(DVec3::ZERO, |n| DVec3::new(n.x, n.y, n.z));
                            fm.normals.push(n.normalize_or_zero());
                            next
                        });
                        idx[k] = id;
                    }
                    if ok && idx[0] != idx[1] && idx[1] != idx[2] && idx[0] != idx[2] {
                        fm.tris.push(idx);
                    }
                }
            }
            faces.push(fm.finish());
        }
        let mut edges: Vec<EdgePoly> = Vec::new();
        let mut map = HashMap::new();
        for (fi, face) in meshed.face_iter().enumerate() {
            for edge in face.edge_iter() {
                match map.entry(edge.id()) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        let i: usize = *e.get();
                        edges[i].faces[1] = fi as u32;
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(edges.len());
                        let curve = edge.oriented_curve();
                        let points = curve.0.iter().map(|p| dp(*p)).collect();
                        edges.push(EdgePoly {
                            points,
                            faces: [fi as u32, fi as u32],
                        });
                    }
                }
            }
        }
        Self { faces, edges, tol }
    }

    /// Build the analysis of a mesh-only solid with `nfaces` faces.
    pub fn from_mesh(ms: &MeshSolid, nfaces: usize, tol: f64) -> Self {
        let mut faces: Vec<FaceMesh> = vec![FaceMesh::default(); nfaces];
        let mut maps: Vec<HashMap<u32, u32>> = vec![HashMap::new(); nfaces];
        for (ti, t) in ms.triangles.iter().enumerate() {
            let f = ms.tri_face.get(ti).copied().unwrap_or(0) as usize;
            let Some(fm) = faces.get_mut(f) else { continue };
            let map = &mut maps[f];
            let mut idx = [0u32; 3];
            for k in 0..3 {
                let v = t[k];
                let next = fm.positions.len() as u32;
                idx[k] = *map.entry(v).or_insert_with(|| {
                    fm.positions
                        .push(ms.positions.get(v as usize).copied().unwrap_or(DVec3::ZERO));
                    fm.normals.push(DVec3::ZERO);
                    next
                });
            }
            fm.tris.push(idx);
        }
        // Smooth normals within a face (area weighted).
        for fm in &mut faces {
            let mut acc = vec![DVec3::ZERO; fm.positions.len()];
            for t in &fm.tris {
                let (a, b, c) = fm.tri(t);
                let n = (b - a).cross(c - a);
                for &v in t {
                    acc[v as usize] += n;
                }
            }
            fm.normals = acc.into_iter().map(|n| n.normalize_or_zero()).collect();
        }
        let faces = faces.into_iter().map(FaceMesh::finish).collect();
        Self {
            faces,
            edges: mesh_face_edges(ms),
            tol,
        }
    }

    pub fn bbox(&self) -> BBox3 {
        let mut b = BBox3::EMPTY;
        for f in &self.faces {
            if !f.bbox.is_empty() {
                b = b.union(&f.bbox);
            }
        }
        b
    }

    pub fn volume(&self) -> f64 {
        let mut v = 0.0;
        for f in &self.faces {
            for t in &f.tris {
                let (a, b, c) = f.tri(t);
                v += a.dot(b.cross(c)) / 6.0;
            }
        }
        v
    }

    pub fn distance_to_face(&self, i: usize, p: DVec3) -> f64 {
        self.faces
            .get(i)
            .and_then(|f| f.closest(p))
            .map_or(f64::INFINITY, |(d, _)| d)
    }

    pub fn distance_to_edge(&self, i: usize, p: DVec3) -> f64 {
        let Some(e) = self.edges.get(i) else {
            return f64::INFINITY;
        };
        match e.points.len() {
            0 => f64::INFINITY,
            1 => e.points[0].distance(p),
            _ => e
                .points
                .windows(2)
                .map(|w| closest_point_segment(p, w[0], w[1]).distance(p))
                .fold(f64::INFINITY, f64::min),
        }
    }

    /// Orientation flipped (for inverted solids).
    pub fn inverted(&self) -> Self {
        let mut a = self.clone();
        for f in &mut a.faces {
            for t in &mut f.tris {
                t.swap(1, 2);
            }
            for n in &mut f.normals {
                *n = -*n;
            }
        }
        a
    }

    /// Geometry mapped by an affine transform (mirrors flip the winding to stay outward).
    pub fn transformed(&self, m: &DAffine3) -> Self {
        let mirror = m.matrix3.determinant() < 0.0;
        let nm = m.matrix3.inverse().transpose();
        let mut a = self.clone();
        for f in &mut a.faces {
            for p in &mut f.positions {
                *p = m.transform_point3(*p);
            }
            for n in &mut f.normals {
                *n = (nm * *n).normalize_or_zero();
            }
            if mirror {
                for t in &mut f.tris {
                    t.swap(1, 2);
                }
            }
            f.bbox = BBox3::from_points(f.positions.iter().copied());
        }
        for e in &mut a.edges {
            for p in &mut e.points {
                *p = m.transform_point3(*p);
            }
        }
        a
    }

    /// Face infos (classification) for the given names (one per face).
    pub fn face_infos(&self, names: &[TopoName]) -> Vec<FaceInfo> {
        let scale = self.bbox().extent().max(1e-9);
        self.faces
            .iter()
            .zip(names)
            .map(|(f, name)| {
                let (kind, centroid, normal, area) = classify(f, scale);
                FaceInfo {
                    name: name.clone(),
                    kind,
                    centroid,
                    normal,
                    area,
                }
            })
            .collect()
    }

    pub fn edge_infos(&self, names: &[TopoName]) -> Vec<EdgeInfo> {
        let fallback = TopoName {
            feature: FeatureId(0),
            tag: TopoTag::Derived { index: u32::MAX },
        };
        self.edges
            .iter()
            .map(|e| {
                let n0 = names
                    .get(e.faces[0] as usize)
                    .cloned()
                    .unwrap_or_else(|| fallback.clone());
                let n1 = names
                    .get(e.faces[1] as usize)
                    .cloned()
                    .unwrap_or_else(|| fallback.clone());
                let (midpoint, direction, length) = polyline_mid(&e.points);
                EdgeInfo {
                    faces: [n0, n1],
                    face_indices: e.faces,
                    midpoint,
                    direction,
                    length,
                }
            })
            .collect()
    }
}

/// Midpoint (by arc length), unit tangent there, and total length of a polyline.
fn polyline_mid(pts: &[DVec3]) -> (DVec3, DVec3, f64) {
    if pts.is_empty() {
        return (DVec3::ZERO, DVec3::X, 0.0);
    }
    let total: f64 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    let half = total * 0.5;
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let l = w[0].distance(w[1]);
        if acc + l >= half && l > 0.0 {
            let t = (half - acc) / l;
            return (w[0].lerp(w[1], t), (w[1] - w[0]) / l, total);
        }
        acc += l;
    }
    (pts[0], DVec3::X, total)
}

/// Classify a face mesh: returns kind, centroid, mean normal, area.
fn classify(f: &FaceMesh, scale: f64) -> (SurfaceKind, DVec3, DVec3, f64) {
    let mut area = 0.0;
    let mut csum = DVec3::ZERO;
    let mut nsum = DVec3::ZERO;
    for t in &f.tris {
        let (a, b, c) = f.tri(t);
        let cr = (b - a).cross(c - a);
        let ar = cr.length() * 0.5;
        area += ar;
        csum += (a + b + c) / 3.0 * ar;
        nsum += cr * 0.5;
    }
    if area <= 0.0 || !area.is_finite() {
        let c = f.positions.first().copied().unwrap_or(DVec3::ZERO);
        return (SurfaceKind::Other, c, DVec3::Z, 0.0);
    }
    let centroid = csum / area;
    let normal = nsum.normalize_or(DVec3::Z);
    let lin = 1e-7 * scale;

    // Plane: all triangle normals parallel and all vertices on the plane.
    let planar = f.tris.iter().all(|t| {
        let (a, b, c) = f.tri(t);
        let cr = (b - a).cross(c - a);
        let l = cr.length();
        l < 1e-300 || cr.dot(normal) / l > 1.0 - 1e-9
    }) && f
        .positions
        .iter()
        .all(|p| (*p - centroid).dot(normal).abs() < lin);
    if planar {
        return (
            SurfaceKind::Plane {
                plane: Plane::from_normal(centroid, normal),
            },
            centroid,
            normal,
            area,
        );
    }

    // Cylinder: vertex normals perpendicular to a common axis, points at constant distance from it.
    if let Some(kind) = fit_cylinder(f, centroid, lin) {
        return (kind, centroid, normal, area);
    }
    (SurfaceKind::Other, centroid, normal, area)
}

fn fit_cylinder(f: &FaceMesh, centroid: DVec3, lin: f64) -> Option<SurfaceKind> {
    let ns: Vec<(DVec3, DVec3)> = f
        .positions
        .iter()
        .zip(&f.normals)
        .filter(|(_, n)| n.length_squared() > 0.5)
        .map(|(p, n)| (*p, *n))
        .collect();
    if ns.len() < 3 {
        return None;
    }
    let n0 = ns[0].1;
    let (_, nk) = ns
        .iter()
        .map(|(_, n)| (n0.cross(*n).length(), *n))
        .max_by(|a, b| a.0.total_cmp(&b.0))?;
    let axis = n0.cross(nk).try_normalize()?;
    if n0.cross(nk).length() < 1e-3 || ns.iter().any(|(_, n)| n.dot(axis).abs() > 1e-6) {
        return None;
    }
    // Least squares intersection of the normal lines, projected perpendicular to the axis.
    let mut a = wcad_math::DMat3::ZERO;
    let mut b = DVec3::ZERO;
    for (p, n) in &ns {
        let m = wcad_math::DMat3::IDENTITY - outer(*n, *n) - outer(axis, axis);
        a += m;
        b += m * *p;
    }
    a += outer(axis, axis);
    b += outer(axis, axis) * centroid;
    if a.determinant().abs() < 1e-12 {
        return None;
    }
    let c = a.inverse() * b;
    let radial = |p: DVec3| {
        let d = p - c;
        (d - axis * d.dot(axis)).length()
    };
    let r = ns.iter().map(|(p, _)| radial(*p)).sum::<f64>() / ns.len() as f64;
    if r.is_nan()
        || r <= lin
        || ns
            .iter()
            .any(|(p, _)| (radial(*p) - r).abs() > lin.max(r * 1e-6))
    {
        return None;
    }
    Some(SurfaceKind::Cylinder {
        origin: c,
        axis,
        radius: r,
    })
}

fn outer(a: DVec3, b: DVec3) -> wcad_math::DMat3 {
    wcad_math::DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}

/// Faces of a mesh solid are groups of triangles; edges are the chained boundaries between groups.
fn mesh_face_edges(ms: &MeshSolid) -> Vec<EdgePoly> {
    let mut half: HashMap<(u32, u32), u32> = HashMap::new();
    for (ti, t) in ms.triangles.iter().enumerate() {
        let f = ms.tri_face.get(ti).copied().unwrap_or(0);
        for k in 0..3 {
            half.insert((t[k], t[(k + 1) % 3]), f);
        }
    }
    // Segments between different faces, grouped by face pair.
    let mut groups: HashMap<(u32, u32), Vec<(u32, u32)>> = HashMap::new();
    for (&(a, b), &fa) in &half {
        if a >= b {
            continue;
        }
        let fb = half.get(&(b, a)).copied().unwrap_or(fa);
        if fa != fb {
            groups
                .entry((fa.min(fb), fa.max(fb)))
                .or_default()
                .push((a, b));
        }
    }
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort_unstable();
    let mut out = Vec::new();
    for key in keys {
        let segs = &groups[&key];
        let mut adj: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, &(a, b)) in segs.iter().enumerate() {
            adj.entry(a).or_default().push(i);
            adj.entry(b).or_default().push(i);
        }
        let mut used = vec![false; segs.len()];
        // Start chains at vertices of odd degree first so open chains are not split.
        let mut order: Vec<usize> = (0..segs.len()).collect();
        order.sort_by_key(|&i| {
            let (a, b) = segs[i];
            let odd = adj[&a].len() % 2 == 1 || adj[&b].len() % 2 == 1;
            (!odd, i)
        });
        for &s in &order {
            if used[s] {
                continue;
            }
            used[s] = true;
            let (a, b) = segs[s];
            let start = if adj[&a].len() % 2 == 1 {
                a
            } else if adj[&b].len() % 2 == 1 {
                b
            } else {
                a
            };
            let mut chain = vec![start, if start == a { b } else { a }];
            while let Some(&cur) = chain.last() {
                let next = adj[&cur].iter().copied().find(|&i| !used[i]);
                let Some(i) = next else { break };
                used[i] = true;
                let (p, q) = segs[i];
                chain.push(if p == cur { q } else { p });
            }
            let points = chain
                .iter()
                .map(|&v| ms.positions.get(v as usize).copied().unwrap_or(DVec3::ZERO))
                .collect();
            out.push(EdgePoly {
                points,
                faces: [key.0, key.1],
            });
        }
    }
    out
}

pub(crate) fn closest_point_segment(p: DVec3, a: DVec3, b: DVec3) -> DVec3 {
    let ab = b - a;
    let l2 = ab.length_squared();
    if l2 <= 0.0 {
        return a;
    }
    a + ab * ((p - a).dot(ab) / l2).clamp(0.0, 1.0)
}

/// Closest point on triangle `abc` to `p` (Ericson, Real-Time Collision Detection 5.1.5).
pub(crate) fn closest_point_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return a + ab * v;
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return a + ac * w;
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        return b + (c - b) * w;
    }
    let denom = va + vb + vc;
    if denom.abs() < 1e-300 {
        return a;
    }
    let v = vb / denom;
    let w = vc / denom;
    a + ab * v + ac * w
}

// ------------------------------------------------------------------------------------------------
// Name propagation

/// A named input of an operation. `flip` is set for operands whose faces appear inverted in the
/// result (the tool of a subtraction).
pub(crate) struct NameSource<'a> {
    pub names: Vec<TopoName>,
    pub analysis: &'a Analysis,
    pub flip: bool,
}

/// Best source face for a set of sample points: `(source index, face index)`.
fn best_source(
    samples: &[(DVec3, DVec3)],
    sources: &[NameSource<'_>],
    thr: f64,
) -> Option<(usize, usize)> {
    if samples.is_empty() {
        return None;
    }
    let sb = BBox3::from_points(samples.iter().map(|s| s.0));
    let mut best: Option<(usize, f64, usize, usize)> = None; // hits, dist, src, face
    for (si, src) in sources.iter().enumerate() {
        let sign = if src.flip { -1.0 } else { 1.0 };
        for (fi, fm) in src.analysis.faces.iter().enumerate() {
            if fm.bbox.is_empty() || !boxes_touch(&fm.bbox, &sb, thr) {
                continue;
            }
            let mut hits = 0usize;
            let mut dsum = 0.0;
            for (p, n) in samples {
                if let Some((d, tn)) = fm.closest(*p)
                    && d <= thr
                    && tn.dot(*n) * sign > 0.3
                {
                    hits += 1;
                    dsum += d;
                }
            }
            if hits == 0 {
                continue;
            }
            let better = match best {
                None => true,
                Some((bh, bd, _, _)) => hits > bh || (hits == bh && dsum < bd - 1e-12),
            };
            if better {
                best = Some((hits, dsum, si, fi));
            }
        }
    }
    let (hits, _, si, fi) = best?;
    (hits * 2 >= samples.len()).then_some((si, fi))
}

fn boxes_touch(a: &BBox3, b: &BBox3, d: f64) -> bool {
    !(a.min.x > b.max.x + d
        || a.max.x < b.min.x - d
        || a.min.y > b.max.y + d
        || a.max.y < b.min.y - d
        || a.min.z > b.max.z + d
        || a.max.z < b.min.z - d)
}

/// Match threshold for two analyses meshed at the given tolerances.
pub(crate) fn match_threshold(tol_a: f64, tol_b: f64, scale: f64) -> f64 {
    2.5 * tol_a.max(tol_b) + 1e-7 * scale
}

/// Names for the faces of `target` inherited from `sources`; `None` where no source face matches.
pub(crate) fn inherit_names(
    target: &Analysis,
    sources: &[NameSource<'_>],
) -> Vec<Option<TopoName>> {
    let scale = target.bbox().extent().max(1e-9);
    let src_tol = sources.iter().map(|s| s.analysis.tol).fold(0.0, f64::max);
    let thr = match_threshold(target.tol, src_tol, scale);
    target
        .faces
        .iter()
        .map(|f| {
            let samples = f.samples(7);
            best_source(&samples, sources, thr).map(|(si, fi)| {
                sources[si].names.get(fi).cloned().unwrap_or(TopoName {
                    feature: FeatureId(0),
                    tag: TopoTag::Derived { index: fi as u32 },
                })
            })
        })
        .collect()
}

/// For each triangle of a mesh, the `(source, face)` it lies on.
pub(crate) fn classify_triangles(
    positions: &[DVec3],
    tris: &[[u32; 3]],
    sources: &[NameSource<'_>],
    tol: f64,
) -> Vec<Option<(usize, usize)>> {
    let scale = BBox3::from_points(positions.iter().copied())
        .extent()
        .max(1e-9);
    let src_tol = sources.iter().map(|s| s.analysis.tol).fold(0.0, f64::max);
    let thr = match_threshold(tol, src_tol, scale);
    let mut last: Option<(usize, usize)> = None;
    tris.iter()
        .map(|t| {
            let get = |i: u32| positions.get(i as usize).copied().unwrap_or(DVec3::ZERO);
            let (a, b, c) = (get(t[0]), get(t[1]), get(t[2]));
            let cr = (b - a).cross(c - a);
            let n = cr.normalize_or_zero();
            let p = (a + b + c) / 3.0;
            // Coherence: most neighbours lie on the same face as the previous triangle.
            if let Some((si, fi)) = last {
                let src = &sources[si];
                let sign = if src.flip { -1.0 } else { 1.0 };
                if let Some((d, tn)) = src.analysis.faces[fi].closest(p)
                    && d <= thr
                    && tn.dot(n) * sign > 0.3
                {
                    return last;
                }
            }
            let r = best_source(&[(p, n)], sources, thr);
            if r.is_some() {
                last = r;
            }
            r
        })
        .collect()
}
