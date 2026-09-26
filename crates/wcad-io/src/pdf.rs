//! Vector PDF export (single page) with `pdf-writer`.
//!
//! Geometry is transformed to page points in f64 before writing (pdf-writer stores f32), arcs
//! become cubic Béziers, hatches become fills or clipped pattern lines, and text is drawn as filled
//! glyph outlines from `wcad_geom2d::text::layout` using the font bytes supplied by the caller.
//! Without a font (or while the layout engine yields no glyphs) text falls back to the built-in
//! Helvetica font for Latin-1 characters.

use pdf_writer::types::{LineCapStyle, LineJoinStyle};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, Str};
use wcad_doc::{Drawing, HAlign};
use wcad_geom2d::text::{Font, PathCmd, TextSpec, layout};
use wcad_math::{BBox2, DVec2};

use crate::geom::{Seg, arc_to_cubics};
use crate::scene::{self, Item, LINE_PITCH, SceneOptions, TextItem};
use crate::{Error, Result};

const PT_PER_MM: f64 = 72.0 / 25.4;

/// Paper sizes (portrait width × height in millimetres).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paper {
    A4,
    A3,
    A2,
    A1,
    A0,
    Letter,
    Custom { width_mm: f64, height_mm: f64 },
}

impl Paper {
    /// Portrait size in millimetres.
    pub fn size_mm(self) -> (f64, f64) {
        match self {
            Paper::A4 => (210.0, 297.0),
            Paper::A3 => (297.0, 420.0),
            Paper::A2 => (420.0, 594.0),
            Paper::A1 => (594.0, 841.0),
            Paper::A0 => (841.0, 1189.0),
            Paper::Letter => (215.9, 279.4),
            Paper::Custom {
                width_mm,
                height_mm,
            } => (width_mm, height_mm),
        }
    }
}

/// How drawing units map to paper.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlotScale {
    /// Fit the plot area into the printable area.
    Fit,
    /// `paper_mm` millimetres on paper = `drawing_units` drawing units (1:100 in a millimetre
    /// drawing is `Ratio { paper_mm: 1.0, drawing_units: 100.0 }`).
    Ratio { paper_mm: f64, drawing_units: f64 },
}

