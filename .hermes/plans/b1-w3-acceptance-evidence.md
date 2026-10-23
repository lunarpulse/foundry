# B1 W3 Gate0 evidence inventory — parent disposition

Authorization: Discord1561905904042115093. Gate0 only; no implementation approval or W3 acceptance.

## Provenance and parent checks

- Full astra stdout: `/home/cosmo/.hermes/cache/scratch/b1-w3-gate0-astra.log`; exit0 is process completion, verdict REFINE.
- Parent compared astra start/end porcelain snapshots: unchanged. This is status verification, not full filesystem content identity. git diff --check passed. No new tests run by parent during this inventory.
- Parent programmatically reconciled all 46 effective matrix names with 46 individually enumerated report entries: no missing/extra/duplicate rows. Astra classification: 1 PASS,45 GAP,0 BLOCKED,0 exclusions. These are whole-row evidence classifications, NOT 45 newly failing tests or a test-pass percentage. Partial proofs remain valuable. Per-row source/log conclusions below remain astra findings until parent validation/implementation gates close each row.
- Parent independently read frozen C§4:1199-1242: stored-set validator includes confined bytes for replay/lookup/history/approval, authority before disclosure, corruption before expectation conflict. Therefore byte integrity is already frozen policy, not a new discretionary requirement.
- Parent independently read result.rs:770-879: run_commit compared directly to terminal host result, continuation classified via followup reservation, artifacts selected only current journal. Contrast C§4:1211 initial journal input vs generation zero and final head vs final chain. These require concrete scoped repair, not validator deletion.
- Parent independently read V3_SCHEMA result.rs:239-256: result FK references host acceptance rather than authority; approval sidebinding additionally UNIQUEs result/binding/tuple. Need frozen DDL conformance and existing-v3 treatment decision before schema edits. No silent repair authorized.

## Disposition

Gate0 inventory is complete as an enumerated candidate ledger, but all-bundle implementation handoff is REFINE. Do not start luna production edits from Gate0 authority. Prepare bounded Gate1 whitelist/spec for approval; in parallel document (not implement) existing-v3 schema disposition and operation-local COMMIT/lookup-I/O fault semantics. No new contract choices are needed for frozen continuation counts or stored-set byte validation. W4/liveGitHub/operatingDB/commit/push remain excluded.

## Astra full report (read-only findings, not parent-certified acceptance)

**REFINE. The fixed acceptance denominator is 46 effective rows: 1 evidence-supported PASS, 45 GAP, 0 BLOCKED, 0 excluded.** This is a Gate0 inventory, not W3 acceptance.

I read the complete approved plan, frozen contract, and recovery matrix. HEAD is `36e994c9897ffa42e56e294a8682009e854777f9`. Expanded untracked-file status matches the luna67 snapshot except for the new approved plan. That verifies status compatibility, not content identity. I made no writes and ran no tests, builds, probes, database operations, workflows, subagents, or external calls. Graph report/wiki absence remains the parent-established fact; no graph verification is claimed.

The governing contract is the [freeze and precedence record](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:3). Historical “DRAFT,” “REFINE,” and pending-freeze passages do not reopen already-frozen choices.

**Evidence and location keys used below**

Every abbreviated location below refers to this exact file; suffixes are source line numbers.

| Key | Actual file |
|---|---|
| R | [ledger product/result.rs](/home/cosmo/foundry/crates/foundry-ledger/src/product/result.rs) |
| V | [ledger product/review.rs](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs) |
| P | [ledger product.rs](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs) |
| H | [line/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs) |
| HX | [line/tests/b1_product_result_processes.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_result_processes.rs) |
| F | [shared product_result_fixture.rs](/home/cosmo/foundry/crates/foundry-line/tests/support/product_result_fixture.rs) |
| L | [ledger/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_product_results.rs) |
| S | [serve/tests/b1_product_artifacts.rs](/home/cosmo/foundry/crates/foundry-serve/tests/b1_product_artifacts.rs) |
| SH | [serve/tests/b1_auth_http.rs](/home/cosmo/foundry/crates/foundry-serve/tests/b1_auth_http.rs) |
| C | [foundry/tests/b1_approve_cli.rs](/home/cosmo/foundry/crates/foundry/tests/b1_approve_cli.rs) |
| SC | [foundry/tests/b1_scope_cli.rs](/home/cosmo/foundry/crates/foundry/tests/b1_scope_cli.rs) |
| SR | [line/tests/b1_service_review.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_service_review.rs) |
| RS | [line/tests/b1_product_restore.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_restore.rs) |

Parent evidence:

| Log key | Executed parent evidence | Result and limits |
|---|---|---|
| H67 | [round67-shared-host](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-shared-host.log:14) | 1 passed, exit 0. Current shared fixture, initial automatic success only. |
| C67 | [round67-cli-enabled](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-cli-enabled.log:284) | 2 passed, exit 0. Genuine CLI approval plus default rejection. |
| D67 | [round67-cli-default](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-cli-default.log:278) | 9 passed across five targets, exit 0. |
| S67 | [round67-http-enabled](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-http-enabled.log:107) | 3 passed, exit 0. `b1_auth_http`, including genuine approval through `Router::oneshot`; not result-read/socket acceptance. |
| K67 | [round67-core](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-core.log:359) | 401 passed, exit 0. Default features; H/HX required-feature targets are not covered. |
| W67 | [round67-watch](/home/cosmo/.hermes/cache/scratch/b1-parent-round67-watch.log:110) | 17 passed, exit 0. Regression evidence, not additional result rows. |
| X63 | [round63-h-hx](/home/cosmo/.hermes/cache/scratch/b1-parent-round63-h-hx.log:13) | HX 5 and H 1 passed, exit 0. Real recorded killpoint tokens, but older shared fixture. |
| M64 | [round64-migration](/home/cosmo/.hermes/cache/scratch/b1-parent-round64-migration.log:2) | Results 3 and scope 4 passed, exit 0; these do not establish faithful-v2 migration coverage. |

All six round67 lanes report zero failed and ignored. Their totals are not independent acceptance counts.

Round62 result-crash evidence is narrower than X63. Round65 HTTP results are superseded as current execution evidence by S67. [Round65 CLI](/home/cosmo/.hermes/cache/scratch/b1-parent-round65-cli.log:273) failed, exit 101; C67 establishes the subsequent genuine-fixture repair, not retrospective success.

The [luna67 fixture diff](/home/cosmo/.hermes/cache/scratch/b1-luna67.log:53881) preserves the existing wrapper while introducing the caller-owned bootstrap seam. H67/S67 exercise that current wrapper. **HX has not been rerun in round67**, so X63 remains historical partial proof requiring renewed compatibility validation. Default ledger child tests can return immediately without a manifest; their `ok` entries are not crash evidence.

**Confirmed implementation gaps**

1. **Resume fails a frozen-contract predicate.** R:789 requires `run_commit == host.result_commit`. Frozen [B’s exact script](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:821) and [C§4](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:1198) require journal input `C0` to equal generation zero, while terminal head `C2` equals the final generation. V:1306 updates terminal result JSON without replacing the original journal input commit. R:976 already compares generation zero with that journal input. The repair must preserve both correct comparisons, not simply remove validation.

2. **CommitPass classification and lineage are incomplete.** R:831 identifies continuation through a reservation whose *follow-up* equals the result run. Direct CommitPass resolves origin 7 with unused reservation 8; it is consequently treated as initial completion and required to have `completed` work/backlog, although the frozen branch retains `claimed`. Scenario B also requires terminal H2 evidence in addition to H1 Resume ancestry. R:1054 always writes `accepted_holds: Vec::new()`, and R:873 collects artifacts only from the current journal run. That cannot produce the required 14-artifact ancestry.

3. **Authority capture stores the wrong revision column.** V:595 selects `g.grant_revision, i.generation, i.revision, s.revision`; the fourth value becomes `connector_revision` at V:622. Here `s` is `product_services_v1`; the connector revision belongs to `ps.revision`. Equal revision-1 fixtures conceal this. Resume copying at V:1843 also does not require the copied-row count to be one or independently establish the permitted legacy-unattested exception.

4. **Stored-result integrity is substantially narrower than frozen C§4.** R:1963 verifies canonical serialization, several digest/identity projections, authority/work fields, terminal-result pair, anchor existence, tuple fields, and receipt projections. It does **not** validate the complete acceptance/progress ancestry, artifact set/provenance/bytes, or the complete S1 anchor relationship. It has no root-handle parameter. It also lacks explicit constant checks for payload/receipt version and receipt kind. Canonical reserialization alone does not enforce those constants.

5. **Service replay/lookup lacks requested-original-source binding before returning a stored result.** R:655 and R:1127 validate the service context’s own grant. The [service validator](/home/cosmo/foundry/crates/foundry-ledger/src/product_service.rs:468) has no requested order/result argument. R:677 can return stored evidence before the later new-result source comparison; lookup has the same separation. A currently authorized service for another source therefore needs an explicit negative baseline. This is a source-level authorization gap, not an executed exploit claim.

6. **Tuple classification and error precedence are incomplete.** R:1989 returns `Conflict` for any census membership without first distinguishing census-without-tuple or census-with-binding corruption. A valid different-activation binding encountered by coordinates becomes `CorruptEvidence` rather than validated-alias `Conflict`. R:2147 compares expected tuple digest before validating the stored winner. Several query errors become `Unauthorized`, `Ineligible`, or generic invariants instead of preserving storage errors.

7. **Migration validates too little.** R:508 uses a transaction and final version CAS, but source-v2 checks largely establish table presence and tuple column names, not faithful schema compatibility and S1/approval relationships. R:270 checks selected columns and trigger names, not required indexes/constraint definitions or trigger behavior. L:52 constructs v2 by stripping a fresh v3 database; it is not the required independently faithful historical-v2 fixture. The scope migration test likewise starts from current `ProductStore::open`.

8. **Existing-v3 DDL needs explicit disposition.** R’s result-binding FK points to `product_review_host_accept_v1`; frozen C§3 says the result binding references the authority numeric host key. The approval-side table also adds a unique result triple beyond the stated operation-key ownership. These need a precise schema-conformance decision before changing existing v3 databases. Normal open must not silently repair them.

**Root identity and required integrity scope**

These are three separate claims:

| Claim | Current evidence |
|---|---|
| Persisted root path/device/inode survives CLI approval | Supported by C:243–257 and C:513–520 under C67. |
| CLI approval opens/configures the original root | Not supported; [main.rs:883](/home/cosmo/foundry/crates/foundry/src/main.rs:883) opens then prepares/approves without configuring it. P:789 initializes both root fields to `None`. |
| CLI approval rejects changed files/root | Not tested, and current shared approval validation performs no file reads. |

The serve startup reads `FOUNDRY_PRODUCT_ARTIFACT_ROOT` at [main.rs:1796](/home/cosmo/foundry/crates/foundry/src/main.rs:1796). Merely setting that variable in the CLI test does not make `product_approve_cmd` use it.

P:1118 configures a pinned root and compares persisted identity. It also creates directories and can initialize missing root records; it is not intrinsically a read-only existing-root validator. Consumer setup must not silently adopt missing/replacement identity.

**No new policy choice is needed about whether metadata, lookup, replay, and approval require bytes.** Frozen C§4 explicitly includes confined bytes in the shared stored-set validator, used by those consumers; PT-10 also rejects metadata-first partial success. The current artifact endpoint validates only the selected artifact’s bytes after the narrower shared validator. The required scope is the complete bound evidence set, including inherited artifacts.

Preserve this ordering: current caller authorization → stored-set integrity → expected-digest/alias conflict. Missing configured root, corruption, and genuinely unavailable I/O need distinct internal classification with existing external screening. V:3058 currently collapses filesystem failures into an invariant, and R:1329 maps all resolver failures to `CorruptEvidence`; that cannot demonstrate genuine I/O → `Storage`/503.

**Complete effective-row inventory**

“GAP/partial” means the cited execution proves only the stated subset. “Absent” means no implementing test/assertion was found in the inspected actual targets. Gate ownership is proposed, not implementation authorization.

1. **`standalone_store_still_rejects_product_v3` — GAP/partial; Gate3.**  
   Actual tests: [exec `standalone_tuple_opener_refuses_product_contract_before_schema_changes`](/home/cosmo/foundry/crates/foundry-exec/tests/tuple_sqlite.rs:146) asserts refusal and absent tuple schema; RS:1531 tests standalone host refusal. K67 executes both targets. Missing: fresh **and faithfully migrated v3** baselines, complete before/after schema/row preservation, and corresponding host/tuple assertions. Existing contract-marker tests cannot alone close the row.

2. **`genuine_host_result_finalizes_without_emit_or_publish` — GAP/partial; Gate1, Gate2 publication checks.**  
   H:14 `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor`, assertions H:140–160 and H:325–340; H67. Proves `4/0/1`, stages/artifacts/sessions `4/4/3`, T/R `1/1`, equal replay, no promotion table. C:235 adds execution-row observations. Missing complete acceptance/result/run census, claim/write deltas, and explicit sink/Publisher/publish-start coverage across first finalization and replay.

3. **`result_uses_persisted_numeric_run_and_activation` — GAP/partial; Gate1.**  
   F constructs run7/activation3; H:140–142 asserts run7/activation3/generation0 under H67. R:750 reconstructs journal identity. Missing genuine **colon-containing order** and persisted journal/tuple mapping assertions. L’s `"colon:order"` digest fixture is not a host mapping test.

4. **`activation_and_lifecycle_aliases_cannot_share_tuple_key` — GAP; Gate1.**  
   No implementing test. V:556 rejects numeric admission aliases; R:1963 handles stored lookup. Missing independent activation/lifecycle alias attempts against a proven winner, correct corruption-before-conflict precedence, T/R unchanged, and no execution. Current alias classification is defective as described above.

5. **`result_coordinates_reject_overflow_and_wrong_attempt` — GAP; Gate1.**  
   No matching coordinate tests in L/H. R:2160 checks public expectation width, but persisted decoding and V admission still contain casts. Missing `i64::MAX+1`, negative stored coordinates, actual replay-attempt mismatch, and zero tuple/result/approval effects.

6. **`tuple_and_result_binding_commit_as_one_set` — GAP/partial; Gate3.**  
   H:97–125 injects tuple/binding insert errors and asserts T/R `0/0`, stages4/sessions3; H67. HX:522 checks binding-insert crash rollback and preserved stage/artifact/session counts; X63. Missing tuple-insert child execution, complete original host/artifact preservation assertions, and current-fixture HX execution. R:1103 already exposes the unused tuple-insert killpoint.

7. **`nonempty_generation_digest_matches_exec_and_preserves_legacy` — GAP/partial; Gate1.**  
   L:90 `typed_tuple_digest_uses_nonempty_exec_generation_encoding`, K67:426 and M64. Asserts one two-element vector’s fixed digest and mutation sensitivity. Missing exact single-chain vector, explicit difference from historical `Value` encoding, and historical digest/byte preservation. R:1012 uses `ArtifactTuple::digest()` correctly.

8. **`result_payload_and_local_receipt_have_exact_v1_bytes` — GAP; Gate1.**  
   R:84–193 declares typed structures; R:2020 and R:2095 perform canonical reserialization. No exact-wire test. Missing outer/nested field order, raw embedded string bytes, constants, duplicate/unknown/missing/noncanonical cases, and stable persisted receipt vectors. H’s parsed payload assertions are not byte-level proof.

9. **`emit_key_and_promotion_tuple_are_not_interchangeable` — GAP; Gate1.**  
   No test passes actual serialized `EmitReceipt` and `PromotionReceipt` into local-result validation/approval. R:177 defines a distinct receipt, but no executed rejection/fallback assertion exists.

10. **`finalizer_rejects_empty_foreign_and_unbacked_generation_chains` — GAP; Gate1.**  
    R:785 and R:970 contain chain checks; no independent genuine G-pre mutation suite. Missing empty/root/index/final/fix-log/produced-commit cases with exact errors and unchanged T/R/host. R currently checks fix-path membership without proving that stage’s `produced_commit` equals the generation commit.

11. **`accepted_resume_binds_three_generations_and_fourteen_artifacts` — GAP; Gate1.**  
    No frozen A fixture in H/F; F:87 is a passing executor. RS:375 exercises older continuation behavior, not strict result finalization with the frozen script. R:789, R:873, and R:1054 prevent required proof. Missing exact A chain, 14 artifacts, eight sessions, `14/3/1`, completed H1 progress, inherited ordering/deduplication, and strict finalization.

12. **`commitpass_resolves_origin_without_materializing_reserved_run` — GAP; Gate1.**  
    RS:918 `accepted_commit_pass_finalizes_without_dispatching_another_stage`, K67:481, proves older host terminal resolution. It does not produce a strict result for B/direct coordinates. Missing B origin8/reservation9, direct origin7/reservation8, retained hold history, accepted lineage, successor absence, and allocator delta zero. V:1897 is the actual terminal producer; R:831 misclassifies direct CommitPass.

13. **`result_location_must_resolve_registered_terminal_evidence` — GAP; Gate1.**  
    R:905 and R:2182 resolve a registered relative path. No G-pre path-negative suite. Missing invalid terminal path, `#run` fallback, foreign-stage, external/unregistered path cases and T/R `0/0` without caller-path adoption.

14. **`held_failed_and_materialized_continuation_are_not_final_results` — GAP; Gate1.**  
    R:785 checks status/exit/held; V:1795 creates the resuming copy. RS contains held/failure host tests under K67, but no three independent strict-finalizer baselines. Missing exact `Ineligible`, T/R zero, and preserved history/session assertions for each branch.

15. **`queued_success_is_not_silently_made_approvable` — GAP/partial; Gate1.**  
    H67 proves Waiting success; S:123 has a queued stage-artifact baseline, but never finalizes it. R:689 rejects non-Waiting. Missing paired genuine Queued/Waiting finalization tests and explicit finalizer S1 delta zero.

