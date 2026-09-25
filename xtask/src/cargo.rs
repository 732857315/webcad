//! Running cargo: artifact discovery via JSON messages and RUSTFLAGS computed at run time.

use anyhow::{Context as _, Result, bail};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Separator of `CARGO_ENCODED_RUSTFLAGS` (lets flags contain spaces, e.g. in paths).
const SEP: char = '\x1f';

/// Run a command, echoing it first; fails on a non-zero exit status.
pub fn run(c: &mut Command) -> Result<()> {
    eprintln!("+ {}", describe(c));
    let st = c
        .status()
        .with_context(|| format!("failed to start {}", describe(c)))?;
    if !st.success() {
        bail!("command failed ({st}): {}", describe(c));
    }
    Ok(())
}

fn describe(c: &Command) -> String {
    std::iter::once(c.get_program())
        .chain(c.get_args())
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `cargo build <args>`, returning the path of the `bin` artifact.
///
/// `rustflags`: `Some(flags)` replaces the compiler flags for this build (passed as
/// `CARGO_ENCODED_RUSTFLAGS`); `None` keeps cargo's own resolution.
pub fn build(args: &[&str], rustflags: Option<Vec<String>>, bin: &str) -> Result<PathBuf> {
    let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.arg("build")
        .arg("--message-format=json-render-diagnostics")
        .args(args)
        .stdout(Stdio::piped());
    if let Some(flags) = rustflags {
        c.env_remove("RUSTFLAGS")
            .env("CARGO_ENCODED_RUSTFLAGS", flags.join(&SEP.to_string()));
    }
    eprintln!("+ {}", describe(&c));
    let t0 = std::time::Instant::now();
    let mut child = c.spawn().context("failed to start cargo")?;
    let mut artifact = None;
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines() {
            let line = line.context("reading cargo output")?;
            if let Some(p) = bin_artifact(&line, bin) {
                artifact = Some(p);
            }
        }
    }
    let st = child.wait().context("waiting for cargo")?;
    if !st.success() {
        bail!("cargo build failed ({st})");
    }
    let artifact = artifact.with_context(|| format!("cargo did not report the `{bin}` binary"))?;
    eprintln!(
        "cargo build: {:.1}s -> {}",
        t0.elapsed().as_secs_f64(),
        artifact.display()
    );
    Ok(artifact)
}

/// Parse one line of `--message-format=json` output; returns the file of the `bin` target `bin`.
fn bin_artifact(line: &str, bin: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["reason"] != "compiler-artifact" || v["target"]["name"] != bin {
        return None;
    }
    let is_bin = v["target"]["kind"]
        .as_array()?
        .iter()
        .any(|k| k.as_str() == Some("bin"));
    if !is_bin {
        return None;
    }
    if let Some(exe) = v["executable"].as_str() {
        return Some(PathBuf::from(exe));
    }
    v["filenames"]
        .as_array()?
        .iter()
        .filter_map(|f| f.as_str())
        .find(|f| !f.ends_with(".pdb") && !f.ends_with(".d"))
        .map(PathBuf::from)
}

/// Compiler flags for a build: whatever cargo would have used, plus `--remap-path-prefix` for the
/// workspace root and CARGO_HOME (so shipped binaries do not contain local absolute paths) when
/// `remap` is set. Returns `None` when there is nothing to change.
///
/// Setting RUSTFLAGS makes cargo ignore `target.<triple>.rustflags` from `.cargo/config.toml`,
/// so those are read here and kept (e.g. the getrandom backend cfg for wasm32).
pub fn rustflags(target: Option<&str>, remap: bool) -> Result<Option<Vec<String>>> {
    if !remap {
        return Ok(None);
    }
    let mut flags = match env_rustflags() {
        Some(f) => f,
        None => match target {
            Some(t) => {
                let cfg_path = crate::root().join(".cargo").join("config.toml");
                match std::fs::read_to_string(&cfg_path) {
                    Ok(text) => config_target_rustflags(&text, t),
                    Err(_) => Vec::new(),
                }
            }
            None => Vec::new(),
        },
    };
    flags.extend(remap_flags(&crate::root(), cargo_home().as_deref()));
    Ok(Some(flags))
}

/// RUSTFLAGS as cargo would read them from the environment.
fn env_rustflags() -> Option<Vec<String>> {
    if let Ok(enc) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        return Some(
            enc.split(SEP)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
        );
    }
    std::env::var("RUSTFLAGS")
        .ok()
        .map(|s| s.split_whitespace().map(str::to_owned).collect())
}

fn cargo_home() -> Option<PathBuf> {
    if let Some(h) = std::env::var_os("CARGO_HOME") {
        return Some(PathBuf::from(h));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".cargo"))
}

