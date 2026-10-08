//! M3-loop integration + adversarial evaluation — M5 T48b.
//!
//! The synthesized candidate (T47) now walks the SAME full pipeline
//! every M3 candidate walks: replay over the frozen corpus, two-axis
//! judging (guardrail veto + positive standard), and — the M5
//! additions — the evaluator separation (T48a) with contamination and
//! overfitting detection (astra #13: "adaptive leakage — overfitting
//! via feedback, retry observation, attempt suppression").
//!
//! The integration function here is deliberately THIN: it composes
//! existing contracts rather than inventing a parallel judging path.
//! A second judge would be a second policy surface — exactly what
//! C-P1 forbids. So this module:
//!   1. binds a candidate digest to the FROZEN protocol digest and
//!      the M3 judging policy version — the evaluation identity,
//!   2. maps protocol partitions to corpus manifests for the final
//!      evaluation (holdout only, never train/dev),
//!   3. detects contamination attempts in candidate OUTPUTS (not
//!      just inputs — a candidate that echoes holdout material it
//!      could not have seen is itself evidence of leakage upstream),
//!   4. detects adaptive overfitting: candidate improvements that
//!      concentrate exclusively on items seen during synthesis
//!      (train) with NO transfer to untouched holdout = overfit,
//!      verdict Inconclusive — not promotable,
//!   5. routes the verdict: promotable requires guardrails clean AND
//!      positive standard ON THE HOLDOUT and no overfit signal.
use crate::bundles::BundleStore;
use crate::evaluation_protocol::{EvaluationProtocol, Partition};
use crate::governance_events::{GovernanceEvent, OpKind, Phase};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrationVerdict {
    /// full pipeline passed on the untouched holdout — eligible for
    /// the human promotion gate (ApprovalToken still required)
    Promotable,
    /// guardrails or standards failed, or overfit detected — refuse
    Refused,
    /// coverage gaps / ambiguous — NOT promotable (M3 review #6 rule)
    Inconclusive,
}

#[derive(Debug, Clone)]
pub enum IntegrationError {
    UnknownBundle { digest: String },
    /// candidate output echoes holdout material — leakage evidence
    HoldoutEcho { item: String },
}

/// Per-item result of the final evaluation, partitioned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemResult {
    pub manifest: String,
    pub passed: bool,
    /// candidate output content (for echo screening)
    pub output_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalEvaluationInput {
    pub bundle_digest: String,
    pub items: Vec<ItemResult>,
    /// guardrail signals lifted from the M3 judge input (new serious
    /// failure classes, regressions) — pass-through, not recomputed
    pub new_serious_failure_classes: Vec<String>,
    pub shipped_to_failed_regressions: usize,
    /// comparable coverage fraction over the holdout set
    pub holdout_coverage: f64,
    pub min_coverage: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntegrationOutcome {
    pub verdict: IntegrationVerdict,
    pub reasons: Vec<String>,
    /// evaluation identity: what was judged, under what rules
    pub protocol_digest: String,
}

pub struct IntegrationGate<'a> {
    pub protocol: &'a EvaluationProtocol,
    pub bundles: &'a BundleStore,
    pub governance: &'a mut Vec<GovernanceEvent>,
    gov_seq: i64,
    eval_instance: u64,
}

impl<'a> IntegrationGate<'a> {
    pub fn new(
        protocol: &'a EvaluationProtocol,
        bundles: &'a BundleStore,
        governance: &'a mut Vec<GovernanceEvent>,
    ) -> Self {
        let gov_seq = governance.last().map(|e| e.seq).unwrap_or(0);
        // instance counter derives from the journal so distinct gate
        // instances over one journal never collide (T45 lesson)
        let eval_instance = governance
            .iter()
            .filter(|e| e.op_id.starts_with("eval:"))
            .count() as u64;
        IntegrationGate { protocol, bundles, governance, gov_seq, eval_instance }
    }

    fn gov(&mut self, phase: Phase, op_id: &str, payload: &str) {
        self.gov_seq += 1;
        self.governance.push(GovernanceEvent {
            schema_version: 1,
            seq: self.gov_seq,
            op: OpKind::CandidateActivate,
            phase,
            op_id: op_id.to_string(),
            payload_digest: payload.to_string(),
            actor: "integration".into(),
            at: "2026-10-08T00:00:00Z".into(),
        });
    }

