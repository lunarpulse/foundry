# T33-B Die stage execution and evidence

## Serve configuration

Review execution is enabled only when `FOUNDRY_REVIEW_PACK`, `FOUNDRY_DIE_BINARY`,
and `FOUNDRY_REPO_PATH` are configured. `FOUNDRY_DIE_PROFILE` defaults to
`coding`; `FOUNDRY_DIE_TIMEOUT` defaults to `600` seconds. The session root is
`FOUNDRY_SESSION_ROOT`, or `sessions/` beside the review host database when it
is omitted. If either the Die binary or repository path is absent, serve keeps
the `UnavailableStageExecutor` behavior. Set `FOUNDRY_TUPLES` and
`FOUNDRY_REVIEW_PACK` as usual for the tuple store and frozen review pack.

The executor clones the configured local repository without checking out a
branch, then detaches at the task base commit. Lens and verification tasks use
the latest artifact commit as their base. Verified generated commits are kept
in `session_root/.commits.git` and fetched into subsequent workdirs; the source
repository is not modified. A fix task uses `fix_context` as
its manifest base and sets `fix_context=true`; other stages use `base_commit`.
The manifest and Die log remain under the execution work directory. Session
records and a copy of the most recent `die-output.json` live under the session
root. The stage's sealed output is the structured `die-output.json`; process
stdout/stderr remain in the sibling `attempt-0.log`. Reused session IDs restore
the previous result into each new workdir, including shared lens sessions.

## Evidence bundle r1-r9

The live PR exercise is run by the parent workflow with network and GitHub
credentials. These read-only queries identify the resulting evidence. Replace
the paths and IDs with the run's values. The host database is the file configured
through the serve review host setup (default `~/.foundry/review_host.db`); the
tuple database is configured by `FOUNDRY_TUPLES`; the lifecycle journal is
`FOUNDRY_LEDGER` (default `~/.foundry/ledger.db`).

| ID | Evidence | Lookup |
|---|---|---|
| r1 | Order creation and lifecycle journal | `GET /api/orders/{order_id}`; raw rows: `SELECT seq,event_json,prev_hash,hash FROM ledger WHERE order_id=? ORDER BY seq;` |
| r2 | Die input manifest and process log | `SELECT sequence,stage_id,status,output_json FROM review_stage_attempts WHERE order_id=? ORDER BY run_id,sequence;` Each complete stage JSON includes `input_manifest` and `log_path`; the log is the `attempt-0.log` under the returned path's work directory. |
| r3 | Verification verdict and findings | Same stage-attempt query; select rows with `json_extract(output_json,'$.role')='verify'` and inspect `$.verdicts`, `$.findings`, and `$.inspected_commit`. |
| r4 | Fix commit, when a redrive ran | `SELECT run_id,sequence,stage_id,output_json FROM review_stage_attempts WHERE order_id=? AND json_extract(output_json,'$.produced_commit') IS NOT NULL ORDER BY run_id,sequence;` |
| r5 | Generation chain | `SELECT result_json FROM host_results WHERE order_id=? ORDER BY rowid;` The `generations` field is copied into the tuple as well. |
| r6 | Sealed tuple | In the tuple DB: `SELECT order_id,attempt,run_id,generation,immutable_commit,artifact_location,generations_json FROM artifact_tuples WHERE order_id=? ORDER BY run_id,generation;` Confirm the final tuple's `immutable_commit` equals the last generation commit and the final host result's `result_commit`. |
| r7 | Human hold decision | `SELECT status,hold_json FROM review_runs WHERE order_id=? ORDER BY rowid;` `hold_json.human_events` records the actor, timestamp, action, and resume target. |
| r8 | Approval and publication transition | `GET /api/orders/{order_id}` shows the ledger events. The terminal review result appears in `host_results`; lifecycle events include the later approval and transition to `Publishing`. |
| r9 | Full journal-chain integrity | Authenticated `GET /api/audit/verify` returns `chain_valid`. For a manual full-chain check, use `SELECT seq,event_json,prev_hash,hash FROM ledger ORDER BY seq;` across all orders, since the hash chain is global. Checking only the order-filtered r1 rows is insufficient. |

The host evidence is in SQLite tables `review_runs`, `review_stage_attempts`,
`review_sessions`, and `host_results`. Human decisions are persisted in
`review_runs.hold_json`; stage output JSON stores the input manifest and output
log path; `host_results.result_json` stores the final host result and generation
chain. The tuple store records the immutable commit, artifact location, and
generations JSON separately. No query in this document mutates the databases.

Example read-only shell queries:

```sh
sqlite3 "$HOST_DB" "SELECT sequence,stage_id,status,output_json FROM review_stage_attempts WHERE order_id='$ORDER_ID' ORDER BY run_id,sequence;"
sqlite3 "$HOST_DB" "SELECT status,hold_json,result_json FROM review_runs WHERE order_id='$ORDER_ID' ORDER BY rowid;"
sqlite3 "$TUPLE_DB" "SELECT immutable_commit,artifact_location,generations_json FROM artifact_tuples WHERE order_id='$ORDER_ID' ORDER BY run_id,generation;"
```
