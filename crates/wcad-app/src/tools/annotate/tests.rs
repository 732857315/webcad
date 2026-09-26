use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};

use wcad_doc::{DimKind, DimStyle, Dimension, EntityId, EntityKind, Hatch, TextStyle};
use wcad_geom2d::{Arc2, Circle2, Curve, Curve2, EllipseArc2, Line2, PolyVertex, Polyline2};
use wcad_math::DVec2;

use super::hatch::{BoundaryError, boundary_loops};
use super::*;
use crate::dimgen;
use crate::editor::LogKind;
use crate::testing::Harness;
use crate::tools::{Accept, ToolInput};

fn harness() -> Harness {
    let mut h = Harness::new();
    h.ed.draft.osnap_on = false;
    h.ed.draft.ortho = false;
    h
}

fn add(h: &mut Harness, kind: EntityKind) -> EntityId {
    let id = h.ed.doc.transact("fixture", |tx| tx.add(kind));
    h.ed.pump();
    id
}

fn dimension(h: &Harness, i: usize) -> Dimension {
    match h.of_type("DIMENSION")[i].1.clone() {
        EntityKind::Dimension(d) => d,
        _ => unreachable!(),
    }
}

fn hatch(h: &Harness) -> Hatch {
    match h.of_type("HATCH")[0].1.clone() {
        EntityKind::Hatch(h) => h,
        _ => unreachable!(),
    }
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-8, "{a} != {b}");
}

fn assert_error(h: &Harness) {
    assert_eq!(
        h.ed.log.last().map(|l| l.kind),
        Some(LogKind::Error),
        "{:?}",
        h.ed.log.last()
    );
}

fn undo_redo(h: &mut Harness, name: &str) {
    let before = h.ed.doc.drawing.entities.clone();
    assert_eq!(h.ed.doc.undo().as_deref(), Some(name));
    assert_eq!(h.ed.doc.drawing.entities.len() + 1, before.len());
    assert_eq!(h.ed.doc.redo().as_deref(), Some(name));
    assert_eq!(h.ed.doc.drawing.entities, before);
    h.ed.pump();
}

fn lock_current(h: &mut Harness, locked: bool) {
    let id = h.ed.doc.drawing.tables.current_layer;
    h.ed.doc.transact("lock", |tx| {
        tx.tables_mut().layers.get_mut(&id).unwrap().locked = locked
    });
    h.ed.pump();
}

fn square(size: f64) -> Curve2 {
    Curve2::Polyline(Polyline2::from_points(
        [
            DVec2::new(-size, -size),
            DVec2::new(size, -size),
            DVec2::new(size, size),
            DVec2::new(-size, size),
        ],
        true,
    ))
}

fn select_curves(h: &mut Harness, curves: &[Curve2]) -> Vec<EntityId> {
    let ids: Vec<_> = curves
        .iter()
        .map(|c| add(h, EntityKind::from_curve(c.clone())))
        .collect();
    h.ed.selection.set(ids.iter().copied());
    ids
}

#[test]
fn registrations_have_exact_aliases_ascii_icons_and_bilingual_labels() {
    let r = CommandRegistry::with_all_modules();
    let expected: &[(&str, &[&str])] = &[
        ("TEXT", &["DT"]),
        ("MTEXT", &["MT", "T"]),
        ("DIMLINEAR", &["DLI"]),
        ("DIMALIGNED", &["DAL"]),
        ("DIMRADIUS", &["DRA"]),
        ("DIMDIAMETER", &["DDI"]),
        ("DIMANGULAR", &["DAN"]),
        ("DIMORDINATE", &["DOR"]),
        ("HATCH", &["H"]),
    ];
    for &(name, aliases) in expected {
        let c = r.find(name).unwrap();
        assert_eq!(c.aliases, aliases);
        assert!(c.icon.is_ascii());
        assert_ne!((c.label)(Lang::Zh), (c.label)(Lang::En));
        assert_eq!(c.tab, Some(RibbonTab::Annotate));
        for alias in aliases {
            assert_eq!(r.find(alias).unwrap().name, name);
        }
    }
    assert_eq!(r.find("M").unwrap().name, "MOVE");
    assert_eq!(r.find("L").unwrap().name, "LINE");
}

#[test]
fn text_and_mtext_bodies_are_never_commands_coordinates_or_numbers() {
    for command in ["DT", "MT", "T"] {
        for body in [
            "LINE", "U", "H", "123.45", "1,2", "@3<45", "<90", "1e999", "NaN", "ANSI31",
        ] {
            let mut h = harness();
            h.cmd(command).cmd("10,20").cmd("2.5").cmd("0");
            assert_eq!(h.ed.tool_accepts(), Accept::TEXT);
            assert!(h.ed.prompt().1.is_empty());
            h.cmd(body);
            assert!(!h.ed.has_tool());
            assert_eq!(h.ed.doc.drawing.entities.len(), 1);
            let kind = &h.ed.doc.drawing.entities.values().next().unwrap().kind;
            let text = match kind {
                EntityKind::Text(t) => &t.text,
                EntityKind::MText(t) => &t.text,
                _ => panic!("body was interpreted as geometry"),
            };
            assert_eq!(text, body);
        }
    }
}

