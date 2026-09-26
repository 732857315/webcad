//! 2D and 3D cameras. All math is `f64`; the renderer converts the final (camera-relative) matrices
//! to `f32`.
//!
//! Screen coordinates are pixels with the origin at the top-left of the viewport and Y pointing down
//! (egui convention). The helpers are unit-agnostic as long as `viewport` and the zoom are expressed
//! in the same unit; the renderer itself works in physical pixels.

use wcad_math::{BBox2, BBox3, DMat4, DVec2, DVec3, DVec4, Plane};

/// Smallest / largest zoom accepted by the 2D camera helpers.
const MIN_PX_PER_UNIT: f64 = 1e-12;
const MAX_PX_PER_UNIT: f64 = 1e12;

/// 2D view: which world point is at the viewport center, the zoom and an optional view twist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2D {
    /// World point shown at the center of the viewport.
    pub center: DVec2,
    /// Zoom: pixels per world unit (must be > 0).
    pub px_per_unit: f64,
    /// View twist in radians: the world direction `(cos r, sin r)` points to the screen's right.
    pub rotation: f64,
}

impl Default for Camera2D {
    fn default() -> Self {
        Self {
            center: DVec2::ZERO,
            px_per_unit: 1.0,
            rotation: 0.0,
        }
    }
}

impl Camera2D {
    pub fn new(center: DVec2, px_per_unit: f64) -> Self {
        Self {
            center,
            px_per_unit,
            rotation: 0.0,
        }
    }

    fn scale(&self) -> f64 {
        if self.px_per_unit.is_finite() && self.px_per_unit > 0.0 {
            self.px_per_unit.clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT)
        } else {
            1.0
        }
    }

    /// World units covered by one pixel.
    pub fn units_per_px(&self) -> f64 {
        1.0 / self.scale()
    }

    /// Screen-right direction in world space.
    pub fn right(&self) -> DVec2 {
        DVec2::from_angle(self.rotation)
    }

    /// Screen-up direction in world space.
    pub fn up(&self) -> DVec2 {
        wcad_math::perp(self.right())
    }

    /// World → screen pixels (top-left origin, Y down).
    pub fn world_to_screen(&self, p: DVec2, viewport: DVec2) -> DVec2 {
        let d = p - self.center;
        let s = self.scale();
        let local = DVec2::new(d.dot(self.right()), d.dot(self.up())) * s;
        DVec2::new(viewport.x * 0.5 + local.x, viewport.y * 0.5 - local.y)
    }

    /// Screen pixels (top-left origin, Y down) → world.
    pub fn screen_to_world(&self, s: DVec2, viewport: DVec2) -> DVec2 {
        let local = DVec2::new(s.x - viewport.x * 0.5, viewport.y * 0.5 - s.y) / self.scale();
        self.center + self.right() * local.x + self.up() * local.y
    }

    /// Converts a pixel distance to world units.
    pub fn px_to_units(&self, px: f64) -> f64 {
        px / self.scale()
    }

    /// Moves the view so the content follows a pointer drag of `delta_px` (screen pixels, Y down).
    pub fn pan_px(&mut self, delta_px: DVec2) {
        let s = self.scale();
        self.center -= (self.right() * delta_px.x - self.up() * delta_px.y) / s;
    }

    /// Multiplies the zoom by `factor`, keeping the world point under `screen_px` fixed.
    pub fn zoom_about(&mut self, screen_px: DVec2, factor: f64, viewport: DVec2) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let anchor = self.screen_to_world(screen_px, viewport);
        self.px_per_unit = (self.scale() * factor).clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT);
        let now = self.screen_to_world(screen_px, viewport);
        self.center += anchor - now;
    }

    /// Centers `bbox` and zooms so it fits inside the viewport with `margin_px` on every side.
    /// Empty or degenerate boxes only re-center (and keep the zoom).
    pub fn fit_bbox(&mut self, bbox: &BBox2, viewport: DVec2, margin_px: f64) {
        if bbox.is_empty() || !bbox.min.is_finite() || !bbox.max.is_finite() {
            return;
        }
        self.center = bbox.center();
        // Extent of the box along the (possibly rotated) screen axes.
        let (r, u) = (self.right(), self.up());
        let corners = [
            bbox.min,
            DVec2::new(bbox.max.x, bbox.min.y),
            bbox.max,
            DVec2::new(bbox.min.x, bbox.max.y),
        ];
        let (mut w, mut h) = (0.0f64, 0.0f64);
        for c in corners {
            let d = c - self.center;
            w = w.max(d.dot(r).abs() * 2.0);
            h = h.max(d.dot(u).abs() * 2.0);
        }
        let avail = (viewport - DVec2::splat(2.0 * margin_px)).max(DVec2::ONE);
        let sx = if w > 0.0 { avail.x / w } else { f64::INFINITY };
        let sy = if h > 0.0 { avail.y / h } else { f64::INFINITY };
        let s = sx.min(sy);
        if s.is_finite() {
            self.px_per_unit = s.clamp(MIN_PX_PER_UNIT, MAX_PX_PER_UNIT);
        }
    }

    /// World-space axis-aligned box covering the whole viewport (for culling).
    pub fn visible_bbox(&self, viewport: DVec2) -> BBox2 {
        BBox2::from_points([
            self.screen_to_world(DVec2::ZERO, viewport),
            self.screen_to_world(DVec2::new(viewport.x, 0.0), viewport),
            self.screen_to_world(viewport, viewport),
            self.screen_to_world(DVec2::new(0.0, viewport.y), viewport),
        ])
    }

    /// Linear map for a batch whose vertices are relative to `origin`: returns the 2×2 matrix
    /// (column-major `[m00, m10, m01, m11]`) and translation taking local coordinates to *centered,
    /// Y-up* pixel coordinates. The translation is computed in `f64` before the cast.
    pub(crate) fn batch_transform(&self, origin: DVec2) -> ([f32; 4], [f32; 2]) {
        let s = self.scale();
        let (r, u) = (self.right(), self.up());
        // local pixel = [r·v, u·v] * s  →  columns are (r.x, u.x)*s and (r.y, u.y)*s
        let m = [
            (r.x * s) as f32,
            (u.x * s) as f32,
            (r.y * s) as f32,
            (u.y * s) as f32,
        ];
        let d = origin - self.center;
        let t = [(d.dot(r) * s) as f32, (d.dot(u) * s) as f32];
        (m, t)
    }
}

