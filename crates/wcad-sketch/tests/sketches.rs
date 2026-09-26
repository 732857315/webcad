//! Solver behaviour on whole sketches (ported from the prototype's tests to the model API).

use wcad_math::DVec2;
use wcad_sketch::{
    ArcEnd, ConstraintKind as K, DimValue, EntityStatus, SkConstraintId, SkEntityId, Sketch,
};

fn v(x: f64, y: f64) -> DVec2 {
    DVec2::new(x, y)
}

fn dim(x: f64) -> DimValue {
    DimValue::new(x)
}

fn pt(sk: &Sketch, p: SkEntityId) -> DVec2 {
    sk.point(p).expect("point")
}

#[track_caller]
fn assert_pt(sk: &Sketch, p: SkEntityId, want: DVec2, tol: f64) {
    let got = pt(sk, p);
    assert!(
        got.distance(want) <= tol,
        "point {p:?}: got {got:?} want {want:?}"
    );
}

fn add(sk: &mut Sketch, k: K) -> SkConstraintId {
    sk.add_constraint_checked(k).expect("valid constraint")
}

/// Put the point at (x, y) and fix it there (the stored position is the anchor).
fn fix_at(sk: &mut Sketch, p: SkEntityId, x: f64, y: f64) -> SkConstraintId {
    sk.set_point(p, v(x, y)).expect("point");
    add(sk, K::Fix { p })
}

fn ends(sk: &Sketch, l: SkEntityId) -> (SkEntityId, SkEntityId) {
    sk.line_ends(l).expect("line")
}

/// 4 lines with own endpoints, corners coincident, bottom/top horizontal, left/right vertical.
/// corners[i] = start point of lines[i] (bottom-left, bottom-right, top-right, top-left).
struct Rect {
    lines: [SkEntityId; 4],
    corners: [SkEntityId; 4],
}

fn rect(sk: &mut Sketch, x0: f64, y0: f64, w: f64, h: f64, jit: f64) -> Rect {
    let c = [v(x0, y0), v(x0 + w, y0), v(x0 + w, y0 + h), v(x0, y0 + h)];
    let mut lines = [SkEntityId(0); 4];
    for i in 0..4 {
        let (a, b) = (c[i], c[(i + 1) % 4]);
        let j = jit * (i as f64 + 1.0);
        lines[i] = sk.add_line_points(v(a.x + j, a.y - j), v(b.x - j, b.y + 0.5 * j));
    }
    for i in 0..4 {
        let end = ends(sk, lines[i]).1;
        let next = ends(sk, lines[(i + 1) % 4]).0;
        add(sk, K::Coincident { p1: end, p2: next });
    }
    add(sk, K::Horizontal { line: lines[0] });
    add(sk, K::Horizontal { line: lines[2] });
    add(sk, K::Vertical { line: lines[1] });
    add(sk, K::Vertical { line: lines[3] });
    let corners = [0, 1, 2, 3].map(|i| ends(sk, lines[i]).0);
    Rect { lines, corners }
}

fn arc_radius(sk: &Sketch, arc: SkEntityId) -> f64 {
    sk.radius_of(arc).expect("arc")
}

#[test]
fn fully_constrained_rectangle_dof0() {
    let mut sk = Sketch::new();
    let r = rect(&mut sk, 0.3, -0.2, 9.0, 5.5, 0.1);
    let [p0, p1, p2, p3] = r.corners;
    fix_at(&mut sk, p0, 0.0, 0.0);
    add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2: p1,
            value: dim(10.0),
        },
    );
    add(
        &mut sk,
        K::Distance {
            p1,
            p2,
            value: dim(5.0),
        },
    );
    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    assert_pt(&sk, p0, v(0.0, 0.0), 1e-9);
    assert_pt(&sk, p1, v(10.0, 0.0), 1e-9);
    assert_pt(&sk, p2, v(10.0, 5.0), 1e-9);
    assert_pt(&sk, p3, v(0.0, 5.0), 1e-9);
    let d = sk.diagnose();
    assert_eq!(d.n_params, 14); // 8 points, one fixed
    assert_eq!(d.dof, 0, "{d:?}");
    assert!(d.groups.is_empty(), "{:?}", d.groups);
    assert!(d.free_params.is_empty());
    assert!(d.is_well_constrained());
    for l in r.lines {
        assert_eq!(d.status(l), Some(EntityStatus::FullyConstrained));
    }
}

