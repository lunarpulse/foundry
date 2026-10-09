//! Serve production wiring — M5.1 T53w (astra B1/B5: "caller-provided
//! role data is not current authorization"; serve must be THE path
//! for policy application).
//!
//! The wired application endpoint composes what already exists:
//!   T29 identity: login -> opaque session -> resolve_session(token)
//!                 yields the CURRENT role from the server-side
//!                 store (a demoted user's session sees the demotion
//!                 immediately — no role rides in the request).
//!   T54w binding: the command's digest is recomputed from the FILED
//!                 proposal's complete content (diff fields, target,
//!                 prediction, base version) — substitution refused.
//!   T50b apply:   owner-gated, single-use, stale/expiry checks.
//!   T52w coord.:  the activation pair appends to the durable
//!                 governance journal.
//!
//! The serve path REPLACES caller-supplied roles: `apply_policy`
//! takes a session TOKEN; the role is looked up. The CLI path can
//! still construct an ApplyCommand directly, but ONLY after
//! `authenticate_cli` resolves a real session (no ambient owner).
use foundry_watch::bundles::BundleStore;
use foundry_watch::command_binding::{HardenedApply, HardenedError};
use foundry_watch::coordinator::GovernanceCoordinator;
use foundry_watch::governance_events::GovernanceEvent;
use foundry_watch::policy_apply::{ApplyCommand, ApplyError, PolicyApplicationLayer, Role};
use foundry_watch::retro::InsightPipeline;
use crate::identity::{Identity, IdentityError};

#[derive(Debug)]
pub enum ServeWiringError {
    /// no session / expired / disabled — authentication failed first
    Unauthenticated(String),
    /// authenticated but the CURRENT role is not Owner (demotion
    /// applies instantly to live sessions)
    Forbidden { current_role: String },
    Binding(HardenedError),
    Apply(ApplyError),
    Coordinator(foundry_watch::coordinator::CoordinatorError),
}

impl From<HardenedError> for ServeWiringError {
    fn from(e: HardenedError) -> Self {
        ServeWiringError::Binding(e)
    }
}
impl From<ApplyError> for ServeWiringError {
    fn from(e: ApplyError) -> Self {
        ServeWiringError::Apply(e)
    }
}
impl From<foundry_watch::coordinator::CoordinatorError> for ServeWiringError {
    fn from(e: foundry_watch::coordinator::CoordinatorError) -> Self {
        ServeWiringError::Coordinator(e)
    }
}

/// The wired serve endpoint for policy application. `session_token`
/// is the opaque credential; the role NEVER rides in the request.
pub fn apply_policy_via_serve(
    identity: &mut Identity,
    pipeline: &InsightPipeline,
    layer: &mut PolicyApplicationLayer,
    coordinator: &mut GovernanceCoordinator,
    session_token: &str,
    cmd: &ApplyCommand,
    proposer: &str,
    now: u64,
) -> Result<String, ServeWiringError> {
    // 1. AUTHENTICATION: resolve the session to the CURRENT user
    let user = identity
        .resolve_session(session_token)
        .map_err(|e: IdentityError| ServeWiringError::Unauthenticated(format!("{e}")))?;

    // 2. CURRENT AUTHORIZATION: the role is looked up, not supplied.
    //    A demoted owner's live session fails HERE, instantly.
    let current_role = role_of(&user.role);
    if current_role != Role::Owner {
        return Err(ServeWiringError::Forbidden { current_role: format!("{:?}", user.role) });
    }

    // 3. BINDING (T54w): digest must equal the canonical digest of
    //    the filed content; field classification; base version.
    HardenedApply::verify_binding(pipeline, cmd, layer.policy_version)?;

    // 4. APPLY (T50b): owner authority, single-use, expiry.
    let applied = layer.apply(cmd, pipeline, &foundry_watch::evaluation_protocol::frozen_policy_graph(), proposer, now)?;

    // 5. DURABILITY (T52w): the activation pair rides the single
    //    journal — appended through the coordinator, seq assigned by
    //    the database.
    for phase_use in ["pending", "committed"] {
        let ev = GovernanceEvent {
            schema_version: 1,
            seq: 0,
            op: foundry_watch::governance_events::OpKind::PolicyApplication,
            phase: if phase_use == "pending" {
                foundry_watch::governance_events::Phase::Pending
            } else {
                foundry_watch::governance_events::Phase::Committed
            },
            op_id: format!("serve:{}", cmd.op_id),
            payload_digest: cmd.proposal_digest.clone(),
            actor: format!("serve:{}", user.username),
            at: "2026-10-08T00:00:00Z".into(),
        };
        coordinator.append(ev)?;
    }

    Ok(format!(
        "applied target={} fields={} policy_version={}",
        applied.target,
        applied.fields.len(),
        layer.policy_version
    ))
}

