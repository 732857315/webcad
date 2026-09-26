//! Dimension graphics for export (extension/dimension lines, arrowheads, measured text).
//!
//! A simplified, style-driven rendition of AutoCAD's dimension blocks: closed filled arrows, text
//! above the dimension line (ISO style), no tolerances or alternate units.

use std::f64::consts::{FRAC_PI_2, PI};

use wcad_doc::{DimKind, DimStyle, Dimension, HAlign, VAlign};
use wcad_geom2d::Arc2;
use wcad_math::{DVec2, ccw_sweep, normalize_pi, perp};

#[derive(Clone, Debug)]
pub(crate) struct DimGraphics {
    pub lines: Vec<(DVec2, DVec2)>,
    pub arcs: Vec<Arc2>,
    /// Filled arrowhead triangles.
    pub arrows: Vec<[DVec2; 3]>,
    pub text: String,
    pub text_pos: DVec2,
    pub text_rotation: f64,
    pub text_height: f64,
    pub halign: HAlign,
    pub valign: VAlign,
}

fn unit(v: DVec2) -> Option<DVec2> {
    let l = v.length();
    (l > 1e-12 && l.is_finite()).then(|| v / l)
}

fn angle_of(v: DVec2) -> f64 {
    v.y.atan2(v.x)
}

/// Rotate text so it never reads upside down. Returns (angle, flipped).
fn readable(a: f64) -> (f64, bool) {
    let a = normalize_pi(a);
    if a > FRAC_PI_2 + 1e-9 || a <= -FRAC_PI_2 + 1e-9 {
        (normalize_pi(a + PI), true)
    } else {
        (a, false)
    }
}

/// The measured value: a length, or an angle in radians for angular dimensions.
pub(crate) fn measure(kind: &DimKind) -> f64 {
    match kind {
        DimKind::Linear {
            p1, p2, rotation, ..
        } => (*p2 - *p1)
            .dot(DVec2::new(rotation.cos(), rotation.sin()))
            .abs(),
        DimKind::Aligned { p1, p2, .. } => p1.distance(*p2),
        DimKind::Radius { center, point } => center.distance(*point),
        DimKind::Diameter { center, point } => 2.0 * center.distance(*point),
        DimKind::Angular { vertex, p1, p2, .. } => {
            let a1 = angle_of(*p1 - *vertex);
            let a2 = angle_of(*p2 - *vertex);
            let s = ccw_sweep(a1, a2);
            if s >= std::f64::consts::TAU { 0.0 } else { s }
        }
        DimKind::Ordinate {
            origin,
            point,
            x_axis,
            ..
        } => {
            if *x_axis {
                (point.x - origin.x).abs()
            } else {
                (point.y - origin.y).abs()
            }
        }
    }
}

/// Dimension text: measured value with style decimals/prefix/suffix, then the override (`<>` =
/// measured value).
pub(crate) fn text(dim: &Dimension, style: &DimStyle) -> String {
    let v = measure(&dim.kind);
    let num = match dim.kind {
        DimKind::Angular { .. } => format!(
            "{:.*}\u{00B0}",
            style.angle_decimals as usize,
            v.to_degrees()
        ),
        _ => format!("{:.*}", style.decimals as usize, v),
    };
    let lead = match dim.kind {
        DimKind::Radius { .. } => "R",
        DimKind::Diameter { .. } => "\u{00D8}",
        _ => "",
    };
    // DIMPOST-style prefix/suffix describe linear units; angles only get the degree sign.
    let measured = match dim.kind {
        DimKind::Angular { .. } => num,
        _ => format!("{}{lead}{num}{}", style.prefix, style.suffix),
    };
    match &dim.text_override {
        Some(o) if !o.is_empty() => o.replace("<>", &measured),
        _ => measured,
    }
}

fn arrow(tip: DVec2, dir: DVec2, size: f64) -> [DVec2; 3] {
    let back = tip - dir * size;
    let n = perp(dir) * (size / 6.0);
    [tip, back + n, back - n]
}

