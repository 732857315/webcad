//! Analytic 2D geometry for drafting and sketches.
//!
//! The curve *types* in [`curves`] are part of the persisted document model and must stay
//! serde-compatible. Algorithms live in their own modules:
//! - [`curve`]: the [`Curve`] trait (evaluation, bounds, length, closest point, split, reverse,
//!   transform, flatten) for every curve type; parameter domains are documented there.
//! - [`bulge`]: polyline bulge helpers and the segment iterator.
//! - [`nurbs`]: NURBS evaluation, fit-point interpolation, knot insertion, exact conversions.
//! - [`intersect`]: curve/curve intersections.
//! - [`snap`]: object snap candidates.
//! - [`edit`]: trim, extend, fillet, chamfer, break, join.
//! - [`offset`]: parallel offset.
//! - [`regions`]: closed regions of a curve arrangement, area/centroid/containment.
//! - [`hatch`]: hatch patterns (`.pat`), built-ins and pattern line generation.
//! - [`text`]: TrueType text layout to outlines.
//! - [`tess`]: fill tessellation (lyon) for solid hatches and glyphs.
//! - [`dash`]: linetype dashing.

// `!(x > 0.0)` style comparisons are deliberate: they also reject NaN.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

pub mod bulge;
pub mod curve;
pub mod curves;
pub mod dash;
pub mod edit;
pub mod hatch;
pub mod intersect;
mod numeric;
pub mod nurbs;
pub mod offset;
pub mod regions;
pub mod snap;
pub mod tess;
pub mod text;

pub use bulge::{BulgeArc, PolySegment, bulge_to_arc, sweep_to_bulge};
pub use curve::Curve;
pub use curves::*;
pub use dash::dash_polyline;
pub use edit::{ChamferResult, FilletResult, break_at, chamfer, extend, fillet, join, trim};
pub use hatch::{HatchPattern, PatLine, hatch_lines, parse_pat};
pub use intersect::{Intersection, intersect, intersect_tol};
pub use offset::{offset, offset_signed};
pub use regions::{Loop, Region, find_regions, point_in_loop, region_at};
pub use snap::{SnapKind, snap_points};
pub use wcad_math::{BBox2, DAffine2, DVec2};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("degenerate geometry: {0}")]
    Degenerate(&'static str),
    #[error("no solution: {0}")]
    NoSolution(&'static str),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("font error: {0}")]
    Font(String),
}

pub type Result<T> = std::result::Result<T, Error>;
