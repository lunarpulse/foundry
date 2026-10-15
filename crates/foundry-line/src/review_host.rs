//! Product adapter from durable review dispatches to a frozen pack graph.
use foundry_exec::{
    ArtifactTuple, ArtifactTupleKey, Attempt, DispatchHost, DurableReviewRun, EmitLog, EmitReceipt,
    EmitSink, Epoch, ExecutionIntent, HostError, HostKey, HostResult,
    IntentLog, OrderId, PromotionReceipt, RunId, TupleStore,
};
use foundry_pack::{load_pack, EngineAction, GraphEngine, RunInput};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, sync::{Arc, Mutex}};

const REVIEW_PACK_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";

#[derive(Clone, Debug)]
pub struct StageTask {
    pub stage_id: String, pub role: String, pub lens_bytes: Vec<u8>, pub session_id: String,
    pub fix_context: Option<String>, pub base_commit: String, pub workdir: PathBuf,
    pub artifact_commit: String, pub inputs: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default)]
pub struct StageOutput { pub verdict_category: Option<String>, pub findings: bool, pub log_path: String, pub inspected_commit: Option<String>, pub verdict_categories: Vec<String>, pub produced_commit: Option<String> }
/// The adapter must normalize output, stop all writes before returning, and
/// confine process, git-ref, and session writes to the supplied workdir.
pub trait StageExecutor: Send {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String>;
    fn ensure_available(&self) -> Result<(), String> { Ok(()) }
    fn restore_session(&mut self, _session_id: &str, _workdir: &Path) -> Result<(), String> {
        Err("session restoration is not implemented".into())
    }
}
/// Product wiring placeholder until the real LLM/Die adapter is supplied by T33.
#[derive(Default)] pub struct UnavailableStageExecutor;
impl StageExecutor for UnavailableStageExecutor { fn ensure_available(&self)->Result<(),String>{Err("review stage executor is not configured".into())} fn execute(&mut self, _task:&StageTask)->Result<StageOutput,String>{Err("review stage executor is not configured".into())} }

type ActiveReviewRow = (String, i64, Option<String>, i64, String, String, Option<String>);

