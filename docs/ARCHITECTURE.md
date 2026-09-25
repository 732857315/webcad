# webcad Architecture

This document describes the shared architecture and contracts for the CAD workspace.

## 1. Goals and hard constraints

- One Rust source tree → `wasm32-unknown-unknown` static site (GitHub Pages, any device, offline via
  service worker) **and** native desktop binaries (Windows / Linux / macOS). All application logic is
  Rust. The only JS is wasm-bindgen glue, a ~30-line loader in `index.html`, and `sw.js`.
- Three modelling domains in one document:
  1. **2D drafting** (AutoCAD-like): unconstrained entities, layers, blocks, dimensions, hatch, text.
  2. **Parametric sketches** on planes with geometric/dimensional constraints.
  3. **3D solids**: feature history (extrude, revolve, fillet, chamfer, booleans, patterns, primitives)
     regenerated into B-rep bodies, with a mesh fallback.
- UI languages: zh-CN (default) and en. Chinese text must render on first frame, offline.
- Web build is **single-threaded** (GitHub Pages cannot send COOP/COEP). Never require threads on wasm.
- Never panic on user input: kernels and parsers return `Result`. On wasm `panic = "abort"`, so a panic
  kills the app (autosave mitigates, see §9).

## 2. Workspace layout

```
Cargo.toml                 workspace, [workspace.dependencies] pins every external crate
.cargo/config.toml         `cargo xtask` alias, wasm32 rustflags (getrandom_backend="wasm_js")
crates/
  wcad-math/               f64 math re-exports (glam), tolerances, angles, bbox, transforms
  wcad-geom2d/             analytic 2D curves, intersections, offset, trim/extend/fillet, regions, text outlines
  wcad-sketch/             constraint solver + sketch model (geometry, constraints, diagnosis, drag)
  wcad-doc/                document model: 2D drawing, 3D part (feature tree), undo/redo, native file format
  wcad-solid/              3D kernel facade: exact B-rep (monstertruck) + mesh fallback (boolmesh), regeneration
  wcad-io/                 DXF/DWG (acadrust adapter), SVG, PDF (pdf-writer), STL/OBJ/STEP export entry points
  wcad-render/             wgpu renderer: 2D line/fill/text batches, 3D shaded meshes + edges, grid, overlays
  wcad-app/                egui/eframe application: lib (shared) + native `main` + wasm entry
xtask/                     build tool: web / dist / serve / native / fonts
web/                       index.html, sw.js, manifest.webmanifest, icons
assets/fonts/              NotoSansSC-Regular-ui.ttf (subset), OFL.txt, subset script + charset lists
docs/                      this file, user guide
.github/workflows/         pages.yml, ci.yml, release.yml
```

Dependency graph (arrows = "depends on"); library crates never depend on egui/eframe/wgpu:

```
wcad-math ← wcad-geom2d ← wcad-sketch
     ↑           ↑    ↖         ↑
     └──── wcad-doc ───────────┘
              ↑   ↖
       wcad-solid  wcad-io (also uses wcad-solid for mesh/STEP export)
              ↑       ↑
wcad-render (uses wcad-math only; consumes display lists)
              ↑       ↑
             wcad-app (everything)
```

`wcad-render` knows nothing about documents: the app converts the document to display lists
(`wcad-app::display`), the renderer uploads and draws them.

## 3. Pinned versions (verified 2026-09-25 on this machine)

| Purpose | Crate | Version / features |
|---|---|---|
| UI shell | eframe / egui / egui-wgpu | 0.36.2, eframe features `wgpu` (WebGPU + WebGL2 fallback) |
| GPU | wgpu | 30.0.1 (via egui-wgpu re-export) |
| wasm glue | wasm-bindgen / wasm-bindgen-futures / js-sys / web-sys | =0.2.129 / 0.4.79 / 0.3.106 |
| Bindgen in xtask | wasm-bindgen-cli-support | =0.2.129 (must equal wasm-bindgen) |
| File dialogs | rfd | 0.17.2 (open on web; save on web uses our Blob helper) |
| Math | glam | latest, f64 types (`DVec2`, `DVec3`, `DMat4`, `DAffine2/3`, `DQuat`) |
| Exact B-rep | monstertruck-{modeling,solid,fillet,meshing,topology} | =0.4.1 (pin exactly; API moves) |
| Mesh boolean | boolmesh | =0.1.10 |
| STEP export | monstertruck io crate (or truck-stepio 0.3 fallback) | decide in wcad-solid |
| DXF/DWG | acadrust | =0.5.5 (MPL-2.0, isolated in wcad-io) |
| Polyline offset/boolean | cavalier_contours | 0.9.0 |
| Tessellation | lyon_tessellation / lyon_path | 1.0.22 / 1.0.19 |
| Spatial index | rstar | 0.13.0 |
| Glyph outlines | ttf-parser | 0.25.1 |
| PDF | pdf-writer | 0.15.0 |
| Serialization | serde + serde_json | latest |
| Build server | tiny_http | 0.12 (localhost only) |

