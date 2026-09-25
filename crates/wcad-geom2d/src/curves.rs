//! Persisted 2D curve types. Field names and meanings are part of the file format.

use serde::{Deserialize, Serialize};
use wcad_math::DVec2;

/// Straight segment from `a` to `b`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line2 {
    pub a: DVec2,
    pub b: DVec2,
}

/// Full circle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Circle2 {
    pub c: DVec2,
    pub r: f64,
}

/// Circular arc, counter-clockwise from `start` to `end` (radians, measured from +X).
/// `start == end` is not a full circle; use [`Circle2`] for that.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Arc2 {
    pub c: DVec2,
    pub r: f64,
    pub start: f64,
    pub end: f64,
}

/// Elliptical arc. `major` is the semi-major axis vector (length = semi-major radius), `ratio` is
/// minor/major in `(0, 1]`, and `start`/`end` are *parametric* angles (DXF convention), CCW.
/// `start = 0, end = 2π` is a full ellipse.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EllipseArc2 {
    pub c: DVec2,
    pub major: DVec2,
    pub ratio: f64,
    pub start: f64,
    pub end: f64,
}

/// Polyline vertex. `bulge` describes the segment from this vertex to the next:
/// `bulge = tan(sweep / 4)`, positive = counter-clockwise arc, 0 = straight.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolyVertex {
    pub p: DVec2,
    #[serde(default)]
    pub bulge: f64,
}

/// Lightweight polyline with optional arc segments (DXF LWPOLYLINE semantics).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Polyline2 {
    pub verts: Vec<PolyVertex>,
    #[serde(default)]
    pub closed: bool,
}

/// Non-uniform rational B-spline. `weights` is empty for a non-rational spline.
/// `fit_points` are kept when the spline was created from fit points (for editing/export).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Nurbs2 {
    pub degree: u32,
    pub ctrl: Vec<DVec2>,
    #[serde(default)]
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
    #[serde(default)]
    pub fit_points: Vec<DVec2>,
    #[serde(default)]
    pub closed: bool,
}

/// Any 2D curve. Algorithms in this crate operate on `Curve2`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Curve2 {
    Line(Line2),
    Circle(Circle2),
    Arc(Arc2),
    Ellipse(EllipseArc2),
    Polyline(Polyline2),
    Spline(Nurbs2),
}

impl Line2 {
    pub const fn new(a: DVec2, b: DVec2) -> Self {
        Self { a, b }
    }
    pub fn length(&self) -> f64 {
        self.a.distance(self.b)
    }
    pub fn dir(&self) -> DVec2 {
        (self.b - self.a).normalize_or_zero()
    }
    pub fn at(&self, t: f64) -> DVec2 {
        self.a.lerp(self.b, t)
    }
    pub fn midpoint(&self) -> DVec2 {
        (self.a + self.b) * 0.5
    }
}

impl Circle2 {
    pub const fn new(c: DVec2, r: f64) -> Self {
        Self { c, r }
    }
    pub fn at_angle(&self, a: f64) -> DVec2 {
        self.c + DVec2::new(a.cos(), a.sin()) * self.r
    }
}

impl Arc2 {
    pub const fn new(c: DVec2, r: f64, start: f64, end: f64) -> Self {
        Self { c, r, start, end }
    }
    /// Counter-clockwise sweep in `(0, 2π]`.
    pub fn sweep(&self) -> f64 {
        wcad_math::ccw_sweep(self.start, self.end)
    }
    pub fn at_angle(&self, a: f64) -> DVec2 {
        self.c + DVec2::new(a.cos(), a.sin()) * self.r
    }
    pub fn start_point(&self) -> DVec2 {
        self.at_angle(self.start)
    }
    pub fn end_point(&self) -> DVec2 {
        self.at_angle(self.end)
    }
    pub fn mid_point(&self) -> DVec2 {
        self.at_angle(self.start + self.sweep() * 0.5)
    }
}

impl EllipseArc2 {
    pub fn minor(&self) -> DVec2 {
        wcad_math::perp(self.major) * self.ratio
    }
    /// Point at parametric angle `t`.
    pub fn at_param(&self, t: f64) -> DVec2 {
        self.c + self.major * t.cos() + self.minor() * t.sin()
    }
    pub fn is_full(&self) -> bool {
        (self.end - self.start - std::f64::consts::TAU).abs() < 1e-12
            || (self.end - self.start).abs() < 1e-15
    }
}

impl PolyVertex {
    pub const fn new(p: DVec2) -> Self {
        Self { p, bulge: 0.0 }
    }
    pub const fn with_bulge(p: DVec2, bulge: f64) -> Self {
        Self { p, bulge }
    }
}

impl Polyline2 {
    pub fn from_points<I: IntoIterator<Item = DVec2>>(pts: I, closed: bool) -> Self {
        Self {
            verts: pts.into_iter().map(PolyVertex::new).collect(),
            closed,
        }
    }
    /// Number of segments (a closed polyline has one extra closing segment).
    pub fn segment_count(&self) -> usize {
        match (self.verts.len(), self.closed) {
            (0 | 1, _) => 0,
            (n, true) => n,
            (n, false) => n - 1,
        }
    }
}
