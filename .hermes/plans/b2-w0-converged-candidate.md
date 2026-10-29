# B2 W0 수렴 후보 — DRAFT, 미동결

> **HISTORICAL W0 TIMING/STATUS:** This document preserves earlier draft timing and status. For the current W0 decision and precedence, see the [closure record](b2-w0-progressive-freeze-revision.md). Its technical evidence and detailed history remain relevant; exact stage contracts remain blocked until decided and tested.

기준 HEAD `971240703aa004347c674b5c8ff1702435908b4d`. 이 문서는 향후 edit-spec 준비를 위한 여섯 결정의 후보 정본이다. 승인, 계약 동결, migration, W0 완료 또는 B2 PASS가 아니다. 근거는 [실행 계획](2026-10-28_214327-b2-product-execution-plan.md), [결정 초안](b2-w0-decision-and-rubric-draft.md), [인수 매트릭스](b2-acceptance-matrix-draft.md), [M9 B2](../../.specs/m9-plan.md), [B1 scope/hold](../../docs/design/b1-scope-architecture.md)이다. 행 ID는 기존 매트릭스의 행만 가리킨다. 아래 관측은 제안된 기대이지 실행 결과가 아니다. `graphify-out/GRAPH_REPORT.md`는 부모의 선행 확인에서 부재했다.

**기존 사실:** review config의 digest는 ProductStore handle별 설정이고 run에도 digest와 authority snapshot이 저장된다. artifact root는 별도로 내구 pin된다 (`crates/foundry-ledger/src/product.rs:1115`, `:1151`, `product/review.rs:544`, `:627`). 이는 주문/lifecycle별 사전 선택 pin이 이미 있다는 뜻이 아니다. host는 pack snapshot을 검증하고 해당 snapshot으로 그래프를 구성한다 (`crates/foundry-line/src/review_host.rs:926`); 저장 시 manifest 재읽기 및 재검증 경로 전체의 교체 안전성은 미검증이다. issue는 `github-issue` snapshot으로 저장되고 미지원 경계가 있다 (`crates/foundry-watch/src/intake.rs:362`, `crates/foundry-ledger/src/product.rs:4412`). 기존 harness의 task는 dedup key에서 오며 HOME도 재주입한다 (`crates/foundry-harness-cli/src/lib.rs:110`, `:128`). B1은 이미 수락된 stage 결과를 service revoke 후에도 수집할 수 있지만 다음 신규 stage는 거부한다 (`docs/design/b1-scope-architecture.md:228`).

**범위:** 원 M9 허용 목록은 `foundry/src/{main,deploy_profile}.rs`, `foundry-serve/src/lib.rs`, `foundry-line/src/{review_host,die_executor,lib}.rs`, harness-cli, watch의 `model_proxy`/`inventory`, 필요한 설정·결속에 한정된 blueprint/workflow/exec, 배포 파일·검사기·관련 시험·문서다. ledger와 pack 구현, serve의 `auth_http.rs`/`review_hold.rs`, watch의 intake/`intake_worker.rs`는 명시 목록 밖이다. 후보 변경은 선택·pin 및 task 권한의 내구 저장과 트랜잭션, host 라우팅, 입력과 생산 연결에 영향을 줄 수 있고 schema/wire, scope audit, upgrade/rollback 검토가 필요하다. 가능한 대안은 digest별 기존 invariant를 유지하는 host/handle 라우팅, 기존 공개 경로 재사용, 또는 store 계약 변경이다. 각각 원자성·재개·구 writer·의존 방향을 해결하는지 미입증이며 어느 파일 변경도 승인된 것으로 간주하지 않는다. 기존 ledger/receipt 바이트를 재작성하지 않는 additive 원칙을 유지한다.

## D-PIN — BLOCKED

