use chrono::{Duration, Utc};
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductHoldDecision, ProductHoldExpectation,
    ProductServiceAction, ProductServiceGrant, ProductStore, RepositoryIdentity, ScopeMutation,
    ScopeSourceManifest,
};
use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn scratch() -> std::path::PathBuf {
    let p = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "foundry-b1-service-review-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&p).unwrap();
    p
}
fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "service-review-install".into(),
        database_id: "service-review-db".into(),
    }
}
fn append_waiting(conn: &rusqlite::Connection, order: &str) {
    use foundry_domain::OrderEvent;
    use sha2::Digest as _;
    for (event, from, to) in [
        (OrderEvent::DieStarted, "queued", "working"),
        (
            OrderEvent::DieSucceeded { attempt: 1 },
            "working",
            "inspecting",
        ),
        (OrderEvent::InspectionPassed, "inspecting", "waiting"),
    ] {
        let seq: i64 = conn
            .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
                r.get(0)
            })
            .unwrap();
        let prev: String = conn
            .query_row(
                "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let pred: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM ledger WHERE order_id=?1",
                [order],
                |r| r.get(0),
            )
            .unwrap();
        let at = Utc::now().to_rfc3339();
        let event_text = format!("{event:?}");
        let event_json = foundry_ledger::event_json::event_to_json(&event);
        let base =
            format!("{seq}|{order}|{from}|{to}|{event_text}|Agent/github-intake||{at}|{prev}");
        let hash = sha2::Sha256::digest(format!("{base}|{event_json}").as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        conn.execute("INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,1,?2,'service-test-lifecycle')",rusqlite::params![order,pred]).unwrap();
        conn.execute("INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,'Agent/github-intake','',?5,?6,?7,?8)",rusqlite::params![order,from,to,event_text,at,prev,event_json,hash]).unwrap();
    }
}

struct RevokingExecutor {
    db: std::path::PathBuf,
    session: String,
    lease: chrono::DateTime<Utc>,
    calls: Arc<AtomicUsize>,
}
impl StageExecutor for RevokingExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut store = ProductStore::open(&self.db, config()).map_err(|e| e.to_string())?;
        store
            .manage_product_service(
                &self.session,
                "review-worker",
                &[],
                false,
                self.lease,
                1,
                "revoke-mid-stage",
                Utc::now(),
            )
            .map_err(|e| e.to_string())?;
        let log = task.workdir.join("service-stage-output.log");
        std::fs::write(&log, b"result accepted before revoke\n").map_err(|e| e.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: log.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }
}

struct BoundaryClock {
    first: chrono::DateTime<Utc>,
    boundary: chrono::DateTime<Utc>,
    calls: AtomicUsize,
}
impl foundry_ledger::ProductClock for BoundaryClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first
        } else {
            self.boundary
        }
    }
}

struct FailingStage;
impl StageExecutor for FailingStage {
    fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
        Err("synthetic local hold fixture".into())
    }
}

