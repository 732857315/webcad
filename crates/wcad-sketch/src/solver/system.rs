//! Translation of the persisted [`Sketch`] into the solver's flat parameter vector and scalar
//! equations, and writing solved values back.
//!
//! Parameters: two per point, one per circle radius (arc radii are `|c - s|`). Points with an
//! enforced `Fix` are *locked*: their parameters are constants, so a fixed point never drifts and
//! the stored position is the anchor. Every other enforced constraint expands to one or more
//! equations `sum coef * term = 0`. The implicit arc condition `|c - s| = |c - e|` is emitted just
//! before the first constraint touching the arc, which keeps the equation order local (narrow
//! envelope) and makes sure diagnosis blames user constraints, never the arc itself.

use std::collections::{BTreeMap, BTreeSet};

use wcad_math::DVec2;

use super::terms::{Pt, Term};
use crate::model::{ArcEnd, ConstraintKind, SkConstraintId, SkEntityId, SkGeom, Sketch};

/// Who produced an equation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    Constraint(SkConstraintId),
    /// Implicit equal-radius condition of an arc.
    Arc(SkEntityId),
}

impl Owner {
    pub(crate) fn constraint(self) -> Option<SkConstraintId> {
        match self {
            Owner::Constraint(c) => Some(c),
            Owner::Arc(_) => None,
        }
    }
}

/// A scalar equation `sum coef * term = 0`.
#[derive(Clone, Debug)]
pub(crate) struct Equation {
    pub(crate) terms: Vec<(f64, Term)>,
    pub(crate) owner: Owner,
}

impl Equation {
    /// Value and merged gradient (sorted by parameter, no duplicates; includes locked parameters).
    pub(crate) fn eval(&self, x: &[f64], grad: &mut Vec<(u32, f64)>) -> f64 {
        grad.clear();
        let mut f = 0.0;
        for &(c, ref t) in &self.terms {
            f += c * t.eval(x, c, grad);
        }
        merge_grad(grad);
        f
    }
}

pub(crate) fn merge_grad(g: &mut Vec<(u32, f64)>) {
    if g.len() < 2 {
        return;
    }
    g.sort_unstable_by_key(|e| e.0);
    let mut w = 0;
    for r in 1..g.len() {
        if g[r].0 == g[w].0 {
            g[w].1 += g[r].1;
        } else {
            w += 1;
            g[w] = g[r];
        }
    }
    g.truncate(w + 1);
}

/// Which geometric quantity a parameter is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamOf {
    X(SkEntityId),
    Y(SkEntityId),
    Radius(SkEntityId),
}

#[derive(Clone, Debug, Default)]
pub(crate) struct System {
    pub(crate) x: Vec<f64>,
    pub(crate) param_of: Vec<ParamOf>,
    pub(crate) locked: Vec<bool>,
    /// The `Fix` constraint that locks a parameter.
    pub(crate) fix_of: Vec<Option<SkConstraintId>>,
    pub(crate) eqs: Vec<Equation>,
    pub(crate) points: BTreeMap<SkEntityId, Pt>,
    pub(crate) radii: BTreeMap<SkEntityId, u32>,
    pub(crate) arcs: Vec<(SkEntityId, [Pt; 3])>,
    /// Enforced constraints that reference missing or wrongly typed entities.
    pub(crate) invalid: Vec<SkConstraintId>,
    /// `(duplicate, original)` pairs of `Fix` constraints on the same point.
    pub(crate) duplicate_fixes: Vec<(SkConstraintId, SkConstraintId)>,
}

fn sgn(v: f64) -> f64 {
    if v < 0.0 { -1.0 } else { 1.0 }
}

