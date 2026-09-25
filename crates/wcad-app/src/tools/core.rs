//! Core commands and proof tools: LINE, CIRCLE, ERASE, MOVE, ZOOM, grip editing, undo/redo and
//! file actions.

use wcad_doc::{EntityId, EntityKind};
use wcad_geom2d::{Circle2, Curve2, Line2};
use wcad_math::{BBox2, DAffine2, DVec2};

use super::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput, entities_of};
use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::editor::{AppRequest, Editor};
use crate::i18n::{Lang, core, fmt};
use crate::select;
use crate::xform;

pub fn register(r: &mut CommandRegistry) {
    let tool = |name,
                aliases,
                label: fn(Lang) -> &'static str,
                icon,
                tab,
                group,
                ctor: fn() -> Box<dyn Tool>| {
        CommandSpec {
            name,
            aliases,
            label,
            icon,
            tab,
            group,
            kind: CommandKind::Tool(ctor),
        }
    };
    let action =
        |name, aliases, label: fn(Lang) -> &'static str, icon, tab, group, f: fn(&mut Editor)| {
            CommandSpec {
                name,
                aliases,
                label,
                icon,
                tab,
                group,
                kind: CommandKind::Action(f),
            }
        };
    r.add(tool(
        "LINE",
        &["L"],
        |l| core(l).cmd_line,
        "／",
        Some(RibbonTab::Draw),
        "core",
        || Box::new(LineTool::default()),
    ));
    r.add(tool(
        "CIRCLE",
        &["C"],
        |l| core(l).cmd_circle,
        "○",
        Some(RibbonTab::Draw),
        "core",
        || Box::new(CircleTool::default()),
    ));
    r.add(tool(
        "MOVE",
        &["M"],
        |l| core(l).cmd_move,
        "✚",
        Some(RibbonTab::Modify),
        "core",
        || Box::new(MoveTool::default()),
    ));
    r.add(tool(
        "ERASE",
        &["E", "DELETE"],
        |l| core(l).cmd_erase,
        "🗑",
        Some(RibbonTab::Modify),
        "core",
        || Box::new(EraseTool),
    ));
    r.add(tool(
        "ZOOM",
        &["Z"],
        |l| core(l).cmd_zoom,
        "⊞",
        Some(RibbonTab::View),
        "zoom",
        || Box::new(ZoomTool::default()),
    ));
    r.add(action(
        "ZOOMEXTENTS",
        &["ZE"],
        |l| core(l).zoom_extents,
        "⛶",
        Some(RibbonTab::View),
        "zoom",
        |ed| ed.requests.push(AppRequest::ZoomExtents),
    ));
    r.add(action(
        "U",
        &["UNDO"],
        |l| core(l).cmd_undo,
        "⟲",
        None,
        "edit",
        Editor::undo,
    ));
    r.add(action(
        "REDO",
        &[],
        |l| core(l).cmd_redo,
        "⟳",
        None,
        "edit",
        Editor::redo,
    ));
    r.add(action(
        "SELECTALL",
        &["AI_SELALL"],
        |l| core(l).cmd_select_all,
        "▣",
        None,
        "edit",
        Editor::select_all,
    ));
    r.add(action(
        "NEW",
        &["QNEW"],
        |l| core(l).new,
        "🗋",
        None,
        "file",
        |ed| ed.requests.push(AppRequest::New),
    ));
    r.add(action(
        "OPEN",
        &[],
        |l| core(l).open,
        "🗁",
        None,
        "file",
        |ed| ed.requests.push(AppRequest::Open),
    ));
    r.add(action(
        "QSAVE",
        &["SAVE"],
        |l| core(l).save,
        "🖴",
        None,
        "file",
        |ed| ed.requests.push(AppRequest::Save),
    ));
    r.add(action(
        "SAVEAS",
        &[],
        |l| core(l).save_as,
        "🖴",
        None,
        "file",
        |ed| ed.requests.push(AppRequest::SaveAs),
    ));
    r.add(action(
        "IMPORT",
        &["DXFIN"],
        |l| core(l).import,
        "🗁",
        None,
        "file",
        |ed| ed.requests.push(AppRequest::Import),
    ));
    r.add(action(
        "LAYER",
        &["LA"],
        |l| core(l).layers,
        "☰",
        Some(RibbonTab::View),
        "panels",
        |ed| ed.requests.push(AppRequest::ShowPanel("layers")),
    ));
}

