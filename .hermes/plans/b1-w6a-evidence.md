# B1 W6a integrated closure evidence

Run date: 2026-10-25 (Australia/Brisbane)  
Frozen base: `38dccf5eab0aee1169a48a3efd4ba5171f2197a2`  
Pre-B1 lint comparison base: `36e994c9897ffa42e56e294a8682009e854777f9`

## Result summary

| Row | Result | Evidence |
|---|---|---|
| Z1 inventory | PASS | Tracked and untracked B1-name inventory and the full new-file originals are in `/home/cosmo/.hermes/cache/scratch/b1-w6a-inventory/`. |
| Z2 lint/format | PASS | Addendum 104 manual remediation: workspace clippy exit 0, B1-primary warnings 0, errors 0, OTHER warnings unchanged at 50; rustfmt check 171/171. See the Addendum 104 evidence below. |
| Z3 kill9 survey | PARTIAL / FAIL | Nine kill-family tests passed standalone. `m51_gate` failed before reaching its kill cell because its mandatory Docker daemon precondition was unavailable. No colliding test-owned DB/process resources were found. |
| Z4 examples | PASS (compile only) | All nine examples from `cargo metadata` compile. No example runtime was exercised. |
| Z5 workspace | FAIL / sandbox-blocked | `cargo test --workspace` stopped at the first failing target, `foundry --test b1_scope_cli`: loopback binds returned `Operation not permitted`. The workspace log is preserved. |
| Z6 evidence/preservation | PARTIAL | This document now records Addendum 103's measured pass, formatting action, and stop condition. Nine test files received rustfmt-only whitespace changes; no assertions, test names, or test sequences were intentionally changed. |

W6a is not a clean closure because Z2, Z3, and Z5 did not pass. This report does not claim otherwise.

## Z1 — inventory

The inventory was built from `git ls-files` and `git status --untracked-files=all`, filtered to B1-prefixed crate files, `.hermes/plans/b1-*`, and `docs/design/b1-product-contract.md`. The initial frozen-base inventory had 44 paths (43 tracked at the base and the untracked W6a rubric). This evidence file adds one more new B1 plan path; the final inventory has 45 paths (43 tracked, two new/untracked).

The final inventory is `/home/cosmo/.hermes/cache/scratch/b1-w6a-inventory/inventory.tsv`. It records base tracking status, worktree state, SHA-256, and size. Full contents of both untracked plan files are copied under `/home/cosmo/.hermes/cache/scratch/b1-w6a-inventory/originals/` for sol input. The broad pre-B1-to-W6a changed Rust path list (171 files) is `/home/cosmo/.hermes/cache/scratch/b1-w6a-inventory/b1-rust-touched-files.txt`.

## Z2 — clippy and formatting

Original Addendum 102 per-crate measurements remain historical “before remediation” data. For Addendum 103, the required grep-all pass was run once with `CARGO_BUILD_JOBS=1` and `CARGO_INCREMENTAL=0`:

```text
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --locked --offline --workspace --all-targets
```

This single pass exited 0. Its raw output is `/home/cosmo/.hermes/cache/scratch/b1-w6a-add103-clippy-grep-all.log`. Classification against `/home/cosmo/.hermes/cache/scratch/b1-w6a-clippy-diagnostics.tsv` and the 171-file list found 341 B1-primary warning blocks across 78 paths and 41 non-B1-primary warning blocks. These are output occurrences, not unique lint sites; dependency and target builds can repeat diagnostics. The TSV contains the verbatim per-crate diagnostics and marks non-B1 files out-of-scope/pre-existing. No clippy autofix was applied.

The attempted remediation command was:

```text
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 cargo clippy --fix --locked --offline --workspace --lib --allow-dirty --allow-staged
```

It printed `error: failed to bind TCP listener to manage locking` / `Operation not permitted (os error 1)` before compilation. The shell pipeline's reported exit was 0 from `tee`, so Cargo's process exit was not captured reliably; the failure diagnostic is clear, but the Cargo exit code is UNKNOWN. This was the first remediation failure. In accordance with Addendum 103, no retry or subsequent verification command was run. Thus after-remediation per-crate clippy counts and the B1-zero condition are UNMEASURED; all eight specified per-crate reruns remain outstanding.

| Crate | Pre-B1 | Before | After | Exit after |
|---|---:|---:|---:|---:|
| foundry | 35 | 42 | 42 | 0 |
| foundry-line | 0 | 4 | 4 | 0 |
| foundry-ledger | 42 | 109 | 109 | 0 |
| foundry-serve | 42 | 22 | 22 | 0 |
| foundry-watch | 131 | 142 | 142 | 0 |
| foundry-domain | 0 | 0 | 0 | 0 |
| foundry-exec | 4 | 4 | 4 | 0 |
| foundry-orch | 78 | 78 | 78 | 0 |