#[test]
fn tangent_arc_chain() {
    let mut sk = Sketch::new();
    let l0 = sk.add_line_points(v(0.1, 0.1), v(9.7, -0.2));
    let (l0a, l0b) = ends(&sk, l0);
    fix_at(&mut sk, l0a, 0.0, 0.0);
    add(&mut sk, K::Horizontal { line: l0 });
    add(
        &mut sk,
        K::Distance {
            p1: l0a,
            p2: l0b,
            value: dim(10.0),
        },
    );
    // arc 1: CCW from its start (tangent to l0 end) up to angle ~0
    let a1 = sk
        .add_arc_center_start_end(v(10.3, 4.8), v(10.2, 0.1), v(14.8, 5.3))
        .unwrap();
    let [c1, s1, e1] = sk.arc_points(a1).unwrap();
    add(&mut sk, K::Coincident { p1: s1, p2: l0b });
    add(
        &mut sk,
        K::TangentArcLine {
            arc: a1,
            end: ArcEnd::Start,
            line: l0,
        },
    );
    add(
        &mut sk,
        K::Radius {
            round: a1,
            value: dim(5.0),
        },
    );
    // arc 2 continues from arc 1 end, tangent, radius 3
    let a2 = sk
        .add_arc_center_start_end(v(12.2, 5.1), v(15.1, 5.2), v(12.1, 8.2))
        .unwrap();
    let [c2, s2, e2] = sk.arc_points(a2).unwrap();
    add(&mut sk, K::Coincident { p1: s2, p2: e1 });
    add(
        &mut sk,
        K::TangentArcArc {
            arc1: a1,
            end1: ArcEnd::End,
            arc2: a2,
            end2: ArcEnd::Start,
        },
    );
    add(
        &mut sk,
        K::Radius {
            round: a2,
            value: dim(3.0),
        },
    );
    // line leaving arc 2 end tangentially
    let l1 = sk.add_line_points(v(12.0, 8.1), v(5.0, 8.3));
    let (l1a, l1b) = ends(&sk, l1);
    add(&mut sk, K::Coincident { p1: l1a, p2: e2 });
    add(
        &mut sk,
        K::TangentArcLine {
            arc: a2,
            end: ArcEnd::End,
            line: l1,
        },
    );

    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    assert_pt(&sk, c1, v(10.0, 5.0), 1e-8);
    assert!((arc_radius(&sk, a1) - 5.0).abs() < 1e-9);
    assert!((pt(&sk, c1).distance(pt(&sk, e1)) - 5.0).abs() < 1e-9);
    assert!((arc_radius(&sk, a2) - 3.0).abs() < 1e-9);
    // internal tangency on the same side: |c1 c2| = 5 - 3
    assert!((pt(&sk, c1).distance(pt(&sk, c2)) - 2.0).abs() < 1e-8);
    // l1 perpendicular to radius c2->e2
    let (c, e, b) = (pt(&sk, c2), pt(&sk, e2), pt(&sk, l1b));
    assert!((e - c).dot(b - e).abs() < 1e-8 * b.distance(e));
    let d = sk.diagnose();
    // free: arc1 sweep, arc2 sweep, length of l1
    assert_eq!(d.dof, 3, "{d:?}");
    assert!(d.groups.is_empty(), "{:?}", d.groups);
    assert!(!d.is_point_free(l0b));
    assert!(d.is_point_free(l1b));
    assert!(d.is_fully_constrained(l0));
    assert_eq!(d.status(l1), Some(EntityStatus::UnderConstrained));
    // minimal movement: arc 1 end stays close to where it started (~angle 0)
    assert!(
        pt(&sk, e1).distance(v(15.0, 5.0)) < 0.5,
        "{:?}",
        pt(&sk, e1)
    );
}

