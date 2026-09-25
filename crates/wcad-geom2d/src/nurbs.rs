//! NURBS evaluation (de Boor / basis-function derivatives, rational weights), interpolation
//! through fit points, knot insertion, sub-curve extraction and exact conversions of the analytic
//! curves to rational B-splines.

use std::f64::consts::FRAC_PI_2;

use wcad_math::{DAffine2, DVec2, DVec3};

use crate::bulge::PolySegment;
use crate::curves::{Arc2, Circle2, EllipseArc2, Line2, Nurbs2, Polyline2};
use crate::numeric::SafeClamp;
use crate::{Error, Result};

/// Highest degree accepted from files (higher degrees are treated as invalid).
pub const MAX_DEGREE: u32 = 32;

impl Nurbs2 {
    /// Cheap structural check (O(1)): degree, array lengths and a non-empty domain. Evaluation of a
    /// structurally valid spline never panics.
    pub fn is_valid(&self) -> bool {
        let p = self.degree as usize;
        let n = self.ctrl.len();
        self.degree >= 1
            && self.degree <= MAX_DEGREE
            && n > p
            && self.knots.len() == n + p + 1
            && (self.weights.is_empty() || self.weights.len() == n)
            && self.knots[p].is_finite()
            && self.knots[n].is_finite()
            && self.knots[p] < self.knots[n]
    }

    /// Full validation (O(n)): structural check plus non-decreasing knots, finite control points
    /// and positive weights.
    pub fn validate(&self) -> Result<()> {
        if !self.is_valid() {
            return Err(Error::Invalid(format!(
                "spline: degree {} with {} control points and {} knots",
                self.degree,
                self.ctrl.len(),
                self.knots.len()
            )));
        }
        if self.knots.windows(2).any(|w| !(w[0] <= w[1])) {
            return Err(Error::Invalid("spline knots must be non-decreasing".into()));
        }
        if self.ctrl.iter().any(|p| !p.is_finite()) {
            return Err(Error::Invalid("spline control point is not finite".into()));
        }
        if self.weights.iter().any(|w| !(*w > 0.0) || !w.is_finite()) {
            return Err(Error::Invalid("spline weights must be positive".into()));
        }
        Ok(())
    }

    pub fn is_rational(&self) -> bool {
        !self.weights.is_empty()
    }

    fn weight(&self, i: usize) -> f64 {
        if self.weights.is_empty() {
            1.0
        } else {
            self.weights[i]
        }
    }

    /// Parameter domain `[knots[p], knots[n]]` (assumes [`Self::is_valid`]).
    pub fn domain(&self) -> (f64, f64) {
        let p = self.degree as usize;
        let n = self.ctrl.len();
        (self.knots[p], self.knots[n])
    }