**정책과 이유:** Owner가 source/repository 범위에서 digest를 승인·취소하고 별도의 scoped 권한으로 주문/lifecycle에 승인된 digest를 명시 선택한다. DriveReview는 선택이나 인간 결과 승인을 하지 않고 인가된 선택을 소비한다. 선택의 내구 기록과 최초 review admission에서 확정하는 불변 pin은 분리한다. 후보는 admission 거래에서 현재 source/service 권한, claim/fence, 선택 및 승인 revision을 재검증하고 `(order_id,lifecycle)` pin과 run/work/authority/active를 함께 commit하는 것이다. 같은 lifecycle의 후속 attempt와 hold는 원 pin을 유지하며 새 lifecycle 선택도 기존 active/held/epoch 검사를 우회하지 않는다. 이는 같은 source의 두 주문이 서로 다른 팩을 선택하면서 설정 변경과 ABA로 기존 실행이 바뀌는 것을 막으려는 제안이다. 신뢰된 catalog에서 경로를 resolve하고 검증된 snapshot 바이트를 실행·저장·재개에 결속하거나 재검증해 거부한다.

**없는 표면과 미결:** 현재 단일 host/config digest 검사와 단일 HTTP host 구성은 주문별 선택 surface가 아니다 (`review_host.rs:606`, `crates/foundry/src/main.rs:1395`, `crates/foundry-serve/src/auth_http.rs:15`). scoped 선택 capability/API, registry 및 revision, 정확한 저장 표현과 transaction, repo까지 포함한 host 선택, 오류 precedence는 미확정이다. 선택 변경과 admission의 순서, exact retry의 원 요청·actor와 현재 권한, revoke/regrant 후 복구 및 비실행 hold 결정, 수락 이후 runtime 소실을 B1과 대조해야 한다. 이미 시작된 stage 결과 수집은 반드시 보존한다. 별도 host 라우팅이 무스키마 또는 안전하다는 증거는 없다.

**기존 행:** [P01/P01b/P02/P03/P03b](b2-acceptance-matrix-draft.md)에서 무권한 무효과, revision/ABA, 같은 source 두 주문의 X/Y, 연결·프로세스별 선택 변경과 admission 순서 및 단일 commit을 관측한다. P04/P05/P06/P09는 원 snapshot 재개, 변조·교체, 교차 혼용 거부 및 receipt/retry를 관측한다. 구체 테스트 위치와 assertion은 미확정이다.

## D-LEGACY — BLOCKED

**정책과 이유:** 원 기록을 보존하고 권한 있는 read, 기존 actor·권한과 원 요청에 정확히 결속된 receipt exact retry, accepted continuation, 신규 activation을 따로 판정한다. 단일 digest·완전한 원 authority·검증된 원 snapshot인 경우에만 현재 권한/fence와 registry를 추가 검사하여 원 continuation을 검토하고, 결손·혼합 이력의 continuation과 신규 activation은 임의 backfill 없이 차단하는 보수 후보다. 권한 있는 read와 조건을 충족한 receipt replay까지 일괄 차단하는 뜻은 아니다. legacy lifecycle의 신규 activation 차단은 호환 비용이 있으며 이력 없는 별도 경우의 최초 pin과 구분한다. 이 12개 가상 조합 및 추가 검사 대상 1개 조합은 운영 데이터의 관측이나 자동 허용이 아니다. 기존 B1의 적법한 accepted continuation 및 이미 accepted된 stage 결과 수집을 부당하게 막는지 먼저 대조해야 한다.

**없는 표면과 미결:** 실제 이력 census, 복구 provenance와 혼합 이력 처리, 구 바이너리와 열린 connection을 포함한 writer quiescence/fence, WAL 일관 backup 및 crash/복원 절차가 없다. `product.rs:780`의 version 검사만으로 이미 열린 구 connection을 차단할 수 없다. 선행 SQLite 모형의 pin 일치 guard는 같은 digest의 구 stage 시작을 허용했다는 보고가 있으며 실제 Foundry 시험은 아니다. UDF guard는 미등록 구 writer의 우발적 쓰기 방지 후보일 뿐 임의 DB 접근자를 상대로 한 보안 경계가 아니다. 새 durable write 후 이전 backup 복원은 기록·외부 효과를 잃을 수 있으므로 무손실 rollback이 아닌 복구 및 reconciliation 문제다.

