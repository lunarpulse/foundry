//! Product adapter from durable review dispatches to a frozen pack graph.
#![allow(clippy::needless_borrows_for_generic_args)] // W6a clippy debt: preserve explicit generic-call borrow boundaries

use foundry_exec::{
    ArtifactTuple, ArtifactTupleKey, Attempt, DispatchHost, DurableReviewRun, EmitLog, EmitReceipt,
    EmitSink, Epoch, ExecutionIntent, HostError, HostKey, HostResult, IntentLog, OrderId,
    PromotionReceipt, ReviewAdmission, ReviewAdmissionRequest, ReviewAdmissionStore,
    ReviewDispatchStore, ReviewHostStore, ReviewStageFinish, ReviewStageFinishRequest,
    ReviewStageStart, RunId, TupleStore,
};
use foundry_pack::{EngineAction, GraphEngine, RunInput, load_pack};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const REVIEW_PACK_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";

#[derive(Clone, Debug)]
pub struct StageTask {
    pub stage_id: String,
    pub role: String,
    pub lens_bytes: Vec<u8>,
    pub session_id: String,
    pub fix_context: Option<String>,
    pub base_commit: String,
    pub workdir: PathBuf,
    pub artifact_commit: String,
    pub inputs: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default)]
pub struct StageOutput {
    pub verdict_category: Option<String>,
    pub findings: bool,
    pub log_path: String,
    pub inspected_commit: Option<String>,
    pub verdict_categories: Vec<String>,
    pub produced_commit: Option<String>,
}
/// The adapter must normalize output, stop all writes before returning, and
/// confine process, git-ref, and session writes to the supplied workdir.
pub trait StageExecutor: Send {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String>;
    fn ensure_available(&self) -> Result<(), String> {
        Ok(())
    }
    fn restore_session(&mut self, _session_id: &str, _workdir: &Path) -> Result<(), String> {
        Err("session restoration is not implemented".into())
    }
}
impl<T: StageExecutor + ?Sized> StageExecutor for Box<T> {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        (**self).execute(task)
    }
    fn ensure_available(&self) -> Result<(), String> {
        (**self).ensure_available()
    }
    fn restore_session(&mut self, session_id: &str, workdir: &Path) -> Result<(), String> {
        (**self).restore_session(session_id, workdir)
    }
}
/// Product wiring placeholder until the real LLM/Die adapter is supplied by T33.
#[derive(Default)]
pub struct UnavailableStageExecutor;
impl StageExecutor for UnavailableStageExecutor {
    fn ensure_available(&self) -> Result<(), String> {
        Err("review stage executor is not configured".into())
    }
    fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
        Err("review stage executor is not configured".into())
    }
}

type ActiveReviewRow = (
    String,
    i64,
    Option<String>,
    i64,
    String,
    String,
    Option<String>,
);

