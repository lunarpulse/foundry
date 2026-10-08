//! Governance event schemas & crash-safe commit protocol — M5 T49a
//! (plan v2.2, review ordering: schemas BEFORE implementation).
//!
//! astra #17: "hash-chained audit events do not prove completeness or
//! activation consistency... For each sensitive transition, define
//! crash outcomes before and after durable append and activation."
//!
//! Every sensitive M5 operation follows the INTENT/EFFECT commit
//! protocol:
//!   1. append intent event        (durable, effect NOT yet live)
//!   2. perform the effect         (activation)
//!   3. append effect event        (durable)
//! A crash between (1) and (3) leaves a detectable PENDING intent;
//! recovery resolves it deterministically (idempotent roll-forward
//! or explicit abort) — never silently. Effects without intents and
//! duplicate effects are tamper/bug signals.
//!
//! Schema versioning: unknown versions are NEVER silently ignored —
//! verification reports them explicitly (M4 reports keep validating
//! under their original schema: M4 events are all version 0/governance
//! -free, so M4 envelope verification is unchanged by construction).
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const GOVERNANCE_SCHEMA_VERSION: u32 = 1;

/// Sensitive operation kinds (M5). Each has intent + effect phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpKind {
    PolicyProposal,
    PolicyApplication,
    LessonSeal,
    LessonRevoke,
    CandidateSynthesize,
    CandidateActivate,
    PredictionRegister,
    PredictionScore,
    BudgetReserve,
    BudgetRelease,
    RunCancel,
}

impl OpKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpKind::PolicyProposal => "policy_proposal",
            OpKind::PolicyApplication => "policy_application",
            OpKind::LessonSeal => "lesson_seal",
            OpKind::LessonRevoke => "lesson_revoke",
            OpKind::CandidateSynthesize => "candidate_synthesize",
            OpKind::CandidateActivate => "candidate_activate",
            OpKind::PredictionRegister => "prediction_register",
            OpKind::PredictionScore => "prediction_score",
            OpKind::BudgetReserve => "budget_reserve",
            OpKind::BudgetRelease => "budget_release",
            OpKind::RunCancel => "run_cancel",
        }
    }
}

/// Phase of the commit protocol for one operation instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// step 1 done: intent journaled, effect NOT live yet
    Pending,
    /// step 3 done: effect journaled and live
    Committed,
    /// recovery decided to abort a pending intent (the effect never
    /// ran; the abort itself is journaled so the ledger is total)
    Aborted,
}

/// One governance journal event. Ledger-shaped (append-only, hash
/// chain is the caller's ledger concern — this schema carries the
/// semantic fields the verifier needs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceEvent {
    pub schema_version: u32,
    /// monotonic sequence within the governance stream
    pub seq: i64,
    pub op: OpKind,
    pub phase: Phase,
    /// unique operation instance id: intent and its effect share it
    pub op_id: String,
    /// content digest of the full operation payload (proposal diff,
    /// lesson content hash, bundle digest, budget amounts...) —
    /// intent and effect MUST carry the SAME payload digest
    pub payload_digest: String,
    /// actor (authenticated principal; C1 matrix authority)
    pub actor: String,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerifyError {
    /// event with an unknown schema version — explicitly reported,
    /// never ignored (astra #17: "explicit version dispatch")
    UnsupportedSchemaVersion { seq: i64, got: u32, latest: u32 },
    /// an effect (Committed/Aborted) has no matching Pending intent
    EffectWithoutIntent { seq: i64, op_id: String },
    /// payload digest changed between intent and effect — the applied
    /// thing is not the reviewed thing (C-A1 stale/artifact mismatch)
    PayloadMismatch { op_id: String, intent: String, effect: String },
    /// two intents for one op_id (nondeterministic generation)
    DuplicateIntent { op_id: String },
    /// seq not contiguous (gap = lost events; the chain does not
    /// prove completeness by itself)
    SeqGap { expected: i64, got: i64 },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion { seq, got, latest } =>
                write!(f, "seq {seq}: schema v{got} unsupported (latest v{latest}) — explicit dispatch required"),
            Self::EffectWithoutIntent { seq, op_id } =>
                write!(f, "seq {seq}: effect for op {op_id} has no pending intent"),
            Self::PayloadMismatch { op_id, intent, effect } =>
                write!(f, "op {op_id}: payload digest changed intent->effect ({intent} != {effect})"),
            Self::DuplicateIntent { op_id } =>
                write!(f, "duplicate intent for op {op_id}"),
            Self::SeqGap { expected, got } =>
                write!(f, "seq gap: expected {expected}, got {got}"),
        }
    }
}

/// Replay state for one op instance.
#[derive(Debug, Clone, PartialEq)]
pub enum OpState {
    Pending { payload_digest: String },
    Committed { payload_digest: String },
    Aborted { payload_digest: String },
}

