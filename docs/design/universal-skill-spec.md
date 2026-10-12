# Foundry Universal Skill Spec — 범용 실행 모델 (Universal Review Engine)

- **버전**: v0.10 (2026-10-12) — r8 REWORK(MAJOR 6+MINOR) 반영.
  generation 모델·인과·epoch·CAS 해소 r8 확인. 재검증 대기.
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
goto  = "replan"     # ★ 자동 재계획 — generation 경유(§2.4a)
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

[[nodes]]
id = "replan"        # ③' redrive의 특수형: replanner 지시문 실행 후
kind = "redrive"     #    새 generation 재구현 — emit 없음(r5 정정)
target = "die-workdir"
payload = "replan"   # (r7-1 스키마 필수)
pre_stage = "replan-stage"   # 팩 정의 스테이지(KEEP 추출·계획 수정)
bump = "replans"
on_done = "lenses"   # 재구현 결과는 다시 검증으로 — 루프 폐쇄(r5
                     # 정정: replans 0..5 전부 attempt=1, Inspecting
                     # 유지 — 도메인 emit/시도 소모 없음)

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
id = "human"         # ⑤ hold
kind = "hold"
# (r8-3) route1 재개 제거 — 유한성 DAG 통과: human→(replan→lenses는
# 차감 경로)만. route1 재개는 route1이 차감 노드 경유 없이 재평가되는
# 사이클이라 로더가 본 예제를 거부했음.
resume_targets = ["replan"]   # (r7-1 스키마 필수)
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
- 식별자 namespace 분리(r5-7): counter_id / group_id / payload_ref
  각각 자기 namespace 로드 검증(교차 오용 거부).
- do_seq는 route branch 필드. emit/hold 뒤 후속 액션 금지(종단).
  route는 무매치 불가 — 마지막 branch `true` 폴백 필수(로드 검증).
  `fail-run` = 실행 중단+저널 review_aborted+Inspecting 유지+알림
  (도메인 이벤트 없음 — 재주문은 운영/인간 정책).
- **노드 규범 스키마(r7-1)** — 유일 규범, 예제·부록 fixture는 이것에
  복종:
  | kind | 필수 | 선택 | 금지 |
  |---|---|---|---|
  | dispatch | stages, on_done | side_records, role | — |
  | route | branches(마지막 원소 when="true") | — | on_done |
  | redrive | target, payload, on_done | bump, pre_stage, base("latest"\|"origin", 기본 latest), on_redrive_error("hold"\|"fail-run", 기본 hold) | — |
  | emit | event, payload_from | — | on_done(HALT 암시) |
  | hold | resume_targets | notify_only | on_done |
  branch: {when, goto} 또는 {when, do+goto} — **do만(무 goto)은
  금지**(r8-2: 비종단 do의 커서 부재) 또는 do가 비종단(bump만)이면
  goto 필수. do는 emit/hold/bump 시퀀스(emit≤1). 오류 정책
  (on_verify_invalid, on_no_valid, on_redrive_error 등)은 **dispatch/
  redrive 노드의 필드**(해당 노드가 주체) — 스키마 선택 필드로 추가.
  hold 유발 경로: 오류 정책 "hold"는 **같은 노드에 hold_target
  필드(명시적 hold 노드 id)**를 요구(기본 없으면 fail-run) — 모든
  hold 발생이 명시적 hold 정의에 연결돼 resume 간선·DAG 검증 가능.
  entry = 그래프 최상위 `entry`(dispatch 노드 id만). stage 전체
  스키마(stages[]의 id/lenses/session/after/inputs_unsealed/role/
  output_schema)는 A.4에 규범 정의(승인 전 고정 — 아래). 예제 정합(반영): replan+payload="replan", pass+
  payload_from="summary", human+resume_targets=["replan","…"],
  emit_failed_node의 on_done="HALT" 제거(HALT 암시).
  부록 A에 **수용 fixture(전체 팩)+거부 fixture(검증 규칙별 1개)**
  필수 — 구현 승인 전 제출(아래 부록에 실제 수록).
