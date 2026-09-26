//! DXF import/export (ASCII and binary DXF on import, ASCII on export).

use std::collections::HashMap;
use std::io::Cursor;

use acadrust::io::dxf::{DxfReader, DxfReaderConfiguration, DxfWriter};
use wcad_doc::Document;
use wcad_math::DVec2;

use crate::acad::{Flavor, export, import};
use crate::{DxfVersion, Error, ImportReport, Result, guarded};

/// Maximum number of reader diagnostics copied into the warnings.
const MAX_DIAGNOSTICS: usize = 20;

/// Parse a DXF file into a fresh document. Unsupported entities become warnings.
pub fn import(bytes: &[u8]) -> Result<ImportReport> {
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Err(Error::Empty);
    }
    let read = |failsafe: bool| {
        guarded("DXF", || {
            let config = DxfReaderConfiguration {
                failsafe,
                default_encoding: None,
            };
            DxfReader::from_reader(Cursor::new(bytes.to_vec()))
                .map_err(|e| Error::Dxf(e.to_string()))?
                .with_configuration(config)
                .read_with_stats()
                .map_err(|e| Error::Dxf(e.to_string()))
        })
    };
    // Strict first; damaged files get a second, failsafe pass.
    let (outcome, recovered) = match read(false) {
        Ok(o) => (o, false),
        Err(strict_err) => match read(true) {
            Ok(o) if o.document.entity_count() > 0 => (o, true),
            _ => return Err(strict_err),
        },
    };
    if outcome.document.entity_count() == 0 && outcome.stats.decoded_source_records == 0 {
        return Err(Error::Dxf("no DXF records found".into()));
    }
    let extras = import::Extras {
        block_bases: block_base_points(bytes),
    };
    let mut report = import::to_document(&outcome.document, Flavor::Dxf, &extras);
    if recovered {
        report.warnings.insert(
            0,
            "the file is damaged; only the readable part was imported".into(),
        );
    }
    let diags = &outcome.stats.diagnostics;
    for d in diags.iter().take(MAX_DIAGNOSTICS) {
        report.warnings.push(format!("DXF reader: {}", d.message));
    }
    if diags.len() > MAX_DIAGNOSTICS {
        report.warnings.push(format!(
            "DXF reader: {} more problems",
            diags.len() - MAX_DIAGNOSTICS
        ));
    }
    Ok(report)
}

/// Write the document's drawing as an ASCII DXF of the given release.
pub fn export(doc: &Document, version: DxfVersion) -> Result<Vec<u8>> {
    let cad = export::from_document(doc, version, Flavor::Dxf)?;
    guarded("DXF writer", || {
        DxfWriter::new(&cad)
            .write_to_vec()
            .map_err(|e| Error::Write {
                format: "DXF",
                message: e.to_string(),
            })
    })
}

/// Block base points from the BLOCKS section of an ASCII DXF, keyed by upper-case block name.
/// (acadrust ignores the BLOCK entity's base point.) Binary DXF is not scanned.
pub(crate) fn block_base_points(bytes: &[u8]) -> HashMap<String, DVec2> {
    let mut out = HashMap::new();
    if bytes.starts_with(b"AutoCAD Binary DXF") {
        return out;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut lines = text.lines().map(str::trim);
    let mut in_blocks = false;
    let mut pending_section = false;
    // Current BLOCK entity being parsed.
    let mut in_block_entity = false;
    let mut name = String::new();
    let mut base = DVec2::ZERO;
    let mut flush = |in_block: &mut bool, name: &mut String, base: &mut DVec2| {
        if *in_block && !name.is_empty() && *base != DVec2::ZERO {
            out.insert(name.to_ascii_uppercase(), *base);
        }
        *in_block = false;
        name.clear();
        *base = DVec2::ZERO;
    };
    while let (Some(code), Some(value)) = (lines.next(), lines.next()) {
        let Ok(code) = code.parse::<i32>() else { break };
        if code == 0 {
            flush(&mut in_block_entity, &mut name, &mut base);
            match value {
                "SECTION" => pending_section = true,
                "ENDSEC" => {
                    if in_blocks {
                        break;
                    }
                }
                "BLOCK" if in_blocks => in_block_entity = true,
                _ => {}
            }
            continue;
        }
        if pending_section && code == 2 {
            in_blocks = value.eq_ignore_ascii_case("BLOCKS");
            pending_section = false;
            continue;
        }
        if in_block_entity {
            match code {
                2 => name = value.to_string(),
                10 => base.x = value.parse().unwrap_or(0.0),
                20 => base.y = value.parse().unwrap_or(0.0),
                _ => {}
            }
        }
    }
    flush(&mut in_block_entity, &mut name, &mut base);
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn scans_block_bases() {
        let dxf = "0\nSECTION\n2\nBLOCKS\n0\nBLOCK\n8\n0\n2\nBOLT\n70\n0\n10\n5.5\n20\n-2\n30\n0\n0\nENDBLK\n0\nENDSEC\n0\nEOF\n";
        let m = super::block_base_points(dxf.as_bytes());
        assert_eq!(
            m.get("BOLT").copied(),
            Some(wcad_math::DVec2::new(5.5, -2.0))
        );
    }
}
