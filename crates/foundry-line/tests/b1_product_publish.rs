use chrono::Utc;
use foundry_domain::Order;
use foundry_domain::ports::{PortResult, PublishOutcome, Publisher};
use foundry_ledger::{
    ProductApprovalReceipt, ProductCapability, ProductConfig, ProductPublishStart, ProductStore,
    ScopeMutation,
};
use foundry_line::product::{
    ProductProjection, ProductPublishReconcileOutcome, ProductPublishReconciler,
    ProductPublishResult,
};
use rusqlite::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[path = "support/product_result_fixture.rs"]
mod product_result_fixture;
use product_result_fixture::{FixtureIdentity, genuine_host_done};

struct LocalPublisher {
    db: std::path::PathBuf,
    calls: AtomicUsize,
    outcome: Mutex<Option<PublishOutcome>>,
    /// Operation-keyed PR registry: one PR per operation_id. A retry of the
    /// same operation reuses its PR (idempotent); only a distinct operation
    /// opens a distinct PR.
    opened_prs: Arc<Mutex<std::collections::HashMap<String, LocalPr>>>,
    /// Race hook: when armed with (db, install, database, url), the next
    /// publish() call performs a GENUINE concurrent settle of the same URL
    /// from a separate ProductStore connection at call entry — simulating
    /// caller B settling while caller A's external call is in flight.
    interleave_settle: Mutex<Option<(std::path::PathBuf, String, String, String)>>,
}

