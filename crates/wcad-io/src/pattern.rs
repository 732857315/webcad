//! Built-in hatch pattern definitions (acadiso.pat subset, millimetre based) and pattern-line
//! generation for SVG/PDF export.
//!
//! `wcad_geom2d::hatch` is being written concurrently; once it provides pattern tables and
//! `hatch_lines`, this module can delegate to it. The line format matches `.pat` files:
//! angle (degrees), base point, offset (along, perpendicular) in the line's own frame, dashes.

use wcad_math::{DVec2, perp};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PatLine {
    pub angle_deg: f64,
    pub base: DVec2,
    /// `.pat` offset: x along the line direction, y perpendicular to it.
    pub offset: DVec2,
    pub dashes: Vec<f64>,
}

fn l(angle: f64, bx: f64, by: f64, dx: f64, dy: f64, dashes: &[f64]) -> PatLine {
    PatLine { angle_deg: angle, base: DVec2::new(bx, by), offset: DVec2::new(dx, dy), dashes: dashes.to_vec() }
}

/// Definition lines for a named pattern (case-insensitive). `None` for SOLID and unknown names.
pub(crate) fn builtin(name: &str) -> Option<Vec<PatLine>> {
    let n = name.to_ascii_uppercase();
    Some(match n.as_str() {
        "ANSI31" => vec![l(45.0, 0.0, 0.0, 0.0, 3.175, &[])],
        "ANSI32" => vec![l(45.0, 0.0, 0.0, 0.0, 9.525, &[]), l(45.0, 4.490128, 0.0, 0.0, 9.525, &[])],
        "ANSI33" => vec![l(45.0, 0.0, 0.0, 0.0, 6.35, &[]), l(45.0, 4.490128, 0.0, 0.0, 6.35, &[3.175, -1.5875])],
        "ANSI34" => vec![
            l(45.0, 0.0, 0.0, 0.0, 19.05, &[]),
            l(45.0, 4.490128, 0.0, 0.0, 19.05, &[]),
            l(45.0, 8.980256, 0.0, 0.0, 19.05, &[]),
            l(45.0, 13.470384, 0.0, 0.0, 19.05, &[]),
        ],
        "ANSI35" => {
            vec![l(45.0, 0.0, 0.0, 0.0, 6.35, &[]), l(45.0, 4.490128, 0.0, 0.0, 6.35, &[7.9375, -1.5875, 0.0, -1.5875])]
        }
        "ANSI36" => vec![l(45.0, 0.0, 0.0, 5.55625, 3.175, &[7.9375, -1.5875, 0.0, -1.5875])],
        "ANSI37" => vec![l(45.0, 0.0, 0.0, 0.0, 3.175, &[]), l(135.0, 0.0, 0.0, 0.0, 3.175, &[])],
        "ANSI38" => vec![l(45.0, 0.0, 0.0, 0.0, 3.175, &[]), l(135.0, 0.0, 0.0, 6.35, 3.175, &[7.9375, -4.7625])],
        "LINE" => vec![l(0.0, 0.0, 0.0, 0.0, 3.175, &[])],
        "NET" => vec![l(0.0, 0.0, 0.0, 0.0, 3.175, &[]), l(90.0, 0.0, 0.0, 0.0, 3.175, &[])],
        "NET3" => vec![
            l(0.0, 0.0, 0.0, 0.0, 3.175, &[]),
            l(60.0, 0.0, 0.0, 0.0, 3.175, &[]),
            l(120.0, 0.0, 0.0, 0.0, 3.175, &[]),
        ],
        "DOTS" => vec![l(0.0, 0.0, 0.0, 0.79375, 1.5875, &[0.0, -1.5875])],
        "SQUARE" => {
            vec![l(0.0, 0.0, 0.0, 0.0, 3.175, &[3.175, -3.175]), l(90.0, 0.0, 0.0, 0.0, 3.175, &[3.175, -3.175])]
        }
        "CROSS" => {
            vec![l(0.0, 0.0, 0.0, 6.35, 6.35, &[3.175, -9.525]), l(90.0, 1.5875, -1.5875, 6.35, 6.35, &[3.175, -9.525])]
        }
        "BRICK" => vec![
            l(0.0, 0.0, 0.0, 0.0, 6.35, &[]),
            l(90.0, 0.0, 0.0, 6.35, 6.35, &[6.35, -6.35]),
            l(90.0, 6.35, 0.0, 6.35, 6.35, &[-6.35, 6.35]),
        ],
        "GRATE" => vec![l(0.0, 0.0, 0.0, 0.0, 0.79375, &[]), l(90.0, 0.0, 0.0, 0.0, 3.175, &[])],
        "EARTH" => vec![
            l(0.0, 0.0, 0.0, 6.35, 6.35, &[6.35, -6.35]),
            l(0.0, 0.0, 2.38125, 6.35, 6.35, &[6.35, -6.35]),
            l(0.0, 0.0, 4.7625, 6.35, 6.35, &[6.35, -6.35]),
            l(90.0, 0.79375, 5.55625, 6.35, 6.35, &[6.35, -6.35]),
            l(90.0, 3.175, 5.55625, 6.35, 6.35, &[6.35, -6.35]),
            l(90.0, 5.55625, 5.55625, 6.35, 6.35, &[6.35, -6.35]),
        ],
        _ => return None,
    })
}

