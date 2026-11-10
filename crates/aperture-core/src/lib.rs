//! Domain model and status rules for Aperture.
//!
//! The rule this crate exists to enforce: a requirement is only shown as done
//! when the daemon's own verification runs passed against the current snapshot
//! of the workspace. Agent self-reports describe activity, never completion.

pub mod manifest;
pub mod model;
pub mod snapshot;
pub mod status;
pub mod store;

pub use model::*;
