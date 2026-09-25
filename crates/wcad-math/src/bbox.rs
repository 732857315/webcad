use glam::{DVec2, DVec3};
use serde::{Deserialize, Serialize};

/// Axis-aligned 2D bounding box. `EMPTY` has `min > max` and absorbs the first point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox2 {
    pub min: DVec2,
    pub max: DVec2,
}

impl BBox2 {
    pub const EMPTY: Self = Self { min: DVec2::splat(f64::INFINITY), max: DVec2::splat(f64::NEG_INFINITY) };

    pub fn new(a: DVec2, b: DVec2) -> Self {
        Self { min: a.min(b), max: a.max(b) }
    }

    pub fn from_points<I: IntoIterator<Item = DVec2>>(pts: I) -> Self {
        let mut b = Self::EMPTY;
        for p in pts {
            b.include(p);
        }
        b
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    pub fn include(&mut self, p: DVec2) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Self) -> Self {
        Self { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    pub fn expanded(&self, d: f64) -> Self {
        Self { min: self.min - DVec2::splat(d), max: self.max + DVec2::splat(d) }
    }

    pub fn size(&self) -> DVec2 {
        if self.is_empty() { DVec2::ZERO } else { self.max - self.min }
    }

    pub fn center(&self) -> DVec2 {
        (self.min + self.max) * 0.5
    }

    pub fn contains(&self, p: DVec2) -> bool {
        p.x >= self.min.x && p.x <= self.max.x && p.y >= self.min.y && p.y <= self.max.y
    }

    /// `true` if `o` lies completely inside `self`.
    pub fn contains_box(&self, o: &Self) -> bool {
        !o.is_empty() && self.contains(o.min) && self.contains(o.max)
    }

    pub fn intersects(&self, o: &Self) -> bool {
        !(o.min.x > self.max.x || o.max.x < self.min.x || o.min.y > self.max.y || o.max.y < self.min.y)
    }
}

impl Default for BBox2 {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Axis-aligned 3D bounding box. `EMPTY` has `min > max` and absorbs the first point.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox3 {
    pub min: DVec3,
    pub max: DVec3,
}

impl BBox3 {
    pub const EMPTY: Self = Self { min: DVec3::splat(f64::INFINITY), max: DVec3::splat(f64::NEG_INFINITY) };

    pub fn from_points<I: IntoIterator<Item = DVec3>>(pts: I) -> Self {
        let mut b = Self::EMPTY;
        for p in pts {
            b.include(p);
        }
        b
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn include(&mut self, p: DVec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Self) -> Self {
        Self { min: self.min.min(o.min), max: self.max.max(o.max) }
    }

    pub fn size(&self) -> DVec3 {
        if self.is_empty() { DVec3::ZERO } else { self.max - self.min }
    }

    pub fn center(&self) -> DVec3 {
        (self.min + self.max) * 0.5
    }

    /// Length of the diagonal; 0 for an empty box.
    pub fn extent(&self) -> f64 {
        self.size().length()
    }
}

impl Default for BBox3 {
    fn default() -> Self {
        Self::EMPTY
    }
}
