//! Persisted sketch model. Geometry references points by id so coincidences are topological
//! (shared points) or explicit constraints, SolveSpace style. Arcs are center + start + end points,
//! counter-clockwise from start to end, with an implicit equal-radius condition.
//!
//! Editing goes through the methods on [`Sketch`]; solving, dragging and diagnosis live in
//! [`crate::solver`] (also methods on [`Sketch`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wcad_geom2d::{Arc2, Circle2, Curve2, Line2};
use wcad_math::DVec2;

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SkEntityId(#[serde(deserialize_with = "deserialize_sketch_id")] pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SkConstraintId(#[serde(deserialize_with = "deserialize_sketch_id")] pub u32);

// Internally tagged parents buffer JSON map keys as strings, bypassing serde_json's numeric
// map-key deserializer. Keep writing numeric IDs, but also read their decimal key representation.
fn deserialize_sketch_id<'de, D>(deserializer: D) -> std::result::Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(u32),
        Key(String),
    }

    match Id::deserialize(deserializer)? {
        Id::Number(id) => Ok(id),
        Id::Key(key) if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) => {
            key.parse().map_err(serde::de::Error::custom)
        }
        Id::Key(_) => Err(serde::de::Error::custom("expected a decimal u32 sketch ID")),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SkGeom {
    Point {
        p: DVec2,
    },
    Line {
        a: SkEntityId,
        b: SkEntityId,
    },
    Circle {
        c: SkEntityId,
        r: f64,
    },
    Arc {
        c: SkEntityId,
        s: SkEntityId,
        e: SkEntityId,
    },
}

impl SkGeom {
    /// The point entities this geometry is defined by (empty for a point).
    pub fn defining_points(&self) -> Vec<SkEntityId> {
        match *self {
            SkGeom::Point { .. } => Vec::new(),
            SkGeom::Line { a, b } => vec![a, b],
            SkGeom::Circle { c, .. } => vec![c],
            SkGeom::Arc { c, s, e } => vec![c, s, e],
        }
    }

