//! Import/export. All functions work on in-memory bytes (the web has no filesystem).
//! See `docs/ARCHITECTURE.md` §5.6.
//!
//! - [`dxf`] / [`dwg`]: AutoCAD formats via the `acadrust` crate. Its types never leave this crate;
//!   everything is mapped to and from [`wcad_doc::Document`].
//! - [`svg`]: hand-written SVG (exact arcs, Bézier splines, `<text>` elements).
//! - [`pdf`]: vector PDF via `pdf-writer`, text as glyph outlines from `wcad_geom2d::text`.

// `!(x > 0.0)` is used deliberately: it is also true for NaN coming from malformed files.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

mod acad;
mod dimgeom;
pub mod dwg;
pub mod dxf;
mod geom;
mod mtext;
mod pattern;
pub mod pdf;
mod scene;
pub mod svg;

use wcad_doc::Document;

pub use pdf::{PageSetup, Paper, PlotScale};
pub use svg::SvgOptions;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the file is empty")]
    Empty,
    #[error("not a DXF file or unsupported DXF variant: {0}")]
    Dxf(String),
    #[error("not a DWG file or unsupported DWG version: {0}")]
    Dwg(String),
    #[error("could not write {format}: {message}")]
    Write { format: &'static str, message: String },
    #[error("the {0} reader failed on malformed input")]
    ReaderPanic(&'static str),
    #[error("invalid page setup: {0}")]
    PageSetup(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Result of importing a foreign file: a fresh document plus human-readable warnings about data
/// that could not be represented (unsupported entities, paper space, ...).
#[derive(Debug)]
pub struct ImportReport {
    pub document: Document,
    pub warnings: Vec<String>,
}

/// AutoCAD file format release used for DXF and DWG export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DxfVersion {
    /// AC1015
    R2000,
    /// AC1018
    R2004,
    /// AC1021
    R2007,
    /// AC1024
    R2010,
    /// AC1027
    R2013,
    /// AC1032
    #[default]
    R2018,
}

/// DWG uses the same release numbering as DXF.
pub type DwgVersion = DxfVersion;

impl DxfVersion {
    pub const ALL: [DxfVersion; 6] = [
        DxfVersion::R2000,
        DxfVersion::R2004,
        DxfVersion::R2007,
        DxfVersion::R2010,
        DxfVersion::R2013,
        DxfVersion::R2018,
    ];

    /// The `$ACADVER` code, e.g. `"AC1032"`.
    pub fn acad_code(self) -> &'static str {
        match self {
            DxfVersion::R2000 => "AC1015",
            DxfVersion::R2004 => "AC1018",
            DxfVersion::R2007 => "AC1021",
            DxfVersion::R2010 => "AC1024",
            DxfVersion::R2013 => "AC1027",
            DxfVersion::R2018 => "AC1032",
        }
    }

    /// Human-readable label for menus ("AutoCAD 2018").
    pub fn label(self) -> &'static str {
        match self {
            DxfVersion::R2000 => "AutoCAD 2000",
            DxfVersion::R2004 => "AutoCAD 2004",
            DxfVersion::R2007 => "AutoCAD 2007",
            DxfVersion::R2010 => "AutoCAD 2010",
            DxfVersion::R2013 => "AutoCAD 2013",
            DxfVersion::R2018 => "AutoCAD 2018",
        }
    }
}

/// `true` if `bytes` look like a DWG file (`AC10xx` release signature).
pub fn is_dwg(bytes: &[u8]) -> bool {
    bytes.len() >= 6 && &bytes[..2] == b"AC" && bytes[2..6].iter().all(u8::is_ascii_digit)
}

/// Import DWG or DXF, chosen by content (for dropped files whose extension is unreliable).
pub fn import_auto(bytes: &[u8]) -> Result<ImportReport> {
    if is_dwg(bytes) { dwg::import(bytes) } else { dxf::import(bytes) }
}

/// Run a third-party reader/writer, turning a panic into an error on targets that unwind.
/// (On wasm `panic = "abort"`, so this cannot help there; inputs are validated where possible.)
pub(crate) fn guarded<T>(what: &'static str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(Error::ReaderPanic(what)),
    }
}
