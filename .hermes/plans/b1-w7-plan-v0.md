# B1 W7 — Real GitHub Verification Plan (v0 DRAFT — pre-review, not frozen)

Target repo (user-specified, 2026-10-25 anchor 1563842659846586441): `github.com:lunarpulse/foundary-demo.git`
Base HEAD: eaebafd (=origin/main, W6 complete). Source: b1-closure.md :137-139 — "별도 repo/effect/cost/cleanup 승인을 받아 실제 GitHub 검증. 미승인은 미검증으로 보고한다."

## 0. Measured facts (parent, 2026-10-25)
- SSH auth: ACTIVE (`Hi lunarpulse!`), origin push already uses this key.
- foundry-demo remote readable; HEAD = 68deb4297564ebd7c1deaafd78ce46524d1034f4 — identical to the result_commit produced by example m8_live_die during W6a Z4 (a real write to this repo has occurred once before, from this host).
- gh CLI: NOT installed. No API token present in this session. No credential values may appear in logs/specs; references by path/env-var only.
- Existing mocked consumers: b1_github_polling (8/0, foundry-line), P1 intake (b1_intake_durability 10/0), P2 publish path (b1_product_publish etc.) — all green offline at eaebafd.

## 1. Scope proposal — two lanes, each independently approvable
- **W7-A (SSH-only, no token)**: verify durable publish against the real remote via git over SSH. Effect: temporary branch(es) on foundry-demo only; no issues/PRs; cleanup = branch deletion after verification record. Zero API calls.
- **W7-B (token, requires user-issued PAT)**: verify the P1 (issue intake → admission → ack → waiting_unsupported) and P2 (issue → genuine host → human approval → publish consumer) loops against real GitHub issues/PR API on foundry-demo. Effect: N test issues + 1 PR (N ≤ 3 proposed), clearly labeled `w7-verify`; cleanup = close/delete all created artifacts and record their IDs before/after.
- Unexecuted lane stays NOT-PERFORMED with exact blocker (token absence), per honest-reporting discipline.

## 2. Credential boundary (frozen rule)
- Secrets never enter chat, specs, scratch logs, or commit content. Parent references only an env var (e.g. GITHUB_TOKEN_FILE) or gh keyring path. Token scope: fine-grained PAT limited to lunarpulse/foundary-demo, issues:read/write, pull_requests:write, contents:read (no admin, no org, no workflow).
- SSH key stays as-is (already in use for origin).
- All network executions happen in the PARENT shell (luna sandbox has no network) — same pattern as Docker lanes.

## 3. Execution plan (each gate stops at its commit for user report)
1. **R0 freeze**: user approves this plan (v1 after review reinforcement) + chooses lanes (A only / A+B / B only) + effect budget (max issues/PRs/branches).
2. **R1 (W7-A)**: parent executes real publish path: create labeled branch `w7-verify/<ts>` on foundry-demo via the product publish consumer route (not raw git plumbing — the actual consumer code path), verify remote ref exists (ls-remote), record evidence, delete branch, verify deletion.
3. **R2 (W7-B)**: parent sets token from user-provided path, runs P1 real intake against a `w7-verify` labeled issue (admission→ack recorded), then P2 issue→host→approval→publish through the production route with 401/no-token negative first; records PR URL and remote state; closes all created issues/PR, verifies closure.
4. **R3 evidence + sol review**: evidence doc b1-w7-evidence.md with every created artifact ID (before/after), exits, remote-verification commands and outputs; sol read-only adjudication against frozen rubric.
5. **R4 acceptance**: parent re-verification → user final approval → single integrated commit/push (plan+evidence+code delta if any consumer gap surfaced; production code changes, if needed, follow the standard luna→round→sol cycle first).

## 4. Cost / cleanup summary (for approval)
- W7-A: 2+ remote refs created+deleted on foundry-demo (branch churn only). API cost 0.
- W7-B: ≤3 issues + 1 PR created and closed/deleted. API calls bounded (<50). No repo settings touched, no protection rules changed, nothing merged to default branch.
- foundry (origin) repo: untouched by W7 except the final acceptance commit.

## 5. Open decisions required from user
- (a) lane choice (A / A+B / B), (b) PAT issuance method (gh auth login on this host vs env-file path), (c) effect cap confirmation, (d) whether foundry-demo default branch must stay untouched (assumed YES).
