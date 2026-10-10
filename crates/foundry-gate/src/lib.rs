//! T56 종단 게이트 — M6 전 성과의 통합 증명.
//!
//! 게이트 목록 (플랜 v5): G3(4)·G13(4)·G14(3)·G16(오라클)·G17(6)·
//! A′(크래시 5지점+경쟁)·D′·프리셋 핀·전수주기.
//!
//! 이 크레이트는 게이트 전용 — 프로덕션 코드를 포함하지 않는다.

pub mod common;
pub mod g3_approval_fixture;
pub mod g13_activation;
pub mod g14_failure_mapping;
pub mod g16_oracle;
pub mod g17_cursor;
pub mod aprime_crash;
pub mod dprime_identity;
pub mod preset_pin;
pub mod full_cycle;
