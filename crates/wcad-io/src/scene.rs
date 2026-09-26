//! Drawing → flat list of styled primitives in world coordinates (shared by SVG and PDF export).
//!
//! Resolves layers (visibility, plot flag, layer-0 inheritance inside blocks), ByLayer/ByBlock
//! colors, lineweights and linetypes, expands block inserts with their transforms, converts
//! dimensions to graphics and hatches to fills or pattern lines.

use wcad_doc::{
    Color, DimStyle, Drawing, Entity, EntityKind, HAlign, LayerId, LineWeight, LinetypeId,
    LinetypeRef, TextStyleId, VAlign,
};
use wcad_geom2d::Curve2;
use wcad_math::{BBox2, DAffine2, DVec2};

use crate::geom::{self, Path, Seg};
use crate::{dimgeom, mtext, pattern};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StrokeStyle {
    pub color: [u8; 3],
    /// Plotted lineweight in millimetres.
    pub weight_mm: f64,
    /// Dash pattern in world units (dash, gap, dash, gap, ...); `None` = continuous.
    pub dash: Option<Vec<f64>>,
}

#[derive(Clone, Debug)]
pub(crate) struct TextItem {
    /// Plain text; lines separated by `\n`.
    pub text: String,
    pub pos: DVec2,
    pub height: f64,
    pub rotation: f64,
    pub width_factor: f64,
    pub oblique: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    pub line_spacing: f64,
    /// MTEXT wrap width in world units.
    pub wrap_width: Option<f64>,
    pub color: [u8; 3],
    #[allow(dead_code)]
    pub style: Option<TextStyleId>,
}

