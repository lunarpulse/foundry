#![cfg(feature = "p2-test-support")]

#[path = "../src/main.rs"]
mod application;
#[path = "support/b2_w1.rs"]
mod b2_w1;

use b2_w1::{
    FailingExecutor, RuntimeFixture, SuccessExecutor, assert_snapshot_unchanged,
    assert_state_and_schema_unchanged, copy_y_lenses_into_snapshot, count, hold_id,
    manifest_digest, request_body, schema_snapshot, snapshot, y_pack_with_changed_lenses,
};
use foundry_ledger::ProductHoldDecision;
use foundry_line::PackReviewHost;
use std::{
    fmt::Display,
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

fn assert_redacted_error(error: impl Display, fixture: &RuntimeFixture) {
    let message = error.to_string();
    assert!(!message.contains(&fixture.root.0.to_string_lossy().to_string()));
    assert!(!message.contains(&fixture.service_token));
    assert!(!message.contains("repo:outside"));
    assert!(!message.contains(fixture.service.source_instance_id()));
}

#[test]
fn w1_failure_root_custody_preserves_armed_panic_and_default_cleanup() {
    let default_path = {
        let root = b2_w1::PrivateRoot::new();
        root.0.clone()
    };
    assert!(
        !default_path.exists(),
        "default successful fixture cleanup changed"
    );

    let mut retained_path = None;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut root = b2_w1::PrivateRoot::new();
        retained_path = Some(root.arm_failure_preservation());
        fs::write(root.0.join("failure-evidence"), b"retained\n").unwrap();
        panic!("intentional W1-b custody probe");
    }));
    assert!(panic.is_err(), "custody probe must exercise panic Drop");
    let retained_path = retained_path.expect("custody probe records its root");
    assert!(retained_path.join("failure-evidence").is_file());
    fs::remove_dir_all(&retained_path).expect("remove verified custody probe root");
}

#[cfg(feature = "product-pack-pins")]
mod product_pin {
    use super::*;
    use foundry_exec::{
        Attempt, Epoch, ExecutionIntent, Generation, OrderId, PolicyRejection,
        ProductSelectionBinding, ReviewAdmissionRequest, ReviewAdmissionStore, RunId,
        SelectedAdmission, SelectedReviewAdmissionRequest,
    };
    use foundry_ledger::PackRegistryConfig;
    use foundry_line::{StageExecutor, StageOutput, StageTask};

    struct ObservedExecutor {
        calls: Arc<AtomicUsize>,
        lenses: Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
        on_execute: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl StageExecutor for ObservedExecutor {
        fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.lenses.lock().unwrap().push(task.lens_bytes.clone());
            if let Some(on_execute) = &self.on_execute {
                on_execute();
            }
            Err("synthetic observed stage failure".into())
        }
    }

    struct FinalPassExecutor {
        calls: Arc<AtomicUsize>,
        revoke_after_final_execute: Arc<dyn Fn(&StageTask) + Send + Sync>,
    }

    struct ObservedContinuationExecutor {
        calls: Arc<AtomicUsize>,
        restores: Arc<AtomicUsize>,
        after_execute: Arc<dyn Fn(&StageTask) + Send + Sync>,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ReservedStageObservation {
        stage_id: String,
        executor_calls: usize,
        stage_rows: i64,
        accepted_rows: i64,
        complete_rows: i64,
    }

    impl StageExecutor for ObservedContinuationExecutor {
        fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (self.after_execute)(task);
            let output = task.workdir.join("b2-w1-observed-continuation.log");
            fs::write(&output, b"synthetic observed continuation output\n")
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
            self.restores.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl StageExecutor for FinalPassExecutor {
        fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if call == 5 {
                (self.revoke_after_final_execute)(task);
            }
            let output = task.workdir.join("b2-w1-final-pass.log");
            fs::write(&output, b"synthetic no-findings continuation output\n")
                .map_err(|error| error.to_string())?;
            Ok(StageOutput {
                verdict_category: None,
                findings: false,
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

    fn intent_at(now: chrono::DateTime<chrono::Utc>, order_id: &str) -> ExecutionIntent {
        ExecutionIntent {
            order: OrderId(order_id.to_owned()),
            attempt: Attempt(1),
            run: RunId(7),
            activation: 1,
            generation: Generation(0),
            epoch: Epoch(1),
            external_ref: format!("review:{order_id}:1:base..result"),
            recorded_at: now,
        }
    }

    fn intent(fixture: &RuntimeFixture, order_id: &str) -> ExecutionIntent {
        intent_at(fixture.now, order_id)
    }

    fn reserved_stage_observation(
        db_path: &Path,
        order_id: &str,
        run_id: &str,
        stage_id: &str,
        executor_calls: usize,
    ) -> ReservedStageObservation {
        let (stage_rows, accepted_rows, complete_rows) = rusqlite::Connection::open(db_path)
            .unwrap()
            .query_row(
                "SELECT (SELECT count(*) FROM product_review_stages_v1),
                        (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id=?3 AND status='accepted'),
                        (SELECT count(*) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND stage_id=?3 AND status='complete')",
                rusqlite::params![order_id, run_id, stage_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        ReservedStageObservation {
            stage_id: stage_id.to_owned(),
            executor_calls,
            stage_rows,
            accepted_rows,
            complete_rows,
        }
    }

    fn registry(
        x_digest: &str,
        x_pack: &Path,
        y_digest: &str,
        y_pack: &Path,
    ) -> PackRegistryConfig {
        PackRegistryConfig {
            snapshots: [
                (x_digest.to_owned(), x_pack.to_path_buf()),
                (y_digest.to_owned(), y_pack.to_path_buf()),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn approve_y(fixture: &RuntimeFixture) -> (std::path::PathBuf, String, String) {
        let y_pack = fixture.root.0.join("pack-y");
        let (y_pack, y_digest) = y_pack_with_changed_lenses(&fixture.pack, &y_pack);
        let x_digest = manifest_digest(&fixture.pack);
        fixture
            .store()
            .with_product_registry(registry(&x_digest, &fixture.pack, &y_digest, &y_pack))
            .change_pack_approval(
                &fixture.owner_session,
                "b2-w1-source",
                &y_digest,
                0,
                true,
                "approve-y-for-activation",
                fixture.now,
            )
            .expect("approve alternate pack for ProductStore activation");
        (y_pack, x_digest, y_digest)
    }

    fn selection_binding(fixture: &RuntimeFixture, order_id: &str) -> ProductSelectionBinding {
        ProductSelectionBinding {
            order_id: order_id.to_owned(),
            lifecycle: fixture.item(order_id).admission.lifecycle,
            source_instance_id: "b2-w1-source".to_owned(),
            pack_digest: manifest_digest(&fixture.pack),
            selection_revision: 1,
            referenced_approval_revision: 1,
        }
    }

    fn admission_request(fixture: &RuntimeFixture, order_id: &str) -> ReviewAdmissionRequest {
        ReviewAdmissionRequest {
            intent: intent(fixture, order_id),
            base_commit: "base".into(),
            result_commit: "result".into(),
            pack_digest: manifest_digest(&fixture.pack),
            product_work: Some(fixture.item(order_id).claim.review_binding()),
        }
    }

    fn selected_request(
        fixture: &RuntimeFixture,
        order_id: &str,
        binding: ProductSelectionBinding,
        request_key: &str,
    ) -> SelectedReviewAdmissionRequest {
        SelectedReviewAdmissionRequest {
            review: admission_request(fixture, order_id),
            binding,
            request_key: request_key.to_owned(),
        }
    }

    fn typed_row(db: &Path, sql: &str) -> Vec<rusqlite::types::Value> {
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open typed atomic write-set observer");
        let mut statement = conn.prepare(sql).expect("prepare typed write-set row");
        let columns = statement.column_count();
        statement
            .query_row([], |row| {
                (0..columns)
                    .map(|index| row.get(index))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .expect("read typed atomic write-set row")
    }

    fn assert_atomic_activation_write_set(
        fixture: &RuntimeFixture,
        request: &SelectedReviewAdmissionRequest,
        admitted: &foundry_exec::ReviewAdmission,
        admission_started_at: i64,
        admission_ended_at: i64,
    ) {
        use rusqlite::types::Value::{Integer, Null, Text};

        let intent = &request.review.intent;
        let work = request
            .review
            .product_work
            .as_ref()
            .expect("ProductStore activation carries the exact work claim");
        let service = work
            .service_binding
            .as_ref()
            .expect("ProductStore activation carries the exact service binding");
        let run = &admitted.journal_run;
        assert_eq!(
            run,
            &format!(
                "{}:{}:{}:{}",
                intent.order.0, intent.attempt.0, intent.run.0, intent.activation
            )
        );
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT order_id,lifecycle,installation_id,database_id,source_instance_id,pack_digest,selection_revision,referenced_approval_revision,service_id,request_key,claim_epoch,claim_fence,payload_digest,admission_seq,admission_hash FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
            ),
            vec![
                Text(request.binding.order_id.clone()),
                Integer(request.binding.lifecycle),
                Text(fixture.config.installation_id.clone()),
                Text(fixture.config.database_id.clone()),
                Text(request.binding.source_instance_id.clone()),
                Text(request.binding.pack_digest.clone()),
                Integer(request.binding.selection_revision),
                Integer(request.binding.referenced_approval_revision),
                Text(service.service_id.clone()),
                Text(request.request_key.clone()),
                Integer(work.epoch),
                Integer(work.fence),
                Text(work.payload_digest.clone()),
                Integer(fixture.item("order-a").admission.sequence),
                Text(fixture.item("order-a").admission.event_hash.clone()),
            ]
        );
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT order_id,epoch FROM product_review_epochs_v1 WHERE order_id='order-a'",
            ),
            vec![Text(intent.order.0.clone()), Integer(intent.epoch.0 as i64)]
        );
        let run_row = typed_row(
            &fixture.db_path,
            "SELECT order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,hold_json,hold_instance_id,deadline,result_json,provenance_json,product_lifecycle,source_revision,admission_seq,admission_hash,created_at FROM product_review_runs_v1 WHERE order_id='order-a'",
        );
        let created_at = match run_row.last() {
            Some(Integer(value)) => *value,
            _ => panic!("review run created_at must be an integer SQLite unixepoch"),
        };
        assert_eq!(
            run_row,
            vec![
                Text(intent.order.0.clone()),
                Integer(intent.attempt.0 as i64),
                Text(run.clone()),
                Text(request.review.base_commit.clone()),
                Text(request.review.result_commit.clone()),
                Text(request.review.pack_digest.clone()),
                Text("running".into()),
                Text("{}".into()),
                Null,
                Null,
                Null,
                Null,
                Null,
                Integer(request.binding.lifecycle),
                Integer(work.source_revision),
                Integer(fixture.item("order-a").admission.sequence),
                Text(fixture.item("order-a").admission.event_hash.clone()),
                Integer(created_at),
            ]
        );
        assert!(
            (admission_started_at..=admission_ended_at).contains(&created_at),
            "review run created_at {created_at} must fall within the real UTC admission interval {admission_started_at}..={admission_ended_at}"
        );
        assert!(
            schema_snapshot(&fixture.db_path)
                .iter()
                .all(|(_, _, table, _)| table != "product_review_task_contracts_v1"),
            "standalone selected activation must preserve the schema without a task contract"
        );
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT order_id,attempt,run_id,lifecycle,delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot FROM product_review_work_bindings_v1 WHERE order_id='order-a'",
            ),
            vec![
                Text(intent.order.0.clone()),
                Integer(intent.attempt.0 as i64),
                Text(run.clone()),
                Integer(work.lifecycle),
                Text(work.delivery_id.clone()),
                Text(work.worker_id.clone()),
                Integer(work.epoch),
                Integer(work.fence),
                Integer(work.source_revision),
                Text(work.payload_digest.clone()),
                Text(work.task_kind.clone()),
                Text(work.payload_snapshot.clone()),
            ]
        );
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT order_id,attempt,run_id,activation FROM product_review_host_accept_v1 WHERE order_id='order-a'",
            ),
            vec![
                Text(intent.order.0.clone()),
                Integer(intent.attempt.0 as i64),
                Integer(intent.run.0 as i64),
                Integer(intent.activation as i64),
            ]
        );
        let expected_key = serde_json::to_string(&foundry_exec::HostKey {
            order: intent.order.0.clone(),
            attempt: intent.attempt.0,
            run: intent.run.0,
            activation: intent.activation,
        })
        .unwrap();
        let expected_intent = serde_json::to_string(intent).unwrap();
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT key_json,intent_json FROM product_review_bindings_v1",
            ),
            vec![Text(expected_key.clone()), Text(expected_intent)]
        );
        let authority_revisions: (i64, i64, i64, i64) = rusqlite::Connection::open_with_flags(
            &fixture.db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row(
            "SELECT g.grant_revision,i.generation,i.revision,ps.revision FROM product_service_grants_v1 g JOIN product_scope_source_instances_v1 i ON i.source_instance_id=g.source_instance_id JOIN product_services_v1 s ON s.service_id=g.service_id JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id JOIN product_intake_source_bindings_v1 ib ON ib.delivery_id=?1 WHERE g.service_id=?2 AND g.source_instance_id=?3 AND g.action='drive_review' AND g.enabled=1 AND i.enabled=1 AND ps.enabled=1 AND i.source_instance_id=ib.source_instance_id AND ib.source_revision=ps.revision AND ib.source_generation=i.generation AND ib.source_instance_revision=i.revision AND s.revision=?4 AND ps.revision=?5",
            rusqlite::params![work.delivery_id, service.service_id, service.source_instance_id, service.service_revision, work.source_revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read exact pre-existing review authority revisions");
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT key_json,order_id,lifecycle,attempt,run_id,activation,journal_run,host_epoch,service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision FROM product_review_authority_v1 WHERE order_id='order-a'",
            ),
            vec![
                Text(expected_key.clone()),
                Text(intent.order.0.clone()),
                Integer(work.lifecycle),
                Integer(intent.attempt.0 as i64),
                Integer(intent.run.0 as i64),
                Integer(intent.activation as i64),
                Text(run.clone()),
                Integer(intent.epoch.0 as i64),
                Text(service.service_id.clone()),
                Integer(service.service_revision),
                Integer(service.credential_revision),
                Integer(authority_revisions.0),
                Text(service.source_instance_id.clone()),
                Integer(authority_revisions.1),
                Integer(authority_revisions.2),
                Integer(authority_revisions.3),
            ]
        );
        assert_eq!(
            typed_row(
                &fixture.db_path,
                "SELECT order_id,key_json,epoch,journal_run FROM product_review_active_v1 WHERE order_id='order-a'",
            ),
            vec![
                Text(intent.order.0.clone()),
                Text(expected_key),
                Integer(intent.epoch.0 as i64),
                Text(run.clone()),
            ]
        );
    }

    #[test]
    fn p03_standalone_selected_admission_fails_closed_without_a_pin() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let item = fixture.item("order-a");
        let binding = selection_binding(&fixture, "order-a");
        let mut store = fixture.store();

        let error = store
            .selected_admission(&item.claim.review_binding(), &binding, "direct-admit")
            .expect_err("standalone admission must not split pin from activation");
        assert!(matches!(
            error,
            foundry_exec::HostError::Acceptance(ref message)
                if message == "standalone selected admission requires atomic review activation"
        ));

        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 0);
        drop(db);
        drop(store);
        assert!(
            foundry_ledger::ProductStore::open(&fixture.db_path, fixture.config.clone()).is_ok(),
            "a pin-free database reopens after refused standalone admission"
        );
    }

