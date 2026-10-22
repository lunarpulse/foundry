# Architecture

Foundry's workspace separates order state, workflow execution, human approval, review, and publishing into Rust crates. The table describes current code ownership, not six separately deployed services.

## Workspace crates (16)

| Crate | Responsibility |
|---|---|
| `foundry-domain` | Order and casting state types, events, and domain ports. |
| `foundry-blueprint` | Parse and validate `foundry.yaml` (`v1alpha1`). |
| `foundry-ledger` | SQLite order ledger, hash-chain verification, replay, and mirror support. |
| `foundry-workflow` | Workflow DSL, versioned interpretation, routing, and recovery engine. |
| `foundry-approval` | Approval grant reservation, commit, fencing, and recovery. |
| `foundry-orch` | Orchestration log, streams, handoff, and chainer. |
| `foundry-pack` | Review-pack loading, validation, graph policy execution. |
| `foundry-exec` | Durable execution, review holds, budgets, emit receipts, and tuples. |
| `foundry-line` | Drives an Order and connects review host and Die adapters. |
| `foundry-harness-cli` | External Die process invocation, manifest, and output protocol. |
| `foundry-inspector` | Placeholder crate (`fn nothing()`); current basic inspection taxonomy is in `foundry-domain::inspector`, and pack review is wired through `foundry-line`. |
| `foundry-furnace` | Placeholder crate (`fn nothing()`); Furnace is the conceptual human decision boundary, currently enforced by domain/line and approval code. |
| `foundry-watch` | Intake, publishing, casting, learning, and governance components. |
| `foundry-serve` | HTTP routes, identity, scope, approval, policy, and review-hold operations. |
| `foundry-gate` | Integrated validation gate. |
| `foundry` | CLI, deployment profile output, and example binary. |

## Call path

```text
foundry CLI
  ├── validate / optional run validation ──> foundry-blueprint
  ├── build_line (run/approve/watch) ──> foundry-line
  │     ├── domain ports + foundry-ledger
  │     ├── injected FakeHarness / CliHarness (foundry-harness-cli)
  │     └── review host ──> foundry-pack + foundry-exec
  ├── inject publisher / call intake ──> foundry-watch
  └── serve ──> foundry-serve
        ├── own Line construction (NoopHarness / NoopPublisher)
        └── identity + scope + foundry-watch policy/governance

Library stream runtime (not constructed by the CLI paths above):
foundry-orch ──> foundry-workflow
             └──> foundry-exec review port
```

The diagram distinguishes construction sites and injected adapters. Blueprint parsing only validates YAML; it does not construct or configure `Line`. `foundry-line` invokes a generic publisher port; it does not depend on `foundry-watch`. Its manifest lists `foundry-furnace`, but no runtime call is wired to that placeholder crate. `foundry-workflow` does not depend on `foundry-pack` or `foundry-exec`; `foundry-orch` wires the review port separately. CLI `run`/`watch` use `UnavailableStageExecutor` for pack review. With `FOUNDRY_REVIEW_PACK` configured, `serve` injects `DieStageExecutor` only when both `FOUNDRY_DIE_BINARY` and `FOUNDRY_REPO_PATH` are set. The serve order line uses a no-op harness and publisher for approval actions; hold resume can execute review stages in the serve process.

## Logical storage ownership S1–S6

| Boundary | Current implementation and owner |
|---|---|
| **S1** | Order journal in SQLite. `foundry-ledger` owns order transition records and ledger path access. |
| **S2** | SQLite `orch` tables. `foundry-orch` owns orchestration validation and append path. |
| **S3** | `<stream_id>.db` derived from stream identity and connected to that stream's WorkflowEngine. |
| **S4** | Review-host DB for execution, receipts, and hold state, plus separate session files; owned by host/Die adapter. |
| **S5** | Git branch/remote push operated by publish path. |
| **S6** | `artifact_tuples` and emit/promotion receipt binding; downstream reads `latest_promoted`. |

These are six logical ownership boundaries, not six independent services and not one transaction spanning all storage. Identity, scope, and governance databases are auxiliary stores: serve derives `.identity.db` and `.scope.db` from the ledger path, while governance uses `FOUNDRY_GOVERNANCE_DB` or `$HOME/.foundry/governance.db`. These do not join an S1–S6 transaction.

S6 promotion authorizes an emitted tuple for downstream consumption. S5 publish changes the git publication path. Promotion is not a remote publish and does not imply that a pull request exists.

## Organization and trust boundaries

The current serve scope configuration is a single local organization (`org_id: local`) with the hardcoded bound source `github:lunarpulse/foundary-demo`. This is a single-organization model, with incomplete handler coverage rather than proven tenant isolation:

