//! CAD text as geometry: TTF/OTF glyph outlines laid out for TEXT/MTEXT entities.
//!
//! CONTRACT (render and export depend on these types): implemented by the geom2d work package.
//! Font bytes are supplied by the caller (the app embeds the Noto Sans SC subset).
//!
//! Layout rules (AutoCAD conventions):
//! - `height` is the cap height; glyphs are scaled by `height / capital_height`.
//! - `width_factor` stretches along the baseline, `oblique` slants (radians, from vertical),
//!   `rotation` rotates the whole block around `pos`.
//! - Lines advance by `5/3 · height · line_spacing`.
//! - `halign` aligns each line at `pos.x`; `valign` places the block: `Baseline` = first baseline,
//!   `Top` = cap top of the first line, `Middle` = middle between that and the last baseline,
//!   `Bottom` = descender of the last line.
//! - `wrap_width` breaks lines greedily at spaces and between CJK characters (with basic
//!   no-break-before/after punctuation rules); over-long words are broken per character.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use ttf_parser::{Face, GlyphId, OutlineBuilder};
use wcad_math::{BBox2, DVec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VAlign {
    #[default]
    Baseline,
    Bottom,
    Middle,
    Top,
}

/// A parsed font. Cheap to clone.
#[derive(Clone)]
pub struct Font {
    data: Arc<[u8]>,
    index: u32,
}

impl Font {
    /// Parse a TTF/OTF (or a face of a TTC). Fails on WOFF/WOFF2 and invalid data.
    pub fn from_bytes(data: Arc<[u8]>, index: u32) -> crate::Result<Self> {
        ttf_parser::Face::parse(&data, index).map_err(|e| crate::Error::Font(e.to_string()))?;
        Ok(Self { data, index })
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    /// `true` if the font has a glyph for `c`.
    pub fn has_glyph(&self, c: char) -> bool {
        Face::parse(&self.data, self.index)
            .ok()
            .and_then(|f| f.glyph_index(c))
            .is_some()
    }
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("bytes", &self.data.len())
            .field("index", &self.index)
            .finish()
    }
}

/// What to lay out. Units are drawing units; `height` is the cap height (AutoCAD convention).
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec<'a> {
    pub text: &'a str,
    pub pos: DVec2,
    pub height: f64,
    pub rotation: f64,
    pub width_factor: f64,
    pub oblique: f64,
    pub halign: HAlign,
    pub valign: VAlign,
    /// Line spacing factor for multi-line text (`\n` or MTEXT `\P` already converted to `\n`).
    pub line_spacing: f64,
    /// Wrap width for MTEXT; `None` = no wrapping.
    pub wrap_width: Option<f64>,
}

impl<'a> TextSpec<'a> {
    /// Left/baseline aligned, unrotated single-style text.
    pub fn simple(text: &'a str, pos: DVec2, height: f64) -> Self {
        Self {
            text,
            pos,
            height,
            rotation: 0.0,
            width_factor: 1.0,
            oblique: 0.0,
            halign: HAlign::Left,
            valign: VAlign::Baseline,
            line_spacing: 1.0,
            wrap_width: None,
        }
    }
}

/// Outline path command in drawing coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    MoveTo(DVec2),
    LineTo(DVec2),
    QuadTo(DVec2, DVec2),
    CubicTo(DVec2, DVec2, DVec2),
    Close,
}

/// Laid-out text: filled outlines (non-zero rule), one path list per glyph.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextOutline {
    pub glyphs: Vec<Vec<PathCmd>>,
    /// Bounds of the outline points (empty when there are no visible glyphs).
    pub bbox: BBox2,
    /// Characters with no glyph in the font (for lazy fallback-font loading).
    pub missing: Vec<char>,
    /// Corners of the layout box (lines × advance widths, descender to ascender), counter-clockwise
    /// from bottom-left, in drawing coordinates (rotation applied). Useful for picking and grips.
    pub frame: [DVec2; 4],
    /// Number of laid-out lines (after wrapping).
    pub lines: usize,
}

