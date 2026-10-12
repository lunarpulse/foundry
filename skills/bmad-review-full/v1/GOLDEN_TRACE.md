# Golden trace — bmad-review-full v1.1.0 (부록 A.3)

대상: skills/bmad-review-full/v1 (pack=22c90f9affd40b8dce351a65519449ba7d66373ca75587f7fcea5f6b5035681b)
도메인: 전 경로 Inspecting 유지, attempt=1 불변(§2.4a).

원본 step-04 계약(50–110행): 루프백은 intent_gap·bad_plan 트리거,
하위 항목 moot. **루프백 전에** review_loop_iteration 증가, 5 초과 시
HALT 인간 — intent_gap 루프백에도 동일(원본 "Before each loopback").
팩: loopbacks 카운터가 공통 상한.

| # | verdicts / 카운터 | 원본 분기 | 팩 실행 | ✓ |
|---|---|---|---|---|
| 1 | {patch}, fix_rounds<3 | 같은 구현자 최소 수정 | fixdie(base=latest)→lenses | ✓ |
| 2 | {bad_plan}, loopbacks<5 | KEEP+롤백+재계획 | replan(pre_stage, base=origin, loopbacks++)→lenses | ✓ |
| 3 | {intent_gap}, loopbacks<5 | 인간 해결→계획 재실행 | human_gap→resume("replan")→pre_stage→replan | ✓ |
| 4 | {defer}만 | 기록 후 진행 | pass + side_records=deferred(schema-defer.toml) | ✓ |
| 5 | {intent_gap,bad_plan}, lb<5 | intent_gap 우선 인간 | human_gap(bad_plan moot) | ✓ |
| 6 | {bad_plan,patch}, lb=5 | 상한 HALT 인간 | 공통 상한 branch→human(누수 없음) | ✓ |
| 7 | {intent_gap}, lb=5 | 상한 HALT(loopback 전 검사) | 공통 상한 branch→human(human_gap 아님) | ✓ |
| 8 | {patch}, fix_rounds=3 | patch 상한→인간 | patch branch 실패→has_findings→human(fix만 resume) | ✓ |
| 9 | 카테고리 없음+findings 없음 | 통과 | pass(적격성 V=ok) | ✓ |
| 10 | 예산 소진 | — | 최우선→human(실행 resume 거부) | ✓ |

16 부분집합 × fix_rounds∈{2,3} × loopbacks∈{4,5} 전수 대조(64조합):
원본 기대와 전부 일치 — tools/verify_golden_trace.py가 전수 생성·
검증(구현 셀프테스트 편입). v0.12의 2건 누수({bad_plan,patch} 상한)와
초안의 intent_gap 상한 누락은 공통 상한 branch로 폐쇄.
