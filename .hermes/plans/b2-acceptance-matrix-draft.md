# B2 인수 매트릭스 — W0 작업 초안

W0 교차 단계 정책 상태 **READY FOR DOCUMENTATION COMMIT**; 부모 검증과 W0 로컬 커밋 시 동결되며 그 전에는 W0 미완료다. 26개 제품 인수행은 **DRAFT/BLOCKED/NOT-RUN**이다. 기준 HEAD `971240703aa004347c674b5c8ff1702435908b4d`. 이 문서는 설계·시험 계획이며 구현 또는 데이터 마이그레이션의 승인·증거가 아니다. 기존 B1 계약, receipt 및 원 M9 수용 검증을 축소하지 않는다. 실 DB와 외부 GitHub는 별도 효과 범위다. 각 행의 test 식별자는 **예정 이름**이며 아직 존재 여부를 주장하지 않는다.

## 공통 실행·판정 규칙

- 로컬 독립 임시 DB/작업공간만 사용한다. 실제 자격증명·운영 DB·원격 쓰기 금지. Rust 시험은 `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline`을 사용한다. 테스트·실행 파일 경로와 정확한 CLI 명령은 구현 전에 동결한다.
- 각 시험은 정상 제어군을 먼저 성립시키고 목표 요소 하나만 바꾼다. denial에는 상태·외부 효과 불변을 함께 단언한다. 승인된 디버그 표본과 호스트의 실제 관측은 분리하고 fake/scripted Die, 진짜 Die, 실제 ProductStore, Docker runtime, 외부 서비스는 각각 표시한다.
- PASS는 행이 요구하는 **모든** 긍정/부정 단언, 해당 관측 층위 및 증거가 충족된 경우만 가능. 하나라도 미실행이면 NOT-RUN 또는 NOT-PERFORMED이지 PASS가 아니다. 조건 미확정은 BLOCKED, 기대와 다른 결과는 FAIL로 기록한다. 전체 PASS는 모든 필수 행 PASS 및 명시한 회귀 전체 충족 시에만 가능하다.
- 예산/수치와 오류 precedence는 출처·경계값·이유가 정해지기 전 임의 확정하지 않는다. W0에서 권한·구 이력·입력·격리의 교차 단계 안전 의미를 정하고, 각 단계 코드를 시작하기 전에 그 단계의 정확한 계약과 API·테스트 위치·입력·단언·증거를 동결한다.

## W2a 선행 seam — W1 인수 전 (DRAFT)

- **A01 `b2_product_producer_seam_is_wired`**: 단일 기존 승인 팩과 독립 임시 ProductStore를 이용해 실제 제품 entrypoint가 명시적으로 승인된 고정 입력을 source/admission→ready→claim→producer→review host/finalizer까지 전달하고 각 경계의 order/lifecycle/source/claim/fence/commit 결속을 관측한다. 테스트 executor 또는 격리된 대체 producer 사용 시 그 사실을 증거에 명시하며, 실제 Die·E01·컨테이너 합격으로 간주하지 않는다. 정확한 허용 입력·task binding·authority·준비 API·테스트 파일과 entrypoint가 정해지기 전 BLOCKED. P2 fixture의 임의 `ready` 승격이나 S1 결과 직접 주입은 이 제품 producer seam의 증거가 아니다.
- A01을 먼저 만들더라도 실제 미신뢰 task를 실제 Die에 전달하는 것은 격리 선행조건의 증거 없이 금지한다. W2a는 W2 완료가 아니며 W1 P02의 제품 entrypoint 전제를 제공하는 선행 작업이다. DRAFT 단계 순서는 W0 → W2a(A01) → W1(P행) → W2(E행) → W3(I행) → W4(R행); 각 W의 로컬 완료 커밋에서 정지·보고한다.

## W1 — 주문·lifecycle 팩 pin

