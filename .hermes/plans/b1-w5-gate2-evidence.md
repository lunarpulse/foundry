# B1 W5 Gate2 evidence

Evidence for frozen rubric rows Y1–Y6. Source changes and measured results are
recorded here; no cursor policy or implementation is approved by this document.

## Changes

- `foundry-serve` export projection adds an explicit truncation boolean and
  label, selected-order history completeness and null byte-limit fields, and
  measured request verification duration. Existing `coverage`, row shape, and
  projected digest inputs remain unchanged.
- Export and audit routes time the ProductStore scoped snapshot (which performs
  the full S1 verification) plus response projection. The response states that
  a user's request is not whole-ledger operational verification and that
  service-wide outcomes are coupled to other users' response failures.
- Y4 uses the real durable intake drain path; the delivery is admitted and
  acknowledged, then remains `waiting_unsupported`. The work claim is
  Unsupported and its execution-attempt count remains zero.
- Y5 keeps the pre-existing audit cell's direct hold-acceptance/continuation
  baseline and measures the production HTTP hold route in a separately named
  cell. The route cell verifies unauthenticated denial and authenticated
  continuation. `AuditPublisher` uses an `example.invalid` URL and is a mock;
  no external call occurs.
- Cursor, response-budget, and verification-cost policy remains PROPOSAL ONLY.
  The measured interface is one page, limit 1–100, and no cursor/offset exists.
  The response includes a measured duration and the test asserts it is numeric,
  but the harness captured test output (not response values), so numeric sample
  durations were not retained as evidence and no duration-based budget is
  recommended.

## Rubric map

| Row | Evidence |
| --- | --- |
| Y1 | `b1_export_audit.rs`: complete and partial response assertions distinguish `selection_truncated` and label from legacy coverage. |
| Y2 | PASS after luna118 (ADD100): long-history genuine fixture — selected order accumulates 5 S1 history rows through the genuine host/hold/approval flow; the scoped export returns all 5 rows with `selected_order_history_complete=true`, `selected_order_history_byte_limit=None`, while `has_more=true` and the truncation label are reported for the additional authorized order (b1_export_audit.rs:235; rerun of the tightened predicate `--exact` 1 passed, `selected_order_history_rows=5`, `total_order_rows=5`, `selection_has_more=true`). |
| Y3 | PASS for response behavior — export and audit assert numeric measured duration and the verification boundary text. Exact numeric samples were not captured because the test harness suppressed the diagnostic prints. The cost-policy proposal remains open. |
| Y4 | `b1_intake_durability.rs::w5_y4_real_intake_consumer_acks_then_leaves_arbitrary_body_unsupported` — parent round11b intake lane 10/0. |
| Y5 | `b1_export_audit.rs::scoped_audit_connects_product_sources_and_preserves_event_time_scope` retains direct hold acceptance/continuation; `b1_export_audit.rs::w5_y5_production_http_hold_route_requires_auth_and_continues` is the separate HTTP acceptance cell. This is not real issue-to-PR integration. |
| Y6 | Remediation verification below: audit and intake targets pass; the full watch lane stops at eight loopback bind failures in `b1_github_polling` (0/8 there). The restored helper is independently exercised by nine existing intake tests with `TMPDIR` unset. |

## Commands and measurements

Earlier Gate2 commands used `--locked --offline`, `CARGO_BUILD_JOBS=1`,
`CARGO_INCREMENTAL=0`, and `TMPDIR=/home/cosmo/.hermes/cache/scratch`.

