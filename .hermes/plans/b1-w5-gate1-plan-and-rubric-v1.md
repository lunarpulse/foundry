# B1 W5 Gate1 — 감사 원천 연결·이벤트 불변 source scope (계획·루브릭 v1, FROZEN 2026-10-24)

승인 근거: 사용자 "착수해"(W5 착수, 2026-10-24, Discord 1563508045521227898). 상설 자동 승인(1563206386211496081) 적용 — 동결 후 구현 착수. 기준 HEAD `c516578`(W4 종결). 원천 정의: b1-closure §W5 + 전환 계획 §8.3.

## 1. 실측 조사 (2026-10-24, HEAD c516578)

- `audit_report.rs`(foundry-serve, 749행): `ProductAuditReportV2`(S1만 available, S2~S6·M6·governance·casting 전부 **정적 "unavailable"**) 및 `ScopedProductAuditReportV3`(scoped; S1 available/partial + 동일 정적 unavailable 목록). "full v2 audit source inventory remain absent"(계약 :54)이 바로 이 갭.
- 인-스토어 원천은 이미 ProductStore에 존재(전부 동일 검증 저장소):
  - **S4 제품 hold/stage/result/session**: product_review_holds_v1·hold_acceptances_v1·hold_progress_v1·runs_v1·stages_v1·sessions_v1·host_results_v1 (+ product_review_work_bindings_v1)
  - **S6 tuple/promotion**: product_result_bindings_v1(+tuple promotion 경로, result.rs)
  - **승인/grant/token/outcome**: product_outcomes_v1·product_grants_v1·product_token_consumptions_v1·product_service_operations_v1
  - **scope/service 변경·복구**: product_scope_events_v1·product_scope_source_events_v1·product_service_events_v1·product_scope_migrations_v1·product_scope_quarantine_v1
  - **S5 durable publish(로컬 기록)**: product_publish_outcomes_v1(W4; 원격 확인 증거 어댑터는 부재 — 구분 필수)
- 이벤트 당시 source scope: product_scope_events_v1/product_service_events_v1에 source_instance_id·revision 필드 존재(실측 :208~:316) — 감사 보고 시 이벤트 시점 속성 사용 가능. service A→B 전환 후 과거 A 이벤트의 현재-B 재분류 금지 단언은 이 컬럼 기반으로 측정.
- 비연결 원천 조사 대상: S2/S3(외부 snapshot 어댑터 부재), casting(foundry-ledger casting_journal.rs 존재 — ProductAtomicV1 감사 미탑재 여부 실측), M6/governance(동일시 금지 — 별개 라벨).
- export MUST(첫 100·has_more·무제한 바이트 명시 등)와 제품 서버 P1/P2 인수는 **Gate2로 분리**(본 Gate 아님). W4의 invocation lease 후속은 W6+ 어댑터 착지 조건부 — 본 Gate 아님.

## 2. Gate1 구현 범위

원칙: **같은 검증 저장소에서 읽은 원천만 "available/partial"로 보고하고, 각 원천의 근거(행 수·digest)를 명시한다. 없는 것은 없다고 명시한다(비적용/미연결/유예 구분). 원격 증거 없는 S5는 로컬 publish 기록으로만 보고한다. 감사는 읽기 전용 — 새 권한·쓰기 경로 없음.**

1. **ScopedProductAuditReportV3 원천 연결**(foundry-serve audit_report.rs, additive 확장): 인-스토어 원천(S4·S6·approval/grant/token/outcome·scope/service 이벤트·S5 로컬 publish)을 available/partial로 보고 — 각 원천에 행 수와 내용 digest(정렬 직렬화 SHA-256), 스키마 설명. unavailable 목록은 실측 기반으로 재분류: S2/S3=미연결(어댑터 부재), casting=실측 결과(탑재 가능하면 연결, 아니면 비적용 사유), M6와 governance=별개 라벨 각각 조사·명시. 기존 v3 응답 스키마는 additive 필드로만 확장(기존 필드·값 불변, 기존 테스트 무수정 통과).
2. **이벤트 불변 source scope 단언**: 감사 원천 보고의 scope/service 이벤트는 이벤트 행에 저장된 당시 source_instance_id/revision으로 속성 보고. 테스트: 소스/서비스 A→B 전환 시나리오에서 감사가 과거 A 이벤트를 A로, 신규 B 이벤트를 B로 보고(현재 상태로 재분류 없음).
3. **테스트**(b1_export_audit.rs 확장 또는 신규 감사 셀): X1~X4 루브릭 행 측정. genuine fixture(기존 부트스트랩·승인·publish 셀 패턴 재사용), direct seeding 금지.
4. **계약 문서 갱신**: 감사 원천 현황(연결/미연결/비적용 목록) 및 export·P1/P2가 Gate2임을 기록.
5. bounds: foundry-serve(audit_report.rs·필요 시 privacy.rs 읽기)·테스트·계약 문서만. foundry-ledger 프로덕션 무변경(읽기는 기존 공개 API/스냅샷으로; 부족 시 스토어 읽기 전용 조회 함수 추가는 최소 범위 허용 — 신규 테이블·쓰기 금지). 감사 경로 권한·인증 변동 없음. 실제 외부 호출·자격증명 금지. Cargo --locked --offline, jobs=1, incremental=0, disk guard 2GiB, B1_LEDGER_TEST_EXE 프리빌드. deletion-to-green·assertion 약화 금지.

## 3. Gate1 루브릭 v1 (행별 단언 — 동결)

- **X1 in_store_sources_connected (L)** — genuine fixture(부트스트랩→주문→승인→hold→publish) 후 scoped audit가 S4(hold/stage/result/session)·S6(tuple/promotion)·approval/grant/token/outcome·scope/service 이벤트·S5 로컬 publish 각각을 available로 행 수+digest와 함께 보고(값 0이면 "available, 0행" — 없다고 숨기지 않음).
- **X2 local_vs_remote_publish_separated (L)** — S5 항목이 "local publish records"로 명시 보고되고, 원격 확인 증거는 별도 unavailable 항목으로 유지(어댑터 부재 사유). 구분 문구가 응답에 존재.
- **X3 event_time_source_scope_immutable (L)** — 소스 A→B 전환(실제 scope/service 변경 경유) 후 감사의 이벤트 속성: 과거 이벤트는 A(당시 source_instance_id), 전환 이후는 B. 현재 상태 기준 재분류 0건.
- **X4 unavailable_taxonomy_investigated (L)** — S2/S3·M6·governance·casting이 서로 구분된 라벨(미연결/비적용/조사 결과)과 사유로 보고(전부 동일 문구 아님; M6≠governance). 각 사유는 실측 근거(코드에 존재 여부)와 일치.
- **X5 regression_lanes_preserved (P)** — 기존 레인 전부 exit 0, 기존 count 감소 0, 기존 테스트 무수정 통과(감사 v3 기존 필드·값 불변).

완료 기준: X1~X5 전부 PASS. 셀 판정은 이 표 행별 단언 인용으로만.

## 4. 순서

ADD97(edit-spec) 작성 → luna 위임 구현 → 부모 독립 재판정(레인) → 증거 문서 → sol 리뷰 → 인수 → 단일 커밋+push → W5 Gate1 완료 보고 정지. Gate2(export MUST·P1/P2 인수)는 후속 착수 지시 대기.
