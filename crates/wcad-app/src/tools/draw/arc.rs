//! ARC construction and its input state machine. Persisted arcs always traverse CCW.

use std::f64::consts::TAU;

use wcad_doc::EntityKind;
use wcad_geom2d::{Arc2, Curve2, bulge::bulge_from_three_points, bulge_to_arc, sweep_to_bulge};
use wcad_math::{DVec2, cross2};

use super::{angle_of, commit, deg, finite_pts, kw, positive, strings_of};
use crate::i18n::Lang;
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

const ANGLE_EPS: f64 = 1e-12;

fn valid_sweep(sweep: f64) -> bool {
    sweep.is_finite() && sweep.abs() > ANGLE_EPS && sweep.abs() < TAU - ANGLE_EPS
}

fn checked_arc(a: Arc2) -> Option<Arc2> {
    (a.c.is_finite()
        && positive(a.r)
        && a.start.is_finite()
        && a.end.is_finite()
        && valid_sweep(a.sweep())
        && (a.r * a.sweep()).is_finite()
        && a.start_point().is_finite()
        && a.end_point().is_finite()
        && a.start_point() != a.end_point()
        && (a.c.abs() + DVec2::splat(a.r)).is_finite())
    .then_some(a)
}

/// Arc through three distinct, non-collinear points in the supplied traversal order.
/// A clockwise traversal is stored with reversed endpoints, without changing its point set.
pub fn arc_3p(s: DVec2, m: DVec2, e: DVec2) -> Option<Arc2> {
    if !finite_pts(&[s, m, e]) || s == m || m == e || s == e {
        return None;
    }
    // Work near the origin at unit scale before calling the shared bulge implementation.
    let scale = (m - s).abs().max((e - s).abs()).max_element();
    if !positive(scale) {
        return None;
    }
    let mid = (m - s) / scale;
    let end = (e - s) / scale;
    let b = bulge_from_three_points(DVec2::ZERO, mid, end);
    let a = bulge_to_arc(DVec2::ZERO, end, b)?.to_arc();
    checked_arc(Arc2::new(s + a.c * scale, a.r * scale, a.start, a.end))
}

/// Start, center and signed included angle in radians, strictly between -2*pi and 2*pi.
pub fn arc_sca(s: DVec2, c: DVec2, sweep: f64) -> Option<Arc2> {
    if !finite_pts(&[s, c]) || !valid_sweep(sweep) {
        return None;
    }
    let start = angle_of(s - c);
    let (a, b) = if sweep > 0.0 {
        (start, start + sweep)
    } else {
        (start + sweep, start)
    };
    checked_arc(Arc2::new(c, s.distance(c), a, b))
}

/// Start, center, end direction. The end is projected onto the start's circle, CCW.
pub fn arc_sce(s: DVec2, c: DVec2, e: DVec2) -> Option<Arc2> {
    if !finite_pts(&[s, c, e]) || e == c || !(e - c).is_finite() {
        return None;
    }
    arc_sca(s, c, (angle_of(e - c) - angle_of(s - c)).rem_euclid(TAU))
}

/// Start, center and chord length. A negative length selects the CCW major arc.
pub fn arc_scl(s: DVec2, c: DVec2, chord: f64) -> Option<Arc2> {
    if !finite_pts(&[s, c]) || !positive(chord.abs()) {
        return None;
    }
    let r = s.distance(c);
    let ratio = (chord.abs() * 0.5) / r;
    if !positive(r) || !ratio.is_finite() || ratio > 1.0 {
        return None;
    }
    let minor = 2.0 * ratio.asin();
    arc_sca(s, c, if chord > 0.0 { minor } else { TAU - minor })
}

/// Start, end and signed included angle in radians.
pub fn arc_sea(s: DVec2, e: DVec2, sweep: f64) -> Option<Arc2> {
    if !finite_pts(&[s, e]) || s == e || !valid_sweep(sweep) {
        return None;
    }
    let delta = e - s;
    let scale = delta.abs().max_element();
    if !positive(scale) {
        return None;
    }
    let a = bulge_to_arc(DVec2::ZERO, delta / scale, sweep_to_bulge(sweep))?.to_arc();
    checked_arc(Arc2::new(s + a.c * scale, a.r * scale, a.start, a.end))
}

