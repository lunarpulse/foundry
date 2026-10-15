//! Product adapter from durable review dispatches to a frozen pack graph.
use foundry_exec::{
    ArtifactTuple, ArtifactTupleKey, Attempt, DispatchHost, DurableReviewRun, EmitLog, EmitReceipt,
    EmitSink, Epoch, ExecutionIntent, HostError, HostKey, HostResult,
    IntentLog, OrderId, PromotionReceipt, RunId, TupleStore,
};
use foundry_pack::{load_pack, EngineAction, GraphEngine, RunInput};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, sync::Mutex};

const REVIEW_PACK_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";

#[derive(Clone, Debug)]
pub struct StageTask {
    pub stage_id: String, pub role: String, pub lens_bytes: Vec<u8>, pub session_id: String,
    pub fix_context: Option<String>, pub base_commit: String, pub workdir: PathBuf,
}
#[derive(Clone, Debug, Default)]
pub struct StageOutput { pub verdict_category: Option<String>, pub findings: bool, pub log_path: String }
pub trait StageExecutor: Send { fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String>; fn restore_session(&mut self, _session_id: &str) -> Result<(), String> { Ok(()) } }
/// Product wiring placeholder until the real LLM/Die adapter is supplied by T33.
#[derive(Default)] pub struct UnavailableStageExecutor;
impl StageExecutor for UnavailableStageExecutor { fn execute(&mut self, _task:&StageTask)->Result<StageOutput,String>{Err("review stage executor is not configured".into())} }

