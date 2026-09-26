use std::f64::consts::{PI, TAU};

use wcad_doc::EntityKind;
use wcad_geom2d::bulge::{PolySegment, bulge_from_three_points, bulge_to_arc};
use wcad_geom2d::{Curve2, PolyVertex, Polyline2};
use wcad_math::{DVec2, ccw_sweep, cross2};

use super::{commit, deg, dir, kw, positive, strings_of, tangent_bulge};
use crate::i18n::{Lang, core};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

#[derive(Clone, Copy, Default)]
enum Step {
    #[default]
    Next,
    Length,
    Second,
    Through(DVec2),
    Center,
    Around(DVec2),
    Angle,
    Sweep(f64),
    Radius,
    WithRadius(f64),
    Direction,
    Tangent(DVec2),
}

/// A polyline stays in the preview until Enter or Close, then becomes one undoable entity.
#[derive(Default)]
pub struct PlineTool {
    verts: Vec<PolyVertex>,
    arc: bool,
    step: Step,
}

impl PlineTool {
    fn last(&self) -> Option<DVec2> {
        self.verts.last().map(|v| v.p)
    }

    fn tangent(&self) -> DVec2 {
        if self.verts.len() < 2 {
            return DVec2::X;
        }
        let n = self.verts.len();
        let a = self.verts[n - 2];
        PolySegment::from_bulge(a.p, self.verts[n - 1].p, a.bulge)
            .deriv_at(1.0)
            .try_normalize()
            .unwrap_or(DVec2::X)
    }

    fn segment(&self, end: DVec2) -> Option<(DVec2, f64)> {
        let start = self.last()?;
        let chord = end - start;
        let length = chord.length();
        if !end.is_finite() || !positive(length) {
            return None;
        }
        let mut end = end;
        let bulge = match self.step {
            Step::Next if !self.arc => 0.0,
            Step::Next => tangent_bulge(start, end, self.tangent())?,
            Step::Through(mid) => {
                let b = bulge_from_three_points(start, mid, end);
                if b.abs() < 1e-12 {
                    return None;
                }
                b
            }
            Step::Around(center) => {
                let radius = start.distance(center);
                if !positive(radius) {
                    return None;
                }
                end = center + dir(center, end)? * radius;
                let from = (start - center).to_angle();
                let to = (end - center).to_angle();
                let sweep = ccw_sweep(from, to);
                if sweep <= 1e-12 || sweep >= TAU - 1e-12 {
                    return None;
                }
                (sweep / 4.0).tan()
            }
            Step::Sweep(sweep) => (sweep / 4.0).tan(),
            Step::WithRadius(radius) => {
                let ratio = (length * 0.5) / radius.abs();
                if !ratio.is_finite() || ratio > 1.0 {
                    return None;
                }
                let minor = 2.0 * ratio.asin();
                let sweep = if radius < 0.0 { TAU - minor } else { minor };
                let sign = if cross2(self.tangent(), chord) < 0.0 {
                    -1.0
                } else {
                    1.0
                };
                (sign * sweep / 4.0).tan()
            }
            Step::Tangent(tangent) => tangent_bulge(start, end, tangent)?,
            _ => return None,
        };
        if !end.is_finite() || !bulge.is_finite() || !positive(start.distance(end)) {
            return None;
        }
        if bulge != 0.0 && bulge_to_arc(start, end, bulge).is_none() {
            return None;
        }
        Some((end, bulge))
    }

    fn append(&mut self, end: DVec2, cx: &mut ToolCx<'_>) {
        if self.verts.is_empty() {
            if end.is_finite() {
                self.verts.push(PolyVertex::new(end));
            }
            return;
        }
        let Some((end, bulge)) = self.segment(end) else {
            cx.error(strings_of(cx.lang()).arc_invalid);
            return;
        };
        if let Some(last) = self.verts.last_mut() {
            last.bulge = bulge;
        }
        self.verts.push(PolyVertex::new(end));
        self.step = Step::Next;
    }