The constraint solver is **in-house**, implemented in `wcad-sketch`. Its unit and integration tests
live alongside the solver and under `crates/wcad-sketch/tests/`.

## 4. Conventions

- Units: document length unit is stored per document (default mm). Angles are radians internally,
  degrees in the UI.
- 2D: right-handed, Y up. 3D: right-handed, **Z up**. Default views: Top (XY), Front (XZ), Right (YZ).
- IDs are small `Copy` newtypes over integers, allocated from per-document counters, never reused,
  stable across save/load: `EntityId(u64)`, `LayerId(u32)`, `BlockId(u32)`, `LinetypeId(u32)`,
  `TextStyleId(u32)`, `DimStyleId(u32)`, `FeatureId(u64)`, `BodyId(u64)`, and inside sketches
  `SkEntityId(u32)`, `SkConstraintId(u32)`.
- Tolerances (`wcad-math::tol`): `LINEAR = 1e-9` (relative to model extent where it matters),
  `ANGULAR = 1e-12`, snap/pick tolerances are in **pixels** and converted by the caller.
- Errors: each library crate has its own `Error` enum (thiserror) and `Result<T>` alias.
- All public types that are persisted derive `serde::{Serialize, Deserialize}`; persisted enums use
  `#[serde(tag = "type")]` for readable JSON.
- No `unwrap()`/`expect()` on data derived from files or user input.

## 5. Core types

### 5.1 wcad-math
`pub use glam::{DVec2, DVec3, DMat3, DMat4, DQuat, DAffine2, DAffine3}`, `BBox2 {min,max}`,
`BBox3`, `Plane { origin: DVec3, x_axis: DVec3, y_axis: DVec3 }` (orthonormal; `normal()`,
`to_local(DVec3)->DVec2`, `to_world(DVec2)->DVec3`), `Angle` helpers (`normalize_0_2pi`,
`ccw_between(start,end,a)`), `tol` constants, `approx_eq`.

### 5.2 wcad-geom2d
- Curves: `Line2 {a,b}`, `Circle2 {c,r}`, `Arc2 {c, r, start, end}` (CCW from start to end angle),
  `EllipseArc2 {c, major: DVec2 (semi-major vector), ratio, start, end}` (parametric angles),
  `Polyline2 {verts: Vec<PolyVertex{p: DVec2, bulge: f64}>, closed}`, `Nurbs2 {degree, ctrl, weights, knots}`.
- `enum Curve2 { Line, Arc, Circle, Ellipse, Polyline, Spline }` implementing the `Curve` trait:
  `eval(t)`, `tangent(t)`, `domain()`, `start()/end()`, `bbox()`, `length()`, `closest(p) -> (t, point)`,
  `split(t) -> (Curve2, Curve2)`, `reversed()`, `transformed(&DAffine2)`, `flatten(tol) -> Vec<DVec2>`,
  `offset(d) -> Vec<Curve2>` (exact for line/arc/circle; polylines via cavalier_contours; others via flatten).
- `intersect(a: &Curve2, b: &Curve2) -> Vec<Intersection{p, ta, tb}>` — closed form for line/arc/circle,
  quartic for ellipses, subdivision + Newton for splines.
- Editing primitives: `trim(curve, cutters, pick_point)`, `extend(curve, boundaries, pick_end)`,
  `fillet(a, b, r, pick_a, pick_b)`, `chamfer(a, b, d1, d2, …)`, `break_at`, `join(curves)`.
- Snaps: `snap_candidates(curve, kind) -> Vec<DVec2>` for endpoint/mid/center/quadrant/node;
  perpendicular/tangent/nearest from a reference point.
- Regions: `find_regions(curves: &[Curve2], tol) -> Vec<Region{outer: Loop, holes: Vec<Loop>}>`
  (planar arrangement: split at intersections, walk minimal faces, nest by containment);
  `region_at(curves, point)` for "pick inside area" (hatch, extrude profile picking).
- Hatch: `hatch_lines(region, pattern: &HatchPattern, scale, angle) -> Vec<Line2>`; `.pat` parser;
  built-in patterns (ANSI31, ANSI37, SOLID, NET, DOTS…).
