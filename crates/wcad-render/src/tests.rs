//! Unit tests: camera math, picking, display-list builders and shader validation.

use std::f64::consts::{FRAC_PI_2, PI};

use wcad_math::{BBox2, BBox3, DMat4, DVec2, DVec3, Plane};

use crate::scene3d::normalize_ranges;
use crate::{Batch2D, Camera2D, Camera3D, MeshData, PointShape, Ray3, StandardView};

const VP: DVec2 = DVec2::new(1280.0, 800.0);

fn close2(a: DVec2, b: DVec2, eps: f64) -> bool {
    (a - b).length() <= eps
}

fn close3(a: DVec3, b: DVec3, eps: f64) -> bool {
    (a - b).length() <= eps
}

/// Small deterministic pseudo-random generator (no external deps).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

// -------------------------------------------------------------------------------------------------
// Camera2D

#[test]
fn camera2d_round_trip_with_rotation_and_large_coordinates() {
    let mut rng = Lcg(1);
    for &rotation in &[0.0, 0.3, -1.2, PI] {
        let cam = Camera2D { center: DVec2::new(1.0e6 + 0.25, -2.0e6 + 0.5), px_per_unit: 250.0, rotation };
        for _ in 0..100 {
            let p = cam.center + DVec2::new(rng.range(-3.0, 3.0), rng.range(-2.0, 2.0));
            let s = cam.world_to_screen(p, VP);
            let back = cam.screen_to_world(s, VP);
            assert!(close2(p, back, 1e-9), "{p} -> {s} -> {back}");
        }
        // The center maps to the viewport center.
        assert!(close2(cam.world_to_screen(cam.center, VP), VP * 0.5, 1e-9));
    }
}

#[test]
fn camera2d_orientation_y_up_and_rotation() {
    let cam = Camera2D::new(DVec2::ZERO, 10.0);
    // +Y world goes up on screen (smaller screen y), +X goes right.
    let s = cam.world_to_screen(DVec2::new(1.0, 1.0), VP);
    assert!(close2(s, DVec2::new(650.0, 390.0), 1e-12));
    // With a 90° twist, world +Y points to the screen's right.
    let cam = Camera2D { rotation: FRAC_PI_2, ..cam };
    let s = cam.world_to_screen(DVec2::new(0.0, 1.0), VP);
    assert!(close2(s, DVec2::new(650.0, 400.0), 1e-9), "{s}");
}

#[test]
fn camera2d_zoom_about_keeps_anchor_fixed() {
    let mut cam = Camera2D { center: DVec2::new(5.0, -3.0), px_per_unit: 2.0, rotation: 0.4 };
    let cursor = DVec2::new(100.0, 700.0);
    let anchor = cam.screen_to_world(cursor, VP);
    cam.zoom_about(cursor, 3.5, VP);
    assert!((cam.px_per_unit - 7.0).abs() < 1e-12);
    assert!(close2(cam.world_to_screen(anchor, VP), cursor, 1e-9));
    // Invalid factors are ignored.
    let before = cam;
    cam.zoom_about(cursor, f64::NAN, VP);
    cam.zoom_about(cursor, -1.0, VP);
    assert_eq!(cam, before);
}

#[test]
fn camera2d_pan_follows_pointer() {
    let mut cam = Camera2D { center: DVec2::new(1.0, 2.0), px_per_unit: 4.0, rotation: 0.7 };
    let p = DVec2::new(3.0, 1.0);
    let s0 = cam.world_to_screen(p, VP);
    let delta = DVec2::new(17.0, -9.0);
    cam.pan_px(delta);
    assert!(close2(cam.world_to_screen(p, VP), s0 + delta, 1e-9));
}

