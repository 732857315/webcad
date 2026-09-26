//! Adapter between `acadrust::CadDocument` and `wcad_doc::Document`. Private to this crate.
//!
//! acadrust stores most angles in radians, but a few fields keep the raw file value and differ
//! between its DXF and DWG code paths (verified against acadrust 0.5.5 sources and round-trip
//! tests). [`Flavor`] selects the right convention:
//!
//! | field                          | DXF reader / writer        | DWG            |
//! |--------------------------------|----------------------------|----------------|
//! | TEXT oblique                   | reads rad, **writes raw**  | rad            |
//! | STYLE oblique                  | raw (degrees)              | rad            |
//! | HATCH elliptic edge start/end  | raw (degrees)              | rad            |
//! | BLOCK base point               | **not read, written as 0** | kept           |

pub(crate) mod export;
pub(crate) mod import;

use acadrust::types::{Color as AColor, LineWeight as ALineWeight, Vector2, Vector3};
use wcad_doc::{Color, LineWeight, Units};
use wcad_math::DVec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flavor {
    Dxf,
    Dwg,
}

#[inline]
pub(crate) fn v2(p: Vector3) -> DVec2 {
    DVec2::new(p.x, p.y)
}

#[inline]
pub(crate) fn v2d(p: Vector2) -> DVec2 {
    DVec2::new(p.x, p.y)
}

#[inline]
pub(crate) fn a3(p: DVec2) -> Vector3 {
    Vector3::new(p.x, p.y, 0.0)
}

#[inline]
pub(crate) fn a2(p: DVec2) -> Vector2 {
    Vector2::new(p.x, p.y)
}

pub(crate) fn color_from(c: AColor) -> Color {
    match c {
        AColor::ByLayer | AColor::None => Color::ByLayer,
        AColor::ByBlock | AColor::Index(0) => Color::ByBlock,
        AColor::Index(i) => Color::Aci(i),
        AColor::Rgb { r, g, b } => Color::Rgb(r, g, b),
    }
}

/// Layer colors are never ByLayer/ByBlock.
pub(crate) fn layer_color_from(c: AColor) -> Color {
    match color_from(c) {
        Color::ByLayer | Color::ByBlock => Color::WHITE,
        c => c,
    }
}

pub(crate) fn color_to(c: Color) -> AColor {
    match c {
        Color::ByLayer => AColor::ByLayer,
        Color::ByBlock => AColor::ByBlock,
        Color::Aci(0) => AColor::ByBlock,
        Color::Aci(i) => AColor::Index(i),
        Color::Rgb(r, g, b) => AColor::Rgb { r, g, b },
    }
}

pub(crate) fn lineweight_from(w: ALineWeight) -> LineWeight {
    match w {
        ALineWeight::ByLayer => LineWeight::ByLayer,
        ALineWeight::ByBlock => LineWeight::ByBlock,
        ALineWeight::Default => LineWeight::Default,
        ALineWeight::Value(v) if (0..=211).contains(&v) => LineWeight::Mm100(v as u16),
        ALineWeight::Value(_) => LineWeight::Default,
    }
}

/// Standard DXF lineweights (1/100 mm); other values are snapped to the nearest one on export.
const STANDARD_WEIGHTS: [i16; 24] = [
    0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200,
    211,
];

pub(crate) fn lineweight_to(w: LineWeight) -> ALineWeight {
    match w {
        LineWeight::ByLayer => ALineWeight::ByLayer,
        LineWeight::ByBlock => ALineWeight::ByBlock,
        LineWeight::Default => ALineWeight::Default,
        LineWeight::Mm100(v) => {
            let v = v.min(211) as i16;
            let best = STANDARD_WEIGHTS
                .iter()
                .copied()
                .min_by_key(|s| (s - v).abs())
                .unwrap_or(25);
            ALineWeight::Value(best)
        }
    }
}

/// `$INSUNITS` → document units. Unknown codes map to `Unitless` with a warning by the caller.
pub(crate) fn units_from(code: i16) -> Option<Units> {
    Some(match code {
        0 => Units::Unitless,
        1 => Units::Inch,
        2 => Units::Foot,
        4 => Units::Millimeter,
        5 => Units::Centimeter,
        6 => Units::Meter,
        _ => return None,
    })
}

pub(crate) fn units_to(u: Units) -> i16 {
    match u {
        Units::Unitless => 0,
        Units::Inch => 1,
        Units::Foot => 2,
        Units::Millimeter => 4,
        Units::Centimeter => 5,
        Units::Meter => 6,
    }
}

/// Convert a true angle on an ellipse to its parametric angle (and back).
pub(crate) fn ellipse_angle_to_param(ratio: f64, angle: f64) -> f64 {
    if !(ratio > 0.0) {
        return angle;
    }
    (angle.sin() / ratio).atan2(angle.cos())
}

pub(crate) fn ellipse_param_to_angle(ratio: f64, param: f64) -> f64 {
    (param.sin() * ratio).atan2(param.cos())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_and_weight_mapping() {
        assert_eq!(color_from(color_to(Color::Aci(3))), Color::Aci(3));
        assert_eq!(
            color_from(color_to(Color::Rgb(1, 2, 3))),
            Color::Rgb(1, 2, 3)
        );
        assert_eq!(color_from(color_to(Color::ByBlock)), Color::ByBlock);
        assert_eq!(
            lineweight_from(lineweight_to(LineWeight::Mm100(35))),
            LineWeight::Mm100(35)
        );
        assert_eq!(
            lineweight_from(lineweight_to(LineWeight::Mm100(33))),
            LineWeight::Mm100(35)
        );
        for a in [0.3, 1.2, 2.0, -2.5] {
            let p = ellipse_angle_to_param(0.4, a);
            assert!((wcad_math::normalize_pi(ellipse_param_to_angle(0.4, p) - a)).abs() < 1e-12);
        }
    }
}
