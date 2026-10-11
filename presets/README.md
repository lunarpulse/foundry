# Foundry 프리셋 작성 가이드 (B1 — M6 보강, M7 T60 선행)

이 문서는 스케일 실증(`74f94be`)에서 검증기에 의해 강제되는 규칙들을
프리셋 작성자를 위해 명시한다. **모든 규칙은 코드가 강제한다** — 이
문서는 왜 거부되는지 설명한다.

## 불변 전제

1. **주문 라이프사이클은 고정·불변**:
   `Received → Queued → Working → Inspecting → FurnaceHold → Publishing → Shipped`
   (실패/거부 엣지: DieFailed 백엣지, Rejected·Canceled 종단).
   프리셋은 이 경로 위의 스텝을 선형으로 걷게 만든다. **경로를 단축할
   수 없다** — 스케일 차이는 역할 구성(어떤 스킬이 어느 엣지를
   담당하는가)으로만 표현된다.
2. **모든 스케일이 인간 게이트를 지난다**: FurnaceHold 도달에는
   InspectionPassed 엣지가 항상 필요하다. "핫픽스라서 검수 생략"은
   프리셋으로 불가능하다 — 그런 정책이 필요하면 부호화 진화(별도
   마일스톤)다.

## 규칙 (검증기 진단 코드 순)

### `default_route_mismatch` — default_route는 자기 id
각 프리셋의 routing.default_route 는 **반드시 자기 workflow id**.
교차 프리셋 default(다른 프리셋을 폴백으로 지정)는 거부된다 — 각
프리셋의 라우팅 테이블은 자기 우주다. 소스 패턴별 선택은 인테이크
계층(M7 오케스트레이션 로그의 preset_bound)이 소유한다.

### `prepare_mixed_branches` / `prepare_no_gate` — Prepare 배치
`policy.approvals[].level = "prepare"` 가 붙을 수 있는 노드는
**도달 상태의 출발 엣지가 전부 승인 패밀리(Approved/Rejected)인
노드**, 실측상 **InspectionPassed 스텝**(도달 상태 = FurnaceHold)뿐.
- DieStarted에 붙이면 → Working의 출구가 비승인 다수 → 혼합 분기 오류
- DieSucceeded에 붙이면 → Inspecting의 출구 InspectionPassed가 만족
  가능 비승인 엣지 → 혼합 분기 오류
- 도달 상태에 출구가 없으면 → no_gate 오류

**작성법**: Prepare를 InspectionPassed 스텝에 붙이고 그 스텝은
skill 없이(또는 advisory로) 둔다. 실례: quickloop의 `quick-inspect`,
feature-prd의 `prd-review`.

### `routing pattern ... invalid` — 소스 패턴 문법
소스 패턴은 접두 와일드카드 형식(`quickfix:*`, `github:*`)만 허용
(라우터 validate_routing이 소유 — 세부는 해당 모듈 문서).

## 스킬 선언

- `kind: "role"` — 저널 이벤트 발행 가능(스텝의 skill 슬롯에 배선 가능)
- `kind: "advisory"` — 산출물만, 저널 권한 없음. **role 슬롯 배선
  불가(구조 차단)** — advisory를 스텝에 배선하면 검증 실패.
- 선언되지 않은 스킬을 스텝이 참조하면 오류. 미참조 스킬 선언은
  허용(advisory 풀).

## encoding_digest

문서의 `body.encoding_digest` 는 **빈 문자열 또는 "enc-frozen-v1"**
(센티넬) 또는 **정확한 live frozen_v1 실측값**만 수용 — 불일치 선언은
"refusing to mask a mismatched declaration" 오류로 거부된다 (T55).

## 배포 핀 (B2)

출하 프리셋은 `crates/foundry-gate/src/preset_pin.rs`에 sha256 상수로
동결된다 — 바이트 변조/무단 편집 시 게이트 실패:

| 프리셋 | sha256 |
|---|---|
| bmad-intake-pool.v3.json | `9000fa16…bb1d1ad` |
| bmad-quickloop.v3.json | `7df2e0eb…338239` |
| bmad-feature-prd.v3.json | `dfc04157…8af5cee` |

프리셋을 수정했으면 핀도 갱신해야 한다(부트스트랩 panic이 실측값을
인쇄한다). **핀 갱신은 배포 승인 행위다** — 커밋 리뷰에서 다뤄진다.

## 작성 체크리스트

1. steps: 고정 경로의 연속 엣지를 건너뛰지 않는 선형 나열
2. 게이트: InspectionPassed 스텝 + Prepare 정책 1개
3. skills: role/advisory 구분 정확히
4. routing: 자기 id default + 소스 패턴들
5. policy_digest: `""` (v3는 policy 객체 사용)
6. 검증: `load_preset` → `validate(frozen_v1)` 통과 확인
7. 배포: preset_pin.rs에 핀 추가 + 게이트 통과

## 거부 코드 진단 (M7 — foundry-orch refusal 레지스트리에서 생성)

거부는 진단이다: 모든 인간 대면 거부는 이유·수락 조건·재디에이션을 동반한다.
이 표는 `foundry_orch::refusal::readme_diagnostic_rows()` 생성값과 일치해야 한다
(테스트 `readme_refusal_table_matches_registry`가 대조).

| 코드 | 이유 | 수락 조건 | 재디에이션 |
|---|---|---|---|
| `UnsizedSpawn` | 사이징 승인 없이 발행된 주문입니다 | 사이징 게이트에서 프리셋이 확정되어야 합니다 (preset_bound) | RunSizing |
| `UndeclaredOrder` | 계획 선언 없는 주문입니다 | plan_declared 행이 이 주문을 포함해야 합니다 | DeclarePlan |
| `DeclarationCycle` | 선언의 의존성에 사이클이 있습니다 | 의존성 간선이 비순환이어야 합니다 | Escalate |
| `MissingPredecessor` | 존재하지 않는 선행 주문을 참조합니다 | 선행 주문이 (같은 선언 안에) 먼저 선언되어야 합니다 | DeclarePlan |
| `DuplicateDeclaration` | 같은 계획이 두 번 선언되었습니다 | 기존 선언을 갱신하거나 새 plan id 를 사용해야 합니다 | DeclarePlan |
| `HandoffMismatch` | 핸드오프 매니페스트가 로그·원장과 일치하지 않습니다 | 매니페스트가 가리키는 로그 행 기준으로 재결속되어야 합니다 | RebindHandoff |
| `GateBypass` | 승인이 게이트를 우회하려 했습니다 (방화벽) | 인간 게이트(dispatch_live + grant)가 유일한 승인 경로입니다 | Escalate |
| `PresetInvalid` | 프리셋이 검증기를 통과하지 못했습니다 | 검증기 진단(예: Prepare 배치·default_route)이 해소되어야 합니다 | Escalate |
