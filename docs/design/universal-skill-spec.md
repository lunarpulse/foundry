# Foundry Universal Skill Spec — 범용 실행 모델 (Universal Review Engine)

- **버전**: v0.5 (2026-10-12) — 소유자 판정 "범용실행 모델 확장" 반영
  전면 재작성. r1~r3(각 BLOCKER 2·2·2, MAJOR 8·8·7) 누적 반영 + r3
  BLOCKER-1(범용 실행 모델 부재) 해법. 재검증 대기.
- **의도(소유자)**: Foundry는 어떤 방법론도 돌릴 수 있는 유니버설
  플랫폼. BMad는 스킬 데이터의 첫 고객. **Rust 도메인에 방법론
  코드가 박히는 순간이 실패.** 엔진 고정·확장 선언(procedural macro
  원칙).
- **인수 기준**: BMad의 **자동 재계획 루프·defer·continuation을 포함한
  전 라우팅**이 팩 데이터로 표현되고, 팩 교체·라우팅 변경 시 **엔진
  diff 0**.

## 0. 측정된 기반 (불변 사실 — r2/r3 실측 확정)

### 0.1 InspectionFailed 실제 전이 (order.rs 283–294, MAX_ATTEMPTS=3)
| 조건 | 결과 |
|---|---|
| reason `no-op` 접두 | Failed (종단) |
| attempt ≥ 3 | Failed (종단) |
| 그 외 | attempt+=1 → **Queued** (Working 아님) |
- Inspecting에서 DieFailed 사용 불가. FurnaceHold 진입은
  InspectionPassed 유일. 엣지 21개(frozen_v1), 발행 주체 미검사.
- reason 이스케이프: `review:{n}‖` 프리픽스가 no-op 접두 충돌 차단.

### 0.2 기존 검사 커밋 3경로 (전환 대상+회귀)
line.rs 233 · line.rs 307(재진입 무조건 통과) · stream.rs 705.

### 0.3 인프라 제약·원본 고정
- dsl.rs 218: v3만 policy 해시. skill.rs 48: id/interface_version/kind/
  declares_tools 해시. SkillEnforcer(183): 도구 교집합,
  Advisory 저널 차단(190). append_validated(lib.rs 286): 단일 행.
- BMad 원본(sha256 전체): step-04=`170bbabde8a2a1172db409e56d1efa5e
  9d4f03ae8e7ea12c96d680c448d1aca6`, verif-gap=`45ab0b8c4c56c5c055c
  97a293a8591a67e256d0b1a2058ce1f093cd866be8d03`, customize=`1a62550
  6c1975d7ee36a85945ca6440f066ad66fdc4e288c1606150938b9217f`.

## 1. 계층 분리

| 계층 | 소유 | 금지 |
|---|---|---|
| 도메인(Rust) | frozen_v1 엣지·가드, 두 이벤트, review-run 증거, **§2 실행기(폐쇄 기계)**, 정규화 실행기 | 방법론 어휘·임계값·순서 |
| 팩(데이터) | 렌즈 지시문, 스키마·정규화 규칙, **액션 그래프(§2.2)**, 조건식, 예산값 | 코드 없음 |
| 호스트 | 디스패치 샌드박스(경로·심링크·프로세스·네트워크), 인간 API | 정책 해석 |

## 2. 범용 실행 모델 (r3-BLOCKER-1 해법)

### 2.1 핵심 구조 — 액션 그래프 = 상태 머신(데이터)

리뷰 실행은 **팩이 선언한 노드 그래프**로 모델링한다. 엔진은 그래프
실행기일 뿐 — 노드의 *의미*를 모른다.

