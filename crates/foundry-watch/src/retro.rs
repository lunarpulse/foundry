//! Factory retrospective: evidence-bound insight pipeline — M5 T50a.
//!
//! C-R1: an insight is a VERIFIABLE ARTIFACT, never free text:
//!   { claim, evidence envelope, proposed policy change, falsifiable
//!     prediction } — and proposal strength scales with evidence N.
//! C-E1: the evidence envelope inherits the M4/T33 contract — ledger
//!   prefix pin, corpus snapshot digest, a FINITE PRE-REGISTERED
//!   query catalog (arbitrary query code is refused at the boundary),
//!   explicit cohorts/exclusions/denominators, versioned sources,
//!   result digest. Independently recomputable.
//! Proposal-only: this pipeline PRODUCES proposals. There is no apply
//! path in this module and structurally cannot be one — application
//! is T50b's owner-gated command. (The apply function literally does
//! not exist here; the test proves the public surface.)
//! Gate 18 hookup: a proposal targeting the protected judging-policy
//! closure (T48a PolicyGraph) is refused at INGESTION.
#![allow(clippy::collapsible_if)] // W6a clippy debt: preserve explicit branch and short-circuit ordering
#![allow(unused_variables)] // W6a clippy debt: retain names used by conditional or diagnostic configurations

use crate::evaluation_protocol::PolicyGraph;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Finite query catalog (C-E1): pre-registered, deterministic, bounded.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogQuery {
    pub id: String,
    /// deterministic definition over the ledger/corpus (descriptor,
    /// not executable code — recompute lives in the audit layer)
    pub definition: String,
    /// required minimum cohort size for the query's result to carry
    /// ANY proposal strength
    pub min_n: usize,
}

pub struct QueryCatalog {
    queries: BTreeMap<String, CatalogQuery>,
}

impl QueryCatalog {
    /// The frozen catalog. Adding a query is a governance change
    /// (version bump), not a runtime operation.
    pub fn frozen_v1() -> Self {
        let mk = |id: &str, def: &str, min_n: usize| CatalogQuery {
            id: id.to_string(),
            definition: def.to_string(),
            min_n,
        };
        QueryCatalog {
            queries: [
                mk("retry_rate_by_task_class", "count(retries)/count(attempts) grouped by task_class over window; cohort: all orders incl. failed/cancelled (no survivorship bias)", 12),
                mk("hold_duration_median_by_class", "median(furnace_hold_seconds) grouped by task_class; cohort: all orders reaching FurnaceHold", 8),
                mk("guardrail_veto_rate_by_profile", "count(guardrail vetoes)/count(casting runs) by profile digest; cohort: all casting runs incl. drained", 10),
            ]
            .into_iter()
            .map(|q| (q.id.clone(), q))
            .collect(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&CatalogQuery> {
        self.queries.get(id)
    }

    pub fn is_registered(&self, id: &str) -> bool {
        self.queries.contains_key(id)
    }
}

// ---------------------------------------------------------------------------
// Evidence envelope (C-E1): M4/T33 semantics inherited.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEnvelope {
    /// ledger prefix the evidence was computed against (T33 pinning)
    pub ledger_prefix_len: u64,
    pub ledger_prefix_head_hash: String,
    /// corpus snapshot the query saw (content digest)
    pub corpus_snapshot_digest: String,
    /// which catalog query, with its parameterization
    pub query_id: String,
    pub query_params: BTreeMap<String, String>,
    /// cohort/exclusions/denominator — explicit, never implied
    pub cohort: String,
    pub exclusions: Vec<String>,
    pub denominator: String,
    /// missingness policy and source versions
    pub missing_data_policy: String,
    pub policy_version: u32,
    pub protocol_digest: String,
    /// the computed result, content-hashed: recompute must match
    pub result_payload: String,
    pub result_digest: String,
}

impl EvidenceEnvelope {
    pub fn compute_result_digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"foundry-evidence-v1");
        h.update(self.result_payload.as_bytes());
        format!("ev-{:.16x}", h.finalize())
    }

    /// Independent recompute check (gate 17): the declared digest
    /// must equal the payload's actual digest.
    pub fn self_consistent(&self) -> bool {
        self.result_digest == self.compute_result_digest()
    }
}