pub struct PackReviewHost { pack_path: PathBuf, db_path: PathBuf, executor: Box<dyn StageExecutor>, conn: Connection, pack_digest: String, dispatches: u64, source_commit: Option<String> }
impl PackReviewHost {
    pub fn new(pack_path: impl AsRef<Path>, executor: impl StageExecutor + 'static, db_path: impl AsRef<Path>) -> Result<Self, HostError> {
        if !pack_path.as_ref().is_absolute() { return Err(HostError::Persistence("FOUNDRY_REVIEW_PACK must be absolute".into())); }
        let snap = load_pack(pack_path.as_ref()).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != REVIEW_PACK_DIGEST { return Err(HostError::Persistence(format!("unexpected review pack digest: {}", snap.digest()))); }
        let conn = open_db(db_path.as_ref()).map_err(HostError::Persistence)?;
        Ok(Self { pack_path: pack_path.as_ref().to_path_buf(), db_path: fs::canonicalize(db_path.as_ref()).map_err(sqlerr).map_err(HostError::Persistence)?, executor: Box::new(executor), conn, pack_digest: snap.digest().to_owned(), dispatches: 0, source_commit: option_env!("FOUNDRY_SOURCE_COMMIT").map(str::to_owned).or_else(||std::env::var("FOUNDRY_SOURCE_COMMIT").ok()) })
    }
    pub fn with_source_commit(mut self, commit: impl Into<String>) -> Result<Self, HostError> {
        let commit = commit.into();
        validate_source_commit(&commit).map_err(HostError::Persistence)?;
        self.source_commit = Some(commit);
        Ok(self)
    }
    pub fn default_db_path() -> PathBuf { expand_home("~/.foundry/review_host.db") }
    pub fn database_path(&self) -> &Path { &self.db_path }
    pub fn resume(&mut self,order:&str,hold_id:&str,target:&str,actor:&str)->Result<(),HostError>{
        <Self as DispatchHost>::resolve_hold(self,order,hold_id,foundry_exec::ReviewHoldAction::Resume{target:target.into()},actor)
    }
    pub fn commit_pass(&mut self,order:&str,hold_id:&str,actor:&str)->Result<(),HostError>{
        <Self as DispatchHost>::resolve_hold(self,order,hold_id,foundry_exec::ReviewHoldAction::CommitPass,actor)
    }
    pub fn commit_fail(&mut self,order:&str,hold_id:&str,actor:&str)->Result<(),HostError>{
        <Self as DispatchHost>::resolve_hold(self,order,hold_id,foundry_exec::ReviewHoldAction::CommitFail,actor)
    }
    pub fn abort(&mut self,order:&str,hold_id:&str,actor:&str)->Result<(),HostError>{
        <Self as DispatchHost>::resolve_hold(self,order,hold_id,foundry_exec::ReviewHoldAction::Abort,actor)
    }
    pub fn sqlite_ports(path: impl AsRef<Path>) -> Result<(SqliteReviewSink, SqliteReviewLog, SqliteIntentLog), String> {
        let p = path.as_ref(); open_db(p)?;
        let scope = Arc::new(Mutex::new(None));
        Ok((SqliteReviewSink { conn: Mutex::new(open_db(p)?), scope: scope.clone() },
            SqliteReviewLog { conn: Mutex::new(open_db(p)?), scope: scope.clone() },
            SqliteIntentLog { conn: Mutex::new(open_db(p)?), scope }))
    }
}
impl DispatchHost for PackReviewHost {
    type Result = HostResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> HostResult { self.try_dispatch(intent).unwrap_or_else(|e| panic!("review host: {e:?}")) }
    fn try_dispatch(&mut self, intent: &ExecutionIntent) -> Result<HostResult, HostError> {
        let key = host_key(intent);
        let (order, attempt, base, result) = parse_external_ref(&intent.external_ref).ok_or_else(|| HostError::Worker("invalid review external_ref".into()))?;
        if order != intent.order.0 || attempt != intent.attempt.0 { return Err(HostError::Worker("external_ref identity mismatch".into())); }
        validate_intent(intent).map_err(HostError::Worker)?;
        self.check_binding(intent)?;
        if let Some(r) = self.load_result(&key)? { return Ok(r); }
        let snap = load_pack(&self.pack_path).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != self.pack_digest { return Err(HostError::Persistence("review pack digest changed".into())); }
        let digest = snap.digest().to_owned();
        let binary = provenance(&digest, self.source_commit.as_deref()).map_err(HostError::Persistence)?;
        let engine = GraphEngine::from_snapshot(&snap).map_err(|e| HostError::Persistence(e.to_string()))?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        let accepted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64], |r| r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?;
        if accepted { return Err(HostError::Persistence("accepted activation has no recoverable result; refusing redispatch".into())); }
        let previous: Option<ActiveReviewRow> = tx.query_row("SELECT r.status,a.epoch,r.result_json,r.attempt,r.base_commit,r.result_commit,r.hold_json FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE r.order_id=?1", [&order], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))).optional().map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut inherited_result=None;let mut inherited_hold_json=None;
        if let Some((status, epoch, previous_result, previous_attempt, previous_base, previous_commit, previous_hold_json)) = previous {
            if intent.epoch.0 < epoch as u64 || matches!(status.as_str(), "held" | "resuming") || (status == "running" && intent.epoch.0 <= epoch as u64) {
                return Err(HostError::Acceptance("stale, held, or still-running order cannot start this activation".into()));
            }
            if status == "running" { tx.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status='running'", [&order]).map_err(sqlerr).map_err(HostError::Persistence)?; }
            if (status=="resolved" || status=="review_aborted") && previous_attempt==attempt as i64 {
                if previous_base!=base || previous_commit!=result { return Err(HostError::Acceptance("resolved review input binding mismatch".into())); }
                inherited_result=previous_result;inherited_hold_json=previous_hold_json;
            }
        }
        advance_epoch(&tx, &order, intent.epoch).map_err(HostError::Acceptance)?;
        let inherited = latest_counters(&tx, &order)?;
        let run_id = format!("{}:{}:{}:{}", order, attempt, intent.run.0, intent.activation);
        tx.execute("INSERT INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,unixepoch())",
            params![order, attempt as i64, run_id, base, result, digest, serde_json::to_string(&inherited).map_err(sqlerr).map_err(HostError::Persistence)?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_accept(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64]).map_err(sqlerr).map_err(HostError::Acceptance)?;
        if let Some(inherited)=inherited_result.as_ref() {
            let inherited_result:HostResult=serde_json::from_str(inherited).map_err(sqlerr).map_err(HostError::Persistence)?;
            let status=if inherited_result.held.is_some() {"held"} else if inherited_result.exit_code==0 {"passed"} else {"failed"};
            if let Some(hold)=inherited_result.held.as_ref() {
                let mut doc:serde_json::Value=serde_json::from_str(inherited_hold_json.as_deref().ok_or_else(||HostError::Persistence("inherited hold has no snapshot".into()))?).map_err(sqlerr).map_err(HostError::Persistence)?;
                let source_run=doc["run_id"].as_str().ok_or_else(||HostError::Persistence("inherited hold has no run identity".into()))?.to_owned();
                if doc.get("snapshot_run_id").is_none(){doc["snapshot_run_id"]=serde_json::json!(source_run);}
                doc["intent"]=serde_json::to_value(intent).map_err(sqlerr).map_err(HostError::Persistence)?;
                doc["run_id"]=serde_json::json!(run_id);
                tx.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) SELECT order_id,?1,sequence,stage_id,status,output_json FROM review_stage_attempts WHERE order_id=?2 AND run_id=?3",params![run_id,order,source_run]).map_err(sqlerr).map_err(HostError::Persistence)?;
                tx.execute("UPDATE review_runs SET hold_json=?1,hold_instance_id=?2,resume_targets=?3,deadline=?4 WHERE order_id=?5 AND attempt=?6 AND run_id=?7",params![doc.to_string(),hold.hold_instance_id,serde_json::to_string(&hold.resume_targets).map_err(sqlerr).map_err(HostError::Persistence)?,if hold.deadline_secs==0{0}else{chrono::Utc::now().timestamp().saturating_add(hold.deadline_secs as i64)},order,attempt as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
            }
            tx.execute("UPDATE review_runs SET status=?1,result_json=?2 WHERE order_id=?3 AND attempt=?4 AND run_id=?5",params![status,inherited,order,attempt as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
            tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",params![key.order,key.attempt as i64,key.run as i64,key.activation as i64,inherited]).map_err(sqlerr).map_err(HostError::Persistence)?;
        }
        tx.execute("INSERT INTO review_bindings(key_json,intent_json) VALUES(?1,?2)",params![key_json(intent),serde_json::to_string(intent).map_err(sqlerr).map_err(HostError::Persistence)?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT OR REPLACE INTO review_active(order_id,key_json,epoch,journal_run) VALUES(?1,?2,?3,?4)",params![order,key_json(intent),intent.epoch.0 as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        // A resolved hold is the completed result for the newly accepted
        // activation. Recover that immutable result instead of executing the
        // review graph a second time.
        if let Some(inherited) = inherited_result {
            return serde_json::from_str(&inherited)
                .map_err(|e| HostError::Persistence(format!("invalid inherited host result: {e}")));
        }
        self.dispatches += 1;
        let mut state = engine.initial_state(inherited);
        let run_root=self.db_path.with_extension("review-runs").join(format!("{:x}",Sha256::digest(run_id.as_bytes())));
        let mut snapshot_written=false;
        let mut sequence = 0_u64;
        let mut session_ids = BTreeSet::new();
        let mut fresh_sessions = BTreeMap::new();
        let mut accepted_logs = Vec::new();
        let mut generations = vec![foundry_exec::GenerationCommit { generation: 0, commit: result.clone(), fix_log_path: String::new() }];
        let mut latest_commit = result.clone();

        let output = RunInput { verdicts: vec![], has_findings: false, budget_exhausted: false, timeout: false };
        let outcome = engine.run_staged_checkpointed(&mut state, &output, 10_000, |node_id, redrive, input, counters| {
            self.conn.execute("UPDATE review_runs SET counters_json=?1 WHERE order_id=?2 AND run_id=?3", params![serde_json::to_string(counters).map_err(packerr)?,order,run_id]).map_err(packerr)?;
            if redrive {
                let generation = generations.len() as u64;
                let node = snap.nodes().iter().find(|n| n.id == node_id).ok_or_else(|| packerr(format!("unknown redrive node {node_id}")))?;
                let role = if node_id == "fixdie" {
                    // T32's original fixdie node has no separate pack_stages
                    // entry; ADDENDUM 12 defines its implementation role.
                    "fix".to_owned()
                } else {
                    let pre_stage = node.value.get("pre_stage").and_then(toml::Value::as_str).ok_or_else(||packerr(format!("redrive node {node_id} has no pre_stage")))?;
                    let stage_def = snap.policy().get("pack_stages").and_then(toml::Value::as_array)
                        .and_then(|stages| stages.iter().find(|stage| stage.get("id").and_then(toml::Value::as_str)==Some(pre_stage)))
                        .ok_or_else(|| packerr(format!("redrive node {node_id} has no pre_stage definition")))?;
                    stage_def.get("role").and_then(toml::Value::as_str).ok_or_else(||packerr("redrive pre_stage missing role"))?.to_owned()
                };
                let payload = node.value.get("payload").and_then(toml::Value::as_str).unwrap_or("findings");
                let session_id=redrive_session_key(&order,node_id);
                let saved:Option<String>=self.conn.query_row("SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_id],|r|r.get(0)).optional().map_err(sqlerr).map_err(packerr)?;
                let saved=saved.map(|s|serde_json::from_str::<SessionReceipt>(&s).map_err(packerr)).transpose()?;
                let task_session=saved.as_ref().map(|r|r.session_id.clone()).unwrap_or_else(||format!("{session_id}:fresh:{run_id}"));
                let workdir=run_root.join("exec").join(format!("{}-{sequence}-{}-g{generation}",intent.activation,node_id)).join("0");
                fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;fs::create_dir(&workdir).map_err(packerr)?;
                let task_session=if let Some(saved)=saved.as_ref() {
                    if session_output_valid(saved) && self.executor.restore_session(&saved.session_id,&workdir).is_ok(){saved.session_id.clone()}else{self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')",[&order]).map_err(packerr)?;format!("{session_id}:fresh:{run_id}")}
                } else {task_session};
                let mut inputs=BTreeMap::new();
                let payload_rows = redrive_payload(&self.conn,&order,&run_id,payload)?;
                inputs.insert(payload.to_owned(),serde_json::to_string(&payload_rows).map_err(packerr)?);
                let task=StageTask{stage_id:format!("{node_id}-g{generation}"),role:role.clone(),lens_bytes:Vec::new(),session_id:task_session.clone(),fix_context:Some(latest_commit.clone()),base_commit:latest_commit.clone(),artifact_commit:latest_commit.clone(),inputs,workdir};
                charge_review_budget(&mut self.conn,&order,intent)?;
                let out=self.executor.execute(&task).and_then(|out|accept_output(&task,out)).map_err(|e|foundry_pack::PackError{code:"E_EXEC_REDRIVE_ERROR",message:e})?;
                let Some(commit)=out.produced_commit.clone().filter(|commit|!commit.is_empty()) else {return Err(foundry_pack::PackError{code:"E_EXEC_REDRIVE_ERROR",message:"fix stage did not produce a commit".into()});};
                sequence+=1;
                self.conn.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'complete',?5)",params![order,run_id,sequence as i64,task.stage_id,serde_json::json!({"role":role,"log_path":out.log_path,"produced_commit":commit}).to_string()]).map_err(sqlerr).map_err(packerr)?;
                self.conn.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![order,run_id,session_id,serde_json::to_string(&SessionReceipt{session_id:task.session_id,output:out.log_path.clone(),sha256:format!("{:x}",Sha256::digest(fs::read(&out.log_path).map_err(packerr)?))}).map_err(packerr)?]).map_err(sqlerr).map_err(packerr)?;
                accepted_logs.push(out.log_path.clone());
                generations.push(foundry_exec::GenerationCommit{generation,commit:commit.clone(),fix_log_path:out.log_path});
                latest_commit=commit;
                return Ok(input.clone());
            }
            if !snapshot_written { persist_snapshot(&run_root,&snap).map_err(packerr)?;snapshot_written=true; }
            let node = snap.nodes().iter().find(|n| n.id == node_id).ok_or_else(|| packerr(format!("unknown stage node {node_id}")))?;
            let stage_values = node.value.get("stages").and_then(toml::Value::as_array).cloned().ok_or_else(||packerr("dispatch has no stages"))?;
            let mut done = BTreeMap::new();
            let mut verdicts = Vec::new(); let mut findings = false;
            let mut valid = 0; let mut verify_failed = false;
            for stage in stage_values {
                let id = stage.get("id").and_then(toml::Value::as_str).ok_or_else(|| packerr("stage missing id"))?.to_owned();
                let role=stage.get("role").and_then(toml::Value::as_str).ok_or_else(|| packerr("stage missing role"))?.to_owned();
                let inputs=match sealed_inputs(&stage,&done) {
                    Ok(inputs)=>inputs,
                    Err(error)=>{
                        sequence+=1;
                        self.conn.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'blocked',?5)",params![order,run_id,sequence as i64,id,error.to_string()]).map_err(packerr)?;
                        verify_failed |= role=="verify";
                        continue;
                    }
                };
                let session=stage.get("session").and_then(toml::Value::as_str).unwrap_or(&id).to_owned();
                let session_id=format!("{}:{}", order, session);
                let lens_names=stage.get("lenses").and_then(toml::Value::as_array).ok_or_else(|| packerr("stage missing lenses"))?;
                let mut lens_bytes=Vec::new(); for name in lens_names { let name=name.as_str().ok_or_else(||packerr("lens path not string"))?; let b=snap.files().get(name).ok_or_else(||packerr(format!("lens not in snapshot: {name}")))?; lens_bytes.extend_from_slice(b); lens_bytes.push(b'\n'); }
                let saved_session: Option<String> = self.conn.query_row("SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_id],|r|r.get(0)).optional().map_err(sqlerr).map_err(packerr)?;
                let saved= saved_session.as_ref().map(|saved|serde_json::from_str::<SessionReceipt>(saved).map_err(packerr)).transpose()?;
                let restored_id=saved.as_ref().map(|saved|saved.session_id.clone()).unwrap_or_else(||session_id.clone());
                let saved_output_valid=saved.as_ref().is_none_or(|saved|fs::read(&saved.output).is_ok_and(|bytes|format!("{:x}",Sha256::digest(bytes))==saved.sha256));
                let mut task_session = fresh_sessions.get(&session_id).cloned().unwrap_or_else(|| restored_id.clone());
                let needs_restore=session_ids.insert(session_id.clone()) && saved_session.is_some();
                sequence += 1;
                let workdir=run_root.join("exec")
                    .join(format!("{}-{sequence}",intent.activation)).join("0");
                let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(packerr)?;
                ensure_active(&tx,intent).map_err(packerr)?;
                tx.execute("INSERT OR IGNORE INTO review_budget(order_id,used) VALUES(?1,0)",[&order]).map_err(packerr)?;
                if tx.execute("UPDATE review_budget SET used=used+1 WHERE order_id=?1 AND used<10000",[&order]).map_err(packerr)? == 0 {
                    return Err(foundry_pack::PackError{code:"E_EXEC_BUDGET",message:"order execution budget exhausted".into()});
                }
                tx.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",params![order,run_id,sequence as i64,id]).map_err(packerr)?;
                tx.commit().map_err(packerr)?;
                fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;
                fs::create_dir(&workdir).map_err(packerr)?;
                if needs_restore && (!saved_output_valid || self.executor.restore_session(&restored_id,&workdir).is_err()) {
                    self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')",[&order]).map_err(sqlerr).map_err(packerr)?;
                    task_session = format!("{order}:{session}:fresh:{run_id}");
                    fresh_sessions.insert(session_id.clone(), task_session.clone());
                }
                let task=StageTask { stage_id:id.clone(), role:role.clone(), lens_bytes, session_id:task_session,
                    fix_context:None, base_commit:base.clone(), artifact_commit:latest_commit.clone(), inputs, workdir };
                let out=match self.executor.execute(&task).and_then(|out|accept_output(&task,out)) {
                    Ok(out)=>out,
                    Err(error)=>{
                        self.conn.execute("UPDATE review_stage_attempts SET status='error',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4",params![error,order,run_id,sequence as i64]).map_err(packerr)?;
                        verify_failed |= role == "verify";
                        continue;
                    }
                };
                let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(packerr)?;
                ensure_active(&tx,intent).map_err(packerr)?;
                let is_defer=role=="verify" && out.verdict_category.iter().chain(&out.verdict_categories).any(|v|v=="defer");
                tx.execute("UPDATE review_stage_attempts SET status='complete',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4",params![serde_json::json!({"log_path":out.log_path,"sha256":format!("{:x}",Sha256::digest(fs::read(&out.log_path).map_err(packerr)?)),"deferred":is_defer,"role":role,"inspected_commit":out.inspected_commit,"findings":out.findings,"verdicts":out.verdict_category.iter().chain(&out.verdict_categories).collect::<Vec<_>>()}).to_string(),order,run_id,sequence as i64]).map_err(packerr)?;
                tx.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![order,run_id,session_id,serde_json::to_string(&SessionReceipt{session_id:task.session_id,output:out.log_path.clone(),sha256:format!("{:x}",Sha256::digest(fs::read(&out.log_path).map_err(packerr)?))}).map_err(packerr)?]).map_err(packerr)?;
                tx.commit().map_err(packerr)?;
                valid += 1;
                done.insert(id,out.log_path.clone());
                accepted_logs.push(out.log_path.clone());
                if role=="verify" {
                    verdicts.extend(out.verdict_category); verdicts.extend(out.verdict_categories);
                    findings=out.findings || verdicts.iter().any(|v|matches!(v.as_str(),"patch"|"intent_gap"|"bad_plan"));
                }
            }
            if valid==0 { return Err(foundry_pack::PackError{code:"E_EXEC_ALL_ERROR",message:"no valid stage output".into()}); }
            if verify_failed { return Err(foundry_pack::PackError{code:"E_EXEC_VERIFY_INVALID",message:"verify output is invalid".into()}); }
            Ok(RunInput { verdicts, has_findings:findings, budget_exhausted:input.budget_exhausted, timeout:input.timeout })
        });
        let outcome=match outcome {
            Ok(outcome)=>outcome,
            Err(error)=>{
                let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
                tx.execute("UPDATE review_runs SET status='review_aborted',counters_json=?1 WHERE order_id=?2 AND run_id=?3 AND status='running'",params![serde_json::to_string(&state.counters).map_err(sqlerr).map_err(HostError::Persistence)?,order,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
                tx.execute("INSERT INTO review_failures(order_id,run_id,error) VALUES(?1,?2,?3)",params![order,run_id,error.to_string()]).map_err(sqlerr).map_err(HostError::Persistence)?;
                tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
                return Err(HostError::Worker(error.to_string()));
            }
        };
        let passed=outcome.terminal && outcome.actions.iter().any(|a|matches!(a,EngineAction::Emit{event,..} if event=="InspectionPassed"));
        let hold_state=outcome.hold.as_ref().map(|entry| {
            let node=snap.nodes().iter().find(|n|n.id==entry.node_id);
            let resume_targets=node.and_then(|n|n.value.get("resume_targets")).and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).map(str::to_owned).collect();
            let deadline_secs=snap.policy().get("hold").and_then(|v|v.get("timeout_s")).and_then(toml::Value::as_integer).unwrap_or(0).max(0) as u64;
            foundry_exec::host::HoldState{hold_instance_id:new_hold_id(),reason:entry.node_id.clone(),resume_targets,deadline_secs}
        });
        let exit_code=if passed {0} else {1};
        let log_path=accepted_logs.last().cloned().unwrap_or_else(||format!("{}#{}", self.pack_path.display(),run_id));
        let host=HostResult{base_commit:base,result_commit:latest_commit.clone(),log_path,exit_code,generations:generations.clone(),held:hold_state.clone()};
        let held_json=hold_state.as_ref().zip(outcome.hold.as_ref()).map(|(hold,entry)|serde_json::json!({
            "hold":hold,"entry":entry,"engine_state":state.export(),"intent":intent,"run_id":run_id,
            "session_ids":session_ids,"accepted_logs":accepted_logs,"generations":generations,"latest_commit":latest_commit,"sequence":sequence
        }).to_string());

        let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        ensure_active(&tx,intent).map_err(HostError::Persistence)?;
        tx.execute("UPDATE review_runs SET status=?1, counters_json=?2, hold_json=?3, hold_instance_id=?4, resume_targets=?5, deadline=?6, result_json=?7, provenance_json=?8 WHERE order_id=?9 AND attempt=?10 AND run_id=?11",
            params![if passed{"passed"} else if outcome.hold.is_some(){"held"}else{"failed"},serde_json::to_string(&state.counters).map_err(sqlerr).map_err(HostError::Persistence)?,held_json.unwrap_or_else(||serde_json::json!({"node_id":outcome.hold.as_ref().map(|h|h.node_id.clone()),"entry_counters":outcome.hold.as_ref().map(|h|h.entry_counters.clone()),"requested_revert":outcome.hold.as_ref().and_then(|h|h.reverted_base.clone()),"artifact_mutation_applied":false}).to_string()),host.held.as_ref().map(|h|h.hold_instance_id.clone()),host.held.as_ref().map(|h|serde_json::to_string(&h.resume_targets).unwrap()),host.held.as_ref().map(|h|if h.deadline_secs==0 {0} else {chrono::Utc::now().timestamp().saturating_add(h.deadline_secs as i64)}),serde_json::to_string(&host).map_err(sqlerr).map_err(HostError::Persistence)?,serde_json::to_string(&binary).map_err(sqlerr).map_err(HostError::Persistence)?,order,attempt as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",params![key.order,key.attempt as i64,key.run as i64,key.activation as i64,serde_json::to_string(&host).map_err(|e|HostError::Persistence(e.to_string()))?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        Ok(host)
    }
    fn dispatch_count(&self)->u64 { self.dispatches }
    fn accepted(&self,intent:&ExecutionIntent)->bool { self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],|r|r.get::<_,bool>(0)).unwrap_or_else(|e|panic!("host acceptance read failed: {e}")) }
    fn recover_result(&mut self,intent:&ExecutionIntent)->Option<HostResult>{ self.check_binding(intent).unwrap_or_else(|e|panic!("host recovery binding: {e:?}")); self.load_result(&host_key(intent)).unwrap_or_else(|e|panic!("host result recovery failed: {e:?}")) }
    fn latest_run(&self, order:&str, attempt:u64)->Result<Option<foundry_exec::RunRecord>,HostError>{
        let mut stmt=self.conn.prepare("SELECT run_id,base_commit,result_commit,status,hold_json,hold_instance_id FROM review_runs WHERE order_id=?1 AND attempt=?2 ORDER BY rowid DESC").map_err(sqlerr).map_err(HostError::Persistence)?;
        let rows=stmt.query_map(params![order,attempt as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?))).map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut best:Option<foundry_exec::RunRecord>=None;
        for row in rows {
            let (run_id,base_commit,result_commit,status,hold_json,hold_instance_id)=row.map_err(sqlerr).map_err(HostError::Persistence)?;
            let parts:Vec<_>=run_id.split(':').collect();
            let run=parts.get(parts.len().saturating_sub(2)).and_then(|s|s.parse::<u64>().ok()).ok_or_else(||HostError::Persistence("invalid durable review run id".into()))?;
            if best.as_ref().is_none_or(|old|run>old.run) { best=Some(foundry_exec::RunRecord{run,base_commit,result_commit,status,hold_json,hold_instance_id}); }
        }
        Ok(best)
    }
    fn resolve_hold(&mut self,order:&str,hold_id:&str,action:foundry_exec::ReviewHoldAction,actor:&str)->Result<(),HostError>{
        let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        let row:Option<(i64,String,String,String,Option<String>)>=tx.query_row("SELECT attempt,run_id,status,hold_json,hold_instance_id FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",[order],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sqlerr).map_err(HostError::Persistence)?;
        let Some((attempt,run_id,status,mut hold_json,stored_id))=row else{return Err(HostError::Worker("review hold not found".into()));};
        if status!="held" || stored_id.as_deref()!=Some(hold_id){return Err(HostError::Worker("review hold is stale or already consumed".into()));}
        let mut doc:serde_json::Value=serde_json::from_str(&hold_json).map_err(sqlerr).map_err(HostError::Persistence)?;
        let hold:foundry_exec::host::HoldState=serde_json::from_value(doc["hold"].clone()).map_err(sqlerr).map_err(HostError::Persistence)?;
        let deadline:i64=tx.query_row("SELECT deadline FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run_id],|r|r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut history=doc.get("human_events").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        if deadline>0 && chrono::Utc::now().timestamp()>=deadline {
            history.push(serde_json::json!({"actor":actor,"at":chrono::Utc::now().to_rfc3339(),"action":"human_deadline_expired"}));
            doc["human_events"]=serde_json::Value::Array(history);hold_json=doc.to_string();
            tx.execute("UPDATE review_runs SET hold_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4",params![hold_json,order,attempt,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
            tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
            return Err(HostError::Worker("human review hold deadline expired".into()));
        }
        if let foundry_exec::ReviewHoldAction::Resume{target}=&action {
            if !hold.resume_targets.iter().any(|candidate|candidate==target){return Err(HostError::Worker("resume target is not allowed".into()));}
            self.executor.ensure_available().map_err(HostError::Worker)?;
            let resume_sequence=doc["resume_sequence"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||HostError::Persistence("resume sequence exhausted".into()))?;
            doc["resume_sequence"]=serde_json::json!(resume_sequence);
            history.push(serde_json::json!({"actor":actor,"at":chrono::Utc::now().to_rfc3339(),"action":"resume","target":target}));
            doc["human_events"]=serde_json::Value::Array(history);
            tx.execute("UPDATE review_runs SET status='resuming',hold_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='held' AND hold_instance_id=?5",params![doc.to_string(),order,attempt,run_id,hold_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
            tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
            if let Err(error)=self.continue_held_engine(order,attempt,&run_id,target,doc) {
                let _=self.conn.execute("UPDATE review_runs SET status='held' WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND status='resuming'",params![order,attempt,run_id]);
                return Err(error);
            }
            return Ok(());
        }
        let what=match &action {
            foundry_exec::ReviewHoldAction::Resume{..}=>unreachable!(),
            foundry_exec::ReviewHoldAction::CommitPass=>"commit_pass",
            foundry_exec::ReviewHoldAction::CommitFail=>"commit_fail",
            foundry_exec::ReviewHoldAction::Abort=>"abort",
        };
        history.push(serde_json::json!({"actor":actor,"at":chrono::Utc::now().to_rfc3339(),"action":what}));
        doc["human_events"]=serde_json::Value::Array(history);
        let intent:ExecutionIntent=serde_json::from_value(doc["intent"].clone()).map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut result:HostResult=serde_json::from_str(&tx.query_row::<String,_,_>("SELECT result_json FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run_id],|r|r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?).map_err(sqlerr).map_err(HostError::Persistence)?;
        result.held=None;
        result.exit_code=if matches!(action,foundry_exec::ReviewHoldAction::CommitPass){0}else{1};
        let resolved_status=if matches!(action,foundry_exec::ReviewHoldAction::Abort){"review_aborted"}else{"resolved"};
        let result_json=serde_json::to_string(&result).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("UPDATE review_runs SET status=?1,hold_json=?2,result_json=?3 WHERE order_id=?4 AND attempt=?5 AND run_id=?6",params![resolved_status,doc.to_string(),result_json,order,attempt,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("UPDATE host_results SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,order,attempt,intent.run.0 as i64,intent.activation as i64]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;Ok(())
    }
}
impl PackReviewHost {
    fn continue_held_engine(&mut self, order:&str, attempt:i64, run_id:&str, target:&str, mut doc:serde_json::Value)->Result<(),HostError>{
        let intent:ExecutionIntent=serde_json::from_value(doc["intent"].clone()).map_err(sqlerr).map_err(HostError::Persistence)?;
        let base_commit=parse_external_ref(&intent.external_ref).map(|parts|parts.2).ok_or_else(||HostError::Persistence("held intent has invalid external_ref".into()))?;
        let mut state=foundry_pack::EngineState::import(doc["engine_state"].as_str().ok_or_else(||HostError::Persistence("held run has no engine snapshot".into()))?).map_err(HostError::Persistence)?;
        let run_root=self.db_path.with_extension("review-runs").join(format!("{:x}",Sha256::digest(run_id.as_bytes())));
        let snapshot_run=doc["snapshot_run_id"].as_str().unwrap_or(run_id);
        let snapshot_root=self.db_path.with_extension("review-runs").join(format!("{:x}",Sha256::digest(snapshot_run.as_bytes())));
        let snap=load_pack(snapshot_root.join("pack-snapshot")).map_err(|e|HostError::Persistence(e.to_string()))?;
        let expected_digest:String=self.conn.query_row("SELECT pack_digest FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run_id],|r|r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?;
        if snap.digest()!=expected_digest || snap.digest()!=self.pack_digest { return Err(HostError::Persistence("held pack snapshot digest changed".into())); }
        let engine=GraphEngine::from_snapshot(&snap).map_err(|e|HostError::Persistence(e.to_string()))?;
        let mut generations:Vec<foundry_exec::GenerationCommit>=serde_json::from_value(doc["generations"].clone()).map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut latest_commit=doc["latest_commit"].as_str().map(str::to_owned).unwrap_or_else(||generations.last().map(|g|g.commit.clone()).unwrap_or_default());
        let mut accepted_logs:Vec<String>=serde_json::from_value(doc["accepted_logs"].clone()).unwrap_or_default();
        let mut sequence:u64=self.conn.query_row("SELECT COALESCE(MAX(sequence),0) FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2",params![order,run_id],|r|r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?;
        let mut session_ids=BTreeSet::new();let mut fresh_sessions=BTreeMap::new();
        let resume_sequence=doc["resume_sequence"].as_u64().ok_or_else(||HostError::Persistence("resume sequence missing".into()))?;
        let execution_root=run_root.join("exec").join(format!("resume-{resume_sequence}"));
        let outcome=engine.resume_staged(&mut state,target,|node_id,redrive,input,counters|{
            self.conn.execute("UPDATE review_runs SET counters_json=?1 WHERE order_id=?2 AND run_id=?3",params![serde_json::to_string(counters).map_err(packerr)?,order,run_id]).map_err(packerr)?;
            if redrive {
                let node=snap.nodes().iter().find(|n|n.id==node_id).ok_or_else(||packerr(format!("unknown redrive node {node_id}")))?;
                let role=if node_id=="fixdie" {"fix"} else {
                    let pre=node.value.get("pre_stage").and_then(toml::Value::as_str).ok_or_else(||packerr("redrive pre_stage missing"))?;
                    snap.policy().get("pack_stages").and_then(toml::Value::as_array).and_then(|a|a.iter().find(|s|s.get("id").and_then(toml::Value::as_str)==Some(pre))).and_then(|s|s.get("role")).and_then(toml::Value::as_str).ok_or_else(||packerr("redrive pre_stage role missing"))?
                }.to_owned();
                let generation=generations.len() as u64;
                let payload=node.value.get("payload").and_then(toml::Value::as_str).unwrap_or("findings");
                let mut inputs=BTreeMap::new();
                let values=redrive_payload(&self.conn,order,run_id,payload)?;
                inputs.insert(payload.to_owned(),serde_json::to_string(&values).map_err(packerr)?);
                sequence+=1;
                let workdir=execution_root.join(format!("{}-{node_id}-g{generation}",intent.activation)).join("0");
                fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;fs::create_dir(&workdir).map_err(packerr)?;
                let session_key=redrive_session_key(order,node_id);
                let saved:Option<String>=self.conn.query_row("SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_key],|r|r.get(0)).optional().map_err(sqlerr).map_err(packerr)?;
                let saved=saved.map(|s|serde_json::from_str::<SessionReceipt>(&s).map_err(packerr)).transpose()?;
                let task_session=if let Some(saved)=saved.as_ref(){if session_output_valid(saved) && self.executor.restore_session(&saved.session_id,&workdir).is_ok(){saved.session_id.clone()}else{self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')",[order]).map_err(packerr)?;format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}")}}else{format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}")};
                let task=StageTask{stage_id:format!("{node_id}-g{generation}"),role:role.clone(),lens_bytes:Vec::new(),session_id:task_session,fix_context:Some(latest_commit.clone()),base_commit:latest_commit.clone(),artifact_commit:latest_commit.clone(),inputs,workdir};
                charge_review_budget(&mut self.conn,order,&intent)?;
                let out=self.executor.execute(&task).and_then(|o|accept_output(&task,o)).map_err(|e|foundry_pack::PackError{code:"E_EXEC_REDRIVE_ERROR",message:e})?;
                let Some(commit)=out.produced_commit.clone().filter(|c|!c.is_empty()) else{return Err(foundry_pack::PackError{code:"E_EXEC_REDRIVE_ERROR",message:"redrive stage produced no commit".into()});};
                self.conn.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'complete',?5)",params![order,run_id,sequence as i64,task.stage_id,serde_json::json!({"role":role,"log_path":out.log_path,"produced_commit":commit}).to_string()]).map_err(packerr)?;
                self.conn.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![order,run_id,session_key,serde_json::to_string(&SessionReceipt{session_id:task.session_id,output:out.log_path.clone(),sha256:format!("{:x}",Sha256::digest(fs::read(&out.log_path).map_err(packerr)?))}).map_err(packerr)?]).map_err(packerr)?;
                accepted_logs.push(out.log_path.clone());generations.push(foundry_exec::GenerationCommit{generation,commit:commit.clone(),fix_log_path:out.log_path});latest_commit=commit;
                return Ok(input.clone());
            }
            let node=snap.nodes().iter().find(|n|n.id==node_id).ok_or_else(||packerr(format!("unknown dispatch node {node_id}")))?;
            let stages=node.value.get("stages").and_then(toml::Value::as_array).cloned().ok_or_else(||packerr("dispatch has no stages"))?;
            let mut done=BTreeMap::new();let mut verdicts=Vec::new();let mut findings=false;let mut valid=0usize;let mut verify_failed=false;
            for stage in stages {
                let id=stage.get("id").and_then(toml::Value::as_str).ok_or_else(||packerr("stage missing id"))?.to_owned();
                let role=stage.get("role").and_then(toml::Value::as_str).ok_or_else(||packerr("stage missing role"))?.to_owned();
                let inputs=match sealed_inputs(&stage,&done){Ok(v)=>v,Err(_)=>{verify_failed|=role=="verify";continue;}};
                let lens_names=stage.get("lenses").and_then(toml::Value::as_array).ok_or_else(||packerr("stage missing lenses"))?;
                let mut lens_bytes=Vec::new();for name in lens_names{let name=name.as_str().ok_or_else(||packerr("lens path not string"))?;lens_bytes.extend_from_slice(snap.files().get(name).ok_or_else(||packerr("lens absent from snapshot"))?);lens_bytes.push(b'\n');}
                sequence+=1;let workdir=execution_root.join(format!("{}-{}",intent.activation,sequence)).join("0");fs::create_dir_all(workdir.parent().unwrap()).map_err(packerr)?;fs::create_dir(&workdir).map_err(packerr)?;
                let session=stage.get("session").and_then(toml::Value::as_str).unwrap_or(&id);
                let session_key=format!("{order}:{session}");
                let saved:Option<String>=self.conn.query_row("SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_key],|r|r.get(0)).optional().map_err(packerr)?;
                let saved=saved.map(|s|serde_json::from_str::<SessionReceipt>(&s).map_err(packerr)).transpose()?;
                let mut task_session=fresh_sessions.get(&session_key).cloned().unwrap_or_else(||saved.as_ref().map(|s|s.session_id.clone()).unwrap_or_else(||session_key.clone()));
                if session_ids.insert(session_key.clone()) && let Some(saved)=saved.as_ref()
                    && (!session_output_valid(saved) || self.executor.restore_session(&saved.session_id,&workdir).is_err()) {
                    self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')",[order]).map_err(packerr)?;
                    task_session=format!("{session_key}:fresh:{run_id}:resume-{resume_sequence}");fresh_sessions.insert(session_key.clone(),task_session.clone());
                }
                let task=StageTask{stage_id:id.clone(),role:role.clone(),lens_bytes,session_id:task_session,fix_context:None,base_commit:base_commit.clone(),artifact_commit:latest_commit.clone(),inputs,workdir};
                charge_review_budget(&mut self.conn,order,&intent)?;
                self.conn.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",params![order,run_id,sequence,id]).map_err(packerr)?;
                let output=match self.executor.execute(&task).and_then(|o|accept_output(&task,o)){Ok(o)=>o,Err(error)=>{
                    self.conn.execute("UPDATE review_stage_attempts SET status='error',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4",params![error,order,run_id,sequence]).map_err(packerr)?;
                    verify_failed|=role=="verify";continue;
                }};
                let categories:Vec<String>=output.verdict_category.iter().chain(&output.verdict_categories).cloned().collect();
                let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(packerr)?;ensure_active(&tx,&intent).map_err(packerr)?;
                tx.execute("UPDATE review_stage_attempts SET status='complete',output_json=?1 WHERE order_id=?2 AND run_id=?3 AND sequence=?4",params![serde_json::json!({"role":role,"log_path":output.log_path,"sha256":format!("{:x}",Sha256::digest(fs::read(&output.log_path).map_err(packerr)?)),"inspected_commit":output.inspected_commit,"findings":output.findings,"verdicts":categories}).to_string(),order,run_id,sequence]).map_err(packerr)?;
                tx.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![order,run_id,session_key,serde_json::to_string(&SessionReceipt{session_id:task.session_id,output:output.log_path.clone(),sha256:format!("{:x}",Sha256::digest(fs::read(&output.log_path).map_err(packerr)?))}).map_err(packerr)?]).map_err(packerr)?;tx.commit().map_err(packerr)?;
                valid+=1;done.insert(id,output.log_path.clone());accepted_logs.push(output.log_path);
                if role=="verify"{findings|=output.findings || categories.iter().any(|v|matches!(v.as_str(),"patch"|"intent_gap"|"bad_plan"));verdicts.extend(categories);}
            }
            if valid==0{return Err(foundry_pack::PackError{code:"E_EXEC_ALL_ERROR",message:"no valid stage output".into()});}
            if verify_failed{return Err(foundry_pack::PackError{code:"E_EXEC_VERIFY_INVALID",message:"verify output invalid".into()});}
            Ok(RunInput{verdicts,has_findings:findings,budget_exhausted:input.budget_exhausted,timeout:input.timeout})
        }).map_err(|e|HostError::Worker(e.to_string()))?;
        let passed=outcome.terminal&&outcome.actions.iter().any(|a|matches!(a,EngineAction::Emit{event,..}if event=="InspectionPassed"));
        let hold=outcome.hold.as_ref().map(|entry|{let targets=snap.nodes().iter().find(|n|n.id==entry.node_id).and_then(|n|n.value.get("resume_targets")).and_then(toml::Value::as_array).into_iter().flatten().filter_map(toml::Value::as_str).map(str::to_owned).collect();foundry_exec::host::HoldState{hold_instance_id:new_hold_id(),reason:entry.node_id.clone(),resume_targets:targets,deadline_secs:snap.policy().get("hold").and_then(|v|v.get("timeout_s")).and_then(toml::Value::as_integer).unwrap_or(0).max(0) as u64}});
        let mut result:HostResult=serde_json::from_str(&self.conn.query_row::<String,_,_>("SELECT result_json FROM review_runs WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run_id],|r|r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?).map_err(sqlerr).map_err(HostError::Persistence)?;
        result.result_commit=latest_commit;result.generations=generations;result.log_path=accepted_logs.last().cloned().unwrap_or(result.log_path);result.exit_code=if passed{0}else{1};result.held=hold.clone();
        doc["engine_state"]=serde_json::Value::String(state.export());doc["generations"]=serde_json::to_value(&result.generations).map_err(sqlerr).map_err(HostError::Persistence)?;doc["latest_commit"]=serde_json::Value::String(result.result_commit.clone());doc["accepted_logs"]=serde_json::to_value(accepted_logs).map_err(sqlerr).map_err(HostError::Persistence)?;doc["sequence"]=serde_json::json!(sequence);
        if let (Some(hold),Some(entry))=(hold.as_ref(),outcome.hold.as_ref()){doc["hold"]=serde_json::to_value(hold).map_err(sqlerr).map_err(HostError::Persistence)?;doc["entry"]=serde_json::to_value(entry).map_err(sqlerr).map_err(HostError::Persistence)?;}
        // The consumed hold is resolved; the runner transfers a newly raised hold
        // to its next run without publishing a tuple.
        let status="resolved";let result_json=serde_json::to_string(&result).map_err(sqlerr).map_err(HostError::Persistence)?;
        let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        ensure_active(&tx,&intent).map_err(HostError::Persistence)?;
        let resume_targets=hold.as_ref().map(|h|serde_json::to_string(&h.resume_targets).unwrap_or_else(|_|"[]".into()));
        let deadline=hold.as_ref().map(|h|if h.deadline_secs==0{0}else{chrono::Utc::now().timestamp().saturating_add(h.deadline_secs as i64)});
        tx.execute("UPDATE review_runs SET status=?1,counters_json=?2,hold_json=?3,hold_instance_id=?4,resume_targets=?5,deadline=?6,result_json=?7 WHERE order_id=?8 AND attempt=?9 AND run_id=?10",params![status,serde_json::to_string(&state.counters).map_err(sqlerr).map_err(HostError::Persistence)?,doc.to_string(),hold.as_ref().map(|h|h.hold_instance_id.clone()),resume_targets,deadline,result_json,order,attempt,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("UPDATE host_results SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,order,attempt,intent.run.0 as i64,intent.activation as i64]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;Ok(())
    }

    fn check_binding(&self,intent:&ExecutionIntent)->Result<(),HostError>{
        let saved:Option<String>=self.conn.query_row("SELECT intent_json FROM review_bindings WHERE key_json=?1",[key_json(intent)],|r|r.get(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?;
        if let Some(saved)=saved {
            let old:ExecutionIntent=serde_json::from_str(&saved).map_err(sqlerr).map_err(HostError::Persistence)?;
            if old.external_ref!=intent.external_ref || old.epoch!=intent.epoch || old.generation!=intent.generation {
                return Err(HostError::Worker("activation binding mismatch".into()));
            }
        } else if self.accepted(intent) {
            return Err(HostError::Persistence("accepted activation lacks binding evidence".into()));
        }
        Ok(())
    }
    fn load_result(&self,k:&HostKey)->Result<Option<HostResult>,HostError>{ self.conn.query_row("SELECT result_json FROM host_results WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![k.order,k.attempt as i64,k.run as i64,k.activation as i64],|r|r.get::<_,String>(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?.map(|s|serde_json::from_str(&s).map_err(|e|HostError::Persistence(e.to_string()))).transpose() }
}

#[derive(serde::Serialize,serde::Deserialize)]
struct SessionReceipt { session_id:String, output:String, sha256:String }


fn redrive_session_key(order:&str,node:&str)->String {
    format!("{order}:{}",if node=="fixdie" {"die"} else {node})
}
fn session_output_valid(saved:&SessionReceipt)->bool {
    fs::read(&saved.output).is_ok_and(|bytes|format!("{:x}",Sha256::digest(bytes))==saved.sha256)
}
// Select only the latest completed occurrence of each requested artifact.
fn redrive_payload(conn:&Connection,order:&str,run:&str,payload:&str)->foundry_pack::Result<Vec<serde_json::Value>> {
    let mut query=conn.prepare("SELECT stage_id,status,output_json FROM review_stage_attempts WHERE order_id=?1 AND run_id=?2 ORDER BY sequence DESC").map_err(packerr)?;
    let rows=query.query_map(params![order,run],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?))).map_err(packerr)?;
    let mut seen=BTreeSet::new();let mut values=Vec::new();
    for row in rows {
        let (stage,status,json)=row.map_err(packerr)?;
        if !seen.insert(stage.clone()) || status!="complete" { continue; }
        let value:serde_json::Value=serde_json::from_str(&json.ok_or_else(||packerr("complete stage lacks output"))?).map_err(packerr)?;
        let matches=if payload=="findings" { value["role"]=="verify" && value["findings"]==true } else {stage==payload};
        if matches {values.push(serde_json::json!({"stage_id":stage,"verdicts":value["verdicts"],"log_path":value["log_path"]}));}
    }
    values.reverse();Ok(values)
}

fn sealed_inputs(stage:&toml::Value,done:&BTreeMap<String,String>)->foundry_pack::Result<BTreeMap<String,String>>{
    let deps=match stage.get("after") {
        None=>vec![],
        Some(toml::Value::String(s))=>vec![s.clone()],
        Some(toml::Value::Array(values))=>values.iter().map(|v|v.as_str().map(str::to_owned).ok_or_else(||packerr("dependency must be string"))).collect::<foundry_pack::Result<Vec<_>>>()?,
        _=>return Err(packerr("invalid stage dependencies")),
    };
    deps.into_iter().map(|id|{
        let path=done.get(&id).ok_or_else(||packerr(format!("stage seal violation: predecessor {id} is incomplete")))?;
        Ok((id,path.clone()))
    }).collect()
}

fn accept_output(task:&StageTask,mut out:StageOutput)->Result<StageOutput,String>{
    let path=fs::canonicalize(&out.log_path).map_err(sqlerr)?;
    let namespace=fs::canonicalize(&task.workdir).map_err(sqlerr)?;
    if !path.starts_with(namespace) {return Err("stage output escaped its execution namespace".into());}
    let bytes=fs::read(&path).map_err(sqlerr)?;
    if bytes.is_empty() {return Err("stage output is empty".into());}
    if task.role=="verify" {
        if out.produced_commit.is_some(){return Err("verify stage returned a produced commit".into());}
        if out.inspected_commit.as_deref()!=Some(task.artifact_commit.as_str()) {return Err("verify output is not bound to the requested immutable commit".into());}
        if out.verdict_category.iter().chain(&out.verdict_categories).any(|v|!matches!(v.as_str(),"patch"|"intent_gap"|"bad_plan"|"defer"|"pass")) {return Err("unknown verify category".into());}
    }
    if task.role=="lens" && out.produced_commit.is_some(){return Err("lens stage returned a produced commit".into());}
    if task.role!="verify" && out.produced_commit.as_ref().is_some_and(|commit|commit.trim().is_empty()) {return Err("implementation stage returned an empty produced commit".into());}
    let accepted=task.workdir.join("accepted-output");
    use std::io::Write;
    let mut file=fs::OpenOptions::new().write(true).create_new(true).open(&accepted).map_err(sqlerr)?;
    file.write_all(&bytes).map_err(sqlerr)?;file.sync_all().map_err(sqlerr)?;
    fs::File::open(&task.workdir).and_then(|dir|dir.sync_all()).map_err(sqlerr)?;
    out.log_path=accepted.to_string_lossy().into_owned();
    Ok(out)
}

fn persist_snapshot(root:&Path,snap:&foundry_pack::PackSnapshot)->Result<(),String>{
    use std::io::Write;
    let destination=root.join("pack-snapshot");
    fs::create_dir_all(&destination).map_err(sqlerr)?;
    for (name,bytes) in snap.files() {
        let relative=Path::new(name);
        if relative.is_absolute() || relative.components().any(|c|!matches!(c,std::path::Component::Normal(_))) {
            return Err("invalid snapshot path".into());
        }
        let path=destination.join(relative);
        fs::create_dir_all(path.parent().unwrap()).map_err(sqlerr)?;
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(sqlerr)?;
        file.write_all(bytes).map_err(sqlerr)?;file.sync_all().map_err(sqlerr)?;
    }
    // The loader keeps manifest bytes outside PackSnapshot::files(). Re-read
    // that one file, then verify the copied closure before executing anything.
    let manifest=fs::read(snap.root().join("manifest.toml")).map_err(sqlerr)?;
    let mut file=fs::OpenOptions::new().write(true).create_new(true).open(destination.join("manifest.toml")).map_err(sqlerr)?;
    file.write_all(&manifest).map_err(sqlerr)?;file.sync_all().map_err(sqlerr)?;
    let copied=load_pack(&destination).map_err(sqlerr)?;
    if copied.digest()!=snap.digest() {return Err("copied pack snapshot digest changed".into());}
    // Sync new directory entries before an external executor can observe them.
    for dir in [&destination,root,root.parent().unwrap()] {
        fs::File::open(dir).and_then(|dir|dir.sync_all()).map_err(sqlerr)?;
    }
    Ok(())
}

fn validate_intent(intent:&ExecutionIntent)->Result<(),String>{
    if intent.generation.0!=0 {return Err("T31 review adapter only accepts generation 0".into());}
    if [intent.attempt.0,intent.run.0,intent.activation,intent.epoch.0].iter().any(|v|*v>i64::MAX as u64) {return Err("review identity exceeds SQLite integer range".into());}
    Ok(())
}
fn key_json(intent:&ExecutionIntent)->String{serde_json::to_string(&host_key(intent)).expect("host key serialization")}
fn advance_epoch(tx:&rusqlite::Transaction<'_>,order:&str,epoch:Epoch)->Result<(),String>{
    let old:Option<i64>=tx.query_row("SELECT epoch FROM review_epochs WHERE order_id=?1",[order],|r|r.get(0)).optional().map_err(sqlerr)?;
    if old.is_some_and(|old|old as u64>epoch.0) {return Err("stale review epoch".into());}
    tx.execute("INSERT INTO review_epochs(order_id,epoch) VALUES(?1,?2) ON CONFLICT(order_id) DO UPDATE SET epoch=excluded.epoch",params![order,epoch.0 as i64]).map_err(sqlerr)?;
    Ok(())
}
fn ensure_active(tx:&rusqlite::Transaction<'_>,intent:&ExecutionIntent)->Result<(),String>{
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM review_active a JOIN review_epochs e ON a.order_id=e.order_id WHERE a.order_id=?1 AND a.key_json=?2 AND a.epoch=?3 AND e.epoch=?3)",params![intent.order.0,key_json(intent),intent.epoch.0 as i64],|r|r.get(0)).map_err(sqlerr)?;
    if valid {Ok(())} else {Err("inactive run or stale epoch".into())}
}

#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)] pub struct BinaryProvenance { pub executable:String,pub sha256:String,pub source_commit:String,pub pack_digest:String }
fn validate_source_commit(s:&str)->Result<(),String>{
    if !matches!(s.len(),40|64) || !s.bytes().all(|b|b.is_ascii_hexdigit()) { return Err("FOUNDRY_SOURCE_COMMIT must be an explicit full source commit hash".into()); }
    Ok(())
}
fn provenance(pack:&str,source:Option<&str>)->Result<BinaryProvenance,String>{
    let source=source.ok_or("FOUNDRY_SOURCE_COMMIT must identify the executable build; launch-directory git HEAD is not provenance")?;
    validate_source_commit(source)?;
    let exe=std::env::current_exe().map_err(sqlerr)?;
    let bytes=fs::read(&exe).map_err(sqlerr)?;
    Ok(BinaryProvenance{executable:exe.display().to_string(),sha256:format!("{:x}",Sha256::digest(bytes)),source_commit:source.into(),pack_digest:pack.into()})
}

type Scope=Arc<Mutex<Option<ExecutionIntent>>>;
pub struct SqliteReviewSink{ conn:Mutex<Connection>,scope:Scope } pub struct SqliteReviewLog{conn:Mutex<Connection>,scope:Scope} pub struct SqliteIntentLog{conn:Mutex<Connection>,scope:Scope}
macro_rules! open_port { ($t:ty) => { impl $t { pub fn open(p:impl AsRef<Path>)->Result<Self,String>{Ok(Self{conn:Mutex::new(open_db(p.as_ref())?),scope:Arc::new(Mutex::new(None))})} } }; }
open_port!(SqliteReviewSink); open_port!(SqliteReviewLog); open_port!(SqliteIntentLog);
impl EmitSink for SqliteReviewSink {
    fn publish(&mut self,t:&ArtifactTuple)->EmitReceipt{self.publish_named(t,"InspectionPassed","summary")}
    fn publish_named(&mut self,t:&ArtifactTuple,event:&str,_payload:&str)->EmitReceipt{
        let mut c=self.conn.lock().unwrap();let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        let key=serde_json::to_string(&t.key()).unwrap();
        let old:Option<String>=tx.query_row("SELECT receipt_json FROM review_emit WHERE key_json=?1",[&key],|r|r.get(0)).optional().unwrap();
        if let Some(old)=old{return serde_json::from_str(&old).unwrap();}
        let intent=self.scope.lock().unwrap().clone().expect("emit requires a bound review intent");
        ensure_active(&tx,&intent).unwrap_or_else(|e|panic!("stale review publication: {e}"));
        assert_eq!(t.order,intent.order,"emit order binding"); assert_eq!(t.attempt,intent.attempt,"emit attempt binding");
        assert_eq!(t.run,intent.run,"emit run binding");assert_eq!(t.generation,intent.generation,"emit generation binding");
        let published:Option<String>=tx.query_row("SELECT r.result_json FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE a.key_json=?1 AND r.status='passed'",[key_json(&intent)],|r|r.get(0)).optional().unwrap();
        let final_commit=published.and_then(|json|serde_json::from_str::<HostResult>(&json).ok()).map(|result|result.result_commit);
        assert!(final_commit.as_deref()==Some(t.immutable_commit.0.as_str()) && event=="InspectionPassed","publication requires a passed review bound to its final generation");
        let deferred:Option<(String,String,String)>=tx.query_row("SELECT s.output_json,r.counters_json,r.run_id FROM review_stage_attempts s JOIN review_runs r ON s.order_id=r.order_id AND s.run_id=r.run_id JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE a.key_json=?1 AND s.status='complete' AND json_extract(s.output_json,'$.role')='verify' ORDER BY s.sequence DESC LIMIT 1",[key_json(&intent)],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().unwrap();
        if let Some((output,counters,run))=deferred {
            let output:serde_json::Value=serde_json::from_str(&output).unwrap();
            if output["deferred"]==true {
                tx.execute("INSERT OR IGNORE INTO review_side_records(order_id,run_id,kind,value) VALUES(?1,?2,'deferred',?3)",params![intent.order.0,run,serde_json::json!({"source_run":run,"output":output["log_path"],"counters":serde_json::from_str::<serde_json::Value>(&counters).unwrap()}).to_string()]).unwrap();
            }
        }
        let rec=receipt(t,event);tx.execute("INSERT INTO review_emit(key_json,receipt_json) VALUES(?1,?2)",params![key,serde_json::to_string(&rec).unwrap()]).unwrap();
        tx.commit().unwrap();rec
    }
    fn receipt_of(&self,k:&ArtifactTupleKey)->Option<EmitReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT receipt_json FROM review_emit WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("emit receipt read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("emit receipt corrupt: {e}")))}
}
impl EmitLog for SqliteReviewLog {
    fn current_epoch(&self)->Epoch{
        let scope=self.scope.lock().unwrap();let Some(intent)=scope.as_ref() else{return Epoch(u64::MAX)};
        let value:i64=self.conn.lock().unwrap().query_row("SELECT epoch FROM review_epochs WHERE order_id=?1",[&intent.order.0],|r|r.get(0)).unwrap();
        Epoch(u64::try_from(value).expect("negative review epoch"))
    }
    fn promotion_of(&self,k:&ArtifactTupleKey)->Option<PromotionReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_promotions WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("promotion read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("promotion receipt corrupt: {e}")))}
    fn emit_of(&self,k:&ArtifactTupleKey)->Option<EmitReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_emit_log WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("emit log read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("emit receipt corrupt: {e}")))}
    fn record_emit(&mut self,r:&EmitReceipt){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_emit_log VALUES(?1,?2)",params![serde_json::to_string(&r.key).unwrap(),serde_json::to_string(r).unwrap()]).unwrap();}
    fn mark_complete(&mut self,k:&ArtifactTupleKey){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_emit_complete VALUES(?1)",[serde_json::to_string(k).unwrap()]).unwrap();}
    fn is_complete(&self,k:&ArtifactTupleKey)->bool{self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM review_emit_complete WHERE key_json=?1)",[serde_json::to_string(k).unwrap()],|r|r.get(0)).unwrap_or_else(|e|panic!("emit completion read failed: {e}"))}
}
impl IntentLog for SqliteIntentLog {
    fn append(&mut self,i:&ExecutionIntent){
        validate_intent(i).expect("invalid durable review intent");
        let mut c=self.conn.lock().unwrap();let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();
        tx.execute("INSERT INTO review_epochs(order_id,epoch) VALUES(?1,?2) ON CONFLICT(order_id) DO UPDATE SET epoch=MAX(epoch,excluded.epoch)",params![i.order.0,i.epoch.0 as i64]).unwrap();
        tx.execute("INSERT OR IGNORE INTO review_intents VALUES(?1,?2,?3,?4,?5)",params![i.order.0,i.attempt.0 as i64,i.run.0 as i64,i.activation as i64,serde_json::to_string(i).unwrap()]).unwrap();
        tx.commit().unwrap();*self.scope.lock().unwrap()=Some(i.clone());
    }
    fn mark_started(&mut self,k:&(OrderId,Attempt,RunId),a:u64){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_started VALUES(?1,?2,?3,?4)",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64]).unwrap();}
    fn latest(&self,k:&(OrderId,Attempt,RunId))->Option<ExecutionIntent>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 ORDER BY activation DESC LIMIT 1",params![k.0.0,k.1.0 as i64,k.2.0 as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("intent corrupt: {e}")))}
    fn get(&self,k:&(OrderId,Attempt,RunId),a:u64)->Option<ExecutionIntent>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("intent corrupt: {e}")))}
    fn is_started(&self,k:&(OrderId,Attempt,RunId),a:u64)->bool{self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM review_started WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get(0)).unwrap_or_else(|e|panic!("intent claim read failed: {e}"))}
}

fn open_db(p:&Path)->Result<Connection,String>{if let Some(p)=p.parent().filter(|p|!p.as_os_str().is_empty()){fs::create_dir_all(p).map_err(|e|e.to_string())?;}let c=Connection::open(p).map_err(sqlerr)?;c.pragma_update(None,"journal_mode","WAL").map_err(sqlerr)?;c.pragma_update(None,"synchronous","FULL").map_err(sqlerr)?;c.execute_batch("CREATE TABLE IF NOT EXISTS review_runs(order_id TEXT,attempt INTEGER,run_id TEXT,base_commit TEXT,result_commit TEXT,pack_digest TEXT,status TEXT,counters_json TEXT,hold_json TEXT,hold_instance_id TEXT,resume_targets TEXT,deadline INTEGER,result_json TEXT,provenance_json TEXT,created_at INTEGER,UNIQUE(order_id,attempt,run_id));CREATE TABLE IF NOT EXISTS host_accept(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS host_results(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,result_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_sessions(order_id TEXT,run_id TEXT,session_id TEXT,receipt_json TEXT,PRIMARY KEY(order_id,run_id,session_id));CREATE TABLE IF NOT EXISTS review_side_records(order_id TEXT,run_id TEXT,kind TEXT,value TEXT);CREATE TABLE IF NOT EXISTS review_emit(key_json TEXT PRIMARY KEY,receipt_json TEXT);CREATE TABLE IF NOT EXISTS review_promotions(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_log(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_complete(key_json TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS review_intents(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,value_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_started(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));
CREATE UNIQUE INDEX IF NOT EXISTS review_side_once ON review_side_records(order_id,run_id,kind);
CREATE TABLE IF NOT EXISTS review_epochs(order_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS review_active(order_id TEXT PRIMARY KEY,key_json TEXT NOT NULL,epoch INTEGER NOT NULL,journal_run TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_bindings(key_json TEXT PRIMARY KEY,intent_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_failures(order_id TEXT,run_id TEXT,error TEXT);
CREATE TABLE IF NOT EXISTS review_stage_attempts(order_id TEXT,run_id TEXT,sequence INTEGER,stage_id TEXT,status TEXT,output_json TEXT,UNIQUE(order_id,run_id,sequence));
CREATE TABLE IF NOT EXISTS review_budget(order_id TEXT PRIMARY KEY,used INTEGER NOT NULL);
").map_err(sqlerr)?;ensure_column(&c,"review_runs","hold_instance_id","TEXT")?;ensure_column(&c,"review_runs","resume_targets","TEXT")?;ensure_column(&c,"review_runs","deadline","INTEGER")?;Ok(c)}
fn ensure_column(c:&Connection,table:&str,column:&str,ty:&str)->Result<(),String>{let mut q=c.prepare(&format!("PRAGMA table_info({table})")).map_err(sqlerr)?;let cols=q.query_map([],|r|r.get::<_,String>(1)).map_err(sqlerr)?.collect::<Result<Vec<_>,_>>().map_err(sqlerr)?;if !cols.iter().any(|c|c==column){c.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"),[]).map_err(sqlerr)?;}Ok(())}
fn latest_counters(tx:&rusqlite::Transaction<'_>,order:&str)->Result<BTreeMap<String,u64>,HostError>{let v:Option<String>=tx.query_row("SELECT counters_json FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",[order],|r|r.get(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?;v.map(|s|serde_json::from_str(&s).map_err(|e|HostError::Persistence(e.to_string()))).transpose().map(|x|x.unwrap_or_default())}
fn parse_external_ref(s:&str)->Option<(String,u64,String,String)>{
    let (identity,commits)=s.strip_prefix("review:")?.rsplit_once(':')?;
    let (order,attempt)=identity.rsplit_once(':')?;let (base,result)=commits.split_once("..")?;
    if order.is_empty() || base.is_empty() || result.is_empty() || result.contains("..") {return None;}
    Some((order.into(),attempt.parse().ok()?,base.into(),result.into()))
}
fn host_key(i:&ExecutionIntent)->HostKey{HostKey{order:i.order.0.clone(),attempt:i.attempt.0,run:i.run.0,activation:i.activation}}
fn new_hold_id()->String{use std::io::Read;let mut bytes=[0u8;16];std::fs::File::open("/dev/urandom").and_then(|mut f|f.read_exact(&mut bytes)).expect("OS random source");bytes[6]=(bytes[6]&0x0f)|0x40;bytes[8]=(bytes[8]&0x3f)|0x80;format!("{:02x?}",bytes).replace(['[',']',',',' '],"")}
fn charge_review_budget(conn:&mut Connection,order:&str,intent:&ExecutionIntent)->foundry_pack::Result<()>{
    let tx=conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(packerr)?;
    ensure_active(&tx,intent).map_err(packerr)?;
    tx.execute("INSERT OR IGNORE INTO review_budget(order_id,used) VALUES(?1,0)",[order]).map_err(packerr)?;
    if tx.execute("UPDATE review_budget SET used=used+1 WHERE order_id=?1 AND used<10000",[order]).map_err(packerr)?==0 {
        return Err(foundry_pack::PackError{code:"E_EXEC_BUDGET",message:"order execution budget exhausted".into()});
    }
    tx.commit().map_err(packerr)
}
fn receipt(t:&ArtifactTuple,e:&str)->EmitReceipt{EmitReceipt{key:t.key(),event:e.into(),at:chrono::Utc::now()}}
fn sqlerr(e:impl std::fmt::Display)->String{e.to_string()} fn packerr(e:impl std::fmt::Display)->foundry_pack::PackError{foundry_pack::PackError{code:"E_HOST_STAGE",message:e.to_string()}}
fn expand_home(s:&str)->PathBuf{if let Some(x)=s.strip_prefix("~/")&&let Ok(h)=std::env::var("HOME"){return PathBuf::from(h).join(x)}PathBuf::from(s)}

// Helper factory used by CLI and serve; all S4 receipts and tuples survive restart.
pub fn durable_review_run(host:PackReviewHost, db:impl AsRef<Path>, tuples:std::sync::Arc<dyn TupleStore>)->Result<DurableReviewRun<SqliteReviewSink,SqliteReviewLog>,String>{if fs::canonicalize(db.as_ref()).map_err(sqlerr)? != host.db_path {return Err("host and runner must use the same database".into());}let(sink,log,intents)=PackReviewHost::sqlite_ports(db)?;Ok(DurableReviewRun::new(sink,log,Box::new(host),Box::new(intents),tuples))}

/// Product adapter for the durable review runner. Hold outcomes remain data;
/// completed rejection errors are returned to Line for its normal failure event.
pub struct InspectingReviewRun<R> { inner:R }
impl<R> InspectingReviewRun<R> {
    pub fn new(inner:R)->Self{Self{inner}}
}
impl<R:foundry_exec::ReviewRunPort> foundry_exec::ReviewRunPort for InspectingReviewRun<R> {
    fn run(&mut self,request:foundry_exec::ReviewRunRequest)->Result<foundry_exec::ReviewRunOutcome,foundry_exec::ReviewRunError>{
        self.inner.run(request)
    }
    fn latest_run(&self,order:&str,attempt:u64)->Result<Option<foundry_exec::RunRecord>,foundry_exec::ReviewRunError>{
        self.inner.latest_run(order,attempt)
    }
    fn resolve_hold(&mut self,order:&str,hold_id:&str,action:foundry_exec::ReviewHoldAction,actor:&str)->Result<(),foundry_exec::ReviewRunError>{
        self.inner.resolve_hold(order,hold_id,action,actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seal_refuses_missing_predecessor_before_opening_inputs(){
        let stage:toml::Value=toml::from_str("id='claims'\nafter=['edge']\n").unwrap();
        assert!(sealed_inputs(&stage,&BTreeMap::new()).unwrap_err().message.contains("seal violation"));
        let done=BTreeMap::from([("edge".into(),"edge-output".into())]);
        assert_eq!(sealed_inputs(&stage,&done).unwrap(),done);
    }
    #[test]
    fn provenance_requires_explicit_build_identity(){
        assert!(provenance(REVIEW_PACK_DIGEST,None).unwrap_err().contains("FOUNDRY_SOURCE_COMMIT"));
        assert!(provenance(REVIEW_PACK_DIGEST,Some("HEAD")).is_err());
    }
}

