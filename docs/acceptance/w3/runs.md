# 최종 소스 실행 기록

기준 커밋은 `a4e07fd56646503fd06ba5b846130a1d2189c9f0`, 제품 소스 패치 SHA-256은 `ea137f812ca47b3aa88815dae8904890de04c04b7e58519aad4a70de87458d32`이다. `cargo-run`은 매 실행에 전용 mode-0700 scratch TMPDIR를 만들고 `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=true`를 사용한다. 테스트·개발 debug info는 0이다. 전체 argv는 각 로그 첫 줄에 있다. 기존 파일의 변경 부분과 신규 Rust 파일을 포맷했고 Cargo 앞에서 포맷 및 diff 검사를 수행했다. [실행기](cargo-run) [포맷 및 diff](format-diff.log) [18개 파일 재현](reconstruction.log)

| 검사 | exit | pass | fail | ignored | measured | filtered | 원 로그 |
|---|---:|---:|---:|---:|---:|---:|---|
| W20/W21/W22 및 W30b canonical 입력 | 0 | 4 | 0 | 0 | 0 | 0 | [명령과 결과](candidate-rows.log) |
| 정상 v4 selected-review CLI | 0 | 1 | 0 | 0 | 0 | 0 | [명령과 결과](candidate-selected.log) |
| 실행 요청 및 proxy binding | 0 | 2 | 0 | 0 | 0 | 10 | [명령과 결과](candidate-unit.log) |
| 기본 feature binding 회귀 | 0 | 1 | 0 | 0 | 0 | 10 | [명령과 결과](candidate-default.log) |
| 이름 있는 Unix listener 및 테스트 upstream | 101 | 0 | 1 | 0 | 0 | 11 | [명령과 결과](candidate-wire.log) |

wire 행은 fixture upstream의 첫 bind에서 운영체제가 EPERM을 반환했다. Cargo가 보고한 실패 한 건을 그대로 유지한다. 표의 pass 수에는 동일 정책의 서로 다른 feature 회귀가 포함된다. W2 행 중 W22는 소유 fixture 자식의 별도 하위 실행을 포함한다. [바이너리 지문](binaries.json)

## Cargo 외 관측

| 실행 | 결과 | 증거 |
|---|---|---|
| `/usr/bin/python3 docs/acceptance/w3/verify_wiring.py <빌드된 foundry>` | exit 0. 실제 Git 객체와 strict worker 입력, 기록용 stage/Docker 제어, 정상 바이너리의 설정 거부 | [출력](components.log) [프로그램](verify_wiring.py) |
| `docker compose -p foundry-w3-render -f deploy/compose.yaml config --format json` | exit 0. 기본 command null과 메모리 단위를 정규화한 뒤 모든 필드 일치 | [정확 argv·피연산자](compose.log) [Compose JSON](compose-render.json) |
| 지정된 로컬 Docker daemon version 조회 | exit 1. permission denied. image 피연산자 미제공 | [관찰](local-runtime.log) |
| 기준 커밋에 소스 패치를 적용하고 바이트·실행 비트 대조 | exit 0. 기존 8개와 신규 10개 | [출력](reconstruction.log) [프로그램](verify_candidate.py) |
| Graphify AST update | 로컬 추출. 기존 graph 부재로 최초 재구축한 후 후보를 다시 갱신 | [최초](graph-initial.log) [후보](graph-update.log) [보고서](GRAPH_REPORT.md) |

초기 개발 회차의 로그는 `development/`에 보관했다. 최종 결과는 위 표의 `candidate-*.log`에서 확인할 수 있다. BMad renderer가 없는 체크아웃에서 사용자가 지정한 동결·구현·검증 절차를 직접 수행했다.
