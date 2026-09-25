# Building webcad

This document covers the build tool (`cargo xtask`), the web and desktop builds, deployment to
GitHub Pages, CI, and the embedded font. The architecture itself is in [ARCHITECTURE.md](ARCHITECTURE.md).

## 1. Toolchain

- Rust **stable ≥ 1.95** with the `wasm32-unknown-unknown` target:
  ```sh
  rustup target add wasm32-unknown-unknown
  ```
- Nothing else is required: wasm-bindgen runs as a library inside `xtask` (no `wasm-bindgen-cli`,
  no `trunk`, no Node.js). `wasm-opt` (binaryen) is optional and only used by `cargo xtask dist`.
- Linux desktop builds need `pkg-config libwayland-dev libxkbcommon-dev` (Debian/Ubuntu names).
  X11, Vulkan and OpenGL are loaded at run time.

### Portable toolchain (development machine)

The development machine keeps Rust on a portable drive under `MoveCoding/Rust` (host
`x86_64-pc-windows-gnu`, GNU binutils from `MoveCoding/MSYS2/ucrt64/bin`). Before any cargo command:

```sh
. tools/env.sh          # Git Bash
. .\tools\env.ps1       # PowerShell
```

The scripts derive every path from their own location or `PORTABLE_ROOT`; no drive letters are
hard-coded and the system environment is not modified. On other machines (and in CI) cargo on
`PATH` is used as is — do not source these scripts there.

## 2. `cargo xtask`

`xtask/` is a normal workspace binary; `.cargo/config.toml` defines the alias
`xtask = "run --package xtask --"`. `cargo xtask help` prints:

| Command | What it does |
|---|---|
| `web` | Dev wasm build (`--profile dev`) of bin `webcad` (package `wcad-app`), wasm-bindgen (`--target web`, debug info kept) → `dist/webcad.js`, `dist/webcad_bg.wasm`, `index.html`, manifest, icons, licenses. No service worker (an old one on the same origin is unregistered). |
| `dist [--no-opt] [--no-hash]` | Release web build (profile `web-release`: opt-level `s`, fat LTO, 1 codegen unit, `panic = "abort"`), wasm-bindgen with name/producers sections stripped, `wasm-opt -Os` if found, content-hashed names `webcad-<hash16>.js` / `webcad-<hash16>_bg.wasm`, templated `index.html` and `sw.js` (per-build cache name `webcad-<build id>`), `manifest.webmanifest`, icons, `licenses/NotoSansSC-OFL.txt`. |
| `serve [--port N] [--dir D]` | Static server for `dist/` (or `D`) on `http://127.0.0.1:N/` (default 8080). Localhost only, `Cache-Control: no-store`, correct MIME types (`application/wasm`, `text/javascript`, `application/manifest+json`, …), path traversal rejected. |
| `native [--release] [--target T]` | Desktop build of bin `webcad`. `cargo run` works as well. |
| `fonts [SRC.ttf]` | Runs `assets/fonts/make_subsets.sh` (see §6). |

Details:

- **Artifact discovery**: the tool runs `cargo build --message-format=json-render-diagnostics` and
  takes the artifact path from cargo's JSON messages, so `CARGO_TARGET_DIR` / `build.target-dir`
  are honoured automatically.
- **Path remapping**: web builds and `native --release` pass
  `--remap-path-prefix=<workspace>=/webcad` and `--remap-path-prefix=<CARGO_HOME>=/cargo`
  (computed at run time, handed to cargo as `CARGO_ENCODED_RUSTFLAGS`), so shipped binaries do not
  contain local absolute paths (panic locations, debug info). Because an explicit RUSTFLAGS makes
  cargo ignore `target.<triple>.rustflags` from `.cargo/config.toml`, xtask reads that section
  itself and keeps its flags (the wasm32 `getrandom_backend="wasm_js"` cfg). If `RUSTFLAGS` or
  `CARGO_ENCODED_RUSTFLAGS` is already set (e.g. `-D warnings` in CI), those are used as the base
  instead, exactly like cargo would. Dev native builds use cargo's normal flags so they share the
  build cache with `cargo run`.
