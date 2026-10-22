# B1 per-user scope and migration

This document records the approved single-organization scope policy and the
ProductStore-backed entrypoints. Source/grant management is available through
the product HTTP API and `product-scope` CLI; product order list/detail routes
use scoped reads. Product review and GitHub intake also carry explicit
service authority through their current consumers. Scoped exports and
operational audit exist as ProductStore APIs, with their route and CLI wiring
and the broader B1 acceptance gates tracked separately below and in the
product contract.

## Identity and provenance

A repository identity is the tuple
(organization, provider, canonical API origin, stable repository ID).
Organization/provider are lowercased; the origin must be an HTTP(S) origin
without user information, query, fragment, or non-root path. Scheme and host
are normalized and default ports are removed. Repository keys use a
domain-separated, length-framed SHA-256 digest of the canonical tuple.
Matching numeric repository IDs at different origins remain distinct.

Every configured connector generation receives an immutable source-instance
ID. Updating or rebinding a connector retires its current instance and creates
a new generation. Grants never copy to that generation. Historical order
provenance stays attached to the original source instance. A verified
historical grant can still read old data after that source is disabled; new
approval/hold mutations require the current enabled source instance.

An order is visible through scoped APIs only when an immutable provenance row
matches its ProductStore order record, S1 admission receipt, lifecycle, and
verified S1 replay. Orders without that mapping are denied. Migration records
only the finite order set explicitly listed in the manifest; every other
legacy order is placed in a privacy-safe quarantine record.

## Capabilities and management

Role is an upper bound; it grants no data access by itself. Viewer and
Operator can receive ReadOrder. Approver and Owner can receive the explicit
data capabilities ReadOrder, ReadReview, ResolveReviewHold, ApproveOrder,
ReadArtifact, ExportEvidence, ReadOperationalAudit, and PublishApproved.
Publish permission is independent of approval permission. Owner account,
source, and grant management is separate from repository data access, so an
Owner with no grant cannot read an order.

Remote Owners cannot expand their own data scope. Initial Owner data access
must be listed in bootstrap_owner_scoped; subsequent grants require another
current Owner session. Grant/revoke operations check the target role ceiling,
an expected per-subject/source scope revision, and a request key in one
BEGIN IMMEDIATE transaction. Events and receipts commit with the current
grant/revision. Exact retries return the stored receipt; a changed request
under the same key conflicts. Revoke/regrant increments grant and scope
revisions. Role changes invalidate that subject's grants and pending approval
reservations, so promotion does not revive a prior grant. Source rebind keeps
old grants on the old source instance.

## Fresh installation and maintenance migration

Fresh installations use `bootstrap_owner_scoped` with a finite JSON manifest.
`foundry product-bootstrap` accepts `--manifest`; omitting it means an
explicit zero-source, zero-grant bootstrap and still activates enforced scope.
The old `bootstrap_owner` method remains only as a compatibility primitive
for isolated pre-scope library fixtures. Product CLI/HTTP paths do not use it.
The manifest file is bounded to 64 KiB and contains only source identities and
explicit grants; passwords remain on bounded stdin. For example:

```json
{
  "sources": [{
    "connector_id": "github-main",
    "source_instance_id": "reviewed-source-1",
    "generation": 1,
    "identity": {
      "organization": "acme",
      "provider": "github",
      "api_origin": "https://api.github.com",
      "stable_repository_id": "42"
    },
    "legacy_repository_locator": "42",
    "scope_query": "open+foundry:make",
    "enabled": true
  }],
  "grants": [{
    "actor_id": "owner",
    "source_instance_id": "reviewed-source-1",
    "capabilities": ["read_order", "read_review"]
  }]
}
```

Owner source configuration and grant/revoke requests include expected
revisions and request keys. Source and grant queries return the current
revision needed for the next conditional mutation. Rebinding creates a new
source instance and does not transfer grants. Product mode refuses an
unactivated scope database at startup; `product-migrate-scope` is the
explicit local maintenance command for a stopped-process reviewed migration.

