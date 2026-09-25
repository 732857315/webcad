//! cargo run --release -p wcad-sketch --example bench -- [N]
//! N units of (rectangle of 4 lines + tangent quarter arc), chained left to right: 16 constraints
//! per unit (N = 25 -> 400 constraints, ~550 equations), one big cluster.

#[path = "../tests/support/bench_sketch.rs"]
mod bench_sketch;

use std::time::Instant;

use bench_sketch::{build, max_point_dev, perturb};
use wcad_math::DVec2;
use wcad_sketch::{ConstraintKind as K, DimValue, Diagnosis, SolveReport};

fn best_ms<F: FnMut()>(reps: usize, mut f: F) -> f64 {
    let mut best = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed().as_secs_f64());
    }
    best * 1e3
}

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(25);
    let b = build(n);
    let base = b.sk.clone();
    let d0 = base.diagnose();
    println!("units={n} constraints={} equations={} params={}", base.constraints.len(), d0.n_equations, d0.n_params);
    let mut rep = SolveReport::default();
    let mut dev = 0.0;
    let ms = best_ms(5, || {
        let mut sk = base.clone();
        perturb(&mut sk, 0.2, 7, b.fixed);
        rep = sk.solve();
        dev = max_point_dev(&sk, &base);
    });
    println!(
        "full solve: {ms:.3} ms converged={} iters={} maxres={:.2e} max|p-p_exact|={dev:.2e}",
        rep.converged, rep.iterations, rep.max_residual
    );
    let mut d = Diagnosis::default();
    let ms = best_ms(5, || d = base.diagnose());
    println!("diagnose: {ms:.3} ms dof={} rank={} groups={}", d.dof, d.rank, d.groups.len());

    let mut sk = base.clone();
    for &h in &b.heights {
        let _ = sk.set_enabled(h, false);
    }
    let ms = best_ms(5, || d = sk.diagnose());
    println!("diagnose (heights disabled): {ms:.3} ms dof={}", d.dof);
    let start = sk.point(b.drag_pt).unwrap_or_default();
    let steps = 200;
    let (mut worst, mut all) = (0, true);
    let t = Instant::now();
    for i in 1..=steps {
        let cur = start + DVec2::new(0.0, 2.0 * i as f64 / steps as f64);
        let rep = sk.drag(b.drag_pt, cur);
        worst = worst.max(rep.iterations);
        all &= rep.converged;
    }
    let per = t.elapsed().as_secs_f64() * 1e3 / steps as f64;
    println!(
        "drag: {per:.3} ms/step over {steps} steps, all converged={all}, max iters/step={worst}, final {:?}",
        sk.point(b.drag_pt)
    );

    let mut sk = base.clone();
    let (Some((a, _)), Some((c, _))) = (sk.line_ends(b.first_line), sk.line_ends(b.third_line)) else {
        return;
    };
    let bad = sk.add_constraint(K::Distance { p1: a, p2: c, value: DimValue::new(1.0) });
    let t = Instant::now();
    let rep = sk.solve();
    let ts = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let d = sk.diagnose();
    let td = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "conflict: solve {ts:.2} ms (converged={}, iters={}), diagnose {td:.2} ms, bad={bad:?} dependents={:?} conflicting={:?}",
        rep.converged, rep.iterations, d.conflicting_dependents, d.conflicting
    );
}
