use foundry_ledger::{ProductConfig, ProductHoldDecision, ProductStore};
use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "support/product_result_fixture.rs"]
mod product_result_fixture;
use product_result_fixture::{
    FixtureClock, FixtureIdentity, ReviewScript, directory_identity, genuine_host_done,
    genuine_result_finalized, grant_review_hold_scope, prepare_distinct_approval, write_private,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuationProcessManifestV1 {
    db_path: PathBuf,
    installation_id: String,
    database_id: String,
    artifact_root: PathBuf,
    session_root: PathBuf,
    claim_root: PathBuf,
    human_session_digest: String,
    operation_id: String,
    now: String,
    started_path: PathBuf,
    release_path: PathBuf,
    execute_count_path: PathBuf,
    result_path: PathBuf,
}

struct ProcessContinuationExecutor {
    started_path: PathBuf,
    release_path: PathBuf,
    execute_count_path: PathBuf,
    execute_count: usize,
}

impl StageExecutor for ProcessContinuationExecutor {
    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.execute_count += 1;
        std::fs::write(&self.execute_count_path, self.execute_count.to_string())
            .map_err(|error| error.to_string())?;
        if task.stage_id == "replan_full-g1" {
            std::fs::write(&self.started_path, b"stage-start-committed")
                .map_err(|error| error.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(30);
            while !self.release_path.exists() {
                if Instant::now() >= deadline {
                    return Err("parent did not release the post-start stage gate".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let output = task.workdir.join("output.json");
        std::fs::write(
            &output,
            serde_json::json!({
                "category": null,
                "categories": [],
                "findings": false,
                "stage": task.stage_id,
                "inspected_commit": task.artifact_commit,
                "produced_commit": "c".repeat(40),
            })
            .to_string(),
        )
        .map_err(|error| error.to_string())?;
        Ok(StageOutput {
            verdict_category: None,
            findings: false,
            log_path: output.to_string_lossy().into_owned(),
            inspected_commit: Some(task.artifact_commit.clone()),
            verdict_categories: vec![],
            produced_commit: Some("c".repeat(40)),
        })
    }

    fn restore_session(&mut self, _session_id: &str, _workdir: &Path) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn product_result_continuation_child() {
    let Some(manifest_path) = std::env::var_os("B1_RESULT_CONTINUATION_MANIFEST") else {
        return;
    };
    let manifest: ContinuationProcessManifestV1 =
        serde_json::from_slice(&std::fs::read(manifest_path).unwrap()).unwrap();
    let config = ProductConfig {
        installation_id: manifest.installation_id,
        database_id: manifest.database_id,
    };
    let mut store = ProductStore::open(&manifest.db_path, config).unwrap();
    store
        .configure_review_backend(foundry_ledger::ProductReviewConfig {
            artifact_root: manifest.artifact_root.clone(),
            session_root: manifest.session_root.clone(),
            claim_root: manifest.claim_root.clone(),
            pack_digest: "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8".into(),
        })
        .unwrap();
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let mut host = PackReviewHost::with_product_store(
        &pack,
        ProcessContinuationExecutor {
            started_path: manifest.started_path,
            release_path: manifest.release_path,
            execute_count_path: manifest.execute_count_path,
            execute_count: 0,
        },
        store,
        manifest.artifact_root.clone(),
        manifest.session_root,
        manifest.claim_root,
    )
    .unwrap()
    .with_source_commit("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")
    .unwrap();
    let result = host.continue_accepted_product_hold(
        &manifest.human_session_digest,
        &manifest.operation_id,
        chrono::DateTime::parse_from_rfc3339(&manifest.now)
            .unwrap()
            .with_timezone(&chrono::Utc),
    );
    std::fs::write(
        manifest.result_path,
        serde_json::json!({
            "result": format!("{result:?}"),
        })
        .to_string(),
    )
    .unwrap();
    assert!(
        result.is_err(),
        "revocation blocks the next continuation edge"
    );
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ChildManifestV1 {
    version: u32,
    operation: String,
    db_path: PathBuf,
    installation_id: String,
    database_id: String,
    artifact_root: PathBuf,
    session_root: PathBuf,
    claim_root: PathBuf,
    artifact_root_device: u64,
    artifact_root_inode: u64,
    connector_id: String,
    api_origin: String,
    repository_id: String,
    source_instance_id: String,
    source_generation: i64,
    source_instance_revision: i64,
    connector_revision: i64,
    service_id: String,
    service_bearer_path: PathBuf,
    test_time: String,
    host_order: String,
    host_attempt: i64,
    host_run: i64,
    host_activation: i64,
    host_result_digest: String,
    human_session_digest: Option<String>,
    human_session_bearer_path: Option<PathBuf>,
    approval: Option<ApprovalManifestV1>,
    expected_lifecycle: i64,
    delivery_id: String,
    worker_id: String,
    hold_operation_id: Option<String>,
    request_identity: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ApprovalManifestV1 {
    operation_id: String,
    request_key: String,
    grant_id: String,
    token_digest: String,
    order_id: String,
    expected_source_revision: i64,
    expected_attempt: i64,
    run_id: i64,
    generation: i64,
    artifact_digest: String,
    tuple_digest: String,
    intent: String,
    now: String,
}

fn write_child_manifest(fixture: &product_result_fixture::GenuineResultFixture) -> PathBuf {
    fixture.assert_genuine_private_handoff();
    let source: (i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT i.generation,i.revision,s.revision
             FROM product_scope_source_instances_v1 i
             JOIN product_scope_current_sources_v1 c USING(connector_id)
             JOIN product_scope_repositories_v1 r USING(repository_key)
             JOIN product_sources_v1 s USING(connector_id)
             WHERE i.source_instance_id=?1 AND c.source_instance_id=i.source_instance_id
               AND r.api_origin=?2 AND r.stable_repository_id=?3",
            rusqlite::params![
                fixture.identity.source_instance_id,
                fixture.identity.api_origin,
                fixture.identity.repository_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let manifest = ChildManifestV1 {
        version: 1,
        operation: "finalize_result".into(),
        db_path: fixture.db_path.clone(),
        installation_id: fixture.config.installation_id.clone(),
        database_id: fixture.config.database_id.clone(),
        artifact_root: fixture.artifact_root.clone(),
        session_root: fixture.session_root.clone(),
        claim_root: fixture.claim_root.clone(),
        artifact_root_device: fixture.artifact_root_device_inode.0,
        artifact_root_inode: fixture.artifact_root_device_inode.1,
        connector_id: fixture.identity.connector_id.clone(),
        api_origin: fixture.identity.api_origin.clone(),
        repository_id: fixture.identity.repository_id.clone(),
        source_instance_id: fixture.identity.source_instance_id.clone(),
        source_generation: source.0,
        source_instance_revision: source.1,
        connector_revision: source.2,
        service_id: fixture.identity.service_id.clone(),
        service_bearer_path: fixture.service_bearer_path.clone(),
        test_time: fixture.now.to_rfc3339(),
        host_order: fixture.expected.host_key.order.clone(),
        host_attempt: i64::try_from(fixture.expected.host_key.attempt).unwrap(),
        host_run: i64::try_from(fixture.expected.host_key.run).unwrap(),
        host_activation: i64::try_from(fixture.expected.host_key.activation).unwrap(),
        host_result_digest: fixture.expected.host_result_digest.clone(),
        human_session_digest: Some(fixture.human_session_digest.clone()),
        // This child performs result finalization only. The session bearer is
        // intentionally not issued to or serialized for this process.
        human_session_bearer_path: Some(fixture.human_session_bearer_path.clone()),
        // Reservation proof is deliberately inapplicable here; H retains the
        // real prepare_approval assertions for the later approval gate.
        approval: None,
        expected_lifecycle: fixture.admission.lifecycle,
        delivery_id: fixture.delivery_id.clone(),
        worker_id: fixture.identity.worker_id.clone(),
        hold_operation_id: None,
        request_identity: format!("{}:genuine-result-finalization", fixture.identity.order_id),
    };
    let path = fixture.root.join("result-child-manifest.json");
    let bytes = serde_json::to_vec(&manifest).unwrap();
    write_private(&path, std::str::from_utf8(&bytes).unwrap());
    // Source revision fields are checked against the DB-derived tuple, not
    // inferred from the connector revision or copied from a caller context.
    assert_eq!(manifest.source_generation, source.0);
    assert_eq!(manifest.source_instance_revision, source.1);
    assert_eq!(manifest.connector_revision, source.2);
    path
}

fn write_approval_manifest(
    fixture: &product_result_fixture::GenuineResultFixture,
    intent: &foundry_ledger::ProductApprovalIntent,
    operation_id: &str,
    suffix: &str,
) -> PathBuf {
    fixture.assert_genuine_private_handoff();
    let source: (i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT i.generation,i.revision,s.revision
             FROM product_scope_source_instances_v1 i
             JOIN product_scope_current_sources_v1 c USING(connector_id)
             JOIN product_scope_repositories_v1 r USING(repository_key)
             JOIN product_sources_v1 s USING(connector_id)
             WHERE i.source_instance_id=?1 AND c.source_instance_id=i.source_instance_id
               AND r.api_origin=?2 AND r.stable_repository_id=?3",
            rusqlite::params![
                fixture.identity.source_instance_id,
                fixture.identity.api_origin,
                fixture.identity.repository_id
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let manifest = ChildManifestV1 {
        version: 1,
        operation: "approve_product".into(),
        db_path: fixture.db_path.clone(),
        installation_id: fixture.config.installation_id.clone(),
        database_id: fixture.config.database_id.clone(),
        artifact_root: fixture.artifact_root.clone(),
        session_root: fixture.session_root.clone(),
        claim_root: fixture.claim_root.clone(),
        artifact_root_device: fixture.artifact_root_device_inode.0,
        artifact_root_inode: fixture.artifact_root_device_inode.1,
        connector_id: fixture.identity.connector_id.clone(),
        api_origin: fixture.identity.api_origin.clone(),
        repository_id: fixture.identity.repository_id.clone(),
        source_instance_id: fixture.identity.source_instance_id.clone(),
        source_generation: source.0,
        source_instance_revision: source.1,
        connector_revision: source.2,
        service_id: fixture.identity.service_id.clone(),
        service_bearer_path: fixture.service_bearer_path.clone(),
        test_time: fixture.now.to_rfc3339(),
        host_order: fixture.expected.host_key.order.clone(),
        host_attempt: i64::try_from(fixture.expected.host_key.attempt).unwrap(),
        host_run: i64::try_from(fixture.expected.host_key.run).unwrap(),
        host_activation: i64::try_from(fixture.expected.host_key.activation).unwrap(),
        host_result_digest: fixture.expected.host_result_digest.clone(),
        human_session_digest: Some(fixture.human_session_digest.clone()),
        human_session_bearer_path: Some(fixture.human_session_bearer_path.clone()),
        approval: Some(ApprovalManifestV1 {
            operation_id: operation_id.to_owned(),
            request_key: intent.request_key.clone(),
            grant_id: intent.grant_id.clone(),
            token_digest: intent.token_digest.clone(),
            order_id: intent.order_id.clone(),
            expected_source_revision: intent.expected_source_revision,
            expected_attempt: intent.expected_attempt,
            run_id: intent.run_id,
            generation: intent.generation,
            artifact_digest: intent.artifact_digest.clone(),
            tuple_digest: intent.tuple_digest.clone(),
            intent: intent.intent.clone(),
            now: intent.now.to_rfc3339(),
        }),
        expected_lifecycle: fixture.admission.lifecycle,
        delivery_id: fixture.delivery_id.clone(),
        worker_id: fixture.identity.worker_id.clone(),
        hold_operation_id: None,
        request_identity: format!("{suffix}:{}", intent.request_key),
    };
    assert_eq!(manifest.source_generation, source.0);
    assert_eq!(manifest.source_instance_revision, source.1);
    assert_eq!(manifest.connector_revision, source.2);
    let path = fixture.root.join(format!("approval-child-{suffix}.json"));
    let bytes = serde_json::to_vec(&manifest).unwrap();
    write_private(&path, std::str::from_utf8(&bytes).unwrap());
    path
}

struct OwnedChild(Option<Child>);
impl OwnedChild {
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned child remains present")
    }
    fn finish(&mut self) -> std::process::ExitStatus {
        let mut child = self.0.take().expect("owned child remains present");
        child.wait().expect("wait for owned child")
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn kill_child_at_token(executable: &Path, manifest: &Path, boundary: &str, test_name: &str) {
    assert!(
        executable.is_absolute(),
        "B1_LEDGER_TEST_EXE must be absolute"
    );
    assert!(
        executable.is_file(),
        "B1_LEDGER_TEST_EXE must name the prebuilt ledger unit executable"
    );
    let child = Command::new(executable)
        .args(["--exact", test_name, "--nocapture"])
        .env("B1_PRODUCT_RESULT_CHILD_MANIFEST", manifest)
        .env("B1_PRODUCT_KILL_BOUNDARY", boundary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn exact prebuilt ledger test executable");
    let mut child = OwnedChild(Some(child));
    let stdout = child.child_mut().stdout.take().expect("child stdout pipe");
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let approval_child = test_name.contains("approval_child_tests");
    let expected = if approval_child {
        format!("B1_BOUNDARY:{boundary}")
    } else {
        format!("B1_PRODUCT_RESULT_BOUNDARY:{boundary}")
    };
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut seen = false;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(remaining) {
            Ok(Ok(line)) if line == expected => {
                if approval_child {
                    eprintln!("HX_REACHED:B1_PRODUCT_APPROVAL_BOUNDARY:{boundary}");
                } else {
                    eprintln!("HX_REACHED:{line}");
                }
                seen = true;
                break;
            }
            Ok(Ok(line)) => assert!(
                !line.starts_with("B1_PRODUCT_RESULT_BOUNDARY:"),
                "child reached wrong barrier: {line}"
            ),
            Ok(Err(error)) => panic!("read child barrier pipe: {error}"),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if !seen {
        let _ = child.child_mut().kill();
        let status = child.finish();
        panic!("child did not reach {expected}; early exit status: {status}");
    }
    child
        .child_mut()
        .kill()
        .expect("SIGKILL only the owned child");
    let status = child.finish();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "child must have died from SIGKILL"
        );
    }
    #[cfg(not(unix))]
    assert!(
        !status.success(),
        "controlled child must have been terminated"
    );
}

fn result_counts(path: &Path) -> (i64, i64, i64, i64, i64, i64) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT
               (SELECT count(*) FROM artifact_tuples WHERE order_id IN ('result-pre','result-post')),
               (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id IN ('result-pre','result-post')),
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id IN ('result-pre','result-post') AND status='complete'),
               (SELECT count(*) FROM product_artifacts_v1 WHERE order_id IN ('result-pre','result-post')),
               (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id IN ('result-pre','result-post')),
               (SELECT count(*) FROM ledger WHERE order_id IN ('result-pre','result-post'))",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap()
}

fn configured_reopen(fixture: &product_result_fixture::GenuineResultFixture) -> ProductStore {
    assert_eq!(
        directory_identity(&fixture.artifact_root),
        fixture.artifact_root_device_inode,
        "reopen must use the original artifact root"
    );
    let mut store = ProductStore::open_with_clock(
        &fixture.db_path,
        ProductConfig {
            installation_id: fixture.config.installation_id.clone(),
            database_id: fixture.config.database_id.clone(),
        },
        std::sync::Arc::new(FixtureClock(fixture.now)),
    )
    .unwrap();
    store
        .configure_product_artifact_root(&fixture.artifact_root)
        .unwrap();
    store
}

fn product_result_counts(path: &Path, order: &str) -> (i64, i64, i64, i64, i64, i64) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT
               (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
               (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),
               (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
               (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
               (SELECT count(*) FROM ledger WHERE order_id=?1)",
            [order],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .unwrap()
}

fn approval_participants(
    path: &Path,
    intent: &foundry_ledger::ProductApprovalIntent,
    operation_id: &str,
) -> (i64, i64, i64, i64, i64, i64, String, String) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT
               (SELECT count(*) FROM ledger WHERE order_id=?1 AND to_state='publishing'),
               (SELECT count(*) FROM product_transition_receipts_v1 WHERE operation='human-approval' AND order_id=?1),
               (SELECT count(*) FROM product_token_consumptions_v1 WHERE operation_id=?2),
               (SELECT count(*) FROM product_grants_v1 WHERE grant_id=?3 AND state='consumed'),
               (SELECT count(*) FROM product_outcomes_v1 WHERE operation_id=?2),
               (SELECT count(*) FROM product_approval_result_bindings_v1 WHERE operation_id=?2),
               (SELECT state FROM product_grants_v1 WHERE grant_id=?3),
               (SELECT phase FROM product_operations_v1 WHERE operation_id=?2)",
            rusqlite::params![intent.order_id, operation_id, intent.grant_id],
            |row| {
                Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                    row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?,
                ))
            },
        )
        .unwrap()
}

fn approval_receipt_wire(path: &Path, operation_id: &str) -> Option<String> {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_outcomes_v1 WHERE operation_id=?1",
            [operation_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

const APPROVAL_CHILD_TEST: &str = "product::result::approval_child_tests::child_approves_exact_genuine_result_with_issued_human_session";

struct OwnedApprovalChild(Option<Child>);

impl OwnedApprovalChild {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }
    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("owned child")
    }
    fn wait_bounded(mut self) -> std::process::Output {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self
                .child_mut()
                .try_wait()
                .expect("poll owned approval child")
            {
                Some(_) => {
                    let child = self.0.take().expect("owned child");
                    return child
                        .wait_with_output()
                        .expect("collect approval child output");
                }
                None if Instant::now() < deadline => std::thread::yield_now(),
                None => {
                    let _ = self.child_mut().kill();
                    let mut child = self.0.take().expect("owned child");
                    let _ = child.wait();
                    panic!("approval child exceeded bounded completion time");
                }
            }
        }
    }
}

impl Drop for OwnedApprovalChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn spawn_gated_approval_child(
    executable: &Path,
    manifest: &Path,
    ready: &Path,
    gate: &Path,
) -> OwnedApprovalChild {
    OwnedApprovalChild::new(
        Command::new(executable)
            .args(["--exact", APPROVAL_CHILD_TEST, "--nocapture"])
            .env("B1_PRODUCT_RESULT_CHILD_MANIFEST", manifest)
            .env("B1_APPROVAL_READY", ready)
            .env("B1_APPROVAL_GATE", gate)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn exact ledger approval child"),
    )
}

fn wait_for_child_file(path: &Path, description: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {description}"
        );
        std::thread::yield_now();
    }
}

fn assert_exact_approval_child(output: &std::process::Output) -> String {
    assert!(
        output.status.success(),
        "approval child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).expect("child stdout is UTF-8");
    assert!(
        stdout.contains("running 1 test"),
        "child selected count missing: {stdout}"
    );
    assert!(
        stdout.contains(&format!("test {APPROVAL_CHILD_TEST} ... ok")),
        "exact child test did not pass: {stdout}"
    );
    assert!(
        stdout.contains("1 passed; 0 failed"),
        "child did not execute exactly one test: {stdout}"
    );
    stdout
}

#[test]
fn genuine_result_child_crash_before_and_after_commit_preserves_atomic_set() {
    let executable = std::env::var_os("B1_LEDGER_TEST_EXE")
        .map(PathBuf::from)
        .expect("B1_LEDGER_TEST_EXE must name the externally prebuilt ledger unit-test executable");
    assert!(
        executable.is_absolute(),
        "B1_LEDGER_TEST_EXE must be absolute"
    );

    // Precommit crash: each case has its own genuine G-pre host evidence.
    let mut before = genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-host-install".into(),
            database_id: "result-before-db".into(),
        },
        "result-before-source",
        "result-pre",
    ));
    let before_manifest = write_child_manifest(&before);
    drop(before.host.take());
    drop(before.result_store.take());
    kill_child_at_token(
        &executable,
        &before_manifest,
        "after_result_binding_insert",
        "product::result::finalizer_child_tests::child_pauses_at_selected_real_result_finalizer_boundary",
    );
    let before_counts = result_counts(&before.db_path);
    assert_eq!(before_counts, (0, 0, 4, 4, 3, 4));
    let mut before_reopened = configured_reopen(&before);
    assert_eq!(
        before_reopened
            .lookup_product_review_result(&before.service, &before.expected)
            .unwrap(),
        None
    );
    let before_retry = before_reopened
        .finalize_product_review_result(&before.service, &before.expected)
        .unwrap();
    assert_eq!(result_counts(&before.db_path), (1, 1, 4, 4, 3, 4));
    assert_eq!(before.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(before.restores.load(std::sync::atomic::Ordering::SeqCst), 0);
    drop(before_reopened);

    // Postcommit response-loss crash: a separate genuine host and database.
    let mut after = genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-host-install".into(),
            database_id: "result-after-db".into(),
        },
        "result-after-source",
        "result-post",
    ));
    let after_manifest = write_child_manifest(&after);
    drop(after.host.take());
    drop(after.result_store.take());
    kill_child_at_token(
        &executable,
        &after_manifest,
        "after_commit_before_response",
        "product::result::finalizer_child_tests::child_pauses_at_selected_real_result_finalizer_boundary",
    );
    assert_eq!(result_counts(&after.db_path), (1, 1, 4, 4, 3, 4));
    let mut after_reopened = configured_reopen(&after);
    let stored = after_reopened
        .lookup_product_review_result(&after.service, &after.expected)
        .unwrap()
        .expect("committed result must be discoverable after child response loss");
    let receipt_before: String = Connection::open(&after.db_path)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id='result-post'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let after_retry = after_reopened
        .finalize_product_review_result(&after.service, &after.expected)
        .unwrap();
    let receipt_after: String = Connection::open(&after.db_path)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id='result-post'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after_retry, stored);
    assert_eq!(
        receipt_after, receipt_before,
        "replay preserves exact receipt wire bytes"
    );
    assert_eq!(result_counts(&after.db_path), (1, 1, 4, 4, 3, 4));
    assert_eq!(after.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert_eq!(after.restores.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        (
            before_retry.run_id,
            before_retry.activation,
            before_retry.generation
        ),
        (7, 3, 0)
    );

    std::fs::remove_dir_all(&before.root).unwrap();
    std::fs::remove_dir_all(&after.root).unwrap();
}

#[test]
fn genuine_approval_child_sigkill_restores_every_participant_boundary() {
    let executable = std::env::var_os("B1_LEDGER_TEST_EXE")
        .map(PathBuf::from)
        .expect("B1_LEDGER_TEST_EXE must name the exact externally prebuilt unit executable");
    assert!(executable.is_absolute() && executable.is_file());
    let boundaries = [
        "after_s1_insert",
        "after_grant_consumption",
        "after_token_insert",
        "after_outcome_insert",
        "after_result_binding_insert",
        "after_approval_commit_before_response",
    ];
    for (index, boundary) in boundaries.into_iter().enumerate() {
        let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
            ProductConfig {
                installation_id: "result-approval-install".into(),
                database_id: format!("result-approval-kill-{index}"),
            },
            format!("result-approval-source-{index}"),
            format!("result-approval-order-{index}"),
        )));
        fixture.assert_genuine_private_handoff();
        let intent = fixture.approval.clone();
        let operation_id = fixture
            .approval_operation_id
            .clone()
            .expect("real prepare_approval persisted operation");
        let manifest = write_approval_manifest(&fixture, &intent, &operation_id, boundary);
        let order = fixture.identity.order_id.clone();
        let db = fixture.db_path.clone();
        let root = fixture.root.clone();
        let result_before = product_result_counts(&db, &order);
        assert_eq!(result_before, (1, 1, 4, 4, 3, 4));
        let result_wire_before = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
                [&order],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(
            approval_participants(&db, &intent, &operation_id),
            (0, 0, 0, 0, 0, 0, "reserved".into(), "prepared".into())
        );
        drop(fixture.host.take());
        drop(fixture.result_store.take());

        kill_child_at_token(
            &executable,
            &manifest,
            boundary,
            "product::result::approval_child_tests::child_approves_exact_genuine_result_with_issued_human_session",
        );
        let expected_result_counts_after_child =
            if boundary == "after_approval_commit_before_response" {
                (1, 1, 4, 4, 3, 5)
            } else {
                result_before
            };
        assert_eq!(
            product_result_counts(&db, &order),
            expected_result_counts_after_child
        );
        let result_wire_after: String = Connection::open(&db)
            .unwrap()
            .query_row(
                "SELECT receipt_json FROM product_result_bindings_v1 WHERE order_id=?1",
                [&order],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result_wire_after, result_wire_before);
        let participants = approval_participants(&db, &intent, &operation_id);
        if boundary == "after_approval_commit_before_response" {
            assert_eq!(
                participants,
                (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
            );
            let before_wire = approval_receipt_wire(&db, &operation_id).unwrap();
            let mut reopened = configured_reopen(&fixture);
            let proof = reopened
                .prepare_approval(&fixture.human_session_digest, &intent)
                .unwrap();
            assert_eq!(proof.operation_id(), operation_id);
            let recovered = reopened
                .approve_product(&fixture.human_session_digest, &intent, &proof)
                .unwrap();
            assert_eq!(
                serde_json::to_string(&recovered).unwrap(),
                before_wire,
                "postcommit reopen returns exact stored receipt bytes"
            );
            assert_eq!(
                approval_receipt_wire(&db, &operation_id).as_deref(),
                Some(before_wire.as_str())
            );
            assert_eq!(
                approval_participants(&db, &intent, &operation_id),
                participants
            );
            drop(reopened);
        } else {
            assert_eq!(
                participants,
                (0, 0, 0, 0, 0, 0, "reserved".into(), "prepared".into())
            );
            let mut reopened = configured_reopen(&fixture);
            let proof = reopened
                .prepare_approval(&fixture.human_session_digest, &intent)
                .unwrap();
            assert_eq!(proof.operation_id(), operation_id);
            let receipt = reopened
                .approve_product(&fixture.human_session_digest, &intent, &proof)
                .unwrap();
            let wire = serde_json::to_string(&receipt).unwrap();
            assert_eq!(
                approval_receipt_wire(&db, &operation_id).as_deref(),
                Some(wire.as_str())
            );
            assert_eq!(
                approval_participants(&db, &intent, &operation_id),
                (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
            );
            let replay_proof = reopened
                .prepare_approval(&fixture.human_session_digest, &intent)
                .unwrap();
            let replay = reopened
                .approve_product(&fixture.human_session_digest, &intent, &replay_proof)
                .unwrap();
            assert_eq!(serde_json::to_string(&replay).unwrap(), wire);
            assert_eq!(
                approval_participants(&db, &intent, &operation_id),
                (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
            );
            drop(reopened);
        }
        assert_eq!(product_result_counts(&db, &order), (1, 1, 4, 4, 3, 5));
        assert_eq!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(
            fixture.restores.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        drop(fixture);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn separate_processes_competing_on_one_revision_commit_exactly_one_approval_set() {
    let executable = std::env::var_os("B1_LEDGER_TEST_EXE")
        .map(PathBuf::from)
        .expect("B1_LEDGER_TEST_EXE is required for approval competition");
    assert!(executable.is_absolute() && executable.is_file());

    for first_contender in ["a", "b"] {
        let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
            ProductConfig {
                installation_id: "result-approval-race-install".into(),
                database_id: format!("result-approval-race-{first_contender}"),
            },
            format!("result-approval-race-source-{first_contender}"),
            format!("result-approval-race-order-{first_contender}"),
        )));
        let mut intents = Vec::new();
        let mut operation_ids = Vec::new();
        for contender in ["a", "b"] {
            let request_key = format!("{}-request-{contender}", fixture.identity.order_id);
            let grant_id = format!("{}-grant-{contender}", fixture.identity.order_id);
            let token_digest = if contender == "a" { "a" } else { "b" }.repeat(64);
            let operation_id =
                prepare_distinct_approval(&mut fixture, &request_key, &grant_id, &token_digest);
            let mut intent = fixture.approval.clone();
            intent.request_key = request_key;
            intent.grant_id = grant_id;
            intent.token_digest = token_digest;
            intents.push(intent);
            operation_ids.push(operation_id);
        }
        let manifests = [
            write_approval_manifest(&fixture, &intents[0], &operation_ids[0], "race-a"),
            write_approval_manifest(&fixture, &intents[1], &operation_ids[1], "race-b"),
        ];
        let root = fixture.root.clone();
        let db = fixture.db_path.clone();
        let order = fixture.identity.order_id.clone();
        drop(fixture.host.take());
        drop(fixture.result_store.take());

        let ready_a = root.join("race-a-ready");
        let ready_b = root.join("race-b-ready");
        let gate_a = root.join("race-a-release");
        let gate_b = root.join("race-b-release");
        let child_a = spawn_gated_approval_child(&executable, &manifests[0], &ready_a, &gate_a);
        let child_b = spawn_gated_approval_child(&executable, &manifests[1], &ready_b, &gate_b);
        wait_for_child_file(&ready_a, "first contender prepare_approval");
        wait_for_child_file(&ready_b, "second contender prepare_approval");

        let (first, first_gate, second, second_gate, first_index, second_index) =
            if first_contender == "a" {
                (child_a, gate_a, child_b, gate_b, 0, 1)
            } else {
                (child_b, gate_b, child_a, gate_a, 1, 0)
            };
        std::fs::write(first_gate, b"commit first").unwrap();
        let first_stdout = assert_exact_approval_child(&first.wait_bounded());
        let first_receipt = first_stdout
            .lines()
            .find_map(|line| line.strip_prefix("B1_PRODUCT_APPROVAL_RECEIPT:"))
            .expect("first process must commit and return a real approval receipt")
            .to_owned();
        let first_wire = approval_receipt_wire(&db, &operation_ids[first_index])
            .expect("winning receipt persisted");
        assert_eq!(first_receipt, first_wire);

        std::fs::write(second_gate, b"commit second").unwrap();
        let second_stdout = assert_exact_approval_child(&second.wait_bounded());
        assert!(
            second_stdout.contains("B1_PRODUCT_APPROVAL_DENIED:")
                && second_stdout.contains("order is not currently approvable"),
            "loser must fail the shared waiting revision semantically: {second_stdout}"
        );
        assert_eq!(
            approval_participants(&db, &intents[first_index], &operation_ids[first_index]),
            (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
        );
        assert_eq!(
            approval_participants(&db, &intents[second_index], &operation_ids[second_index]),
            (1, 1, 0, 0, 0, 0, "reserved".into(), "prepared".into())
        );
        assert_eq!(product_result_counts(&db, &order), (1, 1, 4, 4, 3, 5));
        let mut reopened = configured_reopen(&fixture);
        let winner_proof = reopened
            .prepare_approval(&fixture.human_session_digest, &intents[first_index])
            .unwrap();
        let replay = reopened
            .approve_product(
                &fixture.human_session_digest,
                &intents[first_index],
                &winner_proof,
            )
            .unwrap();
        assert_eq!(serde_json::to_string(&replay).unwrap(), first_wire);
        assert_eq!(product_result_counts(&db, &order), (1, 1, 4, 4, 3, 5));
        drop(reopened);
        drop(fixture);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn separate_processes_replaying_same_key_return_the_exact_committed_receipt() {
    let executable = std::env::var_os("B1_LEDGER_TEST_EXE")
        .map(PathBuf::from)
        .expect("B1_LEDGER_TEST_EXE is required for same-key replay");
    assert!(executable.is_absolute() && executable.is_file());

    for first_process in ["a", "b"] {
        let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
            ProductConfig {
                installation_id: "result-approval-replay-install".into(),
                database_id: format!("result-approval-replay-{first_process}"),
            },
            format!("result-approval-replay-source-{first_process}"),
            format!("result-approval-replay-order-{first_process}"),
        )));
        let intent = fixture.approval.clone();
        let operation_id = fixture.approval_operation_id.clone().unwrap();
        let manifest = write_approval_manifest(&fixture, &intent, &operation_id, "same-key");
        let root = fixture.root.clone();
        let db = fixture.db_path.clone();
        let order = fixture.identity.order_id.clone();
        drop(fixture.host.take());
        drop(fixture.result_store.take());

        let ready_a = root.join("same-a-ready");
        let ready_b = root.join("same-b-ready");
        let gate_a = root.join("same-a-release");
        let gate_b = root.join("same-b-release");
        let child_a = spawn_gated_approval_child(&executable, &manifest, &ready_a, &gate_a);
        let child_b = spawn_gated_approval_child(&executable, &manifest, &ready_b, &gate_b);
        wait_for_child_file(&ready_a, "first identical prepare_approval");
        wait_for_child_file(&ready_b, "second identical prepare_approval");
        let (first, first_gate, second, second_gate) = if first_process == "a" {
            (child_a, gate_a, child_b, gate_b)
        } else {
            (child_b, gate_b, child_a, gate_a)
        };
        std::fs::write(first_gate, b"commit first").unwrap();
        let first_stdout = assert_exact_approval_child(&first.wait_bounded());
        let first_receipt = first_stdout
            .lines()
            .find_map(|line| line.strip_prefix("B1_PRODUCT_APPROVAL_RECEIPT:"))
            .expect("first process must commit exact approval")
            .to_owned();
        std::fs::write(second_gate, b"replay second").unwrap();
        let second_stdout = assert_exact_approval_child(&second.wait_bounded());
        let second_receipt = second_stdout
            .lines()
            .find_map(|line| line.strip_prefix("B1_PRODUCT_APPROVAL_RECEIPT:"))
            .expect("second process must replay committed approval")
            .to_owned();
        assert_eq!(first_receipt, second_receipt);
        assert_eq!(
            approval_receipt_wire(&db, &operation_id).as_deref(),
            Some(first_receipt.as_str())
        );
        assert_eq!(
            approval_participants(&db, &intent, &operation_id),
            (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
        );
        assert_eq!(product_result_counts(&db, &order), (1, 1, 4, 4, 3, 5));
        let mut reopened = configured_reopen(&fixture);
        let proof = reopened
            .prepare_approval(&fixture.human_session_digest, &intent)
            .unwrap();
        let replay = reopened
            .approve_product(&fixture.human_session_digest, &intent, &proof)
            .unwrap();
        assert_eq!(serde_json::to_string(&replay).unwrap(), first_receipt);
        assert_eq!(
            approval_participants(&db, &intent, &operation_id),
            (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
        );
        drop(reopened);
        drop(fixture);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn approval_commit_write_failure_rolls_back_and_reopen_commits_once() {
    let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-approval-io-install".into(),
            database_id: "result-approval-io-db".into(),
        },
        "result-approval-io-source",
        "result-approval-io-order",
    )));
    let path = fixture.db_path.clone();
    let order = fixture.identity.order_id.clone();
    let intent = fixture.approval.clone();
    let operation_id = fixture.approval_operation_id.clone().unwrap();
    let baseline_results = product_result_counts(&path, &order);
    let baseline_approval = approval_participants(&path, &intent, &operation_id);

    let mut store = fixture.result_store.take().unwrap();
    let proof = store
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    // sol round107 gap 1: lock contention (BUSY) is not a filesystem I/O
    // failure. True COMMIT-I/O injection: this store keeps the default
    // rollback journal (journal_mode is never set to WAL on this path), so
    // the journal file is created when the first approval INSERT takes the
    // RESERVED lock, and the commit's final act is unlinking that journal —
    // a directory operation. A watcher thread waits for the journal to
    // appear, makes the directory read-only, and only then releases a
    // second connection that has been holding a SHARED read lock (so every
    // approval statement succeeds under RESERVED and the COMMIT is paused
    // at its EXCLUSIVE promotion until the injection lands). The commit
    // then acquires EXCLUSIVE, flushes its pages, and fails the journal
    // unlink with EACCES — a real filesystem I/O error at the actual commit
    // write — and the production ambiguous-commit recovery path (reopen,
    // inspect the authoritative outcome, never rerun blindly) executes for
    // real under the injected fault.
    let dir = path.parent().unwrap().to_path_buf();
    let dir_mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&dir).unwrap().permissions().mode()
    };
    let journal = path.with_file_name(format!(
        "{}-journal",
        path.file_name().unwrap().to_str().unwrap()
    ));
    let blocker = Connection::open(&path).unwrap();
    blocker
        .execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")
        .unwrap();
    let blocker = std::sync::Arc::new(std::sync::Mutex::new(Some(blocker)));
    let watcher_blocker = blocker.clone();
    let watcher_dir = dir.clone();
    let watcher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !journal.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "approval journal was never created; the write path never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &watcher_dir,
            std::fs::Permissions::from_mode(dir_mode & !0o222),
        )
        .unwrap();
        drop(watcher_blocker.lock().unwrap().take());
    });
    match store.approve_product(&fixture.human_session_digest, &intent, &proof) {
        Err(foundry_ledger::ProductError::Database(error)) => {
            assert!(
                matches!(
                    &error,
                    rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error {
                            code: rusqlite::ffi::ErrorCode::SystemIoFailure,
                            ..
                        },
                        _
                    )
                ),
                "the commit must fail with a filesystem I/O error at the commit write, got {error:?}"
            );
        }
        other => panic!("commit-time I/O failure must propagate a Database error, got {other:?}"),
    }
    watcher.join().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(dir_mode)).unwrap();
    }
    drop(store);
    assert_eq!(product_result_counts(&path, &order), baseline_results);
    assert_eq!(
        approval_participants(&path, &intent, &operation_id),
        baseline_approval,
        "every result and approval participant returns to its pre-call baseline"
    );

    let mut reopened = configured_reopen(&fixture);
    let proof = reopened
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    let receipt = reopened
        .approve_product(&fixture.human_session_digest, &intent, &proof)
        .unwrap();
    assert_eq!(receipt.operation_id, operation_id);
    assert_eq!(product_result_counts(&path, &order), (1, 1, 4, 4, 3, 5));
    assert_eq!(
        approval_participants(&path, &intent, &operation_id),
        (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
    );
    drop(reopened);
    drop(fixture);
}

