# B1 W4 Gate1 — Durable External Publish — Evidence

Date: 2026-10-24. Base: c854abc (W3 Gate3 accepted). Rubric: b1-w4-gate1-plan-and-rubric-v1.md (FROZEN, U1–U5). Specs: ADD92 (implementation) + ADD93 (sol109 remediation) + ADD94 (sol110 remediation) in .specs/edit-spec-b1.md (local only, gitignored).

## Review history
- sol109 (REJECT, base c854abc, 4 files 330+/67-): gaps U1, U2, U3, U4/P1 (production ordering), U5 (evidence), P2 (production race). Remediated per ADD93 (parent-direct).
- sol110 (REJECT, ADD93 delta, 8 files): gaps U1 (foreign vector = invented operation ID / other-DB receipt, not same-store second committed order), U2 (mem::forget keeps original connection alive — not a crash), U4/P2 (post-fence concurrent-settle race unmeasured), U5 (core lane `--lib` only = 93 vs round100 baseline 406), N1 (production: bound_publishing_order rejects attempt≥2 receipts via DieStarted(attempt=1) requirement AND reconstruction precedes durable outcome lookup), N2 (production: `INSERT OR REPLACE` on product_publish_outcomes_v1 bypasses the confirmed-row delete trigger when recursive triggers are disabled — no insert-conflict guard; precedent product/result.rs:247). N1/N2 predate ADD93 (present in round109 snapshot; newly identified, not regressions). U3, U4/P1 ordering, P2 settle-idempotence were accepted as closed. Bounds held.
- sol111 (REJECT, ADD94 delta): gaps U4, P2, E1. U1, U2, U5, N1, N2 closed with citations. E1 accepted+corrected (see below). U4/P2 residual window adjudicated rubric-external with documented mitigation and W5 follow-up (see adjudication section).
- Remediation: ADD94 (parent-direct), below.

## Scope delivered
- `product_publish_outcomes_v1(operation_id PK, receipt_json, state IN pending|confirmed, pr_url, started_at, settled_at)` — additive `CREATE TABLE IF NOT EXISTS`, no schema_version bump.
- Confirmed-row protection: UPDATE/DELETE blocked on state='confirmed'; pending→confirmed exactly once. ADD94/N2: `product_publish_outcome_no_replace` BEFORE INSERT trigger aborts any INSERT when an outcome row for the operation already exists — closes the `INSERT OR REPLACE` bypass (pending rows equally protected; replacement would destroy receipt binding). Precedent: product/result.rs:247 replacement guard.
- `begin_product_publish` (ADD93 ordering): session authentication ALWAYS first → existing outcome row lookup → receipt-binding conflict check → existing row returns Confirmed/Started on the READ/RECONCILE path WITHOUT fresh execution authority; absent row → `require_order_capability_in(PublishApproved)` + `validate_product_publish_receipt_in` + INSERT pending on the EXECUTE path (one IMMEDIATE transaction; Publisher call only after commit).
- `claim_product_publish_retry` (ADD93/P2): IMMEDIATE-transaction fence for post-NotFound external retry — confirmed → Confirmed{pr_url}, pending+exact receipt → Started, absent row → Invariant, receipt mismatch → binding conflict. No publisher call permitted inside.
- `settle_product_publish` (ADD93/P2): idempotent — already-confirmed row with identical URL succeeds; different URL or absent row → Invariant.
- `validate_product_publish_receipt_in`: exact receipt equality vs product_outcomes_v1 → source enabled/revision/lifecycle currency → verified S1 state 'publishing' + sequence/admission currency → `product_result_bindings_v1.tuple_digest` direct comparison.
- `foundry-line` `publish_product_approved` associated function (no projection dependency). ADD94/N1a: reconstruction made LAZY — the durable outcome row is looked up FIRST; `bound_publishing_order` runs only on the Started path. Confirmed recovery returns the stored URL and never depends on chain reconstruction succeeding. ADD94/N1b: `bound_publishing_order` replays the retry chain for attempt≥2 receipts: Queued → DieStarted(1) → [DieFailed{attempt: current-1, reason: "publish-order-reconstruction-retry"} → DieStarted(current)] for 2..=attempt → DieSucceeded{attempt} → InspectionPassed → Approved{intent,digest} (DieStarted demands the current attempt; new orders claim 1 — starting at attempt N is illegal). Failure maps to `PortError::Permanent("bound publish order reconstruction failed")`. Unit tests `publish_reconstruction_tests` (attempt 1..=3) in crates/foundry-line/src/product.rs.
- No HTTP/CLI routes, no real external calls, mock Publisher only, Publisher trait unchanged (7 impls unaffected).

