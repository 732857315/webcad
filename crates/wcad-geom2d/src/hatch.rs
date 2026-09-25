//! Hatch patterns: `.pat` parsing, built-in patterns and pattern line generation clipped to
//! boundary loops with the even-odd rule.

use serde::{Deserialize, Serialize};
use wcad_math::{BBox2, DVec2, perp};

use crate::curve::Curve;
use crate::curves::{Curve2, Line2};
use crate::regions::Region;
use crate::{Error, Result};

/// One pattern line family (AutoCAD `.pat` semantics, lengths in pattern units).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PatLine {
    /// Line direction in radians.
    pub angle: f64,
    /// A point the first line passes through (and where its dash pattern starts).
    pub origin: DVec2,
    /// Offset between successive lines in the line's own frame: `x` along the line (dash
    /// stagger), `y` perpendicular (spacing).
    pub delta: DVec2,
    /// Dash pattern: positive = dash, negative = gap, 0 = dot. Empty = continuous.
    #[serde(default)]
    pub dashes: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HatchPattern {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub lines: Vec<PatLine>,
    /// Solid fill (no lines; callers tessellate the region instead).
    #[serde(default)]
    pub solid: bool,
}

/// Families with more lines than this over the boundary are skipped (spacing too dense).
pub const MAX_LINES_PER_FAMILY: usize = 20_000;
/// Total output cap for [`hatch_lines`].
pub const MAX_SEGMENTS: usize = 500_000;

/// Parse the text of a `.pat` file (`*NAME, description` headers followed by
/// `angle, x-origin, y-origin, delta-x, delta-y [, dash...]` lines; `;` starts a comment).
pub fn parse_pat(text: &str) -> Result<Vec<HatchPattern>> {
    let mut out: Vec<HatchPattern> = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('*') {
            let mut it = rest.splitn(2, ',');
            let name = it.next().unwrap_or("").trim().to_string();
            if name.is_empty() {
                return Err(Error::Invalid(format!(
                    "pat line {}: empty pattern name",
                    lineno + 1
                )));
            }
            let description = it.next().unwrap_or("").trim().to_string();
            let solid = name.eq_ignore_ascii_case("SOLID");
            out.push(HatchPattern {
                name,
                description,
                lines: Vec::new(),
                solid,
            });
            continue;
        }
        let Some(pat) = out.last_mut() else {
            return Err(Error::Invalid(format!(
                "pat line {}: pattern line before any *NAME header",
                lineno + 1
            )));
        };
        let nums: std::result::Result<Vec<f64>, _> = line
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<f64>())
            .collect();
        let nums = nums.map_err(|e| Error::Invalid(format!("pat line {}: {e}", lineno + 1)))?;
        if nums.len() < 5 || nums.iter().any(|v| !v.is_finite()) {
            return Err(Error::Invalid(format!(
                "pat line {}: expected at least 5 numbers",
                lineno + 1
            )));
        }
        if pat.solid {
            continue;
        }
        pat.lines.push(PatLine {
            angle: nums[0].to_radians(),
            origin: DVec2::new(nums[1], nums[2]),
            delta: DVec2::new(nums[3], nums[4]),
            dashes: nums[5..].to_vec(),
        });
    }
    Ok(out)
}

/// Built-in patterns (acad.pat definitions, inch units).
const BUILTIN_PAT: &str = "
*SOLID, Solid fill
*ANSI31, ANSI Iron, Brick, Stone masonry
45, 0,0, 0,.125
*ANSI32, ANSI Steel
45, 0,0, 0,.375
45, .176776695,0, 0,.375
*ANSI33, ANSI Bronze, Brass, Copper
45, 0,0, 0,.25
45, .176776695,0, 0,.25, .125,-.0625
*ANSI37, ANSI Lead, Zinc, Magnesium, Sound/Heat/Elec Insulation
45, 0,0, 0,.125
135, 0,0, 0,.125
*ANSI38, ANSI Aluminum
45, 0,0, 0,.125
135, 0,0, .25,.125, .125,-.375
*NET, Horizontal / vertical grid
0, 0,0, 0,.125
90, 0,0, 0,.125
*NET3, Network pattern 0-60-120
0, 0,0, 0,.125
60, 0,0, 0,.125
120, 0,0, 0,.125
*DOTS, A series of dots
0, 0,0, .03125,.0625, 0,-.0625
*BRICK, Brick or masonry-type surface
0, 0,0, 0,.25
90, 0,0, 0,.5, .25,-.25
90, .25,0, 0,.5, -.25,.25
*LINE, Parallel horizontal lines
0, 0,0, 0,.125
*CROSS, A series of crosses
0, 0,0, .25,.25, .125,-.375
90, .0625,-.0625, .25,.25, .125,-.375
*HONEY, Honeycomb pattern
0, 0,0, .1875,.108253175, .125,-.25
120, 0,0, .1875,.108253175, .125,-.25
60, .125,0, .1875,.108253175, .125,-.25
*SQUARE, Small aligned squares
0, 0,0, 0,.125, .125,-.125
90, 0,0, 0,.125, .125,-.125
*DASH, Dashed lines
0, 0,0, .125,.125, .125,-.125
";

