# B1 W5 Gate2 — export MUST 명시성·검증 결합 기록 + 제품 서버 P1/P2 인수 (계획·루브릭 v1, FROZEN 2026-10-24)

승인 근거: 사용자 "승인"(W5 Gate2 착수, 2026-10-24, Discord `1563597023876227115`). 상설 자동 승인 `1563206386211496081`. 기준 HEAD `20530c7`(W5 Gate1). 원천: b1-closure §W5 ②③④⑤ + :131·:142.

## 1. 실측 조사 (2026-10-24, HEAD 20530c7)

- **export 현황**: `ScopedReadQuery{limit}` 단일 페이지, limit 1..=100(기본 100), **cursor/offset 없음**(export_http.rs:18, :52, :125). DTO에 `coverage`·`has_more`·`source_coverage_complete` 존재(privacy.rs:383~437) — 그러나 "이 선택이 전체가 아님"의 명시 문구, 선택 주문 이력 무제한 바이트 명시, 전체 원장 검증 시간·타 사용자 응답 실패 결합 기록은 부재(b1-closure §W5 ② 갭).
- **cursor·응답 예산·검증 비용**(§W5 ④): "정책 결정 제안(자원 수치는 실측 근거)" — 구현이 아니라 **실측 근거 문서화가 의무**. :142 "audit/export 유예는 inventory·비용 근거를 갖춘 별도 명시 결정"과 정합.
- **P1**(W0 :87): 실제 intake consumer→durable inbox/admission/ack→`waiting_unsupported`. 임의 issue 본문을 task로 실행하지 않음.
- **P2**(W0 :88): 명시적 시험 입력 어댑터→기존 genuine host→인간 hold/승인→실제 제품 artifact/publish consumer. production router/transaction/consumer 사용, 시험 어댑터의 인증 우회 금지, 모의 executor/publisher는 모의로 표기.
- **:131**: P1/P2 각각 검증 + **실제 issue→PR 통합이라 보고하지 않는다**(정직 경계).
- 기존 관련 테스트: b1_service_review.rs·b1_product_restore.rs·b1_product_publish.rs·b1_product_results.rs·b1_product_artifacts.rs 등에 P1/P2 요소 분산 — 인수 재검증 셀로 각각 측정.

## 2. Gate2 구현 범위

원칙: **export 응답은 정직하게 — 잘렸으면 잘렸다고, 선택이면 선택이라고 명시. 선택된 주문의 이력은 바이트 제한 없이 완전. 검증 비용·결합은 측정해 기록. P1/P2는 각각 재검증하고 실제 통합이 아님을 명시. cursor는 구현하지 않고 실측 근거 정책 제안만.**

1. **export MUST 명시**(foundry-serve additive): scoped export/audit 응답에 ① 절단 명시(has_more 참일 때 "truncated selection, not the full set" 계열 명시 라벨 — 기존 coverage 문구와 구분되는 부울+문구), ② 선택 주문 이력 완전성 명시(선택된 주문의 전체 이력이 바이트 상한 없이 포함됨을 나타내는 필드), ③ 전체 원장 검증 시간(export/audit 생성 시 검증 소요의 측정치 기록)과 타 사용자 응답 실패 결합에 대한 정직 기록(단일 사용자 응답이 원장 전체 검증을 대체하지 못하는 경계 명시). 기존 필드·값 불변(additive만).
2. **선택 행 완전성 측정**: 장문 이력을 가진 선택 주문 export가 이력을 절단하지 않음을 측정하는 셀(선택 행만 검증하는 완화 금지 — §W5 ②).
3. **P1 인수 재검증 셀**: 실제 intake consumer 경로(durable inbox→admission→ack)→`waiting_unsupported` 전이 측정 + 임의 issue 본문 task 실행 불가 단언.
4. **P2 인수 재검증 셀**: 시험 어댑터→genuine host→인간 hold/승인→artifact/publish consumer 전체 경로(production router/tx/consumer) 측정 + 시험 어댑터 인증 미우회 + 모의 executor/publisher "모의" 표기 단언.
5. **정직 경계·정책 제안 문서화**(계약문서+증거): P1/P2는 실제 issue→PR 통합이 아님 명시. cursor·응답 예산·검증 비용 정책 제안 — 실측 수치(단일 페이지 최대 100, cursor 부재, 검증 소요 측정치) 근거로 별도 명시 결정 대상임을 기록.
6. bounds: foundry-serve(export_http.rs·privacy.rs·audit_report.rs additive)·테스트·계약 문서만. foundry-ledger 프로덕션 무변경(기존 공개 API만). 스키마·인증·권한 변동 없음. 실제 외부 호출·자격증명 금지. cursor 구현 금지(제안만). Cargo --locked --offline, jobs=1, incremental=0, 2GiB 가드, B1_LEDGER_TEST_EXE 프리빌드. deletion-to-green·assertion 약화 금지.

## 3. Gate2 루브릭 v1 (행별 단언 — 동결)

- **Y1 export_truncation_explicit (L)** — 절단된 선택(has_more 참)에 대해 응답이 명시적으로 "전체 아님"을 표시(부울+문구). has_more 거짓·전체 선택과 구분. 기존 coverage 필드는 불변.
- **Y2 selected_history_complete (L)** — 장문 이력 선택 주문의 export가 전체 이력을 바이트 상한 없이 포함(절단 0). 응답에 선택 이력 완전성 명시 필드 존재.
- **Y3 verification_metadata_recorded (L)** — 응답/증거에 검증 시간 측정치 기록 + 타 사용자 응답 실패 결합 경계 정직 명시(단일 사용자 응답이 전체 검증을 대체하지 않음).
- **Y4 p1_intake_consumer_verified (L)** — 실제 경로 측정: durable inbox→admission→ack→`waiting_unsupported`. 임의 issue 본문 task 실행 0건 단언.
- **Y5 p2_pipeline_verified_honest (L)** — 시험 어댑터→genuine host→인간 hold/승인→artifact/publish consumer 전 경로(production router/tx/consumer). 시험 어댑터 인증 우회 0. 모의 executor/publisher 모의 표기 단언. 계약문서·증거에 "실제 issue→PR 통합 아님" 명시.
- **Y6 regression_lanes_preserved (P)** — 기존 레인 전부 exit 0, 기존 count 감소 0, 기존 테스트 무수정 통과.

완료 기준: Y1~Y6 전부 PASS.

## 4. 순서

ADD99 작성 → luna 위임 → 부모 재판정(round11, P1/P2 레인 포함) → 증거 문서 → sol 리뷰 → 인수 → 단일 커밋+push → W5 Gate2 완료 보고 정지(=W5 종결 여부는 §W5 잔여 없음 시 W5 종결 보고).
