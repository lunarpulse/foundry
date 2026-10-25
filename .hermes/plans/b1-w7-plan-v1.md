# B1 W7 — Real GitHub Adapter Completion + Verification Plan (v1, review-reinforced 2026-10-25)

Supersedes b1-w7-plan-v0.md. Reinforcement source: sol plan critique (proc_6448b2aa298e, 19 numbered items) — all 19 dispositioned below (ACCEPTED / PARTIAL / DEFERRED-with-reason).
Target repo (user-specified, anchor 1563842659846586441): `github.com:lunarpulse/foundary-demo.git`. Base HEAD: eaebafd.
Source contract: b1-closure.md :137-139 — "필요한 consumer 완성·로컬 gate 통과 후 별도 repo/effect/cost/cleanup 승인을 받아 실제 GitHub 검증."

## Phase 0 — Consumer completion (offline, standard luna→round→sol cycle, additive only) — REQUIRED BEFORE ANY LIVE CALL
Measured ground truth (sol-verified): `foundry product publish` (crates/foundry/src/main.rs:1089) routes only through the test-support file-registry adapter and rejects publication otherwise; the existing `GithubPublisher` (crates/foundry-watch/src/github_publisher.rs:101) is token+HTTPS+PR-creating and embeds the token in a remote URL passed via git argv (credential-exposure pattern), and pushes `foundry/<order>` branches.
P0 deliverables:
1. **Real adapter wiring (additive)**: production-configurable GitHub publisher bound to the product publish consumer route (same Publisher trait), default-off, enabled only by explicit config. NO change to existing tests or mock paths.
2. **Secret-safe auth (sol #7/#8 ACCEPTED)**: SSH transport for push (existing key, no token in argv/URL/config); API token used ONLY for issue/PR REST calls via Authorization header from a private credential file (`0600`, path via env var), never in URLs, argv, tracing, or retained git config. Post-run inspection of generated git config required.
3. **Local gates**: full workspace `--locked --offline` green + clippy B1-primary 0 + fmt on touched files; lanes for every touched crate; sol review of the code delta as its own gate (W7-G0 commit, stops for user report).
4. **Reconciler**: replay/reconciliation path for pending/ambiguous publish outcomes (sol #14) — real PR-state query resolving Confirmed/Rejected/Pending, idempotent re-run → no duplicate PR.

## Phase 1 — Live verification (each sub-phase = separate user approval of a concrete effect packet; parent executes all network calls)
**Rubric will be frozen as b1-w7-plan-and-rubric-v1.md BEFORE Phase 1 execution (sol #10 ACCEPTED)** — per-cell invocation, expected local+remote observations, effect allowance, PASS/FAIL/NOT-PERFORMED rule; unmet-precondition ≠ attempted-failure.

**G1 preflight (read-only API + ls-remote; effect: 0 writes)** — verify exact target, stable repo ID, default branch + SHA, existing refs/PRs, rule/Protection state, meaningful diff exists (populated repo — no-diff is a stop condition, not a pass) (sol #15). Record before-state: default-branch SHA, test-ref absence, `68deb429` noted as prior baseline.

**G2 P1 real intake (effect: 1 issue + label + close)** — create `w7-verify-p1` issue WITH the consumer-required `foundry:make` label (sol #5) on foundry-demo; run real intake: metadata/issue poll → durable inbox → admission → LOCAL ack (no GitHub comment; ack is local by design, intake_worker.rs:194) → `waiting_unsupported`, NO task execution. Re-poll + reopen same DB → preservation + dedup proven. Negatives SEPARATED (sol #13): rejected GitHub auth (bad token) vs missing product session vs missing publish capability — zero unauthorized mutations asserted.

**G3 P2 real publish (effect: ≤2 branches + 1 PR + close)** — explicit test-input adapter correlates the real issue (P1/P2 boundary preserved: NO arbitrary issue→execution, that is B2 — sol #4) → genuine host → human hold/approval (by user, bound to exact result SHA+tuple) → artifact promotion → approval receipt → durable publish consumer → real branch `w7-verify/<run-id>` + real PR. Approval outcomes withheld/denied/stale/expired each proven → no publication. Token ≠ product authorization (sol #6).
- Replay: repeat same approved operation → same bound outcome, no additional PR. Closed-unmerged PR → `Rejected` + no auto-republish (sol #14). Merge/race cases stay mocked (not authorized).

**G4 evidence (sol #11/#12 ACCEPTED)** — joined local/remote chain per cell: code revision, API origin, stable repo ID, source instance/revision, issue ID, order/run/attempt, approval receipt, operation ID, approved SHA, head/base refs, remote SHA, PR state — durable local records compared against INDEPENDENT remote reads. Before/during/after observations incl. post-cleanup: issue/PR closed, merged=false, run-owned branches deleted (expected-SHA-matched only), default-branch invariant. Absence checks authenticated.

**G5 failure handling (sol #16/#17 ACCEPTED as plan rules)** — request accounting (<50 calls total incl. retries/pagination/verification/cleanup), retry ceiling 2, per-call timeout 30s, rate-limit → stop+report (no exhaustion), run-id assigned BEFORE first write, partial-success recovery via reconciler, nonzero exit ≠ nothing-happened (residual artifacts enumerated). Token expiry mid-run → STOP + reconcile with SSH reads; credential renewal does not authorize new effects.

## Approval packet (single decision set for user — sol #18)
Effects on foundry-demo only: API reads; 1 label (`foundry:make` — required by consumer, pre-existing or created); ≤3 issues titled `w7-verify-*` created and CLOSED (not deleted); ≤2 run-owned branches created and DELETED; ≤1 PR created and CLOSED UNMERGED (never merged); harmless commit content on run branches only; default branch NEVER written. foundry (origin) untouched except final acceptance commit. Costs: API <50 calls, host CPU only, no workflows triggered beyond default issue/PR notifications (retained in remote history). Cleanup runs even on failure; cleanup-failure reported with artifact list. Isolation: separate DB/HOME/worktree per run; credentials not inherited by test host/Docker children (sol #9).
Lane note (sol #1 ACCEPTED): transport-lane A/B split is DROPPED — B1 closure requires real P1 AND real P2 (G2+G3); SSH-only push without PR proves nothing settleable and stays an internal preflight step, not a reportable lane. Anything not executed = NOT-PERFORMED and B1 completion stays pending.

## Gate ordering / commits (sol #19 ACCEPTED)
Phase 0 → W7-G0 code commit (stop, user report) → rubric freeze → G1→G4 checkpoints (report, no commits) → sol adjudication → parent re-verification → user final approval → single integrated commit/push. Consumer correction invalidates affected evidence → re-run within approved budget.
