# B1 W3/W4 제품 tuple 경계 설계안

상태: 검토용 초안 — 구현 스펙/계약 동결 아님.
요청 근거: Discord 1561373174719844484 (`진행해`), 직전 제안인 계약·실패·복구 경계 설계 진행. 구현/외부 호출/커밋/push 승인으로 확대하지 않는다.
기준 HEAD: 36e994c. 부모 round59: core 404, watch 17, CLI 9, 합계 430 통과/실패 0/무시 0. 이는 tuple 경로 인수 증거가 아니다. graphify 보고서 부재.

## 1. 출처와 현재 구조

- exec/src/review_run.rs:573–607: ArtifactTuple 생성 → emit_once → tuples.record → tuples.promote. 이 경로의 promotion은 emit receipt 이후이다.
- exec/src/tuple_sqlite.rs:15–37: standalone store는 ProductAtomicV1 DB를 거부한다. 이 차단은 유지한다.
- exec/src/types.rs:145–191: tuple key는 order/attempt/run/generation, tuple payload에는 immutable_commit/artifact_location/generations가 있다. 기존 digest 정의를 재해석하지 않는다.
- ledger/src/product.rs:3297–3327: 제품 승인 소비부는 이미 같은 DB의 artifact_tuples + tuple_promotions를 읽고 tuple digest와 promotion의 key를 검증한다. 따라서 '제품에 tuple 소비 계약 자체가 없다'는 표현은 과도하다. 빠진 것은 genuine host 결과에서 해당 기록을 생성하는 검증된 제품 producer와 artifact binding이다.
- line/src/product.rs:103–149: 기존 authorize/publish helper는 별도 network effect를 호출하지만 승인된 W4 durable start/outcome 계약을 대신하지 않는다.
- ledger/src/product/review.rs, serve/src/artifact_http.rs: stage log artifact의 영속 등록 및 현재 ReadArtifact 기반 읽기 경로가 있다. stage log를 코드 산출물이나 promoted tuple로 바꾸지 않는다.

## 2. 사고과정: 선택지와 기각 이유

A. standalone SqliteTupleStore의 제품 DB 차단 제거: 기각. 다른 store 소유권과 트랜잭션 경계를 우회하며 emit-before-promote도 해결하지 못한다.
B. stage 로그를 ArtifactTuple로 취급: 기각. SHA-256(log bytes)은 코드 commit도 아니고 게시 가능한 결과의 증명도 아니다.
C. W3 미완을 문서상 유예하고 W4 먼저 구현: 기각. 승인한 선행 인수와 publish의 산출물 결속을 비운다.
D. 제품 전용 검증·등록 경계를 도입하고 로컬 결과 확정과 원격 게시를 분리: 권고. 기존 standalone 의미는 보존하고, 제품 승인이 실제 결과를 소비하도록 연결한다.

중요: D는 기존 promotion 의미 변경 가능성이 있다. 기존 post-emit PromotionReceipt를 emit 없는 로컬 확정 영수증으로 위장하지 않는다. 아래 의미 분리와 승인 소비부 변경은 명시적 변경관리 대상이다.

## 3. 제안 결정과 이유

### AD-1 제품 DB의 단일 소유자
ProductStore가 product tuple/result binding의 유일한 writer다. line은 실제 host 결과와 typed evidence를 전달하고 exec는 typed port/domain type만 소유한다. ledger→line 의존, HTTP raw SQL, 범용 임의 등록 endpoint는 금지한다.

### AD-2 두 종류의 증거와 세 식별자
- stage artifact: 진단/리뷰 증거 파일. byte_sha256으로 바이트를 검증한다.
- product result tuple: 검증된 실행 결과의 immutable commit + generation chain + 의미가 고정된 artifact location.
- binding: tuple의 기존 digest, 원본 source/lifecycle/run/generation 및 관련 artifact ID/byte digest를 불변 연결한다.
commit ID, byte SHA-256, tuple digest는 별개 필드/의미다. 서로 equality를 요구하지 않는다. 예: 리뷰 로그의 byte hash가 바뀌면 파일 검증 실패지만 그 값을 commit으로 사용할 수 없다.