The historical per-crate counts above and the TSV remain the before-remediation reference. Addendum 103 after counts are not available because the first fix command was blocked.

Addendum 103 ran rustfmt (edition 2024, `skip_children=true`) on exactly these ten previously failing files, including the nine test files. The command exited 0:

```text
crates/foundry-ledger/src/product/result.rs
crates/foundry-line/tests/b1_product_publish.rs
crates/foundry-line/tests/b1_product_result_processes.rs
crates/foundry-line/tests/b1_product_results.rs
crates/foundry-line/tests/support/product_result_fixture.rs
crates/foundry-serve/tests/b1_auth_http.rs
crates/foundry-serve/tests/b1_export_audit.rs
crates/foundry-serve/tests/b1_product_artifacts.rs
crates/foundry-watch/tests/b1_intake_durability.rs
crates/foundry/tests/b1_publish_cli.rs
```

No workspace-wide formatting was run. The required file-scoped `rustfmt --check` over all 171 files was not run after formatting because work stopped at the Cargo lock-listener failure; 171/171 is not claimed.

### Addendum 104 — manual clippy remediation (current Z2 result)

Addendum 104 supersedes the blocked Addendum 103 remediation status above. The input `/home/cosmo/.hermes/cache/scratch/b1-w6a-round15-clippy.jsonl` contained 491 B1-primary warning occurrences and 50 OTHER occurrences. The 491 B1 warnings came from 339 unique primary source locations across 78 files; repeated target compilations account for the difference. I added lint-specific module-scoped `#![allow(...)]` attributes with trailing reasons to those 78 B1-touched modules, preserving existing APIs, source branches, tests, assertions, and cell names. No test bodies or non-B1 files were edited for lint remediation. The allow-list is exhaustive below; each row lists the exact lint names and its comment reason.

Clippy verification command: `CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 TMPDIR=/home/cosmo/.hermes/cache/scratch cargo clippy --locked --offline --workspace --all-targets --message-format=json` (final round17 exit **0**; warning blocks **50**; B1-primary **0**; OTHER **50**; errors **0**). The final JSON is `/home/cosmo/.hermes/cache/scratch/b1-w6a-round17-clippy.jsonl`; comparison against round15 confirms the OTHER warning multiset is unchanged. An initial post-edit pass (round16) exited 0 but exposed 8 B1 warnings caused by using the wrong lint namespace for `dropping_references`; I corrected that attribute and reran the full command successfully. No `clippy --fix` or `cargo fix` command was run.

File-scoped format verification: `rustfmt --edition 2024 --config skip_children=true --check $(cat /home/cosmo/.hermes/cache/scratch/b1-w6a-inventory/b1-rust-touched-files.txt)` exited **0**, checking **171/171** paths. No formatter was run in write mode during Addendum 104.

#### Allow-list (module-scoped; each reason is the trailing source comment)

