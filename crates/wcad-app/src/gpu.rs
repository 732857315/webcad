//! GPU glue: the `wcad_render::Renderer`, offscreen viewports registered as egui native textures,
//! and application of [`crate::display::DisplayDelta`]s. Absent when eframe runs without wgpu (or
//! in headless UI tests): the views then draw only their egui overlays.

use eframe::egui_wgpu::{self, wgpu};
use wcad_render::{Camera2D, Camera3D, Frame2D, Frame3D, Renderer, Viewport};

use crate::display::DisplayDelta;

/// Viewport slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    View2d = 0,
    View3d = 1,
}

struct GpuView {
    vp: Viewport,
    tex: egui::TextureId,
}

pub struct Gpu {
    pub rs: egui_wgpu::RenderState,
    pub renderer: Renderer,
    samples: u32,
    views: [Option<GpuView>; 2],
}

impl Gpu {
    pub fn new(rs: &egui_wgpu::RenderState) -> Self {
        let mut renderer = Renderer::new(&rs.device, &rs.queue);
        let samples = wcad_render::choose_sample_count(&rs.adapter, 4);
        renderer.warm_up(samples);
        let info = rs.adapter.get_info();
        log::info!(
            "wcad-render on {:?} {} (MSAA x{samples})",
            info.backend,
            info.name
        );
        Self {
            rs: rs.clone(),
            renderer,
            samples,
            views: [None, None],
        }
    }

    /// Backend summary for the about box.
    pub fn backend_info(&self) -> String {
        let info = self.rs.adapter.get_info();
        format!(
            "{:?} · {} · MSAA ×{}",
            info.backend, info.name, self.samples
        )
    }

    /// Make sure `slot` has a viewport of `size_px` and return its texture id.
    pub fn prepare(&mut self, slot: Slot, size_px: [u32; 2]) -> egui::TextureId {
        let size = [size_px[0].clamp(1, 8192), size_px[1].clamp(1, 8192)];
        let device = &self.rs.device;
        match &mut self.views[slot as usize] {
            Some(v) => {
                if v.vp.resize(device, size) {
                    self.rs
                        .renderer
                        .write()
                        .update_egui_texture_from_wgpu_texture(
                            device,
                            v.vp.color_view(),
                            wgpu::FilterMode::Nearest,
                            v.tex,
                        );
                }
                v.tex
            }
            slot_ref @ None => {
                let vp = Viewport::new(device, size, self.samples);
                let tex = self.rs.renderer.write().register_native_texture(
                    device,
                    vp.color_view(),
                    wgpu::FilterMode::Nearest,
                );
                *slot_ref = Some(GpuView { vp, tex });
                tex
            }
        }
    }

    pub fn apply(&mut self, delta: DisplayDelta) {
        for id in delta.remove {
            self.renderer.remove_batch2d(id);
        }
        for (id, b) in &delta.upload {
            self.renderer.upload_batch2d(*id, b);
        }
        for (id, vis) in delta.visibility {
            self.renderer.set_batch2d_visible(id, vis);
        }
    }

    /// Render the 2D scene into `slot` if anything changed.
    pub fn render_2d(&mut self, cam: &Camera2D, frame: &Frame2D) {
        let Some(v) = &self.views[Slot::View2d as usize] else {
            return;
        };
        if !self.renderer.needs_render_2d(&v.vp, cam, frame) {
            return;
        }
        let mut enc = self
            .rs
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wcad 2d"),
            });
        self.renderer
            .render_2d(&self.rs.device, &self.rs.queue, &mut enc, &v.vp, cam, frame);
        self.rs.queue.submit([enc.finish()]);
    }

    /// Render the 3D scene into `slot` if anything changed.
    pub fn render_3d(&mut self, cam: &Camera3D, frame: &Frame3D) {
        let Some(v) = &self.views[Slot::View3d as usize] else {
            return;
        };
        if !self.renderer.needs_render_3d(&v.vp, cam, frame) {
            return;
        }
        let mut enc = self
            .rs
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("wcad 3d"),
            });
        self.renderer
            .render_3d(&self.rs.device, &self.rs.queue, &mut enc, &v.vp, cam, frame);
        self.rs.queue.submit([enc.finish()]);
    }
}
