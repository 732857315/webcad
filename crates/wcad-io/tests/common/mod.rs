//! Shared fixtures for the wcad-io integration tests.
#![allow(dead_code)]

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use wcad_doc::*;
use wcad_geom2d::{Arc2, Circle2, Curve2, EllipseArc2, Line2, Nurbs2, PolyVertex, Polyline2};
use wcad_math::DVec2;

pub fn v(x: f64, y: f64) -> DVec2 {
    DVec2::new(x, y)
}

/// Names of everything the sample creates, for lookups after a round trip.
pub struct Sample {
    pub doc: Document,
}

fn ent(layer: LayerId, kind: EntityKind) -> Entity {
    Entity {
        id: EntityId(0),
        layer,
        color: Color::ByLayer,
        linetype: LinetypeRef::ByLayer,
        linetype_scale: 1.0,
        lineweight: LineWeight::ByLayer,
        kind,
    }
}

/// A document exercising every entity kind and table feature the adapters map.
pub fn sample() -> Sample {
    let mut doc = Document::new();
    doc.meta.units = Units::Millimeter;
    doc.transact("sample", |tx| {
        let lt_id = tx.ids().linetype();
        let geom = tx.ids().layer();
        let off = tx.ids().layer();
        let frozen = tx.ids().layer();
        let locked = tx.ids().layer();
        let noplot = tx.ids().layer();
        let cjk = tx.ids().text_style();
        let iso = tx.ids().dim_style();
        let bolt = tx.ids().block();
        let t = tx.tables_mut();
        t.settings.ltscale = 2.5;
        t.linetypes.insert(
            lt_id,
            Linetype {
                name: "MYDASH".into(),
                description: "my dash".into(),
                pattern: vec![5.0, -2.5, 0.0, -2.5],
            },
        );
        let dashed = t
            .linetypes
            .iter()
            .find(|(_, l)| l.name == "DASHED")
            .map(|(id, _)| *id)
            .expect("DASHED");
        let continuous = t
            .linetypes
            .iter()
            .find(|(_, l)| l.name == "Continuous")
            .map(|(id, _)| *id)
            .expect("cont");
        let mk = |name: &str, color: Color, lt: LinetypeId, lw: LineWeight| Layer {
            name: name.into(),
            color,
            linetype: lt,
            lineweight: lw,
            visible: true,
            frozen: false,
            locked: false,
            plot: true,
        };
        t.layers.insert(
            geom,
            mk("GEOM", Color::Aci(1), dashed, LineWeight::Mm100(35)),
        );
        t.layers.insert(
            off,
            Layer {
                visible: false,
                ..mk("OFF", Color::Aci(3), continuous, LineWeight::Default)
            },
        );
        t.layers.insert(
            frozen,
            Layer {
                frozen: true,
                ..mk("FROZEN", Color::Aci(4), continuous, LineWeight::Default)
            },
        );
        t.layers.insert(
            locked,
            Layer {
                locked: true,
                ..mk("LOCKED", Color::Aci(5), continuous, LineWeight::Default)
            },
        );
        t.layers.insert(
            noplot,
            Layer {
                plot: false,
                ..mk(
                    "不打印",
                    Color::Rgb(10, 200, 30),
                    continuous,
                    LineWeight::Mm100(50),
                )
            },
        );
        t.text_styles.insert(
            cjk,
            TextStyle {
                name: "CJK".into(),
                font: "default".into(),
                height: 0.0,
                width_factor: 0.8,
                oblique: 15f64.to_radians(),
            },
        );
        let std_text = t.current_text_style;
        t.dim_styles.insert(
            iso,
            DimStyle {
                name: "ISO".into(),
                text_style: std_text,
                text_height: 3.5,
                arrow_size: 3.0,
                ext_offset: 1.0,
                ext_extend: 2.0,
                text_gap: 0.8,
                scale: 1.0,
                decimals: 1,
                angle_decimals: 1,
                prefix: String::new(),
                suffix: " mm".into(),
            },
        );
        let layer0 = t.current_layer;

        // Block with a non-zero base point.
        let mut bents = std::collections::BTreeMap::new();
        let c_id = tx.ids().entity();
        bents.insert(
            c_id,
            Entity {
                id: c_id,
                ..ent(layer0, EntityKind::Circle(Circle2::new(v(5.0, 5.0), 2.0)))
            },
        );
        let l_id = tx.ids().entity();
        bents.insert(
            l_id,
            Entity {
                id: l_id,
                color: Color::ByBlock,
                ..ent(
                    layer0,
                    EntityKind::Line(Line2::new(v(3.0, 5.0), v(7.0, 5.0))),
                )
            },
        );
        tx.blocks_mut().insert(
            bolt,
            Block {
                name: "BOLT".into(),
                base: v(5.0, 5.0),
                entities: bents,
            },
        );

        let kinds: Vec<Entity> = vec![
            ent(layer0, EntityKind::Point { p: v(1.0, 2.0) }),
            Entity {
                color: Color::Aci(2),
                linetype: LinetypeRef::Id(lt_id),
                linetype_scale: 2.0,
                lineweight: LineWeight::Mm100(50),
                ..ent(
                    geom,
                    EntityKind::Line(Line2::new(v(0.0, 0.0), v(100.0, 50.0))),
                )
            },
            Entity {
                color: Color::Rgb(0x12, 0x34, 0x56),
                ..ent(geom, EntityKind::Circle(Circle2::new(v(50.0, 50.0), 25.0)))
            },
            Entity {
                color: Color::ByBlock,
                linetype: LinetypeRef::ByBlock,
                lineweight: LineWeight::ByBlock,
                ..ent(
                    layer0,
                    EntityKind::Arc(Arc2::new(v(10.0, 10.0), 5.0, 0.3, 2.5)),
                )
            },
            ent(
                layer0,
                EntityKind::Arc(Arc2::new(v(-10.0, 10.0), 3.0, 5.5, 1.0)),
            ),
            ent(
                layer0,
                EntityKind::Ellipse(EllipseArc2 {
                    c: v(20.0, 0.0),
                    major: v(8.0, 3.0),
                    ratio: 0.4,
                    start: 0.5,
                    end: 4.0,
                }),
            ),
            ent(
                layer0,
                EntityKind::Ellipse(EllipseArc2 {
                    c: v(40.0, 0.0),
                    major: v(0.0, 6.0),
                    ratio: 0.5,
                    start: 0.0,
                    end: TAU,
                }),
            ),
            ent(
                off,
                EntityKind::Polyline(Polyline2 {
                    verts: vec![
                        PolyVertex::with_bulge(v(0.0, 0.0), 0.0),
                        PolyVertex::with_bulge(v(10.0, 0.0), 1.0),
                        PolyVertex::with_bulge(v(10.0, 10.0), 0.0),
                        PolyVertex::with_bulge(v(0.0, 10.0), -0.414213562),
                    ],
                    closed: true,
                }),
            ),
            ent(
                layer0,
                EntityKind::Polyline(Polyline2::from_points(
                    [v(0.0, -5.0), v(5.0, -7.0), v(9.0, -5.0)],
                    false,
                )),
            ),
            ent(
                frozen,
                EntityKind::Spline(Nurbs2 {
                    degree: 3,
                    ctrl: vec![
                        v(0.0, 20.0),
                        v(5.0, 25.0),
                        v(10.0, 15.0),
                        v(15.0, 20.0),
                        v(20.0, 22.0),
                        v(25.0, 18.0),
                    ],
                    weights: vec![],
                    knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0, 3.0, 3.0],
                    fit_points: vec![],
                    closed: false,
                }),
            ),
            ent(
                locked,
                EntityKind::Spline(Nurbs2 {
                    degree: 2,
                    ctrl: vec![v(30.0, 20.0), v(35.0, 25.0), v(40.0, 20.0)],
                    weights: vec![1.0, std::f64::consts::FRAC_1_SQRT_2, 1.0],
                    knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                    fit_points: vec![],
                    closed: false,
                }),
            ),
            ent(
                layer0,
                EntityKind::Text(Text {
                    pos: v(0.0, -20.0),
                    height: 2.5,
                    rotation: 0.0,
                    width_factor: 1.0,
                    oblique: 0.0,
                    style: std_text,
                    halign: HAlign::Left,
                    valign: VAlign::Baseline,
                    text: "Hello TEXT %%c10".into(),
                }),
            ),
            ent(
                layer0,
                EntityKind::Text(Text {
                    pos: v(30.0, -20.0),
                    height: 3.0,
                    rotation: 30f64.to_radians(),
                    width_factor: 0.8,
                    oblique: 12f64.to_radians(),
                    style: cjk,
                    halign: HAlign::Center,
                    valign: VAlign::Middle,
                    text: "中文 Text".into(),
                }),
            ),
            ent(
                layer0,
                EntityKind::MText(MText {
                    pos: v(0.0, -30.0),
                    height: 2.5,
                    width: 40.0,
                    rotation: 0.2,
                    line_spacing: 1.2,
                    attachment: 5,
                    style: cjk,
                    text: "MTEXT line1\\Pline2 \u{4E2D}\u{6587} {\\H1.5x;big}".into(),
                }),
            ),
            dim(
                layer0,
                iso,
                DimKind::Linear {
                    p1: v(0.0, 0.0),
                    p2: v(40.0, 10.0),
                    line_point: v(20.0, 30.0),
                    rotation: 0.0,
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Linear {
                    p1: v(0.0, 0.0),
                    p2: v(40.0, 10.0),
                    line_point: v(60.0, 5.0),
                    rotation: FRAC_PI_2,
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Aligned {
                    p1: v(0.0, 0.0),
                    p2: v(30.0, 40.0),
                    line_point: v(-10.0, 30.0),
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Radius {
                    center: v(50.0, 50.0),
                    point: v(67.67766952966369, 67.67766952966369),
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Diameter {
                    center: v(50.0, 50.0),
                    point: v(75.0, 50.0),
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Angular {
                    vertex: v(0.0, 0.0),
                    p1: v(20.0, 0.0),
                    p2: v(10.0, 10.0),
                    arc_point: v(15.0, 5.0),
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Ordinate {
                    origin: v(0.0, 0.0),
                    point: v(25.0, 10.0),
                    leader_end: v(25.0, 30.0),
                    x_axis: true,
                },
            ),
            dim(
                layer0,
                iso,
                DimKind::Ordinate {
                    origin: v(0.0, 0.0),
                    point: v(25.0, 10.0),
                    leader_end: v(45.0, 10.0),
                    x_axis: false,
                },
            ),
            {
                let mut e = dim(
                    layer0,
                    iso,
                    DimKind::Aligned {
                        p1: v(0.0, -40.0),
                        p2: v(20.0, -40.0),
                        line_point: v(10.0, -45.0),
                    },
                );
                if let EntityKind::Dimension(d) = &mut e.kind {
                    d.text_override = Some("L=<>".into());
                    d.text_pos = Some(v(10.0, -50.0));
                }
                e
            },
            ent(
                layer0,
                EntityKind::Hatch(Hatch {
                    loops: vec![
                        HatchLoop {
                            curves: vec![Curve2::Polyline(Polyline2 {
                                verts: vec![
                                    PolyVertex::with_bulge(v(30.0, 30.0), 0.0),
                                    PolyVertex::with_bulge(v(50.0, 30.0), 0.5),
                                    PolyVertex::with_bulge(v(50.0, 50.0), 0.0),
                                    PolyVertex::with_bulge(v(30.0, 50.0), 0.0),
                                ],
                                closed: true,
                            })],
                        },
                        HatchLoop {
                            curves: vec![Curve2::Circle(Circle2::new(v(40.0, 40.0), 3.0))],
                        },
                    ],
                    pattern: HatchPatternRef {
                        name: "SOLID".into(),
                        angle: 0.0,
                        scale: 1.0,
                    },
                }),
            ),
            ent(
                geom,
                EntityKind::Hatch(Hatch {
                    loops: vec![HatchLoop {
                        curves: vec![
                            Curve2::Line(Line2::new(v(60.0, 0.0), v(80.0, 0.0))),
                            Curve2::Arc(Arc2::new(v(80.0, 10.0), 10.0, -FRAC_PI_2, FRAC_PI_2)),
                            Curve2::Line(Line2::new(v(80.0, 20.0), v(70.0, 20.0))),
                            Curve2::Ellipse(EllipseArc2 {
                                c: v(65.0, 20.0),
                                major: v(5.0, 0.0),
                                ratio: 0.6,
                                start: 0.0,
                                end: PI,
                            }),
                            Curve2::Spline(Nurbs2 {
                                degree: 3,
                                ctrl: vec![
                                    v(60.0, 20.0),
                                    v(58.0, 13.0),
                                    v(62.0, 7.0),
                                    v(60.0, 0.0),
                                ],
                                weights: vec![],
                                knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
                                fit_points: vec![],
                                closed: false,
                            }),
                        ],
                    }],
                    pattern: HatchPatternRef {
                        name: "ANSI31".into(),
                        angle: 0.5,
                        scale: 2.0,
                    },
                }),
            ),
            Entity {
                color: Color::Aci(6),
                ..ent(
                    geom,
                    EntityKind::Insert(Insert {
                        block: bolt,
                        pos: v(100.0, 100.0),
                        scale: v(2.0, 1.5),
                        rotation: 0.3,
                    }),
                )
            },
        ];
        for e in kinds {
            tx.insert(e);
        }
    });
    Sample { doc }
}

fn dim(layer: LayerId, style: DimStyleId, kind: DimKind) -> Entity {
    ent(
        layer,
        EntityKind::Dimension(Dimension {
            kind,
            style,
            text_override: None,
            text_pos: None,
        }),
    )
}