**기존 행:** [P07/P08](b2-acceptance-matrix-draft.md)은 digest/authority/snapshot 조합별 read·receipt·continuation·새 작업, 실제 구 바이너리의 열린 연결과 새 open, 동일 digest stage-start/Resume, pin 부재 및 상이 digest의 구 activation, crash/backup 및 B1 accepted-result 양성 대조를 요구한다. 실제 fixture와 방법은 아직 없다. R01/R02도 B1 및 역사 read 회귀에 대응한다.

## D-TASK — BLOCKED

**정책과 이유:** 최초 admission의 source issue snapshot과 receipt는 불변이다. 편집 delivery는 기존 task를 대체하거나 자동으로 새 작업을 승인하지 않는다 (`crates/foundry-ledger/src/product.rs:4376`). 지원 후보는 좁은 `dep-bump-v1` 구조화 task로, raw intake digest와 별도 canonical task bytes/digest를 source, repository, order, lifecycle 및 원 admission에 결속한다. Owner가 정확한 task digest를 **명시적으로 실행 허가**하고 DriveReview는 그 허가를 소비한다. issue label, 본문, service grant 또는 결과의 인간 승인에서 실행 허가를 추론하지 않는다. 최종 인간 결과 승인은 별개이며 task와 결과의 결속을 검증해야 한다. 지원 밖 입력은 기존 대기 또는 명시적 거부 정책을 확정할 때까지 실행하지 않는다.

**없는 표면과 미결:** raw snapshot에서 task bytes를 만들고 Owner가 허가하는 API, 인가·idempotency·ready 전이, task 전달 및 result/approval 검증과 사용자 관찰면은 미정이다. 기존 raw digest (`product.rs:5757`)나 dedup key가 새 task digest를 자동으로 보장하지 않는다. JSON 정규화·중복 key·문자/길이·OID·repository identity 매핑·오류 precedence, inspector와 lockfile 검증, offline cache 실패를 결정해야 한다. `[dependencies]`만 허용하는 후보와 `[workspace.dependencies]`까지 포함하는 후보는 서로 다르다. 초안의 459-byte 표본과 722-byte 최대치는 특정 미동결 grammar의 산술 후보이며 제품 제한이 아니다; 4096도 임시 후보다. 정확한 grammar와 한도는 확정되지 않았다.

**기존 행:** [E02/E02b](b2-acceptance-matrix-draft.md)는 악성·미지원 입력의 비실행과 최초 A 이후 편집 B의 불변, 미지원 A 후 유효 형태 B의 자동 실행 금지를 관측한다. A01/E01은 명시 허가된 task의 실제 전달과 결과 결속을, E03은 revoke/fence를 관측한다. 유효 예제 및 한 요소 변조 반례의 실제 입력과 검증점은 미확정이다.

## D-PRODUCER — BLOCKED

**정책과 이유:** W2a에서 단일 기존 승인 팩과 명시 허가 task로 실제 제품 진입점부터 admission/ready, claim/fence, producer, S1, host/finalizer까지 연결하는 seam을 먼저 입증한다. 기존 host dispatch는 이미 claim과 결과 commit을 가리키는 intent를 받으므로 최초 Die 생산자로 가정하지 않는다 (`crates/foundry-line/src/review_host.rs:863`). watch tick은 poll/admit/claim을 반환하며 해당 경로에서 host dispatch는 확인되지 않았다 (`crates/foundry-watch/src/intake_worker.rs:68`, `crates/foundry/src/main.rs:2769`). 제한된 정적 검색이지 모든 동적 경로 부재의 증명은 아니다. P2 준비 helper와 합성 S1은 제품 생산 연결의 증거가 아니다 (`crates/foundry-ledger/src/product/result.rs:415`).

**없는 표면과 미결:** Owner task 허가→ready와 claim→최초 producer→내구 S1→intent→host/finalizer→인간 승인에 필요한 실제 port/DTO/조립 파일, transaction 및 crash/retry 경계가 식별되지 않았다. DriveReview와 고정 adapter만으로 task를 승인한다는 독립안은 명시적 Owner 허가 후보와 충돌하여 채택하지 않는다. watch→line 및 ledger→exec의 기존 의존 방향을 역전하지 않는 조립과 원 allowlist 밖 변경 대안을 확인해야 한다. fake producer가 실제 seam을 경유하는 관측과 synthetic fixture를 구분해야 한다.

