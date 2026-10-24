# B1 product contract: implementation checkpoint

This document records the current ProductAtomicV1 implementation boundary. P2 has real registered auth/approval routes and local CLI commands, while the rest of B1 remains incomplete as listed below.

## Implemented lower-level contract

`foundry-ledger::ProductStore` owns a private SQLite connection and typed operations. `ProductStore::open` binds a file to `ProductAtomicV1`, an installation ID, and a database ID. It refuses a standalone ledger file or conflicting identity. Product identity, configured sources, order admission, operation reservations, grant/token state, transition receipts, and approval outcomes share that SQLite file. Product writes use `BEGIN IMMEDIATE`; raw appends to a product ledger are blocked by a transition-intent trigger. Standalone `SqliteLedger` behavior remains separate.

The current public approval API is:

- `ProductStore::bootstrap_owner(actor_id, argon2id_phc)` for local initial setup.
- `ProductStore::login(actor_id, password, now)` and `resolve_session` for opaque bearer sessions and current authority lookup.
- `ProductStore::bind_source` / `configure_source` and `admit_order` for scoped, durable admission.
- `ProductStore::issue_approval_grant`, `prepare_approval`, and `approve_product` for a typed reservation proof and final atomic approval.
- `ProductStore::authorize_product_publish` for a fresh exact-outcome, current-S1, source, and promoted-tuple check that yields a private publish authorization.
- `ProductStore::scoped_evidence_snapshot(session, capability, limit, now)` for bounded export/audit reads. It accepts only `ExportEvidence` or `ReadOperationalAudit` and checks current principal, capability, verified S1, transition receipts, lifecycle provenance, and selected rows in one read transaction.
- `ProductStore::fence_prepared_approvals` for explicit recovery fencing. It does not approve work.

`approve_product` verifies the current product session, source revision, verified S1 lifecycle, promoted tuple, operation binding, reservation owner/epoch/fence and expiry in the transaction that writes the Approved event, transition receipt, grant/token consumption, and stored outcome. A committed retry can reacquire its proof using the same currently authenticated actor and idempotency binding. Approval does not publish.

The registered approval route commits through `ProductStore` before applying the returned receipt to the `ProductProjection`. Projection and ordered mirror catch-up read a verified S1 snapshot; a projection failure is not treated as an approval failure and does not rerun approval. `authorize_product_publish` rejects a forged or stale receipt, changed current lifecycle/source binding, or changed promoted artifact tuple. `ProductProjection::publish_approved` composes that gate with the existing injected `Publisher` port as a separate network effect. It returns the port's `Confirmed`/`Unknown` result without claiming SQLite/network atomicity; it is not yet wired to an HTTP or CLI command and does not persist a product publish outcome.

The product schema uses the `foundry_contract` discriminator and additive `product_*_v1` tables, alongside the existing S1 `ledger`. Existing standalone databases are not migrated into product identity or source history. Current defaults in `product_identity` are Argon2id v19 (`m=65536 KiB,t=3,p=1`) and an eight-hour session. These are initial resource/security settings, not measured optimal values.

Scoped export and operational audit are registered ProductAtomicV1 endpoints.
They return a distinct projection with local contiguous ordinals and a digest
over the exact public projection bytes. HMAC aliases use a fresh random key
and context per request. Responses contain no raw S1 sequence/hash/head, stable
installation/source identity, or foreign-row count. `limit` defaults to 100
and accepts 1..100, following the existing order selection budget.
`coverage=complete` means all visible orders fit; `coverage=page` with
`has_more=true` marks a truncated first page. There is no continuation cursor
yet. Source coverage is partial if a selected row lacks verifiable lifecycle
provenance. The additive W5 Gate1 inventory connects the authorized in-store
S4 hold/run/stage/session/result tables, S6 tuple/result bindings, approval and
grant/token/outcome tables, scope/service event and recovery tables, and S5
local publish outcomes. Every table summary reports availability, row count,
SHA-256 content digest over deterministic sorted row serialization, and its
measured schema. Zero rows remain `available, 0 rows`. Summaries are computed
inside the same authenticated ProductStore read transaction and only for
orders/sources visible through the caller's current `ReadOperationalAudit`
grants; raw rows are not returned. The frozen v3 `sources` values remain for
wire compatibility; `connected_sources` is the additive current inventory.

