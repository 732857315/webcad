//! Headless smoke test (no window): renders 2D and 3D scenes into offscreen viewports and writes PNGs.
//!
//!   cargo run -p wcad-render --example headless -- [out_dir]
//!
//! `WGPU_BACKEND=dx12|vulkan|gl` selects the backend (default: the primary backends). The device is
//! requested with WebGL2 downlevel limits so the example exercises the same limits as the web build.

use std::f64::consts::TAU;
use std::path::{Path, PathBuf};

use wcad_render::{
    Batch2D, BatchId, Camera2D, Camera3D, DMat4, DVec2, DVec3, Frame2D, Frame3D, Grid3D, LineSet3D, LineSetId,
    LineStyle3D, MeshData, MeshId, MeshStyle, Plane, PointShape, Renderer, StandardView, Viewport, choose_sample_count,
    rgb8, rgba8,
};

const W: u32 = 1280;
const H: u32 = 800;

fn main() {
    if let Err(e) = run() {
        eprintln!("headless: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let out_dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".scratch/impl/render".to_owned()));
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;

    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
    if std::env::var_os("WGPU_BACKEND").is_none() {
        desc.backends = wgpu::Backends::PRIMARY;
    }
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| format!("no adapter: {e}"))?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("headless"),
        required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| format!("no device: {e}"))?;
    let samples = choose_sample_count(&adapter, 4);
    println!("adapter: {:?} {} ({:?}), msaa {samples}", info.backend, info.name, info.device_type);

    let t0 = std::time::Instant::now();
    let mut renderer = Renderer::new(&device, &queue);
    renderer.warm_up(samples);
    println!("pipelines: {:.0?}", t0.elapsed());
    let mut vp = Viewport::new(&device, [W, H], samples);

    // ------------------------------------------------------------------ 2D
    build_2d_scene(&mut renderer);
    let frame2d = Frame2D { background: rgb8(33, 36, 41), pixel_scale: 1.0 };
    let cam = Camera2D { center: DVec2::new(0.0, 0.0), px_per_unit: 5.0, rotation: 0.0 };
    render_and_save_2d(&device, &queue, &mut renderer, &vp, &cam, &frame2d, &out_dir.join("2d.png"))?;
    // Dirty tracking: nothing changed -> no render needed; a camera change or new overlay -> render.
    assert!(!renderer.needs_render_2d(&vp, &cam, &frame2d), "unchanged view must not need a render");
    let rotated = Camera2D { rotation: 0.35, ..cam };
    assert!(renderer.needs_render_2d(&vp, &rotated, &frame2d));
    render_and_save_2d(&device, &queue, &mut renderer, &vp, &rotated, &frame2d, &out_dir.join("2d_rotated.png"))?;

    // Large coordinates: 1 mm detail around (1e6, 1e6) at 1000 px/unit.
    let far = Camera2D { center: DVec2::new(1.0e6 + 0.3, 1.0e6 + 0.2), px_per_unit: 1000.0, rotation: 0.0 };
    render_and_save_2d(&device, &queue, &mut renderer, &vp, &far, &frame2d, &out_dir.join("2d_large_coords.png"))?;

    // ------------------------------------------------------------------ 3D
    let origin = DVec3::ZERO;
    build_3d_scene(&mut renderer, origin);
    let mut cam3 = Camera3D::default();
    cam3.fit_bbox(&renderer.scene_bbox_3d(), vp.aspect());
    cam3.distance *= 0.85;
    let frame3d = Frame3D {
        grid: Some(Grid3D { spacing: 10.0, major_every: 5, radius: 400.0, ..Default::default() }),
        ..Default::default()
    };
    render_and_save_3d(&device, &queue, &mut renderer, &vp, &cam3, &frame3d, &out_dir.join("3d_persp.png"))?;
    assert!(!renderer.needs_render_3d(&vp, &cam3, &frame3d));

    let ortho = Camera3D { ortho: true, ..cam3 };
    let flat = Frame3D { flat_shading: true, ..frame3d };
    render_and_save_3d(&device, &queue, &mut renderer, &vp, &ortho, &flat, &out_dir.join("3d_ortho_flat.png"))?;

    let mut top = cam3;
    top.set_view(StandardView::Top);
    top.ortho = true;
    render_and_save_3d(&device, &queue, &mut renderer, &vp, &top, &frame3d, &out_dir.join("3d_top.png"))?;

    // The same scene placed at 1e6: must look identical to 3d_persp.png.
    let far_origin = DVec3::new(1.0e6, -2.0e6, 0.0);
    build_3d_scene(&mut renderer, far_origin);
    let far_cam = Camera3D { target: cam3.target + far_origin, ..cam3 };
    let far_frame = Frame3D {
        grid: frame3d.grid.map(|g| Grid3D { plane: Plane { origin: far_origin, ..Plane::XY }, ..g }),
        ..frame3d
    };
    render_and_save_3d(&device, &queue, &mut renderer, &vp, &far_cam, &far_frame, &out_dir.join("3d_far.png"))?;

    // Resize path: the viewport reports new textures.
    assert!(vp.resize(&device, [640, 400]));
    assert!(!vp.resize(&device, [640, 400]));
    assert!(renderer.needs_render_3d(&vp, &far_cam, &far_frame));
    render_and_save_3d(&device, &queue, &mut renderer, &vp, &far_cam, &far_frame, &out_dir.join("3d_small.png"))?;
    println!("done in {:.0?}", t0.elapsed());
    Ok(())
}

