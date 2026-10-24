use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use rusqlite::OptionalExtension;
use foundry_ledger::{
    ProductCapability, ProductConfig, ProductResultExpectation, ProductStore,
    RepositoryIdentity, ScopeGrantManifest, ScopeMutation, ScopeSourceManifest,
};
use foundry_serve::{auth_http::ProductHttpState, http::product_router};
use tower::ServiceExt;

#[cfg(feature = "p2-test-support")]
#[path = "../../foundry-line/tests/support/product_result_fixture.rs"]
mod product_result_fixture;

#[cfg(feature = "p2-test-support")]
struct AuditPublisher;

#[cfg(feature = "p2-test-support")]
impl foundry_domain::ports::Publisher for AuditPublisher {
    fn publish(
        &self,
        _order: &foundry_domain::Order,
        operation_id: &str,
        _expected_digest: &str,
    ) -> impl std::future::Future<
        Output = foundry_domain::ports::PortResult<foundry_domain::ports::PublishOutcome>,
    > + Send {
        let url = format!("https://example.invalid/audit/{operation_id}");
        async move { Ok(foundry_domain::ports::PublishOutcome::Confirmed { pr_url: url }) }
    }
}

#[cfg(feature = "p2-test-support")]
struct AuditNotFound;

#[cfg(feature = "p2-test-support")]
impl foundry_line::product::ProductPublishReconciler for AuditNotFound {
    fn reconcile(
        &self,
        _operation_id: &str,
    ) -> impl std::future::Future<
        Output = foundry_domain::ports::PortResult<
            foundry_line::product::ProductPublishReconcileOutcome,
        >,
    > + Send {
        async { Ok(foundry_line::product::ProductPublishReconcileOutcome::NotFound) }
    }
}

