#![cfg(feature = "p2-test-support")]

#[path = "support/b2_w1.rs"]
mod b2_w1;

use b2_w1::{
    FailingExecutor, RuntimeFixture, SuccessExecutor, assert_snapshot_unchanged,
    copy_y_lenses_into_snapshot, count, hold_id, manifest_digest, request_body, snapshot,
    y_pack_with_changed_lenses,
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
