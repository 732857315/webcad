//! The retained-mode renderer: GPU copies of the display lists, pipelines, and the 2D/3D passes.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use wcad_math::{BBox2, BBox3, DMat4, DVec2, DVec3};
use wgpu::util::DeviceExt as _;

use crate::batch2d::{Batch2D, FillVertex, LineSegment2D, PointMarker};
use crate::camera::{Camera2D, Camera3D};
use crate::scene3d::{Grid3D, LineSegment3D, LineSet3D, LineStyle3D, MeshData, MeshStyle, normalize_ranges};
use crate::viewport::{COLOR_FORMAT, DEPTH_FORMAT, Viewport};
use crate::{Rgba, shader_modules};

/// Id of an uploaded 2D batch. Batches are drawn in ascending id order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BatchId(pub u64);

/// Id of an uploaded mesh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshId(pub u64);

/// Id of an uploaded 3D line set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LineSetId(pub u64);

/// Per-batch display style, applied at draw time without re-uploading (hover/selection highlight of a
/// whole batch, dimming of inactive layers).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Batch2DStyle {
    /// Replaces every vertex color of the batch when set.
    pub color: Option<Rgba>,
    /// Multiplies the alpha of everything in the batch (`0..=1`).
    pub opacity: f32,
}

impl Default for Batch2DStyle {
    fn default() -> Self {
        Self { color: None, opacity: 1.0 }
    }
}

/// Per-render parameters of a 2D view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame2D {
    /// Clear color (sRGB, straight alpha; use an opaque color for the main viewport).
    pub background: Rgba,
    /// Multiplies line widths and point sizes (pass egui's `pixels_per_point`).
    pub pixel_scale: f32,
}

impl Default for Frame2D {
    fn default() -> Self {
        Self { background: [0.13, 0.14, 0.16, 1.0], pixel_scale: 1.0 }
    }
}

/// Per-render parameters of a 3D view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame3D {
    /// Vertical background gradient (sRGB, straight alpha).
    pub background_top: Rgba,
    pub background_bottom: Rgba,
    /// Grid drawn on a plane (ground or sketch plane).
    pub grid: Option<Grid3D>,
    /// Facet normals from screen derivatives instead of the uploaded vertex normals.
    pub flat_shading: bool,
    /// Multiplies line widths (pass egui's `pixels_per_point`).
    pub pixel_scale: f32,
}

