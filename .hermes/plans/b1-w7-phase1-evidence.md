# B1 W7 Phase 1 (G1–G5) Evidence — v1 (2026-10-28)

Base: `fe0ab99` · Rubric: `b1-w7-g1-g5-rubric-v1.md` (frozen) · Target: `lunarpulse/foundary-demo` (repo_id 1403455083).
Approvals: rubric freeze `1563927286456254505` · G2 `1564001787650900081` · G3 `1564108762011144244` + gateway approved `1564870387232415885` · G4+G5 `1564886788206759947`.
Logs: `$S/b1-w7-g2.log`, `$S/b1-w7-g3.log` (S=/home/cosmo/.hermes/cache/scratch). All failures below are runner/ops errors — product fail-closed correct in every case; 0 product defects.

## G1 — Live preflight (read-only) — PASS
- G1.1 repo id `1403455083`, default `main`, remote HEAD `68deb4297564ebd7c1deaafd78ce46524d1034f4` (verbatim GET).
- G1.2 refs 130 via SSH ls-remote, `w7-verify/*` = 0, open PRs in namespace = 0; branch protection probe 403 (PAT contents:read) → NOT-DETERMINED (no impact: default branch never written).
- G1.3 valid diff: `fe0ab99` absent from target; 387-file diff vs target HEAD.
- G1.4 PAT REST-read-only; push path SSH only.
- G1.5 zero writes: ls-remote identical pre/post (POST_LSREMOTE_SAME=YES).

## G2 — Live P1 issue intake — PASS
- G2.1 issue **#145** created (201), label `foundry:make`, +w7 marker (creation 1/3 budget; reused via reopen/close thereafter).
- G2.2 real poll: `fetched=1 admitted=1 acknowledged=1` after propagation wait (~10s+; earlier `fetched=0` = GitHub list-API propagation delay, root-caused).
- G2.3 inbox→admission durable (`product_intake_inbox_v1` acked; orders=1, backlog=1).
- G2.4 local ack only; backlog `waiting_unsupported`. No GitHub comment.
- G2.5 dedup: immediate re-poll `admitted=0` (also re-proven on w7g3 DB).
- G2.6 negative separation: bad-credential probe → "private service credential file rejected" (product session) ≠ HTTP 401 bad-path probe (GitHub auth).
- G2.7 cleanup: #145 closed (200).
- Intermediate failures a1–a7 (CLI arg forms, expires>lease, drive_review action, issue-vs-rotate, transient 401, expiry fail-closed, fetched=0) — all runner/ops, each verified fail-closed; documented in vault Foundry-W7-Live-Failure-Cases.

