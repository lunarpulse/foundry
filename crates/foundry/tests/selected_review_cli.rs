//! Runs the normal executable. All state changes go through authenticated CLI commands.
#![cfg(all(feature = "product-pack-pins", unix))]

#[test]
fn two_orders_consume_their_selected_packs() {
    let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/selected-review-cli.py");
    let status = std::process::Command::new("python3")
        .arg(driver)
        .arg(env!("CARGO_BIN_EXE_foundry"))
        .status()
        .expect("start local product CLI scenario");
    assert!(status.success());
}
