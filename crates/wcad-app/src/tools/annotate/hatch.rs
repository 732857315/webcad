use wcad_doc::{EntityId, EntityKind, Hatch, HatchLoop, HatchPatternRef};
use wcad_geom2d::{Curve, Curve2, bulge, hatch, regions};
use wcad_math::BBox2;

use super::{EPS, annotate, commit, error, finite_input, ghost, kw, positive, separated};
use crate::i18n::{Lang, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput, dedup_ids};

const MAX_SEGMENTS: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BoundaryError {
    Invalid,
    Limit,
}

fn valid_curve(c: &Curve2) -> bool {
    match c {
        Curve2::Line(l) => separated(l.a, l.b),
        Curve2::Circle(c) => c.c.is_finite() && positive(c.r),
        Curve2::Arc(a) => super::dimension::valid_arc(a),
        Curve2::Ellipse(e) => {
            e.c.is_finite()
                && e.major.is_finite()
                && positive(e.major.length())
                && e.ratio.is_finite()
                && e.ratio > 0.0
                && e.ratio <= 1.0
                && positive(e.minor().length())
                && e.start.is_finite()
                && e.end.is_finite()
                && e.is_full()
        }
        Curve2::Polyline(pl) => {
            if pl.segment_count() < 1
                || pl
                    .verts
                    .iter()
                    .any(|v| !v.p.is_finite() || !v.bulge.is_finite())
            {
                return false;
            }
            (0..pl.segment_count()).all(|i| {
                let a = pl.verts[i];
                let b = pl.verts[(i + 1) % pl.verts.len()];
                if !separated(a.p, b.p) {
                    return false;
                }
                if a.bulge.abs() < bulge::BULGE_EPS {
                    return true;
                }
                // bulge_to_arc otherwise falls back to a straight segment. Never accept that
                // fallback for a malformed curved boundary.
                bulge::bulge_to_arc(a.p, b.p, a.bulge).is_some_and(|arc| {
                    arc.c.is_finite()
                        && positive(arc.r)
                        && arc.sweep.is_finite()
                        && arc.sweep.abs() > EPS
                        && arc.sweep.abs() < std::f64::consts::TAU - EPS
                })
            })
        }
        Curve2::Spline(_) => false,
    }
}

/// Preserve boundary geometry. The hatch/tessellation APIs imply a final closing edge, so they
/// must never receive a merely *almost* closed selection or a partial region with dropped edges.
pub(super) fn boundary_loops(curves: &[Curve2]) -> Result<Vec<HatchLoop>, BoundaryError> {
    if curves.is_empty() {
        return Err(BoundaryError::Invalid);
    }
    let mut budget = 0usize;
    for c in curves {
        budget = budget.saturating_add(match c {
            Curve2::Polyline(p) => p.segment_count(),
            _ => 1,
        });
        if budget > MAX_SEGMENTS {
            return Err(BoundaryError::Limit);
        }
        if !valid_curve(c) {
            return Err(BoundaryError::Invalid);
        }
    }
    let bb = curves.iter().fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()));
    let ext = bb.size().length();
    if !positive(ext)
        || ext >= 1e30
        || !bb.min.is_finite()
        || !bb.max.is_finite()
        || !bb.center().is_finite()
    {
        return Err(BoundaryError::Invalid);
    }
    // A numerical seam tolerance, independent of zoom/pickbox. Never heal visible gaps.
    let tol = (ext * 1e-12).clamp(1e-10, 1e-7);
    let mut loops = Vec::new();
    let mut open = Vec::new();
    for c in curves {
        if c.is_closed() {
            loops.push(HatchLoop {
                curves: vec![c.clone()],
            });
        } else {
            open.push(c.clone());
        }
    }
    while let Some(first) = open.pop() {
        let start = first.start();
        let mut chain = vec![first];
        loop {
            let end = chain.last().ok_or(BoundaryError::Invalid)?.end();
            if end.distance(start) <= tol {
                break;
            }
            let mut next = None;
            for (i, c) in open.iter().enumerate() {
                for (q, reverse) in [(c.start(), false), (c.end(), true)] {
                    if end.distance(q) <= tol {
                        if next.is_some() {
                            return Err(BoundaryError::Invalid);
                        }
                        next = Some((i, reverse));
                    }
                }
            }
            let Some((i, reverse)) = next else {
                return Err(BoundaryError::Invalid);
            };
            let c = open.swap_remove(i);
            chain.push(if reverse { c.reversed() } else { c });
        }
        loops.push(HatchLoop { curves: chain });
    }
    for (i, l) in loops.iter().enumerate() {
        if loops[..i].iter().any(|other| other.curves == l.curves) {
            return Err(BoundaryError::Invalid);
        }
        let area = regions::signed_area(&l.curves).abs();
        let length: f64 = l.curves.iter().map(Curve::length).sum();
        if !area.is_finite() || area <= EPS * EPS || !positive(length) {
            return Err(BoundaryError::Invalid);
        }
        // A simple loop produces exactly one face, with no discarded or doubled-back edges.
        // Do not use all find_regions faces as loops: that would fill holes twice.
        let faces = regions::find_regions(&l.curves, tol);
        if faces.len() != 1 {
            return Err(BoundaryError::Invalid);
        }
        let face = &faces[0];
        let face_length: f64 = face.outer.curves.iter().map(Curve::length).sum();
        if !face.holes.is_empty()
            || (face.area() - area).abs() > area * 1e-7
            || (face_length - length).abs() > length * 1e-7 + tol
        {
            return Err(BoundaryError::Invalid);
        }
    }
    Ok(loops)
}

