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
- HoldCommand: 동일 run 재진입 식별자 부재 — b4 hold 세마포어 설계 시 판별
  필수 (현재는 구별 불가).
- IntentKey: activation·retry 변별 불가 — b2 실행기가 intent 재발행
  시점 정의와 함께 키 확장 결정.
- 미래 epoch 수용(fence)은 b1 명시 동작 — 활성 epoch 일치 조건과의
  상충은 b5 fencing 이중 증명에서 재검.
- 비환급 실행 횟수 예산(회수 불가 원칙)은 Budget과 별도 산정 필요.
