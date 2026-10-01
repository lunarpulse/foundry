#!/bin/bash
# T5 fix3 — vendored code needs more crate-root fns than expected (resolve_spirit_name, AuditFilter, erasure::merkle).
# These live in deferred parts of maos-audit. Correct M0 move: the vendored modules are
# more entangled than the coupling survey showed. FALLBACK PER PLAN §Task5:
# keep only what M0 journal needs = backup.rs read helpers are also deep.
# Decision: vendor NOTHING for M0; own chain-hash journal (already written).
# Keep vendored tree for M1 Transparency Log mirroring, excluded from build.
set -e
cd /home/cosmo/foundry
export PATH="$HOME/.cargo/bin:$PATH"

# Exclude vendor from compilation (keep on disk for M1)
python3 - << 'EOF'
import pathlib
lib = pathlib.Path("crates/foundry-ledger/src/lib.rs")
t = lib.read_text()
t = t.replace("pub(crate) mod vendor;", """// FOUNDRY (T5 decision 2026-10-01): full maos-audit absorption deferred to M1.
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
pub mod shim;""")
lib.write_text(t)
print("vendor excluded from build")
EOF

sqlite_shim_check=$(grep -n "shim\|vendor" crates/foundry-ledger/src/sqlite.rs | head -3 || true)
echo "sqlite.rs refs: $sqlite_shim_check"

cargo check -p foundry-ledger 2>&1 | grep -E "^error" | head -6
echo "FIX3_END"