#[derive(Clone, Copy, Default)]
enum Step {
    #[default]
    Select,
    Settings,
    Scale,
    Angle,
}

pub(super) struct HatchTool {
    step: Step,
    ids: Vec<EntityId>,
    loops: Vec<HatchLoop>,
    pattern: &'static str,
    scale: f64,
    angle: f64,
}

impl Default for HatchTool {
    fn default() -> Self {
        Self {
            step: Step::Select,
            ids: vec![],
            loops: vec![],
            pattern: "SOLID",
            scale: 1.0,
            angle: 0.0,
        }
    }
}

impl HatchTool {
    fn selected_loops(cx: &ToolCx<'_>, ids: &[EntityId]) -> Result<Vec<HatchLoop>, &'static str> {
        let s = annotate(cx.lang());
        if ids.is_empty() {
            return Err(s.boundary_empty);
        }
        if ids.len() > MAX_SEGMENTS {
            return Err(s.boundary_limit);
        }
        let mut curves = Vec::new();
        for id in ids {
            let e = cx
                .entity(*id)
                .filter(|e| cx.drawing().is_layer_editable(e.layer))
                .ok_or(s.boundary_locked)?;
            curves.push(e.kind.as_curve().ok_or(s.boundary_invalid)?);
        }
        boundary_loops(&curves).map_err(|e| match e {
            BoundaryError::Invalid => s.boundary_invalid,
            BoundaryError::Limit => s.boundary_limit,
        })
    }

    fn pattern_valid(&self, loops: &[HatchLoop]) -> bool {
        if !positive(self.scale) || !self.angle.is_finite() {
            return false;
        }
        if self.pattern == "SOLID" {
            return true;
        }
        let Some(pattern) = hatch::builtin(self.pattern) else {
            return false;
        };
        let refs: Vec<&[Curve2]> = loops.iter().map(|l| l.curves.as_slice()).collect();
        let bb = refs
            .iter()
            .flat_map(|l| l.iter())
            .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox()));
        let extent = bb.min.abs().max(bb.max.abs()).max_element();
        !hatch::hatch_too_dense(&refs, &pattern, self.scale)
            && pattern.lines.iter().all(|l| {
                let spacing = (l.delta.y * self.scale).abs();
                spacing.is_finite() && spacing > 0.0 && extent / spacing < 1e15
            })
    }

    fn entity(&self) -> EntityKind {
        EntityKind::Hatch(Hatch {
            loops: self.loops.clone(),
            pattern: HatchPatternRef {
                name: self.pattern.into(),
                angle: self.angle,
                scale: self.scale,
            },
        })
    }
}

