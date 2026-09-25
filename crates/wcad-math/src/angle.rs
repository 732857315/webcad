//! Angle helpers. Angles are radians; positive = counter-clockwise.

use std::f64::consts::TAU;

/// Normalize an angle into `[0, 2π)`.
#[inline]
pub fn normalize_0_2pi(a: f64) -> f64 {
    let r = a.rem_euclid(TAU);
    if r >= TAU { 0.0 } else { r }
}

/// Normalize an angle into `(-π, π]`.
#[inline]
pub fn normalize_pi(a: f64) -> f64 {
    let r = normalize_0_2pi(a);
    if r > std::f64::consts::PI { r - TAU } else { r }
}

/// Counter-clockwise sweep from `start` to `end`, in `(0, 2π]` (a zero sweep is a full turn).
#[inline]
pub fn ccw_sweep(start: f64, end: f64) -> f64 {
    let s = normalize_0_2pi(end - start);
    if s <= 0.0 { TAU } else { s }
}

/// `true` if angle `a` lies on the counter-clockwise arc from `start` to `end` (inclusive, with `eps`).
#[inline]
pub fn ccw_between(start: f64, end: f64, a: f64, eps: f64) -> bool {
    let sweep = ccw_sweep(start, end);
    let d = normalize_0_2pi(a - start);
    d <= sweep + eps || d >= TAU - eps
}

#[inline]
pub fn deg(rad: f64) -> f64 {
    rad.to_degrees()
}

#[inline]
pub fn rad(deg: f64) -> f64 {
    deg.to_radians()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{FRAC_PI_2, PI};

    #[test]
    fn sweep_and_between() {
        assert!((ccw_sweep(0.0, FRAC_PI_2) - FRAC_PI_2).abs() < 1e-15);
        assert!((ccw_sweep(FRAC_PI_2, 0.0) - 1.5 * PI).abs() < 1e-12);
        assert!((ccw_sweep(1.0, 1.0) - TAU).abs() < 1e-15);
        assert!(ccw_between(1.5 * PI, 0.5 * PI, 0.0, 1e-12));
        assert!(!ccw_between(1.5 * PI, 0.5 * PI, PI, 1e-12));
        assert!((normalize_pi(1.5 * PI) + 0.5 * PI).abs() < 1e-12);
    }
}
