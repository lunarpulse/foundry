# B1 W3 Gate1 종결 루브릭 v1 (FROZEN)

채택: Discord 메시지 1562098677408075958 (lunarpulse_, 2026-10-20) — 결정 4건 전부 권고대로:
1. completed A/B public-reclaim 보류 해제 — 기존 API(claim_product_work_for_service, 명시적 worker_id)만 사용. 실패 시 정확 predicate 보고 후 재판정, 새 API 임의 생성 금지.
2. legacy-host 17행 — isolated faithful v2 historical fixture를 배치 범위에 추가. Gate3 전체 migration 판정은 별도 유지.
3. expiry 관측 — test-support 우선; 결정론 관측 불가 시 관측 전용·시험 전용 hook의 정확한 위치·내용을 승인 대상으로 제시. production 의미·public API·schema 변경 불허.
4. 본 루브릭(24행 + G 목록)을 마지막 종결 기준으로 채택. 최종 sol 리뷰는 루브릭 대조로 국한. 루브릭 외 신규 지적은 비차단 후속으로 기록.

아래는 astra 수렴 보고서 원문(판정 근거로 동결). 정정 사항: ADD70~72의 "5 executes, 1 restore, 1 finalize" 표현은 정본 기준 execute/restore/dispatch=5/1/1이며 세 번째 1은 dispatch_count — finalizer 호출은 별도 거절 단언으로 고정.

---

# 요약 판정

**CONVERGENT 가능 — 단, 현재 상태의 Gate1 종결은 불가합니다.**  
목표인 **luna 1~2배치 → 부모 검증 1회 → sol 리뷰 1회**는 아래 루브릭과 보류 항목의 처리 방식을 먼저 고정하면 가능합니다. **ADD74의 네 지적만 수정하면 Gate1 전체가 완료된다는 판단은 근거가 없습니다.**

사용자가 지적한 세 문제를 그대로 계획의 출발점으로 삼았습니다.

1. **발산 구조:** 라운드 68~76까지 부모 9레인 검증은 초록인데, sol 리뷰마다 시험 무효성·미측정 의무가 추가되어 REJECT가 반복되고 있습니다.
2. **루브릭 부재:** 46행 매트릭스는 있지만, Gate1 종결에 필요한 행별 단언 집합과 판정 경계가 동결되지 않았습니다.
3. **변경이 다른 것을 계속 부숨:** `review.rs`, `result.rs` 변경에 따라 `Option<HostResult>`, 중복 `artifact_tuples` 생성, migration·authority fixture 호환성 문제가 반복됐습니다.

다만 **검토한 round71·75의 최종 REJECT 항목에서, 리뷰어가 계약에 없는 새 기준을 발명했다고 판정할 근거는 찾지 못했습니다.** 핵심 문제는 기존 의무를 처음부터 완전하게 체크리스트화하지 않고 매번 일부만 검사했다는 것입니다.

이번 보고서는 다음 상태를 구분합니다.

- **PASS:** 해당 행의 명시된 증거가 소스와 보존된 실행 기록에서 확인됨. 이번 읽기 전용 작업의 실행 결과는 아닙니다.
- **EFFECTIVE-GAP:** 관련 시험은 있지만 필수 단언이 빠졌거나 다른 조건·메서드가 먼저 실패해 의무를 입증하지 못함.
- **UNMEASURED:** 요구된 시나리오의 유효한 실행 증거가 없음.

기준 HEAD는 `36e994c9897ffa42e56e294a8682009e854777f9`입니다. 작업 트리에는 진행 중인 luna76 수정이 포함되어 있습니다. 마지막 읽기 시각은 **2026-10-20 11:17 UTC**이며, 당시 H 시험 파일 SHA-256은 `fa62e388cdbc550a8ba93a3e18ef38ad9585daabd34c2343d158827145997eb9`입니다. **round75의 통과 로그를 이 수정본의 실행 증거로 사용하지 않았습니다.**

코드 수정·시험·빌드·커밋·보고서 파일 생성은 하지 않았습니다.

# 동결 루브릭 표

## 범위와 공통 규칙

46행 중 Gate1 소관은 원래 번호 기준 **24행**입니다.

`02, 03, 04, 05, 07, 08, 09, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 44`

이는 [Gate0의 행별 소관 분류](/home/cosmo/foundry/.hermes/plans/b1-w3-acceptance-evidence.md:106)와 [ADD67의 Gate1 범위](/home/cosmo/foundry/.specs/edit-spec-b1.md:1001)를 따른 것입니다. Gate2의 전체 consumer/root/byte 검증, Gate3의 전체 migration·COMMIT-I/O·feature isolation 행은 이번 종결 분모에 추가하지 않습니다. 단, **17행의 historical fixture 의존성과 22행의 명시적 보류는 없어지는 것이 아닙니다.**

시험 위치:

| 약호 | 정확한 대상 |
|---|---|
| H | [line/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs) |
| HX | [line/tests/b1_product_result_processes.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_result_processes.rs) |
| L | [ledger/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_product_results.rs) |
| U | ledger `--lib`, [result_wire_tests](/home/cosmo/foundry/crates/foundry-ledger/src/product/result.rs:2980) |
| F | [공유 genuine fixture](/home/cosmo/foundry/crates/foundry-line/tests/support/product_result_fixture.rs) |

표의 반복을 줄이기 위한 **정확한 기존 시험명**:

| 약호 | 시험명 |
|---|---|
| H0 | `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor` |
| HA | `accepted_resume_binds_three_generations_and_fourteen_artifacts` |
| HB | `commitpass_resolves_origin_without_materializing_reserved_run` |
| HR | `initial_redrive_remains_unsupported_and_cannot_finalize` |
| HW | `completed_initial_result_finalizes_after_worker_lease_expiry_without_renewal` |
| HQ | `queued_success_is_not_a_final_result_and_does_not_change_s1` |

**공통 필수 단언 C0**

각 시나리오에서 finalizer 호출 전후를 구분해 측정합니다.

- 최초 성공: tuple/result-binding 각각 `+1/+1`.
- 거절: 각각 `+0/+0`.
- replay: 동일 저장 receipt, `+0/+0`.
- finalizer-only 호출: execute/restore/dispatch 추가 `0`, S1·claim 변경 `0`.
- 변조 시험: 정상 대조군, 실제 변조 적용 확인, 정확한 오류, 관련 durable state 불변 확인. 서로 다른 변조가 누적된 상태로 다음 사례를 검사하지 않습니다.
- `is_err()`만으로 특정 실패 원인을 입증하지 않습니다. 특히 authority·identity·budget·SQL 오류를 구별합니다.

이는 새 정책이 아니라 [동결 traceability 공통 조건](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:961)과 C§4의 구체화입니다.

**A/B 공통 상수 C1**

- `p2:A` / `p2:B`, lifecycle/attempt `1/1`, initial run/activation/host epoch `7/3/1`.
- base=`a×40`, C0=`b×40`, C1=`c×40`, C2=`d×40`.
- H1 reserves `8`; B의 H2 reserves `9`, terminal origin은 `8`.
- A/B execute/restore/dispatch `14/3/1`; stages/artifacts/sessions/budget `14/14/8/14`.
- chain indexes `0/1/2`; tuple generation `0`.
- artifact 순서: run7 sequence1–4 다음 run8 sequence1–10.
- progress revision `3`; H1 acceptance/continuation fence `2/3`, B H2 `4/5`; progress epoch `0`과 host epoch `1`을 구분.
- exact generation paths, session IDs, `plan_full == "[]"`, fix 입력의 triage findings 1개는 [동결 B의 명시값](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:825)을 그대로 사용합니다.

`†`는 **현재 존재하는 시험이라고 주장하지 않는 신규 제안 시험명**입니다. 기존 시험을 확장해 같은 의무를 충족해도 되지만, 동결 시 최종 시험명과 subcase ID를 하나로 확정해야 합니다.

