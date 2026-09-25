//! Affine transforms of drawing entities (MOVE, grips; also used by COPY/ROTATE/SCALE/MIRROR).

use wcad_doc::{DimKind, Entity, EntityKind, HatchLoop};
use wcad_geom2d::Curve;
use wcad_math::{DAffine2, DVec2};

/// Rotation angle of the transformed X axis.
pub fn rotation_of(m: &DAffine2) -> f64 {
    let x = m.matrix2.x_axis;
    x.y.atan2(x.x)
}

/// Uniform scale factor (sqrt of |det|).
pub fn scale_of(m: &DAffine2) -> f64 {
    let s = m.matrix2.determinant().abs().sqrt();
    if s.is_finite() && s > 0.0 { s } else { 1.0 }
}

/// `true` when the transform mirrors (negative determinant).
pub fn is_mirror(m: &DAffine2) -> bool {
    m.matrix2.determinant() < 0.0
}

/// The entity kind transformed by `m`. Curves are transformed exactly (a non-uniform scale turns
/// a circle into an ellipse); text, inserts and dimensions transform their anchor points and
/// take the rotation and uniform scale of `m`.
pub fn transform_kind(kind: &EntityKind, m: &DAffine2) -> EntityKind {
    let p = |q: DVec2| m.transform_point2(q);
    let rot = rotation_of(m);
    let s = scale_of(m);
    match kind {
        EntityKind::Point { p: q } => EntityKind::Point { p: p(*q) },
        EntityKind::Text(t) => {
            let mut t = t.clone();
            t.pos = p(t.pos);
            t.rotation += rot;
            t.height *= s;
            EntityKind::Text(t)
        }
        EntityKind::MText(t) => {
            let mut t = t.clone();
            t.pos = p(t.pos);
            t.rotation += rot;
            t.height *= s;
            t.width *= s;
            EntityKind::MText(t)
        }
        EntityKind::Dimension(d) => {
            let mut d = d.clone();
            d.kind = match d.kind {
                DimKind::Linear {
                    p1,
                    p2,
                    line_point,
                    rotation,
                } => DimKind::Linear {
                    p1: p(p1),
                    p2: p(p2),
                    line_point: p(line_point),
                    rotation: rotation + rot,
                },
                DimKind::Aligned { p1, p2, line_point } => DimKind::Aligned {
                    p1: p(p1),
                    p2: p(p2),
                    line_point: p(line_point),
                },
                DimKind::Radius { center, point } => DimKind::Radius {
                    center: p(center),
                    point: p(point),
                },
                DimKind::Diameter { center, point } => DimKind::Diameter {
                    center: p(center),
                    point: p(point),
                },
                DimKind::Angular {
                    vertex,
                    p1,
                    p2,
                    arc_point,
                } => DimKind::Angular {
                    vertex: p(vertex),
                    p1: p(p1),
                    p2: p(p2),
                    arc_point: p(arc_point),
                },
                DimKind::Ordinate {
                    origin,
                    point,
                    leader_end,
                    x_axis,
                } => DimKind::Ordinate {
                    origin: p(origin),
                    point: p(point),
                    leader_end: p(leader_end),
                    x_axis,
                },
            };
            d.text_pos = d.text_pos.map(p);
            EntityKind::Dimension(d)
        }
        EntityKind::Hatch(h) => {
            let mut h = h.clone();
            h.loops = h
                .loops
                .iter()
                .map(|l| HatchLoop {
                    curves: l.curves.iter().map(|c| c.transformed(m)).collect(),
                })
                .collect();
            h.pattern.angle += rot;
            h.pattern.scale *= s;
            EntityKind::Hatch(h)
        }
        EntityKind::Insert(i) => {
            let mut i = i.clone();
            i.pos = p(i.pos);
            i.rotation += rot;
            i.scale *= s;
            if is_mirror(m) {
                i.scale.y = -i.scale.y;
            }
            EntityKind::Insert(i)
        }
        other => match other.as_curve() {
            Some(c) => EntityKind::from_curve(c.transformed(m)),
            None => other.clone(),
        },
    }
}

/// Entity (properties kept) transformed by `m`.
pub fn transform_entity(e: &Entity, m: &DAffine2) -> Entity {
    Entity {
        kind: transform_kind(&e.kind, m),
        ..e.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_geom2d::{Circle2, Line2};

    #[test]
    fn translate_and_rotate() {
        let t = DAffine2::from_translation(DVec2::new(1.0, 2.0));
        match transform_kind(&EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::X)), &t) {
            EntityKind::Line(l) => {
                assert_eq!((l.a, l.b), (DVec2::new(1.0, 2.0), DVec2::new(2.0, 2.0)))
            }
            o => panic!("{o:?}"),
        }
        let r = DAffine2::from_angle(std::f64::consts::FRAC_PI_2);
        match transform_kind(&EntityKind::Circle(Circle2::new(DVec2::X, 1.0)), &r) {
            EntityKind::Circle(c) => assert!((c.c - DVec2::Y).length() < 1e-12),
            o => panic!("{o:?}"),
        }
        assert!((rotation_of(&r) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((scale_of(&DAffine2::from_scale(DVec2::splat(2.0))) - 2.0).abs() < 1e-12);
    }
}