#[test]
fn camera2d_fit_bbox_contains_box() {
    for &rotation in &[0.0, 0.5] {
        let mut cam = Camera2D { rotation, ..Default::default() };
        let b = BBox2::new(DVec2::new(1.0e6, 1.0e6), DVec2::new(1.0e6 + 200.0, 1.0e6 + 50.0));
        cam.fit_bbox(&b, VP, 20.0);
        for c in [b.min, b.max, DVec2::new(b.min.x, b.max.y), DVec2::new(b.max.x, b.min.y)] {
            let s = cam.world_to_screen(c, VP);
            assert!(s.x >= 19.999 && s.x <= VP.x - 19.999 && s.y >= 19.999 && s.y <= VP.y - 19.999, "{s}");
        }
        let vis = cam.visible_bbox(VP);
        assert!(vis.contains_box(&b));
    }
    // Empty boxes leave the camera alone.
    let mut cam = Camera2D::default();
    cam.fit_bbox(&BBox2::EMPTY, VP, 10.0);
    assert_eq!(cam, Camera2D::default());
}

#[test]
fn camera2d_batch_transform_matches_world_to_screen() {
    let cam = Camera2D { center: DVec2::new(1.0e6 + 0.123, 1.0e6 + 0.456), px_per_unit: 1000.0, rotation: 0.25 };
    let origin = DVec2::new(1.0e6, 1.0e6);
    let (m, t) = cam.batch_transform(origin);
    for v in [DVec2::new(0.1, 0.2), DVec2::new(0.123, 0.456), DVec2::new(-0.3, 0.05)] {
        let local = [v.x as f32, v.y as f32];
        // What the shader computes (centered, Y-up pixels), then converted to top-left pixels.
        let px = [m[0] * local[0] + m[2] * local[1] + t[0], m[1] * local[0] + m[3] * local[1] + t[1]];
        let shader = DVec2::new(VP.x * 0.5 + px[0] as f64, VP.y * 0.5 - px[1] as f64);
        let exact = cam.world_to_screen(origin + v, VP);
        // f32 math on small numbers: well below a pixel even at 1000 px/unit around 1e6.
        assert!(close2(shader, exact, 1e-3), "{shader} vs {exact}");
    }
}

// -------------------------------------------------------------------------------------------------
// Camera3D

fn cameras() -> Vec<Camera3D> {
    let mut v = Vec::new();
    for ortho in [false, true] {
        for view in [StandardView::IsoSE, StandardView::Top, StandardView::Front, StandardView::Bottom] {
            let (yaw, pitch) = view.yaw_pitch();
            v.push(Camera3D {
                target: DVec3::new(1.0e6, -5.0e5, 250.0),
                distance: 350.0,
                yaw,
                pitch,
                fov_y: 50f64.to_radians(),
                ortho,
            });
        }
    }
    v
}

#[test]
fn camera3d_basis_and_view_matrix() {
    for cam in cameras() {
        let (r, u, f) = (cam.right(), cam.up(), cam.forward());
        assert!((r.length() - 1.0).abs() < 1e-12 && (u.length() - 1.0).abs() < 1e-12);
        assert!(r.dot(u).abs() < 1e-12 && r.dot(f).abs() < 1e-12 && u.dot(f).abs() < 1e-12);
        // Right-handed: right × up = -forward (camera looks down -Z).
        assert!(close3(r.cross(u), -f, 1e-12));
        let v = cam.view_matrix();
        assert!(close3(v.transform_point3(cam.eye()), DVec3::ZERO, 1e-6));
        assert!(close3(v.transform_point3(cam.target), DVec3::new(0.0, 0.0, -350.0), 1e-6));
    }
}

#[test]
fn camera3d_standard_views() {
    let mut cam = Camera3D::default();
    cam.set_view(StandardView::Front);
    assert!(close3(cam.forward(), DVec3::Y, 1e-12));
    assert!(close3(cam.right(), DVec3::X, 1e-12));
    assert!(close3(cam.up(), DVec3::Z, 1e-12));
    cam.set_view(StandardView::Top);
    assert!(close3(cam.forward(), -DVec3::Z, 1e-12));
    assert!(close3(cam.right(), DVec3::X, 1e-12));
    assert!(close3(cam.up(), DVec3::Y, 1e-12));
    cam.set_view(StandardView::Right);
    assert!(close3(cam.forward(), -DVec3::X, 1e-12));
    assert!(close3(cam.right(), DVec3::Y, 1e-12));
    cam.set_view(StandardView::IsoSE);
    let d = cam.view_dir();
    assert!(d.x > 0.0 && d.y < 0.0 && d.z > 0.0);
    assert!((d.x - d.z).abs() < 1e-12 && (d.x + d.y).abs() < 1e-12, "{d}");
}

