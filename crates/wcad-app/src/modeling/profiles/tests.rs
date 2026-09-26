use std::f64::consts::{FRAC_PI_2, PI};
use std::sync::Arc;

use wcad_doc::{FaceRef, GeomHint, Primitive};
use wcad_geom2d::{Arc2, Circle2, EllipseArc2, PolyVertex, Polyline2};
use wcad_math::DAffine3;
use wcad_sketch::SkGeom;
use wcad_solid::{Body, SurfaceKind, mass_properties};

use super::*;

fn editor() -> Editor {
    let mut registry = CommandRegistry::new();
    register(&mut registry);
    let mut ed = Editor::new(Arc::new(registry));
    ed.lang = Lang::En;
    ed
}

fn select(ed: &mut Editor, entities: impl IntoIterator<Item = EntityKind>) -> Vec<EntityId> {
    let ids = ed.doc.transact("source", |tx| {
        entities.into_iter().map(|e| tx.add(e)).collect::<Vec<_>>()
    });
    ed.selection.set(ids.iter().copied());
    ids
}

fn rectangle(a: DVec2, b: DVec2) -> Vec<EntityKind> {
    let points = [a, DVec2::new(b.x, a.y), b, DVec2::new(a.x, b.y)];
    // Deliberately reverse two edges: selection need not be in traversal order.
    (0..4)
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % 4]);
            EntityKind::Line(if i % 2 == 0 {
                Line2::new(p, q)
            } else {
                Line2::new(q, p)
            })
        })
        .collect()
}

fn rect_editor() -> Editor {
    let mut ed = editor();
    select(&mut ed, rectangle(DVec2::ZERO, DVec2::new(4.0, 3.0)));
    ed.doc.clear_history();
    ed
}

fn bodies(ed: &Editor) -> Vec<Body> {
    let regen = regenerate(&ed.doc.part, &mut RegenCache::new());
    check_regen(&regen, Lang::En).unwrap();
    regen.bodies
}

fn volume(body: &Body, expected: f64) {
    let actual = mass_properties(body).unwrap().volume;
    assert!(
        (actual - expected).abs() < expected.abs().max(1.0) * 0.012,
        "{actual} != {expected}"
    );
}

fn only_sketch(ed: &Editor) -> &Sketch {
    ed.doc
        .part
        .features
        .iter()
        .find_map(|f| match &f.kind {
            FeatureKind::Sketch { sketch, .. } => Some(sketch),
            _ => None,
        })
        .unwrap()
}

fn no_change(ed: &mut Editor, kind: Kind, p: &Parameters) -> String {
    ed.doc.take_changes();
    let json = wcad_doc::file::to_json(&ed.doc, false).unwrap();
    let rev = ed.doc.revision();
    let ids = ed.doc.ids().clone();
    let undo = ed.doc.undo_label().map(str::to_owned);
    let redo = ed.doc.redo_label().map(str::to_owned);
    let selected = ed.selection.clone();
    let requests = ed.requests.clone();
    let error = apply(ed, kind, p).expect_err("invalid input must not commit");
    assert_eq!(wcad_doc::file::to_json(&ed.doc, false).unwrap(), json);
    assert_eq!(ed.doc.revision(), rev);
    assert_eq!(ed.doc.ids(), &ids);
    assert_eq!(ed.doc.undo_label(), undo.as_deref());
    assert_eq!(ed.doc.redo_label(), redo.as_deref());
    assert_eq!(ed.selection, selected);
    assert_eq!(ed.requests, requests);
    assert!(ed.doc.take_changes().is_empty());
    error
}

fn add_box(ed: &mut Editor, placement: DAffine3) -> BodyRef {
    ed.doc.transact("base", |tx| {
        let id = tx.ids().feature();
        tx.part_mut().features.push(Feature {
            id,
            name: "Base".into(),
            suppressed: false,
            kind: FeatureKind::Primitive {
                shape: Primitive::Box {
                    size: DVec3::splat(4.0),
                },
                placement,
                op: BodyOp::NewBody,
            },
        });
        BodyRef(id)
    })
}