### AD-3 로컬 결과 확정 ≠ 기존 emit promotion
제품에는 외부 효과 없는 검증된 결과 확정 영수증을 별도 타입/레코드로 도입하는 안을 권고한다. 기존 standalone PromotionReceipt/EmitReceipt의 의미와 wire는 유지한다.
제품 승인 소비부는 새 제품 확정 영수증과 기존 tuple 내용을 함께 검증하도록 명시적으로 전환한다. 기존 tuple_promotions에 가짜 emit receipt를 넣지 않는다. 역사 승인 receipt는 재작성하지 않고 구계약은 버전/출처를 구분해 읽으며 새 게시 가능 여부는 별도 검증한다.
정확한 타입명/테이블명/버전/인코딩은 후속 package spec에서 동결한다. 지금 제안명을 구현 계약으로 간주하지 않는다.

### AD-4 실제 producer의 완료 증거가 필수
제품 host의 실제 성공 결과 및 현재 유효한 work/run fence에서 검증 가능한 result commit, base commit, generation chain을 받아 tuple을 구성한다. caller가 보낸 tuple이나 임의 DB row만으로 등록하지 않는다. commit이 없으면 log hash로 보충하지 않고 결과 확정을 거부한다.
최종 host 결과에 이 필드가 실재하는지, numeric tuple run과 제품 host run 식별자의 매핑이 영속적으로 일관되는지 후속 실측·스펙 게이트로 고정한다. 문자열 run을 임의 숫자로 바꾸거나 attempt=1 fallback을 쓰지 않는다.

### AD-5 로컬 원자성·재시작
현재 source/service/work ownership, immutable admission, lifecycle, run/generation, host 성공 결과와 artifact binding을 검증하고 제품 결과 확정을 같은 SQLite write transaction에서 기록한다.
동일 identity+동일 payload 재시도는 동일 영수증 반환, 동일 identity+다른 payload는 conflict로 거부한다. commit 전 crash는 등록 없음, commit 후 응답 손실은 영수증 재조회로 복구한다. 결과 등록 복구는 executor 재실행이나 원격 게시를 유발하지 않는다.
이미 시작한 실행의 결과 보존과 새 '게시 후보 확정' 권한은 구분한다. 철회 중 결과는 보존하되 확정 가능 여부/재승인 요건을 다음 계약에서 명시한다. 이 권한 정책을 기존 DriveReview 의미에 암묵적으로 추가하지 않는다.

### AD-6 W4 효과 경계
인간 승인은 특정 product result binding/tuple/commit을 고정한다. 게시 직전 현재 PublishApproved 및 필요한 service 권한과 정확한 target repository/base/head를 재검사하고 durable publish-start를 같은 transaction에서 기록한다. 외부 호출은 commit 뒤에만 한다.
외부 성공 여부 불명/로컬 outcome 저장 실패는 recovery_required. 결과 대조와 새 게시 재시도는 별도 권한·행위다. 무조건 재게시와 원격 exactly-once 주장은 금지한다.

### AD-7 역사 읽기와 현재 게시 분리
현재 ReadArtifact의 원본 source 명시 grant로 역사 artifact를 읽을 수 있어도 게시 허가는 아니다. rebind된 새 source grant는 원본 artifact/tuple 접근·게시 권한을 상속하지 않는다. 현재 승인·게시 검사는 원본 출처와 현재 대상의 결속을 별도로 확인한다.

## 4. 실패·복구 및 시험 인수 행렬

