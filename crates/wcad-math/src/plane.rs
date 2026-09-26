use glam::{DAffine3, DMat3, DVec2, DVec3};
use serde::{Deserialize, Serialize};

/// An oriented plane with an in-plane coordinate frame. `x_axis` and `y_axis` are unit length and
/// orthogonal; the normal is `x_axis × y_axis`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plane {
    pub origin: DVec3,
    pub x_axis: DVec3,
    pub y_axis: DVec3,
}

impl Plane {
    /// World XY plane (Top view), normal +Z.
    pub const XY: Self = Self {
        origin: DVec3::ZERO,
        x_axis: DVec3::X,
        y_axis: DVec3::Y,
    };
    /// World XZ plane (Front view), normal -Y (right-handed: X × Z = -Y).
    pub const XZ: Self = Self {
        origin: DVec3::ZERO,
        x_axis: DVec3::X,
        y_axis: DVec3::Z,
    };
    /// World YZ plane (Right view), normal +X.
    pub const YZ: Self = Self {
        origin: DVec3::ZERO,
        x_axis: DVec3::Y,
        y_axis: DVec3::Z,
    };

    /// Build a plane from an origin and normal, choosing a stable in-plane X axis.
    pub fn from_normal(origin: DVec3, normal: DVec3) -> Self {
        let n = normal.normalize_or(DVec3::Z);
        let helper = if n.z.abs() < 0.9 { DVec3::Z } else { DVec3::X };
        let x_axis = helper.cross(n).normalize();
        let y_axis = n.cross(x_axis);
        Self {
            origin,
            x_axis,
            y_axis,
        }
    }

    /// Build a plane from origin, x direction and a vector roughly along +y (re-orthogonalized).
    pub fn from_axes(origin: DVec3, x_dir: DVec3, y_hint: DVec3) -> Option<Self> {
        let x_axis = x_dir.try_normalize()?;
        let n = x_axis.cross(y_hint).try_normalize()?;
        let y_axis = n.cross(x_axis);
        Some(Self {
            origin,
            x_axis,
            y_axis,
        })
    }

    pub fn normal(&self) -> DVec3 {
        self.x_axis.cross(self.y_axis)
    }

    /// Project a world point to plane coordinates (drops the normal component).
    pub fn to_local(&self, p: DVec3) -> DVec2 {
        let d = p - self.origin;
        DVec2::new(d.dot(self.x_axis), d.dot(self.y_axis))
    }

    pub fn to_world(&self, p: DVec2) -> DVec3 {
        self.origin + self.x_axis * p.x + self.y_axis * p.y
    }

    /// Signed distance of `p` from the plane along the normal.
    pub fn signed_distance(&self, p: DVec3) -> f64 {
        (p - self.origin).dot(self.normal())
    }

    pub fn offset(&self, d: f64) -> Self {
        Self {
            origin: self.origin + self.normal() * d,
            ..*self
        }
    }

    /// Plane-local → world transform (local z = normal).
    pub fn to_world_affine(&self) -> DAffine3 {
        DAffine3::from_mat3_translation(
            DMat3::from_cols(self.x_axis, self.y_axis, self.normal()),
            self.origin,
        )
    }

    /// Intersect a ray with the plane; returns the ray parameter.
    pub fn ray_hit(&self, ray_origin: DVec3, ray_dir: DVec3) -> Option<f64> {
        let n = self.normal();
        let denom = ray_dir.dot(n);
        if denom.abs() < 1e-15 {
            return None;
        }
        Some((self.origin - ray_origin).dot(n) / denom)
    }
}

impl Default for Plane {
    fn default() -> Self {
        Self::XY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = Plane::from_normal(DVec3::new(1.0, 2.0, 3.0), DVec3::new(1.0, 1.0, 0.0));
        let local = DVec2::new(3.0, -4.0);
        let w = p.to_world(local);
        assert!((p.to_local(w) - local).length() < 1e-12);
        assert!(p.signed_distance(w).abs() < 1e-12);
        assert!((p.normal().length() - 1.0).abs() < 1e-12);
        assert!((Plane::XZ.normal() - DVec3::NEG_Y).length() < 1e-15);
    }
}
