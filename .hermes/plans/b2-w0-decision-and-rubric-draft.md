# B2 W0 — 설계 선택과 인수 루브릭 초안 (DRAFT, 미동결)

> **HISTORICAL W0 TIMING/STATUS:** This document preserves earlier draft timing and status. For the current W0 decision and precedence, see the [closure record](b2-w0-progressive-freeze-revision.md). Its technical evidence and detailed history remain relevant; exact stage contracts remain blocked until decided and tested.

기준 `971240703aa004347c674b5c8ff1702435908b4d`; 근거 `.specs/m9-plan.md:361-407`, `.hermes/plans/2026-10-28_214327-b2-product-execution-plan.md` v1. 정적 소스 대조이며 신규 제품 실행·테스트 결과는 아님. 외부 repo/운영 DB/실 자격증명 변경 금지. `graphify-out/GRAPH_REPORT.md` 부재로 그래프 검토 불가. 기존 B1 P1/P2·W7 별도 수동 게시/리컨실러 미실측 구분을 유지한다.

## 출처와 관측
- `crates/foundry-line/src/review_host.rs:606-703`: host pack path 단일, 생성 시 `REVIEW_PACK_DIGEST` 확인. `:926-940`의 load snapshot 검사 후 `GraphEngine`이 그 snapshot을 받는다. `:1638-1653`은 저장된 pack snapshot 및 continuation binding과 host digest 대조.
- `crates/foundry-ledger/src/product.rs:376-382,416-422,1115-1148`: ProductStore의 review config는 handle 메모리 내 하나의 pack digest; artifact root는 별도 내구 pin(`:1151-1199`). `product/review.rs:544-553,573-593,627-675`는 실행 수락 시 digest와 claim/source 검사 후 run에 digest와 authority snapshot을 같은 transaction에서 기록. `:710-727`은 후속 stage에 config digest를 적용한다. 즉 '기존 저장 run에는 digest가 없다'거나 '전역 digest가 DB에 영구 저장된다'는 주장은 거짓이다.
- `foundry-watch/src/intake.rs:373-402`는 issue 본문을 `github-issue` payload로 저장, 임의 명령으로 취급하지 않는다. `foundry-ledger/src/product.rs:4412,4576-4620`은 waiting_unsupported 경계를 가진다. `foundry-harness-cli/src/lib.rs:31-42,110-117`은 InputManifest.task에 dedup_key를 사용한다. `:128-146`은 환경을 clear하지만 HOME을 상속한다. allowlist만으로 동일 UID filesystem/DB 비접근을 보장하지 않는다.
- `foundry-watch/src/model_proxy.rs:62-113`의 구조는 요청 검증/스크리닝이다. `.specs/m9-plan.md:280-285`는 운영 transport/listener 연결 미확인을 지적한다. library 구조만으로 실제 컨테이너 egress 차단·budget enforcement를 주장할 수 없다.
- 로컬 Docker 29.4.3/Compose 5.1.3 daemon 동작 및 `t44b-cast:1`, `t56w-cast:1`, `busybox:1.37` 이미지 목록 확인(2026-10-28); runtime isolation·이미지의 원하는 내용은 아직 미검증.

## 의사결정 제안 (전부 DRAFT)

### D1 — 팩 선택과 pin
A **권고**: 신뢰된 등록 목록에서 승인된 정확한 digest만 선택하고, 실제 product review activation/admission 및 현재 service authority/claim 검증을 동일 transaction에 묶어 기존 run record에 pin한다. 동일 lifecycle/attempt의 경합 선택은 하나만 수락하며 이미 pin된 run을 다른 digest로 바꾸지 않는다. 재개는 기존 run/snapshot digest로 복원, 현재 설정의 새 digest나 신규 source grant로 소급 승인하지 않는다. immutable PackSnapshot에서 검증한 바이트를 실행과 저장에 연결하고 저장된 snapshot을 resume 시 독립 검증한다. 지금 코드가 보호하는 authority/fence·receipt를 변경 없이 유지하는 방식과 신규 DB 표현/호환 여부는 실제 구현 조사를 거쳐 결정한다.
B: 모든 주문에 프로세스 전역 팩만 유지 — 변경은 작지만 동시 서로 다른 팩 목표 미달. C: 요청이 임의 로컬 pack 경로를 제공 — 신뢰 경계와 변조 위험으로 기각. **영향:** 저장 identity/권한과 API/스키마에 영향 가능. 기존 run의 digest는 기록돼 있으나 historical 선택 권한·snapshot availability는 미입증; 없는 증거는 추정 backfill하지 않고 읽기/재개/새 작업을 별도로 결정한다. 빈 값·변조·missing snapshot은 실제 원인 구별 및 fail-closed. 대규모 data migration/기존 digest 해시 변경은 이 안의 승인이 아니다.

### D2 — 업무 입력
A **권고**: 지원 작업에 대한 명시적 구조화 grammar·provider/source mapping·권한 부여를 별도 계약으로 동결하기 전 기존 `github-issue`는 `waiting_unsupported`를 유지하고 임의 본문을 셸 실행하지 않는다. 테스트 adapter는 production 경로와 분리하고 단독으로 제품 지원이라고 주장하지 않는다. B: issue 본문을 자유 명령으로 전달 — 권한·재현성 위험으로 기각. 본체 추출, schema, invalid input/error, task-vs-dedup 구분, 인간 승인과 task identity 결속을 W2 구현 전 구체화한다. 이 권고만으로 임의 issue→code 자동화가 완성되는 것은 아니다.

### D3 — 실행과 격리
A **권고**: 기존 M9 결정대로 초기 orchestration은 serve에 유지; Die 자체는 권한/마운트/환경/네트워크가 분리된 경계에서만 실행하도록 설계하고 실측한다. 독립 worker 서비스 전환은 별도 후속 범위다. 최소 정책 후보: Die에 product DB·호스트 HOME·publisher credential·Docker socket 미노출, 권한 없는 직접 serve API/타 산출물 접근 거부, 전용 작업공간과 명시적 ingress/egress, 신뢰하는 이미지·daemon·관리자 명시. proxy 경유 정상 요청과 직접 인터넷 우회 거부를 각각 실측한다. **주의:** '다른 UID', container 명세 또는 `env_clear` 하나만으로 이를 증명할 수 없다. root/daemon 관리자와 호스트 커널은 신뢰 기반인지 W0에서 명시한다. 대안 B 같은 UID subprocess는 B1 구조와 호환은 쉽지만 운영 DB 노출 가능성이 있어 강한 격리 인수 미달; C 전면 별도 서비스는 경계가 더 명료하지만 기존 배치 외 범위·운영 복잡도 증가. 현재 daemon/image 가용성은 관련 시험 통과가 아니다.

### D4 — 변경 권한과 동결
원 B2 목록(`m9-plan.md:367-378`)에 `foundry-ledger` 및 `foundry-pack` 구현 변경 없음. 그 파일의 변경이 필요하다고 가정하지 말고 API/데이터 경로와 대안을 비교해 파일·심볼·schema/wire·upgrade/rollback·테스트 근거를 적어 범위 및 계약 변경 승인을 받는다. 관련 구현 조사·설계·인수 초안 작성은 가능. 상세 권한/식별자·외부 repo 쓰기 승인 없이 수정/게시하지 않는다.

## 인수 루브릭 초안 — 각 행 독립 (DRAFT, 결과 전부 UNTESTED)
각 행은 입력/행동/정확한 상태 및 검증 위치, positive + negative, 테스트 함수명, evidence 경로, 적용 호환 모드, 성공·오류 precedence를 W0 완료 전에 구체화한다. 전체 PASS는 전 필수 행 PASS 및 동결된 환경/회귀 충족 시만 가능. 스킵·NOT-PERFORMED는 PASS가 아니다.
- P01 등록 권한 없는 선택 거부(데이터/권한/효과 불변), P02 동일 바이너리에서 두 주문/stream의 서로 다른 승인 팩 실행, P03 경합 시 원자적 단일 pin 및 다른 digest 거부.
- P04 재기동·hold/resume에서 원래 snapshot/digest·authority 유지, P05 snapshot 변조·부재·검증 뒤 교체 시 거부, P06 타 order/session/tuple 혼용 거부, P07 구 DB/과거 receipt 재개 및 불충분 근거의 별도 정책.
- E01 실 product entrypoint·claim/fence·명시 입력→실 Die→진짜 host/result→승인 digest 결속, E02 미지원·악성 입력 비실행 및 durable 대기, E03 grant/source revoke·claim 교체 ABA와 중단/timeout 후 오래된 결과 거부, E04 pack/Die/repo 누락·잘못된 timeout의 진단·복구.
- I01 렌더링된 compose와 override 검사, I02 실행 중 DB·credential/HOME·Docker socket 접근 거부, I03 타 order 산출물 및 serve 권한 API 접근 거부, I04 proxy 정상 호출, I05 직접 egress 차단 및 budget 초과 거부, I06 실제 runtime UID/mount/caps/read-only 등 정책 확인. daemon/image가 없어 I행이 미실행이면 B2 미완료.
- R01 기존 B1 auth/claim/fence/revocation/tuple/hold/privacy 및 P1 불변, R02 기존 pack fixture/golden 및 정상 backward reads 보존, R03 재시작/프로세스 경계 경합·회귀/누설·실제 대상/가짜 fixture 구분. 원 M9 검사 명령은 계획 v1 W4에 기록; 정확한 전체 테스트 명칭과 baseline은 미확정.

