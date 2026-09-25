//! 2D drawing entities.

use serde::{Deserialize, Serialize};
use wcad_geom2d::{Arc2, Circle2, Curve2, EllipseArc2, Line2, Nurbs2, Polyline2};
use wcad_math::DVec2;

use crate::ids::{BlockId, DimStyleId, EntityId, LayerId, TextStyleId};
use crate::style::{Color, LineWeight, LinetypeRef};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: EntityId,
    pub layer: LayerId,
    #[serde(default)]
    pub color: Color,
    #[serde(default)]
    pub linetype: LinetypeRef,
    /// Per-entity linetype scale, multiplied with the drawing's global `ltscale`.
    #[serde(default = "one")]
    pub linetype_scale: f64,
    #[serde(default)]
    pub lineweight: LineWeight,
    pub kind: EntityKind,
}

fn one() -> f64 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EntityKind {
    Point { p: DVec2 },
    Line(Line2),
    Circle(Circle2),
    Arc(Arc2),
    Ellipse(EllipseArc2),
    Polyline(Polyline2),
    Spline(Nurbs2),
    Text(Text),
    MText(MText),
    Dimension(Dimension),
    Hatch(Hatch),
    Insert(Insert),
}

impl EntityKind {
    /// The entity as a plain curve, if it is one (used by snapping, trim/extend, offset, regions).
    pub fn as_curve(&self) -> Option<Curve2> {
        Some(match self {
            EntityKind::Line(l) => Curve2::Line(*l),
            EntityKind::Circle(c) => Curve2::Circle(*c),
            EntityKind::Arc(a) => Curve2::Arc(*a),
            EntityKind::Ellipse(e) => Curve2::Ellipse(*e),
            EntityKind::Polyline(p) => Curve2::Polyline(p.clone()),
            EntityKind::Spline(s) => Curve2::Spline(s.clone()),
            _ => return None,
        })
    }

    /// Wrap a curve as an entity kind.
    pub fn from_curve(c: Curve2) -> Self {
        match c {
            Curve2::Line(l) => EntityKind::Line(l),
            Curve2::Circle(c) => EntityKind::Circle(c),
            Curve2::Arc(a) => EntityKind::Arc(a),
            Curve2::Ellipse(e) => EntityKind::Ellipse(e),
            Curve2::Polyline(p) => EntityKind::Polyline(p),
            Curve2::Spline(s) => EntityKind::Spline(s),
        }
    }

    /// Stable, translatable-by-key type name ("LINE", "CIRCLE", ...), matching DXF names.
    pub fn type_name(&self) -> &'static str {
        match self {
            EntityKind::Point { .. } => "POINT",
            EntityKind::Line(_) => "LINE",
            EntityKind::Circle(_) => "CIRCLE",
            EntityKind::Arc(_) => "ARC",
            EntityKind::Ellipse(_) => "ELLIPSE",
            EntityKind::Polyline(_) => "LWPOLYLINE",
            EntityKind::Spline(_) => "SPLINE",
            EntityKind::Text(_) => "TEXT",
            EntityKind::MText(_) => "MTEXT",
            EntityKind::Dimension(_) => "DIMENSION",
            EntityKind::Hatch(_) => "HATCH",
            EntityKind::Insert(_) => "INSERT",
        }
    }
}

pub use wcad_geom2d::text::{HAlign, VAlign};

/// Single-line text. `pos` is the alignment point implied by `halign`/`valign`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Text {
    pub pos: DVec2,
    pub height: f64,
    #[serde(default)]
    pub rotation: f64,
    #[serde(default = "one")]
    pub width_factor: f64,
    #[serde(default)]
    pub oblique: f64,
    pub style: TextStyleId,
    #[serde(default)]
    pub halign: HAlign,
    #[serde(default)]
    pub valign: VAlign,
    pub text: String,
}

/// Multi-line text. `attachment` is 1..=9 (top-left .. bottom-right, DXF convention).
/// `text` may contain a subset of MTEXT format codes (`\P` paragraph, `{\H2x;...}`, ...).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MText {
    pub pos: DVec2,
    pub height: f64,
    /// Reference rectangle width; 0 = no wrapping.
    #[serde(default)]
    pub width: f64,
    #[serde(default)]
    pub rotation: f64,
    #[serde(default = "one")]
    pub line_spacing: f64,
    #[serde(default = "top_left")]
    pub attachment: u8,
    pub style: TextStyleId,
    pub text: String,
}

fn top_left() -> u8 {
    1
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DimKind {
    /// Horizontal/vertical/rotated linear dimension measured along `rotation`.
    Linear { p1: DVec2, p2: DVec2, line_point: DVec2, rotation: f64 },
    /// Aligned with p1→p2; `line_point` fixes the offset of the dimension line.
    Aligned { p1: DVec2, p2: DVec2, line_point: DVec2 },
    Radius { center: DVec2, point: DVec2 },
    Diameter { center: DVec2, point: DVec2 },
    /// Angle at `vertex` from ray→p1 to ray→p2 (CCW); `arc_point` places the arc.
    Angular { vertex: DVec2, p1: DVec2, p2: DVec2, arc_point: DVec2 },
    /// Ordinate dimension from the origin; `x_axis` chooses the measured coordinate.
    Ordinate { origin: DVec2, point: DVec2, leader_end: DVec2, x_axis: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dimension {
    pub kind: DimKind,
    pub style: DimStyleId,
    /// Text override; `<>` inside it stands for the measured value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_override: Option<String>,
    /// User-moved text position; `None` = automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_pos: Option<DVec2>,
}

/// One closed boundary loop of a hatch, as a chain of curves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HatchLoop {
    pub curves: Vec<Curve2>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HatchPatternRef {
    /// Pattern name, e.g. "SOLID", "ANSI31".
    pub name: String,
    #[serde(default)]
    pub angle: f64,
    #[serde(default = "one")]
    pub scale: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hatch {
    /// Outer loops and holes; nesting is resolved with the even-odd rule.
    pub loops: Vec<HatchLoop>,
    pub pattern: HatchPatternRef,
}

impl Hatch {
    pub fn is_solid(&self) -> bool {
        self.pattern.name.eq_ignore_ascii_case("SOLID")
    }
}

/// Block reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Insert {
    pub block: BlockId,
    pub pos: DVec2,
    #[serde(default = "unit_scale")]
    pub scale: DVec2,
    #[serde(default)]
    pub rotation: f64,
}

fn unit_scale() -> DVec2 {
    DVec2::ONE
}