- T1 실제 제품 host 성공 → 로컬 결과 확정 → 인증 artifact 읽기: 정확한 bytes/tuple/binding, publisher 호출 0.
- T2 foreign source/order/lifecycle/run/generation/commit/chain: 확정 거부, 신규 tuple/binding/승인 상태 쓰기 없음.
- T3 누락 commit 또는 증명 불가 generation chain: fail closed; 로그 hash/fixture tuple 대체 금지.
- T4 동일 요청 재생 및 충돌: 동일 영수증/정확한 conflict, 중복 기록 없음. 별도 프로세스 양방향 경합 검증.
- T5 등록 transaction 경계 crash/reopen: 전체 없음 또는 전체 있음, executor/publisher 재호출 0.
- T6 stage artifact 변조 및 tuple/binding 변조를 각각 정상 baseline에서 독립 검증.
- T7 인간 승인 후 tuple/target 변경: W4 신규 외부 호출 0. 불변 승인 바이트 유지.
- T8 철회/rebind의 commit 전후, 결과 보존과 게시 후보 확정 차단을 구분해 검증.
- T9 standalone 기존 suite, 역사 receipt 읽기, additive migration 실패 rollback과 재개방.
- T10 W4 unknown 결과 대조: open/closed PR, 응답 손실, 로컬 기록 실패; 새 권한 없는 retry 0. W3 시험과 혼합 집계하지 않는다.

각 항목의 정확한 API/시험명/파일/오류 우선순위는 구현 전 package spec + traceability matrix에서 동결한다. 이 목록은 실행 결과가 아니라 요구 시험이다.

## 5. 구현 전 반드시 닫을 질문

1. genuine ProductStore-backed host 결과에서 final/base commit 및 generation chain이 보존되는 정확한 심볼/타입과 run ID 발급 원천·매핑.
2. 제품 결과 확정의 인간/service capability 정책: 결과 보존과 현재 게시 후보 확정을 분리하되 새 역할·grant를 임의 추가하지 않는다.
3. 제품 확정 영수증과 기존 product approval tuple_promotions 소비부의 이행 규칙, 역사 row 격리 및 additive migration.
4. artifact_location은 로그/산출물 중 무엇을 가리키며 tuple digest에 이미 포함된 경로를 어떻게 안정적으로 유지하는가. 경로를 몰래 opaque ID로 바꾸면 기존 tuple digest가 바뀌므로 금지한다.
5. 식별자/직렬화/해시 도메인·오류 taxonomy·CAS 순서와 신규 바이트/자원 예산은 근거와 함께 동결한다. 임의 수치를 추가하지 않는다.

## 6. 실행 순서와 승인 경계

1. 본 방향 검토 및 설계 승인 — 아직 미승인.
2. 위 질문의 코드 실측, 정확한 package spec/행렬 작성. astra 독립 검토는 codex CLI 기동 승인 후에만 수행한다.
3. NO-GO 수정·human contract freeze 후 luna 제품 producer/결속/승인 소비부 구현. 파일 whitelist에 필요한 실제 crate만 포함한다.
4. format 부채를 변경 파일 범위에서 정리하고 부모 W3 인수 검증. 기존 실패 기록은 보존한다.
5. W4 durable publish 계약 구현 → W5 통합 → 전체 인수/실 GitHub 별도 승인/통합 sol 리뷰/최종 커밋·push.

최초 설계 초안 작성은 생산 코드 변경, 새 schema 적용, codex 기동, 실제 GitHub 호출을 포함하지 않았다. 후속 사용자 요청 1561383685238825074로 astra 읽기 전용 계약 조사를 수행했다.

## 7. Refinement v2 — astra REFINE 및 부모 대조

우선순위: 이 절은 위 초안과 충돌하면 우선한다. 구현 동결/승인이 아니다.
독립 원문: /home/cosmo/.hermes/cache/scratch/b1-product-tuple-astra.log 마지막 codex 응답. ASTRA_EXIT=0. 시작/종료/부모 현재 status 동일, diff-check 통과. 독립 검토는 정적 조사이며 새 테스트 결과가 아니다.

### 부모 직접 확인과 정정

