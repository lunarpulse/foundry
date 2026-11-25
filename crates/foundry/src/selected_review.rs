//! Normal, offline review of an existing commit. Runtime configuration is
//! operator-owned. Request arguments contain identities, never pack paths.
use super::{
    build_product_review_host, product_config, read_private, read_product_json, sha256_hex,
};
use chrono::Utc;
use clap::Args;
use foundry_ledger::product::{COMMIT_REVIEW_KIND, CommitReviewRequest, CommitReviewWork};
use foundry_ledger::{PackRegistryConfig, ProductStore};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Args)]
struct Database {
    #[arg(long)]
    db: PathBuf,
    #[arg(long)]
    installation_id: String,
    #[arg(long)]
    database_id: String,
}

impl Database {
    fn open(&self) -> Result<ProductStore, ()> {
        if !self.db.is_file() {
            return Err(());
        }
        ProductStore::create_product_pin_store(
            &self.db,
            &product_config(&self.installation_id, &self.database_id),
        )
        .map_err(|_| ())
    }
}

#[derive(Args)]
struct Source {
    #[arg(long)]
    service_file: String,
    #[arg(long)]
    connector: String,
    #[arg(long, default_value = "https://api.github.com")]
    api_origin: String,
    #[arg(long)]
    repository_id: String,
}

impl Source {
    fn authenticate(
        &self,
        store: &mut ProductStore,
        action: foundry_ledger::ProductServiceAction,
    ) -> Result<foundry_ledger::ProductServiceContext, ()> {
        let token = read_private(&self.service_file).map_err(|_| ())?;
        store
            .authenticate_product_service_for_action(
                &token,
                &self.connector,
                &self.api_origin,
                &self.repository_id,
                action,
                Utc::now(),
            )
            .map_err(|_| ())
    }
}

#[derive(Args)]
pub(super) struct IntakeArgs {
    #[command(flatten)]
    database: Database,
    #[command(flatten)]
    source: Source,
    #[arg(long)]
    issue_id: String,
    #[arg(long)]
    order: String,
    #[arg(long)]
    #[arg(
        required_unless_present = "issue_snapshot",
        conflicts_with = "issue_snapshot"
    )]
    commit: Option<String>,
    #[arg(long)]
    issue_snapshot: Option<PathBuf>,
    #[arg(long)]
    observed_updated_at: chrono::DateTime<Utc>,
}

#[derive(Args)]
pub(super) struct PackArgs {
    #[command(flatten)]
    database: Database,
    #[arg(long, value_parser=["approve","revoke","select"])]
    action: String,
    #[arg(long)]
    session_file: String,
    #[arg(long)]
    source_instance: String,
    #[arg(long)]
    digest: String,
    #[arg(long)]
    expected_revision: i64,
    #[arg(long)]
    request_key: String,
    #[arg(long)]
    order: Option<String>,
    #[arg(long)]
    lifecycle: Option<i64>,
}

#[derive(Args)]
pub(super) struct DispatchArgs {
    #[command(flatten)]
    database: Database,
    #[command(flatten)]
    source: Source,
    #[arg(long)]
    session_file: String,
    #[arg(long)]
    order: String,
    #[arg(long)]
    lifecycle: i64,
    #[arg(long)]
    selection_revision: i64,
    #[arg(long)]
    admission_hash: String,
    #[arg(long)]
    commit: String,
    #[arg(long)]
    request_key: String,
}

fn report(value: Result<Value, ()>) -> i32 {
    match value {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(()) => {
            eprintln!("product review request rejected");
            1
        }
    }
}

fn registry() -> Result<PackRegistryConfig, ()> {
    let paths: Vec<PathBuf> = match (
        std::env::var_os("FOUNDRY_REVIEW_PACK_REGISTRY"),
        std::env::var_os("FOUNDRY_REVIEW_PACK"),
    ) {
        (Some(manifest), None) => read_product_json(manifest.to_str().ok_or(())?)?,
        (None, Some(path)) => vec![path.into()],
        _ => return Err(()),
    };
    if paths.is_empty() {
        return Err(());
    }
    let mut registry = PackRegistryConfig::default();
    for path in paths {
        if !path.is_absolute() {
            return Err(());
        }
        let snapshot = foundry_pack::load_pack(&path).map_err(|_| ())?;
        if registry
            .snapshots
            .insert(snapshot.digest().to_owned(), path)
            .is_some()
        {
            return Err(());
        }
    }
    Ok(registry)
}

