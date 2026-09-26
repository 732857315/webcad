//! Benchmark sketch shared by `tests/bench.rs` and `examples/bench.rs`: N units of (rectangle of
//! 4 lines with own endpoints + tangent quarter arc), chained left to right into one big cluster.
//! 16 constraints per unit (N = 25 -> 400 constraints, ~550 equations, 550 parameters).

#![allow(dead_code)]

use wcad_math::DVec2;
use wcad_sketch::{ArcEnd, ConstraintKind as K, DimValue, SkConstraintId, SkEntityId, Sketch};

pub struct Built {
    pub sk: Sketch,
    pub heights: Vec<SkConstraintId>,
    pub drag_pt: SkEntityId,
    pub first_line: SkEntityId,
    pub third_line: SkEntityId,
    pub fixed: SkEntityId,
}

fn add(sk: &mut Sketch, k: K) -> SkConstraintId {
    sk.add_constraint_checked(k).expect("valid")
}

fn end(sk: &Sketch, l: SkEntityId) -> (SkEntityId, SkEntityId) {
    sk.line_ends(l).expect("line")
}

pub fn build(n: usize) -> Built {
    let mut sk = Sketch::new();
    let mut heights = Vec::new();
    let mut prev_br: Option<SkEntityId> = None;
    let mut x0 = 0.0;
    let mut drag_pt = SkEntityId(0);
    let mut first_lines = (SkEntityId(0), SkEntityId(0));
    let mut fixed = SkEntityId(0);
    for k in 0..n {
        let (w, h, r) = (4.0 + (k % 3) as f64, 3.0 + (k % 2) as f64, 1.0);
        let c = [
            DVec2::new(x0, 0.0),
            DVec2::new(x0 + w, 0.0),
            DVec2::new(x0 + w, h),
            DVec2::new(x0, h),
        ];
        let lines: [SkEntityId; 4] = [0, 1, 2, 3].map(|i| sk.add_line_points(c[i], c[(i + 1) % 4]));
        if k == 0 {
            first_lines = (lines[0], lines[2]);
        }
        for i in 0..4 {
            let (e, nx) = (end(&sk, lines[i]).1, end(&sk, lines[(i + 1) % 4]).0);
            add(&mut sk, K::Coincident { p1: e, p2: nx });
        }
        add(&mut sk, K::Horizontal { line: lines[0] });
        add(&mut sk, K::Horizontal { line: lines[2] });
        add(&mut sk, K::Vertical { line: lines[1] });
        add(&mut sk, K::Vertical { line: lines[3] });
        let p = [0, 1, 2, 3].map(|i| end(&sk, lines[i]).0);
        add(
            &mut sk,
            K::Distance {
                p1: p[0],
                p2: p[1],
                value: DimValue::new(w),
            },
        );
        heights.push(add(
            &mut sk,
            K::Distance {
                p1: p[1],
                p2: p[2],
                value: DimValue::new(h),
            },
        ));
        match prev_br {
            None => {
                fixed = p[0];
                add(&mut sk, K::Fix { p: p[0] });
            }
            Some(q) => {
                add(&mut sk, K::Coincident { p1: p[0], p2: q });
            }
        }
        prev_br = Some(p[1]);
        // quarter arc from the top-right corner, tangent to the right edge
        let tr = c[2];
        let arc = sk
            .add_arc_center_start_end(
                DVec2::new(tr.x - r, tr.y),
                tr,
                DVec2::new(tr.x - r, tr.y + r),
            )
            .expect("arc");
        let [ac, as_, ae] = sk.arc_points(arc).expect("arc");
        add(&mut sk, K::Coincident { p1: as_, p2: p[2] });
        add(
            &mut sk,
            K::TangentArcLine {
                arc,
                end: ArcEnd::Start,
                line: lines[1],
            },
        );
        add(
            &mut sk,
            K::Radius {
                round: arc,
                value: DimValue::new(r),
            },
        );
        add(&mut sk, K::VerticalPoints { p1: ac, p2: ae });
        if k == n / 2 {
            drag_pt = p[2];
        }
        x0 += w;
    }
    Built {
        sk,
        heights,
        drag_pt,
        first_line: first_lines.0,
        third_line: first_lines.1,
        fixed,
    }
}

/// Move every point except `fixed` by a pseudo-random offset in `[-amp, amp]²`.
pub fn perturb(sk: &mut Sketch, amp: f64, seed: u64, fixed: SkEntityId) {
    let mut s = seed;
    let mut rnd = move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((s >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * amp
    };
    let ids: Vec<SkEntityId> = sk.entities.keys().copied().collect();
    for id in ids {
        if id == fixed {
            continue;
        }
        if let Some(p) = sk.point(id) {
            let q = p + DVec2::new(rnd(), rnd());
            sk.set_point(id, q).expect("point");
        }
    }
}

/// Max distance between corresponding points of two sketches with the same entities.
pub fn max_point_dev(a: &Sketch, b: &Sketch) -> f64 {
    a.entities
        .keys()
        .filter_map(|&id| Some(a.point(id)?.distance(b.point(id)?)))
        .fold(0.0, f64::max)
}