- Audit-enabled: `cargo test -p foundry-serve --features p2-test-support --test b1_export_audit -- --test-threads=1` — exit 0, 4 passed, 41.60 s.
- Intake-enabled: `cargo test -p foundry-watch --test b1_intake_durability -- --test-threads=1` — exit 0, 10 passed, 0.63 s.
- Line regressions: `cargo test -p foundry-line --features p2-test-support --lib --tests -- --test-threads=1` — exit 0, 166 passed across 10 suites. Ledger child executable was prebuilt from Cargo JSON and passed as `B1_LEDGER_TEST_EXE`.
- Serve regressions: `cargo test -p foundry-serve --features p2-test-support --lib --tests -- --test-threads=1` — exit 0, 86 passed across 14 suites. One existing artifact test printed `mknod: Operation not permitted` but passed.
- Full watch regression: `cargo test -p foundry-watch --lib --tests -- --test-threads=1` — exit 101. 176 tests passed before `b1_github_polling` failed 8 loopback fake-server cases at `TcpListener::bind` with `PermissionDenied: Operation not permitted`. Per the stop rule, later regression lanes were not run.
- Initial refreshed audit build — exit 101 due to the local mutable-binding error introduced by the route refactor; corrected, then the audit-enabled lane passed. The final history-completeness predicate was tightened after that successful run and was not rerun due to the stop rule.

Test output timing includes lane-level elapsed values above. Exact per-response
`verification_duration_micros` values were not retained; they were only asserted
numeric inside the response. No commit, push, or external call occurred.

## Post-luna118 and parent verification (2026-10-24)

- luna118 (ADD100) closed the Y2 gap: long-history genuine fixture (5 S1 rows via
  the real host/hold/approval flow, no seeding), completeness assertions added at
  `b1_export_audit.rs:235`; tightened `selected_order_history_complete` predicate
  rerun `--exact --nocapture --test-threads=1` — 1 passed. Two test-only 101 runs
  during construction (invalid self-scope grant; 8-row expectation vs measured 5)
  were corrected to match the genuine path; no production code changed.
- Parent round11 (first 8 lanes green; intake lane initially mis-scripted against
  foundry-line — the target `b1_intake_durability` lives in foundry-watch) plus
  round11b (intake corrected, ledger, core, watch, cli-default) = 13 lanes
  ALL_GREEN, per-lane counts in the Y6 row above. Watch full suite green in the
  parent environment (219/0), resolving the luna sandbox loopback denials.
- Final delta vs `20530c7`: 5 files, +277/−19 at that checkpoint (export_http.rs
  +18/−1, privacy.rs +23, b1_export_audit.rs +182,
  b1_intake_durability.rs +36, b1-product-contract.md +37). At the Y6
  remediation checkpoint, the shared intake helper's earlier TMPDIR-only change
  was restored to `std::env::temp_dir()` behavior (no required `TMPDIR`, no
  `/tmp` rejection); the original audit test's direct hold-acceptance and
  continuation sequence was restored, and its HTTP-route coverage was kept in
  a new separately named cell. The existing test bytes were therefore changed
  during Gate2 and then corrected as described; do not characterize the suite
  as having remained unmodified.

## ADDENDUM 101 remediation verification (2026-10-25)

- Audit-enabled command, locked/offline: `cargo test -p foundry-serve
  --features p2-test-support --test b1_export_audit -- --test-threads=1` —
  exit 0, 5 passed, including the restored direct baseline and the separate
  `w5_y5_production_http_hold_route_requires_auth_and_continues` cell. The first
  build attempt exited 101 on a local missing `mut`; it was corrected before
  the passing rerun.
- Intake command, locked/offline: `cargo test -p foundry-watch --test
  b1_intake_durability -- --test-threads=1` — exit 0, 10 passed.
- No-`TMPDIR` helper regression: `env -u TMPDIR cargo test -p foundry-watch
  --test b1_intake_durability -- --skip
  w5_y4_real_intake_consumer_acks_then_leaves_arbitrary_body_unsupported
  --test-threads=1` — exit 0, 9 passed, 1 W5-only cell filtered. This measures
  all nine pre-existing intake tests against the restored shared helper.
- Full watch command, locked/offline: `cargo test -p foundry-watch --lib --tests
  -- --test-threads=1` — exit 101 at `b1_github_polling`: eight existing
  loopback fake-server tests failed at `TcpListener::bind` with
  `PermissionDenied: Operation not permitted` (0 passed, 8 failed in that
  target). Per stop rule, later watch targets were not reached. This environment
  failure is unrelated to ADDENDUM 101; no networking tests were changed or
  skipped.
- All four commands used `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, and
  `--locked --offline`; the first three used the configured scratch `TMPDIR`,
  while the helper regression explicitly removed it. No commit, push, or
  external calls occurred.