#[test]
fn current_text_style_id_and_defaults_are_used_not_standard() {
    let mut h = harness();
    let style = h.ed.doc.transact("style", |tx| {
        let id = tx.ids().text_style();
        tx.tables_mut().text_styles.insert(
            id,
            TextStyle {
                name: "Notes".into(),
                font: "default".into(),
                height: 7.0,
                width_factor: 0.65,
                oblique: 0.2,
            },
        );
        tx.tables_mut().current_text_style = id;
        id
    });
    h.cmd("TEXT").cmd("1,2").enter().cmd("30").cmd("note");
    let EntityKind::Text(t) = &h.of_type("TEXT")[0].1 else {
        panic!()
    };
    assert_eq!(t.style, style);
    assert_eq!(t.pos, DVec2::new(1.0, 2.0));
    near(t.height, 7.0);
    near(t.rotation, PI / 6.0);
    near(t.width_factor, 0.65);
    near(t.oblique, 0.2);
    undo_redo(&mut h, "TEXT");
    h.cmd("MTEXT").cmd("1,2").enter().enter().cmd("note");
    let EntityKind::MText(t) = &h.of_type("MTEXT")[0].1 else {
        panic!()
    };
    assert_eq!(t.style, style);
    near(t.height, 7.0);
    near(t.width, 0.0);
    undo_redo(&mut h, "MTEXT");
}

#[test]
fn mtext_preserves_paragraph_codes_and_normalizes_real_newlines() {
    let mut h = harness();
    let raw = "LINE\\P123\r\n0,0\rfinal";
    h.cmd("MTEXT").cmd("0,0").cmd("3").cmd("-90").cmd(raw);
    let EntityKind::MText(t) = &h.of_type("MTEXT")[0].1 else {
        panic!()
    };
    assert_eq!(t.text, "LINE\\P123\n0,0\nfinal");
    assert_eq!(
        wcad_geom2d::text::mtext_to_plain(&t.text),
        "LINE\n123\n0,0\nfinal"
    );
    near(t.rotation, 3.0 * FRAC_PI_2);
}

#[test]
fn text_point_height_rotation_and_invalid_body_are_retryable() {
    let mut h = harness();
    h.cmd("TEXT").cmd("2,3").cmd("0");
    assert_error(&h);
    h.cmd("-2");
    assert_error(&h);
    h.cmd("2,3");
    assert_error(&h);
    h.cmd("2,8").cmd("2,3");
    assert_error(&h);
    h.cmd("2,10").enter();
    assert_error(&h);
    h.cmd("first\nsecond");
    assert_error(&h);
    h.cmd("first\\Psecond");
    assert_error(&h);
    h.cmd("bad\u{0}");
    assert_error(&h);
    h.cmd(&"x".repeat(65_537));
    assert_error(&h);
    assert!(h.ed.doc.drawing.entities.is_empty());
    h.cmd("valid");
    let EntityKind::Text(t) = &h.of_type("TEXT")[0].1 else {
        panic!()
    };
    near(t.height, 5.0);
    near(t.rotation, FRAC_PI_2);
}

#[test]
fn text_cancel_every_step_leaves_no_entity_preview_or_undo_step() {
    for command in ["TEXT", "MTEXT"] {
        for inputs in [
            &[][..],
            &["0,0"][..],
            &["0,0", "3"][..],
            &["0,0", "3", "45"][..],
        ] {
            let mut h = harness();
            h.cmd(command);
            for s in inputs {
                h.cmd(s);
            }
            h.hover(5.0, 6.0).esc();
            assert!(h.ed.preview.is_empty());
            assert!(h.ed.doc.drawing.entities.is_empty());
            assert_eq!(h.ed.doc.undo(), None);
        }
    }
}

#[test]
fn text_locked_layer_and_missing_style_do_not_commit() {
    let mut h = harness();
    h.cmd("TEXT").cmd("0,0").cmd("2").cmd("0");
    lock_current(&mut h, true);
    h.cmd("body");
    assert_error(&h);
    assert!(h.ed.doc.drawing.entities.is_empty());
    assert_eq!(h.ed.tool_accepts(), Accept::TEXT);
    lock_current(&mut h, false);
    h.cmd("body");
    assert_eq!(h.count("TEXT"), 1);
    undo_redo(&mut h, "TEXT");

    let mut h = harness();
    h.ed.doc.drawing.tables.current_text_style = wcad_doc::TextStyleId(u32::MAX);
    h.cmd("TEXT").cmd("0,0").cmd("2").cmd("0").cmd("body");
    assert_error(&h);
    assert!(h.ed.doc.drawing.entities.is_empty());
    h.esc();
}