| Surface | Current checks / gaps |
|---|---|
| `GET /api/orders`, `GET /api/audit/verify` | Resolve bearer session and call `guard_read` without an order ID; the order list is not filtered against each `order_scope` row. |
| `GET /api/orders/{id}` | Resolve bearer session and call `guard_read` for that ID; known foreign rows are refused, unregistered IDs fall through to ledger lookup. |
| `POST /api/orders/{id}/approve` | Bearer session and `Approve` capability; no scope guard call. Token consumption and line transition are separate operations. |
| Four review-hold POST routes | Bearer session and `ResolveReviewHold` capability; no scope guard call. |
| `POST /api/policy/apply` | Bearer session, current Owner role, policy binding/evidence checks; no order scope guard call. |
| `GET /`, `GET /api/status` | No bearer or scope check in these handlers. Status includes ledger verification metadata and credential presence/permission results. |
| CLI `run`/`approve`/`watch` line | `LocalPolicy` always allows; no HTTP identity/session or scope enforcement. |

The table above describes only the legacy StandaloneV1 router. Explicit `serve --product` refuses an unactivated scope policy, then mounts ProductStore login/authority, source and grant management, scoped order list/detail, approval, export/audit, and product hold query/accept/progress routes. Order data is filtered in ProductStore transactions by current role ceiling and explicit per-source grants; response DTOs contain no global counts. The local `product-bootstrap --manifest`, `product-account`, `product-scope`, `product-migrate-scope`, `product-login`, and `product-review` commands provide the operator path. Product review decisions need the configured ProductStore-backed frozen-pack host; older `/api/orders/.../review/hold/...` endpoints continue to use legacy `ServeState.identity` and standalone review storage. Product policy and product publish surfaces remain unavailable. See [Usage](usage.md#review-holds) and the [B1 contract](design/b1-product-contract.md).

The human decision boundary is Furnace. CLI approval is journaled and then proceeds to the configured publish call. `serve` exposes order approval, policy application, and review-hold resolution routes, so the HTTP server is not read-only. Bind defaults to loopback (`127.0.0.1:7700`) and can be changed; checks have the route-specific limits above. HTTP order approval calls `line.approve()` only and does not call `line.publish()`.

Execution is a separate boundary: empty `--die` selects the fake harness for CLI paths; an explicit CLI Die path invokes the external process harness. Pack review in CLI `run`/`watch` lacks the real stage executor. A real review executor can be injected through `serve` using `FOUNDRY_DIE_BINARY`, `FOUNDRY_REPO_PATH`, optional `FOUNDRY_DIE_PROFILE` (default `coding`) and `FOUNDRY_DIE_TIMEOUT` (default 600 seconds), and optional `FOUNDRY_SESSION_ROOT`.

Publishing is another boundary. CLI `approve` performs approval and, after it succeeds, calls `line.publish()`. Publisher owner/repository/base are fixed in `build_line()` and are not shared with `watch --repo`. S6 promotion remains local tuple evidence authorization.

## Required future work

The accepted target is concurrent pack selection per order/stream, including digest verification, pin/resume binding, and session isolation. Single-organization scope does not remove this requirement. The engine has tests using different packs, but the current product `PackReviewHost` accepts one hardcoded digest; `FOUNDRY_REVIEW_PACK` is not arbitrary multi-pack support.

Worker separation is the target topology. The agreed incremental start keeps Die execution inside `serve` while establishing the deployment path; moving execution to a worker is a later independent batch. Neither this separation nor Docker deployment has been established by this documentation review. These are future requirements from the accepted M9 decisions, not implemented features.

## Configuration formats

`foundry.yaml` is a YAML Blueprint with `schemaVersion: v1alpha1`. Workflow presets are separate JSON documents with versioned v1–v4 interpretation; their schemas and validation rules do not transfer to Blueprint YAML. The preset DSL explicitly rejects unknown fields; Blueprint structs do not declare that rule. See [Usage](usage.md#configuration-and-environment) for runtime settings.

## 검증되지 않은 실행 경로

이번 배치에서는 Docker 배포와 컨테이너 동작, 외부 GitHub 실연결·publish, 온라인 `watch --once`, 실제 Die review executor를 통한 `serve` 경로를 검증하지 않았다. 문서의 경계 설명은 소스 코드 확인에 기반하며, 이들의 실행 및 외부 서비스 상호작용은 미검증 조건으로 남는다. `cargo doc`의 Rust API 산출물은 Markdown 링크·앵커·예제를 검증하지 않는다.