| 행 / 동결 매트릭스명 | 정확한 시험 연결 | 필수 단언 집합 | 현재 상태 |
|---|---|---|---|
| **02** `genuine_host_result_finalizes_without_emit_or_publish` | H::H0 | genuine initial; accept/result/run 각 1; `4/0/1`, stages/artifacts/sessions `4/4/3`; T/R `1/1`, promotion0; C0. 전체 publication 소비자 검증은 46행/Gate2 소관. | **EFFECTIVE-GAP** — 주요 positive는 통과. accept/result/run 전수와 finalizer·replay별 claim/저장 receipt 불변 연결이 불완전. |
| **03** `result_uses_persisted_numeric_run_and_activation` | H::H0, U::`result_coordinates_use_checked_persisted_widths` | colon order의 persisted intent/key/journal이 정확히 `order:1:7:3`; tuple run7/generation0, activation3; fallback 없이 일치. | **EFFECTIVE-GAP** — colon positive와 숫자 결과는 있음. persisted journal·intent·tuple의 명시적 상호대조가 부족. |
| **04** `activation_and_lifecycle_aliases_cannot_share_tuple_key` | H::H0; H::`activation_and_lifecycle_aliases_cannot_share_tuple_key`† | valid winner에 activation alias 및 lifecycle alias 각각 `Conflict`; corrupt winner이면 `CorruptEvidence` 우선; winner bytes/T/R 불변, 추가 실행0. | **EFFECTIVE-GAP** — activation 및 corrupt-winner 조합 있음. lifecycle alias 없음. |
| **05** `result_coordinates_reject_overflow_and_wrong_attempt` | H::H0, U::`result_coordinates_use_checked_persisted_widths`; H::동일 매트릭스명† | `i64::MAX+1`, 각 persisted width의 음수/불일치, verified replay의 실제 attempt 불일치; 정확한 오류, no wrap/fallback, 결과·승인 효과0. | **EFFECTIVE-GAP** — 현재 `u64::MAX` attempt와 요청 attempt/activation 위주. 정확한 경계·저장 음수·replay attempt 구별 미측정. |
| **07** `nonempty_generation_digest_matches_exec_and_preserves_legacy` | L::`typed_tuple_digest_uses_nonempty_exec_generation_encoding`, `single_generation_digest_uses_exec_struct_bytes_not_historical_value_bytes`, `explicit_v2_migration_censuses_history_and_is_idempotent`의 bytes 보존 단언 | single/multi typed digest 고정값; exec encoding 일치; historical Value encoding과 구별; 보존한 history bytes 불변. | **PASS** — encoding/보존 의무에 한함. 이 시험을 faithful-v2 migration PASS로 확대하지 않음. |
| **08** `result_payload_and_local_receipt_have_exact_v1_bytes` | U::`result_payload_has_exact_nested_v1_bytes_and_strict_work_members`, `local_result_receipt_has_exact_v1_bytes_and_strict_members`; H::동일 매트릭스명† | outer/work/authority/accepted-hold/artifact/receipt 순서와 raw embedded strings; unknown/duplicate/missing/noncanonical 거절; version1/kind 검증; persisted digest·receipt 안정성. | **EFFECTIVE-GAP** — exact encoding은 있음. negative는 outer/work 중심이며 나머지 nested 구조 및 실제 저장 경로의 version/kind·noncanonical 거절 연결이 빠짐. |
| **09** `emit_key_and_promotion_tuple_are_not_interchangeable` | U::`execution_emit_and_promotion_receipts_are_not_local_result_receipts`; H::동일 매트릭스명† | 실제 EmitReceipt/PromotionReceipt 직렬화 모두 local receipt로 거절; genuine 저장 결과에 대입해도 거절, fallback 승인/소비0. | **EFFECTIVE-GAP** — deserialize 거절만 있음. stored-result 경로의 거절·fallback 부재 미측정. |
| **10** `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` | H::H0, HA; H::동일 매트릭스명† | 독립 clean A에서 empty/root/index/final/fix-log/produced/inspected commit, foreign artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot·receipt 각각 검증; C0. | **EFFECTIVE-GAP** — 일부 연속 변조/복원은 있음. 독립 정상 대조와 inspected/foreign/coordinate/progress의 남은 단언 없음. |
| **11** `accepted_resume_binds_three_generations_and_fourteen_artifacts` | H::HA | C1 전체; two host acceptances/results/runs/work bindings; inherited artifact 재등록0; exact paths/session IDs; 원본 hold·authority·claim 보존; origin/reservation mismatch 거절; C0. | **EFFECTIVE-GAP** — 총수·causal 순서·receipt 대응은 있음. 고정 progress 값, 정확한 path/session/script 입력, 전체 producer census와 mismatch 사례 미완. |
| **12** `commitpass_resolves_origin_without_materializing_reserved_run` | H::HB | B C1; direct `4/0/1`, origin7/reservation8; CommitPass 전후 S1/receipt `+1/+1`, stage/session/artifact/run 추가0; successor의 run/accept/result/stage/authority 없음; allocator 추가 증가0; retained hold; CommitFail/Abort 불가. | **EFFECTIVE-GAP** — B/direct 성공·run 부재 있음. CommitPass 경계별 전체 delta, successor 전체 census, allocator, CommitFail/Abort 증거 부족. |
| **13** `result_location_must_resolve_registered_terminal_evidence` | H::H0; H::동일 매트릭스명† | terminal path, fallback `#run`, 다른 stage, external/unregistered path 각각 독립 정상 대조 → `InvalidEvidence`; caller path 채택0; C0. | **EFFECTIVE-GAP** — 세 변조는 있으나 동일 G-pre에서 반복하며 각 사례의 정상 finalization 대조와 전체 no-effect 증거가 없음. |
| **14** `held_failed_and_materialized_continuation_are_not_final_results` | H::HA/HB의 held 부분, HR; H::동일 매트릭스명† | held, failed, **권한이 유효한 copied-resuming 상태** 각각 `Ineligible`; hold/result/session 보존, T/R0/0. | **EFFECTIVE-GAP** — held/failed 있음. copied-resuming을 authority 오류와 독립적으로 검사하지 않음. |
| **15** `queued_success_is_not_silently_made_approvable` | H::HQ + H0 | queued 성공 결과는 `StaleBinding`, T/R0/0; waiting은 성공; 양쪽 finalizer S1 delta0. | **PASS** — 두 실제 경로와 S1 단언 확인. |
| **16** `authority_is_captured_at_activation_and_copied_on_resume` | F를 실행하는 H0/HA/HB, H::`activation_authority_captures_connector_revision_independently` | intake/claim0 → activation+1; service/connector unequal revision; Resume eight members exact+1; CommitPass+0; **activation 및 copy** rollback 전체; UPDATE/DELETE/REPLACE 거절. | **EFFECTIVE-GAP** — capture/copy 성공 있음. activation 실패 원자성 없음. copy SQL 비교는 luna76에서 수정됐으나 현재 실행 미확인. |
| **17** `legacy_host_census_distinguishes_missing_authority_from_corruption` | H::`legacy_census_corrupt_reachable_acceptance_is_corrupt_evidence`; H/HX::동일 매트릭스명† | faithful migrated census host·정확한 descendant missing authority → `Ineligible`; modern initial/copy missing → `CorruptEvidence`; corrupt/cyclic/ambiguous edge는 legacy로 숨기지 않음; no backfill/history 보존. | **UNMEASURED** — synthetic census-corruption 사례는 부분 증거. 정상 historical classification 및 현대 missing/copy 쌍의 완전 증거 없음. |
| **18** `completed_initial_claim_finalizes_after_worker_expiry` | H::HW | six-minute finalization, service/credential live, completed worker expired; `4/0/1`, `4/4/3`; first1/1, replay0/0 동일 receipt; claim/backlog/lease 불변. | **EFFECTIVE-GAP** — 첫 finalization과 claim 비교 있음. 동일 expired-worker baseline의 replay와 전체 관측 연결 없음. |
| **19** `pending_result_requires_original_authority_revisions` | H::`pending_result_rejects_service_grant_and_credential_rotation_without_rewriting_history`, `pending_result_rejects_source_rebind_without_writing_history`, `isolated_service_revision_and_credential_rotations_reject_the_captured_initial_authority`, `disabled_then_reenabled_service_rejects_its_old_captured_context`, `accepted_resume_rejects_isolated_service_and_credential_rotations`; H::동일 매트릭스명† | completed-pending initial/A/B 각각 service 변경·credential 변경·source 변경·revoke/regrant; fresh context도 original revision 대체 불가; lineage human revision만 적용; initial unrelated human change 허용; history/C0 보존. finalize와 lookup의 wrong-source/combined corruption 우선순위 포함. | **EFFECTIVE-GAP** — initial 일부 및 lookup denial 있음. A/B는 주로 **acceptance 후 continuation 진입 거절**이며 completed-pending strict finalization을 대체하지 못함. |
| **20** `revoke_after_stage_start_preserves_result_but_blocks_finalization` | H::`revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next`; HX::동일 매트릭스명† | **두 프로세스**에서 start commit 확인 → revoke/rebind commit → accepted stage completion; stage/session 보존, 다음 execute0, strict finalization 거절, T/R0/0. | **EFFECTIVE-GAP** — 현재 H는 thread 기반이며 stage 완료 후 restore에서 멈춤. 요구한 start-commit 순서의 결과 producer 증거가 아님. |
| **21** `revoke_before_stage_start_has_zero_executor_calls` | H::`revocation_between_hold_acceptance_and_continuation_blocks_pre_stage_work`; HX::동일 매트릭스명† | revoke commit → 실제 stage-start 시도; 신규 execute/stage/session/artifact/T/R 각각0; 이전 evidence 보존과 finalizer 거절. | **EFFECTIVE-GAP** — 현재는 continuation 진입 거절 및 일부 rows만 측정. stage 경계와 전체 vector가 빠짐. |
| **22** `reassigned_owner_cannot_store_or_finalize_old_result` | H::`direct_commitpass_reassignment_is_rejected_before_continuation`, `direct_commitpass_rejects_same_owner_epoch_fence_aba_before_materialization`; HX::동일 매트릭스명†; H::`completed_result_rejects_reclaimed_and_same_owner_aba`† | ordinary stale completion와 direct CommitPass를 분리; 둘 다 strict new finalization 거절; replacement claim 불변. completed A/B 각각 다른 owner·동일 owner ABA public reclaim, epoch2/fence A4·B6, backlog attempts+1, old evidence 보존. | **EFFECTIVE-GAP** — direct continuation 거절 두 건은 있음. 두 strict-finalizer 연결과 completed A/B public reclaim은 없음. ADD72 보류 유지. |
| **23** `initial_redrive_remains_unsupported` | H::HR 및 아래 charge 시험 5종 | redrive `5/0/1`, completed stage/artifact4/4, session3, budget5, failure1, failed/claimed, numeric result/T/R0. charge는 exact active running intent에서 order·stale work/run·revocation·limit·expiry·SQL rollback 검증. | **EFFECTIVE-GAP** — **redrive 본체는 PASS**. charge subcases의 원인 분리·동기화·rollback 증거가 미완. |
| **24** `continuation_revocation_distinguishes_restore_from_execution` | H::`revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next` 확장 | `replan_full-g1` execute 중 revocation commit; 누적 `5/1/1`, stages/artifacts/sessions/budget `5/5/4/5`; followup resuming, progress recovery_required; 다음 edge restore1/execute0; strict finalization 거절, T/R0/0. | **EFFECTIVE-GAP** — luna76에서 finalizer/recovery/T/R 단언 추가. 그러나 현재 pause는 stage 완료 후 restore이며 새 단언의 실행 증거도 없음. |
| **25** `interleaved_order_evidence_pairs_own_sequence_and_hash` | H::H0 | selected order 뒤 foreign append; selected sequence/hash가 같은 검증된 row; global head hash와 구별; 정상 finalize. | **PASS** |
| **44** `p2_preparation_walks_s1_without_result_seeding` | F를 실행하는 H0/HQ; H::동일 매트릭스명† | Waiting +3S1/+3receipts, Queued+0; preclaim intent/claim/result/tuple/promotion0; exact admission/current source; repeated/stale/invalid admission/source 및 conditional-row failure 전체 rollback. | **EFFECTIVE-GAP** — 성공 수치와 S1/backlog SQL-abort 있음. 반복·stale/invalid admission/source·조건부 갱신0행 사례 없음. |

**행 전체 기준: PASS 3, EFFECTIVE-GAP 20, UNMEASURED 1.**  
이는 생산 결함 21개라는 뜻이 아닙니다. 이미 통과한 부분 증거를 인정하면서, **행 전체를 닫기 위해 빠진 단언까지 포함한 집계**입니다.

# 잔여 갭 전수 목록

아래는 위 24행 중 미충족 **21행 전체**입니다. 이 목록 밖의 항목을 마지막 구현 배치에 구두로 추가하지 않아야 합니다.