fn ui_frame(ctx: &egui::Context, ed: &mut Editor) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| dialog_ui(ui.ctx(), ed));
    output.textures_delta.clear();
}

#[test]
fn commands_are_bilingual_model_actions_with_ascii_icons() {
    let ed = editor();
    for (name, alias) in [("EXTRUDE", "EXT"), ("REVOLVE", "REV")] {
        let command = ed.registry.find(alias).unwrap();
        assert_eq!(command.name, name);
        assert_eq!(command.tab, Some(RibbonTab::Model));
        assert!(!command.is_tool());
        assert!(command.icon.is_ascii());
        assert_ne!((command.label)(Lang::Zh), (command.label)(Lang::En));
    }
}

#[test]
fn opening_does_not_switch_workspace_or_consume_selection() {
    let mut ed = rect_editor();
    let selection = ed.selection.clone();
    let json = wcad_doc::file::to_json(&ed.doc, false).unwrap();
    assert!(ed.run_command("EXT"));
    assert_eq!(ed.requests, vec![AppRequest::ShowDialog(EXTRUDE_DIALOG)]);
    assert_eq!(ed.selection, selection);
    assert_eq!(wcad_doc::file::to_json(&ed.doc, false).unwrap(), json);
    assert!(!ed.doc.can_undo());
}

#[test]
fn ui_consumes_the_request_and_discards_state_on_document_replacement() {
    let ctx = egui::Context::default();
    let mut ed = editor();
    let request = egui::Id::new(REVOLVE_DIALOG);
    let key = egui::Id::new(STATE_ID);
    ctx.data_mut(|d| d.insert_temp(request, true));
    ui_frame(&ctx, &mut ed);
    assert!(ctx.data(|d| d.get_temp::<bool>(request)).is_none());
    let state = ctx.data(|d| d.get_temp::<Dialog>(key)).unwrap();
    assert_eq!(state.kind, Kind::Revolve);
    assert_eq!(state.parameters.plane, PlaneRef::Xz);
    assert!(!ed.doc.can_undo());
    assert!(ed.requests.is_empty());
    // Even an identical new document must invalidate an old dialog and target IDs.
    ed.set_document(Document::new());
    ui_frame(&ctx, &mut ed);
    assert!(ctx.data(|d| d.get_temp::<Dialog>(key)).is_none());
}

#[test]
fn selection_can_be_made_or_replaced_after_opening() {
    let mut ed = editor();
    ed.run_command("EXT");
    assert!(
        ed.log
            .last()
            .unwrap()
            .text
            .contains("Select closed contours")
    );
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
    select(&mut ed, rectangle(DVec2::ZERO, DVec2::ONE));
    select(
        &mut ed,
        [EntityKind::Circle(Circle2::new(DVec2::ZERO, 2.0))],
    );
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 3.0,
            ..Default::default()
        },
    )
    .unwrap();
    volume(&bodies(&ed)[0], 12.0 * PI);
    assert_eq!(only_sketch(&ed).curves().len(), 1);
}

#[test]
fn rectangle_creates_real_sketch_and_exact_extrusion_without_editing_sources() {
    let mut ed = rect_editor();
    let drawing = ed.doc.drawing.clone();
    let old_revision = ed.doc.revision();
    let id = apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 5.0,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(ed.doc.revision(), old_revision + 1);
    assert_eq!(ed.doc.part.features.len(), 2);
    assert_eq!(only_sketch(&ed).curves().len(), 4);
    assert!(
        only_sketch(&ed)
            .entities
            .values()
            .any(|e| matches!(e.geom, SkGeom::Line { .. }))
    );
    let body = bodies(&ed).remove(0);
    assert!(body.is_exact());
    assert_eq!(body.id, BodyRef(id));
    volume(&body, 60.0);
    assert_eq!(ed.doc.drawing, drawing);
    assert_eq!(
        ed.requests,
        vec![
            AppRequest::SetWorkspace(Workspace::Modeling),
            AppRequest::ZoomExtents
        ]
    );
}

