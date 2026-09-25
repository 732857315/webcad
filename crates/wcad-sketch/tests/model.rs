//! Model editing API: builders, validation, removal, serde.

use wcad_geom2d::Curve2;
use wcad_math::DVec2;
use wcad_sketch::{ArcEnd, ConstraintKind as K, DimValue, Error, SkConstraintId, SkEntityId, SkGeom, Sketch};

fn v(x: f64, y: f64) -> DVec2 {
    DVec2::new(x, y)
}

#[test]
fn rectangle_builder_shares_points() {
    let mut sk = Sketch::new();
    let r = sk.add_rectangle(v(1.0, 1.0), v(5.0, 3.0));
    assert_eq!(sk.entities.len(), 8);
    assert_eq!(sk.constraints.len(), 4);
    for i in 0..4 {
        let (a, b) = sk.line_ends(r.lines[i]).unwrap();
        assert_eq!(a, r.corners[i]);
        assert_eq!(b, r.corners[(i + 1) % 4]);
    }
    assert_eq!(sk.point(r.corners[2]), Some(v(5.0, 3.0)));
    assert!(sk.solve().converged);
    let d = sk.diagnose();
    // 8 params, 4 equations: position (2) + width + height
    assert_eq!(d.dof, 4);
    assert!(d.groups.is_empty());
    // closed profile of 4 lines
    let curves = sk.curves();
    assert_eq!(curves.len(), 4);
    assert!(curves.iter().all(|(_, c)| matches!(c, Curve2::Line(_))));
}

#[test]
fn arc_builders_are_ccw() {
    let mut sk = Sketch::new();
    // clockwise input order p0 -> p1 -> p2 is stored CCW (ends swapped)
    let cw = sk.add_arc_3points(v(0.0, 1.0), v(1.0, 0.0), v(0.0, -1.0)).unwrap();
    let ccw = sk.add_arc_3points(v(0.0, -1.0), v(1.0, 0.0), v(0.0, 1.0)).unwrap();
    for id in [cw, ccw] {
        let Some(Curve2::Arc(a)) = sk.curve_of(&sk.entity(id).unwrap().geom) else { panic!("arc") };
        assert!(a.c.distance(v(0.0, 0.0)) < 1e-12);
        assert!((a.r - 1.0).abs() < 1e-12);
        // the arc passes through (1, 0): the right half, sweep π
        assert!((a.sweep() - std::f64::consts::PI).abs() < 1e-9, "{a:?}");
        assert!(a.mid_point().distance(v(1.0, 0.0)) < 1e-9);
    }
    assert!(matches!(sk.add_arc_3points(v(0.0, 0.0), v(1.0, 1.0), v(2.0, 2.0)), Err(Error::Degenerate(_))));
    // center/start/end projects the end onto the circle
    let a = sk.add_arc_center_start_end(v(0.0, 0.0), v(2.0, 0.0), v(0.0, 5.0)).unwrap();
    let [_, _, e] = sk.arc_points(a).unwrap();
    assert!(sk.point(e).unwrap().distance(v(0.0, 2.0)) < 1e-12);
    assert!(sk.add_arc_center_start_end(v(0.0, 0.0), v(0.0, 0.0), v(1.0, 0.0)).is_err());
    let c = sk.add_circle_center_radius(v(3.0, 4.0), 2.5);
    assert_eq!(sk.radius_of(c), Some(2.5));
    assert_eq!(sk.point(sk.center_of(c).unwrap()), Some(v(3.0, 4.0)));
}

#[test]
fn checked_constraints_reject_wrong_types() {
    let mut sk = Sketch::new();
    let l = sk.add_line_points(v(0.0, 0.0), v(1.0, 0.0));
    let (a, b) = sk.line_ends(l).unwrap();
    let c = sk.add_circle_center_radius(v(0.0, 3.0), 1.0);
    let arc = sk.add_arc_center_start_end(v(5.0, 0.0), v(6.0, 0.0), v(5.0, 1.0)).unwrap();
    let bad = |sk: &mut Sketch, k: K| matches!(sk.add_constraint_checked(k), Err(Error::BadConstraint(_)));
    assert!(bad(&mut sk, K::Horizontal { line: a }));
    assert!(bad(&mut sk, K::Coincident { p1: a, p2: l }));
    assert!(bad(&mut sk, K::Coincident { p1: a, p2: a }));
    assert!(bad(&mut sk, K::Radius { round: l, value: DimValue::new(1.0) }));
    assert!(bad(&mut sk, K::Radius { round: c, value: DimValue::new(-1.0) }));
    assert!(bad(&mut sk, K::Distance { p1: a, p2: b, value: DimValue::new(f64::NAN) }));
    assert!(bad(&mut sk, K::TangentArcLine { arc: c, end: ArcEnd::Start, line: l })); // circle is not an arc
    assert!(bad(&mut sk, K::Midpoint { p: a, line: l }));
    assert!(bad(&mut sk, K::Parallel { line1: l, line2: l }));
    assert!(matches!(
        sk.add_constraint_checked(K::Fix { p: SkEntityId(1234) }),
        Err(Error::UnknownEntity(SkEntityId(1234)))
    ));
    assert!(sk.constraints.is_empty());
    // valid ones pass; arcs are rounds
    sk.add_constraint_checked(K::EqualRadius { round1: c, round2: arc }).unwrap();
    sk.add_constraint_checked(K::TangentArcLine { arc, end: ArcEnd::End, line: l }).unwrap();
    sk.add_constraint_checked(K::PointOnCircle { p: a, round: arc }).unwrap();
    assert!(matches!(sk.add_reference_dimension(K::Horizontal { line: l }), Err(Error::BadConstraint(_))));
    assert!(sk.set_driving(SkConstraintId(0), false).is_err()); // EqualRadius is not a dimension
    assert!(matches!(sk.set_enabled(SkConstraintId(99), false), Err(Error::UnknownConstraint(_))));
}