/// CLI authentication bridge: the CLI cannot mint owner authority —
/// it must present a session token that resolves to a CURRENT owner.
pub fn authenticate_cli(identity: &mut Identity, session_token: &str) -> Result<String, ServeWiringError> {
    let user = identity
        .resolve_session(session_token)
        .map_err(|e: IdentityError| ServeWiringError::Unauthenticated(format!("{e}")))?;
    if role_of(&user.role) != Role::Owner {
        return Err(ServeWiringError::Forbidden { current_role: format!("{:?}", user.role) });
    }
    Ok(user.username)
}

fn role_of(r: &crate::identity::Role) -> Role {
    use crate::identity::Role as SRole;
    match r {
        SRole::Owner => Role::Owner,
        SRole::Approver => Role::Approver,
        SRole::Observer => Role::Observer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foundry_watch::retro::{EvidenceEnvelope, InsightRecord, PolicyDiff, Prediction, QueryCatalog};
    use std::collections::BTreeMap;

    struct FastHasher;
    impl crate::identity::PasswordHasher for FastHasher {
        fn hash(&self, p: &str) -> String {
            use sha2::Digest as _;
            let mut h = sha2::Sha512::new();
            h.update(p.as_bytes());
            format!("fast${:x}", h.finalize())
        }
        fn verify(&self, p: &str, hash: &str) -> bool {
            self.hash(p) == hash
        }
    }

    static DB_N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    fn identity_with_owner() -> (Identity, String) {
        let n = DB_N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("t53w-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("id.db");
        let _ = std::fs::remove_file(&path);
        let mut id = Identity::open(&path).unwrap();
        id.bootstrap_owner("owner-1", "correct horse battery staple", &FastHasher).unwrap();
        let tok = id.login("owner-1", "correct horse battery staple", &FastHasher, 3600).unwrap();
        (id, tok)
    }

    fn pipeline() -> InsightPipeline {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), foundry_watch::evaluation_protocol::frozen_policy_graph());
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 1,
            ledger_prefix_head_hash: "h".into(),
            corpus_snapshot_digest: "c".into(),
            query_id: "retry_rate_by_task_class".into(),
            query_params: Default::default(),
            cohort: "all".into(),
            exclusions: vec![],
            denominator: "d".into(),
            missing_data_policy: "m".into(),
            policy_version: 1,
            protocol_digest: "x".into(),
            result_payload: "r".into(),
            result_digest: String::new(),
        };
        e.result_digest = e.compute_result_digest();
        p.ingest(InsightRecord {
            insight_id: "I1".into(),
            claim: "c".into(),
            evidence: e,
            proposal: Some(PolicyDiff {
                target: "poll_interval".into(),
                changes: [("max_retries".to_string(), "2".to_string())].into_iter().collect(),
            }),
            prediction: Some(Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "<0.25".into(),
                window: "next-2-retro-windows".into(),
            }),
            n: 20,
        })
        .unwrap();
        p
    }

    fn owner_cmd(digest: String) -> ApplyCommand {
        ApplyCommand {
            op_id: "op-serve-1".into(),
            approver: "owner-1".into(),
            role: Role::Owner,
            proposal_digest: digest,
            prediction: Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "<0.25".into(),
                window: "next-2-retro-windows".into(),
            },
            base_policy_version: 0,
            issued_at: 1,
            expires_at: 9_999,
        }
    }

    #[test]
    fn full_serve_path_applies_policy_durably() {
        let (mut id, tok) = identity_with_owner();
        let p = pipeline();
        let mut layer = PolicyApplicationLayer::new();
        let mut coord = GovernanceCoordinator::open_in_memory().unwrap();
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let out = apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &owner_cmd(d), "agent-1", 10).unwrap();
        assert!(out.contains("policy_version=1"));
        assert_eq!(coord.head().unwrap().1, 2, "activation pair on the durable journal");
        assert_eq!(coord.restart_state().unwrap().clean, true);
    }

    #[test]
    fn demoted_owner_is_instantly_forbidden() {
        let (mut id, tok) = identity_with_owner();
        // demote while the session is LIVE. T29 C2 forbids disabling
        // the LAST owner — create a second owner first, then disable
        // owner-1 (whose session is live).
        let actor = id.resolve_session(&tok).unwrap();
        id.create_user(&actor, "owner-2", "pw2", crate::identity::Role::Owner, &FastHasher).unwrap();
        id.set_disabled(&actor, "owner-1", true).unwrap();
        let p = pipeline();
        let mut layer = PolicyApplicationLayer::new();
        let mut coord = GovernanceCoordinator::open_in_memory().unwrap();
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let err = apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &owner_cmd(d), "a", 10).unwrap_err();
        assert!(matches!(err, ServeWiringError::Unauthenticated(_)),
            "disabled owner's live session refused at authentication");
        assert_eq!(coord.head().unwrap().1, 0, "nothing journaled on refusal");
    }

    #[test]
    fn observer_session_cannot_apply() {
        let (mut id, owner_tok) = identity_with_owner();
        let actor = id.resolve_session(&owner_tok).unwrap();
        id.create_user(&actor, "obs-1", "pw", crate::identity::Role::Observer, &FastHasher).unwrap();
        let tok = id.login("obs-1", "pw", &FastHasher, 3600).unwrap();
        let p = pipeline();
        let mut layer = foundry_watch::policy_apply::PolicyApplicationLayer::new();
        let mut coord = foundry_watch::coordinator::GovernanceCoordinator::open_in_memory().unwrap();
        let d = foundry_watch::command_binding::HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let err = apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &owner_cmd(d), "a", 10).unwrap_err();
        assert!(matches!(err, ServeWiringError::Forbidden { .. }));
        assert_eq!(coord.head().unwrap().1, 0);
    }

    #[test]
    fn forged_digest_refused_before_any_effect() {
        let (mut id, tok) = identity_with_owner();
        let p = pipeline();
        let mut layer = PolicyApplicationLayer::new();
        let mut coord = GovernanceCoordinator::open_in_memory().unwrap();
        let mut cmd = owner_cmd("prop-deadbeef".into());
        cmd.approver = "owner-1".into();
        let err = apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &cmd, "a", 10).unwrap_err();
        assert!(matches!(err, ServeWiringError::Binding(HardenedError::ContentMismatch { .. })));
        assert_eq!(coord.head().unwrap().1, 0, "no journal entries for a refused command");
        assert_eq!(layer.policy_version, 0, "no policy change");
    }

    #[test]
    fn cli_requires_a_real_session() {
        let (mut id, _) = identity_with_owner();
        assert!(authenticate_cli(&mut id, "not-a-session").is_err());
        let tok = id.login("owner-1", "correct horse battery staple", &FastHasher, 3600).unwrap();
        let account = authenticate_cli(&mut id, &tok).unwrap();
        assert_eq!(account, "owner-1");
    }

    #[test]
    fn replay_through_serve_refused() {
        let (mut id, tok) = identity_with_owner();
        let p = pipeline();
        let mut layer = PolicyApplicationLayer::new();
        let mut coord = GovernanceCoordinator::open_in_memory().unwrap();
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &owner_cmd(d.clone()), "a", 10).unwrap();
        // second application of the SAME op: single-use refusal
        let err = apply_policy_via_serve(&mut id, &p, &mut layer, &mut coord, &tok, &owner_cmd(d), "a", 20).unwrap_err();
        // the second attempt fails closed — stale base (policy moved
        // to v1) fires before the replay check here; BOTH are the
        // single-use family. Assert the closed failure precisely:
        assert!(
            matches!(
                err,
                ServeWiringError::Binding(HardenedError::Delegate(ApplyError::StaleBase { .. }))
                    | ServeWiringError::Apply(ApplyError::Replayed { .. })
            ),
            "fail-closed either way, got: {err:?}"
        );
        assert_eq!(coord.head().unwrap().1, 2, "only the FIRST activation is journaled");
    }
}
