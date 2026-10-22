# B1 W3 인수 및 W4 전환 계획

**Goal:** 회귀 복구를 W3 완료로 오인하지 않고, 동결 v3의 전체 유효 인수 행렬을 증거로 닫은 후 W4로 전환한다.
**Architecture:** 기존 ProductStore/PackReviewHost와 genuine test producer를 유지한다. producer→저장 증거→인증 consumer의 전체 경로 및 실패 원자성을 검증한다. 새 정책/validator 완화/수동 modern 결과 seeding은 금지한다.
**Tech Stack:** Rust/Cargo, SQLite, 실제 제품 CLI·loopback 제품 서버, scripted fake executor.
**실행 규칙:** 이 문서는 계획만이며 코드 변경·codex 기동 승인이 아니다. 프로젝트의 순차 codex CLI luna→부모 검증→통합 sol 리뷰 절차가 일반 plan 스킬의 subagent/빈번한 커밋 예시보다 우선한다. B1 리뷰·최종 승인·커밋·push는 배치당 1회. 내부 checkpoint는 커밋이 아니다.

## 1. 출처와 실측 기준

- 동결 정본: `.hermes/plans/b1-product-result-package-spec.md`; precedence v3/D > v2/R1–R7/C > D1–D8 > B > A. 승인1561496539174801409.
- 전체 인수 목록: `.hermes/plans/b1-w3-recovery-acceptance-matrix.md`의 모든 유효 row. 기존 파일의 REQUIRED 상태를 신규 PASS로 추정하지 않는다.
- 부모 round67 실제 로그: `/home/cosmo/.hermes/cache/scratch/b1-parent-round67-{cli-enabled,cli-default,shared-host,http-enabled,core,watch}.log`. 각각2/9/1/3/401/17 passed, 전부0 failed/ignored와 exit0. 중복 실행 lane을 합해 독립 인수 수라고 부르지 않는다.
- HEAD36e994c. B1 미커밋 누적. graphify report/wiki 부재 확인; 소스 열람 근거이지 graph 검증 완료 아님.
- 실제 코드 잔여 후보: `crates/foundry-ledger/src/product/result.rs:789`의 run_commit vs host.result_commit 비교, :1054 accepted_holds 빈 벡터. 원인/수정 설계는 continuation 생산 경로와 함께 검증하며 조건을 단순 삭제하지 않는다.
- root 의심: CLI main.rs의 product_approve_cmd가 configure_product_artifact_root 없이 open→prepare/approve. 결과 저장 검증 `result.rs:2030-2120`에는 authority/work/host/S1/tuple/receipt 검증이 있으나 이것만으로 artifact byte 검증을 입증하지 못한다.

## 2. 판단 방식과 이유

회귀 green은 기존 연결이 다시 작동한다는 증거이고 전체 계약 충족 증거가 아니다. 따라서 파일별 오류를 하나씩 쫓는 운영에서 전체 행렬에 대한 GAP 명세→세 묶음 구현→한 번의 W3 인수 판정으로 전환한다. 단, 계약 변경·제품 수정 범위 확대·실외부 연결은 중간에 사람에게 되돌린다.

고정된 기존 유효 행렬을 denominator로 쓴다. 각 row는 PASS/GAP/BLOCKED, 실제 test target/name, baseline, mutation/barrier, 명령/log/exit, 관측값, 부모 판정으로 기록한다. 테스트 이름 존재나 합계 green은 PASS 근거가 아니다. superseded row만 정본의 명시 근거로 제외 가능하다. 인수 기준은 유효 row 전부 PASS와 GAP/BLOCKED0이며 현재 비율/남은 개수는 전수 대조 전 산정하지 않는다.

## 3. Gate 0 — 잔여 목록 확정 및 제품 변경 범위 승인

읽기 전용 전수 대조를 수행한다. 허가 후 codex astra CLI read-only로 계획의 harness/명령/계약 정합성만 독립 확인하고, 부모가 결과를 판정한다. API 직접 호출 금지.

새 증거 장부 후보: `.hermes/plans/b1-w3-acceptance-evidence.md` (이 계획 단계에서는 생성하지 않음). 최근 통과 항목도 실제 assertion mapping으로만 PASS 전환한다. root는 아래 세 경우를 분리한다.
1. DB의 root 경로/device/inode가 보존됨.
2. consumer가 원래 root handle을 configure하고 사용함.
3. consumer가 변조·교체를 거부함.
현재 CLI positive는1을 보였고2/3은 미입증이다. root env만 추가하면 해결된다고 가정하지 않는다. 승인/metadata/replay에 필요한 stored-set 무결성과 파일 검증 범위는 동결 정본으로 확정한다. 명세 모호성이면 정책을 임의 발명하지 않고 별도 결정 요청한다.