| 갭 ID | 남은 작업 |
|---|---|
| **G02** | H0에 accept/result/run 각1, finalizer·replay별 claim 불변, 저장 receipt bytes 불변을 연결. |
| **G03** | colon order의 실제 persisted journal/intent/key/tuple을 한 번에 비교. 숫자 결과 반환만으로 대체하지 않음. |
| **G04** | lifecycle alias 추가. valid winner `Conflict`와 corrupt winner `CorruptEvidence`를 activation/lifecycle 각각 입증. |
| **G05** | 정확한 `i64::MAX+1` 경계, persisted negative coordinate, verified replay attempt 불일치 추가. 요청 attempt 변경 사례와 구분. |
| **G08** | authority/accepted-hold/artifact nested malformed 사례, 저장 payload/receipt의 version/kind/noncanonical 거절 및 digest/bytes 대응 추가. |
| **G09** | 실제 emit/promotion wire를 genuine stored receipt에 적용하는 독립 negative 추가. 결과 재사용·fallback 승인/소비 없음 확인. |
| **G10** | chain/ancestry mutation을 정상 대조가 있는 독립 사례로 정리. inspected commit, foreign fix artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot/receipt 및 좌표 불일치의 누락 사례 추가. |
| **G11** | A의 정확한 path/session/script input, generation index, progress revision/epoch/fence, 두 host의 accept/result/run/work census와 원본·followup 보존 단언 추가. origin/reservation mismatch 거절 포함. |
| **G12** | B/direct CommitPass 경계의 S1/receipt 증가와 나머지 무변경을 측정. unused successor의 전체 관련 테이블 부재·allocator 추가 증가0, CommitFail/Abort ineligible 추가. |
| **G13** | 각 path negative에 정상 finalization 대조와 복원 확인, 전체 no-effect 단언 추가. |
| **G14** | 유효 authority를 유지한 genuine materialized-resuming 상태에서 `Ineligible`를 직접 입증. revoked caller의 `Unauthorized`로 대체 금지. |
| **G16** | activation authority insert 실패의 전체 rollback 추가. Resume copy의 corrected numeric/text query를 실행 검증하고, run/accept/work/intent/active/authority/progress의 원자성을 연결. |
| **G17** | faithful historical host 및 descendant fixture, modern missing-initial/missing-copy 대조군 마련. 현재 synthetic census 변조는 해당 fixture를 대신하지 못함. **Gate3 fixture 의존성 결정 필요.** |
| **G18** | HW에서 최초 성공 뒤 같은 expired-worker 조건으로 replay; 동일 저장 receipt, T/R 증가0, claim/lease 및 호출 vector 불변 추가. |
| **G19** | completed-pending initial/A/B에 대한 독립 authority 변경 표를 완성. 현재 A-service/B-credential continuation 거절 두 사례로 세 시나리오의 finalizer 의무를 닫지 않음. lineage human account/scope/ResolveReviewHold 변경과 initial unrelated-human 허용도 연결. |
| **G20** | 결과 producer의 두 프로세스 start-commit→revocation/rebind→completion 증거와 strict finalizer refusal 추가. 기존 service-review process 시험을 활용하더라도 **동일 baseline에 결과 단언을 연결**해야 함. |
| **G21** | revocation commit→실제 stage-start 시도의 경계와 신규 stage/session/artifact/T/R/execute 전체0, finalizer refusal 추가. |
| **G22** | ordinary/direct stale-owner 사례의 strict finalizer 거절·replacement claim 불변 추가. completed A/B × 다른 owner/동일 owner ABA public reclaim 네 사례는 보류 해제 후 구현. |
| **G23** | 아래 charge 상세 목록 전부. redrive 본체 단언은 유지. |
| **G24** | revocation을 `replan_full-g1` 실행 중에 commit하도록 pause 이동. followup run을 대상으로 finalizer 호출; `5/1/1`, `5/5/4/5`, resuming/recovery_required/T/R0/0를 같은 baseline에서 실행 검증. |
| **G44** | duplicate preparation, stale/invalid admission, stale source, conditional update 0행 rollback과 preclaim promotion0 추가. |

## G23 — ADD74 수정본에도 남아 있는 정확한 문제

현재 charge 관련 정확한 시험명은 다음과 같습니다.

1. `redrive_charge_rejects_wrong_order_and_stale_run_without_durable_changes`
2. `charge_budget_rejects_revoked_authority_for_the_active_running_run`
3. `charge_budget_rejects_exhausted_budget_without_charging`
4. `charge_budget_sql_abort_rolls_back_the_budget_write`
5. `charge_budget_samples_clock_after_waiting_for_begin_immediate`

현재 소스에서 확인한 잔여 문제:

- **wrong-order/stale-run 시험:** 완료된 `genuine_host_done` baseline을 사용합니다. wrong-order는 첫 검사에 도달하지만, stale-run 사례는 “정상 intent라면 charge 가능했음”을 입증하지 못합니다. active-running 정상 charge 대조군, stale run 및 stale owner/fence를 각각 분리해야 합니다.
- **revoked/exhausted:** luna76에서 `charge_budget` 직접 호출로 수정됐습니다. 현재 수정본 실행은 미확인입니다. 정확한 실패 원인과 budget/run/stage/artifact/session 불변을 동결해야 합니다.
- **SQL failure:** 현재 `BEFORE UPDATE` trigger와 generic `is_err()`입니다. 실제 주입 오류가 발생했는지 단언하고, 변경 후 abort 등으로 rollback을 입증할 수 있는 지점을 사용해야 합니다. 다른 precondition 실패를 SQL rollback 성공으로 세면 안 됩니다.
- **expiry:** 정상 charge 대조군과 정확한 running intent는 추가됐지만, 두 문제가 남습니다.
  - `started_tx.send(())`는 여전히 `charge_budget` 호출 **앞**입니다. 이를 수신했다고 BEGIN 대기에 들어간 것은 아닙니다.
  - clock을 `+3시간`으로 이동합니다. fixture의 work lease는 5분이며, [`ensure_new_work`는 service credential 검사 전에 live work claim을 검사](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2281)합니다. 따라서 **credential expiry가 아니라 worker expiry로 거절될 수 있습니다.**

expiry의 최종 필수 순서는 다음으로 고정해야 합니다.

`정상 running/claimed → credential만 만료될 수 있도록 수명 분리 → write lock 보유 → charge의 실제 BEGIN 시도 확인 → clock 이동 → lock 해제 → credential expiry 거절`

동일 설정에서 clock을 이동하지 않으면 정확히 한 번 charge되어야 합니다. 이 관측 지점을 시험 코드만으로 만들 수 없다면, **관측 전용 hook의 허용 여부를 결정할 문제**이지 `sleep`이나 호출 전 신호로 PASS 처리할 문제가 아닙니다.

근거: [현재 charge 시험](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs:1374), [expiry 시험](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs:1717).

## round71·75 REJECT 분류

분류는 다음 의미로 사용합니다.

- **(a)** 동결 계약 또는 승인된 addendum의 구현·증거 보존 의무 위반.
- **(b)** 기존 의무를 입증한다고 제시된 시험의 무효성·불완전성 입증.
- **(c)** 계약·승인 addendum에 근거가 없는 신규 기준.

| 로그 / 지적 | 분류 | 판단 |
|---|---|---|
| round71-1: `charge_budget`가 BEGIN 이전 시간을 사용 | **(a)** | 현재 authority/expiry를 serialized transaction에서 판정해야 한다는 기존 의무의 구현 결함. |
| round71-2: accepted-edge 검증 전 legacy `Ineligible` | **(a)** | C§2는 완전하고 명확한 ancestry를 요구하며 broken ancestry는 corruption으로 규정. |
| round71-3: redrive dispatch1 단언 삭제 | **(a)+(b)** | ADD68의 단언 보존 규칙 위반이며, terminal이 없는 경우 shared helper가 대신 검사하지 못함. |
| round71-4: TEXT journal run을 integer8로 조회 | **(b)** | orphan run이 있어도 0이 되는 vacuous assertion. |
| round71-5: accounting·authority·barrier·reclaim 등 미측정 | **(a), 증거 측면에서는 (b)** | ADD67/68 및 동결 B/C에 이미 존재. 이 항목은 새로운 실패 시험 발견이라기보다 필수 coverage 누락 지적. |
| round75-1: expiry 시험이 stale run에서 먼저 실패; 호출 전 신호 | **(b)** | expiry 및 lock ordering을 검증하지 못함. |
| round75-2: revoked/exhausted/SQL 시험이 다른 메서드 실행 | **(b)** | continuation/start-stage 검증은 `charge_budget` 자체의 검증이 아님. |
| round75-3: numeric host-accept run에 journal string 비교 | **(b)** | round71의 text 비교 수정이 다른 테이블의 숫자 비교를 깨뜨림. |
| round75-4: continuation counts만 있고 finalizer/recovery/T/R 없음 | **(b)** | [동결 973행](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:973)에 이미 명시된 의무. |

최종 판정 원문: [round71](/home/cosmo/.hermes/cache/scratch/b1-sol-round71.log:12784), [round75](/home/cosmo/.hermes/cache/scratch/b1-sol-round75.log:8416).

**분류 (c): 확인된 항목 없음.**  
다만 ADD72 이후 completed public reclaim을 “이미 해제된 보류”처럼 다시 강제하는 것은 허용되지 않습니다. 의무는 남아 있지만 해당 라운드의 구현 범위에서는 명시적으로 보류됐습니다.

# 배치 계획

## 배치 전: 루브릭·작업 트리 고정

추가 구현 라운드를 시작하기 전에 다음을 한 번 확정합니다.

- 이 보고서의 24행 및 G 목록에 버전 부여.
- 각 subcase에 `행번호.단언번호` 부여.
- exact 시험명·예상 오류·barrier 순서·관측값·로그 위치 지정.
- luna76 완료본의 허용 파일 내용 hash 기록.
- 아래 사용자 결정 항목 처리.
- sol에게도 같은 루브릭을 제공. 새로운 자유 탐색 결과를 종결 기준으로 추가할 권한은 부여하지 않음.

이 단계에서 판단되지 않은 항목을 구현 마지막에 다시 발견하는 구조를 없애야 합니다.

## luna 배치 A — ADD74를 유효한 시험으로 마감

**작업:** G23, G24, G16의 copy-query 수정 검증. 기존 redrive·lookup·rotation·reassignment 단언 보존.

**허용 파일**

- `crates/foundry-line/tests/b1_product_results.rs`
- `crates/foundry-line/tests/support/product_result_fixture.rs`

**금지**

- 모든 production `src` 변경.
- schema/migration/API/manifest/lockfile 변경.
- SQL로 positive running/authority/result를 만들어내기.
- `is_err()`만으로 특정 오류 경로를 입증하기.
- 기존 continuation 시험을 charge 시험으로 교체하면서 원래 continuation 단언을 소실시키기.

**완료 조건:** 정상 control과 정확한 실패 원인, 실제 pause/barrier 도달, durable state 단언이 모두 연결됨. 이 배치는 네 지적의 “문자상 수정”이 아니라 **같은 지적이 재발하지 않는 증거**를 만들어야 합니다.

## luna 배치 B — 나머지 Gate1 증거를 한 번에 완성

**작업:** G02–G22, G44의 잔여 부분. 배치 A가 충분히 작고 결정이 완료돼 있다면 하나의 구현 배치로 합쳐도 됩니다.

