//! Parametric solid dialogs. Drafts live in the egui context, never in global model state.

mod operations;
mod primitives;
mod profiles;
#[path = "../i18n/modeling.rs"]
#[allow(dead_code)]
mod strings;
mod tree;

#[cfg(test)]
mod tests;

use wcad_doc::{BodyOp, BodyRef, Feature, FeatureId, FeatureKind, Part};
use wcad_solid::{FeatureStatus, RegenCache, RegenResult};

use crate::commands::CommandRegistry;
use crate::editor::{AppRequest, Editor, Workspace};
use crate::i18n::{Lang, core, fmt};
use strings::ms;

/// Register after the built-in panels so the editable model tree replaces the read-only tree.
pub fn register(r: &mut CommandRegistry) {
    primitives::register(r);
    operations::register(r);
    tree::register(r);
    profiles::register(r);
}

const MAX_COORD: f64 = 1.0e6;
const MIN_SIZE: f64 = 1.0e-5;

fn show_dialog(ed: &mut Editor, id: &'static str) {
    ed.requests
        .push(AppRequest::SetWorkspace(Workspace::Modeling));
    ed.requests.push(AppRequest::ShowDialog(id));
}

fn stale_message(lang: Lang) -> &'static str {
    lang.pick(
        "文档或模型已更改。已取消旧窗口，请重新打开。",
        "The document or model changed. The old dialog was cancelled; reopen it.",
    )
}

#[derive(Clone)]
struct Snapshot {
    part: Part,
    generation: u64,
}

impl Snapshot {
    fn new(ed: &Editor) -> Self {
        Self {
            part: ed.doc.part.clone(),
            generation: ed.document_generation(),
        }
    }

    fn check(&self, ed: &Editor) -> Result<(), String> {
        if self.generation != ed.document_generation() || self.part != ed.doc.part {
            Err(stale_message(ed.lang).into())
        } else {
            Ok(())
        }
    }
}

/// Regeneration is not fallible as a whole: inspect every status, not just the resulting bodies.
fn checked_regen(part: &Part, lang: Lang) -> Result<RegenResult, String> {
    let result = wcad_solid::regenerate(part, &mut RegenCache::new());
    for (id, status) in &result.feature_status {
        if let FeatureStatus::Error(message) = status {
            let name = feature_name(part, *id);
            return Err(format!("{name}: {message}"));
        }
    }
    for body in &result.bodies {
        let bbox = body.bbox();
        if !bbox.min.is_finite()
            || !bbox.max.is_finite()
            || bbox.is_empty()
            || bbox.min.abs().max_element() > MAX_COORD
            || bbox.max.abs().max_element() > MAX_COORD
            || !body.approx_volume().is_finite()
            || body.approx_volume() <= 0.0
            || part.feature(body.id.0).is_none()
            || part.feature(body.source).is_none()
        {
            return Err(bounds_message(lang));
        }
    }
    Ok(result)
}

fn bounds_message(lang: Lang) -> String {
    lang.pick(
        format!("几何参数无效。尺寸须至少 {MIN_SIZE}，世界坐标须在 +/-{MAX_COORD} 内，且必须为有限数值。"),
        format!("Invalid geometry. Sizes must be at least {MIN_SIZE}, world coordinates within +/-{MAX_COORD}, and all values finite."),
    )
}

fn feature_name(part: &Part, id: FeatureId) -> String {
    match part.feature(id) {
        Some(f) if !f.name.is_empty() => format!("{} [#{}]", f.name, id.0),
        Some(f) => format!("{} [#{}]", f.kind.type_key(), id.0),
        None => format!("#{}", id.0),
    }
}

fn body_name(part: &Part, body: &wcad_solid::Body) -> String {
    let mut name = feature_name(part, body.id.0);
    if body.source != body.id.0 {
        name.push_str(&format!(" / {}", feature_name(part, body.source)));
    }
    if !body.is_exact() {
        name.push_str(" [mesh]");
    }
    name
}

fn warnings(ed: &mut Editor, result: &RegenResult) {
    for (id, status) in &result.feature_status {
        if let FeatureStatus::Warning(message) = status {
            ed.info(format!(
                "[!] {}: {message}",
                feature_name(&ed.doc.part, *id)
            ));
        }
    }
    for body in &result.bodies {
        if let Some(reason) = &body.mesh_only_reason {
            ed.info(format!("[!] {}: {reason}", body_name(&ed.doc.part, body)));
        }
    }
}

/// Bodies available immediately BEFORE the edited feature, rather than at the end of history.
fn input_bodies(ed: &Editor, edit: Option<FeatureId>) -> Result<RegenResult, String> {
    let mut prefix = ed.doc.part.clone();
    if let Some(id) = edit {
        let index = prefix
            .index_of(id)
            .ok_or_else(|| stale_message(ed.lang).to_owned())?;
        if prefix.features[index].suppressed || prefix.rollback.is_some_and(|end| index >= end) {
            return Err(ed
                .lang
                .pick(
                    "请先解除压缩并回退到末尾。",
                    "Unsuppress and roll to the end first.",
                )
                .into());
        }
        prefix.features.truncate(index);
        prefix.rollback = None;
    } else if prefix.rollback.is_some() {
        return Err(ed
            .lang
            .pick(
                "请先将回退控制棒移到末尾。",
                "Roll to the end before adding a feature.",
            )
            .into());
    }
    checked_regen(&prefix, ed.lang)
}

fn validate_op(op: &BodyOp, inputs: &RegenResult, lang: Lang) -> Result<(), String> {
    match op {
        BodyOp::NewBody => Ok(()),
        BodyOp::Join { target } | BodyOp::Cut { target } | BodyOp::Intersect { target }
            if target.is_some_and(|id| inputs.body(id).is_some()) =>
        {
            Ok(())
        }
        _ => Err(lang
            .pick("请选择有效的目标实体。", "Select an available target body.")
            .into()),
    }
}

