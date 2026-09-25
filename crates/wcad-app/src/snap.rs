//! Spatial index over entity bounds (rstar) and the object snap engine.

use std::collections::HashMap;

use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{AABB, RTree};
use wcad_doc::{Drawing, EntityId, EntityKind};
use wcad_geom2d::snap::{self as gsnap, SnapKind};
use wcad_geom2d::{Curve, Curve2, intersect};
use wcad_math::{BBox2, DVec2};

use crate::i18n::{CoreStrings, Lang, core};
use crate::select::entity_bbox;
use crate::settings::{DraftSettings, OsnapModes};

type Item = GeomWithData<Rectangle<[f64; 2]>, EntityId>;

/// R-tree of model-space entity bounding boxes.
#[derive(Default)]
pub struct SpatialIndex {
    tree: RTree<Item>,
    boxes: HashMap<EntityId, BBox2>,
}

fn item(id: EntityId, b: &BBox2) -> Item {
    GeomWithData::new(
        Rectangle::from_corners([b.min.x, b.min.y], [b.max.x, b.max.y]),
        id,
    )
}

impl SpatialIndex {
    pub fn len(&self) -> usize {
        self.boxes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    /// Rebuild from scratch (bulk load).
    pub fn rebuild(&mut self, d: &Drawing) {
        self.boxes.clear();
        let mut items = Vec::with_capacity(d.entities.len());
        for (id, e) in &d.entities {
            let b = entity_bbox(e, d);
            if !b.is_empty() {
                items.push(item(*id, &b));
                self.boxes.insert(*id, b);
            }
        }
        self.tree = RTree::bulk_load(items);
    }

    /// Re-index the given entities (removed ones are dropped).
    pub fn update(&mut self, d: &Drawing, ids: impl IntoIterator<Item = EntityId>) {
        for id in ids {
            if let Some(old) = self.boxes.remove(&id) {
                self.tree.remove(&item(id, &old));
            }
            if let Some(e) = d.entities.get(&id) {
                let b = entity_bbox(e, d);
                if !b.is_empty() {
                    self.tree.insert(item(id, &b));
                    self.boxes.insert(id, b);
                }
            }
        }
    }

    /// Ids whose bounds intersect `b`, in ascending id order.
    pub fn query(&self, b: &BBox2) -> Vec<EntityId> {
        if b.is_empty() {
            return Vec::new();
        }
        let env = AABB::from_corners([b.min.x, b.min.y], [b.max.x, b.max.y]);
        let mut v: Vec<EntityId> = self
            .tree
            .locate_in_envelope_intersecting(env)
            .map(|i| i.data)
            .collect();
        v.sort_unstable();
        v
    }

    pub fn bbox(&self, id: EntityId) -> Option<BBox2> {
        self.boxes.get(&id).copied()
    }

    /// Bounds of everything indexed.
    pub fn extents(&self) -> BBox2 {
        self.boxes.values().fold(BBox2::EMPTY, |a, b| a.union(b))
    }
}

/// Kind of a snap result (object snaps plus grid and polar tracking).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OsnapKind {
    Endpoint,
    Midpoint,
    Center,
    Quadrant,
    Intersection,
    Perpendicular,
    Tangent,
    Nearest,
    Node,
    Grid,
    Polar,
}

