//! Localization. Every module owns its strings: a plain struct of `&'static str` fields with one
//! `static` instance per language (missing translations fail to compile). There is deliberately no
//! shared strings file; feature modules add `i18n`-style structs next to their code.
//!
//! Placeholders in format strings are `{0}`, `{1}`, … and are filled with [`fmt`].

pub mod core;

use serde::{Deserialize, Serialize};

pub use self::core::{CoreStrings, core};

/// UI language.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Lang {
    /// Simplified Chinese (default).
    #[default]
    Zh,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Zh, Lang::En];

    /// Name of the language in itself (for the language menu).
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::Zh => "简体中文",
            Lang::En => "English",
        }
    }

    /// Pick the value for this language.
    pub fn pick<T>(self, zh: T, en: T) -> T {
        match self {
            Lang::Zh => zh,
            Lang::En => en,
        }
    }
}

/// Replace `{0}`, `{1}`, … in `template` with `args`.
pub fn fmt(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = template.to_owned();
    for (i, a) in args.iter().enumerate() {
        out = out.replace(&format!("{{{i}}}"), &a.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        assert_eq!(fmt("{0} of {1}", &[&3, &"x"]), "3 of x");
        assert_eq!(fmt("none", &[&1]), "none");
    }
}