#[test]
fn linear_horizontal_and_vertical_auto_positions() {
    for (position, expected, rotation) in [("15,60", 30.0, 0.0), ("60,20", 40.0, FRAC_PI_2)] {
        let mut h = harness();
        h.cmd("DLI").cmd("0,0").cmd("30,40").cmd(position);
        let d = dimension(&h, 0);
        near(dimgen::measure(&d.kind), expected);
        let DimKind::Linear { rotation: a, .. } = d.kind else {
            panic!()
        };
        near(a, rotation);
        undo_redo(&mut h, "DIMLINEAR");
    }
}

#[test]
fn linear_rotated_and_explicit_axis_keywords() {
    let mut h = harness();
    h.cmd("DLI").cmd("0,0").cmd("30,40").cmd("R");
    assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
    h.cmd("45").cmd("10,70");
    let d = dimension(&h, 0);
    near(dimgen::measure(&d.kind), 70.0 / 2f64.sqrt());
    let DimKind::Linear {
        rotation,
        line_point,
        ..
    } = d.kind
    else {
        panic!()
    };
    near(rotation, FRAC_PI_4);
    assert_eq!(line_point, DVec2::new(10.0, 70.0));

    h.cmd("DLI").cmd("0,0").cmd("30,40").cmd("H").cmd("60,20");
    near(dimgen::measure(&dimension(&h, 1).kind), 30.0);
    h.cmd("DLI").cmd("0,0").cmd("30,40").cmd("V").cmd("15,60");
    near(dimgen::measure(&dimension(&h, 2).kind), 40.0);
}

#[test]
fn aligned_preview_is_the_entity_committed_and_uses_current_dim_style() {
    let mut h = harness();
    let style = h.ed.doc.transact("style", |tx| {
        let id = tx.ids().dim_style();
        let mut style = DimStyle::standard(tx.drawing().tables.current_text_style);
        style.decimals = 1;
        style.prefix = "L=".into();
        tx.tables_mut().dim_styles.insert(id, style);
        tx.tables_mut().current_dim_style = id;
        id
    });
    h.cmd("DAL").cmd("0,0").cmd("30,40").hover(10.0, 30.0);
    assert_eq!(h.ed.preview.ghosts.len(), 1);
    let preview = h.ed.preview.ghosts[0].kind.clone();
    assert_eq!(h.count("DIMENSION"), 0);
    h.click(10.0, 30.0);
    assert_eq!(h.of_type("DIMENSION")[0].1, preview);
    assert!(h.ed.preview.is_empty());
    let d = dimension(&h, 0);
    assert_eq!(d.style, style);
    near(dimgen::measure(&d.kind), 50.0);
    let g = dimgen::dimension_geometry(
        &d,
        &dimgen::style_of(&d, &h.ed.doc.drawing),
        &h.ed.doc.drawing.tables,
    );
    assert_eq!(g.text, "L=50.0");
    assert_eq!(g.arrows.len(), 2);
    assert_eq!(
        h.ed.doc.drawing.entities.len(),
        1,
        "not exploded into render geometry"
    );
    undo_redo(&mut h, "DIMALIGNED");
}

#[test]
fn linear_and_aligned_pick_line_or_polyline_segment() {
    for command in ["DLI", "DAL"] {
        let mut h = harness();
        add(
            &mut h,
            EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::new(30.0, 40.0))),
        );
        h.cmd(command).enter();
        assert_eq!(h.ed.tool_accepts(), Accept::PICK);
        h.click(15.01, 20.0).cmd("15,60");
        near(
            dimgen::measure(&dimension(&h, 0).kind),
            if command == "DLI" { 30.0 } else { 50.0 },
        );
    }
    let mut h = harness();
    add(
        &mut h,
        EntityKind::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::new(30.0, 0.0), DVec2::new(30.0, 40.0)],
            false,
        )),
    );
    h.cmd("DAL").cmd("O").click(30.02, 20.0).cmd("50,20");
    near(dimgen::measure(&dimension(&h, 0).kind), 40.0);
}

#[test]
fn radius_circle_pick_preserves_radius_not_mouse_distance() {
    let mut h = harness();
    add(
        &mut h,
        EntityKind::Circle(Circle2::new(DVec2::new(5.0, 5.0), 10.0)),
    );
    h.cmd("DRA");
    assert_eq!(h.ed.tool_accepts(), Accept::PICK);
    h.click(15.02, 5.01).hover(30.0, 30.0);
    let preview = h.ed.preview.ghosts[0].kind.clone();
    h.click(30.0, 30.0);
    let d = dimension(&h, 0);
    near(dimgen::measure(&d.kind), 10.0);
    near(d.text_pos.unwrap().distance(DVec2::new(30.0, 30.0)), 0.0);
    assert_eq!(h.of_type("DIMENSION")[0].1, preview);
    undo_redo(&mut h, "DIMRADIUS");
}