/// Pattern lines for `name`, falling back to ANSI31 for unknown names (never for SOLID).
pub(crate) fn lines_or_default(name: &str) -> Vec<PatLine> {
    builtin(name).unwrap_or_else(|| builtin("ANSI31").unwrap_or_default())
}

/// A pattern line family in drawing coordinates, after applying hatch angle and scale.
#[derive(Clone, Debug)]
pub(crate) struct Family {
    /// Unit direction of the lines.
    pub dir: DVec2,
    pub base: DVec2,
    /// Offset between successive lines, in drawing coordinates.
    pub offset: DVec2,
    pub dashes: Vec<f64>,
}

/// Families for a pattern at a hatch angle (radians) and scale.
pub(crate) fn families(lines: &[PatLine], angle: f64, scale: f64) -> Vec<Family> {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let rot = |v: DVec2, a: f64| DVec2::new(v.x * a.cos() - v.y * a.sin(), v.x * a.sin() + v.y * a.cos());
    lines
        .iter()
        .map(|pl| {
            let a = pl.angle_deg.to_radians() + angle;
            let dir = DVec2::new(a.cos(), a.sin());
            let offset = rot(pl.offset, a) * scale;
            Family {
                dir,
                base: rot(pl.base, angle) * scale,
                offset,
                dashes: pl.dashes.iter().map(|d| d * scale).collect(),
            }
        })
        .collect()
}

/// Maximum number of generated segments per hatch (protects against absurd scales).
const MAX_SEGMENTS: usize = 200_000;

