//! GPU round-trip tests: render tiny scenes offscreen and check pixels. Skipped (with a message) when
//! no adapter is available, e.g. on CI machines without any GPU or software rasterizer.
#![cfg(not(target_arch = "wasm32"))]

use wcad_render::{
    Batch2D, Batch2DStyle, BatchId, Camera2D, Camera3D, DMat4, DVec2, DVec3, Frame2D, Frame3D, LineSet3D, LineSetId,
    LineStyle3D, MeshData, MeshId, MeshStyle, PointShape, Renderer, StandardView, Viewport, choose_sample_count,
};

fn device() -> Option<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("wcad-render-test"),
        required_limits: wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .ok()?;
    Some((adapter, device, queue))
}

fn read(device: &wgpu::Device, queue: &wgpu::Queue, mut enc: wgpu::CommandEncoder, vp: &Viewport) -> Vec<[u8; 4]> {
    let [w, h] = vp.size();
    let row = (w * 4).div_ceil(256) * 256;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
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
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let data = buf.slice(..).get_mapped_range().expect("mapped");
    let mut px = Vec::with_capacity((w * h) as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = y * row as usize + x * 4;
            px.push([data[i], data[i + 1], data[i + 2], data[i + 3]]);
        }
    }
    px
}

fn near(a: [u8; 4], b: [u8; 4], tol: i32) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i32 - y as i32).abs() <= tol)
}