impl PlotScale {
    /// `1:n` for a drawing in millimetres.
    pub fn one_to(n: f64) -> Self {
        PlotScale::Ratio {
            paper_mm: 1.0,
            drawing_units: n,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PageSetup {
    pub paper: Paper,
    pub landscape: bool,
    pub scale: PlotScale,
    /// Margin on all four sides, millimetres.
    pub margin_mm: f64,
    /// Plot everything black.
    pub monochrome: bool,
    /// Plot entity/layer lineweights; otherwise every line is thin (0.13 mm).
    pub line_weights: bool,
    /// Font for text outlines (the app passes its embedded CJK font).
    pub font: Option<Font>,
    /// Area to plot in drawing coordinates; `None` = drawing extents.
    pub window: Option<BBox2>,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            paper: Paper::A4,
            landscape: true,
            scale: PlotScale::Fit,
            margin_mm: 10.0,
            monochrome: false,
            line_weights: true,
            font: None,
            window: None,
        }
    }
}

impl PageSetup {
    /// Page size in millimetres after orientation.
    pub fn page_size_mm(&self) -> (f64, f64) {
        let (w, h) = self.paper.size_mm();
        let (a, b) = (w.min(h), w.max(h));
        if self.landscape { (b, a) } else { (a, b) }
    }
}

/// World → page points.
#[derive(Clone, Copy)]
struct Map {
    k: f64, // points per drawing unit
    world_center: DVec2,
    page_center: DVec2, // points
}

impl Map {
    fn p(&self, w: DVec2) -> DVec2 {
        self.page_center + (w - self.world_center) * self.k
    }
}

fn f(v: f64) -> f32 {
    if v.is_finite() && v.abs() > 1e-9 {
        v as f32
    } else {
        0.0
    }
}

/// Export the model space of `drawing` as a one-page vector PDF.
pub fn export(drawing: &Drawing, setup: &PageSetup) -> Result<Vec<u8>> {
    let (pw, ph) = setup.page_size_mm();
    if !(pw.is_finite() && ph.is_finite() && pw > 1.0 && ph > 1.0 && pw < 20_000.0 && ph < 20_000.0)
    {
        return Err(Error::PageSetup(format!("paper size {pw} × {ph} mm")));
    }
    let margin = setup.margin_mm;
    if !(margin.is_finite() && margin >= 0.0 && 2.0 * margin < pw.min(ph)) {
        return Err(Error::PageSetup(format!("margin {margin} mm")));
    }
    let scene = scene::build(
        drawing,
        SceneOptions {
            foreground: [0, 0, 0],
            monochrome: setup.monochrome,
            plot_only: true,
        },
    );
    let window = setup.window.or(scene.bbox);

    let printable = DVec2::new(pw - 2.0 * margin, ph - 2.0 * margin);
    let mm_per_unit = match setup.scale {
        PlotScale::Ratio {
            paper_mm,
            drawing_units,
        } => {
            if !(paper_mm.is_finite()
                && drawing_units.is_finite()
                && paper_mm > 0.0
                && drawing_units > 0.0)
            {
                return Err(Error::PageSetup(format!(
                    "scale {paper_mm}:{drawing_units}"
                )));
            }
            paper_mm / drawing_units
        }
        PlotScale::Fit => match window {
            Some(b) => {
                let size = b.max - b.min;
                let kx = if size.x > 0.0 {
                    printable.x / size.x
                } else {
                    f64::INFINITY
                };
                let ky = if size.y > 0.0 {
                    printable.y / size.y
                } else {
                    f64::INFINITY
                };
                let k = kx.min(ky);
                if k.is_finite() && k > 0.0 { k } else { 1.0 }
            }
            None => 1.0,
        },
    };
    let map = Map {
        k: mm_per_unit * PT_PER_MM,
        world_center: window.map(|b| (b.min + b.max) * 0.5).unwrap_or(DVec2::ZERO),
        page_center: DVec2::new(pw, ph) * (0.5 * PT_PER_MM),
    };

    let mut c = Content::new();
    // Clip to the printable area.
    c.rect(
        f(margin * PT_PER_MM),
        f(margin * PT_PER_MM),
        f(printable.x * PT_PER_MM),
        f(printable.y * PT_PER_MM),
    );
    c.clip_nonzero();
    c.end_path();
    c.set_line_cap(LineCapStyle::RoundCap);
    c.set_line_join(LineJoinStyle::RoundJoin);

    let mut state = State::default();
    for item in &scene.items {
        match item {
            Item::Stroke { path, style } => {
                let wmm = if setup.line_weights {
                    style.weight_mm.max(0.05)
                } else {
                    0.13
                };
                state.stroke_color(&mut c, style.color);
                state.line_width(&mut c, wmm * PT_PER_MM);
                let dash: Option<Vec<f32>> = style
                    .dash
                    .as_ref()
                    .map(|d| d.iter().map(|v| f((v * map.k).max(0.0))).collect());
                state.dash(&mut c, dash);
                emit_path(&mut c, path, &map);
                c.stroke();
            }
            Item::Fill { path, color } => {
                state.fill_color(&mut c, *color);
                emit_path(&mut c, path, &map);
                c.fill_even_odd();
            }
            Item::Point {
                p,
                color,
                weight_mm,
            } => {
                state.stroke_color(&mut c, *color);
                state.line_width(&mut c, (weight_mm * PT_PER_MM).max(1.0));
                state.dash(&mut c, None);
                let q = map.p(*p);
                c.move_to(f(q.x), f(q.y));
                c.line_to(f(q.x), f(q.y));
                c.stroke();
            }
            Item::Text(t) => {
                state.fill_color(&mut c, t.color);
                let drawn = setup
                    .font
                    .as_ref()
                    .is_some_and(|font| text_outlines(&mut c, font, t, &map));
                if !drawn {
                    text_fallback(&mut c, t, &map);
                }
            }
        }
    }
    let content = c.finish();

    let catalog_id = Ref::new(1);
    let tree_id = Ref::new(2);
    let page_id = Ref::new(3);
    let content_id = Ref::new(4);
    let font_id = Ref::new(5);
    let info_id = Ref::new(6);
    let mut pdf = Pdf::new();
    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id).kids([page_id]).count(1);
    {
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, f(pw * PT_PER_MM), f(ph * PT_PER_MM)));
        page.parent(tree_id);
        page.contents(content_id);
        page.resources().fonts().pair(Name(b"F1"), font_id);
        page.finish();
    }
    pdf.type1_font(font_id)
        .base_font(Name(b"Helvetica"))
        .encoding_predefined(Name(b"WinAnsiEncoding"));
    pdf.document_info(info_id)
        .producer(pdf_writer::TextStr("webcad2026"));
    let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&content, 6);
    pdf.stream(content_id, &compressed)
        .filter(Filter::FlateDecode);
    Ok(pdf.finish())
}

