//! Ellipses and elliptical arcs. Interactive angles are polar angles relative to the first
//! entered axis; the persisted curve uses parametric angles relative to its actual major axis.

use std::f64::consts::{FRAC_PI_2, TAU};

use wcad_doc::EntityKind;
use wcad_geom2d::{Curve2, EllipseArc2, Line2};
use wcad_math::{DVec2, normalize_0_2pi, perp};

use super::{commit, kw, strings_of};
use crate::i18n::{Lang, core};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

const ANGLE_EPS: f64 = 1e-12;

/// Construct an ellipse/CCW arc from a center, the first semi-axis vector and the perpendicular
/// semi-axis length. `start` and `end` are *parametric radians in the entered axes*, not polar
/// angles. Use `(0, TAU)` for a full ellipse; equal angles are rejected. A negative difference
/// wraps counter-clockwise, and at most one full turn is accepted.
///
/// If the other radius is larger, the major axis rotates by +90 degrees and both parameters
/// shift by -90 degrees, preserving the curve's endpoints and traversal. Non-finite, collapsed
/// or numerically unrepresentable geometry returns `None`.
pub fn ellipse_from_axes(
    center: DVec2,
    first_axis: DVec2,
    other_radius: f64,
    start: f64,
    end: f64,
) -> Option<EllipseArc2> {
    if !center.is_finite()
        || !first_axis.is_finite()
        || !other_radius.is_finite()
        || !start.is_finite()
        || !end.is_finite()
    {
        return None;
    }
    let radius = first_axis.length();
    let eps = 1e-12 * (1.0 + center.abs().max_element().max(radius).max(other_radius));
    if !radius.is_finite()
        || radius <= eps
        || other_radius <= eps
        || !(other_radius * other_radius).is_finite()
    {
        return None;
    }
    let delta = end - start;
    if !delta.is_finite() || delta.abs() <= ANGLE_EPS || delta.abs() > TAU + ANGLE_EPS {
        return None;
    }
    let full = (delta.abs() - TAU).abs() <= ANGLE_EPS;
    let sweep = if full { TAU } else { normalize_0_2pi(delta) };
    if !full && (sweep <= ANGLE_EPS || TAU - sweep <= ANGLE_EPS) {
        return None;
    }
    let (major, ratio, shift) = if other_radius > radius {
        (
            perp(first_axis / radius) * other_radius,
            radius / other_radius,
            FRAC_PI_2,
        )
    } else {
        (first_axis, other_radius / radius, 0.0)
    };
    let start = normalize_0_2pi(normalize_0_2pi(start) - shift);
    let ellipse = EllipseArc2 {
        c: center,
        major,
        ratio,
        start,
        end: start + sweep,
    };
    let extent = major.abs() + ellipse.minor().abs();
    (major.is_finite()
        && ratio > 0.0
        && ratio <= 1.0
        && extent.length_squared().is_finite()
        && (center - extent).is_finite()
        && (center + extent).is_finite())
    .then_some(ellipse)
}

#[derive(Clone, Copy, Debug)]
struct Axes {
    center: DVec2,
    first: DVec2,
    other: f64,
}

impl Axes {
    fn full(self) -> Option<EllipseArc2> {
        ellipse_from_axes(self.center, self.first, self.other, 0.0, TAU)
    }

    /// Convert polar angles before normalizing the major axis. Negative included angles select
    /// the clockwise arc's locus, stored with reversed endpoints because EllipseArc2 is CCW.
    fn arc(self, start: f64, sweep: f64) -> Option<EllipseArc2> {
        if !start.is_finite()
            || !sweep.is_finite()
            || sweep.abs() <= ANGLE_EPS
            || sweep.abs() >= TAU - ANGLE_EPS
        {
            return None;
        }
        let a = self.first.length();
        let scale = a.max(self.other);
        let param = |angle: f64| {
            let angle = normalize_0_2pi(angle);
            (a / scale * angle.sin()).atan2(self.other / scale * angle.cos())
        };
        let (s, e) = if sweep > 0.0 {
            (param(start), param(start + sweep))
        } else {
            (param(start + sweep), param(start))
        };
        let span = normalize_0_2pi(e - s);
        if span <= ANGLE_EPS || TAU - span <= ANGLE_EPS {
            return None;
        }
        ellipse_from_axes(self.center, self.first, self.other, s, s + span)
    }
}