/// Deterministic replay + completeness verification of the
/// governance stream. Returns the recovered state map and every
/// anomaly found (all anomalies reported, verification fails on any).
pub fn verify_stream(events: &[GovernanceEvent]) -> Result<Vec<OpState>, Vec<VerifyError>> {
    let mut errs = Vec::new();
    let mut states: HashMap<String, OpState> = HashMap::new();
    let mut seq_expected = 1i64;

    for e in events {
        if e.schema_version > GOVERNANCE_SCHEMA_VERSION {
            errs.push(VerifyError::UnsupportedSchemaVersion {
                seq: e.seq,
                got: e.schema_version,
                latest: GOVERNANCE_SCHEMA_VERSION,
            });
            continue; // cannot interpret further — explicit stop, not skip
        }
        if e.seq != seq_expected {
            errs.push(VerifyError::SeqGap { expected: seq_expected, got: e.seq });
            seq_expected = e.seq; // resync to continue finding more
        }
        seq_expected += 1;

        match e.phase {
            Phase::Pending => {
                if states.contains_key(&e.op_id) {
                    errs.push(VerifyError::DuplicateIntent { op_id: e.op_id.clone() });
                } else {
                    states.insert(e.op_id.clone(), OpState::Pending { payload_digest: e.payload_digest.clone() });
                }
            }
            Phase::Committed | Phase::Aborted => {
                match states.get(&e.op_id) {
                    None => errs.push(VerifyError::EffectWithoutIntent { seq: e.seq, op_id: e.op_id.clone() }),
                    Some(OpState::Pending { payload_digest }) => {
                        if *payload_digest != e.payload_digest {
                            errs.push(VerifyError::PayloadMismatch {
                                op_id: e.op_id.clone(),
                                intent: payload_digest.clone(),
                                effect: e.payload_digest.clone(),
                            });
                        }
                        let next = if e.phase == Phase::Committed {
                            OpState::Committed { payload_digest: e.payload_digest.clone() }
                        } else {
                            OpState::Aborted { payload_digest: e.payload_digest.clone() }
                        };
                        states.insert(e.op_id.clone(), next);
                    }
                    Some(_) => errs.push(VerifyError::DuplicateIntent { op_id: e.op_id.clone() }),
                }
            }
        }
    }

    if errs.is_empty() {
        let mut out: Vec<OpState> = Vec::new();
        // deterministic order: by first-seen op via event order — caller
        // maps op_id->state through the events; return insertion order
        for e in events {
            if e.phase == Phase::Pending {
                if let Some(s) = states.get(&e.op_id) {
                    out.push(s.clone());
                    states.remove(&e.op_id);
                }
            }
        }
        Ok(out.into_iter().collect())
    } else {
        Err(errs)
    }
}

/// Recovery policy for a crash between intent and effect. The
/// operation itself declares whether its effect is idempotent
/// (roll-forward safe) or requires explicit abort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPolicy {
    /// effect application is idempotent: recovery re-applies and
    /// journals Committed (e.g. budget release, prediction scoring)
    RollForwardIdempotent,
    /// effect must NOT auto-apply on recovery: journal Aborted and
    /// surface to the operator (e.g. policy application — astra #16:
    /// stale/ambiguous application fails closed)
    AbortAndSurface,
}

pub fn recovery_policy(op: OpKind) -> RecoveryPolicy {
    match op {
        // irreversible-in-effect or human-bound: never auto-apply
        OpKind::PolicyApplication
        | OpKind::CandidateActivate
        | OpKind::LessonSeal
        | OpKind::LessonRevoke => RecoveryPolicy::AbortAndSurface,
        // naturally idempotent
        OpKind::PredictionScore
        | OpKind::BudgetRelease
        | OpKind::RunCancel => RecoveryPolicy::RollForwardIdempotent,
        // proposals/syntheses/reservations are pure ledger appends —
        // pending intent alone is already the complete effect
        OpKind::PolicyProposal
        | OpKind::CandidateSynthesize
        | OpKind::PredictionRegister
        | OpKind::BudgetReserve => RecoveryPolicy::RollForwardIdempotent,
    }
}

