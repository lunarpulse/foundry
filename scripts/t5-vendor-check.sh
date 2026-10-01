#!/bin/bash
# T5 shim + check — writes foundry-ledger vendor mod + deps, then cargo check
set -e
cd /home/cosmo/foundry
export PATH="$HOME/.cargo/bin:$PATH"

python3 - << 'PYEOF'
import pathlib
vd = pathlib.Path("crates/foundry-ledger/src/vendor")
for f in vd.glob("*.rs"):
    if f.name == "mod.rs":
        continue
    t = f.read_text()
    t = t.replace("maos_domain::team", "super::maos_team")
    t = t.replace("maos_domain::region", "super::maos_region")
    t = t.replace("use maos_domain::", "use super::")
    f.write_text(t)
print("shimmed")
PYEOF

cat > crates/foundry-ledger/src/vendor/mod.rs << 'EOF'
//! Vendored maos-audit source (commit 92911f59). DO NOT edit without
//! `// FOUNDRY:` markers + VENDORED.md update.
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
#[path = "sealed_export.rs"]
pub mod sealed_export;
#[path = "release_verify.rs"]
pub mod release_verify;
EOF

grep -q sha2 crates/foundry-ledger/Cargo.toml || sed -i 's/rusqlite.workspace = true/rusqlite.workspace = true\nsha2 = "0.10"/' crates/foundry-ledger/Cargo.toml

cargo check -p foundry-ledger 2>&1 | grep -E "^error" | head -8
echo "CHECK_DONE"