impl OsnapKind {
    pub fn label(self, lang: Lang) -> &'static str {
        let s: &CoreStrings = core(lang);
        match self {
            OsnapKind::Endpoint => s.snap_endpoint,
            OsnapKind::Midpoint => s.snap_midpoint,
            OsnapKind::Center => s.snap_center,
            OsnapKind::Quadrant => s.snap_quadrant,
            OsnapKind::Intersection => s.snap_intersection,
            OsnapKind::Perpendicular => s.snap_perpendicular,
            OsnapKind::Tangent => s.snap_tangent,
            OsnapKind::Nearest => s.snap_nearest,
            OsnapKind::Node => s.snap_node,
            OsnapKind::Grid => s.snap_grid,
            OsnapKind::Polar => s.snap_polar,
        }
    }

    /// Lower = preferred when several candidates are inside the aperture.
    fn priority(self) -> u8 {
        match self {
            OsnapKind::Endpoint | OsnapKind::Intersection | OsnapKind::Node => 0,
            OsnapKind::Center | OsnapKind::Midpoint | OsnapKind::Quadrant => 1,
            OsnapKind::Perpendicular | OsnapKind::Tangent => 2,
            OsnapKind::Nearest => 3,
            OsnapKind::Grid | OsnapKind::Polar => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapHit {
    pub p: DVec2,
    pub kind: OsnapKind,
    pub entity: Option<EntityId>,
}

/// Maximum curves considered for pairwise intersections near the cursor.
const MAX_INTERSECT_CURVES: usize = 24;

/// Snap geometry of an entity: curves plus point-like snap nodes.
fn snap_geometry(kind: &EntityKind, d: &Drawing) -> (Vec<Curve2>, Vec<DVec2>) {
    match kind {
        EntityKind::Point { p } => (vec![], vec![*p]),
        EntityKind::Text(t) => (vec![], vec![t.pos]),
        EntityKind::MText(t) => (vec![], vec![t.pos]),
        EntityKind::Insert(i) => (vec![], vec![i.pos]),
        EntityKind::Dimension(dim) => {
            let g =
                crate::dimgen::dimension_geometry(dim, &crate::dimgen::style_of(dim, d), &d.tables);
            (g.curves(), g.def_points)
        }
        EntityKind::Hatch(_) => (vec![], vec![]),
        other => (other.as_curve().into_iter().collect(), vec![]),
    }
}

/// Find the best object snap for cursor `p` within `aperture` world units.
/// `base` enables perpendicular/tangent snaps from the tool's base point.
pub fn find_snap(
    d: &Drawing,
    index: &SpatialIndex,
    p: DVec2,
    aperture: f64,
    modes: &OsnapModes,
    base: Option<DVec2>,
) -> Option<SnapHit> {
    if !(aperture > 0.0) || !p.is_finite() {
        return None;
    }
    let q = BBox2::new(p, p).expanded(aperture);
    let mut cands: Vec<SnapHit> = Vec::new();
    let mut near_curves: Vec<(EntityId, Curve2)> = Vec::new();
    let push = |cands: &mut Vec<SnapHit>, pt: DVec2, kind: OsnapKind, id: EntityId| {
        if pt.is_finite() && pt.distance(p) <= aperture {
            cands.push(SnapHit {
                p: pt,
                kind,
                entity: Some(id),
            });
        }
    };
    for id in index.query(&q) {
        let Some(e) = d.entities.get(&id) else {
            continue;
        };
        if !d.layer(e.layer).is_some_and(|l| l.visible && !l.frozen) {
            continue;
        }
        let (curves, nodes) = snap_geometry(&e.kind, d);
        if modes.node {
            for n in nodes {
                push(&mut cands, n, OsnapKind::Node, id);
            }
        }
        for c in curves {
            let near = c.closest(p).1;
            let on_curve = near.distance(p) <= aperture;
            if modes.endpoint {
                for pt in gsnap::snap_points(&c, SnapKind::Endpoint) {
                    push(&mut cands, pt, OsnapKind::Endpoint, id);
                }
            }
            if modes.midpoint {
                for pt in gsnap::snap_points(&c, SnapKind::Midpoint) {
                    push(&mut cands, pt, OsnapKind::Midpoint, id);
                }
            }
            if modes.center {
                for pt in gsnap::snap_points(&c, SnapKind::Center) {
                    if pt.distance(p) <= aperture {
                        push(&mut cands, pt, OsnapKind::Center, id);
                    } else if on_curve {
                        // Hovering the circle itself offers its center (AutoCAD behaviour).
                        cands.push(SnapHit {
                            p: pt,
                            kind: OsnapKind::Center,
                            entity: Some(id),
                        });
                    }
                }
            }
            if modes.quadrant {
                for pt in gsnap::snap_points(&c, SnapKind::Quadrant) {
                    push(&mut cands, pt, OsnapKind::Quadrant, id);
                }
            }
            if modes.node {
                for pt in gsnap::snap_points(&c, SnapKind::Node) {
                    push(&mut cands, pt, OsnapKind::Node, id);
                }
            }
            if let Some(b) = base {
                if modes.perpendicular && on_curve {
                    for pt in gsnap::perpendicular_feet(&c, b) {
                        push(&mut cands, pt, OsnapKind::Perpendicular, id);
                    }
                }
                if modes.tangent && on_curve {
                    for pt in gsnap::tangent_points(&c, b) {
                        push(&mut cands, pt, OsnapKind::Tangent, id);
                    }
                }
            }
            if on_curve {
                if modes.nearest {
                    push(&mut cands, near, OsnapKind::Nearest, id);
                }
                if near_curves.len() < MAX_INTERSECT_CURVES {
                    near_curves.push((id, c));
                }
            }
        }
    }
    if modes.intersection {
        for i in 0..near_curves.len() {
            for j in i + 1..near_curves.len() {
                for x in intersect(&near_curves[i].1, &near_curves[j].1) {
                    push(&mut cands, x.p, OsnapKind::Intersection, near_curves[i].0);
                }
            }
        }
    }
    // Candidates offered because the cursor is on a circle (center far away) rank by curve distance.
    cands.into_iter().min_by(|a, b| {
        let da = a.p.distance(p).min(aperture);
        let db = b.p.distance(p).min(aperture);
        (a.kind.priority(), da)
            .partial_cmp(&(b.kind.priority(), db))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Round to the snap grid.
pub fn grid_snap(p: DVec2, spacing: f64) -> DVec2 {
    if !(spacing > 0.0) || !spacing.is_finite() {
        return p;
    }
    (p / spacing).round() * spacing
}

/// Result of constraining the cursor relative to a base point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Constrained {
    pub p: DVec2,
    /// Polar/ortho tracking ray angle (radians) if a constraint applied.
    pub angle: Option<f64>,
}

/// Apply angle lock, ortho or polar tracking to cursor `p` relative to `base`.
/// `tol_world` is the polar capture distance in world units.
pub fn constrain(
    p: DVec2,
    base: DVec2,
    draft: &DraftSettings,
    angle_lock: Option<f64>,
    tol_world: f64,
) -> Constrained {
    let v = p - base;
    if v.length_squared() <= 0.0 || !v.is_finite() {
        return Constrained { p, angle: None };
    }
    let project = |a: f64| {
        let dir = DVec2::from_angle(a);
        base + dir * v.dot(dir).max(0.0)
    };
    if let Some(a) = angle_lock {
        return Constrained {
            p: project(a),
            angle: Some(a),
        };
    }
    if draft.ortho {
        let a = if v.x.abs() >= v.y.abs() {
            if v.x >= 0.0 {
                0.0
            } else {
                std::f64::consts::PI
            }
        } else if v.y >= 0.0 {
            std::f64::consts::FRAC_PI_2
        } else {
            -std::f64::consts::FRAC_PI_2
        };
        return Constrained {
            p: project(a),
            angle: Some(a),
        };
    }
    if draft.polar {
        let inc = draft.polar_increment_deg.to_radians();
        if inc > 1e-6 && inc.is_finite() {
            let a = v.y.atan2(v.x);
            let k = (a / inc).round();
            let snapped = k * inc;
            let q = project(snapped);
            if q.distance(p) <= tol_world {
                return Constrained {
                    p: q,
                    angle: Some(snapped),
                };
            }
        }
    }
    Constrained { p, angle: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::Document;
    use wcad_geom2d::{Circle2, Line2};

    fn setup() -> (Document, SpatialIndex, Vec<EntityId>) {
        let mut doc = Document::new();
        let ids = doc.transact("t", |tx| {
            vec![
                tx.add(EntityKind::Line(Line2::new(
                    DVec2::ZERO,
                    DVec2::new(10.0, 0.0),
                ))),
                tx.add(EntityKind::Line(Line2::new(
                    DVec2::new(5.0, -5.0),
                    DVec2::new(5.0, 5.0),
                ))),
                tx.add(EntityKind::Circle(Circle2::new(DVec2::new(20.0, 0.0), 3.0))),
            ]
        });
        let mut idx = SpatialIndex::default();
        idx.rebuild(&doc.drawing);
        (doc, idx, ids)
    }

    #[test]
    fn snaps() {
        let (doc, idx, ids) = setup();
        let d = &doc.drawing;
        let m = OsnapModes::default();
        let hit = find_snap(d, &idx, DVec2::new(9.8, 0.1), 0.5, &m, None).unwrap();
        assert_eq!(
            (hit.kind, hit.p, hit.entity),
            (OsnapKind::Endpoint, DVec2::new(10.0, 0.0), Some(ids[0]))
        );
        let hit = find_snap(d, &idx, DVec2::new(5.1, 0.2), 0.5, &m, None).unwrap();
        assert_eq!(hit.kind, OsnapKind::Intersection);
        assert!(hit.p.distance(DVec2::new(5.0, 0.0)) < 1e-9);
        // On the circle: its center is offered.
        let hit = find_snap(d, &idx, DVec2::new(20.0, 3.1), 0.5, &m, None).unwrap();
        assert_eq!(
            (hit.kind, hit.p),
            (OsnapKind::Center, DVec2::new(20.0, 0.0))
        );
        // Perpendicular from a base point.
        let hit = find_snap(
            d,
            &idx,
            DVec2::new(2.0, 0.1),
            0.5,
            &m,
            Some(DVec2::new(2.0, 7.0)),
        )
        .unwrap();
        assert_eq!(hit.kind, OsnapKind::Perpendicular);
        assert!(hit.p.distance(DVec2::new(2.0, 0.0)) < 1e-9);
        assert!(find_snap(d, &idx, DVec2::new(50.0, 50.0), 0.5, &m, None).is_none());
    }

    #[test]
    fn index_updates() {
        let (mut doc, mut idx, ids) = setup();
        assert_eq!(idx.len(), 3);
        doc.transact("rm", |tx| tx.remove(ids[2]));
        idx.update(&doc.drawing, [ids[2]]);
        assert_eq!(idx.len(), 2);
        assert!(
            idx.query(&BBox2::new(DVec2::new(19.0, -1.0), DVec2::new(21.0, 1.0)))
                .is_empty()
        );
    }

    #[test]
    fn ortho_polar_grid() {
        let mut ds = DraftSettings {
            polar: false,
            ortho: true,
            ..Default::default()
        };
        let c = constrain(DVec2::new(10.0, 1.0), DVec2::ZERO, &ds, None, 0.5);
        assert_eq!(c.p, DVec2::new(10.0, 0.0));
        ds.ortho = false;
        ds.polar = true;
        ds.polar_increment_deg = 45.0;
        let c = constrain(DVec2::new(10.0, 10.2), DVec2::ZERO, &ds, None, 0.5);
        assert!(c.p.distance(DVec2::new(10.1, 10.1)) < 1e-9);
        let c = constrain(DVec2::new(10.0, 5.0), DVec2::ZERO, &ds, None, 0.5);
        assert_eq!(c.angle, None);
        assert_eq!(
            grid_snap(DVec2::new(1.26, -0.74), 0.5),
            DVec2::new(1.5, -0.5)
        );
        let c = constrain(
            DVec2::new(3.0, 1.0),
            DVec2::ZERO,
            &ds,
            Some(std::f64::consts::FRAC_PI_2),
            0.5,
        );
        assert!(c.p.distance(DVec2::new(0.0, 1.0)) < 1e-12);
    }
}