    /// Knot span index `k` with `knots[k] <= u < knots[k+1]`, clamped to `[p, n-1]`.
    pub fn find_span(&self, u: f64) -> usize {
        let p = self.degree as usize;
        let n = self.ctrl.len();
        if u >= self.knots[n] {
            // last non-empty span
            let mut k = n - 1;
            while k > p && self.knots[k] >= self.knots[n] {
                k -= 1;
            }
            return k;
        }
        if u <= self.knots[p] {
            let mut k = p;
            while k + 1 < n && self.knots[k + 1] <= self.knots[p] {
                k += 1;
            }
            return k;
        }
        // binary search in [p, n)
        let (mut lo, mut hi) = (p, n);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if u < self.knots[mid] {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    /// Basis functions and their derivatives up to order `nd` at `u` in span `span`
    /// (The NURBS Book, A2.3). Returns `ders[k][j]` = k-th derivative of `N_{span-p+j}`.
    fn ders_basis(&self, span: usize, u: f64, nd: usize) -> Vec<Vec<f64>> {
        let p = self.degree as usize;
        let k = &self.knots;
        let mut ndu = vec![vec![0.0; p + 1]; p + 1];
        let mut left = vec![0.0; p + 1];
        let mut right = vec![0.0; p + 1];
        ndu[0][0] = 1.0;
        for j in 1..=p {
            left[j] = u - k[span + 1 - j];
            right[j] = k[span + j] - u;
            let mut saved = 0.0;
            for r in 0..j {
                ndu[j][r] = right[r + 1] + left[j - r];
                let temp = safe_div(ndu[r][j - 1], ndu[j][r]);
                ndu[r][j] = saved + right[r + 1] * temp;
                saved = left[j - r] * temp;
            }
            ndu[j][j] = saved;
        }
        let nd = nd.min(p);
        let mut ders = vec![vec![0.0; p + 1]; nd + 1];
        for j in 0..=p {
            ders[0][j] = ndu[j][p];
        }
        let mut a = vec![vec![0.0; p + 1]; 2];
        for r in 0..=p {
            let (mut s1, mut s2) = (0usize, 1usize);
            a[0][0] = 1.0;
            for kk in 1..=nd {
                let mut d = 0.0;
                let rk = r as isize - kk as isize;
                let pk = p - kk;
                if r >= kk {
                    a[s2][0] = safe_div(a[s1][0], ndu[pk + 1][rk as usize]);
                    d = a[s2][0] * ndu[rk as usize][pk];
                }
                let j1 = if rk >= -1 { 1 } else { (-rk) as usize };
                let j2 = if (r as isize - 1) <= pk as isize {
                    kk - 1
                } else {
                    p - r
                };
                for j in j1..=j2 {
                    let idx = (rk + j as isize) as usize;
                    a[s2][j] = safe_div(a[s1][j] - a[s1][j - 1], ndu[pk + 1][idx]);
                    d += a[s2][j] * ndu[idx][pk];
                }
                if r <= pk {
                    a[s2][kk] = safe_div(-a[s1][kk - 1], ndu[pk + 1][r]);
                    d += a[s2][kk] * ndu[r][pk];
                }
                ders[kk][r] = d;
                std::mem::swap(&mut s1, &mut s2);
            }
        }
        let mut f = p as f64;
        for (kk, row) in ders.iter_mut().enumerate().skip(1) {
            for v in row.iter_mut() {
                *v *= f;
            }
            f *= (p - kk) as f64;
        }
        ders
    }

    /// Point and derivatives (up to second order) at `u` (rational formula, NURBS Book A4.2).
    /// Assumes [`Self::is_valid`].
    pub fn eval_derivs(&self, u: f64) -> [DVec2; 3] {
        let (d0, d1) = self.domain();
        let u = if u.is_finite() { u.sclamp(d0, d1) } else { d0 };
        let p = self.degree as usize;
        let span = self.find_span(u);
        let ders = self.ders_basis(span, u, 2);
        let mut a = [DVec2::ZERO; 3];
        let mut w = [0.0f64; 3];
        for (kk, dk) in ders.iter().enumerate() {
            for (j, nb) in dk.iter().enumerate() {
                let i = span - p + j;
                let wi = self.weight(i);
                a[kk] += self.ctrl[i] * (nb * wi);
                w[kk] += nb * wi;
            }
        }
        if w[0].abs() < 1e-300 {
            return [a[0], a[1], a[2]];
        }
        let c0 = a[0] / w[0];
        let c1 = (a[1] - c0 * w[1]) / w[0];
        let c2 = (a[2] - c1 * (2.0 * w[1]) - c0 * w[2]) / w[0];
        [c0, c1, c2]
    }

    /// Point at `u` (de Boor via basis functions).
    pub fn point_at(&self, u: f64) -> DVec2 {
        self.eval_derivs(u)[0]
    }

    /// Distinct knot values inside the domain (span boundaries), including both domain ends.
    pub fn span_breaks(&self) -> Vec<f64> {
        let (d0, d1) = self.domain();
        let mut v = vec![d0];
        for &k in &self.knots {
            if k > d0 && k < d1 && v.last().is_some_and(|l| k > *l) {
                v.push(k);
            }
        }
        v.push(d1);
        v
    }

    fn homog(&self) -> Vec<DVec3> {
        self.ctrl
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let w = self.weight(i);
                DVec3::new(p.x * w, p.y * w, w)
            })
            .collect()
    }