#[test]
fn one_undo_removes_both_features_and_redo_restores_them() {
    let mut ed = rect_editor();
    let drawing = ed.doc.drawing.clone();
    apply(&mut ed, Kind::Extrude, &Parameters::default()).unwrap();
    let part = ed.doc.part.clone();
    let ids = ed.doc.ids().clone();
    ed.undo();
    assert!(ed.doc.part.features.is_empty());
    assert!(bodies(&ed).is_empty());
    assert!(!ed.doc.can_undo());
    assert_eq!(ed.doc.drawing, drawing);
    assert_eq!(ed.doc.ids(), &ids);
    ed.redo();
    assert_eq!(ed.doc.part, part);
    assert_eq!(ed.doc.drawing, drawing);
    assert_eq!(ed.doc.ids(), &ids);
    volume(&bodies(&ed)[0], 120.0);
}

#[test]
fn circle_stays_a_sketch_circle_and_extrudes_to_a_cylinder() {
    let mut ed = editor();
    select(
        &mut ed,
        [EntityKind::Circle(Circle2::new(DVec2::new(2.0, 3.0), 2.0))],
    );
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 4.0,
            ..Default::default()
        },
    )
    .unwrap();
    let sketch = only_sketch(&ed);
    assert_eq!(sketch.entities.len(), 2);
    assert!(
        sketch
            .entities
            .values()
            .any(|e| matches!(e.geom, SkGeom::Circle { r: 2.0, .. }))
    );
    let body = bodies(&ed).remove(0);
    assert!(body.is_exact());
    volume(&body, 16.0 * PI);
    assert!((mass_properties(&body).unwrap().centroid - DVec3::new(2.0, 3.0, 2.0)).length() < 0.02);
}

#[test]
fn rectangle_with_circular_hole_preserves_the_void() {
    let mut ed = editor();
    let mut entities = rectangle(DVec2::ZERO, DVec2::new(10.0, 6.0));
    entities.push(EntityKind::Circle(Circle2::new(DVec2::new(5.0, 3.0), 1.0)));
    select(&mut ed, entities);
    let original = ed.doc.drawing.clone();
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 2.0,
            ..Default::default()
        },
    )
    .unwrap();
    volume(&bodies(&ed)[0], (60.0 - PI) * 2.0);
    let FeatureKind::Extrude { profile, .. } = &ed.doc.part.features[1].kind else {
        panic!()
    };
    assert_eq!(profile.regions.len(), 1);
    assert_eq!(ed.doc.drawing, original);
}

#[test]
fn concentric_circles_make_an_annular_extrusion() {
    let mut ed = editor();
    select(
        &mut ed,
        [3.0, 1.0].map(|r| EntityKind::Circle(Circle2::new(DVec2::ZERO, r))),
    );
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 2.0,
            ..Default::default()
        },
    )
    .unwrap();
    volume(&bodies(&ed)[0], 16.0 * PI);
}

#[test]
fn nested_island_regions_use_even_odd_seeds() {
    let mut ed = editor();
    select(
        &mut ed,
        [5.0, 3.0, 1.0].map(|r| EntityKind::Circle(Circle2::new(DVec2::ZERO, r))),
    );
    let (sketch, seeds, tol) =
        sketch_from_selection(&ed.doc, &ed.selection.to_vec(), Lang::En).unwrap();
    assert_eq!(seeds.len(), 2);
    let regions = find_regions(
        &sketch
            .curves()
            .into_iter()
            .map(|(_, c)| c)
            .collect::<Vec<_>>(),
        tol,
    );
    let area: f64 = seeds
        .iter()
        .map(|seed| regions.iter().find(|r| r.contains(*seed)).unwrap().area())
        .sum();
    assert!((area - 17.0 * PI).abs() < 1e-8);
}

