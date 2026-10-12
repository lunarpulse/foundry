# Foundry Universal Skill Spec — 범용 실행 모델 (Universal Review Engine)

- **버전**: v0.15 (2026-10-12) — r13 REWORK(MAJOR 6+MINOR 2) 반영 +
  독립 이중 검증 도입. 재검증 대기.
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
# stages = [ ... ]   # 팩의 §3.3 스테이지 정의 (부록 A 전체 예 — 여기선
#                  생략; verify 스테이지가 triage 카테고리 verdicts 출력)
on_done = "route1"
hold_target = "human"   # r9-3: 엔진 주도 hold 이송 필수 필드

[[nodes]]
id = "route1"        # ② route: 조건식 → 다음 노드 선택
kind = "route"
# (r10-1/2) 문서 예제는 FULL 팩(skills/bmad-review-full/v1/policy.toml)
# 의 route1과 논리 동일 — 원본 cascading 순서 intent_gap > bad_plan >
# patch > defer, 상한 초과는 하위 누수 없이 human:
[[nodes.branches]]
when  = 'budget_exhausted() || timeout()'
goto  = "human"
[[nodes.branches]]   # (r11-1b) 공통 loopback 상한 — 원본 "loopback 전
when  = '(any_in(verdicts, ["intent_gap", "bad_plan"])) && !count_lt(loopbacks, 5)'
goto  = "human"      # increment 후 5 초과 HALT — replan resume 불가
[[nodes.branches]]   # intent_gap: 인간 해결 후 계획 재실행
when  = 'any_in(verdicts, ["intent_gap"])'
goto  = "human_gap"  # 전용 hold — resume("replan")만 허용
[[nodes.branches]]   # bad_plan: 롤백+재계획(상한 5)
when  = 'any_in(verdicts, ["bad_plan"]) && count_lt(loopbacks, 5)'
goto  = "replan"
[[nodes.branches]]   # patch: 같은 구현자 최소 수정(상한 3)
when  = 'any_in(verdicts, ["patch"]) && count_lt(fix_rounds, 3)'
goto  = "fixdie"
[[nodes.branches]]   # defer: 기록 후 진행 — findings 없을 때만 pass
when  = 'any_in(verdicts, ["defer"]) && !has_findings(any)'
goto  = "pass"       # side_records=deferred 저널(§2.2 표)
[[nodes.branches]]
when  = 'has_findings(any)'
goto  = "human"
[[nodes.branches]]
when  = '!has_findings(any)'
goto  = "pass"

# 마지막 branch는 반드시 true 폴백(§2.3 로드 검증 요건 — r6-5):
[[nodes.branches]]
when  = 'true'
goto  = "human"

[[nodes]]
id = "fixdie"        # ③ redrive: 같은 논리 구현자에게 findings를 실어
kind = "redrive"     #    재디스패치 — 새 generation workdir(물리 격리,
                      #    §2.4a) — 라인 재주문 아님
# redrive 계약(신규 범용 엔진 연산 — r4-MAJOR-2):
#  input 규약(r5-4, r6-3): workdir/gen-N/in/manifest.json — {base_commit,
#    payload_path, payload_digest, plan_ref}. gen-N base = **최신 수용
#    artifact**(출력 수용≠리뷰 통과 — 분리 정의) — 없으면 원산물.
#    원산물 롤백은 팩의 명시적 선택(base = "origin"). pre_stage의
#    plan 출력은 plan_ref로 결속되어 Die 입력에 전달. Die 래퍼는
#    in/manifest.json을 읽음 — 호스트가 CLI --fix-context <경로>로
#    지시(포트 불변, 전달 규약은 workdir 파일 소유 — M0 경로 단일
#    진실원 패턴 승계)
#  격리: generation별 workdir 분리(gen-N), 실행별 로그(attempt-N.log)
#  실패: exit≠0/timeout → on_redrive_error(팩 정책: hold|fail-run)
#  산출: 새 result_commit → 다음 검증의 artifact 결속 + 이전 증거
#    superseded 처리(§2.4a)
target = "die-workdir"
payload = "findings"
bump = "fix_rounds"
on_done = "lenses"   # 루프 폐쇄: lenses→route→fixdie→lenses…
hold_target = "human"

