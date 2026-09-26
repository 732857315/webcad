//! Headless tests exercise the same drafts and commit paths used by the parameter windows.

use std::f64::consts::PI;
use std::sync::Arc;

use wcad_doc::{BooleanKind, Document, Extent, PlaneRef, Primitive, ProfileRef};
use wcad_math::{DAffine3, DVec3};
use wcad_solid::mass_properties;

use super::*;
use operations::{BooleanDialog, InspectDialog, InspectKind};
use primitives::Dialog as PrimitiveDialog;

fn editor() -> Editor {
    let mut registry = CommandRegistry::new();
    primitives::register(&mut registry);
    operations::register(&mut registry);
    tree::register(&mut registry);
    let mut ed = Editor::new(Arc::new(registry));
    ed.lang = Lang::En;
    ed
}

fn primitive(ed: &mut Editor, shape: Primitive, origin: DVec3) -> FeatureId {
    let mut draft = PrimitiveDialog::new(ed, shape, None).unwrap();
    draft.placement = DAffine3::from_translation(origin);
    draft.apply(ed).unwrap()
}

fn cube(ed: &mut Editor, origin: DVec3) -> FeatureId {
    primitive(ed, Primitive::Box { size: DVec3::ONE }, origin)
}

fn near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < expected.abs().max(1.0) * 0.015,
        "{actual} != {expected}"
    );
}

fn volume(ed: &Editor, id: FeatureId) -> f64 {
    let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
    mass_properties(result.body(BodyRef(id)).unwrap())
        .unwrap()
        .volume
}

#[test]
fn commands_and_aliases_request_real_parameter_windows_without_mutation() {
    let cases = [
        ("BOX", "BOX", primitives::REQUESTS[0]),
        ("CYL", "CYLINDER", primitives::REQUESTS[1]),
        ("SPH", "SPHERE", primitives::REQUESTS[2]),
        ("CONE", "CONE", primitives::REQUESTS[3]),
        ("TOR", "TORUS", primitives::REQUESTS[4]),
        ("UNI", "UNION", operations::BOOLEAN_REQUESTS[0]),
        ("SU", "SUBTRACT", operations::BOOLEAN_REQUESTS[1]),
        ("IN", "INTERSECT", operations::BOOLEAN_REQUESTS[2]),
        ("MEASURE3D", "MEASURE3D", operations::INSPECT_REQUESTS[0]),
        ("EXPORTSTL", "EXPORTSTL", operations::INSPECT_REQUESTS[1]),
        ("EXPORTOBJ", "EXPORTOBJ", operations::INSPECT_REQUESTS[2]),
        ("EXPORTSTEP", "EXPORTSTEP", operations::INSPECT_REQUESTS[3]),
    ];
    let mut ed = editor();
    let ids = ed.doc.ids().clone();
    for (alias, name, request) in cases {
        let spec = ed.registry.find(alias).unwrap();
        assert_eq!(spec.name, name);
        assert_eq!(spec.tab, Some(crate::commands::RibbonTab::Model));
        assert!(spec.icon.is_ascii());
        assert!(matches!(spec.kind, crate::commands::CommandKind::Action(_)));
        ed.requests.clear();
        assert!(ed.run_command(alias));
        assert_eq!(
            ed.requests,
            vec![
                AppRequest::SetWorkspace(Workspace::Modeling),
                AppRequest::ShowDialog(request)
            ]
        );
        assert!(ed.doc.part.features.is_empty());
        assert!(!ed.doc.can_undo());
        assert_eq!(ed.doc.ids(), &ids);
    }
}

