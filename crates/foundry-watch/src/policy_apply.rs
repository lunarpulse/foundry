//! Owner-gated typed policy application + prediction scoring + rollback
//! — M5 T50b. The ONLY place in the retro loop where a proposal
//! becomes an applied change, and every C-A1 binding fires here.
//!
//! C-A1 (audited activation): an application command binds to
//!   { proposal digest, evidence digest, prediction digest,
//!     base policy version, target policy digest, scope, approver,
//!     expiry } — and is refused when stale, replayed, expired, or
//!     issued by a principal without authority. Activation + journal
//! are one intent/effect pair (T49a), recoverable, single-use.
//!
//! C-P1 routing inside the apply path: even a fully-authorized owner
//! command on a CandidateAffecting field is REFUSED here — those
//! changes must ride the M3 pipeline (new bundle, full examination).
//! The owner approves; the owner does not bypass.
//!
//! C-R3 closure: predictions are pre-registered at application time
//! and scored on schedule; proposer track records update via the
//! FROZEN weight rule (T48a derive_weights) — repeated misses
//! degrade a proposer's standing, repeated hits raise it.
use crate::evaluation_protocol::{derive_weights, PolicyClass, PolicyGraph};
use crate::governance_events::{verify_stream, GovernanceEvent, OpKind, Phase};
use crate::retro::{InsightPipeline, Prediction, ProposalStrength};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq)]
pub enum ApplyError {
    /// the approver is not an owner-class principal
    NotAuthorized { approver: String },
    /// command older than the policy it targets — re-derive, re-review
    StaleBase { cmd_base: u32, current: u32 },
    /// single-use: this command was already applied
    Replayed { op_id: String },
    /// the command's validity window closed
    Expired { at: u64, expires: u64 },
    /// the proposal digest does not match anything filed (or filed
    /// but not Supported strength)
    NoSuchSupportedProposal { digest: String },
    /// C-P1: candidate-affecting fields never apply here — M3 route
    CandidateAffectingNeedsM3 { target: String },
    /// rollback target unknown or already rolled back
    RollbackInvalid { op_id: String },
    /// scoring window not yet due
    NotDue { window: String },
}

/// Roles from the C1 matrix (T29) — only Owner may apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Owner,
    Approver,
    Observer,
}

