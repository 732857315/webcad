//! RECTANG: rotated rectangles, optional fillets/chamfers and explicit dimensions.

use std::f64::consts::FRAC_PI_2;

use wcad_doc::EntityKind;
use wcad_geom2d::{Curve2, PolyVertex, Polyline2, bulge_to_arc, sweep_to_bulge};
use wcad_math::{DVec2, perp};

use super::{commit, defaults, deg, finite_pts, kw, num, positive, set_defaults, strings_of};
use crate::i18n::{Lang, core, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corners {
    Square,
    Fillet(f64),
    /// Setbacks along the rectangle's local X and Y edges.
    Chamfer(f64, f64),
}

impl Corners {
    fn valid(self) -> bool {
        match self {
            Self::Square => true,
            Self::Fillet(r) => r.is_finite() && r >= 0.0,
            Self::Chamfer(x, y) => x.is_finite() && y.is_finite() && x >= 0.0 && y >= 0.0,
        }
    }

    fn fits(self, size: DVec2) -> bool {
        let half = size * (0.5 + 4.0 * f64::EPSILON);
        match self {
            Self::Square => true,
            Self::Fillet(r) => r <= half.min_element(),
            Self::Chamfer(x, y) => x == 0.0 || y == 0.0 || (x <= half.x && y <= half.y),
        }
    }
}

fn axes(rotation: f64) -> (DVec2, DVec2) {
    let (s, c) = rotation.sin_cos();
    let x = DVec2::new(c, s);
    (x, perp(x))
}

/// Rectangle with world-space opposite corners `a`, `b` and axes rotated by `rotation` radians.
/// Zero corner distances make square corners. Oversized corners fall back to square corners;
/// invalid parameters or degenerate/unrepresentable geometry return `None`.
pub fn rect_polyline(a: DVec2, b: DVec2, corners: Corners, rotation: f64) -> Option<Polyline2> {
    if !finite_pts(&[a, b]) || !rotation.is_finite() || !corners.valid() {
        return None;
    }
    let (x, y) = axes(rotation);
    let delta = b - a;
    let dx = delta.dot(x);
    let dy = delta.dot(y);
    let size = DVec2::new(dx.abs(), dy.abs());
    if !positive(size.x) || !positive(size.y) {
        return None;
    }
    let x = x * dx.signum();
    let y = y * dy.signum();
    let (w, h) = (size.x, size.y);
    let corners = if corners.fits(size) {
        corners
    } else {
        Corners::Square
    };
    let local = match corners {
        Corners::Fillet(r) if r > 0.0 => {
            let r = r.min(size.min_element() * 0.5);
            let bulge = sweep_to_bulge(FRAC_PI_2) * dx.signum() * dy.signum();
            vec![
                PolyVertex::new(DVec2::new(r, 0.0)),
                PolyVertex::with_bulge(DVec2::new(w - r, 0.0), bulge),
                PolyVertex::new(DVec2::new(w, r)),
                PolyVertex::with_bulge(DVec2::new(w, h - r), bulge),
                PolyVertex::new(DVec2::new(w - r, h)),
                PolyVertex::with_bulge(DVec2::new(r, h), bulge),
                PolyVertex::new(DVec2::new(0.0, h - r)),
                PolyVertex::with_bulge(DVec2::new(0.0, r), bulge),
            ]
        }
        Corners::Chamfer(cx, cy) if cx > 0.0 && cy > 0.0 => {
            let cx = cx.min(w * 0.5);
            let cy = cy.min(h * 0.5);
            [
                DVec2::new(cx, 0.0),
                DVec2::new(w - cx, 0.0),
                DVec2::new(w, cy),
                DVec2::new(w, h - cy),
                DVec2::new(w - cx, h),
                DVec2::new(cx, h),
                DVec2::new(0.0, h - cy),
                DVec2::new(0.0, cy),
            ]
            .into_iter()
            .map(PolyVertex::new)
            .collect()
        }
        _ => [DVec2::ZERO, DVec2::new(w, 0.0), size, DVec2::new(0.0, h)]
            .into_iter()
            .map(PolyVertex::new)
            .collect(),
    };
    // Rotation can leave roundoff-sized straight sections at the half-side limit.
    // Collapse only these axial sections, preserving the following arc or bevel.
    let epsilon = size * (8.0 * f64::EPSILON);
    let mut verts: Vec<PolyVertex> = Vec::with_capacity(local.len());
    for v in local {
        if let Some(last) = verts.last_mut()
            && (last.p == v.p
                || (last.bulge == 0.0
                    && ((last.p.x == v.p.x && (last.p.y - v.p.y).abs() <= epsilon.y)
                        || (last.p.y == v.p.y && (last.p.x - v.p.x).abs() <= epsilon.x))))
        {
            last.bulge = v.bulge;
        } else {
            verts.push(v);
        }
    }
    if verts.last().map(|v| v.p) == verts.first().map(|v| v.p) {
        verts.pop();
    }
    for v in &mut verts {
        v.p = a + x * v.p.x + y * v.p.y;
    }
    for i in 0..verts.len() {
        let v = verts[i];
        let next = verts[(i + 1) % verts.len()].p;
        if !v.p.is_finite() || !positive(v.p.distance(next)) {
            return None;
        }
        if v.bulge != 0.0 {
            let arc = bulge_to_arc(v.p, next, v.bulge)?;
            if !positive(arc.r) {
                return None;
            }
        }
    }
    Some(Polyline2 {
        verts,
        closed: true,
    })
}

#[derive(Clone, Copy)]
enum Step {
    First,
    Other,
    Fillet,
    ChamferFirst,
    ChamferSecond(f64),
    Rotation,
    Length,
    Width,
    Place,
}

pub struct RectTool {
    step: Step,
    first: Option<DVec2>,
    corners: Corners,
    rotation: f64,
    length: f64,
    width: f64,
}

impl Default for RectTool {
    fn default() -> Self {
        let d = defaults();
        Self {
            step: Step::First,
            first: None,
            corners: d.rect_corners,
            rotation: d.rect_rotation,
            length: d.rect_length,
            width: d.rect_width,
        }
    }
}

impl RectTool {
    fn default_value(&self) -> f64 {
        match self.step {
            Step::Fillet => match self.corners {
                Corners::Fillet(r) => r,
                _ => 0.0,
            },
            Step::ChamferFirst => match self.corners {
                Corners::Chamfer(x, _) => x,
                _ => 0.0,
            },
            Step::ChamferSecond(first) => match self.corners {
                Corners::Chamfer(_, y) => y,
                _ => first,
            },
            Step::Rotation => self.rotation.to_degrees(),
            Step::Length => self.length,
            Step::Width => self.width,
            _ => 0.0,
        }
    }

    fn opposite(&self, cursor: DVec2) -> Option<DVec2> {
        let a = self.first?;
        match self.step {
            Step::Other => Some(cursor),
            Step::Place => {
                let (x, y) = axes(self.rotation);
                let delta = cursor - a;
                let sx = if delta.dot(x) < 0.0 { -1.0 } else { 1.0 };
                let sy = if delta.dot(y) < 0.0 { -1.0 } else { 1.0 };
                Some(a + x * (sx * self.length) + y * (sy * self.width))
            }
            _ => None,
        }
    }

    fn finish(&self, b: DVec2, cx: &mut ToolCx<'_>) -> ToolFlow {
        let Some(a) = self.first else {
            return ToolFlow::Continue;
        };
        let Some(pl) = rect_polyline(a, b, self.corners, self.rotation) else {
            cx.error(strings_of(cx.lang()).zero_length);
            return ToolFlow::Continue;
        };
        let (x, y) = axes(self.rotation);
        let size = DVec2::new((b - a).dot(x).abs(), (b - a).dot(y).abs());
        if !self.corners.fits(size) {
            cx.message(strings_of(cx.lang()).rect_corner_too_big);
        }
        commit(cx, self.name(), EntityKind::Polyline(pl));
        ToolFlow::Done
    }
}

impl Tool for RectTool {
    fn name(&self) -> &'static str {
        "RECTANG"
    }

    fn start(&mut self, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = strings_of(cx.lang());
        let mode = match self.corners {
            Corners::Fillet(r) if r > 0.0 => Some(fmt(s.rect_mode_fillet, &[&num(r)])),
            Corners::Chamfer(x, y) if x > 0.0 && y > 0.0 => {
                Some(fmt(s.rect_mode_chamfer, &[&num(x), &num(y)]))
            }
            _ => None,
        };
        if let Some(mode) = mode {
            cx.message(fmt(s.rect_modes, &[&mode]));
        }
        ToolFlow::Continue
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        let template = match self.step {
            Step::First => return s.rect_first.into(),
            Step::Other | Step::Place => return s.rect_other.into(),
            Step::Fillet => s.rect_fillet,
            Step::ChamferFirst => s.rect_chamfer1,
            Step::ChamferSecond(_) => s.rect_chamfer2,
            Step::Rotation => s.rect_rotation,
            Step::Length => s.rect_length,
            Step::Width => s.rect_width,
        };
        fmt(template, &[&num(self.default_value())])
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        match self.step {
            Step::First => vec![
                kw("Chamfer", "C", s.kw_chamfer),
                kw("Fillet", "F", s.kw_fillet),
            ],
            Step::Other => vec![
                kw("Dimensions", "D", s.kw_dimensions),
                kw("Rotation", "R", s.kw_rotation),
            ],
            _ => Vec::new(),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::First | Step::Other | Step::Place => Accept::POINT,
            _ => Accept::VALUE,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::Other | Step::Place => self.first,
            _ => None,
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let input = match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter => match self.step {
                Step::First => return ToolFlow::Cancel,
                Step::Other | Step::Place => return ToolFlow::Continue,
                _ => ToolInput::Value(self.default_value()),
            },
            input => input,
        };
        if let ToolInput::Point(p) = &input
            && !p.is_finite()
        {
            cx.error(core(cx.lang()).point_expected);
            return ToolFlow::Continue;
        }
        match (self.step, input) {
            (Step::First, ToolInput::Point(p)) => {
                self.first = Some(p);
                self.step = Step::Other;
            }
            (Step::Other | Step::Place, ToolInput::Point(p)) => {
                if let Some(b) = self.opposite(p) {
                    return self.finish(b, cx);
                }
            }
            (Step::First, ToolInput::Keyword("Fillet")) => self.step = Step::Fillet,
            (Step::First, ToolInput::Keyword("Chamfer")) => self.step = Step::ChamferFirst,
            (Step::Other, ToolInput::Keyword("Rotation")) => self.step = Step::Rotation,
            (Step::Other, ToolInput::Keyword("Dimensions")) => self.step = Step::Length,
            (Step::Fillet | Step::ChamferFirst | Step::ChamferSecond(_), ToolInput::Value(v)) => {
                if !v.is_finite() || v < 0.0 {
                    cx.error(fmt(core(cx.lang()).invalid_input, &[&v]));
                    return ToolFlow::Continue;
                }
                match self.step {
                    Step::Fillet => self.corners = Corners::Fillet(v),
                    Step::ChamferFirst => {
                        self.step = Step::ChamferSecond(v);
                        return ToolFlow::Continue;
                    }
                    Step::ChamferSecond(x) => self.corners = Corners::Chamfer(x, v),
                    _ => unreachable!(),
                }
                set_defaults(|d| d.rect_corners = self.corners);
                self.step = Step::First;
            }
            (Step::Rotation, ToolInput::Value(v)) => {
                if let Some(angle) = deg(v) {
                    self.rotation = angle;
                    set_defaults(|d| d.rect_rotation = angle);
                    self.step = Step::Other;
                } else {
                    cx.error(fmt(core(cx.lang()).invalid_input, &[&v]));
                }
            }
            (Step::Length | Step::Width, ToolInput::Value(v)) => {
                if !positive(v) {
                    cx.error(core(cx.lang()).value_must_be_positive);
                    return ToolFlow::Continue;
                }
                match self.step {
                    Step::Length => {
                        self.length = v;
                        set_defaults(|d| d.rect_length = v);
                        self.step = Step::Width;
                    }
                    Step::Width => {
                        self.width = v;
                        set_defaults(|d| d.rect_width = v);
                        self.step = Step::Place;
                    }
                    _ => unreachable!(),
                }
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let Some(a) = self.first
            && let Some(p) = cx.cursor().filter(|p| p.is_finite())
            && let Some(b) = self.opposite(p)
            && let Some(pl) = rect_polyline(a, b, self.corners, self.rotation)
        {
            out.curves.push(Curve2::Polyline(pl));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::tools::draw::Defaults;
    use std::f64::consts::PI;
    use wcad_geom2d::PolySegment;

    #[test]
    fn rotated_rectangles_preserve_corners_lengths_and_area_in_all_quadrants() {
        let a = DVec2::new(3.0, -4.0);
        let (x, y) = axes(0.4);
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                let b = a + x * (10.0 * sx) + y * (6.0 * sy);
                let p = rect_polyline(a, b, Corners::Square, 0.4).unwrap();
                assert!(p.closed);
                assert_eq!(p.verts.len(), 4);
                assert_eq!(p.verts[0].p, a);
                assert!(p.verts[2].p.distance(b) < 1e-12);
                assert!((p.signed_area() - 60.0 * sx * sy).abs() < 1e-10);
                assert!((p.segment(0).unwrap().length() - 10.0).abs() < 1e-12);
                assert!((p.segment(1).unwrap().length() - 6.0).abs() < 1e-12);
            }
        }
    }

    #[test]
    fn fillets_are_tangent_quarter_circles_with_correct_signed_area() {
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                let p = rect_polyline(
                    DVec2::ZERO,
                    DVec2::new(10.0 * sx, 20.0 * sy),
                    Corners::Fillet(2.0),
                    0.0,
                )
                .unwrap();
                assert_eq!(p.verts.len(), 8);
                let arcs: Vec<_> = p
                    .segments()
                    .filter_map(|s| match s {
                        PolySegment::Arc(a) => Some(a),
                        _ => None,
                    })
                    .collect();
                assert_eq!(arcs.len(), 4);
                assert!(
                    arcs.iter().all(|a| (a.r - 2.0).abs() < 1e-12
                        && (a.sweep - sx * sy * FRAC_PI_2).abs() < 1e-12)
                );
                assert!((p.signed_area().abs() - (200.0 - (4.0 - PI) * 4.0)).abs() < 1e-10);
                for i in 0..p.segment_count() {
                    let before = p.segment(i).unwrap().deriv_at(1.0).normalize();
                    let after = p
                        .segment((i + 1) % p.segment_count())
                        .unwrap()
                        .deriv_at(0.0)
                        .normalize();
                    assert!(before.distance(after) < 1e-12);
                }
            }
        }
    }

    #[test]
    fn chamfers_zero_distances_half_side_limits_and_oversize_fallback() {
        let a = DVec2::ZERO;
        let b = DVec2::new(10.0, 6.0);
        let chamfer = rect_polyline(a, b, Corners::Chamfer(2.0, 1.0), 0.0).unwrap();
        assert_eq!(chamfer.verts.len(), 8);
        assert!((chamfer.signed_area() - 56.0).abs() < 1e-12);
        let diamond = rect_polyline(a, b, Corners::Chamfer(5.0, 3.0), 0.0).unwrap();
        assert_eq!(diamond.verts.len(), 4);
        assert!((diamond.signed_area() - 30.0).abs() < 1e-12);
        let circle = rect_polyline(a, DVec2::splat(10.0), Corners::Fillet(5.0), 0.0).unwrap();
        assert_eq!(circle.verts.len(), 4);
        assert!((circle.signed_area() - 25.0 * PI).abs() < 1e-10);
        let square = rect_polyline(a, b, Corners::Square, 0.0).unwrap();
        for corners in [
            Corners::Fillet(0.0),
            Corners::Chamfer(0.0, 2.0),
            Corners::Fillet(3.1),
            Corners::Chamfer(5.1, 1.0),
            Corners::Chamfer(1.0, 3.1),
        ] {
            assert_eq!(rect_polyline(a, b, corners, 0.0).unwrap(), square);
        }
    }

    #[test]
    fn half_side_corner_limits_survive_rotated_projection() {
        let a = DVec2::new(3.0, -4.0);
        for angle in [0.1, 0.3, 0.7, PI / 3.0, FRAC_PI_2, -0.6] {
            let (x, y) = axes(angle);
            let b = a + x * 10.0 + y * 10.0;
            let circle = rect_polyline(a, b, Corners::Fillet(5.0), angle).unwrap();
            assert_eq!(circle.segment_count(), 4);
            assert!(circle.segments().all(|s| matches!(s, PolySegment::Arc(_))));
            assert!((circle.signed_area() - 25.0 * PI).abs() < 1e-10);
            let diamond = rect_polyline(a, b, Corners::Chamfer(5.0, 5.0), angle).unwrap();
            assert_eq!(diamond.segment_count(), 4);
            assert!((diamond.signed_area() - 50.0).abs() < 1e-10);
        }
    }

    #[test]
    fn rejects_invalid_and_unrepresentable_rectangles() {
        for corners in [
            Corners::Fillet(-1.0),
            Corners::Fillet(f64::NAN),
            Corners::Chamfer(1.0, -1.0),
            Corners::Chamfer(f64::INFINITY, 1.0),
        ] {
            assert!(rect_polyline(DVec2::ZERO, DVec2::ONE, corners, 0.0).is_none());
        }
        for b in [
            DVec2::ZERO,
            DVec2::X,
            DVec2::Y,
            DVec2::splat(f64::NAN),
            DVec2::splat(f64::INFINITY),
            DVec2::splat(f64::MAX),
        ] {
            assert!(rect_polyline(DVec2::ZERO, b, Corners::Square, 0.0).is_none());
        }
        assert!(rect_polyline(DVec2::ZERO, DVec2::ONE, Corners::Square, f64::NAN).is_none());
    }

    #[test]
    fn dimensions_rotation_numeric_contract_preview_and_undo() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("RECTANG").cmd("0,0").cmd("R");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.ed.feed(ToolInput::Point(DVec2::X));
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        h.cmd("90").cmd("D").cmd("0");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.cmd("10");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.cmd("6").hover(-2.0, 3.0);
        let preview = h.ed.preview.curves.clone();
        assert_eq!(preview.len(), 1);
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.cmd("-2,3");
        let kind = &h.of_type("LWPOLYLINE")[0].1;
        assert_eq!(kind.as_curve(), Some(preview[0].clone()));
        let EntityKind::Polyline(p) = kind else {
            panic!("expected rectangle")
        };
        assert!((p.signed_area() - 60.0).abs() < 1e-10);
        assert!(p.verts[2].p.distance(DVec2::new(-6.0, 10.0)) < 1e-12);
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        assert!(h.ed.doc.undo().is_none());
        set_defaults(|d| *d = Defaults::default());
    }

    #[test]
    fn options_defaults_oversize_warning_and_cancel_leave_no_partial_entity() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.cmd("RECTANG").cmd("F").cmd("-2");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        h.cmd("2").cmd("0,0").cmd("3,3");
        assert!(
            h.ed.log
                .iter()
                .any(|l| l.text == strings_of(Lang::En).rect_corner_too_big)
        );
        let EntityKind::Polyline(p) = &h.of_type("LWPOLYLINE")[0].1 else {
            panic!("expected rectangle")
        };
        assert_eq!(p.verts.len(), 4);
        assert_eq!(defaults().rect_corners, Corners::Fillet(2.0));
        h.cmd("RECTANG")
            .cmd("C")
            .cmd("1")
            .enter()
            .cmd("0,0")
            .cmd("8,6");
        assert_eq!(defaults().rect_corners, Corners::Chamfer(1.0, 1.0));
        h.cmd("RECTANG").cmd("0,0").cmd("D").enter().enter().esc();
        assert_eq!(h.count("LWPOLYLINE"), 2);
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 1);
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.cmd("RECTANG").cmd("C").cmd("4").esc();
        assert!(h.ed.doc.undo().is_none());
        assert!(h.ed.preview.is_empty());
        set_defaults(|d| *d = Defaults::default());
    }
}
