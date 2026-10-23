# B1 W3 Gate3 — 계획·루브릭 v1 (동결 대기 초안)

작성: 2026-10-23. 상태: **FROZEN — 사용자 승인 Discord 1563171600449413234 ("권고대로."): 결정 1-A(G17 정식 면제 종결), 결정 2-A(T3 역학 전용 fixture 허용·역사 증거 계속 부인), 루브릭 v1 동결.**
이 문서 전문이 동결 루브릭이며, 이후 측정은 이 표의 행별 단언으로만 판정한다. 행 추가/변경은 사용자 승인으로만. 루브릭 외 단언은 Gate1 progress_revision 사례와 동일하게 "over-extension"으로 기각·제거 대상.

## 0. 전제와 출처

- Gate2 인수 완료: HEAD `8aba8b7` = origin/main, 루브릭 v1 R1~R9 전 PASS(sol round103 APPROVE).
- Gate3 정의: `.hermes/plans/2026-10-20_105122-b1-w3-acceptance-and-transition.md` §6 "crash·ambiguous recovery·migration·isolation 묶음".
- 동결 계약: `.hermes/plans/b1-product-result-package-spec.md` v3.
- G17 이월 상태: Gate1 루브릭 "G17 … **Gate3 fixture 의존성 결정 필요**", Gate2 루브릭 §0 "G17 Gate3 웨이브 면제".
- 부모 실측(2026-10-23): `git log --all -S "product_meta_v1"` — 저장소 전 역사에 v1 DDL 생성/변경 커밋 부재(product.rs 커밋 1개뿐). historical v1 DDL·host bytes는 구조적으로 입수 불가 확정.

## 1. 현재 측정 기저 (변경 불요 — 그대로 인정)

| 항목 | 상태 |
|---|---|
| b1_product_result_processes.rs | 7 시험 — SIGKILL before/after commit, 승인 SIGKILL 참여자 boundary, 경합 단일 승인, 동일 키 exact receipt replay, stage-start 후 철회 |
| b1_product_atomic.rs | 15 시험 — hold receipt insert 실패 rollback, prepared 만료·recovery fence, 권한 변경 직렬화 등 |
| b1_scope.rs | 4 시험 — v1_maintenance_migration(:541, v3→v1 downgrade 구성, atomic+unknown order 격리) |
| round100 9레인 | 전 exit 0 (Gate2 종결 기준) |

## 2. Gate3 루브릭 v1 (행별 단언 — 승인으로 동결)

측정 주체: L=ledger/line 시험, P=부모 직접, X=외부 컴파일 probe. 각 행 PASS 요건은 "나열된 단언 전부가 실측"이다.