Existing schema-v1 ProductAtomic databases require an explicit
ProductStore::migrate_scope_v1 call. Normal ProductStore::open refuses
schema v1, preventing an old process from silently opening a migrated
database. Operators must stop old processes before migration; the function
cannot prove that operational precondition. Supply a reviewed manifest with
canonical repository identities, source instances, exact verified
order/admission coordinates, and finite user grants. The migration:

- runs in one SQLite write transaction and adds scope tables without rewriting
  ledger or receipt bytes;
- validates listed source records and S1/admission receipts;
- quarantines every order omitted from the verified list;
- revokes old sessions and fences prepared operations;
- invalidates unconsumed approval grants without inserting token-consumption
  records; and
- advances the ProductStore schema gate only when all work commits.

Any validation or SQL error rolls back the schema, records, and version change.
No grants are inferred from old roles or connector configuration.

## Scoped evidence export and operational audit

`GET /api/product/export` and `GET /api/product/audit` select evidence through
`ProductStore::scoped_evidence_snapshot`. The current session, role ceiling,
explicit `export_evidence` or `read_operational_audit` grant, verified S1
chain, lifecycle receipt, source provenance, and selected rows are checked in
one SQLite read transaction. `read_order` does not substitute for either
capability. Historical rows remain readable under an existing grant bound to
their retired source instance; a rebind creates a new instance and does not
transfer that grant.

S1 verification is a two-pass cursor traversal in that same snapshot. The
first pass checks each row's existing v1/v2 hash and global sequence link in
`seq` order, retaining only the previous hash and sequence. The second pass
uses the added `(order_id, seq)` index and the existing replay decoder/domain
transition code, retaining one order lifecycle at a time. Authorized evidence
is fetched afterward with SQL provenance and capability predicates; there is
no whole-ledger `Vec` or global sequence set. Verifier auxiliary row/order
state is constant as unrelated order count grows. The complete S1 chain still
costs O(total S1 rows) time, and any chain/domain corruption fails every
scoped export/audit read. Returned selected evidence itself can grow with the
visible history for up to the existing 100-order selection budget; this does
not impose a byte or row bound on that history.

Responses use a fresh random HMAC key and context per request. Public rows use
local contiguous ordinals and short-lived pseudonyms. Their digest covers the
serialized public projection, not the S1 chain. Responses contain no global
sequence, previous hash, chain head, raw source or installation identity, or
hidden-row count. The optional `limit` is the existing 1..100 scoped order
selection budget (default 100). A response says `complete` when all currently
visible orders fit and `page` plus a capability-filtered `has_more` when more
visible orders remain. There is no continuation cursor in this version, so a
`page` response is explicitly incomplete and cannot be represented as a full
export. Audit S1 availability is `partial` when the selected page is
truncated or selected rows lack matching verified lifecycle provenance.
S2/S3/S4/S5/S6/M6/governance/casting remain individually unavailable with
reasons; this operational inventory is not organization-wide security audit
authority.

The CLI loopback integration test now waits for an HTTP auth response, parses
Content-Length or chunked framing, and uses a finite 15-second request deadline
inside a 120-second scenario deadline, with bounded headers/body. These are
test execution allowances, not product latency targets. Failure diagnostics
identify method, route, elapsed time, received bytes, and connect, readiness,
login/KDF, header, body, or EOF-wait phase without printing request content or
credentials. The helper caps response headers at 16 KiB and the body at 1 MiB,
which leaves ample room for these local order/export responses while preventing
unbounded test reads. The earlier 3-second `read_to_end` timeout was observed;
its underlying cause remains undetermined.

## Fail-closed behavior and remaining wiring

On an enforced ProductStore, scoped order list/detail, hold list/progress/accept,
approval grant issuance/preparation/finalization, repository source
management, export, and operational audit consult current scope in their
ProductStore transaction. Raw `projection_snapshot`, the existing publisher
authorization without a current session, review-worker configuration/claims,
and GitHub intake retrieval remain unavailable after scope enforcement when
their contracts do not carry the approved user or service authority. They fail
closed.

The compatibility mode remains for old library fixtures and databases that
have not been explicitly activated. It is not a scoped security claim. The
product server rejects that mode, and product approval/review CLI commands
require enforced scope. Local source setup and maintenance migration remain
available to move an installation into enforced mode.

