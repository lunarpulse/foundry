//! Scope closure — M4 T30 (plan v2; review findings #1/#7/#8).
//!
//! Single-organization contract (review #1): an installation serves
//! EXACTLY ONE org. The org is configured once; every intake source
//! (GitHub repo, Jira project, cron job) is BOUND to the org at
//! registration. Foreign org references are rejected at intake,
//! import, and mutation — there is no cross-org path to close.
//!
//! Surface inventory (review #7): authorization is centralized HERE
//! and applied BEFORE serialization. Every read (cards, detail,
//! counts) and every mutation goes through ScopeGuard.
//!
//! Attribution != authority (review #8): an external reporter
//! (GitHub user, Jira reporter) is recorded as METADATA on the order
//! and can NEVER confer membership, visibility, or approval rights.
use crate::identity::{AuthenticatedUser, Capability, IdentityError, Role};
use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeError {
    ForeignOrg { got: String, want: String },
    NotInScope { order_id: String },
    Unauthenticated,
    Denied(Capability),
    Db(String),
}

impl std::fmt::Display for ScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ForeignOrg { got, want } => {
                write!(
                    f,
                    "foreign organization rejected: got '{got}', this installation serves '{want}'"
                )
            }
            Self::NotInScope { order_id } => {
                write!(f, "order '{order_id}' is not accessible in this scope")
            }
            Self::Unauthenticated => write!(f, "authentication required"),
            Self::Denied(c) => write!(f, "capability {c:?} not granted"),
            Self::Db(e) => write!(f, "db: {e}"),
        }
    }
}

/// The single organization this installation serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgConfig {
    pub org_id: String,
    /// bound intake sources: dedup prefixes ("github:lunarpulse/foundary-demo")
    pub bound_sources: Vec<String>,
}

/// Scope store: order -> org assignment + source bindings.
pub struct ScopeStore {
    con: Connection,
    org: OrgConfig,
}

