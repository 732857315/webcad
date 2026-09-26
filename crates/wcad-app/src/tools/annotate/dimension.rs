use std::f64::consts::{FRAC_PI_2, PI, TAU};

use wcad_doc::{DimKind, Dimension, EntityKind};
use wcad_geom2d::{Arc2, Curve, Curve2, Line2, bulge::PolySegment};
use wcad_math::{DVec2, ccw_sweep};

use super::{
    EPS, MAX_SIZE, annotate, commit, error, finite_input, ghost, kw, positive, separated,
    valid_point,
};
use crate::i18n::{Lang, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DimMode {
    Linear,
    Aligned,
    Radius,
    Diameter,
    Angular,
    Ordinate,
}

#[derive(Clone, Copy)]
enum AngleSource {
    Arc,
    Rays,
    Lines,
}

#[derive(Clone, Copy)]
enum Definition {
    Pair(DVec2, DVec2),
    Round {
        center: DVec2,
        point: DVec2,
    },
    Angle {
        vertex: DVec2,
        p1: DVec2,
        p2: DVec2,
        source: AngleSource,
    },
    Ordinate(DVec2),
}

#[derive(Clone)]
enum Step {
    First,
    Second(DVec2),
    PickLine,
    PickRound,
    PickAngle,
    PickSecondLine(Line2),
    Vertex,
    FirstRay(DVec2),
    SecondRay(DVec2, DVec2),
    OrdinatePoint,
    Origin,
    Place(Definition),
    Rotation(Definition),
    TextPosition(DimKind),
}

pub(super) struct DimensionTool {
    mode: DimMode,
    step: Step,
    rotation: Option<f64>,
    origin: DVec2,
    x_axis: bool,
    custom_text: bool,
}

pub(super) fn valid_arc(a: &Arc2) -> bool {
    valid_point(a.c)
        && positive(a.r)
        && a.start.is_finite()
        && a.end.is_finite()
        && a.sweep() > EPS
        && a.sweep() < TAU - EPS
        && separated(a.start_point(), a.end_point())
}

fn valid_line(l: &Line2) -> bool {
    separated(l.a, l.b)
}

/// PICK is raw (not snapped). A polyline pick resolves its nearest actual segment, not its
/// endpoints or its bounding box. In particular, a line segment must not become an arc pick.
fn picked_curve(cx: &ToolCx<'_>, p: DVec2) -> Option<Curve2> {
    let e = cx.entity(cx.pick(p)?)?;
    if !cx.drawing().is_layer_editable(e.layer) {
        return None;
    }
    let c = e.kind.as_curve()?;
    match c {
        Curve2::Polyline(pl) => {
            if pl.segment_count() > 1024
                || pl
                    .verts
                    .iter()
                    .any(|v| !valid_point(v.p) || !v.bulge.is_finite())
            {
                return None;
            }
            for i in 0..pl.segment_count() {
                let a = pl.verts[i];
                let b = pl.verts[(i + 1) % pl.verts.len()];
                if a.bulge.abs() >= wcad_geom2d::bulge::BULGE_EPS
                    && wcad_geom2d::bulge::bulge_to_arc(a.p, b.p, a.bulge).is_none()
                {
                    return None;
                }
            }
            let chosen = pl
                .segments()
                .map(|seg| match seg {
                    PolySegment::Line(l) => Curve2::Line(l),
                    PolySegment::Arc(a) => Curve2::Arc(a.to_arc()),
                })
                .min_by(|a, b| {
                    a.closest(p)
                        .1
                        .distance(p)
                        .total_cmp(&b.closest(p).1.distance(p))
                })?;
            let valid = match &chosen {
                Curve2::Line(l) => valid_line(l),
                Curve2::Arc(a) => valid_arc(a),
                _ => false,
            };
            let tolerance = cx.draft().pickbox_px as f64 * cx.units_per_px();
            (valid && chosen.closest(p).1.distance(p) <= tolerance).then_some(chosen)
        }
        _ => Some(c),
    }
}

fn angle_valid(v: DVec2, p1: DVec2, p2: DVec2) -> bool {
    if !separated(v, p1) || !separated(v, p2) {
        return false;
    }
    let a = ccw_sweep((p1 - v).to_angle(), (p2 - v).to_angle());
    a > EPS && a < TAU - EPS
}

fn line_angle(a: Line2, b: Line2) -> Option<Definition> {
    if !valid_line(&a) || !valid_line(&b) {
        return None;
    }
    let u = (a.b - a.a).try_normalize()?;
    let v = (b.b - b.a).try_normalize()?;
    let cross = u.perp_dot(v);
    if cross.abs() <= EPS {
        return None;
    }
    let vertex = a.a + u * ((b.a - a.a).perp_dot(v) / cross);
    let endpoint = |l: Line2| {
        if vertex.distance(l.a) >= vertex.distance(l.b) {
            l.a
        } else {
            l.b
        }
    };
    let (p1, p2) = (endpoint(a), endpoint(b));
    angle_valid(vertex, p1, p2).then_some(Definition::Angle {
        vertex,
        p1,
        p2,
        source: AngleSource::Lines,
    })
}

fn angle_rays(
    vertex: DVec2,
    p1: DVec2,
    p2: DVec2,
    p: DVec2,
    source: AngleSource,
) -> Option<(DVec2, DVec2)> {
    if !angle_valid(vertex, p1, p2) || !separated(vertex, p) {
        return None;
    }
    let inside = |a: DVec2, b: DVec2| {
        ((p - vertex).to_angle() - (a - vertex).to_angle()).rem_euclid(TAU)
            <= ccw_sweep((a - vertex).to_angle(), (b - vertex).to_angle()) + EPS
    };
    match source {
        AngleSource::Arc => Some((p1, p2)),
        AngleSource::Rays => Some(if inside(p1, p2) { (p1, p2) } else { (p2, p1) }),
        AngleSource::Lines => {
            // Two infinite lines have four sectors. The placement point chooses one; picking
            // near the lines' intersection must not arbitrarily reverse a ray.
            for a in [p1, vertex - (p1 - vertex)] {
                for b in [p2, vertex - (p2 - vertex)] {
                    let (a, b) = if ccw_sweep((a - vertex).to_angle(), (b - vertex).to_angle()) < PI
                    {
                        (a, b)
                    } else {
                        (b, a)
                    };
                    if a.is_finite() && b.is_finite() && inside(a, b) {
                        return Some((a, b));
                    }
                }
            }
            None
        }
    }
}

impl DimensionTool {
    pub(super) fn new(mode: DimMode) -> Self {
        let step = match mode {
            DimMode::Linear | DimMode::Aligned => Step::First,
            DimMode::Radius | DimMode::Diameter => Step::PickRound,
            DimMode::Angular => Step::PickAngle,
            DimMode::Ordinate => Step::OrdinatePoint,
        };
        Self {
            mode,
            step,
            rotation: None,
            origin: DVec2::ZERO,
            x_axis: true,
            custom_text: false,
        }
    }

    fn kind_at(&self, def: Definition, p: DVec2) -> Option<DimKind> {
        if !valid_point(p) {
            return None;
        }
        let kind = match def {
            Definition::Pair(p1, p2) => {
                if !separated(p1, p2) {
                    return None;
                }
                if self.mode == DimMode::Aligned {
                    DimKind::Aligned {
                        p1,
                        p2,
                        line_point: p,
                    }
                } else {
                    let rotation = self.rotation.unwrap_or_else(|| {
                        let delta = (p2 - p1).abs();
                        let from_mid = (p - (p1 * 0.5 + p2 * 0.5)).abs();
                        if delta.x <= EPS {
                            FRAC_PI_2
                        } else if delta.y <= EPS || from_mid.y / delta.y >= from_mid.x / delta.x {
                            0.0
                        } else {
                            FRAC_PI_2
                        }
                    });
                    DimKind::Linear {
                        p1,
                        p2,
                        line_point: p,
                        rotation,
                    }
                }
            }
            Definition::Round { center, point } => {
                if !separated(center, point) {
                    return None;
                }
                if self.mode == DimMode::Radius {
                    DimKind::Radius { center, point }
                } else {
                    DimKind::Diameter { center, point }
                }
            }
            Definition::Angle {
                vertex,
                p1,
                p2,
                source,
            } => {
                let (p1, p2) = angle_rays(vertex, p1, p2, p, source)?;
                DimKind::Angular {
                    vertex,
                    p1,
                    p2,
                    arc_point: p,
                }
            }
            Definition::Ordinate(point) => {
                if !separated(point, p) || !(point - self.origin).is_finite() {
                    return None;
                }
                DimKind::Ordinate {
                    origin: self.origin,
                    point,
                    leader_end: p,
                    x_axis: self.x_axis,
                }
            }
        };
        let value = crate::dimgen::measure(&kind);
        (value.is_finite() && value < MAX_SIZE && (self.mode == DimMode::Ordinate || value > EPS))
            .then_some(kind)
    }

    fn dimension(
        &self,
        cx: &ToolCx<'_>,
        kind: DimKind,
        text_pos: Option<DVec2>,
    ) -> Option<EntityKind> {
        let t = &cx.drawing().tables;
        let st = t.dim_styles.get(&t.current_dim_style)?;
        if !t.text_styles.contains_key(&st.text_style)
            || !positive(st.scale)
            || !positive(st.text_height)
            || [
                st.text_height,
                st.arrow_size,
                st.ext_offset,
                st.ext_extend,
                st.text_gap,
            ]
            .iter()
            .any(|v| {
                !v.is_finite()
                    || *v < 0.0
                    || !(v * st.scale).is_finite()
                    || v * st.scale >= MAX_SIZE
            })
            || text_pos.is_some_and(|p| !valid_point(p))
        {
            return None;
        }
        Some(EntityKind::Dimension(Dimension {
            kind,
            style: t.current_dim_style,
            text_override: None,
            text_pos,
        }))
    }

    fn finish(&self, cx: &mut ToolCx<'_>, kind: DimKind, text: Option<DVec2>) -> ToolFlow {
        let Some(entity) = self.dimension(cx, kind, text) else {
            let message = annotate(cx.lang()).style_missing;
            return error(cx, message);
        };
        commit(cx, self.name(), entity)
    }
}

impl Tool for DimensionTool {
    fn name(&self) -> &'static str {
        match self.mode {
            DimMode::Linear => "DIMLINEAR",
            DimMode::Aligned => "DIMALIGNED",
            DimMode::Radius => "DIMRADIUS",
            DimMode::Diameter => "DIMDIAMETER",
            DimMode::Angular => "DIMANGULAR",
            DimMode::Ordinate => "DIMORDINATE",
        }
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = annotate(lang);
        match &self.step {
            Step::First => s.first.into(),
            Step::Second(_) => s.second.into(),
            Step::PickLine => s.pick_line.into(),
            Step::PickRound => s.pick_round.into(),
            Step::PickAngle => s.pick_angle.into(),
            Step::PickSecondLine(_) => s.pick_second_line.into(),
            Step::Vertex => s.vertex.into(),
            Step::FirstRay(_) => s.ray_first.into(),
            Step::SecondRay(..) => s.ray_second.into(),
            Step::OrdinatePoint => fmt(
                s.ordinate_point,
                &[
                    &format!("{},{}", self.origin.x, self.origin.y),
                    &if self.x_axis { "X" } else { "Y" },
                ],
            ),
            Step::Origin => s.origin_point.into(),
            Step::Rotation(_) => s.axis.into(),
            Step::TextPosition(_) => s.text_position.into(),
            Step::Place(def) => {
                let p = match def {
                    Definition::Pair(..) => s.dimension_position,
                    Definition::Round { .. } => s.text_position,
                    Definition::Angle {
                        source: AngleSource::Arc,
                        ..
                    } => s.arc_position_fixed,
                    Definition::Angle { .. } => s.arc_position,
                    Definition::Ordinate(_) => s.leader_position,
                };
                format!(
                    "{p}{}",
                    if self.custom_text {
                        s.custom_text_next
                    } else {
                        ""
                    }
                )
            }
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::PickLine | Step::PickRound | Step::PickAngle | Step::PickSecondLine(_) => {
                Accept::PICK
            }
            Step::Rotation(_) => Accept::VALUE,
            _ => Accept::POINT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::Second(p) | Step::FirstRay(p) | Step::SecondRay(p, _) => Some(p),
            Step::Place(Definition::Pair(p, _)) => Some(p),
            Step::Place(Definition::Round { center, .. }) => Some(center),
            Step::Place(Definition::Angle { vertex, .. }) => Some(vertex),
            Step::Place(Definition::Ordinate(p)) => Some(p),
            _ => None,
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = annotate(lang);
        let mut out = match self.step {
            Step::First => vec![kw("Object", "O", s.object)],
            Step::PickLine | Step::PickAngle => vec![kw("Points", "P", s.points)],
            Step::OrdinatePoint => vec![kw("Origin", "O", s.origin)],
            _ => vec![],
        };
        if matches!(self.step, Step::Place(Definition::Pair(..))) && self.mode == DimMode::Linear {
            out.extend([
                kw("Horizontal", "H", s.horizontal),
                kw("Vertical", "V", s.vertical),
                kw("Rotated", "R", s.rotated),
                kw("Automatic", "A", s.automatic),
            ]);
        }
        if matches!(
            self.step,
            Step::OrdinatePoint | Step::Place(Definition::Ordinate(_))
        ) {
            out.extend([kw("Xdatum", "X", s.x_datum), kw("Ydatum", "Y", s.y_datum)]);
        }
        if matches!(self.step, Step::Place(def) if !matches!(def, Definition::Round { .. })) {
            out.push(kw("TextPosition", "TP", s.custom_text));
        }
        out
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = annotate(cx.lang());
        if !finite_input(&input) {
            return error(cx, s.invalid);
        }
        if input == ToolInput::Escape {
            return ToolFlow::Cancel;
        }
        match (self.step.clone(), input) {
            (Step::First, ToolInput::Point(p)) => self.step = Step::Second(p),
            (Step::First, ToolInput::Enter | ToolInput::Keyword("Object")) => {
                self.step = Step::PickLine
            }
            (Step::PickLine, ToolInput::Keyword("Points")) => self.step = Step::First,
            (Step::Second(p1), ToolInput::Point(p2)) => {
                if !separated(p1, p2) {
                    return error(cx, s.degenerate);
                }
                self.step = Step::Place(Definition::Pair(p1, p2));
            }
            (Step::PickLine, ToolInput::Point(p)) => {
                let Some(Curve2::Line(l)) =
                    picked_curve(cx, p).filter(|c| matches!(c, Curve2::Line(l) if valid_line(l)))
                else {
                    return error(cx, s.line_required);
                };
                self.step = Step::Place(Definition::Pair(l.a, l.b));
            }
            (Step::PickRound, ToolInput::Point(p)) => {
                let (center, point) = match picked_curve(cx, p) {
                    Some(Curve2::Circle(c)) if c.c.is_finite() && positive(c.r) => {
                        (c.c, c.closest(p).1)
                    }
                    Some(Curve2::Arc(a)) if valid_arc(&a) => (a.c, a.closest(p).1),
                    _ => return error(cx, s.round_required),
                };
                if !separated(center, point) {
                    return error(cx, s.degenerate);
                }
                self.step = Step::Place(Definition::Round { center, point });
            }
            (Step::PickAngle, ToolInput::Keyword("Points") | ToolInput::Enter) => {
                self.step = Step::Vertex
            }
            (Step::PickAngle, ToolInput::Point(p)) => {
                self.step = match picked_curve(cx, p) {
                    Some(Curve2::Line(l)) if valid_line(&l) => Step::PickSecondLine(l),
                    Some(Curve2::Arc(a)) if valid_arc(&a) => Step::Place(Definition::Angle {
                        vertex: a.c,
                        p1: a.start_point(),
                        p2: a.end_point(),
                        source: AngleSource::Arc,
                    }),
                    _ => return error(cx, s.angle_required),
                };
            }
            (Step::PickSecondLine(l), ToolInput::Point(p)) => {
                let Some(Curve2::Line(other)) = picked_curve(cx, p) else {
                    return error(cx, s.line_required);
                };
                let Some(def) = line_angle(l, other) else {
                    return error(cx, s.parallel);
                };
                self.step = Step::Place(def);
            }
            (Step::Vertex, ToolInput::Point(p)) => self.step = Step::FirstRay(p),
            (Step::FirstRay(v), ToolInput::Point(p1)) => {
                if !separated(v, p1) {
                    return error(cx, s.degenerate);
                }
                self.step = Step::SecondRay(v, p1);
            }
            (Step::SecondRay(v, p1), ToolInput::Point(p2)) => {
                if !angle_valid(v, p1, p2) {
                    return error(cx, s.degenerate);
                }
                self.step = Step::Place(Definition::Angle {
                    vertex: v,
                    p1,
                    p2,
                    source: AngleSource::Rays,
                });
            }
            (Step::OrdinatePoint, ToolInput::Keyword("Origin")) => self.step = Step::Origin,
            (Step::Origin, ToolInput::Point(p)) => {
                self.origin = p;
                self.step = Step::OrdinatePoint;
            }
            (Step::Origin, ToolInput::Enter) => {
                self.origin = DVec2::ZERO;
                self.step = Step::OrdinatePoint;
            }
            (Step::OrdinatePoint, ToolInput::Point(p)) => {
                if !(p - self.origin).is_finite() {
                    return error(cx, s.degenerate);
                }
                self.step = Step::Place(Definition::Ordinate(p));
            }
            (
                Step::OrdinatePoint | Step::Place(Definition::Ordinate(_)),
                ToolInput::Keyword(k @ ("Xdatum" | "Ydatum")),
            ) => self.x_axis = k == "Xdatum",
            (Step::Place(def @ Definition::Pair(..)), ToolInput::Keyword(k))
                if self.mode == DimMode::Linear =>
            {
                match k {
                    "Horizontal" => self.rotation = Some(0.0),
                    "Vertical" => self.rotation = Some(FRAC_PI_2),
                    "Automatic" => self.rotation = None,
                    "Rotated" => self.step = Step::Rotation(def),
                    "TextPosition" => self.custom_text = true,
                    _ => {}
                }
            }
            (Step::Rotation(def), ToolInput::Value(a)) => {
                let a = a.rem_euclid(360.0).to_radians();
                if let Definition::Pair(p1, p2) = def
                    && !positive((p2 - p1).dot(DVec2::from_angle(a)).abs())
                {
                    return error(cx, s.degenerate);
                }
                self.rotation = Some(a);
                self.step = Step::Place(def);
            }
            (Step::Place(def), ToolInput::Keyword("TextPosition"))
                if !matches!(def, Definition::Round { .. }) =>
            {
                self.custom_text = true
            }
            (Step::Place(def), ToolInput::Point(p)) => {
                let Some(kind) = self.kind_at(def, p) else {
                    return error(cx, s.degenerate);
                };
                if self.custom_text {
                    self.step = Step::TextPosition(kind);
                } else {
                    return self.finish(
                        cx,
                        kind,
                        matches!(def, Definition::Round { .. }).then_some(p),
                    );
                }
            }
            (Step::TextPosition(kind), ToolInput::Point(p)) => {
                return self.finish(cx, kind, Some(p));
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let Some(p) = cx.cursor().filter(|p| p.is_finite()) else {
            return;
        };
        let candidate = match self.step.clone() {
            Step::Place(def) => self
                .kind_at(def, p)
                .map(|k| (k, matches!(def, Definition::Round { .. }).then_some(p))),
            Step::TextPosition(k) => Some((k, Some(p))),
            Step::Second(a) | Step::FirstRay(a) | Step::SecondRay(a, _) => {
                out.points.push(a);
                if separated(a, p) {
                    out.curves.push(Curve2::Line(Line2::new(a, p)));
                }
                None
            }
            _ => None,
        };
        if let Some((kind, text)) = candidate
            && let Some(entity) = self.dimension(cx, kind, text)
        {
            ghost(cx, out, entity);
        }
    }
}
