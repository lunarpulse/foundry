# W5 Gate1 Evidence — 감사 원천 연결·이벤트 불변 source scope (2026-10-24)

Base: `c516578`(W4 종결). 구현: luna115(ADD97) + luna116 remediation(ADD98, sol115 프라이버시 격차 폐쇄). 검증: 부모 round9 12레인 ALL_GREEN → sol115 REJECT(프라이버시 1건, X1~X5는 PASS) → 부모 round10 12레인 ALL_GREEN.

## sol115 격차 폐쇄 (ADD98/luna116)

- 격차: event_scope가 원본 source_instance_id·전역 event_seq를 scoped 응답에 노출(전환 쿼리 양-끝점 매칭으로 A-only 권한자가 B 원본 ID 열람) — 계약 :27·scope-arch :141 위반.
- 폐쇄: 요청-로컬 ExportKey alias(동일 원본 ID→동일 alias) + 로컬 순번(전역 시퀀스 미노출). 원장 불변 속성·쿼리 의미론 무변경 — 응답 표현만. X3 셀: alias 상이성·일관성·A/B 속성·로컬 순서 + 직렬화 리포트에 원본 ID/시퀀스 필드 부재 단언(audit-enabled 레인 4/0).

## 구현 요약 (delta 5 files, +854/−7)

- `crates/foundry-serve/src/audit_report.rs`(+85): ScopedProductAuditReportV3 additive 확장 — 인-스토어 원천(S4 hold/stage/result/session, S6 tuple/promotion, approval/grant/token/outcome, scope/service 이벤트, S5 로컬 publish 기록) available 보고 + 행 수·SHA-256 내용 digest·스키마 설명. unavailable 목록 실측 재분류(S2/S3 미연결, casting 별도 DB 비적용, M6·governance 별개 라벨).
- `crates/foundry-ledger/src/product_scope.rs`(+283): 읽기 전용 조회·digest 함수(신규 테이블·쓰기 0 — 부모 SQL 쓰기 스캔 0건 확인).
- `crates/foundry-ledger/src/product.rs`(+5): 감사 digest 도메인 도메인 태그(`foundry-product-audit-table-v1`).
- `crates/foundry-serve/tests/b1_export_audit.rs`(+376): 감사 셀 X1~X4 측정.
- `docs/design/b1-product-contract.md`(+36/−7): 감사 원천 현황·Gate2 분리 기록.

## 루브릭 판정 (X1~X5)

- **X1 PASS** — genuine fixture(부트스트랩→주문→승인→hold→publish) 후 scoped audit가 인-스토어 원천별 행 수+digest 보고(audit-enabled 레인 4/0 중 신규 감사 셀).
- **X2 PASS** — S5 "local publish records" 명시 + 원격 확인 증거 별도 unavailable(어댑터 부재) 유지.
- **X3 PASS** — 실제 A→B 전환 경유 후 이벤트 속성: 과거=A(당시 source_instance_id), 이후=B, 재분류 0건. product_service_events_v1에 source_instance_id 없음 → 추론하지 않고 미측정으로 정직 보고(원칙 준수).
- **X4 PASS** — S2/S3(미연결)·casting(별도 CastingLedger DB, 비적용)·M6·governance(각각 별개 사유) 상이 라벨. 전부 동일 문구 아님.
- **X5 PASS** — round9 12레인 exit 0 전부, 기존 count 감소 0, 기존 테스트 무수정.

## round9 (12 lanes ALL_GREEN)

u-cells 11/0(356.17s) · publish-enabled 3/0(81.45s) · **audit-enabled 4/0 신설**(40.68s) · http-enabled 6/0(40.01s/77.73s) · restore-default 10/0(80.12s) · restore-enabled 10/0(80.50s) · cli-enabled 7/0(29.82s/15.70s) · h-hx 44/0(307.37s/922.01s) · ledger 6/0(0.64s) · core 409/0 · watch 218/0 · cli-default 20/0 — 전부 `-- --test-threads=1`, `--locked --offline`, B1_LEDGER_TEST_EXE 프리빌드.

## 측정 한계 (정직 기록)

- product_service_events_v1에 source_instance_id 부재 — service 이벤트의 source 속성은 미추론(향후 스키마 항목).
- casting 증거는 별도 CastingLedger DB — ProductAtomicV1 감사 미탑재(비적용 사유 명시).
- S2/S3 원격 snapshot 어댑터·M6/governance 스토어 부재 — 각각 미연결 사유 명시.

## 결론

X1~X5 전 PASS — Gate1 인수 조건 충족. sol115 리뷰 진행.