#[test]
fn conflicting_fixes_and_distance_are_identified() {
    let mut sk = Sketch::new();
    let p = sk.add_point(v(0.0, 0.0));
    let q = sk.add_point(v(10.0, 0.0));
    let other = sk.add_point(v(3.0, 3.0));
    let fp = add(&mut sk, K::Fix { p });
    let fq = add(&mut sk, K::Fix { p: q });
    let _ok = add(&mut sk, K::Fix { p: other });
    let dist = add(
        &mut sk,
        K::Distance {
            p1: p,
            p2: q,
            value: dim(5.0),
        },
    );
    let rep = sk.solve();
    assert!(!rep.converged);
    assert!(rep.unsatisfied.contains(&dist));
    // fixed points never drift, even after repeated failed solves
    sk.solve();
    assert_pt(&sk, q, v(10.0, 0.0), 0.0);
    let d = sk.diagnose();
    assert_eq!(d.conflicting, vec![fp, fq, dist], "{d:?}");
    assert_eq!(d.conflicting_dependents, vec![dist]);
    assert_eq!(d.conflict_groups(), vec![vec![fp, fq, dist]]);
    assert!(d.redundant.is_empty());
    assert_eq!(d.status(p), Some(EntityStatus::OverConstrained));
    assert_eq!(d.status(other), Some(EntityStatus::FullyConstrained));
}

#[test]
fn over_constrained_rectangle_reports_conflict_set() {
    let mut sk = Sketch::new();
    let r = rect(&mut sk, 0.0, 0.0, 10.0, 5.0, 0.05);
    let [p0, p1, p2, _] = r.corners;
    fix_at(&mut sk, p0, 0.0, 0.0);
    let w = add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2: p1,
            value: dim(10.0),
        },
    );
    let h = add(
        &mut sk,
        K::Distance {
            p1,
            p2,
            value: dim(5.0),
        },
    );
    let diag = add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2,
            value: dim(20.0),
        },
    ); // should be sqrt(125)
    let rep = sk.solve();
    assert!(!rep.converged);
    let d = sk.diagnose();
    assert_eq!(d.conflicting_dependents, vec![diag], "{d:?}");
    for c in [w, h, diag] {
        assert!(
            d.conflicting.contains(&c),
            "{c:?} missing from {:?}",
            d.conflicting
        );
    }
    assert_eq!(d.status(p2), Some(EntityStatus::OverConstrained));
    // disabling the offending dimension makes it solvable again
    sk.set_enabled(diag, false).unwrap();
    assert!(sk.solve().converged);
    let d = sk.diagnose();
    assert_eq!(d.dof, 0);
    assert!(d.groups.is_empty());
    // and as a reference dimension it just measures
    sk.set_enabled(diag, true).unwrap();
    sk.set_driving(diag, false).unwrap();
    assert!(sk.solve().converged);
    assert!((sk.measure(diag) - 125f64.sqrt()).abs() < 1e-9);
}

#[test]
fn non_intersecting_circles_conflict() {
    // nonlinear infeasibility (m == n): point on two disjoint fixed circles
    let mut sk = Sketch::new();
    let c1 = sk.add_circle_center_radius(v(0.0, 0.0), 1.0);
    let c2 = sk.add_circle_center_radius(v(5.0, 0.0), 1.0);
    let p = sk.add_point(v(2.0, 0.5));
    let cc1 = sk.center_of(c1).unwrap();
    add(&mut sk, K::Fix { p: cc1 });
    add(
        &mut sk,
        K::Radius {
            round: c1,
            value: dim(1.0),
        },
    );
    let cc2 = sk.center_of(c2).unwrap();
    add(&mut sk, K::Fix { p: cc2 });
    add(
        &mut sk,
        K::Radius {
            round: c2,
            value: dim(1.0),
        },
    );
    let on1 = add(&mut sk, K::PointOnCircle { p, round: c1 });
    let on2 = add(&mut sk, K::PointOnCircle { p, round: c2 });
    assert!(!sk.solve().converged);
    let d = sk.diagnose();
    assert!(!d.conflicting.is_empty(), "{d:?}");
    for c in [on1, on2] {
        assert!(d.conflicting.contains(&c), "{d:?}");
    }
}