/// Signed bulge for a segment starting in `tangent`'s direction. Forward-collinear is a line;
/// backward-collinear cannot be joined by a finite arc and returns `None`.
pub fn tangent_bulge(s: DVec2, e: DVec2, tangent: DVec2) -> Option<f64> {
    if !finite_pts(&[s, e, tangent]) {
        return None;
    }
    let chord = (e - s).try_normalize()?;
    let tangent = tangent.try_normalize()?;
    let half_sweep = cross2(tangent, chord).atan2(tangent.dot(chord));
    if half_sweep.abs() >= std::f64::consts::PI - ANGLE_EPS {
        return None;
    }
    let bulge = (half_sweep * 0.5).tan();
    bulge.is_finite().then_some(bulge)
}

/// Start, end and initial tangent direction in radians from +X.
pub fn arc_sed(s: DVec2, e: DVec2, direction: f64) -> Option<Arc2> {
    if !direction.is_finite() {
        return None;
    }
    let tangent = DVec2::new(direction.cos(), direction.sin());
    let b = tangent_bulge(s, e, tangent)?;
    arc_sea(s, e, 4.0 * b.atan())
}

/// Start, end and radius. Positive radius selects the CCW minor arc, negative the major arc.
pub fn arc_ser(s: DVec2, e: DVec2, radius: f64) -> Option<Arc2> {
    if !finite_pts(&[s, e]) || !positive(radius.abs()) {
        return None;
    }
    let ratio = (s.distance(e) * 0.5) / radius.abs();
    if !ratio.is_finite() || ratio > 1.0 {
        return None;
    }
    let minor = 2.0 * ratio.asin();
    arc_sea(s, e, if radius > 0.0 { minor } else { TAU - minor })
}

#[derive(Clone, Copy, Debug, Default)]
enum Step {
    #[default]
    Start,
    Second {
        s: DVec2,
    },
    ThreeEnd {
        s: DVec2,
        m: DVec2,
    },
    Center {
        s: DVec2,
    },
    CenterEnd {
        s: DVec2,
        c: DVec2,
    },
    CenterAngle {
        s: DVec2,
        c: DVec2,
    },
    CenterChord {
        s: DVec2,
        c: DVec2,
    },
    End {
        s: DVec2,
    },
    EndOptions {
        s: DVec2,
        e: DVec2,
    },
    EndAngle {
        s: DVec2,
        e: DVec2,
    },
    EndDirection {
        s: DVec2,
        e: DVec2,
    },
    EndRadius {
        s: DVec2,
        e: DVec2,
    },
}

/// ARC: three points, start/center/end-angle-chord, and start/end/angle-direction-radius.
#[derive(Default)]
pub struct ArcTool {
    step: Step,
}

impl ArcTool {
    fn point_arc(&self, p: DVec2) -> Option<Arc2> {
        if !p.is_finite() {
            return None;
        }
        match self.step {
            Step::ThreeEnd { s, m } => arc_3p(s, m, p),
            Step::CenterEnd { s, c } | Step::CenterAngle { s, c } => arc_sce(s, c, p),
            Step::CenterChord { s, c } => arc_scl(s, c, s.distance(p)),
            Step::EndOptions { s, e }
                if (s.distance(p) - e.distance(p)).abs() <= s.distance(p) * 1e-10 =>
            {
                arc_sce(s, p, e)
            }
            Step::EndAngle { s, e } if p != s => arc_sea(s, e, angle_of(p - s)),
            Step::EndDirection { s, e } if p != s => arc_sed(s, e, angle_of(p - s)),
            Step::EndRadius { s, e } => arc_ser(s, e, s.distance(p)),
            _ => None,
        }
    }

    fn make(&self, arc: Option<Arc2>, cx: &mut ToolCx<'_>) -> ToolFlow {
        match arc {
            Some(a) => {
                commit(cx, "ARC", EntityKind::Arc(a));
                ToolFlow::Done
            }
            None => {
                cx.error(strings_of(cx.lang()).arc_invalid);
                ToolFlow::Continue
            }
        }
    }
}