## 독립 조사와 부모 대조 (astra, 2026-10-28)
독립 조사 `b2-w0-astra-survey.log`, exit 0, 지정 파일 정적 분석. 부모는 `product/review.rs:2303-2373,1725-1762,1184-1218`와 `review_host.rs:2485-2521`을 대조했다.
- run 단위 digest는 이미 DB에 저장되고 admission/source/claim 및 active fence에 묶인다. 이것은 **주문/lifecycle/stream의 사전 팩 선택 pin과 동일하지 않다**. accepted-product-hold 경로는 살아 있으며, 일반 `begin_hold_resume` 미지원과 구분한다. 기존 accepted hold의 저장 run digest는 설정 digest와 달라지면 거부된다.
- pack snapshot의 선언 파일은 메모리 바이트로 저장하나 manifest는 원 경로에서 재읽는다. 복사본을 다시 load하여 digest를 대조한다. 같은 snapshot으로 실행 그래프를 만들며, 후속 조작 가능성은 미검증 가설이다. 기존의 실제 확인 장치를 제거하거나 위험을 확정된 취약점이라 부르지 않는다.
- **D1 구현 대안 추가**: A1 digest별 host/backend를 신뢰된 registry에서 선택하고 각 handle이 기존 `pack_digest()` invariant를 지키는 라우팅. 장점: 기존 검사 재사용 후보; 한계: 선택 pin의 원자성·hold/restart 라우팅·공통 root/프로세스 경합은 별도 증명 필요. A2 store API/검사를 run binding으로 변경. 장점: run별 단일 저장소 일관성 후보; 한계: ledger 공개 포트·다수 fence/continuation 호출 변경과 역사 의미에 영향. A1을 저변경 후보로 우선 조사하되 어느 쪽도 안전·무스키마 변경이라고 확정하지 않는다. 팩 선택이 admission 전에 내구로 필요한지에 따라 별도 레코드가 필요한지 결정한다. manifest snapshot API 확장 대신 기존 재검증 유지가 대안이나 동일 바이트 보장과 경로 교체 반례 검증은 여전히 필수다.

## A1 타당성 조사: 조건부이며 라우팅만으로는 불가
astra `b2-w0-routing-audit.log`(exit 0), 부모 재대조 `review_host.rs:648-703`, `main.rs:1395-1461`, `auth_http.rs:15-38`, `review_hold.rs:242-313`, `.specs/m9-plan.md:367-378`.
- 두 번째 팩은 product 생성자와 공통 생성자의 고정 digest 확인에서 거부된다. 기존 standalone 정책을 보존하면서 product 명시 허용/승인 digest 검증을 추가해야 한다. 별도 host/handle이 공유 DB를 쓸 가능성은 정적 근거만 있으며 동시성과 안전성은 시험하지 않았다. DB에 pin된 artifact root는 singleton이므로 팩별 서로 다른 artifact root라는 가정은 불가하다.
- 제품 HTTP는 현재 host 단일 mutex이며 CLI도 전역 환경으로 host 하나를 만든다. 정확한 host 구성은 **pack digest뿐 아니라 repository identity/executor 경계에도 결속**해야 한다. 같은 digest라도 repo가 다르면 동일 executor를 임의 공유하지 않는다.
- `review_hold.rs:253-276`의 신규 결정 전 runtime 확인과 `:296-313`의 이미 수락된 receipt 반환/재시도 보장은 필수다. 선택을 위해 요청의 임의 digest를 신뢰하거나 scope 검사 전 타 주문의 digest를 노출하지 않는다. 권한 검증된 hold→저장된 run digest 라우팅과 수락 이후 재검증/TOCTOU 처리 방법은 설계 대상이다. 기존 wire DTO에 digest를 추가하는 방법만이 유일한 해법이라는 주장은 보류한다.
- 원 M9 허용 파일은 `foundry-serve/src/lib.rs`만 명시하며, 이 후보 경로의 `auth_http.rs`, `review_hold.rs`는 원 목록 밖이다. ledger typed lookup/API 변경은 필요 가능성이 있으나 유일한 방법이라고 단정하지 않는다. 안전한 대안별 구체 변경면과 승인 필요성을 확정한다.
- 선택을 **review admission 시** 확정하면 기존 run 행 digest를 사용 가능할 수 있으나, 주문 접수/인간 선택 순간부터 재시작을 견뎌야 하거나 lifecycle/재시도 전반에 유지해야 한다면 그 범위를 묶는 추가 내구 선택 계약이 필요하다. 실제 정책이 확정되기 전 양자를 같은 뜻으로 취급하지 않는다.

## W1 선택 정책 후보와 충돌 사례 (DRAFT, 동결 아님)
**권고 정책:** 최초 review admission 시 `(order_id, lifecycle)`별 팩 digest를 같은 트랜잭션에 내구 결속하고, 그 lifecycle의 새 attempt와 hold continuation도 같은 digest를 요구한다. 주문 접수와 최초 review admission 사이의 선택은 내구 확정된 것으로 약속하지 않는다. 명시적 사전 선택 UX가 필요하면 별도 계약으로 확장한다. 이 정책은 run 단위 pin보다 강하고 접수 시점 pin보다 변경 범위가 작다. `product_orders_v1`은 현재 lifecycle·repository identity를 보유하고, work binding/claim은 이미 `(order_id,lifecycle)`을 사용한다(`product.rs:859,881-883`). 추가 저장 구조·인덱스·마이그레이션 버전은 아직 설계/검토 전이다.

예: 주문 A/lifecycle 1이 팩 X로 admission되면 A의 다음 attempt와 hold 재개는 X다. 주문 B/lifecycle 1은 같은 프로세스에서 팩 Y를 사용할 수 있다. A의 동시에 다른 X/Y 선택이 경합하면 한 트랜잭션만 결속할 수 있어야 하며, 패자와 오류 유형·idempotent 재시도는 별도 명세가 필요하다. A의 lifecycle 2는 신규 선택 가능을 후보로 둔다. repository가 다르면 팩이 같아도 실행 대상을 독립 결속해야 한다. 어느 예시도 아직 제품 동작의 증거가 아니다.

**기존 데이터 후보 정책:** 기존 run과 hold는 각 기록의 원래 digest로 읽고 재개하며, 결속이 누락된 기존 lifecycle의 신규 실행은 역사 자료가 모순되지 않는지 검사하기 전 임의 backfill하지 않는다. 다수 다른 digest가 이미 존재하거나 선택 근거가 부족한 경우 기존 읽기/수락된 receipt 재조회와 새 실행/재개를 각각 분리한 구체 정책이 필요하다. 구 DB 이동·rollback 및 동시 open 프로세스 호환은 미검증이다.

**W1 검증 후보 (아직 테스트 코드/동결된 행 아님):** (1) 같은 product entrypoint, 두 승인 팩·두 order의 digest와 실제 사용 snapshot 대조, (2) X/Y 첫 결속 경합을 별도 연결 및 별도 프로세스 양쪽에서 시험하고 승자 1·패자 무부작용 단언, (3) restart 후 configuration 순서 변경에도 동일 lifecycle pin 및 accepted hold continuation 유지, (4) 타 repository·타 order·미승인 digest·missing/변조 snapshot 거부, (5) 현재 B1 단일 팩/legacy fixture의 정상 read 및 receipt replay 보존, (6) lifecycle rollover와 ambiguous legacy를 별도 케이스로 시험. 인수 행은 각 테스트명·명령·정확한 상태·관찰 계층·오류 precedence가 확정되어야만 동결할 수 있다.

## 독립 pin 설계 조사 및 대조 (astra, exit 0; DRAFT)
정적 조사 `b2-w0-pin-design-audit.log:2760-2806`; 부모 대조 `product/review.rs:544-675`, `product.rs:780-804,859,881-896` 및 `product_scope.rs:389-429`. 구현이나 신규 데이터 검사 결과가 아니다.