    fn from_homog(degree: u32, knots: Vec<f64>, pw: &[DVec3], rational: bool) -> Self {
        let mut ctrl = Vec::with_capacity(pw.len());
        let mut weights = Vec::with_capacity(pw.len());
        for q in pw {
            let w = if q.z.abs() < 1e-300 { 1.0 } else { q.z };
            ctrl.push(DVec2::new(q.x / w, q.y / w));
            weights.push(w);
        }
        if !rational {
            weights.clear();
        }
        Nurbs2 {
            degree,
            ctrl,
            weights,
            knots,
            fit_points: Vec::new(),
            closed: false,
        }
    }

    /// Insert knot `u` `times` times (Boehm). `u` must lie inside the domain.
    pub fn insert_knot(&self, u: f64, times: usize) -> Nurbs2 {
        let p = self.degree as usize;
        let mut knots = self.knots.clone();
        let mut pw = self.homog();
        for _ in 0..times {
            let n = pw.len();
            // span: largest k in [p, n-1] with knots[k] <= u
            let mut k = p;
            while k + 1 < n && knots[k + 1] <= u {
                k += 1;
            }
            let mut q = Vec::with_capacity(n + 1);
            for i in 0..=n {
                if i + p <= k {
                    q.push(pw[i]);
                } else if i > k {
                    q.push(pw[i - 1]);
                } else {
                    let den = knots[i + p] - knots[i];
                    let alpha = if den > 0.0 { (u - knots[i]) / den } else { 0.0 };
                    q.push(pw[i] * alpha + pw[i - 1] * (1.0 - alpha));
                }
            }
            knots.insert(k + 1, u);
            pw = q;
        }
        let mut out = Self::from_homog(self.degree, knots, &pw, self.is_rational());
        out.closed = self.closed;
        out
    }

    fn snap_to_knot(&self, u: f64) -> f64 {
        let (d0, d1) = self.domain();
        let eps = 1e-12 * (d1 - d0).abs().max(1e-300);
        for &k in &self.knots {
            if (k - u).abs() <= eps {
                return k;
            }
        }
        u
    }

    fn multiplicity(&self, u: f64) -> usize {
        self.knots.iter().filter(|k| **k == u).count()
    }

    /// The piece of the curve between parameters `u0 < u1` as a clamped spline.
    pub fn sub_curve(&self, u0: f64, u1: f64) -> Option<Nurbs2> {
        if !self.is_valid() {
            return None;
        }
        let (d0, d1) = self.domain();
        let u0 = self.snap_to_knot(u0.sclamp(d0, d1));
        let u1 = self.snap_to_knot(u1.sclamp(d0, d1));
        if !(u1 - u0 > 1e-14 * (d1 - d0)) {
            return None;
        }
        let p = self.degree as usize;
        let mut c = self.clone();
        let m0 = c.multiplicity(u0);
        if m0 < p {
            c = c.insert_knot(u0, p - m0);
        }
        let m1 = c.multiplicity(u1);
        if m1 < p {
            c = c.insert_knot(u1, p - m1);
        }
        let e0 = c.knots.iter().rposition(|k| *k == u0)?;
        let f1 = c.knots.iter().position(|k| *k == u1)?;
        if e0 < p || f1 == 0 || e0 + 1 > f1 || f1 > c.ctrl.len() {
            return None;
        }
        let lo = e0 - p;
        let hi = f1 - 1;
        let ctrl = c.ctrl[lo..=hi].to_vec();
        let weights = if c.weights.is_empty() {
            Vec::new()
        } else {
            c.weights[lo..=hi].to_vec()
        };
        let mut knots = vec![u0; p + 1];
        knots.extend_from_slice(&c.knots[e0 + 1..f1]);
        knots.extend(std::iter::repeat_n(u1, p + 1));
        let out = Nurbs2 {
            degree: self.degree,
            ctrl,
            weights,
            knots,
            fit_points: Vec::new(),
            closed: false,
        };
        out.is_valid().then_some(out)
    }