impl LocalPublisher {
    fn distinct_prs(&self) -> usize {
        self.opened_prs.lock().unwrap().len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalPrState {
    Open,
    ClosedMerged,
    ClosedUnmerged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LocalPr {
    url: String,
    state: LocalPrState,
}

impl LocalPublisher {
    fn register_pr(&self, operation_id: &str, url: &str, state: LocalPrState) {
        self.opened_prs.lock().unwrap().insert(
            operation_id.to_owned(),
            LocalPr {
                url: url.to_owned(),
                state,
            },
        );
    }
}

impl Publisher for LocalPublisher {
    fn publish(
        &self,
        order: &Order,
        operation_id: &str,
        expected_digest: &str,
    ) -> impl std::future::Future<Output = PortResult<PublishOutcome>> + Send {
        assert_eq!(order.state(), foundry_domain::OrderState::Publishing);
        assert!(!operation_id.is_empty());
        assert_eq!(order.artifact_digest.as_deref(), Some(expected_digest));
        let pending: i64 = Connection::open(&self.db)
            .unwrap()
            .query_row(
                "SELECT count(*) FROM product_publish_outcomes_v1 WHERE operation_id=?1 AND state='pending'",
                [operation_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            pending, 1,
            "publish-start must be committed before callback"
        );
        let race = self.interleave_settle.lock().unwrap().take();
        let outcome = if let Some((db, install, database, url)) = race {
            // Concurrent caller B settles the same external effect from a
            // separate connection while this call is in flight.
            let mut other = ProductStore::open(
                &db,
                ProductConfig {
                    installation_id: install,
                    database_id: database,
                },
            )
            .unwrap();
            other.settle_product_publish(operation_id, &url).unwrap();
            drop(other);
            {
                let mut opened = self.opened_prs.lock().unwrap();
                opened.insert(
                    operation_id.to_string(),
                    LocalPr {
                        url: url.clone(),
                        state: LocalPrState::Open,
                    },
                );
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            PublishOutcome::Confirmed { pr_url: url }
        } else {
            {
                let mut opened = self.opened_prs.lock().unwrap();
                let seq = opened.len() + 1;
                opened
                    .entry(operation_id.to_string())
                    .or_insert_with(|| LocalPr {
                        url: format!("https://example.invalid/pr/open-{seq}"),
                        state: LocalPrState::Open,
                    });
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcome.lock().unwrap().take().unwrap()
        };
        async move { Ok(outcome) }
    }
}

#[derive(Clone)]
struct LocalReconciler {
    calls: Arc<AtomicUsize>,
    outcome: Arc<Mutex<ProductPublishReconcileOutcome>>,
    registry: Option<Arc<Mutex<std::collections::HashMap<String, LocalPr>>>>,
    /// Race hook: when armed with (db, install, database, url), the next
    /// reconcile() call performs a GENUINE concurrent settle of that URL
    /// from a separate ProductStore connection BEFORE returning its outcome
    /// — simulating caller B confirming while this reconciler is in flight.
    interleave_settle: Arc<Mutex<Option<(std::path::PathBuf, String, String, String)>>>,
}

impl LocalReconciler {
    fn set_outcome(&self, outcome: ProductPublishReconcileOutcome) {
        *self.outcome.lock().unwrap() = outcome;
    }
}

impl ProductPublishReconciler for LocalReconciler {
    fn reconcile(
        &self,
        operation_id: &str,
    ) -> impl std::future::Future<Output = PortResult<ProductPublishReconcileOutcome>> + Send {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((db, install, database, url)) = self.interleave_settle.lock().unwrap().take() {
            let mut other = ProductStore::open(
                &db,
                ProductConfig {
                    installation_id: install,
                    database_id: database,
                },
            )
            .unwrap();
            other.settle_product_publish(operation_id, &url).unwrap();
        }
        let outcome = self
            .registry
            .as_ref()
            .and_then(|registry| registry.lock().unwrap().get(operation_id).cloned())
            .map(|pr| match pr.state {
                LocalPrState::Open | LocalPrState::ClosedMerged => {
                    ProductPublishReconcileOutcome::Confirmed { pr_url: pr.url }
                }
                LocalPrState::ClosedUnmerged => {
                    ProductPublishReconcileOutcome::Rejected { pr_url: pr.url }
                }
            })
            .unwrap_or_else(|| self.outcome.lock().unwrap().clone());
        async move { Ok(outcome) }
    }
}

struct ApprovedFixture {
    fixture: product_result_fixture::GenuineResultFixture,
    store: ProductStore,
    receipt: ProductApprovalReceipt,
}

fn approved_fixture(tag: &str, add_publish_scope: bool) -> ApprovedFixture {
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: format!("publish-{tag}-install"),
            database_id: format!("publish-{tag}-db"),
        },
        format!("publish-{tag}-source"),
        format!("publish-{tag}-order"),
    );
    let mut fixture = genuine_host_done(identity);
    let mut store = fixture.result_store.take().unwrap();
    let finalized = store
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();
    let token_digest = "9".repeat(64);
    let grant_id = format!("{}-approval-grant", fixture.identity.order_id);
    fixture.approval.grant_id = grant_id;
    fixture.approval.token_digest = token_digest;
    fixture.approval.artifact_digest = fixture.terminal.as_ref().unwrap().result_commit.clone();
    fixture.approval.tuple_digest = finalized.tuple_digest;
    if add_publish_scope {
        store
            .change_product_scope(
                &fixture.owner_session_digest,
                "reader",
                &fixture.identity.source_instance_id,
                ProductCapability::PublishApproved,
                ScopeMutation::Grant,
                2,
                &format!("publish-{tag}-scope-grant"),
                fixture.now,
            )
            .unwrap();
    }
    store
        .issue_approval_grant(
            &fixture.human_session_digest,
            &fixture.approval.grant_id,
            &fixture.approval.token_digest,
            &fixture.identity.order_id,
            fixture.now,
        )
        .unwrap();
    let proof = store
        .prepare_approval(&fixture.human_session_digest, &fixture.approval)
        .unwrap();
    let receipt = store
        .approve_product(&fixture.human_session_digest, &fixture.approval, &proof)
        .unwrap();
    // The durable composition reconstructs its bound order from the receipt;
    // projection catch-up is neither available nor required under scope.
    ApprovedFixture {
        fixture,
        store,
        receipt,
    }
}

fn publish_counts(db: &Path) -> (i64, i64, i64) {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT count(*),
                    coalesce(sum(CASE WHEN state='pending' THEN 1 ELSE 0 END),0),
                    coalesce(sum(CASE WHEN state='confirmed' THEN 1 ELSE 0 END),0)
             FROM product_publish_outcomes_v1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn stored_publish_row(db: &Path, operation_id: &str) -> (String, String) {
    Connection::open(db)
        .unwrap()
        .query_row(
            "SELECT state,receipt_json FROM product_publish_outcomes_v1 WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

fn close(mut fixture: ApprovedFixture) {
    drop(fixture.store);
    drop(fixture.fixture.host.take());
    std::fs::remove_dir_all(fixture.fixture.root).unwrap();
}

/// Append a hash-chain-valid post-approval `PublishConfirmed`
/// (publishing → shipped) journal row using the SAME primitives production
/// uses for journal writes (canonical event JSON + v2 row hash), following
/// the accepted append_g05_retry_chain precedent. The chain stays
/// verifiable: verified_order_state_in replays and hash-checks the whole
/// journal, so the appended row must be — and is — correctly chained.
fn append_publish_confirmed(
    db_path: &Path,
    order_id: &str,
    lifecycle: i64,
    operation_id: &str,
    pr_url: &str,
    at: chrono::DateTime<Utc>,
) {
    use rusqlite::params;
    let mut conn = Connection::open(db_path).unwrap();
    let event = foundry_domain::OrderEvent::PublishConfirmed {
        pr_url: pr_url.to_owned(),
    };
    let event_text = format!("{event:?}");
    let event_json = Some(foundry_ledger::event_json::event_to_json(&event));
    let (next_seq, prev_hash, actor): (i64, String, String) = conn
        .query_row(
            "SELECT COALESCE(MAX(seq),0)+1,\
             COALESCE((SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1),'GENESIS'),\
             (SELECT actor FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1)\
             FROM ledger",
            params![order_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let at_text = at.to_rfc3339();
    let row = foundry_ledger::sqlite::LedgerRow {
        seq: next_seq,
        order_id: order_id.to_owned(),
        from_state: "publishing".into(),
        to_state: "shipped".into(),
        event: event_text,
        actor,
        intent: operation_id.to_owned(),
        at: at_text.clone(),
        prev_hash: prev_hash.clone(),
        hash: String::new(),
        event_json,
    };
    let hash =
        foundry_ledger::sqlite::SqliteLedger::row_hash_at(&[row.clone()]).expect("v2 row hash");
    // Journal appends under ProductAtomicV1 require a pending transition
    // intent for the order (consumed by the AFTER INSERT trigger) — the same
    // requirement the accepted append_g05_retry_chain helper satisfies.
    conn.execute(
        "INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation)\
         VALUES(?1,?2,?3,'publish-journal-advance-preparation')",
        params![order_id, lifecycle, next_seq - 1],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash)\
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            row.order_id,
            row.from_state,
            row.to_state,
            row.event,
            row.actor,
            row.intent,
            at_text,
            prev_hash,
            row.event_json,
            hash
        ],
    )
    .unwrap();
}

#[test]
fn revalidation_and_capability_gate_before_any_start() {
    let mut good = approved_fixture("u1-good", true);
    let wire = serde_json::to_string(&good.receipt).unwrap();
    assert_eq!(
        good.store
            .begin_product_publish(
                &good.fixture.human_session_digest,
                &good.receipt,
                good.fixture.now,
            )
            .unwrap(),
        ProductPublishStart::Started
    );
    let stored: String = Connection::open(&good.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT receipt_json FROM product_publish_outcomes_v1 WHERE operation_id=?1",
            [&good.receipt.operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored.as_bytes(), wire.as_bytes());
    close(good);

    let mut forged = approved_fixture("u1-forged", true);
    let mut changed = forged.receipt.clone();
    changed.artifact_digest.push('x');
    assert!(
        forged
            .store
            .begin_product_publish(
                &forged.fixture.human_session_digest,
                &changed,
                forged.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&forged.fixture.db_path), (0, 0, 0));
    close(forged);

    let mut missing = approved_fixture("u1-missing-scope", false);
    assert!(
        missing
            .store
            .begin_product_publish(
                &missing.fixture.human_session_digest,
                &missing.receipt,
                missing.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&missing.fixture.db_path), (0, 0, 0));
    close(missing);

    // Foreign vector: a genuinely committed approval receipt from a REAL
    // second fixture (real order, real lifecycle) is offered to the first
    // store under the first store's valid session — rejection must come from
    // scoped capability (no such order/scope for this principal), not from an
    // invented identifier.
    let mut other = approved_fixture("u1-foreign-other", true);
    let mut foreign = approved_fixture("u1-foreign-order", true);
    assert!(
        foreign
            .store
            .begin_product_publish(
                &foreign.fixture.human_session_digest,
                &other.receipt,
                foreign.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&foreign.fixture.db_path), (0, 0, 0));
    close(other);
    close(foreign);

    let mut revoked = approved_fixture("u1-revoked-scope", true);
    let revision: i64 = Connection::open(&revoked.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='reader' AND source_instance_id=?1",
            [&revoked.fixture.identity.source_instance_id],
            |row| row.get(0),
        )
        .unwrap();
    revoked
        .store
        .change_product_scope(
            &revoked.fixture.owner_session_digest,
            "reader",
            &revoked.fixture.identity.source_instance_id,
            ProductCapability::PublishApproved,
            ScopeMutation::Revoke,
            revision,
            "publish-u1-revoke",
            revoked.fixture.now,
        )
        .unwrap();
    assert!(
        revoked
            .store
            .begin_product_publish(
                &revoked.fixture.human_session_digest,
                &revoked.receipt,
                revoked.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&revoked.fixture.db_path), (0, 0, 0));
    close(revoked);

    // Stale vector: the supplied receipt stays byte-identical to the committed
    // approval; the durable SOURCE state genuinely advances (reconfigure with
    // enabled=true bumps the source revision), so rejection must come from
    // lifecycle/source revalidation — not from receipt equality.
    let mut stale = approved_fixture("u1-stale-source", true);
    let stale_source_revision: i64 = Connection::open(&stale.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&stale.fixture.identity.connector_id],
            |row| row.get(0),
        )
        .unwrap();
    let stale_identity = foundry_ledger::RepositoryIdentity::canonicalized(
        "acme",
        "github",
        "https://api.github.com",
        &stale.fixture.identity.repository_id,
    )
    .unwrap();
    stale
        .store
        .configure_scoped_source(
            &stale.fixture.owner_session_digest,
            &stale.fixture.identity.connector_id,
            stale_identity,
            "acme",
            "repo",
            &stale.fixture.identity.repository_id,
            "open+foundry:make",
            true,
            stale_source_revision,
            "publish-u1-stale-source",
            stale.fixture.now,
        )
        .unwrap();
    let bumped: i64 = Connection::open(&stale.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&stale.fixture.identity.connector_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_ne!(
        bumped, stale.receipt.source_revision,
        "reconfigure must advance the source revision"
    );
    assert!(
        stale
            .store
            .begin_product_publish(
                &stale.fixture.human_session_digest,
                &stale.receipt,
                stale.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&stale.fixture.db_path), (0, 0, 0));
    close(stale);

    let mut disabled = approved_fixture("u1-disabled-source", true);
    let source_revision: i64 = Connection::open(&disabled.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&disabled.fixture.identity.connector_id],
            |row| row.get(0),
        )
        .unwrap();
    let identity = foundry_ledger::RepositoryIdentity::canonicalized(
        "acme",
        "github",
        "https://api.github.com",
        &disabled.fixture.identity.repository_id,
    )
    .unwrap();
    disabled
        .store
        .configure_scoped_source(
            &disabled.fixture.owner_session_digest,
            &disabled.fixture.identity.connector_id,
            identity,
            "acme",
            "repo",
            &disabled.fixture.identity.repository_id,
            "open+foundry:make",
            false,
            source_revision,
            "publish-u1-disable-source",
            disabled.fixture.now,
        )
        .unwrap();
    assert!(
        disabled
            .store
            .begin_product_publish(
                &disabled.fixture.human_session_digest,
                &disabled.receipt,
                disabled.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&disabled.fixture.db_path), (0, 0, 0));
    close(disabled);

    // A not-yet-approved, queued projection cannot write a start row. The
    // session is VALID and the publish capability is genuinely granted, so
    // rejection must come from receipt/lifecycle revalidation (no committed
    // approval for this order) — not from session resolution.
    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "publish-u1-not-publishing-install".into(),
            database_id: "publish-u1-not-publishing-db".into(),
        },
        "publish-u1-not-publishing-source",
        "publish-u1-not-publishing-order",
    );
    let mut queued = genuine_host_done(identity);
    let mut queued_store = queued.result_store.take().unwrap();
    queued_store
        .change_product_scope(
            &queued.owner_session_digest,
            "reader",
            &queued.identity.source_instance_id,
            ProductCapability::PublishApproved,
            ScopeMutation::Grant,
            2,
            "publish-u1-not-publishing-scope",
            queued.now,
        )
        .unwrap();
    // Scoped stores refuse the unscoped projection snapshot; the durable
    // composition no longer requires it.
    let mut queued_receipt = good_receipt_placeholder();
    queued_receipt.order_id = queued.identity.order_id.clone();
    let publisher = LocalPublisher {
        db: queued.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/1".into(),
        })),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::Unknown)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut queued_store,
                &queued.human_session_digest,
                &queued_receipt,
                &publisher,
                &reconciler,
                Utc::now(),
            ))
            .is_err()
    );
    assert_eq!(publish_counts(&queued.db_path), (0, 0, 0));
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    drop(queued_store);
    drop(queued.host.take());
    std::fs::remove_dir_all(queued.root).unwrap();

    // Foreign-scope vector (intra-store): the receipt is the store's OWN
    // genuinely committed approval (the order EXISTS locally, receipt lookup
    // and source binding both pass) and the session is VALID — but the
    // principal's PublishApproved grant was genuinely issued for a SECOND
    // real source only, so rejection must come from scoped capability
    // (never-scoped, distinct from the revoked vector), not from missing
    // provenance or session resolution.
    let mut foreign_scope = approved_fixture("u1-foreign-scope", false);
    let second_connector = format!("{}-second", foreign_scope.fixture.identity.connector_id);
    let second_repository = format!(
        "{}-second-repo",
        foreign_scope.fixture.identity.repository_id
    );
    let second_identity = foundry_ledger::RepositoryIdentity::canonicalized(
        "acme",
        "github",
        "https://api.github.com",
        &second_repository,
    )
    .unwrap();
    foreign_scope
        .store
        .configure_scoped_source(
            &foreign_scope.fixture.owner_session_digest,
            &second_connector,
            second_identity,
            "acme",
            "repo",
            &second_repository,
            "open+foundry:make",
            true,
            0,
            "publish-u1-foreign-scope-register",
            foreign_scope.fixture.now,
        )
        .unwrap();
    let second_instance: String = Connection::open(&foreign_scope.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT source_instance_id FROM product_scope_source_instances_v1 WHERE connector_id=?1",
            [&second_connector],
            |row| row.get(0),
        )
        .unwrap();
    let second_revision: i64 = {
        use rusqlite::OptionalExtension;
        Connection::open(&foreign_scope.fixture.db_path)
            .unwrap()
            .query_row(
                "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='reader' AND source_instance_id=?1",
                [&second_instance],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .unwrap_or(0)
    };
    foreign_scope
        .store
        .change_product_scope(
            &foreign_scope.fixture.owner_session_digest,
            "reader",
            &second_instance,
            ProductCapability::PublishApproved,
            ScopeMutation::Grant,
            second_revision,
            "publish-u1-foreign-scope-grant",
            foreign_scope.fixture.now,
        )
        .unwrap();
    // The order's own source carries NO grant for this principal: the only
    // grant genuinely issued sits on the second source.
    assert!(
        foreign_scope
            .store
            .begin_product_publish(
                &foreign_scope.fixture.human_session_digest,
                &foreign_scope.receipt,
                foreign_scope.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&foreign_scope.fixture.db_path), (0, 0, 0));
    close(foreign_scope);

    // Non-publishing / journal-advanced vector (rubric U1 ③+⑥): the receipt
    // stays byte-identical and committed, the source binding stays current,
    // but the verified S1 journal has genuinely advanced past the approval
    // (a correctly chained PublishConfirmed publishing→shipped row appended
    // with the production hashing primitives). Rejection must come from the
    // S1 revalidation predicate (replay state 'shipped', head advanced) —
    // the predicate itself is exercised, not short-circuited.
    let mut advanced = approved_fixture("u1-journal-advanced", true);
    let head_before: i64 = Connection::open(&advanced.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT MAX(seq) FROM ledger WHERE order_id=?1",
            [&advanced.fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    append_publish_confirmed(
        &advanced.fixture.db_path,
        &advanced.fixture.identity.order_id,
        advanced.receipt.lifecycle,
        &advanced.receipt.operation_id,
        "https://example.invalid/pr/advanced",
        advanced.fixture.now,
    );
    let (to_state, head_after): (String, i64) = Connection::open(&advanced.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT to_state, seq FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1",
            [&advanced.fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(to_state, "shipped");
    assert!(head_after > head_before);
    assert!(head_after > advanced.receipt.sequence);
    assert!(
        advanced
            .store
            .begin_product_publish(
                &advanced.fixture.human_session_digest,
                &advanced.receipt,
                advanced.fixture.now,
            )
            .is_err()
    );
    assert_eq!(publish_counts(&advanced.fixture.db_path), (0, 0, 0));
    close(advanced);
}

fn good_receipt_placeholder() -> ProductApprovalReceipt {
    ProductApprovalReceipt {
        operation_id: "not-a-real-operation".into(),
        actor_id: "reader".into(),
        order_id: String::new(),
        lifecycle: 2,
        sequence: 6,
        event_hash: "a".repeat(64),
        artifact_digest: "b".repeat(40),
        attempt: 1,
        run_id: 7,
        generation: 0,
        tuple_digest: "c".repeat(64),
        source_revision: 1,
    }
}

#[test]
fn start_commits_before_external_call_and_survives_crash() {
    let mut approved = approved_fixture("u2", true);
    assert_eq!(
        approved
            .store
            .begin_product_publish(
                &approved.fixture.human_session_digest,
                &approved.receipt,
                approved.fixture.now,
            )
            .unwrap(),
        ProductPublishStart::Started
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
    // Forced interruption: the store handle is leaked without any graceful
    // close (no destructor, no WAL checkpoint) — the durable pending start
    // must survive on its own.
    std::mem::forget(approved.store);
    let mut reopened =
        ProductStore::open(&approved.fixture.db_path, approved.fixture.config.clone()).unwrap();
    // First external attempt is lost after reaching the publisher.
    let publisher = LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Unknown {
            operation_id: approved.receipt.operation_id.clone(),
        })),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::NotFound)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut reopened,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Unknown {
            operation_id: approved.receipt.operation_id.clone()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.distinct_prs(), 1);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
    // Idempotent retry of the SAME operation: reconciler still NotFound, the
    // durable row is still pending, so the fence permits exactly one more
    // publisher call — and the operation-keyed registry proves it reuses the
    // SAME PR (one distinct PR for this operation).
    publisher
        .outcome
        .lock()
        .unwrap()
        .replace(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/2".into(),
        });
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut reopened,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/2".into()
        }
    );
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 2);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        publisher.distinct_prs(),
        1,
        "retry of the same operation must reuse its PR"
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    let url: String = Connection::open(&approved.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT pr_url FROM product_publish_outcomes_v1 WHERE operation_id=?1",
            [&approved.receipt.operation_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(url, "https://example.invalid/pr/2");
    // Post-confirmation retry: no publisher invocation, no new PR.
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut reopened,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/2".into()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
    assert_eq!(publisher.distinct_prs(), 1);
    drop(reopened);
    drop(approved.fixture.host.take());
    std::fs::remove_dir_all(approved.fixture.root).unwrap();
}

#[test]
fn response_loss_unknown_keeps_pending_and_reconciles() {
    let mut approved = approved_fixture("u3", true);
    let publisher = LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Unknown {
            operation_id: approved.receipt.operation_id.clone(),
        })),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/3".into(),
        })),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Unknown {
            operation_id: approved.receipt.operation_id.clone()
        }
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    // Pending reconciliation returning Unknown MUST keep the operation
    // pending and MUST NOT invoke the publisher again: Unknown is never
    // treated as absence (no external retry on Unknown).
    reconciler.set_outcome(ProductPublishReconcileOutcome::Unknown);
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Unknown {
            operation_id: approved.receipt.operation_id.clone()
        }
    );
    assert_eq!(
        publisher.calls.load(Ordering::SeqCst),
        1,
        "Unknown reconciliation must not re-execute"
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
    // Reconciliation later observing the confirmed effect settles and returns
    // it without any further publisher invocation.
    reconciler.set_outcome(ProductPublishReconcileOutcome::Confirmed {
        pr_url: "https://example.invalid/pr/3".into(),
    });
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/3".into()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 2);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    close(approved);
}

