//! Explicit-body booleans, measurement and actual kernel exports.

use wcad_doc::BooleanKind;
use wcad_solid::Body;

use super::*;
use crate::commands::{CommandKind, CommandSpec, RibbonTab};

pub(super) const BOOLEAN_REQUESTS: [&str; 3] = [
    "modeling.boolean.union",
    "modeling.boolean.subtract",
    "modeling.boolean.intersect",
];
pub(super) const INSPECT_REQUESTS: [&str; 4] = [
    "modeling.inspect.measure",
    "modeling.inspect.stl",
    "modeling.inspect.obj",
    "modeling.inspect.step",
];
const BOOLEAN_STATE: &str = "modeling.boolean.state";
const INSPECT_STATE: &str = "modeling.inspect.state";

pub(super) fn register(r: &mut CommandRegistry) {
    for (name, aliases, label, icon, action) in [
        (
            "UNION",
            &["UNI"][..],
            (|l| ms(l).cmd_union) as fn(Lang) -> &'static str,
            "U",
            (|e| show_dialog(e, BOOLEAN_REQUESTS[0])) as fn(&mut Editor),
        ),
        (
            "SUBTRACT",
            &["SU"][..],
            (|l| ms(l).cmd_subtract) as fn(Lang) -> &'static str,
            "-",
            (|e| show_dialog(e, BOOLEAN_REQUESTS[1])) as fn(&mut Editor),
        ),
        (
            "INTERSECT",
            &["IN"][..],
            (|l| ms(l).cmd_intersect) as fn(Lang) -> &'static str,
            "I",
            (|e| show_dialog(e, BOOLEAN_REQUESTS[2])) as fn(&mut Editor),
        ),
        (
            "MEASURE3D",
            &[][..],
            (|l| ms(l).cmd_measure3d) as fn(Lang) -> &'static str,
            "M3",
            (|e| show_dialog(e, INSPECT_REQUESTS[0])) as fn(&mut Editor),
        ),
        (
            "EXPORTSTL",
            &[][..],
            (|l| ms(l).cmd_export_stl) as fn(Lang) -> &'static str,
            "STL",
            (|e| show_dialog(e, INSPECT_REQUESTS[1])) as fn(&mut Editor),
        ),
        (
            "EXPORTOBJ",
            &[][..],
            (|l| ms(l).cmd_export_obj) as fn(Lang) -> &'static str,
            "OBJ",
            (|e| show_dialog(e, INSPECT_REQUESTS[2])) as fn(&mut Editor),
        ),
        (
            "EXPORTSTEP",
            &[][..],
            (|l| ms(l).cmd_export_step) as fn(Lang) -> &'static str,
            "STEP",
            (|e| show_dialog(e, INSPECT_REQUESTS[3])) as fn(&mut Editor),
        ),
    ] {
        r.add(CommandSpec {
            name,
            aliases,
            label,
            icon,
            tab: Some(RibbonTab::Model),
            group: if name.starts_with("EXPORT") {
                "export3d"
            } else {
                "solid"
            },
            kind: CommandKind::Action(action),
        });
    }
    r.add_ui_hook(boolean_ui);
    r.add_ui_hook(inspect_ui);
}

fn boolean_label(kind: BooleanKind, lang: Lang) -> &'static str {
    match kind {
        BooleanKind::Union => ms(lang).cmd_union,
        BooleanKind::Subtract => ms(lang).cmd_subtract,
        BooleanKind::Intersect => ms(lang).cmd_intersect,
    }
}

#[derive(Clone)]
pub(super) struct BooleanDialog {
    snapshot: Snapshot,
    edit: Option<FeatureId>,
    pub(super) target: Option<BodyRef>,
    pub(super) tools: Vec<BodyRef>,
    pub(super) kind: BooleanKind,
    pub(super) keep_tools: bool,
    inputs: RegenResult,
    error: Option<String>,
}

impl BooleanDialog {
    pub(super) fn new(
        ed: &Editor,
        kind: BooleanKind,
        edit: Option<FeatureId>,
    ) -> Result<Self, String> {
        let inputs = input_bodies(ed, edit)?;
        if inputs.bodies.len() < 2 {
            return Err(ed
                .lang
                .pick(
                    "布尔运算需要至少两个可用实体。",
                    "A boolean needs at least two available bodies.",
                )
                .into());
        }
        let (target, tools, keep_tools) = match edit.and_then(|id| ed.doc.part.feature(id)) {
            Some(Feature {
                kind:
                    FeatureKind::Boolean {
                        target,
                        tools,
                        keep_tools,
                        ..
                    },
                ..
            }) => (Some(*target), tools.clone(), *keep_tools),
            _ => (None, Vec::new(), false),
        };
        Ok(Self {
            snapshot: Snapshot::new(ed),
            edit,
            target,
            tools,
            kind,
            keep_tools,
            inputs,
            error: None,
        })
    }