#[test]
fn remove_entity_cascades() {
    let mut sk = Sketch::new();
    let r = sk.add_rectangle(v(0.0, 0.0), v(4.0, 2.0));
    let lone = sk.add_point(v(9.0, 9.0));
    let on = sk.add_constraint(K::PointOnLine { p: lone, line: r.lines[0] });
    let fix = sk.add_constraint(K::Fix { p: r.corners[0] });

    // removing a line removes its constraints but keeps corners shared with other lines
    let removed = sk.remove_entity(r.lines[0]).unwrap();
    assert_eq!(removed.entities, vec![r.lines[0]]);
    assert_eq!(removed.constraints, vec![r.constraints[0], on]);
    assert!(sk.point(r.corners[0]).is_some() && sk.point(r.corners[1]).is_some());
    assert!(sk.constraint(fix).is_some());

    // removing a corner point removes the lines using it; now-orphaned points go too
    let removed = sk.remove_entity(r.corners[0]).unwrap();
    assert!(removed.entities.contains(&r.lines[3]));
    assert!(removed.entities.contains(&r.corners[0]));
    assert!(removed.constraints.contains(&fix));
    assert!(sk.point(r.corners[3]).is_some()); // still used by the top line
    assert!(sk.point(lone).is_some());
    assert!(matches!(sk.remove_entity(r.corners[0]), Err(Error::UnknownEntity(_))));
    // every remaining constraint references existing entities
    for c in sk.constraints.values() {
        sk.validate_constraint(&c.kind).unwrap();
    }
    // ids are never reused
    let p = sk.add_point(v(0.0, 0.0));
    assert!(p > lone);
}

#[test]
fn serde_round_trip_preserves_everything() {
    let mut sk = Sketch::new();
    let r = sk.add_rectangle(v(0.0, 0.0), v(4.0, 2.0));
    sk.add_constraint(K::Fix { p: r.corners[0] });
    let w = sk.add_constraint(K::Distance {
        p1: r.corners[0],
        p2: r.corners[1],
        value: DimValue { value: 6.0, expr: Some("2*3".into()) },
    });
    let reference = sk.add_reference_dimension(K::Length { line: r.lines[1], value: DimValue::new(1.0) }).unwrap();
    let arc = sk.add_arc_3points(v(6.0, 0.0), v(7.0, 1.0), v(6.0, 2.0)).unwrap();
    sk.add_constraint(K::TangentArcArc { arc1: arc, end1: ArcEnd::Start, arc2: arc, end2: ArcEnd::End });
    sk.set_construction(r.lines[2], true).unwrap();
    sk.set_enabled(w, false).unwrap();
    let json = serde_json::to_string_pretty(&sk).unwrap();
    assert!(json.contains("\"type\": \"Distance\""), "{json}");
    assert!(json.contains("2*3"));
    let back: Sketch = serde_json::from_str(&json).unwrap();
    assert_eq!(back, sk);
    assert!(!back.constraint(reference).unwrap().driving);
    // counters survive: new ids continue after the old ones
    let mut back = back;
    let id = back.add_point(v(0.0, 0.0));
    assert!(sk.entities.keys().all(|&k| k < id));

    // older/minimal files: flags and counters default
    let minimal = r#"{
        "entities": { "3": { "geom": { "type": "Point", "p": [1.0, 2.0] } } },
        "constraints": { "7": { "kind": { "type": "Fix", "p": 3 } } }
    }"#;
    let mut m: Sketch = serde_json::from_str(minimal).unwrap();
    assert!(m.constraint(SkConstraintId(7)).unwrap().enabled);
    assert_eq!(m.entity(SkEntityId(3)).unwrap().geom, SkGeom::Point { p: v(1.0, 2.0) });
    // the counter is behind the stored ids: allocation must not collide
    let p = m.add_point(v(0.0, 0.0));
    assert_eq!(p, SkEntityId(4));
    let c = m.add_constraint(K::Fix { p });
    assert_eq!(c, SkConstraintId(8));
    assert!(m.solve().converged);
}

