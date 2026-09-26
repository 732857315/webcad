//! In-house geometric constraint solver (production version of the
//! `.scratch/constraint-solver/sketch-solver` prototype).
//!
//! [`Sketch::solve`] builds the parameter vector and equations from the model ([`system`]), splits
//! them into independent clusters, runs a Levenberg-Marquardt iteration with minimum-movement steps
//! per cluster ([`lm`]) and writes points/radii back. [`Sketch::drag`] does the same SolveSpace
//! style for the clusters of a dragged point, [`Sketch::diagnose`] reports DOF, per-entity
//! freedom, redundant and conflicting constraints ([`diagnose`]).

// Index loops over parallel arrays are the clearest form for the numeric kernels here.
#![allow(clippy::needless_range_loop)]

mod diagnose;
mod linalg;
mod lm;
pub(crate) mod system;
mod terms;

use std::collections::BTreeMap;

use wcad_math::DVec2;

use crate::model::{ConstraintKind, SkConstraintId, SkEntityId, SkGeom, Sketch};
use lm::{Work, clusters, scale_of, solve_cluster};
use system::System;

#[derive(Clone, Debug, PartialEq)]
pub struct SolveOptions {
    /// Residual tolerance, scaled by `1 + max |x|` of each cluster.
    pub tol: f64,
    pub max_iter: u32,
    /// Movement cost of dragged parameters relative to the others (weight 1).
    pub drag_weight: f64,
    /// Diagnosis: squared remainder of a unit Jacobian row below which it is dependent on the
    /// earlier rows (1e-10 ≙ a row within ~1e-5 rad of the span of the others).
    pub pivot_tol: f64,
    /// Reject solver steps that flip an arc (change its CCW sweep by more than π); if the guarded
    /// solve fails the cluster is re-solved without the guard.
    pub keep_arc_orientation: bool,
}