**트랜잭션 후보:** 새 내구 결속에 `(order_id,lifecycle)` 유일 키, 선택 digest 및 최초 admission/run 근거를 둔다. 기존 `admit_review_activation`의 `BEGIN IMMEDIATE` 안에서 현재 lifecycle/source/claim/service 검사와 기존 activation/epoch/active-run 검사를 보존하고, 결속 확인/생성과 run·authority·active 기록을 하나로 commit한다. 승인 registry가 변경 가능하다면 그 승인 상태와 revision의 트랜잭션적 결속이 별도 필요하다. 이는 현재 코드가 이미 이렇게 한다는 의미가 아니다.
- 결속 없음·해당 lifecycle의 과거 run 없음 → 최초 생성; 동일 digest → 기존 중복/active 규칙을 계속 적용; 상이 digest → pin conflict로 전체 rollback; 과거 run이 있으나 결속 없음 → 별도 legacy 정책; SQLite busy → conflict와 구별할 운영 오류. 무엇보다 **commit한 admission만 pin 확정**이며 서로 다른 연결/프로세스의 경합과 commit 전·후 중단을 시험해야 한다.
- 기존 신규 hold 결정 전 runtime 가용성 검사, 허가된 receipt의 재조회/권한 정책 및 수락 이후 실패시 receipt 반환을 보존한다. host 선택의 권한·digest 확인 후 실제 claim 거래 안에서 재검증하여 TOCTOU를 닫아야 하며, pin은 현재 인간·service grant와 claim/fence 확인을 대체하지 않는다.
- 기존 혼합 digest 이력은 존재가 확인된 사실이 아니다. 존재할 경우 최초/최신을 임의로 대표값으로 삼지 않는다. legacy 읽기·receipt 조회, 원래 snapshot/권한이 있는 기존 재개, 새 작업 각각의 동작을 확정해야 한다. 신규 lifecycle도 기존 order 단위 active/epoch 및 held run 검사를 우회하지 않는다.
- `open_with_clock`는 기존 DB의 schema version 3을 검사하고 기존 DB 경로에서 별도의 publication schema만 생성한다(`product.rs:780-804`). 단순 새 테이블 추가만으로 기존 실행 바이너리의 pin 우회를 차단할 수 없으며 migration 시 새·구 writer 공존과 rollback 방안을 설계·실측해야 한다. 현재 scope audit의 테이블 목록(`product_scope.rs:389-429`)도 추가 pin에 대한 노출/필터 정책을 검토해야 한다. version 변경, 강제 구 writer 차단 또는 특정 rollback 수단은 아직 결정되지 않았다.
- A1은 고정 digest 검사와 host 선택 변경을 필요로 한다. `product.rs`, `product/review.rs`, `auth_http.rs`, `review_hold.rs`는 원 B2 허용 목록 밖이며, 포트·DTO·검증기·scope audit 등은 영향 조사가 더 필요하다. 이것이 유일한 안전한 파일 구성이라고 단정하지 않는다. 기존 run 이력 직접 검사 대안도 legacy 구분과 구 writer 차단을 해결해야 한다.

### 구 writer 차단의 실현 조건 (후보, 미동결)
기존 v3 opener는 schema version 3을 확인하고 최초 open 후 `ProductStore`가 connection을 보유한다(`product.rs:780-815`). 따라서 **version 변경만으로 이미 열려 있는 구 connection까지 차단할 수 있다고 주장할 수 없다**. 후보 A는 유지보수 정지 후 모든 writer 종료/lease 해제 확인 → SQLite `BEGIN IMMEDIATE`로 migration → 새 버전 open → 구 버전 재접속 거부다. 단 정지 절차가 실제 사용 가능한 모든 writer를 포함한다는 운영 신뢰가 필요하며, 중단·복구 및 이미 열렸던 구 connection의 재쓰기 차단을 독립 프로세스 시험으로 증명해야 한다. 후보 B는 DB 내부 가드까지 추가해 구 connection의 우회 write를 막는 방식이나 기존 합법적 write·migration·receipt 경로와 충돌하지 않는지 검토가 필요하다. 어떤 후보도 현재 구현·실측된 기능은 아니다.

로컬 SQLite 모형 프로브 `/home/cosmo/.hermes/cache/scratch/b2-w0-sqlite-old-writer-probe.py` (exit 0): 구 connection이 v3 조회 후 새 connection이 version=4로 갱신해도 구 connection의 무결속 insert가 **1건 성공**했다. 별도의 일치 pin 요구 trigger 추가 후 무결속 insert는 `pin required`로 거부되었고, 새 connection의 pin+run 단일 트랜잭션은 1건 commit됐다. 이는 SQLite 모형의 사실일 뿐 실제 Foundry 구 바이너리·실제 schema·모든 쓰기 경로·malicious DB 접근 방어를 검증한 것이 아니다. 버전 상승만으로 열린 구 writer를 차단하는 주장은 이 모형에서도 성립하지 않는다.

rollback은 migration 이전 백업 복원과 migration 이후 새 pin/run의 보존 사이 충돌이 있다. 예를 들어 X pin 이후 이전 백업만 복원하면 그 이후 승인·결과를 잃거나 외부 효과와 불일치할 수 있으므로 무손실 rollback으로 부르지 않는다. 신버전 기록 이후의 안전한 읽기·복구·old writer 재활성 조건과 외부 효과 reconciliation은 별도 설계·시험 전 미정이다.

## 생산자 경로의 추가 선행 조사
정적 검색에서 `try_dispatch_product_work(`와 `try_dispatch_and_finalize_product_work(`의 직접 호출은 host 내부 및 테스트에서만 확인됐다(`review_host.rs:863-895`; `crates/`의 해당 심볼 호출 검색). `product_github_tick_with_clock`는 service claim 결과를 `IntakeDrainReport.work_claim`에 담아 반환한다(`foundry-watch/src/intake_worker.rs:68-96`); issue payload는 `github-issue` unsupported 상태로 취급한다(`intake.rs:373-378`). 이는 **직접 호출 검색에 한정된 관측**이며 전체 동적 호출 부재 또는 제품 결함을 확정하지 않는다.

W1 P02의 '제품 entrypoint로 두 팩 실행'이 실제 사용 가능한 경로를 요구한다면, selector→claim→intent→host dispatch→result에 필요한 production 연결을 W1 범위에 포함하거나 W1의 라이브 인수를 W2에 의존하는 것으로 정직하게 재배치해야 한다. fixture가 host를 직접 부르는 시험만으로 제품 entrypoint 연결을 대체하지 않는다. 기존 M9 파일 허용 목록과 충돌할 수 있으므로 실제 watch/serve 호출 그래프 및 필요한 파일은 확정 전 추가 확인한다.

## 승인·legacy·최소 입력 후보 (독립 조사, DRAFT)
`b2-w0-authority-legacy-input-audit.log:2900-2944`, exit 0. 아래는 정적 조사에 기반한 **제안**이며 기존 제품의 기능·취약점 또는 안전성 실측을 뜻하지 않는다. 부모는 `product_service.rs:11-18`, `main.rs:2769-2795`, `product.rs:4409-4413`, `docs/design/b1-product-contract.md:137-160`을 대조했다.

**Registry 후보:** owner가 source/repository 범위별 정확한 digest 및 단조 revision의 등록·변경·취소 권한을 갖고, worker `DriveReview`는 이를 변경하거나 인간 승인하지 못한다. 선택은 승인된 registry의 범위와 현재 service grant/claim을 admission transaction에서 검증하고 그 revision과 digest를 고정하는 후보이다. owner가 source 기본값만 두는 최소안은 **같은 source의 서로 다른 두 주문에 다른 팩을 직접 선택하는 요구를 만족하지 못할 수 있으므로**, P02의 정확한 같은-source 범위와 별도 scoped 선택 주체/입력 표면을 결정해야 한다. registry 수정과 admission의 경합, revoke→재승인 ABA에서 실제 보존·비교할 immutable revision도 명세 대상이다.

**취소 후보:** 신규 admission·stage·Resume은 차단하고 기존 pin은 변경하지 않는다. 이미 시작된 stage의 결과 수집과 새로운 실행·최종 승인 허가는 별도로 검증한다. 원 receipt는 지우지 않으며 정확한 retry는 현재 조회 권한을 확인한다. 미수락 hold의 **abort/commit-fail 같은 비실행 결정까지 차단할지**는 복구 가능성과 안전성의 trade-off가 있어 미정이다. `completed`/`recovery_required` 자동 재실행 금지 및 B1의 인간·service 권한/receipt 의미를 그대로 보존한다.

**Legacy 후보:** 원 digest·authority·검증 가능한 snapshot이 모두 있고 현재 권한/fence/registry까지 만족할 때만 원 run 재개를 검토한다. authority 결손이면 기존 권한 범위의 조회와 receipt 보존만 허용하고 오늘의 grant로 과거 권한을 채우지 않는다. snapshot 부재·변조면 신규 실행을 금지한다. digest 누락·불일치 또는 같은 lifecycle의 혼합 이력은 임의 대표값으로 pin하지 않고 신규 작업을 차단한다. 원 run이 없으면 정상 권한 아래 첫 pin을 생성하는 후보이다. **이관 provenance 정의, 혼합 이력의 기존 hold 재개 및 예외 복구 방법은 아직 미결**이며 기존 역사 유무는 실측하지 않았다. 신규 lifecycle도 기존 held/active 검사 우회 불가.

**Migration 후보:** 모든 writer 정지 및 재기동 통제 → WAL 포함 일관 백업 → migration/schema 전이 → 신규 writer 실행. 오래 열려 있는 구 connection은 version 검사만으로 막힌다는 보장이 없으므로 별도의 DB guard 또는 운영상 완전 정지 증명이 필요하다. 신규 write 전 실패와 신규 write 후 복구는 별도 정책이며, 이후 기록·외부 효과가 있을 때 이전 백업 복원을 무손실 rollback으로 칭하지 않는다. 실 운영 DB에 대해 실행한 적 없다.