#[test]
fn both_bulge_directions_remain_analytic_arcs() {
    for bulge in [-1.0, 1.0] {
        let mut ed = editor();
        select(
            &mut ed,
            [EntityKind::Polyline(Polyline2 {
                verts: vec![
                    PolyVertex::new(DVec2::ZERO),
                    PolyVertex::with_bulge(DVec2::new(2.0, 0.0), bulge),
                    PolyVertex::new(DVec2::new(2.0, 2.0)),
                    PolyVertex::new(DVec2::new(0.0, 2.0)),
                ],
                closed: true,
            })],
        );
        let drawing = ed.doc.drawing.clone();
        apply(
            &mut ed,
            Kind::Extrude,
            &Parameters {
                distance: 1.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(only_sketch(&ed).curves().len(), 4);
        assert_eq!(
            only_sketch(&ed)
                .entities
                .values()
                .filter(|e| matches!(e.geom, SkGeom::Arc { .. }))
                .count(),
            1
        );
        volume(&bodies(&ed)[0], 4.0 + bulge * PI * 0.5);
        assert_eq!(ed.doc.drawing, drawing);
    }
}

#[test]
fn lines_and_arcs_can_jointly_form_a_slot() {
    let mut ed = editor();
    select(
        &mut ed,
        [
            EntityKind::Line(Line2::new(DVec2::new(0.0, -1.0), DVec2::new(4.0, -1.0))),
            EntityKind::Arc(Arc2::new(DVec2::new(4.0, 0.0), 1.0, -FRAC_PI_2, FRAC_PI_2)),
            EntityKind::Line(Line2::new(DVec2::new(0.0, 1.0), DVec2::new(4.0, 1.0))),
            EntityKind::Arc(Arc2::new(DVec2::ZERO, 1.0, FRAC_PI_2, 3.0 * FRAC_PI_2)),
        ],
    );
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 2.0,
            ..Default::default()
        },
    )
    .unwrap();
    volume(&bodies(&ed)[0], (8.0 + PI) * 2.0);
}

#[test]
fn base_planes_reverse_and_symmetric_use_the_correct_normal() {
    for (plane_ref, plane) in [
        (PlaneRef::Xy, Plane::XY),
        (PlaneRef::Xz, Plane::XZ),
        (PlaneRef::Yz, Plane::YZ),
    ] {
        for (symmetric, reversed) in [(false, false), (false, true), (true, true)] {
            let mut ed = rect_editor();
            apply(
                &mut ed,
                Kind::Extrude,
                &Parameters {
                    plane: plane_ref.clone(),
                    distance: 2.0,
                    symmetric,
                    reversed,
                    ..Default::default()
                },
            )
            .unwrap();
            let body = bodies(&ed).remove(0);
            volume(&body, 24.0);
            let distance = if symmetric {
                0.0
            } else if reversed {
                -1.0
            } else {
                1.0
            };
            let center = plane.to_world(DVec2::new(2.0, 1.5)) + plane.normal() * distance;
            assert!((mass_properties(&body).unwrap().centroid - center).length() < 1e-5);
        }
    }
}

#[test]
fn extrusion_on_a_truly_tilted_face_uses_the_resolved_plane() {
    let mut ed = rect_editor();
    let transform = DAffine3::from_axis_angle(DVec3::new(1.0, 2.0, 3.0).normalize(), 0.6);
    let base = add_box(&mut ed, transform);
    let base_body = bodies(&ed).remove(0);
    let face = base_body
        .faces
        .iter()
        .find(|face| face.normal.dot(transform.transform_vector3(DVec3::Z)) > 0.999)
        .unwrap();
    let plane_ref = PlaneRef::Face(FaceRef {
        body: base,
        name: face.name.clone(),
        hint: GeomHint {
            point: face.centroid,
            direction: face.normal,
        },
    });
    assert!(matches!(face.kind, SurfaceKind::Plane { .. }));
    let id = apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            plane: plane_ref,
            distance: 2.0,
            symmetric: true,
            ..Default::default()
        },
    )
    .unwrap();
    let regen = regenerate(&ed.doc.part, &mut RegenCache::new());
    let sketch_id = ed.doc.part.features[1].id;
    let plane = regen
        .sketch_planes
        .iter()
        .find(|(id, _)| *id == sketch_id)
        .unwrap()
        .1;
    assert!(plane.normal().z.abs() < 0.999);
    let body = regen.body(BodyRef(id)).unwrap();
    volume(body, 24.0);
    assert!(
        (mass_properties(body).unwrap().centroid - plane.to_world(DVec2::new(2.0, 1.5))).length()
            < 1e-4
    );
}

