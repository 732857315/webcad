//! Scalar terms with analytic gradients. Every constraint equation is a linear
//! combination `sum_k coef_k * term_k(x)` of these terms.

/// A 2D point as a pair of global parameter indices.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pt {
    pub(crate) x: u32,
    pub(crate) y: u32,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum Term {
    /// constant
    Const(f64),
    /// a single parameter
    Var(u32),
    /// |q - p|
    Len(Pt, Pt),
    /// signed distance of `p` from the infinite line a->b: cross(b-a, p-a)/|b-a|
    /// (positive when p is left of a->b)
    SignedDist { p: Pt, a: Pt, b: Pt },
    /// projection of (q - p) onto the direction of a->b: dot(q-p, b-a)/|b-a|
    Proj { p: Pt, q: Pt, a: Pt, b: Pt },
    /// sine of the angle from (b-a) to (d-c): cross(u,v)/(|u||v|)
    Sin { a: Pt, b: Pt, c: Pt, d: Pt },
    /// cosine of the angle between (b-a) and (d-c): dot(u,v)/(|u||v|)
    Cos { a: Pt, b: Pt, c: Pt, d: Pt },
}

const TINY: f64 = 1e-300;

#[inline]
fn get(x: &[f64], p: Pt) -> [f64; 2] {
    [x[p.x as usize], x[p.y as usize]]
}

#[inline]
fn push(grad: &mut Vec<(u32, f64)>, p: Pt, g: [f64; 2], coef: f64) {
    grad.push((p.x, coef * g[0]));
    grad.push((p.y, coef * g[1]));
}

#[inline]
fn sub(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

#[inline]
fn norm(a: [f64; 2]) -> f64 {
    a[0].hypot(a[1])
}

impl Term {
    /// Returns the term value and appends `(param, coef * d term / d param)` to `grad`
    /// (entries may repeat a param; the caller merges them).
    pub(crate) fn eval(&self, x: &[f64], coef: f64, grad: &mut Vec<(u32, f64)>) -> f64 {
        match *self {
            Term::Const(v) => v,
            Term::Var(i) => {
                grad.push((i, coef));
                x[i as usize]
            }
            Term::Len(p, q) => {
                let d = sub(get(x, q), get(x, p));
                let l = norm(d);
                if l > TINY {
                    let g = [d[0] / l, d[1] / l];
                    push(grad, q, g, coef);
                    push(grad, p, g, -coef);
                } else {
                    // subgradient: pick +x so the Jacobian row is not empty
                    push(grad, q, [1.0, 0.0], coef);
                    push(grad, p, [1.0, 0.0], -coef);
                }
                l
            }
            Term::SignedDist { p, a, b } => {
                let pa = get(x, a);
                let u = sub(get(x, b), pa);
                let w = sub(get(x, p), pa);
                let l = norm(u).max(TINY);
                let c = u[0] * w[1] - u[1] * w[0];
                let f = c / l;
                // df/du = (w.y, -w.x)/L - f u / L^2 ; df/dw = (-u.y, u.x)/L
                let gu = [w[1] / l - f * u[0] / (l * l), -w[0] / l - f * u[1] / (l * l)];
                let gw = [-u[1] / l, u[0] / l];
                push(grad, b, gu, coef);
                push(grad, p, gw, coef);
                push(grad, a, [-gu[0] - gw[0], -gu[1] - gw[1]], coef);
                f
            }
            Term::Proj { p, q, a, b } => {
                let u = sub(get(x, b), get(x, a));
                let v = sub(get(x, q), get(x, p));
                let l = norm(u).max(TINY);
                let f = (u[0] * v[0] + u[1] * v[1]) / l;
                let gv = [u[0] / l, u[1] / l];
                let gu = [v[0] / l - f * u[0] / (l * l), v[1] / l - f * u[1] / (l * l)];
                push(grad, q, gv, coef);
                push(grad, p, gv, -coef);
                push(grad, b, gu, coef);
                push(grad, a, gu, -coef);
                f
            }
            Term::Sin { a, b, c, d } | Term::Cos { a, b, c, d } => {
                let is_sin = matches!(self, Term::Sin { .. });
                let u = sub(get(x, b), get(x, a));
                let v = sub(get(x, d), get(x, c));
                let lu = norm(u).max(TINY);
                let lv = norm(v).max(TINY);
                let luv = lu * lv;
                let (s, dsu, dsv) = if is_sin {
                    (u[0] * v[1] - u[1] * v[0], [v[1], -v[0]], [-u[1], u[0]])
                } else {
                    (u[0] * v[0] + u[1] * v[1], v, u)
                };
                let f = s / luv;
                let gu = [dsu[0] / luv - f * u[0] / (lu * lu), dsu[1] / luv - f * u[1] / (lu * lu)];
                let gv = [dsv[0] / luv - f * v[0] / (lv * lv), dsv[1] / luv - f * v[1] / (lv * lv)];
                push(grad, b, gu, coef);
                push(grad, a, gu, -coef);
                push(grad, d, gv, coef);
                push(grad, c, gv, -coef);
                f
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merge(g: &mut Vec<(u32, f64)>) {
        crate::solver::system::merge_grad(g);
    }

    #[test]
    fn gradients_match_finite_differences() {
        let mut s: u64 = 12345;
        let mut rnd = move || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 11) as f64 / (1u64 << 53) as f64) * 10.0 - 5.0
        };
        let pts = [0, 1, 2, 3].map(|i| Pt { x: 2 * i, y: 2 * i + 1 });
        let [a, b, c, d] = pts;
        let terms = [
            Term::Var(3),
            Term::Len(a, b),
            Term::SignedDist { p: c, a, b },
            Term::Proj { p: a, q: b, a: c, b: d },
            Term::Sin { a, b, c, d },
            Term::Cos { a, b, c, d },
        ];
        for _trial in 0..20 {
            let x: Vec<f64> = (0..8).map(|_| rnd()).collect();
            for t in &terms {
                let mut g = Vec::new();
                t.eval(&x, 1.0, &mut g);
                merge(&mut g);
                for i in 0..8u32 {
                    let h = 1e-6;
                    let (mut xp, mut xm) = (x.clone(), x.clone());
                    xp[i as usize] += h;
                    xm[i as usize] -= h;
                    let mut tmp = Vec::new();
                    let fd = (t.eval(&xp, 1.0, &mut tmp) - t.eval(&xm, 1.0, &mut tmp)) / (2.0 * h);
                    let an = g.iter().find(|e| e.0 == i).map_or(0.0, |e| e.1);
                    assert!((fd - an).abs() < 1e-5 * (1.0 + fd.abs()), "{t:?} d/dx{i}: fd {fd} analytic {an}");
                }
            }
        }
    }
}
