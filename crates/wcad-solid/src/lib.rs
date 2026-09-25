//! 3D kernel facade: exact B-rep via monstertruck, mesh fallback via boolmesh, and regeneration of a
//! `wcad_doc::Part` into bodies. See `docs/ARCHITECTURE.md` §5.5.
//!
//! - [`regenerate`] evaluates the feature history into [`Body`]s with per-feature status, reusing a
//!   [`RegenCache`] so editing feature N only recomputes N..end.
//! - Bodies are exact B-rep ([`BodyRep::Exact`]) or mesh-only ([`BodyRep::Mesh`]) after a boolean fell
//!   back to the mesh kernel. Every face and edge carries a persistent [`TopoName`].
//! - [`tessellate`] produces render/pick meshes, [`to_stl`]/[`to_obj`]/[`to_step`] export, and
//!   [`mass_properties`] measures bodies.
//!
//! Kernel calls are wrapped in `catch_unwind` on native targets; on wasm (panic = abort) inputs are
//! validated before they reach the kernel.

mod analysis;
mod body;
mod conv;
mod export;
mod fallback;
mod kernel;
mod profile;
mod regen;
mod tess;

pub use body::{Body, BodyRep, EdgeInfo, ExactSolid, FaceInfo, MeshSolid, SurfaceKind};
pub use export::{MassProperties, mass_properties, to_obj, to_step, to_stl};
pub use kernel::{BoolOutcome, Kernel, MtKernel, boolean_bodies};
pub use profile::{ProfileEdgeTag, ProfileFace, build_profile_face, simple_regions};
pub use regen::{FeatureStatus, RegenCache, RegenResult, RegenStats, regenerate};
pub use tess::{TriMesh, auto_tolerance, tessellate};

pub use wcad_doc::{BodyRef, FeatureId, TopoName, TopoTag};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("kernel error: {0}")]
    Kernel(String),
    #[error("kernel panicked in {0}")]
    Panic(String),
    #[error("mesh boolean failed: {0}")]
    Mesh(String),
    #[error("not supported: {0}")]
    Unsupported(String),
    #[error("unresolved reference: {0}")]
    Unresolved(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Run a kernel call, turning panics into [`Error::Panic`] on targets that unwind.
///
/// On wasm32 the web build uses `panic = "abort"`, so callers must validate inputs first.
pub(crate) fn guard<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                log::warn!("kernel panic in {what}: {msg}");
                Err(Error::Panic(format!("{what}: {msg}")))
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = what;
        f()
    }
}

#[cfg(test)]
mod tests;
