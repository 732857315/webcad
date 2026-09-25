//! Headless test harness for tools and the command line (no window, no GPU). Feature packages
//! can use it for their own tool tests:
//!
//! ```
//! use wcad_app::testing::Harness;
//! let mut h = Harness::new();
//! h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
//! assert_eq!(h.count("LINE"), 1);
//! ```

use std::sync::Arc;

use wcad_doc::{EntityId, EntityKind};
use wcad_math::DVec2;

use crate::commands::CommandRegistry;
use crate::editor::Editor;

pub struct Harness {
    pub ed: Editor,
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

impl Harness {
    /// Editor with every module registered, English messages, 0.01 units per pixel.
    pub fn new() -> Self {
        let mut ed = Editor::new(Arc::new(CommandRegistry::with_all_modules()));
        ed.lang = crate::i18n::Lang::En;
        ed.units_per_px = 0.01;
        Self { ed }
    }

    /// Submit one command-line entry (`""` = Enter).
    pub fn cmd(&mut self, line: &str) -> &mut Self {
        self.ed.submit(line);
        self
    }

    /// Click at a world point.
    pub fn click(&mut self, x: f64, y: f64) -> &mut Self {
        self.ed.click(DVec2::new(x, y), false);
        self
    }

    pub fn shift_click(&mut self, x: f64, y: f64) -> &mut Self {
        self.ed.click(DVec2::new(x, y), true);
        self
    }

    /// Move the pointer to a world point.
    pub fn hover(&mut self, x: f64, y: f64) -> &mut Self {
        self.ed.pointer_move(DVec2::new(x, y));
        self
    }

    pub fn enter(&mut self) -> &mut Self {
        self.ed.enter();
        self
    }

    pub fn esc(&mut self) -> &mut Self {
        self.ed.escape();
        self
    }

    /// Model-space entities of a DXF type name ("LINE", "CIRCLE", …).
    pub fn of_type(&self, type_name: &str) -> Vec<(EntityId, EntityKind)> {
        self.ed
            .doc
            .drawing
            .entities
            .values()
            .filter(|e| e.kind.type_name() == type_name)
            .map(|e| (e.id, e.kind.clone()))
            .collect()
    }

    pub fn count(&self, type_name: &str) -> usize {
        self.of_type(type_name).len()
    }

    /// Last history line.
    pub fn last_message(&self) -> &str {
        self.ed.log.last().map(|l| l.text.as_str()).unwrap_or("")
    }
}

/// Run the whole application UI for `frames` frames in a bare `egui::Context` (no window, no GPU)
/// and return the last frame's output.
pub fn run_app_frames(
    app: &mut crate::WebCadApp,
    ctx: &egui::Context,
    frames: usize,
    size: egui::Vec2,
) -> egui::FullOutput {
    let mut last = None;
    for _ in 0..frames.max(1) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| app.frame_ui(ui));
        out.textures_delta.clear();
        last = Some(out);
    }
    last.unwrap_or_default()
}