/// Standalone SQLite persistence owner. Recovery is completed before this
/// value can be supplied to the shared host algorithm.
struct StandaloneReviewStore {
    db_path: PathBuf,
    conn: Connection,
    pack_digest: String,
}
impl StandaloneReviewStore {
    fn open(path: &Path, pack_digest: &str) -> Result<Self, String> {
        let conn = open_db(path)?;
        let db_path = fs::canonicalize(path).map_err(sqlerr)?;
        let mut store = Self {
            db_path: db_path.clone(),
            conn,
            pack_digest: pack_digest.to_owned(),
        };
        recover_resuming(&mut store.conn, &db_path)?;
        Ok(store)
    }
}
impl ReviewHostStore for StandaloneReviewStore {
    fn pack_digest(&self) -> &str {
        &self.pack_digest
    }
    fn run_root(&self, run_id: &str) -> Result<PathBuf, HostError> {
        Ok(self
            .db_path
            .with_extension("review-runs")
            .join(format!("{:x}", Sha256::digest(run_id.as_bytes()))))
    }
    fn claim_resume(&self, run_id: &str) -> Result<Option<fs::File>, HostError> {
        Ok(resume_claim(&self.db_path, run_id)?)
    }
    fn resolved_attempt(&self, order: &str, attempt: u64) -> Result<bool, HostError> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_runs WHERE order_id=?1 AND attempt=?2 AND status IN ('resolved','review_aborted'))",
            params![order, attempt as i64], |row| row.get(0),
        ).map_err(sqlerr).map_err(HostError::Persistence)
    }
    fn session_receipt(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<foundry_exec::ReviewSessionReceipt>, HostError> {
        self.conn.query_row(
            "SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",
            params![order, session_id], |row| row.get::<_, String>(0),
        ).optional().map_err(sqlerr).map_err(HostError::Persistence)?.map(|json|serde_json::from_str(&json).map_err(sqlerr).map_err(HostError::Persistence)).transpose()
    }
    fn invalidate_terminal_runs(&mut self, order: &str) -> Result<(), HostError> {
        self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')", [order])
            .map(|_| ()).map_err(sqlerr).map_err(HostError::Persistence)
    }
    fn redrive_payload(
        &self,
        order: &str,
        run: &str,
        payload: &str,
    ) -> Result<Vec<foundry_exec::ReviewRedriveArtifact>, HostError> {
        redrive_payload(&self.conn, order, run, payload)
            .map_err(|e| HostError::Persistence(e.to_string()))
    }
    fn charge_budget(&mut self, order: &str, intent: &ExecutionIntent) -> Result<(), HostError> {
        charge_review_budget(&mut self.conn, order, intent)
            .map_err(|e| HostError::Persistence(e.to_string()))
    }
    fn accepted(&self, intent: &ExecutionIntent) -> Result<bool, HostError> {
        self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",
            params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],
            |row| row.get(0),
        ).map_err(sqlerr).map_err(HostError::Persistence)
    }
    fn latest_run(
        &self,
        order: &str,
        attempt: u64,
    ) -> Result<Option<foundry_exec::RunRecord>, HostError> {
        let mut stmt = self.conn.prepare("SELECT run_id,base_commit,result_commit,status,hold_json,hold_instance_id FROM review_runs WHERE order_id=?1 AND attempt=?2 ORDER BY rowid DESC").map_err(sqlerr)?;
        let rows = stmt
            .query_map(params![order, attempt as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(sqlerr)?;
        let mut best: Option<foundry_exec::RunRecord> = None;
        for row in rows {
            let (run_id, base_commit, result_commit, status, hold_json, hold_instance_id) =
                row.map_err(sqlerr)?;
            let parts: Vec<_> = run_id.split(':').collect();
            let run = parts
                .get(parts.len().saturating_sub(2))
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| "invalid durable review run id".to_owned())?;
            if best.as_ref().is_none_or(|old| run > old.run) {
                best = Some(foundry_exec::RunRecord {
                    run,
                    base_commit,
                    result_commit,
                    status,
                    hold_json,
                    hold_instance_id,
                });
            }
        }
        Ok(best)
    }
    fn mark_hold_expired(
        &mut self,
        request: foundry_exec::ReviewHoldUpdate,
    ) -> Result<(), HostError> {
        validate_hold_update(&request)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        let current = check_hold_expectation(&tx, &request.expected, "held")?;
        if request.expected.deadline <= 0 || request.expected.deadline > request.now {
            return Err(HostError::Worker("hold deadline has not expired".into()));
        }
        let updated = encode_continuation_snapshot(&request.snapshot)?;
        if current.2 != request.expected.deadline {
            return Err(HostError::Worker("hold deadline changed".into()));
        }
        let changed=tx.execute("UPDATE review_runs SET hold_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='held'",params![updated,request.expected.binding.order_id,request.expected.binding.attempt as i64,request.expected.binding.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err(HostError::Worker(
                "review hold changed while recording expiry".into(),
            ));
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn begin_hold_resume(
        &mut self,
        request: foundry_exec::ReviewHoldUpdate,
    ) -> Result<(), HostError> {
        validate_hold_update(&request)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        let current = check_hold_expectation(&tx, &request.expected, "held")?;
        if current.2 != request.expected.deadline || (current.2 > 0 && current.2 <= request.now) {
            return Err(HostError::Worker(
                "review hold deadline expired or changed".into(),
            ));
        }
        let updated = encode_continuation_snapshot(&request.snapshot)?;
        let changed=tx.execute("UPDATE review_runs SET status='resuming',hold_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='held'",params![updated,request.expected.binding.order_id,request.expected.binding.attempt as i64,request.expected.binding.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err(HostError::Worker(
                "review hold changed before continuation".into(),
            ));
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn restore_hold_after_resume_error(
        &mut self,
        binding: foundry_exec::ReviewRunBinding,
    ) -> Result<(), HostError> {
        let changed=self.conn.execute("UPDATE review_runs SET status='held' WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND status='resuming'",params![binding.order_id,binding.attempt as i64,binding.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err(HostError::Worker(
                "review continuation fence changed".into(),
            ));
        }
        Ok(())
    }
    fn resolve_hold_result(
        &mut self,
        request: foundry_exec::ReviewHoldResolution,
    ) -> Result<(), HostError> {
        if request.snapshot.binding != request.expected.binding
            || request.snapshot.held.hold.hold_instance_id != request.expected.hold_id
        {
            return Err(HostError::Worker(
                "hold resolution snapshot binding mismatch".into(),
            ));
        }
        let status = match request.action {
            foundry_exec::ReviewHoldAction::CommitPass
            | foundry_exec::ReviewHoldAction::CommitFail => "resolved",
            foundry_exec::ReviewHoldAction::Abort => "review_aborted",
            foundry_exec::ReviewHoldAction::Resume { .. } => {
                return Err(HostError::Worker(
                    "resume action cannot resolve a hold".into(),
                ));
            }
        };
        if request.result.held.is_some() {
            return Err(HostError::Worker(
                "resolved hold result must not retain a held state".into(),
            ));
        }
        let result_json = serde_json::to_string(&request.result).map_err(sqlerr)?;
        let hold_json = encode_continuation_snapshot(&request.snapshot)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        let current = check_hold_expectation(&tx, &request.expected, "held")?;
        if current.2 != request.expected.deadline {
            return Err(HostError::Worker("review hold deadline changed".into()));
        }
        if request.expected.deadline > 0 && request.expected.deadline <= request.now {
            return Err(HostError::Worker("review hold deadline expired".into()));
        }
        let changed=tx.execute("UPDATE review_runs SET status=?1,hold_json=?2,result_json=?3 WHERE order_id=?4 AND attempt=?5 AND run_id=?6 AND status='held'",params![status,hold_json, result_json,request.expected.binding.order_id,request.expected.binding.attempt as i64,request.expected.binding.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err(HostError::Worker(
                "review hold changed before resolution".into(),
            ));
        }
        let intent = &request.snapshot.intent;
        let changed=tx.execute("UPDATE host_results SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64]).map_err(sqlerr)?;
        if changed != 1 {
            return Err(HostError::Persistence(
                "review result binding is missing".into(),
            ));
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn save_redrive_output(
        &mut self,
        order: &str,
        intent: &ExecutionIntent,
        run: &str,
        sequence: u64,
        stage_id: &str,
        session_key: &str,
        session_id: &str,
        output: &foundry_exec::ReviewStageEvidence,
    ) -> Result<(), HostError> {
        let bytes = fs::read(&output.log_path).map_err(|e| e.to_string())?;
        let session = SessionReceipt {
            session_id: session_id.to_owned(),
            output: output.log_path.clone(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(&tx, intent, run, &self.pack_digest)?;
        ensure_new_stage_sequence(&tx, order, run, sequence)?;
        tx.execute(
            "INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'complete',?5)",
            params![order,run,sequence as i64,stage_id,serde_json::json!({"role":output.role,"log_path":output.log_path,"produced_commit":output.produced_commit,"input_manifest":output.input_manifest}).to_string()],
        ).map_err(sqlerr)?;
        tx.execute(
            "INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",
            params![order,run,session_key,serde_json::to_string(&session).map_err(sqlerr)?],
        ).map_err(sqlerr)?;
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn continuation_binding(
        &self,
        order: &str,
        attempt: i64,
        run: &str,
    ) -> Result<foundry_exec::ReviewContinuationBinding, HostError> {
        let (pack, sequence, result): (String, i64, String) = self.conn.query_row(
            "SELECT pack_digest,COALESCE((SELECT MAX(sequence) FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2),0),result_json FROM review_runs WHERE order_id=?1 AND attempt=?3 AND run_id=?2 AND status='resuming'",
            params![order, run, attempt], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).map_err(sqlerr)?;
        Ok(foundry_exec::ReviewContinuationBinding {
            pack_digest: pack,
            next_sequence: sequence.max(0) as u64,
            stored_result: serde_json::from_str(&result).map_err(sqlerr)?,
        })
    }
    fn continuation_counters(
        &mut self,
        request: foundry_exec::ReviewCounterCheckpoint,
        pack: &str,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_resuming_stage_run(&tx, &request.intent, &request.journal_run, pack)?;
        let changed=tx.execute("UPDATE review_runs SET counters_json=?1 WHERE order_id=?2 AND run_id=?3 AND status='resuming'",params![serde_json::to_string(&request.counters).map_err(sqlerr)?,request.intent.order.0,request.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err("continuation run changed while checkpointing counters".into());
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn continuation_session(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<foundry_exec::ReviewSessionReceipt>, HostError> {
        self.session_receipt(order, session_id)
    }
    fn continuation_budget_and_start(
        &mut self,
        request: foundry_exec::ReviewStageStart,
        pack: &str,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_resuming_stage_run(&tx, &request.intent, &request.journal_run, pack)?;
        ensure_new_stage_sequence(
            &tx,
            &request.intent.order.0,
            &request.journal_run,
            request.sequence,
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO review_budget(order_id,used) VALUES(?1,0)",
            [&request.intent.order.0],
        )
        .map_err(sqlerr)?;
        if tx
            .execute(
                "UPDATE review_budget SET used=used+1 WHERE order_id=?1 AND used<?2",
                params![request.intent.order.0, request.budget_limit as i64],
            )
            .map_err(sqlerr)?
            == 0
        {
            return Err("order execution budget exhausted".into());
        }
        tx.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",params![request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(sqlerr)?;
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn finish_continuation_stage(
        &mut self,
        request: ReviewStageFinishRequest,
        pack: &str,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_resuming_stage_run(&tx, &request.intent, &request.journal_run, pack)?;
        let (status, output_json, session) = match request.finish {
            ReviewStageFinish::Complete(evidence) => {
                let output=serde_json::json!({"role":evidence.role,"log_path":evidence.log_path,"sha256":evidence.sha256,"deferred":evidence.deferred,"inspected_commit":evidence.inspected_commit,"findings":evidence.findings,"verdicts":evidence.verdicts,"input_manifest":evidence.input_manifest,"produced_commit":evidence.produced_commit}).to_string();
                ("complete", output, evidence.session)
            }
            ReviewStageFinish::Error(message) => ("error", message, None),
            ReviewStageFinish::Blocked(_) => {
                return Err(HostError::Worker(
                    "continuation stage cannot be blocked after a durable claim".into(),
                ));
            }
        };
        let changed=tx.execute("UPDATE review_stage_attempts SET status=?1,output_json=?2 WHERE order_id=?3 AND run_id=?4 AND sequence=?5 AND stage_id=?6 AND status='accepted'",params![status,output_json,request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(sqlerr)?;
        if changed != 1 {
            return Err("continuation stage claim is missing or already finalized".into());
        }
        if let Some(session) = session {
            let receipt = serde_json::to_string(&SessionReceipt {
                session_id: session.session_id,
                output: session.output,
                sha256: session.sha256,
            })
            .map_err(sqlerr)?;
            tx.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![request.intent.order.0,request.journal_run,session.session_key,receipt]).map_err(sqlerr)?;
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn complete_continuation(
        &mut self,
        request: foundry_exec::ReviewContinuationCompletion,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_resuming_stage_run(
            &tx,
            &request.intent,
            &request.binding.journal_run,
            &request.pack_digest,
        )?;
        if request.binding.order_id != request.intent.order.0
            || request.binding.attempt != request.intent.attempt.0
            || request.snapshot.binding != request.binding
        {
            return Err(HostError::Worker(
                "continuation completion binding mismatch".into(),
            ));
        }
        if request.result.held.as_ref() != Some(&request.snapshot.held.hold)
            && request.result.held.is_some()
        {
            return Err(HostError::Worker(
                "continued hold snapshot does not match result".into(),
            ));
        }
        let hold_id = request
            .result
            .held
            .as_ref()
            .map(|hold| hold.hold_instance_id.as_str());
        let targets = request
            .result
            .held
            .as_ref()
            .map(|hold| serde_json::to_string(&hold.resume_targets))
            .transpose()
            .map_err(sqlerr)?;
        let deadline = if request.result.held.is_some() {
            request.deadline
        } else {
            None
        };
        let hold_json = encode_continuation_snapshot(&request.snapshot)?;
        let result_json = serde_json::to_string(&request.result).map_err(sqlerr)?;
        let counters_json = serde_json::to_string(&request.counters).map_err(sqlerr)?;
        let changed=tx.execute("UPDATE review_runs SET status='resolved',counters_json=?1,hold_json=?2,hold_instance_id=?3,resume_targets=?4,deadline=?5,result_json=?6 WHERE order_id=?7 AND attempt=?8 AND run_id=?9 AND status='resuming'",params![counters_json,hold_json,hold_id,targets,deadline,result_json,request.binding.order_id,request.binding.attempt as i64,request.binding.journal_run]).map_err(sqlerr)?;
        if changed != 1 {
            return Err("continuation run changed before completion".into());
        }
        let changed=tx.execute("UPDATE host_results SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,request.intent.order.0,request.intent.attempt.0 as i64,request.intent.run.0 as i64,request.intent.activation as i64]).map_err(sqlerr)?;
        if changed != 1 {
            return Err("continuation result binding is missing".into());
        }
        Ok(tx.commit().map_err(sqlerr)?)
    }
    fn latest_hold(
        &self,
        order: &str,
    ) -> Result<Option<foundry_exec::ReviewHoldRecord>, HostError> {
        let row=self.conn.query_row("SELECT attempt,run_id,status,hold_json,hold_instance_id,deadline FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",[order],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<i64>>(5)?.unwrap_or(0)))).optional().map_err(sqlerr).map_err(HostError::Persistence)?;
        let Some((attempt, run_id, status, hold_json, hold_id, deadline)) = row else {
            return Ok(None);
        };
        let Some(hold_json) = hold_json else {
            return Ok(None);
        };
        let attempt = u64::try_from(attempt).map_err(sqlerr)?;
        let binding = foundry_exec::ReviewRunBinding {
            order_id: order.to_owned(),
            attempt,
            journal_run: run_id,
        };
        let snapshot = decode_continuation_snapshot(binding.clone(), &hold_json)?;
        let hold_id = hold_id.unwrap_or_else(|| snapshot.held.hold.hold_instance_id.clone());
        let revision = foundry_exec::ReviewHoldRevision::from_storage_digest(
            Sha256::digest(hold_json.as_bytes()).into(),
        );
        let status = match status.as_str() {
            "held" => foundry_exec::ReviewHoldStatus::Held,
            "resuming" => foundry_exec::ReviewHoldStatus::Resuming,
            "resolved" => foundry_exec::ReviewHoldStatus::Resolved,
            "failed" => foundry_exec::ReviewHoldStatus::Failed,
            "review_aborted" => foundry_exec::ReviewHoldStatus::Aborted,
            _ => foundry_exec::ReviewHoldStatus::Other,
        };
        Ok(Some(foundry_exec::ReviewHoldRecord {
            binding,
            status,
            hold_id,
            deadline,
            revision,
            snapshot,
        }))
    }
    fn run_result(&self, order: &str, attempt: i64, run: &str) -> Result<HostResult, HostError> {
        let json:String=self.conn.query_row("SELECT result_json FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run],|r|r.get(0)).map_err(sqlerr)?;
        serde_json::from_str(&json)
            .map_err(sqlerr)
            .map_err(HostError::Persistence)
    }
    fn binding_intent(
        &self,
        intent: &ExecutionIntent,
    ) -> Result<Option<ExecutionIntent>, HostError> {
        let encoded = self
            .conn
            .query_row(
                "SELECT intent_json FROM review_bindings WHERE key_json=?1",
                [key_json(intent)],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlerr)
            .map_err(HostError::Persistence)?;
        encoded
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(sqlerr)
                    .map_err(HostError::Persistence)
            })
            .transpose()
    }
    fn claim_accepted_hold(
        &mut self,
        _session_digest: &str,
        _operation_id: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<foundry_exec::ReviewAcceptedHold, HostError> {
        Err(HostError::Worker(
            "product acceptance continuation is unavailable in standalone mode".into(),
        ))
    }
    fn finalize_accepted_hold(
        &mut self,
        _claim: foundry_exec::ReviewAcceptedHold,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        Err(HostError::Worker(
            "product acceptance finalization is unavailable in standalone mode".into(),
        ))
    }
}

pub struct PackReviewHost {
    pack_path: PathBuf,
    executor: Box<dyn StageExecutor>,
    store: Box<dyn ReviewHostStore>,
    standalone_db_path: Option<PathBuf>,
    product_store: bool,
    dispatches: u64,
    source_commit: Option<String>,
}
impl PackReviewHost {
    pub fn new(
        pack_path: impl AsRef<Path>,
        executor: impl StageExecutor + 'static,
        db_path: impl AsRef<Path>,
    ) -> Result<Self, HostError> {
        if !pack_path.as_ref().is_absolute() {
            return Err(HostError::Persistence(
                "FOUNDRY_REVIEW_PACK must be absolute".into(),
            ));
        }
        let snap =
            load_pack(pack_path.as_ref()).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != REVIEW_PACK_DIGEST {
            return Err(HostError::Persistence(format!(
                "unexpected review pack digest: {}",
                snap.digest()
            )));
        }
        let store = StandaloneReviewStore::open(db_path.as_ref(), snap.digest())
            .map_err(HostError::Persistence)?;
        let standalone_db_path = store.db_path.clone();
        Self::with_store(
            pack_path,
            executor,
            Box::new(store),
            Some(standalone_db_path),
            false,
        )
    }
    /// Construct the same frozen-pack algorithm over ProductAtomicV1 storage.
    /// Artifact, session, and OS-claim roots are explicit and remain separate
    /// from the ProductStore database path.
    pub fn with_product_store(
        pack_path: impl AsRef<Path>,
        executor: impl StageExecutor + 'static,
        mut store: foundry_ledger::ProductStore,
        artifact_root: impl AsRef<Path>,
        session_root: impl AsRef<Path>,
        claim_root: impl AsRef<Path>,
    ) -> Result<Self, HostError> {
        if !pack_path.as_ref().is_absolute() {
            return Err(HostError::Persistence(
                "FOUNDRY_REVIEW_PACK must be absolute".into(),
            ));
        }
        let snapshot =
            load_pack(pack_path.as_ref()).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snapshot.digest() != REVIEW_PACK_DIGEST {
            return Err(HostError::Persistence(format!(
                "unexpected review pack digest: {}",
                snapshot.digest()
            )));
        }
        store
            .configure_review_backend(foundry_ledger::ProductReviewConfig {
                artifact_root: artifact_root.as_ref().to_path_buf(),
                session_root: session_root.as_ref().to_path_buf(),
                claim_root: claim_root.as_ref().to_path_buf(),
                pack_digest: snapshot.digest().to_owned(),
            })
            .map_err(|e| HostError::Persistence(e.to_string()))?;
        Self::with_store(pack_path, executor, Box::new(store), None, true)
    }
    fn with_store(
        pack_path: impl AsRef<Path>,
        executor: impl StageExecutor + 'static,
        store: Box<dyn ReviewHostStore>,
        standalone_db_path: Option<PathBuf>,
        product_store: bool,
    ) -> Result<Self, HostError> {
        if !pack_path.as_ref().is_absolute() {
            return Err(HostError::Persistence(
                "FOUNDRY_REVIEW_PACK must be absolute".into(),
            ));
        }
        let snap =
            load_pack(pack_path.as_ref()).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != REVIEW_PACK_DIGEST {
            return Err(HostError::Persistence(format!(
                "unexpected review pack digest: {}",
                snap.digest()
            )));
        }
        if store.pack_digest() != snap.digest() {
            return Err(HostError::Persistence(
                "review backend pack binding mismatch".into(),
            ));
        }
        Ok(Self {
            pack_path: pack_path.as_ref().to_path_buf(),
            executor: Box::new(executor),
            store,
            standalone_db_path,
            product_store,
            dispatches: 0,
            source_commit: option_env!("FOUNDRY_SOURCE_COMMIT")
                .map(str::to_owned)
                .or_else(|| std::env::var("FOUNDRY_SOURCE_COMMIT").ok()),
        })
    }
    pub fn with_source_commit(mut self, commit: impl Into<String>) -> Result<Self, HostError> {
        let commit = commit.into();
        validate_source_commit(&commit).map_err(HostError::Persistence)?;
        self.source_commit = Some(commit);
        Ok(self)
    }
    pub fn default_db_path() -> PathBuf {
        expand_home("~/.foundry/review_host.db")
    }
    pub fn database_path(&self) -> &Path {
        self.standalone_db_path
            .as_deref()
            .expect("database_path is only defined for the standalone review backend")
    }
    /// Typed, backend-verified hold evidence. The legacy `RunRecord::hold_json`
    /// field remains available for standalone callers unchanged.
    pub fn latest_hold(
        &self,
        order: &str,
    ) -> Result<Option<foundry_exec::ReviewHoldRecord>, HostError> {
        self.store.latest_hold(order)
    }
    /// Validate that the configured runtime can start a new review stage.
    /// Product hold routes use this before accepting Resume so a missing
    /// executor cannot create a decision that appears executable.
    pub fn ensure_product_executor_available(&self) -> Result<(), HostError> {
        if !self.product_store {
            return Err(HostError::Acceptance(
                "product hold continuation requires ProductStore".into(),
            ));
        }
        self.executor.ensure_available().map_err(HostError::Worker)
    }
    pub fn resume(
        &mut self,
        order: &str,
        hold_id: &str,
        target: &str,
        actor: &str,
    ) -> Result<(), HostError> {
        <Self as DispatchHost>::resolve_hold(
            self,
            order,
            hold_id,
            foundry_exec::ReviewHoldAction::Resume {
                target: target.into(),
            },
            actor,
        )
    }
    pub fn commit_pass(
        &mut self,
        order: &str,
        hold_id: &str,
        actor: &str,
    ) -> Result<(), HostError> {
        <Self as DispatchHost>::resolve_hold(
            self,
            order,
            hold_id,
            foundry_exec::ReviewHoldAction::CommitPass,
            actor,
        )
    }
    pub fn commit_fail(
        &mut self,
        order: &str,
        hold_id: &str,
        actor: &str,
    ) -> Result<(), HostError> {
        <Self as DispatchHost>::resolve_hold(
            self,
            order,
            hold_id,
            foundry_exec::ReviewHoldAction::CommitFail,
            actor,
        )
    }
    pub fn abort(&mut self, order: &str, hold_id: &str, actor: &str) -> Result<(), HostError> {
        <Self as DispatchHost>::resolve_hold(
            self,
            order,
            hold_id,
            foundry_exec::ReviewHoldAction::Abort,
            actor,
        )
    }
    /// Continue a product hold only from an immutable decision already
    /// accepted by ProductStore. The session is resolved again by that store;
    /// caller supplied actor strings are not accepted as authority here.
    pub fn continue_accepted_product_hold(
        &mut self,
        session_digest: &str,
        operation_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        let accepted = self
            .store
            .claim_accepted_hold(session_digest, operation_id, now)?;
        match accepted.decision.clone() {
            foundry_exec::ReviewHoldAction::Resume { target } => {
                if let Err(error) = self.executor.ensure_available() {
                    let _ = self
                        .store
                        .restore_hold_after_resume_error(accepted.binding.clone());
                    return Err(HostError::Worker(error));
                }
                if let Err(error) = self.continue_held_engine(
                    &accepted.binding.order_id,
                    &accepted.binding,
                    &target,
                    accepted.snapshot.clone(),
                ) {
                    let _ = self
                        .store
                        .restore_hold_after_resume_error(accepted.binding.clone());
                    return Err(error);
                }
                Ok(())
            }
            _ => self.store.finalize_accepted_hold(accepted, now),
        }
    }
    pub fn sqlite_ports(
        path: impl AsRef<Path>,
    ) -> Result<(SqliteReviewSink, SqliteReviewLog, SqliteIntentLog), String> {
        let p = path.as_ref();
        open_db(p)?;
        let scope = Arc::new(Mutex::new(None));
        Ok((
            SqliteReviewSink {
                conn: Mutex::new(open_db(p)?),
                scope: scope.clone(),
            },
            SqliteReviewLog {
                conn: Mutex::new(open_db(p)?),
                scope: scope.clone(),
            },
            SqliteIntentLog {
                conn: Mutex::new(open_db(p)?),
                scope,
            },
        ))
    }
    /// Dispatch through the existing frozen-pack host algorithm with the
    /// exact durable ProductAtomicV1 backlog claim. The product storage owner
    /// rechecks the claim at admission and each new external stage boundary.
    pub fn try_dispatch_product_work(
        &mut self,
        intent: &ExecutionIntent,
        claim: &foundry_ledger::ProductWorkClaim,
    ) -> Result<HostResult, HostError> {
        self.try_dispatch_with_product_work(intent, Some(claim.review_binding()))
    }

    /// Run the genuine frozen-pack host and then ask ProductStore to finalize
    /// the exact terminal host receipt. The finalizer is a separate local DB
    /// transaction and is never reached after a host error.
    pub fn try_dispatch_and_finalize_product_work(
        &mut self,
        result_store: &mut foundry_ledger::ProductStore,
        service: &foundry_ledger::ProductServiceContext,
        intent: &ExecutionIntent,
        claim: &foundry_ledger::ProductWorkClaim,
    ) -> Result<(HostResult, foundry_ledger::ProductResultInfo), HostError> {
        let result = self.try_dispatch_product_work(intent, claim)?;
        let digest = foundry_ledger::product_host_result_digest(&result)
            .map_err(|_| HostError::Persistence("product host result encoding failed".into()))?;
        let expected = foundry_ledger::ProductResultExpectation {
            host_key: host_key(intent),
            host_result_digest: digest,
        };
        let info = result_store
            .finalize_product_review_result(service, &expected)
            .map_err(|error| {
                HostError::Persistence(format!(
                    "product review result finalization failed: {error:?}"
                ))
            })?;
        Ok((result, info))
    }
    fn try_dispatch_with_product_work(
        &mut self,
        intent: &ExecutionIntent,
        product_work: Option<foundry_exec::ReviewWorkClaim>,
    ) -> Result<HostResult, HostError> {
        if self.product_store && product_work.is_none() {
            return Err(HostError::Acceptance(
                "product host dispatch requires a durable work claim".into(),
            ));
        }
        if let Some(claim) = product_work.as_ref() {
            self.store
                .validate_product_work_claim(claim, intent, chrono::Utc::now())?;
        }
        let key = host_key(intent);
        let (order, attempt, base, result) = parse_external_ref(&intent.external_ref)
            .ok_or_else(|| HostError::Worker("invalid review external_ref".into()))?;
        if order != intent.order.0 || attempt != intent.attempt.0 {
            return Err(HostError::Worker("external_ref identity mismatch".into()));
        }
        validate_intent(intent)?;
        self.check_binding(intent)?;
        if let Some(r) = self.store.load_review_result(&key)? {
            return Ok(r);
        }
        let resolved_attempt = self.store.resolved_attempt(&order, attempt)?;
        if !resolved_attempt {
            self.executor.ensure_available()?;
        }
        let snap = load_pack(&self.pack_path).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != self.store.pack_digest() {
            return Err(HostError::Persistence("review pack digest changed".into()));
        }
        let digest = snap.digest().to_owned();
        let binary = provenance(&digest, self.source_commit.as_deref())?;
        let engine =
            GraphEngine::from_snapshot(&snap).map_err(|e| HostError::Persistence(e.to_string()))?;
        let admitted = self.store.admit_review_activation(ReviewAdmissionRequest {
            intent: intent.clone(),
            base_commit: base.clone(),
            result_commit: result.clone(),
            pack_digest: digest,
            product_work,
        })?;
        let run_id = admitted.journal_run;
        let inherited = admitted.inherited_counters;
        let inherited_result = admitted.inherited_result_json;
        // A resolved hold is the completed result for the newly accepted
        // activation. Recover that immutable result instead of executing the
        // review graph a second time.
        if let Some(inherited) = inherited_result {
            return serde_json::from_str(&inherited).map_err(|e| {
                HostError::Persistence(format!("invalid inherited host result: {e}"))
            });
        }
        self.dispatches += 1;
        let mut state = engine.initial_state(inherited);
        let run_root = self.store.run_root(&run_id)?;
        let mut snapshot_written = false;
        let mut sequence = 0_u64;
        let mut session_ids = BTreeSet::new();
        let mut fresh_sessions = BTreeMap::new();
        let mut accepted_logs = Vec::new();
        let mut generations = vec![foundry_exec::GenerationCommit {
            generation: 0,
            commit: result.clone(),
            fix_log_path: String::new(),
        }];
        let mut latest_commit = result.clone();

        let output = RunInput {
            verdicts: vec![],
            has_findings: false,
            budget_exhausted: false,
            timeout: false,
        };
        let outcome = engine.run_staged_checkpointed(
            &mut state,
            &output,
            10_000,
            |node_id, redrive, input, counters| {
                self.store
                    .checkpoint_review_counters(foundry_exec::ReviewCounterCheckpoint {
                        intent: intent.clone(),
                        journal_run: run_id.clone(),
                        counters: counters.clone(),
                    })
                    .map_err(stage_port_packerr)?;
                if redrive {
                    let generation = generations.len() as u64;
                    let node = snap
                        .nodes()
                        .iter()
                        .find(|n| n.id == node_id)
                        .ok_or_else(|| packerr(format!("unknown redrive node {node_id}")))?;
                    let role = if node_id == "fixdie" {
                        // T32's original fixdie node has no separate pack_stages
                        // entry; ADDENDUM 12 defines its implementation role.
                        "fix".to_owned()
                    } else {
                        let pre_stage = node
                            .value
                            .get("pre_stage")
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| {
                            packerr(format!("redrive node {node_id} has no pre_stage"))
                        })?;
                        let stage_def = snap
                            .policy()
                            .get("pack_stages")
                            .and_then(toml::Value::as_array)
                            .and_then(|stages| {
                                stages.iter().find(|stage| {
                                    stage.get("id").and_then(toml::Value::as_str) == Some(pre_stage)
                                })
                            })
                            .ok_or_else(|| {
                                packerr(format!(
                                    "redrive node {node_id} has no pre_stage definition"
                                ))
                            })?;
                        stage_def
                            .get("role")
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| packerr("redrive pre_stage missing role"))?
                            .to_owned()
                    };
                    let payload = node
                        .value
                        .get("payload")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("findings");
                    let session_id = redrive_session_key(&order, node_id);
                    let saved = self
                        .store
                        .session_receipt(&order, &session_id)
                        .map_err(host_port_packerr)?;
                    let task_session = saved
                        .as_ref()
                        .map(|r| r.session_id.clone())
                        .unwrap_or_else(|| format!("{session_id}:fresh:{run_id}"));
                    let workdir = run_root
                        .join("exec")
                        .join(format!(
                            "{}-{sequence}-{}-g{generation}",
                            intent.activation, node_id
                        ))
                        .join("0");
                    fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;
                    fs::create_dir(&workdir).map_err(packerr)?;
                    let task_session = if let Some(saved) = saved.as_ref() {
                        if session_output_valid(saved)
                            && self
                                .executor
                                .restore_session(&saved.session_id, &workdir)
                                .is_ok()
                        {
                            saved.session_id.clone()
                        } else {
                            self.store
                                .invalidate_terminal_runs(&order)
                                .map_err(host_port_packerr)?;
                            format!("{session_id}:fresh:{run_id}")
                        }
                    } else {
                        task_session
                    };
                    let mut inputs = BTreeMap::new();
                    let payload_rows = self
                        .store
                        .redrive_payload(&order, &run_id, payload)
                        .map_err(host_port_packerr)?;
                    inputs.insert(
                        payload.to_owned(),
                        serde_json::to_string(&payload_rows).map_err(packerr)?,
                    );
                    let task = StageTask {
                        stage_id: format!("{node_id}-g{generation}"),
                        role: role.clone(),
                        lens_bytes: Vec::new(),
                        session_id: task_session.clone(),
                        fix_context: Some(latest_commit.clone()),
                        base_commit: latest_commit.clone(),
                        artifact_commit: latest_commit.clone(),
                        inputs,
                        workdir,
                    };
                    self.store
                        .charge_budget(&order, intent)
                        .map_err(host_port_packerr)?;
                    let out = self
                        .executor
                        .execute(&task)
                        .and_then(|out| accept_output(&task, out))
                        .map_err(|e| foundry_pack::PackError {
                            code: "E_EXEC_REDRIVE_ERROR",
                            message: e,
                        })?;
                    let Some(commit) = out
                        .produced_commit
                        .clone()
                        .filter(|commit| !commit.is_empty())
                    else {
                        return Err(foundry_pack::PackError {
                            code: "E_EXEC_REDRIVE_ERROR",
                            message: "fix stage did not produce a commit".into(),
                        });
                    };
                    sequence += 1;
                    let output_bytes = fs::read(&out.log_path).map_err(packerr)?;
                    let stage_evidence = foundry_exec::ReviewStageEvidence {
                        role: task.role.clone(),
                        log_path: out.log_path.clone(),
                        sha256: format!("{:x}", Sha256::digest(output_bytes)),
                        inspected_commit: out.inspected_commit.clone(),
                        findings: out.findings,
                        verdicts: out
                            .verdict_category
                            .iter()
                            .chain(&out.verdict_categories)
                            .cloned()
                            .collect(),
                        input_manifest: manifest_evidence(&task),
                        deferred: false,
                        produced_commit: out.produced_commit.clone(),
                        session: None,
                    };
                    self.store
                        .save_redrive_output(
                            &order,
                            intent,
                            &run_id,
                            sequence,
                            &task.stage_id,
                            &session_id,
                            &task.session_id,
                            &stage_evidence,
                        )
                        .map_err(host_port_packerr)?;
                    accepted_logs.push(out.log_path.clone());
                    generations.push(foundry_exec::GenerationCommit {
                        generation,
                        commit: commit.clone(),
                        fix_log_path: out.log_path,
                    });
                    latest_commit = commit;
                    return Ok(input.clone());
                }
                if !snapshot_written {
                    persist_snapshot(&run_root, &snap).map_err(packerr)?;
                    snapshot_written = true;
                }
                let node = snap
                    .nodes()
                    .iter()
                    .find(|n| n.id == node_id)
                    .ok_or_else(|| packerr(format!("unknown stage node {node_id}")))?;
                let stage_values = node
                    .value
                    .get("stages")
                    .and_then(toml::Value::as_array)
                    .cloned()
                    .ok_or_else(|| packerr("dispatch has no stages"))?;
                let mut done = BTreeMap::new();
                let mut verdicts = Vec::new();
                let mut findings = false;
                let mut valid = 0;
                let mut verify_failed = false;
                for stage in stage_values {
                    let id = stage
                        .get("id")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| packerr("stage missing id"))?
                        .to_owned();
                    let role = stage
                        .get("role")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| packerr("stage missing role"))?
                        .to_owned();
                    let inputs = match sealed_inputs(&stage, &done) {
                        Ok(inputs) => inputs,
                        Err(error) => {
                            sequence += 1;
                            self.store
                                .finish_review_stage(ReviewStageFinishRequest {
                                    intent: intent.clone(),
                                    journal_run: run_id.clone(),
                                    sequence,
                                    stage_id: id.clone(),
                                    finish: ReviewStageFinish::Blocked(error.to_string()),
                                })
                                .map_err(stage_port_packerr)?;
                            verify_failed |= role == "verify";
                            continue;
                        }
                    };
                    let session = stage
                        .get("session")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(&id)
                        .to_owned();
                    let session_id = format!("{}:{}", order, session);
                    let lens_names = stage
                        .get("lenses")
                        .and_then(toml::Value::as_array)
                        .ok_or_else(|| packerr("stage missing lenses"))?;
                    let mut lens_bytes = Vec::new();
                    for name in lens_names {
                        let name = name
                            .as_str()
                            .ok_or_else(|| packerr("lens path not string"))?;
                        let b = snap
                            .files()
                            .get(name)
                            .ok_or_else(|| packerr(format!("lens not in snapshot: {name}")))?;
                        lens_bytes.extend_from_slice(b);
                        lens_bytes.push(b'\n');
                    }
                    let saved = self
                        .store
                        .session_receipt(&order, &session_id)
                        .map_err(host_port_packerr)?;
                    let saved_session = saved.is_some();
                    let restored_id = saved
                        .as_ref()
                        .map(|saved| saved.session_id.clone())
                        .unwrap_or_else(|| session_id.clone());
                    let saved_output_valid = saved.as_ref().is_none_or(|saved| {
                        fs::read(&saved.output).is_ok_and(|bytes| {
                            format!("{:x}", Sha256::digest(bytes)) == saved.sha256
                        })
                    });
                    let mut task_session = fresh_sessions
                        .get(&session_id)
                        .cloned()
                        .unwrap_or_else(|| restored_id.clone());
                    let needs_restore = session_ids.insert(session_id.clone()) && saved_session;
                    sequence += 1;
                    let workdir = run_root
                        .join("exec")
                        .join(format!("{}-{sequence}", intent.activation))
                        .join("0");
                    self.store
                        .start_review_stage(ReviewStageStart {
                            intent: intent.clone(),
                            journal_run: run_id.clone(),
                            sequence,
                            stage_id: id.clone(),
                            budget_limit: 10_000,
                        })
                        .map_err(stage_port_packerr)?;
                    fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;
                    fs::create_dir(&workdir).map_err(packerr)?;
                    if needs_restore
                        && (!saved_output_valid
                            || self
                                .executor
                                .restore_session(&restored_id, &workdir)
                                .is_err())
                    {
                        self.store
                            .invalidate_terminal_runs(&order)
                            .map_err(host_port_packerr)?;
                        task_session = format!("{order}:{session}:fresh:{run_id}");
                        fresh_sessions.insert(session_id.clone(), task_session.clone());
                    }
                    let task = StageTask {
                        stage_id: id.clone(),
                        role: role.clone(),
                        lens_bytes,
                        session_id: task_session,
                        fix_context: None,
                        base_commit: latest_commit.clone(),
                        artifact_commit: latest_commit.clone(),
                        inputs,
                        workdir,
                    };
                    let out = match self
                        .executor
                        .execute(&task)
                        .and_then(|out| accept_output(&task, out))
                    {
                        Ok(out) => out,
                        Err(error) => {
                            self.store
                                .finish_review_stage(ReviewStageFinishRequest {
                                    intent: intent.clone(),
                                    journal_run: run_id.clone(),
                                    sequence,
                                    stage_id: id.clone(),
                                    finish: ReviewStageFinish::Error(error),
                                })
                                .map_err(stage_port_packerr)?;
                            verify_failed |= role == "verify";
                            continue;
                        }
                    };
                    let is_defer = role == "verify"
                        && out
                            .verdict_category
                            .iter()
                            .chain(&out.verdict_categories)
                            .any(|v| v == "defer");
                    let output_bytes = fs::read(&out.log_path).map_err(packerr)?;
                    let output_sha = format!("{:x}", Sha256::digest(&output_bytes));
                    self.store
                        .finish_review_stage(ReviewStageFinishRequest {
                            intent: intent.clone(),
                            journal_run: run_id.clone(),
                            sequence,
                            stage_id: id.clone(),
                            finish: ReviewStageFinish::Complete(
                                foundry_exec::ReviewStageEvidence {
                                    role: role.clone(),
                                    log_path: out.log_path.clone(),
                                    sha256: output_sha.clone(),
                                    inspected_commit: out.inspected_commit.clone(),
                                    findings: out.findings,
                                    verdicts: out
                                        .verdict_category
                                        .iter()
                                        .chain(&out.verdict_categories)
                                        .cloned()
                                        .collect(),
                                    input_manifest: manifest_evidence(&task),
                                    deferred: is_defer,
                                    produced_commit: out.produced_commit.clone(),
                                    session: Some(foundry_exec::ReviewSessionEvidence {
                                        session_key: session_id.clone(),
                                        session_id: task.session_id.clone(),
                                        output: out.log_path.clone(),
                                        sha256: output_sha,
                                    }),
                                },
                            ),
                        })
                        .map_err(stage_port_packerr)?;
                    valid += 1;
                    done.insert(id, out.log_path.clone());
                    accepted_logs.push(out.log_path.clone());
                    if role == "verify" {
                        verdicts.extend(out.verdict_category);
                        verdicts.extend(out.verdict_categories);
                        findings = out.findings
                            || verdicts
                                .iter()
                                .any(|v| matches!(v.as_str(), "patch" | "intent_gap" | "bad_plan"));
                    }
                }
                if valid == 0 {
                    return Err(foundry_pack::PackError {
                        code: "E_EXEC_ALL_ERROR",
                        message: "no valid stage output".into(),
                    });
                }
                if verify_failed {
                    return Err(foundry_pack::PackError {
                        code: "E_EXEC_VERIFY_INVALID",
                        message: "verify output is invalid".into(),
                    });
                }
                Ok(RunInput {
                    verdicts,
                    has_findings: findings,
                    budget_exhausted: input.budget_exhausted,
                    timeout: input.timeout,
                })
            },
        );
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.store
                    .fail_review_dispatch(foundry_exec::ReviewDispatchFailure {
                        intent: intent.clone(),
                        journal_run: run_id.clone(),
                        counters: state.counters.clone(),
                        error: error.to_string(),
                    })?;
                return Err(HostError::Worker(error.to_string()));
            }
        };
        let passed = outcome.terminal
            && outcome
                .actions
                .iter()
                .any(|a| matches!(a,EngineAction::Emit{event,..} if event=="InspectionPassed"));
        let hold_state = outcome.hold.as_ref().map(|entry| {
            let node = snap.nodes().iter().find(|n| n.id == entry.node_id);
            let resume_targets = node
                .and_then(|n| n.value.get("resume_targets"))
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect();
            let deadline_secs = snap
                .policy()
                .get("hold")
                .and_then(|v| v.get("timeout_s"))
                .and_then(toml::Value::as_integer)
                .unwrap_or(0)
                .max(0) as u64;
            foundry_exec::host::HoldState {
                hold_instance_id: new_hold_id(),
                reason: entry.node_id.clone(),
                resume_targets,
                deadline_secs,
            }
        });
        let exit_code = if passed { 0 } else { 1 };
        let log_path = accepted_logs
            .last()
            .cloned()
            .unwrap_or_else(|| format!("{}#{}", self.pack_path.display(), run_id));
        validate_tuple_seal(
            generations
                .last()
                .map(|generation| generation.commit.as_str())
                .unwrap_or(&latest_commit),
            &latest_commit,
        )?;
        let host = HostResult {
            base_commit: base,
            result_commit: latest_commit.clone(),
            log_path,
            exit_code,
            generations: generations.clone(),
            held: hold_state.clone(),
        };
        let held_snapshot = hold_state
            .as_ref()
            .zip(outcome.hold.as_ref())
            .map(|(hold, entry)| foundry_exec::ReviewHeldSnapshot {
                hold: hold.clone(),
                node_id: entry.node_id.clone(),
                entry_counters: entry.entry_counters.clone(),
                requested_revert: entry.reverted_base.clone(),
                engine_state: state.export(),
                session_ids: session_ids.iter().cloned().collect(),
                accepted_logs: accepted_logs.clone(),
                generations: generations.clone(),
                latest_commit: latest_commit.clone(),
                sequence,
            });
        let deadline = host.held.as_ref().map(|hold| {
            if hold.deadline_secs == 0 {
                0
            } else {
                chrono::Utc::now()
                    .timestamp()
                    .saturating_add(hold.deadline_secs as i64)
            }
        });
        self.store
            .complete_review_dispatch(foundry_exec::ReviewDispatchCompletion {
                intent: intent.clone(),
                journal_run: run_id.clone(),
                status: if passed {
                    foundry_exec::ReviewTerminalStatus::Passed
                } else if outcome.hold.is_some() {
                    foundry_exec::ReviewTerminalStatus::Held
                } else {
                    foundry_exec::ReviewTerminalStatus::Failed
                },
                counters: state.counters.clone(),
                result: host.clone(),
                held_snapshot,
                provenance: foundry_exec::ReviewBinaryProvenance {
                    executable: binary.executable,
                    sha256: binary.sha256,
                    source_commit: binary.source_commit,
                    pack_digest: binary.pack_digest,
                },
                deadline,
            })?;
        Ok(host)
    }
}
impl DispatchHost for PackReviewHost {
    type Result = HostResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> HostResult {
        self.try_dispatch(intent)
            .unwrap_or_else(|e| panic!("review host: {e:?}"))
    }
    fn try_dispatch(&mut self, intent: &ExecutionIntent) -> Result<HostResult, HostError> {
        self.try_dispatch_with_product_work(intent, None)
    }
    fn dispatch_count(&self) -> u64 {
        self.dispatches
    }
    fn accepted(&self, intent: &ExecutionIntent) -> bool {
        self.store
            .accepted(intent)
            .unwrap_or_else(|e| panic!("host acceptance read failed: {e:?}"))
    }
    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<HostResult> {
        self.check_binding(intent)
            .unwrap_or_else(|e| panic!("host recovery binding: {e:?}"));
        self.store
            .load_review_result(&host_key(intent))
            .unwrap_or_else(|e| panic!("host result recovery failed: {e:?}"))
    }
    fn latest_run(
        &self,
        order: &str,
        attempt: u64,
    ) -> Result<Option<foundry_exec::RunRecord>, HostError> {
        self.store.latest_run(order, attempt)
    }
    fn resolve_hold(
        &mut self,
        order: &str,
        hold_id: &str,
        action: foundry_exec::ReviewHoldAction,
        actor: &str,
    ) -> Result<(), HostError> {
        let record = self
            .store
            .latest_hold(order)?
            .ok_or_else(|| HostError::Worker("review hold not found".into()))?;
        if record.status != foundry_exec::ReviewHoldStatus::Held || record.hold_id != hold_id {
            return Err(HostError::Worker(
                "review hold is stale or already consumed".into(),
            ));
        }
        let expected = foundry_exec::ReviewHoldExpectation {
            binding: record.binding.clone(),
            hold_id: record.hold_id.clone(),
            deadline: record.deadline,
            revision: record.revision.clone(),
        };
        let now = chrono::Utc::now().timestamp();
        let mut snapshot = record.snapshot;
        if record.deadline > 0 && now >= record.deadline {
            snapshot.human_events.push(foundry_exec::ReviewHumanEvent {
                actor: actor.to_owned(),
                occurred_at: chrono::Utc::now().to_rfc3339(),
                action: foundry_exec::ReviewHumanAction::DeadlineExpired,
            });
            self.store
                .mark_hold_expired(foundry_exec::ReviewHoldUpdate {
                    expected,
                    now,
                    snapshot,
                })?;
            return Err(HostError::Worker(
                "human review hold deadline expired".into(),
            ));
        }
        if let foundry_exec::ReviewHoldAction::Resume { target } = &action {
            if !snapshot
                .held
                .hold
                .resume_targets
                .iter()
                .any(|candidate| candidate == target)
            {
                return Err(HostError::Worker("resume target is not allowed".into()));
            }
            self.executor.ensure_available()?;
            // This OS lock survives independent DB connections and is released
            // by the kernel on process death. Startup must not steal a live CAS.
            let _claim = self
                .store
                .claim_resume(&record.binding.journal_run)?
                .ok_or_else(|| HostError::Worker("review continuation is still running".into()))?;
            snapshot.resume_sequence = snapshot
                .resume_sequence
                .checked_add(1)
                .ok_or_else(|| HostError::Persistence("resume sequence exhausted".into()))?;
            snapshot.human_events.push(foundry_exec::ReviewHumanEvent {
                actor: actor.to_owned(),
                occurred_at: chrono::Utc::now().to_rfc3339(),
                action: foundry_exec::ReviewHumanAction::Resume {
                    target: target.clone(),
                },
            });
            self.store
                .begin_hold_resume(foundry_exec::ReviewHoldUpdate {
                    expected,
                    now,
                    snapshot: snapshot.clone(),
                })?;
            if let Err(error) = self.continue_held_engine(order, &record.binding, target, snapshot)
            {
                let _ = self
                    .store
                    .restore_hold_after_resume_error(record.binding.clone());
                return Err(error);
            }
            return Ok(());
        }
        snapshot.human_events.push(foundry_exec::ReviewHumanEvent {
            actor: actor.to_owned(),
            occurred_at: chrono::Utc::now().to_rfc3339(),
            action: match action {
                foundry_exec::ReviewHoldAction::CommitPass => {
                    foundry_exec::ReviewHumanAction::CommitPass
                }
                foundry_exec::ReviewHoldAction::CommitFail => {
                    foundry_exec::ReviewHumanAction::CommitFail
                }
                foundry_exec::ReviewHoldAction::Abort => foundry_exec::ReviewHumanAction::Abort,
                foundry_exec::ReviewHoldAction::Resume { .. } => unreachable!(),
            },
        });
        let mut result: HostResult = self.store.run_result(
            order,
            record.binding.attempt as i64,
            &record.binding.journal_run,
        )?;
        result.held = None;
        result.exit_code = if matches!(action, foundry_exec::ReviewHoldAction::CommitPass) {
            0
        } else {
            1
        };
        self.store
            .resolve_hold_result(foundry_exec::ReviewHoldResolution {
                expected,
                now,
                action,
                snapshot,
                result,
            })?;
        Ok(())
    }
}
impl PackReviewHost {
    fn continue_held_engine(
        &mut self,
        order: &str,
        binding: &foundry_exec::ReviewRunBinding,
        target: &str,
        mut continuation: foundry_exec::ReviewContinuationSnapshot,
    ) -> Result<(), HostError> {
        let intent = continuation.intent.clone();
        let run_id = &binding.journal_run;
        let attempt = binding.attempt as i64;
        let mut state = foundry_pack::EngineState::import(&continuation.held.engine_state)?;
        let run_root = self.store.run_root(run_id)?;
        let snapshot_root = self.store.run_root(&continuation.snapshot_run_id)?;
        let snap = load_pack(snapshot_root.join("pack-snapshot"))
            .map_err(|e| HostError::Persistence(e.to_string()))?;
        let continuation_binding = self.store.continuation_binding(order, attempt, run_id)?;
        let expected_digest = continuation_binding.pack_digest;
        let mut sequence = continuation_binding.next_sequence;
        let stored_result = continuation_binding.stored_result;
        if snap.digest() != expected_digest || snap.digest() != self.store.pack_digest() {
            return Err(HostError::Persistence(
                "held pack snapshot digest changed".into(),
            ));
        }
        let engine =
            GraphEngine::from_snapshot(&snap).map_err(|e| HostError::Persistence(e.to_string()))?;
        let mut generations = continuation.held.generations.clone();
        let mut latest_commit = continuation.held.latest_commit.clone();
        let mut accepted_logs = continuation.held.accepted_logs.clone();
        let mut session_ids = BTreeSet::new();
        let mut fresh_sessions = BTreeMap::new();
        let resume_sequence = continuation.resume_sequence;
        let execution_root = run_root
            .join("exec")
            .join(format!("resume-{resume_sequence}"));
        let outcome = engine
            .resume_staged(&mut state, target, |node_id, redrive, input, counters| {
                self.store
                    .continuation_counters(
                        foundry_exec::ReviewCounterCheckpoint {
                            intent: intent.clone(),
                            journal_run: run_id.to_owned(),
                            counters: counters.clone(),
                        },
                        &expected_digest,
                    )
                    .map_err(host_port_packerr)?;
                if redrive {
                    let node = snap
                        .nodes()
                        .iter()
                        .find(|n| n.id == node_id)
                        .ok_or_else(|| packerr(format!("unknown redrive node {node_id}")))?;
                    let role = if node_id == "fixdie" {
                        "fix"
                    } else {
                        let pre = node
                            .value
                            .get("pre_stage")
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| packerr("redrive pre_stage missing"))?;
                        snap.policy()
                            .get("pack_stages")
                            .and_then(toml::Value::as_array)
                            .and_then(|a| {
                                a.iter().find(|s| {
                                    s.get("id").and_then(toml::Value::as_str) == Some(pre)
                                })
                            })
                            .and_then(|s| s.get("role"))
                            .and_then(toml::Value::as_str)
                            .ok_or_else(|| packerr("redrive pre_stage role missing"))?
                    }
                    .to_owned();
                    let generation = generations.len() as u64;
                    let payload = node
                        .value
                        .get("payload")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("findings");
                    let mut inputs = BTreeMap::new();
                    let values = self
                        .store
                        .redrive_payload(order, run_id, payload)
                        .map_err(host_port_packerr)?;
                    inputs.insert(
                        payload.to_owned(),
                        serde_json::to_string(&values).map_err(packerr)?,
                    );
                    sequence += 1;
                    let workdir = execution_root
                        .join(format!("{}-{node_id}-g{generation}", intent.activation))
                        .join("0");
                    fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;
                    fs::create_dir(&workdir).map_err(packerr)?;
                    let session_key = redrive_session_key(order, node_id);
                    let saved = self
                        .store
                        .continuation_session(order, &session_key)
                        .map_err(host_port_packerr)?;
                    let task_session = if let Some(saved) = saved.as_ref() {
                        if session_output_valid(saved)
                            && self
                                .executor
                                .restore_session(&saved.session_id, &workdir)
                                .is_ok()
                        {
                            saved.session_id.clone()
                        } else {
                            self.store
                                .invalidate_terminal_runs(order)
                                .map_err(host_port_packerr)?;
                            format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}")
                        }
                    } else {
                        format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}")
                    };
                    let task = StageTask {
                        stage_id: format!("{node_id}-g{generation}"),
                        role: role.clone(),
                        lens_bytes: Vec::new(),
                        session_id: task_session,
                        fix_context: Some(latest_commit.clone()),
                        base_commit: latest_commit.clone(),
                        artifact_commit: latest_commit.clone(),
                        inputs,
                        workdir,
                    };
                    self.store
                        .continuation_budget_and_start(
                            ReviewStageStart {
                                intent: intent.clone(),
                                journal_run: run_id.to_owned(),
                                sequence,
                                stage_id: task.stage_id.clone(),
                                budget_limit: 10_000,
                            },
                            &expected_digest,
                        )
                        .map_err(host_port_packerr)?;
                    let out = self
                        .executor
                        .execute(&task)
                        .and_then(|o| accept_output(&task, o))
                        .map_err(|e| foundry_pack::PackError {
                            code: "E_EXEC_REDRIVE_ERROR",
                            message: e,
                        })?;
                    let Some(commit) = out.produced_commit.clone().filter(|c| !c.is_empty()) else {
                        return Err(foundry_pack::PackError {
                            code: "E_EXEC_REDRIVE_ERROR",
                            message: "redrive stage produced no commit".into(),
                        });
                    };
                    let output_sha = format!(
                        "{:x}",
                        Sha256::digest(fs::read(&out.log_path).map_err(packerr)?)
                    );
                    let manifest = manifest_evidence(&task);
                    let evidence = foundry_exec::ReviewStageEvidence {
                        role: role.clone(),
                        log_path: out.log_path.clone(),
                        sha256: output_sha.clone(),
                        inspected_commit: out.inspected_commit.clone(),
                        findings: out.findings,
                        verdicts: out
                            .verdict_category
                            .iter()
                            .chain(&out.verdict_categories)
                            .cloned()
                            .collect(),
                        input_manifest: manifest,
                        deferred: false,
                        produced_commit: Some(commit.clone()),
                        session: Some(foundry_exec::ReviewSessionEvidence {
                            session_key: session_key.clone(),
                            session_id: task.session_id.clone(),
                            output: out.log_path.clone(),
                            sha256: output_sha,
                        }),
                    };
                    self.store
                        .finish_continuation_stage(
                            ReviewStageFinishRequest {
                                intent: intent.clone(),
                                journal_run: run_id.to_owned(),
                                sequence,
                                stage_id: task.stage_id.clone(),
                                finish: ReviewStageFinish::Complete(evidence),
                            },
                            &expected_digest,
                        )
                        .map_err(host_port_packerr)?;
                    accepted_logs.push(out.log_path.clone());
                    generations.push(foundry_exec::GenerationCommit {
                        generation,
                        commit: commit.clone(),
                        fix_log_path: out.log_path,
                    });
                    latest_commit = commit;
                    return Ok(input.clone());
                }
                let node = snap
                    .nodes()
                    .iter()
                    .find(|n| n.id == node_id)
                    .ok_or_else(|| packerr(format!("unknown dispatch node {node_id}")))?;
                let stages = node
                    .value
                    .get("stages")
                    .and_then(toml::Value::as_array)
                    .cloned()
                    .ok_or_else(|| packerr("dispatch has no stages"))?;
                let mut done = BTreeMap::new();
                let mut verdicts = Vec::new();
                let mut findings = false;
                let mut valid = 0usize;
                let mut verify_failed = false;
                for stage in stages {
                    let id = stage
                        .get("id")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| packerr("stage missing id"))?
                        .to_owned();
                    let role = stage
                        .get("role")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| packerr("stage missing role"))?
                        .to_owned();
                    let inputs = match sealed_inputs(&stage, &done) {
                        Ok(v) => v,
                        Err(_) => {
                            verify_failed |= role == "verify";
                            continue;
                        }
                    };
                    let lens_names = stage
                        .get("lenses")
                        .and_then(toml::Value::as_array)
                        .ok_or_else(|| packerr("stage missing lenses"))?;
                    let mut lens_bytes = Vec::new();
                    for name in lens_names {
                        let name = name
                            .as_str()
                            .ok_or_else(|| packerr("lens path not string"))?;
                        lens_bytes.extend_from_slice(
                            snap.files()
                                .get(name)
                                .ok_or_else(|| packerr("lens absent from snapshot"))?,
                        );
                        lens_bytes.push(b'\n');
                    }
                    sequence += 1;
                    let workdir = execution_root
                        .join(format!("{}-{}", intent.activation, sequence))
                        .join("0");
                    fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;
                    fs::create_dir(&workdir).map_err(packerr)?;
                    let session = stage
                        .get("session")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(&id);
                    let session_key = format!("{order}:{session}");
                    let saved = self
                        .store
                        .continuation_session(order, &session_key)
                        .map_err(host_port_packerr)?;
                    let mut task_session = fresh_sessions
                        .get(&session_key)
                        .cloned()
                        .unwrap_or_else(|| {
                            saved
                                .as_ref()
                                .map(|s| s.session_id.clone())
                                .unwrap_or_else(|| session_key.clone())
                        });
                    if session_ids.insert(session_key.clone())
                        && let Some(saved) = saved.as_ref()
                        && (!session_output_valid(saved)
                            || self
                                .executor
                                .restore_session(&saved.session_id, &workdir)
                                .is_err())
                    {
                        self.store
                            .invalidate_terminal_runs(order)
                            .map_err(host_port_packerr)?;
                        task_session =
                            format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}");
                        fresh_sessions.insert(session_key.clone(), task_session.clone());
                    }
                    let task = StageTask {
                        stage_id: id.clone(),
                        role: role.clone(),
                        lens_bytes,
                        session_id: task_session,
                        fix_context: None,
                        base_commit: latest_commit.clone(),
                        artifact_commit: latest_commit.clone(),
                        inputs,
                        workdir,
                    };
                    self.store
                        .continuation_budget_and_start(
                            ReviewStageStart {
                                intent: intent.clone(),
                                journal_run: run_id.to_owned(),
                                sequence,
                                stage_id: id.clone(),
                                budget_limit: 10_000,
                            },
                            &expected_digest,
                        )
                        .map_err(host_port_packerr)?;
                    let output = match self
                        .executor
                        .execute(&task)
                        .and_then(|o| accept_output(&task, o))
                    {
                        Ok(o) => o,
                        Err(error) => {
                            self.store
                                .finish_continuation_stage(
                                    ReviewStageFinishRequest {
                                        intent: intent.clone(),
                                        journal_run: run_id.to_owned(),
                                        sequence,
                                        stage_id: id.clone(),
                                        finish: ReviewStageFinish::Error(error),
                                    },
                                    &expected_digest,
                                )
                                .map_err(host_port_packerr)?;
                            verify_failed |= role == "verify";
                            continue;
                        }
                    };
                    let categories: Vec<String> = output
                        .verdict_category
                        .iter()
                        .chain(&output.verdict_categories)
                        .cloned()
                        .collect();
                    let output_sha = format!(
                        "{:x}",
                        Sha256::digest(fs::read(&output.log_path).map_err(packerr)?)
                    );
                    let evidence = foundry_exec::ReviewStageEvidence {
                        role: role.clone(),
                        log_path: output.log_path.clone(),
                        sha256: output_sha.clone(),
                        inspected_commit: output.inspected_commit.clone(),
                        findings: output.findings,
                        verdicts: categories.clone(),
                        input_manifest: manifest_evidence(&task),
                        deferred: false,
                        produced_commit: output.produced_commit.clone(),
                        session: Some(foundry_exec::ReviewSessionEvidence {
                            session_key: session_key.clone(),
                            session_id: task.session_id.clone(),
                            output: output.log_path.clone(),
                            sha256: output_sha,
                        }),
                    };
                    self.store
                        .finish_continuation_stage(
                            ReviewStageFinishRequest {
                                intent: intent.clone(),
                                journal_run: run_id.to_owned(),
                                sequence,
                                stage_id: id.clone(),
                                finish: ReviewStageFinish::Complete(evidence),
                            },
                            &expected_digest,
                        )
                        .map_err(host_port_packerr)?;
                    valid += 1;
                    done.insert(id, output.log_path.clone());
                    accepted_logs.push(output.log_path);
                    if role == "verify" {
                        findings |= output.findings
                            || categories
                                .iter()
                                .any(|v| matches!(v.as_str(), "patch" | "intent_gap" | "bad_plan"));
                        verdicts.extend(categories);
                    }
                }
                if valid == 0 {
                    return Err(foundry_pack::PackError {
                        code: "E_EXEC_ALL_ERROR",
                        message: "no valid stage output".into(),
                    });
                }
                if verify_failed {
                    return Err(foundry_pack::PackError {
                        code: "E_EXEC_VERIFY_INVALID",
                        message: "verify output invalid".into(),
                    });
                }
                Ok(RunInput {
                    verdicts,
                    has_findings: findings,
                    budget_exhausted: input.budget_exhausted,
                    timeout: input.timeout,
                })
            })
            .map_err(|e| HostError::Worker(e.to_string()))?;
        let passed = outcome.terminal
            && outcome
                .actions
                .iter()
                .any(|a| matches!(a,EngineAction::Emit{event,..}if event=="InspectionPassed"));
        let hold = outcome.hold.as_ref().map(|entry| {
            let targets = snap
                .nodes()
                .iter()
                .find(|n| n.id == entry.node_id)
                .and_then(|n| n.value.get("resume_targets"))
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect();
            foundry_exec::host::HoldState {
                hold_instance_id: new_hold_id(),
                reason: entry.node_id.clone(),
                resume_targets: targets,
                deadline_secs: snap
                    .policy()
                    .get("hold")
                    .and_then(|v| v.get("timeout_s"))
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0)
                    .max(0) as u64,
            }
        });
        let mut result: HostResult = stored_result;
        result.result_commit = latest_commit;
        result.generations = generations;
        result.log_path = accepted_logs.last().cloned().unwrap_or(result.log_path);
        result.exit_code = if passed { 0 } else { 1 };
        result.held = hold.clone();
        continuation.held.engine_state = state.export();
        continuation.held.generations = result.generations.clone();
        continuation.held.latest_commit = result.result_commit.clone();
        continuation.held.accepted_logs = accepted_logs;
        continuation.held.sequence = sequence;
        if let (Some(new_hold), Some(entry)) = (hold.as_ref(), outcome.hold.as_ref()) {
            continuation.held.hold = new_hold.clone();
            continuation.held.node_id = entry.node_id.clone();
            continuation.held.entry_counters = entry.entry_counters.clone();
            continuation.held.requested_revert = entry.reverted_base.clone();
        }
        // The consumed hold is resolved; the runner transfers a newly raised hold
        // to its next run without publishing a tuple.
        let deadline = hold.as_ref().and_then(|h| {
            (h.deadline_secs != 0).then(|| {
                chrono::Utc::now()
                    .timestamp()
                    .saturating_add(h.deadline_secs as i64)
            })
        });
        self.store
            .complete_continuation(foundry_exec::ReviewContinuationCompletion {
                binding: binding.clone(),
                intent,
                pack_digest: expected_digest,
                counters: state.counters.clone(),
                snapshot: continuation,
                result,
                deadline,
            })?;
        Ok(())
    }

    fn check_binding(&self, intent: &ExecutionIntent) -> Result<(), HostError> {
        let saved = self.store.binding_intent(intent)?;
        if let Some(old) = saved {
            if old.external_ref != intent.external_ref
                || old.epoch != intent.epoch
                || old.generation != intent.generation
            {
                return Err(HostError::Worker("activation binding mismatch".into()));
            }
        } else if self.accepted(intent) {
            return Err(HostError::Persistence(
                "accepted activation lacks binding evidence".into(),
            ));
        }
        Ok(())
    }
}

type SessionReceipt = foundry_exec::ReviewSessionReceipt;

fn check_hold_expectation(
    tx: &rusqlite::Transaction<'_>,
    expected: &foundry_exec::ReviewHoldExpectation,
    status: &str,
) -> Result<(String, String, i64), HostError> {
    let row: (String,String,i64)=tx.query_row("SELECT hold_json,hold_instance_id,deadline FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND status=?4",params![expected.binding.order_id,expected.binding.attempt as i64,expected.binding.journal_run,status],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sqlerr)?;
    let digest: [u8; 32] = Sha256::digest(row.0.as_bytes()).into();
    if row.1 != expected.hold_id || digest != *expected.revision.as_bytes() {
        return Err(HostError::Worker("review hold snapshot changed".into()));
    }
    Ok(row)
}

fn validate_hold_update(request: &foundry_exec::ReviewHoldUpdate) -> Result<(), HostError> {
    if request.snapshot.binding != request.expected.binding
        || request.snapshot.held.hold.hold_instance_id != request.expected.hold_id
    {
        return Err(HostError::Worker(
            "hold update snapshot binding mismatch".into(),
        ));
    }
    Ok(())
}

fn decode_continuation_snapshot(
    binding: foundry_exec::ReviewRunBinding,
    encoded: &str,
) -> Result<foundry_exec::ReviewContinuationSnapshot, HostError> {
    let doc: serde_json::Value = serde_json::from_str(encoded).map_err(sqlerr)?;
    let hold: foundry_exec::host::HoldState =
        serde_json::from_value(doc["hold"].clone()).map_err(sqlerr)?;
    let intent: ExecutionIntent = serde_json::from_value(doc["intent"].clone()).map_err(sqlerr)?;
    let node_id = doc["entry"]["node_id"]
        .as_str()
        .ok_or("held snapshot has no engine entry node")?
        .to_owned();
    let entry_counters =
        serde_json::from_value(doc["entry"]["entry_counters"].clone()).map_err(sqlerr)?;
    let requested_revert = doc["entry"]["reverted_base"].as_str().map(str::to_owned);
    let snapshot_run_id = doc
        .get("snapshot_run_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&binding.journal_run)
        .to_owned();
    let resume_sequence = doc
        .get("resume_sequence")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let human_events = doc
        .get("human_events")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|event| {
            let name = event
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let target = event
                .get("target")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            let action = match name {
                "resume" => foundry_exec::ReviewHumanAction::Resume {
                    target: target.clone().ok_or("resume event lacks target")?,
                },
                "commit_pass" => foundry_exec::ReviewHumanAction::CommitPass,
                "commit_fail" => foundry_exec::ReviewHumanAction::CommitFail,
                "abort" => foundry_exec::ReviewHumanAction::Abort,
                "human_deadline_expired" => foundry_exec::ReviewHumanAction::DeadlineExpired,
                other => foundry_exec::ReviewHumanAction::Other {
                    name: other.to_owned(),
                    target,
                },
            };
            Ok(foundry_exec::ReviewHumanEvent {
                actor: event
                    .get("actor")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                occurred_at: event
                    .get("at")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                action,
            })
        })
        .collect::<Result<Vec<_>, HostError>>()?;
    let held = foundry_exec::ReviewHeldSnapshot {
        hold,
        node_id,
        entry_counters,
        requested_revert,
        engine_state: doc["engine_state"]
            .as_str()
            .ok_or("held snapshot has no engine state")?
            .to_owned(),
        session_ids: serde_json::from_value(doc["session_ids"].clone()).map_err(sqlerr)?,
        accepted_logs: serde_json::from_value(doc["accepted_logs"].clone()).map_err(sqlerr)?,
        generations: serde_json::from_value(doc["generations"].clone()).map_err(sqlerr)?,
        latest_commit: doc["latest_commit"]
            .as_str()
            .ok_or("held snapshot has no latest commit")?
            .to_owned(),
        sequence: doc["sequence"]
            .as_u64()
            .ok_or("held snapshot has no sequence")?,
    };
    if intent.order.0 != binding.order_id || intent.attempt.0 != binding.attempt {
        return Err(HostError::Persistence(
            "hold snapshot binding coordinates mismatch".into(),
        ));
    }
    Ok(foundry_exec::ReviewContinuationSnapshot {
        binding,
        snapshot_run_id,
        held,
        intent,
        resume_sequence,
        human_events,
    })
}

fn encode_continuation_snapshot(
    snapshot: &foundry_exec::ReviewContinuationSnapshot,
) -> Result<String, HostError> {
    let held = &snapshot.held;
    let events = snapshot
        .human_events
        .iter()
        .map(|event| {
            let (action, target) = match &event.action {
                foundry_exec::ReviewHumanAction::Resume { target } => {
                    ("resume", Some(target.clone()))
                }
                foundry_exec::ReviewHumanAction::CommitPass => ("commit_pass", None),
                foundry_exec::ReviewHumanAction::CommitFail => ("commit_fail", None),
                foundry_exec::ReviewHumanAction::Abort => ("abort", None),
                foundry_exec::ReviewHumanAction::DeadlineExpired => {
                    ("human_deadline_expired", None)
                }
                foundry_exec::ReviewHumanAction::Other { name, target } => {
                    (name.as_str(), target.clone())
                }
            };
            let mut value =
                serde_json::json!({"actor":event.actor,"at":event.occurred_at,"action":action});
            if let Some(target) = target {
                value["target"] = serde_json::json!(target);
            }
            value
        })
        .collect::<Vec<_>>();
    let document = serde_json::json!({
        "hold":held.hold,"entry":{"node_id":held.node_id,"entry_counters":held.entry_counters,"reverted_base":held.requested_revert},
        "engine_state":held.engine_state,"intent":snapshot.intent,"run_id":snapshot.binding.journal_run,
        "snapshot_run_id":snapshot.snapshot_run_id,"session_ids":held.session_ids,"accepted_logs":held.accepted_logs,
        "generations":held.generations,"latest_commit":held.latest_commit,"sequence":held.sequence,
        "resume_sequence":snapshot.resume_sequence,"human_events":events
    });
    serde_json::to_string(&document)
        .map_err(sqlerr)
        .map_err(HostError::Persistence)
}

fn resume_claim(db: &Path, run_id: &str) -> Result<Option<fs::File>, String> {
    let directory = db.with_extension("review-claims");
    fs::create_dir_all(&directory).map_err(sqlerr)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join(format!("{:x}.lock", Sha256::digest(run_id.as_bytes()))))
        .map_err(sqlerr)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(sqlerr(error)),
    }
}
fn recover_resuming(conn: &mut Connection, db: &Path) -> Result<(), String> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlerr)?;
    let rows = {
        let mut query = tx
            .prepare("SELECT order_id,run_id FROM review_runs WHERE status='resuming'")
            .map_err(sqlerr)?;
        query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sqlerr)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlerr)?
    };
    for (order, run) in rows {
        if let Some(_claim) = resume_claim(db, &run)? {
            tx.execute("UPDATE review_runs SET status='held' WHERE order_id=?1 AND run_id=?2 AND status='resuming'",params![order,run]).map_err(sqlerr)?;
        }
    }
    tx.commit().map_err(sqlerr)
}

fn redrive_session_key(order: &str, node: &str) -> String {
    format!("{order}:{}", if node == "fixdie" { "die" } else { node })
}
fn session_output_valid(saved: &SessionReceipt) -> bool {
    fs::read(&saved.output)
        .is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == saved.sha256)
}
fn manifest_evidence(task: &StageTask) -> Option<foundry_exec::ReviewInputManifest> {
    fs::read(task.workdir.join("input-manifest.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}
fn validate_tuple_seal(immutable_commit: &str, final_head: &str) -> Result<(), String> {
    if immutable_commit != final_head {
        return Err(
            "tuple immutable_commit does not match the final generation head (BindingMismatch)"
                .into(),
        );
    }
    Ok(())
}
// Select only the latest completed occurrence of each requested artifact.
fn redrive_payload(
    conn: &Connection,
    order: &str,
    run: &str,
    payload: &str,
) -> foundry_pack::Result<Vec<foundry_exec::ReviewRedriveArtifact>> {
    let mut query=conn.prepare("SELECT stage_id,status,output_json FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2 ORDER BY sequence DESC").map_err(packerr)?;
    let rows = query
        .query_map(params![order, run], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(packerr)?;
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    for row in rows {
        let (stage, status, json) = row.map_err(packerr)?;
        if !seen.insert(stage.clone()) || status != "complete" {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(&json.ok_or_else(|| packerr("complete stage lacks output"))?)
                .map_err(packerr)?;
        let matches = if payload == "findings" {
            value["role"] == "verify" && value["findings"] == true
        } else {
            stage == payload
        };
        if matches {
            let verdicts = serde_json::from_value(value["verdicts"].clone()).unwrap_or_default();
            let log_path = value["log_path"]
                .as_str()
                .ok_or_else(|| packerr("completed stage lacks a log path"))?
                .to_owned();
            values.push(foundry_exec::ReviewRedriveArtifact {
                stage_id: stage,
                verdicts,
                log_path,
            });
        }
    }
    values.reverse();
    Ok(values)
}

fn sealed_inputs(
    stage: &toml::Value,
    done: &BTreeMap<String, String>,
) -> foundry_pack::Result<BTreeMap<String, String>> {
    let deps = match stage.get("after") {
        None => vec![],
        Some(toml::Value::String(s)) => vec![s.clone()],
        Some(toml::Value::Array(values)) => values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| packerr("dependency must be string"))
            })
            .collect::<foundry_pack::Result<Vec<_>>>()?,
        _ => return Err(packerr("invalid stage dependencies")),
    };
    deps.into_iter()
        .map(|id| {
            let path = done.get(&id).ok_or_else(|| {
                packerr(format!(
                    "stage seal violation: predecessor {id} is incomplete"
                ))
            })?;
            Ok((id, path.clone()))
        })
        .collect()
}

fn accept_output(task: &StageTask, mut out: StageOutput) -> Result<StageOutput, String> {
    let path = fs::canonicalize(&out.log_path).map_err(sqlerr)?;
    let namespace = fs::canonicalize(&task.workdir).map_err(sqlerr)?;
    if !path.starts_with(namespace) {
        return Err("stage output escaped its execution namespace".into());
    }
    let bytes = fs::read(&path).map_err(sqlerr)?;
    if bytes.is_empty() {
        return Err("stage output is empty".into());
    }
    if task.role == "verify" {
        if out.produced_commit.is_some() {
            return Err("verify stage returned a produced commit".into());
        }
        if out.inspected_commit.as_deref() != Some(task.artifact_commit.as_str()) {
            return Err("verify output is not bound to the requested immutable commit".into());
        }
        if out
            .verdict_category
            .iter()
            .chain(&out.verdict_categories)
            .any(|v| {
                !matches!(
                    v.as_str(),
                    "patch" | "intent_gap" | "bad_plan" | "defer" | "pass"
                )
            })
        {
            return Err("unknown verify category".into());
        }
    }
    if task.role == "lens" && out.produced_commit.is_some() {
        return Err("lens stage returned a produced commit".into());
    }
    if task.role != "verify"
        && out
            .produced_commit
            .as_ref()
            .is_some_and(|commit| commit.trim().is_empty())
    {
        return Err("implementation stage returned an empty produced commit".into());
    }
    let accepted = task.workdir.join("accepted-output");
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&accepted)
        .map_err(sqlerr)?;
    file.write_all(&bytes).map_err(sqlerr)?;
    file.sync_all().map_err(sqlerr)?;
    fs::File::open(&task.workdir)
        .and_then(|dir| dir.sync_all())
        .map_err(sqlerr)?;
    out.log_path = accepted.to_string_lossy().into_owned();
    Ok(out)
}

fn persist_snapshot(root: &Path, snap: &foundry_pack::PackSnapshot) -> Result<(), String> {
    use std::io::Write;
    let destination = root.join("pack-snapshot");
    fs::create_dir_all(&destination).map_err(sqlerr)?;
    for (name, bytes) in snap.files() {
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("invalid snapshot path".into());
        }
        let path = destination.join(relative);
        fs::create_dir_all(path.parent().unwrap()).map_err(sqlerr)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(sqlerr)?;
        file.write_all(bytes).map_err(sqlerr)?;
        file.sync_all().map_err(sqlerr)?;
    }
    // The loader keeps manifest bytes outside PackSnapshot::files(). Re-read
    // that one file, then verify the copied closure before executing anything.
    let manifest = fs::read(snap.root().join("manifest.toml")).map_err(sqlerr)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination.join("manifest.toml"))
        .map_err(sqlerr)?;
    file.write_all(&manifest).map_err(sqlerr)?;
    file.sync_all().map_err(sqlerr)?;
    let copied = load_pack(&destination).map_err(sqlerr)?;
    if copied.digest() != snap.digest() {
        return Err("copied pack snapshot digest changed".into());
    }
    // Sync new directory entries before an external executor can observe them.
    for dir in [&destination, root, root.parent().unwrap()] {
        fs::File::open(dir)
            .and_then(|dir| dir.sync_all())
            .map_err(sqlerr)?;
    }
    Ok(())
}