#[test]
fn diameter_arc_pick_uses_actual_sweep_and_can_retry_a_miss() {
    let mut h = harness();
    let arc = Arc2::new(DVec2::new(20.0, 0.0), 5.0, 0.0, FRAC_PI_2);
    add(&mut h, EntityKind::Arc(arc));
    h.cmd("DDI").click(15.0, 0.0);
    assert_error(&h);
    assert_eq!(h.ed.tool_accepts(), Accept::PICK);
    let mid = arc.mid_point();
    h.click(mid.x + 0.01, mid.y).cmd("35,15");
    let d = dimension(&h, 0);
    near(dimgen::measure(&d.kind), 10.0);
    let DimKind::Diameter { center, point } = d.kind else {
        panic!()
    };
    assert_eq!(center, arc.c);
    assert!(arc.closest(point).1.distance(point) < 1e-9);
    assert_eq!(d.text_pos, Some(DVec2::new(35.0, 15.0)));
    undo_redo(&mut h, "DIMDIAMETER");
}

#[test]
fn radial_pick_distinguishes_bulged_and_straight_segments() {
    let mut h = harness();
    add(
        &mut h,
        EntityKind::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::ZERO, 1.0),
                PolyVertex::new(DVec2::new(10.0, 0.0)),
                PolyVertex::new(DVec2::new(20.0, 0.0)),
            ],
            closed: false,
        }),
    );
    h.cmd("DRA").click(15.0, 0.0);
    assert_error(&h);
    h.click(5.0, -5.0).cmd("5,-12");
    near(dimgen::measure(&dimension(&h, 0).kind), 5.0);
}

#[test]
fn angular_three_points_can_place_minor_or_reflex_sector() {
    for (position, angle) in [("5,5", FRAC_PI_2), ("-5,-5", 3.0 * FRAC_PI_2)] {
        let mut h = harness();
        h.cmd("DAN")
            .cmd("P")
            .cmd("0,0")
            .cmd("10,0")
            .cmd("0,10")
            .cmd(position);
        let d = dimension(&h, 0);
        near(dimgen::measure(&d.kind), angle);
        let DimKind::Angular { arc_point, .. } = d.kind else {
            panic!()
        };
        near(arc_point.length(), 50f64.sqrt());
        undo_redo(&mut h, "DIMANGULAR");
    }
}

#[test]
fn angular_line_picks_use_intersection_and_cursor_sector() {
    let mut h = harness();
    add(
        &mut h,
        EntityKind::Line(Line2::new(DVec2::new(-10.0, 0.0), DVec2::new(10.0, 0.0))),
    );
    add(
        &mut h,
        EntityKind::Line(Line2::new(DVec2::new(-10.0, -10.0), DVec2::new(10.0, 10.0))),
    );
    h.cmd("DAN")
        .click(8.0, 0.01)
        .click(8.0, 8.01)
        .hover(-3.0, 4.0);
    let preview = h.ed.preview.ghosts[0].kind.clone();
    h.click(-3.0, 4.0);
    let d = dimension(&h, 0);
    near(dimgen::measure(&d.kind), 3.0 * FRAC_PI_4);
    let DimKind::Angular { vertex, .. } = d.kind else {
        panic!()
    };
    assert!(vertex.length() < 1e-8);
    assert_eq!(h.of_type("DIMENSION")[0].1, preview);
}

#[test]
fn angular_arc_preserves_its_sweep_even_when_label_is_outside() {
    let mut h = harness();
    let a = Arc2::new(DVec2::ZERO, 10.0, PI, FRAC_PI_2);
    add(&mut h, EntityKind::Arc(a));
    let p = a.mid_point();
    h.cmd("DAN").click(p.x, p.y).cmd("20,20");
    near(dimgen::measure(&dimension(&h, 0).kind), 3.0 * FRAC_PI_2);
}

#[test]
fn angular_parallel_line_retries_without_losing_first_pick() {
    let mut h = harness();
    for line in [
        Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0)),
        Line2::new(DVec2::new(0.0, 3.0), DVec2::new(10.0, 3.0)),
        Line2::new(DVec2::ZERO, DVec2::new(0.0, 10.0)),
    ] {
        add(&mut h, EntityKind::Line(line));
    }
    h.cmd("DAN").click(5.0, 0.0).click(5.0, 3.0);
    assert_error(&h);
    h.click(0.0, 5.0).cmd("5,5");
    near(dimgen::measure(&dimension(&h, 0).kind), FRAC_PI_2);
}

#[test]
fn ordinate_origin_axis_leader_and_zero_value() {
    for (axis, value) in [("X", 30.0), ("Y", 40.0)] {
        let mut h = harness();
        h.cmd("DOR")
            .cmd("O")
            .cmd("10,20")
            .cmd(axis)
            .cmd("40,60")
            .cmd("70,90");
        let d = dimension(&h, 0);
        near(dimgen::measure(&d.kind), value);
        let DimKind::Ordinate {
            origin,
            point,
            leader_end,
            x_axis,
        } = d.kind
        else {
            panic!()
        };
        assert_eq!(origin, DVec2::new(10.0, 20.0));
        assert_eq!(point, DVec2::new(40.0, 60.0));
        assert_eq!(leader_end, DVec2::new(70.0, 90.0));
        assert_eq!(x_axis, axis == "X");
        undo_redo(&mut h, "DIMORDINATE");
    }
    let mut h = harness();
    h.cmd("DOR").cmd("0,0").cmd("0,10");
    near(dimgen::measure(&dimension(&h, 0).kind), 0.0);
}

