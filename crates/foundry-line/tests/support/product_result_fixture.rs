//! Genuine, test-only ProductStore → work claim → PackReviewHost fixture.
//!
//! This deliberately stops at G-pre.  Callers finalize through the real
//! ProductStore finalizer, so process tests and the original H assertions use
//! the same producer path.

use chrono::{Duration, Utc};
use foundry_exec::{Attempt, DispatchHost, Epoch, ExecutionIntent, Generation, OrderId, RunId};
use foundry_ledger::{
    P2ReviewPreparation, ProductApprovalIntent, ProductCapability, ProductConfig, ProductDelivery,
    ProductPollCursor, ProductResultExpectation, ProductServiceAction, ProductServiceContext,
    ProductServiceGrant, ProductStore, RepositoryIdentity, ScopeMutation, ScopeSourceManifest,
};
use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
use std::sync::atomic::AtomicI64;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Test-only synchronization point inside a real StageExecutor call. The
/// enclosing host has already admitted the run, claimed the work and charged
/// the stage before this gate is reached.
#[derive(Default)]
pub struct StagePause {
    state: Mutex<StagePauseState>,
    changed: Condvar,
}

#[derive(Default)]
struct StagePauseState {
    stage: Option<String>,
    reached: bool,
    released: bool,
}

impl StagePause {
    pub fn arm(&self, stage: impl Into<String>) {
        let mut state = self.state.lock().unwrap();
        assert!(!state.reached, "cannot re-arm a reached stage pause");
        state.stage = Some(stage.into());
        state.released = false;
    }

    fn stop_if_armed(&self, stage_id: &str) {
        let mut state = self.state.lock().unwrap();
        if !state
            .stage
            .as_deref()
            .is_some_and(|requested| requested == "*" || stage_id.starts_with(requested))
        {
            return;
        }
        state.reached = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).unwrap();
        }
        state.stage = None;
        state.reached = false;
        state.released = false;
        self.changed.notify_all();
    }

    pub fn wait_until_reached(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.reached {
            let (next, timeout) = self
                .changed
                .wait_timeout(state, std::time::Duration::from_secs(30))
                .unwrap();
            state = next;
            assert!(!timeout.timed_out(), "armed stage pause was never reached");
        }
    }

    pub fn release(&self) {
        let mut state = self.state.lock().unwrap();
        assert!(state.reached, "stage pause must be reached before release");
        state.released = true;
        self.changed.notify_all();
    }
}

pub struct FixtureClock(pub chrono::DateTime<Utc>);
impl foundry_ledger::ProductClock for FixtureClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.0
    }
}

#[derive(Clone)]
pub struct AdjustableFixtureClock(pub Arc<AtomicI64>);
impl AdjustableFixtureClock {
    pub fn new(now: chrono::DateTime<Utc>) -> Self {
        Self(Arc::new(AtomicI64::new(now.timestamp())))
    }
    pub fn set(&self, now: chrono::DateTime<Utc>) {
        self.0.store(now.timestamp(), Ordering::SeqCst);
    }
}
impl foundry_ledger::ProductClock for AdjustableFixtureClock {
    fn now(&self) -> chrono::DateTime<Utc> {
        chrono::DateTime::from_timestamp(self.0.load(Ordering::SeqCst), 0)
            .expect("test clock timestamp is valid")
    }
}