impl Tool for HatchTool {
    fn name(&self) -> &'static str {
        "HATCH"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = annotate(lang);
        match self.step {
            Step::Select => s.select_boundary.into(),
            Step::Settings => fmt(
                s.hatch_confirm,
                &[&self.pattern, &self.scale, &self.angle.to_degrees()],
            ),
            Step::Scale => fmt(s.scale_value, &[&self.scale]),
            Step::Angle => fmt(s.angle_value, &[&self.angle.to_degrees()]),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Select => Accept::SELECTION,
            Step::Settings => Accept::NONE,
            Step::Scale | Step::Angle => Accept::VALUE,
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = annotate(lang);
        if matches!(self.step, Step::Scale | Step::Angle) {
            return vec![];
        }
        let mut out = vec![kw("SOLID", "S", s.solid), kw("ANSI31", "A", s.ansi31)];
        if matches!(self.step, Step::Settings) {
            out.extend([
                kw("Scale", "SC", s.scale),
                kw("Angle", "AN", s.angle),
                kw("Reselect", "R", s.reselect),
            ]);
        }
        out
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = annotate(cx.lang());
        if !finite_input(&input) {
            return error(cx, s.invalid);
        }
        if input == ToolInput::Escape {
            cx.selection_mut().clear();
            return ToolFlow::Cancel;
        }
        match (self.step, input) {
            (Step::Select, ToolInput::Selection(ids)) => {
                let ids = dedup_ids(ids);
                match Self::selected_loops(cx, &ids) {
                    Ok(loops) => {
                        self.loops = loops;
                        self.ids = ids;
                        self.step = Step::Settings;
                    }
                    Err(message) => return error(cx, message),
                }
            }
            (Step::Select | Step::Settings, ToolInput::Keyword(k @ ("SOLID" | "ANSI31"))) => {
                self.pattern = k
            }
            (Step::Settings, ToolInput::Keyword("Scale")) => self.step = Step::Scale,
            (Step::Settings, ToolInput::Keyword("Angle")) => self.step = Step::Angle,
            (Step::Settings, ToolInput::Keyword("Reselect")) => {
                self.step = Step::Select;
                self.ids.clear();
                self.loops.clear();
                cx.selection_mut().clear();
            }
            (Step::Scale, ToolInput::Value(v)) => {
                if !positive(v) {
                    return error(cx, s.positive);
                }
                self.scale = v;
                self.step = Step::Settings;
            }
            (Step::Angle, ToolInput::Value(v)) => {
                self.angle = v.rem_euclid(360.0).to_radians();
                self.step = Step::Settings;
            }
            (Step::Scale | Step::Angle, ToolInput::Enter) => self.step = Step::Settings,
            (Step::Settings, ToolInput::Enter) => {
                // Layers/entities can change while a command is active. Revalidate rather than
                // using a stale cached boundary or silently omitting a now-locked hole.
                self.loops = match Self::selected_loops(cx, &self.ids) {
                    Ok(loops) => loops,
                    Err(message) => {
                        self.step = Step::Select;
                        self.ids.clear();
                        self.loops.clear();
                        return error(cx, message);
                    }
                };
                if !self.pattern_valid(&self.loops) {
                    return error(cx, s.hatch_dense);
                }
                let flow = commit(cx, self.name(), self.entity());
                if flow == ToolFlow::Done {
                    cx.selection_mut().clear();
                }
                return flow;
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if self.loops.is_empty() {
            return;
        }
        if self.pattern_valid(&self.loops) {
            ghost(cx, out, self.entity());
        } else {
            out.curves
                .extend(self.loops.iter().flat_map(|l| l.curves.clone()));
        }
    }
}
