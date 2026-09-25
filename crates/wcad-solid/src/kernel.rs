//! Exact kernel facade (monstertruck 0.4.1) and body-level operations with naming and fallback.

use std::collections::HashMap;
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use monstertruck_modeling::topology::ShellCondition;
use monstertruck_modeling::{
    Edge, Face, FilletOptions, FilletProfile, Point3, Rad, Shell, Solid, Vector3, Wire, builder,
    fillet_edges,
};
use wcad_doc::{BodyRef, BooleanKind, FeatureId, Primitive, TopoName, TopoTag};
use wcad_math::{BBox3, DAffine3, DQuat, DVec3};

use crate::analysis::{Analysis, NameSource, inherit_names, match_threshold};
use crate::body::{Body, BodyRep, ExactSolid, FaceInfo};
use crate::conv::{affine_ok, dp, finite3, mat4, p3, v3};
use crate::profile::{ProfileFace, orient_face};
use crate::{Error, Result, fallback, guard};

/// Tolerance ladder of the exact boolean (absolute model units, clamped by model size).
const BOOL_TOLS: [f64; 2] = [0.01, 0.05];

/// Operations of the exact kernel on bare solids (no naming).
pub trait Kernel {
    /// Sweep a profile face along unit `dir`, from `-back` to `fwd`.
    fn extrude(&self, face: &ProfileFace, dir: DVec3, back: f64, fwd: f64) -> Result<ExactSolid>;
    /// Revolve a profile face about the axis through `origin` along `axis` by `angle` (radians,
    /// sign = right-hand rule, |angle| ≥ 2π is a full revolution).
    fn revolve(
        &self,
        face: &ProfileFace,
        origin: DVec3,
        axis: DVec3,
        angle: f64,
    ) -> Result<ExactSolid>;
    /// Primitive in local coordinates mapped by `placement`: box spans `[0, size]` from the local
    /// origin; cylinder and cone stand on the local XY plane centred on +Z (cone: `radius1` at
    /// z = 0, `radius2` at z = height); sphere and torus are centred at the origin (torus axis Z).
    fn primitive(&self, shape: &Primitive, placement: &DAffine3) -> Result<ExactSolid>;
    fn boolean(
        &self,
        a: &ExactSolid,
        b: &ExactSolid,
        kind: BooleanKind,
        tol: f64,
    ) -> Result<ExactSolid>;
    /// Fillet (or chamfer) edges given by index in [`Body::edges`] order.
    fn fillet(
        &self,
        s: &ExactSolid,
        edges: &[usize],
        radius: f64,
        chamfer: bool,
    ) -> Result<ExactSolid>;
    fn transform(&self, s: &ExactSolid, m: &DAffine3) -> Result<ExactSolid>;
}

/// The monstertruck implementation of [`Kernel`].
#[derive(Clone, Copy, Debug, Default)]
pub struct MtKernel;

fn not(s: &Solid) -> Solid {
    let mut s = s.clone();
    s.not();
    s
}

/// Edges in the canonical order (faces in order, boundary edges in order, deduplicated by id).
/// Matches the order of [`Analysis::from_exact`] edges.
pub(crate) fn unique_edges(solid: &Solid) -> Vec<Edge> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for face in solid.face_iter() {
        for e in face.edge_iter() {
            if seen.insert(e.id()) {
                out.push(e);
            }
        }
    }
    out
}