/// Names of the built-in patterns.
pub fn builtin_names() -> Vec<String> {
    parse_pat(BUILTIN_PAT)
        .map(|v| v.into_iter().map(|p| p.name).collect())
        .unwrap_or_default()
}

/// Built-in pattern in inch units (acad.pat), case-insensitive name.
pub fn builtin_imperial(name: &str) -> Option<HatchPattern> {
    parse_pat(BUILTIN_PAT)
        .ok()?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Built-in pattern in millimetres (acadiso.pat convention: the inch definitions × 25.4).
pub fn builtin(name: &str) -> Option<HatchPattern> {
    builtin_imperial(name).map(|p| p.scaled(25.4))
}

impl HatchPattern {
    /// Copy with every length multiplied by `s`.
    pub fn scaled(&self, s: f64) -> HatchPattern {
        HatchPattern {
            lines: self
                .lines
                .iter()
                .map(|l| PatLine {
                    angle: l.angle,
                    origin: l.origin * s,
                    delta: l.delta * s,
                    dashes: l.dashes.iter().map(|d| d * s).collect(),
                })
                .collect(),
            ..self.clone()
        }
    }
}

/// Pattern lines for a region.
pub fn hatch_region(region: &Region, pattern: &HatchPattern, scale: f64, angle: f64) -> Vec<Line2> {
    let loops: Vec<&[Curve2]> = region.loops().map(|l| l.curves.as_slice()).collect();
    hatch_lines(&loops, pattern, scale, angle)
}

/// Pattern lines clipped to `loops` (even-odd: nested loops alternate inside/outside), with the
/// pattern scaled by `scale` and rotated by `angle` around the origin. Dots are returned as
/// zero-length lines (`a == b`). Solid patterns return no lines.
pub fn hatch_lines<L: AsRef<[Curve2]>>(
    loops: &[L],
    pattern: &HatchPattern,
    scale: f64,
    angle: f64,
) -> Vec<Line2> {
    hatch_lines_with_origin(loops, pattern, scale, angle, DVec2::ZERO)
}

/// [`hatch_lines`] with an explicit pattern origin.
pub fn hatch_lines_with_origin<L: AsRef<[Curve2]>>(
    loops: &[L],
    pattern: &HatchPattern,
    scale: f64,
    angle: f64,
    origin: DVec2,
) -> Vec<Line2> {
    if pattern.solid || !(scale > 0.0) || !scale.is_finite() || !angle.is_finite() {
        return Vec::new();
    }
    let polys = flatten_loops(loops);
    if polys.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for pl in &pattern.lines {
        family(&polys, pl, scale, angle, origin, &mut out);
        if out.len() >= MAX_SEGMENTS {
            out.truncate(MAX_SEGMENTS);
            break;
        }
    }
    out
}

/// `true` when some family of `pattern` would exceed [`MAX_LINES_PER_FAMILY`] over the loops.
pub fn hatch_too_dense<L: AsRef<[Curve2]>>(
    loops: &[L],
    pattern: &HatchPattern,
    scale: f64,
) -> bool {
    let bb = loops
        .iter()
        .flat_map(|l| l.as_ref().iter())
        .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()));
    let ext = bb.size().length();
    pattern.lines.iter().any(|l| {
        let sp = (l.delta.y * scale).abs();
        sp <= 0.0 || ext / sp > MAX_LINES_PER_FAMILY as f64
    })
}

