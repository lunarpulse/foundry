# W20–W22 reconstruction and adjudication handoff

W20, E02, E02b, E03 and E04 passed on the uncommitted candidate rebuilt from scoped W1 commit `e3631b1ba40511159b58ff7b2c2c0817a44d2cc5`. This is the engineering result submitted for slice adjudication. W23/E01 and full W2 acceptance remain open. The candidate patch SHA-256 is `71187cdb3ffa8dfdcfea7fabbac79812e921d9f3fcec3979ec65a036986ea080`. It reconstructs all 14 changed or new source and test files from the accepted base, including executable bits. [Patch](w2.patch) · [Clean-base and reverse checks](patch-check.log) · [All 45 patch hunks](hunk-provenance.md)

The graph report was absent on entry. It was generated locally and read before roadmap section 6. The original oracle and alpha decisions were recorded before implementation or Cargo. The five named dirty alpha files were reference inputs. The initial graph had 6,187 nodes and 15,554 edges and used local extraction. The post-edit graph report is archived with the evidence. [Frozen assertions](oracle.md) · [Oracle digest](oracle.sha256) · [Contract and input provenance](contract.md) · [Graph update](graph-update.log) · [Updated report](GRAPH_REPORT.md)

## Final runs

| Run | Exit | Passed | Failed | Ignored | Filtered | Evidence |
|---|---:|---:|---:|---:|---:|---|
| W20–W22, three parent tests | 0 | 3 | 0 | 0 | 0 | [Log](final-rows.log), [independent reader](verify-final.log) |
| W1 v4 selected-review CLI, two orders on different selected packs | 0 | 1 | 0 | 0 | 0 | [Log](final-v4.log) |
| Final exact W22 selector | 0 | 1 | 0 | 0 | 2 | [Log](final-w22.log), [independent reader](verify-exact.log) |

The exact selector reruns a member of the combined suite. Child test harnesses provide the owned producer and preflight fixtures. The table reports parent tests. Each log starts with its Cargo argv. The runner sets locked/offline single-job Cargo, disables incremental compilation and puts temporary data and build targets in the private scratch directory. The changed Rust sections were formatted and the diff checked before Cargo. [Runner](cargo-run) · [Machine-readable results](evidence.json)

## Row measurements

