//! webcad build tool. Run as `cargo xtask <command>` (alias in `.cargo/config.toml`).
//!
//! Everything here is plain Rust so the same commands work on Windows, Linux, macOS and in CI.

mod cargo;
mod serve;
mod web;

use anyhow::{Context as _, Result, bail};
use std::path::PathBuf;
use std::process::ExitCode;

/// Package and binary of the application.
pub const APP_PKG: &str = "wcad-app";
pub const APP_BIN: &str = "webcad";

const USAGE: &str = "\
webcad build tool

usage: cargo xtask <command> [options]

commands:
  web                          dev wasm build + wasm-bindgen -> dist/ (no service worker)
  dist [--no-opt] [--no-hash]  release web build (profile web-release) -> dist/: wasm-opt if
                               found, content-hashed js/wasm, offline service worker, licenses
  serve [--port N] [--dir D]   serve D (default dist/) on http://127.0.0.1:N/ (default 8080)
  native [--release] [--target TRIPLE]
                               desktop build of the app (bin `webcad`)
  fonts [SRC.ttf]              regenerate the CJK UI font subset (bash + python fontTools)
  help                         show this help

environment:
  WASM_OPT=<path>              wasm-opt used by `dist` (default: looked up on PATH)
  RUSTFLAGS, CARGO_TARGET_DIR  honoured; web builds and `native --release` also get
                               --remap-path-prefix for the workspace and CARGO_HOME
";

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Help,
    Web,
    Dist {
        wasm_opt: bool,
        hash: bool,
    },
    Serve {
        port: u16,
        dir: Option<PathBuf>,
    },
    Native {
        release: bool,
        target: Option<String>,
    },
    Fonts {
        src: Option<String>,
    },
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
    let cmd = match parse(&args) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("error: {e:#}\n\nrun `cargo xtask help` for usage");
            return ExitCode::from(2);
        }
    };
    match run(cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Help => {
            print!("{USAGE}");
            Ok(())
        }
        Cmd::Web => web::build(&web::WebOpts::dev()),
        Cmd::Dist { wasm_opt, hash } => web::build(&web::WebOpts::dist(wasm_opt, hash)),
        Cmd::Serve { port, dir } => serve::serve(&dir.unwrap_or_else(|| root().join("dist")), port),
        Cmd::Native { release, target } => native(release, target.as_deref()),
        Cmd::Fonts { src } => fonts(src.as_deref()),
    }
}

fn parse(args: &[String]) -> Result<Cmd> {
    let Some((cmd, rest)) = args.split_first() else {
        return Ok(Cmd::Help);
    };
    let mut p = OptParser::new(rest);
    let cmd = match cmd.as_str() {
        "help" | "-h" | "--help" => Cmd::Help,
        "web" => Cmd::Web,
        "dist" => Cmd::Dist {
            wasm_opt: !p.flag("--no-opt"),
            hash: !p.flag("--no-hash"),
        },
        "serve" => Cmd::Serve {
            port: match p.value("--port")? {
                Some(v) => v
                    .parse()
                    .with_context(|| format!("invalid --port value `{v}`"))?,
                None => 8080,
            },
            dir: p.value("--dir")?.map(PathBuf::from),
        },
        "native" => Cmd::Native {
            release: p.flag("--release"),
            target: p.value("--target")?,
        },
        "fonts" => Cmd::Fonts {
            src: p.positional(),
        },
        other => bail!("unknown command `{other}`"),
    };
    p.finish()?;
    Ok(cmd)
}

/// Minimal option parser: `--flag`, `--key value`, `--key=value` and positionals.
/// Every argument must be consumed, so typos are reported instead of silently ignored.
struct OptParser {
    args: Vec<Option<String>>,
}

impl OptParser {
    fn new(args: &[String]) -> Self {
        Self {
            args: args.iter().cloned().map(Some).collect(),
        }
    }

    fn flag(&mut self, name: &str) -> bool {
        let mut found = false;
        for a in &mut self.args {
            if a.as_deref() == Some(name) {
                *a = None;
                found = true;
            }
        }
        found
    }

