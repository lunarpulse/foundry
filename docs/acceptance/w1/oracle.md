# W1 decision and pre-observation oracle, 22 November 2026

The product decision owner approved all three recommendations and their execution order in Discord message 1573876269811240971, as attributed by the user in the execution instruction. The approval applies to the revised roadmap in the main checkout (SHA-256 3286c9559861c897ea2c06fa6ba08a3b545990f4873511c4dc5640a2363ca58a). The accepted W1-b parent is 508fca10d67016b53e73174c8a01f597f1f2da1f. The 18-path W1 product/test/docs patch is d20ea8aa22a546a9d340e0c96c912a62a7f28eca994af3e90decb55b5d90f118. The original W0 freeze at 9056fa4, B1 D-PIN and D-LEGACY, the W14 pre-observation matrix and W15 private migration freeze remain the comparison baseline.

1. For two P07 cells only, accept independently intact candidate snapshot bytes while historical attribution is unknown. Missing run and provenance fields and absence of a usable commitment must fail closed for unbound new execution and new output registration. The two original literal positive snapshot predicates remain BLOCKED. A known run with a missing product field is a distinct control. Historical identification cannot repair the field.
2. Accept one admission per order ID for scoped W1. Preserve same-issue receipt on edit/reopen. Refuse a different issue under the occupied ID, fabricated lifecycle 2 and epoch bypass. Intake polling may persist delivery metadata. A genuinely new order with no prior run must use its own Owner-approved selection and live service claim for its first pin. A historical unbound order cannot acquire a retroactive pin this way. Defer both creation of a genuine same-ID successor and old-key/authority/fence replay against it to a future product decision and acceptance. The original same-ID positive remains BLOCKED.
3. Preserve the original P08 compound continuing predicate as FAIL. Permit completion of exactly the already-reserved sealed output and recovery progress on that same operation and fence. Each of the five standalone denials retains its own whole-database equality obligation.

## P07 operation-level oracle

Read requires verified S1 and current scope. Exact receipt retry requires the original authenticated actor, request and authority. Peer order read, peer operation denial, changed-body conflict and revoked actor denial are distinct. Every row includes active and held denials at both epochs 1 and 2. A resolved single-digest order with a fresh live claim can proceed at the higher epoch. For unavailable bytes exercise absent and tampered separately. Partial denotes loss of the historical service-to-work binding with the original worker fence retained. Each pair below gives active reserved collection / fresh stage on the original held Resume.

| History | Service binding | Intact snapshot | Unavailable snapshot |
|---|---|---|---|
| Single | Complete | Collect / complete | Collect / zero execution |
| Single | Partial | Collect / zero execution | Collect / zero execution |
| Mixed | Complete | Collect / complete on uniquely bound hold | Collect / zero execution |
| Mixed | Partial | Collect / zero execution | Collect / zero execution |
| None | Complete | Refuse / zero execution, intact unassigned bytes | Refuse / zero execution |
| None | Partial | Refuse / zero execution, intact unassigned bytes | Refuse / zero execution |

Successful collection requires the original reservation, stage, worker fence and sealed bytes. A current service revoke alone permits the bound output and bars the next stage. Read of an already registered artifact has its own authenticated scope. The reconstructed commitment must fail with another valid pack or foreign admission. Neither hold-state hash nor operation ID supplies a pack commitment.

## P08 operation-level oracle

Freeze the post-DDL/pre-release to post-release delta before new measurements. Compare schema SQL and typed row multisets in the same fixture, including the SQLite sequence. Bind the actor, source, admission, hold, numeric followup and original operation/owner/epoch/fence first. Require the immutable receipt bytes and reservation row throughout.

| Table | Sole permitted effect for the held fix stage |
|---|---|
| product_review_stages_v1 | Update one exact order/run/sequence/stage, accepted to complete and NULL output_json to the matching sealed evidence. |
| product_artifacts_v1 | Insert one registration under those coordinates, source/admission, generation, hash, relative path, framed provenance and artifact ID. Verify the file bytes. |
| product_review_sessions_v1 | One session receipt for this same fix stage, matching session key, output path and hash. |
| product_review_runs_v1 | Only counters_json on the reserved resuming run, the pack's single fix_rounds increment for the completed fixdie stage. |
| product_review_hold_progress_v1 | One same-operation row, continuing to recovery_required, revision +1 and the B1 reason “continuation interrupted after accepted decision”. Owner, epoch, fence and reserved run ID equal. |
| All other rows and schema | Exact equality. No new reservation, stage, budget charge, result grant, unrelated registration or additional external call. |

The fix session receipt and graph counter are the B1 bookkeeping of this one output. A change on another stage, counter or run fails. The original actor's exact accepted Resume through the production handler must return the original receipt and recorded recovery progress, with zero table, schema, file-content or call delta. A different interrupted or idle fixture cannot answer that question.

Five separate calls on the archived library connection after migration must each fail with schema and every table equal before/after and zero stage launches. Probe absent-pin activation, different-pin activation, matching-pin stage-start, matching-pin new Resume and continuation claim. Preserve old pre-migration positives, actual archived CLI across the barrier and rejected fresh old open, working current pin/run and denied next stage after revoke. Preserve trusted_schema OFF/UDF, WAL backup and both migration commit windows, uncertain retry, drain, interruption/idle control and post-write recovery with original artifact-root custody.

Private policy admits only independently inventoried, original-schema single-digest copies with matching installation/database, known writer custody and original pinned root. Fixture selection and quiescence are operator preconditions. The test-support entry point checks the store and performs the additive transaction. Existing-installation migration is closed. Keep later writes and retained bytes together for recovery.

## Other rows and adjudication order

Retain A01's bounded fixed-pack producer/host/finalizer, original accepted hold and existing nonowned-directory ignored case, B1 and feature-OFF results. Retain P01 authorization and selection-only effects, P01b CAS/ABA and original Abort/retry, P02 two independently selected and consumed packs, P03 both first-pin commit orders, P03b process races and crash windows/alias, P04 original-digest restart, P05 absent/tampered/swapped execution bytes, P06 order/repository/session/tuple isolation, and P09 separate pre-/post-accept loss and terminal retry. Use each at its original observation layer.

After this freeze, compare retained primary evidence to each row, run only missing private controls, then adjudicate once against this version. Record any unexplained delta as a failure or missing evidence as unknown. A scoped W1 commit requires all amended conditions. Original blockers, failure, deferred claims and the A01 exception retain separate dispositions.