/// Graphics for a dimension. `None` for degenerate input.
pub(crate) fn build(dim: &Dimension, style: &DimStyle) -> Option<DimGraphics> {
    let s = if style.scale.is_finite() && style.scale > 0.0 {
        style.scale
    } else {
        1.0
    };
    let asz = style.arrow_size * s;
    let th = style.text_height * s;
    let exo = style.ext_offset * s;
    let exe = style.ext_extend * s;
    let gap = style.text_gap * s;
    let mut g = DimGraphics {
        lines: Vec::new(),
        arcs: Vec::new(),
        arrows: Vec::new(),
        text: text(dim, style),
        text_pos: DVec2::ZERO,
        text_rotation: 0.0,
        text_height: th,
        halign: HAlign::Center,
        valign: VAlign::Bottom,
    };
    match dim.kind {
        DimKind::Linear {
            p1,
            p2,
            line_point,
            rotation,
        } => {
            linear(
                &mut g,
                p1,
                p2,
                line_point,
                DVec2::new(rotation.cos(), rotation.sin()),
                asz,
                exo,
                exe,
                gap,
            )?;
        }
        DimKind::Aligned { p1, p2, line_point } => {
            linear(
                &mut g,
                p1,
                p2,
                line_point,
                unit(p2 - p1)?,
                asz,
                exo,
                exe,
                gap,
            )?;
        }
        DimKind::Radius { center, point } => {
            let u = unit(point - center)?;
            g.lines.push((center, point));
            g.arrows.push(arrow(point, u, asz));
            place_along(&mut g, (center + point) * 0.5, angle_of(u), gap);
        }
        DimKind::Diameter { center, point } => {
            let u = unit(point - center)?;
            let a = center * 2.0 - point;
            g.lines.push((a, point));
            g.arrows.push(arrow(point, u, asz));
            g.arrows.push(arrow(a, -u, asz));
            place_along(&mut g, center, angle_of(u), gap);
        }
        DimKind::Angular {
            vertex,
            p1,
            p2,
            arc_point,
        } => {
            let u1 = unit(p1 - vertex)?;
            let u2 = unit(p2 - vertex)?;
            let mut r = arc_point.distance(vertex);
            if !(r > 1e-12) {
                r = p1.distance(vertex).min(p2.distance(vertex));
            }
            let a1 = angle_of(u1);
            let a2 = angle_of(u2);
            let sweep = ccw_sweep(a1, a2);
            g.arcs.push(Arc2::new(vertex, r, a1, a2));
            let t1 = vertex + u1 * r;
            let t2 = vertex + u2 * r;
            g.arrows
                .push(arrow(t1, DVec2::new(a1.sin(), -a1.cos()), asz));
            g.arrows
                .push(arrow(t2, DVec2::new(-a2.sin(), a2.cos()), asz));
            for (p, u) in [(p1, u1), (p2, u2)] {
                let d = p.distance(vertex);
                if d + exo < r {
                    g.lines.push((p + u * exo, vertex + u * (r + exe)));
                }
            }
            let am = a1 + sweep * 0.5;
            let (rot, _) = readable(am - FRAC_PI_2);
            g.text_pos = vertex + DVec2::new(am.cos(), am.sin()) * (r + gap);
            g.text_rotation = rot;
            // Text sits outside the arc: its bottom faces the vertex unless flipped.
            let outward = DVec2::new(am.cos(), am.sin());
            let up = perp(DVec2::new(rot.cos(), rot.sin()));
            g.valign = if up.dot(outward) >= 0.0 {
                VAlign::Bottom
            } else {
                VAlign::Top
            };
        }
        DimKind::Ordinate {
            point, leader_end, ..
        } => {
            let d = leader_end - point;
            let u = unit(d).unwrap_or(DVec2::X);
            g.lines
                .push((point + u * exo.min(d.length() * 0.5), leader_end));
            let (rot, flipped) = readable(angle_of(u));
            g.text_pos = leader_end + u * gap;
            g.text_rotation = rot;
            g.halign = if flipped { HAlign::Right } else { HAlign::Left };
            g.valign = VAlign::Middle;
        }
    }
    if let Some(p) = dim.text_pos {
        g.text_pos = p;
        g.valign = VAlign::Middle;
        g.halign = HAlign::Center;
    }
    Some(g)
}