#[test]
fn recovery_lookup_io_failure_propagates_error_not_absence() {
    // Gate3 rubric T2 cell 3 (ADD91, Discord 1563206386211496081; absent at
    // sol round105 gap 2): database-file page corruption (invalid b-tree
    // page type on this fixture's data pages) must surface as a propagated
    // storage error on the recovery lookup — never as Ok(None), which would
    // misclassify an I/O failure as honest absence. Every SQL error on the
    // lookup path maps to ProductResultError::Storage (result.rs
    // service_result_error / verified_order_in / read_stored_result_in).
    let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-lookup-io-install".into(),
            database_id: "result-lookup-io-db".into(),
        },
        "result-lookup-io-source",
        "result-lookup-io-order",
    )));
    let path = fixture.db_path.clone();
    let now = fixture.now;
    let service = fixture.service.clone();
    let expected = fixture.expected.clone();
    drop(fixture.result_store.take());
    drop(fixture);

    let bytes = std::fs::read(&path).unwrap();
    assert!(
        bytes.len() > 16 * 1024,
        "genuine fixture database must span multiple pages"
    );
    // sol round107 gap 2: the previous middle-third corruption could trip
    // during reopen, and an early-return arm accepted that outcome without
    // ever executing the lookup — a vacuous pass. Targeted injection
    // instead: every database page whose payload mentions this fixture's
    // order gets an invalid b-tree page-type byte (0x7F). Page 1 (the
    // sqlite_master schema page) is never touched, so reopening the store
    // must succeed, and the first lookup statement that walks an affected
    // table fails structurally — the lookup itself has to propagate the
    // error; there is no acceptable early return.
    let page_size = {
        let raw = u16::from_be_bytes([bytes[16], bytes[17]]) as usize;
        if raw == 1 { 65536 } else { raw }
    };
    assert!(
        page_size >= 512 && bytes.len() > page_size,
        "invalid page size or single-page database"
    );
    let needle = b"result-lookup-io-order";
    let mut page_starts = std::collections::BTreeSet::new();
    for (offset, window) in bytes.windows(needle.len()).enumerate() {
        if window == needle {
            page_starts.insert((offset / page_size) * page_size);
        }
    }
    assert!(
        page_starts.range(page_size..).next().is_some(),
        "fixture order payload must live on a data page beyond the schema page"
    );
    let mut corrupted = bytes;
    for page_start in page_starts.range(page_size..) {
        corrupted[*page_start] = 0x7F;
    }
    std::fs::write(&path, &corrupted).unwrap();

    let mut store = ProductStore::open_with_clock(
        &path,
        ProductConfig {
            installation_id: "result-lookup-io-install".into(),
            database_id: "result-lookup-io-db".into(),
        },
        std::sync::Arc::new(FixtureClock(now)),
    )
    .expect("the schema page is intact, so reopening the corrupted store must succeed");
    match store.lookup_product_review_result(&service, &expected) {
        Err(foundry_ledger::ProductResultError::Storage) => {}
        other => panic!(
            "corrupt payload must propagate Err(Storage) on the recovery lookup, never Ok(None), got {other:?}"
        ),
    }
    drop(store);
}