16. **`authority_is_captured_at_activation_and_copied_on_resume` — GAP; Gate1.**  
    Production sites V:595/622 and V:1843 exist; H67 implicitly uses initial capture. Missing intake/claim zero snapshots, atomic activation insertion, exact eight-member Resume copy, CommitPass zero-copy, rollback, and update/delete/REPLACE rejection. The connector-revision projection and unchecked copy count require repair.

17. **`legacy_host_census_distinguishes_missing_authority_from_corruption` — GAP; Gate1 with Gate3 historical fixture.**  
    R:843 and R:2280 classify authority absence; no faithful historical-host test. Recursive ancestry presently counts roots without fully validating accepted reservations, activation, cycles, and ambiguity. Missing legacy member/descendant `Ineligible`, new initial/missing-copy `CorruptEvidence`, and no-backfill/history preservation assertions.

18. **`completed_initial_claim_finalizes_after_worker_expiry` — GAP/partial; Gate1.**  
    H67 proves initial finalization/replay and `4/4/3`; R:803 reads but ignores worker lease. F uses `FixtureClock(now)`, not frozen six-minute finalization. Missing expired completed lease with live service, unchanged claim/lease, and exact first/replay deltas.

19. **`pending_result_requires_original_authority_revisions` — GAP; Gate1.**  
    R:858 compares live service/credential/grant/source revisions; SR:790 tests continuation regrant refusal under K67. No independent pending initial/A/B strict-finalization suite. Missing service/credential/source rotation, revoke→regrant, lineage-specific human changes, unrelated-human initial-success control, and preservation of finalized bytes.

20. **`revoke_after_stage_start_preserves_result_but_blocks_finalization` — GAP/partial; Gate1/HX.**  
    SR:1113 `service_revoke_committed_after_stage_start_preserves_exact_result_and_denies_next_stage`, K67:503, uses `STAGE_START_COMMITTED` and asserts retained stage evidence and next-stage denial. Missing strict-finalization assertion on the same genuine baseline and complete matrix deltas. Preserve this existing process proof.

21. **`revoke_before_stage_start_has_zero_executor_calls` — GAP/partial; Gate1/HX.**  
    SR:1027 and SR:1287 test source/service mutation before stage start; K67:502–504. They assert absent call evidence and preserved continuation state. Missing complete stage/session/artifact/T/R zero-delta assertions on the required strict-result fixture and corresponding finalization refusal.

22. **`reassigned_owner_cannot_store_or_finalize_old_result` — GAP/partial; Gate1/HX.**  
    SR:1414–1468 covers stale ordinary completion and unchanged replacement fence under K67. V:2401 provides the ordinary owner fence; V:1897 CommitPass persistence has different checks. Missing both strict-finalization refusals, especially retained CommitPass terminal evidence after reassignment, plus same-owner/new-epoch ABA.

23. **`initial_redrive_remains_unsupported` — GAP; Gate1.**  
    V:1076 explicitly returns unsupported. No frozen failure-script test in H/F. Missing executed `5/0/1`, four completed stages/artifacts, three sessions, budget4, failure1, no numeric host result/T/R, and no persisted fix generation. Static unsupported code is not the requested acceptance proof.

24. **`continuation_revocation_distinguishes_restore_from_execution` — GAP/partial; Gate1/HX.**  
    SR:949/1113 demonstrate preservation and next-stage denial; K67. Host restoration precedes authorization in [review_host.rs:1726](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:1726). Missing frozen cumulative `5/1/1`, stages/artifacts5, sessions4, budget5, recovery-required progress, and strict T/R zero.

25. **`interleaved_order_evidence_pairs_own_sequence_and_hash` — PASS; Gate1 regression ownership.**  
    H:14 admits a foreign order before finalization; H:161–173 asserts selected sequence4, that row’s hash, and inequality with global head. H67:14 records execution success. R:2231 verifies the entire chain and selects the hash from the same order/sequence row. No missing row assertion identified.

26. **`stored_result_replay_survives_approval_and_original_authority_rotation` — GAP/partial; Gate2.**  
    H:319–339 under H67 proves replay after approval and reopened approval receipt equality. Missing original-authority rotation with a currently authorized caller, expired-caller refusal, complete stored corruption cases, and historical lookup beyond current Waiting/Publishing restrictions. R:1135 currently applies live state/attempt restrictions before lookup.

27. **`two_process_exact_replay_returns_one_identical_receipt` — GAP; Gate3.**  
    HX:883 `separate_processes_replaying_same_key_return_the_exact_committed_receipt`, X63:27, is **approval replay**, not competing result finalizers. Missing two G-pre finalizer processes, both commit orders, exact local-result wire equality, T/R1/1, and interaction/write deltas.

28. **`two_process_changed_expectation_never_replaces_winner` — GAP; Gate3.**  
    HX:781 is approval competition; H:175 tests only wrong digest after an existing result. No result-finalizer process race. Missing wrong-first `InvalidEvidence`, correct-first then wrong `Conflict`, both controlled orders, unchanged winner bytes, and only one T/R set.

29. **`kill_before_and_after_result_commit_recovers_without_dispatch` — GAP/partial; Gate3.**  
    HX:522 under X63:23–25 reaches binding-insert and postcommit-before-response tokens, asserts precommit0/0/postcommit1/1, lookup, retry, and exact receipt bytes. Missing executed `after_tuple_insert` and current-fixture replay of all windows. No COMMIT-I/O claim follows from these kills.

30. **`ambiguous_commit_uses_lookup_and_never_treats_io_error_as_absence` — GAP/partial; Gate3.**  
    HX:566 covers response loss via postcommit SIGKILL; R:1108 returns `CommitUncertain` on commit failure. HX:963 kills an approval process while COMMIT is lock-blocked. Missing distinct actual/injected COMMIT-I/O and recovery lookup-I/O failures at their operation sites, exact uncertainty/storage outcomes, and no blind redispatch. Lock blockage plus SIGKILL is not an I/O failure.

31. **`approval_replay_and_ambiguous_commit_validate_result_side_binding` — GAP/partial; Gate3 with Gate2 validator.**  
    P:3426 and P:3633 validate sidebinding on ordinary replay and ambiguous recovery. HX:631 exercises clean postcommit replay under X63. Missing missing/mismatch/corrupt-sidebinding negatives from independent G-final baselines, actual ambiguous branch execution, lookup-I/O failure, and complete participant validation.

32. **`bound_result_artifact_reads_require_original_source_grant` — GAP/partial; Gate2.**  
    S:123 under K67:690 proves stage-artifact authorization/bytes; SH:317 under S67 proves approval. Neither calls the result metadata/bound-artifact routes. Missing G-final result-route 200/401/404/503, association checks, IDs/digests/no-store, bytes and secret/path non-disclosure. Actual consumers are R:1164/1235 and [artifact_http.rs:36](/home/cosmo/foundry/crates/foundry-serve/src/artifact_http.rs:36).

33. **`retired_source_history_remains_readable_without_new_source_inheritance` — GAP/partial; Gate2.**  
    S:433–491 under K67 proves old-source stage-artifact reads and new-source-only refusal. Missing G-final result metadata/bound-artifact equivalents and explicit noninheritance of new approval/publication authority.

34. **`each_result_integrity_mutation_starts_from_verified_baseline` — GAP/partial; Gate2.**  
    S:500–730 tests restored stage-artifact bytes, missing file, symlink, path, SHA, provenance. K67 executes it. Missing genuine-result tuple/binding/host/acceptance/progress variants and the metadata/lookup/replay/approval cross-product. Existing S deliberately drops an update guard without first asserting that guard’s rejection in that case; preserve and strengthen baseline discipline.

35. **`result_reads_preserve_pinned_root_and_no_follow_guards` — GAP/partial; Gate2.**  
    V:3268/3296/3329 resolver tests, K67:380–382, prove deterministic leaf, ancestor, and pinned-root behavior. S:733 checks replacement-root configure refusal. Missing G-final result consumers, FIFO/device cases, unsupported-platform evidence, and no partial/outside bytes across the complete result read.

36. **`product_result_read_and_approval_work_over_authenticated_loopback_server` — GAP; Gate2.**  
    SC:599 under D67:299 is a real source-isolation server test, without G-final result metadata/artifact/approval flow. C67 is a CLI process; S67 is `oneshot`. Missing the actual socket producer→login→metadata→artifact→approval chain on the original DB/root, exact bytes/statuses, and Publisher-zero proof.

37. **`approval_consumes_exact_local_result_in_atomic_transaction` — GAP/partial; Gate2.**  
    H:280–307, SH:433–503, and C:499–543 under H67/S67/C67 prove genuine approval participants, exact sidebinding, replay, and unchanged result evidence. Missing explicit no-publication/publish-start assertions across these consumers and contract-complete stored-set validation. Existing successful approval evidence remains valuable but is not whole-row PASS.

38. **`approval_rejects_changed_result_without_consumption` — GAP/partial; Gate2.**  
    H:203–240 tests wrong tuple digest with zero publishing/token/outcome; C:382–502 tests mismatched retry, revoked/foreign sessions and final counts. H67/C67. Missing independent foreign/stale source, tuple, attempt, generation, digest variants before consumption, per-case grant/token/sidebinding deltas, and restored positive controls.

39. **`approval_result_binding_rolls_back_with_each_existing_participant` — GAP/partial; Gate3.**  
    H:244–279 injects S1/token/grant/outcome/sidebinding errors; H67. HX:631 under X63:16–22 records all six participant/postcommit kill tokens and asserts absent/exact-one sets and replay. Missing current-fixture HX rerun and preservation of the full original orchestration map after validator/root changes. No current acceptance inferred from historical tokens.

40. **`v2_to_v3_migration_is_additive_atomic_and_old_binary_fenced` — GAP/partial; Gate3.**  
    L:29/52 under K67:424–425/M64 proves fresh census counts, basic read-only validation, stripped-v3 migration, preserved generation JSON, repeated migration. [b1_scope.rs:541](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_scope.rs:541) preserves selected scope history. Missing faithful V2 DDL/history, populated host/outcome census, DDL/population/CAS failure injection, complete rollback, guard variants, schema-definition validation, and old-product-binary fencing.

41. **`legacy_tuple_membership_distinguishes_unbound_history_from_corruption` — GAP/partial; Gate3.**  
    L:52 preserves one unbound historical tuple and census membership; K67/M64. No authenticated lookup/finalization classification suite. Missing all five combinations: legacy unbound; damaged modern missing binding; census+binding; census without tuple; binding without tuple. R:1989 mishandles several classifications. Neither error nor corruption may become `None`.

42. **`legacy_receipts_remain_history_but_cannot_authorize_new_results` — GAP; Gate3.**  
    P:3445 contains cutoff handling; H:310 rejects publication of a new strict approval. No executed faithful historical receipt boundary suite. Missing cutoff/cutoff+1, prepared-before/committed-after strictness, complete old receipt/outcome/consumption/S1 preservation, and scoped **and unscoped** legacy publication zero-call tests.

43. **`result_error_precedence_does_not_reveal_foreign_evidence` — GAP; Gate2 with Gate3 I/O.**  
    No combined-error matrix. H:175 proves only valid-winner/wrong-digest conflict. R:1963/2147 and service authorization have the ordering gaps described above. Missing unauthorized+corrupt, stale+missing file, wrong expectation+corrupt winner, expired worker+valid winner, true read-I/O, uniform screened responses, and zero mutation.

44. **`p2_preparation_walks_s1_without_result_seeding` — GAP/partial; Gate1.**  
    F:375 calls the actual R:411 adapter; H67 reaches subsequent genuine finalization. R uses real domain transitions and conditional backlog update. Missing assertions immediately before claim for +3 S1/+3 receipts, zero pending intents/claims/results/tuples/promotions, and independent stale/repeated/invalid admission/source/conditional-update rollback cases.

45. **`p2_support_has_no_product_http_or_cli_entrypoint` — GAP/partial; Gate3.**  
    Four manifests have default-off forwarding; line H/HX have `required-features`; type/method/exports are gated. Source call-site search found no normal product handler/startup call into preparation. Missing retained default **type and method** compile-negative logs, enabled compile positives, and enabled/unified route/help/deserializer negative tests. Normal default test success does not prove API absence.

46. **`w3_paths_never_invoke_publisher_or_create_publish_start` — GAP/partial; Gate2, with Gate1/Gate3 scenarios.**  
    RS:218 tests a forged receipt against a counting Publisher and asserts zero calls under K67. H:310 rejects the helper for a genuine strict approval. Missing genuine-result composition through `ProductProjection::publish_approved`, sink/Publisher spies and publish-start/publication deltas across success/failure/retry/reopen, plus scoped/unscoped historical guards. S1 `publishing` is not evidence of remote publication.

**Six named legacy-test responsibilities remain separate from the 46-row denominator**

1. **`separate_process_hold_acceptance_same_and_different_keys`** remains P:5842, using `setup_hold_only` at P:5709. K67:375 executes it. Preserve the hold-specific competition assertions and barriers.

2. **`sigkill_at_each_real_hold_acceptance_statement_is_all_or_none`** remains P:5748. K67:376 executes it. Preserve its hold-only participant windows; do not replace it with result/approval crash totals.

3. **`sigkill_at_every_uncommitted_insert_rolls_back_and_reopens_writable`** is replaced by HX:631 plus ledger `cfg(test)` approval child and P’s real operation hooks. X63 supplies participant tokens. Retain S1/grant/token/outcome windows, add sidebinding/postcommit, and rerun against the current fixture. The old function’s absence alone is not coverage.

4. **`product_approval_atomically_writes_s1_and_consumption_then_replays_exact_receipt`** maps to H:183–340 and HX:631/781/883/963. H preserves reservation reclaim, stale-proof rejection, five fault rollbacks, successful participants, exact replay and reopen. HX preserves process races/crash windows historically. Full current mapping remains GAP until HX rerun and integrity negatives are added.

5. **The restore test’s `PublishOutcome::Confirmed`/publisher1 block** is explicitly superseded by frozen D8/C§5, not by convenience. RS:285 says its replacement is H. H currently calls only `authorize_product_publish`; it does not preserve the entire genuine-result `ProductProjection` composition, observed-call, catch-up/reopen and receipt obligations. Add those assertions in H/HX; preserve the unrelated default restore suite.

6. **The strict approve CLI positive** now resides at C:188, explicitly feature-gated and executed by C67. C:557 preserves default bootstrap/login/unbound rejection; D67 runs all five default targets. Retain all existing auth/invalid-input/receipt/secret assertions. Root-use and integrity additions remain necessary; persisted root identity is not their replacement.

Only [C§7’s explicit supersessions](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:1336) justify replacing old claims: generic ancestry/reservation/lease rows consolidate into exact scenarios; stale-owner behavior splits ordinary completion from CommitPass; restore differs from execute; wrong expectation differs before/after a winner; retained hold history is allowed; journal input is not terminal head. **None of the current 46 rows is obsolete or excluded.**

**Concrete Gate1 implementation requirements**

The public signatures already exist and should remain:

```rust
finalize_product_review_result(
    &mut self,
    service: &ProductServiceContext,
    expected: &ProductResultExpectation,
) -> Result<ProductResultInfo, ProductResultError>

lookup_product_review_result(
    &mut self,
    service: &ProductServiceContext,
    expected: &ProductResultExpectation,
) -> Result<Option<ProductResultInfo>, ProductResultError>
```

A private completed-result validator must distinguish initial, Resume, and terminal CommitPass from proven acceptance/reservation lineage—not a “has follow-up reservation” boolean.

Required predicates:

- Checked persisted coordinates, actual replay attempt, lifecycle+1, exact intent/key/journal/epoch.
- Current caller DriveReview on the requested original source before existing-result disclosure.
- New finalization: exact original service, credential, grant and source revisions; live service/credential; no worker lease renewal/check.
- Initial claim/backlog `completed`; Resume/CommitPass `claimed`; original delivery/owner/epoch/fence/payload equality.
- Journal input equals generation zero; terminal head equals final generation; original base remains exact.
- Each fix/replan generation has matching completed stage, artifact and `produced_commit`; verification output inspects the appropriate commit.
- Accepted operations ordered root-to-leaf plus terminal CommitPass; exact persisted receipt strings, frozen snapshot digest, completed progress/no recovery reason, reservation, owner=`operation:epoch:fence`.
- Progress epoch is not host/work/global recovery epoch. Fixture revision3 is an assertion, not a production limit.
- Exact inherited/current artifact set, causal run order then numeric sequence, no duplicate IDs or re-registration.

The required scripted executor must implement these frozen outputs:

| Run | Stages | Output |
|---|---|---|
| 7 | edge, verif, claims | Successful lenses at C0 |
| 7 | triage | `intent_gap`, findings true, inspected C0 |
| H1 | Resume `replan_full` | Existing allocator issues run8 |
| 8 | replan_full-g1 | Input/base/artifact C0; produces C1; `plan_full == "[]"` |
| 8 | edge, verif, claims | Successful lenses at C1 |
| 8 | triage | `patch`, findings true, inspected C1 |
| 8 | fixdie-g2 | Input/base/artifact C1; produces C2; one triage findings artifact |
| 8 | edge, verif, claims | Successful lenses at C2 |
| 8 | triage | A: pass/no findings; B: intent_gap/findings; inspected C2 |

Use the frozen `p2:A`/`p2:B`, lifecycle1/attempt1/run7/activation3/host-epoch1/generation0; B/C0/C1/C2 are the frozen 40-character SHA strings. Finalization clock is six minutes; service/credential stay live. Assert A/B `14/3/1`, artifacts14, sessions8, budget14; direct CommitPass `4/0/1`, artifacts4/sessions3. Assert exact frozen paths and session identities.

