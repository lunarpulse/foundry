//! Source-bound service principals for product intake and review execution.
//! Service credentials are opaque high-entropy bearers; this module stores
//! only their verifier and never implements human approval or publish rights.

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::product::{ProductError, ProductStore, resolve_session_in, scope_enforced_in};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductServiceAction {
    IntakeRead,
    AdmitDelivery,
    AckDelivery,
    DriveReview,
}

impl ProductServiceAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IntakeRead => "intake_read",
            Self::AdmitDelivery => "admit_delivery",
            Self::AckDelivery => "ack_delivery",
            Self::DriveReview => "drive_review",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductServiceGrant {
    pub source_instance_id: String,
    pub action: ProductServiceAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductServiceReceipt {
    pub service_id: String,
    pub revision: i64,
    pub enabled: bool,
    pub lease_until: String,
    pub request_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductServiceView {
    pub service_id: String,
    pub revision: i64,
    pub credential_revision: i64,
    pub enabled: bool,
    pub lease_until: String,
    pub grants: Vec<ProductServiceGrant>,
}

/// Plaintext is returned only by the one-time issuance call. It has no Debug
/// or serde implementation and callers should write it to a private file.
pub struct ProductServiceCredential(String);

impl ProductServiceCredential {
    pub fn expose_once_to_file_writer(&self) -> &str {
        &self.0
    }
}

/// Non-forgeable in-process authorization context. Each store operation
/// rechecks the current service, credential, lease, source generation and
/// action grant in its own SQLite transaction.
#[derive(Clone)]
pub struct ProductServiceContext {
    pub(crate) service_id: String,
    pub(crate) service_revision: i64,
    pub(crate) credential_revision: i64,
    pub(crate) source_instance_id: String,
    /// Compatibility source_revision below is the connector revision used by
    /// existing ProductAtomicV1 order/claim receipts. These independent values
    /// must never be compared to it interchangeably.
    pub(crate) source_generation: i64,
    pub(crate) source_instance_revision: i64,
    pub(crate) connector_revision: i64,
    pub(crate) source_revision: i64,
    pub(crate) connector_id: String,
    pub(crate) api_origin: String,
    pub(crate) repository_id: String,
    pub(crate) api_owner: String,
    pub(crate) api_repo: String,
}

impl ProductServiceContext {
    pub fn service_id(&self) -> &str {
        &self.service_id
    }
    pub fn source_instance_id(&self) -> &str {
        &self.source_instance_id
    }
}

impl ProductStore {
    /// Create or revise an explicit service/action grant set. The authenticated
    /// human Owner supplies a finite lease and exact source instances; source
    /// creation never creates a service or grant implicitly.
    #[allow(clippy::too_many_arguments)]
    pub fn manage_product_service(
        &mut self,
        session_digest: &str,
        service_id: &str,
        grants: &[ProductServiceGrant],
        enabled: bool,
        lease_until: DateTime<Utc>,
        expected_revision: i64,
        request_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductServiceReceipt, ProductError> {
        if service_id.trim().is_empty()
            || service_id.len() > 128
            || !service_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || request_key.trim().is_empty()
            || request_key.len() > 256
            || lease_until <= now
            || (enabled && grants.is_empty())
            || grants.len() > 64
        {
            return Err(ProductError::InvalidConfig);
        }
        let mut unique = std::collections::BTreeSet::new();
        for grant in grants {
            if grant.source_instance_id.trim().is_empty()
                || !unique.insert((grant.source_instance_id.as_str(), grant.action.as_str()))
            {
                return Err(ProductError::InvalidConfig);
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !scope_enforced_in(&tx)? {
            return Err(ProductError::Invariant(
                "service management requires enforced product scope",
            ));
        }
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot administer services",
            ));
        }
        let mut parts = vec![
            service_id.to_owned(),
            enabled.to_string(),
            lease_until.to_rfc3339(),
            expected_revision.to_string(),
        ];
        for grant in grants {
            parts.push(grant.source_instance_id.clone());
            parts.push(grant.action.as_str().to_owned());
        }
        let digest = crate::product_scope::framed_hash(
            "foundry-product-service-manage-v1",
            &parts.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let replay: Option<(String,String)> = tx.query_row(
            "SELECT request_digest,receipt_json FROM product_service_operations_v1 WHERE actor_id=?1 AND request_key=?2",
            params![principal.actor_id,request_key], |r|Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        if let Some((saved, receipt)) = replay {
            if saved != digest {
                return Err(ProductError::Invariant("service request key conflict"));
            }
            let receipt: ProductServiceReceipt = serde_json::from_str(&receipt)
                .map_err(|_| ProductError::Invariant("stored service receipt invalid"))?;
            tx.commit()?;
            return Ok(receipt);
        }
        let old: Option<i64> = tx
            .query_row(
                "SELECT revision FROM product_services_v1 WHERE service_id=?1",
                [service_id],
                |r| r.get(0),
            )
            .optional()?;
        if old.unwrap_or(0) != expected_revision {
            return Err(ProductError::Invariant("service revision conflict"));
        }
        for grant in grants {
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM product_scope_source_instances_v1 i JOIN product_scope_current_sources_v1 c ON c.connector_id=i.connector_id AND c.source_instance_id=i.source_instance_id WHERE i.source_instance_id=?1 AND i.enabled=1)",
                [&grant.source_instance_id], |r|r.get(0),
            )?;
            if !valid {
                return Err(ProductError::Invariant(
                    "service source instance is unavailable",
                ));
            }
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or(ProductError::InvalidConfig)?;
        // A service bearer authenticates the current persisted verifier, not
        // the revision captured by a previously-created context. Replace the
        // verifier on every effective management change so revoke/regrant or
        // grant reconfiguration cannot make an old bearer current again.
        invalidate_service_bearer_in(&tx, service_id)?;
        tx.execute(
            "INSERT INTO product_services_v1(service_id,revision,enabled,lease_until) VALUES(?1,?2,?3,?4) ON CONFLICT(service_id) DO UPDATE SET revision=excluded.revision,enabled=excluded.enabled,lease_until=excluded.lease_until",
            params![service_id,revision,i64::from(enabled),lease_until.to_rfc3339()],
        )?;
        tx.execute("UPDATE product_service_grants_v1 SET enabled=0,grant_revision=grant_revision+1 WHERE service_id=?1 AND enabled=1", [service_id])?;
        for grant in grants {
            tx.execute(
                "INSERT INTO product_service_grants_v1(service_id,source_instance_id,action,grant_revision,enabled) VALUES(?1,?2,?3,1,1) ON CONFLICT(service_id,source_instance_id,action) DO UPDATE SET grant_revision=product_service_grants_v1.grant_revision+1,enabled=1",
                params![service_id,grant.source_instance_id,grant.action.as_str()],
            )?;
        }
        let receipt = ProductServiceReceipt {
            service_id: service_id.to_owned(),
            revision,
            enabled,
            lease_until: lease_until.to_rfc3339(),
            request_key: request_key.to_owned(),
        };
        let receipt_json = serde_json::to_string(&receipt)
            .map_err(|_| ProductError::Invariant("service receipt serialization failed"))?;
        tx.execute("INSERT INTO product_service_events_v1(service_id,actor_id,revision,operation,request_key,request_digest,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![service_id,principal.actor_id,revision,if enabled {"configure"} else {"revoke"},request_key,digest,now.to_rfc3339()])?;
        tx.execute("INSERT INTO product_service_operations_v1(actor_id,request_key,request_digest,receipt_json) VALUES(?1,?2,?3,?4)", params![principal.actor_id,request_key,digest,receipt_json])?;
        tx.commit()?;
        Ok(receipt)
    }

    /// Issue or rotate a service bearer exactly once. A lost/private-file
    /// failure cannot be replayed to recover plaintext; an Owner must rotate
    /// with a fresh request key and private-file path.
    pub fn issue_product_service_credential(
        &mut self,
        session_digest: &str,
        service_id: &str,
        expected_service_revision: i64,
        expires_at: DateTime<Utc>,
        request_key: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductServiceCredential, ProductError> {
        use rand_core::RngCore as _;
        if request_key.trim().is_empty() || request_key.len() > 256 || expires_at <= now {
            return Err(ProductError::InvalidConfig);
        }
        let mut random = [0u8; 32];
        rand_core::OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| ProductError::Invariant("secure random source unavailable"))?;
        let token = hex::encode(random);
        random.fill(0);
        let verifier = crate::product::sha256_hex(token.as_bytes());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot issue service credentials",
            ));
        }
        let row: Option<(i64, i64, String)> = tx
            .query_row(
                "SELECT revision,enabled,lease_until FROM product_services_v1 WHERE service_id=?1",
                [service_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((revision, enabled, lease)) = row else {
            return Err(ProductError::Invariant("service unavailable"));
        };
        let lease = DateTime::parse_from_rfc3339(&lease)
            .map_err(|_| ProductError::Invariant("service lease invalid"))?
            .with_timezone(&Utc);
        if revision != expected_service_revision
            || enabled != 1
            || expires_at > lease
            || lease <= now
        {
            return Err(ProductError::Invariant("service revision or lease changed"));
        }
        let issued_before: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_service_events_v1 WHERE service_id=?1 AND operation='credential_issue' AND request_key=?2)",params![service_id,request_key],|r|r.get(0))?;
        if issued_before {
            return Err(ProductError::Invariant(
                "credential issuance already consumed; rotate with a fresh request key",
            ));
        }
        let current_credential_revision: i64 = tx.query_row("SELECT credential_revision FROM product_service_credentials_v1 WHERE service_id=?1",[service_id],|r|r.get(0)).optional()?.unwrap_or(0);
        let credential_revision = current_credential_revision
            .checked_add(1)
            .ok_or(ProductError::InvalidConfig)?;
        tx.execute("INSERT INTO product_service_credentials_v1(service_id,verifier,credential_revision,expires_at,issued_by,request_key) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(service_id) DO UPDATE SET verifier=excluded.verifier,credential_revision=excluded.credential_revision,expires_at=excluded.expires_at,issued_by=excluded.issued_by,request_key=excluded.request_key",params![service_id,verifier,credential_revision,expires_at.to_rfc3339(),principal.actor_id,request_key])?;
        tx.execute("INSERT INTO product_service_events_v1(service_id,actor_id,revision,operation,request_key,request_digest,created_at) VALUES(?1,?2,?3,'credential_issue',?4,?5,?6)",params![service_id,principal.actor_id,revision,request_key,verifier,now.to_rfc3339()])?;
        tx.commit()?;
        Ok(ProductServiceCredential(token))
    }

    pub fn list_product_services(
        &mut self,
        session_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProductServiceView>, ProductError> {
        let tx = self.conn.transaction()?;
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if principal.role != "owner" {
            return Err(ProductError::Invariant(
                "current role cannot query services",
            ));
        }
        let mut stmt = tx.prepare("SELECT s.service_id,s.revision,s.enabled,s.lease_until,COALESCE(c.credential_revision,0) FROM product_services_v1 s LEFT JOIN product_service_credentials_v1 c USING(service_id) ORDER BY s.service_id")?;
        let base = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut result = Vec::with_capacity(base.len());
        for (service_id, revision, enabled, lease_until, credential_revision) in base {
            let mut grants_stmt = tx.prepare("SELECT source_instance_id,action FROM product_service_grants_v1 WHERE service_id=?1 AND enabled=1 ORDER BY source_instance_id,action")?;
            let grants = grants_stmt
                .query_map([&service_id], |r| {
                    let source_instance_id: String = r.get(0)?;
                    let action: String = r.get(1)?;
                    let action = parse_action(&action).ok_or(rusqlite::Error::InvalidQuery)?;
                    Ok(ProductServiceGrant {
                        source_instance_id,
                        action,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            result.push(ProductServiceView {
                service_id,
                revision,
                credential_revision,
                enabled: enabled == 1,
                lease_until,
                grants,
            });
        }
        tx.commit()?;
        Ok(result)
    }

    /// Authenticate a private file bearer for one exact currently configured
    /// repository instance. The returned context is revalidated per use.
    pub fn authenticate_product_service(
        &self,
        token: &str,
        connector: &str,
        origin: &str,
        repository_id: &str,
        now: DateTime<Utc>,
    ) -> Result<ProductServiceContext, ProductError> {
        self.authenticate_product_service_for_action(
            token,
            connector,
            origin,
            repository_id,
            ProductServiceAction::IntakeRead,
            now,
        )
    }

    pub fn authenticate_product_service_for_action(
        &self,
        token: &str,
        connector: &str,
        origin: &str,
        repository_id: &str,
        action: ProductServiceAction,
        now: DateTime<Utc>,
    ) -> Result<ProductServiceContext, ProductError> {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ProductError::Invariant("service credential invalid"));
        }
        let verifier = crate::product::sha256_hex(token.as_bytes());
        let row: Option<(String,i64,i64,String)> = self.conn.query_row("SELECT s.service_id,s.revision,c.credential_revision,s.lease_until FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) WHERE c.verifier=?1 AND s.enabled=1",[verifier],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((service_id, service_revision, credential_revision, lease)) = row else {
            return Err(ProductError::Invariant("service credential invalid"));
        };
        let lease = DateTime::parse_from_rfc3339(&lease)
            .map_err(|_| ProductError::Invariant("service lease invalid"))?
            .with_timezone(&Utc);
        let expires: String = self.conn.query_row(
            "SELECT expires_at FROM product_service_credentials_v1 WHERE service_id=?1",
            [&service_id],
            |r| r.get(0),
        )?;
        let expires = DateTime::parse_from_rfc3339(&expires)
            .map_err(|_| ProductError::Invariant("service credential expiry invalid"))?
            .with_timezone(&Utc);
        if lease <= now || expires <= now {
            return Err(ProductError::Invariant("service credential expired"));
        }
        let (source_instance_id,source_generation,source_instance_revision,connector_revision,api_owner,api_repo): (String,i64,i64,i64,String,String) = self.conn.query_row(
            "SELECT i.source_instance_id,i.generation,i.revision,s.revision,l.api_owner,l.api_repo FROM product_scope_source_instances_v1 i JOIN product_scope_current_sources_v1 c ON c.connector_id=i.connector_id AND c.source_instance_id=i.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 s ON s.connector_id=i.connector_id WHERE i.connector_id=?1 AND i.enabled=1 AND s.enabled=1 AND r.api_origin=?2 AND r.stable_repository_id=?3 AND r.provider=s.provider AND s.origin=?2",
            params![connector,origin,repository_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
        )?;
        let context = ProductServiceContext {
            service_id,
            service_revision,
            credential_revision,
            source_instance_id,
            source_generation,
            source_instance_revision,
            connector_revision,
            source_revision: connector_revision,
            connector_id: connector.to_owned(),
            api_origin: origin.to_owned(),
            repository_id: repository_id.to_owned(),
            api_owner,
            api_repo,
        };
        let tx = self.conn.unchecked_transaction()?;
        require_service_context_in(&tx, &context, action, now)?;
        tx.commit()?;
        Ok(context)
    }

    pub fn require_product_service(
        &self,
        context: &ProductServiceContext,
        action: ProductServiceAction,
        now: DateTime<Utc>,
    ) -> Result<(), ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        require_service_context_in(&tx, context, action, now)?;
        tx.commit()?;
        Ok(())
    }
}

fn invalidate_service_bearer_in(
    tx: &rusqlite::Transaction<'_>,
    service_id: &str,
) -> Result<(), ProductError> {
    use rand_core::RngCore as _;

    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_service_credentials_v1 WHERE service_id=?1)",
        [service_id],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(());
    }
    let mut random = [0u8; 32];
    rand_core::OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| ProductError::Invariant("secure random source unavailable"))?;
    let invalid_verifier = crate::product::sha256_hex(&random);
    random.fill(0);
    tx.execute(
        "UPDATE product_service_credentials_v1 SET verifier=?1,credential_revision=credential_revision+1,expires_at=?2,request_key='invalidated' WHERE service_id=?3",
        params![invalid_verifier, "1970-01-01T00:00:00+00:00", service_id],
    )?;
    Ok(())
}

pub(crate) fn require_service_context_in(
    tx: &rusqlite::Transaction<'_>,
    context: &ProductServiceContext,
    action: ProductServiceAction,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    let exact: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_services_v1 s JOIN product_service_credentials_v1 c USING(service_id) JOIN product_service_grants_v1 g ON g.service_id=s.service_id JOIN product_scope_source_instances_v1 i ON i.source_instance_id=g.source_instance_id JOIN product_scope_current_sources_v1 cs ON cs.connector_id=i.connector_id AND cs.source_instance_id=i.source_instance_id JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key JOIN product_scope_api_locators_v1 l USING(source_instance_id) JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id WHERE s.service_id=?1 AND s.revision=?2 AND s.enabled=1 AND s.lease_until>?3 AND c.credential_revision=?4 AND c.expires_at>?3 AND g.source_instance_id=?5 AND g.action=?6 AND g.enabled=1 AND i.enabled=1 AND i.generation=?7 AND i.revision=?8 AND ps.revision=?9 AND ps.enabled=1 AND i.connector_id=?10 AND r.api_origin=?11 AND r.stable_repository_id=?12 AND ps.origin=?11 AND l.api_owner=?13 COLLATE NOCASE AND l.api_repo=?14 COLLATE NOCASE)",
        params![context.service_id,context.service_revision,now.to_rfc3339(),context.credential_revision,context.source_instance_id,action.as_str(),context.source_generation,context.source_instance_revision,context.connector_revision,context.connector_id,context.api_origin,context.repository_id,context.api_owner,context.api_repo],|r|r.get(0),
    )?;
    if !exact {
        return Err(ProductError::Invariant(
            "service authority is absent, expired, or revoked",
        ));
    }
    Ok(())
}

pub(crate) fn validate_review_work_claim_in(
    tx: &rusqlite::Transaction<'_>,
    claim: &foundry_exec::ReviewWorkClaim,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    let enforced = scope_enforced_in(tx)?;
    let Some(binding) = claim.service_binding.as_ref() else {
        return if enforced {
            Err(ProductError::Invariant("review service authority required"))
        } else {
            Ok(())
        };
    };
    let valid: bool = tx.query_row(
        "SELECT EXISTS(
          SELECT 1 FROM product_service_work_bindings_v1 w
          JOIN product_services_v1 s ON s.service_id=w.service_id
          JOIN product_service_credentials_v1 c ON c.service_id=s.service_id
          JOIN product_service_grants_v1 g ON g.service_id=s.service_id AND g.source_instance_id=w.source_instance_id AND g.action='drive_review' AND g.enabled=1
          JOIN product_scope_source_instances_v1 i ON i.source_instance_id=w.source_instance_id
          JOIN product_scope_current_sources_v1 cs ON cs.connector_id=i.connector_id AND cs.source_instance_id=i.source_instance_id
          JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key
          JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id
          WHERE w.order_id=?1 AND w.lifecycle=?2 AND w.service_id=?3 AND w.service_revision=?4
            AND w.credential_revision=?5 AND w.source_instance_id=?6 AND s.revision=?4 AND s.enabled=1
            AND s.lease_until>?7 AND c.credential_revision=?5 AND c.expires_at>?7
            AND i.enabled=1 AND ps.enabled=1
            AND ps.revision=?8 AND EXISTS(SELECT 1 FROM product_work_claims_v1 wc WHERE wc.order_id=w.order_id AND wc.lifecycle=w.lifecycle AND wc.owner_id=?9 AND wc.epoch=?10 AND wc.fence=?11 AND wc.source_revision=?8 AND wc.state='claimed' AND wc.lease_until>?7)
        )",
        params![claim.order_id,claim.lifecycle,binding.service_id,binding.service_revision,binding.credential_revision,binding.source_instance_id,now.to_rfc3339(),claim.source_revision,claim.worker_id,claim.epoch,claim.fence],|r|r.get(0),
    )?;
    if !valid {
        return Err(ProductError::Invariant(
            "review service authority is stale, expired, or revoked",
        ));
    }
    Ok(())
}