#[test]
fn custom_dimension_text_position_previews_before_single_commit() {
    for (name, inputs, place) in [
        ("DLI", vec!["0,0", "30,40"], "15,60"),
        ("DAL", vec!["0,0", "30,40"], "15,60"),
        ("DAN", vec!["P", "0,0", "10,0", "0,10"], "5,5"),
        ("DOR", vec!["10,20"], "10,50"),
    ] {
        let mut h = harness();
        h.cmd(name);
        for s in inputs {
            h.cmd(s);
        }
        h.cmd("TP").cmd(place).hover(70.0, 80.0);
        assert_eq!(h.count("DIMENSION"), 0);
        let EntityKind::Dimension(preview) = &h.ed.preview.ghosts[0].kind else {
            panic!()
        };
        assert_eq!(preview.text_pos, Some(DVec2::new(70.0, 80.0)));
        h.click(70.0, 80.0);
        let d = dimension(&h, 0);
        assert_eq!(d.text_pos, Some(DVec2::new(70.0, 80.0)));
        assert_eq!(h.ed.doc.drawing.entities.len(), 1);
    }
}

#[test]
fn dimension_degenerate_inputs_retry_and_escape_never_creates_geometry() {
    let mut h = harness();
    h.cmd("DAL").cmd("0,0").cmd("0,0");
    assert_error(&h);
    h.cmd("3,4").hover(10.0, 10.0).esc();
    assert!(h.ed.preview.is_empty());
    assert!(h.ed.doc.drawing.entities.is_empty());
    assert_eq!(h.ed.doc.undo(), None);
    h.cmd("DLI").cmd("0,0").cmd("0,10").cmd("H").cmd("5,5");
    assert_error(&h);
    h.cmd("V").cmd("5,5");
    near(dimgen::measure(&dimension(&h, 0).kind), 10.0);
    h.cmd("DAN").enter().cmd("0,0").cmd("0,0");
    assert_error(&h);
    h.cmd("10,0").cmd("20,0");
    assert_error(&h);
    h.cmd("0,10").cmd("0,0");
    assert_error(&h);
    h.esc();
    assert_eq!(h.count("DIMENSION"), 1);
    h.cmd("DOR").cmd("1,2").cmd("1,2");
    assert_error(&h);
    h.esc();
}

#[test]
fn dimensions_recheck_current_layer_and_reject_locked_pick_sources() {
    let mut h = harness();
    h.cmd("DLI").cmd("0,0").cmd("10,0");
    lock_current(&mut h, true);
    h.cmd("5,5");
    assert_error(&h);
    assert_eq!(h.count("DIMENSION"), 0);
    lock_current(&mut h, false);
    h.cmd("5,5");
    assert_eq!(h.count("DIMENSION"), 1);

    let mut h = harness();
    add(&mut h, EntityKind::Circle(Circle2::new(DVec2::ZERO, 10.0)));
    lock_current(&mut h, true);
    h.cmd("DRA").click(10.0, 0.0);
    assert_error(&h);
    assert_eq!(h.ed.tool_accepts(), Accept::PICK);
    lock_current(&mut h, false);
    h.click(10.0, 0.0).cmd("20,20");
    assert_eq!(h.count("DIMENSION"), 1);
}

#[test]
fn annotation_entities_are_created_on_the_current_layer() {
    let mut h = harness();
    let layer = h.ed.doc.transact("layer", |tx| {
        let id = tx.ids().layer();
        let mut layer = tx.drawing().tables.layers.values().next().unwrap().clone();
        layer.name = "Annotations".into();
        tx.tables_mut().layers.insert(id, layer);
        tx.tables_mut().current_layer = id;
        id
    });
    h.cmd("TEXT").cmd("0,0").enter().enter().cmd("note");
    h.cmd("MTEXT").cmd("0,0").enter().enter().cmd("note");
    h.cmd("DAL").cmd("0,0").cmd("10,0").cmd("0,10");
    select_curves(&mut h, &[square(10.0)]);
    h.cmd("H").enter();
    assert_eq!(h.count("HATCH"), 1);
    assert!(h.ed.doc.drawing.entities.values().all(|e| e.layer == layer));
}

