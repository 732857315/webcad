//! `web` / `dist`: wasm build, wasm-bindgen, optional wasm-opt, hashing, templated PWA files.

use crate::cargo::{self, WASM_TARGET};
use crate::{APP_BIN, APP_PKG, root};
use anyhow::{Context as _, Result, bail};
use sha2::{Digest as _, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Size-oriented profile from the workspace Cargo.toml.
const WEB_RELEASE_PROFILE: &str = "web-release";
/// Files in `web/` that are templates, not copied verbatim.
const TEMPLATES: [&str; 2] = ["index.html", "sw.js"];
/// dist/ subdirectory for third-party licenses (not precached by the service worker).
const LICENSES_DIR: &str = "licenses";

pub struct WebOpts {
    /// Release build (profile web-release) instead of dev.
    pub release: bool,
    /// Run wasm-opt when it can be found.
    pub wasm_opt: bool,
    /// Content-hashed js/wasm file names.
    pub hash: bool,
    /// Ship and register the offline service worker.
    pub service_worker: bool,
}

impl WebOpts {
    pub fn dev() -> Self {
        Self {
            release: false,
            wasm_opt: false,
            hash: false,
            service_worker: false,
        }
    }

    pub fn dist(wasm_opt: bool, hash: bool) -> Self {
        Self {
            release: true,
            wasm_opt,
            hash,
            service_worker: true,
        }
    }
}

pub fn build(o: &WebOpts) -> Result<()> {
    let t0 = std::time::Instant::now();
    let root = root();
    let profile = if o.release {
        WEB_RELEASE_PROFILE
    } else {
        "dev"
    };
    let args = [
        "-p",
        APP_PKG,
        "--bin",
        APP_BIN,
        "--target",
        WASM_TARGET,
        "--profile",
        profile,
    ];
    let flags = cargo::rustflags(Some(WASM_TARGET), true)?;
    let wasm_in = cargo::build(&args, flags, APP_BIN)?;
    if wasm_in.extension().and_then(|e| e.to_str()) != Some("wasm") {
        bail!("expected a .wasm artifact, got {}", wasm_in.display());
    }
    eprintln!("raw wasm: {} bytes", file_len(&wasm_in)?);

    // 1. wasm-bindgen (library; its version must equal the app's wasm-bindgen dependency).
    let t1 = std::time::Instant::now();
    let staging = wasm_in
        .parent()
        .context("wasm artifact has no parent directory")?
        .join("xtask-bindgen");
    remove_dir_if_exists(&staging)?;
    let dev = !o.release;
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm_in)
        .out_name(APP_BIN)
        .web(true)?
        .typescript(false)
        .debug(dev)
        .keep_debug(dev)
        .remove_name_section(!dev)
        .remove_producers_section(!dev)
        .generate(&staging)
        .context("wasm-bindgen")?;
    let js_path = staging.join(format!("{APP_BIN}.js"));
    let bg_path = staging.join(format!("{APP_BIN}_bg.wasm"));
    eprintln!(
        "wasm-bindgen: {:.1}s -> {} bytes",
        t1.elapsed().as_secs_f64(),
        file_len(&bg_path)?
    );

    // 2. Optional wasm-opt (CI installs binaryen; locally it is usually absent).
    if o.wasm_opt {
        match find_wasm_opt() {
            Some(exe) => wasm_opt(&exe, &bg_path)?,
            None => eprintln!(
                "note: wasm-opt not found (set WASM_OPT or put binaryen on PATH); skipping"
            ),
        }
    }

    // 3. Output names. Hashed names never go stale behind GitHub Pages' max-age=600 cache.
    let js = fs::read(&js_path).with_context(|| js_path.display().to_string())?;
    let wasm = fs::read(&bg_path).with_context(|| bg_path.display().to_string())?;
    let stem = if o.hash {
        format!("{APP_BIN}-{}", short_hash(&[&js, &wasm]))
    } else {
        APP_BIN.to_owned()
    };
    let js_name = format!("{stem}.js");
    let wasm_name = format!("{stem}_bg.wasm");

    // 4. Fresh dist/.
    let dist = root.join("dist");
    remove_dir_if_exists(&dist)?;
    fs::create_dir_all(&dist).with_context(|| dist.display().to_string())?;
    write(&dist.join(&js_name), &js)?;
    write(&dist.join(&wasm_name), &wasm)?;
    let mut files = vec![js_name.clone(), wasm_name.clone()];
    // JS snippets (`#[wasm_bindgen(inline_js)]` etc.) are imported relative to the glue file.
    let snippets = staging.join("snippets");
    if snippets.is_dir() {
        copy_tree(&snippets, &dist.join("snippets"), "snippets", &mut files)?;
    }
    let web = root.join("web");
    copy_tree(&web, &dist, "", &mut files)?;
    files.retain(|f| !TEMPLATES.contains(&f.as_str()));
    copy_licenses(&root, &dist, &mut files)?;
    files.sort();

    // 5. Templates. The build id covers every shipped byte (and the templates themselves), so any
    // change installs a new service worker.
    let mut id_parts: Vec<Vec<u8>> = Vec::new();
    for f in &files {
        id_parts.push(f.as_bytes().to_vec());
        id_parts.push(fs::read(dist.join(f))?);
    }
    for t in TEMPLATES {
        id_parts.push(fs::read(web.join(t)).with_context(|| format!("web/{t}"))?);
    }
    let id_refs: Vec<&[u8]> = id_parts.iter().map(Vec::as_slice).collect();
    let build_id = short_hash(&id_refs);
    let vars = Vars {
        js: &js_name,
        wasm: &wasm_name,
        build_id: &build_id,
        version: env!("CARGO_PKG_VERSION"),
        service_worker: o.service_worker,
        precache: &precache_list(&files),
    };
    let templates: &[&str] = if o.service_worker {
        &TEMPLATES
    } else {
        &TEMPLATES[..1]
    };
    for t in templates {
        let src = fs::read_to_string(web.join(t)).with_context(|| format!("web/{t}"))?;
        write(&dist.join(t), subst(&src, &vars)?.as_bytes())?;
    }

    eprintln!(
        "dist/ ready in {:.1}s: {js_name} ({} B), {wasm_name} ({} B), build {build_id}{}",
        t0.elapsed().as_secs_f64(),
        js.len(),
        wasm.len(),
        if o.service_worker {
            ", service worker on"
        } else {
            ""
        }
    );
    eprintln!("try it: cargo xtask serve   (http://127.0.0.1:8080/)");
    Ok(())
}

