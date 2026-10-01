//! foundry-ledger — append-only audit ledger (Ledger trait impl).
//! Chain-hashed, SQLite-backed, append-only (plan §11).
//! The full maos-audit vendor absorption is staged in `vendor/` (see VENDORED.md);
//! this file implements the M0 journal. MAOS types must not leak (tests/no_leak.rs).
// FOUNDRY (T5 decision 2026-10-01): full maos-audit absorption deferred to M1.
// Coupling survey missed crate-root deps (resolve_spirit_name, AuditFilter,
// erasure::merkle) that live in deferred lib.rs/erasure — vendored tree is
// entangled with the rest of maos-audit beyond the M0 boundary.
// M0 uses the native chain-hash journal below (same integrity guarantees:
// append-only + SHA-256 prev-hash chain). Vendored sources stay on disk
// under src/vendor/ for M1 Transparency Log mirroring work.
// #[cfg(any())] keeps it out of the build without deleting history.
#[cfg(any())]
pub(crate) mod vendor;
#[cfg(any())]
pub mod shim;

pub mod sqlite;

pub use sqlite::{AuditEntry, SqliteLedger};