/// Local → drawing transform of the text block.
struct Xf {
    pos: DVec2,
    cos: f64,
    sin: f64,
}

impl Xf {
    fn apply(&self, p: DVec2) -> DVec2 {
        self.pos
            + DVec2::new(
                p.x * self.cos - p.y * self.sin,
                p.x * self.sin + p.y * self.cos,
            )
    }
}

struct Collector<'x> {
    xf: &'x Xf,
    /// font units → local: x' = pen + (x·s·wf) + (y·s)·tan(oblique), y' = base + y·s
    s: f64,
    wf: f64,
    tan_ob: f64,
    pen: f64,
    base: f64,
    cmds: Vec<PathCmd>,
    bbox: BBox2,
}

impl Collector<'_> {
    fn map(&mut self, x: f32, y: f32) -> DVec2 {
        let (x, y) = (x as f64 * self.s, y as f64 * self.s);
        let p = self.xf.apply(DVec2::new(
            self.pen + x * self.wf + y * self.tan_ob,
            self.base + y,
        ));
        self.bbox.include(p);
        p
    }
}

impl OutlineBuilder for Collector<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.cmds.push(PathCmd::MoveTo(p));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.cmds.push(PathCmd::LineTo(p));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let a = self.map(x1, y1);
        let p = self.map(x, y);
        self.cmds.push(PathCmd::QuadTo(a, p));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let a = self.map(x1, y1);
        let b = self.map(x2, y2);
        let p = self.map(x, y);
        self.cmds.push(PathCmd::CubicTo(a, b, p));
    }
    fn close(&mut self) {
        self.cmds.push(PathCmd::Close);
    }
}

/// A shaped character: glyph id and advance (local units, width factor applied).
#[derive(Clone, Copy)]
struct Shaped {
    ch: char,
    gid: GlyphId,
    adv: f64,
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x2FF0..=0x303F | 0x3040..=0x30FF | 0x3100..=0x31FF |
        0x3200..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF |
        0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// Characters that must not start a line.
fn no_break_before(c: char) -> bool {
    matches!(
        c,
        ',' | '.'
            | ';'
            | ':'
            | '?'
            | '!'
            | ')'
            | ']'
            | '}'
            | '%'
            | '，'
            | '。'
            | '、'
            | '；'
            | '：'
            | '？'
            | '！'
            | '）'
            | '」'
            | '』'
            | '】'
            | '》'
            | '〉'
            | '”'
            | '’'
            | '…'
            | 'ー'
            | '〕'
            | '］'
            | '｝'
    )
}

/// Characters that must not end a line.
fn no_break_after(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | '{' | '（' | '「' | '『' | '【' | '《' | '〈' | '“' | '‘' | '〔' | '［' | '｛'
    )
}

/// Break opportunity before `chars[i]` (i > 0).
fn can_break_before(chars: &[Shaped], i: usize) -> bool {
    let (p, c) = (chars[i - 1].ch, chars[i].ch);
    if no_break_before(c) || no_break_after(p) {
        return false;
    }
    p.is_whitespace() || is_cjk(p) || is_cjk(c)
}

fn line_width(line: &[Shaped]) -> f64 {
    let end = line
        .iter()
        .rposition(|s| !s.ch.is_whitespace())
        .map(|i| i + 1)
        .unwrap_or(0);
    line[..end].iter().map(|s| s.adv).sum()
}