```toml
# 노드 타입 5종 (폐쇄 집합 — 이것이 엔진의 전부)
[[nodes]]
id = "lenses"        # ① dispatch: 스테이지 그래프 실행(렌즈/검증기/재계획기 전부 동일 취급)
kind = "dispatch"
# stages = [ ... ]   # 팩의 §3.3 스테이지 정의 (부록 A 전체 예 — 여기선 생략)
on_done = "route1"

[[nodes]]
id = "route1"        # ② route: 조건식 → 다음 노드 선택
kind = "route"
[[nodes.branches]]   # 순서=우선순위, 첫 매치 후 중단
when  = 'budget_exhausted() || timeout()'
goto  = "human"
[[nodes.branches]]
when  = 'has_findings(any) && count_lt(fix_rounds, 3)'
goto  = "fixdie"     # ★ 팩 내부 fix 루프 — 도메인 attempt 불사용
[[nodes.branches]]
when  = 'has_findings(any) && count_lt(replans, 5)'
goto  = "replan"     # ★ 자동 재계획 — 도메인 백엣지 경유
[[nodes.branches]]
when  = 'has_findings(any)'
goto  = "human"
[[nodes.branches]]
when  = '!has_findings(any)'
goto  = "pass"

[[nodes]]
id = "fixdie"        # ③ redrive: 같은 Die workdir에 findings를 실어 재디스패치
kind = "redrive"     #    (라인 재주문 아님 — §2.4 시맨틱)
# redrive 계약(신규 범용 엔진 연산 — r4-MAJOR-2):
#  input: 이전 generation 산출물 경로 + payload(findings/수정 계획)
#  executor: 주문의 implementer 식별자(die harness)에 전달 — Harness
#    인터페이스 확장 없이 CLI 인자(--fix-context <file>)로 findings
#    파일 경로 전달(호스트 어댑터 소유, 포트 불변)
#  격리: generation별 workdir 분리(gen-N), 실행별 로그(attempt-N.log)
#  실패: exit≠0/timeout → on_redrive_error(팩 정책: hold|fail-run)
#  산출: 새 result_commit → 다음 검증의 artifact 결속 + 이전 증거
#    superseded 처리(§2.4a)
target = "die-workdir"
payload = "findings"
bump = "fix_rounds"
on_done = "lenses"   # 루프 폐쇄: lenses→route→fixdie→lenses…

[[nodes]]
id = "replan"        # ③' redrive의 특수형: replanner 지시문 실행 후
kind = "redrive"     #    emit_failed로 도메인 백엣지(새 Die 시도)
target = "die-workdir"
pre_stage = "replan-stage"   # 팩 정의 스테이지(KEEP 추출·계획 수정)
bump = "replans"
on_done = "emit_failed_node"

[[nodes]]
id = "emit_failed_node"   # ④ emit: 도메인 이벤트
kind = "emit"
event = "InspectionFailed"
payload_from = "cumulative_findings"
on_done = "HALT"     # 도메인이 Queued(attempt++)로 재주문 — 그 다음
                      # Inspecting 재진입 시 새 review-run이 저널의
                      # 이전 run을 이어받아 counters 승계(§3.6)

[[nodes]]
id = "pass"          # ④' emit(InspectionPassed)
kind = "emit"
event = "InspectionPassed"

[[nodes]]
id = "human"         # ⑤ hold
kind = "hold"
```

**노드 5종 = 엔진 전부**: `dispatch` / `route` / `redrive` / `emit` /
`hold`. `terminal_halt`은 `hold`의 파라미터(`notify_only=true`)로
흡수. 추가 노드 타입 = 스펙 버전업.

### 2.2 표현력 실증 — BMad 전 라우팅의 데이터 재현

