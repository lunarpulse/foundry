#!/bin/bash
# T5 resume — check vendor compile state
set -e
cd /home/cosmo/foundry
export PATH="$HOME/.cargo/bin:$PATH"
cargo check -p foundry-ledger 2>&1 | grep -E "^error" | head -8
echo "CHECK_END"
