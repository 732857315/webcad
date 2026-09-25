//! Conversions between wcad-math (glam f64) and monstertruck (cgmath) types.

use monstertruck_modeling::{Matrix4, Point3, Vector3, Vector4};
use wcad_math::{DAffine3, DVec3};

#[inline]
pub(crate) fn p3(v: DVec3) -> Point3 {
    Point3::new(v.x, v.y, v.z)
}

#[inline]
pub(crate) fn v3(v: DVec3) -> Vector3 {
    Vector3::new(v.x, v.y, v.z)
}

#[inline]
pub(crate) fn dp(p: Point3) -> DVec3 {
    DVec3::new(p.x, p.y, p.z)
}

/// Affine transform as a column-major cgmath matrix.
pub(crate) fn mat4(a: &DAffine3) -> Matrix4 {
    let m = a.matrix3;
    let t = a.translation;
    Matrix4::from_cols(
        Vector4::new(m.x_axis.x, m.x_axis.y, m.x_axis.z, 0.0),
        Vector4::new(m.y_axis.x, m.y_axis.y, m.y_axis.z, 0.0),
        Vector4::new(m.z_axis.x, m.z_axis.y, m.z_axis.z, 0.0),
        Vector4::new(t.x, t.y, t.z, 1.0),
    )
}

/// `true` if every component is finite.
#[inline]
pub(crate) fn finite3(v: DVec3) -> bool {
    v.is_finite()
}

/// `true` if the affine transform is finite and not (near-)singular.
pub(crate) fn affine_ok(a: &DAffine3) -> bool {
    let d = a.matrix3.determinant();
    a.matrix3.is_finite() && a.translation.is_finite() && d.abs() > 1e-12
}