**허용 파일 제안**

- H, HX, F, L.
- `.hermes/plans/b1-w3-acceptance-evidence.md`
- `.hermes/plans/b1-w3-recovery-acceptance-matrix.md`의 증거 연결 부분.
- private wire 시험 확장이 필요하면 `result.rs`의 **기존 `#[cfg(test)] result_wire_tests` 내부만** 별도 허용. runtime 코드는 동결.

**결정 이후에만 포함**

- G17을 위한 isolated faithful historical fixture.
- G22 completed A/B public reclaim.
- 꼭 필요한 charge lock 관측 hook.

**금지**

- production validator·authority·wire 의미 변경.
- schema/version/기존-v3 repair.
- host algorithm, packs, deps, manifests 변경.
- Gate2의 root/HTTP/socket 개선이나 Gate3 전체 migration/I/O 작업을 함께 끌어오기.
- assertion 삭제·ignore·no-op 또는 fixture 오염으로 통과시키기.

새 시험이 실제 생산 결함을 드러내면 **행/단언 ID와 실패 predicate를 제시하고 그 항목만 재판정**합니다. 이를 숨긴 채 “2배치 목표”를 달성해서는 안 됩니다.

## 부모 검증 1회

두 구현 배치 사이에는 필요한 좁은 시험만 수행하고, 최종 내용 hash가 고정된 뒤 부모가 전체 검증을 한 번 수행합니다.

| 레인 | 범위 |
|---|---|
| H/HX | enabled genuine result 및 process tests |
| ledger | result integration, 필요한 wire lib tests |
| HTTP enabled | auth + artifact targets |
| restore default | 기존 restore 책임 |
| restore enabled | 같은 책임의 enabled 조합 |
| CLI enabled | approval + scope |
| core default | exec/line/ledger/serve lib/tests |
| watch | 기존 두 B1 targets |
| CLI default | 기존 5개 B1 targets |

공통 조건은 기존대로 locked/offline, jobs1, incremental0, serial, free≥2GiB입니다. HX의 ledger executable은 **외부 Cargo JSON prebuild에서 정확히 추출**합니다.

부모의 산출물은 “9개 exit0”가 아니라 다음 둘입니다.

1. **24행 × 단언 ID → 실제 시험·로그·관측값 → PASS/GAP**
2. 기존 assertion의 old→new 보존 map

fixture 변경이 모든 consumer에 미치는 영향도 이 고정본에서 검증합니다. 이후 내용이 바뀌면 관련 레인의 증거만 무효화합니다.

## sol 리뷰 1회

sol은 부모가 완료한 동일 hash의 소스·루브릭·로그를 대조합니다.

- PASS: 모든 필수 단언 충족 및 허용 범위 준수.
- REJECT: 정확한 기존 단언 ID의 반례가 있을 때.
- DECISION-REQUIRED: 보류·계약 충돌·루브릭 자체 누락일 때.
- 참고 개선사항: Gate1 판정에 영향 없이 별도 기록.

# 발산 방지 장치

## ① 루브릭 외 신규 지적 금지 조항

다음 문구를 그대로 채택할 수 있습니다.

> **Gate1 판정 기준 동결**
>
> Gate1의 종결 분모는 본 루브릭의 24행과 명시된 단언 ID로 고정한다. 리뷰어는 각 REJECT에 기존 행·단언 ID, 승인된 근거, 현재 소스·시험의 반례를 제시해야 한다.
>
> 목록 밖의 새로운 시나리오, 조합, 관측값, 구현 방식 또는 품질 개선을 Gate1의 추가 필수 기준으로 만들 수 없다. 해당 제안은 비차단 후속 항목으로 기록한다.
>
> 기존 PASS를 무효화하려면 그 단언을 시험이 입증하지 못한다는 구체적 반례를 제시해야 한다. 이때 수정 범위는 해당 단언의 유효성 복구로 제한한다.
>
> 동결 계약에 실제 의무가 있으나 루브릭에서 누락된 사실을 발견하면, 이를 “루브릭 결함”으로 보고한다. 리뷰어가 일방적으로 종결 분모를 확대하지 않는다. 사용자/부모의 명시적 disposition과 루브릭 버전 변경 전에는 추가 구현을 지시하지 않는다. 누락된 의무를 충족했다고 허위로 인정하지도 않는다.
>
> 이미 보류된 항목은 보류 해제 전까지 자동 재요구하지 않는다. 보류는 PASS나 계약 삭제가 아니다.

## ② 시험–생산 인터페이스 고정 목록

| 경계 | 고정할 내용 | 방지하는 재발 |
|---|---|---|
| finalizer/lookup | 기존 public signature, `ProductResultExpectation`, 오류 우선순위 | 시험 편의를 위한 API·정책 변경 |
| fixture 반환값 | `terminal: Option<HostResult>` 유지. 성공 wrapper는 real terminal 필수, 실패 fixture는 `None + dispatch_error`; 빈 결과 fallback 금지 | HTTP/CLI의 순차 compile 파손 |
| fixture bootstrap | 기존 wrapper와 CLI-owned bootstrap callback, 원본 DB/config/root/identity 보존 | double bootstrap, 복사 DB·root로 맞추기 |
| 스키마 소유 | fresh v3가 tuple storage를 소유. 소비자 시험에서 `CREATE artifact_tuples` 재실행 금지 | duplicate table 실패 |
| run ID 타입 | journal run은 TEXT, host-accept run은 INTEGER. 별도 query 변수·helper 사용 | integer8↔journal string 교차 오용 |
| authority | intake/claim에서는0, activation capture, Resume exact copy; historical backfill 금지 | service-less fixture를 validator 완화로 복구 |
| legacy fixture | faithful-v2 fixture와 stripped-v3 negative fixture 구분 | migration chain의 가짜 positive |
| clock/pause | lifecycle 상태와 실제 도달 지점을 반환. 호출 전 신호를 operation 도달로 취급 금지 | expiry·barrier 시험의 vacuous success |
| features | P2 default-off, 기존 enabled/default 시험 책임 유지 | feature gating으로 시험 소실 |
| HX handoff | Cargo JSON executable, 기존 child transport·barrier token | 오래된 executable 또는 guessed path 실행 |

fixture 변경 전에는 F를 포함하는 H/HX/HTTP/CLI caller를 한 번에 목록화하고, compile compatibility를 먼저 확인해야 합니다. **현재 dirty tree의 전체 diff를 신규 변경으로 취급하지 말고, 배치 시작 hash 대비 변경만 검사**해야 합니다.

## ③ 새 의무와 기존 의무 무효 입증의 판정 규칙

| 상황 | 판정 | Gate1 처리 |
|---|---|---|
| 기존 단언은 credential expiry인데 실제로 stale run에서 먼저 거절 | 기존 의무 무효 입증 | 해당 단언만 재개방 |
| rollback 시험이 잘못된 SQL 타입으로 항상0 반환 | 기존 의무 무효 입증 | query와 정상 대조 복구 |
| frozen 계약에 있는 materialized-resuming 사례가 시험에 없음 | 기존 의무 미측정 | 현재 동결 목록에 넣어 한 번에 완료 |
| “더 강건하니 모든 오류를 모든 consumer와 교차시험하자” | 명시 근거 없으면 신규 기준 | 비차단 후속 제안 |
| counts `5/1/1`의 세 번째1을 finalization 호출 수로 해석 | 기준 오독 | 수정. 세 번째 값은 **dispatch_count** |
| recovery retry 때 restore를 한 번 더 요구 | frozen 누적1과 다르면 신규 기준/오독 | 요구 금지 |
| 기존 scope 밖 production 수정이 없으면 시험 관측 불가능 | 구현 범위 결정 | 정확한 hook/predicate만 사용자 판단 |
| 이미 보류한 completed public reclaim을 다시 즉시 요구 | scope 위반 | 보류 해제 결정 전 차단 지적으로 반복 금지 |

특히 ADD70~72의 “5 executes, 1 restore, 1 finalize” 표현은 정본의 `execute/restore/dispatch=5/1/1`과 맞지 않습니다. **루브릭에서는 dispatch로 정정하고, finalizer 호출은 별도 거절 단언으로 고정해야 합니다.**

# 사용자 결정 필요 항목

## 1. completed A/B public reclaim 보류 해제

ADD72는 public API 문제를 이유로 보류했지만, 현재 [`claim_product_work_for_service`](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4563)는 명시적으로 다음을 받습니다.

```rust
(context, worker_id: &str, now, lease)
```

또한 만료된 `claimed` backlog를 선택해 epoch/fence와 attempts를 증가시키는 경로가 존재합니다.

따라서 **“worker-owner 인자가 없어 새 API가 필요하다”는 설명은 현재 소스와 일치하지 않습니다.** 기존 API로 완료된 A/B의 reclaim을 구성할 가능성이 높습니다. 이는 정적 확인이며 실행 성공을 주장하지 않습니다.

**권고:** ADD72의 보류를 명시적으로 해제하고, 기존 API만 사용하는 네 벡터를 배치 B에 포함합니다. 실패하면 정확한 기존 predicate를 보고하며 새 API를 임의로 만들지 않습니다.

## 2. legacy-host 행의 Gate3 fixture 의존성

17행은 실제로 미측정입니다. 현 synthetic census 변조를 historical positive로 인정할 수 없습니다.

**권고:** isolated faithful historical fixture 작성에 한해 배치 B 범위를 확장하고, Gate3 전체 migration 판정은 그대로 남깁니다. 이를 허용하지 않으면 **Gate1 전체 PASS가 아니라 “mechanical bundle 완료, 17행 미종결”**로 보고해야 합니다.

## 3. expiry 관측 hook의 제한적 허용 여부

시험 전용 기존 도구로 실제 BEGIN 시도/대기와 clock sampling 순서를 결정적으로 관측할 수 없다면, 현 ADD74의 “모든 src 변경 금지”와 정확한 증명 사이에 범위 충돌이 생깁니다.

**권고:** 우선 test-support만으로 구현 가능성을 확인하고, 불가능한 경우에만 **관측 전용·시험 전용 hook**의 정확한 위치와 내용을 승인 대상으로 제시합니다. production 의미·public API·schema 변경은 허용하지 않습니다.

## 4. 이 루브릭을 마지막 종결 기준으로 채택

이 보고서는 **동결 제안**이며 스스로 계약을 변경하지 않습니다.

채택할 내용은 다음 네 가지입니다.

- 24행과 G 목록을 마지막 수정 배치의 고정 목록으로 사용.
- 현재 PASS 부분 증거는 보존하되, 수정된 fixture의 최종 고정본에서 재검증.
- G17/G22 및 관측 hook 결정을 배치 전에 처리.
- 최종 sol 리뷰를 기존 단언의 충족·무효성 판정으로 제한.