| BMad 원본(step-04@hash) | 액션 그래프 표현 | 동등성 |
|---|---|---|
| 렌즈 독립 병렬+after | dispatch stages | 동등 |
| claims=같은 리뷰어 후속 | session 재사용+seal | 동등 |
| 트리아지 실측 검증 | verify 스테이지(팩 지시문) | 동등 |
| patch=같은 구현자 재호출 | redrive(die-workdir) — **동일 workdir 재디스패치로 세션·작업물 지속** | 동등(r3 §4 '변형' → v0.5 승격) |
| fix 루프(무제한이던 것) | fix_rounds 상한(팩 값) | 동등+상한 |
| intent_gap/bad_plan → **자동 재계획 5회** | replan 노드: pre_stage(KEEP 추출·계획 수정) → redrive(generation++) — 도메인 시도 불변 | **동등**(§2.4a) |
| intent_gap: 인간 해결 후 재계획 | hold 후 인간 API의 `resume`이 replan 노드로 진입 | 동등 |
| defer(기록 후 진행) | dispatch 노드의 `side_records = ["deferred"]` — 스테이지 출력에서 팩 정의 필드(`schema-defer.toml`)를 정규화해 outbox intent에 저널 기록(emit과 독립, 액션 문법 확장 아님) | 동등 |
| carried(재런 저널 대조) | verify 지시문에 저널 대조 입력 포함 | 근사 |
| quick/thorough/auto | selection(fixed/by-diff) | 변형 |

**폐쇄성 논증**: 5 노드는 그래프 완결(turing 불완전 의도 — 루프는
bump 카운터+상한 조건식으로만 폐쇄, 로드 검증이 **모든 사이클에
상한 카운터 경로 포함** 확인). 임의 방법론 = 그래프 모양의 차이.

### 2.3 조건식 (r3-MAJOR-3 완전 문법)

```
expr     := or_expr
or_expr  := and_expr ( '||' and_expr )*
and_expr := un_expr ( '&&' un_expr )*
un_expr  := '!' un_expr | primary
primary  := '(' expr ')'
         | 'true' | 'false'
         | all_in ( verdicts , set ) | any_in ( verdicts , set )
         | any_in ( findings.group , set )
         | count_lt ( id , int ) | has_findings ( group_id | 'any' )
         | timeout ( ) | budget_exhausted ( )
         | missing_field ( ) | ext_over ( int )

set      := '[' ( literal ( ',' literal )* )? ']'
literal  := string-literal(TOML 1.0 문자열)
id       := [a-z_][a-z0-9_]*     ; 팩 [counters]에 선언된 것만(로드 검증)
group_id := id | 'any'           ; 'any' 예약(팩 그룹 태그명 충돌 거부)
int      := u64 10진수

action   := 'emit' '(' event_id ',' payload_ref ')'
          | 'hold_for_human' '(' ')'
          | 'bump' '(' id ')'
event_id := 'InspectionPassed' | 'InspectionFailed'   ; 로드 검증
payload_ref := id             ; 노드/스테이지 출력 참조(로드 검증)
do_seq   := action ( ';' action )*     ; emit은 전 시퀀스에 최대 1회
```
- entry: 그래프 `entry` 필드가 지정(로드 검증: dispatch 노드).
  종단: emit 후 HALT, hold는 대기, 그래프 끝 도달은 로드 검증
  거부(모든 경로는 emit/hold로 끝나야 함).
- 우선순위 `!` > `&&` > `||`, 괄호 허용. `any`는 has_findings의
  **예약 와일드카드**(로드 검증에서 일반 태그명과 충돌 거부).
- **provenance(r3-MAJOR-4, r4-MAJOR-8)**: 조건식 바인딩은 엔진이
  결속한 스코프만 노출 — 팩 stages에서 `role = "verify"`로 선언된
  스테이지의 출력만 verdicts/findings.group 스코프가 됨(로드 검증:
  verify 스테이지 정확히 1개, 미선언 시 조건식에서 verdicts/
  findings 참조 거부). 원시 렌즈 출력은 has_findings에서도 불가.
  barrier 상태 5분류 상호 배타 정의: `no-valid-stage`/`all-error`
  상태에서 엔진은 **pass로 가는 branch 평가 자체를 거부**(정책
  필수: on_no_valid = hold|fail-run, 미선언 로드 거부) — `!
  has_findings(any)` 통과 새는 경로 원천 차단.
- `do` 시퀀스 문법: `action ( ';' action )*` — 다중 emit은 로드
  검증 거부(도메인 단일 전이 원칙).

### 2.4 fix 루프 vs 재계획 — 도메인 시맨틱 정확히 분리

