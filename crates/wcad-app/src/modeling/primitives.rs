//! Editable primitives, including explicit target selection for combining operations.

use wcad_doc::Primitive;
use wcad_math::{DAffine3, DVec3};

use super::*;
use crate::commands::{CommandKind, CommandSpec, RibbonTab};

pub(super) const REQUESTS: [&str; 5] = [
    "modeling.primitive.box",
    "modeling.primitive.cylinder",
    "modeling.primitive.sphere",
    "modeling.primitive.cone",
    "modeling.primitive.torus",
];
const STATE: &str = "modeling.primitive.state";

pub(super) fn register(r: &mut CommandRegistry) {
    for (name, aliases, label, icon, action) in [
        (
            "BOX",
            &[][..],
            (|l| ms(l).cmd_box) as fn(Lang) -> &'static str,
            "B",
            (|e| show_dialog(e, REQUESTS[0])) as fn(&mut Editor),
        ),
        (
            "CYLINDER",
            &["CYL"][..],
            (|l| ms(l).cmd_cylinder) as fn(Lang) -> &'static str,
            "CY",
            (|e| show_dialog(e, REQUESTS[1])) as fn(&mut Editor),
        ),
        (
            "SPHERE",
            &["SPH"][..],
            (|l| ms(l).cmd_sphere) as fn(Lang) -> &'static str,
            "SP",
            (|e| show_dialog(e, REQUESTS[2])) as fn(&mut Editor),
        ),
        (
            "CONE",
            &[][..],
            (|l| ms(l).cmd_cone) as fn(Lang) -> &'static str,
            "CN",
            (|e| show_dialog(e, REQUESTS[3])) as fn(&mut Editor),
        ),
        (
            "TORUS",
            &["TOR"][..],
            (|l| ms(l).cmd_torus) as fn(Lang) -> &'static str,
            "TO",
            (|e| show_dialog(e, REQUESTS[4])) as fn(&mut Editor),
        ),
    ] {
        r.add(CommandSpec {
            name,
            aliases,
            label,
            icon,
            tab: Some(RibbonTab::Model),
            group: "primitives",
            kind: CommandKind::Action(action),
        });
    }
    r.add_ui_hook(ui_hook);
}

#[derive(Clone)]
pub(super) struct Dialog {
    snapshot: Snapshot,
    edit: Option<FeatureId>,
    pub(super) shape: Primitive,
    pub(super) placement: DAffine3,
    pub(super) op: BodyOp,
    inputs: RegenResult,
    error: Option<String>,
}

impl Dialog {
    pub(super) fn new(
        ed: &Editor,
        shape: Primitive,
        edit: Option<FeatureId>,
    ) -> Result<Self, String> {
        let inputs = input_bodies(ed, edit)?;
        let (placement, op) = match edit.and_then(|id| ed.doc.part.feature(id)) {
            Some(Feature {
                kind: FeatureKind::Primitive { placement, op, .. },
                ..
            }) => (*placement, op.clone()),
            _ => (DAffine3::IDENTITY, BodyOp::NewBody),
        };
        Ok(Self {
            snapshot: Snapshot::new(ed),
            edit,
            shape,
            placement,
            op,
            inputs,
            error: None,
        })
    }

    pub(super) fn apply(&self, ed: &mut Editor) -> Result<FeatureId, String> {
        self.snapshot.check(ed)?;
        validate_shape(&self.shape, &self.placement, ed.lang)?;
        let inputs = input_bodies(ed, self.edit)?;
        validate_op(&self.op, &inputs, ed.lang)?;
        commit_feature(
            ed,
            &self.snapshot,
            self.edit,
            FeatureKind::Primitive {
                shape: self.shape.clone(),
                placement: self.placement,
                op: self.op.clone(),
            },
            command(&self.shape),
            label(&self.shape, ed.lang),
        )
    }
}

fn command(shape: &Primitive) -> &'static str {
    match shape {
        Primitive::Box { .. } => "BOX",
        Primitive::Cylinder { .. } => "CYLINDER",
        Primitive::Sphere { .. } => "SPHERE",
        Primitive::Cone { .. } => "CONE",
        Primitive::Torus { .. } => "TORUS",
    }
}