// ---------------------------------------------------------------------------------------------------
// Scenes

fn circle(c: DVec2, r: f64, n: usize) -> Vec<DVec2> {
    (0..n).map(|i| c + DVec2::from_angle(i as f64 / n as f64 * TAU) * r).collect()
}

fn build_2d_scene(r: &mut Renderer) {
    let white = rgb8(230, 230, 230);
    let mut b = Batch2D::new(DVec2::ZERO);
    // Line widths 0.5 .. 8 px, horizontal and slanted.
    for (i, w) in [0.5f32, 1.0, 1.5, 2.0, 3.0, 5.0, 8.0].iter().enumerate() {
        let y = 60.0 - i as f64 * 7.0;
        b.push_line(DVec2::new(-120.0, y), DVec2::new(-40.0, y), *w, white);
        b.push_line(DVec2::new(-35.0, y), DVec2::new(-5.0, y + 5.0), *w, rgb8(120, 200, 255));
    }
    // Gradient line and a thick closed polyline (round joins).
    b.push_line_colors(DVec2::new(-120.0, -5.0), DVec2::new(-5.0, -5.0), 4.0, rgb8(255, 60, 60), rgb8(60, 120, 255));
    b.push_polyline(
        &[DVec2::new(-110.0, -20.0), DVec2::new(-60.0, -20.0), DVec2::new(-60.0, -60.0), DVec2::new(-110.0, -45.0)],
        true,
        6.0,
        rgb8(255, 200, 60),
    );
    // Hairline circle and a dash-dot circle.
    b.push_polyline(&circle(DVec2::new(40.0, 30.0), 30.0, 180), true, 1.0, white);
    b.push_dashed_polyline(
        &circle(DVec2::new(40.0, 30.0), 22.0, 180),
        true,
        &[6.0, -2.0, 0.0, -2.0],
        0.0,
        1.5,
        rgb8(90, 220, 120),
    );
    // Dashed lines with different patterns.
    for (i, pat) in [[4.0, -2.0], [1.0, -1.0], [8.0, -1.5]].iter().enumerate() {
        let y = -20.0 - i as f64 * 6.0;
        b.push_dashed_polyline(&[DVec2::new(-40.0, y), DVec2::new(20.0, y)], false, pat, 0.0, 2.0, rgb8(200, 160, 255));
    }
    // Filled triangles: an opaque triangle, a translucent polygon on top of lines, a color fan.
    b.push_triangle(DVec2::new(80.0, -60.0), DVec2::new(120.0, -60.0), DVec2::new(100.0, -25.0), rgb8(220, 70, 70));
    b.push_convex_polygon(
        &[DVec2::new(-30.0, -65.0), DVec2::new(30.0, -65.0), DVec2::new(40.0, -35.0), DVec2::new(-20.0, -30.0)],
        rgba8(60, 140, 255, 110),
    );
    // Points of every shape.
    let shapes = [
        PointShape::Square,
        PointShape::SquareOutline,
        PointShape::Cross,
        PointShape::XCross,
        PointShape::Circle,
        PointShape::CircleOutline,
    ];
    for (i, s) in shapes.iter().enumerate() {
        b.push_point(DVec2::new(80.0 + i as f64 * 7.0, 60.0), 12.0, *s, rgb8(255, 230, 80));
        b.push_point(DVec2::new(80.0 + i as f64 * 7.0, 50.0), 7.0, *s, rgb8(80, 230, 255));
    }
    r.upload_batch2d(BatchId(1), &b);

    // Fine detail around (1e6, 1e6): 10 px spaced grid at 1000 px/unit, a 0.2 radius circle, points.
    let o = DVec2::new(1.0e6, 1.0e6);
    let mut far = Batch2D::new(o);
    for i in 0..=60 {
        let d = i as f64 * 0.01;
        let color = if i % 10 == 0 { rgb8(170, 170, 170) } else { rgb8(80, 84, 90) };
        far.push_line(o + DVec2::new(d, 0.0), o + DVec2::new(d, 0.6), 1.0, color);
        far.push_line(o + DVec2::new(0.0, d), o + DVec2::new(0.6, d), 1.0, color);
    }
    far.push_polyline(&circle(o + DVec2::new(0.3, 0.2), 0.2, 256), true, 2.0, rgb8(255, 200, 60));
    far.push_point(o + DVec2::new(0.3, 0.2), 9.0, PointShape::XCross, rgb8(255, 90, 90));
    far.push_triangle(
        o + DVec2::new(0.05, 0.05),
        o + DVec2::new(0.1, 0.05),
        o + DVec2::new(0.075, 0.1),
        rgb8(90, 200, 120),
    );
    r.upload_batch2d(BatchId(2), &far);

    // A hidden batch must not show up.
    let mut hidden = Batch2D::new(DVec2::ZERO);
    hidden.push_line(DVec2::new(-150.0, -80.0), DVec2::new(150.0, 80.0), 10.0, rgb8(255, 0, 0));
    r.upload_batch2d(BatchId(3), &hidden);
    r.set_batch2d_visible(BatchId(3), false);

    // Overlay: selection window (translucent fill + dashed outline) and a highlighted segment.
    let mut ov = Batch2D::new(DVec2::ZERO);
    let rect = [DVec2::new(20.0, -75.0), DVec2::new(140.0, -75.0), DVec2::new(140.0, -15.0), DVec2::new(20.0, -15.0)];
    ov.push_convex_polygon(&rect, rgba8(80, 160, 255, 40));
    ov.push_dashed_polyline(&rect, true, &[3.0, -2.0], 0.0, 1.0, rgb8(120, 190, 255));
    ov.push_line(DVec2::new(-120.0, 60.0), DVec2::new(-40.0, 60.0), 3.0, rgba8(255, 255, 0, 200));
    r.set_overlay2d(&ov);
}

