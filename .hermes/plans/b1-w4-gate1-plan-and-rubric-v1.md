# B1 W4 Gate1 — Durable External Publication 파이프라인 (계획·루브릭 v1, FROZEN 2026-10-24)

승인 근거: 사용자 "진행해"(W3 수용+W4 범위 승인, §8.1 충족). 상설 자동 승인 원칙(Discord 1563206386211496081) 적용 — 본 문서 동결 후 구현 착수.

## 1. 실측 조사 (2026-10-24, HEAD c854abc)

- `ProductStore::authorize_product_publish`(product.rs:1245)는 v3(`result_schema>=3`)에서 `Invariant("legacy publish authorization is disabled for ProductAtomicV1 v3")` 즉시 거부 — legacy(v1/v2) 전용.
- `scope_enforced_in`(product.rs:5263) 모드에서는 "publish requires an authenticated scoped authorization" — v3 경로는 scoped `PublishApproved` capability(product_scope.rs:149)가 정답이나 소비자 부재.
- `ProductProjection::publish_approved`(foundry-line/product.rs:128) = legacy gate + Publisher 포트 조합. 계약(docs/design/b1-product-contract.md) 21행: "not yet wired to an HTTP or CLI command and does not persist a product publish outcome", 19행: "Approval does not publish", 63행: "no HTTP/CLI publish path or durable product publish outcome".
- `Publisher` 포트(foundry-domain/ports.rs:119): `publish(order, operation_id, expected_digest) -> Confirmed{pr_url} | Unknown{operation_id}` — operation_id 멱등(같은 id = 같은 PR), Unknown은 reconcile 대상.
- 기존 테스트: b1_product_restore.rs:426 위조 receipt → publisher 0회 호출 거부 composition 1건. v3 양성 간행 경로 부재.

## 2. Gate1 구현 범위 (계획 §8.2의 첫 조각)

원칙: **재검증·권한·대상 확인과 durable publish-start를 한 트랜잭션에 기록하고, COMMIT 이후에만 외부 호출한다. 외부 효과는 outcome 행으로 추적하며, 이미 확정된 외부 효과는 재실행하지 않는다.**

1. **신규 테이블 1개** (v3 스키마 확장 — Gate3의 "no schema" 한도는 W4부터 해제, 최소 설계):
   `product_publish_outcomes_v1(operation_id TEXT PRIMARY KEY REFERENCES product_outcomes_v1, receipt_json TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','confirmed')), pr_url TEXT, started_at INTEGER NOT NULL, settled_at INTEGER)` — 생성 DDL은 v3 스키마 버전 4로 승격하지 않고 v3 내 추가 테이블로 한다(기존 v3 store 호환: CREATE TABLE IF NOT EXISTS on open). 이벤트 불변성: state는 pending→confirmed 단방향, confirmed 행은 갱신 불가(트리거로 차단).
2. **`ProductStore::begin_product_publish`**(foundry-ledger/product.rs 또는 result.rs):
   - 한 트랜잭션에서: scoped principal의 `PublishApproved` capability 검사(require_order_capability_in 계열, 외부 행 차단 포함) + v3 재검증(저장 outcome과 receipt byte 동일·현재 S1 lifecycle=publishing·source binding 현재·승격 tuple digest 일치 — 기존 authorize 내부 술어 재사용) + publish-start INSERT.
   - 멱등: 기존 confirmed 행 → `ProductPublishStart::Confirmed(pr_url)` 반환(외부 재호출 금지). 기존 pending 행 → `ProductPublishStart::Pending`(재실행 아닌 reconcile 의무). 신규 → INSERT 후 `Pending`.
3. **`ProductStore::settle_product_publish(operation_id, Confirmed{pr_url})`**: pending→confirmed 확정 기록. Unknown은 settle하지 않는다(pending 유지).
4. **`ProductLine` 조합 또는 store 직저 사용 셀**: begin(commit 완료) 이후 `publisher.publish` 호출 → Confirmed이면 settle. Unknown이면 pending 유지. 이 Gate1에서는 HTTP/CLI 라우팅 없이 라이브러리 경로+테스트로 측정한다(라우팅은 W4 Gate2).
5. **회복**: 크래시(publish-start commit 후 외부 호출 전) → 재오픈 시 pending 행 관찰 → 동일 operation_id 재호출은 어댑터 멱등성(같은 id=같은 PR)으로 안전, 총 PR 1개. 모든 crash/reconcile 시험은 **local mock Publisher**로만(실제 GitHub은 W6/별도 승인).

## 3. Gate1 루브릭 v1 (행별 단언 — 동결)

- **U1 revalidation_and_capability_gate_before_any_start (L)** — genuine 승인 결과에 대해서만 publish-start가 기록된다: ①정상 경로 begin→pending 행 1개(receipt_json 원문 byte) ②위조 receipt(byte 불일치) ③lifecycle 비현재(이후 사건) ④source binding 변경/비활성 ⑤PublishApproved capability 없는 principal/외부 행/차감된 scope ⑥비-publishing 상태 — ②~⑥은 오류 전파 + publish 테이블 0행.
- **U2 start_commits_before_external_call_and_survives_crash (L)** — mock Publisher가 호출되기 전에 publish-start가 이미 커밋됨(호출 전 강제 실패 재현: publisher 호출 시점에 별도 연결로 pending 행 가시). begin 후·외부 호출 전 프로세스 크래시(processes 셀 또는 동등 강제 중단) → 재오픈 pending 관찰 → 재시도 → **총 publisher 호출로 정확히 1개 PR**(같은 operation_id 멱등), 최종 confirmed+pr_url.
- **U3 response_loss_unknown_keeps_pending_and_reconciles (L)** — mock이 Unknown 반환 → settle 없음, pending 유지, publisher 추가 호출 0회. reconcile 경로(어댑터 상태 조회 mock)가 Confirmed로 확정하면 settle 1회·PR 1개. Unknown을 부재(absence)로 취급하지 않는다.
- **U4 confirmed_is_never_reexecuted (L)** — confirmed settled 후 동일 operation_id 재요청 → 저장된 pr_url 반환, publisher 호출 0회 (이미 수락된 외부 효과 vs 새 실행 권한 구분). confirmed 행 갱신/삭제 시도는 트리거/거부로 차단.
- **U5 regression_nine_lanes_preserved (P)** — round100 정의 9레인 전 exit 0, 기존 count 감소 0, H/HX 단언 mapping 유지.

완료 기준: U1~U5 전부 PASS. 셀 판정은 이 표 행별 단언 인용으로만. bounds: W4 Gate1은 foundry-ledger(product.rs/result.rs/product_scope.rs)·foundry-line·foundry-domain(불변 시에 한함)·테스트만 변경. 실제 GitHub/Die 호출 금지, 외부 credential 금지, mock Publisher만. Cargo --locked --offline, jobs=1, incremental=0, disk guard 2GiB, B1_LEDGER_TEST_EXE JSON 프리빌드. deletion-to-green 금지, assertion 약화 금지.

## 4. 순서

ADD92(edit-spec) 작성 → luna 위임 구현 → 부모 독립 재판정(9레인) → 증거 문서 → sol 리뷰 → 인수 → 단일 커밋+push → W4 Gate1 완료 보고 정지.
