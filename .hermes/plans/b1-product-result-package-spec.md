# B1 W3 Product Result Package Contract — v3 FROZEN FOR W3 IMPLEMENTATION

## Human freeze record — highest governance precedence

User lunarpulse_ explicitly approved the v3 freeze/implementation packet in Discord message 1561496539174801409 (승인), following the progress report identifying the v3 freeze as the pending gate. This freezes the effective v3 contract: v3 delta/Appendix D > v2 R1–R7/Appendix C > D1–D8 > B > A. Historical DRAFT/NOT FROZEN/HUMAN FREEZE PENDING/implementation-blocked statements below describe prior review states and are superseded by this record. Historical review reports remain attributed, not implementation evidence.

Authorization: ADDENDUM 59, Codex CLI gpt-6-luna implementation and isolated tests, parent verification of W3 only. No W4, live API/GitHub/operating DB migration, B2 expansion, commit or push. Material authority/schema/identity changes beyond this contract require human change control. A newly discovered mechanical contradiction must be reported before inventing semantics. Freeze approval is NOT W3 acceptance. Independent contract verdict: READY_FOR_HUMAN_FREEZE; actual Rust/ProductStore implementation remains to be verified.

## Candidate v3 delta — exact legacy tuple census

Precedence: this delta + Appendix D correction > Candidate v2 R1–R7 and Appendix C > D1–D8 > B > A. Prior v2 findings are CLOSED as document requirements; Appendix D's REFINE is its verdict on v2, not v3. Implementation remains blocked pending closure verification and exact human freeze.

Adopt Appendix D's full legacy tuple census DDL, migration population/sealing, exact classification and matrix addition as normative candidate requirements. A host census and approval cutoff cannot classify tuples that predate either. Do not infer legacy from missing binding. Preserve host-authority classification independently.

Approval provenance: user 1561401650034245665 approved the presented v2 direction, recorded in b1-w3-v2-user-approval.md. This v3 delta adds explicit historical membership storage to enforce the already-selected historical-preservation/fail-closed policy; it is not claimed approved by that earlier message. Include this exact schema in the eventual freeze packet.

Parent inspected actual tuple_sqlite.rs:44–66 (seven columns including added generations_json) and b1_product_atomic.rs:641–652 (legacy tuple SQL fixture without mandatory host linkage), confirming the classification gap. No Rust/product execution is claimed by this inspection.


## D. Legacy tuple census amendment (read before A/B/C; overrides prior tuple classification)

**VERDICT: REFINE.**

Applied **R1–R7 + Appendix C > D1–D8 > B > A**. Prior findings are closed as specification requirements, not implemented behavior:

| Prior finding | Status and verification anchors |
|---|---|
| Authority DDL/capture/copy | **CLOSED — R1/C§1.** Capture sources and insertion order match [activation admission](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:534), [service validation](/home/cosmo/foundry/crates/foundry-ledger/src/product_service.rs:306), and [Resume materialization](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1711). Original grant revision is explicitly captured, never reconstructed. |
| Sealed legacy-host census | **CLOSED — R2/C§2.** Exact numeric acceptance membership and reservation ancestry resolve historical authority classification against [existing host/reservation schema](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:824). |
| Nested payload/local receipt | **CLOSED — R3/C§3.** Typed members, persisted string bytes, canonical round-trip and receipt projections are specified; sources exist in [artifact registration](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2641) and [typed tuple digest](/home/cosmo/foundry/crates/foundry-exec/src/types.rs:175). |
| Stored-result versus new eligibility/replay | **CLOSED — R3/R7/C§4.** Separate validators accommodate [initial completion](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:718), [Resume completion](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1232), and [CommitPass](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1799). Progress ownership and retained hold history are addressed. |
| Atomic migration/open/approval recovery | **CLOSED for the prior finding — R4/C§5.** Explicit migration, immutable metadata, nonrepairing open and both approval recovery branches address [open](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:696), [approval replay](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3231), and [ambiguous commit](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3405). One additional classification gap remains below. |
| Cargo isolation/test placement/API accessibility | **CLOSED — R5/R6/C§6.** Four default-off forwarding features and H/HX placement are concrete. Required existing types are [publicly exported](/home/cosmo/foundry/crates/foundry-ledger/src/lib.rs:29); [service admission](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3817) returns `AdmissionReceipt`, and [host dispatch](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:861) accepts the public opaque work claim. |
| Obsolete matrix claims | **CLOSED — R6/R7/C§7.** Replacements account for [continuation progress ownership](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1760), [CommitPass persistence](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1825), and the [work-owner fence](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2297). Superseded rows are not additional gates. |

**NEW mechanical blocker — legacy tuple membership is not recorded.**

[C§4](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:1133) requires distinguishing a legacy unbound tuple from a strict tuple missing its binding. C§5 preserves existing tuples but records only host membership and an approval cutoff.

The [seven-column tuple schema](/home/cosmo/foundry/crates/foundry-exec/src/tuple_sqlite.rs:44) has no creation sequence or contract marker. Historical product tuples can exist without host acceptance, as the [existing approval fixture](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_product_atomic.rs:641) demonstrates. Neither host census membership nor approval cutoff classifies every preserved tuple. Inferring “legacy” from binding absence would conceal precisely the corruption C§4 requires detecting.

Add this exact schema to C§5:

```sql
CREATE TABLE product_result_legacy_tuples_v1 (
  order_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  run_id INTEGER NOT NULL,
  generation INTEGER NOT NULL,
  PRIMARY KEY(order_id,attempt,run_id,generation),
  FOREIGN KEY(order_id,attempt,run_id,generation)
    REFERENCES artifact_tuples(order_id,attempt,run_id,generation)
);
```

Add this migration clause:

> After validating or creating compatible tuple storage, copy every existing tuple key into `product_result_legacy_tuples_v1` inside the same migration transaction, before sealing the census and before the final schema-version CAS. Fresh v3 initializes an empty sealed tuple census. Existing-v3 validation requires this table and its sealing triggers; repeated migration never repopulates it. Preserve all historical tuple bytes.

The migration copy and sealing DDL are:

```sql
INSERT INTO product_result_legacy_tuples_v1
  (order_id,attempt,run_id,generation)
SELECT order_id,attempt,run_id,generation FROM artifact_tuples;

CREATE TRIGGER product_result_legacy_tuples_no_insert
BEFORE INSERT ON product_result_legacy_tuples_v1
BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;

CREATE TRIGGER product_result_legacy_tuples_no_update
BEFORE UPDATE ON product_result_legacy_tuples_v1
BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;

CREATE TRIGGER product_result_legacy_tuples_no_delete
BEFORE DELETE ON product_result_legacy_tuples_v1
BEGIN SELECT RAISE(ABORT,'legacy tuple census is sealed'); END;
```

Replace C§4’s ambiguous tuple-classification sentence with:

> After current caller authorization, classify tuple ownership in the same snapshot. A tuple whose key belongs to the sealed legacy tuple census must have no result binding; it is preserved historical evidence and returns `Conflict` when encountered by strict finalization or lookup, never `None` and never adopted. A tuple outside that census must have exactly one valid result binding; absence is `CorruptEvidence`. A binding on a census key, a binding without its tuple, or a census entry without its tuple is `CorruptEvidence`. Validate malformed existing evidence before reporting an expected-digest or alias conflict. Every result-binding writer rejects census keys. Host-authority classification remains independently governed by C§2.

**Matrix correction:** extend C§7’s migration row with atomic tuple-census population/sealing, empty fresh-v3 census, unchanged repeated migration, and rollback on failure. Add H/HX `legacy_tuple_membership_distinguishes_unbound_history_from_corruption`: a migrated unbound tuple without host or approval evidence returns `Conflict`; an independently corrupted genuine finalized baseline missing its binding returns `CorruptEvidence`; neither lookup returns `None`; both produce zero execution/storage mutations. L retains schema-only sealing checks.

Current `DriveReview` finalization authority, original-revision policy, synthetic P2 scope and migration policy remain **HUMAN FREEZE PENDING**, not additional mechanical blockers.

Static review only. No edits or tests executed; all gates remain proposed. No W3 implementation or acceptance claim.

---


DRAFT / NOT FROZEN / IMPLEMENTATION BLOCKED.
Document consolidation authorized: Discord 1561399382761414657. Continuation: 1561395256560455772. Neither is contract freeze.
Baseline HEAD 36e994c; round59 430 passed is pre-contract evidence, not new contract validation.

## Precedence and provenance

Parent resolutions below supersede B; narrow refinement B supersedes broad proposal A. The included astra reports are attributed independent static findings, not parent executions. All proposed tests are NOT EXECUTED. No normative dependency on scratch logs is required; relevant contracts/matrices are included. This is a consolidation candidate, not a complete frozen specification.

Parent inspected ProductServiceContext:71–88, product/review.rs:1799–1902, line/tests/b1_product_restore.rs:44–115 and full bmad-review-full policy. Exact future execution counts remain static predictions.

## Parent candidate resolutions — human freeze pending

D1. Original-source current DriveReview authorizes new local result finalization only, not ApproveOrder/PublishApproved. Completed worker lease may be expired; current service validity and exact original authority revisions remain required. No revival of execution authority.
D2. Persist immutable per-host authority snapshot during admit_review_activation after work/service validation, not intake admission. Copy original snapshot during Resume. Do not backfill historical grant revisions from current state. Missing old evidence blocks new strict finalization, not history reads.
D3. Existing numeric allocator and reservations remain authoritative. Bind lifecycle/activation; reject aliases. CommitPass resolves its actual origin, not unused reserved successor. Tuple generation differs from chain index.
D4. Select fixed-purpose p2-test-support API/feature instead of raw SQL in new positive fixtures. Default off; no production HTTP/CLI route. Explicitly permit ledger/line manifest feature plumbing, no new external crates or frozen pack changes. Adapter validates exact admission/current authority and atomically walks real domain transitions. Finalizer never changes S1. P2 remains synthetic; general issue-to-code is B2.
D5. New digest is ArtifactTuple::digest; historical wire/digests unchanged. Standalone DB rejection and emit/promotion meaning unchanged. artifact_location is accepted evidence path, not fabricated code archive; opaque IDs use side bindings.
D6. Explicit maintenance-only v2→v3 migration includes authority snapshot schema. No auto-upgrade or inferred backfill. Preserve historical bytes; cutoff separates legacy missing bindings from new corruption. Old receipts do not authorize new publication.
D7. Require scenario A/B and direct CommitPass positives below with exact expected counts. Fake executor is not actual Die/Git. Keep initial product redrive unsupported and negatively tested; plan_full=[] is not real plan generation evidence.
D8. Require actual producer→binding→authenticated read→approval consumer wiring. No direct tuple/result/promotion seeding in positives. W3 calls Publisher zero times and creates no publish-start.

## Remaining mechanical review checklist

Before freeze, define exact nested payload members/types/encoding, receipt wire and immutable SQL DDL including authority snapshots, metadata migration/read-only handling, recover/replay validation, feature isolation and exact test matrix. Broad proposal contains superseded directions; use precedence explicitly, then eliminate contradictions before freeze. No production implementation authorized.


## Candidate v2 overriding resolutions

This section and Appendix C supersede all prior candidate instructions. D1–D8 remain proposals pending human freeze; no production implementation authorized. Incorporate Appendix C's numbered 1–7 normative corrections as the candidate contract, not optional implementation advice. The review's verdict describes v1 before incorporation, not a v2 approval.

R1. Accept immutable activation authority DDL, capture from validated work/service binding plus original-source enabled grant inside admit_review_activation, exact original copy on Resume, and no successor on CommitPass. Caller-supplied authority forbidden.
R2. Select exact sealed legacy-host census rather than S1/time inference for host authority. Preserve proven legacy descendants without invented snapshots; strict new finalization remains Ineligible. Other missing mandatory authority is corruption.
R3. Adopt strict nested typed payload/member ordering and LocalResultReceiptV1 from Appendix C. Raw embedded JSON strings preserve original bytes. Stored-set validation is separate from new-result eligibility/current-active checks.
R4. Adopt atomic explicit migration with separate immutable result meta/census, existing-v3 validation without silent repair, approval cutoff on actual approval outcome sequence, and side-binding checks in both replay and ambiguous-commit recovery. Preserve both scoped and unscoped v3 legacy publish rejection.
R5. Expand the candidate feature whitelist to ledger, line, serve and foundry manifests, all default-off forwarding. No unconditional dev-dependency enablement. Gate only new tests/modules; normal product code must have zero calls/routes/commands into support even when feature-unified. Feature presence alone is NOT a security boundary. This is a proposed whitelist expansion requiring human freeze with the rest.
R6. Move genuine-host storage and process positives to line-owned H/HX tests to preserve dependency direction. Ledger tests remain pure encoding/schema tests. Apply exact matrix replacements/deletions in Appendix C §7; earlier superseded rows are not extra acceptance claims.
R7. All new-finalization-only/replay/lookup/recovery calls have zero execute/restore/dispatch/sink/publisher and no S1/claim mutation. Accepted continuation may restore before its next stage authorization check; do not extend that zero-interaction guarantee to continuation itself.

Parent verification status: read-only review exit 0; before/after/current porcelain equal; diff-check passed before v2 document change. New tests/probes remain NOT EXECUTED. Final compliance verification of this assembled v2 is pending.

## A. Broad preflight proposal (astra; static, proposed)

**VERDICT: REFINE.** 제품 결과 확정과 기존 emit/promotion을 분리하는 방향은 타당합니다. 다만 현재 제안은 그대로 구현하거나 동결할 수 없습니다. digest 직렬화, promotion wire, 완료된 work claim의 권한 검사, tuple 좌표와 host activation의 대응, 역사 승인 처리에 구체적인 보완이 필요합니다.

지정된 task 전체, 부모 제안 전체, closure plan의 Refinement v2를 읽고 실제 dirty tree의 관련 구현과 시험 소스를 대조했습니다. HEAD는 `36e994c9897ffa42e56e294a8682009e854777f9`입니다. `graphify-out/GRAPH_REPORT.md`와 wiki index는 발견하지 못했습니다. 파일 수정, 시험 실행, DB 접근, 외부 API 호출, skill 로딩, agent 생성은 하지 않았습니다. 아래 시험은 모두 **제안이며 미실행**입니다. 부모의 430개 통과 보고를 독립 검증 결과나 W3 인수로 승계하지 않습니다.

