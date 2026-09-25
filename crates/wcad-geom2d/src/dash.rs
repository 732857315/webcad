//! Linetype dashing: split a polyline into dash pieces following a pattern, with the pattern
//! continuing around corners.

use wcad_math::DVec2;

use crate::curve::Curve;
use crate::curves::Curve2;

/// Above this many pattern repetitions the line is drawn continuous (AutoCAD behaves the same).
pub const MAX_REPEATS: f64 = 100_000.0;

/// Dash `points` with `pattern` (positive = dash, negative = gap, 0 = dot; drawing units before
/// `scale`). `phase` shifts the pattern start along the line. Dots are returned as two identical
/// points. An empty or zero-length pattern returns the whole polyline.
pub fn dash_polyline(points: &[DVec2], pattern: &[f64], scale: f64, phase: f64) -> Vec<Vec<DVec2>> {
    let pts: Vec<DVec2> = points.iter().copied().filter(|p| p.is_finite()).collect();
    if pts.len() < 2 {
        return Vec::new();
    }
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let elems: Vec<f64> = pattern
        .iter()
        .filter(|x| x.is_finite())
        .map(|x| x * scale)
        .collect();
    let period: f64 = elems.iter().map(|x| x.abs()).sum();
    let total: f64 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
    if elems.is_empty() || !(period > 0.0) || total / period > MAX_REPEATS {
        return vec![pts];
    }
    let n = elems.len();
    let len = |i: usize| elems[i].abs();
    let is_dash = |i: usize| elems[i] > 0.0;
    let is_dot = |i: usize| elems[i] == 0.0;
    // locate the phase
    let mut pos = if phase.is_finite() {
        phase.rem_euclid(period)
    } else {
        0.0
    };
    let mut i = 0usize;
    let mut guard = 0usize;
    while pos > 0.0 && guard < 4 * n {
        if pos < len(i) {
            break;
        }
        pos -= len(i);
        i = (i + 1) % n;
        guard += 1;
    }
    let mut rem = (len(i) - pos).max(0.0);
    let mut out: Vec<Vec<DVec2>> = Vec::new();
    let mut cur: Vec<DVec2> = Vec::new();
    let start = pts[0];
    // dots at the very start
    if rem == 0.0 {
        while is_dot(i) {
            out.push(vec![start, start]);
            i = (i + 1) % n;
        }
        rem = len(i);
    }
    if is_dash(i) {
        cur.push(start);
    }
    for w in pts.windows(2) {
        let (p0, p1) = (w[0], w[1]);
        let seg = p0.distance(p1);
        if seg <= 0.0 {
            continue;
        }
        let dir = (p1 - p0) / seg;
        let mut t = 0.0;
        let mut guard = 0usize;
        while seg - t > rem {
            guard += 1;
            if guard > 10_000_000 {
                break;
            }
            t += rem;
            let q = p0 + dir * t;
            if is_dash(i) {
                cur.push(q);
                out.push(std::mem::take(&mut cur));
            }
            i = (i + 1) % n;
            while is_dot(i) {
                out.push(vec![q, q]);
                i = (i + 1) % n;
            }
            rem = len(i);
            if is_dash(i) {
                cur.push(q);
            }
        }
        rem -= seg - t;
        if is_dash(i) {
            cur.push(p1);
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out.retain(|piece| piece.len() >= 2);
    out
}

/// Flatten `curve` with chord tolerance `tol` and dash it (see [`dash_polyline`]).
pub fn dash_curve(
    curve: &Curve2,
    pattern: &[f64],
    scale: f64,
    phase: f64,
    tol: f64,
) -> Vec<Vec<DVec2>> {
    dash_polyline(&curve.flatten(tol), pattern, scale, phase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plen(p: &[DVec2]) -> f64 {
        p.windows(2).map(|w| w[0].distance(w[1])).sum()
    }

    #[test]
    fn simple_dashes() {
        let pts = [DVec2::ZERO, DVec2::new(10.0, 0.0)];
        let d = dash_polyline(&pts, &[1.0, -1.0], 1.0, 0.0);
        assert_eq!(d.len(), 5);
        for (k, piece) in d.iter().enumerate() {
            assert!((piece[0].x - 2.0 * k as f64).abs() < 1e-12);
            assert!((plen(piece) - 1.0).abs() < 1e-12);
        }
        // continuous
        assert_eq!(dash_polyline(&pts, &[], 1.0, 0.0), vec![pts.to_vec()]);
        // scale
        let d = dash_polyline(&pts, &[1.0, -1.0], 2.5, 0.0);
        assert_eq!(d.len(), 2);
        // phase shifts the start into the gap
        let d = dash_polyline(&pts, &[1.0, -1.0], 1.0, 1.5);
        assert!((d[0][0].x - 0.5).abs() < 1e-12);
    }

    #[test]
    fn continuity_around_corners_and_dots() {
        let pts = [DVec2::ZERO, DVec2::new(1.5, 0.0), DVec2::new(1.5, 5.0)];
        let d = dash_polyline(&pts, &[2.0, -1.0], 1.0, 0.0);
        // first dash turns the corner: 1.5 along x, 0.5 along y
        assert_eq!(d[0].len(), 3);
        assert!((plen(&d[0]) - 2.0).abs() < 1e-12);
        let total: f64 = d.iter().map(|p| plen(p)).sum();
        // 6.5 total: dashes at [0,2], [3,5], [6,6.5]
        assert!((total - 4.5).abs() < 1e-12, "{total}");
        // dash-dot
        let dd = dash_polyline(
            &[DVec2::ZERO, DVec2::new(10.0, 0.0)],
            &[2.0, -1.0, 0.0, -1.0],
            1.0,
            0.0,
        );
        let dots = dd.iter().filter(|p| p[0] == p[1] && p.len() == 2).count();
        assert_eq!(dots, 2); // at x = 3 and 7
        // property: dash total ≈ total * dash_fraction for long lines
        let long = [
            DVec2::ZERO,
            DVec2::new(1000.0, 0.0),
            DVec2::new(1000.0, 1000.0),
        ];
        let d = dash_polyline(&long, &[3.0, -1.0], 1.0, 0.0);
        let s: f64 = d.iter().map(|p| plen(p)).sum();
        assert!((s - 1500.0).abs() < 3.0, "{s}");
        // too many repeats → continuous
        assert_eq!(dash_polyline(&long, &[1e-6, -1e-6], 1.0, 0.0).len(), 1);
    }
}
