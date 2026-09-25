//! Tolerances. Pick and snap tolerances are in pixels and converted by the caller.

/// Absolute linear tolerance for "same point" tests in model units.
pub const LINEAR: f64 = 1e-9;
/// Angular tolerance in radians.
pub const ANGULAR: f64 = 1e-12;
/// Relative tolerance factor: multiply by the model extent for scale-aware comparisons.
pub const RELATIVE: f64 = 1e-10;

/// Linear tolerance scaled to a model of the given extent (never below [`LINEAR`]).
#[inline]
pub fn linear_for_extent(extent: f64) -> f64 {
    (extent.abs() * RELATIVE).max(LINEAR)
}
