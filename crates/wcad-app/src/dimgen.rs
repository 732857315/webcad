//! Dimension graphics used by the display lists, picking and snapping (owned by the annotate
//! package).
//!
//! [`dimension_geometry`] turns a [`Dimension`] into world-space lines, arcs, filled arrowheads
//! and one text item, following AutoCAD/ISO-25 conventions:
//!
//! - **Extension lines** start `ext_offset` away from the measured points and overshoot the
//!   dimension line by `ext_extend`.
//! - **Arrowheads** are closed filled triangles (`arrow_size` long, a third as wide). When the
//!   dimension line is too short they flip outside (pointing inward) with short tails; the
//!   dimension line itself is always drawn between the extension lines (ISO `DIMTOFL`).
//! - **Text** is centred on the dimension line and either *above* it (in the text's reading frame,
//!   `DIMTAD=1`, the default) or *centred* on it with the line broken around the text
//!   ([`TextPlacement`]). Rotation is always readable (never upside down; vertical text reads
//!   bottom-to-top). Text that does not fit between the extension lines moves outside, beside
//!   the second extension line, and the dimension line is extended under it.
//! - A user-moved text position ([`Dimension::text_pos`], the middle-centre of the text as in
//!   DXF group 11) is honoured; the dimension line/arc is extended to reach it and radial
//!   leaders are re-aimed so the text sits on them.
//! - **Values** use the style's decimals, prefix and suffix; radius/diameter get `R`/`Ø`, angles
//!   are in degrees. An override replaces the text, with `<>` standing for the measured value;
//!   a blank override (e.g. `" "`) suppresses the text.
//!
//! All sizes are multiplied by the style's overall `scale` (DIMSCALE).

use std::f64::consts::{FRAC_PI_2, PI};

use wcad_doc::{DimKind, DimStyle, Dimension, Drawing, HAlign, Tables, VAlign};
use wcad_geom2d::{Arc2, Curve, Curve2, Line2};
use wcad_math::{BBox2, DVec2};

/// Graphics of one dimension in world coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DimGeometry {
    pub lines: Vec<(DVec2, DVec2)>,
    pub arcs: Vec<Arc2>,
    /// Filled arrowhead triangles.
    pub arrows: Vec<[DVec2; 3]>,
    /// Text (may contain MTEXT codes).
    pub text: String,
    /// Anchor of the text for `halign`/`valign` (always the middle-centre here).
    pub text_pos: DVec2,
    pub text_rotation: f64,
    pub text_height: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    /// Definition points (for snapping and grips).
    pub def_points: Vec<DVec2>,
    /// Width of the text (measured with the CAD font, estimated without it).
    pub text_width: f64,
}

impl DimGeometry {
    pub fn curves(&self) -> Vec<Curve2> {
        self.lines
            .iter()
            .map(|(a, b)| Curve2::Line(Line2::new(*a, *b)))
            .chain(self.arcs.iter().map(|a| Curve2::Arc(*a)))
            .collect()
    }

    /// Corners of the text box (empty text: none).
    pub fn text_frame(&self) -> Option<[DVec2; 4]> {
        if self.text.is_empty() || !(self.text_height > 0.0) {
            return None;
        }
        let x = DVec2::from_angle(self.text_rotation);
        let y = x.perp();
        let hw = self.text_width.max(self.text_height * 0.5) * 0.5;
        let hh = self.text_height * 0.5;
        let c = self.text_pos;
        Some([
            c - x * hw - y * hh,
            c + x * hw - y * hh,
            c + x * hw + y * hh,
            c - x * hw + y * hh,
        ])
    }

    pub fn bbox(&self) -> BBox2 {
        let mut bb = BBox2::from_points(self.lines.iter().flat_map(|(a, b)| [*a, *b]));
        for a in &self.arcs {
            bb = bb.union(&a.bbox());
        }
        for t in &self.arrows {
            for p in t {
                bb.include(*p);
            }
        }
        if let Some(f) = self.text_frame() {
            for p in f {
                bb.include(p);
            }
        }
        if bb.min.is_finite() && bb.max.is_finite() {
            bb
        } else {
            BBox2::EMPTY
        }
    }

    /// Distance from `p` to the nearest line/arc/arrow or the text box (0 inside the text box).
    pub fn distance(&self, p: DVec2) -> f64 {
        let mut d = self
            .curves()
            .iter()
            .map(|c| c.closest(p).1.distance(p))
            .fold(f64::INFINITY, f64::min);
        for t in &self.arrows {
            for q in t {
                d = d.min(q.distance(p));
            }
        }
        if self.text_frame().is_some() {
            let x = DVec2::from_angle(self.text_rotation);
            let q = p - self.text_pos;
            let local = DVec2::new(q.dot(x), q.dot(x.perp())).abs();
            let half = DVec2::new(
                self.text_width.max(self.text_height * 0.5) * 0.5,
                self.text_height * 0.5,
            );
            let out = (local - half).max(DVec2::ZERO);
            d = d.min(out.length());
        }
        d
    }
}

/// Where the text sits relative to the dimension line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextPlacement {
    /// Above the line in the text's reading frame (ISO, `DIMTAD=1`).
    Above,
    /// On the line, which is broken around the text (`DIMTAD=0`).
    Centered,
}