#[derive(Clone, Debug)]
pub(crate) enum Item {
    Stroke {
        path: Path,
        style: StrokeStyle,
    },
    /// Even-odd filled area.
    Fill {
        path: Path,
        color: [u8; 3],
    },
    Text(TextItem),
    Point {
        p: DVec2,
        color: [u8; 3],
        weight_mm: f64,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Scene {
    pub items: Vec<Item>,
    pub bbox: Option<BBox2>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SceneOptions {
    /// Color for ACI 7 (and everything when `monochrome`).
    pub foreground: [u8; 3],
    pub monochrome: bool,
    /// Skip layers whose plot flag is off (PDF).
    pub plot_only: bool,
}

/// Default plotted lineweight (AutoCAD LWDEFAULT).
pub(crate) const DEFAULT_WEIGHT_MM: f64 = 0.25;

/// AutoCAD's default MTEXT/TEXT line pitch is 5/3 of the text height.
pub(crate) const LINE_PITCH: f64 = 5.0 / 3.0;

const MAX_DEPTH: usize = 16;

#[derive(Clone)]
struct Ctx {
    m: DAffine2,
    /// Layer that entities on layer "0" inherit (the insert's layer).
    layer: Option<LayerId>,
    /// Concrete ByBlock color.
    color: Color,
    linetype: Option<LinetypeId>,
    lineweight: LineWeight,
    ltscale: f64,
    depth: usize,
}

struct Builder<'a> {
    d: &'a Drawing,
    opt: SceneOptions,
    layer0: Option<LayerId>,
    scene: Scene,
    /// Entity visits so far (nested inserts can multiply work exponentially).
    visits: usize,
}

/// Work limits for pathological block nesting.
const MAX_VISITS: usize = 5_000_000;
const MAX_ITEMS: usize = 2_000_000;

pub(crate) fn build(d: &Drawing, opt: SceneOptions) -> Scene {
    let mut b = Builder {
        d,
        opt,
        layer0: d.layer_by_name("0"),
        scene: Scene::default(),
        visits: 0,
    };
    let root = Ctx {
        m: DAffine2::IDENTITY,
        layer: None,
        color: Color::WHITE,
        linetype: None,
        lineweight: LineWeight::Default,
        ltscale: 1.0,
        depth: 0,
    };
    for e in d.entities.values() {
        b.entity(e, &root);
    }
    b.scene
}

impl Builder<'_> {
    fn prop_layer(&self, e: &Entity, cx: &Ctx) -> LayerId {
        match (cx.layer, self.layer0) {
            (Some(l), Some(l0)) if e.layer == l0 => l,
            _ => e.layer,
        }
    }

    fn rgb(&self, c: Color, layer_color: Color, cx: &Ctx) -> [u8; 3] {
        if self.opt.monochrome {
            return self.opt.foreground;
        }
        c.resolve(layer_color, cx.color, self.opt.foreground)
    }

    fn entity(&mut self, e: &Entity, cx: &Ctx) {
        self.visits += 1;
        if self.visits > MAX_VISITS || self.scene.items.len() > MAX_ITEMS {
            return;
        }
        let layer_id = self.prop_layer(e, cx);
        let Some(layer) = self.d.layer(layer_id) else {
            return;
        };
        if !layer.visible || layer.frozen || (self.opt.plot_only && !layer.plot) {
            return;
        }
        // An entity's own layer being frozen hides it even inside a visible insert.
        if layer_id != e.layer && self.d.layer(e.layer).is_some_and(|l| l.frozen) {
            return;
        }
        let color = self.rgb(e.color, layer.color, cx);
        let weight = match e.lineweight {
            LineWeight::ByLayer => layer.lineweight,
            LineWeight::ByBlock => cx.lineweight,
            w => w,
        };
        let weight_mm = match weight {
            LineWeight::Mm100(n) => n as f64 / 100.0,
            _ => DEFAULT_WEIGHT_MM,
        };
        let linetype = match e.linetype {
            LinetypeRef::ByLayer => Some(layer.linetype),
            LinetypeRef::ByBlock => cx.linetype,
            LinetypeRef::Id(id) => Some(id),
        };
        let ltscale =
            self.d.tables.settings.ltscale.max(1e-12) * e.linetype_scale.max(1e-12) * cx.ltscale;
        let dash = linetype
            .and_then(|id| self.d.tables.linetypes.get(&id))
            .and_then(|lt| dash_array(&lt.pattern, ltscale));
        let style = StrokeStyle {
            color,
            weight_mm,
            dash,
        };

        match &e.kind {
            EntityKind::Point { p } => {
                let p = cx.m.transform_point2(*p);
                geom::bbox_add_point(&mut self.scene.bbox, p);
                self.scene.items.push(Item::Point {
                    p,
                    color,
                    weight_mm,
                });
            }
            EntityKind::Text(t) => {
                let item = self.text_item(
                    mtext::percent_codes(&t.text),
                    t.pos,
                    t.height,
                    t.rotation,
                    t.width_factor,
                    t.oblique,
                    t.halign,
                    t.valign,
                    1.0,
                    None,
                    color,
                    None, // TEXT stores its own width factor/oblique
                    cx,
                );
                self.push_text(item);
            }
            EntityKind::MText(m) => {
                let (halign, valign) = attachment_align(m.attachment);
                let item = self.text_item(
                    mtext::mtext_plain(&m.text),
                    m.pos,
                    m.height,
                    m.rotation,
                    1.0,
                    0.0,
                    halign,
                    valign,
                    m.line_spacing,
                    (m.width > 0.0).then_some(m.width),
                    color,
                    Some(m.style),
                    cx,
                );
                self.push_text(item);
            }
            EntityKind::Dimension(dim) => {
                let fallback;
                let ds: &DimStyle = match self.d.tables.dim_styles.get(&dim.style) {
                    Some(s) => s,
                    None => {
                        fallback = DimStyle::standard(self.d.tables.current_text_style);
                        &fallback
                    }
                };
                let Some(g) = dimgeom::build(dim, ds) else {
                    return;
                };
                let solid = StrokeStyle {
                    dash: None,
                    ..style
                };
                let mut path = Vec::new();
                for (a, b) in &g.lines {
                    path.push(Seg::Move(*a));
                    path.push(Seg::Line(*b));
                }
                for arc in &g.arcs {
                    path.extend(geom::curve_path(&Curve2::Arc(*arc)));
                }
                self.push_stroke(path, solid, cx);
                if !g.arrows.is_empty() {
                    let mut fill = Vec::new();
                    for t in &g.arrows {
                        fill.extend([
                            Seg::Move(t[0]),
                            Seg::Line(t[1]),
                            Seg::Line(t[2]),
                            Seg::Close,
                        ]);
                    }
                    self.push_fill(fill, color, cx);
                }
                let item = self.text_item(
                    mtext::mtext_plain(&g.text),
                    g.text_pos,
                    g.text_height,
                    g.text_rotation,
                    1.0,
                    0.0,
                    g.halign,
                    g.valign,
                    1.0,
                    None,
                    color,
                    Some(ds.text_style),
                    cx,
                );
                self.push_text(item);
            }
            EntityKind::Hatch(h) => {
                let mut path = Vec::new();
                for lp in &h.loops {
                    geom::loop_path(&lp.curves, &mut path);
                }
                if path.is_empty() {
                    return;
                }
                if h.is_solid() {
                    self.push_fill(path, color, cx);
                } else {
                    let mut bb = None;
                    geom::path_bbox(&path, &mut bb);
                    let tol = bb
                        .map(|b: BBox2| (b.max - b.min).max_element() * 1e-4)
                        .unwrap_or(1e-3)
                        .max(1e-9);
                    let polys = geom::flatten(&path, tol);
                    let fams = pattern::families(
                        &pattern::lines_or_default(&h.pattern.name),
                        h.pattern.angle,
                        h.pattern.scale,
                    );
                    let segs = pattern::hatch_segments(&polys, &fams);
                    let mut lines = Vec::with_capacity(segs.len() * 2);
                    for (a, b) in segs {
                        lines.push(Seg::Move(a));
                        lines.push(Seg::Line(b));
                    }
                    if !lines.is_empty() {
                        self.push_stroke(
                            lines,
                            StrokeStyle {
                                dash: None,
                                ..style
                            },
                            cx,
                        );
                    }
                }
            }
            EntityKind::Insert(ins) => {
                if cx.depth >= MAX_DEPTH {
                    return;
                }
                let Some(block) = self.d.blocks.get(&ins.block) else {
                    return;
                };
                let local =
                    DAffine2::from_scale_angle_translation(ins.scale, ins.rotation, ins.pos)
                        * DAffine2::from_translation(-block.base);
                let det = (ins.scale.x * ins.scale.y).abs().sqrt();
                let child = Ctx {
                    m: cx.m * local,
                    layer: Some(layer_id),
                    color: match e.color {
                        Color::ByLayer => layer.color,
                        Color::ByBlock => cx.color,
                        c => c,
                    },
                    linetype,
                    lineweight: weight,
                    ltscale: cx.ltscale
                        * if det.is_finite() && det > 0.0 {
                            det
                        } else {
                            1.0
                        },
                    depth: cx.depth + 1,
                };
                for be in block.entities.values() {
                    self.entity(be, &child);
                }
            }
            other => {
                if let Some(c) = other.as_curve() {
                    let path = geom::curve_path(&c);
                    self.push_stroke(path, style, cx);
                }
            }
        }
    }

    fn push_stroke(&mut self, mut path: Path, style: StrokeStyle, cx: &Ctx) {
        if path.is_empty() {
            return;
        }
        geom::transform_path(&mut path, &cx.m);
        geom::path_bbox(&path, &mut self.scene.bbox);
        self.scene.items.push(Item::Stroke { path, style });
    }

    fn push_fill(&mut self, mut path: Path, color: [u8; 3], cx: &Ctx) {
        if path.is_empty() {
            return;
        }
        geom::transform_path(&mut path, &cx.m);
        geom::path_bbox(&path, &mut self.scene.bbox);
        self.scene.items.push(Item::Fill { path, color });
    }

    #[allow(clippy::too_many_arguments)]
    fn text_item(
        &self,
        text: String,
        pos: DVec2,
        height: f64,
        rotation: f64,
        width_factor: f64,
        oblique: f64,
        halign: HAlign,
        valign: VAlign,
        line_spacing: f64,
        wrap_width: Option<f64>,
        color: [u8; 3],
        style: Option<TextStyleId>,
        cx: &Ctx,
    ) -> TextItem {
        let m = cx.m.matrix2;
        let dir = DVec2::new(rotation.cos(), rotation.sin());
        let x = m * dir;
        let y = m * wcad_math::perp(dir);
        let sx = x.length();
        let sy = y.length();
        let (sx, sy) = if sx > 0.0 && sy > 0.0 && sx.is_finite() && sy.is_finite() {
            (sx, sy)
        } else {
            (1.0, 1.0)
        };
        // Style width factor / oblique apply when the entity leaves them at the default.
        let (wf, obl) = match style.and_then(|s| self.d.tables.text_styles.get(&s)) {
            Some(st) => (
                if (width_factor - 1.0).abs() < 1e-12 {
                    st.width_factor
                } else {
                    width_factor
                },
                if oblique == 0.0 { st.oblique } else { oblique },
            ),
            None => (width_factor, oblique),
        };
        TextItem {
            text,
            pos: cx.m.transform_point2(pos),
            height: height * sy,
            rotation: x.y.atan2(x.x),
            width_factor: if wf.is_finite() && wf > 0.0 {
                wf * sx / sy
            } else {
                sx / sy
            },
            oblique: if obl.is_finite() { obl } else { 0.0 },
            halign,
            valign,
            line_spacing: if line_spacing.is_finite() && line_spacing > 0.0 {
                line_spacing
            } else {
                1.0
            },
            wrap_width: wrap_width.map(|w| w * sx),
            color,
            style,
        }
    }

    fn push_text(&mut self, t: TextItem) {
        if t.text.trim().is_empty() || !(t.height > 0.0) || !t.height.is_finite() {
            return;
        }
        // Rough extent for the bounding box.
        let lines = wrap_lines(&t.text, t.height, t.width_factor, t.wrap_width);
        let w = lines
            .iter()
            .map(|l| estimate_width(l, t.height, t.width_factor))
            .fold(0.0, f64::max);
        let h =
            t.height * (1.0 + LINE_PITCH * t.line_spacing * (lines.len().saturating_sub(1)) as f64);
        let (ox, b0) = text_block_offset(t.halign, t.valign, w, h, t.height);
        let dir = DVec2::new(t.rotation.cos(), t.rotation.sin());
        let up = wcad_math::perp(dir);
        let lo = b0 - (h - t.height) - DESCENT * t.height;
        let hi = b0 + t.height;
        for (cx, cy) in [(0.0, lo), (w, lo), (0.0, hi), (w, hi)] {
            geom::bbox_add_point(&mut self.scene.bbox, t.pos + dir * (ox + cx) + up * cy);
        }
        self.scene.items.push(Item::Text(t));
    }
}

/// Descender depth relative to the cap height.
pub(crate) const DESCENT: f64 = 0.3;

/// For a text block of width `w` and height `h` (cap of the first line to the baseline of the
/// last), returns the offset of the line start along the baseline and the offset of the first
/// baseline perpendicular to it, relative to the alignment point.
pub(crate) fn text_block_offset(
    halign: HAlign,
    valign: VAlign,
    w: f64,
    h: f64,
    cap: f64,
) -> (f64, f64) {
    let ox = match halign {
        HAlign::Left => 0.0,
        HAlign::Center => -w * 0.5,
        HAlign::Right => -w,
    };
    let b0 = match valign {
        VAlign::Baseline => 0.0,
        VAlign::Bottom => (h - cap) + DESCENT * cap,
        VAlign::Middle => h * 0.5 - cap,
        VAlign::Top => -cap,
    };
    (ox, b0)
}

/// (halign, valign) for an MTEXT attachment point 1..=9.
pub(crate) fn attachment_align(a: u8) -> (HAlign, VAlign) {
    let a = a.clamp(1, 9) - 1;
    let h = match a % 3 {
        0 => HAlign::Left,
        1 => HAlign::Center,
        _ => HAlign::Right,
    };
    let v = match a / 3 {
        0 => VAlign::Top,
        1 => VAlign::Middle,
        _ => VAlign::Bottom,
    };
    (h, v)
}

fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD)
}

