//! Rank / DOF / redundancy / conflict analysis.
//!
//! The Jacobian rows (unit-normalized) of each cluster are processed in equation order — which is
//! constraint creation order — by a rank-revealing envelope Cholesky of the Gram matrix `J Jᵀ`.
//! A (near) zero pivot marks a row that is a linear combination of earlier rows: its factor row
//! gives the combination coefficients (one triangular solve), the members of the dependency are
//! the rows with non-zero coefficients, and the same combination applied to the residuals tells a
//! consistent (redundant) dependency from an inconsistent (conflicting) one. A parameter is
//! determined when its unit vector lies in the row space of `J` (`|L⁻¹ Jᵀ e_j|² = 1`).
//!
//! Cost is `O(profile · bandwidth)` like the solve, instead of the dense Gram-Schmidt's
//! `O(m · n · rank)`.

use std::collections::BTreeMap;

use super::lm::{Jac, clusters, scale_of};
use super::linalg::Skyline;
use super::system::{Owner, ParamOf, System};
use super::{DependencyGroup, Diagnosis, EntityStatus, Param, SolveOptions};
use crate::model::{SkConstraintId, Sketch};

fn param_public(p: ParamOf) -> Param {
    match p {
        ParamOf::X(e) => Param::X(e),
        ParamOf::Y(e) => Param::Y(e),
        ParamOf::Radius(e) => Param::Radius(e),
    }
}

/// Add or merge a group (a constraint with several equations may produce several dependencies).
fn push_group(groups: &mut Vec<DependencyGroup>, g: DependencyGroup) {
    if let Some(gr) = groups.iter_mut().find(|x| x.constraint == g.constraint) {
        gr.members.extend(g.members);
        gr.members.sort_unstable();
        gr.members.dedup();
        gr.conflicting |= g.conflicting;
        gr.inconsistency = gr.inconsistency.abs().max(g.inconsistency.abs());
    } else {
        groups.push(g);
    }
}

/// Build a group from its owners; the dependent constraint is `dependent`'s constraint or, for an
/// arc-internal row, the most recent member.
fn make_group(dependent: Owner, mut members: Vec<SkConstraintId>, conflicting: bool, inconsistency: f64) -> Option<DependencyGroup> {
    if let Some(c) = dependent.constraint() {
        members.push(c);
    }
    members.sort_unstable();
    members.dedup();
    let constraint = dependent.constraint().or_else(|| members.last().copied())?;
    Some(DependencyGroup { constraint, members, conflicting, inconsistency })
}