- **wasm-opt**: `dist` uses `$WASM_OPT` or `wasm-opt` on `PATH`. Flags: `-Os --strip-debug
  --strip-producers --strip-target-features`; enabled wasm features are taken from the
  `target_features` section rustc emits. Without wasm-opt the build still succeeds (a note is
  printed). Locally binaryen is usually absent (github.com downloads are very slow on the dev
  network); CI installs it (§4).
- **wasm-bindgen versions**: the app's `wasm-bindgen` and xtask's `wasm-bindgen-cli-support` are both
  pinned to the same exact version in the root `Cargo.toml`; bump them together.
- **Templates**: `web/index.html` and `web/sw.js` contain `{{JS}}`, `{{WASM}}`, `{{BUILD_ID}}`,
  `{{VERSION}}`, `{{SERVICE_WORKER}}`, `{{PRECACHE}}`. An unknown placeholder fails the build. Every
  other file under `web/` is copied verbatim (dotfiles skipped).

### Typical loop

```sh
cargo xtask web && cargo xtask serve      # open http://127.0.0.1:8080/
cargo xtask dist && cargo xtask serve     # check the release build, service worker, offline mode
```

Debug builds only: opening `index.html#webcad-panic-test` panics right after start-up, to check the
in-page crash box.

### Headless smoke test

`xtask/scripts/cdp_shot.py` (Python stdlib only) starts headless Chrome with a DevTools port, loads a
URL, prints console output, saves a screenshot through CDP and reports the page state (`#loading`
removed, `#error` hidden, canvas size, service worker in control):

```sh
cargo xtask dist && cargo xtask serve --port 8765 &
python xtask/scripts/cdp_shot.py <path/to/chrome> <throwaway-profile-dir> http://127.0.0.1:8765/ shot.png 20 \
    --use-angle=swiftshader --enable-unsafe-swiftshader
```

Without a GPU, WebGPU reports no adapter and wgpu falls back to WebGL2 on SwiftShader, which is what
the flags enable. Re-running with the server stopped and the same profile checks offline start-up
from the service worker cache. Do not use Chrome's own `--screenshot` flag (it hangs with a live
canvas), and never run `chrome.exe --version` on Windows (it opens a browser window).

The generated JS glue has no default `new URL('…_bg.wasm', import.meta.url)`; `index.html` passes the
(hashed) wasm URL explicitly. All URLs in `index.html`, `sw.js` and the manifest are relative, so the
site works both at `http://127.0.0.1:8080/` and under a GitHub Pages project path
`https://<user>.github.io/<repo>/`.

## 3. The web page

- `index.html` is a loader only: it imports the glue module and calls `init()`. The Rust `main`
  (bin `webcad`, which wasm-bindgen turns into the start function) calls `wcad_app::web::start()`:
  it installs a panic hook (console + page), starts `eframe::WebRunner` on
  `<canvas id="webcad_canvas">` and removes `#loading` once running.
- Failures are shown in the page: the loader reveals the hidden `#error` box when the module cannot
  be loaded or instantiated (no WebAssembly, 404, …); the wasm panic hook writes the panic message
  into `#error_text` and reveals the box. On wasm a panic aborts the app, so the box offers a reload.
- Rendering uses wgpu: WebGPU where available, otherwise WebGL2 automatically.
- Mobile: `viewport` with `viewport-fit=cover`, `theme-color`, `mobile-web-app-capable` /
  `apple-mobile-web-app-capable`, and `touch-action: none` on the canvas so the app receives pan and
  pinch gestures.
- Offline: `sw.js` precaches the shell, js/wasm, manifest and icons into a cache named after the
  build id. Navigations are network-first (new deploys are picked up), everything else cache-first
  (file names are content-hashed). Old `webcad-*` caches are deleted on activation.

## 4. GitHub Pages

One-time setup in the repository: **Settings → Pages → Build and deployment → Source: GitHub Actions**.
For a private repository, the account's GitHub plan must support Pages for private repositories.

`.github/workflows/pages.yml` runs on pushes to `main` (and manually):

1. `dtolnay/rust-toolchain@stable` with the `wasm32-unknown-unknown` target, `Swatinem/rust-cache@v2`.
2. Downloads binaryen `version_133` (`x86_64-linux` tarball), verifies its pinned SHA-256 and exports
   `WASM_OPT`.