    #[test]
    fn p03_standalone_attempt_then_claim_reassignment_reopens_and_retries_atomically() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let item = fixture.item("order-a");
        let binding = selection_binding(&fixture, "order-a");
        let mut store = fixture.configured_review_store(binding.pack_digest.clone());

        let error = store
            .selected_admission(
                &item.claim.review_binding(),
                &binding,
                "standalone-before-activation",
            )
            .expect_err("standalone admission must not leave an orphan lifecycle pin");
        assert!(matches!(
            error,
            foundry_exec::HostError::Acceptance(ref message)
                if message == "standalone selected admission requires atomic review activation"
        ));
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
        drop(store);

        let reclaim_at = fixture.now + chrono::Duration::minutes(6);
        let replacement = match fixture
            .store()
            .claim_product_work_for_service(
                &fixture.service,
                "replacement-worker",
                reclaim_at,
                chrono::Duration::minutes(5),
            )
            .expect("expired claim can be reassigned")
        {
            foundry_ledger::ProductWorkClaimResult::Claimed(claim) => claim,
            other => panic!("expected replacement claim, got {other:?}"),
        };
        assert!(replacement.review_binding().fence > item.claim.review_binding().fence);

        let mut request = selected_request(
            &fixture,
            "order-a",
            binding.clone(),
            "replacement-selected-activation",
        );
        request.review.product_work = Some(replacement.review_binding());
        let admitted = fixture
            .configured_review_store(binding.pack_digest.clone())
            .admit_selected_review_activation(request)
            .expect("reopened activation atomically pins the reassigned claim and run");
        assert_eq!(admitted.journal_run, "order-a:1:7:1");
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_host_accept_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_active_v1"), 1);
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let pinned_fence: i64 = db
            .query_row(
                "SELECT claim_fence FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pinned_fence, replacement.review_binding().fence);
    }

    #[test]
    fn p03_reopened_orphan_pin_is_never_adopted_even_when_coordinates_match() {
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let item = fixture.item("order-a");
        let binding = selection_binding(&fixture, "order-a");
        let claim = item.claim.review_binding();
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        db.execute(
            "INSERT INTO product_w1_lifecycle_pins_v1(order_id,lifecycle,installation_id,database_id,source_instance_id,pack_digest,selection_revision,referenced_approval_revision,service_id,request_key,claim_epoch,claim_fence,payload_digest,admission_seq,admission_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                binding.order_id,
                binding.lifecycle,
                fixture.config.installation_id,
                fixture.config.database_id,
                binding.source_instance_id,
                binding.pack_digest,
                binding.selection_revision,
                binding.referenced_approval_revision,
                fixture.service.service_id(),
                "pre-correction-orphan",
                claim.epoch,
                claim.fence,
                claim.payload_digest,
                item.admission.sequence,
                item.admission.event_hash,
            ],
        )
        .unwrap();
        drop(db);
        let digest = manifest_digest(&fixture.pack);
        let mut legacy_store = fixture.configured_review_store(digest.clone());
        let orphan_state = snapshot(&fixture.db_path);
        let orphan_schema = schema_snapshot(&fixture.db_path);

        let error = legacy_store
            .admit_review_activation(admission_request(&fixture, "order-a"))
            .expect_err("an orphan pin must not be treated as atomic activation state");
        assert!(matches!(
            error,
            foundry_exec::HostError::Acceptance(ref message)
                if message == "selected product pack pin exists outside atomic activation"
        ));
        assert_state_and_schema_unchanged(&orphan_state, &orphan_schema, &fixture.db_path);

        let selected_error = fixture
            .configured_review_store(manifest_digest(&fixture.pack))
            .admit_selected_review_activation(selected_request(
                &fixture,
                "order-a",
                binding,
                "orphan-selected-activation",
            ))
            .expect_err("selected atomic API must not adopt a pre-existing orphan pin");
        assert!(matches!(
            selected_error,
            foundry_exec::HostError::Acceptance(ref message)
                if message == "selected product pack pin exists outside atomic activation"
        ));
        assert_state_and_schema_unchanged(&orphan_state, &orphan_schema, &fixture.db_path);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_active_v1"), 0);
        drop(legacy_store);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p04_product_store_selected_admission_requires_sidecar_before_stale_revision() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let item = fixture.item("order-a");
        let claim = item.claim.review_binding();
        let mut store = fixture.store();

        let mut stale = selection_binding(&fixture, "order-a");
        stale.selection_revision += 1;
        assert!(matches!(
            store
                .selected_admission(&claim, &stale, "stale-direct-admit")
                .unwrap(),
            SelectedAdmission::PolicyRejected {
                reason: PolicyRejection::AuthorizationBinding,
                ..
            }
        ));

        let mut unauthorized = selection_binding(&fixture, "order-a");
        unauthorized.source_instance_id = "unbound-source".to_owned();
        assert!(matches!(
            store
                .selected_admission(&claim, &unauthorized, "unauthorized-direct-admit")
                .unwrap(),
            SelectedAdmission::PolicyRejected {
                reason: PolicyRejection::AuthorizationBinding,
                ..
            }
        ));
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
    }

