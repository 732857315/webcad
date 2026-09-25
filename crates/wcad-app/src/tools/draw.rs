//! Draw commands (Draw ribbon tab): PLINE, ARC, CIRCLE (center/radius, 3P, 2P, Ttr), RECTANG,
//! POLYGON, ELLIPSE (axis/center/arc), SPLINE (fit points), POINT, DONUT and REVCLOUD.
//!
//! Every tool is a small state machine over [`Tool`]; points arrive already snapped and
//! constrained (object snaps, ortho/polar, direct distance entry against [`Tool::base_point`]),
//! so the tools only interpret them. The geometry of each command lives in pure functions next to
//! its tool (unit-tested on their own), and the tools are tested end to end through
//! [`crate::testing::Harness`] in `draw/tests.rs`.
//!
//! Option values that AutoCAD remembers between invocations (polygon sides, rectangle fillet,
//! donut diameters …) are kept in a per-thread [`Defaults`] record for the session.

mod arc;
mod circle;
mod donut;
mod ellipse;
mod pline;
mod point;
mod polygon;
mod rect;
mod revcloud;
mod spline;
#[cfg(test)]
mod tests;

/// Strings of this module (the file lives with the other string tables).
#[path = "../i18n/draw.rs"]
pub mod strings;

use std::cell::Cell;

use wcad_doc::{EntityId, EntityKind};
use wcad_math::DVec2;

use super::{Keyword, Tool, ToolCx};
use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::i18n::{Lang, core};

pub use arc::{
    ArcTool, arc_3p, arc_sca, arc_sce, arc_scl, arc_sea, arc_sed, arc_ser, tangent_bulge,
};
pub use circle::{CircleTool, TanGeom, circle_2p, circle_3p, circle_ttr};
pub use donut::{DonutTool, donut_hatch};
pub use ellipse::{EllipseTool, ellipse_from_axes};
pub use pline::PlineTool;
pub use point::PointTool;
pub use polygon::{PolygonTool, polygon_from_edge, polygon_points};
pub use rect::{Corners, RectTool, rect_polyline};
pub use revcloud::{RevcloudTool, revcloud_rect};
pub use spline::{SplineTool, closed_spline, open_spline};
pub use strings::{DrawStrings, draw as strings_of};

/// Register the draw commands. CIRCLE replaces the core proof tool (same name).
pub fn register(r: &mut CommandRegistry) {
    let tool = |name,
                aliases,
                label: fn(Lang) -> &'static str,
                icon,
                group,
                ctor: fn() -> Box<dyn Tool>| CommandSpec {
        name,
        aliases,
        label,
        icon,
        tab: Some(RibbonTab::Draw),
        group,
        kind: CommandKind::Tool(ctor),
    };
    r.add(tool(
        "CIRCLE",
        &["C"],
        |l| core(l).cmd_circle,
        "○",
        "core",
        || Box::new(CircleTool::default()),
    ));
    r.add(tool(
        "PLINE",
        &["PL"],
        |l| strings_of(l).cmd_pline,
        "⌇",
        "core",
        || Box::new(PlineTool::default()),
    ));
    r.add(tool(
        "ARC",
        &["A"],
        |l| strings_of(l).cmd_arc,
        "◠",
        "core",
        || Box::new(ArcTool::default()),
    ));
    r.add(tool(
        "RECTANG",
        &["REC", "RECTANGLE"],
        |l| strings_of(l).cmd_rectang,
        "▭",
        "shapes",
        || Box::new(RectTool::default()),
    ));
    r.add(tool(
        "POLYGON",
        &["POL"],
        |l| strings_of(l).cmd_polygon,
        "⬡",
        "shapes",
        || Box::new(PolygonTool::default()),
    ));
    r.add(tool(
        "ELLIPSE",
        &["EL"],
        |l| strings_of(l).cmd_ellipse,
        "⬭",
        "shapes",
        || Box::new(EllipseTool::default()),
    ));
    r.add(tool(
        "SPLINE",
        &["SPL"],
        |l| strings_of(l).cmd_spline,
        "∿",
        "curves",
        || Box::new(SplineTool::default()),
    ));
    r.add(tool(
        "POINT",
        &["PO"],
        |l| strings_of(l).cmd_point,
        "·",
        "curves",
        || Box::new(PointTool),
    ));
    r.add(tool(
        "DONUT",
        &["DO", "DOUGHNUT"],
        |l| strings_of(l).cmd_donut,
        "◎",
        "curves",
        || Box::new(DonutTool::default()),
    ));
    r.add(tool(
        "REVCLOUD",
        &[],
        |l| strings_of(l).cmd_revcloud,
        "☁",
        "curves",
        || Box::new(RevcloudTool::default()),
    ));
}