3. `cargo xtask dist`, then `actions/configure-pages`, `actions/upload-pages-artifact@v5`
   (`path: dist/`) and `actions/deploy-pages@v5` in a separate job with `pages: write` and
   `id-token: write`, environment `github-pages`.

To bump binaryen: change `BINARYEN_VERSION` and `BINARYEN_SHA256` (the value is in the release asset
`binaryen-version_<N>-x86_64-linux.tar.gz.sha256`).

GitHub Pages serves `.wasm` as `application/wasm` with gzip and `Cache-Control: max-age=600`; it
cannot send COOP/COEP headers, which is why the web build is single-threaded.

## 5. CI and releases

- `ci.yml` (push to `main`, pull requests): `cargo fmt --check`; `cargo clippy --workspace
  --all-targets -- -D warnings` and `cargo test --workspace` on Ubuntu, Windows and macOS (Linux gets
  the wayland/xkbcommon headers and Mesa lavapipe for wgpu tests); `cargo clippy -p wcad-app --target
  wasm32-unknown-unknown -- -D warnings`; a `cargo xtask web` smoke build.
- `release.yml` (tags `v*`, or manually with a tag): `cargo xtask native --release --target <T>` for
  `x86_64-pc-windows-msvc` (zip), `x86_64-unknown-linux-gnu` on Ubuntu 22.04 (tar.gz; older glibc),
  `aarch64-apple-darwin` and `x86_64-apple-darwin` (tar.gz, macOS 11+). Archives contain the binary,
  README and the font license, plus a `.sha256` file; `softprops/action-gh-release@v3` publishes them
  (tags with a `-` become pre-releases).

Workflows use the runner's cargo; they never source `tools/env.sh`. All cargo invocations use
`--locked`, so commit `Cargo.lock` changes together with dependency changes.

## 6. Embedded CJK font

The UI font `assets/fonts/NotoSansSC-Regular-ui.ttf` (≈1.2 MB, ≈0.7 MB brotli/gzip on the wire as part
of the wasm) is embedded with `include_bytes!` and installed into egui as the primary proportional
font and monospace fallback (`wcad_app::fonts`). It is a subset of Noto Sans SC (SIL OFL 1.1, license
in `assets/fonts/OFL.txt`) with 4515 characters: ASCII, Latin-1, CJK punctuation, fullwidth forms,
CAD symbols, 通用规范汉字表 level 1 (3500) ∪ GB2312 level 1 (3755), plus U+25FB (egui's replacement
glyph).

To regenerate it:

```sh
python -m pip install fonttools
# Source font (static TrueType, see the URL in the script header):
assets/fonts/make_subsets.sh path/to/NotoSansSC-Regular.ttf
# or: cargo xtask fonts path/to/NotoSansSC-Regular.ttf
```

`assets/fonts/charsets/set_ui.txt` is the tracked input used by the subset script. Other generated
character lists are local intermediates and are git-ignored. The optional `make_charsets.py` generator
requires `assets/fonts/charsets/npm/togscc/package/data/characters.txt` from the separately unpacked
`togscc` package; that source staging directory is also ignored. Normal builds and font subsetting
use the tracked character list without running the generator.

Keep the output a plain TTF/OTF: egui cannot read WOFF/WOFF2. Do not name a subset "Source …"
(Reserved Font Name under the OFL).

## 7. Troubleshooting

- *"cargo did not report the `webcad` binary"*: the build succeeded but no bin artifact message was
  seen — check that package `wcad-app` still has `[[bin]] name = "webcad"`.
- *wasm-bindgen schema mismatch*: `wasm-bindgen` and `wasm-bindgen-cli-support` versions differ in
  `Cargo.lock`; pin both to the same `=x.y.z`.
- *Blank page after a deploy*: the service worker serves the previous build until the new one is
  installed; reload once. `cargo xtask web` builds do not register a worker.
- *Port in use*: `cargo xtask serve --port 8081`.
- *Windows: "failed to remove file …\xtask.exe (os error 5)"*: a running `cargo xtask serve` keeps
  `xtask.exe` locked, so cargo cannot rebuild xtask after its sources changed. Stop the server first.
  (Rebuilding the app while the server runs is fine.)
