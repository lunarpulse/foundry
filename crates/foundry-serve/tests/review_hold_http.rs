use axum::{body::Body, http::{Request, StatusCode}, Router};
use foundry_serve::ServeState;
use std::{sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use tower::ServiceExt;

#[tokio::test]
async fn all_review_hold_routes_require_an_authenticated_session() {
    let unique=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let db=std::env::temp_dir().join(format!("review-hold-{unique}.db"));
    let app=Router::new().merge(foundry_serve::http::router(Arc::new(ServeState::new(&db).unwrap())));
    for (suffix,body) in [
        ("resume",r#"{"target":"lenses"}"#),
        ("commit-pass","{}"),
        ("commit-fail","{}"),
        ("abort","{}"),
    ] {
        let response=app.clone().oneshot(Request::builder().method("POST")
            .uri(format!("/api/orders/o-1/review/hold/h-1/{suffix}"))
            .header("content-type","application/json").body(Body::from(body)).unwrap()).await.unwrap();
        assert_eq!(response.status(),StatusCode::UNAUTHORIZED);
    }
    let _=std::fs::remove_file(&db);
}

#[tokio::test]
async fn review_hold_routes_enforce_approver_observer_and_disabled_session_matrix() {
    use foundry_serve::identity::Role;
    let unique=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let db=std::env::temp_dir().join(format!("review-role-{unique}.db"));
    let state=ServeState::new(&db).unwrap();
    let (approver,observer,inactive)={
        let mut identity=state.identity.lock().await;
        identity.bootstrap_owner("owner","pw",&FastHasher).unwrap();
        let owner=identity.login("owner","pw",&FastHasher,300).unwrap();
        let actor=identity.resolve_session(&owner).unwrap();
        identity.create_user(&actor,"approver","pw",Role::Approver,&FastHasher).unwrap();
        identity.create_user(&actor,"observer","pw",Role::Observer,&FastHasher).unwrap();
        let approver=identity.login("approver","pw",&FastHasher,300).unwrap();
        let observer=identity.login("observer","pw",&FastHasher,300).unwrap();
        identity.create_user(&actor,"inactive","pw",Role::Approver,&FastHasher).unwrap();
        let inactive=identity.login("inactive","pw",&FastHasher,300).unwrap();
        let inactive_user=identity.resolve_session(&inactive).unwrap();
        identity.set_disabled(&actor,&inactive_user.username,true).unwrap();
        (approver,observer,inactive)
    };
    let app=Router::new().merge(foundry_serve::http::router(Arc::new(state)));
    let request=|token:&str|Request::builder().method("POST").uri("/api/orders/missing/review/hold/missing/commit-pass")
        .header("authorization",format!("Bearer {token}")).header("content-type","application/json").body(Body::from("{}")).unwrap();
    assert_eq!(app.clone().oneshot(request(&observer)).await.unwrap().status(),StatusCode::FORBIDDEN);
    assert_eq!(app.clone().oneshot(request(&inactive)).await.unwrap().status(),StatusCode::UNAUTHORIZED);
    // The active Approver crosses the authorization boundary; the expected
    // 500 is the downstream missing-order fixture, not an authorization denial.
    assert_eq!(app.oneshot(request(&approver)).await.unwrap().status(),StatusCode::INTERNAL_SERVER_ERROR);
    let _=std::fs::remove_file(db);
}

struct FastHasher;
impl foundry_serve::identity::PasswordHasher for FastHasher {
    fn hash(&self,p:&str)->String {format!("test${p}")}
    fn verify(&self,p:&str,h:&str)->bool {self.hash(p)==h}
}
struct GapExecutor;
impl foundry_line::StageExecutor for GapExecutor {
    fn execute(&mut self,task:&foundry_line::StageTask)->Result<foundry_line::StageOutput,String> {
        let path=task.workdir.join("out");std::fs::write(&path,"evidence").unwrap();
        Ok(foundry_line::StageOutput { log_path:path.to_string_lossy().into_owned(),inspected_commit:Some(task.artifact_commit.clone()),
            verdict_category:(task.role=="verify").then(||"intent_gap".into()),findings:task.role=="verify",..Default::default() })
    }
}

#[tokio::test]
async fn cold_serve_resolves_holds_and_unavailable_resume_returns_500_without_consuming() {
    use foundry_exec::{DispatchHost,TupleStore};
    use foundry_domain::{OrderEvent as E,OrderState};
    for suffix in ["resume","commit-pass","commit-fail","abort"] {
        let unique=SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root=std::env::temp_dir().join(format!("review-cold-{unique}"));std::fs::create_dir_all(&root).unwrap();
        let mut st=ServeState::new(root.join("ledger.db")).unwrap();
        let (token,approver_token)={let mut identity=st.identity.lock().await;identity.bootstrap_owner("reviewer","pw",&FastHasher).unwrap();let owner_token=identity.login("reviewer","pw",&FastHasher,300).unwrap();let owner=identity.resolve_session(&owner_token).unwrap();identity.create_user(&owner,"approver","pw",foundry_serve::identity::Role::Approver,&FastHasher).unwrap();let approver_token=identity.login("approver","pw",&FastHasher,300).unwrap();(owner_token,approver_token)};
        let pack=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
        let host_db=root.join("host.db");
        let mut host=foundry_line::PackReviewHost::new(&pack,GapExecutor,&host_db).unwrap().with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0").unwrap();
        let intent=foundry_exec::ExecutionIntent{order:foundry_exec::OrderId("cold-review".into()),attempt:foundry_exec::Attempt(1),run:foundry_exec::RunId(7),activation:1,generation:foundry_exec::Generation(0),epoch:foundry_exec::Epoch(1),external_ref:"review:cold-review:1:base..result".into(),recorded_at:chrono::Utc::now()};
        let hold=host.try_dispatch(&intent).unwrap().held.unwrap();drop(host);
        let actor=foundry_domain::ActorId{kind:foundry_domain::ActorKind::Agent,source:"test".into(),id:"test".into()};
        let mut order=foundry_domain::Order::new("cold-review".into(),foundry_domain::ExternalRef::cron("test","x"),chrono::Utc::now());
        for event in [E::Queued,E::DieStarted,E::DieSucceeded{attempt:1},E::ReviewHeld{reason:hold.reason.clone()}] {
            let from=order.state();order.transition(event.clone(),1).unwrap();
            use foundry_domain::ports::Ledger;
            st.line.ledger.lock().await.append("cold-review",&event,from,order.state(),&actor,chrono::Utc::now()).unwrap();
        }
        assert!(st.line.get("cold-review").await.is_none(),"cold process has only durable state");
        let tuples=Arc::new(foundry_exec::SqliteTupleStore::open(root.join("tuples.db")).unwrap());
        let host=foundry_line::PackReviewHost::new(&pack,foundry_line::UnavailableStageExecutor,&host_db).unwrap().with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0").unwrap();
        let runner=foundry_line::durable_review_run(host,&host_db,tuples.clone()).unwrap();
        st.line=Arc::new(foundry_line::Line::new(st.line.gate.clone(),st.line.harness.clone(),st.line.ledger.clone(),st.line.publisher.clone(),st.line.policy.clone()).with_review_run(runner).with_tuple_store(tuples.clone()));
        let st=Arc::new(st);let app=foundry_serve::http::router(st.clone());
        let bearer=if suffix=="commit-pass"{&approver_token}else{&token};
        let request=||Request::builder().method("POST").uri(format!("/api/orders/cold-review/review/hold/{}/{suffix}",hold.hold_instance_id)).header("authorization",format!("Bearer {bearer}")).header("content-type","application/json").body(Body::from(r#"{"target":"replan_full"}"#)).unwrap();
        let response=app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(),if suffix=="resume"{StatusCode::INTERNAL_SERVER_ERROR}else{StatusCode::OK},"{suffix}");
        let order=st.line.get("cold-review").await.unwrap();
        assert_eq!(order.state(),if matches!(suffix,"commit-fail"|"abort"){OrderState::Queued}else{OrderState::FurnaceHold});
        let tuple=tuples.latest_promoted(&foundry_exec::OrderId("cold-review".into()),foundry_exec::Attempt(1)).unwrap();
        assert_eq!(tuple.is_some(),suffix=="commit-pass");
        if suffix=="resume" {
            let conn=rusqlite::Connection::open(&host_db).unwrap();
            let id:String=conn.query_row("SELECT hold_instance_id FROM review_runs",[],|r|r.get(0)).unwrap();assert_eq!(id,hold.hold_instance_id);
        } else {assert_eq!(app.oneshot(request()).await.unwrap().status(),StatusCode::INTERNAL_SERVER_ERROR);}
        drop(st);let _=std::fs::remove_dir_all(root);
    }
}