    fn finish(&mut self, close: bool, cx: &mut ToolCx<'_>) -> ToolFlow {
        if self.verts.len() < 2 {
            return ToolFlow::Cancel;
        }
        let mut verts = self.verts.clone();
        if close {
            let first = verts[0].p;
            if self.last() == Some(first) {
                verts.pop();
            } else {
                let step = self.step;
                self.step = Step::Next;
                let closing = self.segment(first);
                self.step = step;
                let Some((_, bulge)) = closing else {
                    cx.error(strings_of(cx.lang()).arc_invalid);
                    return ToolFlow::Continue;
                };
                if let Some(last) = verts.last_mut() {
                    last.bulge = bulge;
                }
            }
            if verts.len() < 2 {
                return ToolFlow::Cancel;
            }
        }
        commit(
            cx,
            "PLINE",
            EntityKind::Polyline(Polyline2 {
                verts,
                closed: close,
            }),
        );
        ToolFlow::Done
    }
}

impl Tool for PlineTool {
    fn name(&self) -> &'static str {
        "PLINE"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        if self.verts.is_empty() {
            return s.pl_start.into();
        }
        match self.step {
            Step::Length => s.pl_length,
            Step::Second => s.pl_arc_second,
            Step::Center => s.pl_arc_center,
            Step::Angle => s.pl_arc_angle,
            Step::Radius => s.pl_arc_radius,
            Step::Direction => s.pl_arc_direction,
            Step::Next if !self.arc => s.pl_next,
            _ => s.pl_arc_end,
        }
        .into()
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        if self.verts.is_empty() {
            return vec![];
        }
        let mut keywords = vec![kw("Undo", "U", s.kw_undo)];
        if !matches!(self.step, Step::Next) {
            return keywords;
        }
        if self.verts.len() >= 3 || (self.arc && self.verts.len() >= 2) {
            keywords.push(kw("Close", "C", s.kw_close));
        }
        if self.arc {
            keywords.extend([
                kw("Line", "L", s.kw_line),
                kw("Second", "S", s.kw_second),
                kw("Center", "CE", s.kw_center),
                kw("Angle", "A", s.kw_angle),
                kw("Radius", "R", s.kw_radius),
                kw("Direction", "D", s.kw_direction),
            ]);
        } else {
            keywords.extend([kw("Arc", "A", s.kw_arc), kw("Length", "L", s.kw_length)]);
        }
        keywords
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Length | Step::Angle | Step::Radius => Accept::VALUE,
            Step::Direction => Accept::POINT_OR_VALUE,
            _ => Accept::POINT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        self.last()
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = strings_of(cx.lang());
        match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter => return self.finish(false, cx),
            ToolInput::Keyword("Close") => return self.finish(true, cx),
            ToolInput::Keyword("Undo") => {
                self.verts.pop();
                if let Some(last) = self.verts.last_mut() {
                    last.bulge = 0.0;
                }
                self.step = Step::Next;
            }
            ToolInput::Keyword("Arc") => {
                self.arc = true;
                self.step = Step::Next;
            }
            ToolInput::Keyword("Line") => {
                self.arc = false;
                self.step = Step::Next;
            }
            ToolInput::Keyword("Length") => self.step = Step::Length,
            ToolInput::Keyword("Second") => self.step = Step::Second,
            ToolInput::Keyword("Center") => self.step = Step::Center,
            ToolInput::Keyword("Angle") => self.step = Step::Angle,
            ToolInput::Keyword("Radius") => self.step = Step::Radius,
            ToolInput::Keyword("Direction") => self.step = Step::Direction,
            ToolInput::Point(p) => match self.step {
                Step::Second | Step::Center => {
                    if self.last().and_then(|a| dir(a, p)).is_some() {
                        self.step = if matches!(self.step, Step::Second) {
                            Step::Through(p)
                        } else {
                            Step::Around(p)
                        };
                    } else {
                        cx.error(s.zero_length);
                    }
                }
                Step::Direction => {
                    if let Some(tangent) = self.last().and_then(|a| dir(a, p)) {
                        self.step = Step::Tangent(tangent);
                    } else {
                        cx.error(s.zero_length);
                    }
                }
                _ => self.append(p, cx),
            },
            ToolInput::Value(v) => match self.step {
                Step::Length => {
                    if positive(v) {
                        if let Some(start) = self.last() {
                            let end = start + self.tangent() * v;
                            self.step = Step::Next;
                            self.append(end, cx);
                        }
                    } else {
                        cx.error(core(cx.lang()).value_must_be_positive);
                    }
                }
                Step::Angle => match deg(v) {
                    Some(sweep) if sweep.abs() > 1e-12 && sweep.abs() < TAU - 1e-12 => {
                        self.step = Step::Sweep(sweep);
                    }
                    _ => cx.error(s.nonzero_angle),
                },
                Step::Radius => {
                    if positive(v.abs()) {
                        self.step = Step::WithRadius(v);
                    } else {
                        cx.error(core(cx.lang()).value_must_be_positive);
                    }
                }
                Step::Direction => {
                    if let Some(angle) = deg(v) {
                        self.step = Step::Tangent(DVec2::from_angle(angle.rem_euclid(2.0 * PI)));
                    }
                }
                _ => {}
            },
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let mut verts = self.verts.clone();
        if let Some(point) = cx.cursor()
            && let Some((end, bulge)) = self.segment(point)
        {
            if let Some(last) = verts.last_mut() {
                last.bulge = bulge;
            }
            verts.push(PolyVertex::new(end));
        }
        if verts.len() >= 2 {
            out.curves.push(Curve2::Polyline(Polyline2 {
                verts,
                closed: false,
            }));
        } else if let Some(p) = self.last() {
            out.points.push(p);
        }
        out.rubber_band = self.last().is_some();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    fn polyline(h: &Harness) -> Polyline2 {
        let EntityKind::Polyline(p) = &h.of_type("LWPOLYLINE")[0].1 else {
            panic!("polyline expected");
        };
        p.clone()
    }

