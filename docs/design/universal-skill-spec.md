# Foundry Universal Skill Spec — 스킬=데이터, 도메인=엣지

- **버전**: v0.2 (2026-10-12) — r1 BLOCKER 2/MAJOR 8 전건 반영, 재검증 대기
- **의도(소유자)**: Foundry는 어떤 방법론도 돌릴 수 있는 유니버설 플랫폼.
  BMad는 스킬 데이터의 첫 고객일 뿐 — **Rust 도메인에 방법론 코드가
  박히는 순간이 이 스펙의 실패다.** 엔진 고정·확장 선언(Rust procedural
  macro 원칙). **인수 기준: 판정 어휘·단계 순서·라우팅을 바꿔도 엔진
  코드 0 변경**(plain-cr 추가만이 아니라).

## 0. 측정된 출발점 (r1 실측 정정 포함)

- frozen_v1: 21 엣지, 이벤트·출발·가드·목적지만 정의 — **발행 주체는
  검사 안 함** → Inspector 전환은 엣지 변경 없이 가능.
- 현재 검사 커밋 주체 3곳(전환 대상): 라인 실행자의 순수 판정 커밋
  (line.rs 233), **Inspecting 재진입 무조건 통과(line.rs 307 — 결함
  후보)**, M8 어댑터 연속 발행(stream.rs 705).
- 도메인 attempt: MAX_ATTEMPTS=3(order.rs 182), Die·Inspection 실패가
  같은 attempt 소비. `InspectionFailed` 백엣지 → attempt++, 3 도달 시
  Failed 종단. **FurnaceHold 진입은 InspectionPassed 유일** — 실패를
  FurnaceHold로 보내는 엣지는 없다(r1-BLOCKER-2).
- 프리셋 검사 스텝은 skill 없음(quickloop 등), review는 미배선
  advisory — dsl.rs 343이 advisory role 배선 거부(r1-MAJOR-4).
- 기존 스킬 로더(skill.rs 48): id/interface_version/kind/declares_tools
  해시 → binding digest. SkillEnforcer(183): 도구 호출 경계 검사.
- BMad 원본(버전 고정: ~/.agents/skills/bmad-build, step-04 r84):
  **5회 상한은 intent_gap/bad_plan 루프백(재계획) 상한** — patch 상한
  아님. claims 반증은 Edge Case Hunter **같은 리뷰어의 후속 단계**.
  verification-gap 출력은 Markdown(팩이 정규화 담당).

## 1. 불변 계층 (도메인이 아는 전부 — r1 좁힘)

1. frozen_v1 엣지 세트 (기존, 불변)
2. **일반 증거 참조**: review-run(주문·attempt·artifact digest·pack
   digest 결속) — 리뷰 전용 필드 없음, 저널 이벤트 JSON 페이로드로
3. 두 이벤트: InspectionPassed / InspectionFailed{reason}
4. **정책 파서: 키-값의 존재·타입만**(값의 의미 해석은 §3 인터프리터)
5. findings 정규화기: 팩이 선언한 출력 스키마(정규화 규칙 포함)를
   공통 4필드(location/trigger/guard/consequence)로 변환 — 렌즈별
   확장 필드는 팩 스키마 소유

**도메인 금지 목록**: 판정 어휘(high/medium/...조차), 라우팅 종류,
루프 의미, claims 순서, 렌즈 이름 — 전부 팩 데이터.

## 2. 스킬 팩 포맷 (전부 데이터, 버전 있음)

```
skills/<pack-id>/v<N>/
  manifest.toml    [skill] id, interface_version, kind="advisory-pack",
                   declares_tools=[] , pack_version, requires
                   digest = pack digest (self 필드 제외·계산식 아래)
  policy.toml      §2.1 전체 참조
  lens-*.md        지시문(파싱 안 함 — 경로 전달)
  schema-*.toml    렌즈별 출력 스키마+정규화 규칙
  fixtures/        셀프테스트 샘플
```

**Digest 사슬 (r1-MAJOR-5)**: `pack digest = SHA256(정렬된
(경로,내용) 쌍: lens-*·policy.toml·schema-*.toml·참조 파일 전부,
self-digest 필드와 fixtures 제외)`. 계약: `preset.skills[].digest ==
manifest.digest == 계산값` — 어느 하나 불일치 시 로드 거부. 검증은
①로드 시 ②디스패치 직전 재계산(교체 방지) 2회. 프리셋 Merkle root·
B2 재핀은 기존 절차 승계(팩 교체 시 프리셋 재핀 필수).