    /// Same curve traversed backwards (domain mapped by `u → a + b - u`).
    pub fn reversed(&self) -> Nurbs2 {
        let a = self.knots.first().copied().unwrap_or(0.0);
        let b = self.knots.last().copied().unwrap_or(1.0);
        let knots = self.knots.iter().rev().map(|k| a + b - k).collect();
        let mut ctrl = self.ctrl.clone();
        ctrl.reverse();
        let mut weights = self.weights.clone();
        weights.reverse();
        let mut fit_points = self.fit_points.clone();
        fit_points.reverse();
        Nurbs2 {
            degree: self.degree,
            ctrl,
            weights,
            knots,
            fit_points,
            closed: self.closed,
        }
    }

    /// Affine image (NURBS are affine invariant: transform control and fit points).
    pub fn transformed(&self, m: &DAffine2) -> Nurbs2 {
        Nurbs2 {
            degree: self.degree,
            ctrl: self.ctrl.iter().map(|p| m.transform_point2(*p)).collect(),
            weights: self.weights.clone(),
            knots: self.knots.clone(),
            fit_points: self
                .fit_points
                .iter()
                .map(|p| m.transform_point2(*p))
                .collect(),
            closed: self.closed,
        }
    }

    /// `true` when both ends have full multiplicity (the curve interpolates its end control points).
    pub fn is_clamped(&self) -> bool {
        if !self.is_valid() {
            return false;
        }
        let p = self.degree as usize;
        let n = self.knots.len();
        self.knots[..=p].iter().all(|k| *k == self.knots[0])
            && self.knots[n - p - 1..]
                .iter()
                .all(|k| *k == self.knots[n - 1])
    }

    /// Clamped copy of the curve over its full domain.
    pub fn clamped(&self) -> Option<Nurbs2> {
        if self.is_clamped() {
            return Some(self.clone());
        }
        let (d0, d1) = self.domain();
        self.sub_curve(d0, d1)
    }

    /// Join `other` after `self` (both clamped, same degree, `self.end ≈ other.start`).
    pub fn concat(&self, other: &Nurbs2) -> Option<Nurbs2> {
        let a = self.clamped()?;
        let b = other.clamped()?;
        if a.degree != b.degree {
            return None;
        }
        let p = a.degree as usize;
        let rational = a.is_rational() || b.is_rational();
        let pa = a.homog();
        let mut pb = b.homog();
        // scale b's homogeneous points so the joint weights match (does not change b's shape)
        let wa = pa.last()?.z;
        let wb = pb.first()?.z;
        if wb.abs() < 1e-300 {
            return None;
        }
        let s = wa / wb;
        for q in &mut pb {
            *q *= s;
        }
        let a_end = *a.knots.last()?;
        let shift = a_end - b.knots[0];
        let mut knots: Vec<f64> = a.knots[..a.knots.len() - 1].to_vec();
        knots.extend(b.knots[p + 1..].iter().map(|k| k + shift));
        let mut pw = pa.clone();
        // average the shared joint point
        if let (Some(last), Some(first)) = (pw.last_mut(), pb.first()) {
            *last = (*last + *first) * 0.5;
        }
        pw.extend_from_slice(&pb[1..]);
        let out = Self::from_homog(a.degree, knots, &pw, rational);
        out.is_valid().then_some(out)
    }

