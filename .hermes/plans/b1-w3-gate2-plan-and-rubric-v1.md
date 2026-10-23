# B1 W3 Gate2 — 계획·루브릭 v1 (동결 대기 초안)

작성: 2026-10-23. 상태: **FROZEN — 사용자 승인 Discord 1562994515545956506 ("1a 2a 3 승인"): 결함 #6 옵션 A(result.rs 수정), CLI root (a) 생산 무변경+근거 문서화, 루브릭 v1 동결.**
이 문서 전문이 동결 루브릭이며, 이후 측정은 이 표의 행별 단언으로만 판정한다. 행 추가/변경은 사용자 승인으로만. 루브릭 외 단언은 Gate1 progress_revision 사례와 동일하게 "over-extension"으로 기각·제거 대상.

## 0. 전제와 출처

- Gate1 인수 완료: HEAD `09cd28b` = origin/main, 루브릭 v1 13/13 PASS, G17 Gate3 웨이브 면제.
- Gate2 정의: `.hermes/plans/2026-10-20_105122-b1-w3-acceptance-and-transition.md` §5 "authenticated consumer·root·integrity 묶음".
- 동결 계약: `.hermes/plans/b1-product-result-package-spec.md` v3 (974/1215/1219/1231/332/879/974/1127/1231행; Gate2 직결: **548행** no-partial-success, **1215행** stored-set 검증기 = "artifact provenance and confined bytes in one DB snapshot", **658행** PT-10).
- 현재 측정 상태: ADD86(luna94, TEST-ONLY, 파일 2개) + round97 부모 재판정 완료. 코드는 작업 트리에 반영(미커밋).

## 1. round97 확정 사실 (변경 불요 — 그대로 인정)