/// Approximate advance of `s` for a font whose cap height is `height` (no font metrics needed).
fn char_em(c: char) -> f64 {
    if is_wide(c) {
        1.0
    } else if c == ' ' {
        0.3
    } else {
        0.56
    }
}

/// Approximate advance of `s` for a font whose cap height is `height` (no font metrics needed).
pub(crate) fn estimate_width(s: &str, height: f64, width_factor: f64) -> f64 {
    s.chars().map(char_em).sum::<f64>() * (height / 0.72) * width_factor
}

/// Split into lines (explicit `\n`) and wrap to `wrap` using estimated widths.
pub(crate) fn wrap_lines(
    text: &str,
    height: f64,
    width_factor: f64,
    wrap: Option<f64>,
) -> Vec<String> {
    let mut out = Vec::new();
    let em = (height / 0.72) * width_factor;
    for para in text.split('\n') {
        let Some(w) = wrap.filter(|w| *w > 0.0 && w.is_finite() && em > 0.0 && em.is_finite())
        else {
            out.push(para.to_string());
            continue;
        };
        let limit = w / em; // in ems
        let mut line = String::new();
        let mut line_em = 0.0;
        let mut chars_in_line = 0usize;
        let mut last_break: Option<usize> = None; // byte index in `line` after a space
        for c in para.chars() {
            line.push(c);
            line_em += char_em(c);
            chars_in_line += 1;
            if c == ' ' {
                last_break = Some(line.len());
            }
            let trailing = if c == ' ' { char_em(' ') } else { 0.0 };
            if line_em - trailing > limit && chars_in_line > 1 {
                let cut = match last_break {
                    Some(b) if !is_wide(c) && b < line.len() => b,
                    _ => line.len() - c.len_utf8(),
                };
                let rest = line.split_off(cut);
                out.push(line.trim_end().to_string());
                line = rest.trim_start().to_string();
                line_em = line.chars().map(char_em).sum();
                chars_in_line = line.chars().count();
                last_break = None;
            }
        }
        out.push(line);
    }
    out
}