Hold IDs/expectations come from `list_product_review_holds`; acceptance uses the actual P:2730 API. P:3042 allocates numeric successor from existing origins/reservations/acceptances. Do not substitute SQLite reservation rowid. H1 reserves8; B’s H2 reserves9 while final result remains run8; direct result remains run7. No additional allocator or successor materialization.

**Minimal proposed file whitelist for the three bundles**

This is a proposed scoped edit list, not permission to implement.

| Bundle | Production changes justified by inspected code | Test changes |
|---|---|---|
| Gate1 | `crates/foundry-ledger/src/product/result.rs`; `crates/foundry-ledger/src/product/review.rs` | H, HX, F; L for pure wire/width/digest cases |
| Gate2 | `crates/foundry-ledger/src/product/result.rs`; `crates/foundry-ledger/src/product.rs`; `crates/foundry-ledger/src/product/review.rs`; `crates/foundry/src/main.rs` | H; S; SH; C; SC |
| Gate3 | `crates/foundry-ledger/src/product/result.rs`; `crates/foundry-ledger/src/product.rs`, narrowly including `cfg(test)` operation faults/children | HX, F, L, H for producer-dependent historical classification; isolated external compile-probe files |

No demonstrated need presently requires edits to the host algorithm, frozen pack, exec tuple/digest implementation, public DTO exports, dependency graph, manifests, lockfile, or domain transitions. The four feature declarations and both H/HX target declarations already exist.

`auth_http.rs` owns shared state but is not the approval implementation; [approval_service.rs:91](/home/cosmo/foundry/crates/foundry-serve/src/approval_service.rs:91) delegates to ProductStore. `artifact_http.rs` already screens result errors. Neither needs speculative inclusion in the production whitelist. Add a path only if a concrete required behavior cannot be implemented through the listed storage/root changes.

Gate2 needs a shared private validator receiving the transaction, expected store identity and pinned root context. It must serve metadata, selected-artifact read, lookup, finalizer replay, new approval, ordinary approval replay and ambiguous recovery. Preserve public signatures and strict receipt bytes. Reopened consumers must configure the same original root; current H:329 and SH’s post-reopen approval path will need corresponding fixture setup when validation is strengthened.

No new schema/version is assumed. Correctness improvements must not rewrite existing immutable authority/result/receipt/history records. Already-inconsistent evidence must fail closed; any migration or repair policy needs separate explicit disposition.

**Gate3 harness and command prerequisites**

Existing child transport is usable but not complete:

- HX manifest contains exact config, original roots/device/inode, source identities/revisions, fixture-issued bearer paths, clock, host coordinates/digest, human session, and full approval intent.
- Children authenticate the same fixture-issued service bearer afresh; do not serialize `ProductServiceContext`, mint replacement credentials, or fabricate reservation proofs.
- R:1442 still hardcodes initial host run7/activation3 and no hold operation. Extend validation for scenario-specific run8/lineage manifests without weakening exact identity checks.
- Current kill protocol is flushed stdout token → blocking stdin → owned-child SIGKILL → checked exit signal → reopen. Preserve deadlines and failure on absent/wrong tokens.
- Current approval races use ready/release files. New result races need their own exact finalizer child and correct/wrong expectation manifests; approval races cannot substitute.
- Keep fixture/child controls under `cfg(test)`, not merely `p2-test-support`.

Three distinct recovery cases are required:

1. **Response loss:** commit actually succeeds, response is withheld; fresh lookup returns exact stored wire.
2. **COMMIT-I/O:** an operation-local test fault produces a real/injected SQLite I/O error at the commit call path; exercise the production uncertainty/recovery branch and inspect durable state.
3. **Lookup-I/O:** fault the actual recovery read, assert `Storage`/specified uncertainty, never `None` or fabricated success.

The present lock-blocked SIGKILL test covers none of the latter two. Fault injection must not return a fabricated successful finalizer/approval result. A narrowly scoped `cfg(test)` commit/read wrapper is feasible without a new dependency, but its exact fault mode and expected durable outcome must be fixed before implementation handoff.

Faithful migration requires independent historical V2 DDL and populated history, not normal v3 open followed by metadata downgrade. Include unbound tuple without host/outcome; legacy host/descendant; complete old approval participants; prepared-before migration operation; malformed schema/history variants. Inject failures at DDL, census population and final CAS, then inspect version/schema/history after reopening. No operating database migration is involved.

Future parent commands—not executed here—must retain jobs1, incremental0, locked/offline, serial libtest execution and the ≥2GiB disk guard:

```text
cargo test --locked --offline -p foundry-ledger \
  --test b1_product_results --test b1_scope --test b1_product_atomic \
  -- --test-threads=1

cargo test --locked --offline -p foundry-ledger \
  --lib --no-run --message-format=json

cargo test --locked --offline -p foundry-line --features p2-test-support \
  --test b1_product_results --test b1_product_result_processes \
  -- --test-threads=1

cargo test --locked --offline -p foundry-serve --features p2-test-support \
  --test b1_auth_http --test b1_product_artifacts -- --test-threads=1

cargo test --locked --offline -p foundry --features p2-test-support \
  --test b1_scope_cli --test b1_approve_cli -- --test-threads=1
```

Parse the prebuild’s `compiler-artifact` records for target name `foundry_ledger`, kind containing `lib`, `profile.test=true`, non-null executable, and exactly one match. Export its absolute path as `B1_LEDGER_TEST_EXE`. Round62/63 JSON contains exactly that default-feature artifact; do not reuse its filename as a future guess.

Additional gaps in the latest parent command evidence:

- Explicit enabled S **result-artifact** and C **result-loopback** coverage is absent.
- Current-fixture HX rerun is absent.
- Default type/method compile-negative and enabled counterpart logs are absent.
- Preserve default core/watch/all-five-CLI lanes.
- Preserve the recovery plan’s four package `--no-default-features` checks and foundry examples check.
- Enabled library command must name ledger/line/serve only; foundry is binary-only and needs `cargo test ... -p foundry --all-features --bin foundry`. Do not use the obsolete four-package `--lib` command.

The external probe must use its own empty `[workspace]`, absolute ledger path dependency with `default-features=false`, forwarding feature, isolated target directory and offline-generated lock. Verify both:

```rust
use foundry_ledger::P2ReviewPreparation;
```

and:

```rust
let _ = foundry_ledger::ProductStore::prepare_p2_review_fixture;
```

Require intended unresolved-type/method diagnostics in default mode, not dependency/cache failure. The enabled counterparts must compile. Separate probe units are appropriate if one compiler error masks the other. Enabled/unified HTTP/CLI/startup/deserializer isolation remains a separate executed requirement.

**Next-gate disposition**

Gate1 can be concretely scoped around the frozen scripts and the confirmed predicates above. Its prerequisites are the approved production/test whitelist, exact scripted-fixture/child manifest extension, and parent evidence commands—not a new contract choice about Resume counts, authority, or byte integrity.

Before handing off all three bundles, settle these exact remaining items:

- **Existing-v3 schema disposition:** resolve the authority-key FK/extra approval uniqueness conformance and how any necessary DDL correction applies to already-created v3 files. Silent open-time repair or an invented migration is prohibited.
- **Operation-local I/O harness:** fix the COMMIT and recovery-lookup fault sites, injected error classes, durable-outcome assertions, and child protocol before claiming those cases implementable.
- **Scope approval:** approve the concrete whitelist above, including `product.rs` and `main.rs` where required; Gate0 authorization itself is not implementation authorization.

The inventory remains **46 effective rows = 1 PASS + 45 GAP + 0 BLOCKED; zero exclusions**, plus six separately mapped legacy-test responsibilities. No W3 completion claim is justified.

**REFINE**

## Round 67 — implementer-run Gate1 producer and continuation evidence (not parent-certified)

Scope was limited to ADDENDUM67. The fixed effective inventory remains **46 rows = 1 PASS + 45 GAP + 0 BLOCKED; zero exclusions**, with the six legacy-test responsibilities still tracked separately. This round does not change the Gate0 counts and does not establish W3 completion.

### Gate1 rows with concrete positive evidence

| Effective row | Implementer-run evidence | Disposition |
|---|---|---|
| `standalone_store_still_rejects_product_v3` | Default core command below includes `foundry-exec --tests`; `graph`/tuple SQLite output includes `standalone_tuple_opener_refuses_product_contract_before_schema_changes` and standalone ledger-contract tests. | Positive refusal/standalone regression passed; this does not prove every v3 schema mutation variant. |
| `genuine_host_result_finalizes_without_emit_or_publish` | H test `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor`; log `/tmp/add67-H-final.log`, exit 0. Genuine-host finalization/replay and S1 anchor assertions passed. | Positive path covered; the matrix's entire corruption/error-precedence set is not covered by this test. |
| `result_uses_persisted_numeric_run_and_activation` | Same H test above; persisted test fixture uses the colon-containing `p2:A` order identifier and persisted run/activation coordinates. | Positive persisted-coordinate mapping covered. |
| `interleaved_order_evidence_pairs_own_sequence_and_hash` | H test `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor` appends and admits a foreign order after the selected order's last event, then checks the result payload's selected sequence/hash and verifies that hash differs from global head. | Frozen interleaved positive covered. |
| `tuple_and_result_binding_commit_as_one_set` | H test `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor` installs tuple-insert and binding-insert abort triggers, asserts both roll back to T/R 0/0 while retaining four stages/three sessions, then finalizes; HX also observes pre/post-commit SIGKILL. Logs `/tmp/add67-H-final.log` and `/tmp/add67-HX.log`, both exit 0. | Injected insert-failure and process crash boundaries passed; matrix race variants remain GAP. |
| `nonempty_generation_digest_matches_exec_and_preserves_legacy` | L tests `single_generation_digest_uses_exec_struct_bytes_not_historical_value_bytes` and `typed_tuple_digest_uses_nonempty_exec_generation_encoding`; `/tmp/add67-L-final.log`, exit 0, 4 passed. | Single and multi-generation typed digest vectors passed, including distinction from historical `Value` encoding. |
| `accepted_resume_binds_three_generations_and_fourteen_artifacts` | H test of frozen scenario A; `/tmp/add67-H-final.log`, exit 0. It asserts exact 14/3/1 calls, 14 artifacts/8 sessions/14 budget, three chain generations, terminal tuple coordinates, causal inherited artifacts, accepted hold receipt/snapshot/digest/progress/owner/epoch/fence/reservation, eight-member authority copy, and unchanged S1 anchor. | Frozen positive A covered. Negative ancestry/corruption cases remain GAP. |
| `commitpass_resolves_origin_without_materializing_reserved_run` | H test of frozen scenario B and direct CommitPass; `/tmp/add67-H-final.log`, exit 0. B asserts 14/3/1 with result origin run 8 and reserved successor 9 unmaterialized; direct asserts 4/0/1 with origin run 7 and reservation 8. | Frozen positives B and direct covered. |
| `authority_is_captured_at_activation_and_copied_on_resume` | Scenario A H assertions inspect the frozen accepted-hold source and exact eight copied authority members; B/direct assert CommitPass adds no copy. | Positive Resume copy and CommitPass zero-copy covered. Atomic rollback, update/delete/replace denial, and authority snapshot failure variants remain GAP. |

The H and L commands were run with `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0`, `--locked --offline`, and serial libtest execution:

```text
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_results -- --test-threads=1
log=/tmp/add67-H-final.log exit=0: 3 passed, 0 failed, 0 ignored

CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-ledger --test b1_product_results -- --test-threads=1
log=/tmp/add67-L-final.log exit=0: 4 passed, 0 failed, 0 ignored

H was rerun against the final source after a mechanical duplicate-predicate cleanup:
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_results -- --test-threads=1
log=/tmp/add67-H-post-audit.log exit=0: 3 passed, 0 failed, 0 ignored
```

The required Cargo-JSON-ledger-executable handoff completed before and after the final source audit. The post-audit executable was rebuilt and reparsed after the final `result.rs` cleanup. Exact commands and outputs:

```text
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-ledger --lib --no-run --message-format=json > /tmp/add67-ledger-lib-artifacts.jsonl 2> /tmp/add67-ledger-lib-build.stderr
exit=0; parsed exactly one matching foundry_ledger/lib/profile.test=true/non-null executable record
B1_LEDGER_TEST_EXE=/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-ledger --lib --no-run --message-format=json > /tmp/add67-ledger-lib-artifacts-post-audit.jsonl 2> /tmp/add67-ledger-lib-build-post-audit.stderr
exit=0; parser found exactly one matching library test executable
parsed executable=/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398
B1_LEDGER_TEST_EXE="$(cat /tmp/add67-ledger-test-executable-post-audit.txt)" CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_result_processes -- --test-threads=1
log=/tmp/add67-HX-post-audit.log exit=0: 5 passed, 0 failed, 0 ignored
```

HX demonstrates existing approval-process and result-finalizer SIGKILL cases only; it is not evidence for result-specific race, operation-local COMMIT-I/O, lookup-I/O, or response-loss cases.

### Gate1 rows still GAP after this round

| Effective row | Executed target/log | Missing frozen assertion; disposition |
|---|---|---|
| `activation_and_lifecycle_aliases_cannot_share_tuple_key` | H `/tmp/add67-H-final.log` | No same-key/different-activation or lifecycle attempt; GAP. |
| `result_coordinates_reject_overflow_and_wrong_attempt` | L `/tmp/add67-L-final.log`; H `/tmp/add67-H-final.log` | Digest vectors and a valid persisted coordinate were tested, but no `i64::MAX+1`, overflow, wrong-attempt replay, or unchanged-set negative; GAP. |
| `result_payload_and_local_receipt_have_exact_v1_bytes` | L and H logs above | No exact nested/outer receipt bytes or unknown/duplicate/missing/noncanonical member matrix; GAP. |
| `emit_key_and_promotion_tuple_are_not_interchangeable` | L and H logs above | No serialized emit/promotion receipt substitution negative; GAP. |
| `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` | H log above | A/B positive generation chains only; no independent restored bad-chain baselines; GAP. |
| `result_location_must_resolve_registered_terminal_evidence` | H log above | No fallback/foreign-stage/external-path mutations; GAP. |
| `held_failed_and_materialized_continuation_are_not_final_results` | H log above; default core `/tmp/add67-core-default.log` includes related host lifecycle tests | No producer finalizer attempts against all three frozen held/failed/materialized baselines; GAP. |
| `queued_success_is_not_silently_made_approvable` | H log above | No paired queued/waiting finalizer runs or S1 delta assertion for them; GAP. |
| `legacy_host_census_distinguishes_missing_authority_from_corruption` | H log above; default core log includes fresh-v3 and read-only ledger tests | No faithful migrated legacy census fixture or separate missing-copy corruption baseline; GAP. |
| `completed_initial_claim_finalizes_after_worker_expiry` | H log above | No worker-expiry clock advance with lease/claim preservation assertion; GAP. |
| `pending_result_requires_original_authority_revisions` | H log above; default core log includes service authority regression tests | No pending genuine result finalized after source/service/credential rotation matrix; GAP. |
| `revoke_after_stage_start_preserves_result_but_blocks_finalization` | HX `/tmp/add67-HX.log`; default core log includes service review regressions | HX covers prior approval/finalizer crash boundaries only; no exact stage-start revoke/rebind barrier; GAP. |
| `revoke_before_stage_start_has_zero_executor_calls` | HX log above; default core log includes service review regressions | No result-producer zero-call stage-start revoke barrier; GAP. |
| `reassigned_owner_cannot_store_or_finalize_old_result` | HX log above; default core log includes hold/process tests | No post-fence ordinary completion and direct CommitPass producer variants; GAP. |
| `initial_redrive_remains_unsupported` | H log above | A/B/direct positive scripts only; no exact frozen unsupported-redrive 5/0/1 run; GAP. |
| `continuation_revocation_distinguishes_restore_from_execution` | H/HX logs above; default core log includes continuation tests | No exact cumulative 5/1/1 revocation run with restore distinguished; GAP. |
| `p2_preparation_walks_s1_without_result_seeding` | H fixture calls the real `prepare_p2_review_fixture(..., Waiting)` before the work claim; `/tmp/add67-H-final.log` | The fixture does not assert the complete +3 S1/+3 receipt and zero-intent/claim/result/tuple/promotion pre-claim matrix or injected preparation rollback; partial only, remains GAP. |

Existing tests that exercise related subsystems are not substitutes for these frozen producer predicates. These rows remain gaps even though the default core and process lanes pass.

### Required broader lanes and environment results

| Lane | Exact command/result | Disposition |
|---|---|---|
| Default core | `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-exec -p foundry-line -p foundry-ledger -p foundry-serve --lib --tests -- --test-threads=1`; `/tmp/add67-core-default.log`; exit 0. | All selected default lib/test targets passed, including line restore/review-host, ledger result and service tests, serve HTTP, and standalone compatibility. |
| Default watch | `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-watch --test b1_github_polling --test b1_intake_durability -- --test-threads=1`; `/tmp/add67-W-default.log`; exit 101 because the first target's eight tests could not bind loopback (`Operation not permitted`); rerun separately for intake: `... -p foundry-watch --test b1_intake_durability -- --test-threads=1`, `/tmp/add67-W-intake-default.log`, exit 0, 9 passed. | Loopback polling is a sandbox rerun gap, not product PASS/failure; intake durability passed. |
| Default CLI | Required five-target chain `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry --test b1_service_cli --test b1_approve_cli --test b1_auth_cli --test b1_watch_cli --test b1_scope_cli -- --test-threads=1`; `/tmp/add67-C-default.log`; exit 101 at scope target after approve/auth passed; four scope tests could not bind loopback. Remaining independent service/auth/watch targets were run in `/tmp/add67-C-default-independent.log`, exit 0, one test each passed. | Scope loopback tests require parent rerun outside this sandbox. |
| Enabled serve | `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-serve --features p2-test-support --test b1_auth_http --test b1_product_artifacts -- --test-threads=1`; `/tmp/add67-S-enabled.log`; exit 0, auth 3 passed and artifact 1 passed. | Existing approval and stage-artifact coverage passed; it does not establish result-artifact metadata/consumer coverage. |
| Enabled CLI | `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry --features p2-test-support --test b1_scope_cli --test b1_approve_cli -- --test-threads=1`; `/tmp/add67-C-enabled.log`; exit 101 at scope loopback binds. The genuine result CLI approve/replay positive in `b1_approve_cli` separately passed in `/tmp/add67-C-enabled.log` before that earlier run's scope target; default approve/auth targets also passed in the later five-target command. | Enabled result approval positive passed; scope socket lane remains a sandbox rerun gap. |
| Formatting / diff | Targeted `rustfmt --check --edition 2024` for changed/allowed Rust files and `git diff --check`; both exit 0. | Passed. |

