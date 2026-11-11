#![cfg(feature = "p2-test-support")]

//! Shared private fixtures for the B2 W1-a runtime slice.
//! ProductStore state is real; StageExecutor implementations are deterministic local fakes.

use chrono::{Duration, Utc};
use foundry_exec::{Attempt, Epoch, ExecutionIntent, Generation, OrderId, RunId};
use foundry_ledger::{
    AdmissionReceipt, P2ReviewPreparation, ProductCapability, ProductConfig, ProductDelivery,
    ProductHoldAcceptance, ProductHoldDecision, ProductHoldExpectation, ProductPollCursor,
    ProductReviewConfig, ProductServiceAction, ProductServiceContext, ProductServiceGrant,
    ProductStore, ProductWorkClaim, ProductWorkClaimResult, RepositoryIdentity, ScopeGrantManifest,
    ScopeSourceManifest,
};
use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
use rusqlite::{Connection, types::Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub type Snapshot = BTreeMap<String, Vec<Vec<Value>>>;

pub struct PrivateRoot(pub PathBuf);

impl PrivateRoot {
    fn new() -> Self {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .expect("TMPDIR must name configured private scratch");
        assert_ne!(
            base,
            Path::new("/tmp"),
            "runtime fixture must use configured scratch"
        );
        let path = base.join(format!(
            "b2-w1-runtime-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create private runtime fixture root");
        Self(path)
    }
}

impl Drop for PrivateRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Clone)]
pub struct WorkItem {
    pub order_id: String,
    pub delivery_id: String,
    pub admission: AdmissionReceipt,
    pub claim: ProductWorkClaim,
}

#[derive(Clone)]
pub struct Held {
    pub order_id: String,
    pub hold_instance_id: String,
    pub resume_target: String,
    pub snapshot_run_id: String,
    pub expected: ProductHoldExpectation,
    pub originating_run: i64,
}

pub struct RuntimeFixture {
    pub root: PrivateRoot,
    pub db_path: PathBuf,
    pub config: ProductConfig,
    pub pack: PathBuf,
    pub now: chrono::DateTime<Utc>,
    pub owner_session: String,
    pub owner_bearer: String,
    pub service_token: String,
    pub service: ProductServiceContext,
    pub items: Vec<WorkItem>,
}

impl RuntimeFixture {
    pub fn new(order_ids: &[&str]) -> Self {
        assert!(!order_ids.is_empty());
        let root = PrivateRoot::new();
        let db_path = root.0.join("product.sqlite");
        let config = ProductConfig {
            installation_id: "b2-w1-runtime-install".into(),
            database_id: "b2-w1-runtime-database".into(),
        };
        let now = Utc::now();
        let source = ScopeSourceManifest {
            connector_id: "github-primary".into(),
            source_instance_id: "b2-w1-source".into(),
            generation: 1,
            identity: RepositoryIdentity::canonicalized(
                "acme",
                "github",
                "https://api.github.com",
                "repo:1",
            )
            .expect("valid fixture repository identity"),
            api_owner: Some("acme".into()),
            api_repo: Some("project".into()),
            legacy_repository_locator: "repo:1".into(),
            scope_query: "open+foundry:make".into(),
            enabled: true,
        };
        let owner_phc =
            foundry_ledger::product_identity::ProductPasswordKdf::hash("b2-w1-test-owner-password")
                .expect("hash test-only password");
        let mut store = ProductStore::open(&db_path, config.clone()).expect("open runtime store");
        store
            .bootstrap_owner_scoped(
                "owner",
                &owner_phc,
                std::slice::from_ref(&source),
                &[ScopeGrantManifest {
                    actor_id: "owner".into(),
                    source_instance_id: source.source_instance_id.clone(),
                    capabilities: vec![
                        ProductCapability::ReadReview,
                        ProductCapability::ResolveReviewHold,
                    ],
                }],
            )
            .expect("bootstrap scoped owner");
        let human = store
            .login("owner", "b2-w1-test-owner-password", now)
            .expect("login fixture owner");
        let owner_session = human.digest().to_owned();
        let owner_bearer = human.bearer().to_owned();
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
                &owner_session,
                "b2-w1-service",
                &actions,
                true,
                now + Duration::hours(2),
                0,
                "b2-w1-service-config",
                now,
            )
            .expect("configure local test service");
        let credential = store
            .issue_product_service_credential(
                &owner_session,
                "b2-w1-service",
                1,
                now + Duration::hours(1),
                "b2-w1-service-credential",
                now,
            )
            .expect("issue local test credential");
        let service_token = credential.expose_once_to_file_writer().to_owned();
        let service = store
            .authenticate_product_service_for_action(
                &service_token,
                "github-primary",
                "https://api.github.com",
                "repo:1",
                ProductServiceAction::DriveReview,
                now,
            )
            .expect("authenticate local service");