#[test]
fn corrupted_files_do_not_panic() {
    // a line whose endpoint is missing, a circle around a line, NaN coordinates
    let json = r#"{
        "entities": {
            "0": { "geom": { "type": "Point", "p": [0.0, 0.0] } },
            "1": { "geom": { "type": "Line", "a": 0, "b": 42 } },
            "2": { "geom": { "type": "Circle", "c": 1, "r": 2.0 } },
            "3": { "geom": { "type": "Point", "p": [1.0, 1.0] } },
            "4": { "geom": { "type": "Arc", "c": 0, "s": 3, "e": 9 } }
        },
        "constraints": {
            "0": { "kind": { "type": "Horizontal", "line": 1 } },
            "1": { "kind": { "type": "Radius", "round": 2, "value": { "value": 1.0 } } },
            "2": { "kind": { "type": "Distance", "p1": 0, "p2": 3, "value": { "value": 5.0 } } },
            "3": { "kind": { "type": "TangentArcLine", "arc": 4, "end": "Start", "line": 1 } }
        }
    }"#;
    let mut sk: Sketch = serde_json::from_str(json).unwrap();
    let rep = sk.solve();
    assert_eq!(rep.invalid, vec![SkConstraintId(0), SkConstraintId(1), SkConstraintId(3)]);
    assert!(rep.converged);
    assert!((sk.point(SkEntityId(3)).unwrap().distance(sk.point(SkEntityId(0)).unwrap()) - 5.0).abs() < 1e-9);
    let d = sk.diagnose();
    assert_eq!(d.invalid.len(), 3);
    assert!(sk.curves().is_empty());
    let _ = sk.drag(SkEntityId(1), v(1.0, 1.0));
    let _ = sk.drag_curve(SkEntityId(1), v(1.0, 1.0));
    let _ = sk.drag_curve(SkEntityId(4), v(1.0, 1.0));
    assert!(sk.measure(SkConstraintId(3)).is_nan() && sk.measure(SkConstraintId(99)).is_nan());

    // NaN in the geometry: solve reports failure instead of panicking or spreading NaN
    let mut sk = Sketch::new();
    let a = sk.add_point(v(f64::NAN, 0.0));
    let b = sk.add_point(v(1.0, 1.0));
    sk.add_constraint(K::Distance { p1: a, p2: b, value: DimValue::new(2.0) });
    let rep = sk.solve();
    assert!(!rep.converged);
    assert!(sk.point(b).unwrap().is_finite());
    let _ = sk.diagnose();
}

#[test]
fn scattered_constraint_order_still_solves() {
    // All geometry first, constraints afterwards in a scrambled order: the natural equation order
    // has a wide envelope, the solver reorders (RCM) and must give the same answer.
    let n = 40;
    let mut sk = Sketch::new();
    let pts: Vec<SkEntityId> = (0..=n).map(|i| sk.add_point(v(i as f64 * 1.1, 0.3 * (i % 3) as f64))).collect();
    let lines: Vec<SkEntityId> = (0..n).map(|i| sk.add_line(pts[i], pts[i + 1])).collect();
    let mut order: Vec<usize> = (0..n).collect();
    let mut s: u64 = 5;
    for i in (1..n).rev() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        order.swap(i, (s >> 33) as usize % (i + 1));
    }
    sk.add_constraint(K::Fix { p: pts[0] });
    for &i in &order {
        sk.add_constraint(K::Length { line: lines[i], value: DimValue::new(1.0) });
        if i > 0 {
            sk.add_constraint(K::Angle { line1: lines[i - 1], line2: lines[i], value: DimValue::new(0.05) });
        }
    }
    sk.add_constraint(K::Horizontal { line: lines[0] });
    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    // a polyline of unit segments turning 0.05 rad each: compare with the closed form
    let mut p = v(0.0, 0.0);
    let mut ang: f64 = 0.0;
    for i in 0..n {
        p += v(ang.cos(), ang.sin());
        ang += 0.05;
        let got = sk.point(pts[i + 1]).unwrap();
        assert!(got.distance(p) < 1e-7, "point {i}: {got:?} vs {p:?}");
    }
    let d = sk.diagnose();
    assert_eq!(d.dof, 0);
    assert!(d.groups.is_empty(), "{:?}", d.groups);
}