- Text: `text::Font::from_bytes`, `layout_text(font, &TextSpec) -> Vec<GlyphOutline(Vec<PathCmd>)>`
  (height, width factor, oblique, rotation, justification, MTEXT paragraphs/line wrap).

### 5.3 wcad-sketch
`Sketch` holds points/lines/circles/arcs (arc = center + start +
end points, CCW, internal equal-radius), each with a `construction: bool` flag, plus constraints:
Coincident, PointOnLine, PointOnCircle, Horizontal, Vertical, Parallel, Perpendicular, Tangent
(line–circle, circle–circle, arc endpoint smooth), Equal (length/radius), Distance (pt–pt, pt–line),
HorizontalDistance, VerticalDistance, Angle, Radius, Diameter, Midpoint, Symmetric, Fix.
API: `solve() -> SolveReport`, `drag(point, target) -> SolveReport`, `diagnose() -> Diagnosis`
(rank, DOF, per-parameter freedom, redundant and conflicting constraint groups), `set_enabled`,
`curves() -> Vec<(SkEntityId, Curve2)>` (for region detection → profiles).
Dimensional constraints store a value **expression string** plus evaluated value, so later we can add
named parameters.

### 5.4 wcad-doc
```rust
pub struct Document {
    pub meta: DocMeta,             // title, units, created/modified, app version, format version
    pub drawing: Drawing,          // 2D drafting
    pub part: Part,                // 3D feature tree
    ids: IdAllocator,
    history: History,              // undo/redo, not persisted
}
pub struct Drawing {
    pub layers: IndexMap<LayerId, Layer>, pub current_layer: LayerId,
    pub linetypes, pub text_styles, pub dim_styles, pub blocks: IndexMap<BlockId, Block>,
    pub entities: BTreeMap<EntityId, Entity>,       // BTreeMap order == draw order
    pub settings: DrawingSettings,                 // ltscale, dimscale, grid/snap spacing, limits
}
pub struct Entity { pub id, pub layer: LayerId, pub color: Color, pub linetype: LinetypeRef,
                    pub lineweight: LineWeight, pub kind: EntityKind }
pub enum EntityKind { Point{p}, Line(Line2), Circle(Circle2), Arc(Arc2), Ellipse(EllipseArc2),
    Polyline(Polyline2), Spline(Nurbs2), Text(Text), MText(MText), Dimension(Dimension),
    Hatch(Hatch), Insert(Insert) }
pub enum Color { ByLayer, ByBlock, Aci(u8), Rgb(u8,u8,u8) }   // ACI 256 palette in wcad-doc::aci
pub struct Part { pub features: Vec<Feature>, pub rollback: Option<usize>, pub bodies_meta: … }
pub struct Feature { pub id: FeatureId, pub name: String, pub suppressed: bool, pub kind: FeatureKind }
pub enum FeatureKind {
    Sketch { plane: PlaneRef, sketch: wcad_sketch::Sketch },
    Extrude { profile: ProfileRef, extent: Extent, direction: ExtrudeDir, op: BodyOp },
    Revolve { profile: ProfileRef, axis: AxisRef, angle: f64, op: BodyOp },
    Fillet { edges: Vec<EdgeRef>, radius: f64 },
    Chamfer { edges: Vec<EdgeRef>, distance: f64 },
    Primitive { shape: Primitive, placement: DAffine3, op: BodyOp },   // box/cylinder/sphere/cone/torus
    Boolean { target: BodyRef, tools: Vec<BodyRef>, op: BooleanKind },
    LinearPattern { … }, CircularPattern { … }, Mirror { … },
}
pub enum BodyOp { NewBody, Join, Cut, Intersect }
pub enum PlaneRef { Xy, Yz, Zx, Offset{base: Box<PlaneRef>, distance}, Face(FaceRef) }
```
- **Undo/redo**: every mutation goes through `doc.transact(label, |tx| …)`; `Transaction` records
  per-entity `before/after: Option<Entity>`, whole-table snapshots when tables change, and whole
  `Part` snapshots when the part changes (parts are small). `undo()`/`redo()` swap them. Tools never
  mutate `Document` directly.
- **Native file format** `.wcad`: JSON `{ "format": "webcad", "version": 1, "meta":…, "drawing":…,
  "part":… }`, optionally gzip (`miniz_oxide`, pure Rust) when saved as `.wcadz`. Loading validates and
  migrates by version.
- **Topological naming**: `FaceRef/EdgeRef` store `TopoName { feature: FeatureId, tag: TopoTag }`
  (`Side{ sk_entity, index }`, `StartCap`, `EndCap`, `FilletFace{ index }`, `PrimitiveFace{ index }`, …)
  plus a geometric fingerprint (centroid, normal/axis direction) used as a fallback matcher.

