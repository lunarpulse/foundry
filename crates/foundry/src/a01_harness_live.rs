//! Isolated real-path assembly; the creator retains the physical store identity.
use super::{a01_coordinator, a01_real_git_producer::RealGitProducer};
use chrono::{Duration, Utc};
use foundry_ledger::{product::*, *};
use rand_core::RngCore;
use std::{path::PathBuf, sync::Arc};

pub(crate) const PACK_DIGEST: &str =
    "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";

#[derive(Clone, clap::Args)]
pub(crate) struct LiveArgs {
    /// Must not already exist. All durable and execution evidence stays here.
    #[arg(long)]
    pub root: PathBuf,
    #[arg(long)]
    pub repository: PathBuf,
    #[arg(long)]
    pub base: String,
    #[arg(long)]
    pub die: PathBuf,
    #[arg(long)]
    pub pack: PathBuf,
    #[arg(long)]
    pub owner: String,
    #[arg(long)]
    pub repo: String,
    #[arg(long)]
    pub repository_id: String,
    /// Stable GitHub issue ID, never the issue number.
    #[arg(long)]
    pub issue_id: String,
    #[arg(long, default_value_t = 300)]
    pub timeout_secs: u64,
    #[arg(long, default_value_t = false)]
    pub private: bool,
    /// Name of an existing environment variable, resolved only by intake.
    #[arg(long)]
    pub credential_ref: Option<String>,
}

fn rejected<E>(_: E) -> String {
    "A01 live operation rejected".into()
}
fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 24];
    rand_core::OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(rejected)?;
    Ok(hex::encode(bytes))
}