// -------------------------------------------------------------------------------------------------

/// Named view directions (Z up).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StandardView {
    Top,
    Bottom,
    Front,
    Back,
    Left,
    Right,
    /// South-east isometric: looking from (+X, −Y, +Z).
    IsoSE,
    /// South-west isometric: looking from (−X, −Y, +Z).
    IsoSW,
    /// North-east isometric: looking from (+X, +Y, +Z).
    IsoNE,
    /// North-west isometric: looking from (−X, +Y, +Z).
    IsoNW,
}

impl StandardView {
    /// `(yaw, pitch)` in radians for [`Camera3D`].
    pub fn yaw_pitch(self) -> (f64, f64) {
        use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};
        let iso = (1.0f64 / 2.0f64.sqrt()).atan(); // 35.264°
        match self {
            StandardView::Top => (0.0, FRAC_PI_2),
            StandardView::Bottom => (0.0, -FRAC_PI_2),
            StandardView::Front => (0.0, 0.0),
            StandardView::Back => (PI, 0.0),
            StandardView::Left => (-FRAC_PI_2, 0.0),
            StandardView::Right => (FRAC_PI_2, 0.0),
            StandardView::IsoSE => (FRAC_PI_4, iso),
            StandardView::IsoSW => (-FRAC_PI_4, iso),
            StandardView::IsoNE => (3.0 * FRAC_PI_4, iso),
            StandardView::IsoNW => (-3.0 * FRAC_PI_4, iso),
        }
    }
}

/// A ray (or, for orthographic cameras, a line) in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray3 {
    pub origin: DVec3,
    /// Unit direction.
    pub dir: DVec3,
}

impl Ray3 {
    pub fn at(&self, t: f64) -> DVec3 {
        self.origin + self.dir * t
    }

    /// Parameter of the intersection with `plane`, if the ray is not parallel to it.
    pub fn intersect_plane(&self, plane: &Plane) -> Option<f64> {
        let n = plane.normal();
        let denom = self.dir.dot(n);
        if denom.abs() < 1e-15 {
            return None;
        }
        let t = (plane.origin - self.origin).dot(n) / denom;
        t.is_finite().then_some(t)
    }

    /// Möller–Trumbore ray/triangle intersection (both sides). Returns the ray parameter.
    pub fn intersect_triangle(&self, a: DVec3, b: DVec3, c: DVec3) -> Option<f64> {
        let e1 = b - a;
        let e2 = c - a;
        let p = self.dir.cross(e2);
        let det = e1.dot(p);
        let scale = e1.length_squared().max(e2.length_squared());
        if det.abs() <= 1e-14 * scale || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        let s = self.origin - a;
        let u = s.dot(p) * inv;
        if !(-1e-12..=1.0 + 1e-12).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = self.dir.dot(q) * inv;
        if v < -1e-12 || u + v > 1.0 + 1e-12 {
            return None;
        }
        Some(e2.dot(q) * inv)
    }