#[test]
fn revolving_a_circle_produces_a_torus() {
    let mut ed = editor();
    select(
        &mut ed,
        [EntityKind::Circle(Circle2::new(DVec2::new(3.0, 0.0), 1.0))],
    );
    let drawing = ed.doc.drawing.clone();
    let id = apply(
        &mut ed,
        Kind::Revolve,
        &Parameters {
            plane: PlaneRef::Xz,
            ..Default::default()
        },
    )
    .unwrap();
    let body = bodies(&ed).remove(0);
    assert!(body.is_exact());
    assert_eq!(body.id, BodyRef(id));
    volume(&body, 6.0 * PI * PI);
    assert_eq!(ed.doc.drawing, drawing);
    ed.undo();
    assert!(ed.doc.part.features.is_empty());
    ed.redo();
    volume(&bodies(&ed)[0], 6.0 * PI * PI);
}

#[test]
fn positive_negative_and_full_revolutions_have_expected_ring_volume() {
    for angle in [-90.0, 90.0, 360.0] {
        let mut ed = editor();
        select(
            &mut ed,
            rectangle(DVec2::new(1.0, 0.0), DVec2::new(2.0, 1.0)),
        );
        apply(
            &mut ed,
            Kind::Revolve,
            &Parameters {
                plane: PlaneRef::Xz,
                angle,
                ..Default::default()
            },
        )
        .unwrap();
        volume(&bodies(&ed)[0], 3.0 * PI * angle.abs() / 360.0);
    }
}

#[test]
fn no_selection_missing_entities_and_unsupported_curves_are_noops() {
    let mut ed = editor();
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
    ed.selection.add(EntityId(999));
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
    for entity in [
        EntityKind::Ellipse(EllipseArc2 {
            c: DVec2::ZERO,
            major: DVec2::new(2.0, 0.0),
            ratio: 0.5,
            start: 0.0,
            end: TAU,
        }),
        EntityKind::Spline(Circle2::new(DVec2::ZERO, 1.0).to_nurbs()),
    ] {
        select(&mut ed, [entity]);
        let error = no_change(&mut ed, Kind::Extrude, &Parameters::default());
        assert!(error.contains("no polyline approximation"));
    }
    select(&mut ed, [EntityKind::Point { p: DVec2::ZERO }]);
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
}

#[test]
fn open_or_extra_dangling_boundaries_never_partially_succeed() {
    let mut open = rectangle(DVec2::ZERO, DVec2::ONE);
    open.pop();
    let mut dangling = rectangle(DVec2::ZERO, DVec2::ONE);
    dangling.push(EntityKind::Line(Line2::new(
        DVec2::new(0.5, 0.0),
        DVec2::new(0.5, 2.0),
    )));
    for entities in [
        open,
        dangling,
        vec![EntityKind::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::X, DVec2::ONE, DVec2::Y],
            false,
        ))],
    ] {
        let mut ed = editor();
        select(&mut ed, entities);
        no_change(&mut ed, Kind::Extrude, &Parameters::default());
    }
}

#[test]
fn duplicate_touching_and_self_crossing_boundaries_are_rejected() {
    let circle = EntityKind::Circle(Circle2::new(DVec2::ZERO, 1.0));
    for entities in [
        vec![circle.clone(), circle.clone()],
        vec![
            circle,
            EntityKind::Circle(Circle2::new(DVec2::new(2.0, 0.0), 1.0)),
        ],
        vec![EntityKind::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::ONE, DVec2::Y, DVec2::X],
            true,
        ))],
    ] {
        let mut ed = editor();
        select(&mut ed, entities);
        no_change(&mut ed, Kind::Extrude, &Parameters::default());
    }
}

