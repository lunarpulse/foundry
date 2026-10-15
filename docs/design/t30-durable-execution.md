# T30 — 내구 실행 그래프: 트랜잭션 소유자 + 장애 주입 표

작성: 2026-10-14 · 기준: M8 Plan v2 T30, USS v0.71 FROZEN
상위: docs/design/universal-skill-spec.md §2 (팩 그래프 규범)

본 문서는 T30 구현의 **설계 정본**이다. 저장소별 트랜잭션 소유자를
먼저 고정하고, 크래시 주입 창을 표로 확정한다. 구현 배치(b1~b5)는
§4의 분해를 따르며 각 배치는 edit-spec → codex 위임 → 부모 검증으로
진행한다(2026-10-14 소유자 지시: 생성/삭제 파일 리포트 + temporaries
정리 확인 포함).

## 1. 저장소 인벤토리와 트랜잭션 소유자

| # | 저장소 | 현재 구현 | 단일 쓰기 소유자 (트랜잭션 주) | 원자성 단위 |
|---|---|---|---|---|
| S1 | SQLite 원장 (`foundry-ledger/sqlite.rs`) — append-only 해시체인, UPDATE/DELETE 트리거 금지 | 주문 상태전이 (from/to/event/actor/intent + prev_hash) | **Ledger port 구현체만** (주문 상태머신 전이 기록). 실행기는 직접 SQL 금지 | 1행 INSERT (해시체인 결속) |
| S2 | 오케스트레이션 로그 (`foundry-orch/lib.rs` OrchestrationLog) | 거부 레코드(3요소)·디스패치 기록 | **코디네이터만** (dispatch/recovery/enroll 경로) | 1레코드 append |
| S3 | 스트림 저널 (per-stream 파일 — `stream.rs` Stream::journal_path, id에서만 파생) | 워크플로 엔진 이벤트 | **WorkflowEngine만** (같은 스트림 1 엔진 — 활성화 1:1) | append + verify |
| S4 | 세션 저장소 (Die 리뷰어 세션 — 논리 구현자 세션 계승) | 라이브 디스패치 경로 (T32에서 실주행) | **호스트 dispatch/redrive 어댑터만** (r1-6: 물리 실행 격리) | 세션 복원 영수증 1건 |
| S5 | git refs (publish 브랜치·태그 — publish-bridge) | 발행 승격 | **publish 경로만** (FurnaceHold 승인 후). fencing: 구 epoch 쓰기 격리 별도 증명 | ref 1개 CAS (update-ref) |
| S6 | 산출물 저장소 (작업 디렉터리 → 승격 산출물) | 임시 경로 추론 (금지 대상 — r1-8) | **산출물 승격 경로만**. downstream은 **튜플만** 소비 | 튜플 1행 기록 + 승격 |

**규칙**: 각 저장소의 쓰기 경로는 표의 소유자 단 하나. 다른 모듈은
읽기 또는 소유자 API 경유만. 교차 쓰기 발견 시 구현 배치 거부 사유.

## 2. 크래시 주입 표 (장애 창 전수)

각 창은 `cfg(test)` 주입 훅(atomic 래치 — t61_kill9 패턴 계승)으로
재현하고, 복구 주장은 **양 저장소 복구 실측**(S1+S3, S5+S6)으로 증명한다.

| 창 | 주입 시점 (직후 크래시) | 위험 | 기대 복구 동작 | 시험 |
|---|---|---|---|---|
| W1 | 최소 intent 기록 **후**, 외부 실행 시작 **전** | 이중 실행 (재시작이 이미 실행됐다고 착각) | intent에 실행 개시 표시 없음 → 외부 실행 재개 (아직 실행 안 함이 증명됨). 이미 개시 표시면 결과 대기로 진입 | t30_w1 |
| W2 | 외부 실행 결과 수신 **후**, 결과·커서·다음 activation 결합 커밋 **전** | 결과 유실·커서 퇴행 | 미결합 결과 감지 → 결합 재수행 (외부 재실행 아님 — 결과 영수증이 이미 S4/S6에 있으므로) | t30_w2 |
| W3 | 산출물 승격 영수증 발급 **후**, emit 커밋 **전** | 미발행 승격 (고아 산출물) | 승격 영수증 존재 + emit 부재 → emit 재수행 (멱등: 영수증이 emit 자격 증명) | t30_w3 |
| W4 | emit 커밋 **후**, 오케스트레이션 완료 **전** | 완료 누락 (무한 재개) | emit 영수증 존재 → 완료 처리만 재수행 (외부 효과 재발행 금지) | t30_w4 |
| W5 | 산출물 튜플 기록 **후**, downstream 소비 **전** | 경로 추론 폴백 유혹 | downstream은 튜플만 읽음 — 튜플 있으면 소비, 없으면 대기 (경로 스캔 금지가 구조적으로 강제) | t30_w5 |