- **T1 existing_crash_and_race_responsibilities_preserved_on_changed_fixture (P)** — §6 첫 줄: 기존 결과/승인 SIGKILL·경합 시험(processes 7 + atomic 15의 해당 행)이 Gate2 후 fixture(result.rs 결함 #6 반영본)에서 전부 그대로 통과. 시험 삭제·ignore·skip·이름 변경·count 감소 0. 단순 재실행이 아니라 책임 단언이 그대로 살아있는지 mapping 대조로 확인.
- **T2 ambiguous_and_io_failure_cases_are_three_distinct_measurements (L)** — §6 둘째 줄: ①응답 손실(COMMIT 성공·응답 상실)→재요청은 exact receipt replay(재실행·재승인 아님, 참여자 delta 0) ②실제 operation 위치의 COMMIT-I/O 실패(파일시스템 오류 주입)→전량 rollback·부분 상태 0, I/O error는 absence(Ok(None)/404)와 구분되는 오류 분류 ③복구 lookup-I/O 실패→오류 전파, 부재와 혼동 금지. 셋은 서로 다른 시험 셀로 실측하고 SIGKILL 하나를 셋의 증거로 대체하지 않는다. receipt/불확실성 구분 단언 포함.
- **T3 migration_cas_atomicity_census_sealing_and_history_preservation (L)** — §6 셋째 줄: **`migrate_product_results_v2`(result.rs:508, v2→v3 결과 마이그레이션)**의 ①DDL/population/CAS 실패 시 부분 마이그레이션 0(원자 rollback) ②host/tuple census 봉인(legacy_hosts/legacy_tuples 이행+6개 봉인 트리거) ③cutoff 경계(`legacy_approval_cutoff_seq`=마이그레이션 시점 MAX(seq), 포함·제외 양측 독립 단언) ④기존 history bytes·receipt 보존 ⑤missing binding은 corruption으로 구분. *(2026-10-23 오기 정정 승인 Discord 1563174189370704035 — 초안이 `migrate_scope_v1`(v1→v2 scope)로 오기재했으나 luna97 실측으로 cutoff·census는 본 연산에 존재함이 확인됨. 5개 하위 단언 무변경, 역학 전용 fixture도 b1_product_results.rs:57의 v2 downgrade 구성으로 정정.)*
- **T4 p2_isolation_compiles_nowhere_by_default (X,P)** — §6 넷째 줄: default 특성에서 타입 부재 E0432 + 메서드 부재 E0599 컴파일 반례, enabled 특성에서 성공(별도 실행, 기본 빌드 통과로 대체 불가); 정상 HTTP/CLI/startup 경로에서 adapter 진입 불가 실측. Gate2 R8 probe 문서화 승계·확장.
- **T5 regression_nine_lanes_preserved (P)** — §6 다섯째 줄: round100 9레인 정의 그대로 전 exit 0. H/HX 옛 단언 mapping 전수 유지, count 감소로 green 처리 금지.

완료 기준: T1~T5 전부 PASS(또는 승인된 면제 행은 근거 문서화로 갈음). 셀 판정은 이 표 행별 단언 인용으로만.

## 3. G17 최종 처리 제안 (승인 항목 1)

- **사실**: Gate1 G17의 잔여 측정치는 "legacy missing-snapshot `Ineligible`" 단 하나. 요건은 faithful historical host/descendant + 원본 history bytes 보존. 저장소 입력(현재 코드·git 전 역사)에 v1 DDL·historical DB·원본 bytes 전부 부재 — 2026-10-23 부모 재확정. ADDENDUM79 및 Gate1 판정들은 fresh-v3 downgrade·hand-crafted bytes를 생산 증거로 부인. 현행 대조군(modern missing-initial/missing-copy `CorruptEvidence`)은 Gate1~2에서 이미 실측·PASS.
- **제안 A(권고)**: G17 legacy cell을 **구조적 불가 사유로 정식 면제 종결** — 증거 문서에 위 사실 chain(git 실측 포함)과 계약상 제한으로 기록. 이는 루브릭 행 약화가 아니라 측정 불가 선언이며, T3의 migration 역학 검증과는 독립.
- **대안 B(비권고)**: 수제 v1 DB로 실측 — ADDENDUM79·Gate1 판정과 직접 충돌, "생산 이력 bytes" 위조에 해당.

## 4. T3 fixture 허용 범위 제안 (승인 항목 2)

- **쟁점**: 마이그레이션 역학(원자성·census·cutoff·보존) 검증에는 v2 상태 DB가 필요한데, 저장소에서 유일한 조성법은 in-tree downgrade 구성 — G17 역사 증거로는 부인된 바 있음.
- **제안 A(권고, 승인됨)**: **증거 종류 분리** — downgrade 구성 fixture(b1_product_results.rs:57의 v3→v2 구성; 초안 표기 b1_scope.rs:541은 오기로 1563174189370704035로 정정)는 "migration 연산 역학 검증 전용"으로 허용(T3 확장), G17 historical 증거로는 계속 부인(문서로 명시). 역학이 실생산 v1에서만 발현되는 성질이 아니라는 근거(연산 자체가 DDL·CAS·census 절차이므로 입력 조성법과 무관)를 증거 문서에 기재.
- **대안 B(비권고)**: 부인 유지 → T3 전행 면제. §6 셋째 줄 요구를 사실상 폐기하는 결과.

## 5. 배치 계획 (동결 후 순차)

- **배치 1 (luna97, ADD89)**: 시험 중심 확장 — T2 3분리 셀 신설(또는 기존 미측정 입증), T3 확장(결정 2 승인 시), T4 probe 확장. 생산 결함 발견 시 즉시 중단·보고(결함 #7+ 승인 요청). 프리셋 bounds 그대로(--locked --offline, jobs1, incremental0, 2GiB guard, scratch temp, B1_LEDGER_TEST_EXE).
- **검증**: 부모 9레인(T1·T5) + 신규 셀 재판정 → 증거 문서 Gate3 조항.
- **sol 리뷰**: Gate3 델타 read-only, 판정 APPROVE 시.
- **인수**: T1~T5 전 PASS(+면제 행 근거) + sol APPROVE → 사용자 인수 → 단일 커밋 push(별도 승인).
- **루프 방지**: 정정 사이클 1회 상한 원칙 동일 — 2회째 필요 시 즉시 중단·보고.

## 6. 명시적 결정 항목 (지금 승인받을 것)

1. **G17 최종 처리**: A 정식 면제 종결(권고) / B 수제 실측(비권고).
2. **T3 fixture**: A downgrade 구성의 역학 검증 전용 허용·역사 증거 계속 부인(권고) / B 부인 유지→T3 면제.
3. **본 문서의 루브릭 v1 동결 승인** (행 추가/변경은 이후 사용자 승인으로만).
