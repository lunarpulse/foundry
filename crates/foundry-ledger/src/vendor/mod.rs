//! Vendored maos-audit source (commit 92911f59). DO NOT edit without
//! `// FOUNDRY:` markers + VENDORED.md update.
//!
//! M0 scope (approved 2026-10-01): log_composition + backup + team/region.
//! sealed_export.rs / release_verify.rs DEFERRED to M1 — they pull
//! ed25519-dalek/hkdf signature infrastructure that M0's local journal
//! does not need (chain-hash integrity is sha2-only).
#![allow(dead_code)]
#![allow(clippy::all)]

#[path = "team.rs"]
pub mod maos_team;
#[path = "region.rs"]
pub mod maos_region;

#[path = "log_composition.rs"]
pub mod log_composition;
#[path = "backup.rs"]
pub mod backup;