    #[test]
    fn p04_selected_admission_requires_exact_live_service_and_claim_binding() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let binding = selection_binding(&fixture, "order-a");
        let mut store = fixture.store();
        let mut stale_service = fixture.item("order-a").claim.review_binding();
        stale_service
            .service_binding
            .as_mut()
            .unwrap()
            .credential_revision += 1;
        assert!(matches!(
            store
                .selected_admission(&stale_service, &binding, "stale-service-admit")
                .unwrap(),
            SelectedAdmission::PolicyRejected {
                reason: PolicyRejection::AuthorizationBinding,
                ..
            }
        ));
        let mut foreign_claim = fixture.item("order-a").claim.review_binding();
        foreign_claim.worker_id = "foreign-worker".into();
        assert!(matches!(
            store
                .selected_admission(&foreign_claim, &binding, "foreign-claim-admit")
                .unwrap(),
            SelectedAdmission::PolicyRejected {
                reason: PolicyRejection::AuthorizationBinding,
                ..
            }
        ));
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
    }

    #[test]
    fn p04_standalone_selected_admission_does_not_create_a_request_key_namespace() {
        let mut fixture = RuntimeFixture::new(&["order-a", "order-b"]);
        fixture.root.arm_failure_preservation();
        let claim_a = fixture.item("order-a").claim.review_binding();
        let binding_a = selection_binding(&fixture, "order-a");
        let claim_b = fixture.item("order-b").claim.review_binding();
        let binding_b = selection_binding(&fixture, "order-b");
        let mut store = fixture.store();
        let baseline = snapshot(&fixture.db_path);
        let baseline_schema = schema_snapshot(&fixture.db_path);
        for (claim, binding) in [
            (&claim_a, &binding_a),
            (&claim_a, &binding_a),
            (&claim_b, &binding_b),
        ] {
            let result = store.selected_admission(claim, binding, "shared-admit-key");
            assert!(matches!(
                result,
                Err(foundry_exec::HostError::Acceptance(ref message))
                    if message == "standalone selected admission requires atomic review activation"
            ));
            assert_state_and_schema_unchanged(&baseline, &baseline_schema, &fixture.db_path);
        }
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
        drop(store);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p04_atomic_selected_activation_replays_exact_service_key_and_conflicts_on_change() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let binding = selection_binding(&fixture, "order-a");
        let service_request_key = "service-activation-request";
        let mut store = fixture.configured_review_store(binding.pack_digest.clone());
        let request = selected_request(&fixture, "order-a", binding, service_request_key);

        let admitted = store
            .admit_selected_review_activation(request.clone())
            .expect("selected activation commits pin and review write set atomically");
        let before_replay = snapshot(&fixture.db_path);
        let replay = store
            .admit_selected_review_activation(request.clone())
            .expect("exact service request-key retry replays admission");
        assert_eq!(replay, admitted);
        assert_snapshot_unchanged(&before_replay, &fixture.db_path);

        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let persisted_key: String = db
            .query_row(
                "SELECT request_key FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_key, service_request_key);
        drop(db);

        let mut changed = request;
        changed.review.result_commit = "changed-result".into();
        changed.review.intent.external_ref = "review:order-a:1:base..changed-result".into();
        let before_conflict = snapshot(&fixture.db_path);
        let result = store.admit_selected_review_activation(changed);
        assert!(matches!(
            result,
            Err(foundry_exec::HostError::Acceptance(ref message))
                if message == "selected review activation request key conflict"
        ));
        assert_snapshot_unchanged(&before_conflict, &fixture.db_path);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_host_accept_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_active_v1"), 1);
    }

    #[test]
    fn p04_atomic_selected_activation_reopens_state_schema_and_replays_exactly() {
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let binding = selection_binding(&fixture, "order-a");
        let request = selected_request(
            &fixture,
            "order-a",
            binding.clone(),
            "reopen-exact-selected-activation",
        );
        let mut store = fixture.configured_review_store(binding.pack_digest.clone());
        assert_eq!(
            count(&fixture.db_path, "product_w1_lifecycle_pins_v1"),
            0,
            "atomic activation starts without a lifecycle pin"
        );
        let admission_started_at = chrono::Utc::now().timestamp();
        for table in [
            "product_w1_lifecycle_pins_v1",
            "product_review_epochs_v1",
            "product_review_runs_v1",
            "product_review_work_bindings_v1",
            "product_review_host_accept_v1",
            "product_review_bindings_v1",
            "product_review_authority_v1",
            "product_review_active_v1",
        ] {
            assert_eq!(
                count(&fixture.db_path, table),
                0,
                "{table} is empty immediately before the selected activation",
            );
        }
        let admitted = store
            .admit_selected_review_activation(request.clone())
            .expect("genuine selected activation commits atomically");
        let admission_ended_at = chrono::Utc::now().timestamp();
        let committed_state = snapshot(&fixture.db_path);
        let committed_schema = schema_snapshot(&fixture.db_path);
        assert_eq!(
            count(&fixture.db_path, "product_w1_lifecycle_pins_v1"),
            1,
            "atomic activation commits exactly one lifecycle pin"
        );

        let exact_selection_receipt = fixture
            .store()
            .select_order_pack(
                &fixture.owner_session,
                "order-a",
                binding.lifecycle,
                &binding.source_instance_id,
                &binding.pack_digest,
                0,
                "b2-w1-select-order-a",
                fixture.now,
            )
            .expect("exact selection retry returns the original canonical receipt");
        assert_state_and_schema_unchanged(&committed_state, &committed_schema, &fixture.db_path);
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let pin: (
            String,
            i64,
            String,
            String,
            String,
            String,
            i64,
            i64,
            String,
            String,
            i64,
            i64,
            String,
            i64,
            String,
        ) = db
            .query_row(
                "SELECT order_id,lifecycle,installation_id,database_id,source_instance_id,pack_digest,selection_revision,referenced_approval_revision,service_id,request_key,claim_epoch,claim_fence,payload_digest,admission_seq,admission_hash FROM product_w1_lifecycle_pins_v1",
                [],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?,row.get(12)?,row.get(13)?,row.get(14)?)),
            )
            .unwrap();
        assert_eq!(pin.0, binding.order_id);
        assert_eq!(pin.1, binding.lifecycle);
        assert_eq!(pin.2, fixture.config.installation_id);
        assert_eq!(pin.3, fixture.config.database_id);
        assert_eq!(pin.4, binding.source_instance_id);
        assert_eq!(pin.5, binding.pack_digest);
        assert_eq!(pin.6, binding.selection_revision);
        assert_eq!(pin.7, binding.referenced_approval_revision);
        assert_eq!(pin.8, fixture.service.service_id());
        assert_eq!(pin.9, request.request_key);
        assert_eq!(pin.10, fixture.item("order-a").claim.review_binding().epoch);
        assert_eq!(pin.11, fixture.item("order-a").claim.review_binding().fence);
        assert_eq!(
            pin.12,
            fixture
                .item("order-a")
                .claim
                .review_binding()
                .payload_digest
        );
        assert_eq!(pin.13, fixture.item("order-a").admission.sequence);
        assert_eq!(pin.14, fixture.item("order-a").admission.event_hash);
        let selection_receipt: Vec<u8> = db
            .query_row(
                "SELECT receipt FROM product_w1_operations_v1 WHERE operation='select' AND order_id='order-a' ORDER BY selection_result_revision DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(selection_receipt, exact_selection_receipt.canonical);
        drop(db);
        assert_atomic_activation_write_set(
            &fixture,
            &request,
            &admitted,
            admission_started_at,
            admission_ended_at,
        );
        drop(store);

        let reopened = fixture.configured_review_store(binding.pack_digest.clone());
        assert_eq!(snapshot(&fixture.db_path), committed_state);
        assert_eq!(schema_snapshot(&fixture.db_path), committed_schema);
        drop(reopened);

        let mut replay_store = fixture.configured_review_store(binding.pack_digest.clone());
        let replay = replay_store
            .admit_selected_review_activation(request)
            .expect("byte-identical request replays after full reopen");
        assert_eq!(replay, admitted);
        assert_state_and_schema_unchanged(&committed_state, &committed_schema, &fixture.db_path);
        drop(replay_store);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p04_atomic_selected_activation_rolls_back_complete_write_set_before_commit() {
        use std::sync::mpsc;
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let binding = selection_binding(&fixture, "order-a");
        let request = selected_request(&fixture, "order-a", binding.clone(), "rollback-key");
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        drop(release_tx);
        let mut store = fixture.configured_review_store(binding.pack_digest.clone());
        let baseline = snapshot(&fixture.db_path);
        let baseline_schema = schema_snapshot(&fixture.db_path);
        store.arm_product_pin_commit_hook(reached_tx, release_rx);
        let error = store
            .admit_selected_review_activation(request.clone())
            .expect_err("pre-commit hook failure rolls back the atomic activation");
        assert!(matches!(
            error,
            foundry_exec::HostError::Persistence(ref message)
                if message == "product pin commit hook release disconnected"
        ));
        reached_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("rollback probe reached the complete pre-commit write set");
        for table in [
            "product_w1_lifecycle_pins_v1",
            "product_review_epochs_v1",
            "product_review_runs_v1",
            "product_review_work_bindings_v1",
            "product_review_host_accept_v1",
            "product_review_bindings_v1",
            "product_review_authority_v1",
            "product_review_active_v1",
        ] {
            assert_eq!(count(&fixture.db_path, table), 0, "{table} rolled back");
        }
        assert_state_and_schema_unchanged(&baseline, &baseline_schema, &fixture.db_path);
        let admission_started_at = chrono::Utc::now().timestamp();
        let retry = store
            .admit_selected_review_activation(request.clone())
            .expect("same request succeeds after known pre-commit rollback");
        let admission_ended_at = chrono::Utc::now().timestamp();
        assert_eq!(retry.journal_run, "order-a:1:7:1");
        assert_atomic_activation_write_set(
            &fixture,
            &request,
            &retry,
            admission_started_at,
            admission_ended_at,
        );
        drop(store);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p04_selected_activation_request_key_byte_and_nul_boundaries_fail_closed() {
        for key in ["", "nul\0key", &"k".repeat(257)] {
            let fixture = RuntimeFixture::new(&["order-a"]);
            let binding = selection_binding(&fixture, "order-a");
            let mut store = fixture.configured_review_store(binding.pack_digest.clone());
            let before = snapshot(&fixture.db_path);
            let error = store
                .admit_selected_review_activation(selected_request(
                    &fixture, "order-a", binding, key,
                ))
                .expect_err("invalid service request key must fail closed");
            assert!(matches!(
                error,
                foundry_exec::HostError::Acceptance(ref message)
                    if message == "selected review activation binding is invalid"
            ));
            assert_snapshot_unchanged(&before, &fixture.db_path);
        }

        let fixture = RuntimeFixture::new(&["order-a"]);
        let binding = selection_binding(&fixture, "order-a");
        let key = "k".repeat(256);
        fixture
            .configured_review_store(binding.pack_digest.clone())
            .admit_selected_review_activation(selected_request(&fixture, "order-a", binding, &key))
            .expect("the exact 256-byte service request-key boundary is accepted");
    }

    #[test]
    fn p04_selector_revocation_is_not_masked_by_another_subject_with_matching_revisions() {
        use foundry_ledger::{ProductCapability, ScopeMutation};
        let fixture = RuntimeFixture::new(&["order-a"]);
        let mut store = fixture.store();
        for actor in ["selector", "decoy"] {
            let password = format!("{actor}-password");
            let phc =
                foundry_ledger::product_identity::ProductPasswordKdf::hash(&password).unwrap();
            store
                .create_account(&fixture.owner_session, actor, "owner", &phc, fixture.now)
                .unwrap();
            store
                .change_product_scope(
                    &fixture.owner_session,
                    actor,
                    "b2-w1-source",
                    ProductCapability::ReadOrder,
                    ScopeMutation::Grant,
                    0,
                    &format!("grant-{actor}"),
                    fixture.now,
                )
                .unwrap();
        }
        let selector_session = store
            .login("selector", "selector-password", fixture.now)
            .unwrap()
            .digest()
            .to_owned();
        let y_dir = fixture.root.0.join("selector-pack-y");
        let (y_pack, y_digest) = y_pack_with_changed_lenses(&fixture.pack, &y_dir);
        let x_digest = manifest_digest(&fixture.pack);
        store = store.with_product_registry(registry(&x_digest, &fixture.pack, &y_digest, &y_pack));
        store
            .change_pack_approval(
                &fixture.owner_session,
                "b2-w1-source",
                &y_digest,
                0,
                true,
                "approve-selector-y",
                fixture.now,
            )
            .unwrap();
        let selected = store
            .select_order_pack(
                &selector_session,
                "order-a",
                fixture.item("order-a").admission.lifecycle,
                "b2-w1-source",
                &y_digest,
                1,
                "selector-select-y",
                fixture.now,
            )
            .unwrap();
        assert_eq!(selected.result_revision, 2);
        store
            .change_product_scope(
                &fixture.owner_session,
                "selector",
                "b2-w1-source",
                ProductCapability::ReadOrder,
                ScopeMutation::Revoke,
                1,
                "revoke-selector",
                fixture.now,
            )
            .unwrap();
        let binding = ProductSelectionBinding {
            order_id: "order-a".into(),
            lifecycle: fixture.item("order-a").admission.lifecycle,
            source_instance_id: "b2-w1-source".into(),
            pack_digest: y_digest.clone(),
            selection_revision: 2,
            referenced_approval_revision: 1,
        };
        assert!(matches!(
            store
                .selected_admission(
                    &fixture.item("order-a").claim.review_binding(),
                    &binding,
                    "revoked-selector-admit"
                )
                .unwrap(),
            SelectedAdmission::PolicyRejected {
                reason: PolicyRejection::AuthorizationBinding,
                ..
            }
        ));
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let sidecar_subject: String = db.query_row(
            "SELECT selector_subject_id FROM product_w1_grant_sidecar_v1 WHERE order_id='order-a' AND selection_revision=2",
            [], |r| r.get(0),
        ).unwrap();
        assert_eq!(sidecar_subject, "selector");
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);

        let mut review = admission_request(&fixture, "order-a");
        review.pack_digest = y_digest.clone();
        let mut activation_store = fixture
            .configured_review_store(y_digest.clone())
            .with_product_registry(registry(&x_digest, &fixture.pack, &y_digest, &y_pack));
        assert!(matches!(
            activation_store.admit_selected_review_activation(SelectedReviewAdmissionRequest {
                review,
                binding,
                request_key: "revoked-selector-activation".into(),
            }),
            Err(foundry_exec::HostError::Acceptance(ref message))
                if message == "selected product pack authority is unavailable"
        ));
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_host_accept_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_active_v1"), 0);
    }

    #[test]
    fn p04_revoked_selected_pack_blocks_fresh_activation_without_effects() {
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let digest = manifest_digest(&fixture.pack);
        fixture
            .store()
            .change_pack_approval(
                &fixture.owner_session,
                "b2-w1-source",
                &digest,
                1,
                false,
                "revoke-before-fresh-activation",
                fixture.now,
            )
            .expect("revoke selected pack approval");
        let binding = selection_binding(&fixture, "order-a");
        let mut activation_store = fixture.configured_review_store(digest);
        let state_before = snapshot(&fixture.db_path);
        let schema_before = schema_snapshot(&fixture.db_path);
        let error = activation_store
            .admit_selected_review_activation(selected_request(
                &fixture,
                "order-a",
                binding,
                "fresh-after-pack-revoke",
            ))
            .expect_err("a fresh selected activation cannot use a revoked pack");
        assert!(matches!(
            error,
            foundry_exec::HostError::Acceptance(ref message)
                if message == "selected product pack is unavailable"
        ));
        assert_state_and_schema_unchanged(&state_before, &schema_before, &fixture.db_path);
        for table in [
            "product_w1_lifecycle_pins_v1",
            "product_review_runs_v1",
            "product_review_host_accept_v1",
            "product_review_active_v1",
        ] {
            assert_eq!(
                count(&fixture.db_path, table),
                0,
                "{table} has no activation effect"
            );
        }
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p05_failed_standalone_admission_does_not_freeze_selection() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let item = fixture.item("order-a");
        let binding = selection_binding(&fixture, "order-a");
        let mut store = fixture.store();
        assert!(matches!(
            store.selected_admission(
                &item.claim.review_binding(),
                &binding,
                "admit-before-change"
            ),
            Err(foundry_exec::HostError::Acceptance(ref message))
                if message == "standalone selected admission requires atomic review activation"
        ));

        let (_other_pack, other_digest) =
            y_pack_with_changed_lenses(&fixture.pack, &fixture.root.0.join("pack-y"));
        store = store.with_product_registry(PackRegistryConfig {
            snapshots: [
                (binding.pack_digest.clone(), fixture.pack.clone()),
                (other_digest.clone(), fixture.root.0.join("pack-y")),
            ]
            .into_iter()
            .collect(),
        });
        store
            .change_pack_approval(
                &fixture.owner_session,
                "b2-w1-source",
                &other_digest,
                0,
                true,
                "approve-y-after-admission",
                fixture.now,
            )
            .expect("approve alternate pack for selection-change attempt");
        let changed = store
            .select_order_pack(
                &fixture.owner_session,
                "order-a",
                binding.lifecycle,
                "b2-w1-source",
                &other_digest,
                binding.selection_revision,
                "change-after-failed-standalone-admission",
                fixture.now,
            )
            .expect("a refused standalone call must not freeze lifecycle selection");
        assert_eq!(changed.digest, other_digest);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
    }

    #[test]
    fn p03_product_store_admission_commit_wins_real_api_contention() {
        use std::{sync::mpsc, thread};
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let (y_pack, x_digest, y_digest) = approve_y(&fixture);
        let request = selected_request(
            &fixture,
            "order-a",
            selection_binding(&fixture, "order-a"),
            "race-activation-first",
        );
        let winner_request = request.clone();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut admission_store = fixture.configured_review_store(x_digest.clone());
        admission_store.arm_product_pin_commit_hook(reached_tx, release_rx);
        let mut busy_selector = fixture.store().with_product_registry(registry(
            &x_digest,
            &fixture.pack,
            &y_digest,
            &y_pack,
        ));
        busy_selector
            .set_product_pin_test_busy_timeout(Duration::ZERO)
            .unwrap();
        let loser_key = "race-select-loser";
        let loser_state = snapshot(&fixture.db_path);
        let loser_schema = schema_snapshot(&fixture.db_path);
        let admission_started_at = chrono::Utc::now().timestamp();
        let admission =
            thread::spawn(move || admission_store.admit_selected_review_activation(request));
        reached_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("host activation holds the real ProductStore writer transaction before commit");
        let busy = busy_selector
            .select_order_pack(
                &fixture.owner_session,
                "order-a",
                fixture.item("order-a").admission.lifecycle,
                "b2-w1-source",
                &y_digest,
                1,
                loser_key,
                fixture.now,
            )
            .unwrap_err();
        assert!(
            matches!(
                &busy,
                foundry_ledger::ProductError::Database(rusqlite::Error::SqliteFailure(code, message))
                    if code.code == rusqlite::ErrorCode::DatabaseBusy
                        && code.extended_code == 5
                        && message.as_deref() == Some("database is locked")
            ),
            "selection product API must observe the held admission writer lock exactly: {busy:?}"
        );
        assert_state_and_schema_unchanged(&loser_state, &loser_schema, &fixture.db_path);
        release_tx.send(()).unwrap();
        let admitted = admission
            .join()
            .unwrap()
            .expect("host activation commits first");
        let admission_ended_at = chrono::Utc::now().timestamp();
        assert_eq!(admitted.journal_run, "order-a:1:7:1");
        let before_refusal = snapshot(&fixture.db_path);
        let before_refusal_schema = schema_snapshot(&fixture.db_path);
        let retry = busy_selector
            .select_order_pack(
                &fixture.owner_session,
                "order-a",
                fixture.item("order-a").admission.lifecycle,
                "b2-w1-source",
                &y_digest,
                1,
                loser_key,
                fixture.now,
            )
            .expect_err("the exact busy request is retried once after the winner commits");
        assert!(matches!(
            retry,
            foundry_ledger::ProductError::Invariant("w1 selection blocked by review")
        ));
        assert_state_and_schema_unchanged(
            &before_refusal,
            &before_refusal_schema,
            &fixture.db_path,
        );
        drop(busy_selector);
        let reopened = fixture.store();
        assert_atomic_activation_write_set(
            &fixture,
            &winner_request,
            &admitted,
            admission_started_at,
            admission_ended_at,
        );
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT service_id FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            fixture.service.service_id()
        );
        assert_eq!(db.query_row("SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='order-a' AND attempt=1 AND run_id=7 AND activation=1", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        assert_eq!(db.query_row("SELECT status FROM product_review_runs_v1 WHERE order_id='order-a' AND run_id='order-a:1:7:1'", [], |r| r.get::<_,String>(0)).unwrap(), "running");
        assert_eq!(
            db.query_row(
                "SELECT journal_run FROM product_review_active_v1 WHERE order_id='order-a'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "order-a:1:7:1"
        );
        assert_eq!(db.query_row("SELECT count(*) FROM product_w1_operations_v1 WHERE operation='select' AND digest=?1", [&y_digest], |r| r.get::<_,i64>(0)).unwrap(), 0);
        drop(db);
        drop(reopened);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p03_product_store_selection_commit_wins_real_api_contention() {
        use std::{sync::mpsc, thread};
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let (y_pack, x_digest, y_digest) = approve_y(&fixture);
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut selector_store = fixture.store().with_product_registry(registry(
            &x_digest,
            &fixture.pack,
            &y_digest,
            &y_pack,
        ));
        selector_store.arm_product_pin_commit_hook(reached_tx, release_rx);
        let mut busy_admission = fixture.configured_review_store(x_digest.clone());
        busy_admission
            .set_product_pin_test_busy_timeout(Duration::ZERO)
            .unwrap();
        let loser_request = selected_request(
            &fixture,
            "order-a",
            selection_binding(&fixture, "order-a"),
            "race-selection-first",
        );
        let loser_state = snapshot(&fixture.db_path);
        let loser_schema = schema_snapshot(&fixture.db_path);
        let selector_owner = fixture.owner_session.clone();
        let selector_y = y_digest.clone();
        let lifecycle = fixture.item("order-a").admission.lifecycle;
        let now = fixture.now;
        let selector = thread::spawn(move || {
            selector_store.select_order_pack(
                &selector_owner,
                "order-a",
                lifecycle,
                "b2-w1-source",
                &selector_y,
                1,
                "race-select-first",
                now,
            )
        });
        reached_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("selection holds the real ProductStore writer transaction before commit");
        let busy = busy_admission
            .admit_selected_review_activation(loser_request.clone())
            .unwrap_err();
        assert!(
            matches!(
                &busy,
                foundry_exec::HostError::Persistence(message) if message == "database is locked"
            ),
            "admission product API must observe the held selection writer lock exactly: {busy:?}"
        );
        assert_state_and_schema_unchanged(&loser_state, &loser_schema, &fixture.db_path);
        release_tx.send(()).unwrap();
        selector.join().unwrap().expect("Y selection commits first");
        let before_stale = snapshot(&fixture.db_path);
        let before_stale_schema = schema_snapshot(&fixture.db_path);
        let stale = busy_admission
            .admit_selected_review_activation(loser_request)
            .expect_err("the exact busy activation is retried once after Y commits");
        assert!(
            matches!(stale, foundry_exec::HostError::Acceptance(ref message) if message == "selected review activation binding is stale")
        );
        assert_state_and_schema_unchanged(&before_stale, &before_stale_schema, &fixture.db_path);
        drop(busy_admission);
        let reopened_selection = fixture.store();
        {
            let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
            assert_eq!(db.query_row("SELECT digest FROM product_w1_operations_v1 WHERE operation='select' AND order_id='order-a' ORDER BY selection_result_revision DESC LIMIT 1", [], |r| r.get::<_,String>(0)).unwrap(), y_digest);
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='order-a'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM product_review_runs_v1 WHERE order_id='order-a'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM product_review_active_v1 WHERE order_id='order-a'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                0
            );
        }
        drop(reopened_selection);

        let mut y_review = admission_request(&fixture, "order-a");
        y_review.pack_digest = y_digest.clone();
        let y_request = SelectedReviewAdmissionRequest {
            review: y_review,
            binding: ProductSelectionBinding {
                order_id: "order-a".into(),
                lifecycle: fixture.item("order-a").admission.lifecycle,
                source_instance_id: "b2-w1-source".into(),
                pack_digest: y_digest.clone(),
                selection_revision: 2,
                referenced_approval_revision: 1,
            },
            request_key: "race-y-activation".into(),
        };
        let committed_y_request = y_request.clone();
        let mut y_admission_store = fixture
            .configured_review_store(y_digest.clone())
            .with_product_registry(registry(&x_digest, &fixture.pack, &y_digest, &y_pack));
        let admission_started_at = chrono::Utc::now().timestamp();
        let admitted = y_admission_store
            .admit_selected_review_activation(y_request)
            .expect("the activation matching the committed Y selection succeeds");
        let admission_ended_at = chrono::Utc::now().timestamp();
        assert_eq!(admitted.journal_run, "order-a:1:7:1");
        drop(y_admission_store);
        let reopened_after_y = fixture.store();
        assert_atomic_activation_write_set(
            &fixture,
            &committed_y_request,
            &admitted,
            admission_started_at,
            admission_ended_at,
        );
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT pack_digest FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            y_digest
        );
        assert_eq!(db.query_row("SELECT count(*) FROM product_review_host_accept_v1 WHERE order_id='order-a' AND attempt=1 AND run_id=7 AND activation=1", [], |r| r.get::<_,i64>(0)).unwrap(), 1);
        drop(db);
        drop(reopened_after_y);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p01b_product_pin_positive_control_executes() {
        // ProductStore rows and pack snapshot are real; the stage executor is a deterministic local fake.
        let fixture = RuntimeFixture::new(&["order-a"]);
        let calls = Arc::new(AtomicUsize::new(0));
        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: calls.clone(),
            },
        );
        assert!(!held.hold_instance_id.is_empty());
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 1);
    }

    #[test]
    fn p02_two_orders_two_packs_bind_independently() {
        // Both orders pass through ProductStore host admission and a local executor.
        let fixture = RuntimeFixture::new(&["order-a", "order-b"]);
        let y_dir = fixture.root.0.join("pack-y");
        let (y_pack, y_digest) = y_pack_with_changed_lenses(&fixture.pack, &y_dir);
        let x_digest = manifest_digest(&fixture.pack);
        let packs = registry(&x_digest, &fixture.pack, &y_digest, &y_pack);
        let mut store = fixture.store().with_product_registry(packs.clone());
        store
            .change_pack_approval(
                &fixture.owner_session,
                "b2-w1-source",
                &y_digest,
                0,
                true,
                "approve-y",
                fixture.now,
            )
            .unwrap();
        let changed = store
            .select_order_pack(
                &fixture.owner_session,
                "order-b",
                1,
                "b2-w1-source",
                &y_digest,
                1,
                "select-y",
                fixture.now,
            )
            .unwrap();
        assert_eq!(changed.digest, y_digest);
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let x:String=db.query_row("SELECT digest FROM product_w1_operations_v1 WHERE operation='select' AND order_id='order-a' ORDER BY selection_result_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
        let y:String=db.query_row("SELECT digest FROM product_w1_operations_v1 WHERE operation='select' AND order_id='order-b' ORDER BY selection_result_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
        assert_eq!(x, x_digest);
        assert_eq!(y, y_digest);
        assert_ne!(x, y);

        let calls_x = Arc::new(AtomicUsize::new(0));
        let calls_y = Arc::new(AtomicUsize::new(0));
        let lenses_x = Arc::new(std::sync::Mutex::new(Vec::new()));
        let lenses_y = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut host_x = PackReviewHost::with_product_store(
            &fixture.pack,
            ObservedExecutor {
                calls: calls_x.clone(),
                lenses: lenses_x.clone(),
                on_execute: None,
            },
            fixture.store().with_product_registry(packs.clone()),
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        let mut host_y = PackReviewHost::with_selected_product_pack(
            &y_pack,
            ObservedExecutor {
                calls: calls_y.clone(),
                lenses: lenses_y.clone(),
                on_execute: None,
            },
            fixture.store().with_product_registry(packs),
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        host_x
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-a"),
                &fixture.item("order-a").claim,
                selection_binding(&fixture, "order-a"),
                "host-select-order-a",
            )
            .expect("X host executes order A through atomic selected activation");
        host_y
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-b"),
                &fixture.item("order-b").claim,
                ProductSelectionBinding {
                    order_id: "order-b".into(),
                    lifecycle: fixture.item("order-b").admission.lifecycle,
                    source_instance_id: "b2-w1-source".into(),
                    pack_digest: y_digest.clone(),
                    selection_revision: 2,
                    referenced_approval_revision: 1,
                },
                "host-select-order-b",
            )
            .expect("Y host executes order B through atomic selected activation");
        assert!(calls_x.load(Ordering::SeqCst) > 0);
        assert!(calls_y.load(Ordering::SeqCst) > 0);
        let lens_x = lenses_x.lock().unwrap();
        let lens_y = lenses_y.lock().unwrap();
        assert!(!lens_x.is_empty());
        assert!(!lens_y.is_empty());
        assert_ne!(
            lens_x[0], lens_y[0],
            "executor received distinct pack lenses"
        );
        drop(lens_x);
        drop(lens_y);
        drop(host_x);
        drop(host_y);

        let _reopened = fixture.store();
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let run_a: String = db
            .query_row(
                "SELECT pack_digest FROM product_review_runs_v1 WHERE order_id='order-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let run_b: String = db
            .query_row(
                "SELECT pack_digest FROM product_review_runs_v1 WHERE order_id='order-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_a, x_digest);
        assert_eq!(run_b, y_digest);
    }

    #[test]
    fn product_entry_resolves_selected_binding_registry_and_service_key() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let digest = manifest_digest(&fixture.pack);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = PackReviewHost::with_selected_product_pack(
            &fixture.pack,
            ObservedExecutor {
                calls: calls.clone(),
                lenses: Arc::new(std::sync::Mutex::new(Vec::new())),
                on_execute: None,
            },
            fixture.store().with_product_registry(PackRegistryConfig {
                snapshots: [(digest, fixture.pack.clone())].into_iter().collect(),
            }),
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();

        let coordinator_store = fixture.store();
        coordinator_store
            .selected_review_binding(&intent(&fixture, "order-a"), &fixture.item("order-a").claim)
            .expect("production binding resolver accepts the live selected claim")
            .expect("v4 product store has a selected binding");
        application::product_coordinator::dispatch_review(
            &coordinator_store,
            &mut host,
            &intent(&fixture, "order-a"),
            &fixture.item("order-a").claim,
            "product-entry-service-key",
        )
        .expect("the production coordinator reaches atomic selected activation");
        assert!(calls.load(Ordering::SeqCst) > 0);
        let db = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let key: String = db
            .query_row(
                "SELECT request_key FROM product_w1_lifecycle_pins_v1 WHERE order_id='order-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(key, "product-entry-service-key");
    }

    #[test]
    fn selected_host_replay_validates_key_before_terminal_result_shortcut() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let digest = manifest_digest(&fixture.pack);
        let packs = PackRegistryConfig {
            snapshots: [(digest, fixture.pack.clone())].into_iter().collect(),
        };
        let binding = selection_binding(&fixture, "order-a");
        let first_calls = Arc::new(AtomicUsize::new(0));
        let make_host = |calls: Arc<AtomicUsize>| {
            PackReviewHost::with_selected_product_pack(
                &fixture.pack,
                ObservedExecutor {
                    calls,
                    lenses: Arc::new(std::sync::Mutex::new(Vec::new())),
                    on_execute: None,
                },
                fixture.store().with_product_registry(packs.clone()),
                fixture.artifact_root(),
                fixture.session_root(),
                fixture.claim_root(),
            )
            .unwrap()
            .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
            .unwrap()
        };
        make_host(first_calls.clone())
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-a"),
                &fixture.item("order-a").claim,
                binding.clone(),
                "selected-replay-key",
            )
            .unwrap();
        assert!(first_calls.load(Ordering::SeqCst) > 0);

        let replay_calls = Arc::new(AtomicUsize::new(0));
        make_host(replay_calls.clone())
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-a"),
                &fixture.item("order-a").claim,
                binding.clone(),
                "selected-replay-key",
            )
            .expect("exact replay returns the committed terminal result");
        assert_eq!(replay_calls.load(Ordering::SeqCst), 0);

        let conflict_calls = Arc::new(AtomicUsize::new(0));
        let conflict = make_host(conflict_calls.clone())
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-a"),
                &fixture.item("order-a").claim,
                binding,
                "different-selected-key",
            )
            .expect_err("a different key cannot borrow the terminal result");
        assert!(
            matches!(
                conflict,
                foundry_exec::HostError::Acceptance(ref message)
                    if message == "selected review activation request key conflict"
            ),
            "unexpected conflict result: {conflict:?}"
        );
        assert_eq!(conflict_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn selected_host_restart_continues_admitted_activation_without_result() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let digest = manifest_digest(&fixture.pack);
        let binding = selection_binding(&fixture, "order-a");
        fixture
            .configured_review_store(digest.clone())
            .admit_selected_review_activation(selected_request(
                &fixture,
                "order-a",
                binding.clone(),
                "restart-selected-key",
            ))
            .expect("simulate crash after atomic admission and before host execution");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut restarted = PackReviewHost::with_selected_product_pack(
            &fixture.pack,
            ObservedExecutor {
                calls: calls.clone(),
                lenses: Arc::new(std::sync::Mutex::new(Vec::new())),
                on_execute: None,
            },
            fixture.store().with_product_registry(PackRegistryConfig {
                snapshots: [(digest, fixture.pack.clone())].into_iter().collect(),
            }),
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        restarted
            .try_dispatch_selected_product_work(
                &intent(&fixture, "order-a"),
                &fixture.item("order-a").claim,
                binding,
                "restart-selected-key",
            )
            .expect("restarted host safely continues the exact admitted activation");
        assert!(calls.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn feature_on_legacy_host_keeps_fixed_pack_behavior_without_a_pin() {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = PackReviewHost::with_product_store(
            &fixture.pack,
            ObservedExecutor {
                calls: calls.clone(),
                lenses: Arc::new(std::sync::Mutex::new(Vec::new())),
                on_execute: None,
            },
            fixture.store(),
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        host.try_dispatch_product_work(
            &intent(&fixture, "order-a"),
            &fixture.item("order-a").claim,
        )
        .expect("feature-on legacy dispatch remains available");
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 0);
    }

    #[test]
    fn unselected_alternate_pack_rejects_fresh_and_cached_dispatch() {
        struct CountCalls(Arc<AtomicUsize>);
        impl StageExecutor for CountCalls {
            fn ensure_available(&self) -> Result<(), String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            fn execute(&mut self, _: &StageTask) -> Result<StageOutput, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Err("synthetic failure to produce a held result".into())
            }
        }

        let fixture = RuntimeFixture::new(&["order-a"]);
        let (y_pack, x_digest, y_digest) = approve_y(&fixture);
        assert_ne!(x_digest, y_digest);
        let mut store = fixture.store().with_product_registry(registry(
            &x_digest,
            &fixture.pack,
            &y_digest,
            &y_pack,
        ));
        let mut binding = selection_binding(&fixture, "order-a");
        let selected = store
            .select_order_pack(
                &fixture.owner_session,
                "order-a",
                binding.lifecycle,
                &binding.source_instance_id,
                &y_digest,
                binding.selection_revision,
                "select-y",
                fixture.now,
            )
            .unwrap();
        binding.pack_digest = y_digest;
        binding.selection_revision = selected.result_revision;
        binding.referenced_approval_revision = selected.approval_revision.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = PackReviewHost::with_selected_product_pack(
            &y_pack,
            CountCalls(calls.clone()),
            store,
            fixture.artifact_root(),
            fixture.session_root(),
            fixture.claim_root(),
        )
        .unwrap()
        .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
        .unwrap();
        let intent = intent(&fixture, "order-a");
        let claim = &fixture.item("order-a").claim;
        let reject_unselected = |host: &mut PackReviewHost| {
            let before = snapshot(&fixture.db_path);
            let before_calls = calls.load(Ordering::SeqCst);
            let error = host.try_dispatch_product_work(&intent, claim).unwrap_err();
            assert!(
                matches!(error, foundry_exec::HostError::Acceptance(ref message)
                if message == "alternate review pack requires selected activation"),
                "{error:?}"
            );
            assert_snapshot_unchanged(&before, &fixture.db_path);
            assert_eq!(calls.load(Ordering::SeqCst), before_calls);
        };

        assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 0);
        assert_eq!(count(&fixture.db_path, "product_review_host_results_v1"), 0);
        reject_unselected(&mut host);
        let result = host
            .try_dispatch_selected_product_work(&intent, claim, binding, "selected-y")
            .unwrap();
        assert!(result.held.is_some());
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert_eq!(count(&fixture.db_path, "product_w1_lifecycle_pins_v1"), 1);
        assert_eq!(count(&fixture.db_path, "product_review_host_results_v1"), 1);
        reject_unselected(&mut host);
    }

    #[test]
    fn feature_on_genuine_v3_resumes_accepted_fixed_pack_hold() {
        let path = std::path::PathBuf::from(std::env::var_os("W1_V3_PREIMAGE").unwrap());
        let mut fixture = RuntimeFixture::with_preimage(
            &["order-a"],
            Some((
                &path,
                foundry_ledger::ProductConfig {
                    installation_id: "w1-genuine-v3-install".into(),
                    database_id: "w1-genuine-v3-database".into(),
                },
            )),
        );
        fixture.root.arm_failure_preservation();
        let schema = schema_snapshot(&path);
        assert_eq!(schema_snapshot(&fixture.db_path), schema);
        assert!(
            !schema
                .iter()
                .any(|(_, name, _, _)| name.starts_with("product_w1_"))
        );
        let version: i64 = rusqlite::Connection::open(&fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 3);

        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: Arc::default(),
            },
        );
        let receipt = fixture.accept_resume(&held, "legacy-v3-resume");
        use foundry_exec::ReviewHostStore;
        for (session, digest, expected) in [
            (
                "invalid-session",
                manifest_digest(&fixture.pack),
                "current review authority is unavailable",
            ),
            (
                fixture.owner_session.as_str(),
                "b".repeat(64),
                "accepted evolving snapshot does not match immutable origin evidence",
            ),
        ] {
            let error = fixture
                .configured_review_store(digest)
                .claim_accepted_hold(session, &receipt.operation_id, fixture.now, &mut |_, _| {
                    panic!("denied v3 resume reached preflight")
                })
                .unwrap_err();
            assert!(
                matches!(error, foundry_exec::HostError::Acceptance(ref message) if message == expected),
                "{error:?}"
            );
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = fixture.host(SuccessExecutor {
            calls: calls.clone(),
            lens_bytes: None,
            after_preflight: None,
            after_execute: None,
        });
        host.continue_accepted_product_hold(
            &fixture.owner_session,
            &receipt.operation_id,
            fixture.now,
        )
        .expect("pin-enabled host resumes the genuine v3 accepted hold");
        assert!(calls.load(Ordering::SeqCst) > 0);
        drop(host);
        let progress = fixture
            .store()
            .product_review_progress(&fixture.owner_session, &receipt.operation_id, fixture.now)
            .unwrap();
        assert_eq!(progress.receipt, receipt);
        assert_eq!(progress.phase, "completed");
        assert_eq!(schema_snapshot(&fixture.db_path), schema);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn changed_work_fence_refuses_reserved_stage_collection() {
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: Arc::default(),
            },
        );
        let receipt = fixture.accept_resume(&held, "changed-work-fence");
        let db = fixture.db_path.clone();
        let run = format!(
            "{}:{}:{}:{}",
            receipt.order_id, receipt.attempt, receipt.reserved_run_id, receipt.activation
        );
        let reserved_run = run.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let mut host = fixture.host(ObservedContinuationExecutor {
            calls: calls.clone(),
            restores: Arc::default(),
            after_execute: Arc::new(move |task| {
                let observed = reserved_stage_observation(
                    &db, "order-a", &reserved_run, &task.stage_id, observed_calls.load(Ordering::SeqCst),
                );
                assert_eq!((observed.accepted_rows, observed.complete_rows), (1, 0));
                let changed = rusqlite::Connection::open(&db).unwrap().execute(
                    "UPDATE product_work_claims_v1 SET fence=fence+1 WHERE order_id='order-a' AND state='claimed'",
                    [],
                ).unwrap();
                assert_eq!(changed, 1);
            }),
        });
        let err = host
            .continue_accepted_product_hold(
                &fixture.owner_session,
                &receipt.operation_id,
                fixture.now,
            )
            .expect_err("collection must reject a reassigned work fence");
        assert!(
            matches!(err, foundry_exec::HostError::Worker(ref message)
            if message.contains("accepted continuation worker owner or fence was reassigned")),
            "{err:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let stages: (i64, i64) = rusqlite::Connection::open(&fixture.db_path).unwrap().query_row(
            "SELECT count(*),count(CASE WHEN status='accepted' AND output_json IS NULL THEN 1 END)
             FROM product_review_stages_v1 WHERE order_id='order-a' AND run_id=?1",
            [&run], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(stages, (1, 1));
        drop(host);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p01b_revoke_after_final_reserved_stage_collects_and_completes() {
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
            },
        );
        let accepted = fixture.accept_resume(&held, "revoked-resume-final-stage");
        let before_stages = count(&fixture.db_path, "product_review_stages_v1");
        let calls = Arc::new(AtomicUsize::new(0));
        let revoke_path = fixture.db_path.clone();
        let revoke_config = fixture.config.clone();
        let revoke_owner = fixture.owner_session.clone();
        let revoke_digest = manifest_digest(&fixture.pack);
        let revoke_now = fixture.now;
        let reserved_order = accepted.order_id.clone();
        let reserved_run = format!(
            "{}:{}:{}:{}",
            accepted.order_id, accepted.attempt, accepted.reserved_run_id, accepted.activation
        );
        let revoke_observation = Arc::new(std::sync::Mutex::new(None));
        let observed_at_revoke = revoke_observation.clone();
        let observed_calls = calls.clone();
        let revoke = Arc::new(move |task: &StageTask| {
            let observation = reserved_stage_observation(
                &revoke_path,
                &reserved_order,
                &reserved_run,
                &task.stage_id,
                observed_calls.load(Ordering::SeqCst),
            );
            *observed_at_revoke.lock().unwrap() = Some(observation);
            foundry_ledger::ProductStore::open(&revoke_path, revoke_config.clone())
                .unwrap()
                .change_pack_approval(
                    &revoke_owner,
                    "b2-w1-source",
                    &revoke_digest,
                    1,
                    false,
                    "revoke-x-final-stage",
                    revoke_now,
                )
                .expect("revoke after the final reserved stage executes");
        });
        let mut host = fixture.host(FinalPassExecutor {
            calls: calls.clone(),
            revoke_after_final_execute: revoke,
        });
        host.continue_accepted_product_hold(
            &fixture.owner_session,
            &accepted.operation_id,
            fixture.now,
        )
        .expect("a reserved final stage is collected and completes after revoke");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            5,
            "fixture reaches its final stage"
        );
        assert_eq!(
            count(&fixture.db_path, "product_review_stages_v1"),
            before_stages + 5,
            "all five reserved continuation stages are complete"
        );
        let at_revoke = revoke_observation
            .lock()
            .unwrap()
            .clone()
            .expect("final reserved stage is observed immediately before revoke");
        assert_eq!(at_revoke.executor_calls, 5);
        assert_eq!(at_revoke.stage_rows, before_stages + 5);
        assert_eq!((at_revoke.accepted_rows, at_revoke.complete_rows), (1, 0));
        let after_revoke = reserved_stage_observation(
            &fixture.db_path,
            &accepted.order_id,
            &format!(
                "{}:{}:{}:{}",
                accepted.order_id, accepted.attempt, accepted.reserved_run_id, accepted.activation
            ),
            &at_revoke.stage_id,
            calls.load(Ordering::SeqCst),
        );
        assert_eq!(after_revoke.stage_id, at_revoke.stage_id);
        assert_eq!(after_revoke.executor_calls, at_revoke.executor_calls);
        assert_eq!(after_revoke.stage_rows, at_revoke.stage_rows);
        assert_eq!(
            (after_revoke.accepted_rows, after_revoke.complete_rows),
            (0, 1),
            "the exact stage reserved before revoke becomes durably complete without another stage or executor call"
        );
        let progress = fixture
            .store()
            .product_review_progress(&fixture.owner_session, &accepted.operation_id, fixture.now)
            .expect("read completed final-stage progress");
        assert_eq!(progress.phase, "completed");
        assert_eq!(progress.receipt, accepted);
        fixture.root.cleanup_on_success();
    }

    #[test]
    fn p01b_revoke_preserves_reserved_stage_then_blocks_next_stage() {
        // ProductStore hold/stage rows are durable; the fake executor is not a real worker process.
        let mut fixture = RuntimeFixture::new(&["order-a"]);
        fixture.root.arm_failure_preservation();
        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
            },
        );
        let before = count(&fixture.db_path, "product_review_stages_v1");
        let accepted = fixture.accept_resume(&held, "revoked-resume-non-final");
        let revoke_path = fixture.db_path.clone();
        let revoke_config = fixture.config.clone();
        let revoke_owner = fixture.owner_session.clone();
        let revoke_digest = manifest_digest(&fixture.pack);
        let revoke_now = fixture.now;
        let reserved_order = accepted.order_id.clone();
        let reserved_run = format!(
            "{}:{}:{}:{}",
            accepted.order_id, accepted.attempt, accepted.reserved_run_id, accepted.activation
        );
        let revoke_observation = Arc::new(std::sync::Mutex::new(None));
        let observed_at_revoke = revoke_observation.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let revoke = Arc::new(move |task: &StageTask| {
            let observation = reserved_stage_observation(
                &revoke_path,
                &reserved_order,
                &reserved_run,
                &task.stage_id,
                observed_calls.load(Ordering::SeqCst),
            );
            *observed_at_revoke.lock().unwrap() = Some(observation);
            foundry_ledger::ProductStore::open(&revoke_path, revoke_config.clone())
                .unwrap()
                .change_pack_approval(
                    &revoke_owner,
                    "b2-w1-source",
                    &revoke_digest,
                    1,
                    false,
                    "revoke-x-non-final",
                    revoke_now,
                )
                .expect("revoke after non-final stage reservation");
        });
        let restores = Arc::new(AtomicUsize::new(0));
        let mut host = fixture.host(ObservedContinuationExecutor {
            calls: calls.clone(),
            restores: restores.clone(),
            after_execute: revoke,
        });
        let outcome = host.continue_accepted_product_hold(
            &fixture.owner_session,
            &accepted.operation_id,
            fixture.now,
        );
        assert!(
            outcome.is_err(),
            "the next stage is denied after revocation"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "only the reserved stage executes"
        );
        assert_eq!(
            (
                restores.load(Ordering::SeqCst),
                foundry_exec::DispatchHost::dispatch_count(&host),
            ),
            (0, 0),
            "the denied next edge performs no session restore or new host dispatch"
        );
        assert_eq!(
            count(&fixture.db_path, "product_review_stages_v1"),
            before + 1,
            "the already-reserved stage is collected after revoke"
        );
        let at_revoke = revoke_observation
            .lock()
            .unwrap()
            .clone()
            .expect("non-final reserved stage is observed immediately before revoke");
        assert_eq!(at_revoke.executor_calls, 1);
        assert_eq!(at_revoke.stage_rows, before + 1);
        assert_eq!((at_revoke.accepted_rows, at_revoke.complete_rows), (1, 0));
        let after_revoke = reserved_stage_observation(
            &fixture.db_path,
            &accepted.order_id,
            &format!(
                "{}:{}:{}:{}",
                accepted.order_id, accepted.attempt, accepted.reserved_run_id, accepted.activation
            ),
            &at_revoke.stage_id,
            calls.load(Ordering::SeqCst),
        );
        assert_eq!(after_revoke.stage_id, at_revoke.stage_id);
        assert_eq!(after_revoke.executor_calls, at_revoke.executor_calls);
        assert_eq!(after_revoke.stage_rows, at_revoke.stage_rows);
        assert_eq!(
            (after_revoke.accepted_rows, after_revoke.complete_rows),
            (0, 1),
            "the exact stage reserved before revoke becomes durably complete without another stage or executor call"
        );
        let progress = fixture
            .store()
            .product_review_progress(&fixture.owner_session, &accepted.operation_id, fixture.now)
            .expect("read durable denial progress");
        assert_eq!(progress.phase, "recovery_required");
        assert_eq!(progress.receipt, accepted);
        assert_eq!(
            count(&fixture.db_path, "product_review_hold_acceptances_v1"),
            1,
            "accepted fence remains available for recovery"
        );
        let effects_before_retry = (
            calls.load(Ordering::SeqCst),
            restores.load(Ordering::SeqCst),
            foundry_exec::DispatchHost::dispatch_count(&host),
            count(&fixture.db_path, "product_review_stages_v1"),
            count(&fixture.db_path, "product_review_runs_v1"),
        );
        assert!(
            host.continue_accepted_product_hold(
                &fixture.owner_session,
                &accepted.operation_id,
                fixture.now,
            )
            .is_err(),
            "durable denial is replayed"
        );
        assert_eq!(
            effects_before_retry,
            (
                calls.load(Ordering::SeqCst),
                restores.load(Ordering::SeqCst),
                foundry_exec::DispatchHost::dispatch_count(&host),
                count(&fixture.db_path, "product_review_stages_v1"),
                count(&fixture.db_path, "product_review_runs_v1"),
            ),
            "retry creates no stage, restore, dispatch, or executor effect"
        );
        drop(host);
        fixture.root.cleanup_on_success();
    }
}

