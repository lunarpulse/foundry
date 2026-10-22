# B1 W3 recovery — executable acceptance handoff

Status: REQUIRED / NOT YET VERIFIED. Approval 1561615896773009419; frozen contract approval 1561496539174801409. These rows operationalize the COMPLETE effective contract, not amend it. Governance/v3 Appendix D > R1–R7/C > D1–D8 > B > A. Read that contract for exact nested schemas, stage scripts, error precedence and named scenario coordinates. No passing status is inferred from static inspection.

## Owners, fixtures and command keys

- L: `crates/foundry-ledger/tests/b1_product_results.rs`; pure schema/encoding fixtures only.
- H: `crates/foundry-line/tests/b1_product_results.rs`; genuine-host positives and producer-dependent corruption negatives.
- HX: `crates/foundry-line/tests/b1_product_result_processes.rs`; process races/crashes, ledger cfg(test) child handoff as defined in recovery plan/addendum. Add explicit required-features target declaration.
- S: `crates/foundry-serve/tests/b1_product_artifacts.rs`; feature-gated result modules, retain default stage artifact coverage.
- C: `crates/foundry/tests/b1_scope_cli.rs` and `b1_approve_cli.rs`; feature-gated strict-result/server modules, retain default non-result coverage.
- Shared test-only producer: `crates/foundry-line/tests/support/product_result_fixture.rs`, included by path from S/C test code, not exported by production libraries. Parameterize installation/database/config/source/order identities; each caller supplies its own expected config. Return exact paths/IDs, never copied DB/root trees. S/C already depend on line; no reverse ledger dependency.
- G: exact admission → explicit synthetic P2 lifecycle preparation → genuine work claim → frozen PackReviewHost with scripted fake executor → finalizer. No direct tuple/result/promotion seeds. G-pre denotes the same genuine host baseline BEFORE finalization. G-final denotes demonstrated successful finalization.
- A/B/direct: exact scripts/coordinates in frozen B and C§7. Call vector is execute/restore/dispatch. Initial/direct vector 4/0/1; scenario A/B vector 14/3/1; exact stage scripts are normative, never guessed from these totals.
- V2: faithful isolated historical v2 schema/rows, not a fresh-v3 metadata downgrade as the sole proof. Original history bytes retained.
- Every corruption variant starts with demonstrated G-final or appropriate genuine G-pre success on a separate baseline; restore before each independent mutation. Immutable-record tamper negatives may deliberately remove a guard ONLY in isolated negative evidence copies after first proving guard rejection; never in positives.
- Command K-L: `cargo test --locked --offline -p foundry-ledger --test b1_product_results`.
- K-H: `cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_results`.
- K-HX: prepare exact ledger unit executable via Cargo JSON OUTSIDE tests, export B1_LEDGER_TEST_EXE, then `cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_result_processes`.
- K-S: `cargo test --locked --offline -p foundry-serve --features p2-test-support --test b1_product_artifacts`.
- K-C: `cargo test --locked --offline -p foundry --features p2-test-support --test b1_scope_cli --test b1_approve_cli`.
- K-D: existing default core→watch→CLI chain from recovery plan, stop on failure.
- All Cargo commands serial, jobs=1, incremental=0, locked/offline, >=2GiB free guard. Commands indicate future required runs, not claimed results.

## Effective rows: name — fixture/order — required assertions — command

### Ownership, mapping and wire