[[nodes]]
id = "replan"        # ③' redrive의 특수형: replanner 지시문 실행 후
kind = "redrive"     #    새 generation 재구현 — emit 없음(r5 정정)
target = "die-workdir"
payload = "replan"   # (r7-1 스키마 필수)
base = "origin"      # r9-7: bad_plan = 롤백 후 재계획(원본 step-04)
pre_stage = "replan-stage"   # 팩 [[pack_stages]]에 등록(r11-4)   # 팩 정의 스테이지(KEEP 추출·계획 수정)
bump = "loopbacks"   # 공통 loopback 카운터(원본 review_loop_iteration)
on_done = "lenses"   # 재구현 결과는 다시 검증으로 — 루프 폐쇄(r5
                     # 정정: replans 0..5 전부 attempt=1, Inspecting
                     # 유지 — 도메인 emit/시도 소모 없음)
hold_target = "human"

[[nodes]]
id = "emit_failed_node"   # ④ emit: 재주문 정책 경로 — HALT 암시(r7-1:
kind = "emit"             #    on_done 불가). 팩이 route에서 명시 선택
event = "InspectionFailed"   #    시만. BMad 재계획은 미경유(§2.4a).
payload_from = "cumulative_findings"  # 재주문 산출물: 기본 폐기(청정
                      # workdir). 새 run은 counters만 승계, 산출물은
                      # 재검증 의무(§3.3a).

[[nodes]]
id = "pass"          # ④' emit(InspectionPassed)
kind = "emit"
event = "InspectionPassed"
payload_from = "summary"   # (r7-1 스키마 필수)

[[nodes]]
id = "human_gap"     # ⑤' intent_gap 전용 hold — 계획 재실행으로만 resume
kind = "hold"
resume_targets = ["replan"]

[[nodes]]
id = "human"         # ⑤ hold
kind = "hold"
# (r11) 상한·잔여 인간 — fix만 resume(replan은 human_gap 전용).
resume_targets = ["fixdie"]
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
| patch=같은 구현자 재호출 | redrive: 물리 실행 격리(gen-N workdir) + **논리 구현자 세션 계승**(fix-context 전달·base=최신 수용) | 동등(물리/논리 분리 — r7-7) |
| fix 루프(무제한이던 것) | fix_rounds 상한(팩 값) | 동등+상한 |
| bad_plan → **롤백+자동 재계획 5회** | route 카테고리 분기(any_in) → replan(pre_stage KEEP·**base="origin"** 롤백) → redrive(generation++) — 도메인 시도 불변 | **동등**(§2.4a·r9-7) |
| intent_gap: 인간 해결 후 재계획 | route 카테고리 분기 → human hold → `resume("replan")` 진입 | 동등(예제 trace) |
| defer(기록 후 진행) | `side_records = ["deferred"]` + 출력 스키마 `condition = "no_loopback"` — 루프백 시 기록 억제(원본 moot 규칙, r12-5). 정규화된 deferred 레코드(schema-defer.toml)는 outbox intent로 저널 기록(emit과 독립) | 동등 |
| carried(재런 저널 대조) | verify 지시문에 저널 대조 입력 포함 | 근사 |
| quick/thorough/auto | selection(fixed/by-diff) | 변형 |

**BMad FULL 라우팅 기대 trace(r9-7)** — 예제 팩으로 원본 step-04
전 분기 재현:
- patch → fixdie(논리 구현자 세션 계승, base=latest) → lenses 재검증
  (fix_rounds 0→3, 전부 도메인 attempt 불변) — 원본 "같은 구현자 최소 수정"
- bad_plan → replan(pre_stage KEEP, **base=origin 롤백**) → lenses
  (replans 0→5, attempt 불변) — 원본 "롤백 후 재계획"
- intent_gap → human(hold) → resume("replan") — 원본 "인간 해결"
- defer → pass + side_records=deferred 저널 — 원본 "기록 후 진행"
- replans=5 도달 시 bad_plan branch 미매치 → has_findings → human
  (HALT 인간 승격 — 원본 상한 5→HALT와 동등)
- continuation = fixdie의 논리 구현자 세션 계승 계약(--fix-context 전달,
  §2.4a generation) — 별도 노드 아님.

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
          | 'bump' '(' id ')'        ; hold는 액션이 아닌 노드(r9-3)
