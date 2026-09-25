//! Demo drawing (menu "Load demo drawing", web `?demo`): a small plate with holes, center lines,
//! a hatch, a dimension, Chinese text, and a box body in the 3D part.

use wcad_doc::{
    BodyOp, Color, DimKind, Dimension, Document, Entity, EntityId, EntityKind, Feature,
    FeatureKind, HAlign, Hatch, HatchLoop, HatchPatternRef, Layer, LayerId, LineWeight,
    LinetypeRef, Primitive, Text, VAlign,
};
use wcad_geom2d::{Arc2, Circle2, Curve2, Line2, Polyline2};
use wcad_math::{DAffine3, DVec2, DVec3};

pub fn demo_document() -> Document {
    let mut doc = Document::new();
    let d = &doc.drawing;
    let continuous = d.linetype_by_name("Continuous");
    let center = d.linetype_by_name("CENTER");
    let style = d.tables.current_text_style;
    let dim_style = d.tables.current_dim_style;
    doc.meta.title = "Demo".into();
    doc.transact("demo", |tx| {
        let layer = |tx: &mut wcad_doc::Tx<'_>,
                     name: &str,
                     color: Color,
                     lt: Option<wcad_doc::LinetypeId>,
                     w: u16|
         -> LayerId {
            let id = tx.ids().layer();
            if let Some(lt) = lt.or(continuous) {
                tx.tables_mut().layers.insert(
                    id,
                    Layer {
                        name: name.into(),
                        color,
                        linetype: lt,
                        lineweight: LineWeight::Mm100(w),
                        visible: true,
                        frozen: false,
                        locked: false,
                        plot: true,
                    },
                );
            }
            id
        };
        let outline = layer(tx, "轮廓线", Color::Aci(7), None, 50);
        let centers = layer(tx, "中心线", Color::Aci(1), center, 18);
        let hatch_l = layer(tx, "剖面线", Color::Aci(8), None, 13);
        let annot = layer(tx, "标注", Color::Aci(3), None, 18);
        let put = |tx: &mut wcad_doc::Tx<'_>, layer: LayerId, kind: EntityKind| -> EntityId {
            tx.insert(Entity {
                id: EntityId(0),
                layer,
                color: Color::ByLayer,
                linetype: LinetypeRef::ByLayer,
                linetype_scale: 0.3,
                lineweight: LineWeight::ByLayer,
                kind,
            })
        };
        // Plate outline with a rounded corner.
        let p = |x: f64, y: f64| DVec2::new(x, y);
        put(
            tx,
            outline,
            EntityKind::Line(Line2::new(p(0.0, 0.0), p(120.0, 0.0))),
        );
        put(
            tx,
            outline,
            EntityKind::Line(Line2::new(p(120.0, 0.0), p(120.0, 60.0))),
        );
        put(
            tx,
            outline,
            EntityKind::Arc(Arc2::new(
                p(100.0, 60.0),
                20.0,
                0.0,
                std::f64::consts::FRAC_PI_2,
            )),
        );
        put(
            tx,
            outline,
            EntityKind::Line(Line2::new(p(100.0, 80.0), p(0.0, 80.0))),
        );
        put(
            tx,
            outline,
            EntityKind::Line(Line2::new(p(0.0, 80.0), p(0.0, 0.0))),
        );
        // Holes and their center marks.
        for c in [p(25.0, 25.0), p(25.0, 55.0), p(75.0, 40.0)] {
            let r = if c.x > 50.0 { 15.0 } else { 6.0 };
            put(tx, outline, EntityKind::Circle(Circle2::new(c, r)));
            put(
                tx,
                centers,
                EntityKind::Line(Line2::new(
                    c - DVec2::X * (r + 4.0),
                    c + DVec2::X * (r + 4.0),
                )),
            );
            put(
                tx,
                centers,
                EntityKind::Line(Line2::new(
                    c - DVec2::Y * (r + 4.0),
                    c + DVec2::Y * (r + 4.0),
                )),
            );
        }
        // Slot as a bulged polyline.
        let mut slot = Polyline2::from_points(
            [p(95.0, 15.0), p(110.0, 15.0), p(110.0, 25.0), p(95.0, 25.0)],
            true,
        );
        slot.verts[1].bulge = 1.0;
        slot.verts[3].bulge = 1.0;
        put(tx, outline, EntityKind::Polyline(slot));
        // Hatched region to the right of the part.
        let sq = Polyline2::from_points(
            [p(140.0, 0.0), p(180.0, 0.0), p(180.0, 30.0), p(140.0, 30.0)],
            true,
        );
        put(
            tx,
            hatch_l,
            EntityKind::Hatch(Hatch {
                loops: vec![HatchLoop {
                    curves: vec![Curve2::Polyline(sq.clone())],
                }],
                pattern: HatchPatternRef {
                    name: "ANSI31".into(),
                    angle: 0.0,
                    scale: 1.0,
                },
            }),
        );
        put(tx, outline, EntityKind::Polyline(sq));
        let solid = Polyline2::from_points([p(140.0, 40.0), p(180.0, 40.0), p(160.0, 70.0)], true);
        put(
            tx,
            hatch_l,
            EntityKind::Hatch(Hatch {
                loops: vec![HatchLoop {
                    curves: vec![Curve2::Polyline(solid)],
                }],
                pattern: HatchPatternRef {
                    name: "SOLID".into(),
                    angle: 0.0,
                    scale: 1.0,
                },
            }),
        );
        // Dimensions and text.
        put(
            tx,
            annot,
            EntityKind::Dimension(Dimension {
                kind: DimKind::Linear {
                    p1: p(0.0, 0.0),
                    p2: p(120.0, 0.0),
                    line_point: p(0.0, -12.0),
                    rotation: 0.0,
                },
                style: dim_style,
                text_override: None,
                text_pos: None,
            }),
        );
        put(
            tx,
            annot,
            EntityKind::Dimension(Dimension {
                kind: DimKind::Diameter {
                    center: p(75.0, 40.0),
                    point: p(75.0, 40.0) + DVec2::splat(15.0 * std::f64::consts::FRAC_1_SQRT_2),
                },
                style: dim_style,
                text_override: None,
                text_pos: None,
            }),
        );
        put(
            tx,
            annot,
            EntityKind::Text(Text {
                pos: p(0.0, 92.0),
                height: 7.0,
                rotation: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                style,
                halign: HAlign::Left,
                valign: VAlign::Baseline,
                text: "示例零件图 Demo plate".into(),
            }),
        );
        put(
            tx,
            annot,
            EntityKind::Text(Text {
                pos: p(140.0, -12.0),
                height: 3.5,
                rotation: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                style,
                halign: HAlign::Left,
                valign: VAlign::Baseline,
                text: "材料：Q235  比例 1:1".into(),
            }),
        );
        put(tx, annot, EntityKind::Point { p: p(60.0, 70.0) });
        tx.tables_mut().current_layer = outline;
        // 3D: a plate-sized box.
        let fid = tx.ids().feature();
        tx.part_mut().features.push(Feature {
            id: fid,
            name: "Box".into(),
            suppressed: false,
            kind: FeatureKind::Primitive {
                shape: Primitive::Box {
                    size: DVec3::new(120.0, 80.0, 10.0),
                },
                placement: DAffine3::IDENTITY,
                op: BodyOp::NewBody,
            },
        });
    });
    doc.clear_history();
    doc.mark_saved();
    doc
}

#[cfg(test)]
mod tests {
    #[test]
    fn demo_builds() {
        let d = super::demo_document();
        assert!(d.drawing.entities.len() > 15);
        assert_eq!(d.part.features.len(), 1);
        assert!(!d.can_undo());
    }
}