#[test]
fn every_primitive_has_expected_volume_position_and_atomic_undo_redo() {
    let origin = DVec3::new(10.0, -4.0, 3.0);
    let cases = [
        (
            Primitive::Box {
                size: DVec3::new(2.0, 3.0, 4.0),
            },
            24.0,
            DVec3::new(1.0, 1.5, 2.0),
        ),
        (
            Primitive::Cylinder {
                radius: 2.0,
                height: 3.0,
            },
            12.0 * PI,
            DVec3::new(0.0, 0.0, 1.5),
        ),
        (
            Primitive::Sphere { radius: 2.0 },
            32.0 * PI / 3.0,
            DVec3::ZERO,
        ),
        (
            Primitive::Cone {
                radius1: 2.0,
                radius2: 1.0,
                height: 3.0,
            },
            7.0 * PI,
            DVec3::new(0.0, 0.0, 33.0 / 28.0),
        ),
        (
            Primitive::Cone {
                radius1: 2.0,
                radius2: 0.0,
                height: 3.0,
            },
            4.0 * PI,
            DVec3::new(0.0, 0.0, 0.75),
        ),
        (
            Primitive::Torus {
                major: 3.0,
                minor: 1.0,
            },
            6.0 * PI * PI,
            DVec3::ZERO,
        ),
    ];
    for (shape, expected, centroid) in cases {
        let mut ed = editor();
        let id = primitive(&mut ed, shape, origin);
        assert_eq!(ed.doc.revision(), 1);
        let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
        let body = result.body(BodyRef(id)).unwrap();
        assert_eq!(body.source, id);
        let m = mass_properties(body).unwrap();
        near(m.volume, expected);
        assert!(
            (m.centroid - origin - centroid).length() < 0.025,
            "{:?}",
            m.centroid
        );
        assert!(m.bbox.min.is_finite() && m.bbox.max.is_finite());
        let part = ed.doc.part.clone();
        ed.doc.undo().unwrap();
        assert!(ed.doc.part.features.is_empty());
        assert!(!ed.doc.can_undo());
        ed.doc.redo().unwrap();
        assert_eq!(ed.doc.part, part);
        near(volume(&ed, id), expected);
    }
}

#[test]
fn invalid_geometry_never_advances_ids_or_history() {
    let mut ed = editor();
    let ids = ed.doc.ids().clone();
    for shape in [
        Primitive::Box {
            size: DVec3::new(1.0, 0.0, 1.0),
        },
        Primitive::Box {
            size: DVec3::splat(1e20),
        },
        Primitive::Box {
            size: DVec3::splat(1e-10),
        },
        Primitive::Cylinder {
            radius: f64::NAN,
            height: 1.0,
        },
        Primitive::Cylinder {
            radius: 1.0,
            height: f64::INFINITY,
        },
        Primitive::Sphere { radius: -1.0 },
        Primitive::Cone {
            radius1: 0.0,
            radius2: 0.0,
            height: 1.0,
        },
        Primitive::Cone {
            radius1: 1.0,
            radius2: -1.0,
            height: 1.0,
        },
        Primitive::Torus {
            major: 1.0,
            minor: 1.0,
        },
    ] {
        assert!(
            PrimitiveDialog::new(&ed, shape, None)
                .unwrap()
                .apply(&mut ed)
                .is_err()
        );
    }
    for origin in [
        DVec3::splat(f64::NAN),
        DVec3::splat(f64::INFINITY),
        DVec3::splat(MAX_COORD),
    ] {
        let mut draft =
            PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, None).unwrap();
        draft.placement = DAffine3::from_translation(origin);
        assert!(draft.apply(&mut ed).is_err());
    }
    assert!(ed.doc.part.features.is_empty());
    assert_eq!(ed.doc.ids(), &ids);
    assert_eq!(ed.doc.revision(), 0);
    assert!(!ed.doc.can_undo());
}

#[test]
fn primitive_combine_modes_use_explicit_target_and_keep_its_identity() {
    for (mode, expected) in [(0, 1.875), (1, 0.875), (2, 0.125)] {
        let mut ed = editor();
        let target = cube(&mut ed, DVec3::ZERO);
        let unrelated = cube(&mut ed, DVec3::splat(10.0));
        let mut draft =
            PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, None).unwrap();
        draft.placement = DAffine3::from_translation(DVec3::splat(0.5));
        draft.op = match mode {
            0 => BodyOp::Join {
                target: Some(BodyRef(target)),
            },
            1 => BodyOp::Cut {
                target: Some(BodyRef(target)),
            },
            _ => BodyOp::Intersect {
                target: Some(BodyRef(target)),
            },
        };
        let feature = draft.apply(&mut ed).unwrap();
        let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
        assert_eq!(result.bodies.len(), 2);
        assert_eq!(result.body(BodyRef(target)).unwrap().source, feature);
        near(volume(&ed, target), expected);
        near(volume(&ed, unrelated), 1.0);
    }
}