pub(super) fn intake(args: IntakeArgs) -> i32 {
    report((|| {
        if args.order.is_empty()
            || args.order.len() > 128
            || !args
                .order
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(());
        }
        let (kind, payload) = match (args.commit, args.issue_snapshot) {
            (Some(commit), None)
                if commit.len() == 40
                    && commit
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                (COMMIT_REVIEW_KIND, commit)
            }
            (None, Some(path)) => {
                use std::io::Read;
                let mut bytes = Vec::new();
                std::fs::File::open(path)
                    .map_err(|_| ())?
                    .take(1_000_001)
                    .read_to_end(&mut bytes)
                    .map_err(|_| ())?;
                if bytes.len() > 1_000_000 {
                    return Err(());
                }
                ("github-issue", String::from_utf8(bytes).map_err(|_| ())?)
            }
            _ => return Err(()),
        };
        let mut store = args.database.open()?;
        let service = args
            .source
            .authenticate(&mut store, foundry_ledger::ProductServiceAction::IntakeRead)?;
        let now = Utc::now();
        for action in [
            foundry_ledger::ProductServiceAction::AdmitDelivery,
            foundry_ledger::ProductServiceAction::AckDelivery,
        ] {
            store
                .require_product_service(&service, action, now)
                .map_err(|_| ())?;
        }
        let updated = args.observed_updated_at.to_rfc3339();
        let version = foundry_ledger::product::delivery_version_digest(
            &args.issue_id,
            &updated,
            kind,
            &payload,
        );
        let delivery_id = foundry_ledger::product::github_delivery_id(
            store.installation_id(),
            &args.source.connector,
            &args.source.api_origin,
            &args.source.repository_id,
            &args.issue_id,
            &version,
        );
        let delivery = foundry_ledger::ProductDelivery {
            delivery_id: delivery_id.clone(),
            connector_id: args.source.connector.clone(),
            repository_id: args.source.repository_id,
            issue_id: args.issue_id,
            observed_updated_at: updated.clone(),
            version_digest: version,
            task_kind: kind.into(),
            payload,
        };
        let cursor = foundry_ledger::ProductPollCursor {
            connector_id: args.source.connector,
            query_fingerprint: "local-commit-review".into(),
            sweep_id: "local-commit-review".into(),
            next_page: 1,
            page_ceiling: 1,
            updated_at: updated,
        };
        store
            .persist_intake_page_for_service(&service, &cursor, &[delivery], &[], now)
            .map_err(|_| ())?;
        let receipt = store
            .admit_intake_delivery_for_service(&service, &delivery_id, &args.order, now)
            .map_err(|_| ())?;
        store
            .mark_intake_acked_for_service(&service, &delivery_id, &receipt, now)
            .map_err(|_| ())?;
        Ok(
            json!({"status":"acknowledged","order_id":receipt.order_id,"lifecycle":receipt.lifecycle,"admission_hash":receipt.event_hash,"admission_sequence":receipt.sequence,"delivery_id":delivery_id}),
        )
    })())
}

pub(super) fn pack(args: PackArgs) -> i32 {
    report((|| {
        let session = sha256_hex(read_private(&args.session_file).map_err(|_| ())?.as_bytes());
        let mut store = args.database.open()?;
        store
            .authorize_pack_registry(
                &session,
                &args.source_instance,
                args.order.as_deref().zip(args.lifecycle),
            )
            .map_err(|_| ())?;
        if args.action != "revoke" {
            let configured = registry()?;
            if !configured.snapshots.contains_key(&args.digest) {
                return Err(());
            }
            store = store.with_product_registry(configured);
        }
        let now = Utc::now();
        let receipt = match args.action.as_str() {
            "select" => store.select_order_pack(
                &session,
                args.order.as_deref().ok_or(())?,
                args.lifecycle.ok_or(())?,
                &args.source_instance,
                &args.digest,
                args.expected_revision,
                &args.request_key,
                now,
            ),
            action if args.order.is_none() && args.lifecycle.is_none() => store
                .change_pack_approval(
                    &session,
                    &args.source_instance,
                    &args.digest,
                    args.expected_revision,
                    action == "approve",
                    &args.request_key,
                    now,
                ),
            _ => return Err(()),
        }
        .map_err(|_| ())?;
        Ok(
            json!({"action":args.action,"canonical_hex":hex::encode(receipt.canonical),"order_id":receipt.order_id,"lifecycle":receipt.lifecycle,"digest":receipt.digest,"revision":receipt.result_revision,"approval_revision":receipt.approval_revision,"source_instance_id":receipt.source_instance_id,"actor_id":receipt.actor_id}),
        )
    })())
}

