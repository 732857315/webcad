//! Render/pick tessellation of bodies.

use std::ops::Range;

use serde::{Deserialize, Serialize};
use wcad_doc::TopoName;
use wcad_math::DVec3;

use crate::analysis::Analysis;
use crate::body::{Body, BodyRep};
use crate::{Result, guard};

/// Triangle mesh of a body with face and edge maps for picking.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TriMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Per body face: range into `indices` (multiples of 3). Faces are contiguous.
    pub face_ranges: Vec<Range<u32>>,
    pub face_names: Vec<TopoName>,
    /// Edge polylines (one per body edge).
    pub edges: Vec<Vec<[f32; 3]>>,
    pub edge_names: Vec<[TopoName; 2]>,
}

impl TriMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// Body face index of triangle `tri`.
    pub fn face_of_triangle(&self, tri: usize) -> Option<usize> {
        let i = (tri * 3) as u32;
        self.face_ranges.iter().position(|r| r.contains(&i))
    }

    /// Signed volume enclosed by the mesh.
    pub fn volume(&self) -> f64 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let p = |i: u32| {
                    let v = self.positions.get(i as usize).copied().unwrap_or_default();
                    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                };
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }
}

/// Chord tolerance adapted to the body size (0.1% of the bbox diagonal).
pub fn auto_tolerance(body: &Body) -> f64 {
    (body.extent() * 1e-3).max(1e-6)
}

fn f3(v: DVec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

/// Tessellate a body face by face. `tol <= 0` (or non-finite) selects [`auto_tolerance`].
pub fn tessellate(body: &Body, tol: f64) -> Result<TriMesh> {
    let tol = if tol.is_finite() && tol > 0.0 {
        tol
    } else {
        auto_tolerance(body)
    };
    let an_owned;
    let an: &Analysis = match &body.rep {
        // Reuse the analysis mesh when it is fine enough.
        BodyRep::Exact(_) if body.analysis.tol <= tol * 1.0001 => &body.analysis,
        BodyRep::Exact(s) => {
            an_owned = guard("tessellation", || Ok(Analysis::from_exact(&s.solid, tol)))?;
            &an_owned
        }
        BodyRep::Mesh(_) => &body.analysis,
    };
    let mut m = TriMesh::default();
    for (fi, f) in an.faces.iter().enumerate() {
        let base = m.positions.len() as u32;
        let start = m.indices.len() as u32;
        let flat = f.normals.iter().all(|n| n.length_squared() < 0.5);
        for (i, p) in f.positions.iter().enumerate() {
            m.positions.push(f3(*p));
            let n = if flat { DVec3::ZERO } else { f.normals[i] };
            m.normals.push(f3(n));
        }
        for t in &f.tris {
            m.indices.extend([base + t[0], base + t[1], base + t[2]]);
        }
        if flat {
            // No surface normals: use area-weighted vertex normals within the face.
            for t in &f.tris {
                let (a, b, c) = (
                    f.positions[t[0] as usize],
                    f.positions[t[1] as usize],
                    f.positions[t[2] as usize],
                );
                let n = (b - a).cross(c - a);
                for &v in t {
                    let slot = &mut m.normals[(base + v) as usize];
                    slot[0] += n.x as f32;
                    slot[1] += n.y as f32;
                    slot[2] += n.z as f32;
                }
            }
            for slot in &mut m.normals[base as usize..] {
                let l = (slot[0] * slot[0] + slot[1] * slot[1] + slot[2] * slot[2]).sqrt();
                if l > 0.0 {
                    slot.iter_mut().for_each(|c| *c /= l);
                }
            }
        }
        m.face_ranges.push(start..m.indices.len() as u32);
        let name = body
            .faces
            .get(fi)
            .map(|fi| fi.name.clone())
            .unwrap_or_else(|| TopoName {
                feature: body.source,
                tag: wcad_doc::TopoTag::Derived { index: fi as u32 },
            });
        m.face_names.push(name);
    }
    for (ei, e) in an.edges.iter().enumerate() {
        m.edges.push(e.points.iter().map(|p| f3(*p)).collect());
        let names = body
            .edges
            .get(ei)
            .map(|e| e.faces.clone())
            .unwrap_or_else(|| {
                let fallback = TopoName {
                    feature: body.source,
                    tag: wcad_doc::TopoTag::Derived { index: u32::MAX },
                };
                let n = |i: u32| {
                    m.face_names
                        .get(i as usize)
                        .cloned()
                        .unwrap_or_else(|| fallback.clone())
                };
                [n(e.faces[0]), n(e.faces[1])]
            });
        m.edge_names.push(names);
    }
    Ok(m)
}
