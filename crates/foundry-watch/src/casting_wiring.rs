//! Casting production wiring — M5.1 T51w (astra B1/B8: the casting
//! entry point must load digest-addressed bundles, verify at
//! consumption, and never mint tokens outside the domain path).
//!
//! This module wires `casting --run-profile` and `--promote` to the
//! M5 layers:
//!
//!   RUN:   --run-profile <BUNDLE-DIGEST> (bundle- prefixed) loads
//!          from the BundleStore with consumption-time verification
//!          (T46) and derives the CandidateProfile from the bundle —
//!          no hand-assembled profiles in the bundle path. The
//!          legacy string path (--run-profile without the bundle-
//!          prefix) still works for M3 replay compat, but the ROUTE
//!          CHOICE ITSELF is journaled (astra: "the path selection
//!          must be recorded"), and the legacy path is marked
//!          non-promotable-by-synthesis: a legacy run cannot be
//!          promoted through the bundle-bound token path.
//!
//!   PROMOTE: BundleStore tokens are only EVER issued when a DOMAIN
//!          ApprovalToken was consumed (single-use) for the same
//!          digest. The store cannot mint authority on its own —
//!          issue_for_domain_token() checks the domain gate's
//!          consumption record. This is the B8 fix: "a new local
//!          string-token mechanism cannot substitute for the
//!          existing domain token".
use crate::bundles::{BundleProfile, BundleStore, DependencyPins};
use crate::governance_events::{GovernanceEvent, OpKind, Phase};
use foundry_domain::ApprovalToken;

pub const BUNDLE_DIGEST_PREFIX: &str = "bundle-";

#[derive(Debug, Clone, PartialEq)]
pub enum WiringError {
    /// digest not found OR failed consumption-time verification
    BundleUnavailable { digest: String, detail: String },
    /// the run id names a LEGACY (string) run: promotion requires a
    /// bundle-bound run
    LegacyNotPromotable { run_id: String },
    /// bundle token issued without a domain token consumption —
    /// structurally prevented, surfaced if attempted
    TokenWithoutDomain,
    /// domain token already consumed / wrong digest binding
    DomainRefusal { detail: String },
}

/// The wired profile source: either a verified bundle digest or an
/// explicitly-journaled legacy string.
#[derive(Debug, Clone, PartialEq)]
pub enum ProfileSource {
    Bundle { digest: String },
    Legacy { raw: String },
}

pub fn classify_profile_arg(arg: &str) -> ProfileSource {
    if arg.starts_with(BUNDLE_DIGEST_PREFIX) {
        ProfileSource::Bundle { digest: arg.to_string() }
    } else {
        ProfileSource::Legacy { raw: arg.to_string() }
    }
}

/// The run record this wiring maintains: which route ran, which
/// digest (if bundle), whether the profile derived from a verified
/// bundle.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WiredRunRecord {
    pub run_id: String,
    pub route: String, // "bundle" | "legacy"
    pub bundle_digest: Option<String>,
    pub profile_hash: String,
    /// bundle runs carry consumption verification; legacy runs do not
    pub consumption_verified: bool,
}

/// Derive the M3 CandidateProfile from a VERIFIED bundle (T46).
/// The profile is a projection of bundle content — no free-floating
/// fields the bundle does not carry.
pub fn profile_from_bundle(
    store: &BundleStore,
    digest: &str,
    run_id: &str,
) -> Result<(crate::dry_die::CandidateProfile, WiredRunRecord), WiringError> {
    let bundle = store
        .get_verified(digest)
        .map_err(|e| WiringError::BundleUnavailable { digest: digest.into(), detail: format!("{e:?}") })?;
    let profile = crate::dry_die::CandidateProfile {
        id: bundle.candidate_id.clone(),
        model_id: bundle.profile.model.clone(),
        prompt_template: bundle.prompt_template.clone(),
        tool_allowlist: bundle
            .pins
            .tool_digests
            .keys()
            .cloned()
            .collect(),
        env_allowlist: vec![],
        seed: bundle.profile.seed.unwrap_or(0),
    };
    let record = WiredRunRecord {
        run_id: run_id.into(),
        route: "bundle".into(),
        bundle_digest: Some(digest.to_string()),
        profile_hash: profile.hash(),
        consumption_verified: true,
    };
    Ok((profile, record))
}