- **P01 `b2_unauthorized_pack_selection_is_effect_free`**: 현재 사용자/서비스 권한이 없는 요청과 승인되지 않은 digest를 각각 거부. 신규 pin·review run·work/hold 상태 및 executor 호출 변화 없음. 정상 승인된 control은 같은 제품 진입점에서 수행.
- **P01b `b2_owner_selection_revision_and_revocation_are_bound`**: 승인 X 선택 뒤 owner가 Y로 변경하는 정상 제어, 선택자 위조·타 source·stale revision·동일 idempotency key의 다른 내용 거부를 각각 구분한다. 선택 기록만으로는 review pin·결과 승인·executor 호출이 생기지 않음을 확인한다. 승인 취소 후 같은 digest 재승인 시 새 revision이 과거와 구별되고 원 pin/receipt는 보존되며 원 revision으로 새로운 stage/Resume은 실행되지 않는다. 취소와 이미 실행 중인 stage의 수집/승인, 비실행 hold 결정의 허용 범위는 계약 확정 전 BLOCKED.
- **P02 `b2_two_orders_run_different_approved_packs`**: 동일 제품 바이너리·동일 source/repository의 두 실제 ProductStore 주문에 owner가 승인된 상이 digest X/Y를 각각 지정하고, 각 host의 실행에 사용된 snapshot bytes→digest→run 결속을 확인. 다른 source 두 개만 시험하면 이 행을 충족하지 않는다. 테스트 전용 pack의 범위와 fake/실제 executor 여부를 증거에 명시; 실 Die 행을 대체하지 않는다.
- **P03 `b2_first_pin_race_is_atomic_across_connections`** 및 **P03b `b2_first_pin_race_is_atomic_across_processes`**: 선택 정책이 '주문별 현재 선택 하나+revision CAS'라면 서로 다른 X/Y가 동일 시점의 현재 선택으로 둘 다 유효하다는 전제는 성립하지 않으므로, 먼저 X 선택→Y 변경과 X admission의 두 commit 순서를 별도 결정론적으로 시험한다. admission이 먼저면 X pin과 Y 변경 거부, 변경이 먼저면 X stale-revision 거부 후 Y admission 성공. 각 정상 대조군에서 source·grant·claim·DB 전제를 확인하고, 성공한 admission에 정확히 한 digest가 commit되며 패자 거래의 pin/run/work/authority/active 부작용은 없어야 한다. DB busy는 conflict와 구별하고 명시한 재시도를 수행한다. 동일 activation 재시도는 재실행되지 않으며 기존 activation·예약 run alias는 거부된다. commit 직전·직후 중단을 별도로 주입해 pin/run/work/authority/active가 부분 commit되지 않는지 확인한다. 독립 프로세스의 연결·barrier와 commit 순서는 증거로 남긴다. 이 expected ordering은 DRAFT이며 selection transaction·선택 변경의 구체 계약과 제어 가능한 프로세스 fixture를 동결한 뒤에만 실행한다.
- **P04 `b2_restart_and_accepted_hold_use_original_pack`**: X로 실제 제품 hold 생성·결정 수락, 프로세스 재시작과 registry 순서 변경 후 original X snapshot으로 정확한 continuation. 새 설정 Y로 자동 전환 금지; current session/scope 및 권한과 work fence 검사 보존. 잘못된 host 및 누락 host도 별도 부정 확인.
- **P05 `b2_snapshot_missing_tampered_or_swapped_is_rejected`**: 각각 snapshot 부재, 바이트 변조, 검증 이후 경로 교체를 독립 fixture에서 수행. 실행에 사용되는 바이트가 검증된 digest와 다르면 실행·결과 추가 금지, 기존 hold 및 기록 보존. replacement의 관측 방법은 구현 전 동결한다.
- **P06 `b2_foreign_order_repository_session_or_tuple_is_rejected`**: order, stable repository identity, session, tuple을 각각 바꿔 거부. scope 밖 digest·내부 경로 비노출과 pin·executor 무변화를 확인.
- **P07 `b2_legacy_and_lifecycle_policy_is_enforced`**: 구 DB의 digest 이력 `{단일,혼합,부재}` × 원 authority `{완전,불완전}` × 원 snapshot `{존재·검증,부재·변조}`를 synthetic fixture로 구분한다. 각 행에서 권한 검사된 기존 read·receipt replay, accepted continuation, 새 실행을 각각 관측하고 정해진 legacy 정책에 대조한다. authority 부재를 현재 grant로 대체하거나 잘못된 snapshot을 실행하지 않는다. 혼합 이력의 존재를 실제 운영 DB의 사실로 가정하지 않는다. 신규 lifecycle의 선택과 기존 active/held 및 order-level epoch 충돌도 별도 확인한다. 기존 인가·receipt가 없으면 접근 가능한 것으로 가정하지 않는다.
  - **후보 판정 함수:** 이력이 있는 12개 조합 중 `단일 digest ∧ 원 authority 완전 ∧ 원 snapshot 검증 성공`인 1개 조합만 원 run continuation의 현재 권한·fence·registry 추가 검사를 수행한다. 그 검사 통과 시에도 B1의 원래 조건을 모두 만족해야 한다. 나머지 11개 조합은 continuation을 거부한다. `snapshot 부재`와 `변조`는 각 조합에서 별도로 주입한다. 12/11 수치는 `itertools.product` 로컬 열거 결과이며 실제 fixture나 운영 데이터의 수가 아니다.
  - 전 조합에서 원 기록은 기존 read scope/role에 맞는 사람에게만 제공하고 receipt는 기존 actor/권한 및 정확 retry binding을 따로 확인한다. 해당 legacy lifecycle의 **새 activation은 원 이력에서 임의 pin/backfill하지 않고 차단**하는 후보; run 자체가 없는 별도 control은 현재 권한으로 최초 pin 가능성을 검사한다. 새 lifecycle은 active/held/epoch를 우회하지 않는다. 이 정책과 기존 B1 허용 continuation의 충돌 여부는 동결 전 대조가 필요하다.