Exit: 전체 row 장부와 정확한 변경 whitelist·API/스키마 영향·기존 데이터 영향이 준비되고 사용자가 구현 범위를 승인함. 새 스키마 변경을 기본값으로 가정하지 않는다.

## 4. Gate 1 — producer·continuation·authority 묶음

주요 경로: `crates/foundry-ledger/src/product/result.rs`, 필요 시 `product/review.rs`; 테스트 `crates/foundry-line/tests/{b1_product_results.rs,b1_product_result_processes.rs,support/product_result_fixture.rs}`.

- 동결 scenario A/B 및 direct CommitPass를 그대로 scripted executor로 먼저 실행해 RED/원인을 기록한다.
- Resume의 세 generation, artifact14/session8, execute/restore/dispatch14/3/1과 accepted hold 원본 receipt/progress epoch/fence/reserved run 이력 결속을 검증한다. 이 숫자는 새 resource limit가 아니라 동결된 script의 기대 관측값이다.
- CommitPass의 origin run과 미사용 reservation을 분리한다. A/B14/3/1 및 direct4/0/1, successor/allocator 추가 증가 없음.
- 현재 run_commit 비교가 base/result/continuation 의미를 혼동하는지 추적해 올바른 일치 대상을 검증한다. 검사를 삭제해서 통과시키지 않는다.
- activation 당시 authority 캡처 및 Resume 복사, 철회→재발급/credential 교체/source 교체/worker 재배정, 완료 claim의 lease 만료, legacy missing vs new corruption을 독립 baseline으로 닫는다.

Exit: 해당 행렬 row 전부 부모 PASS. initial positive만으로 continuation을 대체하지 않는다.

## 5. Gate 2 — authenticated consumer·root·integrity 묶음

주요 경로: `crates/foundry/src/main.rs`, `crates/foundry-ledger/src/product/{result.rs,review.rs}`, `crates/foundry-serve/src/auth_http.rs` 중 Gate0에서 승인된 최소 범위.
테스트: `crates/foundry/tests/{b1_approve_cli.rs,b1_scope_cli.rs}`, `crates/foundry-serve/tests/{b1_product_artifacts.rs,b1_auth_http.rs}`.

- genuine G-final을 원본 DB/root 그대로 실제 CLI 및 실제 제품 서버 소켓에 연결한다. Router::oneshot은 별도 증거.
- 계약상 필요한 consumer 지점별로 정상 metadata/artifact/lookup/replay/approval→독립 파일·SHA·path·provenance·tuple·binding·host·acceptance·progress 변조→거부→baseline 복원 후 성공을 검증한다.
- root 미설정/다른 root/원래 경로 교체/leaf·ancestor 교체를 구분한다. root 경로를 DB 값으로 무조건 신뢰하거나 새 디렉터리로 복사해 맞추지 않는다. 필요하면 기존 configure/검증 로직을 최소 공유하고 error screening·fail closed를 유지한다.
- unauthorized+corrupt 조합은 정본 오류 우선순위와 정보 비노출 유지. 변조/잘못된 tuple/세션 철회/외부 세션 모두 소비·S1·sidebinding delta0.
- 과거 원본 source ReadArtifact와 새 source 권한 상속 금지를 분리. no-store/HTTP status/bytes/path 및 secret 비노출 검증.
- Publisher 호출0 및 publish-start0. S1 publishing 상태와 원격 게시를 혼동하지 않는다.

Exit: CLI와 socket consumer row 포함 해당 행렬 전부 PASS; genuine fixture positive와 제품 파일 무결성 보장을 별도로 기록.

## 6. Gate 3 — crash·ambiguous recovery·migration·isolation 묶음

테스트: `crates/foundry-line/tests/b1_product_result_processes.rs`, `crates/foundry-ledger/tests/{b1_product_results.rs,b1_scope.rs,b1_product_atomic.rs}`와 cfg(test) 단위 child.

- 기존 결과/승인 SIGKILL·경합 책임은 보존하고 변경 fixture로 재실행한다.
- 응답 손실, 실제 operation 위치의 COMMIT-I/O 실패, 복구 lookup-I/O 실패를 서로 다른 사례로 검증한다. SIGKILL 하나를 셋 모두의 증거로 세지 않는다. I/O error≠absence, 재실행 금지, receipt/불확실성 구분.
- faithful isolated v2의 DDL/population/CAS 실패 원자성, host/tuple census 봉인, cutoff 경계, 기존 history bytes·receipt 보존 및 missing binding corruption 구분.
- default-off P2의 타입/METHOD 부재 컴파일 반례와 enabled 성공, 정상 HTTP/CLI/startup에서 adapter 진입 불가. feature-enabled positive는 반드시 별도 실행.
- H/HX 옛 단언 mapping 전수 유지. 실패 시험 삭제/ignore/skip 또는 count 감소로 green 처리 금지.

