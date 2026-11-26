//! Explicit product requests. Executable work needs a fresh decision from the
//! task's original owner naming this image and the exact W2 receipt.
use super::ProductLauncher;
use crate::selected_review::{Database, dispatch_bound, registry, require_local_commit};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use foundry_ledger::{
    ProductServiceAction, ProductServiceContext, ProductStore, product::CommitReviewRequest,
};
use foundry_line::{StageExecutor, StageTask};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Deserialize)]
#[serde(tag = "action", deny_unknown_fields, rename_all = "kebab-case")]
enum Operation {
    Review {
        commit: String,
        selection_revision: i64,
    },
    Produce {
        task_sha256: String,
        image: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    order: String,
    lifecycle: i64,
    admission_hash: String,
    request_key: String,
    connector: String,
    repository_id: String,
    api_origin: String,
    operation: Operation,
}

pub(crate) fn router(db: PathBuf, installation_id: String, database_id: String) -> Router {
    Router::new()
        .route("/api/product/execution", post(execute))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(Database {
            db,
            installation_id,
            database_id,
        })
}

async fn execute(
    State(db): State<Database>,
    headers: HeaderMap,
    Json(req): Json<Request>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let owner = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let session = hex::encode(Sha256::digest(owner.as_bytes()));
    let token = headers
        .get("x-foundry-service")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?
        .to_owned();
    tokio::task::spawn_blocking(move || {
        let store = db.open()?;
        if !store.scope_policy_enforced().map_err(|_| ())? {
            return Err(());
        }
        let service = store
            .authenticate_product_service_for_action(
                &token,
                &req.connector,
                &req.api_origin,
                &req.repository_id,
                ProductServiceAction::DriveReview,
                chrono::Utc::now(),
            )
            .map_err(|_| ())?;
        drop(token);
        if std::env::var("FOUNDRY_PRODUCT_REVIEW_SOURCE")
            .ok()
            .as_deref()
            != Some(service.source_instance_id())
        {
            return Err(());
        }
        match &req.operation {
            Operation::Review {
                commit,
                selection_revision,
            } => dispatch_bound(
                &db,
                store,
                &session,
                &service,
                CommitReviewRequest {
                    order: &req.order,
                    lifecycle: req.lifecycle,
                    admission_hash: &req.admission_hash,
                    commit,
                    selection_revision: *selection_revision,
                    request_key: &req.request_key,
                },
            ),
            Operation::Produce { task_sha256, image } => {
                produce(store, &session, &service, &req, task_sha256, image)
            }
        }
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map(|value| {
        let status = match value.get("state").and_then(Value::as_str) {
            Some("uncertain" | "reserved" | "sealed") => StatusCode::CONFLICT,
            _ => StatusCode::OK,
        };
        (status, Json(value))
    })
    .map_err(|_| StatusCode::FORBIDDEN)
}

fn produce(
    mut store: ProductStore,
    session: &str,
    service: &ProductServiceContext,
    req: &Request,
    task_sha: &str,
    image: &str,
) -> Result<Value, ()> {
    let receipt = store
        .code_change_for_producer(service, &req.order)
        .map_err(|_| ())?;
    if receipt.lifecycle != req.lifecycle
        || receipt.admission_hash != req.admission_hash
        || receipt.task_sha256 != task_sha
        || req.request_key.is_empty()
    {
        return Err(());
    }
    // Replaying the exact immutable receipt requires the original live owner,
    // task grant, source, selection and approval revisions.
    let fresh = store
        .authorize_code_change(
            session,
            &req.order,
            req.lifecycle,
            &req.admission_hash,
            task_sha,
            &receipt.request_key,
        )
        .map_err(|_| ())?;
    if fresh != receipt {
        return Err(());
    }
    let decision = json!(["isolated-produce-v1", receipt, image, req.request_key]);
    let key = format!(
        "w3-{:x}",
        Sha256::digest(serde_json::to_vec(&decision).map_err(|_| ())?)
    );
    if let Ok(existing) = store.code_change_status(service, &req.order, &key) {
        return serde_json::to_value(existing).map_err(|_| ());
    }
    let artifacts = PathBuf::from(std::env::var_os("FOUNDRY_PRODUCT_ARTIFACT_ROOT").ok_or(())?);
    let sessions = PathBuf::from(std::env::var_os("FOUNDRY_PRODUCT_SESSION_ROOT").ok_or(())?);
    let mut launcher = ProductLauncher::configured(&artifacts, &sessions, false).map_err(|_| ())?;
    if launcher.image != image {
        return Err(());
    }
    launcher.ensure_available().map_err(|_| ())?;
    require_local_commit(&receipt.base_commit)?;
    let configured = registry()?;
    if !configured.snapshots.contains_key(&receipt.pack_digest) {
        return Err(());
    }
    store = store.with_product_registry(configured);
    let claim = store
        .reserve_code_change(service, &req.order, &key)
        .map_err(|_| ())?;
    let run: Result<(), ()> = (|| {
        let dir = artifacts.join("w3-producers").join(&key);
        fs::create_dir_all(dir.parent().ok_or(())?).map_err(|_| ())?;
        fs::create_dir(&dir).map_err(|_| ())?;
        fs::write(
            dir.join("decision.json"),
            serde_json::to_vec(&decision).map_err(|_| ())?,
        )
        .map_err(|_| ())?;
        launcher.producer_task = Some(
            store
                .reserved_code_change_task(service, &claim, &key)
                .map_err(|_| ())?,
        );
        launcher.deadline = Some(claim.lease_until());
        let fence = claim.review_binding().fence;
        let intent = foundry_exec::ExecutionIntent {
            order: foundry_exec::OrderId(req.order.clone()),
            attempt: foundry_exec::Attempt(1),
            run: foundry_exec::RunId(1),
            activation: 1,
            generation: foundry_exec::Generation(0),
            epoch: foundry_exec::Epoch(fence as u64),
            recorded_at: chrono::Utc::now(),
            external_ref: key.clone(),
        };
        let stage = StageTask {
            stage_id: "authorized-producer".into(),
            role: "producer".into(),
            session_id: key.clone(),
            fix_context: None,
            base_commit: receipt.base_commit.clone(),
            artifact_commit: receipt.base_commit.clone(),
            lens_bytes: vec![],
            inputs: BTreeMap::new(),
            workdir: dir.clone(),
        };
        store
            .validate_product_work_claim(&claim, chrono::Utc::now())
            .map_err(|_| ())?;
        let output = launcher
            .execute_in_run(&intent, &key, &stage)
            .map_err(|_| ())?;
        let head = output.produced_commit.ok_or(())?;
        let sealed = store
            .seal_code_change(service, &req.order, &key, &dir.join("objects.git"), &head)
            .map_err(|_| ())?;
        let patch = fs::read(dir.join("collected.patch")).map_err(|_| ())?;
        if sealed.artifact_sha256.as_deref() != Some(&format!("{:x}", Sha256::digest(patch))) {
            return Err(());
        }
        store
            .collect_code_change(service, &req.order, &key)
            .map_err(|_| ())?;
        Ok(())
    })();
    let status = if run.is_ok() {
        store.code_change_status(service, &req.order, &key)
    } else {
        store.mark_code_change_uncertain(service, &req.order, &key)
    }
    .map_err(|_| ())?;
    serde_json::to_value(status).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn execution_requires_both_credentials_and_fixed_request_shape() {
        let root = std::env::temp_dir().join("unused-w3-execution-store");
        let db = Database {
            db: root.clone(),
            installation_id: "fixture".into(),
            database_id: "fixture".into(),
        };
        let body = json!({"order":"fixture","lifecycle":1,"admission_hash":"fixture","request_key":"fixture",
            "api_origin":"https://api.github.com","connector":"github-primary","repository_id":"repo:1",
            "operation":{"action":"produce","task_sha256":"fixture","image":"fixture"}});
        let mut injected = body.clone();
        injected["operation"]["argv"] = json!(["forbidden"]);
        assert!(serde_json::from_value::<Request>(injected).is_err());
        for (name, value) in [
            ("authorization", "Bearer fixture"),
            ("x-foundry-service", "fixture"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(name, value.parse().unwrap());
            let result = execute(
                State(db.clone()),
                headers,
                Json(serde_json::from_value(body.clone()).unwrap()),
            )
            .await;
            assert_eq!(result.unwrap_err(), StatusCode::UNAUTHORIZED);
        }
        assert!(!root.exists());
    }
}

#[cfg(test)]
#[path = "w31_test.rs"]
mod w31;