S5 is explicitly local publish history. Remote verification remains a
separate unavailable source because no remote adapter is configured. S2/S3
are not connected because no verified snapshot adapters exist. Casting is
not applicable to the measured ProductAtomicV1 store: `casting_runs` is
absent there and `CastingLedger` opens a separate SQLite file; if a casting
table is present in a ProductAtomicV1 file, its rows still lack product
order/source provenance and are not included in this scoped report. M6
standalone ledger/replay evidence is not mounted through ProductStore.
Governance evidence is distinct: the governance journal is opened by the
standalone serve state and is not part of this authenticated ProductStore
snapshot. Scope and source-change events retain their event-row source IDs and
revisions; service events do not store a source ID, and the audit does not
infer one from current grants. This is operational evidence scoped to the
caller's grants, not an organization-wide security audit.

## Product routes and CLI wired in this checkpoint

`foundry serve --product --db <path>` mounts the isolated `http::product_router` over a ProductStore for that same file. It requires `FOUNDRY_INSTALLATION_ID` and `FOUNDRY_DATABASE_ID`; a standalone database or conflicting identity is refused. Registered routes include product login/logout/session, account administration, approval grants/approval, export/audit, `GET /api/product/review/holds`, `GET /api/product/orders/{order_id}/review/holds`, `POST /api/product/orders/{order_id}/review/holds/{hold_id}/accept`, and `GET /api/product/review/operations/{operation_id}`. Account administration requires a current Owner. Hold reads and decisions require a current Owner or Approver session and current enabled ProductStore source binding. Decisions include exact hold expectation and idempotency key and return the immutable receipt plus progress phase. The existing frozen-pack host is attached when `FOUNDRY_REVIEW_PACK` and separate artifact/session/claim roots are configured; without the runtime, reads remain available but decisions fail before acceptance. Resume also requires the real executor binary/repository pair. Login checks durable installation-local throttle state (five failures in a five-minute window, then five minutes blocked); throttle rows are capped at 1024 account keys. Approval requires an idempotency key, expected source revision, tuple coordinates/digests, and a grant bearer; it does not publish. Export uses fresh per-export HMAC aliases. Audit reports verified S1 plus explicit unavailable sources and does not claim complete cross-system evidence.

`foundry product-bootstrap --db <path> --installation-id <id> --database-id <id>` reads exactly two bounded stdin lines (`actor_id`, then password). Interactive TTY input is refused; the password is never accepted as an argv field. Product-mode serve mounts product APIs only; the older board and policy remain StandaloneV1-only.

`foundry product-login` reads actor ID and password from bounded stdin and stores the bearer only in a newly created mode-0600 session file. `product-grant` writes its one-time token to a private file; `product-approve` reads both secret files and calls the same ProductStore prepare/finalize path. `product-review --action holds|progress|resume|commit-pass|commit-fail|abort` uses the protected session file for authenticated hold reads, decisions, and progress. Decisions also need the configured ProductStore-backed frozen-pack host. `product-reset` is local-only; HTTP bootstrap/reset routes are not exposed.

## Not wired yet

- The shared frozen-pack host now has a public `PackReviewHost::with_product_store` constructor. ProductStore registers the exact host-generated hold snapshot. Authenticated ProductAtomicV1 HTTP routes and `product-review` CLI commands expose hold discovery, decision acceptance, and actor-bound progress. The router test generates H1 through the existing frozen-pack host, accepts Resume through a real authenticated request, observes executor activity and durable progress, and confirms an exact-key retry does not execute stages again. The legacy `RunRecord::hold_json` remains for standalone compatibility; product views withhold raw hold JSON. Ambiguous continuation recovery reconciliation and full lifecycle coverage remain incomplete.

