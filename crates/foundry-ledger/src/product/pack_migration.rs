//! Connection fencing for private, explicitly migrated W1 stores.
use super::ProductError;
#[cfg(feature = "p2-test-support")]
use super::{ProductStore, StoreOpenMode, initialize_product_pack_pin_schema, result};
#[cfg(feature = "p2-test-support")]
use rusqlite::TransactionBehavior;
use rusqlite::functions::FunctionFlags;
use rusqlite::{Connection, OptionalExtension};

const MARKER: &str = "CREATE TABLE product_pack_migration_v1(singleton INTEGER PRIMARY KEY CHECK(singleton=1),from_version INTEGER NOT NULL CHECK(from_version=3),to_version INTEGER NOT NULL CHECK(to_version=4))";
const FENCES: &[(&str, &str)] = &[
    ("run", "INSERT ON product_review_runs_v1"),
    ("decision", "INSERT ON product_review_hold_acceptances_v1"),
    (
        "claim",
        "UPDATE OF owner_id ON product_review_hold_progress_v1",
    ),
    ("budget", "INSERT ON product_review_budget_v1"),
    ("stage", "INSERT ON product_review_stages_v1"),
];

fn trigger(name: &str, event: &str) -> String {
    format!(
        "CREATE TRIGGER product_pack_writer_{name} BEFORE {event} WHEN foundry_pack_writer_version() IS NOT 4 BEGIN SELECT RAISE(ABORT,'incompatible pack writer'); END"
    )
}

fn is_migrated(db: &Connection) -> Result<bool, ProductError> {
    let count: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_schema
         WHERE name='product_pack_migration_v1' OR name GLOB 'product_pack_writer_*'",
        [],
        |row| row.get(0),
    )?;
    if count == 0 {
        return Ok(false);
    }
    let marker: Option<String> = db
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type='table' AND name='product_pack_migration_v1'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if count != 1 + FENCES.len() as i64 || marker.as_deref() != Some(MARKER) {
        return Err(ProductError::ContractMismatch);
    }
    let valid: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_pack_migration_v1
                       WHERE singleton=1 AND from_version=3 AND to_version=4)
         AND (SELECT schema_version FROM product_meta_v1 WHERE singleton=1)=4",
        [],
        |row| row.get(0),
    )?;
    if !valid {
        return Err(ProductError::ContractMismatch);
    }
    for &(name, event) in FENCES {
        let actual: Option<String> = db
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
                [format!("product_pack_writer_{name}")],
                |r| r.get(0),
            )
            .optional()?;
        if actual.as_deref() != Some(trigger(name, event).as_str()) {
            return Err(ProductError::ContractMismatch);
        }
    }
    Ok(true)
}

pub(super) fn configure_migrated_writer(db: &Connection) -> Result<(), ProductError> {
    if is_migrated(db)? {
        db.pragma_update(None, "trusted_schema", false)?;
        db.create_scalar_function(
            "foundry_pack_writer_version",
            0,
            FunctionFlags::SQLITE_UTF8
                | FunctionFlags::SQLITE_INNOCUOUS
                | FunctionFlags::SQLITE_DETERMINISTIC,
            |_| Ok(4_i64),
        )?;
    }
    Ok(())
}

#[cfg(feature = "p2-test-support")]
impl ProductStore {
    /// Exercise cutover only on an operator-owned test copy. A production
    /// rollout additionally needs a quiescence, backup and recovery contract.
    #[doc(hidden)]
    pub fn migrate_pack_pin_test_copy(
        &mut self,
        before_commit: impl FnOnce(),
    ) -> Result<(), ProductError> {
        if self.open_mode != StoreOpenMode::Normal {
            return Err(ProductError::ContractMismatch);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (installation, database, version): (String, String, i64) = tx.query_row(
            "SELECT installation_id,database_id,schema_version FROM product_meta_v1 WHERE singleton=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if installation != self.installation_id || database != self.database_id {
            return Err(ProductError::ContractMismatch);
        }
        if !is_migrated(&tx)? {
            if version != 3 {
                return Err(ProductError::ContractMismatch);
            }
            result::validate_v3_connection(&tx, &installation, &database)?;
            initialize_product_pack_pin_schema(&tx)?;
            tx.execute_batch(MARKER)?;
            tx.execute("INSERT INTO product_pack_migration_v1 VALUES(1,3,4)", [])?;
            for &(name, event) in FENCES {
                tx.execute_batch(&trigger(name, event))?;
            }
            let changed = tx.execute(
                "UPDATE product_meta_v1 SET schema_version=4 WHERE singleton=1 AND schema_version=3",
                [],
            )?;
            if changed != 1 {
                return Err(ProductError::ContractMismatch);
            }
            if !is_migrated(&tx)? {
                return Err(ProductError::ContractMismatch);
            }
            before_commit();
        }
        tx.commit()?;
        configure_migrated_writer(&self.conn)
    }
}
