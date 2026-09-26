//! Drawing contours -> persisted sketches -> kernel features. No drawing entities are consumed.

use std::f64::consts::TAU;

use wcad_doc::{
    AxisRef, BodyOp, BodyRef, Document, EntityId, EntityKind, Extent, Feature, FeatureId,
    FeatureKind, Part, PlaneRef, ProfileRef,
};
use wcad_geom2d::{Curve, Curve2, Line2, Region, bulge_to_arc, find_regions, intersect_tol};
use wcad_math::{BBox2, DVec2, DVec3, Plane};
use wcad_sketch::Sketch;
use wcad_solid::{FeatureStatus, RegenCache, RegenResult, regenerate};

use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::editor::{AppRequest, Editor, Workspace};
use crate::i18n::Lang;

const EXTRUDE_DIALOG: &str = "model_profile_extrude";
const REVOLVE_DIALOG: &str = "model_profile_revolve";
const STATE_ID: &str = "model_profile_state";
const MAX_CURVES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Extrude,
    Revolve,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Extrude => "EXTRUDE",
            Self::Revolve => "REVOLVE",
        }
    }

    fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Extrude => lang.pick("拉伸", "Extrude"),
            Self::Revolve => lang.pick("旋转成体", "Revolve"),
        }
    }

    fn dialog_id(self) -> &'static str {
        match self {
            Self::Extrude => EXTRUDE_DIALOG,
            Self::Revolve => REVOLVE_DIALOG,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    NewBody,
    Join,
    Cut,
    Intersect,
}

impl Operation {
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::NewBody => lang.pick("新实体", "New body"),
            Self::Join => lang.pick("合并", "Join"),
            Self::Cut => lang.pick("切除", "Cut"),
            Self::Intersect => lang.pick("相交", "Intersect"),
        }
    }

    fn body_op(self, target: Option<BodyRef>) -> BodyOp {
        match self {
            Self::NewBody => BodyOp::NewBody,
            Self::Join => BodyOp::Join { target },
            Self::Cut => BodyOp::Cut { target },
            Self::Intersect => BodyOp::Intersect { target },
        }
    }
}

#[derive(Clone, Debug)]
struct Parameters {
    plane: PlaneRef,
    distance: f64,
    symmetric: bool,
    reversed: bool,
    axis: AxisRef,
    angle: f64,
    operation: Operation,
    target: Option<BodyRef>,
}

impl Default for Parameters {
    fn default() -> Self {
        Self {
            plane: PlaneRef::Xy,
            distance: 10.0,
            symmetric: false,
            reversed: false,
            axis: AxisRef::Z,
            angle: 360.0,
            operation: Operation::NewBody,
            target: None,
        }
    }
}

#[derive(Clone)]
struct Dialog {
    kind: Kind,
    parameters: Parameters,
    generation: u64,
    target_part: Option<Part>,
    targets: Vec<(BodyRef, String)>,
    error: Option<String>,
}

pub(super) fn register(r: &mut CommandRegistry) {
    r.add(CommandSpec {
        name: "EXTRUDE",
        aliases: &["EXT"],
        label: |lang| Kind::Extrude.label(lang),
        icon: "Ext",
        tab: Some(RibbonTab::Model),
        group: "features",
        kind: CommandKind::Action(|ed| open(ed, Kind::Extrude)),
    });
    r.add(CommandSpec {
        name: "REVOLVE",
        aliases: &["REV"],
        label: |lang| Kind::Revolve.label(lang),
        icon: "Rev",
        tab: Some(RibbonTab::Model),
        group: "features",
        kind: CommandKind::Action(|ed| open(ed, Kind::Revolve)),
    });
    r.add_ui_hook(dialog_ui);
}

fn selection_prompt(lang: Lang) -> &'static str {
    lang.pick(
        "请先在二维视图选择封闭轮廓。可在窗口打开后继续选择；创建时使用当前选择。",
        "Select closed contours in the 2D view first. Selection remains live while this window is open.",
    )
}

