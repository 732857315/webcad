//! Mesh-boolean fallback (boolmesh) on tessellations. Results are mesh-only bodies whose triangles
//! are grouped into faces named after the input faces they lie on.

use std::collections::HashMap;

use boolmesh::prelude::{Manifold, OpType, compute_boolean};
use monstertruck_meshing::prelude::*;
use wcad_doc::{BooleanKind, FeatureId, TopoName, TopoTag};
use wcad_math::DVec3;

use crate::analysis::{Analysis, NameSource, classify_triangles};
use crate::body::{Body, BodyRep, MeshSolid};
use crate::conv::dp;
use crate::kernel::finish_body;
use crate::{Error, Result, guard};

/// Welded triangle soup of a body for the mesh kernel.
pub(crate) fn body_triangles(body: &Body, tol: f64) -> Result<(Vec<DVec3>, Vec<[u32; 3]>)> {
    match &body.rep {
        BodyRep::Mesh(ms) => Ok((ms.positions.clone(), ms.triangles.clone())),
        BodyRep::Exact(s) => guard("tessellation", || {
            let meshed = s.solid.triangulation(tol);
            let mut poly = meshed.to_polygon();
            poly.put_together_same_attrs(monstertruck_modeling::TOLERANCE * 50.0)
                .remove_degenerate_faces()
                .remove_unused_attrs();
            let positions: Vec<DVec3> = poly.positions().iter().map(|p| dp(*p)).collect();
            let tris: Vec<[u32; 3]> = poly
                .faces()
                .triangle_iter()
                .map(|t| [t[0].pos as u32, t[1].pos as u32, t[2].pos as u32])
                .collect();
            Ok((positions, tris))
        }),
    }
}

fn to_manifold(pos: &[DVec3], tris: &[[u32; 3]]) -> Result<Manifold> {
    let p: Vec<f64> = pos.iter().flat_map(|v| [v.x, v.y, v.z]).collect();
    let idx: Vec<usize> = tris
        .iter()
        .flat_map(|t| [t[0] as usize, t[1] as usize, t[2] as usize])
        .collect();
    if idx.iter().any(|&i| i >= pos.len()) {
        return Err(Error::Mesh("triangle index out of range".into()));
    }
    guard("mesh import", || {
        Manifold::new(&p, &idx).map_err(Error::Mesh)
    })
}

/// Fine tessellation tolerance for mesh booleans.
pub(crate) fn mesh_tol(extent: f64) -> f64 {
    (extent * 5e-4).max(1e-6)
}

/// Mesh boolean of two bodies; the result keeps `a.id` and is mesh-only with `reason`.
pub(crate) fn mesh_boolean(
    a: &Body,
    b: &Body,
    kind: BooleanKind,
    feature: FeatureId,
    reason: String,
) -> Result<Body> {
    let tol = mesh_tol(a.extent().max(b.extent()));
    let (pa, ta) = body_triangles(a, tol)?;
    let (pb, tb) = body_triangles(b, tol)?;
    let ma = to_manifold(&pa, &ta)?;
    let mb = to_manifold(&pb, &tb)?;
    let op = match kind {
        BooleanKind::Union => OpType::Add,
        BooleanKind::Subtract => OpType::Subtract,
        BooleanKind::Intersect => OpType::Intersect,
    };
    let m = guard("mesh boolean", || {
        compute_boolean(&ma, &mb, op).map_err(Error::Mesh)
    })?;
    let positions: Vec<DVec3> = m.ps.iter().map(|p| DVec3::new(p.x, p.y, p.z)).collect();
    let triangles: Vec<[u32; 3]> =
        m.hs.chunks(3)
            .filter(|h| h.len() == 3)
            .map(|h| [h[0].tail as u32, h[1].tail as u32, h[2].tail as u32])
            .collect();
    if triangles.is_empty() {
        return Err(Error::Mesh("boolean result is empty".into()));
    }
    let sources = [
        NameSource {
            names: a.faces.iter().map(|f| f.name.clone()).collect(),
            analysis: &a.analysis,
            flip: false,
        },
        NameSource {
            names: b.faces.iter().map(|f| f.name.clone()).collect(),
            analysis: &b.analysis,
            flip: kind == BooleanKind::Subtract,
        },
    ];
    let cls = classify_triangles(&positions, &triangles, &sources, tol);
    let (tri_face, names) = group_faces(&triangles, &cls, &sources, feature);
    let ms = MeshSolid {
        positions,
        triangles,
        tri_face,
    };
    let an = Analysis::from_mesh(&ms, names.len(), tol);
    Ok(finish_body(
        a.id,
        feature,
        BodyRep::Mesh(ms),
        an,
        names,
        Some(reason),
    ))
}

/// Assign triangles to faces: one face per matched source face, one per connected component of
/// unmatched triangles.
fn group_faces(
    tris: &[[u32; 3]],
    cls: &[Option<(usize, usize)>],
    sources: &[NameSource<'_>],
    feature: FeatureId,
) -> (Vec<u32>, Vec<TopoName>) {
    let mut names: Vec<TopoName> = Vec::new();
    let mut key_to_face: HashMap<(usize, usize), u32> = HashMap::new();
    let mut tri_face = vec![u32::MAX; tris.len()];
    for (i, c) in cls.iter().enumerate() {
        if let Some(k) = c {
            let f = *key_to_face.entry(*k).or_insert_with(|| {
                let n = sources[k.0].names.get(k.1).cloned().unwrap_or(TopoName {
                    feature,
                    tag: TopoTag::Derived { index: u32::MAX },
                });
                names.push(n);
                (names.len() - 1) as u32
            });
            tri_face[i] = f;
        }
    }
    // Union-find over unmatched triangles sharing a vertex.
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    let mut by_vertex: HashMap<u32, usize> = HashMap::new();
    for (i, t) in tris.iter().enumerate() {
        if tri_face[i] != u32::MAX {
            continue;
        }
        for &v in t {
            if let Some(&j) = by_vertex.get(&v) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            } else {
                by_vertex.insert(v, i);
            }
        }
    }
    let mut comp_face: HashMap<usize, u32> = HashMap::new();
    let mut derived = 0u32;
    for (i, slot) in tri_face.iter_mut().enumerate() {
        if *slot != u32::MAX {
            continue;
        }
        let r = find(&mut parent, i);
        let f = *comp_face.entry(r).or_insert_with(|| {
            names.push(TopoName {
                feature,
                tag: TopoTag::Derived { index: derived },
            });
            derived += 1;
            (names.len() - 1) as u32
        });
        *slot = f;
    }
    (tri_face, names)
}