fn point_angle(center: DVec2, axis: DVec2, point: DVec2) -> Option<f64> {
    if !point.is_finite() {
        return None;
    }
    let ray = (point - center).try_normalize()?;
    let axis = axis.try_normalize()?;
    let angle = ray.dot(perp(axis)).atan2(ray.dot(axis));
    angle.is_finite().then(|| normalize_0_2pi(angle))
}

fn angle_input(input: &ToolInput, center: DVec2, axis: DVec2) -> Option<f64> {
    match input {
        ToolInput::Point(p) => point_angle(center, axis, *p),
        ToolInput::Value(v) if v.is_finite() => Some(v.rem_euclid(360.0).to_radians()),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum Step {
    #[default]
    First,
    Center,
    AxisEnd {
        first: DVec2,
        centered: bool,
    },
    OtherAxis {
        center: DVec2,
        axis: DVec2,
        rotation: bool,
    },
    ArcStart(Axes),
    ArcEnd {
        axes: Axes,
        start: f64,
        included: bool,
    },
}

/// ELLIPSE: axis endpoints or center/axis, other radius or rotation, optionally arc angles.
#[derive(Default)]
pub struct EllipseTool {
    step: Step,
    arc: bool,
}

impl EllipseTool {
    fn set_axes(&mut self, axes: Axes, cx: &mut ToolCx<'_>) -> ToolFlow {
        let Some(ellipse) = axes.full() else {
            cx.error(core(cx.lang()).value_must_be_positive);
            return ToolFlow::Continue;
        };
        if self.arc {
            self.step = Step::ArcStart(axes);
            ToolFlow::Continue
        } else {
            commit(cx, "ELLIPSE", EntityKind::Ellipse(ellipse));
            ToolFlow::Done
        }
    }
}

impl Tool for EllipseTool {
    fn name(&self) -> &'static str {
        "ELLIPSE"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        match self.step {
            Step::First if self.arc => s.el_arc_axis,
            Step::First => s.el_axis,
            Step::Center => s.el_center,
            Step::AxisEnd { centered: true, .. } => s.el_axis_end,
            Step::AxisEnd { .. } => s.el_axis_other,
            Step::OtherAxis { rotation: true, .. } => s.el_rotation,
            Step::OtherAxis { .. } => s.el_other,
            Step::ArcStart(_) => s.el_start_angle,
            Step::ArcEnd { included: true, .. } => s.el_included,
            Step::ArcEnd { .. } => s.el_end_angle,
        }
        .into()
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        let mut out = Vec::new();
        if matches!(self.step, Step::First | Step::Center) && !self.arc {
            out.push(kw("Arc", "A", s.kw_arc));
        }
        match self.step {
            Step::First => out.push(kw("Center", "C", s.kw_ell_center)),
            Step::OtherAxis {
                rotation: false, ..
            } => {
                out.push(kw("Rotation", "R", s.kw_rotation));
            }
            Step::ArcEnd {
                included: false, ..
            } => {
                out.push(kw("Included", "I", s.kw_included));
            }
            _ => {}
        }
        out
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::First | Step::Center | Step::AxisEnd { .. } => Accept::POINT,
            _ => Accept::POINT_OR_VALUE,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::First | Step::Center => None,
            Step::AxisEnd { first, .. } => Some(first),
            Step::OtherAxis { center, .. } => Some(center),
            Step::ArcStart(axes) | Step::ArcEnd { axes, .. } => Some(axes.center),
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = strings_of(cx.lang());
        match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter if matches!(self.step, Step::First | Step::Center) => {
                return ToolFlow::Cancel;
            }
            ToolInput::Point(p) if !p.is_finite() || !p.length_squared().is_finite() => {
                cx.error(core(cx.lang()).point_expected);
                return ToolFlow::Continue;
            }
            _ => {}
        }
        match (self.step, input) {
            (Step::First | Step::Center, ToolInput::Keyword("Arc")) => self.arc = true,
            (Step::First, ToolInput::Keyword("Center")) => self.step = Step::Center,
            (Step::First | Step::Center, ToolInput::Point(p)) => {
                self.step = Step::AxisEnd {
                    first: p,
                    centered: matches!(self.step, Step::Center),
                };
            }
            (Step::AxisEnd { first, centered }, ToolInput::Point(p)) => {
                let (center, axis) = if centered {
                    (first, p - first)
                } else {
                    (first * 0.5 + p * 0.5, (p - first) * 0.5)
                };
                if ellipse_from_axes(center, axis, axis.length(), 0.0, TAU).is_some() {
                    self.step = Step::OtherAxis {
                        center,
                        axis,
                        rotation: false,
                    };
                } else {
                    cx.error(s.zero_length);
                }
            }
            (Step::OtherAxis { center, axis, .. }, ToolInput::Keyword("Rotation")) => {
                self.step = Step::OtherAxis {
                    center,
                    axis,
                    rotation: true,
                };
            }
            (
                Step::OtherAxis {
                    center,
                    axis,
                    rotation,
                },
                input @ (ToolInput::Point(_) | ToolInput::Value(_)),
            ) => {
                let other = if rotation {
                    let degrees = match input {
                        ToolInput::Value(v) => v,
                        ToolInput::Point(p) => point_angle(center, axis, p)
                            .map(f64::to_degrees)
                            .unwrap_or(f64::NAN),
                        _ => unreachable!(),
                    };
                    if !degrees.is_finite() || !(0.0..=89.4).contains(&degrees) {
                        cx.error(s.el_bad_rotation);
                        return ToolFlow::Continue;
                    }
                    axis.length() * degrees.to_radians().cos()
                } else {
                    match input {
                        ToolInput::Value(v) => v,
                        ToolInput::Point(p) => p.distance(center),
                        _ => unreachable!(),
                    }
                };
                return self.set_axes(
                    Axes {
                        center,
                        first: axis,
                        other,
                    },
                    cx,
                );
            }
            (Step::ArcStart(axes), input @ (ToolInput::Point(_) | ToolInput::Value(_))) => {
                if let Some(start) = angle_input(&input, axes.center, axes.first) {
                    self.step = Step::ArcEnd {
                        axes,
                        start,
                        included: false,
                    };
                } else {
                    cx.error(s.nonzero_angle);
                }
            }
            (Step::ArcEnd { axes, start, .. }, ToolInput::Keyword("Included")) => {
                self.step = Step::ArcEnd {
                    axes,
                    start,
                    included: true,
                };
            }
            (
                Step::ArcEnd {
                    axes,
                    start,
                    included,
                },
                input @ (ToolInput::Point(_) | ToolInput::Value(_)),
            ) => {
                let sweep = match input {
                    ToolInput::Value(v) if included => Some(v.to_radians()),
                    _ => angle_input(&input, axes.center, axes.first)
                        .map(|end| normalize_0_2pi(end - start)),
                };
                if let Some(ellipse) = sweep.and_then(|sweep| axes.arc(start, sweep)) {
                    commit(cx, "ELLIPSE", EntityKind::Ellipse(ellipse));
                    return ToolFlow::Done;
                }
                cx.error(s.nonzero_angle);
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let cursor = cx
            .cursor()
            .filter(|p| p.is_finite() && p.length_squared().is_finite());
        let ellipse = match self.step {
            Step::AxisEnd { first, .. } => {
                out.points.push(first);
                if let Some(p) = cursor
                    && p != first
                    && (p - first).length_squared().is_finite()
                {
                    out.curves.push(Curve2::Line(Line2::new(first, p)));
                }
                None
            }
            Step::OtherAxis {
                center,
                axis,
                rotation,
            } => {
                out.points.push(center);
                let other = cursor.and_then(|p| {
                    if rotation {
                        let angle = point_angle(center, axis, p)?;
                        (angle <= 89.4_f64.to_radians()).then(|| axis.length() * angle.cos())
                    } else {
                        Some(p.distance(center))
                    }
                });
                other.and_then(|other| ellipse_from_axes(center, axis, other, 0.0, TAU))
            }
            Step::ArcStart(axes) => {
                if let Some(angle) = cursor.and_then(|p| point_angle(axes.center, axes.first, p))
                    && let Some(arc) = axes.arc(angle, FRAC_PI_2)
                {
                    out.points.push(arc.at_param(arc.start));
                }
                axes.full()
            }
            Step::ArcEnd { axes, start, .. } => {
                if let Some(arc) = axes.arc(start, FRAC_PI_2) {
                    out.points.push(arc.at_param(arc.start));
                }
                cursor
                    .and_then(|p| point_angle(axes.center, axes.first, p))
                    .and_then(|end| axes.arc(start, normalize_0_2pi(end - start)))
            }
            _ => None,
        };
        if let Some(ellipse) = ellipse {
            out.curves.push(Curve2::Ellipse(ellipse));
        }
        out.rubber_band = self.base_point().is_some() && cursor.is_some();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use wcad_geom2d::Curve;

    fn ellipse(h: &Harness) -> EllipseArc2 {
        let EntityKind::Ellipse(ellipse) = h.of_type("ELLIPSE")[0].1 else {
            panic!("expected ellipse");
        };
        ellipse
    }

    #[test]
    fn axes_and_swapped_axes_preserve_parametric_points() {
        let center = DVec2::new(3.0, -2.0);
        let first = DVec2::new(3.0, 4.0);
        for other in [2.0, 5.0, 10.0] {
            for (start, end) in [(0.0, TAU), (0.3, 2.1), (5.2, 0.4)] {
                let e = ellipse_from_axes(center, first, other, start, end).unwrap();
                assert!(e.ratio > 0.0 && e.ratio <= 1.0);
                assert!((e.major.length() - other.max(5.0)).abs() < 1e-12);
                let other_axis = perp(first / 5.0) * other;
                let sweep = if end == TAU {
                    TAU
                } else {
                    normalize_0_2pi(end - start)
                };
                for i in 0..=8 {
                    let fraction = i as f64 / 8.0;
                    let t = start + sweep * fraction;
                    let expected = center + first * t.cos() + other_axis * t.sin();
                    assert!(
                        e.at_param(e.start + e.sweep() * fraction)
                            .distance(expected)
                            < 1e-11
                    );
                }
            }
        }
    }

    #[test]
    fn invalid_axes_and_angles_are_rejected() {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-300, f64::MAX] {
            assert!(ellipse_from_axes(DVec2::ZERO, DVec2::X, bad, 0.0, TAU).is_none());
        }
        for bad in [DVec2::ZERO, DVec2::splat(f64::NAN), DVec2::splat(f64::MAX)] {
            assert!(ellipse_from_axes(DVec2::ZERO, bad, 1.0, 0.0, TAU).is_none());
        }
        assert!(ellipse_from_axes(DVec2::splat(f64::INFINITY), DVec2::X, 1.0, 0.0, TAU).is_none());
        for (start, end) in [
            (0.0, 0.0),
            (0.0, 2.0 * TAU),
            (f64::NAN, 1.0),
            (0.0, f64::INFINITY),
        ] {
            assert!(ellipse_from_axes(DVec2::ZERO, DVec2::X, 1.0, start, end).is_none());
        }
    }

    #[test]
    fn polar_angles_and_negative_included_angle() {
        let axes = Axes {
            center: DVec2::ZERO,
            first: DVec2::new(2.0, 0.0),
            other: 4.0,
        };
        let start = 30_f64.to_radians();
        let arc = axes.arc(start, 90_f64.to_radians()).unwrap();
        assert!((arc.start().y.atan2(arc.start().x) - start).abs() < 1e-12);
        assert!((arc.end().y.atan2(arc.end().x) - 120_f64.to_radians()).abs() < 1e-12);
        let clockwise = axes.arc(start + FRAC_PI_2, -FRAC_PI_2).unwrap();
        assert!(clockwise.start().distance(arc.start()) < 1e-12);
        assert!(clockwise.end().distance(arc.end()) < 1e-12);
        assert!(axes.arc(start, 0.0).is_none());
        assert!(axes.arc(start, TAU).is_none());
    }

    #[test]
    fn endpoints_and_center_modes_commit_one_undo_step() {
        let mut h = Harness::new();
        h.cmd("ELLIPSE").cmd("-4,0").cmd("4,0").hover(0.0, 2.0);
        assert_eq!(h.count("ELLIPSE"), 0);
        assert!(matches!(
            h.ed.preview.curves.first(),
            Some(Curve2::Ellipse(_))
        ));
        h.cmd("0,2");
        assert!(!h.ed.has_tool());
        let e = ellipse(&h);
        assert_eq!(e.c, DVec2::ZERO);
        assert_eq!(e.major, DVec2::new(4.0, 0.0));
        assert_eq!(e.ratio, 0.5);
        h.ed.undo();
        assert_eq!(h.count("ELLIPSE"), 0);
        h.ed.redo();
        assert_eq!(h.count("ELLIPSE"), 1);
        h.ed.undo();
        h.cmd("ELLIPSE").cmd("C").cmd("1,2").cmd("4,2").cmd("6");
        let e = ellipse(&h);
        assert_eq!(e.c, DVec2::new(1.0, 2.0));
        assert_eq!(e.major, DVec2::new(0.0, 6.0));
        assert_eq!(e.ratio, 0.5);
    }

    #[test]
    fn rotation_bounds_retry_and_cancel() {
        for angle in [0.0, 60.0, 89.4] {
            let mut h = Harness::new();
            h.cmd("ELLIPSE").cmd("C").cmd("0,0").cmd("5,0").cmd("R");
            h.cmd("-1").cmd("90");
            assert!(h.ed.has_tool());
            assert_eq!(h.count("ELLIPSE"), 0);
            h.ed.feed(ToolInput::Value(angle));
            assert!(!h.ed.has_tool());
            assert!((ellipse(&h).ratio - angle.to_radians().cos()).abs() < 1e-12);
        }
        let mut h = Harness::new();
        h.cmd("ELLIPSE").cmd("0,0").cmd("0,0");
        assert!(!h.ed.tool_accepts().value);
        h.cmd("8,0").cmd("0").cmd("-1");
        assert_eq!(h.count("ELLIPSE"), 0);
        assert!(h.ed.has_tool());
        h.cmd("2");
        assert_eq!(h.count("ELLIPSE"), 1);
        h.cmd("ELLIPSE")
            .cmd("A")
            .cmd("C")
            .cmd("0,0")
            .cmd("2,0")
            .cmd("4")
            .cmd("0");
        h.hover(0.0, 4.0).esc();
        assert_eq!(h.count("ELLIPSE"), 1);
        h.ed.undo();
        assert_eq!(h.count("ELLIPSE"), 0);
    }

    #[test]
    fn arc_angles_points_and_included_after_axis_swap() {
        for included in [false, true] {
            let mut h = Harness::new();
            h.cmd("ELLIPSE")
                .cmd("A")
                .cmd("C")
                .cmd("0,0")
                .cmd("2,0")
                .cmd("4");
            assert_eq!(h.count("ELLIPSE"), 0);
            h.cmd("0,0");
            assert!(h.ed.has_tool());
            h.cmd("0");
            if included {
                h.cmd("I").cmd("0").cmd("360");
            } else {
                h.cmd("0");
            }
            assert_eq!(h.count("ELLIPSE"), 0);
            h.hover(0.0, 4.0);
            assert!(matches!(
                h.ed.preview.curves.first(),
                Some(Curve2::Ellipse(_))
            ));
            if included {
                h.cmd("90");
            } else {
                h.cmd("0,4");
            }
            let e = ellipse(&h);
            assert_eq!(e.ratio, 0.5);
            assert!(!e.is_full());
            assert!(e.start().distance(DVec2::new(2.0, 0.0)) < 1e-12);
            assert!(e.end().distance(DVec2::new(0.0, 4.0)) < 1e-12);
        }
    }

    #[test]
    fn tool_rejects_nonfinite_input_without_framework_filtering() {
        let mut h = Harness::new();
        let ed = &mut h.ed;
        let mut cx = ToolCx {
            doc: &mut ed.doc,
            selection: &mut ed.selection,
            draft: &ed.draft,
            index: &ed.index,
            log: &mut ed.log,
            requests: &mut ed.requests,
            lang: Lang::En,
            cursor: None,
            last_point: None,
            units_per_px: 0.01,
        };
        let mut tool = EllipseTool::default();
        for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
            tool.on_input(ToolInput::Point(DVec2::splat(bad)), &mut cx);
            assert!(matches!(tool.step, Step::First));
        }
        tool.on_input(ToolInput::Keyword("Arc"), &mut cx);
        tool.on_input(ToolInput::Point(DVec2::ZERO), &mut cx);
        tool.on_input(ToolInput::Point(DVec2::new(4.0, 0.0)), &mut cx);
        for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
            tool.on_input(ToolInput::Value(bad), &mut cx);
            assert!(matches!(tool.step, Step::OtherAxis { .. }));
        }
        tool.on_input(ToolInput::Value(1.0), &mut cx);
        for bad in [f64::NAN, f64::INFINITY] {
            tool.on_input(ToolInput::Value(bad), &mut cx);
            assert!(matches!(tool.step, Step::ArcStart(_)));
        }
        tool.on_input(ToolInput::Value(0.0), &mut cx);
        tool.on_input(ToolInput::Keyword("Included"), &mut cx);
        for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
            tool.on_input(ToolInput::Value(bad), &mut cx);
            assert!(matches!(tool.step, Step::ArcEnd { .. }));
        }
        assert_eq!(tool.on_input(ToolInput::Escape, &mut cx), ToolFlow::Cancel);
        assert!(cx.drawing().entities.is_empty());
        assert!(cx.undo().is_none());
    }
}