fn file_len(p: &Path) -> Result<u64> {
    Ok(fs::metadata(p)
        .with_context(|| p.display().to_string())?
        .len())
}

fn write(p: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(p, bytes).with_context(|| format!("writing {}", p.display()))
}

fn remove_dir_if_exists(p: &Path) -> Result<()> {
    if p.exists() {
        fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
    }
    Ok(())
}

fn wasm_opt(exe: &Path, wasm: &Path) -> Result<()> {
    let before = file_len(wasm)?;
    let t = std::time::Instant::now();
    let out = wasm.with_extension("opt.wasm");
    cargo::run(
        Command::new(exe)
            .arg(wasm)
            // Enabled features (bulk-memory, sign-ext, reference-types, …) are read from the
            // `target_features` section that rustc emits and wasm-bindgen keeps.
            .args([
                "-Os",
                "--strip-debug",
                "--strip-producers",
                "--strip-target-features",
            ])
            .arg("-o")
            .arg(&out),
    )?;
    fs::rename(&out, wasm).context("replacing wasm with wasm-opt output")?;
    let after = file_len(wasm)?;
    eprintln!(
        "wasm-opt: {:.1}s, {before} -> {after} bytes ({:.1}%)",
        t.elapsed().as_secs_f64(),
        100.0 * after as f64 / before.max(1) as f64
    );
    Ok(())
}

fn find_wasm_opt() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WASM_OPT").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = if cfg!(windows) {
        "wasm-opt.exe"
    } else {
        "wasm-opt"
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

/// 16 hex chars of SHA-256 over length-prefixed parts.
pub fn short_hash(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p);
    }
    h.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Copy a directory tree (skipping `web/` templates at the top level), recording relative paths
/// with `/` separators, prefixed by `prefix`.
fn copy_tree(src: &Path, dst: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    fs::create_dir_all(dst).with_context(|| dst.display().to_string())?;
    let mut entries: Vec<_> = fs::read_dir(src)
        .with_context(|| src.display().to_string())?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue; // editor/OS junk; GitHub Pages artifacts also drop dotfiles
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let p = e.path();
        if p.is_dir() {
            copy_tree(&p, &dst.join(&name), &rel, out)?;
        } else {
            if !(prefix.is_empty() && TEMPLATES.contains(&name.as_str())) {
                fs::copy(&p, dst.join(&name))
                    .with_context(|| format!("copying {}", p.display()))?;
            }
            out.push(rel);
        }
    }
    Ok(())
}

/// Third-party and project licenses shipped with the site.
fn copy_licenses(root: &Path, dist: &Path, out: &mut Vec<String>) -> Result<()> {
    let dir = dist.join(LICENSES_DIR);
    fs::create_dir_all(&dir)?;
    let mut copies = vec![(
        root.join("assets").join("fonts").join("OFL.txt"),
        "NotoSansSC-OFL.txt".to_owned(),
    )];
    for name in ["LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE.md"] {
        let p = root.join(name);
        if p.is_file() {
            copies.push((p, name.to_owned()));
        }
    }
    for (src, name) in copies {
        fs::copy(&src, dir.join(&name)).with_context(|| format!("copying {}", src.display()))?;
        out.push(format!("{LICENSES_DIR}/{name}"));
    }
    Ok(())
}