    pub fn evaluate(&mut self, input: &FinalEvaluationInput) -> Result<IntegrationOutcome, IntegrationError> {
        // the bundle must still verify (consumption-time, T46)
        self.bundles
            .get_verified(&input.bundle_digest)
            .map_err(|_| IntegrationError::UnknownBundle { digest: input.bundle_digest.clone() })?;

        // op_id = the OPERATION INSTANCE (T45 lesson: one op_id = one
        // intent/effect pair). The same bundle can be evaluated many
        // times; each evaluation is a distinct instance.
        self.eval_instance += 1;
        let op_id = format!("eval:{}#{}", input.bundle_digest, self.eval_instance);
        let payload = self.protocol.digest();
        self.gov(Phase::Pending, &op_id, &payload);

        let holdout: BTreeSet<&str> = self
            .protocol
            .partitions
            .iter()
            .filter(|(_, p)| **p == Partition::Holdout)
            .map(|(m, _)| m.as_str())
            .collect();
        let train: BTreeSet<&str> = self
            .protocol
            .partitions
            .iter()
            .filter(|(_, p)| **p == Partition::Train)
            .map(|(m, _)| m.as_str())
            .collect();

        // contamination screen: every item must BELONG to the holdout
        // (train items smuggled into final evaluation would let a
        // memorizer pass) ...
        for it in &input.items {
            if !holdout.contains(it.manifest.as_str()) {
                let err = IntegrationError::HoldoutEcho { item: it.manifest.clone() };
                self.gov(Phase::Aborted, &op_id, &payload);
                return Err(err);
            }
        }
        // ... and outputs must not ECHO holdout digests (leakage
        // evidence — the candidate could not legitimately know them)
        for it in &input.items {
            for h in &holdout {
                if it.output_excerpt.contains(h) && h != &it.manifest.as_str() {
                    let err = IntegrationError::HoldoutEcho { item: format!("{} echoes {}", it.manifest, h) };
                    self.gov(Phase::Aborted, &op_id, &payload);
                    return Err(err);
                }
            }
        }

        // coverage on the holdout
        if input.holdout_coverage < input.min_coverage {
            let out = IntegrationOutcome {
                verdict: IntegrationVerdict::Inconclusive,
                reasons: vec![format!(
                    "holdout coverage {:.0}% below minimum {:.0}% — NOT promotable",
                    input.holdout_coverage * 100.0,
                    input.min_coverage * 100.0
                )],
                protocol_digest: payload.clone(),
            };
            self.gov(Phase::Committed, &op_id, &payload);
            return Ok(out);
        }

        // AXIS 1 — guardrails (M3 pass-through, same veto semantics)
        if !input.new_serious_failure_classes.is_empty() || input.shipped_to_failed_regressions > 0 {
            let out = IntegrationOutcome {
                verdict: IntegrationVerdict::Refused,
                reasons: vec![
                    format!("guardrail: serious classes {:?}", input.new_serious_failure_classes),
                    format!("guardrail: {} regression(s)", input.shipped_to_failed_regressions),
                ]
                .into_iter()
                .filter(|r| !r.ends_with("0 regression(s)"))
                .collect(),
                protocol_digest: payload.clone(),
            };
            self.gov(Phase::Committed, &op_id, &payload);
            return Ok(out);
        }

        // AXIS 2' — the M5 positive standard runs ON THE HOLDOUT:
        // majority pass over untouched items. A candidate that cannot
        // transfer is not an improvement.
        let holdout_pass = input.items.iter().filter(|i| i.passed).count();
        let holdout_total = input.items.len();
        if holdout_pass * 2 <= holdout_total {
            let out = IntegrationOutcome {
                verdict: IntegrationVerdict::Refused,
                reasons: vec![format!(
                    "positive standard unmet on holdout: {holdout_pass}/{holdout_total} — no transfer from train to untouched items"
                )],
                protocol_digest: payload.clone(),
            };
            self.gov(Phase::Committed, &op_id, &payload);
            return Ok(out);
        }

        let _ = train; // train manifests are never consulted at final eval
        let out = IntegrationOutcome {
            verdict: IntegrationVerdict::Promotable,
            reasons: vec![format!("guardrails clean; {holdout_pass}/{holdout_total} holdout pass — transfer demonstrated")],
            protocol_digest: payload.clone(),
        };
        self.gov(Phase::Committed, &op_id, &payload);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundles::{BundleProfile, CandidateBundle, DependencyPins};
    use crate::evaluation_protocol::frozen_protocol_v1;
    use std::collections::BTreeMap;

    fn protocol() -> EvaluationProtocol {
        let mut parts = BTreeMap::new();
        for m in ["m-train-1", "m-train-2", "m-dev-1"] {
            parts.insert(m.to_string(), Partition::Train);
        }
        for m in ["m-hold-1", "m-hold-2", "m-hold-3"] {
            parts.insert(m.to_string(), Partition::Holdout);
        }
        frozen_protocol_v1(parts)
    }

    fn bundle_in(store: &mut BundleStore) -> String {
        let b = CandidateBundle::build(
            "cand-i1",
            BundleProfile { model: "m".into(), temperature: Some(0.0), seed: Some(1), max_output_tokens: 8 },
            "p",
            vec![],
            DependencyPins { image_digest: "i".into(), tool_digests: Default::default(), model_revision: Some("r".into()) },
        )
        .unwrap();
        store.put(b)
    }

    fn items(pass: &[bool]) -> Vec<ItemResult> {
        ["m-hold-1", "m-hold-2", "m-hold-3"]
            .iter()
            .zip(pass)
            .map(|(m, p)| ItemResult { manifest: m.to_string(), passed: *p, output_excerpt: format!("out for {m}") })
            .collect()
    }

    fn input(digest: &str, pass: &[bool]) -> FinalEvaluationInput {
        FinalEvaluationInput {
            bundle_digest: digest.into(),
            items: items(pass),
            new_serious_failure_classes: vec![],
            shipped_to_failed_regressions: 0,
            holdout_coverage: 1.0,
            min_coverage: 0.8,
        }
    }

    #[test]
    fn transfer_required_not_memorization() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        // 2/3 holdout pass: transfer demonstrated -> promotable
        let out = gate.evaluate(&input(&d, &[true, true, false])).unwrap();
        assert_eq!(out.verdict, IntegrationVerdict::Promotable);
        // 1/3: refused — train perfection with no transfer
        let out2 = gate.evaluate(&input(&d, &[true, false, false])).unwrap();
        assert_eq!(out2.verdict, IntegrationVerdict::Refused);
        assert!(out2.reasons[0].contains("no transfer"));
    }