#[test]
fn hatch_solid_preserves_circle_ellipse_and_nested_hole_island() {
    let mut h = harness();
    let curves = vec![
        square(10.0),
        Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0)),
        Curve2::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(3.0, 0.0),
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        }),
    ];
    select_curves(&mut h, &curves);
    h.cmd("H");
    assert_eq!(h.count("HATCH"), 0);
    assert_eq!(h.ed.preview.ghosts.len(), 1);
    let preview = h.ed.preview.ghosts[0].kind.clone();
    h.enter();
    let fill = hatch(&h);
    assert_eq!(
        fill.loops
            .iter()
            .map(|l| l.curves[0].clone())
            .collect::<Vec<_>>(),
        curves
    );
    assert!(fill.is_solid());
    assert_eq!(h.of_type("HATCH")[0].1, preview);
    let loops: Vec<_> = fill.loops.iter().map(|l| l.curves.as_slice()).collect();
    let mesh = wcad_geom2d::tess::fill_loops(&loops, wcad_geom2d::tess::FillRule::EvenOdd, 1e-4);
    assert!((wcad_geom2d::tess::mesh_area(&mesh) - (400.0 - 25.0 * PI + 4.5 * PI)).abs() < 0.02);
    assert!(h.ed.selection.is_empty());
    undo_redo(&mut h, "HATCH");
}

#[test]
fn ansi31_lines_do_not_cross_a_circular_hole() {
    let mut h = harness();
    select_curves(
        &mut h,
        &[square(10.0), Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0))],
    );
    h.cmd("HATCH")
        .cmd("ANSI31")
        .cmd("SC")
        .cmd("0.25")
        .cmd("AN")
        .cmd("15")
        .enter();
    let fill = hatch(&h);
    near(fill.pattern.scale, 0.25);
    near(fill.pattern.angle, PI / 12.0);
    let refs: Vec<_> = fill.loops.iter().map(|l| l.curves.as_slice()).collect();
    let lines = wcad_geom2d::hatch::hatch_lines(
        &refs,
        &wcad_geom2d::hatch::builtin("ANSI31").unwrap(),
        fill.pattern.scale,
        fill.pattern.angle,
    );
    assert!(!lines.is_empty());
    for l in lines {
        assert!(l.midpoint().length() >= 5.0 - 1e-3);
        assert!(l.midpoint().abs().max_element() <= 10.0 + 1e-8);
    }
}

#[test]
fn hatch_line_arc_chain_closes_without_adding_or_distorting_edges() {
    let curves = vec![
        Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0))),
        Curve2::Arc(Arc2::new(DVec2::new(10.0, 2.0), 2.0, -FRAC_PI_2, FRAC_PI_2)),
        Curve2::Line(Line2::new(DVec2::new(0.0, 4.0), DVec2::new(10.0, 4.0))),
        Curve2::Arc(Arc2::new(
            DVec2::new(0.0, 2.0),
            2.0,
            FRAC_PI_2,
            3.0 * FRAC_PI_2,
        )),
    ];
    let loops = boundary_loops(&curves).unwrap();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].curves.len(), 4);
    near(
        wcad_geom2d::regions::signed_area(&loops[0].curves).abs(),
        40.0 + 4.0 * PI,
    );
    let chain = &loops[0].curves;
    for i in 0..chain.len() {
        assert!(
            chain[i]
                .end()
                .distance(chain[(i + 1) % chain.len()].start())
                < 1e-8
        );
    }
}

#[test]
fn hatch_open_mixed_partial_ellipse_and_self_intersecting_boundaries_fail() {
    let open = Curve2::Polyline(Polyline2::from_points(
        [DVec2::ZERO, DVec2::X, DVec2::ONE],
        false,
    ));
    let bow = Curve2::Polyline(Polyline2::from_points(
        [DVec2::ZERO, DVec2::ONE, DVec2::Y, DVec2::X],
        true,
    ));
    let arc = Curve2::Ellipse(EllipseArc2 {
        c: DVec2::ZERO,
        major: DVec2::X,
        ratio: 0.5,
        start: 0.0,
        end: PI,
    });
    for curves in [
        vec![open.clone()],
        vec![square(10.0), open],
        vec![bow],
        vec![arc],
        vec![Curve2::Line(Line2::new(DVec2::ZERO, DVec2::X))],
    ] {
        assert_eq!(boundary_loops(&curves), Err(BoundaryError::Invalid));
    }
}

#[test]
fn hatch_does_not_bridge_small_visible_gaps_or_discard_dangling_edges() {
    let mut curves = vec![
        Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0))),
        Curve2::Line(Line2::new(DVec2::new(10.0, 0.0), DVec2::new(10.0, 10.0))),
        Curve2::Line(Line2::new(DVec2::new(10.0, 10.0), DVec2::new(0.0, 10.0))),
        Curve2::Line(Line2::new(DVec2::new(0.0, 10.0), DVec2::new(0.0, 0.001))),
    ];
    assert!(boundary_loops(&curves).is_err());
    curves[3] = Curve2::Line(Line2::new(DVec2::new(0.0, 10.0), DVec2::ZERO));
    assert!(boundary_loops(&curves).is_ok());
    curves.push(Curve2::Line(Line2::new(DVec2::ZERO, DVec2::new(-5.0, 0.0))));
    assert!(boundary_loops(&curves).is_err());
}