`graphify` was unavailable on PATH and `graphify-out/GRAPH_REPORT.md` was absent; no report was fabricated. Free disk after the required lanes was 9.0 GiB, above the 2 GiB guard. Test-created directories remain in the configured scratch location and are not cleaned by this round.

Gate2 and Gate3 remain GAP as recorded above: no broad result consumer/read validator, result-artifact HTTP/CLI loopback proof, external compile probes, faithful historical migration fixtures, or local COMMIT/lookup I/O injection was added. The acceptance denominator and prior Gate0 inventory are unchanged pending parent validation.

### Round67 file and temporary-artifact inventory

Existing untracked files whose contents changed in this round: `crates/foundry-ledger/src/product/result.rs`, `crates/foundry-line/tests/b1_product_results.rs`, `crates/foundry-line/tests/support/product_result_fixture.rs`, `crates/foundry-ledger/tests/b1_product_results.rs`, and this evidence document. No workspace path was newly created or deleted. `crates/foundry-ledger/src/product/review.rs` and `crates/foundry-line/tests/b1_product_result_processes.rs` were in the whitelist and snapshotted, but their contents did not change. The full pre-round repository status and original allowed-file contents/hashes are preserved in `/tmp/add67-pre-edit/git-status-before.txt`, `/tmp/add67-pre-edit/sha256-before.txt`, and the corresponding path-preserving copies under `/tmp/add67-pre-edit/`. The configured `.hermes/cache/scratch` snapshot location was not writable for these snapshot files, so the copies/manifests were placed in permitted `/tmp`.

Round67 `/tmp` output files (all retained): `/tmp/add67-C-default-independent.log`, `/tmp/add67-C-default.log`, `/tmp/add67-C-enabled.log`, `/tmp/add67-H-final.log`, `/tmp/add67-H-post-audit.log`, `/tmp/add67-H.log`, `/tmp/add67-HX-post-audit.log`, `/tmp/add67-HX.log`, `/tmp/add67-L-final.log`, `/tmp/add67-L.log`, `/tmp/add67-S-enabled.log`, `/tmp/add67-W-default.log`, `/tmp/add67-W-intake-default.log`, `/tmp/add67-core-default.log`, `/tmp/add67-ledger-lib-artifacts-post-audit.jsonl`, `/tmp/add67-ledger-lib-artifacts.jsonl`, `/tmp/add67-ledger-lib-build-post-audit.stderr`, `/tmp/add67-ledger-lib-build.stderr`, `/tmp/add67-ledger-test-executable-post-audit.txt`, `/tmp/add67-ledger-test-executable.txt`, and the nine pre-edit snapshot files listed by `find /tmp/add67-pre-edit -type f` (seven original files, status manifest and hash manifest).

Test-created directories present under configured scratch at inventory time (none deleted):

```text
foundry-b1-product-result-genuine-37-0-1792459023540450054
foundry-b1-product-result-genuine-37-0-1792459083423162315
foundry-b1-product-result-genuine-37-0-1792459158836638719
foundry-b1-product-result-genuine-37-1-1792459035583515438
foundry-b1-product-result-genuine-37-1-1792459100715858424
foundry-b1-product-result-genuine-37-2-1792459047763455523
foundry-b1-product-result-genuine-42-2-1792402501755379375
foundry-b1-product-result-genuine-42-4-1792402501755411004
foundry-b1-product-result-genuine-61-0-1792400417632587733
foundry-b1-product-result-genuine-72-0-1792400453474161842
foundry-b1-product-result-genuine-82-0-1792446276749874408
foundry-b1-product-result-genuine-82-0-1792458915692282776
foundry-b1-product-result-host-13-0-1792389328309119720
foundry-b1-product-result-host-2357800-0-1792388930806149736
foundry-b1-product-result-host-80-0-1792389424650102315
foundry-b1-product-result-host-80-0-1792389467120192402
foundry-b1-product-result-host-80-0-1792389506736486522
foundry-b1-product-result-host-80-0-1792389555457985351
foundry-b1-product-result-host-80-0-1792389601326187769
foundry-b1-product-result-host-80-0-1792389659226445541
foundry-b1-product-result-host-80-0-1792389706247923020
foundry-b1-product-result-host-80-0-1792389743828175482
```

Cargo also produced the exact ledger test executable recorded above and normal ignored build products under the existing `target/` directory.

## Round 69 — ADDENDUM67 Gate1 continuation (implementer evidence; parent judgment pending)

The effective denominator is still **46 rows, with 0 exclusions**. The prior evidence labels remain historical; this section records round69 assertions without changing any frozen requirement or claiming W3 completion. Parent round68 logs were preserved and independently supplied as regression evidence: `/home/cosmo/.hermes/cache/scratch/b1-parent-round68-{h-hx,ledger,http-enabled,cli-enabled,core,watch,cli-default}.log`. They do not close the missing Gate1 rows below.

### Gate1 source/assertion map

| Frozen matrix row | Source and actual assertion in round69 | Status |
|---|---|---|
| `standalone_store_still_rejects_product_v3` | Regression only: `add67-round69-core.log` ran default `foundry-exec` and ledger tests; parent round68’s ledger/core lanes also passed. No new full schema-mutation matrix. | Partial |
| `genuine_host_result_finalizes_without_emit_or_publish` | `b1_product_results.rs::genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor`: genuine host, four complete stages/artifacts, three sessions, tuple/result 1/1, zero promotions, no S1 write, exact replay, approval does not publish. `add67-round69-H-final6.log`, 7 passed/0 failed/0 ignored. | Positive path PASS; full error-precedence set remains open |
| `result_uses_persisted_numeric_run_and_activation` | Same H test asserts run7/activation3/generation0 from a colon-containing `result-order` and wrong-attempt/activation outcomes. | Positive mapping PASS |
| `activation_and_lifecycle_aliases_cannot_share_tuple_key` | H main test first finalizes the genuine run7/activation3 tuple, then requests activation4 against the same tuple key and requires `CorruptEvidence` without replacing the winner. | Partial: activation winner alias covered; a second lifecycle alias remains GAP |
| `result_coordinates_reject_overflow_and_wrong_attempt` | H checks `u64::MAX`→`InvalidInput`, wrong attempt→`StaleBinding`, wrong activation→`Ineligible`, and unchanged tuple/result/stage/session counts. `result.rs::result_coordinates_use_checked_persisted_widths` checks colon mapping and checked conversion. | Covered for these exact vectors; remaining signed-boundary/replay variants GAP |
| `tuple_and_result_binding_commit_as_one_set` | H genuine G-pre trigger-aborts tuple and result-binding inserts independently and asserts T/R remain 0/0 with host evidence intact. HX final run tests result finalizer pre/post-COMMIT SIGKILL, separate-process replay and competition; `add67-round69-HX-handoff-final.log`, 5 passed. | Partial: explicit per-insert failure is H; operation-local COMMIT-I/O remains Gate3 GAP |
| `nonempty_generation_digest_matches_exec_and_preserves_legacy` | `b1_product_results.rs` pure encoding tests compare single and nonempty multi-generation digest bytes against exec and distinguish historical `Value` bytes; `add67-round69-ledger-results.log`, 4 passed. | PASS for recorded digest vectors |
| `result_payload_and_local_receipt_have_exact_v1_bytes` | `result.rs::result_payload_has_exact_nested_v1_bytes_and_strict_work_members` checks exact complete outer/nested ResultPayload v1 bytes, raw embedded receipt/output/payload-snapshot strings, unknown/duplicate/missing outer and work members, and noncanonical work order. `local_result_receipt_has_exact_v1_bytes_and_strict_members` separately checks exact receipt bytes, round-trip and strict member cases. | PASS for frozen outer/nested schema vectors |
| `emit_key_and_promotion_tuple_are_not_interchangeable` | `result.rs::execution_emit_and_promotion_receipts_are_not_local_result_receipts` serializes actual `EmitReceipt` and `PromotionReceipt` and rejects each as `LocalResultReceiptV1`. | PASS |
| `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` | H main test uses a genuine G-pre baseline, empty chain, changed root/terminal commits, wrong generation index and initial fix-log path, restoring the host result between mutations and asserting no tuple/result writes. Scenario A independently changes the followup fix-log path and the registered verification artifact's `produced_commit`, then restores each exact value. | Partial: foreign/unbacked chain and remaining independent root-to-terminal corruption variants remain GAP |
| `accepted_resume_binds_three_generations_and_fourteen_artifacts` | `b1_product_results.rs::accepted_resume_binds_three_generations_and_fourteen_artifacts` asserts frozen A 14/3/1, 14 artifacts, 8 sessions, 14 budget, generation chain, causal ancestry, hold receipt/snapshot/progress owner+epoch+fence+reservation and unchanged S1. | Positive PASS |
| `commitpass_resolves_origin_without_materializing_reserved_run` | `commitpass_resolves_origin_without_materializing_reserved_run` runs frozen B and direct scripts: B 14/3/1 origin8/reservation9; direct 4/0/1 origin7/reservation8; no successor materialization or allocator increment. | Positive PASS |
| `result_location_must_resolve_registered_terminal_evidence` | H main test independently supplies `pack#7`, external/unregistered path, earlier-stage registered path, empty chain and bad root; invalid paths return `InvalidEvidence`; each mutation restores the genuine terminal row. | PASS for listed path vectors |
| `held_failed_and_materialized_continuation_are_not_final_results` | `assert_continuation_result` first submits genuine held terminal results and gets `Ineligible`; no T/R write. | Partial: genuine failed and materialized-but-pending continuation finalizer baselines remain GAP |
| `queued_success_is_not_silently_made_approvable` | `queued_success_is_not_a_final_result_and_does_not_change_s1` generates a genuine host success from queued preparation and checks `StaleBinding`, unchanged queued S1/zero result set/zero extra calls. Main H positive is the waiting baseline and succeeds without an S1 append. | Covered queued rejection and waiting success; paired full S1 delta matrix is partial |
| `authority_is_captured_at_activation_and_copied_on_resume` | Fixture asserts zero authority rows after intake/claim and one on activation. H A asserts exact eight-member Resume copy; B/direct assert no CommitPass copy; immutable UPDATE/DELETE/REPLACE guards refuse. A Resume-copy INSERT trigger failure asserts rollback to one snapshot, accepted progress, and no materialized run/host acceptance; H final passed. | PASS for required capture/copy/rollback/immutability assertions |
| `legacy_host_census_distinguishes_missing_authority_from_corruption` | No faithful migrated legacy-host/census baseline was added; migration census remains a separate Gate3 fixture requirement. | GAP; no historical authority inferred |
| `completed_initial_claim_finalizes_after_worker_expiry` | `completed_initial_result_finalizes_after_worker_lease_expiry_without_renewal` advances the result store six minutes, finalizes successfully, and compares the complete claim/backlog owner/epoch/fence/state/lease tuple byte-for-byte before/after. | PASS |
| `pending_result_requires_original_authority_revisions` | Pending initial result is rejected after real service-grant/service-revision reconfiguration plus credential rotation and fresh DriveReview authentication. A separate genuine pending initial result is rejected `Unauthorized` after Owner-driven scoped-source rebind; T/R remain 0/0 and stage/session/S1 counts remain unchanged. On the same genuine completed-claim baseline, replacement owner and same-owner/new-epoch/fence mutations each return `StaleBinding`; the exact claim row is restored before source rebind. Initial unrelated human scope change remains accepted; pending continuation unrelated human scope revision returns `StaleBinding`; continuation authority snapshot alteration returns `CorruptEvidence`. | Partial: pending A/B service rotations and source disable→regrant without replacement context remain GAP |
| `revoke_after_stage_start_preserves_result_but_blocks_finalization` | Existing default service-review tests cover a related host-stage boundary; round69 does not run the frozen result-specific HX barrier/count script. | GAP |
| `revoke_before_stage_start_has_zero_executor_calls` | No frozen result-specific pre-stage revoke barrier was run. | GAP |
| `reassigned_owner_cannot_store_or_finalize_old_result` | H mutates a genuine completed initial claim to a replacement owner and separately to the same owner with a newer epoch/fence; both new finalizations return `StaleBinding`, with the exact original claim row restored. | Partial: direct CommitPass terminal persistence and separate-process ownership transfer variants remain GAP |
| `initial_redrive_remains_unsupported` | Probe log `/home/cosmo/.hermes/cache/scratch/add67-round69-H-redrive-rerun.log`: the exact attempted failure script reached four executor calls and failed the frozen `calls == 5` assertion. The temporary in-source probe test was removed after it exposed the out-of-scope blocker; its failed log and isolated DB were retained, but the final H target does not contain that probe (this is not acceptance evidence). The DB is `/home/cosmo/.hermes/cache/scratch/foundry-b1-product-result-genuine-39-0-1792465363990434906/product.sqlite`; its failure row records `E_HOST_STAGE: Worker("product review recovery operation unavailable: legacy detached budget charge")`, with four completed stage/artifacts, three sessions, budget4, failure1 and no numeric host result. `ProductStore::charge_budget` in `crates/foundry-ledger/src/product/review.rs` returns unsupported before the fifth `StageExecutor::execute`; changing that host algorithm is outside ADD67’s permitted result/ancestry/authority/provenance production scope. | GAP / explicit mechanical scope blocker; required 5/0/1 was not met |
| `continuation_revocation_distinguishes_restore_from_execution` | No frozen cumulative 5/1/1 revoke-during-continuation script with independently measured restore count was run. | GAP |
| `interleaved_order_evidence_pairs_own_sequence_and_hash` | H main test appends/adopts a foreign order after the selected order’s last event and asserts selected sequence/hash pair with selected hash different from global head. | PASS |
| `p2_preparation_walks_s1_without_result_seeding` | `support/product_result_fixture.rs::genuine_host_done` tests Waiting preparation rollback on S1 insert failure and Queued rollback on backlog update failure; successful Waiting preparation asserts exact 4 ledger rows/4 receipts including admission, zero intents/claims/results/tuples/stages, ready backlog; Queued branch asserts its matching pre-claim state. | PASS for scoped P2 preparation and rollback assertions |

Remaining in-scope Gate1 mechanical gaps are explicitly lifecycle aliasing (activation alias is tested), foreign/unbacked chain and remaining independent chain corruptions, failed/materialized eligibility baselines, source disable→regrant without replacement context and pending A/B authority rotations, result-specific before/after-stage revoke barriers, direct CommitPass/separate-process owner reassignment, and continuation revoke 5/1/1. The faithful legacy-host census-negative needs its Gate3 historical fixture. `initial_redrive_remains_unsupported` cannot meet its frozen fifth-executor-call assertion inside the current allowed production predicate; the measured host algorithm predicate and exact isolated evidence are above. These gaps do not change 46 or become human policy decisions.

### Round69 validation actually run

All new command output files below use `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`, and serial `--test-threads=1` where an integration target was selected. Free disk remained 8.9 GiB (guard: 2 GiB).

| Lane | Result |
|---|---|
| H final | `cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_results -- --test-threads=1`; `/home/cosmo/.hermes/cache/scratch/add67-round69-H-final6.log`; exit0, 7 passed/0 failed/0 ignored. An intermediate H-final3 log records a test-harness SQL-column mistake (`output_json` belongs to `product_review_stages_v1`); the test query was corrected and later H runs passed. |
| Ledger result integration | `cargo test --locked --offline -p foundry-ledger --test b1_product_results -- --test-threads=1`; `add67-round69-ledger-results.log`; exit0, 4/0/0. |
| Ledger lib | Final serial rerun `cargo test --locked --offline -p foundry-ledger --lib -- --test-threads=1`; `/home/cosmo/.hermes/cache/scratch/add67-round69-ledger-lib-wire-final.log`; exit0, 39/0/0. |
| HX final | Cargo JSON prebuild `cargo test --locked --offline -p foundry-ledger --lib --no-run --message-format=json > add67-round69-ledger-artifacts-hx-final.jsonl 2> add67-round69-ledger-build-hx-final.stderr`; parser selected exactly one `foundry_ledger`, `kind` contains `lib`, `profile.test=true`, non-null executable and wrote `add67-round69-ledger-test-exe-hx-final.txt` containing `/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398`. Then `B1_LEDGER_TEST_EXE="$(cat /home/cosmo/.hermes/cache/scratch/add67-round69-ledger-test-exe-hx-final.txt)" cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_result_processes -- --test-threads=1`; `add67-round69-HX-handoff-final.log`; exit0, 5/0/0. |
| Enabled HTTP | `cargo test --locked --offline -p foundry-serve --features p2-test-support --test b1_auth_http --test b1_product_artifacts -- --test-threads=1`; `add67-round69-http-enabled-final.log`; exit0, 3+1 passed, zero failed/ignored. |
| Enabled CLI | `cargo test --locked --offline -p foundry --features p2-test-support --test b1_scope_cli --test b1_approve_cli -- --test-threads=1`; `add67-round69-cli-enabled-final.log`; exit101: approval/bootstrap 2/0/0 passed; scope target 1 passed, 4 failed at sandbox loopback bind (`Operation not permitted`). |
| Default core | `cargo test --locked --offline -p foundry-exec -p foundry-line -p foundry-ledger -p foundry-serve --lib --tests -- --test-threads=1`; `add67-round69-core.log`; exit0, 405 passed/0 failed/0 ignored across 39 lib/test targets. |
| Watch | `cargo test --locked --offline -p foundry-watch --test b1_github_polling --test b1_intake_durability -- --test-threads=1`; `add67-round69-watch.log`; exit101, all 8 polling tests could not bind loopback (`Operation not permitted`). Separate `cargo test --locked --offline -p foundry-watch --test b1_intake_durability -- --test-threads=1`; `add67-round69-watch-intake.log`; exit0, 9/0/0. |
| Default five CLI | `cargo test --locked --offline -p foundry --test b1_service_cli --test b1_approve_cli --test b1_auth_cli --test b1_watch_cli --test b1_scope_cli -- --test-threads=1`; `add67-round69-cli-default-final.log`; exit101 at scope target: 4 loopback cases failed with `Operation not permitted`. Independent `b1_service_cli`, `b1_auth_cli`, `b1_watch_cli`; `add67-round69-cli-default-independent-final.log`; exit0, 3/0/0. Earlier service/approval/auth/watch targets in the chained run passed before scope failure. |
| Formatting and whitespace | `rustfmt --check --edition 2024` on all six authorized Rust paths and `git diff --check`; exit0. `graphify` is unavailable on PATH and `graphify-out/GRAPH_REPORT.md` is absent; no report was fabricated. |

