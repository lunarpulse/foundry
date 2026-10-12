# Golden trace — bmad-review-full v1.2.0 (부록 A.3)

대상: skills/bmad-review-full/v1 (pack=e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8)
도메인: 전 경로 Inspecting 유지, attempt=1 불변(§2.4a).

**검증은 실행형**(r12-1): tools/verify_golden_trace.py가 **팩 policy.toml을
실제로 파싱**해 조건식을 평가·goto/hold/resume 전이·bump 적용 후
원본 step-04 계약과 대조한다. 수기 분기 미러링이 아니다.

원본 계약: 루프백은 intent_gap·bad_plan 트리거(하위 moot), **루프백 전**
review_loop_iteration 증가, 증가 후 5 초과면 HALT — 팩: loopbacks 단일
카운터, 증가는 route branch do(bad_plan·intent_gap·상한 branch 전부 —
redrive 노드 bump 아님), 인간 hold 진입 시점 카운터=6(6번째 루프백).

| # | verdicts / 카운터 | 원본 분기 | 팩 실행 | ✓ |
|---|---|---|---|---|
| 1 | {patch}, fix_rounds<3 | 같은 구현자 최소 수정 | fixdie(base=latest)→lenses | ✓ |
| 2 | {bad_plan}, lb<5 | KEEP+롤백+비동절 부분 수정 | replan(pre_stage, base=origin, lb++)→lenses | ✓ |
| 3 | {intent_gap}, lb<5 | **되돌림→인간 해결→step-02 전체 재생성** | human_gap→resume("replan_full")→replan_full(base=origin, pre_stage=replan-full-stage, lb++) | ✓ |
| 4 | {defer}만 | 기록 후 진행 | pass + deferred 저널(schema-defer) | ✓ |
| 5 | {intent_gap,bad_plan}, lb<5 | intent_gap 우선 인간 | human_gap | ✓ |
| 6 | {bad_plan,patch}, lb=5 | 상한 HALT | 공통 상한 branch→human, **deferred 기록 억제**(condition=no_loopback, r12-5) | ✓ |
| 7 | {intent_gap}, lb=5 | 상한 HALT | 공통 상한 branch→human(replan_full resume 불가) | ✓ |
| 8 | {patch}, fix_rounds=3 | patch 상한→인간 | human(fix만 resume) | ✓ |
| 9 | 카테고리 없음+findings 없음 | 통과 | pass(적격성 V=ok) | ✓ |
| 10 | 예산 소진 | — | 최우선→human | ✓ |

**반복 전이(r12-1 핵심)**:
- bad_plan 반복(신규 run): replan×5 → 6번째 진입 **human**(lb=5에서
  cap branch 발화) — v0.13의 replans/loopbacks 분리 결함 실증·폐쇄.
- intent_gap 반복: human_gap→replan_full 쌍 ×5 → 6번째 진입 human.

렌즈 구성(r12-2): edge→claims 동일 session(trace) = 원본 "같은 리뷰어가
추적 뒤 claims 확인". verif은 독립 렌즈(원본 verification-gap의 독립
탐색 계약). triage가 전부 after로 수집.