/// Tracks graphics state to avoid redundant operators.
#[derive(Default)]
struct State {
    stroke: Option<[u8; 3]>,
    fill: Option<[u8; 3]>,
    width: Option<f64>,
    dash: Option<Option<Vec<f32>>>,
}

fn rgb(c: [u8; 3]) -> (f32, f32, f32) {
    (
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
    )
}

impl State {
    fn stroke_color(&mut self, c: &mut Content, col: [u8; 3]) {
        if self.stroke != Some(col) {
            let (r, g, b) = rgb(col);
            c.set_stroke_rgb(r, g, b);
            self.stroke = Some(col);
        }
    }
    fn fill_color(&mut self, c: &mut Content, col: [u8; 3]) {
        if self.fill != Some(col) {
            let (r, g, b) = rgb(col);
            c.set_fill_rgb(r, g, b);
            self.fill = Some(col);
        }
    }
    fn line_width(&mut self, c: &mut Content, w: f64) {
        if self.width != Some(w) {
            c.set_line_width(f(w));
            self.width = Some(w);
        }
    }
    fn dash(&mut self, c: &mut Content, d: Option<Vec<f32>>) {
        // Dash arrays with zero total length are invalid in PDF.
        let d = d.filter(|v| v.iter().sum::<f32>() > 0.0);
        if self.dash.as_ref() != Some(&d) {
            match &d {
                Some(v) => c.set_dash_pattern(v.iter().copied(), 0.0),
                None => c.set_dash_pattern(std::iter::empty::<f32>(), 0.0),
            };
            self.dash = Some(d);
        }
    }
}

fn emit_path(c: &mut Content, path: &[Seg], map: &Map) {
    let mut cur = DVec2::ZERO;
    for s in path {
        match *s {
            Seg::Move(p) => {
                let q = map.p(p);
                c.move_to(f(q.x), f(q.y));
                cur = p;
            }
            Seg::Line(p) => {
                let q = map.p(p);
                c.line_to(f(q.x), f(q.y));
                cur = p;
            }
            Seg::Cubic(a, b, p) => {
                let (a, b, q) = (map.p(a), map.p(b), map.p(p));
                c.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(q.x), f(q.y));
                cur = p;
            }
            Seg::Arc {
                c: center,
                u,
                v,
                t0,
                dt,
            } => {
                for (a, b, p) in arc_to_cubics(center, u, v, t0, dt) {
                    let (a, b, q) = (map.p(a), map.p(b), map.p(p));
                    c.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(q.x), f(q.y));
                    cur = p;
                }
            }
            Seg::Close => {
                c.close_path();
            }
        }
    }
    let _ = cur;
}