impl Default for SolveOptions {
    fn default() -> Self {
        SolveOptions {
            tol: 1e-10,
            max_iter: 100,
            drag_weight: 1e6,
            pivot_tol: 1e-10,
            keep_arc_orientation: true,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SolveReport {
    /// Every enforced equation is satisfied.
    pub converged: bool,
    /// Max LM iterations over the solved clusters.
    pub iterations: u32,
    pub max_residual: f64,
    /// Number of clusters solved.
    pub clusters: usize,
    pub failed_clusters: usize,
    /// Enforced constraints whose residual is above tolerance after solving.
    pub unsatisfied: Vec<SkConstraintId>,
    /// Enforced constraints ignored because they reference missing or wrongly typed entities.
    pub invalid: Vec<SkConstraintId>,
}

/// A linear dependency between constraint equations at the current state.
#[derive(Clone, Debug, PartialEq)]
pub struct DependencyGroup {
    /// The most recently added constraint of the dependency (best "remove me" candidate).
    pub constraint: SkConstraintId,
    /// Every constraint taking part (sorted, includes `constraint`).
    pub members: Vec<SkConstraintId>,
    /// The dependency is inconsistent with the residuals: the members conflict.
    pub conflicting: bool,
    pub inconsistency: f64,
}

/// A solver parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Param {
    X(SkEntityId),
    Y(SkEntityId),
    /// Radius of a circle.
    Radius(SkEntityId),
}

/// Constraint status of an entity, for coloring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EntityStatus {
    /// Some of its parameters can still move.
    UnderConstrained,
    /// Every parameter is determined by the constraints (or fixed).
    FullyConstrained,
    /// Referenced by a conflicting constraint.
    OverConstrained,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnosis {
    /// Unlocked parameters (points with `Fix` are constants).
    pub n_params: usize,
    pub n_equations: usize,
    pub rank: usize,
    /// Remaining degrees of freedom = `n_params - rank`.
    pub dof: usize,
    pub groups: Vec<DependencyGroup>,
    /// Dependent constraints of consistent groups (removable without changing the solution locally).
    pub redundant: Vec<SkConstraintId>,
    /// Union of the members of all conflicting groups.
    pub conflicting: Vec<SkConstraintId>,
    /// The dependent constraint of each conflicting group.
    pub conflicting_dependents: Vec<SkConstraintId>,
    /// Parameters not determined by the constraints (includes parameters in no constraint).
    pub free_params: Vec<Param>,
    /// Status of every entity.
    pub entities: BTreeMap<SkEntityId, EntityStatus>,
    /// Enforced constraints that reference missing or wrongly typed entities.
    pub invalid: Vec<SkConstraintId>,
}

impl Diagnosis {
    /// Members of each conflicting group.
    pub fn conflict_groups(&self) -> Vec<Vec<SkConstraintId>> {
        self.groups
            .iter()
            .filter(|g| g.conflicting)
            .map(|g| g.members.clone())
            .collect()
    }

    /// Members of each redundant (consistent) group.
    pub fn redundant_groups(&self) -> Vec<Vec<SkConstraintId>> {
        self.groups
            .iter()
            .filter(|g| !g.conflicting)
            .map(|g| g.members.clone())
            .collect()
    }

    pub fn status(&self, id: SkEntityId) -> Option<EntityStatus> {
        self.entities.get(&id).copied()
    }

    pub fn is_fully_constrained(&self, id: SkEntityId) -> bool {
        self.status(id) == Some(EntityStatus::FullyConstrained)
    }

    /// A point with at least one free coordinate.
    pub fn is_point_free(&self, id: SkEntityId) -> bool {
        self.free_params
            .iter()
            .any(|p| matches!(*p, Param::X(e) | Param::Y(e) if e == id))
    }

    /// Fully constrained, no conflicts, no redundancy.
    pub fn is_well_constrained(&self) -> bool {
        self.dof == 0 && self.groups.is_empty() && self.invalid.is_empty()
    }
}

/// Solve every cluster (or only those containing a parameter of `only`).
fn run(sys: &mut System, opt: &SolveOptions, weights: &[(u32, f64)], only: &[u32]) -> SolveReport {
    let (cls, constant) = clusters(sys);
    let mut w = Work::new(sys.x.len());
    let mut rep = SolveReport {
        converged: true,
        invalid: sys.invalid.clone(),
        ..Default::default()
    };
    let mut weight = vec![1.0; sys.x.len()];
    for &(p, v) in weights {
        if let Some(slot) = weight.get_mut(p as usize) {
            *slot = v.max(1e-12);
        }
    }
    let mut winv = Vec::new();
    let mut arcs = Vec::new();
    let mut start = Vec::new();
    let mut guarded = Vec::new();
    for cl in &cls {
        if !only.is_empty() && !only.iter().any(|p| cl.params.binary_search(p).is_ok()) {
            continue;
        }
        winv.clear();
        winv.extend(cl.params.iter().map(|&p| 1.0 / weight[p as usize]));
        arcs.clear();
        if opt.keep_arc_orientation {
            arcs.extend(
                sys.arcs
                    .iter()
                    .filter(|(_, pts)| {
                        pts.iter().any(|p| {
                            cl.params.binary_search(&p.x).is_ok()
                                || cl.params.binary_search(&p.y).is_ok()
                        })
                    })
                    .map(|(_, pts)| *pts),
            );
        }
        start.clear();
        start.extend(cl.params.iter().map(|&p| sys.x[p as usize]));
        let mut res = solve_cluster(&sys.eqs, &mut sys.x, cl, &winv, &arcs, opt, &mut w);
        if !res.converged && !arcs.is_empty() {
            // The configuration may only be reachable through an arc flip: retry unguarded.
            guarded.clear();
            guarded.extend(cl.params.iter().map(|&p| sys.x[p as usize]));
            for (&p, &v) in cl.params.iter().zip(&start) {
                sys.x[p as usize] = v;
            }
            let free = solve_cluster(&sys.eqs, &mut sys.x, cl, &winv, &[], opt, &mut w);
            if free.converged || free.norm2 < res.norm2 {
                res = free;
            } else {
                for (&p, &v) in cl.params.iter().zip(&guarded) {
                    sys.x[p as usize] = v;
                }
            }
        }
        rep.clusters += 1;
        rep.iterations = rep.iterations.max(res.iterations);
        rep.max_residual = rep.max_residual.max(res.max_residual);
        if !res.converged {
            rep.converged = false;
            rep.failed_clusters += 1;
            let tol = opt.tol * scale_of(&sys.x, &cl.params);
            for &k in &cl.eqs {
                let e = &sys.eqs[k as usize];
                if !(e.eval(&sys.x, &mut w.grad).abs() <= tol)
                    && let Some(c) = e.owner.constraint()
                {
                    rep.unsatisfied.push(c);
                }
            }
        }
    }
    if only.is_empty() {
        for &k in &constant {
            let e = &sys.eqs[k as usize];
            let f = e.eval(&sys.x, &mut w.grad);
            let params: Vec<u32> = w.grad.iter().map(|g| g.0).collect();
            let tol = opt.tol * scale_of(&sys.x, &params);
            rep.max_residual = rep.max_residual.max(f.abs());
            if !(f.abs() <= tol) {
                rep.converged = false;
                if let Some(c) = e.owner.constraint() {
                    rep.unsatisfied.push(c);
                }
            }
        }
    }
    rep.unsatisfied.sort_unstable();
    rep.unsatisfied.dedup();
    rep
}

impl Sketch {
    /// Solve all enforced constraints from the current state and write the result back.
    pub fn solve(&mut self) -> SolveReport {
        self.solve_with(&SolveOptions::default())
    }

    pub fn solve_with(&mut self, opt: &SolveOptions) -> SolveReport {
        let mut sys = System::build(self);
        let rep = run(&mut sys, opt, &[], &[]);
        sys.write_back(self);
        rep
    }

    /// Drag a point towards `target` (SolveSpace style): the point is put at the target, its
    /// coordinates get weight [`SolveOptions::drag_weight`] so they move least, and only the
    /// clusters containing it are re-solved. A fixed point does not move. Unknown ids return a
    /// default (not converged) report.
    pub fn drag(&mut self, point: SkEntityId, target: DVec2) -> SolveReport {
        self.drag_with(point, target, &SolveOptions::default())
    }

    pub fn drag_with(
        &mut self,
        point: SkEntityId,
        target: DVec2,
        opt: &SolveOptions,
    ) -> SolveReport {
        if !target.is_finite() {
            return SolveReport::default();
        }
        let Some(p) = self.point(point) else {
            return SolveReport::default();
        };
        self.move_points(&[point], target - p, &[], opt)
    }

    /// Move a whole curve (line: both endpoints; circle: center, radius kept; arc: all three points)
    /// or a point by `delta`, then re-solve its clusters with the moved parameters weighted.
    pub fn drag_curve(&mut self, entity: SkEntityId, delta: DVec2) -> SolveReport {
        self.drag_curve_with(entity, delta, &SolveOptions::default())
    }

    pub fn drag_curve_with(
        &mut self,
        entity: SkEntityId,
        delta: DVec2,
        opt: &SolveOptions,
    ) -> SolveReport {
        if !delta.is_finite() {
            return SolveReport::default();
        }
        let Some(e) = self.entities.get(&entity) else {
            return SolveReport::default();
        };
        let mut pts = e.geom.defining_points();
        let mut radius = Vec::new();
        match e.geom {
            SkGeom::Point { .. } => pts.push(entity),
            SkGeom::Circle { .. } => radius.push(entity),
            _ => {}
        }
        self.move_points(&pts, delta, &radius, opt)
    }

    fn move_points(
        &mut self,
        pts: &[SkEntityId],
        delta: DVec2,
        weighted_radii: &[SkEntityId],
        opt: &SolveOptions,
    ) -> SolveReport {
        // Orientation choices (tangent sides, ...) come from the state before the move.
        let mut sys = System::build(self);
        let mut weights = Vec::new();
        for id in pts {
            let Some(&pt) = sys.points.get(id) else {
                continue;
            };
            if sys.locked[pt.x as usize] {
                continue;
            }
            if weights.iter().any(|&(p, _)| p == pt.x) {
                continue;
            }
            sys.x[pt.x as usize] += delta.x;
            sys.x[pt.y as usize] += delta.y;
            weights.push((pt.x, opt.drag_weight));
            weights.push((pt.y, opt.drag_weight));
        }
        if weights.is_empty() {
            // Nothing movable (fixed or unknown): report the current state of the sketch.
            return SolveReport {
                converged: true,
                invalid: sys.invalid.clone(),
                ..Default::default()
            };
        }
        for id in weighted_radii {
            if let Some(&r) = sys.radii.get(id) {
                weights.push((r, opt.drag_weight));
            }
        }
        let only: Vec<u32> = weights.iter().map(|w| w.0).collect();
        let rep = run(&mut sys, opt, &weights, &only);
        sys.write_back(self);
        rep
    }

    /// Degrees of freedom, per-entity status, redundant and conflicting constraints at the current
    /// state (call after [`Sketch::solve`]).
    pub fn diagnose(&self) -> Diagnosis {
        self.diagnose_with(&SolveOptions::default())
    }

    pub fn diagnose_with(&self, opt: &SolveOptions) -> Diagnosis {
        let sys = System::build(self);
        diagnose::diagnose(self, &sys, opt)
    }

    /// Current value of a dimensional constraint's quantity (for reference dimensions and for
    /// showing driving ones): distances are unsigned, `Angle` is the signed CCW angle from line 1
    /// to line 2 in `(-π, π]`. `NaN` for unknown ids, non-dimensional constraints or broken
    /// references.
    pub fn measure(&self, id: SkConstraintId) -> f64 {
        self.constraints
            .get(&id)
            .and_then(|c| self.measure_kind(&c.kind))
            .unwrap_or(f64::NAN)
    }

    /// Like [`Sketch::measure`] for a constraint that is not (yet) in the sketch.
    pub fn measure_kind(&self, kind: &ConstraintKind) -> Option<f64> {
        use ConstraintKind as K;
        let pt = |id| self.point(id);
        let line = |id| {
            let (a, b) = self.line_ends(id)?;
            Some((self.point(a)?, self.point(b)?))
        };
        Some(match *kind {
            K::Distance { p1, p2, .. } => pt(p1)?.distance(pt(p2)?),
            K::HorizontalDistance { p1, p2, .. } => (pt(p2)?.x - pt(p1)?.x).abs(),
            K::VerticalDistance { p1, p2, .. } => (pt(p2)?.y - pt(p1)?.y).abs(),
            K::PointLineDistance { p, line: l, .. } => {
                let (a, b) = line(l)?;
                let u = b - a;
                let len = u.length();
                if !(len > 0.0) {
                    return Some(pt(p)?.distance(a));
                }
                (wcad_math::cross2(u, pt(p)? - a) / len).abs()
            }
            K::Length { line: l, .. } => {
                let (a, b) = line(l)?;
                a.distance(b)
            }
            K::Angle { line1, line2, .. } => {
                let ((a, b), (c, d)) = (line(line1)?, line(line2)?);
                let (u, v) = (b - a, d - c);
                wcad_math::cross2(u, v).atan2(u.dot(v))
            }
            K::Radius { round, .. } => self.radius_of(round)?,
            K::Diameter { round, .. } => 2.0 * self.radius_of(round)?,
            _ => return None,
        })
    }

    /// Max |residual| of a constraint's equations at the current state (`None` if it is unknown,
    /// invalid, a `Fix`, or not enforced).
    pub fn residual(&self, id: SkConstraintId) -> Option<f64> {
        let sys = System::build(self);
        let mut g = Vec::new();
        let mut any = false;
        let mut worst = 0.0f64;
        for e in sys.eqs.iter().filter(|e| e.owner.constraint() == Some(id)) {
            any = true;
            worst = worst.max(e.eval(&sys.x, &mut g).abs());
        }
        any.then_some(worst)
    }
}
