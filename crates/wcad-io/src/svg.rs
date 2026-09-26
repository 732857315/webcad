//! Hand-written SVG export.
//!
//! World coordinates (Y up) are emitted unchanged inside a `scale(1,-1)` group, so arcs keep their
//! natural orientation (SVG sweep flag 1 = counter-clockwise in world space). Text elements undo the
//! flip locally so glyphs stay upright.

use std::f64::consts::{PI, TAU};
use std::fmt::Write as _;

use wcad_doc::{Drawing, HAlign};
use wcad_math::cross2;

use crate::geom::{Seg, arc_point, ellipse_axes};
use crate::scene::{self, Item, LINE_PITCH, SceneOptions, TextItem};

/// Font stack used for `<text>` elements (the app embeds Noto Sans SC).
pub const FONT_FAMILY: &str = "Noto Sans SC, sans-serif";

#[derive(Clone, Debug, PartialEq)]
pub struct SvgOptions {
    /// Background color; `None` = transparent. Also decides the color of ACI 7 (white on dark
    /// backgrounds, black otherwise).
    pub background: Option<[u8; 3]>,
    /// Draw everything in the foreground color.
    pub monochrome: bool,
    /// Multiplier for stroke widths (1.0 = default: a 0.25 mm line is 1/1000 of the drawing size).
    pub stroke_scale: f64,
    /// Scale stroke widths by entity/layer lineweights; otherwise every stroke is thin.
    pub lineweights: bool,
    /// Empty border around the drawing, as a fraction of its larger side.
    pub margin: f64,
    /// Width/height attributes: the larger side in pixels.
    pub size_px: f64,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            background: None,
            monochrome: false,
            stroke_scale: 1.0,
            lineweights: true,
            margin: 0.02,
            size_px: 1000.0,
        }
    }
}

/// Format a number compactly (≤ 6 decimals, no trailing zeros, never `-0`).
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let mut s = format!("{v:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".into() } else { s }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 forbids most control characters.
            c if (c as u32) < 0x20 && c != '\t' => {}
            c => out.push(c),
        }
    }
    out
}

/// SVG path data for a segment list.
fn path_data(path: &[Seg]) -> String {
    let mut d = String::new();
    let mut cur = wcad_math::DVec2::ZERO;
    let mut start = cur;
    for s in path {
        match *s {
            Seg::Move(p) => {
                let _ = write!(d, "M{} {}", num(p.x), num(p.y));
                cur = p;
                start = p;
            }
            Seg::Line(p) => {
                let _ = write!(d, "L{} {}", num(p.x), num(p.y));
                cur = p;
            }
            Seg::Cubic(a, b, p) => {
                let _ = write!(
                    d,
                    "C{} {} {} {} {} {}",
                    num(a.x),
                    num(a.y),
                    num(b.x),
                    num(b.y),
                    num(p.x),
                    num(p.y)
                );
                cur = p;
            }
            Seg::Arc { c, u, v, t0, dt } => {
                let (rx, ry, rot) = ellipse_axes(u, v);
                let end = arc_point(c, u, v, t0 + dt);
                if !(ry > rx * 1e-9) || !(rx > 0.0) {
                    let _ = write!(d, "L{} {}", num(end.x), num(end.y));
                    cur = end;
                    continue;
                }
                let sweep = if (dt > 0.0) == (cross2(u, v) > 0.0) {
                    1
                } else {
                    0
                };
                // A full turn cannot be one arc command: split into halves.
                let pieces = if dt.abs() >= TAU - 1e-9 { 2 } else { 1 };
                for i in 1..=pieces {
                    let t = t0 + dt * i as f64 / pieces as f64;
                    let p = arc_point(c, u, v, t);
                    let large = if (dt.abs() / pieces as f64) > PI + 1e-12 {
                        1
                    } else {
                        0
                    };
                    let _ = write!(
                        d,
                        "A{} {} {} {} {} {} {}",
                        num(rx),
                        num(ry),
                        num(rot.to_degrees()),
                        large,
                        sweep,
                        num(p.x),
                        num(p.y)
                    );
                }
                cur = end;
            }
            Seg::Close => {
                d.push('Z');
                cur = start;
            }
        }
    }
    let _ = cur;
    d
}

