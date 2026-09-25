//! DXF/DWG round trips of every entity kind, all in memory.

mod common;

use wcad_doc::*;
use wcad_geom2d::Curve2;
use wcad_io::{DxfVersion, dwg, dxf};
use wcad_math::{DVec2, normalize_pi};

const EPS: f64 = 1e-6;

fn close(a: f64, b: f64, what: &str) {
    assert!((a - b).abs() <= EPS * (1.0 + a.abs().max(b.abs())), "{what}: {a} vs {b}");
}

fn close_angle(a: f64, b: f64, what: &str) {
    assert!(normalize_pi(a - b).abs() <= 1e-6, "{what}: {a} vs {b}");
}

fn close_pt(a: DVec2, b: DVec2, what: &str) {
    assert!(a.distance(b) <= EPS * (1.0 + a.length().max(b.length())), "{what}: {a:?} vs {b:?}");
}

fn close_curve(a: &Curve2, b: &Curve2, what: &str) {
    match (a, b) {
        (Curve2::Line(x), Curve2::Line(y)) => {
            close_pt(x.a, y.a, what);
            close_pt(x.b, y.b, what);
        }
        (Curve2::Circle(x), Curve2::Circle(y)) => {
            close_pt(x.c, y.c, what);
            close(x.r, y.r, what);
        }
        (Curve2::Arc(x), Curve2::Arc(y)) => {
            close_pt(x.c, y.c, what);
            close(x.r, y.r, what);
            close_angle(x.start, y.start, what);
            close_angle(x.end, y.end, what);
        }
        (Curve2::Ellipse(x), Curve2::Ellipse(y)) => {
            close_pt(x.c, y.c, what);
            close_pt(x.major, y.major, what);
            close(x.ratio, y.ratio, what);
            if x.is_full() {
                assert!(y.is_full(), "{what}: full ellipse became {y:?}");
            } else {
                close_angle(x.start, y.start, what);
                close_angle(x.end, y.end, what);
            }
        }
        (Curve2::Polyline(x), Curve2::Polyline(y)) => {
            assert_eq!(x.closed, y.closed, "{what}: closed");
            assert_eq!(x.verts.len(), y.verts.len(), "{what}: vertex count");
            for (p, q) in x.verts.iter().zip(&y.verts) {
                close_pt(p.p, q.p, what);
                close(p.bulge, q.bulge, what);
            }
        }
        (Curve2::Spline(x), Curve2::Spline(y)) => {
            assert_eq!(x.degree, y.degree, "{what}: degree");
            assert_eq!(x.ctrl.len(), y.ctrl.len(), "{what}: control points");
            for (p, q) in x.ctrl.iter().zip(&y.ctrl) {
                close_pt(*p, *q, what);
            }
            assert_eq!(x.knots.len(), y.knots.len(), "{what}: knots");
            for (p, q) in x.knots.iter().zip(&y.knots) {
                close(*p, *q, what);
            }
            assert_eq!(x.weights.len(), y.weights.len(), "{what}: weights");
            for (p, q) in x.weights.iter().zip(&y.weights) {
                close(*p, *q, what);
            }
        }
        _ => panic!("{what}: curve kind changed: {a:?} vs {b:?}"),
    }
}

/// R2000 has no true colors: they are written as the nearest ACI.
fn assert_color(a: Color, b: Color, what: &str) {
    if what.contains("R2000") && matches!((a, b), (Color::Rgb(..), Color::Aci(_))) {
        return;
    }
    assert_eq!(a, b, "{what}: color");
}

fn ltname(d: &Drawing, r: LinetypeRef) -> String {
    match r {
        LinetypeRef::ByLayer => "BYLAYER".into(),
        LinetypeRef::ByBlock => "BYBLOCK".into(),
        LinetypeRef::Id(id) => d.tables.linetypes.get(&id).map(|l| l.name.to_ascii_uppercase()).unwrap_or_default(),
    }
}