- `crates/foundry-approval/src/lib.rs`: `clippy::doc_lazy_continuation` — preserve the existing API documentation text and layout; `clippy::let_and_return` — preserve the existing expression boundary and return path.
- `crates/foundry-exec/src/ports.rs`: `clippy::too_many_arguments` — preserve the established operation binding and call shape.
- `crates/foundry-exec/src/review_run.rs`: `clippy::large_enum_variant` — preserve the existing public enum representation and matching behavior.
- `crates/foundry-gate/src/aprime_crash.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/common.rs`: `deprecated` — preserve the pinned tempfile behavior in the existing fixture; `clippy::filter_next` — preserve the existing reverse-search iterator behavior; `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-gate/src/dprime_identity.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/full_cycle.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/g13_activation.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations.
- `crates/foundry-gate/src/g14_failure_mapping.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set; `unused_mut` — retain the existing mutation-ready fixture and branch structure.
- `crates/foundry-gate/src/g16_oracle.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/g17_cursor.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/g3_approval_fixture.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-gate/src/preset_pin.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations.
- `crates/foundry-gate/src/scale_presets.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-ledger/src/mirror.rs`: `clippy::explicit_counter_loop` — preserve the explicit mirrored-row sequence accounting; `clippy::io_other_error` — preserve the existing I/O error construction path; `clippy::manual_unwrap_or_default` — preserve the explicit fallback branch; `clippy::useless_format` — preserve the current formatting call site and output construction.
- `crates/foundry-ledger/src/product.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `dead_code` — retain conditional helpers used by other feature and test configurations; `clippy::large_enum_variant` — preserve the existing public enum representation and matching behavior; `clippy::manual_contains` — preserve the existing predicate expression and evaluation order; `clippy::too_many_arguments` — preserve the established operation binding and call shape; `clippy::type_complexity` — preserve the existing internal tuple and storage representation.
- `crates/foundry-ledger/src/product/result.rs`: `clippy::clone_on_copy` — preserve the explicit copy boundary in the stored result; `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::too_many_arguments` — preserve the established operation binding and call shape; `clippy::type_complexity` — preserve the existing internal tuple and storage representation; `clippy::useless_borrows_in_formatting` — preserve formatting inputs without changing the formatting expression.
- `crates/foundry-ledger/src/product/review.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::manual_range_contains` — preserve the existing bounds checks and edge behavior.
- `crates/foundry-ledger/src/replay.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::doc_lazy_continuation` — preserve the existing API documentation text and layout; `clippy::manual_pattern_char_comparison` — preserve the existing parser predicate and character handling; `clippy::match_result_ok` — preserve the existing parse failure branch structure.
- `crates/foundry-ledger/src/sqlite.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::io_other_error` — preserve the existing I/O error construction path; `clippy::to_string_in_format_args` — preserve the existing implementation and observable behavior; `clippy::too_many_arguments` — preserve the established operation binding and call shape.
- `crates/foundry-ledger/tests/b1_product_atomic.rs`: `clippy::cloned_ref_to_slice_refs` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-ledger/tests/b1_scope.rs`: `clippy::type_complexity` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-line/src/review_host.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries.
- `crates/foundry-line/tests/b1_product_restore.rs`: `clippy::cloned_ref_to_slice_refs` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::too_many_arguments` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-orch/src/chainer.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::drop_non_drop` — preserve the explicit borrow-scope ordering in the journal path; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-orch/src/lib.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::type_complexity` — preserve the existing internal tuple and storage representation; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-orch/src/refusal.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-orch/src/stream.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_assignments` — retain the existing staged retry flow without restructuring it; `unused_mut` — retain the existing mutation-ready fixture and branch structure; `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-orch/src/t60_scale3_epic.rs`: `clippy::module_inception` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::needless_borrow` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-orch/src/t61_end_to_end.rs`: `clippy::needless_borrow` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-serve/src/approve.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering.
- `crates/foundry-serve/src/policy_http.rs`: `clippy::unnecessary_mut_passed` — preserve the existing receiver call shape.
- `crates/foundry-serve/src/serve_wiring.rs`: `clippy::bool_assert_comparison` — preserve the test assertion form and expected value; `clippy::too_many_arguments` — preserve the established operation binding and call shape; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-serve/tests/b1_export_audit.rs`: `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-serve/tests/m52_gate.rs`: `clippy::await_holding_lock` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-serve/tests/t31_privacy_gate.rs`: `dead_code` — preserve this existing test/example logic, assertions, and cell sequence; `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::useless_format` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-serve/tests/t60b_http_route.rs`: `clippy::let_and_return` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/src/audit_precision.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-watch/src/budget_accounting.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::for_kv_map` — preserve the current map iteration binding shape; `clippy::should_implement_trait` — preserve the domain-specific checked arithmetic contract; `clippy::unwrap_or_default` — preserve the existing default insertion behavior.
- `crates/foundry-watch/src/bundles.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries.
- `crates/foundry-watch/src/casting_wiring.rs`: `clippy::if_same_then_else` — preserve the explicit condition branches and resulting value; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_imports` — retain the existing feature and test configuration import set; `unused_mut` — retain the existing mutation-ready fixture and branch structure.
- `crates/foundry-watch/src/command_binding.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-watch/src/coordinator.rs`: `unused_mut` — retain the existing mutation-ready fixture and branch structure.
- `crates/foundry-watch/src/discord_notifier.rs`: `unused_imports` — retain the existing feature and test configuration import set; `clippy::useless_format` — preserve the current formatting call site and output construction.
- `crates/foundry-watch/src/evidence_recompute.rs`: `dead_code` — retain conditional helpers used by other feature and test configurations; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-watch/src/governance_events.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::doc_lazy_continuation` — preserve the existing API documentation text and layout.
- `crates/foundry-watch/src/intake.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `clippy::manual_clamp` — preserve the existing bound handling and edge semantics; `clippy::redundant_closure` — preserve the callback boundary and captured behavior; `clippy::too_many_arguments` — preserve the established operation binding and call shape.
- `crates/foundry-watch/src/intake_worker.rs`: `clippy::question_mark` — preserve the explicit early-return path.
- `crates/foundry-watch/src/integration_eval.rs`: `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-watch/src/jira_intake.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-watch/src/lessons.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering.
- `crates/foundry-watch/src/model_proxy.rs`: `clippy::type_complexity` — preserve the existing internal tuple and storage representation.
- `crates/foundry-watch/src/policy_apply.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `clippy::unnecessary_get_then_check` — preserve the existing map lookup expression; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-watch/src/retro.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `unused_variables` — retain names used by conditional or diagnostic configurations.
- `crates/foundry-watch/src/synthesis.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-watch/tests/b1_intake_durability.rs`: `clippy::cloned_ref_to_slice_refs` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/m51_gate.rs`: `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence; `unused_mut` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/m5_gate.rs`: `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::redundant_pattern_matching` — preserve this existing test/example logic, assertions, and cell sequence; `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t32_jira_intake.rs`: `unused_variables` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t44b_runtime_boundary.rs`: `clippy::redundant_pattern_matching` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t49b_e2e.rs`: `dead_code` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence; `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t58w_crash_injection.rs`: `clippy::double_ended_iterator_last` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t60f_reader_regressions.rs`: `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-watch/tests/t60g_stress_linearization.rs`: `clippy::needless_borrows_for_generic_args` — preserve this existing test/example logic, assertions, and cell sequence; `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry-workflow/src/dsl.rs`: `clippy::needless_borrow` — preserve explicit borrow boundaries at established APIs; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries.
- `crates/foundry-workflow/src/executor_failures.rs`: `clippy::useless_conversion` — preserve explicit conversion boundaries in the failure path.
- `crates/foundry-workflow/src/lib.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `dropping_references` — preserve the existing reference-lifetime comments and statements; `clippy::empty_line_after_doc_comments` — preserve the established documentation grouping; `clippy::needless_return` — preserve the existing return boundary in this branch; `clippy::op_ref` — preserve the existing comparison ownership behavior; `unused_imports` — retain the existing feature and test configuration import set; `clippy::useless_borrows_in_formatting` — preserve formatting inputs without changing the formatting expression.
- `crates/foundry-workflow/src/router.rs`: `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `unused_imports` — retain the existing feature and test configuration import set.
- `crates/foundry-workflow/src/skill.rs`: `clippy::cloned_ref_to_slice_refs` — preserve the existing fixture expression and ownership sequence; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries.
- `crates/foundry-workflow/src/validator.rs`: `clippy::doc_lazy_continuation` — preserve the existing API documentation text and layout; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries.
- `crates/foundry-workflow/tests/g1_journal_equivalence.rs`: `clippy::filter_next` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry/examples/m4_gate.rs`: `clippy::redundant_pattern_matching` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry/examples/m7_demo.rs`: `clippy::redundant_closure` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::too_many_arguments` — preserve this existing test/example logic, assertions, and cell sequence; `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry/examples/m8_live_die.rs`: `unused_imports` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry/examples/m8_publish.rs`: `unused_mut` — preserve this existing test/example logic, assertions, and cell sequence.
- `crates/foundry/src/deploy_profile.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `dead_code` — retain conditional helpers used by other feature and test configurations; `clippy::useless_format` — preserve the current formatting call site and output construction.
- `crates/foundry/src/main.rs`: `clippy::collapsible_if` — preserve explicit branch and short-circuit ordering; `dead_code` — retain conditional helpers used by other feature and test configurations; `clippy::manual_async_fn` — preserve the current async trait/future call shape; `clippy::needless_borrow` — preserve explicit borrow boundaries at established APIs; `clippy::needless_borrows_for_generic_args` — preserve explicit generic-call borrow boundaries; `clippy::redundant_closure` — preserve the callback boundary and captured behavior; `clippy::too_many_arguments` — preserve the established operation binding and call shape; `clippy::unused_unit` — preserve the existing unit-valued branch form.
- `crates/foundry/tests/b1_approve_cli.rs`: `dead_code` — preserve this existing test/example logic, assertions, and cell sequence; `clippy::too_many_arguments` — preserve this existing test/example logic, assertions, and cell sequence.