/// Placement for a style. `DimStyle` has no DIMTAD field yet, so styles whose name contains
/// "center"/"centre"/"居中" use [`TextPlacement::Centered`]; everything else is above.
pub fn text_placement(style: &DimStyle) -> TextPlacement {
    let n = style.name.to_lowercase();
    if n.contains("center") || n.contains("centre") || n.contains("居中") {
        TextPlacement::Centered
    } else {
        TextPlacement::Above
    }
}

/// The dimension's style from the drawing tables (Standard fallback).
pub fn style_of(dim: &Dimension, d: &Drawing) -> DimStyle {
    d.tables
        .dim_styles
        .get(&dim.style)
        .cloned()
        .unwrap_or_else(|| DimStyle::standard(d.tables.current_text_style))
}

/// Measured value: length, or angle in radians for angular dimensions.
pub fn measure(kind: &DimKind) -> f64 {
    match kind {
        DimKind::Linear {
            p1, p2, rotation, ..
        } => (*p2 - *p1).dot(DVec2::from_angle(*rotation)).abs(),
        DimKind::Aligned { p1, p2, .. } => p1.distance(*p2),
        DimKind::Radius { center, point } => center.distance(*point),
        DimKind::Diameter { center, point } => 2.0 * center.distance(*point),
        DimKind::Angular { vertex, p1, p2, .. } => {
            wcad_math::ccw_sweep((*p1 - *vertex).to_angle(), (*p2 - *vertex).to_angle())
        }
        DimKind::Ordinate {
            origin,
            point,
            x_axis,
            ..
        } => {
            if *x_axis {
                (point.x - origin.x).abs()
            } else {
                (point.y - origin.y).abs()
            }
        }
    }
}

/// `v` with `decimals` places (0..=8); never prints `-0`.
pub fn format_value(v: f64, decimals: u8) -> String {
    if !v.is_finite() {
        return "?".into();
    }
    let n = decimals.min(8) as usize;
    let s = format!("{v:.n$}");
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_owned()
    } else {
        s
    }
}

/// The measured value formatted with the style (symbol, decimals, prefix, suffix).
pub fn value_text(kind: &DimKind, style: &DimStyle) -> String {
    let v = measure(kind);
    let value = match kind {
        DimKind::Angular { .. } => format!("{}°", format_value(v.to_degrees(), style.angle_decimals)),
        DimKind::Radius { .. } => format!("R{}", format_value(v, style.decimals)),
        DimKind::Diameter { .. } => format!("Ø{}", format_value(v, style.decimals)),
        _ => format_value(v, style.decimals),
    };
    format!("{}{}{}", style.prefix, value, style.suffix)
}

/// Displayed text: the formatted value, or the override with `<>` replaced by the value.
/// A blank (whitespace-only) override suppresses the text.
pub fn dim_text(dim: &Dimension, style: &DimStyle) -> String {
    let value = value_text(&dim.kind, style);
    match &dim.text_override {
        Some(o) if o.is_empty() => value,
        Some(o) if o.trim().is_empty() => String::new(),
        Some(o) => o.replace("<>", &value),
        None => value,
    }
}

/// Width of dimension text of `height` (longest line, MTEXT/%% codes resolved).
pub fn text_width(text: &str, height: f64) -> f64 {
    if text.is_empty() || !(height > 0.0) || !height.is_finite() {
        return 0.0;
    }
    let plain = wcad_geom2d::text::mtext_to_plain(text);
    plain
        .lines()
        .map(|l| match crate::fonts::cad_font() {
            Some(f) => wcad_geom2d::text::measure(f, l, height, 1.0),
            None => l.chars().count() as f64 * height * 0.7,
        })
        .filter(|w| w.is_finite())
        .fold(0.0, f64::max)
}

/// Readable text angle: in `(-90°, 90°]` so text never reads upside down.
pub fn readable(a: f64) -> f64 {
    let a = wcad_math::normalize_pi(a);
    if a > FRAC_PI_2 + 1e-9 || a <= -FRAC_PI_2 + 1e-9 {
        wcad_math::normalize_pi(a + PI)
    } else {
        a
    }
}

/// Closed arrowhead with its tip at `tip`, pointing along `dir`.
fn arrow(tip: DVec2, dir: DVec2, size: f64) -> [DVec2; 3] {
    let d = dir.normalize_or_zero();
    let n = d.perp();
    let back = tip - d * size;
    [tip, back + n * size / 6.0, back - n * size / 6.0]
}

/// Sanitized sizes of a style (world units).
#[derive(Clone, Copy, Debug)]
struct Sizes {
    arrow: f64,
    text: f64,
    gap: f64,
    exo: f64,
    exe: f64,
    placement: TextPlacement,
}

fn nonneg(v: f64, default: f64) -> f64 {
    if v.is_finite() && v >= 0.0 { v } else { default }
}

