use foundry_exec::{Attempt, DispatchHost, Epoch, ExecutionIntent, Generation, OrderId, ReviewRunPort, ReviewRunRequest, RunId, SqliteTupleStore, TupleStore};
use foundry_line::{durable_review_run, PackReviewHost, StageExecutor, StageOutput, StageTask};
use std::{collections::VecDeque, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

const PACK: &str = "../../skills/bmad-review-full/v1";
struct Db(PathBuf);
impl Db { fn new() -> Self { static N: std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(0); Self(std::env::temp_dir().join(format!("foundry-review-live-{}-{}.db",std::process::id(),N.fetch_add(1,Ordering::Relaxed)))) } }
impl Drop for Db { fn drop(&mut self) { let _=std::fs::remove_dir_all(self.0.with_extension("review-runs")); for p in [&self.0,self.0.with_extension("db-wal").as_path(),self.0.with_extension("db-shm").as_path()] { let _=std::fs::remove_file(p); } } }
#[derive(Default)] struct Trace { tasks:Vec<StageTask>, restores:Vec<String>, outputs:VecDeque<StageOutput>, fail_stage:Option<String>, wrong_commit:bool }
struct Fake(Arc<Mutex<Trace>>, Arc<AtomicBool>);
impl StageExecutor for Fake {
    fn execute(&mut self,t:&StageTask)->Result<StageOutput,String>{let mut tr=self.0.lock().unwrap();tr.tasks.push(t.clone());
        if tr.fail_stage.as_deref()==Some(t.stage_id.as_str()){return Err("injected stage failure".into());}
        let mut out=tr.outputs.pop_front().unwrap_or_default();
        let path=t.workdir.join("output.json");
        std::fs::write(&path,serde_json::json!({"category":out.verdict_category,"categories":out.verdict_categories,"findings":out.findings,"stage":t.stage_id}).to_string()).unwrap();
        out.log_path=path.to_string_lossy().into_owned();
        out.inspected_commit=Some(if tr.wrong_commit {"wrong".into()}else{t.artifact_commit.clone()});
        Ok(out)}
    fn restore_session(&mut self,s:&str,_workdir:&Path)->Result<(),String>{self.0.lock().unwrap().restores.push(s.into());if self.1.load(Ordering::SeqCst){Err("session receipt unavailable".into())}else{Ok(())}}
}
fn output(verdict:Option<&str>,findings:bool,n:usize)->StageOutput{StageOutput{verdict_category:verdict.map(str::to_owned),findings,log_path:format!("stage-{n}.log"),..StageOutput::default()}}
fn host(db:&Db,trace:Arc<Mutex<Trace>>,fail:Arc<AtomicBool>)->PackReviewHost{let pack=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);PackReviewHost::new(pack,Fake(trace,fail),&db.0).unwrap().with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0").unwrap()}
fn intent(order:&str,activation:u64)->ExecutionIntent{ExecutionIntent{order:OrderId(order.into()),attempt:Attempt(1),run:RunId(7),activation,generation:Generation(0),epoch:Epoch(1),external_ref:format!("review:{order}:1:base..result"),recorded_at:chrono::Utc::now()}}
fn passed_outputs(tr:&Arc<Mutex<Trace>>){let mut t=tr.lock().unwrap();for i in 0..4{t.outputs.push_back(output(None,false,i));}}

struct Gate; impl foundry_domain::ports::Gate for Gate{fn verify(&self,_:&foundry_domain::Order,_:&foundry_domain::ports::ApprovalToken,_:&str)->foundry_domain::ports::PortResult<()>{Ok(())}}
struct Policy; impl foundry_domain::ports::Policy for Policy{fn authorize(&self,_:&foundry_domain::ActorId,_:&str,_:&str)->foundry_domain::ports::Decision{foundry_domain::ports::Decision::Allow{reason:"test".into()}}}
#[derive(Default)]struct Events(Mutex<Vec<foundry_domain::OrderEvent>>);
impl foundry_domain::ports::Ledger for Events{fn append(&mut self,_:&str,e:&foundry_domain::OrderEvent,_:foundry_domain::OrderState,_:foundry_domain::OrderState,_:&foundry_domain::ActorId,_:chrono::DateTime<chrono::Utc>)->foundry_domain::ports::PortResult<()>{self.0.lock().unwrap().push(e.clone());Ok(())}}
struct Publisher; impl foundry_domain::ports::Publisher for Publisher{async fn publish(&self,_:&foundry_domain::Order,_:&str,_:&str)->foundry_domain::ports::PortResult<foundry_domain::ports::PublishOutcome>{Ok(foundry_domain::ports::PublishOutcome::Confirmed{pr_url:"test".into()})}}

#[test]
fn real_pack_passes_through_durable_review_and_promotes_tuple(){let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);let fail=Arc::new(AtomicBool::new(false));let h=host(&db,trace,fail);let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;let mut run=durable_review_run(h,&db.0,tuples.clone()).unwrap();let e=run.run(ReviewRunRequest{order:OrderId("pass".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1}).unwrap();assert_eq!(e.verdict,foundry_exec::ReviewVerdict::Passed);assert!(tuples.latest_promoted(&OrderId("pass".into()),Attempt(1)).unwrap().is_some());}

#[tokio::test]
async fn live_line_commits_inspection_passed_after_host_and_tuple_promotion(){let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);let h=host(&db,trace,Arc::new(AtomicBool::new(false)));let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;let runner=durable_review_run(h,&db.0,tuple_port.clone()).unwrap();let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy)).with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuple_port);line.submit(foundry_domain::Order::new("line-live".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;assert_eq!(line.drive("line-live").await.unwrap(),foundry_domain::OrderState::FurnaceHold);let events=line.ledger.lock().await.0.lock().unwrap().clone();assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed)));assert!(tuples.latest_promoted(&OrderId("line-live".into()),Attempt(1)).unwrap().is_some());}