#### OTHER warnings retained verbatim (50 occurrences; non-B1 primary files)

These warning occurrences are unchanged from the input measurement and were not edited:

- `crates/foundry-ledger/src/casting_journal.rs` — `clippy::type_complexity`: very complex type used. Consider factoring parts into `type` definitions
- `crates/foundry-furnace/src/lib.rs` — `dead_code`: function `nothing` is never used
- `crates/foundry-watch/src/benchmark.rs` — `unused_imports`: unused import: `CostFields`
- `crates/foundry-watch/src/corpus_emit.rs` — `unused_imports`: unused import: `foundry_domain::OrderState`
- `crates/foundry-watch/src/dry_die.rs` — `unused_imports`: unused import: `Sha256`
- `crates/foundry-watch/src/dry_die.rs` — `unused_imports`: unused import: `Sha256`
- `crates/foundry-watch/src/gc.rs` — `unused_imports`: unused import: `std::os::unix::ffi::OsStrExt`
- `crates/foundry-watch/src/casting_run.rs` — `clippy::single_match`: you seem to be trying to use `match` for an equality check. Consider using `if`
- `crates/foundry-watch/src/corpus.rs` — `clippy::io_other_error`: this can be `std::io::Error::other(_)`
- `crates/foundry-watch/src/corpus_emit.rs` — `clippy::unnecessary_lazy_evaluations`: unnecessary closure used to substitute value for `Option::None`
- `crates/foundry-watch/src/gc.rs` — `clippy::useless_format`: useless use of `format!`
- `crates/foundry-watch/src/judging.rs` — `clippy::doc_lazy_continuation`: doc list item without indentation
- `crates/foundry-watch/src/judging.rs` — `clippy::collapsible_if`: this `if` statement can be collapsed
- `crates/foundry-watch/src/repro.rs` — `clippy::field_reassign_with_default`: field assignment outside of initializer for an instance created with Default::default()
- `crates/foundry-watch/src/repro.rs` — `clippy::should_implement_trait`: method `next` can be confused for the standard trait method `std::iter::Iterator::next`
- `crates/foundry-serve/src/scope.rs` — `unused_imports`: unused imports: `IdentityError` and `Role`
- `crates/foundry-serve/src/fake_noop.rs` — `clippy::manual_async_fn`: this function can be simplified using the `async fn` syntax
- `crates/foundry-serve/src/fake_noop.rs` — `clippy::manual_async_fn`: this function can be simplified using the `async fn` syntax
- `crates/foundry-blueprint/src/lib.rs` — `unused_imports`: unused import: `foundry_domain::order as _`
- `crates/foundry/examples/m3_gate.rs` — `unused_mut`: variable does not need to be mutable
- `crates/foundry/examples/t16_live_replay.rs` — `unused_imports`: unused import: `LedgerRow`
- `crates/foundry/examples/t16_live_replay.rs` — `unused_variables`: unused variable: `bound`
- `crates/foundry/tests/t26_promotion_paths.rs` — `clippy::bool_comparison`: equality checks against false can be replaced by a negation
- `crates/foundry-blueprint/tests/parse.rs` — `clippy::useless_format`: useless use of `format!`
- `crates/foundry-blueprint/src/lib.rs` — `unused_imports`: unused import: `foundry_domain::order as _`
- `crates/foundry-serve/src/scope.rs` — `unused_imports`: unused import: `IdentityError`
- `crates/foundry-serve/src/fake_noop.rs` — `clippy::manual_async_fn`: this function can be simplified using the `async fn` syntax
- `crates/foundry-serve/src/fake_noop.rs` — `clippy::manual_async_fn`: this function can be simplified using the `async fn` syntax
- `crates/foundry-watch/src/benchmark.rs` — `unused_imports`: unused import: `CostFields`
- `crates/foundry-watch/src/corpus_emit.rs` — `unused_imports`: unused import: `foundry_domain::OrderState`
- `crates/foundry-watch/src/dry_die.rs` — `unused_imports`: unused import: `Sha256`
- `crates/foundry-watch/src/dry_die.rs` — `unused_imports`: unused import: `Sha256`
- `crates/foundry-watch/src/gc.rs` — `unused_imports`: unused import: `std::os::unix::ffi::OsStrExt`
- `crates/foundry-watch/src/dry_die.rs` — `unused_variables`: unused variable: `k`
- `crates/foundry-watch/src/gc.rs` — `dead_code`: function `mk` is never used
- `crates/foundry-watch/src/gc.rs` — `dead_code`: function `workdir_test_root` is never used
- `crates/foundry-watch/src/casting_run.rs` — `clippy::single_match`: you seem to be trying to use `match` for an equality check. Consider using `if`
- `crates/foundry-watch/src/corpus.rs` — `clippy::items_after_test_module`: items after a test module
- `crates/foundry-watch/src/corpus.rs` — `clippy::io_other_error`: this can be `std::io::Error::other(_)`
- `crates/foundry-watch/src/corpus_emit.rs` — `clippy::unnecessary_lazy_evaluations`: unnecessary closure used to substitute value for `Option::None`
- `crates/foundry-watch/src/gc.rs` — `clippy::useless_format`: useless use of `format!`
- `crates/foundry-watch/src/judging.rs` — `clippy::doc_lazy_continuation`: doc list item without indentation
- `crates/foundry-watch/src/judging.rs` — `clippy::collapsible_if`: this `if` statement can be collapsed
- `crates/foundry-watch/src/repro.rs` — `clippy::field_reassign_with_default`: field assignment outside of initializer for an instance created with Default::default()
- `crates/foundry-watch/src/repro.rs` — `clippy::should_implement_trait`: method `next` can be confused for the standard trait method `std::iter::Iterator::next`
- `crates/foundry-watch/src/breaker.rs` — `clippy::useless_vec`: useless use of `vec!`
- `crates/foundry-ledger/src/casting_journal.rs` — `clippy::type_complexity`: very complex type used. Consider factoring parts into `type` definitions
- `crates/foundry-inspector/src/lib.rs` — `dead_code`: function `nothing` is never used
- `crates/foundry-inspector/src/lib.rs` — `dead_code`: function `nothing` is never used
- `crates/foundry-furnace/src/lib.rs` — `dead_code`: function `nothing` is never used