#[test]
fn redundant_but_consistent() {
    let mut sk = Sketch::new();
    let r = rect(&mut sk, 0.2, 0.1, 10.0, 5.0, 0.1);
    let [p0, p1, p2, _] = r.corners;
    fix_at(&mut sk, p0, 0.0, 0.0);
    add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2: p1,
            value: dim(10.0),
        },
    );
    add(
        &mut sk,
        K::Distance {
            p1,
            p2,
            value: dim(5.0),
        },
    );
    let eq = add(
        &mut sk,
        K::EqualLength {
            line1: r.lines[0],
            line2: r.lines[2],
        },
    );
    let par = add(
        &mut sk,
        K::Parallel {
            line1: r.lines[1],
            line2: r.lines[3],
        },
    );
    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    let d = sk.diagnose();
    assert_eq!(d.dof, 0);
    assert_eq!(d.redundant, vec![eq, par], "{d:?}");
    assert!(d.conflicting.is_empty());
    assert_eq!(d.redundant_groups().len(), 2);
    for g in d.redundant_groups() {
        assert!(g.len() >= 2, "{g:?}");
    }
}

#[test]
fn duplicate_fix_is_redundant() {
    let mut sk = Sketch::new();
    let p = sk.add_point(v(1.0, 2.0));
    let f1 = add(&mut sk, K::Fix { p });
    let f2 = add(&mut sk, K::Fix { p });
    assert!(sk.solve().converged);
    let d = sk.diagnose();
    assert_eq!(d.redundant, vec![f2]);
    assert_eq!(d.redundant_groups(), vec![vec![f1, f2]]);
    assert_eq!(d.dof, 0);
}

#[test]
fn drag_under_constrained_rectangle() {
    let mut sk = Sketch::new();
    let r = rect(&mut sk, 0.0, 0.0, 4.0, 2.0, 0.0);
    let [p0, p1, p2, p3] = r.corners;
    fix_at(&mut sk, p0, 0.0, 0.0);
    let lone = sk.add_point(v(50.0, 50.0));
    assert!(sk.solve().converged);
    let d = sk.diagnose();
    assert_eq!(d.dof, 2 + 2); // width, height + lone point
    for k in 1..=20 {
        let t = k as f64 / 20.0;
        let cur = v(4.0 + 3.0 * t, 2.0 + 1.0 * t);
        let rep = sk.drag(p2, cur);
        assert!(rep.converged, "{rep:?}");
        assert!(
            pt(&sk, p2).distance(cur) < 1e-4,
            "{:?} vs {cur:?}",
            pt(&sk, p2)
        );
    }
    assert_pt(&sk, p0, v(0.0, 0.0), 1e-12);
    assert_pt(&sk, p1, v(7.0, 0.0), 1e-4);
    assert_pt(&sk, p3, v(0.0, 3.0), 1e-4);
    assert_pt(&sk, lone, v(50.0, 50.0), 1e-15); // other clusters untouched
}

#[test]
fn drag_constrained_point_projects_onto_locus() {
    let mut sk = Sketch::new();
    let l = sk.add_line_points(v(0.0, 0.0), v(10.0, 0.0));
    let (a, b) = ends(&sk, l);
    add(&mut sk, K::Fix { p: a });
    add(
        &mut sk,
        K::Distance {
            p1: a,
            p2: b,
            value: dim(10.0),
        },
    );
    let rep = sk.drag(b, v(20.0, 5.0));
    assert!(rep.converged);
    let n = 20f64.hypot(5.0);
    assert_pt(&sk, b, v(200.0 / n, 50.0 / n), 1e-6);
    assert_pt(&sk, a, v(0.0, 0.0), 1e-9);
}

#[test]
fn drag_fixed_point_does_not_move() {
    let mut sk = Sketch::new();
    let l = sk.add_line_points(v(0.0, 0.0), v(10.0, 0.0));
    let (a, _) = ends(&sk, l);
    add(&mut sk, K::Fix { p: a });
    let rep = sk.drag(a, v(3.0, 3.0));
    assert!(rep.converged);
    assert_pt(&sk, a, v(0.0, 0.0), 0.0);
    // unknown ids are reported, not panicked on
    assert!(!sk.drag(SkEntityId(999), v(1.0, 1.0)).converged);
    assert!(!sk.drag(l, v(1.0, 1.0)).converged); // not a point
}