event_id := 'InspectionPassed' | 'InspectionFailed'   ; 로드 검증
payload_ref := id             ; 노드/스테이지 출력 참조(로드 검증)
do_seq   := action ( ';' action )*     ; emit은 전 시퀀스에 최대 1회
```
- 식별자 namespace 분리(r5-7): counter_id / group_id / payload_ref
  각각 자기 namespace 로드 검증(교차 오용 거부).
- do_seq는 route branch 필드. emit 뒤 후속 액션·goto 금지(종단 — r9-3).
  route는 무매치 불가 — 마지막 branch `true` 폴백 필수(로드 검증).
  `fail-run` = 실행 중단+저널 review_aborted+Inspecting 유지+알림
  (도메인 이벤트 없음 — 재주문은 운영/인간 정책).
- **노드 규범 스키마(r7-1)** — 유일 규범, 예제·부록 fixture는 이것에
  복종:
  | kind | 필수 | 선택 | 금지 |
  |---|---|---|---|
  | dispatch | stages(최소 1 — r10-6), on_done, hold_target, on_verify_invalid(hold\|fail-run), on_no_valid(hold\|fail-run) | side_records, role | — |
  | route | branches(마지막 원소 when="true") | — | on_done |
  | redrive | target, payload, on_done, hold_target, on_redrive_error(hold\|fail-run), **bump — 루프백 redrive는 필수**(가드가 검사하는 카운터를 증가시켜야 함, r12-1) | pre_stage, base("latest"\|"origin", 기본 latest) | — |
  | emit | event, payload_from | — | on_done(HALT 암시) |
  | hold | resume_targets | notify_only | on_done |
  branch(r9-3 종단 규칙): **do가 종단 액션(emit 포함)이면 goto 금지**
  (emit 후 HALT). **do가 비종단(bump만)이면 goto 필수**. {when, goto}
  단독 항상 허용. do는 emit/bump 시퀀스(emit≤1) — hold_for_human
  액션은 폐지(hold는 노드 진입으로만 발생). **bump-가드 일치(r12-1)**:
  루프를 닫는 redrive의 bump는 루프 진입을 검사하는 count_lt가 참조하는
  같은 카운터여야 한다(로드 검증: 사이클 경로 카운터-가드-증가 일치
  정적 검사) — 다른 카운터를 증가시키면 상한이 발화하지 않는다(v0.13
  실증 반례: loopbacks 가드 + replans bump → 7회 진입). 골든 trace는
  실제 팩 TOML을 파싱·실행하는 검증기로만 유효(수기 분기 미러링 금지).
  오류 정책
  (on_verify_invalid, on_no_valid, on_redrive_error)은 dispatch/redrive
  노드의 필드. **hold 이송 통일(r9-3)**: 모든 엔진 주도 hold(오류
  정책 hold·예산 소진 진입 차단·카운터 overflow·pass 적격성 거부)는
  활성 dispatch/redrive 노드의 `hold_target`(필수 필드) hold 노드로
  이송 — 미선언 시 fail-run. 모든 hold가 명시적 hold 정의에 연결돼
  resume 간선·DAG 검증 가능.
  entry = 그래프 최상위 `entry`(dispatch 노드 id만). stage 전체
  스키마(stages[]의 id/lenses/session/after/inputs_unsealed/role/
  output_schema)는 A.4에 규범 정의(승인 전 고정 — 아래).
  **hold 목적지 실행 컨텍스트(r10-7)**: hold_target은 activation
  커서에 **dispatch/redrive 진입 시 승계 저장**(내구 컨텍스트 —
  route/emit은 hold_target 없음, 진입 시점의 활성 값 사용). 간선
  dispatch/redrive→hold_target도 DAG 검증에 포함. route의 bump
  overflow hold는 같은 컨텍스트로 이송. 예제 정합(반영): replan+payload="replan", pass+
  payload_from="summary", human+resume_targets=["replan","…"],
  emit_failed_node의 on_done="HALT" 제거(HALT 암시).
  부록 A에 **수용 fixture(전체 팩)+거부 fixture(검증 규칙별 1개)**
  필수 — 구현 승인 전 제출(아래 부록에 실제 수록).
- 우선순위 `!` > `&&` > `||`, 괄호 허용. `any`는 has_findings의
  **예약 와일드카드**(로드 검증에서 일반 태그명과 충돌 거부).
- **provenance(r3-MAJOR-4, r4-MAJOR-8)**: 조건식 바인딩은 엔진이
  결속한 스코프만 노출 — 팩 stages에서 `role = "verify"`로 선언된
  스테이지의 출력만 verdicts/findings.group 스코프가 됨(로드 검증:
  verify 스테이지는 팩 전체 **≤1** — 0개면 조건식에서 verdicts/
  findings 참조 거부, r9-4). 원시 렌즈 출력은 has_findings에서도 불가.
  **barrier 필요충분조건(r7-5)** — 집계 단위 = **dispatch activation
  하나**(다중 dispatch는 각자 barrier). 술어: T=스테이지 수,
  E=오류(dispatch/timeout) 수, R=정상·정규화 거부 수, V=verify
  스테이지의 최종 상태(ok|error|absent).
  - all-error ⟺ E=T (V 포함 전부 오류)
  - no-valid-stage ⟺ E<T ∧ 유효 출력 0 (R>0 정규화 거부 포함)
  - verify-invalid ⟺ 유효 출력 ≥1 ∧ V=error (verify 정규화 거부 포함)
  - valid-partial ⟺ 0<유효<T ∧ V∈{ok,absent}
  - all-ok ⟺ 유효=T ∧ V∈{ok,absent}
  (r9-1) 완료 분류는 V의 ok/absent를 흡수 — T·E·유효·V=error 여부만으로
  5상태가 배타적·전사(반례 T=1,E=0,유효=1,V=absent → all-ok(absent)).
  **검증 적격성은 별도 게이트**: 자동 emit(InspectionPassed)은 현재
  artifact에 결속된 V=ok verify 증거 필수 — 엔진 런타임 검사로
  absent 상태의 pass 도달 시 커밋 거부 → hold_target 이송.
  verify 없는 dispatch의 조건식은 verdicts/findings 참조 불가(로드
  검증 정적 차단). verify 스테이지는 팩 전체 **≤1**(0 허용 — §2.5·
  A.4 일치, r9-4). verify-invalid는 on_verify_invalid(hold|fail-run)만.
- `do` 시퀀스 문법: `action ( ';' action )*` — 다중 emit은 로드
  검증 거부(도메인 단일 전이 원칙).

### 2.4 fix 루프 vs 재계획 — 도메인 시맨틱 정확히 분리

| 루프 | 매커니즘 | 도메인 attempt |
|---|---|---|
| **fix 루프**(팩 내부) | redrive: Inspecting 유지 + **새 gen workdir에서 Die 프로세스 재실행**(물리 재실행, 논리 세션 계승 — §2.4a), 이벤트 없음 | **불변** |
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
- **내구 산출물 결속(r5-2)**: 검증 통과 시 단일 커밋에 `(order,
  attempt, run_id, generation, immutable_commit, artifact_location)`
  저널 기록 + InspectionPassed 원자 확정. 복구·승인·Publisher는 이
  튜플의 경로·digest만 사용(attempt 기본 경로 추론 폐지).
  immutable_commit = **verify가 실제 검사한 불변 commit**(검증 대상
  스냅샷 — "완료 시점 HEAD" 아님). order.artifact_digest 조회·복구
  경로(line.rs 346 승계)는 이 튜플에서 파생됨을 명시.
- 비용 통제(r5-5, r6-4): 2종 예산 — (a) 비용 예산(토큰 등): 실행 전
  예약→완료 정산·반환. (b) **비환급 실행 횟수 예산**(u64, 주문 단위):
  모든 외부 시도·재시도에서 **최소 1을 원자 차감**(정산 0이어도 횟수
  는 소모) — 실소비 0 사이클의 무한 실행 차단. 복구·새 run 생성 시
  승계. 소진 hold는 인간 commit_pass/fail만(실행 resume 엔진 거부).
  사이클 검증은 resume·오류·복구 전이 포함.

### 2.4b 유한성 강제 (r4-MAJOR-3)

- **모든 redrive/dispatch 노드 실행은 예산을 차감**하고, 소진 시
  엔진이 해당 노드 진입 자체를 거부(→ hold). 조건식의 count_lt와
  무관하게 **우회 불가** — "상한 경로 포함"이 아니라 진입 차단.
- 카운터: u64, run 생성 시 0, bump만 증가, 감소 없음, overflow 시
  엔진 hold(래핑 방지). run 간 승계는 §3.3a(주문 단위 누계).
- 그래프 로드 검증(r7-2): **차감 노드(dispatch/redrive)를 제거한
  전이 그래프가 비순환(DAG)인지** 정적 검사 — route↔route 상호
  참조·hold→resume→hold 사이클(resume_targets 그래프 포함) 전부
  거부. "구조적 자동 충족" 주장 철회(반례: route A↔B). 거부
  fixture 2종(route 순환, resume 순환) 부록 고정.

### 2.5 정규화 (r3-MAJOR-7)
- 4분류(ok/parse_error/dispatch_error/timeout)별 정책표 — 팩이
  **4 상태 전부 선언**(ok의 정상 수용 정책 accept-stage 포함,
  r10-4; 엔진 기본값 없음, 미선언 시 로드 거부). 오류 정책은 팩
  전체 [[output.on_error]]가 유일 소유(노드 필드 아님):
```toml
[[output.on_error]]        # 순서=우선순위
state = "dispatch_error"   # ok | parse_error | dispatch_error | timeout
policy = "discard-stage"   # discard-stage | hold | fail-run
[[output.on_error]]
state = "parse_error"
policy = "discard-finding" # 개별 finding 거부(부분 거부)
```
- barrier 집계 상태는 **§2.3 진리표로 단일화(r6-6)**: all-ok /
  valid-partial / no-valid-stage / all-error / verify-invalid —
  배타적. no-valid-stage·all-error·verify-invalid은 route 평가
  진입 **전** 오류 정책(on_no_valid 등)으로 강제 이송(통과 불가).
  verify 스테이지 ≤1(0 허용)은 팩 전체 범위 — §2.3과 일치(r9-4).
  `missing_field`
  필수 집합·`ext_over` 단위(bytes)·markdown 그룹 규칙 형식은
  정규화 스키마 표준 템플릿(§부록 A, 구현 시점 확정)에 명시.

## 3. 실행 계약

### 3.1 팩 로드·digest (r3-MAJOR-9 완결)
- pack digest canonical bytes(독립 구현 동일성 보장 — r4-MAJOR-9):
  `input = Σ (for file in sorted-by(UTF-8 bytes of POSIX path)):
  u64le(len(path)) ‖ path ‖ u64le(len(content)) ‖ content` (파일은
  매니페스트 참조 폐쇄 + manifest.toml 자체; manifest의 digest
  필드 제거(r5-10, r9-8): manifest에서 `digest =`로 시작하는 **정확히 1개
  최상위 key-value 라인 전체(LF 포함, 선행 공백 포함)를 원본 바이트
  에서 삭제**. canonical 입력은 **LF 개행 + 파일 끝 LF 필수만 수용**
  — CRLF·CR·끝 LF 부재는 로드 거부(부록 A.2와 단일 규칙).
  pack_digest = SHA256(input), hex 소문자 64자 표기.
  v4 leaf(r6-8) = SHA256(결합: for f in [step_id,pack_id,pack_version,
  enforcement(ASCII 소문자),pack_digest(원시 32바이트)]:
  u64le(len(f))‖f — 문자열은 UTF-8 원본 바이트). manifest digest
  필드 제거 규칙: `digest` 키는 **unquoted 소문자 리터럴만** 허용
  (quoted 키·multiline 값·라인 내 복수 키 거부), 제거는 그 한 줄
  전체(LF 포함; 끝 LF 부재·CRLF는 로드 거부 — 위 통일 규칙).
  **최소 팩 fixture의 입력 바이트+예상 digest(hex)를 부록에 고정 —
  구현 전 독립 계산, 구현은 재현 의무**.
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
   §3.3a).
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
   식별자 3단계(r6-1): `activation_seq`(u64 단조 — 노드 **방문마다**
   발급, dispatch 재방문도 새 activation) → `activation_id =
   run_id#node_id#activation_seq` → 외부 시도 id = activation_id#
   retry_seq. 커서 전진과 다음 activation_seq 할당은 동일 트랜잭션.
   크래시 규칙: (a) intent·완료 없음 → retry_seq+1 재실행(같은
   activation). (b) 결과 커밋에 카운터·커서·후속 intent 포함.
   **(b-pre, r8-6) 산출물 내구 승격이 결과 커밋의 선행조건**: 내구
   저장→영수증 확보→결과/커서 커밋 순서. 커밋 전 크래시 orphan 승격물은
   GC(저널 무참조). 저장 영수증 없는 emit은 거부.
   (c) hold CAS 소비+resume intent 동일 트랜잭션. (d) emit 원자
   경계(r6-2): **이벤트 반영+증거+UNIQUE(action_id) 영수증을 stream
   ledger 단일 트랜잭션**에 기록(InspectionFailed 포함). 복구는 원래
   action_id로 영수증 회수, 커서 완료는 그 영수증 근거 멱등 확정.
   늦은 emit 재실행은 UNIQUE 위반으로 차단(order.rs 284가 attempt
   미검사 사실의 대비책). **영수증 없는 구 run emit(r7-3)**: ledger
   트랜잭션 안에서 expected order version·attempt·**활성 run/epoch**
   ·generation·artifact·유효 증거를 전부 대조 — 구 run 무효화는
   활성 epoch 전진으로 직렬화, 구 run의 emit은 epoch 불일치로
   거부(UNIQUE만으로는 구 run 최초 지연 emit을 못 막는 r7 반례
   폐쇄). 기존 영수증은 재적용 없이 반환.