#[test]
fn combining_never_implicitly_chooses_last_body_or_creates_on_missing_target() {
    let mut ed = editor();
    cube(&mut ed, DVec3::ZERO);
    let before = ed.doc.part.clone();
    let revision = ed.doc.revision();
    for op in [
        BodyOp::Join { target: None },
        BodyOp::Cut { target: None },
        BodyOp::Intersect {
            target: Some(BodyRef(FeatureId(999))),
        },
    ] {
        let mut draft = PrimitiveDialog::new(&ed, Primitive::Sphere { radius: 1.0 }, None).unwrap();
        draft.op = op;
        assert!(draft.apply(&mut ed).is_err());
    }
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn all_booleans_respect_tools_retention_target_and_undo() {
    for (kind, expected) in [
        (BooleanKind::Union, 1.875),
        (BooleanKind::Subtract, 0.875),
        (BooleanKind::Intersect, 0.125),
    ] {
        for keep_tools in [false, true] {
            let mut ed = editor();
            let target = cube(&mut ed, DVec3::ZERO);
            let tool = cube(&mut ed, DVec3::splat(0.5));
            let unrelated = cube(&mut ed, DVec3::splat(10.0));
            let before = ed.doc.part.clone();
            let mut draft = BooleanDialog::new(&ed, kind, None).unwrap();
            assert!(draft.target.is_none() && draft.tools.is_empty());
            draft.target = Some(BodyRef(target));
            draft.tools = vec![BodyRef(tool)];
            draft.keep_tools = keep_tools;
            let id = draft.apply(&mut ed).unwrap();
            let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
            assert_eq!(result.bodies.len(), if keep_tools { 3 } else { 2 });
            assert_eq!(result.body(BodyRef(target)).unwrap().source, id);
            assert_eq!(result.body(BodyRef(tool)).is_some(), keep_tools);
            near(volume(&ed, target), expected);
            near(volume(&ed, unrelated), 1.0);
            if keep_tools {
                near(volume(&ed, tool), 1.0);
            }
            let after = ed.doc.part.clone();
            ed.doc.undo().unwrap();
            assert_eq!(ed.doc.part, before);
            ed.doc.redo().unwrap();
            assert_eq!(ed.doc.part, after);
        }
    }
}

#[test]
fn duplicate_missing_self_and_empty_tools_are_rejected_before_commit() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let b = cube(&mut ed, DVec3::splat(0.5));
    let before = ed.doc.part.clone();
    let ids = ed.doc.ids().clone();
    let revision = ed.doc.revision();
    for tools in [
        vec![],
        vec![BodyRef(a)],
        vec![BodyRef(b), BodyRef(b)],
        vec![BodyRef(FeatureId(999))],
    ] {
        let mut draft = BooleanDialog::new(&ed, BooleanKind::Union, None).unwrap();
        draft.target = Some(BodyRef(a));
        draft.tools = tools;
        assert!(draft.apply(&mut ed).is_err());
    }
    let mut draft = BooleanDialog::new(&ed, BooleanKind::Union, None).unwrap();
    draft.tools = vec![BodyRef(b)];
    assert!(draft.apply(&mut ed).is_err());
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.ids(), &ids);
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn failed_kernel_regeneration_preserves_document_allocator_and_redo() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    cube(&mut ed, DVec3::splat(5.0));
    cube(&mut ed, DVec3::splat(9.0));
    ed.doc.undo().unwrap();
    assert!(ed.doc.can_redo());
    let before = ed.doc.part.clone();
    let ids = ed.doc.ids().clone();
    let revision = ed.doc.revision();
    let mut draft = PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, None).unwrap();
    draft.placement = DAffine3::from_translation(DVec3::splat(30.0));
    draft.op = BodyOp::Intersect {
        target: Some(BodyRef(a)),
    };
    assert!(draft.apply(&mut ed).is_err());
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.ids(), &ids);
    assert_eq!(ed.doc.revision(), revision);
    assert!(ed.doc.can_redo());
    ed.doc.redo().unwrap();
    assert_eq!(ed.doc.part.features.len(), 3);
}

