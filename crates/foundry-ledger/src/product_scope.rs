//! Product scope identities, capability vocabulary, and explicit migration
//! manifest. Storage operations are owned by `ProductStore`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryIdentity {
    pub organization: String,
    pub provider: String,
    pub api_origin: String,
    pub stable_repository_id: String,
}

impl RepositoryIdentity {
    pub fn canonicalized(
        organization: impl Into<String>,
        provider: impl Into<String>,
        api_origin: impl Into<String>,
        stable_repository_id: impl Into<String>,
    ) -> Result<Self, ScopeInputError> {
        let organization = organization.into().trim().to_lowercase();
        let provider = provider.into().trim().to_lowercase();
        let stable_repository_id = stable_repository_id.into().trim().to_owned();
        if organization.is_empty()
            || organization.len() > 256
            || !organization
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || provider.is_empty()
            || provider.len() > 64
            || !provider
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || stable_repository_id.is_empty()
            || stable_repository_id.len() > 256
        {
            return Err(ScopeInputError);
        }
        let api_origin = canonical_origin(&api_origin.into())?;
        Ok(Self {
            organization,
            provider,
            api_origin,
            stable_repository_id,
        })
    }

    pub fn key(&self) -> String {
        framed_hash(
            "foundry-product-repository-identity-v1",
            &[
                &self.organization,
                &self.provider,
                &self.api_origin,
                &self.stable_repository_id,
            ],
        )
    }
}