- The ordinary `foundry serve` mode, `ServeState`, legacy `http::router`, and `foundry approve` CLI still use the older sidecar identity/scope stores and Line approval path. Product mode is explicit; the legacy routes are not a ProductAtomicV1 read/hold/policy/export surface.
- Product login throttling, logout, account create/role/disable administration, local reset, current-session resolution, scoped order/source management, export, and operational audit are wired. Product policy remains unavailable in product mode.
- `foundry product-watch --once` and its long-running form use the ProductStore-backed GitHub tick. The tick verifies the repository locator against the configured stable ID, fetches at most five 100-item pages, commits page snapshots plus cursor, and admits the original issue snapshot through the existing product inbox/S1 path. A local acknowledgement records only the exact durable admission receipt; intake uses GET requests only and never mutates GitHub. It requires a preconfigured `github-primary` ProductAtomicV1 source binding. The connector token is resolved from the named environment variable only at run time. Durable retry state handles transport/5xx/429 and rate-limit 403 responses with exponential delay capped at one hour and GitHub Retry-After/reset capped at 24 hours; a committed page clears it. Malformed individual issue items advance with a durable digest-only rejection record. Work claims use a persisted epoch, fence, lease and source-revision check; expired claims can be reclaimed and stale proofs fail. Ordinary GitHub admissions remain `waiting_unsupported`; this build does not claim arbitrary issue bodies or review packs execute.
- The product publish authorization gate and injected-Publisher composition are implemented and narrowly tested. The authenticated `foundry product publish` CLI path consumes a receipt file and fails closed when no adapter is configured; its local mock also distinguishes an existing PR and closed+merged PR (both recover through Confirmed) from closed+unmerged (reported Rejected, durable outcome stays pending, and a new approval is required before retry). Target-repository verification is bound to the configured adapter and lands with the real W6 adapter; there is no durable target-repository configuration today. The real external adapter remains W6 under separate approval. Post-commit Line projection and ordered mirror catch-up are implemented and narrowly tested. Product hold HTTP/CLI query, accept, and progress are wired; explicit ambiguous continuation recovery remains incomplete.
- Jira intake remains backlog. B2 multi-pack/order-stream selection and worker/container separation remain out of scope.

## Jira backlog

Jira adapter/store/routes/tests remain deferred. No Jira-specific behavior is implemented or implied by the GitHub work. B2 multi-pack/order-stream selection and worker/container separation remain out of scope.

## Remaining B1 acceptance gaps

W5 Gate1 connects the in-store audit source summaries and immutable
event-time source attribution described above. W5 Gate2 owns the export MUST
requirements and product-server P1/P2 acceptance; Gate1 does not claim those
items complete.

- The ordinary board HTTP and `foundry approve` CLI still use legacy sidecar identity/scope stores. ProductAtomicV1 has an authenticated receipt-file `foundry product publish` CLI path and durable publish outcome; builds without a configured adapter fail closed. Local mock coverage verifies existing-PR and closed+merged recovery through the Confirmed idempotent path, while closed+unmerged is reported Rejected and leaves the outcome pending, grants no fresh execution authority, and requires a new approval to retry. Target-repository verification is bound to the configured adapter and remains for W6 because no durable target-repository configuration exists today. A real external adapter remains W6 under separate approval; approval does not trigger publishing.
- Durable GitHub polling, restart cursor, inbox admission/local acknowledgement, basic rate-limit retry, CLI watch integration, and ProductStore-backed fenced work claims are present. A claimable row currently requires an explicit adapter state; production admissions remain unsupported because the repository does not define an issue-to-dep-bump payload grammar or a safe adapter mapping. Retry jitter and complete-sweep/non-starvation coverage remain incomplete. The bounded fake-server tests use explicit loopback HTTP opt-in; the product CLI does not expose that opt-in.
- Product hold acceptance, genuine host registration, authenticated HTTP/CLI endpoints, and accepted Resume through the existing host algorithm have a focused route test. Remaining work includes explicit recovery reconciliation after ambiguous stages and further followup-cycle route coverage. Separate-process service revoke/source rebind ordering around host stage start is covered in both commit orders by `b1_service_review`; human acceptance-versus-revoke ordering remains in-process. Scoped export/audit have focused capability, foreign-row, alias, and partial-source coverage tests; the CLI-launched loopback request test needs rerun where sandbox loopback binding is permitted. Product approval projection and ordered mirror catch-up have focused tests; injected publishing has positive and denial composition tests but no external adapter invocation.
- Product hold routes use committed immutable ProductStore acceptance, a current session, and the host's exact progress fence. Ambiguous in-flight work is reported as recovery-required and is not automatically replayed. Legacy `/api/orders/.../review/hold/...` handlers still authorize through `ServeState.identity` and call the standalone host database; they are separate from ProductAtomicV1.
- Separate-process different-key single-winner and identical same-key replay checks, plus child SIGKILL after each of the four final-transaction inserts, were exercised in the bounded ledger checkpoints. The `b1_service_review` target separately runs service revoke and source rebind both before and after a real host stage-start commit, with the manager and host in distinct processes. Human acceptance-versus-revoke ordering and promoted-tuple races, full affected-crate/example/Clippy gates, independent review, and final parent verification remain pending.