/// Box with per-face normals (24 vertices), faces in the order -X, +X, -Y, +Y, -Z, +Z (6 indices each).
fn box_mesh(min: DVec3, max: DVec3) -> MeshData {
    let mut m = MeshData::default();
    let faces: [(DVec3, DVec3, DVec3); 6] = [
        (DVec3::NEG_X, DVec3::NEG_Y, DVec3::Z),
        (DVec3::X, DVec3::Y, DVec3::Z),
        (DVec3::NEG_Y, DVec3::X, DVec3::Z),
        (DVec3::Y, DVec3::NEG_X, DVec3::Z),
        (DVec3::NEG_Z, DVec3::Y, DVec3::X),
        (DVec3::Z, DVec3::X, DVec3::Y),
    ];
    let c = (min + max) * 0.5;
    let h = (max - min) * 0.5;
    for (n, u, v) in faces {
        let base = m.positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = c + (n + u * su + v * sv) * h;
            m.positions.push(p.as_vec3().to_array());
            m.normals.push(n.as_vec3().to_array());
        }
        m.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    m
}

/// Cylinder along +Z from the origin: smooth side + flat caps.
fn cylinder_mesh(r: f64, h: f64, n: u32) -> MeshData {
    let mut m = MeshData::default();
    for i in 0..=n {
        let a = i as f64 / n as f64 * TAU;
        let (s, c) = a.sin_cos();
        for z in [0.0, h] {
            m.positions.push([(r * c) as f32, (r * s) as f32, z as f32]);
            m.normals.push([c as f32, s as f32, 0.0]);
        }
    }
    for i in 0..n {
        let b = i * 2;
        m.indices.extend_from_slice(&[b, b + 2, b + 3, b, b + 3, b + 1]);
    }
    for (z, nz) in [(0.0, -1.0f32), (h, 1.0)] {
        let center = m.positions.len() as u32;
        m.positions.push([0.0, 0.0, z as f32]);
        m.normals.push([0.0, 0.0, nz]);
        for i in 0..=n {
            let a = i as f64 / n as f64 * TAU;
            m.positions.push([(r * a.cos()) as f32, (r * a.sin()) as f32, z as f32]);
            m.normals.push([0.0, 0.0, nz]);
        }
        for i in 0..n {
            let (p, q) = (center + 1 + i, center + 2 + i);
            if nz > 0.0 {
                m.indices.extend_from_slice(&[center, p, q]);
            } else {
                m.indices.extend_from_slice(&[center, q, p]);
            }
        }
    }
    m
}