#[test]
fn r1_resume_reopens_the_same_product_store_and_continues_the_accepted_hold() {
    // The observation uses actual ProductStore review rows and a fake local StageExecutor.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let failed_stages = Arc::new(AtomicUsize::new(0));
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: failed_stages.clone(),
        },
    );
    assert!(failed_stages.load(Ordering::SeqCst) > 0);
    let receipt = fixture.accept_resume(&held, "r1-accepted-resume");
    let resumed_stages = Arc::new(AtomicUsize::new(0));
    // A new ProductStore and PackReviewHost instance is the explicit substitute
    // observation for process restart; this test does not claim an OS restart.
    let mut host = fixture.host(SuccessExecutor {
        calls: resumed_stages.clone(),
        lens_bytes: None,
        after_preflight: None,
        after_execute: None,
    });
    host.continue_accepted_product_hold(&fixture.owner_session, &receipt.operation_id, fixture.now)
        .expect("accepted Resume continues on a newly opened instance");
    assert!(resumed_stages.load(Ordering::SeqCst) > 0);
    let progress = fixture
        .store()
        .product_review_progress(&fixture.owner_session, &receipt.operation_id, fixture.now)
        .expect("read accepted progress");
    assert_eq!(progress.phase, "completed");
    assert_eq!(
        count(&fixture.db_path, "product_review_hold_acceptances_v1"),
        1
    );
    assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 2);
}