fn compare_entity(da: &Drawing, a: &Entity, db: &Drawing, b: &Entity, what: &str) {
    let what = format!("{what} {}", a.kind.type_name());
    assert_eq!(
        da.layer(a.layer).map(|l| l.name.to_ascii_uppercase()),
        db.layer(b.layer).map(|l| l.name.to_ascii_uppercase()),
        "{what}: layer"
    );
    assert_color(a.color, b.color, &what);
    assert_eq!(ltname(da, a.linetype), ltname(db, b.linetype), "{what}: linetype");
    close(a.linetype_scale, b.linetype_scale, &format!("{what}: ltscale"));
    assert_eq!(a.lineweight, b.lineweight, "{what}: lineweight");
    match (&a.kind, &b.kind) {
        (EntityKind::Point { p }, EntityKind::Point { p: q }) => close_pt(*p, *q, &what),
        (EntityKind::Text(x), EntityKind::Text(y)) => {
            close_pt(x.pos, y.pos, &what);
            close(x.height, y.height, &what);
            close_angle(x.rotation, y.rotation, &format!("{what} rotation"));
            close(x.width_factor, y.width_factor, &format!("{what} width"));
            close_angle(x.oblique, y.oblique, &format!("{what} oblique"));
            assert_eq!((x.halign, x.valign), (y.halign, y.valign), "{what}: alignment");
            assert_eq!(x.text, y.text, "{what}: text");
            assert_eq!(da.tables.text_styles[&x.style].name, db.tables.text_styles[&y.style].name, "{what}: style");
        }
        (EntityKind::MText(x), EntityKind::MText(y)) => {
            close_pt(x.pos, y.pos, &what);
            close(x.height, y.height, &what);
            close(x.width, y.width, &what);
            close_angle(x.rotation, y.rotation, &what);
            close(x.line_spacing, y.line_spacing, &what);
            assert_eq!(x.attachment, y.attachment, "{what}: attachment");
            assert_eq!(x.text, y.text, "{what}: text");
        }
        (EntityKind::Dimension(x), EntityKind::Dimension(y)) => {
            assert_eq!(da.tables.dim_styles[&x.style].name, db.tables.dim_styles[&y.style].name, "{what}: style");
            assert_eq!(x.text_override, y.text_override, "{what}: override");
            match (x.text_pos, y.text_pos) {
                (Some(p), Some(q)) => close_pt(p, q, &what),
                (None, None) => {}
                other => panic!("{what}: text_pos {other:?}"),
            }
            match (&x.kind, &y.kind) {
                (
                    DimKind::Linear { p1, p2, line_point, rotation },
                    DimKind::Linear { p1: q1, p2: q2, line_point: l2, rotation: r2 },
                ) => {
                    close_pt(*p1, *q1, &what);
                    close_pt(*p2, *q2, &what);
                    close_pt(*line_point, *l2, &what);
                    close_angle(*rotation, *r2, &what);
                }
                (DimKind::Aligned { p1, p2, line_point }, DimKind::Aligned { p1: q1, p2: q2, line_point: l2 }) => {
                    close_pt(*p1, *q1, &what);
                    close_pt(*p2, *q2, &what);
                    close_pt(*line_point, *l2, &what);
                }
                (DimKind::Radius { center, point }, DimKind::Radius { center: c2, point: p2 })
                | (DimKind::Diameter { center, point }, DimKind::Diameter { center: c2, point: p2 }) => {
                    close_pt(*center, *c2, &what);
                    close_pt(*point, *p2, &what);
                }
                (
                    DimKind::Angular { vertex, p1, p2, arc_point },
                    DimKind::Angular { vertex: v2, p1: q1, p2: q2, arc_point: a2 },
                ) => {
                    close_pt(*vertex, *v2, &what);
                    close_pt(*p1, *q1, &what);
                    close_pt(*p2, *q2, &what);
                    close_pt(*arc_point, *a2, &what);
                }
                (
                    DimKind::Ordinate { origin, point, leader_end, x_axis },
                    DimKind::Ordinate { origin: o2, point: p2, leader_end: l2, x_axis: x2 },
                ) => {
                    close_pt(*origin, *o2, &what);
                    close_pt(*point, *p2, &what);
                    close_pt(*leader_end, *l2, &what);
                    assert_eq!(x_axis, x2, "{what}: ordinate axis");
                }
                (k1, k2) => panic!("{what}: dimension kind changed {k1:?} vs {k2:?}"),
            }
        }
        (EntityKind::Hatch(x), EntityKind::Hatch(y)) => {
            assert_eq!(x.pattern.name.to_ascii_uppercase(), y.pattern.name.to_ascii_uppercase(), "{what}: pattern");
            if !x.is_solid() {
                close_angle(x.pattern.angle, y.pattern.angle, &what);
                close(x.pattern.scale, y.pattern.scale, &what);
            }
            assert_eq!(x.loops.len(), y.loops.len(), "{what}: loops");
            for (la, lb) in x.loops.iter().zip(&y.loops) {
                assert_eq!(la.curves.len(), lb.curves.len(), "{what}: loop curves {:?} vs {:?}", la.curves, lb.curves);
                for (ca, cb) in la.curves.iter().zip(&lb.curves) {
                    close_curve(ca, cb, &what);
                }
            }
        }
        (EntityKind::Insert(x), EntityKind::Insert(y)) => {
            assert_eq!(da.blocks[&x.block].name, db.blocks[&y.block].name, "{what}: block");
            close_pt(x.pos, y.pos, &what);
            close_pt(x.scale, y.scale, &what);
            close_angle(x.rotation, y.rotation, &what);
        }
        (ka, kb) => match (ka.as_curve(), kb.as_curve()) {
            (Some(ca), Some(cb)) => close_curve(&ca, &cb, &what),
            _ => panic!("{what}: kind changed {ka:?} vs {kb:?}"),
        },
    }
}