#[test]
fn v2_migration_cutoff_bounds_legacy_replay_and_postcutoff_rejection() {
    // Mechanics-only v3->v2 downgrade of a GENUINE approval (Gate3 rubric T3,
    // Decision 2-A, Discord 1563171600449413234): historical-origin evidence
    // remains denied; this cell measures only the migrate_product_results_v2
    // cutoff consumer predicate inside approve_product (product.rs:3452-3474).
    let mut fixture = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-cutoff-install".into(),
            database_id: "result-cutoff-db".into(),
        },
        "result-cutoff-source",
        "result-cutoff-order",
    )));
    let path = fixture.db_path.clone();
    let intent = fixture.approval.clone();
    let operation_id = fixture.approval_operation_id.clone().unwrap();
    drop(fixture.host.take());
    // 1. Genuine v3 approval commit through the public API.
    let mut store = fixture.result_store.take().unwrap();
    let proof = store
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    let first = store
        .approve_product(&fixture.human_session_digest, &intent, &proof)
        .unwrap();
    drop(store);
    let first_wire = serde_json::to_string(&first).unwrap();
    // 2. The approval event is the ledger tail; its seq becomes the cutoff.
    let approval_seq: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT MAX(seq) FROM ledger", [], |r| r.get(0))
        .unwrap();
    // 3. Downgrade drops the approval side binding; migration seals the cutoff.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TRIGGER product_result_meta_no_update; DROP TRIGGER product_result_meta_no_delete; DROP TRIGGER product_result_meta_no_replace; DROP TRIGGER product_result_legacy_hosts_no_insert; DROP TRIGGER product_result_legacy_hosts_no_update; DROP TRIGGER product_result_legacy_hosts_no_delete; DROP TRIGGER product_result_legacy_tuples_no_insert; DROP TRIGGER product_result_legacy_tuples_no_update; DROP TRIGGER product_result_legacy_tuples_no_delete; DROP TRIGGER product_review_authority_no_update; DROP TRIGGER product_review_authority_no_delete; DROP TRIGGER product_review_authority_no_replace; DROP TRIGGER product_review_authority_no_legacy; DROP TRIGGER product_result_bindings_no_update; DROP TRIGGER product_result_bindings_no_delete; DROP TRIGGER product_result_bindings_no_replace; DROP TRIGGER product_result_bindings_no_legacy_tuple; DROP TRIGGER product_approval_result_bindings_no_update; DROP TRIGGER product_approval_result_bindings_no_delete; DROP TRIGGER product_approval_result_bindings_no_replace; DROP TRIGGER product_tuples_no_update; DROP TRIGGER product_tuples_no_delete; DROP TRIGGER product_tuples_no_replace; DROP TABLE product_approval_result_bindings_v1; DROP TABLE product_result_bindings_v1; DROP TABLE product_review_authority_v1; DROP TABLE product_result_legacy_hosts_v1; DROP TABLE product_result_legacy_tuples_v1; DROP TABLE product_result_meta_v1; UPDATE product_meta_v1 SET schema_version=2;",
        )
        .unwrap();
    foundry_ledger::ProductStore::migrate_product_results_v2(&path, &fixture.config).unwrap();
    let cutoff: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT legacy_approval_cutoff_seq FROM product_result_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        cutoff, approval_seq,
        "cutoff equals MAX(seq) at migration time; boundary equality keeps the legacy approval covered"
    );
    // 4. INCLUDED side: replay returns the exact committed receipt through the
    //    legacy branch (no side binding, seq == cutoff, evidence complete).
    let mut reopened = configured_reopen(&fixture);
    let replay_proof = reopened
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    let replay = reopened
        .approve_product(&fixture.human_session_digest, &intent, &replay_proof)
        .unwrap();
    assert_eq!(serde_json::to_string(&replay).unwrap(), first_wire);
    // 5. Legacy evidence negative: consumed grant tampered -> the same replay
    //    is rejected as incomplete legacy evidence, not treated as absence.
    let grant: String = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT grant_id FROM product_operations_v1 WHERE operation_id=?1",
            [&operation_id],
            |r| r.get(0),
        )
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE product_grants_v1 SET state='revoked' WHERE grant_id=?1",
            [&grant],
        )
        .unwrap();
    let tampered_proof = reopened
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    match reopened.approve_product(&fixture.human_session_digest, &intent, &tampered_proof) {
        Err(foundry_ledger::ProductError::Invariant(message))
            if message.contains("legacy approval evidence is incomplete") => {}
        other => panic!(
            "tampered legacy evidence must reject with the incomplete-evidence invariant, got {other:?}"
        ),
    }
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE product_grants_v1 SET state='consumed' WHERE grant_id=?1",
            [&grant],
        )
        .unwrap();
    let restored_proof = reopened
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    let restored = reopened
        .approve_product(&fixture.human_session_digest, &intent, &restored_proof)
        .unwrap();
    assert_eq!(serde_json::to_string(&restored).unwrap(), first_wire);
    // 6. EXCLUDED side: cutoff lowered below the approval seq -> the same
    //    replay is rejected as a post-cutoff approval lacking side binding.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TRIGGER product_result_meta_no_update; DROP TRIGGER product_result_meta_no_delete; DROP TRIGGER product_result_meta_no_replace;",
        )
        .unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "UPDATE product_result_meta_v1 SET legacy_approval_cutoff_seq={}",
            cutoff - 1
        ))
        .unwrap();
    let excluded_proof = reopened
        .prepare_approval(&fixture.human_session_digest, &intent)
        .unwrap();
    match reopened.approve_product(&fixture.human_session_digest, &intent, &excluded_proof) {
        Err(foundry_ledger::ProductError::Invariant(message))
            if message.contains("post-cutoff approval lacks result side binding") => {}
        other => panic!(
            "post-cutoff replay without binding must reject with the post-cutoff invariant, got {other:?}"
        ),
    }
    drop(reopened);
    drop(fixture);
}