fn box_edges(min: DVec3, max: DVec3, color: [f32; 4]) -> LineSet3D {
    let mut l = LineSet3D::new();
    let p = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    };
    for (a, b) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
        l.push_segment_f64(p(a), p(b), color);
    }
    l
}

fn build_3d_scene(r: &mut Renderer, origin: DVec3) {
    let place = DMat4::from_translation(origin);
    let edge = rgb8(20, 22, 26);

    // Cube with the top face (+Z, indices 30..36) highlighted.
    let (cmin, cmax) = (DVec3::new(-20.0, -20.0, 0.0), DVec3::new(20.0, 20.0, 40.0));
    r.upload_mesh(MeshId(1), &box_mesh(cmin, cmax));
    r.set_mesh_style(
        MeshId(1),
        MeshStyle {
            transform: place,
            color: rgb8(170, 176, 186),
            highlight_ranges: vec![(30..36, rgba8(255, 150, 40, 200))],
            visible: true,
        },
    );
    let mut cube_edges = box_edges(cmin, cmax, edge);
    // Highlight one vertical edge (segment 11: corner 3 -> 7).
    let hl = LineStyle3D {
        transform: place,
        width_px: 1.5,
        highlight_ranges: vec![(11..12, rgb8(255, 230, 0))],
        ..Default::default()
    };
    cube_edges.segments.shrink_to_fit();
    r.upload_lines3d(LineSetId(1), &cube_edges);
    r.set_lines3d_style(LineSetId(1), hl);

    // Smooth cylinder.
    let cyl_at = DMat4::from_translation(origin + DVec3::new(55.0, 25.0, 0.0));
    r.upload_mesh(MeshId(2), &cylinder_mesh(15.0, 50.0, 64));
    r.set_mesh_style(MeshId(2), MeshStyle { transform: cyl_at, color: rgb8(70, 130, 210), ..Default::default() });
    let mut cyl_edges = LineSet3D::new();
    for z in [0.0, 50.0] {
        let pts: Vec<DVec3> = (0..=64)
            .map(|i| DVec3::new(15.0 * (i as f64 / 64.0 * TAU).cos(), 15.0 * (i as f64 / 64.0 * TAU).sin(), z))
            .collect();
        cyl_edges.push_polyline_f64(&pts, edge);
    }
    r.upload_lines3d(LineSetId(2), &cyl_edges);
    r.set_lines3d_style(LineSetId(2), LineStyle3D { transform: cyl_at, ..Default::default() });

    // Transparent box behind the cube with its edges (visible through it).
    let (tmin, tmax) = (DVec3::new(-70.0, 10.0, 0.0), DVec3::new(-35.0, 45.0, 30.0));
    r.upload_mesh(MeshId(3), &box_mesh(tmin, tmax));
    r.set_mesh_style(MeshId(3), MeshStyle { transform: place, color: rgba8(120, 220, 160, 90), ..Default::default() });
    r.upload_lines3d(LineSetId(3), &box_edges(tmin, tmax, rgb8(60, 140, 90)));
    r.set_lines3d_style(LineSetId(3), LineStyle3D { transform: place, width_px: 1.0, ..Default::default() });

    // Sketch-like curve on a tilted plane, and axes drawn on top.
    let plane = Plane::from_normal(DVec3::new(0.0, -40.0, 20.0), DVec3::new(0.0, -1.0, 0.3));
    let mut sketch = LineSet3D::new();
    let pts: Vec<DVec2> = (0..=48).map(|i| DVec2::from_angle(i as f64 / 48.0 * TAU) * DVec2::new(18.0, 10.0)).collect();
    sketch.push_plane_polyline(&plane, &pts, rgb8(255, 120, 200));
    r.upload_lines3d(LineSetId(4), &sketch);
    r.set_lines3d_style(LineSetId(4), LineStyle3D { transform: place, width_px: 2.0, ..Default::default() });

    let mut axes = LineSet3D::new();
    axes.push_segment_f64(DVec3::ZERO, DVec3::X * 30.0, rgb8(230, 60, 60));
    axes.push_segment_f64(DVec3::ZERO, DVec3::Y * 30.0, rgb8(60, 200, 60));
    axes.push_segment_f64(DVec3::ZERO, DVec3::Z * 30.0, rgb8(70, 110, 255));
    r.upload_lines3d(LineSetId(5), &axes);
    r.set_lines3d_style(
        LineSetId(5),
        LineStyle3D { transform: place, width_px: 3.0, on_top: true, ..Default::default() },
    );
}