/// Greedy wrapping of one paragraph.
fn wrap(chars: &[Shaped], width: Option<f64>) -> Vec<Vec<Shaped>> {
    let Some(w) = width.filter(|w| *w > 0.0 && w.is_finite()) else {
        return vec![chars.to_vec()];
    };
    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < chars.len() {
        let mut acc = 0.0;
        let mut last_break: Option<usize> = None;
        let mut end = chars.len();
        let mut i = start;
        while i < chars.len() {
            if i > start && can_break_before(chars, i) {
                last_break = Some(i);
            }
            let c = chars[i];
            if acc + c.adv > w && i > start && !c.ch.is_whitespace() {
                end = match last_break {
                    Some(b) if b > start => b,
                    _ => i,
                };
                break;
            }
            acc += c.adv;
            i += 1;
        }
        lines.push(chars[start..end].to_vec());
        // skip leading spaces of the next line
        start = end;
        while start < chars.len() && chars[start].ch == ' ' {
            start += 1;
        }
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Lay out `spec` with `font`.
pub fn layout(font: &Font, spec: &TextSpec<'_>) -> TextOutline {
    let mut out = TextOutline {
        frame: [spec.pos; 4],
        ..Default::default()
    };
    let Ok(face) = Face::parse(&font.data, font.index) else {
        out.missing = spec.text.chars().filter(|c| !c.is_control()).collect();
        return out;
    };
    if !(spec.height > 0.0) || !spec.height.is_finite() || !spec.pos.is_finite() {
        return out;
    }
    let upem = face.units_per_em().max(1) as f64;
    let cap = face
        .capital_height()
        .filter(|c| *c > 0)
        .map(|c| c as f64)
        .or_else(|| {
            face.glyph_index('H')
                .and_then(|g| face.glyph_bounding_box(g))
                .map(|r| r.y_max as f64)
                .filter(|h| *h > 0.0)
        })
        .unwrap_or(upem * 0.7);
    let s = spec.height / cap;
    let wf = if spec.width_factor > 0.0 && spec.width_factor.is_finite() {
        spec.width_factor
    } else {
        1.0
    };
    let ob = if spec.oblique.is_finite() {
        spec.oblique.clamp(-1.4, 1.4)
    } else {
        0.0
    };
    let tan_ob = ob.tan();
    let ls = if spec.line_spacing > 0.0 && spec.line_spacing.is_finite() {
        spec.line_spacing
    } else {
        1.0
    };
    let adv_line = 5.0 / 3.0 * spec.height * ls;
    let rot = if spec.rotation.is_finite() {
        spec.rotation
    } else {
        0.0
    };
    let xf = Xf {
        pos: spec.pos,
        cos: rot.cos(),
        sin: rot.sin(),
    };
    let kern = face.tables().kern;
    let space_adv = face
        .glyph_index(' ')
        .and_then(|g| face.glyph_hor_advance(g))
        .map(|a| a as f64)
        .unwrap_or(upem * 0.25);

    // shape paragraphs
    let mut missing: Vec<char> = Vec::new();
    let mut lines: Vec<Vec<Shaped>> = Vec::new();
    for para in spec.text.split('\n') {
        let mut shaped: Vec<Shaped> = Vec::new();
        for ch in para.chars() {
            if ch == '\t' {
                for _ in 0..4 {
                    shaped.push(Shaped {
                        ch: ' ',
                        gid: face.glyph_index(' ').unwrap_or(GlyphId(0)),
                        adv: space_adv * s * wf,
                    });
                }
                continue;
            }
            if ch.is_control() {
                continue;
            }
            let gid = match face.glyph_index(ch) {
                Some(g) => g,
                None => {
                    if !missing.contains(&ch) {
                        missing.push(ch);
                    }
                    GlyphId(0)
                }
            };
            let adv = face
                .glyph_hor_advance(gid)
                .map(|a| a as f64)
                .unwrap_or(upem * 0.5)
                * s
                * wf;
            // pair kerning with the previous glyph
            if let (Some(k), Some(prev)) = (kern, shaped.last_mut()) {
                let kv = k
                    .subtables
                    .into_iter()
                    .filter(|t| t.horizontal && !t.variable)
                    .find_map(|t| t.glyphs_kerning(prev.gid, gid));
                if let Some(kv) = kv {
                    prev.adv += kv as f64 * s * wf;
                }
            }
            shaped.push(Shaped { ch, gid, adv });
        }
        lines.extend(wrap(&shaped, spec.wrap_width));
    }
    let n = lines.len().max(1);
    let widths: Vec<f64> = lines.iter().map(|l| line_width(l)).collect();
    let desc = face.descender() as f64 * s;
    let asc = face.ascender() as f64 * s;
    let last_base = -((n - 1) as f64) * adv_line;
    let y_shift = match spec.valign {
        VAlign::Baseline => 0.0,
        VAlign::Top => -spec.height,
        VAlign::Middle => -0.5 * (spec.height + last_base),
        VAlign::Bottom => -(last_base + desc),
    };
    let mut bbox = BBox2::EMPTY;
    let mut xmin = f64::INFINITY;
    let mut xmax = f64::NEG_INFINITY;
    for (li, line) in lines.iter().enumerate() {
        let w = widths[li];
        let x0 = match spec.halign {
            HAlign::Left => 0.0,
            HAlign::Center => -0.5 * w,
            HAlign::Right => -w,
        };
        xmin = xmin.min(x0);
        xmax = xmax.max(x0 + w);
        let base = y_shift - li as f64 * adv_line;
        let mut pen = x0;
        for g in line {
            if !g.ch.is_whitespace() {
                let mut c = Collector {
                    xf: &xf,
                    s,
                    wf,
                    tan_ob,
                    pen,
                    base,
                    cmds: Vec::new(),
                    bbox: BBox2::EMPTY,
                };
                face.outline_glyph(g.gid, &mut c);
                if !c.cmds.is_empty() {
                    bbox = bbox.union(&c.bbox);
                    out.glyphs.push(c.cmds);
                }
            }
            pen += g.adv;
        }
    }
    if !xmin.is_finite() {
        xmin = 0.0;
        xmax = 0.0;
    }
    let top = y_shift + asc;
    let bottom = y_shift + last_base + desc;
    out.frame = [
        xf.apply(DVec2::new(xmin, bottom)),
        xf.apply(DVec2::new(xmax, bottom)),
        xf.apply(DVec2::new(xmax, top)),
        xf.apply(DVec2::new(xmin, top)),
    ];
    out.bbox = bbox;
    out.missing = missing;
    out.lines = n;
    out
}

/// Convert AutoCAD TEXT/MTEXT markup to plain text for [`layout`]: `\P` → newline, `\~` → no-break
/// space, `\U+XXXX` → character, `\S a^b;` stacks → `a/b`, formatting codes with arguments
/// (`\H…;`, `\f…;`, `\C…;`, `\A…;`, `\Q…;`, `\T…;`, `\W…;`, `\p…;`) and toggles
/// (`\L \l \O \o \K \k`) removed, braces dropped, escaped `\\ \{ \}` kept, and the `%%c`/`%%d`/
/// `%%p`/`%%%` specials replaced by `Ø`/`°`/`±`/`%`.
pub fn mtext_to_plain(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() => {
                let k = chars[i + 1];
                i += 2;
                match k {
                    'P' | 'X' => out.push('\n'),
                    '~' => out.push('\u{a0}'),
                    '\\' | '{' | '}' => out.push(k),
                    'L' | 'l' | 'O' | 'o' | 'K' | 'k' => {}
                    'U' | 'u' if i < chars.len() && chars[i] == '+' => {
                        let hex: String = chars[i + 1..].iter().take(4).collect();
                        match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            Some(ch) if hex.len() == 4 => {
                                out.push(ch);
                                i += 5;
                            }
                            _ => out.push(k),
                        }
                    }
                    'S' => {
                        while i < chars.len() && chars[i] != ';' {
                            let ch = chars[i];
                            out.push(if ch == '^' || ch == '#' { '/' } else { ch });
                            i += 1;
                        }
                        i += 1;
                    }
                    'A' | 'C' | 'c' | 'F' | 'f' | 'H' | 'h' | 'Q' | 'q' | 'T' | 't' | 'W' | 'w'
                    | 'p' => {
                        while i < chars.len() && chars[i] != ';' {
                            i += 1;
                        }
                        i += 1;
                    }
                    other => out.push(other),
                }
            }
            '{' | '}' => i += 1,
            '%' if i + 2 < chars.len() && chars[i + 1] == '%' => {
                let rep = match chars[i + 2].to_ascii_lowercase() {
                    'c' => Some('Ø'),
                    'd' => Some('°'),
                    'p' => Some('±'),
                    '%' => Some('%'),
                    _ => None,
                };
                match rep {
                    Some(r) => {
                        out.push(r);
                        i += 3;
                    }
                    None => {
                        out.push('%');
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Advance width of a single line of text (no wrapping), in drawing units.
pub fn measure(font: &Font, text: &str, height: f64, width_factor: f64) -> f64 {
    let spec = TextSpec {
        width_factor,
        ..TextSpec::simple(text, DVec2::ZERO, height)
    };
    let o = layout(font, &spec);
    o.frame[1].x - o.frame[0].x
}

#[cfg(test)]
mod tests {
    use super::*;

    static FONT: &[u8] = include_bytes!("../../../assets/fonts/NotoSansSC-Regular-ui.ttf");

    fn font() -> Font {
        Font::from_bytes(Arc::from(FONT), 0).unwrap()
    }

    #[test]
    fn cap_height_and_width_factor() {
        let f = font();
        let o = layout(&f, &TextSpec::simple("H", DVec2::new(10.0, 20.0), 5.0));
        assert_eq!(o.glyphs.len(), 1);
        assert!((o.bbox.min.y - 20.0).abs() < 0.01, "{:?}", o.bbox);
        assert!((o.bbox.max.y - 25.0).abs() < 0.05, "{:?}", o.bbox);
        let w1 = o.bbox.size().x;
        let o2 = layout(
            &f,
            &TextSpec {
                width_factor: 2.0,
                ..TextSpec::simple("H", DVec2::new(10.0, 20.0), 5.0)
            },
        );
        assert!((o2.bbox.size().x - 2.0 * w1).abs() < 1e-6);
        // oblique slants to the right at the top
        let o3 = layout(
            &f,
            &TextSpec {
                oblique: 15f64.to_radians(),
                ..TextSpec::simple("H", DVec2::ZERO, 5.0)
            },
        );
        assert!(o3.bbox.max.x > o.bbox.max.x - 10.0 + 1.0);
        assert!(Font::from_bytes(Arc::from(&b"not a font"[..]), 0).is_err());
    }

    #[test]
    fn alignment_rotation_and_lines() {
        let f = font();
        let c = layout(
            &f,
            &TextSpec {
                halign: HAlign::Center,
                valign: VAlign::Middle,
                ..TextSpec::simple("HHHH", DVec2::ZERO, 10.0)
            },
        );
        assert!(c.bbox.center().x.abs() < 0.5, "{:?}", c.bbox);
        assert!(c.bbox.center().y.abs() < 0.2, "{:?}", c.bbox);
        let r = layout(
            &f,
            &TextSpec {
                halign: HAlign::Right,
                ..TextSpec::simple("HH", DVec2::ZERO, 10.0)
            },
        );
        assert!(r.bbox.max.x <= 0.01 && r.bbox.max.x > -2.0);
        let t = layout(
            &f,
            &TextSpec {
                valign: VAlign::Top,
                ..TextSpec::simple("H", DVec2::ZERO, 10.0)
            },
        );
        assert!(t.bbox.max.y.abs() < 0.1);
        let rot = layout(
            &f,
            &TextSpec {
                rotation: std::f64::consts::FRAC_PI_2,
                ..TextSpec::simple("HHH", DVec2::ZERO, 10.0)
            },
        );
        assert!(rot.bbox.size().y > rot.bbox.size().x);
        assert!(rot.bbox.max.x < 0.1); // text goes up, glyph tops point to -x
        let two = layout(&f, &TextSpec::simple("H\nH", DVec2::ZERO, 10.0));
        assert_eq!(two.lines, 2);
        assert!(
            (two.bbox.min.y + 10.0 * 5.0 / 3.0).abs() < 0.1,
            "{:?}",
            two.bbox
        );
        let bottom = layout(
            &f,
            &TextSpec {
                valign: VAlign::Bottom,
                ..TextSpec::simple("Hg", DVec2::ZERO, 10.0)
            },
        );
        assert!(
            bottom.bbox.min.y > -0.5 && bottom.bbox.min.y < 1.0,
            "{:?}",
            bottom.bbox
        );
        assert!(bottom.frame[0].y.abs() < 1e-9);
    }

    #[test]
    fn mtext_markup() {
        assert_eq!(mtext_to_plain("A\\PB"), "A\nB");
        assert_eq!(
            mtext_to_plain("{\\H2.5x;\\fArial|b0;Big} %%c10 %%d %%p0.1 100%%%"),
            "Big Ø10 ° ±0.1 100%"
        );
        assert_eq!(
            mtext_to_plain("\\S1^2; \\U+4E2D \\\\ \\{x\\}"),
            "1/2 中 \\ {x}"
        );
        assert_eq!(mtext_to_plain("\\Lunder\\l\\C1;red"), "underred");
        assert_eq!(mtext_to_plain("trailing\\"), "trailing\\");
    }

    #[test]
    fn cjk_wrap_and_missing() {
        let f = font();
        let s = "中文标注测试文字";
        let o = layout(&f, &TextSpec::simple(s, DVec2::ZERO, 10.0));
        assert!(o.missing.is_empty(), "{:?}", o.missing);
        assert_eq!(o.glyphs.len(), 8);
        let w = measure(&f, s, 10.0, 1.0);
        let wrapped = layout(
            &f,
            &TextSpec {
                wrap_width: Some(w * 0.3),
                ..TextSpec::simple(s, DVec2::ZERO, 10.0)
            },
        );
        assert!(wrapped.lines >= 3, "{}", wrapped.lines);
        assert!(wrapped.bbox.size().x <= w * 0.3 + 1e-6);
        // latin words break at spaces
        let latin = layout(
            &f,
            &TextSpec {
                wrap_width: Some(measure(&f, "HELLO W", 10.0, 1.0)),
                ..TextSpec::simple("HELLO WORLD AGAIN", DVec2::ZERO, 10.0)
            },
        );
        assert_eq!(latin.lines, 3);
        // no line starts with a full-width comma
        let p = layout(
            &f,
            &TextSpec {
                wrap_width: Some(measure(&f, "中文", 10.0, 1.0) + 0.1),
                ..TextSpec::simple("中文，标注", DVec2::ZERO, 10.0)
            },
        );
        assert!(p.lines >= 2);
        let m = layout(
            &f,
            &TextSpec::simple("A\u{1F600}B\u{1F600}", DVec2::ZERO, 10.0),
        );
        assert_eq!(m.missing, vec!['\u{1F600}']);
        // degenerate specs do not panic
        let _ = layout(&f, &TextSpec::simple("", DVec2::ZERO, 10.0));
        let _ = layout(&f, &TextSpec::simple("x", DVec2::ZERO, 0.0));
        let _ = layout(
            &f,
            &TextSpec {
                wrap_width: Some(0.001),
                ..TextSpec::simple("abc 中文", DVec2::ZERO, 1.0)
            },
        );
        let _ = layout(
            &f,
            &TextSpec {
                width_factor: f64::NAN,
                oblique: f64::INFINITY,
                ..TextSpec::simple("x", DVec2::ZERO, 1.0)
            },
        );
    }
}
