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
pub struct StageOutput { pub verdict_category: Option<String>, pub findings: bool, pub log_path: String, pub inspected_commit: Option<String>, pub verdict_categories: Vec<String> }
/// The adapter must normalize output, stop all writes before returning, and
/// confine process, git-ref, and session writes to the supplied workdir.
pub trait StageExecutor: Send {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String>;
    fn restore_session(&mut self, _session_id: &str, _workdir: &Path) -> Result<(), String> {
        Err("session restoration is not implemented".into())
    }
}
/// Product wiring placeholder until the real LLM/Die adapter is supplied by T33.
#[derive(Default)] pub struct UnavailableStageExecutor;
impl StageExecutor for UnavailableStageExecutor { fn execute(&mut self, _task:&StageTask)->Result<StageOutput,String>{Err("review stage executor is not configured".into())} }

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
        let previous: Option<(String, i64)> = tx.query_row("SELECT r.status,a.epoch FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE r.order_id=?1", [&order], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(sqlerr).map_err(HostError::Persistence)?;
        if let Some((status, epoch)) = previous {
            if intent.epoch.0 < epoch as u64 || status == "held" || (status == "running" && intent.epoch.0 <= epoch as u64) {
                return Err(HostError::Acceptance("stale, held, or still-running order cannot start this activation".into()));
            }
            if status == "running" { tx.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status='running'", [&order]).map_err(sqlerr).map_err(HostError::Persistence)?; }
        }
        advance_epoch(&tx, &order, intent.epoch).map_err(HostError::Acceptance)?;
        let inherited = latest_counters(&tx, &order)?;
        let run_id = format!("{}:{}:{}:{}", order, attempt, intent.run.0, intent.activation);
        tx.execute("INSERT INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,unixepoch())",
            params![order, attempt as i64, run_id, base, result, digest, serde_json::to_string(&inherited).map_err(sqlerr).map_err(HostError::Persistence)?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_accept(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64]).map_err(sqlerr).map_err(HostError::Acceptance)?;
        tx.execute("INSERT INTO review_bindings(key_json,intent_json) VALUES(?1,?2)",params![key_json(intent),serde_json::to_string(intent).map_err(sqlerr).map_err(HostError::Persistence)?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT OR REPLACE INTO review_active(order_id,key_json,epoch,journal_run) VALUES(?1,?2,?3,?4)",params![order,key_json(intent),intent.epoch.0 as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        self.dispatches += 1;
        let mut state = engine.initial_state(inherited);
        let run_root=self.db_path.with_extension("review-runs").join(format!("{:x}",Sha256::digest(run_id.as_bytes())));
        let mut snapshot_written=false;
        let mut sequence = 0_u64;
        let mut session_ids = BTreeSet::new();
        let mut fresh_sessions = BTreeMap::new();
        let mut accepted_logs = Vec::new();

        let output = RunInput { verdicts: vec![], has_findings: false, budget_exhausted: false, timeout: false };
        let outcome = engine.run_staged_checkpointed(&mut state, &output, 10_000, |node_id, redrive, input, counters| {
            self.conn.execute("UPDATE review_runs SET counters_json=?1 WHERE order_id=?2 AND run_id=?3", params![serde_json::to_string(counters).map_err(packerr)?,order,run_id]).map_err(packerr)?;
            // T31 binds the original commit and generation 0. A no-op redrive
            // must never authorize that artifact as a newly implemented result.
            if redrive { return Err(foundry_pack::PackError{code:"E_EXEC_REDRIVE_ERROR",message:"redrive artifact handoff is not configured".into()}); }
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
                    fix_context:None, base_commit:base.clone(), artifact_commit:result.clone(), inputs, workdir };
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
        let exit_code=if passed {0} else {1};
        let log_path=accepted_logs.last().cloned().unwrap_or_else(||format!("{}#{}", self.pack_path.display(),run_id));
        let host=HostResult{base_commit:base,result_commit:result,log_path,exit_code};

        let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        ensure_active(&tx,intent).map_err(HostError::Persistence)?;
        tx.execute("UPDATE review_runs SET status=?1, counters_json=?2, hold_json=?3, result_json=?4, provenance_json=?5 WHERE order_id=?6 AND attempt=?7 AND run_id=?8",
            params![if passed{"passed"} else if outcome.hold.is_some(){"held"}else{"failed"},serde_json::to_string(&state.counters).map_err(sqlerr).map_err(HostError::Persistence)?,serde_json::json!({"node_id":outcome.hold.as_ref().map(|h|h.node_id.clone()),"entry_counters":outcome.hold.as_ref().map(|h|h.entry_counters.clone()),"requested_revert":outcome.hold.as_ref().and_then(|h|h.reverted_base.clone()),"artifact_mutation_applied":false}).to_string(),serde_json::to_string(&host).map_err(sqlerr).map_err(HostError::Persistence)?,serde_json::to_string(&binary).map_err(sqlerr).map_err(HostError::Persistence)?,order,attempt as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",params![key.order,key.attempt as i64,key.run as i64,key.activation as i64,serde_json::to_string(&host).map_err(|e|HostError::Persistence(e.to_string()))?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        Ok(host)
    }
    fn dispatch_count(&self)->u64 { self.dispatches }
    fn accepted(&self,intent:&ExecutionIntent)->bool { self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],|r|r.get::<_,bool>(0)).unwrap_or_else(|e|panic!("host acceptance read failed: {e}")) }
    fn recover_result(&mut self,intent:&ExecutionIntent)->Option<HostResult>{ self.check_binding(intent).unwrap_or_else(|e|panic!("host recovery binding: {e:?}")); self.load_result(&host_key(intent)).unwrap_or_else(|e|panic!("host result recovery failed: {e:?}")) }
}
impl PackReviewHost {
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
        if out.inspected_commit.as_deref()!=Some(task.artifact_commit.as_str()) {return Err("verify output is not bound to the requested immutable commit".into());}
        if out.verdict_category.iter().chain(&out.verdict_categories).any(|v|!matches!(v.as_str(),"patch"|"intent_gap"|"bad_plan"|"defer"|"pass")) {return Err("unknown verify category".into());}
    }
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
        let (_,_,_,commit)=parse_external_ref(&intent.external_ref).expect("bound external_ref");assert_eq!(t.immutable_commit.0,commit,"emit artifact binding");
        let passed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM review_runs r JOIN review_active a ON r.order_id=a.order_id AND r.run_id=a.journal_run WHERE a.key_json=?1 AND r.status='passed')",[key_json(&intent)],|r|r.get(0)).unwrap();
        assert!(passed && event=="InspectionPassed","publication requires a passed review");
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

fn open_db(p:&Path)->Result<Connection,String>{if let Some(p)=p.parent().filter(|p|!p.as_os_str().is_empty()){fs::create_dir_all(p).map_err(|e|e.to_string())?;}let c=Connection::open(p).map_err(sqlerr)?;c.pragma_update(None,"journal_mode","WAL").map_err(sqlerr)?;c.pragma_update(None,"synchronous","FULL").map_err(sqlerr)?;c.execute_batch("CREATE TABLE IF NOT EXISTS review_runs(order_id TEXT,attempt INTEGER,run_id TEXT,base_commit TEXT,result_commit TEXT,pack_digest TEXT,status TEXT,counters_json TEXT,hold_json TEXT,result_json TEXT,provenance_json TEXT,created_at INTEGER,UNIQUE(order_id,attempt,run_id));CREATE TABLE IF NOT EXISTS host_accept(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS host_results(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,result_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_sessions(order_id TEXT,run_id TEXT,session_id TEXT,receipt_json TEXT,PRIMARY KEY(order_id,run_id,session_id));CREATE TABLE IF NOT EXISTS review_side_records(order_id TEXT,run_id TEXT,kind TEXT,value TEXT);CREATE TABLE IF NOT EXISTS review_emit(key_json TEXT PRIMARY KEY,receipt_json TEXT);CREATE TABLE IF NOT EXISTS review_promotions(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_log(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_complete(key_json TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS review_intents(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,value_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_started(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));
CREATE UNIQUE INDEX IF NOT EXISTS review_side_once ON review_side_records(order_id,run_id,kind);
CREATE TABLE IF NOT EXISTS review_epochs(order_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS review_active(order_id TEXT PRIMARY KEY,key_json TEXT NOT NULL,epoch INTEGER NOT NULL,journal_run TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_bindings(key_json TEXT PRIMARY KEY,intent_json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS review_failures(order_id TEXT,run_id TEXT,error TEXT);
CREATE TABLE IF NOT EXISTS review_stage_attempts(order_id TEXT,run_id TEXT,sequence INTEGER,stage_id TEXT,status TEXT,output_json TEXT,UNIQUE(order_id,run_id,sequence));
CREATE TABLE IF NOT EXISTS review_budget(order_id TEXT PRIMARY KEY,used INTEGER NOT NULL);
").map_err(sqlerr)?;Ok(c)}
fn latest_counters(tx:&rusqlite::Transaction<'_>,order:&str)->Result<BTreeMap<String,u64>,HostError>{let v:Option<String>=tx.query_row("SELECT counters_json FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",[order],|r|r.get(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?;v.map(|s|serde_json::from_str(&s).map_err(|e|HostError::Persistence(e.to_string()))).transpose().map(|x|x.unwrap_or_default())}
fn parse_external_ref(s:&str)->Option<(String,u64,String,String)>{
    let (identity,commits)=s.strip_prefix("review:")?.rsplit_once(':')?;
    let (order,attempt)=identity.rsplit_once(':')?;let (base,result)=commits.split_once("..")?;
    if order.is_empty() || base.is_empty() || result.is_empty() || result.contains("..") {return None;}
    Some((order.into(),attempt.parse().ok()?,base.into(),result.into()))
}
fn host_key(i:&ExecutionIntent)->HostKey{HostKey{order:i.order.0.clone(),attempt:i.attempt.0,run:i.run.0,activation:i.activation}}
fn receipt(t:&ArtifactTuple,e:&str)->EmitReceipt{EmitReceipt{key:t.key(),event:e.into(),at:chrono::Utc::now()}}
fn sqlerr(e:impl std::fmt::Display)->String{e.to_string()} fn packerr(e:impl std::fmt::Display)->foundry_pack::PackError{foundry_pack::PackError{code:"E_HOST_STAGE",message:e.to_string()}}
fn expand_home(s:&str)->PathBuf{if let Some(x)=s.strip_prefix("~/")&&let Ok(h)=std::env::var("HOME"){return PathBuf::from(h).join(x)}PathBuf::from(s)}

// Helper factory used by CLI and serve; all S4 receipts and tuples survive restart.
pub fn durable_review_run(host:PackReviewHost, db:impl AsRef<Path>, tuples:std::sync::Arc<dyn TupleStore>)->Result<DurableReviewRun<SqliteReviewSink,SqliteReviewLog>,String>{if fs::canonicalize(db.as_ref()).map_err(sqlerr)? != host.db_path {return Err("host and runner must use the same database".into());}let(sink,log,intents)=PackReviewHost::sqlite_ports(db)?;Ok(DurableReviewRun::new(sink,log,Box::new(host),Box::new(intents),tuples))}

/// T31 Line maps every runner Err to InspectionFailed. Pack holds and fail-run
/// authorize no domain event, so product wiring must stop that call before the
/// legacy error mapping. The durable host journal remains the recovery source.
/// Replace this fail-stop bridge when Line has a hold-aware review outcome.
pub struct InspectingReviewRun<R> { inner:R }
impl<R> InspectingReviewRun<R> {
    pub fn new(inner:R)->Self{Self{inner}}
}
impl<R:foundry_exec::ReviewRunPort> foundry_exec::ReviewRunPort for InspectingReviewRun<R> {
    fn run(&mut self,request:foundry_exec::ReviewRunRequest)->Result<foundry_exec::ReviewEvidence,foundry_exec::ReviewRunError>{
        match self.inner.run(request) {
            Ok(evidence)=>Ok(evidence),
            Err(error)=>panic!("pack review stopped in Inspecting; no domain event authorized: {error:?}"),
        }
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