#[test]
fn r1_absent_or_mutated_snapshot_rejects_before_execution_and_preserves_receipt() {
    // Actual ProductStore state is compared; stage execution remains a fake boundary.
    for missing in [true, false] {
        let fixture = RuntimeFixture::new(&["order-a"]);
        let held = fixture.make_hold(
            "order-a",
            FailingExecutor {
                calls: Arc::new(AtomicUsize::new(0)),
            },
        );
        let receipt =
            fixture.accept_resume(&held, if missing { "r1-missing" } else { "r1-mutated" });
        let snapshot_before = snapshot(&fixture.db_path);
        let snapshot_dir = fixture.snapshot_dir(&held);
        if missing {
            fs::rename(&snapshot_dir, fixture.root.0.join("moved-away-snapshot"))
                .expect("make the test snapshot unavailable");
        } else {
            let lens = snapshot_dir.join("lens-edge.md");
            let mut bytes = fs::read(&lens).expect("read frozen test lens");
            bytes.extend_from_slice(b"\ncorrupt test snapshot\n");
            fs::write(lens, bytes).expect("mutate private test snapshot");
        }
        let calls = Arc::new(AtomicUsize::new(0));
        let mut host = fixture.host(SuccessExecutor {
            calls: calls.clone(),
            lens_bytes: None,
            after_preflight: None,
            after_execute: None,
        });
        assert!(
            host.continue_accepted_product_hold(
                &fixture.owner_session,
                &receipt.operation_id,
                fixture.now
            )
            .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "executor must not run");
        assert_snapshot_unchanged(&snapshot_before, &fixture.db_path);
        let progress = fixture
            .store()
            .product_review_progress(&fixture.owner_session, &receipt.operation_id, fixture.now)
            .expect("accepted receipt remains readable");
        assert!(
            progress.receipt == receipt,
            "original acceptance receipt changed"
        );
        assert_eq!(progress.phase, "accepted");
        assert_eq!(count(&fixture.db_path, "product_review_host_results_v1"), 1);
    }
}