/// `--remap-path-prefix` flags. rustc applies the *last* matching mapping, so the more specific
/// prefix (CARGO_HOME, which may live inside the workspace on some setups) comes last.
fn remap_flags(root: &Path, cargo_home: Option<&Path>) -> Vec<String> {
    let mut out = vec![format!("--remap-path-prefix={}=/webcad", root.display())];
    if let Some(h) = cargo_home {
        out.push(format!("--remap-path-prefix={}=/cargo", h.display()));
    }
    out
}

/// Extract `rustflags` of `[target.<triple>]` from a `.cargo/config.toml` (strings in `'…'` or
/// `"…"`, single- or multi-line array). Small on purpose: the file is ours and simple.
fn config_target_rustflags(text: &str, triple: &str) -> Vec<String> {
    let header = format!("[target.{triple}]");
    let quoted_header = format!("[target.\"{triple}\"]");
    let mut in_section = false;
    let mut collecting = false;
    let mut array = String::new();
    for line in text.lines() {
        let t = line.trim();
        if !collecting && t.starts_with('[') {
            in_section = t == header || t == quoted_header;
            continue;
        }
        if !in_section {
            continue;
        }
        if !collecting {
            let Some(rest) = t.strip_prefix("rustflags") else {
                continue;
            };
            let Some(rest) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            collecting = true;
            array.push_str(rest);
        } else {
            array.push(' ');
            array.push_str(t);
        }
        if array_closed(&array) {
            break;
        }
    }
    parse_string_array(&array)
}

/// True once the TOML array text contains its closing `]` outside of strings.
fn array_closed(s: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in s.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if q == '"' && ch == '\\' {
                    escaped = true;
                } else if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '\'' | '"' => quote = Some(ch),
                ']' => return true,
                '#' => return false,
                _ => {}
            },
        }
    }
    false
}

fn parse_string_array(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => out.push(chars.by_ref().take_while(|&c| c != '\'').collect()),
            '"' => {
                let mut v = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => v.push('\n'),
                            Some('t') => v.push('\t'),
                            Some(other) => v.push(other),
                            None => break,
                        },
                        c => v.push(c),
                    }
                }
                out.push(v);
            }
            ']' => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_target_rustflags_from_config() {
        let cfg = r#"
[alias]
xtask = "run --package xtask --"

[target.wasm32-unknown-unknown]
# comment
rustflags = ['--cfg', 'getrandom_backend="wasm_js"']

[target.x86_64-pc-windows-gnu]
rustflags = [
  "-C", "target-cpu=native", # trailing comment
  "--cfg", "x=\"y\"",
]
"#;
        assert_eq!(
            config_target_rustflags(cfg, WASM_TARGET),
            vec!["--cfg", r#"getrandom_backend="wasm_js""#]
        );
        assert_eq!(
            config_target_rustflags(cfg, "x86_64-pc-windows-gnu"),
            vec!["-C", "target-cpu=native", "--cfg", r#"x="y""#]
        );
        assert!(config_target_rustflags(cfg, "aarch64-apple-darwin").is_empty());
    }

    #[test]
    fn workspace_config_keeps_getrandom_backend() {
        let text = std::fs::read_to_string(crate::root().join(".cargo").join("config.toml"))
            .expect("workspace .cargo/config.toml");
        let flags = config_target_rustflags(&text, WASM_TARGET);
        assert!(
            flags.iter().any(|f| f.contains("getrandom_backend")),
            "{flags:?}"
        );
    }

    #[test]
    fn remap_flags_cover_root_and_cargo_home() {
        let f = remap_flags(Path::new("/src/webcad"), Some(Path::new("/home/u/.cargo")));
        assert_eq!(
            f,
            vec![
                "--remap-path-prefix=/src/webcad=/webcad",
                "--remap-path-prefix=/home/u/.cargo=/cargo"
            ]
        );
    }

    #[test]
    fn finds_bin_artifact_in_cargo_json() {
        let wasm = r#"{"reason":"compiler-artifact","package_id":"x","target":{"kind":["bin"],"crate_types":["bin"],"name":"webcad","src_path":"s"},"filenames":["/t/wasm32-unknown-unknown/debug/webcad.wasm"],"executable":"/t/wasm32-unknown-unknown/debug/webcad.wasm","fresh":true}"#;
        assert_eq!(
            bin_artifact(wasm, "webcad"),
            Some(PathBuf::from("/t/wasm32-unknown-unknown/debug/webcad.wasm"))
        );
        let lib = r#"{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"webcad"},"filenames":["/t/libwebcad.rlib"],"executable":null}"#;
        assert_eq!(bin_artifact(lib, "webcad"), None);
        let other = r#"{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"xtask"},"filenames":["/t/xtask"],"executable":"/t/xtask"}"#;
        assert_eq!(bin_artifact(other, "webcad"), None);
        assert_eq!(bin_artifact("not json", "webcad"), None);
        assert_eq!(
            bin_artifact(r#"{"reason":"build-finished","success":true}"#, "webcad"),
            None
        );
    }
}