- `standalone_store_still_rejects_product_v3` (L plus existing exec tuple_sqlite target): fresh/migrated v3 → standalone tuple/host adapters; reject before schema/row mutation, preserve standalone semantics. K-L and `cargo test --locked --offline -p foundry-exec --test tuple_sqlite`.
- `genuine_host_result_finalizes_without_emit_or_publish` (H): G-pre → finalize → replay; exact accept/result/run each 1; stage/artifact/session 4/4/3, T/R 1/1, promotions 0; calls 4/0/1, replay additional calls and writes 0. K-H.
- `result_uses_persisted_numeric_run_and_activation` (H): G with run7/activation3 and colon-containing order; exact journal mapping, tuple run7/generation0, no parse fallback. K-H.
- `activation_and_lifecycle_aliases_cannot_share_tuple_key` (H): G-final winner then same tuple key/different activation or lifecycle; correct conflict/corruption precedence, unchanged T/R 1/1 and no execution. K-H.
- `result_coordinates_reject_overflow_and_wrong_attempt` (L pure overflow; H replay-backed wrong/negative DB coordinates): checked width errors including i64::MAX+1 and wrong replay attempt; no wrap/fallback or tuple/result/approval effects. K-L/K-H.
- `tuple_and_result_binding_commit_as_one_set` (HX): G-pre → deterministic tuple-insert and binding-insert error/barrier; precommit T/R 0/0, original host/artifact intact, no dispatch. K-HX.
- `nonempty_generation_digest_matches_exec_and_preserves_legacy` (L): exact single/multi typed chains and legacy encoding fixture; new digest == exec, distinguish nonempty historical Value encoding, unchanged history bytes. K-L.
- `result_payload_and_local_receipt_have_exact_v1_bytes` (L pure private encoding unit coverage if needed; H persisted genuine evidence): exact outer/nested order/raw embedded bytes/local receipt; unknown/duplicate/missing/noncanonical members rejected, stable digest. K-L/K-H and affected ledger unit target.
- `emit_key_and_promotion_tuple_are_not_interchangeable` (L encoding/H stored negative): real serialized emit/promotion receipt is not accepted as local-result receipt; no fallback approval. K-L/K-H.

### Genuine ancestry, authority and eligibility

- `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` (H): independent G-pre variants for empty chain/first commit/index/last commit/fix log/produced_commit; fail without T/R changes or host rerun. K-H.
- `accepted_resume_binds_three_generations_and_fourteen_artifacts` (H): frozen scenario A; 14/3/1, artifacts14/sessions8, chain indexes0/1/2, tuple generation0; original plus followup artifact/hold provenance retained, no ancestor re-registration. K-H.
- `commitpass_resolves_origin_without_materializing_reserved_run` (H): frozen scenario B 14/3/1 with origin8/reservation9; direct 4/0/1 origin7/reservation8; retain accepted hold JSON, no successor, no extra allocator increment. K-H.
- `result_location_must_resolve_registered_terminal_evidence` (H): independent G-pre invalid terminal path, fallback fragment, foreign stage, external/unregistered path; reject with T/R 0/0 and no caller-path adoption. K-H.
- `held_failed_and_materialized_continuation_are_not_final_results` (H): genuine held, failed, copied-resuming baselines → Ineligible; no T/R, original history/session preserved. K-H.
- `queued_success_is_not_silently_made_approvable` (H): genuine queued vs waiting baselines; reject queued, accept waiting; finalizer S1 delta0. K-H.
- `authority_is_captured_at_activation_and_copied_on_resume` (H plus HX error/barriers as necessary): intake/claim snapshots0 → activation +1 atomic → Resume +1 exact eight members → CommitPass +0; copy/admission rollback all-or-none; update/delete/REPLACE denied. K-H/K-HX.
- `legacy_host_census_distinguishes_missing_authority_from_corruption` (H): faithfully migrated genuine legacy host/descendant missing snapshot → Ineligible; independent new initial/Resume missing mandatory copy → CorruptEvidence; no inferred authority. K-H.
- `completed_initial_claim_finalizes_after_worker_expiry` (H): initial G completed work/backlog, expire worker lease without service expiry; finalization succeeds with 4/0/1 total and stage/artifact/session4/4/3, T/R+1/+1, replay+0/+0, claim/lease unchanged. K-H.
- `pending_result_requires_original_authority_revisions` (H/HX): after genuine pending initial/A/B independently rotate service/credential/source or revoke→regrant; reject new finalization; for A/B validate human revision only where that accepted lineage exists, unrelated human change must not reject initial automatic success. Preserve old finalized bytes. K-H/K-HX.
- `revoke_after_stage_start_preserves_result_but_blocks_finalization` (HX): flushed start-commit barrier → revocation/rebind commits → stage completion; preserve accepted stage/session, no next execute/result finalization; claimed interaction limits follow contract. K-HX.
- `revoke_before_stage_start_has_zero_executor_calls` (HX): revocation commit → start; zero new stage/session/artifact/tuple/result, execute0. K-HX.
- `reassigned_owner_cannot_store_or_finalize_old_result` (HX, split variants): ordinary stage/completion and direct CommitPass terminal persistence each exercised after fence reassignment; ordinary stale completion refusal vs possibly retained CommitPass terminal evidence distinguished; BOTH strict new finalizations reject and replacement ownership is unchanged. K-HX.
- `initial_redrive_remains_unsupported` (H): exact frozen failure script; calls5/0/1, complete stages/artifacts4, sessions3, **budget5** by approved Discord1561969234354114641 option A (budget4→5 only), failure row1, no numeric host result or tuple/result. K-H.
- `continuation_revocation_distinguishes_restore_from_execution` (H/HX): exact frozen cumulative5/1/1, restore may precede next authorization, forbidden further stage execute0; no broad zero-restore claim. K-H/K-HX.
- `interleaved_order_evidence_pairs_own_sequence_and_hash` (H): G order row followed by foreign-order row before finalization; payload S1 sequence/hash match SAME selected verified row, global chain still verified; unrelated append does not corrupt baseline. K-H.