### 2.1 policy.toml (모든 의미 결정이 여기 산다)

```toml
[review]
enforcement = "required"          # advisory=증거만 | required=결과가 전이 결정
on_timeout = "fail"               # fail | pass | escalate (정책 어휘, 팩 정의)
on_dispatch_error = "escalate"
on_invalid_output = "fail"
on_partial_results = "use-valid"  # 병렬 렌즈 일부 실패 시
selection = "fixed:thorough"      # fixed:<set> | by-diff:<임계값표> — BMad auto는
                                  # 팩이 by-diff로 재현(원본은 경로 기반 — 차이 명시)
loop_max = 2                      # InspectionFailed 백엣지 상한 — 도메인
                                  # MAX_ATTEMPTS(3) 이하만 허용, 초과 선언은 로드 거부
escalation = "hold-for-human"     # 인간 개입: Inspecting 유지 + 관측 알림으로
                                  # 인간 호출. 인간 승인 API가 InspectionPassed/
                                  # Failed 커밋(유일한 비-Inspector 커밋 주체)
[stages]                          # 단계 의존성 그래프 (r1-MAJOR-7)
trace   = { lenses = ["edge-case"], gives = ["trace-done"] }
claims  = { lenses = ["claims"], after = ["trace-done"], same_reviewer = true,
            inputs_unsealed = ["claims"] }   # claims 입력은 trace 완료 후 개봉
others  = { lenses = ["verification-gap"], parallel_with = ["trace"] }

[verdicts]                        # 판정 어휘 — 팩이 정의 (r1-BLOCKER-1)
values = ["high", "medium", "low", "false", "maybe-false"]
unknown = "maybe-false"           # 불확실 시 채택값
[verdicts.verify]
enabled = true                   # findings 실측 검증(검증기도 팩 지시문)
[route]                          # 라우팅 우선순위 — 팩 소유
order = ["patch", "replan-human", "fail"]
rules = [
  { verdicts = ["high","medium","low"], group_by = "root-cause", to = "patch",
    max_retries = 3 },           # 같은 Die 재시도 — review-run 카운터(도메인 아님)
  { verdicts = ["maybe-false"], to = "replan-human" },
]
[run]
lens_timeout_s = 600 ; total_timeout_s = 3600
max_calls = 12 ; max_concurrency = 3
budget_tokens = 2_000_000        # 예약제: 초과 시 on_budget="escalate"
[output]
common_fields = 4 ; ext_field_max_bytes = 4096 ; invalid = "reject-finding"
```

**런타임 인터프리터의 범용 연산(전부)**: 그래프 스케줄(stages 해석),
경로 전달, barrier 수집, 카운터·예산 집행, verdicts.values 집합
비교, route.order 순회. **어휘·임계값·순서를 코드가 모른다.**

### 2.2 시맨틱 표 (r1-BLOCKER-2 — frozen_v1 준수)

| 정책 결과 | 발행 이벤트 | 목적 상태 | 카운터 |
|---|---|---|---|
| 모든 판정 통과(patches=0) | InspectionPassed | FurnaceHold | — |
| patch (재시도 < max_retries) | InspectionFailed{findings} | Working(백엣지) | 도메인 attempt 소비 + review-run+1 |
| 루프 상한/escalation | **없음** | **Inspecting 유지** | review-run 고정, 인간 대기 |
| 도메인 MAX_ATTEMPTS 도달 | DieFailed 경유 | Failed(종단) | 재주문은 Chainer·선언·승인 절차 |

- **도메인 attempt와 review-run 분리**: review-run(팩 카운터, 저널
  내구 기록·재시작 복원)은 정책 상한용, 도메인 attempt는 frozen_v1
  그대로. 정책 loop_max ≤ MAX_ATTEMPTS 강제(로드 거부).
- FurnaceHold 승격 = BMad 재계획 루프백의 **수동 대체** — 자동
  재계획을 포기하고 인간 소유로 넘김(더 느리고 더 엄격, 등가 아님).

## 3. 실행 계약

