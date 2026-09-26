//! CIRCLE, including picked line/circle/arc carriers and individual polyline segments for Ttr.

use wcad_doc::EntityKind;
use wcad_geom2d::{Arc2, Circle2, Curve, Curve2, Line2, PolySegment, intersect_tol, offset_signed};
use wcad_math::{DVec2, cross2};

use super::{
    angle_of, arc_3p, commit, defaults, finite_pts, kw, num, positive, set_defaults, strings_of,
};
use crate::i18n::{Lang, core, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

fn checked_circle(c: DVec2, r: f64) -> Option<Circle2> {
    let lo = c - DVec2::splat(r);
    let hi = c + DVec2::splat(r);
    (c.is_finite()
        && positive(r)
        && (r * std::f64::consts::TAU).is_finite()
        && lo.is_finite()
        && hi.is_finite()
        && lo.x < hi.x
        && lo.y < hi.y)
        .then_some(Circle2::new(c, r))
}

/// Circle with diameter endpoints `a`, `b`.
pub fn circle_2p(a: DVec2, b: DVec2) -> Option<Circle2> {
    if !finite_pts(&[a, b]) {
        return None;
    }
    checked_circle(a * 0.5 + b * 0.5, a.distance(b) * 0.5)
}

/// Circumcircle through three distinct, non-collinear points.
pub fn circle_3p(a: DVec2, b: DVec2, c: DVec2) -> Option<Circle2> {
    let arc = arc_3p(a, b, c)?;
    checked_circle(arc.c, arc.r)
}

/// A picked Ttr object. Lines use their infinite carriers; arcs retain their angular bounds.
/// A polyline pick is resolved to exactly one of its line or arc segments.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TanGeom {
    Line(Line2),
    Circle(Circle2),
    Arc(Arc2),
}

impl TanGeom {
    fn curve(self) -> Curve2 {
        match self {
            Self::Line(l) => Curve2::Line(l),
            Self::Circle(c) => Curve2::Circle(c),
            Self::Arc(a) => Curve2::Arc(a),
        }
    }

    fn circle(self) -> Option<Circle2> {
        match self {
            Self::Line(_) => None,
            Self::Circle(c) => Some(c),
            Self::Arc(a) => Some(Circle2::new(a.c, a.r)),
        }
    }

    fn valid(self) -> bool {
        match self {
            Self::Line(l) => finite_pts(&[l.a, l.b]) && positive(l.length()),
            Self::Circle(c) => checked_circle(c.c, c.r).is_some(),
            Self::Arc(a) => {
                checked_circle(a.c, a.r).is_some()
                    && a.start.is_finite()
                    && a.end.is_finite()
                    && a.sweep() > 1e-12
                    && a.sweep() < std::f64::consts::TAU - 1e-12
            }
        }
    }

    fn normalized(self, origin: DVec2, scale: f64) -> Self {
        match self {
            Self::Line(l) => Self::Line(Line2::new((l.a - origin) / scale, (l.b - origin) / scale)),
            Self::Circle(c) => Self::Circle(Circle2::new((c.c - origin) / scale, c.r / scale)),
            Self::Arc(a) => Self::Arc(Arc2::new(
                (a.c - origin) / scale,
                a.r / scale,
                a.start,
                a.end,
            )),
        }
    }

    fn same_carrier(self, other: Self) -> bool {
        match (self, other) {
            (Self::Line(a), Self::Line(b)) => {
                cross2(a.dir(), b.dir()).abs() < 1e-12
                    && cross2(a.dir(), b.a - a.a).abs() <= 1e-12 * a.length().max(b.length())
            }
            _ => match (self.circle(), other.circle()) {
                (Some(a), Some(b)) => a.c == b.c && a.r == b.r,
                _ => false,
            },
        }
    }