5. **디스패치 fencing(r3-MAJOR-6, r4-MAJOR-7)**: (a) 응답 차단 —
   stage별 dispatch generation + 활성 실행 토큰 내구 기록, 응답
   수용 시 generation·토큰·run·attempt·artifact를 **단일 수용
   트랜잭션**에서 재검사(원자 경계). (b) **쓰기 차단(r5-8)** — 모든 외부 실행(dispatch·pre_stage·
   redrive 공통)은 activation#retry별 쓰기 namespace(run-dir/exec/
   <activation>/<retry>/)로 격리 — sibling·git refs·세션 저장소
   포함(샌드박스 허용 경로가 그 namespace로 한정). 수용 출력만
   승격(gen 디렉터리·불변 저장소), retry도 새 namespace(겹침 없음).
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
- `resume(node_id)`: 인간 응답(해결문·수정 intent)은 **불변 artifact
  (human_resolution)로 저장되어 resume에 결속** — 재개된 redrive의
  pre_stage가 입력으로 소비(r12-3). **팩이 hold 노드에 선언한 허용 목적지 목록**
  (`resume_targets`)으로 제한. hold 진입 시 **불변 hold_instance_id
  발급**(재진입 시마다 신규 발급 — r7-4) + 선행 activation/evidence
  id 집합 + 현재 artifact digest를 hold 레코드에 저장. **네 인간
  API(resume/commit_pass/commit_fail/abort) 전부 hold_instance_id를
  필수 입력** — CAS는 그 ID·미소비 상태·증거 스냅샷에 대해 수행
  (H1 화면의 늦은 요청이 H2를 소비하는 구현 배제). 자동 Passed는
  현재 검증 회차의 verify 증거만 인정. commit_pass 대체는 명시적
  override: 신원+사유+원장 증거. 경합 시 하나만 승리.

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