Exit: 남은 행렬 전부 부모 PASS. 운영 DB migration은 실행하지 않는다.

## 7. 부모 검증 명령과 W3 인수

공통 `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0`, 매 lane free>=2GiB, `cargo test --locked --offline`, `-- --test-threads=1`. 디스크 가드 실패 시 중단, 무단 정리 없음.

- ledger: `-p foundry-ledger --test b1_product_results --test b1_scope --test b1_product_atomic`
- H/HX: `-p foundry-line --features p2-test-support --test b1_product_results --test b1_product_result_processes`
- HX 전제: 외부 Cargo `--message-format=json` 사전빌드 출력에서 정확한 ledger lib test executable을 추출해 `B1_LEDGER_TEST_EXE` 전달. glob/nested Cargo 금지.
- S: `-p foundry-serve --features p2-test-support --test b1_auth_http --test b1_product_artifacts` 및 대응 default lane.
- C: `-p foundry --features p2-test-support --test b1_approve_cli --test b1_scope_cli` 및 default CLI5개(service/approve/auth/watch/scope).
- core: `-p foundry-exec -p foundry-line -p foundry-ledger -p foundry-serve --lib --tests`
- watch: `-p foundry-watch --test b1_github_polling --test b1_intake_durability`
- 별도 compile negative/probe와 정확한 commands는 Gate0에서 기존 harness와 대조해 고정한다. 불명확한 API/명령을 추정해 구현 위임하지 않는다.

최종 W3 판정 자료: (a) 모든 유효 행렬 row PASS, (b) 실제 parent logs/exits/counts, (c) 기존 단언 보존 map, (d) default/enabled 격리, (e) W4 외부 효과0, (f) 미검증 범위(실제 Die/GitHub 등) 명시. 하나라도 미충족이면 인수하지 않는다. 제품 코드 변경은 별도 승인된 whitelist 안에서만. 프로젝트 전체 fmt/Clippy 부채는 숨기지 않고 종결 게이트에 인계하되 새 correctness/security 결함은 W3에서 해결한다.

## 8. W4 이후 순서와 승인 경계

1. 부모가 W3 인수 증거 묶음을 제시하고 사용자가 W3 수용 및 W4 범위를 승인한다. 자동 W4 착수 없음.
2. W4: 승인 결과/현재 publish 권한/대상 repo 확인→같은 transaction의 durable publish-start→commit 이후 외부 호출. crash/response loss/기존·닫힌 PR/재시도 복구는 우선 local mock으로 검증. 이미 수락된 외부 효과와 새 실행 권한을 구분.
3. W5: 감사 원천 연결, unavailable 원천 명시, export pagination/한계, 제품 서버 통합 인수.
4. W6–W7/B1 종결: 전체 workspace 회귀·명시 enabled positives·fmt·Clippy 및 warning inventory, sol 통합 리뷰·MUST 수정·부모 재검증. 실제 GitHub 검증은 별도 사용자 승인/자격증명 경계에서 수행; mock 성공으로 대체하지 않음. 운영 DB 변경도 별도 승인.
5. 모든 B1 인수 충족 후 최종 승인→B1 통합 커밋1회→SSH push1회. 중간 checkpoint commit/push 없음.
6. B2 실제 Die/다중 팩/컨테이너 및 B3 학습·평가·예산 연결은 B1 이후, 이번 계획의 실행 범위 아님.

## 9. 리스크·중단 조건

- frozen contract와 현재 구현이 충돌하면 validator 완화가 아니라 명세 대조 및 정직한 blocker 보고.
- fixture 분리는 bootstrap 문제가 해결됐다는 증거일 뿐 byte/root 무결성 보장이 아니다.
- 실제 loopback 성공도 외부 GitHub/실제 Die E2E가 아니다.
- source 상태가 예상 스냅샷과 다르면 중단/귀속 확인. 누적 fmt 변경은 B1 통합 리뷰 때 분리 판정.
- graphify가 사용 가능하면 승인된 코드 변경 후 `graphify update .`; 사용 불가이면 근거를 기록하며 graph 완료를 주장하지 않는다.
