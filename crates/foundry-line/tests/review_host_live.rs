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

fn recover_new_hold(host:&mut PackReviewHost,order:&str) {
    let latest=host.latest_run(order,1).unwrap().unwrap();
    let mut next=intent(order,latest.run+1);next.run=RunId(latest.run+1);next.epoch=Epoch(latest.run+1);
    assert!(host.try_dispatch(&next).unwrap().held.is_some());
}

#[test]
fn real_pack_passes_through_durable_review_and_promotes_tuple(){let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);let fail=Arc::new(AtomicBool::new(false));let h=host(&db,trace,fail);let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;let mut run=durable_review_run(h,&db.0,tuples.clone()).unwrap();let e=run.run(ReviewRunRequest{order:OrderId("pass".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1}).unwrap();assert!(matches!(e,foundry_exec::ReviewRunOutcome::Completed(e) if e.verdict==foundry_exec::ReviewVerdict::Passed));assert!(tuples.latest_promoted(&OrderId("pass".into()),Attempt(1)).unwrap().is_some());}

#[tokio::test]
async fn live_line_commits_inspection_passed_after_host_and_tuple_promotion(){let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);let h=host(&db,trace,Arc::new(AtomicBool::new(false)));let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;let runner=durable_review_run(h,&db.0,tuple_port.clone()).unwrap();let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy)).with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuple_port);line.submit(foundry_domain::Order::new("line-live".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;assert_eq!(line.drive("line-live").await.unwrap(),foundry_domain::OrderState::FurnaceHold);let events=line.ledger.lock().await.0.lock().unwrap().clone();assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed)));assert!(tuples.latest_promoted(&OrderId("line-live".into()),Attempt(1)).unwrap().is_some());}

#[tokio::test]
async fn line_resume_review_binds_final_generation_through_approval_and_publish(){
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    for i in 4..8 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    let h=host(&db,trace,Arc::new(AtomicBool::new(false)));
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;
    let runner=durable_review_run(h,&db.0,tuple_port.clone()).unwrap();
    let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy))
        .with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuple_port);
    line.submit(foundry_domain::Order::new("line-resume".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;
    assert_eq!(line.drive("line-resume").await.unwrap(),foundry_domain::OrderState::FurnaceHold);
    let hold:String=connection(&db).query_row("SELECT hold_instance_id FROM review_runs WHERE order_id='line-resume'",[],|r|r.get(0)).unwrap();
    assert_eq!(line.resolve_review_hold("line-resume",&hold,foundry_exec::ReviewHoldAction::Resume{target:"replan_full".into()},"reviewer").await.unwrap(),foundry_domain::OrderState::FurnaceHold);
    let order=line.get("line-resume").await.unwrap();assert_eq!(order.artifact_digest.as_deref(),Some("commit-B"));
    let tuple=tuples.latest_promoted(&OrderId("line-resume".into()),Attempt(1)).unwrap().unwrap();
    assert_eq!(tuple.immutable_commit.0,"commit-B");assert_eq!(tuple.generations.last().unwrap().commit,"commit-B");
    let events=line.ledger.lock().await.0.lock().unwrap().clone();
    assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::ReviewHeld{..})));
    assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed)));
    let actor=foundry_domain::ActorId{kind:foundry_domain::ActorKind::Human,source:"test".into(),id:"reviewer".into()};
    let token=foundry_domain::ports::ApprovalToken{order_id:"line-resume".into(),intent:"approve".into(),artifact_digest:"commit-B".into(),actor:actor.clone(),granted_at:chrono::Utc::now()};
    assert_eq!(line.approve("line-resume",&token,"approve",&actor).await.unwrap(),foundry_domain::OrderState::Publishing);
    line.publish("line-resume",&actor).await.unwrap();
    assert_eq!(line.get("line-resume").await.unwrap().state(),foundry_domain::OrderState::Shipped);
}