pub(crate) fn review_work_service_valid_in(
    tx: &rusqlite::Transaction<'_>,
    order_id: &str,
    attempt: i64,
    run: &str,
    now: DateTime<Utc>,
) -> Result<bool, ProductError> {
    if !scope_enforced_in(tx)? {
        return Ok(true);
    }
    tx.query_row(
        "SELECT EXISTS(
          SELECT 1 FROM product_review_work_bindings_v1 b
          JOIN product_service_work_bindings_v1 w ON w.order_id=b.order_id AND w.lifecycle=b.lifecycle
          JOIN product_services_v1 s ON s.service_id=w.service_id
          JOIN product_service_credentials_v1 c ON c.service_id=s.service_id
          JOIN product_service_grants_v1 g ON g.service_id=s.service_id AND g.source_instance_id=w.source_instance_id AND g.action='drive_review' AND g.enabled=1
          JOIN product_scope_source_instances_v1 i ON i.source_instance_id=w.source_instance_id
          JOIN product_scope_current_sources_v1 cs ON cs.connector_id=i.connector_id AND cs.source_instance_id=i.source_instance_id
          JOIN product_scope_repositories_v1 r ON r.repository_key=i.repository_key
          JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id
          JOIN product_work_claims_v1 wc ON wc.order_id=b.order_id AND wc.lifecycle=b.lifecycle
          WHERE b.order_id=?1 AND b.attempt=?2 AND b.run_id=?3 AND w.service_id=s.service_id
            AND s.enabled=1 AND s.revision=w.service_revision AND s.lease_until>?4
            AND c.credential_revision=w.credential_revision AND c.expires_at>?4
            AND i.enabled=1 AND ps.enabled=1
            AND wc.state='claimed' AND wc.owner_id=b.worker_id AND wc.epoch=b.claim_epoch AND wc.fence=b.claim_fence
            AND wc.lease_until>?4 AND wc.source_revision=b.source_revision
        )",
        params![order_id,attempt,run,now.to_rfc3339()],|r|r.get(0),
    ).map_err(ProductError::from)
}

fn parse_action(value: &str) -> Option<ProductServiceAction> {
    match value {
        "intake_read" => Some(ProductServiceAction::IntakeRead),
        "admit_delivery" => Some(ProductServiceAction::AdmitDelivery),
        "ack_delivery" => Some(ProductServiceAction::AckDelivery),
        "drive_review" => Some(ProductServiceAction::DriveReview),
        _ => None,
    }
}