    /// Closest approach between the ray line and segment `a..b`: `(t_ray, s_segment in 0..=1, distance)`.
    pub fn closest_to_segment(&self, a: DVec3, b: DVec3) -> (f64, f64, f64) {
        let d2 = b - a;
        let r = self.origin - a;
        let aa = self.dir.dot(self.dir);
        let ee = d2.dot(d2);
        let f = d2.dot(r);
        let (t, s);
        if ee <= 1e-300 {
            s = 0.0;
            t = -self.dir.dot(r) / aa.max(1e-300);
        } else {
            let c = self.dir.dot(r);
            let bb = self.dir.dot(d2);
            let denom = aa * ee - bb * bb;
            let s0 = if denom.abs() > 1e-300 {
                ((bb * c - aa * f) / denom).clamp(0.0, 1.0)
            } else {
                0.0
            };
            // Line parameter for that segment point, then re-clamp the segment parameter.
            let t0 = (bb * s0 - c) / aa.max(1e-300);
            s = ((t0 * bb + f) / ee).clamp(0.0, 1.0);
            t = (bb * s - c) / aa.max(1e-300);
        }
        let dist = (self.at(t) - (a + d2 * s)).length();
        (t, s, dist)
    }
}

/// Orbit camera around `target`, Z up.
///
/// The eye sits at `target + distance * (cos(pitch)·sin(yaw), −cos(pitch)·cos(yaw), sin(pitch))`:
/// yaw 0 / pitch 0 is the Front view (looking along +Y), pitch +90° is Top. In orthographic mode the
/// visible half-height is `distance · tan(fov_y / 2)`, so toggling projection keeps the apparent size of
/// objects at the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera3D {
    pub target: DVec3,
    pub distance: f64,
    pub yaw: f64,
    pub pitch: f64,
    /// Vertical field of view in radians.
    pub fov_y: f64,
    pub ortho: bool,
}

impl Default for Camera3D {
    fn default() -> Self {
        let (yaw, pitch) = StandardView::IsoSE.yaw_pitch();
        Self {
            target: DVec3::ZERO,
            distance: 100.0,
            yaw,
            pitch,
            fov_y: 45f64.to_radians(),
            ortho: false,
        }
    }
}

impl Camera3D {
    fn dist(&self) -> f64 {
        if self.distance.is_finite() && self.distance > 0.0 {
            self.distance.clamp(1e-9, 1e15)
        } else {
            1.0
        }
    }

    fn fov(&self) -> f64 {
        if self.fov_y.is_finite() {
            self.fov_y.clamp(1e-3, 3.0)
        } else {
            45f64.to_radians()
        }
    }

    fn pitch_c(&self) -> f64 {
        use std::f64::consts::FRAC_PI_2;
        if self.pitch.is_finite() {
            self.pitch.clamp(-FRAC_PI_2, FRAC_PI_2)
        } else {
            0.0
        }
    }

    fn yaw_c(&self) -> f64 {
        if self.yaw.is_finite() { self.yaw } else { 0.0 }
    }

    /// Unit vector from the target towards the eye.
    pub fn view_dir(&self) -> DVec3 {
        let (sy, cy) = self.yaw_c().sin_cos();
        let (sp, cp) = self.pitch_c().sin_cos();
        DVec3::new(cp * sy, -cp * cy, sp)
    }

    /// Unit viewing direction (from the eye into the scene).
    pub fn forward(&self) -> DVec3 {
        -self.view_dir()
    }

    /// Screen-right direction in world space (always horizontal).
    pub fn right(&self) -> DVec3 {
        let (sy, cy) = self.yaw_c().sin_cos();
        DVec3::new(cy, sy, 0.0)
    }

    /// Screen-up direction in world space.
    pub fn up(&self) -> DVec3 {
        self.right().cross(self.forward())
    }

    pub fn eye(&self) -> DVec3 {
        self.target + self.view_dir() * self.dist()
    }

    /// Half of the visible height at the target distance (world units).
    pub fn half_height_at_target(&self) -> f64 {
        self.dist() * (self.fov() * 0.5).tan()
    }

    /// World → view (right-handed, camera looks down −Z).
    pub fn view_matrix(&self) -> DMat4 {
        let (r, u, f) = (self.right(), self.up(), self.forward());
        let e = self.eye();
        DMat4::from_cols(
            DVec4::new(r.x, u.x, -f.x, 0.0),
            DVec4::new(r.y, u.y, -f.y, 0.0),
            DVec4::new(r.z, u.z, -f.z, 0.0),
            DVec4::new(-r.dot(e), -u.dot(e), f.dot(e), 1.0),
        )
    }