#[tokio::test]
async fn commit_fail_and_abort_follow_the_normal_inspection_failed_attempt_transition(){
    for (order_id,action) in [
        ("line-commit-fail",foundry_exec::ReviewHoldAction::CommitFail),
        ("line-abort",foundry_exec::ReviewHoldAction::Abort),
    ] {
        let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
        for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
        trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
        let h=host(&db,trace,Arc::new(AtomicBool::new(false)));
        let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;
        let runner=durable_review_run(h,&db.0,tuple_port.clone()).unwrap();
        let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy))
            .with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuple_port);
        line.submit(foundry_domain::Order::new(order_id.into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;
        assert_eq!(line.drive(order_id).await.unwrap(),foundry_domain::OrderState::FurnaceHold);
        let hold:String=connection(&db).query_row("SELECT hold_instance_id FROM review_runs WHERE order_id=?1",[order_id],|r|r.get(0)).unwrap();
        assert_eq!(line.resolve_review_hold(order_id,&hold,action,"reviewer").await.unwrap(),foundry_domain::OrderState::Queued);
        let order=line.get(order_id).await.unwrap();assert_eq!(order.attempt(),2);
        assert!(line.ledger.lock().await.0.lock().unwrap().iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionFailed{..})));
    }
}

#[test]
fn redrive_fix_failure_holds_without_revalidating_the_original_artifact(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{tr.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert!(h.try_dispatch(&intent("patch",1)).unwrap().held.is_some());
    assert!(h.try_dispatch(&intent("patch",2)).is_err(),"new activation must not bypass a hold");
    assert_eq!(tr.lock().unwrap().tasks.len(),5,"fix is dispatched once; no original-commit revalidation");
    let c:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT counters_json FROM review_runs",[],|r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&c).unwrap()["fix_rounds"],0);
}

#[test]
fn redrive_fix_commit_is_handed_to_revalidation_and_recorded_as_generation(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4 { tr.lock().unwrap().outputs.push_back(output(Some("patch"),true,i)); }
    let fix=StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()};
    tr.lock().unwrap().outputs.push_back(fix);
    for i in 4..8 { tr.lock().unwrap().outputs.push_back(output(Some("pass"),false,i)); }
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    let result=h.try_dispatch(&intent("redrive-pass",1)).unwrap();
    assert_eq!(result.exit_code,0);
    assert_eq!(result.result_commit,"commit-B");
    assert_eq!(result.generations.iter().map(|g|(g.generation,g.commit.as_str())).collect::<Vec<_>>(),vec![(0,"result"),(1,"commit-B")]);
    let trace=tr.lock().unwrap();
    let fix_task=trace.tasks.iter().find(|task|task.role=="fix").unwrap();
    assert_eq!(fix_task.fix_context.as_deref(),Some("result"));
    assert_eq!(fix_task.base_commit,"result");
    assert_eq!(fix_task.artifact_commit,"result");
    let reverify=trace.tasks.iter().find(|task|task.role=="verify"&&task.artifact_commit=="commit-B").unwrap();
    assert_eq!(reverify.fix_context,None);
}

#[test]
fn runner_publishes_and_promotes_tuple_at_the_final_redrive_head(){
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{trace.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let fix=StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()};trace.lock().unwrap().outputs.push_back(fix);
    for i in 4..8{trace.lock().unwrap().outputs.push_back(output(Some("pass"),false,i));}
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;
    let mut runner=durable_review_run(host(&db,trace,Arc::new(AtomicBool::new(false))),&db.0,tuple_port).unwrap();
    let outcome=runner.run(ReviewRunRequest{order:OrderId("final-head".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1}).unwrap();
    let foundry_exec::ReviewRunOutcome::Completed(evidence)=outcome else{panic!("expected completed review")};
    let tuple=tuples.latest_promoted(&OrderId("final-head".into()),Attempt(1)).unwrap().unwrap();
    assert_eq!(tuple.immutable_commit.0,"commit-B");assert_eq!(evidence.tuple_key,tuple.key());
    assert_eq!(tuple.generations.iter().map(|g|g.commit.as_str()).collect::<Vec<_>>(),vec!["result","commit-B"]);
}

#[test]
fn human_resume_replan_full_executes_policy_role_and_revalidates_new_head(){
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3{trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let held=h.try_dispatch(&intent("replan-resume",1)).unwrap();
    let hold_id=held.held.unwrap().hold_instance_id;
    drop(h);
    let replan=StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()};
    trace.lock().unwrap().outputs.push_back(replan);
    for i in 4..8{trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    connection(&db).execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status,output_json) SELECT order_id,run_id,0,'plan_full','complete',?1 FROM review_runs WHERE order_id='replan-resume'",[serde_json::json!({"log_path":"plan-full.log","verdicts":[]}).to_string()]).unwrap();
    h.resume("replan-resume",&hold_id,"replan_full","human").unwrap();
    let task=trace.lock().unwrap().tasks.iter().find(|task|task.stage_id=="replan_full-g1").unwrap().clone();
    assert_eq!(task.role,"replan");assert_eq!(task.fix_context.as_deref(),Some("result"));
    assert_eq!(task.base_commit,"result");assert_eq!(task.artifact_commit,"result");
    let payload:serde_json::Value=serde_json::from_str(task.inputs.get("plan_full").unwrap()).unwrap();
    assert_eq!(payload,serde_json::json!([{"stage_id":"plan_full","verdicts":[],"log_path":"plan-full.log"}]));
    let latest=h.latest_run("replan-resume",1).unwrap().unwrap();
    assert_eq!(latest.status,"resolved");assert_eq!(latest.result_commit,"result");
    let result_json:String=connection(&db).query_row("SELECT result_json FROM host_results WHERE order_id='replan-resume'",[],|row|row.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&result_json).unwrap()["result_commit"],"commit-B");
    assert!(trace.lock().unwrap().tasks.iter().any(|task|task.role=="verify"&&task.artifact_commit=="commit-B"));
}

