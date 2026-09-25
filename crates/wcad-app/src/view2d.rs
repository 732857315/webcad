//! The drafting viewport: camera navigation, pointer/touch input → [`Editor`] events, display-list
//! maintenance, offscreen rendering and egui overlays (crosshair, snap markers, selection window,
//! grips, UCS icon).

use egui::{Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use wcad_doc::ChangeSet;
use wcad_math::{BBox2, DVec2};
use wcad_render::{Batch2DStyle, Camera2D, Frame2D, rgb8, rgba8};

use crate::display::{self, DisplayCache, DisplayParams, GRID_BATCH, HOVER_BATCH, SELECTION_BATCH};
use crate::editor::Editor;
use crate::gpu::{Gpu, Slot};
use crate::i18n::core;
use crate::select;
use crate::settings::Theme;
use crate::snap::OsnapKind;

/// Pixels the pointer may move before a press becomes a drag.
const DRAG_THRESHOLD: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Pan,
    /// Selection window from this world point.
    Window(DVec2),
    /// Grip drag (the grip tool is active); release enters the point.
    Grip,
}

pub struct View2d {
    pub camera: Camera2D,
    display: DisplayCache,
    pending: ChangeSet,
    /// Viewport size in logical points and pixels-per-point of the last frame.
    size: Vec2,
    ppp: f32,
    /// Fit the drawing on the next frame (after load).
    /// Fit the drawing on the next frames (after load); the first frame's layout may be provisional.
    pub fit_pending: bool,
    fit_frames: u8,
    drag: Option<Drag>,
    press_pos: Option<Pos2>,
    last_middle_click: f64,
    highlight_key: Option<(u64, u64, Option<wcad_doc::EntityId>, i64)>,
    grid_key: Option<(BBox2, f64, bool)>,
    preview_shown: bool,
    params: Option<DisplayParams>,
}

impl Default for View2d {
    fn default() -> Self {
        Self {
            camera: Camera2D::new(DVec2::new(100.0, 70.0), 3.0),
            display: DisplayCache::new(),
            pending: ChangeSet {
                all: true,
                ..Default::default()
            },
            size: Vec2::new(800.0, 600.0),
            ppp: 1.0,
            fit_pending: true,
            fit_frames: 0,
            drag: None,
            press_pos: None,
            last_middle_click: -10.0,
            highlight_key: None,
            grid_key: None,
            preview_shown: false,
            params: None,
        }
    }
}

pub fn background(theme: Theme) -> [u8; 3] {
    match theme {
        Theme::Dark => [33, 40, 48],
        Theme::Light => [250, 250, 250],
    }
}

fn foreground(theme: Theme) -> [u8; 3] {
    match theme {
        Theme::Dark => [255, 255, 255],
        Theme::Light => [0, 0, 0],
    }
}

impl View2d {
    /// Queue document changes (called every frame, even when the view is hidden).
    pub fn absorb(&mut self, ch: &ChangeSet) {
        self.pending.entities.extend(ch.entities.iter().copied());
        self.pending.tables |= ch.tables;
        self.pending.blocks |= ch.blocks;
        self.pending.all |= ch.all;
    }

    fn vp_px(&self) -> DVec2 {
        DVec2::new(
            (self.size.x * self.ppp) as f64,
            (self.size.y * self.ppp) as f64,
        )
        .max(DVec2::ONE)
    }

    /// Screen position (logical, relative to the view rect) → world.
    fn to_world(&self, rect: Rect, pos: Pos2) -> DVec2 {
        let px = DVec2::new(
            ((pos.x - rect.min.x) * self.ppp) as f64,
            ((pos.y - rect.min.y) * self.ppp) as f64,
        );
        self.camera.screen_to_world(px, self.vp_px())
    }

    /// World → screen position (logical).
    pub fn to_screen(&self, rect: Rect, p: DVec2) -> Pos2 {
        let s = self.camera.world_to_screen(p, self.vp_px());
        Pos2::new(
            rect.min.x + (s.x as f32) / self.ppp,
            rect.min.y + (s.y as f32) / self.ppp,
        )
    }

    /// World units per logical point.
    pub fn units_per_point(&self) -> f64 {
        self.camera.units_per_px() * self.ppp as f64
    }