impl System {
    pub(crate) fn build(sk: &Sketch) -> System {
        let mut sys = System::default();
        for (&id, e) in &sk.entities {
            if let SkGeom::Point { p } = e.geom {
                let x = sys.param(p.x, ParamOf::X(id));
                let y = sys.param(p.y, ParamOf::Y(id));
                sys.points.insert(id, Pt { x, y });
            }
        }
        for (&id, e) in &sk.entities {
            match e.geom {
                SkGeom::Circle { c, r } if sys.points.contains_key(&c) => {
                    let p = sys.param(r, ParamOf::Radius(id));
                    sys.radii.insert(id, p);
                }
                SkGeom::Arc { c, s, e } => {
                    if let (Some(&c), Some(&s), Some(&e)) =
                        (sys.points.get(&c), sys.points.get(&s), sys.points.get(&e))
                    {
                        sys.arcs.push((id, [c, s, e]));
                    }
                }
                _ => {}
            }
        }
        // Fix constraints lock parameters.
        let mut fix_by_point: BTreeMap<SkEntityId, SkConstraintId> = BTreeMap::new();
        for (&cid, c) in &sk.constraints {
            if !c.is_enforced() {
                continue;
            }
            if let ConstraintKind::Fix { p } = c.kind {
                let Some(&pt) = sys.points.get(&p) else {
                    sys.invalid.push(cid);
                    continue;
                };
                if let Some(&orig) = fix_by_point.get(&p) {
                    sys.duplicate_fixes.push((cid, orig));
                    continue;
                }
                fix_by_point.insert(p, cid);
                for q in [pt.x, pt.y] {
                    sys.locked[q as usize] = true;
                    sys.fix_of[q as usize] = Some(cid);
                }
            }
        }
        // Arc lookup by point, for emitting the implicit equations in a local order.
        let mut arcs_of_point: BTreeMap<SkEntityId, Vec<usize>> = BTreeMap::new();
        for (k, (id, _)) in sys.arcs.iter().enumerate() {
            if let Some(SkGeom::Arc { c, s, e }) = sk.entities.get(id).map(|e| &e.geom) {
                for p in [c, s, e] {
                    arcs_of_point.entry(*p).or_default().push(k);
                }
            }
        }
        let mut arc_done = vec![false; sys.arcs.len()];
        let mut touched: BTreeSet<SkEntityId> = BTreeSet::new();
        for (&cid, c) in &sk.constraints {
            if !c.is_enforced() || matches!(c.kind, ConstraintKind::Fix { .. }) {
                continue;
            }
            let Some(rows) = sys.expand(sk, &c.kind) else {
                sys.invalid.push(cid);
                continue;
            };
            touched.clear();
            for e in c.kind.entities() {
                touched.insert(e);
                if let Some(ent) = sk.entities.get(&e) {
                    touched.extend(ent.geom.defining_points());
                }
            }
            for p in &touched {
                for &k in arcs_of_point.get(p).map_or(&[][..], |v| v.as_slice()) {
                    if !arc_done[k] {
                        arc_done[k] = true;
                        sys.push_arc_eq(k);
                    }
                }
            }
            for terms in rows {
                sys.eqs.push(Equation {
                    terms,
                    owner: Owner::Constraint(cid),
                });
            }
        }
        for k in 0..sys.arcs.len() {
            if !arc_done[k] {
                sys.push_arc_eq(k);
            }
        }
        sys.invalid.sort_unstable();
        sys
    }

    fn param(&mut self, v: f64, of: ParamOf) -> u32 {
        self.x.push(v);
        self.param_of.push(of);
        self.locked.push(false);
        self.fix_of.push(None);
        (self.x.len() - 1) as u32
    }

    fn push_arc_eq(&mut self, k: usize) {
        let (id, [c, s, e]) = self.arcs[k];
        self.eqs.push(Equation {
            terms: vec![(1.0, Term::Len(c, s)), (-1.0, Term::Len(c, e))],
            owner: Owner::Arc(id),
        });
    }

    pub(crate) fn num_free_params(&self) -> usize {
        self.locked.iter().filter(|l| !**l).count()
    }

    pub(crate) fn pt_value(&self, p: Pt) -> DVec2 {
        DVec2::new(self.x[p.x as usize], self.x[p.y as usize])
    }

    fn val(&self, t: &Term) -> f64 {
        let mut g = Vec::new();
        t.eval(&self.x, 1.0, &mut g)
    }

    fn line(&self, sk: &Sketch, id: SkEntityId) -> Option<(Pt, Pt)> {
        let (a, b) = sk.line_ends(id)?;
        Some((*self.points.get(&a)?, *self.points.get(&b)?))
    }

    fn pt(&self, id: SkEntityId) -> Option<Pt> {
        self.points.get(&id).copied()
    }

    /// Center of a circle/arc.
    fn center(&self, sk: &Sketch, id: SkEntityId) -> Option<Pt> {
        match sk.entities.get(&id)?.geom {
            SkGeom::Circle { c, .. } if self.radii.contains_key(&id) => self.pt(c),
            SkGeom::Arc { c, .. } if self.arcs.iter().any(|a| a.0 == id) => self.pt(c),
            _ => None,
        }
    }

    /// Radius of a circle/arc as a term with coefficient.
    fn rterm(&self, sk: &Sketch, id: SkEntityId, coef: f64) -> Option<(f64, Term)> {
        match sk.entities.get(&id)?.geom {
            SkGeom::Circle { .. } => Some((coef, Term::Var(*self.radii.get(&id)?))),
            SkGeom::Arc { c, s, .. } => Some((coef, Term::Len(self.pt(c)?, self.pt(s)?))),
            _ => None,
        }
    }

    fn radius_value(&self, sk: &Sketch, id: SkEntityId) -> Option<f64> {
        let (c, t) = self.rterm(sk, id, 1.0)?;
        Some(c * self.val(&t))
    }