### 중복·지연 emit 규칙 (전수)
- **중복 emit**: 동일 (order, attempt, run, generation) emit 재요청 →
  외부 효과 없이 **기존 영수증만 반환** (멱등).
- **지연 emit 거부**: 영수증 없는 **구 epoch**의 최초 emit → 거부.
  fencing 통과(현 epoch) + 영수증 부재 = 최초 emit만 허용.

### fencing (r1-5 별도 증명)
1. **응답 거부**: 구 epoch 실행의 결과·이벤트 제출 → epoch 불일치로 거부.
2. **쓰기 격리**: 구 epoch이 (a) sibling 산출물 (b) git refs (c) 세션
   저장소에 **쓰지 못함**을 각각 별도 시험 (응답 거부와 독립 증명).

## 3. 불변식 (구현 배치 공통 인수 조건)

- I1 외부 실행 전 intent 필수 (intent 없는 외부 효과 = 위반)
- I2 결과·커서·다음 activation은 **한 트랜잭션**으로 결합 (부분 결합 금지)
- I3 산출물 승격은 영수증 동반 (영수증 없는 승격 금지)
- I4 emit 정확히 1회 (멱등 영수증 반환 허용, 외부 재발행 금지)
- I5 주문 단위 비환급 예산: 예약·정산·누계 승게 — 크래시 후 잔여 예산
  소멸 금지 (재시작은 run 신규 예약, 소진 우회 거부)
- I6 hold 4연산(approve/reject/amend/info) 전부 `hold_instance_id` CAS +
  증거 결속 + resume 목적지 제한 + human_resolution 전달
- I7 내구 산출물 튜플 `(order, attempt, run, generation,
  immutable_commit, artifact_location)` — 원자 기록, downstream 전용 인터페이스

## 4. 구현 배치 분해 (위임 단위)

| 배치 | 내용 | 신규/정정 | 소유 저장소 |
|---|---|---|---|
| b1 | 내구 실행 그래프 코어 타입 — intent·activation·커서·영수증(emit/승격/세션)·튜플·epoch + 트레이트 (신규 크레이트 `foundry-exec`) | 신규 | 없음 (타입만) |
| b2 | 노드 5종 실행기 (dispatch/route/hold/redrive/emit — 팩 그래프 해석·foundry-pack 의존) + W1~W4 주입 훅·복구 | 신규+정정 | S3 읽기, S1 소유자 API |
| b3 | 호스트 dispatch/redrive 어댑터 + Die 래퍼 프로토콜 (`--fix-context`·입력 manifest·latest/origin base) — BMad ID 분기 없이 팩 선언만 | 신규 | S4 |
| b4 | 내구 hold/인간 API (4연산 CAS·증거·resume 제한·human_resolution) + 비환급 예산 | 신규+정정 | S1, S2 |
| b5 | 내구 산출물 튜플 원자 기록·승격 + downstream 소비부 전환 준비 (replay·rehydrate·승인·publish) + W5·fencing 별도 증명 | 신규+정정 | S5, S6 |

각 배치 종료 조건: §2 해당 창 시험 green + §3 해당 불변식 시험 +
cargo test 전체 green + 생성/삭제 파일 리포트 정합 (부모 대조).

## 5. 의존 규율

- `foundry-exec` → `foundry-pack`·`foundry-domain`·`foundry-ledger`(port) 허용.
- `foundry-pack → foundry-exec` 등 역방향 금지 (T29a r1 권고 계승).
- BMad 고유 어휘(ID·verdict·상한 수치)는 팩 데이터 — 실행기 코드에
  분기 없음 (엔진 diff 0, T34 게이트 전제).

## b1 리뷰 지연 항목 (b2/b4 계약 정합 필요 — 2026-10-14 gpt-6.1-sol 리뷰)
- HoldCommand 재진입 식별은 b4에서 종결: HoldInstanceId의 run·hold_node·entry와
  영속 Started/Completed 방문 이력으로 같은 run의 재진입을 구별한다.