    pub fn zoom_extents(&mut self, ed: &Editor) {
        let bb = ed.extents();
        if bb.is_empty() {
            return;
        }
        let bb = if bb.size().max_element() <= 0.0 {
            bb.expanded(1.0)
        } else {
            bb
        };
        self.camera
            .fit_bbox(&bb, self.vp_px(), 24.0 * self.ppp as f64);
    }

    pub fn zoom_window(&mut self, b: &BBox2) {
        self.camera.fit_bbox(b, self.vp_px(), 4.0);
    }

    /// Show the viewport filling `ui`'s available space.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ed: &mut Editor,
        gpu: Option<&mut Gpu>,
        theme: Theme,
    ) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.size = rect.size();
        self.ppp = ui.ctx().pixels_per_point();
        if self.fit_pending && !ed.extents().is_empty() {
            self.zoom_extents(ed);
            self.fit_frames += 1;
            if self.fit_frames >= 2 {
                self.fit_pending = false;
                self.fit_frames = 0;
            }
        }
        self.handle_input(ui, rect, &resp, ed);
        ed.units_per_px = self.units_per_point();

        // Display lists.
        let params = DisplayParams {
            foreground: foreground(theme),
            units_per_px: self.camera.units_per_px(),
            show_lineweights: ed.draft.show_lineweights,
        };
        let dark = theme == Theme::Dark;
        if let Some(gpu) = gpu {
            if !self.pending.is_empty()
                || self.display.needs_rebuild_for(&params)
                || self.params.is_none()
            {
                let ch = std::mem::take(&mut self.pending);
                let delta = self.display.update(&ed.doc.drawing, &ch, &params);
                gpu.apply(delta);
                self.highlight_key = None;
            }
            self.params = Some(params);
            self.update_highlight(gpu, ed, &params, dark);
            self.update_grid(gpu, ed, dark);
            self.update_preview(gpu, ed, &params, dark);
            let size_px = [
                (rect.width() * self.ppp).round() as u32,
                (rect.height() * self.ppp).round() as u32,
            ];
            let tex = gpu.prepare(Slot::View2d, size_px);
            let bg = background(theme);
            let frame = Frame2D {
                background: rgb8(bg[0], bg[1], bg[2]),
                pixel_scale: self.ppp,
            };
            gpu.render_2d(&self.camera, &frame);
            ui.painter().image(
                tex,
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            let bg = background(theme);
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgb(bg[0], bg[1], bg[2]));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                core(ed.lang).gpu_unavailable,
                egui::FontId::proportional(14.0),
                Color32::GRAY,
            );
        }
        self.paint_overlays(ui, rect, &resp, ed, theme);
    }

    fn handle_input(&mut self, ui: &egui::Ui, rect: Rect, resp: &egui::Response, ed: &mut Editor) {
        let vp = self.vp_px();
        let ppp = self.ppp;
        let to_px = |p: Pos2| {
            DVec2::new(
                ((p.x - rect.min.x) * ppp) as f64,
                ((p.y - rect.min.y) * ppp) as f64,
            )
        };
        let (hover_pos, multi, wheel, zoom_delta, modifiers, any_touch, time, press_origin) = ui
            .input(|i| {
                let mut steps = 0.0f32;
                for e in &i.events {
                    if let egui::Event::MouseWheel { unit, delta, .. } = e {
                        steps += match unit {
                            egui::MouseWheelUnit::Point => delta.y / 50.0,
                            egui::MouseWheelUnit::Line => delta.y,
                            egui::MouseWheelUnit::Page => delta.y * 3.0,
                        };
                    }
                }
                (
                    i.pointer.hover_pos(),
                    i.multi_touch(),
                    steps,
                    i.zoom_delta(),
                    i.modifiers,
                    i.any_touches(),
                    i.time,
                    i.pointer.press_origin(),
                )
            });
        let inside = hover_pos.is_some_and(|p| rect.contains(p));

        // Wheel zoom about the cursor.
        if inside
            && resp.hovered()
            && let Some(p) = hover_pos
        {
            if wheel != 0.0 {
                self.camera
                    .zoom_about(to_px(p), 1.2f64.powf(wheel as f64), vp);
            }
            if multi.is_none() && (zoom_delta - 1.0).abs() > 1e-4 {
                self.camera.zoom_about(to_px(p), zoom_delta as f64, vp);
            }
        }
        // Two-finger pan + pinch.
        if let Some(mt) = multi
            && rect.contains(mt.center_pos)
        {
            self.camera.pan_px(
                DVec2::new(mt.translation_delta.x as f64, mt.translation_delta.y as f64)
                    * ppp as f64,
            );
            if (mt.zoom_delta - 1.0).abs() > 1e-4 {
                self.camera
                    .zoom_about(to_px(mt.center_pos), mt.zoom_delta as f64, vp);
            }
            self.drag = None;
            return;
        }

        // Middle / right drag pan; double middle click = zoom extents.
        let pan_button = resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary);
        if pan_button {
            let d = resp.drag_delta();
            self.camera
                .pan_px(DVec2::new(d.x as f64, d.y as f64) * ppp as f64);
        }
        if resp.clicked_by(egui::PointerButton::Middle) {
            if time - self.last_middle_click < 0.4 {
                self.zoom_extents(ed);
            }
            self.last_middle_click = time;
        }

        if let Some(p) = hover_pos.filter(|_| inside) {
            let w = self.to_world(rect, p);
            ed.units_per_px = self.units_per_point();
            ed.pointer_move(w);
        } else if resp.hover_pos().is_none() && ed.cursor.is_some() && self.drag.is_none() {
            ed.pointer_leave();
        }

        // Primary button: click / window drag / grip drag / touch pan.
        let shift = modifiers.shift;
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.press_pos = press_origin.or(resp.interact_pointer_pos());
            let start = self.press_pos.map(|p| self.to_world(rect, p));
            let accepts = ed.tool_accepts();
            self.drag = match start {
                _ if any_touch && !ed.has_tool() => Some(Drag::Pan),
                Some(w) if !ed.has_tool() && ed.grip_at(w).is_some() => {
                    ed.click(w, false); // starts the grip tool
                    Some(Drag::Grip)
                }
                Some(w) if !ed.has_tool() || accepts.selection => {
                    ed.window_start = None;
                    Some(Drag::Window(w))
                }
                _ => None,
            };
        }
        if resp.dragged_by(egui::PointerButton::Primary) && self.drag == Some(Drag::Pan) {
            let d = resp.drag_delta();
            self.camera
                .pan_px(DVec2::new(d.x as f64, d.y as f64) * ppp as f64);
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) {
            let end = resp
                .interact_pointer_pos()
                .or(hover_pos)
                .map(|p| self.to_world(rect, p));
            let moved = match (self.press_pos, resp.interact_pointer_pos()) {
                (Some(a), Some(b)) => a.distance(b) >= DRAG_THRESHOLD,
                _ => true,
            };
            match (self.drag.take(), end) {
                (Some(Drag::Window(a)), Some(b)) if moved => ed.window(a, b, shift),
                (Some(Drag::Window(a)), _) => ed.click(a, shift),
                (Some(Drag::Grip), Some(b)) if moved => ed.click(b, false),
                (None, Some(b)) => ed.click(b, shift),
                _ => {}
            }
            self.press_pos = None;
        }
        if resp.clicked_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            let w = self.to_world(rect, p);
            ed.click(w, shift);
        }
        if resp.clicked_by(egui::PointerButton::Secondary) && ed.has_tool() {
            ed.enter();
        }
        if !ed.has_tool() {
            resp.context_menu(|ui| {
                let s = core(ed.lang);
                if let Some(c) = ed.last_command()
                    && ui.button(format!("⟳ {c}")).clicked()
                {
                    ed.run_command(c);
                    ui.close();
                }
                if ui
                    .add_enabled(ed.doc.can_undo(), egui::Button::new(s.undo))
                    .clicked()
                {
                    ed.undo();
                    ui.close();
                }
                if ui
                    .add_enabled(ed.doc.can_redo(), egui::Button::new(s.redo))
                    .clicked()
                {
                    ed.redo();
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(!ed.selection.is_empty(), egui::Button::new(s.erase))
                    .clicked()
                {
                    ed.run_command("ERASE");
                    ui.close();
                }
                if ui.button(s.select_all).clicked() {
                    ed.select_all();
                    ui.close();
                }
                if ui.button(s.zoom_extents).clicked() {
                    self.zoom_extents(ed);
                    ui.close();
                }
            });
        }
        if inside && self.drag != Some(Drag::Pan) && !pan_button {
            ui.ctx().set_cursor_icon(CursorIcon::None);
        } else if pan_button {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
    }

    fn update_highlight(&mut self, gpu: &mut Gpu, ed: &Editor, params: &DisplayParams, dark: bool) {
        let key = (
            ed.selection.revision(),
            ed.doc.revision(),
            ed.hover,
            (params.units_per_px.log2() * 2.0) as i64,
        );
        if self.highlight_key == Some(key) {
            return;
        }
        self.highlight_key = Some(key);
        let sel = ed.selection.to_vec();
        let sel_color = if dark {
            rgba8(90, 160, 255, 150)
        } else {
            rgba8(0, 90, 220, 140)
        };
        if sel.is_empty() {
            gpu.renderer.remove_batch2d(SELECTION_BATCH);
        } else {
            let b = display::highlight_batch(&ed.doc.drawing, &sel, params, sel_color, 2.0);
            gpu.renderer.upload_batch2d(SELECTION_BATCH, &b);
            gpu.renderer.set_batch2d_style(
                SELECTION_BATCH,
                Batch2DStyle {
                    color: None,
                    opacity: 1.0,
                },
            );
        }
        match ed.hover.filter(|h| !ed.selection.contains(*h)) {
            Some(h) => {
                let c = if dark {
                    rgba8(255, 255, 255, 90)
                } else {
                    rgba8(0, 0, 0, 70)
                };
                let b = display::highlight_batch(&ed.doc.drawing, &[h], params, c, 2.0);
                gpu.renderer.upload_batch2d(HOVER_BATCH, &b);
            }
            None => {
                gpu.renderer.remove_batch2d(HOVER_BATCH);
            }
        }
    }

    fn update_grid(&mut self, gpu: &mut Gpu, ed: &Editor, dark: bool) {
        if !ed.draft.grid_on {
            if self.grid_key.take().is_some() {
                gpu.renderer.remove_batch2d(GRID_BATCH);
            }
            return;
        }
        let vis = self.camera.visible_bbox(self.vp_px());
        let spacing = ed.doc.drawing.tables.settings.grid_spacing;
        let upp = self.camera.units_per_px();
        let level = (upp.log2() * 2.0).round();
        if let Some((area, lvl, d)) = self.grid_key
            && area.contains_box(&vis)
            && lvl == level
            && d == dark
        {
            return;
        }
        let area = vis.expanded(vis.size().max_element());
        let (b, _) = display::grid_batch(&area, spacing, upp, dark);
        gpu.renderer.upload_batch2d(GRID_BATCH, &b);
        self.grid_key = Some((area, level, dark));
    }

    fn update_preview(&mut self, gpu: &mut Gpu, ed: &Editor, params: &DisplayParams, dark: bool) {
        if ed.preview.is_empty() {
            if self.preview_shown {
                gpu.renderer.clear_overlay2d();
                self.preview_shown = false;
            }
            return;
        }
        let color = if dark {
            rgba8(255, 214, 90, 230)
        } else {
            rgba8(200, 110, 0, 230)
        };
        let b = display::preview_batch(
            &ed.doc.drawing,
            &ed.preview,
            ed.tool_base_point(),
            ed.cursor.map(|c| c.point),
            params,
            color,
        );
        gpu.renderer.set_overlay2d(&b);
        self.preview_shown = true;
    }

    fn paint_overlays(
        &self,
        ui: &egui::Ui,
        rect: Rect,
        resp: &egui::Response,
        ed: &Editor,
        theme: Theme,
    ) {
        let painter = ui.painter_at(rect);
        let dark = theme == Theme::Dark;
        let fg = if dark {
            Color32::from_gray(230)
        } else {
            Color32::from_gray(20)
        };
        let accent = Color32::from_rgb(255, 200, 40);

        // Grips of the selection.
        if ed.selection.len() <= 200 {
            let gs = ed.draft.grip_px;
            let hover_grip = ed.cursor.and_then(|c| ed.grip_at(c.raw)).map(|g| g.2);
            for id in ed.selection.iter() {
                if let Some(e) = ed.doc.drawing.entities.get(&id) {
                    for g in select::grips(&e.kind) {
                        let p = self.to_screen(rect, g);
                        if !rect.contains(p) {
                            continue;
                        }
                        let col = if hover_grip == Some(g) {
                            Color32::from_rgb(255, 60, 60)
                        } else {
                            Color32::from_rgb(40, 120, 255)
                        };
                        let r = Rect::from_center_size(p, Vec2::splat(gs * 2.0));
                        painter.rect_filled(r, 0.0, col);
                        painter.rect_stroke(
                            r,
                            0.0,
                            Stroke::new(1.0, Color32::WHITE),
                            StrokeKind::Outside,
                        );
                    }
                }
            }
        }

        // Selection window (drag or click-click).
        let window_start = match self.drag {
            Some(Drag::Window(a)) => Some(a),
            _ => ed.window_start,
        };
        if let (Some(a), Some(c)) = (
            window_start,
            ed.cursor
                .map(|c| c.raw)
                .or_else(|| resp.hover_pos().map(|p| self.to_world(rect, p))),
        ) {
            let pa = self.to_screen(rect, a);
            let pc = self.to_screen(rect, c);
            let r = Rect::from_two_pos(pa, pc);
            let crossing = c.x < a.x;
            let (fill, stroke) = if crossing {
                (
                    Color32::from_rgba_unmultiplied(60, 200, 90, 40),
                    Color32::from_rgb(60, 200, 90),
                )
            } else {
                (
                    Color32::from_rgba_unmultiplied(60, 120, 255, 40),
                    Color32::from_rgb(80, 140, 255),
                )
            };
            painter.rect_filled(r, 0.0, fill);
            if crossing {
                let pts = [
                    r.left_top(),
                    r.right_top(),
                    r.right_bottom(),
                    r.left_bottom(),
                    r.left_top(),
                ];
                for w in pts.windows(2) {
                    painter.extend(egui::Shape::dashed_line(
                        &[w[0], w[1]],
                        Stroke::new(1.0, stroke),
                        5.0,
                        3.0,
                    ));
                }
            } else {
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, stroke), StrokeKind::Inside);
            }
        }

        // Tracking ray.
        if let Some(c) = ed.cursor
            && let Some((base, angle)) = c.tracking
        {
            let a = self.to_screen(rect, base);
            let far = base + DVec2::from_angle(angle) * self.units_per_point() * 4000.0;
            let b = self.to_screen(rect, far);
            painter.extend(egui::Shape::dashed_line(
                &[a, b],
                Stroke::new(1.0, Color32::from_rgb(80, 200, 120)),
                6.0,
                4.0,
            ));
            let deg = angle.to_degrees().rem_euclid(360.0);
            let d = c.point.distance(base);
            painter.text(
                self.to_screen(rect, c.point) + Vec2::new(14.0, 14.0),
                egui::Align2::LEFT_TOP,
                format!("{}: {d:.4} < {deg:.0}°", core(ed.lang).snap_polar),
                egui::FontId::proportional(12.0),
                Color32::from_rgb(80, 200, 120),
            );
        }

        // Crosshair + pick box, snap marker.
        if let Some(c) = ed.cursor
            && resp.hover_pos().is_some()
        {
            let p = self.to_screen(rect, c.raw);
            let stroke = Stroke::new(1.0, fg.gamma_multiply(0.85));
            let arm = 40.0f32.max(rect.width().max(rect.height()) * 0.04);
            painter.line_segment([p - Vec2::new(arm, 0.0), p + Vec2::new(arm, 0.0)], stroke);
            painter.line_segment([p - Vec2::new(0.0, arm), p + Vec2::new(0.0, arm)], stroke);
            let accepts = ed.tool_accepts();
            if !accepts.point || accepts.selection || accepts.pick {
                let pb = ed.draft.pickbox_px;
                painter.rect_stroke(
                    Rect::from_center_size(p, Vec2::splat(pb * 2.0)),
                    0.0,
                    stroke,
                    StrokeKind::Middle,
                );
            }
            if let Some(s) = c.snap {
                let q = self.to_screen(rect, s.p);
                paint_snap_marker(&painter, q, s.kind, accent);
                painter.text(
                    q + Vec2::new(12.0, -14.0),
                    egui::Align2::LEFT_BOTTOM,
                    s.kind.label(ed.lang),
                    egui::FontId::proportional(12.0),
                    accent,
                );
            }
        }

        // UCS icon (bottom-left).
        let o = rect.left_bottom() + Vec2::new(24.0, -24.0);
        let len = 36.0;
        painter.arrow(
            o,
            Vec2::new(len, 0.0),
            Stroke::new(1.5, Color32::from_rgb(230, 80, 80)),
        );
        painter.arrow(
            o,
            Vec2::new(0.0, -len),
            Stroke::new(1.5, Color32::from_rgb(80, 200, 100)),
        );
        painter.text(
            o + Vec2::new(len + 4.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "X",
            egui::FontId::proportional(11.0),
            fg,
        );
        painter.text(
            o + Vec2::new(0.0, -len - 4.0),
            egui::Align2::CENTER_BOTTOM,
            "Y",
            egui::FontId::proportional(11.0),
            fg,
        );
        painter.rect_stroke(
            Rect::from_center_size(o, Vec2::splat(6.0)),
            0.0,
            Stroke::new(1.0, fg),
            StrokeKind::Middle,
        );
    }
}

