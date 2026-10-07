//! Die workdir layout (single source of truth).
//!
//! The Line creates `<root>/<order>-<attempt>/`, the Die wrapper clones the
//! gate repo into `<...>/repo`, the Publisher reads `<...>/repo` for the
//! result HEAD, and T10's GC sweeps the same roots. One builder for all of
//! them so the contract can never drift (M0 defect #4 lesson: two code
//! sites guessing each other's paths is how empty digests happen).
use std::path::PathBuf;

/// Root under which every Die workdir lives.
pub const WORKDIR_ROOT: &str = "/tmp";

/// Workdir for one attempt: `<root>/foundry-die-<order_id>-<attempt>`.
pub fn workdir_path(order_id: &str, attempt: u32) -> PathBuf {
    PathBuf::from(format!("{WORKDIR_ROOT}/foundry-die-{order_id}-{attempt}"))
}

/// The Die-side clone inside a workdir (wrapper contract).
pub fn repo_path(order_id: &str, attempt: u32) -> PathBuf {
    workdir_path(order_id, attempt).join("repo")
}