pub struct PackReviewHost { pack_path: PathBuf, db_path: PathBuf, executor: Box<dyn StageExecutor>, conn: Connection, pack_digest: String, dispatches: u64 }
impl PackReviewHost {
    pub fn new(pack_path: impl AsRef<Path>, executor: impl StageExecutor + 'static, db_path: impl AsRef<Path>) -> Result<Self, HostError> {
        if !pack_path.as_ref().is_absolute() { return Err(HostError::Persistence("FOUNDRY_REVIEW_PACK must be absolute".into())); }
        let snap = load_pack(pack_path.as_ref()).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != REVIEW_PACK_DIGEST { return Err(HostError::Persistence(format!("unexpected review pack digest: {}", snap.digest()))); }
        let conn = open_db(db_path.as_ref()).map_err(HostError::Persistence)?;
        Ok(Self { pack_path: pack_path.as_ref().to_path_buf(), db_path: db_path.as_ref().to_path_buf(), executor: Box::new(executor), conn, pack_digest: snap.digest().to_owned(), dispatches: 0 })
    }
    pub fn default_db_path() -> PathBuf { expand_home("~/.foundry/review_host.db") }
    pub fn database_path(&self) -> &Path { &self.db_path }
    pub fn sqlite_ports(path: impl AsRef<Path>) -> Result<(SqliteReviewSink, SqliteReviewLog, SqliteIntentLog), String> {
        let p = path.as_ref(); open_db(p)?;
        Ok((SqliteReviewSink::open(p)?, SqliteReviewLog::open(p)?, SqliteIntentLog::open(p)?))
    }
}
impl DispatchHost for PackReviewHost {
    type Result = HostResult;
    fn dispatch(&mut self, intent: &ExecutionIntent) -> HostResult { self.try_dispatch(intent).unwrap_or_else(|e| panic!("review host: {e:?}")) }
    fn try_dispatch(&mut self, intent: &ExecutionIntent) -> Result<HostResult, HostError> {
        let key = host_key(intent);
        if let Some(r) = self.load_result(&key)? { return Ok(r); }
        let (order, attempt, base, result) = parse_external_ref(&intent.external_ref).ok_or_else(|| HostError::Worker("invalid review external_ref".into()))?;
        if order != intent.order.0 || attempt != intent.attempt.0 { return Err(HostError::Worker("external_ref identity mismatch".into())); }
        let snap = load_pack(&self.pack_path).map_err(|e| HostError::Persistence(e.to_string()))?;
        if snap.digest() != self.pack_digest { return Err(HostError::Persistence("review pack digest changed".into())); }
        let digest = snap.digest().to_owned();
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        let accepted: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64], |r| r.get(0)).map_err(sqlerr).map_err(HostError::Persistence)?;
        if accepted { return Err(HostError::Persistence("accepted activation has no recoverable result; refusing redispatch".into())); }
        let inherited = latest_counters(&tx, &order)?;
        let run_id = format!("{}:{}:{}:{}", order, attempt, intent.run.0, intent.activation);
        tx.execute("INSERT OR IGNORE INTO review_runs(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,unixepoch())",
            params![order, attempt as i64, run_id, base, result, digest, serde_json::to_string(&inherited).map_err(sqlerr).map_err(HostError::Persistence)?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_accept(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)", params![key.order,key.attempt as i64,key.run as i64,key.activation as i64]).map_err(sqlerr).map_err(HostError::Acceptance)?;
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        self.dispatches += 1;
        let engine = GraphEngine::from_snapshot(&snap).map_err(|e| HostError::Persistence(e.to_string()))?;
        let mut state = engine.initial_state(inherited);
        let mut generation = 0_u64;
        let mut last_fix_context = None;
        let mut session_ids = BTreeSet::new();
        let mut fresh_sessions = BTreeMap::new();
        let mut accepted_logs = Vec::new();
        let mut outcomes = Vec::new();
        let output = RunInput { verdicts: vec![], has_findings: false, budget_exhausted: false, timeout: false };
        let outcome = engine.run_staged(&mut state, &output, 10_000, |node_id, redrive, input| {
            let node = snap.nodes().iter().find(|n| n.id == node_id).ok_or_else(|| packerr(format!("unknown stage node {node_id}")))?;
            let stage_values: Vec<toml::Value> = if redrive {
                let Some(id) = node.value.get("pre_stage").and_then(toml::Value::as_str) else { generation = generation.saturating_add(1); return Ok(input.clone()); };
                let p = snap.policy().get("pack_stages").and_then(toml::Value::as_array).and_then(|a| a.iter().find(|v| v.get("id").and_then(toml::Value::as_str)==Some(id))).cloned().ok_or_else(|| packerr(format!("missing pre-stage {id}")))?;
                generation = generation.saturating_add(1); vec![p]
            } else {
                node.value.get("stages").and_then(toml::Value::as_array).cloned().ok_or_else(|| packerr("dispatch has no stages"))?
            };
            let mut done = BTreeSet::new();
            let mut verdicts = Vec::new(); let mut findings = false;
            for stage in stage_values {
                let id = stage.get("id").and_then(toml::Value::as_str).ok_or_else(|| packerr("stage missing id"))?.to_owned();
                let deps: Vec<String> = match stage.get("after") { Some(v) if v.is_str()=>vec![v.as_str().unwrap().to_owned()], Some(v) if v.is_array()=>v.as_array().unwrap().iter().filter_map(toml::Value::as_str).map(str::to_owned).collect(), _=>vec![] };
                if deps.iter().any(|d| !done.contains(d)) { return Err(packerr(format!("stage seal violation: {id}"))); }
                let role=stage.get("role").and_then(toml::Value::as_str).ok_or_else(|| packerr("stage missing role"))?.to_owned();
                let session=stage.get("session").and_then(toml::Value::as_str).unwrap_or(&id).to_owned();
                let session_id=format!("{}:{}", order, session);
                let lens_names=stage.get("lenses").and_then(toml::Value::as_array).ok_or_else(|| packerr("stage missing lenses"))?;
                let mut lens_bytes=Vec::new(); for name in lens_names { let name=name.as_str().ok_or_else(||packerr("lens path not string"))?; let b=snap.files().get(name).ok_or_else(||packerr(format!("lens not in snapshot: {name}")))?; lens_bytes.extend_from_slice(b); lens_bytes.push(b'\n'); }
                let saved_session: Option<String> = self.conn.query_row("SELECT receipt_json FROM review_sessions WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_id],|r|r.get(0)).optional().map_err(sqlerr).map_err(packerr)?;
                let restored_id = saved_session.as_ref().and_then(|s|serde_json::from_str::<serde_json::Value>(s).ok()).and_then(|v|v.get("session_id").and_then(serde_json::Value::as_str).map(str::to_owned)).unwrap_or_else(||session_id.clone());
                let mut task_session = fresh_sessions.get(&session_id).cloned().unwrap_or_else(|| restored_id.clone());
                if session_ids.insert(session_id.clone()) && saved_session.is_some() && self.executor.restore_session(&restored_id).is_err() {
                    self.conn.execute("UPDATE review_runs SET status='invalidated' WHERE order_id=?1 AND status IN ('passed','held','failed')",[&order]).map_err(sqlerr).map_err(packerr)?;
                    task_session = format!("{}:{}:fresh:{}",order,session,intent.run.0);
                    fresh_sessions.insert(session_id.clone(), task_session.clone());
                }
                let workdir=self.db_path.parent().unwrap_or(Path::new("." )).join("review-runs").join(&run_id).join("exec").join(intent.activation.to_string()).join("0").join(format!("gen-{generation}"));
                fs::create_dir_all(&workdir).map_err(|e|packerr(e.to_string()))?;
                let task=StageTask { stage_id:id.clone(), role, lens_bytes, session_id:task_session, fix_context:last_fix_context.clone(), base_commit:base.clone(), workdir };
                let out=self.executor.execute(&task).map_err(packerr)?;
                if !out.log_path.is_empty() { accepted_logs.push(out.log_path.clone()); }
                if !redrive { if let Some(v)=out.verdict_category.clone() { verdicts.push(v.clone()); if v == "defer" { self.conn.execute("INSERT INTO review_side_records(order_id,run_id,kind,value) VALUES(?1,?2,'deferred',?3)",params![order,run_id,out.log_path]).map_err(sqlerr).map_err(packerr)?; } } findings |= out.findings; if !out.log_path.is_empty() { last_fix_context=Some(out.log_path.clone()); } }
                else if !out.log_path.is_empty() { last_fix_context=Some(out.log_path.clone()); }
                done.insert(id);
                outcomes.push((session_id, task.session_id, out.log_path));
            }
            if redrive { Ok(input.clone()) } else { Ok(RunInput { verdicts, has_findings:findings, budget_exhausted:input.budget_exhausted, timeout:input.timeout }) }
        }).map_err(|e|HostError::Worker(e.to_string()))?;
        let passed=outcome.terminal && outcome.actions.iter().any(|a|matches!(a,EngineAction::Emit{event,..} if event=="InspectionPassed"));
        let exit_code=if passed {0} else {1};
        let log_path=accepted_logs.last().cloned().unwrap_or_else(||format!("{}#{}", self.pack_path.display(),run_id));
        let host=HostResult{base_commit:base,result_commit:result,log_path,exit_code};
        let provenance=provenance(&digest).map_err(HostError::Persistence)?;
        let tx=self.conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("UPDATE review_runs SET status=?1, counters_json=?2, hold_json=?3, result_json=?4, provenance_json=?5 WHERE order_id=?6 AND attempt=?7 AND run_id=?8",
            params![if passed{"passed"} else if outcome.hold.is_some(){"held"}else{"failed"},serde_json::to_string(&state.counters).map_err(sqlerr).map_err(HostError::Persistence)?,serde_json::json!({"node_id":outcome.hold.as_ref().map(|h|h.node_id.clone()),"entry_counters":outcome.hold.as_ref().map(|h|h.entry_counters.clone()),"reverted_base":outcome.hold.as_ref().and_then(|h|h.reverted_base.clone())}).to_string(),serde_json::to_string(&host).map_err(sqlerr).map_err(HostError::Persistence)?,serde_json::to_string(&provenance).map_err(sqlerr).map_err(HostError::Persistence)?,order,attempt as i64,run_id]).map_err(sqlerr).map_err(HostError::Persistence)?;
        tx.execute("INSERT INTO host_results(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",params![key.order,key.attempt as i64,key.run as i64,key.activation as i64,serde_json::to_string(&host).map_err(|e|HostError::Persistence(e.to_string()))?]).map_err(sqlerr).map_err(HostError::Persistence)?;
        for (sid,actual,path) in outcomes { tx.execute("INSERT OR REPLACE INTO review_sessions(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![order,run_id,sid,serde_json::json!({"session_id":actual,"output":path}).to_string()]).map_err(sqlerr).map_err(HostError::Persistence)?; }
        tx.commit().map_err(sqlerr).map_err(HostError::Persistence)?;
        Ok(host)
    }
    fn dispatch_count(&self)->u64 { self.dispatches }
    fn accepted(&self,intent:&ExecutionIntent)->bool { self.conn.query_row("SELECT EXISTS(SELECT 1 FROM host_accept WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],|r|r.get::<_,bool>(0)).unwrap_or_else(|e|panic!("host acceptance read failed: {e}")) }
    fn recover_result(&mut self,intent:&ExecutionIntent)->Option<HostResult>{ self.load_result(&host_key(intent)).unwrap_or_else(|e|panic!("host result recovery failed: {e:?}")) }
}
impl PackReviewHost {
    fn load_result(&self,k:&HostKey)->Result<Option<HostResult>,HostError>{ self.conn.query_row("SELECT result_json FROM host_results WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![k.order,k.attempt as i64,k.run as i64,k.activation as i64],|r|r.get::<_,String>(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?.map(|s|serde_json::from_str(&s).map_err(|e|HostError::Persistence(e.to_string()))).transpose() }
}

#[derive(Clone,Debug,serde::Serialize,serde::Deserialize)] pub struct BinaryProvenance { pub executable:String,pub sha256:String,pub source_commit:String,pub pack_digest:String }
fn provenance(pack:&str)->Result<BinaryProvenance,String> { let exe=std::env::current_exe().map_err(sqlerr)?; let bytes=fs::read(&exe).map_err(sqlerr)?; let source_commit=std::env::var("FOUNDRY_SOURCE_COMMIT").ok().or_else(||option_env!("FOUNDRY_SOURCE_COMMIT").map(str::to_owned)).or_else(||std::process::Command::new("git").args(["rev-parse","HEAD"]).output().ok().filter(|o|o.status.success()).and_then(|o|String::from_utf8(o.stdout).ok()).map(|s|s.trim().to_owned())).filter(|s|!s.is_empty()).ok_or_else(||"FOUNDRY_SOURCE_COMMIT is unset and git metadata is unavailable".to_owned())?;Ok(BinaryProvenance{executable:exe.display().to_string(),sha256:format!("{:x}",Sha256::digest(bytes)),source_commit,pack_digest:pack.into()}) }

pub struct SqliteReviewSink{ conn:Mutex<Connection> } pub struct SqliteReviewLog{conn:Mutex<Connection>} pub struct SqliteIntentLog{conn:Mutex<Connection>}
macro_rules! open_port { ($t:ty) => { impl $t { pub fn open(p:impl AsRef<Path>)->Result<Self,String>{Ok(Self{conn:Mutex::new(open_db(p.as_ref())?)})} } }; }
open_port!(SqliteReviewSink); open_port!(SqliteReviewLog); open_port!(SqliteIntentLog);
impl EmitSink for SqliteReviewSink {
    fn publish(&mut self,t:&ArtifactTuple)->EmitReceipt{self.publish_named(t,"InspectionPassed","summary")}
    fn publish_named(&mut self,t:&ArtifactTuple,event:&str,_payload:&str)->EmitReceipt{let mut c=self.conn.lock().unwrap();let tx=c.transaction_with_behavior(TransactionBehavior::Immediate).unwrap();let rec=receipt(t,event);tx.execute("INSERT OR IGNORE INTO review_emit(key_json,receipt_json) VALUES(?1,?2)",params![serde_json::to_string(&t.key()).unwrap(),serde_json::to_string(&rec).unwrap()]).unwrap();let s:String=tx.query_row("SELECT receipt_json FROM review_emit WHERE key_json=?1",[serde_json::to_string(&t.key()).unwrap()],|r|r.get(0)).unwrap();tx.commit().unwrap();serde_json::from_str(&s).unwrap()}
    fn receipt_of(&self,k:&ArtifactTupleKey)->Option<EmitReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT receipt_json FROM review_emit WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("emit receipt read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("emit receipt corrupt: {e}")))}
}
impl EmitLog for SqliteReviewLog {
    fn current_epoch(&self)->Epoch{Epoch(0)}
    fn promotion_of(&self,k:&ArtifactTupleKey)->Option<PromotionReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_promotions WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("promotion read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("promotion receipt corrupt: {e}")))}
    fn emit_of(&self,k:&ArtifactTupleKey)->Option<EmitReceipt>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_emit_log WHERE key_json=?1",[serde_json::to_string(k).unwrap()],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("emit log read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("emit receipt corrupt: {e}")))}
    fn record_emit(&mut self,r:&EmitReceipt){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_emit_log VALUES(?1,?2)",params![serde_json::to_string(&r.key).unwrap(),serde_json::to_string(r).unwrap()]).unwrap();}
    fn mark_complete(&mut self,k:&ArtifactTupleKey){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_emit_complete VALUES(?1)",[serde_json::to_string(k).unwrap()]).unwrap();}
    fn is_complete(&self,k:&ArtifactTupleKey)->bool{self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM review_emit_complete WHERE key_json=?1)",[serde_json::to_string(k).unwrap()],|r|r.get(0)).unwrap_or_else(|e|panic!("emit completion read failed: {e}"))}
}
impl IntentLog for SqliteIntentLog {
    fn append(&mut self,i:&ExecutionIntent){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_intents VALUES(?1,?2,?3,?4,?5)",params![i.order.0,i.attempt.0 as i64,i.run.0 as i64,i.activation,serde_json::to_string(i).unwrap()]).unwrap();}
    fn mark_started(&mut self,k:&(OrderId,Attempt,RunId),a:u64){self.conn.lock().unwrap().execute("INSERT OR IGNORE INTO review_started VALUES(?1,?2,?3,?4)",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64]).unwrap();}
    fn latest(&self,k:&(OrderId,Attempt,RunId))->Option<ExecutionIntent>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 ORDER BY activation DESC LIMIT 1",params![k.0.0,k.1.0 as i64,k.2.0 as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("intent corrupt: {e}")))}
    fn get(&self,k:&(OrderId,Attempt,RunId),a:u64)->Option<ExecutionIntent>{let s=self.conn.lock().unwrap().query_row("SELECT value_json FROM review_intents WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get::<_,String>(0)).optional().unwrap_or_else(|e|panic!("intent read failed: {e}"));s.map(|v|serde_json::from_str(&v).unwrap_or_else(|e|panic!("intent corrupt: {e}")))}
    fn is_started(&self,k:&(OrderId,Attempt,RunId),a:u64)->bool{self.conn.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM review_started WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![k.0.0,k.1.0 as i64,k.2.0 as i64,a as i64],|r|r.get(0)).unwrap_or_else(|e|panic!("intent claim read failed: {e}"))}
}

fn open_db(p:&Path)->Result<Connection,String>{if let Some(p)=p.parent(){fs::create_dir_all(p).map_err(|e|e.to_string())?;}let c=Connection::open(p).map_err(sqlerr)?;c.pragma_update(None,"journal_mode","WAL").map_err(sqlerr)?;c.pragma_update(None,"synchronous","FULL").map_err(sqlerr)?;c.execute_batch("CREATE TABLE IF NOT EXISTS review_runs(order_id TEXT,attempt INTEGER,run_id TEXT,base_commit TEXT,result_commit TEXT,pack_digest TEXT,status TEXT,counters_json TEXT,hold_json TEXT,result_json TEXT,provenance_json TEXT,created_at INTEGER,UNIQUE(order_id,attempt,run_id));CREATE TABLE IF NOT EXISTS host_accept(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS host_results(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,result_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_sessions(order_id TEXT,run_id TEXT,session_id TEXT,receipt_json TEXT,PRIMARY KEY(order_id,run_id,session_id));CREATE TABLE IF NOT EXISTS review_side_records(order_id TEXT,run_id TEXT,kind TEXT,value TEXT);CREATE TABLE IF NOT EXISTS review_emit(key_json TEXT PRIMARY KEY,receipt_json TEXT);CREATE TABLE IF NOT EXISTS review_promotions(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_log(key_json TEXT PRIMARY KEY,value_json TEXT);CREATE TABLE IF NOT EXISTS review_emit_complete(key_json TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS review_intents(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,value_json TEXT,UNIQUE(order_id,attempt,run_id,activation));CREATE TABLE IF NOT EXISTS review_started(order_id TEXT,attempt INTEGER,run_id INTEGER,activation INTEGER,UNIQUE(order_id,attempt,run_id,activation));").map_err(sqlerr)?;Ok(c)}
fn latest_counters(tx:&rusqlite::Transaction<'_>,order:&str)->Result<BTreeMap<String,u64>,HostError>{let v:Option<String>=tx.query_row("SELECT counters_json FROM review_runs WHERE order_id=?1 ORDER BY rowid DESC LIMIT 1",[order],|r|r.get(0)).optional().map_err(sqlerr).map_err(HostError::Persistence)?;v.map(|s|serde_json::from_str(&s).map_err(|e|HostError::Persistence(e.to_string()))).transpose().map(|x|x.unwrap_or_default())}
fn parse_external_ref(s:&str)->Option<(String,u64,String,String)>{let rest=s.strip_prefix("review:")?;let (order,tail)=rest.split_once(':')?;let (attempt,commits)=tail.split_once(':')?;let (base,result)=commits.split_once("..")?;Some((order.to_owned(),attempt.parse().ok()?,base.to_owned(),result.to_owned()))}
fn host_key(i:&ExecutionIntent)->HostKey{HostKey{order:i.order.0.clone(),attempt:i.attempt.0,run:i.run.0,activation:i.activation}}
fn receipt(t:&ArtifactTuple,e:&str)->EmitReceipt{EmitReceipt{key:t.key(),event:e.into(),at:chrono::Utc::now()}}
fn sqlerr(e:impl std::fmt::Display)->String{e.to_string()} fn packerr(e:impl std::fmt::Display)->foundry_pack::PackError{foundry_pack::PackError{code:"E_HOST_STAGE",message:e.to_string()}}
fn expand_home(s:&str)->PathBuf{if let Some(x)=s.strip_prefix("~/")&&let Ok(h)=std::env::var("HOME"){return PathBuf::from(h).join(x)}PathBuf::from(s)}

// Helper factory used by CLI and serve; all S4 receipts and tuples survive restart.
pub fn durable_review_run(host:PackReviewHost, db:impl AsRef<Path>, tuples:std::sync::Arc<dyn TupleStore>)->Result<DurableReviewRun<SqliteReviewSink,SqliteReviewLog>,String>{let(sink,log,intents)=PackReviewHost::sqlite_ports(db)?;Ok(DurableReviewRun::new(sink,log,Box::new(host),Box::new(intents),tuples))}

