//! Fit-point splines. Open fits reuse geom2d's banded solver; closed fits are C1 piecewise cubics.

use wcad_doc::EntityKind;
use wcad_geom2d::{Curve2, Nurbs2, Polyline2};
use wcad_math::{DVec2, cross2};

use super::{commit, kw, strings_of};
use crate::i18n::Lang;
use crate::tools::{Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

const MAX_FIT_POINTS: usize = 1024;
const MAX_PREVIEW_POINTS: usize = 64;

fn distinct(a: DVec2, b: DVec2) -> bool {
    let distance = a.distance(b);
    distance.is_finite()
        && distance > 1e-12 * (1.0 + a.abs().max_element().max(b.abs().max_element()))
}

fn valid_points(points: &[DVec2], minimum: usize) -> bool {
    (minimum..=MAX_FIT_POINTS).contains(&points.len())
        && points
            .iter()
            .all(|p| p.is_finite() && p.length_squared().is_finite())
        && points.windows(2).all(|pair| distinct(pair[0], pair[1]))
        && points
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .sum::<f64>()
            .is_finite()
}

/// Open interpolating spline through 2..=1024 fit points, using the existing chord-length,
/// banded fit. Degree is cubic, reduced to quadratic/linear for three/two points. Consecutive
/// coincident points, non-finite input and numerically singular fits return `None`. Coincident
/// endpoints must use `closed_spline` instead, so the app cannot mistake a non-periodic open fit
/// for a smooth closed spline merely because its endpoints meet.
pub fn open_spline(points: &[DVec2]) -> Option<Nurbs2> {
    if !valid_points(points, 2) || !distinct(points[0], points[points.len() - 1]) {
        return None;
    }
    let spline = Nurbs2::from_fit_points(points, 3).ok()?;
    spline.validate().ok()?;
    spline
        .ctrl
        .iter()
        .all(|p| p.length_squared().is_finite())
        .then_some(spline)
}

/// Smooth closed interpolation through 3..=1024 points. A repeated final copy of the first
/// point is accepted (and removed). Uniform periodic Catmull-Rom tangents define cubic Bezier
/// pieces, assembled into one degree-3 NURBS. At *every* join, including the seam, positions and
/// first derivatives agree; this is C1, not generally C2. Collinear/zero-tangent loops fail.
pub fn closed_spline(points: &[DVec2]) -> Option<Nurbs2> {
    if !valid_points(points, 3) {
        return None;
    }
    let points = if !distinct(points[0], points[points.len() - 1]) {
        &points[..points.len() - 1]
    } else {
        points
    };
    if points.len() < 3 {
        return None;
    }
    let axis = (points[1] - points[0]).try_normalize()?;
    if !points[2..].iter().any(|p| {
        (*p - points[0])
            .try_normalize()
            .is_some_and(|direction| cross2(axis, direction).abs() > 1e-12)
    }) {
        return None;
    }
    let n = points.len();
    let mut tangents = Vec::with_capacity(n);
    for i in 0..n {
        let before = points[(i + n - 1) % n];
        let after = points[(i + 1) % n];
        if !distinct(before, after) {
            return None;
        }
        tangents.push((after - before) * 0.5);
    }
    // Sharing each join control point and tripling internal knots represents exact Bezier
    // pieces. Matching handle vectors on either side supply C1 despite knot multiplicity 3.
    let mut ctrl = Vec::with_capacity(3 * n + 1);
    let mut knots = Vec::with_capacity(3 * n + 5);
    ctrl.push(points[0]);
    knots.extend([0.0; 4]);
    for i in 0..n {
        let next = (i + 1) % n;
        ctrl.extend([
            points[i] + tangents[i] / 3.0,
            points[next] - tangents[next] / 3.0,
            points[next],
        ]);
        let end = (i + 1) as f64;
        knots.extend([end; 3]);
    }
    knots.push(n as f64);
    let spline = Nurbs2 {
        degree: 3,
        ctrl,
        weights: Vec::new(),
        knots,
        fit_points: points.to_vec(),
        closed: true,
    };
    spline.validate().ok()?;
    spline
        .ctrl
        .iter()
        .all(|p| p.length_squared().is_finite())
        .then_some(spline)
}

/// SPLINE: collect fit points, Undo removes only the last pending point, Enter fits an open
/// spline, Close fits a smooth closed spline. No document changes occur before successful fit.
#[derive(Default)]
pub struct SplineTool {
    points: Vec<DVec2>,
}

impl SplineTool {
    fn finish(&self, closed: bool, cx: &mut ToolCx<'_>) -> ToolFlow {
        let spline = if closed {
            closed_spline(&self.points)
        } else {
            open_spline(&self.points)
        };
        let Some(spline) = spline else {
            cx.error(strings_of(cx.lang()).spl_failed);
            return ToolFlow::Continue;
        };
        commit(cx, "SPLINE", EntityKind::Spline(spline));
        ToolFlow::Done
    }

    fn can_add(&self, point: DVec2) -> bool {
        self.points.len() < MAX_FIT_POINTS
            && point.is_finite()
            && point.length_squared().is_finite()
            && self.points.last().is_none_or(|last| distinct(*last, point))
    }
}

impl Tool for SplineTool {
    fn name(&self) -> &'static str {
        "SPLINE"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        if self.points.is_empty() {
            s.spl_first.into()
        } else {
            format!("{} ({}/{MAX_FIT_POINTS})", s.spl_next, self.points.len())
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        let mut out = Vec::new();
        if !self.points.is_empty() {
            out.push(kw("Undo", "U", s.kw_undo));
        }
        if self.points.len() >= 3 {
            out.push(kw("Close", "C", s.kw_close));
        }
        out
    }

    fn base_point(&self) -> Option<DVec2> {
        self.points.last().copied()
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Point(point) => {
                if self.can_add(point) {
                    self.points.push(point);
                } else {
                    cx.error(strings_of(cx.lang()).spl_failed);
                }
            }
            ToolInput::Keyword("Undo") => {
                self.points.pop();
            }
            ToolInput::Keyword("Close") => return self.finish(true, cx),
            ToolInput::Enter if self.points.is_empty() => return ToolFlow::Done,
            ToolInput::Enter => return self.finish(false, cx),
            ToolInput::Escape => return ToolFlow::Cancel,
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        out.points.extend_from_slice(&self.points);
        let mut points = self.points.clone();
        if let Some(point) = cx.cursor().filter(|p| self.can_add(*p)) {
            points.push(point);
        }
        if points.len() < 2 {
            return;
        }
        // Limit solver and tessellation work during pointer motion. Large inputs preview only
        // their fit polygon; Enter/Close still run the real bounded spline construction once.
        if points.len() <= MAX_PREVIEW_POINTS
            && let Some(spline) = open_spline(&points)
        {
            out.curves.push(Curve2::Spline(spline));
        } else {
            out.curves
                .push(Curve2::Polyline(Polyline2::from_points(points, false)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    fn spline(h: &Harness) -> Nurbs2 {
        let EntityKind::Spline(spline) = &h.of_type("SPLINE")[0].1 else {
            panic!("expected spline");
        };
        spline.clone()
    }

    #[test]
    fn open_interpolates_chord_length_fit_points() {
        let points: Vec<_> = (0..12)
            .map(|i| DVec2::new(i as f64, (i as f64 * 0.7).sin() * 3.0))
            .collect();
        let spline = open_spline(&points).unwrap();
        spline.validate().unwrap();
        assert!(!spline.closed);
        assert_eq!(spline.degree, 3);
        assert_eq!(spline.fit_points, points);
        let length: f64 = points.windows(2).map(|p| p[0].distance(p[1])).sum();
        let mut along = 0.0;
        for (i, point) in points.iter().enumerate() {
            if i > 0 {
                along += point.distance(points[i - 1]);
            }
            assert!(spline.point_at(along / length).distance(*point) < 1e-9);
        }
        assert_eq!(open_spline(&points[..2]).unwrap().degree, 1);
        assert_eq!(open_spline(&points[..3]).unwrap().degree, 2);
    }

    #[test]
    fn closed_interpolates_with_matching_positions_and_tangents_at_every_join() {
        let points = [
            DVec2::new(0.0, 0.0),
            DVec2::new(5.0, 1.0),
            DVec2::new(3.0, 4.0),
            DVec2::new(-1.0, 2.0),
        ];
        let spline = closed_spline(&points).unwrap();
        spline.validate().unwrap();
        assert!(spline.closed);
        assert_eq!(spline.degree, 3);
        for (i, point) in points.iter().enumerate() {
            assert!(spline.point_at(i as f64).distance(*point) < 1e-12);
            let incoming = spline.sub_curve(i as f64, (i + 1) as f64).unwrap();
            let next = (i + 1) % points.len();
            let outgoing = spline.sub_curve(next as f64, (next + 1) as f64).unwrap();
            let left = incoming.eval_derivs((i + 1) as f64);
            let right = outgoing.eval_derivs(next as f64);
            assert!(left[0].distance(right[0]) < 1e-12);
            assert!(left[1].distance(right[1]) < 1e-12);
            assert!(left[1].length() > 0.0);
        }
        let (a, b) = spline.domain();
        let start = spline.eval_derivs(a);
        let end = spline.eval_derivs(b);
        assert!(start[0].distance(end[0]) < 1e-12);
        assert!(start[1].distance(end[1]) < 1e-12);
        let mut repeated = points.to_vec();
        repeated.push(points[0]);
        assert_eq!(closed_spline(&repeated).unwrap(), spline);
        assert!(closed_spline(&points[..3]).is_some());
    }

    #[test]
    fn invalid_and_degenerate_fits_are_rejected() {
        for points in [
            vec![],
            vec![DVec2::ZERO],
            vec![DVec2::ZERO; 3],
            vec![DVec2::ZERO, DVec2::X, DVec2::X],
            vec![DVec2::ZERO, DVec2::splat(f64::NAN), DVec2::Y],
            vec![DVec2::ZERO, DVec2::splat(f64::INFINITY), DVec2::Y],
            vec![DVec2::splat(-f64::MAX), DVec2::splat(f64::MAX), DVec2::Y],
            vec![DVec2::ZERO, DVec2::splat(1e-300), DVec2::Y],
        ] {
            assert!(open_spline(&points).is_none());
            assert!(closed_spline(&points).is_none());
        }
        assert!(closed_spline(&[DVec2::ZERO, DVec2::X]).is_none());
        assert!(open_spline(&[DVec2::ZERO, DVec2::X, DVec2::Y, DVec2::ZERO]).is_none());
        assert!(closed_spline(&[DVec2::ZERO, DVec2::X, 2.0 * DVec2::X]).is_none());
        assert!(closed_spline(&[DVec2::ZERO, DVec2::X, DVec2::Y, DVec2::X]).is_none());
        let points: Vec<_> = (0..=MAX_FIT_POINTS)
            .map(|i| DVec2::new(i as f64, (i as f64).sin()))
            .collect();
        assert!(open_spline(&points).is_none());
        assert!(closed_spline(&points).is_none());
    }

    #[test]
    fn pending_undo_finish_document_undo_and_cancel() {
        let mut h = Harness::new();
        h.cmd("SPLINE").cmd("0,0").enter();
        assert!(h.ed.has_tool());
        h.cmd("0,0").cmd("1,2").cmd("2,0").cmd("U").hover(3.0, 1.0);
        assert_eq!(h.count("SPLINE"), 0);
        assert!(matches!(
            h.ed.preview.curves.first(),
            Some(Curve2::Spline(_))
        ));
        h.cmd("3,1").enter();
        assert!(!h.ed.has_tool());
        assert_eq!(
            spline(&h).fit_points,
            vec![DVec2::ZERO, DVec2::new(1.0, 2.0), DVec2::new(3.0, 1.0)]
        );
        h.ed.undo();
        assert_eq!(h.count("SPLINE"), 0);
        h.ed.redo();
        assert_eq!(h.count("SPLINE"), 1);
        h.cmd("SPLINE")
            .cmd("10,0")
            .cmd("U")
            .cmd("10,1")
            .cmd("11,2")
            .esc();
        assert_eq!(h.count("SPLINE"), 1);
        h.ed.undo();
        assert_eq!(h.count("SPLINE"), 0);
    }

    #[test]
    fn close_failure_is_retryable_and_close_commits_one_entity() {
        let mut h = Harness::new();
        h.cmd("SPLINE").cmd("0,0").cmd("1,0").cmd("2,0").cmd("C");
        assert!(h.ed.has_tool());
        assert_eq!(h.count("SPLINE"), 0);
        h.cmd("U").cmd("1,2").cmd("C");
        assert!(!h.ed.has_tool());
        assert_eq!(h.count("SPLINE"), 1);
        let spline = spline(&h);
        let (a, b) = spline.domain();
        assert!(spline.closed);
        assert!(spline.point_at(a).distance(spline.point_at(b)) < 1e-12);
        assert!(spline.eval_derivs(a)[1].distance(spline.eval_derivs(b)[1]) < 1e-12);
        h.ed.undo();
        assert_eq!(h.count("SPLINE"), 0);
    }

    #[test]
    fn preview_budget_and_final_fit_are_independent() {
        let points: Vec<_> = (0..MAX_FIT_POINTS)
            .map(|i| DVec2::new(i as f64, (i as f64 * 0.1).sin()))
            .collect();
        let mut h = Harness::new();
        h.ed.start_tool(Box::new(SplineTool {
            points: points[..MAX_PREVIEW_POINTS].to_vec(),
        }));
        h.hover(64.0, 1.0);
        assert!(matches!(
            h.ed.preview.curves.first(),
            Some(Curve2::Polyline(_))
        ));
        h.ed.feed(ToolInput::Point(DVec2::new(64.0, 1.0)));
        h.enter();
        let fitted = spline(&h);
        assert_eq!(fitted.degree, 3);
        assert_eq!(fitted.fit_points.len(), MAX_PREVIEW_POINTS + 1);
        assert!(!fitted.closed);
        h.ed.start_tool(Box::new(SplineTool {
            points: points.clone(),
        }));
        h.ed.feed(ToolInput::Point(DVec2::new(2048.0, 2.0)));
        assert!(h.ed.has_tool());
        h.enter();
        assert_eq!(h.count("SPLINE"), 2);
        let EntityKind::Spline(last) = &h.of_type("SPLINE")[1].1 else {
            panic!("expected spline");
        };
        assert_eq!(last.fit_points, points);
    }

    #[test]
    fn tool_rejects_nonfinite_points_without_framework_filtering() {
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
        let mut tool = SplineTool::default();
        for bad in [f64::NAN, f64::INFINITY, f64::MAX] {
            assert_eq!(
                tool.on_input(ToolInput::Point(DVec2::splat(bad)), &mut cx),
                ToolFlow::Continue
            );
            assert!(tool.points.is_empty());
        }
        tool.on_input(ToolInput::Point(DVec2::ZERO), &mut cx);
        tool.on_input(ToolInput::Point(DVec2::ZERO), &mut cx);
        assert_eq!(tool.points.len(), 1);
        assert_eq!(tool.on_input(ToolInput::Enter, &mut cx), ToolFlow::Continue);
        tool.on_input(ToolInput::Point(DVec2::X), &mut cx);
        assert_eq!(tool.on_input(ToolInput::Escape, &mut cx), ToolFlow::Cancel);
        assert!(cx.drawing().entities.is_empty());
        assert!(cx.undo().is_none());
    }
}
