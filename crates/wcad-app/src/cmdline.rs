//! Command-line grammar (AutoCAD style), independent of the UI:
//!
//! | input      | meaning                                               |
//! |------------|-------------------------------------------------------|
//! | `x,y`      | absolute point (`x,y,z` accepted, z ignored)          |
//! | `@dx,dy`   | relative to the last point (`@` alone = last point)   |
//! | `@d<a`     | polar, relative to the last point (degrees)           |
//! | `d<a`      | polar from the origin                                 |
//! | `<a`       | angle override for the next point (degrees)           |
//! | `12.5`     | a number: value, or direct distance along the cursor  |
//! | keyword    | option of the active tool (shortcut letters or name)  |
//! | name/alias | a command (`L`, `LINE`, …)                            |
//!
//! Full-width punctuation typed with a Chinese IME (`，＜＠．－`) is accepted.

use wcad_math::DVec2;

use crate::commands::CommandRegistry;
use crate::tools::Keyword;

/// A typed point, not yet resolved against the last point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PointInput {
    Abs(DVec2),
    /// Offset from the last point.
    Rel(DVec2),
    /// `dist` at `angle_deg` (CCW from +X), from the last point (`relative`) or the origin.
    Polar {
        dist: f64,
        angle_deg: f64,
        relative: bool,
    },
}