## Isolation limits

Product authorization is enforced by SQLite transactions and typed
ProductStore APIs. This does not isolate processes running as the same OS
user from the database file. Stronger worker/container separation remains
outside B1 and belongs to B2.

## Source identity, locator, and revision binding

`RepositoryIdentity` is the canonical authorization identity: organization,
provider, canonical API origin, and stable provider repository ID. The API
owner/repository name is stored separately in the additive
`product_scope_api_locators_v1` mapping and is used only to form provider API
URLs. The compatibility field `legacy_repository_locator` remains explicit
in v1 manifests/receipts; it is never treated as stable identity or routing
authority. New source configuration requires both representations. The
legacy `product_sources_v1.repository_id` now stores the stable ID for new
bindings; migration accepts an old value as a locator only when the reviewed
manifest explicitly identifies it as such. It does not guess an owner/name,
rewrite old orders, or change historical ledger and receipt bytes. Old poll
bindings receive zero-valued additive revision fields and fail closed until
explicitly rebound.

Intake records connector revision, source instance revision, and source
generation independently. Page persistence rechecks all three plus source
identity and API locator in the same transaction that writes deliveries and
cursor. A rebind retires the old source instance and does not copy its service
grants. The service must receive explicit grants and a newly issued credential
for the new instance; contexts from the prior instance fail on every protected
operation. Existing admission receipts are returned only when verified
immutable provenance matches that exact source instance, canonical origin,
connector, and stable repository ID. A foreign collision leaves the incoming
delivery fetched and the original S1 receipt unchanged.

The poller uses the existing repository metadata GET both before polling and
after each issue-page response. It requires both the stable `id` and the
expected `full_name` before committing that page. This detects a rename or
name reuse that occurs during the page request. GitHub issue retrieval still
uses the owner/name endpoint, and there is a remote TOCTOU window after the
post-page metadata response and before/during the local SQLite commit (and a
remote rename can occur between metadata and the issue request). No stable-ID
issue endpoint or stronger remote atomicity is assumed, so this check does
not claim to eliminate every remote name-reuse race.

## W2 accepted-hold authority and clock binding

Newly accepted hold receipts persist the approving actor and the source
instance, source generation, scope revision, and ResolveReviewHold grant
revision. A continuation must present the current session and current grant,
then match the immutable receipt binding while claiming progress in its write
transaction. Revoke/regrant cannot restore execution permission for an older
acceptance. Older receipt JSON remains readable, but missing authority fields
deny new continuation; recorded human decisions and accepted stage results
remain intact. The actual host continuation path uses this ProductStore check.

Before a new external stage is reserved, that transaction also validates the
current DriveReview service, credential and grant revisions, source instance,
service lease, and exact worker ownership/fence. An already accepted stage
result can still be collected and persisted after service revoke, while the
next new stage is denied. Service configure/revoke mutations rotate the
stored verifier and advance credential revision transactionally, including
disable/re-enable and grant changes. A fresh bearer requires explicit issue;
management retries do not rotate again.

ProductStore defaults to the system UTC clock and exposes a typed injectable
clock for deterministic tests. Protected write operations sample one instant
after acquiring `BEGIN IMMEDIATE`; expiry and lease equality deny. Real watch
polling samples fresh time before each protected provider request and again
before page persistence, so time spent in an HTTP request cannot extend the
service credential's lifetime. Legacy caller-time wrappers remain for
compatibility and fixed-time fixtures.

These local authority checks do not make a remote owner/name-addressed GitHub
request atomic with remote repository identity. The pre/post metadata checks
still leave the previously documented remote TOCTOU/name-reuse limitation.
`b1_service_review` now exercises all four service/source mutation-order cells
with a host continuation in a child process and the mutation committed through
a separate ProductStore handle: service revoke before stage start, service
revoke after a committed stage start, source rebind before stage start, and
source rebind after stage start. The before-start cases assert no stage call or
continuation-state change; the after-start cases preserve the one authorized
result/session and deny the next stage. The same target also covers worker
reassignment and committed-stage crash/reopen. Human revoke/regrant and
polling-expiry cases use their existing authenticated host and injected-clock
tests; they are not claimed as separate-process races.