#[test]
fn camera3d_look_at_plane() {
    let mut cam = Camera3D::default();
    cam.look_at_plane(&Plane::XY);
    assert!(close3(cam.right(), DVec3::X, 1e-9) && close3(cam.up(), DVec3::Y, 1e-9));
    cam.look_at_plane(&Plane::YZ);
    assert!(close3(cam.right(), DVec3::Y, 1e-9) && close3(cam.up(), DVec3::Z, 1e-9));
    // XZ's normal is -Y: looking from -Y is the Front view.
    cam.look_at_plane(&Plane::XZ);
    assert!(close3(cam.right(), DVec3::X, 1e-9) && close3(cam.up(), DVec3::Z, 1e-9));
    // An offset plane: the target is moved onto it.
    let p = Plane::from_normal(DVec3::new(0.0, 0.0, 7.0), DVec3::Z);
    cam.target = DVec3::new(1.0, 2.0, 3.0);
    cam.look_at_plane(&p);
    assert!(close3(cam.target, DVec3::new(1.0, 2.0, 7.0), 1e-12));
    assert!(close3(cam.view_dir(), DVec3::Z, 1e-9));
    assert!(close3(cam.right(), p.x_axis, 1e-9));
}

#[test]
fn camera3d_projection_matches_world_to_screen() {
    let mut rng = Lcg(7);
    for cam in cameras() {
        let aspect = VP.x / VP.y;
        let bbox = BBox3 { min: cam.target - DVec3::splat(100.0), max: cam.target + DVec3::splat(100.0) };
        let (near, far) = cam.depth_range(&bbox);
        let vp = cam.proj_matrix(aspect, near, far) * cam.view_matrix();
        for _ in 0..50 {
            let p = cam.target + DVec3::new(rng.range(-90.0, 90.0), rng.range(-90.0, 90.0), rng.range(-90.0, 90.0));
            let clip = vp * p.extend(1.0);
            assert!(clip.w > 0.0);
            let ndc = clip.truncate() / clip.w;
            // Reverse-Z: everything inside the scene box lies inside the depth range.
            assert!(ndc.z > 0.0 && ndc.z <= 1.0, "depth {}", ndc.z);
            let from_clip = DVec2::new((ndc.x + 1.0) * 0.5 * VP.x, (1.0 - ndc.y) * 0.5 * VP.y);
            let s = cam.world_to_screen(p, VP).expect("in front");
            assert!(close2(s, from_clip, 1e-6), "{s} vs {from_clip}");
        }
        // Target projects to the viewport center.
        assert!(close2(cam.world_to_screen(cam.target, VP).expect("visible"), VP * 0.5, 1e-6));
    }
}

#[test]
fn camera3d_reverse_z_ordering() {
    let cam = Camera3D { target: DVec3::ZERO, distance: 10.0, ..Default::default() };
    let (near, far) = cam.depth_range(&BBox3 { min: DVec3::splat(-1.0), max: DVec3::splat(1.0) });
    let m = cam.proj_matrix(1.0, near, far) * cam.view_matrix();
    let depth = |p: DVec3| {
        let c = m * p.extend(1.0);
        c.z / c.w
    };
    let near_p = cam.eye() + cam.forward() * 5.0;
    let far_p = cam.eye() + cam.forward() * 15.0;
    assert!(depth(near_p) > depth(far_p));
    // Exactly at the near plane depth is 1.
    assert!((depth(cam.eye() + cam.forward() * near) - 1.0).abs() < 1e-9);
}

