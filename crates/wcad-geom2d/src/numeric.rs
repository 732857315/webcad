//! Small numeric helpers: quadrature, 1D root isolation, tiny linear solves.

/// `clamp` that never panics (garbage bounds or NaN just pass through).
pub(crate) trait SafeClamp {
    fn sclamp(self, lo: f64, hi: f64) -> f64;
}

impl SafeClamp for f64 {
    #[inline]
    fn sclamp(self, lo: f64, hi: f64) -> f64 {
        if self < lo {
            lo
        } else if self > hi {
            hi
        } else {
            self
        }
    }
}

/// 5-point Gauss-Legendre nodes on [-1, 1].
const GL5_X: [f64; 5] = [
    0.0,
    -0.538_469_310_105_683_1,
    0.538_469_310_105_683_1,
    -0.906_179_845_938_664,
    0.906_179_845_938_664,
];
const GL5_W: [f64; 5] = [
    0.568_888_888_888_888_9,
    0.478_628_670_499_366_47,
    0.478_628_670_499_366_47,
    0.236_926_885_056_189_08,
    0.236_926_885_056_189_08,
];

/// 5-point Gauss-Legendre estimate of `∫_a^b f`.
pub(crate) fn gauss5(f: &impl Fn(f64) -> f64, a: f64, b: f64) -> f64 {
    let h = 0.5 * (b - a);
    let m = 0.5 * (a + b);
    let mut s = 0.0;
    for i in 0..5 {
        s += GL5_W[i] * f(m + h * GL5_X[i]);
    }
    s * h
}

/// Adaptive Gauss-Legendre integration of `f` over `[a, b]` with relative tolerance `rel`.
pub(crate) fn integrate(f: &impl Fn(f64) -> f64, a: f64, b: f64, pieces: usize, rel: f64) -> f64 {
    if !(a.is_finite() && b.is_finite()) || a == b {
        return 0.0;
    }
    let n = pieces.clamp(1, 4096);
    let h = (b - a) / n as f64;
    let mut total = 0.0;
    for i in 0..n {
        let x0 = a + h * i as f64;
        let x1 = if i + 1 == n { b } else { x0 + h };
        let whole = gauss5(f, x0, x1);
        total += adapt(f, x0, x1, whole, rel, 0);
    }
    total
}

fn adapt(f: &impl Fn(f64) -> f64, a: f64, b: f64, whole: f64, rel: f64, depth: u32) -> f64 {
    let m = 0.5 * (a + b);
    let l = gauss5(f, a, m);
    let r = gauss5(f, m, b);
    let sum = l + r;
    if depth >= 18 || (sum - whole).abs() <= rel * sum.abs().max(1e-300) {
        return sum;
    }
    adapt(f, a, m, l, rel, depth + 1) + adapt(f, m, b, r, rel, depth + 1)
}

/// Find all roots of a smooth scalar function on `[a, b]` by sampling `n` intervals, locating
/// sign changes of `f` (bisection + secant polish) and near-zero local minima of `|f|` (tangent
/// roots, detected where `df` changes sign and `|f| <= zero_eps`). Roots are returned sorted and
/// de-duplicated within `dedup` in parameter space.
pub(crate) fn find_roots(
    f: &impl Fn(f64) -> f64,
    df: &impl Fn(f64) -> f64,
    a: f64,
    b: f64,
    n: usize,
    zero_eps: f64,
    dedup: f64,
) -> Vec<f64> {
    let mut roots = Vec::new();
    if !(a.is_finite() && b.is_finite()) || b < a {
        return roots;
    }
    let n = n.clamp(2, 100_000);
    let h = (b - a) / n as f64;
    let mut x0 = a;
    let mut f0 = f(x0);
    let mut d0 = df(x0);
    if f0.abs() <= zero_eps {
        roots.push(x0);
    }
    for i in 1..=n {
        let x1 = if i == n { b } else { a + h * i as f64 };
        let f1 = f(x1);
        let d1 = df(x1);
        if f1.abs() <= zero_eps {
            // (Near-)zero at a sample: polish with a guarded Newton step.
            roots.push(refine_min_abs(f, df, x0, (x1 + h).min(b), x1));
        } else if f0.signum() != f1.signum() && f0.abs() > zero_eps {
            roots.push(bisect(f, x0, x1, f0));
        }
        // Critical point of f inside (x0, x1): possible tangent root.
        if d0.signum() != d1.signum() && d0 != 0.0 {
            let xc = bisect(df, x0, x1, d0);
            if f(xc).abs() <= zero_eps {
                roots.push(xc);
            }
        }
        x0 = x1;
        f0 = f1;
        d0 = d1;
    }
    roots.sort_by(|p, q| p.total_cmp(q));
    roots.dedup_by(|p, q| (*p - *q).abs() <= dedup);
    roots
}