- 우선순위 `!` > `&&` > `||`, 괄호 허용. `any`는 has_findings의
  **예약 와일드카드**(로드 검증에서 일반 태그명과 충돌 거부).
- **provenance(r3-MAJOR-4, r4-MAJOR-8)**: 조건식 바인딩은 엔진이
  결속한 스코프만 노출 — 팩 stages에서 `role = "verify"`로 선언된
  스테이지의 출력만 verdicts/findings.group 스코프가 됨(로드 검증:
  verify 스테이지 정확히 1개, 미선언 시 조건식에서 verdicts/
  findings 참조 거부). 원시 렌즈 출력은 has_findings에서도 불가.
  **barrier 필요충분조건(r7-5)** — 집계 단위 = **dispatch activation
  하나**(다중 dispatch는 각자 barrier). 술어: T=스테이지 수,
  E=오류(dispatch/timeout) 수, R=정상·정규화 거부 수, V=verify
  스테이지의 최종 상태(ok|error|absent).
  - all-error ⟺ E=T (V 포함 전부 오류)
  - no-valid-stage ⟺ E<T ∧ 유효 출력 0 (R>0 정규화 거부 포함)
  - verify-invalid ⟺ 유효 출력 ≥1 ∧ V=error
  - valid-partial ⟺ 0<유효<T ∧ V=ok
  - all-ok ⟺ 유효=T ∧ V=ok
  배타적(조건 불충족 시 정의되지 않음 없음). **verify 없는 dispatch
  허용** — 단 그 activation의 조건식은 verdicts/findings 참조
  불가(로드 검증이 정적 차단) — provenance 모순 제거. pass 진입은
  all-ok·valid-partial만, 엔진이 상태별 branch 진입 집합 강제.
  verify-invalid는 on_verify_invalid(hold|fail-run)만.
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
- barrier 집계 상태는 **§2.3 진리표로 단일화(r6-6)**: all-ok /
  valid-partial / no-valid-stage / all-error / verify-invalid —
  배타적. no-valid-stage·all-error·verify-invalid은 route 평가
  진입 **전** 오류 정책(on_no_valid 등)으로 강제 이송(통과 불가).
  verify 스테이지 "정확히 1개"는 팩 전체 범위. `missing_field`
  필수 집합·`ext_over` 단위(bytes)·markdown 그룹 규칙 형식은
  정규화 스키마 표준 템플릿(§부록 A, 구현 시점 확정)에 명시.

## 3. 실행 계약

