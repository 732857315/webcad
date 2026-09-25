//! Dock panels: built-in Layers, Model tree and Properties, plus registered extras
//! ([`extra::register`]). Panels are plain functions over a [`PanelCx`].

pub mod extra;

use wcad_doc::{Color, EntityId, EntityKind, Layer, LayerId, LineWeight, LinetypeRef};
use wcad_math::DVec2;
use wcad_solid::RegenResult;

use crate::commands::CommandRegistry;
use crate::editor::{Editor, Workspace};
use crate::i18n::{Lang, core, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dock {
    Left,
    Right,
}

/// Everything a panel may use.
pub struct PanelCx<'a> {
    pub editor: &'a mut Editor,
    pub regen: &'a RegenResult,
    pub workspace: Workspace,
}

impl PanelCx<'_> {
    pub fn lang(&self) -> Lang {
        self.editor.lang
    }
}

/// A registered dock panel.
#[derive(Clone, Copy)]
pub struct PanelSpec {
    /// Stable id (`"layers"`, `"model_tree"`, `"properties"`, …).
    pub id: &'static str,
    pub title: fn(Lang) -> &'static str,
    pub dock: Dock,
    pub ui: fn(&mut egui::Ui, &mut PanelCx<'_>),
}

pub fn register_builtin(r: &mut CommandRegistry) {
    r.add_panel(PanelSpec {
        id: "layers",
        title: |l| core(l).layers,
        dock: Dock::Left,
        ui: layers_ui,
    });
    r.add_panel(PanelSpec {
        id: "model_tree",
        title: |l| core(l).model_tree,
        dock: Dock::Left,
        ui: model_tree_ui,
    });
    r.add_panel(PanelSpec {
        id: "properties",
        title: |l| core(l).properties,
        dock: Dock::Right,
        ui: properties_ui,
    });
}

/// Swatch color of a layer/entity color (ACI 7 shown as the UI text color).
pub fn swatch(c: Color, layer: Color, fg: [u8; 3]) -> egui::Color32 {
    let [r, g, b] = c.resolve(layer, Color::WHITE, fg);
    egui::Color32::from_rgb(r, g, b)
}

fn color_name(c: Color, lang: Lang) -> String {
    let s = core(lang);
    match c {
        Color::ByLayer => s.by_layer.into(),
        Color::ByBlock => s.by_block.into(),
        Color::Aci(i) => {
            let names = lang.pick(
                ["", "红", "黄", "绿", "青", "蓝", "洋红", "白"],
                [
                    "", "Red", "Yellow", "Green", "Cyan", "Blue", "Magenta", "White",
                ],
            );
            if (1..=7).contains(&i) {
                names[i as usize].into()
            } else {
                format!("ACI {i}")
            }
        }
        Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
    }
}

const COLOR_CHOICES: [Color; 11] = [
    Color::ByLayer,
    Color::ByBlock,
    Color::Aci(1),
    Color::Aci(2),
    Color::Aci(3),
    Color::Aci(4),
    Color::Aci(5),
    Color::Aci(6),
    Color::Aci(7),
    Color::Aci(8),
    Color::Aci(9),
];

const WEIGHTS: [u16; 12] = [0, 5, 9, 13, 18, 25, 30, 35, 50, 70, 100, 140];

fn weight_name(w: LineWeight, lang: Lang) -> String {
    let s = core(lang);
    match w {
        LineWeight::ByLayer => s.by_layer.into(),
        LineWeight::ByBlock => s.by_block.into(),
        LineWeight::Default => s.default_weight.into(),
        LineWeight::Mm100(n) => format!("{:.2} mm", n as f64 / 100.0),
    }
}

fn ui_fg(ui: &egui::Ui) -> [u8; 3] {
    let c = ui.visuals().text_color();
    [c.r(), c.g(), c.b()]
}

// -------------------------------------------------------------------------------------------
// Layers

fn modify_layer(ed: &mut Editor, id: LayerId, f: impl FnOnce(&mut Layer)) {
    ed.doc.transact("LAYER", |tx| {
        if let Some(l) = tx.tables_mut().layers.get_mut(&id) {
            f(l);
        }
    });
    ed.pump();
}

pub fn layers_ui(ui: &mut egui::Ui, cx: &mut PanelCx<'_>) {
    let lang = cx.lang();
    let s = core(lang);
    let ed = &mut *cx.editor;
    let fg = ui_fg(ui);
    if ui.button(format!("＋ {}", s.layer_new)).clicked() {
        let n = ed.doc.drawing.tables.layers.len();
        let mut name = fmt(s.layer_new_name, &[&n]);
        let mut k = n;
        while ed.doc.drawing.layer_by_name(&name).is_some() {
            k += 1;
            name = fmt(s.layer_new_name, &[&k]);
        }
        let lt = ed.doc.drawing.tables.linetypes.keys().next().copied();
        ed.doc.transact("LAYER", |tx| {
            let id = tx.ids().layer();
            let t = tx.tables_mut();
            if let Some(lt) = lt {
                t.layers.insert(
                    id,
                    Layer {
                        name,
                        color: Color::WHITE,
                        linetype: lt,
                        lineweight: LineWeight::Default,
                        visible: true,
                        frozen: false,
                        locked: false,
                        plot: true,
                    },
                );
                t.current_layer = id;
            }
        });
        ed.pump();
    }
    ui.separator();
    let layers: Vec<(LayerId, Layer)> = ed
        .doc
        .drawing
        .tables
        .layers
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let current = ed.doc.drawing.tables.current_layer;
    egui::ScrollArea::vertical()
        .id_salt("layers_scroll")
        .show(ui, |ui| {
            egui::Grid::new("layers_grid")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    ui.label("");
                    ui.label(s.prop_layer);
                    ui.label(s.layer_on).on_hover_text(s.layer_on);
                    ui.label(s.layer_freeze);
                    ui.label(s.layer_lock);
                    ui.label(s.prop_color);
                    ui.end_row();
                    for (id, l) in layers {
                        let is_cur = id == current;
                        if ui
                            .radio(is_cur, "")
                            .on_hover_text(s.layer_set_current)
                            .clicked()
                            && !is_cur
                        {
                            ed.doc
                                .transact("LAYER", |tx| tx.tables_mut().current_layer = id);
                            ed.pump();
                        }
                        ui.label(if is_cur {
                            egui::RichText::new(&l.name).strong()
                        } else {
                            egui::RichText::new(&l.name)
                        });
                        let mut v = l.visible;
                        if ui.checkbox(&mut v, "").changed() {
                            modify_layer(ed, id, |l| l.visible = v);
                        }
                        let mut fr = l.frozen;
                        if ui.checkbox(&mut fr, "").changed() {
                            modify_layer(ed, id, |l| l.frozen = fr);
                        }
                        let mut lk = l.locked;
                        if ui.checkbox(&mut lk, "").changed() {
                            modify_layer(ed, id, |l| l.locked = lk);
                        }
                        let cur_color = l.color;
                        egui::ComboBox::from_id_salt(("layer_color", id.0))
                            .width(70.0)
                            .selected_text(
                                egui::RichText::new(format!("■ {}", color_name(cur_color, lang)))
                                    .color(swatch(cur_color, cur_color, fg)),
                            )
                            .show_ui(ui, |ui| {
                                for c in COLOR_CHOICES.iter().skip(2) {
                                    let t =
                                        egui::RichText::new(format!("■ {}", color_name(*c, lang)))
                                            .color(swatch(*c, *c, fg));
                                    if ui.selectable_label(cur_color == *c, t).clicked() {
                                        let c = *c;
                                        modify_layer(ed, id, |l| l.color = c);
                                    }
                                }
                            });
                        ui.end_row();
                    }
                });
        });
}

