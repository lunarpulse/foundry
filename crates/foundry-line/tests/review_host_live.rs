use foundry_exec::{Attempt, DispatchHost, Epoch, ExecutionIntent, Generation, OrderId, ReviewRunPort, ReviewRunRequest, RunId, SqliteTupleStore, TupleStore};
use foundry_line::{durable_review_run, PackReviewHost, StageExecutor, StageOutput, StageTask};
use std::{collections::VecDeque, path::{Path, PathBuf}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}};

const PACK: &str = "../../skills/bmad-review-full/v1";
struct Db(PathBuf);
impl Db { fn new() -> Self { static N: std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(0); Self(std::env::temp_dir().join(format!("foundry-review-live-{}-{}.db",std::process::id(),N.fetch_add(1,Ordering::Relaxed)))) } }
impl Drop for Db { fn drop(&mut self) { for p in [&self.0,self.0.with_extension("db-wal").as_path(),self.0.with_extension("db-shm").as_path()] { let _=std::fs::remove_file(p); } } }
#[derive(Default)] struct Trace { tasks:Vec<StageTask>, restores:Vec<String>, outputs:VecDeque<StageOutput> }
struct Fake(Arc<Mutex<Trace>>, Arc<AtomicBool>);
impl StageExecutor for Fake {
    fn execute(&mut self,t:&StageTask)->Result<StageOutput,String>{let mut tr=self.0.lock().unwrap();tr.tasks.push(t.clone());Ok(tr.outputs.pop_front().unwrap_or_default())}
    fn restore_session(&mut self,s:&str)->Result<(),String>{self.0.lock().unwrap().restores.push(s.into());if self.1.load(Ordering::SeqCst){Err("session receipt unavailable".into())}else{Ok(())}}
}
fn output(verdict:Option<&str>,findings:bool,n:usize)->StageOutput{StageOutput{verdict_category:verdict.map(str::to_owned),findings,log_path:format!("stage-{n}.log")}}
fn host(db:&Db,trace:Arc<Mutex<Trace>>,fail:Arc<AtomicBool>)->PackReviewHost{let pack=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);PackReviewHost::new(pack,Fake(trace,fail),&db.0).unwrap()}
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
async fn live_line_commits_inspection_passed_after_host_and_tuple_promotion(){let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);let h=host(&db,trace,Arc::new(AtomicBool::new(false)));let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;let runner=durable_review_run(h,&db.0,tuple_port.clone()).unwrap();let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy)).with_review_run(runner).with_tuple_store(tuple_port);line.submit(foundry_domain::Order::new("line-live".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;assert_eq!(line.drive("line-live").await.unwrap(),foundry_domain::OrderState::FurnaceHold);let events=line.ledger.lock().await.0.lock().unwrap().clone();assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed)));assert!(tuples.latest_promoted(&OrderId("line-live".into()),Attempt(1)).unwrap().is_some());}

#[test]
fn patch_redrive_uses_new_generation_fix_context_and_inherits_counter(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..4{t.outputs.push_back(output(Some("patch"),true,i));}for i in 4..8{t.outputs.push_back(output(None,false,i));}}let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));assert_eq!(h.try_dispatch(&intent("patch",1)).unwrap().exit_code,0);assert_eq!(h.try_dispatch(&intent("patch",2)).unwrap().exit_code,0);let t=tr.lock().unwrap();assert!(t.tasks.iter().any(|x|x.workdir.to_string_lossy().contains("gen-1")));assert!(t.tasks.iter().any(|x|x.fix_context.is_some()));let c: String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT counters_json FROM review_runs ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap();assert!(c.contains("fix_rounds\":1"));}