    fn arc_end(&self, sk: &Sketch, arc: SkEntityId, end: ArcEnd) -> Option<(Pt, Pt)> {
        let [c, s, e] = sk.arc_points(arc)?;
        let p = if end == ArcEnd::Start { s } else { e };
        Some((self.pt(c)?, self.pt(p)?))
    }

    /// Equations of one constraint (`None` when an argument is missing or has the wrong type).
    /// Orientation choices use the current parameter values.
    fn expand(&self, sk: &Sketch, kind: &ConstraintKind) -> Option<Vec<Vec<(f64, Term)>>> {
        use ConstraintKind as K;
        use Term::*;
        let two = |p: Pt, q: Pt| {
            vec![
                vec![(1.0, Var(p.x)), (-1.0, Var(q.x))],
                vec![(1.0, Var(p.y)), (-1.0, Var(q.y))],
            ]
        };
        let dist_eq = |t: Term, v: f64| vec![vec![(1.0, t), (1.0, Const(-v))]];
        Some(match *kind {
            K::Coincident { p1, p2 } => two(self.pt(p1)?, self.pt(p2)?),
            K::PointOnLine { p, line } => {
                let (a, b) = self.line(sk, line)?;
                vec![vec![(
                    1.0,
                    SignedDist {
                        p: self.pt(p)?,
                        a,
                        b,
                    },
                )]]
            }
            K::PointOnCircle { p, round } => {
                vec![vec![
                    (1.0, Len(self.center(sk, round)?, self.pt(p)?)),
                    self.rterm(sk, round, -1.0)?,
                ]]
            }
            K::Horizontal { line } => {
                let (a, b) = self.line(sk, line)?;
                vec![vec![(1.0, Var(a.y)), (-1.0, Var(b.y))]]
            }
            K::Vertical { line } => {
                let (a, b) = self.line(sk, line)?;
                vec![vec![(1.0, Var(a.x)), (-1.0, Var(b.x))]]
            }
            K::HorizontalPoints { p1, p2 } => {
                vec![vec![
                    (1.0, Var(self.pt(p1)?.y)),
                    (-1.0, Var(self.pt(p2)?.y)),
                ]]
            }
            K::VerticalPoints { p1, p2 } => {
                vec![vec![
                    (1.0, Var(self.pt(p1)?.x)),
                    (-1.0, Var(self.pt(p2)?.x)),
                ]]
            }
            K::Parallel { line1, line2 } => {
                let ((a, b), (c, d)) = (self.line(sk, line1)?, self.line(sk, line2)?);
                vec![vec![(1.0, Sin { a, b, c, d })]]
            }
            K::Perpendicular { line1, line2 } => {
                let ((a, b), (c, d)) = (self.line(sk, line1)?, self.line(sk, line2)?);
                vec![vec![(1.0, Cos { a, b, c, d })]]
            }
            K::Angle {
                line1,
                line2,
                ref value,
            } => {
                let ((a, b), (c, d)) = (self.line(sk, line1)?, self.line(sk, line2)?);
                let th = value.value;
                // sin(phi - th) = sin(phi) cos(th) - cos(phi) sin(th)
                vec![vec![
                    (th.cos(), Sin { a, b, c, d }),
                    (-th.sin(), Cos { a, b, c, d }),
                ]]
            }
            K::TangentLineCircle { line, round } => {
                let (a, b) = self.line(sk, line)?;
                let t = SignedDist {
                    p: self.center(sk, round)?,
                    a,
                    b,
                };
                let s = sgn(self.val(&t));
                vec![vec![(1.0, t), self.rterm(sk, round, -s)?]]
            }
            K::TangentCircles { round1, round2 } => {
                let d = Len(self.center(sk, round1)?, self.center(sk, round2)?);
                let (dv, r1, r2) = (
                    self.val(&d),
                    self.radius_value(sk, round1)?,
                    self.radius_value(sk, round2)?,
                );
                if (dv - (r1 + r2)).abs() <= (dv - (r1 - r2).abs()).abs() {
                    vec![vec![
                        (1.0, d),
                        self.rterm(sk, round1, -1.0)?,
                        self.rterm(sk, round2, -1.0)?,
                    ]]
                } else {
                    let s = sgn(r1 - r2);
                    vec![vec![
                        (1.0, d),
                        self.rterm(sk, round1, -s)?,
                        self.rterm(sk, round2, s)?,
                    ]]
                }
            }
            K::TangentArcLine { arc, end, line } => {
                let (c, p) = self.arc_end(sk, arc, end)?;
                let (a, b) = self.line(sk, line)?;
                vec![vec![(
                    1.0,
                    Cos {
                        a: c,
                        b: p,
                        c: a,
                        d: b,
                    },
                )]]
            }
            K::TangentArcArc {
                arc1,
                end1,
                arc2,
                end2,
            } => {
                let (c1, p1) = self.arc_end(sk, arc1, end1)?;
                let (c2, p2) = self.arc_end(sk, arc2, end2)?;
                vec![vec![(
                    1.0,
                    Sin {
                        a: c1,
                        b: p1,
                        c: c2,
                        d: p2,
                    },
                )]]
            }
            K::EqualLength { line1, line2 } => {
                let ((a, b), (c, d)) = (self.line(sk, line1)?, self.line(sk, line2)?);
                vec![vec![(1.0, Len(a, b)), (-1.0, Len(c, d))]]
            }
            K::EqualRadius { round1, round2 } => {
                vec![vec![
                    self.rterm(sk, round1, 1.0)?,
                    self.rterm(sk, round2, -1.0)?,
                ]]
            }
            K::Midpoint { p, line } => {
                let (a, b) = self.line(sk, line)?;
                let m = self.pt(p)?;
                vec![
                    vec![(2.0, Var(m.x)), (-1.0, Var(a.x)), (-1.0, Var(b.x))],
                    vec![(2.0, Var(m.y)), (-1.0, Var(a.y)), (-1.0, Var(b.y))],
                ]
            }
            K::Symmetric { p1, p2, line } => {
                let (a, b) = self.line(sk, line)?;
                let (p, q) = (self.pt(p1)?, self.pt(p2)?);
                vec![
                    // midpoint on the line (signed distance is affine in the point)
                    vec![
                        (1.0, SignedDist { p, a, b }),
                        (1.0, SignedDist { p: q, a, b }),
                    ],
                    // p->q perpendicular to the line
                    vec![(1.0, Proj { p, q, a, b })],
                ]
            }
            // Handled by locking; never expanded.
            K::Fix { .. } => return None,
            K::Distance { p1, p2, ref value } => {
                dist_eq(Len(self.pt(p1)?, self.pt(p2)?), value.value)
            }
            K::HorizontalDistance { p1, p2, ref value } => {
                let (p, q) = (self.pt(p1)?, self.pt(p2)?);
                let s = sgn(self.x[q.x as usize] - self.x[p.x as usize]);
                vec![vec![
                    (1.0, Var(q.x)),
                    (-1.0, Var(p.x)),
                    (1.0, Const(-s * value.value)),
                ]]
            }
            K::VerticalDistance { p1, p2, ref value } => {
                let (p, q) = (self.pt(p1)?, self.pt(p2)?);
                let s = sgn(self.x[q.y as usize] - self.x[p.y as usize]);
                vec![vec![
                    (1.0, Var(q.y)),
                    (-1.0, Var(p.y)),
                    (1.0, Const(-s * value.value)),
                ]]
            }
            K::PointLineDistance { p, line, ref value } => {
                let (a, b) = self.line(sk, line)?;
                let t = SignedDist {
                    p: self.pt(p)?,
                    a,
                    b,
                };
                let s = sgn(self.val(&t));
                dist_eq(t, s * value.value)
            }
            K::Length { line, ref value } => {
                let (a, b) = self.line(sk, line)?;
                dist_eq(Len(a, b), value.value)
            }
            K::Radius { round, ref value } => {
                self.center(sk, round)?;
                vec![vec![
                    self.rterm(sk, round, 1.0)?,
                    (1.0, Const(-value.value)),
                ]]
            }
            K::Diameter { round, ref value } => {
                self.center(sk, round)?;
                vec![vec![
                    self.rterm(sk, round, 2.0)?,
                    (1.0, Const(-value.value)),
                ]]
            }
        })
    }