fn compare_docs(a: &Document, b: &Document, label: &str) {
    let (da, db) = (&a.drawing, &b.drawing);
    assert_eq!(a.meta.units, b.meta.units, "{label}: units");
    close(da.tables.settings.ltscale, db.tables.settings.ltscale, "ltscale");

    // Tables.
    for l in da.tables.layers.values() {
        let id = db.layer_by_name(&l.name).unwrap_or_else(|| panic!("{label}: layer {} missing", l.name));
        let m = &db.tables.layers[&id];
        assert_color(l.color, m.color, &format!("{label}: layer {} color", l.name));
        assert_eq!(l.lineweight, m.lineweight, "{label}: layer {} lineweight", l.name);
        assert_eq!(
            (l.visible, l.frozen, l.locked, l.plot),
            (m.visible, m.frozen, m.locked, m.plot),
            "{label}: layer {} flags",
            l.name
        );
        assert_eq!(
            da.tables.linetypes[&l.linetype].name.to_ascii_uppercase(),
            db.tables.linetypes[&m.linetype].name.to_ascii_uppercase(),
            "{label}: layer {} linetype",
            l.name
        );
    }
    let lt = db.linetype_by_name("MYDASH").expect("MYDASH linetype");
    let pat = &db.tables.linetypes[&lt].pattern;
    assert_eq!(pat.len(), 4, "{label}: MYDASH pattern {pat:?}");
    for (p, q) in pat.iter().zip([5.0, -2.5, 0.0, -2.5]) {
        close(*p, q, "MYDASH");
    }
    let cjk = db.tables.text_styles.values().find(|s| s.name == "CJK").expect("CJK style");
    assert_eq!(cjk.font, "default");
    close(cjk.width_factor, 0.8, "style width");
    close_angle(cjk.oblique, 15f64.to_radians(), &format!("{label}: style oblique"));
    let iso = db.tables.dim_styles.values().find(|s| s.name == "ISO").expect("ISO dim style");
    close(iso.text_height, 3.5, "dimtxt");
    close(iso.arrow_size, 3.0, "dimasz");
    close(iso.ext_offset, 1.0, "dimexo");
    close(iso.ext_extend, 2.0, "dimexe");
    close(iso.text_gap, 0.8, "dimgap");
    assert_eq!((iso.decimals, iso.angle_decimals), (1, 1), "{label}: dim decimals");
    assert_eq!(iso.suffix, " mm", "{label}: dimpost");

    // Blocks: base points are folded into the geometry on export.
    let ba = da.blocks.values().find(|b| b.name == "BOLT").expect("BOLT");
    let bb = db.blocks.values().find(|b| b.name == "BOLT").expect("BOLT back");
    assert_eq!(ba.entities.len(), bb.entities.len(), "{label}: block entities");
    for (ea, eb) in ba.entities.values().zip(bb.entities.values()) {
        let mut ea = ea.clone();
        let shift = bb.base - ba.base;
        match &mut ea.kind {
            EntityKind::Circle(c) => c.c += shift,
            EntityKind::Line(l) => {
                l.a += shift;
                l.b += shift;
            }
            _ => {}
        }
        compare_entity(da, &ea, db, eb, &format!("{label} block"));
    }

    // Model space, in order.
    let ea: Vec<&Entity> = da.entities.values().collect();
    let eb: Vec<&Entity> = db.entities.values().collect();
    let names = |v: &[&Entity]| v.iter().map(|e| e.kind.type_name()).collect::<Vec<_>>();
    assert_eq!(ea.len(), eb.len(), "{label}: entity count {:?} vs {:?}", names(&ea), names(&eb));
    for (i, (x, y)) in ea.iter().zip(&eb).enumerate() {
        compare_entity(da, x, db, y, &format!("{label} #{i}"));
    }
}

