# B1 W7 Phase 1 (G1–G5) Rubric v1 — FROZEN DRAFT (pending user approval)

Base: fe0ab99 (W7-G0). Plan: b1-w7-plan-v1.md. Target repo: lunarpulse/foundary-demo.
Credentials: SSH (push), PAT at ~/.foundary-w7-token (REST, path-only reference; value never in URLs/argv/tracing/errors/logs/git config/chat).
Effect budget (hard caps): label 1 (foundry:make) · issues ≤3 (closed, not deleted) · branches ≤2 (deleted after) · PRs ≤1 (closed, NEVER merged) · default branch NEVER written · origin (foundry) untouched except final acceptance commit · live REST/git calls <50 total · retries ≤2 per operation · run-id pre-issued before any live call.
Every gate G1–G3 requires an explicit user effect approval BEFORE its first live call (separate from the standing directive).

## G1 — Live preflight (read-only)
- G1.1 Repo fixed ID: GET /repos/lunarpulse/foundary-demo returns id, default_branch, and HEAD SHA; recorded verbatim.
- G1.2 Existing state inventory: existing refs (w7-verify/* must be absent or enumerated), open PRs enumerated (must be 0 in our namespace), branch protection rules on default branch recorded.
- G1.3 Valid diff exists: local worktree vs target HEAD has a non-empty, meaningful diff — empty diff = STOP condition (no publish).
- G1.4 Auth boundary: PAT used ONLY for REST reads here; SSH-only push verified (no HTTPS write path exercised).
- G1.5 Zero writes: git push --dry-run output recorded; no branch/label/issue/PR created.
- G1 PASS = all five recorded, zero side effects. Failure at any point = STOP + honest report, no retry beyond budget.

## G2 — Live P1 (issue intake path)
- G2.1 Create exactly ONE issue labeled foundry:make (+w7-verify marker) via REST; issue number/URL recorded.
- G2.2 Real polling: adapter fetches issues by label; the created issue appears exactly once in one poll cycle.
- G2.3 Inbox→admission: issue admitted into durable inbox through the real consumer path.
- G2.4 Local ack: ack recorded LOCALLY (no GitHub comment — comment posting is out of scope), issue state transitions to waiting_unsupported.
- G2.5 Re-poll invariance: second poll cycle produces no duplicate admission (dedup proven with recorded operation/inbox ids).
- G2.6 Negative case separation: GitHub auth failure (401/403 on a deliberately-bad-path probe against the SAME adapter, not the user PAT) is distinguished from product session/permission absence — recorded as distinct outcomes.
- G2.7 Cleanup: issue closed (not deleted); issue count against budget = 1.
- G2 PASS = G2.1–G2.6 all recorded + cleanup done. Any 429/403 Retryable → stop after ≤2 retries.

## G3 — Live P2 (publish path with human approval)
- G3.1 Explicit test adapter correlation: a real admitted issue (≤2 total issues budget) is correlated to the explicit test adapter through the production route.
- G3.2 Genuine host: artifact produced by genuine host run (result_commit digest recorded = expected_sha).
- G3.3 HUMAN APPROVAL: user personally performs hold/approve via the product CLI (agent automation invalidates this cell). Approval anchor + result SHA recorded.
- G3.4 Real branch push: object-pinned push of expected_sha to w7-verify/<run-id> over SSH; pre-push HEAD re-verification logged; branch count = 1.
- G3.5 Real PR: exactly one PR created via REST (title/body carry run-id); reconciler confirms Confirmed{pr_url} only after head_sha==expected_sha AND head_ref==deterministic branch.
- G3.6 Hold/deny/expiry negatives: at least one hold or denial path proven → no branch/PR created for the unapproved operation.
- G3.7 Idempotent re-run: second reconcile of same operation_id → same bound outcome, no second branch/PR.
- G3.8 Cleanup: PR closed (NEVER merged), branch deleted; recorded absence verified.
- G3 PASS = G3.1–G3.7 recorded + cleanup done + user approval was human-performed.

## G4 — Evidence
- G4.1 Local ledger vs independent remote reads: every remote state transition (issue, branch, PR) cross-checked by a SECOND independent read (fresh REST query / git ls-remote), not only via the adapter.
- G4.2 Receipt/operation-id/SHA/PR-state binding: each recorded effect links (operation_id, expected_sha, pr_url, final state) in one evidence table.
- G4.3 Post-cleanup absence: after cleanup, independent reads show no w7-verify/* branch, no open w7 PR, w7 issues closed.
- G4.4 Budget accounting: total live call count (<50), issues (≤3), branches (≤2), PRs (≤1) — actuals recorded against caps.
- G4 PASS = all four tables complete and internally consistent.

## G5 — Failure handling (evidence from any live failures encountered; if none occur, negative probes substitute)
- G5.1 Rate limit: a 429 (or 403+Retry-After) response maps to Retryable and the run stops within ≤2 retries (mock-verified in Phase 0; live occurrence recorded if it happens).
- G5.2 Partial success recovery: if any multi-step operation fails midway, reconciler re-run converges to a single consistent outcome (no duplicate effects).
- G5.3 "Failed exit ≠ nothing happened": every non-zero exit or error is accompanied by a recorded remote-state check proving what did/didn't happen.
- G5 PASS = each item either live-evidenced or honestly recorded as NOT-PERFORMED with reason (no fabricated substitutes).

## Adjudication
Parent runs gates; sol reviews evidence read-only after G4. Verdict per gate: PASS / FAIL / NOT-PERFORMED (honest). Rubric frozen at approval; factual errors corrected via Erratum only (original preserved).

---

# ERRATUM v1.1 (2026-10-28, user approval anchor 1564907629761921135 "권장대로")

Original text above preserved unchanged. Sole amendment:
- **Line 5 budget**: "combined live REST/git calls <50" → **"<60"**. Rationale: exact post-hoc ledger of Phase 1 (sol126 finding) totals 50 including failure-retry consumption (3 failed runner attempts × poll + 2 redundant clones); remaining gap remediation requires ~4 further calls. All other caps (issues ≤3, branches ≤2, PRs ≤1 never merged, default branch never written, retries ≤2/operation) UNCHANGED and remain hard.
No other row is amended by this erratum.

---

# ERRATUM v1.2 (2026-10-28, user approval anchor 1564934772734697503 "둘다 2")

Disposition of final adjudication rows (no caps changed):
- **G1.2**: NOT-DETERMINED is the FINAL recorded state — default-branch protection is unreadable with the user-issued contents:read PAT (403); accepted rather than issuing an administration-scope PAT.
- **G3.5 / G3.7**: NOT-PERFORMED-live is the FINAL recorded state — a live reconciler Confirmed would require amending the frozen PR cap (≤1) and an additional external write, and post-cleanup closed-unmerged PRs reconcile to Rejected by design; accepted on the strength of code-cited reconciler semantics, Phase 0 offline tests, and live manual double-execution convergence.
Adjudication closed: G1 PASS (G1.2 NOT-DETERMINED), G2 PASS, G3 PASS (G3.5/G3.7 NOT-PERFORMED-live), G4 PASS (58/60 under v1.1), G5 PASS.