fn commit_feature(
    ed: &mut Editor,
    snapshot: &Snapshot,
    edit: Option<FeatureId>,
    kind: FeatureKind,
    label: &str,
    default_name: &str,
) -> Result<FeatureId, String> {
    snapshot.check(ed)?;
    let mut candidate = snapshot.part.clone();
    let id = if let Some(id) = edit {
        let f = candidate
            .feature_mut(id)
            .ok_or_else(|| stale_message(ed.lang).to_owned())?;
        let creates_body = |kind: &FeatureKind| {
            matches!(
                kind,
                FeatureKind::Primitive {
                    op: BodyOp::NewBody,
                    ..
                } | FeatureKind::Extrude {
                    op: BodyOp::NewBody,
                    ..
                } | FeatureKind::Revolve {
                    op: BodyOp::NewBody,
                    ..
                }
            )
        };
        if creates_body(&f.kind)
            && !creates_body(&kind)
            && !tree::dependents(&snapshot.part, id).is_empty()
        {
            return Err(ed.lang.pick(
                "此实体有依赖特征，不能将其新建操作改为合并、切除或相交。",
                "This body has dependents; its New Body operation cannot be changed to Join, Cut or Intersect.",
            ).into());
        }
        f.kind = kind;
        id
    } else {
        let id = ed.doc.ids().clone().feature();
        candidate.features.push(Feature {
            id,
            name: format!("{default_name} {}", id.0),
            suppressed: false,
            kind,
        });
        id
    };
    let result = checked_regen(&candidate, ed.lang)?;
    if result.status_of(id).is_none() {
        return Err(stale_message(ed.lang).into());
    }
    // No mutation, including allocator advancement, occurs until the entire candidate succeeds.
    ed.doc.transact(label, |tx| {
        tx.ids().bump_feature(id);
        *tx.part_mut() = candidate;
    });
    ed.pump();
    ed.requests
        .push(AppRequest::SetWorkspace(Workspace::Modeling));
    ed.requests.push(AppRequest::ZoomExtents);
    ed.info(fmt(
        if edit.is_some() {
            ms(ed.lang).feature_updated
        } else {
            ms(ed.lang).feature_created
        },
        &[&feature_name(&ed.doc.part, id)],
    ));
    warnings(ed, &result);
    Ok(id)
}

fn body_picker(
    ui: &mut egui::Ui,
    salt: &str,
    selected: &mut Option<BodyRef>,
    part: &Part,
    result: &RegenResult,
    lang: Lang,
) {
    let text = selected
        .and_then(|id| result.body(id))
        .map(|body| body_name(part, body))
        .unwrap_or_else(|| lang.pick("请选择", "Select...").to_owned());
    egui::ComboBox::from_id_salt(salt)
        .width(ui.available_width().clamp(80.0, 290.0))
        .truncate()
        .selected_text(text)
        .show_ui(ui, |ui| {
            ui.selectable_value(selected, None, lang.pick("请选择", "Select..."));
            for body in &result.bodies {
                ui.selectable_value(selected, Some(body.id), body_name(part, body));
            }
        });
}

fn op_ui(ui: &mut egui::Ui, op: &mut BodyOp, part: &Part, result: &RegenResult, lang: Lang) {
    let s = ms(lang);
    let (mut index, target) = match op {
        BodyOp::NewBody => (0, None),
        BodyOp::Join { target } => (1, *target),
        BodyOp::Cut { target } => (2, *target),
        BodyOp::Intersect { target } => (3, *target),
    };
    let labels = [s.op_new, s.op_join, s.op_cut, s.op_intersect];
    ui.horizontal_wrapped(|ui| {
        ui.label(s.operation);
        egui::ComboBox::from_id_salt("body_operation")
            .selected_text(labels[index])
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    ui.selectable_value(&mut index, i, *label);
                }
            });
    });
    *op = match index {
        1 => BodyOp::Join { target },
        2 => BodyOp::Cut { target },
        3 => BodyOp::Intersect { target },
        _ => BodyOp::NewBody,
    };
    match op {
        BodyOp::NewBody => {}
        BodyOp::Join { target } | BodyOp::Cut { target } | BodyOp::Intersect { target } => {
            ui.horizontal_wrapped(|ui| {
                ui.label(s.target_body);
                body_picker(ui, "primitive_target", target, part, result, lang);
            });
        }
    }
}

fn error_ui(ui: &mut egui::Ui, error: &Option<String>) {
    if let Some(message) = error {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

fn cancelled(ed: &mut Editor) {
    ed.info(core(ed.lang).cancelled);
}

pub(crate) const CANCEL_DIALOGS: &str = "modeling.cancel_dialogs";

fn cancel_requested(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.key_pressed(egui::Key::Escape))
        || ctx.data(|data| data.get_temp::<bool>(egui::Id::new(CANCEL_DIALOGS))) == Some(true)
}

fn take_state<T: Clone + Send + Sync + 'static>(ctx: &egui::Context, id: egui::Id) -> Option<T> {
    ctx.data_mut(|data| {
        let state = data.get_temp::<T>(id);
        data.remove::<T>(id);
        state
    })
}

fn window_size(ctx: &egui::Context) -> egui::Vec2 {
    let rect = ctx.content_rect();
    // Leave room for the title bar and frame even on a small touch viewport.
    egui::vec2(
        (rect.width() - 32.0).max(80.0),
        (rect.height() - 72.0).max(60.0),
    )
}