#[test]
fn commit_fail_and_abort_are_inherited_by_the_next_activation(){
    for (order, action, expected_status) in [
        ("commit-fail-inherit", foundry_exec::ReviewHoldAction::CommitFail, "resolved"),
        ("abort-inherit", foundry_exec::ReviewHoldAction::Abort, "review_aborted"),
    ] {
        let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
        for i in 0..32 { trace.lock().unwrap().outputs.push_back(output(Some("bad_plan"),true,i)); }
        let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
        assert!(h.try_dispatch(&intent(order,1)).unwrap().held.is_some());
        let hold:String=connection(&db).query_row("SELECT hold_instance_id FROM review_runs WHERE order_id=?1",[order],|r|r.get(0)).unwrap();
        h.resolve_hold(order,&hold,action,"reviewer").unwrap();
        assert_eq!(h.latest_run(order,1).unwrap().unwrap().status,expected_status);
        let tasks_before=trace.lock().unwrap().tasks.len();
        let mut next=intent(order,2);next.run=RunId(8);next.epoch=Epoch(2);
        let recovered=h.try_dispatch(&next).unwrap();
        assert_eq!(recovered.exit_code,1);
        assert_eq!(trace.lock().unwrap().tasks.len(),tasks_before,"resolved fail/abort results are inherited without redispatch");
        let inherited=h.latest_run(order,1).unwrap().unwrap();
        assert_eq!(inherited.run,8);
        assert_eq!(inherited.status,"failed");
    }
}

#[test]
fn resumed_hold_survives_host_restart_then_second_resolution_publishes_final_generation(){
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let first=h.try_dispatch(&intent("repeat-hold",1)).unwrap().held.unwrap();
    drop(h);

    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    for i in 4..7 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,7));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    h.resume("repeat-hold",&first.hold_instance_id,"replan_full","reviewer").unwrap();
    recover_new_hold(&mut h,"repeat-hold");
    let second=h.latest_run("repeat-hold",1).unwrap().unwrap();
    let second_hold:foundry_exec::host::HoldState=serde_json::from_value(serde_json::from_str::<serde_json::Value>(second.hold_json.as_deref().unwrap()).unwrap()["hold"].clone()).unwrap();
    assert_ne!(second_hold.hold_instance_id,first.hold_instance_id);
    assert_eq!(second.status,"held");

    drop(h);
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-C".into()),..StageOutput::default()});
    for i in 8..12 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    h.resume("repeat-hold",&second_hold.hold_instance_id,"replan_full","reviewer").unwrap();
    let resolved=h.latest_run("repeat-hold",1).unwrap().unwrap();
    assert_eq!(resolved.status,"resolved");
    let result:foundry_exec::HostResult=serde_json::from_str(&connection(&db).query_row::<String,_,_>("SELECT result_json FROM host_results WHERE order_id='repeat-hold' ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap()).unwrap();
    assert_eq!(result.result_commit,"commit-C");
    assert_eq!(result.generations.last().unwrap().commit,"commit-C");

    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());let tuple_port=tuples.clone() as Arc<dyn TupleStore>;
    let mut runner=durable_review_run(h,&db.0,tuple_port).unwrap();
    let outcome=runner.run(ReviewRunRequest{order:OrderId("repeat-hold".into()),attempt:Attempt(1),run:RunId(resolved.run+1),base_commit:resolved.base_commit,result_commit:"result".into(),artifact_digest:"commit-C".into(),epoch:Epoch(resolved.run+1),activation:resolved.run+1}).unwrap();
    assert!(matches!(outcome,foundry_exec::ReviewRunOutcome::Completed(_)));
    let tuple=tuples.latest_promoted(&OrderId("repeat-hold".into()),Attempt(1)).unwrap().unwrap();
    assert_eq!(tuple.run.0,resolved.run+1);
    assert_eq!(tuple.immutable_commit.0,"commit-C");
    assert_eq!(tuple.generations.iter().map(|g|g.commit.as_str()).collect::<Vec<_>>(),vec!["result","commit-B","commit-C"]);
}

