//! Full command binding + field-level policy classification — M5.1
//! T54w (astra B5: "the demonstrated authorization digest does not
//! bind the proposed change").
//!
//! Two hardenings:
//!
//! 1. CANONICAL PROPOSAL DIGEST binds EVERYTHING the owner reviewed:
//!    { insight_id, evidence result digest, target, the FULL policy
//!    diff (field+value, sorted), the prediction, base policy
//!    version }. The substitution attack — same insight/evidence,
//!    altered diff or prediction — changes the digest, so the old
//!    authorization cannot promote the new content.
//!
//! 2. FIELD-LEVEL CLASSIFICATION: classifying only the outer target
//!    label is insufficient ("poll_interval" target with a
//!    "model_routing" field inside the diff would slip through).
//!    Every changed field must be classified in the field registry;
//!    an unclassified field fails closed, and a CandidateAffecting
//!    FIELD makes the whole proposal CandidateAffecting (M3 route),
//!    regardless of its outer target.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::evaluation_protocol::{PolicyClass, PolicyGraph};
use crate::policy_apply::{ApplyCommand, ApplyError};
use crate::retro::InsightPipeline;

// ---------------------------------------------------------------------------
// 1. Field registry: field name -> class. Unknown = fail closed.
// ---------------------------------------------------------------------------

pub fn field_class(field: &str) -> Option<PolicyClass> {
    Some(match field {
        // operational knobs
        "poll_interval_secs" | "max_retries" | "queue_depth" | "tmp_retention_days" => {
            PolicyClass::Operational
        }
        // candidate-affecting (worker behavior)
        "model_id" | "prompt_template" | "tool_allowlist" | "env_allowlist" | "seed"
        | "temperature" | "route" | "lesson_selection" => PolicyClass::CandidateAffecting,
        // protected governance
        "judging_policy"
        | "metric_definition"
        | "cohort_definition"
        | "weight_rule"
        | "evaluator_partition"
        | "source_history" => PolicyClass::ProtectedGovernance,
        _ => return None, // unclassified -> fail closed
    })
}

/// Classify a whole proposal by its FIELDS: the strongest (most
/// restricted) class wins. Unknown field = None (fail closed).
pub fn classify_diff(changes: &BTreeMap<String, String>) -> Option<PolicyClass> {
    let mut strongest: Option<PolicyClass> = None;
    for f in changes.keys() {
        let c = field_class(f)?; // unknown field: None propagates
        strongest = Some(match strongest {
            None => c,
            Some(cur) => {
                // ordering of restriction: ProtectedGovernance > CandidateAffecting > Operational
                let rank = |x: PolicyClass| match x {
                    PolicyClass::ProtectedGovernance => 2,
                    PolicyClass::CandidateAffecting => 1,
                    PolicyClass::Operational => 0,
                    PolicyClass::Unclassified => 3, // most restrictive
                };
                if rank(c) > rank(cur) { c } else { cur }
            }
        });
    }
    strongest
}

// ---------------------------------------------------------------------------
// 2. Canonical proposal digest over the COMPLETE reviewed content.
// ---------------------------------------------------------------------------

fn feed(h: &mut Sha256, part: &str) {
    h.update(&(part.len() as u64).to_be_bytes());
    h.update(part.as_bytes());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalContent {
    pub insight_id: String,
    pub evidence_result_digest: String,
    pub target: String,
    pub changes: BTreeMap<String, String>,
    pub prediction_metric: String,
    pub prediction_direction: String,
    pub prediction_threshold: String,
    pub prediction_window: String,
    pub base_policy_version: u32,
}

impl ProposalContent {
    /// The canonical digest — over EVERY field the owner reviewed.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"foundry-proposal-v2");
        feed(&mut h, &self.insight_id);
        feed(&mut h, &self.evidence_result_digest);
        feed(&mut h, &self.target);
        // sorted (BTreeMap iterates in order) field:value pairs
        for (k, v) in &self.changes {
            feed(&mut h, k);
            feed(&mut h, v);
        }
        feed(&mut h, &self.prediction_metric);
        feed(&mut h, &self.prediction_direction);
        feed(&mut h, &self.prediction_threshold);
        feed(&mut h, &self.prediction_window);
        h.update(&self.base_policy_version.to_be_bytes());
        format!("prop-{:.16x}", h.finalize())
    }
}

// ---------------------------------------------------------------------------
// 3. Hardened apply: full binding + field classification.
// ---------------------------------------------------------------------------

pub struct HardenedApply;

#[derive(Debug, Clone, PartialEq)]
pub enum HardenedError {
    /// astra's substitution test: command content != filed content
    ContentMismatch {
        detail: String,
    },
    /// unclassified field — fail closed
    UnclassifiedField {
        field: String,
    },
    /// a CandidateAffecting FIELD routes the whole proposal to M3
    FieldNeedsM3 {
        field: String,
    },
    Delegate(ApplyError),
}

