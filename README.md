# webcad

[中文](#中文) | [English](#english)

---

## 中文

**webcad** 是一个用 Rust 编写的 CAD 应用：二维绘图（类似 AutoCAD）、带几何/尺寸约束的参数化草图，以及基于特征历史的三维 B-rep 实体建模。同一套源码既编译成 WebAssembly 静态网站（GitHub Pages，任何设备的浏览器都能用，可离线），也编译成 Windows / Linux / macOS 桌面程序。

> 状态：早期开发中。已包含二维几何、草图求解、实体建模和文件读写等模块，界面集成与完整功能仍在完善。

### 计划功能

- **二维绘图**：直线、多段线、圆、圆弧、椭圆、样条、文字/多行文字、尺寸标注、图案填充、图块；图层、线型、颜色；修剪、延伸、偏移、圆角、倒角、阵列、镜像等编辑命令；对象捕捉、正交、极轴追踪；AutoCAD 风格命令行与命令别名（L、C、TR、O…）。
- **参数化草图**：在基准面或实体面上绘制草图，支持重合、水平、竖直、平行、垂直、相切、相等、对称、距离、角度、半径等约束，显示自由度并诊断过约束/冲突。
- **三维实体**：拉伸、旋转、圆角、倒角、布尔运算（并/差/交）、阵列、镜像、基本体；特征树可编辑、可回滚；精确 B-rep 内核失败时自动退回网格布尔运算。
- **文件**：原生格式 `.wcad`（JSON，可 gzip 压缩为 `.wcadz`）；DXF / DWG 导入导出；SVG、PDF 导出；STL、OBJ、STEP 导出。
- **界面**：中文（默认）与英文；首帧即可显示中文（内置字体，无需联网）；支持触摸（单指平移/旋转、双指缩放、长按菜单）。
- **网页版**：单线程 WebAssembly（GitHub Pages 无法提供跨源隔离），Service Worker 离线缓存，可“添加到主屏幕”。

### 直接使用

- 网页版：配置并成功部署 GitHub Pages 后，访问 `https://732857315.github.io/webcad/`。部署前可用 `cargo xtask serve` 本地预览。
- 桌面版：发布后从 [GitHub Releases](https://github.com/732857315/webcad/releases) 下载对应平台的压缩包（Windows `.zip`，Linux / macOS `.tar.gz`），解压后运行 `webcad`。

### 从源码构建

需要 Rust stable（1.95 及以上）。

```sh
rustup target add wasm32-unknown-unknown

cargo xtask web        # 开发版网页构建 -> dist/
cargo xtask serve      # 本机预览：http://127.0.0.1:8080/
cargo xtask dist       # 发布版网页构建（体积优化、文件名带内容哈希、离线 Service Worker）
cargo xtask native     # 桌面版（加 --release 为发布版）
cargo run              # 直接运行桌面版
```

**便携工具链**（本仓库的开发机）：在 Git Bash 中先执行 `. tools/env.sh`（PowerShell：`. .\tools\env.ps1`），它会从 `MoveCoding/Rust` 找到便携版 Rust 和 GNU binutils，不修改系统环境。其他机器上直接使用 PATH 里的 cargo 即可。

详细说明（wasm-opt、GitHub Pages 设置、字体子集脚本、CI）见 [docs/BUILD.md](docs/BUILD.md)，架构见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

### 许可证

- 代码：MIT 或 Apache-2.0，二选一（`MIT OR Apache-2.0`）。
- 内置字体：`assets/fonts/NotoSansSC-Regular-ui.ttf` 是 Noto Sans SC（思源黑体）的子集，采用 SIL Open Font License 1.1，见 [assets/fonts/OFL.txt](assets/fonts/OFL.txt)；网页版和桌面版发布包都附带该许可证。
- 依赖：DXF/DWG 读写使用 [acadrust](https://crates.io/crates/acadrust)（MPL-2.0），作为未修改的依赖库使用；其他依赖的许可证见各自的 crate。

---

## English

**webcad** is a CAD application written in Rust: AutoCAD-style 2D drafting, parametric sketches with geometric and dimensional constraints, and feature-based 3D B-rep solid modeling. One source tree compiles to a WebAssembly static site (GitHub Pages; runs in any modern browser, works offline) and to native desktop programs for Windows, Linux and macOS.

> Status: early development. Modules for 2D geometry, sketch solving, solid modeling and file I/O are present; UI integration and full functionality are still in progress.

### Planned features

- **2D drafting**: lines, polylines, circles, arcs, ellipses, splines, text/mtext, dimensions, hatches, blocks; layers, linetypes, colors; trim, extend, offset, fillet, chamfer, array, mirror and more; object snaps, ortho, polar tracking; an AutoCAD-style command line with the usual aliases (L, C, TR, O, …).
- **Parametric sketches** on planes or solid faces: coincident, horizontal, vertical, parallel, perpendicular, tangent, equal, symmetric, distance, angle, radius constraints, with degree-of-freedom display and over-constraint/conflict diagnosis.
- **3D solids**: extrude, revolve, fillet, chamfer, booleans (union/subtract/intersect), patterns, mirror, primitives; an editable feature tree with rollback; automatic fallback to mesh booleans when the exact B-rep kernel fails.
- **Files**: native `.wcad` (JSON, optionally gzip-compressed `.wcadz`); DXF/DWG import and export; SVG and PDF export; STL, OBJ and STEP export.
- **UI**: Chinese (default) and English; Chinese text renders on the first frame with an embedded font (no network needed); touch support (one-finger pan/orbit, pinch zoom, long-press menu).
- **Web build**: single-threaded WebAssembly (GitHub Pages cannot send cross-origin isolation headers), offline service worker, installable as a PWA.

### Use it

- Web: after configuring and successfully deploying GitHub Pages, open `https://732857315.github.io/webcad/`. Before deployment, use `cargo xtask serve` for a local preview.
- Desktop: once a release is published, download the archive for your platform from [GitHub Releases](https://github.com/732857315/webcad/releases) (Windows `.zip`, Linux/macOS `.tar.gz`), unpack and run `webcad`.

### Build from source

Requires stable Rust (1.95 or newer).

```sh
rustup target add wasm32-unknown-unknown

cargo xtask web        # dev web build -> dist/
cargo xtask serve      # preview at http://127.0.0.1:8080/
cargo xtask dist       # release web build (size-optimized, content-hashed names, offline service worker)
cargo xtask native     # desktop build (add --release for an optimized build)
cargo run              # run the desktop app
```

**Portable toolchain** (this repository's development machine): in Git Bash run `. tools/env.sh` first (PowerShell: `. .\tools\env.ps1`). It locates the portable Rust and GNU binutils under `MoveCoding/Rust` without touching the system environment. On other machines just use the cargo on your PATH.

More details (wasm-opt, GitHub Pages setup, the font subset script, CI) are in [docs/BUILD.md](docs/BUILD.md); the architecture is described in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

### License

- Code: MIT or Apache-2.0, at your option (`MIT OR Apache-2.0`).
- Embedded font: `assets/fonts/NotoSansSC-Regular-ui.ttf` is a subset of Noto Sans SC under the SIL Open Font License 1.1, see [assets/fonts/OFL.txt](assets/fonts/OFL.txt). The web and desktop packages ship this license.
- Dependencies: DXF/DWG support uses [acadrust](https://crates.io/crates/acadrust) (MPL-2.0) as an unmodified library dependency; other dependencies are under their own licenses.