impl Sizes {
    fn of(style: &DimStyle) -> Self {
        let sc = if style.scale > 0.0 && style.scale.is_finite() {
            style.scale
        } else {
            1.0
        };
        let th = if style.text_height > 0.0 && style.text_height.is_finite() {
            style.text_height
        } else {
            2.5
        };
        Self {
            arrow: nonneg(style.arrow_size, 2.5) * sc,
            text: th * sc,
            gap: nonneg(style.text_gap, 0.625) * sc,
            exo: nonneg(style.ext_offset, 0.625) * sc,
            exe: nonneg(style.ext_extend, 1.25) * sc,
            placement: text_placement(style),
        }
    }

    /// Distance from the line to the text centre for "above" placement.
    fn lift(&self) -> f64 {
        match self.placement {
            TextPlacement::Above => self.gap + self.text * 0.5,
            TextPlacement::Centered => 0.0,
        }
    }
}

/// Builder: extension lines are kept apart from dimension lines/leaders (only the latter are
/// broken around the text).
#[derive(Default)]
struct Build {
    ext: Vec<(DVec2, DVec2)>,
    dim: Vec<(DVec2, DVec2)>,
    arcs: Vec<Arc2>,
    arrows: Vec<[DVec2; 3]>,
    text_pos: DVec2,
    text_rotation: f64,
    def_points: Vec<DVec2>,
}

/// Graphics for `dim` with its style.
pub fn dimension_geometry(dim: &Dimension, style: &DimStyle, _tables: &Tables) -> DimGeometry {
    let s = Sizes::of(style);
    let text = dim_text(dim, style);
    let tw = text_width(&text, s.text);
    let user = dim.text_pos.filter(|p| p.is_finite());
    let mut b = Build::default();
    match dim.kind {
        DimKind::Linear {
            p1,
            p2,
            line_point,
            rotation,
        } => {
            let dir = if rotation.is_finite() {
                DVec2::from_angle(rotation)
            } else {
                DVec2::X
            };
            linear(&mut b, &s, tw, p1, p2, line_point, dir, user);
        }
        DimKind::Aligned { p1, p2, line_point } => {
            let dir = (p2 - p1).try_normalize().unwrap_or(DVec2::X);
            linear(&mut b, &s, tw, p1, p2, line_point, dir, user);
        }
        DimKind::Radius { center, point } => radial(&mut b, &s, tw, center, point, false, user),
        DimKind::Diameter { center, point } => radial(&mut b, &s, tw, center, point, true, user),
        DimKind::Angular {
            vertex,
            p1,
            p2,
            arc_point,
        } => angular(&mut b, &s, tw, vertex, p1, p2, arc_point, user),
        DimKind::Ordinate {
            origin: _,
            point,
            leader_end,
            x_axis,
        } => ordinate(&mut b, &s, tw, point, leader_end, x_axis, user),
    }
    let mut g = DimGeometry {
        text,
        text_pos: b.text_pos,
        text_rotation: b.text_rotation,
        text_height: s.text,
        halign: HAlign::Center,
        valign: VAlign::Middle,
        def_points: b.def_points,
        text_width: tw,
        ..Default::default()
    };
    let finite = |p: &DVec2| p.is_finite();
    g.lines = b
        .ext
        .into_iter()
        .filter(|(a, c)| finite(a) && finite(c) && a != c)
        .collect();
    // Break dimension lines and leaders around the text box.
    let clip = (!g.text.is_empty()).then(|| {
        let x = DVec2::from_angle(g.text_rotation);
        let half = DVec2::new(tw * 0.5 + s.gap, s.text * 0.5 + s.gap * 0.5);
        (g.text_pos, x, half)
    });
    for (a, c) in b.dim {
        if !(finite(&a) && finite(&c)) || a == c {
            continue;
        }
        match clip {
            Some((o, x, half)) => g.lines.extend(clip_outside(a, c, o, x, half)),
            None => g.lines.push((a, c)),
        }
    }
    g.arcs = b
        .arcs
        .into_iter()
        .filter(|a| a.c.is_finite() && a.r > 0.0 && a.r.is_finite())
        .collect();
    g.arrows = b
        .arrows
        .into_iter()
        .filter(|t| t.iter().all(finite))
        .collect();
    if !g.text_pos.is_finite() {
        g.text_pos = DVec2::ZERO;
        g.text.clear();
    }
    if !g.text_rotation.is_finite() {
        g.text_rotation = 0.0;
    }
    g
}