**최소 task 후보:** `dep-bump-v1`의 repository/base commit, 허용 manifest 경로·dependency·정확한 target version을 검증하는 구조화 입력. 기존 intake는 `github-issue` 본문을 저장하고 생산 admission을 `waiting_unsupported`로 둔다. watch는 claim을 가져와도 현재 CLI는 `claimed-awaiting-adapter`만 출력한다. 따라서 parser → 검증 입력·digest → ready → claim → intent → host → tuple → 인간 승인으로 실제 제품 연결을 새로 설계해야 하며, `InputManifest.task = dedup_key`와 task의 실제 의미를 구분해야 한다. source 기반 권한·producer와 승인된 task의 identity·grammar·경계값·오류·작업 공간/실행 격리 전제는 확정 전이다. fake fixture나 P2 synthetic 입력은 생산 경로 증거가 아니다.

### 최소 입력과 issue 편집의 실제 경계 (정적 대조; DRAFT)
- `intake.rs:362-378`은 열린 issue의 label을 확인해 issue id·제목·본문을 `github-issue` snapshot에 넣는다. 그 label이나 issue 작성자의 텍스트만으로 task 실행 권한을 부여하는 계약은 여기서 확인되지 않는다. `intake_worker.rs:77-96`은 poll·admission·claim을 호출하지만 이 함수에서 host dispatch는 호출하지 않는다. `main.rs:2769-2795`는 claim을 출력한 뒤 해당 tick 분기를 마친다. 이 관찰은 지정 경로의 정적 사실이며 저장소 전체의 유일 경로를 증명하지 않는다.
- **편집 불변 사례:** `product.rs:4376-4396`은 동일 connector/repository/issue의 수정·재개장을 새 lifecycle이 아닌 새 delivery version으로 취급하고 최초 admission의 task snapshot과 receipt를 유지한다. 따라서 가령 처음에 지원하지 않는 본문으로 접수된 issue를 나중에 `dep-bump-v1` 형식으로 편집해도, 현재 admission 경로가 기존 주문의 task snapshot을 그 편집본으로 대체한다고 약속할 수 없다. 이 변경을 W2에서 몰래 덮어쓰면 기존 B1 의미·승인 identity와 충돌한다.
- **권고 방향:** W2의 지원 입력은 immutable admitted snapshot에서 검증하고, source identity 및 별도의 현재 실행 권한에 결속한다. 기존 issue 편집을 task 수정/재승인으로 취급하지 않는다. 수정본을 새 작업으로 수용하는 UX가 필요하면 새 lifecycle 생성 조건, 기존 hold·receipt 보존, 명시적 권한 및 정확한 snapshot binding을 별도 계약으로 정한다. 최초 snapshot을 수정하지 않고 별도의 owner-authorized task record를 두는 대안도 가능하지만 새 내구 API·마이그레이션 비용이 있으므로 아직 선택하지 않는다.
- **W2 필수 반례:** 최초 허용 snapshot A 접수 → 같은 issue의 편집본 B 관측 → 원 A, 원 receipt, 승인 대상 identity 불변. 최초 미지원 A → 허용 모양 B 편집에서도 B가 자동 실행되지 않음. 유효한 명시적 새 lifecycle/새 작업 절차가 따로 동결된다면 그 절차의 positive control은 별도 실행한다. 이는 예정 인수 시나리오이지 기실행 결과가 아니다.
- `foundry-harness-cli/src/lib.rs:83-84,110-117,137-141`에서 `Order.external.dedup_key`는 현재 manifest의 `task`와 `--task`에 전달된다. W2가 구조화 task를 그 경로로 보내려면 전용 전달 경로, canonical task bytes와 digest, 오래된/다른 task의 승인 바인딩을 정의해야 한다. `dedup_key`의 의미를 변경한 것으로 가정하거나 fake executor 관찰을 실제 Die 증거로 대체하지 않는다.

## 독립 병렬 검토 후 수렴안 (DRAFT — 미동결)

`deleg_29d97445`의 두 독립 정적 검토를 부모가 `product_scope.rs:139-185`, `product/review.rs:544-675`, `product.rs:2802-2842,2990-3042`, `review_hold.rs:169-313`, `b1-scope-architecture.md:90-115`와 대조했다. 검토 보고의 새 schema·trigger·API는 **설계 후보**이지 현재 구현이나 유일한 해법이 아니다. 생산 코드 변경과 운영 migration은 하지 않았다.

### 선택: owner의 주문별 지정과 admission pin을 구분
- **권고:** owner만 source/repository 범위의 digest를 승인·취소하고, owner가 특정 `(order_id,lifecycle)`에 승인된 digest를 선택한다. `DriveReview` 서비스는 선택하거나 승인하지 않고 이미 인가된 선택을 소비한다. 현재 `ProductCapability`에는 팩 선택 권한이 없다. 따라서 명시적 scoped 선택 surface와 권한 검증이 새로 필요하며, 미인증 issue 본문 또는 임의 로컬 경로를 selector로 쓰지 않는다. source 기본값 하나만으로 같은 source의 A→X, B→Y를 표현할 수 없다.
- 비동기 watch가 사람이 접속하지 않은 시간에도 실행할 수 있어야 하므로, **선택 요청의 내구 기록**과 **실행 pin**을 별개로 제안한다. owner의 선택 요청은 검증된 `(order_id,lifecycle, repository/source identity, digest, approval revision, selection revision, actor, idempotency identity)`를 기록하는 후보이며, **아직 실행 pin이나 인간의 결과 승인과 같지 않다**. 선택 요청은 admission commit 전까지만 변경 가능하게 하는 방안이다. 정확한 공개 API·필드 인코딩·테이블 및 오류는 확정 전이다. 이것은 앞의 '접수부터 pin 확정 아님'과 양립하지만 선택 요청 자체도 비내구라고 가정했던 후보보다 변경면이 크다.
- 최초 review admission의 `BEGIN IMMEDIATE` 내에서 현재 source/service grant·claim/fence, 선택 요청과 승인 revision 및 digest를 재확인하고 `(order_id,lifecycle)` 유일 pin과 run/work/authority/active를 원자적으로 commit한다. 기존 host config digest 비교, run alias 및 authority 검사를 삭제하지 않는다. X/Y 요청이 경합하면 한 pin만 commit; 바쁜 DB는 정책상 conflict와 다르게 표시. 선택 변경과 admission의 순서별 결과를 시험한다.
- 승인 취소·동일 digest 재승인의 revision ABA는 동일 승인으로 치지 않는다. 이 경우 기존 pin·수락 receipt는 보존하고 새 stage/Resume을 차단하는 보수안이다. 재승인 후 같은 lifecycle을 어떻게 회복할지, 비실행 hold 결정의 허용 여부 및 이미 시작된 stage의 결과 수집/최종 승인 기준은 B1 호환 검토 전 미결이다. 진행 상태 조회는 현행대로 결정자 자신의 현재 세션/역할과 `ReadReview` 검사를 포함하며, 정확한 재시도는 request 및 원 authority binding도 일치해야 한다. 단순한 '현재 조회 권한만 있으면 어떤 retry든 receipt 반환'은 아니다.
- 파일 영향 후보: `product.rs`/`product/review.rs` schema·선택 및 pin transaction, `product_scope.rs` 권한, `review_host.rs` host 구성, `main.rs` 제품 구성, `auth_http.rs`/`review_hold.rs` host 선택 및 receipt 경로, 신규 selector 표면·관련 시험. 일부는 원 M9 허용 목록 밖이며 실제 최소 변경면과 새로운 wire/schema 영향은 독립 확정이 필요하다.

