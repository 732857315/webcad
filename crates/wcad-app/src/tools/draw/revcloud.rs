//! REVCLOUD: a rectangular, outward-scalloped closed polyline with bounded preview cost.

use std::f64::consts::PI;

use wcad_doc::EntityKind;
use wcad_geom2d::{Curve2, PolyVertex, Polyline2, bulge_to_arc, sweep_to_bulge};
use wcad_math::DVec2;

use super::{commit, defaults, finite_pts, kw, num, positive, set_defaults, strings_of};
use crate::i18n::{Lang, core, fmt};
use crate::tools::{Accept, Keyword, Preview, Tool, ToolCx, ToolFlow, ToolInput};

/// The cap applies to the whole cloud, including the closing arc, for creation and preview.
pub const MAX_ARC_SEGMENTS: usize = 4096;

/// A CCW rectangular cloud made of outward 120-degree circular arcs.
/// `arc_chord` is the requested chord length; zero chooses a size-dependent default.
/// Each edge has at least one arc and at most a quarter of [`MAX_ARC_SEGMENTS`].
/// When capped, the chord length increases rather than allocating unbounded geometry.
pub fn revcloud_rect(a: DVec2, b: DVec2, arc_chord: f64) -> Option<Polyline2> {
    if !finite_pts(&[a, b]) || !arc_chord.is_finite() || arc_chord < 0.0 {
        return None;
    }
    let lo = a.min(b);
    let hi = a.max(b);
    let size = hi - lo;
    if !positive(size.x) || !positive(size.y) {
        return None;
    }
    let chord = if arc_chord == 0.0 {
        size.max_element() / 8.0
    } else {
        arc_chord
    };
    if !positive(chord) {
        return None;
    }
    // Clamp in floating point before casting: length/chord can overflow for tiny positive input.
    let count = |length: f64| {
        (length / chord)
            .ceil()
            .clamp(1.0, (MAX_ARC_SEGMENTS / 4) as f64) as usize
    };
    let nx = count(size.x);
    let ny = count(size.y);
    let corners = [lo, DVec2::new(hi.x, lo.y), hi, DVec2::new(lo.x, hi.y)];
    let counts = [nx, ny, nx, ny];
    let bulge = sweep_to_bulge(2.0 * PI / 3.0);
    let mut verts = Vec::with_capacity(2 * (nx + ny));
    for edge in 0..4 {
        let start = corners[edge];
        let delta = corners[(edge + 1) % 4] - start;
        for i in 0..counts[edge] {
            let p = start + delta * (i as f64 / counts[edge] as f64);
            verts.push(PolyVertex::with_bulge(p, bulge));
        }
    }
    for i in 0..verts.len() {
        let a = verts[i].p;
        let b = verts[(i + 1) % verts.len()].p;
        let arc = bulge_to_arc(a, b, bulge)?;
        if !finite_pts(&[a, b])
            || !positive(arc.r)
            || !(arc.c - DVec2::splat(arc.r)).is_finite()
            || !(arc.c + DVec2::splat(arc.r)).is_finite()
        {
            return None;
        }
    }
    Some(Polyline2 {
        verts,
        closed: true,
    })
}

pub struct RevcloudTool {
    first: Option<DVec2>,
    arc_chord: f64,
    setting_arc: bool,
}

impl Default for RevcloudTool {
    fn default() -> Self {
        Self {
            first: None,
            arc_chord: defaults().revcloud_arc,
            setting_arc: false,
        }
    }
}