이 결정 없이 ADD74 네 건만 고치고 다시 자유형 sol 리뷰를 수행하면, **이미 문서에 남아 있는 다른 Gate1 의무가 다음 REJECT로 등장하는 구조가 그대로 유지됩니다.**
tokens used
147,541
# 요약 판정

**CONVERGENT 가능 — 단, 현재 상태의 Gate1 종결은 불가합니다.**  
목표인 **luna 1~2배치 → 부모 검증 1회 → sol 리뷰 1회**는 아래 루브릭과 보류 항목의 처리 방식을 먼저 고정하면 가능합니다. **ADD74의 네 지적만 수정하면 Gate1 전체가 완료된다는 판단은 근거가 없습니다.**

사용자가 지적한 세 문제를 그대로 계획의 출발점으로 삼았습니다.

1. **발산 구조:** 라운드 68~76까지 부모 9레인 검증은 초록인데, sol 리뷰마다 시험 무효성·미측정 의무가 추가되어 REJECT가 반복되고 있습니다.
2. **루브릭 부재:** 46행 매트릭스는 있지만, Gate1 종결에 필요한 행별 단언 집합과 판정 경계가 동결되지 않았습니다.
3. **변경이 다른 것을 계속 부숨:** `review.rs`, `result.rs` 변경에 따라 `Option<HostResult>`, 중복 `artifact_tuples` 생성, migration·authority fixture 호환성 문제가 반복됐습니다.

다만 **검토한 round71·75의 최종 REJECT 항목에서, 리뷰어가 계약에 없는 새 기준을 발명했다고 판정할 근거는 찾지 못했습니다.** 핵심 문제는 기존 의무를 처음부터 완전하게 체크리스트화하지 않고 매번 일부만 검사했다는 것입니다.

이번 보고서는 다음 상태를 구분합니다.

- **PASS:** 해당 행의 명시된 증거가 소스와 보존된 실행 기록에서 확인됨. 이번 읽기 전용 작업의 실행 결과는 아닙니다.
- **EFFECTIVE-GAP:** 관련 시험은 있지만 필수 단언이 빠졌거나 다른 조건·메서드가 먼저 실패해 의무를 입증하지 못함.
- **UNMEASURED:** 요구된 시나리오의 유효한 실행 증거가 없음.

기준 HEAD는 `36e994c9897ffa42e56e294a8682009e854777f9`입니다. 작업 트리에는 진행 중인 luna76 수정이 포함되어 있습니다. 마지막 읽기 시각은 **2026-10-20 11:17 UTC**이며, 당시 H 시험 파일 SHA-256은 `fa62e388cdbc550a8ba93a3e18ef38ad9585daabd34c2343d158827145997eb9`입니다. **round75의 통과 로그를 이 수정본의 실행 증거로 사용하지 않았습니다.**

코드 수정·시험·빌드·커밋·보고서 파일 생성은 하지 않았습니다.

# 동결 루브릭 표

## 범위와 공통 규칙

46행 중 Gate1 소관은 원래 번호 기준 **24행**입니다.

`02, 03, 04, 05, 07, 08, 09, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 44`

이는 [Gate0의 행별 소관 분류](/home/cosmo/foundry/.hermes/plans/b1-w3-acceptance-evidence.md:106)와 [ADD67의 Gate1 범위](/home/cosmo/foundry/.specs/edit-spec-b1.md:1001)를 따른 것입니다. Gate2의 전체 consumer/root/byte 검증, Gate3의 전체 migration·COMMIT-I/O·feature isolation 행은 이번 종결 분모에 추가하지 않습니다. 단, **17행의 historical fixture 의존성과 22행의 명시적 보류는 없어지는 것이 아닙니다.**

시험 위치:

| 약호 | 정확한 대상 |
|---|---|
| H | [line/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs) |
| HX | [line/tests/b1_product_result_processes.rs](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_result_processes.rs) |
| L | [ledger/tests/b1_product_results.rs](/home/cosmo/foundry/crates/foundry-ledger/tests/b1_product_results.rs) |
| U | ledger `--lib`, [result_wire_tests](/home/cosmo/foundry/crates/foundry-ledger/src/product/result.rs:2980) |
| F | [공유 genuine fixture](/home/cosmo/foundry/crates/foundry-line/tests/support/product_result_fixture.rs) |

표의 반복을 줄이기 위한 **정확한 기존 시험명**:

| 약호 | 시험명 |
|---|---|
| H0 | `genuine_host_result_finalizes_replays_and_preserves_selected_s1_anchor` |
| HA | `accepted_resume_binds_three_generations_and_fourteen_artifacts` |
| HB | `commitpass_resolves_origin_without_materializing_reserved_run` |
| HR | `initial_redrive_remains_unsupported_and_cannot_finalize` |
| HW | `completed_initial_result_finalizes_after_worker_lease_expiry_without_renewal` |
| HQ | `queued_success_is_not_a_final_result_and_does_not_change_s1` |

**공통 필수 단언 C0**

각 시나리오에서 finalizer 호출 전후를 구분해 측정합니다.

- 최초 성공: tuple/result-binding 각각 `+1/+1`.
- 거절: 각각 `+0/+0`.
- replay: 동일 저장 receipt, `+0/+0`.
- finalizer-only 호출: execute/restore/dispatch 추가 `0`, S1·claim 변경 `0`.
- 변조 시험: 정상 대조군, 실제 변조 적용 확인, 정확한 오류, 관련 durable state 불변 확인. 서로 다른 변조가 누적된 상태로 다음 사례를 검사하지 않습니다.
- `is_err()`만으로 특정 실패 원인을 입증하지 않습니다. 특히 authority·identity·budget·SQL 오류를 구별합니다.

이는 새 정책이 아니라 [동결 traceability 공통 조건](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:961)과 C§4의 구체화입니다.

**A/B 공통 상수 C1**

- `p2:A` / `p2:B`, lifecycle/attempt `1/1`, initial run/activation/host epoch `7/3/1`.
- base=`a×40`, C0=`b×40`, C1=`c×40`, C2=`d×40`.
- H1 reserves `8`; B의 H2 reserves `9`, terminal origin은 `8`.
- A/B execute/restore/dispatch `14/3/1`; stages/artifacts/sessions/budget `14/14/8/14`.
- chain indexes `0/1/2`; tuple generation `0`.
- artifact 순서: run7 sequence1–4 다음 run8 sequence1–10.
- progress revision `3`; H1 acceptance/continuation fence `2/3`, B H2 `4/5`; progress epoch `0`과 host epoch `1`을 구분.
- exact generation paths, session IDs, `plan_full == "[]"`, fix 입력의 triage findings 1개는 [동결 B의 명시값](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:825)을 그대로 사용합니다.

`†`는 **현재 존재하는 시험이라고 주장하지 않는 신규 제안 시험명**입니다. 기존 시험을 확장해 같은 의무를 충족해도 되지만, 동결 시 최종 시험명과 subcase ID를 하나로 확정해야 합니다.

