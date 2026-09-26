//! Regular polygons from a center and radius, or from an oriented edge.

use std::f64::consts::{PI, TAU};

use wcad_doc::EntityKind;
use wcad_geom2d::{Curve2, Polyline2};
use wcad_math::{DVec2, perp};

use super::{angle_of, commit, defaults, finite_pts, kw, positive, set_defaults, strings_of};
use crate::i18n::{Lang, core, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

/// CCW vertices of a regular polygon with 3..=1024 sides.
/// For an inscribed polygon, `radius` reaches a vertex in direction `angle` (radians).
/// Otherwise it reaches the midpoint of the first edge (the polygon's apothem).
pub fn polygon_points(
    center: DVec2,
    sides: u32,
    radius: f64,
    inscribed: bool,
    angle: f64,
) -> Option<Vec<DVec2>> {
    if !(3..=1024).contains(&sides)
        || !center.is_finite()
        || !positive(radius)
        || !angle.is_finite()
    {
        return None;
    }
    let half = PI / f64::from(sides);
    let r = if inscribed {
        radius
    } else {
        radius / half.cos()
    };
    if !positive(r) {
        return None;
    }
    let start = angle.rem_euclid(TAU) - if inscribed { 0.0 } else { half };
    let points: Vec<_> = (0..sides)
        .map(|i| {
            let a = start + TAU * f64::from(i) / f64::from(sides);
            center + DVec2::new(a.cos(), a.sin()) * r
        })
        .collect();
    valid_edges(&points).then_some(points)
}

/// A CCW regular polygon to the left of edge `a -> b`. Its first two vertices are exactly `a, b`.
pub fn polygon_from_edge(a: DVec2, b: DVec2, sides: u32) -> Option<Vec<DVec2>> {
    if !(3..=1024).contains(&sides) || !finite_pts(&[a, b]) {
        return None;
    }
    let edge = b - a;
    let length = edge.length();
    if !positive(length) {
        return None;
    }
    let half = PI / f64::from(sides);
    let center = a + edge * 0.5 + perp(edge) * (0.5 / half.tan());
    let mut points = polygon_points(
        center,
        sides,
        length / (2.0 * half.sin()),
        true,
        angle_of(a - center),
    )?;
    points[0] = a;
    points[1] = b;
    valid_edges(&points).then_some(points)
}

fn valid_edges(points: &[DVec2]) -> bool {
    finite_pts(points)
        && (0..points.len()).all(|i| positive(points[i].distance(points[(i + 1) % points.len()])))
}

#[derive(Clone, Copy)]
enum Step {
    Sides,
    Center,
    Option(DVec2),
    Radius(DVec2),
    EdgeFirst,
    EdgeSecond(DVec2),
}

pub struct PolygonTool {
    step: Step,
    sides: u32,
    inscribed: bool,
}

impl Default for PolygonTool {
    fn default() -> Self {
        let d = defaults();
        Self {
            step: Step::Sides,
            sides: d.polygon_sides,
            inscribed: d.polygon_inscribed,
        }
    }
}

impl PolygonTool {
    fn finish(&self, points: Option<Vec<DVec2>>, cx: &mut ToolCx<'_>) -> ToolFlow {
        if let Some(points) = points {
            commit(
                cx,
                self.name(),
                EntityKind::Polyline(Polyline2::from_points(points, true)),
            );
            ToolFlow::Done
        } else {
            cx.error(strings_of(cx.lang()).zero_length);
            ToolFlow::Continue
        }
    }
}

impl Tool for PolygonTool {
    fn name(&self) -> &'static str {
        "POLYGON"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        match self.step {
            Step::Sides => fmt(s.pg_sides, &[&self.sides]),
            Step::Center => s.pg_center.into(),
            Step::Option(_) => fmt(
                s.pg_option,
                &[&if self.inscribed {
                    s.kw_inscribed
                } else {
                    s.kw_circumscribed
                }],
            ),
            Step::Radius(_) => s.pg_radius.into(),
            Step::EdgeFirst => s.pg_edge1.into(),
            Step::EdgeSecond(_) => s.pg_edge2.into(),
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        match self.step {
            Step::Center => vec![kw("Edge", "E", s.kw_edge)],
            Step::Option(_) => vec![
                kw("Inscribed", "I", s.kw_inscribed),
                kw("Circumscribed", "C", s.kw_circumscribed),
            ],
            _ => Vec::new(),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Sides => Accept::VALUE,
            Step::Option(_) => Accept::NONE,
            Step::Radius(_) => Accept::POINT_OR_VALUE,
            _ => Accept::POINT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        match self.step {
            Step::Radius(c) | Step::EdgeSecond(c) => Some(c),
            _ => None,
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let input = match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter => match self.step {
                Step::Sides => ToolInput::Value(f64::from(self.sides)),
                Step::Option(c) => {
                    self.step = Step::Radius(c);
                    return ToolFlow::Continue;
                }
                _ => return ToolFlow::Continue,
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
            (Step::Sides, ToolInput::Value(v)) => {
                if v.is_finite() && (3.0..=1024.0).contains(&v) && v.fract() == 0.0 {
                    self.sides = v as u32;
                    set_defaults(|d| d.polygon_sides = self.sides);
                    self.step = Step::Center;
                } else {
                    cx.error(strings_of(cx.lang()).pg_bad_sides);
                }
            }
            (Step::Center, ToolInput::Point(c)) => self.step = Step::Option(c),
            (Step::Center, ToolInput::Keyword("Edge")) => self.step = Step::EdgeFirst,
            (Step::Option(c), ToolInput::Keyword(option @ ("Inscribed" | "Circumscribed"))) => {
                self.inscribed = option == "Inscribed";
                set_defaults(|d| d.polygon_inscribed = self.inscribed);
                self.step = Step::Radius(c);
            }
            (Step::Radius(c), ToolInput::Point(p)) => {
                return self.finish(
                    polygon_points(
                        c,
                        self.sides,
                        c.distance(p),
                        self.inscribed,
                        angle_of(p - c),
                    ),
                    cx,
                );
            }
            (Step::Radius(c), ToolInput::Value(v)) => {
                if !positive(v) {
                    cx.error(core(cx.lang()).value_must_be_positive);
                    return ToolFlow::Continue;
                }
                let angle = cx
                    .cursor()
                    .filter(|p| p.is_finite() && *p != c)
                    .map(|p| angle_of(p - c))
                    .unwrap_or(0.0);
                return self.finish(polygon_points(c, self.sides, v, self.inscribed, angle), cx);
            }
            (Step::EdgeFirst, ToolInput::Point(p)) => self.step = Step::EdgeSecond(p),
            (Step::EdgeSecond(a), ToolInput::Point(b)) => {
                return self.finish(polygon_from_edge(a, b, self.sides), cx);
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let Some(p) = cx.cursor().filter(|p| p.is_finite()) else {
            return;
        };
        let points = match self.step {
            Step::Radius(c) => polygon_points(
                c,
                self.sides,
                c.distance(p),
                self.inscribed,
                angle_of(p - c),
            ),
            Step::EdgeSecond(a) => polygon_from_edge(a, p, self.sides),
            _ => None,
        };
        if let Some(points) = points {
            out.curves
                .push(Curve2::Polyline(Polyline2::from_points(points, true)));
            out.rubber_band = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::tools::draw::Defaults;

    #[test]
    fn inscribed_vertices_and_circumscribed_edge_midpoints() {
        let c = DVec2::new(3.0, 4.0);
        for n in [3, 4, 7, 1024] {
            let inscribed = polygon_points(c, n, 5.0, true, 0.3).unwrap();
            assert_eq!(inscribed.len(), n as usize);
            assert!(
                inscribed
                    .iter()
                    .all(|p| (p.distance(c) - 5.0).abs() < 1e-10)
            );
            let circumscribed = polygon_points(c, n, 5.0, false, 0.3).unwrap();
            for i in 0..n as usize {
                let mid = (circumscribed[i] + circumscribed[(i + 1) % n as usize]) * 0.5;
                assert!((mid.distance(c) - 5.0).abs() < 1e-10);
            }
            let mid = (circumscribed[0] + circumscribed[1]) * 0.5;
            assert!((angle_of(mid - c) - 0.3).abs() < 1e-10);
            assert!(Polyline2::from_points(circumscribed, true).signed_area() > 0.0);
        }
    }

    #[test]
    fn edge_endpoints_orientation_and_regular_lengths() {
        let a = DVec2::new(4.0, -2.0);
        let b = DVec2::new(-1.0, 3.0);
        for n in [3, 4, 6, 1024] {
            let pts = polygon_from_edge(a, b, n).unwrap();
            assert_eq!(pts[0], a);
            assert_eq!(pts[1], b);
            let pl = Polyline2::from_points(pts, true);
            assert!(pl.signed_area() > 0.0);
            assert!(
                pl.segments()
                    .all(|s| (s.length() - a.distance(b)).abs() < 1e-8)
            );
        }
    }

    #[test]
    fn invalid_geometry_and_side_limits() {
        for n in [0, 1, 2, 1025, u32::MAX] {
            assert!(polygon_points(DVec2::ZERO, n, 1.0, true, 0.0).is_none());
            assert!(polygon_from_edge(DVec2::ZERO, DVec2::X, n).is_none());
        }
        for r in [-1.0, 0.0, f64::NAN, f64::INFINITY, f64::MAX] {
            assert!(polygon_points(DVec2::ZERO, 4, r, true, 0.0).is_none());
        }
        assert!(polygon_points(DVec2::ZERO, 4, 1.0, false, f64::NAN).is_none());
        assert!(polygon_points(DVec2::splat(f64::INFINITY), 4, 1.0, false, 0.0).is_none());
        assert!(polygon_from_edge(DVec2::X, DVec2::X, 4).is_none());
        assert!(polygon_from_edge(DVec2::ZERO, DVec2::splat(f64::NAN), 4).is_none());
    }

    #[test]
    fn tool_modes_preview_single_transaction_and_cancel() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("POLYGON");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        h.ed.feed(ToolInput::Point(DVec2::ONE));
        for bad in [2.0, 3.5, 1025.0, f64::INFINITY, f64::NAN] {
            h.ed.feed(ToolInput::Value(bad));
            assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
            assert_eq!(h.ed.tool_base_point(), None);
        }
        h.cmd("5").cmd("0,0");
        assert_eq!(h.ed.tool_accepts(), Accept::NONE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.ed.feed(ToolInput::Point(DVec2::X));
        assert_eq!(h.ed.tool_accepts(), Accept::NONE);
        h.cmd("I").hover(3.0, 4.0);
        let preview = h.ed.preview.curves.clone();
        assert_eq!(preview.len(), 1);
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.cmd("3,4");
        assert_eq!(
            h.of_type("LWPOLYLINE")[0].1.as_curve(),
            Some(preview[0].clone())
        );
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.cmd("POLYGON").enter().cmd("E").cmd("0,0").cmd("0,0");
        assert!(h.ed.has_tool());
        h.cmd("4,0");
        assert_eq!(h.count("LWPOLYLINE"), 1);
        h.cmd("POLYGON").cmd("4").cmd("0,0").cmd("C").cmd("2");
        assert_eq!(h.count("LWPOLYLINE"), 2);
        h.cmd("POLYGON")
            .enter()
            .cmd("0,0")
            .enter()
            .hover(2.0, 2.0)
            .esc();
        assert_eq!(h.count("LWPOLYLINE"), 2);
        assert!(h.ed.preview.is_empty());
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 1);
        set_defaults(|d| *d = Defaults::default());
    }
}