/// Filled glyph outlines. Returns `false` when the layout produced nothing (placeholder engine or
/// font without the glyphs), so the caller can fall back.
fn text_outlines(c: &mut Content, font: &Font, t: &TextItem, map: &Map) -> bool {
    let spec = TextSpec {
        text: &t.text,
        pos: t.pos,
        height: t.height,
        rotation: t.rotation,
        width_factor: t.width_factor,
        oblique: t.oblique,
        halign: t.halign,
        valign: t.valign,
        line_spacing: t.line_spacing,
        wrap_width: t.wrap_width,
    };
    let out = layout(font, &spec);
    if out.glyphs.iter().all(|g| g.is_empty()) {
        return false;
    }
    for glyph in &out.glyphs {
        if glyph.is_empty() {
            continue;
        }
        let mut cur = DVec2::ZERO;
        for cmd in glyph {
            match *cmd {
                PathCmd::MoveTo(p) => {
                    let q = map.p(p);
                    c.move_to(f(q.x), f(q.y));
                    cur = p;
                }
                PathCmd::LineTo(p) => {
                    let q = map.p(p);
                    c.line_to(f(q.x), f(q.y));
                    cur = p;
                }
                PathCmd::QuadTo(ctrl, p) => {
                    let c1 = cur + (ctrl - cur) * (2.0 / 3.0);
                    let c2 = p + (ctrl - p) * (2.0 / 3.0);
                    let (a, b, q) = (map.p(c1), map.p(c2), map.p(p));
                    c.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(q.x), f(q.y));
                    cur = p;
                }
                PathCmd::CubicTo(c1, c2, p) => {
                    let (a, b, q) = (map.p(c1), map.p(c2), map.p(p));
                    c.cubic_to(f(a.x), f(a.y), f(b.x), f(b.y), f(q.x), f(q.y));
                    cur = p;
                }
                PathCmd::Close => {
                    c.close_path();
                }
            }
        }
        c.fill_nonzero();
    }
    true
}

/// Encode as WinAnsi (Latin-1 subset); other characters become '?'.
fn win_ansi(s: &str) -> Vec<u8> {
    s.chars()
        .map(|ch| match ch as u32 {
            0x20..=0x7E | 0xA0..=0xFF => ch as u32 as u8,
            _ => b'?',
        })
        .collect()
}

/// Helvetica text (cap height 0.718 em) positioned like the outline text.
fn text_fallback(c: &mut Content, t: &TextItem, map: &Map) {
    const CAP: f64 = 0.718;
    let lines = scene::wrap_lines(&t.text, t.height, t.width_factor, t.wrap_width);
    let pitch = t.height * LINE_PITCH * t.line_spacing;
    let h = t.height + pitch * lines.len().saturating_sub(1) as f64;
    let size_pt = t.height * map.k / CAP;
    if !(size_pt.is_finite() && size_pt > 0.01) {
        return;
    }
    let (_, b0) = scene::text_block_offset(t.halign, t.valign, 0.0, h, t.height);
    let dir = DVec2::new(t.rotation.cos(), t.rotation.sin());
    let up = wcad_math::perp(dir);
    let shear = t.oblique.tan();
    // Text space: x axis = width-scaled baseline direction, y axis = sheared up direction.
    let xa = dir * t.width_factor;
    let ya = up + dir * shear;
    c.begin_text();
    c.set_font(Name(b"F1"), f(size_pt));
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bytes = win_ansi(line);
        // Helvetica averages ~0.55 em per character.
        let width = bytes.len() as f64 * 0.55 * t.height / CAP * t.width_factor;
        let ox = match t.halign {
            HAlign::Left => 0.0,
            HAlign::Center => -width * 0.5,
            HAlign::Right => -width,
        };
        let origin = t.pos + dir * ox + up * (b0 - pitch * i as f64);
        let o = map.p(origin);
        c.set_text_matrix([f(xa.x), f(xa.y), f(ya.x), f(ya.y), f(o.x), f(o.y)]);
        c.show(Str(&bytes));
    }
    c.end_text();
}
