//! Model history management. Destructive changes never leave dangling dependents.

use std::collections::{HashMap, HashSet};

use wcad_doc::{AxisRef, EdgeRef, PlaneRef};

use super::*;
use crate::panels::{Dock, PanelCx, PanelSpec};

const STATE: &str = "modeling.tree.dialog";
const SELECTION: &str = "modeling.tree.selection";

pub(super) fn register(r: &mut CommandRegistry) {
    r.add_panel(PanelSpec {
        id: "model_tree",
        title: |l| core(l).model_tree,
        dock: Dock::Left,
        ui: panel,
    });
    r.add_ui_hook(dialog_ui);
}

fn edge_refs(edge: &EdgeRef, refs: &mut Vec<FeatureId>) {
    refs.push(edge.body.0);
    refs.extend(edge.faces.iter().map(|face| face.feature));
}

fn plane_refs(plane: &PlaneRef, refs: &mut Vec<FeatureId>) {
    match plane {
        PlaneRef::Face(face) => {
            refs.push(face.body.0);
            refs.push(face.name.feature);
        }
        PlaneRef::Offset { base, .. } => plane_refs(base, refs),
        _ => {}
    }
}

fn axis_refs(axis: &AxisRef, refs: &mut Vec<FeatureId>) {
    match axis {
        AxisRef::SketchLine { sketch, .. } => refs.push(*sketch),
        AxisRef::Edge(edge) => edge_refs(edge, refs),
        _ => {}
    }
}

/// Include provenance as well as body identity: deleting an intermediate cut can invalidate a
/// later feature even though that later feature still refers to the original primitive's body id.
/// Suppressed/rolled-back dependents are intentionally included so restoring them remains safe.
pub(super) fn dependents(part: &Part, removed: FeatureId) -> Vec<FeatureId> {
    let mut deps: HashMap<FeatureId, HashSet<FeatureId>> = HashMap::new();
    let mut sources: HashMap<FeatureId, FeatureId> = HashMap::new();
    let mut previous = Vec::new();
    let mut last = None;
    let mut result = Vec::new();
    for feature in &part.features {
        let mut refs = Vec::new();
        let mut outputs = Vec::new();
        match &feature.kind {
            FeatureKind::Sketch { plane, .. } => plane_refs(plane, &mut refs),
            FeatureKind::Extrude { profile, .. } => refs.push(profile.sketch),
            FeatureKind::Revolve { profile, axis, .. } => {
                refs.push(profile.sketch);
                axis_refs(axis, &mut refs);
            }
            FeatureKind::Fillet { edges, .. } | FeatureKind::Chamfer { edges, .. } => {
                for edge in edges {
                    edge_refs(edge, &mut refs);
                    outputs.push(edge.body.0);
                }
            }
            FeatureKind::Boolean { target, tools, .. } => {
                refs.push(target.0);
                refs.extend(tools.iter().map(|tool| tool.0));
                outputs.push(target.0);
            }
            FeatureKind::LinearPattern { body, .. } => {
                refs.push(body.0);
                outputs.push(body.0);
            }
            FeatureKind::CircularPattern { body, axis, .. } => {
                refs.push(body.0);
                axis_refs(axis, &mut refs);
                outputs.push(body.0);
            }
            FeatureKind::Mirror { body, plane, join } => {
                refs.push(body.0);
                plane_refs(plane, &mut refs);
                outputs.push(if *join { body.0 } else { feature.id });
            }
            FeatureKind::Primitive { .. } => {}
        }
        match &feature.kind {
            FeatureKind::Primitive { op, .. }
            | FeatureKind::Extrude { op, .. }
            | FeatureKind::Revolve { op, .. } => {
                match op {
                    BodyOp::NewBody => outputs.push(feature.id),
                    BodyOp::Join { target }
                    | BodyOp::Cut { target }
                    | BodyOp::Intersect { target } => {
                        if let Some(target) = target {
                            refs.push(target.0);
                            outputs.push(target.0);
                        } else {
                            // Legacy implicit targets cannot be safely resolved from static refs.
                            refs.extend(previous.iter().copied());
                            outputs.push(last.unwrap_or(feature.id));
                        }
                    }
                }
            }
            _ => {}
        }
        let provenance: Vec<_> = refs
            .iter()
            .filter_map(|id| sources.get(id).copied())
            .collect();
        refs.extend(provenance);
        let mut closure = HashSet::new();
        for reference in refs {
            closure.insert(reference);
            if let Some(ancestors) = deps.get(&reference) {
                closure.extend(ancestors);
            }
        }
        if feature.id != removed && closure.contains(&removed) {
            result.push(feature.id);
        }
        deps.insert(feature.id, closure);
        for body in outputs {
            sources.insert(body, feature.id);
            last = Some(body);
        }
        previous.push(feature.id);
    }
    result
}

#[derive(Clone)]
pub(super) enum Change {
    Rename(String),
    Suppress(bool),
    Delete,
}