### Legacy와 migration의 보수 정책 후보
- 원 기록은 삭제·변경하지 않고 **read**, **receipt 조회/정확 retry**, **accepted continuation**, **새 작업**을 개별 판정한다. 'read 허용'은 누구나 접근을 뜻하지 않고 기존 세션·역할·scope·actor 요건을 충족한 경우만을 뜻한다.
- 과거 run이 없으면 현재 선택·registry·grant·claim/fence를 만족한 최초 pin을 검토한다. 단일 digest + 완전한 원 authority + 검증 가능한 원 snapshot이면 원 run의 continuation을 현재 권한/fence/registry와 원 binding을 재검증한 뒤 허용하는 후보. 해당 legacy lifecycle의 신규 activation은 자동 pin/backfill 없이 차단하고 새 lifecycle도 기존 active/held/epoch를 우회하지 않는다. 이 제한은 기존 업무 재시도 가능성을 줄이는 호환 비용이 있다.
- 원 authority 누락, snapshot 부재/변조 또는 digest 누락·불일치·혼합 이력은 원 기록 조회와 해당 요건을 만족한 receipt 접근은 보존하되 continuation 및 신규 작업을 기본 차단하는 후보. 여러 결손이 동시에 존재해도 차단하며 오늘의 grant로 과거 권한을 채우지 않는다. 원래 snapshot의 복원이나 관리자의 수동 복구를 허용할지와 그 provenance는 별도 미결이다. B1이 보장한 기존 합법 accepted continuation까지 불필요하게 차단하는지 구현 전 대조가 필요하다.
- 유지보수 정지와 구 writer 종료/재기동 통제는 운영 사전조건이다. 프로세스·namespace·열린 FD 확인은 운영 증거이며 모든 가능한 구 writer 부재의 수학적 증명은 아니다. WAL 유무를 포함한 SQLite 일관 backup API 사용과 복원 검증을 후보로 둔다. migration은 schema·이력 census·guard·version을 단일 commit하는 방향이나 SQLite/실제 schema에 대해 아직 시험하지 않았다. 신규 durable write 전의 백업 복원과, 이후의 전진 복구/외부 효과 reconciliation을 별도 절차로 둔다. 이후 백업 복원을 무손실 rollback으로 부르지 않는다.
- 독립 검토는 미등록 connection-generation UDF를 요구하는 trigger를 추가 방어로 제안했다. 로컬 SQLite 모형 `/home/cosmo/.hermes/cache/scratch/b2-w0-udf-guard-probe.py` 실행(exit 0): 열린 구 연결은 `no such function: b2_generation`으로 insert 거부, UDF를 등록한 새 연결은 commit. **그러나 구 연결도 같은 이름의 UDF를 직접 등록하면 insert가 성공했다.** 따라서 이 방식은 미등록 정상 구 바이너리의 우발적 쓰기를 막는 후보이지, 임의 DB 접근이 가능한 악의적 writer에 대한 인증·보안 경계가 아니다. 같은 모형에서 `PRAGMA trusted_schema=OFF`를 설정한 신규 연결은 Python 기본 UDF에 대해 `unsafe use of b2_generation()`으로 거부되었다. 이는 사용하는 SQLite/UDF 등록 방식의 실험 결과이지 Rust rusqlite의 UDF flag 또는 Foundry의 실제 설정에 대한 결과가 아니다. trigger를 적용할 실제 모든 table/write와 accepted result 보존, 정상 read·receipt·migration, `trusted_schema`/UDF 호환성, 실제 구 바이너리 및 프로세스 중단을 별도 시험하기 전 채택·안전 판정 금지.

## 후속 독립 심사: W1/W2 순서와 실제 생산 경계 (DRAFT)

`deleg_bcadc552`의 정적 조사를 부모가 `product/result.rs:415-479`, `product.rs:4687-4787`, `review_host.rs:863-940`, `die_executor.rs:124-170`, `approval_service.rs:16-32,91-136`, root `Cargo.toml:27-32`, `product/review.rs:710-807` 및 B1 아키텍처 `:228-243`에 대조했다. 어느 검토도 제품 경로·migration을 실행한 것은 아니다.

- **준비 상태의 실질적 결손:** `waiting_unsupported→ready` 문자열 검색에서 제품 source의 직접 UPDATE는 feature-gated `prepare_p2_review_fixture` (`product/result.rs:415-448`)에서만 확인되었다. 같은 식의 여러 테스트 SQL도 있다. 이 제한된 검색 결과는 모든 다른 SQL 표현이나 동적 우회를 배제하지 않는다. P2 helper는 `DieStarted → DieSucceeded → InspectionPassed`까지 시험 준비할 수 있으나 실제 Die가 일을 했다는 뜻이 아니다. 서비스 claim은 `ready`뿐 아니라 `retryable` 및 lease 조건을 만족한 `claimed/completed`도 후보로 취급한다(`product.rs:4707-4710`).
- 현재 review host의 `try_dispatch_and_finalize_product_work`는 이미 **claim과 결과 commit을 가리키는 intent**를 입력으로 받는다(`review_host.rs:863-895,911-940`). 이것을 최초 Die 실행 및 S1 생산자 자체라고 부를 수 없다. 실제 초기 Die 실행 → 결과 commit 확인 → S1 transition → review intent/host/finalizer → 인증된 인간 approval을 제품 경로로 잇는 producer surface를 별도로 설계해야 한다. 기존 `line.rs` 등의 일반 경로 재사용 가능성은 미실측이며 전부 새로 구현해야 한다고 단정하지 않는다.
- 최초 Die의 `CliHarness`는 `dedup_key`를 task로, review stage의 `die_executor.rs:152-170`은 `role:stage`를 task로 쓴다. 둘은 별도 호출이다. task source, canonical bytes/digest가 각 입력·claim·result binding·인간이 보는 승인 대상까지 어떻게 전달되는지 명시하고, approval DTO에 task digest를 추가할지 기존 내구 결속을 통한 검증으로 충분한지는 대안별로 검증한다. DTO에 field가 없다는 사실만으로 그 추가가 유일한 방법이라고 가정하지 않는다.
- **의존성 재배치 권고:** W0 계약/격리 선행조건 정제 → W2a로 단일 기존 승인 팩을 이용하는 최소 production producer seam을 먼저 구축(실제 미신뢰 task는 선행 격리 충족 전 실행 금지) → W1 선택/pin과 같은 source 두 주문 P02 → W2 실제 Die·승인 E01/E02/E02b → W3 배포 환경 격리 실측. W2a는 W2 전체 PASS가 아니며 fake executor 또는 synthetic 준비를 real Die 증거로 내세우지 않는다. 기존 W1을 W2 완결 전 독립 완료라고 가정하지 않으며, W 단위 커밋 순서 변경의 범위는 계약을 동결하기 전에 이 계획과 매트릭스에 함께 정리한다.

### 구 writer 보조 guard의 반례와 B1 보존
- 신규 run뿐 아니라 stage 시작(`product/review.rs:710-746`), stage 완료(`:748-807`), hold 수락과 final result는 서로 다른 쓰기 경로다. B1은 service revoke 뒤 이미 accepted된 stage 결과 수집·저장을 허용하면서 다음 새 stage를 거부한다(`b1-scope-architecture.md:239-243`). 전체 write를 무차별 막는 trigger는 이 보장을 깨뜨릴 수 있다. 검토자의 pure-SQL pin-match trigger는 한 후보이지 구 writer의 세대 인증을 증명하지 않는다.
- 로컬 SQLite 모형 `/home/cosmo/.hermes/cache/scratch/b2-w0-pure-sql-guard-probe.py` 실행(exit 0): 이미 pin A→X가 있을 때 구 연결의 stage Y는 `pin mismatch`로 거부되었지만, **같은 pin X를 참조하는 구 연결의 stage 시작은 commit되었다**. 모형 stage table은 설명용 축약 구조로 실제 Foundry schema가 아니다. 따라서 pin 일치 조건만으로 이미 선택된 같은 digest의 구 writer까지 차단할 수 있다고 말할 수 없다. 정지/quiescence 또는 별도의 작동이 입증된 세대 fence가 필요하다.
- workspace `Cargo.toml:32`는 rusqlite에 `bundled` feature를 선언한다. **별도 scratch Rust 모형** `/home/cosmo/.hermes/cache/scratch/b2-rusqlite-guard-probe`를 `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/cosmo/.hermes/cache/scratch/b2-rusqlite-guard-probe/target cargo run --offline --manifest-path /home/cosmo/.hermes/cache/scratch/b2-rusqlite-guard-probe/Cargo.toml`로 실행했다(exit 0). `rusqlite=0.32.1`, `bundled,functions`, SQLite 3.46.0에서 `SQLITE_INNOCUOUS`로 등록한 새 연결은 `trusted_schema=OFF` 상태에서도 insert 성공, UDF 미등록 구 연결은 `no such function`으로 거부되었다. 같은 이름의 UDF를 등록한 구 연결은 성공했다. 이는 **해당 feature/flag 조합이 모형에서 동작함과 악의적 등록을 구별하지 못함**을 보여줄 뿐, Foundry의 Cargo feature 통합·실제 migration·모든 opener 등록·실제 쓰기 경로·구 바이너리 호환을 검증하지 않는다.

## W2 최소 task·격리 후보의 독립 조사 (DRAFT, 미동결)

`deleg_803565b7`의 제안을 부모가 `product.rs:5757-5765`, `product/result.rs:1127-1198`, `order.rs:129-154`, `harness-cli/src/lib.rs:110-182`, `model_proxy.rs:241-274`, 두 crate Cargo.toml에 대조했다. 아래는 아직 동결·구현·실측되지 않은 한정된 후보이다.