    pub(super) fn apply(&self, ed: &mut Editor) -> Result<FeatureId, String> {
        self.snapshot.check(ed)?;
        let inputs = input_bodies(ed, self.edit)?;
        let target = self
            .target
            .filter(|t| inputs.body(*t).is_some())
            .ok_or_else(|| {
                ed.lang
                    .pick("请选择有效的目标实体。", "Select an available target body.")
                    .to_owned()
            })?;
        if self.tools.is_empty() {
            return Err(ms(ed.lang).no_tools.into());
        }
        let mut seen = std::collections::HashSet::new();
        if self
            .tools
            .iter()
            .any(|tool| *tool == target || inputs.body(*tool).is_none() || !seen.insert(*tool))
        {
            return Err(ed
                .lang
                .pick(
                    "工具实体须存在、互不重复，且不能是目标实体。",
                    "Tool bodies must exist, be distinct, and differ from the target.",
                )
                .into());
        }
        let label = match self.kind {
            BooleanKind::Union => "UNION",
            BooleanKind::Subtract => "SUBTRACT",
            BooleanKind::Intersect => "INTERSECT",
        };
        commit_feature(
            ed,
            &self.snapshot,
            self.edit,
            FeatureKind::Boolean {
                target,
                tools: self.tools.clone(),
                kind: self.kind,
                keep_tools: self.keep_tools,
            },
            label,
            boolean_label(self.kind, ed.lang),
        )
    }
}

pub(super) fn open_edit(ctx: &egui::Context, ed: &mut Editor, id: FeatureId) {
    let Some(Feature {
        kind: FeatureKind::Boolean { kind, .. },
        ..
    }) = ed.doc.part.feature(id)
    else {
        return;
    };
    match BooleanDialog::new(ed, *kind, Some(id)) {
        Ok(dialog) => {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(BOOLEAN_STATE), dialog));
        }
        Err(error) => ed.error(error),
    }
}