pub fn scratch() -> std::path::PathBuf {
    let base = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .expect("TMPDIR must point at the configured private scratch directory");
    assert_ne!(base, std::path::Path::new("/tmp"));
    let path = base.join(format!(
        "foundry-b1-product-result-genuine-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).expect("create private scratch fixture");
    path
}

#[derive(Clone)]
pub struct FixtureIdentity {
    pub product: ProductConfig,
    pub connector_id: String,
    pub api_origin: String,
    pub repository_id: String,
    pub repository_owner: String,
    pub repository_name: String,
    pub source_instance_id: String,
    pub order_id: String,
    pub issue_id: String,
    pub service_id: String,
    pub worker_id: String,
    pub script: ReviewScript,
    pub bump_service_revision: bool,
    pub preparation: P2ReviewPreparation,
    pub service_credential_minutes: i64,
    pub adjustable_host_clock: bool,
    pub stale_admission_preparation_probe: bool,
    pub g05_retry_ledger: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // Shared by the H/HX/S/C path-included test fixture.
pub enum ReviewScript {
    #[default]
    Passing,
    InitialRedrive,
    ResumePass,
    ResumeCommitPass,
    DirectCommitPass,
}

impl FixtureIdentity {
    pub fn new(
        product: ProductConfig,
        source_instance_id: impl Into<String>,
        order_id: impl Into<String>,
    ) -> Self {
        let order_id = order_id.into();
        Self {
            product,
            connector_id: "github-main".into(),
            api_origin: "https://api.github.com".into(),
            repository_id: "repo:1".into(),
            repository_owner: "acme".into(),
            repository_name: "repo".into(),
            source_instance_id: source_instance_id.into(),
            issue_id: format!("{}-issue", order_id.replace(':', "-")),
            service_id: format!("{}-service", order_id.replace(':', "-")),
            worker_id: format!("{}-worker", order_id.replace(':', "-")),
            script: ReviewScript::Passing,
            bump_service_revision: false,
            preparation: P2ReviewPreparation::Waiting,
            service_credential_minutes: 120,
            adjustable_host_clock: false,
            stale_admission_preparation_probe: false,
            g05_retry_ledger: false,
            order_id,
        }
    }

    #[allow(dead_code)]
    pub fn with_script(mut self, script: ReviewScript) -> Self {
        self.script = script;
        self
    }

    #[allow(dead_code)]
    pub fn with_unequal_service_and_connector_revisions(mut self) -> Self {
        self.bump_service_revision = true;
        self
    }

    #[allow(dead_code)]
    pub fn with_service_credential_minutes(mut self, minutes: i64) -> Self {
        assert!(minutes > 0);
        self.service_credential_minutes = minutes;
        self
    }

    #[allow(dead_code)]
    pub fn with_adjustable_host_clock(mut self) -> Self {
        self.adjustable_host_clock = true;
        self
    }

    #[allow(dead_code)]
    pub fn with_preparation(mut self, preparation: P2ReviewPreparation) -> Self {
        self.preparation = preparation;
        self
    }

    pub fn with_stale_admission_preparation_probe(mut self) -> Self {
        self.stale_admission_preparation_probe = true;
        self
    }

    /// G05 (rubric v1 line 132): replace the passing three-transition P2
    /// preparation with a chain-consistent retry history — DieFailed(1)
    /// advances the domain attempt, DieStarted re-claims 2, DieSucceeded(2)
    /// completes — so the replay-derived actual attempt is 2 while every
    /// stored copy (event repr, canonical JSON, chain hash) stays mutually
    /// consistent. An attempt-1 genuine caller coordinate must then be
    /// rejected by the finalizer's actual-attempt comparison itself, not by
    /// stored-set contamination or hash-chain corruption evidence.
    /// Requires Queued preparation (backlog CAS only, no appends).
    pub fn with_g05_retry_ledger(mut self) -> Self {
        self.g05_retry_ledger = true;
        self
    }
}

pub struct PassingExecutor {
    pub executions: std::sync::Arc<AtomicUsize>,
    pub restores: std::sync::Arc<AtomicUsize>,
    pub observations: Arc<Mutex<Vec<TaskObservation>>>,
    pub script: ReviewScript,
    pub pause: Arc<StagePause>,
    triage: usize,
}

#[derive(Clone, Debug)]
pub struct TaskObservation {
    pub stage_id: String,
    pub session_id: String,
    pub artifact_commit: String,
    pub inputs: std::collections::BTreeMap<String, String>,
}

impl StageExecutor for PassingExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        self.pause.stop_if_armed(&task.stage_id);
        self.observations.lock().unwrap().push(TaskObservation {
            stage_id: task.stage_id.clone(),
            session_id: task.session_id.clone(),
            artifact_commit: task.artifact_commit.clone(),
            inputs: task.inputs.clone(),
        });
        if self.script == ReviewScript::ResumePass && task.stage_id == "replan_full-g1" {
            let plan = task
                .inputs
                .get("plan_full")
                .expect("resume generation receives the plan_full input");
            assert_eq!(
                plan, "[]",
                "the resume generation receives the frozen serialized plan_full input"
            );
        }
        if self.script == ReviewScript::ResumePass && task.stage_id.starts_with("fixdie-g") {
            let findings = task
                .inputs
                .get("findings")
                .expect("fix input receives the triage findings");
            let findings: Vec<foundry_exec::ReviewRedriveArtifact> =
                serde_json::from_str(findings).unwrap();
            assert_eq!(
                findings
                    .iter()
                    .map(|artifact| artifact.stage_id.as_str())
                    .collect::<Vec<_>>(),
                ["triage"],
                "the fix input contains the single triage finding carried by this run"
            );
        }
        let output = task.workdir.join("output.json");
        let (verdict_category, findings, produced_commit) =
            match (self.script, task.stage_id.as_str()) {
                (ReviewScript::InitialRedrive, "triage") => (Some("patch".to_owned()), true, None),
                (ReviewScript::InitialRedrive, "fixdie-g1") => (None, false, Some("f".repeat(40))),
                (
                    ReviewScript::ResumePass
                    | ReviewScript::ResumeCommitPass
                    | ReviewScript::DirectCommitPass,
                    "triage",
                ) => {
                    self.triage += 1;
                    match self.triage {
                        1 => (Some("intent_gap".to_owned()), true, None),
                        2 => (Some("patch".to_owned()), true, None),
                        _ if self.script == ReviewScript::ResumePass => {
                            (Some("pass".to_owned()), false, None)
                        }
                        _ => (Some("intent_gap".to_owned()), true, None),
                    }
                }
                (ReviewScript::ResumePass | ReviewScript::ResumeCommitPass, "replan_full-g1") => {
                    (None, false, Some("c".repeat(40)))
                }
                (ReviewScript::ResumePass | ReviewScript::ResumeCommitPass, "fixdie-g2") => {
                    (None, false, Some("d".repeat(40)))
                }
                _ => (None, false, None),
            };
        let output_json = if matches!(self.script, ReviewScript::Passing) {
            serde_json::json!({"category": null, "categories": [], "findings": false, "stage": task.stage_id}).to_string()
        } else {
            serde_json::json!({"category": verdict_category, "categories": [], "findings": findings, "stage": task.stage_id, "inspected_commit": task.artifact_commit, "produced_commit": produced_commit}).to_string()
        };
        std::fs::write(&output, output_json).map_err(|error| error.to_string())?;
        Ok(StageOutput {
            verdict_category,
            findings,
            log_path: output.to_string_lossy().into_owned(),
            inspected_commit: Some(task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit,
        })
    }

    fn restore_session(
        &mut self,
        session_id: &str,
        _workdir: &std::path::Path,
    ) -> Result<(), String> {
        self.restores.fetch_add(1, Ordering::SeqCst);
        self.pause.stop_if_armed(&format!("restore:{session_id}"));
        Ok(())
    }
}

pub struct GenuineResultFixture {
    pub root: std::path::PathBuf,
    pub db_path: std::path::PathBuf,
    /// Snapshot after durable admission/ack but before P2 preparation or claim.
    /// Used only for independent fresh-work preparation negatives.
    pub preclaim_db_path: std::path::PathBuf,
    pub artifact_root: std::path::PathBuf,
    pub session_root: std::path::PathBuf,
    pub claim_root: std::path::PathBuf,
    pub service_bearer_path: std::path::PathBuf,
    pub human_session_bearer_path: std::path::PathBuf,
    pub delivery_id: String,
    #[allow(dead_code)]
    pub config: ProductConfig,
    pub identity: FixtureIdentity,
    pub now: chrono::DateTime<Utc>,
    pub service: ProductServiceContext,
    pub service_token: String,
    pub human_session_digest: String,
    #[allow(dead_code)]
    pub owner_session_digest: String,
    pub approval: ProductApprovalIntent,
    #[allow(dead_code)] // Consumed by the sibling HX target; H only checks human retries.
    pub approval_operation_id: Option<String>,
    pub admission: foundry_ledger::AdmissionReceipt,
    pub intent: ExecutionIntent,
    pub claim: foundry_ledger::ProductWorkClaim,
    pub host: Option<PackReviewHost>,
    pub result_store: Option<ProductStore>,
    /// Present only when the real host dispatch completed. Failed dispatch
    /// fixtures deliberately carry no synthetic HostResult.
    pub terminal: Option<foundry_exec::HostResult>,
    pub dispatch_error: Option<String>,
    pub expected: ProductResultExpectation,
    pub calls: std::sync::Arc<AtomicUsize>,
    pub restores: std::sync::Arc<AtomicUsize>,
    pub artifact_root_device_inode: (u64, u64),
    pub stage_pause: Arc<StagePause>,
    pub host_clock: Option<AdjustableFixtureClock>,
    pub task_observations: Arc<Mutex<Vec<TaskObservation>>>,
}

impl GenuineResultFixture {
    pub fn assert_genuine_private_handoff(&self) {
        assert_eq!(self.admission.order_id, self.identity.order_id);
        assert_eq!(self.intent.order.0, self.identity.order_id);
        assert!(!self.delivery_id.is_empty());
        assert!(!self.identity.worker_id.is_empty());
        assert!(!self.service_token.is_empty());
        assert!(
            self.service_token
                .as_bytes()
                .eq(std::fs::read(&self.service_bearer_path)
                    .expect("service bearer file")
                    .as_slice())
        );
        assert!(self.human_session_bearer_path.is_file());
        assert!(!self.human_session_digest.is_empty());
        assert!(self.session_root.is_absolute());
        assert!(self.claim_root.is_absolute());
        if let Some(terminal) = self.terminal.as_ref() {
            assert!(
                foundry_ledger::product_host_result_digest(terminal)
                    .map(|digest| digest == self.expected.host_result_digest)
                    .unwrap_or(false)
            );
            assert_eq!(self.calls.load(Ordering::SeqCst), 4);
            assert_eq!(self.restores.load(Ordering::SeqCst), 0);
            assert_eq!(self.host.as_ref().unwrap().dispatch_count(), 1);
        }
        assert_eq!(self.approval.order_id, self.identity.order_id);
        assert_eq!(
            directory_identity(&self.artifact_root),
            self.artifact_root_device_inode
        );
    }
}

fn product_configured(config: &ProductConfig) -> ProductConfig {
    config.clone()
}

/// G05 test-support constructor (rubric v1 line 132): append a CHAIN-CONSISTENT
/// retry history to a Queued-prepared order. Every transition obeys
/// foundry-domain `Order::transition` exactly — DieFailed(1) bumps the domain
/// attempt to 2 and returns to queued, the second DieStarted re-claims attempt
/// 2, DieSucceeded(2) reaches review, InspectionPassed reaches waiting — and
/// every row is written through the same durable machinery as production
/// appends (intent row first, receipt generated by the ledger trigger), with
/// the v2 hash recomputed via the ledger's own `row_hash_at`. The result
/// replays cleanly (verify_rows passes) with an actual attempt of 2, so only
/// the finalizer's actual-attempt comparison can reject an attempt-1 caller.
pub fn append_g05_retry_chain(
    db_path: &std::path::Path,
    order_id: &str,
    lifecycle: i64,
    at: chrono::DateTime<Utc>,
) {
    use rusqlite::params;
    let mut conn = rusqlite::Connection::open(db_path).unwrap();
    // (from, to, event repr, intent, canonical event JSON)
    let chain: [(&str, &str, &str, &str, &str); 5] = [
        (
            "queued",
            "working",
            "DieStarted",
            "p2-test-support",
            r#"{"type":"die_started"}"#,
        ),
        (
            "working",
            "queued",
            "DieFailed { attempt: 1, reason: \"g05-retry\" }",
            // decode_event takes the DieFailed reason from the INTENT column.
            "g05-retry",
            r#"{"type":"die_failed","attempt":1,"reason":"g05-retry"}"#,
        ),
        (
            "queued",
            "working",
            "DieStarted",
            "p2-test-support",
            r#"{"type":"die_started"}"#,
        ),
        (
            "working",
            "review",
            "DieSucceeded { attempt: 2 }",
            "p2-test-support",
            r#"{"type":"die_succeeded","attempt":2}"#,
        ),
        (
            "review",
            "waiting",
            "InspectionPassed",
            "p2-test-support",
            r#"{"type":"inspection_passed"}"#,
        ),
    ];
    let actor = "Agent/p2-synthetic-test";
    let at_text = at.to_rfc3339();
    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    for (from, to, event, intent, json) in chain {
        let (next_seq, prev_hash): (i64, String) = conn
            .query_row(
                "SELECT COALESCE(MAX(seq),0)+1,\
                 COALESCE((SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1),'GENESIS')\
                 FROM ledger",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let row = foundry_ledger::sqlite::LedgerRow {
            seq: next_seq,
            order_id: order_id.to_owned(),
            from_state: from.to_owned(),
            to_state: to.to_owned(),
            event: event.to_owned(),
            actor: actor.to_owned(),
            intent: intent.to_owned(),
            at: at_text.clone(),
            prev_hash: prev_hash.clone(),
            hash: String::new(),
            event_json: Some(json.to_owned()),
        };
        let hash =
            foundry_ledger::sqlite::SqliteLedger::row_hash_at(&[row.clone()]).expect("v2 row hash");
        conn.execute(
            "INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation)\
             VALUES(?1,?2,?3,'p2-test-preparation')",
            params![order_id, lifecycle, next_seq - 1],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash)\
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![order_id, from, to, event, actor, intent, at_text, prev_hash, json, hash],
        )
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
}

#[allow(dead_code)]
pub fn genuine_host_done(identity: FixtureIdentity) -> GenuineResultFixture {
    genuine_host_done_with_bootstrap(
        identity,
        "owner",
        "result-owner-password",
        |_, db_path, config, source, _now, owner_id, owner_password| {
            let mut store = ProductStore::open(db_path, product_configured(config)).unwrap();
            let owner_phc =
                foundry_ledger::product_identity::ProductPasswordKdf::hash(owner_password).unwrap();
            store
                .bootstrap_owner_scoped(owner_id, &owner_phc, std::slice::from_ref(source), &[])
                .unwrap();
        },
    )
}

/// Same genuine producer with the test-support stage-start observation signal
/// installed before the ProductStore is moved into PackReviewHost.
pub fn genuine_host_done_with_stage_start_observer(
    identity: FixtureIdentity,
    observer: foundry_ledger::ChargeBudgetObserver,
) -> GenuineResultFixture {
    genuine_host_with_bootstrap_and_observer(
        identity,
        "owner",
        "result-owner-password",
        |_, db_path, config, source, _now, owner_id, owner_password| {
            let mut store = ProductStore::open(db_path, product_configured(config)).unwrap();
            let owner_phc =
                foundry_ledger::product_identity::ProductPasswordKdf::hash(owner_password).unwrap();
            store
                .bootstrap_owner_scoped(owner_id, &owner_phc, std::slice::from_ref(source), &[])
                .unwrap();
        },
        true,
        Some(observer),
    )
}

/// Prepare the same genuine flow but leave host dispatch to the caller so a
/// test can observe a real active run at a deterministic executor barrier.
#[allow(dead_code)]
pub fn genuine_host_inflight(identity: FixtureIdentity) -> GenuineResultFixture {
    genuine_host_with_bootstrap(
        identity,
        "owner",
        "result-owner-password",
        |_, db_path, config, source, _now, owner_id, owner_password| {
            let mut store = ProductStore::open(db_path, product_configured(config)).unwrap();
            let owner_phc =
                foundry_ledger::product_identity::ProductPasswordKdf::hash(owner_password).unwrap();
            store
                .bootstrap_owner_scoped(owner_id, &owner_phc, std::slice::from_ref(source), &[])
                .unwrap();
        },
        false,
    )
}

/// Run a caller-owned bootstrap against the fixture's exact DB, config,
/// source and root, then continue through the same genuine producer path as
/// `genuine_host_done`. The callback runs once before this helper opens the
/// store and authenticates the synthetic owner for service/admission setup.
pub fn genuine_host_done_with_bootstrap<F>(
    identity: FixtureIdentity,
    owner_id: &str,
    owner_password: &str,
    bootstrap: F,
) -> GenuineResultFixture
where
    F: FnOnce(
        &std::path::Path,
        &std::path::Path,
        &ProductConfig,
        &ScopeSourceManifest,
        chrono::DateTime<Utc>,
        &str,
        &str,
    ),
{
    genuine_host_with_bootstrap(identity, owner_id, owner_password, bootstrap, true)
}

fn genuine_host_with_bootstrap<F>(
    identity: FixtureIdentity,
    owner_id: &str,
    owner_password: &str,
    bootstrap: F,
    dispatch_on_return: bool,
) -> GenuineResultFixture
where
    F: FnOnce(
        &std::path::Path,
        &std::path::Path,
        &ProductConfig,
        &ScopeSourceManifest,
        chrono::DateTime<Utc>,
        &str,
        &str,
    ),
{
    genuine_host_with_bootstrap_and_observer(
        identity,
        owner_id,
        owner_password,
        bootstrap,
        dispatch_on_return,
        None,
    )
}

fn genuine_host_with_bootstrap_and_observer<F>(
    identity: FixtureIdentity,
    owner_id: &str,
    owner_password: &str,
    bootstrap: F,
    dispatch_on_return: bool,
    observer: Option<foundry_ledger::ChargeBudgetObserver>,
) -> GenuineResultFixture
where
    F: FnOnce(
        &std::path::Path,
        &std::path::Path,
        &ProductConfig,
        &ScopeSourceManifest,
        chrono::DateTime<Utc>,
        &str,
        &str,
    ),
{
    let root = scratch();
    let db_path = root.join("product.sqlite");
    let now = Utc::now();
    let source = ScopeSourceManifest {
        connector_id: identity.connector_id.clone(),
        source_instance_id: identity.source_instance_id.clone(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            identity.repository_owner.clone(),
            "github",
            &identity.api_origin,
            &identity.repository_id,
        )
        .expect("valid repository identity"),
        api_owner: Some(identity.repository_owner.clone()),
        api_repo: Some(identity.repository_name.clone()),
        legacy_repository_locator: identity.repository_id.clone(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let config = product_configured(&identity.product);
    bootstrap(
        &root,
        &db_path,
        &config,
        &source,
        now,
        owner_id,
        owner_password,
    );
    let mut store = ProductStore::open(&db_path, product_configured(&identity.product)).unwrap();
    let owner = store
        .login(owner_id, owner_password, now)
        .unwrap()
        .digest()
        .to_owned();
    let script = identity.script;
    let service_credential_minutes = identity.service_credential_minutes;
    let bump_service_revision = identity.bump_service_revision;
    store
        .create_account(
            &owner,
            "reader",
            "approver",
            &foundry_ledger::product_identity::ProductPasswordKdf::hash("result-reader-password")
                .unwrap(),
            now,
        )
        .unwrap();
    store
        .change_product_scope(
            &owner,
            "reader",
            &source.source_instance_id,
            ProductCapability::ReadArtifact,
            ScopeMutation::Grant,
            0,
            "result-read-grant",
            now,
        )
        .unwrap();
    store
        .change_product_scope(
            &owner,
            "reader",
            &source.source_instance_id,
            ProductCapability::ApproveOrder,
            ScopeMutation::Grant,
            1,
            "result-approve-scope-grant",
            now,
        )
        .unwrap();
    let reader_session = store
        .login("reader", "result-reader-password", now)
        .unwrap();
    let human_session_digest = reader_session.digest().to_owned();
    let human_session_bearer_path = root.join("synthetic-human-session-bearer");
    write_private(&human_session_bearer_path, reader_session.bearer());
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
            &identity.service_id,
            &actions,
            true,
            now + Duration::hours(3),
            0,
            "result-service",
            now,
        )
        .unwrap();
    if bump_service_revision {
        store
            .manage_product_service(
                &owner,
                &identity.service_id,
                &actions,
                true,
                now + Duration::hours(3),
                1,
                "result-service-revision-two",
                now,
            )
            .unwrap();
    }
    let credential = store
        .issue_product_service_credential(
            &owner,
            &identity.service_id,
            if bump_service_revision { 2 } else { 1 },
            now + Duration::minutes(service_credential_minutes),
            "result-credential",
            now,
        )
        .unwrap();
    let service_token = credential.expose_once_to_file_writer().to_owned();
    let service_bearer_path = root.join("synthetic-service-bearer");
    write_private(&service_bearer_path, &service_token);
    let service = store
        .authenticate_product_service_for_action(
            &service_token,
            &identity.connector_id,
            &identity.api_origin,
            &identity.repository_id,
            ProductServiceAction::DriveReview,
            now,
        )
        .unwrap();
    let updated_at = now.to_rfc3339();
    let payload = "explicit P2 synthetic test snapshot";
    let version = foundry_ledger::product::delivery_version_digest(
        &identity.issue_id,
        &updated_at,
        "dep-bump",
        payload,
    );
    let delivery_id = foundry_ledger::product::github_delivery_id(
        store.installation_id(),
        &identity.connector_id,
        &identity.api_origin,
        &identity.repository_id,
        &identity.issue_id,
        &version,
    );
    let delivery = ProductDelivery {
        delivery_id: delivery_id.clone(),
        connector_id: identity.connector_id.clone(),
        repository_id: identity.repository_id.clone(),
        issue_id: identity.issue_id.clone(),
        observed_updated_at: updated_at.clone(),
        version_digest: version,
        task_kind: "dep-bump".into(),
        payload: payload.into(),
    };
    let cursor = ProductPollCursor {
        connector_id: identity.connector_id.clone(),
        query_fingerprint: "result-query".into(),
        sweep_id: "result-sweep".into(),
        next_page: 1,
        page_ceiling: 1,
        updated_at,
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
        .admit_intake_delivery_for_service(&service, &delivery.delivery_id, &identity.order_id, now)
        .unwrap();
    store
        .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admission, now)
        .unwrap();
    let preclaim_db_path = root.join("product-preclaim.sqlite");
    rusqlite::Connection::open(&db_path)
        .unwrap()
        .execute(
            "VACUUM INTO ?1",
            [preclaim_db_path.to_string_lossy().as_ref()],
        )
        .unwrap();
    if identity.stale_admission_preparation_probe {
        let before: (i64, i64, i64, i64, i64, i64) = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_work_backlog_v1 WHERE order_id=?1 AND state='waiting_unsupported')",
                [&identity.order_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            before,
            (0, 0, 0, 0, 0, 1),
            "stale-admission vector starts with genuinely fresh waiting work"
        );
        let mut stale = admission.clone();
        stale.sequence += 1;
        assert!(matches!(
            store.prepare_p2_review_fixture(
                &service,
                &stale,
                &delivery.delivery_id,
                identity.preparation
            ),
            Err(foundry_ledger::ProductError::Invariant(
                "P2 preparation requires exact acknowledged admission and fresh work"
            ))
        ));
        let after: (i64, i64, i64, i64, i64, i64) = rusqlite::Connection::open(&db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_run_reservations_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_review_runs_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                        (SELECT count(*) FROM product_work_backlog_v1 WHERE order_id=?1 AND state='waiting_unsupported')",
                [&identity.order_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            after, before,
            "stale admission fails before changing fresh-work evidence"
        );
    }
    let preparation_fault = rusqlite::Connection::open(&db_path).unwrap();
    let (create_preparation_fault, drop_preparation_fault) = match identity.preparation {
        P2ReviewPreparation::Waiting => (
            "CREATE TRIGGER p2_preparation_fail_s1 BEFORE INSERT ON ledger
             BEGIN SELECT RAISE(ABORT,'injected P2 preparation S1 failure'); END;",
            "DROP TRIGGER p2_preparation_fail_s1;",
        ),
        P2ReviewPreparation::Queued => (
            "CREATE TRIGGER p2_preparation_fail_backlog BEFORE UPDATE ON product_work_backlog_v1
             BEGIN SELECT RAISE(ABORT,'injected P2 preparation backlog failure'); END;",
            "DROP TRIGGER p2_preparation_fail_backlog;",
        ),
    };
    preparation_fault
        .execute_batch(create_preparation_fault)
        .unwrap();
    assert!(
        store
            .prepare_p2_review_fixture(
                &service,
                &admission,
                &delivery.delivery_id,
                identity.preparation,
            )
            .is_err()
    );
    let rolled_back: (i64, i64, i64, String) = preparation_fault
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
                    (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2)",
            rusqlite::params![identity.order_id, admission.lifecycle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(rolled_back, (1, 1, 0, "waiting_unsupported".into()));
    preparation_fault
        .execute_batch(drop_preparation_fault)
        .unwrap();
    let before_zero_row: (i64, i64, i64, String) = preparation_fault
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
                    (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2)",
            rusqlite::params![identity.order_id, admission.lifecycle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    preparation_fault
        .execute_batch(
            "CREATE TRIGGER p2_preparation_zero_row BEFORE UPDATE ON product_work_backlog_v1
             BEGIN SELECT RAISE(IGNORE); END;",
        )
        .unwrap();
    let zero_row_result = store.prepare_p2_review_fixture(
        &service,
        &admission,
        &delivery.delivery_id,
        identity.preparation,
    );
    assert!(
        matches!(
            zero_row_result,
            Err(foundry_ledger::ProductError::Invariant(
                "P2 backlog CAS failed"
            ))
        ),
        "a conditional zero-row backlog update is an exact rollback failure"
    );
    let after_zero_row: (i64, i64, i64, String) = preparation_fault
        .query_row(
            "SELECT (SELECT count(*) FROM ledger WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
                    (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2)",
            rusqlite::params![identity.order_id, admission.lifecycle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after_zero_row, before_zero_row);
    preparation_fault
        .execute_batch("DROP TRIGGER p2_preparation_zero_row;")
        .unwrap();
    drop(preparation_fault);
    if identity.g05_retry_ledger {
        assert_eq!(
            identity.preparation,
            P2ReviewPreparation::Queued,
            "the G05 retry-ledger probe runs the backlog CAS preparation only"
        );
        store
            .prepare_p2_review_fixture(
                &service,
                &admission,
                &delivery.delivery_id,
                P2ReviewPreparation::Queued,
            )
            .unwrap();
        append_g05_retry_chain(&db_path, &identity.order_id, admission.lifecycle, now);
    } else {
        store
            .prepare_p2_review_fixture(
                &service,
                &admission,
                &delivery.delivery_id,
                identity.preparation,
            )
            .unwrap();
    }
    // Prove the explicit P2 preparation boundary before a worker claim or
    // review host can create evidence. The accepted fixture starts with the
    // admission plus exactly three product-owned lifecycle transitions.
    let preparation = rusqlite::Connection::open(&db_path).unwrap();
    let prepared: (i64, i64, i64, i64, i64, i64, i64, String) = preparation
        .query_row(
            "SELECT
               (SELECT count(*) FROM ledger WHERE order_id=?1),
               (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
               (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
               (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2)",
            rusqlite::params![identity.order_id, admission.lifecycle],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    let expected_preparation = if identity.g05_retry_ledger {
        // Admission row plus the five chain-consistent retry transitions.
        (6, 6, 0, 0, 0, 0, 0, "ready".into())
    } else {
        match identity.preparation {
            P2ReviewPreparation::Waiting => (4, 4, 0, 0, 0, 0, 0, "ready".into()),
            P2ReviewPreparation::Queued => (1, 1, 0, 0, 0, 0, 0, "ready".into()),
        }
    };
    assert_eq!(prepared, expected_preparation);
    let duplicate_result = store.prepare_p2_review_fixture(
        &service,
        &admission,
        &delivery.delivery_id,
        identity.preparation,
    );
    assert!(
        matches!(
            duplicate_result,
            Err(foundry_ledger::ProductError::Invariant(
                "P2 preparation requires exact acknowledged admission and fresh work"
            ))
        ),
        "duplicate preparation before any worker claim is rejected exactly"
    );
    let after_duplicate: (i64, i64, i64, i64, i64, i64, i64, String) = preparation
        .query_row(
            "SELECT
               (SELECT count(*) FROM ledger WHERE order_id=?1),
               (SELECT count(*) FROM product_transition_receipts_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_transition_intents_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_work_claims_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
               (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1),
               (SELECT state FROM product_work_backlog_v1 WHERE order_id=?1 AND lifecycle=?2)",
            rusqlite::params![identity.order_id, admission.lifecycle],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(after_duplicate, expected_preparation);
    drop(preparation);
    let claim = match store
        .claim_product_work_for_service(&service, &identity.worker_id, now, Duration::minutes(5))
        .unwrap()
    {
        foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
        other => panic!("expected genuine durable work claim, got {other:?}"),
    };
    let preactivation_authority: i64 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1",
            [&identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        preactivation_authority, 0,
        "intake and claim do not create activation authority"
    );
    let artifact_root = root.join("artifacts");
    let session_root = root.join("sessions");
    let claim_root = root.join("claims");
    let pack =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let pack_digest = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8".to_owned();
    let review_config = foundry_ledger::ProductReviewConfig {
        artifact_root: artifact_root.clone(),
        session_root: session_root.clone(),
        claim_root: claim_root.clone(),
        pack_digest,
    };
    let host_clock = identity
        .adjustable_host_clock
        .then(|| AdjustableFixtureClock::new(now));
    let mut host_store = if let Some(clock) = host_clock.as_ref() {
        ProductStore::open_with_clock(
            &db_path,
            product_configured(&identity.product),
            Arc::new(clock.clone()),
        )
        .unwrap()
    } else {
        ProductStore::open(&db_path, product_configured(&identity.product)).unwrap()
    };
    host_store.set_charge_budget_observer(observer);
    host_store.configure_review_backend(review_config).unwrap();
    let mut result_store = ProductStore::open_with_clock(
        &db_path,
        product_configured(&identity.product),
        std::sync::Arc::new(FixtureClock(now)),
    )
    .unwrap();
    result_store
        .configure_product_artifact_root(&artifact_root)
        .unwrap();
    let intent = ExecutionIntent {
        order: OrderId(identity.order_id.clone()),
        attempt: Attempt(1),
        run: RunId(7),
        activation: 3,
        generation: Generation(0),
        epoch: Epoch(1),
        external_ref: format!(
            "review:{}:1:{}..{}",
            identity.order_id,
            "a".repeat(40),
            "b".repeat(40)
        ),
        recorded_at: now,
    };
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let restores = std::sync::Arc::new(AtomicUsize::new(0));
    let stage_pause = Arc::new(StagePause::default());
    let task_observations = Arc::new(Mutex::new(Vec::new()));
    let mut host = PackReviewHost::with_product_store(
        &pack,
        PassingExecutor {
            executions: calls.clone(),
            restores: restores.clone(),
            observations: task_observations.clone(),
            script,
            pause: stage_pause.clone(),
            triage: 0,
        },
        host_store,
        artifact_root.clone(),
        session_root.clone(),
        claim_root.clone(),
    )
    .unwrap()
    .with_source_commit("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
    .unwrap();
    let (terminal, dispatch_error) = if dispatch_on_return {
        match host.try_dispatch_product_work(&intent, &claim) {
            Ok(terminal) => (Some(terminal), None),
            Err(error) => (None, Some(format!("{error:?}"))),
        }
    } else {
        (None, None)
    };
    let activated_authority: i64 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM product_review_authority_v1 WHERE order_id=?1",
            [&identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(activated_authority, i64::from(dispatch_on_return));
    let expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: identity.order_id.clone(),
            attempt: 1,
            run: 7,
            activation: 3,
        },
        host_result_digest: terminal
            .as_ref()
            .and_then(|result| foundry_ledger::product_host_result_digest(result).ok())
            .unwrap_or_else(|| "0".repeat(64)),
    };
    let mut approval = ProductApprovalIntent {
        request_key: "result-approval".into(),
        grant_id: "result-approval-grant".into(),
        token_digest: "c".repeat(64),
        order_id: identity.order_id.clone(),
        expected_source_revision: 1,
        expected_attempt: 1,
        run_id: 7,
        generation: 0,
        artifact_digest: terminal
            .as_ref()
            .map(|result| result.result_commit.clone())
            .unwrap_or_default(),
        tuple_digest: String::new(),
        intent: "approve exact local result".into(),
        now,
    };
    let artifact_root_device_inode = directory_identity(&artifact_root);
    // Filled by H after a real finalization; these are coordinates only, not proof.
    approval.tuple_digest.clear();
    GenuineResultFixture {
        root: root.clone(),
        db_path,
        preclaim_db_path,
        artifact_root,
        session_root,
        claim_root,
        service_bearer_path,
        human_session_bearer_path,
        delivery_id,
        config: identity.product.clone(),
        identity,
        now,
        service,
        service_token,
        human_session_digest,
        owner_session_digest: owner,
        approval,
        approval_operation_id: None,
        admission,
        intent,
        claim,
        host: Some(host),
        result_store: Some(result_store),
        terminal,
        dispatch_error,
        expected,
        calls,
        restores,
        artifact_root_device_inode,
        stage_pause,
        host_clock,
        task_observations,
    }
}

/// Advance G-pre to G-final through the actual ProductStore finalizer, then
/// issue a scoped human approval grant and persist the exact approval intent.
#[allow(dead_code)] // H is G-pre coverage; HX advances through the G-final helper.
pub fn genuine_result_finalized(mut fixture: GenuineResultFixture) -> GenuineResultFixture {
    assert!(
        fixture.dispatch_error.is_none(),
        "cannot finalize a failed host dispatch"
    );
    assert!(
        fixture.terminal.is_some(),
        "finalized fixture requires real host result"
    );
    let mut store = fixture
        .result_store
        .take()
        .expect("G-pre retains its ProductStore finalizer handle");
    let info = store
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .expect("genuine host result finalizes through ProductStore");
    let token_digest = "9".repeat(64);
    let grant_id = format!("{}-strict-approval-grant", fixture.identity.order_id);
    let request_key = format!("{}-strict-approval-request", fixture.identity.order_id);
    store
        .issue_approval_grant(
            &fixture.human_session_digest,
            &grant_id,
            &token_digest,
            &fixture.identity.order_id,
            fixture.now,
        )
        .expect("issue actual human approval grant");
    fixture.approval.request_key = request_key;
    fixture.approval.grant_id = grant_id;
    fixture.approval.token_digest = token_digest;
    fixture.approval.artifact_digest = fixture.terminal.as_ref().unwrap().result_commit.clone();
    fixture.approval.tuple_digest = info.tuple_digest.clone();
    let proof = store
        .prepare_approval(&fixture.human_session_digest, &fixture.approval)
        .expect("persist exact human approval intent and reservation");
    fixture.approval_operation_id = Some(proof.operation_id().to_owned());
    fixture.result_store = Some(store);
    fixture
}

/// Add the two current, source-bound capabilities required by the genuine
/// human hold APIs. Existing H/HX fixtures retain their original read and
/// approval scope unless a continuation scenario opts into this helper.
#[allow(dead_code)]
pub fn grant_review_hold_scope(fixture: &GenuineResultFixture) {
    let revision: i64 = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1
             WHERE subject_id='reader' AND source_instance_id=?1",
            [&fixture.identity.source_instance_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut store = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let read_review = store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &fixture.identity.source_instance_id,
            ProductCapability::ReadReview,
            ScopeMutation::Grant,
            revision,
            "result-read-review-grant",
            fixture.now,
        )
        .unwrap();
    store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &fixture.identity.source_instance_id,
            ProductCapability::ResolveReviewHold,
            ScopeMutation::Grant,
            read_review.scope_revision,
            "result-resolve-hold-grant",
            fixture.now,
        )
        .unwrap();
}

/// Provision a separate, real single-use approval grant and persist its
/// request. Used for distinct-key process competition against one result.
#[allow(dead_code)] // Used by HX's two-process distinct-key competition.
pub fn prepare_distinct_approval(
    fixture: &mut GenuineResultFixture,
    request_key: &str,
    grant_id: &str,
    token_digest: &str,
) -> String {
    assert_eq!(token_digest.len(), 64);
    let store = fixture
        .result_store
        .as_mut()
        .expect("G-final ProductStore remains open");
    store
        .issue_approval_grant(
            &fixture.human_session_digest,
            grant_id,
            token_digest,
            &fixture.identity.order_id,
            fixture.now,
        )
        .expect("issue distinct real approval grant");
    let mut intent = fixture.approval.clone();
    intent.request_key = request_key.to_owned();
    intent.grant_id = grant_id.to_owned();
    intent.token_digest = token_digest.to_owned();
    store
        .prepare_approval(&fixture.human_session_digest, &intent)
        .expect("persist distinct exact approval request")
        .operation_id()
        .to_owned()
}

pub fn write_private(path: &std::path::Path, contents: &str) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .expect("create private synthetic credential file");
    file.write_all(contents.as_bytes())
        .expect("write private synthetic credential file");
    file.sync_all()
        .expect("flush private synthetic credential file");
}

#[cfg(unix)]
pub fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).expect("artifact root metadata");
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
pub fn directory_identity(path: &std::path::Path) -> (u64, u64) {
    let metadata = std::fs::metadata(path).expect("artifact root metadata");
    (
        metadata.len(),
        metadata
            .modified()
            .ok()
            .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|v| v.as_nanos() as u64)
            .unwrap_or(0),
    )
}