#[test]
fn camera3d_ray_passes_through_projected_point() {
    let mut rng = Lcg(3);
    for cam in cameras() {
        for _ in 0..50 {
            let p = cam.target + DVec3::new(rng.range(-80.0, 80.0), rng.range(-80.0, 80.0), rng.range(-80.0, 80.0));
            let s = cam.world_to_screen(p, VP).expect("in front");
            let ray = cam.ray_from_screen(s, VP);
            assert!((ray.dir.length() - 1.0).abs() < 1e-12);
            // Distance from p to the ray line.
            let t = (p - ray.origin).dot(ray.dir);
            let d = (ray.at(t) - p).length();
            assert!(d < 1e-6, "distance {d} (ortho {})", cam.ortho);
            assert!(t >= cam.pick_t_min());
        }
        // The center pixel looks straight at the target.
        let ray = cam.ray_from_screen(VP * 0.5, VP);
        assert!(close3(ray.dir, cam.forward(), 1e-12));
    }
}

#[test]
fn camera3d_zoom_about_keeps_anchor() {
    for ortho in [false, true] {
        let mut cam = Camera3D { ortho, target: DVec3::new(10.0, 20.0, 30.0), distance: 200.0, ..Default::default() };
        let cursor = DVec2::new(900.0, 150.0);
        let plane = Plane::from_normal(cam.target, cam.view_dir());
        let ray = cam.ray_from_screen(cursor, VP);
        let anchor = ray.at(ray.intersect_plane(&plane).expect("hit"));
        cam.zoom_about(cursor, 2.0, VP);
        assert!((cam.distance - 100.0).abs() < 1e-9);
        let s = cam.world_to_screen(anchor, VP).expect("visible");
        assert!(close2(s, cursor, 1e-6), "{s}");
    }
}

#[test]
fn camera3d_pan_follows_pointer_at_target_depth() {
    for ortho in [false, true] {
        let mut cam = Camera3D { ortho, ..Default::default() };
        let s0 = cam.world_to_screen(cam.target, VP).expect("visible");
        let p = cam.target;
        cam.pan_px(DVec2::new(40.0, -25.0), VP);
        let s1 = cam.world_to_screen(p, VP).expect("visible");
        assert!(close2(s1, s0 + DVec2::new(40.0, -25.0), 1e-6), "{s1}");
    }
}

#[test]
fn camera3d_fit_bbox_shows_all_corners() {
    for ortho in [false, true] {
        for aspect_vp in [VP, DVec2::new(600.0, 900.0)] {
            let mut cam = Camera3D { ortho, ..Default::default() };
            let b = BBox3 { min: DVec3::new(-50.0, 0.0, 10.0), max: DVec3::new(150.0, 30.0, 40.0) };
            cam.fit_bbox(&b, aspect_vp.x / aspect_vp.y);
            assert!(close3(cam.target, b.center(), 1e-12));
            for i in 0..8 {
                let c = DVec3::new(
                    if i & 1 == 0 { b.min.x } else { b.max.x },
                    if i & 2 == 0 { b.min.y } else { b.max.y },
                    if i & 4 == 0 { b.min.z } else { b.max.z },
                );
                let s = cam.world_to_screen(c, aspect_vp).expect("in front");
                assert!(s.x >= 0.0 && s.y >= 0.0 && s.x <= aspect_vp.x && s.y <= aspect_vp.y, "{s} ortho {ortho}");
            }
        }
    }
}

#[test]
fn camera3d_survives_garbage() {
    let cam = Camera3D { distance: f64::NAN, yaw: f64::INFINITY, pitch: 10.0, fov_y: -1.0, ..Default::default() };
    assert!(cam.view_matrix().is_finite());
    assert!(cam.proj_matrix(f64::NAN, -1.0, -2.0).is_finite());
    let r = cam.ray_from_screen(DVec2::new(f64::NAN, 3.0), DVec2::ZERO);
    assert!(r.origin.is_finite());
}

// -------------------------------------------------------------------------------------------------
// Picking