| 루프 | 매커니즘 | 도메인 attempt |
|---|---|---|
| **fix 루프**(팩 내부) | redrive가 **같은 Inspecting 상태에서** 같은 Die workdir 재디스패치 — Die 재시작 없음, 이벤트 없음 | **불변** |
| **재계획**(BMad 5회) | pre_stage(KEEP·계획 수정) 후 redrive(새 **generation** 실행 — §2.4a) | **불변** (도메인 시도 아님) |

- 두 루프의 구분은 **팩의 그래프 설계가 소유** — 엔진은 redrive가
  emit을 동반하는지만 본다. "무한 재계획 없음"은 §2.4b 예산 차감
  우회 불가 규칙이 강제.

### 2.4a generation — 범용 실행 회차 (r4-BLOCKER 해법)

BMad의 "재계획 후 새 구현 실행"이 Foundry 도메인 시도(attempt,
MAX 3)와 충돌하는 문제: **새 구현 실행을 도메인 Die 시도가 아니라
review-run 소유의 범용 실행 회차(generation)로 정의한다.**

- redrive 노드는 실행 시마다 `generation += 1` — 같은 주문·같은
  Inspecting 상태에서 Die 프로세스를 새 파라미터(수정된 태스크·
  findings)로 재실행하는 것은 **review 엔진의 내부 실행**이다.
  도메인은 Inspecting에 머문다(이벤트 없음).
- workdir: generation별 분리(`workdir/gen-N/`) — 이전 세션 산출물
  오염 차단 + 아티팩트 A→B 교체 시 **이전 review 증거 무효화**
  (run 저널에 `superseded_by=gen-N` 기록, 다음 검증은 B에 결속).
- 라인 재주문 경로(Queued→새 주문 시도)는 오직 emit(InspectionFailed)
  로만 — 팩이 그래프에서 emit을 쓸 때만 도메인 시도가 소모된다.
  BMad 5회는 전부 generation으로, 도메인 시도 소모 없이 표현된다.
- **인과 보존**: generation 산출물이 최종 emit(InspectionPassed)을
  받으면 artifact_digest는 **그때의 generation HEAD**에서 결속
  (finish_die_segment 패턴 승계) — 어떤 generation 결과가 발행되는지
  원장이 정확히 가리킨다.
- 비용 통제: generation마다 [run] 예산 차감(§2.4b).

### 2.4b 유한성 강제 (r4-MAJOR-3)

- **모든 redrive/dispatch 노드 실행은 예산을 차감**하고, 소진 시
  엔진이 해당 노드 진입 자체를 거부(→ hold). 조건식의 count_lt와
  무관하게 **우회 불가** — "상한 경로 포함"이 아니라 진입 차단.
- 카운터: u64, run 생성 시 0, bump만 증가, 감소 없음, overflow 시
  엔진 hold(래핑 방지). run 간 승계는 §3.3a(주문 단위 누계).
- 그래프 로드 검증: 모든 사이클 경로에 {예산 차감 노드} 포함 —
  redrive/dispatch 외 노드만의 사이클은 존재 불가(route는
  이동만, emit/hold는 종단·HALT)이므로 구조적으로 자동 충족.

### 2.5 정규화 (r3-MAJOR-7)
- 4분류(ok/parse_error/dispatch_error/timeout)별 정책표 — 팩이
  **키·허용값·기본값 전부 선언**(엔진 기본값 없음, 미선언 시 로드
  거부):
```toml
[[output.on_error]]        # 순서=우선순위
state = "dispatch_error"   # ok | parse_error | dispatch_error | timeout
policy = "discard-stage"   # discard-stage | hold | fail-run
[[output.on_error]]
state = "parse_error"
policy = "discard-finding" # 개별 finding 거부(부분 거부)
```
- barrier 집계 상태 5분류: `all-ok-empty`(정상 빈)·`all-ok-nonempty`·
  `valid-partial`(일부 폐기+유효 존재)·`no-valid-stage`(유효 0 —
  **on_error 정책 우선, 통과 아님**)·`all-error`. `missing_field`의
  필수 집합·`ext_over` 단위(bytes)·markdown 그룹 규칙 형식은
  정규화 스키마 표준 템플릿(§부록 A, 구현 시점 확정)에 명시.