pub(crate) fn diagnose(sk: &Sketch, sys: &System, opt: &SolveOptions) -> Diagnosis {
    let n_all = sys.x.len();
    let (cls, constant) = clusters(sys);
    let mut d = Diagnosis {
        n_params: sys.num_free_params(),
        n_equations: sys.eqs.len(),
        invalid: sys.invalid.clone(),
        ..Default::default()
    };
    let mut determined: Vec<bool> = sys.locked.clone();
    let mut grad = Vec::new();

    // Equations over locked parameters only: satisfied (redundant) or not (conflicting). The Fix
    // constraints of the referenced points take part in the dependency.
    for &k in &constant {
        let e = &sys.eqs[k as usize];
        let f = e.eval(&sys.x, &mut grad);
        let params: Vec<u32> = grad.iter().map(|g| g.0).collect();
        let tol = conflict_tol(opt) * scale_of(&sys.x, &params);
        let members: Vec<SkConstraintId> = params.iter().filter_map(|&p| sys.fix_of[p as usize]).collect();
        if let Some(g) = make_group(e.owner, members, f.abs() > tol || !f.is_finite(), f) {
            push_group(&mut d.groups, g);
        }
    }
    for &(dup, orig) in &sys.duplicate_fixes {
        push_group(
            &mut d.groups,
            DependencyGroup { constraint: dup, members: vec![orig.min(dup), orig.max(dup)], conflicting: false, inconsistency: 0.0 },
        );
    }

    let mut local = vec![u32::MAX; n_all];
    let mut jac = Jac::default();
    let mut g = Skyline::default();
    let mut dep: Vec<bool> = Vec::new();
    let mut rhs: Vec<f64> = Vec::new();
    let mut coef: Vec<f64> = Vec::new();
    for cl in &cls {
        let (m, n) = (cl.eqs.len(), cl.params.len());
        for (li, &p) in cl.params.iter().enumerate() {
            local[p as usize] = li as u32;
        }
        let scale = scale_of(&sys.x, &cl.params);
        let ctol = conflict_tol(opt) * scale;
        let groups_before = d.groups.len();
        jac.eval(&sys.eqs, &sys.x, &cl.eqs, &local, &mut grad);
        // Normalize rows; zero rows are dependent from the start.
        dep.clear();
        dep.resize(m, false);
        for i in 0..m {
            let r = jac.rp[i]..jac.rp[i + 1];
            let nr = jac.rv[r.clone()].iter().map(|v| v * v).sum::<f64>().sqrt();
            let owner = sys.eqs[cl.eqs[i] as usize].owner;
            if !(nr > 1e-300) || !nr.is_finite() {
                dep[i] = true;
                let fi = jac.f[i];
                if let Some(gr) = make_group(owner, Vec::new(), fi.abs() > ctol || !fi.is_finite(), fi) {
                    push_group(&mut d.groups, gr);
                }
                jac.rv[r].iter_mut().for_each(|v| *v = 0.0);
                continue;
            }
            jac.rv[r].iter_mut().for_each(|v| *v /= nr);
            jac.f[i] /= nr;
        }
        jac.transpose(n);
        // Gram matrix structure: row i starts at the first row sharing a column with it.
        let mut first: Vec<usize> = (0..m).collect();
        for c in 0..n {
            let rows = &jac.ci[jac.cp[c]..jac.cp[c + 1]];
            if let Some(&lo) = rows.first() {
                for &r in rows {
                    first[r as usize] = first[r as usize].min(lo as usize);
                }
            }
        }
        g.set_structure(&first);
        for c in 0..n {
            for a in jac.cp[c]..jac.cp[c + 1] {
                for b in jac.cp[c]..=a {
                    g.add(jac.ci[a] as usize, jac.ci[b] as usize, jac.cv[a] * jac.cv[b]);
                }
            }
        }
        // Rank-revealing Cholesky in equation order.
        rhs.clear();
        rhs.resize(m, 0.0);
        coef.clear();
        coef.resize(m, 0.0);
        let mut rank = 0;
        for i in 0..m {
            let fi = g.first[i];
            if dep[i] {
                let (s, e) = (g.start[i], g.start[i + 1]);
                g.a[s..e].iter_mut().for_each(|v| *v = 0.0);
                continue;
            }
            for j in fi..i {
                let ij = g.idx(i, j);
                if dep[j] {
                    g.a[ij] = 0.0;
                    continue;
                }
                let s = g.a[ij] - g.row_dot(i, j, fi.max(g.first[j]));
                g.a[ij] = s / g.diag(j);
            }
            let ii = g.idx(i, i);
            let piv = g.a[ii] - g.row_dot(i, i, fi);
            if piv > opt.pivot_tol && piv.is_finite() {
                g.a[ii] = piv.sqrt();
                rank += 1;
                continue;
            }
            // Dependent row: solve Lᵀ c = l_i over the independent rows before i.
            for j in fi..i {
                rhs[j] = g.a[g.idx(i, j)];
            }
            let mut lo = fi;
            let mut j = i;
            while j > lo {
                j -= 1;
                if dep[j] || rhs[j] == 0.0 {
                    coef[j] = 0.0;
                    rhs[j] = 0.0;
                    continue;
                }
                let cj = rhs[j] / g.diag(j);
                coef[j] = cj;
                rhs[j] = 0.0;
                let fj = g.first[j];
                let row = &g.a[g.start[j]..g.start[j] + (j - fj)];
                for (k, l) in (fj..j).zip(row) {
                    rhs[k] -= l * cj;
                }
                lo = lo.min(fj);
            }
            let mut incons = jac.f[i];
            let mut members = Vec::new();
            for k in lo..i {
                let c = coef[k];
                if c != 0.0 {
                    incons -= c * jac.f[k];
                    if c.abs() > 1e-6
                        && let Some(id) = sys.eqs[cl.eqs[k] as usize].owner.constraint()
                    {
                        members.push(id);
                    }
                    coef[k] = 0.0;
                }
            }
            dep[i] = true;
            let (s, e) = (g.start[i], g.start[i + 1]);
            g.a[s..e].iter_mut().for_each(|v| *v = 0.0);
            let owner = sys.eqs[cl.eqs[i] as usize].owner;
            if let Some(gr) = make_group(owner, members, incons.abs() > ctol || !incons.is_finite(), incons) {
                push_group(&mut d.groups, gr);
            }
        }
        // Fallback: the solve stopped near (not at) a least-squares stationary point, so J is only
        // nearly rank deficient. There F/|F| approximates a left-null vector of J: the equations
        // with non-zero residual are the ones fighting each other.
        if !d.groups[groups_before..].iter().any(|g| g.conflicting) {
            let mut members = Vec::new();
            let mut worst = 0.0f64;
            let mut last_owner = None;
            for &k in &cl.eqs {
                let e = &sys.eqs[k as usize];
                let f = e.eval(&sys.x, &mut grad).abs();
                if f > ctol || !f.is_finite() {
                    if let Some(c) = e.owner.constraint() {
                        members.push(c);
                    }
                    last_owner = Some(e.owner);
                    worst = worst.max(f);
                }
            }
            if last_owner.is_some() {
                members.sort_unstable();
                members.dedup();
                if let Some(&last) = members.last() {
                    push_group(
                        &mut d.groups,
                        DependencyGroup { constraint: last, members, conflicting: true, inconsistency: worst },
                    );
                }
            }
        }
        d.rank += rank;
        // Determined parameters.
        if rank == n {
            for &p in &cl.params {
                determined[p as usize] = true;
            }
        } else {
            let y = &mut rhs;
            for (j, &p) in cl.params.iter().enumerate() {
                let (c0, c1) = (jac.cp[j], jac.cp[j + 1]);
                let Some(&r0) = jac.ci[c0..c1].first() else { continue };
                for e in c0..c1 {
                    y[jac.ci[e] as usize] = jac.cv[e];
                }
                let mut s = 0.0;
                let mut hi = r0 as usize; // last row with a non-zero y
                for i in r0 as usize..m {
                    if dep[i] {
                        y[i] = 0.0;
                        continue;
                    }
                    let fi = g.first[i];
                    if fi > hi && y[i] == 0.0 {
                        continue;
                    }
                    let row = &g.a[g.start[i]..g.start[i + 1]];
                    let mut v = y[i];
                    for (k, l) in (fi..i).zip(row) {
                        v -= l * y[k];
                    }
                    v /= row[i - fi];
                    y[i] = v;
                    if v != 0.0 {
                        s += v * v;
                        hi = i;
                    }
                }
                y[r0 as usize..].iter_mut().for_each(|v| *v = 0.0);
                determined[p as usize] = 1.0 - s < 1e-6;
            }
        }
        for &p in &cl.params {
            local[p as usize] = u32::MAX;
        }
    }
    d.dof = d.n_params.saturating_sub(d.rank);
    d.free_params = (0..n_all).filter(|&p| !determined[p]).map(|p| param_public(sys.param_of[p])).collect();
    for gr in &d.groups {
        if gr.conflicting {
            d.conflicting.extend(&gr.members);
            d.conflicting_dependents.push(gr.constraint);
        } else {
            d.redundant.push(gr.constraint);
        }
    }
    for v in [&mut d.conflicting, &mut d.redundant, &mut d.conflicting_dependents] {
        v.sort_unstable();
        v.dedup();
    }
    // Per-entity status.
    let mut in_conflict: BTreeMap<crate::SkEntityId, ()> = BTreeMap::new();
    for c in &d.conflicting {
        if let Some(con) = sk.constraints.get(c) {
            for e in con.kind.entities() {
                in_conflict.insert(e, ());
                if let Some(ent) = sk.entities.get(&e) {
                    for p in ent.geom.defining_points() {
                        in_conflict.insert(p, ());
                    }
                }
            }
        }
    }
    let mut params = Vec::new();
    for &id in sk.entities.keys() {
        sys.entity_params(sk, id, &mut params);
        let status = if in_conflict.contains_key(&id) {
            EntityStatus::OverConstrained
        } else if params.iter().all(|&p| determined[p as usize]) {
            EntityStatus::FullyConstrained
        } else {
            EntityStatus::UnderConstrained
        };
        d.entities.insert(id, status);
    }
    d
}

fn conflict_tol(opt: &SolveOptions) -> f64 {
    (opt.tol * 1e3).max(1e-7)
}