/// Parts of segment `a`–`b` outside the rotated box centred at `o` (x axis `x`, half extents
/// `half`).
fn clip_outside(a: DVec2, b: DVec2, o: DVec2, x: DVec2, half: DVec2) -> Vec<(DVec2, DVec2)> {
    let y = x.perp();
    let la = DVec2::new((a - o).dot(x), (a - o).dot(y));
    let lb = DVec2::new((b - o).dot(x), (b - o).dot(y));
    let d = lb - la;
    // Liang–Barsky: parameter range of the segment inside the box.
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-d.x, la.x + half.x),
        (d.x, half.x - la.x),
        (-d.y, la.y + half.y),
        (d.y, half.y - la.y),
    ] {
        if p.abs() < 1e-15 {
            if q < 0.0 {
                return vec![(a, b)];
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
        }
    }
    if t0 >= t1 {
        return vec![(a, b)];
    }
    let mut out = Vec::new();
    if t0 > 1e-9 {
        out.push((a, a.lerp(b, t0)));
    }
    if t1 < 1.0 - 1e-9 {
        out.push((a.lerp(b, t1), b));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn linear(
    b: &mut Build,
    s: &Sizes,
    tw: f64,
    p1: DVec2,
    p2: DVec2,
    line_point: DVec2,
    dir: DVec2,
    user: Option<DVec2>,
) {
    let d1 = line_point + dir * (p1 - line_point).dot(dir);
    let d2 = line_point + dir * (p2 - line_point).dot(dir);
    b.def_points = vec![p1, p2, d1, d2];
    // Extension lines.
    for (p, d) in [(p1, d1), (p2, d2)] {
        let v = d - p;
        let len = v.length();
        if len > 1e-12 {
            let u = v / len;
            b.ext.push((p + u * s.exo.min(len), d + u * s.exe));
        }
    }
    let len = d1.distance(d2);
    let along = (d2 - d1).try_normalize().unwrap_or(dir);
    let rot = readable(along.to_angle());
    let up = DVec2::from_angle(rot).perp();
    b.text_rotation = rot;

    // Fit (AutoCAD "best fit"): both inside, else text inside, else arrows inside.
    let text_alone = len >= tw + 2.0 * s.gap;
    let both = match s.placement {
        TextPlacement::Above => len >= (2.0 * s.arrow).max(tw + 2.0 * s.gap),
        TextPlacement::Centered => len >= tw + 2.0 * s.gap + 2.0 * s.arrow,
    };
    let (arrows_in, text_in) = if both {
        (true, true)
    } else if text_alone && user.is_none() {
        (false, true)
    } else {
        (len >= 2.0 * s.arrow, false)
    };
    let tail = if arrows_in { 0.0 } else { 2.0 * s.arrow };

    b.dim.push((d1, d2));
    if arrows_in {
        b.arrows.push(arrow(d1, -along, s.arrow));
        b.arrows.push(arrow(d2, along, s.arrow));
    } else {
        b.arrows.push(arrow(d1, along, s.arrow));
        b.arrows.push(arrow(d2, -along, s.arrow));
        b.dim.push((d1 - along * tail, d1));
        b.dim.push((d2, d2 + along * tail));
    }

    let center = match user {
        Some(tp) => tp,
        None if text_in => (d1 + d2) * 0.5 + up * s.lift(),
        None => d2 + along * (tail + s.gap + tw * 0.5) + up * s.lift(),
    };
    b.text_pos = center;
    // Extend the dimension line under text placed beyond the extension lines.
    let t = (center - d1).dot(along);
    let reach = tw * 0.5 + s.gap;
    if t + reach > len + tail {
        b.dim.push((d2 + along * tail, d1 + along * (t + reach)));
    } else if t - reach < -tail {
        b.dim.push((d1 + along * (t - reach), d1 - along * tail));
    }
}

#[allow(clippy::too_many_arguments)]
fn radial(
    b: &mut Build,
    s: &Sizes,
    tw: f64,
    center: DVec2,
    point: DVec2,
    diameter: bool,
    user: Option<DVec2>,
) {
    let r = center.distance(point);
    let u0 = (point - center).try_normalize().unwrap_or(DVec2::X);
    let up_of = |u: DVec2| DVec2::from_angle(readable(u.to_angle())).perp();
    let lift = s.lift();
    // Text centre: user position, or computed from the fit.
    let tp = match user {
        Some(tp) => tp,
        None => {
            let up = up_of(u0);
            let inside = if diameter {
                2.0 * r >= tw + 2.0 * s.gap + 2.0 * s.arrow
            } else {
                r >= tw + 2.0 * s.gap + s.arrow
            };
            let along = match (inside, diameter) {
                (true, true) => 0.0,
                (true, false) => (r - s.arrow) * 0.5,
                (false, _) => r + s.arrow + s.gap + tw * 0.5,
            };
            center + u0 * along + up * lift
        }
    };
    // Aim the leader so the text sits on it (fixed point of anchor = tp - up(u) * lift).
    let mut u = u0;
    for _ in 0..6 {
        let anchor = tp - up_of(u) * lift;
        match (anchor - center).try_normalize() {
            Some(n) if (anchor - center).length() > 1e-9 * (1.0 + r) => u = n,
            _ => break,
        }
    }
    let up = up_of(u);
    let dist = (tp - up * lift - center).dot(u);
    let pa = center + u * r;
    let pb = center - u * r;
    b.text_pos = tp;
    b.text_rotation = readable(u.to_angle());
    b.def_points = if diameter {
        vec![center, pa, pb]
    } else {
        vec![center, pa]
    };
    if !(r > 0.0) {
        return;
    }
    let text_end = dist + tw * 0.5;
    let outside = dist - tw * 0.5 > r - s.gap;
    if diameter {
        if 2.0 * r >= 2.0 * s.arrow {
            b.dim.push((pb, pa));
            b.arrows.push(arrow(pa, u, s.arrow));
            b.arrows.push(arrow(pb, -u, s.arrow));
        } else {
            // Tiny circle: arrows outside pointing inward.
            b.dim.push((pb - u * 2.0 * s.arrow, pa));
            b.arrows.push(arrow(pa, -u, s.arrow));
            b.arrows.push(arrow(pb, u, s.arrow));
        }
        if text_end > r {
            b.dim.push((pa, center + u * text_end));
        }
    } else if outside {
        b.arrows.push(arrow(pa, -u, s.arrow));
        b.dim.push((pa, center + u * text_end.max(r + s.arrow)));
    } else {
        b.dim.push((center, pa));
        b.arrows.push(arrow(pa, u, s.arrow));
    }
    if outside {
        // Centre mark.
        let m = s.arrow * 0.5;
        b.ext.push((center - DVec2::X * m, center + DVec2::X * m));
        b.ext.push((center - DVec2::Y * m, center + DVec2::Y * m));
    }
}

#[allow(clippy::too_many_arguments)]
fn angular(
    b: &mut Build,
    s: &Sizes,
    tw: f64,
    v: DVec2,
    p1: DVec2,
    p2: DVec2,
    arc_point: DVec2,
    user: Option<DVec2>,
) {
    let a0 = (p1 - v).to_angle();
    let a1 = (p2 - v).to_angle();
    let sweep = wcad_math::ccw_sweep(a0, a1);
    let mut r = v.distance(arc_point);
    if !(r > 1e-12) {
        r = (v.distance(p1).max(v.distance(p2)) * 0.5).max(s.arrow * 4.0);
    }
    let at = |a: f64| v + DVec2::from_angle(a) * r;
    b.def_points = vec![v, p1, p2, at(a0), at(a1)];
    // Extension lines (only where the arc lies beyond the defining point).
    for (p, a) in [(p1, a0), (p2, a1)] {
        let u = DVec2::from_angle(a);
        let dp = (p - v).dot(u);
        if dp < r - s.exo {
            b.ext.push((v + u * (dp.max(0.0) + s.exo), at(a) + u * s.exe));
        }
    }
    let arc_len = r * sweep;
    let arrows_in = arc_len >= 2.0 * s.arrow;
    let da = (s.arrow / r).min(sweep * 0.5);
    if arrows_in {
        b.arrows.push(arrow(at(a0), at(a0) - at(a0 + da), s.arrow));
        b.arrows.push(arrow(at(a1), at(a1) - at(a1 - da), s.arrow));
    } else {
        let da = s.arrow / r;
        b.arrows.push(arrow(at(a0), at(a0) - at(a0 - da), s.arrow));
        b.arrows.push(arrow(at(a1), at(a1) - at(a1 + da), s.arrow));
        b.arcs.push(Arc2::new(v, r, a0 - 2.0 * da, a0));
        b.arcs.push(Arc2::new(v, r, a1, a1 + 2.0 * da));
    }

    let (ta, tp) = match user {
        Some(tp) => ((tp - v).to_angle(), tp),
        None => {
            let am = a0 + sweep * 0.5;
            let rot = readable(am + FRAC_PI_2);
            let up = DVec2::from_angle(rot).perp();
            (am, at(am) + up * s.lift())
        }
    };
    b.text_pos = tp;
    b.text_rotation = readable(ta + FRAC_PI_2);

    // The main arc, broken around centred text and extended towards text outside the sweep.
    let mut pieces = vec![(a0, a0 + sweep)];
    let off = wcad_math::normalize_0_2pi(ta - a0);
    let half = (tw * 0.5 + s.gap) / r;
    if off > sweep {
        // Outside the sweep: extend from the nearer end.
        let after = off - sweep;
        let before = std::f64::consts::TAU - off;
        if after <= before {
            pieces.push((a0 + sweep, a0 + off + half));
        } else {
            pieces.push((a0 - before - half, a0));
        }
    }
    let on_arc = (tp.distance(v) - r).abs() < s.text * 0.5 + s.gap * 0.5;
    if on_arc && !b_text_empty(tw) {
        let (lo, hi) = (a0 + off - half, a0 + off + half);
        pieces = pieces
            .into_iter()
            .flat_map(|(s0, s1)| {
                let mut v = Vec::new();
                if lo > s0 {
                    v.push((s0, lo.min(s1)));
                }
                if hi < s1 {
                    v.push((hi.max(s0), s1));
                }
                v
            })
            .collect();
    }
    for (s0, s1) in pieces {
        if s1 - s0 > 1e-9 {
            b.arcs.push(Arc2::new(v, r, s0, s1));
        }
    }
}

fn b_text_empty(tw: f64) -> bool {
    !(tw > 0.0)
}

#[allow(clippy::too_many_arguments)]
fn ordinate(
    b: &mut Build,
    s: &Sizes,
    tw: f64,
    point: DVec2,
    leader_end: DVec2,
    x_axis: bool,
    user: Option<DVec2>,
) {
    let ax = if x_axis {
        DVec2::new(0.0, if leader_end.y >= point.y { 1.0 } else { -1.0 })
    } else {
        DVec2::new(if leader_end.x >= point.x { 1.0 } else { -1.0 }, 0.0)
    };
    let run = (leader_end - point).dot(ax);
    let lat = leader_end - point - ax * run;
    let start = point + ax * s.exo.min(run.max(0.0));
    if lat.length() < 1e-9 * (1.0 + run.abs()) {
        b.dim.push((start, leader_end));
    } else {
        // Dogleg: along the axis, a jog of one arrow length, then along the axis to the end.
        let k = (run * 0.5).max(s.exo.min(run.max(0.0)));
        let j1 = point + ax * k;
        let j2 = if run - k - s.arrow > 0.0 {
            point + ax * (k + s.arrow) + lat
        } else {
            leader_end
        };
        b.dim.push((start, j1));
        b.dim.push((j1, j2));
        b.dim.push((j2, leader_end));
    }
    b.text_rotation = readable(ax.to_angle());
    b.text_pos = user.unwrap_or(leader_end + ax * (s.gap + tw * 0.5));
    b.def_points = vec![point, leader_end];
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::Document;

    fn dim(kind: DimKind) -> Dimension {
        Dimension {
            kind,
            style: Document::new().drawing.tables.current_dim_style,
            text_override: None,
            text_pos: None,
        }
    }

    fn geom(d: &Dimension) -> DimGeometry {
        let doc = Document::new();
        let st = DimStyle::standard(doc.drawing.tables.current_text_style);
        dimension_geometry(d, &st, &doc.drawing.tables)
    }

    fn near(a: DVec2, b: DVec2) -> bool {
        a.distance(b) < 1e-9
    }

    fn has_line(g: &DimGeometry, a: DVec2, b: DVec2) -> bool {
        g.lines
            .iter()
            .any(|(p, q)| (near(*p, a) && near(*q, b)) || (near(*p, b) && near(*q, a)))
    }

    #[test]
    fn aligned_horizontal_numbers() {
        let g = geom(&dim(DimKind::Aligned {
            p1: DVec2::ZERO,
            p2: DVec2::new(100.0, 0.0),
            line_point: DVec2::new(0.0, 10.0),
        }));
        assert_eq!(g.text, "100.00");
        // Extension lines: from 0.625 above the points to 1.25 past the dimension line.
        assert!(has_line(&g, DVec2::new(0.0, 0.625), DVec2::new(0.0, 11.25)));
        assert!(has_line(&g, DVec2::new(100.0, 0.625), DVec2::new(100.0, 11.25)));
        // Dimension line and inward-pointing closed arrows at both ends.
        assert!(has_line(&g, DVec2::new(0.0, 10.0), DVec2::new(100.0, 10.0)));
        assert_eq!(g.arrows.len(), 2);
        assert!(near(g.arrows[0][0], DVec2::new(0.0, 10.0)));
        assert!(near(g.arrows[0][1], DVec2::new(2.5, 10.0 + 2.5 / 6.0)));
        assert!(near(g.arrows[1][0], DVec2::new(100.0, 10.0)));
        assert!(g.arrows[1][1].x < 100.0);
        // Text above the line: centre at gap + h/2 = 0.625 + 1.25.
        assert!(near(g.text_pos, DVec2::new(50.0, 11.875)), "{:?}", g.text_pos);
        assert_eq!(g.text_rotation, 0.0);
        assert_eq!((g.halign, g.valign), (HAlign::Center, VAlign::Middle));
        assert!(g.text_width > 5.0 && g.text_width < 12.0, "{}", g.text_width);
        assert!(g.distance(DVec2::new(50.0, 10.0)) < 1e-9);
        assert!(g.distance(g.text_pos) == 0.0);
        let bb = g.bbox();
        assert!(bb.max.y >= 12.5 && bb.min.y <= 0.625 + 1e-9);
    }

    #[test]
    fn linear_vertical_text_reads_bottom_to_top_on_the_left() {
        let g = geom(&dim(DimKind::Linear {
            p1: DVec2::new(0.0, 0.0),
            p2: DVec2::new(3.0, 40.0),
            line_point: DVec2::new(20.0, 0.0),
            rotation: FRAC_PI_2,
        }));
        assert_eq!(g.text, "40.00");
        assert!(has_line(&g, DVec2::new(20.0, 0.0), DVec2::new(20.0, 40.0)));
        // Extension lines run horizontally to x = 20 + 1.25.
        assert!(has_line(&g, DVec2::new(0.625, 0.0), DVec2::new(21.25, 0.0)));
        assert!(has_line(&g, DVec2::new(3.625, 40.0), DVec2::new(21.25, 40.0)));
        assert!((g.text_rotation - FRAC_PI_2).abs() < 1e-12);
        assert!(near(g.text_pos, DVec2::new(20.0 - 1.875, 20.0)), "{:?}", g.text_pos);
    }

    #[test]
    fn rotated_upside_down_direction_is_made_readable() {
        let g = geom(&dim(DimKind::Aligned {
            p1: DVec2::new(10.0, 0.0),
            p2: DVec2::ZERO,
            line_point: DVec2::new(0.0, -5.0),
        }));
        assert_eq!(g.text_rotation, 0.0);
        // "Above" in the reading frame (+Y), even though the line is below the points.
        assert!(near(g.text_pos, DVec2::new(5.0, -5.0 + 1.875)), "{:?}", g.text_pos);
        let a = readable(170f64.to_radians());
        assert!((a + 10f64.to_radians()).abs() < 1e-12);
        assert!((readable(-FRAC_PI_2) - FRAC_PI_2).abs() < 1e-12);
    }

    #[test]
    fn short_dimension_moves_arrows_and_text_outside() {
        let g = geom(&dim(DimKind::Aligned {
            p1: DVec2::ZERO,
            p2: DVec2::new(3.0, 0.0),
            line_point: DVec2::new(0.0, 5.0),
        }));
        // Arrows flipped: the first points +X from outside the left extension line.
        assert!(near(g.arrows[0][0], DVec2::ZERO + DVec2::new(0.0, 5.0)));
        assert!(g.arrows[0][1].x < 0.0);
        assert!(g.arrows[1][1].x > 3.0);
        // Text beyond the second extension line, the dimension line extended under it.
        assert!(g.text_pos.x > 3.0 + 5.0, "{:?}", g.text_pos);
        let far = g
            .lines
            .iter()
            .flat_map(|(a, b)| [a.x, b.x])
            .fold(f64::MIN, f64::max);
        assert!(far >= g.text_pos.x + g.text_width * 0.5 - 1e-9);
    }

    #[test]
    fn centered_style_breaks_the_line() {
        let doc = Document::new();
        let mut st = DimStyle::standard(doc.drawing.tables.current_text_style);
        st.name = "ISO-Center".into();
        let d = dim(DimKind::Aligned {
            p1: DVec2::ZERO,
            p2: DVec2::new(100.0, 0.0),
            line_point: DVec2::new(0.0, 10.0),
        });
        let g = dimension_geometry(&d, &st, &doc.drawing.tables);
        assert!(near(g.text_pos, DVec2::new(50.0, 10.0)));
        let on_line: Vec<_> = g
            .lines
            .iter()
            .filter(|(a, b)| (a.y - 10.0).abs() < 1e-9 && (b.y - 10.0).abs() < 1e-9)
            .collect();
        assert_eq!(on_line.len(), 2, "{on_line:?}");
        let gap_lo = on_line.iter().map(|(a, b)| a.x.max(b.x)).fold(f64::MAX, f64::min);
        let gap_hi = on_line.iter().map(|(a, b)| a.x.min(b.x)).fold(f64::MIN, f64::max);
        assert!((gap_hi - gap_lo - (g.text_width + 2.0 * 0.625)).abs() < 1e-6);
    }

    #[test]
    fn value_formatting() {
        let doc = Document::new();
        let mut st = DimStyle::standard(doc.drawing.tables.current_text_style);
        st.decimals = 1;
        st.prefix = "~".into();
        st.suffix = " mm".into();
        let mut d = dim(DimKind::Aligned {
            p1: DVec2::ZERO,
            p2: DVec2::new(12.345, 0.0),
            line_point: DVec2::new(0.0, 5.0),
        });
        assert_eq!(dim_text(&d, &st), "~12.3 mm");
        d.text_override = Some("L=<> (ref)".into());
        assert_eq!(dim_text(&d, &st), "L=~12.3 mm (ref)");
        d.text_override = Some(" ".into());
        assert_eq!(dim_text(&d, &st), "");
        assert!(dimension_geometry(&d, &st, &doc.drawing.tables).text.is_empty());
        d.text_override = Some(String::new());
        assert_eq!(dim_text(&d, &st), "~12.3 mm");
        assert_eq!(format_value(-0.0001, 2), "0.00");
        assert_eq!(format_value(2.5, 0), "2");
        assert_eq!(format_value(f64::NAN, 2), "?");
    }

    #[test]
    fn radius_and_diameter() {
        let g = geom(&dim(DimKind::Radius {
            center: DVec2::ZERO,
            point: DVec2::new(20.0, 0.0),
        }));
        assert_eq!(g.text, "R20.00");
        assert!(has_line(&g, DVec2::ZERO, DVec2::new(20.0, 0.0)));
        assert!(near(g.arrows[0][0], DVec2::new(20.0, 0.0)));
        assert!(g.arrows[0][1].x < 20.0, "arrow points outward from inside");
        assert!(near(g.text_pos, DVec2::new(8.75, 1.875)), "{:?}", g.text_pos);

        let g = geom(&dim(DimKind::Diameter {
            center: DVec2::new(5.0, 5.0),
            point: DVec2::new(5.0, 25.0),
        }));
        assert_eq!(g.text, "Ø40.00");
        assert!(has_line(&g, DVec2::new(5.0, -15.0), DVec2::new(5.0, 25.0)));
        assert_eq!(g.arrows.len(), 2);
        assert!((g.text_rotation - FRAC_PI_2).abs() < 1e-12);
        assert!(near(g.text_pos, DVec2::new(5.0 - 1.875, 5.0)), "{:?}", g.text_pos);

        // User text outside: leader re-aimed through the centre, arrow on the circle pointing in.
        let mut d = dim(DimKind::Radius {
            center: DVec2::ZERO,
            point: DVec2::new(10.0, 0.0),
        });
        d.text_pos = Some(DVec2::new(30.0, 30.0));
        let g = geom(&d);
        assert_eq!(g.text_pos, DVec2::new(30.0, 30.0));
        let tip = g.arrows[0][0];
        assert!((tip.length() - 10.0).abs() < 1e-9);
        let back = (g.arrows[0][1] + g.arrows[0][2]) * 0.5;
        assert!(back.length() > 10.0, "arrow outside the circle");
        // The text centre sits `lift` above the leader line through the centre.
        let u = tip / 10.0;
        let off = (DVec2::new(30.0, 30.0)).dot(u.perp()).abs();
        assert!((off - 1.875).abs() < 1e-6, "{off}");
        assert!(g.lines.len() >= 3, "leader + centre mark");
    }

    #[test]
    fn angular_arc_arrows_and_text() {
        let g = geom(&dim(DimKind::Angular {
            vertex: DVec2::ZERO,
            p1: DVec2::new(10.0, 0.0),
            p2: DVec2::new(0.0, 10.0),
            arc_point: DVec2::new(20.0, 20.0),
        }));
        assert_eq!(g.text, "90°");
        let r = 800f64.sqrt();
        assert_eq!(g.arcs.len(), 1);
        assert!((g.arcs[0].r - r).abs() < 1e-9);
        assert!((g.arcs[0].sweep() - FRAC_PI_2).abs() < 1e-9);
        // Extension lines from beyond the defining points out past the arc.
        assert!(has_line(&g, DVec2::new(10.625, 0.0), DVec2::new(r + 1.25, 0.0)));
        assert!(has_line(&g, DVec2::new(0.0, 10.625), DVec2::new(0.0, r + 1.25)));
        assert!(near(g.arrows[0][0], DVec2::new(r, 0.0)));
        assert!(g.arrows[0][1].y > 0.0, "arrow body inside the arc");
        let mid = DVec2::from_angle(PI / 4.0);
        assert!(near(g.text_pos, mid * r + DVec2::from_angle(PI / 4.0) * 1.875));
        assert!((g.text_rotation + PI / 4.0).abs() < 1e-9);
        // Arc inside the lines: no extension lines.
        let g = geom(&dim(DimKind::Angular {
            vertex: DVec2::ZERO,
            p1: DVec2::new(50.0, 0.0),
            p2: DVec2::new(0.0, 50.0),
            arc_point: DVec2::new(10.0, 10.0),
        }));
        assert!(g.lines.is_empty(), "{:?}", g.lines);
    }

    #[test]
    fn ordinate_leader_and_text() {
        let g = geom(&dim(DimKind::Ordinate {
            origin: DVec2::ZERO,
            point: DVec2::new(30.0, 10.0),
            leader_end: DVec2::new(30.0, 40.0),
            x_axis: true,
        }));
        assert_eq!(g.text, "30.00");
        assert!(has_line(&g, DVec2::new(30.0, 10.625), DVec2::new(30.0, 40.0)));
        assert!((g.text_rotation - FRAC_PI_2).abs() < 1e-12);
        assert!(near(g.text_pos, DVec2::new(30.0, 40.625 + g.text_width * 0.5)));
        // Jogged leader.
        let g = geom(&dim(DimKind::Ordinate {
            origin: DVec2::ZERO,
            point: DVec2::new(30.0, 10.0),
            leader_end: DVec2::new(60.0, 12.0),
            x_axis: false,
        }));
        assert_eq!(g.text, "10.00");
        assert_eq!(g.lines.len(), 3);
        assert_eq!(g.text_rotation, 0.0);
    }

    #[test]
    fn hostile_input_never_panics() {
        let doc = Document::new();
        let mut st = DimStyle::standard(doc.drawing.tables.current_text_style);
        st.scale = f64::NAN;
        st.text_height = -1.0;
        st.arrow_size = f64::INFINITY;
        st.decimals = 255;
        let n = f64::NAN;
        for kind in [
            DimKind::Linear { p1: DVec2::ZERO, p2: DVec2::ZERO, line_point: DVec2::ZERO, rotation: n },
            DimKind::Aligned { p1: DVec2::splat(n), p2: DVec2::ONE, line_point: DVec2::ZERO },
            DimKind::Radius { center: DVec2::ZERO, point: DVec2::ZERO },
            DimKind::Diameter { center: DVec2::ONE, point: DVec2::splat(f64::INFINITY) },
            DimKind::Angular { vertex: DVec2::ZERO, p1: DVec2::ZERO, p2: DVec2::ZERO, arc_point: DVec2::ZERO },
            DimKind::Ordinate { origin: DVec2::ZERO, point: DVec2::ZERO, leader_end: DVec2::ZERO, x_axis: true },
        ] {
            let mut d = dim(kind);
            let g = dimension_geometry(&d, &st, &doc.drawing.tables);
            let _ = (g.bbox(), g.distance(DVec2::ONE));
            d.text_pos = Some(DVec2::ZERO);
            let g = dimension_geometry(&d, &DimStyle::standard(st.text_style), &doc.drawing.tables);
            let _ = (g.bbox(), g.distance(DVec2::ONE));
        }
    }
}