#[test]
fn bad_plan_reaches_hold_and_never_passes_without_human(){let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));{let mut t=tr.lock().unwrap();for i in 0..32{t.outputs.push_back(output(Some("bad_plan"),true,i));}}let mut h=host(&db,tr,Arc::new(AtomicBool::new(false)));let r=h.try_dispatch(&intent("bad",1)).unwrap();assert_ne!(r.exit_code,0);let status:String=rusqlite::Connection::open(&db.0).unwrap().query_row("SELECT status FROM review_runs",[],|r|r.get(0)).unwrap();assert_eq!(status,"held");}

#[test]
fn human_hold_resolution_is_cas_durable_and_deadline_is_lazy(){
    let db=Db::new();
    let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..32 { tr.lock().unwrap().outputs.push_back(output(Some("bad_plan"),true,i)); }
    let mut h=host(&db,tr.clone(),Arc::new(AtomicBool::new(false)));
    assert!(h.try_dispatch(&intent("human",1)).unwrap().held.is_some());
    let hold:String=connection(&db).query_row("SELECT hold_instance_id FROM review_runs WHERE order_id='human'",[],|r|r.get(0)).unwrap();
    h.commit_pass("human",&hold,"reviewer").unwrap();
    assert!(h.commit_fail("human",&hold,"reviewer").is_err(),"a hold instance is consumed once");
    drop(h);
    let h=host(&db,tr,Arc::new(AtomicBool::new(false)));
    assert_eq!(h.latest_run("human",1).unwrap().unwrap().status,"resolved");

    let db2=Db::new();
    let tr2=Arc::new(Mutex::new(Trace::default()));
    for i in 0..32 { tr2.lock().unwrap().outputs.push_back(output(Some("bad_plan"),true,i)); }
    let mut h=host(&db2,tr2,Arc::new(AtomicBool::new(false)));
    assert!(h.try_dispatch(&intent("deadline",1)).unwrap().held.is_some());
    let hold:String=connection(&db2).query_row("SELECT hold_instance_id FROM review_runs WHERE order_id='deadline'",[],|r|r.get(0)).unwrap();
    connection(&db2).execute("UPDATE review_runs SET deadline=?1 WHERE order_id='deadline'",[chrono::Utc::now().timestamp()-1]).unwrap();
    assert!(h.commit_pass("deadline",&hold,"reviewer").is_err());
    assert_eq!(status(&db2),"held");
    let json:String=connection(&db2).query_row("SELECT hold_json FROM review_runs WHERE order_id='deadline'",[],|r|r.get(0)).unwrap();
    assert!(json.contains("human_deadline_expired"));
}

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
        immutable_commit:foundry_exec::ImmutableCommit("result".into()),artifact_location:foundry_exec::ArtifactLocation("review-output".into()),generations:Vec::new()}
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
fn failed_verify_and_wrong_artifact_hold_without_tuple_promotion(){
    for wrong_commit in [false,true]{
        let db=Db::new();let tr=Arc::new(Mutex::new(Trace{wrong_commit,fail_stage:(!wrong_commit).then(||"triage".into()),..Trace::default()}));
        let h=host(&db,tr,Arc::new(AtomicBool::new(false)));
        let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
        let mut runner=durable_review_run(h,&db.0,tuples.clone()).unwrap();
        assert!(matches!(runner.run(ReviewRunRequest{order:OrderId("invalid-verify".into()),attempt:Attempt(1),run:RunId(7),base_commit:"base".into(),result_commit:"result".into(),artifact_digest:"result".into(),epoch:Epoch(1),activation:1}).unwrap(),foundry_exec::ReviewRunOutcome::Held(_)));
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
async fn product_bridge_records_review_held_without_consuming_a_domain_attempt(){
    let db=Db::new();let tr=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4{tr.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let h=host(&db,tr,Arc::new(AtomicBool::new(false)));
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap()) as Arc<dyn TupleStore>;
    let runner=durable_review_run(h,&db.0,tuples.clone()).unwrap();
    let line=Arc::new(foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy))
        .with_review_run(foundry_line::InspectingReviewRun::new(runner)).with_tuple_store(tuples.clone()));
    line.submit(foundry_domain::Order::new("line-hold".into(),foundry_domain::ExternalRef::cron("dep","x"),chrono::Utc::now())).await;
    assert_eq!(line.drive("line-hold").await.unwrap(),foundry_domain::OrderState::FurnaceHold);
    assert_eq!(line.get("line-hold").await.unwrap().attempt(),1);
    let events=line.ledger.lock().await.0.lock().unwrap().clone();
    assert!(events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::ReviewHeld{..})));
    assert!(!events.iter().any(|e|matches!(e,foundry_domain::OrderEvent::InspectionPassed|foundry_domain::OrderEvent::InspectionFailed{..})));
    assert_eq!(status(&db),"held");
    assert!(tuples.latest_promoted(&OrderId("line-hold".into()),Attempt(1)).unwrap().is_none());
}