/// Export the model space of `drawing` as a standalone SVG document.
pub fn export(drawing: &Drawing, opts: &SvgOptions) -> String {
    let foreground = match opts.background {
        Some([r, g, b]) if (0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64) < 128.0 => {
            [255, 255, 255]
        }
        _ => [0, 0, 0],
    };
    let scene = scene::build(
        drawing,
        SceneOptions {
            foreground,
            monochrome: opts.monochrome,
            plot_only: false,
        },
    );
    let (minx, miny, w, h) = match scene.bbox {
        Some(b) => {
            let size = b.max - b.min;
            (b.min.x, b.min.y, size.x, size.y)
        }
        None => (0.0, 0.0, 100.0, 100.0),
    };
    let extent = w.max(h).max(1e-9);
    let margin = extent
        * if opts.margin.is_finite() {
            opts.margin.clamp(0.0, 1.0)
        } else {
            0.02
        };
    let (vx, vy, vw, vh) = (
        minx - margin,
        -(miny + h + margin),
        w + 2.0 * margin,
        h + 2.0 * margin,
    );
    let (vw, vh) = (vw.max(extent * 1e-6), vh.max(extent * 1e-6));
    let size_px = if opts.size_px.is_finite() && opts.size_px > 0.0 {
        opts.size_px
    } else {
        1000.0
    };
    let k = size_px / vw.max(vh);
    let stroke_scale = if opts.stroke_scale.is_finite() && opts.stroke_scale > 0.0 {
        opts.stroke_scale
    } else {
        1.0
    };
    let thin = extent / 1000.0 * stroke_scale;

    let mut s = String::with_capacity(1024 + scene.items.len() * 96);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" version=\"1.1\" width=\"{}\" height=\"{}\" viewBox=\"{} {} {} {}\">",
        num(vw * k),
        num(vh * k),
        num(vx),
        num(vy),
        num(vw),
        num(vh)
    );
    if let Some(bg) = opts.background {
        let _ = writeln!(
            s,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
            num(vx),
            num(vy),
            num(vw),
            num(vh),
            hex(bg)
        );
    }
    s.push_str("<g transform=\"scale(1 -1)\" fill=\"none\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n");
    for item in &scene.items {
        match item {
            Item::Stroke { path, style } => {
                let wmm = if opts.lineweights {
                    style.weight_mm
                } else {
                    scene::DEFAULT_WEIGHT_MM
                };
                let width = thin * (wmm / scene::DEFAULT_WEIGHT_MM).max(0.4);
                let _ = write!(
                    s,
                    "<path d=\"{}\" stroke=\"{}\" stroke-width=\"{}\"",
                    path_data(path),
                    hex(style.color),
                    num(width)
                );
                if let Some(dash) = &style.dash {
                    let list: Vec<String> = dash.iter().map(|v| num(*v)).collect();
                    let _ = write!(s, " stroke-dasharray=\"{}\"", list.join(" "));
                }
                s.push_str("/>\n");
            }
            Item::Fill { path, color } => {
                let _ = writeln!(
                    s,
                    "<path d=\"{}\" fill=\"{}\" fill-rule=\"evenodd\" stroke=\"none\"/>",
                    path_data(path),
                    hex(*color)
                );
            }
            Item::Point { p, color, .. } => {
                let _ = writeln!(
                    s,
                    "<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill=\"{}\" stroke=\"none\"/>",
                    num(p.x),
                    num(p.y),
                    num(thin * 1.5),
                    hex(*color)
                );
            }
            Item::Text(t) => write_text(&mut s, t),
        }
    }
    s.push_str("</g>\n</svg>\n");
    s
}

fn write_text(s: &mut String, t: &TextItem) {
    let lines = scene::wrap_lines(&t.text, t.height, t.width_factor, t.wrap_width);
    let pitch = t.height * LINE_PITCH * t.line_spacing;
    let h = t.height + pitch * lines.len().saturating_sub(1) as f64;
    let (_, b0) = scene::text_block_offset(t.halign, t.valign, 0.0, h, t.height);
    let anchor = match t.halign {
        HAlign::Left => "start",
        HAlign::Center => "middle",
        HAlign::Right => "end",
    };
    let mut transform = format!(
        "translate({} {}) scale(1 -1) rotate({})",
        num(t.pos.x),
        num(t.pos.y),
        num(-t.rotation.to_degrees())
    );
    if t.oblique.abs() > 1e-9 {
        let _ = write!(transform, " skewX({})", num(-t.oblique.to_degrees()));
    }
    if (t.width_factor - 1.0).abs() > 1e-9 {
        let _ = write!(transform, " scale({} 1)", num(t.width_factor));
    }
    let _ = write!(
        s,
        "<text transform=\"{transform}\" font-family=\"{FONT_FAMILY}\" font-size=\"{}\" text-anchor=\"{anchor}\" fill=\"{}\" stroke=\"none\" xml:space=\"preserve\">",
        num(t.height / 0.72),
        hex(t.color)
    );
    for (i, line) in lines.iter().enumerate() {
        // Local y points down after the flip; the first baseline is b0 above the anchor.
        let y = -b0 + pitch * i as f64;
        let _ = write!(
            s,
            "<tspan x=\"0\" y=\"{}\">{}</tspan>",
            num(y),
            escape(line)
        );
    }
    s.push_str("</text>\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_escaping() {
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(-0.0000001), "0");
        assert_eq!(num(10.0), "10");
        assert_eq!(escape("a<b & \"c\""), "a&lt;b &amp; &quot;c&quot;");
    }

    #[test]
    fn arc_flags() {
        use wcad_math::DVec2;
        // Quarter circle CCW from (1,0) to (0,1).
        let p = vec![
            Seg::Move(DVec2::new(1.0, 0.0)),
            Seg::Arc {
                c: DVec2::ZERO,
                u: DVec2::X,
                v: DVec2::Y,
                t0: 0.0,
                dt: std::f64::consts::FRAC_PI_2,
            },
        ];
        assert_eq!(path_data(&p), "M1 0A1 1 0 0 1 0 1");
        // Three quarters clockwise: large arc, sweep 0.
        let p = vec![
            Seg::Move(DVec2::new(1.0, 0.0)),
            Seg::Arc {
                c: DVec2::ZERO,
                u: DVec2::X,
                v: DVec2::Y,
                t0: 0.0,
                dt: -1.5 * PI,
            },
        ];
        assert_eq!(path_data(&p), "M1 0A1 1 0 1 0 0 1");
    }
}