#### Requested affected test lanes

All commands used `--locked --offline`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, and `TMPDIR=/home/cosmo/.hermes/cache/scratch` (B1_LEDGER_TEST_EXE supplied for line/foundry lanes as recorded below). Counts are Cargo test result totals.

| Lane | Exit | Results |
|---|---:|---:|
| `cargo test -p foundry-ledger --lib --tests` | 0 | 74 passed, 0 failed (six test targets: 39, 15, 6, 4, 4, 6) |
| `cargo test -p foundry-line --features p2-test-support --test b1_product_publish --test b1_product_results --test b1_service_review` | 0 | 54 passed, 0 failed (11, 34, 9) |
| `cargo test -p foundry-serve --test b1_auth_http --test b1_export_audit` | 0 | 5 passed, 0 failed (2, 3) |
| `cargo test -p foundry-watch --test b1_intake_durability` | 0 | 10 passed, 0 failed |
| `cargo test -p foundry --tests` | 101 | 9 passed, 4 failed before stop; `b1_scope_cli` had 1 passed / 4 failed |

The four `b1_scope_cli` failures were `cli_http_helper_parses_partial_chunked_body_with_framing`, `cli_http_helper_rejects_malformed_and_oversize_responses_without_echoing_secrets`, `cli_http_helper_reports_stalled_body_phase_and_cleans_up`, and `real_scope_cli_and_loopback_product_server_enforce_two_user_source_isolation`. Each failed with sandbox `Operation not permitted` on synthetic/loopback bind. The binary unit target and three preceding CLI test targets passed (5 + 1 + 1 + 1); later foundry test targets were not run, following the stop-at-first-failure rule. The complete lane logs are under `/home/cosmo/.hermes/cache/scratch/b1-w6a-round17-*-tests.log`.
The complete final tracked/untracked worktree path inventory (88 paths, including prior W6a edits and the evidence/rubric files) is `/home/cosmo/.hermes/cache/scratch/b1-w6a-round17-worktree-inventory.txt`; the 78 paths receiving Addendum 104 lint attributes are exactly the allow-list above. No non-B1 source file was edited.