        let mut items = Vec::new();
        for order_id in order_ids {
            let issue_id = format!("{order_id}-issue");
            let updated_at = now.to_rfc3339();
            let task_kind = "dep-bump";
            let payload = format!("synthetic P2 payload for {order_id}");
            let version = foundry_ledger::product::delivery_version_digest(
                &issue_id,
                &updated_at,
                task_kind,
                &payload,
            );
            let delivery_id = foundry_ledger::product::github_delivery_id(
                store.installation_id(),
                "github-primary",
                "https://api.github.com",
                "repo:1",
                &issue_id,
                &version,
            );
            let delivery = ProductDelivery {
                delivery_id: delivery_id.clone(),
                connector_id: "github-primary".into(),
                repository_id: "repo:1".into(),
                issue_id,
                observed_updated_at: updated_at.clone(),
                version_digest: version,
                task_kind: task_kind.into(),
                payload,
            };
            let cursor = ProductPollCursor {
                connector_id: "github-primary".into(),
                query_fingerprint: format!("b2-w1-{order_id}"),
                sweep_id: format!("b2-w1-sweep-{order_id}"),
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
                .expect("persist synthetic P2 intake page");
            let admission = store
                .admit_intake_delivery_for_service(&service, &delivery.delivery_id, order_id, now)
                .expect("admit synthetic P2 delivery");
            store
                .mark_intake_acked_for_service(&service, &delivery.delivery_id, &admission, now)
                .expect("ack synthetic P2 delivery");
            // Normal intake stays waiting_unsupported. This existing feature-gated
            // preparation API creates the control state for runtime tests.
            store
                .prepare_p2_review_fixture(
                    &service,
                    &admission,
                    &delivery.delivery_id,
                    P2ReviewPreparation::Waiting,
                )
                .expect("prepare supported P2 review state");
            let claim = match store
                .claim_product_work_for_service(
                    &service,
                    &format!("b2-w1-worker-{}", order_id),
                    now,
                    Duration::minutes(5),
                )
                .expect("claim prepared P2 work")
            {
                ProductWorkClaimResult::Claimed(claim) if claim.order_id() == *order_id => claim,
                _ => panic!("prepared P2 work was not claimed for its expected order"),
            };
            items.push(WorkItem {
                order_id: (*order_id).to_owned(),
                delivery_id,
                admission,
                claim,
            });
        }
        let pack = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/bmad-review-full/v1")
            .canonicalize()
            .expect("resolve frozen review pack");
        Self {
            root,
            db_path,
            config,
            pack,
            now,
            owner_session,
            owner_bearer,
            service_token,
            service,
            items,
        }
    }

    pub fn item(&self, order_id: &str) -> &WorkItem {
        self.items
            .iter()
            .find(|item| item.order_id == order_id)
            .expect("requested fixture order exists")
    }

    pub fn artifact_root(&self) -> PathBuf {
        self.root.0.join("artifacts")
    }

    pub fn session_root(&self) -> PathBuf {
        self.root.0.join("sessions")
    }

    pub fn claim_root(&self) -> PathBuf {
        self.root.0.join("claims")
    }

    pub fn store(&self) -> ProductStore {
        ProductStore::open(&self.db_path, self.config.clone()).expect("open fixture ProductStore")
    }