#[test]
fn parameter_edit_preserves_id_and_name_and_is_one_undo_step() {
    let mut ed = editor();
    let id = cube(&mut ed, DVec3::ZERO);
    let before = ed.doc.part.clone();
    let ids = ed.doc.ids().clone();
    let mut draft = PrimitiveDialog::new(
        &ed,
        Primitive::Box {
            size: DVec3::new(2.0, 3.0, 4.0),
        },
        Some(id),
    )
    .unwrap();
    draft.placement.translation = DVec3::new(7.0, 8.0, 9.0);
    assert_eq!(draft.apply(&mut ed).unwrap(), id);
    assert_eq!(ed.doc.part.features.len(), 1);
    assert_eq!(ed.doc.part.features[0].name, before.features[0].name);
    assert_eq!(ed.doc.ids(), &ids);
    near(volume(&ed, id), 24.0);
    let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
    let m = mass_properties(result.body(BodyRef(id)).unwrap()).unwrap();
    assert!((m.centroid - DVec3::new(8.0, 9.5, 11.0)).length() < 1e-5);
    let after = ed.doc.part.clone();
    ed.doc.undo().unwrap();
    assert_eq!(ed.doc.part, before);
    ed.doc.redo().unwrap();
    assert_eq!(ed.doc.part, after);
}

#[test]
fn boolean_edit_uses_prefix_where_consumed_tools_still_exist() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let b = cube(&mut ed, DVec3::splat(0.5));
    let mut draft = BooleanDialog::new(&ed, BooleanKind::Union, None).unwrap();
    draft.target = Some(BodyRef(a));
    draft.tools = vec![BodyRef(b)];
    let id = draft.apply(&mut ed).unwrap();
    assert!(
        checked_regen(&ed.doc.part, ed.lang)
            .unwrap()
            .body(BodyRef(b))
            .is_none()
    );
    let before = ed.doc.part.clone();
    let mut edit = BooleanDialog::new(&ed, BooleanKind::Subtract, Some(id)).unwrap();
    edit.keep_tools = true;
    edit.apply(&mut ed).unwrap();
    near(volume(&ed, a), 0.875);
    near(volume(&ed, b), 1.0);
    ed.doc.undo().unwrap();
    assert_eq!(ed.doc.part, before);
}

#[test]
fn upstream_edit_cannot_leave_a_failed_downstream_feature() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let b = cube(&mut ed, DVec3::splat(0.5));
    let mut boolean = BooleanDialog::new(&ed, BooleanKind::Intersect, None).unwrap();
    boolean.target = Some(BodyRef(a));
    boolean.tools = vec![BodyRef(b)];
    boolean.apply(&mut ed).unwrap();
    let before = ed.doc.part.clone();
    let revision = ed.doc.revision();
    let mut edit = PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, Some(b)).unwrap();
    edit.placement.translation = DVec3::splat(20.0);
    assert!(edit.apply(&mut ed).is_err());
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn tree_rename_suppress_restore_delete_and_undo_redo() {
    let mut ed = editor();
    let id = cube(&mut ed, DVec3::ZERO);
    for change in [
        tree::Change::Rename("Housing".into()),
        tree::Change::Suppress(true),
        tree::Change::Suppress(false),
        tree::Change::Delete,
    ] {
        let before = ed.doc.part.clone();
        let snapshot = Snapshot::new(&ed);
        tree::apply_change(&mut ed, &snapshot, id, change).unwrap();
        let after = ed.doc.part.clone();
        let result = checked_regen(&after, ed.lang).unwrap();
        if after.features.is_empty() || after.features[0].suppressed {
            assert!(result.bodies.is_empty());
        } else {
            near(volume(&ed, id), 1.0);
        }
        ed.doc.undo().unwrap();
        assert_eq!(ed.doc.part, before);
        ed.doc.redo().unwrap();
        assert_eq!(ed.doc.part, after);
    }
    assert!(ed.doc.part.features.is_empty());
}

