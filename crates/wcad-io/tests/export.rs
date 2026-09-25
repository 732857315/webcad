//! SVG and PDF export smoke tests (well-formedness, page size, content).

mod common;

use std::sync::Arc;

use wcad_geom2d::text::Font;
use wcad_io::{DxfVersion, PageSetup, Paper, PlotScale, SvgOptions, dxf, pdf, svg};

/// Minimal XML well-formedness check: balanced tags, quoted attributes, no stray `<`/`&`.
fn check_xml(s: &str) {
    let mut stack: Vec<String> = Vec::new();
    let mut i = 0;
    let b = s.as_bytes();
    while i < b.len() {
        match b[i] {
            b'<' => {
                let end = s[i..].find('>').map(|e| i + e).expect("unterminated tag");
                let tag = &s[i + 1..end];
                assert!(!tag.contains('<'), "nested '<' in tag: {tag}");
                if tag.starts_with('?') {
                    assert!(tag.ends_with('?'), "bad processing instruction");
                } else if let Some(name) = tag.strip_prefix('/') {
                    let open = stack.pop().unwrap_or_else(|| panic!("unexpected </{name}>"));
                    assert_eq!(open, name.trim(), "mismatched close tag");
                } else {
                    let name: String = tag.chars().take_while(|c| !c.is_whitespace() && *c != '/').collect();
                    assert_eq!(tag.matches('"').count() % 2, 0, "unbalanced quotes in <{tag}>");
                    if !tag.ends_with('/') {
                        stack.push(name);
                    }
                }
                i = end + 1;
            }
            b'&' => {
                let end = s[i..].find(';').map(|e| i + e).expect("unterminated entity");
                let ent = &s[i + 1..end];
                assert!(matches!(ent, "amp" | "lt" | "gt" | "quot" | "apos"), "unknown entity &{ent};");
                i = end + 1;
            }
            _ => i += 1,
        }
    }
    assert!(stack.is_empty(), "unclosed tags: {stack:?}");
}

fn dump(name: &str, bytes: &[u8]) {
    if let Ok(dir) = std::env::var("WCAD_IO_DUMP") {
        let _ = std::fs::write(format!("{dir}/{name}"), bytes);
    }
}

#[test]
fn svg_is_well_formed_and_complete() {
    let s = common::sample();
    let out = svg::export(&s.doc.drawing, &SvgOptions::default());
    dump("sample.svg", out.as_bytes());
    assert!(out.starts_with("<?xml"));
    assert!(out.trim_end().ends_with("</svg>"));
    check_xml(&out);
    assert!(out.contains("viewBox=\""));
    let path_has = |c: char| out.split("d=\"").skip(1).any(|d| d.split('"').next().is_some_and(|p| p.contains(c)));
    assert!(path_has('A'), "native arcs");
    assert!(!path_has('C'), "the only cubic spline is on a frozen layer");
    assert!(out.contains("fill-rule=\"evenodd\""), "solid hatch");
    assert!(out.contains("stroke-dasharray"), "linetypes");
    assert!(out.contains("中文"), "CJK text");
    assert!(out.contains("Ø10"), "%%c code");
    assert!(out.contains("Noto Sans SC"));
    assert!(out.contains("L=20.0 mm"), "dimension override text");
    assert!(out.contains("45.0°<"), "angular dimension without linear suffix");
    assert!(out.contains("#123456"), "true color");
    // Frozen and off layers are hidden: the frozen spline must not be drawn.
    let paths = out.matches("<path").count();
    assert!(paths > 20, "{paths} paths");

    // Thaw everything: the cubic spline appears as exact Bézier segments.
    let mut thawed = common::sample().doc;
    thawed.transact("thaw", |tx| {
        for l in tx.tables_mut().layers.values_mut() {
            l.frozen = false;
            l.visible = true;
        }
    });
    let all = svg::export(&thawed.drawing, &SvgOptions::default());
    check_xml(&all);
    assert!(all.split("d=\"").skip(1).any(|d| d.split('"').next().is_some_and(|p| p.matches('C').count() == 3)));
    assert!(all.matches("<path").count() > paths);

    let dark = svg::export(
        &s.doc.drawing,
        &SvgOptions { background: Some([33, 40, 48]), monochrome: true, ..Default::default() },
    );
    check_xml(&dark);
    assert!(dark.contains("fill=\"#212830\""), "background rect");
    assert!(dark.contains("stroke=\"#ffffff\""), "monochrome white on dark");
    assert!(!dark.contains("#123456"));
}

#[test]
fn svg_of_empty_and_imported_drawings() {
    let empty = wcad_doc::Document::new();
    let out = svg::export(&empty.drawing, &SvgOptions::default());
    check_xml(&out);
    let bytes = std::fs::read(format!("{}/tests/data/sample_R2018.dxf", env!("CARGO_MANIFEST_DIR"))).expect("sample");
    let doc = dxf::import(&bytes).expect("import").document;
    let out = svg::export(&doc.drawing, &SvgOptions::default());
    dump("sample_R2018.svg", out.as_bytes());
    check_xml(&out);
    assert!(out.contains("中文"));
}