    pub fn host<E: StageExecutor + 'static>(&self, executor: E) -> PackReviewHost {
        PackReviewHost::with_product_store(
            &self.pack,
            executor,
            self.store(),
            self.artifact_root(),
            self.session_root(),
            self.claim_root(),
        )
        .expect("construct ProductStore-backed frozen-pack host")
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .expect("set synthetic source commit")
    }

    pub fn make_hold<E: StageExecutor + 'static>(&self, order_id: &str, executor: E) -> Held {
        let item = self.item(order_id);
        let mut host = self.host(executor);
        let intent = ExecutionIntent {
            order: OrderId(order_id.to_owned()),
            attempt: Attempt(1),
            run: RunId(7),
            activation: 1,
            generation: Generation(0),
            epoch: Epoch(1),
            external_ref: format!("review:{order_id}:1:base..result"),
            recorded_at: self.now,
        };
        let held = host
            .try_dispatch_product_work(&intent, &item.claim)
            .expect("dispatch prepared fixture work")
            .held
            .expect("fake stage failure should persist a hold");
        let latest = host
            .latest_hold(order_id)
            .expect("read persisted hold")
            .expect("persisted hold exists");
        assert!(
            latest.hold_id == held.hold_instance_id,
            "persisted hold identity matches"
        );
        let expected = hold_expectation(&self.db_path, order_id, &held.hold_instance_id);
        Held {
            order_id: order_id.to_owned(),
            hold_instance_id: held.hold_instance_id,
            resume_target: held
                .resume_targets
                .first()
                .cloned()
                .expect("frozen hold has an allowed resume target"),
            snapshot_run_id: latest.snapshot.snapshot_run_id,
            expected,
            originating_run: 7,
        }
    }

    pub fn accept_resume(&self, held: &Held, request_key: &str) -> ProductHoldAcceptance {
        self.store()
            .accept_product_review_hold(
                &self.owner_session,
                &held.order_id,
                &held.hold_instance_id,
                &held.expected,
                request_key,
                ProductHoldDecision::Resume(held.resume_target.clone()),
                self.now,
            )
            .expect("accept exact fixture Resume decision")
    }

    pub fn snapshot_dir(&self, held: &Held) -> PathBuf {
        let run_hash = hex(&Sha256::digest(held.snapshot_run_id.as_bytes()));
        self.artifact_root().join(run_hash).join("pack-snapshot")
    }

    pub fn configured_review_store(&self, pack_digest: String) -> ProductStore {
        let mut store = self.store();
        store
            .configure_review_backend(ProductReviewConfig {
                artifact_root: self.artifact_root(),
                session_root: self.session_root(),
                claim_root: self.claim_root(),
                pack_digest,
            })
            .expect("configure direct review test store");
        store
    }
}

pub fn hold_expectation(db: &Path, order: &str, hold_id: &str) -> ProductHoldExpectation {
    Connection::open(db)
        .expect("open hold expectation observer")
        .query_row(
            "SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 WHERE order_id=?1 AND hold_instance_id=?2",
            rusqlite::params![order, hold_id],
            |row| {
                Ok(ProductHoldExpectation {
                    lifecycle: row.get(0)?,
                    attempt: row.get(1)?,
                    originating_run: row.get(2)?,
                    activation: row.get(3)?,
                    host_epoch: row.get(4)?,
                    hold_revision: row.get(5)?,
                    source_revision: row.get(6)?,
                    predecessor_seq: row.get(7)?,
                    snapshot_digest: row.get(8)?,
                })
            },
        )
        .expect("read immutable hold expectation")
}

pub fn snapshot(db: &Path) -> Snapshot {
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open logical ProductStore observer");
    let mut tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .expect("list logical ProductStore tables");
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .expect("query logical ProductStore table names")
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|name| {
            let escaped = name.replace('"', "\"\"");
            let mut statement = conn
                .prepare(&format!("SELECT * FROM \"{escaped}\""))
                .expect("prepare logical ProductStore table query");
            let columns = statement.column_count();
            let mut rows: Vec<Vec<Value>> = statement
                .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                .expect("query logical ProductStore rows")
                .map(Result::unwrap)
                .collect();
            rows.sort_by_cached_key(|row| format!("{row:?}"));
            (name, rows)
        })
        .collect()
}

pub fn assert_snapshot_unchanged(before: &Snapshot, db: &Path) {
    let after = snapshot(db);
    assert!(before == &after, "logical ProductStore snapshot changed");
}

pub fn count(db: &Path, table: &str) -> i64 {
    Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open ProductStore count observer")
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count ProductStore evidence")
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub struct FailingExecutor {
    pub calls: Arc<AtomicUsize>,
}

impl StageExecutor for FailingExecutor {
    fn execute(&mut self, _task: &StageTask) -> Result<StageOutput, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err("synthetic fixture stage failure".into())
    }
}