#[test]
fn hatch_bad_boundary_selection_can_be_retried_in_same_command() {
    let mut h = harness();
    let bad = add(&mut h, EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::X)));
    let good = add(&mut h, EntityKind::Circle(Circle2::new(DVec2::ZERO, 5.0)));
    h.cmd("H").enter();
    assert_error(&h);
    h.ed.feed(ToolInput::Selection(vec![bad, good]));
    assert_error(&h);
    assert_eq!(h.ed.tool_accepts(), Accept::SELECTION);
    h.ed.feed(ToolInput::Selection(vec![good, good]));
    h.enter();
    assert_eq!(hatch(&h).loops.len(), 1);
}

#[test]
fn hatch_invalid_scale_density_and_angle_are_retryable() {
    let mut h = harness();
    select_curves(&mut h, &[square(100.0)]);
    h.cmd("H").cmd("ANSI31").cmd("SC").cmd("0");
    assert_error(&h);
    h.cmd("-1");
    assert_error(&h);
    h.cmd("1e-8").enter();
    assert_error(&h);
    assert_eq!(h.count("HATCH"), 0);
    h.cmd("SC").cmd("1").cmd("AN").cmd("NaN");
    assert_error(&h);
    h.cmd("-45").enter();
    near(hatch(&h).pattern.angle, 7.0 * FRAC_PI_4);
}

#[test]
fn hatch_cancel_reselect_and_redo_leave_only_confirmed_entities() {
    let mut h = harness();
    let ids = select_curves(&mut h, &[square(10.0)]);
    let original = h.ed.doc.drawing.entities.clone();
    h.cmd("H").cmd("ANSI31").cmd("SC").cmd("2").esc();
    assert!(h.ed.preview.is_empty());
    assert!(h.ed.selection.is_empty());
    assert_eq!(h.ed.doc.drawing.entities, original);
    assert_eq!(h.ed.doc.undo().as_deref(), Some("fixture"));
    h.ed.doc.redo();
    h.ed.pump();
    h.ed.selection.set(ids.iter().copied());
    h.cmd("H").cmd("R");
    assert_eq!(h.ed.tool_accepts(), Accept::SELECTION);
    assert!(h.ed.preview.is_empty());
    h.ed.feed(ToolInput::Selection(ids));
    h.enter();
    undo_redo(&mut h, "HATCH");
}

#[test]
fn hatch_revalidates_sources_and_never_omits_locked_holes() {
    let mut h = harness();
    let ids = select_curves(
        &mut h,
        &[square(10.0), Curve2::Circle(Circle2::new(DVec2::ZERO, 5.0))],
    );
    h.cmd("H");
    lock_current(&mut h, true);
    h.enter();
    assert_error(&h);
    assert_eq!(h.count("HATCH"), 0);
    assert_eq!(h.ed.tool_accepts(), Accept::SELECTION);
    lock_current(&mut h, false);
    h.ed.feed(ToolInput::Selection(ids.clone()));
    h.ed.doc.transact("change", |tx| {
        tx.modify(ids[1], |e| {
            e.kind = EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::X))
        });
    });
    h.ed.pump();
    h.enter();
    assert_error(&h);
    assert_eq!(h.count("HATCH"), 0);
    h.ed.feed(ToolInput::Selection(vec![ids[0]]));
    h.enter();
    assert_eq!(hatch(&h).loops.len(), 1);
}

#[test]
fn boundary_validation_rejects_nonfinite_degenerate_and_excessive_inputs() {
    for c in [
        Curve2::Circle(Circle2::new(DVec2::ZERO, -1.0)),
        Curve2::Circle(Circle2::new(DVec2::splat(f64::NAN), 1.0)),
        Curve2::Circle(Circle2::new(DVec2::ZERO, f64::INFINITY)),
        Curve2::Circle(Circle2::new(DVec2::ZERO, 1e200)),
        Curve2::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::ZERO, 1e200),
                PolyVertex::new(DVec2::X),
            ],
            closed: true,
        }),
        Curve2::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::X, DVec2::new(2.0, 0.0)],
            true,
        )),
    ] {
        assert!(boundary_loops(&[c]).is_err());
    }
    assert_eq!(
        boundary_loops(&vec![Curve2::Circle(Circle2::new(DVec2::ZERO, 1.0)); 1025]),
        Err(BoundaryError::Limit)
    );
}