The round68 parent logs are prior independent evidence, not a replacement for the round69 reruns or any missing result-consumer/W3 rows. Sandbox loopback failures are environment reruns for the parent; no test was skipped or changed to obtain a pass.

### Round69 file and temporary inventory

Changed from the pre-edit snapshot: `crates/foundry-ledger/src/product/result.rs`, `crates/foundry-ledger/src/product/review.rs`, `crates/foundry-line/tests/b1_product_results.rs`, `crates/foundry-line/tests/support/product_result_fixture.rs`, and this evidence document. The whitelisted `crates/foundry-line/tests/b1_product_result_processes.rs` and `crates/foundry-ledger/tests/b1_product_results.rs` matched their snapshots unchanged. No workspace path was created or deleted, and no pre-round assertion was removed. One temporary redrive probe test added during this round was removed after its exact `calls==5` expectation failed at four; the failure was recorded and the frozen row remains GAP, as documented above. The round69 evidence document is an existing allowed ledger.

Round69 scratch logs/probes/snapshots are retained under `/home/cosmo/.hermes/cache/scratch/`; their exact path inventory is `/home/cosmo/.hermes/cache/scratch/add67-round69-inventory.txt`. It covers H/HX/L/core/watch/CLI/HTTP logs, redrive probe logs, JSONL/stderr/executable handoff, initial/final hashes/status, allowed-file copies, and test-created scratch directories. Initial and final porcelain status snapshots match exactly, so all existing dirty/untracked workspace paths predate this round and no workspace path was created or deleted. `/home/cosmo/.hermes/cache/scratch/add67-round69-before/preexisting-tmp-files.txt` separately enumerates 715 paths that were already under `/tmp` before round69; those paths were not read for implementation, modified, or deleted by round69, and no round69 output was written to `/tmp`. The failed redrive fixture DB named in the row above was retained. Cargo reused the existing ignored `target/`; no separate target tree was made. No cleanup was performed.

The acceptance matrix still has Gate2/Gate3 rows open as recorded above and in the prior section. Parent verification is pending. This evidence is not a W3 completion claim.

## Round 70 — ADDENDUM68 charge-at-start and current validation

ADDENDUM68 approval Discord1561969234354114641 option A changes only the initial unsupported-redrive budget observation from 4 to 5. The matrix row now carries that provenance. The denominator remains 46, with zero exclusions. Round69 entries above are historical; this section supersedes only the redrive GAP and adds the measured current-round evidence below. No Gate1 or W3 acceptance is claimed.

### Current ADDENDUM68 row mapping

| Gate1 obligation | Current source/assertion | Current result |
|---|---|---|
| Initial unsupported redrive and charge-at-start | `b1_product_results.rs::initial_redrive_remains_unsupported_and_cannot_finalize` uses `ReviewScript::InitialRedrive`, explicit `dispatch_error` and absent `terminal`; asserts the exact nested `E_HOST_STAGE: Worker("product review recovery operation unavailable: product redrive persistence is not configured")` error, execute/restore/dispatch 5/0/1, four completed stages/artifacts, three sessions, budget5, failure1, failed run, claimed backlog/work, no fix generation, no numeric host result/tuple/result binding, unchanged S1, and no work on re-finalization. `review.rs::charge_budget` validates order, exact current run/pack/work/service/source authority under `BEGIN IMMEDIATE`, uses checked limit/usage and one-row CAS, commits before executor start, and does not refund a failed stage/save. Historical RED: `/home/cosmo/.hermes/cache/scratch/add67-round69-H-redrive-rerun.log`, exact calls4-versus5 failure; current full H GREEN: `b1-add68-H-final.log`; exact-error rerun: `b1-add68-redrive-exact-GREEN-rerun.log`. The first stricter expectation omitted the enclosing `E_HOST_STAGE` wrapper and failed in `b1-add68-redrive-exact-GREEN.log`; its exact observed string was corrected without weakening the assertion. | PASS for the amended frozen 5/0/1 observation and exact unsupported-save error; failures were retained. Wrong-order/stale/revoked/exhausted/SQL-rollback charge guard vectors remain unmeasured. |
| Connector revision capture | `review.rs` selects `ps.revision` independently from service revision; `b1_product_results.rs::activation_authority_captures_connector_revision_independently` mutates service revision to2 while connector revision stays1 and asserts the authority tuple `(service=2, connector=1)` plus genuine finalization. | PASS for unequal revision capture. |
| Exact successful Resume authority copy and rollback | `b1_product_results.rs::assert_continuation_result` checks exact eight-member Resume authority copy; trigger-suppressed `INSERT SELECT` requires exact affected row count and rolls all materialization back to accepted progress with no run/host acceptance. CommitPass/direct paths assert zero successor copy. | PASS for successful count and zero-copy rollback. |
| Structural ancestry, coordinates, held provenance, alias and stored wire validation | `result.rs` validates terminal order, checked attempt/run/activation widths, full ancestry coordinates, exact run journal and lifecycle, host acceptance/intent/authority/work bindings, immutable accepted hold receipt and source/admission snapshot; checks stored payload/receipt v1 and `local_review_result`; validates winner before alias Conflict; verifies producer/verification commits in causal artifact order. H includes colon order, wrong attempt/activation/overflow, empty/root/index/terminal/fix-log mutations, valid activation alias Conflict, and isolated alias-winner corruption returning CorruptEvidence. | Source repairs present and current H passes. Independent fresh-baseline coordinate/snapshot/progress/foreign-artifact vectors and full error-precedence cross-product remain GAP. |
| Original-source authorization on finalize and lookup | `result.rs::require_requested_source_in` is shared by finalize and lookup and runs before stored-result disclosure; original requested source is authenticated separately from the caller's service grant. | H finalization source rebind denial passes. Lookup-specific wrong-source and wrong-source-plus-corrupt-winner vectors remain GAP. |
| P2 setup and live authority changes | Shared fixture verifies exact Waiting/Queued preparation rows and rollback on S1/backlog injected failure. H verifies pending new-finalization denial after service/credential rotation and source rebind, with unchanged history; service-backed A/B lineage retains captured authority. | Partial: isolated initial/A/B rotations and disable/re-enable with the old context are not all covered independently. |
| Result-specific barriers and reclaims | HX ran actual child-process finalization/approval and SIGKILL boundaries (5/0/0 failures); current scope does not add a result-specific pre-stage or post-stage revocation barrier, continuation 5/1/1, direct CommitPass reassignment, or completed A/B different-owner and same-owner ABA reclaim pair. | GAP; existing approval races do not substitute for result races. |
| Legacy census and strict absent-authority behavior | No fabricated historical authority was seeded. Current core test run found two older service-less synthetic continuation fixtures in `b1_product_restore` that do not carry the required activation snapshot and are refused before continuation; these fixtures are not a faithful Gate3 migrated-history proof. | GAP / compatibility follow-up: add genuine service-backed fixture setup or retain the exact refusal expectation; do not relax missing-authority validation. |

### Round70 command results

All commands used `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`; selected test targets used `--test-threads=1`. Free space stayed above 2 GiB (last measured 8.5 GiB).

| Lane | Log and result |
|---|---|
| H enabled | `b1-add68-H-final.log`; `foundry-line --features p2-test-support --test b1_product_results`; exit0, 9 passed/0 failed/0 ignored. |
| HX enabled process barriers | `b1-add68-HX.log`; exact Cargo JSON prebuild selected one `foundry_ledger` lib test executable at `/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398`; `B1_LEDGER_TEST_EXE` supplied to `b1_product_result_processes`; exit0, 5 passed/0 failed/0 ignored in 225.40s. |
| L | `b1-add68-L.log`; ledger `b1_product_results`; exit0, 4 passed/0 failed/0 ignored. |
| Enabled HTTP artifact consumer | `b1-add68-S-enabled.log`; serve `--features p2-test-support --test b1_product_artifacts`; exit0, 1 passed. |
| Enabled approval and scope CLI | Initial compile failure retained in `b1-add68-C-enabled.log` (two callers of the new optional failed-result fixture were fixed to unwrap only the successful positive fixture). Rerun `b1-add68-C-enabled-rerun.log`: approval CLI 2 passed; scope CLI 1 passed and 4 loopback tests failed with sandbox `Operation not permitted`; exit101. These failures require parent rerun and are not green. |
| All-feature library lane | `b1-add68-allfeatures-libs.log`; ledger/line/serve `--all-features --lib`; 39+9+43 passed, zero failed/ignored; exit0. |
| All-feature executable lane | `b1-add68-allfeatures-foundry-bin.log`; foundry binary; 5 passed, zero failed/ignored; exit0. |
| No-default feature isolation | `b1-add68-nodefault-{ledger,line,serve,foundry,foundry-examples}.log`; four package checks plus foundry examples; all exit0. |
| Default core | `b1-add68-core-default.log`; four-crate `--lib --tests`; stopped with exit101 at line `b1_product_restore`: 8 passed, 2 failed. Both failures are the exact service-less missing-authority cases described above. Per ADDENDUM67, watch/default-CLI chain was stopped after this core failure. |
| Clippy | `b1-add68-clippy.log`; affected packages, `--all-targets --no-deps`; exit0. Existing warnings were reported by Cargo/Clippy, including 51 ledger lib-test warnings; no warning denial was enabled. |
| Format / whitespace | `b1-add68-format-and-diff-final.log`; rustfmt check on the five changed Rust paths and `git diff --check`; both exit0. Formatting was limited to those paths. `graphify` was not found on PATH; no graph report was fabricated. |

### Round70 inventory and retained evidence

Relative to the pre-edit content/hash/status snapshot `/home/cosmo/.hermes/cache/scratch/b1-add68-before-20261020T050921Z` (status SHA256 `84d9a4c0fe1c642b032bb358cc3005a9c47d2208589b8ab60cbc83c13c9e3d7a`; all-worktree inventory SHA256 `b3b56272ee366080d686999faa001f35fa0085aeaae88d4d1454c6257f55085f`), changed workspace paths are:

- `crates/foundry-ledger/src/product/result.rs`, `crates/foundry-ledger/src/product/review.rs` — authorized production files.
- `crates/foundry-line/tests/b1_product_results.rs`, `crates/foundry-line/tests/support/product_result_fixture.rs`, `crates/foundry/tests/b1_approve_cli.rs` — whitelisted tests/fixture; CLI callers now explicitly unwrap a successful fixture result.
- `crates/foundry-line/tests/b1_product_restore.rs` — targeted formatting only; speculative fixture adaptation was reverted after its feature and assertion probes failed. Current default core failure remains the preserved service-less missing-authority case.
- `.hermes/plans/b1-w3-recovery-acceptance-matrix.md` — only the approved initial-redrive budget4→5 observation/provenance changed.
- This evidence file — ADDENDUM68 measurements and current gaps appended; earlier evidence remains intact.

No workspace files were created or deleted. New scratch outputs are `b1-add68-H-final.log`, `b1-add68-HX.log`, `b1-add68-C-enabled.log`, `b1-add68-C-enabled-rerun.log`, `b1-add68-S-enabled.log`, `b1-add68-allfeatures-libs.log`, `b1-add68-allfeatures-foundry-bin.log`, `b1-add68-nodefault-{ledger,line,serve,foundry,foundry-examples}.log`, `b1-add68-core-default.log`, `b1-add68-product-restore-rerun.log`, `b1-add68-product-restore-p2.log`, `b1-add68-product-restore-p2-rerun.log`, `b1-add68-restore-hold-filter.log`, `b1-add68-redrive-exact-GREEN.log`, `b1-add68-redrive-exact-GREEN-rerun.log`, `b1-add68-clippy.log`, `b1-add68-format-and-diff.log`, and `b1-add68-format-and-diff-final.log`; the historical RED, L, initial H and JSONL/executable evidence remains retained. Test-created isolated SQLite/artifact roots remain under the configured scratch directory. Cargo reused only the existing ignored `target/`; no `/tmp` output was created. Parent independent verification remains required.
# Round 80 — ADDENDUM77 section 3 residual rubric work (implementer evidence; parent verification pending)

Scope was limited to ADDENDUM77 section 3. G22 completed reclaim and G23 observation hook were left untouched and remain parent-verified from round79 (nine lanes, H/HX 31/0). No production runtime file changed; the only `result.rs` edit is inside its existing `#[cfg(test)] result_wire_tests` module. No commit or push was made.

## Gap disposition

| Gap | Round80 status | Evidence / remaining obligation |
|---|---|---|
| G05 | PASS | Exact `i64::MAX+1` width boundaries for attempt/run/activation, persisted negative coordinates, and replay-attempt rejection covered in H and ledger wire unit tests. |
| G06 | N/A | No G06 row exists in rubric v1. |
| G07 | PASS, inherited | Exact typed digest / legacy history byte obligations remain covered by ledger integration tests; this round's L lane passed 4/4. This is not a faithful-v2 host fixture claim. |
| G08 | PARTIAL | Added nested malformed authority/accepted-hold/artifact wire cases and genuine stored payload/receipt version, kind, and noncanonical corruption with byte restoration. Full per-member digest/byte and all nested negative matrix still needs parent audit. |
| G09 | PARTIAL | Real serialized EmitReceipt and PromotionReceipt substituted into a genuine stored result are rejected as CorruptEvidence and restored. Fallback approval/consumption no-effect assertions are not fully connected to both independent cases. |
| G10 | PARTIAL | Existing H ancestry negatives remain; this round did not fill the full independent clean-baseline matrix for inspected commit, foreign fix artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot/receipt, and coordinates. |
| G11 | PARTIAL | Existing A continuation plus new exercised assertions remain green, but exact frozen paths/session/script inputs, fixed progress coordinates, both-host full producer census, and origin/reservation mismatch set are not complete. |
| G12 | PARTIAL | Existing B/direct CommitPass evidence remains green. Complete boundary deltas, successor table census/allocator, and CommitFail/Abort ineligible checks remain open. |
| G13 | PARTIAL | `result_location_must_resolve_registered_terminal_evidence` now uses genuine successful controls, isolated fallback/foreign-stage/external-path mutations, exact InvalidEvidence, no-effect census, restoration, and re-finalization. The distinct malformed terminal-path case remains unmeasured. |
| G14 | PASS | `held_failed_and_materialized_continuation_are_not_final_results` exercises a live-authority genuine run8 resuming state and directly asserts Ineligible and no result/tuple change. |
| G15 | PASS, inherited | Existing queued/waiting controls and zero-S1 behavior were preserved; the full H lane passed. |
| G16 | PARTIAL | `activation_authority_insert_failure_rolls_back_the_complete_admission_set` injects authority insertion failure and checks transactional rollback of the measured admission/activation set. Resume-copy corrected numeric/text query runs in the full core restore lane. Progress-row and every requested participant linkage are not all measured in the new rollback baseline. |
| G17 | BLOCKED / GAP | No faithful historical-v2 host fixture was produced. The existing migration test opens fresh v3 then downgrades/rewrites schema and is explicitly not accepted as this proof. The allowed test/fixture paths expose no historical host database bytes or supported v2 host opener to preserve; constructing the fixture from fresh-v3 would violate the row. Missing: historical host/descendant with original history bytes, plus modern missing-initial/missing-copy contrasts. |
| G18 | PASS | Existing `completed_initial_result_finalizes_after_worker_lease_expiry_without_renewal` includes first success and replay on the expired-worker state, with the exact stored receipt and unchanged claim/result binding; H passed 30/30. |
| G19 | GAP | No completed-pending initial/A/B authority-change table with finalizer and lookup assertions was added. Existing continuation-entry rejection evidence is not substituted for finalizer rejection. |
| G20 | GAP | No genuine two-process stage-start-commit → revoke/rebind commit → accepted completion → strict result-finalizer refusal test was added. HX's five process tests passed but cover other result/approval process boundaries. |
| G21 | PARTIAL / GAP | Revocation continuation test now measures pre-stage durable counts, executor counters, and finalizer refusal, but authorization is rejected before a real stage-start transaction is attempted. |
| G22 | PASS, parent-verified round79 | Completed reclaim and four-vector evidence were not touched this round. |
| G23 | PASS, parent-verified round79 | Observation hook and charge rows were not touched this round. |
| G24 | PARTIAL / GAP | Followup-run finalizer refusal and recovery/T/R observations were added; the revocation pause remains after stage completion rather than during `replan_full-g1`, so the required same-baseline `5/1/1`, `5/5/4/5` execution proof remains open. |
| G25 | PASS, inherited | Existing interleaved-order evidence remains unchanged; H passed. |
| G44 | PARTIAL | `p2_preparation_walks_s1_without_result_seeding` now proves duplicate post-claim/host preparation fails without durable changes. Duplicate preclaim, stale/invalid admission/source, and conditional update 0-row rollback cases remain open. |