**기존 행:** [A01](b2-acceptance-matrix-draft.md)은 실제 entrypoint와 각 결속·producer/host/finalizer를 관측한다. 테스트 executor 또는 격리된 대체 producer를 사용하면 그 사실을 증거에 명시하며 실제 Die·E01·격리 PASS로 간주하지 않는다. P02는 그 seam 위 두 팩을 관측하고 E01은 실제 Die, commit/tuple 및 인간 결과 승인을 별도로 관측한다. A01만으로 E01이나 격리 PASS가 아니다.

## D-ISOLATION — BLOCKED

**정책과 이유:** orchestration은 M9대로 serve에 남기되 비신뢰 task를 실제 Die에 전달하기 전에 주문 실행별 격리와 인증된 run/provider/budget 결속 proxy의 정상·부정 경로를 실측한다. 후보는 non-root, capability 제거, no-new-privileges, read-only rootfs, 자기 workspace만 쓰기, DB/WAL·HOME·credential·Docker socket 및 타 주문/watch 산출물 비노출, 권한 API와 직접 egress 차단이다. host kernel·관리자·daemon·신뢰 이미지 관리자는 신뢰 기반 후보이다. 같은 UID나 env 정리만으로 파일 격리를 증명할 수 없다 (`docs/design/b1-scope-architecture.md:184`, `harness-cli/src/lib.rs:128`). proxy screening 코드가 실제 listener나 egress 차단을 증명하지 않는다 (`crates/foundry-watch/src/model_proxy.rs:62`).

**없는 표면과 미결:** 실제 실행 구성, 이미지 출처, UID/mount/network, proxy listener/인증·budget 및 DNS/redirect/IPv4/IPv6 범위, 종료 후 자식 통제와 재현 가능한 fixture가 미확정이다. 정상 workspace/proxy 및 접근 가능한 대상의 양성 대조와 거부 시 host 상태 불변을 함께 관측해야 한다. 기존 Docker 시험은 고정 이름 자원 삭제 및 pull/tag를 시도한다 (`crates/foundry-watch/tests/t56w_docker_mandatory.rs:60`); 고유 자원·local image/no-pull 대안과 원 assertion의 동등성은 미결이다. W2 선행 관측과 W3 배포 인수는 별개다. 코드를 실행할 수 있는 inspector/check 역시 같은 수준 이상의 격리를 실제 Die 전에 실측해야 한다. 기존 I02–I06에서 E01의 선행조건으로 필요한 구체 assertion·증거 subset은 아직 미확정이며 확정 전까지 E01 진입은 BLOCKED다; 선행 subset 성공만으로 W3 PASS를 주장하지 않는다.

**기존 행:** [I01–I06](b2-acceptance-matrix-draft.md)은 compose/override, 실제 DB·비밀·socket 및 타 산출물/API 거부, proxy 성공, 직접 egress/budget 거부, runtime 속성과 자식 종료를 각각 관측한다. E01의 실제 Die에는 선행 격리 증거가 필요하다. Docker 또는 listener 미실행은 NOT-PERFORMED이며 B2 PASS가 아니다.

## D-TRACE — BLOCKED

**정책과 이유:** 모든 기존 A/P/E/I/R 행을 실제 제안 테스트나 관측에 연결하고, 입력·정상/부정 assertion·관측 계층·파일/함수·명령·증거 위치·B1/M9 대응 및 변경 범위를 동결한 뒤 좁은 독립 검토를 받는다. PASS는 요구 층위의 실행 증거에만 적용한다. 계획상의 테스트명은 예정 이름이지 존재하거나 실행된 명령이 아니다.