### Replay, race, crash and ambiguous recovery

- `stored_result_replay_survives_approval_and_original_authority_rotation` (H): G-final → approve/rotate → currently authorized replay returns identical receipt; no expired-old-work/waiting check; expired caller denied; all stored-set corruption rejected. K-H.
- `two_process_exact_replay_returns_one_identical_receipt` (HX): same G-pre expectation, both controlled commit orders; equal receipt, T/R1/1, zero added execute/restore/dispatch/sink/publish during finalization/replay. K-HX.
- `two_process_changed_expectation_never_replaces_winner` (HX): same baseline correct/wrong digest, both orders; before winner wrong -> InvalidEvidence, after valid winner wrong -> Conflict, only correct succeeds; unchanged winner bytes/T/R1/1. K-HX.
- `kill_before_and_after_result_commit_recovers_without_dispatch` (HX): observe tuple/binding insert and postcommit-before-response child barriers, SIGKILL, reopen; precommit0/0, postcommit1/1, retry/lookup exact, no execution. K-HX.
- `ambiguous_commit_uses_lookup_and_never_treats_io_error_as_absence` (HX): DISTINCT response-loss, actual/injected COMMIT-I/O failure, and recovery lookup-I/O failure cases. Postcommit SIGKILL alone is not all three. Exact receipt or specified uncertainty/storage error; no blind redispatch, no absence-on-error. Test-only fault injection must be at the actual operation, not fabricate successful API output. K-HX.
- `approval_replay_and_ambiguous_commit_validate_result_side_binding` (HX): G-final approval replay/ambiguous recovery; missing/mismatch/corrupt sidebinding never succeeds, no duplicate participants; storage errors not absence. K-HX.

### Authenticated result and artifact consumers

- `bound_result_artifact_reads_require_original_source_grant` (S/C): G-final → login → actual result metadata and bound-artifact routes; correct200 bytes/IDs/digests/no-store, bearer absent401, foreign/no grant/wrong association404, real storage/unavailable503, no internal paths/secrets. K-S/K-C.
- `retired_source_history_remains_readable_without_new_source_inheritance` (S/C): G-final → disable/rebind source; old explicit ReadArtifact succeeds, new-source-only account404; no new approval/publication inheritance. K-S/K-C.
- `each_result_integrity_mutation_starts_from_verified_baseline` (H/S): independent file bytes/SHA/path/provenance/tuple/binding/host/acceptance/progress tamper; positive before each mutation, rejection after, positive after restore; full stored-set checks also apply to METADATA/lookup/replay/approval, not just artifact endpoint. K-H/K-S.
- `result_reads_preserve_pinned_root_and_no_follow_guards` (S plus existing ledger resolver units): deterministic leaf/ancestor/root replacement, symlink/FIFO/device; original bytes or safe rejection, never outside/partial bytes; unsupported platforms fail closed. K-S and ledger lib tests.
- `product_result_read_and_approval_work_over_authenticated_loopback_server` (C): test-owned G-final DB/root → REAL product server socket → login/metadata/artifact/prepare/approve; exact200/refusal statuses and bytes; no Publisher calls. Router::oneshot proof remains separate S evidence. K-C.