The lower-level atomic tests do not establish those unwired product behaviors. The current process-crash test proves rollback while a child is blocked at COMMIT and durability after a child reports COMMIT success; injected SQLite triggers separately prove rollback at S1, token, grant, and outcome write boundaries.

## Source/action service authority

Product review services are separate from human accounts and worker lease IDs. `ProductServiceAction` currently exposes only `intake_read`, `admit_delivery`, `ack_delivery`, and `drive_review`; none grants hold resolution, order approval, or publishing. An Owner explicitly registers each service with source-instance/action grants and a caller-supplied finite lease through `ProductStore::manage_product_service` or `foundry product-service --action configure`. New source generations do not inherit grants. Service revisions change when grants or service state change; credential revisions change on issue/rotation. Both are checked with the source instance on protected use.

`issue_product_service_credential` issues a random 256-bit bearer once and stores only its SHA-256 verifier. The CLI writes the bearer to a new mode-0600 file with create-new semantics; it does not print it. A repeated issuance request key fails and cannot recover the secret. If file creation fails after the database commit, issue a fresh rotation using a new request key and a new private file path. `foundry product-service --action revoke` disables the service and its grants in one transaction. `query` returns service revisions, lease, enabled grants and no credential material. These management operations are CLI/ProductStore APIs; there is no service-management HTTP route.

`foundry product-watch` now requires `--service-file` in addition to the independent GitHub token environment reference. The polling code checks `IntakeRead` before each repository or issue request and again before each page commit. Service-specific due/admit/ack and work-claim operations recheck `AdmitDelivery`, `AckDelivery`, or `DriveReview` inside their SQLite transaction and filter by the authorized source instance. The host receives a typed service binding through the durable ProductWorkClaim; every new external review stage rechecks the current DriveReview grant, service/credential revisions, source generation, lease, worker epoch and fence in the stage-reservation transaction. Result collection for an already accepted stage checks the original worker fence but does not require the service to remain enabled; a later new stage is denied after revoke/expiry/rebind. Worker ownership alone grants no service authority.

An effective service configure/revoke mutation also replaces the stored bearer
verifier with an unreachable random verifier and advances credential revision
in the same transaction. This applies to grant changes and disable/re-enable;
the old bearer cannot become valid again when the service is re-enabled. Exact
management retries return their prior receipt without rotating a second time.
Re-enabling or regranting requires an explicit new credential issuance with a
new request key; the old plaintext is never recoverable. Expired credentials
are denied at `expires_at <= now`.

Accepted human hold receipts additively bind the actor, source instance, scope
revision and ResolveReviewHold grant revision that authorized the decision.
Starting a new continuation compares those fields with current authority in
the same write transaction that claims the accepted hold. A revoke/regrant
therefore does not revive the old continuation permission. Historical receipt
JSON remains unchanged; receipts predating this binding deserialize with
missing values and fail closed for new continuation. Already recorded stage
results and terminal human decisions remain history and are not erased.

`ProductStore::open` uses the system UTC clock. `open_with_clock` is an
additive test/integration seam; protected review operations sample it once
after acquiring their SQLite write transaction. The real watch tick uses a
fresh system-clock sample at each authorization request and before durable
page persistence, while the explicit clock entrypoint supports deterministic
boundary tests. Compatibility polling wrappers retain their fixed caller
instant. Provider backoff timestamps remain separate from authorization
lifetime.

