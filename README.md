# Foundry

Foundry is a personal software factory prototype. It models work as an **Order** moving through a line of execution, with a **Die** doing work, a **Furnace** representing the human approval boundary, and an **Inspector** describing inspection configuration. These terms name distinct parts of the code and workflow; they do not imply that every path is wired to a live service.

## What runs today

The Rust workspace builds a `foundry` CLI. Blueprint validation is the minimal offline entry point. Source tracing shows that a fresh default fake-harness `run`, with `FOUNDRY_REVIEW_PACK` unset, makes no network request and never invokes its configured GitHub publisher. It does create local work directories and write ledger/mirror and, when available, tuple storage. Without existing matching review evidence, the missing review runner causes inspection retries and a terminal `Failed` order; the CLI still returns 0. This is not a successful end-to-end work example, and was not executed in this documentation batch.

Live intake in `watch` calls GitHub even with `--once`; publishing uses a GitHub publisher configured in the binary. CLI `run` and `watch` do not have a real pack-stage Die review executor. The HTTP `serve` path can receive a real review executor through environment configuration, and exposes state-changing approval, policy, and review-hold operations. Authentication provisioning and scope coverage remain incomplete; see [trust boundaries](docs/architecture.md#organization-and-trust-boundaries) before using these APIs.

The smallest offline check is to validate a local `foundry.yaml`:

```sh
# 전제: Foundry 저장소 루트, 빌드된 target/release/foundry, 현재 디렉터리에 foundry.yaml.
# 예상 종료 코드: 유효한 Blueprint면 0; 파일 또는 검증 오류면 1.
target/release/foundry validate
```

See [Getting started](docs/getting-started.md) for the validated example and setup details, [Usage](docs/usage.md) for the commands and settings, and [Architecture](docs/architecture.md) for the component and storage boundaries.

## Historical design rationale

These design documents record historical design rationale. Treat their plans and intended contracts as context; current behavior in this README and the user guides is based on source inspection.

- [Integrated demo script](docs/demo/integrated-demo-script.md)
- [Independent review engine design](docs/design/independent-review-engine-design.md)
- [Notifier v2](docs/design/notifier-v2.md)
- [Publish bridge](docs/design/publish-bridge.md)
- [T30 durable execution](docs/design/t30-durable-execution.md)
- [T33 Die end-to-end](docs/design/t33-die-e2e.md)
- [Universal skill specification](docs/design/universal-skill-spec.md)

## 검증되지 않은 실행 경로

이번 오프라인 배치에서는 Docker 배포 예제, 외부 GitHub 실연결 및 실제 publish, `watch --once` 온라인 intake, `serve`에서 주입하는 실제 Die 리뷰 실행을 검증하지 않았다. 이 항목들은 문서에서 미검증 조건으로 다룬다. `cargo doc`는 Rust API 문서이며 이 Markdown 파일의 링크·앵커·예제 검증을 대신하지 않는다.