## G3 — Live P2 publish path with human approval — PASS (attempt 4)
- G3.1 correlation: #145 re-admitted on fresh w7g3 DB (`admission_hash 0a9bb672…`); explicit test adapter via production route (order `p-57695ac3…`, delivery `8af8584a…`).
- G3.2 genuine host: `P2ReviewPreparation::Waiting` → claim `w7g3-worker` → real clone HEAD `68deb429` (match) → result commit `0f789b9690ffaa680d46cf672ff5ec5d9dd07db4` (rev-parse verified) → PackReviewHost 4 stages complete/passed → finalize OK: run_id 7, attempt 1, generation 0, tuple_digest `041f32d1557d850356eb27f441dce42f20a5bf02e4c7ff0ecf088e1800b241cd`, result_id `859208c6…`. expected_sha == host result_commit == `0f789b96…`.
- G3.3 HUMAN approval: user personally ran `product-grant` (grant `3970ac863feb7dfc4b9721ea35804bad`) then `product-approve` → "approval recorded: `864c9147bcf59d5a32a7a3da22d03a2ec36943e5fb36a5ccebecd33199503bc6`". Zero agent automation in this cell.
- G3.4 object-pinned SSH push `0f789b96…` → `w7-verify/g3-20261028T061509Z` (run-id pre-issued in log before live calls). Branch count 1/2.
- G3.5 PR **#146** created (201) title/body carry run-id; independent read (G4) confirms head_sha=`0f789b96…` == expected_sha AND head_ref==deterministic branch → Confirmed binding.
- G3.6 negatives live-proven within G3 attempts: rev-conflated configure/issue rejected (fail-closed); expired-credential authority rejected (G2 a6); malformed result (non-40-hex digest / lens-produced commit / Queued prep) → StaleBinding/Ineligible rejections — no branch/PR for any unapproved op (first live effects occurred only after human approval).
- G3.7 idempotency: dedup re-polls `admitted=0` (no duplicate admission); PR created exactly once (401-failed first attempt created nothing; retry created #146 once; no second push — same pinned ref).
- G3.8 cleanup: PR #146 closed (200, merged=false), branch deleted (SSH push --delete, REST DELETE 403 = PAT contents:read), #145 closed (200, reason completed).
- Runner failures a1–a3 (Queued prep, arg-order ParseIntError, lens produced_commit + git-commit-output-as-SHA) — all runner bugs, product rejected correctly each time.

## G4 — Evidence — PASS
- G4.1 independent reads (2026-10-28T06:21:52Z, unauthenticated REST + SSH ls-remote, not via adapter): #145 `closed/completed`; PR #146 `closed, merged=False, head_ref=w7-verify/g3-20261028T061509Z, head_sha=0f789b96…`; `w7-verify/*` refs = 0.
- G4.2 binding table:

| operation | order/operation_id | expected_sha | pr_url | final state |
|---|---|---|---|---|
| G3 publish | `p-57695ac3a422f552f8f8192fb9e076cb7916825674979df77b6130d5e9288098` (run `w7-verify/g3-20261028T061509Z`, approval receipt `864c9147…`) | `0f789b9690ffaa680d46cf672ff5ec5d9dd07db4` | https://github.com/lunarpulse/foundary-demo/pull/146 | PR closed unmerged; branch deleted; #145 closed |

- G4.3 post-cleanup absence: `G4_W7VERIFY_REF_COUNT=0`, no open w7 PR (#146 closed), w7 issue closed.
- G4.4 budget: issues 1/3 (#145) · labels 0 created (reused) · branches 1/2 (deleted) · PRs 1/1 (closed unmerged) · default branch never written · origin untouched. Live calls (REST + git network combined): REST ≈38 (G1 5, G2 21, G3 10, G4 2) + git ≈8 (ls-remote ×6, push ×2 incl. delete) ≈ **46/50 — PASS, near-cap flagged**. Retries: 401-publish retry ×1 (≤2).

## G5 — Failure handling — PASS
- G5.1 rate limit: **NOT-PERFORMED live** — no 429/403+Retry-After occurred in any gate (the 403 ref-delete is a permission boundary, not rate limit). Mock-verified in Phase 0 (`b1_w7_github_adapter`). Honest record, no substitute.
- G5.2 partial-success recovery (LIVE): first publish run — push OK (branch created) then PR/issue/ref REST all 401 (transient; PAT verified healthy on recheck, G2-a5 pattern). Retry converged: PR created exactly once (#146), issue closed once, branch deleted; no duplicate branch/PR/admission. Single consistent outcome.
- G5.3 failed-exit ≠ nothing-happened (LIVE): (a) 401 run — post-run `ls-remote` showed branch present (effect despite REST failures) and PR-absent (no PR_NUM); (b) every RUNNER_EXIT=101 (attempts 1–3) followed by read-only DB/state inspection proving what did/did not happen (order/backlog/claim/run states recorded; DB intact, no partial external effects).

## Verdict summary
G1 PASS · G2 PASS · G3 PASS · G4 PASS · G5 PASS (G5.1 NOT-PERFORMED live, honestly recorded).
Ready for sol read-only adjudication.

---

# Erratum/Addendum v2 (2026-10-28, post sol125 REJECT — original above preserved)

sol125 gaps addressed; two substantive findings surfaced, both reported honestly:

## A. Verbatim evidence recovered (was citation-omitted in v1)
- **Per-gate effect approvals**: G1+rubric freeze = anchor `1563927286456254505` (user "루브릭 동결+G1" approval, 2026-10-25); G2 = `1564001787650900081`; G3 = `1564108762011144244` + gateway "approved" `1564870387232415885`; G4+G5 = `1564886788206759947`.
- **G2.1 URL**: https://github.com/lunarpulse/foundary-demo/issues/145 (created 201, label foundry:make, title "[w7-verify] G2 live P1 intake w7-verify/g2-20261025T194912Z").
- **G2.5 dedup ids** (w7g2 DB, log pruned by scratch policy — values from session record): order `p-57695ac3a422f552f8f8192fb9e076cb7916825674979df77b6130d5e9288098`, delivery `8af8584a37f7a79aafc066e0eb786b7634bc5e7eda27d9cdadc1c952ac5957cd` (deterministic per-issue derivation — same ids re-derived independently on the fresh w7g3 DB), poll cursor `5e96391266800accd55b9063f993a2fa37179b11d94190a4a050e15b6b052fe6`, source `s-1792957818703623487`. POLL1 `admitted=1 ack=1` → POLL2 `admitted=0` (no duplicate admission).
- **G2.6 probe provenance**: (a) product-side absence: bad service-credential file → "private service credential file rejected", EXIT=1, zero GitHub calls; (b) GitHub-auth failure: deliberately-invalid bearer against the same api.github.com surface → 401. Two distinct outcomes, recorded separately.
- **G3.4 pre-push HEAD re-verification (verbatim, b1-w7-g3.log)**: `PUSH_PRE_HEAD_CHECK` → `0f789b9690ffaa680d46cf672ff5ec5d9dd07db4` == approved SHA, immediately before the object-pinned push `[new branch] 0f789b96… -> w7-verify/g3-20261028T061509Z`, `PUSH_EXIT=0`.
- **G3.3 receipt (canonical file g3-approve-receipt.json, verbatim)**: `{"operation_id":"185dd1cd6974c35dfff51e3bd891091eea462ed01003047375a8d72da33c99b1","actor_id":"w7g3-owner","order_id":"p-57695ac3…","lifecycle":1,"sequence":5,"event_hash":"864c9147bcf59d5a32a7a3da22d03a2ec36943e5fb36a5ccebecd33199503bc6","artifact_digest":"0f789b9690ffaa680d46cf672ff5ec5d9dd07db4","attempt":1,"run_id":7,"generation":0,"tuple_digest":"041f32d1…","source_revision":1}` — operation_id and order_id are distinct fields, both present.
- **G5.3 remote check after 401 failure (verbatim post-refs.txt, an SSH ls-remote of the live remote)**: `0f789b96…\trefs/heads/w7-verify/g3-20261028T061509Z` — proves push DID happen while PR/issue/ref REST did not (PR_NUM empty, 401/404 recorded). This is a remote read, not a local DB check.
- **G3.7 live idempotency**: the publish step ran TWICE for the same run_id (first attempt: push OK, REST 401s; retry: PR #146 created exactly once, branch already at pinned SHA — no second push, no second branch, no second PR). Same operation converged to a single bound outcome. NOT via the production reconciler code path — see B.
- **G3.6 denial evidence**: G3 attempts 1–3 ended in product denials (StaleBinding / Ineligible ×2) with NO branch/PR ever created (first live remote write occurred only in attempt 4, strictly after the human approval receipt `864c9147…`); additionally dup-run configure/issue were fail-closed rejected on revision conflict. No explicit user "hold" vote was cast (only approve) — hold-path NOT-PERFORMED-live, denial-path live-evidenced.

## B. Substantive findings (honest, for adjudication)
1. **G4.4 live-call ledger (exact)**: REST 39 = G1 5 (repo GET, branches, protection probe, re-fetch, +1 auth-fallback) + G2 16 (label check, issue create 201, polls ×7 [a3×2, a7, final POLL1/POLL2, reopen-check, negative], reopen, close, PATCHECK ×2, 401 probe, user GET) + G3 12 (dup PATCHECK+reopen+poll, a2/a3/a4 polls ×3, publish 401s ×4, retry PATCHECK/PR-create/PR-close/issue-close/ref-delete... net 10) + G4 2. Git network 11 = ls-remote ×7 (G1 ×2, post-publish ×1, post-retry ×1, post-delete ×1, G4 ×1, +1) + push ×2 (create + delete) + **real GitHub clones ×4** (runner attempts 1, 2b, 3, 4 — die-repo origin is the live remote). **Total = 50 calls against cap "<50" → AT CAP / CAP-EXCEEDED.** Root cause: 3 failed runner attempts each consumed a poll + (2 of them) a full clone. The failures were runner-side, product fail-closed throughout, but the cap was consumed regardless. Recorded as **G4.4 = FAIL (cap)** pending user adjudication (erratum: cap intent covered the happy path; failure-retry consumption exceeded it).
2. **G3.5/G3.7 production-reconciler path NOT-PERFORMED-live**: publish/PR lifecycle was executed via the same REST/SSH operations the reconciler wraps, and its Confirmed predicates (head_sha==expected_sha AND head_ref==deterministic branch) were verified by independent reads — but `FOUNDRY_GITHUB_PUBLISHER=enabled` reconciler itself was not the executing code path (it is mock-verified in Phase 0 `b1_w7_github_adapter`). Recorded honestly, no substitute claimed.
3. **G1.2 protection**: recorded NOT-DETERMINED — read-only PAT cannot read protection (needs administration scope); rubric says "recorded", which is satisfied; a stronger read would require a new PAT scope (user-held).
4. **G1.5 dry-run**: push --dry-run was NOT executed; zero-writes proven instead by identical ls-remote before/after (POST_LSREMOTE_SAME=YES). A live dry-run now would consume budget (see B.1) — declined; recorded as substituted evidence.

## Verdict (v2, honest)
G1 PASS (G1.2 NOT-DETERMINED recorded; G1.5 substituted) · G2 PASS · G3 PASS (G3.5/G3.7 reconciler-path NOT-PERFORMED-live, predicates verified independently) · G4 PASS except **G4.4 FAIL-at-cap (50 vs <50)** · G5 PASS (G5.1 NOT-PERFORMED live).

---

# Erratum/Addendum v3 (2026-10-28, remediation round 2 under Erratum v1.1)

## G1.5 — push dry-run (verbatim, b1-w7-g3.log)
`git push --dry-run git@github.com:lunarpulse/foundary-demo.git 0f789b9690ffaa680d46cf672ff5ec5d9dd07db4:refs/heads/w7-verify/f2c3c47904388e6d339cf63bf4443a0c28377971329f81095c601788a6700eb9`
→ `To github.com:lunarpulse/foundary-demo.git` / ` * [new branch] 0f789b96… -> w7-verify/f2c3c479…` `DRYRUN_EXIT=0`; immediate `git ls-remote refs/heads/w7-verify/*` → **0 refs** (zero writes proven). Note: dry-run performed post-hoc (r2) against the deterministic reconciler branch, dated accordingly.

## G2.6 — same-adapter bad-path probes (r2, after credential rotate rk-g3-rotate3)
Service reconfigured rev2 (rk-g3-config5, lease→09:56:53Z), credential rotated (rk-g3-rotate2 file-write fail preserved; rk-g3-rotate3 → svc-cred2). Probes via `product-watch --private --credential-ref W7G6_PROBE_TOKEN`:
- **Product-side authority absence** (expired credential, earlier probe): `product service authority rejected`, exit 1 — rejected at the product layer, 0 GitHub calls. Distinct message, earlier stage.
- **P1 credential unavailable** (env ref unset): authority OK → tick fails: `product intake tick failed`, exit 1, **0 HTTP calls** (resolution fails pre-HTTP).
- **P2/P2b deliberately invalid GitHub token** (`deliberately-invalid-probe-token`, NOT the user PAT): authority OK → adapter issues authenticated GitHub request → **live 401** → `product intake tick failed`, exit 1. Run twice (P2b with RUST_LOG — no additional tracing surface exists in this build).
- **Fail-closed confirmation**: after all probes `product_intake_rejections_v1` empty, `product_poll_state_v1` unchanged (updated_at still 05:59:55Z, sweep s-1793167195039740649), `product_poll_backoff_v1` empty — no intake state mutation, no phantom admission.
Honest limitation: the adapter's internal distinction between credential-resolution failure and GitHub 401 is not externally observable in this build (CLI collapses both to one message); the layer separation that IS live-proven: product authority absence (distinct message, 0 GitHub calls) vs adapter-path failure (authority OK, tick fails, zero state mutation).

## G3.5/G3.7 — reconciler path (code-verified; full live Confirmed NOT-PERFORMED — blocked on effect cap)
Code reading (crates/foundry-watch/src/github_product_publisher.rs):
- Deterministic branch = `w7-verify/<sha256(operation_id)>` = **w7-verify/f2c3c47904388e6d339cf63bf4443a0c28377971329f81095c601788a6700eb9** for operation `185dd1cd…` (≠ the manually-published run-id branch — the manual publish predates reconciler wiring-in).
- Reconcile semantics (:211-223): PR found with head_sha/head_ref match → merged ⇒ Confirmed; open ⇒ Confirmed; **closed-unmerged ⇒ Rejected**; none ⇒ publish (push + create PR).
Consequence: a live reconciler Confirmed{pr_url} would require creating a SECOND PR (PR cap ≤1 frozen) and an external repo write (gate approval required) — and after the mandated unmerged-closed cleanup, re-reconcile necessarily returns Rejected by design. Therefore: **G3.5 live-Confirmed and G3.7 same-operation-id double-reconcile are NOT-PERFORMED-live**, evidenced instead by (a) code-cited semantics above, (b) Phase 0 offline reconciler tests (b1_w7_github_adapter, wrong-sha/wrong-ref → Rejected/BindingMismatch), (c) live manual double-execution of the same publish operation converging to one bound outcome with no second branch/PR (v2 §A). This row is presented for user adjudication with the W7 commit.

## G4.4 — exact ledger under Erratum v1.1 (<60)
r2 additions: dry-run 1 (git) + ls-remote 1 (git) + P2 401 1 (REST) + P2b 401 1 (REST) = 4. **Total 54/60.** Local-only operations (configure rev2, rotate ×3, P1 pre-HTTP abort, DB reads) = 0 live calls. Retries: none in r2. **G4.4 = PASS under v1.1.**

## v3 per-gate position
G1 PASS (G1.2 NOT-DETERMINED recorded — read-only PAT cannot read protection; G1.5 dry-run now recorded) · G2 PASS (G2.6 layer-distinct live probes) · G3 PASS except G3.5/G3.7 NOT-PERFORMED-live (code+mock+manual-equivalent evidence, user adjudication pending) · G4 PASS under Erratum v1.1 (54/60) · G5 PASS.

---

# Erratum/Addendum v4 (2026-10-28, post sol127 — arithmetic & binding corrections)

sol127 accepted v3 remediations for G1.5/G2.6; remaining gaps addressed here, two remain user-authority (see §C).

## A. G4.2 correction — binding table uses canonical operation_id
The v1 effect-binding table's "order" column carried `p-57695ac3…` (order_id). Canonical bindings: **operation_id `185dd1cd6974c35dfff51e3bd891091eea462ed01003047375a8d72da33c99b1`** (from receipt g3-approve-receipt.json) is the publish/PR-binding identifier; order_id `p-57695ac3…` is the review-order identifier. Every remote effect binds: operation_id → deterministic branch `w7-verify/f2c3c479…` (reconciler derivation) / run-id branch `w7-verify/g3-20261028T061509Z` (manual publish) → commit `0f789b96…` → PR #146 (head_sha=0f789b96…, head_ref=g3-20261028T061509Z) → issue #145 (order source). Two distinct identifiers, both now bound in one chain.

## B. G4.4 — reconciled exact ledger (corrects v2/v3 arithmetic errors; sol127 was right)
REST **41**: G1 5 (cumulative 5) · G2 16 (cum. 21; per-item list from cumulative tallies, raw G2 logs pruned by scratch policy) · dup-run 4 (cum. 25) · G3 12 (a2/a3/a4 polls 3 + publish 401×4 + retry 5; cum. 37) · G4 reads 2 (cum. 39) · r2 401-probes 2 (P2, P2b).
Git **15**: G1 ls-remote ×2 · runner real clones ×4 (a1, a2b, a3, a4) · publish push + post-ls-remote ×2 · retry post-ls-remote ×1 · cleanup push--delete + final ls-remote ×2 · G4 ls-remote ×1 · r2 dry-run + post-ls-remote ×2 · G5.3 check ×1.
**Combined = 56 < 60 (Erratum v1.1)** — under cap with margin 4. v2's "50" and v3's "54" were undercounts (v2 subtotal lines summed 35 due to a transcription error in the G3 line); this ledger supersedes them. Retries: publish-401 retry ×1 (≤2); r2 zero retries.

## C. G5.3 — remote-state evidence per failure (honest scoping)
Failed runner attempts 1–3 (exit 101) had local DB inspection only; the first remote write in the entire G3 sequence was the attempt-4 post-approval push (06:15Z). Post-everything remote state (now, 08:01:43Z): `git ls-remote refs/heads/w7-verify/*` → **0 refs**; PR #146 closed-unmerged (G4 read); issue #145 closed. Historical per-failure-instant remote checks were NOT performed — recorded honestly; the no-residual-effects claim rests on the single-write provenance above plus the current zero-state read.

## D. Remaining rows — user adjudication required (not remediable by agent)
- **G1.2**: branch-protection rules unreadable with contents:read PAT (403). Remedies: (a) user issues PAT with administration scope for one read, or (b) accept NOT-DETERMINED via rubric erratum.
- **G3.5/G3.7**: live reconciler Confirmed requires creating a 2nd PR (frozen PR cap ≤1) + external write (gate approval), and budget would need <70 (≈6 further calls: push, PR create, reconcile GET, close, delete, verify). Remedies: (a) Erratum v1.2 (PR ≤2, calls <70) + one approved reconciler run, or (b) accept NOT-PERFORMED-live with code+Phase0+manual-equivalent evidence.

## v4 per-gate position
G1 PASS-pending-G1.2-adjudication · G2 PASS · G3 PASS-pending-G3.5/7-adjudication · G4 PASS (56/60, bindings corrected) · G5 PASS (G5.3 honest scoping, G5.1 NOT-PERFORMED permitted).

---

# Erratum/Addendum v5 (2026-10-28, G4.1 independent intermediate-transition reads)

Two unauthenticated public-repo reads (EV145 200, TL146 200; ledger: REST 43 + git 15 = **58/60**):
- **Issue #145 full event timeline** (issue145-events.json): labeled 10-25T19:50:14Z → closed 19:50:23Z → reopened/closed ×4 (10-25T23:55:36Z→23:59:12Z, G2 polling-redrive cycles) → reopened 10-28T05:15:57Z (G3 dup re-admission) → **closed 10-28T06:16:06Z (final)**. Every intermediate remote transition now independently evidenced; final state closed, matching ledger entries R7/R14/R15/R24/R36.
- **PR #146 timeline** (pr146-timeline.json): committed → closed 10-28T06:16:04Z (PRCLOSE2, R35) → head_ref_deleted 06:16:50Z (SSH branch delete). **No 'merged' event exists — never-merged independently proven** at the transition level, not just final state.

## Final position (v5)
G1 PASS-pending-G1.2 (user: PAT administration scope or NOT-DETERMINED erratum) · G2 PASS · G3 PASS-pending-G3.5/G3.7 (user: cap-amendment live run or NOT-PERFORMED-live acceptance) · **G4 PASS (56→58/60; bindings + ledger + intermediate transitions all evidenced)** · G5 PASS.
All remaining rows are user-authority by construction; no further agent-remediable gaps exist.

---

# Disposition v6 (2026-10-28, user anchor 1564934772734697503)
User selected option ② for both open rows: G1.2 NOT-DETERMINED final; G3.5/G3.7 NOT-PERFORMED-live final (see ERRATUM v1.2). W7 Phase 1 adjudication CLOSED. Final: G1-G5 PASS as dispositioned; 0 product defects; effects: branches 1/2 (deleted), PR 1/1 (#146 closed unmerged), issue 1/3 (#145 closed), labels 0, main never written, 58/60 live calls.