    pub fn is_point(&self) -> bool {
        matches!(self, SkGeom::Point { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkEntity {
    pub geom: SkGeom,
    /// Construction geometry is solved but never becomes part of a profile.
    #[serde(default)]
    pub construction: bool,
}

/// A dimensional value: evaluated number plus the expression the user typed (for future parameters).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DimValue {
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
}

impl DimValue {
    pub fn new(value: f64) -> Self {
        Self { value, expr: None }
    }
}

/// Which end of an arc a tangency refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArcEnd {
    Start,
    End,
}

/// Constraint kinds. Entity arguments must have the geometry the variant names
/// (`p*` = point, `line*` = line, `round*` = circle or arc, `arc*` = arc).
///
/// Orientation choices (which side of a line a tangent circle or a distance lies on, internal vs
/// external tangency, the sign of horizontal/vertical distances) are taken from the current
/// geometry each time the sketch is solved, so a solved sketch keeps its configuration.
/// Distances and the angle value are unsigned/signed as documented per variant.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ConstraintKind {
    Coincident {
        p1: SkEntityId,
        p2: SkEntityId,
    },
    PointOnLine {
        p: SkEntityId,
        line: SkEntityId,
    },
    /// Point on the (full) circle of a circle or arc.
    PointOnCircle {
        p: SkEntityId,
        round: SkEntityId,
    },
    Horizontal {
        line: SkEntityId,
    },
    Vertical {
        line: SkEntityId,
    },
    HorizontalPoints {
        p1: SkEntityId,
        p2: SkEntityId,
    },
    VerticalPoints {
        p1: SkEntityId,
        p2: SkEntityId,
    },
    Parallel {
        line1: SkEntityId,
        line2: SkEntityId,
    },
    Perpendicular {
        line1: SkEntityId,
        line2: SkEntityId,
    },
    /// Infinite line tangent to a circle/arc.
    TangentLineCircle {
        line: SkEntityId,
        round: SkEntityId,
    },
    /// Circle/arc tangent to circle/arc (external or internal, whichever is closer).
    TangentCircles {
        round1: SkEntityId,
        round2: SkEntityId,
    },
    EqualLength {
        line1: SkEntityId,
        line2: SkEntityId,
    },
    EqualRadius {
        round1: SkEntityId,
        round2: SkEntityId,
    },
    Midpoint {
        p: SkEntityId,
        line: SkEntityId,
    },
    /// `p1` and `p2` symmetric about the line.
    Symmetric {
        p1: SkEntityId,
        p2: SkEntityId,
        line: SkEntityId,
    },
    /// The point does not move (its stored position is the anchor).
    Fix {
        p: SkEntityId,
    },
    Distance {
        p1: SkEntityId,
        p2: SkEntityId,
        value: DimValue,
    },
    /// `|x2 - x1| = value`.
    HorizontalDistance {
        p1: SkEntityId,
        p2: SkEntityId,
        value: DimValue,
    },
    /// `|y2 - y1| = value`.
    VerticalDistance {
        p1: SkEntityId,
        p2: SkEntityId,
        value: DimValue,
    },
    /// Unsigned distance of a point from the infinite line.
    PointLineDistance {
        p: SkEntityId,
        line: SkEntityId,
        value: DimValue,
    },
    Length {
        line: SkEntityId,
        value: DimValue,
    },
    /// Signed angle (radians, CCW) from the direction of `line1` to the direction of `line2`,
    /// modulo π (a line reversed by the solver still satisfies it).
    Angle {
        line1: SkEntityId,
        line2: SkEntityId,
        value: DimValue,
    },
    Radius {
        round: SkEntityId,
        value: DimValue,
    },
    Diameter {
        round: SkEntityId,
        value: DimValue,
    },
    /// Smooth joint: the radius of the arc at `end` is perpendicular to the line. Endpoint
    /// coincidence is a separate constraint (or a shared point).
    TangentArcLine {
        arc: SkEntityId,
        end: ArcEnd,
        line: SkEntityId,
    },
    /// Smooth joint of two arcs: their radii at the given ends are collinear.
    TangentArcArc {
        arc1: SkEntityId,
        end1: ArcEnd,
        arc2: SkEntityId,
        end2: ArcEnd,
    },
}

/// Expected geometry of a constraint argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    Point,
    Line,
    /// Circle or arc.
    Round,
    Arc,
}

impl ConstraintKind {
    /// Entity arguments with the geometry each must have.
    pub fn args(&self) -> Vec<(SkEntityId, ArgKind)> {
        use ArgKind as A;
        use ConstraintKind as K;
        match *self {
            K::Coincident { p1, p2 }
            | K::HorizontalPoints { p1, p2 }
            | K::VerticalPoints { p1, p2 }
            | K::Distance { p1, p2, .. }
            | K::HorizontalDistance { p1, p2, .. }
            | K::VerticalDistance { p1, p2, .. } => vec![(p1, A::Point), (p2, A::Point)],
            K::PointOnLine { p, line }
            | K::Midpoint { p, line }
            | K::PointLineDistance { p, line, .. } => {
                vec![(p, A::Point), (line, A::Line)]
            }
            K::PointOnCircle { p, round } => vec![(p, A::Point), (round, A::Round)],
            K::Horizontal { line } | K::Vertical { line } | K::Length { line, .. } => {
                vec![(line, A::Line)]
            }
            K::Parallel { line1, line2 }
            | K::Perpendicular { line1, line2 }
            | K::EqualLength { line1, line2 }
            | K::Angle { line1, line2, .. } => vec![(line1, A::Line), (line2, A::Line)],
            K::TangentLineCircle { line, round } => vec![(line, A::Line), (round, A::Round)],
            K::TangentCircles { round1, round2 } | K::EqualRadius { round1, round2 } => {
                vec![(round1, A::Round), (round2, A::Round)]
            }
            K::Symmetric { p1, p2, line } => vec![(p1, A::Point), (p2, A::Point), (line, A::Line)],
            K::Fix { p } => vec![(p, A::Point)],
            K::Radius { round, .. } | K::Diameter { round, .. } => vec![(round, A::Round)],
            K::TangentArcLine { arc, line, .. } => vec![(arc, A::Arc), (line, A::Line)],
            K::TangentArcArc { arc1, arc2, .. } => vec![(arc1, A::Arc), (arc2, A::Arc)],
        }
    }