    fn value(&mut self, name: &str) -> Result<Option<String>> {
        let prefix = format!("{name}=");
        for i in 0..self.args.len() {
            let Some(a) = self.args[i].clone() else {
                continue;
            };
            if let Some(v) = a.strip_prefix(&prefix) {
                self.args[i] = None;
                return Ok(Some(v.to_owned()));
            }
            if a == name {
                self.args[i] = None;
                return match self.args.get_mut(i + 1).and_then(Option::take) {
                    Some(v) if !v.starts_with("--") => Ok(Some(v)),
                    _ => bail!("`{name}` needs a value"),
                };
            }
        }
        Ok(None)
    }

    fn positional(&mut self) -> Option<String> {
        self.args
            .iter_mut()
            .find(|a| a.as_deref().is_some_and(|s| !s.starts_with('-')))
            .and_then(Option::take)
    }

    fn finish(self) -> Result<()> {
        let left: Vec<String> = self.args.into_iter().flatten().collect();
        if !left.is_empty() {
            bail!("unexpected argument(s): {}", left.join(" "));
        }
        Ok(())
    }
}

/// Workspace root (the parent of `xtask/`).
pub fn root() -> PathBuf {
    // `cargo run` sets CARGO_MANIFEST_DIR at run time too; prefer it so a moved checkout (portable
    // drive with a different letter) still works with a stale xtask binary.
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    manifest_dir
        .parent()
        .map(PathBuf::from)
        .unwrap_or(manifest_dir)
}

fn native(release: bool, target: Option<&str>) -> Result<()> {
    let mut args = vec!["-p", APP_PKG, "--bin", APP_BIN];
    if release {
        args.push("--release");
    }
    if let Some(t) = target {
        args.extend(["--target", t]);
    }
    // Dev builds keep cargo's normal flags so they share the cache with plain `cargo run`.
    let flags = if release {
        cargo::rustflags(target, true)?
    } else {
        None
    };
    let exe = cargo::build(&args, flags, APP_BIN)?;
    eprintln!("native build ready: {}", exe.display());
    Ok(())
}

fn fonts(src: Option<&str>) -> Result<()> {
    let script = root().join("assets").join("fonts").join("make_subsets.sh");
    let mut c = std::process::Command::new("bash");
    c.arg(&script);
    if let Some(src) = src {
        // Relative paths are resolved by the script after it cds into assets/fonts.
        let abs = std::path::absolute(src).with_context(|| format!("font path {src}"))?;
        c.arg(abs);
    }
    cargo::run(&mut c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_commands() {
        assert_eq!(parse(&args("help")).unwrap(), Cmd::Help);
        assert_eq!(parse(&args("web")).unwrap(), Cmd::Web);
        assert_eq!(
            parse(&args("dist --no-opt")).unwrap(),
            Cmd::Dist {
                wasm_opt: false,
                hash: true
            }
        );
        assert_eq!(
            parse(&args("serve --port 9000 --dir out")).unwrap(),
            Cmd::Serve {
                port: 9000,
                dir: Some(PathBuf::from("out"))
            }
        );
        assert_eq!(
            parse(&args("serve --port=8123")).unwrap(),
            Cmd::Serve {
                port: 8123,
                dir: None
            }
        );
        assert_eq!(
            parse(&args("native --release --target x86_64-apple-darwin")).unwrap(),
            Cmd::Native {
                release: true,
                target: Some("x86_64-apple-darwin".into())
            }
        );
        assert_eq!(
            parse(&args("fonts src.ttf")).unwrap(),
            Cmd::Fonts {
                src: Some("src.ttf".into())
            }
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse(&args("bogus")).is_err());
        assert!(parse(&args("web --release")).is_err());
        assert!(parse(&args("serve --port")).is_err());
        assert!(parse(&args("serve --port --dir x")).is_err());
        assert!(parse(&args("serve --port 99999")).is_err());
        assert!(parse(&args("dist --no-opt extra")).is_err());
    }

    #[test]
    fn root_contains_workspace_manifest() {
        assert!(root().join("Cargo.toml").is_file());
        assert!(root().join("xtask").is_dir());
    }
}
