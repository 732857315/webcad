//! The 3D part: an ordered feature history regenerated into bodies by `wcad-solid`.

use serde::{Deserialize, Serialize};
use wcad_math::{DAffine3, DVec2, DVec3};
use wcad_sketch::{SkEntityId, Sketch};

use crate::ids::FeatureId;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub features: Vec<Feature>,
    /// Regenerate only features `[0, rollback)`; `None` = all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<usize>,
}

impl Part {
    pub fn feature(&self, id: FeatureId) -> Option<&Feature> {
        self.features.iter().find(|f| f.id == id)
    }
    pub fn feature_mut(&mut self, id: FeatureId) -> Option<&mut Feature> {
        self.features.iter_mut().find(|f| f.id == id)
    }
    pub fn index_of(&self, id: FeatureId) -> Option<usize> {
        self.features.iter().position(|f| f.id == id)
    }
    /// Features that take part in regeneration (respects rollback, skips suppressed ones).
    pub fn active_features(&self) -> impl Iterator<Item = &Feature> {
        let end = self.rollback.unwrap_or(self.features.len()).min(self.features.len());
        self.features[..end].iter().filter(|f| !f.suppressed)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feature {
    pub id: FeatureId,
    pub name: String,
    #[serde(default)]
    pub suppressed: bool,
    pub kind: FeatureKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FeatureKind {
    Sketch { plane: PlaneRef, sketch: Sketch },
    Extrude { profile: ProfileRef, extent: Extent, #[serde(default)] reversed: bool, op: BodyOp },
    Revolve { profile: ProfileRef, axis: AxisRef, angle: f64, op: BodyOp },
    Fillet { edges: Vec<EdgeRef>, radius: f64 },
    Chamfer { edges: Vec<EdgeRef>, distance: f64 },
    Primitive { shape: Primitive, placement: DAffine3, op: BodyOp },
    Boolean { target: BodyRef, tools: Vec<BodyRef>, kind: BooleanKind, #[serde(default)] keep_tools: bool },
    LinearPattern { body: BodyRef, direction: DVec3, count: u32, spacing: f64 },
    CircularPattern { body: BodyRef, axis: AxisRef, count: u32, angle: f64 },
    Mirror { body: BodyRef, plane: PlaneRef, #[serde(default = "yes")] join: bool },
}

fn yes() -> bool {
    true
}

impl FeatureKind {
    /// Stable type key used for i18n lookup and default names ("Sketch", "Extrude", ...).
    pub fn type_key(&self) -> &'static str {
        match self {
            FeatureKind::Sketch { .. } => "Sketch",
            FeatureKind::Extrude { .. } => "Extrude",
            FeatureKind::Revolve { .. } => "Revolve",
            FeatureKind::Fillet { .. } => "Fillet",
            FeatureKind::Chamfer { .. } => "Chamfer",
            FeatureKind::Primitive { .. } => "Primitive",
            FeatureKind::Boolean { .. } => "Boolean",
            FeatureKind::LinearPattern { .. } => "LinearPattern",
            FeatureKind::CircularPattern { .. } => "CircularPattern",
            FeatureKind::Mirror { .. } => "Mirror",
        }
    }
}

/// How a body-producing feature combines with existing bodies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BodyOp {
    NewBody,
    /// Combine with `target` (`None` = the most recently created or modified body).
    Join { #[serde(default)] target: Option<BodyRef> },
    Cut { #[serde(default)] target: Option<BodyRef> },
    Intersect { #[serde(default)] target: Option<BodyRef> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BooleanKind {
    Union,
    Subtract,
    Intersect,
}

/// A body, identified by the feature that created it (bodies keep their identity through
/// Join/Cut/Fillet features that modify them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BodyRef(pub FeatureId);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Extent {
    Blind { distance: f64 },
    Symmetric { distance: f64 },
    TwoSided { forward: f64, backward: f64 },
    ThroughAll,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Primitive {
    Box { size: DVec3 },
    Cylinder { radius: f64, height: f64 },
    Sphere { radius: f64 },
    Cone { radius1: f64, radius2: f64, height: f64 },
    Torus { major: f64, minor: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlaneRef {
    /// World XY (Top).
    Xy,
    /// World XZ (Front).
    Xz,
    /// World YZ (Right).
    Yz,
    Offset { base: Box<PlaneRef>, distance: f64 },
    /// A planar face of a body.
    Face(FaceRef),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AxisRef {
    X,
    Y,
    Z,
    /// A line of a sketch (typically a construction line).
    SketchLine { sketch: FeatureId, line: SkEntityId },
    Edge(EdgeRef),
}

/// Closed regions of a sketch used as a profile. Each region is identified by a seed point inside it
/// (sketch-plane coordinates), which survives edits that move the geometry slightly.
/// An empty `regions` list means "every closed outer region".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileRef {
    pub sketch: FeatureId,
    #[serde(default)]
    pub regions: Vec<DVec2>,
}

/// Persistent name of a generated face or edge.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TopoName {
    pub feature: FeatureId,
    pub tag: TopoTag,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TopoTag {
    /// Side face swept from a sketch curve (`index` distinguishes pieces of a split curve).
    Side { entity: SkEntityId, index: u32 },
    StartCap { region: u32 },
    EndCap { region: u32 },
    /// Face created by a fillet/chamfer feature on its `index`-th edge.
    Blend { index: u32 },
    /// Face `index` of a primitive in the kernel's canonical order.
    PrimitiveFace { index: u32 },
    /// Face produced by a boolean or other op without a better name.
    Derived { index: u32 },
}

/// Geometric fingerprint used when the topological name no longer matches after regeneration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GeomHint {
    pub point: DVec3,
    pub direction: DVec3,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceRef {
    pub body: BodyRef,
    pub name: TopoName,
    pub hint: GeomHint,
}

/// An edge is named by its two adjacent faces (order-independent) plus a fingerprint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EdgeRef {
    pub body: BodyRef,
    pub faces: [TopoName; 2],
    pub hint: GeomHint,
}
