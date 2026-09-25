//! The editing core: document, active tool, selection, snapping, command line and history.
//! Independent of egui/wgpu so tools and command-line behaviour can be tested headless.
//!
//! The UI feeds it viewport events ([`Editor::pointer_move`], [`Editor::click`],
//! [`Editor::window`], [`Editor::escape`], …) in world coordinates and command-line submissions
//! ([`Editor::submit`]); it drains [`Editor::requests`] for things only the app can do (file
//! dialogs, zooming) and [`Editor::take_display_changes`] for incremental display-list updates.

use std::sync::Arc;

use wcad_doc::{ChangeSet, Document, EntityId};
use wcad_math::{BBox2, DVec2};

use crate::cmdline::{self, ParseCx, Parsed};
use crate::commands::{CommandKind, CommandRegistry};
use crate::i18n::{Lang, core, fmt};
use crate::select::{self, Selection};
use crate::settings::DraftSettings;
use crate::snap::{self, SnapHit, SpatialIndex};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

/// Things the editor asks the application to do.
#[derive(Clone, Debug, PartialEq)]
pub enum AppRequest {
    ZoomExtents,
    ZoomWindow(BBox2),
    New,
    Open,
    Save,
    SaveAs,
    Import,
    Export(ExportFormat),
    SetWorkspace(Workspace),
    LoadDemo,
    /// Show a dock panel by id (e.g. `"layers"`).
    ShowPanel(&'static str),
    /// Save/download bytes under a file name (native: save dialog; web: download).
    SaveBytes {
        name: String,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Dxf,
    Dwg,
    Svg,
    Pdf,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Workspace {
    #[default]
    Drafting,
    Modeling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// Echo of the prompt and what the user typed.
    Echo,
    Info,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogLine {
    pub kind: LogKind,
    pub text: String,
}

/// Maximum lines kept in the command history.
const MAX_LOG: usize = 500;

/// The resolved cursor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorInfo {
    /// Raw world position under the pointer.
    pub raw: DVec2,
    /// Snapped/constrained position a click would enter.
    pub point: DVec2,
    pub snap: Option<SnapHit>,
    /// Active tracking ray (base point, angle) from ortho/polar/angle lock.
    pub tracking: Option<(DVec2, f64)>,
}

pub struct Editor {
    pub doc: Document,
    pub selection: Selection,
    pub draft: DraftSettings,
    pub lang: Lang,
    pub registry: Arc<CommandRegistry>,
    pub index: SpatialIndex,
    pub log: Vec<LogLine>,
    pub requests: Vec<AppRequest>,
    pub cursor: Option<CursorInfo>,
    pub last_point: Option<DVec2>,
    /// Entity under the pointer (pre-selection highlight).
    pub hover: Option<EntityId>,
    /// World units per logical pixel of the active viewport.
    pub units_per_px: f64,
    /// First corner of a click-click selection window.
    pub window_start: Option<DVec2>,
    pub preview: Preview,
    tool: Option<Box<dyn Tool>>,
    last_command: Option<&'static str>,
    angle_lock: Option<f64>,
    display_changes: ChangeSet,
}

fn merge(into: &mut ChangeSet, c: ChangeSet) {
    into.entities.extend(c.entities);
    into.tables |= c.tables;
    into.blocks |= c.blocks;
    into.part |= c.part;
    into.all |= c.all;
}

impl Editor {
    pub fn new(registry: Arc<CommandRegistry>) -> Self {
        let mut e = Self {
            doc: Document::new(),
            selection: Selection::default(),
            draft: DraftSettings::default(),
            lang: Lang::default(),
            registry,
            index: SpatialIndex::default(),
            log: Vec::new(),
            requests: Vec::new(),
            cursor: None,
            last_point: None,
            hover: None,
            units_per_px: 1.0,
            window_start: None,
            preview: Preview::default(),
            tool: None,
            last_command: None,
            angle_lock: None,
            display_changes: ChangeSet {
                all: true,
                ..Default::default()
            },
        };
        e.pump();
        e
    }

    /// Replace the document (open/new/import). Cancels the active tool and clears the selection.
    pub fn set_document(&mut self, doc: Document) {
        self.tool = None;
        self.preview.clear();
        self.selection.clear();
        self.hover = None;
        self.window_start = None;
        self.last_point = None;
        self.doc = doc;
        self.display_changes.all = true;
        self.pump();
    }

    /// Absorb document changes: update the spatial index, prune the selection and remember the
    /// changes for the display lists. Called after every mutation and once per frame.
    pub fn pump(&mut self) {
        let ch = self.doc.take_changes();
        if ch.is_empty() {
            return;
        }
        if ch.all || ch.blocks || ch.tables {
            self.index.rebuild(&self.doc.drawing);
        } else {
            self.index
                .update(&self.doc.drawing, ch.entities.iter().copied());
        }
        let d = &self.doc.drawing;
        self.selection.retain(|id| d.entities.contains_key(&id));
        if self.hover.is_some_and(|h| !d.entities.contains_key(&h)) {
            self.hover = None;
        }
        merge(&mut self.display_changes, ch);
    }

    /// Changes since the last call (for [`crate::display`]).
    pub fn take_display_changes(&mut self) -> ChangeSet {
        self.pump();
        std::mem::take(&mut self.display_changes)
    }

    /// Force a full display rebuild (theme/lineweight/zoom-level changes).
    pub fn invalidate_display(&mut self) {
        self.display_changes.all = true;
    }

    // ---------------------------------------------------------------------------------------
    // Log

    pub fn info(&mut self, text: impl Into<String>) {
        self.push_log(LogKind::Info, text.into());
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push_log(LogKind::Error, text.into());
    }

    fn push_log(&mut self, kind: LogKind, text: String) {
        self.log.push(LogLine { kind, text });
        if self.log.len() > MAX_LOG {
            let n = self.log.len() - MAX_LOG;
            self.log.drain(..n);
        }
    }

    /// The last `n` history lines' texts (tests, status).
    pub fn last_messages(&self, n: usize) -> Vec<&str> {
        self.log
            .iter()
            .rev()
            .take(n)
            .map(|l| l.text.as_str())
            .collect()
    }

    // ---------------------------------------------------------------------------------------
    // Tools

    pub fn active_tool_name(&self) -> Option<&'static str> {
        self.tool.as_ref().map(|t| t.name())
    }

    pub fn has_tool(&self) -> bool {
        self.tool.is_some()
    }

    pub fn tool_accepts(&self) -> Accept {
        self.tool
            .as_ref()
            .map(|t| t.accepts())
            .unwrap_or(Accept::NONE)
    }

    pub fn tool_base_point(&self) -> Option<DVec2> {
        self.tool.as_ref().and_then(|t| t.base_point())
    }

    pub fn last_command(&self) -> Option<&'static str> {
        self.last_command
    }

    /// Current prompt and keywords (the idle prompt is "Command:").
    pub fn prompt(&self) -> (String, Vec<Keyword>) {
        match &self.tool {
            Some(t) => (t.prompt(self.lang), t.keywords(self.lang)),
            None => (
                core(self.lang)
                    .command_prompt
                    .trim_end_matches(':')
                    .to_owned(),
                Vec::new(),
            ),
        }
    }

    /// Prompt formatted AutoCAD style: `Specify next point [Undo(U)/Close(C)]:`.
    pub fn prompt_text(&self) -> String {
        let (p, kws) = self.prompt();
        if kws.is_empty() {
            format!("{p}:")
        } else {
            let k: Vec<String> = kws
                .iter()
                .map(|k| format!("{}({})", k.label, k.key))
                .collect();
            format!("{p} [{}]:", k.join("/"))
        }
    }

    fn with_tool<R>(
        &mut self,
        f: impl FnOnce(&mut dyn Tool, &mut ToolCx<'_>) -> R,
    ) -> Option<(Box<dyn Tool>, R)> {
        let mut tool = self.tool.take()?;
        let cursor = self.cursor.map(|c| c.point);
        let mut cx = ToolCx {
            doc: &mut self.doc,
            selection: &mut self.selection,
            draft: &self.draft,
            index: &self.index,
            log: &mut self.log,
            requests: &mut self.requests,
            lang: self.lang,
            cursor,
            last_point: self.last_point,
            units_per_px: self.units_per_px,
        };
        let r = f(tool.as_mut(), &mut cx);
        Some((tool, r))
    }

    fn settle(&mut self, tool: Box<dyn Tool>, flow: ToolFlow) {
        self.pump();
        match flow {
            ToolFlow::Continue => self.tool = Some(tool),
            ToolFlow::Done => {
                self.angle_lock = None;
            }
            ToolFlow::Cancel => {
                self.angle_lock = None;
                let s = core(self.lang).cancelled;
                self.info(s);
            }
        }
        self.refresh_preview();
    }

    /// Start `tool` (the current one is cancelled). Pre-selected entities are handed to tools that
    /// accept a selection.
    pub fn start_tool(&mut self, tool: Box<dyn Tool>) {
        self.tool = None;
        self.window_start = None;
        self.angle_lock = None;
        self.tool = Some(tool);
        let Some((tool, mut flow)) = self.with_tool(|t, cx| t.start(cx)) else {
            return;
        };
        if flow == ToolFlow::Continue && tool.accepts().selection {
            let pre: Vec<EntityId> = self.selection.iter().collect();
            if !pre.is_empty() {
                self.tool = Some(tool);
                let Some((tool2, f2)) =
                    self.with_tool(|t, cx| t.on_input(ToolInput::Selection(pre), cx))
                else {
                    return;
                };
                flow = f2;
                self.settle(tool2, flow);
                return;
            }
        }
        self.settle(tool, flow);
    }

    /// Deliver an input to the active tool.
    pub fn feed(&mut self, input: ToolInput) {
        if let ToolInput::Point(p) = input {
            self.last_point = Some(p);
            self.angle_lock = None;
        }
        if let Some((tool, flow)) = self.with_tool(|t, cx| t.on_input(input, cx)) {
            self.settle(tool, flow);
        }
    }

    /// Run a command by name or alias. Returns `false` for unknown commands.
    pub fn run_command(&mut self, name: &str) -> bool {
        let Some(spec) = self.registry.find(name).copied() else {
            let msg = fmt(core(self.lang).unknown_command, &[&name.trim()]);
            self.error(msg);
            return false;
        };
        self.last_command = Some(spec.name);
        match spec.kind {
            CommandKind::Tool(ctor) => {
                if self.tool.is_some() {
                    self.escape_tool();
                }
                self.start_tool(ctor());
            }
            CommandKind::Action(f) => {
                f(self);
                self.pump();
            }
        }
        true
    }

    /// Enter / Space / right click: finish the selection prompt, confirm the step, or repeat the
    /// last command when idle.
    pub fn enter(&mut self) {
        if self.tool.is_some() {
            if self.tool_accepts().selection {
                let sel: Vec<EntityId> = self.selection.iter().collect();
                self.feed(ToolInput::Selection(sel));
            } else {
                self.feed(ToolInput::Enter);
            }
        } else if let Some(c) = self.last_command {
            self.echo(c);
            self.run_command(c);
        }
    }

    fn escape_tool(&mut self) {
        if let Some((tool, _)) = self.with_tool(|t, cx| t.on_input(ToolInput::Escape, cx)) {
            // Esc always ends the command.
            self.settle(tool, ToolFlow::Cancel);
        }
    }

    /// Esc: cancel the tool, else the pending window, else the selection.
    pub fn escape(&mut self) {
        if self.tool.is_some() {
            self.escape_tool();
        } else if self.window_start.is_some() {
            self.window_start = None;
        } else {
            self.selection.clear();
        }
        self.angle_lock = None;
    }

    fn echo(&mut self, input: &str) {
        let line = format!("{} {}", self.prompt_text(), input);
        self.push_log(LogKind::Echo, line);
    }

    /// A keyword clicked in the command line.
    pub fn echo_keyword(&mut self, k: Keyword) {
        self.echo(k.label);
        self.feed(ToolInput::Keyword(k.id));
    }

    /// Handle one command-line submission.
    pub fn submit(&mut self, line: &str) {
        let (keywords, accepts) = match &self.tool {
            Some(t) => (t.keywords(self.lang), t.accepts()),
            None => (Vec::new(), Accept::NONE),
        };
        let registry = self.registry.clone();
        let parsed = cmdline::parse(
            line,
            &ParseCx {
                keywords: &keywords,
                accepts_text: accepts.text,
                registry: Some(&registry),
            },
        );
        if !line.trim().is_empty() {
            self.echo(line.trim());
        }
        let s = core(self.lang);
        match parsed {
            Parsed::Empty => self.enter(),
            Parsed::Keyword(k) => self.feed(ToolInput::Keyword(k)),
            Parsed::Text(t) => self.feed(ToolInput::Text(t)),
            Parsed::Point(pi) => {
                if accepts.point {
                    let p = pi.resolve(self.last_point);
                    self.feed(ToolInput::Point(p));
                } else if self.tool.is_some() {
                    self.error(s.point_expected);
                } else {
                    self.error(fmt(s.unknown_command, &[&line.trim()]));
                }
            }
            Parsed::AngleLock(a) => {
                if accepts.point {
                    self.angle_lock = Some(a.to_radians());
                } else {
                    self.error(fmt(s.invalid_input, &[&line.trim()]));
                }
            }
            Parsed::Number(v) => {
                if accepts.value {
                    self.feed(ToolInput::Value(v));
                } else if accepts.point {
                    match self.direct_distance(v) {
                        Some(p) => self.feed(ToolInput::Point(p)),
                        None => self.error(s.point_expected),
                    }
                } else if self.tool.is_some() {
                    self.error(fmt(s.invalid_input, &[&line.trim()]));
                } else {
                    self.error(fmt(s.unknown_command, &[&line.trim()]));
                }
            }
            Parsed::Command(name) => {
                self.run_command(name);
            }
            Parsed::Unknown(u) => {
                if self.tool.is_some() {
                    self.error(fmt(s.invalid_input, &[&u]));
                } else {
                    self.error(fmt(s.unknown_command, &[&u]));
                }
            }
        }
        self.pump();
    }

    /// Direct distance entry: `dist` along the direction from the base point to the cursor
    /// (or along the angle lock).
    fn direct_distance(&self, dist: f64) -> Option<DVec2> {
        let base = self.tool_base_point()?;
        let dir = match self.angle_lock {
            Some(a) => DVec2::from_angle(a),
            None => self
                .cursor
                .and_then(|c| (c.point - base).try_normalize())
                .unwrap_or(DVec2::X),
        };
        Some(base + dir * dist)
    }

    // ---------------------------------------------------------------------------------------
    // Viewport events (world coordinates)

    /// Pointer moved to world position `raw`.
    pub fn pointer_move(&mut self, raw: DVec2) {
        if !raw.is_finite() {
            return;
        }
        let accepts = self.tool_accepts();
        let base = self.tool_base_point();
        let aperture = self.draft.aperture_px as f64 * self.units_per_px;
        let mut info = CursorInfo {
            raw,
            point: raw,
            snap: None,
            tracking: None,
        };
        if accepts.point && !accepts.pick {
            if self.draft.osnap_on {
                info.snap = snap::find_snap(
                    &self.doc.drawing,
                    &self.index,
                    raw,
                    aperture,
                    &self.draft.osnap,
                    base,
                );
            }
            if let Some(s) = info.snap {
                info.point = s.p;
            } else {
                let mut p = raw;
                if let Some(b) = base {
                    let c = snap::constrain(raw, b, &self.draft, self.angle_lock, aperture);
                    p = c.p;
                    info.tracking = c.angle.map(|a| (b, a));
                }
                if self.draft.snap_on {
                    let sp = self.doc.drawing.tables.settings.snap_spacing;
                    p = match info.tracking {
                        Some((b, a)) => {
                            let dir = DVec2::from_angle(a);
                            let d = ((p - b).dot(dir) / sp).round() * sp;
                            if sp > 0.0 && d.is_finite() {
                                b + dir * d
                            } else {
                                p
                            }
                        }
                        None => snap::grid_snap(p, sp),
                    };
                    if info.tracking.is_none() {
                        info.snap = Some(SnapHit {
                            p,
                            kind: snap::OsnapKind::Grid,
                            entity: None,
                        });
                    }
                }
                info.point = p;
            }
        }
        self.cursor = Some(info);
        self.hover = if !accepts.point || accepts.selection || accepts.pick {
            let tol = self.draft.pickbox_px as f64 * self.units_per_px;
            select::pick(&self.doc.drawing, &self.index, raw, tol)
        } else {
            None
        };
        if self.tool.is_some() && accepts.point {
            self.feed_hover(info.point);
        } else {
            self.refresh_preview();
        }
    }

    fn feed_hover(&mut self, p: DVec2) {
        if let Some((tool, flow)) = self.with_tool(|t, cx| t.on_input(ToolInput::Hover(p), cx)) {
            self.settle(tool, flow);
        }
    }

    /// Pointer left the viewport.
    pub fn pointer_leave(&mut self) {
        self.cursor = None;
        self.hover = None;
        self.refresh_preview();
    }

    /// Primary click at world position `raw`. `shift` removes from the selection.
    pub fn click(&mut self, raw: DVec2, shift: bool) {
        self.pointer_move(raw);
        let accepts = self.tool_accepts();
        if self.tool.is_some() && accepts.point {
            let p = self.cursor.map(|c| c.point).unwrap_or(raw);
            self.feed(ToolInput::Point(p));
            return;
        }
        if self.tool.is_some() && !accepts.selection {
            return;
        }
        // Selection click.
        if let Some(start) = self.window_start.take() {
            self.window(start, raw, shift);
            return;
        }
        if self.tool.is_none()
            && !shift
            && let Some((_, _, pos)) = self.grip_at(raw)
        {
            self.start_tool(Box::new(crate::tools::core::GripTool::new(pos)));
            return;
        }
        let tol = self.draft.pickbox_px as f64 * self.units_per_px;
        match select::pick(&self.doc.drawing, &self.index, raw, tol) {
            Some(id) => {
                if shift {
                    self.selection.remove(id);
                } else {
                    self.selection.add(id);
                }
                self.report_selection();
            }
            None => self.window_start = Some(raw),
        }
    }

    /// Finish a selection window from `a` to `b`: left→right = window, right→left = crossing.
    pub fn window(&mut self, a: DVec2, b: DVec2, shift: bool) {
        self.window_start = None;
        let crossing = b.x < a.x;
        let tol = self.units_per_px * 0.5;
        let ids = select::window_select(&self.doc.drawing, &self.index, a, b, crossing, tol);
        if shift {
            for id in ids {
                self.selection.remove(id);
            }
        } else {
            self.selection.extend(ids);
        }
        self.report_selection();
    }

    fn report_selection(&mut self) {
        if self.tool.is_some() {
            let n = self.selection.len();
            let msg = fmt(core(self.lang).n_found, &[&n]);
            self.info(msg);
        }
    }

    /// Grip of a selected entity under `raw` (within the grip size): (entity, grip index, position).
    pub fn grip_at(&self, raw: DVec2) -> Option<(EntityId, usize, DVec2)> {
        if self.selection.len() > 200 {
            return None;
        }
        let tol = (self.draft.grip_px as f64 + 2.0) * self.units_per_px;
        let mut best: Option<(f64, (EntityId, usize, DVec2))> = None;
        for id in self.selection.iter() {
            let Some(e) = self.doc.drawing.entities.get(&id) else {
                continue;
            };
            for (i, g) in select::grips(&e.kind).into_iter().enumerate() {
                let d = (g - raw).abs().max_element();
                if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, (id, i, g)));
                }
            }
        }
        best.map(|(_, g)| g)
    }

    /// Recompute the tool preview for the current cursor.
    pub fn refresh_preview(&mut self) {
        let mut out = Preview::default();
        if let Some(tool) = self.tool.take() {
            {
                let cursor = self.cursor.map(|c| c.point);
                let cx = ToolCx {
                    doc: &mut self.doc,
                    selection: &mut self.selection,
                    draft: &self.draft,
                    index: &self.index,
                    log: &mut self.log,
                    requests: &mut self.requests,
                    lang: self.lang,
                    cursor,
                    last_point: self.last_point,
                    units_per_px: self.units_per_px,
                };
                tool.preview(&cx, &mut out);
            }
            self.tool = Some(tool);
        }
        self.preview = out;
    }

    // ---------------------------------------------------------------------------------------
    // Common actions

    pub fn undo(&mut self) {
        if self.tool.is_some() {
            self.escape_tool();
        }
        let s = core(self.lang);
        match self.doc.undo() {
            Some(label) => self.info(fmt(s.undone, &[&label])),
            None => self.info(s.nothing_to_undo),
        }
        self.pump();
    }

    pub fn redo(&mut self) {
        if self.tool.is_some() {
            self.escape_tool();
        }
        let s = core(self.lang);
        match self.doc.redo() {
            Some(label) => self.info(fmt(s.redone, &[&label])),
            None => self.info(s.nothing_to_redo),
        }
        self.pump();
    }

    /// Select every entity on visible, thawed layers.
    pub fn select_all(&mut self) {
        let d = &self.doc.drawing;
        let ids: Vec<EntityId> = d
            .entities
            .values()
            .filter(|e| d.layer(e.layer).is_some_and(|l| l.visible && !l.frozen))
            .map(|e| e.id)
            .collect();
        self.selection.extend(ids);
    }

    /// Bounds of all visible entities.
    pub fn extents(&self) -> BBox2 {
        self.index.extents()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_command_reports_error() {
        let mut ed = Editor::new(Arc::new(CommandRegistry::with_all_modules()));
        ed.submit("FOOBAR");
        assert_eq!(ed.log.last().map(|l| l.kind), Some(LogKind::Error));
        ed.submit("10,10");
        assert_eq!(ed.log.last().map(|l| l.kind), Some(LogKind::Error));
        assert!(!ed.has_tool());
    }
}
