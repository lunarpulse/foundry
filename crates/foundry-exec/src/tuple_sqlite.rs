use crate::{
    ports::{PortError, Recorded, TupleStore},
    types::{ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, OrderId},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, sync::Mutex};

/// SQLite-backed durable tuple and promotion store. SQLite file locks serialize
/// independent processes; each mutation uses one IMMEDIATE transaction.
pub struct SqliteTupleStore {
    connection: Mutex<Connection>,
}

impl SqliteTupleStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PortError> {
        let conn = Connection::open(path).map_err(store_error)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(store_error)?;
        conn.pragma_update(None, "synchronous", "FULL").map_err(store_error)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS artifact_tuples (
                order_id TEXT NOT NULL, attempt INTEGER NOT NULL, run_id INTEGER NOT NULL,
                generation INTEGER NOT NULL, immutable_commit TEXT NOT NULL,
                artifact_location TEXT NOT NULL,
                PRIMARY KEY(order_id, attempt, run_id, generation));
             CREATE TABLE IF NOT EXISTS tuple_promotions (
                order_id TEXT NOT NULL, attempt INTEGER NOT NULL, run_id INTEGER NOT NULL,
                generation INTEGER NOT NULL, receipt_json TEXT NOT NULL,
                PRIMARY KEY(order_id, attempt, run_id, generation));"
        ).map_err(store_error)?;
        Ok(Self { connection: Mutex::new(conn) })
    }
}

impl TupleStore for SqliteTupleStore {
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, PortError> {
        let mut conn = self.connection.lock().map_err(|e| store_error(e.to_string()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let existing = tx.query_row(
            "SELECT immutable_commit, artifact_location FROM artifact_tuples
             WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",
            params![tuple.order.0, tuple.attempt.0, tuple.run.0, tuple.generation.0],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional().map_err(store_error)?;
        match existing {
            Some((commit, location)) if commit == tuple.immutable_commit.0 && location == tuple.artifact_location.0 => {},
            Some(_) => return Err(PortError::Permanent("tuple key collision".into())),
            None => { tx.execute(
                "INSERT INTO artifact_tuples VALUES (?1,?2,?3,?4,?5,?6)",
                params![tuple.order.0, tuple.attempt.0, tuple.run.0, tuple.generation.0,
                    tuple.immutable_commit.0, tuple.artifact_location.0],
            ).map_err(store_error)?; }
        }
        tx.commit().map_err(store_error)?;
        Ok(Recorded)
    }

    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError> {
        let mut conn = self.connection.lock().map_err(|e| store_error(e.to_string()))?;
        let tx = conn.transaction().map_err(store_error)?;
        let tuple = tx.query_row(
            "SELECT run_id, generation, immutable_commit, artifact_location FROM artifact_tuples
             WHERE order_id=?1 AND attempt=?2 ORDER BY run_id DESC, generation DESC LIMIT 1",
            params![order.0, attempt.0],
            |row| Ok(ArtifactTuple {
                order: order.clone(), attempt, run: crate::types::RunId(row.get(0)?),
                generation: crate::types::Generation(row.get(1)?),
                immutable_commit: crate::types::ImmutableCommit(row.get(2)?),
                artifact_location: crate::types::ArtifactLocation(row.get(3)?),
            }),
        ).optional().map_err(store_error)?;
        tx.commit().map_err(store_error)?;
        Ok(tuple)
    }

    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError> {
        let mut conn = self.connection.lock().map_err(|e| store_error(e.to_string()))?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(store_error)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifact_tuples WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4)",
            params![key.order.0, key.attempt.0, key.run.0, key.generation.0], |row| row.get(0),
        ).map_err(store_error)?;
        if !exists || receipt.key != *key { return Err(PortError::MissingEmitReceipt); }
        let encoded = serde_json::to_string(receipt).map_err(store_error)?;
        let old: Option<String> = tx.query_row(
            "SELECT receipt_json FROM tuple_promotions WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND generation=?4",
            params![key.order.0, key.attempt.0, key.run.0, key.generation.0], |row| row.get(0),
        ).optional().map_err(store_error)?;
        match old {
            Some(old) if old != encoded => return Err(PortError::Permanent("promotion receipt conflict".into())),
            Some(_) => {},
            None => { tx.execute(
                "INSERT INTO tuple_promotions VALUES (?1,?2,?3,?4,?5)",
                params![key.order.0, key.attempt.0, key.run.0, key.generation.0, encoded],
            ).map_err(store_error)?; }
        }
        tx.commit().map_err(store_error)
    }
}

fn store_error(error: impl std::fmt::Display) -> PortError { PortError::Store(error.to_string()) }