#[test]
fn render_2d_and_3d_pixels() {
    let Some((adapter, device, queue)) = device() else {
        eprintln!("no GPU adapter: skipping GPU tests");
        return;
    };
    let mut r = Renderer::new(&device, &queue);
    for samples in [1, choose_sample_count(&adapter, 4)] {
        let vp = Viewport::new(&device, [64, 48], samples);
        let px_at = |px: &[[u8; 4]], x: usize, y: usize| px[y * 64 + x];

        // ---- 2D: world units = pixels, origin at the viewport center.
        let mut b = Batch2D::new(DVec2::new(1.0e6, 1.0e6));
        let o = b.origin;
        b.push_line(o + DVec2::new(-30.0, 0.0), o + DVec2::new(30.0, 0.0), 5.0, [1.0, 0.0, 0.0, 1.0]);
        b.push_triangle(
            o + DVec2::new(-32.0, 24.0),
            o + DVec2::new(-20.0, 24.0),
            o + DVec2::new(-32.0, 12.0),
            [0.0, 1.0, 0.0, 1.0],
        );
        b.push_point(o + DVec2::new(20.0, 15.0), 8.0, PointShape::Square, [0.0, 0.0, 1.0, 1.0]);
        r.upload_batch2d(BatchId(1), &b);
        let mut hidden = Batch2D::new(DVec2::ZERO);
        hidden.push_line(DVec2::new(1.0e6, 1.0e6 - 20.0), DVec2::new(1.0e6, 1.0e6 + 20.0), 9.0, [1.0; 4]);
        r.upload_batch2d(BatchId(2), &hidden);
        r.set_batch2d_visible(BatchId(2), false);
        let cam = Camera2D::new(o, 1.0);
        let frame = Frame2D { background: [0.0, 0.0, 0.0, 1.0], pixel_scale: 1.0 };
        assert!(r.needs_render_2d(&vp, &cam, &frame));
        let mut enc = device.create_command_encoder(&Default::default());
        r.render_2d(&device, &queue, &mut enc, &vp, &cam, &frame);
        let px = read(&device, &queue, enc, &vp);
        assert!(!r.needs_render_2d(&vp, &cam, &frame));
        assert!(near(px_at(&px, 32, 24), [255, 0, 0, 255], 2), "line {:?} (msaa {samples})", px_at(&px, 32, 24));
        assert!(near(px_at(&px, 32, 5), [0, 0, 0, 255], 0), "background {:?}", px_at(&px, 32, 5));
        assert!(near(px_at(&px, 1, 1), [0, 255, 0, 255], 2), "fill {:?}", px_at(&px, 1, 1));
        assert!(near(px_at(&px, 52, 9), [0, 0, 255, 255], 2), "point {:?}", px_at(&px, 52, 9));
        // The line is 5 px thick around y = 24: rows 22..=25 fully covered, 21 and 26 anti-aliased
        // (half coverage), 19 and 29 untouched.
        for row in 22..=25 {
            assert!(px_at(&px, 32, row)[0] > 240, "row {row}: {:?}", px_at(&px, 32, row));
        }
        for row in [21, 26] {
            let v = px_at(&px, 32, row)[0];
            assert!((60..=200).contains(&v), "fringe row {row}: {v}");
        }
        assert!(px_at(&px, 32, 19)[0] == 0 && px_at(&px, 32, 29)[0] == 0);
        // Content change dirties the view.
        r.set_batch2d_visible(BatchId(2), false);
        assert!(!r.needs_render_2d(&vp, &cam, &frame), "no-op visibility change");
        assert!(r.set_batch2d_style(BatchId(1), Batch2DStyle { color: Some([0.0, 1.0, 1.0, 1.0]), opacity: 1.0 }));
        assert!(r.needs_render_2d(&vp, &cam, &frame));
        let mut enc = device.create_command_encoder(&Default::default());
        r.render_2d(&device, &queue, &mut enc, &vp, &cam, &frame);
        let px = read(&device, &queue, enc, &vp);
        assert!(near(px_at(&px, 32, 24), [0, 255, 255, 255], 2), "override {:?}", px_at(&px, 32, 24));
        r.set_batch2d_style(BatchId(1), Batch2DStyle { color: None, opacity: 0.5 });
        let mut enc = device.create_command_encoder(&Default::default());
        r.render_2d(&device, &queue, &mut enc, &vp, &cam, &frame);
        let px = read(&device, &queue, enc, &vp);
        assert!(near(px_at(&px, 32, 24), [128, 0, 0, 255], 3), "half opacity {:?}", px_at(&px, 32, 24));

        // ---- 3D: a cube seen from the front, one highlighted face, an on-top line.
        let cube = cube_mesh();
        r.upload_mesh(MeshId(1), &cube);
        r.set_mesh_style(
            MeshId(1),
            MeshStyle {
                transform: DMat4::from_translation(DVec3::new(1.0e6, 0.0, 0.0)),
                color: [0.8, 0.8, 0.8, 1.0],
                // The -Y face (facing the Front camera) in pure yellow.
                highlight_ranges: vec![(12..18, [1.0, 1.0, 0.0, 1.0])],
                visible: true,
            },
        );
        let mut line = LineSet3D::new();
        line.push_segment([-2.0, -1.5, 0.8], [2.0, -1.5, 0.8], [1.0, 0.0, 1.0, 1.0]);
        r.upload_lines3d(LineSetId(1), &line);
        r.set_lines3d_style(
            LineSetId(1),
            LineStyle3D {
                transform: DMat4::from_translation(DVec3::new(1.0e6, 0.0, 0.0)),
                width_px: 3.0,
                on_top: true,
                ..Default::default()
            },
        );
        let mut cam3 = Camera3D { target: DVec3::new(1.0e6, 0.0, 0.0), distance: 6.0, ..Default::default() };
        cam3.set_view(StandardView::Front);
        let frame3 = Frame3D {
            background_top: [0.0, 0.0, 0.0, 1.0],
            background_bottom: [0.0, 0.0, 0.0, 1.0],
            grid: None,
            ..Default::default()
        };
        for ortho in [false, true] {
            let cam3 = Camera3D { ortho, ..cam3 };
            let mut enc = device.create_command_encoder(&Default::default());
            r.render_3d(&device, &queue, &mut enc, &vp, &cam3, &frame3);
            let px = read(&device, &queue, enc, &vp);
            let c = px_at(&px, 32, 30);
            // Lit yellow: red and green high, blue low.
            assert!(c[0] > 150 && c[1] > 150 && c[2] < 90, "face {c:?} (ortho {ortho}, msaa {samples})");
            assert!(near(px_at(&px, 1, 1), [0, 0, 0, 255], 1), "background {:?}", px_at(&px, 1, 1));
            // The on-top magenta line crosses the face above the center (z = 0.8).
            let s = cam3.world_to_screen(DVec3::new(1.0e6, -1.5, 0.8), vp.size_f64()).expect("visible");
            let (sx, sy) = (s.x.clamp(0.0, 63.0) as usize, s.y.clamp(0.0, 47.0) as usize);
            let l = px_at(&px, sx, sy);
            assert!(l[0] > 200 && l[1] < 60 && l[2] > 200, "line {l:?} at {s}");
        }
        r.clear_meshes();
        r.clear_lines3d();
        r.clear_batches2d();
    }
}

/// Unit cube with per-face normals; faces -X, +X, -Y, +Y, -Z, +Z (6 indices each).
fn cube_mesh() -> MeshData {
    let mut m = MeshData::default();
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([-1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
        ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 1.0, 0.0], [-1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0], [1.0, 0.0, 0.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ];
    for (n, u, v) in faces {
        let base = m.positions.len() as u32;
        for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            m.positions.push(std::array::from_fn(|k| n[k] + u[k] * su + v[k] * sv));
            m.normals.push(n);
        }
        m.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    m
}