fn flatten_loops<L: AsRef<[Curve2]>>(loops: &[L]) -> Vec<Vec<DVec2>> {
    let bb = loops
        .iter()
        .flat_map(|l| l.as_ref().iter())
        .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()));
    let ext = bb.size().length();
    if !(ext > 0.0) || !ext.is_finite() {
        return Vec::new();
    }
    let tol = ext * 1e-5;
    let mut out = Vec::new();
    for l in loops {
        let mut pts: Vec<DVec2> = Vec::new();
        for c in l.as_ref() {
            let f = c.flatten(tol);
            let skip = usize::from(
                pts.last()
                    .zip(f.first())
                    .is_some_and(|(a, b)| a.distance(*b) <= tol),
            );
            pts.extend(f.into_iter().skip(skip));
        }
        if pts.len() > 1
            && pts
                .first()
                .zip(pts.last())
                .is_some_and(|(a, b)| a.distance(*b) <= tol)
        {
            pts.pop();
        }
        if pts.len() >= 3 {
            out.push(pts);
        }
    }
    out
}

fn family(
    polys: &[Vec<DVec2>],
    pl: &PatLine,
    scale: f64,
    angle: f64,
    origin: DVec2,
    out: &mut Vec<Line2>,
) {
    let th = pl.angle + angle;
    let d = DVec2::new(th.cos(), th.sin());
    let n = perp(d);
    let rot = DVec2::new(angle.cos(), angle.sin());
    let o = origin + rot.rotate(pl.origin * scale);
    let (mut dx, mut dy) = (pl.delta.x * scale, pl.delta.y * scale);
    if dy < 0.0 {
        dx = -dx;
        dy = -dy;
    }
    if !(dy > 0.0) || !dy.is_finite() {
        return;
    }
    // local frame: u along d, v along n
    let local: Vec<Vec<(f64, f64)>> = polys
        .iter()
        .map(|p| {
            p.iter()
                .map(|q| ((*q - o).dot(d), (*q - o).dot(n)))
                .collect()
        })
        .collect();
    let (mut vmin, mut vmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in &local {
        for &(_, v) in p {
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
    }
    let k0 = (vmin / dy).ceil();
    let k1 = (vmax / dy).floor();
    if !(k1 >= k0) || k1 - k0 > MAX_LINES_PER_FAMILY as f64 {
        return;
    }
    let (k0, k1) = (k0 as i64, k1 as i64);
    let nk = (k1 - k0 + 1) as usize;
    // crossings per scanline
    let mut rows: Vec<Vec<f64>> = vec![Vec::new(); nk];
    for p in &local {
        let m = p.len();
        for i in 0..m {
            let (u1, v1) = p[i];
            let (u2, v2) = p[(i + 1) % m];
            if v1 == v2 {
                continue;
            }
            let (lo, hi) = if v1 < v2 { (v1, v2) } else { (v2, v1) };
            let ka = ((lo / dy).ceil() as i64).max(k0);
            let kb = ((hi / dy).ceil() as i64 - 1).min(k1);
            for k in ka..=kb {
                let v = k as f64 * dy;
                if (v1 > v) != (v2 > v) {
                    let u = u1 + (v - v1) / (v2 - v1) * (u2 - u1);
                    rows[(k - k0) as usize].push(u);
                }
            }
        }
    }
    let dashes: Vec<f64> = pl.dashes.iter().map(|x| x * scale).collect();
    let period: f64 = dashes.iter().map(|x| x.abs()).sum();
    let continuous = dashes.is_empty() || !(period > 0.0);
    for (ri, xs) in rows.iter_mut().enumerate() {
        if xs.len() < 2 {
            continue;
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        let k = (k0 + ri as i64) as f64;
        let v = k * dy;
        let u0 = k * dx; // dash phase origin of this line
        let base = o + n * v;
        for &[ua, ub] in xs.as_chunks::<2>().0 {
            if ub <= ua {
                continue;
            }
            if continuous {
                out.push(Line2::new(base + d * ua, base + d * ub));
                continue;
            }
            if (ub - ua) / period > 1e6 {
                continue;
            }
            // walk the dash pattern from ua
            let mut pos = ua - (ua - u0).rem_euclid(period);
            'walk: loop {
                for &e in &dashes {
                    let len = e.abs();
                    let (s, t) = (pos, pos + len);
                    if s > ub {
                        break 'walk;
                    }
                    if e > 0.0 {
                        let (cs, ct) = (s.max(ua), t.min(ub));
                        if ct > cs {
                            out.push(Line2::new(base + d * cs, base + d * ct));
                        }
                    } else if e == 0.0 && s >= ua && s <= ub {
                        let q = base + d * s;
                        out.push(Line2::new(q, q));
                    }
                    pos = t;
                }
                if out.len() >= MAX_SEGMENTS {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curves::Circle2;

    fn square(s: f64) -> Vec<Curve2> {
        let p = [
            DVec2::ZERO,
            DVec2::new(s, 0.0),
            DVec2::new(s, s),
            DVec2::new(0.0, s),
        ];
        (0..4)
            .map(|i| Curve2::Line(Line2::new(p[i], p[(i + 1) % 4])))
            .collect()
    }

    #[test]
    fn parse_builtins() {
        let names = builtin_names();
        for n in [
            "SOLID", "ANSI31", "ANSI32", "ANSI37", "ANSI38", "NET", "DOTS", "BRICK", "LINE",
            "CROSS", "HONEY",
        ] {
            assert!(names.iter().any(|x| x == n), "{n}");
        }
        let a = builtin("ansi31").unwrap();
        assert_eq!(a.lines.len(), 1);
        assert!((a.lines[0].delta.y - 3.175).abs() < 1e-12);
        assert!(builtin("SOLID").unwrap().solid);
        assert!(parse_pat("45, 0,0, 0,1").is_err());
        assert!(parse_pat("*X\n45, 0,0").is_err());
        assert!(parse_pat("*X, y\n45, 0,0, 0,abc").is_err());
        let p =
            parse_pat("; comment\n*MINE, test ; trailing\n0, 0,0, 0,1, 1,-0.5, 0,-0.5\n").unwrap();
        assert_eq!(p[0].name, "MINE");
        assert_eq!(p[0].lines[0].dashes, vec![1.0, -0.5, 0.0, -0.5]);
    }

    #[test]
    fn line_pattern_in_square() {
        let pat = parse_pat("*L\n0, 0,0, 0,1").unwrap().remove(0);
        let lines = hatch_lines(&[square(10.0)], &pat, 1.0, 0.0);
        // y = 1..9 strictly inside, y = 0 and y = 10 lie on the boundary (half-open rule keeps y=0)
        assert!(lines.len() == 10 || lines.len() == 9, "{}", lines.len());
        for l in &lines {
            assert!((l.length() - 10.0).abs() < 1e-9);
        }
        // 45° ANSI31 with scale: total hatched length ≈ area / spacing
        let a = builtin_imperial("ANSI31").unwrap();
        let lines = hatch_lines(&[square(10.0)], &a, 1.0, 0.0);
        let total: f64 = lines.iter().map(|l| l.length()).sum();
        assert!((total - 100.0 / 0.125).abs() < 0.02 * 800.0, "{total}");
        // rotation by 45° turns ANSI31 into vertical lines
        let lines = hatch_lines(&[square(10.0)], &a, 1.0, std::f64::consts::FRAC_PI_4);
        assert!(lines.iter().all(|l| (l.a.x - l.b.x).abs() < 1e-9));
    }

    #[test]
    fn holes_and_dashes() {
        let pat = parse_pat("*L\n0, 0,0.5, 0,1").unwrap().remove(0);
        let hole = vec![Curve2::Circle(Circle2::new(DVec2::new(5.0, 5.0), 2.0))];
        let lines = hatch_lines(&[square(10.0), hole], &pat, 1.0, 0.0);
        // lines through the hole are split in two
        let through: Vec<&Line2> = lines
            .iter()
            .filter(|l| (l.a.y - 5.5).abs() < 1e-9)
            .collect();
        assert_eq!(through.len(), 2);
        for l in &lines {
            let m = l.midpoint();
            assert!(m.distance(DVec2::new(5.0, 5.0)) > 2.0 - 1e-6);
        }
        // dashes: 1 on, 1 off → half the length
        let dpat = parse_pat("*D\n0, 0,0.5, 0,1, 1,-1").unwrap().remove(0);
        let lines = hatch_lines(&[square(10.0)], &dpat, 1.0, 0.0);
        let total: f64 = lines.iter().map(|l| l.length()).sum();
        assert!((total - 50.0).abs() < 1e-9, "{total}");
        // dots
        let dots = builtin("DOTS").unwrap();
        let lines = hatch_lines(&[square(20.0)], &dots, 1.0, 0.0);
        assert!(!lines.is_empty() && lines.iter().all(|l| l.a == l.b));
        // too dense
        assert!(hatch_lines(&[square(10.0)], &pat, 1e-6, 0.0).is_empty());
        assert!(hatch_too_dense(&[square(10.0)], &pat, 1e-6));
        // every built-in works in a circle
        for n in builtin_names() {
            let p = builtin(&n).unwrap();
            let c = vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 50.0))];
            let l = hatch_lines(&[c], &p, 1.0, 0.3);
            assert_eq!(l.is_empty(), p.solid, "{n}");
        }
    }
}