- IntentKey 변별은 b2에서 종결: run 조회 키와 별도로 activation을 보존하고,
  IntentLog::get과 OutcomeStore::get이 활성화별 재시도를 식별한다.
- epoch 정책 충돌은 b5에서 종결: epoch < active 최초 제출 거부,
  epoch >= active 수용, 기존 emit 영수증 재사용. 쓰기 격리의 실제 어댑터
  통합은 미종결이며 아래 b5 결정의 검증 경계를 따른다.
- 비환급 실행 횟수는 b4/b5의 AttemptLedger로 Budget과 별도 산정한다.
  b5 edit-spec에 따라 (order, attempt, run) 멱등 키, (order, attempt)당
  max를 적용하고 consumed(order)는 모든 pair의 합계다. I5의 nano 예산은
  계속 주문 단위이며 실행 횟수의 pair당 상한과 구별한다.

## b2a 리뷰 지연 항목 (b3 계약 정합 필요 — 2026-10-14 gpt-6.1-sol 리뷰)
- mark_started 직후·host 결과 접수 전 종료 창: 접수 증거가 불명인 경우
  AwaitResult로 대기한다. host가 결과를 실제로 받았는지 판별하는 b3 HostLedger::is_accepted/load와 DispatchHost::accepted/
  recover_result가 구현되었다. 접수 부재가 증명된 경우만 재dispatch하고,
  접수됐으나 결과가 없거나 legacy 호스트이면 AwaitResult로 대기한다.
  이 항목은 접수 조정에 한해 종결이며 물리 세션 fencing의 증명은 아니다.

## b2b 리뷰 지연 항목 및 종결 현황 (2026-10-14 gpt-6.1-sol 리뷰)
- 드라이버 상태 복원은 b2c에서 종결: CounterLog의 Started(activation 포함)/
  Bump/Completed를 append 순서로 재생하고, OutcomeStore의 continuation과
  결합하여 GraphDriver::restore가 카운터·커서·미완료 방문을 복원한다.
  메모리 전용 상태라는 기존 blocker는 현재 구현에 해당하지 않는다.
- epoch 정책 충돌은 b5에서 종결했으나 실제 sibling/ref/session 어댑터의
  쓰기 격리는 미종결이다. 아래 b5 결정 참조.

## b2c 리뷰 지연 항목 (2026-10-15)
- E_DAG_CYCLE: 순환 거부는 팩 스펙 소유다. 로더가 E_DAG_CYCLE로
  순환을 거부하는 것을 정규 입장으로 확정하며 엔진 내 순환 지원 계획은
  없다. 방문별 기록은 드라이버 직접 구성 시험으로 검증한다.
- activation 없는 구형 카운터 로그는 자동 복원 대신 Fault 중단
  (b2c-2 계약 이전 로그는 미지원 — 자동 마이그레이션을 하지 않기로
  결정했다. activation 이력을 모호하게 만드는 구형 host 방문 복구는
  Fault 중단을 유지하며 모든 구형 route-only 로그를 무조건 거부하지는 않는다).

## b5 통합 결정 — 튜플·epoch·fencing
- **I7 필수 연결**: GraphDriver의 emit 노드와 route emit action은
  with_tuple_store 연결이 없으면 외부 발행 전에 Fault한다. 비emit 그래프는
  기존 생성자를 사용할 수 있다. emit 성공은 record와 promote(emit 영수증
  동반)가 모두 성공한 뒤에만 완료한다. record의 동일 key·동일 값은 멱등,
  immutable_commit/artifact_location 불일치는 Permanent로 거부한다.
- **W3/W4/W5 복구**: W3는 승격 영수증 이후·emit 이전, W4는 emit 커밋 이후·
  튜플 기록 이전, W5는 튜플 기록·승격 이후·소비 이전이다. 기존 orchestration
  영수증 경로도 record/promote를 재수행한 뒤 완료하며 외부 sink 조회·발행은
  하지 않는다. 따라서 emit 성공 후 튜플 저장 실패도 재시작으로 보완한다.
- **S6 경계**: TupleStore는 원본 순서 조회 `latest(order, attempt)`와 소비 자격
  조회 `latest_promoted(order, attempt)`를 분리한다. 소비자는 record와 emit 영수증
  promote가 모두 끝난 튜플만 받으며, record-only 튜플은 소비할 수 없다.
  저장소 파일 경로나 스캔 메서드는 없고, latest는 (run, generation) 최댓값을
  반환한다. 튜플의 artifact_location은 계약 데이터이며 저장소 파일 경로와
  구별한다. 경로 스캔 폴백 API와 영수증 없는 promote는 compile-fail 시험으로
  거부를 확인한다. promote는 emit 영수증의 S6 결속이며 실제 S5 ref 쓰기와 다르다.