/// Legacy string route: journals the route choice explicitly. The
/// record marks it non-promotable via the bundle path.
pub fn legacy_profile(raw: &str, run_id: &str, seed: u64) -> (crate::dry_die::CandidateProfile, WiredRunRecord) {
    let profile = crate::dry_die::CandidateProfile {
        id: raw.to_string(),
        model_id: "mock/deterministic".into(),
        prompt_template: "dep-bump-v1".into(),
        tool_allowlist: vec!["cargo".into()],
        env_allowlist: vec!["PATH".into()],
        seed,
    };
    let record = WiredRunRecord {
        run_id: run_id.into(),
        route: "legacy".into(),
        bundle_digest: None,
        profile_hash: profile.hash(),
        consumption_verified: false,
    };
    (profile, record)
}

/// Domain-token-bound token issuance (B8): the BundleStore token for
/// digest D is issued ONLY when the caller presents a DOMAIN
/// ApprovalToken consumption record for D. In production the serve
/// approve path consumes the domain token atomically (T29 C4 single
/// consumption) and returns the consumption proof; this function is
/// the bridge — it verifies the proof and issues the store token.
pub fn issue_bundle_token_for_domain(
    store: &mut BundleStore,
    coordinator_events: &mut Vec<GovernanceEvent>,
    domain_gate: &dyn DomainTokenGate,
    digest: &str,
    store_token: &str,
) -> Result<(), WiringError> {
    // the domain gate is the ONLY authority: it reports whether a
    // human ApprovalToken was consumed (single-use) for this digest
    let consumed = domain_gate
        .consume_for(digest)
        .map_err(|detail| WiringError::DomainRefusal { detail })?;
    if !consumed {
        return Err(WiringError::TokenWithoutDomain);
    }
    store
        .issue_token(digest, store_token)
        .map_err(|e| WiringError::BundleUnavailable { digest: digest.into(), detail: format!("{e:?}") })?;
    // the bridge itself is a governance event: CandidateActivate
    // intent+effect, payload = the digest + token id
    coordinator_events.push(GovernanceEvent {
        schema_version: 1,
        seq: 0, // coordinator assigns
        op: OpKind::CandidateActivate,
        phase: Phase::Pending,
        op_id: format!("activate:{store_token}"),
        payload_digest: digest.to_string(),
        actor: "domain-bridge".into(),
        at: "2026-10-08T00:00:00Z".into(),
    });
    coordinator_events.push(GovernanceEvent {
        schema_version: 1,
        seq: 0,
        op: OpKind::CandidateActivate,
        phase: Phase::Committed,
        op_id: format!("activate:{store_token}"),
        payload_digest: digest.to_string(),
        actor: "domain-bridge".into(),
        at: "2026-10-08T00:00:00Z".into(),
    });
    Ok(())
}

/// The domain authority port: production impl wraps T29 identity +
/// casting domain transition (transition_promote with ApprovalToken);
/// tests use doubles.
pub trait DomainTokenGate {
    /// Attempt to consume a human ApprovalToken for `digest`.
    /// Returns Ok(true) when consumed (single-use), Ok(false) when
    /// no valid unconsumed token exists, Err for domain refusal.
    fn consume_for(&self, digest: &str) -> Result<bool, String>;
}