## 3. 실행 계약

### 3.1 팩 로드·digest (r3-MAJOR-9 완결)
- pack digest canonical bytes(독립 구현 동일성 보장 — r4-MAJOR-9):
  `input = Σ (for file in sorted-by(UTF-8 bytes of POSIX path)):
  u64le(len(path)) ‖ path ‖ u64le(len(content)) ‖ content` (파일은
  매니페스트 참조 폐쇄 + manifest.toml 자체; manifest의 digest
  필드는 **해당 줄 제거 후** 직렬화 — 줄 단위 정확 명세; NFC 유니코드
  정규화 아님(원본 바이트), 경로 중복 거부). `pack_digest =
  SHA256(input)`. v4 leaf = SHA256(u64le 각 len‖
  "step_id","pack_id","pack_version","enforcement","pack_digest" 결합).
  **fixture 1개(최소 팩: 빈 lens 1개+최소 policy)의 예상 digest를
  구현 전에 독립 계산해 부록에 고정** — 구현이 이를 재현해야 함.
- 2회 검증(로드·디스패치 직전) + **불변 스냅샷**(run-dir 복제) 실행.
  스냅샷 영수증 → VerifiedSkill + Merkle 결속.
- 프리셋 v4: `review_pack` 필드 + Merkle v3 정책 해시 유지 +
  review-pack leaf((step_id, pack_id, pack_version, enforcement,
  pack_digest)의 canonical bytes — 리프 스키마 이식). v3 공존(로더
  버전 분기), v3 golden root 불변 회귀. `advisory-pack` kind는
  SkillKind::Advisory의 저널 차단(190) 승계 + role 배선 불가.

### 3.2 review-run (락 밖)
1. run 생성: order·attempt·artifact·pack digest·스냅샷 결속,
   멱등키=run_id, **카운터 스냅샷**(이전 run의 주문 누계 승계 —
   §3.6).
2. stages: session 참조(`session="trace"` = 같은 리뷰어), seal(선행
   완료 전 개봉 불가 — 호스트 경계), 같은 session 병렬 stage는
   **직렬화 강제**(같은 세션 동시성 모순 — 로드 검증 거부), barrier
   후 라우팅. 순환·미정의 참조 로드 거부. 각 스테이지는 `role`
  선언(예: "lens" | "verify" | "replan") — verify 프로바넌스는
  §2.3이 소유.
3. 격리: SkillEnforcer(팩 declares_tools에 최소 읽기 역량 포함) +
   호스트 샌드박스. findings 비신뢰.
4. **원자성(r3-MAJOR-5, r4-MAJOR-6)**: 이중 저장(orchestration 로그
   + stream ledger)은 단일 트랜잭션으로 못 묶는다 — **transactional
   outbox**: 모든 액션은 ①내구 intent(action_id, run·node·generation
   결속) 먼저 기록 ②외부 실행 ③결과+완료 표시 커밋. 재시작 시
   intent 있고 완료 없는 action만 재실행. 외부 실행(redrive의 Die
   프로세스)은 **절차 재실행 안전**하게 설계: generation별 분리
   workdir이므로 재실행은 새 gen 디렉터리 생성(중복 부작용 없음 —
   이전 gen은 고아로 남고 저널 superseded). emit(도메인 이벤트)은
   outbox 소비 단계에서 단일 stream ledger 트랜잭션으로 — 중복
   소비는 action_id 멱급 키로 차단. action_id = run_id#node_id#
   generation(u64) — 크래시 후에도 결정적.