## 부록 A — 규범 fixture (r7-6 실수록)

### A.1 최소 팩 (수용) — 실제 파일 (r9-2/5: 완결·실파일이 유일 canonical)

`skills/fixture-min/v1/` **실파일이 canonical 입력의 유일한 원본** —
아래 전사는 가독용이며 계산에 쓰지 않는다(문서-파일 불일치 원천 차단).
- `manifest.toml`(LF, 끝 LF):
```toml
entry = "scan"

[[nodes]]
id = "scan"
kind = "dispatch"
stages = [{ id = "lens-a", lenses = ["lens-a.md"], role = "lens" }]
on_done = "done"
hold_target = "end_hold"

[[nodes]]
id = "done"
kind = "route"
[[nodes.branches]]
when = "true"
do = "emit(InspectionPassed, summary)"

[[nodes]]
id = "end_hold"
kind = "hold"
resume_targets = []

[[output.on_error]]
state = "ok"
policy = "accept-stage"

[[output.on_error]]
state = "dispatch_error"
policy = "discard-stage"

[[output.on_error]]
state = "parse_error"
policy = "discard-finding"

[[output.on_error]]
state = "timeout"
policy = "hold"

[hold]
timeout_s = 0
```
**기대 trace**: T=1, E=0, 유효=1, V=absent → all-ok(absent) →
route(true) → do=emit(InspectionPassed) → **적격성 게이트가 V=ok
증거 부재로 거부** → hold_target(end_hold) 진입 — "verify 없는 자동
pass 거부→hold" 수용 시험(성공 emit fixture가 아님).
kind="review-pack" — advisory-pack은 emit 노드 포함 자체가 로드 거부.