Rubric-external suggestions: none added. No production defect predicate was exposed by the added tests.

## Regression lanes run

All Cargo invocations used `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`, and serial test threads. Disk guard was checked before the regression chain (7.1 GiB free, above 2 GiB).

| Lane | Result and retained log |
|---|---|
| H | 30 passed, 0 failed, 0 ignored; [b1-round80-H.log](/home/cosmo/.hermes/cache/scratch/b1-round80-H.log). |
| HX | 5 passed, 0 failed, 0 ignored; exact Cargo JSON prebuild handoff at [b1-round80-ledger-json.jsonl](/home/cosmo/.hermes/cache/scratch/b1-round80-ledger-json.jsonl), parser stderr [b1-round80-ledger-build.stderr](/home/cosmo/.hermes/cache/scratch/b1-round80-ledger-build.stderr), executable [b1-round80-ledger-test-executable.txt](/home/cosmo/.hermes/cache/scratch/b1-round80-ledger-test-executable.txt); [b1-round80-HX.log](/home/cosmo/.hermes/cache/scratch/b1-round80-HX.log). This did not satisfy G20. |
| Ledger | Integration 4/0/0 and private wire unit subset 4 passed, 0 failed, 35 filtered; [b1-round80-ledger-results.log](/home/cosmo/.hermes/cache/scratch/b1-round80-ledger-results.log), [b1-round80-ledger-wire.log](/home/cosmo/.hermes/cache/scratch/b1-round80-ledger-wire.log). |
| HTTP enabled | 1 passed, 0 failed, 0 ignored; [b1-round80-http-enabled.log](/home/cosmo/.hermes/cache/scratch/b1-round80-http-enabled.log). |
| Restore default | 10/0/0; [b1-round80-restore-default.log](/home/cosmo/.hermes/cache/scratch/b1-round80-restore-default.log). |
| Restore enabled | 10/0/0; [b1-round80-restore-enabled.log](/home/cosmo/.hermes/cache/scratch/b1-round80-restore-enabled.log). |
| CLI enabled | Approval target 2/0/0; [b1-round80-cli-enabled-approve.log](/home/cosmo/.hermes/cache/scratch/b1-round80-cli-enabled-approve.log). Scope CLI loopback target not run because this sandbox denies loopback binds; parent rerun required. |
| Core default | Four-crate `--lib --tests`: 406 passed, 0 failed, 0 ignored across 39 test binaries; [b1-round80-core-default.log](/home/cosmo/.hermes/cache/scratch/b1-round80-core-default.log). |
| Watch default | Combined polling/intake lane: polling 0 passed, 8 failed at loopback bind (`Operation not permitted`); stop occurred at first test binary. Separate intake durability: 9/0/0. Logs: [combined](/home/cosmo/.hermes/cache/scratch/b1-round80-watch-default.log), [intake](/home/cosmo/.hermes/cache/scratch/b1-round80-watch-intake.log). Parent rerun of polling is required. |
| CLI default | Required five-target chain: approval 1/0/0, auth 1/0/0, scope CLI 1 passed and 4 failed because loopback bind was denied (`Operation not permitted`); chain stopped there. Independent remaining service/watch CLI targets passed 1/0/0 each. Logs: [chain](/home/cosmo/.hermes/cache/scratch/b1-round80-cli-default.log), [independent remainder](/home/cosmo/.hermes/cache/scratch/b1-round80-cli-default-independent.log). Scope CLI parent rerun required. |

## Round80 file and scratch inventory

Changed this round: `crates/foundry-line/tests/b1_product_results.rs`; test-only private wire additions inside `crates/foundry-ledger/src/product/result.rs`; this evidence document; and the round80 entry in the recovery matrix. No other allowed test path changed. In particular, `b1_product_result_processes.rs`, `support/product_result_fixture.rs`, ledger integration tests, serve artifact tests, and CLI test files are unchanged from the before-edit snapshot. No `src/` runtime path was edited. Baseline snapshots and hashes are retained under `/home/cosmo/.hermes/cache/scratch/b1-round80-baseline/`; logs, Cargo JSON, stderr, and executable handoff are under `/home/cosmo/.hermes/cache/scratch/` with `b1-round80-` prefixes. No files were written under `/tmp` and no temporary artifacts were deleted.

`git diff --check` passed. Targeted `rustfmt --check --edition 2021` on the touched H test file and private wire file returned nonzero because those existing untracked files contain broad pre-existing formatting differences (including unrelated imports and old test blocks); no broad formatting rewrite was applied. Scratch free space remained 7.1 GiB.

# Round81 — ADDENDUM78 final implementation evidence (parent verification pending)

This round followed ADDENDUM78 only. Production runtime files were not edited. The new HX case uses a spawned copy of its own test binary sharing the same SQLite database; no thread is used as the cross-process substitute. Rubric-external implementation suggestions were not added.

## Gap disposition

| Gap | Round81 status | Evidence / remaining obligation |
|---|---|---|
| G08 | PARTIAL | H independently changes a stored authority revision, artifact digest, and accepted-hold receipt bytes; each rejection is `CorruptEvidence` with persisted result/participant state checked and original bytes restored. The exhaustive member-by-member nested digest/byte mutation matrix is not complete. |
| G09 | PASS | The genuine stored result is independently replaced with serialized `EmitReceipt` and `PromotionReceipt`; both lookup and strict finalizer reject each as `CorruptEvidence`. Tuple/result/outcome/token-consumption/stage/payload state is measured unchanged, and the exact stored receipt is restored. |
| G10 | PARTIAL | Existing genuine ancestry and path controls remain. The independent clean-baseline matrix still lacks some inspected-commit/foreign-fix/host-journal/active-key/progress/snapshot/coordinate mutations. |
| G11 | PARTIAL | Existing A/B continuation counts, artifact ordering, hold receipts, progress and acceptance evidence remain green. Full exact path/session/script inputs, both-host census and all origin/reservation mismatches are not yet measured. |
| G12 | PARTIAL | Existing B/direct CommitPass evidence remains green; complete boundary deltas, successor-table/allocator census, and CommitFail/Abort ineligible controls remain open. |
| G13 | PARTIAL | Existing location cases have clean controls, exact `InvalidEvidence`, no-effect counts, restoration and successful finalization. The distinct malformed terminal-path case remains unmeasured. |
| G16 | PARTIAL | Activation authority insert rollback and Resume copy rollback/zero-copy controls pass. The new rollback baselines still do not measure every requested progress row and participant linkage together. |
| G17 | PARTIAL / GAP | H now contrasts modern missing initial authority and missing Resume copy (`CorruptEvidence`) with restored positive controls. No faithful historical-v2 host/descendant fixture was made, so legacy missing-snapshot `Ineligible` remains unmeasured. The only existing migration fixture in `b1_scope.rs` derives its v1 state by rewriting a fresh-v3 database; using it as the requested historical origin would violate the addendum. No independent historical host bytes/fixture are available in the repository. |
| G19 | PARTIAL / GAP | Existing initial service/credential/source and lineage scope evidence remains. Completed-pending initial/A/B independent authority-change matrices with finalizer plus lookup, and lineage human-account/ResolveReviewHold changes, remain incomplete. |
| G20 | PASS | `two_process_stage_start_then_service_revocation_preserves_completed_result_and_blocks_finalizer` uses a real child test process on the same DB. The child writes its marker from the executor only after `replan_full-g1` stage-start commit; the parent commits service revocation before releasing child completion. The completed stage/artifact/session and recovery-required state are preserved, and strict run-8 finalization returns `Unauthorized` with T/R 0/0. |
| G21 | UNMEASURED | The existing pre-stage case rejects during accepted continuation materialization: its durable observation remains one run and zero follow-up stage rows. The fixture exposes only a pause inside `StageExecutor::execute`, which is after stage-start commit. Test/fixture-only code cannot observe a pre-start pause inside `PackReviewHost`; production runtime is frozen. This exact boundary remains for parent disposition. |
| G24 | PASS | `revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next` pauses during `replan_full-g1` execution. Same-baseline observations are execute/restore/dispatch `5/1/1`, cumulative stages/artifacts/sessions/budget `5/5/4/5`, follow-up `resuming`, durable `recovery_required`, strict follow-up finalizer refusal, and T/R `0/0`. The third count is `dispatch_count`; refusal is asserted separately. |
| G44 | PARTIAL | Duplicate post-claim preparation rejection and no-effect state remain covered. Duplicate preclaim, stale/invalid admission/source, and conditional update 0-row rollback obligations are still open. |

Previously covered G23 charge probes also passed in H, including direct active-run charge checks and the lock/expiry observation case. Other rubric rows were not expanded beyond ADDENDUM78 obligations.

## Regression lanes

All Cargo commands used `--locked --offline`, one build job, incremental compilation disabled, and the target directory under `/home/cosmo/.hermes/cache/scratch`. The ledger unit-test executable was extracted from Cargo JSON before HX and passed through `B1_LEDGER_TEST_EXE`. No test source writes to `/tmp` were observed; test fixtures use the configured scratch path.

| Lane | Result |
|---|---|
| H | 30 passed, 0 failed. |
| HX | 7 passed, 0 failed, including the new same-database child process case. |
| Ledger | Covered in the four-crate core invocation: ledger unit 39, `b1_product_atomic` 15, `b1_product_results` 4, `b1_scope` 4, `b1_service_authority` 4, and `ledger_integrity` 6 all passed (72 total). |
| HTTP enabled | 1 passed, 0 failed. |
| Restore default | 52 passed, 0 failed. |
| Restore enabled | 10 passed, 0 failed. |
| CLI enabled | Approval 2 passed. Scope CLI 1 passed; 4 loopback-dependent tests were blocked by sandbox `Operation not permitted`. Parent rerun required. |
| Core default | Four-crate `--lib --tests` completed with passing target summaries (406 passed, 0 failed across the same pre-existing matrix). |
| Watch | Intake durability 9 passed. All 8 GitHub polling tests were blocked at local listener bind with `PermissionDenied (Operation not permitted)`; parent rerun required. |
| CLI default | Approval, auth, watch, and service targets passed 1 each. Scope CLI 1 passed and 4 loopback-dependent cases were blocked with `Operation not permitted`; parent rerun required. |

The loopback failures were isolated to test listeners and are environmental, as allowed by ADDENDUM78. No unrelated test failure was observed. Core target summaries ended without a failure; their consolidated process exit code was not retained by the tool session, so the parent should rerun the prescribed core command as part of independent verification.

## Round81 workspace and scratch inventory

Files modified by this round:

- `crates/foundry-line/tests/b1_product_results.rs`
- `crates/foundry-line/tests/b1_product_result_processes.rs`
- `.hermes/plans/b1-w3-acceptance-evidence.md`

No new workspace files or deleted files were created. Scratch outputs created: `/home/cosmo/.hermes/cache/scratch/b1-add78-ledger-lib-build.json` (Cargo JSON prebuild handoff) and `/home/cosmo/.hermes/cache/scratch/b1-add78-target/` (Cargo build artifacts). Test-created fixture directories were cleaned by their tests. No commit or push was made.

The scratch filesystem had 7,480,208 KiB free before the regression chain and 1,453,380 KiB free after it. Since the final measurement is below the 2 GiB guard, no further build or test command was started.

# Round82 — ADDENDUM79 final implementation evidence (parent verification pending)

This round followed ADDENDUM79 only. Existing dirty paths were treated as the round baseline. No host algorithm, schema, or unrelated production code changed. The single production edit is an observation-only test-support signal at the real continuation stage-start boundary.

## Final per-gap status

| Gap | ADDENDUM79 status | Evidence / remaining disposition |
|---|---|---|
| G08 | STOPPED — production predicate defect; matrix incomplete | Added independent payload-member mutations for all eight authority members and all eighteen artifact members, plus member-by-member accepted-hold receipt mutations. The first accepted-hold `actor_id` mutation changed the stored receipt bytes and finalization returned `StaleBinding`; the rubric requires `CorruptEvidence` for malformed stored evidence. Exact failing assertion: `accepted_resume_binds_three_generations_and_fourteen_artifacts`, G08 accepted-hold `actor_id` member, `b1_product_results.rs:3790`; observed `left=Err(StaleBinding)`, expected `Err(CorruptEvidence)`. Per instruction, the G08 item stopped and production was not changed. Later accepted-hold members remain unmeasured. |
| G10 | GAP | The requested independent clean-baseline matrix for inspected commit, foreign fix artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot/receipt, and coordinates was not completed. Existing ancestry evidence remains partial. |
| G11 | GAP | The full frozen paths/session/script inputs, fixed progress coordinates, both-host producer census, and origin/reservation mismatch set were not completed. Existing A/B evidence remains partial. |
| G12 | GAP | Complete CommitPass boundary deltas, successor table census/allocator, and CommitFail/Abort ineligible cases were not completed. |
| G13 | PASS for requested distinct malformed terminal-path case | Added an independent `../registered/../../malformed-terminal.log` case alongside the existing independently controlled fallback, foreign-stage, and external-path cases. The exact test passed and retained `InvalidEvidence`, unchanged evidence census, byte restoration, and successful re-finalization. |
| G16 | PASS for rollback baseline obligations | The activation-authority insert rollback now measures the same isolated baseline's progress rows, host acceptance/run/work-binding/intent/active/authority/reservation participants, budget/stages/sessions, tuple/result rows, and S1/transition-receipt anchor. A test-query mistake was corrected: hold progress is keyed by operation, not order. The focused test passed. |
| G17 | UNMEASURED only for legacy missing-snapshot `Ineligible` | Modern missing-initial and missing-copy `CorruptEvidence` contrasts are covered by existing H evidence. A faithful migrated v2 host/descendant with preserved original history bytes remains impossible from repository inputs: `migrate_scope_v1` accepts only an existing `ProductAtomicV1` row with `product_meta_v1.schema_version=1`; normal fresh `ProductStore::open_with_clock` creates schema version 3 and refuses v1/v2. The only repository migration fixture, `b1_scope.rs::v1_maintenance_migration_is_explicit_atomic_and_quarantines_unknown_orders`, creates v3 then drops v3 tables and rewrites the version to 1, which ADDENDUM79 explicitly disallows. No historical host/descendant database or original history bytes exist in the repository, and hand-created host rows would not be production-produced historical evidence. Test-only migration exposure cannot recover those missing bytes. |
| G19 | GAP | Completed-pending initial/A/B service, credential, source, and revoke/regrant matrix with finalizer and lookup assertions was not completed. The continuation lineage scope mutation was changed to exercise `ResolveReviewHold`, but that later assertion was not reached in the H lane because the earlier G08 production defect stopped its test. Human account revision and the full A/B table remain unmeasured. Initial unrelated-human allow remains covered by existing H0 evidence. |
| G21 | PASS | Added `StageStartBeginAttempt` to the existing test-support observer enum and emitted it immediately before `continuation_budget_and_start` begins its transaction. The observer is compiled only under `cfg(test)` or `p2-test-support`, has no decision inputs, and production builds are unchanged. The test arms the signal for a real accepted continuation, waits at the boundary, commits service revocation through the existing owner API, then releases stage-start. It asserts the exact revoked-service error, no new stage/session/artifact/tuple/result rows, preserved prior evidence, unchanged execute/restore/dispatch counts, recovery state, and finalizer refusal. Focused test: 1 passed. |
| G44 | GAP | Duplicate preclaim, stale/invalid admission/source, and conditional update zero-row rollback cases were not completed. Existing duplicate post-claim preparation remains the prior partial evidence. |

Rubric-external suggestions: none. No production fix was made for the G08 defect.

## Focused checks and nine-lane regression status