- **예시 작업:** crates.io 단일 의존성의 `Cargo.toml` `[dependencies]` 정확 버전만 변경하는 `dep-bump-v1`; path/git/alias/submodule/symlink 및 임의 셸 입력은 지원하지 않는 방향이다. 후보 입력은 고정 repository/source identity, full base commit, manifest 상대 경로와 원본 digest, dependency, 변경 전후 정확 버전 및 기존 `Cargo.lock` 원본 digest다. 목표 manifest의 예측 digest까지 사전 요청하면 예상 바이트의 생성 주체가 필요하고 포맷·주석 변경에 민감하므로, 그 필드를 필수로 동결하기 전에 owner가 어떻게 산출하는지와 의미 기반 검사 대안을 비교한다. 값의 문자집합·길이·JSON 중복 key/직렬화·version 문법·에러 순서는 아직 정해지지 않았다.
- **권한/바이트:** issue 본문을 최초 raw snapshot으로 보존하고 별도 owner 승인으로 검증 task의 불변 bytes/digest를 source·admission·order/lifecycle에 연결하는 후보. 기존 `payload_digest`는 `foundry-product-intake-v1\0`과 길이 framing을 사용하는 *기존 raw intake* hash(`product.rs:5757-5765`)이며 private 함수다. 구조화 task digest를 이 값으로 대체/동일시하지 않는다. 별도 task domain 또는 명시적으로 정의한 기존 digest 재사용과 이중 binding을 비교하고 canonical bytes의 실제 생산/검증 경로를 동결해야 한다. 기존 result payload가 work snapshot/digest를 담는다는 사실(`product/result.rs:1144-1185`)만으로 새 task digest가 자동 검증되거나 승인에 노출된다고 주장하지 않는다.
- **실행과 검사 후보:** 승인된 repo identity→안전한 workspace resolver, 검증한 detached base와 파일 preimage → claim/fence 검증 및 합법적인 `DieStarted` → 격리된 Die → 실제 result commit → 독립 inspection → 기존 B1 인간 결과 승인. 기존 `Approved`는 결과 승인이지 task 실행 허가가 아니다(`order.rs:150-154`). 가능한 검사에는 결과가 base의 정확한 자식 commit인지, 변경 범위가 지정 manifest와 기존 lockfile뿐인지, TOML 의미가 단일 지정 변경인지, lockfile 삭제·생성 및 예상 외 파일 변경이 없는지 포함한다. 실제 정확한 S1 transition, receipt 및 retry/재개를 기존 상태 기계에 대조하기 전 임의 이벤트 삽입은 금지한다.
- `cargo update --offline`이나 `cargo metadata/check --locked --offline`의 성공은 해당 registry index·crate 및 transitive 의존성 캐시에 달린다. **cache miss는 미실행/명시적 실패이지 네트워크 허용이나 성공이 아니다.** Cargo build script·proc-macro도 신뢰하지 않으므로 check/inspection에서 실제 코드를 실행한다면 동일 이상 수준의 격리가 필요하다. 여러 버전의 같은 crate, workspace dependencies·features·target별 테이블·pre-release, lockfile 결정성과 exact 버전 표기는 범위를 좁히거나 별도 정의해야 하며, 이 문단은 구현 완료 주장이 아니다.
- **문법 모형 실험:** `/home/cosmo/.hermes/cache/scratch/b2-w0-task-grammar-probe.py`를 `python3`로 실행해 exit 0, 6 tests OK. 예시로 ASCII canonical JSON 고정 필드/정렬, duplicate key·알 수 없는 필드·비정규 바이트·상대 경로 이동·비정상 commit·동일/잘못된 정확 버전을 거부하고, task bytes 변경 시 독립 domain-framed SHA-256이 달라지는지 검사했다. 모형은 범위를 root `Cargo.toml`만 허용하고 4096 bytes 상한을 임시로 쓰며, **4096은 최적값·승인된 제품 제한이 아니다**. 숫자의 상한, 실제 SemVer·Cargo TOML 해석, symlink·파일·DB·권한·실제 Die·승인 binding 및 격리를 검사하지 않는다. 제안된 bytes/domain/schema를 제품 계약으로 동결하거나 6건을 B2 인수 PASS로 세지 않는다.
- **격리 선행:** 기존 harness는 host에서 `Command`를 실행하고 HOME을 다시 넣는다(`harness-cli/src/lib.rs:128-147`); 기존 `docker_argv`의 non-root/read-only 등의 선언(`model_proxy.rs:244-274`)은 실제 제품 Die의 전체 mount/network 정책 실행 증거가 아니다. 주문별 작업공간 외 DB/WAL·HOME·자격증명·소켓·타 주문 및 privileged API 차단, 승인된 인증·예산 프록시만 허용, 직접 DNS/IP/IPv4/IPv6 우회와 종료 후 자식 쓰기 차단을 실제 runtime에서 측정해야 한다. normal workspace/proxy control과 반례는 각각 독립한다. image/daemon/host kernel 및 관리자 신뢰는 명시적 한계다.
- **의존성 방향:** 현 `foundry-watch → foundry-line` 및 `foundry-ledger → foundry-exec`(`Cargo.toml` 각 crate dependency) 관계에서 조립을 root 또는 기존 방향에 두고 역방향 `line → watch`, `exec → ledger`를 추가하면 순환한다. 필요한 공통 port/DTO를 어느 소유 crate에 둘지는 공개 계약과 범위 조사 후 결정한다.

## W0 격리 위협 모델과 실행 전 조건 (DRAFT)
**보호 대상:** product SQLite/WAL, owner·service·publisher credential, 다른 주문의 작업공간·산출물, 권한 있는 serve API, 외부 provider 예산. 공격자는 Die/cast 내부에서 생성된 코드와 자식 프로세스를 임의 실행할 수 있다고 가정한다. 동일 사용자 subprocess는 이 공격자에 대한 파일 격리 수단이 아니다(`docs/design/b1-scope-architecture.md:184-189`). 호스트 관리자, 커널, Docker daemon 및 신뢰된 이미지 관리자는 신뢰 기반으로 둔다는 **후보**이며, 이들 침해까지 격리한다고 주장하지 않는다. 이미지 출처·digest 고정 및 daemon 접근 권한은 배포 사전조건이다.

**경계 후보와 인수 관찰:**
- **DB/비밀:** 해당 컨테이너에 호스트 HOME, product DB/WAL, credential 디렉터리, Docker socket을 주입·마운트하지 않는다. 런타임 mount/env/UID 관찰과 컨테이너 안에서 경로 접근 시도, 그리고 host 측 실제 파일·DB 불변 확인이 필요하다. 경로가 없다는 관찰은 호스트 전체 경로에 대한 완전 탐색 증명이 아니다.
- **작업공간/출력:** 하나의 주문 실행에 필요한 작업공간만 쓰기 허용; 타 주문 작업공간은 별도 경계로 접근 거부. output을 지휘자가 검증·봉인하기 전에는 공격자가 신뢰된 결과로 직접 등록할 수 없도록 한다. 저장 공간의 링크·mount·소유권 및 실행 종료 후 자식 프로세스까지의 범위를 시험한다.
- **네트워크:** Die/cast가 직접 인터넷과 권한 있는 serve API에 도달하지 못하면서, 인증·run·예산 결속된 제한 프록시 정상 호출은 가능해야 한다. DNS·IP 직접 연결·리다이렉트와 IPv4/IPv6 각각의 실제 지원 범위를 명시하고 시험한다. `ModelProxy`의 요청 형태·screening 함수만으로 운영 listener 연결이나 OS 수준 차단이 증명되지 않는다(`model_proxy.rs:62-70,100-133`). 프록시가 허용하는 진입점과 권한 API가 같은 네트워크에 놓인다면 라우팅 규칙 또는 별도 경계가 필수다.
- **프로세스 환경:** `env_clear`는 환경 상속만 제한하며, 현재 `HOME`은 호스트 값으로 설정된다(`foundry-harness-cli/src/lib.rs:128-147`). 개별 명령의 env 검사와 파일 접근 거부를 별도 인수한다. W2는 위 조건을 만족하는 실제 실행 경계를 갖추기 전 실제 Die에 비신뢰 task를 전달하지 않는다. W3는 배포 구성·override 및 실제 런타임까지 인수한다.

**기존 Docker 시험의 실행상 주의:** `crates/foundry-watch/tests/t56w_docker_mandatory.rs:60-69`는 고정 이름 `t56w-live` 컨테이너와 `t56w-proxynet` 네트워크를 삭제 시도하고 `docker pull busybox:1.37`을 수행한 뒤 이미지를 태그한다. 따라서 현재 원 명령을 무심코 실행하면 외부 pull과 기존 동일 이름 자원에 영향이 있을 수 있다. 오프라인 재현 및 안전한 자원 식별/정리 방법을 먼저 설계하고 해당 시험은 **미실행**으로 둔다. 기존 시험만으로 제품 Die 전체 격리나 정상 proxy listener를 입증하지 않는다.

## D-LEGACY / D-PRODUCER 독립 검토 결과 (DRAFT)

`deleg_2e74f9e8` 정적 검토. 기존 6개 W0 판정·A/P/E/I/R 행 밖 신규 요구를 만들지 않는다. **구현·제품 시험은 하지 않았다.**

