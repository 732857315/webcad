//! The modeling viewport: regenerates the part (`wcad_solid::regenerate`) when it changes, uploads
//! body meshes, edges and sketch curves, and navigates the 3D camera (orbit/pan/zoom, touch,
//! standard views). Feature UI is added by the modeling package through [`View3dHook`]s.

use egui::{Color32, Pos2, Rect, Sense, Vec2};
use wcad_doc::FeatureKind;
use wcad_math::{BBox3, DVec2, DVec3};
use wcad_render::{
    Camera3D, Frame3D, Grid3D, LineSet3D, LineSetId, LineStyle3D, MeshData, MeshId, MeshStyle,
    Ray3, StandardView,
};
use wcad_solid::{RegenCache, RegenResult};

use crate::editor::Editor;
use crate::gpu::{Gpu, Slot};
use crate::i18n::{core, fmt};
use crate::settings::Theme;

/// Context handed to [`View3dHook`]s every frame the modeling viewport is shown.
pub struct View3dCx<'a> {
    pub ui: &'a mut egui::Ui,
    pub rect: Rect,
    pub response: &'a egui::Response,
    pub camera: &'a mut Camera3D,
    pub editor: &'a mut Editor,
    pub regen: &'a RegenResult,
    pub pixels_per_point: f32,
    /// Set to `true` to stop the default primary-drag orbit this frame.
    pub consume_primary: bool,
}

impl View3dCx<'_> {
    /// Viewport size in physical pixels.
    pub fn viewport_px(&self) -> DVec2 {
        DVec2::new(
            (self.rect.width() * self.pixels_per_point) as f64,
            (self.rect.height() * self.pixels_per_point) as f64,
        )
    }
    /// Pick ray through a screen position (logical points).
    pub fn ray(&self, pos: Pos2) -> Ray3 {
        let px = DVec2::new(
            ((pos.x - self.rect.min.x) * self.pixels_per_point) as f64,
            ((pos.y - self.rect.min.y) * self.pixels_per_point) as f64,
        );
        self.camera.ray_from_screen(px, self.viewport_px())
    }
    /// World → screen (logical points), `None` behind the camera.
    pub fn to_screen(&self, p: DVec3) -> Option<Pos2> {
        let s = self.camera.world_to_screen(p, self.viewport_px())?;
        Some(Pos2::new(
            self.rect.min.x + s.x as f32 / self.pixels_per_point,
            self.rect.min.y + s.y as f32 / self.pixels_per_point,
        ))
    }
}