Commands used `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_TARGET_DIR=/home/cosmo/.hermes/cache/scratch/add79-target`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`, and serial test threads. Scratch free space was 31.8 GiB after the target rebuild, above the 2 GiB floor.

| Check / lane | Result |
|---|---|
| G21 focused | Exit 0, 1 passed; `/home/cosmo/.hermes/cache/scratch/add79-g21-focused-final.log`. Earlier hook-placement probe logs are retained; the first placement observed ordinary stage starts and was moved to the actual continuation transaction. |
| G16 focused | Exit 0, 1 passed; `/home/cosmo/.hermes/cache/scratch/add79-g16-focused-rerun.log`. The initial wrong-column probe is retained in `add79-g16-focused.log`. |
| G13 focused | Exit 0, 1 passed; `/home/cosmo/.hermes/cache/scratch/add79-g13-focused.log`. |
| G08 initial-result member matrix | Exit 0, 1 passed before the accepted-hold member extension; `/home/cosmo/.hermes/cache/scratch/add79-g08-focused.log`. |
| H/HX | H was interrupted at the first unrelated failing test after the G08 defect: 13 completed passed, 2 completed failed, the next test was interrupted, and the remaining tests were not run. H log: `/home/cosmo/.hermes/cache/scratch/add79-H.log`. The G08 failure's exact assertion is above. The unrelated failing test was `completed_result_rejects_reclaimed_and_same_owner_aba`; the process was stopped before its panic detail was flushed, and it was not rerun. HX was not run. |
| Ledger result/wire | Not run; stopped at the first unrelated H failure. |
| HTTP enabled | Not run; stopped at the first unrelated H failure. |
| Restore default | Not run; stopped at the first unrelated H failure. |
| Restore enabled | Not run; stopped at the first unrelated H failure. |
| CLI enabled | Not run; stopped at the first unrelated H failure. |
| Core default | Not run; stopped at the first unrelated H failure. |
| Watch | Not run; stopped at the first unrelated H failure. |
| CLI default | Not run; stopped at the first unrelated H failure. |

`git diff --check` passed. `rustfmt --check --edition 2024` returned 1 because the pre-existing H and shared fixture files contain broad formatting differences beyond this round; no workspace-wide or unrelated formatting rewrite was applied. `graphify-out/GRAPH_REPORT.md` was absent and `graphify` was unavailable, so no graph report was generated.

## Round82 workspace and scratch inventory

Changed from the pre-edit hash snapshot: `crates/foundry-ledger/src/product/review.rs`, `crates/foundry-line/tests/b1_product_results.rs`, `crates/foundry-line/tests/support/product_result_fixture.rs`, this evidence file, and the Round82 entry appended to `b1-w3-recovery-acceptance-matrix.md`. No G17 migration source/test path was changed. No new or deleted workspace files, commit, or push.

Round82 logs, before hashes/status, final status, inventory, and the rebuilt Cargo target are under `/home/cosmo/.hermes/cache/scratch/` with `add79-` prefixes. Failed focused probes and test-created SQLite roots remain there; no broad cleanup was performed. The full scratch filename inventory is `/home/cosmo/.hermes/cache/scratch/add79-scratch-inventory.txt`.

## Round83 — ADDENDUM80 final implementation evidence (parent verification pending)

### Changes and G08 result

`completed_result_ancestry_in` now loads the durable acceptance actor, request key, and request digest alongside the stored receipt. It rejects a receipt whose `actor_id` differs from the immutable `product_review_hold_acceptances_v1.actor_id` column, and checks the receipt members covered by the immutable acceptance request digest before calling `ensure_current_acceptance_authority_in`. The latter remains unchanged and still reports genuine account/scope authority revocation as `StaleBinding`. Receipt `accepted_at` must remain a canonical RFC3339 timestamp. No schema or public API changed.

The full G08 accepted-hold member matrix passed: all 19 receipt members, including `actor_id`, independently return `CorruptEvidence`; the existing authority and artifact member matrices also passed in the same H test. Focused H log: `/home/cosmo/.hermes/cache/scratch/add80-g08-focused.log` (exit 0, 1 passed). The focused test exposed a fixture restoration defect after the negative matrix: its `ResolveReviewHold` grant mutation restored the subject revision but not the grant revision. The fixture now restores both revisions before its positive finalization control.

The interrupted `completed_result_rejects_reclaimed_and_same_owner_aba` test failed in Round82 at the same G08 `actor_id` receipt assertion, invoked by its shared continuation helper. It was not a reclaim predicate failure and did not come from the G21 observer. After the classification fix, the focused four-vector test passed (A/B × different-owner/same-owner ABA; 1 passed, 73.14s; `/home/cosmo/.hermes/cache/scratch/add80-aba-focused.log`) and the complete H lane passed. No further change to that reclaim test was needed.

### Final gap status

| Gap | ADDENDUM80 final status | Evidence / remaining work |
|---|---|---|
| G08 | PASS for requested member classification | H test independently mutates all 8 authority members, 19 accepted-hold receipt members, and 18 artifact members; every malformed member is rejected as `CorruptEvidence`, including `actor_id`. Genuine current-authority revocation retains `StaleBinding`. Parent verification pending. |
| G10 | GAP | The independent clean-baseline matrix is still incomplete for inspected commit, foreign fix artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot/receipt, and coordinates. Existing H negatives remain partial. |
| G11 | GAP | Full frozen path/session/script inputs, fixed progress coordinates, both-host producer census, and origin/reservation mismatch cases remain incomplete. |
| G12 | GAP | Full CommitPass boundary deltas, successor-table/allocator census, and CommitFail/Abort `Ineligible` controls remain incomplete. |
| G13 | PASS, inherited | Distinct malformed terminal path and controls passed in Round82. |
| G16 | PASS, inherited | Combined progress/participant/S1 rollback baseline passed in Round82. |
| G17 | UNMEASURED only for legacy missing-snapshot `Ineligible` | One source-based construction attempt stopped before creating a database: `migrate_scope_v1` consumes an existing v1 database and creates v2 scope schema; the repository contains no v1 DDL artifact or historical host/descendant database/bytes to extract or preserve. `open_with_clock` creates schema version 3. The only in-tree migration fixture starts from v3 and rewrites it to v1, which is expressly inadmissible. No retry or fresh-v3 downgrade was made. Modern missing-initial/missing-copy `CorruptEvidence` controls remain measured. |
| G19 | GAP | Completed-pending initial/A/B service, credential, source, and revoke/regrant finalizer+lookup matrix, human-account revision case, and full lineage human-change cases remain incomplete. The ResolveReviewHold negative and unrelated-human initial allowance remain covered. |
| G20 | PASS, inherited | Existing HX producer child-process revocation/completion test remains parent-verification pending. |
| G21 | PASS, inherited | Existing observation hook reached the actual stage-start transaction boundary and passed its assertions. |
| G22/G23 | PASS, inherited | Round79 parent-verified disposition retained. |
| G24 | PASS, inherited | Existing 5/1/1 and 5/5/4/5 assertions include separate finalizer refusal and durable recovery state. |
| G44 | GAP | Duplicate preclaim, stale/invalid admission/source, and conditional zero-row rollback cases remain incomplete; duplicate post-claim preparation remains measured. |

Rubric-external suggestions: none. No further implementation round is authorized by ADDENDUM80.

### Regression lanes

Commands used `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_TARGET_DIR=/home/cosmo/.hermes/cache/scratch/add80-target`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`, and serial test threads.

| Lane | Result | Log / blocker |
|---|---|---|
| H/HX | H 30 passed, 0 failed; HX 7 passed, 0 failed. | `add80-H.log`, `add80-HX.log`; HX executable was selected from `add80-ledger-lib.jsonl` Cargo JSON and handed off through `B1_LEDGER_TEST_EXE`. |
| Ledger | 4 integration + 4 private result-wire tests passed. The separate default ledger core suite passed 72 tests. | `add80-ledger-results.log`, `add80-ledger-wire.log`, `add80-core-ledger.log`. |
| HTTP enabled | 1 passed, 0 failed. | `add80-http-enabled.log`. |
| Restore default | 10 passed, 0 failed. | `add80-restore-default.log`. |
| Restore enabled | 10 passed, 0 failed. | `add80-restore-enabled.log`. |
| CLI enabled | Approval 2 passed. Scope CLI: 1 passed, 4 blocked at loopback bind with `Operation not permitted`. | `add80-cli-enabled-approve.log`, `add80-cli-enabled-scope.log`; parent rerun required for four socket cases. |
| Core default | Combined command stopped at foundry scope CLI: 1 passed and 4 loopback binds denied. Follow-up default packages passed: ledger 72, line 110, serve 82. Foundry unit/default CLI targets passed: 5 unit + approval 1 + auth 1 + scope 1 + watch 1 + service 1; scope had 4 additional loopback-blocked cases. | `add80-core-default.log`, `add80-core-ledger.log`, `add80-core-line.log`, `add80-core-serve.log`; only socket cases require parent rerun. |
| Watch | Intake durability 9 passed. GitHub polling: 0 passed, 8 blocked at listener bind (`PermissionDenied: Operation not permitted`). | `add80-watch-intake.log`, `add80-watch-polling.log`; all eight polling tests require parent rerun. |
| CLI default | Approval, auth, watch, and service targets passed 1 each. Scope target: 1 passed, 4 loopback binds denied. | Core log plus `add80-cli-default-watch.log`, `add80-cli-default-service.log`; parent rerun required for four scope socket cases. |

The sandbox-blocked cases are environmental, not product assertions; they are explicitly left for parent rerun. No unrelated test failure occurred. The focused compile/test correction initially had one compiler error from passing `self` in a free helper; the helper was corrected and the focused run then passed. `git diff --check` passed for tracked paths. `rustfmt --check --edition 2021` returned 1 because these pre-existing untracked H/result files and the transitive shared fixture contain broad formatting differences; no broad formatting rewrite was applied. The added Markdown section has no trailing whitespace; `git diff --no-index --check` reports pre-existing hard-break trailing spaces in earlier Round65 content of the untracked evidence file.

## ADDENDUM81 — Gate1 closure remediation evidence

G17 remains **UNMEASURED — 저장소에 historical v1 DDL/host bytes 부재로 구조적으로 측정 불가, Gate3 historical migration census에서 실측 예정**. No implementation attempt was made: the required historical DDL and original host bytes are absent, and constructing them from a fresh-v3 database would violate the frozen fixture rule.

Non-blocking follow-up: the finalizer samples its clock before it acquires `BEGIN IMMEDIATE` (`crates/foundry-ledger/src/product/result.rs:650`; frozen contract `b1-product-result-package-spec.md:330`). This remains outside Gate1's closure denominator.

Two requested assertions expose production predicates that are frozen by ADDENDUM81; both items were stopped without changing `src/`:

| Row | Assertion and observed predicate | Disposition |
|---|---|---|
| G10 | `crates/foundry-line/tests/b1_product_results.rs:4173`, `accepted_resume_binds_three_generations_and_fourteen_artifacts`: mutate only the persisted completed progress `progress_revision` from its frozen value to `+1`, then replay the same verified result. The finalizer returned `Ok(ProductResultInfo)` instead of the asserted `Err(CorruptEvidence)`. The completed progress revision is not validated on result replay. | STOPPED — production defect exposed; no production edit. |
| G12 | `crates/foundry-line/tests/b1_product_results.rs:3398`, `commit_fail_and_abort_continuations_are_ineligible_finalizer_controls`: after genuine CommitFail continuation, load the actual persisted terminal host result/digest and call the finalizer on its exact key. It returned `StaleBinding`, not the required `Ineligible`. The test stops in the CommitFail control; the Abort branch did not execute. | STOPPED — production defect exposed; no production edit. |

### Workspace and scratch inventory

This round changed only the allowed paths:

- `crates/foundry-ledger/src/product/result.rs`
- `crates/foundry-line/tests/b1_product_results.rs`
- `.hermes/plans/b1-w3-acceptance-evidence.md`
- `.hermes/plans/b1-w3-recovery-acceptance-matrix.md`

No new or deleted workspace files were created. Cargo artifacts and logs are under `/home/cosmo/.hermes/cache/scratch/add80-target/` and `/home/cosmo/.hermes/cache/scratch/add80-*.log` plus Cargo JSON/stderr/executable handoff files. No files were written under `/tmp`; test-created fixtures were cleaned by their tests. No commit or push was made. Parent independent verification and the frozen-rubric sol review remain pending.

## ADDENDUM81 final batch reconciliation

This final test/evidence-only batch changed the two line test files, the shared P2 fixture test file, and this evidence document. No `src/` file was edited; no commit or push was made.

| Row | Final status | Evidence and blocker |
|---|---|---|
| G02 | PASS | Claim/backlog state is measured separately after first finalization and after replay. |
| G03 | PASS | Persisted `product_review_bindings_v1.intent_json` is deserialized and compared to the expected `ExecutionIntent`. |
| G05 | PASS | Verified replay keeps caller attempt fixed while a separately mutated stored attempt is rejected; negative persisted coordinates cover tuple attempt/run/generation and artifact lifecycle/admission/attempt/generation/stage sequence. |
| G08 | PASS | Each of the 19 accepted-receipt member refusals compares the complete durable result-evidence snapshot to C0. |
| G10 | STOPPED — production defect | Clean-A `progress_revision` mutation at `b1_product_results.rs:4173` returned `Ok(ProductResultInfo)` instead of `Err(CorruptEvidence)`. This was the first failing predicate. Later active key/journal/epoch and remaining progress-coordinate cells did not all execute. No production repair was attempted. |
| G11 | PARTIAL | Fixed progress constants, CommitPass successor census, allocator replay invariance, and producer rows pass in CommitPass controls. The clean A ResumePass finalization path is blocked by the G10 progress-revision defect before the full payload/DB and two-host census assertions complete. |
| G12 | STOPPED — production defect | CommitPass boundary deltas, successor-table census, allocator invariance, and direct owner/ABA refusal controls pass. Genuine CommitFail at `b1_product_results.rs:3398` returned `StaleBinding` instead of `Ineligible`; execution stopped before Abort. No production repair was attempted. |
| G16 | PASS | Resume-copy abort and conditional zero-copy rollback compare work binding, persisted intent, active binding, full progress, and durable participants with the pre-copy state. |
| G17 | UNMEASURED — 저장소에 historical v1 DDL/host bytes 부재로 구조적으로 측정 불가, Gate3 historical migration census에서 실측 예정 | No implementation attempt was made. |
| G19 | PARTIAL | Pending service-grant/credential finalizer refusals and source-rebind lookup behavior pass. Completed-result human-account and full continuation-lineage finalizer/lookup cells in the shared helper are unreachable after its G10 failure. The pending-only service fixture has no result binding, so `None` from lookup is not counted as completed-result evidence. |
| G20 | PASS | HX executor counter observes one in-flight call and stays at one after child completion under revoked authority, proving next execute=0. |
| G22 | PASS | Public replacement reclaim verifies claim/backlog after finalizer refusal; direct CommitPass owner and same-owner epoch/fence ABA controls verify refusal after continuation. |
| G23 | PASS | Completed stale-run baseline, exact error variants, injected BEFORE UPDATE error, and post-write rollback assertions pass. No `is_err()` substitution is used for those error controls. |
| G44 | PASS | Before worker claim, duplicate P2 preparation returns the exact admission/fresh-work invariant and leaves prepared C0 unchanged. BEFORE UPDATE `RAISE(IGNORE)` forces a conditional zero-row backlog update; the exact CAS error is observed and S1/receipt/intent/backlog equal the pre-call snapshot. Existing stale/invalid admission and source-binding negatives pass. |

G10's replay mismatch also causes `completed_result_rejects_reclaimed_and_same_owner_aba` and `legacy_census_corrupt_reachable_acceptance_is_corrupt_evidence` to fail at the shared helper before later row-specific assertions. These are dependent failures of the same predicate, not additional production defects.

### Nine regression lanes

Commands used `TMPDIR=/home/cosmo/.hermes/cache/scratch`, `CARGO_TARGET_DIR=/home/cosmo/.hermes/cache/scratch/add80-target`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `--locked --offline`, and serial test threads. Where required, the ledger test executable was selected from Cargo JSON and passed with `B1_LEDGER_TEST_EXE`. Scratch free space remained above the 2 GiB guard.

| Lane | Result | Log / blocker |
|---|---|---|
| H/HX | HX 7 passed; H 27 passed, 4 failed. | `add81-lane-h-hx-final-final.log`. Failures: G10 clean-A replay, G12 CommitFail, and two dependent G10 shared-helper cases. |
| Ledger | 4 passed, 0 failed. | `add81-lane-ledger.log`. |
| HTTP enabled | 4 passed, 0 failed. | `add81-lane-http-enabled.log` (3 auth HTTP + 1 artifact). |
| Restore default | 10 passed, 0 failed. | `add81-lane-restore-default.log`. |
| Restore enabled | 10 passed, 0 failed. | `add81-lane-restore-enabled.log`. |
| CLI enabled | 6 passed; 4 loopback cases filtered for parent rerun. | `add81-lane-cli-enabled-sandbox-skip.log`. Unfiltered attempt `add81-lane-cli-enabled.log` hit four `Operation not permitted` bind failures. |
| Core default | 406 passed, 0 failed across 39 test targets. | `add81-lane-core.log`. |
| Watch | Intake durability 9 passed; 8 GitHub polling loopback cases blocked for parent rerun. | `add81-lane-watch-durability.log`; unfiltered polling attempt `add81-lane-watch.log` failed at listener bind with `PermissionDenied: Operation not permitted`. |
| CLI default | 5 passed; 4 loopback cases filtered for parent rerun. | `add81-lane-cli-default.log`. |

Parent rerun commands printed by the blocked tests are `cargo test --locked --offline -p foundry --test b1_scope_cli cli_http_helper`, `cargo test --locked --offline -p foundry --test b1_scope_cli`, and `cargo test --locked --offline -p foundry-watch --test b1_github_polling`. Bind failures were not counted as passing tests.

### Final inventory

Workspace paths changed by this batch: `crates/foundry-line/tests/b1_product_results.rs`, `crates/foundry-line/tests/b1_product_result_processes.rs`, `crates/foundry-line/tests/support/product_result_fixture.rs`, and this evidence document. No fixture data, production source, or spec file changed. The workspace already contained other dirty paths before this batch; they were preserved.

Scratch outputs created for this batch are in `/home/cosmo/.hermes/cache/scratch/`: `add81-prebuild.jsonl`, `add81-prebuild.stderr`, `add81-ledger-exe`, `add81-H.log`, `add81-lane-h.log`, `add81-lane-h-rerun.log`, `add81-lane-h-hx-final.log`, `add81-lane-h-hx-final-final.log`, `add81-lane-ledger.log`, `add81-lane-http-enabled.log`, `add81-lane-restore-default.log`, `add81-lane-restore-enabled.log`, `add81-lane-cli-enabled.log`, `add81-lane-cli-enabled-remainder.log`, `add81-lane-cli-enabled-sandbox-skip.log`, `add81-lane-cli-default.log`, `add81-lane-watch.log`, `add81-lane-watch-durability.log`, and `add81-lane-core.log`. Cargo artifacts used the existing `add80-target` scratch target. One rejected attempt pointed Cargo at `/home/cosmo/.hermes/cache/add81-target`; it failed before writing files. No files were written under `/tmp`.

Non-blocking follow-up remains: finalizer clock sampling occurs before `BEGIN IMMEDIATE` (`crates/foundry-ledger/src/product/result.rs:650`; frozen contract `b1-product-result-package-spec.md:330`). Parent independent verification and the single 14-row-scoped sol delta review remain pending.

## Round 87 — G10 first-finalization progress_revision vector adjudication (2026-10-21)

