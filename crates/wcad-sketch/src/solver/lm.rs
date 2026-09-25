//! Clustering and the damped Gauss-Newton / Levenberg-Marquardt solve.
//!
//! Step (per cluster, W = diag movement weights, mu = damping):
//!   dx = argmin |dx|_W² + |J dx + F|² / mu   (LM with damping matrix mu·W)
//!      = -(Jᵀ J + mu W)⁻¹ Jᵀ F                     ("primal", n × n)
//!      = -W⁻¹ Jᵀ (J W⁻¹ Jᵀ + mu I)⁻¹ F            ("dual",   m × m)
//! Both are the same step (push-through identity); the smaller system is used. As mu → 0 the
//! step becomes the minimum-W-norm Gauss-Newton step, so an under-constrained sketch moves as
//! little as possible from its current state, and heavily weighted (dragged) parameters move least.

use super::linalg::{Skyline, rcm};
use super::system::{Equation, System};
use super::terms::Pt;
use super::SolveOptions;

/// Connected component of the (unlocked) parameter / equation graph.
#[derive(Clone, Debug, Default)]
pub(crate) struct Cluster {
    /// Sorted global parameter indices.
    pub(crate) params: Vec<u32>,
    /// Sorted equation indices (system order).
    pub(crate) eqs: Vec<u32>,
}

fn find(p: &mut [u32], mut i: u32) -> u32 {
    while p[i as usize] != i {
        let g = p[p[i as usize] as usize];
        p[i as usize] = g;
        i = g;
    }
    i
}

/// Connected components over the equations, ignoring locked parameters. Returns the clusters
/// (parameters in no equation are omitted) and the equations without any unlocked parameter.
pub(crate) fn clusters(sys: &System) -> (Vec<Cluster>, Vec<u32>) {
    let n = sys.x.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    let mut touched = vec![false; n];
    let mut first = vec![u32::MAX; sys.eqs.len()];
    let mut constant = Vec::new();
    let mut g = Vec::new();
    for (k, e) in sys.eqs.iter().enumerate() {
        e.eval(&sys.x, &mut g);
        let mut r0 = u32::MAX;
        for &(p, _) in &g {
            if sys.locked[p as usize] {
                continue;
            }
            touched[p as usize] = true;
            if r0 == u32::MAX {
                r0 = find(&mut parent, p);
                first[k] = p;
            } else {
                let r = find(&mut parent, p);
                if r != r0 {
                    parent[r as usize] = r0;
                }
            }
        }
        if r0 == u32::MAX {
            constant.push(k as u32);
        }
    }
    let mut cid = vec![u32::MAX; n];
    let mut out: Vec<Cluster> = Vec::new();
    for p in 0..n as u32 {
        if touched[p as usize] {
            let r = find(&mut parent, p) as usize;
            if cid[r] == u32::MAX {
                cid[r] = out.len() as u32;
                out.push(Cluster::default());
            }
            out[cid[r] as usize].params.push(p);
        }
    }
    for (k, &p) in first.iter().enumerate() {
        if p != u32::MAX {
            let r = find(&mut parent, p) as usize;
            out[cid[r] as usize].eqs.push(k as u32);
        }
    }
    (out, constant)
}

/// Sparse Jacobian of a cluster in CSR (local columns) plus residuals, and its CSC transpose.
#[derive(Default)]
pub(crate) struct Jac {
    pub(crate) rp: Vec<usize>,
    pub(crate) ri: Vec<u32>,
    pub(crate) rv: Vec<f64>,
    pub(crate) f: Vec<f64>,
    pub(crate) cp: Vec<usize>,
    pub(crate) ci: Vec<u32>,
    pub(crate) cv: Vec<f64>,
}

impl Jac {
    /// Evaluate residuals and Jacobian rows. `local[p]` maps global params to local columns
    /// (`u32::MAX` = locked / not in the cluster: dropped). Returns `|F|²`.
    pub(crate) fn eval(&mut self, eqs: &[Equation], x: &[f64], cl_eqs: &[u32], local: &[u32], grad: &mut Vec<(u32, f64)>) -> f64 {
        self.f.clear();
        self.rp.clear();
        self.ri.clear();
        self.rv.clear();
        self.rp.push(0);
        let mut s = 0.0;
        for &k in cl_eqs {
            let v = eqs[k as usize].eval(x, grad);
            s += v * v;
            self.f.push(v);
            for &(p, d) in grad.iter() {
                let l = local[p as usize];
                if l != u32::MAX {
                    self.ri.push(l);
                    self.rv.push(d);
                }
            }
            self.rp.push(self.ri.len());
        }
        s
    }