1. **emit=원격 게시라는 전제 철회.** line/review_host.rs:3175–3230의 실제 SqliteReviewSink는 로컬 review_emit SQLite 기록이다. 따라서 앞의 '호출하면 외부 게시가 섞인다'는 설명은 이 구현에는 적용되지 않는다. exec 내부 단계명과 B1 W3/W4를 혼동하지 않는다. 제품 전용 경계의 근거는 저장소 소유권·원자성·권한·증거 의미다. 기존 sink를 제품 DB에 억지 연결하지 않는 결론은 유지한다.
2. **digest 직렬화 불일치 후보.** exec/types.rs:175–191은 typed GenerationCommit 직렬화, ledger/product.rs:4936–4966은 Value 재직렬화다. 부모가 두 경로의 차이를 확인했다. 비어 있지 않은 chain에서 필드 순서 차이로 digest가 달라진다는 astra 정적 발견은 신규 반례 시험으로 고정할 것; 부모는 아직 Rust 실행 재현하지 않았다. 신규 결과는 ArtifactTuple::digest(), 역사 digest는 구계약 그대로 분리한다.
3. **promotion wire 불일치 확인.** tuple_sqlite.rs의 promote는 EmitReceipt를 저장하지만 product.rs:3317–3327은 tuple 필드를 요구한다. 새 finalization을 key/tuple 양쪽 허용 decoder나 가짜 promotion으로 연결하지 않는다.
4. **host 결과 존재≠확정 가능.** product/review.rs:718–769에서 terminal run 상태와 numeric 결과 저장을 확인했다. held/resuming/실패와 성공 final을 구분하며 완료 후 claim 검사를 신규 stage validator와 혼동하지 않는다. completed claim lease 처리 세부는 후속 직접 대조 필요.

### 정밀 계약 후보 — 승인 요청용 방향

- 유일 writer ProductStore. caller가 tuple/commit/path를 제출해 등록하는 범용 API 금지. 기대 host key+host-result digest를 받아 저장된 exact 결과에서 재구성한다.
- 제품 결과 binding/approval side-binding을 additive하게 분리하고 standalone promotion/emit wire 및 DB 차단 유지.
- 기존 tuple key 외 lifecycle/activation을 result binding에 결속; 같은 tuple key의 다른 lifecycle/activation은 conflict. 새 allocator나 run 재번호화로 숨기지 않음.
- intent tuple generation과 fix-chain index는 다른 의미. numeric run과 journal_run의 기존 발급/예약을 재사용.
- 신규 확정은 현재 원본 source DriveReview 및 정확한 기존 work/인간 acceptance revision을 검사하는 안을 제안. stage 결과 보존 권한과 별개. 완료된 claim은 완료 증거로 검증하고 lease를 부활시키지 않음.
- 제품 승인부는 새 로컬 result binding을 exact 소비; legacy promotion fallback으로 신규 승인하지 않음. 역사 receipt/digest/원장 bytes 보존.
- 원격 target 증명은 W4. W3의 genuine host+fake executor+인증 router 인수를 실제 Die/GitHub 검증으로 확대하지 않음.

### 스펙 동결을 막는 미결정

A. DriveReview의 새 로컬 결과 확정 의미, 인간 revision ABA/credential rotation 후 pending 결과 recovery 권한.
B. waiting lifecycle을 P2 명시 시험 어댑터로 준비할지 제품 lifecycle producer를 범위에 넣을지. finalizer가 몰래 S1 전이를 추가하지 않음.
C. product v2→v3 명시 maintenance migration, 역사 승인 cutoff 및 신규 side-binding 누락의 corruption 분류. 버전/스키마는 제안이지 적용 완료가 아님.
D. result payload/hash domain/API와 artifact_location의 원래 evidence-path 의미. 기존 tuple digest 경로를 opaque ID로 몰래 치환하지 않음.
E. 복수 generation/continuation의 실제 stage script·inherited artifact 집합·정확한 호출 수. 아직 확인되지 않은 수치를 고정하거나 단언을 >=1로 약화하지 않음.

astra의 PT-01~PT-15 계약 및 시험 행렬은 독립 원문에 보존되어 있다. 제안 시험은 모두 미실행이며 정확한 서명/필드/권한/호출 수에 위 미결정이 남아 있으므로 이를 구현 준비 완료 스펙으로 승격하지 않는다. 다음 산출물은 이 질문을 닫은 package spec+traceability matrix이며 부모 실측 및 human freeze 전 luna 구현은 차단한다.