/// Files the service worker precaches (the page shell is listed in sw.js itself).
fn precache_list(files: &[String]) -> String {
    files
        .iter()
        .filter(|f| !f.starts_with(&format!("{LICENSES_DIR}/")))
        .map(|f| format!("\"./{f}\""))
        .collect::<Vec<_>>()
        .join(",\n  ")
}

struct Vars<'a> {
    js: &'a str,
    wasm: &'a str,
    build_id: &'a str,
    version: &'a str,
    service_worker: bool,
    precache: &'a str,
}

/// Substitute `{{NAME}}` placeholders; unknown placeholders are an error.
fn subst(src: &str, v: &Vars<'_>) -> Result<String> {
    let out = src
        .replace("{{JS}}", v.js)
        .replace("{{WASM}}", v.wasm)
        .replace("{{BUILD_ID}}", v.build_id)
        .replace("{{VERSION}}", v.version)
        .replace(
            "{{SERVICE_WORKER}}",
            if v.service_worker { "true" } else { "false" },
        )
        .replace("{{PRECACHE}}", v.precache);
    if let Some(i) = out.find("{{") {
        let snippet: String = out[i..].chars().take(40).collect();
        bail!("unknown template placeholder near `{snippet}`");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(precache: &'a str) -> Vars<'a> {
        Vars {
            js: "webcad-00.js",
            wasm: "webcad-00_bg.wasm",
            build_id: "abc",
            version: "0.1.0",
            service_worker: true,
            precache,
        }
    }

    #[test]
    fn hash_is_stable_and_length_prefixed() {
        assert_eq!(short_hash(&[b"ab", b"c"]).len(), 16);
        assert_eq!(short_hash(&[b"ab", b"c"]), short_hash(&[b"ab", b"c"]));
        assert_ne!(short_hash(&[b"ab", b"c"]), short_hash(&[b"a", b"bc"]));
    }

    #[test]
    fn substitutes_placeholders() {
        let s = subst(
            "{{JS}} {{WASM}} {{BUILD_ID}} {{VERSION}} {{SERVICE_WORKER}} [{{PRECACHE}}]",
            &vars("\"./a\""),
        )
        .unwrap();
        assert_eq!(s, "webcad-00.js webcad-00_bg.wasm abc 0.1.0 true [\"./a\"]");
        assert!(subst("{{NOPE}}", &vars("")).is_err());
    }

    #[test]
    fn precache_skips_licenses() {
        let files = vec![
            "icons/icon-192.png".to_owned(),
            "licenses/NotoSansSC-OFL.txt".to_owned(),
            "webcad.js".to_owned(),
        ];
        assert_eq!(
            precache_list(&files),
            "\"./icons/icon-192.png\",\n  \"./webcad.js\""
        );
    }

    #[test]
    fn web_templates_have_only_known_placeholders() {
        let web = root().join("web");
        for t in TEMPLATES {
            let src = fs::read_to_string(web.join(t)).unwrap();
            let out = subst(&src, &vars("\"./x.js\"")).unwrap();
            assert!(!out.contains("{{"), "{t}");
        }
        let index = fs::read_to_string(web.join("index.html")).unwrap();
        // GitHub Pages project sites live under /<repo>/: every URL must be relative.
        for attr in ["href=\"/", "src=\"/", "import(\"/", "register(\"/"] {
            assert!(!index.contains(attr), "absolute URL `{attr}` in index.html");
        }
        for id in [
            "id=\"webcad_canvas\"",
            "id=\"loading\"",
            "id=\"error\"",
            "hidden",
        ] {
            assert!(index.contains(id), "index.html lacks {id}");
        }
    }

    #[test]
    fn copy_tree_skips_templates_and_dotfiles() {
        let tmp = std::env::temp_dir().join(format!("xtask-copy-test-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(src.join("icons")).unwrap();
        fs::write(src.join("index.html"), "t").unwrap();
        fs::write(src.join(".DS_Store"), "x").unwrap();
        fs::write(src.join("manifest.webmanifest"), "{}").unwrap();
        fs::write(src.join("icons").join("a.png"), "png").unwrap();
        let mut files = Vec::new();
        copy_tree(&src, &dst, "", &mut files).unwrap();
        assert_eq!(
            files,
            vec!["icons/a.png", "index.html", "manifest.webmanifest"]
        );
        assert!(dst.join("icons").join("a.png").is_file());
        assert!(!dst.join("index.html").exists());
        assert!(!dst.join(".DS_Store").exists());
        let _ = fs::remove_dir_all(&tmp);
    }
}