#[test]
fn confirmed_is_never_reexecuted() {
    let mut approved = approved_fixture("u4", true);
    let publisher = LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/4".into(),
        })),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::Unknown)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .is_ok()
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    // Concurrent same-URL settlement is idempotent success; a DIFFERENT URL
    // for the confirmed operation is rejected.
    assert!(
        approved
            .store
            .settle_product_publish(
                &approved.receipt.operation_id,
                "https://example.invalid/pr/4"
            )
            .is_ok()
    );
    assert!(
        approved
            .store
            .settle_product_publish(
                &approved.receipt.operation_id,
                "https://example.invalid/pr/other"
            )
            .is_err()
    );
    assert!(Connection::open(&approved.fixture.db_path)
        .unwrap()
        .execute(
            "UPDATE product_publish_outcomes_v1 SET pr_url='https://example.invalid/pr/other' WHERE operation_id=?1",
            [&approved.receipt.operation_id],
        )
        .is_err());
    assert!(
        Connection::open(&approved.fixture.db_path)
            .unwrap()
            .execute(
                "DELETE FROM product_publish_outcomes_v1 WHERE operation_id=?1",
                [&approved.receipt.operation_id],
            )
            .is_err()
    );
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/4".into()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 0);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    // Accepted-effect distinction (U4/P1): after the operation is confirmed,
    // revoking the publish capability or disabling the source must NOT turn a
    // retry into an error — the stored confirmed URL is an existing effect,
    // retrievable without fresh execution authority (the session still
    // authenticates).
    let revoke_revision: i64 = Connection::open(&approved.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id='reader' AND source_instance_id=?1",
            [&approved.fixture.identity.source_instance_id],
            |row| row.get(0),
        )
        .unwrap();
    approved
        .store
        .change_product_scope(
            &approved.fixture.owner_session_digest,
            "reader",
            &approved.fixture.identity.source_instance_id,
            ProductCapability::PublishApproved,
            ScopeMutation::Revoke,
            revoke_revision,
            "publish-u4-post-confirm-revoke",
            approved.fixture.now,
        )
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/4".into()
        }
    );
    let disable_revision: i64 = Connection::open(&approved.fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&approved.fixture.identity.connector_id],
            |row| row.get(0),
        )
        .unwrap();
    let disable_identity = foundry_ledger::RepositoryIdentity::canonicalized(
        "acme",
        "github",
        "https://api.github.com",
        &approved.fixture.identity.repository_id,
    )
    .unwrap();
    approved
        .store
        .configure_scoped_source(
            &approved.fixture.owner_session_digest,
            &approved.fixture.identity.connector_id,
            disable_identity,
            "acme",
            "repo",
            &approved.fixture.identity.repository_id,
            "open+foundry:make",
            false,
            disable_revision,
            "publish-u4-post-confirm-disable",
            approved.fixture.now,
        )
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/4".into()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 0);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    close(approved);
}