fn open(ed: &mut Editor, kind: Kind) {
    if ed.has_tool() {
        ed.escape();
    }
    ed.info(selection_prompt(ed.lang));
    ed.requests.push(AppRequest::ShowDialog(kind.dialog_id()));
}

fn dialog_ui(ctx: &egui::Context, ed: &mut Editor) {
    let state_id = egui::Id::new(STATE_ID);
    let mut state = ctx.data_mut(|d| {
        let state = d.get_temp::<Dialog>(state_id);
        d.remove::<Dialog>(state_id);
        state
    });
    if state
        .as_ref()
        .is_some_and(|s| s.generation != ed.document_generation())
    {
        state = None;
    }
    for kind in [Kind::Extrude, Kind::Revolve] {
        if ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new(kind.dialog_id()))) == Some(true) {
            state = Some(Dialog {
                kind,
                parameters: Parameters {
                    plane: if kind == Kind::Revolve {
                        PlaneRef::Xz
                    } else {
                        PlaneRef::Xy
                    },
                    ..Parameters::default()
                },
                generation: ed.document_generation(),
                target_part: None,
                targets: Vec::new(),
                error: None,
            });
        }
    }
    let Some(mut state) = state else { return };
    let lang = ed.lang;
    let mut visible = true;
    let mut create = false;
    let mut cancel = false;
    // Cache just the target picker, never the selected drawing geometry or the commit candidate.
    if state.target_part.as_ref() != Some(&ed.doc.part) {
        let regen = regenerate(&ed.doc.part, &mut RegenCache::new());
        state.targets = regen
            .bodies
            .iter()
            .map(|b| {
                let name = ed
                    .doc
                    .part
                    .feature(b.id.0)
                    .map_or("Body", |f| f.name.as_str());
                (
                    b.id,
                    format!(
                        "{name} (#{}{})",
                        b.id.0.0,
                        if b.is_exact() { "" } else { ", mesh" }
                    ),
                )
            })
            .collect();
        state.target_part = Some(ed.doc.part.clone());
    }
    egui::Window::new(state.kind.label(lang))
        .id(egui::Id::new("model_profile_window"))
        .open(&mut visible)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0_f32.min((ctx.content_rect().width() - 32.0).max(1.0)))
        .max_width((ctx.content_rect().width() - 32.0).max(1.0))
        .max_height((ctx.content_rect().height() - 48.0).max(1.0))
        .vscroll(true)
        .show(ctx, |ui| {
            ui.label(selection_prompt(lang));
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}: {}", lang.pick("当前选中", "Selected now"), ed.selection.len()));
                if ui.button(lang.pick("到二维视图选择", "Select in 2D")).clicked() {
                    if ed.has_tool() { ed.escape(); }
                    ed.requests.push(AppRequest::SetWorkspace(Workspace::Drafting));
                }
            });
            ui.small(lang.pick(
                "支持线、圆、圆弧和带圆弧的闭合多段线；不支持椭圆、样条及相交/接触边界。原二维对象保持不变。",
                "Lines, circles, arcs and closed bulge polylines. No ellipses, splines or crossing/touching loops. Source entities are retained.",
            ));
            ui.separator();
            let p = &mut state.parameters;
            ui.horizontal(|ui| {
                ui.label(lang.pick("草图平面", "Sketch plane"));
                ui.selectable_value(&mut p.plane, PlaneRef::Xy, "XY");
                ui.selectable_value(&mut p.plane, PlaneRef::Xz, "XZ");
                ui.selectable_value(&mut p.plane, PlaneRef::Yz, "YZ");
            });
            ui.small(lang.pick(
                "二维 (x,y) 映射为平面局部坐标。正向法线：XY +Z，XZ -Y，YZ +X。",
                "Drawing (x,y) becomes plane-local coordinates. Normals: XY +Z, XZ -Y, YZ +X.",
            ));
            if state.kind == Kind::Extrude {
                ui.horizontal(|ui| {
                    ui.label(lang.pick("距离（总长度）", "Distance (total length)"));
                    ui.add(egui::DragValue::new(&mut p.distance).speed(0.1));
                    ui.label(ed.doc.meta.units.suffix());
                });
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut p.symmetric, false, lang.pick("单向定长", "Blind"));
                    ui.selectable_value(&mut p.symmetric, true, lang.pick("对称", "Symmetric"));
                    ui.add_enabled_ui(!p.symmetric, |ui| {
                        ui.checkbox(&mut p.reversed, lang.pick("反向", "Reverse"));
                    });
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(lang.pick("世界旋转轴", "World axis"));
                    ui.selectable_value(&mut p.axis, AxisRef::X, "X");
                    ui.selectable_value(&mut p.axis, AxisRef::Y, "Y");
                    ui.selectable_value(&mut p.axis, AxisRef::Z, "Z");
                });
                ui.horizontal(|ui| {
                    ui.label(lang.pick("角度（度）", "Angle (degrees)"));
                    ui.add(egui::DragValue::new(&mut p.angle).speed(1.0));
                });
                ui.small(lang.pick(
                    "轴经过世界原点且必须位于草图平面内；轮廓不能跨轴。负角度为反向，最大一周。",
                    "Axis passes through the origin and must lie in the sketch plane. Contours cannot cross it. Negative angles reverse; maximum one turn.",
                ));
            }
            ui.horizontal(|ui| {
                ui.label(lang.pick("操作", "Operation"));
                egui::ComboBox::from_id_salt("profile_operation")
                    .selected_text(p.operation.label(lang))
                    .show_ui(ui, |ui| {
                        for op in [Operation::NewBody, Operation::Join, Operation::Cut, Operation::Intersect] {
                            ui.selectable_value(&mut p.operation, op, op.label(lang));
                        }
                    });
            });
            if p.operation != Operation::NewBody {
                let auto = lang.pick("自动（最近创建或修改的实体）", "Automatic (last created or modified body)");
                let selected = match p.target {
                    None => auto.to_owned(),
                    Some(id) => state.targets.iter().find(|(r, _)| *r == id).map(|(_, s)| s.clone())
                        .unwrap_or_else(|| format!("{} #{}", lang.pick("目标已失效", "Unavailable target"), id.0.0)),
                };
                egui::ComboBox::from_id_salt("profile_target")
                    .selected_text(selected)
                    .width(ui.available_width().min(350.0))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut p.target, None, auto);
                        for (id, name) in &state.targets {
                            ui.selectable_value(&mut p.target, Some(*id), name);
                        }
                    });
                if state.targets.is_empty() {
                    ui.colored_label(ui.visuals().error_fg_color, lang.pick("此操作需要已有实体。", "This operation requires an existing body."));
                }
            }
            if let Some(error) = &state.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                create = ui.add_enabled(!ed.selection.is_empty(), egui::Button::new(lang.pick("创建", "Create"))).clicked();
                cancel = ui.button(lang.pick("取消", "Cancel")).clicked();
            });
        });
    cancel |= super::cancel_requested(ctx);
    if cancel {
        super::cancelled(ed);
    }
    if visible && !cancel && create {
        match apply(ed, state.kind, &state.parameters) {
            Ok(_) => visible = false,
            Err(error) => {
                ed.error(&error);
                state.error = Some(error);
            }
        }
    }
    if visible && !cancel {
        ctx.data_mut(|d| d.insert_temp(state_id, state));
    }
}