// -------------------------------------------------------------------------------------------
// Model tree

pub fn model_tree_ui(ui: &mut egui::Ui, cx: &mut PanelCx<'_>) {
    let s = core(cx.lang());
    let part = &cx.editor.doc.part;
    if part.features.is_empty() {
        ui.weak(s.no_features);
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("tree_scroll")
        .show(ui, |ui| {
            for f in &part.features {
                let status = cx.regen.status_of(f.id);
                let (icon, color) = match status {
                    Some(wcad_solid::FeatureStatus::Error(_)) => {
                        ("×", egui::Color32::from_rgb(230, 90, 80))
                    }
                    Some(wcad_solid::FeatureStatus::Warning(_)) => {
                        ("△", egui::Color32::from_rgb(230, 180, 60))
                    }
                    _ => ("●", ui.visuals().text_color()),
                };
                let mut text = format!(
                    "{icon} {}",
                    if f.name.is_empty() {
                        f.kind.type_key()
                    } else {
                        &f.name
                    }
                );
                if f.suppressed {
                    text.push_str(&format!(" ({})", s.suppressed));
                }
                let r = ui.label(egui::RichText::new(text).color(color));
                match status {
                    Some(wcad_solid::FeatureStatus::Error(m))
                    | Some(wcad_solid::FeatureStatus::Warning(m)) => {
                        r.on_hover_text(m);
                    }
                    _ => {}
                }
            }
        });
}

// -------------------------------------------------------------------------------------------
// Properties

fn edit_selected(ed: &mut Editor, ids: &[EntityId], f: impl Fn(&mut wcad_doc::Entity)) {
    let ids = ids.to_vec();
    ed.doc.transact("PROPERTIES", |tx| {
        for id in &ids {
            let editable = tx
                .entity(*id)
                .is_some_and(|e| tx.drawing().is_layer_editable(e.layer));
            if editable {
                tx.modify(*id, &f);
            }
        }
    });
    ed.pump();
}

fn vec_row(ui: &mut egui::Ui, label: &str, v: &mut DVec2, speed: f64) -> bool {
    ui.label(label);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui
            .add(
                egui::DragValue::new(&mut v.x)
                    .speed(speed)
                    .prefix("X ")
                    .max_decimals(4),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut v.y)
                    .speed(speed)
                    .prefix("Y ")
                    .max_decimals(4),
            )
            .changed();
    });
    ui.end_row();
    changed
}