#[test]
fn unsupported_redrive_holds_without_revalidating_the_original_artifact(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{tr.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert_ne!(h.try_dispatch(&intent("patch",1)).unwrap().exit_code,0);
    assert!(h.try_dispatch(&intent("patch",2)).is_err(),"new activation must not bypass a hold");
    assert_eq!(tr.lock().unwrap().tasks.len(),4,"no fake redrive or second review");
    let c:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT counters_json FROM review_runs",[],|r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&c).unwrap()["fix_rounds"],0);
}

#[test]
fn bad_plan_reaches_hold_and_never_passes_without_human(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..32{t.outputs.push_back(output(Some("bad_plan"),true,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));let r=h.try_dispatch(&intent("bad",1)).unwrap();assert_ne!(r.exit_code,0);let status:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT status FROM review_runs",[],|r|r.get(0)).unwrap();assert_eq!(status,"held");}

#[test]
fn defer_is_journaled_once_with_the_durable_emit_and_can_pass(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{tr.lock().unwrap().outputs.push_back(output(Some("defer"),false,i));}
    let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));
    assert_eq!(h.try_dispatch(&intent("defer",1)).unwrap().exit_code,0);
    let n:i64=connection(&db).query_row("SELECT count(*) FROM review_side_records",[],|r|r.get(0)).unwrap();assert_eq!(n,0,"host result alone is not an emit");
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
    let mut runner=durable_review_run(h,&db.0,tuples).unwrap();
    let request=ReviewRunRequest{order:OrderId("defer".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1};
    runner.run(request.clone()).unwrap();runner.run(request).unwrap();
    let n:i64=connection(&db).query_row("SELECT count(*) FROM review_side_records WHERE kind='deferred'",[],|r|r.get(0)).unwrap();assert_eq!(n,1);
}

#[test]
fn findings_block_pass_even_when_fake_returns_no_verdict(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..4{t.outputs.push_back(output(None,true,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));assert_ne!(h.try_dispatch(&intent("findings",1)).unwrap().exit_code,0);}

#[test]
fn failed_session_restore_invalidates_prior_run_and_starts_fresh(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let fail=Arc::new(AtomicBool::new(false));let mut h=host(&db,tr.clone(),fail.clone());assert_eq!(h.try_dispatch(&intent("restore",1)).unwrap().exit_code,0);passed_outputs(&tr);fail.store(true,Ordering::SeqCst);assert_eq!(h.try_dispatch(&intent("restore",2)).unwrap().exit_code,0);let status:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT status FROM review_runs WHERE run_id LIKE '%:7:1'",[],|r|r.get(0)).unwrap();assert_eq!(status,"invalidated");assert!(tr.lock().unwrap().tasks.iter().any(|t|t.session_id.contains("fresh")));
    passed_outputs(&tr);assert_eq!(h.try_dispatch(&intent("restore",3)).unwrap().exit_code,0);
    let sessions:std::collections::BTreeSet<_>=tr.lock().unwrap().tasks.iter().filter(|t|t.stage_id=="edge").map(|t|t.session_id.clone()).collect();
    assert_eq!(sessions.len(),3,"each failed restoration creates a fresh run-scoped session");
}

#[test]
fn sealed_claims_run_after_edge_in_trace_session(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));h.try_dispatch(&intent("seal",1)).unwrap();let t=tr.lock().unwrap();let edge=t.tasks.iter().position(|x|x.stage_id=="edge").unwrap();let claims=t.tasks.iter().position(|x|x.stage_id=="claims").unwrap();assert!(edge<claims);assert_eq!(t.tasks[edge].session_id,t.tasks[claims].session_id);}

#[test]
fn accepted_result_recovers_idempotently_without_reexecution(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));let i=intent("recover",1);let first=h.try_dispatch(&i).unwrap();let count=tr.lock().unwrap().tasks.len();let second=h.try_dispatch(&i).unwrap();assert_eq!(first,second);assert_eq!(count,tr.lock().unwrap().tasks.len());assert_eq!(h.recover_result(&i),Some(first));}