    /// Entities referenced directly by the constraint.
    pub fn entities(&self) -> Vec<SkEntityId> {
        self.args().into_iter().map(|(id, _)| id).collect()
    }

    /// The dimension value of dimensional constraints.
    pub fn dim_value(&self) -> Option<&DimValue> {
        use ConstraintKind as K;
        match self {
            K::Distance { value, .. }
            | K::HorizontalDistance { value, .. }
            | K::VerticalDistance { value, .. }
            | K::PointLineDistance { value, .. }
            | K::Length { value, .. }
            | K::Angle { value, .. }
            | K::Radius { value, .. }
            | K::Diameter { value, .. } => Some(value),
            _ => None,
        }
    }

    pub fn dim_value_mut(&mut self) -> Option<&mut DimValue> {
        use ConstraintKind as K;
        match self {
            K::Distance { value, .. }
            | K::HorizontalDistance { value, .. }
            | K::VerticalDistance { value, .. }
            | K::PointLineDistance { value, .. }
            | K::Length { value, .. }
            | K::Angle { value, .. }
            | K::Radius { value, .. }
            | K::Diameter { value, .. } => Some(value),
            _ => None,
        }
    }

    /// Dimensional constraints can be reference (non-driving) dimensions.
    pub fn is_dimensional(&self) -> bool {
        self.dim_value().is_some()
    }

    /// Stable type key (i18n lookup, UI labels).
    pub fn type_name(&self) -> &'static str {
        use ConstraintKind as K;
        match self {
            K::Coincident { .. } => "Coincident",
            K::PointOnLine { .. } => "PointOnLine",
            K::PointOnCircle { .. } => "PointOnCircle",
            K::Horizontal { .. } => "Horizontal",
            K::Vertical { .. } => "Vertical",
            K::HorizontalPoints { .. } => "HorizontalPoints",
            K::VerticalPoints { .. } => "VerticalPoints",
            K::Parallel { .. } => "Parallel",
            K::Perpendicular { .. } => "Perpendicular",
            K::TangentLineCircle { .. } => "TangentLineCircle",
            K::TangentCircles { .. } => "TangentCircles",
            K::EqualLength { .. } => "EqualLength",
            K::EqualRadius { .. } => "EqualRadius",
            K::Midpoint { .. } => "Midpoint",
            K::Symmetric { .. } => "Symmetric",
            K::Fix { .. } => "Fix",
            K::Distance { .. } => "Distance",
            K::HorizontalDistance { .. } => "HorizontalDistance",
            K::VerticalDistance { .. } => "VerticalDistance",
            K::PointLineDistance { .. } => "PointLineDistance",
            K::Length { .. } => "Length",
            K::Angle { .. } => "Angle",
            K::Radius { .. } => "Radius",
            K::Diameter { .. } => "Diameter",
            K::TangentArcLine { .. } => "TangentArcLine",
            K::TangentArcArc { .. } => "TangentArcArc",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkConstraint {
    pub kind: ConstraintKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Reference (non-driving) dimensions are measured, not enforced.
    #[serde(default = "yes")]
    pub driving: bool,
}

impl SkConstraint {
    /// Enabled and driving: the solver enforces it.
    pub fn is_enforced(&self) -> bool {
        self.enabled && self.driving
    }
}

fn yes() -> bool {
    true
}

/// Ids created by [`Sketch::add_rectangle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectangleIds {
    /// Bottom, right, top, left (for `corner0` = lower-left, `corner1` = upper-right).
    pub lines: [SkEntityId; 4],
    /// `corners[i]` is the start point of `lines[i]` (lower-left, lower-right, upper-right, upper-left).
    pub corners: [SkEntityId; 4],
    /// Horizontal(bottom), Horizontal(top), Vertical(right), Vertical(left).
    pub constraints: [SkConstraintId; 4],
}

/// What [`Sketch::remove_entity`] removed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub entities: Vec<SkEntityId>,
    pub constraints: Vec<SkConstraintId>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sketch {
    pub entities: BTreeMap<SkEntityId, SkEntity>,
    pub constraints: BTreeMap<SkConstraintId, SkConstraint>,
    #[serde(default)]
    next_entity: u32,
    #[serde(default)]
    next_constraint: u32,
}

impl Sketch {
    pub fn new() -> Self {
        Self::default()
    }