#[test]
fn invalid_and_degenerate_geometry_is_never_committed() {
    for entity in [
        EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::ZERO)),
        EntityKind::Circle(Circle2::new(DVec2::ZERO, 0.0)),
        EntityKind::Circle(Circle2::new(DVec2::ZERO, f64::NAN)),
        EntityKind::Arc(Arc2::new(DVec2::ZERO, 1.0, 0.0, 0.0)),
        EntityKind::Polyline(Polyline2::from_points(
            [DVec2::ZERO, DVec2::X, DVec2::new(2.0, 0.0)],
            true,
        )),
        EntityKind::Polyline(Polyline2 {
            verts: vec![
                PolyVertex::with_bulge(DVec2::ZERO, f64::MAX),
                PolyVertex::new(DVec2::X),
            ],
            closed: true,
        }),
        EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::new(f64::INFINITY, 1.0))),
    ] {
        let mut ed = editor();
        select(&mut ed, [entity]);
        no_change(&mut ed, Kind::Extrude, &Parameters::default());
    }
}

#[test]
fn invalid_lengths_angles_and_axes_preserve_history() {
    let mut ed = rect_editor();
    for distance in [0.0, -1.0, 1e-12, f64::NAN, f64::INFINITY] {
        no_change(
            &mut ed,
            Kind::Extrude,
            &Parameters {
                distance,
                ..Default::default()
            },
        );
    }
    for angle in [0.0, 361.0, -361.0, f64::NAN, f64::INFINITY] {
        no_change(
            &mut ed,
            Kind::Revolve,
            &Parameters {
                angle,
                plane: PlaneRef::Xz,
                ..Default::default()
            },
        );
    }
    let error = no_change(&mut ed, Kind::Revolve, &Parameters::default());
    assert!(error.contains("axis must lie"));
    select(
        &mut ed,
        [EntityKind::Circle(Circle2::new(DVec2::ZERO, 1.0))],
    );
    assert!(
        no_change(
            &mut ed,
            Kind::Revolve,
            &Parameters {
                plane: PlaneRef::Xz,
                ..Default::default()
            }
        )
        .contains("cross")
    );
}

#[test]
fn missing_boolean_targets_do_not_silently_create_a_new_body() {
    let mut ed = rect_editor();
    for operation in [Operation::Join, Operation::Cut, Operation::Intersect] {
        no_change(
            &mut ed,
            Kind::Extrude,
            &Parameters {
                operation,
                ..Default::default()
            },
        );
    }
    add_box(&mut ed, DAffine3::IDENTITY);
    no_change(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            operation: Operation::Join,
            target: Some(BodyRef(FeatureId(999))),
            ..Default::default()
        },
    );
}

#[test]
fn join_cut_and_intersection_modify_only_the_explicit_target() {
    for (operation, expected) in [
        (Operation::Join, 72.0),
        (Operation::Cut, 56.0),
        (Operation::Intersect, 8.0),
    ] {
        let mut ed = editor();
        let target = add_box(&mut ed, DAffine3::IDENTITY);
        let other = add_box(&mut ed, DAffine3::from_translation(DVec3::splat(20.0)));
        select(
            &mut ed,
            rectangle(DVec2::new(2.0, 1.0), DVec2::new(6.0, 3.0)),
        );
        let before = ed.doc.part.clone();
        let id = apply(
            &mut ed,
            Kind::Extrude,
            &Parameters {
                distance: 2.0,
                operation,
                target: Some(target),
                ..Default::default()
            },
        )
        .unwrap();
        let regen = regenerate(&ed.doc.part, &mut RegenCache::new());
        assert_eq!(regen.bodies.len(), 2);
        assert_eq!(regen.body(target).unwrap().source, id);
        volume(regen.body(target).unwrap(), expected);
        volume(regen.body(other).unwrap(), 64.0);
        ed.undo();
        assert_eq!(ed.doc.part, before);
        ed.redo();
        volume(
            &bodies(&ed).into_iter().find(|b| b.id == target).unwrap(),
            expected,
        );
    }
}

