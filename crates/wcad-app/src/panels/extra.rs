//! Editable defaults for text and dimensions, with explicit undoable application.

use wcad_doc::{DimStyle, DimStyleId, TextStyle, TextStyleId};

use super::{Dock, PanelCx, PanelSpec};
use crate::commands::{CommandKind, CommandRegistry, CommandSpec, RibbonTab};
use crate::editor::{AppRequest, Editor};

pub fn register(r: &mut CommandRegistry) {
    r.add_panel(PanelSpec {
        id: "styles",
        title: |l| l.pick("样式", "Styles"),
        dock: Dock::Left,
        ui: styles_ui,
    });
    for (name, aliases, label) in [
        (
            "STYLE",
            &["ST"][..],
            (|l: crate::i18n::Lang| l.pick("文字样式", "Text Styles"))
                as fn(crate::i18n::Lang) -> &'static str,
        ),
        ("DIMSTYLE", &["D"][..], |l| {
            l.pick("标注样式", "Dimension Styles")
        }),
    ] {
        r.add(CommandSpec {
            name,
            aliases,
            label,
            icon: "Aa",
            tab: Some(RibbonTab::Annotate),
            group: "styles",
            kind: CommandKind::Action(|ed| ed.requests.push(AppRequest::ShowPanel("styles"))),
        });
    }
}

#[derive(Clone)]
struct StyleDraft {
    generation: u64,
    text_id: TextStyleId,
    dim_id: DimStyleId,
    source_text: TextStyle,
    source_dim: DimStyle,
    text: TextStyle,
    dim: DimStyle,
    error: Option<String>,
}

impl StyleDraft {
    fn has_changes(&self) -> bool {
        self.text != self.source_text || self.dim != self.source_dim
    }

    fn load(ed: &Editor) -> Option<Self> {
        let t = &ed.doc.drawing.tables;
        let text = t.text_styles.get(&t.current_text_style)?.clone();
        let dim = t.dim_styles.get(&t.current_dim_style)?.clone();
        Some(Self {
            generation: ed.document_generation(),
            text_id: t.current_text_style,
            dim_id: t.current_dim_style,
            source_text: text.clone(),
            source_dim: dim.clone(),
            text,
            dim,
            error: None,
        })
    }

    fn source_matches(&self, ed: &Editor) -> bool {
        let t = &ed.doc.drawing.tables;
        self.generation == ed.document_generation()
            && self.text_id == t.current_text_style
            && self.dim_id == t.current_dim_style
            && t.text_styles.get(&self.text_id) == Some(&self.source_text)
            && t.dim_styles.get(&self.dim_id) == Some(&self.source_dim)
    }
}

fn apply_styles(ed: &mut Editor, d: &StyleDraft) -> Result<(), &'static str> {
    let lang = ed.lang;
    if !d.source_matches(ed) {
        return Err(lang.pick(
            "文档或样式已改变，请重新加载",
            "The document or styles changed; reload first",
        ));
    }
    let t = &ed.doc.drawing.tables;
    if d.text.name.trim().is_empty()
        || d.dim.name.trim().is_empty()
        || t.text_styles
            .iter()
            .any(|(id, s)| *id != d.text_id && s.name.eq_ignore_ascii_case(d.text.name.trim()))
        || t.dim_styles
            .iter()
            .any(|(id, s)| *id != d.dim_id && s.name.eq_ignore_ascii_case(d.dim.name.trim()))
    {
        return Err(lang.pick(
            "样式名称不能为空或重复",
            "Style names must be nonempty and unique",
        ));
    }
    let values = [
        d.text.height,
        d.text.width_factor,
        d.text.oblique,
        d.dim.text_height,
        d.dim.arrow_size,
        d.dim.ext_offset,
        d.dim.ext_extend,
        d.dim.text_gap,
        d.dim.scale,
    ];
    if values.iter().any(|v| !v.is_finite())
        || d.text.height < 0.0
        || d.text.width_factor <= 0.0
        || d.text.oblique.abs() >= std::f64::consts::FRAC_PI_2
        || d.dim.text_height <= 0.0
        || d.dim.arrow_size < 0.0
        || d.dim.ext_offset < 0.0
        || d.dim.ext_extend < 0.0
        || d.dim.text_gap < 0.0
        || d.dim.scale <= 0.0
        || d.dim.decimals > 12
        || d.dim.angle_decimals > 12
        || !t.text_styles.contains_key(&d.dim.text_style)
    {
        return Err(lang.pick("样式参数无效", "Invalid style parameters"));
    }
    ed.doc.transact("STYLE", |tx| {
        let tables = tx.tables_mut();
        let mut text = d.text.clone();
        let mut dim = d.dim.clone();
        text.name = text.name.trim().to_owned();
        dim.name = dim.name.trim().to_owned();
        tables.text_styles.insert(d.text_id, text);
        tables.dim_styles.insert(d.dim_id, dim);
    });
    ed.pump();
    Ok(())
}