#[test]
fn drag_arc_endpoint_keeps_orientation() {
    // Arc with fixed center; dragging the end far across the chord must not flip start/end.
    let mut sk = Sketch::new();
    let arc = sk
        .add_arc_center_start_end(v(0.0, 0.0), v(5.0, 0.0), v(0.0, 5.0))
        .unwrap();
    let [c, s, e] = sk.arc_points(arc).unwrap();
    add(&mut sk, K::Fix { p: c });
    add(
        &mut sk,
        K::Radius {
            round: arc,
            value: dim(5.0),
        },
    );
    assert!(sk.solve().converged);
    // move the end around CCW in small steps up to ~300 degrees
    for k in 1..=60 {
        let ang = std::f64::consts::FRAC_PI_2 + k as f64 * 0.035;
        let rep = sk.drag(e, v(6.0 * ang.cos(), 6.0 * ang.sin()));
        assert!(rep.converged);
        let sweep = sweep_of(&sk, c, s, e);
        assert!(
            (sweep - ang).abs() < 1e-6,
            "step {k}: sweep {sweep} want {ang}"
        );
    }
    assert_pt(&sk, s, v(5.0, 0.0), 1e-6);
}

fn sweep_of(sk: &Sketch, c: SkEntityId, s: SkEntityId, e: SkEntityId) -> f64 {
    let (c, s, e) = (pt(sk, c), pt(sk, s), pt(sk, e));
    wcad_math::normalize_0_2pi((e - c).to_angle() - (s - c).to_angle())
}

#[test]
fn fixed_point_with_distance_dimensions() {
    let mut sk = Sketch::new();
    let p0 = sk.add_point(v(1.2, 1.9));
    let p1 = sk.add_point(v(5.5, 2.3));
    let p2 = sk.add_point(v(5.8, 5.4));
    fix_at(&mut sk, p0, 1.0, 2.0);
    add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2: p1,
            value: dim(5.0),
        },
    );
    add(&mut sk, K::HorizontalPoints { p1: p0, p2: p1 });
    add(
        &mut sk,
        K::Distance {
            p1,
            p2,
            value: dim(3.0),
        },
    );
    add(
        &mut sk,
        K::Distance {
            p1: p0,
            p2,
            value: dim(4.0),
        },
    );
    assert!(sk.solve().converged);
    assert_pt(&sk, p0, v(1.0, 2.0), 1e-9);
    assert_pt(&sk, p1, v(6.0, 2.0), 1e-9);
    assert_pt(&sk, p2, v(4.2, 4.4), 1e-9);
    assert_eq!(sk.diagnose().dof, 0);
}

#[test]
fn independent_clusters() {
    let mut sk = Sketch::new();
    let a = rect(&mut sk, 0.0, 0.0, 3.0, 2.0, 0.1);
    let b = rect(&mut sk, 10.0, 0.0, 3.0, 2.0, 0.1);
    fix_at(&mut sk, a.corners[0], 0.0, 0.0);
    fix_at(&mut sk, b.corners[0], 10.0, 0.0);
    sk.add_point(v(1.0, 1.0));
    let rep = sk.solve();
    // H/V/coincident separate x from y, and a rectangle's left/right x (and bottom/top y) are
    // independent: 4 clusters per rectangle, 5 once the fixed corner splits its y chain
    assert!(rep.converged && rep.clusters == 10, "{rep:?}");
    let d = sk.diagnose();
    assert_eq!(d.dof, 2 + 2 + 2);
}