### Approval, history, migration and support isolation

- `approval_consumes_exact_local_result_in_atomic_transaction` (H/S/C): G-final human grant→prepare→approve; approval S1/transition/token/outcome/consumed grant/sidebinding exactly one set; no publication. K-H/K-S/K-C.
- `approval_rejects_changed_result_without_consumption` (H/S): independent foreign/stale tuple/source/attempt/generation/digest negatives; no approval delta or grant/token consumption, original result preserved. K-H/K-S.
- `approval_result_binding_rolls_back_with_each_existing_participant` (HX): barriers for S1/grant/token/outcome/new sidebinding and commit response; all absent or exact one set on reopen; no duplicate replay. K-HX.
- `v2_to_v3_migration_is_additive_atomic_and_old_binary_fenced` (L pure schema; HX genuine historical hosts/outcomes): faithfulV2 → migration with DDL/population/CAS failure injection; exact host AND tuple census membership, cutoff/meta/version one transaction, old bytes unchanged; rollback remainsV2 with no partial schema; fresh empty sealed lists; INSERT/UPDATE/DELETE/REPLACE rejection; idempotent existing-v3 nonrepairing validation; v2 normal open refuses; old standalone/product open fences verified. K-L/K-HX.
- `legacy_tuple_membership_distinguishes_unbound_history_from_corruption` (L sealed DDL; H/HX classification): migrated unbound tuple WITHOUT host/approval evidence -> Conflict; independently damaged genuine winner missing binding -> CorruptEvidence; census tuple with binding, census without tuple and binding without tuple -> CorruptEvidence; no None or mutations; no adoption of history. K-L/K-H/K-HX.
- `legacy_receipts_remain_history_but_cannot_authorize_new_results` (HX): exact cutoff vs cutoff+1, prepared-before/committed-after strict, preserve complete old receipt/outcome/consumption/S1 relationships; scoped AND unscoped v3 legacy publish authorization reject with0 calls. K-HX.
- `result_error_precedence_does_not_reveal_foreign_evidence` (H/S): combined unauthorized+corrupt, stale+missing file, expected mismatch+corrupt winner, expired worker+valid existing winner and read-I/O errors; full frozen precedence, uniform external responses and no mutation. K-H/K-S.
- `p2_preparation_walks_s1_without_result_seeding` (H): exact original acked admission/current authority/zero prior state; adapter+3S1/+3receipts atomically, pending intents0, claims/results/tuples/promotions0 before claim; invalid inputs/conditional-row failure rollback. K-H.
- `p2_support_has_no_product_http_or_cli_entrypoint` (S/C plus external compile probes): default public type+METHOD absent with intended compiler diagnostics; enabled counterparts compile; even enabled/unified normal routes/commands/startup/deserializers cannot invoke adapter. K-S/K-C plus probe commands in addendum.
- `w3_paths_never_invoke_publisher_or_create_publish_start` (H/S/C): spies and DB deltas across success/failure/retry/reopen; publisher0, publish-start/outcome publication effects0, scoped/unscoped legacy guards stay blocked. K-H/K-S/K-C.

## Old test assertion ownership (no silent deletion/skip)