fn duplicate_style(ed: &mut Editor, text: bool) {
    ed.doc
        .transact(if text { "STYLE" } else { "DIMSTYLE" }, |tx| {
            if text {
                let id = tx.drawing().tables.current_text_style;
                let Some(mut style) = tx.drawing().tables.text_styles.get(&id).cloned() else {
                    return;
                };
                let mut n = tx.drawing().tables.text_styles.len() + 1;
                loop {
                    style.name = format!("Text {n}");
                    if !tx
                        .drawing()
                        .tables
                        .text_styles
                        .values()
                        .any(|s| s.name.eq_ignore_ascii_case(&style.name))
                    {
                        break;
                    }
                    n += 1;
                }
                let id = tx.ids().text_style();
                let tables = tx.tables_mut();
                tables.text_styles.insert(id, style);
                tables.current_text_style = id;
            } else {
                let id = tx.drawing().tables.current_dim_style;
                let Some(mut style) = tx.drawing().tables.dim_styles.get(&id).cloned() else {
                    return;
                };
                let mut n = tx.drawing().tables.dim_styles.len() + 1;
                loop {
                    style.name = format!("Dimension {n}");
                    if !tx
                        .drawing()
                        .tables
                        .dim_styles
                        .values()
                        .any(|s| s.name.eq_ignore_ascii_case(&style.name))
                    {
                        break;
                    }
                    n += 1;
                }
                let id = tx.ids().dim_style();
                let tables = tx.tables_mut();
                tables.dim_styles.insert(id, style);
                tables.current_dim_style = id;
            }
        });
    ed.pump();
}

