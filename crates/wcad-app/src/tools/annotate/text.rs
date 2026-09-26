use wcad_doc::{EntityKind, MText, Text};
use wcad_math::DVec2;

use super::{MAX_SIZE, annotate, commit, error, finite_input, ghost, positive, separated};
use crate::i18n::{Lang, fmt};
use crate::tools::{Accept, Preview, Tool, ToolCx, ToolFlow, ToolInput};

#[derive(Clone, Copy)]
enum Step {
    Position,
    Height,
    Rotation,
    Body,
}

pub(super) struct TextTool {
    multiline: bool,
    step: Step,
    pos: DVec2,
    height: f64,
    rotation: f64,
}

impl TextTool {
    pub(super) fn new(multiline: bool) -> Self {
        Self {
            multiline,
            step: Step::Position,
            pos: DVec2::ZERO,
            height: 2.5,
            rotation: 0.0,
        }
    }

    fn entity(
        &self,
        cx: &ToolCx<'_>,
        text: String,
        height: f64,
        rotation: f64,
    ) -> Option<EntityKind> {
        let tables = &cx.drawing().tables;
        let style = tables.current_text_style;
        let st = tables.text_styles.get(&style)?;
        if !self.pos.is_finite()
            || !positive(height)
            || !rotation.is_finite()
            || !positive(st.width_factor)
            || !st.oblique.is_finite()
            || !st.height.is_finite()
            || st.height < 0.0
            || !positive(height * st.width_factor)
            || !(height * st.oblique.tan()).is_finite()
            || (height * st.oblique.tan()).abs() >= MAX_SIZE
            || !positive(height * text.len().max(1) as f64 * st.width_factor)
        {
            return None;
        }
        Some(if self.multiline {
            EntityKind::MText(MText {
                pos: self.pos,
                height,
                width: 0.0,
                rotation,
                line_spacing: 1.0,
                attachment: 1,
                style,
                text,
            })
        } else {
            EntityKind::Text(Text {
                pos: self.pos,
                height,
                rotation,
                width_factor: st.width_factor,
                oblique: st.oblique,
                style,
                halign: Default::default(),
                valign: Default::default(),
                text,
            })
        })
    }
}

impl Tool for TextTool {
    fn name(&self) -> &'static str {
        if self.multiline { "MTEXT" } else { "TEXT" }
    }

    fn start(&mut self, cx: &mut ToolCx<'_>) -> ToolFlow {
        if let Some(st) = cx
            .drawing()
            .tables
            .text_styles
            .get(&cx.drawing().tables.current_text_style)
            && positive(st.height)
        {
            self.height = st.height;
        }
        ToolFlow::Continue
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = annotate(lang);
        match self.step {
            Step::Position => s.position.into(),
            Step::Height => fmt(s.height, &[&self.height]),
            Step::Rotation => s.rotation.into(),
            Step::Body => if self.multiline {
                s.multiline_body
            } else {
                s.body
            }
            .into(),
        }
    }

    fn accepts(&self) -> Accept {
        match self.step {
            Step::Position => Accept::POINT,
            Step::Height | Step::Rotation => Accept::POINT_OR_VALUE,
            // No keywords in this state: "LINE", "123", "0,0" and "U" are all text.
            Step::Body => Accept::TEXT,
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        matches!(self.step, Step::Height | Step::Rotation).then_some(self.pos)
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let s = annotate(cx.lang());
        if !finite_input(&input) {
            return error(cx, s.invalid);
        }
        if input == ToolInput::Escape {
            return ToolFlow::Cancel;
        }
        match (self.step, input) {
            (Step::Position, ToolInput::Point(p)) => {
                self.pos = p;
                self.step = Step::Height;
            }
            (Step::Height, ToolInput::Value(h)) => {
                if !positive(h) {
                    return error(cx, s.positive);
                }
                self.height = h;
                self.step = Step::Rotation;
            }
            (Step::Height, ToolInput::Point(p)) => {
                if !separated(self.pos, p) {
                    return error(cx, s.positive);
                }
                self.height = self.pos.distance(p);
                self.step = Step::Rotation;
            }
            (Step::Height, ToolInput::Enter) => self.step = Step::Rotation,
            (Step::Rotation, ToolInput::Value(a)) => {
                self.rotation = a.rem_euclid(360.0).to_radians();
                self.step = Step::Body;
            }
            (Step::Rotation, ToolInput::Point(p)) => {
                if !separated(self.pos, p) {
                    return error(cx, s.degenerate);
                }
                self.rotation = (p - self.pos).to_angle();
                self.step = Step::Body;
            }
            (Step::Rotation, ToolInput::Enter) => self.step = Step::Body,
            (Step::Body, ToolInput::Text(text)) => {
                if text.trim().is_empty() {
                    return error(cx, s.empty_text);
                }
                if !self.multiline
                    && (text.contains(['\n', '\r'])
                        || wcad_geom2d::text::mtext_to_plain(&text).contains(['\n', '\r']))
                {
                    return error(cx, s.single_line);
                }
                if text.len() > 65_536
                    || text
                        .chars()
                        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
                {
                    return error(cx, s.text_limit);
                }
                // Preserve MTEXT format codes, normalizing platform-specific paragraph endings.
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let Some(kind) = self.entity(cx, text, self.height, self.rotation) else {
                    return error(cx, s.style_missing);
                };
                return commit(cx, self.name(), kind);
            }
            (Step::Body, ToolInput::Enter) => return error(cx, s.empty_text),
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if matches!(self.step, Step::Position) {
            return;
        }
        out.points.push(self.pos);
        let mut height = self.height;
        let mut rotation = self.rotation;
        if let Some(p) = cx.cursor().filter(|p| separated(self.pos, *p)) {
            match self.step {
                Step::Height => height = self.pos.distance(p),
                Step::Rotation => rotation = (p - self.pos).to_angle(),
                _ => {}
            }
        }
        if let Some(kind) = self.entity(cx, "Abc".into(), height, rotation) {
            ghost(cx, out, kind);
        }
    }
}
