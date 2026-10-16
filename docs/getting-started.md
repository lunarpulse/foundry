# Getting started

## Requirements and installation

The measured toolchain for this checkout is **rustc 1.97.0** and **Cargo 1.97.0**; the workspace uses Rust **edition 2024**. The repository does not declare an MSRV, so these measurements are not an MSRV guarantee. Build the CLI from the repository root:

```sh
# 전제: 저장소 루트, 위 실측 툴체인, Cargo 잠금 파일에 필요한 의존성이 로컬 캐시에 있음.
# 예상 종료 코드: 빌드 성공이면 0; 오프라인 의존성이 없거나 빌드 실패면 0이 아님.
cargo build --release --locked --offline -p foundry
```

The executable is `target/release/foundry`. To install it into Cargo's configured bin directory from this checkout:

```sh
# 전제: 저장소 루트, 오프라인 의존성 캐시가 완전하고 Cargo 설치 디렉터리에 쓰기 가능.
# 예상 종료 코드: 설치 성공이면 0; 누락된 오프라인 의존성 또는 쓰기 오류면 0이 아님.
cargo install --path crates/foundry --locked --offline --force
```

## Validate a Blueprint

Save this as `foundry.yaml` at the repository root. This minimal sample has been run with `foundry validate` by the offline documentation checker.

```yaml
schemaVersion: v1alpha1
name: local-example
description: Minimal offline Blueprint validation example
repositories:
  - owner: example
    name: sample
agentDefaults:
  profile: coding
lines:
  - name: default
    stages: []
    harness:
      type: fake
      profile: coding
    approval: human
inspector:
  scorer: default
```

```sh
# 전제: 위 내용을 foundry.yaml로 저장했고 target/release/foundry가 빌드됨.
# 예상 종료 코드: 예제가 유효하면 0; YAML 파싱/Blueprint 검증 실패면 1.
target/release/foundry validate foundry.yaml
```

Blueprint YAML은 `v1alpha1`을 지원한다. `name`은 공백만 있어도 거부하고 repository가 하나 이상 있어야 한다. 각 line에는 `name`, `harness`, `harness.type`이 파싱상 필수이고 `harness.type`은 공백만 있어도 거부된다. `approval` enum은 `human`만 받는다. 알 수 없는 필드를 거부하는 선언은 없다. 다음은 현재 파서의 기본값이며, 실행 설정에 적용된다는 뜻은 아니다.

| Structure / field | Omission behavior |
|---|---|
| `schemaVersion` | Required; only `v1alpha1` validates. |
| `name`, `description` | Default empty strings; empty/whitespace `name` then fails validation. |
| `repositories` | Defaults to `[]`, then fails the minimum-one validation. Each entry requires `owner` and `name`; their strings have no nonempty validation. |
| `agentDefaults` | Whole structure omitted: derived `Default` gives `profile: ""`. Present as `{}`: field serde default gives `profile: coding`. |
| `lines` | Defaults to `[]`; no minimum line count. Each line requires `name` (no nonempty validation) and `harness`. |
| line `intake` | Whole structure omitted: `cron` and `label` are `None`; either field is optional when present. |
| line `stages`, `approval` | Default `[]` and `human`. |
| line `harness` | Requires `type`; omitted `profile` defaults to `coding`. `type: fake` in the example is a nonempty string, not a validated harness enum. |
| `inspector` | Whole structure omitted: derived `Default` gives `scorer: ""`. Present as `{}`: field serde default gives `scorer: default`. |

`foundry.yaml`과 workflow preset JSON은 별개다. Preset은 JSON DSL로 schema version v1–v4를 다루며, v3에서 정책을 포함하고 v4에서는 review pack 참조가 필요하다. YAML Blueprint의 `v1alpha1`과 같은 스키마가 아니다.

## Fake and live paths

`run`의 기본 fake harness는 외부 프로세스를 실행하지 않는다. 그러나 `run` 자체는 `~/.foundry/ledger.db`와 mirror, 가능한 경우 tuple DB를 쓰고 `/tmp/foundry-die-<order>-<attempt>/`를 생성한다. `FOUNDRY_REVIEW_PACK`이 없고 기존 일치 review tuple도 없는 새 주문은 리뷰 실행기 부재로 inspection을 재시도한 뒤 `Failed`가 된다. 이 경우에도 `run`은 0을 반환하므로 종료 코드만으로 작업 성공을 판단하면 안 된다. 이 기본 경로는 소스상 네트워크와 publish 호출이 없지만, 이번 배치에서는 실행하지 않았다.

`--die`를 지정하면 CLI harness가 실행 파일을 호출한다(profile `coding`, timeout 900초). CLI `run`/`watch`의 팩 리뷰는 `UnavailableStageExecutor`를 사용한다. 실제 review executor를 주입하려면 `serve`에 허용 digest의 절대 경로 `FOUNDRY_REVIEW_PACK`과 `FOUNDRY_DIE_BINARY`/`FOUNDRY_REPO_PATH` 쌍이 필요하다. review dispatch에는 빌드 시 또는 런타임의 전체 `FOUNDRY_SOURCE_COMMIT`도 필요하다. 상세 전제와 인증 provisioning 제한은 [Usage](usage.md#review-holds)에 있다. 실제 Die는 코드·git·session 파일을 변경하고 자체 설정에 따라 외부 서비스에 접속할 수 있으며 이 경로는 미검증이다.

`run`은 지정한 Blueprint 파일을 읽을 수 있으면 파싱·검증하고 잘못된 경우 거부한다. 파일을 읽지 못하는 경우 현재 구현은 Blueprint 검증을 건너뛰고 계속 진행하므로, validate 성공을 run 성공이나 설정 적용의 증거로 보지 않는다.

`watch --once`는 한 번 실행하고 끝나는 모드이지 오프라인 모드가 아니다. 현재 intake는 인증 헤더 없이 공개 GitHub API를 호출한다. 공개 저장소 접근과 네트워크가 필요하며, 자격 증명 파일을 준비해도 이 intake에 private repository 인증이 연결되지는 않는다. 실행하면 ledger/mirror/tuple 및 writer lock 파일을 사용하고, 시작 시 기존 Die 작업 디렉터리 GC가 보관·삭제를 수행할 수 있다. 기본 `--notify-mode discord`는 notifier 파일이 설정되어 있으면 Discord 전송도 시도한다. 아래 예시는 콘솔 알림을 선택하며 이번 배치에서는 실행하지 않았다.

```sh
# 전제: 외부 GitHub 연결 및 공개 repo 조회 가능, 기본 ledger 부모 디렉터리 존재·쓰기 가능, 단일 writer, GC 대상 확인.
# 예상 종료 코드: 환경과 GitHub 응답에 따라 달라지며 이번 오프라인 배치에서는 미검증.
target/release/foundry watch --once --repo owner/repo --notify-mode console
```

## 검증되지 않은 실행 경로

Docker 배포 산출물 및 컨테이너 실행, 외부 GitHub 실연결(`watch --once` 포함), 실제 publish, `serve`에서 주입하는 실제 Die review 실행은 검증하지 않았다. 앞의 YAML validate와 빌드만 오프라인 경로다. Rust API 문서 생성(`cargo doc`)은 Markdown 문서 검증을 대신하지 않는다.