- **D-LEGACY:** 이력의 digest `{단일, 혼합, 부재}` × 원 authority `{완전, 불완전}` × snapshot `{검증, 불량}` 총 12개 조합에서 read와 기존 receipt exact retry는 기존 세션/역할/scope 및 원 request/actor 규칙으로만 다룬다. 단일·완전·검증 1조합만 원 authority와 현재 권한/fence/snapshot/registry를 추가 확인해 continuation 가능성을 판단하며 나머지 11개는 거부하는 후보. 해당 legacy lifecycle 신규 activation은 이력을 현재 권한으로 backfill하지 않고 거부하는 후보; run 이력 없는 별도 control은 현재 정책으로 최초 pin 가능성을 시험한다. 불량은 부재·변조로 각각 시험한다. 12/11은 가상 열거이지 운영 데이터 수가 아니다.
- **migration 후보:** writer 중지·열린 구 connection 종료 확인 → SQLite backup API의 일관 백업과 독립 복원 검증 → 단일 transaction migration. schema version은 새 구 바이너리 open을 차단하는 후보, connection UDF trigger는 미등록 열린 정상 구 writer의 우발 쓰기 차단 후보이지 임의 SQL을 실행하는 동일 UID 공격자의 인증 경계가 아니다. 운영 환경의 열린 writer 전수 확인 및 구 바이너리 실제 차단은 미측정이다. migration commit 전 crash와 commit 후 첫 신규 durable write 전 restore는 별도 검증; 첫 신규 durable write 후 과거 backup 복원은 무손실 rollback이 아니라 재해복구와 reconciliation이다. B1은 이미 accepted된 stage 결과 수집을 보존하되 신규 stage는 revoke 후 거부한다(`docs/design/b1-scope-architecture.md:228-243`). P07/P08 양성·부정 fixture 모두 NOT-RUN.
- **D-PRODUCER:** 현재 `product_github_tick_with_clock`은 poll→admit→claim까지이며(`foundry-watch/src/intake_worker.rs:68-96`), `try_dispatch_and_finalize_product_work`는 이미 주어진 claim·intent를 사용하고 host error면 finalizer를 호출하지 않는다(`foundry-line/src/review_host.rs:863-895`). `waiting_unsupported→ready` 및 S1을 합성하는 P2 helper를 A01 생산 증거로 대체하지 않는다. 신규 adapter/ready CAS→claim/fence→fake producer→내구 S1→기존 host/finalizer의 실제 진입점·트랜잭션·관측점을 정의해야 한다.
- **권한 충돌 미해결:** 독립안이 제안한 `DriveReview`와 고정 adapter 정책만으로 task 실행을 승인하는 방식은 이 초안의 **Owner가 명시적으로 task digest를 허가하고 DriveReview는 소비만 한다는 후보**와 충돌한다. 이 부분은 채택하지 않는다. 명시적 task 허가와 ready 전이의 계약·다중 transaction crash/retry 경계를 확정하기 전 A01은 BLOCKED다. fake producer의 합성 S1과 실제 producer path의 차이를 시험 body에서 입증해야 하며, fake 성공은 실 Die·격리 PASS가 아니다.
- `watch→line`, `ledger→exec`의 기존 의존 방향을 거꾸로 만들지 않는 DTO/port/조립 위치가 필요하다. ledger 및 `foundry-watch/src/intake_worker.rs`는 `.specs/m9-plan.md:367-377` 범위 밖이다. 필요성·대안·호환을 결정하기 전 변경 허용으로 간주하지 않는다. W0→W2a(A01)→W1→W2(격리 선행)→W3→W4 후보 순서는 유지하며, 각 W 완료 시 검증·커밋 정지 규칙을 따른다.

## D-ISOLATION 독립 구체화 (DRAFT)

Codex CLI `gpt-6.1-terra` / `model_reasoning_effort=high`, read-only 검토 `proc_c14db619d613`, exit 0, 로그 `/home/cosmo/.hermes/cache/scratch/b2-w0-terra-isolation.log:1726-1739`. **후보이지 제품 구현이나 인수 결과가 아니다.** orchestration은 serve에 남기고, 비신뢰 입력을 실제 Die에 전달하기 전 주문 실행별 컨테이너와 인증·run/provider/예산 결속 프록시 listener를 실측한다. W3에서는 같은 정책을 Compose 기본·허용 override 및 실제 runtime의 I01–I06으로 별도 인수한다. W2 선행 실측은 W3 PASS가 아니다.

- 비root UID, 모든 capability 제거, no-new-privileges, 읽기전용 rootfs 및 자기 주문 workspace만 쓰기 허용. DB/WAL, host HOME, 자격증명, Docker socket, 다른 주문 및 watch 소유 산출물은 비노출·비수정을 요구한다. 이미지 출처/digest, host kernel·관리자·daemon은 신뢰 기반으로 명시한다. UID만으로 격리를 주장하지 않는다.
- Die는 직접 egress 없는 내부 경계에서만 프록시에 도달하고 프록시는 별도 egress에서 허가된 provider로 전달하는 후보. 프록시의 DNS 해석과 각 redirect 검사, run/provider/budget 결속, IPv4/IPv6 지원 범위 및 직접 DNS/IP·serve 권한 API 차단을 실제 경로에서 확인한다. 네트워크 이름만으로 격리를 입증하지 않는다.
- I02/I03은 대상 서비스·자원이 별도 양성 제어에서 실제 접근 가능함을 확인한 뒤 Die에서 거부 및 host 상태 불변을 단언한다. 자기 workspace 쓰기·프록시의 정상 요청은 성공 대조군. I04/I05는 upstream 도달·budget charge를, I06은 timeout 이후 하위 프로세스와 추가 write를 관측한다. **전부 NOT-RUN**.
- 부모 대조: `die_executor.rs:164-165`는 `CliHarness`를 호출하며 `harness-cli/src/lib.rs:130-146`은 host `Command`와 HOME 재주입을 사용한다. `:131-135,169-173`의 명시 commit stage process-group 설정만으로 모든 자식이 제거되었다고 주장할 수 없다. `model_proxy.rs:221-227,244-261`의 cast argv/spec 속성은 **제품 Die 경로에서 실행된 격리 증거가 아니다**.
- 기존 Docker 시험의 고정 이름 삭제·pull/tag를 그대로 실행하지 않는다. 고유 이름·local image/no-pull·자기 자원만 정리하는 별도 fixture 후보는 기존 R1–R4/UID/flags와 대조하며 원 시험을 실제 실행한 것처럼 표시하지 않는다. 프록시 budget 연결이 목록 밖 파일이나 schema 변경을 필요로 하는지는 미확정이다. 실제 Docker·proxy listener·격리 시험은 이번 검토에서 수행하지 않았다.

## D-PIN / D-TASK 독립 구체화 (2026-10-29, DRAFT)

`deleg_7c1c9ff0` 두 독립 검토의 제안. 구현·시험·계약 동결이 아니다. D-PIN과 D-TASK는 아래 잔여 정합성 검토 전 **BLOCKED**다.

**D-PIN 후보:** Owner의 source/repository 범위 승인·취소와 별도 `SelectReviewPack` capability를 가진 Owner의 주문/lifecycle별 선택을 분리한다. `DriveReview`는 선택자가 아니다. 승인과 선택에 각각 단조 revision, actor·scope·idempotency binding을 둔다. admission의 기존 `BEGIN IMMEDIATE` 안에서 현재 claim/fence·선택·승인 revision·host binding을 재검증하고 `(order_id,lifecycle)` 불변 pin과 run/work/authority/active를 함께 commit하는 안이다. selection 변경이 먼저면 구 revision admission을 거부하고, admission이 먼저면 selection 변경을 거부한다. exact retry는 현재 권한을 먼저 확인하고 동일 요청의 기존 receipt를 반환하는 후보이며 key 재사용 시 다른 요청은 conflict다. 취소·재승인은 revision을 새로 부여하고 원 pin과 receipt는 보존한다. 이미 시작한 stage의 결과 수집은 신규 stage·Resume 허가와 별개로 취급해 B1 불변을 보존한다. path는 외부 요청이 아닌 신뢰된 로컬 catalog에서 resolve하는 후보. 각각의 권한·트랜잭션·host/receipt 오류 우선순위 및 acceptance/terminal 분기는 실제 B1 경로와 다시 대조해야 한다.

**D-TASK 후보:** root `Cargo.toml`의 `[dependencies]` 또는 `[workspace.dependencies]`에 있는 단일 정확 버전 상향만 대상으로 하는 lexical-key ASCII JSON이다. 기존 raw intake digest와 별도의 `SHA256(b"foundry-dep-bump-task-v1\0" || u64be(len(task_bytes)) || task_bytes)`를 제안한다. manifest 예상 postimage를 요청자가 제공하는 대신 inspector가 실제 postimage hash와 의미상 변경을 검증해 결과에 봉인하는 안이다. root workspace table 포함은 기존 `[dependencies]` 전용 후보의 **범위 변경 제안**이므로 무심코 동결하지 않는다. 권한 binding은 raw/task digest, source/repository/order/lifecycle과 approval revision에 결속; 최종 사람의 결과 승인은 결속된 결과를 통하여 task digest에 전이적으로 연결하는 후보다. 현재 승인자가 실제 task·diff를 볼 수 있는 read/UI, 입력 parser→ready, result binding은 미구현이다. offline cache 부족은 정상 요청도 실패하게 하는 의도된 trade-off이며 cache snapshot이 없으면 완전한 재현성은 주장하지 않는다.