fn kw(id: &'static str, key: &'static str, label: &'static str) -> Keyword {
    Keyword { id, key, label }
}

// -------------------------------------------------------------------------------------------
// LINE

/// LINE: continuous segments; `Undo` removes the last segment, `Close` closes back to the first
/// point, Enter ends.
#[derive(Default)]
pub struct LineTool {
    first: Option<DVec2>,
    last: Option<DVec2>,
    /// Points entered so far (for Undo) and ids of the created segments.
    points: Vec<DVec2>,
    segments: Vec<EntityId>,
}

impl Tool for LineTool {
    fn name(&self) -> &'static str {
        "LINE"
    }
    fn prompt(&self, lang: Lang) -> String {
        let s = core(lang);
        if self.last.is_none() {
            s.line_first.into()
        } else {
            s.line_next.into()
        }
    }
    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = core(lang);
        let mut v = Vec::new();
        if !self.segments.is_empty() {
            v.push(kw("Undo", "U", s.kw_undo));
        }
        if self.segments.len() >= 2 {
            v.push(kw("Close", "C", s.kw_close));
        }
        v
    }
    fn base_point(&self) -> Option<DVec2> {
        self.last
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Point(p) => {
                match self.last {
                    None => {
                        self.first = Some(p);
                        self.points.push(p);
                    }
                    Some(a) => {
                        if a.distance(p) <= 0.0 {
                            return ToolFlow::Continue;
                        }
                        let id =
                            cx.transact("LINE", |tx| tx.add(EntityKind::Line(Line2::new(a, p))));
                        self.segments.push(id);
                        self.points.push(p);
                    }
                }
                self.last = Some(p);
                ToolFlow::Continue
            }
            ToolInput::Keyword("Undo") => {
                if self.segments.pop().is_some() {
                    cx.undo();
                    self.points.pop();
                    self.last = self.points.last().copied();
                }
                ToolFlow::Continue
            }
            ToolInput::Keyword("Close") => {
                if let (Some(a), Some(f)) = (self.last, self.first)
                    && self.segments.len() >= 2
                {
                    cx.transact("LINE", |tx| tx.add(EntityKind::Line(Line2::new(a, f))));
                    return ToolFlow::Done;
                }
                ToolFlow::Continue
            }
            ToolInput::Enter | ToolInput::Escape => ToolFlow::Done,
            _ => ToolFlow::Continue,
        }
    }
    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let (Some(a), Some(c)) = (self.last, cx.cursor())
            && a.distance(c) > 0.0
        {
            out.curves.push(Curve2::Line(Line2::new(a, c)));
        }
    }
}

// -------------------------------------------------------------------------------------------
// CIRCLE

/// CIRCLE (center, radius): radius by point or value; `Diameter` switches to a diameter value.
#[derive(Default)]
pub struct CircleTool {
    center: Option<DVec2>,
    diameter: bool,
}

impl CircleTool {
    fn make(&self, cx: &mut ToolCx<'_>, r: f64) -> ToolFlow {
        let Some(c) = self.center else {
            return ToolFlow::Continue;
        };
        if !(r > 0.0) || !r.is_finite() {
            cx.error(core(cx.lang()).value_must_be_positive);
            return ToolFlow::Continue;
        }
        cx.transact("CIRCLE", |tx| {
            tx.add(EntityKind::Circle(Circle2::new(c, r)))
        });
        ToolFlow::Done
    }
}