/// Require a locally available commit before exposing the order to a worker.
fn require_local_commit(commit: &str) -> Result<(), ()> {
    let repository = std::env::var_os("FOUNDRY_REPO_PATH").ok_or(())?;
    let object = std::process::Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(["cat-file", "-t", commit])
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .output()
        .map_err(|_| ())?;
    if object.status.success() && object.stdout == b"commit\n" {
        Ok(())
    } else {
        Err(())
    }
}

pub(super) fn dispatch(args: DispatchArgs) -> i32 {
    report((|| {
        let session = sha256_hex(read_private(&args.session_file).map_err(|_| ())?.as_bytes());
        let mut store = args.database.open()?;
        let service = args.source.authenticate(
            &mut store,
            foundry_ledger::ProductServiceAction::DriveReview,
        )?;
        if std::env::var("FOUNDRY_PRODUCT_REVIEW_SOURCE")
            .ok()
            .as_deref()
            != Some(service.source_instance_id())
        {
            return Err(());
        }
        let request = CommitReviewRequest {
            order: &args.order,
            lifecycle: args.lifecycle,
            admission_hash: &args.admission_hash,
            commit: &args.commit,
            selection_revision: args.selection_revision,
            request_key: &args.request_key,
        };
        let (binding, work) = store
            .prepare_commit_review(&session, &service, &request, false)
            .map_err(|_| ())?;
        let digest = binding.pack_digest.clone();
        let registry = registry()?;
        let path = registry.snapshots.get(&digest).ok_or(())?.clone();
        store = store.with_product_registry(registry.clone());
        let result = match work {
            CommitReviewWork::Recorded(result) => result,
            CommitReviewWork::Dispatch(_) => return Err(()),
            CommitReviewWork::Pending => {
                require_local_commit(&args.commit)?;
                let mut host = build_product_review_host(
                    &path,
                    args.database.open()?.with_product_registry(registry),
                    true,
                )
                .map_err(|_| ())?;
                host.ensure_commit_review_available().map_err(|_| ())?;
                let (current, work) = store
                    .prepare_commit_review(&session, &service, &request, true)
                    .map_err(|_| ())?;
                if binding != current {
                    return Err(());
                }
                match work {
                    CommitReviewWork::Recorded(result) => result,
                    CommitReviewWork::Pending => return Err(()),
                    CommitReviewWork::Dispatch(claim) => {
                        let intent = foundry_exec::ExecutionIntent {
                            order: foundry_exec::OrderId(args.order.clone()),
                            attempt: foundry_exec::Attempt(1),
                            run: foundry_exec::RunId(1),
                            activation: 1,
                            generation: foundry_exec::Generation(0),
                            epoch: foundry_exec::Epoch(claim.review_binding().fence as u64),
                            recorded_at: claim.lease_until() - chrono::Duration::minutes(30),
                            external_ref: format!(
                                "review:{}:1:{}..{}",
                                args.order, args.commit, args.commit
                            ),
                        };
                        host.try_dispatch_selected_product_work(
                            &intent,
                            &claim,
                            binding,
                            &args.request_key,
                        )
                        .map_err(|_| ())?
                    }
                }
            }
        };
        Ok(
            json!({"order_id":args.order,"lifecycle":args.lifecycle,"selection_revision":args.selection_revision,"pack_digest":digest,"commit":result.result_commit,"exit_code":result.exit_code,"hold":result.held}),
        )
    })())
}

fn hold_host(
    store: &ProductStore,
    held: &foundry_ledger::product::SelectedReviewHold,
) -> Result<super::PackReviewHost, ()> {
    if std::env::var("FOUNDRY_PRODUCT_REVIEW_SOURCE").ok().as_ref()
        != Some(&held.source_instance_id)
    {
        return Err(());
    }
    let configured = registry()?;
    let path = configured
        .snapshots
        .get(&held.pack_digest)
        .ok_or(())?
        .clone();
    let backend = ProductStore::open(
        store.path(),
        product_config(store.installation_id(), store.database_id()),
    )
    .map_err(|_| ())?
    .with_product_registry(configured);
    build_product_review_host(&path, backend, held.commit_review).map_err(|_| ())
}