#[test]
fn deleting_or_suppressing_dependencies_is_refused_including_suppressed_dependents() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let mut draft = PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, None).unwrap();
    draft.placement.translation = DVec3::splat(5.0);
    draft.op = BodyOp::Join {
        target: Some(BodyRef(a)),
    };
    let modifier = draft.apply(&mut ed).unwrap();
    let snapshot = Snapshot::new(&ed);
    tree::apply_change(&mut ed, &snapshot, modifier, tree::Change::Suppress(true)).unwrap();
    let before = ed.doc.part.clone();
    let snapshot = Snapshot::new(&ed);
    let revision = ed.doc.revision();
    for change in [tree::Change::Delete, tree::Change::Suppress(true)] {
        assert!(
            tree::apply_change(&mut ed, &snapshot, a, change)
                .unwrap_err()
                .contains(&format!("#{}", modifier.0))
        );
    }
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn dependency_graph_includes_body_modification_sources_and_sketches() {
    let f = |id, kind| Feature {
        id: FeatureId(id),
        name: format!("F{id}"),
        suppressed: false,
        kind,
    };
    let body = BodyRef(FeatureId(1));
    let part = Part {
        features: vec![
            f(
                1,
                FeatureKind::Primitive {
                    shape: Primitive::Box { size: DVec3::ONE },
                    placement: DAffine3::IDENTITY,
                    op: BodyOp::NewBody,
                },
            ),
            f(
                2,
                FeatureKind::Primitive {
                    shape: Primitive::Sphere { radius: 1.0 },
                    placement: DAffine3::IDENTITY,
                    op: BodyOp::Cut { target: Some(body) },
                },
            ),
            f(
                3,
                FeatureKind::Primitive {
                    shape: Primitive::Sphere { radius: 2.0 },
                    placement: DAffine3::IDENTITY,
                    op: BodyOp::Join { target: Some(body) },
                },
            ),
            f(
                4,
                FeatureKind::Sketch {
                    plane: PlaneRef::Xy,
                    sketch: wcad_sketch::Sketch::new(),
                },
            ),
            f(
                5,
                FeatureKind::Extrude {
                    profile: ProfileRef {
                        sketch: FeatureId(4),
                        regions: vec![],
                    },
                    extent: Extent::Blind { distance: 1.0 },
                    reversed: false,
                    op: BodyOp::Join { target: Some(body) },
                },
            ),
        ],
        rollback: Some(2),
    };
    assert_eq!(
        tree::dependents(&part, FeatureId(2)),
        vec![FeatureId(3), FeatureId(5)]
    );
    assert_eq!(tree::dependents(&part, FeatureId(4)), vec![FeatureId(5)]);
    assert!(tree::dependents(&part, FeatureId(5)).is_empty());
}

#[test]
fn editing_operation_cannot_remove_body_identity_used_by_suppressed_features() {
    let mut ed = editor();
    let target = cube(&mut ed, DVec3::splat(10.0));
    let source = cube(&mut ed, DVec3::ZERO);
    let mut modifier =
        PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, None).unwrap();
    modifier.placement.translation = DVec3::splat(5.0);
    modifier.op = BodyOp::Join {
        target: Some(BodyRef(source)),
    };
    let dependent = modifier.apply(&mut ed).unwrap();
    let snapshot = Snapshot::new(&ed);
    tree::apply_change(&mut ed, &snapshot, dependent, tree::Change::Suppress(true)).unwrap();
    let before = ed.doc.part.clone();
    let revision = ed.doc.revision();
    let mut edit =
        PrimitiveDialog::new(&ed, Primitive::Box { size: DVec3::ONE }, Some(source)).unwrap();
    edit.op = BodyOp::Join {
        target: Some(BodyRef(target)),
    };
    assert!(edit.apply(&mut ed).unwrap_err().contains("dependents"));
    assert_eq!(ed.doc.part, before);
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn invalid_tree_names_and_unchanged_rename_do_not_add_history() {
    let mut ed = editor();
    let id = cube(&mut ed, DVec3::ZERO);
    ed.doc.clear_history();
    let snapshot = Snapshot::new(&ed);
    let revision = ed.doc.revision();
    for name in ["   ".to_owned(), "bad\nname".to_owned(), "x".repeat(129)] {
        assert!(tree::apply_change(&mut ed, &snapshot, id, tree::Change::Rename(name)).is_err());
    }
    let same_name = ed.doc.part.features[0].name.clone();
    tree::apply_change(&mut ed, &snapshot, id, tree::Change::Rename(same_name)).unwrap();
    assert_eq!(ed.doc.revision(), revision);
    assert!(!ed.doc.can_undo());
}