fn unit_cube() -> MeshData {
    // 8 corners, 12 triangles; faces: -Z, +Z, -Y, +Y, -X, +X (two triangles each).
    let positions: Vec<[f32; 3]> = (0..8)
        .map(|i| [(i & 1) as f32 * 2.0 - 1.0, ((i >> 1) & 1) as f32 * 2.0 - 1.0, ((i >> 2) & 1) as f32 * 2.0 - 1.0])
        .collect();
    let indices = vec![
        0, 2, 3, 0, 3, 1, // -Z
        4, 5, 7, 4, 7, 6, // +Z
        0, 1, 5, 0, 5, 4, // -Y
        2, 6, 7, 2, 7, 3, // +Y
        0, 4, 6, 0, 6, 2, // -X
        1, 3, 7, 1, 7, 5, // +X
    ];
    MeshData { positions, normals: Vec::new(), indices }
}

#[test]
fn ray_triangle_and_plane() {
    let ray = Ray3 { origin: DVec3::new(0.2, 0.2, 5.0), dir: -DVec3::Z };
    let t = ray.intersect_triangle(DVec3::ZERO, DVec3::X, DVec3::Y).expect("hit");
    assert!((t - 5.0).abs() < 1e-12);
    assert!(
        ray.intersect_triangle(DVec3::new(1.0, 1.0, 0.0), DVec3::new(2.0, 1.0, 0.0), DVec3::new(1.0, 2.0, 0.0))
            .is_none()
    );
    let t = ray.intersect_plane(&Plane::XY).expect("hit");
    assert!((t - 5.0).abs() < 1e-12);
    let parallel = Ray3 { origin: DVec3::Z, dir: DVec3::X };
    assert!(parallel.intersect_plane(&Plane::XY).is_none());
    // Segment closest approach: the ray passes 1 unit above the segment's middle.
    let r = Ray3 { origin: DVec3::new(0.0, -5.0, 1.0), dir: DVec3::Y };
    let (t, s, d) = r.closest_to_segment(DVec3::new(-1.0, 0.0, 0.0), DVec3::new(1.0, 0.0, 0.0));
    assert!((t - 5.0).abs() < 1e-12 && (s - 0.5).abs() < 1e-12 && (d - 1.0).abs() < 1e-12);
}

#[test]
fn pick_mesh_through_camera_ray() {
    let mesh = unit_cube();
    let transform = DMat4::from_translation(DVec3::new(1.0e6, 2.0e6, 0.0));
    for ortho in [false, true] {
        let mut cam = Camera3D { ortho, target: DVec3::new(1.0e6, 2.0e6, 0.0), distance: 10.0, ..Default::default() };
        cam.set_view(StandardView::Front); // looking along +Y: the -Y face (triangles 4, 5) is in front
        let ray = cam.ray_from_screen(VP * 0.5 + DVec2::new(10.0, -5.0), VP);
        let (tri, t) = mesh.pick(&ray, &transform, cam.pick_t_min()).expect("hit");
        assert!(tri == 4 || tri == 5, "picked triangle {tri}");
        let hit = ray.at(t);
        assert!((hit.y - (2.0e6 - 1.0)).abs() < 1e-6, "{hit}");
        // Looking away misses.
        let away = Ray3 { origin: ray.origin, dir: -ray.dir };
        if !ortho {
            assert!(mesh.pick(&away, &transform, 0.0).is_none());
        }
    }
}

#[test]
fn mesh_normals_and_bbox() {
    let mesh = unit_cube();
    let n = mesh.compute_normals();
    assert_eq!(n.len(), 8);
    // Corner normals of a cube point diagonally outwards.
    for (p, n) in mesh.positions.iter().zip(&n) {
        let p = DVec3::from(p.map(f64::from)).normalize();
        let n = DVec3::from(n.map(f64::from));
        assert!(p.dot(n) > 0.99, "{p} {n}");
    }
    let b = mesh.bbox();
    assert_eq!(b.min, DVec3::splat(-1.0));
    assert_eq!(b.max, DVec3::splat(1.0));
    // Invalid indices do not panic.
    let bad = MeshData { indices: vec![0, 1, 99], ..mesh };
    let _ = bad.compute_normals();
    let ray = Ray3 { origin: DVec3::new(0.0, 0.0, 5.0), dir: -DVec3::Z };
    assert!(bad.pick(&ray, &DMat4::IDENTITY, 0.0).is_none());
}

