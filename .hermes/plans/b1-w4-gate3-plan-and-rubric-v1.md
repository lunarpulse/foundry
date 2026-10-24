# B1 W4 Gate3 — 기존·닫힌 PR 복구 (계획·루브릭 v1, FROZEN 2026-10-24)

승인 근거: 사용자 "같은 방식으로 착수"(2026-10-24). 상설 자동 승인(1563206386211496081) 적용 — 동결 후 구현 착수. 기준 HEAD `2c6f4af`(W4 Gate2).

## 1. 실측 조사 (2026-10-24, HEAD 2c6f4af)

- 전환 계획 §8.2 W4 정의 잔여: "기존·닫힌 PR 재시도 복구는 우선 local mock으로 검증. 이미 수락된 외부 효과와 새 실행 권한을 구분." Gate1(파이프라인)·Gate2(CLI 경로)는 닫힌 PR 의미론 미포함.
- `ProductPublishReconcileOutcome`(foundry-line/product.rs:12) = Confirmed{pr_url}/NotFound/Unknown. **닫힌 PR(merged 아님) 표현 없음** — reconcile이 거부된 PR을 Confirmed로 오보고하거나 NotFound로 자동 재실행 유도할 여지.
- 계약 원칙(§8.2): 외부에서 이미 수락된 효과(merged 포함 URL)와 새 실행 권한은 구분 — 닫힌 미병합 PR은 수락된 효과가 아니므로 **자동 재시도 금지, 새 승인 필요**.
- `PublishOutcome`(domain ports:130)·`Publisher` 트레잇·저널 이벤트(PublishConfirmed/PublishUnknown)는 불변 유지(공유 포트 계약 동결).
- "대상 repo 확인": publish 경로 전반에 repo 식별 원천이 없음(어댑터가 구성 시점에 repo에 바인딩). mock 단계에서 검증 가능한 형태가 없으므로 **W6 실제 어댑터 착지 시점 귀속**으로 계약 문서에 기록 — 본 Gate에서 신규 스키마/설정 도입 안 함.

## 2. Gate3 구현 범위

원칙: **닫힌 미병합 PR은 "수락된 외부 효과"가 아니다 — durable 상태는 pending 보존, 자동 재집행 권한 없음, 새 승인만 재시도 가능. merged 닫힘·기존 PR은 기존 Confirmed 멱등 경로로 회복.**

1. **`ProductPublishReconcileOutcome`에 `Rejected { pr_url }` variant 추가**(foundry-line, additive): 외부 PR이 닫혔고 병합되지 않은 경우. 공유 `Publisher` 포트·`PublishOutcome`·저널 이벤트 무변경.
2. **`publish_product_approved` reconcile 경로 처리**: Rejected 반환 시 ①settle하지 않음 ②durable 행 pending 유지 ③새 publish 호출 없음 ④호출자에게 Rejected{pr_url} 명시 전파(오류 아닌 비즈니스 결과). 동일 operation 재요청은 같은 결과로 멱동 재생(레지스트리·durable 상태 불변).
3. **CLI `product publish` 표면**: Rejected 결과를 명시 메시지로 출력("external PR closed without merge: <url>; stored pending; a new approval is required to retry"), exit 1(조치 필요), durable 상태 불변. 테스트 퍼블리셔 레지스트리에 닫힘 상태 표현 추가(closed+merged / closed+unmerged / pre-existing).
4. **계약 문서 갱신**: 기존·닫힌 PR 복구 의미론(merged→Confirmed 멱동, unmerged→Rejected+pending 보존+재승인 필요) 및 대상 repo 확인의 W6 귀속 명시.
5. bounds: foundry-line(product.rs)·foundry(CLI)·테스트·계약 문서만. foundry-ledger 스키마 무변경(outcomes 테이블 pending/confirmed 유지 — Rejected는 상태가 아니라 reconcile 보고). foundry-domain 포트 무변경. foundry-serve 무변경. 실제 외부 호출·자격증명 금지. 기존 인자·종료코드 불변. Cargo --locked --offline, jobs=1, incremental=0, disk guard 2GiB, B1_LEDGER_TEST_EXE 프리빌드. deletion-to-green·assertion 약화 금지.

## 3. Gate3 루브릭 v1 (행별 단언 — 동결)

- **W1 existing_pr_reconciles_confirmed (L)** — 레지스트리에 operation의 PR이 이미 존재(settle 유실/선존재 시뮬레이션), 스토어 pending → reconcile Confirmed(동일 URL) → settle 성공 → confirmed 행 1개(receipt_json byte 동일), 레지스트리 PR 수 불변.
- **W2 closed_merged_reconciles_confirmed (L)** — 레지스트리 PR closed+merged → reconcile Confirmed(URL) → settle 성공, 새 PR 0.
- **W3 closed_unmerged_rejected_keeps_pending (L)** — 레지스트리 PR closed+unmerged → Rejected{pr_url} 전파, settle 0회, durable 행 pending 유지(state 불변), 새 publish 호출 0회; 동일 재요청 동일 결과·상태·레지스트리 불변(멱동).
- **W4 cli_reports_rejected_without_side_effects (L)** — CLI publish가 Rejected를 명시 메시지+exit 1로 보고, durable 상태 불변, 퍼블리셔 신규 호출 0.
- **W5 regression_lanes_preserved (P)** — 기존 레인 전부 exit 0, 기존 count 감소 0, 기존 테스트 무수정 통과(u-cells 8셀 포함).

완료 기준: W1~W5 전부 PASS. 셀 판정은 이 표 행별 단언 인용으로만.

## 4. 순서

ADD96(edit-spec) 작성 → luna 위임 구현 → 부모 독립 재판정(레인) → 증거 문서 → sol 리뷰 → 인수 → 단일 커밋+push → W4 Gate3 완료 보고 정지. Gate3 인수 시 §8.2 local-mock 검증 의무는 전부 소화(W4 종결, 실제 어댑터=W6).
