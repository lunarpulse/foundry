# B1 W4 Gate2 — 증거 문서 (CLI publish 경로, 2026-10-24)

기준: HEAD `bff9dff`(W4 Gate1). 델타: crates/foundry/Cargo.toml(+9), crates/foundry/src/main.rs(+319/-19 일부), docs/design/b1-product-contract.md(±2 라인), 신규 tests/b1_publish_cli.rs, tests/b1_publish_cli_default.rs. foundry-serve·foundry-ledger·foundry-line 프로덕션 코드 무변경.

## 구현 요약 (luna113, proc_8e14d2fa0a78)

- `ProductApprove --receipt-out <path>`(선택): 승인 성공 시 `ProductApprovalReceipt` canonical JSON을 0600 파일로 기록. 미지정 시 기존 동작 불변.
- 신규 `ProductPublish` 커맨드: `--db --installation-id --database-id --session-file --receipt-file`. 세션 64-hex 검증(기존 패턴) → 영수증 엄격 역직렬화 → 스토어 open+`scope_policy_enforced` → 퍼블리셔 미구성 시 **begin_product_publish 이전 거부**(exit 1, "no publish adapter configured — external publishing arrives in W6", 행 0) → 구성 시 foundry-line `publish_product_approved` 소비.
- p2-test-support 전용: env `B1_PUBLISH_TEST_DIR` armed 파일 레지스트리 퍼블리셔/리컨실러(operation-keyed 멱등, 네트워크 없음). 미 armed 시 미구성 경로와 동일.
- 계약 문서 :54/:63 갱신 — CLI publish 경로 존재·fail-closed 명시, "Approval does not publish"·"real external adapter remains W6" 유지.

## 루브릭 판정 (V1~V5, FROZEN .hermes/plans/b1-w4-gate2-plan-and-rubric-v1.md)

- **V1 PASS** — b1_publish_cli: 부트스트랩→승인(`--receipt-out`)→publish exit 0, pr_url 출력, `product_publish_outcomes_v1` confirmed 1행(receipt_json byte 동일), 레지스트리 PR 1(호출 1회).
- **V2 PASS** — 동일 operation 재게시: exit 0, 저장 URL 반환, 레지스트리 불변(신규 호출 0).
- **V3 PASS** — b1_publish_cli_default(기본 기능 빌드): 즉시 거부 exit 1, 미구성 메시지, publish 행 0. luna113 자체 1 passed + 부모 round7 cli-default 레인 포함 확인.
- **V4 PASS** — ①잘못된 세션 ②훼손/unknown-필드 영수증 ③타 저장소 영수증 ④미 armed 환경: 전부 exit 1, durable 행 0, 퍼블리셔 호출 0. luna113 "2 passed"(V1/V2+V4 통합 타깃), 부모 재판정 일치.
- **V5 PASS** — round7 11레인 ALL_GREEN(부모, proc_24f366ea86c4, 2026-10-24):
  - publish-enabled 2/0(61.78s) · http-enabled 6/0 · restore-default 10/0 · restore-enabled 10/0(79.85s) · cli-enabled 7/0 · h-hx 44/0(297.42s/906.37s) · ledger 6/0(0.65s) · **core 409/0**(`--lib --tests`, round6과 동일 409 — 본 델타는 foundry 크레이트만, core 크레이트 무변경) · watch 218/0 · u-cells 8/0(303.48s) · **cli-default 20/0**(기존 19 + V3 셀 1; luna 샌드박스의 b1_scope_cli loopback bind 거부 4건은 부모 환경에서 전부 green — 샌드박스 제한, 코드 결함 아님)
  - 기존 카운트 감소 0, 기존 테스트 무수정 통과(ProductApprove 기존 인자·출력 불변).

## 프로세스 이력

- 루브릭 V1~V5 동결(.hermes/plans/b1-w4-gate2-plan-and-rubric-v1.md) → ADD95(.specs/edit-spec-b1.md, gitignore 로컬) → luna113(proc_8e14d2fa0a78): 구현+셀 green, cli-default 샌드박스 loopback 4실패 정직 보고 → 부모 round7(proc_24f366ea86c4) 11레인 ALL_GREEN로 재판정(loopback 포함 해소) → sol113 리뷰 → 인수 → 단일 커밋+push.

## 잔여 (비차단)

- 실제 외부 어댑터(GitHub)는 W6 별도 승인·자격증명 경계. 기본 빌드 fail-closed 유지.
- Gate1에서 기록된 U4/P2 post-fence pre-call 잔여 창(W5 invocation lease/queue 후속) — Gate2 경로는 foundry-line 동일 조합 소비, 창 변화 없음.
- N2 저장소 회귀 커버리지(Gate1 sol111 지적, APPROVE 무관)·result.rs:650 시계·progress_revision 원본 부재 — 기존 기록 유지.
