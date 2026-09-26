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
    // A reflection reverses existing directions, rather than adding its X-axis angle.
    let mirrored = is_mirror(m);
    let angle = |a: f64| if mirrored { rot - a } else { rot + a };
    match kind {
        EntityKind::Point { p: q } => EntityKind::Point { p: p(*q) },
        EntityKind::Text(t) => {
            let mut t = t.clone();
            t.pos = p(t.pos);
            t.rotation = angle(t.rotation);
            t.height *= s;
            EntityKind::Text(t)
        }
        EntityKind::MText(t) => {
            let mut t = t.clone();
            t.pos = p(t.pos);
            t.rotation = angle(t.rotation);
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
                    rotation: angle(rotation),
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
                    p1: p(if mirrored { p2 } else { p1 }),
                    p2: p(if mirrored { p1 } else { p2 }),
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
            h.pattern.angle = angle(h.pattern.angle);
            h.pattern.scale *= s;
            EntityKind::Hatch(h)
        }
        EntityKind::Insert(i) => {
            let mut i = i.clone();
            i.pos = p(i.pos);
            i.rotation = angle(i.rotation);
            i.scale *= s;
            if mirrored {
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
    use wcad_doc::{
        BlockId, DimStyleId, Dimension, HAlign, Hatch, HatchPatternRef, Insert, MText, Text,
        TextStyleId, VAlign,
    };
    use wcad_geom2d::{Circle2, Line2};

    fn near(a: DVec2, b: DVec2) {
        assert!(a.distance(b) < 1e-10, "{a:?} != {b:?}");
    }

    fn direction(angle: f64) -> DVec2 {
        DVec2::new(angle.cos(), angle.sin())
    }

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

    #[test]
    fn mirror_text_reflects_existing_baseline_and_preserves_text_properties() {
        let source = Text {
            pos: DVec2::new(3.0, -2.0),
            height: 2.5,
            rotation: 0.37,
            width_factor: 0.8,
            oblique: 0.2,
            style: TextStyleId(7),
            halign: HAlign::Center,
            valign: VAlign::Top,
            text: "Preserved text".into(),
        };
        for angle in [0.0, 0.9, std::f64::consts::PI] {
            let m = DAffine2::from_scale_angle_translation(
                DVec2::new(2.0, -2.0),
                angle,
                DVec2::new(4.0, 5.0),
            );
            let EntityKind::Text(t) = transform_kind(&EntityKind::Text(source.clone()), &m) else {
                panic!("text");
            };
            near(t.pos, m.transform_point2(source.pos));
            near(
                direction(t.rotation),
                m.transform_vector2(direction(source.rotation)).normalize(),
            );
            let mut expected = source.clone();
            expected.pos = t.pos;
            expected.rotation = t.rotation;
            assert!((t.height - source.height * 2.0).abs() < 1e-12);
            expected.height = t.height;
            assert_eq!(t, expected);
        }
    }

    #[test]
    fn mirror_mtext_reflects_baseline_and_retains_formatting_and_attachment() {
        let source = MText {
            pos: DVec2::new(-2.0, 4.0),
            height: 3.0,
            width: 30.0,
            rotation: -0.63,
            line_spacing: 1.5,
            attachment: 8,
            style: TextStyleId(9),
            text: "First\\P{\\H2x;Second}".into(),
        };
        let m = DAffine2::from_scale_angle_translation(DVec2::new(2.0, -2.0), 0.8, DVec2::ONE);
        let EntityKind::MText(t) = transform_kind(&EntityKind::MText(source.clone()), &m) else {
            panic!("mtext");
        };
        near(
            direction(t.rotation),
            m.transform_vector2(direction(source.rotation)).normalize(),
        );
        near(t.pos, m.transform_point2(source.pos));
        assert!((t.height - source.height * 2.0).abs() < 1e-12);
        assert!((t.width - source.width * 2.0).abs() < 1e-12);
        let mut expected = source;
        expected.pos = t.pos;
        expected.rotation = t.rotation;
        expected.height = t.height;
        expected.width = t.width;
        assert_eq!(t, expected);
    }

    #[test]
    fn mirror_linear_dimension_reflects_measurement_axis_and_override_position() {
        let p1 = DVec2::new(1.0, 2.0);
        let p2 = DVec2::new(7.0, 5.0);
        let angle = 0.41;
        let source = Dimension {
            kind: DimKind::Linear {
                p1,
                p2,
                line_point: DVec2::new(2.0, 8.0),
                rotation: angle,
            },
            style: DimStyleId(3),
            text_override: Some("Length <>".into()),
            text_pos: Some(DVec2::new(4.0, 8.0)),
        };
        let m = DAffine2::from_scale_angle_translation(DVec2::new(2.0, -2.0), 0.9, DVec2::ONE);
        let EntityKind::Dimension(d) = transform_kind(&EntityKind::Dimension(source.clone()), &m)
        else {
            panic!("dim");
        };
        let DimKind::Linear {
            p1: a,
            p2: b,
            line_point,
            rotation,
        } = d.kind
        else {
            panic!("linear");
        };
        near(a, m.transform_point2(p1));
        near(b, m.transform_point2(p2));
        near(line_point, m.transform_point2(DVec2::new(2.0, 8.0)));
        near(
            direction(rotation),
            m.transform_vector2(direction(angle)).normalize(),
        );
        assert!(
            ((b - a).dot(direction(rotation)) - 2.0 * (p2 - p1).dot(direction(angle))).abs()
                < 1e-10
        );
        assert_eq!(d.style, source.style);
        assert_eq!(d.text_override, source.text_override);
        near(
            d.text_pos.unwrap(),
            m.transform_point2(source.text_pos.unwrap()),
        );
    }

    #[test]
    fn mirrored_angular_dimension_retains_the_ccw_measured_sector() {
        let source = Dimension {
            kind: DimKind::Angular {
                vertex: DVec2::ZERO,
                p1: DVec2::X,
                p2: DVec2::Y,
                arc_point: DVec2::ONE,
            },
            style: DimStyleId(1),
            text_override: None,
            text_pos: None,
        };
        let m = DAffine2::from_scale(DVec2::new(1.0, -1.0));
        let EntityKind::Dimension(d) = transform_kind(&EntityKind::Dimension(source), &m) else {
            panic!("dim");
        };
        let DimKind::Angular {
            vertex,
            p1,
            p2,
            arc_point,
        } = d.kind
        else {
            panic!("angular");
        };
        assert_eq!(vertex, DVec2::ZERO);
        assert_eq!(p1, -DVec2::Y);
        assert_eq!(p2, DVec2::X);
        assert_eq!(arc_point, DVec2::new(1.0, -1.0));
        assert!(
            (wcad_math::ccw_sweep(p1.y.atan2(p1.x), p2.y.atan2(p2.x))
                - std::f64::consts::FRAC_PI_2)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn mirrored_insert_composition_and_hatch_pattern_directions_are_consistent() {
        let source = Insert {
            block: BlockId(8),
            pos: DVec2::new(2.0, -3.0),
            scale: DVec2::new(2.0, 3.0),
            rotation: 0.47,
        };
        let m = DAffine2::from_scale_angle_translation(DVec2::new(2.0, -2.0), 0.8, DVec2::ONE);
        let EntityKind::Insert(i) = transform_kind(&EntityKind::Insert(source.clone()), &m) else {
            panic!("insert");
        };
        assert_eq!(i.block, source.block);
        let before =
            DAffine2::from_scale_angle_translation(source.scale, source.rotation, source.pos);
        let after = DAffine2::from_scale_angle_translation(i.scale, i.rotation, i.pos);
        for p in [DVec2::ZERO, DVec2::X, DVec2::Y, DVec2::ONE] {
            near(
                after.transform_point2(p),
                m.transform_point2(before.transform_point2(p)),
            );
        }
        let h = Hatch {
            loops: vec![HatchLoop {
                curves: vec![wcad_geom2d::Curve2::Circle(Circle2::new(DVec2::ZERO, 3.0))],
            }],
            pattern: HatchPatternRef {
                name: "ANSI31".into(),
                angle: 0.47,
                scale: 3.0,
            },
        };
        let EntityKind::Hatch(result) = transform_kind(&EntityKind::Hatch(h.clone()), &m) else {
            panic!("hatch");
        };
        assert_eq!(result.pattern.name, h.pattern.name);
        assert!((result.pattern.scale - 6.0).abs() < 1e-12);
        near(
            direction(result.pattern.angle),
            m.transform_vector2(direction(h.pattern.angle)).normalize(),
        );
        assert_eq!(
            result.loops[0].curves[0],
            h.loops[0].curves[0].transformed(&m)
        );
    }
}