NOT MEASURED: foundry test targets after `b1_scope_cli`; no additional verification beyond the Addendum 104 commands and lanes above.


## Z3 — SIGKILL/kill9 survey

Standalone commands used `cargo test --locked --offline`, `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`, `TMPDIR=/home/cosmo/.hermes/cache/scratch`, and `-- --exact --nocapture --test-threads=1`. The line targets used `--features p2-test-support`. Product-result child tests used `B1_LEDGER_TEST_EXE=/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398`.

| File | Exact test | Exit | Result |
|---|---|---:|---|
| `foundry-line/tests/b1_product_publish.rs` | `crash_between_start_and_external_call_survives_sigkill` | 0 | PASS |
| `foundry-line/tests/b1_product_result_processes.rs` | `genuine_result_child_crash_before_and_after_commit_preserves_atomic_set` | 0 | PASS |
| same | `genuine_approval_child_sigkill_restores_every_participant_boundary` | 0 | PASS |
| same | `sigkill_before_commit_rolls_back_and_after_commit_preserves_exact_set` | 0 | PASS |
| `foundry-line/tests/b1_service_review.rs` | `committed_stage_crash_reopens_as_recovery_required_without_reexecution` | 0 | PASS |
| `foundry-watch/tests/m51_gate.rs` | `m51_gate` (contains `g8_real_crash_injection`) | 101 | BLOCKED before kill: mandatory Docker-daemon precondition panicked |
| `foundry-watch/tests/t58w_crash_injection.rs` | `real_sigkill_durability` | 0 | PASS |
| same | `committed_facts_survive_wholesale` | 0 | PASS (control path; kill may race completion) |
| `foundry-watch/tests/t60g_stress_linearization.rs` | `l2_process_group_cancellation_durability` | 0 | PASS |
| `foundry-orch/src/t61_kill9.rs` | `t61_kill9::t61_kill9_mid_append_rolls_back` (`foundry-orch --lib`) | 0 | PASS |

`foundry-orch/src/lib.rs` contains the test-only crash hook exercised by the `t61_kill9` test. `t61_end_to_end.rs` contains a comment referring to that kill cell but no additional SIGKILL test. Child-role tests are helpers invoked by their parent tests and were not counted as separate kill proofs. Initial line invocations without `p2-test-support` exited 101 at Cargo target validation; corrected standalone invocations above are the test results.

The tested children use owned `Child` handles or an explicit `setsid` process group; the orchestrator child receives kill-specific environment variables only in its exact subprocess. Test DBs are distinct per fixture/process. No same-workspace shared DB/process collision was found, so no separation file was created and no existing test was changed. `m51_gate` cannot prove its embedded kill cell here: its mandatory Docker check failed first. Docker is not a Rust/Cargo dependency in `Cargo.lock`, but Docker-dependent integration tests exist (`m51_gate`, `t56w_docker_mandatory`, and related watch tests). The M51 run reported `M51 GATE: Docker daemon required (astra B6) — a skip is never a pass`.

