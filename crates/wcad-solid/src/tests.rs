//! Kernel and regeneration tests.

use std::f64::consts::{PI, TAU};

use wcad_doc::{
    AxisRef, BodyOp, BodyRef, BooleanKind, EdgeRef, Extent, FaceRef, Feature, FeatureId,
    FeatureKind, GeomHint, Part, PlaneRef, Primitive, ProfileRef, TopoName, TopoTag,
};
use wcad_geom2d::{Arc2, Circle2, Curve2, Line2, Loop, PolyVertex, Polyline2, Region};
use wcad_math::{DAffine3, DVec2, DVec3, Plane};
use wcad_sketch::{SkEntityId, Sketch};

use crate::kernel::{extrude_body, primitive_body, revolve_body};
use crate::*;

fn rel_err(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-12)
}

fn lp(curves: Vec<Curve2>) -> Loop {
    let n = curves.len();
    Loop {
        curves,
        sources: (0..n).collect(),
    }
}

fn lp_from(curves: Vec<Curve2>, first_source: usize) -> Loop {
    let n = curves.len();
    Loop {
        curves,
        sources: (first_source..first_source + n).collect(),
    }
}

fn rect_curves(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Curve2> {
    let p = [
        DVec2::new(x0, y0),
        DVec2::new(x1, y0),
        DVec2::new(x1, y1),
        DVec2::new(x0, y1),
    ];
    (0..4)
        .map(|i| Curve2::Line(Line2::new(p[i], p[(i + 1) % 4])))
        .collect()
}

fn ent(i: usize) -> SkEntityId {
    SkEntityId(i as u32)
}

fn profile(plane: &Plane, region: &Region) -> ProfileFace {
    build_profile_face(plane, region, &ent).expect("profile face")
}

fn volume(b: &Body) -> f64 {
    mass_properties(b).expect("mass").volume
}

fn prim(id: u64, shape: Primitive, placement: DAffine3) -> Body {
    primitive_body(FeatureId(id), &shape, &placement).expect("primitive")
}

fn boxp(id: u64, min: DVec3, max: DVec3) -> Body {
    prim(
        id,
        Primitive::Box { size: max - min },
        DAffine3::from_translation(min),
    )
}

// ------------------------------------------------------------------------------------------------
// Primitives

#[test]
fn box_and_cylinder_volumes() {
    let b = boxp(1, DVec3::ZERO, DVec3::new(2.0, 3.0, 4.0));
    assert!(b.is_exact());
    assert_eq!(b.faces.len(), 6);
    assert_eq!(b.edges.len(), 12);
    assert!(
        rel_err(volume(&b), 24.0) < 1e-9,
        "box volume {}",
        volume(&b)
    );
    let mp = mass_properties(&b).unwrap();
    assert!(rel_err(mp.area, 2.0 * (6.0 + 8.0 + 12.0)) < 1e-9);
    assert!((mp.centroid - DVec3::new(1.0, 1.5, 2.0)).length() < 1e-9);
    assert!(
        b.faces
            .iter()
            .all(|f| matches!(f.kind, SurfaceKind::Plane { .. }))
    );

    let c = prim(
        2,
        Primitive::Cylinder {
            radius: 1.0,
            height: 2.0,
        },
        DAffine3::IDENTITY,
    );
    assert!(
        rel_err(volume(&c), TAU) < 0.01,
        "cylinder volume {}",
        volume(&c)
    );
    assert!(c.faces.iter().any(
        |f| matches!(f.kind, SurfaceKind::Cylinder { radius, .. } if (radius - 1.0).abs() < 1e-6)
    ));
}

#[test]
fn sphere_cone_torus_volumes() {
    let s = prim(
        1,
        Primitive::Sphere { radius: 2.0 },
        DAffine3::from_translation(DVec3::new(1.0, 2.0, 3.0)),
    );
    assert!(
        rel_err(volume(&s), 4.0 / 3.0 * PI * 8.0) < 0.01,
        "sphere {}",
        volume(&s)
    );
    let c = prim(
        2,
        Primitive::Cone {
            radius1: 2.0,
            radius2: 1.0,
            height: 3.0,
        },
        DAffine3::IDENTITY,
    );
    let vc = PI * 3.0 / 3.0 * (4.0 + 2.0 + 1.0);
    assert!(
        rel_err(volume(&c), vc) < 0.01,
        "cone {} vs {vc}",
        volume(&c)
    );
    let apex = prim(
        3,
        Primitive::Cone {
            radius1: 1.0,
            radius2: 0.0,
            height: 3.0,
        },
        DAffine3::IDENTITY,
    );
    assert!(
        rel_err(volume(&apex), PI) < 0.01,
        "apex cone {}",
        volume(&apex)
    );
    let t = prim(
        4,
        Primitive::Torus {
            major: 3.0,
            minor: 1.0,
        },
        DAffine3::IDENTITY,
    );
    let vt = 2.0 * PI * PI * 3.0;
    assert!(
        rel_err(volume(&t), vt) < 0.01,
        "torus {} vs {vt}",
        volume(&t)
    );
}

#[test]
fn invalid_primitives_are_errors() {
    for shape in [
        Primitive::Box {
            size: DVec3::new(1.0, 0.0, 1.0),
        },
        Primitive::Cylinder {
            radius: f64::NAN,
            height: 1.0,
        },
        Primitive::Sphere { radius: -1.0 },
        Primitive::Torus {
            major: 1.0,
            minor: 2.0,
        },
    ] {
        assert!(primitive_body(FeatureId(1), &shape, &DAffine3::IDENTITY).is_err());
    }
    let singular = DAffine3::from_scale(DVec3::new(1.0, 0.0, 1.0));
    assert!(primitive_body(FeatureId(1), &Primitive::Sphere { radius: 1.0 }, &singular).is_err());
}

// ------------------------------------------------------------------------------------------------
// Profiles, extrude, revolve

#[test]
fn extrude_rectangle_with_hole() {
    let region = Region {
        outer: lp(rect_curves(0.0, 0.0, 10.0, 6.0)),
        holes: vec![lp_from(
            vec![Curve2::Circle(Circle2::new(DVec2::new(5.0, 3.0), 1.0))],
            4,
        )],
    };
    let pf = profile(&Plane::XY, &region);
    let (b, w) = extrude_body(FeatureId(7), &[(0, pf)], DVec3::Z, 0.0, 2.0).unwrap();
    assert!(w.is_none());
    let expected = (60.0 - PI) * 2.0;
    assert!(
        rel_err(volume(&b), expected) < 0.01,
        "volume {} vs {expected}",
        volume(&b)
    );
    let has = |tag: TopoTag| {
        b.faces.iter().any(|f| {
            f.name
                == TopoName {
                    feature: FeatureId(7),
                    tag: tag.clone(),
                }
        })
    };
    assert!(has(TopoTag::StartCap { region: 0 }));
    assert!(has(TopoTag::EndCap { region: 0 }));
    for i in 0..4 {
        assert!(
            has(TopoTag::Side {
                entity: ent(i),
                index: 0
            }),
            "side {i}"
        );
    }
    // The hole circle is split into two half arcs → two side faces.
    assert!(has(TopoTag::Side {
        entity: ent(4),
        index: 0
    }));
    assert!(has(TopoTag::Side {
        entity: ent(4),
        index: 1
    }));
    // Caps are planar with outward normals.
    let top = b
        .faces
        .iter()
        .find(|f| matches!(f.name.tag, TopoTag::EndCap { .. }))
        .unwrap();
    assert!((top.normal - DVec3::Z).length() < 1e-9);
    assert!(matches!(top.kind, SurfaceKind::Plane { .. }));
}

#[test]
fn extrude_on_tilted_plane_symmetric_and_reversed() {
    let plane = Plane::from_normal(DVec3::new(1.0, 2.0, 3.0), DVec3::new(1.0, -1.0, 0.5));
    let region = Region {
        outer: lp(rect_curves(-1.0, -1.0, 1.0, 2.0)),
        holes: vec![],
    };
    let pf = profile(&plane, &region);
    let (b, _) = extrude_body(FeatureId(1), &[(0, pf.clone())], -plane.normal(), 1.5, 1.5).unwrap();
    assert!(rel_err(volume(&b), 6.0 * 3.0) < 1e-6, "{}", volume(&b));
    let (b2, _) = extrude_body(FeatureId(1), &[(0, pf)], plane.normal(), 0.0, 0.5).unwrap();
    assert!(rel_err(volume(&b2), 3.0) < 1e-6);
}

#[test]
fn slot_profile_with_reversed_arcs() {
    // Slot: two lines and two semicircles; the second arc is traversed against its CCW direction.
    let (r, l) = (1.0, 4.0);
    let curves = vec![
        Curve2::Line(Line2::new(DVec2::new(0.0, -r), DVec2::new(l, -r))),
        Curve2::Arc(Arc2::new(DVec2::new(l, 0.0), r, -PI / 2.0, PI / 2.0)),
        Curve2::Line(Line2::new(DVec2::new(l, r), DVec2::new(0.0, r))),
        Curve2::Arc(Arc2::new(DVec2::new(0.0, 0.0), r, PI / 2.0, 1.5 * PI)),
    ];
    let mut reversed = curves.clone();
    reversed.reverse();
    for cs in [curves, reversed] {
        let pf = profile(
            &Plane::XY,
            &Region {
                outer: lp(cs),
                holes: vec![],
            },
        );
        let (b, _) = extrude_body(FeatureId(1), &[(0, pf)], DVec3::Z, 0.0, 1.0).unwrap();
        let expected = 2.0 * r * l + PI * r * r;
        assert!(
            rel_err(volume(&b), expected) < 0.01,
            "{} vs {expected}",
            volume(&b)
        );
    }
}

#[test]
fn polyline_bulge_profile() {
    // Rounded rectangle-ish: square with one semicircular bulge (bulge = 1 → 180°).
    let pl = Polyline2 {
        verts: vec![
            PolyVertex::new(DVec2::new(0.0, 0.0)),
            PolyVertex::with_bulge(DVec2::new(2.0, 0.0), 1.0),
            PolyVertex::new(DVec2::new(2.0, 2.0)),
            PolyVertex::new(DVec2::new(0.0, 2.0)),
        ],
        closed: true,
    };
    let pf = profile(
        &Plane::XY,
        &Region {
            outer: lp(vec![Curve2::Polyline(pl)]),
            holes: vec![],
        },
    );
    let (b, _) = extrude_body(FeatureId(1), &[(0, pf)], DVec3::Z, 0.0, 1.0).unwrap();
    let expected = 4.0 + PI * 0.5;
    assert!(
        rel_err(volume(&b), expected) < 0.01,
        "{} vs {expected}",
        volume(&b)
    );
}

#[test]
fn ellipse_profile() {
    let e = wcad_geom2d::EllipseArc2 {
        c: DVec2::new(1.0, 1.0),
        major: DVec2::new(2.0, 0.0),
        ratio: 0.5,
        start: 0.0,
        end: TAU,
    };
    let pf = profile(
        &Plane::XY,
        &Region {
            outer: lp(vec![Curve2::Ellipse(e)]),
            holes: vec![],
        },
    );
    let (b, _) = extrude_body(FeatureId(1), &[(0, pf)], DVec3::Z, 0.0, 1.0).unwrap();
    assert!(rel_err(volume(&b), PI * 2.0 * 1.0) < 0.01, "{}", volume(&b));
}

#[test]
fn revolve_full_and_partial() {
    // Rectangle x∈[1,2], z∈[0,1] in the XZ plane (plane coords (x, z)), revolved about Z.
    let region = Region {
        outer: lp(rect_curves(1.0, 0.0, 2.0, 1.0)),
        holes: vec![],
    };
    let pf = profile(&Plane::XZ, &region);
    let (b, _) =
        revolve_body(FeatureId(3), &[(0, pf.clone())], DVec3::ZERO, DVec3::Z, TAU).unwrap();
    assert!(rel_err(volume(&b), 3.0 * PI) < 0.01, "full {}", volume(&b));
    let (q, _) = revolve_body(
        FeatureId(3),
        &[(0, pf.clone())],
        DVec3::ZERO,
        DVec3::Z,
        PI / 2.0,
    )
    .unwrap();
    assert!(
        rel_err(volume(&q), 3.0 * PI / 4.0) < 0.01,
        "quarter {}",
        volume(&q)
    );
    assert!(
        q.faces
            .iter()
            .any(|f| matches!(f.name.tag, TopoTag::StartCap { .. }))
    );
    assert!(
        q.faces
            .iter()
            .any(|f| matches!(f.name.tag, TopoTag::EndCap { .. }))
    );
    let (n, _) = revolve_body(FeatureId(3), &[(0, pf)], DVec3::ZERO, DVec3::Z, -PI / 2.0).unwrap();
    assert!(
        rel_err(volume(&n), 3.0 * PI / 4.0) < 0.01,
        "negative {}",
        volume(&n)
    );
    // Crossing the axis is rejected.
    let bad = profile(
        &Plane::XZ,
        &Region {
            outer: lp(rect_curves(-1.0, 0.0, 1.0, 1.0)),
            holes: vec![],
        },
    );
    assert!(revolve_body(FeatureId(3), &[(0, bad)], DVec3::ZERO, DVec3::Z, TAU).is_err());
}

#[test]
fn revolve_profile_touching_axis() {
    let region = Region {
        outer: lp(rect_curves(0.0, 0.0, 1.0, 2.0)),
        holes: vec![],
    };
    let pf = profile(&Plane::XZ, &region);
    match revolve_body(FeatureId(3), &[(0, pf)], DVec3::ZERO, DVec3::Z, TAU) {
        Ok((b, _)) => assert!(
            rel_err(volume(&b), TAU) < 0.02,
            "touching axis {}",
            volume(&b)
        ),
        Err(e) => panic!("revolve touching the axis failed: {e}"),
    }
}

// ------------------------------------------------------------------------------------------------
// Booleans

#[test]
fn plate_with_four_chained_holes() {
    let mut plate = boxp(1, DVec3::ZERO, DVec3::new(4.0, 3.0, 0.5));
    let mut warnings = 0;
    for (k, (x, y)) in [(0.5, 0.5), (3.5, 0.5), (0.5, 2.5), (3.5, 2.5)]
        .into_iter()
        .enumerate()
    {
        let cyl = prim(
            10 + k as u64,
            Primitive::Cylinder {
                radius: 0.2,
                height: 1.5,
            },
            DAffine3::from_translation(DVec3::new(x, y, -0.5)),
        );
        let out = boolean_bodies(
            &plate,
            &[cyl],
            BooleanKind::Subtract,
            FeatureId(20 + k as u64),
        )
        .unwrap();
        warnings += out.warning.is_some() as usize;
        plate = out.body;
    }
    let expected = 6.0 - 4.0 * PI * 0.04 * 0.5;
    let v = volume(&plate);
    eprintln!(
        "plate: exact={} warnings={warnings} volume={v} expected={expected}",
        plate.is_exact()
    );
    assert!(
        rel_err(v, expected) < 0.01,
        "plate volume {v} vs {expected}"
    );
    assert_eq!(plate.id, BodyRef(FeatureId(1)));
    // The top face keeps its primitive name through all cuts.
    let top_name = TopoName {
        feature: FeatureId(1),
        tag: TopoTag::PrimitiveFace { index: 1 },
    };
    assert!(
        plate.faces.iter().any(|f| f.name == top_name),
        "names: {:?}",
        plate.faces.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[test]
fn coplanar_union_falls_back_to_mesh() {
    let a = boxp(1, DVec3::ZERO, DVec3::ONE);
    let b = boxp(2, DVec3::new(0.5, 0.5, 0.0), DVec3::new(1.5, 1.5, 1.0));
    let out = boolean_bodies(&a, &[b], BooleanKind::Union, FeatureId(3)).unwrap();
    let v = volume(&out.body);
    eprintln!(
        "coplanar union: exact={} warning={:?} volume={v}",
        out.body.is_exact(),
        out.warning
    );
    assert!(rel_err(v, 1.75) < 0.01, "volume {v}");
    assert_eq!(out.warning.is_some(), !out.body.is_exact());
    assert!(
        !out.body.is_exact(),
        "expected the mesh fallback for a coplanar union"
    );
    assert!(out.body.mesh_only_reason.is_some());
    // Mesh faces still carry the input names.
    assert!(
        out.body
            .faces
            .iter()
            .any(|f| f.name.feature == FeatureId(1))
    );
    assert!(
        out.body
            .faces
            .iter()
            .any(|f| f.name.feature == FeatureId(2))
    );
    let m = tessellate(&out.body, 0.0).unwrap();
    assert!(rel_err(m.volume(), 1.75) < 0.01);
    assert_eq!(m.face_ranges.len(), out.body.faces.len());
    // Exact ops on the mesh body keep working through the mesh kernel.
    let c = boxp(4, DVec3::new(0.25, 0.25, -1.0), DVec3::new(0.75, 0.75, 2.0));
    let cut = boolean_bodies(&out.body, &[c], BooleanKind::Subtract, FeatureId(5)).unwrap();
    assert!(
        rel_err(volume(&cut.body), 1.5) < 0.01,
        "{}",
        volume(&cut.body)
    );
}

#[test]
fn disjoint_union_and_subtract() {
    let a = boxp(1, DVec3::ZERO, DVec3::ONE);
    let b = boxp(2, DVec3::new(3.0, 0.0, 0.0), DVec3::new(4.0, 1.0, 1.0));
    let u = boolean_bodies(
        &a,
        std::slice::from_ref(&b),
        BooleanKind::Union,
        FeatureId(3),
    )
    .unwrap();
    assert!(u.body.is_exact() && u.warning.is_none());
    assert!(rel_err(volume(&u.body), 2.0) < 1e-9);
    let s = boolean_bodies(
        &a,
        std::slice::from_ref(&b),
        BooleanKind::Subtract,
        FeatureId(3),
    )
    .unwrap();
    assert!(rel_err(volume(&s.body), 1.0) < 1e-9);
    assert!(boolean_bodies(&a, &[b], BooleanKind::Intersect, FeatureId(3)).is_err());
}

#[test]
fn overlapping_union_and_intersect_exact() {
    let a = boxp(1, DVec3::ZERO, DVec3::ONE);
    let b = boxp(2, DVec3::splat(0.5), DVec3::splat(1.5));
    let u = boolean_bodies(
        &a,
        std::slice::from_ref(&b),
        BooleanKind::Union,
        FeatureId(3),
    )
    .unwrap();
    assert!(
        rel_err(volume(&u.body), 1.875) < 0.01,
        "{}",
        volume(&u.body)
    );
    let i = boolean_bodies(&a, &[b], BooleanKind::Intersect, FeatureId(3)).unwrap();
    assert!(
        rel_err(volume(&i.body), 0.125) < 0.01,
        "{}",
        volume(&i.body)
    );
}

// ------------------------------------------------------------------------------------------------
// Fillet / chamfer

#[test]
fn fillet_box_edge() {
    let b = boxp(1, DVec3::ZERO, DVec3::ONE);
    // A vertical edge: direction ±Z.
    let ei = b
        .edges
        .iter()
        .position(|e| e.direction.z.abs() > 0.99)
        .expect("vertical edge");
    let out = crate::kernel::fillet_body(&b, &[ei], 0.1, false, FeatureId(2)).expect("fillet");
    let expected = 1.0 - (1.0 - PI / 4.0) * 0.01;
    assert!(
        rel_err(volume(&out), expected) < 0.005,
        "fillet volume {} vs {expected}",
        volume(&out)
    );
    assert!(out.faces.iter().any(|f| f.name
        == TopoName {
            feature: FeatureId(2),
            tag: TopoTag::Blend { index: 0 }
        }));
    // All six primitive faces survive with their names.
    for i in 0..6 {
        let n = TopoName {
            feature: FeatureId(1),
            tag: TopoTag::PrimitiveFace { index: i },
        };
        assert!(out.faces.iter().any(|f| f.name == n), "face {i} lost");
    }
    let ch = crate::kernel::fillet_body(&b, &[ei], 0.1, true, FeatureId(2)).expect("chamfer");
    assert!(
        rel_err(volume(&ch), 1.0 - 0.005) < 0.005,
        "chamfer {}",
        volume(&ch)
    );
    // Mesh-only bodies cannot be filleted.
    let a = boxp(1, DVec3::ZERO, DVec3::ONE);
    let c = boxp(2, DVec3::new(0.5, 0.5, 0.0), DVec3::new(1.5, 1.5, 1.0));
    let m = boolean_bodies(&a, &[c], BooleanKind::Union, FeatureId(3))
        .unwrap()
        .body;
    if !m.is_exact() {
        assert!(matches!(
            crate::kernel::fillet_body(&m, &[0], 0.1, false, FeatureId(4)),
            Err(Error::Unsupported(_))
        ));
    }
}

// ------------------------------------------------------------------------------------------------
// Tessellation and export

#[test]
fn tessellation_maps_and_exports() {
    let region = Region {
        outer: lp(rect_curves(0.0, 0.0, 4.0, 2.0)),
        holes: vec![lp_from(
            vec![Curve2::Circle(Circle2::new(DVec2::new(2.0, 1.0), 0.5))],
            4,
        )],
    };
    let (b, _) = extrude_body(
        FeatureId(1),
        &[(0, profile(&Plane::XY, &region))],
        DVec3::Z,
        0.0,
        1.0,
    )
    .unwrap();
    let m = tessellate(&b, 0.0).unwrap();
    assert_eq!(m.face_ranges.len(), b.faces.len());
    assert_eq!(m.face_names.len(), b.faces.len());
    assert_eq!(m.edges.len(), b.edges.len());
    assert_eq!(m.normals.len(), m.positions.len());
    assert!(m.indices.iter().all(|&i| (i as usize) < m.positions.len()));
    for (fi, r) in m.face_ranges.iter().enumerate() {
        if r.end > r.start {
            assert_eq!(m.face_of_triangle((r.start / 3) as usize), Some(fi));
        }
    }
    let expected = 8.0 - PI * 0.25;
    assert!(rel_err(m.volume(), expected) < 0.01);
    let fine = tessellate(&b, 0.0005).unwrap();
    assert!(fine.triangle_count() >= m.triangle_count());

    let stl = to_stl(std::slice::from_ref(&b)).unwrap();
    let n = u32::from_le_bytes([stl[80], stl[81], stl[82], stl[83]]) as usize;
    assert_eq!(stl.len(), 84 + 50 * n);
    assert_eq!(n, m.triangle_count());
    let obj = to_obj(std::slice::from_ref(&b)).unwrap();
    assert!(obj.contains("\nv ") && obj.contains("\nf "));
    let step = to_step(std::slice::from_ref(&b)).unwrap();
    assert!(
        step.starts_with("ISO-10303-21;")
            && step.contains("MANIFOLD_SOLID_BREP")
            && step.contains("END-ISO-10303-21;")
    );
    // Boolean results (intersection curves) export too.
    let a = boxp(1, DVec3::ZERO, DVec3::ONE);
    let c = prim(
        2,
        Primitive::Cylinder {
            radius: 0.25,
            height: 2.0,
        },
        DAffine3::from_translation(DVec3::new(0.5, 0.5, -0.5)),
    );
    let holed = boolean_bodies(&a, &[c], BooleanKind::Subtract, FeatureId(3))
        .unwrap()
        .body;
    assert!(holed.is_exact());
    let step = to_step(&[b.clone(), holed]).unwrap();
    assert!(step.matches("MANIFOLD_SOLID_BREP").count() >= 2);
}

#[test]
fn adjacent_regions_merge_in_2d() {
    // Two rectangles sharing the edge x = 2, with a disk hole in the right one.
    let left = Region {
        outer: lp(rect_curves(0.0, 0.0, 2.0, 2.0)),
        holes: vec![],
    };
    let right = Region {
        outer: lp_from(rect_curves(2.0, 0.0, 5.0, 2.0), 4),
        holes: vec![lp_from(
            vec![Curve2::Circle(Circle2::new(DVec2::new(3.5, 1.0), 0.5))],
            8,
        )],
    };
    let merged = crate::profile::merge_regions(&[&left, &right]).expect("merge");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].holes.len(), 1);
    assert_eq!(merged[0].outer.curves.len(), 6);
    let pf = profile(&Plane::XY, &merged[0]);
    let (b, w) = extrude_body(FeatureId(1), &[(0, pf)], DVec3::Z, 0.0, 1.0).unwrap();
    assert!(w.is_none() && b.is_exact());
    assert!(
        rel_err(volume(&b), 10.0 - PI * 0.25) < 0.01,
        "{}",
        volume(&b)
    );
    // The shared edge (source 1 of the left, source 7 of the right) produced no face.
    assert!(!b.faces.iter().any(|f| f.name.tag
        == TopoTag::Side {
            entity: ent(1),
            index: 0
        }));
    assert!(!b.faces.iter().any(|f| f.name.tag
        == TopoTag::Side {
            entity: ent(7),
            index: 0
        }));
    // Disjoint regions stay separate.
    let far = Region {
        outer: lp_from(rect_curves(10.0, 0.0, 11.0, 1.0), 20),
        holes: vec![],
    };
    assert_eq!(
        crate::profile::merge_regions(&[&left, &far]).unwrap().len(),
        2
    );
}

#[test]
fn simple_region_nesting() {
    let mut curves = rect_curves(0.0, 0.0, 10.0, 10.0);
    curves.push(Curve2::Circle(Circle2::new(DVec2::new(5.0, 5.0), 2.0)));
    curves.push(Curve2::Circle(Circle2::new(DVec2::new(5.0, 5.0), 1.0)));
    let regions = simple_regions(&curves, 1e-9);
    // Outer rect with a hole (circle r=2) and the inner disk r=1 (depth 2).
    assert_eq!(regions.len(), 2);
    let with_hole = regions.iter().find(|r| r.outer.curves.len() == 4).unwrap();
    assert_eq!(with_hole.holes.len(), 1);
    assert_eq!(with_hole.outer.sources.len(), 4);
}

// ------------------------------------------------------------------------------------------------
// Regeneration

fn rect_sketch(x0: f64, y0: f64, x1: f64, y1: f64, hole: Option<(DVec2, f64)>) -> Sketch {
    let mut s = Sketch::new();
    let p = [
        s.add_point(DVec2::new(x0, y0)),
        s.add_point(DVec2::new(x1, y0)),
        s.add_point(DVec2::new(x1, y1)),
        s.add_point(DVec2::new(x0, y1)),
    ];
    for i in 0..4 {
        s.add_line(p[i], p[(i + 1) % 4]);
    }
    if let Some((c, r)) = hole {
        let cp = s.add_point(c);
        s.add_circle(cp, r);
    }
    s
}

fn feature(id: u64, kind: FeatureKind) -> Feature {
    Feature {
        id: FeatureId(id),
        name: format!("F{id}"),
        suppressed: false,
        kind,
    }
}

fn extrude(id: u64, sketch: u64, extent: Extent, op: BodyOp) -> Feature {
    feature(
        id,
        FeatureKind::Extrude {
            profile: ProfileRef {
                sketch: FeatureId(sketch),
                regions: vec![],
            },
            extent,
            reversed: false,
            op,
        },
    )
}

fn base_part(distance: f64) -> Part {
    Part {
        features: vec![
            feature(
                1,
                FeatureKind::Sketch {
                    plane: PlaneRef::Xy,
                    sketch: rect_sketch(0.0, 0.0, 10.0, 6.0, Some((DVec2::new(5.0, 3.0), 1.0))),
                },
            ),
            extrude(2, 1, Extent::Blind { distance }, BodyOp::NewBody),
        ],
        rollback: None,
    }
}

fn names_of(b: &Body) -> Vec<String> {
    let mut v: Vec<String> = b.faces.iter().map(|f| format!("{:?}", f.name)).collect();
    v.sort();
    v
}

#[test]
fn regenerate_extrude_and_cache_reuse() {
    let part = base_part(2.0);
    let mut cache = RegenCache::new();
    let r = regenerate(&part, &mut cache);
    assert_eq!(r.feature_status.len(), 2);
    assert!(
        r.feature_status
            .iter()
            .all(|(_, s)| *s == FeatureStatus::Ok),
        "{:?}",
        r.feature_status
    );
    assert_eq!(r.bodies.len(), 1);
    assert_eq!(r.sketch_planes, vec![(FeatureId(1), Plane::XY)]);
    assert!(rel_err(volume(&r.bodies[0]), (60.0 - PI) * 2.0) < 0.01);
    assert_eq!(
        cache.stats(),
        RegenStats {
            reused: 0,
            computed: 2
        }
    );

    let r2 = regenerate(&part, &mut cache);
    assert_eq!(
        cache.stats(),
        RegenStats {
            reused: 2,
            computed: 0
        }
    );
    assert_eq!(r2.bodies.len(), 1);

    // Editing feature 2 reuses feature 1.
    let part3 = base_part(3.0);
    let r3 = regenerate(&part3, &mut cache);
    assert_eq!(
        cache.stats(),
        RegenStats {
            reused: 1,
            computed: 1
        }
    );
    assert!(rel_err(volume(&r3.bodies[0]), (60.0 - PI) * 3.0) < 0.01);

    // Suppressing the extrude removes the body; rollback likewise.
    let mut part4 = part3.clone();
    part4.features[1].suppressed = true;
    let r4 = regenerate(&part4, &mut cache);
    assert!(r4.bodies.is_empty());
    assert_eq!(r4.feature_status.len(), 1);
}

#[test]
fn naming_stable_after_distance_change() {
    let mut cache = RegenCache::new();
    let a = regenerate(&base_part(2.0), &mut cache);
    let b = regenerate(&base_part(5.0), &mut cache);
    assert_eq!(names_of(&a.bodies[0]), names_of(&b.bodies[0]));
    let mut ea: Vec<String> = a.bodies[0]
        .edges
        .iter()
        .map(|e| format!("{:?}", e.faces))
        .collect();
    let mut eb: Vec<String> = b.bodies[0]
        .edges
        .iter()
        .map(|e| format!("{:?}", e.faces))
        .collect();
    ea.sort();
    eb.sort();
    assert_eq!(ea, eb);
}

#[test]
fn regenerate_sketch_on_face_cut_and_fillet() {
    let top = TopoName {
        feature: FeatureId(2),
        tag: TopoTag::EndCap { region: 0 },
    };
    // Edge between the side face of line 0 (entity index 4: points take ids 0..3) and the top cap.
    let side0 = TopoName {
        feature: FeatureId(2),
        tag: TopoTag::Side {
            entity: SkEntityId(4),
            index: 0,
        },
    };
    let make = |d: f64| {
        let mut part = base_part(d);
        part.features.push(feature(
            3,
            FeatureKind::Sketch {
                plane: PlaneRef::Face(FaceRef {
                    body: BodyRef(FeatureId(2)),
                    name: top.clone(),
                    hint: GeomHint {
                        point: DVec3::new(1.0, 1.0, d),
                        direction: DVec3::Z,
                    },
                }),
                sketch: {
                    let mut s = Sketch::new();
                    let c = s.add_point(DVec2::new(8.0, 3.0));
                    s.add_circle(c, 0.5);
                    s
                },
            },
        ));
        part.features.push(extrude(
            4,
            3,
            Extent::ThroughAll,
            BodyOp::Cut { target: None },
        ));
        part.features.push(feature(
            5,
            FeatureKind::Fillet {
                edges: vec![EdgeRef {
                    body: BodyRef(FeatureId(2)),
                    faces: [side0.clone(), top.clone()],
                    hint: GeomHint {
                        point: DVec3::new(5.0, 0.0, d),
                        direction: DVec3::X,
                    },
                }],
                radius: 0.3,
            },
        ));
        part
    };
    let mut cache = RegenCache::new();
    for d in [2.0, 3.0] {
        let r = regenerate(&make(d), &mut cache);
        eprintln!("d={d}: {:?}", r.feature_status);
        assert_eq!(r.feature_status.len(), 5);
        for (id, s) in &r.feature_status {
            assert!(!s.is_error(), "feature {id:?} failed: {s:?}");
        }
        let plane = r
            .sketch_planes
            .iter()
            .find(|(f, _)| *f == FeatureId(3))
            .unwrap()
            .1;
        assert!((plane.origin.z - d).abs() < 1e-9 && (plane.normal() - DVec3::Z).length() < 1e-9);
        let body = &r.bodies[0];
        let fillet_loss = (1.0 - PI / 4.0) * 0.09 * 10.0;
        let expected = (60.0 - PI - PI * 0.25) * d - fillet_loss;
        assert!(
            rel_err(volume(body), expected) < 0.01,
            "d={d}: volume {} vs {expected}",
            volume(body)
        );
    }
}

#[test]
fn regenerate_errors_do_not_panic() {
    let mut part = base_part(0.0);
    // Revolve crossing its axis, unknown sketch, fillet on missing body, boolean on missing body.
    part.features.push(feature(
        3,
        FeatureKind::Revolve {
            profile: ProfileRef {
                sketch: FeatureId(1),
                regions: vec![],
            },
            axis: AxisRef::Z,
            angle: 1.0,
            op: BodyOp::NewBody,
        },
    ));
    part.features.push(extrude(
        4,
        99,
        Extent::Blind { distance: 1.0 },
        BodyOp::NewBody,
    ));
    part.features.push(feature(
        5,
        FeatureKind::Fillet {
            edges: vec![],
            radius: 1.0,
        },
    ));
    part.features.push(feature(
        6,
        FeatureKind::Boolean {
            target: BodyRef(FeatureId(42)),
            tools: vec![BodyRef(FeatureId(2))],
            kind: BooleanKind::Union,
            keep_tools: false,
        },
    ));
    part.features.push(feature(
        7,
        FeatureKind::Primitive {
            shape: Primitive::Sphere {
                radius: f64::INFINITY,
            },
            placement: DAffine3::IDENTITY,
            op: BodyOp::NewBody,
        },
    ));
    part.features.push(extrude(
        8,
        1,
        Extent::ThroughAll,
        BodyOp::Join { target: None },
    ));
    let mut cache = RegenCache::new();
    let r = regenerate(&part, &mut cache);
    assert_eq!(r.feature_status.len(), 8);
    assert_eq!(r.feature_status[0].1, FeatureStatus::Ok);
    for (id, s) in &r.feature_status[1..] {
        assert!(s.is_error(), "feature {id:?} should fail: {s:?}");
    }
    assert!(r.bodies.is_empty());
}

#[test]
fn regenerate_primitives_patterns_mirror() {
    let part = Part {
        features: vec![
            feature(
                1,
                FeatureKind::Primitive {
                    shape: Primitive::Box { size: DVec3::ONE },
                    placement: DAffine3::IDENTITY,
                    op: BodyOp::NewBody,
                },
            ),
            feature(
                2,
                FeatureKind::LinearPattern {
                    body: BodyRef(FeatureId(1)),
                    direction: DVec3::X,
                    count: 3,
                    spacing: 2.0,
                },
            ),
            feature(
                3,
                FeatureKind::Mirror {
                    body: BodyRef(FeatureId(1)),
                    plane: PlaneRef::Offset {
                        base: Box::new(PlaneRef::Yz),
                        distance: -1.0,
                    },
                    join: true,
                },
            ),
            feature(
                4,
                FeatureKind::Primitive {
                    shape: Primitive::Cylinder {
                        radius: 0.25,
                        height: 3.0,
                    },
                    placement: DAffine3::from_translation(DVec3::new(0.5, 0.5, -1.0)),
                    op: BodyOp::Cut {
                        target: Some(BodyRef(FeatureId(1))),
                    },
                },
            ),
            feature(
                5,
                FeatureKind::Primitive {
                    shape: Primitive::Sphere { radius: 0.5 },
                    placement: DAffine3::from_translation(DVec3::new(0.0, 5.0, 0.0)),
                    op: BodyOp::NewBody,
                },
            ),
            feature(
                6,
                FeatureKind::CircularPattern {
                    body: BodyRef(FeatureId(5)),
                    axis: AxisRef::Z,
                    count: 4,
                    angle: TAU,
                },
            ),
        ],
        rollback: None,
    };
    let mut cache = RegenCache::new();
    let r = regenerate(&part, &mut cache);
    eprintln!("{:?}", r.feature_status);
    for (id, s) in &r.feature_status {
        assert!(!s.is_error(), "feature {id:?}: {s:?}");
    }
    assert_eq!(r.bodies.len(), 2);
    let boxes = r.body(BodyRef(FeatureId(1))).unwrap();
    let expected = 6.0 - PI * 0.0625;
    assert!(
        rel_err(volume(boxes), expected) < 0.01,
        "boxes {} vs {expected}",
        volume(boxes)
    );
    let spheres = r.body(BodyRef(FeatureId(5))).unwrap();
    assert!(
        rel_err(volume(spheres), 4.0 * 4.0 / 3.0 * PI * 0.125) < 0.02,
        "spheres {}",
        volume(spheres)
    );

    let rolled = Part {
        rollback: Some(1),
        ..part
    };
    let r1 = regenerate(&rolled, &mut cache);
    assert_eq!(r1.feature_status.len(), 1);
    assert_eq!(cache.stats().reused, 1);
}

#[test]
fn regenerate_revolve_about_sketch_line() {
    let mut s = rect_sketch(1.0, 0.0, 2.0, 1.0, None);
    let a = s.add_point(DVec2::new(0.0, 0.0));
    let b = s.add_point(DVec2::new(0.0, 1.0));
    let axis = s.add_line(a, b);
    if let Some(e) = s.entities.get_mut(&axis) {
        e.construction = true;
    }
    let part = Part {
        features: vec![
            feature(
                1,
                FeatureKind::Sketch {
                    plane: PlaneRef::Xz,
                    sketch: s,
                },
            ),
            feature(
                2,
                FeatureKind::Revolve {
                    profile: ProfileRef {
                        sketch: FeatureId(1),
                        regions: vec![DVec2::new(1.5, 0.5)],
                    },
                    axis: AxisRef::SketchLine {
                        sketch: FeatureId(1),
                        line: axis,
                    },
                    angle: PI,
                    op: BodyOp::NewBody,
                },
            ),
        ],
        rollback: None,
    };
    let r = regenerate(&part, &mut RegenCache::new());
    assert!(
        r.feature_status
            .iter()
            .all(|(_, s)| *s == FeatureStatus::Ok),
        "{:?}",
        r.feature_status
    );
    assert!(
        rel_err(volume(&r.bodies[0]), 1.5 * PI) < 0.01,
        "{}",
        volume(&r.bodies[0])
    );
    let caps = r.bodies[0]
        .faces
        .iter()
        .filter(|f| {
            matches!(
                f.name.tag,
                TopoTag::StartCap { .. } | TopoTag::EndCap { .. }
            )
        })
        .count();
    assert_eq!(caps, 2);
}