#[test]
fn rollback_requires_explicit_roll_to_end_and_preserves_history() {
    let mut ed = editor();
    cube(&mut ed, DVec3::ZERO);
    ed.doc
        .transact("rollback", |tx| tx.part_mut().rollback = Some(0));
    assert!(PrimitiveDialog::new(&ed, Primitive::Sphere { radius: 1.0 }, None).is_err());
    tree::roll_end(&mut ed).unwrap();
    assert_eq!(
        checked_regen(&ed.doc.part, ed.lang).unwrap().bodies.len(),
        1
    );
    ed.doc.undo().unwrap();
    assert_eq!(ed.doc.part.rollback, Some(0));
}

#[test]
fn actual_dialog_cancel_consumes_request_and_keeps_history_empty() {
    let ctx = egui::Context::default();
    let mut ed = editor();
    for request in primitives::REQUESTS {
        ctx.data_mut(|d| d.insert_temp(egui::Id::new(request), true));
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| primitives::ui_hook(ui.ctx(), &mut ed));
        output.textures_delta.clear();
        assert!(
            ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::new(request)))
                .is_none()
        );
        assert!(
            ctx.data_mut(
                |d| d.get_temp::<PrimitiveDialog>(egui::Id::new("modeling.primitive.state"))
            )
            .is_none()
        );
        assert!(!ed.doc.can_undo());
        assert!(ed.doc.part.features.is_empty());
        // Release Escape so the next frame is another key press.
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |_| {},
        );
        output.textures_delta.clear();
    }
    assert!(
        ed.log
            .iter()
            .any(|line| line.text == core(ed.lang).cancelled)
    );
}

#[test]
fn old_drafts_cannot_commit_to_an_identical_replacement_document() {
    let mut ed = editor();
    cube(&mut ed, DVec3::ZERO);
    let draft = PrimitiveDialog::new(&ed, Primitive::Sphere { radius: 1.0 }, None).unwrap();
    let replacement = Document::from_parts(
        ed.doc.meta.clone(),
        ed.doc.drawing.clone(),
        ed.doc.part.clone(),
        ed.doc.ids().clone(),
    );
    ed.set_document(replacement);
    assert!(draft.apply(&mut ed).is_err());
    assert_eq!(ed.doc.part.features.len(), 1);
    assert!(!ed.doc.can_undo());
}

#[test]
fn changed_part_invalidates_boolean_tree_and_export_snapshots() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let b = cube(&mut ed, DVec3::splat(0.5));
    let mut boolean = BooleanDialog::new(&ed, BooleanKind::Union, None).unwrap();
    boolean.target = Some(BodyRef(a));
    boolean.tools = vec![BodyRef(b)];
    let export = InspectDialog::new(&ed, InspectKind::Stl).unwrap();
    let snapshot = Snapshot::new(&ed);
    ed.doc.transact("external rename", |tx| {
        tx.part_mut().features[0].name = "Changed".into()
    });
    let revision = ed.doc.revision();
    assert!(boolean.apply(&mut ed).is_err());
    assert!(export.export_bytes(&ed).is_err());
    assert!(tree::apply_change(&mut ed, &snapshot, b, tree::Change::Delete).is_err());
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn export_requests_contain_real_stl_obj_and_step_bytes_without_history() {
    let mut ed = editor();
    cube(&mut ed, DVec3::new(2.0, 3.0, 4.0));
    ed.doc.clear_history();
    let revision = ed.doc.revision();
    for (kind, extension) in [
        (InspectKind::Stl, "stl"),
        (InspectKind::Obj, "obj"),
        (InspectKind::Step, "step"),
    ] {
        let mut draft = InspectDialog::new(&ed, kind).unwrap();
        draft.filename = "housing".into();
        ed.requests.clear();
        draft.apply(&mut ed).unwrap();
        let [AppRequest::SaveBytes { name, bytes }] = &ed.requests[..] else {
            panic!("missing download");
        };
        assert_eq!(name, &format!("housing.{extension}"));
        match kind {
            InspectKind::Stl => {
                let triangles = u32::from_le_bytes(bytes[80..84].try_into().unwrap());
                assert_eq!(triangles, 12);
                assert_eq!(bytes.len(), 84 + triangles as usize * 50);
            }
            InspectKind::Obj => {
                let text = std::str::from_utf8(bytes).unwrap();
                assert!(text.contains("\nv ") && text.contains("\nvn "));
                assert_eq!(
                    text.lines().filter(|line| line.starts_with("f ")).count(),
                    12
                );
            }
            InspectKind::Step => {
                let text = std::str::from_utf8(bytes).unwrap();
                assert!(
                    text.contains("ISO-10303-21")
                        && text.contains("MANIFOLD_SOLID_BREP")
                        && text.contains("END-ISO-10303-21")
                );
            }
            _ => unreachable!(),
        }
        assert_eq!(ed.doc.revision(), revision);
        assert!(!ed.doc.can_undo());
    }
}

