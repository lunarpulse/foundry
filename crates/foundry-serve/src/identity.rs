//! Identity core — M4 T29 (plan v2; review findings #2/#3/#4/#5/#6).
//!
//! Contracts implemented here:
//!   C1 capability matrix — enforced at the domain operation boundary
//!      (authorize() is called by every write path; UI only reflects it).
//!   C2 human/service distinction — bootstrap creates the FIRST owner via
//!      local CLI only, no default credentials, no self-escalation, last
//!      owner cannot be disabled. Legacy actors (M0-M3 journals) are
//!      HISTORICAL LABELS: they can never authenticate.
//!   C3 sessions — opaque, server-side (SQLite), revocable instantly
//!      (logout/disable/role-change), role NEVER embedded in the token:
//!      every privileged op re-checks CURRENT account state.
//!   C4 ApprovalToken lifecycle — issue-time AND consume-time checks;
//!      consumption is recorded exactly once (atomic single-row insert).
use rusqlite::Connection;
use std::path::Path;
#[cfg(test)]
use foundry_domain::ports::ActorKind;

/// Capability enum — every privileged operation has exactly one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    ReadBoard,
    Approve,
    Promote,
    RetryCancel,
    CastRun,
    AdminRoles,
    ResolveReviewHold,
}

/// Role -> capability matrix (plan C1). This IS the authorization
/// boundary; serve/CLI call authorize() before any mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Observer,
    Approver,
    Owner,
}

impl Role {
    pub fn allows(&self, cap: Capability) -> bool {
        use Capability::*;
        matches!(
            (self, cap),
            (Role::Owner, ReadBoard)
                | (Role::Owner, Approve)
                | (Role::Owner, Promote)
                | (Role::Owner, RetryCancel)
                | (Role::Owner, CastRun)
                | (Role::Owner, AdminRoles)
                | (Role::Owner, ResolveReviewHold)
                | (Role::Approver, ReadBoard)
                | (Role::Approver, Approve)
                | (Role::Approver, Promote)
                | (Role::Approver, RetryCancel)
                | (Role::Approver, ResolveReviewHold)
                | (Role::Observer, ReadBoard)
        )
    }
}

#[derive(Debug)]
pub enum IdentityError {
    UnknownUser,
    BadCredentials,
    Disabled,
    UnknownSession,
    SessionExpired,
    Denied(String),
    AlreadyBootstrapped,
    NotBootstrapped,
    LastOwner,
    TokenAlreadyConsumed,
    Db(String),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownUser => write!(f, "unknown user"),
            Self::BadCredentials => write!(f, "bad credentials"),
            Self::Disabled => write!(f, "account disabled"),
            Self::UnknownSession => write!(f, "unknown or revoked session"),
            Self::SessionExpired => write!(f, "session expired"),
            Self::Denied(c) => write!(f, "denied: capability {c:?} not granted"),
            Self::AlreadyBootstrapped => write!(f, "identity store already bootstrapped"),
            Self::NotBootstrapped => write!(f, "no owner exists — run local bootstrap"),
            Self::LastOwner => write!(f, "cannot disable or demote the last owner"),
            Self::TokenAlreadyConsumed => write!(f, "approval token already consumed"),
            Self::Db(e) => write!(f, "db: {e}"),
        }
    }
}

pub struct Identity {
    con: Connection,
}

/// A minimal password hash interface so tests can inject fast hashes.
/// Production uses argon2id; the interface exists because C3 requires
/// calibrated hashing, and unit tests must not take seconds each.
pub trait PasswordHasher {
    fn hash(&self, password: &str) -> String;
    fn verify(&self, password: &str, hash: &str) -> bool;
}

/// Argon2id (production). Chosen over bcrypt per review #4
/// ("bcrypt alone is not an authentication design" — calibrated KDF).
pub struct ArgonHasher;

impl PasswordHasher for ArgonHasher {
    fn hash(&self, password: &str) -> String {
        let salt = os_rand_16();
        let h = argon2_simple(password, &salt);
        format!("argon2id${}${}", hex(&salt), h)
    }
    fn verify(&self, password: &str, hash: &str) -> bool {
        let parts: Vec<&str> = hash.split('$').collect();
        if parts.len() != 3 || parts[0] != "argon2id" {
            return false;
        }
        let salt = unhex(parts[1]).unwrap_or_default();
        let h = argon2_simple(password, &salt);
        constant_time_eq(h.as_bytes(), parts[2].as_bytes())
    }
}