#[allow(clippy::too_many_arguments)]
fn linear(
    g: &mut DimGraphics,
    p1: DVec2,
    p2: DVec2,
    line_point: DVec2,
    dir: DVec2,
    asz: f64,
    exo: f64,
    exe: f64,
    gap: f64,
) -> Option<()> {
    if !(dir.x.is_finite() && dir.y.is_finite()) {
        return None;
    }
    let n = perp(dir);
    let e1 = p1 + n * (line_point - p1).dot(n);
    let e2 = p2 + n * (line_point - p2).dot(n);
    for (p, e) in [(p1, e1), (p2, e2)] {
        if let Some(u) = unit(e - p) {
            g.lines.push((p + u * exo, e + u * exe));
        }
    }
    g.lines.push((e1, e2));
    if let Some(u) = unit(e2 - e1) {
        g.arrows.push(arrow(e1, -u, asz));
        g.arrows.push(arrow(e2, u, asz));
    }
    place_along(g, (e1 + e2) * 0.5, angle_of(dir), gap);
    Some(())
}

/// Text centered above a line through `mid` at angle `a`.
fn place_along(g: &mut DimGraphics, mid: DVec2, a: f64, gap: f64) {
    let (rot, _) = readable(a);
    let up = perp(DVec2::new(rot.cos(), rot.sin()));
    g.text_pos = mid + up * gap;
    g.text_rotation = rot;
    g.halign = HAlign::Center;
    g.valign = VAlign::Bottom;
}

/// Where the text goes when the user has not moved it (used for DXF text midpoints).
pub(crate) fn default_text_pos(dim: &Dimension, style: &DimStyle) -> DVec2 {
    let auto = Dimension {
        text_pos: None,
        ..dim.clone()
    };
    match build(&auto, style) {
        Some(g) => {
            g.text_pos
                + perp(DVec2::new(g.text_rotation.cos(), g.text_rotation.sin()))
                    * (g.text_height * 0.5)
        }
        None => match dim.kind {
            DimKind::Linear { line_point, .. } | DimKind::Aligned { line_point, .. } => line_point,
            DimKind::Radius { point, .. } | DimKind::Diameter { point, .. } => point,
            DimKind::Angular { arc_point, .. } => arc_point,
            DimKind::Ordinate { leader_end, .. } => leader_end,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::{DimStyleId, TextStyleId};

    #[test]
    fn linear_text_and_lines() {
        let style = DimStyle::standard(TextStyleId(1));
        let dim = Dimension {
            kind: DimKind::Linear {
                p1: DVec2::new(0.0, 0.0),
                p2: DVec2::new(10.0, 3.0),
                line_point: DVec2::new(5.0, 8.0),
                rotation: 0.0,
            },
            style: DimStyleId(1),
            text_override: None,
            text_pos: None,
        };
        let g = build(&dim, &style).expect("graphics");
        assert_eq!(g.text, "10.00");
        assert_eq!(g.lines.len(), 3);
        assert_eq!(g.arrows.len(), 2);
        assert!((g.text_pos.y - (8.0 + style.text_gap)).abs() < 1e-12);
        let o = Dimension {
            text_override: Some("L=<> mm".into()),
            ..dim
        };
        assert_eq!(text(&o, &style), "L=10.00 mm");
        let ang = Dimension {
            kind: DimKind::Angular {
                vertex: DVec2::ZERO,
                p1: DVec2::new(10.0, 0.0),
                p2: DVec2::new(0.0, 10.0),
                arc_point: DVec2::new(5.0, 5.0),
            },
            ..o
        };
        assert!((measure(&ang.kind) - FRAC_PI_2).abs() < 1e-12);
        assert_eq!(
            text(
                &Dimension {
                    text_override: None,
                    ..ang
                },
                &style
            ),
            "90°"
        );
    }
}