This does not add an issue-body/task adapter. Ordinary GitHub issues remain `waiting_unsupported`; the service review integration test selects a synthetic local adapter state only inside its fixture, then exercises the existing frozen-pack host. The ordinary watch command can poll/admit/ack and claim only backlog explicitly made ready by a supported adapter. The loopback GitHub tests now provision a real scoped service, but sandbox socket creation returned `PermissionDenied`; rerun `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo test --locked --offline -p foundry-watch --test b1_github_polling` in a socket-enabled parent environment.

## W1 source identity correction

`repository_id` is the stable provider ID. API owner/repository name is an
explicit locator mapping used to construct GET URLs; it does not identify or
authorize a repository. The legacy `legacy_repository_locator` field remains
an explicit v1 compatibility value and is not silently reinterpreted as the
stable ID. Additive mapping and revision columns preserve old history; rows
without reviewed provenance stay unbound and fail closed. Rebinds create a
new source instance, increment generation and connector revision independently,
retire the old instance revision, and transfer no service grant. The service
test covers a new grant and rotated credential on the rebound instance, denial
of the old context, exact own-receipt retry, edit snapshot preservation, and
foreign-provenance collision rejection without changing the incoming fetched
delivery or old receipt.

GitHub polling validates both metadata `id` and `full_name` before fetching,
then validates metadata again after each issue page and before local persistence.
This rejects a locator rename/name reuse observed during a page request. Since
the issue endpoint remains owner/name-addressed, a remote change after that
metadata check and before/during the SQLite commit remains a TOCTOU race; this
implementation does not claim remote atomicity or assume an undocumented
stable-ID issue endpoint.

## Product artifact reads (W3)

The ProductAtomicV1 artifact producer is the ProductStore-backed frozen review
host. It creates accepted stage outputs below the explicitly configured
`FOUNDRY_PRODUCT_ARTIFACT_ROOT`; successful stage evidence contains the
SHA-256 of the exact output bytes and the product review store commits the
stage row in the same transaction. That transaction also inserts an immutable
`product_artifacts_v1` registration bound to the verified S1 admission
receipt, source instance and canonical repository key, lifecycle, attempt,
run, generation, pack digest, stage sequence/id, byte digest and relative
file location. The artifact ID is a domain-separated digest of this binding.
The root path is separately pinned in the same ProductAtomicV1 file; another
process cannot silently select a different root.

The product router exposes an authenticated list and byte-read route under
`/api/product/orders/{order_id}/artifacts`. `ProductStore` checks the current
session, role ceiling and explicit `ReadArtifact` grant against the artifact's
original source instance in one SQLite read snapshot. A disabled or retired
source remains readable only while that original explicit grant remains
enabled. A grant on a rebound source does not apply to old registrations.
The caller supplies an order ID and opaque artifact ID, never a filesystem
path. The store verifies the durable admission and provenance digest, confines
the registered relative path to the pinned root, reads the whole file, and
checks SHA-256 before the HTTP handler returns any bytes. Denials and file or
integrity failures use a path-free response. On Linux the configured root is
pinned by directory handle and its device/inode identity is retained with the
installation. Reads and registration walk one component at a time from pinned
directory handles with `O_PATH`, `O_NOFOLLOW`, and `O_DIRECTORY`; the final
object is pinned and checked as a regular file before its bytes are opened
through that descriptor. This prevents ancestor, leaf, and configured-root
rename/symlink replacement from redirecting the opened object. The primitive
depends on Linux procfs descriptor paths and fails closed on other platforms.
The configured root is trusted at the moment it is pinned; in-place artifact
modification is detected by the full byte digest before response delivery.

The product review host's stage artifact is not an `ArtifactTuple`: the
current product host does not create a promoted tuple record for these stage
outputs. The existing tuple producer is `DurableReviewRun::run`; it creates an
`ArtifactTuple`, invokes `emit_once`, then records and promotes through
`TupleStore`. Its SQLite implementation is `SqliteTupleStore`, a distinct
standalone contract store. No ProductAtomicV1 typed producer path currently
connects these tuple/promotion records to stage artifact registration. Calling
the existing producer from this checkpoint would cross into its emit/publish
effect, which belongs to W4, so no stage log is relabeled and no tuple is
manufactured. W3 therefore remains incomplete on tuple binding. Legacy or
unregistered files remain unavailable. The response holds verified artifact
bytes in memory before delivery, so memory use scales with the requested file;
no new size limit is asserted.
