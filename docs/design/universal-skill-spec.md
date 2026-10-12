# Foundry Universal Skill Spec — 스킬=데이터, 도메인=엣지

- **버전**: v0.4 (2026-10-12) — r3 BLOCKER-2(예제 평가 순서 결함)·
  주요 MAJOR 즉치 반영. 잔여 r3 이슈는 소유자 판정 대기(§4a).
- **의도(소유자)**: Foundry는 어떤 방법론도 돌릴 수 있는 유니버설 플랫폼.
  BMad는 스킬 데이터의 첫 고객일 뿐 — **Rust 도메인에 방법론 코드가
  박히는 순간이 이 스펙의 실패다.** 엔진 고정·확장 선언(Rust procedural
  macro 원칙).
- **인수 기준(변경 없음, r2 재확인)**: 판정 어휘·단계 순서·라우팅
  동작을 팩 데이터만 바꿔서 바꿀 수 있고, 그때 **엔진 코드 diff 0**.

## 0. 측정된 기반 (r2 실측 확정 — 이 표가 시맨틱의 원전)

### 0.1 InspectionFailed 실제 전이 (order.rs 284–294, MAX_ATTEMPTS=3)

| 조건 (Inspecting에서 InspectionFailed) | 결과 |
|---|---|
| reason이 `no-op` 접두 | **Failed (종단)** — 시도 불가 |
| attempt ≥ 3 | **Failed (종단)** |
| 그 외 | **attempt += 1 → Queued** (Die 재실행부터) |

- Inspecting에서 **DieFailed는 사용 불가**(Working 전용 엣지).
- FurnaceHold 진입은 InspectionPassed 유일. 엣지 집합 21개(frozen_v1)
  — 발행 주체는 검사 안 함(주체 전환은 엣지 변경 불필요).
- **reason 인코딩 충돌(r2-2)**: findings 원문이 `no-op`로 시작하면
  의도치 않은 종단. → 본 스펙 §3.4 이스케이프 계약으로 차단.

### 0.2 기존 검사 커밋 경로 (전환 대상, 회귀 테스트 필수)
line.rs 233(순수 판정 커밋) · line.rs 307(Inspecting 재진입 무조건
통과) · stream.rs 705(M8 어댑터 연속 발행).

### 0.3 기존 인프라 제약
- dsl.rs 218 Merkle: **schema_version=3만** 내장 policy 해시.
- skill.rs 48 해시: id/interface_version/kind/declares_tools.
- SkillEnforcer(183): 도구명 교집합만 — 경로·심링크·네트워크 미검사.
- BMad 원본 고정: bmad-build step-04@`170bbabd…`, verification-gap@
  `45ab0b8c…`, customize@`1a625506…` (sha256).

## 1. 계층 분리 — 무엇이 어디 사나

| 계층 | 소유 | 금지 |
|---|---|---|
| **도메인(Rust)** | frozen_v1 엣지·가드, 두 이벤트 타입, review-run **일반 증거 참조**(저널 JSON: order/attempt/artifact_digest/run_id/결속 digests/counters), §2 액션 인터프리터(폐쇄 집합 실행), 정규화 **실행기**(§2.4 규칙 해석) | 판정 어휘·라우팅 의미·렌즈 이름·루프 수칙·claims 순서 등 방법론 어떤 것도 |
| **팩(데이터)** | 렌즈 지시문, 스키마+정규화 규칙, 판정 어휘, 조건식, 액션 스크립트(§2.2), 단계 그래프, 예산·타임아웃 값 | Rust 실행 불가 — 선언만 |
| **호스트(운영)** | 리뷰어 디스패치 샌드박스(파일 범위·심링크·프로세스·네트워크 격리), 인간 API | 정책 해석 |

정규화·트리아지 추론은 **팩 지시문이 수행**(r2-1: 도메인 밖).
도메인 정규화 실행기는 §2.4의 **폐쇄적 규칙 형식**(패턴→필드 매핑)
만 해석 — 자유 코드 없음.

## 2. 액션 어휘 (r2-BLOCKER-1 해법 — 핵심)

### 2.1 엔진이 아는 폐쇄 집합 (전부. 추가는 스펙 버전 필요)

**액션** (라우팅 스크립트가 선택하는 실행 프리미티브):
- `emit(event, payload_ref)` — OrderEvent 발행. `event ∈ {InspectionPassed,
  InspectionFailed}`만 허용(로드 검증). payload_ref는 §3.4 인코딩.