**독립 바이트 재계산:** 아래 허구의 저장소 표본은 실제 repo 또는 버전의 존재를 주장하지 않는다. 정확히 LF 없는 459바이트이며 별도 domain + u64be framing은 `00000000000001cb`, SHA-256은 `a123dccd24981b53c55166d4ae3c142f6f1f7f0120a75a5b8f4e7e0c59226dc4`다. 부모가 Python `hashlib`/`struct.pack`으로 2026-10-29 재계산했다. 바이트/hash 산술 확인일 뿐 parser·인수 PASS가 아니다.

```json
{"base_commit":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","dependency":"serde","from_exact":"=1.0.228","kind":"dep-bump-v1","lockfile_before_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","manifest_before_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","manifest_path":"Cargo.toml","repository_id":"1403455083","source_instance_id":"source-1","table":"workspace.dependencies","to_exact":"=1.0.229","v":1}
```

**입력 크기 후보 검산:** 독립 검토가 제안한 허용 필드·최대 길이(64자 commit, 64자 dependency, 128자 source, `u64::MAX` repository, 각 `u32` 범위 버전, 긴 table 이름)로 정규 JSON을 생성하면 **722 bytes**다. Python `json.dumps(sort_keys=True,separators=(',',':'))`로 계산했다. 이는 해당 후보 grammar 안의 산술 결과이지 기존 제품 제한이 아니다. 1024와 4096 모두 이 표본을 담으며, 4096은 parse 전 한정 및 확장 여유를 위한 **임시 자원 예산**이지 최적값이 아니다. 문자 제한·필드 타입·정규화가 확정되기 전 상한도 미동결이다.

**아직 막힌 쟁점:** (1) 표본의 문자열 `repository_id`와 실제 `RepositoryIdentity`의 key/connector 표현의 정합성, 40/64자 git oid 지원 범위 및 migration; (2) 최초 raw issue snapshot에서 명시적으로 허가된 task bytes가 생성되는 정확한 API와 인가·idempotency; (3) ASCII/JSON/duplicate/schema와 복합 오류의 실현 가능한 precedence, 4096 상한 및 최대 유효 크기의 독립 검산; (4) inspector가 comment/format 유지와 lockfile 추가 변경을 어떻게 검증할지; (5) 선택자 authority와 admission-time 서비스 authority 검사의 주체, revoke 후 accepted 수집과 exact-retry 및 P09의 실제 경로; (6) 제안된 PIN schema와 legacy migration/old-writer fence의 상호 정합성. 전체 6개 W0 판정과 B1/M9 불변 외 신규 요구를 만들어 합격 범위를 늘리지 않는다.

**변경 범위 대조:** `.specs/m9-plan.md:367-377`은 `foundry/src/{main,deploy_profile}.rs`, `foundry-serve/src/lib.rs`, `foundry-line/src/{review_host,die_executor,lib}.rs`, harness-cli, watch의 `model_proxy/inventory`, 제한된 blueprint/workflow/exec, 배포 파일·관련 시험·문서를 허용한다. ledger와 serve의 `auth_http.rs`·`review_hold.rs`, watch의 intake 변경은 목록 밖이다. 독립 검토의 파일 목록은 후보일 뿐 확정된 최소 diff가 아니며, 범위 확장·schema/wire 영향과 대안을 W0에서 명시해야 한다.

## 기준선 실행 (2026-10-28~29)
`CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-pack --test plain_pack`: exit 0, 1 passed / 0 failed / 0 ignored. 테스트 `plain_pack_loads_routes_and_runs_alongside_the_golden_pack_in_one_binary`는 팩 엔진 수준 기준선이며 제품 주문별 선택·재개·권한 또는 컨테이너 경로 인수 증거가 아니다.

추가 단독 실행 두 건 (각각 `cargo test --locked --offline -p foundry-line --test review_host_live <아래 정확한 테스트명> -- --exact --test-threads=1`, 같은 Cargo 환경):
- `persisted_pack_snapshot_reloads_with_the_same_digest_and_completed_outputs_are_bound`: exit 0, 1 passed / 0 failed / 51 filtered. snapshot 재로드 digest와 결과 로그 hash 결속 단언.
- `resume_rejects_a_changed_durable_pack_snapshot_before_execution`: exit 0, 1 passed / 0 failed / 51 filtered. snapshot policy.toml 변조 후 resume 오류, 추가 executor 호출 없음 및 원 hold 보존 단언.
두 테스트는 `review_host_live.rs`의 standalone host와 Fake executor를 사용한다. 실제 제품 ProductStore accepted-hold, 다중 팩, 실제 Die 또는 Docker 격리 인수 증거는 아니다.

추가 제품 스토어 기준선: `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-line --features p2-test-support --test b1_product_results accepted_resume_binds_three_generations_and_fourteen_artifacts -- --exact --test-threads=1` → exit 0, 1 passed / 0 failed / 33 filtered. 실제 ProductStore/host와 synthetic P2 test-support 입력 및 scripted `PassingExecutor`를 사용한다(`tests/support/product_result_fixture.rs:240-257,804`); 실제 Die·임의 GitHub issue·다중 팩·컨테이너 경로의 증거가 아니다. 실행 중 기존 `foundry-furnace` unused 함수와 fixture unused mut 경고가 각각 1건 출력됐다. 다른 회귀와 Docker 격리 실행은 아직 수행하지 않았다.

추가 harness 기준선: `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-harness-cli` → exit 0, 통합 테스트 17 passed / 0 failed, unit·doc tests 각 0. `die_env_is_allowlisted`는 임시 셸 wrapper의 환경에 지정 두 키 이름이 없는지 검사한다(`tests/harness_cli.rs:48-103`). 임의 파일/DB 접근 차단, 실제 Die 또는 컨테이너 격리를 증명하지 않는다.

추가 line 기준선: `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-line --test die_executor --test review_evidence -- --test-threads=1` → exit 0, `die_executor` 12 passed, `review_evidence` 1 passed, 실패·ignored 0; 기존 foundry-furnace dead_code 경고 1건. 이름에 `real_die_adapter`가 있어도 `make_executor`는 `fake-die.sh`를 사용한다(`tests/die_executor.rs:47-69,323-349`). 따라서 실제 Die binary·제품 ProductStore·다중 팩의 인수 증거가 아니다.

`review_host_live` 전체 기준선: 첫 foreground 실행은 도구 420초 제한으로 종료되어 결과 미확정이었다. 파일에 기록한 재실행 `proc_fd2fb55baa59`는 exit 0, **52 passed / 0 failed / 0 ignored**, 432.87초 (`/home/cosmo/.hermes/cache/scratch/b2-review-host-baseline.log:71-74`). `epoch_is_durable_scoped_per_order_and_rejects_first_stale_emit`의 의도된 panic 출력이 있으나 해당 테스트는 최종 ok로 판정되었다. standalone host 및 테스트 executor 기반이며 실제 제품 다중 팩·Die·컨테이너 인수 증거가 아니다.

추가 exec/workflow/orch 기준선: `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-exec -p foundry-workflow -p foundry-orch` → exit 0, 결과 행 18개 전체 ok, **268 passed / 0 failed / 0 ignored**(doc test 포함; 로그 `/home/cosmo/.hermes/cache/scratch/b2-w0-exec-workflow-orch-baseline.log`). 이는 해당 세 crate의 기존 시험 결과이지 B2 제품 다중 팩·실 Die·Docker 인수 증거가 아니다.

## 독립 인수 매트릭스 심사 (astra REFINE)
`b2-w0-matrix-review.log:788-806`, exit 0, 지정 파일 정적 검토. 7개 보완 요구: (1) 선택/registry 승인 주체·취소·검증 시점·파일/schema 범위 구체화, (2) 유효 경합 승자 정확히 1 및 idempotency·alias·commit 중단 단언, (3) legacy의 digest×authority×snapshot별 정책과 구 writer 차단/rollback, (4) host 및 Resume executor 가용성·수락 후 receipt·terminal phase 분기, (5) W2의 최소 지원 task 생산자→승인 및 선행 격리, (6) 같은 주문 watch 소유 산출물 보호, (7) 원 M9 명령별 대응·Docker 안전 대안. 매트릭스에서 (2)(4)(6)(7)의 **시험 요구를 정제**했으나 실제 실행 전이므로 해결된 제품 기능이 아니다. (1)(3)(5)는 구체 계약을 결정해야 하므로 아직 blocker로 남는다. 오류 precedence, test names/paths, 독립 재검토도 미완료다.

## 사전 승인/진행 경계
이 문서는 **제안**이며 구현을 위한 schema/wire/승인 권한 동결이 아니다. 사용자 '적용하고 진행'은 검토사항 반영 및 일상적 W0 조사/설계로 처리한다. 중간 승인 남발 없이 좁은 독립 심사와 부모 확인으로 진행하되, 위 D1/D2/D3/D4의 구체 계약·변경범위가 밝혀지면 영향·권고와 함께 인간 freeze를 요청한다. 별도의 실제 GitHub 효과는 별도 패킷과 승인 필요. B3는 범위 밖.