/// Bisection on a bracketing interval (`f(a)` has sign of `fa`, `f(b)` the opposite sign).
pub(crate) fn bisect(f: &impl Fn(f64) -> f64, mut a: f64, mut b: f64, fa: f64) -> f64 {
    let sa = fa.signum();
    for _ in 0..80 {
        let m = 0.5 * (a + b);
        if m <= a || m >= b {
            break;
        }
        let fm = f(m);
        if fm == 0.0 {
            return m;
        }
        if fm.signum() == sa {
            a = m;
        } else {
            b = m;
        }
    }
    0.5 * (a + b)
}

/// Golden-section-free local minimum of `|f|` near `x`, restricted to `[a, b]`.
fn refine_min_abs(
    f: &impl Fn(f64) -> f64,
    df: &impl Fn(f64) -> f64,
    a: f64,
    b: f64,
    x: f64,
) -> f64 {
    let mut x = x;
    for _ in 0..30 {
        let fx = f(x);
        let d = df(x);
        if fx == 0.0 || d == 0.0 {
            break;
        }
        let nx = (x - fx / d).sclamp(a, b);
        if (nx - x).abs() <= 1e-16 * (1.0 + x.abs()) {
            x = nx;
            break;
        }
        if f(nx).abs() > fx.abs() {
            break;
        }
        x = nx;
    }
    x
}

/// Solve the 2x2 system `[a b; c d] x = r`. `None` when singular.
pub(crate) fn solve2(a: f64, b: f64, c: f64, d: f64, r0: f64, r1: f64) -> Option<(f64, f64)> {
    let det = a * d - b * c;
    let scale = (a.abs() + b.abs()) * (c.abs() + d.abs());
    if det.abs() <= 1e-300 || det.abs() <= 1e-14 * scale {
        return None;
    }
    Some(((r0 * d - b * r1) / det, (a * r1 - c * r0) / det))
}

/// Real roots of `a x² + b x + c = 0`, numerically stable, with double-root merging when the
/// discriminant is within `eps` (relative) of zero.
#[allow(dead_code)]
pub(crate) fn quadratic_roots(a: f64, b: f64, c: f64) -> Vec<f64> {
    if a.abs() < 1e-300 {
        if b.abs() < 1e-300 {
            return Vec::new();
        }
        return vec![-c / b];
    }
    let disc = b * b - 4.0 * a * c;
    let scale = (b * b).max((4.0 * a * c).abs()).max(1e-300);
    if disc.abs() <= 1e-12 * scale {
        return vec![-b / (2.0 * a)];
    }
    if disc < 0.0 {
        return Vec::new();
    }
    let s = disc.sqrt();
    let q = -0.5 * (b + b.signum() * s);
    let (r0, r1) = if q.abs() < 1e-300 {
        (0.0, 0.0)
    } else {
        (q / a, c / q)
    };
    if r0 < r1 { vec![r0, r1] } else { vec![r1, r0] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadrature() {
        let v = integrate(&|x: f64| x.sin(), 0.0, std::f64::consts::PI, 4, 1e-13);
        assert!((v - 2.0).abs() < 1e-12);
        let v = integrate(&|x: f64| (1.0 - x * x).max(0.0).sqrt(), -1.0, 1.0, 4, 1e-12);
        assert!((v - std::f64::consts::FRAC_PI_2).abs() < 1e-7, "{v}");
    }

    #[test]
    fn roots_with_tangency() {
        // (x-1)^2 (x-3): double root at 1, simple root at 3.
        let f = |x: f64| (x - 1.0).powi(2) * (x - 3.0);
        let df = |x: f64| 2.0 * (x - 1.0) * (x - 3.0) + (x - 1.0).powi(2);
        let r = find_roots(&f, &df, -2.0, 5.0, 37, 1e-10, 1e-7);
        assert_eq!(r.len(), 2, "{r:?}");
        assert!((r[0] - 1.0).abs() < 1e-6);
        assert!((r[1] - 3.0).abs() < 1e-12);
        let q = quadratic_roots(1.0, -2.0, 1.0);
        assert_eq!(q, vec![1.0]);
        let q = quadratic_roots(1.0, 0.0, -4.0);
        assert!((q[0] + 2.0).abs() < 1e-15 && (q[1] - 2.0).abs() < 1e-15);
    }
}