fn start_pending_publish(approved: &mut ApprovedFixture) {
    assert_eq!(
        approved
            .store
            .begin_product_publish(
                &approved.fixture.human_session_digest,
                &approved.receipt,
                approved.fixture.now,
            )
            .unwrap(),
        ProductPublishStart::Started
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
}

fn reconcile_with_registry(
    registry: Arc<Mutex<std::collections::HashMap<String, LocalPr>>>,
) -> LocalReconciler {
    LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::NotFound)),
        registry: Some(registry),
        interleave_settle: Arc::new(Mutex::new(None)),
    }
}

fn local_publisher(approved: &ApprovedFixture) -> LocalPublisher {
    LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/must-not-publish".into(),
        })),
    }
}

/// W1: a pending local operation reconciles an already-existing operation PR
/// to the same URL and settles exactly once without opening another PR.
#[test]
fn existing_pr_reconciles_confirmed() {
    let mut approved = approved_fixture("w1-existing-pr", true);
    start_pending_publish(&mut approved);
    let publisher = local_publisher(&approved);
    let url = format!(
        "https://example.invalid/pr/{}",
        approved.receipt.operation_id
    );
    publisher.register_pr(&approved.receipt.operation_id, &url, LocalPrState::Open);
    let registry = publisher.opened_prs.clone();
    let registry_before = registry.lock().unwrap().clone();
    let receipt_json =
        stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id).1;
    let reconciler = reconcile_with_registry(registry.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed { pr_url: url }
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    assert_eq!(
        stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id).1,
        receipt_json
    );
    assert_eq!(*registry.lock().unwrap(), registry_before);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(publisher.distinct_prs(), 1);
    close(approved);
}

