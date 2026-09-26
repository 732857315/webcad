use wcad_math::DVec2;

use crate::commands::CommandRegistry;
use crate::testing::Harness;
use crate::tools::ToolInput;

#[test]
fn every_draw_command_starts_and_cancels_without_modifying_the_document() {
    let registry = CommandRegistry::with_all_modules();
    for name in [
        "PLINE", "ARC", "CIRCLE", "RECTANG", "POLYGON", "ELLIPSE", "SPLINE", "POINT", "DONUT",
        "REVCLOUD",
    ] {
        assert!(registry.find(name).is_some(), "missing command {name}");
        let mut h = Harness::new();
        h.cmd(name);
        assert_eq!(h.ed.active_tool_name(), Some(name));
        h.esc();
        assert!(!h.ed.has_tool(), "{name} did not cancel");
        assert!(h.ed.doc.drawing.entities.is_empty());
        assert!(h.ed.preview.is_empty());
    }
}

#[test]
fn non_finite_input_cannot_enter_draw_tool_state() {
    for name in [
        "PLINE", "ARC", "CIRCLE", "RECTANG", "POLYGON", "ELLIPSE", "SPLINE", "POINT", "DONUT",
        "REVCLOUD",
    ] {
        let mut h = Harness::new();
        h.cmd(name);
        let prompt = h.ed.prompt_text();
        for input in [
            ToolInput::Point(DVec2::new(f64::NAN, 0.0)),
            ToolInput::Point(DVec2::new(0.0, f64::INFINITY)),
            ToolInput::Value(f64::INFINITY),
            ToolInput::Hover(DVec2::splat(f64::NEG_INFINITY)),
        ] {
            h.ed.feed(input);
            assert_eq!(h.ed.prompt_text(), prompt, "invalid input changed {name}");
            assert!(h.ed.doc.drawing.entities.is_empty());
            assert_eq!(h.ed.last_point, None);
        }
        h.esc();
    }
}
