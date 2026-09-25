//! File formats by extension: native `.wcad`/`.wcadz` (wcad-doc), DXF/DWG import/export and
//! SVG/PDF export (wcad-io), plus autosave encoding. Pure functions over bytes (testable).

use wcad_doc::Document;

use crate::editor::ExportFormat;
use crate::platform::{self, AUTOSAVE_MAX_BYTES};

pub const AUTOSAVE_KEY: &str = "autosave";
pub const AUTOSAVE_META_KEY: &str = "autosave_meta";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Wcad,
    Wcadz,
    Dxf,
    Dwg,
    Unknown,
}

/// File kind from the name's extension (case-insensitive).
pub fn kind_of(name: &str) -> FileKind {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wcad" | "json" => FileKind::Wcad,
        "wcadz" => FileKind::Wcadz,
        "dxf" => FileKind::Dxf,
        "dwg" => FileKind::Dwg,
        _ => FileKind::Unknown,
    }
}

/// Name without directory and extension.
pub fn stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(base)
        .to_owned()
}

/// Open-dialog filters.
pub fn open_filters() -> Vec<(&'static str, &'static [&'static str])> {
    vec![
        ("webcad / DXF / DWG", &["wcad", "wcadz", "dxf", "dwg"][..]),
        ("webcad", &["wcad", "wcadz"][..]),
        ("DXF", &["dxf"][..]),
        ("DWG", &["dwg"][..]),
    ]
}

pub struct Loaded {
    pub doc: Document,
    pub warnings: Vec<String>,
    /// `true` for native documents (saving keeps the name); imports get a new native name.
    pub native: bool,
}

/// Load any supported file from bytes. Errors are human-readable.
pub fn load_bytes(name: &str, bytes: &[u8]) -> Result<Loaded, String> {
    let sniff_dwg =
        bytes.len() >= 6 && &bytes[..2] == b"AC" && bytes[2..6].iter().all(u8::is_ascii_digit);
    let kind = match kind_of(name) {
        FileKind::Unknown if sniff_dwg => FileKind::Dwg,
        k => k,
    };
    match kind {
        FileKind::Wcad | FileKind::Wcadz => {
            let doc = wcad_doc::file::load(bytes).map_err(|e| e.to_string())?;
            Ok(Loaded {
                doc,
                warnings: Vec::new(),
                native: true,
            })
        }
        FileKind::Dxf => {
            let r = wcad_io::dxf::import(bytes).map_err(|e| e.to_string())?;
            Ok(Loaded {
                doc: r.document,
                warnings: r.warnings,
                native: false,
            })
        }
        FileKind::Dwg => {
            let r = wcad_io::dwg::import(bytes).map_err(|e| e.to_string())?;
            Ok(Loaded {
                doc: r.document,
                warnings: r.warnings,
                native: false,
            })
        }
        FileKind::Unknown => Err(format!("unsupported file type: {name}")),
    }
}

/// Serialize the document natively (`compressed` = `.wcadz`).
pub fn save_native(doc: &Document, compressed: bool) -> Result<Vec<u8>, String> {
    wcad_doc::file::save(doc, compressed).map_err(|e| e.to_string())
}

/// Export; returns the file extension and bytes.
pub fn export_bytes(
    doc: &Document,
    fmt: ExportFormat,
    dark_background: bool,
) -> Result<(&'static str, Vec<u8>), String> {
    match fmt {
        ExportFormat::Dxf => wcad_io::dxf::export(doc, wcad_io::DxfVersion::R2018)
            .map(|b| ("dxf", b))
            .map_err(|e| e.to_string()),
        ExportFormat::Dwg => wcad_io::dwg::export(doc, wcad_io::DxfVersion::R2018)
            .map(|b| ("dwg", b))
            .map_err(|e| e.to_string()),
        ExportFormat::Svg => {
            let opts = wcad_io::SvgOptions {
                background: Some(if dark_background {
                    [33, 40, 48]
                } else {
                    [255, 255, 255]
                }),
                ..Default::default()
            };
            Ok((
                "svg",
                wcad_io::svg::export(&doc.drawing, &opts).into_bytes(),
            ))
        }
        ExportFormat::Pdf => {
            let setup = wcad_io::PageSetup {
                font: crate::fonts::cad_font().cloned(),
                ..Default::default()
            };
            wcad_io::pdf::export(&doc.drawing, &setup)
                .map(|b| ("pdf", b))
                .map_err(|e| e.to_string())
        }
    }
}

/// Autosave payload: base64 of the compressed document, `None` when over the size cap.
pub fn autosave_payload(doc: &Document) -> Option<String> {
    let bytes = save_native(doc, true).ok()?;
    if bytes.len() > AUTOSAVE_MAX_BYTES {
        log::warn!("autosave skipped: {} bytes exceeds the cap", bytes.len());
        return None;
    }
    Some(platform::base64_encode(&bytes))
}

/// Decode an autosave payload.
pub fn recover(payload: &str) -> Option<Document> {
    let bytes = platform::base64_decode(payload)?;
    wcad_doc::file::load(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wcad_doc::EntityKind;
    use wcad_geom2d::Line2;
    use wcad_math::DVec2;

    fn doc() -> Document {
        let mut d = Document::new();
        d.transact("l", |tx| {
            tx.add(EntityKind::Line(Line2::new(
                DVec2::ZERO,
                DVec2::new(3.0, 4.0),
            )))
        });
        d
    }

    #[test]
    fn kinds_and_names() {
        assert_eq!(kind_of("a.DXF"), FileKind::Dxf);
        assert_eq!(kind_of("x/y/plan.wcadz"), FileKind::Wcadz);
        assert_eq!(kind_of("noext"), FileKind::Unknown);
        assert_eq!(stem("C:\\d\\plan.v2.dxf"), "plan.v2");
        assert_eq!(stem("plan"), "plan");
    }

    #[test]
    fn native_and_dxf_round_trip() {
        let d = doc();
        for (name, bytes) in [
            ("a.wcad", save_native(&d, false).unwrap()),
            ("a.wcadz", save_native(&d, true).unwrap()),
        ] {
            let l = load_bytes(name, &bytes).unwrap();
            assert!(l.native);
            assert_eq!(l.doc.drawing.entities.len(), 1);
        }
        let (ext, dxf) = export_bytes(&d, ExportFormat::Dxf, true).unwrap();
        assert_eq!(ext, "dxf");
        let l = load_bytes("b.dxf", &dxf).unwrap();
        assert!(!l.native);
        assert_eq!(l.doc.drawing.entities.len(), 1);
        let (_, svg) = export_bytes(&d, ExportFormat::Svg, false).unwrap();
        assert!(String::from_utf8_lossy(&svg).contains("<svg"));
        let (_, pdf) = export_bytes(&d, ExportFormat::Pdf, false).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert!(load_bytes("c.txt", b"hello").is_err());
        assert!(load_bytes("c.dxf", b"garbage").is_err());
        assert!(load_bytes("c.wcad", b"{not json").is_err());
    }

    #[test]
    fn autosave_round_trip() {
        let d = doc();
        let p = autosave_payload(&d).unwrap();
        let r = recover(&p).unwrap();
        assert_eq!(r.drawing, d.drawing);
        assert!(recover("garbage!").is_none());
    }
}