fn label(shape: &Primitive, lang: Lang) -> &'static str {
    let s = ms(lang);
    match shape {
        Primitive::Box { .. } => s.cmd_box,
        Primitive::Cylinder { .. } => s.cmd_cylinder,
        Primitive::Sphere { .. } => s.cmd_sphere,
        Primitive::Cone { .. } => s.cmd_cone,
        Primitive::Torus { .. } => s.cmd_torus,
    }
}

fn default_shape(index: usize) -> Primitive {
    match index {
        1 => Primitive::Cylinder {
            radius: 5.0,
            height: 10.0,
        },
        2 => Primitive::Sphere { radius: 5.0 },
        3 => Primitive::Cone {
            radius1: 5.0,
            radius2: 0.0,
            height: 10.0,
        },
        4 => Primitive::Torus {
            major: 10.0,
            minor: 2.0,
        },
        _ => Primitive::Box {
            size: DVec3::splat(10.0),
        },
    }
}

/// Check BEFORE entering the kernel (wasm cannot unwind a kernel panic).
fn validate_shape(shape: &Primitive, placement: &DAffine3, lang: Lang) -> Result<(), String> {
    let positive = |v: f64| v.is_finite() && (MIN_SIZE..=MAX_COORD).contains(&v);
    let (lo, hi, small) = match *shape {
        Primitive::Box { size } if positive(size.x) && positive(size.y) && positive(size.z) => {
            (DVec3::ZERO, size, size.min_element())
        }
        Primitive::Cylinder { radius, height } if positive(radius) && positive(height) => (
            DVec3::new(-radius, -radius, 0.0),
            DVec3::new(radius, radius, height),
            radius.min(height),
        ),
        Primitive::Sphere { radius } if positive(radius) => {
            (-DVec3::splat(radius), DVec3::splat(radius), radius)
        }
        Primitive::Cone {
            radius1,
            radius2,
            height,
        } if positive(height)
            && (radius1 == 0.0 || positive(radius1))
            && (radius2 == 0.0 || positive(radius2))
            && positive(radius1.max(radius2)) =>
        {
            let r = radius1.max(radius2);
            let min_r = if radius1 == 0.0 || radius2 == 0.0 {
                r
            } else {
                radius1.min(radius2)
            };
            (
                DVec3::new(-r, -r, 0.0),
                DVec3::new(r, r, height),
                min_r.min(height),
            )
        }
        Primitive::Torus { major, minor }
            if positive(major) && positive(minor) && positive(major - minor) =>
        {
            let r = major + minor;
            (
                DVec3::new(-r, -r, -minor),
                DVec3::new(r, r, minor),
                minor.min(major - minor),
            )
        }
        Primitive::Torus { .. } => return Err(lang.pick(
            "圆环半径须满足 0 < 截面半径 < 主半径，且尺寸须在有效范围内。",
            "Torus radii must satisfy 0 < minor < major, within the supported size range.",
        ).into()),
        Primitive::Cone { .. } => return Err(lang.pick(
            "圆锥高度须为正；两个半径非负且至少一个为正，尺寸须在有效范围内。",
            "Cone height must be positive; radii must be non-negative with at least one positive, within the supported size range.",
        ).into()),
        _ => return Err(bounds_message(lang)),
    };
    // Preserve an imported rigid placement, but never silently discard rotation or accept scale/shear.
    let m = placement.matrix3;
    let rigid = [m.x_axis, m.y_axis, m.z_axis]
        .iter()
        .all(|v| v.is_finite() && (v.length_squared() - 1.0).abs() < 1e-8)
        && m.x_axis.dot(m.y_axis).abs() < 1e-8
        && m.x_axis.dot(m.z_axis).abs() < 1e-8
        && m.y_axis.dot(m.z_axis).abs() < 1e-8
        && (m.determinant() - 1.0).abs() < 1e-8;
    if !rigid {
        return Err(lang
            .pick(
                "参数编辑仅支持刚性放置，不支持带缩放或剪切的放置矩阵。",
                "Parameter editing supports rigid placements, not scaled or sheared placements.",
            )
            .into());
    }
    if !placement.translation.is_finite() || (hi - lo).max_element() / small > 1e6 {
        return Err(bounds_message(lang));
    }
    for bits in 0..8 {
        let p = DVec3::new(
            if bits & 1 == 0 { lo.x } else { hi.x },
            if bits & 2 == 0 { lo.y } else { hi.y },
            if bits & 4 == 0 { lo.z } else { hi.z },
        );
        let p = placement.transform_point3(p);
        if !p.is_finite() || p.abs().max_element() > MAX_COORD {
            return Err(bounds_message(lang));
        }
    }
    Ok(())
}