- `hold_for_human()` — **이벤트 없음**, Inspecting 유지, 인간 대기
  상태로 전환(§3.5). 자동 커밋 없음.
- `terminal_halt(reason)` — 이벤트 없음, Inspecting 유지 + 관측
  알림(운영 중단 신호 — Failed로 가는 길은 §0.1 표대로 emit만).

**조건식 미니언어** (폐쇄 문법, 로드 시 파싱 검증):
```
expr := all_in(verdicts, set) | any_in(verdicts, set) | count_lt(counter, n)
      | has_findings(group) | timeout() | budget_exhausted()
      | expr && expr | expr || expr | !expr
set  := 팩이 정의한 verdict 값들의 리터럴 집합
```
- `group_by`는 액션이 아니라 **팩 지시문이 findings에 붙이는 태그**로
  대체(r2-1): 렌즈/검증기가 group 태그를 생성, 조건식은
  `has_findings(group)`로 소비. 엔진은 태그 존재만 본다.
- `verdicts.verify`도 데이터 참조로: `verify = { instruction =
  "lens-verify.md", input = "findings+diff", output_schema =
  "schema-findings.toml" }` — 검증기 지시문·입출력이 전부 팩 소유.

**바인딩**: `{review_retries}`, `{attempt}`, verdict 값, `{budget_left}`
— 런타임 값 주입. 조건식·한계값 전부 팩 리터럴.

**엔진의 범용 연산(전부)**: 그래프 스케줄·경로 전달·barrier 수집·
카운터 bump·예산 집행·조건식 평가·정규화 규칙 적용·액션 실행.
**어휘·임계값·순서를 코드가 모른다.**

### 2.2 라우팅 스크립트 (팩 데이터 — 예: bmad-review)

```toml
[[review.actions]]            # 순서 = 우선순위, 첫 매치 후 중단 (r3-2)
# 0) 예산·타임아웃은 무조건 최우선 — 미해결 findings가 있어도 재시도 아님
when = 'budget_exhausted() || timeout()'
do = 'hold_for_human()'

[[review.actions]]
# 1) 미해결 finding 존재 + 재시도 잔여 → 재시도
when = 'has_findings(any) && count_lt(review_retries, 2)'
do = 'emit(InspectionFailed, findings); bump(review_retries)'

[[review.actions]]
# 2) 미해결 finding 존재 + 재시도 소진 → 승격 (통과 아님!)
when = 'has_findings(any)'
do = 'hold_for_human()'

[[review.actions]]
# 3) 불확실만 존재 → 승격
when = 'any_in(verdicts, ["maybe-false"])'
do = 'hold_for_human()'

[[review.actions]]            # 4) 통과 — findings 0일 때만 도달
when = '!has_findings(any)'
do = 'emit(InspectionPassed, summary)'
```
**통과 조건(불변 계약)**: 폴백이 아니라 `!has_findings(any)` —
미해결 finding이 있는 어떤 상태에서도 자동 통과 불가(우선순위 0~2가
전부 선행). 예산 초과가 재시도보다 우선한다.
- `bump(counter)` — review-run 내구 카운터 증가(엔진 연산).
- **"patch를 repair로 바꾸면?"** — 라우팅 **이름 자체가 없다**: 동작 =
  액션 조합. 다른 방법론은 다른 when/do 조합(plain-cr은 검증 없이
  `when='!has_findings(any)' do='emit(InspectionPassed, summary)'`만).
- BMad intent_gap/bad_plan 구분: 팩 조건식 — `has_findings(plan-root)`
  그룹 태그(팩 검증기 지시문이 생산) → hold_for_human.

### 2.3 액션 ↔ 도메인 전이 사상 (§0.1과 정확히 정렬)

| 액션 결과 | 도메인 효과 |
|---|---|
| emit(InspectionPassed) | FurnaceHold (유일 경로) |
| emit(InspectionFailed, 이스케이프된 findings) | §0.1 표 그대로: no-op 아님+attempt<3 → Queued+attempt++ / 그 외 → Failed |
| hold_for_human / terminal_halt | 상태 불변(Inspecting), 저널에 대기/중단 기록 |
| (재시도 소진·예산 초과 시) | **자동 통과 없음** — §2.2 우선순위 0/2가 hold |