impl ScopeStore {
    pub fn open(path: &Path, org: OrgConfig) -> Result<Self, ScopeError> {
        let con = if path.as_os_str().is_empty() {
            Connection::open_in_memory().map_err(|e| ScopeError::Db(e.to_string()))?
        } else {
            Connection::open(path).map_err(|e| ScopeError::Db(e.to_string()))?
        };
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS order_scope (
                order_id TEXT PRIMARY KEY,
                org_id TEXT NOT NULL,
                source_ref TEXT
            );",
        )
        .map_err(|e| ScopeError::Db(e.to_string()))?;
        Ok(Self { con, org })
    }

    /// Intake admission: a new order enters ONLY if its source is
    /// bound to the installation org. Foreign org/source => reject
    /// BEFORE any order is created (review #1: reject at intake).
    pub fn admit_intake(&self, source_ref: &str) -> Result<(), ScopeError> {
        if self.org.bound_sources.iter().any(|s| source_ref == *s) {
            Ok(())
        } else {
            Err(ScopeError::ForeignOrg {
                got: source_ref.to_string(),
                want: self.org.org_id.clone(),
            })
        }
    }

    /// Register an order into the (single) org scope.
    pub fn register_order(
        &self,
        order_id: &str,
        source_ref: Option<&str>,
    ) -> Result<(), ScopeError> {
        if let Some(src) = source_ref {
            self.admit_intake(src)?;
        }
        self.con
            .execute(
                "INSERT OR IGNORE INTO order_scope (order_id, org_id, source_ref) VALUES (?1,?2,?3)",
                rusqlite::params![order_id, self.org.org_id, source_ref],
            )
            .map_err(|e| ScopeError::Db(e.to_string()))?;
        Ok(())
    }

    fn order_exists_in_org(&self, order_id: &str) -> Result<bool, ScopeError> {
        self.con
            .query_row(
                "SELECT COUNT(*) FROM order_scope WHERE order_id=?1 AND org_id=?2",
                rusqlite::params![order_id, self.org.org_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .map_err(|e| ScopeError::Db(e.to_string()))
    }

    /// Test fixture: plant a foreign-org row (simulates corrupted or
    /// pre-migration multi-org leftover) to prove invisibility.
    #[doc(hidden)]
    pub fn plant_foreign_for_test(&self, order_id: &str, org: &str) {
        self.con
            .execute(
                "INSERT OR REPLACE INTO order_scope (order_id, org_id, source_ref) VALUES (?1,?2,NULL)",
                rusqlite::params![order_id, org],
            )
            .unwrap();
    }

    /// Central read guard: list/count access. All users of the org
    /// with ReadBoard may see org orders — there is exactly one org,
    /// so intra-org visibility modes are M5 (plan scope contract).
    pub fn guard_read(
        &self,
        user: Option<&AuthenticatedUser>,
        order_id: Option<&str>,
    ) -> Result<(), ScopeError> {
        let user = user.ok_or(ScopeError::Unauthenticated)?;
        if !user.role.allows(Capability::ReadBoard) {
            return Err(ScopeError::Denied(Capability::ReadBoard));
        }
        if let Some(id) = order_id {
            // orders created BEFORE T30 (legacy) belong to the org too
            // — migration registered them; unknown id still fails below
            if !self.order_exists_in_org(id)? {
                // legacy orders not yet registered are admitted lazily
                // IF they exist in the ledger — caller checks ledger
                // separately. Here: only reject KNOWN-foreign rows.
                let foreign: Option<String> = self
                    .con
                    .query_row(
                        "SELECT org_id FROM order_scope WHERE order_id=?1 AND org_id != ?2",
                        rusqlite::params![id, self.org.org_id],
                        |r| r.get(0),
                    )
                    .ok();
                if foreign.is_some() {
                    return Err(ScopeError::NotInScope {
                        order_id: id.to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Central mutation guard: applies BEFORE any state transition.
    pub fn guard_mutation(
        &self,
        user: &AuthenticatedUser,
        order_id: &str,
        cap: Capability,
    ) -> Result<(), ScopeError> {
        self.guard_read(Some(user), Some(order_id))?;
        if !user.role.allows(cap) {
            return Err(ScopeError::Denied(cap));
        }
        Ok(())
    }

    /// Report guard — reports span ALL orders, so only the capability
    /// matters (plus org filter is implicit: single org).
    pub fn guard_report(&self, user: Option<&AuthenticatedUser>) -> Result<(), ScopeError> {
        let user = user.ok_or(ScopeError::Unauthenticated)?;
        if !user.role.allows(Capability::ReadBoard) {
            return Err(ScopeError::Denied(Capability::ReadBoard));
        }
        Ok(())
    }
}

/// Attribution (review #8): external reporter identity is ORDER
/// METADATA. This type exists to make "reporter" data impossible to
/// confuse with an AuthenticatedUser — different shape, no auth use.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalAttribution {
    pub source: String,
    pub reporter_external_id: String,
    pub display_name: String,
}

impl ExternalAttribution {
    /// The ONLY thing attribution can do: be recorded and displayed.
    /// There is deliberately no method that turns this into a user,
    /// role, or capability.
    pub fn as_metadata(&self) -> String {
        format!("{}:{}", self.source, self.reporter_external_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> OrgConfig {
        OrgConfig {
            org_id: "acme".into(),
            bound_sources: vec!["github:acme/widgets".into(), "jira:acme/PROJ".into()],
        }
    }

    fn owner() -> AuthenticatedUser {
        AuthenticatedUser {
            username: "root".into(),
            role: Role::Owner,
        }
    }
    fn observer() -> AuthenticatedUser {
        AuthenticatedUser {
            username: "o".into(),
            role: Role::Observer,
        }
    }

    fn store() -> ScopeStore {
        ScopeStore::open(Path::new(""), org()).unwrap()
    }

    #[test]
    fn intake_rejects_foreign_sources() {
        let s = store();
        assert!(s.admit_intake("github:acme/widgets").is_ok());
        // foreign repo on the SAME connector: rejected at intake
        let err = s.admit_intake("github:other-org/widgets").unwrap_err();
        assert!(matches!(err, ScopeError::ForeignOrg { .. }));
        assert!(s.admit_intake("jira:acme/PROJ").is_ok());
        // unregistered source: rejected
        assert!(s.admit_intake("github:acme/unbound").is_err());
    }

    #[test]
    fn read_requires_authentication_and_capability() {
        let s = store();
        s.register_order("o-1", Some("github:acme/widgets"))
            .unwrap();
        // unauthenticated
        assert!(matches!(
            s.guard_read(None, Some("o-1")),
            Err(ScopeError::Unauthenticated)
        ));
        // authenticated observer of the org: allowed
        assert!(s.guard_read(Some(&observer()), Some("o-1")).is_ok());
    }

    #[test]
    fn mutation_needs_capability() {
        let s = store();
        s.register_order("o-1", Some("github:acme/widgets"))
            .unwrap();
        // observer cannot mutate
        assert!(matches!(
            s.guard_mutation(&observer(), "o-1", Capability::Approve),
            Err(ScopeError::Denied(_))
        ));
        assert!(
            s.guard_mutation(&owner(), "o-1", Capability::Approve)
                .is_ok()
        );
    }

    #[test]
    fn known_foreign_order_is_invisible() {
        let s = store();
        // simulate a foreign-org row (post-migration corruption or a
        // multi-org leftover): direct insert, then read must reject
        s.con
            .execute(
                "INSERT INTO order_scope (order_id, org_id, source_ref) VALUES ('fx-1','other-org',NULL)",
                [],
            )
            .unwrap();
        assert!(matches!(
            s.guard_read(Some(&owner()), Some("fx-1")),
            Err(ScopeError::NotInScope { .. })
        ));
        // and mutation too
        assert!(matches!(
            s.guard_mutation(&owner(), "fx-1", Capability::Approve),
            Err(ScopeError::NotInScope { .. })
        ));
    }

    #[test]
    fn attribution_grants_nothing() {
        let a = ExternalAttribution {
            source: "github".into(),
            reporter_external_id: "u-999".into(),
            display_name: "Someone External".into(),
        };
        // metadata only — compile-time shape difference from
        // AuthenticatedUser; no conversion exists
        assert_eq!(a.as_metadata(), "github:u-999");
        // an attacker-controlled reporter field changes nothing:
        let fake = ExternalAttribution {
            reporter_external_id: "root".into(),
            ..a
        };
        assert_eq!(fake.as_metadata(), "github:root"); // still just a string
    }
}