/// W2: a closed and merged PR is an accepted external effect and recovers via
/// the same Confirmed idempotent path, without a new publish call.
#[test]
fn closed_merged_reconciles_confirmed() {
    let mut approved = approved_fixture("w2-closed-merged", true);
    start_pending_publish(&mut approved);
    let publisher = local_publisher(&approved);
    let url = format!(
        "https://example.invalid/pr/{}",
        approved.receipt.operation_id
    );
    publisher.register_pr(
        &approved.receipt.operation_id,
        &url,
        LocalPrState::ClosedMerged,
    );
    let registry = publisher.opened_prs.clone();
    let registry_before = registry.lock().unwrap().clone();
    let receipt_json =
        stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id).1;
    let reconciler = reconcile_with_registry(registry.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();

    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed { pr_url: url }
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    assert_eq!(
        stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id).1,
        receipt_json
    );
    assert_eq!(*registry.lock().unwrap(), registry_before);
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
    assert_eq!(publisher.distinct_prs(), 1);
    close(approved);
}

/// W3: closed without merge is an explicit business rejection. It never
/// settles the pending row or invokes Publisher; replay returns the same
/// result with the durable receipt and external registry unchanged.
#[test]
fn closed_unmerged_rejected_keeps_pending() {
    let mut approved = approved_fixture("w3-closed-unmerged", true);
    start_pending_publish(&mut approved);
    let publisher = local_publisher(&approved);
    let url = format!(
        "https://example.invalid/pr/{}",
        approved.receipt.operation_id
    );
    publisher.register_pr(
        &approved.receipt.operation_id,
        &url,
        LocalPrState::ClosedUnmerged,
    );
    let registry = publisher.opened_prs.clone();
    let registry_before = registry.lock().unwrap().clone();
    let row_before = stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id);
    let reconciler = reconcile_with_registry(registry.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let expected = ProductPublishResult::Rejected { pr_url: url };

    for _ in 0..2 {
        assert_eq!(
            runtime
                .block_on(ProductProjection::publish_product_approved(
                    &mut approved.store,
                    &approved.fixture.human_session_digest,
                    &approved.receipt,
                    &publisher,
                    &reconciler,
                    approved.fixture.now,
                ))
                .unwrap(),
            expected
        );
        assert_eq!(
            stored_publish_row(&approved.fixture.db_path, &approved.receipt.operation_id),
            row_before
        );
        assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
        assert_eq!(*registry.lock().unwrap(), registry_before);
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 0);
        assert_eq!(publisher.distinct_prs(), 1);
    }
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 2);
    close(approved);
}