**1. 독립 발견사항**

1. **빠진 것은 제품 tuple producer와 binding이며, 승인 소비부 자체는 존재합니다.**  
   [product.rs:3297](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3297)의 `ProductStore::approve_product`는 같은 transaction에서 `artifact_tuples`와 `tuple_promotions`를 조회합니다. 그러나 제품 schema 생성부에는 이 두 테이블의 정상 producer가 없고, 제품 승인 시험들은 직접 SQL로 테이블과 fixture tuple/promotion을 만듭니다. 예: [b1_product_atomic.rs:641](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_product_atomic.rs:641).  
   실제 경로는 `PackReviewHost → product_review_host_results_v1 / product_artifacts_v1`에서 끝납니다. 이 정상 경로 뒤에 승인하면 tuple 증거가 생기는 구조가 아닙니다. 이는 실행 재현 보고가 아니라 **현재 호출·DDL 경로에서 확인되는 결손**입니다.

2. **부모 제안의 “emit은 원격 게시이므로 W4”라는 근거는 정정해야 합니다.**  
   [review_run.rs:573](/home/cosmo/foundry/crates/foundry-exec/src/review_run.rs:573)는 실제로 `emit_once → record → promote` 순서입니다. 하지만 구체적인 [review_host.rs:3175](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:3175)의 `SqliteReviewSink::publish_named`는 로컬 SQLite의 `review_emit`와 관련 기록을 씁니다. GitHub `Publisher`를 호출하지 않습니다.  
   exec 내부의 W3/W4 명칭과 B1 계획의 W3/W4는 같은 경계를 뜻하지 않습니다. 분리 근거는 **제품 저장소 소유권, 오류 전파, 단일 transaction, 승인 결속**이어야 합니다. 새로운 제품 결과 영수증은 여전히 합리적인 선택입니다.

3. **기존 제품 tuple digest와 exec digest는 비어 있지 않은 generation chain에서 불일치합니다.**  
   [types.rs:175](/home/cosmo/foundry/crates/foundry-exec/src/types.rs:175)는 `Vec<GenerationCommit>`을 typed 직렬화합니다. [product.rs:4936](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4936)는 `serde_json::Value`로 파싱한 뒤 다시 직렬화합니다. 현재 dependency에는 `preserve_order`가 없고, 설치된 `serde_json` 소스도 기본 `BTreeMap`을 확인시켜 줍니다.

   동일한 원소가 다음 서로 다른 바이트가 됩니다.

   ```text
   exec:
   {"generation":0,"commit":"…","fix_log_path":""}

   product helper:
   {"commit":"…","fix_log_path":"","generation":0}
   ```

   기존 `[]` fixture는 이를 숨깁니다. 새 제품 경로는 `ArtifactTuple::digest()`를 그대로 사용해야 합니다. 역사 digest 함수의 의미를 일괄 변경하면 안 됩니다.

4. **기존 제품 승인부가 기대하는 promotion wire는 standalone 저장부와 다릅니다.**  
   `SqliteTupleStore::promote`는 `EmitReceipt { key, event, at }`를 `tuple_promotions.receipt_json`에 저장합니다. 제품 승인부는 JSON의 `tuple` 필드를 요구합니다. `PromotionReceipt { tuple, promoted_at }`와도 증거 종류가 다릅니다.  
   따라서 실제 standalone 저장 결과를 제품 승인 소비부에 그대로 연결하면 `promotion tuple missing` 경로에 도달합니다. 이를 `key`/`tuple` 양쪽을 무조건 허용하는 느슨한 decoder로 해결해서는 안 됩니다.

5. **host 결과 저장은 이미 존재하지만, 저장된 모든 `HostResult`가 최종 immutable 결과는 아닙니다.**  
   [product/review.rs:718](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:718)의 `complete_review_dispatch`는 run 결과와 numeric host 결과를 원자적으로 저장합니다. 그러나 `complete_continuation` 및 `finalize_accepted_hold`는 held 결과를 갱신합니다.  
   따라서 “host result row가 있다”만으로 확정하면 held snapshot 또는 복사된 continuation 초기 결과를 확정할 수 있습니다. 성공 상태, `held=None`, 완료된 continuation progress, 정확한 active binding을 함께 검사해야 합니다.

6. **기존 신규 작업 validator를 완료 후 finalizer에 재사용하면 정상 결과도 거부됩니다.**  
   초기 terminal completion은 work claim/backlog를 `completed`로 바꾸고 `lease_until`을 완료 시각으로 설정합니다. 반면 `ensure_active_fence`, `ensure_new_work`, `review_work_service_valid_in`은 `running` 또는 `claimed`를 요구합니다.  
   정상 host 완료 직후 이 함수들을 그대로 호출하는 제안은 구조적으로 실패합니다. 결과 보존 fence, 새 stage 권한, 새 결과 확정 권한은 별도 조건이어야 합니다.

7. **run 발급을 새로 만들 필요는 없지만, tuple identity의 축 손실은 해결해야 합니다.**  
   현재 대응은 다음과 같습니다.

   ```text
   HostKey = (order, attempt, numeric run, activation)
   journal_run = "{order}:{attempt}:{numeric_run}:{activation}"
   ArtifactTupleKey = (order, attempt, numeric run, generation)
   ```

   초기 numeric run은 입력 `ExecutionIntent.run`이며, hold 후속 run은 [product.rs:2889](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:2889)의 기존 예약에서 발급합니다. `reservation_id`는 numeric run이 아닙니다. `CommitPass`는 예약된 후속 run을 실행하지 않고 원래 run을 해결할 수 있습니다.  
   tuple key에는 `activation`과 `lifecycle`이 없습니다. 따라서 서로 다른 host identity가 같은 tuple key로 내려가는 충돌을 명시적으로 거부해야 합니다. `latest_run`의 문자열 파싱과 `unwrap_or(0)`은 새 결속의 근거로 쓰면 안 됩니다.

8. **generation이라는 이름의 두 의미를 섞으면 안 됩니다.**  
   현재 host intent와 tuple의 `generation`은 `0`입니다. `HostResult.generations[i].generation`은 fix chain index입니다. stage artifact의 generation도 `intent.generation`, 즉 `0`입니다.  
   최종 fix index를 tuple generation에 넣거나 stage ID의 `-gN`을 파싱하여 저장 generation을 바꾸면 기존 의미와 digest가 달라집니다.

9. **현 terminal validator는 새 확정 계약에 충분하지 않습니다.**  
   [product/review.rs:2377](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2377)는 일부 연속성·base·마지막 commit을 검사하지만, 첫 generation이 admitted input commit인지, fix log가 실제 완료 stage artifact인지까지 확인하지 않습니다. 빈 chain도 특정 조건에서 허용합니다.  
   정확한 active claim을 가진 in-process caller가 첫 commit을 바꾼 chain을 전달하는 것은 현재 validator를 통과할 수 있는 **정적 API 반례**입니다. 외부 사용자가 해당 trusted port를 호출할 수 있다는 공격 재현은 아닙니다.

10. **실제 executor의 Git 검증과 제품 출처 결속은 구별해야 합니다.**  
    [die_executor.rs:204](/home/cosmo/foundry/crates/foundry-line/src/die_executor.rs:204)는 verification HEAD를, `verify_produced_commit`은 실제 commit object·HEAD·부모를 검사합니다. 따라서 “실제 commit 검증이 전혀 없다”는 결론은 틀립니다.  
    반면 `HostResult` 자체에는 executor 종류나 설정된 `repo_path`와 source identity의 증명이 없습니다. W3 영수증은 trusted host가 수락한 실행 증거를 결속할 수 있지만, 그것만으로 원격 target repository의 일치까지 증명했다고 하면 안 됩니다.

11. **기존 artifact 시험은 genuine host + fake executor + 내부 router 수준입니다.**  
    [b1_product_artifacts.rs:123](/home/cosmo/foundry/crates/foundry-serve/tests/b1_product_artifacts.rs:123)는 실제 `PackReviewHost`와 `ProductStore`를 사용합니다. 다만 `GapExecutor`, `base..result`, 명시적인 synthetic backlog 선택, `Router::oneshot`을 사용합니다. tuple 확정, 실제 Die, loopback server, 원격 게시 인수는 아닙니다. 독립 변조 baseline 복원은 현재 소스에 추가되어 있습니다.

**2. 실제 코드 표면 inventory**

| 표면 | 실제 소유자·심볼 | 확정 계약에 주는 제약 |
|---|---|---|
| 최종 실행 데이터 | `foundry-exec/src/host.rs:31–65`, `HostKey`, `HostResult`, `GenerationCommit` | key 숫자는 `u64`; exit는 `i32`; commit/path는 `String` |
| genuine staged host | `foundry-line/src/review_host.rs:646`, `with_product_store`; `:861`, `try_dispatch_product_work`; `:1391`, terminal 구성 | 기존 algorithm 재사용. 새 runner 불필요 |
| 실제 Git executor | `foundry-line/src/die_executor.rs:95`, `DieStageExecutor`; `:416`, `verify_produced_commit` | fake executor와 실제 executor 증거를 구별 |
| admission·numeric/text mapping | `foundry-ledger/src/product/review.rs:485–584`, `admit_review_activation` | persisted `intent_json`과 정확한 format으로 검증 |
| 후속 run 예약 | `foundry-ledger/src/product.rs:2889`; `product/review.rs:1659–1752` | 기존 `followup_run_id` 사용; 두 번째 allocator 금지 |
| accepted-result fence | `product/review.rs:2030`, `ensure_active_fence`; `:2297`, `ensure_product_work_owner_fence_in` | source/service 철회와 worker reassignment의 효과가 다름 |
| 신규 stage 권한 | `product/review.rs:2111`, `ensure_new_work`; `:2327`, `ensure_product_source_for_new_stage` | 결과 확정에 그대로 재사용 불가 |
| terminal durability | `product/review.rs:718`, `complete_review_dispatch`; `:1232`, `complete_continuation`; `:1802` 부근 `finalize_accepted_hold` | held/copy/terminal 구분, 두 결과 row 일치 필요 |
| chain 검증 | `foundry-exec/src/review_run.rs:633`, `valid_generations`; `product/review.rs:2377–2551` | 제품 finalizer는 첫 commit·fix evidence 결속을 추가 |
| stage artifact 등록 | `product/review.rs:2569`, `register_product_artifact_in` | byte SHA-256와 immutable provenance; tuple 아님 |
| artifact 읽기 | `product/review.rs:2678`, `artifact_record_in`; `:2810`, `read_product_artifact` | 현재 원본-source `ReadArtifact`, 역사 읽기 가능 |
| 파일 confinement | `product/review.rs:2854`, `read_confined_artifact_with_hook`; `:2916`, `open_pinned_child` | pinned FD·Linux `O_PATH/O_NOFOLLOW`; 지원 불가 플랫폼 fail closed |
| standalone tuple | `foundry-exec/src/types.rs:145–191`; `tuple_sqlite.rs:15–37` | 타입·digest·ProductAtomicV1 거부 유지 |
| 제품 승인 | `foundry-ledger/src/product.rs:2969`, `prepare_approval`; `:3179`, `approve_product` | 승인·S1·grant/token·outcome 원자성 유지 |
| 승인 HTTP | `foundry-serve/src/approval_service.rs:24`, `ApproveRequest`; `:95` 부근 `approve` | 기존 coordinates/digests DTO를 유지할 수 있음 |
| 역사 publish helper | `product.rs:1165`, `authorize_product_publish`; `foundry-line/src/product.rs:103–149` | scoped mode 거부 유지; durable publish 계약 아님 |
| schema/version | `product.rs:696`, `open_with_clock`; `:818` 이후 review schema | 현재 product schema version `2`, review version `1` |
| 의존성 | `line → ledger → exec`; `line → exec`; `serve → ledger/line` | `ledger → line/serve/watch` 추가 금지 |

**3. 부모가 draft spec으로 옮길 제안 계약**

아래는 **제안된 정확한 계약**입니다. 새 이름은 현재 구현된 API라고 주장하지 않습니다. 인간 변경관리 사항은 뒤의 BLOCKER 목록과 함께 동결해야 합니다.

**PT-01 — 소유권과 범위**

제품 finalizer의 유일한 writer는 `ProductStore`입니다. 기존 host ports를 통해 저장된 결과를 다시 읽어 tuple을 구성합니다. caller가 제출한 tuple, artifact path, commit 문자열만으로 등록하는 API는 만들지 않습니다.

최소 구현은 `foundry-ledger/src/product/result.rs`에 둘 수 있습니다. 기존 exec 타입으로 충분하므로 새 exec trait나 `ReviewRunOutcome` 변경은 필수가 아닙니다. `PackReviewHost`가 소유한 store와 별도의 `ProductStore` handle을 사용해도 SQLite transaction이 경계입니다.

`DurableReviewRun`, `TupleStore::promote`, `EmitReceipt`, `PromotionReceipt`, standalone digest는 변경하지 않습니다. 새 결과를 `tuple_promotions`에 기록하지 않습니다.

**PT-02 — 공개 타입과 API**