fn canonical_origin(raw: &str) -> Result<String, ScopeInputError> {
    let value = raw.trim();
    let (scheme, remainder) = value.split_once("://").ok_or(ScopeInputError)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return Err(ScopeInputError);
    }
    if remainder.is_empty() || remainder.contains(['?', '#', '@', '\\']) {
        return Err(ScopeInputError);
    }
    let authority = remainder.split('/').next().ok_or(ScopeInputError)?;
    let suffix = &remainder[authority.len()..];
    if !suffix.is_empty() && suffix != "/" {
        return Err(ScopeInputError);
    }
    if authority.is_empty() || authority.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(ScopeInputError);
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (address, suffix) = bracketed.split_once(']').ok_or(ScopeInputError)?;
        address
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| ScopeInputError)?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(ScopeInputError)?)
        };
        (format!("[{address}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host.to_owned(), Some(port)),
            Some(_) => return Err(ScopeInputError),
            None => (authority.to_owned(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    let host = if host.starts_with('[') {
        host
    } else {
        let host = host.strip_suffix('.').unwrap_or(&host);
        if host.is_empty()
            || host.len() > 253
            || !host.is_ascii()
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(ScopeInputError);
        }
        host.to_owned()
    };
    let port = match port {
        Some(port) => {
            if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ScopeInputError);
            }
            let value = port.parse::<u16>().map_err(|_| ScopeInputError)?;
            if value == 0 {
                return Err(ScopeInputError);
            }
            let default = if scheme == "https" { 443 } else { 80 };
            (value != default).then(|| format!(":{value}"))
        }
        None => None,
    }
    .unwrap_or_default();
    Ok(format!("{scheme}://{host}{port}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductCapability {
    ReadOrder,
    ReadReview,
    ResolveReviewHold,
    ApproveOrder,
    ReadArtifact,
    ExportEvidence,
    ReadOperationalAudit,
    PublishApproved,
}

impl ProductCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOrder => "read_order",
            Self::ReadReview => "read_review",
            Self::ResolveReviewHold => "resolve_review_hold",
            Self::ApproveOrder => "approve_order",
            Self::ReadArtifact => "read_artifact",
            Self::ExportEvidence => "export_evidence",
            Self::ReadOperationalAudit => "read_operational_audit",
            Self::PublishApproved => "publish_approved",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ScopeInputError> {
        match value {
            "read_order" => Ok(Self::ReadOrder),
            "read_review" => Ok(Self::ReadReview),
            "resolve_review_hold" => Ok(Self::ResolveReviewHold),
            "approve_order" => Ok(Self::ApproveOrder),
            "read_artifact" => Ok(Self::ReadArtifact),
            "export_evidence" => Ok(Self::ExportEvidence),
            "read_operational_audit" => Ok(Self::ReadOperationalAudit),
            "publish_approved" => Ok(Self::PublishApproved),
            _ => Err(ScopeInputError),
        }
    }

    pub fn allowed_for_role(self, role: &str) -> bool {
        match role {
            "viewer" | "operator" => self == Self::ReadOrder,
            "approver" | "owner" => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMutation {
    Grant,
    Revoke,
}

impl ScopeMutation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Revoke => "revoke",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeSourceManifest {
    pub connector_id: String,
    pub source_instance_id: String,
    pub generation: i64,
    pub identity: RepositoryIdentity,
    /// Explicit GitHub API path components. These are request locators only;
    /// they never participate in repository authorization identity.
    #[serde(default)]
    pub api_owner: Option<String>,
    #[serde(default)]
    pub api_repo: Option<String>,
    /// Compatibility value copied from the pre-scope `product_sources_v1`
    /// field. It is used only to verify an operator-reviewed legacy mapping;
    /// new code must use `identity.stable_repository_id` for identity and the
    /// explicit `api_owner`/`api_repo` pair for network requests.
    pub legacy_repository_locator: String,
    pub scope_query: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeGrantManifest {
    pub actor_id: String,
    pub source_instance_id: String,
    pub capabilities: Vec<ProductCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedOrderProvenance {
    pub order_id: String,
    pub source_instance_id: String,
    pub lifecycle: i64,
    pub admission_sequence: i64,
    pub admission_hash: String,
}

/// Explicit operator-reviewed v1 -> scope-v2 maintenance input. An omitted
/// historical order is quarantined; no source or user grant is inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeMigrationManifest {
    pub manifest_id: String,
    pub reviewed_by: String,
    pub sources: Vec<ScopeSourceManifest>,
    pub verified_orders: Vec<VerifiedOrderProvenance>,
    pub grants: Vec<ScopeGrantManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeGrantReceipt {
    pub actor_id: String,
    pub subject_id: String,
    pub source_instance_id: String,
    pub capability: ProductCapability,
    pub mutation: ScopeMutationReceiptKind,
    pub scope_revision: i64,
    pub grant_revision: i64,
    pub request_key: String,
}

/// Owner-only administrative projection of a configured source. This contains
/// routing metadata only; it never includes order payloads or counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeSourceView {
    pub source: ScopeSourceManifest,
    pub source_instance_revision: i64,
    pub connector_revision: i64,
    /// Compatibility alias for connector_revision. New callers should use the
    /// explicitly named revision fields above.
    pub source_revision: i64,
    pub current: bool,
}

/// Owner-only administrative projection of one actor/source capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeGrantView {
    pub actor_id: String,
    pub source_instance_id: String,
    pub capability: ProductCapability,
    pub grant_revision: i64,
    pub scope_revision: i64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeMutationReceiptKind {
    Grant,
    Revoke,
}

impl From<ScopeMutation> for ScopeMutationReceiptKind {
    fn from(value: ScopeMutation) -> Self {
        match value {
            ScopeMutation::Grant => Self::Grant,
            ScopeMutation::Revoke => Self::Revoke,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedOrderView {
    pub order_id: String,
    pub lifecycle: i64,
    pub state: String,
    pub source_instance_id: String,
    pub repository_key: String,
    pub admission_sequence: i64,
    pub source_revision: i64,
    pub scope_revision: i64,
    pub grant_revision: i64,
}

/// Evidence selected for one authenticated, capability-scoped read. The rows
/// are raw only inside the product adapter so it can pseudonymize identifiers;
/// callers must never serialize LedgerRow directly. No global chain head or
/// sequence boundary is included in this snapshot.
#[derive(Debug, Clone)]
pub struct ScopedEvidenceSnapshot {
    pub rows: Vec<crate::LedgerRow>,
    /// True only when another order visible under this same grant exists.
    pub has_more: bool,
    /// False if any journal rows for selected orders lack matching immutable
    /// lifecycle provenance and a verified transition receipt.
    pub coverage_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeMigrationReceipt {
    pub manifest_digest: String,
    pub source_instances: usize,
    pub verified_orders: usize,
    pub quarantined_orders: usize,
    pub grants: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeInputError;

impl fmt::Display for ScopeInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid product scope input")
    }
}

impl std::error::Error for ScopeInputError {}

pub(crate) fn framed_hash(domain: &str, values: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update((domain.len() as u64).to_be_bytes());
    hash.update(domain.as_bytes());
    for value in values {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hex::encode(hash.finalize())
}

pub(crate) fn create_schema(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS product_scope_repositories_v1(
            repository_key TEXT PRIMARY KEY, organization TEXT NOT NULL, provider TEXT NOT NULL,
            api_origin TEXT NOT NULL, stable_repository_id TEXT NOT NULL,
            UNIQUE(organization,provider,api_origin,stable_repository_id));
         CREATE TABLE IF NOT EXISTS product_scope_source_instances_v1(
            source_instance_id TEXT PRIMARY KEY, connector_id TEXT NOT NULL, repository_key TEXT NOT NULL,
            generation INTEGER NOT NULL, revision INTEGER NOT NULL, legacy_repository_locator TEXT NOT NULL,
            scope_query TEXT NOT NULL, enabled INTEGER NOT NULL, retired_at TEXT,
            UNIQUE(connector_id,generation));
         CREATE TABLE IF NOT EXISTS product_scope_api_locators_v1(
            source_instance_id TEXT PRIMARY KEY, api_owner TEXT NOT NULL, api_repo TEXT NOT NULL);
         CREATE TRIGGER IF NOT EXISTS product_scope_api_locator_no_update
            BEFORE UPDATE ON product_scope_api_locators_v1
            BEGIN SELECT RAISE(ABORT,'product API locator mapping is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_api_locator_no_delete
            BEFORE DELETE ON product_scope_api_locators_v1
            BEGIN SELECT RAISE(ABORT,'product API locator mapping is immutable'); END;
         CREATE TABLE IF NOT EXISTS product_scope_current_sources_v1(
            connector_id TEXT PRIMARY KEY, source_instance_id TEXT NOT NULL UNIQUE);
         CREATE TABLE IF NOT EXISTS product_scope_order_provenance_v1(
            order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL, source_instance_id TEXT NOT NULL,
            admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL, verified INTEGER NOT NULL,
            PRIMARY KEY(order_id,lifecycle));
         CREATE TABLE IF NOT EXISTS product_artifacts_v1(
            artifact_id TEXT PRIMARY KEY, order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL,
            source_instance_id TEXT NOT NULL, repository_key TEXT NOT NULL,
            admission_seq INTEGER NOT NULL, admission_hash TEXT NOT NULL,
            attempt INTEGER NOT NULL, run_id TEXT NOT NULL, generation INTEGER NOT NULL,
            stage_sequence INTEGER NOT NULL, stage_id TEXT NOT NULL,
            artifact_sha256 TEXT NOT NULL, provenance_digest TEXT NOT NULL,
            relative_path TEXT NOT NULL, created_at TEXT NOT NULL,
            UNIQUE(order_id,lifecycle,attempt,run_id,generation,stage_sequence));
         CREATE TRIGGER IF NOT EXISTS product_artifact_no_update
            BEFORE UPDATE ON product_artifacts_v1
            BEGIN SELECT RAISE(ABORT,'product artifact binding is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_artifact_no_delete
            BEFORE DELETE ON product_artifacts_v1
            BEGIN SELECT RAISE(ABORT,'product artifact binding is immutable'); END;
         CREATE TABLE IF NOT EXISTS product_artifact_root_v1(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), root_path TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS product_artifact_root_identity_v1(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1), device INTEGER NOT NULL, inode INTEGER NOT NULL);
         CREATE TRIGGER IF NOT EXISTS product_artifact_root_identity_no_update
            BEFORE UPDATE ON product_artifact_root_identity_v1
            BEGIN SELECT RAISE(ABORT,'product artifact root identity is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_artifact_root_identity_no_delete
            BEFORE DELETE ON product_artifact_root_identity_v1
            BEGIN SELECT RAISE(ABORT,'product artifact root identity is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_artifact_root_no_update
            BEFORE UPDATE ON product_artifact_root_v1
            BEGIN SELECT RAISE(ABORT,'product artifact root is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_artifact_root_no_delete
            BEFORE DELETE ON product_artifact_root_v1
            BEGIN SELECT RAISE(ABORT,'product artifact root is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_provenance_immutable_update
            BEFORE UPDATE ON product_scope_order_provenance_v1
            BEGIN SELECT RAISE(ABORT,'product scope provenance is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_provenance_immutable_delete
            BEFORE DELETE ON product_scope_order_provenance_v1
            BEGIN SELECT RAISE(ABORT,'product scope provenance is immutable'); END;
         CREATE TABLE IF NOT EXISTS product_scope_quarantine_v1(
            order_id TEXT NOT NULL, lifecycle INTEGER NOT NULL, evidence_digest TEXT NOT NULL,
            reason TEXT NOT NULL, recorded_at TEXT NOT NULL, PRIMARY KEY(order_id,lifecycle));
         CREATE TABLE IF NOT EXISTS product_scope_subject_revisions_v1(
            subject_id TEXT NOT NULL, source_instance_id TEXT NOT NULL, revision INTEGER NOT NULL,
            PRIMARY KEY(subject_id,source_instance_id));
         CREATE TABLE IF NOT EXISTS product_scope_grants_v1(
            grant_id TEXT PRIMARY KEY, subject_id TEXT NOT NULL, source_instance_id TEXT NOT NULL,
            capability TEXT NOT NULL, grant_revision INTEGER NOT NULL, enabled INTEGER NOT NULL,
            created_by TEXT NOT NULL, request_key TEXT NOT NULL,
            UNIQUE(subject_id,source_instance_id,capability));
         CREATE TABLE IF NOT EXISTS product_scope_events_v1(
            event_seq INTEGER PRIMARY KEY AUTOINCREMENT, actor_id TEXT NOT NULL, subject_id TEXT NOT NULL,
            source_instance_id TEXT NOT NULL, capability TEXT NOT NULL, operation TEXT NOT NULL,
            scope_revision INTEGER NOT NULL, grant_revision INTEGER NOT NULL, request_key TEXT NOT NULL,
            request_digest TEXT NOT NULL, created_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS product_scope_source_events_v1(
            event_seq INTEGER PRIMARY KEY AUTOINCREMENT, actor_id TEXT NOT NULL,
            connector_id TEXT NOT NULL, old_source_instance_id TEXT, new_source_instance_id TEXT NOT NULL,
            operation TEXT NOT NULL, source_revision INTEGER NOT NULL, request_key TEXT NOT NULL,
            request_digest TEXT NOT NULL, created_at TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS product_scope_operations_v1(
            actor_id TEXT NOT NULL, request_key TEXT NOT NULL, request_digest TEXT NOT NULL,
            receipt_json TEXT NOT NULL, PRIMARY KEY(actor_id,request_key));
         CREATE TABLE IF NOT EXISTS product_scope_migrations_v1(
            manifest_id TEXT PRIMARY KEY, manifest_digest TEXT NOT NULL, reviewed_by TEXT NOT NULL,
            migrated_at TEXT NOT NULL, verified_orders INTEGER NOT NULL, quarantined_orders INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS product_scope_policy_state_v1(
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            mode TEXT NOT NULL CHECK(mode IN ('legacy_unscoped','enforced')));
         INSERT OR IGNORE INTO product_scope_policy_state_v1(singleton,mode) VALUES(1,'legacy_unscoped');
         CREATE TRIGGER IF NOT EXISTS product_scope_repository_no_update
            BEFORE UPDATE ON product_scope_repositories_v1
            BEGIN SELECT RAISE(ABORT,'product repository identity is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_repository_no_delete
            BEFORE DELETE ON product_scope_repositories_v1
            BEGIN SELECT RAISE(ABORT,'product repository identity is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_source_no_delete
            BEFORE DELETE ON product_scope_source_instances_v1
            BEGIN SELECT RAISE(ABORT,'product source history is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_source_identity_immutable
            BEFORE UPDATE ON product_scope_source_instances_v1
            WHEN NEW.source_instance_id IS NOT OLD.source_instance_id
              OR NEW.connector_id IS NOT OLD.connector_id
              OR NEW.repository_key IS NOT OLD.repository_key
              OR NEW.generation IS NOT OLD.generation
              OR NEW.legacy_repository_locator IS NOT OLD.legacy_repository_locator
              OR NEW.scope_query IS NOT OLD.scope_query
              OR NEW.revision<=OLD.revision
            BEGIN SELECT RAISE(ABORT,'product source identity is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_event_no_update
            BEFORE UPDATE ON product_scope_events_v1
            BEGIN SELECT RAISE(ABORT,'product scope events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_event_no_delete
            BEFORE DELETE ON product_scope_events_v1
            BEGIN SELECT RAISE(ABORT,'product scope events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_source_event_no_update
            BEFORE UPDATE ON product_scope_source_events_v1
            BEGIN SELECT RAISE(ABORT,'product source events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_source_event_no_delete
            BEFORE DELETE ON product_scope_source_events_v1
            BEGIN SELECT RAISE(ABORT,'product source events are append-only'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_quarantine_no_update
            BEFORE UPDATE ON product_scope_quarantine_v1
            BEGIN SELECT RAISE(ABORT,'product scope quarantine is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_quarantine_no_delete
            BEFORE DELETE ON product_scope_quarantine_v1
            BEGIN SELECT RAISE(ABORT,'product scope quarantine is immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_migration_no_update
            BEFORE UPDATE ON product_scope_migrations_v1
            BEGIN SELECT RAISE(ABORT,'product scope migration receipts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_migration_no_delete
            BEFORE DELETE ON product_scope_migrations_v1
            BEGIN SELECT RAISE(ABORT,'product scope migration receipts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_operation_no_update
            BEFORE UPDATE ON product_scope_operations_v1
            BEGIN SELECT RAISE(ABORT,'product scope operation receipts are immutable'); END;
         CREATE TRIGGER IF NOT EXISTS product_scope_operation_no_delete
            BEFORE DELETE ON product_scope_operations_v1
            BEGIN SELECT RAISE(ABORT,'product scope operation receipts are immutable'); END;"
    )
}