/// Child role for the SIGKILL crash cell. Runs ONLY when the orchestrator
/// spawns this test binary with the B1_PUBLISH_CRASH_* environment armed;
/// in normal suite runs it is an immediate no-op.
#[test]
fn u2_crash_child() {
    let Ok(db) = std::env::var("B1_PUBLISH_CRASH_DB") else {
        return;
    };
    let config = ProductConfig {
        installation_id: std::env::var("B1_PUBLISH_CRASH_INSTALL").unwrap(),
        database_id: std::env::var("B1_PUBLISH_CRASH_DATABASE").unwrap(),
    };
    let receipt: ProductApprovalReceipt =
        serde_json::from_str(&std::env::var("B1_PUBLISH_CRASH_RECEIPT").unwrap()).unwrap();
    let session = std::env::var("B1_PUBLISH_CRASH_SESSION").unwrap();
    let now = chrono::DateTime::parse_from_rfc3339(&std::env::var("B1_PUBLISH_CRASH_AT").unwrap())
        .unwrap()
        .with_timezone(&Utc);
    let mut store = ProductStore::open(std::path::Path::new(&db), config).unwrap();
    assert_eq!(
        store
            .begin_product_publish(&session, &receipt, now)
            .unwrap(),
        ProductPublishStart::Started
    );
    // Commit is durable; park forever so the parent can SIGKILL this
    // process between the committed start and any external call.
    println!("CHILD_STARTED");
    loop {
        std::thread::park();
    }
}