impl Tool for RevcloudTool {
    fn name(&self) -> &'static str {
        "REVCLOUD"
    }

    fn prompt(&self, lang: Lang) -> String {
        let s = strings_of(lang);
        if self.setting_arc {
            let value = if self.arc_chord == 0.0 {
                s.rc_auto.into()
            } else {
                num(self.arc_chord)
            };
            fmt(s.rc_arc_length, &[&value])
        } else if self.first.is_some() {
            s.rc_opposite.into()
        } else {
            s.rc_first.into()
        }
    }

    fn keywords(&self, lang: Lang) -> Vec<Keyword> {
        let s = strings_of(lang);
        if self.setting_arc {
            vec![kw("Auto", "A", s.rc_auto)]
        } else {
            vec![kw("ArcLength", "A", s.kw_arc_length)]
        }
    }

    fn accepts(&self) -> Accept {
        if self.setting_arc {
            Accept::VALUE
        } else {
            Accept::POINT
        }
    }

    fn base_point(&self) -> Option<DVec2> {
        if self.setting_arc { None } else { self.first }
    }

    fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx<'_>) -> ToolFlow {
        let input = match input {
            ToolInput::Escape => return ToolFlow::Cancel,
            ToolInput::Enter if self.setting_arc => ToolInput::Value(self.arc_chord),
            ToolInput::Keyword("Auto") if self.setting_arc => ToolInput::Value(0.0),
            ToolInput::Enter if self.first.is_none() => return ToolFlow::Cancel,
            input => input,
        };
        match input {
            ToolInput::Keyword("ArcLength") if !self.setting_arc => self.setting_arc = true,
            ToolInput::Value(v) if self.setting_arc => {
                if v.is_finite() && v >= 0.0 {
                    self.arc_chord = v;
                    set_defaults(|d| d.revcloud_arc = v);
                    self.setting_arc = false;
                } else {
                    cx.error(fmt(core(cx.lang()).invalid_input, &[&v]));
                }
            }
            ToolInput::Point(p) if !self.setting_arc => {
                if !p.is_finite() {
                    cx.error(core(cx.lang()).point_expected);
                    return ToolFlow::Continue;
                }
                if let Some(a) = self.first {
                    if let Some(pl) = revcloud_rect(a, p, self.arc_chord) {
                        commit(cx, self.name(), EntityKind::Polyline(pl));
                        return ToolFlow::Done;
                    }
                    cx.error(strings_of(cx.lang()).zero_length);
                } else {
                    self.first = Some(p);
                }
            }
            _ => {}
        }
        ToolFlow::Continue
    }

    fn preview(&self, cx: &ToolCx<'_>, out: &mut Preview) {
        if !self.setting_arc
            && let Some(a) = self.first
            && let Some(b) = cx.cursor()
            && let Some(pl) = revcloud_rect(a, b, self.arc_chord)
        {
            out.curves.push(Curve2::Polyline(pl));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;
    use crate::tools::draw::Defaults;
    use wcad_geom2d::PolySegment;

    #[test]
    fn closed_outward_cloud_has_exact_boundary_endpoints_and_ccw_arcs() {
        let a = DVec2::new(1.0, 2.0);
        let b = DVec2::new(11.0, 8.0);
        let p = revcloud_rect(a, b, 2.0).unwrap();
        assert!(p.closed);
        assert_eq!(p.segment_count(), 16);
        assert!(p.signed_area() > 60.0);
        for (i, seg) in p.segments().enumerate() {
            let PolySegment::Arc(arc) = seg else {
                panic!("expected cloud arc")
            };
            assert!((arc.sweep - 2.0 * PI / 3.0).abs() < 1e-12);
            let mid = arc.point_at(0.5);
            assert!(mid.x < a.x || mid.x > b.x || mid.y < a.y || mid.y > b.y);
            assert!(seg.start().distance(p.verts[i].p) < 1e-12);
            assert!(seg.end().distance(p.verts[(i + 1) % p.verts.len()].p) < 1e-12);
        }
        assert_eq!(revcloud_rect(b, a, 2.0).unwrap(), p);
    }

    #[test]
    fn automatic_length_and_tiny_length_are_bounded() {
        let p = revcloud_rect(DVec2::ZERO, DVec2::splat(10.0), 0.0).unwrap();
        assert_eq!(p.segment_count(), 32);
        for tiny in [1e-20, f64::MIN_POSITIVE, f64::from_bits(1)] {
            let p = revcloud_rect(DVec2::ZERO, DVec2::new(10.0, 4.0), tiny).unwrap();
            assert_eq!(p.segment_count(), MAX_ARC_SEGMENTS);
            assert!(p.segments().all(|s| matches!(s, PolySegment::Arc(_))));
        }
        assert_eq!(
            revcloud_rect(DVec2::ZERO, DVec2::ONE, f64::MAX)
                .unwrap()
                .segment_count(),
            4
        );
        assert!(
            revcloud_rect(DVec2::ZERO, DVec2::new(1e6, 1.0), 0.01)
                .unwrap()
                .segment_count()
                <= MAX_ARC_SEGMENTS
        );
    }

    #[test]
    fn invalid_geometry_and_arc_length_are_rejected() {
        for length in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(revcloud_rect(DVec2::ZERO, DVec2::ONE, length).is_none());
        }
        for b in [
            DVec2::ZERO,
            DVec2::X,
            DVec2::Y,
            DVec2::splat(f64::NAN),
            DVec2::splat(f64::INFINITY),
            DVec2::splat(f64::MAX),
        ] {
            assert!(revcloud_rect(DVec2::ZERO, b, 1.0).is_none());
        }
    }

    #[test]
    fn arc_length_numeric_contract_preview_creation_and_undo() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.ed.draft.osnap_on = false;
        h.cmd("REVCLOUD").cmd("A");
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        assert_eq!(h.ed.tool_base_point(), None);
        h.cmd("-1");
        h.ed.feed(ToolInput::Point(DVec2::ONE));
        assert_eq!(h.ed.tool_accepts(), Accept::VALUE);
        h.cmd("2").cmd("0,0").cmd("A");
        assert_eq!(h.ed.tool_base_point(), None);
        h.enter().cmd("0,0");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        h.hover(10.0, 6.0);
        let preview = h.ed.preview.curves.clone();
        assert_eq!(preview.len(), 1);
        h.cmd("10,6");
        assert_eq!(
            h.of_type("LWPOLYLINE")[0].1.as_curve(),
            Some(preview[0].clone())
        );
        h.cmd("U");
        assert_eq!(h.count("LWPOLYLINE"), 0);
        assert!(h.ed.doc.undo().is_none());
        set_defaults(|d| *d = Defaults::default());
    }

    #[test]
    fn capped_preview_auto_reset_and_cancel() {
        set_defaults(|d| *d = Defaults::default());
        let mut h = Harness::new();
        h.cmd("REVCLOUD")
            .cmd("A")
            .cmd("1e-300")
            .cmd("0,0")
            .hover(8.0, 5.0);
        let Curve2::Polyline(p) = &h.ed.preview.curves[0] else {
            panic!("expected cloud")
        };
        assert_eq!(p.segment_count(), MAX_ARC_SEGMENTS);
        h.esc();
        assert_eq!(h.count("LWPOLYLINE"), 0);
        assert!(h.ed.doc.undo().is_none());
        h.cmd("REVCLOUD").cmd("A").cmd("Auto");
        assert_eq!(defaults().revcloud_arc, 0.0);
        h.cmd("0,0").cmd("8,8");
        let EntityKind::Polyline(p) = &h.of_type("LWPOLYLINE")[0].1 else {
            panic!("expected cloud")
        };
        assert_eq!(p.segment_count(), 32);
        set_defaults(|d| *d = Defaults::default());
    }
}