    /// Copy solved values into the model (non-finite values are never written).
    pub(crate) fn write_back(&self, sk: &mut Sketch) {
        for (id, &pt) in &self.points {
            let p = self.pt_value(pt);
            if p.is_finite()
                && let Some(SkGeom::Point { p: q }) = sk.entities.get_mut(id).map(|e| &mut e.geom)
            {
                *q = p;
            }
        }
        for (id, &r) in &self.radii {
            let v = self.x[r as usize];
            if v.is_finite()
                && let Some(SkGeom::Circle { r: q, .. }) =
                    sk.entities.get_mut(id).map(|e| &mut e.geom)
            {
                *q = v;
            }
        }
    }

    /// Parameters of an entity (points: x, y; circles: center + radius; lines/arcs: their points).
    pub(crate) fn entity_params(&self, sk: &Sketch, id: SkEntityId, out: &mut Vec<u32>) {
        out.clear();
        let Some(e) = sk.entities.get(&id) else {
            return;
        };
        let mut pts = e.geom.defining_points();
        if e.geom.is_point() {
            pts.push(id);
        }
        for p in pts {
            if let Some(pt) = self.pt(p) {
                out.extend([pt.x, pt.y]);
            }
        }
        if let Some(&r) = self.radii.get(&id) {
            out.push(r);
        }
    }
}