impl Default for Frame3D {
    fn default() -> Self {
        Self {
            background_top: [0.36, 0.40, 0.47, 1.0],
            background_bottom: [0.12, 0.13, 0.15, 1.0],
            grid: Some(Grid3D::default()),
            flat_shading: false,
            pixel_scale: 1.0,
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Uniform layouts (must match the WGSL structs)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Frame2DU {
    viewport: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Draw2DU {
    m: [f32; 4],
    t: [f32; 4],
    color: [f32; 4],
    flags: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Frame3DU {
    proj: [[f32; 4]; 4],
    viewport: [f32; 4],
    light: [f32; 4],
    up: [f32; 4],
    sky: [f32; 4],
    ground: [f32; 4],
    bg_top: [f32; 4],
    bg_bottom: [f32; 4],
    misc: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Draw3DU {
    model_view: [[f32; 4]; 4],
    normal_mat: [[f32; 4]; 4],
    color: [f32; 4],
    params: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridDrawU {
    model_view: [[f32; 4]; 4],
    minor: [f32; 4],
    major: [f32; 4],
    axis_x: [f32; 4],
    axis_y: [f32; 4],
    params: [f32; 4],
    offsets: [f32; 4],
}

/// Size of one uniform slot as seen by the shaders (every uniform struct fits).
const SLOT_SIZE: u64 = 256;

/// Per-render uniform data: slot 0 = frame uniforms, then one slot per draw (dynamic offsets).
/// A fresh buffer is created for every render call, so several renders recorded into one encoder
/// before a submit never overwrite each other's uniforms.
struct UniformArena {
    data: Vec<u8>,
    stride: usize,
}

impl UniformArena {
    fn new(stride: usize) -> Self {
        Self { data: Vec::with_capacity(stride * 16), stride }
    }

    fn push<T: bytemuck::Pod>(&mut self, v: &T) -> u32 {
        let off = self.data.len();
        self.data.extend_from_slice(bytemuck::bytes_of(v));
        self.data.resize(off + self.stride, 0);
        off as u32
    }
}

fn mat_f32(m: &DMat4) -> [[f32; 4]; 4] {
    m.as_mat4().to_cols_array_2d()
}

fn premultiplied_clear(c: Rgba) -> wgpu::Color {
    let a = c[3].clamp(0.0, 1.0) as f64;
    let ch = |v: f32| (v.clamp(0.0, 1.0) as f64) * a;
    wgpu::Color { r: ch(c[0]), g: ch(c[1]), b: ch(c[2]), a }
}

fn finite_or(v: f32, d: f32) -> f32 {
    if v.is_finite() { v } else { d }
}

fn transform_bbox(b: &BBox3, m: &DMat4) -> BBox3 {
    if b.is_empty() {
        return BBox3::EMPTY;
    }
    BBox3::from_points((0..8).map(|i| {
        m.transform_point3(DVec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        ))
    }))
}

fn hash_f64(h: &mut impl Hasher, vals: &[f64]) {
    for v in vals {
        v.to_bits().hash(h);
    }
}

fn hash_f32(h: &mut impl Hasher, vals: &[f32]) {
    for v in vals {
        v.to_bits().hash(h);
    }
}

// -------------------------------------------------------------------------------------------------
// GPU-side storage

struct GpuBatch2D {
    origin: DVec2,
    bbox: BBox2,
    visible: bool,
    style: Batch2DStyle,
    fills: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
    lines: Option<(wgpu::Buffer, u32)>,
    points: Option<(wgpu::Buffer, u32)>,
}

struct GpuMesh {
    buffers: Option<(wgpu::Buffer, wgpu::Buffer)>,
    index_count: u32,
    bbox: BBox3,
    style: MeshStyle,
}

struct GpuLines {
    buffer: Option<wgpu::Buffer>,
    count: u32,
    bbox: BBox3,
    style: LineStyle3D,
}

struct Pipelines {
    samples: u32,
    fill2d: wgpu::RenderPipeline,
    line2d: wgpu::RenderPipeline,
    point2d: wgpu::RenderPipeline,
    background: wgpu::RenderPipeline,
    mesh_opaque: wgpu::RenderPipeline,
    mesh_transparent: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    line3d: wgpu::RenderPipeline,
    line3d_top: wgpu::RenderPipeline,
}

const FILL_ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4];
const LINE2D_ATTRS: [wgpu::VertexAttribute; 5] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Unorm8x4, 3 => Unorm8x4, 4 => Float32];
const POINT_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x2, 1 => Unorm8x4, 2 => Float32, 3 => Uint32];
const MESH_ATTRS: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const LINE3D_ATTRS: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Unorm8x4, 3 => Unorm8x4];

/// Interleaved mesh vertex: position + normal.
const MESH_STRIDE: u64 = 24;

/// Headlight direction in view space (towards the light: slightly above and left of the eye).
const LIGHT_DIR_VIEW: [f64; 3] = [-0.35, 0.5, 1.0];
/// Hemispheric ambient (linear rgb) for normals facing world up / down.
const AMBIENT_SKY: [f32; 3] = [0.34, 0.35, 0.38];
const AMBIENT_GROUND: [f32; 3] = [0.14, 0.13, 0.12];

/// A queued 3D draw, recorded while building uniforms and replayed inside the pass.
enum Draw3D<'a> {
    Mesh { bufs: &'a (wgpu::Buffer, wgpu::Buffer), range: Range<u32>, offset: u32 },
    Lines { buf: &'a wgpu::Buffer, range: Range<u32>, offset: u32 },
}

/// GPU renderer for 2D batches and 3D meshes/lines. Create once per device.
pub struct Renderer {
    device: wgpu::Device,
    bgl: wgpu::BindGroupLayout,
    layout: wgpu::PipelineLayout,
    shader2d: wgpu::ShaderModule,
    shader3d: wgpu::ShaderModule,
    shader_grid: wgpu::ShaderModule,
    pipelines: Vec<Pipelines>,
    stride: usize,
    batches: BTreeMap<BatchId, GpuBatch2D>,
    overlay2d: Option<GpuBatch2D>,
    meshes: BTreeMap<MeshId, GpuMesh>,
    lines: BTreeMap<LineSetId, GpuLines>,
    overlay3d: Option<GpuLines>,
    rev2d: u64,
    rev3d: u64,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("batches", &self.batches.len())
            .field("meshes", &self.meshes.len())
            .field("lines", &self.lines.len())
            .field("rev2d", &self.rev2d)
            .field("rev3d", &self.rev3d)
            .finish_non_exhaustive()
    }
}

impl Renderer {
    /// Compiles the shaders. Pipelines are created lazily per MSAA sample count (see [`Self::warm_up`]).
    /// The device is cloned (cheap handle) for later uploads; `_queue` is accepted for API symmetry
    /// (uploads create initialized buffers, so no queue writes are needed).
    pub fn new(device: &wgpu::Device, _queue: &wgpu::Queue) -> Self {
        let align = device.limits().min_uniform_buffer_offset_alignment.max(1) as usize;
        let stride = (SLOT_SIZE as usize).div_ceil(align) * align;
        let entry = |binding, dynamic| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: None,
            },
            count: None,
        };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wcad_bgl"),
            entries: &[entry(0, false), entry(1, true)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wcad_layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let [s2, s3, sg] = shader_modules().map(|(label, src)| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(src.into()),
            })
        });
        Self {
            device: device.clone(),
            bgl,
            layout,
            shader2d: s2,
            shader3d: s3,
            shader_grid: sg,
            pipelines: Vec::new(),
            stride,
            batches: BTreeMap::new(),
            overlay2d: None,
            meshes: BTreeMap::new(),
            lines: BTreeMap::new(),
            overlay3d: None,
            rev2d: 1,
            rev3d: 1,
        }
    }

    /// Creates the pipelines for `sample_count` now instead of on the first render.
    pub fn warm_up(&mut self, sample_count: u32) {
        self.pipeline_index(sample_count);
    }

    // ---------------------------------------------------------------------------------------------
    // Buffers

    fn buffer(&self, label: &str, bytes: &[u8], usage: wgpu::BufferUsages) -> Option<wgpu::Buffer> {
        if bytes.is_empty() {
            return None;
        }
        if bytes.len() as u64 > self.device.limits().max_buffer_size {
            log::warn!("wcad-render: {label} is {} bytes, larger than the device limit; not drawn", bytes.len());
            return None;
        }
        Some(self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: bytes,
            usage,
        }))
    }

    fn make_batch2d(&self, batch: &Batch2D) -> GpuBatch2D {
        let nv = batch.fill_vertices.len() as u32;
        // Keep indices valid: out-of-range triangles become degenerate.
        let idx: Vec<u32> = batch
            .fill_indices
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| if t.iter().all(|&i| i < nv) { [t[0], t[1], t[2]] } else { [0, 0, 0] })
            .collect();
        let fills = if nv > 0 && !idx.is_empty() {
            let vb =
                self.buffer("wcad_fill_vb", bytemuck::cast_slice(&batch.fill_vertices), wgpu::BufferUsages::VERTEX);
            let ib = self.buffer("wcad_fill_ib", bytemuck::cast_slice(&idx), wgpu::BufferUsages::INDEX);
            vb.zip(ib).map(|(vb, ib)| (vb, ib, idx.len() as u32))
        } else {
            None
        };
        let lines = self
            .buffer("wcad_lines2d", bytemuck::cast_slice(&batch.lines), wgpu::BufferUsages::VERTEX)
            .map(|b| (b, batch.lines.len() as u32));
        let points = self
            .buffer("wcad_points2d", bytemuck::cast_slice(&batch.points), wgpu::BufferUsages::VERTEX)
            .map(|b| (b, batch.points.len() as u32));
        GpuBatch2D {
            origin: batch.origin,
            bbox: batch.bbox(),
            visible: true,
            style: Batch2DStyle::default(),
            fills,
            lines,
            points,
        }
    }

    fn make_lines(&self, set: &LineSet3D, style: LineStyle3D) -> GpuLines {
        let buffer = self.buffer("wcad_lines3d", bytemuck::cast_slice(&set.segments), wgpu::BufferUsages::VERTEX);
        let count = if buffer.is_some() { set.segments.len() as u32 } else { 0 };
        GpuLines { buffer, count, bbox: set.bbox(), style }
    }

    // ---------------------------------------------------------------------------------------------
    // 2D content

    /// Uploads (or replaces) a batch. Replacing keeps the batch's visibility flag and style.
    pub fn upload_batch2d(&mut self, id: BatchId, batch: &Batch2D) {
        let mut gpu = self.make_batch2d(batch);
        if let Some(old) = self.batches.get(&id) {
            gpu.visible = old.visible;
            gpu.style = old.style;
        }
        self.batches.insert(id, gpu);
        self.rev2d += 1;
    }

    /// Removes a batch; returns whether it existed.
    pub fn remove_batch2d(&mut self, id: BatchId) -> bool {
        let removed = self.batches.remove(&id).is_some();
        if removed {
            self.rev2d += 1;
        }
        removed
    }

    pub fn set_batch2d_visible(&mut self, id: BatchId, visible: bool) {
        if let Some(b) = self.batches.get_mut(&id)
            && b.visible != visible
        {
            b.visible = visible;
            self.rev2d += 1;
        }
    }

    /// Sets a batch's draw-time style; returns `false` if the batch does not exist.
    pub fn set_batch2d_style(&mut self, id: BatchId, style: Batch2DStyle) -> bool {
        match self.batches.get_mut(&id) {
            Some(b) => {
                if b.style != style {
                    b.style = style;
                    self.rev2d += 1;
                }
                true
            }
            None => false,
        }
    }

    pub fn has_batch2d(&self, id: BatchId) -> bool {
        self.batches.contains_key(&id)
    }

    pub fn clear_batches2d(&mut self) {
        if !self.batches.is_empty() {
            self.batches.clear();
            self.rev2d += 1;
        }
    }

    /// Replaces the transient overlay (highlights, previews) drawn after all batches.
    pub fn set_overlay2d(&mut self, batch: &Batch2D) {
        self.overlay2d = (!batch.is_empty()).then(|| self.make_batch2d(batch));
        self.rev2d += 1;
    }

    pub fn clear_overlay2d(&mut self) {
        if self.overlay2d.take().is_some() {
            self.rev2d += 1;
        }
    }

    /// Bounds of all visible batches (for "zoom extents").
    pub fn scene_bbox_2d(&self) -> BBox2 {
        self.batches.values().filter(|b| b.visible).fold(BBox2::EMPTY, |acc, b| acc.union(&b.bbox))
    }

    /// Changes whenever the 2D content changes.
    pub fn revision_2d(&self) -> u64 {
        self.rev2d
    }

    fn key_2d(&self, vp: &Viewport, cam: &Camera2D, frame: &Frame2D) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "2d".hash(&mut h);
        (vp.uid(), vp.generation(), self.rev2d).hash(&mut h);
        hash_f64(&mut h, &[cam.center.x, cam.center.y, cam.px_per_unit, cam.rotation]);
        hash_f32(&mut h, &frame.background);
        hash_f32(&mut h, &[frame.pixel_scale]);
        h.finish()
    }

    /// `true` if `vp` does not show this camera/frame/content yet.
    pub fn needs_render_2d(&self, vp: &Viewport, cam: &Camera2D, frame: &Frame2D) -> bool {
        vp.rendered_key() != self.key_2d(vp, cam, frame)
    }

    /// Records the 2D pass into `encoder` (clears the viewport). The caller submits the encoder.
    pub fn render_2d(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vp: &Viewport,
        cam: &Camera2D,
        frame: &Frame2D,
    ) {
        let key = self.key_2d(vp, cam, frame);
        let pi = self.pipeline_index(vp.sample_count());
        let size = vp.size_f64();
        let pixel_scale = finite_or(frame.pixel_scale, 1.0).clamp(0.01, 100.0);
        // Culling rectangle with a generous margin for line widths and markers.
        let visible = cam.visible_bbox(size).expanded(cam.px_to_units(64.0 * pixel_scale as f64));

        let mut u = UniformArena::new(self.stride);
        u.push(&Frame2DU { viewport: [size.x as f32, size.y as f32, pixel_scale, 0.0] });
        let mut draws: Vec<(&GpuBatch2D, u32)> = Vec::new();
        for b in self.batches.values().filter(|b| b.visible).chain(self.overlay2d.iter()) {
            if b.bbox.is_empty() || !b.bbox.intersects(&visible) {
                continue;
            }
            let (m, t) = cam.batch_transform(b.origin);
            let st = &b.style;
            let opacity = finite_or(st.opacity, 1.0).clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            let off = u.push(&Draw2DU {
                m,
                t: [t[0], t[1], 0.0, 0.0],
                color: st.color.unwrap_or([0.0; 4]),
                flags: [if st.color.is_some() { 1.0 } else { 0.0 }, opacity, 0.0, 0.0],
            });
            draws.push((b, off));
        }
        let bind_group = self.uniform_bind_group(device, &u);

        let p = &self.pipelines[pi];
        let (view, resolve_target) = vp.color_attachment();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wcad_2d"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(premultiplied_clear(frame.background)),
                    store: if resolve_target.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        for (b, off) in draws {
            if let Some((vb, ib, n)) = &b.fills {
                pass.set_pipeline(&p.fill2d);
                pass.set_bind_group(0, &bind_group, &[off]);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*n, 0, 0..1);
            }
            if let Some((buf, n)) = &b.lines {
                pass.set_pipeline(&p.line2d);
                pass.set_bind_group(0, &bind_group, &[off]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*n);
            }
            if let Some((buf, n)) = &b.points {
                pass.set_pipeline(&p.point2d);
                pass.set_bind_group(0, &bind_group, &[off]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..6, 0..*n);
            }
        }
        drop(pass);
        vp.set_rendered_key(key);
    }

    // ---------------------------------------------------------------------------------------------
    // 3D content

    /// Uploads (or replaces) a mesh. Replacing keeps the existing style. Missing/mismatched normals are
    /// computed; triangles with out-of-range indices are drawn degenerate (index positions are kept
    /// so highlight ranges stay valid).
    pub fn upload_mesh(&mut self, id: MeshId, mesh: &MeshData) {
        let n = mesh.positions.len();
        let computed;
        let normals = if mesh.normals.len() == n {
            &mesh.normals
        } else {
            computed = mesh.compute_normals();
            &computed
        };
        let mut verts: Vec<f32> = Vec::with_capacity(n * 6);
        for (p, nr) in mesh.positions.iter().zip(normals) {
            let p = if p.iter().all(|c| c.is_finite()) { *p } else { [0.0; 3] };
            let nr = if nr.iter().all(|c| c.is_finite()) { *nr } else { [0.0, 0.0, 1.0] };
            verts.extend_from_slice(&p);
            verts.extend_from_slice(&nr);
        }
        let idx: Vec<u32> = mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|t| if t.iter().all(|&i| (i as usize) < n) { [t[0], t[1], t[2]] } else { [0, 0, 0] })
            .collect();
        let buffers = if n > 0 && !idx.is_empty() {
            let vb = self.buffer("wcad_mesh_vb", bytemuck::cast_slice(&verts), wgpu::BufferUsages::VERTEX);
            let ib = self.buffer("wcad_mesh_ib", bytemuck::cast_slice(&idx), wgpu::BufferUsages::INDEX);
            vb.zip(ib)
        } else {
            None
        };
        let index_count = if buffers.is_some() { idx.len() as u32 } else { 0 };
        let style = self.meshes.remove(&id).map(|m| m.style).unwrap_or_default();
        self.meshes.insert(id, GpuMesh { buffers, index_count, bbox: mesh.bbox(), style });
        self.rev3d += 1;
    }

    /// Sets a mesh's style; returns `false` if the mesh does not exist. Unchanged styles do not dirty
    /// the scene.
    pub fn set_mesh_style(&mut self, id: MeshId, style: MeshStyle) -> bool {
        match self.meshes.get_mut(&id) {
            Some(m) => {
                if m.style != style {
                    m.style = style;
                    self.rev3d += 1;
                }
                true
            }
            None => false,
        }
    }

    pub fn mesh_style(&self, id: MeshId) -> Option<&MeshStyle> {
        self.meshes.get(&id).map(|m| &m.style)
    }

    pub fn remove_mesh(&mut self, id: MeshId) -> bool {
        let removed = self.meshes.remove(&id).is_some();
        if removed {
            self.rev3d += 1;
        }
        removed
    }

    pub fn clear_meshes(&mut self) {
        if !self.meshes.is_empty() {
            self.meshes.clear();
            self.rev3d += 1;
        }
    }

    /// Uploads (or replaces) a 3D line set. Replacing keeps the existing style.
    pub fn upload_lines3d(&mut self, id: LineSetId, set: &LineSet3D) {
        let style = self.lines.remove(&id).map(|l| l.style).unwrap_or_default();
        let gpu = self.make_lines(set, style);
        self.lines.insert(id, gpu);
        self.rev3d += 1;
    }

    pub fn set_lines3d_style(&mut self, id: LineSetId, style: LineStyle3D) -> bool {
        match self.lines.get_mut(&id) {
            Some(l) => {
                if l.style != style {
                    l.style = style;
                    self.rev3d += 1;
                }
                true
            }
            None => false,
        }
    }

    pub fn lines3d_style(&self, id: LineSetId) -> Option<&LineStyle3D> {
        self.lines.get(&id).map(|l| &l.style)
    }

    pub fn remove_lines3d(&mut self, id: LineSetId) -> bool {
        let removed = self.lines.remove(&id).is_some();
        if removed {
            self.rev3d += 1;
        }
        removed
    }

    pub fn clear_lines3d(&mut self) {
        if !self.lines.is_empty() {
            self.lines.clear();
            self.rev3d += 1;
        }
    }

    /// Replaces the transient 3D overlay (previews, hovered edges), drawn after all other lines.
    pub fn set_overlay3d(&mut self, set: &LineSet3D, style: LineStyle3D) {
        self.overlay3d = (!set.is_empty()).then(|| self.make_lines(set, style));
        self.rev3d += 1;
    }

    pub fn clear_overlay3d(&mut self) {
        if self.overlay3d.take().is_some() {
            self.rev3d += 1;
        }
    }

    /// World bounds of all visible meshes and line sets (for "zoom extents"; grid and overlay excluded).
    pub fn scene_bbox_3d(&self) -> BBox3 {
        let mut b = BBox3::EMPTY;
        for m in self.meshes.values().filter(|m| m.style.visible) {
            b = b.union(&transform_bbox(&m.bbox, &m.style.transform));
        }
        for l in self.lines.values().filter(|l| l.style.visible) {
            b = b.union(&transform_bbox(&l.bbox, &l.style.transform));
        }
        b
    }

    /// Changes whenever the 3D content changes.
    pub fn revision_3d(&self) -> u64 {
        self.rev3d
    }

    fn key_3d(&self, vp: &Viewport, cam: &Camera3D, frame: &Frame3D) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "3d".hash(&mut h);
        (vp.uid(), vp.generation(), self.rev3d, cam.ortho, frame.flat_shading).hash(&mut h);
        hash_f64(&mut h, &[cam.target.x, cam.target.y, cam.target.z, cam.distance, cam.yaw, cam.pitch, cam.fov_y]);
        hash_f32(&mut h, &frame.background_top);
        hash_f32(&mut h, &frame.background_bottom);
        hash_f32(&mut h, &[frame.pixel_scale]);
        if let Some(g) = &frame.grid {
            let p = &g.plane;
            hash_f64(&mut h, &p.origin.to_array());
            hash_f64(&mut h, &p.x_axis.to_array());
            hash_f64(&mut h, &p.y_axis.to_array());
            hash_f64(&mut h, &[g.spacing, g.radius]);
            g.major_every.hash(&mut h);
            for c in [g.minor_color, g.major_color, g.x_axis_color, g.y_axis_color] {
                hash_f32(&mut h, &c);
            }
            hash_f32(&mut h, &[g.line_width_px]);
        }
        h.finish()
    }

    /// `true` if `vp` does not show this camera/frame/content yet.
    pub fn needs_render_3d(&self, vp: &Viewport, cam: &Camera3D, frame: &Frame3D) -> bool {
        vp.rendered_key() != self.key_3d(vp, cam, frame)
    }

    /// Grid model matrix (grid-local → world) and uniform parameters, or `None` for an invalid grid.
    fn grid_setup(grid: &Grid3D, cam: &Camera3D) -> Option<(DMat4, f64, GridDrawU)> {
        let p = &grid.plane;
        let n = p.normal();
        let ok = |v: DVec3| v.is_finite() && v.length_squared() > 1e-20;
        if !(ok(p.x_axis) && ok(p.y_axis) && ok(n) && p.origin.is_finite()) {
            return None;
        }
        if !(grid.spacing.is_finite() && grid.spacing > 0.0) {
            return None;
        }
        let spacing = grid.spacing;
        let major = if grid.major_every > 1 { spacing * grid.major_every as f64 } else { 0.0 };
        let period = if major > 0.0 { major } else { spacing };
        let radius = if grid.radius.is_finite() && grid.radius > 0.0 { grid.radius } else { cam.distance.abs() * 4.0 };
        if !(radius.is_finite() && radius > 0.0) {
            return None;
        }
        // Center the drawn square on the camera target's projection, snapped to the grid period so the
        // pattern stays fixed in world space and the shader works with small numbers.
        let local = p.to_local(cam.target);
        let anchor = (local / period).round() * period;
        if !anchor.is_finite() {
            return None;
        }
        let anchor_world = p.to_world(anchor);
        let model = DMat4::from_cols(
            p.x_axis.extend(0.0),
            p.y_axis.extend(0.0),
            n.normalize().extend(0.0),
            anchor_world.extend(1.0),
        );
        let u = GridDrawU {
            model_view: [[0.0; 4]; 4],
            minor: grid.minor_color,
            major: grid.major_color,
            axis_x: grid.x_axis_color,
            axis_y: grid.y_axis_color,
            params: [spacing as f32, major as f32, radius as f32, finite_or(grid.line_width_px, 1.0)],
            offsets: [0.0, 0.0, anchor.x as f32, anchor.y as f32],
        };
        Some((model, radius, u))
    }

    /// Records the 3D pass into `encoder` (clears color and depth). The caller submits the encoder.
    pub fn render_3d(
        &mut self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        vp: &Viewport,
        cam: &Camera3D,
        frame: &Frame3D,
    ) {
        let key = self.key_3d(vp, cam, frame);
        let pi = self.pipeline_index(vp.sample_count());
        let size = vp.size_f64();
        let pixel_scale = finite_or(frame.pixel_scale, 1.0).clamp(0.01, 100.0);
        let view = cam.view_matrix();

        let grid = frame.grid.as_ref().and_then(|g| Self::grid_setup(g, cam));
        let mut bbox = self.scene_bbox_3d();
        if let Some(o) = &self.overlay3d {
            bbox = bbox.union(&transform_bbox(&o.bbox, &o.style.transform));
        }
        if let Some((model, radius, _)) = &grid {
            let quad = BBox3 { min: DVec3::new(-radius, -radius, 0.0), max: DVec3::new(*radius, *radius, 0.0) };
            bbox = bbox.union(&transform_bbox(&quad, model));
        }
        let (near, far) = cam.depth_range(&bbox);
        let proj = cam.proj_matrix(vp.aspect(), near, far);
        let light = DVec3::from_array(LIGHT_DIR_VIEW).normalize();
        let up = view.transform_vector3(DVec3::Z).normalize_or(DVec3::Y);

        let mut u = UniformArena::new(self.stride);
        u.push(&Frame3DU {
            proj: mat_f32(&proj),
            viewport: [size.x as f32, size.y as f32, pixel_scale, if cam.ortho { 0.0 } else { 1.0 }],
            light: [light.x as f32, light.y as f32, light.z as f32, if frame.flat_shading { 1.0 } else { 0.0 }],
            up: [up.x as f32, up.y as f32, up.z as f32, 0.0],
            sky: [AMBIENT_SKY[0], AMBIENT_SKY[1], AMBIENT_SKY[2], 1.0],
            ground: [AMBIENT_GROUND[0], AMBIENT_GROUND[1], AMBIENT_GROUND[2], 1.0],
            bg_top: frame.background_top,
            bg_bottom: frame.background_bottom,
            misc: [near as f32, (far - near) as f32, 0.0, 0.0],
        });

        // Meshes: split each into base/highlight pieces; opaque first, transparent sorted back to front.
        let mut opaque: Vec<Draw3D<'_>> = Vec::new();
        let mut transparent: Vec<(f64, Draw3D<'_>)> = Vec::new();
        for m in self.meshes.values() {
            let Some(bufs) = &m.buffers else { continue };
            if !m.style.visible || m.index_count == 0 {
                continue;
            }
            let mv = view * m.style.transform;
            if !mv.is_finite() {
                continue;
            }
            let nm = mv.inverse().transpose();
            let nm = if nm.is_finite() { nm } else { mv };
            let (mv32, nm32) = (mat_f32(&mv), mat_f32(&nm));
            let base = m.style.color.map(|c| finite_or(c, 1.0).clamp(0.0, 1.0));
            let is_transparent = base[3] < 0.999;
            let depth = {
                let c = transform_bbox(&m.bbox, &m.style.transform).center();
                -view.transform_point3(c).z
            };
            let ranges = normalize_ranges(&m.style.highlight_ranges, m.index_count, 3);
            for (range, color, _) in split_ranges(m.index_count, &ranges, base, true) {
                let offset = u.push(&Draw3DU { model_view: mv32, normal_mat: nm32, color, params: [0.0; 4] });
                let d = Draw3D::Mesh { bufs, range, offset };
                if is_transparent {
                    transparent.push((depth, d));
                } else {
                    opaque.push(d);
                }
            }
        }
        transparent.sort_by(|a, b| b.0.total_cmp(&a.0));

        let grid_offset = grid.map(|(model, _, mut gu)| {
            gu.model_view = mat_f32(&(view * model));
            u.push(&gu)
        });

        let mut depth_lines: Vec<Draw3D<'_>> = Vec::new();
        let mut top_lines: Vec<Draw3D<'_>> = Vec::new();
        for l in self.lines.values().chain(self.overlay3d.iter()) {
            let Some(buf) = &l.buffer else { continue };
            if !l.style.visible || l.count == 0 {
                continue;
            }
            let mv = view * l.style.transform;
            if !mv.is_finite() {
                continue;
            }
            let mv32 = mat_f32(&mv);
            let s = &l.style;
            let width = finite_or(s.width_px, 1.0);
            let bias = finite_or(s.depth_bias, 0.0).clamp(0.0, 0.1);
            let ranges = normalize_ranges(&s.highlight_ranges, l.count, 1);
            let base = s.color.unwrap_or([0.0; 4]);
            for (range, color, is_highlight) in split_ranges(l.count, &ranges, base, false) {
                let override_color = s.color.is_some() || is_highlight;
                let offset = u.push(&Draw3DU {
                    model_view: mv32,
                    normal_mat: [[0.0; 4]; 4],
                    color,
                    params: [width, bias, if override_color { 1.0 } else { 0.0 }, 0.0],
                });
                let d = Draw3D::Lines { buf, range, offset };
                if s.on_top {
                    top_lines.push(d);
                } else {
                    depth_lines.push(d);
                }
            }
        }

        let bind_group = self.uniform_bind_group(device, &u);
        let p = &self.pipelines[pi];
        let (view_att, resolve_target) = vp.color_attachment();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("wcad_3d"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: view_att,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(premultiplied_clear(frame.background_bottom)),
                    store: if resolve_target.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: vp.depth_view(),
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&p.background);
        pass.set_bind_group(0, &bind_group, &[0]);
        pass.draw(0..3, 0..1);

        let replay = |pass: &mut wgpu::RenderPass<'_>, d: &Draw3D<'_>| match d {
            Draw3D::Mesh { bufs, range, offset } => {
                pass.set_bind_group(0, &bind_group, &[*offset]);
                pass.set_vertex_buffer(0, bufs.0.slice(..));
                pass.set_index_buffer(bufs.1.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(range.clone(), 0, 0..1);
            }
            Draw3D::Lines { buf, range, offset } => {
                pass.set_bind_group(0, &bind_group, &[*offset]);
                let stride = size_of::<LineSegment3D>() as u64;
                pass.set_vertex_buffer(0, buf.slice(range.start as u64 * stride..range.end as u64 * stride));
                pass.draw(0..6, 0..(range.end - range.start));
            }
        };

        pass.set_pipeline(&p.mesh_opaque);
        for d in &opaque {
            replay(&mut pass, d);
        }
        if let Some(off) = grid_offset {
            pass.set_pipeline(&p.grid);
            pass.set_bind_group(0, &bind_group, &[off]);
            pass.draw(0..6, 0..1);
        }
        // Transparent faces do not write depth, so edges behind them stay visible (CAD convention).
        pass.set_pipeline(&p.mesh_transparent);
        for (_, d) in &transparent {
            replay(&mut pass, d);
        }
        pass.set_pipeline(&p.line3d);
        for d in &depth_lines {
            replay(&mut pass, d);
        }
        pass.set_pipeline(&p.line3d_top);
        for d in &top_lines {
            replay(&mut pass, d);
        }
        drop(pass);
        vp.set_rendered_key(key);
    }

    // ---------------------------------------------------------------------------------------------
    // Pipelines

    fn uniform_bind_group(&self, device: &wgpu::Device, u: &UniformArena) -> wgpu::BindGroup {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("wcad_uniforms"),
            contents: &u.data,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let binding = |binding| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: wgpu::BufferSize::new(SLOT_SIZE),
            }),
        };
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wcad_uniforms_bg"),
            layout: &self.bgl,
            entries: &[binding(0), binding(1)],
        })
    }

    fn pipeline_index(&mut self, samples: u32) -> usize {
        if let Some(i) = self.pipelines.iter().position(|p| p.samples == samples) {
            return i;
        }
        let p = self.create_pipelines(samples);
        self.pipelines.push(p);
        self.pipelines.len() - 1
    }

    fn create_pipelines(&self, samples: u32) -> Pipelines {
        let d = &self.device;
        let premul = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let vb = |stride: u64, step, attrs: &'static [wgpu::VertexAttribute]| wgpu::VertexBufferLayout {
            array_stride: stride,
            step_mode: step,
            attributes: attrs,
        };
        let depth = |write: bool, compare, bias: wgpu::DepthBiasState| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: Default::default(),
                bias,
            })
        };
        // Faces are pushed away from the eye (reverse-Z: negative bias) so coplanar edges win.
        let face_bias = wgpu::DepthBiasState { constant: -4, slope_scale: -1.0, clamp: 0.0 };
        let no_bias = wgpu::DepthBiasState::default();
        let make = |label: &str,
                    module: &wgpu::ShaderModule,
                    vs: &str,
                    fs: &str,
                    buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                    depth_stencil: Option<wgpu::DepthStencilState>,
                    blend: Option<wgpu::BlendState>| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&self.layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(vs),
                    buffers,
                    compilation_options: Default::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil,
                multisample: wgpu::MultisampleState { count: samples, mask: !0, alpha_to_coverage_enabled: false },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: COLOR_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let inst = wgpu::VertexStepMode::Instance;
        let vert = wgpu::VertexStepMode::Vertex;
        let fill_vb = [Some(vb(size_of::<FillVertex>() as u64, vert, &FILL_ATTRS))];
        let line2d_vb = [Some(vb(size_of::<LineSegment2D>() as u64, inst, &LINE2D_ATTRS))];
        let point_vb = [Some(vb(size_of::<PointMarker>() as u64, inst, &POINT_ATTRS))];
        let mesh_vb = [Some(vb(MESH_STRIDE, vert, &MESH_ATTRS))];
        let line3d_vb = [Some(vb(size_of::<LineSegment3D>() as u64, inst, &LINE3D_ATTRS))];
        use wgpu::CompareFunction::{Always, GreaterEqual};
        Pipelines {
            samples,
            fill2d: make("wcad_fill2d", &self.shader2d, "vs_fill", "fs_fill", &fill_vb, None, premul),
            line2d: make("wcad_line2d", &self.shader2d, "vs_line", "fs_line", &line2d_vb, None, premul),
            point2d: make("wcad_point2d", &self.shader2d, "vs_point", "fs_point", &point_vb, None, premul),
            background: make(
                "wcad_background",
                &self.shader3d,
                "vs_background",
                "fs_background",
                &[],
                depth(false, Always, no_bias),
                None,
            ),
            mesh_opaque: make(
                "wcad_mesh_opaque",
                &self.shader3d,
                "vs_mesh",
                "fs_mesh",
                &mesh_vb,
                depth(true, GreaterEqual, face_bias),
                None,
            ),
            mesh_transparent: make(
                "wcad_mesh_transparent",
                &self.shader3d,
                "vs_mesh",
                "fs_mesh",
                &mesh_vb,
                depth(false, GreaterEqual, face_bias),
                premul,
            ),
            grid: make(
                "wcad_grid",
                &self.shader_grid,
                "vs_grid",
                "fs_grid",
                &[],
                depth(false, GreaterEqual, no_bias),
                premul,
            ),
            line3d: make(
                "wcad_line3d",
                &self.shader3d,
                "vs_line3",
                "fs_line3",
                &line3d_vb,
                depth(false, GreaterEqual, no_bias),
                premul,
            ),
            line3d_top: make(
                "wcad_line3d_top",
                &self.shader3d,
                "vs_line3",
                "fs_line3",
                &line3d_vb,
                depth(false, Always, no_bias),
                premul,
            ),
        }
    }
}