pub(super) fn open_edit(ctx: &egui::Context, ed: &mut Editor, id: FeatureId) {
    let Some(Feature {
        kind: FeatureKind::Primitive { shape, .. },
        ..
    }) = ed.doc.part.feature(id)
    else {
        return;
    };
    match Dialog::new(ed, shape.clone(), Some(id)) {
        Ok(dialog) => {
            ctx.data_mut(|d| d.insert_temp(egui::Id::new(STATE), dialog));
        }
        Err(error) => ed.error(error),
    }
}

fn number(ui: &mut egui::Ui, text: &str, value: &mut f64) {
    ui.label(text);
    ui.add(egui::DragValue::new(value).speed(0.1).max_decimals(8));
    ui.end_row();
}

pub(super) fn ui_hook(ctx: &egui::Context, ed: &mut Editor) {
    let state_id = egui::Id::new(STATE);
    let mut dialog = take_state::<Dialog>(ctx, state_id);
    for (index, request) in REQUESTS.iter().enumerate() {
        if ctx
            .data_mut(|d| d.remove_temp::<bool>(egui::Id::new(request)))
            .unwrap_or(false)
        {
            if dialog.take().is_some() {
                cancelled(ed);
            }
            match Dialog::new(ed, default_shape(index), None) {
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
    let title = if dialog.edit.is_some() {
        format!("{}: {}", s.edit, label(&dialog.shape, lang))
    } else {
        label(&dialog.shape, lang).to_owned()
    };
    let mut open = true;
    let mut apply = false;
    let mut cancel = false;
    let size = window_size(ctx);
    egui::Window::new(title).id(egui::Id::new("modeling.primitive.window")).open(&mut open)
        .resizable(true).min_width(80.0).max_width(size.x).max_height(size.y)
        .default_width(340.0_f32.min(size.x)).constrain_to(ctx.content_rect()).vscroll(true).show(ctx, |ui| {
            ui.label(lang.pick("原点为世界坐标，尺寸为局部坐标。新建时轴向 +Z，编辑保留已有旋转。长方体原点为最小角，圆柱/圆锥为底面中心，球/圆环为中心。", "Origins use world coordinates; sizes use local coordinates. New shapes use +Z; edits preserve existing rotation. Box: minimum corner; cylinder/cone: base center; sphere/torus: center."));
            ui.label(format!("{}: {}", lang.pick("单位", "Units"), ed.doc.meta.units.suffix()));
            egui::Grid::new("primitive_parameters").num_columns(2).show(ui, |ui| {
                number(ui, &format!("{} X", s.origin), &mut dialog.placement.translation.x);
                number(ui, &format!("{} Y", s.origin), &mut dialog.placement.translation.y);
                number(ui, &format!("{} Z", s.origin), &mut dialog.placement.translation.z);
                match &mut dialog.shape {
                    Primitive::Box { size } => { number(ui, s.size_x, &mut size.x); number(ui, s.size_y, &mut size.y); number(ui, s.size_z, &mut size.z); },
                    Primitive::Cylinder { radius, height } => { number(ui, s.radius, radius); number(ui, s.height, height); },
                    Primitive::Sphere { radius } => number(ui, s.radius, radius),
                    Primitive::Cone { radius1, radius2, height } => { number(ui, s.radius1, radius1); number(ui, s.radius2, radius2); number(ui, s.height, height); },
                    Primitive::Torus { major, minor } => { number(ui, s.major_radius, major); number(ui, s.minor_radius, minor); },
                }
            });
            op_ui(ui, &mut dialog.op, &dialog.snapshot.part, &dialog.inputs, lang);
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
            Ok(_) => return,
            Err(error) => {
                ed.error(&error);
                dialog.error = Some(error);
            }
        }
    }
    ctx.data_mut(|d| d.insert_temp(state_id, dialog));
}