fn geometry_error(lang: Lang) -> String {
    lang.pick(
        "轮廓包含非有限值、零长度或退化曲线。",
        "The profile contains non-finite, zero-length or degenerate curves.",
    )
    .to_owned()
}

fn boundary_error(lang: Lang) -> String {
    lang.pick(
        "所有选中边界必须组成独立封闭环，不能开放、重复、自交或相互接触。",
        "All selected boundaries must form separate closed loops, without open ends, duplicates, crossings or touching loops.",
    ).to_owned()
}

/// Region detection intentionally ignores dangling edges. Validate the entire input first, so an
/// extra open line never silently disappears from a user's selection. Nested loops remain holes.
fn sketch_from_selection(
    doc: &Document,
    selected: &[EntityId],
    lang: Lang,
) -> Result<(Sketch, Vec<DVec2>, f64), String> {
    if selected.is_empty() {
        return Err(selection_prompt(lang).to_owned());
    }
    if selected.len() > MAX_CURVES {
        return Err(lang
            .pick(
                "轮廓最多支持 2048 段曲线。",
                "Profiles support at most 2048 curves.",
            )
            .to_owned());
    }
    let mut curves = Vec::new();
    for id in selected {
        let entity = doc.drawing.entities.get(id).ok_or_else(|| {
            lang.pick(
                "选中对象已不存在，请重新选择。",
                "A selected entity no longer exists; select again.",
            )
            .to_owned()
        })?;
        match &entity.kind {
            EntityKind::Line(l) => curves.push(Curve2::Line(*l)),
            EntityKind::Circle(c) => curves.push(Curve2::Circle(*c)),
            EntityKind::Arc(a) => curves.push(Curve2::Arc(*a)),
            EntityKind::Polyline(p) => {
                if !p.closed || p.segment_count() == 0 {
                    return Err(boundary_error(lang));
                }
                if curves.len() + p.segment_count() > MAX_CURVES {
                    return Err(lang.pick("轮廓最多支持 2048 段曲线。", "Profiles support at most 2048 curves.").to_owned());
                }
                for i in 0..p.segment_count() {
                    let a = p.verts[i];
                    let b = p.verts[(i + 1) % p.verts.len()];
                    if !a.p.is_finite() || !b.p.is_finite() || !a.bulge.is_finite() || a.p == b.p {
                        return Err(geometry_error(lang));
                    }
                    if a.bulge.abs() < wcad_geom2d::bulge::BULGE_EPS {
                        curves.push(Curve2::Line(Line2::new(a.p, b.p)));
                    } else {
                        // Do not use the iterator's invalid-bulge -> straight-line fallback.
                        let arc = bulge_to_arc(a.p, b.p, a.bulge).ok_or_else(|| geometry_error(lang))?;
                        curves.push(Curve2::Arc(arc.to_arc()));
                    }
                }
            }
            EntityKind::Ellipse(_) | EntityKind::Spline(_) => return Err(lang.pick(
                "草图库不支持椭圆或样条轮廓；不会近似替换为折线。",
                "The sketch model does not support ellipse or spline profiles; no polyline approximation is substituted.",
            ).to_owned()),
            _ => return Err(format!("{}: {}", lang.pick("不是支持的轮廓曲线", "Unsupported profile entity"), entity.kind.type_name())),
        }
        if curves.len() > MAX_CURVES {
            return Err(lang
                .pick(
                    "轮廓最多支持 2048 段曲线。",
                    "Profiles support at most 2048 curves.",
                )
                .to_owned());
        }
    }
    let mut bounds = BBox2::EMPTY;
    for c in &curves {
        let finite = match c {
            Curve2::Line(l) => l.a.is_finite() && l.b.is_finite(),
            Curve2::Circle(c) => c.c.is_finite() && c.r.is_finite() && c.r > 0.0,
            Curve2::Arc(a) => {
                a.c.is_finite()
                    && a.r.is_finite()
                    && a.r > 0.0
                    && a.start.is_finite()
                    && a.end.is_finite()
                    && a.start != a.end
            }
            _ => false,
        };
        if !finite {
            return Err(geometry_error(lang));
        }
        // Match the tolerance used by wcad-solid's sketch profile regeneration.
        bounds = bounds.union(&match c {
            Curve2::Arc(a) => BBox2::new(a.c - DVec2::splat(a.r), a.c + DVec2::splat(a.r)),
            _ => c.bbox(),
        });
    }
    let size = bounds.size().length();
    if !size.is_finite() || size <= 0.0 {
        return Err(geometry_error(lang));
    }
    let tol = (size * 1e-7).max(1e-9);
    for c in &curves {
        if !c.length().is_finite()
            || c.length() <= tol
            || !c.start().is_finite()
            || !c.end().is_finite()
            || (!c.is_closed() && c.start().distance(c.end()) <= tol)
        {
            return Err(geometry_error(lang));
        }
    }
    let endpoints: Vec<DVec2> = curves
        .iter()
        .filter(|c| !c.is_closed())
        .flat_map(|c| [c.start(), c.end()])
        .collect();
    for (i, p) in endpoints.iter().enumerate() {
        if endpoints
            .iter()
            .enumerate()
            .filter(|(j, q)| *j != i && p.distance(**q) <= tol)
            .count()
            != 1
        {
            return Err(boundary_error(lang));
        }
    }
    let at_end = |c: &Curve2, p: DVec2| {
        !c.is_closed() && (c.start().distance(p) <= tol || c.end().distance(p) <= tol)
    };
    for (i, a) in curves.iter().enumerate() {
        for b in &curves[i + 1..] {
            if !a.bbox().expanded(tol).intersects(&b.bbox().expanded(tol)) {
                continue;
            }
            for hit in intersect_tol(a, b, tol) {
                if !at_end(a, hit.p) || !at_end(b, hit.p) {
                    return Err(boundary_error(lang));
                }
            }
            for (one, other) in [(a, b), (b, a)] {
                let (lo, hi) = one.domain();
                let mid = one.point_at((lo + hi) * 0.5);
                if other.closest(mid).1.distance(mid) <= tol {
                    return Err(boundary_error(lang));
                }
            }
        }
    }
    let mut sketch = Sketch::new();
    for c in &curves {
        match c {
            Curve2::Line(l) => {
                sketch.add_line_points(l.a, l.b);
            }
            Curve2::Circle(c) => {
                sketch.add_circle_center_radius(c.c, c.r);
            }
            Curve2::Arc(a) => {
                sketch
                    .add_arc_center_start_end(a.c, a.start_point(), a.end_point())
                    .map_err(|_| geometry_error(lang))?;
            }
            _ => unreachable!(),
        }
    }
    let sketch_curves = sketch.curves();
    let curves: Vec<Curve2> = sketch_curves.iter().map(|(_, c)| c.clone()).collect();
    let regions = find_regions(&curves, tol);
    if regions.is_empty()
        || regions
            .iter()
            .any(|r| !r.area().is_finite() || r.area() <= tol * tol)
    {
        return Err(boundary_error(lang));
    }
    let mut used = vec![false; curves.len()];
    let mut seeds = Vec::new();
    for (i, region) in regions.iter().enumerate() {
        // For disjoint simple loops, containment depth implements even/odd holes, including islands.
        let sample = region.outer.curves[0].start();
        let depth = regions
            .iter()
            .enumerate()
            .filter(|(j, other)| *j != i && other.outer.contains(sample))
            .count();
        if !depth.is_multiple_of(2) {
            continue;
        }
        for lp in region.loops() {
            for &source in &lp.sources {
                if let Some(u) = used.get_mut(source) {
                    *u = true;
                }
            }
        }
        let face =
            wcad_solid::build_profile_face(&Plane::XY, region, &|source| sketch_curves[source].0)
                .map_err(|e| {
                format!(
                    "{}: {e}",
                    lang.pick("轮廓内核校验失败", "Profile kernel validation failed")
                )
            })?;
        seeds.push(interior_seed(region, &face).ok_or_else(|| boundary_error(lang))?);
    }
    if seeds.is_empty() || used.contains(&false) {
        return Err(boundary_error(lang));
    }
    Ok((sketch, seeds, tol))
}

