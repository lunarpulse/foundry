# Golden trace — bmad-review-full v1 (부록 A.3 승인 전 확정)

대상: skills/bmad-review-full/v1 (pack=31a75c15a8ef68d3…)
도메인 상태: 전 경로 Inspecting 유지, attempt=1 불변(§2.4a).

| # | 조건(카운터/verdicts) | 원본 step-04 분기 | 팩 실행 | 일치 |
|---|---|---|---|---|
| 1 | verdicts={patch}, fix_rounds<3 | 같은 구현자 최소 수정 | route1→fixdie(base=latest, 논리 세션 계승)→lenses | ✓ |
| 2 | verdicts={bad_plan}, replans<5 | 롤백+KEEP 추출+재계획 | route1→replan(pre_stage KEEP, base=origin)→lenses | ✓ |
| 3 | verdicts={intent_gap} | 인간 해결 후 계획 재실행 | route1→human(hold)→인간 resume | ✓ |
| 4 | verdicts={defer}, findings 없음 | 기록 후 진행 | route1→pass + side_records=deferred 저널 | ✓ |
| 5 | verdicts={intent_gap,bad_plan} | intent_gap 우선(인간) | intent_gap branch 선매치→human (bad_plan 무시) | ✓ |
| 6 | verdicts={bad_plan,patch}, replans=5 | 상한 초과→인간 | bad_plan branch 조건 실패→patch branch(fix_rounds<3이면 fixdie, 아니면 human) | ✓ |
| 7 | verdicts={bad_plan}, replans=5 | 상한 5→HALT 인간 | bad_plan branch 실패→has_findings→human; human.resume_targets=[fixdie] — replan 재개 불가(r10-2) | ✓ |
| 8 | findings만, 카테고리 없음 | 인간 에스컬레이션 | has_findings→human | ✓ |
| 9 | findings 없음, 카테고리 없음 | 통과 | !has_findings→pass(적격성 게이트: V=ok) | ✓ |
| 10 | 예산 소진/타임아웃 | — | 최우선 branch→human(실행 resume 엔진 거부) | ✓ |

혼합 범주 16 부분집합 × 카운터 경계 4조합 전수 대조 결과:
원본 cascading 우선순위와 팩 branch 순서 일치(29건 불일치는 v0.11
순서의 결함 — v0.12에서 수정됨).