- **P08 `b2_old_writer_and_migration_rollback_are_fail_closed`**: 실제 구 바이너리가 격리된 v3 DB에 migration 전 connection을 연 상태, 신규 바이너리의 migration, 구 연결 재쓰기 및 구 바이너리 새 open을 시험한다. 일치하는 pin이 이미 존재하는 동일 digest의 구 stage-start/Resume와 pin 부재·다른 digest의 구 activation을 각각 포함해 pin 일치만의 가짜 fence를 배제한다. 각 부정 케이스에서 pin/run/hold/stage 및 다른 변경 대상 row 전체 불변을 단언한다. 별도 양성 제어: 신규 writer의 정상 pin+run commit과 B1의 migration 전 accepted stage 결과 수집은 성공하고, revoke 후 새 stage는 거부된다. migration commit 전/후 중단과 재시작을 분리하고, WAL을 포함한 backup/복원과 receipt/result 보존을 검증한다. 신 writer의 첫 durable write 이후 과거 backup 복원은 '무손실 rollback' PASS가 아닌 재해복구/reconciliation 별도 범위로 기록한다. UDF 사용 시 미등록·`trusted_schema=OFF`와 정상 innocuous 등록을 대조하며 DB 파일 접근 가능한 공격자에 대한 보안 주장 금지. 구현·정책을 확정하기 전은 BLOCKED.
- **P09 `b2_acceptance_retry_preserves_receipt_across_runtime_loss`**: 신규 결정에서 host 없음과 Resume에서 executor unavailable을 별도 시험해 수락·receipt가 생기지 않음을 단언한다. 가용성 확인 후 수락 전 runtime 소실과 수락 후 continuation 실패도 별도 주입하여 결정의 durable 수락 여부·receipt·실행 횟수를 검사한다. 이미 수락된 정확한 retry는 현재 권한 검사 후 원 receipt/progress를 반환하고 중복 acceptance·실행을 만들지 않는다. 이미 `completed` 또는 `recovery_required`인 상태에서 continuation 호출이 0임을 각각 확인한다.

## W2 — 실제 제품 실행

- **E01 `b2_product_real_die_commit_tuple_matches_approval`**: 실제 제품 진입점, 실제 ProductStore의 service grant/claim/fence, 명시된 허용 입력, 실제 Die binary, host 결과 및 승인까지 연결. Die가 생성한 commit, 최종 tuple의 immutable commit과 승인 digest를 비교하고 실행 프로세스·binary·격리 환경을 기록. 실제 외부 GitHub 연결은 별도 증거가 없으면 주장하지 않는다.
- **E02 `b2_untrusted_and_unsupported_issue_is_never_a_command`**: 허용 grammar 밖과 악의적 issue 본문 각 fixture에 대해 확정된 대기/거부 상태를 확인하고 Die·shell·외부 효과가 0임을 단언한다. 제품 입력 grammar가 결정될 때까지 BLOCKED.
- **E02b `b2_issue_edit_does_not_replace_admitted_task`**: 같은 connector/repository/issue의 최초 유효 snapshot A 수용 이후 본문 B의 새 delivery를 관측한다. 원 A와 admission receipt 및 task/승인 identity가 변하지 않고 B로 별도 실행이 생기지 않음을 단언한다. 별도 fixture에서는 최초 미지원 A 뒤 허용 형식 B를 편집해도 B가 자동 실행되지 않음을 확인한다. 실제 허용 입력·검증 경로와 명시적 새 작업 정책이 동결되기 전 BLOCKED; 기존 B1 편집 경로의 정적 관찰을 W2 기능 통과로 취급하지 않는다.
- **E03 `b2_revocation_and_fence_stop_new_work`**: source/service/인간 grant revoke, revoke→regrant ABA, claim 재할당, timeout 및 중단을 독립 fixture로 수행한다. 이미 수락한 결과 보존과 새 stage 차단을 구분하고 오래된 결과가 새 권한으로 승인되지 않음을 검증한다.
- **E04 `b2_runtime_configuration_errors_are_recoverable`**: pack, Die, repo 누락·부분 설정, invalid timeout, 죽은 프로세스를 각각 생성. 명세된 오류·내구 상태·복구 경로를 확인하고 불명확한 기본값 사용을 성공으로 간주하지 않는다.

