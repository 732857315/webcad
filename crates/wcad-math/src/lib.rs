//! f64 math foundation shared by every webcad crate.
//!
//! Geometry is computed in `f64`; only the renderer converts to `f32`, relative to a batch origin.

pub use glam::{DAffine2, DAffine3, DMat2, DMat3, DMat4, DQuat, DVec2, DVec3, DVec4};

mod angle;
mod bbox;
mod plane;
pub mod tol;

pub use angle::*;
pub use bbox::{BBox2, BBox3};
pub use plane::Plane;

/// `true` when `a` and `b` differ by at most `eps`.
#[inline]
pub fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

/// `true` when the points are within `eps` of each other.
#[inline]
pub fn approx_eq2(a: DVec2, b: DVec2, eps: f64) -> bool {
    a.distance_squared(b) <= eps * eps
}

/// `true` when the points are within `eps` of each other.
#[inline]
pub fn approx_eq3(a: DVec3, b: DVec3, eps: f64) -> bool {
    a.distance_squared(b) <= eps * eps
}

/// 2D cross product (z component of the 3D cross product).
#[inline]
pub fn cross2(a: DVec2, b: DVec2) -> f64 {
    a.x * b.y - a.y * b.x
}

/// Counter-clockwise perpendicular.
#[inline]
pub fn perp(v: DVec2) -> DVec2 {
    DVec2::new(-v.y, v.x)
}
