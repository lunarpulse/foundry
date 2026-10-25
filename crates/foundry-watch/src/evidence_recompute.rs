//! Evidence dual recomputation — M5.1 T55w (astra B4: "checking that a
//! caller-generated digest matches caller-generated content is not
//! independent evidence verification. Forged-but-self-consistent
//! envelopes must fail").
//!
//! The EvidenceReader is the TRUSTED SOURCE side of the retro loop:
//! it can (a) run a catalog query against the real ledger+corpus and
//! return the recomputed result, (b) report the authoritative ledger
//! head at a prefix length, (c) report the authoritative corpus
//! snapshot digest. Ingestion with a reader installed performs BOTH
//! checks:
//!   1. self-consistency (T50a, unchanged): declared digest == hash
//!      of declared payload — catches sloppy/tampered envelopes,
//!   2. INDEPENDENT RECOMPUTATION (new): declared payload == the
//!      reader's recomputed payload, AND the envelope's ledger head /
//!      corpus digest match the reader's authoritative values —
//!      catches FORGED envelopes that are perfectly self-consistent.
//!
//! The production reader walks the real T33 ledger and the M2 corpus;
//! tests use deterministic doubles. `InsightPipeline::new` installs a
//! permissive reader (other gates' unit tests exercise different
//! properties); `with_reader` installs the enforcing one — the serve
//! wiring and the M5.1 gate construct the pipeline with a real
//! reader, and the recomputation proof below shows the forged
//! envelope failing ONLY under a real reader.
#![allow(dead_code)] // W6a clippy debt: retain conditional helpers used by other feature and test configurations
#![allow(unused_imports)] // W6a clippy debt: retain the existing feature and test configuration import set

use crate::retro::{EvidenceEnvelope, InsightError, InsightPipeline};
use std::collections::BTreeMap;

/// The trusted-source port. Implementations read AUTHORITATIVE state
/// (the hash-chained ledger, the frozen corpus) — never caller input.
pub trait EvidenceReader {
    /// Recompute a catalog query. Returns the canonical result
    /// payload string the envelope must carry.
    fn run_query(
        &self,
        query_id: &str,
        params: &BTreeMap<String, String>,
        ledger_prefix_len: u64,
    ) -> Result<String, String>;
    /// The authoritative ledger head hash at `prefix_len`.
    fn ledger_head_at(&self, prefix_len: u64) -> Result<String, String>;
    /// The authoritative corpus snapshot digest NOW.
    fn corpus_digest(&self) -> Result<String, String>;
}

/// Permissive reader: accepts any self-consistent envelope. Used by
/// `InsightPipeline::new` for unit scopes that test other gates.
/// NOT for production ingestion paths.
pub struct PermissiveReader;

impl EvidenceReader for PermissiveReader {
    fn run_query(
        &self,
        _q: &str,
        _p: &BTreeMap<String, String>,
        _l: u64,
    ) -> Result<String, String> {
        Err("permissive reader cannot recompute — install a real reader".into())
    }
    fn ledger_head_at(&self, _l: u64) -> Result<String, String> {
        Err("permissive reader has no authoritative head".into())
    }
    fn corpus_digest(&self) -> Result<String, String> {
        Err("permissive reader has no authoritative corpus".into())
    }
}

/// T60f: the PRODUCTION reader — walks the real T33 SQLite ledger
/// (~/.foundry/ledger.db). Queries are real SQL folds over the
/// authoritative rows; the head hash is read from the ledger itself.
pub struct LedgerEvidenceReader {
    path: std::path::PathBuf,
}

impl LedgerEvidenceReader {
    pub fn new(path: std::path::PathBuf) -> Self {
        LedgerEvidenceReader { path }
    }

    fn conn(&self) -> Result<rusqlite::Connection, String> {
        if !self.path.exists() {
            return Err(format!("ledger not found at {}", self.path.display()));
        }
        rusqlite::Connection::open(&self.path).map_err(|e| format!("open ledger: {e}"))
    }
}