| 행 / 동결 매트릭스명 | 정확한 시험 연결 | 필수 단언 집합 | 현재 상태 |
|---|---|---|---|
| **02** `genuine_host_result_finalizes_without_emit_or_publish` | H::H0 | genuine initial; accept/result/run 각 1; `4/0/1`, stages/artifacts/sessions `4/4/3`; T/R `1/1`, promotion0; C0. 전체 publication 소비자 검증은 46행/Gate2 소관. | **EFFECTIVE-GAP** — 주요 positive는 통과. accept/result/run 전수와 finalizer·replay별 claim/저장 receipt 불변 연결이 불완전. |
| **03** `result_uses_persisted_numeric_run_and_activation` | H::H0, U::`result_coordinates_use_checked_persisted_widths` | colon order의 persisted intent/key/journal이 정확히 `order:1:7:3`; tuple run7/generation0, activation3; fallback 없이 일치. | **EFFECTIVE-GAP** — colon positive와 숫자 결과는 있음. persisted journal·intent·tuple의 명시적 상호대조가 부족. |
| **04** `activation_and_lifecycle_aliases_cannot_share_tuple_key` | H::H0; H::`activation_and_lifecycle_aliases_cannot_share_tuple_key`† | valid winner에 activation alias 및 lifecycle alias 각각 `Conflict`; corrupt winner이면 `CorruptEvidence` 우선; winner bytes/T/R 불변, 추가 실행0. | **EFFECTIVE-GAP** — activation 및 corrupt-winner 조합 있음. lifecycle alias 없음. |
| **05** `result_coordinates_reject_overflow_and_wrong_attempt` | H::H0, U::`result_coordinates_use_checked_persisted_widths`; H::동일 매트릭스명† | `i64::MAX+1`, 각 persisted width의 음수/불일치, verified replay의 실제 attempt 불일치; 정확한 오류, no wrap/fallback, 결과·승인 효과0. | **EFFECTIVE-GAP** — 현재 `u64::MAX` attempt와 요청 attempt/activation 위주. 정확한 경계·저장 음수·replay attempt 구별 미측정. |
| **07** `nonempty_generation_digest_matches_exec_and_preserves_legacy` | L::`typed_tuple_digest_uses_nonempty_exec_generation_encoding`, `single_generation_digest_uses_exec_struct_bytes_not_historical_value_bytes`, `explicit_v2_migration_censuses_history_and_is_idempotent`의 bytes 보존 단언 | single/multi typed digest 고정값; exec encoding 일치; historical Value encoding과 구별; 보존한 history bytes 불변. | **PASS** — encoding/보존 의무에 한함. 이 시험을 faithful-v2 migration PASS로 확대하지 않음. |
| **08** `result_payload_and_local_receipt_have_exact_v1_bytes` | U::`result_payload_has_exact_nested_v1_bytes_and_strict_work_members`, `local_result_receipt_has_exact_v1_bytes_and_strict_members`; H::동일 매트릭스명† | outer/work/authority/accepted-hold/artifact/receipt 순서와 raw embedded strings; unknown/duplicate/missing/noncanonical 거절; version1/kind 검증; persisted digest·receipt 안정성. | **EFFECTIVE-GAP** — exact encoding은 있음. negative는 outer/work 중심이며 나머지 nested 구조 및 실제 저장 경로의 version/kind·noncanonical 거절 연결이 빠짐. |
| **09** `emit_key_and_promotion_tuple_are_not_interchangeable` | U::`execution_emit_and_promotion_receipts_are_not_local_result_receipts`; H::동일 매트릭스명† | 실제 EmitReceipt/PromotionReceipt 직렬화 모두 local receipt로 거절; genuine 저장 결과에 대입해도 거절, fallback 승인/소비0. | **EFFECTIVE-GAP** — deserialize 거절만 있음. stored-result 경로의 거절·fallback 부재 미측정. |
| **10** `finalizer_rejects_empty_foreign_and_unbacked_generation_chains` | H::H0, HA; H::동일 매트릭스명† | 독립 clean A에서 empty/root/index/final/fix-log/produced/inspected commit, foreign artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot·receipt 각각 검증; C0. | **EFFECTIVE-GAP** — 일부 연속 변조/복원은 있음. 독립 정상 대조와 inspected/foreign/coordinate/progress의 남은 단언 없음. |
| **11** `accepted_resume_binds_three_generations_and_fourteen_artifacts` | H::HA | C1 전체; two host acceptances/results/runs/work bindings; inherited artifact 재등록0; exact paths/session IDs; 원본 hold·authority·claim 보존; origin/reservation mismatch 거절; C0. | **EFFECTIVE-GAP** — 총수·causal 순서·receipt 대응은 있음. 고정 progress 값, 정확한 path/session/script 입력, 전체 producer census와 mismatch 사례 미완. |
| **12** `commitpass_resolves_origin_without_materializing_reserved_run` | H::HB | B C1; direct `4/0/1`, origin7/reservation8; CommitPass 전후 S1/receipt `+1/+1`, stage/session/artifact/run 추가0; successor의 run/accept/result/stage/authority 없음; allocator 추가 증가0; retained hold; CommitFail/Abort 불가. | **EFFECTIVE-GAP** — B/direct 성공·run 부재 있음. CommitPass 경계별 전체 delta, successor 전체 census, allocator, CommitFail/Abort 증거 부족. |
| **13** `result_location_must_resolve_registered_terminal_evidence` | H::H0; H::동일 매트릭스명† | terminal path, fallback `#run`, 다른 stage, external/unregistered path 각각 독립 정상 대조 → `InvalidEvidence`; caller path 채택0; C0. | **EFFECTIVE-GAP** — 세 변조는 있으나 동일 G-pre에서 반복하며 각 사례의 정상 finalization 대조와 전체 no-effect 증거가 없음. |
| **14** `held_failed_and_materialized_continuation_are_not_final_results` | H::HA/HB의 held 부분, HR; H::동일 매트릭스명† | held, failed, **권한이 유효한 copied-resuming 상태** 각각 `Ineligible`; hold/result/session 보존, T/R0/0. | **EFFECTIVE-GAP** — held/failed 있음. copied-resuming을 authority 오류와 독립적으로 검사하지 않음. |
| **15** `queued_success_is_not_silently_made_approvable` | H::HQ + H0 | queued 성공 결과는 `StaleBinding`, T/R0/0; waiting은 성공; 양쪽 finalizer S1 delta0. | **PASS** — 두 실제 경로와 S1 단언 확인. |
| **16** `authority_is_captured_at_activation_and_copied_on_resume` | F를 실행하는 H0/HA/HB, H::`activation_authority_captures_connector_revision_independently` | intake/claim0 → activation+1; service/connector unequal revision; Resume eight members exact+1; CommitPass+0; **activation 및 copy** rollback 전체; UPDATE/DELETE/REPLACE 거절. | **EFFECTIVE-GAP** — capture/copy 성공 있음. activation 실패 원자성 없음. copy SQL 비교는 luna76에서 수정됐으나 현재 실행 미확인. |
| **17** `legacy_host_census_distinguishes_missing_authority_from_corruption` | H::`legacy_census_corrupt_reachable_acceptance_is_corrupt_evidence`; H/HX::동일 매트릭스명† | faithful migrated census host·정확한 descendant missing authority → `Ineligible`; modern initial/copy missing → `CorruptEvidence`; corrupt/cyclic/ambiguous edge는 legacy로 숨기지 않음; no backfill/history 보존. | **UNMEASURED** — synthetic census-corruption 사례는 부분 증거. 정상 historical classification 및 현대 missing/copy 쌍의 완전 증거 없음. |
| **18** `completed_initial_claim_finalizes_after_worker_expiry` | H::HW | six-minute finalization, service/credential live, completed worker expired; `4/0/1`, `4/4/3`; first1/1, replay0/0 동일 receipt; claim/backlog/lease 불변. | **EFFECTIVE-GAP** — 첫 finalization과 claim 비교 있음. 동일 expired-worker baseline의 replay와 전체 관측 연결 없음. |
| **19** `pending_result_requires_original_authority_revisions` | H::`pending_result_rejects_service_grant_and_credential_rotation_without_rewriting_history`, `pending_result_rejects_source_rebind_without_writing_history`, `isolated_service_revision_and_credential_rotations_reject_the_captured_initial_authority`, `disabled_then_reenabled_service_rejects_its_old_captured_context`, `accepted_resume_rejects_isolated_service_and_credential_rotations`; H::동일 매트릭스명† | completed-pending initial/A/B 각각 service 변경·credential 변경·source 변경·revoke/regrant; fresh context도 original revision 대체 불가; lineage human revision만 적용; initial unrelated human change 허용; history/C0 보존. finalize와 lookup의 wrong-source/combined corruption 우선순위 포함. | **EFFECTIVE-GAP** — initial 일부 및 lookup denial 있음. A/B는 주로 **acceptance 후 continuation 진입 거절**이며 completed-pending strict finalization을 대체하지 못함. |
| **20** `revoke_after_stage_start_preserves_result_but_blocks_finalization` | H::`revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next`; HX::동일 매트릭스명† | **두 프로세스**에서 start commit 확인 → revoke/rebind commit → accepted stage completion; stage/session 보존, 다음 execute0, strict finalization 거절, T/R0/0. | **EFFECTIVE-GAP** — 현재 H는 thread 기반이며 stage 완료 후 restore에서 멈춤. 요구한 start-commit 순서의 결과 producer 증거가 아님. |
| **21** `revoke_before_stage_start_has_zero_executor_calls` | H::`revocation_between_hold_acceptance_and_continuation_blocks_pre_stage_work`; HX::동일 매트릭스명† | revoke commit → 실제 stage-start 시도; 신규 execute/stage/session/artifact/T/R 각각0; 이전 evidence 보존과 finalizer 거절. | **EFFECTIVE-GAP** — 현재는 continuation 진입 거절 및 일부 rows만 측정. stage 경계와 전체 vector가 빠짐. |
| **22** `reassigned_owner_cannot_store_or_finalize_old_result` | H::`direct_commitpass_reassignment_is_rejected_before_continuation`, `direct_commitpass_rejects_same_owner_epoch_fence_aba_before_materialization`; HX::동일 매트릭스명†; H::`completed_result_rejects_reclaimed_and_same_owner_aba`† | ordinary stale completion와 direct CommitPass를 분리; 둘 다 strict new finalization 거절; replacement claim 불변. completed A/B 각각 다른 owner·동일 owner ABA public reclaim, epoch2/fence A4·B6, backlog attempts+1, old evidence 보존. | **EFFECTIVE-GAP** — direct continuation 거절 두 건은 있음. 두 strict-finalizer 연결과 completed A/B public reclaim은 없음. ADD72 보류 유지. |
| **23** `initial_redrive_remains_unsupported` | H::HR 및 아래 charge 시험 5종 | redrive `5/0/1`, completed stage/artifact4/4, session3, budget5, failure1, failed/claimed, numeric result/T/R0. charge는 exact active running intent에서 order·stale work/run·revocation·limit·expiry·SQL rollback 검증. | **EFFECTIVE-GAP** — **redrive 본체는 PASS**. charge subcases의 원인 분리·동기화·rollback 증거가 미완. |
| **24** `continuation_revocation_distinguishes_restore_from_execution` | H::`revocation_after_completed_continuation_stage_preserves_it_and_blocks_the_next` 확장 | `replan_full-g1` execute 중 revocation commit; 누적 `5/1/1`, stages/artifacts/sessions/budget `5/5/4/5`; followup resuming, progress recovery_required; 다음 edge restore1/execute0; strict finalization 거절, T/R0/0. | **EFFECTIVE-GAP** — luna76에서 finalizer/recovery/T/R 단언 추가. 그러나 현재 pause는 stage 완료 후 restore이며 새 단언의 실행 증거도 없음. |
| **25** `interleaved_order_evidence_pairs_own_sequence_and_hash` | H::H0 | selected order 뒤 foreign append; selected sequence/hash가 같은 검증된 row; global head hash와 구별; 정상 finalize. | **PASS** |
| **44** `p2_preparation_walks_s1_without_result_seeding` | F를 실행하는 H0/HQ; H::동일 매트릭스명† | Waiting +3S1/+3receipts, Queued+0; preclaim intent/claim/result/tuple/promotion0; exact admission/current source; repeated/stale/invalid admission/source 및 conditional-row failure 전체 rollback. | **EFFECTIVE-GAP** — 성공 수치와 S1/backlog SQL-abort 있음. 반복·stale/invalid admission/source·조건부 갱신0행 사례 없음. |

**행 전체 기준: PASS 3, EFFECTIVE-GAP 20, UNMEASURED 1.**  
이는 생산 결함 21개라는 뜻이 아닙니다. 이미 통과한 부분 증거를 인정하면서, **행 전체를 닫기 위해 빠진 단언까지 포함한 집계**입니다.

# 잔여 갭 전수 목록

아래는 위 24행 중 미충족 **21행 전체**입니다. 이 목록 밖의 항목을 마지막 구현 배치에 구두로 추가하지 않아야 합니다.

