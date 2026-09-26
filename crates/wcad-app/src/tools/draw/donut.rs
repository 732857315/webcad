//! DONUT: diameters followed by any number of centers. Holes use hatch even-odd nesting.

use wcad_doc::{EntityKind, Hatch, HatchLoop, HatchPatternRef};
use wcad_geom2d::{Circle2, Curve2};
use wcad_math::DVec2;

use super::{commit, defaults, num, positive, set_defaults, strings_of};
use crate::i18n::{Lang, fmt};
use crate::tools::{Accept, Preview, Tool, ToolCx, ToolFlow, ToolInput};

/// A solid circular ring, specified by diameters. A zero inside diameter makes a disk.
/// Invalid, non-finite or unrepresentable circles are rejected rather than stored in the drawing.
pub fn donut_hatch(center: DVec2, inside: f64, outside: f64) -> Option<Hatch> {
    if !center.is_finite()
        || !inside.is_finite()
        || inside < 0.0
        || !positive(outside)
        || outside <= inside
    {
        return None;
    }
    let inner = inside * 0.5;
    let outer = outside * 0.5;
    if !positive(outer) || (inside > 0.0 && !positive(inner)) || outer <= inner {
        return None;
    }
    let mut loops = Vec::with_capacity(if inside == 0.0 { 1 } else { 2 });
    for r in [outer, inner] {
        if r == 0.0 {
            continue;
        }
        let lo = center - DVec2::splat(r);
        let hi = center + DVec2::splat(r);
        if !lo.is_finite()
            || !hi.is_finite()
            || lo.x >= center.x
            || lo.y >= center.y
            || hi.x <= center.x
            || hi.y <= center.y
        {
            return None;
        }
        loops.push(HatchLoop {
            curves: vec![Curve2::Circle(Circle2::new(center, r))],
        });
    }
    Some(Hatch {
        loops,
        pattern: HatchPatternRef {
            name: "SOLID".into(),
            angle: 0.0,
            scale: 1.0,
        },
    })
}

#[derive(Clone, Copy)]
enum Step {
    Inside,
    Outside,
    Center,
}

pub struct DonutTool {
    step: Step,
    inside: f64,
    outside: f64,
}

impl Default for DonutTool {
    fn default() -> Self {
        let d = defaults();
        Self {
            step: Step::Inside,
            inside: d.donut_inside,
            outside: d.donut_outside,
        }
    }
}

impl Tool for DonutTool {
    fn name(&self) -> &'static str {
        "DONUT"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        match self.step {
            Step::Inside => fmt(s.do_inside, &[&num(self.inside)]),
            Step::Outside => fmt(s.do_outside, &[&num(self.outside)]),
            Step::Center => s.do_center.into(),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Inside | Step::Outside => Accept::VALUE,
            Step::Center => Accept::POINT,
        }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let input = match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter => match self.step {
                Step::Inside => ToolInput::Value(self.inside),
                Step::Outside => ToolInput::Value(self.outside),
                Step::Center => return ToolFlow::Done,
            },
            input => input,
        };
        match (self.step, input) {
            (Step::Inside, ToolInput::Value(v)) => {
                if v.is_finite() && v >= 0.0 && (v == 0.0 || positive(v * 0.5)) {
                    self.inside = v;
                    self.step = Step::Outside;
                } else {
                    cx.error(strings_of(cx.lang()).do_bad);
                }
            }
            (Step::Outside, ToolInput::Value(v)) => {
                if donut_hatch(DVec2::ZERO, self.inside, v).is_some() {
                    self.outside = v;
                    set_defaults(|d| {
                        d.donut_inside = self.inside;
                        d.donut_outside = v;
                    });
                    self.step = Step::Center;
                } else {
                    cx.error(strings_of(cx.lang()).do_bad);
                }
            }
            (Step::Center, ToolInput::Point(p)) => {
                if let Some(h) = donut_hatch(p, self.inside, self.outside) {
                    commit(cx, self.name(), EntityKind::Hatch(h));
                } else {
                    cx.error(strings_of(cx.lang()).do_bad);
                }
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let Step::Center = self.step
            && let Some(p) = cx.cursor()
            && let Some(h) = donut_hatch(p, self.inside, self.outside)
        {
            out.curves
                .extend(h.loops.into_iter().flat_map(|l| l.curves));
            out.points.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::tools::draw::Defaults;

    #[test]
    fn exact_ring_and_disk_boundaries() {
        let c = DVec2::new(2.0, -3.0);
        let h = donut_hatch(c, 4.0, 10.0).unwrap();
        assert!(h.is_solid());
        assert_eq!(h.loops.len(), 2);
        assert_eq!(
            h.loops[0].curves,
            vec![Curve2::Circle(Circle2::new(c, 5.0))]
        );
        assert_eq!(
            h.loops[1].curves,
            vec![Curve2::Circle(Circle2::new(c, 2.0))]
        );
        let disk = donut_hatch(c, 0.0, 10.0).unwrap();
        assert_eq!(disk.loops.len(), 1);
        assert_eq!(disk.loops[0], h.loops[0]);
    }

    #[test]
    fn invalid_diameters_and_nonfinite_geometry() {
        for (inside, outside) in [
            (-1.0, 2.0),
            (0.0, 0.0),
            (2.0, 2.0),
            (3.0, 2.0),
            (f64::NAN, 2.0),
            (0.0, f64::INFINITY),
            (0.0, f64::NAN),
            (f64::INFINITY, f64::INFINITY),
            (0.0, f64::from_bits(1)),
            (f64::from_bits(1), 1.0),
        ] {
            assert!(donut_hatch(DVec2::ZERO, inside, outside).is_none());
        }
        assert!(donut_hatch(DVec2::splat(f64::NAN), 0.0, 2.0).is_none());
        assert!(donut_hatch(DVec2::splat(f64::MAX), 0.0, f64::MAX).is_none());
        assert!(donut_hatch(DVec2::splat(1e100), 0.0, 1.0).is_none());
    }

    #[test]
    fn diameters_reject_points_then_centers_repeat_and_undo_separately() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("DONUT");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.click(3.0, 4.0);
        h.ed.feed(ToolInput::Point(DVec2::ONE));
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        h.cmd("-1");
        assert_eq!(h.count("HATCH"), 0);
        h.cmd("2").cmd("2");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.cmd("6").hover(5.0, 5.0);
        assert_eq!(h.ed.preview.curves.len(), 2);
        assert_eq!(h.count("HATCH"), 0);
        h.cmd("5,5").cmd("15,5").esc();
        assert_eq!(h.count("HATCH"), 2);
        h.cmd("U");
        assert_eq!(h.count("HATCH"), 1);
        h.cmd("U");
        assert_eq!(h.count("HATCH"), 0);
        h.cmd("DONUT").enter().enter().cmd("0,0").enter();
        assert_eq!(h.count("HATCH"), 1);
        assert!(!h.ed.has_tool());
        set_defaults(|d| *d = Defaults::default());
    }

    #[test]
    fn cancel_at_each_stage_does_not_add_an_undo_step() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.cmd("DONUT").esc();
        h.cmd("DONUT").cmd("0").esc();
        h.cmd("DONUT").cmd("0").cmd("4").hover(2.0, 2.0).esc();
        assert_eq!(h.count("HATCH"), 0);
        assert!(h.ed.doc.undo().is_none());
        assert!(h.ed.preview.is_empty());
        set_defaults(|d| *d = Defaults::default());
    }
}