    /// Build the CSC transpose for `n` columns (rows within a column ascending).
    pub(crate) fn transpose(&mut self, n: usize) {
        let m = self.f.len();
        self.cp.clear();
        self.cp.resize(n + 1, 0);
        for &c in &self.ri {
            self.cp[c as usize + 1] += 1;
        }
        for j in 0..n {
            self.cp[j + 1] += self.cp[j];
        }
        self.ci.resize(self.ri.len(), 0);
        self.cv.resize(self.ri.len(), 0.0);
        let mut next: Vec<usize> = self.cp[..n].to_vec();
        for i in 0..m {
            for e in self.rp[i]..self.rp[i + 1] {
                let c = self.ri[e] as usize;
                let pos = next[c];
                self.ci[pos] = i as u32;
                self.cv[pos] = self.rv[e];
                next[c] += 1;
            }
        }
    }
}

/// Envelope `first[]` of the normal matrix whose rows/cols are "objects" (equations for the dual,
/// parameters for the primal) in the order `pos[object]`; `groups` lists the objects coupled by
/// each shared index (a column of J for the dual, a row of J for the primal).
fn envelope(dim: usize, groups: impl Iterator<Item = Vec<u32>>, pos: &[u32]) -> Vec<usize> {
    let mut first: Vec<usize> = (0..dim).collect();
    for g in groups {
        let lo = g.iter().map(|&o| pos[o as usize] as usize).min().unwrap_or(0);
        for &o in &g {
            let p = pos[o as usize] as usize;
            if lo < first[p] {
                first[p] = lo;
            }
        }
    }
    first
}

fn profile(first: &[usize]) -> usize {
    first.iter().enumerate().map(|(i, &f)| i + 1 - f).sum()
}

/// Ordering of the normal matrix: natural, or reverse Cuthill-McKee when that is clearly narrower.
/// Returns (`pos[object]`, envelope `first`).
fn ordering(dim: usize, groups: &[Vec<u32>]) -> (Vec<u32>, Vec<usize>) {
    let natural: Vec<u32> = (0..dim as u32).collect();
    let nat_first = envelope(dim, groups.iter().cloned(), &natural);
    let nat_profile = profile(&nat_first);
    // Cheap heuristic: only try RCM when the natural profile is far above a banded matrix's.
    if dim < 64 || nat_profile < 16 * dim {
        return (natural, nat_first);
    }
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); dim];
    for g in groups {
        for &a in g {
            for &b in g {
                if a != b {
                    adj[a as usize].push(b);
                }
            }
        }
    }
    for v in &mut adj {
        v.sort_unstable();
        v.dedup();
    }
    let perm = rcm(&adj);
    let mut pos = vec![0u32; dim];
    for (new, &old) in perm.iter().enumerate() {
        pos[old as usize] = new as u32;
    }
    let first = envelope(dim, groups.iter().cloned(), &pos);
    if profile(&first) < nat_profile {
        (pos, first)
    } else {
        (natural, nat_first)
    }
}

/// Scratch buffers reused across clusters and iterations.
#[derive(Default)]
pub(crate) struct Work {
    pub(crate) local: Vec<u32>,
    pub(crate) grad: Vec<(u32, f64)>,
    jac: Jac,
    ft: Vec<f64>,
    base: Skyline,
    fac: Skyline,
    rhs: Vec<f64>,
    dx: Vec<f64>,
    x0: Vec<f64>,
}

impl Work {
    pub(crate) fn new(n_params: usize) -> Self {
        Work { local: vec![u32::MAX; n_params], ..Default::default() }
    }
}

fn eval_f(eqs: &[Equation], x: &[f64], cl: &Cluster, f: &mut Vec<f64>, g: &mut Vec<(u32, f64)>) -> f64 {
    f.clear();
    let mut s = 0.0;
    for &k in &cl.eqs {
        let v = eqs[k as usize].eval(x, g);
        s += v * v;
        f.push(v);
    }
    s
}