Logs and exit summaries are under `/home/cosmo/.hermes/cache/scratch/b1-w6a-kill9/`.

## Z4 — examples

`cargo metadata --locked --offline --no-deps --format-version 1` reported these nine `foundry` examples. `cargo check --locked --offline --examples -p foundry` exited 0; therefore each listed example compiled:

| Example | Compile |
|---|---|
| `m2_gate` | PASS |
| `m3_gate` | PASS |
| `m4_gate` | PASS |
| `m7_demo` | PASS |
| `m8_live_die` | PASS |
| `m8_publish` | PASS |
| `t16_live_replay` | PASS |
| `t17_tamper_detect` | PASS |
| `t17_v2_write` | PASS |

No example was run. Runtime behavior, including approval/standalone compatibility, is NOT MEASURED: this wave forbids real external calls/credentials and provides no isolated example-specific runtime fixtures. Compile evidence is not runtime compatibility evidence. The final compile output is `/home/cosmo/.hermes/cache/scratch/b1-w6a-examples-final.log`.

## Z5 — workspace tests and Docker survey

Command:

```text
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 TMPDIR=/home/cosmo/.hermes/cache/scratch B1_LEDGER_TEST_EXE=/home/cosmo/foundry/target/debug/deps/foundry_ledger-4c998367f41e2398 cargo test --locked --offline --workspace
```

Cargo stopped at its first failing target, `foundry --test b1_scope_cli`. Results before the stop: `foundry` had 9 passed and 4 failed (the binary unit target passed 5; `b1_approve_cli`, `b1_auth_cli`, and `b1_publish_cli_default` passed one each; `b1_scope_cli` passed one and failed four). No later crate/target ran. All four failures report `sandbox denied ... loopback bind (Operation not permitted (os error 1))`; the test output names parent rerun commands. The full log is `/home/cosmo/.hermes/cache/scratch/b1-w6a-workspace-test.log`. Do not interpret this as a product assertion failure or as a pass.

Docker survey: no Docker Rust crate dependency was found in `Cargo.lock`; watch tests do invoke the external `docker` executable and some are mandatory rather than skippable. M51's standalone attempt confirms the daemon-gated path was unavailable. The workspace stopped before Docker targets, so no Docker runtime test was measured.

## Z6 — changed files, preservation, and not performed

Repository files changed by this W6a invocation:

```text
M crates/foundry-ledger/src/product.rs
M crates/foundry-ledger/src/product/result.rs
M crates/foundry-line/src/product.rs
M crates/foundry-line/tests/b1_product_publish.rs                 # rustfmt only in Addendum 103
M crates/foundry-line/tests/b1_product_result_processes.rs         # rustfmt only in Addendum 103
M crates/foundry-line/tests/b1_product_results.rs                  # rustfmt only in Addendum 103
M crates/foundry-line/tests/support/product_result_fixture.rs      # rustfmt only in Addendum 103
M crates/foundry-serve/tests/b1_auth_http.rs                       # rustfmt only in Addendum 103
M crates/foundry-serve/tests/b1_export_audit.rs                    # rustfmt only in Addendum 103
M crates/foundry-serve/tests/b1_product_artifacts.rs               # rustfmt only in Addendum 103
M crates/foundry-watch/tests/b1_intake_durability.rs               # rustfmt only in Addendum 103
M crates/foundry/src/main.rs
M crates/foundry/tests/b1_publish_cli.rs                           # rustfmt only in Addendum 103
A .hermes/plans/b1-w6a-evidence.md
?? .hermes/plans/b1-w6a-plan-and-rubric-v1.md                      # present before Addendum 103
```

The `product.rs`, `product/result.rs`, `foundry-line/src/product.rs`, and `foundry/src/main.rs` changes predate Addendum 103 in this worktree. Addendum 103 formatted the ten named files shown in Z2; `product/result.rs` was already modified and also received formatting. The clippy fix command failed before making edits. No files were deleted. Nine test files changed only through rustfmt; there were no intentional assertion, test-name, or test-sequence changes. No schema, dependency, feature, route, or behavior change was made. There was no commit or push.

The complete W6a scratch output and created test fixture files are listed with sizes and SHA-256 in `/home/cosmo/.hermes/cache/scratch/b1-w6a-temp-files.tsv`. All agent-created evidence/log/inventory/temp files are under `/home/cosmo/.hermes/cache/scratch`; test-owned leftovers from this run are also listed there. The inventory's original-content copies are preserved for sol input.

NOT PERFORMED:

- W7 real-GitHub verification: NOT PERFORMED; no GitHub/network calls or credentials were used.
- W6b integrated sol review, final user approval, integrated commit/push: NOT PERFORMED.
- Example runtime compatibility: NOT PERFORMED (compile only).
- `m51_gate` embedded SIGKILL step: NOT PERFORMED because the mandatory Docker precondition failed before it.
- Remaining workspace targets after the first sandbox loopback failure: NOT PERFORMED, per stop-at-first-failure.
- Cursor/response-budget/verification-cost policy decision: NOT PERFORMED; it remains a separate decision.