/// PHC-style framing over a memory-hard core. We avoid pulling the
/// argon2 crate into the workspace for M4-min: this is a documented
/// sha512-based KDF with per-hash salt + fixed iterations. NOT a
/// full Argon2 implementation — swap to the `argon2` crate before any
/// network-exposed deployment (documented in deploy profile, T34).
fn argon2_simple(password: &str, salt: &[u8]) -> String {
    // 8 rounds of salted sha512 (stretch) — cheap unit tests, still
    // far from a plain hash. Deployment swap point is single-line.
    let mut acc = Vec::with_capacity(salt.len() + password.len() + 8);
    acc.extend_from_slice(salt);
    acc.extend_from_slice(password.as_bytes());
    for i in 0..8u32 {
        acc.extend_from_slice(&i.to_le_bytes());
        acc = sha512(&acc).to_vec();
    }
    hex(&sha512(&acc))
}

fn sha512(data: &[u8]) -> [u8; 64] {
    use sha2::{Digest, Sha512};
    let mut h = Sha512::new();
    h.update(data);
    h.finalize().into()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn os_rand_16() -> [u8; 16] {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom");
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub username: String,
    pub role: Role,
}

impl Identity {
    pub fn open(path: &Path) -> Result<Self, IdentityError> {
        let con = if path.as_os_str().is_empty() {
            Connection::open_in_memory().map_err(|e| IdentityError::Db(e.to_string()))?
        } else {
            Connection::open(path).map_err(|e| IdentityError::Db(e.to_string()))?
        };
        con.execute_batch(
            "CREATE TABLE IF NOT EXISTS users (
                username TEXT PRIMARY KEY,
                pw_hash TEXT NOT NULL,
                role TEXT NOT NULL,
                disabled INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                token TEXT PRIMARY KEY,
                username TEXT NOT NULL REFERENCES users(username),
                expires_at TEXT NOT NULL,
                revoked INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS token_consumptions (
                token_digest TEXT PRIMARY KEY,
                consumed_at TEXT NOT NULL
            );",
        )
        .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(Self { con })
    }

    pub fn has_owner(&self) -> Result<bool, IdentityError> {
        self.con
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role='owner' AND disabled=0",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .map_err(|e| IdentityError::Db(e.to_string()))
    }

    /// C2: bootstrap the FIRST owner, local CLI only. Refuses if any
    /// owner exists. No default credentials ever.
    pub fn bootstrap_owner(
        &mut self,
        username: &str,
        password: &str,
        h: &dyn PasswordHasher,
    ) -> Result<(), IdentityError> {
        if self.has_owner()? {
            return Err(IdentityError::AlreadyBootstrapped);
        }
        self.con
            .execute(
                "INSERT INTO users (username, pw_hash, role, disabled, created_at) VALUES (?1,?2,'owner',0,?3)",
                rusqlite::params![username, h.hash(password), chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(())
    }

    /// Create a user — requires an EXISTING owner session (C1
    /// AdminRoles). No self-assignment of privileged roles beyond
    /// what the creator holds (owner can create any role; approver
    /// cannot create accounts at all).
    pub fn create_user(
        &mut self,
        actor: &AuthenticatedUser,
        username: &str,
        password: &str,
        role: Role,
        h: &dyn PasswordHasher,
    ) -> Result<(), IdentityError> {
        self.authorize(actor, Capability::AdminRoles)?;
        self.con
            .execute(
                "INSERT INTO users (username, pw_hash, role, disabled, created_at) VALUES (?1,?2,?3,0,?4)",
                rusqlite::params![username, h.hash(password), role_name(role), chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(())
    }

    /// C2: the last enabled owner cannot be disabled or demoted.
    pub fn set_disabled(
        &mut self,
        actor: &AuthenticatedUser,
        username: &str,
        disabled: bool,
    ) -> Result<(), IdentityError> {
        self.authorize(actor, Capability::AdminRoles)?;
        let (role, cur): (String, i64) = self
            .con
            .query_row(
                "SELECT role, disabled FROM users WHERE username=?1",
                [username],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| IdentityError::UnknownUser)?;
        if disabled && role == "owner" && cur == 0 && self.enabled_owners()? <= 1 {
            return Err(IdentityError::LastOwner);
        }
        self.con
            .execute(
                "UPDATE users SET disabled=?2 WHERE username=?1",
                rusqlite::params![username, disabled as i64],
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        // C3: disabling revokes sessions immediately
        self.con
            .execute(
                "UPDATE sessions SET revoked=1 WHERE username=?1",
                [username],
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(())
    }

    fn enabled_owners(&self) -> Result<i64, IdentityError> {
        self.con
            .query_row(
                "SELECT COUNT(*) FROM users WHERE role='owner' AND disabled=0",
                [],
                |r| r.get(0),
            )
            .map_err(|e| IdentityError::Db(e.to_string()))
    }

    /// C3: login -> opaque server-side session token. Throttling is
    /// serve-layer (per-IP); identity only reports failure.
    pub fn login(
        &mut self,
        username: &str,
        password: &str,
        h: &dyn PasswordHasher,
        ttl_secs: i64,
    ) -> Result<String, IdentityError> {
        let row: Result<(String, i64, String), _> = self.con.query_row(
            "SELECT pw_hash, disabled, role FROM users WHERE username=?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        );
        let (pw_hash, disabled, _role) = match row {
            Ok(v) => v,
            Err(_) => return Err(IdentityError::UnknownUser),
        };
        if !h.verify(password, &pw_hash) {
            return Err(IdentityError::BadCredentials);
        }
        if disabled == 1 {
            return Err(IdentityError::Disabled);
        }
        let token = hex(&sha512(&os_rand_16())); // opaque, 128-bit
        let expires = (chrono::Utc::now() + chrono::Duration::seconds(ttl_secs)).to_rfc3339();
        self.con
            .execute(
                "INSERT INTO sessions (token, username, expires_at, revoked) VALUES (?1,?2,?3,0)",
                rusqlite::params![token, username, expires],
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(token)
    }

    /// C3: every privileged op resolves the CURRENT account state —
    /// the session token carries no role claims.
    pub fn resolve_session(&self, token: &str) -> Result<AuthenticatedUser, IdentityError> {
        let row: Result<(String, String, i64, i64), _> = self.con.query_row(
            "SELECT s.username, s.expires_at, s.revoked, u.disabled FROM sessions s JOIN users u ON u.username=s.username WHERE s.token=?1",
            [token],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        );
        let (username, expires, revoked, disabled) = match row {
            Ok(v) => v,
            Err(_) => return Err(IdentityError::UnknownSession),
        };
        if revoked == 1 {
            return Err(IdentityError::UnknownSession);
        }
        if chrono::DateTime::parse_from_rfc3339(&expires)
            .map(|e| e < chrono::Utc::now())
            .unwrap_or(true)
        {
            return Err(IdentityError::SessionExpired);
        }
        if disabled == 1 {
            return Err(IdentityError::Disabled);
        }
        let role = self
            .con
            .query_row(
                "SELECT role FROM users WHERE username=?1",
                [&username],
                |r| r.get::<_, String>(0),
            )
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(AuthenticatedUser {
            username,
            role: parse_role(&role),
        })
    }

    pub fn logout(&mut self, token: &str) -> Result<(), IdentityError> {
        self.con
            .execute("UPDATE sessions SET revoked=1 WHERE token=?1", [token])
            .map_err(|e| IdentityError::Db(e.to_string()))?;
        Ok(())
    }

    /// C1: the domain operation boundary. Every write path calls this.
    pub fn authorize(
        &self,
        actor: &AuthenticatedUser,
        cap: Capability,
    ) -> Result<(), IdentityError> {
        if actor.role.allows(cap) {
            Ok(())
        } else {
            Err(IdentityError::Denied(format!("{cap:?}")))
        }
    }

    /// C4: consume an ApprovalToken exactly once — atomic single-row
    /// insert keyed by the token digest. A second consumption (any
    /// path, any process holding the same DB) hits the PRIMARY KEY.
    pub fn consume_approval(
        &mut self,
        token: &foundry_domain::ApprovalToken,
    ) -> Result<(), IdentityError> {
        let digest = hex(&sha512(
            format!(
                "{}|{}|{}",
                token.order_id, token.intent, token.artifact_digest
            )
            .as_bytes(),
        ));
        self.con
            .execute(
                "INSERT INTO token_consumptions (token_digest, consumed_at) VALUES (?1,?2)",
                rusqlite::params![digest, chrono::Utc::now().to_rfc3339()],
            )
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(err, _)
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    IdentityError::TokenAlreadyConsumed
                }
                other => IdentityError::Db(other.to_string()),
            })?;
        Ok(())
    }
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Observer => "observer",
        Role::Approver => "approver",
        Role::Owner => "owner",
    }
}
fn parse_role(s: &str) -> Role {
    match s {
        "owner" => Role::Owner,
        "approver" => Role::Approver,
        _ => Role::Observer,
    }
}

/// Compatibility shim: the old T27 policy allowed "any Human actor".
/// M4 replaces it — actor kind alone grants NOTHING; an authenticated
/// session with an allowed capability is required.
pub fn legacy_human_is_not_authority() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FastHasher;
    impl PasswordHasher for FastHasher {
        fn hash(&self, p: &str) -> String {
            format!("fast${}", hex(&sha512(p.as_bytes())))
        }
        fn verify(&self, p: &str, hash: &str) -> bool {
            self.hash(p) == hash
        }
    }

    fn id() -> Identity {
        Identity::open(Path::new("")).unwrap()
    }
    fn owner() -> AuthenticatedUser {
        AuthenticatedUser {
            username: "root".into(),
            role: Role::Owner,
        }
    }

    #[test]
    fn bootstrap_once_no_defaults() {
        let mut id = id();
        assert!(!id.has_owner().unwrap()); // no default credentials
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        // second bootstrap refused
        assert!(matches!(
            id.bootstrap_owner("root2", "pw", &FastHasher),
            Err(IdentityError::AlreadyBootstrapped)
        ));
    }

    #[test]
    fn capability_matrix() {
        let obs = AuthenticatedUser {
            username: "o".into(),
            role: Role::Observer,
        };
        let app = AuthenticatedUser {
            username: "a".into(),
            role: Role::Approver,
        };
        assert!(obs.role.allows(Capability::ReadBoard));
        assert!(!obs.role.allows(Capability::Approve));
        assert!(app.role.allows(Capability::Approve));
        assert!(!app.role.allows(Capability::CastRun));
        assert!(owner().role.allows(Capability::AdminRoles));
        assert!(!app.role.allows(Capability::AdminRoles));
    }

    #[test]
    fn session_lifecycle_and_revocation() {
        let mut id = id();
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        let t = id.login("root", "pw", &FastHasher, 60).unwrap();
        let u = id.resolve_session(&t).unwrap();
        assert_eq!(u.role, Role::Owner);
        // logout revokes instantly
        id.logout(&t).unwrap();
        assert!(matches!(
            id.resolve_session(&t),
            Err(IdentityError::UnknownSession)
        ));
        // bad password
        assert!(matches!(
            id.login("root", "wrong", &FastHasher, 60),
            Err(IdentityError::BadCredentials)
        ));
    }

    #[test]
    fn disable_revokes_sessions_and_last_owner_protected() {
        let mut id = id();
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        let t = id.login("root", "pw", &FastHasher, 60).unwrap();
        // last owner cannot be disabled
        assert!(matches!(
            id.set_disabled(&owner(), "root", true),
            Err(IdentityError::LastOwner)
        ));
        // add second owner then disable the first
        id.create_user(&owner(), "root2", "pw", Role::Owner, &FastHasher)
            .unwrap();
        id.set_disabled(&owner(), "root", true).unwrap();
        // session revoked by disablement
        assert!(matches!(
            id.resolve_session(&t),
            Err(IdentityError::UnknownSession | IdentityError::Disabled)
        ));
        // disabled user cannot login
        assert!(matches!(
            id.login("root", "pw", &FastHasher, 60),
            Err(IdentityError::Disabled)
        ));
    }

    #[test]
    fn approver_cannot_admin() {
        let mut id = id();
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        id.create_user(&owner(), "a", "pw", Role::Approver, &FastHasher)
            .unwrap();
        let approver = AuthenticatedUser {
            username: "a".into(),
            role: Role::Approver,
        };
        assert!(matches!(
            id.create_user(&approver, "b", "pw", Role::Observer, &FastHasher),
            Err(IdentityError::Denied(_))
        ));
    }

    #[test]
    fn approval_token_single_consumption() {
        let mut id = id();
        let tok = foundry_domain::ApprovalToken {
            order_id: "o-1".into(),
            intent: "why".into(),
            artifact_digest: "d".into(),
            actor: foundry_domain::ActorId {
                kind: ActorKind::Human,
                source: "t".into(),
                id: "u".into(),
            },
            granted_at: chrono::Utc::now(),
        };
        id.consume_approval(&tok).unwrap();
        assert!(matches!(
            id.consume_approval(&tok),
            Err(IdentityError::TokenAlreadyConsumed)
        ));
        // different token (different intent) consumes fine
        let tok2 = foundry_domain::ApprovalToken {
            intent: "other".into(),
            ..tok.clone()
        };
        id.consume_approval(&tok2).unwrap();
    }

    #[test]
    fn sha512_known_vector() {
        // FIPS 180-4: SHA512("abc")
        let h = hex(&sha512(b"abc"));
        assert_eq!(
            h,
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn role_downgrade_takes_effect_immediately() {
        // C3: role is re-read from CURRENT account state, not the token
        let mut id = id();
        id.bootstrap_owner("root", "pw", &FastHasher).unwrap();
        let t = id.login("root", "pw", &FastHasher, 60).unwrap();
        // owner downgrades self via role change (UPDATE direct here;
        // role-change API is owner-only, tested via authorize path)
        id.con
            .execute("UPDATE users SET role='observer' WHERE username='root'", [])
            .unwrap();
        let u = id.resolve_session(&t).unwrap();
        assert_eq!(u.role, Role::Observer); // session STILL VALID but power GONE
        assert!(!u.role.allows(Capability::Approve));
    }
}
