# webcad

[中文](#中文) | [English](#english)

---

## 中文

**webcad** 是一个用 Rust 编写的 CAD 应用：二维绘图（类似 AutoCAD）、带几何/尺寸约束的参数化草图，以及基于特征历史的三维 B-rep 实体建模。同一套源码既编译成 WebAssembly 静态网站（GitHub Pages，任何设备的浏览器都能用，可离线），也编译成 Windows / Linux / macOS 桌面程序。

> 状态：早期可用版本。下表列出的功能已有界面入口和交互测试；底层草图求解器已实现，但完整约束草图编辑界面等高级功能仍未完成。

### 当前可用功能

| 类别 | 界面与命令 |
| --- | --- |
| 绘图 | LINE、PLINE、CIRCLE、ARC、RECTANG、POLYGON、ELLIPSE、SPLINE、POINT、DONUT、REVCLOUD |
| 修改 | MOVE、ERASE、COPY、ROTATE、SCALE、MIRROR、ARRAYRECT、OFFSET、TRIM、EXTEND、FILLET、CHAMFER、BREAK、EXPLODE、JOIN |
| 文字与标注 | TEXT、MTEXT；DIMLINEAR、DIMALIGNED、DIMRADIUS、DIMDIAMETER、DIMANGULAR、DIMORDINATE；HATCH 的 SOLID/ANSI31 填充；STYLE、DIMSTYLE 样式面板 |
| 实体建模 | BOX、CYLINDER、SPHERE、CONE、TORUS；UNION、SUBTRACT、INTERSECT；封闭二维轮廓 EXTRUDE、REVOLVE，均有参数窗口 |
| 模型管理 | 模型树重命名、压缩/解除、删除及依赖检查；基本体/布尔参数编辑；MEASURE3D；EXPORTSTL、EXPORTOBJ、EXPORTSTEP |
| 绘图与文件管理 | 图层、颜色、线型、属性、对象捕捉、正交/极轴、撤销/重做；`.wcad`/`.wcadz` 保存与打开；DXF/DWG 导入导出；SVG/PDF 导出 |
| 网页与界面 | 中文/英文、内置字体、桌面/窄屏布局、触摸导航、单线程 WebAssembly、Service Worker 离线缓存 |

### 操作示例

1. 新建空图后执行 `RECTANG`，输入 `0,0`、`40,20`；执行 `SELECTALL`、`EXTRUDE`，在窗口设置距离并点击“创建”。二维原图保留，草图与实体作为一次可撤销操作生成。
2. 执行 `BOX` 设置尺寸和原点；再建立其他基本体。`UNION` 等布尔窗口必须选择目标和工具实体，不会无提示地修改最后一个实体。
3. `TEXT` 依次输入插入点、高度、角度、正文；`DIMLINEAR` 输入两点及尺寸线位置。填充先选封闭边界，执行 `HATCH`，设置图案后 Enter 确认。`STYLE`/`DIMSTYLE` 打开样式面板，修改后点击“应用”。

二维修改支持预选择或命令内选择；取消尚未确认的操作不写入文档。模型参数只有再生成功才提交，错误不会留下半个特征。Escape 可关闭建模窗口。空的功能标签不显示。

### 尚未完成与限制

- 完整约束草图编辑器、三维边圆角/倒角、三维阵列/镜像、图块创建与属性编辑等尚无完整交互入口；不能将内核测试通过等同于这些产品功能已完成。
- 拉伸/旋转接受简单闭合线、圆、圆弧及 bulge 多段线，最多 2048 段；不接受椭圆、样条、开放或相交/接触边界。界面提供 XY/XZ/YZ 平面及世界轴，不提供任意面/轴选择。
- 阵列为非关联世界 XY 矩形阵列，最多 10000 个实体；缩放仅为正等比。倒角仅支持直线，延伸不支持闭合曲线或样条。椭圆/样条偏移输出近似多段线。
- 标注和填充不与源对象关联；HATCH 不提供内部点自动寻界。MTEXT 无自动折行且不继承文字样式的宽度/倾斜；文字镜像调整锚点和基线，不镜像字形轮廓。
- 基本体窗口不提供放置旋转或实时几何预览；模型树只直接编辑基本体/布尔参数。存在依赖的删除/压缩会被拒绝，不自动级联删除。
- 测量、STL、OBJ 使用细分网格；STEP 仅导出精确实体。布尔运算降级为网格时会明确提示，不假称精确 B-rep。

### 直接使用

- 网页版：[https://risc.ink/webcad/](https://risc.ink/webcad/)，由 GitHub Pages 自动发布。也可用 `cargo xtask serve` 本地预览。
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

> Status: early usable version. The features below have application entry points and interaction tests. The sketch solver exists, but the full constraint-editing UI and other advanced features are not complete.

### Available features

| Area | UI and commands |
| --- | --- |
| Drawing | LINE, PLINE, CIRCLE, ARC, RECTANG, POLYGON, ELLIPSE, SPLINE, POINT, DONUT, REVCLOUD |
| Editing | MOVE, ERASE, COPY, ROTATE, SCALE, MIRROR, ARRAYRECT, OFFSET, TRIM, EXTEND, FILLET, CHAMFER, BREAK, EXPLODE, JOIN |
| Annotation | TEXT, MTEXT; DIMLINEAR, DIMALIGNED, DIMRADIUS, DIMDIAMETER, DIMANGULAR, DIMORDINATE; SOLID/ANSI31 HATCH; STYLE and DIMSTYLE panel |
| Modeling | BOX, CYLINDER, SPHERE, CONE, TORUS; UNION, SUBTRACT, INTERSECT; EXTRUDE and REVOLVE of closed 2D contours, with parameter dialogs |
| Model management | Rename, suppress/restore and delete with dependency checks; primitive/boolean parameter editing; MEASURE3D; EXPORTSTL, EXPORTOBJ, EXPORTSTEP |
| Drawing and files | Layers, colors, linetypes, properties, snaps, ortho/polar, undo/redo; native `.wcad`/compressed `.wcadz`; DXF/DWG import/export; SVG/PDF export |
| Web and UI | Chinese/English, embedded font, desktop/narrow layouts, touch navigation, single-threaded WebAssembly, offline service worker |

### Example workflows

1. In an empty drawing, run `RECTANG`, enter `0,0` and `40,20`, then `SELECTALL` and `EXTRUDE`. Set the distance and click Create. The source drawing is retained; one undo removes both the generated sketch and solid feature.
2. Run `BOX` to set dimensions and origin, then add other primitives. Boolean dialogs require explicit target and tool selection instead of silently modifying the last body.
3. `TEXT` asks for insertion point, height, angle and content. `DIMLINEAR` takes two points and a dimension-line position. For `HATCH`, select closed boundaries, configure the pattern, then confirm with Enter. `STYLE`/`DIMSTYLE` edits use an explicit Apply button.

2D editing accepts preselection or in-command selection. Cancellation does not commit unfinished work. Model changes commit only after successful regeneration, so failures do not leave partial features. Escape closes modeling dialogs. Empty ribbon tabs are hidden.

### Remaining limitations

- The full constraint-sketch editor, 3D edge fillet/chamfer, 3D patterns/mirror, and block creation/attribute editing do not yet have complete interactive entry points. Kernel support is not equivalent to a finished UI feature.
- Extrude/revolve accepts at most 2048 segments of simple closed lines, circles, arcs and bulge polylines. Ellipses, splines, open, crossing or touching boundaries are rejected. The UI offers XY/XZ/YZ planes and world axes, not arbitrary face/axis picking.
- Rectangular arrays are non-associative world-XY copies, capped at 10000 entities; scaling is positive and uniform. Chamfer is line-only; extend excludes closed curves and splines. Ellipse/spline offsets are approximate polylines.
- Dimensions and hatches are not associative with their sources. Hatch has no interior-point boundary search. MTEXT does not auto-wrap or inherit style width/oblique settings. Text mirroring changes anchors and baselines, not glyph outlines.
- Primitive dialogs have no placement-rotation controls or live geometry preview. Tree parameter editing covers primitives and booleans. Dependent features block deletion/suppression instead of being silently removed.
- Measurement, STL and OBJ use tessellated meshes. STEP requires exact bodies. Boolean mesh fallback is reported explicitly rather than presented as exact B-rep.

### Use it

- Web: [https://risc.ink/webcad/](https://risc.ink/webcad/), published through GitHub Pages. Use `cargo xtask serve` for a local preview.
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