// -------------------------------------------------------------------------------------------------
// Display lists

#[test]
fn batch2d_local_coordinates_and_bbox() {
    let mut b = Batch2D::new(DVec2::new(1.0e6, 1.0e6));
    b.push_line(DVec2::new(1.0e6 + 0.001, 1.0e6), DVec2::new(1.0e6 + 1.0, 1.0e6 + 2.0), 1.0, [1.0; 4]);
    b.push_point(DVec2::new(1.0e6 - 3.0, 1.0e6), 6.0, PointShape::Cross, [1.0; 4]);
    b.push_triangle(
        DVec2::new(1.0e6, 1.0e6 + 5.0),
        DVec2::new(1.0e6 + 1.0, 1.0e6 + 5.0),
        DVec2::new(1.0e6, 1.0e6 + 6.0),
        [0.5; 4],
    );
    // Relative storage keeps sub-millimetre detail at 1e6.
    assert_eq!(b.lines[0].a, [0.001, 0.0]);
    let bb = b.bbox();
    assert!(close2(bb.min, DVec2::new(1.0e6 - 3.0, 1.0e6), 1e-6));
    assert!(close2(bb.max, DVec2::new(1.0e6 + 1.0, 1.0e6 + 6.0), 1e-6));
    // Non-finite input is dropped, not stored.
    b.push_line(DVec2::new(f64::NAN, 0.0), DVec2::ZERO, 1.0, [1.0; 4]);
    b.push_point(DVec2::new(0.0, f64::INFINITY), 1.0, PointShape::Square, [1.0; 4]);
    assert_eq!(b.lines.len(), 1);
    assert_eq!(b.points.len(), 1);
    b.push_triangles(&[DVec2::ZERO, DVec2::X, DVec2::Y], &[0, 1, 2, 0, 1, 7], [1.0; 4]);
    assert_eq!(b.fill_indices.len(), 6, "out-of-range triangle skipped");
    b.clear();
    assert!(b.is_empty());
}

#[test]
fn batch2d_dashes() {
    let mut b = Batch2D::new(DVec2::ZERO);
    b.push_dashed_polyline(&[DVec2::ZERO, DVec2::new(10.0, 0.0)], false, &[2.0, -1.0], 0.0, 1.0, [1.0; 4]);
    // 10 = 3 full periods (2 dash + 1 gap) + 1 dash unit.
    assert_eq!(b.lines.len(), 4);
    let total: f32 = b.lines.iter().map(|l| (l.b[0] - l.a[0]).abs()).sum();
    assert!((total - 7.0).abs() < 1e-5, "{total}");
    // Pattern continues around corners.
    b.clear();
    b.push_dashed_polyline(
        &[DVec2::ZERO, DVec2::new(3.0, 0.0), DVec2::new(3.0, 3.0)],
        false,
        &[2.0, -2.0],
        0.0,
        1.0,
        [1.0; 4],
    );
    let len = |l: &crate::LineSegment2D| ((l.b[0] - l.a[0]).powi(2) + (l.b[1] - l.a[1]).powi(2)).sqrt();
    let lens: Vec<f32> = b.lines.iter().map(len).collect();
    assert_eq!(lens.len(), 2, "{lens:?}");
    // dash 0..2 on the first leg, gap 2..3 + 0..1 across the corner, dash 1..3 on the second leg
    assert!((lens[0] - 2.0).abs() < 1e-5 && (lens[1] - 2.0).abs() < 1e-5, "{lens:?}");
    assert_eq!(b.lines[1].a, [3.0, 1.0]);
    // Dots (0) become zero-length segments; phase shifts the start.
    b.clear();
    b.push_dashed_polyline(&[DVec2::ZERO, DVec2::new(4.0, 0.0)], false, &[0.0, -1.0], 0.0, 2.0, [1.0; 4]);
    assert_eq!(b.lines.len(), 5);
    assert!(b.lines.iter().all(|l| l.a == l.b));
    b.clear();
    b.push_dashed_polyline(&[DVec2::ZERO, DVec2::new(3.0, 0.0)], false, &[2.0, -1.0], 1.5, 1.0, [1.0; 4]);
    assert!((b.lines[0].b[0] - 0.5).abs() < 1e-6, "{:?}", b.lines[0]);
    // Degenerate patterns fall back to a solid line.
    b.clear();
    b.push_dashed_polyline(&[DVec2::ZERO, DVec2::X, DVec2::Y], true, &[], 0.0, 1.0, [1.0; 4]);
    assert_eq!(b.lines.len(), 3);
    b.clear();
    b.push_dashed_polyline(&[DVec2::ZERO, DVec2::new(1.0e9, 0.0)], false, &[1e-6, -1e-6], 0.0, 1.0, [1.0; 4]);
    assert_eq!(b.lines.len(), 1, "absurd dash counts are drawn solid");
}