**현재 연결과 결손:** A01은 제품 seam, P01–P09는 선택·pin·legacy·receipt, E01–E04 및 E02b는 생산·입력·권한·설정, I01–I06은 배포/실행 격리, R01–R03은 B1·pack·통합 증거에 대응한다. 이는 행 수준의 목적 연결일 뿐 정확한 실제 테스트 파일/함수, 명령, 모든 assertion, evidence 위치와 실패 precedence의 대응은 미완성이다. 원 M9의 pack/line/harness/exec-workflow-orch 명령은 해당 기존 회귀의 후보이고 compose/deploy 검사 및 Docker 명령은 I행 후보이나, 기존 crate 테스트나 compose render만으로 제품 경로·runtime 행을 대체할 수 없다. 원 Docker 시험의 안전 대안은 원 시험 실행으로 표시하지 않는다. 이전 초안에 기록된 baseline 및 모형 결과는 그 자체의 층위에서만 참고하며 이번 작업의 실행 결과가 아니다.

**기존 행:** [A01, P01–P09, E01–E04, I01–I06, R01–R03](b2-acceptance-matrix-draft.md) 전체가 대상이며 P01b, P03b, E02b도 포함한다. 각 행의 실제 증거 연결과 승인된 파일/schema/wire 범위가 아직 없어 READY가 아니다. fake/scripted producer, 실제 ProductStore, 실제 Die, Docker runtime 및 실제 외부 서비스는 각각 별도로 표기한다. 외부 GitHub나 provider 연결은 별도 증거 없이는 주장하지 않는다.

## W2a/A01 및 E01 선행 격리 — 독립 구체화 후보 (DRAFT)

출처: 독립 검토 `deleg_45cb02b8` task-1/task-2. 아래는 설계 후보이며 실행 가능성, 허용 파일 예외, schema/wire, 상세 API 및 복구 동작은 아직 승인·검증되지 않았다. 기존 행을 늘리지 않는다.

- **A01 고정 입력 후보:** 원 `github-issue` snapshot과 raw digest는 그대로 보존하고 별도 고정 task kind `a01-fixed-v1`, UTF-8 bytes `foundry-a01-fixed-task-v1\n`를 검토한다. domain `foundry-product-task-v1\0` 뒤 kind와 task 각각의 u64 big-endian 길이와 bytes를 연결해 SHA-256한 후보 벡터는 26 bytes, `374c781c25a77f6bae15e76265cbcd60fb396c0188248234fc8f0b125ab98e47`이다. 부모가 별도로 계산했지만 계약은 미동결이다. issue 본문을 명령으로 해석하지 않으며 일반 dep-bump grammar를 대체하지 않는다.
- **A01 연결 후보:** Owner의 명시적 task 허가와 `waiting_unsupported→ready`를 한 transaction으로 묶고, 기존 service claim/fence를 재검증한다. 외부 효과가 없는 test fake producer의 출력과 S1 전이·receipt를 원자적으로 결속하는 방안을 검토한다. 단 `InspectionPassed`의 전후 순서와 host 결과를 waiting으로 옮기는 bridge는 기존 B1 및 초기 설계와의 정합성이 확인되지 않아 BLOCKED다. host와 별도 finalizer를 거쳐 task/result 결속을 확인하는 흐름도 해당 선결조건을 해결해야 한다. fake의 사용과 테스트 전용 조립을 증거에 명시하며 운영 기본 producer로 넣지 않는다. fake의 commit 문자열은 정적 조사상 fake StageExecutor와 기존 host/finalizer의 문자열 검증에는 사용할 수 있으나 Git object 존재가 필요한 실제 Die 증거는 아니다 (`review_host.rs:960`, `product/result.rs:825`, `die_executor.rs:204`). 테스트 진입점이 제품 조립을 실제로 거치는지와 crash/retry가 B1을 보존하는지는 미검증이다. 현재 S1 `DieSucceeded`는 attempt만 담으므로 출력 commit 결속을 별도 durable receipt로 같은 transaction에 저장하는 설계는 제안이지 기존 기능이 아니다 (`event_json.rs:45`, `product/result.rs:461`). host와 finalizer는 별도 transaction이고 finalizer는 현재 service 권한을 재검증한다. DriveReview나 adapter가 Owner 허가를 대체하지 않는다.
- **변경 범위 후보:** ledger의 task 권한·ready·S1·result 결속과 `foundry/src/main.rs`, line 조립 및 관련 시험. ledger와 schema 변경은 M9 명시 목록 밖이며 필요성·대안·구 writer 및 migration 영향을 검토하기 전 승인된 것으로 보지 않는다. 제안된 3개 테이블/API와 watch 파일 무변경 가능성은 구현·실증 전 미확정이다. 일반 task grammar, 실제 Die, 다중 팩, E01 및 격리는 A01 통과로 입증되지 않는다.
- **E01 전제 후보:** 기존 I02–I06의 실제 DB/비밀/socket, 타 주문·권한 API 차단 및 자기 workspace 정상 작동, 인증·예산 proxy 정상/거부, 직접 egress 거부, runtime 신원/mount/capability·timeout 자식 통제를 실제 Die 및 코드를 실행하는 inspector와 동일한 실행 경계에서 관측한다. 각 부정 시도는 임시 DB/WAL·HOME·가짜 비밀·socket·타 주문/watch 산출물·권한 API 및 egress 대상이 실제 접근 가능함을 신뢰된 측에서 먼저 증명하는 양성 대조와 부작용 불변을 동반한다. 동일성은 image/binary, launcher, UID/GID, capabilities, 보안 정책, mounts/env 및 network의 실효 경계로 결속한다. timeout 자식 중단 검사는 동일 실효 경계를 가진 별도 희생 fixture도 후보이며 실제 종료 이후 늦은 쓰기가 발생하지 않는지 관측한다. 사용 가능한 주소군의 DNS/IP/redirect를 시험하고 IPv6 미지원은 근거와 미검증 범위를 남긴다. 신뢰된 probe가 실제 Die와 동일 컨테이너·namespace를 공유한 뒤 이어 실행하는 방식은 한 후보이지 유일한 허용 방식으로 동결되지 않았다. 상세 assertion·동일성·네트워크 지원·fixture·명령은 미결이다.
- 실제 listener를 경유한 테스트 upstream 사용은 외부 provider 연결이 아니며, 단일 실행 경계의 선행 관측은 Compose/override를 포함한 W3 전체 인수가 아니다. Docker의 안전한 고유 리소스/no-pull 실행 조건과 부정 관측이 마련될 때까지 실제 비신뢰 task의 Die/코드 실행 검사 전달은 BLOCKED다.