- **내구성 증거의 범위 (T31 갱신)**: `foundry-exec::SqliteTupleStore`는
  `BEGIN IMMEDIATE` 트랜잭션과 `synchronous=FULL`로 record/promote를 저장하고,
  SQLite 파일 잠금에 다중 프로세스 직렬화를 맡긴다. latest와 latest_promoted는
  읽기 트랜잭션에서 각각 전체 튜플과 승격된 튜플 중 `(run, generation)` 최댓값을
  반환한다. 참조 파일 어댑터 테스트는 기존 크래시
  창 계약 확인용으로 유지한다.
- **epoch 규칙과 포트**: epoch < active 최초 결과·이벤트 제출과 쓰기를
  거부하고 epoch >= active를 수용한다. 기존 emit 영수증은 epoch와 무관하게
  멱등 반환한다. EpochGate::with_epoch는 주문 소유권 잠금 안에서 stale을
  거부하고, active=max(active,presented)를 효과보다 먼저 영속화하며 callback
  종료까지 잠금을 유지하도록 계약한다. 실패한 callback도 epoch를 낮추지 않는다.
  포트 참조 어댑터로 sibling/ref 모델/session 파일 쓰기 격리를 각각 독립 시험하고,
  재시작·쓰기 실패 후 단조 증가도 검증한다. 단순 check 후 잠금 밖 쓰기는 금지다.
- **미종결 경계**: 실제 S4 세션·S5 git refs·sibling 쓰기 소유자가 이 포트를
  호출하는 제품 경로는 이 배치 범위 밖이다. 위 3종 시험은 참조 어댑터 계약
  증거이며 실제 git CAS·호스트 세션 물리 격리의 통합 증거가 아니다. EmitLog의
  active epoch 갱신 및 Executor의 host 결과 제출도 실제 소유자의 fencing과
  직렬화가 필요하다. b5에서 epoch 정책 결정은 종결했지만 이 제품 통합까지
  완료했다고 주장하지 않는다. 외부 어댑터의 격리·단조 증가 통합 검증은 남는다.

## T31 — 리뷰 증거 게이트와 소비자 연결 (2026-10-15)
- `DurableReviewRun<S, L>`은 IntentLog append → `DispatchHost<Result=HostResult>`
  실행 → exit code 및 base/result commit 결속 검증 → 성공 tuple emit/record/promote
  순서를 소유한다. 기각과 host 오류, commit 불일치는 증거를 만들지 않고 실패한다.
- Line의 일반 완료는 no-op taxonomy를 유지하며, 변경 산출물의
  `InspectionPassed`는 ReviewRunPort의 통과 증거와 latest tuple/digest 결속을
  확인한 뒤 기록한다. Inspecting 재진입은 일치 tuple이 없으면 Inspecting에 머문다.
- Line 재진입·승인·publish 및 rehydrate는 latest_promoted `(order, attempt)` 튜플의
  `immutable_commit == artifact_digest`를 요구한다. orch의 리뷰 통과 재생도
  동일 확인을 하며 리더 미주입 시 거부한다. 구 저널에 대한 마이그레이션은 없다.
- 제품 튜플 저장은 SQLite로 제공한다. 기본 경로는 `FOUNDRY_TUPLES` 또는
  `~/.foundry/tuples.db`다. 제품 review host는 T32 소유이며, 현재 Line 기본
  ReviewRunPort는 미주입 상태라 fail-closed다.
- EpochGate의 실제 S4/S5/sibling 제품 배선은 T32 이후 범위다.

## T32 — 그래프 정책 실행기와 제품 review host (독립 리뷰 반영)
- `GraphEngine`은 팩의 선언 순서·카운터·hold/resume·오류 이송을 실행한다.
  dispatch/redrive 진입 전에 budget/timeout을 차단하며, route 내 inline
  emit도 독립 emit 노드와 동일한 verify 적격성 검사를 받는다. `run()`은
  순수 정책 시뮬레이션이다. 외부 실행 증거는 host가 주입하는 staged 경계 소유다.
