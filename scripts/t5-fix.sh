#!/bin/bash
# T5 fix — add serde/rusqlite/thiserror/chrono to ledger deps + shim AuditError in vendored files
set -e
cd /home/cosmo/foundry
export PATH="$HOME/.cargo/bin:$PATH"

# deps: serde needed by vendored code
grep -q '^serde' crates/foundry-ledger/Cargo.toml || cat >> crates/foundry-ledger/Cargo.toml << 'EOF'
serde.workspace = true
serde_json.workspace = true
EOF

python3 - << 'PYEOF'
import pathlib, re
vd = pathlib.Path("crates/foundry-ledger/src/vendor")
for f in vd.glob("*.rs"):
    if f.name == "mod.rs":
        continue
    t = f.read_text()
    # audit root types -> ledger re-exports (shim)
    t = t.replace("crate::AuditError", "crate::shim::AuditError")
    # foundry-domain paths (from original maos copy drift) -> our domain
    t = t.replace("foundry_domain::ActorId", "super::maos_team::ActorIdShim")
    f.write_text(t)
print("rewrote vendored imports")
PYEOF

# Create shim module with AuditError
cat > crates/foundry-ledger/src/shim.rs << 'EOF'
//! FOUNDRY shim — minimal stand-in for maos-audit's crate-root error type
//! (the original lives in maos-audit/src/lib.rs which we deferred; M0 journal
//! doesn't need its full variant set). Marked per VENDORED.md policy.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("db: {0}")]
    Db(String),
    #[error("integrity: {0}")]
    Integrity(String),
}

impl From<rusqlite::Error> for AuditError {
    fn from(e: rusqlite::Error) -> Self {
        AuditError::Db(e.to_string())
    }
}
EOF

# lib.rs: add shim module
grep -q "pub mod shim" crates/foundry-ledger/src/lib.rs || sed -i 's/^pub(crate) mod vendor;/pub(crate) mod vendor;\npub mod shim;/' crates/foundry-ledger/src/lib.rs

# team.rs: ActorIdShim — vendored files referencing ActorId from domain
grep -q "ActorIdShim" crates/foundry-ledger/src/vendor/team.rs || cat >> crates/foundry-ledger/src/vendor/team.rs << 'EOF'

// FOUNDRY shim: sealed_export referenced foundry_domain::ActorId (not part of
// vendored maos-audit originally — copy drift). M0 doesn't use it; deferred
// module will be re-attached at M1 with the real type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorIdShim(pub String);
EOF

cargo check -p foundry-ledger 2>&1 | grep -E "^error" | head -8
echo "FIX_END"