// ---------------------------------------------------------------------------------------------------
// Rendering + readback

fn render_and_save_2d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut Renderer,
    vp: &Viewport,
    cam: &Camera2D,
    frame: &Frame2D,
    path: &Path,
) -> Result<(), String> {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("2d") });
    r.render_2d(device, queue, &mut enc, vp, cam, frame);
    save(device, queue, enc, vp, path)
}

fn render_and_save_3d(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    r: &mut Renderer,
    vp: &Viewport,
    cam: &Camera3D,
    frame: &Frame3D,
    path: &Path,
) -> Result<(), String> {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("3d") });
    r.render_3d(device, queue, &mut enc, vp, cam, frame);
    save(device, queue, enc, vp, path)
}

fn save(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut enc: wgpu::CommandEncoder,
    vp: &Viewport,
    path: &Path,
) -> Result<(), String> {
    let [w, h] = vp.size();
    let row = (w * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: vp.color_texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    readback.slice(..).map_async(wgpu::MapMode::Read, |r| {
        if let Err(e) = r {
            eprintln!("map failed: {e}");
        }
    });
    device.poll(wgpu::PollType::wait_indefinitely()).map_err(|e| format!("poll: {e}"))?;
    let mapped = readback.slice(..).get_mapped_range().map_err(|e| format!("map: {e:?}"))?;
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h as usize {
        let start = y * row as usize;
        px.extend_from_slice(&mapped[start..start + w as usize * 4]);
    }
    drop(mapped);
    readback.unmap();
    write_png(path, w, h, &px)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Minimal RGBA8 PNG encoder (zlib via miniz_oxide).
fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let mut raw = Vec::with_capacity((w * 4 + 1) as usize * h as usize);
    for y in 0..h as usize {
        raw.push(0); // filter: none
        raw.extend_from_slice(&rgba[y * w as usize * 4..(y + 1) * w as usize * 4]);
    }
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in
        [(b"IHDR", ihdr), (b"IDAT", miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6)), (b"IEND", Vec::new())]
    {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(kind);
        out.extend_from_slice(&data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
