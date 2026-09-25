//! Feature regeneration: `wcad_doc::Part` → bodies, with per-feature status and an incremental cache.

use std::collections::hash_map::DefaultHasher;
use std::f64::consts::TAU;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use wcad_doc::{
    AxisRef, BodyOp, BodyRef, BooleanKind, EdgeRef, Extent, Feature, FeatureId, FeatureKind, Part,
    PlaneRef, ProfileRef, TopoName, TopoTag,
};
use wcad_geom2d::{Curve2, Region};
use wcad_math::{BBox2, DAffine3, DMat3, DVec3, Plane};
use wcad_sketch::{SkEntityId, SkGeom};

use crate::body::{Body, SurfaceKind};
use crate::kernel::{
    boolean_bodies, extrude_body, fillet_body, primitive_body, revolve_body, transformed_body,
};
use crate::profile::{
    ProfileFace, build_profile_face, merge_regions, outer_regions, region_contains, simple_regions,
};
use crate::{Error, Result};

/// Outcome of one feature.
#[derive(Clone, Debug, PartialEq)]
pub enum FeatureStatus {
    Ok,
    /// The feature was applied with a degradation (e.g. mesh fallback, fuzzy reference match).
    Warning(String),
    /// The feature failed and was skipped; later features still run.
    Error(String),
}

impl FeatureStatus {
    pub fn is_error(&self) -> bool {
        matches!(self, FeatureStatus::Error(_))
    }
}

#[derive(Clone, Debug, Default)]
pub struct RegenResult {
    pub bodies: Vec<Body>,
    /// One entry per active feature, in history order.
    pub feature_status: Vec<(FeatureId, FeatureStatus)>,
    /// Resolved plane of every active sketch feature.
    pub sketch_planes: Vec<(FeatureId, Plane)>,
}

impl RegenResult {
    pub fn status_of(&self, id: FeatureId) -> Option<&FeatureStatus> {
        self.feature_status
            .iter()
            .find(|(f, _)| *f == id)
            .map(|(_, s)| s)
    }
    pub fn body(&self, id: BodyRef) -> Option<&Body> {
        self.bodies.iter().find(|b| b.id == id)
    }
}

/// How many features the last [`regenerate`] call reused from the cache vs. recomputed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegenStats {
    pub reused: usize,
    pub computed: usize,
}

#[derive(Clone, Debug, Default)]
struct State {
    bodies: Vec<Body>,
    last: Option<BodyRef>,
    planes: Vec<(FeatureId, Plane)>,
}

#[derive(Clone, Debug)]
struct Entry {
    key: u64,
    status: FeatureStatus,
    state: Arc<State>,
}

/// Per-feature regeneration cache. Entry `i` is keyed by a hash chained over features `0..=i`,
/// so an edit of feature N invalidates exactly N..end.
#[derive(Clone, Debug, Default)]
pub struct RegenCache {
    entries: Vec<Entry>,
    stats: RegenStats,
}

impl RegenCache {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
    /// Statistics of the last regeneration.
    pub fn stats(&self) -> RegenStats {
        self.stats
    }
}

fn chain_key(prev: u64, f: &Feature) -> u64 {
    let mut h = DefaultHasher::new();
    prev.hash(&mut h);
    f.id.hash(&mut h);
    // Debug output of f64 is the shortest round-trip representation, so distinct values differ.
    format!("{:?}", f.kind).hash(&mut h);
    h.finish()
}

/// Regenerate the active features of `part` in order. Never panics; failing features get an
/// `Error` status and are skipped.
pub fn regenerate(part: &Part, cache: &mut RegenCache) -> RegenResult {
    let mut state = Arc::new(State::default());
    let mut key = 0x9e37_79b9_7f4a_7c15u64;
    let mut statuses = Vec::new();
    let mut entries = Vec::new();
    let mut stats = RegenStats::default();
    let mut reusing = true;
    for (i, f) in part.active_features().enumerate() {
        key = chain_key(key, f);
        if reusing && let Some(e) = cache.entries.get(i).filter(|e| e.key == key) {
            state = e.state.clone();
            statuses.push((f.id, e.status.clone()));
            entries.push(e.clone());
            stats.reused += 1;
            continue;
        }
        reusing = false;
        let mut next = (*state).clone();
        let mut ctx = Ctx {
            part,
            warnings: Vec::new(),
        };
        let status = match ctx.apply(f, &mut next) {
            Ok(()) if ctx.warnings.is_empty() => FeatureStatus::Ok,
            Ok(()) => FeatureStatus::Warning(ctx.warnings.join("; ")),
            Err(e) => {
                log::info!("feature {:?} ({}) failed: {e}", f.id, f.name);
                next = (*state).clone();
                FeatureStatus::Error(e.to_string())
            }
        };
        state = Arc::new(next);
        statuses.push((f.id, status.clone()));
        entries.push(Entry {
            key,
            status,
            state: state.clone(),
        });
        stats.computed += 1;
    }
    cache.entries = entries;
    cache.stats = stats;
    RegenResult {
        bodies: state.bodies.clone(),
        feature_status: statuses,
        sketch_planes: state.planes.clone(),
    }
}