struct CountingContinuation(Arc<AtomicUsize>);
impl StageExecutor for CountingContinuation {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let log = task.workdir.join("human-authority-stage.log");
        std::fs::write(&log, b"synthetic accepted stage output\n")
            .map_err(|error| error.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: log.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }
    fn restore_session(
        &mut self,
        _session_id: &str,
        _workdir: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct HumanRevokingContinuation {
    calls: Arc<AtomicUsize>,
    db: std::path::PathBuf,
    config: ProductConfig,
    admin_session: String,
    source_instance_id: String,
    expected_scope_revision: i64,
    now: chrono::DateTime<Utc>,
}
impl StageExecutor for HumanRevokingContinuation {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        let stage_number = self.calls.fetch_add(1, Ordering::SeqCst);
        if stage_number == 0 {
            let mut store =
                ProductStore::open(&self.db, self.config.clone()).map_err(|e| e.to_string())?;
            store
                .change_product_scope(
                    &self.admin_session,
                    "reviewer",
                    &self.source_instance_id,
                    ProductCapability::ResolveReviewHold,
                    ScopeMutation::Revoke,
                    self.expected_scope_revision,
                    "human-revoke-during-stage",
                    self.now,
                )
                .map_err(|e| e.to_string())?;
            let enabled:i64=rusqlite::Connection::open(&self.db).map_err(|e|e.to_string())?.query_row("SELECT enabled FROM product_scope_grants_v1 WHERE subject_id='reviewer' AND source_instance_id=?1 AND capability='resolve_review_hold'",[&self.source_instance_id],|r|r.get(0)).map_err(|e|e.to_string())?;
            if enabled != 0 {
                return Err("human grant revoke did not commit".into());
            }
        }
        let log = task.workdir.join("accepted-before-human-revoke.log");
        std::fs::write(
            &log,
            b"stage result accepted before human authority revoke\n",
        )
        .map_err(|e| e.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: log.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }
    fn restore_session(
        &mut self,
        _session_id: &str,
        _workdir: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct StageGateExecutor {
    marker: std::path::PathBuf,
    gate: std::path::PathBuf,
    calls: std::path::PathBuf,
    count: usize,
}
impl StageExecutor for StageGateExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.count += 1;
        std::fs::write(&self.calls, self.count.to_string()).map_err(|e| e.to_string())?;
        if self.count == 1 {
            std::fs::write(&self.marker, b"STAGE_START_COMMITTED\n").map_err(|e| e.to_string())?;
            wait_file(&self.gate, std::time::Duration::from_secs(45))?;
        }
        let log = task.workdir.join("cross-process-stage-output.log");
        std::fs::write(&log, b"cross-process accepted result\n").map_err(|e| e.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: log.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }
    fn restore_session(
        &mut self,
        _session_id: &str,
        _workdir: &std::path::Path,
    ) -> Result<(), String> {
        Ok(())
    }
}

fn wait_file(path: &std::path::Path, timeout: std::time::Duration) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    while !path.exists() {
        if std::time::Instant::now() >= deadline {
            return Err("bounded test barrier timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

struct OwnedTestChild(std::process::Child);
impl Drop for OwnedTestChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn spawn_continuation_child(
    root: &std::path::Path,
    db: &std::path::Path,
    session: &str,
    operation: &str,
    mode: &str,
) -> (
    OwnedTestChild,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    use std::os::unix::fs::PermissionsExt;
    let session_file = root.join(format!("{mode}.session"));
    std::fs::write(&session_file, session).unwrap();
    std::fs::set_permissions(&session_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let marker = root.join(format!("{mode}.marker"));
    let gate = root.join(format!("{mode}.gate"));
    let calls = root.join(format!("{mode}.calls"));
    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("product_review_continuation_child")
        .arg("--nocapture")
        .env("B1_REVIEW_CHILD_MODE", mode)
        .env("B1_REVIEW_CHILD_DB", db)
        .env("B1_REVIEW_CHILD_SESSION", &session_file)
        .env("B1_REVIEW_CHILD_OPERATION", operation)
        .env("B1_REVIEW_CHILD_MARKER", &marker)
        .env("B1_REVIEW_CHILD_GATE", &gate)
        .env("B1_REVIEW_CHILD_CALLS", &calls)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    (OwnedTestChild(child), marker, gate, calls)
}

fn wait_child(mut child: OwnedTestChild, timeout: std::time::Duration) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        if std::time::Instant::now() >= deadline {
            panic!("test-owned child exceeded bounded completion deadline")
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn wait_child_marker(
    child: &mut OwnedTestChild,
    path: &std::path::Path,
    timeout: std::time::Duration,
) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if path.exists() {
            return;
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("test child exited before its barrier marker with {status}")
        }
        if std::time::Instant::now() >= deadline {
            panic!("test child failed to reach barrier marker before bounded deadline")
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

type CrashStageState = (
    i64,
    Option<String>,
    i64,
    String,
    i64,
    String,
    i64,
    i64,
    String,
    i64,
    String,
    i64,
    String,
);
type CrashStageFinalState = (
    i64,
    Option<String>,
    String,
    String,
    i64,
    i64,
    i64,
    String,
    i64,
    String,
    i64,
    String,
);

#[test]
fn product_review_continuation_child() {
    let Ok(mode) = std::env::var("B1_REVIEW_CHILD_MODE") else {
        return;
    };
    let db = std::path::PathBuf::from(std::env::var_os("B1_REVIEW_CHILD_DB").unwrap());
    let root = db.parent().unwrap();
    let session =
        std::fs::read_to_string(std::env::var_os("B1_REVIEW_CHILD_SESSION").unwrap()).unwrap();
    let operation = std::env::var("B1_REVIEW_CHILD_OPERATION").unwrap();
    let marker = std::path::PathBuf::from(std::env::var_os("B1_REVIEW_CHILD_MARKER").unwrap());
    let gate = std::path::PathBuf::from(std::env::var_os("B1_REVIEW_CHILD_GATE").unwrap());
    let calls = std::path::PathBuf::from(std::env::var_os("B1_REVIEW_CHILD_CALLS").unwrap());
    if mode == "before" || mode == "rebind_before" {
        std::fs::write(&marker, b"CHILD_READY\n").unwrap();
        wait_file(&gate, std::time::Duration::from_secs(45)).unwrap();
    }
    let config = ProductConfig {
        installation_id: "service-review-install".into(),
        database_id: "service-review-db".into(),
    };
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let executor = StageGateExecutor {
        marker: marker.clone(),
        gate: gate.clone(),
        calls,
        count: 0,
    };
    let mut host = PackReviewHost::with_product_store(
        &pack,
        executor,
        ProductStore::open(&db, config).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let result = host.continue_accepted_product_hold(&session, &operation, Utc::now());
    match mode.as_str() {
        "before" => assert!(
            result.is_err(),
            "revoked service must fail before any stage begins"
        ),
        "rebind_before" => assert!(
            result.is_err(),
            "a source rebind committed before stage start must be rejected"
        ),
        "after" => assert!(
            result.is_err(),
            "source rebind must deny the next external stage"
        ),
        "service_after" => assert!(
            result.is_err(),
            "a service revoke after stage start must deny the next stage"
        ),
        "reassign" => assert!(
            result.is_err(),
            "a reassigned worker fence must reject the old stage result"
        ),
        "crash" => {
            std::fs::write(marker, format!("UNEXPECTED_RESULT:{result:?}")).unwrap();
            return;
        }
        _ => panic!("unexpected child mode"),
    }
    std::fs::write(marker, format!("DONE:{result:?}")).unwrap();
}

fn expected_hold(db: &std::path::Path, order: &str, hold_id: &str) -> ProductHoldExpectation {
    rusqlite::Connection::open(db).unwrap().query_row(
        "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id=?1 AND hold_instance_id=?2",
        rusqlite::params![order,hold_id],
        |r|Ok(ProductHoldExpectation{lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)?}),
    ).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct ContinuationState {
    holds: Vec<(String, String, String, String)>,
    receipts: Vec<String>,
    progress: Option<(String, i64, String, i64, i64, i64)>,
    stages: Vec<(String, i64, String, String, Option<String>)>,
    sessions: Vec<(String, String, String)>,
    host_results: Vec<String>,
    budget: i64,
    s1: Vec<(i64, String, String)>,
    claim: Option<(String, Option<String>, i64, i64)>,
    backlog: Option<(String, i64)>,
}

fn continuation_state(db: &std::path::Path, order: &str, operation: &str) -> ContinuationState {
    use rusqlite::OptionalExtension;
    let conn = rusqlite::Connection::open(db).unwrap();
    let mut holds_stmt=conn.prepare("SELECT hold_instance_id,state,snapshot_json,snapshot_digest FROM product_review_holds_v1 WHERE order_id=?1 ORDER BY hold_instance_id").unwrap();
    let holds = holds_stmt
        .query_map([order], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut receipts_stmt=conn.prepare("SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE order_id=?1 ORDER BY operation_id").unwrap();
    let receipts = receipts_stmt
        .query_map([order], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    let progress=conn.query_row("SELECT phase,progress_revision,owner_id,epoch,fence,reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).ok();
    let mut stages_stmt=conn.prepare("SELECT run_id,sequence,stage_id,status,output_json FROM product_review_stages_v1 WHERE order_id=?1 ORDER BY run_id,sequence").unwrap();
    let stages = stages_stmt
        .query_map([order], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut sessions_stmt=conn.prepare("SELECT run_id,session_id,receipt_json FROM product_review_sessions_v1 WHERE order_id=?1 ORDER BY run_id,session_id").unwrap();
    let sessions = sessions_stmt
        .query_map([order], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut results_stmt=conn.prepare("SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 ORDER BY attempt,run_id,activation").unwrap();
    let host_results = results_stmt
        .query_map([order], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    let budget = conn
        .query_row(
            "SELECT used FROM product_review_budget_v1 WHERE order_id=?1",
            [order],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .unwrap_or(0);
    let mut s1_stmt = conn
        .prepare("SELECT seq,event,hash FROM ledger WHERE order_id=?1 ORDER BY seq")
        .unwrap();
    let s1 = s1_stmt
        .query_map([order], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let claim = conn
        .query_row(
            "SELECT state,owner_id,epoch,fence FROM product_work_claims_v1 WHERE order_id=?1",
            [order],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .ok();
    let backlog = conn
        .query_row(
            "SELECT state,attempts FROM product_work_backlog_v1 WHERE order_id=?1",
            [order],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    ContinuationState {
        holds,
        receipts,
        progress,
        stages,
        sessions,
        host_results,
        budget,
        s1,
        claim,
        backlog,
    }
}

fn setup_accepted_hold(
    root: &std::path::Path,
    order: &str,
) -> (
    std::path::PathBuf,
    ProductConfig,
    String,
    String,
    String,
    String,
    foundry_ledger::ProductHoldAcceptance,
    chrono::DateTime<Utc>,
    String,
) {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let db = root.join(format!("{order}.sqlite"));
    let now = Utc::now();
    let origin = "https://api.github.com";
    let source = ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: format!("{order}-source-v1"),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, "repo:1").unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:1".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let config = ProductConfig {
        installation_id: "service-review-install".into(),
        database_id: "service-review-db".into(),
    };
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash(
        "human-acceptance-owner-password",
    )
    .unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, std::slice::from_ref(&source), &[])
        .unwrap();
    let admin_session = store
        .login("owner", "human-acceptance-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    let reviewer_phc = foundry_ledger::product_identity::ProductPasswordKdf::hash(
        "human-acceptance-reviewer-password",
    )
    .unwrap();
    store
        .create_account(&admin_session, "reviewer", "approver", &reviewer_phc, now)
        .unwrap();
    let initial = store
        .change_product_scope(
            &admin_session,
            "reviewer",
            &source.source_instance_id,
            ProductCapability::ResolveReviewHold,
            ScopeMutation::Grant,
            0,
            &format!("{order}-initial-grant"),
            now,
        )
        .unwrap();
    store
        .change_product_scope(
            &admin_session,
            "reviewer",
            &source.source_instance_id,
            ProductCapability::ReadReview,
            ScopeMutation::Grant,
            initial.scope_revision,
            &format!("{order}-read-review-grant"),
            now,
        )
        .unwrap();
    let session = store
        .login("reviewer", "human-acceptance-reviewer-password", now)
        .unwrap()
        .digest()
        .to_owned();
    let actions = [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    let lease = now + Duration::hours(3);
    store
        .manage_product_service(
            &admin_session,
            "review-worker",
            &actions,
            true,
            lease,
            0,
            &format!("{order}-service-config"),
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &admin_session,
            "review-worker",
            1,
            now + Duration::hours(2),
            &format!("{order}-service-credential"),
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer().to_owned();
    let service = store
        .authenticate_product_service(&token, "github-main", origin, "repo:1", now)
        .unwrap();
    let updated = now.to_rfc3339();
    let payload = "explicit synthetic local adapter fixture";
    let version =
        foundry_ledger::product::delivery_version_digest("issue:1", &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            store.installation_id(),
            "github-main",
            origin,
            "repo:1",
            "issue:1",
            &version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo:1".into(),
        issue_id: "issue:1".into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = foundry_ledger::ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: format!("{order}-query"),
        sweep_id: "synthetic".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: updated,
    };
    store
        .persist_intake_page_for_service(
            &service,
            &cursor,
            std::slice::from_ref(&delivery),
            &[],
            now,
        )
        .unwrap();
    let admission = store
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, order, now)
        .unwrap();
    store
        .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admission, now)
        .unwrap();
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=1",
            [order],
        )
        .unwrap();
    append_waiting(&rusqlite::Connection::open(&db).unwrap(), order);
    let claim = match store
        .claim_product_work_for_service(&service, "worker-owner", now, Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("expected host fixture claim, got {other:?}"),
    };
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let intent = ExecutionIntent {
        order: OrderId(order.into()),
        attempt: Attempt(1),
        run: RunId(7),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: format!("review:{order}:1:base..result"),
        recorded_at: now,
    };
    let mut host = PackReviewHost::with_product_store(
        &pack,
        FailingStage,
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let hold = host
        .try_dispatch_product_work(&intent, &claim)
        .unwrap()
        .held
        .expect("genuine host algorithm records H1");
    drop(host);
    let mut store = ProductStore::open(&db, config.clone()).unwrap();
    let expected = expected_hold(&db, order, &hold.hold_instance_id);
    let target = hold
        .resume_targets
        .first()
        .cloned()
        .expect("frozen host provides Resume target");
    let receipt = store
        .accept_product_review_hold(
            &session,
            order,
            &hold.hold_instance_id,
            &expected,
            &format!("{order}-accept"),
            ProductHoldDecision::Resume(target),
            now,
        )
        .unwrap();
    assert!(receipt.scope_revision > initial.scope_revision);
    assert_eq!(receipt.grant_revision, initial.grant_revision);
    (
        db,
        config,
        session,
        admin_session,
        token,
        source.source_instance_id,
        receipt,
        now,
        hold.hold_instance_id,
    )
}

#[test]
fn genuine_host_resume_rejects_revoked_then_regranted_human_acceptance_authority() {
    let root = scratch();
    let (db, config, session, admin_session, token, source_id, receipt, now, hold_id) =
        setup_accepted_hold(&root, "human-aba-order");
    let mut admin = ProductStore::open(&db, config.clone()).unwrap();
    let revoked = admin
        .change_product_scope(
            &admin_session,
            "reviewer",
            &source_id,
            ProductCapability::ResolveReviewHold,
            ScopeMutation::Revoke,
            receipt.scope_revision,
            "human-revoke",
            now,
        )
        .unwrap();
    let regranted = admin
        .change_product_scope(
            &admin_session,
            "reviewer",
            &source_id,
            ProductCapability::ResolveReviewHold,
            ScopeMutation::Grant,
            revoked.scope_revision,
            "human-regrant",
            now,
        )
        .unwrap();
    assert!(
        regranted.grant_revision > receipt.grant_revision
            && regranted.scope_revision > receipt.scope_revision
    );
    let current_session = admin
        .login("reviewer", "human-acceptance-reviewer-password", now)
        .unwrap()
        .digest()
        .to_owned();
    let current_service = admin
        .authenticate_product_service(
            &token,
            "github-main",
            "https://api.github.com",
            "repo:1",
            now,
        )
        .unwrap();
    admin
        .require_product_service(&current_service, ProductServiceAction::DriveReview, now)
        .unwrap();
    let before:(String,String,String,i64,String,i64,i64,i64,String,i64,i64,i64)=rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1),(SELECT snapshot_json FROM product_review_holds_v1 WHERE hold_instance_id=?2),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT owner_id FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT epoch FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT fence FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?3 ORDER BY activation LIMIT 1),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?3),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?3),(SELECT MAX(seq) FROM ledger WHERE order_id=?3)",
        rusqlite::params![receipt.operation_id,hold_id,"human-aba-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?))).unwrap();
    drop(admin);
    let calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        CountingContinuation(calls.clone()),
        ProductStore::open(&db, config).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let error = format!(
        "{:?}",
        host.continue_accepted_product_hold(&current_session, &receipt.operation_id, now)
            .unwrap_err()
    );
    assert!(
        error.contains("authority binding changed"),
        "rejection must identify stale acceptance authority, got: {error}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "stale human receipt is rejected before any external continuation stage"
    );
    drop(host);
    let after:(String,String,String,i64,String,i64,i64,i64,String,i64,i64,i64)=rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1),(SELECT snapshot_json FROM product_review_holds_v1 WHERE hold_instance_id=?2),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT owner_id FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT epoch FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT fence FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?3 ORDER BY activation LIMIT 1),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?3),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?3),(SELECT MAX(seq) FROM ledger WHERE order_id=?3)",
        rusqlite::params![receipt.operation_id,hold_id,"human-aba-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?))).unwrap();
    assert_eq!(
        before, after,
        "only the deliberate scope revoke/regrant events may change durable state"
    );
    let (db2, config2, session2, admin2, token2, source2, receipt2, now2, _hold2) =
        setup_accepted_hold(&root, "human-revoke-only-order");
    let mut manager = ProductStore::open(&db2, config2.clone()).unwrap();
    manager
        .change_product_scope(
            &admin2,
            "reviewer",
            &source2,
            ProductCapability::ResolveReviewHold,
            ScopeMutation::Revoke,
            receipt2.scope_revision,
            "human-revoke-only",
            now2,
        )
        .unwrap();
    let calls2 = Arc::new(AtomicUsize::new(0));
    let pack2 =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut denied = PackReviewHost::with_product_store(
        &pack2,
        CountingContinuation(calls2.clone()),
        ProductStore::open(&db2, config2.clone()).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let revoke_error = format!(
        "{:?}",
        denied
            .continue_accepted_product_hold(&session2, &receipt2.operation_id, now2)
            .unwrap_err()
    );
    assert!(revoke_error.contains("scope is unavailable"));
    assert_eq!(calls2.load(Ordering::SeqCst), 0);
    drop(denied);
    manager
        .set_account_authority(&admin2, "reviewer", "approver", false, now2)
        .unwrap();
    let disabled_calls = Arc::new(AtomicUsize::new(0));
    let disabled_pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut disabled_host = PackReviewHost::with_product_store(
        &disabled_pack,
        CountingContinuation(disabled_calls.clone()),
        ProductStore::open(&db2, config2.clone()).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let disabled_error = format!(
        "{:?}",
        disabled_host
            .continue_accepted_product_hold(&session2, &receipt2.operation_id, now2)
            .unwrap_err()
    );
    assert!(disabled_error.contains("current review authority is unavailable"));
    assert_eq!(disabled_calls.load(Ordering::SeqCst), 0);
    drop(disabled_host);
    drop(manager);
    drop(token);
    drop(session);
    drop(admin_session);
    drop(token2);
    drop(session2);
    drop(admin2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn human_revoke_during_external_stage_preserves_result_and_denies_next_stage_start() {
    let root = scratch();
    let (db, config, session, admin_session, token, source_id, receipt, now, hold_id) =
        setup_accepted_hold(&root, "human-mid-stage-order");
    let before:(i64,i64,i64,i64)=rusqlite::Connection::open(&db).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT MAX(seq) FROM ledger WHERE order_id=?1)",["human-mid-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        HumanRevokingContinuation {
            calls: calls.clone(),
            db: db.clone(),
            config: config.clone(),
            admin_session,
            source_instance_id: source_id,
            expected_scope_revision: receipt.scope_revision,
            now,
        },
        ProductStore::open(&db, config.clone()).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let error = format!(
        "{:?}",
        host.continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .unwrap_err()
    );
    assert!(
        error.contains("human scope authority is unavailable")
            || error.contains("human authority binding changed"),
        "new stage must fail at the human acceptance fence: {error}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "exactly the stage authorized before revoke reached the executor"
    );
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (receipt_after,hold_state,phase,stages,complete,session_count,seq):(String,String,String,i64,i64,i64,i64)=conn.query_row(
        "SELECT (SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1),(SELECT state FROM product_review_holds_v1 WHERE hold_instance_id=?2),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?1),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?3),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?3 AND status='complete'),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?3),(SELECT MAX(seq) FROM ledger WHERE order_id=?3)",
        rusqlite::params![receipt.operation_id,hold_id,"human-mid-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).unwrap();
    assert_eq!(receipt_after, serde_json::to_string(&receipt).unwrap());
    assert_eq!(hold_state, "accepted");
    assert_eq!(
        phase, "recovery_required",
        "the accepted result is retained and no automatic replay follows an authority loss"
    );
    assert_eq!(
        (stages, complete, session_count),
        (before.0 + 1, before.1 + 1, before.2 + 1)
    );
    assert_eq!(seq, before.3, "continuation does not append an S1 decision");
    let output_json:String=conn.query_row("SELECT output_json FROM product_review_stages_v1 WHERE order_id='human-mid-stage-order' AND status='complete'",[],|r|r.get(0)).unwrap();
    let output: serde_json::Value = serde_json::from_str(&output_json).unwrap();
    assert!(std::path::Path::new(output["log_path"].as_str().unwrap()).exists());
    drop(conn);
    let reopened = ProductStore::open(&db, config).unwrap();
    let current = reopened
        .authenticate_product_service(
            &token,
            "github-main",
            "https://api.github.com",
            "repo:1",
            now,
        )
        .unwrap();
    reopened
        .require_product_service(&current, ProductServiceAction::DriveReview, now)
        .unwrap();
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_rebind_committed_before_host_stage_start_leaves_continuation_unchanged() {
    let root = scratch();
    let (db, config, session, admin, token, _source, receipt, now, _hold) =
        setup_accepted_hold(&root, "source-rebind-before-stage-order");
    let before = continuation_state(
        &db,
        "source-rebind-before-stage-order",
        &receipt.operation_id,
    );
    let (mut child, marker, gate, calls) =
        spawn_continuation_child(&root, &db, &session, &receipt.operation_id, "rebind_before");
    wait_child_marker(&mut child, &marker, std::time::Duration::from_secs(60));

    let mut manager = ProductStore::open(&db, config.clone()).unwrap();
    manager
        .configure_scoped_source(
            &admin,
            "github-main",
            RepositoryIdentity::canonicalized("acme", "github", "https://api.github.com", "repo:2")
                .unwrap(),
            "acme",
            "renamed",
            "repo:2",
            "open+foundry:make",
            true,
            1,
            "cross-process-rebind-before",
            now,
        )
        .unwrap();
    std::fs::write(&gate, b"GO\n").unwrap();
    let status = wait_child(child, std::time::Duration::from_secs(60));
    assert!(
        status.success(),
        "the exact continuation child completed its denial assertions: {status}"
    );
    let result = std::fs::read_to_string(&marker).unwrap();
    assert!(
        result.starts_with("DONE:") && (result.contains("source") || result.contains("service")),
        "the current source/service binding rejected continuation: {result}"
    );
    assert!(
        !calls.exists(),
        "rebind committed before stage reservation, so the executor was never called"
    );
    assert_eq!(
        before,
        continuation_state(
            &db,
            "source-rebind-before-stage-order",
            &receipt.operation_id
        ),
        "the denied continuation leaves receipt, hold, progress, stages, sessions, result, budget, S1 and work claims unchanged"
    );

    let conn = rusqlite::Connection::open(&db).unwrap();
    let current_source:String=conn.query_row("SELECT source_instance_id FROM product_scope_current_sources_v1 WHERE connector_id='github-main'",[],|r|r.get(0)).unwrap();
    assert_ne!(
        current_source, _source,
        "rebind creates a distinct source instance"
    );
    let inherited_service_grants:i64=conn.query_row("SELECT count(*) FROM product_service_grants_v1 WHERE service_id='review-worker' AND source_instance_id=?1 AND enabled=1",[&current_source],|r|r.get(0)).unwrap();
    assert_eq!(
        inherited_service_grants, 0,
        "new source instance receives no prior service grant"
    );
    drop(conn);
    let reopened = ProductStore::open(&db, config).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                &token,
                "github-main",
                "https://api.github.com",
                "repo:1",
                now
            )
            .is_err(),
        "old source credential cannot authenticate after reopen"
    );
    drop(reopened);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_revoke_committed_after_stage_start_preserves_exact_result_and_denies_next_stage() {
    let root = scratch();
    let (db, config, session, admin, token, _source, receipt, now, _hold) =
        setup_accepted_hold(&root, "service-revoke-after-stage-order");
    let before = continuation_state(
        &db,
        "service-revoke-after-stage-order",
        &receipt.operation_id,
    );
    let (mut child, marker, gate, calls) =
        spawn_continuation_child(&root, &db, &session, &receipt.operation_id, "service_after");
    wait_child_marker(&mut child, &marker, std::time::Duration::from_secs(60));
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"STAGE_START_COMMITTED\n",
        "the executor marker is emitted only after the new-stage reservation commits"
    );
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "1",
        "one external stage started before the service mutation"
    );

    let mut manager = ProductStore::open(&db, config.clone()).unwrap();
    manager
        .manage_product_service(
            &admin,
            "review-worker",
            &[],
            false,
            now + Duration::hours(3),
            1,
            "cross-process-revoke-after-stage",
            now,
        )
        .unwrap();
    std::fs::write(&gate, b"GO\n").unwrap();
    let status = wait_child(child, std::time::Duration::from_secs(60));
    assert!(
        status.success(),
        "the host child persisted the authorized stage and denied the following stage: {status}"
    );
    let result = std::fs::read_to_string(&marker).unwrap();
    assert!(
        result.starts_with("DONE:") && result.contains("service"),
        "the subsequent stage is denied specifically by current service authority: {result}"
    );

    let after = continuation_state(
        &db,
        "service-revoke-after-stage-order",
        &receipt.operation_id,
    );
    assert_eq!(
        after.receipts, before.receipts,
        "the immutable human acceptance receipt is preserved"
    );
    assert_eq!(
        after.holds, before.holds,
        "the accepted hold history is preserved"
    );
    assert!(
        after.host_results.starts_with(&before.host_results),
        "the original host result remains byte-for-byte unchanged"
    );
    assert_eq!(
        after.host_results.len(),
        before.host_results.len() + 1,
        "the completed continuation records one new host result without overwriting history"
    );
    assert_eq!(
        after.s1, before.s1,
        "no unintended S1 transition is appended"
    );
    assert_eq!(
        after.budget,
        before.budget + 1,
        "exactly the authorized stage reservation is charged"
    );
    assert_eq!(
        after.stages.len(),
        before.stages.len() + 1,
        "exactly one continuation stage is added"
    );
    let started = after.stages.last().unwrap();
    assert_eq!(
        started.3, "complete",
        "the already-reserved stage result is retained"
    );
    let output_json = started
        .4
        .as_deref()
        .expect("completed stage output is durable");
    let output: serde_json::Value = serde_json::from_str(output_json).unwrap();
    let log = std::path::PathBuf::from(
        output["log_path"]
            .as_str()
            .expect("normalized stage has a log path"),
    );
    assert_eq!(
        std::fs::read(log).unwrap(),
        b"cross-process accepted result\n"
    );
    assert_eq!(
        after.sessions.len(),
        before.sessions.len() + 1,
        "the session receipt for the accepted stage is saved once"
    );
    assert!(
        after.sessions.starts_with(&before.sessions),
        "prior session receipts remain byte-for-byte intact"
    );
    let saved_session = after.sessions.last().unwrap();
    assert_eq!(
        saved_session.0, started.0,
        "the accepted session is bound to the committed stage run"
    );
    assert!(
        !saved_session.2.is_empty(),
        "the accepted stage has a durable session receipt"
    );
    assert_eq!(
        after.progress.as_ref().map(|p| p.0.as_str()),
        Some("recovery_required"),
        "the denied next stage leaves the accepted decision in explicit recovery state"
    );
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "1",
        "the denied next stage never reaches the executor"
    );

    let reopened = ProductStore::open(&db, config.clone()).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                &token,
                "github-main",
                "https://api.github.com",
                "repo:1",
                now
            )
            .is_err(),
        "the revoked bearer is invalid after reopen"
    );
    drop(reopened);
    let retry_calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut retry = PackReviewHost::with_product_store(
        &pack,
        CountingContinuation(retry_calls.clone()),
        ProductStore::open(&db, config).unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    assert!(
        retry
            .continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .is_err()
    );
    assert_eq!(
        retry_calls.load(Ordering::SeqCst),
        0,
        "reopen/retry does not replay the already completed or uncertain external stage"
    );
    drop(retry);
    drop(manager);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_mutation_and_real_stage_start_are_linearized_across_processes_in_both_orders() {
    let root = scratch();
    // Mutation commits before the separate host process is released to claim
    // and reserve its first continuation stage.
    let (db, config, session, admin, token, _source, receipt, now, _hold) =
        setup_accepted_hold(&root, "service-before-stage-order");
    let before:(i64,i64,i64)=rusqlite::Connection::open(&db).unwrap().query_row("SELECT COALESCE((SELECT used FROM product_review_budget_v1 WHERE order_id=?1),0),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",["service-before-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let before_state = continuation_state(&db, "service-before-stage-order", &receipt.operation_id);
    let (mut child, marker, gate, calls) =
        spawn_continuation_child(&root, &db, &session, &receipt.operation_id, "before");
    wait_child_marker(&mut child, &marker, std::time::Duration::from_secs(60));
    let mut manager = ProductStore::open(&db, config.clone()).unwrap();
    manager
        .manage_product_service(
            &admin,
            "review-worker",
            &[],
            false,
            now + Duration::hours(3),
            1,
            "cross-process-revoke-before",
            now,
        )
        .unwrap();
    std::fs::write(&gate, b"GO\n").unwrap();
    let status = wait_child(child, std::time::Duration::from_secs(60));
    assert!(
        status.success(),
        "child selected the exact host scenario and observed the intended service denial: {status}"
    );
    let child_result = std::fs::read_to_string(&marker).unwrap();
    assert!(
        child_result.contains("service") || child_result.contains("source"),
        "child returned intended service/source authority error: {child_result}"
    );
    assert!(
        !calls.exists(),
        "mutation-before-start produces zero external calls"
    );
    let after:(i64,i64,i64)=rusqlite::Connection::open(&db).unwrap().query_row("SELECT COALESCE((SELECT used FROM product_review_budget_v1 WHERE order_id=?1),0),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",["service-before-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        before, after,
        "no partial budget/stage/session write occurred"
    );
    assert_eq!(
        before_state,
        continuation_state(&db, "service-before-stage-order", &receipt.operation_id),
        "revoke-before leaves hold receipt, progress, stages, sessions, result, S1, budget and work claim unchanged"
    );
    drop(manager);

    // In the opposite order, the child reports only from StageExecutor, which
    // is called after its stage reservation transaction committed. A real
    // Owner rebinds the connector in another process; this result is retained
    // and the next stage reservation is denied.
    let (db2, config2, session2, admin2, token2, _source2, receipt2, now2, _hold2) =
        setup_accepted_hold(&root, "source-rebind-after-stage-order");
    let before2:(i64,i64,i64)=rusqlite::Connection::open(&db2).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT COALESCE(used,0) FROM product_review_budget_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1)",["source-rebind-after-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    let (mut child2, marker2, gate2, calls2) =
        spawn_continuation_child(&root, &db2, &session2, &receipt2.operation_id, "after");
    wait_child_marker(&mut child2, &marker2, std::time::Duration::from_secs(60));
    assert!(
        calls2.exists(),
        "stage start marker follows durable stage reservation"
    );
    let mut manager2 = ProductStore::open(&db2, config2.clone()).unwrap();
    manager2
        .configure_scoped_source(
            &admin2,
            "github-main",
            RepositoryIdentity::canonicalized("acme", "github", "https://api.github.com", "repo:2")
                .unwrap(),
            "acme",
            "renamed",
            "repo:2",
            "open+foundry:make",
            true,
            1,
            "cross-process-rebind-after",
            now2,
        )
        .unwrap();
    std::fs::write(&gate2, b"GO\n").unwrap();
    let status = wait_child(child2, std::time::Duration::from_secs(60));
    assert!(
        status.success(),
        "child saved accepted stage cleanup and rejected the following stage: {status}"
    );
    let result = std::fs::read_to_string(&marker2).unwrap();
    assert!(result.starts_with("DONE:"));
    let conn = rusqlite::Connection::open(&db2).unwrap();
    let stage:(i64,i64,i64,i64)=conn.query_row("SELECT count(*),sum(status='complete'),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT used FROM product_review_budget_v1 WHERE order_id=?1) FROM product_review_stages_v1 WHERE order_id=?1",["source-rebind-after-stage-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(
        stage.0,
        before2.0 + 1,
        "only one new continuation stage was reserved"
    );
    assert_eq!(stage.1, 1, "the committed in-flight result was saved");
    assert_eq!(stage.2, before2.2 + 1);
    assert_eq!(stage.3, before2.1 + 1);
    assert_eq!(std::fs::read_to_string(&calls2).unwrap(), "1");
    drop(conn);
    let reopened = ProductStore::open(&db2, config2).unwrap();
    assert!(
        reopened
            .authenticate_product_service(
                &token2,
                "github-main",
                "https://api.github.com",
                "repo:1",
                now2
            )
            .is_err(),
        "old service context cannot authenticate after source rebind"
    );
    drop(reopened);
    drop(manager2);
    drop(token);
    std::fs::remove_dir_all(root).unwrap();

    // Reassign the durable work owner while the accepted external call is
    // paused. The old worker may not persist its result or session under the
    // newer claim's fence.
    let root3 = scratch();
    let (db3, config3, session3, admin3, token3, _source3, receipt3, now3, _hold3) =
        setup_accepted_hold(&root3, "worker-reassigned-order");
    let before3:(i64,i64,i64,i64)=rusqlite::Connection::open(&db3).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT used FROM product_review_budget_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT fence FROM product_work_claims_v1 WHERE order_id=?1)",["worker-reassigned-order"],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    let (mut old_child, old_marker, old_gate, old_calls) =
        spawn_continuation_child(&root3, &db3, &session3, &receipt3.operation_id, "reassign");
    wait_child_marker(
        &mut old_child,
        &old_marker,
        std::time::Duration::from_secs(60),
    );
    let mut manager3 = ProductStore::open(&db3, config3.clone()).unwrap();
    let renewed = manager3
        .authenticate_product_service(
            &token3,
            "github-main",
            "https://api.github.com",
            "repo:1",
            now3 + Duration::minutes(6),
        )
        .unwrap();
    let replacement = match manager3
        .claim_product_work_for_service(
            &renewed,
            "replacement-worker",
            now3 + Duration::minutes(6),
            Duration::minutes(5),
        )
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("expired accepted work claim was not reallocated: {other:?}"),
    };
    let replacement_fence = replacement.review_binding().fence;
    assert!(
        replacement_fence > before3.3,
        "reallocation advances the durable fence"
    );
    std::fs::write(&old_gate, b"RELEASE\n").unwrap();
    let old_status = wait_child(old_child, std::time::Duration::from_secs(60));
    assert!(
        old_status.success(),
        "old child reports stale result rejection"
    );
    assert!(
        std::fs::read_to_string(&old_marker)
            .unwrap()
            .starts_with("DONE:")
    );
    assert_eq!(std::fs::read_to_string(&old_calls).unwrap(), "1");
    let final3:(i64,Option<String>,i64,String,i64,i64,String,String)=rusqlite::Connection::open(&db3).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT output_json FROM product_review_stages_v1 WHERE order_id=?1 AND status='accepted'),(SELECT used FROM product_review_budget_v1 WHERE order_id=?1),(SELECT state FROM product_work_claims_v1 WHERE order_id=?1),(SELECT fence FROM product_work_claims_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT state FROM product_work_backlog_v1 WHERE order_id=?1)",rusqlite::params!["worker-reassigned-order",receipt3.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).unwrap();
    assert_eq!(final3.0, before3.0 + 1);
    assert!(final3.1.is_none());
    assert_eq!(final3.2, before3.1 + 1);
    assert_eq!(final3.3, "claimed");
    assert_eq!(final3.4, replacement_fence);
    assert_eq!(final3.5, before3.2);
    assert_eq!(final3.6, "recovery_required");
    assert_eq!(final3.7, "claimed");
    drop(manager3);
    let _ = admin3;
    std::fs::remove_dir_all(root3).unwrap();
}

#[test]
fn committed_stage_crash_reopens_as_recovery_required_without_reexecution() {
    use std::os::unix::process::ExitStatusExt;
    let root = scratch();
    let (db, config, session, admin, _token, _source, receipt, now, _hold_id) =
        setup_accepted_hold(&root, "crash-stage-order");
    let before:(i64,i64,String,i64,i64,String,i64,String,i64) = rusqlite::Connection::open(&db).unwrap().query_row(
        "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT COALESCE(used,0) FROM product_review_budget_v1 WHERE order_id=?1),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT state FROM product_work_claims_v1 WHERE order_id=?1),(SELECT fence FROM product_work_claims_v1 WHERE order_id=?1),(SELECT state FROM product_work_backlog_v1 WHERE order_id=?1),(SELECT attempts FROM product_work_backlog_v1 WHERE order_id=?1)",
        rusqlite::params!["crash-stage-order",receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).unwrap();
    let original_result:String=rusqlite::Connection::open(&db).unwrap().query_row("SELECT result_json FROM product_review_host_results_v1 WHERE order_id='crash-stage-order' ORDER BY activation LIMIT 1",[],|r|r.get(0)).unwrap();
    let (mut child, marker, _gate, calls) =
        spawn_continuation_child(&root, &db, &session, &receipt.operation_id, "crash");
    wait_child_marker(&mut child, &marker, std::time::Duration::from_secs(60));
    assert_eq!(std::fs::read(&marker).unwrap(), b"STAGE_START_COMMITTED\n");
    assert_eq!(std::fs::read_to_string(&calls).unwrap(), "1");
    child.0.kill().unwrap();
    let status = child.0.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "only the coordinated test child is SIGKILLed after the accepted reservation marker"
    );
    let conn = rusqlite::Connection::open(&db).unwrap();
    let (accepted,output,stage_count,receipt_json,used,phase,progress_revision,sessions,claim_state,claim_fence,backlog_state,attempts,result_after):CrashStageState=conn.query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='accepted'),(SELECT output_json FROM product_review_stages_v1 WHERE order_id=?1 AND status='accepted'),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?2),(SELECT used FROM product_review_budget_v1 WHERE order_id=?1),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT state FROM product_work_claims_v1 WHERE order_id=?1),(SELECT fence FROM product_work_claims_v1 WHERE order_id=?1),(SELECT state FROM product_work_backlog_v1 WHERE order_id=?1),(SELECT attempts FROM product_work_backlog_v1 WHERE order_id=?1),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 ORDER BY activation LIMIT 1)",rusqlite::params!["crash-stage-order",receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?))).unwrap();
    assert_eq!(
        (accepted, output),
        (1, None),
        "one committed stage reservation has no persisted result"
    );
    assert_eq!(stage_count, before.0 + 1);
    assert_eq!(used, before.1 + 1);
    assert_eq!(phase, "continuing");
    assert_eq!(progress_revision, before.3 + 1);
    assert_eq!(sessions, before.4);
    assert_eq!(
        (claim_state, claim_fence, backlog_state, attempts),
        (before.5.clone(), before.6, before.7.clone(), before.8)
    );
    assert_eq!(result_after, original_result);
    assert_eq!(receipt_json, serde_json::to_string(&receipt).unwrap());
    drop(conn);
    let reopened = ProductStore::open(&db, config.clone()).unwrap();
    let recovery_calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        CountingContinuation(recovery_calls.clone()),
        reopened,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    assert!(
        host.continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .is_err(),
        "an uncertain committed stage requires recovery, never blind redispatch"
    );
    drop(host);
    let mut verify = ProductStore::open(&db, config).unwrap();
    let progress = verify
        .product_review_progress(&session, &receipt.operation_id, now)
        .unwrap();
    assert!(progress.recovery_required);
    assert_eq!(progress.phase, "recovery_required");
    assert_eq!(
        recovery_calls.load(Ordering::SeqCst),
        0,
        "reopen/retry does not repeat the uncertain external call"
    );
    let retry_calls = Arc::new(AtomicUsize::new(0));
    let retry_pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut retry = PackReviewHost::with_product_store(
        &retry_pack,
        CountingContinuation(retry_calls.clone()),
        ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "service-review-install".into(),
                database_id: "service-review-db".into(),
            },
        )
        .unwrap(),
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    assert!(
        retry
            .continue_accepted_product_hold(&session, &receipt.operation_id, now)
            .is_err()
    );
    assert_eq!(
        retry_calls.load(Ordering::SeqCst),
        0,
        "subsequent retry remains fenced from the ambiguous stage"
    );
    drop(retry);
    let final_state:CrashStageFinalState = rusqlite::Connection::open(&db).unwrap().query_row("SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),(SELECT output_json FROM product_review_stages_v1 WHERE order_id=?1 AND status='accepted'),(SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?2),(SELECT COALESCE(used,0) FROM product_review_budget_v1 WHERE order_id=?1),(SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),(SELECT progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?2),(SELECT state FROM product_work_claims_v1 WHERE order_id=?1),(SELECT fence FROM product_work_claims_v1 WHERE order_id=?1),(SELECT state FROM product_work_backlog_v1 WHERE order_id=?1),(SELECT attempts FROM product_work_backlog_v1 WHERE order_id=?1),(SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 ORDER BY activation LIMIT 1)",rusqlite::params!["crash-stage-order",receipt.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?))).unwrap();
    assert_eq!(final_state.0, before.0 + 1);
    assert!(final_state.1.is_none());
    assert_eq!(final_state.2, "recovery_required");
    assert_eq!(final_state.3, serde_json::to_string(&receipt).unwrap());
    assert_eq!(final_state.4, before.1 + 1);
    assert_eq!(final_state.5, before.4);
    assert_eq!(final_state.6, before.3 + 2);
    assert_eq!(
        (final_state.7, final_state.8, final_state.9, final_state.10),
        (before.5, before.6, before.7, before.8)
    );
    assert_eq!(final_state.11, original_result);
    drop(verify);
    let _ = admin;
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_claim_drives_shared_host_and_revocation_preserves_accepted_stage_but_blocks_next() {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let root = scratch();
    let db = root.join("product.sqlite");
    let now = Utc::now();
    let origin = "https://api.github.com";
    let source = ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: "github-source-v1".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, "repo:1").unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:1".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let mut store = ProductStore::open(&db, config()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("service-review-owner-password")
            .unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, std::slice::from_ref(&source), &[])
        .unwrap();
    let owner = store
        .login("owner", "service-review-owner-password", now)
        .unwrap()
        .digest()
        .to_owned();
    let actions = [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    let lease = now + Duration::hours(3);
    store
        .manage_product_service(
            &owner,
            "review-worker",
            &actions,
            true,
            lease,
            0,
            "service-config",
            now,
        )
        .unwrap();
    let credential = store
        .issue_product_service_credential(
            &owner,
            "review-worker",
            1,
            now + Duration::hours(2),
            "service-credential",
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer().to_owned();
    let service = store
        .authenticate_product_service(&token, "github-main", origin, "repo:1", now)
        .unwrap();
    let updated = now.to_rfc3339();
    let payload = "explicit synthetic test adapter input";
    let version =
        foundry_ledger::product::delivery_version_digest("issue:1", &updated, "dep-bump", payload);
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "service-review-install",
            "github-main",
            origin,
            "repo:1",
            "issue:1",
            &version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo:1".into(),
        issue_id: "issue:1".into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = foundry_ledger::ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "service-test-query".into(),
        sweep_id: "synthetic".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: updated,
    };
    store
        .persist_intake_page_for_service(
            &service,
            &cursor,
            std::slice::from_ref(&delivery),
            &[],
            now,
        )
        .unwrap();
    let admission = store
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, "service-order", now)
        .unwrap();
    store
        .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admission, now)
        .unwrap();
    // This is a test-only explicit synthetic adapter selection. Product
    // GitHub intake still leaves unsupported task grammars waiting_unsupported.
    rusqlite::Connection::open(&db).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='service-order' AND lifecycle=1",[]).unwrap();
    append_waiting(&rusqlite::Connection::open(&db).unwrap(), "service-order");
    let claim = match store
        .claim_product_work_for_service(&service, "worker-owner", now, Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("expected explicit synthetic service claim, got {other:?}"),
    };
    store.validate_product_work_claim(&claim, now).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        RevokingExecutor {
            db: db.clone(),
            session: owner.clone(),
            lease,
            calls: calls.clone(),
        },
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("service-order".into()),
        attempt: Attempt(1),
        run: RunId(1),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:service-order:1:base..result".into(),
        recorded_at: now,
    };
    assert!(
        host.try_dispatch_product_work(&intent, &claim).is_err(),
        "mid-stage service revoke prevents the next new-work reservation"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "accepted first stage is not repeated"
    );
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let stage:(String,Option<String>)=conn.query_row("SELECT status,output_json FROM product_review_stages_v1 WHERE order_id='service-order' ORDER BY sequence LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(stage.0, "complete");
    let output: serde_json::Value = serde_json::from_str(stage.1.as_deref().unwrap()).unwrap();
    let saved = std::path::PathBuf::from(output["log_path"].as_str().unwrap());
    assert!(
        saved.exists(),
        "the accepted normalized stage result remains on disk"
    );
    let receipts: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_review_work_bindings_v1 WHERE order_id='service-order'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(receipts, 1);
    drop(conn);
    let reopened = ProductStore::open(&db, config()).unwrap();
    assert!(
        reopened
            .authenticate_product_service(&token, "github-main", origin, "repo:1", Utc::now())
            .is_err()
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn service_expiry_at_stage_transaction_boundary_prevents_budget_and_external_call() {
    use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
    let root = scratch();
    let db = root.join("product.sqlite");
    let origin = "https://api.github.com";
    let source = ScopeSourceManifest {
        connector_id: "github-main".into(),
        source_instance_id: "clock-source-v1".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized("acme", "github", origin, "repo:1").unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:1".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let mut store = ProductStore::open(&db, config()).unwrap();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("clock-owner-password").unwrap();
    store
        .bootstrap_owner_scoped("owner", &phc, std::slice::from_ref(&source), &[])
        .unwrap();
    let owner = store
        .login("owner", "clock-owner-password", Utc::now())
        .unwrap()
        .digest()
        .to_owned();
    let now = Utc::now();
    let lease = now + Duration::minutes(10);
    let actions = [
        ProductServiceAction::IntakeRead,
        ProductServiceAction::AdmitDelivery,
        ProductServiceAction::AckDelivery,
        ProductServiceAction::DriveReview,
    ]
    .into_iter()
    .map(|action| ProductServiceGrant {
        source_instance_id: source.source_instance_id.clone(),
        action,
    })
    .collect::<Vec<_>>();
    store
        .manage_product_service(
            &owner,
            "review-worker",
            &actions,
            true,
            lease,
            0,
            "clock-config",
            now,
        )
        .unwrap();
    let expiry = now + Duration::minutes(2);
    let credential = store
        .issue_product_service_credential(
            &owner,
            "review-worker",
            1,
            expiry,
            "clock-credential",
            now,
        )
        .unwrap();
    let token = credential.expose_once_to_file_writer().to_owned();
    let service = store
        .authenticate_product_service(&token, "github-main", origin, "repo:1", now)
        .unwrap();
    let updated = now.to_rfc3339();
    let payload = "explicit synthetic test adapter input";
    let version = foundry_ledger::product::delivery_version_digest(
        "issue:clock",
        &updated,
        "dep-bump",
        payload,
    );
    let delivery = foundry_ledger::ProductDelivery {
        delivery_id: foundry_ledger::product::github_delivery_id(
            "service-review-install",
            "github-main",
            origin,
            "repo:1",
            "issue:clock",
            &version,
        ),
        connector_id: "github-main".into(),
        repository_id: "repo:1".into(),
        issue_id: "issue:clock".into(),
        observed_updated_at: updated.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = foundry_ledger::ProductPollCursor {
        connector_id: "github-main".into(),
        query_fingerprint: "clock-query".into(),
        sweep_id: "clock-sweep".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at: updated,
    };
    store
        .persist_intake_page_for_service(
            &service,
            &cursor,
            std::slice::from_ref(&delivery),
            &[],
            now,
        )
        .unwrap();
    let admitted = store
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, "clock-order", now)
        .unwrap();
    store
        .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admitted, now)
        .unwrap();
    rusqlite::Connection::open(&db).unwrap().execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id='clock-order' AND lifecycle=1",[]).unwrap();
    append_waiting(&rusqlite::Connection::open(&db).unwrap(), "clock-order");
    let claim = match store
        .claim_product_work_for_service(&service, "clock-worker", now, Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(c) => c,
        other => panic!("expected service claim, got {other:?}"),
    };
    drop(store);
    // Admission takes the first instant; the next protected transaction is
    // exactly at expires_at and must refuse the external stage.
    let clock = Arc::new(BoundaryClock {
        first: now,
        boundary: expiry,
        calls: AtomicUsize::new(0),
    });
    let store = ProductStore::open_with_clock(&db, config(), clock.clone()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        RevokingExecutor {
            db: db.clone(),
            session: owner,
            lease,
            calls: calls.clone(),
        },
        store,
        root.join("artifacts"),
        root.join("sessions"),
        root.join("claims"),
    )
    .unwrap();
    let intent = ExecutionIntent {
        order: OrderId("clock-order".into()),
        attempt: Attempt(1),
        run: RunId(1),
        activation: 1,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: "review:clock-order:1:base..result".into(),
        recorded_at: now,
    };
    assert!(
        host.try_dispatch_product_work(&intent, &claim).is_err(),
        "expiry is exclusive at the stage-start transaction boundary"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "expired service authority prevents the external call"
    );
    drop(host);
    let conn = rusqlite::Connection::open(&db).unwrap();
    let state:(i64,i64,i64)=conn.query_row("SELECT COALESCE((SELECT used FROM product_review_budget_v1 WHERE order_id='clock-order'),0),(SELECT count(*) FROM product_review_stages_v1 WHERE order_id='clock-order'),(SELECT count(*) FROM product_review_host_results_v1 WHERE order_id='clock-order')",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        state,
        (0, 0, 0),
        "stage expiry leaves budget, stage, and result untouched"
    );
    drop(conn);
    drop(clock);
    std::fs::remove_dir_all(root).unwrap();
}