fn num_row(ui: &mut egui::Ui, label: &str, v: &mut f64, speed: f64) -> bool {
    ui.label(label);
    let c = ui
        .add(egui::DragValue::new(v).speed(speed).max_decimals(4))
        .changed();
    ui.end_row();
    c
}

pub fn properties_ui(ui: &mut egui::Ui, cx: &mut PanelCx<'_>) {
    let lang = cx.lang();
    let s = core(lang);
    let ed = &mut *cx.editor;
    let fg = ui_fg(ui);
    let ids: Vec<EntityId> = ed.selection.iter().collect();
    if ids.is_empty() {
        ui.weak(s.prop_none_selected);
        ui.separator();
        let cur = ed.doc.drawing.tables.current_layer;
        let name = ed
            .doc
            .drawing
            .layer(cur)
            .map(|l| l.name.clone())
            .unwrap_or_default();
        ui.label(format!("{}: {name}", s.layer_current));
        let mut st = ed.doc.drawing.tables.settings.clone();
        let mut changed = false;
        egui::Grid::new("drawing_settings")
            .num_columns(2)
            .show(ui, |ui| {
                changed |= num_row(ui, "LTSCALE", &mut st.ltscale, 0.05);
                changed |= num_row(ui, s.st_grid, &mut st.grid_spacing, 0.5);
                changed |= num_row(ui, s.st_snap, &mut st.snap_spacing, 0.1);
            });
        if changed && st.ltscale > 0.0 && st.grid_spacing > 0.0 && st.snap_spacing > 0.0 {
            ed.doc
                .transact("SETTINGS", |tx| tx.tables_mut().settings = st);
            ed.pump();
        }
        return;
    }
    ui.label(fmt(s.prop_selected_n, &[&ids.len()]));
    let d = &ed.doc.drawing;
    let first = d.entities.get(&ids[0]).cloned();
    let Some(first) = first else { return };
    let same = |f: &dyn Fn(&wcad_doc::Entity) -> bool| {
        ids.iter().filter_map(|id| d.entities.get(id)).all(f)
    };
    let layer_same = same(&|e| e.layer == first.layer);
    let color_same = same(&|e| e.color == first.color);
    let lt_same = same(&|e| e.linetype == first.linetype);
    let lw_same = same(&|e| e.lineweight == first.lineweight);
    let layers: Vec<(LayerId, String, Color)> = d
        .tables
        .layers
        .iter()
        .map(|(k, v)| (*k, v.name.clone(), v.color))
        .collect();
    let linetypes: Vec<(wcad_doc::LinetypeId, String)> = d
        .tables
        .linetypes
        .iter()
        .map(|(k, v)| (*k, v.name.clone()))
        .collect();
    let layer_color = d
        .layer(first.layer)
        .map(|l| l.color)
        .unwrap_or(Color::WHITE);
    let varies = s.prop_varies;

    egui::CollapsingHeader::new(s.prop_general)
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("props_general")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(s.prop_layer);
                    let cur = if layer_same {
                        layers
                            .iter()
                            .find(|l| l.0 == first.layer)
                            .map(|l| l.1.clone())
                            .unwrap_or_default()
                    } else {
                        varies.into()
                    };
                    egui::ComboBox::from_id_salt("prop_layer")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for (id, name, _) in &layers {
                                if ui
                                    .selectable_label(layer_same && *id == first.layer, name)
                                    .clicked()
                                {
                                    let id = *id;
                                    edit_selected(ed, &ids, |e| e.layer = id);
                                }
                            }
                        });
                    ui.end_row();
                    ui.label(s.prop_color);
                    let cur = if color_same {
                        color_name(first.color, lang)
                    } else {
                        varies.into()
                    };
                    egui::ComboBox::from_id_salt("prop_color")
                        .selected_text(egui::RichText::new(format!("■ {cur}")).color(swatch(
                            first.color,
                            layer_color,
                            fg,
                        )))
                        .show_ui(ui, |ui| {
                            for c in COLOR_CHOICES {
                                let t = egui::RichText::new(format!("■ {}", color_name(c, lang)))
                                    .color(swatch(c, layer_color, fg));
                                if ui
                                    .selectable_label(color_same && c == first.color, t)
                                    .clicked()
                                {
                                    edit_selected(ed, &ids, |e| e.color = c);
                                }
                            }
                        });
                    ui.end_row();
                    ui.label(s.prop_linetype);
                    let lt_name = |r: LinetypeRef| match r {
                        LinetypeRef::ByLayer => s.by_layer.to_owned(),
                        LinetypeRef::ByBlock => s.by_block.to_owned(),
                        LinetypeRef::Id(id) => linetypes
                            .iter()
                            .find(|l| l.0 == id)
                            .map(|l| l.1.clone())
                            .unwrap_or_default(),
                    };
                    let cur = if lt_same {
                        lt_name(first.linetype)
                    } else {
                        varies.into()
                    };
                    egui::ComboBox::from_id_salt("prop_lt")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            let mut choices = vec![LinetypeRef::ByLayer, LinetypeRef::ByBlock];
                            choices.extend(linetypes.iter().map(|l| LinetypeRef::Id(l.0)));
                            for c in choices {
                                if ui
                                    .selectable_label(lt_same && c == first.linetype, lt_name(c))
                                    .clicked()
                                {
                                    edit_selected(ed, &ids, |e| e.linetype = c);
                                }
                            }
                        });
                    ui.end_row();
                    ui.label(s.prop_lineweight);
                    let cur = if lw_same {
                        weight_name(first.lineweight, lang)
                    } else {
                        varies.into()
                    };
                    egui::ComboBox::from_id_salt("prop_lw")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            let mut choices = vec![
                                LineWeight::ByLayer,
                                LineWeight::ByBlock,
                                LineWeight::Default,
                            ];
                            choices.extend(WEIGHTS.iter().map(|w| LineWeight::Mm100(*w)));
                            for c in choices {
                                if ui
                                    .selectable_label(
                                        lw_same && c == first.lineweight,
                                        weight_name(c, lang),
                                    )
                                    .clicked()
                                {
                                    edit_selected(ed, &ids, |e| e.lineweight = c);
                                }
                            }
                        });
                    ui.end_row();
                });
        });

    if ids.len() != 1 {
        return;
    }
    let id = ids[0];
    let mut kind = first.kind.clone();
    let mut changed = false;
    egui::CollapsingHeader::new(format!("{} · {}", s.prop_geometry, kind.type_name()))
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("props_geom")
                .num_columns(2)
                .show(ui, |ui| match &mut kind {
                    EntityKind::Point { p } => {
                        changed |= vec_row(ui, s.prop_position, p, 0.1);
                    }
                    EntityKind::Line(l) => {
                        changed |= vec_row(ui, s.prop_start, &mut l.a, 0.1);
                        changed |= vec_row(ui, s.prop_end, &mut l.b, 0.1);
                        ui.label(s.prop_length);
                        ui.label(format!("{:.4}", l.length()));
                        ui.end_row();
                        ui.label(s.prop_angle);
                        ui.label(format!(
                            "{:.2}°",
                            (l.b - l.a).to_angle().to_degrees().rem_euclid(360.0)
                        ));
                        ui.end_row();
                    }
                    EntityKind::Circle(c) => {
                        changed |= vec_row(ui, s.prop_center, &mut c.c, 0.1);
                        let mut r = c.r;
                        if num_row(ui, s.prop_radius, &mut r, 0.05) && r > 0.0 {
                            c.r = r;
                            changed = true;
                        }
                    }
                    EntityKind::Arc(a) => {
                        changed |= vec_row(ui, s.prop_center, &mut a.c, 0.1);
                        let mut r = a.r;
                        if num_row(ui, s.prop_radius, &mut r, 0.05) && r > 0.0 {
                            a.r = r;
                            changed = true;
                        }
                        let mut sa = a.start.to_degrees();
                        if num_row(ui, s.prop_start_angle, &mut sa, 0.5) {
                            a.start = sa.to_radians();
                            changed = true;
                        }
                        let mut ea = a.end.to_degrees();
                        if num_row(ui, s.prop_end_angle, &mut ea, 0.5) {
                            a.end = ea.to_radians();
                            changed = true;
                        }
                    }
                    EntityKind::Polyline(p) => {
                        ui.label(s.prop_vertices);
                        ui.label(p.verts.len().to_string());
                        ui.end_row();
                        ui.label(s.prop_closed);
                        changed |= ui.checkbox(&mut p.closed, "").changed();
                        ui.end_row();
                    }
                    EntityKind::Text(t) => {
                        ui.label(s.prop_text);
                        changed |= ui.text_edit_singleline(&mut t.text).changed();
                        ui.end_row();
                        changed |= vec_row(ui, s.prop_position, &mut t.pos, 0.1);
                        let mut h = t.height;
                        if num_row(ui, s.prop_height, &mut h, 0.05) && h > 0.0 {
                            t.height = h;
                            changed = true;
                        }
                        let mut rot = t.rotation.to_degrees();
                        if num_row(ui, s.prop_angle, &mut rot, 0.5) {
                            t.rotation = rot.to_radians();
                            changed = true;
                        }
                    }
                    EntityKind::MText(t) => {
                        ui.label(s.prop_text);
                        changed |= ui.text_edit_multiline(&mut t.text).changed();
                        ui.end_row();
                        changed |= vec_row(ui, s.prop_position, &mut t.pos, 0.1);
                        let mut h = t.height;
                        if num_row(ui, s.prop_height, &mut h, 0.05) && h > 0.0 {
                            t.height = h;
                            changed = true;
                        }
                    }
                    EntityKind::Insert(i) => {
                        changed |= vec_row(ui, s.prop_position, &mut i.pos, 0.1);
                    }
                    _ => {}
                });
        });
    if changed {
        edit_selected(ed, &[id], |e| e.kind = kind.clone());
    }
}
