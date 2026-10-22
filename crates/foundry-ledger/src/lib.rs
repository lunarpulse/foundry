//! foundry-ledger — append-only audit ledger (Ledger trait impl).
//! Chain-hashed, SQLite-backed, append-only (plan §11).
//! The full maos-audit vendor absorption is staged in `vendor/` (see VENDORED.md);
//! this file implements the M0 journal. MAOS types must not leak (tests/no_leak.rs).
// FOUNDRY (T5 decision 2026-10-01): full maos-audit absorption deferred to M1.
// Coupling survey missed crate-root deps (resolve_spirit_name, AuditFilter,
// erasure::merkle) that live in deferred lib.rs/erasure — vendored tree is
// entangled with the rest of maos-audit beyond the M0 boundary.
// M0 uses the native chain-hash journal below (same integrity guarantees:
// append-only + SHA-256 prev-hash chain). Vendored sources stay on disk
// under src/vendor/ for M1 Transparency Log mirroring work.
// #[cfg(any())] keeps it out of the build without deleting history.
#[cfg(any())]
pub mod shim;
#[cfg(any())]
pub(crate) mod vendor;

pub mod casting_journal;
pub mod event_json;
pub mod mirror;
pub mod product;
pub mod product_identity;
pub mod product_intake;
pub mod product_scope;
pub mod replay;
pub mod sqlite;

pub use mirror::{Mirror, MirrorLine, MirrorState};
#[cfg(feature = "p2-test-support")]
pub use product::P2ReviewPreparation;
pub use product::{
    AdmissionReceipt, ProductApprovalIntent, ProductApprovalReceipt, ProductArtifactContent,
    ProductArtifactInfo, ProductClock, ProductConfig, ProductError, ProductHoldAcceptance,
    ProductHoldDecision, ProductHoldExpectation, ProductPublishAuthorization,
    ProductResultArtifactContent, ProductResultError, ProductResultExpectation, ProductResultInfo,
    ProductReviewConfig, ProductServiceAction, ProductServiceContext, ProductServiceCredential,
    ProductServiceGrant, ProductServiceReceipt, ProductServiceView, ProductStore, ProductWorkClaim,
    ProductWorkClaimResult, SystemProductClock, product_host_result_digest,
};
#[cfg(feature = "p2-test-support")]
pub use product::{ChargeBudgetObservation, ChargeBudgetObserver};
pub use product_intake::{DueDelivery, ProductDelivery, ProductPollCursor};
pub use product_scope::{
    ProductCapability, RepositoryIdentity, ScopeGrantManifest, ScopeGrantReceipt, ScopeGrantView,
    ScopeMigrationManifest, ScopeMigrationReceipt, ScopeMutation, ScopeMutationReceiptKind,
    ScopeSourceManifest, ScopeSourceView, ScopedEvidenceSnapshot, ScopedOrderView,
    VerifiedOrderProvenance,
};
pub use sqlite::{AuditEntry, LedgerRow, SqliteLedger};