#[test]
fn unavailable_resume_preserves_the_original_hold_and_returns_an_error() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace,Arc::new(AtomicBool::new(false)));
    let hold=h.try_dispatch(&intent("unavailable",1)).unwrap().held.unwrap();drop(h);
    let pack=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);
    let mut h=PackReviewHost::new(pack,foundry_line::UnavailableStageExecutor,&db.0).unwrap();
    assert!(h.resume("unavailable",&hold.hold_instance_id,"replan_full","human").is_err());
    let latest=h.latest_run("unavailable",1).unwrap().unwrap();
    assert_eq!(latest.status,"held");assert_eq!(latest.hold_instance_id.as_deref(),Some(hold.hold_instance_id.as_str()));
}

#[test]
fn independent_host_connections_allow_exactly_one_hold_cas_winner() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut initial=host(&db,trace,Arc::new(AtomicBool::new(false)));
    let hold=initial.try_dispatch(&intent("cas",1)).unwrap().held.unwrap();drop(initial);
    let pack=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);
    let a=PackReviewHost::new(&pack,foundry_line::UnavailableStageExecutor,&db.0).unwrap();
    let b=PackReviewHost::new(&pack,foundry_line::UnavailableStageExecutor,&db.0).unwrap();
    let barrier=Arc::new(std::sync::Barrier::new(3));
    let run=|mut host:PackReviewHost,action,barrier:Arc<std::sync::Barrier>,id:String|std::thread::spawn(move||{
        barrier.wait();
        host.resolve_hold("cas",&id,action,"reviewer")
    });
    let first=run(a,foundry_exec::ReviewHoldAction::CommitPass,barrier.clone(),hold.hold_instance_id.clone());
    let second=run(b,foundry_exec::ReviewHoldAction::CommitFail,barrier.clone(),hold.hold_instance_id.clone());
    barrier.wait();
    let outcomes=[first.join().unwrap(),second.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|result|result.is_ok()).count(),1);
    let conn=connection(&db);
    let status:String=conn.query_row("SELECT status FROM review_runs WHERE order_id='cas'",[],|r|r.get(0)).unwrap();
    assert!(matches!(status.as_str(),"resolved"|"review_aborted"));
}