    /// Cubic (or lower, for few points) interpolating spline through `points` using chord-length
    /// parametrization and averaged knots (The NURBS Book §9.2.1). Consecutive duplicate points are
    /// removed. The fit points are stored in the result.
    pub fn from_fit_points(points: &[DVec2], degree: u32) -> Result<Nurbs2> {
        let mut pts: Vec<DVec2> = Vec::with_capacity(points.len());
        for &p in points {
            if !p.is_finite() {
                return Err(Error::Invalid("fit point is not finite".into()));
            }
            if pts
                .last()
                .is_none_or(|l: &DVec2| l.distance(p) > 1e-12 * (1.0 + p.abs().max_element()))
            {
                pts.push(p);
            }
        }
        let n = pts.len();
        if n < 2 {
            return Err(Error::Degenerate(
                "spline needs at least two distinct fit points",
            ));
        }
        let p = (degree.clamp(1, MAX_DEGREE) as usize).min(n - 1);
        // chord-length parameters
        let mut ub = vec![0.0; n];
        let mut total = 0.0;
        for i in 1..n {
            total += pts[i].distance(pts[i - 1]);
            ub[i] = total;
        }
        for u in &mut ub {
            *u /= total;
        }
        ub[n - 1] = 1.0;
        // averaged knots
        let mut knots = vec![0.0; p + 1];
        for j in 1..n - p {
            let s: f64 = ub[j..j + p].iter().sum();
            knots.push(s / p as f64);
        }
        knots.extend(std::iter::repeat_n(1.0, p + 1));
        let proto = Nurbs2 {
            degree: p as u32,
            ctrl: vec![DVec2::ZERO; n],
            weights: Vec::new(),
            knots,
            fit_points: Vec::new(),
            closed: false,
        };
        // banded collocation matrix: row i has nonzeros in columns [i-p, i+p]
        let w = 2 * p + 1;
        let mut band = vec![0.0; n * w];
        for i in 0..n {
            let span = proto.find_span(ub[i]);
            let nb = &proto.ders_basis(span, ub[i], 0)[0];
            for (j, v) in nb.iter().enumerate() {
                let col = span - p + j;
                let off = col as isize - i as isize + p as isize;
                if off < 0 || off >= w as isize {
                    if v.abs() > 1e-14 {
                        return Err(Error::NoSolution(
                            "spline interpolation matrix is not banded",
                        ));
                    }
                    continue;
                }
                band[i * w + off as usize] = *v;
            }
        }
        let mut rhs = pts.clone();
        let at = |i: usize, j: usize| i * w + (j + p - i);
        // elimination without pivoting (totally positive collocation matrix)
        for k in 0..n {
            let piv = band[at(k, k)];
            if piv.abs() < 1e-14 {
                return Err(Error::NoSolution("singular spline interpolation matrix"));
            }
            for i in k + 1..(k + p + 1).min(n) {
                let f = band[at(i, k)] / piv;
                if f == 0.0 {
                    continue;
                }
                for j in k..(k + p + 1).min(n) {
                    band[at(i, j)] -= f * band[at(k, j)];
                }
                let rk = rhs[k];
                rhs[i] -= rk * f;
            }
        }
        let mut ctrl = vec![DVec2::ZERO; n];
        for k in (0..n).rev() {
            let mut s = rhs[k];
            for j in k + 1..(k + p + 1).min(n) {
                s -= ctrl[j] * band[at(k, j)];
            }
            ctrl[k] = s / band[at(k, k)];
        }
        Ok(Nurbs2 {
            degree: p as u32,
            ctrl,
            weights: Vec::new(),
            knots: proto.knots,
            fit_points: points.to_vec(),
            closed: false,
        })
    }

    /// Degree-1 spline through `points` (used as a fallback representation).
    pub fn polyline(points: &[DVec2]) -> Option<Nurbs2> {
        let n = points.len();
        if n < 2 {
            return None;
        }
        let mut knots = vec![0.0];
        knots.extend((0..n).map(|i| i as f64));
        knots.push((n - 1) as f64);
        Some(Nurbs2 {
            degree: 1,
            ctrl: points.to_vec(),
            weights: Vec::new(),
            knots,
            fit_points: Vec::new(),
            closed: false,
        })
    }