fn validate_intent(intent: &ExecutionIntent) -> Result<(), String> {
    if intent.generation.0 != 0 {
        return Err("T31 review adapter only accepts generation 0".into());
    }
    if [
        intent.attempt.0,
        intent.run.0,
        intent.activation,
        intent.epoch.0,
    ]
    .iter()
    .any(|v| *v > i64::MAX as u64)
    {
        return Err("review identity exceeds SQLite integer range".into());
    }
    Ok(())
}
fn key_json(intent: &ExecutionIntent) -> String {
    serde_json::to_string(&host_key(intent)).expect("host key serialization")
}
fn advance_epoch(tx: &rusqlite::Transaction<'_>, order: &str, epoch: Epoch) -> Result<(), String> {
    let old: Option<i64> = tx
        .query_row(
            "SELECT epoch FROM review_epochs WHERE order_id=?1",
            [order],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlerr)?;
    if old.is_some_and(|old| old as u64 > epoch.0) {
        return Err("stale review epoch".into());
    }
    tx.execute("INSERT INTO review_epochs(order_id,epoch) VALUES(?1,?2) ON CONFLICT(order_id) DO UPDATE SET epoch=excluded.epoch",params![order,epoch.0 as i64]).map_err(sqlerr)?;
    Ok(())
}
fn ensure_active(tx: &rusqlite::Transaction<'_>, intent: &ExecutionIntent) -> Result<(), String> {
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM review_active a JOIN review_epochs e ON a.order_id=e.order_id WHERE a.order_id=?1 AND a.key_json=?2 AND a.epoch=?3 AND e.epoch=?3)",params![intent.order.0,key_json(intent),intent.epoch.0 as i64],|r|r.get(0)).map_err(sqlerr)?;
    if valid {
        Ok(())
    } else {
        Err("inactive run or stale epoch".into())
    }
}