#[test]
fn opening_another_host_cannot_reclaim_a_live_resume() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut initial=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let hold=initial.try_dispatch(&intent("live-resume",1)).unwrap().held.unwrap();drop(initial);
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    passed_outputs(&trace);
    struct Pausing(Fake,Arc<std::sync::Barrier>,Arc<std::sync::Barrier>);
    impl StageExecutor for Pausing {
        fn execute(&mut self,task:&StageTask)->Result<StageOutput,String>{
            if task.fix_context.is_some(){self.1.wait();self.2.wait();}
            self.0.execute(task)
        }
        fn restore_session(&mut self,id:&str,dir:&Path)->Result<(),String>{self.0.restore_session(id,dir)}
    }
    let entered=Arc::new(std::sync::Barrier::new(2));let release=Arc::new(std::sync::Barrier::new(2));
    let pack=Path::new(env!("CARGO_MANIFEST_DIR")).join(PACK);
    let mut active=PackReviewHost::new(&pack,Pausing(Fake(trace,Arc::new(AtomicBool::new(false))),entered.clone(),release.clone()),&db.0).unwrap();
    let id=hold.hold_instance_id.clone();
    let worker=std::thread::spawn(move||active.resume("live-resume",&id,"replan_full","first"));
    entered.wait();
    let mut newcomer=PackReviewHost::new(&pack,foundry_line::UnavailableStageExecutor,&db.0).unwrap();
    let status=newcomer.latest_run("live-resume",1).unwrap().unwrap().status;
    let second=newcomer.commit_pass("live-resume",&hold.hold_instance_id,"second");
    release.wait();let resumed=worker.join().unwrap();
    assert_eq!(status,"resuming");assert!(second.is_err());assert!(resumed.is_ok(),"{resumed:?}");
}

#[test]
fn crash_after_resume_claim_recovers_snapshot_and_can_retry() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut original=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let hold=original.try_dispatch(&intent("crash-resume",1)).unwrap().held.unwrap();drop(original);
    let conn=connection(&db);
    // Fault injection: the durable CAS committed, then the process died after
    // accepting one stage but before saving a new engine checkpoint.
    conn.execute("UPDATE review_runs SET status='resuming',hold_json=json_set(hold_json,'$.resume_sequence',1) WHERE order_id='crash-resume'",[]).unwrap();
    conn.execute("INSERT INTO review_stage_attempts(order_id,run_id,sequence,stage_id,status) SELECT order_id,run_id,99,'interrupted','accepted' FROM review_runs WHERE order_id='crash-resume'",[]).unwrap();
    drop(conn);
    let mut restarted=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let recovered=restarted.latest_run("crash-resume",1).unwrap().unwrap();
    assert_eq!(recovered.status,"held");assert_eq!(recovered.hold_instance_id.as_deref(),Some(hold.hold_instance_id.as_str()));
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    passed_outputs(&trace);
    restarted.resume("crash-resume",&hold.hold_instance_id,"replan_full","retry").unwrap();
    let final_run=restarted.latest_run("crash-resume",1).unwrap().unwrap();
    assert_eq!(final_run.status,"resolved");assert_eq!(final_run.hold_json.unwrap().parse::<serde_json::Value>().unwrap()["resume_sequence"],2);
}

#[test]
fn resuming_run_cannot_be_bypassed_by_another_activation() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));passed_outputs(&trace);
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    h.try_dispatch(&intent("resume-fence",1)).unwrap();
    connection(&db).execute("UPDATE review_runs SET status='resuming'",[]).unwrap();
    let before=trace.lock().unwrap().tasks.len();let mut next=intent("resume-fence",2);next.run=RunId(8);next.epoch=Epoch(2);
    assert!(h.try_dispatch(&next).is_err());assert_eq!(trace.lock().unwrap().tasks.len(),before);
}

#[test]
fn resolved_review_is_not_inherited_by_a_different_attempt() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let hold=h.try_dispatch(&intent("attempt-fence",1)).unwrap().held.unwrap();
    h.commit_pass("attempt-fence",&hold.hold_instance_id,"human").unwrap();
    let before=trace.lock().unwrap().tasks.len();passed_outputs(&trace);
    let mut next=intent("attempt-fence",2);next.attempt=Attempt(2);next.run=RunId(8);next.epoch=Epoch(2);next.external_ref="review:attempt-fence:2:base..result".into();
    h.try_dispatch(&next).unwrap();assert!(trace.lock().unwrap().tasks.len()>before,"new attempts require their own review");
}

#[test]
fn resumed_invalid_verify_enters_a_new_barrier_hold() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let hold=h.try_dispatch(&intent("invalid-resume",1)).unwrap().held.unwrap();
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    trace.lock().unwrap().wrong_commit=true;
    h.resume("invalid-resume",&hold.hold_instance_id,"replan_full","human").unwrap();
    recover_new_hold(&mut h,"invalid-resume");
    let latest=h.latest_run("invalid-resume",1).unwrap().unwrap();assert_eq!(latest.status,"held");
    assert_ne!(latest.hold_instance_id.as_deref(),Some(hold.hold_instance_id.as_str()));
    let doc:serde_json::Value=serde_json::from_str(latest.hold_json.as_deref().unwrap()).unwrap();
    assert_eq!(doc["latest_commit"],"commit-B");
}

