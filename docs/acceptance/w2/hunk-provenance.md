# W2 patch provenance

Exact hunk coordinates refer to w2.patch. The W1 base is e3631b1ba40511159b58ff7b2c2c0817a44d2cc5. The five dirty alpha source digests and the excluded launcher/model hunks are recorded in contract.md. The additional capability spelling comes from dirty crates/foundry-ledger/src/product_scope.rs with SHA-256 b87dc84ba7fe6e5515c247c2cf7c079f825221ca63286c8d53f4c30980737228. All source comparisons used read-only access.

## crates/foundry-ledger/src/product.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-373,6 +373,8` | W1 module wiring for the new task authority implementation. |
| `-4457,6 +4459,8` | E03 new decision at the W1 fresh approval write. Require the original collection before consuming the human grant. |
| `-4839,6 +4843,23` | E02b new fix at the scoped page write. Pin first observation in the same transaction. |
| `-5005,7 +5026,7` | E02b new fix. Drain in observation order. |
| `-5043,7 +5064,7` | E02b new fix. Keep the source-scoped drain in observation order. |
| `-5201,6 +5222,39` | E02b new fix at W1 admission. A direct request to admit B first must also wait. |
| `-5543,8 +5597,20` | E04 new fix in W1 scoped claim selection. Keep sealed and terminal journals out of the reclaim queue. |
| `-5636,7 +5702,8` | E03 new fix at W1 worker validation. Read claim and service authority in one transaction. |
| `-5645,6 +5712,12` | E03 new fix at the same validation boundary. Match the reserved worker and current service grant. |

## crates/foundry-ledger/src/product/commit_review.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-354,7 +354,7` | W1 existing S1 transition helper exposed to the W2 ledger module. Compare alpha task.rs lines 767 onward. |

## crates/foundry-ledger/src/product/result.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-848,6 +848,17` | E03 new W1 finalizer check binding fresh results to the collected tuple. |

## crates/foundry-ledger/src/product/review.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-160,7 +160,12` | W1 hold transition recognizes the collected producer review state. |
| `-235,7 +240,15` | W1 state predicate extended with W2 collection. Compare alpha task.rs lines 1935–2294. |
| `-439,7 +452,7` | W1 admission read uses the common producer state predicate. |
| `-486,7 +499,7` | W1 admission transaction uses the common producer state predicate. |
| `-846,7 +859,7` | W1 activation open mode check uses the common state predicate. |
| `-877,6 +890,35` | E03 new activation guard joins selected W1 pack, collection key, head and fence. |
| `-1501,6 +1543,39` | E03 new terminal check and S1 append in the W1 host transaction. Compare alpha task.rs lines 1935–2294. |
| `-3184,7 +3259,7` | W1 new-work gate uses the same state predicate. |
| `-3363,7 +3438,7` | W1 stage source gate uses the same state predicate. |
| `-3599,6 +3674,11` | E04 new offline-profile guard on real continuation. |

## crates/foundry-ledger/src/product_scope.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-147,6 +147,7` | Alpha product_scope.rs line 150. Add the human task capability to W1. |
| `-161,6 +162,7` | Alpha product_scope.rs line 165. Preserve the capability wire spelling. |
| `-175,13 +177,16` | Alpha product_scope.rs line 180 for parsing. W2 adds an explicit owner-only role rule. |

## crates/foundry-line/src/die_executor.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-62,6 +62,19` | E04 new shared timeout parser at the W1 executor. The task/check clock remains separate. |

## crates/foundry-line/src/review_host.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-48,6 +48,12` | E03 new opt-in executor capability for the trusted offline reader. |
| `-59,6 +65,11` | E03 forward that capability through the boxed W1 executor. |
| `-1031,6 +1042,16` | E03 W2 host refuses real execution for an offline task. |
| `-1062,6 +1083,28` | E03 inspect the selected W1 pack before dispatch and reject fix or redrive for this profile. |

## crates/foundry-serve/src/lib.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-162,6 +162,14` | E04 add shared strict timeout and reject partially configured serve execution. |
| `-196,10 +204,6` | E04 remove the independent permissive timeout parser from serve. |