다음 타입은 `foundry_ledger::product`에서 공개하고 필요한 것만 crate root에서 re-export합니다.

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductResultExpectation {
    pub host_key: foundry_exec::HostKey,
    pub host_result_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProductResultInfo {
    pub result_id: String,
    pub order_id: String,
    pub lifecycle: i64,
    pub attempt: i64,
    pub run_id: i64,
    pub activation: i64,
    pub generation: i64,
    pub base_commit: String,
    pub result_commit: String,
    pub host_result_digest: String,
    pub tuple_digest: String,
    pub binding_digest: String,
    pub final_artifact_id: String,
    pub artifact_ids: Vec<String>,
    pub finalized_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductResultArtifactContent {
    pub result: ProductResultInfo,
    pub artifact: ProductArtifactContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductResultError {
    InvalidInput,
    Unauthorized,
    Unavailable,
    Ineligible,
    StaleBinding,
    InvalidEvidence,
    Conflict,
    CorruptEvidence,
    Storage,
    CommitUncertain,
}
```

`ProductResultInfo`는 조회 결과이며 authority token이 아닙니다. 저장용 payload와 constructor는 private입니다. 공개 DTO에는 filesystem path, credential verifier, raw work payload를 넣지 않습니다.

제안하는 inherent method 서명은 다음과 같습니다. `ProductServiceContext`는 기존 private-constructor 타입을 사용합니다.

```text
ProductStore::finalize_product_review_result(
    &mut self,
    service: &ProductServiceContext,
    expected: &ProductResultExpectation,
) -> Result<ProductResultInfo, ProductResultError>

ProductStore::lookup_product_review_result(
    &mut self,
    service: &ProductServiceContext,
    expected: &ProductResultExpectation,
) -> Result<Option<ProductResultInfo>, ProductResultError>

ProductStore::read_product_result(
    &mut self,
    session_digest: &str,
    order_id: &str,
    result_id: &str,
) -> Result<ProductResultInfo, ProductResultError>

ProductStore::read_product_result_artifact(
    &mut self,
    session_digest: &str,
    order_id: &str,
    result_id: &str,
    artifact_id: &str,
) -> Result<ProductResultArtifactContent, ProductResultError>
```

순수 helper:

```text
pub fn product_host_result_digest(
    result: &foundry_exec::HostResult,
) -> Result<String, ProductResultError>
```

시각은 이 새 API의 caller 입력이 아니라 `ProductStore.clock.now()`에서 transaction 시작 후 얻습니다. 기존 API의 시각 정책까지 광범위하게 변경하지 않습니다.

`lookup_product_review_result`는 **조회만** 합니다. `None`은 일관된 DB snapshot에서 증명된 부재일 때만 반환합니다. 오류·손상·불명확 commit을 `None`으로 바꾸지 않습니다.

**PT-03 — identity·숫자 폭·mapping**

불변 business identity:

```text
(installation_id, database_id,
 order_id, lifecycle,
 attempt, numeric_run, activation, tuple_generation)
```

별도 unique tuple identity:

```text
(order_id, attempt, numeric_run, tuple_generation)
```

후자가 이미 다른 lifecycle/activation/result identity에 결속되었으면 `Conflict`입니다. 새 numeric run을 발급해 충돌을 숨기지 않습니다.

| 필드 | 원천·폭 |
|---|---|
| `installation_id`, `database_id` | 검증된 `ProductStore` 설정과 meta의 `String` |
| `order_id` | persisted `ExecutionIntent.order.0` |
| `attempt`, `numeric_run`, `activation`, `epoch` | exec의 `u64`; DB 저장 전 `i64::try_from` |
| 실제 order attempt | domain의 `u32`; replay된 현재 attempt와 exact 비교 |
| `lifecycle` | product DB의 `i64`; replay lifecycle의 `u32 + 1`과 checked 비교 |
| `tuple_generation` | persisted intent의 `Generation(u64)`; 현재 제품 계약에서는 `0` |
| chain index | `GenerationCommit.generation: u64`; vector index와 비교 |
| `journal_run` | persisted intent에서 재구성한 정확한 format 및 run row와 비교 |
| source generation/revision | 각 source 테이블의 `i64`; connector revision과 수치 동일성 요구 금지 |
| stage sequence | 기존 `u64` 입력, DB에서 `i64`; checked 변환 |
| commit | `String`; 새 결과에는 기존 승인 HTTP와 일치하는 full 40/64 hex 요구 |
| digest | lowercase 64 hex SHA-256 |
| 시간 | 최초 transaction의 RFC3339 문자열; replay 시 재생성 금지 |

v3 이후 신규 admission은 기존 accepted run 및 followup reservation과 충돌하는 numeric run 재사용을 실행 전에 거부합니다. 기존 row를 재번호화하지 않습니다. `CommitPass`는 실제 해결한 originating run을 사용하며, 사용되지 않은 `reserved_run_id`를 tuple run으로 바꾸지 않습니다.

**PT-04 — 최소 영속 schema**

`artifact_tuples`는 기존 컬럼과 tuple 의미를 그대로 사용하되 제품 DB에서는 `ProductStore`만 씁니다.

추가 테이블은 다음 두 개를 권고합니다.

```text
product_result_bindings_v1
  result_id             TEXT PRIMARY KEY
  order_id              TEXT NOT NULL
  lifecycle             INTEGER NOT NULL
  attempt               INTEGER NOT NULL
  run_id                INTEGER NOT NULL
  activation            INTEGER NOT NULL
  generation            INTEGER NOT NULL
  host_result_digest    TEXT NOT NULL
  tuple_digest          TEXT NOT NULL
  binding_digest        TEXT NOT NULL
  payload_json          TEXT NOT NULL
  receipt_json          TEXT NOT NULL
  UNIQUE(order_id,lifecycle,attempt,run_id,activation,generation)
  UNIQUE(order_id,attempt,run_id,generation)

product_approval_result_bindings_v1
  operation_id          TEXT PRIMARY KEY
  result_id             TEXT NOT NULL
  binding_digest        TEXT NOT NULL
  tuple_digest          TEXT NOT NULL
```

새 테이블과 제품 tuple에는 UPDATE/DELETE 방지 trigger를 둡니다. replay에 `INSERT OR REPLACE`를 사용하지 않습니다. FK 설정에만 의존하지 않고 transaction 내 exact join을 검사합니다.

독립적인 “emit 없는 promotion” 테이블은 만들지 않습니다. `product_result_bindings_v1` 자체가 로컬 확정 영수증과 artifact 결속을 소유합니다.

**PT-05 — 저장 payload와 hash domain**

`payload_json`은 `deny_unknown_fields`를 가진 private typed 구조체로 아래 순서의 필드를 직렬화합니다. `serde_json::Value`를 거쳐 재정렬하지 않습니다.

| 순서 | 필드·타입·원천 |
|---|---|
| 1 | `version: u32 = 1` |
| 2 | `installation_id`, `database_id`, `order_id: String`; `lifecycle: i64` |
| 3 | `source_instance_id`, `repository_key: String`; `source_generation`, `source_instance_revision`, `source_revision: i64` |
| 4 | `admission_seq: i64`, `admission_hash: String`; 원본 immutable admission |
| 5 | `intent: ExecutionIntent`, `journal_run: String`; 저장된 binding |
| 6 | `host_result: HostResult`, `host_result_digest: String`; terminal 두 결과 row가 일치한 값 |
| 7 | `tuple: ArtifactTuple`, `tuple_digest: String` |
| 8 | `binary_provenance: ReviewBinaryProvenance`; 해당 run의 persisted provenance |
| 9 | `work`: delivery ID, worker ID, claim epoch/fence, payload digest, service ID/revision, credential revision, DriveReview grant revision |
| 10 | `accepted_holds`: root부터 현재까지의 operation ID, **원래 receipt JSON**, completed progress epoch/fence, reserved run ID |
| 11 | `artifacts`: 아래 artifact 결속 목록 |
| 12 | `s1_sequence: i64`, `s1_event_hash: String`; 확정 시 검증한 lifecycle anchor |
| 13 | `final_artifact_id: String`, `finalized_at: String` |

`artifacts`의 각 항목은 기존 artifact의 ID, provenance digest, byte SHA-256, lifecycle, source instance, repository key, admission anchor, attempt, journal run, generation, sequence, stage ID, relative path, 원래 `output_json`을 결속합니다. 목록 순서는 lineage의 run 순서, 각 run의 numeric stage sequence 순서입니다. 동일 artifact ID를 중복 삽입하지 않습니다.

기존 `product_scope::framed_hash` 규칙으로 다음 신규 domain을 제안합니다.

```text
host_result_digest =
  framed_hash("foundry-product-host-result-v1",
              [typed HostResult의 compact JSON])

result_id =
  framed_hash("foundry-product-result-id-v1",
              [installation_id, database_id, order_id, lifecycle(decimal),
               attempt(decimal), run(decimal), activation(decimal),
               generation(decimal)])

binding_digest =
  framed_hash("foundry-product-result-binding-v1",
              [typed payload의 compact JSON])
```

`tuple_digest`는 반드시 `ArtifactTuple::digest()`입니다. commit ID, byte digest, host-result digest, tuple digest, binding digest 사이에 equality를 요구하지 않습니다.

이 새 domain과 encoding은 **아직 동결되지 않은 제안**입니다.

**PT-06 — tuple 구성과 artifact 의미**

```text
tuple.order              = persisted intent.order
tuple.attempt            = persisted intent.attempt
tuple.run                = persisted intent.run
tuple.generation         = persisted intent.generation       // 현재 0
tuple.immutable_commit   = terminal HostResult.result_commit
tuple.artifact_location  = terminal HostResult.log_path       // 정확한 원래 문자열
tuple.generations        = terminal HostResult.generations
```

`artifact_location`은 현 계약대로 **수락된 review evidence 파일의 위치**입니다. 코드 archive나 Git object 저장소라고 주장하지 않습니다. opaque artifact ID는 별도 binding에 저장하며 tuple location을 대체하지 않습니다.

`HostResult.log_path`는 정확히 등록된 artifact 하나로 해결되어야 합니다. `pack_path#run_id` fallback, 미등록 경로, caller가 새로 고른 파일은 거부합니다.

필수 chain 검증:

- chain은 비어 있지 않습니다.
- 첫 항목은 generation `0`, admitted input `result_commit`, 빈 `fix_log_path`입니다.
- index가 연속적이며 모든 commit이 full SHA 형식입니다.
- 마지막 commit은 terminal `result_commit`과 같습니다.
- 각 후속 항목의 fix log는 동일 run 또는 증명된 hold ancestry의 완료 stage·등록 artifact와 일치합니다.
- 그 stage의 `produced_commit`은 해당 chain commit과 일치합니다.
- terminal base는 admitted base와 일치합니다.
- verification evidence의 `inspected_commit`은 해당 검증 대상 commit과 일치합니다.
- 명시적인 `CommitPass`는 자동 검증 통과와 구별하여 원본 인간 acceptance를 결속합니다.
- continuation이 물려받은 artifact를 새 run에서 생성된 것처럼 재등록하지 않습니다.

이것은 trusted host/executor 증거의 결속입니다. 원격 repository identity나 W4 target의 독립 증명은 아닙니다.

**PT-07 — 신규 확정 transaction과 CAS 순서**

host 결과 보존 transaction과 결과 확정 transaction을 분리합니다. 후자의 실패가 이미 저장된 결과·stage·session을 rollback하거나 재실행시키지 않습니다.

신규 확정의 순서:

1. 형식·숫자 범위를 검사합니다.
2. `BEGIN IMMEDIATE`, transaction 시각 획득, schema/installation 확인.
3. 현재 `ProductServiceContext`의 원본 source에 대한 `DriveReview`를 재검사합니다.
4. business identity에 기존 영수증이 있는지 확인합니다. 있으면 아래 replay 규칙을 따릅니다.
5. S1 전체 검증과 현재 order attempt/lifecycle, immutable admission, source instance/repository를 대조합니다.
6. persisted host acceptance, intent binding, journal run, active key/epoch를 exact 비교합니다.
7. run 결과와 numeric host 결과를 typed decode하여 동일성을 검사합니다.
8. `status ∈ {passed,resolved}`, `exit_code=0`, `held=None`을 요구합니다. continuation/CommitPass가 있으면 acceptance·예약·completed progress를 검증합니다.
9. terminal owner/fence 및 신규 확정 권한을 검사합니다.
10. PT-06 chain과 모든 결속 artifact의 provenance·전체 파일 bytes를 검증합니다.
11. tuple key의 기존 점유를 검사합니다. binding 없는 기존 tuple을 새 결과로 채택하지 않습니다.
12. tuple과 result binding을 같은 transaction에 삽입합니다.
13. commit 후에만 `ProductResultInfo`를 반환합니다.

최소 계약에서는 이 operation이 S1 상태를 바꾸지 않습니다. 신규 확정 후보는 현재 replay 상태가 `waiting`인 결과로 제한하는 안을 권고합니다. `approve_product`는 이후 별도 transaction에서 기존 `waiting → publishing` 전이를 수행합니다.

현재 queued 상태에서 성공하는 artifact fixture를 곧바로 승인 가능한 결과라고 간주하면 안 됩니다. P2의 명시적 lifecycle 입력 어댑터로 `waiting`을 준비할지, 별도 제품 lifecycle producer를 범위에 넣을지는 BLOCKER입니다.

**PT-08 — 권한과 보존**

다음은 변경관리 대상으로 제안하는 정책입니다.

| 상황 | 제안 처리 |
|---|---|
| stage 시작 뒤 service revoke/expiry/source rebind | exact worker fence가 유지되면 기존 결과·session 보존 |
| worker owner/epoch/fence 재배정 | 옛 worker 결과 저장 거부 |
| 새 결과 확정 | 현재 원본 source·DriveReview 필요; 원래 service/work binding과 일치 |
| 초기 terminal 결과 | claim/backlog의 정확한 `completed` 상태 허용; 완료 시 만료된 work lease를 신규 stage lease처럼 검사하지 않음 |
| continuation/CommitPass 결과 | completed acceptance progress와 원래 work owner/fence를 검사; 현 구현의 `claimed` 상태를 자동 완료로 바꾸지 않음 |
| 인간 acceptance가 사용된 새 확정 | 해당 인간 account/scope/grant의 현재 revision도 exact 검사 |
| revoke→regrant/credential rotation 전후의 미확정 결과 | 옛 authority를 새 것으로 덮어쓰지 않음; 별도 recovery 승인 없이는 확정 거부 |
| 확정 뒤 revoke/rebind | immutable 결과를 삭제하거나 수정하지 않음; 새로운 승인/게시 권한은 별도로 검사 |
| 역사 읽기 | 현재 인간 session과 **원본 source instance**의 명시적 `ReadArtifact`로 허용 |

새 capability, blanket grant, worker-ID 권한, ambient bypass는 추가하지 않습니다. `DriveReview`에 “새 로컬 결과 후보 확정”까지 포함할지 인간 동결이 필요합니다. 이것이 `ApproveOrder`나 `PublishApproved`를 부여하지는 않습니다.

**PT-09 — replay·conflict·복구**

- 동일 identity와 동일 expected host digest의 성공한 재요청은 **저장된 동일 영수증**을 반환합니다. 시각·digest·ID를 다시 만들지 않습니다.
- 기존 identity에 다른 expected host digest는 `Conflict`.
- 기존 tuple key가 다른 activation/lifecycle에 속하면 `Conflict`.
- 저장된 tuple/payload/영수증 간 불일치는 `CorruptEvidence`; 새 값으로 “수리”하지 않습니다.
- 영수증 replay에는 현재 요청자의 service/source 권한을 확인하되, 이미 완료된 worker lease를 되살리거나 새 stage를 예약하지 않습니다.
- 현재 source가 바뀌어 service replay가 거부되어도 인간 역사 읽기는 PT-08 정책을 따릅니다.
- commit 전 crash: tuple과 binding 모두 없음. 원래 host 결과는 보존.
- commit 후 응답 손실: 둘 다 있음. fresh handle의 lookup으로 동일 영수증 회수.
- commit 결과 불명: `CommitUncertain`; fresh handle로 확인하기 전 executor 재호출 금지.
- host acceptance만 있고 terminal 결과가 없는 경우: `Ineligible`; “부재이므로 재실행”으로 해석 금지.
- finalization recovery는 executor·emit sink·publisher를 호출하지 않습니다.

**PT-10 — 인증된 result 읽기**

기존 stage artifact routes는 그대로 유지합니다. 제품 결과 결속을 실제로 소비하는 읽기 두 개를 추가합니다.

```text
GET /api/product/orders/{order_id}/results/{result_id}
GET /api/product/orders/{order_id}/results/{result_id}/artifacts/{artifact_id}
```

두 번째 route는 result에 결속된 artifact ID만 허용합니다. 동일 order의 무관한 stage artifact도 거부합니다. tuple, result payload, 원본 provenance를 같은 DB snapshot에서 검증하고 pinned resolver로 bytes를 확인한 뒤 응답합니다.

성공 시 기존 artifact headers에 `x-foundry-result-id`, `x-foundry-tuple-digest`를 추가할 수 있습니다. `no-store`를 유지하며 path를 반환하지 않습니다.

권한 없는 원본 source, foreign ID, 미등록 legacy, 변조는 존재 여부를 구분하지 않는 `404`입니다. bearer 자체가 없으면 기존처럼 `401`, DB availability failure는 `503`입니다. result metadata를 먼저 반환하고 파일 오류를 나중에 보고하는 부분 성공은 허용하지 않습니다.

**PT-11 — 승인 소비부 전환**

`ProductApprovalIntent`와 `ProductApprovalReceipt`의 기존 필드는 유지할 수 있습니다. 신규 승인에서 tuple coordinates/digest로 **정확히 하나의** `product_result_bindings_v1`을 찾아 결속합니다.

신규 승인 transaction은 다음을 모두 검사합니다.

- 기존 session/account/`ApproveOrder`, reservation owner/epoch/fence, grant/token 조건.
- 현재 source instance, connector revision, lifecycle와 **replay된 실제 attempt**.
- 현재 승인 가능한 run과 exact result identity.
- typed tuple digest와 binding digest.
- 원본 artifact provenance와 필요한 evidence bytes.
- 성공한 terminal 결과 및 acceptance lineage.

동일 승인 transaction에서 기존 S1·transition receipt·grant/token consumption·outcome에 더해 `product_approval_result_bindings_v1` 한 행을 삽입합니다. 어느 하나라도 실패하면 전부 rollback합니다.

`artifact_digest`의 기존 의미는 final commit입니다. log byte hash로 바꾸지 않습니다. 새 승인에 legacy promotion fallback을 허용하지 않습니다.

**PT-12 — version·legacy**

권고 migration:

- `foundry_contract = ProductAtomicV1` 유지.
- product schema `2 → 3`; 기존 review schema version과 혼동하지 않음.
- 기존 v2 binary가 v3를 열지 못하도록 기존 version guard를 유지.
- v3 normal open은 v2를 자동 변환하지 않음.
- 별도 maintenance 함수 제안:

  ```text
  ProductStore::migrate_product_results_v2(
      path: &std::path::Path,
      config: &ProductConfig,
  ) -> Result<(), ProductError>
  ```

- writer를 중지한 maintenance 절차에서 한 `IMMEDIATE` transaction으로 새 schema와 version을 적용.
- meta에 immutable `result_contract_version=1`, `legacy_approval_cutoff_seq`를 기록.
- 기존 tuple, promotion, approval receipt, S1 bytes를 재작성하지 않음.
- 구 receipt의 side binding 부재는 cutoff 이전 역사 outcome일 때만 legacy로 분류. cutoff 이후 부재는 손상.
- legacy tuple에 host/artifact provenance를 추정하여 backfill하지 않음.
- 기존 `product_tuple_digest`는 역사 계약 확인용으로 보존. 새 결과는 exec typed digest.
- legacy approval 조회/replay는 기존 인증 정책과 원래 receipt를 보존하되 **새 publish 허가로 승격하지 않음**.
- migration 실패는 version·DDL·meta 전체 rollback. 재개방 시 부분 성공을 채택하지 않음.

기존 unscoped helper가 새 결과 영수증을 publish 권한으로 받게 확장하지 않습니다. scoped helper 거부는 유지합니다.

**PT-13 — 오류 우선순위**

독립적인 여러 오류가 동시에 있을 때 제안 순서는 다음과 같습니다.

```text
순수 입력 형식/범위
→ 저장소 contract/version/접근 오류
→ 현재 요청자 권한
→ 기존 identity replay/conflict 또는 저장 증거 손상
→ 현재 source/lifecycle/run/owner fence
→ terminal eligibility
→ generation/provenance 결속
→ 파일 confinement/byte integrity
→ unique/CAS 충돌
→ commit 결과
```

DB 오류는 해당 접근 지점에서 즉시 반환합니다. authority 실패 뒤 foreign result의 상세 상태를 노출하지 않습니다. 영수증 replay는 현재 worker lease나 새 artifact 생성 없이 저장 증거를 확인합니다. `Storage`와 `CommitUncertain`을 일반적인 재실행 가능 오류로 취급하지 않습니다.

**4. Traceability matrix**

모든 행은 **PROPOSED / NOT EXECUTED**입니다.

파일 약어:

- **L**: `crates/foundry-ledger/tests/b1_product_results.rs` — 신규
- **X**: `crates/foundry-ledger/tests/b1_product_result_processes.rs` — 신규
- **H**: `crates/foundry-line/tests/b1_product_results.rs` — 신규
- **S**: `crates/foundry-serve/tests/b1_product_artifacts.rs` — 기존 확장
- **C**: `crates/foundry/tests/b1_scope_cli.rs` — 실제 server 시험 확장

공통 양성 fixture **B**는 명시적 P2 adapter로 입력/lifecycle을 준비하고, 실제 auth/service/admission/claim API, 실제 `PackReviewHost`, fake `StageExecutor`를 사용합니다. tuple·result binding·promotion row는 직접 seed하지 않습니다. 정상 첫 pass는 기존 시험이 사용하는 `edge`, `verif`, `claims`, `triage`의 네 stage를 고정하여 `execute=4`, `restore=0`, `host dispatch=1`을 단언합니다. full SHA 입력을 사용합니다.

`T/R/A`는 해당 tuple/result binding/approval side binding의 행 수입니다. 모든 W3 행에서 `Publisher::publish=0`, 새 finalizer의 `EmitSink::publish=0`입니다.

| Gate / 계약 | 제안 시험명·파일 | Setup → operation | 기대 행·호출·증거 |
|---|---|---|---|
| PT-01 소유권 | `standalone_store_still_rejects_product_v3` — L 및 기존 `exec/tests/tuple_sqlite.rs` | v3 제품 DB를 standalone tuple/host adapter로 open | 거부, 신규 row 0. standalone 의미 유지. 저장소 수준 |
| PT-02 API·producer | `genuine_host_result_finalizes_without_emit_or_publish` — H | B → host 완료 → ProductStore finalizer | accept/result/run 각 1, complete stage/artifact 각 4, `T=1,R=1,A=0`, promotion 0; execute=4. 실제 host+제품 producer, fake executor |
| PT-03 mapping | `result_uses_persisted_numeric_run_and_activation` — H | run 7, activation 3, 콜론 포함 order → finalize | exact journal format, tuple run 7/gen 0; parse fallback 0 없음 |
| PT-03 충돌 | `activation_and_lifecycle_aliases_cannot_share_tuple_key` — L | 동일 tuple key의 다른 activation/lifecycle 시도 | admission 또는 finalize에서 거부; 기존 `T/R=1/1` 불변, 추가 execute=0 |
| PT-03 예약 | `commit_pass_keeps_origin_run_resume_uses_reserved_run` — H | genuine hold 각각 CommitPass/Resume | CommitPass는 origin tuple, Resume는 기존 followup 예약; allocator 증가 추가 0 |
| PT-03 폭 | `result_coordinates_reject_overflow_and_wrong_attempt` — L | `i64::MAX+1`, 음수 DB 좌표, replay attempt 불일치 각각 | 명시 오류, `T/R/A=0/0/0`; wrap/fallback 없음 |
| PT-04 원자성 | `tuple_and_result_binding_commit_as_one_set` — L | B 뒤 tuple insert/receipt insert 각각 오류 주입 | 전체 `T/R=0/0`; 원래 host·artifact 불변; 추가 execute=0 |
| PT-05 digest | `nonempty_generation_digest_matches_exec_and_preserves_legacy` — L | 단일·복수 typed chain 및 역사 Value encoding | 새 digest=exec; 비어 있지 않은 구 encoding과 차이 단언; 역사 bytes 불변 |
| PT-05 wire | `emit_key_and_promotion_tuple_are_not_interchangeable` — L | 실제 두 receipt 직렬화 전달 | 신규 local-result 증거로 둘 다 거부; fallback 승인 0 |
| PT-06 chain | `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` — L | 깨끗한 B별로 empty, 첫 commit, index, last commit, fix log, produced_commit 하나씩 변경 | 해당 오류, `T/R=0/0`; host 재실행 0 |
| PT-06 ancestry | `continued_result_binds_original_and_followup_artifacts` — H | genuine accepted Resume와 fix/reverify → finalize | chain 원소마다 실제 stage/artifact 결속; ancestor 재등록 0; 새 tuple/result 각 1 |
| PT-06 location | `result_location_must_resolve_registered_terminal_evidence` — L | fallback `#run`, 다른 stage path, 외부 path, 무등록 path | 모두 거부, `T/R=0/0`; caller path 사용 없음 |
| PT-07 terminal | `held_failed_and_materialized_continuation_are_not_final_results` — H/L | held, failed, 복사 직후 resuming 결과 각각 | `Ineligible`; `T/R=0/0`; 기존 hold/result/session 불변 |
| PT-07 lifecycle | `queued_success_is_not_silently_made_approvable` — H | 현 queued artifact fixture와 B의 waiting fixture 비교 | queued 확정 거부; waiting 성공; finalizer의 S1 추가 0 |
| PT-08 보존 | `revoke_after_stage_start_preserves_result_but_blocks_finalization` — X/H | pipe barrier로 start commit → revoke/rebind → completion | 수락 stage result/session 보존, 다음 stage 추가 호출 0, `R=0`; 두 프로세스 |
| PT-08 선행 철회 | `revoke_before_stage_start_has_zero_executor_calls` — X | revoke commit → stage start | 새 stage/session/artifact/tuple/result 0, execute=0 |
| PT-08 완료 claim | `completed_claim_can_finalize_without_reviving_its_lease` — H | 정상 terminal로 claim/backlog completed → finalize | `T/R=1/1`, claim·lease 불변, 추가 execute=0 |
| PT-08 stale owner | `reassigned_owner_cannot_store_or_finalize_old_result` — X | worker fence 재배정 → old completion/finalize | stale 거부, replacement claim 불변, 기존 증거 덮어쓰기 0 |
| PT-08 ABA | `regrant_rotation_and_human_revision_do_not_reauthorize_pending_result` — X/L | revoke→regrant, credential rotation, 인간 grant revision 변경 각각 | pending 확정 거부; 기존 receipt bytes 불변; 새 권한 추정 없음 |
| PT-09 replay | `two_process_exact_replay_returns_one_identical_receipt` — X | 두 프로세스 같은 B/expectation, commit 순서 양방향 | 둘 다 동일 receipt, `T/R=1/1`, 추가 execute/emit/publish=0 |
| PT-09 conflict | `two_process_changed_expectation_never_replaces_winner` — X | 같은 key, 다른 expected host digest; 순서 양방향 | 정상 요청만 성공, 다른 요청 conflict/evidence mismatch; 최종 `T/R=1/1` |
| PT-09 crash | `kill_before_and_after_result_commit_recovers_without_dispatch` — X | tuple insert 뒤, binding insert 뒤, commit 뒤 barrier에서 child kill | precommit `0/0`, postcommit `1/1`; reopen 가능, recovery execute=0 |
| PT-09 불명확 | `ambiguous_commit_uses_lookup_and_never_treats_io_error_as_absence` — L/X | response loss·commit I/O·lookup failure 각각 | exact receipt 또는 명시 불명확 오류; blind rerun=0 |
| PT-10 인증 읽기 | `bound_result_artifact_reads_require_original_source_grant` — S | B→finalize→login→새 result/artifact routes | 200·정확한 bytes/digests; foreign/no-grant 404, bearer 없음 401; 내부 router |
| PT-10 역사 | `retired_source_history_remains_readable_without_new_source_inheritance` — S | result 확정→source disable/rebind→old/new grant 비교 | old explicit read 200, 새 source만 grant한 계정 404; 승인/게시 허가 0 |
| PT-10 독립 변조 | `each_result_integrity_mutation_starts_from_verified_baseline` — S/L | 각 case 앞 성공 확인 후 bytes/SHA/path/provenance/tuple/binding/host 결과 하나씩 변조 | 각각 실패; restore 후 성공; 부분 bytes/path 누출 0 |
| PT-10 confinement | `result_reads_preserve_pinned_root_and_no_follow_guards` — S 및 기존 review unit tests | leaf/ancestor/root replacement, symlink, FIFO/device | 원래 bytes 또는 거부, 외부 bytes 0, 미지원 플랫폼 거부 |
| PT-11 승인 | `approval_consumes_exact_local_result_in_atomic_transaction` — L/S | B result→실제 grant/prepare/approve | `A=1`; Approved S1·transition receipt·token·outcome 각 1, grant consumed 1; publish=0 |
| PT-11 실패 | `approval_rejects_changed_result_without_consumption` — L/S | foreign/stale tuple, source, attempt, generation, digest 각각 | 승인 관련 Δrow=0; grant/token 소비 0; 기존 result 보존 |
| PT-11 원자성 | `approval_result_binding_rolls_back_with_each_existing_participant` — X | 기존 승인 각 insert 및 새 side binding 전후 crash | 전부 없음 또는 정확한 한 세트; receipt replay에서 중복 0 |
| PT-12 migration | `v2_to_v3_migration_is_additive_atomic_and_old_binary_fenced` — L/X | 실제 v2 역사 fixture→migration; 각 DDL 오류 주입 | 성공 시 bytes 불변·v3, 실패 시 v2·부분 schema 없음; v2 open의 v3 거부 |
| PT-12 legacy | `legacy_receipts_remain_history_but_cannot_authorize_new_results` — L | cutoff 이전 outcome, 미등록 tuple, cutoff 이후 누락 side binding | 역사 receipt 동일; 신규 승인 거부; 후자는 corruption |
| PT-13 우선순위 | `result_error_precedence_does_not_reveal_foreign_evidence` — L/S | unauthorized+tamper, stale+missing file, conflict+expired worker, DB read 오류 | 정의된 우선순위·동일 외부 오류, 신규 row 0 |
| PT-14 실제 서버 | `product_result_read_and_approval_work_over_authenticated_loopback_server` — C | test-owned B DB→실제 제품 server→login/read/approve | socket HTTP 200/거부 코드·동일 bytes; publisher=0. server 증거 |
| PT-15 W4 분리 | `w3_paths_never_invoke_publisher_or_create_publish_start` — H/S/C | 성공·실패·retry·reopen 전체에 spy | publisher=0, publish-start/outcome 신규 0; scoped legacy helper 계속 거부 |

복수 generation 양성 fixture의 정확한 stage script와 호출 수는 기존 continuation pack 경로를 부모가 고정해야 합니다. **이 수치가 미확정인 PT-06 ancestry gate는 현재 BLOCKER**입니다. `>=1` 같은 느슨한 assertion으로 대체하지 않습니다. 위 정상 첫 pass의 `execute=4`와 별개입니다.

실제 Die/Git 검증 시험은 fake executor 증거와 따로 기록하고, 실제 GitHub/live 검증은 이 W3 matrix에 포함하여 통과 처리하지 않습니다.

**5. 좁은 whitelist와 구현 순서**

최소 생산 변경 후보:

- `crates/foundry-ledger/src/product/result.rs` — 신규 result writer/read/validation.
- `crates/foundry-ledger/src/product.rs` — module, v3 migration/open guard, 신규 승인 결속.
- `crates/foundry-ledger/src/product/review.rs` — exact run mapping·terminal evidence helper의 좁은 공유, admission 충돌 검사.
- `crates/foundry-ledger/src/lib.rs` — 공개 타입 export.
- `crates/foundry-serve/src/artifact_http.rs` — result-bound authenticated reads.
- `crates/foundry-serve/src/approval_service.rs` — 필요한 경우 결과 오류 연결만.
- 위 matrix의 ledger/line/serve 시험 파일.
- `crates/foundry/tests/b1_scope_cli.rs` — actual server gate가 승인된 whitelist에 포함될 때.
- `.specs/edit-spec-b1.md`, `docs/design/b1-product-contract.md`, 부모 proposal — **이번에는 수정하지 않음**, 동결 문서 후보.

`foundry-line/src/product.rs` 또는 `review_host.rs`는 실제 orchestration 연결에 필요한 좁은 변경만 추가합니다. 최소안에서는 새 exec API, dependency, `Cargo.lock`, domain transition 변경이 필요하지 않습니다. 기존 frozen pack, standalone 타입·digest·golden fixture, 광범위한 schema/refactor는 제외합니다.

순서:

1. 발견사항과 BLOCKER를 부모가 직접 재검증하고 계약 동결.
2. 비어 있지 않은 chain digest, wire mismatch, completed-claim 반례를 시험으로 고정.
3. v3 migration과 immutable receipt 저장.
4. 저장된 genuine host 결과를 검증하는 finalizer와 crash/replay.
5. result-bound 인증 읽기와 독립 변조 시험.
6. 신규 승인 소비부 전환 및 원자성 시험.
7. 별도 프로세스 권한 경합, 실제 loopback server, 기존 standalone 회귀.
8. 부모가 W3 인수를 판정한 뒤 W4로 진행.

W3를 막는 것은 producer→binding→인증 읽기·승인 연결 및 위 계약 미결정입니다. **durable publish-start/outcome, target repository/base/head 검증, `PublishApproved`, remote reconciliation, unknown outcome, 명시적 새 retry, 실제 GitHub 호출은 엄격히 W4 이후**입니다.

**6. 동결 전 BLOCKER와 부모 권고**

- **BLOCKER — 권한:** `DriveReview`가 새 로컬 결과 확정을 허용하는지, 인간 acceptance revision 변경 후 pending 결과의 recovery 정책을 승인해야 합니다.
- **BLOCKER — identity:** 제품 numeric run을 다른 activation에서 재사용하지 않는 규칙과 기존 alias 충돌 처리에 동의해야 합니다. 기존 후속 allocator는 재사용합니다.
- **BLOCKER — lifecycle:** genuine queued host 성공과 `waiting` 승인 후보 사이를 P2 입력 adapter가 준비할지, 제품 lifecycle producer를 이번 범위에 넣을지 결정해야 합니다. 현재 finalizer에 임의 S1 전이를 숨기면 안 됩니다.
- **BLOCKER — field/domain:** 제안한 payload 필드 순서, hash domain, full commit 형식, `artifact_location`의 evidence-path 의미를 동결해야 합니다.
- **BLOCKER — legacy:** v3 migration, 역사 digest/wire 보존, 신규 승인에서 legacy fallback 금지, cutoff 분류를 승인해야 합니다.
- **BLOCKER — 복수 generation 인수:** 실제 continuation 양성 경로, inherited artifact 집합, 정확한 stage 호출 수를 고정해야 합니다. 초기 product `save_redrive_output`은 현재 unsupported이므로 지원한다고 보고하면 안 됩니다.
- **BLOCKER — 증거 수준:** trusted host 결과 결속을 B1 P2의 W3 인수로 인정하는 범위를 명시해야 합니다. 이를 실제 Die나 원격 repository identity 검증으로 확대 해석하면 안 됩니다.

기계적인 선택은 helper 분리, private struct 배치, SQL 컬럼 순서, export 위치입니다. 위 권한·identity·lifecycle·legacy 의미 변경은 기계적인 선택이 아닙니다.

부모에게는 **REFINE 후 재검증**을 권고합니다. 특히 emit의 로컬 의미, 두 digest encoding, `EmitReceipt.key`와 제품의 `tuple` 소비 차이, 완료된 claim 상태를 먼저 직접 확인하십시오. 문서를 정정하고 인간 contract freeze를 받은 다음 luna 구현으로 넘겨야 합니다. 현재 상태는 구현 준비 완료나 W3 완료가 아닙니다.

## B. Narrow refinement (astra; supersedes A)

**Verdict: REFINE, narrowly.** The completion branches and fixture counts are mechanically determined. Two additions must enter the parent spec explicitly: persisted original review authority evidence, and a P2 preparation port if preparation must use ProductStore APIs exclusively.

Read-only inspection at HEAD `36e994c`; task, proposal, and previous final response read. Graph report/wiki were absent. No edits, tests, probes, database opens, credentials, external calls, skills, or agents were used. All contracts and tests below are **PROPOSED / NOT EXECUTED**.

**REFINED PACKAGE CONTRACT**

**1. Completed-result validation — PT-07/PT-08 corrections**

The three completion paths leave different durable states:

| Evidence after completion | Initial success | Accepted Resume success | Accepted CommitPass |
|---|---|---|---|
| Journal run status | `passed` | Follow-up `resolved`; origin remains `held` | Accepted hold’s originating run becomes `resolved` |
| Numeric host result | Inserted | Follow-up copy updated | Origin result updated |
| Result eligibility | `exit_code=0`, `held=None` | Same | Same, through explicit human override |
| Work claim/backlog | Both `completed` | Both remain `claimed` | Both remain `claimed` |
| Work lease | Replaced with completion time | Unchanged | Unchanged |
| Work owner/epoch/fence | Retained | Original binding copied to follow-up | Original binding retained |
| Hold progress | Absent | `completed` | `completed` |
| Active host binding | Original key/epoch/journal | Follow-up key, unchanged host epoch | Origin key retained |
| S1 write by completion | None | None | One `InspectionPassed`, `waiting → waiting` |

Sources: `complete_review_dispatch`, lines 717–823; `complete_continuation`, 1232–1392; `finalize_accepted_hold`, 1799–1902 in [product/review.rs](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:717).

The available authority evidence is:

- `product_review_work_bindings_v1`: lifecycle, delivery, worker, claim epoch/fence, connector source revision, payload digest, task kind and snapshot, keyed by order/attempt/journal run.
- `product_service_work_bindings_v1`: service ID/revision, credential revision and source instance, keyed by order/lifecycle. **Mutable on a subsequent work claim**, not an immutable per-run authority receipt.
- Intake source bindings: original source instance, connector revision, source-instance revision and generation.
- Hold acceptance: original actor/account authority revision, source instance, human scope revision and `ResolveReviewHold` grant revision.
- Completed hold progress: owner, recovery epoch, fence, reserved run and progress revision. These are retained after the completion CAS; they are **not embedded in `HostResult`**.
- Intent/active/epoch rows: host epoch, independently of worker claim epoch and hold-progress recovery epoch.

**Absent evidence:** the original `DriveReview` **grant revision** is not captured in `ProductServiceContext`, `ReviewServiceBinding`, work-service binding or run binding. Current grants contain a revision, but reading it during finalization would manufacture historical evidence. Human credential revision is also not in hold acceptance; however, the existing credential-reset API increments account authority revision, so its rotation is already detectable through the recorded account revision.

Sources: [schema and work-binding definitions, lines 809–875](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:809), [claim/service binding writes, 4161–4196](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4161), [service context and validators, 71–88 and 297–361](/home/cosmo/foundry/crates/foundry-ledger/src/product_service.rs:297), [human authority validation, 1953–2022](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1953).

Proposed additive persisted evidence, private to ProductStore:

```rust
struct ReviewAuthoritySnapshot {
    service_id: String,
    service_revision: i64,
    credential_revision: i64,
    drive_review_grant_revision: i64,
    source_instance_id: String,
    source_generation: i64,
    source_instance_revision: i64,
    connector_revision: i64,
}
```

Store this in an immutable `product_review_authority_v1` row keyed by the exact persisted host key, linked to its existing work binding. Capture it inside the validated admission transaction; copy the original snapshot during accepted Resume materialization. Do not refresh it from newer grants. Existing history lacking this evidence remains history and cannot satisfy the strict proposed original-revision policy through inferred backfill.

The existing service management API advances service revision and invalidates credentials on grant changes, so ordinary revoke/regrant is already detectable through those revisions. That does **not** establish the separately requested original grant revision. See [manage_product_service, 142–165](/home/cosmo/foundry/crates/foundry-ledger/src/product_service.rs:142).

Retain the prior proposed public finalizer signature:

```rust
ProductStore::finalize_product_review_result(
    &mut self,
    service: &ProductServiceContext,
    expected: &ProductResultExpectation,
) -> Result<ProductResultInfo, ProductResultError>
```

Add a private validator with this proposed signature; it grants no execution authority:

```rust
fn validate_completed_review_in(
    tx: &rusqlite::Transaction<'_>,
    service: &ProductServiceContext,
    key: &foundry_exec::HostKey,
    result: &foundry_exec::HostResult,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), ProductResultError>
```

Its exact new-finalization sequence should be:

1. Validate coordinate conversions; begin `IMMEDIATE`; obtain the store clock time; validate contract/installation.
2. Check current caller `DriveReview` for the **original** source. Handle an existing immutable finalization through the separate replay contract.
3. Verify S1 chain/replay. Require `waiting`, exact lifecycle and actual attempt, admission receipt/hash, original source/repository and delivery payload binding.
4. Join numeric host acceptance/result to persisted intent. Reconstruct `"{order}:{attempt}:{run}:{activation}"`; compare active key, journal and host epoch, plus the stored epoch row. Never parse a “latest run” fallback.
5. Decode journal/numeric results and require equality plus the expected host digest. Require `passed` or eligible `resolved`, zero exit and no held result. **Do not require `hold_json` or `hold_instance_id` to be NULL:** CommitPass retains hold history.
6. Choose a completion branch:
   - Initial pass: claim **and** backlog must be `completed`.
   - Resume/CommitPass: claim **and** backlog must remain `claimed`; require the exact acceptance, reservation, frozen hold and `completed` progress.
   - For Resume, reservation follow-up equals this run. For CommitPass, acceptance origin equals this run; its reserved successor need not exist.
7. In either branch compare the original work binding to current claim owner/epoch/fence/source revision/payload and backlog delivery. Compare task snapshot/digest to immutable admission. **Never test worker `lease_until > now`, renew it, or change claim state.** Reclaim, including the same worker name with a newer epoch/fence, rejects the old result.
8. Validate current service/credential validity and exact original snapshot revisions, including the persisted grant revision. Current service lease and credential expiry still matter; worker lease does not.
9. Walk every accepted ancestry operation. Check original human account/scope/grant revisions using the existing authority logic; verify reservation/progress coordinates, completed phase, retained owner/fence and frozen snapshot digest. Completed progress is the existing transactional completion evidence, not a new claim. Do not equate its recovery epoch with host/work epochs or the globally current approval-recovery epoch.
10. Validate nonempty generation chain, root admitted result commit, consecutive indexes, final commit, each producing stage/artifact and each verification target. Validate artifact bytes/provenance across the proven ancestry.
11. Check tuple-key occupancy and atomically insert tuple/result binding. No lifecycle, claim, reservation, stage or budget mutation.

`ensure_active_fence`, `ensure_new_work`, `ensure_resuming_fence`, `ensure_product_claim` and `review_work_service_valid_in` cannot substitute for this validator: they require running/resuming/continuing or live claimed work. Their exact conditions are in [review.rs, 2030–2374](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2030).

Two earlier assertions need narrowing:

- **CommitPass persistence does not currently recheck the original work-owner fence.** Its finalization checks the accepted-hold claim, then writes the outcome. Therefore “reassigned worker can never store any old terminal result” is too broad. The new result finalizer must independently reject that result.
- Continuation restores a saved session **before** reserving the next stage. Revocation can therefore produce zero additional `execute` calls but one `restore_session` call. Zero executor calls must not be misreported as zero executor interaction.

**2. Exact continuation fixtures — PT-03/PT-06 corrections**

Use the existing, unchanged [skills/bmad-review-full/v1/policy.toml](/home/cosmo/foundry/skills/bmad-review-full/v1/policy.toml:1). No new pack or golden change is needed.

Common fixture:

- Fresh scoped test database; no prior reservations or work.
- Order `p2:A` for scenario A; independently `p2:B` for B.
- Lifecycle `1`, attempt `1`, numeric run `7`, activation `3`, host epoch `1`, intent generation `0`.
- Exact full SHA strings: `B = "a".repeat(40)`, `C0 = "b".repeat(40)`, `C1 = "c".repeat(40)`, `C2 = "d".repeat(40)`.
- External reference `review:{order}:1:{B}..{C0}`.
- Initial host explicitly configured with full source-provenance SHA `"e".repeat(40)`.
- Fake executor writes nonempty stage-specific bytes inside `task.workdir`; host creates `accepted-output`. Verify outputs report exactly `task.artifact_commit`; only replan/fix outputs produce commits. All restores succeed.
- Service remains valid for three hours, credential for two; work lease five minutes. All execution occurs before work expiry. Finalization uses an injected clock at six minutes.
- Reviewer has current `ReadReview` and `ResolveReviewHold` before either acceptance. No grant changes during positive execution.

Actual pack route and host sequencing come from [initial dispatch, 923–1114](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:923), [continuation, 1599–1799](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:1599), and [continuation sessions/completion, 1817–2071](/home/cosmo/foundry/crates/foundry-line/src/review_host.rs:1817).

| Run / sequence | Exact stage | Fake output affecting routing |
|---|---|---|
| 7 / 1–3 | `edge`, `verif`, `claims` | Successful lens outputs; no produced commit |
| 7 / 4 | `triage` | `intent_gap`, findings=true, inspected=`C0` |
| — | `route1 → human_gap` | loopbacks becomes 1; frozen hold H1 |
| — | Accept H1 `Resume("replan_full")` | Existing allocator reserves run **8** |
| 8 / 1 | `replan_full-g1` | role=`replan`, input/base/artifact=`C0`, produces `C1` |
| 8 / 2–4 | `edge`, `verif`, `claims` | Successful lens outputs at `C1` |
| 8 / 5 | `triage` | `patch`, findings=true, inspected=`C1` |
| 8 / 6 | `fixdie-g2` | role=`fix`, input/base/artifact=`C1`, produces `C2` |
| 8 / 7–9 | `edge`, `verif`, `claims` | Successful lens outputs at `C2` |
| 8 / 10 | `triage` | A: `pass`, findings=false. B: `intent_gap`, findings=true. Both inspect `C2` |

Precisely:

- Run 8 stage numbering starts at **1**, not 5: materialization copies result/counters/work binding, **not stage rows**.
- `replan_full` receives `inputs["plan_full"] == "[]"`: the current payload reader searches this new run’s stage rows, and no `plan_full` stage exists. This is an explicit fake-executor fixture, not proof of a real plan producer.
- `fixdie` receives one findings artifact: run 8 sequence 5 `triage`.
- Graph `revert(origin)` records engine state; this fixture does not prove a Git revert.
- Final journal `result_commit` column remains admitted `C0`; terminal `HostResult.result_commit` is `C2`. Comparing those two as final-head equality would reject genuine results.

The generation chain is exactly:

```text
(0, C0, "")
(1, C1, run8/exec/resume-1/3-replan_full-g1/0/accepted-output)
(2, C2, run8/exec/resume-1/3-fixdie-g2/0/accepted-output)
```

Here `runN` denotes `artifact_root / sha256("{order}:1:N:3")`. Ordinary stage paths are:

```text
run7/exec/3-{sequence}/0/accepted-output
run8/exec/resume-1/3-{sequence}/0/accepted-output
```

Tuple and artifact generation remain **0**; chain indexes are **0,1,2**.

Session accounting:

- Initial sessions: `{order}:trace`, `{order}:verif`, `{order}:triage`; edge/claims share trace. **Zero restores.**
- Resume restores those three sessions on their first use only. The later lens cycle reuses them without another restore.
- Replan session is `{order}:replan_full:fresh:{order}:1:8:3:resume-1`.
- Fix session is `{order}:die:fresh:{order}:1:8:3:resume-1`.
- Both redrive sessions are new, so neither restores.
- Final session rows: **3 on run 7 + 5 on run 8 = 8**.

**Scenario A — accepted Resume, fix, reverify, automatic success**

Run 8 finishes `resolved`, exit 0, held None. H1 stays `accepted`; run 7 stays `held`. Result artifact set, in order, is exactly run 7 sequences **1–4**, followed by run 8 sequences **1–10**. Four inherited artifacts are referenced, never re-registered. Final location is run 8 sequence 10’s accepted output.

Totals: **execute=14, restore=3, dispatch_count=1**, one accepted-continuation API call; two host acceptances/results/runs/work bindings; fourteen completed stages/artifacts; eight sessions; budget used 14; one reservation/acceptance/completed progress.

**Scenario B — multiple generations, second hold, CommitPass**

Use the same script, except final triage returns `intent_gap`. It raises H2 on run **8**; loopbacks becomes 2. First Resume progress is completed, but run 8 is still held.

List H2 through the public hold API, accept `CommitPass`, then call `continue_accepted_product_hold`. The allocator reserves **9**, while CommitPass resolves its **originating run 8**. No run-9 acceptance/result/run/stage is materialized.

Totals remain **execute=14, restore=3, dispatch_count=1**. There are two continuation API calls, two reservations/acceptances/completed progress rows, two accepted holds, and one additional S1/transition-receipt row. Chain, fourteen artifacts and final evidence path are unchanged. This is human acceptance of the held result, not an automatic verification pass.

For the separate direct-H1 CommitPass subcase: origin **7**, reserved **8**, only one materialized run; **execute=4, restore=0, dispatch_count=1**, four artifacts and three sessions.

With a fresh fence counter and no unrelated operations: work owner=`worker-owner`, claim epoch/fence=`1/1`; H1 acceptance uses recovery epoch/fence=`0/2`, continuation=`0/3`; completed progress revision=`3`. Scenario B’s H2 uses fences `4`, then `5`, also ending at progress revision `3`. These are distinct from host epoch `1`.

`dispatch_count` increments only in initial dispatch, not accepted continuation. Graph `EngineAction::Emit` in A is not an `EmitSink` call. All scenarios have sink publish **0** and publisher calls **0**.

**Unsupported truth retained:** initial redrive still calls unsupported `ProductStore::save_redrive_output` ([review.rs, 1011–1022](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1011)). Accepted continuation uses implemented stage-start/finish APIs. A direct initial `patch → fixdie` fixture cannot be called a successful product generation fixture.

**3. P2 preparation — exact boundary and sequence**

The real public sequence can provision identity/scope/service, persist/admit synthetic delivery, claim work, run the host and accept holds. It **cannot currently perform both preparation mutations**:

1. `waiting_unsupported → ready` backlog selection.
2. Durable S1 `queued → working → inspecting → waiting`.

The existing fixtures perform these through raw test SQL: [append_waiting_lifecycle and synthetic_product_claim, lines 44–115](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_restore.rs:44). The scoped fixture does likewise at `b1_service_review.rs:198`.

`Order::transition` publicly supports the three events, but only changes an in-memory order. ProductStore’s connection is private; it exposes no general lifecycle append API. `SqliteLedger::open` rejects ProductAtomicV1, so standalone append is not a solution. Sources: [domain transitions, 240–285](/home/cosmo/foundry/crates/foundry-domain/src/order.rs:240), [standalone guard, 33–56](/home/cosmo/foundry/crates/foundry-ledger/src/sqlite.rs:33).

For an exclusively ProductStore-API fixture, propose this **test-support-only**, fixed-purpose addition:

```rust
pub enum P2ReviewPreparation {
    Queued,
    Waiting,
}

// Available only with an explicit p2-test-support feature.
pub fn prepare_p2_review_fixture(
    &mut self,
    service: &ProductServiceContext,
    admission: &AdmissionReceipt,
    delivery_id: &str,
    target: P2ReviewPreparation,
) -> Result<(), ProductError>;
```

It must validate current original-source service authority, exact immutable admission/delivery and fresh queued/unclaimed state. In one transaction:

- CAS exactly one backlog row from `waiting_unsupported` to `ready`.
- `Queued`: append zero S1 events.
- `Waiting`: use `Order::transition` for `DieStarted` at attempt 1, `DieSucceeded { attempt:1 }`, then `InspectionPassed`; append three correctly hashed S1 rows with three transition intents/receipts. Intents are consumed by the existing trigger.
- Write no review result, tuple, promotion or finalization rows; allocate no work/run claim.
- Reject stale/repeated preparation without partial writes.

The narrow dependency change is feature/export plumbing in ledger and enabling that feature for the line integration test. No new external crate is necessary. Without this addition, describe the fixture honestly as **public APIs plus explicit test-SQL preconditioning**, not an existing all-public API path.

The future setup order is:

1. `bootstrap_owner_scoped → login → create_account → change_product_scope` for reviewer `ResolveReviewHold` and `ReadReview`.
2. `manage_product_service → issue_product_service_credential → authenticate_product_service_for_action`, granting the original source `IntakeRead`, `AdmitDelivery`, `AckDelivery`, `DriveReview`.
3. Construct test-owned `ProductDelivery`/cursor with existing digest helpers; call `persist_intake_page_for_service → admit_intake_delivery_for_service → mark_intake_acked_for_service`.
4. Proposed `prepare_p2_review_fixture(Waiting)`.
5. `claim_product_work_for_service → PackReviewHost::with_product_store → try_dispatch_product_work`.
6. For holds: `list_product_review_holds → accept_product_review_hold → continue_accepted_product_hold`.
7. Proposed result finalizer.

Fresh preparation leaves one admitted order, one acked delivery, one ready backlog and **four S1/transition receipts total**: admission plus three preparation events. Claiming then adds one work claim and one service-work binding. Synthetic inputs and lifecycle events are expressly P2 test evidence, not a real intake-to-code producer.

**TRACEABILITY MATRIX**

All gates are **PROPOSED / NOT EXECUTED**. Paths:

- **H:** `crates/foundry-line/tests/b1_product_results.rs`
- **L:** `crates/foundry-ledger/tests/b1_product_results.rs`

`T/R` means tuple/result-binding rows. Every finalizer-only operation below adds **zero execute/restore/dispatch/sink/publisher calls**, zero S1 rows and zero claim mutations. Successful first finalization adds exactly `T/R=1/1`; rejection adds `0/0`; replay adds `0/0`.

| Future test / path | Setup → operation | Exact assertions and negative cases |
|---|---|---|
| `completed_initial_claim_finalizes_after_worker_expiry` — H | P2 Waiting; initial four-stage pass; advance injected clock past work lease → finalize/replay | Execute/restore/dispatch=`4/0/1`; stages/artifacts/sessions=`4/4/3`; claim/backlog remain completed with original completion lease. First `T/R +1/+1`, replay unchanged receipt. Failed or held result: reject, `+0/+0`. |
| `accepted_resume_binds_three_generations_and_fourteen_artifacts` — H | Scenario A → finalize run 8 | `14/3/1`; exact chain/path/order above; 4 inherited + 10 new artifacts; 8 sessions; one reservation, progress completed/revision 3; claim/backlog stay claimed. Origin run 7 or reserved-run mismatch rejects. |
| `commitpass_resolves_origin_without_materializing_reserved_run` — H | Scenario B → CommitPass → finalize run 8 | Execution counts stay `14/3/1`; CommitPass adds S1/receipt `1/1`, no stage/session/artifact/run. Reservations=2; run 9 absent; final tuple run=8/generation=0. Direct-H1 subcase asserts `4/0/1`, tuple run=7, run 8 absent. CommitFail/Abort are ineligible. |
| `completed_result_rejects_reclaimed_and_same_owner_aba` — H | Independently complete A/B; expire lease; reclaim with different/same worker ID → finalize old result | New claim epoch=2; fence A=4, B=6; backlog attempts increases once. Old stage/artifact/result rows preserved; `T/R +0/+0`. Covers CommitPass being stored despite missing work-owner check in its current persistence path. |
| `pending_result_requires_original_authority_revisions` — L | Valid initial/A/B completion; independently revoke/regrant service, rotate service credential, change human scope/account, or rebind source → finalize | `T/R +0/+0`; no host re-execution or receipt rewrite. Fresh context/new grant cannot replace original authority. Missing original authority snapshot also rejects; no inferred grant backfill. |
| `continuation_revocation_preserves_one_stage_before_next_start_denial` — H | A through H1; revoke service or human grant inside `replan_full-g1` execute → continuation/finalize | Totals execute/restore/dispatch=`5/1/1`; completed stages/artifacts=5, sessions=4, budget=5. Follow-up remains resuming; progress becomes recovery_required. Next edge restores trace once but executes zero times. `T/R=0/0`. |
| `result_chain_and_completion_evidence_fail_closed` — L | Independent clean A baseline for each mutation → finalize | Empty/root/index/final commit, produced commit, inspected commit, foreign fix artifact, final path, host/journal mismatch, active key/epoch, progress phase or owner/fence inconsistency each rejects with `T/R +0/+0`; all unrelated rows unchanged. Materialized resuming copy is ineligible. |
| `p2_preparation_walks_s1_without_result_seeding` — H | Public setup through admission → proposed Waiting preparation → real claim/host/finalize | Preparation: backlog update=1; S1/receipts `+3/+3`; pending intents=0; claims/results/tuples/promotions=0. Initial pass then gives `4/0/1` and `T/R +1/+1`. Queued variant adds zero S1 events and finalization rejects; stale admission/source or duplicate preparation rolls back all preparation changes. |
| `initial_product_redrive_remains_unsupported` — H | P2 Waiting; initial triage=`patch`; fake fix produces full SHA → dispatch | Execute/restore/dispatch=`5/0/1`; only 4 completed stages/artifacts, 3 sessions, budget=5. Unsupported save propagates; run failed, failure row=1, numeric host result=0, claim/backlog claimed. No persisted fix generation, tuple or finalization. **Approved narrow amendment Discord1561969234354114641 (option A): budget4→5 only for this initial unsupported-redrive observation. Charge each execution durably before starting it, including the fifth fix whose later save remains unsupported; enforce current authority/work identity and existing budget limit. No refund/reservation exception, unconditional success, or executor call inside a SQLite transaction. All other frozen observations and schema remain unchanged; historical budget4 citations are superseded for this scenario only.** |

Evidence scope for H is real ProductStore + real PackReviewHost/control flow + fake executor; L validates storage/contracts using genuine-host baselines. Neither establishes actual Die/Git execution, remote repository identity, external publication or executed regression coverage.

**Remaining blockers versus human freeze**

1. **Mechanical:** original `DriveReview` grant revision and immutable per-run service authority are missing. Add the explicit snapshot capture/copy contract; existing pending history cannot silently acquire it.
2. **Mechanical:** all-public P2 preparation is unavailable. Add the bounded test-support port/feature or explicitly retain test-SQL preconditioning in the evidence contract. The earlier unconditional “no dependency-surface change” assertion is inaccurate for the former.
3. **Human freeze:** approve current original-source `DriveReview` as local-finalization authority, exact original human/service revisions, expired-worker-lease acceptance, and no automatic recovery after rotation/regrant.
4. **Human freeze:** accept synthetic P2 lifecycle preparation and trusted-host/fake-executor evidence as this gate’s scope. Preserve the previously proposed additive maintenance migration and historical bytes.

The multi-generation stage script, inherited artifact set, run reservations and exact call counts are no longer an unresolved mechanical question. Implementation authorization and W3 acceptance remain outside this report.


## C. Mechanical closure amendments — overrides A/B (astra static review provenance)

**VERDICT: REFINE.** D1–D8 take precedence over B, then A. HEAD matches `36e994c`; graph report/wiki were not found. The following are proposed corrections only, based on static reads; no tests or execution probes were run.

1. **Authority snapshot: add the complete storage and capture contract.**

`admit_review_activation` receives `ReviewWorkClaim`, not `ProductServiceContext`. Its validated service binding supplies service ID/revisions/source ID; the enabled `drive_review` grant revision must be read inside that same admission transaction. Do not add a caller-supplied grant revision or reconstruct one during finalization. Sources: [admission](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:534), [service validator](/home/cosmo/foundry/crates/foundry-ledger/src/product_service.rs:306), [mutable work-service binding](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4194).

Proposed normative DDL—all symbols below are **new**:

```sql
CREATE TABLE product_review_authority_v1 (
  key_json TEXT NOT NULL PRIMARY KEY,
  order_id TEXT NOT NULL,
  lifecycle INTEGER NOT NULL CHECK(lifecycle > 0),
  attempt INTEGER NOT NULL CHECK(attempt >= 0),
  run_id INTEGER NOT NULL CHECK(run_id >= 0),
  activation INTEGER NOT NULL CHECK(activation >= 0),
  journal_run TEXT NOT NULL,
  host_epoch INTEGER NOT NULL CHECK(host_epoch >= 0),
  service_id TEXT NOT NULL,
  service_revision INTEGER NOT NULL CHECK(service_revision > 0),
  credential_revision INTEGER NOT NULL CHECK(credential_revision > 0),
  drive_review_grant_revision INTEGER NOT NULL
    CHECK(drive_review_grant_revision > 0),
  source_instance_id TEXT NOT NULL,
  source_generation INTEGER NOT NULL CHECK(source_generation > 0),
  source_instance_revision INTEGER NOT NULL
    CHECK(source_instance_revision > 0),
  connector_revision INTEGER NOT NULL CHECK(connector_revision > 0),
  UNIQUE(order_id,attempt,run_id,activation),
  UNIQUE(order_id,attempt,journal_run),
  FOREIGN KEY(key_json)
    REFERENCES product_review_bindings_v1(key_json),
  FOREIGN KEY(order_id,attempt,run_id,activation)
    REFERENCES product_review_host_accept_v1
      (order_id,attempt,run_id,activation),
  FOREIGN KEY(order_id,attempt,journal_run)
    REFERENCES product_review_work_bindings_v1(order_id,attempt,run_id)
);

CREATE TRIGGER product_review_authority_no_update
BEFORE UPDATE ON product_review_authority_v1
BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;

CREATE TRIGGER product_review_authority_no_delete
BEFORE DELETE ON product_review_authority_v1
BEGIN SELECT RAISE(ABORT,'review authority is immutable'); END;

CREATE TRIGGER product_review_authority_no_replace
BEFORE INSERT ON product_review_authority_v1
WHEN EXISTS (
  SELECT 1 FROM product_review_authority_v1 a
  WHERE a.key_json=NEW.key_json
     OR (a.order_id=NEW.order_id AND a.attempt=NEW.attempt
         AND a.run_id=NEW.run_id AND a.activation=NEW.activation)
     OR (a.order_id=NEW.order_id AND a.attempt=NEW.attempt
         AND a.journal_run=NEW.journal_run)
)
BEGIN SELECT RAISE(ABORT,'review authority already exists'); END;
```

The insert trigger prevents `REPLACE` bypasses without relying on recursive delete triggers. Revision columns intentionally have no foreign keys to mutable current revisions. Enable foreign keys on each connection before starting transactions, and retain exact application joins.

Replace the ambiguous capture instruction with:

> Initial admission performs checked coordinate conversion, `BEGIN IMMEDIATE`, store-clock acquisition, admission/work/service validation, and run-alias rejection before capturing authority. Read the service and credential revisions from the validated work-service binding; read the enabled original-source `drive_review` grant revision from `product_service_grants_v1`. Read source generation/instance revision from the delivery’s persisted intake source binding and require exact agreement with the current source instance; connector revision must independently equal the work claim, order, intake binding and current connector revision. Require verified order provenance and the exact original repository/source. Reject missing or zero provenance; never substitute current values for missing historical values.
>
> Insert run, work binding, numeric acceptance and intent binding, then the authority row, then active binding, in one transaction. Its canonical `key_json`, reconstructed journal string, lifecycle and host epoch must agree with those rows. Any failure rolls back the entire admission.
>
> Resume materialization copies the origin’s eight authority members unchanged after existing continuation validation. Only host coordinates/journal change; lifecycle and host epoch remain exact. Insert the copied authority after the destination’s work/acceptance/intent rows and before committing materialization/progress. Never refresh revisions. CommitPass creates no successor authority row.

The eight copied members are the service/source members in B’s `ReviewAuthoritySnapshot`. Capture timing is **review activation**, not intake or work claim. Existing Resume insertion order is at [review.rs:1711](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1711).

2. **Historical authority needs exact membership, independently of the approval cutoff.**

An admission/S1 cutoff cannot classify review activations: a new activation may use an old admission, and admission of a review activation writes no S1 event. `created_at` seconds and implicit SQLite rowids are unsuitable historical authority boundaries.

Add this **new** migration census:

```sql
CREATE TABLE product_result_legacy_hosts_v1 (
  order_id TEXT NOT NULL,
  attempt INTEGER NOT NULL,
  run_id INTEGER NOT NULL,
  activation INTEGER NOT NULL,
  PRIMARY KEY(order_id,attempt,run_id,activation),
  FOREIGN KEY(order_id,attempt,run_id,activation)
    REFERENCES product_review_host_accept_v1
      (order_id,attempt,run_id,activation)
);
```

Within migration, copy the exact existing numeric acceptance keys into this table. After filling it, install unconditional `BEFORE INSERT`, `BEFORE UPDATE` and `BEFORE DELETE` triggers, each raising `ABORT, 'legacy host census is sealed'`. Fresh v3 creates and seals an empty census. This records historical membership, **not historical authority**.

Normative classification:

> Missing authority for a census member is legacy-unattested: preserve history reads, reject new strict finalization with `Ineligible`, and never backfill. A later Resume descending through exact accepted reservations from such an unattested origin may preserve existing continuation behavior, but copies no authority and remains ineligible for strict finalization. This exception requires a complete, unambiguous ancestry ending at a census member; it is not inferred from admission sequence or time.
>
> Missing authority for every other new initial activation, or missing copy where its origin has authority, is `CorruptEvidence`. Broken/cyclic/ambiguous ancestry is corruption, not legacy. An authority row unexpectedly attached to a census member is also corruption.

Add an authority `BEFORE INSERT` trigger rejecting exact census membership, using the four numeric key columns and `RAISE(ABORT,'legacy authority cannot be backfilled')`. All authority writers must also enforce the ancestry rule.

This preserves D2 without silently changing historical continuation into newly authorized result production.

3. **Specify nested payload bytes and the local receipt.**

Keep PT-05’s outer member order and existing exec types. The following are **new private** types; field order shown is serialization order. Every structure uses `Serialize`, `Deserialize`, `deny_unknown_fields`; no defaults or omitted members.

```rust
struct ResultWorkV1 {
    delivery_id: String,
    worker_id: String,
    claim_epoch: i64,
    claim_fence: i64,
    source_revision: i64,
    payload_digest: String,
    task_kind: String,
    payload_snapshot: String,
    authority: ResultAuthorityV1,
}

struct ResultAuthorityV1 {
    service_id: String,
    service_revision: i64,
    credential_revision: i64,
    drive_review_grant_revision: i64,
    source_instance_id: String,
    source_generation: i64,
    source_instance_revision: i64,
    connector_revision: i64,
}

struct ResultAcceptedHoldV1 {
    operation_id: String,
    receipt_json: String,
    snapshot_digest: String,
    progress_revision: i64,
    owner_id: String,
    epoch: i64,
    fence: i64,
    reserved_run_id: i64,
}

struct ResultArtifactV1 {
    artifact_id: String,
    provenance_digest: String,
    artifact_sha256: String,
    order_id: String,
    lifecycle: i64,
    source_instance_id: String,
    repository_key: String,
    admission_seq: i64,
    admission_hash: String,
    attempt: i64,
    journal_run: String,
    generation: i64,
    stage_sequence: i64,
    stage_id: String,
    pack_digest: String,
    relative_path: String,
    output_json: String,
}
```

Outer types are `work: ResultWorkV1`, `accepted_holds: Vec<ResultAcceptedHoldV1>`, `artifacts: Vec<ResultArtifactV1>`.

Normative encoding/source rules:

- Encode with `serde_json::to_vec`, UTF-8, compact, no trailing newline. Preserve strings without trimming, Unicode normalization, path normalization or hex case rewriting.
- `receipt_json`, `output_json` and `payload_snapshot` are **JSON string members containing the exact persisted strings**, not reparsed JSON objects. Decode separately for validation.
- `work` comes from the persisted review work binding, not the current mutable service-work row. Its authority comes exclusively from the new snapshot. The outer source fields must agree with it.
- Accepted holds are ordered by proven causal ancestry: Resume operations root-to-leaf, followed by terminal CommitPass when present. Include no unrelated acceptance and no duplicate operation.
- Progress must be completed, have no recovery reason, and retain the recorded owner/epoch/fence/reserved run. Validate `owner_id == format!("{operation_id}:{epoch}:{fence}")`; do not compare that epoch to host/work epochs or the current global recovery epoch. Do not require production progress revision to equal fixture-specific `3`.
- Artifacts follow causal run order, then numeric stage sequence; deduplicate by artifact ID. `output_json` comes from the matching completed stage; `pack_digest` comes from its run. Require the exact completed-stage/artifact set, including inherited artifacts once.
- New typed payload bytes must round-trip exactly; reject unknown, duplicate, omitted or noncanonical members. Existing embedded receipt/output strings retain their historical encoding.
- Continue using `ArtifactTuple::digest()` directly; do not substitute the framing helper’s differently framed hash.

These fields exist in [work schema](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:861), [artifact registration](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2641), [progress owner creation](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1760), and [tuple digest](/home/cosmo/foundry/crates/foundry-exec/src/types.rs:175).

Define **new private** `LocalResultReceiptV1` with this exact wire/member order:

```text
{
  "version": u32 (=1),
  "kind": String (="local_review_result"),
  "installation_id": String,
  "database_id": String,
  "result_id": String,
  "order_id": String,
  "lifecycle": i64,
  "attempt": i64,
  "run_id": i64,
  "activation": i64,
  "generation": i64,
  "host_result_digest": String,
  "tuple_digest": String,
  "binding_digest": String,
  "final_artifact_id": String,
  "finalized_at": String
}
```

Use the same strict compact encoding. Every member equals the stored payload/binding projection; `finalized_at` is the original store-clock UTC RFC3339 string. No receipt hash is needed: `binding_digest` hashes the payload, which excludes this receipt. Persist receipt bytes once. The existing proposed APIs return `ProductResultInfo` projected from this validated set; “identical receipt replay” refers to the retained wire, not a new public return type. Neither emit nor promotion wire is accepted as this receipt.

Complete PT-04 with:

> All primary-key text columns are explicitly `NOT NULL`. Result binding references the tuple composite key and authority numeric host key. Add `UNIQUE(result_id,binding_digest,tuple_digest)`. Approval side binding references `product_outcomes_v1(operation_id)` and that exact result triple; insert it after the outcome in the same approval transaction. Apply update/delete rejection and duplicate-insert rejection covering every unique key to result bindings, approval bindings and product-owned tuples. Do not apply terminal immutability triggers to mutable held host-result rows.

4. **Separate new-finalization checks from stored-result validation and replay.**

Retain B’s completion branches, with these exact additions:

> For a new result, require current original-source `DriveReview`, then exact original service ID/service revision/credential revision/grant revision/source generation/instance revision/connector revision. Validate current service lease and credential expiry using store time. A fresh context does not replace the original snapshot. Never test, renew or rewrite the completed worker lease.
>
> Initial success requires claim and backlog `completed`; Resume/CommitPass requires both `claimed`. Compare owner/epoch/fence/delivery/payload to the original work binding in either branch. Validate original human account/scope/ResolveReviewHold revisions only for acceptances actually used.
>
> Reconstruct the journal from the persisted intent; compare numeric key, active key/epoch and epoch row. Obtain actual attempt from verified replay’s `Order`, not lifecycle, a parsed latest-run string or intent alone.
>
> CommitPass accepts retained `hold_json` and `hold_instance_id`. Verify frozen hold evidence against `product_review_holds_v1.snapshot_json`, not byte equality with the evolving run’s `hold_json`. Its terminal result is the origin’s updated result, and its reserved successor need not exist.
>
> The journal’s `result_commit` remains the initial admitted review input. Compare it with chain element zero; compare terminal `HostResult.result_commit` with the final chain element.

Existing replay helper omits actual attempt: [product.rs:4782](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4782). CommitPass retains evolving hold history and lacks a work-owner check in its persistence path: [review.rs:1825](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:1825).

Define a shared **new private stored-set validator**:

> Validate canonical payload/receipt, all derived IDs/digests, tuple equality, original authority/work records, immutable acceptance/frozen snapshots, completed progress, both stored terminal results, S1 anchors, artifact provenance and confined bytes in one DB snapshot. Historical validation does not require current active host, current claim ownership, `waiting`, or old authority still being live.
>
> Service replay/lookup first checks the caller’s current original-source `DriveReview`; then validates the stored set. It does not reapply new-finalization authority revisions. Human history reads instead require a current session and explicit original-source `ReadArtifact`, without requiring that source to remain current/enabled.
>
> Approval consumes that validated stored result plus its existing current human/source/lifecycle/actual-attempt/run requirements. Do not accidentally make approval depend on the old worker lease or re-execution. Result replay after approval must work although S1 is now `publishing`.

`lookup` returns `None` only after proving that the requested binding is absent with no contradictory strict tuple/binding evidence. Known legacy unbound tuples remain ineligible/conflicting evidence; a strict tuple without its binding is corruption. A persisted acceptance without terminal completion is not permission to dispatch.

Use one precedence everywhere:

```text
invalid public syntax/range -> InvalidInput
contract/version/installation incompatibility -> Unavailable
current caller authority failure -> Unauthorized
existing set malformed/inconsistent -> CorruptEvidence
valid existing identity, different expected digest/tuple alias -> Conflict
new-result source/lifecycle/owner/original-authority mismatch -> StaleBinding
nonterminal/failed/held/legacy-unattested -> Ineligible
new candidate chain/provenance/file mismatch -> InvalidEvidence
unique/CAS loss -> Conflict
unresolved COMMIT outcome -> CommitUncertain
```

Database access errors return `Storage` at the point encountered; never convert them to absence. Missing mandatory v3 schema is contract failure, not a cue to create it. Existing-set file corruption is `CorruptEvidence`; genuinely unavailable I/O is `Storage`. Without an existing winner, an incorrect expected host digest is `InvalidEvidence`, not necessarily `Conflict`.

All finalizer-only/replay/lookup/recovery operations make **zero execute, restore, dispatch, sink and publisher calls**, with zero S1 or claim mutations.

5. **Make migration/open/recovery atomic and history-safe.**

Proposed **new immutable metadata**, rather than freezing mutable `product_meta_v1.recovery_epoch`:

```sql
CREATE TABLE product_result_meta_v1 (
  singleton INTEGER NOT NULL PRIMARY KEY CHECK(singleton=1),
  installation_id TEXT NOT NULL,
  database_id TEXT NOT NULL,
  result_contract_version INTEGER NOT NULL CHECK(result_contract_version=1),
  legacy_approval_cutoff_seq INTEGER NOT NULL
    CHECK(legacy_approval_cutoff_seq >= 0),
  FOREIGN KEY(singleton) REFERENCES product_meta_v1(singleton)
);
```

Install update/delete rejection plus duplicate-singleton insert rejection. Verify installation/database equality in every open.

Replacement migration text:

> `migrate_product_results_v2` opens an existing file read/write without CREATE, validates ProductAtomicV1/config/product-v2/review-v1, and rechecks under `BEGIN IMMEDIATE`. It does not call normal `ProductStore::open`.
>
> Validate the source v2 schema and S1/approval relationships before changing anything. Create missing tuple storage with the existing seven columns, including `generations_json`; reuse only a compatible existing table. Reject incompatible schemas rather than rebuilding historical rows. Do not create promotion receipts or infer source/authority provenance.
>
> In this same transaction create result/approval/authority/meta/census schema and triggers; set cutoff to `COALESCE(MAX(ledger.seq),0)`; fill and seal the legacy-host census; insert metadata; CAS product schema version `2→3` last, requiring exactly one changed row. Commit once. Every failure rolls back all changes.
>
> Fresh databases initialize complete v3 with cutoff `0` and empty census. A repeated migration on fully valid v3 returns success without mutation; inconsistent/partial v3 fails closed.
>
> Existing-v3 normal open validates required tables, columns, indexes, triggers and metadata without schema repair or metadata insertion. Existing-v2 normal open rejects before mutations. A read-only connection may validate existing v3 without DDL or writable PRAGMAs; it cannot initialize or migrate. Do not claim the current write-opening constructor already supports this.

Current open performs additive source-column/schema writes, so merely changing `version == 2` to `3` is insufficient: [product.rs:735](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:735), [product.rs:840](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:840).

Exact cutoff/recovery rules:

> An approval outcome lacking a result side binding is legacy only when its sequence is `<= cutoff` and its original outcome, operation, approval S1 event/hash, transition receipt and consumption records agree. Cutoff compares approval outcome sequence, not admission/preparation time. A prepared pre-migration operation that commits afterward requires a strict binding.
>
> Above-cutoff missing side binding is corruption. Below-cutoff missing original outcome/consumption evidence is also corruption, not legacy. Validate these rules in ordinary approval replay and ambiguous-commit recovery, not only new approval.
>
> Preserve historical JSON/digest bytes and authentication rules. Do not require legacy outcomes to acquire new artifact evidence. Legacy outcome replay never grants new publication. Add a v3 rejection to the unscoped legacy publish-authorization helper; preserve its existing scoped rejection. Otherwise migrated unscoped receipts can still reach that helper.

The current approval early return and ambiguous-commit branch validate too little for the new side binding: [product.rs:3231](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3231), [product.rs:3405](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:3405). The unscoped publish path remains present at [product.rs:1165](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:1165).

6. **Close fixture accessibility and Cargo feature wiring.**

D4’s adapter can perform the prescribed preparation through existing domain transitions and ledger intent/receipt mechanics. It must also require the exact acked original delivery, verified provenance, current `DriveReview`, initial queued replay state, zero existing claims/review acceptances/reservations, and the matching backlog row. Its conditional backlog update must affect one row; all three transitions and their receipts commit atomically. Use an explicit synthetic-test actor/operation, without claiming real Die execution.

Existing fixtures currently use SQL for both preparation mutations: [b1_product_restore.rs:44](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_restore.rs:44).

Minimum feature declarations:

```toml
# foundry-ledger/Cargo.toml
[features]
default = []
p2-test-support = []

# foundry-line/Cargo.toml
[features]
default = []
p2-test-support = ["foundry-ledger/p2-test-support"]
```

Gate adapter module, type and export with `cfg(feature="p2-test-support")`. Gate new line integration targets with `required-features=["p2-test-support"]`; do not enable the feature unconditionally on normal or dev dependencies.

Two placement corrections are required:

- Move genuine-host-dependent L tests to H, and genuine-host-dependent X tests to `crates/foundry-line/tests/b1_product_result_processes.rs`. Keep pure encoding/schema tests in ledger. Ledger currently has no line dependency; do not introduce `ledger → line` to satisfy the proposed fixture matrix.
- To keep S/C positive gates self-contained in their proposed packages, add the same default-off forwarding feature to **both** `crates/foundry-serve/Cargo.toml` and `crates/foundry/Cargo.toml`, forwarding `foundry-ledger/p2-test-support`; gate only the new tests/modules. This is an explicit proposed whitelist addition beyond D4’s named ledger/line manifests. Alternatively retain that narrower whitelist and specify an external line-owned fixture producer and its handoff; no such handoff is presently defined.

Workspace resolver 2 does not make a feature a security boundary: workspace tests, explicit features or `--all-features` can unify ledger’s support feature into the graph. Therefore **no production handler, CLI command, startup branch, environment switch or deserializer may call the adapter**, even when the feature is compiled. Normal default builds omit it; enabled builds still expose no HTTP/CLI route.

Relevant dependency surfaces: [ledger manifest](/home/cosmo/foundry/crates/foundry-ledger/Cargo.toml:7), [line manifest](/home/cosmo/foundry/crates/foundry-line/Cargo.toml:7), [serve manifest](/home/cosmo/foundry/crates/foundry-serve/Cargo.toml:7), [CLI manifest](/home/cosmo/foundry/crates/foundry/Cargo.toml:11).

7. **Affected matrix rows and exact proposed gates.**

Paths: **H** `crates/foundry-line/tests/b1_product_results.rs`; **HX** `crates/foundry-line/tests/b1_product_result_processes.rs`; **L** `crates/foundry-ledger/tests/b1_product_results.rs`; **S/C** retain A’s paths.

Every positive producer baseline uses admission → proposed preparation → real claim/PackReviewHost → finalizer. No direct tuple/result/promotion seeds. Mutations are isolated negative cases after a verified positive baseline. Counts below remain static expectations.

| Correction / affected rows | Exact proposed test and assertions |
|---|---|
| PT-04/08; B authority row | **H `authority_is_captured_at_activation_and_copied_on_resume`**: zero snapshots at intake/claim; initial snapshot +1 atomically; Resume +1 with identical eight authority members; CommitPass +0; admission/copy failure leaves no partial rows; update/delete/REPLACE reject. |
| PT-12; B missing authority | **H `legacy_host_census_distinguishes_missing_authority_from_corruption`**: census-member and proven legacy-descendant absence → `Ineligible`; post-migration initial/copy absence → `CorruptEvidence`; no inferred grant; existing history remains readable. |
| PT-05 wire/digest | **L `result_payload_and_local_receipt_have_exact_v1_bytes`**: exact nested/member order, raw embedded strings, unknown/duplicate/missing rejection, stable receipt; emit/promotion wires rejected; nonempty tuple digest equals exec. |
| PT-08 completed claim/ABA | Retain **H `completed_initial_claim_finalizes_after_worker_expiry`**: `4/0/1`, stages/artifacts/sessions `4/4/3`, first T/R +1/+1, replay +0/+0, unchanged claim/lease. Retain A/B reclaimed-owner gate; initial completed backlog is not publicly reclaimable. |
| PT-03/06 ancestry | Retain **H `accepted_resume_binds_three_generations_and_fourteen_artifacts`**: `14/3/1`, 14 artifacts, 8 sessions, chain indexes 0/1/2, tuple generation 0, original authority copied. |
| PT-03/07 CommitPass | Retain **H `commitpass_resolves_origin_without_materializing_reserved_run`**: B stays `14/3/1`, origin 8/reservation 9; direct case `4/0/1`, origin 7/reservation 8; retained hold JSON accepted; successor absent. |
| PT-08 original revisions | Move **`pending_result_requires_original_authority_revisions`** to H. Service/credential/source changes reject initial/A/B. Human revision changes reject A/B **only when that acceptance is in the lineage**; an unrelated human change must not invalidate initial automatic success. |
| PT-09/10/11 replay | **H `stored_result_replay_survives_approval_and_original_authority_rotation`**: unchanged receipt after approval/current-authorized replay; no old-lease or waiting check; expired caller denied; tampered stored set rejected. Extend S historical-source read gate. |
| PT-09/11 recovery | **HX `approval_replay_and_ambiguous_commit_validate_result_side_binding`**: every participant present once or rollback; missing/mismatched side binding never returns success; fresh lookup errors never become absence. |
| PT-12 migration | Extend **L/X `v2_to_v3_migration_is_additive_atomic_and_old_binary_fenced`**: cutoff/meta/census/version atomic; historical bytes unchanged; repeated migration unchanged; v2 normal-open rejection; partial v3 rejection; read-only v3 validation performs no writes. |
| PT-12 legacy/publication | Extend **`legacy_receipts_remain_history_but_cannot_authorize_new_results`**: boundary `seq=cutoff` versus `cutoff+1`; pre-cutoff preparation/post-cutoff commit strict; historical wire unchanged; scoped and unscoped v3 legacy publication rejected. |
| PT-02/14 feature isolation | **H `p2_preparation_walks_s1_without_result_seeding`** retains +3 S1/+3 receipts, pending intents 0, claim/result/tuple/promotion 0 before claim. **S/C `p2_support_has_no_product_http_or_cli_entrypoint`** checks absent routes/commands with support enabled; separate default-feature compilation gate checks API absence. |

Delete or replace these obsolete claims/rows:

- A’s unresolved multi-generation-count blocker: replaced by D7/B’s exact A/B/direct-CommitPass scenarios.
- A’s `reassigned_owner_cannot_store_or_finalize_old_result`: split ordinary stage/completion rejection from CommitPass’s currently possible terminal persistence; **both** must fail new strict finalization after reassignment.
- A’s blanket “zero executor calls” on continuation revocation: distinguish `execute=0` from restoration. Retain B’s cumulative `5/1/1` revocation fixture.
- A’s generic reservation/ancestry/completed-lease rows: consolidate into B’s named exact scenarios above.
- B’s optional SQL-preconditioning fallback and A’s unconditional no-manifest-change claim: superseded by D4.
- Blanket wrong-expectation `Conflict`: use `InvalidEvidence` before a winner exists, `Conflict` against a valid existing winner.
- Any null-hold-history requirement or equality between final journal input commit and terminal head: delete.

**Mechanical blockers:** incorporate the authority DDL/capture/copy rules, exact legacy-host classification, typed payload/receipt, stored-set/recovery validation, atomic migration/open guards, and executable fixture/manifest placement.

**Pending human freeze:** D1–D8 remain candidate policy selections; this review neither freezes them nor authorizes implementation or completes W3.
