//! POINT: independent points, one undo step per point, until Enter or Escape.

use wcad_doc::EntityKind;

use super::{commit, strings_of};
use crate::i18n::{Lang, core};
use crate::tools::{Preview, Tool, ToolCx, ToolFlow, ToolInput};

#[derive(Default)]
pub struct PointTool;

impl Tool for PointTool {
    fn name(&self) -> &'static str {
        "POINT"
    }

    fn prompt(&self, lang: Lang) -> String {
        strings_of(lang).pt_point.into()
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Point(p) if p.is_finite() => {
                commit(cx, self.name(), EntityKind::Point { p });
            }
            ToolInput::Point(_) => cx.error(core(cx.lang()).point_expected),
            ToolInput::Enter => return ToolFlow::Done,
            ToolInput::Escape => return ToolFlow::Cancel,
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let Some(p) = cx.cursor().filter(|p| p.is_finite()) {
            out.points.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::tools::Accept;
    use wcad_math::DVec2;

    #[test]
    fn points_are_continuous_independent_and_undoable() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("POINT").hover(2.0, 3.0);
        assert_eq!(h.ed.preview.points, vec![DVec2::new(2.0, 3.0)]);
        assert_eq!(h.ed.tool_base_point(), None);
        h.cmd("2,3").cmd("@4,5");
        assert_eq!(h.ed.active_tool_name(), Some("POINT"));
        assert_eq!(
            h.of_type("POINT")[1].1,
            EntityKind::Point {
                p: DVec2::new(6.0, 8.0)
            }
        );
        h.esc();
        assert_eq!(h.count("POINT"), 2);
        assert!(h.ed.preview.is_empty());
        h.cmd("U");
        assert_eq!(h.count("POINT"), 1);
        h.cmd("U");
        assert_eq!(h.count("POINT"), 0);
        h.cmd("REDO");
        assert_eq!(h.count("POINT"), 1);
    }

    #[test]
    fn invalid_inputs_and_empty_cancel_create_nothing() {
        let mut h = Harness::new();
        h.cmd("POINT");
        assert_eq!(h.ed.tool_accepts(), Accept::POINT);
        h.ed.feed(ToolInput::Point(DVec2::new(f64::NAN, 0.0)));
        h.ed.feed(ToolInput::Point(DVec2::splat(f64::INFINITY)));
        h.ed.feed(ToolInput::Value(4.0));
        h.esc();
        assert_eq!(h.count("POINT"), 0);
        assert!(h.ed.doc.undo().is_none());
        h.cmd("POINT").cmd("0,0").enter();
        assert!(!h.ed.has_tool());
        assert_eq!(h.count("POINT"), 1);
    }
}
