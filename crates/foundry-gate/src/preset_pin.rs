//! 프리셋 배포 핀 — bmad-intake-pool.v3.json 의 sha256 상수 동결.
//! 배포 자산 바이트 변조 감지 (T55 MINOR 이관, T56 게이트).

use sha2::Digest;

/// 배포 시점 실측 지문 — 프리셋 파일이 이 값을 벗어나면 게이트 실패.
const BMAD_INTAKE_POOL_SHA256: &str = "9000fa16fd28bcb620738227109ecbb02b469b72fcd97fe2e04874da1bb1d1ad";

fn preset_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("presets/bmad-intake-pool.v3.json")
}

#[test]
fn preset_pin_bmad_intake_pool_v3_bytes_frozen() {
    let bytes = std::fs::read(preset_path()).unwrap();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    if BMAD_INTAKE_POOL_SHA256 == "<MEASURED>" {
        // 부트스트랩: 최초 실행은 실측값 인쇄 — 상수로 굳힌다.
        panic!("PIN THIS VALUE into BMAD_INTAKE_POOL_SHA256: {digest}");
    }
    assert_eq!(
        digest, BMAD_INTAKE_POOL_SHA256,
        "preset bytes drifted from the shipped pin — distribution tampered or edited without re-pinning"
    );
    // 핀된 프리셋은 검증기를 통과해야 한다 (출하 조건 재확인).
    let doc = foundry_workflow::dsl::load_preset(&preset_path()).unwrap();
    assert!(foundry_workflow::validator::validate(
        &doc,
        &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
    )
    .is_ok(), "pinned preset must still validate");
}