#[cfg(feature = "p2-test-support")]
#[tokio::test]
async fn w5_y5_production_http_hold_route_requires_auth_and_continues() {
    use foundry_ledger::ProductConfig;
    use product_result_fixture::{
        FixtureIdentity, ReviewScript, genuine_host_done, grant_review_hold_scope,
    };

    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "audit-y5-http-install".into(),
            database_id: "audit-y5-http-db".into(),
        },
        "audit-y5-http-source",
        "audit-y5-http-order",
    )
    .with_script(ReviewScript::ResumePass);
    let mut fixture = genuine_host_done(identity);
    grant_review_hold_scope(&fixture);
    let mut store = fixture.result_store.take().unwrap();
    let hold = store
        .list_product_review_holds(&fixture.human_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == fixture.identity.order_id)
        .expect("genuine ResumePass fixture reaches a review hold");

    // The explicit local test input adapter has no authority to call the
    // human hold route. The production router rejects a missing bearer.
    let state = ProductHttpState::with_review_host(store, fixture.host.take().unwrap());
    let review_router = product_router(state.clone());
    let request_body = serde_json::json!({
        "expected": hold.expectation.clone(),
        "decision": "resume",
        "target": "replan_full"
    })
    .to_string();
    let route = format!(
        "/api/product/orders/{}/review/holds/{}/accept",
        fixture.identity.order_id, hold.hold_instance_id
    );
    let unauthorized = review_router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&route)
                .header("idempotency-key", "w5-y5-unauthenticated-adapter")
                .header("content-type", "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let human_bearer = std::fs::read_to_string(&fixture.human_session_bearer_path).unwrap();
    let accepted_response = review_router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&route)
                .header("authorization", format!("Bearer {human_bearer}"))
                .header("idempotency-key", "audit-genuine-hold-acceptance")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(accepted_response.status(), StatusCode::OK);
    let acceptance: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(accepted_response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(acceptance["progress"]["phase"], "completed");
    assert!(fixture.calls.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the genuine host used its labeled test executor mock");
}

#[cfg(feature = "p2-test-support")]
#[tokio::test]
async fn scoped_audit_connects_product_sources_and_preserves_event_time_scope() {
    use foundry_ledger::{ProductCapability as Cap, ProductConfig};
    use product_result_fixture::{
        FixtureIdentity, ReviewScript, genuine_host_done, grant_review_hold_scope,
    };

    let identity = FixtureIdentity::new(
        ProductConfig {
            installation_id: "audit-gate1-install".into(),
            database_id: "audit-gate1-db".into(),
        },
        "audit-source-a",
        "audit-order-genuine",
    )
    .with_script(ReviewScript::ResumePass);
    let mut fixture = genuine_host_done(identity);
    grant_review_hold_scope(&fixture);
    let mut store = fixture.result_store.take().unwrap();

    // Preserve the original direct hold-acceptance/continuation baseline. The
    // separate W5 route cell below measures the production HTTP path.
    let hold = store
        .list_product_review_holds(&fixture.human_session_digest, fixture.now)
        .unwrap()
        .into_iter()
        .find(|hold| hold.order_id == fixture.identity.order_id)
        .expect("genuine ResumePass fixture reaches a review hold");
    let acceptance = store
        .accept_product_review_hold(
            &fixture.human_session_digest,
            &fixture.identity.order_id,
            &hold.hold_instance_id,
            &hold.expectation,
            "audit-genuine-hold-acceptance",
            foundry_ledger::ProductHoldDecision::Resume("replan_full".into()),
            fixture.now,
        )
        .unwrap();
    let mut host = fixture.host.take().unwrap();
    host.continue_accepted_product_hold(
        &fixture.human_session_digest,
        &acceptance.operation_id,
        fixture.now,
    )
    .unwrap();
    drop(host);
    let (final_run, result_json): (i64, String) = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT run_id,result_json FROM product_review_host_results_v1
             WHERE order_id=?1 AND attempt=1 ORDER BY CAST(run_id AS INTEGER) DESC LIMIT 1",
            [&fixture.identity.order_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let terminal: foundry_exec::HostResult = serde_json::from_str(&result_json).unwrap();
    fixture.expected = ProductResultExpectation {
        host_key: foundry_exec::HostKey {
            order: fixture.identity.order_id.clone(),
            attempt: 1,
            run: u64::try_from(final_run).unwrap(),
            activation: 3,
        },
        host_result_digest: foundry_ledger::product_host_result_digest(&terminal).unwrap(),
    };
    fixture.approval.run_id = final_run;
    fixture.terminal = Some(terminal);
    let finalized = store
        .finalize_product_review_result(&fixture.service, &fixture.expected)
        .unwrap();
    fixture.approval.grant_id = format!("{}-grant", fixture.identity.order_id);
    fixture.approval.token_digest = "a".repeat(64);
    fixture.approval.artifact_digest = fixture.terminal.as_ref().unwrap().result_commit.clone();
    fixture.approval.tuple_digest = finalized.tuple_digest;

    // The real host fixture supplies bootstrap, admitted order, accepted
    // hold, resumed run, stages, host result, promoted tuple, and service
    // history. Add current audit and publish capabilities through the
    // production scope-change path.
    let scope_revision: i64 = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1
             WHERE subject_id='reader' AND source_instance_id=?1",
            [&fixture.identity.source_instance_id],
            |row| row.get(0),
        )
        .unwrap();
    store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &fixture.identity.source_instance_id,
            Cap::ReadOperationalAudit,
            ScopeMutation::Grant,
            scope_revision,
            "audit-reader-source-a",
            fixture.now,
        )
        .unwrap();
    store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &fixture.identity.source_instance_id,
            Cap::PublishApproved,
            ScopeMutation::Grant,
            scope_revision + 1,
            "audit-reader-publish",
            fixture.now,
        )
        .unwrap();
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
    // AuditPublisher is a mock publisher (example.invalid URL); all review
    // execution and durable product consumers remain the genuine host/store.
    let published = foundry_line::product::ProductProjection::publish_product_approved(
        &mut store,
        &fixture.human_session_digest,
        &receipt,
        &AuditPublisher,
        &AuditNotFound,
        fixture.now,
    )
    .await
    .unwrap();
    assert!(matches!(
        published,
        foundry_line::product::ProductPublishResult::Published(
            foundry_domain::ports::PublishOutcome::Confirmed { .. }
        )
    ));

    // Keep a second genuinely admitted order behind the long-running host
    // order so the Y2 snapshot can exercise order-selection pagination while
    // still returning every S1 event for the selected order.
    store
        .admit_order(
            "zz-w5-y2-selection-tail",
            &fixture.identity.connector_id,
            &fixture.identity.repository_id,
            "issue:w5-y2-selection-tail",
            "dep-bump",
            "immutable supported-task snapshot",
            fixture.now,
        )
        .unwrap();

    // A real source rebind creates a new immutable generation. Access to the
    // retired and current source is explicitly granted for this audit cell.
    let expected_revision = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_sources_v1 WHERE connector_id=?1",
            [&fixture.identity.connector_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    let source_b = store
        .configure_scoped_source(
            &fixture.owner_session_digest,
            &fixture.identity.connector_id,
            RepositoryIdentity::canonicalized(
                &fixture.identity.repository_owner,
                "github",
                &fixture.identity.api_origin,
                "repo:2",
            )
            .unwrap(),
            &fixture.identity.repository_owner,
            "repo-renamed",
            "repo:2",
            "open+foundry:make",
            true,
            expected_revision,
            "audit-source-rebind-a-to-b",
            fixture.now,
        )
        .unwrap();
    store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &source_b.source_instance_id,
            Cap::ReadOperationalAudit,
            foundry_ledger::ScopeMutation::Grant,
            0,
            "audit-reader-source-b",
            fixture.now,
        )
        .unwrap();

    let snapshot = store
        .scoped_evidence_snapshot(
            &fixture.human_session_digest,
            Cap::ReadOperationalAudit,
            100,
            fixture.now,
        )
        .unwrap();
    let key = foundry_serve::privacy::ExportKey::generate().unwrap();
    let report = foundry_serve::audit_report::scoped_product_inventory_v3(
        &snapshot,
        fixture.now,
        "audit-gate1-context",
        &key,
    )
    .unwrap();
    assert!(!snapshot.casting_journal_present);
    let repeat_snapshot = store
        .scoped_evidence_snapshot(
            &fixture.human_session_digest,
            Cap::ReadOperationalAudit,
            100,
            fixture.now,
        )
        .unwrap();
    let repeat_report = foundry_serve::audit_report::scoped_product_inventory_v3(
        &repeat_snapshot,
        fixture.now,
        "audit-gate1-context",
        &key,
    )
    .unwrap();
    assert_eq!(report.connected_sources, repeat_report.connected_sources);
    assert_eq!(report.event_scope, repeat_report.event_scope);

    let source = |name: &str| {
        report
            .connected_sources
            .iter()
            .find(|entry| entry.source == name)
            .unwrap_or_else(|| panic!("missing connected source {name}"))
    };
    for name in [
        "S4.product_review_holds_v1",
        "S4.product_review_hold_acceptances_v1",
        "S4.product_review_hold_progress_v1",
        "S4.product_review_runs_v1",
        "S4.product_review_stages_v1",
        "S4.product_review_sessions_v1",
        "S4.product_review_host_results_v1",
        "S6.product_result_bindings_v1",
        "S6.product_approval_result_bindings_v1",
        "S6.artifact_tuples",
        "approval.product_outcomes_v1",
        "approval.product_grants_v1",
        "approval.product_token_consumptions_v1",
        "approval.product_service_operations_v1",
        "scope.product_scope_events_v1",
        "scope.product_scope_source_events_v1",
        "scope.product_service_events_v1",
        "scope.product_scope_migrations_v1",
        "scope.product_scope_quarantine_v1",
        "S5.local_product_publish_outcomes_v1",
    ] {
        let entry = source(name);
        assert_eq!(entry.availability, "available");
        assert!(entry.row_count >= 0);
        assert_eq!(entry.content_digest.len(), 64, "{name}");
        assert!(!entry.schema_description.is_empty(), "{name}");
    }
    assert_eq!(source("S4.product_review_runs_v1").row_count, 2);
    assert!(source("S4.product_review_stages_v1").row_count > 0);
    assert_eq!(source("S6.product_result_bindings_v1").row_count, 1);
    assert_eq!(source("approval.product_outcomes_v1").row_count, 1);
    assert_eq!(
        source("approval.product_token_consumptions_v1").row_count,
        1
    );
    assert_eq!(source("S5.local_product_publish_outcomes_v1").row_count, 1);
    assert!(source("S4.product_review_holds_v1").row_count > 0);
    assert!(source("S4.product_review_hold_acceptances_v1").row_count > 0);
    assert!(source("S4.product_review_hold_progress_v1").row_count > 0);

    let transition = report
        .event_scope
        .iter()
        .find(|event| {
            event.event_source == "product_scope_source_events_v1"
                && event.operation == "rebound"
        })
        .unwrap();
    let source_a_alias = transition.previous_source_alias.as_deref().unwrap();
    let source_b_alias = transition.source_alias.as_deref().unwrap();
    assert_ne!(source_a_alias, source_b_alias);
    assert!(transition.source_revision.is_some());

    let source_events: Vec<_> = report
        .event_scope
        .iter()
        .filter(|event| event.event_source == "product_scope_events_v1")
        .collect();
    assert!(source_events.len() >= 2, "scope grants for both sources are present");
    assert!(source_events
        .iter()
        .any(|event| event.source_alias.as_deref() == Some(source_a_alias)));
    assert!(source_events
        .iter()
        .any(|event| event.source_alias.as_deref() == Some(source_b_alias)));

    // Repeated references to a stored source use the transition's same
    // request-local alias, and no event attributed to the old source is
    // reclassified as the new source.
    let first_post_transition = source_events
        .iter()
        .position(|event| event.source_alias.as_deref() == Some(source_b_alias))
        .unwrap();
    let pre_transition_events = &source_events[..first_post_transition];
    assert!(!pre_transition_events.is_empty());
    assert!(pre_transition_events
        .iter()
        .all(|event| event.source_alias.as_deref() == Some(source_a_alias)));
    assert_eq!(
        transition.previous_source_alias.as_deref(),
        pre_transition_events[0].source_alias.as_deref(),
        "the same stored source has one alias throughout this response"
    );
    assert_eq!(
        transition.source_alias.as_deref(),
        source_events
            .iter()
            .find(|event| event.source_alias.as_deref() == Some(source_b_alias))
            .unwrap()
            .source_alias
            .as_deref(),
        "the rebound source alias matches its later scope event"
    );
    assert_eq!(
        report
            .event_scope
            .iter()
            .map(|event| event.event_ordinal)
            .collect::<Vec<_>>(),
        (1..=report.event_scope.len()).collect::<Vec<_>>()
    );
    let serialized_report = serde_json::to_string(&report).unwrap();
    assert!(!serialized_report.contains("audit-source-a"));
    assert!(!serialized_report.contains(&source_b.source_instance_id));
    let serialized_event_scope = serde_json::to_string(&report.event_scope).unwrap();
    assert!(!serialized_event_scope.contains("event_sequence"));
    assert!(!serialized_event_scope.contains("event_seq"));

    let local_publish = report
        .connected_sources
        .iter()
        .find(|entry| entry.source == "S5.local_product_publish_outcomes_v1")
        .unwrap();
    assert!(local_publish.source.contains("local"));
    let remote = report
        .unavailable_sources
        .iter()
        .find(|entry| entry.source == "S5.remote_verification")
        .unwrap();
    assert_eq!(remote.availability, "unavailable");
    assert!(remote.reason.contains("no remote verification adapter"));

    let unavailable = |name: &str| {
        report
            .unavailable_sources
            .iter()
            .find(|entry| entry.source == name)
            .unwrap_or_else(|| panic!("missing unavailable source {name}"))
    };
    assert_eq!(unavailable("S2").availability, "not_connected");
    assert_eq!(unavailable("S3").availability, "not_connected");
    assert_eq!(unavailable("M6").availability, "not_connected");
    assert_eq!(unavailable("governance").availability, "not_connected");
    assert_eq!(unavailable("casting").availability, "not_applicable");
    assert_ne!(unavailable("M6").reason, unavailable("governance").reason);
    assert_ne!(unavailable("S2").reason, unavailable("casting").reason);
    assert!(report.event_scope_notes.iter().any(|note| {
        note.contains("product_service_events_v1") && note.contains("no source_instance_id")
    }));

    // Y2: this order has passed through admission, a genuine multi-run host
    // execution, an accepted human hold, finalization, approval, and publish.
    // Its own history must stay complete even though the authorized order
    // selection has another order beyond this one-row page.
    let export_scope_revision: Option<i64> = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1
             WHERE subject_id=?1 AND source_instance_id=?2",
            rusqlite::params!["reader", fixture.identity.source_instance_id],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    store
        .change_product_scope(
            &fixture.owner_session_digest,
            "reader",
            &fixture.identity.source_instance_id,
            Cap::ExportEvidence,
            ScopeMutation::Grant,
            export_scope_revision.unwrap_or(0),
            "audit-y2-export-long-order",
            fixture.now,
        )
        .unwrap();
    let long_history = store
        .scoped_evidence_snapshot(
            &fixture.human_session_digest,
            Cap::ExportEvidence,
            1,
            fixture.now,
        )
        .unwrap();
    assert!(long_history.has_more, "another authorized order follows the selected order");
    assert!(long_history.coverage_complete, "selection paging does not truncate selected-order history");
    let long_history_rows = long_history
        .rows
        .iter()
        .filter(|row| row.order_id == fixture.identity.order_id)
        .count();
    let total_order_history: i64 = rusqlite::Connection::open(&fixture.db_path)
        .unwrap()
        .query_row(
            "SELECT count(*) FROM ledger WHERE order_id=?1",
            [&fixture.identity.order_id],
            |row| row.get(0),
        )
        .unwrap();
    // Ordinary export fixtures have one admission row. This fixture has five
    // genuine lifecycle events (five times the ordinary history) and retains
    // every one of them on the selected order's paginated export.
    assert!(
        long_history_rows >= 5,
        "the genuine multi-run review and approval path should exceed the one-row ordinary fixture; got {long_history_rows} rows"
    );
    assert_eq!(long_history_rows as i64, total_order_history);
    eprintln!(
        "W5 Y2 selected_order_history_rows={long_history_rows} total_order_rows={total_order_history} selection_has_more={}",
        long_history.has_more
    );
    assert!(long_history.rows.iter().all(|row| row.order_id == fixture.identity.order_id));
    let long_export = foundry_serve::privacy::scoped_product_export(
        &long_history,
        "audit-y2-long-history",
        &key,
    )
    .unwrap();
    assert!(long_export.has_more, "selection has more authorized orders");
    assert!(long_export.selection_truncated);
    assert!(long_export
        .selection_truncation_label
        .as_deref()
        .is_some_and(|label| label.contains("not the full set")));
    assert!(long_export.selected_order_history_complete);
    assert_eq!(long_export.selected_order_history_byte_limit, None);
    assert_eq!(long_export.rows.len() as i64, total_order_history);

    drop(store);
    drop(fixture.host.take());
}