    /// Reverse-Z projection (depth 1 at `near`, 0 at `far`/infinity) for wgpu clip space.
    /// Perspective uses an infinite far plane (`far` is ignored); orthographic maps `near..far`
    /// (view-space distances in front of the eye, may be negative) to `1..0`.
    pub fn proj_matrix(&self, aspect: f64, near: f64, far: f64) -> DMat4 {
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        if self.ortho {
            let h = self.half_height_at_target();
            let w = h * aspect;
            let (n, f) = if far - near > 1e-12 {
                (near, far)
            } else {
                (near, near + 1.0)
            };
            let r = 1.0 / (f - n);
            DMat4::from_cols(
                DVec4::new(1.0 / w, 0.0, 0.0, 0.0),
                DVec4::new(0.0, 1.0 / h, 0.0, 0.0),
                DVec4::new(0.0, 0.0, r, 0.0),
                DVec4::new(0.0, 0.0, f * r, 1.0),
            )
        } else {
            let near = if near.is_finite() && near > 0.0 {
                near
            } else {
                self.dist() * 1e-3
            };
            let g = 1.0 / (self.fov() * 0.5).tan();
            DMat4::from_cols(
                DVec4::new(g / aspect, 0.0, 0.0, 0.0),
                DVec4::new(0.0, g, 0.0, 0.0),
                DVec4::new(0.0, 0.0, 0.0, -1.0),
                DVec4::new(0.0, 0.0, near, 0.0),
            )
        }
    }

    /// Near/far distances for [`Self::proj_matrix`] that enclose `bbox` (plus a margin).
    pub fn depth_range(&self, bbox: &BBox3) -> (f64, f64) {
        let d = self.dist();
        if bbox.is_empty() || !bbox.min.is_finite() || !bbox.max.is_finite() {
            return if self.ortho {
                (-d * 10.0, d * 10.0)
            } else {
                (d * 1e-3, d * 1e3)
            };
        }
        let view = self.view_matrix();
        let (mut dmin, mut dmax) = (f64::INFINITY, f64::NEG_INFINITY);
        for i in 0..8 {
            let c = DVec3::new(
                if i & 1 == 0 { bbox.min.x } else { bbox.max.x },
                if i & 2 == 0 { bbox.min.y } else { bbox.max.y },
                if i & 4 == 0 { bbox.min.z } else { bbox.max.z },
            );
            let depth = -view.transform_point3(c).z;
            dmin = dmin.min(depth);
            dmax = dmax.max(depth);
        }
        if self.ortho {
            let margin = (dmax - dmin) * 0.02 + d * 1e-3 + 1e-9;
            (dmin - margin, dmax + margin)
        } else {
            let near = if dmin > 0.0 {
                dmin * 0.5
            } else {
                dmax.max(d) * 1e-5
            };
            (near.max(1e-12), dmax.max(near) * 2.0 + 1e-9)
        }
    }

    /// Smallest ray parameter a picker should accept: 0 for perspective, −∞ for orthographic (the ray
    /// origin lies on the eye plane but the orthographic view also shows geometry behind it).
    pub fn pick_t_min(&self) -> f64 {
        if self.ortho { f64::NEG_INFINITY } else { 0.0 }
    }

    /// Normalized device coordinates (x right, y up, −1..1) of a screen pixel.
    fn screen_to_ndc(px: DVec2, viewport: DVec2) -> DVec2 {
        let vp = viewport.max(DVec2::ONE);
        DVec2::new(px.x / vp.x * 2.0 - 1.0, 1.0 - px.y / vp.y * 2.0)
    }

    /// Picking ray through the screen pixel `px` (top-left origin, Y down).
    pub fn ray_from_screen(&self, px: DVec2, viewport: DVec2) -> Ray3 {
        let ndc = Self::screen_to_ndc(px, viewport);
        let vp = viewport.max(DVec2::ONE);
        let aspect = vp.x / vp.y;
        let h = self.half_height_at_target();
        let (r, u, f) = (self.right(), self.up(), self.forward());
        if self.ortho {
            let origin = self.eye() + r * (ndc.x * h * aspect) + u * (ndc.y * h);
            Ray3 { origin, dir: f }
        } else {
            let tan = (self.fov() * 0.5).tan();
            let dir = (f + r * (ndc.x * tan * aspect) + u * (ndc.y * tan)).normalize_or(f);
            Ray3 {
                origin: self.eye(),
                dir,
            }
        }
    }