/// U2 crash cell (processes): a REAL child process commits the durable
/// publish start and is then SIGKILLed between commit and any external
/// call. The pending start must survive the kill; the retry composition
/// must converge with exactly one distinct PR.
#[test]
fn crash_between_start_and_external_call_survives_sigkill() {
    let mut approved = approved_fixture("u2-sigkill", true);
    let receipt_json = serde_json::to_string(&approved.receipt).unwrap();
    let db_path = approved.fixture.db_path.clone();
    let config = approved.fixture.config.clone();
    drop(approved.store);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "u2_crash_child",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env("B1_PUBLISH_CRASH_DB", &db_path)
        .env("B1_PUBLISH_CRASH_INSTALL", &config.installation_id)
        .env("B1_PUBLISH_CRASH_DATABASE", &config.database_id)
        .env("B1_PUBLISH_CRASH_RECEIPT", &receipt_json)
        .env(
            "B1_PUBLISH_CRASH_SESSION",
            &approved.fixture.human_session_digest,
        )
        .env("B1_PUBLISH_CRASH_AT", approved.fixture.now.to_rfc3339())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // Observe the committed pending start from THIS (separate) connection.
    let mut visible = false;
    for _ in 0..900 {
        if publish_counts(&db_path) == (1, 1, 0) {
            visible = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(visible, "child must commit the pending start first");
    // Forced interruption: SIGKILL between the committed start and the
    // external call — no destructor, no graceful close of any kind.
    child.kill().unwrap();
    let _ = child.wait();
    assert_eq!(publish_counts(&db_path), (1, 1, 0));
    // Retry after the crash: pending → reconcile NotFound → fence → exactly
    // one publisher call → confirmed, one distinct PR for the operation.
    let mut reopened = ProductStore::open(&db_path, config.clone()).unwrap();
    let publisher = LocalPublisher {
        db: db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/post-crash".into(),
        })),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::NotFound)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut reopened,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/post-crash".into()
        }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.distinct_prs(), 1);
    assert_eq!(publish_counts(&db_path), (1, 0, 1));
    drop(reopened);
    drop(approved.fixture.host.take());
    std::fs::remove_dir_all(approved.fixture.root).unwrap();
}