#[test]
fn redrive_sessions_use_the_die_key_and_validate_saved_output_before_restore() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4 { trace.lock().unwrap().outputs.push_back(output(Some("patch"),true,i)); }
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..StageOutput::default()});
    for i in 4..7 { trace.lock().unwrap().outputs.push_back(output(None,false,i)); }
    trace.lock().unwrap().outputs.push_back(output(Some("patch"),true,7));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let result=h.try_dispatch(&intent("session-key",1)).unwrap();assert!(result.held.is_some());
    let json:String=connection(&db).query_row("SELECT receipt_json FROM review_sessions WHERE session_id='session-key:die'",[],|r|r.get(0)).unwrap();
    let receipt:serde_json::Value=serde_json::from_str(&json).unwrap();
    std::fs::write(receipt["output"].as_str().unwrap(),"tampered").unwrap();
    trace.lock().unwrap().restores.clear();
    // Exercise the same implementation session via a human fix resume.
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-C".into()),..StageOutput::default()});passed_outputs(&trace);
    h.resume("session-key",&result.held.unwrap().hold_instance_id,"fixdie","human").unwrap();
    assert!(!trace.lock().unwrap().restores.iter().any(|id|id==receipt["session_id"].as_str().unwrap()));
}

#[test]
fn resume_rejects_a_changed_durable_pack_snapshot_before_execution() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 {trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let hold=h.try_dispatch(&intent("snapshot-resume",1)).unwrap().held.unwrap();drop(h);
    let root=trace.lock().unwrap().tasks[0].workdir.ancestors().nth(3).unwrap().to_owned();
    use std::io::Write;
    std::fs::OpenOptions::new().append(true).open(root.join("pack-snapshot/policy.toml")).unwrap().write_all(b"\n# changed snapshot\n").unwrap();
    let before=trace.lock().unwrap().tasks.len();
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    assert!(h.resume("snapshot-resume",&hold.hold_instance_id,"replan_full","human").is_err());
    assert_eq!(trace.lock().unwrap().tasks.len(),before);
    assert_eq!(h.latest_run("snapshot-resume",1).unwrap().unwrap().hold_instance_id.as_deref(),Some(hold.hold_instance_id.as_str()));
}

#[test]
fn repeated_fix_failure_then_success_uses_new_execution_directories() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..4 {trace.lock().unwrap().outputs.push_back(output(Some("patch"),true,i));}
    let mut h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
    let first=h.try_dispatch(&intent("retry-resume",1)).unwrap().held.unwrap();
    h.resume("retry-resume",&first.hold_instance_id,"fixdie","human").unwrap();
    recover_new_hold(&mut h,"retry-resume");
    let second=h.latest_run("retry-resume",1).unwrap().unwrap().hold_instance_id.unwrap();
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("fixed".into()),..StageOutput::default()});passed_outputs(&trace);
    h.resume("retry-resume",&second,"fixdie","human").unwrap();
    assert_eq!(h.latest_run("retry-resume",1).unwrap().unwrap().status,"resolved");
    let tasks=trace.lock().unwrap();let dirs:std::collections::BTreeSet<_>=tasks.tasks.iter().map(|t|&t.workdir).collect();
    assert_eq!(dirs.len(),tasks.tasks.len());
}