/// AutoCAD-style snap marker glyphs.
pub fn paint_snap_marker(p: &egui::Painter, c: Pos2, kind: OsnapKind, color: Color32) {
    let s = 6.0;
    let st = Stroke::new(2.0, color);
    let sq = Rect::from_center_size(c, Vec2::splat(2.0 * s));
    match kind {
        OsnapKind::Endpoint => {
            p.rect_stroke(sq, 0.0, st, StrokeKind::Middle);
        }
        OsnapKind::Midpoint => {
            let pts = vec![
                c + Vec2::new(0.0, -s),
                c + Vec2::new(s, s * 0.8),
                c + Vec2::new(-s, s * 0.8),
            ];
            p.add(egui::Shape::closed_line(pts, st));
        }
        OsnapKind::Center => {
            p.circle_stroke(c, s, st);
        }
        OsnapKind::Quadrant => {
            let pts = vec![
                c + Vec2::new(0.0, -s),
                c + Vec2::new(s, 0.0),
                c + Vec2::new(0.0, s),
                c + Vec2::new(-s, 0.0),
            ];
            p.add(egui::Shape::closed_line(pts, st));
        }
        OsnapKind::Intersection => {
            p.line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], st);
            p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, -s)], st);
        }
        OsnapKind::Perpendicular => {
            p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, s)], st);
            p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(-s, -s)], st);
            p.line_segment([c + Vec2::new(-s, 0.0), c + Vec2::new(0.0, 0.0)], st);
            p.line_segment([c + Vec2::new(0.0, 0.0), c + Vec2::new(0.0, s)], st);
        }
        OsnapKind::Tangent => {
            p.circle_stroke(c, s * 0.8, st);
            p.line_segment(
                [c + Vec2::new(-s, -s * 0.8), c + Vec2::new(s, -s * 0.8)],
                st,
            );
        }
        OsnapKind::Nearest => {
            let pts = vec![
                c + Vec2::new(-s, -s),
                c + Vec2::new(s, -s),
                c + Vec2::new(-s, s),
                c + Vec2::new(s, s),
            ];
            p.add(egui::Shape::closed_line(pts, st));
        }
        OsnapKind::Node => {
            p.circle_stroke(c, s, st);
            p.line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], st);
            p.line_segment([c + Vec2::new(-s, s), c + Vec2::new(s, -s)], st);
        }
        OsnapKind::Grid | OsnapKind::Polar => {
            p.line_segment(
                [c + Vec2::new(-s * 0.6, 0.0), c + Vec2::new(s * 0.6, 0.0)],
                Stroke::new(1.0, color),
            );
            p.line_segment(
                [c + Vec2::new(0.0, -s * 0.6), c + Vec2::new(0.0, s * 0.6)],
                Stroke::new(1.0, color),
            );
        }
    }
}