impl HardenedApply {
    /// Compute the digest the owner's command MUST carry, from the
    /// filed proposal — complete content, not insight+evidence only.
    pub fn command_digest_for(
        pipeline: &InsightPipeline,
        insight_id: &str,
        base_policy_version: u32,
    ) -> Option<String> {
        let rec = pipeline
            .proposals()
            .into_iter()
            .find(|r| r.insight_id == insight_id)?;
        let prop = rec.proposal.as_ref()?;
        let pred = rec.prediction.as_ref()?;
        Some(
            ProposalContent {
                insight_id: rec.insight_id.clone(),
                evidence_result_digest: rec.evidence.result_digest.clone(),
                target: prop.target.clone(),
                changes: prop.changes.clone(),
                prediction_metric: pred.metric.clone(),
                prediction_direction: pred.direction.clone(),
                prediction_threshold: pred.threshold.clone(),
                prediction_window: pred.window.clone(),
                base_policy_version,
            }
            .digest(),
        )
    }

    /// Verify an ApplyCommand against the filed proposal: the command
    /// must bind the COMPLETE canonical digest of what it applies.
    pub fn verify_binding(
        pipeline: &InsightPipeline,
        cmd: &ApplyCommand,
        current_policy_version: u32,
    ) -> Result<(), HardenedError> {
        // locate the filed proposal the command claims
        let mut found: Option<&crate::retro::InsightRecord> = None;
        for r in pipeline.proposals() {
            let pred = r.prediction.as_ref().unwrap();
            if pred.metric == cmd.prediction.metric
                && pred.direction == cmd.prediction.direction
                && pred.threshold == cmd.prediction.threshold
                && pred.window == cmd.prediction.window
            {
                found = Some(r);
                break;
            }
        }
        let rec = found.ok_or_else(|| HardenedError::ContentMismatch {
            detail: "no filed proposal matches the command's prediction".into(),
        })?;
        let prop = rec.proposal.as_ref().unwrap();
        let expected = ProposalContent {
            insight_id: rec.insight_id.clone(),
            evidence_result_digest: rec.evidence.result_digest.clone(),
            target: prop.target.clone(),
            changes: prop.changes.clone(),
            prediction_metric: cmd.prediction.metric.clone(),
            prediction_direction: cmd.prediction.direction.clone(),
            prediction_threshold: cmd.prediction.threshold.clone(),
            prediction_window: cmd.prediction.window.clone(),
            base_policy_version: cmd.base_policy_version,
        }
        .digest();
        if expected != cmd.proposal_digest {
            return Err(HardenedError::ContentMismatch {
                detail: format!(
                    "command digest {} != canonical content digest {} — content substitution attempted",
                    cmd.proposal_digest, expected
                ),
            });
        }
        if cmd.base_policy_version != current_policy_version {
            return Err(HardenedError::Delegate(ApplyError::StaleBase {
                cmd_base: cmd.base_policy_version,
                current: current_policy_version,
            }));
        }
        // FIELD classification: every changed field must be classified
        for f in prop.changes.keys() {
            match field_class(f) {
                None => {
                    return Err(HardenedError::UnclassifiedField { field: f.clone() });
                }
                Some(PolicyClass::CandidateAffecting) => {
                    return Err(HardenedError::FieldNeedsM3 { field: f.clone() });
                }
                Some(PolicyClass::ProtectedGovernance) => {
                    return Err(HardenedError::ContentMismatch {
                        detail: format!(
                            "protected field {f} in a filed proposal — ingestion should have refused"
                        ),
                    });
                }
                Some(PolicyClass::Operational) | Some(PolicyClass::Unclassified) => {}
            }
        }
        let _ = classify_diff(&prop.changes); // None already handled per-field
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation_protocol::frozen_policy_graph;
    use crate::policy_apply::Role;
    use crate::retro::{EvidenceEnvelope, InsightRecord, PolicyDiff, Prediction, QueryCatalog};

    fn pipeline(changes: BTreeMap<String, String>, target: &str) -> InsightPipeline {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
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
                target: target.into(),
                changes,
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

    fn cmd_for(_p: &InsightPipeline, digest: String) -> ApplyCommand {
        ApplyCommand {
            op_id: "op-1".into(),
            approver: "owner".into(),
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

    fn ok_changes() -> BTreeMap<String, String> {
        [("max_retries".to_string(), "2".to_string())]
            .into_iter()
            .collect()
    }

    #[test]
    fn digest_binds_the_complete_content() {
        let p = pipeline(ok_changes(), "poll_interval");
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        // the command built FROM the filed content verifies
        HardenedApply::verify_binding(&p, &cmd_for(&p, d.clone()), 0).unwrap();
        // determinism
        assert_eq!(d, HardenedApply::command_digest_for(&p, "I1", 0).unwrap());
    }

    #[test]
    fn substitution_attack_refused() {
        // filed: max_retries=2. Attacker issues a command with the
        // ORIGINAL digest but the diff mutated to model_id.
        let honest = pipeline(ok_changes(), "poll_interval");
        let d = HardenedApply::command_digest_for(&honest, "I1", 0).unwrap();
        // a proposal with different content files under a different digest:
        let mut evil_changes = ok_changes();
        evil_changes.insert("model_id".to_string(), "attacker-model".to_string());
        let evil = pipeline(evil_changes.clone(), "poll_interval");
        // LAYER 1: an honest-digest command against mutated content is
        // refused at the DIGEST — the canonical binding fires first.
        let cmd = cmd_for(&evil, d.clone());
        let err = HardenedApply::verify_binding(&evil, &cmd, 0).unwrap_err();
        assert!(
            matches!(&err, HardenedError::ContentMismatch { detail } if detail.contains("substitution")),
            "layer 1: digest binding catches the swap: {err:?}"
        );
        // LAYER 2: even a command carrying the EVIL content's OWN correct
        // digest is refused on FIELD classification — a candidate-
        // affecting FIELD routes the whole proposal to M3 regardless
        // of its operational outer target.
        let evil_d = HardenedApply::command_digest_for(&evil, "I1", 0).unwrap();
        let err2 = HardenedApply::verify_binding(&evil, &cmd_for(&evil, evil_d), 0).unwrap_err();
        assert!(
            matches!(&err2, HardenedError::FieldNeedsM3 { field } if field == "model_id"),
            "layer 2: field classification catches what digest matching alone cannot: {err2:?}"
        );
        // and even an operational-value mutation changes the digest:
        let mut val_change = ok_changes();
        val_change.insert("max_retries".to_string(), "99".to_string());
        let val_p = pipeline(val_change, "poll_interval");
        let d2 = HardenedApply::command_digest_for(&val_p, "I1", 0).unwrap();
        assert_ne!(d, d2, "a changed VALUE is a different authorization");
        assert!(matches!(
            HardenedApply::verify_binding(&val_p, &cmd_for(&val_p, d), 0),
            Err(HardenedError::ContentMismatch { .. })
        ));
    }

    #[test]
    fn prediction_is_part_of_the_binding() {
        let p = pipeline(ok_changes(), "poll_interval");
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let mut cmd = cmd_for(&p, d);
        cmd.prediction.threshold = "<0.99".into(); // relaxed after review
        let err = HardenedApply::verify_binding(&p, &cmd, 0).unwrap_err();
        assert!(
            matches!(err, HardenedError::ContentMismatch { .. }),
            "the prediction is bound"
        );
    }

    #[test]
    fn base_version_is_part_of_the_binding() {
        let p = pipeline(ok_changes(), "poll_interval");
        let d0 = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let d1 = HardenedApply::command_digest_for(&p, "I1", 1).unwrap();
        assert_ne!(
            d0, d1,
            "commands reviewed against different versions are different authorizations"
        );
        // correct digest but stale version
        assert!(matches!(
            HardenedApply::verify_binding(&p, &cmd_for(&p, d0), 1),
            Err(HardenedError::Delegate(ApplyError::StaleBase { .. }))
        ));
    }

    #[test]
    fn unclassified_field_fails_closed() {
        let mut changes = ok_changes();
        changes.insert("brand_new_knob".to_string(), "1".to_string());
        let p = pipeline(changes, "poll_interval");
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        assert!(matches!(
            HardenedApply::verify_binding(&p, &cmd_for(&p, d), 0),
            Err(HardenedError::UnclassifiedField { field }) if field == "brand_new_knob"
        ));
    }

    #[test]
    fn protected_field_in_diff_refused() {
        let mut changes = ok_changes();
        changes.insert("weight_rule".to_string(), "attacker".to_string());
        // note: ingestion's gate-18 checks the TARGET, not fields —
        // this proposal files (target poll_interval); the hardened
        // layer catches the protected FIELD at binding time
        let p = pipeline(changes, "poll_interval");
        let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
        let err = HardenedApply::verify_binding(&p, &cmd_for(&p, d), 0).unwrap_err();
        assert!(
            matches!(err, HardenedError::ContentMismatch { detail } if detail.contains("protected field weight_rule"))
        );
    }

    #[test]
    fn classify_diff_takes_strongest() {
        let mut c: BTreeMap<String, String> = [("max_retries".to_string(), "2".to_string())]
            .into_iter()
            .collect();
        assert_eq!(classify_diff(&c), Some(PolicyClass::Operational));
        c.insert("model_id".to_string(), "m".to_string());
        assert_eq!(classify_diff(&c), Some(PolicyClass::CandidateAffecting));
        c.insert("judging_policy".to_string(), "x".to_string());
        assert_eq!(classify_diff(&c), Some(PolicyClass::ProtectedGovernance));
        c.insert("mystery".to_string(), "x".to_string());
        assert_eq!(
            classify_diff(&c),
            None,
            "unclassified poisons the whole diff"
        );
    }
}
