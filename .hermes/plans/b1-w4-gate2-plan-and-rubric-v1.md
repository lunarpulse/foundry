# B1 W4 Gate2 — CLI Publish 경로 (계획·루브릭 v1, FROZEN 2026-10-24)

승인 근거: 사용자 "착수"(W4 Gate2 착수, 2026-10-24). 상설 자동 승인 원칙(Discord 1563206386211496081) 적용 — 본 문서 동결 후 구현 착수. Gate1 커밋 `bff9dff` 위에 구축.

## 1. 실측 조사 (2026-10-24, HEAD bff9dff)

- `foundry-serve`는 구조적으로 게시 금지: `fake_noop.rs:38` `NoopPublisher`("serve does not publish — use watch/CLI publish"), `lib.rs:130` Line이 이를 주입. HTTP 라우트 추가는 이 원칙과 충돌 → **Gate2는 CLI 경로**가 계약 부합적 선택.
- `foundry` CLI(crates/foundry/src/main.rs)에 `ProductApprove`(:259, product_approve_cmd :883) 선례: `--session-file`(64 hex 비공개 파일) + grant 파일 + 스토어 open(`scope_policy_enforced` 검사) → `prepare_approval`→`approve_product` → `receipt.event_hash` 출력. **영수증은 파일로 저장되지 않음**.
- `ProductApprovalReceipt`(foundry-ledger product.rs:153): operation_id, actor_id, order_id, lifecycle, sequence, event_hash, artifact_digest, attempt, run_id, generation, tuple_digest, source_revision — 호출자가 재구성 불가능한 필드(actor_id, sequence, lifecycle, event_hash) 포함 → **재구성 대신 영수증 파일 전달**이 정확.
- Gate1 `publish_product_approved`(foundry-line product.rs) + `claim_product_publish_retry`/`settle_product_publish` + `product_publish_outcomes_v1`(receipt_json byte 동일성 :1315) 이미 인수됨. CLI는 이 조합을 소비만 한다.
- 실제 외부 어댑터는 W6(별도 승인/자격증명 경계). 현재 실제 Publisher 구현 부재 → **기본 빌드 publish는 자격 미구성으로 즉시 거부(fail-closed)**, p2-test-support 기능에서만 결정적 테스트 퍼블리셔·리컨실러 구동(기존 default-off 관례 준수).
- 계약 문서 docs/design/b1-product-contract.md "Remaining B1 acceptance gaps"(:63) "no HTTP/CLI publish path" 문언 갱신 필요.

## 2. Gate2 구현 범위

원칙: **CLI는 인증 세션과 byte 정확 영수증을 전달하고, 모든 durable 의사결정은 이미 인수된 ProductStore/foundry-line 조합이 내린다. CLI 자체는 새로운 권한·검증 논리를 만들지 않는다. 실제 외부 호출 없음(기본 빌드 fail-closed).**

1. **`ProductApprove`에 `--receipt-out <path>` 추가**(선택 인자, 기존 동작 불변): 승인 성공 시 `ProductApprovalReceipt`를 canonical JSON으로 파일 기록(0600). 미지정 시 현재와 동일(event_hash 출력만).
2. **신규 `ProductPublish` 커맨드**: 인자 `--db --installation-id --database-id --session-file --receipt-file`. 동작: 세션 파일 검증(64 hex, 기존 패턴) → 영수증 파일 읽기·엄격 역직렬화(unknown 필드 거부, serde 기존 관례) → 스토어 open + `scope_policy_enforced` → **퍼블리셔 미구성 시 즉시 거부**(기본 빌드: "no publish adapter configured — external publishing arrives in W6" 출력, exit 1, 스토어 행 0) → 구성 시(p2-test-support) `foundry_line::publish_product_approved` 호출 → 결과 출력(Confirmed=pr_url / Unknown=operation_id 재시도 안내).
3. **테스트 퍼블리셔·리컨실러**(p2-test-support 게이트, foundry CLI 크레이트 내): 환경 변수로 armed(`B1_PUBLISH_TEST_DIR` 등 결정적 디렉터리), operation-keyed 멱등(operation_id당 PR 1개, 파일 레지스트리), reconciler는 같은 레지스트리 조회. 미 armed 시 퍼블리셔 미구성 취급(fail-closed과 동일 경로).
4. **계약 문서 갱신**: publish CLI 경로 존재(기본 빌드 fail-closed, 실제 어댑터는 W6) 문언으로 gap 라인 수정. "Approval does not publish" 원칙 유지(승인이 게시를 유발하지 않음 — 게시는 별도 인증 명령).
5. bounds: foundry(CLI)·foundry-line(불변 소비)·테스트·계약 문서만. foundry-serve 무변경. 실제 GitHub/자격증명/Die 금지. additive CLI 인자만(기존 인자·종료코드 불변). Cargo --locked --offline, jobs=1, incremental=0, disk guard 2GiB, B1_LEDGER_TEST_EXE 프리빌드. deletion-to-green 금지, assertion 약화 금지.

## 3. Gate2 루브릭 v1 (행별 단언 — 동결)

- **V1 cli_publish_positive (L)** — p2-test-support: 부트스트랩→승인(`--receipt-out`)→`product publish --receipt-file` → exit 0, 출력 pr_url, `product_publish_outcomes_v1` confirmed 행 1개(receipt_json byte 동일), 테스트 퍼블리셔 레지스트리 PR 1개(publisher 호출 1회).
- **V2 cli_publish_confirmed_replay (L)** — 동일 operation_id 재게시 요청 → exit 0, 저장 pr_url 반환, 퍼블리셔 신규 호출 0(레지스트리 불변).
- **V3 cli_publish_fail_closed_default (L)** — 기본 기능(default, p2-test-support 없음) 빌드에서 publish → 즉시 거부 exit 1, 명시적 미구성 메시지, `product_publish_outcomes_v1` 0행(스토어 미기록 — 거부가 begin_product_publish 이전).
- **V4 cli_publish_denials (L)** — ①잘못된 세션 파일 ②훼손 영수증(byte/필드 변경, unknown 필드 포함) ③타 스토어/타 주문 영수증 ④퍼블리셔 미 armed 환경 → 전부 exit 1, durable publish 행 0, 퍼블리셔 호출 0.
- **V5 regression_lanes_preserved (P)** — 기존 레인(cli-default·cli-enabled·http·restore·h-hx·ledger·core·watch·u-cells) 전 exit 0, 기존 count 감소 0, ProductApprove 기존 인자·출력 불변(기존 테스트 무수정 통과).

완료 기준: V1~V5 전부 PASS. 셀 판정은 이 표 행별 단언 인용으로만.

## 4. 순서

ADD95(edit-spec) 작성 → luna 위임 구현 → 부모 독립 재판정(레인) → 증거 문서 → sol 리뷰 → 인수 → 단일 커밋+push → W4 Gate2 완료 보고 정지.