pub(crate) fn scale_of(x: &[f64], params: &[u32]) -> f64 {
    1.0 + params.iter().map(|&p| x[p as usize].abs()).filter(|v| v.is_finite()).fold(0.0, f64::max)
}

/// CCW sweep of an arc in `[0, 2π)`, `None` for (near) zero radius.
fn sweep(x: &[f64], [c, s, e]: [Pt; 3]) -> Option<f64> {
    let get = |p: Pt| (x[p.x as usize], x[p.y as usize]);
    let (c, s, e) = (get(c), get(s), get(e));
    let (us, ue) = ((s.0 - c.0, s.1 - c.1), (e.0 - c.0, e.1 - c.1));
    if us.0.hypot(us.1) < 1e-12 || ue.0.hypot(ue.1) < 1e-12 {
        return None;
    }
    Some(wcad_math::normalize_0_2pi(ue.1.atan2(ue.0) - us.1.atan2(us.0)))
}

/// Result of one cluster solve.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ClusterResult {
    pub(crate) converged: bool,
    pub(crate) iterations: u32,
    pub(crate) max_residual: f64,
    pub(crate) norm2: f64,
}

/// Solve one cluster in place. `winv` = inverse movement weights per local param. With
/// `arcs` non-empty, steps that flip an arc (change its sweep by more than π) are rejected.
pub(crate) fn solve_cluster(
    eqs: &[Equation],
    x: &mut [f64],
    cl: &Cluster,
    winv: &[f64],
    arcs: &[[Pt; 3]],
    opt: &SolveOptions,
    w: &mut Work,
) -> ClusterResult {
    let (m, n) = (cl.eqs.len(), cl.params.len());
    for (li, &p) in cl.params.iter().enumerate() {
        w.local[p as usize] = li as u32;
    }
    let tol = opt.tol * scale_of(x, &cl.params);
    let dual = m <= n;
    let dim = if dual { m } else { n };
    let mut norm2 = w.jac.eval(eqs, x, &cl.eqs, &w.local, &mut w.grad);
    w.jac.transpose(n);
    // Structure of J is fixed during the solve: pick the ordering once.
    let groups: Vec<Vec<u32>> = if dual {
        (0..n).map(|c| w.jac.ci[w.jac.cp[c]..w.jac.cp[c + 1]].to_vec()).collect()
    } else {
        (0..m).map(|i| w.jac.ri[w.jac.rp[i]..w.jac.rp[i + 1]].to_vec()).collect()
    };
    let (pos, first) = ordering(dim, &groups);
    drop(groups);
    let mut sweeps: Vec<Option<f64>> = Vec::with_capacity(arcs.len());
    let mut mu = 1e-12_f64;
    let mut iters = 0;
    let mut stall = 0;
    let mut converged = false;
    loop {
        if w.jac.f.iter().all(|v| v.abs() <= tol) {
            converged = true;
            break;
        }
        if !norm2.is_finite() || iters >= opt.max_iter || stall >= 4 {
            break;
        }
        iters += 1;
        let jac = &w.jac;
        // ---- assemble the undamped normal matrix
        w.base.set_structure(&first);
        if dual {
            for c in 0..n {
                let wi = winv[c];
                for a in jac.cp[c]..jac.cp[c + 1] {
                    let pa = pos[jac.ci[a] as usize] as usize;
                    for b in jac.cp[c]..=a {
                        let pb = pos[jac.ci[b] as usize] as usize;
                        w.base.add(pa, pb, jac.cv[a] * jac.cv[b] * wi);
                    }
                }
            }
        } else {
            for i in 0..m {
                for a in jac.rp[i]..jac.rp[i + 1] {
                    let pa = pos[jac.ri[a] as usize] as usize;
                    for b in jac.rp[i]..=a {
                        let pb = pos[jac.ri[b] as usize] as usize;
                        w.base.add(pa, pb, jac.rv[a] * jac.rv[b]);
                    }
                }
            }
        }
        let md = w.base.max_diag().max(1e-300);
        w.x0.clear();
        w.x0.extend(cl.params.iter().map(|&p| x[p as usize]));
        sweeps.clear();
        sweeps.extend(arcs.iter().map(|&a| sweep(x, a)));
        // ---- damping loop
        let mut accepted = false;
        while mu < 1e10 {
            w.fac.copy_from(&w.base);
            if dual {
                for i in 0..m {
                    w.fac.add_diag(i, mu * md);
                }
            } else {
                for (j, &wi) in winv.iter().enumerate() {
                    w.fac.add_diag(pos[j] as usize, mu * md / wi);
                }
            }
            w.rhs.clear();
            w.rhs.resize(dim, 0.0);
            if dual {
                for (i, &fi) in jac.f.iter().enumerate() {
                    w.rhs[pos[i] as usize] = fi;
                }
            } else {
                for (i, &fi) in jac.f.iter().enumerate() {
                    for e in jac.rp[i]..jac.rp[i + 1] {
                        w.rhs[pos[jac.ri[e] as usize] as usize] += jac.rv[e] * fi;
                    }
                }
            }
            if !w.fac.cholesky() {
                mu *= 100.0;
                continue;
            }
            w.fac.solve(&mut w.rhs);
            w.dx.clear();
            w.dx.resize(n, 0.0);
            if dual {
                for i in 0..m {
                    let yi = w.rhs[pos[i] as usize];
                    for e in jac.rp[i]..jac.rp[i + 1] {
                        w.dx[jac.ri[e] as usize] += jac.rv[e] * yi;
                    }
                }
                for (d, &wi) in w.dx.iter_mut().zip(winv) {
                    *d *= -wi;
                }
            } else {
                for (j, d) in w.dx.iter_mut().enumerate() {
                    *d = -w.rhs[pos[j] as usize];
                }
            }
            for (j, &p) in cl.params.iter().enumerate() {
                x[p as usize] = w.x0[j] + w.dx[j];
            }
            let flips = arcs.iter().zip(&sweeps).any(|(&a, &s0)| match (s0, sweep(x, a)) {
                (Some(s0), Some(s1)) => (s1 - s0).abs() > std::f64::consts::PI,
                _ => false,
            });
            let nt = eval_f(eqs, x, cl, &mut w.ft, &mut w.grad);
            if !flips && nt.is_finite() && nt < norm2 {
                stall = if nt > norm2 * (1.0 - 1e-9) { stall + 1 } else { 0 };
                norm2 = nt;
                mu = (mu * 0.1).max(1e-14);
                accepted = true;
                break;
            }
            for (j, &p) in cl.params.iter().enumerate() {
                x[p as usize] = w.x0[j];
            }
            mu *= 10.0;
        }
        if !accepted {
            break;
        }
        norm2 = w.jac.eval(eqs, x, &cl.eqs, &w.local, &mut w.grad);
        w.jac.transpose(n);
    }
    let max_residual = w.jac.f.iter().fold(0.0, |a: f64, v| a.max(v.abs()));
    for &p in &cl.params {
        w.local[p as usize] = u32::MAX;
    }
    ClusterResult { converged, iterations: iters, max_residual, norm2 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_uses_rcm_for_scattered_structure() {
        // a chain whose objects are numbered in a scrambled order
        let dim = 100;
        let mut label: Vec<u32> = (0..dim as u32).collect();
        let mut s: u64 = 3;
        for i in (1..dim).rev() {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            label.swap(i, (s >> 33) as usize % (i + 1));
        }
        let groups: Vec<Vec<u32>> = (0..dim - 1).map(|i| vec![label[i], label[i + 1]]).collect();
        let natural: Vec<u32> = (0..dim as u32).collect();
        let nat = profile(&envelope(dim, groups.iter().cloned(), &natural));
        let (pos, first) = ordering(dim, &groups);
        let mut sorted = pos.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, natural);
        assert!(profile(&first) < nat / 4, "{} vs {nat}", profile(&first));
        // every coupled pair lies inside the envelope
        for g in &groups {
            let (a, b) = (pos[g[0] as usize] as usize, pos[g[1] as usize] as usize);
            let (hi, lo) = (a.max(b), a.min(b));
            assert!(first[hi] <= lo);
        }
    }
}
