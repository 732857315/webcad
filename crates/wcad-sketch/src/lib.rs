//! Parametric sketches.
//!
//! [`model`] is the persisted sketch (what the document stores). The in-house constraint solver
//! ([`solver`]) builds a parameter vector and scalar equations from a [`Sketch`], solves them per
//! independent cluster with a damped Gauss-Newton / Levenberg-Marquardt iteration (minimum-movement
//! steps, envelope Cholesky), writes the result back into the model and diagnoses degrees of freedom,
//! redundant and conflicting constraints.
//!
//! ```
//! use wcad_math::DVec2;
//! use wcad_sketch::{ConstraintKind, DimValue, Sketch};
//!
//! let mut sk = Sketch::new();
//! let r = sk.add_rectangle(DVec2::new(0.0, 0.0), DVec2::new(9.0, 4.0));
//! sk.add_constraint(ConstraintKind::Fix { p: r.corners[0] });
//! let (a, b) = (r.corners[0], r.corners[1]);
//! sk.add_constraint_checked(ConstraintKind::Distance { p1: a, p2: b, value: DimValue::new(10.0) }).unwrap();
//! assert!(sk.solve().converged);
//! assert!((sk.point(b).unwrap().x - 10.0).abs() < 1e-9);
//! assert_eq!(sk.diagnose().dof, 1); // the height is still free
//! ```

// NaN-aware comparisons (`!(x > 0.0)`) are deliberate: they reject NaN from files/user input.
#![allow(clippy::neg_cmp_op_on_partial_ord)]

pub mod model;
pub mod solver;

pub use model::*;
pub use solver::{DependencyGroup, Diagnosis, EntityStatus, Param, SolveOptions, SolveReport};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown sketch entity {0:?}")]
    UnknownEntity(SkEntityId),
    #[error("unknown constraint {0:?}")]
    UnknownConstraint(SkConstraintId),
    #[error("constraint does not apply to these entities: {0}")]
    BadConstraint(String),
    #[error("degenerate geometry: {0}")]
    Degenerate(String),
}

pub type Result<T> = std::result::Result<T, Error>;