**FULL 팩(r10-3)**: `skills/bmad-review-full/v1/` — BMad FULL
라우팅 수용 팩 실파일(counters·entry·stages 4[verify=triage]·
side_records=deferred·pack_stages(replan-stage)·schema-defer.toml·
GOLDEN_TRACE.md+tools/verify_golden_trace.py(실행형: 팩 TOML 실파싱·60조합+반복 전이),
pack=e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8).
§2.1 예제는 이 팩 policy.toml의 간략화, 기대 trace는
GOLDEN_TRACE.md(혼합 범주·상한 경계 10케이스 — 부록 A.3 승인 전 확정).

**이중 독립 검증(r14)**: `tools/verify_golden_trace.py`(주 검증기 —
실제 그래프 순회)와 `tools/astra_verify.py`(독립 검증기 — 주 검증기를
보지 않고 작성, 자체 DSL 인터프리터)가 같은 팩을 검증한다. 독립
검증기가 주 검증기가 놓친 결함(상한 branch의 bump 누락 — 인간 hold
진입 시 loopbacks=6이어야)을 실제로 발견·수정했다. 두 검증기 모두
PASS여야 회귀 조건.

**digest(독립 계산, python hashlib §3.1 canonical)** — fixture-min:
pack=8528b29eb4564756d29adfa58698dcfc74cd7a1fbd6c365632ec6d7c2e9160c6,
leaf(quick-inspect,fixture-min,1.0.0,advisory)=2be18a52eeea833d605f42e1d037d4430f0977b3815b9620619f6b5b185962dd —
구현은 재현 의무(파일→canonical→digest 대조).