/// Clip every family against the polygons (even-odd) and return the resulting segments.
pub(crate) fn hatch_segments(polys: &[Vec<DVec2>], fams: &[Family]) -> Vec<(DVec2, DVec2)> {
    let mut out = Vec::new();
    for f in fams {
        let n = perp(f.dir);
        let spacing = f.offset.dot(n);
        if !spacing.is_finite() || spacing.abs() < 1e-9 {
            continue;
        }
        let shift = f.offset.dot(f.dir);
        // Polygon edges in the family frame: x along dir, y along n, origin at base.
        let to_frame = |p: DVec2| DVec2::new((p - f.base).dot(f.dir), (p - f.base).dot(n));
        let mut edges: Vec<(DVec2, DVec2)> = Vec::new();
        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        for poly in polys {
            for w in poly.windows(2) {
                let a = to_frame(w[0]);
                let b = to_frame(w[1]);
                ymin = ymin.min(a.y).min(b.y);
                ymax = ymax.max(a.y).max(b.y);
                edges.push((a, b));
            }
            // Close the ring if the caller did not.
            if let (Some(&first), Some(&last)) = (poly.first(), poly.last())
                && first.distance_squared(last) > 0.0
            {
                edges.push((to_frame(last), to_frame(first)));
            }
        }
        if edges.is_empty() || !ymin.is_finite() {
            continue;
        }
        let (k0, k1) = {
            let a = (ymin / spacing).floor();
            let b = (ymax / spacing).ceil();
            (a.min(b), a.max(b))
        };
        // Bound the work: lines × edges.
        if (k1 - k0) > MAX_SEGMENTS as f64 || (k1 - k0) * edges.len() as f64 > 50_000_000.0 {
            continue;
        }
        let mut xs: Vec<f64> = Vec::new();
        let mut k = k0;
        while k <= k1 {
            let y = k * spacing;
            xs.clear();
            for &(a, b) in &edges {
                if (a.y <= y && y < b.y) || (b.y <= y && y < a.y) {
                    xs.push(a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
            xs.sort_by(f64::total_cmp);
            let phase = k * shift;
            for pair in xs.as_chunks::<2>().0 {
                let to_world = |x: f64| f.base + f.dir * x + n * y;
                if f.dashes.is_empty() {
                    out.push((to_world(pair[0]), to_world(pair[1])));
                } else {
                    dash_segment(pair[0], pair[1], phase, &f.dashes, &mut |a, b| out.push((to_world(a), to_world(b))));
                }
                if out.len() > MAX_SEGMENTS {
                    return out;
                }
            }
            k += 1.0;
        }
    }
    out
}

/// Split `[x0, x1]` by a dash pattern whose period starts at `phase`.
fn dash_segment(x0: f64, x1: f64, phase: f64, dashes: &[f64], emit: &mut dyn FnMut(f64, f64)) {
    let period: f64 = dashes.iter().map(|d| d.abs()).sum();
    if !(period > 1e-12) {
        emit(x0, x1);
        return;
    }
    let dot = period * 1e-3;
    // Start of the period containing x0.
    let mut t = phase + ((x0 - phase) / period).floor() * period;
    let mut guard = 0usize;
    while t < x1 && guard < 100_000 {
        for &d in dashes {
            let len = d.abs();
            if d >= 0.0 {
                let (a, b) = if d == 0.0 { (t, t + dot) } else { (t, t + len) };
                let (a, b) = (a.max(x0), b.min(x1));
                if b > a {
                    emit(a, b);
                }
            }
            t += len;
            if t >= x1 {
                break;
            }
        }
        guard += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi31_in_square() {
        let sq = vec![vec![
            DVec2::new(0.0, 0.0),
            DVec2::new(100.0, 0.0),
            DVec2::new(100.0, 100.0),
            DVec2::new(0.0, 100.0),
            DVec2::new(0.0, 0.0),
        ]];
        let fams = families(&lines_or_default("ANSI31"), 0.0, 1.0);
        let segs = hatch_segments(&sq, &fams);
        // Perpendicular extent 141.4 / spacing 3.175 ≈ 44.5 lines.
        assert!(segs.len() >= 43 && segs.len() <= 46, "{}", segs.len());
        for (a, b) in &segs {
            let d = *b - *a;
            assert!((d.x - d.y).abs() < 1e-9, "45° lines");
            for p in [a, b] {
                assert!(p.x > -1e-9 && p.x < 100.0 + 1e-9 && p.y > -1e-9 && p.y < 100.0 + 1e-9);
            }
        }
        // Dashed family produces more, shorter pieces.
        let fams = families(&lines_or_default("DOTS"), 0.0, 2.0);
        assert!(hatch_segments(&sq, &fams).len() > 500);
    }
}