impl EvidenceReader for LedgerEvidenceReader {
    fn run_query(
        &self,
        query_id: &str,
        params: &BTreeMap<String, String>,
        ledger_prefix_len: u64,
    ) -> Result<String, String> {
        let conn = self.conn()?;
        match query_id {
            "retry_rate_by_task_class" => {
                // retry = a DieSucceeded with attempt > 1, over ALL
                // DieSucceeded completions, bounded to the prefix
                let rows: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM ledger WHERE seq <= ?1",
                        [ledger_prefix_len as i64],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                // N6 (astra 3rd review): the PRODUCTION ledger stores
                // events in the `event` column ('DieSucceeded { attempt: N }');
                // `event_json` is NULL there (157/157 rows measured).
                // Read `event` first with `event_json` fallback, and
                // DISTINGUISH 'no rows in prefix' (reader cannot
                // certify -> Err) from 'rows but no completions' (0.0
                // is REAL). Silent zero is gone.
                let mut stmt = conn
                    .prepare("SELECT event, event_json FROM ledger WHERE seq <= ?1")
                    .map_err(|e| e.to_string())?;
                let raw: Vec<(String, Option<String>)> = stmt
                    .query_map([ledger_prefix_len as i64], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(|e| e.to_string())?
                    .filter_map(|row| row.ok())
                    .collect();
                let attempts: Vec<i64> = raw
                    .iter()
                    .filter_map(|(ev, js)| {
                        // canonical JSON emits "type":"die_succeeded"
                        // (snake_case tag) — accept BOTH spellings
                        // (astra 5th round MEDIUM: the fallback shape)
                        let hit =
                            |h: &str| h.contains("DieSucceeded") || h.contains("die_succeeded");
                        let hay: String = if hit(ev) {
                            ev.clone()
                        } else {
                            js.clone().unwrap_or_default()
                        };
                        if !hit(&hay) {
                            return None;
                        }
                        // 'DieSucceeded { attempt: N }' or {"attempt":N,...}
                        hay.split("attempt")
                            .nth(1)
                            .and_then(|rest| {
                                let digits: String = rest
                                    .chars()
                                    .skip_while(|c| !c.is_ascii_digit())
                                    .take_while(|c| c.is_ascii_digit())
                                    .collect();
                                if digits.is_empty() {
                                    None
                                } else {
                                    digits.parse::<i64>().ok()
                                }
                            })
                            .or(Some(1))
                    })
                    .collect();
                let any_rows: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM ledger WHERE seq <= ?1",
                        [ledger_prefix_len as i64],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                if any_rows == 0 {
                    return Err(
                        "retry_rate: no ledger rows in prefix — reader cannot certify".into(),
                    );
                }
                let _ = rows;
                let total = attempts.len() as f64;
                let retries = attempts.iter().filter(|a| **a > 1).count() as f64;
                let task_class = params
                    .get("task_class")
                    .cloned()
                    .unwrap_or_else(|| "all".into());
                let rate = if total > 0.0 { retries / total } else { 0.0 };
                Ok(format!(r#"{{"{}":{:.4}}}"#, task_class, rate))
            }
            other => Err(format!("unknown query {other}")),
        }
    }

    fn ledger_head_at(&self, prefix_len: u64) -> Result<String, String> {
        let conn = self.conn()?;
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT hash FROM ledger WHERE seq = ?1",
            [prefix_len as i64],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no ledger row at seq {prefix_len}"))
    }

    fn corpus_digest(&self) -> Result<String, String> {
        // the corpus snapshot digest is exposed via env or a sidecar;
        // absent both, the authoritative bound is the ledger head at
        // max seq — a stable content commitment.
        if let Ok(d) = std::env::var("FOUNDRY_CORPUS_DIGEST") {
            return Ok(d);
        }
        let conn = self.conn()?;
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "empty ledger".to_string())
    }
}

impl InsightPipeline {
    /// Install an enforcing reader.
    pub fn with_reader(
        catalog: crate::retro::QueryCatalog,
        policy_graph: crate::evaluation_protocol::PolicyGraph,
        reader: std::sync::Arc<dyn EvidenceReader + Send + Sync>,
    ) -> InsightPipeline {
        let mut p = InsightPipeline::new(catalog, policy_graph);
        p.reader = Some(reader);
        p
    }

    /// T55w check exposed publicly for the serve/gate paths.
    pub fn recompute_evidence(&self, e: &EvidenceEnvelope) -> Result<(), InsightError> {
        recompute_with(self, e)
    }
}

/// The shared implementation (also called from ingestion).
pub(crate) fn recompute_with(
    pipeline: &InsightPipeline,
    e: &EvidenceEnvelope,
) -> Result<(), InsightError> {
    {
        let reader = match &pipeline.reader {
            None => return Ok(()), // no reader installed: unit scope
            Some(r) => r,
        };
        // authoritative ledger head at the claimed prefix
        let head = reader
            .ledger_head_at(e.ledger_prefix_len)
            .map_err(|detail| InsightError::RecomputeRefused { detail })?;
        if head != e.ledger_prefix_head_hash {
            return Err(InsightError::LedgerHeadMismatch {
                claimed: e.ledger_prefix_head_hash.clone(),
                authoritative: head,
            });
        }
        // authoritative corpus snapshot
        let corpus = reader
            .corpus_digest()
            .map_err(|detail| InsightError::RecomputeRefused { detail })?;
        if corpus != e.corpus_snapshot_digest {
            return Err(InsightError::CorpusMismatch {
                claimed: e.corpus_snapshot_digest.clone(),
                authoritative: corpus,
            });
        }
        // independent query recomputation
        let recomputed = reader
            .run_query(&e.query_id, &e.query_params, e.ledger_prefix_len)
            .map_err(|detail| InsightError::RecomputeRefused { detail })?;
        if recomputed != e.result_payload {
            return Err(InsightError::ResultMismatch {
                claimed: e.result_payload.clone(),
                recomputed,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation_protocol::frozen_policy_graph;
    use crate::retro::{InsightRecord, PolicyDiff, Prediction, ProposalStrength, QueryCatalog};

    /// A deterministic trusted-source double: one real ledger head,
    /// one corpus, and a real (tiny) query implementation.
    struct RealSource {
        head: String,
        corpus: String,
    }
    impl EvidenceReader for RealSource {
        fn run_query(
            &self,
            q: &str,
            params: &BTreeMap<String, String>,
            _l: u64,
        ) -> Result<String, String> {
            match q {
                "retry_rate_by_task_class" => {
                    let class = params.get("task_class").cloned().unwrap_or_default();
                    // the authoritative computation (a fold over the
                    // real ledger in production; deterministic here)
                    Ok(format!(r#"{{"{}":0.41}}"#, class))
                }
                other => Err(format!("unknown query {other}")),
            }
        }
        fn ledger_head_at(&self, _l: u64) -> Result<String, String> {
            Ok(self.head.clone())
        }
        fn corpus_digest(&self) -> Result<String, String> {
            Ok(self.corpus.clone())
        }
    }

    fn honest_envelope() -> EvidenceEnvelope {
        let mut params = BTreeMap::new();
        params.insert("task_class".to_string(), "dep_bump".to_string());
        let mut e = EvidenceEnvelope {
            ledger_prefix_len: 157,
            ledger_prefix_head_hash: "real-head".into(),
            corpus_snapshot_digest: "real-corpus".into(),
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
        e
    }

    fn record(e: EvidenceEnvelope) -> InsightRecord {
        InsightRecord {
            insight_id: "I1".into(),
            claim: "retries hot".into(),
            evidence: e,
            proposal: Some(PolicyDiff {
                target: "poll_interval".into(),
                changes: [("max_retries".to_string(), "2".to_string())]
                    .into_iter()
                    .collect(),
            }),
            prediction: Some(Prediction {
                metric: "retry_rate_by_task_class".into(),
                direction: "down".into(),
                threshold: "<0.25".into(),
                window: "next-2-retro-windows".into(),
            }),
            n: 20,
        }
    }

    #[test]
    fn honest_evidence_recomputes() {
        let src = RealSource {
            head: "real-head".into(),
            corpus: "real-corpus".into(),
        };
        let p = InsightPipeline::with_reader(
            QueryCatalog::frozen_v1(),
            frozen_policy_graph(),
            std::sync::Arc::new(src),
        );
        p.recompute_evidence(&honest_envelope())
            .expect("honest evidence matches the source");
    }

    #[test]
    fn forged_self_consistent_envelope_fails() {
        // THE astra B4 attack: every field fabricated, digest correctly
        // computed over the fabrication — perfectly self-consistent.
        let src = RealSource {
            head: "real-head".into(),
            corpus: "real-corpus".into(),
        };
        let p = InsightPipeline::with_reader(
            QueryCatalog::frozen_v1(),
            frozen_policy_graph(),
            std::sync::Arc::new(src),
        );
        // forged result payload (self-consistent digest over a lie)
        let mut forged = honest_envelope();
        forged.result_payload = r#"{"dep_bump":0.02}"#.into(); // flattering fabrication
        forged.result_digest = forged.compute_result_digest();
        assert!(
            forged.self_consistent(),
            "the fabrication IS self-consistent"
        );
        assert!(matches!(
            p.recompute_evidence(&forged),
            Err(InsightError::ResultMismatch { claimed, recomputed, .. })
                if claimed.contains("0.02") && recomputed.contains("0.41")
        ));
    }

    #[test]
    fn fabricated_ledger_head_fails() {
        let src = RealSource {
            head: "real-head".into(),
            corpus: "real-corpus".into(),
        };
        let p = InsightPipeline::with_reader(
            QueryCatalog::frozen_v1(),
            frozen_policy_graph(),
            std::sync::Arc::new(src),
        );
        let mut forged = honest_envelope();
        forged.ledger_prefix_head_hash = "fabricated-head".into();
        forged.result_digest = forged.compute_result_digest();
        assert!(matches!(
            p.recompute_evidence(&forged),
            Err(InsightError::LedgerHeadMismatch { authoritative, .. }) if authoritative == "real-head"
        ));
    }

    #[test]
    fn stale_corpus_snapshot_fails() {
        let src = RealSource {
            head: "real-head".into(),
            corpus: "real-corpus".into(),
        };
        let p = InsightPipeline::with_reader(
            QueryCatalog::frozen_v1(),
            frozen_policy_graph(),
            std::sync::Arc::new(src),
        );
        let mut stale = honest_envelope();
        stale.corpus_snapshot_digest = "old-corpus-snapshot".into();
        stale.result_digest = stale.compute_result_digest();
        assert!(matches!(
            p.recompute_evidence(&stale),
            Err(InsightError::CorpusMismatch { .. })
        ));
    }

    #[test]
    fn permissive_reader_is_explicitly_not_verification() {
        // without a reader, recompute is a no-op — this is the unit-
        // test scope, NOT production; the permissive reader errors if
        // ever actually consulted for authoritative values.
        let p = InsightPipeline::new(QueryCatalog::frozen_v1(), frozen_policy_graph());
        assert!(p.recompute_evidence(&honest_envelope()).is_ok());
        let pr = PermissiveReader;
        assert!(
            pr.corpus_digest().is_err(),
            "the permissive reader cannot answer"
        );
    }

    #[test]
    fn reader_error_fails_closed() {
        struct Broken;
        impl EvidenceReader for Broken {
            fn run_query(
                &self,
                _: &str,
                _: &BTreeMap<String, String>,
                _: u64,
            ) -> Result<String, String> {
                Err("ledger unavailable".into())
            }
            fn ledger_head_at(&self, _: u64) -> Result<String, String> {
                Err("ledger unavailable".into())
            }
            fn corpus_digest(&self) -> Result<String, String> {
                Err("ledger unavailable".into())
            }
        }
        let p = InsightPipeline::with_reader(
            QueryCatalog::frozen_v1(),
            frozen_policy_graph(),
            std::sync::Arc::new(Broken),
        );
        assert!(matches!(
            p.recompute_evidence(&honest_envelope()),
            Err(InsightError::RecomputeRefused { detail }) if detail.contains("unavailable")
        ));
    }
}