    /// World → screen pixels; `None` when the point is behind a perspective camera.
    pub fn world_to_screen(&self, p: DVec3, viewport: DVec2) -> Option<DVec2> {
        let vp = viewport.max(DVec2::ONE);
        let v = self.view_matrix().transform_point3(p);
        let h = self.half_height_at_target();
        let aspect = vp.x / vp.y;
        let ndc = if self.ortho {
            DVec2::new(v.x / (h * aspect), v.y / h)
        } else {
            let depth = -v.z;
            if depth <= 1e-12 * self.dist() {
                return None;
            }
            let tan = (self.fov() * 0.5).tan();
            DVec2::new(v.x / (depth * tan * aspect), v.y / (depth * tan))
        };
        Some(DVec2::new(
            (ndc.x + 1.0) * 0.5 * vp.x,
            (1.0 - ndc.y) * 0.5 * vp.y,
        ))
    }

    /// World units per pixel at the target distance.
    pub fn units_per_px_at_target(&self, viewport_height: f64) -> f64 {
        2.0 * self.half_height_at_target() / viewport_height.max(1.0)
    }

    /// Rotates around the target (radians).
    pub fn orbit(&mut self, d_yaw: f64, d_pitch: f64) {
        use std::f64::consts::{FRAC_PI_2, TAU};
        if d_yaw.is_finite() {
            self.yaw = (self.yaw_c() + d_yaw).rem_euclid(TAU);
        }
        if d_pitch.is_finite() {
            self.pitch = (self.pitch_c() + d_pitch).clamp(-FRAC_PI_2, FRAC_PI_2);
        }
    }

    /// Moves the target so the content follows a pointer drag of `delta_px` (Y down).
    pub fn pan_px(&mut self, delta_px: DVec2, viewport: DVec2) {
        let upp = self.units_per_px_at_target(viewport.y);
        self.target += (-self.right() * delta_px.x + self.up() * delta_px.y) * upp;
    }

    /// Divides the distance by `factor` (> 1 zooms in), keeping the point under `screen_px` on the
    /// target plane fixed on screen.
    pub fn zoom_about(&mut self, screen_px: DVec2, factor: f64, viewport: DVec2) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let plane = Plane::from_normal(self.target, self.view_dir());
        let ray = self.ray_from_screen(screen_px, viewport);
        let anchor = ray
            .intersect_plane(&plane)
            .map(|t| ray.at(t))
            .unwrap_or(self.target);
        let new_dist = (self.dist() / factor).clamp(1e-9, 1e15);
        let k = new_dist / self.dist();
        self.target = anchor + (self.target - anchor) * k;
        self.distance = new_dist;
    }

    /// Sets a standard view direction (keeps target and distance).
    pub fn set_view(&mut self, view: StandardView) {
        (self.yaw, self.pitch) = view.yaw_pitch();
    }

    /// Looks straight at `plane` (from the side its normal points to). The in-plane rotation follows
    /// the camera's Z-up convention, so it matches the plane axes only for XY/XZ/YZ-like planes.
    pub fn look_at_plane(&mut self, plane: &Plane) {
        let n = plane.normal().normalize_or(DVec3::Z);
        self.pitch = n.z.clamp(-1.0, 1.0).asin();
        if n.z.abs() < 1.0 - 1e-9 {
            self.yaw = n.x.atan2(-n.y);
        } else {
            // Looking along ±Z: `right()` is (cos yaw, sin yaw, 0), so align it with the plane's X axis.
            let x = plane.x_axis;
            self.yaw = x.y.atan2(x.x);
        }
        // Keep the target on the plane so orbiting afterwards pivots around it.
        self.target -= n * (self.target - plane.origin).dot(n);
    }

    /// Centers `bbox` and chooses the distance so its bounding sphere fits the view.
    pub fn fit_bbox(&mut self, bbox: &BBox3, aspect: f64) {
        if bbox.is_empty() || !bbox.min.is_finite() || !bbox.max.is_finite() {
            return;
        }
        self.target = bbox.center();
        let radius = (bbox.extent() * 0.5).max(1e-9);
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.0
        };
        let half_v = self.fov() * 0.5;
        let half_min = if aspect < 1.0 {
            (half_v.tan() * aspect).atan()
        } else {
            half_v
        };
        self.distance = if self.ortho {
            // Visible half-extent (in the narrow direction) = distance · tan(half_v) · min(aspect, 1).
            radius * 1.05 / (half_v.tan() * aspect.min(1.0))
        } else {
            radius * 1.05 / half_min.sin()
        };
    }
}