fn circle_wire(center: Point3, start: Point3, axis: Vector3) -> Wire {
    builder::revolve(
        &builder::vertex(start),
        center,
        axis,
        builder::SweepAngle::Closed,
        4,
    )
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

impl Kernel for MtKernel {
    fn extrude(&self, face: &ProfileFace, dir: DVec3, back: f64, fwd: f64) -> Result<ExactSolid> {
        let len = back + fwd;
        let dir = dir
            .try_normalize()
            .ok_or_else(|| Error::Invalid("zero extrude direction".into()))?;
        if !(back.is_finite() && fwd.is_finite()) || len.is_nan() || len <= 1e-9 {
            return Err(Error::Invalid(format!(
                "extrude length must be positive (got {len})"
            )));
        }
        guard("extrude", || {
            let mut f: Face = orient_face(face.face.clone(), dir);
            if back != 0.0 {
                f = builder::translated(&f, v3(-dir * back));
            }
            let s: Solid = builder::extrude(&f, v3(dir * len));
            Ok(ExactSolid::new(s))
        })
    }

    fn revolve(
        &self,
        face: &ProfileFace,
        origin: DVec3,
        axis: DVec3,
        angle: f64,
    ) -> Result<ExactSolid> {
        let axis = axis
            .try_normalize()
            .ok_or_else(|| Error::Invalid("zero revolve axis".into()))?;
        if !finite3(origin) || !angle.is_finite() || angle.abs() < 1e-9 {
            return Err(Error::Invalid("revolve angle must be non-zero".into()));
        }
        let (axis, angle) = if angle < 0.0 {
            (-axis, -angle)
        } else {
            (axis, angle)
        };
        // The axis must lie in (or parallel to) the profile plane, and the profile must stay on
        // one side of it.
        let n = face.plane.normal();
        if axis.dot(n).abs() > 1e-6 {
            return Err(Error::Invalid(
                "the revolve axis must lie in the profile plane".into(),
            ));
        }
        let mut sides = (false, false);
        let mut interior = None;
        for poly in &face.polygons {
            for p in poly {
                let w = face.plane.to_world(*p);
                let s = (w - origin).cross(axis).dot(n);
                let scale = (w - origin).length().max(1.0);
                if s > 1e-9 * scale {
                    sides.0 = true;
                } else if s < -1e-9 * scale {
                    sides.1 = true;
                }
            }
        }
        if sides.0 && sides.1 {
            return Err(Error::Invalid(
                "the profile crosses the revolve axis".into(),
            ));
        }
        if let Some(p) = crate::profile::interior_point_of(face) {
            interior = Some(face.plane.to_world(p));
        }
        let p = interior.ok_or_else(|| Error::Invalid("profile has no interior".into()))?;
        let dir = axis.cross(p - origin);
        if dir.length() < 1e-12 {
            return Err(Error::Invalid("profile lies on the revolve axis".into()));
        }
        guard("revolve", || {
            let f = orient_face(face.face.clone(), dir);
            let s: Solid = if angle >= TAU - 1e-9 {
                builder::revolve(&f, p3(origin), v3(axis), builder::SweepAngle::Closed, 4)
            } else {
                let div = ((angle / (PI / 2.0)).ceil() as usize).max(1);
                builder::revolve(
                    &f,
                    p3(origin),
                    v3(axis),
                    builder::SweepAngle::Partial(Rad(angle)),
                    div,
                )
            };
            Ok(ExactSolid::new(s))
        })
    }

    fn primitive(&self, shape: &Primitive, placement: &DAffine3) -> Result<ExactSolid> {
        if !affine_ok(placement) {
            return Err(Error::Invalid(
                "primitive placement is singular or non-finite".into(),
            ));
        }
        let o = Point3::new(0.0, 0.0, 0.0);
        let z = Vector3::new(0.0, 0.0, 1.0);
        let local: Solid = match *shape {
            Primitive::Box { size } => {
                if !(positive(size.x) && positive(size.y) && positive(size.z)) {
                    return Err(Error::Invalid("box size must be positive".into()));
                }
                guard("box", || {
                    let v = builder::vertices([
                        Point3::new(0.0, 0.0, 0.0),
                        Point3::new(size.x, 0.0, 0.0),
                        Point3::new(size.x, size.y, 0.0),
                        Point3::new(0.0, size.y, 0.0),
                    ]);
                    let w: Wire = (0..4)
                        .map(|i| -> Edge { builder::line(&v[i], &v[(i + 1) % 4]) })
                        .collect();
                    let f: Face = builder::try_attach_plane(vec![w])
                        .map_err(|e| Error::Kernel(e.to_string()))?;
                    let f = orient_face(f, DVec3::Z);
                    Ok(builder::extrude(&f, Vector3::new(0.0, 0.0, size.z)))
                })?
            }
            Primitive::Cylinder { radius, height } => {
                if !(positive(radius) && positive(height)) {
                    return Err(Error::Invalid(
                        "cylinder radius and height must be positive".into(),
                    ));
                }
                guard("cylinder", || {
                    let w = circle_wire(o, Point3::new(radius, 0.0, 0.0), z);
                    let f: Face = builder::try_attach_plane(vec![w])
                        .map_err(|e| Error::Kernel(e.to_string()))?;
                    let f = orient_face(f, DVec3::Z);
                    Ok(builder::extrude(&f, z * height))
                })?
            }
            Primitive::Sphere { radius } => {
                if !positive(radius) {
                    return Err(Error::Invalid("sphere radius must be positive".into()));
                }
                guard("sphere", || {
                    let v0 = builder::vertex(Point3::new(0.0, 0.0, radius));
                    let half: Wire = builder::revolve(
                        &v0,
                        o,
                        Vector3::new(0.0, 1.0, 0.0),
                        builder::SweepAngle::Partial(Rad(PI)),
                        3,
                    );
                    let shell: Shell =
                        builder::revolve_wire(&half, o, z, builder::SweepAngle::Closed, 4);
                    Ok(Solid::new_unchecked(vec![shell]))
                })?
            }
            Primitive::Cone {
                radius1,
                radius2,
                height,
            } => {
                let ok =
                    radius1.is_finite() && radius2.is_finite() && radius1 >= 0.0 && radius2 >= 0.0;
                if !ok || !positive(height) || radius1.max(radius2) <= 0.0 {
                    return Err(Error::Invalid(
                        "cone radii must be non-negative (one positive), height positive".into(),
                    ));
                }
                guard("cone", || {
                    let eps = 1e-9 * (radius1.max(radius2) + height);
                    let mut pts = vec![Point3::new(0.0, 0.0, 0.0)];
                    if radius1 > eps {
                        pts.push(Point3::new(radius1, 0.0, 0.0));
                    }
                    if radius2 > eps {
                        pts.push(Point3::new(radius2, 0.0, height));
                    }
                    pts.push(Point3::new(0.0, 0.0, height));
                    let v = builder::vertices(pts);
                    let w: Wire = (0..v.len() - 1)
                        .map(|i| -> Edge { builder::line(&v[i], &v[i + 1]) })
                        .collect();
                    let shell: Shell =
                        builder::revolve_wire(&w, o, z, builder::SweepAngle::Closed, 4);
                    Ok(Solid::new_unchecked(vec![shell]))
                })?
            }
            Primitive::Torus { major, minor } => {
                if !(positive(major) && positive(minor)) || minor >= major {
                    return Err(Error::Invalid(
                        "torus radii must satisfy 0 < minor < major".into(),
                    ));
                }
                guard("torus", || {
                    let c = circle_wire(
                        Point3::new(major, 0.0, 0.0),
                        Point3::new(major + minor, 0.0, 0.0),
                        Vector3::new(0.0, 1.0, 0.0),
                    );
                    let shell: Shell = builder::revolve(&c, o, z, builder::SweepAngle::Closed, 4);
                    Ok(Solid::new_unchecked(vec![shell]))
                })?
            }
        };
        let s = guard("placement", || {
            Ok(builder::transformed(&local, mat4(placement)))
        })?;
        Ok(ExactSolid::new(s))
    }

    fn boolean(
        &self,
        a: &ExactSolid,
        b: &ExactSolid,
        kind: BooleanKind,
        tol: f64,
    ) -> Result<ExactSolid> {
        let name = match kind {
            BooleanKind::Union => "union",
            BooleanKind::Subtract => "subtract",
            BooleanKind::Intersect => "intersect",
        };
        guard(name, || {
            let r = match kind {
                BooleanKind::Union => monstertruck_solid::or(&a.solid, &b.solid, tol),
                BooleanKind::Subtract => monstertruck_solid::and(&a.solid, &not(&b.solid), tol),
                BooleanKind::Intersect => monstertruck_solid::and(&a.solid, &b.solid, tol),
            };
            r.map(ExactSolid::new)
                .map_err(|e| Error::Kernel(format!("{name}: {e}")))
        })
    }

    fn fillet(
        &self,
        s: &ExactSolid,
        edges: &[usize],
        radius: f64,
        chamfer: bool,
    ) -> Result<ExactSolid> {
        if !positive(radius) {
            return Err(Error::Invalid("fillet radius must be positive".into()));
        }
        if edges.is_empty() {
            return Err(Error::Invalid("no edges to fillet".into()));
        }
        guard("fillet", || {
            // Deep copy: the fillet mutates topology that cached bodies share.
            let solid: Solid = builder::clone(&s.solid);
            let all = unique_edges(&solid);
            let mut chosen = Vec::new();
            for &i in edges {
                let e = all
                    .get(i)
                    .ok_or_else(|| Error::Invalid(format!("edge index {i} out of range")))?;
                if !chosen.iter().any(|c: &Edge| c.id() == e.id()) {
                    chosen.push(e.clone());
                }
            }
            let opts = FilletOptions {
                profile: if chamfer {
                    FilletProfile::Chamfer
                } else {
                    FilletProfile::Round
                },
                ..FilletOptions::constant(radius)
            };
            let mut shells = solid.into_boundaries();
            for shell in &mut shells {
                let ids: std::collections::HashSet<_> = shell.edge_iter().map(|e| e.id()).collect();
                let mine: Vec<Edge> = chosen
                    .iter()
                    .filter(|e| ids.contains(&e.id()))
                    .cloned()
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                fillet_edges(shell, &mine, Some(&opts))
                    .map_err(|e| Error::Kernel(format!("fillet: {e:?}")))?;
                if shell.shell_condition() != ShellCondition::Closed {
                    return Err(Error::Kernel("fillet produced an open shell".into()));
                }
            }
            Ok(ExactSolid::new(Solid::new_unchecked(shells)))
        })
    }

    fn transform(&self, s: &ExactSolid, m: &DAffine3) -> Result<ExactSolid> {
        if !affine_ok(m) {
            return Err(Error::Invalid("singular or non-finite transform".into()));
        }
        guard("transform", || {
            let mut t: Solid = builder::transformed(&s.solid, mat4(m));
            if m.matrix3.determinant() < 0.0 {
                t.not();
            }
            Ok(ExactSolid::new(t))
        })
    }
}

// ------------------------------------------------------------------------------------------------
// Bodies

fn vertex_bbox(s: &Solid) -> BBox3 {
    BBox3::from_points(s.vertex_iter().map(|v| dp(v.point())))
}

/// Chord tolerance for the analysis mesh of a body of the given size.
pub(crate) fn analysis_tol(extent: f64) -> f64 {
    (extent * 1e-3).max(1e-6)
}

fn placeholder(n: usize) -> Vec<TopoName> {
    (0..n)
        .map(|i| TopoName {
            feature: FeatureId(0),
            tag: TopoTag::Derived { index: i as u32 },
        })
        .collect()
}

/// Build a body from an exact solid: analysis mesh, orientation normalization, validation, naming.
/// `name` receives the analysis and placeholder-named face infos and returns one name per face.
pub(crate) fn exact_body(
    id: BodyRef,
    source: FeatureId,
    mut solid: ExactSolid,
    name: impl FnOnce(&Analysis, &[FaceInfo]) -> Vec<TopoName>,
) -> Result<Body> {
    if solid.solid.is_empty() {
        return Err(Error::Kernel("empty solid".into()));
    }
    let closed = solid
        .solid
        .boundaries()
        .iter()
        .all(|sh| sh.shell_condition() == ShellCondition::Closed);
    if !closed {
        return Err(Error::Kernel("solid is not closed".into()));
    }
    let vb = vertex_bbox(&solid.solid);
    let tol = analysis_tol(vb.extent().max(1e-6));
    let mut an = guard("tessellation", || {
        Ok(Analysis::from_exact(&solid.solid, tol))
    })?;
    // Recompute with the true size (curved faces reach beyond the vertex hull).
    let ext = an.bbox().extent();
    if ext > 2.0 * vb.extent() {
        an = guard("tessellation", || {
            Ok(Analysis::from_exact(&solid.solid, analysis_tol(ext)))
        })?;
    }
    let vol = an.volume();
    if !vol.is_finite() || vol.abs() < 1e-12 * ext.powi(3).max(1e-30) || an.faces.is_empty() {
        return Err(Error::Kernel("solid has no volume".into()));
    }
    if vol < 0.0 {
        solid.solid.not();
        an = an.inverted();
    }
    let infos = an.face_infos(&placeholder(an.faces.len()));
    let mut names = name(&an, &infos);
    names.resize_with(an.faces.len(), || TopoName {
        feature: source,
        tag: TopoTag::Derived { index: u32::MAX },
    });
    Ok(finish_body(
        id,
        source,
        BodyRep::Exact(solid),
        an,
        names,
        None,
    ))
}

pub(crate) fn finish_body(
    id: BodyRef,
    source: FeatureId,
    rep: BodyRep,
    an: Analysis,
    names: Vec<TopoName>,
    mesh_only_reason: Option<String>,
) -> Body {
    let faces = an.face_infos(&names);
    let edges = an.edge_infos(&names);
    Body {
        id,
        rep,
        faces,
        edges,
        source,
        mesh_only_reason,
        analysis: Arc::new(an),
    }
}

/// Fill unnamed faces with `Derived { index }` names of `feature` (index counts unnamed faces).
pub(crate) fn fill_derived(names: Vec<Option<TopoName>>, feature: FeatureId) -> Vec<TopoName> {
    let mut k = 0;
    names
        .into_iter()
        .map(|n| {
            n.unwrap_or_else(|| {
                k += 1;
                TopoName {
                    feature,
                    tag: TopoTag::Derived { index: k - 1 },
                }
            })
        })
        .collect()
}

/// Rename faces of a freshly swept solid: caps by the probe points `start`/`end` (world points
/// inside the start/end caps, with outward normals), sides by the nearest side probe.
pub(crate) fn sweep_names(
    feature: FeatureId,
    an: &Analysis,
    infos: &[FaceInfo],
    caps: &[(DVec3, DVec3, TopoTag)],
    sides: &[(DVec3, TopoTag)],
) -> Vec<TopoName> {
    let thr = match_threshold(an.tol, an.tol, an.bbox().extent());
    let mut names = Vec::with_capacity(infos.len());
    let mut derived = 0;
    for (i, info) in infos.iter().enumerate() {
        let planar = matches!(info.kind, crate::SurfaceKind::Plane { .. });
        let cap = caps
            .iter()
            .find(|(p, n, _)| {
                planar && info.normal.dot(*n) > 1.0 - 1e-6 && an.distance_to_face(i, *p) <= thr
            })
            .map(|(_, _, t)| t.clone());
        let tag = cap.or_else(|| {
            sides
                .iter()
                .map(|(p, t)| (an.distance_to_face(i, *p), t))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .filter(|(d, _)| d.is_finite())
                .map(|(_, t)| t.clone())
        });
        names.push(TopoName {
            feature,
            tag: tag.unwrap_or_else(|| {
                derived += 1;
                TopoTag::Derived { index: derived - 1 }
            }),
        });
    }
    names
}

/// Extrude profile faces into one body named after `feature`.
/// `regions[i]` is the region index used for cap names of `profiles[i]`.
pub(crate) fn extrude_body(
    feature: FeatureId,
    profiles: &[(u32, ProfileFace)],
    dir: DVec3,
    back: f64,
    fwd: f64,
) -> Result<(Body, Option<String>)> {
    let k = MtKernel;
    let dir = dir
        .try_normalize()
        .ok_or_else(|| Error::Invalid("zero extrude direction".into()))?;
    let mut acc: Option<(Body, Option<String>)> = None;
    for (region, pf) in profiles {
        let solid = k.extrude(pf, dir, back, fwd)?;
        let interior = crate::profile::interior_point_of(pf).map(|p| pf.plane.to_world(p));
        let mut caps = Vec::new();
        if let Some(p) = interior {
            caps.push((p - dir * back, -dir, TopoTag::StartCap { region: *region }));
            caps.push((p + dir * fwd, dir, TopoTag::EndCap { region: *region }));
        }
        let sides: Vec<(DVec3, TopoTag)> = pf
            .tags
            .iter()
            .map(|t| {
                (
                    pf.plane.to_world(t.mid) + dir * (fwd - back) * 0.5,
                    TopoTag::Side {
                        entity: t.entity,
                        index: t.index,
                    },
                )
            })
            .collect();
        let body = exact_body(BodyRef(feature), feature, solid, |an, infos| {
            sweep_names(feature, an, infos, &caps, &sides)
        })?;
        acc = Some(match acc {
            None => (body, None),
            Some((prev, w)) => {
                let out = boolean_bodies(&prev, &[body], BooleanKind::Union, feature)?;
                (out.body, w.or(out.warning))
            }
        });
    }
    acc.ok_or_else(|| Error::Invalid("no profile regions".into()))
}

/// Revolve profile faces into one body.
pub(crate) fn revolve_body(
    feature: FeatureId,
    profiles: &[(u32, ProfileFace)],
    origin: DVec3,
    axis: DVec3,
    angle: f64,
) -> Result<(Body, Option<String>)> {
    let k = MtKernel;
    let axis_n = axis
        .try_normalize()
        .ok_or_else(|| Error::Invalid("zero revolve axis".into()))?;
    let full = angle.abs() >= TAU - 1e-9;
    let rot = |p: DVec3, a: f64| origin + DQuat::from_axis_angle(axis_n, a) * (p - origin);
    let mut acc: Option<(Body, Option<String>)> = None;
    for (region, pf) in profiles {
        let solid = k.revolve(pf, origin, axis_n, angle)?;
        let mut caps = Vec::new();
        if !full
            && let Some(p) = crate::profile::interior_point_of(pf).map(|p| pf.plane.to_world(p))
        {
            let sweep = axis_n.cross(p - origin).normalize_or_zero() * angle.signum();
            caps.push((p, -sweep, TopoTag::StartCap { region: *region }));
            let q = rot(p, angle);
            let sweep_end = DQuat::from_axis_angle(axis_n, angle) * sweep;
            caps.push((q, sweep_end, TopoTag::EndCap { region: *region }));
        }
        let half = if full { PI } else { angle * 0.5 };
        let sides: Vec<(DVec3, TopoTag)> = pf
            .tags
            .iter()
            .map(|t| {
                (
                    rot(pf.plane.to_world(t.mid), half),
                    TopoTag::Side {
                        entity: t.entity,
                        index: t.index,
                    },
                )
            })
            .collect();
        let body = exact_body(BodyRef(feature), feature, solid, |an, infos| {
            sweep_names(feature, an, infos, &caps, &sides)
        })?;
        acc = Some(match acc {
            None => (body, None),
            Some((prev, w)) => {
                let out = boolean_bodies(&prev, &[body], BooleanKind::Union, feature)?;
                (out.body, w.or(out.warning))
            }
        });
    }
    acc.ok_or_else(|| Error::Invalid("no profile regions".into()))
}

/// Primitive body; faces are named `PrimitiveFace { index }` in the kernel's face order.
pub(crate) fn primitive_body(
    feature: FeatureId,
    shape: &Primitive,
    placement: &DAffine3,
) -> Result<Body> {
    let s = MtKernel.primitive(shape, placement)?;
    exact_body(BodyRef(feature), feature, s, |_, infos| {
        (0..infos.len())
            .map(|i| TopoName {
                feature,
                tag: TopoTag::PrimitiveFace { index: i as u32 },
            })
            .collect()
    })
}

/// Result of a boolean between bodies.
#[derive(Clone, Debug)]
pub struct BoolOutcome {
    pub body: Body,
    /// Set when the exact kernel failed and the result is a mesh-only body.
    pub warning: Option<String>,
}

fn boxes_apart(a: &BBox3, b: &BBox3, gap: f64) -> bool {
    a.min.x > b.max.x + gap
        || b.min.x > a.max.x + gap
        || a.min.y > b.max.y + gap
        || b.min.y > a.max.y + gap
        || a.min.z > b.max.z + gap
        || b.min.z > a.max.z + gap
}

/// Plausibility check of a boolean result volume.
fn plausible(kind: BooleanKind, va: f64, vb: f64, vr: f64) -> bool {
    let slack = 2e-3 * va.max(vb) + 1e-12;
    match kind {
        BooleanKind::Union => vr >= va.max(vb) - slack && vr <= va + vb + slack,
        BooleanKind::Subtract => vr <= va + slack && vr >= va - vb - slack,
        BooleanKind::Intersect => vr <= va.min(vb) + slack && vr > 0.0,
    }
}

/// Boolean `target (kind) tool` with the architecture's policy: exact at 0.01, retry at 0.05 and
/// with swapped operands where valid, then each alternative tool in `tools` (e.g. an overshooting
/// cutter), finally the mesh kernel (result is mesh-only, with a warning).
///
/// The result keeps `target.id`; faces inherit names from both operands, new faces are
/// `Derived { index }` of `feature`.
pub fn boolean_bodies(
    target: &Body,
    tools: &[Body],
    kind: BooleanKind,
    feature: FeatureId,
) -> Result<BoolOutcome> {
    let Some(tool0) = tools.first() else {
        return Err(Error::Invalid("boolean without tool".into()));
    };
    let scale = target.extent().max(tool0.extent()).max(1e-9);
    // Disjoint operands: union keeps both shells, subtraction is a no-op, intersection is empty.
    if boxes_apart(&target.bbox(), &tool0.bbox(), 1e-6 * scale) {
        match kind {
            BooleanKind::Subtract => {
                let mut b = target.clone();
                b.source = feature;
                return Ok(BoolOutcome {
                    body: b,
                    warning: None,
                });
            }
            BooleanKind::Intersect => {
                return Err(Error::Invalid("the bodies do not intersect".into()));
            }
            BooleanKind::Union => {
                if let (BodyRep::Exact(a), BodyRep::Exact(b)) = (&target.rep, &tool0.rep) {
                    let mut shells = a.solid.boundaries().clone();
                    shells.extend(b.solid.boundaries().iter().cloned());
                    let merged = ExactSolid::new(Solid::new_unchecked(shells));
                    if let Ok(body) = named_result(target, tool0, kind, feature, merged) {
                        return Ok(BoolOutcome {
                            body,
                            warning: None,
                        });
                    }
                }
            }
        }
    }
    let mut last_err = String::new();
    if let BodyRep::Exact(a) = &target.rep {
        let va = target.approx_volume();
        for tool in tools {
            let BodyRep::Exact(b) = &tool.rep else {
                continue;
            };
            let vb = tool.approx_volume();
            let multi = a.solid.boundaries().len() > 1 || b.solid.boundaries().len() > 1;
            let (abox, bbox) = (
                shell_boxes(&a.solid, &target.analysis),
                shell_boxes(&b.solid, &tool.analysis),
            );
            for base in BOOL_TOLS {
                let tol = base.min(scale * 0.01).max(1e-6);
                let mut attempts: Vec<(&ExactSolid, &ExactSolid)> = vec![(a, b)];
                if kind != BooleanKind::Subtract && !multi {
                    attempts.push((b, a));
                }
                for (x, y) in attempts {
                    let r = if multi {
                        multi_shell_boolean(x, &abox, y, &bbox, kind, tol)
                    } else {
                        MtKernel.boolean(x, y, kind, tol)
                    };
                    match r {
                        Ok(r) => match named_result(target, tool, kind, feature, r) {
                            Ok(body) if plausible(kind, va, vb, body.approx_volume()) => {
                                return Ok(BoolOutcome {
                                    body,
                                    warning: None,
                                });
                            }
                            Ok(body) => {
                                last_err = format!(
                                    "implausible result volume {:.6}",
                                    body.approx_volume()
                                );
                            }
                            Err(e) => last_err = e.to_string(),
                        },
                        Err(e) => last_err = e.to_string(),
                    }
                }
            }
        }
    } else {
        last_err = target
            .mesh_only_reason
            .clone()
            .unwrap_or_else(|| "target is mesh-only".into());
    }
    if last_err.is_empty() {
        last_err = "tool is mesh-only".into();
    }
    let reason = format!("exact boolean failed ({last_err}); using mesh fallback");
    log::info!("{reason}");
    let body = fallback::mesh_boolean(target, tool0, kind, feature, reason.clone())?;
    Ok(BoolOutcome {
        body,
        warning: Some(reason),
    })
}

/// Bounding box of each shell of `solid` from the body's per-face analysis (faces are in shell order).
fn shell_boxes(solid: &Solid, an: &Analysis) -> Vec<BBox3> {
    let mut out = Vec::new();
    let mut k = 0;
    for shell in solid.boundaries() {
        let mut b = BBox3::EMPTY;
        for f in an.faces.iter().skip(k).take(shell.len()) {
            if !f.bbox.is_empty() {
                b = b.union(&f.bbox);
            }
        }
        k += shell.len();
        out.push(b);
    }
    out
}

/// Boolean of multi-shell solids, shell by shell: only shells whose boxes touch the tool shell are
/// passed to the kernel (monstertruck expects single-shell operands).
fn multi_shell_boolean(
    a: &ExactSolid,
    abox: &[BBox3],
    b: &ExactSolid,
    bbox: &[BBox3],
    kind: BooleanKind,
    tol: f64,
) -> Result<ExactSolid> {
    let single = |s: Shell| ExactSolid::new(Solid::new_unchecked(vec![s]));
    let tool_shells = b.solid.boundaries();
    if kind == BooleanKind::Intersect && tool_shells.len() > 1 {
        return Err(Error::Unsupported(
            "intersection with a multi-shell tool".into(),
        ));
    }
    let mut cur: Vec<(Shell, BBox3)> = a
        .solid
        .boundaries()
        .iter()
        .cloned()
        .zip(abox.iter().copied().chain(std::iter::repeat(BBox3::EMPTY)))
        .collect();
    for (ts, tb) in tool_shells
        .iter()
        .zip(bbox.iter().copied().chain(std::iter::repeat(BBox3::EMPTY)))
    {
        let tool = single(ts.clone());
        let (touch, rest): (Vec<_>, Vec<_>) = cur
            .into_iter()
            .partition(|(_, sb)| sb.is_empty() || tb.is_empty() || !boxes_apart(sb, &tb, tol));
        let mut next = match kind {
            BooleanKind::Intersect => Vec::new(),
            _ => rest,
        };
        match kind {
            BooleanKind::Subtract | BooleanKind::Intersect => {
                for (s, sb) in touch {
                    let r = MtKernel.boolean(&single(s), &tool, kind, tol)?;
                    next.extend(r.solid.boundaries().iter().cloned().map(|sh| (sh, sb)));
                }
            }
            BooleanKind::Union => {
                let mut merged = tool;
                let mut mb = tb;
                for (s, sb) in touch {
                    merged = MtKernel.boolean(&merged, &single(s), kind, tol)?;
                    mb = mb.union(&sb);
                }
                next.extend(merged.solid.boundaries().iter().cloned().map(|sh| (sh, mb)));
            }
        }
        cur = next;
    }
    if cur.is_empty() {
        return Err(Error::Kernel("boolean result is empty".into()));
    }
    Ok(ExactSolid::new(Solid::new_unchecked(
        cur.into_iter().map(|(s, _)| s).collect(),
    )))
}

fn named_result(
    target: &Body,
    tool: &Body,
    kind: BooleanKind,
    feature: FeatureId,
    r: ExactSolid,
) -> Result<Body> {
    exact_body(target.id, feature, r, |an, _| {
        let sources = [
            NameSource {
                names: target.faces.iter().map(|f| f.name.clone()).collect(),
                analysis: &target.analysis,
                flip: false,
            },
            NameSource {
                names: tool.faces.iter().map(|f| f.name.clone()).collect(),
                analysis: &tool.analysis,
                flip: kind == BooleanKind::Subtract,
            },
        ];
        fill_derived(inherit_names(an, &sources), feature)
    })
}

/// Fillet or chamfer edges of an exact body. Faces keep their names; blend faces are
/// `Blend { index }` (index into `edges`).
pub(crate) fn fillet_body(
    body: &Body,
    edges: &[usize],
    radius: f64,
    chamfer: bool,
    feature: FeatureId,
) -> Result<Body> {
    let BodyRep::Exact(s) = &body.rep else {
        return Err(Error::Unsupported(format!(
            "fillet on a mesh-only body ({})",
            body.mesh_only_reason.as_deref().unwrap_or("mesh")
        )));
    };
    if radius >= body.extent() {
        return Err(Error::Invalid(
            "fillet radius is larger than the body".into(),
        ));
    }
    let r = MtKernel.fillet(s, edges, radius, chamfer)?;
    let va = body.approx_volume();
    let out = exact_body(body.id, feature, r, |an, infos| {
        let sources = [NameSource {
            names: body.faces.iter().map(|f| f.name.clone()).collect(),
            analysis: &body.analysis,
            flip: false,
        }];
        let inherited = inherit_names(an, &sources);
        inherited
            .into_iter()
            .zip(infos)
            .map(|(n, info)| {
                n.unwrap_or_else(|| {
                    let k = edges
                        .iter()
                        .enumerate()
                        .map(|(k, &e)| (k, body.analysis.distance_to_edge(e, info.centroid)))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .map_or(0, |(k, _)| k);
                    TopoName {
                        feature,
                        tag: TopoTag::Blend { index: k as u32 },
                    }
                })
            })
            .collect()
    })?;
    if out.approx_volume() > va * (1.0 + 1e-3) || out.approx_volume() < va * 0.5 {
        return Err(Error::Kernel("fillet produced an implausible solid".into()));
    }
    Ok(out)
}

/// Transformed copy of a body with renamed faces.
pub(crate) fn transformed_body(
    body: &Body,
    m: &DAffine3,
    id: BodyRef,
    source: FeatureId,
    rename: impl Fn(usize, &TopoName) -> TopoName,
) -> Result<Body> {
    if !affine_ok(m) {
        return Err(Error::Invalid("singular or non-finite transform".into()));
    }
    let rep = match &body.rep {
        BodyRep::Exact(s) => BodyRep::Exact(MtKernel.transform(s, m)?),
        BodyRep::Mesh(ms) => {
            let mut ms = ms.clone();
            for p in &mut ms.positions {
                *p = m.transform_point3(*p);
            }
            if m.matrix3.determinant() < 0.0 {
                for t in &mut ms.triangles {
                    t.swap(1, 2);
                }
            }
            BodyRep::Mesh(ms)
        }
    };
    let an = body.analysis.transformed(m);
    let names: Vec<TopoName> = body
        .faces
        .iter()
        .enumerate()
        .map(|(i, f)| rename(i, &f.name))
        .collect();
    Ok(finish_body(
        id,
        source,
        rep,
        an,
        names,
        body.mesh_only_reason.clone(),
    ))
}

/// Map of body edges to indices for quick lookup in tests and regeneration.
#[allow(dead_code)]
pub(crate) fn edge_index_by_faces(body: &Body) -> HashMap<(TopoName, TopoName), usize> {
    body.edges
        .iter()
        .enumerate()
        .map(|(i, e)| ((e.faces[0].clone(), e.faces[1].clone()), i))
        .collect()
}
