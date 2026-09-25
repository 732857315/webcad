//! Mesh and B-rep exports, mass properties.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use wcad_math::{BBox3, DVec3};

use crate::body::{Body, BodyRep};
use crate::tess::{TriMesh, tessellate};
use crate::{Error, Result};

fn meshes(bodies: &[Body]) -> Result<Vec<TriMesh>> {
    bodies.iter().map(|b| tessellate(b, 0.0)).collect()
}

fn dv(v: [f32; 3]) -> DVec3 {
    DVec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
}

/// Binary STL of all bodies (auto tolerance).
pub fn to_stl(bodies: &[Body]) -> Result<Vec<u8>> {
    let ms = meshes(bodies)?;
    let count: usize = ms.iter().map(TriMesh::triangle_count).sum();
    let count =
        u32::try_from(count).map_err(|_| Error::Invalid("too many triangles for STL".into()))?;
    let mut out = Vec::with_capacity(84 + count as usize * 50);
    let mut header = [0u8; 80];
    let tag = b"webcad2026 binary STL";
    header[..tag.len()].copy_from_slice(tag);
    out.extend_from_slice(&header);
    out.extend_from_slice(&count.to_le_bytes());
    for m in &ms {
        for t in m.indices.as_chunks::<3>().0.iter() {
            let p = [
                m.positions[t[0] as usize],
                m.positions[t[1] as usize],
                m.positions[t[2] as usize],
            ];
            let n = (dv(p[1]) - dv(p[0]))
                .cross(dv(p[2]) - dv(p[0]))
                .normalize_or_zero();
            for c in [n.x as f32, n.y as f32, n.z as f32] {
                out.extend_from_slice(&c.to_le_bytes());
            }
            for v in p {
                for c in v {
                    out.extend_from_slice(&c.to_le_bytes());
                }
            }
            out.extend_from_slice(&0u16.to_le_bytes());
        }
    }
    Ok(out)
}

/// Wavefront OBJ (one object per body, with normals).
pub fn to_obj(bodies: &[Body]) -> Result<String> {
    let ms = meshes(bodies)?;
    let mut s = String::from("# webcad2026 OBJ export\n");
    let mut base = 1usize;
    for (b, m) in bodies.iter().zip(&ms) {
        let _ = writeln!(s, "o body_{}", b.id.0.0);
        for p in &m.positions {
            let _ = writeln!(s, "v {} {} {}", p[0], p[1], p[2]);
        }
        for n in &m.normals {
            let _ = writeln!(s, "vn {} {} {}", n[0], n[1], n[2]);
        }
        for t in m.indices.as_chunks::<3>().0.iter() {
            let (a, b2, c) = (
                t[0] as usize + base,
                t[1] as usize + base,
                t[2] as usize + base,
            );
            let _ = writeln!(s, "f {a}//{a} {b2}//{b2} {c}//{c}");
        }
        base += m.positions.len();
    }
    Ok(s)
}

/// STEP AP214 text of the exact bodies. Mesh-only bodies cannot be exported and yield an error.
pub fn to_step(bodies: &[Body]) -> Result<String> {
    if let Some(b) = bodies.iter().find(|b| !b.is_exact()) {
        return Err(Error::Unsupported(format!(
            "STEP export of mesh-only body {} ({})",
            b.id.0.0,
            b.mesh_only_reason.as_deref().unwrap_or("mesh")
        )));
    }
    if bodies.is_empty() {
        return Err(Error::Invalid("nothing to export".into()));
    }
    step::write(bodies)
}

mod step {
    use monstertruck_io::step::save::{CompleteStepDisplay, StepHeaderDescriptor, StepModels};

    use super::*;

    /// AP214 via monstertruck-io (pure Rust, also builds for wasm32).
    pub(super) fn write(bodies: &[Body]) -> Result<String> {
        crate::guard("STEP export", || {
            let compressed: Vec<_> = bodies
                .iter()
                .filter_map(|b| match &b.rep {
                    BodyRep::Exact(s) => Some(s.solid.compress()),
                    BodyRep::Mesh(_) => None,
                })
                .collect();
            let mut models = StepModels::default();
            for c in &compressed {
                models.push_solid(c);
            }
            let header = StepHeaderDescriptor {
                organization_system: "webcad2026".to_owned(),
                ..Default::default()
            };
            Ok(CompleteStepDisplay::new(models, header).to_string())
        })
    }
}

/// Volume, surface area, bounding box and centroid of a body (from a fine tessellation).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MassProperties {
    pub volume: f64,
    pub area: f64,
    pub bbox: BBox3,
    pub centroid: DVec3,
}

pub fn mass_properties(body: &Body) -> Result<MassProperties> {
    let tol = (body.extent() * 2e-4).max(1e-7);
    let (pos, tris): (Vec<DVec3>, Vec<[u32; 3]>) = match &body.rep {
        BodyRep::Mesh(ms) => (ms.positions.clone(), ms.triangles.clone()),
        BodyRep::Exact(_) => {
            let m = tessellate(body, tol)?;
            let pos = m.positions.iter().map(|p| dv(*p)).collect();
            (pos, m.indices.as_chunks::<3>().0.to_vec())
        }
    };
    // Accumulate relative to a reference point for precision.
    let r = body.bbox().center();
    let r = if r.is_finite() { r } else { DVec3::ZERO };
    let mut volume = 0.0;
    let mut area = 0.0;
    let mut moment = DVec3::ZERO;
    let mut bbox = BBox3::EMPTY;
    for t in &tris {
        let get = |i: u32| pos.get(i as usize).copied().unwrap_or(r);
        let (a, b, c) = (get(t[0]) - r, get(t[1]) - r, get(t[2]) - r);
        let v = a.dot(b.cross(c)) / 6.0;
        volume += v;
        moment += (a + b + c) / 4.0 * v;
        area += (b - a).cross(c - a).length() * 0.5;
        for p in [a, b, c] {
            bbox.include(p + r);
        }
    }
    let centroid = if volume.abs() > 1e-300 {
        moment / volume + r
    } else {
        r
    };
    Ok(MassProperties {
        volume,
        area,
        bbox,
        centroid,
    })
}
