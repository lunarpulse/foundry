use foundry_exec::{
    ArtifactLocation, ArtifactTuple, Attempt, Generation, ImmutableCommit, OrderId, RunId,
};
use foundry_ledger::{ProductConfig, ProductStore};
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn scratch() -> std::path::PathBuf {
    let path = std::env::var_os("TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "foundry-b1-result-ledger-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
fn config() -> ProductConfig {
    ProductConfig {
        installation_id: "result-ledger-install".into(),
        database_id: "result-ledger-db".into(),
    }
}

#[test]
fn fresh_v3_is_sealed_and_read_only_validation_does_not_repair() {
    let root = scratch();
    let db = root.join("product.sqlite");
    drop(ProductStore::open(&db, config()).unwrap());
    let conn = Connection::open(&db).unwrap();
    let schema: i64 = conn
        .query_row(
            "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let (version,cutoff):(i64,i64)=conn.query_row("SELECT result_contract_version,legacy_approval_cutoff_seq FROM product_result_meta_v1 WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let (hosts,tuples):(i64,i64)=conn.query_row("SELECT (SELECT count(*) FROM product_result_legacy_hosts_v1),(SELECT count(*) FROM product_result_legacy_tuples_v1)",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((schema, version, cutoff, hosts, tuples), (3, 1, 0, 0, 0));
    ProductStore::validate_product_results_v3_read_only(&db, &config()).unwrap();
    let result_tables:i64=conn.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name LIKE 'product_result_%_v1'",[],|r|r.get(0)).unwrap();
    assert!(result_tables >= 4);
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn explicit_v2_migration_censuses_history_and_is_idempotent() {
    let root = scratch();
    let db = root.join("product.sqlite");
    drop(ProductStore::open(&db, config()).unwrap());
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch("DROP TRIGGER product_result_meta_no_update; DROP TRIGGER product_result_meta_no_delete; DROP TRIGGER product_result_meta_no_replace; DROP TRIGGER product_result_legacy_hosts_no_insert; DROP TRIGGER product_result_legacy_hosts_no_update; DROP TRIGGER product_result_legacy_hosts_no_delete; DROP TRIGGER product_result_legacy_tuples_no_insert; DROP TRIGGER product_result_legacy_tuples_no_update; DROP TRIGGER product_result_legacy_tuples_no_delete; DROP TRIGGER product_review_authority_no_update; DROP TRIGGER product_review_authority_no_delete; DROP TRIGGER product_review_authority_no_replace; DROP TRIGGER product_review_authority_no_legacy; DROP TRIGGER product_result_bindings_no_update; DROP TRIGGER product_result_bindings_no_delete; DROP TRIGGER product_result_bindings_no_replace; DROP TRIGGER product_result_bindings_no_legacy_tuple; DROP TRIGGER product_approval_result_bindings_no_update; DROP TRIGGER product_approval_result_bindings_no_delete; DROP TRIGGER product_approval_result_bindings_no_replace; DROP TRIGGER product_tuples_no_update; DROP TRIGGER product_tuples_no_delete; DROP TRIGGER product_tuples_no_replace; DROP TABLE product_approval_result_bindings_v1; DROP TABLE product_result_bindings_v1; DROP TABLE product_review_authority_v1; DROP TABLE product_result_legacy_hosts_v1; DROP TABLE product_result_legacy_tuples_v1; DROP TABLE product_result_meta_v1; UPDATE product_meta_v1 SET schema_version=2;").unwrap();
    conn.execute("INSERT INTO artifact_tuples VALUES('legacy-order',2,8,0,?1,'/old/evidence.log','[{\"generation\":0,\"commit\":\"cccccccccccccccccccccccccccccccccccccccc\",\"fix_log_path\":\"\"}]')",["a".repeat(40)]).unwrap();
    let baseline: String = conn
        .query_row(
            "SELECT generations_json FROM artifact_tuples WHERE order_id='legacy-order'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(conn);
    assert!(
        ProductStore::open(&db, config()).is_err(),
        "normal open refuses v2 before migration"
    );
    ProductStore::migrate_product_results_v2(&db, &config()).unwrap();
    ProductStore::migrate_product_results_v2(&db, &config()).unwrap();
    let conn = Connection::open(&db).unwrap();
    let (schema,host_count,tuple_count,cutoff):(i64,i64,i64,i64)=conn.query_row("SELECT (SELECT schema_version FROM product_meta_v1),(SELECT count(*) FROM product_result_legacy_hosts_v1),(SELECT count(*) FROM product_result_legacy_tuples_v1),(SELECT legacy_approval_cutoff_seq FROM product_result_meta_v1)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    let saved: String = conn
        .query_row(
            "SELECT generations_json FROM artifact_tuples WHERE order_id='legacy-order'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!((schema, host_count, tuple_count, cutoff), (3, 0, 1, 0));
    assert_eq!(saved, baseline);
    drop(conn);
    ProductStore::validate_product_results_v3_read_only(&db, &config()).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn v2_migration_cas_and_missing_table_failures_leave_zero_partial_state() {
    // Gate3 rubric T3 cell 1 (Discord 1563206386211496081): mechanics-only
    // v3->v2 downgrade fixture; a CAS miss must roll back every intermediate
    // write, and a missing required table must reject before any write.
    let root = scratch();
    let db = root.join("product.sqlite");
    drop(ProductStore::open(&db, config()).unwrap());
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TRIGGER product_result_meta_no_update; DROP TRIGGER product_result_meta_no_delete; DROP TRIGGER product_result_meta_no_replace; DROP TRIGGER product_result_legacy_hosts_no_insert; DROP TRIGGER product_result_legacy_hosts_no_update; DROP TRIGGER product_result_legacy_hosts_no_delete; DROP TRIGGER product_result_legacy_tuples_no_insert; DROP TRIGGER product_result_legacy_tuples_no_update; DROP TRIGGER product_result_legacy_tuples_no_delete; DROP TRIGGER product_review_authority_no_update; DROP TRIGGER product_review_authority_no_delete; DROP TRIGGER product_review_authority_no_replace; DROP TRIGGER product_review_authority_no_legacy; DROP TRIGGER product_result_bindings_no_update; DROP TRIGGER product_result_bindings_no_delete; DROP TRIGGER product_result_bindings_no_replace; DROP TRIGGER product_result_bindings_no_legacy_tuple; DROP TRIGGER product_approval_result_bindings_no_update; DROP TRIGGER product_approval_result_bindings_no_delete; DROP TRIGGER product_approval_result_bindings_no_replace; DROP TRIGGER product_tuples_no_update; DROP TRIGGER product_tuples_no_delete; DROP TRIGGER product_tuples_no_replace; DROP TABLE product_approval_result_bindings_v1; DROP TABLE product_result_bindings_v1; DROP TABLE product_review_authority_v1; DROP TABLE product_result_legacy_hosts_v1; DROP TABLE product_result_legacy_tuples_v1; DROP TABLE product_result_meta_v1; UPDATE product_meta_v1 SET schema_version=2;").unwrap();
    }
    // sol round105 gap 3: setting schema_version=1 only trips the entry guard
    // (result.rs:553) before any write. Instead a trigger aborts the final CAS
    // UPDATE (schema_version 2->3) itself: DDL, population, census sealing,
    // and the cutoff insert have all executed when the CAS statement fails,
    // so zero-partial-state now measures rollback of real intermediate
    // writes. (The version row is locked by the migration's own transaction,
    // so a mid-flight changed!=1 is structurally unreachable; aborting the
    // UPDATE measures the same rollback obligation at the exact CAS site.)
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER test_abort_v2_to_v3_cas BEFORE UPDATE ON product_meta_v1\n             WHEN NEW.schema_version=3\n             BEGIN SELECT RAISE(ABORT,'injected v2->v3 cas failure'); END;",
        )
        .unwrap();
    assert!(ProductStore::migrate_product_results_v2(&db, &config()).is_err());
    {
        let conn = Connection::open(&db).unwrap();
        let schema: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let partial: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('product_result_meta_v1','product_result_legacy_hosts_v1','product_result_legacy_tuples_v1','product_result_bindings_v1','product_approval_result_bindings_v1','product_review_authority_v1')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let sealed: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'product_result_legacy_%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            (schema, partial, sealed),
            (2, 0, 0),
            "CAS failure after full DDL/population/cutoff must roll back every intermediate migration write"
        );
        conn.execute_batch("DROP TRIGGER test_abort_v2_to_v3_cas;")
            .unwrap();
        // Missing required table at v2: rejected before any migration write.
        conn.execute("DROP TABLE product_outcomes_v1", []).unwrap();
    }
    assert!(ProductStore::migrate_product_results_v2(&db, &config()).is_err());
    {
        let conn = Connection::open(&db).unwrap();
        let schema: i64 = conn
            .query_row(
                "SELECT schema_version FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let partial: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='product_result_meta_v1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            (schema, partial),
            (2, 0),
            "missing required table must leave zero partial migration state"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn v2_migration_seals_legacy_census_with_exact_rows() {
    // Gate3 rubric T3 cell 2 (Discord 1563206386211496081): the migrated
    // census equals its source rows exactly and all six sealing triggers
    // reject INSERT/UPDATE/DELETE afterwards.
    let root = scratch();
    let db = root.join("product.sqlite");
    drop(ProductStore::open(&db, config()).unwrap());
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TRIGGER product_result_meta_no_update; DROP TRIGGER product_result_meta_no_delete; DROP TRIGGER product_result_meta_no_replace; DROP TRIGGER product_result_legacy_hosts_no_insert; DROP TRIGGER product_result_legacy_hosts_no_update; DROP TRIGGER product_result_legacy_hosts_no_delete; DROP TRIGGER product_result_legacy_tuples_no_insert; DROP TRIGGER product_result_legacy_tuples_no_update; DROP TRIGGER product_result_legacy_tuples_no_delete; DROP TRIGGER product_review_authority_no_update; DROP TRIGGER product_review_authority_no_delete; DROP TRIGGER product_review_authority_no_replace; DROP TRIGGER product_review_authority_no_legacy; DROP TRIGGER product_result_bindings_no_update; DROP TRIGGER product_result_bindings_no_delete; DROP TRIGGER product_result_bindings_no_replace; DROP TRIGGER product_result_bindings_no_legacy_tuple; DROP TRIGGER product_approval_result_bindings_no_update; DROP TRIGGER product_approval_result_bindings_no_delete; DROP TRIGGER product_approval_result_bindings_no_replace; DROP TRIGGER product_tuples_no_update; DROP TRIGGER product_tuples_no_delete; DROP TRIGGER product_tuples_no_replace; DROP TABLE product_approval_result_bindings_v1; DROP TABLE product_result_bindings_v1; DROP TABLE product_review_authority_v1; DROP TABLE product_result_legacy_hosts_v1; DROP TABLE product_result_legacy_tuples_v1; DROP TABLE product_result_meta_v1; UPDATE product_meta_v1 SET schema_version=2;").unwrap();
        conn.execute(
            "INSERT INTO artifact_tuples VALUES('seal-order',1,3,0,?1,'/old/evidence.log','[{\"generation\":0,\"commit\":\"cccccccccccccccccccccccccccccccccccccccc\",\"fix_log_path\":\"\"}]')",
            ["a".repeat(40)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO product_review_host_accept_v1(order_id,attempt,run_id,activation) VALUES('seal-order',1,3,1)",
            [],
        )
        .unwrap();
    }
    ProductStore::migrate_product_results_v2(&db, &config()).unwrap();
    let conn = Connection::open(&db).unwrap();
    let host: (String, i64, i64, i64) = conn
        .query_row(
            "SELECT order_id,attempt,run_id,activation FROM product_result_legacy_hosts_v1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let tuple: (String, i64, i64, i64) = conn
        .query_row(
            "SELECT order_id,attempt,run_id,generation FROM product_result_legacy_tuples_v1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(host, ("seal-order".into(), 1, 3, 1));
    assert_eq!(tuple, ("seal-order".into(), 1, 3, 0));
    for sql in [
        "INSERT INTO product_result_legacy_hosts_v1(order_id,attempt,run_id,activation) VALUES('x',9,9,9)",
        "UPDATE product_result_legacy_hosts_v1 SET activation=2",
        "DELETE FROM product_result_legacy_hosts_v1",
        "INSERT INTO product_result_legacy_tuples_v1(order_id,attempt,run_id,generation) VALUES('x',9,9,9)",
        "UPDATE product_result_legacy_tuples_v1 SET generation=2",
        "DELETE FROM product_result_legacy_tuples_v1",
    ] {
        assert!(
            conn.execute(sql, []).is_err(),
            "sealed census must reject: {sql}"
        );
    }
    drop(conn);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_tuple_digest_uses_nonempty_exec_generation_encoding() {
    let tuple = ArtifactTuple {
        order: OrderId("colon:order".into()),
        attempt: Attempt(1),
        run: RunId(7),
        generation: Generation(0),
        immutable_commit: ImmutableCommit("d".repeat(40)),
        artifact_location: ArtifactLocation("/pinned/review/run/accepted-output".into()),
        generations: vec![
            foundry_exec::GenerationCommit {
                generation: 0,
                commit: "b".repeat(40),
                fix_log_path: String::new(),
            },
            foundry_exec::GenerationCommit {
                generation: 1,
                commit: "c".repeat(40),
                fix_log_path: "run8/exec/resume-1/3-replan_full-g1/0/accepted-output".into(),
            },
        ],
    };
    let digest = tuple.digest();
    assert_eq!(
        digest,
        "f04dc5f176150a9e5a4914baa984836e17557c7484374d1d00da069251b2935b"
    );
    let mut changed = tuple.clone();
    changed.generations[1].fix_log_path.push_str("/changed");
    assert_ne!(digest, changed.digest());
    let mut changed_location = tuple.clone();
    changed_location.artifact_location.0.push_str("/changed");
    assert_ne!(digest, changed_location.digest());
    assert_eq!(tuple.key().order.0, "colon:order");
    assert_eq!(tuple.generations.len(), 2);
}

#[test]
fn single_generation_digest_uses_exec_struct_bytes_not_historical_value_bytes() {
    let tuple = ArtifactTuple {
        order: OrderId("colon:order".into()),
        attempt: Attempt(1),
        run: RunId(7),
        generation: Generation(0),
        immutable_commit: ImmutableCommit("d".repeat(40)),
        artifact_location: ArtifactLocation("/pinned/review/run/accepted-output".into()),
        generations: vec![foundry_exec::GenerationCommit {
            generation: 0,
            commit: "b".repeat(40),
            fix_log_path: String::new(),
        }],
    };
    let exec_bytes = serde_json::to_vec(&tuple.generations).unwrap();
    assert_eq!(
        std::str::from_utf8(&exec_bytes).unwrap(),
        "[{\"generation\":0,\"commit\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"fix_log_path\":\"\"}]"
    );
    assert_eq!(
        tuple.digest(),
        "550b7bd21f50071c8fd97a81be49650d8ddece72a60b50e3f93131334116e0f9"
    );
    let historical_value_bytes =
        serde_json::to_vec(&serde_json::to_value(&tuple.generations).unwrap()).unwrap();
    assert_eq!(
        std::str::from_utf8(&historical_value_bytes).unwrap(),
        "[{\"commit\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",\"fix_log_path\":\"\",\"generation\":0}]"
    );
    assert_ne!(exec_bytes, historical_value_bytes);
}