    /// Center loci for both sides of a carrier. Internal tangency also exists when r > R.
    fn loci(self, r: f64, line_extent: f64) -> Vec<Curve2> {
        match self {
            Self::Line(l) => {
                let d = l.dir();
                let foot = l.a - d * l.a.dot(d);
                let long = Curve2::Line(Line2::new(foot - d * line_extent, foot + d * line_extent));
                let mut out = offset_signed(&long, r);
                out.extend(offset_signed(&long, -r));
                out
            }
            _ => {
                let c = self.circle().unwrap();
                [c.r + r, (c.r - r).abs()]
                    .into_iter()
                    .filter_map(|radius| checked_circle(c.c, radius).map(Curve2::Circle))
                    .collect()
            }
        }
    }

    /// A true tangent point, not merely the nearest endpoint of a trimmed arc.
    fn contact(self, circle: Circle2) -> Option<DVec2> {
        let tol = circle.r * 1e-8;
        match self {
            Self::Line(l) => {
                let d = l.dir();
                let p = l.a + d * (circle.c - l.a).dot(d);
                ((circle.c.distance(p) - circle.r).abs() <= tol).then_some(p)
            }
            _ => {
                let c = self.circle()?;
                let d = (circle.c - c.c).try_normalize()?;
                [c.c + d * c.r, c.c - d * c.r].into_iter().find(|p| {
                    (circle.c.distance(*p) - circle.r).abs() <= tol
                        && match self {
                            Self::Arc(a) => a.param_of_angle(angle_of(*p - a.c), 1e-10).is_some(),
                            _ => true,
                        }
                })
            }
        }
    }

    fn from_kind(kind: &EntityKind, pick: DVec2) -> Option<Self> {
        let geom = match kind {
            EntityKind::Line(l) => Self::Line(*l),
            EntityKind::Circle(c) => Self::Circle(*c),
            EntityKind::Arc(a) => Self::Arc(*a),
            EntityKind::Polyline(p) => p
                .segments()
                .map(|s| match s {
                    PolySegment::Line(l) => Self::Line(l),
                    PolySegment::Arc(a) => Self::Arc(a.to_arc()),
                })
                .filter(|g| g.valid())
                .min_by(|a, b| {
                    a.curve()
                        .closest(pick)
                        .1
                        .distance_squared(pick)
                        .total_cmp(&b.curve().closest(pick).1.distance_squared(pick))
                })?,
            _ => return None,
        };
        geom.valid().then_some(geom)
    }
}

