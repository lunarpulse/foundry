# VENDORED — MAOS crates (vendor-in, git dependency = 0)

Per plan §6 vendor rule: MAOS source absorbed into this repo, wrapped behind
foundry-domain traits. MAOS types must not leak (compile-time assertion tests).

## Source

- **Repo:** github.com/lunarpulse/maos
- **Commit:** `92911f59da6a75a7745a23ec05036d18116f68af` (2026-09-25)
- **Local mirror:** `/home/cosmo/maos-vendor-src` (kept for diffing)

## Vendored crates

| Source crate | Foundry crate | Original size | Purpose | License |
|---|---|---|---|---|
| maos-audit | foundry-ledger | 948K (7,261 lines) | Append-only audit ledger (SQLite, chain-hash) → implements `Ledger` trait | Apache-2.0 OR MIT |
| maos-capability | foundry-furnace | 112K | Capability/approval token verification → implements `Gate` trait | Apache-2.0 OR MIT |
| maos-domain (partial: team.rs, region.rs only) | foundry-ledger/src/vendor_support/ | ~20K | Types maos-audit's TeamId/Region depend on | Apache-2.0 OR MIT |

## Isolation contract (plan §6)

- MAOS types are visible ONLY inside `foundry-ledger` and `foundry-furnace`.
- Public APIs of both crates speak exclusively foundry-domain types.
- `tests/no_leak.rs` in each crate enforces this at compile time.
- Any modification to vendored code must be marked `// FOUNDRY:` inline.

## Coupling findings (measured 2026-10-01)

- maos-audit: kernel-core referenced in 2 comments only; kernel-core is a
  dev-dependency (test fixtures), NOT in the library dependency graph.
- maos-audit lib deps: rusqlite, serde, serde_json, thiserror, maos-domain
  (team/region only), ed25519-dalek, sha2, hex, hkdf.
- maos-capability lib deps: maos-domain, maos-attrs, serde, serde_json,
  thiserror, tokio(sync), dashmap, parking_lot, subtle, ring.

## Upgrade procedure

1. `git -C /home/cosmo/maos-vendor-src fetch && git log --oneline -1`
2. Diff vendored dirs against new commit.
3. Re-run isolation tests + full workspace test.
4. Update the commit SHA here.