/// Like [`run_app_frames`] but with explicit input events for the frame.
pub fn run_app_frame_with(
    app: &mut crate::WebCadApp,
    ctx: &egui::Context,
    size: egui::Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| app.frame_ui(ui));
    out.textures_delta.clear();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::LogKind;
    use wcad_geom2d::{Circle2, Line2};

    fn line(h: &Harness, i: usize) -> Line2 {
        match &h.of_type("LINE")[i].1 {
            EntityKind::Line(l) => *l,
            _ => unreachable!(),
        }
    }

    #[test]
    fn line_continuous_undo_close() {
        let mut h = Harness::new();
        h.cmd("L").cmd("0,0").cmd("10,0").cmd("@0,10").cmd("@-10,0");
        assert_eq!(h.count("LINE"), 3);
        // In-command Undo removes the last segment and continues from its start.
        h.cmd("u");
        assert_eq!(h.count("LINE"), 2);
        assert_eq!(h.ed.active_tool_name(), Some("LINE"));
        h.cmd("5<180"); // absolute polar -> (-5, 0)
        assert_eq!(line(&h, 2).b.round(), DVec2::new(-5.0, 0.0));
        h.cmd("c"); // close back to 0,0
        assert_eq!(h.count("LINE"), 4);
        assert!(!h.ed.has_tool());
        let l = line(&h, 3);
        assert_eq!((l.a.round(), l.b), (DVec2::new(-5.0, 0.0), DVec2::ZERO));
        // Enter repeats LINE.
        h.cmd("");
        assert_eq!(h.ed.active_tool_name(), Some("LINE"));
        h.esc();
        assert!(!h.ed.has_tool());
    }

    #[test]
    fn line_direct_distance_and_ortho() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.ed.draft.ortho = true;
        h.cmd("LINE").click(1.0, 1.0);
        h.hover(5.0, 1.4); // mostly +X; ortho locks the direction
        h.cmd("20");
        assert_eq!(h.count("LINE"), 1);
        let l = line(&h, 0);
        assert!((l.b - DVec2::new(21.0, 1.0)).length() < 1e-9, "{:?}", l.b);
        // Angle override.
        h.cmd("<90").cmd("5");
        let l = line(&h, 1);
        assert!((l.b - DVec2::new(21.0, 6.0)).length() < 1e-9, "{:?}", l.b);
        h.enter();
        assert!(!h.ed.has_tool());
    }

    #[test]
    fn snapping_click_uses_endpoint() {
        let mut h = Harness::new();
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        // aperture = 10 px * 0.01 = 0.1 units
        h.cmd("LINE").click(10.05, 0.03);
        h.click(10.0, 5.0).enter();
        let l = line(&h, 1);
        assert_eq!(l.a, DVec2::new(10.0, 0.0));
    }

    #[test]
    fn circle_radius_and_diameter() {
        let mut h = Harness::new();
        h.cmd("C").cmd("5,5").cmd("2.5");
        h.cmd("CIRCLE").cmd("0,0").cmd("d").cmd("10");
        h.cmd("CIRCLE").cmd("0,0").cmd("-1");
        assert_eq!(h.ed.log.last().map(|l| l.kind), Some(LogKind::Error));
        h.cmd("@3,4");
        let cs: Vec<Circle2> = h
            .of_type("CIRCLE")
            .into_iter()
            .filter_map(|(_, k)| {
                if let EntityKind::Circle(c) = k {
                    Some(c)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(cs.len(), 3);
        assert_eq!(cs[0].r, 2.5);
        assert_eq!(cs[1].r, 5.0);
        assert!((cs[2].r - 5.0).abs() < 1e-12);
    }

    #[test]
    fn move_with_and_without_preselection() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        // Without selection: MOVE prompts for objects; pick the line, Enter, base, second point.
        h.cmd("M");
        assert!(h.ed.tool_accepts().selection);
        h.click(5.0, 0.0);
        assert_eq!(h.ed.selection.len(), 1);
        h.enter();
        h.cmd("0,0").cmd("@5,5");
        assert!(!h.ed.has_tool());
        let l = line(&h, 0);
        assert_eq!((l.a, l.b), (DVec2::new(5.0, 5.0), DVec2::new(15.0, 5.0)));
        // Pre-selection (noun-verb) and a window selection.
        h.click(4.0, 4.0).click(16.0, 6.0); // click-click window around the line
        assert_eq!(h.ed.selection.len(), 1);
        h.cmd("MOVE").cmd("0,0").cmd("1,0");
        assert_eq!(line(&h, 0).a, DVec2::new(6.0, 5.0));
        // Undo/redo of the move.
        h.cmd("U");
        assert_eq!(line(&h, 0).a, DVec2::new(5.0, 5.0));
        h.cmd("REDO");
        assert_eq!(line(&h, 0).a, DVec2::new(6.0, 5.0));
        h.ed.undo();
        h.ed.undo();
        h.ed.undo();
        assert_eq!(h.count("LINE"), 0);
        h.ed.redo();
        assert_eq!(h.count("LINE"), 1);
    }

    #[test]
    fn erase_and_selection_modes() {
        let mut h = Harness::new();
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        h.cmd("CIRCLE").cmd("20,0").cmd("2");
        // Crossing window (right to left) touching both.
        h.click(21.0, 1.0).click(5.0, -1.0);
        assert_eq!(h.ed.selection.len(), 2);
        // Shift-click removes.
        h.shift_click(5.0, 0.0);
        assert_eq!(h.ed.selection.len(), 1);
        h.cmd("E");
        assert_eq!(h.count("CIRCLE"), 0);
        assert_eq!(h.count("LINE"), 1);
        assert!(h.ed.selection.is_empty());
        // ERASE without selection: select then Enter.
        h.cmd("ERASE").click(3.0, 0.0).enter();
        assert_eq!(h.count("LINE"), 0);
        h.cmd("u");
        assert_eq!(h.count("LINE"), 1);
        // Esc clears the selection.
        h.click(3.0, 0.0);
        assert_eq!(h.ed.selection.len(), 1);
        h.esc();
        assert!(h.ed.selection.is_empty());
    }

    #[test]
    fn grip_edit_line_endpoint() {
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        h.click(5.0, 0.0); // select
        h.click(10.0, 0.0); // grip at the end -> grip tool
        assert_eq!(h.ed.active_tool_name(), Some("GRIP"));
        h.click(10.0, 5.0);
        assert_eq!(line(&h, 0).b, DVec2::new(10.0, 5.0));
        assert!(!h.ed.has_tool());
    }

    #[test]
    fn locked_layer_is_protected() {
        let mut h = Harness::new();
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        let l0 = h.ed.doc.drawing.tables.current_layer;
        h.ed.doc.transact("lock", |tx| {
            if let Some(l) = tx.tables_mut().layers.get_mut(&l0) {
                l.locked = true;
            }
        });
        h.ed.pump();
        h.click(5.0, 0.0).cmd("E");
        assert_eq!(h.count("LINE"), 1);
    }

    /// A pick-mode tool as TRIM-like tools would write it.
    #[derive(Default)]
    struct PickTool {
        picked: Option<wcad_doc::EntityId>,
    }

    impl crate::tools::Tool for PickTool {
        fn name(&self) -> &'static str {
            "PICKTEST"
        }
        fn prompt(&self, _: crate::i18n::Lang) -> String {
            "Pick".into()
        }
        fn accepts(&self) -> crate::tools::Accept {
            crate::tools::Accept::PICK
        }
        fn on_input(
            &mut self,
            input: crate::tools::ToolInput,
            cx: &mut crate::tools::ToolCx<'_>,
        ) -> crate::tools::ToolFlow {
            if let crate::tools::ToolInput::Point(p) = input {
                // Raw position: not snapped to the nearby endpoint.
                assert_eq!(p, DVec2::new(9.95, 0.02));
                self.picked = cx.pick(p);
                cx.message(format!("picked {:?}", self.picked.map(|i| i.0)));
                return crate::tools::ToolFlow::Done;
            }
            crate::tools::ToolFlow::Continue
        }
    }

    #[test]
    fn pick_mode_delivers_raw_points() {
        let mut h = Harness::new();
        h.cmd("LINE").cmd("0,0").cmd("10,0").cmd("");
        let id = h.of_type("LINE")[0].0;
        h.ed.start_tool(Box::new(PickTool::default()));
        h.hover(9.95, 0.02);
        assert_eq!(
            h.ed.hover,
            Some(id),
            "pick mode highlights the entity under the cursor"
        );
        assert!(h.ed.cursor.is_some_and(|c| c.snap.is_none()));
        h.click(9.95, 0.02);
        assert_eq!(h.last_message(), format!("picked Some({})", id.0));
    }

    #[test]
    fn zoom_requests() {
        let mut h = Harness::new();
        h.cmd("Z").cmd("E");
        assert_eq!(
            h.ed.requests.pop(),
            Some(crate::editor::AppRequest::ZoomExtents)
        );
        h.cmd("ZOOM").cmd("0,0").cmd("10,10");
        assert!(matches!(
            h.ed.requests.pop(),
            Some(crate::editor::AppRequest::ZoomWindow(_))
        ));
    }

    #[test]
    fn garbage_input_never_panics() {
        let mut h = Harness::new();
        for s in [
            "", " ", "@", "<", "1e999", "nan,1", "L", "@1<", "10,10", "u", "c", "ERASE", "",
            "MOVE", "Z", "x", "",
        ] {
            h.cmd(s);
        }
        h.esc();
        assert!(!h.ed.has_tool());
        let _ = Line2::new(DVec2::ZERO, DVec2::X);
    }
}