impl Change {
    fn label(&self, lang: Lang) -> &'static str {
        match self {
            Self::Rename(_) => ms(lang).rename,
            Self::Suppress(true) => ms(lang).suppress,
            Self::Suppress(false) => ms(lang).unsuppress,
            Self::Delete => ms(lang).delete,
        }
    }
    fn removes_input(&self) -> bool {
        matches!(self, Self::Suppress(true) | Self::Delete)
    }
}

pub(super) fn apply_change(
    ed: &mut Editor,
    snapshot: &Snapshot,
    id: FeatureId,
    change: Change,
) -> Result<(), String> {
    snapshot.check(ed)?;
    let mut candidate = snapshot.part.clone();
    let index = candidate
        .index_of(id)
        .ok_or_else(|| stale_message(ed.lang).to_owned())?;
    if change.removes_input() {
        let dependents = dependents(&candidate, id);
        if !dependents.is_empty() {
            return Err(format!(
                "{}\n{}\n{}",
                ed.lang.pick(
                    "操作已拒绝。请先处理依赖特征。",
                    "Operation refused. Resolve dependent features first."
                ),
                ms(ed.lang).dependents,
                dependents
                    .iter()
                    .map(|id| feature_name(&candidate, *id))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    let label = change.label(ed.lang);
    match change {
        Change::Rename(name) => {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
                return Err(ed
                    .lang
                    .pick(
                        "名称须为 1 到 128 个可打印字符。",
                        "Use a name of 1 to 128 printable characters.",
                    )
                    .into());
            }
            candidate.features[index].name = name.to_owned();
        }
        Change::Suppress(value) => candidate.features[index].suppressed = value,
        Change::Delete => {
            candidate.features.remove(index);
            if let Some(end) = candidate.rollback.as_mut()
                && index < *end
            {
                *end -= 1;
            }
        }
    }
    let result = checked_regen(&candidate, ed.lang)?;
    ed.doc.transact(label, |tx| *tx.part_mut() = candidate);
    ed.pump();
    ed.info(format!("{label}: #{}", id.0));
    warnings(ed, &result);
    Ok(())
}

pub(super) fn roll_end(ed: &mut Editor) -> Result<(), String> {
    let mut candidate = ed.doc.part.clone();
    candidate.rollback = None;
    let result = checked_regen(&candidate, ed.lang)?;
    ed.doc.transact("ROLLEND", |tx| *tx.part_mut() = candidate);
    ed.pump();
    warnings(ed, &result);
    Ok(())
}

#[derive(Clone)]
struct Selection {
    snapshot: Snapshot,
    selected: Option<FeatureId>,
}

#[derive(Clone)]
struct Dialog {
    snapshot: Snapshot,
    id: FeatureId,
    change: Change,
    error: Option<String>,
}

fn open(ctx: &egui::Context, ed: &Editor, id: FeatureId, change: Change) {
    ctx.data_mut(|d| {
        d.insert_temp(
            egui::Id::new(STATE),
            Dialog {
                snapshot: Snapshot::new(ed),
                id,
                change,
                error: None,
            },
        )
    });
}

fn type_label(kind: &FeatureKind, lang: Lang) -> &'static str {
    let s = ms(lang);
    match kind {
        FeatureKind::Sketch { .. } => s.ft_sketch,
        FeatureKind::Extrude { .. } => s.ft_extrude,
        FeatureKind::Revolve { .. } => s.ft_revolve,
        FeatureKind::Fillet { .. } => s.ft_fillet,
        FeatureKind::Chamfer { .. } => s.ft_chamfer,
        FeatureKind::Primitive { .. } => s.ft_primitive,
        FeatureKind::Boolean { .. } => s.ft_boolean,
        FeatureKind::LinearPattern { .. } => s.ft_linear_pattern,
        FeatureKind::CircularPattern { .. } => s.ft_circular_pattern,
        FeatureKind::Mirror { .. } => s.ft_mirror,
    }
}

fn panel(ui: &mut egui::Ui, cx: &mut PanelCx<'_>) {
    let lang = cx.lang();
    let s = ms(lang);
    let ed = &mut *cx.editor;
    let key = egui::Id::new(SELECTION);
    let mut selection = take_state::<Selection>(ui.ctx(), key)
        .filter(|state| state.snapshot.check(ed).is_ok())
        .unwrap_or_else(|| Selection {
            snapshot: Snapshot::new(ed),
            selected: None,
        });
    if ed.doc.part.features.is_empty() {
        ui.weak(core(lang).no_features);
        return;
    }
    if ed.doc.part.rollback.is_some()
        && ui.button(s.roll_end).clicked()
        && let Err(error) = roll_end(ed)
    {
        ed.error(error);
    }
    let part = ed.doc.part.clone();
    egui::ScrollArea::vertical()
        .id_salt("modeling.tree.scroll")
        .max_height((ui.available_height() - 140.0).clamp(50.0, 400.0))
        .show(ui, |ui| {
            for (index, feature) in part.features.iter().enumerate() {
                let active = !feature.suppressed && part.rollback.is_none_or(|end| index < end);
                let status = cx.regen.status_of(feature.id);
                let (icon, color) = if !active {
                    ("[-]", ui.visuals().weak_text_color())
                } else {
                    match status {
                        Some(FeatureStatus::Error(_)) => ("[x]", ui.visuals().error_fg_color),
                        Some(FeatureStatus::Warning(_)) => ("[!]", ui.visuals().warn_fg_color),
                        _ => ("[+]", ui.visuals().text_color()),
                    }
                };
                let text = format!(
                    "{icon} {} ({})",
                    feature_name(&part, feature.id),
                    type_label(&feature.kind, lang)
                );
                let response = ui.selectable_label(
                    selection.selected == Some(feature.id),
                    egui::RichText::new(text).color(color),
                );
                if response.clicked() {
                    selection.selected = Some(feature.id);
                }
                if let Some(FeatureStatus::Warning(message) | FeatureStatus::Error(message)) =
                    status
                {
                    response.on_hover_text(message);
                }
            }
        });
    if let Some(feature) = selection.selected.and_then(|id| part.feature(id)) {
        ui.separator();
        ui.label(feature_name(&part, feature.id));
        let editable = matches!(
            feature.kind,
            FeatureKind::Primitive { .. } | FeatureKind::Boolean { .. }
        ) && !feature.suppressed
            && part
                .rollback
                .is_none_or(|end| part.index_of(feature.id).is_some_and(|i| i < end));
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(editable, egui::Button::new(s.edit))
                .clicked()
            {
                match feature.kind {
                    FeatureKind::Primitive { .. } => {
                        primitives::open_edit(ui.ctx(), ed, feature.id)
                    }
                    FeatureKind::Boolean { .. } => operations::open_edit(ui.ctx(), ed, feature.id),
                    _ => {}
                }
            }
            if ui.button(s.rename).clicked() {
                open(
                    ui.ctx(),
                    ed,
                    feature.id,
                    Change::Rename(feature.name.clone()),
                );
            }
            let toggle = Change::Suppress(!feature.suppressed);
            if ui.button(toggle.label(lang)).clicked() {
                open(ui.ctx(), ed, feature.id, toggle);
            }
            if ui.button(s.delete).clicked() {
                open(ui.ctx(), ed, feature.id, Change::Delete);
            }
        });
        if !editable {
            ui.weak(lang.pick("参数编辑仅支持活动的基本体和布尔特征；其他特征仍可重命名、压缩或删除。", "Parameter editing supports active primitive and boolean features; other features can still be renamed, suppressed or deleted."));
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(key, selection));
}

pub(super) fn dialog_ui(ctx: &egui::Context, ed: &mut Editor) {
    let key = egui::Id::new(STATE);
    let Some(mut dialog) = take_state::<Dialog>(ctx, key) else {
        return;
    };
    if let Err(error) = dialog.snapshot.check(ed) {
        ed.error(error);
        return;
    }
    let s = ms(ed.lang);
    let title = dialog.change.label(ed.lang);
    let blockers = if dialog.change.removes_input() {
        dependents(&dialog.snapshot.part, dialog.id)
    } else {
        Vec::new()
    };
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let size = window_size(ctx);
    egui::Window::new(title).id(egui::Id::new("modeling.tree.window")).open(&mut open)
        .min_width(80.0).max_width(size.x).max_height(size.y)
        .default_width(340.0_f32.min(size.x)).constrain_to(ctx.content_rect()).vscroll(true).show(ctx, |ui| {
            let name = feature_name(&dialog.snapshot.part, dialog.id);
            match &mut dialog.change {
                Change::Rename(value) => { ui.label(&name); ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width())); },
                Change::Delete => { ui.label(fmt(s.delete_confirm, &[&name])); },
                Change::Suppress(_) => { ui.label(format!("{title}: {name}")); },
            }
            if !blockers.is_empty() {
                ui.colored_label(ui.visuals().warn_fg_color, ed.lang.pick("操作已拒绝。请先处理依赖特征（包括已压缩特征）。", "Operation refused. Resolve dependents first, including suppressed features."));
                for id in &blockers { ui.label(feature_name(&dialog.snapshot.part, *id)); }
            }
            error_ui(ui, &dialog.error);
            ui.horizontal(|ui| { apply = ui.add_enabled(blockers.is_empty(), egui::Button::new(s.ok)).clicked(); cancel = ui.button(s.cancel).clicked(); });
        });
    cancel |= !open || cancel_requested(ctx);
    if cancel {
        cancelled(ed);
        return;
    }
    if apply {
        match apply_change(ed, &dialog.snapshot, dialog.id, dialog.change.clone()) {
            Ok(()) => return,
            Err(error) => {
                ed.error(&error);
                dialog.error = Some(error);
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(key, dialog));
}