/// Convert a linetype pattern (dash > 0, gap < 0, dot = 0) into an alternating dash array
/// starting with a dash, scaled by `scale`. `None` for continuous lines.
pub(crate) fn dash_array(pattern: &[f64], scale: f64) -> Option<Vec<f64>> {
    if pattern.is_empty() || !pattern.iter().any(|&d| d < 0.0) {
        return None;
    }
    let total: f64 = pattern.iter().map(|d| d.abs()).sum::<f64>() * scale;
    let dot = (total * 0.01).max(1e-9);
    let mut out: Vec<f64> = Vec::new();
    for &d in pattern {
        if !d.is_finite() {
            return None;
        }
        let is_gap = d < 0.0;
        let len = if d == 0.0 { dot } else { d.abs() * scale };
        let expect_gap = out.len() % 2 == 1;
        if is_gap == expect_gap {
            out.push(len);
        } else if let Some(last) = out.last_mut() {
            *last += len;
        } else {
            // Pattern starts with a gap: zero-length dash first.
            out.push(0.0);
            out.push(len);
        }
    }
    if out.len() % 2 == 1 {
        out.push(0.0);
    }
    (out.iter().sum::<f64>() > 0.0 && out.iter().all(|v| v.is_finite())).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashes_and_wrapping() {
        assert_eq!(dash_array(&[12.7, -6.35], 1.0), Some(vec![12.7, 6.35]));
        assert_eq!(dash_array(&[], 1.0), None);
        let d = dash_array(&[10.0, -2.0, 0.0, -2.0], 2.0).expect("dash-dot");
        assert_eq!(d.len(), 4);
        assert!((d[0] - 20.0).abs() < 1e-12 && (d[3] - 4.0).abs() < 1e-12);
        let lines = wrap_lines("aaaa bbbb cccc", 1.0, 1.0, Some(4.0));
        assert!(lines.len() >= 2, "{lines:?}");
        assert_eq!(
            wrap_lines("中文\n第二行", 1.0, 1.0, None),
            vec!["中文".to_string(), "第二行".to_string()]
        );
        assert_eq!(attachment_align(5), (HAlign::Center, VAlign::Middle));
        assert_eq!(attachment_align(7), (HAlign::Left, VAlign::Bottom));
    }
}