- `PackReviewHost`의 라우팅 입력은 verify 출력만이다. 각 stage는 선행 완료가
  내구 기록된 출력 경로만 `inputs`로 받으며, 독립 렌즈에 앞 렌즈의 로그를
  fix-context로 전달하지 않는다. verify의 `inspected_commit`은 요청 result
  commit과 일치해야 한다. 출력 원본·SHA-256·세션 영수증·팩 스냅샷을 보존한다.
- stage별 `exec/<activation>-<stage-sequence>/0/` namespace를 사용한다.
  수용된 동일 activation은 재실행하지 않으므로 retry는 0뿐이다. 향후 같은
  activation의 재시도를 구현하면 retry도 새 namespace여야 한다. executor와
  세션 복원은 해당 namespace 안에 쓰기를 한정하는 어댑터 계약을 따른다.
  실제 프로세스 샌드박스/Die executor는 T33 소유이며 제품 기본값은 여전히
  `UnavailableStageExecutor`다. 주문 ID는 경로에 직접 사용하지 않는다.
- 정상 stage 오류는 barrier/팩 오류 정책으로 hold한다. 저널/스냅샷 등 host
  오류는 `review_aborted`와 진단·카운터를 남긴다. 같은 수용 activation은
  재디스패치하지 않는다. 중단된 run은 새 activation으로 새 run을 만들 수
  있다. 모호한 크래시로 `running`인 run 교체는 더 높은 epoch만 허용한다.
  held run은 새 activation으로 우회할 수 없다. 주문 최신 run의 카운터는
  invalidated/aborted 상태도 포함하여 신규 run 생성 트랜잭션에서 승계한다.
- **Line 경계**: 동결된 T31 Line은 모든 runner `Err`를 `InspectionFailed`로
  해석한다. 제품 CLI/serve는 `InspectingReviewRun`으로 runner를 감싸 오류 시
  fail-stop(panic)하여 이 분기로 돌아가지 않는다. 따라서 pack hold/fail-run이
  도메인 시도를 소모하지 않고 Inspecting에 남는다. 정상 통과는 그대로 반환한다.
  이는 정상적인 인간 대기 API가 아닌 임시 중단 경계이며, 해당 실행 task와
  runner mutex는 중단/poison될 수 있다. hold-aware Line 결과 어댑터가 생기기
  전까지 복구에는 저널 확인과 런타임 재시작이 필요하다. raw
  `durable_review_run`의 `Err`는 Line에 직접 연결하지 않는다.
- 외부 stage 시도는 실행/세션 복원 전에 주문 단위 비환급 횟수 예산을
  SQLite에서 1씩 예약한다. 현재 host 운영 상한은 주문당 10,000회다.
  새 run·도메인 attempt·세션 무효화가 이 누계를 초기화하지 않는다.
- review DB의 epoch는 주문별 단조 증가한다. scoped EmitLog가 이를 읽고,
  sink는 활성 run/activation/epoch·artifact 결속을 영수증 INSERT와 같은
  트랜잭션에서 재확인한다. 기존 emit 영수증은 epoch 전진 후에도 재사용한다.
  defer side record는 verify 입력으로 평가하고 pass emit 영수증과 함께 한 번만
  기록한다. 이 DB 검사는 T31의 이후 범위인 제품 EpochGate 전체 배선을
  대체하지 않는다. 현재 Line의 요청 epoch는 0이며 자동 크래시 재개 API는 없다.
- **redrive 제한**: T31 `DurableReviewRun`의 고정 요청/결과는 원 result commit과
  generation 0을 결속한다. T32 host에는 새 artifact를 전달하는 Die 경계가
  없다. 따라서 redrive는 무실행 성공을 만들지 않고 `on_redrive_error`를 따른다
  (canonical 팩은 hold). 실제 재계획/수정/새 generation 통과는 T33에서 T31
  위의 artifact handoff 어댑터를 구현하기 전까지 제공하지 않는다. revert
  hold 기록은 `requested_revert`와 `artifact_mutation_applied=false`로 구별한다.
  인간 hold API/CAS도 이 host가 아직 제공하지 않는다.
- 바이너리 provenance는 실행 경로·SHA-256·명시적 full source commit·pack
  digest다. 빌드 시 `FOUNDRY_SOURCE_COMMIT` 또는 임베더의 명시적 값, 없으면
  실행 환경의 같은 변수를 요구한다. 실행 cwd의 git HEAD는 사용하지 않으며,
  출처 미설정은 외부 실행 수용 전에 실패한다. 옛 저널의 누락 증거를 추정하거나
  마이그레이션하지 않는다.