## W3 — 격리 및 배포

- **I01 `b2_compose_render_and_override_security_policy`**: 기본과 허용 override로 실제 Compose render 후 이미지 pin·UID·mount·env·capability·read-only·network 등 동결된 정책을 검사한다. 보안 속성을 악화시키는 override는 각각 거부되거나 인수 실패해야 한다.
- **I02 `b2_die_cannot_read_product_db_credentials_or_socket`**: 실행 중 실제 Die/cast 내부에서 DB/WAL·credential·host HOME·Docker socket 접근 시도. mount/env/UID 및 호스트 파일·DB 상태를 함께 관측한다. 파일 부재 관측만으로 임의 호스트 파일 전체 비접근을 주장하지 않는다.
- **I03 `b2_die_cannot_reach_privileged_api_or_other_order`**: 별개 주문의 실 작업공간/산출물 읽기·수정, 같은 주문을 포함한 watch 소유 산출물 수정, 권한 serve API 직접 호출을 각각 시도해 실패 및 원 파일 불변을 확인한다. 정상 본인 작업공간 쓰기는 성공해야 한다.
- **I04 `b2_authenticated_budgeted_proxy_request_succeeds`**: 실제 listener/transport를 경유한 허용 요청이 정확한 run·provider·예산에 결속되어 성공한다. 테스트 upstream 사용 여부를 표시하고 실제 외부 provider 이용으로 오인하지 않는다.
- **I05 `b2_direct_egress_and_budget_overrun_are_denied`**: 해당 환경에서 지원하는 IPv4/IPv6, DNS/IP 직접 우회, redirect 및 예산 초과를 각각 실시. 요청별 upstream 도달·charge·결과 상태를 확인. 정책상 미지원 주소군은 설정과 범위를 증거화하고 미검증 부분을 명시한다.
- **I06 `b2_runtime_identity_mount_caps_and_children`**: 실제 runtime UID, mount, caps, no-new-privileges, rootfs, 네트워크를 검사하고 timeout/종료 뒤 자식 프로세스가 결과를 계속 쓸 수 없는지 확인한다.
- 기존 `t56w_docker_mandatory`는 고정 이름 container/network 삭제, 이미지 pull/tag를 수행하므로 사전 안전 검토 전 실행 금지. 오프라인 안전 대체/fixture를 마련해야 하며 도구 미가용·미실행은 전체 B2 미완료.

## W4 — 회귀·통합

- **R01 `b2_b1_product_auth_fence_hold_receipts_regression`**: B1의 인증·scope·claim/fence·revocation·receipt/tuple·hold 관련 기존 테스트를 정확한 명령과 기능별 결과로 재실행한다. 해당 테스트 목록은 코드와 계약 대조 후 동결한다.
- **R02 `b2_existing_pack_and_legacy_reads_regression`**: 기존 pack bytes/golden과 이전 receipt 읽기·정상 기존 경로를 검증한다. unsupported legacy는 명세된 범위에서 기록하고 신규 승인처럼 위장하지 않는다.
- **R03 `b2_end_to_end_evidence_and_scope_audit`**: 각 P/E/I 행의 입력·관측·출력과 실제 제품/스크립트/외부 서비스 층위를 독립 확인하고, commit 및 schema/wire diff, 실패·미실시·원격 효과를 기록한다. `docs`만의 주장이나 전체 테스트 개수는 각 행 증거를 대체하지 않는다.