#[test]
fn r1_verified_x_survives_y_replacement_after_preflight() {
    // ProductStore rows are real; only the stage executor and pack replacement are synthetic.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let receipt = fixture.accept_resume(&held, "r1-x-verified-before-y");
    let y_dir = fixture.root.0.join("pack-y");
    let (y_pack, y_digest) = y_pack_with_changed_lenses(&fixture.pack, &y_dir);
    let snapshot_dir = fixture.snapshot_dir(&held);
    let calls = Arc::new(AtomicUsize::new(0));
    let replace_snapshot = {
        let y_pack = y_pack.clone();
        let snapshot_dir = snapshot_dir.clone();
        Arc::new(move || copy_y_lenses_into_snapshot(&y_pack, &snapshot_dir))
    };

    // This constructor receives a store configured for Y and the X pack path.
    // The accepted X constructor behavior overwrites that setting with X; the
    // later executor callback replaces persisted bytes only after preflight.
    let y_configured_store = fixture.configured_review_store(y_digest.clone());
    let mut host = PackReviewHost::with_product_store(
        &fixture.pack,
        SuccessExecutor {
            calls: calls.clone(),
            lens_bytes: None,
            after_preflight: Some(replace_snapshot),
            after_execute: None,
        },
        y_configured_store,
        fixture.artifact_root(),
        fixture.session_root(),
        fixture.claim_root(),
    )
    .expect("X constructor accepts X path and replaces prior Y setting")
    .with_source_commit("67aa6c2bbea68ddec015cd8a3ec6fc7d26f837b0")
    .expect("set synthetic source commit");
    host.continue_accepted_product_hold(&fixture.owner_session, &receipt.operation_id, fixture.now)
        .expect("verified X pack executes after persisted snapshot is replaced by Y");
    assert!(calls.load(Ordering::SeqCst) > 0);
    assert!(
        manifest_digest(&snapshot_dir) == y_digest,
        "replacement snapshot has the Y manifest digest"
    );
    assert!(
        y_digest != manifest_digest(&fixture.pack),
        "Y manifest digest differs from frozen X"
    );
    let progress = fixture
        .store()
        .product_review_progress(&fixture.owner_session, &receipt.operation_id, fixture.now)
        .expect("read replacement continuation progress");
    assert_eq!(progress.phase, "completed");

    // Y is a valid but differently digested pack; the fixed-digest host constructor rejects it.
    let rejected = PackReviewHost::with_product_store(
        &y_pack,
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
        fixture.store(),
        fixture.root.0.join("y-artifacts"),
        fixture.root.0.join("y-sessions"),
        fixture.root.0.join("y-claims"),
    );
    let rejected = match rejected {
        Err(error) => error,
        Ok(_) => panic!("Y pack itself passed the fixed digest check"),
    };
    let rejection_text = format!("{rejected:?}");
    assert!(
        rejection_text.contains("unexpected review pack digest"),
        "Y pack must be rejected by the fixed digest comparison"
    );
}

