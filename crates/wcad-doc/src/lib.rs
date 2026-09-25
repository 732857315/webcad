//! The webcad document model.
//!
//! A [`Document`] holds a 2D [`Drawing`] (drafting), a 3D [`Part`] (feature tree) and metadata.
//! Every mutation goes through [`Document::transact`], which records undo/redo information and a
//! change set the UI uses to update display lists incrementally.

pub mod aci;
pub mod document;
pub mod drawing;
pub mod entity;
pub mod file;
pub mod history;
pub mod ids;
pub mod part;
pub mod style;

pub use document::{ChangeSet, DocMeta, Document, Tx, Units};
pub use drawing::{Block, Drawing, DrawingSettings, Tables};
pub use entity::*;
pub use ids::*;
pub use part::*;
pub use style::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a webcad document")]
    NotWebcad,
    #[error("unsupported document version {0}")]
    UnsupportedVersion(u32),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("decompression failed")]
    Decompress,
    #[error("unknown entity {0:?}")]
    UnknownEntity(EntityId),
    #[error("unknown feature {0:?}")]
    UnknownFeature(FeatureId),
}

pub type Result<T> = std::result::Result<T, Error>;
