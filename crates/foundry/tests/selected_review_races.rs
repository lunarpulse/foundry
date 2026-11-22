//! Separate product-store processes, synchronized at the real commit hooks.
#![cfg(all(feature = "p2-test-support", feature = "product-pack-pins", unix))]

use chrono::{Duration as Lease, Utc};
use foundry_exec::{
    Attempt, Epoch, ExecutionIntent, Generation, HostError, OrderId, ReviewAdmissionRequest,
    ReviewAdmissionStore, RunId, SelectedReviewAdmissionRequest,
};
use foundry_ledger::product::{CommitReviewRequest, CommitReviewWork};
use foundry_ledger::{
    PackRegistryConfig, ProductConfig, ProductReviewConfig, ProductServiceAction, ProductStore,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Deserialize)]
struct Operand {
    root: PathBuf,
    order: String,
    admission: String,
    commit: String,
    admission_operand: bool,
    hold_commit: bool,
}

fn wait(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(45),
            "barrier timed out: {path:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_operand(dir: &Path) {
    let job: Operand = serde_json::from_slice(&fs::read(dir.join("job.json")).unwrap()).unwrap();
    let root = &job.root;
    let x = root.join("pack-x");
    let y = root.join("pack-y");
    let x_digest = foundry_pack::load_pack(&x).unwrap().digest().to_owned();
    let y_digest = foundry_pack::load_pack(&y).unwrap().digest().to_owned();
    let mut store = ProductStore::create_product_pin_store(
        &root.join("product.sqlite"),
        &ProductConfig {
            installation_id: "review-test".into(),
            database_id: "review-db".into(),
        },
    )
    .unwrap()
    .with_product_registry(PackRegistryConfig {
        snapshots: [(x_digest.clone(), x), (y_digest.clone(), y)]
            .into_iter()
            .collect(),
    });
    let token = fs::read_to_string(root.join("owner.session")).unwrap();
    let session = hex::encode(Sha256::digest(token.trim().as_bytes()));
    let request_key = format!("admit-{}", job.order);
    let selection_key = format!("select-y-{}", job.order);
    let request = if job.admission_operand {
        let credential = fs::read_to_string(root.join("service.credential")).unwrap();
        let service = store
            .authenticate_product_service_for_action(
                credential.trim(),
                "github-primary",
                "https://api.github.com",
                "42",
                ProductServiceAction::DriveReview,
                Utc::now(),
            )
            .unwrap();
        let (binding, claim) = store
            .prepare_commit_review(
                &session,
                &service,
                &CommitReviewRequest {
                    order: &job.order,
                    lifecycle: 1,
                    admission_hash: &job.admission,
                    commit: &job.commit,
                    selection_revision: 1,
                    request_key: &request_key,
                },
                true,
            )
            .unwrap();
        let CommitReviewWork::Dispatch(claim) = claim else {
            panic!("expected unpinned scope-bound claim")
        };
        store
            .configure_review_backend(ProductReviewConfig {
                artifact_root: root.join("artifacts"),
                session_root: root.join("sessions"),
                claim_root: root.join("claims"),
                pack_digest: x_digest,
            })
            .unwrap();
        let work = claim.review_binding();
        Some(SelectedReviewAdmissionRequest {
            review: ReviewAdmissionRequest {
                intent: ExecutionIntent {
                    order: OrderId(job.order.clone()),
                    attempt: Attempt(1),
                    run: RunId(1),
                    activation: 1,
                    generation: Generation(0),
                    epoch: Epoch(work.fence as u64),
                    recorded_at: claim.lease_until() - Lease::minutes(30),
                    external_ref: format!("review:{}:1:{}..{}", job.order, job.commit, job.commit),
                },
                base_commit: job.commit.clone(),
                result_commit: job.commit.clone(),
                pack_digest: binding.pack_digest.clone(),
                product_work: Some(work),
            },
            binding,
            request_key,
        })
    } else {
        None
    };
    store
        .set_product_pin_test_busy_timeout(Duration::ZERO)
        .unwrap();
    if job.hold_commit {
        let (reached, reached_rx) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        store.arm_product_pin_commit_hook(reached, release);
        let barrier = dir.to_owned();
        thread::spawn(move || {
            reached_rx.recv_timeout(Duration::from_secs(45)).unwrap();
            fs::write(barrier.join("before-commit"), b"").unwrap();
            wait(&barrier.join("release"));
            release_tx.send(()).unwrap();
        });
    }
    fs::write(dir.join("ready"), b"").unwrap();
    for attempt in 1..=2 {
        wait(&dir.join(format!("go-{attempt}")));
        let value: Value = if let Some(request) = &request {
            match store.admit_selected_review_activation(request.clone()) {
                Ok(admission) => json!({"status":"committed","run":admission.journal_run}),
                Err(HostError::Persistence(message)) if message == "database is locked" => {
                    json!({"status":"busy","error":message})
                }
                Err(error) => json!({"status":"denied","error":format!("{error:?}")}),
            }
        } else {
            match store.select_order_pack(
                &session,
                &job.order,
                1,
                "commit-review-source",
                &y_digest,
                1,
                &selection_key,
                Utc::now(),
            ) {
                Ok(receipt) => {
                    json!({"status":"committed","receipt":hex::encode(receipt.canonical)})
                }
                Err(foundry_ledger::ProductError::Database(rusqlite::Error::SqliteFailure(
                    error,
                    _,
                ))) if error.code == rusqlite::ErrorCode::DatabaseBusy => {
                    json!({"status":"busy","sqlite_code":error.extended_code})
                }
                Err(error) => json!({"status":"denied","error":error.to_string()}),
            }
        };
        let tmp = dir.join("writing");
        fs::write(&tmp, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::rename(tmp, dir.join(format!("reply-{attempt}"))).unwrap();
    }
}

#[test]
fn first_pin_across_processes() {
    if let Some(dir) = std::env::var_os("FOUNDRY_W1_RACE_OPERAND") {
        run_operand(Path::new(&dir));
        return;
    }
    let driver =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/selected-review-cli.py");
    let status = std::process::Command::new("python3")
        .arg(driver)
        .arg(env!("CARGO_BIN_EXE_foundry"))
        .arg("--race-worker")
        .arg(std::env::current_exe().unwrap())
        .status()
        .unwrap();
    assert!(status.success());
}