#[test]
fn sigkill_before_commit_rolls_back_and_after_commit_preserves_exact_set() {
    let executable = std::env::var_os("B1_LEDGER_TEST_EXE")
        .map(PathBuf::from)
        .expect("B1_LEDGER_TEST_EXE is required for lock-blocked COMMIT proof");
    assert!(executable.is_absolute() && executable.is_file());

    let mut before = genuine_result_finalized(genuine_host_done(FixtureIdentity::new(
        ProductConfig {
            installation_id: "result-approval-lock-install".into(),
            database_id: "result-approval-lock-before".into(),
        },
        "result-approval-lock-source",
        "result-approval-lock-order",
    )));
    let intent = before.approval.clone();
    let operation_id = before.approval_operation_id.clone().unwrap();
    let manifest = write_approval_manifest(&before, &intent, &operation_id, "blocked-commit");
    let db = before.db_path.clone();
    let order = before.identity.order_id.clone();
    let root = before.root.clone();
    drop(before.host.take());
    drop(before.result_store.take());

    let mut process = OwnedChild(Some(
        Command::new(&executable)
            .args(["--exact", APPROVAL_CHILD_TEST, "--nocapture"])
            .env("B1_PRODUCT_RESULT_CHILD_MANIFEST", &manifest)
            .env("B1_PRODUCT_KILL_BOUNDARY", "before_approval_commit")
            .env("B1_APPROVAL_COMMIT_OBSERVER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn owned commit-blocked approval child"),
    ));
    let stdout = process.child_mut().stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    let wait_line = |expected: &str| {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(line)) if line.trim() == expected => return,
                Ok(Ok(line)) => assert!(
                    !line.starts_with("B1_BOUNDARY:") || line.trim() == expected,
                    "child reached unexpected barrier: {line}"
                ),
                Ok(Err(error)) => panic!("child stdout read failed: {error}"),
                Err(mpsc::RecvTimeoutError::Timeout) => panic!("missing child marker {expected}"),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("child exited before {expected}")
                }
            }
        }
    };
    wait_line("B1_BOUNDARY:before_approval_commit");
    // Acquire a real SHARED reader after the child has completed its write
    // participants and paused immediately before COMMIT. This avoids blocking
    // the child's strict startup/schema checks while still preventing COMMIT.
    let reader = Connection::open(&db).unwrap();
    reader.busy_timeout(Duration::ZERO).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let _: i64 = reader
        .query_row("SELECT count(*) FROM ledger", [], |row| row.get(0))
        .unwrap();
    process
        .child_mut()
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"commit")
        .unwrap();
    process.child_mut().stdin.as_mut().unwrap().flush().unwrap();
    wait_line("B1_APPROVAL_COMMIT_ATTEMPT");

    // The callback token is emitted immediately before Transaction::commit.
    // The child already owns the RESERVED write lock; the real reader above
    // prevents its EXCLUSIVE commit lock. Verify a competing writer is busy
    // and the child cannot finish while that reader remains open.
    let probe = Connection::open(&db).unwrap();
    probe.busy_timeout(Duration::ZERO).unwrap();
    assert!(matches!(
        probe.execute_batch("BEGIN IMMEDIATE"),
        Err(rusqlite::Error::SqliteFailure(code, _))
            if code.code == rusqlite::ErrorCode::DatabaseBusy
                || code.code == rusqlite::ErrorCode::DatabaseLocked
    ));
    let blocked_until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < blocked_until {
        assert!(
            process.child_mut().try_wait().unwrap().is_none(),
            "approval child completed COMMIT despite retained reader lock"
        );
        std::thread::yield_now();
    }
    process
        .child_mut()
        .kill()
        .expect("kill only owned approval child");
    let status = process.finish();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9));
    }
    reader.execute_batch("ROLLBACK").unwrap();
    drop(probe);
    assert_eq!(product_result_counts(&db, &order), (1, 1, 4, 4, 3, 4));
    assert_eq!(
        approval_participants(&db, &intent, &operation_id),
        (0, 0, 0, 0, 0, 0, "reserved".into(), "prepared".into())
    );
    let mut reopened = configured_reopen(&before);
    let proof = reopened
        .prepare_approval(&before.human_session_digest, &intent)
        .unwrap();
    let committed = reopened
        .approve_product(&before.human_session_digest, &intent, &proof)
        .unwrap();
    let committed_wire = serde_json::to_string(&committed).unwrap();
    assert_eq!(
        approval_receipt_wire(&db, &operation_id).as_deref(),
        Some(committed_wire.as_str())
    );
    assert_eq!(
        approval_participants(&db, &intent, &operation_id),
        (1, 1, 1, 1, 1, 1, "consumed".into(), "completed".into())
    );
    drop(reopened);
    drop(before);
    std::fs::remove_dir_all(root).unwrap();

    // The postcommit-before-response SIGKILL is exercised independently by
    // genuine_approval_child_sigkill_restores_every_participant_boundary.
}