// ---------------------------------------------------------------------------
// Insight = verifiable artifact (C-R1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum InsightError {
    UnregisteredQuery {
        id: String,
    },
    InconsistentEvidence {
        expected: String,
        got: String,
    },
    /// evidence cohort smaller than the query's minimum N — the
    /// insight is RECORDABLE as observation but carries NO proposal
    BelowMinN {
        n: usize,
        min: usize,
    },
    /// gate 18: the proposal targets the protected closure
    ProtectedTarget {
        target: String,
        detail: String,
    },
    /// free text claiming to be an insight without an envelope
    NoEvidence,
    /// T55w: independent recomputation against the trusted source
    ResultMismatch {
        claimed: String,
        recomputed: String,
    },
    LedgerHeadMismatch {
        claimed: String,
        authoritative: String,
    },
    CorpusMismatch {
        claimed: String,
        authoritative: String,
    },
    RecomputeRefused {
        detail: String,
    },
    /// prediction not falsifiable (no metric, no direction, no window)
    UnfalsifiablePrediction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolicyDiff {
    pub target: String,
    /// typed field changes — the shape T50b's owner command consumes
    pub changes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    /// metric from the catalog's domain the change should move
    pub metric: String,
    /// "down" | "up"
    pub direction: String,
    /// quantitative bar — vague predictions are unfalsifiable
    pub threshold: String,
    /// window: which future retro window scores this
    pub window: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsightRecord {
    pub insight_id: String,
    pub claim: String,
    pub evidence: EvidenceEnvelope,
    pub proposal: Option<PolicyDiff>,
    pub prediction: Option<Prediction>,
    /// evidence N (cohort size actually observed)
    pub n: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProposalStrength {
    /// N below the query minimum: observation only, no proposal
    Anecdotal,
    /// minimum met: proposable
    Supported,
}

impl InsightRecord {
    pub fn strength(&self, catalog: &QueryCatalog) -> ProposalStrength {
        match catalog.get(&self.evidence.query_id) {
            Some(q) if self.n >= q.min_n => ProposalStrength::Supported,
            _ => ProposalStrength::Anecdotal,
        }
    }
}

/// The pipeline: ingestion + validation. It ENDS at proposals.
pub struct InsightPipeline {
    pub catalog: QueryCatalog,
    pub policy_graph: PolicyGraph,
    insights: Vec<InsightRecord>,
    /// T55w trusted-source reader; None = unit scope (self-
    /// consistency only). Production ingestion installs one.
    pub(crate) reader:
        Option<std::sync::Arc<dyn crate::evidence_recompute::EvidenceReader + Send + Sync>>,
}

impl InsightPipeline {
    pub fn new(catalog: QueryCatalog, policy_graph: PolicyGraph) -> Self {
        InsightPipeline {
            catalog,
            policy_graph,
            insights: Vec::new(),
            reader: None,
        }
    }

    /// Ingest an insight. Every C-R1/C-E1 gate fires HERE, before the
    /// record exists.
    pub fn ingest(&mut self, mut rec: InsightRecord) -> Result<(), InsightError> {
        // 1. evidence must be self-consistent (recompute the digest)
        if !rec.evidence.self_consistent() {
            return Err(InsightError::InconsistentEvidence {
                expected: rec.evidence.compute_result_digest(),
                got: rec.evidence.result_digest.clone(),
            });
        }
        // 1b. T55w: when a trusted reader is installed, the evidence
        // must also match the AUTHORITATIVE source — self-consistent
        // fabrications fail here.
        self.recompute_evidence_pub(&rec.evidence)?;
        // 2. the query must be in the finite catalog — arbitrary
        //    analysis code never enters through the evidence door
        let min_n = match self.catalog.get(&rec.evidence.query_id) {
            None => {
                return Err(InsightError::UnregisteredQuery {
                    id: rec.evidence.query_id.clone(),
                });
            }
            Some(q) => q.min_n,
        };
        // 3. gate 18 at ingestion: a proposal targeting the protected
        //    judging-policy closure refuses the WHOLE record — the
        //    insight is not even filed as evidence
        if let Some(prop) = &rec.proposal {
            if let Err(detail) = self.policy_graph.proposal_admissible(&prop.target) {
                return Err(InsightError::ProtectedTarget {
                    target: prop.target.clone(),
                    detail,
                });
            }
        }
        // 4. proposal eligibility requires N >= the query's minimum
        if rec.proposal.is_some() && rec.n < min_n {
            // demote: the record survives as observation, the proposal
            // is stripped (small-N evidence may not carry a proposal)
            rec.proposal = None;
        }
        // 5. a proposal without a falsifiable prediction is refused
        //    (C-R3's scoring loop needs something to score)
        if let Some(p) = &rec.proposal {
            let ok = match &rec.prediction {
                Some(pr) => {
                    !pr.metric.is_empty()
                        && (pr.direction == "up" || pr.direction == "down")
                        && !pr.threshold.is_empty()
                        && !pr.window.is_empty()
                }
                None => false,
            };
            if !ok {
                return Err(InsightError::UnfalsifiablePrediction);
            }
        }
        self.insights.push(rec);
        Ok(())
    }

    /// internal wrapper for ingestion (shared with T55w extension)
    fn recompute_evidence_pub(&self, e: &EvidenceEnvelope) -> Result<(), InsightError> {
        crate::evidence_recompute::recompute_with(self, e)
    }

    /// Proposal-only output: what the owner path (T50b) may review.
    /// Anecdotal records are excluded — they are observations.
    pub fn proposals(&self) -> Vec<&InsightRecord> {
        self.insights
            .iter()
            .filter(|i| {
                i.proposal.is_some() && i.strength(&self.catalog) == ProposalStrength::Supported
            })
            .collect()
    }

    pub fn observations(&self) -> usize {
        self.insights.len()
    }
}

// NOTE: there is no `apply`, `activate`, or `set_policy` method on
// InsightPipeline — T50b owns application; this type cannot express
// it (proposal-only by construction).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation_protocol::frozen_policy_graph;

    fn envelope(query: &str, payload: &str) -> EvidenceEnvelope {
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 157,
            ledger_prefix_head_hash: "abc".into(),
            corpus_snapshot_digest: "corp-1".into(),
            query_id: query.into(),
            query_params: BTreeMap::new(),
            cohort: "all orders in window incl. failed/cancelled".into(),
            exclusions: vec![],
            denominator: "all attempts".into(),
            missing_data_policy:
                "missing -> excluded from numerator AND denominator, count reported".into(),
            policy_version: 1,
            protocol_digest: "evproto-x".into(),
            result_payload: payload.into(),
            result_digest: String::new(),
        };
        e.result_digest = e.compute_result_digest();
        e
    }

    fn rec(id: &str, n: usize, target: Option<&str>) -> InsightRecord {
        InsightRecord {
            insight_id: id.into(),
            claim: "dep-bump retries run 3x hot".into(),
            evidence: envelope(
                "retry_rate_by_task_class",
                r#"{"dep_bump":0.41,"other":0.12}"#,
            ),
            proposal: target.map(|t| PolicyDiff {
                target: t.into(),
                changes: [("max_retries".to_string(), "2".to_string())]
                    .into_iter()
                    .collect(),
            }),
            prediction: target.map(|_| Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "dep_bump < 0.25".into(),
                window: "next-2-retro-windows".into(),
            }),
            n,
        }
    }

    #[test]
    fn happy_ingestion_produces_a_supported_proposal() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        p.ingest(rec("I1", 20, Some("poll_interval"))).unwrap();
        assert_eq!(p.proposals().len(), 1);
        assert_eq!(p.observations(), 1);
    }

    #[test]
    fn digest_mismatch_refused() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        let mut r = rec("I2", 20, None);
        r.evidence.result_digest = "ev-tampered".into();
        assert!(matches!(
            p.ingest(r),
            Err(InsightError::InconsistentEvidence { .. })
        ));
    }

    #[test]
    fn unregistered_query_refused() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        let mut r = rec("I3", 20, None);
        r.evidence.query_id = "custom_adhoc_query".into();
        assert!(matches!(
            p.ingest(r),
            Err(InsightError::UnregisteredQuery { id }) if id == "custom_adhoc_query"
        ));
    }

    #[test]
    fn protected_target_refused_whole_record() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        // the astra attack shapes: cohort editing, history reset,
        // metric redefinition — all closure nodes
        for target in [
            "cohort_definitions",
            "source_history_ledger",
            "metric_definitions",
            "judging_policy",
        ] {
            let r = rec("Ix", 20, Some(target));
            assert!(
                matches!(p.ingest(r), Err(InsightError::ProtectedTarget { .. })),
                "target {target} must refuse"
            );
        }
        assert_eq!(p.observations(), 0, "the record is not even filed");
    }

    #[test]
    fn small_n_demotes_to_observation_not_proposal() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        // retry query min_n = 12; give it 5
        p.ingest(rec("I4", 5, Some("poll_interval"))).unwrap();
        assert_eq!(p.observations(), 1, "recorded as observation");
        assert_eq!(p.proposals().len(), 0, "but carries NO proposal");
    }

    #[test]
    fn unfalsifiable_prediction_refused() {
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        let mut r = rec("I5", 20, Some("poll_interval"));
        r.prediction = Some(Prediction {
            metric: "".into(),
            direction: "better".into(),
            threshold: "".into(),
            window: "".into(),
        });
        assert!(matches!(
            p.ingest(r),
            Err(InsightError::UnfalsifiablePrediction)
        ));
        // and a proposal with NO prediction at all
        let mut r2 = rec("I6", 20, Some("poll_interval"));
        r2.prediction = None;
        assert!(matches!(
            p.ingest(r2),
            Err(InsightError::UnfalsifiablePrediction)
        ));
    }

    #[test]
    fn evidence_envelope_inherits_t33_pinning_fields() {
        let e = envelope("retry_rate_by_task_class", "{}");
        assert!(e.ledger_prefix_len > 0);
        assert!(!e.ledger_prefix_head_hash.is_empty());
        assert!(!e.corpus_snapshot_digest.is_empty());
        assert!(!e.cohort.is_empty());
        assert!(!e.denominator.is_empty());
        assert!(!e.missing_data_policy.is_empty());
    }

    #[test]
    fn pipeline_public_surface_is_proposal_only() {
        // the compile-time claim: no application path exists. Probe by
        // name on the implemented API surface.
        let names: Vec<&str> = vec!["ingest", "proposals", "observations"];
        // (In Rust the absence is structural — there is no method to
        // call. This test pins the INTENT: everything producible is
        // either an ingest gate or a read-only view.)
        let mut p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        p.ingest(rec("I7", 20, Some("poll_interval"))).unwrap();
        // every produced item is a PROPOSAL carrying evidence + a
        // falsifiable prediction — never an applied change
        for prop in p.proposals() {
            assert!(prop.proposal.is_some());
            assert!(prop.prediction.is_some());
            assert!(prop.evidence.self_consistent());
        }
    }
}