#[test]
fn dxf_round_trip_all_versions() {
    let s = common::sample();
    for v in DxfVersion::ALL {
        let bytes = dxf::export(&s.doc, v).unwrap_or_else(|e| panic!("export {v:?}: {e}"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(v.acad_code()), "{v:?}: $ACADVER");
        let back = dxf::import(&bytes).unwrap_or_else(|e| panic!("import {v:?}: {e}"));
        println!("DXF {v:?}: {} bytes, warnings {:?}", bytes.len(), back.warnings);
        compare_docs(&s.doc, &back.document, &format!("DXF {v:?}"));
    }
}

#[test]
fn dwg_round_trip() {
    let s = common::sample();
    for v in [DxfVersion::R2000, DxfVersion::R2004, DxfVersion::R2010, DxfVersion::R2013, DxfVersion::R2018] {
        let bytes = dwg::export(&s.doc, v).unwrap_or_else(|e| panic!("export {v:?}: {e}"));
        assert_eq!(&bytes[..6], v.acad_code().as_bytes(), "{v:?}: signature");
        let back = dwg::import(&bytes).unwrap_or_else(|e| panic!("import {v:?}: {e}"));
        println!("DWG {v:?}: {} bytes, warnings {:?}", bytes.len(), back.warnings);
        compare_docs(&s.doc, &back.document, &format!("DWG {v:?}"));
    }
}

#[test]
fn dwg_r2007_round_trip() {
    let s = common::sample();
    match dwg::export(&s.doc, DxfVersion::R2007) {
        Ok(bytes) => {
            let back = dwg::import(&bytes).expect("import R2007");
            compare_docs(&s.doc, &back.document, "DWG R2007");
        }
        Err(e) => println!("R2007 DWG export not available: {e}"),
    }
}

#[test]
fn units_round_trip() {
    let mut s = common::sample();
    for u in [Units::Inch, Units::Meter, Units::Unitless, Units::Foot, Units::Centimeter] {
        s.doc.meta.units = u;
        let back = dxf::import(&dxf::export(&s.doc, DxfVersion::R2018).expect("dxf")).expect("import");
        assert_eq!(back.document.meta.units, u);
        let back = dwg::import(&dwg::export(&s.doc, DxfVersion::R2004).expect("dwg")).expect("import");
        assert_eq!(back.document.meta.units, u);
    }
}

#[test]
fn garbage_is_an_error_not_a_panic() {
    assert!(dxf::import(b"").is_err());
    assert!(dwg::import(b"").is_err());
    assert!(dwg::import(b"hello world, not a dwg").is_err());
    assert!(dwg::import(b"AC1032\0\0\0\0garbage garbage garbage").is_err());
    let _ = dxf::import(b"0\nSECTION\n2\nENTITIES\n0\nLINE\n10\nabc\n");
    let _ = dxf::import(&[0xff, 0xfe, 0x00, 0x01, 0x02]);
    // Truncated valid file.
    let s = common::sample();
    let bytes = dxf::export(&s.doc, DxfVersion::R2018).expect("dxf");
    let _ = dxf::import(&bytes[..bytes.len() / 2]);
    let dwgb = dwg::export(&s.doc, DxfVersion::R2018).expect("dwg");
    let _ = dwg::import(&dwgb[..dwgb.len() / 2]);
}

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).expect("test data")
}

