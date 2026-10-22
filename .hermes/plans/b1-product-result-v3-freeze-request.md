# B1 W3 v3 — Human freeze request

Status: APPROVED FOR W3 IMPLEMENTATION by user lunarpulse_, Discord 1561496539174801409 (승인), following the explicit progress-report freeze gate. This approval freezes the referenced effective v3 contract and authorizes ADDENDUM 59 + Codex CLI gpt-6-luna implementation/isolated verification only. It is not W3 acceptance, live DB migration, W4, commit or push approval. The request wording below is retained as the approved packet.
Contract: .hermes/plans/b1-product-result-package-spec.md, Candidate v3.
Effective precedence: v3 delta/Appendix D > v2 R1–R7/Appendix C > D1–D8 > B > A.

Review evidence: b1-product-result-contract-v3-check.log final verdict READY_FOR_HUMAN_FREEZE. Prior seven findings closed in v2 review; final legacy tuple census finding closed in v3 check. Read-only start/end/current git status equal; git diff --check passed. Parent Python SQLite in-memory seven checks passed: census population, insert/update/delete/REPLACE denial, fresh empty census, transactional DDL rollback. NOT Rust/ProductStore migration acceptance. Existing round59 430/0 remains a pre-implementation baseline only.

Requested policy/schema freeze:
1. Current original-source DriveReview allows new LOCAL result finalization, never ApproveOrder or PublishApproved. Capture exact original service/credential/grant/source revisions at review activation, copy on Resume; no inferred historical authority. Completed worker lease may expire, but current service authority remains required. Rotation/regrant cannot automatically authorize pending historical work. Tradeoff: deliberate recovery/new authority workflow needed later rather than silent reuse.
2. ProductStore is sole writer; existing numeric runs/reservations reused; result binds lifecycle/activation and rejects tuple-key aliases. CommitPass uses actual origin, not reserved successor. Finalization/replay/lookup/recovery never dispatch or publish.
3. Explicit maintenance-only product schema v2→v3 migration: immutable result/approval/authority records, separate metadata, sealed legacy host AND legacy tuple census. Preserve historical bytes/digests. Legacy tuple ownership yields Conflict, new missing binding is CorruptEvidence, never inferred absence/adoption. No auto-upgrade, no application to live DB under this approval.
4. Default-off p2-test-support feature forwarding in ledger/line/serve/foundry manifests. Fixed-purpose synthetic input/lifecycle preparation only. No product HTTP/CLI route even with feature unification; no ledger→line dependency. Real host plus fake executor proves W3 plumbing, not real code generation/Die/GitHub.
5. New typed tuple digest and exact local payload/receipt bytes as specified; commit ID, file SHA, tuple digest and binding digest remain separate. Evidence paths remain evidence, not code archives. Authenticate result-bound artifact reads; approval consumes exact binding atomically. Migration/history/replay/current authority errors remain fail closed.

Requested implementation authorization AFTER explicit freeze: ADDENDUM 59 + gpt-6-luna via Codex CLI only, W3 implementation and isolated tests; parent independently verifies producer→binding→authenticated read→approval path, exact generation fixtures/counters, corruption, authority races, crash/recovery, migration and feature isolation. Preserve valid accumulated B1 dirty work. Initial product redrive unsupported remains explicit; do not claim full general issue-to-code pipeline. Changes outside these semantics require human change control.

Not authorized by this request: W4 remote publication, actual GitHub/credentials/live DB migration, B2 expansion, commits or push. One final integrated B1 review/approval/commit batch remains required.

Earlier user approval 1561401650034245665 covers presented v2 direction only, not new exact legacy tuple census schema. The next explicit approval should name or clearly respond to this complete v3 freeze/implementation packet.