| 갭 ID | 남은 작업 |
|---|---|
| **G02** | H0에 accept/result/run 각1, finalizer·replay별 claim 불변, 저장 receipt bytes 불변을 연결. |
| **G03** | colon order의 실제 persisted journal/intent/key/tuple을 한 번에 비교. 숫자 결과 반환만으로 대체하지 않음. |
| **G04** | lifecycle alias 추가. valid winner `Conflict`와 corrupt winner `CorruptEvidence`를 activation/lifecycle 각각 입증. |
| **G05** | 정확한 `i64::MAX+1` 경계, persisted negative coordinate, verified replay attempt 불일치 추가. 요청 attempt 변경 사례와 구분. |
| **G08** | authority/accepted-hold/artifact nested malformed 사례, 저장 payload/receipt의 version/kind/noncanonical 거절 및 digest/bytes 대응 추가. |
| **G09** | 실제 emit/promotion wire를 genuine stored receipt에 적용하는 독립 negative 추가. 결과 재사용·fallback 승인/소비 없음 확인. |
| **G10** | chain/ancestry mutation을 정상 대조가 있는 독립 사례로 정리. inspected commit, foreign fix artifact, host/journal, active key/epoch, progress phase/owner/fence, snapshot/receipt 및 좌표 불일치의 누락 사례 추가. |
| **G11** | A의 정확한 path/session/script input, generation index, progress revision/epoch/fence, 두 host의 accept/result/run/work census와 원본·followup 보존 단언 추가. origin/reservation mismatch 거절 포함. |
| **G12** | B/direct CommitPass 경계의 S1/receipt 증가와 나머지 무변경을 측정. unused successor의 전체 관련 테이블 부재·allocator 추가 증가0, CommitFail/Abort ineligible 추가. |
| **G13** | 각 path negative에 정상 finalization 대조와 복원 확인, 전체 no-effect 단언 추가. |
| **G14** | 유효 authority를 유지한 genuine materialized-resuming 상태에서 `Ineligible`를 직접 입증. revoked caller의 `Unauthorized`로 대체 금지. |
| **G16** | activation authority insert 실패의 전체 rollback 추가. Resume copy의 corrected numeric/text query를 실행 검증하고, run/accept/work/intent/active/authority/progress의 원자성을 연결. |
| **G17** | faithful historical host 및 descendant fixture, modern missing-initial/missing-copy 대조군 마련. 현재 synthetic census 변조는 해당 fixture를 대신하지 못함. **Gate3 fixture 의존성 결정 필요.** |
| **G18** | HW에서 최초 성공 뒤 같은 expired-worker 조건으로 replay; 동일 저장 receipt, T/R 증가0, claim/lease 및 호출 vector 불변 추가. |
| **G19** | completed-pending initial/A/B에 대한 독립 authority 변경 표를 완성. 현재 A-service/B-credential continuation 거절 두 사례로 세 시나리오의 finalizer 의무를 닫지 않음. lineage human account/scope/ResolveReviewHold 변경과 initial unrelated-human 허용도 연결. |
| **G20** | 결과 producer의 두 프로세스 start-commit→revocation/rebind→completion 증거와 strict finalizer refusal 추가. 기존 service-review process 시험을 활용하더라도 **동일 baseline에 결과 단언을 연결**해야 함. |
| **G21** | revocation commit→실제 stage-start 시도의 경계와 신규 stage/session/artifact/T/R/execute 전체0, finalizer refusal 추가. |
| **G22** | ordinary/direct stale-owner 사례의 strict finalizer 거절·replacement claim 불변 추가. completed A/B × 다른 owner/동일 owner ABA public reclaim 네 사례는 보류 해제 후 구현. |
| **G23** | 아래 charge 상세 목록 전부. redrive 본체 단언은 유지. |
| **G24** | revocation을 `replan_full-g1` 실행 중에 commit하도록 pause 이동. followup run을 대상으로 finalizer 호출; `5/1/1`, `5/5/4/5`, resuming/recovery_required/T/R0/0를 같은 baseline에서 실행 검증. |
| **G44** | duplicate preparation, stale/invalid admission, stale source, conditional update 0행 rollback과 preclaim promotion0 추가. |

## G23 — ADD74 수정본에도 남아 있는 정확한 문제

현재 charge 관련 정확한 시험명은 다음과 같습니다.

1. `redrive_charge_rejects_wrong_order_and_stale_run_without_durable_changes`
2. `charge_budget_rejects_revoked_authority_for_the_active_running_run`
3. `charge_budget_rejects_exhausted_budget_without_charging`
4. `charge_budget_sql_abort_rolls_back_the_budget_write`
5. `charge_budget_samples_clock_after_waiting_for_begin_immediate`

현재 소스에서 확인한 잔여 문제:

- **wrong-order/stale-run 시험:** 완료된 `genuine_host_done` baseline을 사용합니다. wrong-order는 첫 검사에 도달하지만, stale-run 사례는 “정상 intent라면 charge 가능했음”을 입증하지 못합니다. active-running 정상 charge 대조군, stale run 및 stale owner/fence를 각각 분리해야 합니다.
- **revoked/exhausted:** luna76에서 `charge_budget` 직접 호출로 수정됐습니다. 현재 수정본 실행은 미확인입니다. 정확한 실패 원인과 budget/run/stage/artifact/session 불변을 동결해야 합니다.
- **SQL failure:** 현재 `BEFORE UPDATE` trigger와 generic `is_err()`입니다. 실제 주입 오류가 발생했는지 단언하고, 변경 후 abort 등으로 rollback을 입증할 수 있는 지점을 사용해야 합니다. 다른 precondition 실패를 SQL rollback 성공으로 세면 안 됩니다.
- **expiry:** 정상 charge 대조군과 정확한 running intent는 추가됐지만, 두 문제가 남습니다.
  - `started_tx.send(())`는 여전히 `charge_budget` 호출 **앞**입니다. 이를 수신했다고 BEGIN 대기에 들어간 것은 아닙니다.
  - clock을 `+3시간`으로 이동합니다. fixture의 work lease는 5분이며, [`ensure_new_work`는 service credential 검사 전에 live work claim을 검사](/home/cosmo/foundry/crates/foundry-ledger/src/product/review.rs:2281)합니다. 따라서 **credential expiry가 아니라 worker expiry로 거절될 수 있습니다.**

expiry의 최종 필수 순서는 다음으로 고정해야 합니다.

`정상 running/claimed → credential만 만료될 수 있도록 수명 분리 → write lock 보유 → charge의 실제 BEGIN 시도 확인 → clock 이동 → lock 해제 → credential expiry 거절`

동일 설정에서 clock을 이동하지 않으면 정확히 한 번 charge되어야 합니다. 이 관측 지점을 시험 코드만으로 만들 수 없다면, **관측 전용 hook의 허용 여부를 결정할 문제**이지 `sleep`이나 호출 전 신호로 PASS 처리할 문제가 아닙니다.

근거: [현재 charge 시험](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs:1374), [expiry 시험](/home/cosmo/foundry/crates/foundry-line/tests/b1_product_results.rs:1717).

## round71·75 REJECT 분류

분류는 다음 의미로 사용합니다.

- **(a)** 동결 계약 또는 승인된 addendum의 구현·증거 보존 의무 위반.
- **(b)** 기존 의무를 입증한다고 제시된 시험의 무효성·불완전성 입증.
- **(c)** 계약·승인 addendum에 근거가 없는 신규 기준.

| 로그 / 지적 | 분류 | 판단 |
|---|---|---|
| round71-1: `charge_budget`가 BEGIN 이전 시간을 사용 | **(a)** | 현재 authority/expiry를 serialized transaction에서 판정해야 한다는 기존 의무의 구현 결함. |
| round71-2: accepted-edge 검증 전 legacy `Ineligible` | **(a)** | C§2는 완전하고 명확한 ancestry를 요구하며 broken ancestry는 corruption으로 규정. |
| round71-3: redrive dispatch1 단언 삭제 | **(a)+(b)** | ADD68의 단언 보존 규칙 위반이며, terminal이 없는 경우 shared helper가 대신 검사하지 못함. |
| round71-4: TEXT journal run을 integer8로 조회 | **(b)** | orphan run이 있어도 0이 되는 vacuous assertion. |
| round71-5: accounting·authority·barrier·reclaim 등 미측정 | **(a), 증거 측면에서는 (b)** | ADD67/68 및 동결 B/C에 이미 존재. 이 항목은 새로운 실패 시험 발견이라기보다 필수 coverage 누락 지적. |
| round75-1: expiry 시험이 stale run에서 먼저 실패; 호출 전 신호 | **(b)** | expiry 및 lock ordering을 검증하지 못함. |
| round75-2: revoked/exhausted/SQL 시험이 다른 메서드 실행 | **(b)** | continuation/start-stage 검증은 `charge_budget` 자체의 검증이 아님. |
| round75-3: numeric host-accept run에 journal string 비교 | **(b)** | round71의 text 비교 수정이 다른 테이블의 숫자 비교를 깨뜨림. |
| round75-4: continuation counts만 있고 finalizer/recovery/T/R 없음 | **(b)** | [동결 973행](/home/cosmo/foundry/.hermes/plans/b1-product-result-package-spec.md:973)에 이미 명시된 의무. |

최종 판정 원문: [round71](/home/cosmo/.hermes/cache/scratch/b1-sol-round71.log:12784), [round75](/home/cosmo/.hermes/cache/scratch/b1-sol-round75.log:8416).

**분류 (c): 확인된 항목 없음.**  
다만 ADD72 이후 completed public reclaim을 “이미 해제된 보류”처럼 다시 강제하는 것은 허용되지 않습니다. 의무는 남아 있지만 해당 라운드의 구현 범위에서는 명시적으로 보류됐습니다.

# 배치 계획

## 배치 전: 루브릭·작업 트리 고정

추가 구현 라운드를 시작하기 전에 다음을 한 번 확정합니다.

- 이 보고서의 24행 및 G 목록에 버전 부여.
- 각 subcase에 `행번호.단언번호` 부여.
- exact 시험명·예상 오류·barrier 순서·관측값·로그 위치 지정.
- luna76 완료본의 허용 파일 내용 hash 기록.
- 아래 사용자 결정 항목 처리.
- sol에게도 같은 루브릭을 제공. 새로운 자유 탐색 결과를 종결 기준으로 추가할 권한은 부여하지 않음.

이 단계에서 판단되지 않은 항목을 구현 마지막에 다시 발견하는 구조를 없애야 합니다.

## luna 배치 A — ADD74를 유효한 시험으로 마감

**작업:** G23, G24, G16의 copy-query 수정 검증. 기존 redrive·lookup·rotation·reassignment 단언 보존.

**허용 파일**

- `crates/foundry-line/tests/b1_product_results.rs`
- `crates/foundry-line/tests/support/product_result_fixture.rs`

**금지**

- 모든 production `src` 변경.
- schema/migration/API/manifest/lockfile 변경.
- SQL로 positive running/authority/result를 만들어내기.
- `is_err()`만으로 특정 오류 경로를 입증하기.
- 기존 continuation 시험을 charge 시험으로 교체하면서 원래 continuation 단언을 소실시키기.

**완료 조건:** 정상 control과 정확한 실패 원인, 실제 pause/barrier 도달, durable state 단언이 모두 연결됨. 이 배치는 네 지적의 “문자상 수정”이 아니라 **같은 지적이 재발하지 않는 증거**를 만들어야 합니다.

## luna 배치 B — 나머지 Gate1 증거를 한 번에 완성

**작업:** G02–G22, G44의 잔여 부분. 배치 A가 충분히 작고 결정이 완료돼 있다면 하나의 구현 배치로 합쳐도 됩니다.

**허용 파일 제안**

- H, HX, F, L.
- `.hermes/plans/b1-w3-acceptance-evidence.md`
- `.hermes/plans/b1-w3-recovery-acceptance-matrix.md`의 증거 연결 부분.
- private wire 시험 확장이 필요하면 `result.rs`의 **기존 `#[cfg(test)] result_wire_tests` 내부만** 별도 허용. runtime 코드는 동결.