struct Ctx<'a> {
    part: &'a Part,
    warnings: Vec<String>,
}

fn body_index(state: &State, r: BodyRef) -> Result<usize> {
    state
        .bodies
        .iter()
        .position(|b| b.id == r)
        .ok_or_else(|| Error::Unresolved(format!("body {} does not exist", r.0.0)))
}

/// Sketch plane on a planar face: origin = projection of the world origin (in-plane coordinates stay
/// stable when the face moves along its normal), x axis = world X projected into the plane (world Y
/// when the normal is close to X), normal = outward face normal.
fn face_plane(n: DVec3, point: DVec3) -> Plane {
    let n = n.normalize_or(DVec3::Z);
    let helper = if n.x.abs() < 0.9 { DVec3::X } else { DVec3::Y };
    let x_axis = (helper - n * n.dot(helper)).normalize();
    Plane {
        origin: n * n.dot(point),
        x_axis,
        y_axis: n.cross(x_axis),
    }
}

fn finite(v: f64, what: &str) -> Result<f64> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(Error::Invalid(format!("{what} is not a number")))
    }
}

impl Ctx<'_> {
    fn apply(&mut self, f: &Feature, st: &mut State) -> Result<()> {
        match &f.kind {
            FeatureKind::Sketch { plane, .. } => {
                let p = self.plane(plane, st)?;
                st.planes.push((f.id, p));
                Ok(())
            }
            FeatureKind::Extrude {
                profile,
                extent,
                reversed,
                op,
            } => self.extrude(f.id, profile, extent, *reversed, op, st),
            FeatureKind::Revolve {
                profile,
                axis,
                angle,
                op,
            } => {
                let angle = finite(*angle, "revolve angle")?;
                let (plane, profiles) = self.profile(profile, st)?;
                let _ = plane;
                let (o, d) = self.axis(axis, st)?;
                let (tool, w) = revolve_body(f.id, &profiles, o, d, angle)?;
                self.warn(w);
                self.body_op(f.id, op, vec![tool], st)
            }
            FeatureKind::Primitive {
                shape,
                placement,
                op,
            } => {
                let tool = primitive_body(f.id, shape, placement)?;
                self.body_op(f.id, op, vec![tool], st)
            }
            FeatureKind::Fillet { edges, radius } => self.fillet(f.id, edges, *radius, false, st),
            FeatureKind::Chamfer { edges, distance } => {
                self.fillet(f.id, edges, *distance, true, st)
            }
            FeatureKind::Boolean {
                target,
                tools,
                kind,
                keep_tools,
            } => {
                let ti = body_index(st, *target)?;
                if tools.is_empty() {
                    return Err(Error::Invalid("boolean without tool bodies".into()));
                }
                let mut remove = Vec::new();
                for t in tools {
                    if t == target {
                        continue;
                    }
                    let k = body_index(st, *t)?;
                    let out = boolean_bodies(&st.bodies[ti], &[st.bodies[k].clone()], *kind, f.id)?;
                    self.warn(out.warning);
                    st.bodies[ti] = out.body;
                    remove.push(*t);
                }
                if !keep_tools {
                    st.bodies.retain(|b| !remove.contains(&b.id));
                }
                st.last = Some(*target);
                Ok(())
            }
            FeatureKind::LinearPattern {
                body,
                direction,
                count,
                spacing,
            } => {
                let dir = direction
                    .try_normalize()
                    .filter(|d| d.is_finite())
                    .ok_or_else(|| Error::Invalid("pattern direction is zero".into()))?;
                let spacing = finite(*spacing, "pattern spacing")?;
                let moves: Vec<DAffine3> = (1..*count)
                    .map(|k| DAffine3::from_translation(dir * spacing * k as f64))
                    .collect();
                self.pattern(f.id, *body, &moves, st)
            }
            FeatureKind::CircularPattern {
                body,
                axis,
                count,
                angle,
            } => {
                let angle = finite(*angle, "pattern angle")?;
                let (o, d) = self.axis(axis, st)?;
                let n = *count as f64;
                let step = if angle.abs() >= TAU - 1e-9 {
                    TAU / n
                } else if *count > 1 {
                    angle / (n - 1.0)
                } else {
                    0.0
                };
                let moves: Vec<DAffine3> = (1..*count)
                    .map(|k| {
                        DAffine3::from_translation(o)
                            * DAffine3::from_axis_angle(d, step * k as f64)
                            * DAffine3::from_translation(-o)
                    })
                    .collect();
                self.pattern(f.id, *body, &moves, st)
            }
            FeatureKind::Mirror { body, plane, join } => {
                let bi = body_index(st, *body)?;
                let p = self.plane(plane, st)?;
                let n = p.normal();
                let m = DMat3::IDENTITY - 2.0 * DMat3::from_cols(n * n.x, n * n.y, n * n.z);
                let t = n * (2.0 * n.dot(p.origin));
                let mirror = DAffine3::from_mat3_translation(m, t);
                let src = st.bodies[bi].clone();
                let nf = src.faces.len() as u32;
                let _ = nf;
                let id = if *join { src.id } else { BodyRef(f.id) };
                let copy = transformed_body(&src, &mirror, id, f.id, |i, _| TopoName {
                    feature: f.id,
                    tag: TopoTag::Derived { index: i as u32 },
                })?;
                if *join {
                    let out = boolean_bodies(&src, &[copy], BooleanKind::Union, f.id)?;
                    self.warn(out.warning);
                    st.bodies[bi] = out.body;
                    st.last = Some(src.id);
                } else {
                    st.bodies.push(copy);
                    st.last = Some(id);
                }
                Ok(())
            }
        }
    }

    fn warn(&mut self, w: Option<String>) {
        if let Some(w) = w {
            self.warnings.push(w);
        }
    }

    fn pattern(
        &mut self,
        feature: FeatureId,
        body: BodyRef,
        moves: &[DAffine3],
        st: &mut State,
    ) -> Result<()> {
        if moves.len() > 1000 {
            return Err(Error::Invalid("pattern count is too large".into()));
        }
        let bi = body_index(st, body)?;
        let src = st.bodies[bi].clone();
        let nf = src.faces.len() as u32;
        let mut acc = src.clone();
        for (k, m) in moves.iter().enumerate() {
            let k = k as u32 + 1;
            let copy = transformed_body(&src, m, src.id, feature, |i, _| TopoName {
                feature,
                tag: TopoTag::Derived {
                    index: k * nf + i as u32,
                },
            })?;
            let out = boolean_bodies(&acc, &[copy], BooleanKind::Union, feature)?;
            self.warn(out.warning);
            acc = out.body;
        }
        acc.source = feature;
        st.bodies[bi] = acc;
        st.last = Some(body);
        Ok(())
    }

    fn fillet(
        &mut self,
        feature: FeatureId,
        edges: &[EdgeRef],
        radius: f64,
        chamfer: bool,
        st: &mut State,
    ) -> Result<()> {
        let radius = finite(radius, "fillet radius")?;
        if edges.is_empty() {
            return Err(Error::Invalid("no edges selected".into()));
        }
        let mut bodies: Vec<BodyRef> = Vec::new();
        for e in edges {
            if !bodies.contains(&e.body) {
                bodies.push(e.body);
            }
        }
        for b in bodies {
            let bi = body_index(st, b)?;
            let body = &st.bodies[bi];
            let mut idx = Vec::new();
            for e in edges.iter().filter(|e| e.body == b) {
                let i = match body.find_edge(&e.faces, Some(e.hint.point)) {
                    Some(i) => i,
                    None => {
                        let i = body
                            .nearest_edge(e.hint.point)
                            .ok_or_else(|| Error::Unresolved("edge not found".into()))?;
                        self.warnings
                            .push("an edge reference was matched by position".into());
                        i
                    }
                };
                if !idx.contains(&i) {
                    idx.push(i);
                }
            }
            let out = fillet_body(body, &idx, radius, chamfer, feature)?;
            st.bodies[bi] = out;
            st.last = Some(b);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn extrude(
        &mut self,
        feature: FeatureId,
        profile: &ProfileRef,
        extent: &Extent,
        reversed: bool,
        op: &BodyOp,
        st: &mut State,
    ) -> Result<()> {
        let (plane, profiles) = self.profile(profile, st)?;
        let mut dir = if reversed {
            -plane.normal()
        } else {
            plane.normal()
        };
        let is_cut = matches!(op, BodyOp::Cut { .. } | BodyOp::Intersect { .. });
        let (mut back, mut fwd) = match *extent {
            Extent::Blind { distance } => {
                let d = finite(distance, "extrude distance")?;
                if d < 0.0 {
                    dir = -dir;
                }
                (0.0, d.abs())
            }
            Extent::Symmetric { distance } => {
                let d = finite(distance, "extrude distance")?.abs();
                (d * 0.5, d * 0.5)
            }
            Extent::TwoSided { forward, backward } => (
                finite(backward, "extrude distance")?,
                finite(forward, "extrude distance")?,
            ),
            Extent::ThroughAll => {
                let mut bb = wcad_math::BBox3::EMPTY;
                for b in &st.bodies {
                    bb = bb.union(&b.bbox());
                }
                if bb.is_empty() {
                    return Err(Error::Invalid("Through All needs an existing body".into()));
                }
                let margin = bb.extent() * 0.01;
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for i in 0..8 {
                    let c = DVec3::new(
                        if i & 1 == 0 { bb.min.x } else { bb.max.x },
                        if i & 2 == 0 { bb.min.y } else { bb.max.y },
                        if i & 4 == 0 { bb.min.z } else { bb.max.z },
                    );
                    let d = (c - plane.origin).dot(dir);
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
                if is_cut {
                    ((-lo).max(0.0) + margin, hi.max(0.0) + margin)
                } else if hi > margin * 1e-3 {
                    (0.0, hi)
                } else {
                    return Err(Error::Invalid(
                        "nothing to extrude through in this direction".into(),
                    ));
                }
            }
        };
        if (back + fwd).is_nan() || back + fwd <= 0.0 {
            return Err(Error::Invalid("extrude length must be positive".into()));
        }
        let (tool, w) = extrude_body(feature, &profiles, dir, back, fwd)?;
        self.warn(w);
        let mut tools = vec![tool];
        // Flush faces: an alternative tool that overshoots (cut) or sinks into (join) the target.
        if !matches!(op, BodyOp::NewBody)
            && let Ok(target) = self.op_target(op, st)
        {
            let t = &st.bodies[target];
            let ov = t.extent() * 1e-3;
            let tol = t.extent() * 1e-7 + 1e-9;
            let (mut start_flush, mut end_flush) = (false, false);
            for f in &t.faces {
                if let SurfaceKind::Plane { plane: fp } = &f.kind
                    && fp.normal().dot(dir).abs() > 1.0 - 1e-9
                {
                    let d = (fp.origin - plane.origin).dot(dir);
                    start_flush |= (d + back).abs() < tol;
                    end_flush |= (d - fwd).abs() < tol;
                }
            }
            if start_flush || end_flush {
                if start_flush {
                    back += ov;
                }
                if end_flush && is_cut {
                    fwd += ov;
                }
                if let Ok((alt, _)) = extrude_body(feature, &profiles, dir, back, fwd) {
                    if is_cut {
                        tools.insert(0, alt);
                    } else {
                        tools.push(alt);
                    }
                }
            }
        }
        self.body_op(feature, op, tools, st)
    }

    fn op_target(&self, op: &BodyOp, st: &State) -> Result<usize> {
        let target = match op {
            BodyOp::NewBody => None,
            BodyOp::Join { target } | BodyOp::Cut { target } | BodyOp::Intersect { target } => {
                *target
            }
        };
        match target {
            Some(r) => body_index(st, r),
            None => st
                .last
                .and_then(|r| st.bodies.iter().position(|b| b.id == r))
                .or_else(|| st.bodies.len().checked_sub(1))
                .ok_or_else(|| Error::Unresolved("there is no body to combine with".into())),
        }
    }

    /// Apply a body-producing feature's tool (with alternatives for the boolean ladder).
    fn body_op(
        &mut self,
        feature: FeatureId,
        op: &BodyOp,
        tools: Vec<Body>,
        st: &mut State,
    ) -> Result<()> {
        let kind = match op {
            BodyOp::NewBody => None,
            BodyOp::Join { .. } => Some(BooleanKind::Union),
            BodyOp::Cut { .. } => Some(BooleanKind::Subtract),
            BodyOp::Intersect { .. } => Some(BooleanKind::Intersect),
        };
        let Some(first) = tools.first() else {
            return Err(Error::Invalid("no tool body".into()));
        };
        let Some(kind) = kind else {
            st.bodies.retain(|b| b.id != BodyRef(feature));
            st.bodies.push(first.clone());
            st.last = Some(BodyRef(feature));
            return Ok(());
        };
        let ti = match self.op_target(op, st) {
            Ok(i) => i,
            Err(e) if kind == BooleanKind::Union && st.bodies.is_empty() => {
                let _ = e;
                self.warnings
                    .push("nothing to join; created a new body".into());
                st.bodies.push(first.clone());
                st.last = Some(BodyRef(feature));
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        let out = boolean_bodies(&st.bodies[ti], &tools, kind, feature)?;
        self.warn(out.warning);
        st.last = Some(out.body.id);
        st.bodies[ti] = out.body;
        Ok(())
    }

    fn plane(&mut self, r: &PlaneRef, st: &State) -> Result<Plane> {
        match r {
            PlaneRef::Xy => Ok(Plane::XY),
            PlaneRef::Xz => Ok(Plane::XZ),
            PlaneRef::Yz => Ok(Plane::YZ),
            PlaneRef::Offset { base, distance } => Ok(self
                .plane(base, st)?
                .offset(finite(*distance, "plane offset")?)),
            PlaneRef::Face(fr) => {
                let bi = body_index(st, fr.body)?;
                let body = &st.bodies[bi];
                let fi = match body.find_face(&fr.name, Some(fr.hint.point)) {
                    Some(i) => i,
                    None => {
                        let i = body
                            .nearest_face(fr.hint.point)
                            .ok_or_else(|| Error::Unresolved("face not found".into()))?;
                        self.warnings
                            .push("a face reference was matched by position".into());
                        i
                    }
                };
                match &body.faces[fi].kind {
                    SurfaceKind::Plane { plane } => Ok(face_plane(plane.normal(), plane.origin)),
                    _ => Err(Error::Invalid("the selected face is not planar".into())),
                }
            }
        }
    }

    fn sketch_of(&self, id: FeatureId) -> Result<&wcad_sketch::Sketch> {
        match self.part.feature(id).map(|f| &f.kind) {
            Some(FeatureKind::Sketch { sketch, .. }) => Ok(sketch),
            Some(_) => Err(Error::Unresolved(format!(
                "feature {} is not a sketch",
                id.0
            ))),
            None => Err(Error::Unresolved(format!("sketch {} does not exist", id.0))),
        }
    }

    fn profile(&mut self, pr: &ProfileRef, st: &State) -> Result<(Plane, Vec<(u32, ProfileFace)>)> {
        let sketch = self.sketch_of(pr.sketch)?;
        let plane = st
            .planes
            .iter()
            .find(|(f, _)| *f == pr.sketch)
            .map(|(_, p)| *p)
            .ok_or_else(|| {
                Error::Unresolved(format!(
                    "sketch {} is not regenerated before this feature",
                    pr.sketch.0
                ))
            })?;
        let curves = sketch.curves();
        let ids: Vec<SkEntityId> = curves.iter().map(|(id, _)| *id).collect();
        let list: Vec<Curve2> = curves.into_iter().map(|(_, c)| c).collect();
        let mut bb = BBox2::EMPTY;
        for c in &list {
            match c {
                Curve2::Line(l) => {
                    bb.include(l.a);
                    bb.include(l.b);
                }
                Curve2::Circle(ci) => {
                    bb.include(ci.c - wcad_math::DVec2::splat(ci.r));
                    bb.include(ci.c + wcad_math::DVec2::splat(ci.r));
                }
                Curve2::Arc(a) => {
                    bb.include(a.c - wcad_math::DVec2::splat(a.r));
                    bb.include(a.c + wcad_math::DVec2::splat(a.r));
                }
                _ => {}
            }
        }
        let tol = (bb.size().length() * 1e-7).max(1e-9);
        let mut regions = wcad_geom2d::find_regions(&list, tol);
        if regions.is_empty() {
            regions = simple_regions(&list, tol * 10.0);
        }
        if regions.is_empty() {
            return Err(Error::Invalid("the sketch has no closed profile".into()));
        }
        let chosen: Vec<usize> = if pr.regions.is_empty() {
            outer_regions(&regions)
        } else {
            let mut v = Vec::new();
            for seed in &pr.regions {
                match (0..regions.len()).find(|&i| region_contains(&regions[i], *seed)) {
                    Some(i) if !v.contains(&i) => v.push(i),
                    Some(_) => {}
                    None => self.warnings.push(format!(
                        "no profile region at ({:.3}, {:.3})",
                        seed.x, seed.y
                    )),
                }
            }
            v
        };
        if chosen.is_empty() {
            return Err(Error::Invalid("no profile region selected".into()));
        }
        let entity_of = |s: usize| ids.get(s).copied().unwrap_or(SkEntityId(s as u32));
        // Adjacent regions are merged in 2D (a union of separate sweeps would hit coplanar faces).
        let selected: Vec<Region> = if chosen.len() > 1 {
            let refs: Vec<&Region> = chosen.iter().map(|&i| &regions[i]).collect();
            merge_regions(&refs)
                .unwrap_or_else(|| chosen.iter().map(|&i| regions[i].clone()).collect())
        } else {
            vec![regions[chosen[0]].clone()]
        };
        let mut out = Vec::new();
        for (k, r) in selected.iter().enumerate() {
            out.push((k as u32, build_profile_face(&plane, r, &entity_of)?));
        }
        Ok((plane, out))
    }

    fn axis(&mut self, a: &AxisRef, st: &State) -> Result<(DVec3, DVec3)> {
        match a {
            AxisRef::X => Ok((DVec3::ZERO, DVec3::X)),
            AxisRef::Y => Ok((DVec3::ZERO, DVec3::Y)),
            AxisRef::Z => Ok((DVec3::ZERO, DVec3::Z)),
            AxisRef::SketchLine { sketch, line } => {
                let sk = self.sketch_of(*sketch)?;
                let plane = st
                    .planes
                    .iter()
                    .find(|(f, _)| f == sketch)
                    .map(|(_, p)| *p)
                    .ok_or_else(|| Error::Unresolved("axis sketch is not regenerated".into()))?;
                let (a, b) = match sk.entities.get(line).map(|e| &e.geom) {
                    Some(SkGeom::Line { a, b }) => (
                        sk.point(*a)
                            .ok_or_else(|| Error::Unresolved("axis line point".into()))?,
                        sk.point(*b)
                            .ok_or_else(|| Error::Unresolved("axis line point".into()))?,
                    ),
                    _ => return Err(Error::Unresolved("axis is not a sketch line".into())),
                };
                let (pa, pb) = (plane.to_world(a), plane.to_world(b));
                let d = (pb - pa)
                    .try_normalize()
                    .ok_or_else(|| Error::Invalid("axis line has zero length".into()))?;
                Ok((pa, d))
            }
            AxisRef::Edge(er) => {
                let bi = body_index(st, er.body)?;
                let body = &st.bodies[bi];
                let ei = match body.find_edge(&er.faces, Some(er.hint.point)) {
                    Some(i) => i,
                    None => {
                        let i = body
                            .nearest_edge(er.hint.point)
                            .ok_or_else(|| Error::Unresolved("edge not found".into()))?;
                        self.warnings
                            .push("an axis edge was matched by position".into());
                        i
                    }
                };
                let pts = &body.analysis.edges[ei].points;
                let (Some(&p0), Some(&p1)) = (pts.first(), pts.last()) else {
                    return Err(Error::Unresolved("edge has no geometry".into()));
                };
                let d = (p1 - p0)
                    .try_normalize()
                    .ok_or_else(|| Error::Invalid("axis edge is closed".into()))?;
                let len = p0.distance(p1);
                let straight = pts.iter().all(|p| {
                    let v = *p - p0;
                    (v - d * v.dot(d)).length() <= 1e-6 * len.max(1.0)
                });
                if !straight {
                    return Err(Error::Invalid("axis edge is not straight".into()));
                }
                Ok((p0, d))
            }
        }
    }
}
