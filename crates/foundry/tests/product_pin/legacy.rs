use super::*;
use foundry_exec::ReviewDispatchStore;
use foundry_exec::review_run::{
    ReviewStageEvidence, ReviewStageFinish, ReviewStageFinishRequest, ReviewStageStart,
};
use foundry_ledger::{ProductCapability, ProductConfig, ScopeMutation};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

pub(super) fn v3(orders: &[&str]) -> RuntimeFixture {
    let preimage = std::path::PathBuf::from(std::env::var_os("W1_V3_PREIMAGE").unwrap());
    RuntimeFixture::with_preimage(
        orders,
        Some((
            &preimage,
            ProductConfig {
                installation_id: "w1-genuine-v3-install".into(),
                database_id: "w1-genuine-v3-database".into(),
            },
        )),
    )
}

fn install_history(db: &Connection, order: &str, history: &str, authority: bool, other: &str) {
    match history {
        "single" => (),
        "mixed" => {
            // A synthetic earlier run in this same lifecycle; keep the real current run unique.
            assert_eq!(db.execute("INSERT INTO product_review_runs_v1(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,product_lifecycle,source_revision,admission_seq,admission_hash,created_at)
                SELECT order_id,attempt,?2,base_commit,result_commit,?3,'resolved','{}',product_lifecycle,source_revision,admission_seq,admission_hash,created_at-1 FROM product_review_runs_v1 WHERE order_id=?1",params![order,format!("{order}:1:1:1"),other]).unwrap(),1);
        }
        "none" => assert_eq!(
            db.execute(
                "UPDATE product_review_runs_v1 SET pack_digest='',provenance_json=NULL WHERE order_id=?1",
                [order]
            )
            .unwrap(),
            1
        ),
        _ => unreachable!(),
    }
    if !authority {
        assert_eq!(
            db.execute(
                "DELETE FROM product_service_work_bindings_v1 WHERE order_id=?1",
                [order]
            )
            .unwrap(),
            1
        );
    }
}

fn disturb_snapshot(path: &Path, condition: &str, original: &str) {
    assert_eq!(foundry_pack::load_pack(path).unwrap().digest(), original);
    match condition {
        "verified" => (),
        "absent" => fs::rename(path, path.with_file_name("retained-original")).unwrap(),
        "tampered" => {
            fs::write(path.join("policy.toml"), b"# changed after reservation\n").unwrap()
        }
        _ => unreachable!(),
    }
    assert_eq!(
        foundry_pack::load_pack(path).is_ok(),
        condition == "verified"
    );
}

fn activation_denials(f: &RuntimeFixture, order: &str) -> Vec<String> {
    let mut store = f.configured_review_store(manifest_digest(&f.pack));
    let mut messages = Vec::new();
    for epoch in [1, 2] {
        let mut request = admission_request(f, order);
        request.intent.run = RunId(99);
        request.intent.epoch = Epoch(epoch);
        let before = snapshot(&f.db_path);
        let err = store.admit_review_activation(request).unwrap_err();
        assert_snapshot_unchanged(&before, &f.db_path);
        messages.push(format!("{err:?}"));
    }
    messages
}

fn matrix_cell(history: &str, authority: bool, condition: &str) -> serde_json::Value {
    let f = v3(&["active", "held"]);
    let held = f.make_hold(
        "held",
        FailingExecutor {
            calls: Arc::default(),
        },
    );
    let receipt = f.accept_resume(&held, "original");
    let digest = manifest_digest(&f.pack);
    let (_, other) = y_pack_with_changed_lenses(&f.pack, &f.root.0.join("other-pack"));
    let mut store = f.configured_review_store(digest.clone());
    let active = admission_request(&f, "active");
    let intent = active.intent.clone();
    let run = store.admit_review_activation(active).unwrap().journal_run;
    store
        .start_review_stage(ReviewStageStart {
            intent: intent.clone(),
            journal_run: run.clone(),
            sequence: 1,
            stage_id: "review".into(),
            budget_limit: 10,
        })
        .unwrap();
    let output = f.artifact_root().join("reserved-output.txt");
    fs::write(&output, b"sealed stage result").unwrap();
    let hash = hex::encode(Sha256::digest(fs::read(&output).unwrap()));
    let active_dir = f
        .artifact_root()
        .join(hex::encode(Sha256::digest(run.as_bytes())))
        .join("pack-snapshot");
    let pack = foundry_pack::load_pack(&f.pack).unwrap();
    fs::create_dir_all(&active_dir).unwrap();
    for name in pack
        .files()
        .keys()
        .map(String::as_str)
        .chain(["manifest.toml"])
    {
        let dest = active_dir.join(name);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(f.pack.join(name), dest).unwrap();
    }
    let db = Connection::open(&f.db_path).unwrap();
    for (order, path) in [("active", active_dir), ("held", f.snapshot_dir(&held))] {
        install_history(&db, order, history, authority, &other);
        disturb_snapshot(&path, condition, &digest);
        let view = store
            .read_scoped_order(&f.owner_session, order, f.now)
            .unwrap();
        assert_eq!(view.admission_sequence, f.item(order).admission.sequence);
    }
    let before = snapshot(&f.db_path);
    assert_eq!(receipt, f.accept_resume(&held, "original"));
    let progress = store
        .product_review_progress(&f.owner_session, &receipt.operation_id, f.now)
        .unwrap();
    assert_eq!(progress.receipt, receipt);
    assert_eq!(progress.phase, "accepted");
    assert_snapshot_unchanged(&before, &f.db_path);
    let active_denials = activation_denials(&f, "active");
    let held_denials = activation_denials(&f, "held");
    let before = snapshot(&f.db_path);
    let collection = store.finish_review_stage(ReviewStageFinishRequest {
        intent,
        journal_run: run,
        sequence: 1,
        stage_id: "review".into(),
        finish: ReviewStageFinish::Complete(ReviewStageEvidence {
            role: "lens".into(),
            log_path: output.to_str().unwrap().into(),
            sha256: hash.clone(),
            inspected_commit: None,
            findings: false,
            verdicts: vec![],
            input_manifest: None,
            deferred: false,
            produced_commit: None,
            session: None,
        }),
    });
    if collection.is_ok() {
        let registered:(String,String)=db.query_row("SELECT s.status,a.artifact_sha256 FROM product_review_stages_v1 s JOIN product_artifacts_v1 a USING(order_id,run_id,stage_id) WHERE s.order_id='active'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(registered, ("complete".into(), hash));
    } else {
        assert_snapshot_unchanged(&before, &f.db_path);
    }
    let before = snapshot(&f.db_path);
    let calls = Arc::new(AtomicUsize::new(0));
    let outcome = f
        .host(SuccessExecutor {
            calls: calls.clone(),
            lens_bytes: None,
            after_execute: None,
            after_preflight: None,
        })
        .continue_accepted_product_hold(&f.owner_session, &receipt.operation_id, f.now);
    let after = snapshot(&f.db_path);
    let changed: Vec<_> = after
        .iter()
        .filter(|(t, rows)| before.get(*t) != Some(*rows))
        .map(|(t, _)| t.as_str())
        .collect();
    let phase = store
        .product_review_progress(&f.owner_session, &receipt.operation_id, f.now)
        .unwrap();
    assert_eq!(phase.receipt, receipt);
    let executions = calls.load(Ordering::SeqCst);
    if executions == 0 {
        assert_snapshot_unchanged(&before, &f.db_path);
    }
    serde_json::json!({"history":history,"authority":authority,"snapshot":condition,"scoped_reads":2,"exact_receipt":receipt.operation_id,"active_activation_denials":active_denials,"held_activation_denials":held_denials,"reserved_output":collection.is_ok(),"collection_error":collection.err().map(|e|format!("{e:?}")),"resume_ok":outcome.is_ok(),"resume_error":outcome.err().map(|e|format!("{e:?}")),"phase":phase.phase,"executor_calls":executions,"changed_tables":changed})
}

fn check_history(history: &str) {
    let mut mismatches = Vec::new();
    for authority in [true, false] {
        for condition in ["verified", "absent", "tampered"] {
            let row = matrix_cell(history, authority, condition);
            println!("P07 {row}");
            let permitted = history != "none" && authority && condition == "verified";
            // The active reservation must still carry a verifiable original run/pack binding.
            if row["reserved_output"] != (history != "none")
                || row["resume_ok"] != permitted
                || (row["executor_calls"].as_u64().unwrap() > 0) != permitted
            {
                mismatches.push(row);
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "frozen legacy expectations diverged: {mismatches:?}"
    );
}

#[test]
fn single_digest_history() {
    check_history("single");
}
#[test]
fn mixed_digest_history() {
    check_history("mixed");
}
#[test]
fn missing_digest_history() {
    check_history("none");
}

#[test]
fn historical_actor_and_scope_controls() {
    let f = v3(&["order-a"]);
    let held = f.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::default(),
        },
    );
    let receipt = f.accept_resume(&held, "actor-key");
    let mut store = f.store();
    let phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("fixture-password").unwrap();
    store
        .create_account(&f.owner_session, "peer", "approver", &phc, f.now)
        .unwrap();
    for (rev, cap) in [
        ProductCapability::ReadOrder,
        ProductCapability::ReadReview,
        ProductCapability::ResolveReviewHold,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .change_product_scope(
                &f.owner_session,
                "peer",
                "b2-w1-source",
                cap,
                ScopeMutation::Grant,
                rev as i64,
                &format!("grant-peer-{rev}"),
                f.now,
            )
            .unwrap();
    }
    let peer = store.login("peer", "fixture-password", f.now).unwrap();
    let before = snapshot(&f.db_path);
    assert!(
        store
            .read_scoped_order(peer.digest(), "order-a", f.now)
            .is_ok()
    );
    assert!(
        store
            .product_review_progress(peer.digest(), &receipt.operation_id, f.now)
            .is_err()
    );
    assert!(
        store
            .accept_product_review_hold(
                peer.digest(),
                "order-a",
                &held.hold_instance_id,
                &held.expected,
                "actor-key",
                ProductHoldDecision::Resume(held.resume_target.clone()),
                f.now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &f.db_path);
    let original = store
        .read_scoped_order(&f.owner_session, "order-a", f.now)
        .unwrap();
    store
        .change_product_scope(
            &f.owner_session,
            "owner",
            "b2-w1-source",
            ProductCapability::ReadOrder,
            ScopeMutation::Revoke,
            original.scope_revision,
            "revoke-order-read",
            f.now,
        )
        .unwrap();
    let before = snapshot(&f.db_path);
    assert!(
        store
            .read_scoped_order(&f.owner_session, "order-a", f.now)
            .is_err()
    );
    assert!(
        store
            .accept_product_review_hold(
                &f.owner_session,
                "order-a",
                &held.hold_instance_id,
                &held.expected,
                "actor-key",
                ProductHoldDecision::Resume(held.resume_target),
                f.now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &f.db_path);
}

/// The historical process and private Git repository are supplied by the offline
/// experiment driver. Only the test harness prepares the initial B1 work.
#[test]
#[ignore = "requires the archived writer, private repository and experiment driver"]
fn historical_writer_fixture() {
    let f = v3(&["historical", "new-order"]);
    let mut accounts = f.store();
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash("fixture-reader-password")
        .unwrap();
    accounts
        .create_account(&f.owner_session, "artifact-reader", "approver", &phc, f.now)
        .unwrap();
    accounts
        .change_product_scope(
            &f.owner_session,
            "artifact-reader",
            "b2-w1-source",
            ProductCapability::ReadArtifact,
            ScopeMutation::Grant,
            0,
            "read-historical-output",
            f.now,
        )
        .unwrap();
    let artifact_reader = accounts
        .login("artifact-reader", "fixture-reader-password", f.now)
        .unwrap();
    drop(accounts);
    let commit = std::env::var("W1_HISTORY_COMMIT").unwrap();
    let held = f.make_hold_for_commits(
        "historical",
        FailingExecutor {
            calls: Arc::default(),
        },
        &commit,
        &commit,
    );
    let session = f.root.0.join("owner.session");
    fs::write(&session, format!("{}\n", f.owner_bearer)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&session, fs::Permissions::from_mode(0o600)).unwrap();
    let script = std::env::var_os("W1_HISTORY_DRIVER").unwrap();
    let status = std::process::Command::new("python3")
        .env("W1_MIGRATION_WORKER", std::env::current_exe().unwrap())
        .arg(script)
        .arg(&f.root.0)
        .arg(&f.pack)
        .arg(&held.hold_instance_id)
        .arg(&held.resume_target)
        .status()
        .unwrap();
    assert!(status.success());
    if std::env::var_os("W1_HISTORY_MIGRATE").is_some() {
        current_writer_control(&f);
        let archive = std::env::var_os("W1_HISTORY_ARCHIVE").unwrap();
        assert!(
            std::process::Command::new("python3")
                .arg(archive)
                .arg(&f.db_path)
                .status()
                .unwrap()
                .success()
        );
        let report_root = std::path::PathBuf::from(std::env::var_os("W1_HISTORY_OUTPUT").unwrap());
        let mut restored = foundry_ledger::ProductStore::open(
            &report_root.join("current-restore.sqlite"),
            f.config.clone(),
        )
        .unwrap();
        restored
            .configure_product_artifact_root(f.artifact_root())
            .unwrap();
        let records = restored
            .list_product_artifacts(artifact_reader.digest(), "historical", chrono::Utc::now())
            .unwrap();
        assert_eq!(records.len(), 1);
        for record in records {
            let content = restored
                .read_product_artifact(
                    artifact_reader.digest(),
                    "historical",
                    &record.artifact_id,
                    chrono::Utc::now(),
                )
                .unwrap();
            assert_eq!(hex::encode(Sha256::digest(&content.bytes)), record.sha256);
            fs::write(report_root.join("reserved-stage-output"), content.bytes).unwrap();
            fs::write(
                report_root.join("verified-output.json"),
                serde_json::to_vec_pretty(&content.info).unwrap(),
            )
            .unwrap();
        }
    }
}

#[test]
#[ignore = "migration operand run by historical_writer_fixture"]
fn migrate_private_copy() {
    let root = std::path::PathBuf::from(std::env::var_os("W1_MIGRATION_ROOT").unwrap());
    let barrier = std::path::PathBuf::from(std::env::var_os("W1_MIGRATION_BARRIER").unwrap());
    let mut store = foundry_ledger::ProductStore::open(
        &root.join("product.sqlite"),
        ProductConfig {
            installation_id: "w1-genuine-v3-install".into(),
            database_id: "w1-genuine-v3-database".into(),
        },
    )
    .unwrap();
    let pause = |when: &str| {
        fs::write(barrier.join(when), b"").unwrap();
        if std::env::var("W1_MIGRATION_PAUSE").unwrap() == when {
            let start = std::time::Instant::now();
            while !barrier.join("release").exists() {
                assert!(start.elapsed() < Duration::from_secs(45));
                std::thread::sleep(Duration::from_millis(15));
            }
        }
    };
    store
        .migrate_pack_pin_test_copy(|| pause("before"))
        .unwrap();
    pause("after");
}

fn current_writer_control(f: &RuntimeFixture) {
    let digest = manifest_digest(&f.pack);
    let mut store = f.configured_review_store(digest.clone());
    store
        .change_pack_approval(
            &f.owner_session,
            "b2-w1-source",
            &digest,
            0,
            true,
            "approve-after-migration",
            f.now,
        )
        .unwrap();
    store
        .select_order_pack(
            &f.owner_session,
            "new-order",
            1,
            "b2-w1-source",
            &digest,
            0,
            "select-after-migration",
            f.now,
        )
        .unwrap();
    let request = selected_request(
        f,
        "new-order",
        selection_binding(f, "new-order"),
        "first-new-key",
    );
    let admitted = store
        .admit_selected_review_activation(request.clone())
        .unwrap();
    assert_eq!(count(&f.db_path, "product_w1_lifecycle_pins_v1"), 1);
    let before = snapshot(&f.db_path);
    assert_eq!(
        store
            .admit_selected_review_activation(request.clone())
            .unwrap()
            .journal_run,
        admitted.journal_run
    );
    assert_snapshot_unchanged(&before, &f.db_path);
    let mut start = ReviewStageStart {
        intent: request.review.intent,
        journal_run: admitted.journal_run,
        sequence: 1,
        stage_id: "control".into(),
        budget_limit: 10,
    };
    store.start_review_stage(start.clone()).unwrap();
    store
        .change_pack_approval(
            &f.owner_session,
            "b2-w1-source",
            &digest,
            1,
            false,
            "revoke-new-work",
            f.now,
        )
        .unwrap();
    start.sequence = 2;
    start.stage_id = "after-revoke".into();
    let before = snapshot(&f.db_path);
    let denied = store.start_review_stage(start).unwrap_err();
    assert!(
        matches!(denied, foundry_exec::HostError::Acceptance(_)),
        "{denied:?}"
    );
    assert_snapshot_unchanged(&before, &f.db_path);
    println!(
        "current writer pinned and admitted new-order, exact retry preserved all rows, next stage after revoke: {denied:?}"
    );
}

struct ArchivedWriter(std::process::Child);

impl Drop for ArchivedWriter {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn await_reply(path: &Path, child: &mut ArchivedWriter) {
    let start = std::time::Instant::now();
    while !path.exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "archived operand exited before {path:?}"
        );
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "missing archived reply at {path:?}"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
}

struct WriterControl<'a> {
    fixture: &'a RuntimeFixture,
    child: ArchivedWriter,
    turn: usize,
    reclaimed: Option<foundry_ledger::ProductWorkClaim>,
}

impl WriterControl<'_> {
    fn probe(&mut self, order: &str, action: &str, operation: Option<&str>, deny: bool) {
        let f = self.fixture;
        let claim = self
            .reclaimed
            .as_ref()
            .filter(|c| c.order_id() == order)
            .unwrap_or(&f.item(order).claim);
        let work = claim.review_binding();
        let service = work.service_binding.as_ref().unwrap();
        let mut intent = intent(f, order);
        if order == "different" {
            intent.run = RunId(77);
        }
        let job = serde_json::json!({
            "order":order,"action":action,"operation":operation,"intent":intent,
            "lifecycle":work.lifecycle,"delivery":work.delivery_id,"worker":work.worker_id,
            "epoch":work.epoch,"fence":work.fence,"source_revision":work.source_revision,
            "payload_digest":work.payload_digest,"lease_until":work.lease_until,"kind":work.task_kind,
            "payload":work.payload_snapshot,"service":service.service_id,"source":service.source_instance_id,
            "service_revision":service.service_revision,"credential_revision":service.credential_revision,
        });
        let before = snapshot(&f.db_path);
        fs::write(f.root.0.join("pending"), serde_json::to_vec(&job).unwrap()).unwrap();
        fs::rename(
            f.root.0.join("pending"),
            f.root.0.join(format!("command-{}", self.turn)),
        )
        .unwrap();
        let reply = f.root.0.join(format!("reply-{}", self.turn));
        await_reply(&reply, &mut self.child);
        let actual = fs::read_to_string(reply).unwrap();
        if deny {
            assert!(
                actual.contains("no such function: foundry_pack_writer_version"),
                "{order}/{action}: {actual}"
            );
            assert_snapshot_unchanged(&before, &f.db_path);
        } else {
            assert_eq!(actual, "committed", "{order}/{action}");
            assert_ne!(before, snapshot(&f.db_path));
        }
        println!(
            "ARCHIVED order={order} action={action} pid={} tables={} reply={actual}",
            self.child.0.id(),
            before.len()
        );
        self.turn += 1;
    }
}

#[test]
#[ignore = "requires the operand linked against the archived ProductStore"]
fn held_old_connection_pin_combinations() {
    let f = v3(&["old-control", "empty", "different", "matching", "resume"]);
    let x = manifest_digest(&f.pack);
    let (_, y) = y_pack_with_changed_lenses(&f.pack, &f.root.0.join("other"));
    fs::write(f.root.0.join("owner.digest"), &f.owner_session).unwrap();
    fs::write(f.root.0.join("fixed-digest"), &x).unwrap();
    let child = std::process::Command::new(std::env::var_os("W1_ARCHIVED_OPERAND").unwrap())
        .arg(&f.root.0)
        .spawn()
        .unwrap();
    let mut old = WriterControl {
        fixture: &f,
        child: ArchivedWriter(child),
        turn: 0,
        reclaimed: None,
    };
    await_reply(&f.root.0.join("old-ready"), &mut old.child);
    old.probe("old-control", "admit", None, false);
    old.probe("old-control", "stage", None, false);
    f.store().migrate_pack_pin_test_copy(|| {}).unwrap();
    let packs = registry(&x, &f.pack, &y, &f.root.0.join("other"));
    let mut manager = f.store().with_product_registry(packs.clone());
    for (digest, key) in [(&x, "x"), (&y, "y")] {
        manager
            .change_pack_approval(
                &f.owner_session,
                "b2-w1-source",
                digest,
                0,
                true,
                key,
                f.now,
            )
            .unwrap();
    }
    for (order, digest) in [("different", &y), ("matching", &x), ("resume", &x)] {
        manager
            .select_order_pack(
                &f.owner_session,
                order,
                1,
                "b2-w1-source",
                digest,
                0,
                order,
                f.now,
            )
            .unwrap();
    }
    let mut binding = selection_binding(&f, "different");
    binding.pack_digest = y.clone();
    let mut host = PackReviewHost::with_selected_product_pack(
        f.root.0.join("other"),
        FinalPassExecutor {
            calls: Arc::default(),
            revoke_after_final_execute: Arc::new(|_| {}),
        },
        f.store().with_product_registry(packs),
        f.artifact_root(),
        f.session_root(),
        f.claim_root(),
    )
    .unwrap()
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .unwrap();
    let completed = host
        .try_dispatch_selected_product_work(
            &intent(&f, "different"),
            &f.item("different").claim,
            binding,
            "y-pin",
        )
        .unwrap();
    assert!(completed.held.is_none() && completed.exit_code == 0);
    let foundry_ledger::ProductWorkClaimResult::Claimed(reclaimed) = manager
        .claim_product_work_for_service(
            &f.service,
            "different-retry",
            chrono::Utc::now(),
            chrono::Duration::minutes(5),
        )
        .unwrap()
    else {
        panic!("expected the completed order to be claimable")
    };
    assert_eq!(reclaimed.order_id(), "different");
    old.reclaimed = Some(reclaimed);

    let mut store = f.configured_review_store(x.clone());
    store
        .admit_selected_review_activation(selected_request(
            &f,
            "matching",
            selection_binding(&f, "matching"),
            "x-pin",
        ))
        .unwrap();
    let mut host = f.host(FailingExecutor {
        calls: Arc::default(),
    });
    let held = host
        .try_dispatch_selected_product_work(
            &intent(&f, "resume"),
            &f.item("resume").claim,
            selection_binding(&f, "resume"),
            "resume-pin",
        )
        .unwrap()
        .held
        .unwrap();
    assert_eq!(count(&f.db_path, "product_w1_lifecycle_pins_v1"), 3);
    let db = Connection::open(&f.db_path).unwrap();
    for (order, digest) in [("different", &y), ("matching", &x), ("resume", &x)] {
        assert_eq!(
            db.query_row(
                "SELECT pack_digest FROM product_w1_lifecycle_pins_v1 WHERE order_id=?1",
                [order],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            *digest
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM product_w1_lifecycle_pins_v1 WHERE order_id='empty'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    old.probe("empty", "admit", None, true);
    old.probe("different", "admit", None, true);
    old.probe("matching", "stage", None, true);
    old.probe("resume", "accept", None, true);
    let receipt = manager
        .accept_product_review_hold(
            &f.owner_session,
            "resume",
            &held.hold_instance_id,
            &b2_w1::hold_expectation(&f.db_path, "resume", &held.hold_instance_id),
            "current-decision",
            ProductHoldDecision::Resume(held.resume_targets[0].clone()),
            f.now,
        )
        .unwrap();
    old.probe("resume", "continue", Some(&receipt.operation_id), true);
    assert!(old.child.0.wait().unwrap().success());
}

#[test]
fn lifecycle_and_new_order_controls() {
    let f = RuntimeFixture::new(&["held", "fresh"]);
    let _held = f.make_hold(
        "held",
        FailingExecutor {
            calls: Arc::default(),
        },
    );
    let digest = manifest_digest(&f.pack);
    let mut store = f.configured_review_store(digest.clone());
    let before = snapshot(&f.db_path);
    let replay = store
        .admit_intake_delivery_for_service(&f.service, &f.item("held").delivery_id, "held", f.now)
        .unwrap();
    assert_eq!(replay, f.item("held").admission);
    assert_snapshot_unchanged(&before, &f.db_path);
    let err = store
        .admit_order(
            "held",
            "github-primary",
            "repo:1",
            "new-issue",
            "dep-bump",
            "new task",
            f.now,
        )
        .unwrap_err();
    assert_snapshot_unchanged(&before, &f.db_path);
    assert!(
        store
            .select_order_pack(
                &f.owner_session,
                "held",
                2,
                "b2-w1-source",
                &digest,
                0,
                "next-selection",
                f.now
            )
            .is_err()
    );
    assert_snapshot_unchanged(&before, &f.db_path);
    let mut next = selected_request(&f, "held", selection_binding(&f, "held"), "next-activation");
    next.binding.lifecycle = 2;
    next.review.intent.epoch = Epoch(2);
    assert!(store.admit_selected_review_activation(next).is_err());
    assert_snapshot_unchanged(&before, &f.db_path);
    let new = store
        .admit_selected_review_activation(selected_request(
            &f,
            "fresh",
            selection_binding(&f, "fresh"),
            "fresh-key",
        ))
        .unwrap();
    assert_eq!(count(&f.db_path, "product_w1_lifecycle_pins_v1"), 2);
    println!(
        "same-order replay lifecycle={} other issue error={err:?} fresh-order run={}",
        replay.lifecycle, new.journal_run
    );
}

#[test]
fn migration_profile_scope_and_damaged_fence() {
    let fresh = RuntimeFixture::new(&["fresh"]);
    let original = snapshot(&fresh.db_path);
    assert!(
        fresh
            .store()
            .migrate_pack_pin_test_copy(|| panic!("unexpected migration"))
            .is_err()
    );
    assert_snapshot_unchanged(&original, &fresh.db_path);
    let f = v3(&["legacy"]);
    f.store().migrate_pack_pin_test_copy(|| {}).unwrap();
    let db = Connection::open(&f.db_path).unwrap();
    db.execute_batch("DROP TRIGGER product_pack_writer_stage")
        .unwrap();
    let damaged = snapshot(&f.db_path);
    assert!(matches!(
        foundry_ledger::ProductStore::open(&f.db_path, f.config.clone()),
        Err(foundry_ledger::ProductError::ContractMismatch)
    ));
    assert_snapshot_unchanged(&damaged, &f.db_path);
}

#[test]
fn resolved_single_digest_can_admit_new_epoch_under_live_b1_claim() {
    let f = v3(&["legacy"]);
    let mut host = f.host(FinalPassExecutor {
        calls: Arc::default(),
        revoke_after_final_execute: Arc::new(|_| {}),
    });
    let result = host
        .try_dispatch_product_work(&intent(&f, "legacy"), &f.item("legacy").claim)
        .unwrap();
    assert_eq!(result.exit_code, 0);
    assert!(result.held.is_none());
    let mut store = f.configured_review_store(manifest_digest(&f.pack));
    let foundry_ledger::ProductWorkClaimResult::Claimed(claim) = store
        .claim_product_work_for_service(
            &f.service,
            "replacement",
            chrono::Utc::now(),
            chrono::Duration::minutes(5),
        )
        .unwrap()
    else {
        panic!("expected a terminal order reclaim")
    };
    let mut next = admission_request(&f, "legacy");
    next.intent.epoch = Epoch(2);
    next.intent.run = RunId(19);
    next.product_work = Some(claim.review_binding());
    let admitted = store.admit_review_activation(next).unwrap();
    assert_eq!(admitted.journal_run, "legacy:1:19:1");
    assert_eq!(count(&f.db_path, "product_review_host_results_v1"), 1);
    assert_eq!(count(&f.db_path, "product_review_runs_v1"), 2);
}