fn styles_ui(ui: &mut egui::Ui, cx: &mut PanelCx<'_>) {
    egui::ScrollArea::vertical()
        .id_salt("styles_panel_scroll")
        .show(ui, |ui| {
            let ed = &mut *cx.editor;
            let lang = ed.lang;
            let key = ui.make_persistent_id("style_draft");
            let unsaved = ui
                .ctx()
                .data(|data| data.get_temp::<StyleDraft>(key))
                .is_some_and(|d| d.source_matches(ed) && d.has_changes());
            ui.add_enabled_ui(!unsaved, |ui| {
                let mut text_id = ed.doc.drawing.tables.current_text_style;
                let mut dim_id = ed.doc.drawing.tables.current_dim_style;
                ui.label(lang.pick("当前文字样式", "Current text style"));
                egui::ComboBox::from_id_salt("current_text_style")
                    .selected_text(
                        ed.doc
                            .drawing
                            .tables
                            .text_styles
                            .get(&text_id)
                            .map(|s| s.name.as_str())
                            .unwrap_or("?"),
                    )
                    .show_ui(ui, |ui| {
                        for (id, style) in &ed.doc.drawing.tables.text_styles {
                            ui.selectable_value(&mut text_id, *id, &style.name);
                        }
                    });
                ui.label(lang.pick("当前标注样式", "Current dimension style"));
                egui::ComboBox::from_id_salt("current_dim_style")
                    .selected_text(
                        ed.doc
                            .drawing
                            .tables
                            .dim_styles
                            .get(&dim_id)
                            .map(|s| s.name.as_str())
                            .unwrap_or("?"),
                    )
                    .show_ui(ui, |ui| {
                        for (id, style) in &ed.doc.drawing.tables.dim_styles {
                            ui.selectable_value(&mut dim_id, *id, &style.name);
                        }
                    });
                if text_id != ed.doc.drawing.tables.current_text_style
                    || dim_id != ed.doc.drawing.tables.current_dim_style
                {
                    ed.doc.transact("STYLE", |tx| {
                        let tables = tx.tables_mut();
                        tables.current_text_style = text_id;
                        tables.current_dim_style = dim_id;
                    });
                    ed.pump();
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(lang.pick("新文字样式", "New text style"))
                        .clicked()
                    {
                        duplicate_style(ed, true);
                    }
                    if ui
                        .button(lang.pick("新标注样式", "New dimension style"))
                        .clicked()
                    {
                        duplicate_style(ed, false);
                    }
                });
            });
            if unsaved {
                ui.label(lang.pick(
                    "切换样式前请先应用或重置",
                    "Apply or reset changes before switching styles",
                ));
            }

            let draft = ui.ctx().data(|data| data.get_temp::<StyleDraft>(key));
            let Some(mut draft) = draft
                .filter(|d| d.source_matches(ed))
                .or_else(|| StyleDraft::load(ed))
            else {
                ui.label(lang.pick("当前样式不存在", "The current style is missing"));
                return;
            };
            ui.separator();
            ui.label(lang.pick("文字默认值（用于新文字）", "Text defaults (for new text)"));
            ui.text_edit_singleline(&mut draft.text.name);
            egui::Grid::new("text_style_values")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label(lang.pick("固定高度，0 为自定", "Fixed height; 0 = variable"));
                    ui.add(
                        egui::DragValue::new(&mut draft.text.height)
                            .range(0.0..=1e6)
                            .speed(0.1),
                    );
                    ui.end_row();
                    ui.label(lang.pick("宽度比例", "Width factor"));
                    ui.add(
                        egui::DragValue::new(&mut draft.text.width_factor)
                            .range(0.01..=100.0)
                            .speed(0.01),
                    );
                    ui.end_row();
                    ui.label(lang.pick("倾斜角度", "Oblique angle"));
                    let mut angle = draft.text.oblique.to_degrees();
                    if ui
                        .add(
                            egui::DragValue::new(&mut angle)
                                .range(-85.0..=85.0)
                                .suffix(" deg"),
                        )
                        .changed()
                    {
                        draft.text.oblique = angle.to_radians();
                    }
                    ui.end_row();
                });
            ui.separator();
            ui.label(lang.pick(
                "标注样式（影响已有标注）",
                "Dimension style (updates existing dimensions)",
            ));
            ui.text_edit_singleline(&mut draft.dim.name);
            egui::Grid::new("dimension_style_values")
                .num_columns(2)
                .show(ui, |ui| {
                    for (label, value, min) in [
                        (
                            lang.pick("文字高度", "Text height"),
                            &mut draft.dim.text_height,
                            0.01,
                        ),
                        (
                            lang.pick("箭头大小", "Arrow size"),
                            &mut draft.dim.arrow_size,
                            0.0,
                        ),
                        (
                            lang.pick("延伸线偏移", "Extension offset"),
                            &mut draft.dim.ext_offset,
                            0.0,
                        ),
                        (
                            lang.pick("延伸线超出", "Extension overshoot"),
                            &mut draft.dim.ext_extend,
                            0.0,
                        ),
                        (
                            lang.pick("文字间隔", "Text gap"),
                            &mut draft.dim.text_gap,
                            0.0,
                        ),
                        (
                            lang.pick("总体比例", "Overall scale"),
                            &mut draft.dim.scale,
                            0.01,
                        ),
                    ] {
                        ui.label(label);
                        ui.add(egui::DragValue::new(value).range(min..=1e6).speed(0.1));
                        ui.end_row();
                    }
                    ui.label(lang.pick("小数位数", "Decimal places"));
                    ui.add(egui::DragValue::new(&mut draft.dim.decimals).range(0..=12));
                    ui.end_row();
                    ui.label(lang.pick("角度小数位数", "Angle decimal places"));
                    ui.add(egui::DragValue::new(&mut draft.dim.angle_decimals).range(0..=12));
                    ui.end_row();
                    ui.label(lang.pick("前缀", "Prefix"));
                    ui.text_edit_singleline(&mut draft.dim.prefix);
                    ui.end_row();
                    ui.label(lang.pick("后缀", "Suffix"));
                    ui.text_edit_singleline(&mut draft.dim.suffix);
                    ui.end_row();
                });
            if let Some(error) = &draft.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal(|ui| {
                if ui.button(lang.pick("应用", "Apply")).clicked() {
                    match apply_styles(ed, &draft) {
                        Ok(()) => {
                            if let Some(fresh) = StyleDraft::load(ed) {
                                draft = fresh;
                            }
                        }
                        Err(error) => draft.error = Some(error.to_owned()),
                    }
                }
                if ui.button(lang.pick("重置", "Reset")).clicked()
                    && let Some(fresh) = StyleDraft::load(ed)
                {
                    draft = fresh;
                }
            });
            ui.ctx().data_mut(|data| data.insert_temp(key, draft));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Harness;

    #[test]
    fn styles_scroll_in_a_short_viewport() {
        let mut h = Harness::new();
        let ctx = egui::Context::default();
        let regen = wcad_solid::RegenResult::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 240.0),
                )),
                ..Default::default()
            },
            |ui| {
                let bottom = ui.max_rect().bottom();
                styles_ui(
                    ui,
                    &mut PanelCx {
                        editor: &mut h.ed,
                        regen: &regen,
                        workspace: crate::editor::Workspace::Drafting,
                    },
                );
                assert!(ui.min_rect().bottom() <= bottom + ui.spacing().item_spacing.y);
            },
        );
        output.textures_delta.clear();
    }

    #[test]
    fn styles_apply_as_one_undoable_change() {
        let mut h = Harness::new();
        let original = h.ed.doc.drawing.tables.clone();
        let mut draft = StyleDraft::load(&h.ed).unwrap();
        assert!(!draft.has_changes());
        draft.text.height = 5.0;
        draft.dim.scale = 2.0;
        draft.dim.suffix = " mm".into();
        assert!(draft.has_changes());
        apply_styles(&mut h.ed, &draft).unwrap();
        assert!(!StyleDraft::load(&h.ed).unwrap().has_changes());
        let changed = h.ed.doc.drawing.tables.clone();
        assert_ne!(changed, original);
        h.ed.undo();
        assert_eq!(h.ed.doc.drawing.tables, original);
        h.ed.redo();
        assert_eq!(h.ed.doc.drawing.tables, changed);
    }

    #[test]
    fn invalid_or_stale_styles_do_not_mutate_document() {
        let mut h = Harness::new();
        let mut draft = StyleDraft::load(&h.ed).unwrap();
        let revision = h.ed.doc.revision();
        draft.dim.scale = f64::NAN;
        assert!(apply_styles(&mut h.ed, &draft).is_err());
        assert_eq!(h.ed.doc.revision(), revision);
        draft.dim.scale = 1.0;
        h.ed.set_document(wcad_doc::Document::new());
        assert!(apply_styles(&mut h.ed, &draft).is_err());
        assert_eq!(h.ed.doc.revision(), 0);
    }

    #[test]
    fn duplicating_styles_keeps_unique_persisted_ids_after_undo() {
        let mut h = Harness::new();
        duplicate_style(&mut h.ed, true);
        let first = h.ed.doc.drawing.tables.current_text_style;
        h.ed.undo();
        duplicate_style(&mut h.ed, true);
        assert_ne!(h.ed.doc.drawing.tables.current_text_style, first);
        duplicate_style(&mut h.ed, false);
        assert_eq!(h.ed.doc.drawing.tables.dim_styles.len(), 2);
        let mut draft = StyleDraft::load(&h.ed).unwrap();
        draft.text.name = "standard".into();
        assert!(apply_styles(&mut h.ed, &draft).is_err());
    }
}