#[test]
fn two_process_stage_start_then_service_revocation_preserves_completed_result_and_blocks_finalizer()
{
    let order = "result-process-revoke-after-stage-start";
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("{order}-install"),
            database_id: format!("{order}-db"),
        },
        format!("{order}-source"),
        order,
    )
    .with_script(ReviewScript::ResumePass);
    let mut fixture = genuine_host_done(identity);
    grant_review_hold_scope(&fixture);
    let mut owner = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    let hold = owner
        .list_product_review_holds(&fixture.human_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == order)
        .unwrap();
    let acceptance = owner
        .accept_product_review_hold(
            &fixture.human_session_digest,
            order,
            &hold.hold_instance_id,
            &hold.expectation,
            "result-process-revoke-accept",
            ProductHoldDecision::Resume("replan_full".into()),
            fixture.now,
        )
        .unwrap();
    drop(owner);
    drop(fixture.host.take());

    let started_path = fixture.root.join("continuation-stage-started");
    let release_path = fixture.root.join("continuation-stage-release");
    let execute_count_path = fixture.root.join("continuation-execute-count");
    std::fs::write(&execute_count_path, "0").unwrap();
    let result_path = fixture.root.join("continuation-child-result.json");
    let manifest = ContinuationProcessManifestV1 {
        db_path: fixture.db_path.clone(),
        installation_id: fixture.config.installation_id.clone(),
        database_id: fixture.config.database_id.clone(),
        artifact_root: fixture.artifact_root.clone(),
        session_root: fixture.session_root.clone(),
        claim_root: fixture.claim_root.clone(),
        human_session_digest: fixture.human_session_digest.clone(),
        operation_id: acceptance.operation_id.clone(),
        now: fixture.now.to_rfc3339(),
        started_path: started_path.clone(),
        release_path: release_path.clone(),
        execute_count_path: execute_count_path.clone(),
        result_path: result_path.clone(),
    };
    let manifest_path = fixture.root.join("continuation-process-manifest.json");
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    write_private(
        &manifest_path,
        std::str::from_utf8(&manifest_bytes).unwrap(),
    );
    let executable = std::env::current_exe().unwrap();
    let child = Command::new(executable)
        .args([
            "--exact",
            "product_result_continuation_child",
            "--nocapture",
        ])
        .env("B1_RESULT_CONTINUATION_MANIFEST", &manifest_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn a separate test process sharing the genuine result database");
    wait_for_child_file(&started_path, "the committed run-8 stage-start marker");
    let started: (String, String, String, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT r.status,
                    s.status,
                    a.journal_run,
                    (SELECT used FROM product_review_budget_v1 WHERE order_id=?1)
             FROM product_review_runs_v1 r
             JOIN product_review_stages_v1 s ON s.order_id=r.order_id AND s.run_id=r.run_id
             JOIN product_review_active_v1 a ON a.order_id=r.order_id
             WHERE r.order_id=?1 AND r.run_id=?2 AND s.stage_id='replan_full-g1'",
            rusqlite::params![order, format!("{order}:1:8:3")],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        started,
        (
            "resuming".into(),
            "accepted".into(),
            format!("{order}:1:8:3"),
            5
        ),
        "the child reaches real stage execution only after the stage-start transaction commits"
    );
    assert_eq!(
        std::fs::read_to_string(&execute_count_path).unwrap(),
        "1",
        "exactly one executor call is in flight at the committed stage-start barrier"
    );
    let service_revision: i64 = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_services_v1 WHERE service_id=?1",
            [&fixture.identity.service_id],
            |r| r.get(0),
        )
        .unwrap();
    let mut revoke = ProductStore::open(&fixture.db_path, fixture.config.clone()).unwrap();
    revoke
        .manage_product_service(
            &fixture.owner_session_digest,
            &fixture.identity.service_id,
            &[],
            false,
            fixture.now + chrono::Duration::hours(2),
            service_revision,
            "result-process-revoke-after-start",
            fixture.now,
        )
        .unwrap();
    drop(revoke);
    std::fs::write(&release_path, b"continue-after-revoke").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "continuation child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("test product_result_continuation_child ... ok")
    );
    let child_result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&result_path).unwrap()).unwrap();
    assert!(
        child_result["result"]
            .as_str()
            .unwrap()
            .contains("review service authority")
    );
    assert_eq!(
        std::fs::read_to_string(&execute_count_path).unwrap(),
        "1",
        "the in-flight stage is the only execute call; after revocation next execute=0"
    );

    let preserved: (i64, i64, i64, i64, i64, String, String) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id='replan_full-g1' AND status='complete'),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_sessions_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND run_id=?2),
                    (SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?3)",
            rusqlite::params![order, format!("{order}:1:8:3"), acceptance.operation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .unwrap();
    assert_eq!(
        preserved,
        (1, 5, 4, 0, 0, "resuming".into(), "recovery_required".into())
    );
    let followup = foundry_ledger::ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            run: 8,
            ..fixture.expected.host_key.clone()
        },
        host_result_digest: fixture.expected.host_result_digest.clone(),
    };
    assert_eq!(
        fixture
            .result_store
            .as_mut()
            .unwrap()
            .finalize_product_review_result(&fixture.service, &followup),
        Err(foundry_ledger::ProductResultError::Unauthorized),
        "strict finalization on the same database refuses the revoked process result"
    );
    let no_effect: (i64, i64, i64, i64) = Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM artifact_tuples WHERE order_id=?1),
                    (SELECT count(*) FROM product_result_bindings_v1 WHERE order_id=?1),
                    (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND status='complete'),
                    (SELECT count(*) FROM product_artifacts_v1 WHERE order_id=?1)",
            [order],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(no_effect, (0, 0, 5, 5));
    drop(fixture.result_store.take());
    std::fs::remove_dir_all(fixture.root).unwrap();
}