/// Splits `0..count` into consecutive `(range, color, is_highlight)` pieces: gaps get `base`, highlight
/// ranges (sorted, non-overlapping, see `normalize_ranges`) their color. With `blend_over_base`, a
/// highlight's alpha mixes its rgb over the base (the result keeps the base alpha).
pub(crate) fn split_ranges(
    count: u32,
    highlights: &[(Range<u32>, Rgba)],
    base: Rgba,
    blend_over_base: bool,
) -> Vec<(Range<u32>, Rgba, bool)> {
    let mut out = Vec::with_capacity(highlights.len() * 2 + 1);
    let mut cursor = 0u32;
    for (r, c) in highlights {
        if r.start > cursor {
            out.push((cursor..r.start, base, false));
        }
        let color = if blend_over_base {
            let t = c[3].clamp(0.0, 1.0);
            [base[0] + (c[0] - base[0]) * t, base[1] + (c[1] - base[1]) * t, base[2] + (c[2] - base[2]) * t, base[3]]
        } else {
            *c
        };
        out.push((r.clone(), color, true));
        cursor = r.end;
    }
    if cursor < count {
        out.push((cursor..count, base, false));
    }
    out
}

// Uniform structs must fit a slot; vertex structs must match the attribute layouts.
const _: () = assert!(size_of::<Frame3DU>() as u64 <= SLOT_SIZE);
const _: () = assert!(size_of::<Draw3DU>() as u64 <= SLOT_SIZE);
const _: () = assert!(size_of::<GridDrawU>() as u64 <= SLOT_SIZE);
const _: () = assert!(size_of::<LineSegment2D>() == 28);
const _: () = assert!(size_of::<LineSegment3D>() == 32);
const _: () = assert!(size_of::<PointMarker>() == 20);
const _: () = assert!(size_of::<FillVertex>() == 12);