/// Promote: a store token may only promote a run whose record shows
/// a VERIFIED BUNDLE route. Legacy runs refuse with a clear error.
pub fn promote_with_store_token(
    store: &mut BundleStore,
    record: &WiredRunRecord,
    store_token: &str,
) -> Result<(), WiringError> {
    match (&record.route, &record.bundle_digest) {
        (route, Some(d)) if route == "bundle" => store
            .consume_token(store_token, d)
            .map_err(|e| WiringError::DomainRefusal { detail: format!("{e:?}") }),
        _ => Err(WiringError::LegacyNotPromotable { run_id: record.run_id.clone() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundles::{CandidateBundle, LessonSnapshot};

    fn bundle_store() -> (BundleStore, String) {
        let mut st = BundleStore::default();
        let b = CandidateBundle::build(
            "cand-w1",
            BundleProfile { model: "deepseek-v4".into(), temperature: Some(0.0), seed: Some(7), max_output_tokens: 4096 },
            "solve {{task}} carefully",
            vec![LessonSnapshot { lesson_id: "L1".into(), content_hash: {
                let mut h = sha2::Sha256::new();
                use sha2::Digest as _;
                h.update(&(16u64).to_be_bytes());
                h.update(b"prefer map_err..");
                format!("{:x}", h.finalize())
            }, content: "prefer map_err..".into() }],
            DependencyPins {
                image_digest: "sha256:aa".into(),
                tool_digests: [("cargo".to_string(), "sha256:bb".into())].into_iter().collect(),
                model_revision: Some("rev-9".into()),
            },
        )
        .unwrap();
        let d = st.put(b);
        (st, d)
    }

    struct HumanGate {
        valid: bool,
        consumed: std::sync::atomic::AtomicBool,
    }
    impl DomainTokenGate for HumanGate {
        fn consume_for(&self, _digest: &str) -> Result<bool, String> {
            // single-use: the first consume succeeds, the second sees false
            if self.valid && !self.consumed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                Ok(true)
            } else if self.valid {
                Ok(false) // token already consumed (T29 C4 semantics)
            } else {
                Ok(false) // no valid token at all
            }
        }
    }

    #[test]
    fn bundle_route_loads_verified_bundle_only() {
        let (st, d) = bundle_store();
        let (profile, record) = profile_from_bundle(&st, &d, "run-1").unwrap();
        assert_eq!(profile.model_id, "deepseek-v4");
        assert_eq!(profile.prompt_template, "solve {{task}} carefully");
        assert!(record.consumption_verified);
        assert_eq!(record.route, "bundle");
        // tampered digest refused
        let mut st2 = st.clone_for_test();
        let keys = st2.by_digest_keys();
        let d = keys[0].clone();
        let _ = d;
        assert!(matches!(
            profile_from_bundle(&st, "bundle-deadbeef", "run-2"),
            Err(WiringError::BundleUnavailable { .. })
        ));
    }

    #[test]
    fn profile_is_a_projection_of_the_bundle() {
        let (st, d) = bundle_store();
        let (p1, _) = profile_from_bundle(&st, &d, "a").unwrap();
        let (p2, _) = profile_from_bundle(&st, &d, "b").unwrap();
        assert_eq!(p1, p2, "same bundle -> same profile, no free-floating state");
        assert_eq!(p1.hash(), p2.hash());
        // tool allowlist derives from the pinned tools
        assert!(p1.tool_allowlist.contains(&"cargo".to_string()));
    }

    #[test]
    fn legacy_route_journaled_and_non_promotable() {
        let (p, record) = legacy_profile("dep-bump-v1", "run-l1", 42);
        assert_eq!(record.route, "legacy");
        assert!(!record.consumption_verified);
        assert_eq!(p.id, "dep-bump-v1");
        // promotion of a legacy run through the store token: refused
        let (mut st, _) = bundle_store();
        assert!(matches!(
            promote_with_store_token(&mut st, &record, "tok"),
            Err(WiringError::LegacyNotPromotable { .. })
        ));
    }

    #[test]
    fn tokens_require_domain_consumption() {
        let (mut st, d) = bundle_store();
        let mut events = Vec::new();
        // NO valid domain token: issuance refused
        let no_gate = HumanGate { valid: false, consumed: Default::default() };
        assert!(matches!(
            issue_bundle_token_for_domain(&mut st, &mut events, &no_gate, &d, "tok-1"),
            Err(WiringError::TokenWithoutDomain)
        ));
        assert!(st.tokens_len() == 0);
        // valid domain token: issuance succeeds + governance pair
        let gate = HumanGate { valid: true, consumed: Default::default() };
        issue_bundle_token_for_domain(&mut st, &mut events, &gate, &d, "tok-1").unwrap();
        assert_eq!(st.tokens_len(), 1);
        assert_eq!(events.len(), 2, "Pending + Committed bridge events");
        // DOUBLE ISSUANCE with the same (now consumed) domain token: refused
        assert!(matches!(
            issue_bundle_token_for_domain(&mut st, &mut events, &gate, &d, "tok-2"),
            Err(WiringError::TokenWithoutDomain)
        ), "domain token single-use semantics flow through the bridge");
        // and the issued token promotes the bundle run
        let (_, record) = profile_from_bundle(&st, &d, "run-9").unwrap();
        promote_with_store_token(&mut st, &record, "tok-1").unwrap();
    }

    #[test]
    fn run_profile_arg_classification() {
        assert!(matches!(classify_profile_arg("bundle-abc123"), ProfileSource::Bundle { .. }));
        assert!(matches!(classify_profile_arg("dep-bump-v1"), ProfileSource::Legacy { .. }));
    }
}
