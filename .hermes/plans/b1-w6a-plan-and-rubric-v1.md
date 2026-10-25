# B1 W6a — Integrated Closure Verification (Plan & Rubric v1)

FROZEN 2026-10-24 (approval anchors: standing 1563206386211496081, W6a authorization 1563645799899074662)
Base HEAD: 38dccf5 (W5 complete). Source: b1-closure.md :133-139 (W6/W7 ordering correction).

## Scope
W6a only: the integrated-closure verification wave. NO W7 (real-GitHub verification) — that requires a separate repo/effect/cost/cleanup approval and is reported as NOT PERFORMED. No feature work, no schema, no new routes, no external calls, no credentials, no commit/push at the luna stage.

## Rubric (each row judged PASS/FAIL against its literal text)
- Z1 INVENTORY: a complete tracked+untracked B1 file inventory is fixed at 38dccf5 (every b1-prefixed source/test file, b1-* plans/evidence under .hermes/plans, b1-product-contract.md), saved under scratch, including for each file its tracked status. Plain `git diff` alone is not accepted as the inventory mechanism; new-file originals are materialized for sol input.
- Z2 LINT/FMT: all B1-touched files (per Z1 inventory vs the pre-B1 baseline) show ZERO new clippy warnings (`cargo clippy --locked --offline` per affected crate); pre-existing out-of-scope warnings are enumerated verbatim, not silently ignored. `cargo fmt --check` on B1-touched files passes; whole-workspace reformatting is forbidden (minimal targeted fixes only, existing tests' semantics unchanged).
- Z3 KILL9 SURVEY: every SIGKILL/kill9-family test is enumerated by exact test name across the 9 identified files (foundry-line b1_product_publish / b1_product_result_processes / b1_service_review, foundry-watch m51_gate / t58w_crash_injection / t60g_stress_linearization, foundry-orch lib / t61_end_to_end / t61_kill9). Conflicting same-workspace kill tests are identified and either proven already isolated or separated (new files only; existing tests unmodified). Each isolated kill test has a single-run (standalone) execution record with exit code. A single skipped name is NOT claimed to cover all SIGKILL verification.
- Z4 EXAMPLES: all foundry-crate examples (m2/m3/m4/m7/m8 x2/t16 x2/t17 x2 at minimum, complete list from Z-scan) compile (`cargo check --locked --offline --examples`) and the closure report states approval/standalone compatibility honestly — any example that cannot run in the offline sandbox is reported as such with its exact blocker, not silently dropped.
- Z5 WORKSPACE+DOCKER: `cargo test --locked --offline --workspace` result is recorded with per-crate totals (or the exact first unrelated failure, stopping there); Docker-dependent tests/environment are surveyed — absence of Docker dependency is recorded explicitly if none exists.
- Z6 EVIDENCE+PRESERVATION: an evidence document (.hermes/plans/b1-w6a-evidence.md) records every measured artifact (inventory path, lint counts, kill9 name list + standalone exits, example results, workspace totals). No existing test is modified or weakened; no deletion-to-green; remediation-style edits are restricted to Z2 lint/fmt minimal fixes and Z3 separation-new-files. Honest reporting of anything not measured.

## Out of scope (recorded as open)
W7 real-GitHub verification (separate approval), W6b integrated sol review + final user approval + single integrated commit/push (follows W6a acceptance), cursor/response-budget/verification-cost policy (separate explicit decision, b1-closure :142).

## Erratum v1.1 (2026-10-25, post-freeze correction of a factual error — original text preserved above)
Z4 line: the phrase "t16 x2" is a factual error in the frozen minimum list. The example inventory at 38dccf5 contains exactly NINE targets with exactly ONE T16 example (t16_live_replay); T17 x2 and M8 x2 exist, T16 x2 does not. The rubric's own "complete list from Z-scan" clause makes the scanned inventory authoritative. No rubric criterion is otherwise changed; this erratum corrects the minimum list to match the measured repository (recorded because creating a second T16 example would violate the W6a no-feature-work scope). All other Z1-Z6 text stands unchanged.
