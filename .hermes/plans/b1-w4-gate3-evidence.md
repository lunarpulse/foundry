# B1 W4 Gate3 — 증거 문서 (기존·닫힌 PR 복구, 2026-10-24)

기준: HEAD `2c6f4af`(W4 Gate2). 델타: foundry-line/src/product.rs(+56 일부), foundry-line/tests/b1_product_publish.rs(+253), foundry/src/main.rs(+67), foundry/tests/b1_publish_cli.rs(+65), docs/design/b1-product-contract.md(±2). 5파일 +402/−43. foundry-ledger·foundry-domain·foundry-serve 무변경.

## 구현 요약 (luna114, proc_7f8385a5fe16)

- `ProductPublishReconcileOutcome`에 `Rejected { pr_url }` variant 추가(additive). 공유 `Publisher` 포트·`PublishOutcome`·저널 이벤트 무변경.
- `publish_product_approved` reconcile 경로: Rejected → settle 0회, durable 행 pending 유지, 새 publish 호출 0회, 호출자에게 명시 전파. 재요청 멱동(상태·레지스트리 불변).
- 테스트 레지스트리(L local + CLI 파일 레지스트리)에 닫힘 상태 표현 추가: closed+merged → Confirmed(동일 URL), closed+unmerged → Rejected, 선존재 PR → Confirmed.
- CLI `product publish`: Rejected를 "external PR closed without merge: <url>; stored pending; a new approval is required to retry" 메시지 + exit 1로 보고, 부수효과 0.
- 계약 문서: 기존/닫힌 PR 복구 의미론 + 대상 repo 확인의 W6 어댑터 귀속 명시.

## 루브릭 판정 (W1~W5, FROZEN .hermes/plans/b1-w4-gate3-plan-and-rubric-v1.md)

- **W1 PASS** — existing_pr_reconciles_confirmed: 선존재 PR + 스토어 pending → Confirmed(동일 URL) → settle → confirmed 행 1개(receipt_json byte 동일), 레지스트리 PR 수 불변.
- **W2 PASS** — closed_merged_reconciles_confirmed: closed+merged → Confirmed(URL) → settle 성공, 새 PR 0.
- **W3 PASS** — closed_unmerged_rejected_keeps_pending: Rejected{pr_url} 전파, settle 0, durable pending 유지, 새 호출 0; 재요청 동일 결과·상태·레지스트리(멱동).
- **W4 PASS** — cli_reports_rejected_without_side_effects: 명시 메시지+exit 1, durable 불변, 퍼블리셔 신규 호출 0.
- **W5 PASS** — round8 11레인 ALL_GREEN(부모, proc_26ec86668742, 2026-10-24):
  - **u-cells 11/0**(354.10s; 기존 8 + W1~W3 3셀) · **publish-enabled 3/0**(82.25s; 기존 2 + W4 1셀) · http-enabled 6/0 · restore-default 10/0 · restore-enabled 10/0(82.46s) · cli-enabled 7/0 · h-hx 44/0(299.03s/916.91s) · ledger 6/0(0.66s) · **core 409/0**(foundry-line 델타가 `#[test]` 추가 없음 — 라이브러리 코드 전용) · watch 218/0 · cli-default 20/0
  - 기존 카운트 감소 0, 기존 테스트 무수정 통과. luna 샌드박스 watch loopback 거부는 부모 환경에서 218/0 green(환경 제한, 코드 결함 아님).

## 프로세스 이력

루브릭 W1~W5 동결 → ADD96 → luna114(proc_7f8385a5fe16): 셀 green(라인 11·CLI 3·default 1), watch/core 잔여를 부모 재판정로 이관 보고 → 부모 round8(proc_26ec86668742) 11레인 ALL_GREEN → sol114 리뷰 → 인수 → 단일 커밋+push → W4 Gate3 완료 보고 정지.

**Gate3 인수 시 §8.2 W4 local-mock 검증 의무 전부 소화 — W4 종결**(실제 GitHub 어댑터·대상 repo 확인은 W6 별도 승인).

## 잔여 (비차단)

- 실제 외부 어댑터(GitHub)·대상 repo 바인딩 확인: W6 별도 승인·자격증명 경계(계약 문서 기록).
- W5 후속: invocation lease/queue(Gate1 post-fence 잔여 창), result.rs:650 시계, R4 device leaf — 기존 기록 유지.