#[test]
fn run_records_binary_and_pack_provenance(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);
    let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));h.try_dispatch(&intent("prov",1)).unwrap();
    let p:String=connection(&db).query_row("SELECT provenance_json FROM review_runs",[],|r|r.get(0)).unwrap();
    let p:foundry_line::BinaryProvenance=serde_json::from_str(&p).unwrap();
    assert_eq!(p.source_commit,"67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0");
    assert_eq!(p.pack_digest,"e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8");
    use sha2::{Digest,Sha256};assert_eq!(p.sha256,format!("{:x}",Sha256::digest(std::fs::read(p.executable).unwrap())));
}

#[test]
fn malformed_external_ref_is_rejected_and_changed_pack_digest_fails_closed(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));let mut bad=intent("bad-ref",1);bad.external_ref="wat".into();assert!(h.try_dispatch(&bad).is_err());let source=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);let copy=std::env::temp_dir().join(format!("review-pack-{}",std::process::id()));let _=std::fs::remove_dir_all(&copy);std::fs::create_dir_all(&copy).unwrap();for entry in std::fs::read_dir(&source).unwrap(){let entry=entry.unwrap();if entry.file_type().unwrap().is_file(){std::fs::copy(entry.path(),copy.join(entry.file_name())).unwrap();}}let d2=Db::new();let mut changed=PackReviewHost::new(&copy,Fake(tr,Arc::new(AtomicBool::new(false))),&d2.0).unwrap().with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0").unwrap();let lens=copy.join("lens-edge.md");let before=std::fs::read(&lens).unwrap();std::fs::write(&lens,[before.as_slice(),b"\n"].concat()).unwrap();let err=changed.try_dispatch(&intent("digest",2)).unwrap_err();assert!(matches!(err,foundry_exec::HostError::Persistence(_)));let _=std::fs::remove_dir_all(copy);}