#[test]
fn automatic_target_uses_the_last_body() {
    let mut ed = editor();
    let other = add_box(&mut ed, DAffine3::from_translation(DVec3::splat(20.0)));
    let target = add_box(&mut ed, DAffine3::IDENTITY);
    select(
        &mut ed,
        rectangle(DVec2::new(1.0, 1.0), DVec2::new(3.0, 3.0)),
    );
    apply(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 2.0,
            operation: Operation::Cut,
            ..Default::default()
        },
    )
    .unwrap();
    let regen = regenerate(&ed.doc.part, &mut RegenCache::new());
    volume(regen.body(target).unwrap(), 56.0);
    volume(regen.body(other).unwrap(), 64.0);
}

#[test]
fn regeneration_errors_empty_intersection_and_rollback_leave_no_partial_sketch() {
    let mut ed = rect_editor();
    let target = add_box(&mut ed, DAffine3::from_translation(DVec3::splat(20.0)));
    no_change(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            operation: Operation::Intersect,
            target: Some(target),
            ..Default::default()
        },
    );
    ed.doc.transact("broken feature", |tx| {
        let id = tx.ids().feature();
        tx.part_mut().features.push(Feature {
            id,
            name: "Broken".into(),
            suppressed: false,
            kind: FeatureKind::Primitive {
                shape: Primitive::Sphere { radius: -1.0 },
                placement: DAffine3::IDENTITY,
                op: BodyOp::NewBody,
            },
        });
    });
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
    ed.doc.undo();
    // Failed validation must not clear an existing redo branch.
    no_change(
        &mut ed,
        Kind::Extrude,
        &Parameters {
            distance: 0.0,
            ..Default::default()
        },
    );
    ed.doc
        .transact("rollback", |tx| tx.part_mut().rollback = Some(0));
    no_change(&mut ed, Kind::Extrude, &Parameters::default());
}

#[test]
fn exports_and_native_roundtrips_preserve_allocator_high_water_marks() {
    let mut ed = rect_editor();
    let first = apply(&mut ed, Kind::Extrude, &Parameters::default()).unwrap();
    let original = wcad_doc::file::to_json(&ed.doc, false).unwrap();
    let bodies = bodies(&ed);
    assert!(
        wcad_solid::to_step(&bodies)
            .unwrap()
            .contains("ISO-10303-21")
    );
    assert!(
        wcad_solid::to_obj(&bodies)
            .unwrap()
            .contains(&format!("body_{}", first.0))
    );
    assert!(wcad_solid::to_stl(&bodies).unwrap().len() > 84);
    assert_eq!(wcad_doc::file::to_json(&ed.doc, false).unwrap(), original);
    for compressed in [false, true] {
        let saved = wcad_doc::file::save(&ed.doc, compressed).unwrap();
        let loaded = wcad_doc::file::load(&saved).unwrap();
        assert_eq!(loaded.part, ed.doc.part);
        assert_eq!(loaded.ids(), ed.doc.ids());
        let mut next = editor();
        next.set_document(loaded);
        next.selection
            .set(next.doc.drawing.entities.keys().copied());
        let second = apply(&mut next, Kind::Extrude, &Parameters::default()).unwrap();
        assert!(next.doc.part.features[2].id.0 > first.0);
        assert!(second.0 > first.0);
        let mut sketch = only_sketch(&next).clone();
        let high = sketch.entities.keys().map(|id| id.0).max().unwrap();
        assert!(
            sketch
                .add_circle_center_radius(DVec2::new(100.0, 0.0), 1.0)
                .0
                > high
        );
    }
    ed.undo();
    for compressed in [false, true] {
        let saved = wcad_doc::file::save(&ed.doc, compressed).unwrap();
        let mut next = editor();
        next.set_document(wcad_doc::file::load(&saved).unwrap());
        next.selection
            .set(next.doc.drawing.entities.keys().copied());
        let id = apply(&mut next, Kind::Extrude, &Parameters::default()).unwrap();
        assert!(next.doc.part.features[0].id.0 > first.0);
        assert!(id.0 > first.0);
    }
    let second = apply(&mut ed, Kind::Extrude, &Parameters::default()).unwrap();
    assert!(second.0 > first.0);
    assert!(!ed.doc.can_redo());
}