Inspecting 진입 시(권한 있는 실행자만 — Inspector 서비스):
1. preset 스텝의 **review_pack 참조**(신규: 프리셋 스키마 v4 필드,
   마이그레이션+재핀 필수 — v3 조용한 변경 금지, r1-MAJOR-4) → 팩
   로드(digest 사슬 2회 검증)
2. review-run 생성: 주문·attempt·artifact digest·pack digest 결속,
   저널 내구 기록(재시작·중복 응답·지연 응답 멱등 키)
3. stages 그래프 스케줄 — 디스패치는 **DB 쓰기 락 밖**(r1-MAJOR-9;
   검증 클로저는 결과 확인만, 잠금 내 마이크로초 단위). 리뷰어:
   read-only 샌드박스 + SkillEnforcer 계약 승계(interface_version/
   kind/declares_tools — 파일 범위·심링크·네트워크·하위 에이전트·
   저널 권한 차단). **findings는 검증 전 비신뢰 데이터**.
4. 정규화(팩 스키마) → verdicts.verify → route 순회 → §2.2 표 적용
5. 전 과정(입력 digest·findings·판정·라우팅 근거·카운터) 저널 기록

**관측/필수 분리**: 알림 실패 무차단 유지. `enforcement="required"`
에서 **필수 리뷰 증거 저장 실패**는 별도 — on_dispatch_error 정책이
결정(기본 escalate). advisory 팩: 증거만 남고 전이는 기존 경로.

**기존 경로 전환**: line.rs 233(판정 커밋)·307(재진입 무조건 통과 —
Inspector 유일 커밋 원칙으로 제거, 회귀 테스트로 고정)·stream.rs 705.

## 4. BMad 팩 충실도 (원본 고정: bmad-build step-04 r84)

| BMad 원본 | Foundry 표현 | 동등성 |
|---|---|---|
| 렌즈 독립 병렬 + after 의존 | stages 그래프 | 동등 |
| claims: 같은 Edge-Case 리뷰어 후속 단계 | same_reviewer=true, inputs_unsealed | 동등 |
| findings 4필드+kind/confidence | 팩 스키마+확장 필드 | 동등 |
| 트리아지 실측 검증·carried·그룹화 | verdicts.verify+group_by (carried: review-run 저널 대조 — v0.2는 원본 carried를 "재런 시 저널 대조"로 근사, 원본의 세부 룰은 verify 지시문에 이식) | 근사(지시문 이식) |
| patch 재시도(같은 구현자 재호출) | route patch + 같은 Die 재시도 | 동등 |
| 5회 루프백 상한(재계획) | loop_max + escalate | **수동 대체**(자동 재계획 포기 — 인간이 KEEP/수정 판단) |
| intent_gap/bad_plan 코드 되돌림+재유도 | replan-human | 수동 대체 |
| defer | 저널 deferred + 재주문은 인테이크 | 변형 |
| quick/thorough/auto(경로 기반) | selection fixed/by-diff | 변형(diff 크기 기반 — 원본은 경로) |
| 상태머신 자체 재현 | — | 의도적 미지원(엣지 채움으로 재현) |

## 5. 보안·운영

- 팩·인터프리터 버전은 review-run 생성 시 주문에 고정; 진행 중 팩
  교체 시 진행 run은 이전 버전으로 완주(팩 디렉터리 버전 분리로
  보존), 신규 주문만 새 팩. 마이그레이션 규칙 명시적 문서화.
- review-run 중복 응답: 저널 멱등 키로 폐기. 재시작: 내구 카운터로
  재개(스케줄 재구성, 완료 렌즈 재실행 없음).
- 팩별 타임아웃·호출·토큰·동시성·예산 예약(§2.1 [run]) — 초과 시
  on_budget 정책(기본 escalate).

## 6. 구현 순서 (M8)

1. 로더+digest 사슬+정책 파서+셀프테스트(위임) — 파서는 존재·타입만
2. review-run 증거·인터프리터(직접) — 그래프 스케줘러·verdicts 비교·
   route 순회만 (§2.1 "범용 연산" 명단 밖 금지)
3. `skills/bmad-review/v1` 팩 실현(렌즈 3·스키마·policy 이식)
4. e2e: 실 PR 렌즈 리뷰 → patch 루프 → escalate 실주행
5. `skills/plain-cr-review/v1` — **인수: 판정 어휘·라우팅·단계가
   bmad-review와 다른데 엔진 코드 diff 0**