#[tokio::test]
async fn line_restarts_between_repeated_holds_and_publishes_the_third_run() {
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 {trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());
    let events=Arc::new(tokio::sync::Mutex::new(Events::default()));
    let make_line=||{
        let h=host(&db,trace.clone(),Arc::new(AtomicBool::new(false)));
        foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),events.clone(),Arc::new(Publisher),Arc::new(Policy))
            .with_review_run(durable_review_run(h,&db.0,tuples.clone()).unwrap()).with_tuple_store(tuples.clone())
    };
    let line=make_line();line.submit(foundry_domain::Order::new("line-repeat".into(),foundry_domain::ExternalRef::cron("test","x"),chrono::Utc::now())).await;
    assert_eq!(line.drive("line-repeat").await.unwrap(),foundry_domain::OrderState::FurnaceHold);
    let order=line.get("line-repeat").await.unwrap();drop(line);
    let line=make_line();line.restore_review_hold(order).await.unwrap();
    let first:String=connection(&db).query_row("SELECT hold_instance_id FROM review_runs ORDER BY rowid DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    let event_count=events.lock().await.0.lock().unwrap().len();
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-B".into()),..Default::default()});
    for i in 4..7 {trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,7));
    line.resolve_review_hold("line-repeat",&first,foundry_exec::ReviewHoldAction::Resume{target:"replan_full".into()},"human").await.unwrap();
    assert_eq!(events.lock().await.0.lock().unwrap().len(),event_count,"repeated hold adds no domain event");
    let (run,second):(String,String)=connection(&db).query_row("SELECT run_id,hold_instance_id FROM review_runs WHERE status='held'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert!(run.starts_with("line-repeat:1:2:"));assert_ne!(first,second);
    let order=line.get("line-repeat").await.unwrap();drop(line);
    let line=make_line();line.restore_review_hold(order).await.unwrap();
    trace.lock().unwrap().outputs.push_back(StageOutput{produced_commit:Some("commit-C".into()),..Default::default()});passed_outputs(&trace);
    line.resolve_review_hold("line-repeat",&second,foundry_exec::ReviewHoldAction::Resume{target:"replan_full".into()},"human").await.unwrap();
    let tuple=tuples.latest_promoted(&OrderId("line-repeat".into()),Attempt(1)).unwrap().unwrap();
    assert_eq!(tuple.run,RunId(3));assert_eq!(tuple.immutable_commit.0,"commit-C");assert_eq!(tuple.generations.len(),3);
    let actor=foundry_domain::ActorId{kind:foundry_domain::ActorKind::Human,source:"test".into(),id:"human".into()};
    let token=foundry_domain::ports::ApprovalToken{order_id:"line-repeat".into(),intent:"approve".into(),artifact_digest:"commit-C".into(),actor:actor.clone(),granted_at:chrono::Utc::now()};
    assert_eq!(line.approve("line-repeat",&token,"approve",&actor).await.unwrap(),foundry_domain::OrderState::Publishing);
}

#[tokio::test]
async fn resumed_commit_fail_at_max_attempts_is_terminal() {
    use foundry_domain::{OrderEvent as E,OrderState};
    let db=Db::new();let trace=Arc::new(Mutex::new(Trace::default()));
    for i in 0..3 {trace.lock().unwrap().outputs.push_back(output(None,false,i));}
    trace.lock().unwrap().outputs.push_back(output(Some("intent_gap"),true,3));
    let mut order=foundry_domain::Order::new("max-review".into(),foundry_domain::ExternalRef::cron("test","x"),chrono::Utc::now());
    order.transition(E::Queued,1).unwrap();
    for attempt in 1..foundry_domain::order::MAX_ATTEMPTS {
        order.transition(E::DieStarted,attempt).unwrap();order.transition(E::DieFailed{attempt,reason:"retry".into()},attempt).unwrap();
    }
    let attempt=foundry_domain::order::MAX_ATTEMPTS;
    order.transition(E::DieStarted,attempt).unwrap();order.transition(E::DieSucceeded{attempt},attempt).unwrap();order.transition(E::ReviewHeld{reason:"gap".into()},attempt).unwrap();
    let mut h=host(&db,trace,Arc::new(AtomicBool::new(false)));let mut request=intent("max-review",1);
    request.attempt=Attempt(attempt as u64);request.external_ref=format!("review:max-review:{attempt}:base..result");
    let hold=h.try_dispatch(&request).unwrap().held.unwrap();
    let tuples=Arc::new(SqliteTupleStore::open(&db.0).unwrap());
    let line=foundry_line::Line::new(Arc::new(Gate),Arc::new(foundry_harness_cli::fake::FakeHarness::default()),Arc::new(tokio::sync::Mutex::new(Events::default())),Arc::new(Publisher),Arc::new(Policy))
        .with_review_run(durable_review_run(h,&db.0,tuples.clone()).unwrap()).with_tuple_store(tuples.clone());
    line.restore_review_hold(order).await.unwrap();
    assert_eq!(line.resolve_review_hold("max-review",&hold.hold_instance_id,foundry_exec::ReviewHoldAction::CommitFail,"human").await.unwrap(),OrderState::Failed);
    assert!(tuples.latest_promoted(&OrderId("max-review".into()),Attempt(attempt as u64)).unwrap().is_none());
}