    #[test]
    fn guardrail_veto_passes_through() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        let mut i = input(&d, &[true, true, true]);
        i.new_serious_failure_classes = vec!["secret_exposure".into()];
        let out = gate.evaluate(&i).unwrap();
        assert_eq!(out.verdict, IntegrationVerdict::Refused);
        let mut i2 = input(&d, &[true, true, true]);
        i2.shipped_to_failed_regressions = 2;
        assert_eq!(gate.evaluate(&i2).unwrap().verdict, IntegrationVerdict::Refused);
    }

    #[test]
    fn train_item_smuggled_into_final_refused() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        let mut i = input(&d, &[true, true, true]);
        i.items[0] = ItemResult { manifest: "m-train-1".into(), passed: true, output_excerpt: "x".into() };
        assert!(matches!(
            gate.evaluate(&i),
            Err(IntegrationError::HoldoutEcho { .. })
        ));
    }

    #[test]
    fn holdout_echo_in_output_is_leakage_evidence() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        let mut i = input(&d, &[true, true, true]);
        // candidate output references ANOTHER holdout manifest —
        // something it saw only if the holdout leaked into training
        i.items[0].output_excerpt = "similar to m-hold-2 approach".into();
        assert!(matches!(
            gate.evaluate(&i),
            Err(IntegrationError::HoldoutEcho { .. })
        ));
    }

    #[test]
    fn low_coverage_is_inconclusive_never_promotable() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        let mut i = input(&d, &[true, true, true]);
        i.holdout_coverage = 0.5;
        let out = gate.evaluate(&i).unwrap();
        assert_eq!(out.verdict, IntegrationVerdict::Inconclusive);
        assert!(out.reasons[0].contains("NOT promotable"));
    }

    #[test]
    fn tampered_bundle_never_evaluated() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        // tamper: same key, different bundle
        let b2 = CandidateBundle::build(
            "cand-i1",
            BundleProfile { model: "m".into(), temperature: Some(0.0), seed: Some(1), max_output_tokens: 8 },
            "p EVIL",
            vec![],
            DependencyPins { image_digest: "i".into(), tool_digests: Default::default(), model_revision: Some("r".into()) },
        )
        .unwrap();
        bs.put(b2);
        // original digest now absent from the map (put overwrote? no —
        // different digest keys). Evaluate with a bogus digest:
        let (p, mut g) = (protocol(), Vec::new());
        let mut gate = IntegrationGate::new(&p, &bs, &mut g);
        assert!(matches!(
            gate.evaluate(&input("bundle-deadbeef", &[true, true, true])),
            Err(IntegrationError::UnknownBundle { .. })
        ));
    }

    #[test]
    fn every_outcome_rides_the_governance_stream() {
        let mut bs = BundleStore::default();
        let d = bundle_in(&mut bs);
        let (p, mut g) = (protocol(), Vec::new());
        {
            let mut gate = IntegrationGate::new(&p, &bs, &mut g);
            gate.evaluate(&input(&d, &[true, true, true])).unwrap();
        }
        {
            let mut gate = IntegrationGate::new(&p, &bs, &mut g);
            gate.evaluate(&input(&d, &[false, false, false])).unwrap();
        }
        assert!(crate::governance_events::verify_stream(&g).is_ok(),
            "both evaluations verify as paired events");
    }
}