#[test]
fn mesh_fallback_warning_survives_and_step_is_explicitly_rejected() {
    let mut ed = editor();
    let a = cube(&mut ed, DVec3::ZERO);
    let b = cube(&mut ed, DVec3::new(0.5, 0.5, 0.0));
    let mut boolean = BooleanDialog::new(&ed, BooleanKind::Union, None).unwrap();
    boolean.target = Some(BodyRef(a));
    boolean.tools = vec![BodyRef(b)];
    let id = boolean.apply(&mut ed).unwrap();
    let result = checked_regen(&ed.doc.part, ed.lang).unwrap();
    assert!(matches!(
        result.status_of(id),
        Some(FeatureStatus::Warning(_))
    ));
    assert!(!result.body(BodyRef(a)).unwrap().is_exact());
    assert!(ed.log.iter().any(|line| line.text.starts_with("[!]")));
    ed.requests.clear();
    let revision = ed.doc.revision();
    let mut step = InspectDialog::new(&ed, InspectKind::Step).unwrap();
    assert!(step.apply(&mut ed).unwrap_err().contains("mesh-only"));
    assert!(ed.requests.is_empty());
    assert!(
        InspectDialog::new(&ed, InspectKind::Stl)
            .unwrap()
            .export_bytes(&ed)
            .unwrap()
            .1
            .len()
            > 84
    );
    assert_eq!(ed.doc.revision(), revision);
}

#[test]
fn measure_requires_selection_and_does_not_change_history() {
    let mut ed = editor();
    let id = primitive(
        &mut ed,
        Primitive::Box {
            size: DVec3::new(2.0, 3.0, 4.0),
        },
        DVec3::ZERO,
    );
    let mut draft = InspectDialog::new(&ed, InspectKind::Measure).unwrap();
    assert!(draft.target.is_none() && !draft.all);
    assert!(draft.apply(&mut ed).is_err());
    ed.doc.clear_history();
    draft.target = Some(BodyRef(id));
    draft.apply(&mut ed).unwrap();
    assert!(
        ed.log
            .iter()
            .any(|line| line.text.contains("volume 24.000000") && line.text.contains("mm^3"))
    );
    assert!(!ed.doc.can_undo());
}

#[test]
fn empty_export_and_invalid_filename_have_no_requests_or_history() {
    let mut ed = editor();
    assert!(InspectDialog::new(&ed, InspectKind::Stl).is_err());
    cube(&mut ed, DVec3::ZERO);
    ed.requests.clear();
    ed.doc.clear_history();
    for name in ["", "../file", "C:\\secret", "bad\nname", "file."] {
        let mut draft = InspectDialog::new(&ed, InspectKind::Obj).unwrap();
        draft.filename = name.into();
        assert!(draft.apply(&mut ed).is_err());
        assert!(ed.requests.is_empty());
    }
    assert!(!ed.doc.can_undo());
}

#[test]
fn editable_tree_replaces_builtin_panel() {
    let registry = CommandRegistry::with_all_modules();
    let panels: Vec<_> = registry
        .panels()
        .iter()
        .filter(|panel| panel.id == "model_tree")
        .collect();
    assert_eq!(panels.len(), 1);
    assert!(!std::ptr::fn_addr_eq(
        panels[0].ui,
        crate::panels::model_tree_ui as fn(&mut egui::Ui, &mut crate::panels::PanelCx<'_>),
    ));
}