#[test]
fn all_constraint_kinds() {
    use std::f64::consts::PI;
    let mut sk = Sketch::new();
    let l1 = sk.add_line_points(v(0.0, 0.0), v(10.0, 0.0));
    let (l1a, l1b) = ends(&sk, l1);
    add(&mut sk, K::Fix { p: l1a });
    add(&mut sk, K::Fix { p: l1b });
    let p = sk.add_point(v(3.2, 0.5));
    add(&mut sk, K::PointOnLine { p, line: l1 });
    add(
        &mut sk,
        K::Distance {
            p1: l1a,
            p2: p,
            value: dim(3.0),
        },
    );
    let m = sk.add_point(v(4.0, 0.3));
    add(&mut sk, K::Midpoint { p: m, line: l1 });
    let c1 = sk.add_circle_center_radius(v(5.3, 4.4), 3.7);
    add(
        &mut sk,
        K::TangentLineCircle {
            line: l1,
            round: c1,
        },
    );
    add(
        &mut sk,
        K::Radius {
            round: c1,
            value: dim(4.0),
        },
    );
    let cc1 = sk.center_of(c1).unwrap();
    add(&mut sk, K::VerticalPoints { p1: cc1, p2: m });
    let c2 = sk.add_circle_center_radius(v(11.4, 4.3), 2.2);
    add(
        &mut sk,
        K::TangentCircles {
            round1: c1,
            round2: c2,
        },
    );
    add(
        &mut sk,
        K::Diameter {
            round: c2,
            value: dim(4.0),
        },
    );
    let cc2 = sk.center_of(c2).unwrap();
    add(&mut sk, K::HorizontalPoints { p1: cc1, p2: cc2 });
    let l2 = sk.add_line_points(v(0.3, 0.2), v(0.5, 5.5));
    let (l2a, l2b) = ends(&sk, l2);
    add(
        &mut sk,
        K::Perpendicular {
            line1: l2,
            line2: l1,
        },
    );
    add(&mut sk, K::Coincident { p1: l2a, p2: l1a });
    add(
        &mut sk,
        K::Length {
            line: l2,
            value: dim(6.0),
        },
    );
    let l3 = sk.add_line_points(v(0.5, 6.5), v(5.5, 9.4));
    let (l3a, l3b) = ends(&sk, l3);
    add(
        &mut sk,
        K::Angle {
            line1: l1,
            line2: l3,
            value: dim(PI / 6.0),
        },
    );
    add(&mut sk, K::Coincident { p1: l3a, p2: l2b });
    add(
        &mut sk,
        K::EqualLength {
            line1: l3,
            line2: l2,
        },
    );
    let q = sk.add_point(v(12.9, 5.3));
    add(&mut sk, K::PointOnCircle { p: q, round: c2 });
    add(
        &mut sk,
        K::PointLineDistance {
            p: q,
            line: l1,
            value: dim(5.0),
        },
    );
    let s1 = sk.add_point(v(-2.0, 3.0));
    let s2 = sk.add_point(v(2.3, 2.6));
    add(&mut sk, K::Fix { p: s1 });
    add(
        &mut sk,
        K::Symmetric {
            p1: s1,
            p2: s2,
            line: l2,
        },
    );
    let l4 = sk.add_line_points(v(3.0, -2.0), v(7.0, -2.5));
    let (l4a, l4b) = ends(&sk, l4);
    add(
        &mut sk,
        K::Parallel {
            line1: l4,
            line2: l1,
        },
    );
    add(&mut sk, K::Fix { p: l4a });
    add(
        &mut sk,
        K::Distance {
            p1: l4a,
            p2: l4b,
            value: dim(4.0),
        },
    );
    let arc = sk
        .add_arc_center_start_end(v(20.0, 0.0), v(23.0, 0.2), v(20.1, 3.1))
        .unwrap();
    let [ac, _, ae] = sk.arc_points(arc).unwrap();
    add(&mut sk, K::Fix { p: ac });
    add(
        &mut sk,
        K::EqualRadius {
            round1: arc,
            round2: c2,
        },
    );
    let pa = sk.add_point(v(22.2, 2.3));
    add(&mut sk, K::PointOnCircle { p: pa, round: arc });
    let h1 = sk.add_point(v(30.0, 0.0));
    let h2 = sk.add_point(v(33.0, 1.0));
    add(&mut sk, K::Fix { p: h1 });
    add(
        &mut sk,
        K::HorizontalDistance {
            p1: h1,
            p2: h2,
            value: dim(2.5),
        },
    );
    add(
        &mut sk,
        K::VerticalDistance {
            p1: h1,
            p2: h2,
            value: dim(1.5),
        },
    );

    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    assert!(rep.invalid.is_empty());
    for &id in sk.constraints.keys() {
        if let Some(r) = sk.residual(id) {
            assert!(r < 1e-9, "{id:?} residual {r}");
        }
    }
    assert_pt(&sk, p, v(3.0, 0.0), 1e-9);
    assert_pt(&sk, m, v(5.0, 0.0), 1e-9);
    assert_pt(&sk, sk.center_of(c1).unwrap(), v(5.0, 4.0), 1e-9);
    assert_pt(&sk, sk.center_of(c2).unwrap(), v(11.0, 4.0), 1e-9);
    assert_pt(&sk, l2b, v(0.0, 6.0), 1e-9);
    assert_pt(&sk, l3b, v(6.0 * (PI / 6.0).cos(), 6.0 + 3.0), 1e-9);
    assert_pt(&sk, q, v(11.0 + 3f64.sqrt(), 5.0), 1e-9);
    assert_pt(&sk, s2, v(2.0, 3.0), 1e-9);
    assert_pt(&sk, l4b, v(7.0, -2.0), 1e-9);
    assert_pt(&sk, h2, v(32.5, 1.5), 1e-9);
    // arc radius comes from EqualRadius(arc, c2) = 2; end point and pa stay on it
    assert!((pt(&sk, ac).distance(pt(&sk, ae)) - 2.0).abs() < 1e-9);
    assert!((pt(&sk, ac).distance(pt(&sk, pa)) - 2.0).abs() < 1e-9);
    // measuring driving dimensions returns their values
    for (&id, c) in &sk.constraints {
        if let Some(dv) = c.kind.dim_value() {
            let got = sk.measure(id);
            let want = if matches!(c.kind, K::Angle { .. }) {
                dv.value
            } else {
                dv.value.abs()
            };
            assert!(
                (got - want).abs() < 1e-9,
                "{id:?} {}: {got} vs {want}",
                c.kind.type_name()
            );
        }
    }
}

