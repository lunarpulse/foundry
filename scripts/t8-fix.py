import pathlib

m = pathlib.Path("crates/foundry/src/main.rs")
s = m.read_text()

s = s.replace(
    "async fn approve(order_id: String, intent: String, db: &str) -> i32 {\n    let ledger = match SqliteLedger::open(db) {",
    "async fn approve(order_id: String, intent: String, db: &str) -> i32 {\n    let ledger = match SqliteLedger::open(std::path::Path::new(db)) {",
)
s = s.replace(
    "async fn show(order_id: String, db: &str) -> i32 {\n    match SqliteLedger::open(db) {",
    "async fn show(order_id: String, db: &str) -> i32 {\n    match SqliteLedger::open(std::path::Path::new(db)) {",
)
s = s.replace(
    "let ledger = match SqliteLedger::open(&db_path) {",
    "let ledger = match SqliteLedger::open(std::path::Path::new(&db_path)) {",
)
s = s.replace(
    'eprintln!("✗ {e}");',
    'eprintln!("✗ {e:?}");',
)
s = s.replace(
    'eprintln!("✗ publish: {e}");',
    'eprintln!("✗ publish: {e:?}");',
)
m.write_text(s)
print("ok")
