#!/bin/bash
# T5 fix4 — clean sqlite.rs imports; fix sha2 writer; check domain ActorId name
set -e
cd /home/cosmo/foundry
export PATH="$HOME/.cargo/bin:$PATH"

# What is the actor type actually called in foundry-domain?
grep -n "pub struct ActorId\|pub enum ActorKind" crates/foundry-domain/src/ports.rs || true

python3 - << 'EOF'
import pathlib
sq = pathlib.Path("crates/foundry-ledger/src/sqlite.rs")
s = sq.read_text()
# collapse to single import line
s = s.replace("use foundry_domain::ports::{Ledger, PortError, PortResult};\nuse foundry_domain::{ActorId, OrderEvent, OrderState};",
              "use foundry_domain::ports::{Ledger, PortError, PortResult};")
s = s.replace("use foundry_domain::ports::{Ledger, PortError, PortResult};\nuse foundry_domain::{ActorId, OrderEvent, OrderState};",
              "use foundry_domain::ports::{Ledger, PortError, PortResult};")
sq.write_text(s)
print("imports collapsed")
EOF

cargo check -p foundry-ledger 2>&1 | grep -E "^error" | head -6
echo "FIX4_END"