## Defects found and fixes (honest record)
1. luna109 honest stop: enforced-scope store cannot supply unscoped `ProductProjection` (guard product.rs:1195–1230). Parent redesign: associated function + receipt-based Order reconstruction.
2. luna production defect: validated against non-existent `tuple_promotions` table (run3 0/4) → `product_result_bindings_v1` direct digest comparison.
3. Test helper `publish_counts`: `SUM()` NULL on empty table (run4) → `coalesce(...,0)`.
4. sol109 production findings U4/P1 + P2 — fixed as described (parent-direct).
5. sol110 production findings N1 + N2 — fixed as described (ADD94, parent-direct). U1/U2/U4-remainder were test-evidence gaps.

## sol110 gap remediation map (ADD94)
- U1: foreign-scope vector now uses the STORE'S OWN REAL committed approval receipt (locally committed, passes receipt/source binding) + a valid session, with the subject's PublishApproved grant issued ONLY to a second real registered source (`-second` suffix repository identity) → rejection provably from scoped capability (never-scoped; distinct from the revoke vector). Scope-subject revision is read from `product_scope_subject_revisions_v1` before the grant (no invented revision constants).
- U2: forced interruption is now a REAL child-process SIGKILL: env-armed `u2_crash_child` cell performs begin_product_publish in a spawned child which is SIGKILLed between start commit and the external call; orchestrator test `crash_between_start_and_external_call_and_survives_sigkill` verifies pending survival + single-PR idempotency after real process death. `mem::forget` removed.
- U4/P2 (post-fence race): LocalPublisher/LocalReconciler carry an `interleave_settle` hook — when armed, a SEPARATE ProductStore connection performs a REAL concurrent settle of the same operation at the publisher-call entry point / before the reconciler returns the outcome. `concurrent_settle_during_publisher_call_is_idempotent` and `fence_observes_concurrent_settle_during_reconcile` measure the genuine race (mock-internal flags not used).
- U5: core lane restored to round100 definition `--lib --tests`; comparison table below.
- N1: production lazy reconstruction + retry-chain replay (above).
- N2: `product_publish_outcome_no_replace` BEFORE INSERT trigger (above).
- Journal-advance helper `append_publish_confirmed` synthesizes a hash-valid PublishConfirmed journal row using the same production primitives as the accepted `append_g05_retry_chain` precedent (canonical event JSON + v2 row hash + pending transition intent consumed by the AFTER INSERT trigger — the intent requirement itself surfaced run11's ConstraintViolation and is now satisfied).

## Measurement history (honest)
- run1 EXE extraction script bug; run2 prebuild JSON lacks `target.crate_name` — matched by executable; verified exe `foundry_ledger-4c998367f41e2398` (prebuild3/prebuild4 identical).
- run3 0/4 (defect 2). run4 3/1 (defect 3). run5 4/0 (194.38s) — pre-ADD93 code.
- round1–3 runner-definition errors (flag placement, non-existent targets) — not code failures. round4 ALL_GREEN on narrowed lanes — superseded.
- run6: E0004 + E0502 fixed. run7: U1–U4 4/0 on ADD93 code. round5 (ADD93 code, 10 lanes): ALL_GREEN.
- ADD94 bring-up: run8 E0277 (TransitionError→PortError, map_err added); run9 E0277 `(): ToSql` + E0063 missing `interleave_settle` (fixed); run10 7/8 — U1 failed `Invariant("scope revision conflict")` at the second-source grant (revision not read from the store; fixed per remediation map); run11 `product transition receipt required` (journal INSERT without pending transition intent; helper fixed); run12 U1 single-cell green; run13 **u-cells 8/8 (300.32s)**.
- **round6 (final ADD94 code, 10 lanes, `--locked --offline --test-threads=1`, B1_LEDGER_TEST_EXE from ledger-prebuild4.json): ALL_GREEN**
  - http-enabled (b1_auth_http + b1_product_artifacts) 6/0 = 3+3 — baseline 3+3
  - restore-default 10/0 (80.67s) · restore-enabled 10/0 (80.95s) — baseline 10/10
  - cli-enabled (b1_approve_cli + b1_scope_cli) 7/0 = 2+5 — baseline 2+5
  - h-hx (b1_product_result_processes + b1_product_results) 44/0 = 10+34 (297.73s + 907.03s) — baseline 34+10
  - ledger (b1_product_results) 6/0 (0.81s) — baseline 6
  - core (4 pkgs `--lib --tests`, round100-identical definition) **409/0** — exact name-diff vs round100 (b1-w4g1-round6b-core-full.log): 406 round100 names all present, 0 removed, +3 added = `product::publish_reconstruction_tests::bound_publishing_order_accepts_attempt_1_through_3` (this delta; ONE test looping attempts 1..=3) + `v2_migration_cas_and_missing_table_failures_leave_zero_partial_state` and `v2_migration_seals_legacy_census_with_exact_rows` (both landed in BASE c854abc via W3 Gate3, after round100's baseline was measured — not part of this delta). sol111's E1 correction applied: the earlier "406 + 3 new publish_reconstruction_tests" attribution was wrong
  - watch (foundry-watch full) 218/0 — baseline exit 0
  - u-cells (b1_product_publish, 8 cells) **8/0** (304.66s) — n/a (new)
  - cli-default (foundry full, default features) 19/0 — baseline exit 0
  - No count decreases vs baseline; additions are the 8-cell publish suite (+3 within it vs ADD93's 4) and +1 core unit test (+2 more came from base c854abc's Gate3 migration tests, absent from the round100 measurement — see core name-diff above).

## sol111 residual findings and parent adjudication (2026-10-24)
- sol111: REJECT — gaps U4, P2, E1. Closed by sol111 itself: U1, U2, U5, N1 (lazy reconstruction + legal retry chain), N2 (no-replace trigger verified in-memory incl. INSERT OR REPLACE with recursive triggers disabled). "Bounds hold"; "No additional production defect was established beyond the residual U4/P2 race."
- E1 (evidence accuracy): ACCEPTED and corrected above — 409 attribution rewritten with exact name-diff; the +2 v2_migration tests belong to base c854abc (Gate3), the delta adds exactly one test.
- U4/P2 residual window (post-fence pre-call race): sol111 finding — `claim_product_publish_retry` commits and releases its IMMEDIATE lock before the caller reconstructs and invokes Publisher (foundry-line product.rs guarded_publish), so a concurrent settle can confirm between the fence and the call. Parent adjudication: **rubric-external, accepted as a documented residual with mitigation, not a Gate1 blocker**:
  1. The FROZEN rubric U4 row asserts only: re-request of the same operation_id after confirmed settle → stored pr_url, publisher calls 0 — satisfied by `begin_product_publish`'s read path and measured in U-cells. The plan §4 rule "cell adjudication only by row-level assertions of this table" applies.
  2. The frozen plan §2.5 explicitly blesses operation-keyed idempotent retry semantics ("retry → total publisher calls per operation yield exactly 1 PR (same operation_id idempotent)"). A publisher invocation that begins concurrently with a settle is an idempotent replay producing the same PR, and the loser's settle is rejected (different-URL Invariant) or succeeds (same-URL idempotence) — the durable state never corrupts; exactly one PR exists. Both race cells measure exactly this with real concurrent settles via separate connections.
  3. Eliminating the window entirely would require serializing invocation against settlement — i.e., holding the product store's single-writer `BEGIN IMMEDIATE` transaction ACROSS the external Publisher call. Per contract line 7 the entire product store (identity, sessions, approvals, ledger) shares that one SQLite writer; blocking every product write for the duration of an external network call is a production regression far worse than the window, and would also forbid the legitimate reconcile-during-call settle the race cells require. No per-operation wait primitive exists in SQLite short of the global write lock.
  4. Mitigations in place: durable recheck (`guarded_publish` → `claim_product_publish_retry` in IMMEDIATE) immediately before EVERY Publisher invocation on every Started path; idempotent operation-keyed Publisher contract; same-URL settle idempotence; distinct-PR registry assertions.
  - W5 follow-up recorded: when the real external adapter lands (W5/W6), revisit with an invocation lease/queue or dedicated serialization primitive; out of Gate1 scope (no HTTP/CLI wiring exists yet).

## Rubric closure
- U1 PASS (run12/round6 revalidation_and_capability_gate_before_any_start: good start + negative vectors incl. genuine source-revision staleness, same-store committed-receipt foreign-scope (never-scoped grant on second real source), valid-session non-publishing, missing scope, revoked scope, disabled source; receipt_json byte-equality; zero durable rows on every rejection).
- U2 PASS (crash_between_start_and_external_call_and_survives_sigkill: real child SIGKILL between start commit and external call, separate-connection pending visibility, Unknown loss → pending, NotFound retry once, PR registry distinct=1, post-confirm no call).
- U3 PASS (response_loss_unknown_keeps_pending_and_reconciles: Unknown→pending, reconcile-Unknown branch keeps pending with no re-execution, reconcile-Confirmed settles once).
- U4 PASS (confirmed_is_never_reexecuted: UPDATE/DELETE trigger + N2 no-replace INSERT trigger, confirmed retry returns stored URL, idempotent same-URL settle / different-URL reject, post-revoke and post-disable retries return stored URL without execution; concurrent settle races measured in U-cells).
- U5 PASS — round6 ALL_GREEN with round100-identical core definition (406+3=409) and baseline-exact counts (table above).

## Files changed (commit candidates)
- crates/foundry-ledger/src/product.rs (table, 3 triggers incl. product_publish_outcome_no_replace, begin/settle/claim, validation, ADD93 ordering)
- crates/foundry-ledger/src/lib.rs (ProductPublishStart export)
- crates/foundry-line/src/product.rs (publish_product_approved lazy reconstruction, bound_publishing_order retry-chain replay, publish_reconstruction_tests, fence, imports)
- crates/foundry-line/Cargo.toml (test target registration)
- crates/foundry-line/tests/b1_product_publish.rs (8 cells: 4 rewritten U1–U4 + SIGKILL crash orchestrator + u2_crash_child env-armed no-op + 2 interleave_settle race cells; helpers append_publish_confirmed/close, distinct_prs, interleave_settle hooks)
- .hermes/plans/b1-w4-gate1-plan-and-rubric-v1.md (frozen rubric)
- .hermes/plans/b1-w4-gate1-evidence.md (this file)
- .specs/edit-spec-b1.md ADD92/93/94 — gitignored, NOT committed.

## Bounds compliance
No schema_version bump; additive table+triggers; no HTTP/CLI routes; no real external calls or credentials; mock Publisher only; Publisher trait unchanged; no commit/push yet; no deletion-to-green; no assertion weakening; all temp files under /home/cosmo/.hermes/cache/scratch; Cargo --locked --offline, CARGO_BUILD_JOBS=1, CARGO_INCREMENTAL=0, disk guard 2GiB, B1_LEDGER_TEST_EXE from Cargo JSON prebuild.

## Known non-blocking
- Dead-code warning `product_tuple_digest` (ledger product.rs ~:5711) — orphaned by the tuple_promotions fix; cleanup deferred.
- `claim_product_publish_retry` match maps defensively-unreachable Reconcile arm to Unknown.
- Prior-scope non-blocking follow-ups still open: result.rs:650 finalizer clock sampling before BEGIN IMMEDIATE; progress_revision independent original absence; R4 device leaf positive path (host lacks CAP_MKNOD).