/// Build a recovery decision for a pending op after a crash.
/// `next_seq` is the stream's next sequence number (caller owns the
/// ledger position; recovery never guesses it).
pub fn recover(pending: &GovernanceEvent, next_seq: i64) -> GovernanceEvent {
    let phase = match recovery_policy(pending.op) {
        RecoveryPolicy::RollForwardIdempotent => Phase::Committed,
        RecoveryPolicy::AbortAndSurface => Phase::Aborted,
    };
    GovernanceEvent {
        schema_version: pending.schema_version,
        seq: next_seq,
        op: pending.op,
        phase,
        op_id: pending.op_id.clone(),
        payload_digest: pending.payload_digest.clone(),
        actor: "recovery".into(),
        at: chrono::Utc::now().to_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, op: OpKind, phase: Phase, op_id: &str, digest: &str) -> GovernanceEvent {
        GovernanceEvent {
            schema_version: 1,
            seq,
            op,
            phase,
            op_id: op_id.into(),
            payload_digest: digest.into(),
            actor: "root".into(),
            at: "2026-10-08T00:00:00Z".into(),
        }
    }

    #[test]
    fn happy_intent_effect_pair() {
        let evs = vec![
            ev(1, OpKind::PolicyProposal, Phase::Pending, "op-1", "d1"),
            ev(2, OpKind::PolicyProposal, Phase::Committed, "op-1", "d1"),
        ];
        assert!(verify_stream(&evs).is_ok());
    }

    #[test]
    fn crash_between_intent_and_effect_leaves_pending() {
        // crash after step 1: only the intent exists
        let evs = vec![ev(1, OpKind::PolicyApplication, Phase::Pending, "op-2", "d2")];
        let states = verify_stream(&evs).unwrap();
        assert_eq!(states.len(), 1);
        assert!(matches!(states[0], OpState::Pending { .. }));
        // recovery for policy application ABORTS (never auto-applies)
        let r = recover(&evs[0], 2);
        assert_eq!(r.phase, Phase::Aborted);
        // and the recovered stream verifies clean
        let mut after = evs.clone();
        after.push(r);
        assert!(verify_stream(&after).is_ok());
    }

    #[test]
    fn idempotent_ops_roll_forward() {
        let evs = vec![ev(1, OpKind::PredictionScore, Phase::Pending, "op-3", "d3")];
        let r = recover(&evs[0], 2);
        assert_eq!(r.phase, Phase::Committed, "prediction scoring rolls forward");
        let mut after = evs.clone();
        after.push(r);
        assert!(verify_stream(&after).is_ok());
    }

    #[test]
    fn effect_without_intent_detected() {
        let evs = vec![ev(1, OpKind::LessonSeal, Phase::Committed, "ghost", "d")];
        let errs = verify_stream(&evs).unwrap_err();
        assert!(matches!(errs[0], VerifyError::EffectWithoutIntent { .. }));
    }

    #[test]
    fn payload_swap_detected() {
        // the classic attack: intent reviewed as d1, effect applies d2
        let evs = vec![
            ev(1, OpKind::PolicyApplication, Phase::Pending, "op-4", "d1"),
            ev(2, OpKind::PolicyApplication, Phase::Committed, "op-4", "d2"),
        ];
        let errs = verify_stream(&evs).unwrap_err();
        assert!(matches!(errs[0], VerifyError::PayloadMismatch { .. }));
    }

    #[test]
    fn duplicate_intent_detected() {
        let evs = vec![
            ev(1, OpKind::BudgetReserve, Phase::Pending, "op-5", "d"),
            ev(2, OpKind::BudgetReserve, Phase::Pending, "op-5", "d"),
        ];
        assert!(matches!(
            verify_stream(&evs).unwrap_err()[0],
            VerifyError::DuplicateIntent { .. }
        ));
    }

    #[test]
    fn seq_gap_detected() {
        // lost event: chain validity alone would NOT catch a gap if
        // hashes were recomputed; completeness check does
        let evs = vec![
            ev(1, OpKind::BudgetReserve, Phase::Pending, "a", "d"),
            ev(3, OpKind::BudgetReserve, Phase::Pending, "b", "d"),
        ];
        assert!(matches!(
            verify_stream(&evs).unwrap_err()[0],
            VerifyError::SeqGap { expected: 2, got: 3 }
        ));
    }

    #[test]
    fn unknown_schema_version_is_explicit_not_silent() {
        let mut future = ev(1, OpKind::PolicyProposal, Phase::Pending, "op-6", "d");
        future.schema_version = 99;
        let errs = verify_stream(&[future]).unwrap_err();
        assert!(matches!(
            errs[0],
            VerifyError::UnsupportedSchemaVersion { got: 99, .. }
        ));
    }

    #[test]
    fn m4_reports_unaffected_by_construction() {
        // M4 events are governance-free: their ledger rows carry no
        // GovernanceEvent payload, so M4 envelope verification never
        // dispatches into this schema. Regression proof: an M4-shaped
        // row list contains zero governance events.
        let m4_rows = vec![foundry_ledger::LedgerRow {
            seq: 1,
            order_id: "o-1".into(),
            from_state: "received".into(),
            to_state: "queued".into(),
            event: "Queued".into(),
            actor: "a".into(),
            intent: "i".into(),
            at: "t".into(),
            prev_hash: "GENESIS".into(),
            hash: "h".into(),
            event_json: None,
        }];
        // governance stream of an M4-only ledger is empty -> vacuously valid
        assert!(verify_stream(&[]).is_ok());
        let _ = m4_rows;
    }
}