### 3.1 팩 로드·digest (r3-MAJOR-9 완결)
- pack digest canonical bytes(독립 구현 동일성 보장 — r4-MAJOR-9):
  `input = Σ (for file in sorted-by(UTF-8 bytes of POSIX path)):
  u64le(len(path)) ‖ path ‖ u64le(len(content)) ‖ content` (파일은
  매니페스트 참조 폐쇄 + manifest.toml 자체; manifest의 digest
  필드 제거(r5-10): manifest에서 `digest =`로 시작하는 **정확히 1개
  최상위 key-value 라인 전체(LF 포함, 선행 공백 포함)를 원본 바이트
  에서 삭제** — multiline·주석·CRLF는 원본 보존(정규화 없음, 경로
  중복 거부). pack_digest = SHA256(input), hex 소문자 64자 표기.
  v4 leaf(r6-8) = SHA256(결합: for f in [step_id,pack_id,pack_version,
  enforcement(ASCII 소문자),pack_digest(원시 32바이트)]:
  u64le(len(f))‖f — 문자열은 UTF-8 원본 바이트). manifest digest
  필드 제거 규칙: `digest` 키는 **unquoted 소문자 리터럴만** 허용
  (quoted 키·multiline 값·라인 내 복수 키 거부), 제거는 그 한 줄
  전체(LF 포함; 파일 끝 LF 부재 허용 — 마지막 바이트까지 원본 보존).
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
- `resume(node_id)`: **팩이 hold 노드에 선언한 허용 목적지 목록**
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

### A.1 최소 팩 (수용) — 실제 파일 (r8-4: 레포에 실재)
`skills/fixture-min/v1/`에 실제 파일로 존재(구현 전 배치):
- `manifest.toml`(LF, 끝 LF 있음):
  ```
  id = "fixture-min"
  interface_version = 1
  kind = "advisory-pack"
  version = "1.0.0"
  files = ["lens-a.md", "policy.toml"]   # 참조 폐쇄 명시
  digest = "<계산값>"
  ```
- `lens-a.md`: `ok`(LF, 끝 LF 있음)
- `policy.toml`(LF, 끝 LF 있음):
  ```
  entry = "scan"

  [[nodes]]
  id = "scan"
  kind = "dispatch"
  stages = []
  on_done = "done"

  [[nodes]]
  id = "done"
  kind = "route"
  [[nodes.branches]]
  when = "true"
  do = "emit(InspectionPassed, summary)"
  goto = "end_hold"

  [[nodes]]
  id = "end_hold"
  kind = "hold"
  resume_targets = []
  ```
  (r8-2 스키마 준수: do+goto, entry 최상위, 단일 노드 그래프 무사이클.
  summary = dispatch 기본 산출물 요약 참조 — A.4 stage 스키마의
  표준 payload_ref. 이 fixture는 Passed 적격성(V=absent→거부→
  hold) 경로를 **의도적으로** 담는다 — 자동 pass가 아닌 hold로
  끝나는 수용 fixture.)
**digest 절차**: 실측은 스펙 확정 직후 독립 스크립트(python hashlib,
§3.1 canonical)로 계산해 이곳에 hex로 기입하고 리뷰 검증 — 구현은
그 값을 재현. (아래 "DIGEST:" 행 — 계산 후 기입)
DIGEST: pack=31d09ee9aa964ce13e2a685438bfd34857c2f08ba9c8a2006480f9c29dc69cbd leaf(quick-inspect,fixture-min,1.0.0,advisory)=9728fc35aead252abcfa37c6380ca680ce5748bcd005905f68d6365facb9454a — python hashlib §3.1 canonical 독립 계산(2026-10-12), 구현은 재현 의무. fixture 실파일: skills/fixture-min/v1/ (리포 동봉)
### A.2 거부 fixture (규칙별 1) — 각 실제 파일
route 순환(a→b→a), resume 순환(hold→predecessor), replan payload
누락, verify 없는 activation의 verdicts 참조, 마지막 branch true
아님, do만(무 goto) 비종단, quoted digest 키, **CRLF manifest**,
**끝 LF 부재 manifest** — 각 1파일, 로더가 전부 거부해야 함.
**수용성 규칙 통일(r8-5)**: canonical 입력은 **LF 개행 + 파일 끝 LF
필수**만 수용 — CRLF·CR·끝 LF 부재는 **로드 거부**(§3.1 "원본 보존"
문구 정정: 보존되는 것은 수용된 LF 원본 바이트이며, 변형 자체를
거부한다. 예외 없음). 두 구현이 다르게 판단할 여지 제거.
### A.4 stage 규범 스키마 (승인 전 고정 — r8-2)
dispatch.stages[] 각 항목: {id(필수), lenses[파일명](필수≥1),
session?(id), after?[stage id], inputs_unsealed?[입력명],
role?("lens"|"verify"|"replan" — 팩 전체 verify≤1),
output_schema?(파일명), on_error?{state→policy}}. 표준 payload_ref:
"summary"(dispatch 산출 요약), "findings"(정규화 출력),
"cumulative_findings"(run 누적). markdown 그룹 규칙 템플릿·전체
예는 구현 중 확정(A.3 명맥) — 스키마 자체는 이대로 동결.