| Row | Measurements and observation |
|---|---|
| W20 | Strict task decoding normalized a 550-byte issue body payload into 417 canonical task bytes in this fixture. The owner receipt binds the snapshot and task SHA-256, identity and scope revisions, source/order/lifecycle/admission and active pack selection. Admission, task execution, DriveReview, result approval and publication use distinct checks. [Canonical bytes and receipt](runs/final/w20-tasks.json) |
| E02 | Eleven disallowed inputs included shell-shaped text, invalid paths, argv, timeout, model calls and cost. They remained unexecuted. The shell canary was absent. The watch could claim none. The independent database read found zero claims, producer rows/events, review runs, results, grants, approvals and publication outcomes. [Rows](runs/final/w21.json) |
| E02b | Both edits had IDs that sorted ahead of their first delivery. Direct admission of B first failed. The real watch drain acknowledged both deliveries and reused the first admission. Across the fixture there were 13 first observations, 15 acknowledged deliveries and one owner task. A valid A retained its bytes, digest, receipt and actor after a different valid B and redelivery. An unsupported A remained unsupported after a valid B. [First bytes and digests](runs/final/first-observations.json) · [Delivery and ack rows](runs/final/observed-deliveries.json) · [Retained receipt](runs/final/w21-tasks.json) |
| E03 | The live adapter checks DriveReview and worker/epoch/fence in one database snapshot. Source and service revocation and regrant, lease expiry, reassignment and task grant withdrawal all had refusal controls. A separate approver account held a real fixture grant, token and prepared proof. The intact private copy approved once. Isolated copies with revoked service or missing original collection refused the same proof. Fresh approval after owner withdrawal and after regrant failed. The principal fixture retained one readable finalized result, zero fresh receipts and zero token consumptions. [Before refusal](runs/exact/withdrawn-before.json) · [After refusal](runs/exact/withdrawn-after.json) |
| E03 collection | A real ProductStore reservation, seal and collect connected the 180-byte full-index diff to one Git parent, the selected pack, the work fence, the review host and the separately finalized tuple. An offline reader unbundled the retained commits, recomputed the diff and tuple digest, and verified the inspector's hash against the README blob. Exact-run head is `59951eef0813f7af50483dd050496cf4c818edca`, tuple SHA-256 is `067dba8debd9678bbb5e57c6f0d3db16d1c7370573451ca0f7efb3019446f155`. [Final result](runs/exact/final-result.json) · [Retained Git bundle](runs/exact/fixture.bundle) · [Sealed patch](runs/exact/sealed.patch) |
| E04 | Six missing/partial pack, repo or Die configurations and three invalid serve timeouts refused work. Valid preflight reserved without entering the executable canary. The test separately injected a pre-seal loss with empty output coordinates. For actual post-seal loss, an owned child made a new Git commit and sealed its output, then the parent killed and reaped that child with signal 9 before collection. Recovery changed only the producer journal and event history. Operation key, original fence, result commit and artifact digest survived. Each of two retries preserved the schema, every typed row in 82 tables (81 application tables and SQLite's sequence table), and bytes and inode identity of five review files and the producer patch. Both Git object directories were also compared. [Journal before](runs/exact/pre-recovery-producer.json) · [Journal after](runs/exact/post-recovery-producer.json) · [First retry](runs/exact/retry-1.json) · [Second retry](runs/exact/retry-2.json) · [Loss Git bundle](runs/exact/loss.bundle) |

## Schema, wire and fixture scope

On normal feature-enabled v4 issue intake the first-observation table pins exact UTF-8 bytes and delivery ID per source/connector/repository/issue. A first authorization can populate that anchor from an existing inbox; the admission must agree with it. This fourth sidecar supplements the three initially specified task, producer and event tables. Those three are provisioned inside explicit authorization. The anchor fixes the discovered delivery-order defect and supports inspecting rejected first snapshots before any authorization. Fresh v4 approvals require original producer collection evidence, including for otherwise readable earlier local-commit results. The replay path for an existing receipt precedes that new-write check. This work retains the scoped W1 single-admission-per-order-ID constraint. [Observed schema](runs/exact/schema.json)

The wire contract accepts literal relative file paths, one fixed Git check, a task deadline of 1–3600 seconds, and a per-check deadline within the task deadline. The model identifier is `none`, with zero calls, cost and fix generations. The result was generated by an offline fixture producer and inspected by a deterministic Git-blob reader through the product host. The owned child loss is a real process termination. The deadline control uses an injected test clock. Missing collection is simulated by removing the event in an isolated database copy. The successful approval control is an automated fixture actor. No human W23 approval, real Die/model execution, external publication, production database access, or deployment migration was performed. No commit or push was made to the integration repository.

## Iteration record

Earlier runs are retained as diagnostics and excluded from the final totals. Initial E02/W22 setup attempted a self-grant that the product refused. A distinct scope administrator fixed the fixture. A lens-only fixture produced a hold; the selected pack was changed to the verify role and its manifest recomputed. Two runs exposed the 81-table count and prompted the immutable first-observation table. Another exposed an unconfigured artifact root on the test reader. A later real-child run exposed the fixture clone's disabled local file transport. The fixture clone now permits file transport, while the product verifier disables Git transports. [First E02](w21-first.log) · [First W22](w22-first.log) · [Held result](w2-second.log) · [Table count](w22-verify-pack.log) · [Repeated count](w22-controls.log) · [Reader configuration](w22-anchor.log) · [Owned clone](candidate-rows.log) · [Intermediate success](rows-rebuilt.log)

## Durable verification

From the worktree root, the following reads the evidence from disk and uses a disposable local Git directory. The two readers require only Python 3 and Git. SHA256SUMS covers the patch, records, programs, logs and retained fixture files. The original integration worktree and dirty main are unnecessary for reading the saved results.

```sh
(cd docs/acceptance/w2 && sha256sum -c SHA256SUMS)
sha256sum -c docs/acceptance/w2/oracle.sha256
python3 docs/acceptance/w2/verify.py docs/acceptance/w2/runs/final
python3 docs/acceptance/w2/verify.py docs/acceptance/w2/runs/exact
```

For a clean reconstruction, apply w2.patch to a separate checkout at the recorded W1 base. The optional acceptance exporter used by the Rust tests lives in this evidence directory. To rerun and save new observations, supply a new absolute W2_EVIDENCE_DIR under docs/acceptance/w2 and a fresh log name to cargo-run. [Exporter](observe.py) · [Standalone verifier](verify.py)