fn kinds(doc: &Document) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = doc.drawing.entities.values().map(|e| e.kind.type_name()).collect();
    v.sort();
    v
}

#[test]
fn import_sample_files() {
    for (name, min, must) in [
        (
            "sample_R2018.dxf",
            14,
            &["LINE", "CIRCLE", "ARC", "ELLIPSE", "LWPOLYLINE", "SPLINE", "TEXT", "MTEXT", "DIMENSION", "INSERT"][..],
        ),
        ("sample_R2000.dxf", 14, &["LINE", "CIRCLE", "ARC", "ELLIPSE", "SPLINE", "MTEXT", "DIMENSION"][..]),
        ("sample_R12.dxf", 5, &["LINE", "CIRCLE", "ARC", "TEXT"][..]),
    ] {
        let r = dxf::import(&data(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let k = kinds(&r.document);
        println!("{name}: {k:?}\n  warnings: {:?}", r.warnings);
        assert!(k.len() >= min, "{name}: only {} entities: {k:?}", k.len());
        for m in must {
            assert!(k.contains(m), "{name}: missing {m} in {k:?}");
        }
        // MTEXT with CJK survives.
        if name != "sample_R12.dxf" {
            assert!(
                r.document
                    .drawing
                    .entities
                    .values()
                    .any(|e| matches!(&e.kind, EntityKind::MText(m) if m.text.contains('中')))
            );
        }
    }
    for name in ["acad_AC1015.dwg", "acad_AC1018.dwg", "acad_AC1032.dwg"] {
        assert!(wcad_io::is_dwg(&data(name)));
        let r = wcad_io::import_auto(&data(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let k = kinds(&r.document);
        println!("{name}: {k:?}\n  warnings: {:?}", r.warnings);
        for m in [
            "LINE",
            "CIRCLE",
            "ARC",
            "ELLIPSE",
            "LWPOLYLINE",
            "SPLINE",
            "TEXT",
            "MTEXT",
            "DIMENSION",
            "HATCH",
            "INSERT",
        ] {
            assert!(k.contains(&m), "{name}: missing {m} in {k:?}");
        }
        assert_eq!(k.iter().filter(|n| **n == "DIMENSION").count(), 5, "{name}: dimensions");
        let hatch = r.document.drawing.entities.values().find_map(|e| match &e.kind {
            EntityKind::Hatch(h) => Some(h.clone()),
            _ => None,
        });
        let hatch = hatch.expect("hatch");
        assert_eq!(hatch.loops.len(), 3, "{name}: hatch loops");
        assert!(hatch.is_solid());
        assert!(matches!(hatch.loops[1].curves[0], Curve2::Circle(_)), "{name}: full-circle arc edge → circle");
        let layer = r.document.drawing.layer_by_name("GEOM").expect("GEOM layer");
        assert_eq!(r.document.drawing.tables.layers[&layer].color, Color::Aci(1));
        assert!(r.document.drawing.block_by_name("BOLT").is_some());
    }
}