- 정책 `review_retries`와 도메인 attempt는 **독립 카운터**: 정책이
  먼저 닿으면 hold_for_human(자동 재시도 중단), 도메인이 먼저 닿으면
  Failed 종단. 우선순위는 조건식 평가 순서가 곧 그것.

### 2.4 정규화 규칙 (폐쇄 형식 — r2-9)

```toml
[output.normalization]
format = "json-array"          # or "markdown-list" — 팩 선택
reject_if = "missing_field || ext_over(4096)"

[[output.normalization.fields]]
common = "location"
from = "where"                 # json 키 또는 markdown 그룹 규칙 id

[[output.normalization.fields]]
common = "trigger_condition"
from = "what"
```
- 렌즈 출력 상태 4분류: `ok`(정상, 빈 포함)·`parse_error`·
  `dispatch_error`·`timeout` — 각각 §3.3 on_* 정책으로 소비.
- **빈 결과 통과 조건**: 유효(ok) findings=0이고 폴백 액션의 when이
  참 → 통과. `reject_if`로 전량 거부 → `on_invalid_output`(기본
  hold) — v0.2 모호성 제거.
- `reject_if` 조건식도 §2.1 미니언어의 하위 집합(`missing_field`,
  `ext_over(n)` 추가 프리미티브 — 로드 검증).

### 2.5 TOML 유효성 (r2-11)
본 스펙 예제 전부 유효 TOML(TOML 1.0) — 구현 §6.1 셀프테스트가
예제 전체를 파싱 검증.

## 3. 실행 계약

### 3.1 팩 로드·digest 사슬 (r2-5)
- pack digest = SHA256(정규화 경로(길이 prefix 구분)‖내용 정렬 결합:
  manifest.toml의 digest 필드 제외 전체 + lens-* + policy.toml +
  schema-* + 참조 폐쇄(매니페스트 나열 파일만 — 폐쇄 검증)).
- 검증 2회(로드 시·디스패치 직전) 후 **검증 바이트의 불변 스냅샷**
  (run-dir 복제)으로 실행 — 디스패치 창 교체 차단. 스냅샷 영수증이
  VerifiedSkill+Merkle 결속 생성.
- 계약: `preset.skills[].digest == manifest.digest == 계산값` — 불일치
  로드 거부. 매니페스트 보안 필드(interface_version/kind/
  declares_tools)는 pack digest 인증 범위에 포함.

### 3.2 프리셋 v4 (r2-4)
- `schema_version: 4`에만 `review_pack: <pack-id>@<version>` 필드.
- v4 Merkle은 v3 로더 **확장**: v3 내장 policy 해시 유지 + review_pack
  leaf 추가(리프 스키마 문서화). v3 주문·프리셋·재생 그대로(로더
  버전 분기 — 공존).
- review_pack 부재 v4 스텝: `enforcement=advisory` 기본 = 증거만,
  전이는 기존 경로. required 팩 부재 = 로드 거부.
- `kind: "advisory-pack"` — 기존 SkillKind::Advisory와 별개 신규 kind
  (role 배선 불가 규칙 승계 — 팩은 role이 아니라 검사 스텝 속성).
- 기존 3 경로(§0.2)의 required 리뷰 우회 방지 = 회귀 조건에 포함.

### 3.3 review-run 실행 (락 밖)
1. run 생성: 주문·attempt·artifact digest·pack digest·스냅샷 영수증
   결속, 내구 기록(멱등 키 = run_id).
2. [stages] 그래프: stage는 **세션 참조**(r2-7) — `session = "trace"`
   선언, 같은 session = 같은 리뷰어 연속 단계(claims는 trace 세션
   후속). `inputs_unsealed`는 **stage 시작 전 차단**: claims 경로가
   seal 상태로 주어지고 선행 완료 시에만 호스트가 개봉(지시문이
   우회 불가 — 샌드박스 경계). 로드 검증: 순환·미정의 의존·없는
   session 참조 거부.
3. barrier: 모든 활성 stage 완료(4분류) 후에만 조건식 평가.
4. 디스패치: DB 쓰기 락 밖. 격리 = SkillEnforcer(도구 교집합 — 팩이
   declares_tools에 **최소 읽기 역량** 포함 선언, r2-8) + 호스트
   샌드박스(경로 범위·심링크·프로세스·네트워크 — 인프라 층). findings
   = 비신뢰 데이터: 평가·인코딩 전 전달 경로 격리(§3.4).
