//! The 550-equation benchmark sketch: correctness always, timings with `--ignored`.

mod support;

use std::time::Instant;

use support::bench_sketch::{build, max_point_dev, perturb};
use wcad_math::DVec2;
use wcad_sketch::{ConstraintKind as K, DimValue};

#[test]
fn big_chain_solves_and_diagnoses() {
    let b = build(25);
    let base = b.sk.clone();
    let mut sk = base.clone();
    perturb(&mut sk, 0.2, 7, b.fixed);
    let rep = sk.solve();
    assert!(rep.converged, "{rep:?}");
    assert!(max_point_dev(&sk, &base) < 1e-8);
    let d = sk.diagnose();
    assert_eq!(d.dof, 0, "{:?}", d.groups);
    assert!(d.groups.is_empty());
    assert!(d.n_equations >= 540, "{}", d.n_equations);

    // heights free -> one DOF per unit, dragging moves the chain
    for &h in &b.heights {
        sk.set_enabled(h, false).unwrap();
    }
    assert_eq!(sk.diagnose().dof, 25);
    let start = sk.point(b.drag_pt).unwrap();
    for i in 1..=20 {
        let cur = start + DVec2::new(0.0, 0.1 * i as f64);
        let rep = sk.drag(b.drag_pt, cur);
        assert!(rep.converged, "{rep:?}");
        assert!(sk.point(b.drag_pt).unwrap().distance(cur) < 1e-5);
    }

    // one conflicting dimension is found and blamed
    let mut sk = base.clone();
    let (a, _) = sk.line_ends(b.first_line).unwrap();
    let (c, _) = sk.line_ends(b.third_line).unwrap();
    let bad = sk.add_constraint_checked(K::Distance { p1: a, p2: c, value: DimValue::new(1.0) }).unwrap();
    assert!(!sk.solve().converged);
    let d = sk.diagnose();
    assert_eq!(d.conflicting_dependents, vec![bad], "{:?}", d.groups);
}

fn best_ms<F: FnMut()>(reps: usize, mut f: F) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_secs_f64());
    }
    best * 1e3
}

/// `cargo test --release -p wcad-sketch --test bench -- --ignored --nocapture`
#[test]
#[ignore = "timing benchmark; run in release"]
fn bench_timings() {
    let b = build(25);
    let base = b.sk.clone();
    let solve_ms = best_ms(5, || {
        let mut sk = base.clone();
        perturb(&mut sk, 0.2, 7, b.fixed);
        assert!(sk.solve().converged);
    });
    let diag_ms = best_ms(5, || {
        let d = base.diagnose();
        assert_eq!(d.dof, 0);
    });
    let mut sk = base.clone();
    for &h in &b.heights {
        sk.set_enabled(h, false).unwrap();
    }
    let diag_free_ms = best_ms(5, || {
        assert_eq!(sk.diagnose().dof, 25);
    });
    let start = sk.point(b.drag_pt).unwrap();
    let steps = 100;
    let t = Instant::now();
    for i in 1..=steps {
        let rep = sk.drag(b.drag_pt, start + DVec2::new(0.0, 2.0 * i as f64 / steps as f64));
        assert!(rep.converged);
    }
    let drag_ms = t.elapsed().as_secs_f64() * 1e3 / steps as f64;
    println!(
        "solve {solve_ms:.2} ms, diagnose {diag_ms:.2} ms (dof 0) / {diag_free_ms:.2} ms (dof 25), drag {drag_ms:.2} ms/step"
    );
    assert!(diag_ms < 20.0 && diag_free_ms < 20.0, "diagnose too slow");
}