pub(super) fn boolean_ui(ctx: &egui::Context, ed: &mut Editor) {
    let state_id = egui::Id::new(BOOLEAN_STATE);
    let mut dialog = take_state::<BooleanDialog>(ctx, state_id);
    for (index, request) in BOOLEAN_REQUESTS.iter().enumerate() {
        if ctx
            .data_mut(|d| d.remove_temp::<bool>(egui::Id::new(request)))
            .unwrap_or(false)
        {
            if dialog.take().is_some() {
                cancelled(ed);
            }
            let kind = [
                BooleanKind::Union,
                BooleanKind::Subtract,
                BooleanKind::Intersect,
            ][index];
            match BooleanDialog::new(ed, kind, None) {
                Ok(value) => dialog = Some(value),
                Err(error) => ed.error(error),
            }
        }
    }
    let Some(mut dialog) = dialog else {
        return;
    };
    if let Err(error) = dialog.snapshot.check(ed) {
        ed.error(error);
        return;
    }
    let lang = ed.lang;
    let s = ms(lang);
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let size = window_size(ctx);
    egui::Window::new(s.cmd_boolean)
        .id(egui::Id::new("modeling.boolean.window"))
        .open(&mut open)
        .min_width(80.0)
        .max_width(size.x)
        .max_height(size.y)
        .default_width(370.0_f32.min(size.x))
        .constrain_to(ctx.content_rect())
        .vscroll(true)
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(s.operation);
                for kind in [
                    BooleanKind::Union,
                    BooleanKind::Subtract,
                    BooleanKind::Intersect,
                ] {
                    ui.selectable_value(&mut dialog.kind, kind, boolean_label(kind, lang));
                }
            });
            ui.label(s.target_body);
            body_picker(
                ui,
                "boolean_target",
                &mut dialog.target,
                &dialog.snapshot.part,
                &dialog.inputs,
                lang,
            );
            // A target change must not leave the same body selected as a tool.
            dialog.tools.retain(|tool| Some(*tool) != dialog.target);
            ui.separator();
            ui.label(s.tools);
            for body in &dialog.inputs.bodies {
                if Some(body.id) == dialog.target {
                    continue;
                }
                let mut selected = dialog.tools.contains(&body.id);
                if ui
                    .checkbox(&mut selected, body_name(&dialog.snapshot.part, body))
                    .changed()
                {
                    if selected {
                        dialog.tools.push(body.id);
                    } else {
                        dialog.tools.retain(|id| *id != body.id);
                    }
                }
            }
            ui.checkbox(&mut dialog.keep_tools, s.keep_tools);
            error_ui(ui, &dialog.error);
            ui.horizontal(|ui| {
                apply = ui.button(s.ok).clicked();
                cancel = ui.button(s.cancel).clicked();
            });
        });
    cancel |= !open || cancel_requested(ctx);
    if cancel {
        cancelled(ed);
        return;
    }
    if apply {
        match dialog.apply(ed) {
            Ok(_) => return,
            Err(error) => {
                ed.error(&error);
                dialog.error = Some(error);
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(state_id, dialog));
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InspectKind {
    Measure,
    Stl,
    Obj,
    Step,
}

impl InspectKind {
    fn label(self, lang: Lang) -> &'static str {
        match self {
            Self::Measure => ms(lang).cmd_measure3d,
            Self::Stl => ms(lang).cmd_export_stl,
            Self::Obj => ms(lang).cmd_export_obj,
            Self::Step => ms(lang).cmd_export_step,
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Measure => "",
            Self::Stl => "stl",
            Self::Obj => "obj",
            Self::Step => "step",
        }
    }
}

#[derive(Clone)]
pub(super) struct InspectDialog {
    snapshot: Snapshot,
    kind: InspectKind,
    pub(super) target: Option<BodyRef>,
    pub(super) all: bool,
    pub(super) filename: String,
    inputs: RegenResult,
    measurements: Vec<String>,
    error: Option<String>,
}

impl InspectDialog {
    pub(super) fn new(ed: &Editor, kind: InspectKind) -> Result<Self, String> {
        let inputs = checked_regen(&ed.doc.part, ed.lang)?;
        if inputs.bodies.is_empty() {
            return Err(if kind == InspectKind::Measure {
                ms(ed.lang).measure_none
            } else {
                ms(ed.lang).export_none
            }
            .into());
        }
        Ok(Self {
            snapshot: Snapshot::new(ed),
            kind,
            target: None,
            all: kind != InspectKind::Measure,
            filename: "model".into(),
            inputs,
            measurements: Vec::new(),
            error: None,
        })
    }

    fn bodies(&self, ed: &Editor) -> Result<(RegenResult, Vec<Body>), String> {
        self.snapshot.check(ed)?;
        let result = checked_regen(&ed.doc.part, ed.lang)?;
        let bodies = if self.all {
            result.bodies.clone()
        } else {
            self.target
                .and_then(|id| result.body(id))
                .cloned()
                .into_iter()
                .collect()
        };
        if bodies.is_empty() {
            return Err(ms(ed.lang).no_bodies_for_op.into());
        }
        Ok((result, bodies))
    }

    pub(super) fn export_bytes(&self, ed: &Editor) -> Result<(String, Vec<u8>), String> {
        let (_, bodies) = self.bodies(ed)?;
        let name = self.filename.trim();
        if name.is_empty()
            || name.len() > 128
            || name.ends_with('.')
            || name
                .chars()
                .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        {
            return Err(ed
                .lang
                .pick(
                    "文件名无效，请只输入名称，不含路径或扩展名。",
                    "Invalid file name. Enter a name without a path or extension.",
                )
                .into());
        }
        let bytes = match self.kind {
            InspectKind::Stl => wcad_solid::to_stl(&bodies),
            InspectKind::Obj => wcad_solid::to_obj(&bodies).map(String::into_bytes),
            InspectKind::Step => wcad_solid::to_step(&bodies).map(String::into_bytes),
            InspectKind::Measure => return Err(ms(ed.lang).export_failed.into()),
        }
        .map_err(|e| fmt(ms(ed.lang).export_failed, &[&e]))?;
        if bytes.is_empty() {
            return Err(ms(ed.lang).export_none.into());
        }
        Ok((format!("{name}.{}", self.kind.extension()), bytes))
    }

    pub(super) fn apply(&mut self, ed: &mut Editor) -> Result<(), String> {
        let (result, bodies) = self.bodies(ed)?;
        if self.kind == InspectKind::Measure {
            let mut lines = Vec::new();
            let unit = ed.doc.meta.units.suffix();
            for body in &bodies {
                let m = wcad_solid::mass_properties(body).map_err(|e| e.to_string())?;
                if !m.volume.is_finite() || !m.area.is_finite() || !m.centroid.is_finite() {
                    return Err(bounds_message(ed.lang));
                }
                lines.push(fmt(
                    ms(ed.lang).measure_result,
                    &[
                        &body_name(&ed.doc.part, body),
                        &format!("{:.6} {unit}^3", m.volume),
                        &format!("{:.6} {unit}^2", m.area),
                        &format!(
                            "({:.5}, {:.5}, {:.5}) - ({:.5}, {:.5}, {:.5}) {unit}",
                            m.bbox.min.x,
                            m.bbox.min.y,
                            m.bbox.min.z,
                            m.bbox.max.x,
                            m.bbox.max.y,
                            m.bbox.max.z
                        ),
                        &format!(
                            "({:.5}, {:.5}, {:.5}) {unit}",
                            m.centroid.x, m.centroid.y, m.centroid.z
                        ),
                    ],
                ));
            }
            for line in &lines {
                ed.info(line);
            }
            self.measurements = lines;
        } else {
            let (name, bytes) = self.export_bytes(ed)?;
            ed.info(format!(
                "{}: {name}",
                ed.lang.pick("已请求保存", "Save requested")
            ));
            ed.requests.push(AppRequest::SaveBytes { name, bytes });
        }
        warnings(ed, &result);
        Ok(())
    }
}

pub(super) fn inspect_ui(ctx: &egui::Context, ed: &mut Editor) {
    let state_id = egui::Id::new(INSPECT_STATE);
    let mut dialog = take_state::<InspectDialog>(ctx, state_id);
    for (index, request) in INSPECT_REQUESTS.iter().enumerate() {
        if ctx
            .data_mut(|d| d.remove_temp::<bool>(egui::Id::new(request)))
            .unwrap_or(false)
        {
            if dialog.take().is_some() {
                cancelled(ed);
            }
            let kind = [
                InspectKind::Measure,
                InspectKind::Stl,
                InspectKind::Obj,
                InspectKind::Step,
            ][index];
            match InspectDialog::new(ed, kind) {
                Ok(value) => dialog = Some(value),
                Err(error) => ed.error(error),
            }
        }
    }
    let Some(mut dialog) = dialog else {
        return;
    };
    if let Err(error) = dialog.snapshot.check(ed) {
        ed.error(error);
        return;
    }
    let lang = ed.lang;
    let s = ms(lang);
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let size = window_size(ctx);
    egui::Window::new(dialog.kind.label(lang)).id(egui::Id::new("modeling.inspect.window")).open(&mut open)
        .min_width(80.0).max_width(size.x).max_height(size.y)
        .default_width(420.0_f32.min(size.x)).constrain_to(ctx.content_rect()).vscroll(true).show(ctx, |ui| {
            let mut selection_changed = ui.checkbox(&mut dialog.all, lang.pick("全部活动实体", "All active bodies")).changed();
            let old_target = dialog.target;
            if !dialog.all {
                body_picker(ui, "inspect_body", &mut dialog.target, &dialog.snapshot.part, &dialog.inputs, lang);
            }
            selection_changed |= old_target != dialog.target;
            if selection_changed { dialog.measurements.clear(); }
            if dialog.kind == InspectKind::Measure {
                ui.weak(lang.pick("测量由细分网格计算，为近似值。", "Measurements are approximate, computed from a fine tessellation."));
            } else {
                ui.horizontal(|ui| {
                    ui.label(lang.pick("文件名", "File name"));
                    ui.add(egui::TextEdit::singleline(&mut dialog.filename).desired_width((ui.available_width() - 60.0).max(40.0)));
                    ui.label(format!(".{}", dialog.kind.extension()));
                });
                ui.weak(lang.pick("导出不改变文档。STL/OBJ 为网格；STEP 仅支持精确实体，网格回退实体会明确报错。", "Export does not change the document. STL/OBJ are meshes; STEP requires exact bodies and rejects mesh-only fallbacks."));
            }
            for body in &dialog.inputs.bodies {
                if (dialog.all || dialog.target == Some(body.id)) && let Some(reason) = &body.mesh_only_reason {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("[!] {}: {reason}", body_name(&dialog.snapshot.part, body)));
                }
            }
            for line in &dialog.measurements { ui.label(line); }
            error_ui(ui, &dialog.error);
            ui.horizontal(|ui| { apply = ui.button(s.ok).clicked(); cancel = ui.button(s.cancel).clicked(); });
        });
    cancel |= !open || cancel_requested(ctx);
    if cancel {
        cancelled(ed);
        return;
    }
    if apply {
        match dialog.apply(ed) {
            Ok(()) if dialog.kind != InspectKind::Measure => return,
            Ok(()) => dialog.error = None,
            Err(error) => {
                ed.error(&error);
                dialog.error = Some(error);
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(state_id, dialog));
}