    /// Raise the degree by one (exact; used to match degrees before concatenation). Works on the
    /// clamped Bézier decomposition.
    pub fn elevated(&self) -> Option<Nurbs2> {
        let c = self.clamped()?;
        let p = c.degree as usize;
        // decompose into Bézier segments by inserting each interior knot to multiplicity p
        let breaks = c.span_breaks();
        let mut segs: Vec<Nurbs2> = Vec::new();
        for w in breaks.windows(2) {
            segs.push(c.sub_curve(w[0], w[1])?);
        }
        let mut out: Option<Nurbs2> = None;
        for s in segs {
            let pw = s.homog();
            if pw.len() != p + 1 {
                return None;
            }
            // Bézier degree elevation: Q_i = i/(p+1) P_{i-1} + (1 - i/(p+1)) P_i
            let mut q = Vec::with_capacity(p + 2);
            for i in 0..=p + 1 {
                let a = i as f64 / (p + 1) as f64;
                let prev = if i > 0 { pw[i - 1] } else { DVec3::ZERO };
                let cur = if i <= p { pw[i] } else { DVec3::ZERO };
                q.push(prev * a + cur * (1.0 - a));
            }
            let (u0, u1) = (s.knots[0], *s.knots.last()?);
            let mut knots = vec![u0; p + 2];
            knots.extend(std::iter::repeat_n(u1, p + 2));
            let e = Self::from_homog(c.degree + 1, knots, &q, c.is_rational());
            out = Some(match out {
                None => e,
                Some(acc) => acc.concat(&e)?,
            });
        }
        out
    }
}

fn safe_div(a: f64, b: f64) -> f64 {
    if b == 0.0 { 0.0 } else { a / b }
}

/// Rational quadratic Bézier pieces (each ≤ 90°) of a unit-circle arc from `a0` with signed sweep.
/// Returns control points `(p0, p1, p2)` and the middle weight for each piece.
fn unit_arc_pieces(a0: f64, sweep: f64) -> Vec<([DVec2; 3], f64)> {
    let n = ((sweep.abs() / FRAC_PI_2) - 1e-9).ceil().max(1.0) as usize;
    let d = sweep / n as f64;
    let w = (d * 0.5).cos();
    (0..n)
        .map(|i| {
            let t0 = a0 + d * i as f64;
            let t1 = t0 + d;
            let tm = t0 + d * 0.5;
            let p0 = DVec2::new(t0.cos(), t0.sin());
            let p2 = DVec2::new(t1.cos(), t1.sin());
            let p1 = DVec2::new(tm.cos(), tm.sin()) / w;
            ([p0, p1, p2], w)
        })
        .collect()
}

/// Assemble degree-2 pieces (control triples + middle weights) into one spline, piece `i`
/// spanning parameters `[i, i+1]`.
fn assemble_quadratic(pieces: &[([DVec2; 3], f64)], m: &DAffine2) -> Option<Nurbs2> {
    if pieces.is_empty() {
        return None;
    }
    let mut ctrl = Vec::with_capacity(pieces.len() * 2 + 1);
    let mut weights = Vec::with_capacity(pieces.len() * 2 + 1);
    let mut knots = vec![0.0, 0.0, 0.0];
    for (i, (p, w)) in pieces.iter().enumerate() {
        if i == 0 {
            ctrl.push(m.transform_point2(p[0]));
            weights.push(1.0);
        }
        ctrl.push(m.transform_point2(p[1]));
        weights.push(*w);
        ctrl.push(m.transform_point2(p[2]));
        weights.push(1.0);
        let k = (i + 1) as f64;
        if i + 1 < pieces.len() {
            knots.push(k);
            knots.push(k);
        } else {
            knots.extend([k, k, k]);
        }
    }
    let rational = weights.iter().any(|w| (*w - 1.0).abs() > 1e-15);
    Some(Nurbs2 {
        degree: 2,
        ctrl,
        weights: if rational { weights } else { Vec::new() },
        knots,
        fit_points: Vec::new(),
        closed: false,
    })
}

fn unit_to(c: DVec2, x: DVec2, y: DVec2) -> DAffine2 {
    DAffine2::from_cols(x, y, c)
}

impl Line2 {
    pub fn to_nurbs(&self) -> Nurbs2 {
        Nurbs2 {
            degree: 1,
            ctrl: vec![self.a, self.b],
            weights: Vec::new(),
            knots: vec![0.0, 0.0, 1.0, 1.0],
            fit_points: Vec::new(),
            closed: false,
        }
    }
}

impl Arc2 {
    /// Exact rational quadratic representation.
    pub fn to_nurbs(&self) -> Nurbs2 {
        let m = unit_to(self.c, DVec2::X * self.r, DVec2::Y * self.r);
        assemble_quadratic(&unit_arc_pieces(self.start, self.sweep()), &m).unwrap_or_default()
    }
}