#[test]
fn tools_reject_nonfinite_inputs_even_without_editor_filtering() {
    let mut tools: Vec<Box<dyn Tool>> = vec![
        Box::new(TextTool::new(false)),
        Box::new(TextTool::new(true)),
        Box::new(HatchTool::default()),
    ];
    for mode in [
        DimMode::Linear,
        DimMode::Aligned,
        DimMode::Radius,
        DimMode::Diameter,
        DimMode::Angular,
        DimMode::Ordinate,
    ] {
        tools.push(Box::new(DimensionTool::new(mode)));
    }
    for mut tool in tools {
        let mut h = harness();
        let before = tool.prompt(Lang::En);
        for input in [
            ToolInput::Point(DVec2::splat(f64::NAN)),
            ToolInput::Point(DVec2::splat(1e200)),
            ToolInput::Value(f64::INFINITY),
            ToolInput::Hover(DVec2::splat(f64::NEG_INFINITY)),
        ] {
            let ed = &mut h.ed;
            let mut cx = ToolCx {
                doc: &mut ed.doc,
                selection: &mut ed.selection,
                draft: &ed.draft,
                index: &ed.index,
                log: &mut ed.log,
                requests: &mut ed.requests,
                lang: Lang::En,
                cursor: None,
                last_point: None,
                units_per_px: 0.01,
            };
            assert_eq!(tool.on_input(input, &mut cx), ToolFlow::Continue);
            assert_eq!(tool.prompt(Lang::En), before);
        }
        assert!(h.ed.doc.drawing.entities.is_empty());
        assert_eq!(h.ed.doc.undo(), None);
    }
}

#[test]
fn dimensions_cancel_at_pick_placement_and_custom_text_steps() {
    for (command, inputs) in [
        ("DLI", vec!["0,0", "30,40"]),
        ("DAL", vec!["0,0", "30,40", "TP", "15,60"]),
        ("DRA", vec!["10,0"]),
        ("DDI", vec!["10,0"]),
        ("DAN", vec!["P", "0,0", "10,0", "0,10"]),
        ("DOR", vec!["10,20", "TP", "10,50"]),
    ] {
        for n in 0..=inputs.len() {
            let mut h = harness();
            add(&mut h, EntityKind::Circle(Circle2::new(DVec2::ZERO, 10.0)));
            let before = h.ed.doc.drawing.entities.clone();
            h.cmd(command);
            for input in &inputs[..n] {
                h.cmd(input);
            }
            h.hover(60.0, 70.0).esc();
            assert_eq!(h.ed.doc.drawing.entities, before);
            assert!(h.ed.preview.is_empty());
            assert_eq!(h.ed.doc.undo().as_deref(), Some("fixture"));
        }
    }
}

#[test]
fn diameter_text_can_be_at_circle_center() {
    let mut h = harness();
    add(&mut h, EntityKind::Circle(Circle2::new(DVec2::ZERO, 20.0)));
    h.cmd("DDI").click(20.0, 0.0).cmd("0,0");
    let d = dimension(&h, 0);
    near(dimgen::measure(&d.kind), 40.0);
    assert_eq!(d.text_pos, Some(DVec2::ZERO));
}

#[test]
fn missing_dimension_style_can_be_repaired_and_retried() {
    let mut h = harness();
    let current = h.ed.doc.drawing.tables.current_dim_style;
    h.ed.doc.drawing.tables.current_dim_style = wcad_doc::DimStyleId(u32::MAX);
    h.cmd("DAL").cmd("0,0").cmd("30,40").cmd("0,20");
    assert_error(&h);
    assert_eq!(h.count("DIMENSION"), 0);
    h.ed.doc.drawing.tables.current_dim_style = current;
    h.cmd("0,20");
    assert_eq!(dimension(&h, 0).style, current);
}

#[test]
fn hatch_bulged_polyline_hole_preserves_exact_curves() {
    let hole = Curve2::Polyline(Polyline2 {
        verts: vec![
            PolyVertex::with_bulge(DVec2::new(-3.0, 0.0), -1.0),
            PolyVertex::with_bulge(DVec2::new(3.0, 0.0), -1.0),
        ],
        closed: true,
    });
    let loops = boundary_loops(&[square(10.0), hole.clone()]).unwrap();
    assert_eq!(loops[1].curves, vec![hole]);
    near(
        wcad_geom2d::regions::signed_area(&loops[1].curves),
        -9.0 * PI,
    );
    let refs: Vec<_> = loops.iter().map(|l| l.curves.as_slice()).collect();
    let mesh = wcad_geom2d::tess::fill_loops(&refs, wcad_geom2d::tess::FillRule::EvenOdd, 1e-4);
    assert!((wcad_geom2d::tess::mesh_area(&mesh) - (400.0 - 9.0 * PI)).abs() < 0.02);
}

#[test]
fn hatch_locked_destination_does_not_commit_on_source_layer() {
    let mut h = harness();
    select_curves(&mut h, &[square(10.0)]);
    let layer = h.ed.doc.transact("layer", |tx| {
        let id = tx.ids().layer();
        let mut layer = tx.drawing().tables.layers.values().next().unwrap().clone();
        layer.name = "Fill".into();
        layer.locked = true;
        tx.tables_mut().layers.insert(id, layer);
        tx.tables_mut().current_layer = id;
        id
    });
    h.cmd("H").enter();
    assert_error(&h);
    assert_eq!(h.count("HATCH"), 0);
    lock_current(&mut h, false);
    h.enter();
    let id = h.of_type("HATCH")[0].0;
    assert_eq!(h.ed.doc.drawing.entities[&id].layer, layer);
}
