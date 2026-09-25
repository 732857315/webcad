//! `serve`: tiny static file server for dist/, bound to 127.0.0.1 only.

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

pub fn serve(dir: &Path, port: u16) -> Result<()> {
    if !dir.join("index.html").is_file() {
        anyhow::bail!(
            "{} has no index.html (run `cargo xtask web` or `cargo xtask dist` first)",
            dir.display()
        );
    }
    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| {
        anyhow::anyhow!("cannot listen on 127.0.0.1:{port}: {e} (try --port <other>)")
    })?;
    eprintln!(
        "serving {} at http://127.0.0.1:{port}/  (Ctrl+C to stop)",
        dir.display()
    );
    for req in server.incoming_requests() {
        let url = req.url().to_owned();
        let resp = match resolve(dir, &url).map(|p| (fs::read(&p), p)) {
            Some((Ok(bytes), path)) => with_headers(
                tiny_http::Response::from_data(bytes),
                &[
                    ("Content-Type", mime(&path)),
                    ("Cache-Control", "no-store"),
                    ("X-Content-Type-Options", "nosniff"),
                ],
            ),
            _ => with_headers(
                tiny_http::Response::from_data(b"404 not found".to_vec()).with_status_code(404),
                &[("Content-Type", "text/plain; charset=utf-8")],
            ),
        };
        eprintln!("{} {url} -> {}", req.method(), resp.status_code().0);
        if let Err(e) = req.respond(resp) {
            eprintln!("  (client went away: {e})");
        }
    }
    Ok(())
}

fn with_headers<R: std::io::Read>(
    mut resp: tiny_http::Response<R>,
    headers: &[(&str, &str)],
) -> tiny_http::Response<R> {
    for (k, v) in headers {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            resp.add_header(h);
        }
    }
    resp
}

/// Map a request URL to a file below `dir`. Only plain path segments are accepted (no `..`,
/// drive letters, backslashes or percent-escapes), so nothing outside `dir` can be reached.
fn resolve(dir: &Path, url: &str) -> Option<PathBuf> {
    let path = url.split(['?', '#']).next().unwrap_or("/");
    let mut rel = path.strip_prefix('/')?.to_owned();
    if rel.is_empty() || rel.ends_with('/') {
        rel.push_str("index.html");
    }
    let mut out = dir.to_path_buf();
    for seg in rel.split('/') {
        let bad =
            seg.is_empty() || seg == "." || seg == ".." || seg.contains(['\\', ':', '%', '\0']);
        if bad {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

fn mime(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "css" => "text/css; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_plain_paths() {
        let d = Path::new("dist");
        assert_eq!(resolve(d, "/"), Some(d.join("index.html")));
        assert_eq!(resolve(d, "/?x=1#y"), Some(d.join("index.html")));
        assert_eq!(
            resolve(d, "/icons/icon-192.png"),
            Some(d.join("icons").join("icon-192.png"))
        );
        assert_eq!(
            resolve(d, "/icons/"),
            Some(d.join("icons").join("index.html"))
        );
    }

    #[test]
    fn rejects_traversal() {
        let d = Path::new("dist");
        for url in [
            "/../Cargo.toml",
            "/icons/../../x",
            "/./index.html",
            "/..%2fCargo.toml",
            "/C:/Windows/win.ini",
            "/a\\..\\b",
            "//etc/passwd",
            "relative",
        ] {
            assert_eq!(resolve(d, url), None, "{url}");
        }
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime(Path::new("a_bg.wasm")), "application/wasm");
        assert_eq!(
            mime(Path::new("webcad-0123.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            mime(Path::new("manifest.webmanifest")),
            "application/manifest+json"
        );
        assert_eq!(mime(Path::new("INDEX.HTML")), "text/html; charset=utf-8");
        assert_eq!(mime(Path::new("noext")), "application/octet-stream");
    }
}
