//! Bodies: an exact or mesh-only solid plus named faces and edges.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wcad_doc::{BodyRef, FeatureId, TopoName};
use wcad_math::{BBox3, DVec3, Plane};

use crate::analysis::Analysis;

/// Exact B-rep solid (monstertruck). Cloning is shallow (topology is reference counted); operations
/// that mutate in place deep-copy first.
#[derive(Clone, Debug)]
pub struct ExactSolid {
    pub(crate) solid: monstertruck_modeling::Solid,
}

impl ExactSolid {
    pub(crate) fn new(solid: monstertruck_modeling::Solid) -> Self {
        Self { solid }
    }
    /// Number of B-rep faces.
    pub fn face_count(&self) -> usize {
        self.solid.face_iter().count()
    }
}

/// Indexed triangle mesh solid (result of a mesh-boolean fallback). `tri_face[i]` is the index of
/// the body face (`Body::faces`) triangle `i` belongs to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshSolid {
    pub positions: Vec<DVec3>,
    pub triangles: Vec<[u32; 3]>,
    pub tri_face: Vec<u32>,
}

#[derive(Clone, Debug)]
pub enum BodyRep {
    Exact(ExactSolid),
    Mesh(MeshSolid),
}

/// Geometric classification of a face (computed from the surface samples).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SurfaceKind {
    /// Planar face; `plane.normal()` is the outward face normal.
    Plane {
        plane: Plane,
    },
    /// Cylindrical face with axis through `origin` along unit `axis`.
    Cylinder {
        origin: DVec3,
        axis: DVec3,
        radius: f64,
    },
    Other,
}

/// A named face of a body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceInfo {
    pub name: TopoName,
    pub kind: SurfaceKind,
    /// Area-weighted centroid of the face (not necessarily on the face for curved faces).
    pub centroid: DVec3,
    /// Area-weighted mean outward normal (exact for planar faces).
    pub normal: DVec3,
    pub area: f64,
}

/// A named edge, identified by its two adjacent faces (seam edges repeat the same face).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeInfo {
    pub faces: [TopoName; 2],
    /// Indices into `Body::faces`.
    pub face_indices: [u32; 2],
    pub midpoint: DVec3,
    /// Unit tangent at the midpoint.
    pub direction: DVec3,
    pub length: f64,
}

/// A solid body produced by regeneration.
#[derive(Clone, Debug)]
pub struct Body {
    /// Identity: the feature that created the body (kept through Join/Cut/Fillet).
    pub id: BodyRef,
    pub rep: BodyRep,
    pub faces: Vec<FaceInfo>,
    pub edges: Vec<EdgeInfo>,
    /// The last feature that created or modified this body.
    pub source: FeatureId,
    /// Why the body degraded to a mesh (no further exact operations are possible).
    pub mesh_only_reason: Option<String>,
    pub(crate) analysis: Arc<Analysis>,
}

impl Body {
    pub fn is_exact(&self) -> bool {
        matches!(self.rep, BodyRep::Exact(_))
    }

    pub fn bbox(&self) -> BBox3 {
        self.analysis.bbox()
    }

    /// Size of the body (bbox diagonal).
    pub fn extent(&self) -> f64 {
        self.bbox().extent()
    }

    /// Index of the face with `name`; ties are broken by the distance of the centroid to `hint`.
    pub fn find_face(&self, name: &TopoName, hint: Option<DVec3>) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, f) in self.faces.iter().enumerate() {
            if &f.name != name {
                continue;
            }
            let d = hint.map_or(0.0, |h| self.analysis.distance_to_face(i, h));
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Face nearest to a point (by distance to the face surface samples).
    pub fn nearest_face(&self, p: DVec3) -> Option<usize> {
        (0..self.faces.len())
            .map(|i| (i, self.analysis.distance_to_face(i, p)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Index of the edge between faces `names` (order-independent); ties broken by `hint`.
    pub fn find_edge(&self, names: &[TopoName; 2], hint: Option<DVec3>) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        for (i, e) in self.edges.iter().enumerate() {
            let same = (e.faces[0] == names[0] && e.faces[1] == names[1])
                || (e.faces[0] == names[1] && e.faces[1] == names[0]);
            if !same {
                continue;
            }
            let d = hint.map_or(0.0, |h| self.analysis.distance_to_edge(i, h));
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    /// Edge nearest to a point.
    pub fn nearest_edge(&self, p: DVec3) -> Option<usize> {
        (0..self.edges.len())
            .map(|i| (i, self.analysis.distance_to_edge(i, p)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// Signed volume from the analysis tessellation (fast, approximate).
    pub fn approx_volume(&self) -> f64 {
        self.analysis.volume()
    }
}