/// Validate a stage mutation against the concrete run selected by the durable
/// active binding. A valid order/epoch alone is not enough: otherwise a caller
/// could charge the active order's budget while writing a different run.
fn ensure_active_stage_run(
    tx: &rusqlite::Transaction<'_>,
    intent: &ExecutionIntent,
    journal_run: &str,
    pinned_pack_digest: &str,
) -> Result<(), String> {
    ensure_active(tx, intent)?;
    let active_run: String = tx
        .query_row(
            "SELECT journal_run FROM review_active WHERE order_id=?1 AND key_json=?2 AND epoch=?3",
            params![intent.order.0, key_json(intent), intent.epoch.0 as i64],
            |row| row.get(0),
        )
        .map_err(sqlerr)?;
    if active_run != journal_run {
        return Err("stage journal run does not match the active run".into());
    }
    let (status, attempt, base, result, pack_digest): (String, i64, String, String, String) = tx.query_row(
        "SELECT status,attempt,base_commit,result_commit,pack_digest FROM review_runs WHERE order_id=?1 AND run_id=?2",
        params![intent.order.0, active_run],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    ).map_err(sqlerr)?;
    let (ref_order, ref_attempt, ref_base, ref_result) =
        parse_external_ref(&intent.external_ref)
            .ok_or_else(|| "invalid review external_ref for stage run".to_owned())?;
    if status != "running"
        || attempt != intent.attempt.0 as i64
        || ref_order != intent.order.0
        || ref_attempt != intent.attempt.0
        || ref_base != base
        || ref_result != result
        || pack_digest != pinned_pack_digest
    {
        return Err("active stage run coordinates do not match the persisted run".into());
    }
    Ok(())
}