## 공통 미결 및 진행 경계

1. Owner의 팩 승인·선택과 task digest 허가의 scoped 권한, revision, idempotency 및 취소/재승인 정책을 확정한다.
2. 선택 변경과 admission의 transaction, host/repository binding, 오류 precedence와 B1 receipt·accepted-result 충돌을 대조한다.
3. legacy 조합별 접근·continuation·신규 작업과 복구 provenance를 B1에 대조한다.
4. 실제 구 writer quiescence/fence, WAL backup, crash와 새 write 이후 복구를 검증 가능한 절차로 정한다.
5. task grammar, raw/task bytes 생성과 identity mapping, 크기·검사·실패 정책 및 유효/변조 예제를 정한다.
6. 실제 producer 조립과 권한→ready→claim→S1→결과의 port, transaction 및 retry 경계를 식별한다.
7. 실제 Die 이전 격리와 proxy의 신뢰·네트워크 범위, 안전한 Docker fixture 및 W3 관측을 확정한다.
8. 원 allowlist 밖의 최소 변경 후보별 영향·대안·schema/wire·호환을 검토하고 별도 범위 결정을 받는다.
9. 기존 모든 행을 실제 테스트/관측·assertion·명령·증거와 연결하고 독립 검토한다.

순서는 **W0 → W2a → W1 → W2 → W3 → W4 후보**다. 각 W 수준 완료 커밋 지점에서 사용자 지시에 따라 정지한다. W2a의 fake 또는 대체 producer는 실제 Die가 아니며, W2의 선행 격리 증거는 W3 배포 인수가 아니다. 이번 작업에서는 테스트를 실행하지 않았다. 구현, 기존 계획 수정, migration, 외부 효과, 커밋 및 push는 수행하지 않았다.