pub(crate) struct Creator {
    pub(crate) root: PathBuf,
    pub(crate) store: ProductStore,
    identity: A01TaskStoreIdentity,
    pub(crate) config: ProductConfig,
    pub(crate) clock: Arc<dyn ProductClock>,
    pub(crate) session: String,
    pub(crate) session_bearer: String,
    pub(crate) admin: String,
    pub(crate) service: ProductServiceContext,
}
impl Creator {
    /// Also used by local fixtures; no existing store or identity is adopted.
    pub(crate) fn create(
        root: PathBuf,
        owner: &str,
        repo: &str,
        repository_id: &str,
    ) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .map_err(rejected)?;
        }
        #[cfg(not(unix))]
        {
            return Err("A01 requires Unix private directories".into());
        }
        let root = root.canonicalize().map_err(rejected)?;
        let config = ProductConfig {
            installation_id: format!("a01-{}", nonce()?),
            database_id: format!("a01-{}", nonce()?),
        };
        let clock: Arc<dyn ProductClock> = Arc::new(SystemProductClock);
        let (mut store, identity) = ProductStore::create_a01_task_claim_test_store(
            &root,
            "store.db",
            config.clone(),
            clock.clone(),
        )
        .map_err(rejected)?;
        let source = ScopeSourceManifest {
            connector_id: "github-primary".into(),
            source_instance_id: "a01-source".into(),
            generation: 1,
            identity: RepositoryIdentity::canonicalized(
                owner,
                "GitHub",
                "https://api.github.com",
                repository_id,
            )
            .map_err(rejected)?,
            api_owner: Some(owner.into()),
            api_repo: Some(repo.into()),
            legacy_repository_locator: format!("{owner}/{repo}"),
            scope_query: "open".into(),
            enabled: true,
        };
        let grants = [ScopeGrantManifest {
            actor_id: "a01-owner".into(),
            source_instance_id: "a01-source".into(),
            capabilities: vec![
                ProductCapability::ReadOrder,
                ProductCapability::ReadReview,
                ProductCapability::ReadArtifact,
                ProductCapability::ResolveReviewHold,
            ],
        }];
        let password = nonce()?;
        store
            .bootstrap_owner_scoped(
                "a01-owner",
                &product_identity::ProductPasswordKdf::hash(&password).map_err(rejected)?,
                &[source],
                &grants,
            )
            .map_err(rejected)?;
        let at = clock.now();
        let login = store.login("a01-owner", &password, at).map_err(rejected)?;
        let session = login.digest().to_owned();
        let session_bearer = login.bearer().to_owned();
        let password = nonce()?;
        store
            .create_account(
                &session,
                "a01-admin",
                "owner",
                &product_identity::ProductPasswordKdf::hash(&password).map_err(rejected)?,
                at,
            )
            .map_err(rejected)?;
        let admin = store
            .login("a01-admin", &password, at)
            .map_err(rejected)?
            .digest()
            .to_owned();
        let revision = store
            .list_product_scope_grants(&admin, "a01-owner", "a01-source", at)
            .map_err(rejected)?
            .first()
            .ok_or("A01 initial scope missing")?
            .scope_revision;
        store
            .change_product_scope(
                &admin,
                "a01-owner",
                "a01-source",
                ProductCapability::A01BridgePassed,
                ScopeMutation::Grant,
                revision,
                "bridge-issue",
                at,
            )
            .map_err(rejected)?;
        let grants: Vec<_> = [
            ProductServiceAction::IntakeRead,
            ProductServiceAction::AdmitDelivery,
            ProductServiceAction::AckDelivery,
            ProductServiceAction::DriveReview,
        ]
        .into_iter()
        .map(|action| ProductServiceGrant {
            source_instance_id: "a01-source".into(),
            action,
        })
        .collect();
        store
            .manage_product_service(
                &session,
                "a01-service",
                &grants,
                true,
                at + Duration::hours(4),
                0,
                "service",
                at,
            )
            .map_err(rejected)?;
        let credential = store
            .issue_product_service_credential(
                &session,
                "a01-service",
                1,
                at + Duration::hours(3),
                "credential",
                at,
            )
            .map_err(rejected)?;
        let service = store
            .authenticate_product_service_for_action(
                credential.expose_once_to_file_writer(),
                "github-primary",
                "https://api.github.com",
                repository_id,
                ProductServiceAction::DriveReview,
                at,
            )
            .map_err(rejected)?;
        Ok(Self {
            root,
            store,
            identity,
            config,
            clock,
            session,
            session_bearer,
            admin,
            service,
        })
    }
    pub(crate) fn checked(&self) -> Result<ProductStore, String> {
        ProductStore::reopen_a01_task_claim_test_store(
            &self.root,
            "store.db",
            self.config.clone(),
            self.clock.clone(),
            &self.identity,
        )
        .map_err(rejected)
    }
    pub(crate) fn backend(&self) -> Result<ProductStore, String> {
        let mut store = self.checked()?;
        store
            .configure_review_backend(ProductReviewConfig {
                artifact_root: self.root.join("artifacts"),
                session_root: self.root.join("sessions"),
                claim_root: self.root.join("claims"),
                pack_digest: PACK_DIGEST.into(),
            })
            .map_err(rejected)?;
        Ok(store)
    }
    pub(crate) fn host(
        &self,
        pack: &std::path::Path,
        executor: impl foundry_line::StageExecutor + 'static,
        source: &str,
    ) -> Result<foundry_line::PackReviewHost, String> {
        foundry_line::PackReviewHost::with_product_store(
            pack,
            executor,
            self.checked()?,
            self.root.join("artifacts"),
            self.root.join("sessions"),
            self.root.join("claims"),
        )
        .and_then(|h| h.with_source_commit(source))
        .map_err(rejected)
    }
    pub(crate) fn request(
        &mut self,
        receipts: &[AdmissionReceipt],
        issue: &str,
    ) -> Result<A01AuthorizeTaskRequest, String> {
        let permission = self
            .store
            .read_a01_bridge_permission(&self.session, self.service.source_instance_id())
            .map_err(rejected)?
            .ok_or("A01 explicit bridge permission missing")?;
        self.store
            .prepare_a01_authorization_request(
                &self.session,
                &self.service,
                receipts,
                issue,
                &permission,
                "authorize",
                (self.clock.now() + Duration::minutes(50))
                    .timestamp_nanos_opt()
                    .ok_or("A01 time rejected")?,
            )
            .map_err(rejected)
    }
    pub(crate) fn authorize_claim(
        &mut self,
        request: &A01AuthorizeTaskRequest,
    ) -> Result<(A01TaskReceipt, ProductWorkClaim), String> {
        let task = self
            .store
            .authorize_a01_task(&self.session, &self.service, request)
            .map_err(rejected)?;
        let claim = match self
            .store
            .claim_product_work_for_service(
                &self.service,
                "a01-live",
                self.clock.now(),
                Duration::hours(1),
            )
            .map_err(rejected)?
        {
            ProductWorkClaimResult::Claimed(c) => c,
            _ => return Err("A01 scoped claim missing".into()),
        };
        if claim.order_id() != task.admission.order_id {
            return Err("A01 selected claim mismatch".into());
        }
        Ok((task, claim))
    }
    pub(crate) fn finalize(
        &self,
        d: &a01_coordinator::AcceptedDispatch,
    ) -> Result<ProductResultInfo, String> {
        let expected = ProductResultExpectation {
            host_key: foundry_exec::HostKey {
                order: d.output.intent.order.0.clone(),
                attempt: d.output.intent.attempt.0,
                run: d.output.intent.run.0,
                activation: d.output.intent.activation,
            },
            host_result_digest: product_host_result_digest(&d.host).map_err(rejected)?,
        };
        self.backend()?
            .finalize_product_review_result(&self.service, &expected)
            .map_err(rejected)
    }
}