/// Continuation writes use the same durable active binding as initial dispatch,
/// but require the exact run to be in its accepted `resuming` phase.
fn ensure_resuming_stage_run(
    tx: &rusqlite::Transaction<'_>,
    intent: &ExecutionIntent,
    journal_run: &str,
    pinned_pack_digest: &str,
) -> Result<(), String> {
    ensure_active(tx, intent)?;
    let active_run: String = tx
        .query_row(
            "SELECT journal_run FROM review_active WHERE order_id=?1 AND key_json=?2 AND epoch=?3",
            params![intent.order.0, key_json(intent), intent.epoch.0 as i64],
            |r| r.get(0),
        )
        .map_err(sqlerr)?;
    if active_run != journal_run {
        return Err("continuation journal run does not match the active run".into());
    }
    let(status,attempt,base,result,pack):(String,i64,String,String,String)=tx.query_row("SELECT status,attempt,base_commit,result_commit,pack_digest FROM review_runs WHERE order_id=?1 AND run_id=?2",params![intent.order.0,active_run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(sqlerr)?;
    let (ref_order, ref_attempt, ref_base, ref_result) =
        parse_external_ref(&intent.external_ref)
            .ok_or_else(|| "invalid review external_ref for continuation".to_owned())?;
    if status != "resuming"
        || attempt != intent.attempt.0 as i64
        || ref_order != intent.order.0
        || ref_attempt != intent.attempt.0
        || ref_base != base
        || ref_result != result
        || pack != pinned_pack_digest
    {
        return Err("continuation run coordinates do not match the persisted run".into());
    }
    Ok(())
}

fn ensure_new_stage_sequence(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    run: &str,
    sequence: u64,
) -> Result<(), String> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2 AND sequence=?3)",
        params![order, run, sequence as i64],
        |row| row.get(0),
    ).map_err(sqlerr)?;
    if exists {
        return Err("review stage sequence is already claimed".into());
    }
    let max: Option<i64> = tx
        .query_row(
            "SELECT MAX(sequence) FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2",
            params![order, run],
            |row| row.get(0),
        )
        .map_err(sqlerr)?;
    if max.is_some_and(|previous| sequence as i64 <= previous) {
        return Err("review stage sequence is not after the persisted run cursor".into());
    }
    Ok(())
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct BinaryProvenance {
    pub executable: String,
    pub sha256: String,
    pub source_commit: String,
    pub pack_digest: String,
}
impl ReviewAdmissionStore for StandaloneReviewStore {
    fn validate_product_work_claim(
        &self,
        _claim: &foundry_exec::ReviewWorkClaim,
        _intent: &ExecutionIntent,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        Err(HostError::Acceptance(
            "product work claims cannot be used with standalone review storage".into(),
        ))
    }
    fn admit_review_activation(
        &mut self,
        request: ReviewAdmissionRequest,
    ) -> Result<ReviewAdmission, HostError> {
        let intent = &request.intent;
        validate_intent(intent)?;
        let (ref_order, ref_attempt, ref_base, ref_result) =
            parse_external_ref(&intent.external_ref)
                .ok_or_else(|| HostError::Worker("invalid review external_ref".into()))?;
        if ref_order != intent.order.0
            || ref_attempt != intent.attempt.0
            || ref_base != request.base_commit
            || ref_result != request.result_commit
            || request.pack_digest != self.pack_digest
        {
            return Err(HostError::Worker(
                "review admission binding mismatch".into(),
            ));
        }
        let order = intent.order.0.as_str();
        let attempt = intent.attempt.0;
        let key = host_key(intent);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        let accepted: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",
            params![key.order,key.attempt as i64,key.run as i64,key.activation as i64], |r| r.get(0),
        ).map_err(sqlerr)?;
        if accepted {
            return Err(HostError::Persistence(
                "accepted activation has no recoverable result; refusing redispatch".into(),
            ));
        }
        let previous: Option<ActiveReviewRow> = tx.query_row(
            "SELECT r.status,a.epoch,r.result_json,r.attempt,r.base_commit,r.result_commit,r.hold_json FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE r.order_id=?1",
            [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
        ).optional().map_err(sqlerr)?;
        let mut inherited_result = None;
        let mut inherited_hold_json = None;
        if let Some((
            status,
            epoch,
            prior_result,
            prior_attempt,
            prior_base,
            prior_commit,
            prior_hold,
        )) = previous
        {
            if intent.epoch.0 < epoch as u64
                || matches!(status.as_str(), "held" | "resuming")
                || (status == "running" && intent.epoch.0 <= epoch as u64)
            {
                return Err(HostError::Acceptance(
                    "stale, held, or still-running order cannot start this activation".into(),
                ));
            }
            if status == "running" {
                tx.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status='running'", [order]).map_err(sqlerr)?;
            }
            if (status == "resolved" || status == "review_aborted")
                && prior_attempt == attempt as i64
            {
                if prior_base != request.base_commit || prior_commit != request.result_commit {
                    return Err(HostError::Acceptance(
                        "resolved review input binding mismatch".into(),
                    ));
                }
                inherited_result = prior_result;
                inherited_hold_json = prior_hold;
            }
        }
        advance_epoch(&tx, order, intent.epoch).map_err(HostError::Acceptance)?;
        let inherited_counters = latest_counters(&tx, order)?;
        let run_id = format!(
            "{}:{}:{}:{}",
            order, attempt, intent.run.0, intent.activation
        );
        tx.execute(
            "INSERT INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,unixepoch())",
            params![order,attempt as i64,run_id,request.base_commit,request.result_commit,request.pack_digest,serde_json::to_string(&inherited_counters).map_err(sqlerr)?],
        ).map_err(sqlerr)?;
        tx.execute(
            "INSERT INTO host_accept(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)",
            params![
                key.order,
                key.attempt as i64,
                key.run as i64,
                key.activation as i64
            ],
        )
        .map_err(sqlerr)
        .map_err(HostError::Acceptance)?;
        if let Some(encoded) = inherited_result.as_ref() {
            let result: HostResult = serde_json::from_str(encoded).map_err(sqlerr)?;
            let status = if result.held.is_some() {
                "held"
            } else if result.exit_code == 0 {
                "passed"
            } else {
                "failed"
            };
            if let Some(hold) = result.held.as_ref() {
                let mut doc: serde_json::Value =
                    serde_json::from_str(inherited_hold_json.as_deref().ok_or_else(|| {
                        HostError::Persistence("inherited hold has no snapshot".into())
                    })?)
                    .map_err(sqlerr)?;
                let source_run = doc["run_id"]
                    .as_str()
                    .ok_or_else(|| {
                        HostError::Persistence("inherited hold has no run identity".into())
                    })?
                    .to_owned();
                if doc.get("snapshot_run_id").is_none() {
                    doc["snapshot_run_id"] = serde_json::json!(source_run);
                }
                doc["intent"] = serde_json::to_value(intent).map_err(sqlerr)?;
                doc["run_id"] = serde_json::json!(run_id);
                tx.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) SELECT order_id,?1,sequence,stage_id,status,output_json FROM review_stage_attempts WHERE order_id=?2 AND run_id=?3", params![run_id,order,source_run]).map_err(sqlerr)?;
                tx.execute("UPDATE review_runs SET hold_json=?1,hold_instance_id=?2,resume_targets=?3,deadline=?4 WHERE order_id=?5 AND attempt=?6 AND run_id=?7", params![doc.to_string(),hold.hold_instance_id,serde_json::to_string(&hold.resume_targets).map_err(sqlerr)?,if hold.deadline_secs==0 {0} else {chrono::Utc::now().timestamp().saturating_add(hold.deadline_secs as i64)},order,attempt as i64,run_id]).map_err(sqlerr)?;
            }
            tx.execute("UPDATE review_runs SET status=?1,result_json=?2 WHERE order_id=?3 AND attempt=?4 AND run_id=?5", params![status,encoded,order,attempt as i64,run_id]).map_err(sqlerr)?;
            tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64,encoded]).map_err(sqlerr)?;
        }
        tx.execute(
            "INSERT INTO review_bindings(key_json,intent_json) VALUES(?1,?2)",
            params![
                key_json(intent),
                serde_json::to_string(intent).map_err(sqlerr)?
            ],
        )
        .map_err(sqlerr)?;
        tx.execute("INSERT OR REPLACE INTO review_active(order_id,key_json,epoch,journal_run) VALUES(?1,?2,?3,?4)", params![order,key_json(intent),intent.epoch.0 as i64,run_id]).map_err(sqlerr)?;
        tx.commit()
            .map_err(sqlerr)
            .map_err(HostError::Persistence)?;
        Ok(ReviewAdmission {
            journal_run: run_id,
            inherited_counters,
            inherited_result_json: inherited_result,
        })
    }
}