### A.2 거부 fixture — 9종 실제 파일 (r10-5: baseline 정합 + 기대 오류 코드)

baseline = A.1 팩 전체(모든 필드 정합·TOML·DSL 문법 유효·digest
재계산 일치). 각 디렉터리는 **규칙 1개만 위반** +
`expected_error.txt`(기대 오류 코드) — 01~06의 manifest digest는
변형 후 재계산되어 정상(위반은 policy 그래프뿔), 07~09는 manifest
형식만 위반. 로더 회귀: 각 fixture가 **정확히 그 코드로** 거부:
| 디렉터리 | 위반 | 기대 코드 |
|---|---|---|
| 01-route-cycle | ra↔rb 상호참조 | E_DAG_CYCLE |
| 02-resume-cycle | resume 간선 사이클(done) | E_DAG_RESUME_CYCLE |
| 03-missing-payload | redrive payload 누락 | E_SCHEMA_MISSING_FIELD |
| 04-verdicts-no-verify | verify 없는 verdicts 참조 | E_PROVENANCE |
| 05-no-true-fallback | 마지막 branch false | E_NO_TRUE_FALLBACK |
| 06-do-only-nonterminal | do=bump, goto 없음 | E_NONTERMINAL_DO_NO_GOTO |
| 07-quoted-digest-key | manifest `"digest" =` | E_DIGEST_KEY_QUOTED |
| 08-crlf-manifest | CRLF 개행(digest 정상) | E_LF_REQUIRED |
| 09-no-trailing-lf | 끝 LF 부재(digest 정상) | E_LF_REQUIRED |

**수용성 규칙(단일)**: LF 개행 + 파일 끝 LF 필수 — CRLF·CR·끝 LF
부재는 로드 거부. 9종 전부 거부 + A.1 수용이 회귀 조건.

### A.4 stage 규범 스키마 (승인 전 고정 — r8-2)
dispatch.stages[] 각 항목: {id(필수), lenses[파일명](필수≥1),
session?(id), after?[stage id], inputs_unsealed?[입력명],
role?("lens"|"verify"|"replan" — 팩 전체 verify≤1),
output_schema?(파일명, 권장 — verdicts/findings/side_records 매핑을
  팩 데이터로 고정, r12-4: values/from/active_categories/condition)} —
  오류 정책은 스테이지가 아닌 §2.5 팩 전체
[[output.on_error]](필수·순서=우선순위)가 소유(r9-4). 표준 payload_ref:
"summary"(dispatch 산출 요약), "findings"(정규화 출력),
"cumulative_findings"(run 누적). markdown 그룹 규칙 템플릿·전체
예는 구현 중 확정(A.3 명맥) — 스키마 자체는 이대로 동결.