#[test]
fn highlight_ranges_are_normalized() {
    let red = [1.0, 0.0, 0.0, 1.0];
    let blue = [0.0, 0.0, 1.0, 1.0];
    let r = normalize_ranges(&[(7..11, red), (0..2, blue), (9..30, blue), (100..200, red), (5..5, red)], 24, 3);
    assert_eq!(r.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>(), vec![0..3, 6..12, 12..24]);
    let pieces = crate::renderer::split_ranges(30, &r, [0.5; 4], true);
    let ranges: Vec<_> = pieces.iter().map(|(r, _, h)| (r.clone(), *h)).collect();
    assert_eq!(ranges, vec![(0..3, true), (3..6, false), (6..12, true), (12..24, true), (24..30, false)]);
    // Blending over the base keeps the base alpha.
    let half = crate::renderer::split_ranges(3, &[(0..3, [1.0, 1.0, 1.0, 0.5])], [0.0, 0.0, 0.0, 0.25], true);
    assert_eq!(half[0].1, [0.5, 0.5, 0.5, 0.25]);
}

#[test]
fn color_packing() {
    assert_eq!(crate::pack_rgba([1.0, 0.0, 0.5, f32::NAN]), [255, 0, 128, 0]);
    assert_eq!(crate::pack_rgba([2.0, -1.0, 0.2, 1.0]), [255, 0, 51, 255]);
    assert_eq!(crate::rgb8(10, 20, 30)[3], 1.0);
}

// -------------------------------------------------------------------------------------------------
// Shaders

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn shaders_validate_and_translate_to_webgl2_glsl() {
    use naga::back::glsl;
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    for (label, src) in crate::shader_modules() {
        let module =
            naga::front::wgsl::parse_str(&src).unwrap_or_else(|e| panic!("{label}: {}", e.emit_to_string(&src)));
        let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        assert!(!module.entry_points.is_empty());
        for ep in &module.entry_points {
            let options = glsl::Options {
                version: glsl::Version::Embedded { version: 300, is_webgl: true },
                ..Default::default()
            };
            let pipeline =
                glsl::PipelineOptions { shader_stage: ep.stage, entry_point: ep.name.clone(), multiview: None };
            let mut out = String::new();
            let mut w = glsl::Writer::new(&mut out, &module, &info, &options, &pipeline, Default::default())
                .unwrap_or_else(|e| panic!("{label}::{}: {e:?}", ep.name));
            w.write().unwrap_or_else(|e| panic!("{label}::{}: {e:?}", ep.name));
            assert!(out.starts_with("#version 300 es"), "{label}::{}", ep.name);
            assert!(!out.contains("noperspective"), "{label}::{} needs noperspective", ep.name);
        }
    }
}