fn connection(db:&Db)->rusqlite::Connection{rusqlite::Connection::open(&db.0).unwrap()}
fn status(db:&Db)->String{connection(db).query_row("SELECT status FROM review_runs ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap()}
fn tuple(i:&ExecutionIntent)->foundry_exec::ArtifactTuple{
    foundry_exec::ArtifactTuple{order:i.order.clone(),attempt:i.attempt,run:i.run,generation:i.generation,
        immutable_commit:foundry_exec::ImmutableCommit("result".into()),artifact_location:foundry_exec::ArtifactLocation("review-output".into())}
}

#[test]
fn only_verify_output_controls_routing_and_predecessor_inputs_are_sealed(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for n in 0..3{tr.lock().unwrap().outputs.push_back(output(Some("bad_plan"),true,n));}
    tr.lock().unwrap().outputs.push_back(output(None,false,3));
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert_eq!(h.try_dispatch(&intent("scope",1)).unwrap().exit_code,0);
    let tr=tr.lock().unwrap();
    assert!(tr.tasks[0].inputs.is_empty());assert!(tr.tasks[1].inputs.is_empty());
    assert_eq!(tr.tasks[2].inputs.keys().cloned().collect::<Vec<_>>(),vec!["edge"]);
    assert_eq!(tr.tasks[3].inputs.keys().cloned().collect::<Vec<_>>(),vec!["claims","edge","verif"]);
    assert!(tr.tasks.iter().all(|t|t.fix_context.is_none() && t.artifact_commit=="result"));
}

#[test]
fn failed_edge_never_opens_claims_or_verify_and_records_seal_refusal(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace{fail_stage:Some("edge".into()),..Trace::default()}));
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert_ne!(h.try_dispatch(&intent("seal-refusal",1)).unwrap().exit_code,0);
    assert_eq!(status(&db),"held");
    assert_eq!(tr.lock().unwrap().tasks.iter().map(|t|t.stage_id.as_str()).collect::<Vec<_>>(),vec!["edge","verif"]);
    let blocked:i64=connection(&db).query_row("SELECT count(*) FROM review_stage_attempts WHERE status='blocked' AND output_json LIKE '%seal violation%'",[],|r|r.get(0)).unwrap();
    assert_eq!(blocked,2);
}