Parent instrumentation diagnosis (PDBG probes, removed after use; production file restored from `scratch/result.rs.bak-round86`): the three G10 failures at `b1_product_results.rs:4173` are first-finalization scenarios, not replays — the fixture stores no prior result row (28 stored-result lookups all missed; a single INSERT occurred at the end). A mutated `progress_revision` therefore passed first finalization because no independent durable original for `progress_revision` exists anywhere (receipt, reservation, review.rs completion path, s1 chain) — the value is only defined by the progress row itself at first finalization. Frozen rubric G10 enumerates "progress phase/owner/fence" mutations only, and the frozen v3 contract mentions `progress_revision` solely as a recorded payload field (`b1-product-result-package-spec.md:1127`).

Disposition (user approval Discord `1562410025484296274`, option A): the `progress_revision` first-finalization rejection vector in the shared mutation loop was a rubric-external assertion (luna84's over-reading of sol's "remaining coordinates") and was removed with a documenting comment at the site. Grounded vectors (`epoch`, `reserved_run_id`) are retained; phase/owner/fence checks and replay-side corruption protection (`validate_stored_hold_progress_in`, prior-payload comparison) are unchanged. This is a documented removal of a rubric-external assertion, not an assertion weakening within the frozen rubric.

New non-blocking follow-up: `progress_revision` has no independent durable original at first finalization; if the contract is ever amended to require tamper-evidence there, hold-completion must durably record the progress coordinates (production change, out of Gate1 scope).

## Round 88 — G10 active-key classification production defect fix (2026-10-21)

Round 87 h-hx exposed three failures at the `b1_product_results.rs:4203` active-`key_json` tamper vector: production returned `StaleBinding` where the rubric G10 row (and frozen contract `b1-product-result-package-spec.md:1231`) requires `CorruptEvidence` for recorded durable-vs-durable inconsistency. Parent adjudication (user approval, option A): `result.rs:786-793` admission block classified journal_run↔intent, epoch_row↔active_epoch, active_epoch↔intent.epoch mismatches — all caller-independent recorded-state inconsistencies — as `StaleBinding`. Fix applied by the parent directly (luna85/86 insertion-point misses had established the parent-fix principle): a new `active_own` query (`product_review_bindings_v1` ⋈ `product_review_active_v1` ⋈ `product_review_epochs_v1`, `WHERE b.key_json=a.key_json`) distinguishes rekey (row exists under another binding → `StaleBinding`) from orphan (nowhere → `CorruptEvidence`), reconstructs `own_intent` to classify durable-vs-durable inconsistency as `CorruptEvidence`, and leaves caller-coordinate mismatches (order/attempt/run/activation/requested key) as `StaleBinding` per contract line 974. The G12 `Ineligible` precedence (`result.rs:693`) precedes the insertion point and is preserved. Round 88 h-hx confirmed the 4203 vector fully passes.

## Round 88 — lookup ABA assertion corrected to contract semantics (2026-10-21)

Round 88 h-hx then surfaced the account-ABA lookup assertion at `b1_product_results.rs:4276` expecting `Err(StaleBinding)` from `lookup_product_review_result` after an account-authority ABA. Frozen contract line 1219 states lookup does not reapply new-finalization authority revisions, and line 332 defines proven absence in a consistent snapshot — so `Ok(None)` is the contract-conformant result. The luna84 assertion was an over-extension; corrected in the test to pin `Ok(None)` with the contract citation in a comment. The finalize-side ABA rejection (`:4272`) remains and passes, carrying the G19 obligation.

## Round 89–90 — G19 accepted_resume test-fixture state-restoration defects (2026-10-21)

Round 89 h-hx: 36 passed / 2 failed (`:4473` evidence-row snapshot mismatch, `:4510` `StaleBinding` unwrap). Parent diagnosis, all test-fixture defects, production unchanged:

1. **Snapshot timing (`:4473`)** — the `preserved` evidence-row snapshot was captured before `reclaim`, so reclaim's legitimate mutations (claim epoch 1→2, fence 1→4, attempts+1 — each directly asserted elsewhere at `:4455-4471`) appeared as diff. Snapshot capture moved after the "reclaim increments attempts once" assertion.
2. **Account ABA state not restored (`:4510`)** — the ABA probe advanced `product_accounts_v1.revision` via `set_account_authority` and left it; `ensure_current_acceptance_authority_in` (`review.rs:2156`, called from `result.rs:2767`) compares it against `receipt.authority_revision`. Fixture now captures `(role,revision,enabled)` before the ABA and restores after the scope section.
3. **Scope restore targeted captured values, not receipt values (`:4535`)** — after the account fix, parent instrumentation (PDBG4/PDBG5 probes at the `HostError→StaleBinding` mapping `result.rs:2767` and the scope-mismatch branch `review.rs:~2217`; both removed after use, production files restored) showed `current_scope=8 vs receipt.scope_revision=4`, `current_grant=2 vs receipt.grant_revision=1`: `product_scope_subject_revisions_v1.revision` is shared across capabilities, so later fixture grants advanced it past the acceptance receipt between acceptance and the probe. The restore UPDATEs now target `accepted.scope_revision`/`accepted.grant_revision` (the receipt's values, which the authority check compares) instead of the values captured just before `change_product_scope` (the capture remains only as the CAS expected-revision parameter).

All instrumentation removed (PDBG grep across `crates/` = 0); `result.rs` restored to the clean round-88 classification-fix state (backup `scratch/result.rs.bak-round89`). Single-test verification: `accepted_resume_binds_three_generations_and_fourteen_artifacts` 1 passed / 0 failed (`scratch/b1-dbg93.log`).

## Round 90 — full nine-lane regression after fixture corrections (2026-10-21)

Clean production state (PDBG instrumentation removed, `result.rs` at the round-88 classification-fix state restored from `scratch/result.rs.bak-round89`) with all five test corrections applied. Parent-run nine lanes, `--locked --offline --test-threads=1`, disk guard 2 GiB, `B1_LEDGER_TEST_EXE` from Cargo JSON prebuild:

| Lane | Result | Log |
|---|---|---|
| http-enabled | 4 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-http-enabled.log` |
| restore-default | 10 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-restore-default.log` |
| restore-enabled | 10 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-restore-enabled.log` |
| cli-enabled | 7 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-cli-enabled.log` |
| h-hx | 38 passed, 0 failed, exit 0 (H 31 + HX 7) | `scratch/b1-parent-round90-h-hx.log` |
| ledger | 4 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-ledger.log` |
| core | 406 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-core.log` |
| watch | 17 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-watch.log` |
| cli-default | 9 passed, 0 failed, exit 0 | `scratch/b1-parent-round90-cli-default.log` |

All lanes exit 0; no unrelated failures. Gate1 candidate evidence complete pending the 14-row-scoped sol delta review.

## Round 92–93 — ADD84 residual 8-gap remediation and full regression (2026-10-22)

sol round91 REJECT (5 PASS / 8 EFFECTIVE-GAP / 1 deferred G17) was adjudicated and remediated under user approval Discord `1562704527227101194` ("A진행") as ADD84: test-only batch closing G05 (verified-order replay actual-attempt mismatch that survives stored-set validation), G10 (inspected-commit mutation and registered foreign-fix-artifact negatives with clean baselines), G11 (per-generation exact paths/session IDs/script inputs, fixture executor `plan_full=="[]"` and single-triage-finding inspection, origin/reservation mismatch rejection), G12 (CommitPass boundary before/after stage/session/artifact/run deltas and reservation/allocator invariance, unused-successor full-table absence), G19 (completed-pending initial/A/B authority-change matrix with finalizer `StaleBinding` + lookup `Ok(None)`, initial unrelated-human allowed, scope probe reordered after account ABA restoration so the scope predicate is independently exercised), G22 (ordinary/direct stale-owner cases now make explicit strict finalizer calls with replacement-claim preservation), G23 (stale-run control from an active-running fixture, post-write SQL-injection rollback evidence), G44 (stale admission exercised before host completion with six-table before/after invariance, stale/invalid source-binding preparation negative).

luna92 (`scratch/b1-luna92.log`, exit 0) edited only `crates/foundry-line/tests/b1_product_results.rs` and `crates/foundry-line/tests/support/product_result_fixture.rs`. Parent scope verification: pre/post git snapshots identical (no tracked production file touched), `result.rs` byte-identical to `scratch/result.rs.bak-round89`, PDBG residue 0 across crates.

Round 93 parent nine-lane regression (all exit 0, `scratch/b1-parent-round93-*.log`):

| Lane | Result |
|---|---|
| http-enabled | 4 passed, 0 failed |
| restore-default | 10 passed, 0 failed |
| restore-enabled | 10 passed, 0 failed |
| cli-enabled | 7 passed, 0 failed |
| h-hx | 39 passed, 0 failed (H 32 + HX 7) |
| ledger | 4 passed, 0 failed |
| core | 406 passed, 0 failed |
| watch | 17 passed, 0 failed |
| cli-default | 9 passed, 0 failed |

H grew 38→39 with the ADD84 vectors; no prior assertion removed or weakened. Pending: 14-row-scoped sol delta re-review of the eight remediated rows.

## Round 94–96 — ADD85 remediation, production defect #5, and full regression (2026-10-23)

**sol round94 verdict: REJECT — 4 PASS (G10 G12 G22 G23) / 4 EFFECTIVE-GAP (G05 G11 G19 G44).**
All four gaps closed since:

- **G19 + G44** — closed by luna93 (ADD85, test-only, 2 files): completed-pending
  initial/A/B matrix cells and fresh-unclaimed source-binding baseline vectors.
- **G11** — sol's finding exposed **production defect #5**: `review.rs:298`
  (`hold_snapshot`) recorded the generation `snapshot.sequence` into the
  `resume_sequence` field, so every initial-run hold accept overwrote the resume
  counter with the generation count (4) and the host resumed as `resume-5`
  instead of the frozen contract's `resume-1` (spec line 879). PDBG6
  instrumentation proved both genuine increment sites never fired; the write
  came from the doc record. Fix (user approval "A 승인", same Discord anchor as
  ADD85): initial-run holds record `resume_sequence: 0` by definition (resumed
  holds persist the live counter through `complete_continuation`). dbg96
  single-run: `legacy_census…` 1 passed; session IDs regressed to `resume-1`,
  validating luna93's previously failing assertions (path per contract 825,
  `plan_full == "[]"`).
- **G05** — closed by a parent-written vector: new test
  `verified_replay_actual_attempt_mismatch_is_stale_binding`
  (b1_product_results.rs:105) with fixture support `with_g05_retry_ledger` /
  `append_g05_retry_chain` (product_result_fixture.rs:232/438). The vector
  appends a CHAIN-CONSISTENT retry history (DieStarted → DieFailed(1) →
  DieStarted(2) → DieSucceeded{2} → InspectionPassed) through the production
  durable machinery (intent row → ledger INSERT → trigger receipt, hash via the
  ledger's own `row_hash_at`), so `verify_rows` passes and replay derives
  actual attempt 2. Finalizing with the genuine attempt-1 caller coordinate is
  then rejected by the actual-attempt comparison itself
  (`result.rs:695` → StaleBinding) with zero binding rows written — no
  stored-set contamination, no CorruptEvidence shortcut (sol round91 H:832 /
  round94 H:765 findings addressed).

**Parent round96 nine-lane regression — all exit 0** (post defect-#5 fix,
ADD85 tests, G05 vector; instrumentation grep 0; `git diff --check` clean):

| lane | result |
|---|---|
| h-hx (b1_product_results + b1_product_result_processes) | 41 passed / 0 failed (incl. HX 7; +2 new vectors vs round93) |
| ledger | 4 / 0 |
| core | 406 / 0 |
| watch | 17 / 0 |
| http-enabled | 4 / 0 |
| restore-default / restore-enabled | 10 / 0 and 10 / 0 |
| cli-enabled / cli-default | 7 / 0 and 9 / 0 |

Production delta since the round94-reviewed state is exactly one approved
change: `review.rs:298` `resume_sequence: 0u64` + rationale comment.

**sol round95 verdict: APPROVE (exit 0; read-only verified — git status before/after identical).**
All four round94 EFFECTIVE-GAP rows now PASS with file:line evidence:
- G05 PASS — retry-chain fixture (product_result_fixture.rs:438) advances actual
  attempt to 2 with consistent event bytes and hashes; the new test
  (b1_product_results.rs:105) keeps caller attempt 1 and gets StaleBinding at
  result.rs:695 with zero result bindings — closes both shortcut findings.
- G11 PASS — session/generation-path assertions now require frozen resume-1;
  executor asserts literal "[]" and one triage finding.
- G19 PASS — continuation completes before the independent authority matrix;
  each cell asserts finalizer StaleBinding, lookup Ok(None), T/R 0/0.
- G44 PASS — source probe uses an acknowledged pre-preparation snapshot,
  asserts fresh unclaimed work, authenticates the rebound service, and
  verifies rejection preserves the baseline.

**Rubric v1 Gate1 status: 13/13 measured rows PASS; G17 deferred to the
Gate3 UNMEASURED wave (documented waiver). No outstanding rubric gaps.**

## Gate2 — consumer/root/integrity bundle (2026-10-23)

Frozen rubric: `.hermes/plans/b1-w3-gate2-plan-and-rubric-v1.md` (R1–R9), frozen by user approval Discord 1562994515545956506 ("1a 2a 3 승인"): defect #6 option A, CLI approve root (a) production-unchanged (contract line 1215 — approval consumes the validated stored set; file verification is confined to result reads per PT-10), rubric v1 freeze.

Chain: ADD86 (luna94, TEST-ONLY, b1_product_artifacts.rs + b1_auth_http.rs only; per-row audit ledger produced) → parent round97 (3 lanes; loopback real-socket C test PASSED on real TCP bind incl. login/metadata/artifact/grant/approve/exact replay/reopen replay; b1_scope_cli sandbox-bind failures resolved as sandbox-only; sole failure = artifact-SHA metadata cell) → adjudication: **production defect #6** — `read_product_result` (result.rs:1286) validated binding/tuple/host but no artifact rows/bytes, returning metadata 200 while the artifact set was corrupt (partial success; contract 548/1215). Parent fix (result.rs:1346+): every payload-bound artifact row (9-field equality) plus `read_confined_artifact` digest verified in the same snapshot; mismatch → CorruptEvidence (404), missing root → Storage (503). One test-only correction cycle (ancestor-replacement cell setup missing parent-dir creation). dbg102/dbg103 http lanes green → **parent round98: all nine lanes exit 0** — http 6/0 (+2 new tests: G-final stored-evidence mutations incl. artifact-SHA/provenance/path/file-bytes with metadata+artifact rejection and restore, PT-10 root/ancestor/FIFO guards; real-loopback-socket test), restore 10+10/0, cli 7/0, h-hx 41/0, ledger 4/0, core 406/0, watch 17/0, cli-default 9/0.

R8 measured by parent: external probe crate (scratch/p2-support-probe, path dep) — default features → E0432 ×3 (unresolved `P2ReviewPreparation`) + E0599 (`prepare_p2_review_fixture`), exit 101; `support` feature → compiles, exit 0.

Known host limitation: PT-10 device-leaf cell (mknod c 1 3) unmeasurable on this host (user lacks CAP_MKNOD; sudo requires interactive auth). The test's deny branch asserts no leftover artifact; full device measurement requires a CAP_MKNOD-capable host.

### Gate2 remediation cycle (2026-10-23, sol round99 → ADD87 → round100)
- sol round99 (proc 3799655): REJECT — 4 PASS (R1/R5/R8/R9) / 5 EFFECTIVE-GAP (R2 new-source metadata 404 only post-corruption; R3 file-byte cell artifact-route-only; R4 mknod-denied leaf left absent + ancestor cell no restored-200; R6 rubric text conflicted with contract 563; R7 unauthorized+file-byte combo absent). Defect #6 production fix itself unopposed.
- User approval "A" (Discord 1563076393028558919): R6 rubric amended in the frozen doc (success approval = exactly 1 consumption + 1 outcome row; read paths and refusal/retry/reopen add 0; anchor recorded in the rubric file).
- ADD87 (test-only, 2 serve test files): four cell extensions implemented by luna95 (proc 4064165, EXIT 0): intact-fixture new-source metadata 404; file-byte cell extended to metadata 404 + uniform foreign body; FIFO/mknod denied branches restore leaf + assert_clean; ancestor cell restored root + 200 + assert_clean; approval retry/reopen assert consumption/outcome stay 1/1 and publishing 0.
- Parent round100 (proc 4071715): all nine lanes exit 0 — http-enabled 6/0 (3+3), restore 10+10/0, cli-enabled 7/0, h-hx 41/0, ledger 4/0, core 406/0, watch 17/0, cli-default 9/0.

### Gate2 final adjudication (2026-10-23, sol round101→103)
- sol round101 (proc 4125698): REJECT — R2/R3/R6/R7 PASS, R4 EFFECTIVE-GAP (denied/unavailable FIFO+device branches restored without fail-closed measurement; conditional FIFO creation bypassed the rejection measurement). ADD87 patch integrity verified (no seeding/deletion-to-green; result.rs identical).
- User approval (Discord 1563123240056922133): explicit exception to the one-correction-cycle cap; ADD88 micro-batch authorized (test-only, one file).
- ADD88 (luna96, proc 55768, EXIT 0): all four platform-denied/unavailable branches now assert authenticated GET → 404 + no artifact prefix before restoring the leaf; success branches byte-identical.
- Parent round102 (proc 60024): http-enabled lane exit 0 (3/0 + 3/0), denied-device branch executed and passed the new assertions. Round100's other eight lanes remain valid (ADD88 touched only the PT-10 cell inside b1_product_artifacts.rs, exercised only in the http lane).
- sol round103 (proc 62667): **APPROVE — R4 PASS**. All four fallback branches measured fail-closed (FIFO:1363/1377, device:1411/1425) with assert_clean restoration; ADD88 audit: 32 inserted lines, zero deletions, no weakening. **Gate2 rubric R1–R9: all PASS.**
