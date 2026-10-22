//! 프리셋 배포 핀 — bmad-intake-pool.v3.json 의 sha256 상수 동결.
//! 배포 자산 바이트 변조 감지 (T55 MINOR 이관, T56 게이트).

use sha2::Digest;

/// 배포 시점 실측 지문 — 프리셋 파일이 이 값을 벗어나면 게이트 실패.
const BMAD_INTAKE_POOL_SHA256: &str =
    "9000fa16fd28bcb620738227109ecbb02b469b72fcd97fe2e04874da1bb1d1ad";
/// (B2) 스케일 1 — quickloop 실측 핀 (74f94be 배포).
const BMAD_QUICKLOOP_SHA256: &str =
    "7df2e0eb21b2fddc44f867c218af2ad8a4faff0ba38c59ea86fa083468338239";
/// (B2) 스케일 2 — feature-prd 실측 핀 (74f94be 배포).
const BMAD_FEATURE_PRD_SHA256: &str =
    "dfc041578692968eb7ae9e4b5ef0af10c91389b3bf7eb9195fae9c3238af5cee";
/// (T60) 스케일 3 — epic-build 실측 핀.
const BMAD_EPIC_BUILD_SHA256: &str =
    "7bb2bb7c60b3c598e040168c139890898b009db145c51cb86bd1553c62fea907";

fn preset_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("presets/bmad-intake-pool.v3.json")
}

/// (B2) 스케일 프리셋 핀 검증 공용 헬퍼 — 바이트 핀 + 검증기 통과.
fn assert_preset_pinned(rel: &str, pin: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(format!("presets/{rel}"));
    let bytes = std::fs::read(&path).unwrap();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    if pin == "<MEASURED>" {
        panic!("PIN THIS VALUE for {rel}: {digest}");
    }
    assert_eq!(
        digest, pin,
        "{rel} bytes drifted from the shipped pin — distribution tampered or edited without re-pinning"
    );
    let doc = foundry_workflow::dsl::load_preset(&path).unwrap();
    assert!(
        foundry_workflow::validator::validate(
            &doc,
            &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
        )
        .is_ok(),
        "{rel}: pinned preset must still validate"
    );
}

/// (B2) 스케일 1 — quickloop 배포 바이트 동결.
#[test]
fn preset_pin_bmad_quickloop_v3_bytes_frozen() {
    assert_preset_pinned("bmad-quickloop.v3.json", BMAD_QUICKLOOP_SHA256);
}

/// (B2) 스케일 2 — feature-prd 배포 바이트 동결.
#[test]
fn preset_pin_bmad_feature_prd_v3_bytes_frozen() {
    assert_preset_pinned("bmad-feature-prd.v3.json", BMAD_FEATURE_PRD_SHA256);
}

#[test]
fn preset_pin_bmad_epic_build_v3_bytes_frozen() {
    assert_preset_pinned("bmad-epic-build.v3.json", BMAD_EPIC_BUILD_SHA256);
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
    assert!(
        foundry_workflow::validator::validate(
            &doc,
            &foundry_domain::workflow_encoding::EmbeddedWorkflow::frozen_v1(),
        )
        .is_ok(),
        "pinned preset must still validate"
    );
}