5. 오류 우선순위(r2-9): stage 단위 on_* → stage 폐기 또는 hold.
   부분 결과는 barrier 후 유효 stage만 조건식 입력.

### 3.4 reason 인코딩 (r2-2 완결)
emit(InspectionFailed)의 payload는 구조화 findings(저널 JSON)와 별개
로 reason 문자열 **프리픽스 이스케이프**: reason = `review:{n}‖` +
findings 요약. `review:` 접두는 `no-op` 접두 매치와 충돌 불가 — 우연한
종단 원천 차단. 인코딩은 엔진이 수행(방법론 무관 계약).

### 3.5 인간 대기 종료(r2-3)
- hold_for_human 진입 시 내구 상태 `awaiting_human` + deadline
  (`[hold] timeout_s`, 기본 무한 = **명시적 운영 정책 선언**).
- 인간 API: run·attempt·artifact 재검사 후 커밋 승인(대기 중 자동
  경로 부재 + API 결속 재검사 이중 차단). 인간은 Passed/Failed 커밋
  또는 재개. 방치 시 재알림(`[hold] remind_s`). 종료는 인간만.
- Inspecting 취소 엣지 부재(frozen_v1) — 취소는 별도 부호화 진화
  과제(본 스펙 범위 밖 명시).

### 3.6 재시작 복원(r2-10)
- 내구 상태: stage 세션 식별자·출력 digest·입력 digest·절대 deadline·
  예약 예산·review_retries 누계(주문 단위).
- 재시작: 완료 stage(출력 digest) 재실행 없음, 미완료 재디스패치.
  세션 복원 불가 → 해당 stage 폐기, 같은 리뷰어 보장 필요 시 **새
  run 재시작**(저널 명시). 지연 응답 수용 시 run/attempt/artifact
  digest 재검사 — 불일치 폐기.

### 3.7 관측/필수 분리·원자성
알림 실패 무차단. `enforcement=required`에서 필수 증거(스냅샷 영수증·
findings) 저장 실패 = 전이 커밋 불가(저널 append+이벤트 단일
트랜잭션 — 기존 append_validated 승계).

## 4. BMad 팩 충실도 (원본 hash 고정 — §0.3)

| BMad 원본 | Foundry 표현 | 동등성 |
|---|---|---|
| 렌즈 독립 병렬+after | stages 그래프+barrier | 동등 |
| claims=같은 Edge-Case 리뷰어 후속 | session 재사용+inputs_unsealed+호스트 seal | 동등 |
| findings 4필드+확장 | 정규화 규칙(팩 소유) | 동등 |
| 트리아지 실측 검증·carried·그룹화 | 검증기 지시문(팩)+group 태그+재런 저널 대조 | 근사(지시문 이식, carried는 재런 저널 대조 근사) |
| patch=같은 구현자 **세션** 재호출 | emit→Queued→같은 Harness attempt 경로(line.rs 206은 attempt별 경로 — 세션 지속 아님) | **변형**: findings 재시도 입력 전달되나 동일 세션·작업물 지속 아님(Foundry attempt 시맨틱) |
| 5회 루프백 상한(재계획) | review_retries+hold_for_human | 수동 대체(자동 재계획 포기) |
| intent_gap/bad_plan/defer | 조건식+group 태그+hold/저널 deferred | 변형 |
| quick/thorough/auto(경로 기반) | selection(fixed/by-diff) | 변형 |
| 상태머신 자체 재현 | — | 의도적 미지원 |

## 5. 보안·운영 요약
팩 버전 주문 고정·진행 run 이전 버전 완주(버전 디렉터리 보존)·
[run] 예산/타임아웃/동시성·초과 시 hold. 비신뢰 findings 경로 격리.
팩 지시문 = 리뷰 대상과 다른 신뢰 수준(스냅샷 인증).

## 6. 구현 순서 (M8)
1. 로더+digest 사슬+스냅샷+정규화 실행기+액션/조건식 파서+**스펙
   예제 전체 유효성 셀프테스트**(위임)
2. review-run 증거·스케줄러·액션 인터프리터(직접 — §2.1 폐쇄 집합만)
3. `skills/bmad-review/v1`(렌즈 3+검증기+policy 이식)
4. e2e: 실 PR → 렌즈 → 조건식 → patch 루프 → hold 실주행
5. `skills/plain-cr-review/v1` — **인수: 액션 조합·판정 어휘·단계가
   전부 다른데 엔진 diff 0**