## 원 M9 검증 명령 대응 (계획, 아직 실행 증거 아님)
- `cargo test --locked --offline -p foundry-pack --test plain_pack` → R02/P02; baseline 1 pass만 기실행.
- `cargo test --locked --offline -p foundry-line --test die_executor --test review_host_live --test review_evidence` → R01/R02; 별도 실행 기준선에서 die_executor 12·review_host_live 52·review_evidence 1 통과(각 0 실패). 이는 원 결합 명령 자체의 실행 기록과는 별도이며 fake Die와 실제 Die를 구별한다.
- `cargo test --locked --offline -p foundry-harness-cli` → R01/E04; W0에서 17 통과.
- `cargo test --locked --offline -p foundry-exec -p foundry-workflow -p foundry-orch` → R01/R02; W0 실행 exit 0, 총 268 passed / 0 failed / 0 ignored (doc test 포함).
- `docker compose -f deploy/compose.yaml config` → I01; 배포 파일 미구현·미실행.
- `python3 scripts/check-deploy.py` → I01; 검사기 미구현·미실행.
- `cargo test --locked --offline -p foundry-watch --test t56w_docker_mandatory` → I02/I05/I06의 기존 runtime 기준선; 현재 고정 이름 삭제/pull 부작용을 먼저 해결해야 하며 미실행. 별도 대안 fixture라면 고유 container/network 이름·이미 있는 로컬 이미지와 no-pull, 자기 자원만 정리 및 원 R1~R4/UID/flags의 assertion 보존을 구체적으로 대조한다. 원 명령 대신 대안 사용 여부·허용 범위는 구현·동결 전 명시하며, 대안이 다르거나 행을 생략하면 같은 원 시험 PASS라고 부르지 않는다.

## W0 교차 단계 의미와 열린 선택 — READY FOR DOCUMENTATION COMMIT

W0는 아래 안전·권한 의미, 행 소유와 필수 증거 층위, 단계 순서 및 동결 게이트를 정한다. 여섯 교차 단계 원칙의 현행 결정은 [W0 closure record](b2-w0-progressive-freeze-revision.md)이며 부모 검증과 로컬 커밋 전 W0는 미완료다. 각 단계의 정확한 계약은 결정·시험될 때까지 DRAFT/BLOCKED다. 권고를 일상적 계획 판단에 적용하는 상설 지시가 정확한 schema/wire 또는 허용 목록 예외의 명시적 승인을 뜻하지 않는다. 상세 선택은 적용 단계의 코드 전에 동결한다.

1. **D-PIN:** Owner 범위의 팩 선택 권한을 service 권한 및 결과 승인과 구별하고, 승인·선택과 admission pin을 구별하며 원 snapshot, 권한, revision, claim/fence와 receipt를 결속한다. 선택 주체/registry, CAS·경합 순서, revoke 중 진행 중 stage 처리, 오류 precedence와 정확한 API는 열려 있다.
2. **D-LEGACY:** 원 기록·권한·receipt를 보존하며 read, 정확 retry, accepted continuation, 새 작업을 구분한다. 새 작업에는 현재 권한이 필요하고 이미 수락된 stage 결과의 수집·보존은 B1대로 유지한다. P07 후보와 B1 continuation의 호환, 각 이력 정책, migration·구 writer·복구 방식은 열려 있다.
3. **D-TASK:** 불변의 원 issue snapshot과 명시적인 Owner 범위 task 실행 권한을 service 권한 및 결과 승인과 구별하고 task와 결과/승인을 결속하며 편집으로 기존 작업을 교체하거나 자동 실행하지 않는다. 일반 grammar, bytes/limits, 정규화, 권한 API와 오류 상태는 열려 있다.
4. **D-PRODUCER:** 실제 제품 진입점과 claim/fence를 통과하는 생산자→host→별도 finalizer를 관측한다. W2a의 고정 입력 test-only fake는 일반 task나 실제 Die를 증명하지 않으며 Held/Failed 및 B1의 기존 경로를 약화하지 않는다. assembly, ports/DTO, bridge 권한, 상태 전이와 crash/retry는 열려 있다.
5. **D-ISOLATION:** 실제 Die 또는 코드를 실행하는 inspector 전에 같은 유효 identity·mount·network 경계에서 실측한 격리와 인증된 proxy의 정상/거부 제어가 필요하다. UID, 이미지, launcher, secret/daemon 신뢰, listener·budget, 자식 종료와 안전한 fixture는 열려 있다. W3 I01–I06 전체는 여전히 필수다.
6. **D-TRACE:** 모든 기존 행과 B1/M9 의무를 소유 단계와 실제 제품/fake/runtime/외부 관측 층위에 연결한다. 정확한 파일·명령·단언·증거 위치와 범위는 각 단계 전 동결하고 누락 없이 검토한다. 동결은 실행 PASS가 아니다.