impl Tool for CircleTool {
    fn name(&self) -> &'static str {
        "CIRCLE"
    }
    fn prompt(&self, lang: Lang) -> String {
        let s = core(lang);
        match (self.center, self.diameter) {
            (None, _) => s.circle_center.into(),
            (Some(_), false) => s.circle_radius.into(),
            (Some(_), true) => s.circle_diameter.into(),
        }
    }
    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        if self.center.is_some() && !self.diameter {
            vec![kw("Diameter", "D", core(lang).kw_diameter)]
        } else {
            vec![]
        }
    }
    fn accepts(&self) -> Accept {
        if self.center.is_some() {
            Accept::POINT_OR_VALUE
        } else {
            Accept::POINT
        }
    }
    fn base_point(&self) -> Option<DVec2> {
        self.center
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Point(p) => match self.center {
                None => {
                    self.center = Some(p);
                    ToolFlow::Continue
                }
                Some(c) => {
                    let d = c.distance(p);
                    self.make(cx, if self.diameter { d / 2.0 } else { d })
                }
            },
            ToolInput::Value(v) if self.center.is_some() => {
                self.make(cx, if self.diameter { v / 2.0 } else { v })
            }
            ToolInput::Keyword("Diameter") => {
                self.diameter = true;
                ToolFlow::Continue
            }
            ToolInput::Escape => ToolFlow::Cancel,
            ToolInput::Enter => {
                if self.center.is_none() {
                    ToolFlow::Cancel
                } else {
                    ToolFlow::Continue
                }
            }
            _ => ToolFlow::Continue,
        }
    }
    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let (Some(c), Some(p)) = (self.center, cx.cursor()) {
            let d = c.distance(p);
            let r = if self.diameter { d / 2.0 } else { d };
            if r > 0.0 {
                out.curves.push(Curve2::Circle(Circle2::new(c, r)));
                out.rubber_band = true;
            }
        }
    }
}

// -------------------------------------------------------------------------------------------
// ERASE

#[derive(Default)]
pub struct EraseTool;

impl Tool for EraseTool {
    fn name(&self) -> &'static str {
        "ERASE"
    }
    fn prompt(&self, lang: Lang) -> String {
        core(lang).select_objects.into()
    }
    fn accepts(&self) -> Accept {
        Accept::SELECTION
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Selection(ids) => {
                let ids = cx.editable(&ids);
                if ids.is_empty() {
                    cx.message(core(cx.lang()).nothing_selected);
                    return ToolFlow::Done;
                }
                let n = cx.transact("ERASE", |tx| {
                    ids.iter().filter(|id| tx.remove(**id).is_some()).count()
                });
                cx.selection_mut().clear();
                let msg = fmt(core(cx.lang()).n_erased, &[&n]);
                cx.message(msg);
                ToolFlow::Done
            }
            ToolInput::Escape => ToolFlow::Cancel,
            _ => ToolFlow::Continue,
        }
    }
}

// -------------------------------------------------------------------------------------------
// MOVE

/// MOVE: select objects (or use the pre-selection), base point, second point. Enter at the second
/// point uses the base point as the displacement (AutoCAD behaviour).
#[derive(Default)]
pub struct MoveTool {
    ids: Vec<EntityId>,
    base: Option<DVec2>,
}

impl MoveTool {
    fn apply(&self, cx: &mut ToolCx<'_>, delta: DVec2) -> ToolFlow {
        if !delta.is_finite() {
            return ToolFlow::Continue;
        }
        let m = DAffine2::from_translation(delta);
        let ids = self.ids.clone();
        let n = cx.transact("MOVE", |tx| {
            ids.iter()
                .filter(|id| tx.modify(**id, |e| e.kind = xform::transform_kind(&e.kind, &m)))
                .count()
        });
        let msg = fmt(core(cx.lang()).n_moved, &[&n]);
        cx.message(msg);
        cx.selection_mut().clear();
        ToolFlow::Done
    }
}

impl Tool for MoveTool {
    fn name(&self) -> &'static str {
        "MOVE"
    }
    fn prompt(&self, lang: Lang) -> String {
        let s = core(lang);
        if self.ids.is_empty() {
            s.select_objects.into()
        } else if self.base.is_none() {
            s.move_base.into()
        } else {
            s.move_second.into()
        }
    }
    fn accepts(&self) -> Accept {
        if self.ids.is_empty() {
            Accept::SELECTION
        } else {
            Accept::POINT
        }
    }
    fn base_point(&self) -> Option<DVec2> {
        self.base
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Selection(ids) if self.ids.is_empty() => {
                self.ids = cx.editable(&ids);
                if self.ids.is_empty() {
                    cx.message(core(cx.lang()).nothing_selected);
                    return ToolFlow::Done;
                }
                ToolFlow::Continue
            }
            ToolInput::Point(p) if !self.ids.is_empty() => match self.base {
                None => {
                    self.base = Some(p);
                    ToolFlow::Continue
                }
                Some(b) => self.apply(cx, p - b),
            },
            ToolInput::Enter => match self.base {
                Some(b) => self.apply(cx, b),
                None => ToolFlow::Continue,
            },
            ToolInput::Escape => ToolFlow::Cancel,
            _ => ToolFlow::Continue,
        }
    }
    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let (Some(b), Some(c)) = (self.base, cx.cursor()) {
            let m = DAffine2::from_translation(c - b);
            out.ghosts = entities_of(cx.drawing(), &self.ids)
                .iter()
                .map(|e| xform::transform_entity(e, &m))
                .collect();
            out.rubber_band = true;
        }
    }
}