impl Tool for ArcTool {
    fn name(&self) -> &'static str {
        "ARC"
    }

    fn prompt(&self, lang: Lang) -> String {
        let text = strings_of(lang);
        match self.step {
            Step::Start => text.arc_start,
            Step::Second { .. } => text.arc_second,
            Step::ThreeEnd { .. } | Step::CenterEnd { .. } | Step::End { .. } => text.arc_end,
            Step::Center { .. } | Step::EndOptions { .. } => text.arc_center,
            Step::CenterAngle { .. } | Step::EndAngle { .. } => text.arc_angle,
            Step::CenterChord { .. } => text.arc_chord,
            Step::EndDirection { .. } => text.arc_direction,
            Step::EndRadius { .. } => text.arc_radius,
        }
        .into()
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let text = strings_of(lang);
        match self.step {
            Step::Second { .. } => vec![
                kw("Center", "C", text.kw_center),
                kw("End", "E", text.kw_end),
            ],
            Step::CenterEnd { .. } => vec![
                kw("Angle", "A", text.kw_angle),
                kw("Length", "L", text.kw_chord),
            ],
            Step::EndOptions { .. } => vec![
                kw("Angle", "A", text.kw_angle),
                kw("Direction", "D", text.kw_direction),
                kw("Radius", "R", text.kw_radius),
            ],
            _ => vec![],
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::CenterAngle { .. }
            | Step::CenterChord { .. }
            | Step::EndAngle { .. }
            | Step::EndDirection { .. }
            | Step::EndRadius { .. } => Accept::POINT_OR_VALUE,
            _ => Accept::POINT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::Start => None,
            Step::ThreeEnd { m, .. } => Some(m),
            Step::CenterEnd { c, .. } | Step::CenterAngle { c, .. } => Some(c),
            Step::EndOptions { e, .. } => Some(e),
            Step::Second { s }
            | Step::Center { s }
            | Step::CenterChord { s, .. }
            | Step::End { s }
            | Step::EndAngle { s, .. }
            | Step::EndDirection { s, .. }
            | Step::EndRadius { s, .. } => Some(s),
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match (self.step, input) {
            (_, ToolInput::Escape) | (Step::Start, ToolInput::Enter) => return ToolFlow::Cancel,
            (_, ToolInput::Point(p)) if !p.is_finite() => return self.make(None, cx),
            (Step::Start, ToolInput::Point(s)) => self.step = Step::Second { s },
            (Step::Second { s }, ToolInput::Point(m)) if positive(s.distance(m)) => {
                self.step = Step::ThreeEnd { s, m };
            }
            (Step::Center { s }, ToolInput::Point(c)) if positive(s.distance(c)) => {
                self.step = Step::CenterEnd { s, c };
            }
            (Step::End { s }, ToolInput::Point(e)) if positive(s.distance(e)) => {
                self.step = Step::EndOptions { s, e };
            }
            (_, ToolInput::Point(p)) => return self.make(self.point_arc(p), cx),
            (Step::Second { s }, ToolInput::Keyword("Center")) => self.step = Step::Center { s },
            (Step::Second { s }, ToolInput::Keyword("End")) => self.step = Step::End { s },
            (Step::CenterEnd { s, c }, ToolInput::Keyword("Angle")) => {
                self.step = Step::CenterAngle { s, c };
            }
            (Step::CenterEnd { s, c }, ToolInput::Keyword("Length")) => {
                self.step = Step::CenterChord { s, c };
            }
            (Step::EndOptions { s, e }, ToolInput::Keyword("Angle")) => {
                self.step = Step::EndAngle { s, e };
            }
            (Step::EndOptions { s, e }, ToolInput::Keyword("Direction")) => {
                self.step = Step::EndDirection { s, e };
            }
            (Step::EndOptions { s, e }, ToolInput::Keyword("Radius")) => {
                self.step = Step::EndRadius { s, e };
            }
            (Step::CenterAngle { s, c }, ToolInput::Value(v)) => {
                return self.make(deg(v).and_then(|a| arc_sca(s, c, a)), cx);
            }
            (Step::CenterChord { s, c }, ToolInput::Value(v)) => {
                return self.make(arc_scl(s, c, v), cx);
            }
            (Step::EndAngle { s, e }, ToolInput::Value(v)) => {
                return self.make(deg(v).and_then(|a| arc_sea(s, e, a)), cx);
            }
            (Step::EndDirection { s, e }, ToolInput::Value(v)) => {
                return self.make(deg(v).and_then(|a| arc_sed(s, e, a)), cx);
            }
            (Step::EndRadius { s, e }, ToolInput::Value(v)) => {
                return self.make(arc_ser(s, e, v), cx);
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
        if let Some(a) = self.point_arc(p) {
            out.curves.push(Curve2::Arc(a));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use std::f64::consts::{FRAC_PI_2, PI};

    fn near(a: DVec2, b: DVec2) {
        assert!(a.distance(b) < 1e-9, "{a:?} != {b:?}");
    }

    fn arc(h: &Harness) -> Arc2 {
        let EntityKind::Arc(a) = h.of_type("ARC")[0].1 else {
            panic!("expected arc");
        };
        a
    }

    #[test]
    fn three_points_preserve_both_traversal_loci() {
        let a = arc_3p(DVec2::X, DVec2::Y, -DVec2::X).unwrap();
        let b = arc_3p(-DVec2::X, DVec2::Y, DVec2::X).unwrap();
        near(a.c, DVec2::ZERO);
        near(a.mid_point(), DVec2::Y);
        near(b.mid_point(), DVec2::Y);
        assert!((a.sweep() - PI).abs() < 1e-12);
        let origin = DVec2::new(1e9, -1e9);
        let shifted = arc_3p(origin + DVec2::X, origin + DVec2::Y, origin - DVec2::X).unwrap();
        near(shifted.c, origin);
    }

    #[test]
    fn three_points_reject_collinear_orders_and_duplicates() {
        let p = [DVec2::ZERO, DVec2::X, DVec2::X * 2.0];
        for [i, j, k] in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            assert!(arc_3p(p[i], p[j], p[k]).is_none());
        }
        assert!(arc_3p(p[0], p[0], p[1]).is_none());
        assert!(arc_3p(p[0], p[1], p[0]).is_none());
        assert!(arc_3p(p[0], p[1], p[1]).is_none());
    }

    #[test]
    fn center_angle_and_end_projection() {
        let a = arc_sca(DVec2::X, DVec2::ZERO, -FRAC_PI_2).unwrap();
        near(a.start_point(), -DVec2::Y);
        near(a.end_point(), DVec2::X);
        assert!((a.sweep() - FRAC_PI_2).abs() < 1e-12);
        let a = arc_sce(DVec2::X, DVec2::ZERO, DVec2::Y * 3.0).unwrap();
        near(a.end_point(), DVec2::Y);
        assert!(arc_sce(DVec2::X, DVec2::ZERO, DVec2::ZERO).is_none());
        assert!(arc_sce(DVec2::X, DVec2::ZERO, DVec2::X * 2.0).is_none());
    }

    #[test]
    fn chord_and_radius_minor_major_and_diameter() {
        for sign in [-1.0, 1.0] {
            let a = arc_scl(DVec2::X, DVec2::ZERO, sign * 2.0f64.sqrt()).unwrap();
            let expected = if sign > 0.0 {
                FRAC_PI_2
            } else {
                3.0 * FRAC_PI_2
            };
            assert!((a.sweep() - expected).abs() < 1e-12);
            let a = arc_ser(DVec2::X, DVec2::Y, sign).unwrap();
            assert!((a.sweep() - expected).abs() < 1e-12);
            near(a.start_point(), DVec2::X);
            near(a.end_point(), DVec2::Y);
        }
        assert!(arc_scl(DVec2::X, DVec2::ZERO, 2.1).is_none());
        assert!(arc_ser(DVec2::X, -DVec2::X, 0.99).is_none());
        assert!((arc_ser(DVec2::X, -DVec2::X, 1.0).unwrap().sweep() - PI).abs() < 1e-12);
    }

    #[test]
    fn endpoint_signed_angles_keep_endpoints() {
        for sweep in [FRAC_PI_2, -FRAC_PI_2, 3.0 * FRAC_PI_2, -3.0 * FRAC_PI_2] {
            let a = arc_sea(DVec2::X, DVec2::Y, sweep).unwrap();
            let (s, e) = if sweep > 0.0 {
                (DVec2::X, DVec2::Y)
            } else {
                (DVec2::Y, DVec2::X)
            };
            near(a.start_point(), s);
            near(a.end_point(), e);
            assert!((a.sweep() - sweep.abs()).abs() < 1e-12);
        }
    }

    #[test]
    fn tangent_bulge_retains_direction_including_straight() {
        for (e, tangent) in [(DVec2::Y, DVec2::Y), (-DVec2::Y, -DVec2::Y)] {
            let b = tangent_bulge(DVec2::X, e, tangent).unwrap();
            assert_eq!(b.is_sign_positive(), e.y > 0.0);
            let a = bulge_to_arc(DVec2::X, e, b).unwrap();
            near(a.c, DVec2::ZERO);
            near(a.point_at(1.0), e);
            let a = arc_sed(DVec2::X, e, angle_of(tangent)).unwrap();
            near(a.c, DVec2::ZERO);
        }
        assert_eq!(tangent_bulge(DVec2::ZERO, DVec2::X, DVec2::X), Some(0.0));
        assert!(tangent_bulge(DVec2::ZERO, DVec2::X, -DVec2::X).is_none());
        assert!(tangent_bulge(DVec2::ZERO, DVec2::X, DVec2::ZERO).is_none());
        assert!(arc_sed(DVec2::ZERO, DVec2::X, 0.0).is_none());
    }

    #[test]
    fn helpers_reject_nonfinite_zero_and_full_turns() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, TAU, -TAU] {
            assert!(arc_sca(DVec2::X, DVec2::ZERO, v).is_none());
            assert!(arc_sea(DVec2::X, DVec2::Y, v).is_none());
        }
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let p = DVec2::new(v, 0.0);
            assert!(arc_3p(p, DVec2::X, DVec2::Y).is_none());
            assert!(arc_sce(DVec2::X, DVec2::ZERO, p).is_none());
            assert!(arc_scl(DVec2::X, DVec2::ZERO, v).is_none());
            assert!(arc_ser(DVec2::X, DVec2::Y, v).is_none());
            assert!(arc_sed(DVec2::X, DVec2::Y, v).is_none());
            assert!(tangent_bulge(DVec2::X, DVec2::Y, p).is_none());
        }
        assert!(arc_sca(DVec2::ZERO, DVec2::ZERO, 1.0).is_none());
        assert!(arc_sea(DVec2::X, DVec2::X, 1.0).is_none());
    }

    #[test]
    fn tool_three_points_preview_retry_and_one_undo() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("ARC").cmd("1,0").cmd("1,0");
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).arc_second);
        h.cmd("0,1").cmd("-1,2");
        assert_eq!(h.count("ARC"), 0);
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).arc_end);
        h.hover(-1.0, 0.0);
        assert!(matches!(h.ed.preview.curves.as_slice(), [Curve2::Arc(_)]));
        h.cmd("-1,0");
        assert_eq!(h.count("ARC"), 1);
        assert!(!h.ed.has_tool());
        near(arc(&h).mid_point(), DVec2::Y);
        h.ed.undo();
        assert_eq!(h.count("ARC"), 0);
        h.ed.redo();
        assert_eq!(h.count("ARC"), 1);
    }

    #[test]
    fn tool_center_modes_numeric_and_mouse() {
        for tail in [
            vec!["0,2"],
            vec!["A", "90"],
            vec!["A", "0,2"],
            vec!["L", "1.4142135623730951"],
            vec!["L", "1,1"],
        ] {
            let mut h = Harness::new();
            h.cmd("ARC").cmd("1,0").cmd("C").cmd("0,0");
            for input in tail {
                h.cmd(input);
            }
            assert_eq!(h.count("ARC"), 1);
            assert!(!h.ed.has_tool());
        }
        let mut h = Harness::new();
        h.cmd("ARC")
            .cmd("1,0")
            .cmd("C")
            .cmd("0,0")
            .cmd("A")
            .cmd("-90");
        near(arc(&h).start_point(), -DVec2::Y);
    }

    #[test]
    fn tool_endpoint_modes_numeric_and_mouse() {
        for tail in [
            vec!["0,0"],
            vec!["A", "90"],
            vec!["A", "1,1"],
            vec!["D", "90"],
            vec!["D", "1,1"],
            vec!["R", "1"],
            vec!["R", "2,0"],
            vec!["R", "-1"],
            vec!["A", "-90"],
        ] {
            let mut h = Harness::new();
            h.cmd("ARC").cmd("1,0").cmd("E").cmd("0,1");
            for input in tail {
                h.cmd(input);
            }
            assert_eq!(h.count("ARC"), 1);
            assert!(!h.ed.has_tool());
        }
    }

    #[test]
    fn tool_mouse_previews_match_each_final_construction() {
        for (prefix, p) in [
            (vec!["C", "0,0"], DVec2::Y),
            (vec!["C", "0,0", "A"], DVec2::Y),
            (vec!["C", "0,0", "L"], DVec2::ONE),
            (vec!["E", "0,1"], DVec2::ZERO),
            (vec!["E", "0,1", "A"], DVec2::ONE),
            (vec!["E", "0,1", "D"], DVec2::ONE),
            (vec!["E", "0,1", "R"], DVec2::X * 2.0),
        ] {
            let mut h = Harness::new();
            h.ed.draft.osnap_on = false;
            h.cmd("ARC").cmd("1,0");
            for input in prefix {
                h.cmd(input);
            }
            h.hover(p.x, p.y);
            let Curve2::Arc(preview) = h.ed.preview.curves[0] else {
                panic!("arc preview");
            };
            assert_eq!(h.count("ARC"), 0);
            h.click(p.x, p.y);
            assert_eq!(arc(&h), preview);
            assert!(!h.ed.has_tool());
        }
    }

    #[test]
    fn tool_invalid_values_retry_without_advancing() {
        for prefix in [
            vec!["C", "0,0", "A"],
            vec!["C", "0,0", "L"],
            vec!["E", "0,1", "A"],
            vec!["E", "0,1", "D"],
            vec!["E", "0,1", "R"],
        ] {
            let mut h = Harness::new();
            h.cmd("ARC").cmd("1,0");
            for input in prefix {
                h.cmd(input);
            }
            let prompt = h.ed.prompt();
            h.ed.feed(ToolInput::Value(f64::NAN));
            assert_eq!(h.ed.prompt(), prompt);
            assert_eq!(h.count("ARC"), 0);
            h.ed.feed(ToolInput::Point(DVec2::new(f64::INFINITY, 0.0)));
            assert_eq!(h.ed.prompt(), prompt);
            h.esc();
            assert_eq!(h.count("ARC"), 0);
        }
        let mut h = Harness::new();
        h.cmd("ARC")
            .cmd("1,0")
            .cmd("E")
            .cmd("0,1")
            .cmd("R")
            .cmd("0.1");
        assert!(h.ed.has_tool());
        h.cmd("1");
        assert_eq!(h.count("ARC"), 1);
        let mut h = Harness::new();
        h.cmd("ARC").cmd("1,0").cmd("E").cmd("0,1").cmd("2,0");
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).arc_center);
        h.cmd("0,0");
        assert_eq!(h.count("ARC"), 1);
    }

    #[test]
    fn tool_cancellation_never_commits_and_keywords_are_local() {
        for prefix in [
            vec![],
            vec!["1,0"],
            vec!["1,0", "0,1"],
            vec!["1,0", "C"],
            vec!["1,0", "C", "0,0"],
            vec!["1,0", "E"],
            vec!["1,0", "E", "0,1"],
        ] {
            let mut h = Harness::new();
            h.cmd("ARC");
            for input in prefix {
                h.cmd(input);
            }
            h.esc();
            assert_eq!(h.count("ARC"), 0);
            assert!(!h.ed.has_tool());
        }
        let mut h = Harness::new();
        h.cmd("ARC");
        assert!(h.ed.prompt().1.is_empty());
        h.ed.feed(ToolInput::Keyword("Radius"));
        assert_eq!(h.ed.prompt().0, strings_of(Lang::En).arc_start);
    }
}