impl ReviewDispatchStore for StandaloneReviewStore {
    fn load_review_result(&self, key: &HostKey) -> Result<Option<HostResult>, HostError> {
        self.conn.query_row(
            "SELECT result_json FROM host_results WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",
            params![key.order, key.attempt as i64, key.run as i64, key.activation as i64],
            |row| row.get::<_, String>(0),
        ).optional().map_err(sqlerr)?
            .map(|encoded| serde_json::from_str(&encoded).map_err(sqlerr).map_err(HostError::Persistence)).transpose()
    }

    fn checkpoint_review_counters(
        &mut self,
        request: foundry_exec::ReviewCounterCheckpoint,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(
            &tx,
            &request.intent,
            &request.journal_run,
            &self.pack_digest,
        )?;
        let counters = serde_json::to_string(&request.counters).map_err(sqlerr)?;
        if tx.execute("UPDATE review_runs SET counters_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='running'",
            params![counters, request.intent.order.0, request.intent.attempt.0 as i64, request.journal_run])
            .map_err(sqlerr)? != 1 {
            return Err(HostError::Worker("review counter checkpoint is outside the active running run".into()));
        }
        Ok(tx.commit().map_err(sqlerr)?).map_err(HostError::Persistence)
    }

    fn start_review_stage(&mut self, request: ReviewStageStart) -> Result<(), HostError> {
        if request.sequence > i64::MAX as u64 || request.budget_limit > i64::MAX as u64 {
            return Err(HostError::Persistence(
                "review stage coordinate exceeds SQLite range".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(
            &tx,
            &request.intent,
            &request.journal_run,
            &self.pack_digest,
        )?;
        ensure_new_stage_sequence(
            &tx,
            &request.intent.order.0,
            &request.journal_run,
            request.sequence,
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO review_budget(order_id,used) VALUES(?1,0)",
            [&request.intent.order.0],
        )
        .map_err(sqlerr)?;
        if tx
            .execute(
                "UPDATE review_budget SET used=used+1 WHERE order_id=?1 AND used<?2",
                params![request.intent.order.0, request.budget_limit as i64],
            )
            .map_err(sqlerr)?
            == 0
        {
            return Err(HostError::Worker("order execution budget exhausted".into()));
        }
        tx.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",
            params![request.intent.order.0, request.journal_run, request.sequence as i64, request.stage_id])
            .map_err(sqlerr)?;
        Ok(tx.commit().map_err(sqlerr)?).map_err(HostError::Persistence)
    }

    fn fail_review_dispatch(
        &mut self,
        request: foundry_exec::ReviewDispatchFailure,
    ) -> Result<(), HostError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(
            &tx,
            &request.intent,
            &request.journal_run,
            &self.pack_digest,
        )?;
        let counters = serde_json::to_string(&request.counters).map_err(sqlerr)?;
        if tx.execute("UPDATE review_runs SET status='review_aborted',counters_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='running'",
            params![counters, request.intent.order.0, request.intent.attempt.0 as i64, request.journal_run])
            .map_err(sqlerr)? != 1 {
            return Err(HostError::Worker("review failure is outside the active running run".into()));
        }
        tx.execute(
            "INSERT INTO review_failures(order_id,run_id,error) VALUES(?1,?2,?3)",
            params![request.intent.order.0, request.journal_run, request.error],
        )
        .map_err(sqlerr)?;
        Ok(tx.commit().map_err(sqlerr)?).map_err(HostError::Persistence)
    }

    fn complete_review_dispatch(
        &mut self,
        request: foundry_exec::ReviewDispatchCompletion,
    ) -> Result<(), HostError> {
        if request.result.held.as_ref()
            != request
                .held_snapshot
                .as_ref()
                .map(|snapshot| &snapshot.hold)
        {
            return Err(HostError::Worker(
                "terminal hold snapshot does not match the host result".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(
            &tx,
            &request.intent,
            &request.journal_run,
            &self.pack_digest,
        )?;
        let (status, hold_json, hold_id, targets) = match request.status {
            foundry_exec::ReviewTerminalStatus::Passed => ("passed", Some(serde_json::json!({
                "node_id":null,"entry_counters":null,"requested_revert":null,"artifact_mutation_applied":false
            }).to_string()), None, None),
            foundry_exec::ReviewTerminalStatus::Held => {
                let snapshot = request.held_snapshot.as_ref().ok_or_else(||HostError::Worker("held dispatch lacks its immutable snapshot".into()))?;
                let entry = serde_json::json!({"node_id":snapshot.node_id,"entry_counters":snapshot.entry_counters,"reverted_base":snapshot.requested_revert});
                let saved = serde_json::json!({
                    "hold":snapshot.hold,"entry":entry,"engine_state":snapshot.engine_state,"intent":request.intent,
                    "run_id":request.journal_run,"session_ids":snapshot.session_ids,"accepted_logs":snapshot.accepted_logs,
                    "generations":snapshot.generations,"latest_commit":snapshot.latest_commit,"sequence":snapshot.sequence
                }).to_string();
                ("held", Some(saved), Some(snapshot.hold.hold_instance_id.clone()),
                    Some(serde_json::to_string(&snapshot.hold.resume_targets).map_err(sqlerr)?))
            }
            foundry_exec::ReviewTerminalStatus::Failed => ("failed", Some(serde_json::json!({
                "node_id":null,"entry_counters":null,"requested_revert":null,"artifact_mutation_applied":false
            }).to_string()), None, None),
        };
        if (request.status == foundry_exec::ReviewTerminalStatus::Held)
            != request.result.held.is_some()
        {
            return Err(HostError::Worker(
                "terminal status disagrees with host result".into(),
            ));
        }
        match request.status {
            foundry_exec::ReviewTerminalStatus::Passed if request.result.exit_code != 0 => {
                return Err(HostError::Worker(
                    "passed terminal status requires a zero exit code".into(),
                ));
            }
            foundry_exec::ReviewTerminalStatus::Failed if request.result.exit_code == 0 => {
                return Err(HostError::Worker(
                    "failed terminal status requires a nonzero exit code".into(),
                ));
            }
            _ => {}
        }
        let counters = serde_json::to_string(&request.counters).map_err(sqlerr)?;
        let result_json = serde_json::to_string(&request.result).map_err(sqlerr)?;
        let provenance_json = serde_json::to_string(&request.provenance).map_err(sqlerr)?;
        if tx.execute("UPDATE review_runs SET status=?1,counters_json=?2,hold_json=?3,hold_instance_id=?4,resume_targets=?5,deadline=?6,result_json=?7,provenance_json=?8 WHERE order_id=?9 AND attempt=?10 AND run_id=?11 AND status='running'",
            params![status,counters,hold_json,hold_id,targets,request.deadline,result_json,provenance_json,
                request.intent.order.0,request.intent.attempt.0 as i64,request.journal_run])
            .map_err(sqlerr)? != 1 {
            return Err(HostError::Worker("terminal result is outside the active running run".into()));
        }
        tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",
            params![request.intent.order.0,request.intent.attempt.0 as i64,request.intent.run.0 as i64,
                request.intent.activation as i64,result_json])
            .map_err(sqlerr)?;
        Ok(tx.commit().map_err(sqlerr)?).map_err(HostError::Persistence)
    }

    fn finish_review_stage(&mut self, request: ReviewStageFinishRequest) -> Result<(), HostError> {
        if request.sequence > i64::MAX as u64 {
            return Err(HostError::Persistence(
                "review stage coordinate exceeds SQLite range".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sqlerr)?;
        ensure_active_stage_run(
            &tx,
            &request.intent,
            &request.journal_run,
            &self.pack_digest,
        )?;
        let order = &request.intent.order.0;
        let run = &request.journal_run;
        let seq = request.sequence as i64;
        match &request.finish {
            ReviewStageFinish::Blocked(_) => {
                ensure_new_stage_sequence(&tx, order, run, request.sequence)?;
            }
            ReviewStageFinish::Complete(_) | ReviewStageFinish::Error(_) => {
                let claim: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2 AND sequence=?3 AND stage_id=?4 AND status='accepted')",
                    params![order, run, seq, request.stage_id],
                    |row| row.get(0),
                ).map_err(sqlerr)?;
                if !claim {
                    return Err(HostError::Worker(
                        "review stage claim is missing or already finalized".into(),
                    ));
                }
            }
        }
        let changed = match request.finish {
            ReviewStageFinish::Complete(evidence) => {
                let output = serde_json::json!({
                    "role": evidence.role,
                    "log_path": evidence.log_path,
                    "sha256": evidence.sha256,
                    "deferred": evidence.deferred,
                    "inspected_commit": evidence.inspected_commit,
                    "findings": evidence.findings,
                    "verdicts": evidence.verdicts,
                    "input_manifest": evidence.input_manifest,
                }).to_string();
                let count = tx.execute("UPDATE review_stage_attempts SET status='complete',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4 AND stage_id=?5 AND status='accepted'",
                    params![output, order, run, seq, request.stage_id])
                    .map_err(sqlerr)?;
                if let Some(session) = evidence.session {
                    let receipt = serde_json::to_string(&SessionReceipt {
                        session_id: session.session_id,
                        output: session.output,
                        sha256: session.sha256,
                    }).map_err(sqlerr)?;
                    tx.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",
                        params![order, run, session.session_key, receipt])
                        .map_err(sqlerr)?;
                }
                count
            }
            ReviewStageFinish::Error(message) => tx.execute(
                "UPDATE review_stage_attempts SET status='error',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4 AND stage_id=?5 AND status='accepted'",
                params![message, order, run, seq, request.stage_id])
                .map_err(sqlerr)?,
            ReviewStageFinish::Blocked(message) => tx.execute(
                "INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'blocked',?5)",
                params![order, run, seq, request.stage_id, message])
                .map_err(sqlerr)?,
        };
        if changed != 1 {
            return Err(HostError::Worker(
                "review stage claim is missing or already finalized".into(),
            ));
        }
        Ok(tx.commit().map_err(sqlerr)?).map_err(HostError::Persistence)
    }
}

// Preserve the standalone host's existing public port surface. Production
// dispatch itself calls the store directly; these delegations keep existing
// standalone integrations source compatible during the storage extraction.
impl ReviewAdmissionStore for PackReviewHost {
    fn validate_product_work_claim(
        &self,
        claim: &foundry_exec::ReviewWorkClaim,
        intent: &ExecutionIntent,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        self.store.validate_product_work_claim(claim, intent, now)
    }
    fn admit_review_activation(
        &mut self,
        request: ReviewAdmissionRequest,
    ) -> Result<ReviewAdmission, HostError> {
        self.store.admit_review_activation(request)
    }
}
impl ReviewDispatchStore for PackReviewHost {
    fn load_review_result(&self, key: &HostKey) -> Result<Option<HostResult>, HostError> {
        self.store.load_review_result(key)
    }
    fn checkpoint_review_counters(
        &mut self,
        request: foundry_exec::ReviewCounterCheckpoint,
    ) -> Result<(), HostError> {
        self.store.checkpoint_review_counters(request)
    }
    fn start_review_stage(&mut self, request: ReviewStageStart) -> Result<(), HostError> {
        self.store.start_review_stage(request)
    }
    fn finish_review_stage(&mut self, request: ReviewStageFinishRequest) -> Result<(), HostError> {
        self.store.finish_review_stage(request)
    }
    fn fail_review_dispatch(
        &mut self,
        request: foundry_exec::ReviewDispatchFailure,
    ) -> Result<(), HostError> {
        self.store.fail_review_dispatch(request)
    }
    fn complete_review_dispatch(
        &mut self,
        request: foundry_exec::ReviewDispatchCompletion,
    ) -> Result<(), HostError> {
        self.store.complete_review_dispatch(request)
    }
}

fn validate_source_commit(s: &str) -> Result<(), String> {
    if !matches!(s.len(), 40 | 64) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("FOUNDRY_SOURCE_COMMIT must be an explicit full source commit hash".into());
    }
    Ok(())
}
fn provenance(pack: &str, source: Option<&str>) -> Result<BinaryProvenance, String> {
    let source=source.ok_or("FOUNDRY_SOURCE_COMMIT must identify the executable build; launch-directory git HEAD is not provenance")?;
    validate_source_commit(source)?;
    let exe = std::env::current_exe().map_err(sqlerr)?;
    let bytes = fs::read(&exe).map_err(sqlerr)?;
    Ok(BinaryProvenance {
        executable: exe.display().to_string(),
        sha256: format!("{:x}", Sha256::digest(bytes)),
        source_commit: source.into(),
        pack_digest: pack.into(),
    })
}

type Scope = Arc<Mutex<Option<ExecutionIntent>>>;
pub struct SqliteReviewSink {
    conn: Mutex<Connection>,
    scope: Scope,
}
pub struct SqliteReviewLog {
    conn: Mutex<Connection>,
    scope: Scope,
}
pub struct SqliteIntentLog {
    conn: Mutex<Connection>,
    scope: Scope,
}
macro_rules! open_port {
    ($t:ty) => {
        impl $t {
            pub fn open(p: impl AsRef<Path>) -> Result<Self, String> {
                Ok(Self {
                    conn: Mutex::new(open_db(p.as_ref())?),
                    scope: Arc::new(Mutex::new(None)),
                })
            }
        }
    };
}
open_port!(SqliteReviewSink);
open_port!(SqliteReviewLog);
open_port!(SqliteIntentLog);
impl EmitSink for SqliteReviewSink {
    fn publish(&mut self, t: &ArtifactTuple) -> EmitReceipt {
        self.publish_named(t, "InspectionPassed", "summary")
    }
    fn publish_named(&mut self, t: &ArtifactTuple, event: &str, _payload: &str) -> EmitReceipt {
        let mut c = self.conn.lock().unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let key = serde_json::to_string(&t.key()).unwrap();
        let old: Option<String> = tx
            .query_row(
                "SELECT receipt_json FROM review_emit WHERE key_json=?1",
                [&key],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        if let Some(old) = old {
            return serde_json::from_str(&old).unwrap();
        }
        let intent = self
            .scope
            .lock()
            .unwrap()
            .clone()
            .expect("emit requires a bound review intent");
        ensure_active(&tx, &intent).unwrap_or_else(|e| panic!("stale review publication: {e}"));
        assert_eq!(t.order, intent.order, "emit order binding");
        assert_eq!(t.attempt, intent.attempt, "emit attempt binding");
        assert_eq!(t.run, intent.run, "emit run binding");
        assert_eq!(t.generation, intent.generation, "emit generation binding");
        let published:Option<String>=tx.query_row("SELECT r.result_json FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE a.key_json=?1 AND r.status='passed'",[key_json(&intent)],|r|r.get(0)).optional().unwrap();
        let final_commit = published
            .and_then(|json| serde_json::from_str::<HostResult>(&json).ok())
            .map(|result| result.result_commit);
        assert!(
            final_commit.as_deref() == Some(t.immutable_commit.0.as_str())
                && event == "InspectionPassed",
            "publication requires a passed review bound to its final generation"
        );
        let deferred:Option<(String,String,String)>=tx.query_row("SELECT s.output_json,r.counters_json,r.run_id FROM review_stage_attempts s JOIN review_runs r ON s.order_id=r.order_id AND s.run_id=r.run_id JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE a.key_json=?1 AND s.status='complete' AND json_extract(s.output_json,'$.role')='verify' ORDER BY s.sequence DESC LIMIT 1",[key_json(&intent)],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().unwrap();
        if let Some((output, counters, run)) = deferred {
            let output: serde_json::Value = serde_json::from_str(&output).unwrap();
            if output["deferred"] == true {
                tx.execute("INSERT OR IGNORE INTO review_side_records(order_id,run_id,kind,value) VALUES(?1,?2,'deferred',?3)",params![intent.order.0,run,serde_json::json!({"source_run":run,"output":output["log_path"],"counters":serde_json::from_str::<serde_json::Value>(&counters).unwrap()}).to_string()]).unwrap();
            }
        }
        let rec = receipt(t, event);
        tx.execute(
            "INSERT INTO review_emit(key_json,receipt_json) VALUES(?1,?2)",
            params![key, serde_json::to_string(&rec).unwrap()],
        )
        .unwrap();
        tx.commit().unwrap();
        rec
    }
    fn receipt_of(&self, k: &ArtifactTupleKey) -> Option<EmitReceipt> {
        let s = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT receipt_json FROM review_emit WHERE key_json=?1",
                [serde_json::to_string(k).unwrap()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap_or_else(|e| panic!("emit receipt read failed: {e}"));
        s.map(|v| serde_json::from_str(&v).unwrap_or_else(|e| panic!("emit receipt corrupt: {e}")))
    }
}
impl EmitLog for SqliteReviewLog {
    fn current_epoch(&self) -> Epoch {
        let scope = self.scope.lock().unwrap();
        let Some(intent) = scope.as_ref() else {
            return Epoch(u64::MAX);
        };
        let value: i64 = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT epoch FROM review_epochs WHERE order_id=?1",
                [&intent.order.0],
                |r| r.get(0),
            )
            .unwrap();
        Epoch(u64::try_from(value).expect("negative review epoch"))
    }
    fn promotion_of(&self, k: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        let s = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value_json FROM review_promotions WHERE key_json=?1",
                [serde_json::to_string(k).unwrap()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap_or_else(|e| panic!("promotion read failed: {e}"));
        s.map(|v| {
            serde_json::from_str(&v).unwrap_or_else(|e| panic!("promotion receipt corrupt: {e}"))
        })
    }
    fn emit_of(&self, k: &ArtifactTupleKey) -> Option<EmitReceipt> {
        let s = self
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT value_json FROM review_emit_log WHERE key_json=?1",
                [serde_json::to_string(k).unwrap()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .unwrap_or_else(|e| panic!("emit log read failed: {e}"));
        s.map(|v| serde_json::from_str(&v).unwrap_or_else(|e| panic!("emit receipt corrupt: {e}")))
    }
    fn record_emit(&mut self, r: &EmitReceipt) {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO review_emit_log VALUES(?1,?2)",
                params![
                    serde_json::to_string(&r.key).unwrap(),
                    serde_json::to_string(r).unwrap()
                ],
            )
            .unwrap();
    }
    fn mark_complete(&mut self, k: &ArtifactTupleKey) {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO review_emit_complete VALUES(?1)",
                [serde_json::to_string(k).unwrap()],
            )
            .unwrap();
    }
    fn is_complete(&self, k: &ArtifactTupleKey) -> bool {
        self.conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM review_emit_complete WHERE key_json=?1)",
                [serde_json::to_string(k).unwrap()],
                |r| r.get(0),
            )
            .unwrap_or_else(|e| panic!("emit completion read failed: {e}"))
    }
}
impl IntentLog for SqliteIntentLog {
    fn append(&mut self, i: &ExecutionIntent) {
        validate_intent(i).expect("invalid durable review intent");
        let mut c = self.conn.lock().unwrap();
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        tx.execute("INSERT INTO review_epochs(order_id,epoch) VALUES(?1,?2) ON CONFLICT(order_id) DO UPDATE SET epoch=MAX(epoch,excluded.epoch)",params![i.order.0,i.epoch.0 as i64]).unwrap();
        tx.execute(
            "INSERT OR IGNORE INTO review_intents VALUES(?1,?2,?3,?4,?5)",
            params![
                i.order.0,
                i.attempt.0 as i64,
                i.run.0 as i64,
                i.activation as i64,
                serde_json::to_string(i).unwrap()
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        *self.scope.lock().unwrap() = Some(i.clone());
    }
    fn mark_started(&mut self, k: &(OrderId, Attempt, RunId), a: u64) {
        self.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT OR IGNORE INTO review_started VALUES(?1,?2,?3,?4)",
                params![k.0.0, k.1.0 as i64, k.2.0 as i64, a as i64],
            )
            .unwrap();
    }
    fn latest(&self, k: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent> {
        let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 ORDER BY activation DESC LIMIT 1",params![k.0.0,k.1.0 as i64,k.2.0 as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));
        s.map(|v| serde_json::from_str(&v).unwrap_or_else(|e| panic!("intent corrupt: {e}")))
    }
    fn get(&self, k: &(OrderId, Attempt, RunId), a: u64) -> Option<ExecutionIntent> {
        let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));
        s.map(|v| serde_json::from_str(&v).unwrap_or_else(|e| panic!("intent corrupt: {e}")))
    }
    fn is_started(&self, k: &(OrderId, Attempt, RunId), a: u64) -> bool {
        self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM review_started WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get(0)).unwrap_or_else(|e|panic!("intent claim read failed: {e}"))
    }
}