/// P2 confirmation race (a): while this caller's reconciler is in flight,
/// a concurrent caller genuinely settles the operation from a SECOND
/// connection. The retry fence must observe the confirmed row and return
/// the stored URL — the Publisher is never contacted for it.
#[test]
fn fence_observes_concurrent_settle_during_reconcile() {
    let mut approved = approved_fixture("p2-fence-race", true);
    assert_eq!(
        approved
            .store
            .begin_product_publish(
                &approved.fixture.human_session_digest,
                &approved.receipt,
                approved.fixture.now,
            )
            .unwrap(),
        ProductPublishStart::Started
    );
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 1, 0));
    let race_url = "https://example.invalid/pr/settled-by-peer".to_string();
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::NotFound)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(Some((
            approved.fixture.db_path.clone(),
            approved.fixture.config.installation_id.clone(),
            approved.fixture.config.database_id.clone(),
            race_url.clone(),
        )))),
    };
    let publisher = LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(None),
        outcome: Mutex::new(Some(PublishOutcome::Confirmed {
            pr_url: "https://example.invalid/pr/must-not-open".into(),
        })),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed { pr_url: race_url }
    );
    assert_eq!(reconciler.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        publisher.calls.load(Ordering::SeqCst),
        0,
        "fence must short-circuit"
    );
    assert_eq!(publisher.distinct_prs(), 0);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    close(approved);
}

/// P2 confirmation race (b): the concurrent settle lands DURING this
/// caller's external call (from a second connection at publish() entry).
/// The Publisher is operation-idempotent (same operation → same PR/URL),
/// and the caller's settle of the identical URL must succeed idempotently.
#[test]
fn concurrent_settle_during_publisher_call_is_idempotent() {
    let mut approved = approved_fixture("p2-during-call", true);
    let during_url = "https://example.invalid/pr/during-call".to_string();
    let publisher = LocalPublisher {
        db: approved.fixture.db_path.clone(),
        calls: AtomicUsize::new(0),
        opened_prs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        interleave_settle: Mutex::new(Some((
            approved.fixture.db_path.clone(),
            approved.fixture.config.installation_id.clone(),
            approved.fixture.config.database_id.clone(),
            during_url.clone(),
        ))),
        outcome: Mutex::new(None),
    };
    let reconciler = LocalReconciler {
        calls: Arc::new(AtomicUsize::new(0)),
        outcome: Arc::new(Mutex::new(ProductPublishReconcileOutcome::Unknown)),
        registry: None,
        interleave_settle: Arc::new(Mutex::new(None)),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert_eq!(
        runtime
            .block_on(ProductProjection::publish_product_approved(
                &mut approved.store,
                &approved.fixture.human_session_digest,
                &approved.receipt,
                &publisher,
                &reconciler,
                approved.fixture.now,
            ))
            .unwrap(),
        PublishOutcome::Confirmed { pr_url: during_url }
    );
    assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(publisher.distinct_prs(), 1);
    assert_eq!(publish_counts(&approved.fixture.db_path), (1, 0, 1));
    close(approved);
}