| 항목 | 결과 |
|---|---|
| b1_auth_http | **3/3 green** |
| b1_product_artifacts | 2/3 — G-final 시험만 실패(아래 결함 #6) |
| loopback 소켓 C 시험(`product_result_read_and_approval_routes_use_a_real_loopback_socket_when_available`) | **부모 환경 green** — 실 TCP bind→login/metadata/artifact/grant/approve→exact replay→reopen 재시도 전부 실소켓 통과 |
| cli-enabled | 2/0 + 5/0 green (b1_scope_cli 샌드박스 차단분 해소 확인) |
| cli-default | 9/0 green |
| 생산 소스 | round97 기준 무변경 (luna94 편집 = 시험 2개 파일만) |

## 2. Gate2 루브릭 v1 (행별 단언 — 승인으로 동결)

측정 주체 표기: S=serve 시험, C=CLI 시험, P=부모 직접(샌드박스 불가), X=외부 컴파일 probe. 각 행 PASS 요건은 "나열된 단언 전부가 실측"이다.

- **R1 bound_result_artifact_reads_require_original_source_grant (S)** — 200: no-store + x-foundry-result-id/artifact-id/tuple-digest 헤더 + 본문 bytes 일치; bearer 부재 401; foreign order/존재하지 않는 result·artifact ID 404; storage(root 미설정) 503 + 본문에 경로/내부정보 무; retired source 이후 기존 grant reader 200·신규 source-only 계정 404; publication(consumption/outcome/publishing) delta 0. *(luna94 구현 + round97 부분 green; SHA 셀은 R3에서)*
- **R2 retired_source_history_remains_readable_without_new_source_inheritance (S)** — G-final 후 source rebind: 기존 explicit ReadArtifact 세션은 metadata+artifact 200 원본 bytes; 신규 source-only 계정은 metadata·artifact 전부 404; 승인/발행 부수효과 0. *(luna94 구현됨 — G-final 시험 내 포함)*
- **R3 each_result_integrity_mutation_starts_from_verified_baseline (S)** — 각 변형(파일 bytes / artifact-SHA / artifact-provenance / artifact-path / tuple / host-result / binding)마다: 변형 전 200 baseline → 변형 후 **metadata 404 및 artifact 404**(uniform 본문) → 원복 후 200 원본 bytes. unauthorized+변형 조합도 동일 404. *(tuple/host/binding/file-bytes green; **artifact-SHA 셀이 결함 #6으로 실패 중**; path/provenance는 SHA 셀 뒤 미도달)*
- **R4 result_reads_preserve_pinned_root_and_no_follow_guards (S, PT-10=계약 658행)** — leaf symlink 거부; **root 교체(symlink→외부 디렉터리) 시 원본 bytes 또는 404, 외부 bytes 0**; ancestor(root 상위) 교체 동일; FIFO leaf 404; device leaf(c 1 3) 404; 각 셀 후 원복→200. 미지원 플랫폼은 fail-closed(404) 단언. *(leaf symlink·missing→복원 green; root 교체 구현됨·미도달, ancestor/FIFO/device 구현됨·미도달 — 전부 G-final 시험 내, SHA 셀 뒤)*
- **R5 product_result_read_and_approval_work_over_authenticated_loopback_server (C,P)** — 실제 loopback TCP: login→metadata 200→artifact 200 bytes→401→grant 201→approve 200→동일 요청 재시도 exact response replay→무효 intent 거부(4xx)→서버 reopen 후 동일 승인 재요청 exact replay; producer 호출 counter는 baseline 유지, restore 0. *(round97 부모 green — 종결)*
- **R6 w3_paths_never_invoke_publisher_or_create_publish_start (S)** — 읽기(metadata/artifact) 경로 전후 ledger `to_state='publishing'` 0행, consumption/outcome 0행. **승인 경로**: 성공 승인은 계약 563행이 의무하는 승인 참여자(consumption·outcome) **각 정확히 1행** 생성, 거부·재시도·재개·reopen은 추가 0행. *(2026-10-23 정정 승인 Discord 1563076393028558919 — 초안 문안이 성공 승인까지 0행을 요구해 계약 563행과 충돌했음을 sol round99이 지적, 계약 부합 문안으로 정정. 행 외 나머지 무변경.)*
- **R7 result_error_precedence_does_not_reveal_foreign_evidence (S)** — unauthorized+변형 조합 uniform 404 동일 본문; approval 라우트 corrupt-winner+expected 불일치 동일 외부 거부·소비 0. *(auth_http 3/3 green + G-final 내 조합 셀)*
- **R8 p2_support_has_no_product_http_or_cli_entrypoint (X,P)** — 외부 probe crate: default 특성에서 `P2ReviewPreparation` import E0432 + `prepare_p2_review_fixture` E0599; `support` 특성에서 컴파일 성공. probe 명령은 b1_product_artifacts.rs 헤더에 문서화 완료. *(부모 실행 예정 — 미측정)*
- **R9 기존 회귀 보존 (P)** — round96 9레인 정의 그대로 전부 exit 0 (h-hx 41/0, ledger 4/0, core 406/0, watch 17/0, http 4+2/0 ← 신규 2개 시험 포함해 6/0, restore 10+10/0, cli 7+9/0).

완료 기준: R1~R9 전부 PASS. 단, G-final 단일 시험 내 미도달 셀들은 결함 #6 수정 후 자연 도달하며, 도달 시점에 각 셀이 이 표 단언과 충돌하면 **루브릭 우선, 시험 수정은 이 문서의 배치 2 절차로만**.

## 3. 결함 #6 판정 제안 (승인 항목 1)

- **증상**: `product_artifacts_v1.artifact_sha256` 변형 시 artifact 라우트는 404, **metadata 라우트는 200** (round97 부모 재현: b1_product_artifacts.rs:1163 `left: 200, right: 404`).
- **원인**: `read_product_result`(result.rs:1286)는 binding/tuple/host만 검증, artifact 행(provenance/SHA)·confined bytes 미검증.
- **계약 근거**: 548행 "result metadata를 먼저 반환하고 파일 오류를 나중에 보고하는 부분 성공은 허용하지 않습니다" + 1215행 검증기 명세가 "artifact provenance and confined bytes in one DB snapshot" 포함.
- **제안 수정 설계 (result.rs 단일 파일)**: `read_product_result`가 `read_stored_result_in` 통과 후, payload가 규정하는 각 artifact에 대해 ① `product_artifacts_v1` 행의 provenance_digest/artifact_sha256이 payload 기대와 일치 ② `review::read_confined_artifact`로 confined bytes 다이제스트 검증 — 행 불일치/본문 불일치 → `CorruptEvidence`(→404), root 미설정/IO → `Storage`(→503), 트리거 원칙 그대로. 공개 API·스키마·의존성 무변경.
- **부수 확인 필요**: metadata 200이던 기존 green 시험 중 root 미설정 상태로 metadata만 호출하는 경로가 있다면 503으로 바뀜 — 배치 1에서 전 레인 재측정으로 확인(회귀면 시험 측 fixture에 root 설정 보강, 단언 완화 아님).
- **대안**: B(시험 완화)는 계약 문면과 충돌, C(계약 개정)는 동결 위반 — 둘 다 비권고.

## 4. 배치 계획 (동결 후 순차, 배치당 1회 커밋 원칙은 인수 시점에)

- **배치 1 (생산+시험 완결, 부모 직접 수정 — luna85/86 이후 원칙)**: ① 결함 #6 수정(result.rs) ② G-final 시험 미도달 셀 전량 도달 확인(path/provenance/root/ancestor/FIFO/device/publisher) — 셀 실패 시 루브릭 대조 판정 후 같은 배치에서 시험 정정(over-extension) 또는 결함 #7+ 승인 요청으로 분리 ③ R8 probe 실행 ④ round96 9레인 재실행(R9).
- **검증**: 부모 9레인 전 green → 증거 문서 Gate2 조항 신설(rounds 94–99 체인).
- **sol 리뷰**: Gate2 델타(ADD86 시험 2파일 + result.rs #6) 대상 read-only 리뷰, 판정 APPROVE 시.
- **인수**: 루브릭 v1 R1~R9 전 PASS + sol APPROVE → 사용자 인수 결정 → 단일 커밋 push(별도 승인).
- **루프 방지 규칙**: 배치 1은 최대 1회의 정정 사이클만 허용. 2회째 정정 필요 사항이 발생하면 즉시 중단·보고(사용자 발산 우려 대응). 모든 셀 판정은 이 문서 행별 단언 인용으로만.

## 5. 명시적 결정 항목 (지금 승인받을 것)

1. **결함 #6**: 위 §3 설계대로 result.rs 수정 (권고 A) / 시험 완화 B / 계약 개정 C.
2. **CLI approve artifact-root 패리티(main.rs product_approve_cmd)**: 계약상 승인 경로는 저장-set 무결성만 요구(1215행 "Approval consumes that validated stored result"), 파일 검증 의무 없음 → **(a) 생산 무변경 + 근거 문서화(권고)**. (b) serve와 동일 env 패리티 추가는 불필요한 표면 확장.
3. **본 문서의 루브릭 v1 동결 승인** (행 추가/변경은 이후 사용자 승인으로만).