pub(crate) fn run(args: LiveArgs) -> Result<(), String> {
    if args.issue_id.is_empty()
        || args.repository_id.is_empty()
        || (args.private && args.credential_ref.is_none())
    {
        return Err("A01 live configuration rejected".into());
    }
    let binary = args.die.canonicalize().map_err(rejected)?;
    let mut creator = Creator::create(
        args.root.clone(),
        &args.owner,
        &args.repo,
        &args.repository_id,
    )?;
    let mut producer = RealGitProducer::new(
        binary.clone(),
        args.timeout_secs,
        args.repository.clone(),
        creator.root.clone(),
        &args.base,
    )?;
    let executor = foundry_line::DieStageExecutor::new(
        binary,
        "coding".into(),
        args.timeout_secs,
        args.repository,
        creator.root.join("sessions"),
    );
    let mut host = creator.host(&args.pack, executor, producer.base())?;
    let cfg = foundry_watch::intake::ProductGithubConfig {
        connector_id: "github-primary".into(),
        api_origin: "https://api.github.com".into(),
        owner: args.owner,
        repo: args.repo,
        repository_id: args.repository_id,
        private: args.private,
        credential_ref: args.credential_ref,
        allow_loopback_http: false,
        max_pages_per_tick: 5,
        now: Utc::now(),
    };
    // Intake has a distinct checked handle, always opened with the retained identity.
    let (_, drain) = foundry_watch::intake_worker::product_github_tick(
        &mut creator.checked()?,
        &cfg,
        &super::EnvSecretProvider,
        Utc::now(),
        1000,
        &creator.service,
    )
    .map_err(rejected)?;
    if matches!(drain.work_claim, ProductWorkClaimResult::Claimed(_)) {
        return Err("A01 fresh tick unexpectedly claimed".into());
    }
    let request = creator.request(&drain.receipts, &args.issue_id)?;
    let (task, claim) = creator.authorize_claim(&request)?;
    let dispatched = a01_coordinator::dispatch(
        &mut creator.store,
        &creator.service,
        &claim,
        task.revision,
        "producer",
        &mut producer,
        &mut host,
    )?;
    decode_a01_real_output(&dispatched.output.reservation, &dispatched.output.output)
        .map_err(rejected)?;
    if dispatched.host.held.is_some() {
        return Err("A01 Held; evidence retained for creator-lifetime continuation".into());
    }
    if dispatched.host.exit_code != 0 {
        return Err("A01 review did not pass; evidence retained".into());
    }
    creator.finalize(&dispatched)?;
    Ok(())
}