#[test]
fn r2_two_order_control_and_external_order_repository_actor_tuple_denials() {
    // Both orders use the p2-test-support preparation path; ProductStore and review rows are real.
    let fixture = RuntimeFixture::new(&["order-a", "order-b"]);
    assert_eq!(fixture.items.len(), 2);
    assert_eq!(fixture.item("order-a").admission.order_id, "order-a");
    assert_eq!(fixture.item("order-b").admission.order_id, "order-b");
    assert!(!fixture.item("order-a").delivery_id.is_empty());
    assert!(!fixture.item("order-b").delivery_id.is_empty());
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let held_a = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: calls_a.clone(),
        },
    );
    let held_b = fixture.make_hold(
        "order-b",
        FailingExecutor {
            calls: calls_b.clone(),
        },
    );
    assert_eq!(held_a.originating_run, held_a.expected.originating_run);
    let control = fixture
        .store()
        .accept_product_review_hold(
            &fixture.owner_session,
            &held_b.order_id,
            &held_b.hold_instance_id,
            &held_b.expected,
            "r2-normal-order-b",
            ProductHoldDecision::CommitPass,
            fixture.now,
        )
        .expect("normal second-order decision control");
    assert_eq!(control.order_id, "order-b");

    let stranger_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("viewer-password")
            .expect("hash test-only viewer password");
    let mut setup = fixture.store();
    setup
        .create_account(
            &fixture.owner_session,
            "viewer",
            "viewer",
            &stranger_phc,
            fixture.now,
        )
        .expect("create non-reviewer actor");
    let stranger = setup
        .login("viewer", "viewer-password", fixture.now)
        .expect("login non-reviewer actor")
        .digest()
        .to_owned();
    drop(setup);

    let executor_calls_before = (
        calls_a.load(Ordering::SeqCst),
        calls_b.load(Ordering::SeqCst),
    );
    let before_order = snapshot(&fixture.db_path);
    let order_error = fixture.store().accept_product_review_hold(
        &fixture.owner_session,
        "order-b",
        &held_a.hold_instance_id,
        &held_a.expected,
        "r2-order-axis",
        ProductHoldDecision::CommitPass,
        fixture.now,
    );
    let order_error = match order_error {
        Err(error) => error,
        Ok(_) => panic!("external order substitution was accepted"),
    };
    assert_redacted_error(order_error, &fixture);
    assert_snapshot_unchanged(&before_order, &fixture.db_path);

    let before_repository = snapshot(&fixture.db_path);
    let repository_error = match fixture.store().authenticate_product_service_for_action(
        &fixture.service_token,
        "github-primary",
        "https://api.github.com",
        "repo:outside",
        foundry_ledger::ProductServiceAction::DriveReview,
        fixture.now,
    ) {
        Err(error) => error,
        Ok(_) => panic!("stable repository identity substitution was accepted"),
    };
    let repository_error_text = repository_error.to_string();
    assert_redacted_error(repository_error_text, &fixture);
    assert_snapshot_unchanged(&before_repository, &fixture.db_path);

    let before_actor = snapshot(&fixture.db_path);
    let actor_error = fixture.store().accept_product_review_hold(
        &stranger,
        &held_a.order_id,
        &held_a.hold_instance_id,
        &held_a.expected,
        "r2-actor-axis",
        ProductHoldDecision::CommitPass,
        fixture.now,
    );
    let actor_error = match actor_error {
        Err(error) => error,
        Ok(_) => panic!("non-reviewer session was accepted"),
    };
    assert_redacted_error(actor_error, &fixture);
    assert_snapshot_unchanged(&before_actor, &fixture.db_path);

    let mut foreign_tuple = held_a.expected.clone();
    foreign_tuple.activation += 1;
    let before_tuple = snapshot(&fixture.db_path);
    let tuple_error = fixture.store().accept_product_review_hold(
        &fixture.owner_session,
        &held_a.order_id,
        &held_a.hold_instance_id,
        &foreign_tuple,
        "r2-tuple-axis",
        ProductHoldDecision::CommitPass,
        fixture.now,
    );
    let tuple_error = match tuple_error {
        Err(error) => error,
        Ok(_) => panic!("one changed tuple coordinate was accepted"),
    };
    assert_redacted_error(tuple_error, &fixture);
    assert_snapshot_unchanged(&before_tuple, &fixture.db_path);
    assert_eq!(
        (
            calls_a.load(Ordering::SeqCst),
            calls_b.load(Ordering::SeqCst)
        ),
        executor_calls_before,
        "denials do not call the fake executor"
    );
}

