# Usage

Run `foundry help` or `foundry <command> --help` for the built CLI's live help. **Required** means no default; values shown as defaults are taken from the command definition. `run`, `approve`, `watch`, and `serve` can write state or contact external systems; examples below identify their preconditions and expected result.

## Command reference

| Command | Arguments, defaults, and required values |
|---|---|
| `deploy-profile` | `--check`, `--units`, `--readme` (all flags, default off). |
| `validate` | `[path]` (optional; default `foundry.yaml`). |
| `run` | `--id <ID>` (optional, generated if absent); `--task <TASK>` (**required**); `--die <DIE>` (default empty, fake harness); `--blueprint <BLUEPRINT>` (default `foundry.yaml`). |
| `approve` | `--order <ORDER>` and `--intent <INTENT>` (**required**); `--db <DB>` (default `~/.foundry/ledger.db`). This remains a legacy local-actor path and is not wired to ProductStore sessions; do not treat it as the B1 product approval flow. It records approval and then invokes publish. |
| `watch` | `--once` (false); `--every <SECONDS>` (300); `--repo <OWNER/NAME>` (`lunarpulse/contextmesh-rs`); `--die <DIE>` (empty=fake); deprecated `--auto-approve <INTENT>` (any non-empty value is rejected); `--notify-mode <MODE>` (`discord`). `--once` still calls GitHub. |
| `show` | `--order <ORDER>` (**required**); `--db <DB>` (`~/.foundry/ledger.db`). Despite the help text, this currently checks ledger chain integrity and does not show order details. |
| `status` | `--db <DB>` (`~/.foundry/ledger.db`). |
| `doctor` | `--db <DB>` (`~/.foundry/ledger.db`). |
| `casting` | `--run-profile <VALUE>`, `--judge <VALUE>`, `--promote <VALUE>` (optional); `--corpus <PATH>` (`~/.foundry/corpus`); `--casting-dir <PATH>` (`~/.foundry/casting`); `--seed <N>` (`42`). Judge/promote are currently failure-returning CLI placeholders. |
| `benchmark` | `--baseline <VALUE>`, `--compare <VALUE>` (optional); `--corpus <PATH>` (`~/.foundry/corpus`). |
| `corpus` | `--db <DB>` (`~/.foundry/ledger.db`); `--out <PATH>` (`~/.foundry/corpus`). |
| `reconcile` | `--db <DB>` (`~/.foundry/ledger.db`); `--apply` (false; requests mirror catch-up, attempts writer lock acquisition). |
| `serve` | `--db <DB>` (`~/.foundry/ledger.db`); `--bind <ADDRESS>` (`127.0.0.1:7700`). It serves mutation endpoints as well as reads. |
| `product-bootstrap` | `--db`, `--installation-id`, `--database-id`; optional `--manifest <JSON>`. Reads actor ID then password from stdin. No manifest means zero sources/grants and enforced deny-by-default scope. |
| `product-login` | `--db`, installation/database IDs, `--session-file`. Reads actor ID and password from stdin; writes a mode-0600 bearer file. |
| `product-account` | Database IDs, `--session-file`, `--actor`, `--role`. Reads the new account password from bounded stdin; current Owner required. |
| `product-scope` | `--action sources|configure-source|grants|grant|revoke`, database IDs, `--session-file`; mutations require expected revision and request key. |
| `product-migrate-scope` | Database IDs and reviewed finite `--manifest`; local maintenance after old processes stop. |
| `product-reset`, `product-grant`, `product-approve`, `product-review`, `product-watch` | ProductAtomicV1 local credential, approval, review, and intake operations described below. |
| `help` | Clap help command; accepts command-path arguments, e.g. `foundry help watch`. |

### Example commands and effects