#[test]
fn failed_verify_and_wrong_artifact_never_promote_a_tuple(){
    for wrong_commit in [false,true]{
        let db=Db::new();let tr=Arc::new(Mutex::new(Trace{wrong_commit,fail_stage:(!wrong_commit).then(||"triage".into()),..Trace::default()}));
        let h=host(&db,tr,Arc::new(AtomicBool::new(false)));
        let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
        let mut runner=durable_review_run(h,&db.0,tuples.clone()).unwrap();
        assert!(runner.run(ReviewRunRequest{order:OrderId("invalid-verify".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1}).is_err());
        assert_eq!(status(&db),"held");
        assert!(tuples.latest_promoted(&OrderId("invalid-verify".into()),Attempt(1)).unwrap().is_none());
    }
}

#[test]
fn aborted_run_keeps_completed_stages_and_counters_and_retries_in_a_new_namespace(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    h.try_dispatch(&intent("retry",1)).unwrap();
    connection(&db).execute("UPDATE review_runs SET counters_json=?1",[r#"{"fix_rounds":2,"loopbacks":4}"#]).unwrap();
    connection(&db).execute("UPDATE review_sessions SET receipt_json='broken-json' WHERE session_id='retry:verif'",[]).unwrap();
    assert!(h.try_dispatch(&intent("retry",2)).is_err());
    assert_eq!(status(&db),"review_aborted");assert!(h.accepted(&intent("retry",2)));
    let completed:i64=connection(&db).query_row("SELECT count(*) FROM review_stage_attempts WHERE run_id LIKE '%:7:2' AND status='complete'",[],|r|r.get(0)).unwrap();
    assert_eq!(completed,1);
    let count=tr.lock().unwrap().tasks.len();
    assert!(h.try_dispatch(&intent("retry",2)).is_err());assert_eq!(tr.lock().unwrap().tasks.len(),count);
    connection(&db).execute("DELETE FROM review_sessions WHERE session_id='retry:verif'",[]).unwrap();
    drop(h);
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert_eq!(h.try_dispatch(&intent("retry",3)).unwrap().exit_code,0);
    let counters:String=connection(&db).query_row("SELECT counters_json FROM review_runs ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&counters).unwrap(),serde_json::json!({"fix_rounds":2,"loopbacks":4}));
    let tasks=&tr.lock().unwrap().tasks;
    let dirs:std::collections::BTreeSet<_>=tasks.iter().map(|t|&t.workdir).collect();
    assert_eq!(dirs.len(),tasks.len(),"accepted attempts never reuse a write namespace");
}

#[test]
fn invalidated_latest_run_counters_are_inherited_across_domain_attempts(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));
    connection(&db).execute("INSERT INTO review_runs(order_id,attempt,run_id,status,counters_json) VALUES('inherit',1,'old','invalidated',?1)",[r#"{"fix_rounds":2,"loopbacks":5}"#]).unwrap();
    let mut i=intent("inherit",1);i.attempt=Attempt(2);i.external_ref="review:inherit:2:base..result".into();
    assert_eq!(h.try_dispatch(&i).unwrap().exit_code,0);
    let counters:String=connection(&db).query_row("SELECT counters_json FROM review_runs ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&counters).unwrap(),serde_json::json!({"fix_rounds":2,"loopbacks":5}));
}

#[test]
fn untrusted_order_ids_and_each_stage_have_separate_contained_workdirs(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    h.try_dispatch(&intent("../../escape:with:colon",1)).unwrap();
    h.try_dispatch(&intent("../../escape:with:colon",2)).unwrap();
    let tasks=&tr.lock().unwrap().tasks;
    let dirs:std::collections::BTreeSet<_>=tasks.iter().map(|t|&t.workdir).collect();
    assert_eq!(dirs.len(),8);
    for task in tasks {
        assert!(task.workdir.starts_with(db.0.with_extension("review-runs")));
        assert_eq!(task.workdir.file_name().unwrap(),"0");
        assert!(!task.workdir.components().any(|part|part==std::path::Component::ParentDir));
    }
}

#[test]
fn epoch_is_durable_scoped_per_order_and_rejects_first_stale_emit(){
    use foundry_exec::{EmitLog,EmitSink,IntentLog};
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));let i=intent("epoch",1);h.try_dispatch(&i).unwrap();
    let (mut sink,mut log,mut intents)=PackReviewHost::sqlite_ports(&db.0).unwrap();intents.append(&i);
    let (_,_,mut other)=PackReviewHost::sqlite_ports(&db.0).unwrap();
    let mut unrelated=intent("unrelated",1);unrelated.epoch=Epoch(99);other.append(&unrelated);
    assert_eq!(log.current_epoch(),Epoch(1));
    let mut newer=i.clone();newer.activation=2;newer.epoch=Epoch(2);other.append(&newer);
    assert_eq!(log.current_epoch(),Epoch(2));
    assert_eq!(foundry_exec::emit_once(&mut sink,&mut log,&tuple(&i),Epoch(1)),foundry_exec::EmitOutcome::StaleRejected);
    // Exercise the sink transaction separately: an epoch change between the
    // log check and publication must still reject before inserting a receipt.
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(||sink.publish(&tuple(&i)))).is_err());
    let count:i64=connection(&db).query_row("SELECT count(*) FROM review_emit",[],|r|r.get(0)).unwrap();assert_eq!(count,0);
    let (_,reopened,mut rebound)=PackReviewHost::sqlite_ports(&db.0).unwrap();rebound.append(&i);
    assert_eq!(reopened.current_epoch(),Epoch(2),"restart and stale append cannot reset epoch");
}

#[test]
fn exact_emit_receipt_is_reused_after_epoch_advance_and_restart(){
    use foundry_exec::{EmitLog,EmitSink,IntentLog};
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));let i=intent("receipt",1);h.try_dispatch(&i).unwrap();
    let (mut sink,_,mut intents)=PackReviewHost::sqlite_ports(&db.0).unwrap();intents.append(&i);
    let first=sink.publish(&tuple(&i));
    let mut newer=i.clone();newer.activation=2;newer.epoch=Epoch(2);intents.append(&newer);
    drop(sink);drop(intents);
    let (mut sink,mut log,mut intents)=PackReviewHost::sqlite_ports(&db.0).unwrap();intents.append(&i);
    assert_eq!(log.current_epoch(),Epoch(2));
    assert_eq!(foundry_exec::emit_once(&mut sink,&mut log,&tuple(&i),Epoch(1)),foundry_exec::EmitOutcome::Existing(first));
    let count:i64=connection(&db).query_row("SELECT count(*) FROM review_emit",[],|r|r.get(0)).unwrap();assert_eq!(count,1);
}