impl Circle2 {
    pub fn to_nurbs(&self) -> Nurbs2 {
        let m = unit_to(self.c, DVec2::X * self.r, DVec2::Y * self.r);
        let mut n = assemble_quadratic(&unit_arc_pieces(0.0, std::f64::consts::TAU), &m)
            .unwrap_or_default();
        n.closed = true;
        n
    }
}

impl EllipseArc2 {
    /// Exact rational quadratic representation.
    pub fn to_nurbs(&self) -> Nurbs2 {
        let m = unit_to(self.c, self.major, self.minor());
        let sweep = if self.is_full() {
            std::f64::consts::TAU
        } else {
            wcad_math::ccw_sweep(self.start, self.end)
        };
        let mut n = assemble_quadratic(&unit_arc_pieces(self.start, sweep), &m).unwrap_or_default();
        n.closed = self.is_full();
        n
    }
}

impl Polyline2 {
    /// Exact degree-2 rational representation (lines are degree-elevated). `None` for fewer than
    /// two vertices.
    pub fn to_nurbs(&self) -> Option<Nurbs2> {
        let mut pieces = Vec::new();
        for s in self.segments() {
            match s {
                PolySegment::Line(l) => pieces.push(([l.a, l.midpoint(), l.b], 1.0)),
                PolySegment::Arc(a) => {
                    let m = unit_to(a.c, DVec2::X * a.r, DVec2::Y * a.r);
                    for (p, w) in unit_arc_pieces(a.start, a.sweep) {
                        pieces.push((
                            [
                                m.transform_point2(p[0]),
                                m.transform_point2(p[1]),
                                m.transform_point2(p[2]),
                            ],
                            w,
                        ));
                    }
                }
            }
        }
        let mut n = assemble_quadratic(&pieces, &DAffine2::IDENTITY)?;
        n.closed = self.closed;
        Some(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn cubic() -> Nurbs2 {
        Nurbs2 {
            degree: 3,
            ctrl: vec![
                DVec2::new(0.0, 0.0),
                DVec2::new(1.0, 2.0),
                DVec2::new(3.0, 2.0),
                DVec2::new(4.0, 0.0),
                DVec2::new(6.0, 1.0),
            ],
            weights: vec![],
            knots: vec![0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0],
            fit_points: vec![],
            closed: false,
        }
    }

    #[test]
    fn circle_nurbs_is_exact() {
        let c = Circle2::new(DVec2::new(1.0, 2.0), 3.0).to_nurbs();
        c.validate().unwrap();
        let (d0, d1) = c.domain();
        for i in 0..=100 {
            let u = d0 + (d1 - d0) * i as f64 / 100.0;
            let [p, d, _] = c.eval_derivs(u);
            assert!(
                (p.distance(DVec2::new(1.0, 2.0)) - 3.0).abs() < 1e-12,
                "u={u}"
            );
            // derivative is tangent (perpendicular to radius)
            assert!(d.dot(p - DVec2::new(1.0, 2.0)).abs() < 1e-9 * d.length());
        }
        let e = EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(4.0, 0.0),
            ratio: 0.5,
            start: 0.3,
            end: 2.0,
        }
        .to_nurbs();
        let (d0, d1) = e.domain();
        for i in 0..=50 {
            let p = e.point_at(d0 + (d1 - d0) * i as f64 / 50.0);
            assert!(((p.x / 4.0).powi(2) + (p.y / 2.0).powi(2) - 1.0).abs() < 1e-12);
        }
        assert!(
            (e.point_at(d0) - DVec2::new(4.0 * 0.3f64.cos(), 2.0 * 0.3f64.sin())).length() < 1e-12
        );
    }

    #[test]
    fn derivatives_match_finite_differences() {
        let c = Arc2::new(DVec2::ZERO, 2.0, 0.2, 2.9).to_nurbs();
        for s in [cubic(), c] {
            let (d0, d1) = s.domain();
            for i in 1..20 {
                let u = d0 + (d1 - d0) * (i as f64 + 0.37) / 21.0;
                let h = 1e-6 * (d1 - d0);
                let [_, d, dd] = s.eval_derivs(u);
                let fd = (s.point_at(u + h) - s.point_at(u - h)) / (2.0 * h);
                let fdd = (s.eval_derivs(u + h)[1] - s.eval_derivs(u - h)[1]) / (2.0 * h);
                assert!((d - fd).length() < 1e-5 * (1.0 + d.length()), "{d} {fd}");
                assert!(
                    (dd - fdd).length() < 1e-4 * (1.0 + dd.length()),
                    "{dd} {fdd}"
                );
            }
        }
    }

    #[test]
    fn knot_insertion_and_sub_curve() {
        let s = cubic();
        let t = s.insert_knot(0.3, 2);
        for i in 0..=20 {
            let u = i as f64 / 20.0;
            assert!((s.point_at(u) - t.point_at(u)).length() < 1e-12);
        }
        let sub = s.sub_curve(0.2, 0.7).unwrap();
        sub.validate().unwrap();
        assert!((sub.point_at(0.2) - s.point_at(0.2)).length() < 1e-12);
        assert!((sub.point_at(0.7) - s.point_at(0.7)).length() < 1e-12);
        assert!((sub.point_at(0.45) - s.point_at(0.45)).length() < 1e-12);
        assert_eq!(sub.ctrl[0], sub.point_at(0.2));
        let r = s.reversed();
        assert!((r.point_at(0.25) - s.point_at(0.75)).length() < 1e-12);
        // rational sub-curve stays on the circle
        let c = Circle2::new(DVec2::ZERO, 1.0).to_nurbs();
        let sc = c.sub_curve(0.5, 2.5).unwrap();
        for i in 0..=10 {
            let p = sc.point_at(0.5 + 0.2 * i as f64);
            assert!((p.length() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn concat_and_elevate() {
        let s = cubic();
        let a = s.sub_curve(0.0, 0.4).unwrap();
        let b = s.sub_curve(0.4, 1.0).unwrap();
        let j = a.concat(&b).unwrap();
        j.validate().unwrap();
        for i in 0..=20 {
            let u = i as f64 / 20.0;
            assert!((j.point_at(u) - s.point_at(u)).length() < 1e-12, "u={u}");
        }
        let e = Arc2::new(DVec2::ZERO, 1.0, 0.0, PI)
            .to_nurbs()
            .elevated()
            .unwrap();
        assert_eq!(e.degree, 3);
        let (d0, d1) = e.domain();
        for i in 0..=10 {
            let p = e.point_at(d0 + (d1 - d0) * i as f64 / 10.0);
            assert!((p.length() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn fit_points_interpolated() {
        let pts: Vec<DVec2> = (0..12)
            .map(|i| DVec2::new(i as f64, (i as f64 * 0.7).sin() * 3.0))
            .collect();
        let s = Nurbs2::from_fit_points(&pts, 3).unwrap();
        s.validate().unwrap();
        // every fit point lies on the curve (closest-point check via dense sampling + known params)
        let mut ub = vec![0.0];
        let mut tot = 0.0;
        for i in 1..pts.len() {
            tot += pts[i].distance(pts[i - 1]);
            ub.push(tot);
        }
        for (i, p) in pts.iter().enumerate() {
            assert!((s.point_at(ub[i] / tot) - *p).length() < 1e-9, "i={i}");
        }
        let two = Nurbs2::from_fit_points(&[DVec2::ZERO, DVec2::ONE], 3).unwrap();
        assert_eq!(two.degree, 1);
        assert!(Nurbs2::from_fit_points(&[DVec2::ZERO, DVec2::ZERO], 3).is_err());
    }

    #[test]
    fn invalid_splines_detected() {
        let mut s = cubic();
        s.knots.pop();
        assert!(!s.is_valid());
        let mut s = cubic();
        s.weights = vec![1.0, -1.0, 1.0, 1.0, 1.0];
        assert!(s.validate().is_err());
    }
}