### 5.5 wcad-solid
- `trait Kernel` wraps the exact kernel; implementation `MtKernel` (monstertruck 0.4.1).
- `enum BodyRep { Exact(ExactSolid), Mesh(MeshSolid) }`. `Body { id, rep, faces: Vec<FaceInfo>,
  edges: Vec<EdgeInfo>, source: FeatureId, mesh_only_reason: Option<String> }`.
- Boolean policy: exact at tol 0.01, retry 0.05 (and inverse order); pre-pass avoids coplanar cases
  (extend cutting tools 1e-3·extent past flush faces; merge coplanar join profiles in 2D first); on
  error fall back to boolmesh on tessellations and mark the body mesh-only (no further exact ops, STL/OBJ
  export still works). Never let a kernel panic escape on native (`catch_unwind`); on wasm, validate
  inputs first and prefer monstertruck's `Result` API.
- Orientation: normalize profile faces so the normal points along the sweep (truck-family sweeps are
  inside-out otherwise).
- `regenerate(part: &Part, cache: &mut RegenCache) -> RegenResult { bodies, feature_status: Vec<FeatureStatus>,
  sketch_planes }`; cache keyed by a hash of each feature's inputs so editing feature N only recomputes N..end.
- `tessellate(body, tol) -> TriMesh { positions: Vec<[f32;3]>, normals, indices: Vec<u32>,
  face_ranges: Vec<Range<u32>>, edges: Vec<Vec<[f32;3]>>, face_names, edge_names }` — per-face meshing
  keeps triangle→face and edge polyline→edge maps for picking.
- Exports: `to_stl(&[Body]) -> Vec<u8>` (binary), `to_obj`, `to_step` (exact bodies only).

### 5.6 wcad-io
- `dxf::import(bytes) -> Result<ImportReport{ drawing, warnings }>` and `dxf::export(&Drawing,
  DxfVersion) -> Vec<u8>`; `dwg::import/export` — all via acadrust, in-memory only, mapped to our model.
- `svg::export(&Drawing, &ExportOptions) -> String` (hand-written), `pdf::export(&Drawing, &PageSetup)
  -> Vec<u8>` (pdf-writer; text as glyph outlines via wcad-geom2d::text).
- Mesh/STEP export re-exported from wcad-solid.

### 5.7 wcad-render
- `Renderer::new(&wgpu::Device, color_format)`; one `Viewport` = offscreen color (Rgba8Unorm) + depth
  (+ MSAA 4× where supported) texture registered as an egui native texture. The scene is re-rendered only
  when the view or display list changes; egui repaints do not re-render the CAD scene.
- Display lists (built by the app): `DisplayList2D { origin: DVec2, lines: Vec<LineVertex>, fills:
  Vec<FillVertex>+indices, points, per-batch style }` in f32 **relative to `origin`** (large coordinates
  stay precise). `DisplayList3D { meshes: Vec<GpuMeshData{ positions, normals, indices, color,
  transform }>, edges, sketch overlays }`.
- Cameras: `Camera2D { center: DVec2, px_per_unit: f64, size }`, `Camera3D { target, distance, yaw,
  pitch, projection: Perspective|Orthographic }` with view/proj matrices computed in f64.
- Shader output is sRGB-encoded (egui target formats are non-sRGB).
- Grid, axes, UCS icon, snap markers, rubber-band previews: small counts, drawn with the egui painter
  on top of the viewport image.

### 5.8 wcad-app
- `WebCadApp: eframe::App`. Layout: top menu; ribbon tabs (Home/Draw, Modify, Annotate, 3D Model,
  Sketch, View); left dock (Layers | Model tree); right dock (Properties); bottom command line with
  history + status bar (cursor coords, SNAP/GRID/ORTHO/POLAR/OSNAP toggles, units).
- Workspaces: **Drafting** (2D drawing viewport) and **Modeling** (3D viewport). Entering a sketch
  switches the 3D viewport to a plane-aligned 2D editor that reuses the 2D tool framework.
- Tool framework:
  ```rust
  pub trait Tool {
      fn name(&self) -> &'static str;                         // command name, e.g. "LINE"
      fn prompt(&self, s: &Strings) -> String;                // current step prompt
      fn keywords(&self) -> &[Keyword];                       // options shown in the prompt
      fn on_input(&mut self, input: ToolInput, cx: &mut ToolCx) -> ToolFlow;
      fn preview(&self, cx: &ToolCx, out: &mut Preview);      // rubber band geometry
  }
  pub enum ToolInput { Point(DVec2), Hover(DVec2), Value(f64), Text(String), Keyword(&'static str),
                       Selection(Vec<EntityId>), Enter, Escape }
  pub enum ToolFlow { Continue, Done, Cancel }
  ```
  `ToolCx` gives read access to the document, a transaction API, the snap engine, selection and settings.
