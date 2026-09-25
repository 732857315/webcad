//! Offscreen render target: resolved color texture (shown by the app as an egui native texture),
//! optional MSAA color, and depth.

use std::sync::atomic::{AtomicU64, Ordering};

use wcad_math::DVec2;

/// Resolved color format. `egui_wgpu::Renderer::register_native_texture` samples it as gamma-space color.
pub const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Depth format (reverse-Z: cleared to 0, larger = nearer).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Largest sample count `<= wanted` supported by the adapter for both [`COLOR_FORMAT`] and
/// [`DEPTH_FORMAT`] (always at least 1). WebGPU guarantees 4 for these formats; WebGL2 usually too.
pub fn choose_sample_count(adapter: &wgpu::Adapter, wanted: u32) -> u32 {
    let color = adapter.get_texture_format_features(COLOR_FORMAT).flags;
    let depth = adapter.get_texture_format_features(DEPTH_FORMAT).flags;
    [16, 8, 4, 2]
        .into_iter()
        .find(|&n| n <= wanted && color.sample_count_supported(n) && depth.sample_count_supported(n))
        .unwrap_or(1)
}

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// One offscreen viewport. Create one per on-screen view; the renderer keeps no per-viewport state
/// except the "last rendered" fingerprint stored here for dirty tracking.
pub struct Viewport {
    uid: u64,
    size: [u32; 2],
    sample_count: u32,
    generation: u64,
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    msaa_view: Option<wgpu::TextureView>,
    depth_view: wgpu::TextureView,
    /// Fingerprint of the last rendered frame (0 = none / dirty).
    rendered: AtomicU64,
}

impl std::fmt::Debug for Viewport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewport")
            .field("uid", &self.uid)
            .field("size", &self.size)
            .field("sample_count", &self.sample_count)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

struct Targets {
    color: wgpu::Texture,
    color_view: wgpu::TextureView,
    msaa_view: Option<wgpu::TextureView>,
    depth_view: wgpu::TextureView,
}

fn clamp_size(device: &wgpu::Device, size: [u32; 2]) -> [u32; 2] {
    let max = device.limits().max_texture_dimension_2d.max(1);
    [size[0].clamp(1, max), size[1].clamp(1, max)]
}

fn make_targets(device: &wgpu::Device, size: [u32; 2], samples: u32) -> Targets {
    let extent = wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 };
    let tex = |label: &str, format, sample_count, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent,
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = tex(
        "wcad_viewport_color",
        COLOR_FORMAT,
        1,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let msaa_view = (samples > 1).then(|| {
        tex("wcad_viewport_msaa", COLOR_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .create_view(&Default::default())
    });
    let depth_view = tex("wcad_viewport_depth", DEPTH_FORMAT, samples, wgpu::TextureUsages::RENDER_ATTACHMENT)
        .create_view(&Default::default());
    let color_view = color.create_view(&Default::default());
    Targets { color, color_view, msaa_view, depth_view }
}

impl Viewport {
    /// Creates the targets. `size_px` is clamped to `1..=max_texture_dimension_2d`; `sample_count` should
    /// come from [`choose_sample_count`] (only 1 and 4 are portable; other values fall back to 4 or 1).
    pub fn new(device: &wgpu::Device, size_px: [u32; 2], sample_count: u32) -> Self {
        let sample_count = match sample_count {
            0 | 1 => 1,
            2 | 4 | 8 | 16 => sample_count,
            _ => 4,
        };
        let size = clamp_size(device, size_px);
        let t = make_targets(device, size, sample_count);
        Self {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            size,
            sample_count,
            generation: 1,
            color: t.color,
            color_view: t.color_view,
            msaa_view: t.msaa_view,
            depth_view: t.depth_view,
            rendered: AtomicU64::new(0),
        }
    }

    /// Recreates the targets if the (clamped) size changed. Returns `true` when the textures were
    /// replaced: the app must then re-point its egui texture (`update_egui_texture_from_wgpu_texture`).
    pub fn resize(&mut self, device: &wgpu::Device, size_px: [u32; 2]) -> bool {
        let size = clamp_size(device, size_px);
        if size == self.size {
            return false;
        }
        let t = make_targets(device, size, self.sample_count);
        self.size = size;
        self.color = t.color;
        self.color_view = t.color_view;
        self.msaa_view = t.msaa_view;
        self.depth_view = t.depth_view;
        self.generation += 1;
        self.mark_dirty();
        true
    }

    /// Size in physical pixels.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    pub fn size_f64(&self) -> DVec2 {
        DVec2::new(self.size[0] as f64, self.size[1] as f64)
    }

    pub fn aspect(&self) -> f64 {
        self.size[0] as f64 / self.size[1].max(1) as f64
    }

    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    /// Increments every time the textures are recreated.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Unique id of this viewport (stable for its lifetime).
    pub fn uid(&self) -> u64 {
        self.uid
    }

    /// The resolved color texture (`Rgba8Unorm`, usable as texture binding and copy source).
    pub fn color_texture(&self) -> &wgpu::Texture {
        &self.color
    }

    /// View to register with egui (`register_native_texture`).
    pub fn color_view(&self) -> &wgpu::TextureView {
        &self.color_view
    }

    /// Forces the next `needs_render_*` check to return `true`.
    pub fn mark_dirty(&self) {
        self.rendered.store(0, Ordering::Relaxed);
    }

    /// `true` until something has been rendered since creation, the last resize or [`Self::mark_dirty`].
    pub fn needs_redraw(&self) -> bool {
        self.rendered.load(Ordering::Relaxed) == 0
    }

    pub(crate) fn rendered_key(&self) -> u64 {
        self.rendered.load(Ordering::Relaxed)
    }

    pub(crate) fn set_rendered_key(&self, key: u64) {
        self.rendered.store(key.max(1), Ordering::Relaxed);
    }

    /// `(attachment view, resolve target)` for the color attachment.
    pub(crate) fn color_attachment(&self) -> (&wgpu::TextureView, Option<&wgpu::TextureView>) {
        match &self.msaa_view {
            Some(msaa) => (msaa, Some(&self.color_view)),
            None => (&self.color_view, None),
        }
    }

    pub(crate) fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }
}