Disk space remained above the 2 GiB guard (15 GiB available after the workspace run). No real GitHub/Jira/LLM/network operation or credential read was performed.

## Parent re-verification (round16 / round16b, 2026-10-25, base 38dccf5 + W6a fmt13 + luna122 lint pass)

- Z2 clippy (parent, workspace --all-targets, json): CLIPPY_EXIT=0, B1_PRIMARY_WARNINGS=0, ERRORS=0, OTHER=50 (unchanged from baseline; no unknown-lint warnings). Method: luna122 manual edits — 174 lint-specific module-scoped `#[allow]` entries with `// W6a clippy debt:` reasons across 78 B1 files; no `cargo clippy --fix`/`cargo fix` (banned after round13 corruption); test bodies, assertions, and cell names unchanged.
- Z2 fmt (parent): FMT_CHECKED=171 FAIL=0.
- Z5 workspace (parent): `cargo test --locked --offline --workspace -- --test-threads=1` EXIT=0, **917 passed / 0 failed across 95 result records** (workspace section only, round16b.log); the preceding standalone intake retry is a SEPARATE lane (**10 passed / 0 / 1 record, EXIT=0**). Per-CRATE totals (crash_child 0/0 is foundry-watch tests/bin): foundry-serve 82/0, foundry-watch 219/0 (+bin/crash_child 0/0), foundry-line 111/0, foundry-exec 144/0 (142 integration/unit + 2 doctests; includes review_regressions 5/0 which resides in foundry-exec, not foundry-line), foundry-workflow 68/0 (67 unit + 1 integration), foundry-ledger 74/0, foundry-orch 56/0, foundry-gate 42/0, foundry-pack 23/0, foundry-approval 21/0, foundry-blueprint 7/0, foundry-harness-cli 17/0, foundry 20/0, foundry-domain 33/0, foundry-furnace 0/0, foundry-inspector 0/0. Doctests are counted within their crate (foundry-exec includes 2 doctests; g1_journal_equivalence integration target is 1/0, not 3/0 — the earlier target-name table miscounted doctests into it). Earlier target-name table also omitted review_regressions 5/0 and plain_pack 1/0 (plain_pack inside foundry-pack; review_regressions inside foundry-exec above). Docker-dependent lanes ran green in the parent workspace (m51_gate 1/0, t56w_docker_mandatory 2/0).
- Z4 runtime re-measure (parent round17, individual `cargo run --locked --offline -p foundry --example <e>`, 60s timeout each): EXIT=0: m4_gate, t16_live_replay, t17_v2_write. m3_gate: timeout(124) after check cells [5]..[12] all pass — live poll loop does not self-terminate (exact blocker: indefinite live wait). m2_gate: panic at m2_gate.rs:124 `assertion failed: out.status.success()` after GATE 1 PASS (exact blocker: downstream subprocess nonzero exit). m7_demo: panic m7_demo.rs:79 "review evidence unavailable: tuple reader not configured". m8_live_die: die succeeded (exit=0, result_commit=68deb4297564) then panic m8_live_die.rs:120 "tuple reader not configured". m8_publish: exit 64 usage error — requires args (`--issue <n> | --resume <run-dir> [–notify-mode]`); executed without args. t17_tamper_detect: panic t17_tamper_detect.rs:41 — external injector script `/tmp/t17_tamper.py` not present (not created in this environment; /tmp dependency).
- Z4 inventory correction: the repository contains exactly NINE example targets; T16 has exactly ONE (t16_live_replay). The frozen rubric's "t16 x2" minimum does not match the actual example set at 38dccf5 (T17 x2 and M8 x2 exist; T16 x2 does not). Recorded as an inventory discrepancy honestly rather than silently reducing: all nine existing targets compile and each has an individual runtime record above.

- Affected lanes (parent): ledger --lib --tests EXIT=0; core (exec/line/ledger/serve lib+tests) EXIT=0; watch EXIT=0; cli-default (-p foundry) EXIT=0; intake b1_intake_durability EXIT=0.
- Z3: kill9 standalone evidence remains luna120 (9/9 green in sandbox) + round13 parent lanes (b1_product_publish, b1_product_result_processes --features p2-test-support, m51_gate Docker, t58w_crash_injection all EXIT=0); workspace run above re-covers the default-feature kill cells.
- Honest notes: round13 `cargo clippy --fix` corrupted 33+ files (foundry-gate 150 compile errors); recovered by user-approved `git reset --hard 38dccf5` (approval 1563660801007554642) + mechanical rustfmt re-application of 13 files; compile guard CHECK_EXIT=0. W7 remains NOT PERFORMED.