    #[test]
    fn line_arc_undo_close_is_one_undo_step() {
        let mut h = Harness::new();
        h.cmd("PL").cmd("0,0").cmd("10,0");
        h.cmd("A").cmd("10,10").cmd("U").cmd("10,5");
        h.cmd("L").cmd("0,5").cmd("C");
        let p = polyline(&h);
        assert!(p.closed);
        assert_eq!(p.verts.len(), 4);
        assert!((p.verts[1].bulge - 1.0).abs() < 1e-12);
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.cmd("REDO");
        assert_eq!(h.count("LWPOLYLINE"), 1);
    }

    #[test]
    fn cancel_discards_uncommitted_vertices() {
        let mut h = Harness::new();
        h.cmd("PL").cmd("0,0").cmd("10,0").cmd("10,10");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.esc();
        assert_eq!(h.count("LWPOLYLINE"), 0);
        assert!(h.ed.preview.is_empty());
    }

    #[test]
    fn three_point_and_negative_angle_arcs_keep_direction() {
        let mut h = Harness::new();
        h.cmd("PL").cmd("1,0").cmd("A").cmd("S");
        h.cmd("0,1").cmd("-1,0").cmd("A").cmd("-180");
        h.cmd("-3,0").cmd("");
        let p = polyline(&h);
        assert!((p.verts[0].bulge - 1.0).abs() < 1e-12);
        assert!((p.verts[1].bulge + 1.0).abs() < 1e-12);
    }

    #[test]
    fn center_projects_endpoint_and_invalid_radius_can_retry() {
        let mut h = Harness::new();
        h.cmd("PL").cmd("1,0").cmd("A").cmd("CE");
        h.cmd("0,0").cmd("0,5").cmd("R").cmd("1");
        h.cmd("20,20");
        assert!(h.ed.has_tool());
        h.cmd("-1,0").cmd("");
        let p = polyline(&h);
        assert!(p.verts[1].p.distance(DVec2::Y) < 1e-12);
        assert_eq!(p.verts.len(), 3);
    }

    #[test]
    fn line_length_follows_previous_segment() {
        let mut h = Harness::new();
        h.cmd("PL").cmd("0,0").cmd("0,5").cmd("L").cmd("3").cmd("");
        assert_eq!(polyline(&h).verts[2].p, DVec2::new(0.0, 8.0));
    }
}
