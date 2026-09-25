//! Selection set, picking (point, window, crossing), entity bounds and grips.

use std::collections::BTreeSet;

use wcad_doc::{Drawing, Entity, EntityId, EntityKind};
use wcad_geom2d::{Arc2, Curve, Curve2};
use wcad_math::{BBox2, DAffine2, DVec2};

use crate::snap::SpatialIndex;

/// The current selection (ordered by id = draw order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    ids: BTreeSet<EntityId>,
    revision: u64,
}

impl Selection {
    pub fn len(&self) -> usize {
        self.ids.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub fn contains(&self, id: EntityId) -> bool {
        self.ids.contains(&id)
    }
    pub fn iter(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.ids.iter().copied()
    }
    pub fn to_vec(&self) -> Vec<EntityId> {
        self.ids.iter().copied().collect()
    }
    /// Bumped on every change (display code rebuilds the highlight when it changes).
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn add(&mut self, id: EntityId) {
        if self.ids.insert(id) {
            self.revision += 1;
        }
    }
    pub fn remove(&mut self, id: EntityId) {
        if self.ids.remove(&id) {
            self.revision += 1;
        }
    }
    pub fn extend(&mut self, ids: impl IntoIterator<Item = EntityId>) {
        for id in ids {
            self.add(id);
        }
    }
    pub fn clear(&mut self) {
        if !self.ids.is_empty() {
            self.ids.clear();
            self.revision += 1;
        }
    }
    pub fn set(&mut self, ids: impl IntoIterator<Item = EntityId>) {
        let new: BTreeSet<EntityId> = ids.into_iter().collect();
        if new != self.ids {
            self.ids = new;
            self.revision += 1;
        }
    }
    /// Drop ids for which `keep` is false (e.g. deleted entities).
    pub fn retain(&mut self, keep: impl Fn(EntityId) -> bool) {
        let before = self.ids.len();
        self.ids.retain(|id| keep(*id));
        if self.ids.len() != before {
            self.revision += 1;
        }
    }
}

const MAX_BLOCK_DEPTH: usize = 16;

/// Approximate bounds of an entity in world coordinates (text uses its layout frame estimate).
pub fn entity_bbox(e: &Entity, d: &Drawing) -> BBox2 {
    kind_bbox(&e.kind, d, 0)
}

fn kind_bbox(kind: &EntityKind, d: &Drawing, depth: usize) -> BBox2 {
    let bb = match kind {
        EntityKind::Point { p } => BBox2::new(*p, *p),
        EntityKind::Text(t) => {
            let chars = t.text.chars().count().max(1) as f64;
            let w = t.height * t.width_factor.abs().max(0.1) * chars * 0.9;
            let (ax, ay) = match t.halign {
                wcad_doc::HAlign::Left => (0.0, 1.0),
                wcad_doc::HAlign::Center => (-0.5, 0.5),
                wcad_doc::HAlign::Right => (-1.0, 0.0),
            };
            let _ = ay;
            let (y0, y1) = match t.valign {
                wcad_doc::VAlign::Baseline | wcad_doc::VAlign::Bottom => (-0.3, 1.0),
                wcad_doc::VAlign::Middle => (-0.5, 0.5),
                wcad_doc::VAlign::Top => (-1.0, 0.0),
            };
            rotated_box(
                t.pos,
                t.rotation,
                DVec2::new(ax * w, y0 * t.height),
                DVec2::new(ax * w + w, y1 * t.height),
            )
        }
        EntityKind::MText(m) => {
            let plain = wcad_geom2d::text::mtext_to_plain(&m.text);
            let lines = plain.lines().count().max(1) as f64;
            let longest = plain
                .lines()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(1)
                .max(1) as f64;
            let w = if m.width > 0.0 {
                m.width
            } else {
                m.height * longest * 0.9
            };
            let h = m.height * (1.0 + (lines - 1.0) * 5.0 / 3.0 * m.line_spacing.max(0.1));
            let col = ((m.attachment.clamp(1, 9) - 1) % 3) as f64;
            let row = ((m.attachment.clamp(1, 9) - 1) / 3) as f64;
            let x0 = -col * 0.5 * w;
            let y1 = row * 0.5 * h;
            rotated_box(
                m.pos,
                m.rotation,
                DVec2::new(x0, y1 - h),
                DVec2::new(x0 + w, y1),
            )
        }
        EntityKind::Dimension(dim) => {
            let g =
                crate::dimgen::dimension_geometry(dim, &crate::dimgen::style_of(dim, d), &d.tables);
            g.bbox()
        }
        EntityKind::Hatch(h) => h
            .loops
            .iter()
            .flat_map(|l| l.curves.iter())
            .fold(BBox2::EMPTY, |b, c| b.union(&c.bbox())),
        EntityKind::Insert(ins) => {
            if depth >= MAX_BLOCK_DEPTH {
                return BBox2::EMPTY;
            }
            let Some(block) = d.blocks.get(&ins.block) else {
                return BBox2::new(ins.pos, ins.pos);
            };
            let m = insert_transform(ins, block.base);
            let mut bb = BBox2::new(ins.pos, ins.pos);
            for be in block.entities.values() {
                let b = kind_bbox(&be.kind, d, depth + 1);
                if !b.is_empty() {
                    for c in corners(&b) {
                        bb.include(m.transform_point2(c));
                    }
                }
            }
            bb
        }
        other => other.as_curve().map(|c| c.bbox()).unwrap_or(BBox2::EMPTY),
    };
    if bb.min.is_finite() && bb.max.is_finite() {
        bb
    } else {
        BBox2::EMPTY
    }
}

/// Block insert transform (block-local → world).
pub fn insert_transform(ins: &wcad_doc::Insert, base: DVec2) -> DAffine2 {
    DAffine2::from_scale_angle_translation(ins.scale, ins.rotation, ins.pos)
        * DAffine2::from_translation(-base)
}

fn corners(b: &BBox2) -> [DVec2; 4] {
    [
        b.min,
        DVec2::new(b.max.x, b.min.y),
        b.max,
        DVec2::new(b.min.x, b.max.y),
    ]
}

fn rotated_box(origin: DVec2, rot: f64, lo: DVec2, hi: DVec2) -> BBox2 {
    let m = DAffine2::from_angle_translation(rot, origin);
    BBox2::from_points(corners(&BBox2::new(lo, hi)).map(|c| m.transform_point2(c)))
}

/// Distance from `p` to the entity's visible geometry (∞ when not measurable).
pub fn entity_distance(e: &Entity, d: &Drawing, p: DVec2) -> f64 {
    match &e.kind {
        EntityKind::Point { p: q } => q.distance(p),
        EntityKind::Text(_) | EntityKind::MText(_) | EntityKind::Insert(_) => {
            let bb = entity_bbox(e, d);
            if bb.is_empty() {
                f64::INFINITY
            } else if bb.contains(p) {
                0.0
            } else {
                (p.clamp(bb.min, bb.max) - p).length()
            }
        }
        EntityKind::Dimension(dim) => {
            let g =
                crate::dimgen::dimension_geometry(dim, &crate::dimgen::style_of(dim, d), &d.tables);
            g.distance(p)
        }
        EntityKind::Hatch(h) => {
            let boundary = h
                .loops
                .iter()
                .flat_map(|l| l.curves.iter())
                .map(|c| c.closest(p).1.distance(p))
                .fold(f64::INFINITY, f64::min);
            let loops: Vec<&[Curve2]> = h.loops.iter().map(|l| l.curves.as_slice()).collect();
            let inside = loops
                .iter()
                .filter(|l| wcad_geom2d::point_in_loop(l, p))
                .count()
                % 2
                == 1;
            if inside { 0.0 } else { boundary }
        }
        other => other
            .as_curve()
            .map(|c| c.closest(p).1.distance(p))
            .unwrap_or(f64::INFINITY),
    }
}

fn selectable(d: &Drawing, e: &Entity) -> bool {
    d.layer(e.layer).is_some_and(|l| l.visible && !l.frozen)
}

/// Topmost (highest id) selectable entity within `tol` of `p`. Entities on locked layers can be
/// picked (for inspection) but tools filter them with [`crate::tools::ToolCx::editable`].
pub fn pick(d: &Drawing, index: &SpatialIndex, p: DVec2, tol: f64) -> Option<EntityId> {
    let q = BBox2::new(p, p).expanded(tol);
    let mut best: Option<(f64, EntityId)> = None;
    for id in index.query(&q) {
        let Some(e) = d.entities.get(&id) else {
            continue;
        };
        if !selectable(d, e) {
            continue;
        }
        let dist = entity_distance(e, d, p);
        if dist <= tol {
            // Prefer closer; among equals the later (topmost) entity.
            let better = match best {
                None => true,
                Some((bd, bid)) => {
                    dist < bd - tol * 0.05 || ((dist - bd).abs() <= tol * 0.05 && id > bid)
                }
            };
            if better {
                best = Some((dist, id));
            }
        }
    }
    best.map(|(_, id)| id)
}

/// Window selection (`crossing = false`: entirely inside) or crossing selection (inside or
/// touching the rectangle) between corners `a` and `b`.
pub fn window_select(
    d: &Drawing,
    index: &SpatialIndex,
    a: DVec2,
    b: DVec2,
    crossing: bool,
    tol: f64,
) -> Vec<EntityId> {
    let rect = BBox2::new(a, b);
    let mut out = Vec::new();
    for id in index.query(&rect) {
        let Some(e) = d.entities.get(&id) else {
            continue;
        };
        if !selectable(d, e) {
            continue;
        }
        let bb = index.bbox(id).unwrap_or_else(|| entity_bbox(e, d));
        if rect.contains_box(&bb) || (crossing && crosses(e, d, &rect, tol)) {
            out.push(id);
        }
    }
    out.sort_unstable();
    out
}

fn crosses(e: &Entity, d: &Drawing, rect: &BBox2, tol: f64) -> bool {
    match e.kind.as_curve() {
        Some(c) => {
            let pts = c.flatten(tol.max(1e-9));
            pts.windows(2).any(|w| segment_hits_rect(w[0], w[1], rect))
                || pts.iter().any(|p| rect.contains(*p))
        }
        None => match &e.kind {
            EntityKind::Hatch(h) => h.loops.iter().flat_map(|l| l.curves.iter()).any(|c| {
                c.flatten(tol.max(1e-9))
                    .windows(2)
                    .any(|w| segment_hits_rect(w[0], w[1], rect))
            }),
            _ => entity_bbox(e, d).intersects(rect),
        },
    }
}

/// Liang–Barsky style segment/rectangle overlap test.
pub fn segment_hits_rect(a: DVec2, b: DVec2, r: &BBox2) -> bool {
    if r.contains(a) || r.contains(b) {
        return true;
    }
    let d = b - a;
    let mut t0: f64 = 0.0;
    let mut t1: f64 = 1.0;
    for (p, q) in [
        (-d.x, a.x - r.min.x),
        (d.x, r.max.x - a.x),
        (-d.y, a.y - r.min.y),
        (d.y, r.max.y - a.y),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return false;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// Grip points of an entity (endpoints, midpoints, centers, quadrants, vertices, insertion points).
pub fn grips(kind: &EntityKind) -> Vec<DVec2> {
    match kind {
        EntityKind::Point { p } => vec![*p],
        EntityKind::Line(l) => vec![l.a, l.midpoint(), l.b],
        EntityKind::Circle(c) => vec![
            c.c,
            c.c + DVec2::new(c.r, 0.0),
            c.c + DVec2::new(0.0, c.r),
            c.c + DVec2::new(-c.r, 0.0),
            c.c + DVec2::new(0.0, -c.r),
        ],
        EntityKind::Arc(a) => vec![a.c, a.start_point(), a.mid_point(), a.end_point()],
        EntityKind::Ellipse(e) => vec![
            e.c,
            e.c + e.major,
            e.c + e.minor(),
            e.c - e.major,
            e.c - e.minor(),
        ],
        EntityKind::Polyline(p) => p.verts.iter().map(|v| v.p).collect(),
        EntityKind::Spline(s) => s.ctrl.clone(),
        EntityKind::Text(t) => vec![t.pos],
        EntityKind::MText(m) => vec![m.pos],
        EntityKind::Insert(i) => vec![i.pos],
        EntityKind::Dimension(_) | EntityKind::Hatch(_) => Vec::new(),
    }
}

/// Entity with grip `index` moved to `to`. Moving a center/midpoint/insertion grip translates the
/// entity; endpoint and vertex grips stretch it. `None` when the result would be degenerate.
pub fn move_grip(kind: &EntityKind, index: usize, to: DVec2) -> Option<EntityKind> {
    if !to.is_finite() {
        return None;
    }
    let g = grips(kind);
    let from = *g.get(index)?;
    let delta = to - from;
    let translated = || crate::xform::transform_kind(kind, &DAffine2::from_translation(delta));
    Some(match kind {
        EntityKind::Line(l) => {
            let mut l = *l;
            match index {
                0 => l.a = to,
                2 => l.b = to,
                _ => return Some(translated()),
            }
            if l.a.distance(l.b) <= 0.0 {
                return None;
            }
            EntityKind::Line(l)
        }
        EntityKind::Circle(c) => {
            if index == 0 {
                return Some(translated());
            }
            let r = c.c.distance(to);
            if !(r > 0.0) {
                return None;
            }
            EntityKind::Circle(wcad_geom2d::Circle2 { c: c.c, r })
        }
        EntityKind::Arc(a) => {
            if index == 0 {
                return Some(translated());
            }
            let (mut s, mut m, mut e) = (a.start_point(), a.mid_point(), a.end_point());
            match index {
                1 => s = to,
                2 => m = to,
                _ => e = to,
            }
            arc_through(s, m, e)?
        }
        EntityKind::Ellipse(el) => {
            let mut el = *el;
            match index {
                0 => return Some(translated()),
                1 | 3 => {
                    let v = to - el.c;
                    let minor_len = el.major.length() * el.ratio;
                    if v.length() <= 0.0 || minor_len <= 0.0 {
                        return None;
                    }
                    el.major = if index == 1 { v } else { -v };
                    el.ratio = minor_len / v.length();
                }
                _ => {
                    let len = (to - el.c).length();
                    let maj = el.major.length();
                    if !(len > 0.0) || !(maj > 0.0) {
                        return None;
                    }
                    el.ratio = len / maj;
                }
            }
            EntityKind::Ellipse(el)
        }
        EntityKind::Polyline(p) => {
            let mut p = p.clone();
            p.verts.get_mut(index)?.p = to;
            EntityKind::Polyline(p)
        }
        EntityKind::Spline(s) => {
            let mut s = s.clone();
            *s.ctrl.get_mut(index)? = to;
            EntityKind::Spline(s)
        }
        _ => translated(),
    })
}

/// CCW arc from `s` through `m` to `e` (reversed internally when the three points turn clockwise).
pub fn arc_through(s: DVec2, m: DVec2, e: DVec2) -> Option<EntityKind> {
    let c = wcad_sketch::circumcenter(s, m, e)?;
    let r = c.distance(s);
    if !(r > 0.0) || !r.is_finite() {
        return None;
    }
    let ang = |p: DVec2| (p - c).y.atan2((p - c).x);
    let cross = (m - s).perp_dot(e - m);
    let (a0, a1) = if cross > 0.0 {
        (ang(s), ang(e))
    } else {
        (ang(e), ang(s))
    };
    Some(EntityKind::Arc(Arc2 {
        c,
        r,
        start: a0,
        end: a1,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::Document;
    use wcad_geom2d::{Circle2, Line2};

    fn doc_with(kinds: Vec<EntityKind>) -> (Document, SpatialIndex, Vec<EntityId>) {
        let mut doc = Document::new();
        let ids = doc.transact("t", |tx| {
            kinds.into_iter().map(|k| tx.add(k)).collect::<Vec<_>>()
        });
        let mut idx = SpatialIndex::default();
        idx.rebuild(&doc.drawing);
        (doc, idx, ids)
    }

    #[test]
    fn pick_window_crossing() {
        let (doc, idx, ids) = doc_with(vec![
            EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0))),
            EntityKind::Circle(Circle2::new(DVec2::new(20.0, 0.0), 2.0)),
        ]);
        let d = &doc.drawing;
        assert_eq!(pick(d, &idx, DVec2::new(5.0, 0.1), 0.2), Some(ids[0]));
        assert_eq!(pick(d, &idx, DVec2::new(22.0, 0.05), 0.2), Some(ids[1]));
        assert_eq!(
            pick(d, &idx, DVec2::new(20.0, 0.0), 0.2),
            None,
            "circle center is not on the curve"
        );
        // Window must contain the whole entity.
        assert_eq!(
            window_select(
                d,
                &idx,
                DVec2::new(-1.0, -1.0),
                DVec2::new(11.0, 1.0),
                false,
                0.01
            ),
            vec![ids[0]]
        );
        assert!(
            window_select(
                d,
                &idx,
                DVec2::new(1.0, -3.0),
                DVec2::new(30.0, 3.0),
                false,
                0.01
            ) == vec![ids[1]]
        );
        // Crossing picks everything touched.
        assert_eq!(
            window_select(
                d,
                &idx,
                DVec2::new(5.0, -1.0),
                DVec2::new(19.0, 1.0),
                true,
                0.01
            ),
            vec![ids[0], ids[1]]
        );
        // A crossing window inside the circle that touches nothing selects nothing.
        assert!(
            window_select(
                d,
                &idx,
                DVec2::new(19.5, -0.5),
                DVec2::new(20.5, 0.5),
                true,
                0.01
            )
            .is_empty()
        );
    }

    #[test]
    fn grips_edit() {
        let l = EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 0.0)));
        assert_eq!(grips(&l).len(), 3);
        match move_grip(&l, 2, DVec2::new(10.0, 5.0)) {
            Some(EntityKind::Line(l)) => assert_eq!(l.b, DVec2::new(10.0, 5.0)),
            other => panic!("{other:?}"),
        }
        match move_grip(&l, 1, DVec2::new(5.0, 1.0)) {
            Some(EntityKind::Line(l)) => {
                assert_eq!((l.a, l.b), (DVec2::new(0.0, 1.0), DVec2::new(10.0, 1.0)))
            }
            other => panic!("{other:?}"),
        }
        let c = EntityKind::Circle(Circle2::new(DVec2::ZERO, 1.0));
        match move_grip(&c, 1, DVec2::new(3.0, 0.0)) {
            Some(EntityKind::Circle(c)) => assert!((c.r - 3.0).abs() < 1e-12),
            other => panic!("{other:?}"),
        }
        assert!(move_grip(&c, 1, DVec2::ZERO).is_none());
        assert!(move_grip(&c, 99, DVec2::ZERO).is_none());
        let a = EntityKind::Arc(Arc2::new(DVec2::ZERO, 1.0, 0.0, std::f64::consts::PI));
        match move_grip(&a, 2, DVec2::new(0.0, 2.0)) {
            Some(EntityKind::Arc(a)) => {
                assert!(a.start_point().distance(DVec2::new(1.0, 0.0)) < 1e-9);
                assert!(a.end_point().distance(DVec2::new(-1.0, 0.0)) < 1e-9);
                assert!(a.mid_point().distance(DVec2::new(0.0, 2.0)) < 1e-9);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn segment_rect() {
        let r = BBox2::new(DVec2::ZERO, DVec2::ONE);
        assert!(segment_hits_rect(
            DVec2::new(-1.0, 0.5),
            DVec2::new(2.0, 0.5),
            &r
        ));
        assert!(!segment_hits_rect(
            DVec2::new(-1.0, 2.0),
            DVec2::new(2.0, 2.0),
            &r
        ));
        assert!(segment_hits_rect(
            DVec2::new(-1.0, -1.0),
            DVec2::new(2.0, 2.0),
            &r
        ));
        assert!(!segment_hits_rect(
            DVec2::new(1.5, -1.0),
            DVec2::new(3.0, 0.5),
            &r
        ));
    }
}
