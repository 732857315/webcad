//! DWG import/export (R2000 … R2018 via acadrust).

use std::io::Cursor;

use acadrust::io::dwg::{DwgReadOptions, DwgReader, DwgWriter};
use wcad_doc::Document;

use crate::acad::{Flavor, export, import};
use crate::{DwgVersion, Error, ImportReport, Result, guarded};

const MAX_DIAGNOSTICS: usize = 20;

/// Parse a DWG file into a fresh document. Unsupported entities become warnings.
pub fn import(bytes: &[u8]) -> Result<ImportReport> {
    if bytes.is_empty() {
        return Err(Error::Empty);
    }
    // Every DWG starts with its release code, e.g. "AC1032".
    if !crate::is_dwg(bytes) {
        return Err(Error::Dwg("missing AC10xx signature".into()));
    }
    let read = |options: DwgReadOptions| {
        guarded("DWG", || {
            DwgReader::from_stream_with_options(Cursor::new(bytes.to_vec()), options)
                .read_with_stats()
                .map_err(|e| Error::Dwg(e.to_string()))
        })
    };
    // Strict first; damaged files get a second, failsafe pass.
    let (outcome, recovered) = match read(DwgReadOptions::default()) {
        Ok(o) => (o, false),
        Err(strict_err) => match read(DwgReadOptions::failsafe()) {
            Ok(o) if o.document.entity_count() > 0 => (o, true),
            _ => return Err(strict_err),
        },
    };
    let mut report = import::to_document(&outcome.document, Flavor::Dwg, &import::Extras::default());
    if recovered {
        report.warnings.insert(0, "the file is damaged; only the readable part was imported".into());
    }
    let diags = &outcome.stats.diagnostics;
    for d in diags.iter().take(MAX_DIAGNOSTICS) {
        report.warnings.push(format!("DWG reader: {}", d.message));
    }
    if diags.len() > MAX_DIAGNOSTICS {
        report.warnings.push(format!("DWG reader: {} more problems", diags.len() - MAX_DIAGNOSTICS));
    }
    Ok(report)
}

/// Write the document's drawing as a DWG of the given release.
pub fn export(doc: &Document, version: DwgVersion) -> Result<Vec<u8>> {
    let cad = export::from_document(doc, version, Flavor::Dwg)?;
    guarded("DWG writer", || {
        DwgWriter::write_to_vec(&cad).map_err(|e| Error::Write { format: "DWG", message: e.to_string() })
    })
}