pub(super) fn decide_hold(
    store: &mut ProductStore,
    session: &str,
    order: &str,
    hold: &str,
    key: &str,
    decision: foundry_ledger::ProductHoldDecision,
    held: foundry_ledger::product::SelectedReviewHold,
    accept_only: bool,
) -> i32 {
    let mut runtime = None;
    if held.active {
        let Ok(host) = hold_host(store, &held) else {
            return report(Err(()));
        };
        if matches!(&decision, foundry_ledger::ProductHoldDecision::Resume(_))
            && host.ensure_product_executor_available().is_err()
        {
            return report(Err(()));
        }
        runtime = Some(host);
    }
    let Ok(receipt) = store.accept_product_review_hold(
        session,
        order,
        hold,
        &held.expectation,
        key,
        decision,
        Utc::now(),
    ) else {
        return report(Err(()));
    };
    let Ok(mut current) = store.product_review_progress(session, &receipt.operation_id, Utc::now())
    else {
        println!("{}", json!({"receipt":receipt,"phase":"unknown"}));
        return 1;
    };
    if !accept_only && !matches!(current.phase.as_str(), "completed" | "recovery_required") {
        if let Some(mut host) = runtime.or_else(|| hold_host(store, &held).ok()) {
            let _ = host.continue_accepted_product_hold(session, &receipt.operation_id, Utc::now());
            if let Ok(progress) =
                store.product_review_progress(session, &receipt.operation_id, Utc::now())
            {
                current = progress;
            }
        }
    }
    println!(
        "{}",
        json!({"receipt":receipt,"phase":current.phase,"progress_revision":current.progress_revision})
    );
    if current.phase == "completed" || accept_only {
        0
    } else {
        1
    }
}

#[derive(Args)]
pub(super) struct TaskArgs {
    #[command(flatten)]
    database: Database,
    #[arg(long)]
    session_file: String,
    #[arg(long)]
    order: String,
    #[arg(long)]
    lifecycle: i64,
    #[arg(long)]
    admission_hash: String,
    #[arg(long)]
    task_sha256: String,
    #[arg(long)]
    request_key: String,
}

pub(super) fn task(args: TaskArgs) -> i32 {
    report((|| {
        let session = sha256_hex(read_private(&args.session_file).map_err(|_| ())?.as_bytes());
        let receipt = args
            .database
            .open()?
            .authorize_code_change(
                &session,
                &args.order,
                args.lifecycle,
                &args.admission_hash,
                &args.task_sha256,
                &args.request_key,
            )
            .map_err(|_| ())?;
        serde_json::to_value(receipt).map_err(|_| ())
    })())
}

#[derive(Args)]
pub(super) struct ProducerArgs {
    #[command(flatten)]
    database: Database,
    #[command(flatten)]
    source: Source,
    #[arg(long,value_parser=["reserve","seal","collect","status","recover"])]
    action: String,
    #[arg(long)]
    order: String,
    #[arg(long)]
    request_key: String,
    #[arg(long, required_if_eq("action", "seal"))]
    commit: Option<String>,
}

/// An operator can inspect and reconcile a reservation after restart. The
/// offline producer interface never invokes a Die or starts a model.
pub(super) fn producer(args: ProducerArgs) -> i32 {
    report((|| {
        let mut store = args.database.open()?;
        let service = args.source.authenticate(
            &mut store,
            foundry_ledger::ProductServiceAction::DriveReview,
        )?;
        if args.action != "seal" && args.commit.is_some() {
            return Err(());
        }
        let order = &args.order;
        let key = &args.request_key;
        match args.action.as_str() {
            "reserve" => {
                use foundry_line::StageExecutor;
                let r = store
                    .code_change_for_producer(&service, order)
                    .map_err(|_| ())?;
                let config = registry()?;
                if !config.snapshots.contains_key(&r.pack_digest) {
                    return Err(());
                }
                let repo = std::env::var_os("FOUNDRY_REPO_PATH").ok_or(())?;
                let binary = std::env::var_os("FOUNDRY_DIE_BINARY").ok_or(())?;
                foundry_line::DieStageExecutor::new(
                    binary.into(),
                    "coding".into(),
                    foundry_line::DieStageExecutor::configured_timeout().map_err(|_| ())?,
                    repo.into(),
                    PathBuf::new(),
                )
                .ensure_available()
                .map_err(|_| ())?;
                require_local_commit(&r.base_commit)?;
                store = store.with_product_registry(config);
                store
                    .reserve_code_change(&service, order, key)
                    .map_err(|_| ())?;
            }
            "seal" => {
                let repo = PathBuf::from(std::env::var_os("FOUNDRY_REPO_PATH").ok_or(())?);
                store
                    .seal_code_change(
                        &service,
                        order,
                        key,
                        &repo,
                        args.commit.as_deref().ok_or(())?,
                    )
                    .map_err(|_| ())?;
            }
            "collect" => {
                store
                    .collect_code_change(&service, order, key)
                    .map_err(|_| ())?;
            }
            "recover" => {
                store
                    .mark_code_change_uncertain(&service, order, key)
                    .map_err(|_| ())?;
            }
            "status" => {}
            _ => return Err(()),
        }
        serde_json::to_value(
            store
                .code_change_status(&service, order, key)
                .map_err(|_| ())?,
        )
        .map_err(|_| ())
    })())
}