    fn alloc_entity(&mut self, geom: SkGeom) -> SkEntityId {
        // Robust against files whose counter lags behind the stored ids: ids are never reused.
        let floor = self
            .entities
            .last_key_value()
            .map_or(0, |(k, _)| k.0.saturating_add(1));
        let id = SkEntityId(self.next_entity.max(floor));
        self.next_entity = id.0.saturating_add(1);
        self.entities.insert(
            id,
            SkEntity {
                geom,
                construction: false,
            },
        );
        id
    }

    pub fn add_point(&mut self, p: DVec2) -> SkEntityId {
        self.alloc_entity(SkGeom::Point { p })
    }

    pub fn add_line(&mut self, a: SkEntityId, b: SkEntityId) -> SkEntityId {
        self.alloc_entity(SkGeom::Line { a, b })
    }

    pub fn add_circle(&mut self, c: SkEntityId, r: f64) -> SkEntityId {
        self.alloc_entity(SkGeom::Circle { c, r })
    }

    /// Arc counter-clockwise from `s` to `e` around `c`. The solver enforces `|c-s| = |c-e|`.
    pub fn add_arc(&mut self, c: SkEntityId, s: SkEntityId, e: SkEntityId) -> SkEntityId {
        self.alloc_entity(SkGeom::Arc { c, s, e })
    }

    /// Adds a constraint without validation (see [`Sketch::add_constraint_checked`]). Invalid
    /// constraints are ignored by the solver and listed in [`crate::SolveReport::invalid`].
    pub fn add_constraint(&mut self, kind: ConstraintKind) -> SkConstraintId {
        let floor = self
            .constraints
            .last_key_value()
            .map_or(0, |(k, _)| k.0.saturating_add(1));
        let id = SkConstraintId(self.next_constraint.max(floor));
        self.next_constraint = id.0.saturating_add(1);
        self.constraints.insert(
            id,
            SkConstraint {
                kind,
                enabled: true,
                driving: true,
            },
        );
        id
    }

    /// Validates entity types, degenerate arguments and dimension values, then adds the constraint.
    pub fn add_constraint_checked(&mut self, kind: ConstraintKind) -> Result<SkConstraintId> {
        self.validate_constraint(&kind)?;
        Ok(self.add_constraint(kind))
    }

    /// Adds a reference (non-driving) dimension; see [`Sketch::measure`].
    pub fn add_reference_dimension(&mut self, kind: ConstraintKind) -> Result<SkConstraintId> {
        if !kind.is_dimensional() {
            return Err(Error::BadConstraint(format!(
                "{} is not a dimension",
                kind.type_name()
            )));
        }
        let id = self.add_constraint_checked(kind)?;
        if let Some(c) = self.constraints.get_mut(&id) {
            c.driving = false;
        }
        Ok(id)
    }