- ledger unit `separate_process_hold_acceptance_same_and_different_keys` and `sigkill_at_each_real_hold_acceptance_statement_is_all_or_none`: remain ledger lib tests, hold-only setup, unchanged hold-specific barriers/assertions; exclude irrelevant tuple/promotion/approval setup. K-D.
- ledger unit `sigkill_at_every_uncommitted_insert_rolls_back_and_reopens_writable`: strict approval orchestration replaced by HX approval boundary test; ledger unit child remains cfg(test), parameterized manifest. Preserve all old barriers plus new sidebinding/postcommit. K-HX.
- ledger integration `product_approval_atomically_writes_s1_and_consumption_then_replays_exact_receipt`: relocate genuine positive to H/HX; preserve reservation reclaim, stale-proof rejection, EACH fault rollback, exact successful/reopened replay and participants; add sidebinding. Legacy publish-positive and mutable tuple update/restore are superseded: v3 authorization rejects, tuple immutability is separately asserted, deliberate integrity negatives use independent baseline. K-H/K-HX. Keep unrelated ledger-only cases in original target.
- line restore test containing `PublishOutcome::Confirmed` and publisher1: choose a G-final STRICT result fixture and reject legacy publication, replacing the entire Confirmed/observed-call block and obsolete tuple seed setup. Preserve projection catch-up/reopen/receipt properties where still valid. Put replacement into H/HX to avoid feature-gating the whole existing restore target; remove only mapped obsolete orchestration after replacement exists. Historical outcome bytes/replay separately covered by HX legacy row. K-H/K-HX; default restore suite retained K-D.
- foundry approve CLI strict positive: feature-gated G-final parameterized fixture; K-C explicitly covers it; keep default invalid-input/auth negatives, no ignored tests or unconditional support features.

## Recording rule

For EACH row, implementation report records actual target/test, baseline path, mutation/barrier order, exact frozen expected counts/errors, reached token where applicable, command/log/exit and PASS/GAP/BLOCKED. A missing case or prohibited substitution is GAP even if totals pass. Parent validates matrix against source and actual output. No W3 acceptance while any effective row is missing. This matrix does not authorize W4 or batch commits/push.

## Round80 implementer delta — ADDENDUM77 section 3 (parent verification pending)

Only H test assertions and private `#[cfg(test)]` result-wire cases were extended. Parent verification remains required; this section records measured subcases and does not close rows whose full frozen assertion set remains absent.

| Row | Round80 evidence | Status |
|---|---|---|
| G05 | Exact `i64::MAX+1` checked width probes, persisted negative attempt/run coordinates, and replay mismatch exercised in H/L. | PASS |
| G07 | Existing typed digest/history-byte tests rerun in L (4 passed). | PASS; no v2-host implication |
| G08 | Nested malformed authority/accepted-hold/artifact wire cases and stored payload/receipt version/kind/noncanonical mutations added; raw bytes restored. | PARTIAL; full independent nested matrix/digest-byte mapping pending audit |
| G09 | Genuine stored result replaced independently by serialized EmitReceipt and PromotionReceipt; authorized lookup rejects as CorruptEvidence and receipt bytes are restored. | PARTIAL; fallback approval-consumption vector not fully joined |
| G10–G12 | Existing H ancestry/A/B/direct tests rerun; the remaining complete clean-baseline matrix, exact A census/coordinates, and full B successor/allocator/CommitFail/Abort measurements were not added. | GAP |
| G13 | Named path test now has genuine controls, isolated fallback/foreign-stage/external mutations, exact rejection, no-effect counts, restore, and re-finalization. | PARTIAL; malformed terminal path still absent |
| G14 | Genuine live-authority materialized run8/resuming state returns Ineligible with no result/tuple changes. | PASS |
| G15 | Existing queued/waiting contrasts rerun in H. | PASS |
| G16 | Activation authority insert error rollback measured; corrected Resume copy query executes in core restore lane. | PARTIAL; progress and all participant linkage not fully covered |
| G17 | No faithful historical-v2 host fixture. Fresh-v3 downgrade remains explicitly inadmissible. | GAP / blocked by unavailable historical host bytes and supported v2 fixture opener |
| G18 | Existing completed-initial expired-worker success/replay test rerun in H. | PASS |
| G19 | No complete completed-pending initial/A/B authority mutation matrix joined to finalizer and lookup. | GAP |
| G20 | HX child-process lane passed 5/0, but no result-producer start-commit → revoke/rebind → completion child sequence was added. | GAP |
| G21 | Pre-stage revocation now measures executor and durable counts plus finalizer refusal, but does not reach an actual stage-start transaction attempt. | PARTIAL / GAP |
| G22/G23 | No edits this round; preserve round79 parent-verified disposition. | PASS, inherited |
| G24 | Followup finalizer/recovery assertions added; revocation still occurs after completed stage, not during `replan_full-g1`. | PARTIAL / GAP |
| G25 | Existing interleaved-order row rerun in H. | PASS, inherited |
| G44 | Duplicate post-claim preparation rejection and unchanged durable census added. | PARTIAL; required preclaim/stale/invalid/zero-row rollback matrix absent |

