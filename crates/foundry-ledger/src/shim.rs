//! FOUNDRY shim — minimal stand-in for maos-audit's crate-root AuditError
//! (original lives in the deferred lib.rs; M0 journal needs the common
//! variants only). Marked per VENDORED.md policy.
use std::fmt;

#[derive(Debug)]
pub enum AuditError {
    Db(String),
    Io(String),
    Open(String),
    Read(String),
    Row(String),
    Integrity(String),
}

impl fmt::Display for AuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuditError::Db(m) => write!(f, "db: {m}"),
            AuditError::Io(m) => write!(f, "io: {m}"),
            AuditError::Open(m) => write!(f, "open: {m}"),
            AuditError::Read(m) => write!(f, "read: {m}"),
            AuditError::Row(m) => write!(f, "row: {m}"),
            AuditError::Integrity(m) => write!(f, "integrity: {m}"),
        }
    }
}

impl std::error::Error for AuditError {}

impl From<rusqlite::Error> for AuditError {
    fn from(e: rusqlite::Error) -> Self {
        AuditError::Db(e.to_string())
    }
}

impl From<std::io::Error> for AuditError {
    fn from(e: std::io::Error) -> Self {
        AuditError::Io(e.to_string())
    }
}

impl From<rusqlite::types::FromSqlConversionFailure> for AuditError {
    fn from(e: rusqlite::types::FromSqlConversionFailure) -> Self {
        AuditError::Row(e.to_string())
    }
}