/// A modeling-viewport extension (see [`crate::modeling`]).
pub type View3dHook = fn(&mut View3dCx<'_>);

/// Mesh/line-set ids: bodies use `i`, edges `i`, sketches `SKETCH_BASE + i`.
const SKETCH_BASE: u64 = 1 << 32;

pub struct View3d {
    pub camera: Camera3D,
    pub regen: RegenResult,
    cache: RegenCache,
    part_dirty: bool,
    uploaded_meshes: Vec<MeshId>,
    uploaded_lines: Vec<LineSetId>,
    gpu_dirty: bool,
    pub fit_pending: bool,
    last_middle_click: f64,
    bbox: BBox3,
}

impl Default for View3d {
    fn default() -> Self {
        let mut camera = Camera3D::default();
        camera.set_view(StandardView::IsoSE);
        Self {
            camera,
            regen: RegenResult::default(),
            cache: RegenCache::new(),
            part_dirty: true,
            uploaded_meshes: Vec::new(),
            uploaded_lines: Vec::new(),
            gpu_dirty: true,
            fit_pending: true,
            last_middle_click: -10.0,
            bbox: BBox3::EMPTY,
        }
    }
}

impl View3d {
    pub fn mark_part_dirty(&mut self) {
        self.part_dirty = true;
    }

    /// Regenerate the part if it changed (runs even when the view is hidden, e.g. for tests).
    pub fn regenerate_if_needed(&mut self, ed: &Editor) {
        if !self.part_dirty {
            return;
        }
        self.part_dirty = false;
        self.regen = wcad_solid::regenerate(&ed.doc.part, &mut self.cache);
        self.gpu_dirty = true;
    }

    fn upload(&mut self, gpu: &mut Gpu, ed: &Editor, dark: bool) {
        if !self.gpu_dirty {
            return;
        }
        self.gpu_dirty = false;
        for id in self.uploaded_meshes.drain(..) {
            gpu.renderer.remove_mesh(id);
        }
        for id in self.uploaded_lines.drain(..) {
            gpu.renderer.remove_lines3d(id);
        }
        let mut bbox = BBox3::EMPTY;
        let body_color = if dark {
            [0.70, 0.74, 0.80, 1.0]
        } else {
            [0.62, 0.66, 0.72, 1.0]
        };
        let edge_color = if dark {
            [0.08, 0.09, 0.11, 1.0]
        } else {
            [0.15, 0.15, 0.18, 1.0]
        };
        for (i, body) in self.regen.bodies.iter().enumerate() {
            let Ok(tm) = wcad_solid::tessellate(body, 0.0) else {
                continue;
            };
            let mesh = MeshData {
                positions: tm.positions.clone(),
                normals: tm.normals.clone(),
                indices: tm.indices.clone(),
            };
            bbox = bbox.union(&mesh.bbox());
            let id = MeshId(i as u64);
            gpu.renderer.upload_mesh(id, &mesh);
            let color = if body.is_exact() {
                body_color
            } else {
                [0.80, 0.66, 0.50, 1.0]
            };
            gpu.renderer.set_mesh_style(
                id,
                MeshStyle {
                    color,
                    ..Default::default()
                },
            );
            self.uploaded_meshes.push(id);
            let mut ls = LineSet3D::new();
            for e in &tm.edges {
                ls.push_polyline(e, edge_color);
            }
            let lid = LineSetId(i as u64);
            gpu.renderer.upload_lines3d(lid, &ls);
            gpu.renderer.set_lines3d_style(
                lid,
                LineStyle3D {
                    width_px: 1.5,
                    ..Default::default()
                },
            );
            self.uploaded_lines.push(lid);
        }
        // Sketch curves on their planes.
        let sketch_color = [0.25, 0.65, 1.0, 1.0];
        for (i, f) in ed.doc.part.active_features().enumerate() {
            let FeatureKind::Sketch { sketch, .. } = &f.kind else {
                continue;
            };
            let Some((_, plane)) = self.regen.sketch_planes.iter().find(|(id, _)| *id == f.id)
            else {
                continue;
            };
            let mut ls = LineSet3D::new();
            for (_, c) in sketch.curves() {
                let tol = (wcad_geom2d::Curve::bbox(&c).size().max_element() * 2e-3).max(1e-6);
                ls.push_plane_polyline(plane, &wcad_geom2d::Curve::flatten(&c, tol), sketch_color);
            }
            if ls.is_empty() {
                continue;
            }
            bbox = bbox.union(&ls.bbox());
            let lid = LineSetId(SKETCH_BASE + i as u64);
            gpu.renderer.upload_lines3d(lid, &ls);
            gpu.renderer.set_lines3d_style(
                lid,
                LineStyle3D {
                    width_px: 2.0,
                    ..Default::default()
                },
            );
            self.uploaded_lines.push(lid);
        }
        self.bbox = bbox;
    }

    pub fn fit(&mut self, aspect: f64) {
        if !self.bbox.is_empty() {
            self.camera.fit_bbox(&self.bbox, aspect);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ed: &mut Editor,
        mut gpu: Option<&mut Gpu>,
        theme: Theme,
        hooks: &[View3dHook],
    ) {
        self.regenerate_if_needed(ed);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let ppp = ui.ctx().pixels_per_point();
        let vp =
            DVec2::new((rect.width() * ppp) as f64, (rect.height() * ppp) as f64).max(DVec2::ONE);
        let dark = theme == Theme::Dark;
        if let Some(gpu) = gpu.as_deref_mut() {
            self.upload(gpu, ed, dark);
        }
        if self.fit_pending && !self.bbox.is_empty() {
            self.fit(vp.x / vp.y);
            self.fit_pending = false;
        }

        // Extension hooks first (they may consume the primary drag).
        let mut consume = false;
        {
            let regen = std::mem::take(&mut self.regen);
            let mut cx = View3dCx {
                ui,
                rect,
                response: &resp,
                camera: &mut self.camera,
                editor: ed,
                regen: &regen,
                pixels_per_point: ppp,
                consume_primary: false,
            };
            for h in hooks {
                h(&mut cx);
            }
            consume |= cx.consume_primary;
            self.regen = regen;
        }
        self.navigate(ui, rect, &resp, consume, vp, ppp);

        if let Some(gpu) = gpu {
            let tex = gpu.prepare(Slot::View3d, [vp.x as u32, vp.y as u32]);
            let spacing = grid_spacing(self.camera.distance);
            let mut frame = Frame3D {
                grid: Some(Grid3D {
                    spacing,
                    radius: spacing * 200.0,
                    ..Default::default()
                }),
                pixel_scale: ppp,
                ..Default::default()
            };
            if !dark {
                frame.background_top = [0.93, 0.94, 0.96, 1.0];
                frame.background_bottom = [0.78, 0.80, 0.84, 1.0];
            }
            gpu.render_3d(&self.camera, &frame);
            ui.painter().image(
                tex,
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgb(40, 44, 52));
        }
        self.overlay(ui, rect, ed, vp);
    }

    fn navigate(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        resp: &egui::Response,
        consume: bool,
        vp: DVec2,
        ppp: f32,
    ) {
        let to_px = |p: Pos2| {
            DVec2::new(
                ((p.x - rect.min.x) * ppp) as f64,
                ((p.y - rect.min.y) * ppp) as f64,
            )
        };
        let (hover, wheel, multi, time, shift) = ui.input(|i| {
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
                steps,
                i.multi_touch(),
                i.time,
                i.modifiers.shift,
            )
        });
        if let Some(mt) = multi
            && rect.contains(mt.center_pos)
        {
            self.camera.pan_px(
                DVec2::new(mt.translation_delta.x as f64, mt.translation_delta.y as f64)
                    * ppp as f64,
                vp,
            );
            if (mt.zoom_delta - 1.0).abs() > 1e-4 {
                self.camera
                    .zoom_about(to_px(mt.center_pos), mt.zoom_delta as f64, vp);
            }
            return;
        }
        if resp.hovered()
            && wheel != 0.0
            && let Some(p) = hover
        {
            self.camera
                .zoom_about(to_px(p), 1.2f64.powf(wheel as f64), vp);
        }
        let d = resp.drag_delta();
        let dd = DVec2::new(d.x as f64, d.y as f64);
        if (resp.dragged_by(egui::PointerButton::Primary) && !consume && !shift)
            || (resp.dragged_by(egui::PointerButton::Middle) && shift)
        {
            self.camera.orbit(-dd.x * 0.008, dd.y * 0.008);
        } else if resp.dragged_by(egui::PointerButton::Middle)
            || resp.dragged_by(egui::PointerButton::Secondary)
            || (resp.dragged_by(egui::PointerButton::Primary) && shift)
        {
            self.camera.pan_px(dd * ppp as f64, vp);
        }
        if resp.clicked_by(egui::PointerButton::Middle) {
            if time - self.last_middle_click < 0.4 {
                self.fit(vp.x / vp.y);
            }
            self.last_middle_click = time;
        }
    }

    fn overlay(&mut self, ui: &mut egui::Ui, rect: Rect, ed: &Editor, vp: DVec2) {
        let s = core(ed.lang);
        // Standard views toolbar (top-right).
        let bar = Rect::from_min_size(
            rect.right_top() + Vec2::new(-330.0, 6.0),
            Vec2::new(324.0, 28.0),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        egui::Frame::popup(child.style())
            .inner_margin(2.0)
            .show(&mut child, |ui| {
                ui.horizontal(|ui| {
                    for (label, view) in [
                        (s.view_iso, Some(StandardView::IsoSE)),
                        (s.view_right, Some(StandardView::Right)),
                        (s.view_front, Some(StandardView::Front)),
                        (s.view_top, Some(StandardView::Top)),
                        (s.fit_all, None),
                    ] {
                        if ui.small_button(label).clicked() {
                            match view {
                                Some(v) => {
                                    self.camera.set_view(v);
                                    self.fit(vp.x / vp.y);
                                }
                                None => self.fit(vp.x / vp.y),
                            }
                        }
                    }
                });
            });
        let painter = ui.painter_at(rect);
        let failed = self
            .regen
            .feature_status
            .iter()
            .filter(|(_, st)| st.is_error())
            .count();
        let msg = if self.regen.bodies.is_empty() {
            s.no_bodies.to_owned()
        } else {
            fmt(s.bodies_n, &[&self.regen.bodies.len()])
        };
        painter.text(
            rect.left_top() + Vec2::new(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            msg,
            egui::FontId::proportional(13.0),
            Color32::from_gray(220),
        );
        if failed > 0 {
            painter.text(
                rect.left_top() + Vec2::new(10.0, 30.0),
                egui::Align2::LEFT_TOP,
                fmt(s.regen_failed, &[&failed]),
                egui::FontId::proportional(13.0),
                Color32::from_rgb(255, 120, 100),
            );
        }
        // Axis triad (bottom-left).
        let o = rect.left_bottom() + Vec2::new(40.0, -40.0);
        for (axis, col, name) in [
            (DVec3::X, Color32::from_rgb(230, 80, 80), "X"),
            (DVec3::Y, Color32::from_rgb(80, 200, 100), "Y"),
            (DVec3::Z, Color32::from_rgb(90, 140, 255), "Z"),
        ] {
            let v = Vec2::new(
                axis.dot(self.camera.right()) as f32,
                -axis.dot(self.camera.up()) as f32,
            ) * 28.0;
            painter.arrow(o, v, egui::Stroke::new(2.0, col));
            painter.text(
                o + v * 1.25,
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(11.0),
                col,
            );
        }
    }
}

/// Grid spacing: a power of ten giving a few dozen cells across the view.
fn grid_spacing(distance: f64) -> f64 {
    let d = if distance.is_finite() && distance > 0.0 {
        distance
    } else {
        100.0
    };
    10f64.powf((d / 20.0).log10().floor()).max(1e-6)
}