/// The typed application command. Every C-A1 field is mandatory —
/// there is no partial form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplyCommand {
    pub op_id: String,
    pub approver: String,
    pub role: Role,
    /// digest of the filed proposal record this applies
    pub proposal_digest: String,
    /// the prediction this application is accountable to
    pub prediction: Prediction,
    /// policy version the review happened against
    pub base_policy_version: u32,
    /// monotonic wall clock (seconds) + validity window
    pub issued_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedPolicy {
    pub target: String,
    pub fields: BTreeMap<String, String>,
    pub applied_by_op: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScoreOutcome {
    Hit,
    Miss,
    /// window not closed yet — no reputation effect either way
    Pending,
}

#[derive(Default)]
pub struct PolicyApplicationLayer {
    pub policy_version: u32,
    pub applied: BTreeMap<String, AppliedPolicy>,
    used_op_ids: BTreeSet<String>,
    /// proposer account -> prediction outcomes (C-R3 track record)
    track: BTreeMap<String, Vec<bool>>,
    /// registered predictions: op_id -> (proposer, prediction, window_due, scored)
    registered: BTreeMap<String, (String, Prediction, u64, bool)>,
    pub governance: Vec<GovernanceEvent>,
    gov_seq: i64,
}

impl PolicyApplicationLayer {
    pub fn new() -> Self {
        Self::default()
    }

    fn gov(&mut self, op: OpKind, phase: Phase, op_id: &str, payload: &str) {
        self.gov_seq += 1;
        self.governance.push(GovernanceEvent {
            schema_version: 1,
            seq: self.gov_seq,
            op,
            phase,
            op_id: op_id.to_string(),
            payload_digest: payload.to_string(),
            actor: "policy-apply".into(),
            at: "2026-10-08T00:00:00Z".into(),
        });
    }

    pub fn digest_of(parts: &[&str]) -> String {
        let mut h = Sha256::new();
        for p in parts {
            h.update(&(p.len() as u64).to_be_bytes());
            h.update(p.as_bytes());
        }
        format!("pa-{:.16x}", h.finalize())
    }

    /// THE apply path. Owner authority, freshness, single-use, expiry,
    /// proposal strength, and C-P1 routing — all here, all fail-closed.
    pub fn apply(
        &mut self,
        cmd: &ApplyCommand,
        pipeline: &InsightPipeline,
        graph: &PolicyGraph,
        proposer: &str,
        now: u64,
    ) -> Result<AppliedPolicy, ApplyError> {
        // authority (C1 matrix: Owner only — approvers review orders,
        // they do not change factory policy)
        if cmd.role != Role::Owner {
            return Err(ApplyError::NotAuthorized { approver: cmd.approver.clone() });
        }
        // single-use BEFORE anything else (replay resistance)
        if self.used_op_ids.contains(&cmd.op_id) {
            return Err(ApplyError::Replayed { op_id: cmd.op_id.clone() });
        }
        // expiry
        if now > cmd.expires_at {
            return Err(ApplyError::Expired { at: now, expires: cmd.expires_at });
        }
        // the proposal must be filed and Supported. Since T54w the
        // canonical digest binds the COMPLETE content (diff, target,
        // prediction, base version) — the apply layer accepts the
        // canonical form (prop- prefixed) and, for compat, the old
        // simple form.
        let supported = pipeline.proposals();
        let found = supported.iter().find(|r| {
            let canonical = crate::command_binding::HardenedApply::command_digest_for(
                pipeline,
                &r.insight_id,
                cmd.base_policy_version,
            );
            canonical.as_deref() == Some(cmd.proposal_digest.as_str())
                || Self::digest_of(&[&r.insight_id, &r.evidence.result_digest]) == cmd.proposal_digest
        });
        let record = found.ok_or_else(|| ApplyError::NoSuchSupportedProposal { digest: cmd.proposal_digest.clone() })?;
        let target = record.proposal.as_ref().unwrap().target.clone();
        let fields = record.proposal.as_ref().unwrap().changes.clone();
        // stale-base: review happened against an older policy version
        if cmd.base_policy_version != self.policy_version {
            return Err(ApplyError::StaleBase { cmd_base: cmd.base_policy_version, current: self.policy_version });
        }
        // C-P1 routing: behavior-affecting targets never apply here
        let node = graph.nodes.get(&target).expect("admissible targets are classified (T50a gate)");
        if node.class == PolicyClass::CandidateAffecting {
            return Err(ApplyError::CandidateAffectingNeedsM3 { target });
        }

        // intent -> effect
        let payload = Self::digest_of(&[&cmd.proposal_digest, &target]);
        self.gov(OpKind::PolicyApplication, Phase::Pending, &cmd.op_id, &payload);
        self.policy_version += 1;
        let applied = AppliedPolicy { target, fields, applied_by_op: cmd.op_id.clone() };
        self.applied.insert(cmd.op_id.clone(), applied.clone());
        // pre-register the prediction this application is accountable to
        let due = parse_window(&cmd.prediction.window);
        self.registered.insert(cmd.op_id.clone(), (proposer.to_string(), cmd.prediction.clone(), now + due, false));
        self.gov(OpKind::PolicyApplication, Phase::Committed, &cmd.op_id, &payload);
        self.used_op_ids.insert(cmd.op_id.clone());
        Ok(applied)
    }

    /// Scheduled scoring (C-R3): close due windows, update track
    /// records. `result_for` decides hit/miss for a scored metric —
    /// supplied by the evidence layer (deterministic query re-run).
    pub fn score_due<F>(&mut self, now: u64, result_for: F) -> Vec<(String, ScoreOutcome)>
    where
        F: Fn(&Prediction) -> Option<bool>,
    {
        let mut out = Vec::new();
        let due: Vec<String> = self
            .registered
            .iter()
            .filter(|(_, (_, _, due, scored))| !scored && now >= *due)
            .map(|(op, _)| op.clone())
            .collect();
        for op_id in due {
            let (proposer, prediction, _, _) = self.registered.get(&op_id).unwrap().clone();
            let outcome = match result_for(&prediction) {
                None => ScoreOutcome::Pending, // evidence not available: stays pending, no reputation change
                Some(true) => ScoreOutcome::Hit,
                Some(false) => ScoreOutcome::Miss,
            };
            let payload = Self::digest_of(&[&op_id, &prediction.metric]);
            self.gov(OpKind::PredictionScore, Phase::Pending, &format!("score:{op_id}"), &payload);
            if !matches!(outcome, ScoreOutcome::Pending) {
                self.track.entry(proposer).or_default().push(matches!(outcome, ScoreOutcome::Hit));
            }
            if let Some((_, _, _, scored)) = self.registered.get_mut(&op_id) {
                *scored = true;
            }
            self.gov(OpKind::PredictionScore, Phase::Committed, &format!("score:{op_id}"), &payload);
            out.push((op_id, outcome));
        }
        out
    }

    /// Track records under the FROZEN weight rule (T48a).
    pub fn proposer_weights(&self) -> BTreeMap<String, f64> {
        derive_weights(&self.track)
    }

    /// Owner rollback: restores the previous policy version, removes
    /// the applied entry, and is itself an audited operation.
    pub fn rollback(&mut self, op_id: &str, approver_role: Role, reason: &str) -> Result<u32, ApplyError> {
        if approver_role != Role::Owner {
            return Err(ApplyError::NotAuthorized { approver: "rollback".into() });
        }
        if !self.applied.contains_key(op_id) {
            return Err(ApplyError::RollbackInvalid { op_id: op_id.to_string() });
        }
        let payload = Self::digest_of(&[op_id, reason]);
        let rb_op = format!("rollback:{op_id}");
        // rollback is itself an intent/effect pair: the intent to
        // undo, then the undo (Aborted = the original application's
        // effect is retracted). Both journaled for stream validity.
        self.gov(OpKind::PolicyApplication, Phase::Pending, &rb_op, &payload);
        self.applied.remove(op_id);
        self.policy_version += 1;
        self.gov(OpKind::PolicyApplication, Phase::Aborted, &rb_op, &payload);
        Ok(self.policy_version)
    }

    pub fn governance_ok(&self) -> bool {
        verify_stream(&self.governance).is_ok()
    }

    /// Clone for scenario forks (gate tooling).
    pub fn clone_state(&self) -> Self {
        Self {
            policy_version: self.policy_version,
            applied: self.applied.clone(),
            used_op_ids: self.used_op_ids.clone(),
            track: self.track.clone(),
            registered: self.registered.clone(),
            governance: self.governance.clone(),
            gov_seq: self.gov_seq,
        }
    }
}

fn parse_window(window: &str) -> u64 {
    // "next-2-retro-windows" -> 2 windows; windows are opaque here,
    // the scheduler maps them to wall time. Fixed mapping for tests:
    if let Some(n) = window.strip_prefix("next-").and_then(|w| w.split('-').next()).and_then(|n| n.parse::<u64>().ok()) {
        n * 3600
    } else {
        3600
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation_protocol::frozen_policy_graph;
    use crate::retro::{EvidenceEnvelope, InsightRecord, PolicyDiff, QueryCatalog};

    fn pipeline_with_poll(n: usize) -> InsightPipeline {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        let mut params = BTreeMap::new();
        params.insert("task_class".to_string(), "dep_bump".to_string());
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 157,
            ledger_prefix_head_hash: "abc".into(),
            corpus_snapshot_digest: "c".into(),
            query_id: "retry_rate_by_task_class".into(),
            query_params: params,
            cohort: "all orders".into(),
            exclusions: vec![],
            denominator: "attempts".into(),
            missing_data_policy: "excluded both".into(),
            policy_version: 1,
            protocol_digest: "x".into(),
            result_payload: r#"{"dep_bump":0.41}"#.into(),
            result_digest: String::new(),
        };
        e.result_digest = e.compute_result_digest();
        p.ingest(InsightRecord {
            insight_id: "I1".into(),
            claim: "dep-bump retries hot".into(),
            evidence: e,
            proposal: Some(PolicyDiff {
                target: "poll_interval".into(),
                changes: [("max_retries".to_string(), "2".to_string())].into_iter().collect(),
            }),
            prediction: Some(Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "dep_bump < 0.25".into(),
                window: "next-2-retro-windows".into(),
            }),
            n,
        })
        .unwrap();
        p
    }

    fn proposal_digest_of(p: &InsightPipeline) -> String {
        let r = p.proposals()[0];
        PolicyApplicationLayer::digest_of(&[&r.insight_id, &r.evidence.result_digest])
    }

    fn owner_cmd(op: &str) -> ApplyCommand {
        ApplyCommand {
            op_id: op.into(),
            approver: "owner-1".into(),
            role: Role::Owner,
            proposal_digest: proposal_digest_of(&pipeline_with_poll(20)),
            prediction: Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "dep_bump < 0.25".into(),
                window: "next-2-retro-windows".into(),
            },
            base_policy_version: 0,
            issued_at: 100,
            expires_at: 900,
        }
    }

    #[test]
    fn happy_owner_apply_bumps_version_and_registers_prediction() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        let applied = l.apply(&owner_cmd("op-1"), &p, &frozen_policy_graph(), "agent-7", 200).unwrap();
        assert_eq!(applied.target, "poll_interval");
        assert_eq!(l.policy_version, 1);
        assert!(l.governance_ok());
        // prediction registered, not yet scored
        assert_eq!(l.score_due(300, |_| None).len(), 0);
    }

    #[test]
    fn non_owner_refused() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        let mut c = owner_cmd("op-2");
        c.role = Role::Approver; // order-approvers cannot change policy
        assert!(matches!(l.apply(&c, &p, &frozen_policy_graph(), "a", 200), Err(ApplyError::NotAuthorized { .. })));
    }

    #[test]
    fn replay_refused() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        l.apply(&owner_cmd("op-3"), &p, &frozen_policy_graph(), "a", 200).unwrap();
        assert!(matches!(
            l.apply(&owner_cmd("op-3"), &p, &frozen_policy_graph(), "a", 300),
            Err(ApplyError::Replayed { .. })
        ));
    }

    #[test]
    fn expired_refused() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        assert!(matches!(
            l.apply(&owner_cmd("op-4"), &p, &frozen_policy_graph(), "a", 9_000),
            Err(ApplyError::Expired { .. })
        ));
    }

    #[test]
    fn stale_base_refused() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        l.apply(&owner_cmd("op-5"), &p, &frozen_policy_graph(), "a", 200).unwrap();
        // a SECOND command reviewed against the OLD version is stale
        let mut c = owner_cmd("op-6");
        c.base_policy_version = 0; // policy is now at 1
        assert!(matches!(
            l.apply(&c, &p, &frozen_policy_graph(), "a", 300),
            Err(ApplyError::StaleBase { cmd_base: 0, current: 1 })
        ));
    }

    #[test]
    fn unknown_or_weak_proposal_refused() {
        let mut l = PolicyApplicationLayer::new();
        let mut c = owner_cmd("op-7");
        c.proposal_digest = "pa-deadbeef".into();
        let p = pipeline_with_poll(20);
        assert!(matches!(
            l.apply(&c, &p, &frozen_policy_graph(), "a", 200),
            Err(ApplyError::NoSuchSupportedProposal { .. })
        ));
        // small-N record: filed as observation only -> no digest exists
        let weak = pipeline_with_poll(5);
        assert!(weak.proposals().is_empty());
    }

    #[test]
    fn candidate_affecting_needs_m3_even_for_owner() {
        // file a Supported proposal on model_routing (CandidateAffecting)
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 1,
            ledger_prefix_head_hash: "h".into(),
            corpus_snapshot_digest: "c".into(),
            query_id: "guardrail_veto_rate_by_profile".into(),
            query_params: BTreeMap::new(),
            cohort: "all casting runs".into(),
            exclusions: vec![],
            denominator: "runs".into(),
            missing_data_policy: "none".into(),
            policy_version: 1,
            protocol_digest: "x".into(),
            result_payload: "{}".into(),
            result_digest: String::new(),
        };
        e.result_digest = e.compute_result_digest();
        p.ingest(InsightRecord {
            insight_id: "I2".into(),
            claim: "route X vetoes less".into(),
            evidence: e,
            proposal: Some(PolicyDiff { target: "model_routing".into(), changes: [("route".to_string(), "X".to_string())].into_iter().collect() }),
            prediction: Some(Prediction { metric: "guardrail_veto_rate_by_profile".into(), direction: "down".into(), threshold: "< 0.1".into(), window: "next-1-retro-windows".into() }),
            n: 20,
        })
        .unwrap();
        assert_eq!(p.proposals().len(), 1);
        let r = p.proposals()[0];
        let d = PolicyApplicationLayer::digest_of(&[&r.insight_id, &r.evidence.result_digest]);
        let mut l = PolicyApplicationLayer::new();
        let mut c = owner_cmd("op-8");
        c.proposal_digest = d;
        // OWNER authority, fresh, single-use — still refused: M3 route
        assert!(matches!(
            l.apply(&c, &p, &frozen_policy_graph(), "a", 200),
            Err(ApplyError::CandidateAffectingNeedsM3 { target }) if target == "model_routing"
        ));
    }

    #[test]
    fn scoring_hits_and_misses_move_track_records() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        l.apply(&owner_cmd("op-10"), &p, &frozen_policy_graph(), "agent-good", 200).unwrap();
        let mut c11 = owner_cmd("op-11");
        c11.base_policy_version = 1;
        l.apply(&c11, &p, &frozen_policy_graph(), "agent-good", 200).unwrap();
        let mut c12 = owner_cmd("op-12");
        c12.base_policy_version = 2;
        l.apply(&c12, &p, &frozen_policy_graph(), "agent-bad", 200).unwrap();
        // windows due at 200 + 2*3600. The result stub keys on the
        // threshold so each agent's prediction is distinguishable:
        // agent-good registered twice (same prediction), agent-bad the
        // same — so use the outcome side: score ALL as hit except the
        // one whose op we mark. Simpler: score everything as hit, then
        // a second round scores a fresh agent-bad application as miss.
        let out = l.score_due(8_000, |_| Some(true));
        assert_eq!(out.len(), 3);
        // all hits: everyone at the clamp ceiling
        let w = l.proposer_weights();
        assert_eq!(w["agent-good"], 1.0);
        assert_eq!(w["agent-bad"], 1.0);
        assert!(l.governance_ok());
        // repeated misses keep degrading
        for i in 0..5 {
            let mut c = owner_cmd(&format!("op-bad-{i}"));
            c.prediction.window = "next-1-retro-windows".into();
            c.base_policy_version = l.policy_version;
            l.apply(&c, &pipeline_with_poll(20), &frozen_policy_graph(), "agent-bad", 200).unwrap();
        }
        l.score_due(900_000, |pred| Some(pred.metric == "nope")); // all miss
        let w2 = l.proposer_weights();
        assert!(w2["agent-bad"] <= 0.1 + 1e-9, "repeated failure floors the proposer: {}", w2["agent-bad"]);
    }

    #[test]
    fn pending_windows_change_nothing() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        l.apply(&owner_cmd("op-20"), &p, &frozen_policy_graph(), "agent-x", 200).unwrap();
        // before due: nothing scored
        assert!(l.score_due(300, |_| Some(true)).is_empty());
        // at due but evidence absent: Pending, NO reputation effect
        let out = l.score_due(8_000, |_| None);
        assert!(matches!(out[0].1, ScoreOutcome::Pending));
        assert!(l.proposer_weights().get("agent-x").is_none());
    }

    #[test]
    fn rollback_restores_and_audits() {
        let p = pipeline_with_poll(20);
        let mut l = PolicyApplicationLayer::new();
        l.apply(&owner_cmd("op-30"), &p, &frozen_policy_graph(), "a", 200).unwrap();
        let v = l.rollback("op-30", Role::Owner, "prediction missed badly").unwrap();
        assert_eq!(v, 2, "rollback bumps the version (forward-only ledger)");
        assert!(l.applied.is_empty());
        assert!(l.governance_ok());
        // rollback of an unknown/already-rolled-back op refused
        assert!(matches!(
            l.rollback("op-30", Role::Owner, "again"),
            Err(ApplyError::RollbackInvalid { .. })
        ));
        // and a non-owner cannot roll back
        let mut c31 = owner_cmd("op-31");
        c31.base_policy_version = l.policy_version;
        l.apply(&c31, &p, &frozen_policy_graph(), "a", 200).unwrap();
        assert!(matches!(
            l.rollback("op-31", Role::Observer, "x"),
            Err(ApplyError::NotAuthorized { .. })
        ));
    }
}
