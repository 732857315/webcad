//! Native file format: JSON `{ "format": "webcad", "version": 1, ... }`, optionally zlib-compressed
//! (`.wcadz`). [`load`] detects compression automatically.

use serde::{Deserialize, Serialize};

use crate::document::{DocMeta, Document};
use crate::drawing::Drawing;
use crate::ids::IdAllocator;
use crate::part::Part;
use crate::{Error, Result};

pub const FORMAT: &str = "webcad";
pub const VERSION: u32 = 1;
pub const EXTENSION: &str = "wcad";
pub const EXTENSION_COMPRESSED: &str = "wcadz";

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'a str,
    version: u32,
    meta: &'a DocMeta,
    ids: &'a IdAllocator,
    drawing: &'a Drawing,
    part: &'a Part,
}

#[derive(Deserialize)]
struct FileHeader {
    format: String,
    version: u32,
}

#[derive(Deserialize)]
struct FileIn {
    meta: DocMeta,
    ids: IdAllocator,
    drawing: Drawing,
    #[serde(default)]
    part: Part,
}

pub fn to_json(doc: &Document, pretty: bool) -> Result<String> {
    let out = FileOut {
        format: FORMAT,
        version: VERSION,
        meta: &doc.meta,
        ids: &doc.ids,
        drawing: &doc.drawing,
        part: &doc.part,
    };
    Ok(if pretty { serde_json::to_string_pretty(&out)? } else { serde_json::to_string(&out)? })
}

/// Serialize; `compressed` selects zlib (`.wcadz`).
pub fn save(doc: &Document, compressed: bool) -> Result<Vec<u8>> {
    let json = to_json(doc, !compressed)?;
    Ok(if compressed {
        miniz_oxide::deflate::compress_to_vec_zlib(json.as_bytes(), 6)
    } else {
        json.into_bytes()
    })
}

/// Load from bytes (plain JSON or zlib-compressed JSON).
pub fn load(bytes: &[u8]) -> Result<Document> {
    let first = bytes.iter().copied().find(|b| !b.is_ascii_whitespace());
    let json: std::borrow::Cow<[u8]> = if first == Some(b'{') {
        bytes.into()
    } else {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes, 1 << 30)
            .map_err(|_| Error::Decompress)?
            .into()
    };
    let header: FileHeader = serde_json::from_slice(&json).map_err(|_| Error::NotWebcad)?;
    if header.format != FORMAT {
        return Err(Error::NotWebcad);
    }
    if header.version > VERSION {
        return Err(Error::UnsupportedVersion(header.version));
    }
    let f: FileIn = serde_json::from_slice(&json)?;
    let mut doc = Document::from_parts(f.meta, f.drawing, f.part, f.ids);
    doc.mark_saved();
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityKind;
    use wcad_geom2d::{Arc2, Line2};
    use wcad_math::DVec2;

    #[test]
    fn round_trip_plain_and_compressed() {
        let mut doc = Document::new();
        doc.transact("draw", |tx| {
            tx.add(EntityKind::Line(Line2::new(DVec2::ZERO, DVec2::new(10.0, 5.0))));
            tx.add(EntityKind::Arc(Arc2::new(DVec2::ONE, 3.0, 0.0, 1.5)));
        });
        for compressed in [false, true] {
            let bytes = save(&doc, compressed).unwrap();
            let back = load(&bytes).unwrap();
            assert_eq!(back.drawing, doc.drawing);
            assert_eq!(back.part, doc.part);
            assert!(!back.is_dirty());
        }
        assert!(matches!(load(b"{\"format\":\"other\",\"version\":1}"), Err(Error::NotWebcad)));
    }
}