#[test]
fn invalid_constraints_are_skipped_not_fatal() {
    let mut sk = Sketch::new();
    let l = sk.add_line_points(v(0.0, 0.0), v(3.0, 1.0));
    let (a, _) = ends(&sk, l);
    // unchecked insertion of garbage (e.g. from a corrupted file)
    let bad1 = sk.add_constraint(K::Horizontal { line: a }); // a point, not a line
    let bad2 = sk.add_constraint(K::Fix { p: SkEntityId(77) });
    let good = sk.add_constraint(K::Horizontal { line: l });
    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    assert_eq!(rep.invalid, vec![bad1, bad2]);
    assert!(sk.residual(good).unwrap() < 1e-9);
    let d = sk.diagnose();
    assert_eq!(d.invalid, vec![bad1, bad2]);
    assert!(sk.measure(good).is_nan());
}

#[test]
fn circle_center_radius_dimensions_and_reference() {
    let mut sk = Sketch::new();
    let c = sk.add_circle_center_radius(v(1.0, 1.0), 3.0);
    let cen = sk.center_of(c).unwrap();
    add(&mut sk, K::Fix { p: cen });
    let dia = add(
        &mut sk,
        K::Diameter {
            round: c,
            value: dim(10.0),
        },
    );
    let reference = sk
        .add_reference_dimension(K::Radius {
            round: c,
            value: dim(1.0),
        })
        .unwrap();
    let rep = sk.solve();
    assert!(rep.converged);
    assert!((sk.radius_of(c).unwrap() - 5.0).abs() < 1e-9);
    assert!((sk.measure(reference) - 5.0).abs() < 1e-9);
    assert!((sk.measure(dia) - 10.0).abs() < 1e-9);
    let d = sk.diagnose();
    assert_eq!(d.dof, 0);
    assert!(d.is_fully_constrained(c));
    // editing the value re-drives the geometry
    sk.set_dimension(dia, 4.0, Some("4".into())).unwrap();
    assert!(sk.solve().converged);
    assert!((sk.radius_of(c).unwrap() - 2.0).abs() < 1e-9);
}
#[test]
fn drag_whole_line_and_circle() {
    let mut sk = Sketch::new();
    let l = sk.add_line_points(v(0.0, 0.0), v(10.0, 0.0));
    let (a, b) = ends(&sk, l);
    add(&mut sk, K::Horizontal { line: l });
    add(
        &mut sk,
        K::Length {
            line: l,
            value: dim(10.0),
        },
    );
    let c = sk.add_circle_center_radius(v(5.0, 5.0), 2.0);
    let t = add(&mut sk, K::TangentLineCircle { line: l, round: c });
    assert!(sk.solve().converged);
    let (a0, b0) = (pt(&sk, a), pt(&sk, b));
    // translate the line: it moves rigidly, the circle follows the tangency
    let rep = sk.drag_curve(l, v(1.0, 2.0));
    assert!(rep.converged, "{rep:?}");
    assert_pt(&sk, a, a0 + v(1.0, 2.0), 1e-5);
    assert_pt(&sk, b, b0 + v(1.0, 2.0), 1e-5);
    assert!(sk.residual(t).unwrap() < 1e-9);
    // translate the circle along the line: keeps its radius and the tangency
    let before = sk.radius_of(c).unwrap();
    let c0 = pt(&sk, sk.center_of(c).unwrap());
    let rep = sk.drag_curve(c, v(3.0, 0.0));
    assert!(rep.converged, "{rep:?}");
    assert!((sk.radius_of(c).unwrap() - before).abs() < 1e-5);
    assert_pt(&sk, sk.center_of(c).unwrap(), c0 + v(3.0, 0.0), 1e-5);
    assert!(sk.residual(t).unwrap() < 1e-9);
}
#[test]
fn solve_keeps_short_arc_from_perturbed_starts() {
    // Radius 2 and chord 3 admit a short (97°) and a long (263°) arc; starting from short arcs the
    // solver must end on the short one.
    let mut s: u64 = 99;
    let mut rnd = move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    for trial in 0..40 {
        let amp = 0.05 + 0.02 * trial as f64;
        let mut sk = Sketch::new();
        let c = v(rnd() * amp, rnd() * amp);
        let arc = sk
            .add_arc_center_start_end(c, v(2.0, 0.0), v(rnd() * amp, 2.0 + rnd() * amp))
            .unwrap();
        let [pc, ps, pe] = sk.arc_points(arc).unwrap();
        add(&mut sk, K::Fix { p: ps });
        add(
            &mut sk,
            K::Radius {
                round: arc,
                value: dim(2.0),
            },
        );
        add(
            &mut sk,
            K::Distance {
                p1: ps,
                p2: pe,
                value: dim(3.0),
            },
        );
        let sweep0 = sweep_of(&sk, pc, ps, pe);
        assert!(sweep0 < std::f64::consts::PI);
        let rep = sk.solve();
        assert!(rep.converged, "{rep:?}");
        let sweep1 = sweep_of(&sk, pc, ps, pe);
        let want = 2.0 * 0.75f64.asin();
        assert!(
            (sweep1 - want).abs() < 1e-6,
            "trial {trial}: {sweep0} -> {sweep1}"
        );
    }
}

#[test]
fn drag_unconstrained_point_moves_exactly() {
    let mut sk = Sketch::new();
    let p = sk.add_point(v(1.0, 1.0));
    let q = sk.add_point(v(5.0, 5.0));
    add(&mut sk, K::Fix { p: q });
    let rep = sk.drag(p, v(-3.0, 2.5));
    assert!(rep.converged);
    assert_eq!(pt(&sk, p), v(-3.0, 2.5));
    assert!(!sk.drag(p, v(f64::NAN, 0.0)).converged);
    assert_eq!(pt(&sk, p), v(-3.0, 2.5));
    let d = sk.diagnose();
    assert_eq!(d.dof, 2);
    assert_eq!(d.status(p), Some(EntityStatus::UnderConstrained));
    assert_eq!(d.status(q), Some(EntityStatus::FullyConstrained));
}