#[test]
fn r3_constructor_digest_and_ledger_configuration_checks_are_separate() {
    // Actual ProductStore hold, snapshot and run rows; constructor checks do not invoke a fake executor.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let receipt = fixture.accept_resume(&held, "r3-digest-check");
    let y_dir = fixture.root.0.join("r3-pack-y");
    let (y_pack, y_digest) = y_pack_with_changed_lenses(&fixture.pack, &y_dir);
    assert!(
        y_digest != manifest_digest(&fixture.pack),
        "Y manifest digest differs from frozen X"
    );

    // Observation 1: passing Y bytes as the constructor pack is rejected by the fixed digest check.
    let wrong_host_calls = Arc::new(AtomicUsize::new(0));
    let rejected = PackReviewHost::with_product_store(
        &y_pack,
        FailingExecutor {
            calls: wrong_host_calls.clone(),
        },
        fixture.store(),
        fixture.root.0.join("wrong-artifacts"),
        fixture.root.0.join("wrong-sessions"),
        fixture.root.0.join("wrong-claims"),
    );
    let rejected = match rejected {
        Err(error) => error,
        Ok(_) => panic!("Y host constructor unexpectedly succeeded"),
    };
    let rejection_text = format!("{rejected:?}");
    assert!(rejection_text.contains("unexpected review pack digest"));
    assert_eq!(wrong_host_calls.load(Ordering::SeqCst), 0);

    // Observation 2: a Y-configured store plus X path is accepted; the constructor
    // overwrites the setting to X. R1 separately proves only captured X reaches execution.
    let y_configured_store = fixture.configured_review_store(y_digest.clone());
    let x_host = PackReviewHost::with_product_store(
        &fixture.pack,
        SuccessExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            lens_bytes: None,
            after_preflight: None,
            after_execute: None,
        },
        y_configured_store,
        fixture.artifact_root(),
        fixture.session_root(),
        fixture.claim_root(),
    );
    assert!(
        x_host.is_ok(),
        "X path is accepted and rewrites Y settings to X"
    );

    // Observation 3: ledger/config mismatch is checked by the ledger store independently
    // of the host constructor's pack-file validation.
    use foundry_exec::ReviewHostStore;
    let mut ledger_y = fixture.configured_review_store(y_digest);
    let before = snapshot(&fixture.db_path);
    let mut preflight_calls = 0usize;
    let mismatch = ledger_y.claim_accepted_hold(
        &fixture.owner_session,
        &receipt.operation_id,
        fixture.now,
        &mut |_, _| {
            preflight_calls += 1;
            Ok(())
        },
    );
    assert!(mismatch.is_err());
    assert_eq!(
        preflight_calls, 0,
        "ledger rejects run/config mismatch before preflight"
    );
    assert_snapshot_unchanged(&before, &fixture.db_path);
}

#[tokio::test]
async fn r3_missing_host_is_503_before_acceptance_and_202_for_accepted_retry() {
    // Actual HTTP router and ProductStore are observed; no executor exists in this missing-host case.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let state = foundry_serve::auth_http::ProductHttpState::new(fixture.store());
    let (client, server) = start_http(state.clone());
    let body = request_body(&held, "resume", Some(&held.resume_target));
    let before_new = snapshot(&fixture.db_path);
    let (new_status, _) = post_accept(
        &client,
        &fixture.owner_bearer,
        &held.order_id,
        hold_id(&held),
        "r3-missing-host",
        &body,
    )
    .await;
    assert_eq!(new_status, 503);
    assert_snapshot_unchanged(&before_new, &fixture.db_path);
    assert_eq!(
        count(&fixture.db_path, "product_review_hold_acceptances_v1"),
        0
    );

    let receipt = state
        .store
        .lock()
        .await
        .accept_product_review_hold(
            &fixture.owner_session,
            &held.order_id,
            hold_id(&held),
            &held.expected,
            "r3-missing-host",
            ProductHoldDecision::Resume(held.resume_target.clone()),
            fixture.now,
        )
        .expect("seed already accepted nonterminal retry");
    let before_retry = snapshot(&fixture.db_path);
    let (retry_status, retry_body) = post_accept(
        &client,
        &fixture.owner_bearer,
        &held.order_id,
        hold_id(&held),
        "r3-missing-host",
        &body,
    )
    .await;
    assert_eq!(retry_status, 202);
    let returned: serde_json::Value =
        serde_json::from_str(&retry_body).expect("retry JSON response");
    let expected_receipt = serde_json::to_value(&receipt).expect("serialize receipt");
    assert!(
        returned["receipt"] == expected_receipt,
        "retry returned a different accepted receipt"
    );
    assert_snapshot_unchanged(&before_retry, &fixture.db_path);
    assert_eq!(
        count(&fixture.db_path, "product_review_hold_acceptances_v1"),
        1
    );
    server.abort();
}

#[tokio::test]
async fn r4_exact_retry_returns_receipt_and_terminal_branches_skip_host_lock() {
    // Actual HTTP router and ledger receipts; executor counts identify the fake stage boundary.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let host = fixture.host(SuccessExecutor {
        calls: calls.clone(),
        lens_bytes: None,
        after_preflight: None,
        after_execute: None,
    });
    let state = foundry_serve::auth_http::ProductHttpState::with_review_host(fixture.store(), host);
    let (client, server) = start_http(state.clone());
    let commit_body = request_body(&held, "commit_pass", None);
    let (first_status, first_body) = post_accept(
        &client,
        &fixture.owner_bearer,
        &held.order_id,
        hold_id(&held),
        "r4-terminal-commit",
        &commit_body,
    )
    .await;
    assert_eq!(first_status, 200);
    let first: serde_json::Value = serde_json::from_str(&first_body).expect("first decision JSON");
    let receipt = first["receipt"].clone();
    assert_eq!(
        state
            .store
            .lock()
            .await
            .product_review_progress(
                &fixture.owner_session,
                receipt["operation_id"].as_str().unwrap(),
                fixture.now
            )
            .expect("read completed control")
            .phase,
        "completed"
    );
    let before_retry = snapshot(&fixture.db_path);
    let lock = state.review_host.lock().await;
    let (retry_status, retry_body) = tokio::time::timeout(
        Duration::from_secs(2),
        post_accept(
            &client,
            &fixture.owner_bearer,
            &held.order_id,
            hold_id(&held),
            "r4-terminal-commit",
            &commit_body,
        ),
    )
    .await
    .expect("completed retry must not enter review_host mutex");
    drop(lock);
    assert_eq!(retry_status, 200);
    let retried: serde_json::Value = serde_json::from_str(&retry_body).expect("retry JSON");
    assert!(
        retried["receipt"] == receipt,
        "completed retry returned a different receipt"
    );
    assert_snapshot_unchanged(&before_retry, &fixture.db_path);
    assert_eq!(
        count(&fixture.db_path, "product_review_hold_acceptances_v1"),
        1
    );
    assert_eq!(count(&fixture.db_path, "product_review_runs_v1"), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let before_conflict = snapshot(&fixture.db_path);
    let changed_body = request_body(&held, "commit_fail", None);
    let (changed_status, _) = post_accept(
        &client,
        &fixture.owner_bearer,
        &held.order_id,
        hold_id(&held),
        "r4-terminal-commit",
        &changed_body,
    )
    .await;
    assert_eq!(changed_status, 409);
    assert_snapshot_unchanged(&before_conflict, &fixture.db_path);
    server.abort();
}

#[tokio::test]
async fn r4_recovery_required_retry_keeps_receipt_and_does_not_reenter_host() {
    // Actual ProductStore acceptance and recovery rows; the HTTP retry is the observed boundary.
    let fixture = RuntimeFixture::new(&["order-a"]);
    let held = fixture.make_hold(
        "order-a",
        FailingExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
        },
    );
    let receipt = fixture.accept_resume(&held, "r4-recovery-resume");
    use foundry_exec::ReviewHostStore;
    let mut recovery_store = fixture.configured_review_store(manifest_digest(&fixture.pack));
    let mut preflight = |snapshot_root: &Path, expected_digest: &str| {
        let snapshot = snapshot_root.join("pack-snapshot");
        assert!(snapshot.is_dir(), "accepted pack snapshot exists");
        assert!(
            manifest_digest(&snapshot) == expected_digest,
            "accepted pack snapshot digest matches"
        );
        Ok(())
    };
    let accepted = recovery_store
        .claim_accepted_hold(
            &fixture.owner_session,
            &receipt.operation_id,
            fixture.now,
            &mut preflight,
        )
        .expect("claim accepted hold against its real frozen snapshot");
    recovery_store
        .restore_hold_after_resume_error(accepted.binding.clone())
        .expect("persist recovery-required phase through ProductStore");
    drop(recovery_store);

    let calls = Arc::new(AtomicUsize::new(0));
    let host = fixture.host(SuccessExecutor {
        calls: calls.clone(),
        lens_bytes: None,
        after_preflight: None,
        after_execute: None,
    });
    let state = foundry_serve::auth_http::ProductHttpState::with_review_host(fixture.store(), host);
    let (client, server) = start_http(state.clone());
    let body = request_body(&held, "resume", Some(&held.resume_target));
    assert_eq!(
        state
            .store
            .lock()
            .await
            .product_review_progress(&fixture.owner_session, &receipt.operation_id, fixture.now)
            .expect("read recovery control")
            .phase,
        "recovery_required"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let before_retry = snapshot(&fixture.db_path);
    let lock = state.review_host.lock().await;
    let (retry_status, retry_body) = tokio::time::timeout(
        Duration::from_secs(2),
        post_accept(
            &client,
            &fixture.owner_bearer,
            &held.order_id,
            hold_id(&held),
            "r4-recovery-resume",
            &body,
        ),
    )
    .await
    .expect("recovery_required retry must skip review_host mutex");
    drop(lock);
    assert_eq!(retry_status, 202);
    let retried: serde_json::Value =
        serde_json::from_str(&retry_body).expect("recovery retry JSON");
    let expected_receipt = serde_json::to_value(&receipt).expect("serialize original receipt");
    assert!(
        retried["receipt"] == expected_receipt,
        "recovery retry returned a different accepted receipt"
    );
    assert_eq!(retried["progress"]["phase"], "recovery_required");
    assert_snapshot_unchanged(&before_retry, &fixture.db_path);
    assert_eq!(
        count(&fixture.db_path, "product_review_hold_acceptances_v1"),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

// NOT-CLAIMED: registry ordering, runtime-loss injection, and full W1 completion are outside this slice.

#[derive(Clone)]
struct LocalRouterClient {
    accepted: tokio::sync::mpsc::Sender<tokio::io::DuplexStream>,
}

struct LocalRouterListener {
    accepted: tokio::sync::mpsc::Receiver<tokio::io::DuplexStream>,
}

impl axum::serve::Listener for LocalRouterListener {
    type Io = tokio::io::DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        (
            self.accepted.recv().await.expect("local request stream"),
            (),
        )
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(())
    }
}

fn start_http(
    state: Arc<foundry_serve::auth_http::ProductHttpState>,
) -> (LocalRouterClient, tokio::task::JoinHandle<()>) {
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let listener = LocalRouterListener { accepted: receiver };
    let router = foundry_serve::review_hold::product_router(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("serve in-memory product router");
    });
    (LocalRouterClient { accepted: sender }, server)
}

async fn post_accept(
    client: &LocalRouterClient,
    bearer: &str,
    order: &str,
    hold_id: &str,
    request_key: &str,
    body: &serde_json::Value,
) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = serde_json::to_vec(body).expect("encode local HTTP test request");
    let request = format!(
        "POST /api/product/orders/{order}/review/holds/{hold_id}/accept HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {bearer}\r\nIdempotency-Key: {request_key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let (mut stream, server_stream) = tokio::io::duplex(16 * 1024);
    client
        .accepted
        .send(server_stream)
        .await
        .expect("submit in-memory request stream");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write local HTTP headers");
    stream
        .write_all(&body)
        .await
        .expect("write local HTTP body");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read local HTTP response");
    let response = String::from_utf8(response).expect("HTTP response UTF-8");
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .expect("HTTP response boundary");
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("HTTP status code");
    (status, body.to_owned())
}
