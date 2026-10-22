use foundry_approval::{
    ApprovalLedger,
    product::{
        ProductApprovalAnchor, ProductGrantBinding, finalize_product_in, issue_product_grant_in,
        prepare_product_in,
    },
};

#[test]
fn standalone_open_refuses_product_contract_before_creating_legacy_tables() {
    let dir = std::env::temp_dir().join(format!(
        "foundry-b1-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("product.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE foundry_contract(singleton INTEGER PRIMARY KEY,contract TEXT NOT NULL); INSERT INTO foundry_contract VALUES(1,'ProductAtomicV1'); CREATE TABLE product_meta_v1(singleton INTEGER PRIMARY KEY,installation_id TEXT,database_id TEXT,schema_version INTEGER,recovery_epoch INTEGER);").unwrap();
    drop(conn);
    assert!(ApprovalLedger::open(&path).is_err());
    let check = rusqlite::Connection::open(&path).unwrap();
    let legacy_tables: i64 = check
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='grants'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy_tables, 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn product_participant_requires_exact_s1_anchor_and_shares_transaction() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE product_grants_v1(grant_id TEXT PRIMARY KEY,token_digest TEXT UNIQUE,actor_id TEXT,order_id TEXT,state TEXT,fence INTEGER); CREATE TABLE product_fence_counter_v1(singleton INTEGER PRIMARY KEY,next INTEGER); INSERT INTO product_fence_counter_v1 VALUES(1,0); CREATE TABLE product_token_consumptions_v1(token_digest TEXT PRIMARY KEY,operation_id TEXT UNIQUE,seq INTEGER UNIQUE); CREATE TABLE ledger(seq INTEGER PRIMARY KEY,order_id TEXT,hash TEXT,from_state TEXT,to_state TEXT,event_json TEXT); CREATE TABLE product_transition_receipts_v1(seq INTEGER,event_hash TEXT,order_id TEXT,lifecycle INTEGER,operation TEXT);").unwrap();
    let binding = ProductGrantBinding {
        grant_id: "g1".into(),
        token_digest: "token-digest".into(),
        actor_id: "owner".into(),
        order_id: "order-1".into(),
        operation_id: "op-1".into(),
    };
    let tx = conn.unchecked_transaction().unwrap();
    issue_product_grant_in(&tx, &binding).unwrap();
    let reservation = prepare_product_in(&tx, binding.clone()).unwrap();
    tx.execute("INSERT INTO ledger VALUES(7,'order-1','hash-7','waiting','publishing','{\"type\":\"approved\",\"intent\":\"approve\",\"artifact_digest\":\"artifact-1\"}')", [])
        .unwrap();
    tx.execute("INSERT INTO product_transition_receipts_v1 VALUES(7,'hash-7','order-1',1,'human-approval')", []).unwrap();
    finalize_product_in(
        &tx,
        &reservation,
        &ProductApprovalAnchor {
            sequence: 7,
            event_hash: "hash-7".into(),
            lifecycle: 1,
            artifact_digest: "artifact-1".into(),
        },
    )
    .unwrap();
    tx.commit().unwrap();
    let consumed: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_token_consumptions_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(consumed, 1);

    let tx = conn.unchecked_transaction().unwrap();
    assert!(
        finalize_product_in(
            &tx,
            &reservation,
            &ProductApprovalAnchor {
                sequence: 8,
                event_hash: "wrong".into(),
                lifecycle: 1,
                artifact_digest: "artifact-1".into(),
            }
        )
        .is_err()
    );
    drop(tx);
    let still_one: i64 = conn
        .query_row(
            "SELECT count(*) FROM product_token_consumptions_v1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still_one, 1);
}

#[test]
fn duplicate_legacy_activation_tokens_are_reported_without_deleting_rows() {
    let path = std::env::temp_dir().join(format!(
        "foundry-b1-activation-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE activations(root TEXT NOT NULL,intent TEXT NOT NULL,token_id TEXT NOT NULL,activated_at TEXT NOT NULL,PRIMARY KEY(root,intent)); INSERT INTO activations VALUES('r1','i1','same','t1'); INSERT INTO activations VALUES('r2','i2','same','t2');").unwrap();
    drop(conn);
    assert!(ApprovalLedger::open(&path).is_err());
    let check = rusqlite::Connection::open(&path).unwrap();
    let rows: i64 = check
        .query_row("SELECT count(*) FROM activations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    drop(check);
    std::fs::remove_file(path).unwrap();
}