/// Circle tangent to two picked objects. Chooses the solution whose tangent points are closest
/// to the picks. Supports internal/external circle tangencies and parallel lines 2*r apart.
pub fn circle_ttr(
    a: TanGeom,
    b: TanGeom,
    radius: f64,
    pick_a: DVec2,
    pick_b: DVec2,
) -> Option<Circle2> {
    if !positive(radius) || !finite_pts(&[pick_a, pick_b]) || !a.valid() || !b.valid() {
        return None;
    }
    // Normalize before intersection: its absolute tolerances must not depend on drawing units.
    let origin = pick_a * 0.5 + pick_b * 0.5;
    let bbox = a.curve().bbox().union(&b.curve().bbox());
    let scale = (bbox.min - origin)
        .abs()
        .max((bbox.max - origin).abs())
        .max((pick_a - origin).abs())
        .max((pick_b - origin).abs())
        .max_element()
        .max(radius);
    if !positive(scale) {
        return None;
    }
    let (a, b) = (a.normalized(origin, scale), b.normalized(origin, scale));
    let (pa, pb) = ((pick_a - origin) / scale, (pick_b - origin) / scale);
    let r = radius / scale;
    if !positive(r) || !a.valid() || !b.valid() || a.same_carrier(b) {
        return None;
    }
    // Every circular locus fits in this bound. Two nearly parallel lines need a longer carrier;
    // the actual intersections still come exclusively from geom2d's intersection implementation.
    let extent = match (a, b) {
        (TanGeom::Line(x), TanGeom::Line(y)) => {
            let sin = cross2(x.dir(), y.dir()).abs();
            if sin > 1e-12 { 8.0 / sin } else { 8.0 }
        }
        _ => 8.0,
    };
    let (la, lb) = (a.loci(r, extent), b.loci(r, extent));
    let mut centers = Vec::new();
    for x in &la {
        for y in &lb {
            centers.extend(intersect_tol(x, y, 1e-10 * r).into_iter().map(|h| h.p));
            // Coincident circular loci have infinitely many solutions. Minimize the same
            // tangent-point score analytically, including a containing circle's reversed contact.
            if let (Curve2::Circle(x), Curve2::Circle(y)) = (x, y)
                && x.c.distance(y.c) <= 1e-12
                && (x.r - y.r).abs() <= 1e-12
                && let (Some(ca), Some(cb)) = (a.circle(), b.circle())
            {
                let sa = if r > ca.r && (x.r - (r - ca.r)).abs() <= 1e-12 {
                    -1.0
                } else {
                    1.0
                };
                let sb = if r > cb.r && (x.r - (r - cb.r)).abs() <= 1e-12 {
                    -1.0
                } else {
                    1.0
                };
                let v = (pa - x.c) * (sa * ca.r) + (pb - x.c) * (sb * cb.r);
                centers.push(x.c + v.normalize_or(DVec2::X) * x.r);
            }
        }
    }
    // Coincident parallel line loci: project the average pick. Arc endpoints also delimit
    // admissible intervals when two circular loci coincide but the objects are trimmed arcs.
    for locus in la.iter().chain(&lb) {
        centers.push(locus.closest((pa + pb) * 0.5).1);
        for geom in [a, b] {
            if let TanGeom::Arc(arc) = geom {
                for p in [arc.start_point(), arc.end_point()] {
                    centers.push(locus.closest(p).1);
                    centers.push(locus.closest(arc.c * 2.0 - p).1);
                }
            }
        }
    }
    let best = centers
        .into_iter()
        .filter_map(|c| {
            let circle = checked_circle(c, r)?;
            let ta = a.contact(circle)?;
            let tb = b.contact(circle)?;
            let score = ta.distance_squared(pa) + tb.distance_squared(pb);
            score.is_finite().then_some((circle, score))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))?
        .0;
    checked_circle(origin + best.c * scale, radius)
}

#[derive(Clone, Copy, Debug, Default)]
enum Step {
    #[default]
    Center,
    Radius {
        c: DVec2,
        diameter: bool,
    },
    TwoFirst,
    TwoSecond {
        a: DVec2,
    },
    ThreeFirst,
    ThreeSecond {
        a: DVec2,
    },
    ThreeThird {
        a: DVec2,
        b: DVec2,
    },
    TanFirst,
    TanSecond {
        a: TanGeom,
        pa: DVec2,
    },
    TanRadius {
        a: TanGeom,
        b: TanGeom,
        pa: DVec2,
        pb: DVec2,
    },
}

/// CIRCLE: center/radius or diameter, 2P, 3P and tangent/tangent/radius.
#[derive(Default)]
pub struct CircleTool {
    step: Step,
}

impl CircleTool {
    fn make(&self, c: Option<Circle2>, cx: &mut ToolCx<'_>) -> ToolFlow {
        if let Some(c) = c {
            commit(cx, "CIRCLE", EntityKind::Circle(c));
            set_defaults(|d| d.circle_radius = c.r);
            ToolFlow::Done
        } else {
            let text = strings_of(cx.lang());
            cx.error(match self.step {
                Step::Radius { .. } => core(cx.lang()).value_must_be_positive,
                Step::ThreeThird { .. } => text.collinear,
                _ => text.circle_none,
            });
            ToolFlow::Continue
        }
    }

    fn value_circle(&self, v: f64) -> Option<Circle2> {
        match self.step {
            Step::Radius { c, diameter } => checked_circle(c, if diameter { v * 0.5 } else { v }),
            Step::TanRadius { a, b, pa, pb } => circle_ttr(a, b, v, pa, pb),
            _ => None,
        }
    }

    fn point_circle(&self, p: DVec2) -> Option<Circle2> {
        if !p.is_finite() {
            return None;
        }
        match self.step {
            Step::Radius { c, .. } => self.value_circle(c.distance(p)),
            Step::TwoSecond { a } => circle_2p(a, p),
            Step::ThreeThird { a, b } => circle_3p(a, b, p),
            Step::TanRadius { pb, .. } => self.value_circle(pb.distance(p)),
            _ => None,
        }
    }