## crates/foundry/src/main.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-405,6 +405,12` | Alpha main.rs lines 2409–2498 and 4368–4441 adapted to W1 CLI commands. |
| `-731,10 +737,7` | E04 use the shared parser at CLI host construction. |
| `-2494,6 +2497,10` | Alpha main.rs lines 2409–2498 and 4368–4441. Dispatch to explicit W1 task and producer calls. |

## crates/foundry/src/selected_review.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-76,7 +76,13` | E02b extend W1 local intake with an issue snapshot input. |
| `-175,14 +181,33` | E02b read the original body bytes after checking the operator input and dispatch by task kind. |
| `-200,8 +225,8` | E02b bind the W1 delivery identity to the chosen kind and value. |
| `-218,8 +243,8` | E02b forward the chosen kind and exact snapshot to the existing intake path. |
| `-476,3 +501,131` | Alpha main.rs lines 2409–2498 for owner authorization and 4360–4441 for preflight/reserve/collect. The caller supplies a digest, the retained inbox supplies the bytes. Status and recover read the W2 journal. |

## crates/foundry/tests/support/b2_w1.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-121,7 +121,7` | W1 fixture supports a private bootstrap without P2 seed rows. |
| `-686,6 +686,11` | Extract the existing W1 manifest refresh for the inspect-only fixture pack. |
| `-745,7 +750,7` | Return the refreshed digest from the extracted W1 fixture helper. |

## crates/foundry-ledger/src/product/code_change.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-0,0 +1,808` | See the new-file ranges below for alpha adaptation and new W2 logic. |

## crates/foundry-ledger/src/product/code_change/retained_git.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-0,0 +1,121` | Alpha task.rs lines 318–463 and 526 onward, product_git_producer.rs lines 64–87 and 146–201, and product-alpha-die lines 169–200 and 214–264 inform retained Git and allowlist checks. W2 implements the fixed read-only check and bounded binary diff. |

## crates/foundry/tests/w2.rs

| Old and new hunk coordinates | Origin and integration decision |
|---|---|
| `-0,0 +1,1370` | See the new-file ranges below for fixture provenance and oracle coverage. |

## Ranges inside new files

| Destination lines | Origin and disposition |
|---|---|
| code_change.rs 1–100 | Adapt alpha task.rs 25–64 and 211–257. Strict structured fields, sorted paths, approved argv and pinned zero-cost model profile. |
| code_change.rs 101–182 | New E02b first-observation sidecar, typed W2 receipt and producer schema, immutable evidence and terminal guards. |
| code_change.rs 183–410 | Adapt alpha task.rs 25–64 and 1603–1747. Bind original inbox/ack/S1, current W1 actor and selected pack, installation/database, task and snapshot digests. |
| code_change.rs 411–571 | Adapt alpha task.rs 79–145 and 1101–1533. The reservation and immutable worker/epoch/fence share the W1 claim transaction. |
| code_change.rs 572–705 | Adapt alpha task.rs 1101–1533 and 1935–2294, and main.rs 4368–4441. Separate sealing, collection, inspection and explicit uncertain recovery. |
| code_change.rs 706–808 | New E03 bridge to W1 host, adapter, finalizer and fresh approval. Require original collection and retained authority revisions. |
| retained_git.rs 1–54, 55–121 | Controller runs bounded local Git and compares parent, NUL-separated paths, fixed check, and full-index patch. See source ranges in the hunk list. |
| w2.rs 1–312 | Adapt alpha product_alpha_cli.rs 155–166 and 280–360 fixture pattern on top of the W1 RuntimeFixture. Source page and ack are private/offline. |
| w2.rs 313–400, 401–560 | New W20 canonical digest and authority coordinates, W21 E02/E02b inversion, redelivery and no-effect assertions. |
| w2.rs 561–829 | New deterministic reader, owned producer child, SIGKILL boundary, typed row/file snapshots and twice-retried recovery. |
| w2.rs 830–1054 | New W22 positive path and preflight including strict serve timeout. |
| w2.rs 1055–1370 | New independent source/service/owner controls, expired and reassigned claims, fresh approval controls and retained reads. |