// -------------------------------------------------------------------------------------------
// ZOOM

/// ZOOM: `Extents`/`All` or a window by two corners.
#[derive(Default)]
pub struct ZoomTool {
    corner: Option<DVec2>,
}

impl Tool for ZoomTool {
    fn name(&self) -> &'static str {
        "ZOOM"
    }
    fn prompt(&self, lang: Lang) -> String {
        let s = core(lang);
        if self.corner.is_none() {
            s.zoom_prompt.into()
        } else {
            s.zoom_corner2.into()
        }
    }
    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = core(lang);
        if self.corner.is_none() {
            vec![
                kw("All", "A", s.kw_all),
                kw("Extents", "E", s.kw_extents),
                kw("Window", "W", s.kw_window),
            ]
        } else {
            vec![]
        }
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Keyword("Extents") | ToolInput::Keyword("All") => {
                cx.request(AppRequest::ZoomExtents);
                ToolFlow::Done
            }
            ToolInput::Keyword("Window") => ToolFlow::Continue,
            ToolInput::Point(p) => match self.corner {
                None => {
                    self.corner = Some(p);
                    ToolFlow::Continue
                }
                Some(a) => {
                    let b = BBox2::new(a, p);
                    if b.size().min_element() > 0.0 {
                        cx.request(AppRequest::ZoomWindow(b));
                    }
                    ToolFlow::Done
                }
            },
            ToolInput::Enter | ToolInput::Escape => ToolFlow::Done,
            _ => ToolFlow::Continue,
        }
    }
    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if let (Some(a), Some(b)) = (self.corner, cx.cursor()) {
            let pl = wcad_geom2d::Polyline2::from_points(
                [a, DVec2::new(b.x, a.y), b, DVec2::new(a.x, b.y)],
                true,
            );
            out.curves.push(Curve2::Polyline(pl));
        }
    }
}

// -------------------------------------------------------------------------------------------
// Grips

/// Grip editing: stretches every selected entity that has a grip at the picked location.
pub struct GripTool {
    from: DVec2,
}

impl GripTool {
    pub fn new(from: DVec2) -> Self {
        Self { from }
    }

    fn targets(&self, cx: &ToolCx<'_>) -> Vec<(EntityId, usize)> {
        let eps = cx.units_per_px() * 0.5 + 1e-12;
        let d = cx.drawing();
        cx.selection()
            .iter()
            .filter_map(|id| {
                let e = d.entities.get(&id)?;
                if !d.is_layer_editable(e.layer) {
                    return None;
                }
                let g = select::grips(&e.kind);
                let i = g.iter().position(|g| g.distance(self.from) <= eps)?;
                Some((id, i))
            })
            .collect()
    }
}

impl Tool for GripTool {
    fn name(&self) -> &'static str {
        "GRIP"
    }
    fn prompt(&self, lang: Lang) -> String {
        core(lang).grip_stretch.into()
    }
    fn base_point(&self) -> Option<DVec2> {
        Some(self.from)
    }
    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        match input {
            ToolInput::Point(p) => {
                let targets = self.targets(cx);
                cx.transact("GRIP", |tx| {
                    for (id, i) in &targets {
                        let Some(kind) = tx
                            .entity(*id)
                            .and_then(|e| select::move_grip(&e.kind, *i, p))
                        else {
                            continue;
                        };
                        tx.modify(*id, |e| e.kind = kind);
                    }
                });
                ToolFlow::Done
            }
            ToolInput::Escape | ToolInput::Enter => ToolFlow::Cancel,
            _ => ToolFlow::Continue,
        }
    }
    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        let Some(c) = cx.cursor() else { return };
        for (id, i) in self.targets(cx) {
            if let Some(e) = cx.entity(id)
                && let Some(kind) = select::move_grip(&e.kind, i, c)
            {
                out.ghosts.push(wcad_doc::Entity { kind, ..e.clone() });
            }
        }
        out.rubber_band = true;
    }
}