    fn pick(cx: &ToolCx<'_>, p: DVec2) -> Result<(TanGeom, DVec2), &'static str> {
        let text = strings_of(cx.lang());
        let id = cx.pick(p).ok_or(text.nothing_picked)?;
        let entity = cx.entity(id).ok_or(text.nothing_picked)?;
        let geom = TanGeom::from_kind(&entity.kind, p).ok_or(text.tangent_unsupported)?;
        Ok((geom, geom.curve().closest(p).1))
    }
}

impl Tool for CircleTool {
    fn name(&self) -> &'static str {
        "CIRCLE"
    }

    fn prompt(&self, lang: Lang) -> String {
        let text = strings_of(lang);
        match self.step {
            Step::Center => text.circle_center,
            Step::Radius {
                diameter: false, ..
            } => core(lang).circle_radius,
            Step::Radius { diameter: true, .. } => core(lang).circle_diameter,
            Step::TwoFirst => text.circle_2p_first,
            Step::TwoSecond { .. } => text.circle_2p_second,
            Step::ThreeFirst => text.circle_3p_first,
            Step::ThreeSecond { .. } => text.circle_3p_second,
            Step::ThreeThird { .. } => text.circle_3p_third,
            Step::TanFirst => text.circle_tan1,
            Step::TanSecond { .. } => text.circle_tan2,
            Step::TanRadius { .. } => {
                let radius = defaults().circle_radius;
                return if positive(radius) {
                    fmt(text.circle_ttr_radius_default, &[&num(radius)])
                } else {
                    text.circle_ttr_radius.into()
                };
            }
        }
        .into()
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let text = strings_of(lang);
        match self.step {
            Step::Center => vec![
                kw("3P", "3P", text.kw_3p),
                kw("2P", "2P", text.kw_2p),
                kw("Ttr", "T", text.kw_ttr),
            ],
            Step::Radius {
                diameter: false, ..
            } => vec![kw("Diameter", "D", core(lang).kw_diameter)],
            _ => vec![],
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::TanFirst | Step::TanSecond { .. } => Accept::PICK,
            Step::Radius { .. } | Step::TanRadius { .. } => Accept::POINT_OR_VALUE,
            _ => Accept::POINT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::Radius { c, .. } => Some(c),
            Step::TwoSecond { a } | Step::ThreeSecond { a } => Some(a),
            Step::ThreeThird { b, .. } => Some(b),
            Step::TanRadius { pb, .. } => Some(pb),
            _ => None,
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match (self.step, input) {
            (_, ToolInput::Escape) | (Step::Center, ToolInput::Enter) => return ToolFlow::Cancel,
            (_, ToolInput::Point(p)) if !p.is_finite() => return self.make(None, cx),
            (Step::Center, ToolInput::Point(c)) => self.step = Step::Radius { c, diameter: false },
            (Step::Center, ToolInput::Keyword("2P")) => self.step = Step::TwoFirst,
            (Step::Center, ToolInput::Keyword("3P")) => self.step = Step::ThreeFirst,
            (Step::Center, ToolInput::Keyword("Ttr")) => self.step = Step::TanFirst,
            (Step::Radius { c, diameter: false }, ToolInput::Keyword("Diameter")) => {
                self.step = Step::Radius { c, diameter: true }
            }
            (Step::TwoFirst, ToolInput::Point(a)) => self.step = Step::TwoSecond { a },
            (Step::ThreeFirst, ToolInput::Point(a)) => self.step = Step::ThreeSecond { a },
            (Step::ThreeSecond { a }, ToolInput::Point(b)) if positive(a.distance(b)) => {
                self.step = Step::ThreeThird { a, b }
            }
            (Step::TanFirst, ToolInput::Point(p)) => match Self::pick(cx, p) {
                Ok((a, pa)) => self.step = Step::TanSecond { a, pa },
                Err(error) => cx.error(error),
            },
            (Step::TanSecond { a, pa }, ToolInput::Point(p)) => match Self::pick(cx, p) {
                Ok((b, pb)) if !a.same_carrier(b) => self.step = Step::TanRadius { a, b, pa, pb },
                Ok(_) => cx.error(strings_of(cx.lang()).circle_none),
                Err(error) => cx.error(error),
            },
            (_, ToolInput::Point(p)) => return self.make(self.point_circle(p), cx),
            (Step::Radius { .. } | Step::TanRadius { .. }, ToolInput::Value(v)) => {
                return self.make(self.value_circle(v), cx);
            }
            (Step::TanRadius { .. }, ToolInput::Enter) if positive(defaults().circle_radius) => {
                return self.make(self.value_circle(defaults().circle_radius), cx);
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let Some(p) = cx.cursor().filter(|p| p.is_finite()) else {
            return;
        };
        if let Some(base) = self.base_point() {
            out.points.push(base);
            out.rubber_band = true;
        }
        let circle = match self.step {
            Step::TanSecond { a, pa } => {
                out.points.push(pa);
                Self::pick(cx, p)
                    .ok()
                    .and_then(|(b, pb)| circle_ttr(a, b, defaults().circle_radius, pa, pb))
            }
            _ => self.point_circle(p),
        };
        if let Some(c) = circle {
            out.curves.push(Curve2::Circle(c));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use std::f64::consts::PI;
    use wcad_geom2d::{PolyVertex, Polyline2};

    fn near(a: DVec2, b: DVec2) {
        assert!(a.distance(b) < 1e-8, "{a:?} != {b:?}");
    }

    fn circle(h: &Harness, i: usize) -> Circle2 {
        let EntityKind::Circle(c) = h.of_type("CIRCLE")[i].1 else {
            panic!("expected circle")
        };
        c
    }

    fn axes() -> (TanGeom, TanGeom) {
        (
            TanGeom::Line(Line2::new(-DVec2::X * 10.0, DVec2::X * 10.0)),
            TanGeom::Line(Line2::new(-DVec2::Y * 10.0, DVec2::Y * 10.0)),
        )
    }

    #[test]
    fn diameter_and_circumcircle_geometry() {
        let c = circle_2p(-DVec2::X, DVec2::X).unwrap();
        near(c.c, DVec2::ZERO);
        assert_eq!(c.r, 1.0);
        for points in [
            [DVec2::X, DVec2::Y, -DVec2::X],
            [-DVec2::X, DVec2::Y, DVec2::X],
        ] {
            let c = circle_3p(points[0], points[1], points[2]).unwrap();
            near(c.c, DVec2::ZERO);
            assert!((c.r - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn invalid_points_and_radius_are_rejected() {
        assert!(circle_2p(DVec2::X, DVec2::X).is_none());
        assert!(circle_3p(DVec2::ZERO, DVec2::X, DVec2::X * 2.0).is_none());
        let (a, b) = axes();
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(circle_2p(DVec2::new(v, 0.0), DVec2::ZERO).is_none());
            assert!(circle_3p(DVec2::X, DVec2::Y, DVec2::new(0.0, v)).is_none());
            assert!(circle_ttr(a, b, v, DVec2::X, DVec2::Y).is_none());
        }
        for r in [0.0, -1.0] {
            assert!(circle_ttr(a, b, r, DVec2::X, DVec2::Y).is_none());
        }
        assert!(circle_ttr(a, a, 1.0, DVec2::X, -DVec2::X).is_none());
        assert!(checked_circle(DVec2::ZERO, f64::MAX).is_none());
        assert!(checked_circle(DVec2::splat(1e20), 1.0).is_none());
    }

    #[test]
    fn ttr_line_quadrants_are_selected_by_picks() {
        let (a, b) = axes();
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                let c = circle_ttr(
                    a,
                    b,
                    2.0,
                    DVec2::new(x * 2.0, 0.0),
                    DVec2::new(0.0, y * 2.0),
                )
                .unwrap();
                near(c.c, DVec2::new(x * 2.0, y * 2.0));
                assert_eq!(c.r, 2.0);
            }
        }
    }

    #[test]
    fn ttr_parallel_lines_choose_near_average_pick() {
        let a = TanGeom::Line(Line2::new(DVec2::ZERO, DVec2::X * 10.0));
        let b = TanGeom::Line(Line2::new(DVec2::Y * 4.0, DVec2::new(10.0, 4.0)));
        let c = circle_ttr(a, b, 2.0, DVec2::X, DVec2::new(3.0, 4.0)).unwrap();
        near(c.c, DVec2::new(2.0, 2.0));
        assert!(circle_ttr(a, b, 1.0, DVec2::X, DVec2::new(3.0, 4.0)).is_none());
    }

    #[test]
    fn ttr_line_circle_external_and_containing_contacts() {
        let (line, _) = axes();
        let object = TanGeom::Circle(Circle2::new(DVec2::new(0.0, 4.0), 2.0));
        let c = circle_ttr(line, object, 1.0, DVec2::ZERO, DVec2::new(0.0, 2.0)).unwrap();
        near(c.c, DVec2::Y);
        let x = 8.0f64.sqrt();
        let pick = DVec2::new(-2.0 * x / 3.0, 4.0 - 2.0 / 3.0);
        let c = circle_ttr(line, object, 5.0, DVec2::new(x, 0.0), pick).unwrap();
        near(c.c, DVec2::new(x, 5.0));
        near(object.contact(c).unwrap(), pick);
    }

    #[test]
    fn ttr_circle_circle_chooses_side_and_internal_solution() {
        let a = TanGeom::Circle(Circle2::new(-DVec2::X * 2.0, 1.0));
        let b = TanGeom::Circle(Circle2::new(DVec2::X * 2.0, 1.0));
        for sign in [-1.0, 1.0] {
            let y = sign * 5.0f64.sqrt();
            let c = circle_ttr(
                a,
                b,
                2.0,
                DVec2::new(-4.0 / 3.0, y / 3.0),
                DVec2::new(4.0 / 3.0, y / 3.0),
            )
            .unwrap();
            near(c.c, DVec2::new(0.0, y));
        }
        let c = circle_ttr(a, b, 3.0, -DVec2::X * 3.0, DVec2::X * 3.0).unwrap();
        near(c.c, DVec2::ZERO);
        let a = TanGeom::Circle(Circle2::new(DVec2::ZERO, 1.0));
        let b = TanGeom::Circle(Circle2::new(DVec2::ZERO, 3.0));
        let c = circle_ttr(a, b, 1.0, DVec2::Y, DVec2::Y * 3.0).unwrap();
        near(c.c, DVec2::Y * 2.0);
    }

    #[test]
    fn ttr_arc_contact_is_on_actual_sweep() {
        let (line, _) = axes();
        let c = DVec2::new(0.0, 4.0);
        let bottom = TanGeom::Arc(Arc2::new(c, 2.0, PI, 2.0 * PI));
        let top = TanGeom::Arc(Arc2::new(c, 2.0, 0.0, PI));
        let result = circle_ttr(line, bottom, 1.0, DVec2::ZERO, c - DVec2::Y * 2.0).unwrap();
        near(result.c, DVec2::Y);
        assert!(circle_ttr(line, top, 1.0, DVec2::ZERO, c + DVec2::Y * 2.0).is_none());
    }

    #[test]
    fn ttr_normalization_preserves_drawing_scale_and_translation() {
        for scale in [1e-6, 1.0, 1e6] {
            let origin = DVec2::new(100.0, -300.0) * scale;
            let a = TanGeom::Line(Line2::new(
                origin - DVec2::X * scale,
                origin + DVec2::X * scale,
            ));
            let b = TanGeom::Line(Line2::new(
                origin - DVec2::Y * scale,
                origin + DVec2::Y * scale,
            ));
            let c = circle_ttr(
                a,
                b,
                scale,
                origin + DVec2::X * scale,
                origin + DVec2::Y * scale,
            )
            .unwrap();
            assert!((c.c - origin - DVec2::ONE * scale).length() < 1e-8 * scale);
        }
    }

    #[test]
    fn polyline_pick_chooses_line_or_bulge_segment() {
        let p = Polyline2 {
            verts: vec![
                PolyVertex::new(DVec2::ZERO),
                PolyVertex::with_bulge(DVec2::X * 2.0, 1.0),
                PolyVertex::new(DVec2::new(2.0, 2.0)),
            ],
            closed: false,
        };
        let kind = EntityKind::Polyline(p);
        assert!(matches!(
            TanGeom::from_kind(&kind, DVec2::X),
            Some(TanGeom::Line(_))
        ));
        let Some(TanGeom::Arc(a)) = TanGeom::from_kind(&kind, DVec2::new(3.0, 1.0)) else {
            panic!("arc segment")
        };
        near(a.c, DVec2::new(2.0, 1.0));
        assert!((a.sweep() - PI).abs() < 1e-12);
    }

    #[test]
    fn tool_radius_and_diameter_value_point_preview_retry() {
        for (diameter, value) in [(false, true), (true, true), (false, false), (true, false)] {
            let mut h = Harness::new();
            h.ed.draft.osnap_on = false;
            h.cmd("CIRCLE").cmd("0,0");
            if diameter {
                h.cmd("D");
            }
            h.cmd("0").cmd("-2");
            assert_eq!(h.count("CIRCLE"), 0);
            h.hover(4.0, 0.0);
            let Curve2::Circle(preview) = h.ed.preview.curves[0] else {
                panic!("circle preview")
            };
            assert_eq!(preview.r, if diameter { 2.0 } else { 4.0 });
            if value {
                h.cmd("4");
            } else {
                h.click(4.0, 0.0);
            }
            assert_eq!(h.count("CIRCLE"), 1);
            assert_eq!(circle(&h, 0).r, preview.r);
            h.ed.undo();
            assert_eq!(h.count("CIRCLE"), 0);
            h.ed.redo();
            assert_eq!(h.count("CIRCLE"), 1);
        }
    }

    #[test]
    fn tool_two_and_three_points_retry_preview_and_commit() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("CIRCLE").cmd("2P").cmd("-1,0").cmd("-1,0");
        assert_eq!(h.count("CIRCLE"), 0);
        h.hover(1.0, 0.0);
        assert!(matches!(
            h.ed.preview.curves.as_slice(),
            [Curve2::Circle(_)]
        ));
        h.cmd("1,0");
        assert_eq!(circle(&h, 0).r, 1.0);
        h.cmd("CIRCLE").cmd("3P").cmd("1,0").cmd("1,0");
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).circle_3p_second);
        h.cmd("0,1").cmd("-1,2");
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).circle_3p_third);
        h.hover(-1.0, 0.0);
        assert!(matches!(
            h.ed.preview.curves.as_slice(),
            [Curve2::Circle(_)]
        ));
        h.cmd("-1,0");
        near(circle(&h, 1).c, DVec2::ZERO);
        h.ed.undo();
        assert_eq!(h.count("CIRCLE"), 1);
    }

    #[test]
    fn tool_ttr_picks_retry_radius_preview_and_default() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = true;
        h.ed.draft.ortho = true;
        h.cmd("LINE").cmd("-10,0").cmd("10,0").enter();
        h.cmd("LINE").cmd("0,-10").cmd("0,10").enter();
        h.cmd("CIRCLE").cmd("Ttr");
        assert!(h.ed.tool_accepts().pick);
        h.click(5.0, 5.0);
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).circle_tan1);
        h.click(2.0, 0.0).click(3.0, 0.0);
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).circle_tan2);
        h.click(0.0, 2.0).cmd("-1");
        assert!(h.ed.tool_accepts().value);
        assert_eq!(h.count("CIRCLE"), 0);
        h.hover(2.0, 2.0);
        assert!(matches!(
            h.ed.preview.curves.as_slice(),
            [Curve2::Circle(_)]
        ));
        h.cmd("2");
        near(circle(&h, 0).c, DVec2::splat(2.0));
        h.ed.undo();
        assert_eq!(h.count("CIRCLE"), 0);
        assert_eq!(h.count("LINE"), 2);
        h.cmd("CIRCLE")
            .cmd("Ttr")
            .click(-2.0, 0.0)
            .click(0.0, -2.0)
            .enter();
        near(circle(&h, 0).c, DVec2::splat(-2.0));
    }

    #[test]
    fn tool_ttr_circle_arc_and_polyline_picks() {
        for target in [
            EntityKind::Circle(Circle2::new(DVec2::new(0.0, 4.0), 2.0)),
            EntityKind::Arc(Arc2::new(DVec2::new(0.0, 4.0), 2.0, PI, 2.0 * PI)),
            EntityKind::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::with_bulge(DVec2::new(-2.0, 4.0), 1.0),
                    PolyVertex::new(DVec2::new(2.0, 4.0)),
                ],
                closed: false,
            }),
        ] {
            let mut h = Harness::new();
            h.ed.doc.transact("objects", |tx| {
                tx.add(EntityKind::Line(Line2::new(
                    -DVec2::X * 10.0,
                    DVec2::X * 10.0,
                )));
                tx.add(target);
            });
            h.ed.pump();
            let before = h.count("CIRCLE");
            h.cmd("CIRCLE")
                .cmd("Ttr")
                .click(0.0, 0.0)
                .click(0.0, 2.0)
                .cmd("1");
            assert_eq!(h.count("CIRCLE"), before + 1);
            near(circle(&h, before).c, DVec2::Y);
        }
    }

    #[test]
    fn tool_ttr_no_solution_keeps_radius_step_for_retry() {
        let mut h = Harness::new();
        h.cmd("LINE").cmd("0,0").cmd("10,0").enter();
        h.cmd("LINE").cmd("0,4").cmd("10,4").enter();
        h.cmd("CIRCLE").cmd("Ttr").click(1.0, 0.0).click(3.0, 4.0);
        let prompt = h.ed.prompt();
        h.cmd("1");
        assert_eq!(h.ed.prompt(), prompt);
        assert_eq!(h.count("CIRCLE"), 0);
        h.cmd("2");
        near(circle(&h, 0).c, DVec2::new(2.0, 2.0));
    }

    #[test]
    fn tool_ttr_mouse_radius_and_cancel_preserve_source_geometry() {
        for picks in [1, 2, 3] {
            let mut h = Harness::new();
            h.ed.draft.osnap_on = false;
            h.cmd("LINE").cmd("-10,0").cmd("10,0").enter();
            h.cmd("LINE").cmd("0,-10").cmd("0,10").enter();
            h.cmd("CIRCLE").cmd("Ttr").click(2.0, 0.0);
            if picks >= 2 {
                h.click(0.0, 2.0);
            }
            if picks == 3 {
                h.hover(2.0, 2.0).click(2.0, 2.0);
                near(circle(&h, 0).c, DVec2::splat(2.0));
                h.ed.undo();
            } else {
                h.esc();
            }
            assert_eq!(h.count("CIRCLE"), 0);
            assert_eq!(h.count("LINE"), 2);
            assert!(!h.ed.has_tool());
        }
    }

    #[test]
    fn tool_cancel_all_point_modes_without_commit() {
        for prefix in [
            vec![],
            vec!["0,0"],
            vec!["0,0", "D"],
            vec!["2P"],
            vec!["2P", "1,0"],
            vec!["3P"],
            vec!["3P", "1,0"],
            vec!["3P", "1,0", "0,1"],
            vec!["Ttr"],
        ] {
            let mut h = Harness::new();
            h.cmd("CIRCLE");
            for input in prefix {
                h.cmd(input);
            }
            let prompt = h.ed.prompt();
            h.ed.feed(ToolInput::Point(DVec2::new(f64::NAN, 0.0)));
            assert_eq!(h.ed.prompt(), prompt);
            h.esc();
            assert_eq!(h.count("CIRCLE"), 0);
            assert!(!h.ed.has_tool());
        }
    }
}
