//! MTEXT/TEXT control codes → plain text for export (paragraphs become `\n`).

/// Replace AutoCAD `%%` codes used in TEXT and MTEXT (`%%c` Ø, `%%d` °, `%%p` ±, `%%%` %,
/// `%%nnn` character code). Underline/overline toggles (`%%u`, `%%o`) are dropped.
pub(crate) fn percent_codes(s: &str) -> String {
    if !s.contains("%%") {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && chars.get(i + 1) == Some(&'%') {
            match chars.get(i + 2).map(|c| c.to_ascii_lowercase()) {
                Some('c') => out.push('\u{00D8}'),
                Some('d') => out.push('\u{00B0}'),
                Some('p') => out.push('\u{00B1}'),
                Some('%') => out.push('%'),
                Some('u') | Some('o') | Some('k') => {}
                Some(d) if d.is_ascii_digit() => {
                    let digits: String = chars[i + 2..]
                        .iter()
                        .take(3)
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Some(c) = digits.parse::<u32>().ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                    i += 2 + digits.len();
                    continue;
                }
                _ => {
                    out.push_str("%%");
                    i += 2;
                    continue;
                }
            }
            i += 3;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// Decode `\U+XXXX` escapes (how pre-2007 DXF stores non-ANSI characters) into characters,
/// leaving every other control code untouched.
pub(crate) fn decode_unicode_escapes(s: &str) -> String {
    if !s.contains("\\U+") && !s.contains("\\u+") {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' {
            match chars.get(i + 1) {
                Some('\\') => {
                    out.push_str("\\\\");
                    i += 2;
                    continue;
                }
                Some('U') | Some('u') if chars.get(i + 2) == Some(&'+') => {
                    let hex: String = chars[i + 3..].iter().take(4).collect();
                    if hex.len() == 4
                        && let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                    {
                        out.push(c);
                        i += 7;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Encode non-ASCII characters as `\U+XXXX` for pre-2007 DXF/DWG, whose strings are stored in a
/// single-byte code page. Characters outside the BMP become `?`.
pub(crate) fn encode_unicode_escapes(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c as u32 {
            0..=0x7F => out.push(c),
            v @ 0x80..=0xFFFF => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\U+{v:04X}");
            }
            _ => out.push('?'),
        }
    }
    out
}

/// Strip MTEXT inline formatting: `\P` → newline, stacked fractions `\Sa^b;` → `a/b`,
/// `\U+XXXX` → char, font/height/color/tracking codes removed, braces removed.
pub(crate) fn mtext_plain(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&code) = chars.get(i + 1) else {
                    break;
                };
                i += 2;
                match code {
                    'P' | 'n' => out.push('\n'),
                    '~' => out.push('\u{00A0}'),
                    '\\' | '{' | '}' => out.push(code),
                    'L' | 'l' | 'O' | 'o' | 'K' | 'k' | 'N' => {}
                    'U' | 'u' if chars.get(i) == Some(&'+') => {
                        let hex: String = chars[i + 1..].iter().take(4).collect();
                        if let Some(ch) =
                            u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            out.push(ch);
                            i += 5;
                        } else {
                            out.push('\\');
                            out.push(code);
                        }
                    }
                    'S' => {
                        // \Snum^den; or \Snum/den; or \Snum#den;
                        let mut j = i;
                        let mut part = String::new();
                        while j < chars.len() && chars[j] != ';' {
                            let ch = chars[j];
                            part.push(match ch {
                                '^' | '#' => '/',
                                _ => ch,
                            });
                            j += 1;
                        }
                        out.push_str(part.trim());
                        i = (j + 1).min(chars.len());
                    }
                    'f' | 'F' | 'H' | 'h' | 'W' | 'w' | 'Q' | 'q' | 'T' | 't' | 'A' | 'a' | 'C'
                    | 'c' | 'p' => {
                        // Argument terminated by ';'.
                        while i < chars.len() && chars[i] != ';' {
                            i += 1;
                        }
                        i = (i + 1).min(chars.len());
                    }
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '{' | '}' => i += 1,
            '\r' => i += 1,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    percent_codes(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_codes() {
        assert_eq!(mtext_plain("line1\\Pline2 中文"), "line1\nline2 中文");
        assert_eq!(
            mtext_plain("{\\fArial|b0|i0;\\H2.5x;Big} \\C1;red"),
            "Big red"
        );
        assert_eq!(mtext_plain("\\S1^2; in"), "1/2 in");
        assert_eq!(mtext_plain("a\\U+4E2Db"), "a中b");
        assert_eq!(mtext_plain("50%%d %%c10 %%p0.1"), "50° Ø10 ±0.1");
        assert_eq!(percent_codes("100%"), "100%");
        assert_eq!(mtext_plain("trailing\\"), "trailing");
        assert_eq!(
            decode_unicode_escapes("a\\U+4E2D\\U+6587b \\\\U+4E2D \\P"),
            "a中文b \\\\U+4E2D \\P"
        );
        assert_eq!(encode_unicode_escapes("Ø中a"), "\\U+00D8\\U+4E2Da");
        assert_eq!(
            decode_unicode_escapes(&encode_unicode_escapes("图层 A°")),
            "图层 A°"
        );
    }
}