#[test]
fn bad_plan_reaches_hold_and_never_passes_without_human(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..32{t.outputs.push_back(output(Some("bad_plan"),true,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));let r=h.try_dispatch(&intent("bad",1)).unwrap();assert_ne!(r.exit_code,0);let status:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT status FROM review_runs",[],|r|r.get(0)).unwrap();assert_eq!(status,"held");}

#[test]
fn defer_is_journaled_as_side_record_and_can_pass(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..4{t.outputs.push_back(output(Some("defer"),false,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));assert_eq!(h.try_dispatch(&intent("defer",1)).unwrap().exit_code,0);let n:i64=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT count(*) FROM review_side_records WHERE kind='deferred'",[],|r|r.get(0)).unwrap();assert!(n>0);}

#[test]
fn findings_block_pass_even_when_fake_returns_no_verdict(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..4{t.outputs.push_back(output(None,true,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));assert_ne!(h.try_dispatch(&intent("findings",1)).unwrap().exit_code,0);}

#[test]
fn failed_session_restore_invalidates_prior_run_and_starts_fresh(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let fail=Arc::new(AtomicBool::new(false));let mut h=host(&db,tr.clone(),fail.clone());assert_eq!(h.try_dispatch(&intent("restore",1)).unwrap().exit_code,0);passed_outputs(&tr);fail.store(true,Ordering::SeqCst);assert_eq!(h.try_dispatch(&intent("restore",2)).unwrap().exit_code,0);let status:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT status FROM review_runs WHERE run_id LIKE '%:7:1'",[],|r|r.get(0)).unwrap();assert_eq!(status,"invalidated");assert!(tr.lock().unwrap().tasks.iter().any(|t|t.session_id.contains("fresh")));}

#[test]
fn sealed_claims_run_after_edge_in_trace_session(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));h.try_dispatch(&intent("seal",1)).unwrap();let t=tr.lock().unwrap();let edge=t.tasks.iter().position(|x|x.stage_id=="edge").unwrap();let claims=t.tasks.iter().position(|x|x.stage_id=="claims").unwrap();assert!(edge<claims);assert_eq!(t.tasks[edge].session_id,t.tasks[claims].session_id);}

#[test]
fn accepted_result_recovers_idempotently_without_reexecution(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));let i=intent("recover",1);let first=h.try_dispatch(&i).unwrap();let count=tr.lock().unwrap().tasks.len();let second=h.try_dispatch(&i).unwrap();assert_eq!(first,second);assert_eq!(count,tr.lock().unwrap().tasks.len());assert_eq!(h.recover_result(&i),Some(first));}

#[test]
fn run_records_binary_and_pack_provenance(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));passed_outputs(&tr);let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));h.try_dispatch(&intent("prov",1)).unwrap();let p:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT provenance_json FROM review_runs",[],|r|r.get(0)).unwrap();assert!(p.contains("sha256"));assert!(p.contains("e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8"));}

#[test]
fn malformed_external_ref_is_rejected_and_changed_pack_digest_fails_closed(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));let mut bad=intent("bad-ref",1);bad.external_ref="wat".into();assert!(h.try_dispatch(&bad).is_err());let source=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);let copy=std::env::temp_dir().join(format!("review-pack-{}",std::process::id()));let _=std::fs::remove_dir_all(&copy);std::fs::create_dir_all(&copy).unwrap();for entry in std::fs::read_dir(&source).unwrap(){let entry=entry.unwrap();if entry.file_type().unwrap().is_file(){std::fs::copy(entry.path(),copy.join(entry.file_name())).unwrap();}}let d2=Db::new();let mut changed=PackReviewHost::new(&copy,Fake(tr,Arc::new(AtomicBool::new(false))),&d2.0).unwrap();let lens=copy.join("lens-edge.md");let before=std::fs::read(&lens).unwrap();std::fs::write(&lens,[before.as_slice(),b"\n"].concat()).unwrap();let err=changed.try_dispatch(&intent("digest",2)).unwrap_err();assert!(matches!(err,foundry_exec::HostError::Persistence(_)));let _=std::fs::remove_dir_all(copy);}