// -------------------------------------------------------------------------------------------
// Shared helpers

/// Option values remembered for the session (AutoCAD keeps them in system variables).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Defaults {
    /// POLYGON number of sides (POLYSIDES).
    pub polygon_sides: u32,
    /// POLYGON inscribed (`true`) or circumscribed.
    pub polygon_inscribed: bool,
    /// RECTANG corner mode.
    pub rect_corners: Corners,
    /// RECTANG Dimensions defaults.
    pub rect_length: f64,
    pub rect_width: f64,
    /// RECTANG rotation (radians).
    pub rect_rotation: f64,
    /// CIRCLE Ttr radius (CIRCLERAD); 0 = none yet.
    pub circle_radius: f64,
    /// DONUT diameters (DONUTID / DONUTOD).
    pub donut_inside: f64,
    pub donut_outside: f64,
    /// REVCLOUD arc chord length; 0 = automatic from the cloud size.
    pub revcloud_arc: f64,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            polygon_sides: 4,
            polygon_inscribed: true,
            rect_corners: Corners::Square,
            rect_length: 10.0,
            rect_width: 10.0,
            rect_rotation: 0.0,
            circle_radius: 0.0,
            donut_inside: 0.5,
            donut_outside: 1.0,
            revcloud_arc: 0.0,
        }
    }
}

thread_local! {
    static DEFAULTS: Cell<Defaults> = Cell::new(Defaults::default());
}

/// The session defaults.
pub fn defaults() -> Defaults {
    DEFAULTS.with(|d| d.get())
}

/// Update the session defaults.
pub fn set_defaults(f: impl FnOnce(&mut Defaults)) {
    DEFAULTS.with(|d| {
        let mut v = d.get();
        f(&mut v);
        d.set(v);
    });
}

pub(crate) fn kw(id: &'static str, key: &'static str, label: &'static str) -> Keyword {
    Keyword { id, key, label }
}

/// Polar angle of a vector (radians, CCW from +X).
pub(crate) fn angle_of(v: DVec2) -> f64 {
    v.y.atan2(v.x)
}

/// `v` is a finite, strictly positive number.
pub(crate) fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

/// Number formatting for `<default>` values in prompts (up to 4 decimals, no trailing zeros).
pub(crate) fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// The geometry is finite and not degenerate enough to be useless.
pub(crate) fn finite_pts(pts: &[DVec2]) -> bool {
    pts.iter().all(|p| p.is_finite())
}

/// Add one entity as its own undo step.
pub(crate) fn commit(cx: &mut ToolCx<'_>, label: &str, kind: EntityKind) -> EntityId {
    cx.transact(label, |tx| tx.add(kind))
}

/// Unit direction from `a` to `b`, if they are distinct.
pub(crate) fn dir(a: DVec2, b: DVec2) -> Option<DVec2> {
    (b - a).try_normalize()
}

/// An angle in degrees typed by the user, as radians (`None` if not finite).
pub(crate) fn deg(v: f64) -> Option<f64> {
    v.is_finite().then(|| v.to_radians())
}