**결정 이후에만 포함**

- G17을 위한 isolated faithful historical fixture.
- G22 completed A/B public reclaim.
- 꼭 필요한 charge lock 관측 hook.

**금지**

- production validator·authority·wire 의미 변경.
- schema/version/기존-v3 repair.
- host algorithm, packs, deps, manifests 변경.
- Gate2의 root/HTTP/socket 개선이나 Gate3 전체 migration/I/O 작업을 함께 끌어오기.
- assertion 삭제·ignore·no-op 또는 fixture 오염으로 통과시키기.

새 시험이 실제 생산 결함을 드러내면 **행/단언 ID와 실패 predicate를 제시하고 그 항목만 재판정**합니다. 이를 숨긴 채 “2배치 목표”를 달성해서는 안 됩니다.

## 부모 검증 1회

두 구현 배치 사이에는 필요한 좁은 시험만 수행하고, 최종 내용 hash가 고정된 뒤 부모가 전체 검증을 한 번 수행합니다.

| 레인 | 범위 |
|---|---|
| H/HX | enabled genuine result 및 process tests |
| ledger | result integration, 필요한 wire lib tests |
| HTTP enabled | auth + artifact targets |
| restore default | 기존 restore 책임 |
| restore enabled | 같은 책임의 enabled 조합 |
| CLI enabled | approval + scope |
| core default | exec/line/ledger/serve lib/tests |
| watch | 기존 두 B1 targets |
| CLI default | 기존 5개 B1 targets |

공통 조건은 기존대로 locked/offline, jobs1, incremental0, serial, free≥2GiB입니다. HX의 ledger executable은 **외부 Cargo JSON prebuild에서 정확히 추출**합니다.

부모의 산출물은 “9개 exit0”가 아니라 다음 둘입니다.

1. **24행 × 단언 ID → 실제 시험·로그·관측값 → PASS/GAP**
2. 기존 assertion의 old→new 보존 map

fixture 변경이 모든 consumer에 미치는 영향도 이 고정본에서 검증합니다. 이후 내용이 바뀌면 관련 레인의 증거만 무효화합니다.

## sol 리뷰 1회

sol은 부모가 완료한 동일 hash의 소스·루브릭·로그를 대조합니다.

- PASS: 모든 필수 단언 충족 및 허용 범위 준수.
- REJECT: 정확한 기존 단언 ID의 반례가 있을 때.
- DECISION-REQUIRED: 보류·계약 충돌·루브릭 자체 누락일 때.
- 참고 개선사항: Gate1 판정에 영향 없이 별도 기록.

# 발산 방지 장치

## ① 루브릭 외 신규 지적 금지 조항

다음 문구를 그대로 채택할 수 있습니다.

> **Gate1 판정 기준 동결**
>
> Gate1의 종결 분모는 본 루브릭의 24행과 명시된 단언 ID로 고정한다. 리뷰어는 각 REJECT에 기존 행·단언 ID, 승인된 근거, 현재 소스·시험의 반례를 제시해야 한다.
>
> 목록 밖의 새로운 시나리오, 조합, 관측값, 구현 방식 또는 품질 개선을 Gate1의 추가 필수 기준으로 만들 수 없다. 해당 제안은 비차단 후속 항목으로 기록한다.
>
> 기존 PASS를 무효화하려면 그 단언을 시험이 입증하지 못한다는 구체적 반례를 제시해야 한다. 이때 수정 범위는 해당 단언의 유효성 복구로 제한한다.
>
> 동결 계약에 실제 의무가 있으나 루브릭에서 누락된 사실을 발견하면, 이를 “루브릭 결함”으로 보고한다. 리뷰어가 일방적으로 종결 분모를 확대하지 않는다. 사용자/부모의 명시적 disposition과 루브릭 버전 변경 전에는 추가 구현을 지시하지 않는다. 누락된 의무를 충족했다고 허위로 인정하지도 않는다.
>
> 이미 보류된 항목은 보류 해제 전까지 자동 재요구하지 않는다. 보류는 PASS나 계약 삭제가 아니다.

## ② 시험–생산 인터페이스 고정 목록

| 경계 | 고정할 내용 | 방지하는 재발 |
|---|---|---|
| finalizer/lookup | 기존 public signature, `ProductResultExpectation`, 오류 우선순위 | 시험 편의를 위한 API·정책 변경 |
| fixture 반환값 | `terminal: Option<HostResult>` 유지. 성공 wrapper는 real terminal 필수, 실패 fixture는 `None + dispatch_error`; 빈 결과 fallback 금지 | HTTP/CLI의 순차 compile 파손 |
| fixture bootstrap | 기존 wrapper와 CLI-owned bootstrap callback, 원본 DB/config/root/identity 보존 | double bootstrap, 복사 DB·root로 맞추기 |
| 스키마 소유 | fresh v3가 tuple storage를 소유. 소비자 시험에서 `CREATE artifact_tuples` 재실행 금지 | duplicate table 실패 |
| run ID 타입 | journal run은 TEXT, host-accept run은 INTEGER. 별도 query 변수·helper 사용 | integer8↔journal string 교차 오용 |
| authority | intake/claim에서는0, activation capture, Resume exact copy; historical backfill 금지 | service-less fixture를 validator 완화로 복구 |
| legacy fixture | faithful-v2 fixture와 stripped-v3 negative fixture 구분 | migration chain의 가짜 positive |
| clock/pause | lifecycle 상태와 실제 도달 지점을 반환. 호출 전 신호를 operation 도달로 취급 금지 | expiry·barrier 시험의 vacuous success |
| features | P2 default-off, 기존 enabled/default 시험 책임 유지 | feature gating으로 시험 소실 |
| HX handoff | Cargo JSON executable, 기존 child transport·barrier token | 오래된 executable 또는 guessed path 실행 |

fixture 변경 전에는 F를 포함하는 H/HX/HTTP/CLI caller를 한 번에 목록화하고, compile compatibility를 먼저 확인해야 합니다. **현재 dirty tree의 전체 diff를 신규 변경으로 취급하지 말고, 배치 시작 hash 대비 변경만 검사**해야 합니다.

## ③ 새 의무와 기존 의무 무효 입증의 판정 규칙

| 상황 | 판정 | Gate1 처리 |
|---|---|---|
| 기존 단언은 credential expiry인데 실제로 stale run에서 먼저 거절 | 기존 의무 무효 입증 | 해당 단언만 재개방 |
| rollback 시험이 잘못된 SQL 타입으로 항상0 반환 | 기존 의무 무효 입증 | query와 정상 대조 복구 |
| frozen 계약에 있는 materialized-resuming 사례가 시험에 없음 | 기존 의무 미측정 | 현재 동결 목록에 넣어 한 번에 완료 |
| “더 강건하니 모든 오류를 모든 consumer와 교차시험하자” | 명시 근거 없으면 신규 기준 | 비차단 후속 제안 |
| counts `5/1/1`의 세 번째1을 finalization 호출 수로 해석 | 기준 오독 | 수정. 세 번째 값은 **dispatch_count** |
| recovery retry 때 restore를 한 번 더 요구 | frozen 누적1과 다르면 신규 기준/오독 | 요구 금지 |
| 기존 scope 밖 production 수정이 없으면 시험 관측 불가능 | 구현 범위 결정 | 정확한 hook/predicate만 사용자 판단 |
| 이미 보류한 completed public reclaim을 다시 즉시 요구 | scope 위반 | 보류 해제 결정 전 차단 지적으로 반복 금지 |

특히 ADD70~72의 “5 executes, 1 restore, 1 finalize” 표현은 정본의 `execute/restore/dispatch=5/1/1`과 맞지 않습니다. **루브릭에서는 dispatch로 정정하고, finalizer 호출은 별도 거절 단언으로 고정해야 합니다.**

# 사용자 결정 필요 항목

## 1. completed A/B public reclaim 보류 해제

ADD72는 public API 문제를 이유로 보류했지만, 현재 [`claim_product_work_for_service`](/home/cosmo/foundry/crates/foundry-ledger/src/product.rs:4563)는 명시적으로 다음을 받습니다.

```rust
(context, worker_id: &str, now, lease)
```

또한 만료된 `claimed` backlog를 선택해 epoch/fence와 attempts를 증가시키는 경로가 존재합니다.

따라서 **“worker-owner 인자가 없어 새 API가 필요하다”는 설명은 현재 소스와 일치하지 않습니다.** 기존 API로 완료된 A/B의 reclaim을 구성할 가능성이 높습니다. 이는 정적 확인이며 실행 성공을 주장하지 않습니다.

**권고:** ADD72의 보류를 명시적으로 해제하고, 기존 API만 사용하는 네 벡터를 배치 B에 포함합니다. 실패하면 정확한 기존 predicate를 보고하며 새 API를 임의로 만들지 않습니다.

## 2. legacy-host 행의 Gate3 fixture 의존성

17행은 실제로 미측정입니다. 현 synthetic census 변조를 historical positive로 인정할 수 없습니다.

**권고:** isolated faithful historical fixture 작성에 한해 배치 B 범위를 확장하고, Gate3 전체 migration 판정은 그대로 남깁니다. 이를 허용하지 않으면 **Gate1 전체 PASS가 아니라 “mechanical bundle 완료, 17행 미종결”**로 보고해야 합니다.

## 3. expiry 관측 hook의 제한적 허용 여부

시험 전용 기존 도구로 실제 BEGIN 시도/대기와 clock sampling 순서를 결정적으로 관측할 수 없다면, 현 ADD74의 “모든 src 변경 금지”와 정확한 증명 사이에 범위 충돌이 생깁니다.

**권고:** 우선 test-support만으로 구현 가능성을 확인하고, 불가능한 경우에만 **관측 전용·시험 전용 hook**의 정확한 위치와 내용을 승인 대상으로 제시합니다. production 의미·public API·schema 변경은 허용하지 않습니다.

## 4. 이 루브릭을 마지막 종결 기준으로 채택

이 보고서는 **동결 제안**이며 스스로 계약을 변경하지 않습니다.

채택할 내용은 다음 네 가지입니다.

- 24행과 G 목록을 마지막 수정 배치의 고정 목록으로 사용.
- 현재 PASS 부분 증거는 보존하되, 수정된 fixture의 최종 고정본에서 재검증.
- G17/G22 및 관측 hook 결정을 배치 전에 처리.
- 최종 sol 리뷰를 기존 단언의 충족·무효성 판정으로 제한.

이 결정 없이 ADD74 네 건만 고치고 다시 자유형 sol 리뷰를 수행하면, **이미 문서에 남아 있는 다른 Gate1 의무가 다음 REJECT로 등장하는 구조가 그대로 유지됩니다.**