#[test]
fn exhausted_order_budget_blocks_dispatch_before_external_execution(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    connection(&db).execute("INSERT INTO review_budget(order_id,used) VALUES('budget',10000)",[]).unwrap();
    assert_ne!(h.try_dispatch(&intent("budget",1)).unwrap().exit_code,0);
    assert_eq!(status(&db),"held");assert!(tr.lock().unwrap().tasks.is_empty());
}

#[test]
fn recovery_refuses_a_different_artifact_under_the_same_activation(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    let i=intent("binding",1);let first=h.try_dispatch(&i).unwrap();
    let mut changed=i.clone();changed.external_ref="review:binding:1:base..other-result".into();
    assert!(h.try_dispatch(&changed).is_err());assert_eq!(h.try_dispatch(&i).unwrap(),first);
    assert_eq!(tr.lock().unwrap().tasks.len(),4);
}

#[test]
fn ambiguous_running_activation_requires_a_newer_epoch_before_replacement(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    let old=intent("crash",1);h.try_dispatch(&old).unwrap();
    // Reproduce the durable window after stage completion, before host result commit.
    connection(&db).execute("DELETE FROM host_results",[]).unwrap();
    connection(&db).execute("UPDATE review_runs SET status='running',result_json=NULL",[]).unwrap();
    assert!(h.try_dispatch(&old).is_err());assert!(h.try_dispatch(&intent("crash",2)).is_err());
    assert_eq!(tr.lock().unwrap().tasks.len(),4);
    drop(h);
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    let mut replacement=intent("crash",2);replacement.epoch=Epoch(2);
    assert_eq!(h.try_dispatch(&replacement).unwrap().exit_code,0);
    let previous:String=connection(&db).query_row("SELECT status FROM review_runs WHERE run_id LIKE '%:7:1'",[],|r|r.get(0)).unwrap();
    assert_eq!(previous,"invalidated");assert_eq!(tr.lock().unwrap().tasks.len(),8);
}

#[test]
fn persisted_pack_snapshot_reloads_with_the_same_digest_and_completed_outputs_are_bound(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    h.try_dispatch(&intent("snapshot",1)).unwrap();
    let tr=tr.lock().unwrap();let root=tr.tasks[0].workdir.ancestors().nth(3).unwrap();
    let pack=foundry_pack::load_pack(root.join("pack-snapshot")).unwrap();
    assert_eq!(pack.digest(),"e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8");
    let receipt:String=connection(&db).query_row("SELECT output_json FROM review_stage_attempts WHERE stage_id='triage'",[],|r|r.get(0)).unwrap();
    let receipt:serde_json::Value=serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["inspected_commit"],"result");
    let bytes=std::fs::read(receipt["log_path"].as_str().unwrap()).unwrap();
    use sha2::{Digest,Sha256};assert_eq!(receipt["sha256"],format!("{:x}",Sha256::digest(bytes)));
}

#[tokio::test]
async fn product_bridge_keeps_hold_in_inspecting_without_consuming_a_domain_attempt(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{tr.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let h=host(&db,tr,Arc::new(AtomicBool::new(false)));
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
    let runner=durable_review_run(h,&db.0,tuples.clone()).unwrap();
    let line=Arc::new(foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy))
        .with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuples.clone()));
    line.submit(foundry_domain::Order::new("line-hold".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;
    let driving=line.clone();
    let stopped=tokio::spawn(async move{driving.drive("line-hold").await}).await;
    assert!(stopped.unwrap_err().is_panic(),"T31 cannot express hold as a returned runner error");
    assert_eq!(line.get("line-hold").await.unwrap().state(),foundry_domain::OrderState::Inspecting);
    let events=line.ledger.lock().await.0.lock().unwrap().clone();
    assert!(!events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed|foundry_domain::OrderEvent::InspectionFailed{..})));
    assert_eq!(status(&db),"held");
    assert!(tuples.latest_promoted(&OrderId("line-hold".into()),Attempt(1)).unwrap().is_none());
}