fn media_box(pdf: &[u8]) -> [f64; 4] {
    let s = String::from_utf8_lossy(pdf);
    let at = s.find("/MediaBox [").expect("MediaBox") + "/MediaBox [".len();
    let end = s[at..].find(']').expect("]") + at;
    let v: Vec<f64> = s[at..end].split_whitespace().map(|x| x.parse().expect("number")).collect();
    [v[0], v[1], v[2], v[3]]
}

#[test]
fn pdf_page_sizes() {
    let s = common::sample();
    for (paper, landscape, w, h) in [
        (Paper::A4, true, 841.89, 595.28),
        (Paper::A4, false, 595.28, 841.89),
        (Paper::A3, true, 1190.55, 841.89),
        (Paper::A0, false, 2383.94, 3370.39),
        (Paper::Letter, false, 612.0, 792.0),
        (Paper::Custom { width_mm: 100.0, height_mm: 50.0 }, true, 283.46, 141.73),
    ] {
        let setup = PageSetup { paper, landscape, ..Default::default() };
        let bytes = pdf::export(&s.doc.drawing, &setup).expect("pdf");
        assert!(bytes.starts_with(b"%PDF-"), "header");
        assert!(bytes.ends_with(b"%%EOF\n") || bytes.ends_with(b"%%EOF"), "trailer");
        let mb = media_box(&bytes);
        assert!((mb[2] - w).abs() < 0.05 && (mb[3] - h).abs() < 0.05, "{paper:?} {landscape}: {mb:?}");
    }
}

#[test]
fn pdf_options_and_errors() {
    let s = common::sample();
    let d = &s.doc.drawing;
    let base = PageSetup::default();
    let a = pdf::export(d, &base).expect("fit");
    dump("sample.pdf", &a);
    let b = pdf::export(
        d,
        &PageSetup { scale: PlotScale::one_to(2.0), monochrome: true, line_weights: false, ..base.clone() },
    )
    .expect("1:2");
    assert!(b.starts_with(b"%PDF-"));
    // Invalid setups are errors, not panics.
    assert!(pdf::export(d, &PageSetup { margin_mm: 200.0, ..base.clone() }).is_err());
    assert!(
        pdf::export(d, &PageSetup { scale: PlotScale::Ratio { paper_mm: 0.0, drawing_units: 1.0 }, ..base.clone() })
            .is_err()
    );
    assert!(
        pdf::export(d, &PageSetup { paper: Paper::Custom { width_mm: f64::NAN, height_mm: 10.0 }, ..base.clone() })
            .is_err()
    );
    // Empty drawing still gives a valid page.
    let empty = wcad_doc::Document::new();
    assert!(pdf::export(&empty.drawing, &base).expect("empty").starts_with(b"%PDF-"));
}

#[test]
fn pdf_with_embedded_font() {
    let path = format!("{}/../../assets/fonts/NotoSansSC-Regular-ui.ttf", env!("CARGO_MANIFEST_DIR"));
    let Ok(bytes) = std::fs::read(&path) else {
        println!("font not found, skipping");
        return;
    };
    let font = Font::from_bytes(Arc::from(bytes), 0).expect("font parses");
    let s = common::sample();
    let setup = PageSetup { font: Some(font), paper: Paper::A3, ..Default::default() };
    let out = pdf::export(&s.doc.drawing, &setup).expect("pdf");
    dump("sample_font.pdf", &out);
    assert!(out.starts_with(b"%PDF-"));
}

#[test]
fn dxf_output_uses_autocad_units() {
    // Spot-check raw group codes where acadrust's conventions differ between paths.
    let s = common::sample();
    let bytes = dxf::export(&s.doc, DxfVersion::R2018).expect("dxf");
    dump("sample_R2018.dxf", &bytes);
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    // TEXT oblique (51) must be written in degrees: 12°.
    let mut found = false;
    for w in lines.windows(2) {
        if w[0] == "51" && w[1].parse::<f64>().is_ok_and(|v| (v - 12.0).abs() < 1e-6) {
            found = true;
        }
    }
    assert!(found, "TEXT oblique 12 degrees");
    // Block BOLT is written with its entities relative to the origin (base folded in).
    assert!(text.contains("BOLT"));
    // Dimensions reference generated anonymous geometry blocks.
    assert!(lines.windows(2).any(|w| w[0] == "2" && w[1] == "*D1"), "DIMENSION → *D1 block");
}

#[test]
fn pre_2007_text_uses_unicode_escapes() {
    let s = common::sample();
    let old = dxf::export(&s.doc, DxfVersion::R2000).expect("R2000");
    assert!(old.is_ascii(), "R2000 DXF is pure ASCII (non-ASCII text escaped)");
    let text = String::from_utf8_lossy(&old);
    assert!(text.contains(r"\U+4E2D\U+6587"), "CJK escaped");
    let new = dxf::export(&s.doc, DxfVersion::R2018).expect("R2018");
    assert!(String::from_utf8_lossy(&new).contains("中文"), "R2018 keeps UTF-8");
    let back = dxf::import(&old).expect("import");
    assert!(back.document.drawing.layer_by_name("不打印").is_some(), "CJK layer name decoded");
}