pub struct SuccessExecutor {
    pub calls: Arc<AtomicUsize>,
    pub lens_bytes: Option<Arc<std::sync::Mutex<Vec<Vec<u8>>>>>,
    pub after_preflight: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl StageExecutor for SuccessExecutor {
    fn ensure_available(&self) -> Result<(), String> {
        if let Some(callback) = &self.after_preflight {
            callback();
        }
        Ok(())
    }

    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(lenses) = &self.lens_bytes {
            lenses
                .lock()
                .expect("lock stage observations")
                .push(task.lens_bytes.clone());
        }
        let output = task.workdir.join("b2-w1-stage-output.log");
        fs::write(&output, b"synthetic accepted continuation output\n")
            .map_err(|error| error.to_string())?;
        Ok(StageOutput {
            verdict_category: Some("patch".into()),
            findings: true,
            log_path: output.to_string_lossy().into_owned(),
            inspected_commit: (task.role == "verify").then(|| task.artifact_commit.clone()),
            verdict_categories: Vec::new(),
            produced_commit: (task.role == "fix").then(|| task.artifact_commit.clone()),
        })
    }

    fn restore_session(&mut self, _session_id: &str, _workdir: &Path) -> Result<(), String> {
        Ok(())
    }
}

pub fn y_pack_with_changed_lenses(source: &Path, destination: &Path) -> (PathBuf, String) {
    copy_tree(source, destination).expect("copy frozen pack for Y fixture");
    for entry in fs::read_dir(destination).expect("list copied pack") {
        let entry = entry.expect("read copied pack entry");
        if entry.file_type().expect("read pack entry type").is_file()
            && entry.file_name().to_string_lossy().starts_with("lens-")
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "md")
        {
            let mut bytes = fs::read(entry.path()).expect("read copied lens");
            bytes.extend_from_slice(b"\nB2-W1-Y-FIXTURE\n");
            fs::write(entry.path(), bytes).expect("change Y pack lens bytes");
        }
    }
    let manifest_path = destination.join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path).expect("read copied manifest");
    let files_line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("files = ["))
        .expect("manifest files list");
    let declared: Vec<String> = files_line
        .split('[')
        .nth(1)
        .and_then(|tail| tail.split(']').next())
        .expect("manifest files array")
        .split(',')
        .filter_map(|part| {
            let name = part.trim().trim_matches('"');
            (!name.is_empty()).then(|| name.to_owned())
        })
        .collect();
    let mut entries = declared
        .into_iter()
        .map(|name| {
            (
                name.clone(),
                fs::read(destination.join(name)).expect("read declared pack file"),
            )
        })
        .collect::<Vec<_>>();
    let canonical_manifest = fs::read(&manifest_path).expect("read manifest bytes");
    let manifest_lines = String::from_utf8(canonical_manifest).expect("manifest UTF-8");
    let stripped = manifest_lines
        .lines()
        .filter(|line| !line.trim_start().starts_with("digest ="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    entries.push(("manifest.toml".to_owned(), stripped.into_bytes()));
    entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let mut canonical = Vec::new();
    for (name, bytes) in entries {
        put_field(&mut canonical, name.as_bytes());
        put_field(&mut canonical, &bytes);
    }
    let digest = hex(&Sha256::digest(&canonical));
    let new_manifest = manifest
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("digest =") {
                format!("digest = \"{digest}\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(&manifest_path, new_manifest).expect("write recomputed Y manifest");
    assert!(
        manifest_digest(destination) == digest,
        "recomputed pack manifest digest differs"
    );
    (destination.to_owned(), digest)
}

pub fn manifest_digest(pack_dir: &Path) -> String {
    fs::read_to_string(pack_dir.join("manifest.toml"))
        .expect("read pack manifest")
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("digest = \"")
                .and_then(|value| value.strip_suffix('\"'))
                .map(str::to_owned)
        })
        .expect("pack manifest digest")
}

fn put_field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value);
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn copy_y_lenses_into_snapshot(y_pack: &Path, snapshot_dir: &Path) {
    for entry in fs::read_dir(y_pack).expect("list Y pack lenses") {
        let entry = entry.expect("read Y lens entry");
        if entry.file_type().expect("read Y entry type").is_file()
            && entry.file_name().to_string_lossy().starts_with("lens-")
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "md")
        {
            fs::copy(entry.path(), snapshot_dir.join(entry.file_name()))
                .expect("replace persisted snapshot lens with Y bytes");
        }
    }
    let manifest = fs::read(y_pack.join("manifest.toml")).expect("read Y manifest");
    fs::write(snapshot_dir.join("manifest.toml"), manifest)
        .expect("replace persisted snapshot manifest with Y bytes");
}

pub fn request_body(held: &Held, decision: &str, target: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "expected": held.expected,
        "decision": decision,
        "target": target,
    })
}

pub fn hold_id(held: &Held) -> &str {
    &held.hold_instance_id
}