| Example | Preconditions, effects, and expected exit (source inspection unless stated) |
|---|---|
| `foundry validate /tmp/example.yaml` | Local valid YAML and built binary; 0 if valid, 1 on parse/validation or file error. |
| `foundry deploy-profile --units` | Prints systemd units; 0. Review hardcoded user/paths before use; output was not installed. `--check` checks declared boundaries and credential permission metadata (0/1); no action flag returns 2. Priority when combined: check, units, readme. |
| `foundry run --task dep:serde` | Writes default ledger/mirror, possible tuple DB and `/tmp/foundry-die-<order>-<attempt>/`. With pack unset and no matching review tuple, fake work reaches terminal Failed for missing review runner, yet exits 0. Port/setup/validation errors can return 1 or panic. No publish call here. Not run. |
| `foundry approve --order ID --intent "reviewed" --db /tmp/ledger.db` | Legacy path only: fixed local human actor, no login, and not ProductStore-backed. Not accepted as the B1 product approval path. |
| `foundry watch --once --repo owner/name --notify-mode console` | Public GitHub API reachable; existing writable ledger parent, isolated tuple paths and single writer. Writes ledger/mirror/lock; startup GC may archive/delete existing Die workdirs. Default Discord mode may send notifications. A non-empty `--auto-approve` is rejected. Tick Ok returns 0 even if individual orders fail; intake error returns 1. Not run. |
| `foundry product-watch --once --db <product.db> --installation-id <id> --database-id <id> --repo owner/name --repository-id <stable-id>` | ProductAtomicV1 database with an enabled `github-primary` source binding matching API origin and stable repository ID. Fetches at most five 100-item pages, persists snapshots/cursor, and locally acknowledges only after the matching admission receipt is durable; intake sends GET requests only and never removes labels. `--private --credential-ref ENV_NAME` resolves a token from that environment variable at runtime. Unsupported work remains visibly waiting; this command does not run arbitrary issue bodies. Production configuration requires HTTPS; loopback HTTP is an explicit fake-server test setting and is not exposed by this command. |
| `foundry show --order ID --db /tmp/ledger.db` | Opens SQLite read/write and may initialize an absent ledger; verifies the entire chain, does not check whether ID exists. 0 for valid chain, 1 otherwise. Not run. |
| `foundry status --db /tmp/ledger.db` | Reports height/mirror/state counts and disk usage. Uses `SqliteLedger::open`, which can create/initialize the DB despite the read-only help label. 0 after reporting, 1 if open fails; not a full integrity gate. Not run. |
| `foundry doctor --db /tmp/ledger.db` | Opens/initializes ledger; reads chain/mirror, credential file metadata (not values), disk use. 0 checks pass, 1 unhealthy, 2 cannot open ledger. Not run. |
| `foundry casting --run-profile PROFILE --casting-dir /tmp/casting --corpus /tmp/corpus` | Writable existing casting directory, corpus base SHA matching current git HEAD, and git on PATH; writes casting DB, sandbox/candidate artifacts and deterministic mock DryDie results (no real LLM evaluation). `bundle-<digest>` needs a verified stored bundle; legacy profile runs are non-promotable. 0 if run succeeds, 1 on handled error. `--judge` and `--promote` currently print guidance and return 1, even with an ID. Run-profile takes precedence over judge, then promote. Not run. |
| `foundry benchmark --baseline /tmp/baseline.json --corpus /tmp/corpus` | Readable corpus, writable output parent; writes snapshot. Compare reads a baseline and rejects schema/metric mismatch. 0 on success, 1 on load/write/compare failure or neither action; baseline takes precedence. Not run. |
| `foundry corpus --db /tmp/ledger.db --out /tmp/corpus` | Ledger open/initialization and writable output; emits terminal-order manifests. 1 on ledger/replay error; individual manifest write errors are logged but final exit can still be 0. Not run. |
| `foundry reconcile --db /tmp/ledger.db` | Ledger open/initialization; default dry run reports mirror repair without rebuilding it. 0 for report or no work, 1 on open/repair/lock error. Apply writes mirror and lock file; see recovery limits below. Not run. |
| `foundry serve --db /tmp/ledger.db` | Ledger and governance parents exist and are writable; bind available. Opens/initializes ledger, identity, scope, governance and configured review stores. Runs until stopped; setup/bind/serve failure returns 1. APIs mutate approval/policy/holds and configured resume can invoke Die. Not run. |
| `foundry help watch` | Built CLI; prints help, 0. All 13 product `--help` paths and `foundry help` are executed by the checker. Clap argument errors normally return 2. |