fn open_db(p: &Path) -> Result<Connection, String> {
    if let Some(p) = p.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let c = Connection::open(p).map_err(sqlerr)?;
    reject_product_contract(&c)?;
    c.pragma_update(None, "journal_mode", "WAL")
        .map_err(sqlerr)?;
    c.pragma_update(None, "synchronous", "FULL")
        .map_err(sqlerr)?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS review_runs(order_id TEXT,attempt INTEGER,run_id TEXT,base_commit TEXT,result_commit TEXT,pack_digest TEXT,status TEXT,counters_json TEXT,hold_json TEXT,hold_instance_id TEXT,resume_targets TEXT,deadline INTEGER,result_json TEXT,provenance_json TEXT,created_at INTEGER,UNIQUE(order_id,attempt,run_id));CREATE TABLE IF NOT EXISTS host_accept(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS host_results(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,result_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_sessions(order_id TEXT,run_id TEXT,session_id TEXT,receipt_json TEXT,PRIMARY KEY(order_id,run_id,session_id));CREATE TABLE IF NOT EXISTS review_side_records(order_id TEXT,run_id TEXT,kind TEXT,value TEXT);CREATE TABLE IF NOT EXISTS review_emit(key_json TEXT PRIMARY KEY,receipt_json TEXT);CREATE TABLE IF NOT EXISTS review_promotions(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_log(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_complete(key_json TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS review_intents(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,value_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_started(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));
CREATE UNIQUE INDEX IF NOT EXISTS review_side_once ON review_side_records(order_id,run_id,kind);
CREATE TABLE IF NOT EXISTS review_epochs(order_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS review_active(order_id TEXT PRIMARY KEY,key_json TEXT NOT NULL,epoch INTEGER NOT NULL,journal_run TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_bindings(key_json TEXT PRIMARY KEY,intent_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_failures(order_id TEXT,run_id TEXT,error TEXT);
CREATE TABLE IF NOT EXISTS review_stage_attempts(order_id TEXT,run_id TEXT,sequence INTEGER,stage_id TEXT,status TEXT,output_json TEXT,UNIQUE(order_id,run_id,sequence));
CREATE TABLE IF NOT EXISTS review_budget(order_id TEXT PRIMARY KEY,used INTEGER NOT NULL);
").map_err(sqlerr)?;
    ensure_column(&c, "review_runs", "hold_instance_id", "TEXT")?;
    ensure_column(&c, "review_runs", "resume_targets", "TEXT")?;
    ensure_column(&c, "review_runs", "deadline", "INTEGER")?;
    Ok(c)
}
fn reject_product_contract(c: &Connection) -> Result<(), String> {
    let has_contract:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='foundry_contract')",[],|r|r.get(0)).map_err(sqlerr)?;
    if has_contract {
        let contract: Option<String> = c
            .query_row(
                "SELECT contract FROM foundry_contract WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlerr)?;
        if contract.as_deref() != Some("StandaloneV1") {
            return Err("database contract cannot be opened by standalone review host".into());
        }
    }
    Ok(())
}
fn ensure_column(c: &Connection, table: &str, column: &str, ty: &str) -> Result<(), String> {
    let mut q = c
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sqlerr)?;
    let cols = q
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(sqlerr)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlerr)?;
    if !cols.iter().any(|c| c == column) {
        c.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"), [])
            .map_err(sqlerr)?;
    }
    Ok(())
}
fn latest_counters(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
) -> Result<BTreeMap<String, u64>, HostError> {
    let v: Option<String> = tx
        .query_row(
            "SELECT counters_json FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",
            [order],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlerr)
        .map_err(HostError::Persistence)?;
    v.map(|s| serde_json::from_str(&s).map_err(|e| HostError::Persistence(e.to_string())))
        .transpose()
        .map(|x| x.unwrap_or_default())
}
fn parse_external_ref(s: &str) -> Option<(String, u64, String, String)> {
    let (identity, commits) = s.strip_prefix("review:")?.rsplit_once(':')?;
    let (order, attempt) = identity.rsplit_once(':')?;
    let (base, result) = commits.split_once("..")?;
    if order.is_empty() || base.is_empty() || result.is_empty() || result.contains("..") {
        return None;
    }
    Some((
        order.into(),
        attempt.parse().ok()?,
        base.into(),
        result.into(),
    ))
}
fn host_key(i: &ExecutionIntent) -> HostKey {
    HostKey {
        order: i.order.0.clone(),
        attempt: i.attempt.0,
        run: i.run.0,
        activation: i.activation,
    }
}
fn new_hold_id() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("OS random source");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!("{:02x?}", bytes).replace(['[', ']', ',', ' '], "")
}
fn charge_review_budget(
    conn: &mut Connection,
    order: &str,
    intent: &ExecutionIntent,
) -> foundry_pack::Result<()> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(packerr)?;
    ensure_active(&tx, intent).map_err(packerr)?;
    tx.execute(
        "INSERT OR IGNORE INTO review_budget(order_id,used) VALUES(?1,0)",
        [order],
    )
    .map_err(packerr)?;
    if tx
        .execute(
            "UPDATE review_budget SET used=used+1 WHERE order_id=?1 AND used<10000",
            [order],
        )
        .map_err(packerr)?
        == 0
    {
        return Err(foundry_pack::PackError {
            code: "E_EXEC_BUDGET",
            message: "order execution budget exhausted".into(),
        });
    }
    tx.commit().map_err(packerr)
}
fn receipt(t: &ArtifactTuple, e: &str) -> EmitReceipt {
    EmitReceipt {
        key: t.key(),
        event: e.into(),
        at: chrono::Utc::now(),
    }
}
fn sqlerr(e: impl std::fmt::Display) -> String {
    e.to_string()
}
fn packerr(e: impl std::fmt::Display) -> foundry_pack::PackError {
    foundry_pack::PackError {
        code: "E_HOST_STAGE",
        message: e.to_string(),
    }
}
fn host_port_packerr(e: HostError) -> foundry_pack::PackError {
    packerr(format!("{e:?}"))
}
fn stage_port_packerr(error: HostError) -> foundry_pack::PackError {
    match error {
        HostError::Worker(message) if message == "order execution budget exhausted" => {
            foundry_pack::PackError {
                code: "E_EXEC_BUDGET",
                message,
            }
        }
        other => packerr(format!("{other:?}")),
    }
}
fn expand_home(s: &str) -> PathBuf {
    if let Some(x) = s.strip_prefix("~/")
        && let Ok(h) = std::env::var("HOME")
    {
        return PathBuf::from(h).join(x);
    }
    PathBuf::from(s)
}

// Helper factory used by CLI and serve; all S4 receipts and tuples survive restart.
pub fn durable_review_run(
    host: PackReviewHost,
    db: impl AsRef<Path>,
    tuples: std::sync::Arc<dyn TupleStore>,
) -> Result<DurableReviewRun<SqliteReviewSink, SqliteReviewLog>, String> {
    if Some(fs::canonicalize(db.as_ref()).map_err(sqlerr)?) != host.standalone_db_path {
        return Err("host and runner must use the same database".into());
    }
    let (sink, log, intents) = PackReviewHost::sqlite_ports(db)?;
    Ok(DurableReviewRun::new(
        sink,
        log,
        Box::new(host),
        Box::new(intents),
        tuples,
    ))
}

/// Product adapter for the durable review runner. Hold outcomes remain data;
/// completed rejection errors are returned to Line for its normal failure event.
pub struct InspectingReviewRun<R> {
    inner: R,
}
impl<R> InspectingReviewRun<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}
impl<R: foundry_exec::ReviewRunPort> foundry_exec::ReviewRunPort for InspectingReviewRun<R> {
    fn run(
        &mut self,
        request: foundry_exec::ReviewRunRequest,
    ) -> Result<foundry_exec::ReviewRunOutcome, foundry_exec::ReviewRunError> {
        self.inner.run(request)
    }
    fn latest_run(
        &self,
        order: &str,
        attempt: u64,
    ) -> Result<Option<foundry_exec::RunRecord>, foundry_exec::ReviewRunError> {
        self.inner.latest_run(order, attempt)
    }
    fn resolve_hold(
        &mut self,
        order: &str,
        hold_id: &str,
        action: foundry_exec::ReviewHoldAction,
        actor: &str,
    ) -> Result<(), foundry_exec::ReviewRunError> {
        self.inner.resolve_hold(order, hold_id, action, actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    struct ProbeExecutor;
    impl StageExecutor for ProbeExecutor {
        fn ensure_available(&self) -> Result<(), String> {
            Ok(())
        }
        fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
            panic!("admission probe must stop before execution")
        }
    }

    /// An alternate supplied owner that delegates durable work to the
    /// standalone implementation while recording calls. This proves that the
    /// shared host algorithm can be driven through an injected owner; it is
    /// intentionally not evidence of ProductStore acceptance or authority.
    struct RecordingReviewStore {
        inner: StandaloneReviewStore,
        calls: Arc<Mutex<Vec<&'static str>>>,
        reject_admission: bool,
    }
    impl ReviewHostStore for RecordingReviewStore {
        fn pack_digest(&self) -> &str {
            self.inner.pack_digest()
        }
        fn run_root(&self, run: &str) -> Result<PathBuf, HostError> {
            self.inner.run_root(run)
        }
        fn claim_resume(&self, run: &str) -> Result<Option<fs::File>, HostError> {
            self.inner.claim_resume(run)
        }
        fn resolved_attempt(&self, order: &str, attempt: u64) -> Result<bool, HostError> {
            self.inner.resolved_attempt(order, attempt)
        }
        fn session_receipt(
            &self,
            order: &str,
            session: &str,
        ) -> Result<Option<foundry_exec::ReviewSessionReceipt>, HostError> {
            self.inner.session_receipt(order, session)
        }
        fn invalidate_terminal_runs(&mut self, order: &str) -> Result<(), HostError> {
            self.inner.invalidate_terminal_runs(order)
        }
        fn redrive_payload(
            &self,
            order: &str,
            run: &str,
            payload: &str,
        ) -> Result<Vec<foundry_exec::ReviewRedriveArtifact>, HostError> {
            self.inner.redrive_payload(order, run, payload)
        }
        fn charge_budget(
            &mut self,
            order: &str,
            intent: &ExecutionIntent,
        ) -> Result<(), HostError> {
            self.inner.charge_budget(order, intent)
        }
        fn accepted(&self, intent: &ExecutionIntent) -> Result<bool, HostError> {
            self.inner.accepted(intent)
        }
        fn latest_run(
            &self,
            order: &str,
            attempt: u64,
        ) -> Result<Option<foundry_exec::RunRecord>, HostError> {
            self.inner.latest_run(order, attempt)
        }
        fn mark_hold_expired(
            &mut self,
            request: foundry_exec::ReviewHoldUpdate,
        ) -> Result<(), HostError> {
            self.inner.mark_hold_expired(request)
        }
        fn begin_hold_resume(
            &mut self,
            request: foundry_exec::ReviewHoldUpdate,
        ) -> Result<(), HostError> {
            self.inner.begin_hold_resume(request)
        }
        fn restore_hold_after_resume_error(
            &mut self,
            binding: foundry_exec::ReviewRunBinding,
        ) -> Result<(), HostError> {
            self.inner.restore_hold_after_resume_error(binding)
        }
        fn resolve_hold_result(
            &mut self,
            request: foundry_exec::ReviewHoldResolution,
        ) -> Result<(), HostError> {
            self.inner.resolve_hold_result(request)
        }
        fn save_redrive_output(
            &mut self,
            order: &str,
            intent: &ExecutionIntent,
            run: &str,
            sequence: u64,
            stage: &str,
            session_key: &str,
            session_id: &str,
            output: &foundry_exec::ReviewStageEvidence,
        ) -> Result<(), HostError> {
            self.inner.save_redrive_output(
                order,
                intent,
                run,
                sequence,
                stage,
                session_key,
                session_id,
                output,
            )
        }
        fn continuation_binding(
            &self,
            order: &str,
            attempt: i64,
            run: &str,
        ) -> Result<foundry_exec::ReviewContinuationBinding, HostError> {
            self.inner.continuation_binding(order, attempt, run)
        }
        fn continuation_counters(
            &mut self,
            request: foundry_exec::ReviewCounterCheckpoint,
            pack: &str,
        ) -> Result<(), HostError> {
            self.inner.continuation_counters(request, pack)
        }
        fn continuation_session(
            &self,
            order: &str,
            session: &str,
        ) -> Result<Option<foundry_exec::ReviewSessionReceipt>, HostError> {
            self.inner.continuation_session(order, session)
        }
        fn continuation_budget_and_start(
            &mut self,
            request: ReviewStageStart,
            pack: &str,
        ) -> Result<(), HostError> {
            self.inner.continuation_budget_and_start(request, pack)
        }
        fn finish_continuation_stage(
            &mut self,
            request: ReviewStageFinishRequest,
            pack: &str,
        ) -> Result<(), HostError> {
            self.inner.finish_continuation_stage(request, pack)
        }
        fn complete_continuation(
            &mut self,
            request: foundry_exec::ReviewContinuationCompletion,
        ) -> Result<(), HostError> {
            self.inner.complete_continuation(request)
        }
        fn latest_hold(
            &self,
            order: &str,
        ) -> Result<Option<foundry_exec::ReviewHoldRecord>, HostError> {
            self.inner.latest_hold(order)
        }
        fn run_result(
            &self,
            order: &str,
            attempt: i64,
            run: &str,
        ) -> Result<HostResult, HostError> {
            self.inner.run_result(order, attempt, run)
        }
        fn binding_intent(
            &self,
            intent: &ExecutionIntent,
        ) -> Result<Option<ExecutionIntent>, HostError> {
            self.inner.binding_intent(intent)
        }
        fn claim_accepted_hold(
            &mut self,
            session_digest: &str,
            operation_id: &str,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<foundry_exec::ReviewAcceptedHold, HostError> {
            self.inner
                .claim_accepted_hold(session_digest, operation_id, now)
        }
        fn finalize_accepted_hold(
            &mut self,
            claim: foundry_exec::ReviewAcceptedHold,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), HostError> {
            self.inner.finalize_accepted_hold(claim, now)
        }
    }
    impl ReviewAdmissionStore for RecordingReviewStore {
        fn validate_product_work_claim(
            &self,
            claim: &foundry_exec::ReviewWorkClaim,
            intent: &ExecutionIntent,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), HostError> {
            self.inner.validate_product_work_claim(claim, intent, now)
        }
        fn admit_review_activation(
            &mut self,
            request: ReviewAdmissionRequest,
        ) -> Result<ReviewAdmission, HostError> {
            self.calls.lock().unwrap().push("admission");
            if self.reject_admission {
                return Err(HostError::Acceptance("supplied backend reached".into()));
            }
            self.inner.admit_review_activation(request)
        }
    }
    impl ReviewDispatchStore for RecordingReviewStore {
        fn load_review_result(&self, key: &HostKey) -> Result<Option<HostResult>, HostError> {
            self.calls.lock().unwrap().push("result_lookup");
            self.inner.load_review_result(key)
        }
        fn checkpoint_review_counters(
            &mut self,
            request: foundry_exec::ReviewCounterCheckpoint,
        ) -> Result<(), HostError> {
            self.calls.lock().unwrap().push("counter_checkpoint");
            self.inner.checkpoint_review_counters(request)
        }
        fn start_review_stage(&mut self, request: ReviewStageStart) -> Result<(), HostError> {
            self.calls.lock().unwrap().push("stage_start");
            self.inner.start_review_stage(request)
        }
        fn finish_review_stage(
            &mut self,
            request: ReviewStageFinishRequest,
        ) -> Result<(), HostError> {
            self.calls.lock().unwrap().push("stage_finish");
            self.inner.finish_review_stage(request)
        }
        fn fail_review_dispatch(
            &mut self,
            request: foundry_exec::ReviewDispatchFailure,
        ) -> Result<(), HostError> {
            self.calls.lock().unwrap().push("dispatch_failure");
            self.inner.fail_review_dispatch(request)
        }
        fn complete_review_dispatch(
            &mut self,
            request: foundry_exec::ReviewDispatchCompletion,
        ) -> Result<(), HostError> {
            self.calls.lock().unwrap().push("dispatch_completion");
            self.inner.complete_review_dispatch(request)
        }
    }

    struct CompletingExecutor;
    impl StageExecutor for CompletingExecutor {
        fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
            fs::write(&task.workdir.join("output.log"), b"checked stage output")
                .map_err(|e| e.to_string())?;
            Ok(StageOutput {
                log_path: task
                    .workdir
                    .join("output.log")
                    .to_string_lossy()
                    .into_owned(),
                inspected_commit: Some(task.artifact_commit.clone()),
                ..StageOutput::default()
            })
        }
    }

    #[test]
    fn supplied_backend_receives_real_host_dispatch_stage_and_terminal_writes() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let db = std::env::temp_dir().join(format!(
            "review-injected-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
        let store = StandaloneReviewStore::open(&db, REVIEW_PACK_DIGEST).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut host = PackReviewHost::with_store(
            pack,
            CompletingExecutor,
            Box::new(RecordingReviewStore {
                inner: store,
                calls: calls.clone(),
                reject_admission: false,
            }),
            None,
            false,
        )
        .unwrap()
        .with_source_commit("a".repeat(40))
        .unwrap();
        let intent = ExecutionIntent {
            order: foundry_exec::OrderId("injected-dispatch".into()),
            attempt: Attempt(1),
            run: RunId(7),
            activation: 1,
            generation: foundry_exec::Generation(0),
            epoch: Epoch(1),
            external_ref: "review:injected-dispatch:1:base..result".into(),
            recorded_at: chrono::Utc::now(),
        };
        let result = host.try_dispatch(&intent).unwrap();
        assert_eq!(result.exit_code, 0);
        let calls = calls.lock().unwrap().clone();
        assert!(
            calls.contains(&"admission"),
            "host bypassed injected admission owner: {calls:?}"
        );
        assert!(
            calls.contains(&"stage_start"),
            "host did not persist a stage claim through injected owner: {calls:?}"
        );
        assert!(
            calls.contains(&"stage_finish"),
            "host did not persist stage evidence through injected owner: {calls:?}"
        );
        assert!(
            calls.contains(&"dispatch_completion"),
            "host did not persist terminal evidence through injected owner: {calls:?}"
        );
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(db.with_extension("db-wal"));
        let _ = fs::remove_file(db.with_extension("db-shm"));
        let _ = fs::remove_dir_all(db.with_extension("review-runs"));
    }

    #[test]
    fn shared_host_algorithm_calls_the_supplied_storage_backend() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let db = std::env::temp_dir().join(format!(
            "review-backend-{}-{}.db",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
        let store = StandaloneReviewStore::open(&db, REVIEW_PACK_DIGEST).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut host = PackReviewHost::with_store(
            pack,
            ProbeExecutor,
            Box::new(RecordingReviewStore {
                inner: store,
                calls: calls.clone(),
                reject_admission: true,
            }),
            None,
            false,
        )
        .unwrap()
        .with_source_commit("a".repeat(40))
        .unwrap();
        let intent = ExecutionIntent {
            order: foundry_exec::OrderId("backend-order".into()),
            attempt: Attempt(1),
            run: RunId(1),
            activation: 1,
            generation: foundry_exec::Generation(0),
            epoch: Epoch(1),
            external_ref: "review:backend-order:1:base..result".into(),
            recorded_at: chrono::Utc::now(),
        };
        let error = host.try_dispatch(&intent).unwrap_err();
        assert!(
            matches!(error,HostError::Acceptance(ref message) if message=="supplied backend reached"),
            "unexpected host result: {error:?}"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec!["result_lookup", "admission"],
            "host bypassed the supplied backend"
        );
        drop(host);
        let _ = fs::remove_file(&db);
        let _ = fs::remove_file(db.with_extension("db-wal"));
        let _ = fs::remove_file(db.with_extension("db-shm"));
    }

    #[test]
    fn seal_refuses_missing_predecessor_before_opening_inputs() {
        let stage: toml::Value = toml::from_str("id='claims'\nafter=['edge']\n").unwrap();
        assert!(
            sealed_inputs(&stage, &BTreeMap::new())
                .unwrap_err()
                .message
                .contains("seal violation")
        );
        let done = BTreeMap::from([("edge".into(), "edge-output".into())]);
        assert_eq!(sealed_inputs(&stage, &done).unwrap(), done);
    }
    #[test]
    fn provenance_requires_explicit_build_identity() {
        assert!(
            provenance(REVIEW_PACK_DIGEST, None)
                .unwrap_err()
                .contains("FOUNDRY_SOURCE_COMMIT")
        );
        assert!(provenance(REVIEW_PACK_DIGEST, Some("HEAD")).is_err());
    }
    #[test]
    fn startup_recovers_durable_resuming_run_to_held() {
        let db = std::env::temp_dir().join(format!("review-resuming-{}.db", std::process::id()));
        let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
        let conn = open_db(&db).unwrap();
        conn.execute("INSERT INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,created_at) VALUES('order',1,'order:1:1:1','base','result',?1,'resuming','{}',0)",[REVIEW_PACK_DIGEST]).unwrap();
        drop(conn);
        let host = PackReviewHost::new(pack, UnavailableStageExecutor, &db).unwrap();
        let status: String = open_db(&db)
            .unwrap()
            .query_row(
                "SELECT status FROM review_runs WHERE order_id='order'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "held");
        drop(host);
        let _ = fs::remove_file(db);
    }
    #[test]
    fn hold_cas_does_not_overwrite_a_competing_snapshot() {
        let db = std::env::temp_dir().join(format!("review-hold-cas-{}.db", std::process::id()));
        let conn = open_db(&db).unwrap();
        conn.execute("INSERT INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,hold_json,hold_instance_id,deadline,created_at) VALUES('order',1,'run','base','result',?1,'held','{}','{\"snapshot\":\"original\"}','hold-id',100,0)",[REVIEW_PACK_DIGEST]).unwrap();
        drop(conn);
        let mut store = StandaloneReviewStore::open(&db, REVIEW_PACK_DIGEST).unwrap();
        let expected = r#"{"snapshot":"original"}"#;
        let competitor = r#"{"snapshot":"competitor"}"#;
        store
            .conn
            .execute(
                "UPDATE review_runs SET hold_json=?1 WHERE order_id='order' AND run_id='run'",
                [competitor],
            )
            .unwrap();
        let binding = foundry_exec::ReviewRunBinding {
            order_id: "order".into(),
            attempt: 1,
            journal_run: "run".into(),
        };
        let intent = ExecutionIntent {
            order: foundry_exec::OrderId("order".into()),
            attempt: Attempt(1),
            run: RunId(1),
            activation: 1,
            generation: foundry_exec::Generation(0),
            epoch: Epoch(1),
            external_ref: "review:order:1:base..result".into(),
            recorded_at: chrono::Utc::now(),
        };
        let hold = foundry_exec::host::HoldState {
            hold_instance_id: "hold-id".into(),
            reason: "test".into(),
            resume_targets: vec![],
            deadline_secs: 0,
        };
        let snapshot = foundry_exec::ReviewContinuationSnapshot {
            binding: binding.clone(),
            snapshot_run_id: "run".into(),
            held: foundry_exec::ReviewHeldSnapshot {
                hold,
                node_id: "node".into(),
                entry_counters: BTreeMap::new(),
                requested_revert: None,
                engine_state: String::new(),
                session_ids: vec![],
                accepted_logs: vec![],
                generations: vec![],
                latest_commit: "result".into(),
                sequence: 0,
            },
            intent,
            resume_sequence: 0,
            human_events: vec![],
        };
        let request = foundry_exec::ReviewHoldUpdate {
            expected: foundry_exec::ReviewHoldExpectation {
                binding,
                hold_id: "hold-id".into(),
                deadline: 100,
                revision: foundry_exec::ReviewHoldRevision::from_storage_digest(
                    Sha256::digest(expected.as_bytes()).into(),
                ),
            },
            now: 10,
            snapshot,
        };
        let result = store.begin_hold_resume(request);
        assert!(result.is_err(), "stale hold snapshot must lose the CAS");
        let (status, snapshot): (String, String) = store
            .conn
            .query_row(
                "SELECT status,hold_json FROM review_runs WHERE order_id='order' AND run_id='run'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((status, snapshot), ("held".into(), competitor.into()));
        drop(store);
        let _ = fs::remove_file(db);
    }
    #[test]
    fn typed_continuation_snapshot_roundtrips_the_standalone_evidence_shape() {
        let binding = foundry_exec::ReviewRunBinding {
            order_id: "typed-hold".into(),
            attempt: 2,
            journal_run: "typed-hold:2:7:3".into(),
        };
        let intent = ExecutionIntent {
            order: foundry_exec::OrderId("typed-hold".into()),
            attempt: Attempt(2),
            run: RunId(7),
            activation: 3,
            generation: foundry_exec::Generation(0),
            epoch: Epoch(9),
            external_ref: "review:typed-hold:2:base..result".into(),
            recorded_at: chrono::Utc::now(),
        };
        let snapshot = foundry_exec::ReviewContinuationSnapshot {
            binding: binding.clone(),
            snapshot_run_id: "typed-hold:2:7:3".into(),
            held: foundry_exec::ReviewHeldSnapshot {
                hold: foundry_exec::host::HoldState {
                    hold_instance_id: "hold-typed".into(),
                    reason: "review".into(),
                    resume_targets: vec!["verify".into()],
                    deadline_secs: 60,
                },
                node_id: "review".into(),
                entry_counters: BTreeMap::from([("review".into(), 2)]),
                requested_revert: Some("base".into()),
                engine_state: "engine-state".into(),
                session_ids: vec!["session-a".into()],
                accepted_logs: vec!["/private/log".into()],
                generations: vec![foundry_exec::GenerationCommit {
                    generation: 1,
                    commit: "result".into(),
                    fix_log_path: "/private/fix.log".into(),
                }],
                latest_commit: "result".into(),
                sequence: 4,
            },
            intent,
            resume_sequence: 1,
            human_events: vec![foundry_exec::ReviewHumanEvent {
                actor: "reviewer".into(),
                occurred_at: "2026-01-01T00:00:00Z".into(),
                action: foundry_exec::ReviewHumanAction::Resume {
                    target: "verify".into(),
                },
            }],
        };
        let encoded = encode_continuation_snapshot(&snapshot).unwrap();
        assert_eq!(
            decode_continuation_snapshot(binding, &encoded).unwrap(),
            snapshot
        );
    }
    #[test]
    fn tuple_seal_refuses_immutable_commit_different_from_final_head() {
        assert!(
            validate_tuple_seal("sealed-commit", "final-head")
                .unwrap_err()
                .contains("BindingMismatch")
        );
        assert!(validate_tuple_seal("final-head", "final-head").is_ok());
    }
}