Full per-gap and per-lane evidence is recorded in [Round80 acceptance evidence](/home/cosmo/foundry/.hermes/plans/b1-w3-acceptance-evidence.md#round-80--addendum77-section-3-residual-rubric-work-implementer-evidence-parent-verification-pending). The nine-lane run did not establish W3 acceptance; sandbox loopback cases require parent rerun as recorded there.

## Round82 — ADDENDUM79 final disposition (parent verification pending)

| Gap | Final status for this implementation round |
|---|---|
| G08 | STOPPED on production defect: accepted-hold `actor_id` mutation returns `StaleBinding`, expected `CorruptEvidence`; remaining nested members unmeasured. |
| G10 | GAP — independent clean-baseline mutation matrix incomplete. |
| G11 | GAP — frozen input/path/session, fixed progress, both-host census, and origin/reservation mismatch set incomplete. |
| G12 | GAP — full boundary deltas, successor census/allocator, CommitFail/Abort controls incomplete. |
| G13 | PASS for the distinct malformed terminal-path case; focused test also exercised independent controls and no-effect/restoration assertions. |
| G16 | PASS for combined progress-row, participant-linkage, and S1/receipt rollback baseline. |
| G17 | UNMEASURED only for legacy missing-snapshot `Ineligible`; the exact historical-v2 fixture blocker and measured modern missing-initial/missing-copy contrasts are documented in Round82 evidence. |
| G19 | GAP — completed-pending initial/A/B authority table and lineage human-account cases incomplete. |
| G21 | PASS — a test-support-only signal observed the real continuation immediately before its stage-start transaction; revocation committed at that boundary and finalizer refusal/no-effect assertions passed. |
| G44 | GAP — duplicate preclaim, stale/invalid admission/source, and conditional zero-row rollback cases incomplete. |

The nine regression lanes stopped during H at the first unrelated failing test, `completed_result_rejects_reclaimed_and_same_owner_aba`; see Round82 evidence for partial counts. No subsequent lane was started.

## Round83 — ADDENDUM80 final disposition (parent verification pending)

G08 actor classification is repaired in `result.rs`: accepted-hold receipt `actor_id` must match the durable acceptance actor column, and immutable request-digest-backed receipt members are checked as stored-evidence integrity before current authority validation. The full H member matrix passed with `CorruptEvidence` for all 19 receipt members, including actor_id; real authority revocation/replacement still reaches the unchanged `ensure_current_acceptance_authority_in` and returns `StaleBinding`. H passed 30/0. The earlier panic for `completed_result_rejects_reclaimed_and_same_owner_aba` was the same shared G08 actor corruption assertion, not a reclaim defect or G21-hook regression; the exact four-vector focused test and H both now pass.

G10, G11, G12, G19, and G44 remain GAP with their Round82-listed missing cases unchanged. G17 remains UNMEASURED only for legacy missing-snapshot `Ineligible`: the single source-based attempt found no historical v1 DDL or host/descendant history bytes; `migrate_scope_v1` consumes an existing schema-version-1 database and creates v2 scope tables, while normal open creates v3. No fresh-v3 downgrade or retry was used. Modern missing-initial/missing-copy `CorruptEvidence` contrasts remain measured. G13, G16, G20, G21, G22/G23, and G24 retain their previous PASS dispositions pending parent verification.

Round83 lane results and exact logs are recorded in the Round83 section of `b1-w3-acceptance-evidence.md`. Loopback-bound scope CLI and GitHub polling cases were denied by the sandbox (`Operation not permitted`) and require parent rerun. No commit or push.