- Command line grammar: `x,y` absolute, `@dx,dy` relative, `@d<a` polar, `<a` angle override, a bare
  number = direct distance along the cursor direction, keywords, command names and AutoCAD aliases
  (L, PL, C, A, REC, POL, EL, SPL, T, MT, M, CO, RO, SC, MI, O, TR, EX, F, CHA, E, X, BR, J, AR, H,
  DLI, DAL, DRA, DDI, DAN, LA, Z, U, REDO, EXT, REV, …).
- Snap engine: endpoint, midpoint, center, quadrant, intersection, perpendicular, tangent, nearest,
  node, grid; ortho and polar tracking; pixel aperture converted to world units.
- Selection: click, window (drag left→right), crossing (right→left), Shift removes, Esc clears; grips
  on selected entities (drag to edit).
- Touch: one-finger drag = pan (2D) / orbit (3D) when no tool is active, pinch = zoom, tap = click,
  long-press = context menu; toolbar buttons large enough for fingers; the command line is optional.
- i18n: `Strings` struct with `static ZH: Strings` and `static EN: Strings`; missing keys fail to compile.
- Fonts: `assets/fonts/NotoSansSC-Regular-ui.ttf` embedded with `include_bytes!`, installed as the
  primary proportional + monospace fallback; the same bytes feed `wcad-geom2d::text` for CAD text.
- Platform layer `wcad-app::platform`: `open_file() -> Future<Option<(name, bytes)>>`,
  `save_file(name, bytes)` (native rfd; web Blob download), dropped files (`bytes_async` on web),
  autosave (native: file next to settings; web: eframe storage/localStorage, size-capped).

## 6. Build, profiles, deployment

- `cargo xtask web` (dev wasm + bindgen → `dist/`), `cargo xtask dist` (profile `web-release`: opt-level
  "s", lto, codegen-units 1, panic abort; optional wasm-opt; content-hashed file names; copies `web/`
  and fonts license), `cargo xtask serve [--port]` (127.0.0.1, correct MIME types, no-store),
  `cargo xtask native [--release]`.
- `.cargo/config.toml`: `[alias] xtask = "run -p xtask --"`; `[target.wasm32-unknown-unknown]
  rustflags = ['--cfg', 'getrandom_backend="wasm_js"']`.
- Dev profile: `opt-level = 1`, dependencies `opt-level = 2`, `debug = "line-tables-only"` (disk).
- Measured baseline: egui+wgpu app at opt "s" = 6.3 MB wasm (2.5 MB gzip) before wasm-opt. Budget for
  the full app: ≤ 14 MB raw / ≤ 5 MB gzip before wasm-opt.
- GitHub Actions: `pages.yml` (build with `cargo xtask dist`, wasm-opt from binaryen version_133
  tarball, upload-pages-artifact@v5, deploy-pages@v5), `ci.yml` (fmt, clippy, tests on 3 OSes, wasm32
  clippy, `cargo xtask web` smoke), `release.yml` (native binaries for Windows msvc, Linux, macOS
  arm64/x86_64 on tags).
- Local host specifics (Windows GNU): `tools/env.sh` / `tools/env.ps1` provide cargo and GNU binutils.

## 7. Testing strategy

- Unit tests in every library crate (geometry identities, intersections vs brute force, solver sketches,
  kernel volume checks, IO round-trips with in-memory bytes).
- Golden files under `crates/*/tests/data/` (small DXF/DWG/SVG/STEP samples).
- `wcad-app` has a headless test harness (egui `Context` without a window) for tool state machines
  and command-line parsing.
- Web smoke test (manual/CI optional): `cargo xtask dist && cargo xtask serve`, headless Chrome with
  SwiftShader WebGL2 (see `xtask/scripts/cdp_shot.py` for a stdlib CDP client).

## 8. Performance budgets

- 2D: 100k simple entities pan/zoom at 60 fps on desktop (static GPU buffers; rebuild only dirty
  batches). Picking via rstar index.
- Sketch solve ≤ 10 ms for 500 equations; drag step ≤ 5 ms.
- 3D regeneration: incremental; show progress/status per feature; tessellation tolerance adaptive to
  body size.

## 9. Robustness

- Autosave every 60 s and on visibility change (web) to local storage; offer recovery on start.
- Kernel failures degrade to mesh fallback with a visible warning on the feature, never a crash.
- File import returns warnings for unsupported entities instead of failing.