fn request_method(
    app: &axum::Router,
    method: &str,
    path: &str,
    bearer: &str,
    body: &str,
) -> (StatusCode, Vec<u8>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("authorization", format!("Bearer {bearer}"));
        if !body.is_empty() {
            builder = builder.header("content-type", "application/json");
        }
        let response = app
            .clone()
            .oneshot(builder.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec();
        (status, bytes)
    })
}

fn request(app: &axum::Router, path: &str, bearer: &str) -> (StatusCode, Vec<u8>) {
    request_method(app, "GET", path, bearer, "")
}

#[tokio::test]
async fn registered_product_export_requires_current_approver_and_redacts_identifiers() {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.db");
    let mut store = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "install-canary".into(),
            database_id: "export-db".into(),
        },
    )
    .unwrap();
    let password = "synthetic-export-password";
    let phc = foundry_ledger::product_identity::ProductPasswordKdf::hash(password).unwrap();
    let source = ScopeSourceManifest {
        connector_id: "gh".into(),
        source_instance_id: "export-source".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "repo:42",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo".into()),
        legacy_repository_locator: "repo:42".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    store
        .bootstrap_owner_scoped(
            "canary-owner",
            &phc,
            std::slice::from_ref(&source),
            &[ScopeGrantManifest {
                actor_id: "canary-owner".into(),
                source_instance_id: source.source_instance_id.clone(),
                capabilities: vec![
                    ProductCapability::ReadOrder,
                    ProductCapability::ExportEvidence,
                    ProductCapability::ReadOperationalAudit,
                ],
            }],
        )
        .unwrap();
    store
        .admit_order(
            "ORDER-PII-CANARY-73",
            "gh",
            "repo:42",
            "issue:7",
            "dep-bump",
            "immutable task snapshot",
            chrono::Utc::now(),
        )
        .unwrap();
    let session = store
        .login("canary-owner", password, chrono::Utc::now())
        .unwrap();
    let bearer = session.bearer().to_owned();
    drop(session);

    let app = product_router(ProductHttpState::new(store));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/product/export")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/product/export")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "explicit ExportEvidence grant returns an actual scoped projection"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    for canary in [
        "ORDER-PII-CANARY-73",
        "canary-owner",
        "install-canary",
        password,
        &bearer,
    ] {
        assert!(!text.contains(canary));
    }
    let export: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    eprintln!("W5 Y3 export verification_duration_micros={}", export["verification_duration_micros"]);
    assert_eq!(export["schema"], "foundry-scoped-evidence-projection-v1");
    assert_eq!(export["coverage"], "complete");
    assert_eq!(export["selection_truncated"], false);
    assert!(export["selection_truncation_label"].is_null());
    assert_eq!(export["selected_order_history_complete"], true);
    assert!(export["selected_order_history_byte_limit"].is_null());
    assert!(export["verification_duration_micros"].as_u64().is_some());
    assert!(export["verification_boundary"].as_str().unwrap().contains("does not substitute"));
    assert!(
        export["projected_digest"]
            .as_str()
            .is_some_and(|v| v.len() == 64)
    );
    assert_eq!(export["rows"][0]["ordinal"], 1);
    for forbidden in [
        "seq",
        "sequence",
        "prev_hash",
        "hash",
        "head",
        "prefix_sequence",
        "raw_prefix_head",
    ] {
        assert!(
            export.get(forbidden).is_none(),
            "global coordinate escaped as {forbidden}"
        );
        assert!(
            export["rows"][0].get(forbidden).is_none(),
            "global coordinate escaped as {forbidden}"
        );
    }

    let audit_response = app
        .oneshot(
            Request::builder()
                .uri("/api/product/audit")
                .header("authorization", format!("Bearer {bearer}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        audit_response.status(),
        StatusCode::OK,
        "explicit ReadOperationalAudit grant returns an actual scoped inventory"
    );
    let audit_bytes = axum::body::to_bytes(audit_response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let audit_text = std::str::from_utf8(&audit_bytes).unwrap();
    for canary in [
        "ORDER-PII-CANARY-73",
        "canary-owner",
        "install-canary",
        password,
        &bearer,
    ] {
        assert!(!audit_text.contains(canary));
    }
    let audit: serde_json::Value = serde_json::from_slice(&audit_bytes).unwrap();
    eprintln!("W5 Y3 audit verification_duration_micros={}", audit["projection"]["verification_duration_micros"]);
    assert!(audit["projection"]["verification_duration_micros"].as_u64().is_some());
    assert!(audit["projection"]["verification_boundary"].as_str().unwrap().contains("does not substitute"));
    assert_eq!(audit["schema_version"], 3);
    assert_eq!(audit["sources"][0]["source"], "S1");
    assert_eq!(audit["sources"][0]["availability"], "available");
    assert_eq!(audit["sources"][1]["availability"], "unavailable");
    assert!(audit["sources"][1]["missing_reason"].as_str().is_some());
    for forbidden in [
        "prefix_sequence",
        "head_hash",
        "snapshot_digest",
        "identity",
        "raw_prefix_head",
    ] {
        assert!(audit.get(forbidden).is_none());
        assert!(audit["projection"].get(forbidden).is_none());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn product_export_aliases_short_unicode_and_path_like_identifiers() {
    let key = foundry_serve::privacy::ExportKey::generate().unwrap();
    let order_id = "u/é/7";
    let actor = "s/会/7";
    let path_marker = "/tmp/π/session/7";
    let row = foundry_ledger::LedgerRow {
        seq: 1,
        order_id: order_id.into(),
        from_state: "received".into(),
        to_state: "queued".into(),
        event: "Queued".into(),
        actor: actor.into(),
        intent: path_marker.into(),
        at: "2026-10-16T00:00:00Z".into(),
        prev_hash: "GENESIS".into(),
        hash: "synthetic-head".into(),
        event_json: None,
    };
    let document = foundry_serve::privacy::product_export("i/ø/1", &[row], "ctx", &key);
    let encoded = serde_json::to_string(&document).unwrap();
    for raw in ["i/ø/1", order_id, actor, path_marker] {
        assert!(
            !encoded.contains(raw),
            "raw identifier escaped export projection"
        );
    }
    assert!(document.rows[0].order.starts_with("anon-"));
    assert!(document.rows[0].actor.starts_with("anon-"));
}

#[test]
fn scoped_export_and_audit_require_their_own_capability_and_hide_foreign_rows() {
    let root = std::env::temp_dir().join(format!(
        "foundry-b1-scoped-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let db = root.join("product.sqlite");
    let now = chrono::Utc::now();
    let source_a = ScopeSourceManifest {
        connector_id: "gh-a".into(),
        source_instance_id: "source-a".into(),
        generation: 1,
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://api.github.com",
            "same-repo-id",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-a".into()),
        legacy_repository_locator: "same-repo-id".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let source_b = ScopeSourceManifest {
        connector_id: "gh-b".into(),
        source_instance_id: "source-b".into(),
        generation: 1,
        // Same stable numeric repository ID at a different origin is a
        // different source and cannot inherit source-a grants.
        identity: RepositoryIdentity::canonicalized(
            "acme",
            "github",
            "https://enterprise.example",
            "same-repo-id",
        )
        .unwrap(),
        api_owner: Some("acme".into()),
        api_repo: Some("repo-b".into()),
        legacy_repository_locator: "same-repo-id".into(),
        scope_query: "open+foundry:make".into(),
        enabled: true,
    };
    let config = ProductConfig {
        installation_id: "private-install-canary".into(),
        database_id: "private-db-canary".into(),
    };
    let owner_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("owner-export-password")
            .unwrap();
    let mut store = ProductStore::open(&db, config).unwrap();
    store
        .bootstrap_owner_scoped(
            "owner",
            &owner_phc,
            &[source_a.clone(), source_b.clone()],
            &[ScopeGrantManifest {
                actor_id: "owner".into(),
                source_instance_id: "source-a".into(),
                capabilities: vec![
                    ProductCapability::ReadOrder,
                    ProductCapability::ExportEvidence,
                    ProductCapability::ReadOperationalAudit,
                ],
            }],
        )
        .unwrap();
    store
        .admit_order(
            "own-order-canary",
            "gh-a",
            "same-repo-id",
            "issue:1",
            "dep-bump",
            "own payload",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "own-order-canary-2",
            "gh-a",
            "same-repo-id",
            "issue:2",
            "dep-bump",
            "own payload 2",
            now,
        )
        .unwrap();
    store
        .admit_order(
            "foreign-order-canary",
            "gh-b",
            "same-repo-id",
            "issue:1",
            "dep-bump",
            "foreign payload",
            now,
        )
        .unwrap();
    for index in 0..120 {
        store
            .admit_order(
                &format!("foreign-order-{index:03}"),
                "gh-b",
                "same-repo-id",
                &format!("issue:{}", index + 3),
                "dep-bump",
                "foreign payload",
                now,
            )
            .unwrap();
    }
    let owner = store.login("owner", "owner-export-password", now).unwrap();
    let owner_bearer = owner.bearer().to_owned();
    let owner_digest = owner.digest().to_owned();
    let viewer_phc =
        foundry_ledger::product_identity::ProductPasswordKdf::hash("viewer-export-password")
            .unwrap();
    store
        .create_account(&owner_digest, "reader", "approver", &viewer_phc, now)
        .unwrap();
    store
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOrder,
            foundry_ledger::product_scope::ScopeMutation::Grant,
            0,
            "reader-order",
            now,
        )
        .unwrap();
    let reader = store
        .login("reader", "viewer-export-password", now)
        .unwrap();
    let reader_bearer = reader.bearer().to_owned();

    let first = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
        .unwrap();
    let key = foundry_serve::privacy::ExportKey::generate().unwrap();
    let before =
        foundry_serve::privacy::scoped_product_export(&first, "fixed-test-context", &key).unwrap();
    // Add another foreign repository row. Since the projection filters and
    // hashes only authorized rows, it must not disclose chain gaps/head changes.
    store
        .admit_order(
            "foreign-order-2",
            "gh-b",
            "same-repo-id",
            "issue:2",
            "dep-bump",
            "another foreign payload",
            now,
        )
        .unwrap();
    let second = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
        .unwrap();
    let after =
        foundry_serve::privacy::scoped_product_export(&second, "fixed-test-context", &key).unwrap();
    assert_eq!(
        before, after,
        "foreign rows and global chain changes do not alter visible projection"
    );
    assert_eq!(
        before
            .rows
            .iter()
            .map(|row| row.ordinal)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(before.selected_order_history_complete);
    assert_eq!(before.selected_order_history_byte_limit, None);
    assert!(!before.selection_truncated);
    let first_page = store
        .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 1, now)
        .unwrap();
    assert!(
        first_page.has_more,
        "has_more reflects only authorized orders"
    );
    assert!(
        first_page
            .rows
            .iter()
            .all(|row| row.order_id != "foreign-order-canary")
    );
    // Retire/rebind the connector. Existing explicit grants can still read the
    // precisely bound historical source, but cannot transfer to the new source.
    store
        .configure_scoped_source(
            &owner_digest,
            "gh-a",
            source_a.identity.clone(),
            "acme",
            "repo-a",
            "same-repo-id",
            "open+foundry:make",
            true,
            1,
            "rebind-gh-a",
            now,
        )
        .unwrap();
    assert_eq!(
        store
            .scoped_evidence_snapshot(&owner_digest, ProductCapability::ExportEvidence, 100, now)
            .unwrap()
            .rows
            .len(),
        2,
        "historical evidence remains tied to its old granted source instance"
    );
    assert!(
        store
            .read_scoped_order(&owner_digest, "foreign-order-canary", now)
            .is_err(),
        "Owner role alone does not grant repository data access"
    );

    let app = product_router(ProductHttpState::new(store));
    for path in ["/api/product/export", "/api/product/audit"] {
        let (status, body) = request(&app, path, &owner_bearer);
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8(body).unwrap();
        assert!(body.contains("anon-"));
        for canary in [
            "own-order-canary",
            "foreign-order-canary",
            "private-install-canary",
            "private-db-canary",
        ] {
            assert!(!body.contains(canary));
        }
        for global in [
            "prev_hash",
            "raw_prefix_head",
            "prefix_sequence",
            "head_hash",
        ] {
            assert!(!body.contains(global));
        }
    }
    for path in ["/api/product/export?limit=1", "/api/product/audit?limit=1"] {
        let (status, bytes) = request(&app, path, &owner_bearer);
        assert_eq!(status, StatusCode::OK);
        let projection: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let projection = if path.contains("audit") {
            &projection["projection"]
        } else {
            &projection
        };
        assert_eq!(projection["coverage"], "page");
        assert!(projection["has_more"].as_bool().unwrap());
        assert_eq!(projection["selection_truncated"], true);
        assert!(projection["selection_truncation_label"].as_str().unwrap().contains("not the full set"));
        assert_eq!(projection["selected_order_history_complete"], true);
        assert!(projection["selected_order_history_byte_limit"].is_null());
        assert!(projection["verification_duration_micros"].as_u64().is_some());
        assert!(projection["verification_boundary"].as_str().unwrap().contains("other users' response failures"));
        assert_eq!(projection["rows"].as_array().unwrap().len(), 1);
        assert_eq!(projection["rows"][0]["ordinal"], 1);
        assert!(!String::from_utf8_lossy(&bytes).contains("foreign-order-canary"));
    }
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "ReadOrder is not ExportEvidence"
    );
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "ReadOrder is not ReadOperationalAudit"
    );

    let export_grant = {
        let mut store = ProductStore::open(
            &db,
            ProductConfig {
                installation_id: "private-install-canary".into(),
                database_id: "private-db-canary".into(),
            },
        )
        .unwrap();
        store
            .change_product_scope(
                &owner_digest,
                "reader",
                "source-a",
                ProductCapability::ExportEvidence,
                foundry_ledger::product_scope::ScopeMutation::Grant,
                1,
                "reader-export",
                now,
            )
            .unwrap()
    };
    assert_eq!(export_grant.scope_revision, 2);
    // Router holds its own SQLite handle and sees the committed grant.
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::OK
    );
    let mut admin = ProductStore::open(
        &db,
        ProductConfig {
            installation_id: "private-install-canary".into(),
            database_id: "private-db-canary".into(),
        },
    )
    .unwrap();
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ExportEvidence,
            foundry_ledger::product_scope::ScopeMutation::Revoke,
            2,
            "reader-export-revoke",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/export", &reader_bearer).0,
        StatusCode::FORBIDDEN,
        "same session is denied after a committed capability revoke"
    );
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOperationalAudit,
            foundry_ledger::product_scope::ScopeMutation::Grant,
            3,
            "reader-audit",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::OK
    );
    admin
        .change_product_scope(
            &owner_digest,
            "reader",
            "source-a",
            ProductCapability::ReadOperationalAudit,
            foundry_ledger::product_scope::ScopeMutation::Revoke,
            4,
            "reader-audit-revoke",
            now,
        )
        .unwrap();
    assert_eq!(
        request(&app, "/api/product/audit", &reader_bearer).0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = request_method(
        &app,
        "POST",
        "/api/product/accounts/reader/authority",
        &owner_bearer,
        r#"{"role":"approver","enabled":false}"#,
    );
    assert_eq!(status, StatusCode::NO_CONTENT);
    for path in ["/api/product/export", "/api/product/audit"] {
        assert_eq!(
            request(&app, path, &reader_bearer).0,
            StatusCode::FORBIDDEN,
            "disabled account cannot retain export/audit access"
        );
    }
    assert_eq!(
        request_method(&app, "POST", "/api/product/logout", &owner_bearer, "").0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        request(&app, "/api/product/export", &owner_bearer).0,
        StatusCode::FORBIDDEN,
        "logout revokes export access for the bearer"
    );
    drop(admin);
    drop(owner);
    drop(reader);
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let _ = std::fs::remove_dir_all(root);
}