    /// Checks that a constraint's entities exist with the right geometry and that its value is sane.
    pub fn validate_constraint(&self, kind: &ConstraintKind) -> Result<()> {
        let bad = |msg: String| Err(Error::BadConstraint(msg));
        let args = kind.args();
        for &(id, want) in &args {
            let Some(e) = self.entities.get(&id) else {
                return Err(Error::UnknownEntity(id));
            };
            let ok = matches!(
                (want, &e.geom),
                (ArgKind::Point, SkGeom::Point { .. })
                    | (ArgKind::Line, SkGeom::Line { .. })
                    | (ArgKind::Round, SkGeom::Circle { .. } | SkGeom::Arc { .. })
                    | (ArgKind::Arc, SkGeom::Arc { .. })
            );
            if !ok {
                return bad(format!(
                    "{}: entity {} must be a {want:?}",
                    kind.type_name(),
                    id.0
                ));
            }
            // Curves must be well formed (their points exist) for the solver to use them.
            for p in e.geom.defining_points() {
                if self.point(p).is_none() {
                    return bad(format!("entity {} references missing point {}", id.0, p.0));
                }
            }
        }
        // The same entity twice is degenerate for every kind taking two entities of one kind.
        use ConstraintKind as K;
        let same = |a: SkEntityId, b: SkEntityId| a == b;
        let degenerate = match *kind {
            K::Coincident { p1, p2 }
            | K::HorizontalPoints { p1, p2 }
            | K::VerticalPoints { p1, p2 }
            | K::Distance { p1, p2, .. }
            | K::HorizontalDistance { p1, p2, .. }
            | K::VerticalDistance { p1, p2, .. } => same(p1, p2),
            K::Parallel { line1, line2 }
            | K::Perpendicular { line1, line2 }
            | K::EqualLength { line1, line2 }
            | K::Angle { line1, line2, .. } => same(line1, line2),
            K::TangentCircles { round1, round2 } | K::EqualRadius { round1, round2 } => {
                same(round1, round2)
            }
            K::TangentArcArc { arc1, arc2, .. } => same(arc1, arc2),
            K::Symmetric { p1, p2, .. } => same(p1, p2),
            K::PointOnLine { p, line }
            | K::Midpoint { p, line }
            | K::PointLineDistance { p, line, .. } => {
                self.line_ends(line).is_some_and(|(a, b)| a == p || b == p)
            }
            _ => false,
        };
        if degenerate {
            return bad(format!("{}: degenerate arguments", kind.type_name()));
        }
        if let Some(v) = kind.dim_value() {
            let x = v.value;
            let ok = match kind {
                K::Angle { .. } => x.is_finite(),
                K::Radius { .. } | K::Diameter { .. } => x.is_finite() && x > 0.0,
                _ => x.is_finite() && x >= 0.0,
            };
            if !ok {
                return bad(format!("{}: invalid value {x}", kind.type_name()));
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- convenience builders

    /// Line with two new endpoints. Returns the line id (see [`Sketch::line_ends`]).
    pub fn add_line_points(&mut self, p0: DVec2, p1: DVec2) -> SkEntityId {
        let a = self.add_point(p0);
        let b = self.add_point(p1);
        self.add_line(a, b)
    }

    /// Axis-aligned rectangle from two opposite corners: 4 shared corner points, 4 lines,
    /// 2 horizontal + 2 vertical constraints. Lines run counter-clockwise when `corner0` is the
    /// lower-left corner.
    pub fn add_rectangle(&mut self, corner0: DVec2, corner1: DVec2) -> RectangleIds {
        let c = [
            corner0,
            DVec2::new(corner1.x, corner0.y),
            corner1,
            DVec2::new(corner0.x, corner1.y),
        ];
        let corners = c.map(|p| self.add_point(p));
        let lines = [0, 1, 2, 3].map(|i| self.add_line(corners[i], corners[(i + 1) % 4]));
        let constraints = [
            self.add_constraint(ConstraintKind::Horizontal { line: lines[0] }),
            self.add_constraint(ConstraintKind::Horizontal { line: lines[2] }),
            self.add_constraint(ConstraintKind::Vertical { line: lines[1] }),
            self.add_constraint(ConstraintKind::Vertical { line: lines[3] }),
        ];
        RectangleIds {
            lines,
            corners,
            constraints,
        }
    }

    /// Circle with a new center point.
    pub fn add_circle_center_radius(&mut self, c: DVec2, r: f64) -> SkEntityId {
        let pc = self.add_point(c);
        self.add_circle(pc, r)
    }

    /// Arc CCW around `c` from `s` towards `e`. `e` is projected onto the circle through `s`
    /// so the implicit equal-radius condition holds from the start.
    pub fn add_arc_center_start_end(&mut self, c: DVec2, s: DVec2, e: DVec2) -> Result<SkEntityId> {
        let r = c.distance(s);
        let de = e - c;
        if !(r > 0.0) || !r.is_finite() || !(de.length() > 0.0) || !de.is_finite() {
            return Err(Error::Degenerate(
                "arc needs distinct center, start and end".into(),
            ));
        }
        let e = c + de.normalize() * r;
        let pc = self.add_point(c);
        let ps = self.add_point(s);
        let pe = self.add_point(e);
        Ok(self.add_arc(pc, ps, pe))
    }

    /// Arc through three points (`p0` and `p2` are the ends, `p1` lies on the arc). The stored arc
    /// is CCW, so start/end are swapped when the points run clockwise.
    pub fn add_arc_3points(&mut self, p0: DVec2, p1: DVec2, p2: DVec2) -> Result<SkEntityId> {
        let Some(c) = circumcenter(p0, p1, p2) else {
            return Err(Error::Degenerate("arc points are collinear".into()));
        };
        let ccw = wcad_math::cross2(p1 - p0, p2 - p1) > 0.0;
        let (s, e) = if ccw { (p0, p2) } else { (p2, p0) };
        self.add_arc_center_start_end(c, s, e)
    }

    // ---------------------------------------------------------------- editing

    /// Removes an entity, every curve defined by it (when it is a point), the defining points of
    /// removed curves that no remaining curve uses, and all constraints referencing removed entities.
    pub fn remove_entity(&mut self, id: SkEntityId) -> Result<Removed> {
        let Some(ent) = self.entities.get(&id) else {
            return Err(Error::UnknownEntity(id));
        };
        let mut gone: BTreeSet<SkEntityId> = BTreeSet::new();
        gone.insert(id);
        let mut curves: Vec<SkEntityId> = Vec::new();
        if ent.geom.is_point() {
            curves.extend(
                self.entities
                    .iter()
                    .filter(|(_, e)| e.geom.defining_points().contains(&id))
                    .map(|(&k, _)| k),
            );
        } else {
            curves.push(id);
        }
        gone.extend(curves.iter().copied());
        // Orphaned defining points of removed curves.
        let mut candidates: BTreeSet<SkEntityId> = BTreeSet::new();
        for c in &curves {
            if let Some(e) = self.entities.get(c) {
                candidates.extend(e.geom.defining_points());
            }
        }
        for p in candidates {
            let used = self
                .entities
                .iter()
                .any(|(k, e)| !gone.contains(k) && e.geom.defining_points().contains(&p));
            if !used {
                gone.insert(p);
            }
        }
        let mut removed = Removed::default();
        for e in &gone {
            if self.entities.remove(e).is_some() {
                removed.entities.push(*e);
            }
        }
        let dead: Vec<SkConstraintId> = self
            .constraints
            .iter()
            .filter(|(_, c)| c.kind.entities().iter().any(|e| gone.contains(e)))
            .map(|(&k, _)| k)
            .collect();
        for c in dead {
            self.constraints.remove(&c);
            removed.constraints.push(c);
        }
        Ok(removed)
    }

    pub fn remove_constraint(&mut self, id: SkConstraintId) -> Result<SkConstraint> {
        self.constraints
            .remove(&id)
            .ok_or(Error::UnknownConstraint(id))
    }

    pub fn set_enabled(&mut self, id: SkConstraintId, on: bool) -> Result<()> {
        let c = self
            .constraints
            .get_mut(&id)
            .ok_or(Error::UnknownConstraint(id))?;
        c.enabled = on;
        Ok(())
    }

    /// Switch a dimension between driving and reference.
    pub fn set_driving(&mut self, id: SkConstraintId, driving: bool) -> Result<()> {
        let c = self
            .constraints
            .get_mut(&id)
            .ok_or(Error::UnknownConstraint(id))?;
        if !driving && !c.kind.is_dimensional() {
            return Err(Error::BadConstraint(format!(
                "{} is not a dimension",
                c.kind.type_name()
            )));
        }
        c.driving = driving;
        Ok(())
    }

    /// Change the value of a dimensional constraint (keeps the expression text if `expr` is `None`).
    pub fn set_dimension(
        &mut self,
        id: SkConstraintId,
        value: f64,
        expr: Option<String>,
    ) -> Result<()> {
        let c = self
            .constraints
            .get(&id)
            .ok_or(Error::UnknownConstraint(id))?;
        let mut kind = c.kind.clone();
        let Some(v) = kind.dim_value_mut() else {
            return Err(Error::BadConstraint(format!(
                "{} is not a dimension",
                c.kind.type_name()
            )));
        };
        v.value = value;
        if expr.is_some() {
            v.expr = expr;
        }
        self.validate_constraint(&kind)?;
        if let Some(c) = self.constraints.get_mut(&id) {
            c.kind = kind;
        }
        Ok(())
    }

    pub fn set_construction(&mut self, id: SkEntityId, construction: bool) -> Result<()> {
        let e = self.entities.get_mut(&id).ok_or(Error::UnknownEntity(id))?;
        e.construction = construction;
        Ok(())
    }

    /// Move a point without solving.
    pub fn set_point(&mut self, id: SkEntityId, p: DVec2) -> Result<()> {
        match self.entities.get_mut(&id).map(|e| &mut e.geom) {
            Some(SkGeom::Point { p: q }) => {
                *q = p;
                Ok(())
            }
            Some(_) => Err(Error::BadConstraint(format!(
                "entity {} is not a point",
                id.0
            ))),
            None => Err(Error::UnknownEntity(id)),
        }
    }

    /// Set a circle's radius without solving.
    pub fn set_circle_radius(&mut self, id: SkEntityId, r: f64) -> Result<()> {
        match self.entities.get_mut(&id).map(|e| &mut e.geom) {
            Some(SkGeom::Circle { r: q, .. }) => {
                *q = r;
                Ok(())
            }
            Some(_) => Err(Error::BadConstraint(format!(
                "entity {} is not a circle",
                id.0
            ))),
            None => Err(Error::UnknownEntity(id)),
        }
    }

    // ---------------------------------------------------------------- queries

    pub fn entity(&self, id: SkEntityId) -> Option<&SkEntity> {
        self.entities.get(&id)
    }

    pub fn constraint(&self, id: SkConstraintId) -> Option<&SkConstraint> {
        self.constraints.get(&id)
    }

    pub fn point(&self, id: SkEntityId) -> Option<DVec2> {
        match self.entities.get(&id)?.geom {
            SkGeom::Point { p } => Some(p),
            _ => None,
        }
    }

    /// Endpoint ids of a line.
    pub fn line_ends(&self, id: SkEntityId) -> Option<(SkEntityId, SkEntityId)> {
        match self.entities.get(&id)?.geom {
            SkGeom::Line { a, b } => Some((a, b)),
            _ => None,
        }
    }

    /// `[center, start, end]` point ids of an arc.
    pub fn arc_points(&self, id: SkEntityId) -> Option<[SkEntityId; 3]> {
        match self.entities.get(&id)?.geom {
            SkGeom::Arc { c, s, e } => Some([c, s, e]),
            _ => None,
        }
    }

    /// Center point id of a circle or arc.
    pub fn center_of(&self, id: SkEntityId) -> Option<SkEntityId> {
        match self.entities.get(&id)?.geom {
            SkGeom::Circle { c, .. } | SkGeom::Arc { c, .. } => Some(c),
            _ => None,
        }
    }

    /// Radius of a circle or arc (arc radius = |center - start|).
    pub fn radius_of(&self, id: SkEntityId) -> Option<f64> {
        match self.entities.get(&id)?.geom {
            SkGeom::Circle { r, .. } => Some(r),
            SkGeom::Arc { c, s, .. } => Some(self.point(c)?.distance(self.point(s)?)),
            _ => None,
        }
    }

    /// Constraints referencing an entity directly.
    pub fn constraints_of(&self, id: SkEntityId) -> Vec<SkConstraintId> {
        self.constraints
            .iter()
            .filter(|(_, c)| c.kind.entities().contains(&id))
            .map(|(&k, _)| k)
            .collect()
    }

    /// Curves (lines, circles, arcs) using a point as a defining point.
    pub fn curves_using(&self, point: SkEntityId) -> Vec<SkEntityId> {
        self.entities
            .iter()
            .filter(|(_, e)| e.geom.defining_points().contains(&point))
            .map(|(&k, _)| k)
            .collect()
    }

    /// Non-construction curves in plane coordinates, for region/profile detection and display.
    pub fn curves(&self) -> Vec<(SkEntityId, Curve2)> {
        self.entities
            .iter()
            .filter(|(_, e)| !e.construction)
            .filter_map(|(&id, e)| self.curve_of(&e.geom).map(|c| (id, c)))
            .collect()
    }

    /// All curves including construction geometry.
    pub fn all_curves(&self) -> Vec<(SkEntityId, Curve2)> {
        self.entities
            .iter()
            .filter_map(|(&id, e)| self.curve_of(&e.geom).map(|c| (id, c)))
            .collect()
    }

    /// The curve an entity describes (points have none).
    pub fn curve_of(&self, g: &SkGeom) -> Option<Curve2> {
        match *g {
            SkGeom::Point { .. } => None,
            SkGeom::Line { a, b } => Some(Curve2::Line(Line2::new(self.point(a)?, self.point(b)?))),
            SkGeom::Circle { c, r } => Some(Curve2::Circle(Circle2::new(self.point(c)?, r))),
            SkGeom::Arc { c, s, e } => {
                let (c, s, e) = (self.point(c)?, self.point(s)?, self.point(e)?);
                let r = c.distance(s);
                let a0 = (s - c).to_angle();
                let a1 = (e - c).to_angle();
                Some(Curve2::Arc(Arc2::new(c, r, a0, a1)))
            }
        }
    }
}

/// Center of the circle through three points, `None` when (nearly) collinear.
pub fn circumcenter(a: DVec2, b: DVec2, c: DVec2) -> Option<DVec2> {
    let (ab, ac) = (b - a, c - a);
    let d = 2.0 * wcad_math::cross2(ab, ac);
    let scale = ab.length_squared().max(ac.length_squared());
    if !(d.abs() > 1e-12 * scale) || !d.is_finite() {
        return None;
    }
    let (b2, c2) = (ab.length_squared(), ac.length_squared());
    let ux = (ac.y * b2 - ab.y * c2) / d;
    let uy = (ab.x * c2 - ac.x * b2) / d;
    let o = a + DVec2::new(ux, uy);
    o.is_finite().then_some(o)
}