Use the local, offline validation example in [Getting started](getting-started.md#validate-a-blueprint) as the only end-to-end CLI example executed for this document batch. Do not infer that the other commands are offline-safe from the absence of a visible network flag.

The checked release build, help paths and extracted YAML validation are the observed results. Installation and operational examples above are source-based guidance, not successful executions in this batch. Legacy `watch` does not send a GitHub token; ProductAtomicV1 `product-watch` can resolve a private connector token from a named environment variable. `watch --auto-approve` is disabled and returns an error; it cannot synthesize a human approval.

## Configuration and environment

### Product settings (13)

| Name | Meaning and default/source |
|---|---|
| `FOUNDRY_REVIEW_PACK` | No default; absolute pack path enables review wiring, but product host accepts only the hardcoded digest. CLI executor remains unavailable. |
| `FOUNDRY_TUPLES` | Tuple DB path; default `~/.foundry/tuples.db`. |
| `FOUNDRY_SOURCE_COMMIT` | Full 40- or 64-character hex source commit required for review dispatch; build-time value preferred, runtime fallback. |
| `FOUNDRY_DIE_BINARY` | No default; executable for real review in `serve`, only inside review-pack wiring and paired with `FOUNDRY_REPO_PATH`. |
| `FOUNDRY_DIE_PROFILE` | Die profile; default `coding`. |
| `FOUNDRY_DIE_TIMEOUT` | Timeout in seconds; default `600`, also used if parsing fails. |
| `FOUNDRY_REPO_PATH` | Local repository given to Die; required with `FOUNDRY_DIE_BINARY` to inject the real executor. |
| `FOUNDRY_SESSION_ROOT` | Session directory; default `sessions/` beside the review host DB. |
| `FOUNDRY_LEDGER` | Evidence reader ledger path; default `~/.foundry/ledger.db`. Not a universal CLI DB override. |
| `FOUNDRY_GOVERNANCE_DB` | Governance journal path; default `$HOME/.foundry/governance.db` (`./.foundry/governance.db` if HOME absent). |
| `FOUNDRY_CORPUS_DIGEST` | Corpus digest override for evidence reader. |
| `FOUNDRY_GIT_SHA` | Source SHA metadata when emitting corpus. |
| `FOUNDRY_GC_INTERVAL_MS` | Failed-work retention grace calculation interval; default `60000` ms. |

`FOUNDRY_DIE_*` comprises exactly `BINARY`, `PROFILE`, and `TIMEOUT`. `REPO_PATH` and `SESSION_ROOT` are separate settings. The notifier reads its Discord webhook key from `~/.foundry/notifier.env`; `FOUNDRY_DISCORD_WEBHOOK` is not read directly from process environment and is not a product environment variable.

All 13 rows are settings read by product source, not a claim that all are required. The optional digest/SHA overrides have no implicit credential values. `FOUNDRY_DIE_PROFILE`, `FOUNDRY_DIE_TIMEOUT`, and `FOUNDRY_SESSION_ROOT` affect the serve review executor only when pack and binary/repository wiring is active. Paths are not universally shell-expanded: governance DB and session/repository/binary overrides are used as supplied; use absolute paths. `FOUNDRY_LEDGER` should agree with `serve --db` when policy evidence must come from that ledger, because the paths are selected independently.

### Other environment channels

| Category | Names / behavior |
|---|---|
| Child Die injection | `FOUNDRY_ORDER_ID`, `FOUNDRY_ATTEMPT`. |
| Cast container spec forwarding allowlist | `FOUNDRY_ORDER_ID`, `FOUNDRY_TASK`, `FOUNDRY_ARTIFACT_DIGEST`, `FOUNDRY_PROXY_ADDR`; this specification is not evidence of a deployed container/transport. |
| Execution basis | `HOME`, `PATH`; the Die review adapter's git subprocesses set `GIT_NO_REPLACE_OBJECTS=1`, not every git call in the product. |
| Dynamic | A `CandidateProfile.env_allowlist` can select additional environment names. |
| Test-only | `FOUNDRY_BASE`, `FOUNDRY_CONTEXT`, `FOUNDRY_FIX_CONTEXT`, `FOUNDRY_EXEC_CRASH_DIR`, `FOUNDRY_EXEC_CRASH_STAGE`, `FOUNDRY_EXEC_TEST_INVENTORY`, `FOUNDRY_REVIEW_DIR`, `FOUNDRY_REVIEW_EXPECTED`, `FOUNDRY_REVIEW_MODE`, `FOUNDRY_T31_SQLITE_CHILD_DB`, `FOUNDRY_T31_SQLITE_CHILD_VALUE`, `FOUNDRY_T61_KILL9_CHILD`, `FOUNDRY_T61_KILL9_DB`, `FOUNDRY_TEST_SECRET`, `FOUNDRY_TEST_WORKDIR_ROOT`. These are test instrumentation, not operator settings. |

### Data paths

| Data | Default / selection |
|---|---|
| Order ledger | CLI `--db` where supported, otherwise `~/.foundry/ledger.db`; `watch` uses this default. `FOUNDRY_LEDGER` is specifically consumed by the evidence reader, not all CLI ledger opens. |
| Artifact tuples | `FOUNDRY_TUPLES`, otherwise `~/.foundry/tuples.db`. |
| Governance journal | `FOUNDRY_GOVERNANCE_DB`, otherwise `$HOME/.foundry/governance.db`. |
| Review sessions | `FOUNDRY_SESSION_ROOT`, otherwise `sessions/` beside review host DB. |
| Review host | `~/.foundry/review_host.db`; enabled by review-pack wiring. Default sessions are therefore `~/.foundry/sessions/`. |
| Corpus and casting output | CLI defaults `~/.foundry/corpus` and `~/.foundry/casting` as listed above. |
| Serve identity/scope | Separate files with `.identity.db` and `.scope.db` extensions derived from the selected ledger path. |

## Approval, publish, and promotion

### ProductAtomicV1 checkpoint interface

The explicit product server mode is `foundry serve --product --db <path>`. It requires `FOUNDRY_INSTALLATION_ID` and `FOUNDRY_DATABASE_ID`, refuses to open a standalone ledger file, and refuses ProductAtomicV1 databases whose explicit scope policy is not active. Initialize its first Owner locally with `foundry product-bootstrap --db <path> --installation-id <id> --database-id <id> [--manifest <path>]` and provide exactly two stdin lines: actor ID, then password. TTY input and extra lines are rejected so the password is not entered through argv or an echoed prompt. The optional strict JSON manifest is bounded to 64 KiB and lists finite source instances and grants. An empty manifest still activates enforced scope.

Product mode also registers source/grant administration and scoped order reads. `GET/POST /api/product/sources` queries/configures source instances. `GET/POST /api/product/scope/grants` queries/grants and `/api/product/scope/grants/revoke` revokes. Owner-only administration returns revisions and source IDs, not order payloads/counts. `GET /api/product/orders` returns up to 100 visible rows and accepts `after=<last-visible-order-id>` plus optional `limit=1..100`; it never returns a global total. `GET /api/product/orders/{id}` returns a sanitized scoped view and uses the same not-found response for foreign and unknown orders. Role ceiling and explicit source capability are both required. Owners have no implicit data access and cannot remotely expand their own grants. Product mode still has no policy or product publish routes; ordinary `serve` / `approve` remain on legacy StandaloneV1 integration.

The other product routes are `/api/product/login`, `/api/product/logout`, `/api/product/session`, `/api/product/accounts`, `/api/product/accounts/{actor_id}/authority`, `/api/product/approval-grants`, `/api/product/orders/{id}/approve`, `/api/product/review/holds`, per-order hold query/accept routes, `/api/product/review/operations/{operation_id}`, `/api/product/export`, and `/api/product/audit`. Account creation and authority changes require a current Owner. Login throttles after five failures in five minutes for five minutes. Keep bearer and grant tokens in memory; login/grant responses are `no-store`. Approval requires an `Idempotency-Key` header and current promoted tuple coordinates/digests. Review hold reads require ReadReview; decisions require ResolveReviewHold plus a current role ceiling. A committed decision returns its receipt and progress phase; `202 Accepted` means the decision is durable but continuation is incomplete or requires recovery. Export requires `export_evidence`; audit requires `read_operational_audit` and neither can be inferred from `read_order`. Both select current-authenticated evidence and provenance in one ProductStore transaction. They use fresh per-request HMAC pseudonyms and local ordinals; their digest covers the public projection and is not a proof of the raw S1 chain. `limit` defaults to 100 and accepts 1..100. A response with `coverage=page` and `has_more=true` is a truncated first page; there is no continuation cursor yet. Audit reports S1 coverage plus explicit unavailable reasons for unmounted sources, not complete cross-system evidence or organization-wide security audit authority.

`foundry product-login` takes bounded stdin lines for actor ID and password and writes a bearer to a new mode-0600 `--session-file`. `product-account` reads a new password from stdin under a current Owner session. `product-scope --action sources` lists source generations; `configure-source` takes connector, canonical repository identity, query, expected source revision and request key. `grants` queries one subject/source; `grant` and `revoke` take a capability, expected scope revision and request key. Capability values are `read_order`, `read_review`, `resolve_review_hold`, `approve_order`, `read_artifact`, `export_evidence`, `read_operational_audit`, and `publish_approved`. `foundry product-grant` reads a private session file and writes the one-time grant token to `--token-file`. `foundry product-approve` reads private session and grant files and requires source revision, attempt, run, generation, artifact/tuple digests, intent, and idempotency key. `foundry product-review --action holds` lists holds visible under current grants; progress and decision calls recheck current scope. `foundry product-reset` is local-only and invalidates existing sessions. No secret is accepted through argv, and product approval does not publish. See [the B1 product contract checkpoint](design/b1-product-contract.md) for remaining integrations.

Human approval is a Furnace decision recorded against an order. The CLI `approve` command then calls `line.publish()` after approval succeeds. Publish operates on a git branch/remote path and may contact GitHub; the configured publisher's owner/repository/base are fixed in `build_line()` and are not selected by `watch --repo`. Therefore `watch --repo` does not configure the later publish destination. There is no independent CLI `publish` command.

The fixed destination is `lunarpulse/foundary-demo`, base `main`. Publishing requires a nonempty token at `~/.foundry/gh_token` with repository Contents and Pull requests write access, plus `/tmp/foundry-die-<order>-<attempt>/repo` whose HEAD matches the approved digest. It checks out `foundry/<sanitized-order>`, pushes the branch, looks up an open PR and may create one. These are external and local git side effects; a failed later step does not undo an earlier push. HTTP approval uses a bearer session and `Approve` capability, consumes the token then calls `line.approve()` separately; it does not invoke publish. These operations were not executed here.

`run --blueprint` parses and rejects an invalid file if it can read it. A read error is currently ignored by `run`, which then continues; validation is a separate CLI command and does not prove that a Blueprint's values are applied to line construction.

S6 tuple promotion records that emitted evidence is authorized for downstream consumption. It does not mutate the S5 git ref or publish remotely. Do not use “promoted” and “published” interchangeably.

## Review holds

Legacy `/api/orders/.../review/hold/...` routes still require `Authorization: Bearer <session-token>` and `ResolveReviewHold` from the StandaloneV1 identity store. Product mode uses separate `/api/product/review/holds` and `/api/product/orders/{order}/review/holds/{hold}/accept` routes backed by ProductStore, with actor-bound progress at `/api/product/review/operations/{operation}`. The product UI holds its bearer in page memory only. Product route scope is the current enabled ProductStore source binding; legacy handlers retain their separate legacy scope limitations. See [Architecture](architecture.md#organization-and-trust-boundaries).

The order must be in `FurnaceHold` with a ProductStore-backed review run and exact admission/source binding. Configure `FOUNDRY_REVIEW_PACK` plus distinct `FOUNDRY_PRODUCT_ARTIFACT_ROOT`, `FOUNDRY_PRODUCT_SESSION_ROOT`, and `FOUNDRY_PRODUCT_CLAIM_ROOT`. Resume that executes stages also needs the binary/repository pair, source provenance, git and writable session/work directories. Product holds are discovered from the authenticated hold list or `foundry product-review --action holds`; clients must use the returned hold instance ID, allowed targets, and expectation rather than infer coordinates from the order ID. Legacy standalone hold integrations are unchanged and remain separate.

```sql
SELECT hold_instance_id, resume_targets, deadline, status
FROM review_runs WHERE order_id = 'ORDER_ID'
ORDER BY rowid DESC LIMIT 1;
```

Only a current `held` row is actionable. A nonzero deadline is Unix seconds; expired holds are refused. Replace `{id}` and `{hold_id}` below with URL-encoded actual identifiers. No request below was executed in this batch.

| Method and route | Body and behavior |
|---|---|
| `POST /api/orders/{id}/review/hold/{hold_id}/resume` | `Content-Type: application/json`, body `{"target":"ALLOWED_TARGET"}`. Target must exactly match an entry in stored `resume_targets`; continuation can run Die stages and create a new hold. |
| `POST /api/orders/{id}/review/hold/{hold_id}/commit-pass` | No body required. Records passing review resolution and re-enters the durable review path; resulting evidence must still pass tuple binding checks. Does not publish. |
| `POST /api/orders/{id}/review/hold/{hold_id}/commit-fail` | No body required. Records a failing review result and re-enters review processing; does not promise an HTTP success or a terminal order transition. |
| `POST /api/orders/{id}/review/hold/{hold_id}/abort` | No body required. Records `review_aborted` and a failing result; does not cancel the order or roll back its git work. |

Success is HTTP 200 JSON `{"order_id":"ID","state":"STATE"}`; inspect state rather than assuming approval or shipment. Missing/expired/revoked session or disabled account returns 401; insufficient capability returns 403. Restore or resolution errors currently return 500 with sanitized `error`, including unknown order/hold, stale or consumed hold ID, deadline expiry, disallowed target, unavailable executor, and storage/evidence errors. Resume JSON extraction can reject missing content type or invalid/missing target before these handler checks.

Resolution checks the latest held instance within a SQLite transaction; it is separate from the subsequent order/evidence update. In particular, commit-fail/abort may already be persisted before the re-entry returns a nonpassing-result error (HTTP 500). Preserve the response and inspect durable state before retrying; retries of a consumed instance are not guaranteed idempotent. The public routes do not expose the separate library `HoldOperation` API. CLI `run`/`watch` have no corresponding hold-resolution options.

## Replay, recovery, and troubleshooting

### Product service credentials

After ProductAtomicV1 scope setup, an authenticated Owner can provision a source-bound watch/review service. Use finite RFC3339 expiry values and protect the generated credential file:

```sh
foundry product-service --action configure \
  --db ~/.foundry/ledger.db --installation-id INSTALL --database-id DATABASE \
  --session-file ~/.foundry/owner.session --service-id github-review \
  --expected-revision 0 --request-key service-config-1 \
  --lease-until 2026-10-19T00:00:00Z \
  --source-instance github-primary-generation-1 \
  --actions intake_read,admit_delivery,ack_delivery,drive_review

foundry product-service --action issue \
  --db ~/.foundry/ledger.db --installation-id INSTALL --database-id DATABASE \
  --session-file ~/.foundry/owner.session --service-id github-review \
  --expected-revision 1 --request-key service-credential-1 \
  --expires-at 2026-10-18T12:00:00Z \
  --credential-file ~/.foundry/github-review.credential

foundry product-watch --once --db ~/.foundry/ledger.db \
  --installation-id INSTALL --database-id DATABASE --repo acme/project \
  --repository-id 42 --service-file ~/.foundry/github-review.credential
```

The examples use placeholder IDs and dates; choose future finite timestamps for the configured service lease and credential expiry. Credential issuance is one time: retries with the same request key cannot return the plaintext. If private-file creation fails, rotate with a new request key and path. `product-service --action revoke` invalidates current service contexts. The GitHub API token remains separate and is resolved from the configured environment-variable reference for private repositories.

- `show --order ID` verifies the ledger chain; it is not an order detail view. If verification returns broken or the database cannot open, investigate the selected ledger path and preserve it for diagnosis.
- `doctor --db PATH` runs chain, mirror, credential metadata, and disk checks. `status` reports operational status. `reconcile` defaults to dry run; apply writes the mirror. Stop other writers first: watch may continue if its lock file cannot open, run/approve/serve do not use this OS lock, and reconcile's lock handle is dropped before mirror rebuild. The current implementation does not provide a blanket cross-process repair lock.
- Ledger replay reconstructs order state from journal rows. Ordinary approval rehydrate verifies the chain, requires a recoverable artifact digest (workdir HEAD can supply it), and checks matching promoted tuples for actionable states; missing tuple/workdir evidence is not bypassed. Review-hold restoration instead reads the verified ledger plus host latest-run data, since an unresolved hold has no promoted tuple yet. Preserve ledger, tuple/host DBs, workdir and sessions together; replay is not an automatic retry/publish facility.
- If a command reports an unknown order, verify the selected ledger and journal. For a missing CLI review executor, merely setting a Die binary cannot enable CLI pack review. Serve injection permits review continuation only with an existing recoverable hold and the integration prerequisites above; it is not a shipped intake/re-execution replacement. Missing evidence requires investigation rather than blind approval.
- `casting --help` mentions `--apply` in descriptive text, but the casting parser has no such option. `--apply` belongs to `reconcile`.

## 검증되지 않은 실행 경로

이번 배치에서는 Docker 배포·컨테이너 경로, 외부 GitHub 실연결 및 실제 publish, `watch --once`의 온라인 intake, `serve`를 통한 실제 Die review 실행을 실행하지 않았다. 승인 및 serve의 변경 API도 실사용자 데이터에 실행하지 않았다. `cargo doc`는 Rust API 문서만 다루므로 이 Markdown의 링크·앵커·코드 예제 검증을 대신하지 않는다.