## 단계별 동결 및 출구 게이트 (DRAFT)

모든 단계는 **해당 단계 코드 전에** 정확한 계약, 적용 API/schema/파일 범위, 시험 이름·명령·입력·긍정/부정 단언·증거 위치와 관측 층위를 동결한다. 해당 행의 필수 증거와 회귀를 검토한 뒤 로컬 W 단위 커밋에서 정지·보고한다. 이번 문서 작업은 부모 검증과 로컬 W0 커밋 전까지 완료가 아니다.

| 단계 / 소유 행 | 코드 전 동결 게이트 | 출구 증거 및 이후로 남는 사항 |
| --- | --- | --- |
| W0 / 교차 단계 | 위 여섯 의미, 안전·권한 및 변경 통제, 행별 소유/증거 층위, 순서와 각 단계 게이트를 정하고 행 손실을 검토한다. | W0 문서·범위 차이 검토 후 로컬 커밋·정지. 후속 단계의 정확한 구현 상세는 그 단계 전까지 남는다. 부모 검증과 로컬 커밋 전에는 W0 미완료. |
| W2a / A01 | 고정 입력 bytes와 별도 권한, 실제 entrypoint/assembly, test-only fake, ports/DTO·파일, identity·claim/fence·transaction/receipt, bridge·Passed 전이와 별도 finalizer, B1 Held/Failed 호환·crash/retry, A01의 정확한 시험과 범위 결정을 동결한다. 필요한 예외나 schema/wire 결정이 없으면 구현 BLOCKED. | 실제 제품 seam의 A01 증거만; fake는 실제 Die, Git 객체, Docker, E01 또는 외부 서비스가 아니다. 일반 grammar는 W2 전까지 남는다. |
| W1 / P01, P01b, P02, P03, P03b, P04–P09 | 선택/registry 권한·CAS·pin 원자성, snapshot/resume, legacy 및 revoke/오류 정책, 경합·실제 구 writer fixture, migration/복구와 P행 시험·범위를 동결한다. | 각 P행의 원래 모든 단언과 B1 보존 증거; A01은 P02와 실제 Die를 대체하지 않는다. |
| W2 / E01–E04, E02b | 일반 task grammar/bytes/limits·authority·결과 결속·실패 정책, E행 시험 및 범위를 동결한다. 실제 Die 또는 코드를 실행하는 inspector **실행 전** 동일 유효 경계의 격리·proxy 실측, 정상 제어, 거부 및 자식 제어를 확보한다. | E행 실제 관측 및 필요한 선행 격리 증거; W3 전체 인수로 계산하지 않는다. |
| W3 / I01–I06 | 배포/override 정책, 실제 listener/runtime 시험, 안전한 Docker 자원·로컬 이미지/no-pull 조건, I행 시험과 범위를 동결한다. | I01–I06 전체 실측과 원 Docker 명령/단언의 안전한 처리. 미실행은 NOT-PERFORMED; 대안을 원 시험 PASS로 위장하지 않는다. |
| W4 / R01–R03 | 정확한 B1 회귀 목록과 원 M9 명령 manifest, R행 증거·schema/wire/파일 범위 감사 및 독립 검토 기준을 동결한다. | 모든 필수 행, 원 M9 검증과 회귀의 실제 증거 및 미실시/차이를 기록한다. |

어느 단계든 **schema 코드 전** 정확한 migration/open과 이미 열린 구 writer의 동작 및 fence, quiescence, WAL 포함 backup, crash/불확실 commit, rollback 또는 새 durable write 이후 reconciliation, receipt/accepted-result 보존과 필요한 범위 결정을 동결한다. 새 open 검사만으로 이미 열린 writer를 막았다고 보지 않는다. schema 불필요는 필수 결정이 아닌 미검증 선택지다. 실제 운영 migration과 외부 쓰기는 별도 효과·범위 결정이 필요하다.

M9 제외 파일(특히 제안된 ledger 예외), 실제 schema/wire 또는 기존 frozen 계약 변경은 정확한 범위·필요성·대안·호환성을 제시하고 별도 명시적 변경 통제를 거친다. 파괴적 작업과 실제 외부 효과도 별도 통제한다. ledger 예외는 제안일 뿐 승인되지 않았다. W2a가 그 예외에 의존하는지는 열린 구현 차단 사항이다.

이번 범위에서 허용 목록 밖 편집, migration, 운영 및 원격 효과는 승인되지 않았다. 안전한 migration은 추론하지 않는다.