5. **디스패치 fencing(r3-MAJOR-6, r4-MAJOR-7)**: (a) 응답 차단 —
   stage별 dispatch generation + 활성 실행 토큰 내구 기록, 응답
   수용 시 generation·토큰·run·attempt·artifact를 **단일 수용
   트랜잭션**에서 재검사(원자 경계). (b) **쓰기 차단** — 구 세대
   프로세스의 workdir 오염은 generation별 디렉터리 분리로 원천
   차단(구 프로세스는 구 gen 디렉터리에만 씀 — 새 gen과 무결함).
   프로세스 종료 확인(토큰 폐기)은 best-effort + 타임아웃 강제
   kill, 잔존 구 gen 결과는 폐기. pre_stage 포함 **모든 외부
   디스패치는 동일 dispatch 계약**(격리·예산·fencing·오류 정책).
   pre_stage 실패 시: bump/emit 실행 없이 on_redrive_error 정책.
   완료 stage 출력은 원본 보존(run-dir 파일+digest). 세션 복원
   실패 → 새 run 시 이전 run 무효화(저널) + 예산 누계 승계.

### 3.3 인간 API (r3-MAJOR-8)
- hold 진입: 내구 `awaiting_human` + deadline. **유한 deadline 만료
  효과**: 저널에 `human_deadline_expired` 기록 + remind 재발송 +
  **상태 유지**(자동 결정 없음 — 종료는 인간만; 무한 대기는
  `[hold] timeout_s = 0` 명시 설정, 생략 시 로드 거부 — "명시적
  운영 정책"의 강제형).
- 인간 선택: `commit_pass` / `commit_fail` / `resume(node_id)` /
  `abort`. 결속 재검사(run·attempt·artifact)와 커밋이 **동일 원자
  경계**(§3.2.4). API 멱등(중복 응답 폐기 — action_id).
- `resume(node_id)`: **팩이 hold 노드에 선언한 허용 목적지 목록**
  (`resume_targets`)으로 제한(로드 검증: 그래프 내 노드·사전조건
  노드 완료 여부 런타임 검사 — 우회 시 거부). 인간 결정은
  `hold_instance_id` 미소비 상태를 **동일 트랜잭션에서 CAS 소비** —
  두 응답 경쟁 시 하나만 승리, 늦은 응답은 hold_instance 소멸로
  폐기(검증자 지적 경합 경로 차단).

### 3.3a 카운터·세션 승계 (구 §3.6)
- 주문 단위 누계(fix_rounds·replans 등): run 저널에서 주문 최신 run
  의 counters 스냅샷을 신규 run 생성 시 승계(도메인 attempt와 별개
  수명). 승계는 run 생성 트랜잭션에 포함(중복 승계 방지).
- 세션: 완료 stage의 세션 식별자·출력 원본 보존(§3.2.5). 재시작 시
  미완료만 재디스패치, 같은 session 필요 시 세션 복원 불가능하면
  새 run(이전 run 무효화 + 누계 승계).

### 3.4 reason 인코딩·관측 분리 (승계)
`review:{n}‖` 프리픽스(엔진 인코더). 알림 무차단. required 증거
저장 실패 = 커밋 불가(원자성과 동일 경계).

## 4. 구현 순서 (M8)
1. **로더+digest+스냅샷+그래프 파서+조건식 파서+정규화+셀프테스트**
   (위임) — 스펙 예제 전체 TOML+DSL 파싱·타입 검증 golden
2. **그래프 실행기+원자성(append_validated_batch)+fencing**(직접)
   — 노드 5종만
3. `skills/bmad-review/v1` — 렌즈 3+검증기+**액션 그래프(BMad 전
   라우팅: fix 루프+자동 재계획 5회+defer)** 이식
4. e2e: 실 PR → 렌즈 → fix 루프 → 재계획 → hold → 인간 API 실주행
5. `skills/plain-cr-review/v1` — 인수: **라우팅·루프·어휘 전부
   다른데 엔진 diff 0**

## 부록 A (구현 시점 확정 예정)
정규화 스키마 표준 템플릿(markdown 그룹 규칙 형식·필수 필드 집합)·
golden digest 목록·dispatch 스테이지 TOML 전체 예.