impl PointInput {
    /// Resolve to a world point. Relative input without a last point is taken from the origin.
    pub fn resolve(self, last: Option<DVec2>) -> DVec2 {
        let base = last.unwrap_or(DVec2::ZERO);
        match self {
            PointInput::Abs(p) => p,
            PointInput::Rel(d) => base + d,
            PointInput::Polar {
                dist,
                angle_deg,
                relative,
            } => {
                let a = angle_deg.to_radians();
                let v = DVec2::new(a.cos(), a.sin()) * dist;
                if relative { base + v } else { v }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// Empty input (Enter / Space).
    Empty,
    Point(PointInput),
    /// `<a`: lock the direction of the next point to this angle (degrees).
    AngleLock(f64),
    /// A bare number.
    Number(f64),
    /// A keyword of the active tool (its `id`).
    Keyword(&'static str),
    /// A registered command (canonical name).
    Command(&'static str),
    /// Free text (tools that accept text).
    Text(String),
    /// Nothing matched; the original input.
    Unknown(String),
}

/// What the parser needs to know about the current state.
pub struct ParseCx<'a> {
    pub keywords: &'a [Keyword],
    /// The active tool accepts free text: anything that is not a keyword becomes [`Parsed::Text`].
    pub accepts_text: bool,
    pub registry: Option<&'a CommandRegistry>,
}

fn normalize(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '，' => ',',
            '＜' | '《' => '<',
            '＠' => '@',
            '．' | '。' => '.',
            '－' => '-',
            '＋' => '+',
            '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// Parse a finite number (rejects `inf`, `NaN`, empty).
pub fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty()
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E'))
    {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_xy(s: &str) -> Option<DVec2> {
    let parts: Vec<&str> = s.split(',').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let x = parse_number(parts[0])?;
    let y = parse_number(parts[1])?;
    if parts.len() == 3 {
        parse_number(parts[2])?;
    }
    Some(DVec2::new(x, y))
}

fn parse_polar(s: &str) -> Option<(f64, f64)> {
    let (d, a) = s.split_once('<')?;
    Some((parse_number(d)?, parse_number(a)?))
}

/// Match `input` against keywords: shortcut key, id, localized label, or an id prefix at least as
/// long as the shortcut (case-insensitive).
pub fn match_keyword(input: &str, keywords: &[Keyword]) -> Option<&'static str> {
    let t = input.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_lowercase();
    keywords
        .iter()
        .find(|k| k.key.eq_ignore_ascii_case(t) || k.id.eq_ignore_ascii_case(t) || k.label == t)
        .or_else(|| {
            keywords
                .iter()
                .find(|k| t.len() >= k.key.len() && k.id.to_lowercase().starts_with(&lower))
        })
        .map(|k| k.id)
}

pub fn parse(input: &str, cx: &ParseCx<'_>) -> Parsed {
    let raw = input.trim();
    if raw.is_empty() {
        return Parsed::Empty;
    }
    if let Some(k) = match_keyword(raw, cx.keywords) {
        return Parsed::Keyword(k);
    }
    if cx.accepts_text {
        return Parsed::Text(raw.to_owned());
    }
    let s: String = normalize(raw)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if let Some(rest) = s.strip_prefix('@') {
        if rest.is_empty() {
            return Parsed::Point(PointInput::Rel(DVec2::ZERO));
        }
        if let Some(d) = parse_xy(rest) {
            return Parsed::Point(PointInput::Rel(d));
        }
        if let Some((dist, angle_deg)) = parse_polar(rest) {
            return Parsed::Point(PointInput::Polar {
                dist,
                angle_deg,
                relative: true,
            });
        }
        return Parsed::Unknown(raw.to_owned());
    }
    if let Some(rest) = s.strip_prefix('<') {
        return match parse_number(rest) {
            Some(a) => Parsed::AngleLock(a),
            None => Parsed::Unknown(raw.to_owned()),
        };
    }
    if let Some(p) = parse_xy(&s) {
        return Parsed::Point(PointInput::Abs(p));
    }
    if let Some((dist, angle_deg)) = parse_polar(&s) {
        return Parsed::Point(PointInput::Polar {
            dist,
            angle_deg,
            relative: false,
        });
    }
    if let Some(v) = parse_number(&s) {
        return Parsed::Number(v);
    }
    if let Some(c) = cx.registry.and_then(|r| r.find(&s)) {
        return Parsed::Command(c.name);
    }
    Parsed::Unknown(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KW: &[Keyword] = &[
        Keyword {
            id: "Undo",
            key: "U",
            label: "放弃",
        },
        Keyword {
            id: "Close",
            key: "C",
            label: "闭合",
        },
    ];

    fn p(s: &str) -> Parsed {
        let reg = CommandRegistry::with_all_modules();
        parse(
            s,
            &ParseCx {
                keywords: KW,
                accepts_text: false,
                registry: Some(&reg),
            },
        )
    }

    #[test]
    fn coordinates() {
        assert_eq!(
            p("10,20"),
            Parsed::Point(PointInput::Abs(DVec2::new(10.0, 20.0)))
        );
        assert_eq!(
            p(" -1.5 , 2e1 "),
            Parsed::Point(PointInput::Abs(DVec2::new(-1.5, 20.0)))
        );
        assert_eq!(
            p("1,2,3"),
            Parsed::Point(PointInput::Abs(DVec2::new(1.0, 2.0)))
        );
        assert_eq!(
            p("@5,-5"),
            Parsed::Point(PointInput::Rel(DVec2::new(5.0, -5.0)))
        );
        assert_eq!(p("@"), Parsed::Point(PointInput::Rel(DVec2::ZERO)));
        assert_eq!(
            p("@10<90"),
            Parsed::Point(PointInput::Polar {
                dist: 10.0,
                angle_deg: 90.0,
                relative: true
            })
        );
        assert_eq!(
            p("10<45"),
            Parsed::Point(PointInput::Polar {
                dist: 10.0,
                angle_deg: 45.0,
                relative: false
            })
        );
        assert_eq!(p("<30"), Parsed::AngleLock(30.0));
        // Full-width input from a Chinese IME.
        assert_eq!(
            p("１０，２０"),
            Parsed::Point(PointInput::Abs(DVec2::new(10.0, 20.0)))
        );
        assert_eq!(
            p("＠3＜0"),
            Parsed::Point(PointInput::Polar {
                dist: 3.0,
                angle_deg: 0.0,
                relative: true
            })
        );
    }

    #[test]
    fn numbers_keywords_commands() {
        assert_eq!(p("12.5"), Parsed::Number(12.5));
        assert_eq!(p("-.5"), Parsed::Number(-0.5));
        assert_eq!(p("u"), Parsed::Keyword("Undo"));
        assert_eq!(p("CLOSE"), Parsed::Keyword("Close"));
        assert_eq!(p("cl"), Parsed::Keyword("Close"));
        assert_eq!(p("闭合"), Parsed::Keyword("Close"));
        assert_eq!(p("l"), Parsed::Command("LINE"));
        assert_eq!(p("line"), Parsed::Command("LINE"));
        assert_eq!(p(""), Parsed::Empty);
        assert_eq!(p("   "), Parsed::Empty);
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "inf", "nan", "1,", ",", "@x,y", "<", "1<", "abc,def", "1e999", "@1<",
        ] {
            assert!(matches!(p(s), Parsed::Unknown(_)), "{s} -> {:?}", p(s));
        }
    }

    #[test]
    fn text_mode() {
        let cx = ParseCx {
            keywords: &[],
            accepts_text: true,
            registry: None,
        };
        assert_eq!(parse("Hello 世界", &cx), Parsed::Text("Hello 世界".into()));
        assert_eq!(parse("", &cx), Parsed::Empty);
    }

    #[test]
    fn resolve_points() {
        let last = Some(DVec2::new(1.0, 1.0));
        assert_eq!(
            PointInput::Rel(DVec2::new(2.0, 3.0)).resolve(last),
            DVec2::new(3.0, 4.0)
        );
        let q = PointInput::Polar {
            dist: 2.0,
            angle_deg: 90.0,
            relative: true,
        }
        .resolve(last);
        assert!((q - DVec2::new(1.0, 3.0)).length() < 1e-12);
        assert_eq!(PointInput::Abs(DVec2::X).resolve(None), DVec2::X);
        assert_eq!(PointInput::Rel(DVec2::X).resolve(None), DVec2::X);
    }
}