/// Sampling is only for locating a region seed. The persisted curves and swept faces stay analytic.
fn interior_seed(region: &Region, face: &wcad_solid::ProfileFace) -> Option<DVec2> {
    let center = region.centroid();
    if face.contains(center) && region.contains(center) {
        return Some(center);
    }
    let bb = region.bbox();
    let mut best = None;
    for row in 0..32 {
        let y = bb.min.y + bb.size().y * (row as f64 + 0.513) / 32.0;
        let mut xs = Vec::new();
        for poly in &face.polygons {
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                if (a.y > y) != (b.y > y) {
                    xs.push(a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.windows(2) {
            let p = DVec2::new((pair[0] + pair[1]) * 0.5, y);
            let width = pair[1] - pair[0];
            if width > 0.0
                && face.contains(p)
                && region.contains(p)
                && best.is_none_or(|(w, _)| width > w)
            {
                best = Some((width, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

fn check_regen(regen: &RegenResult, lang: Lang) -> Result<(), String> {
    for (id, status) in &regen.feature_status {
        match status {
            FeatureStatus::Error(error) => {
                return Err(format!(
                    "{} #{}: {error}",
                    lang.pick("特征再生失败", "Feature regeneration failed"),
                    id.0
                ));
            }
            FeatureStatus::Warning(w) if w.contains("no profile region at") => {
                return Err(format!(
                    "{}: {w}",
                    lang.pick("轮廓区域未全部生成", "Not all profile regions regenerated")
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn apply(ed: &mut Editor, kind: Kind, p: &Parameters) -> Result<FeatureId, String> {
    let lang = ed.lang;
    if ed.doc.part.rollback.is_some() {
        return Err(lang
            .pick(
                "请先将特征历史回退位置恢复到末尾。",
                "Restore the feature history to its end before adding a profile feature.",
            )
            .to_owned());
    }
    match kind {
        Kind::Extrude if !p.distance.is_finite() || p.distance <= 1e-9 => {
            return Err(lang
                .pick(
                    "拉伸距离必须为正的有限数值。",
                    "Extrude distance must be finite and positive.",
                )
                .to_owned());
        }
        Kind::Revolve if !p.angle.is_finite() || p.angle.abs() <= 1e-7 || p.angle.abs() > 360.0 => {
            return Err(lang
                .pick(
                    "旋转角度必须非零且介于 -360 和 360 度之间。",
                    "Revolve angle must be non-zero and between -360 and 360 degrees.",
                )
                .to_owned());
        }
        _ => {}
    }
    let selected = ed.selection.to_vec();
    let (sketch, regions, tol) = sketch_from_selection(&ed.doc, &selected, lang)?;
    let mut part = ed.doc.part.clone();
    let mut ids = ed.doc.ids().clone();
    for f in &part.features {
        ids.bump_feature(f.id);
    }
    let sketch_id = ids.feature();
    let feature_id = ids.feature();
    part.features.push(Feature {
        id: sketch_id,
        name: format!(
            "{} {}",
            lang.pick("轮廓草图", "Profile sketch"),
            sketch_id.0
        ),
        suppressed: false,
        kind: FeatureKind::Sketch {
            plane: p.plane.clone(),
            sketch,
        },
    });
    let mut cache = RegenCache::new();
    let before = regenerate(&part, &mut cache);
    check_regen(&before, lang)?;
    if p.operation != Operation::NewBody
        && (before.bodies.is_empty()
            || p.target.is_some_and(|target| before.body(target).is_none()))
    {
        return Err(lang
            .pick(
                "目标实体不存在，请选择已有实体或新实体模式。",
                "The target body does not exist; select an existing body or New body.",
            )
            .to_owned());
    }
    let profile = ProfileRef {
        sketch: sketch_id,
        regions,
    };
    let op = p.operation.body_op(p.target);
    let feature = match kind {
        Kind::Extrude => FeatureKind::Extrude {
            profile,
            extent: if p.symmetric {
                Extent::Symmetric {
                    distance: p.distance,
                }
            } else {
                Extent::Blind {
                    distance: p.distance,
                }
            },
            reversed: p.reversed && !p.symmetric,
            op,
        },
        Kind::Revolve => {
            // Check exact extrema, not just the kernel's sampled boundary: a small arc crossing
            // the axis must not become an invalid self-intersecting solid on wasm (panic=abort).
            let plane = before
                .sketch_planes
                .iter()
                .find(|(id, _)| *id == sketch_id)
                .map(|(_, plane)| *plane)
                .ok_or_else(|| geometry_error(lang))?;
            let axis = match p.axis {
                AxisRef::X => DVec3::X,
                AxisRef::Y => DVec3::Y,
                AxisRef::Z => DVec3::Z,
                _ => {
                    return Err(lang
                        .pick(
                            "仅支持世界 X/Y/Z 旋转轴。",
                            "Only world X/Y/Z revolve axes are supported.",
                        )
                        .to_owned());
                }
            };
            if axis.dot(plane.normal()).abs() > 1e-9
                || plane.signed_distance(DVec3::ZERO).abs() > tol
            {
                return Err(lang
                    .pick(
                        "旋转轴必须位于草图平面内。",
                        "The revolve axis must lie in the sketch plane.",
                    )
                    .to_owned());
            }
            let radial = axis.cross(plane.normal());
            let local = DVec2::new(radial.dot(plane.x_axis), radial.dot(plane.y_axis));
            let offset = radial.dot(plane.origin);
            if let FeatureKind::Sketch { sketch, .. } = &part.features.last().unwrap().kind {
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for (_, c) in sketch.curves() {
                    let mut points = vec![c.start(), c.end()];
                    match c {
                        Curve2::Circle(circle) => points
                            .extend([circle.c + local * circle.r, circle.c - local * circle.r]),
                        Curve2::Arc(arc) => {
                            for a in [local.to_angle(), local.to_angle() + std::f64::consts::PI] {
                                if wcad_math::ccw_between(arc.start, arc.end, a, 1e-12) {
                                    points.push(arc.at_angle(a));
                                }
                            }
                        }
                        _ => {}
                    }
                    for point in points {
                        let side = point.dot(local) + offset;
                        lo = lo.min(side);
                        hi = hi.max(side);
                    }
                }
                if lo < -tol && hi > tol {
                    return Err(lang
                        .pick(
                            "轮廓不能跨越旋转轴。",
                            "The profile must not cross the revolve axis.",
                        )
                        .to_owned());
                }
            }
            FeatureKind::Revolve {
                profile,
                axis: p.axis.clone(),
                angle: p.angle / 360.0 * TAU,
                op,
            }
        }
    };
    part.features.push(Feature {
        id: feature_id,
        name: format!("{} {}", kind.label(lang), feature_id.0),
        suppressed: false,
        kind: feature,
    });
    let regen = regenerate(&part, &mut cache);
    check_regen(&regen, lang)?;
    let body = regen
        .bodies
        .iter()
        .find(|b| b.source == feature_id)
        .ok_or_else(|| {
            lang.pick("内核未生成实体。", "The kernel did not produce a body.")
                .to_owned()
        })?;
    if body.bbox().is_empty()
        || !body.bbox().size().is_finite()
        || !body.approx_volume().is_finite()
        || body.approx_volume().abs() <= tol.powi(3)
    {
        return Err(lang
            .pick(
                "生成的实体为空或退化；未修改文档。",
                "The generated body is empty or degenerate; the document was not changed.",
            )
            .to_owned());
    }
    let warnings: Vec<String> = regen
        .feature_status
        .iter()
        .filter_map(|(id, status)| match status {
            FeatureStatus::Warning(w) if *id == feature_id => Some(w.clone()),
            _ => None,
        })
        .collect();
    // No fallible work belongs inside the transaction. Allocator counters are intentionally not
    // undoable, so an undone sketch/feature ID cannot be reused, including after save/reload.
    ed.doc.transact(kind.name(), |tx| {
        *tx.part_mut() = part;
        *tx.ids() = ids;
    });
    ed.pump();
    ed.info(lang.pick(
        "已创建草图和实体特征；一次撤销同时移除两者，原二维对象未改动。",
        "Created sketch and solid feature in one undo step; source 2D entities are unchanged.",
    ));
    for warning in warnings {
        ed.info(format!(
            "{}: {warning}",
            lang.pick("内核警告", "Kernel warning")
        ));
    }
    ed.requests
        .push(AppRequest::SetWorkspace(Workspace::Modeling));
    ed.requests.push(AppRequest::ZoomExtents);
    Ok(feature_id)
}

#[cfg(test)]
mod tests;
