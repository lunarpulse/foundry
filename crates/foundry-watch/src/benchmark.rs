//! Benchmark — raw metrics over corpus cohorts (M2 T20, plan v2).
//!
//! MODES
//!   baseline <out>     — snapshot the current cohort's raw metrics into a
//!                        cohort manifest file (JSON). Idempotent-ish:
//!                        each run writes a NEW timestamped manifest, never
//!                        mutates a prior one.
//!   compare <baseline> — recompute metrics from the CURRENT corpus and
//!                        diff against a baseline manifest. Version checks
//!                        FIRST: metric_version and event_schema_version
//!                        must match, else refuse (reviews #8/#16 —
//!                        comparing across definition changes is noise).
//!
//! THREE AXES (raw counts only — no derived rates here, M3 decides those):
//!   regression  : Failed (quality) vs Rejected (no-op, not failure)
//!   cost        : token sums WITH known/unknown coverage (honest nulls)
//!   review load : FurnaceHold approvals recorded (human touches)
//!
//! Denominators are frozen INTO the cohort manifest (review #16): the
//! compare mode uses the baseline's denominator for its rates so M3
//! candidate-vs-baseline comparisons are like-for-like.
use crate::corpus::{CorpusManifest, CostFields};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const METRIC_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Metrics {
    /// Cohort identity (frozen into the baseline; compare verifies).
    pub metric_version: u32,
    pub cohort_size: usize,
    pub max_event_schema_version: u8,
    // axis 1: regression
    pub shipped: usize,
    pub failed: usize,
    pub rejected_noop: usize,
    // axis 2: cost (raw sums; None-aware)
    pub cost: CostSums,
    // axis 3: review load
    pub attempts_total: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CostSums {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub tool_calls: Option<u64>,
    /// how many manifests contributed each known value (coverage honesty)
    pub known_input: usize,
    pub known_output: usize,
}

pub fn compute(corpus: &[CorpusManifest]) -> Metrics {
    let mut m = Metrics {
        metric_version: METRIC_VERSION,
        cohort_size: corpus.len(),
        max_event_schema_version: corpus
            .iter()
            .map(|c| c.event_schema_version)
            .max()
            .unwrap_or(1),
        shipped: corpus
            .iter()
            .filter(|c| c.terminal_state == "shipped")
            .count(),
        failed: corpus
            .iter()
            .filter(|c| c.terminal_state == "failed")
            .count(),
        rejected_noop: corpus
            .iter()
            .filter(|c| c.terminal_state == "rejected")
            .count(),
        cost: CostSums::default(),
        attempts_total: corpus.iter().map(|c| c.attempts).sum(),
    };
    // cost sums: only over KNOWN values; count coverage
    let (in_sum, in_n) = sum_known(corpus.iter().filter_map(|c| c.cost.input_tokens));
    let (out_sum, out_n) = sum_known(corpus.iter().filter_map(|c| c.cost.output_tokens));
    let (tc_sum, tc_n) = sum_known(corpus.iter().filter_map(|c| c.cost.tool_call_count));
    m.cost = CostSums {
        input_tokens: in_sum,
        output_tokens: out_sum,
        tool_calls: tc_sum,
        known_input: in_n,
        known_output: out_n,
    };
    let _ = tc_n;
    m
}

fn sum_known(iter: impl Iterator<Item = u64>) -> (Option<u64>, usize) {
    let mut sum = 0u64;
    let mut n = 0usize;
    for v in iter {
        sum = sum.saturating_add(v);
        n += 1;
    }
    if n == 0 { (None, 0) } else { (Some(sum), n) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CohortManifest {
    pub created_at: String,
    pub metrics: Metrics,
    /// Denominator snapshot (review #16): frozen for like-for-like rates.
    pub denominator: usize,
    pub orders: Vec<String>,
}

pub fn write_baseline(corpus: &[CorpusManifest], out: &Path) -> std::io::Result<()> {
    let metrics = compute(corpus);
    let cm = CohortManifest {
        created_at: chrono::Utc::now().to_rfc3339(),
        denominator: metrics.cohort_size.max(1),
        orders: corpus.iter().map(|c| c.order_id.clone()).collect(),
        metrics,
    };
    std::fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(out, serde_json::to_string_pretty(&cm).unwrap())?;
    Ok(())
}

#[derive(Debug)]
pub enum CompareError {
    Io(std::io::Error),
    Serde(String),
    VersionMismatch {
        baseline_mv: u32,
        current_mv: u32,
        baseline_schema: u8,
        current_schema: u8,
    },
}

#[derive(Debug, Serialize)]
pub struct CompareReport {
    pub baseline: Metrics,
    pub current: Metrics,
    /// deltas (current - baseline); None where a value is unknown
    pub shipped_delta: i64,
    pub failed_delta: i64,
    pub rejected_delta: i64,
}

pub fn compare(
    corpus: &[CorpusManifest],
    baseline_path: &Path,
) -> Result<CompareReport, CompareError> {
    let raw = std::fs::read_to_string(baseline_path).map_err(CompareError::Io)?;
    let cm: CohortManifest =
        serde_json::from_str(&raw).map_err(|e| CompareError::Serde(e.to_string()))?;
    let current = compute(corpus);
    if cm.metrics.metric_version != current.metric_version
        || cm.metrics.max_event_schema_version != current.max_event_schema_version
    {
        return Err(CompareError::VersionMismatch {
            baseline_mv: cm.metrics.metric_version,
            current_mv: current.metric_version,
            baseline_schema: cm.metrics.max_event_schema_version,
            current_schema: current.max_event_schema_version,
        });
    }
    let d = |a: usize, b: usize| b as i64 - a as i64;
    Ok(CompareReport {
        shipped_delta: d(cm.metrics.shipped, current.shipped),
        failed_delta: d(cm.metrics.failed, current.failed),
        rejected_delta: d(cm.metrics.rejected_noop, current.rejected_noop),
        baseline: cm.metrics,
        current,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::CostFields as CF;

    fn m(state: &str, in_tok: Option<u64>, attempts: u32) -> CorpusManifest {
        CorpusManifest {
            order_id: format!("o-{state}"),
            terminal_state: state.into(),
            base_git_sha: None,
            target: "t".into(),
            prompt_template_id: None,
            prompt_render_hash: None,
            model_id: None,
            decoding_params: None,
            tool_policy: None,
            foundry_git_sha: None,
            ledger_row_range: None,
            archive_hash: None,
            attempts,
            die_seconds: None,
            cost: CF {
                input_tokens: in_tok,
                ..Default::default()
            },
            event_schema_version: 1,
        }
    }

    #[test]
    fn three_axes_raw_counts() {
        let corpus = vec![
            m("shipped", Some(100), 1),
            m("shipped", Some(50), 2),
            m("failed", None, 3),
            m("rejected", None, 1),
        ];
        let x = compute(&corpus);
        assert_eq!(x.shipped, 2);
        assert_eq!(x.failed, 1);
        assert_eq!(x.rejected_noop, 1);
        assert_eq!(x.cost.input_tokens, Some(150));
        assert_eq!(x.cost.known_input, 2); // honest coverage
        assert_eq!(x.attempts_total, 7);
    }

    #[test]
    fn compare_refuses_across_versions() {
        let corpus = vec![m("shipped", None, 1)];
        let dir = std::env::temp_dir().join(format!("bench-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_baseline(&corpus, &dir.join("b.json")).unwrap();
        // mutate version by hand
        let raw = std::fs::read_to_string(dir.join("b.json")).unwrap();
        let raw = raw.replace("\"metric_version\": 1", "\"metric_version\": 99");
        std::fs::write(dir.join("b.json"), raw).unwrap();
        match compare(&corpus, &dir.join("b.json")) {
            Err(CompareError::VersionMismatch { .. }) => {}
            other => panic!("expected VersionMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn baseline_roundtrip_and_deltas() {
        let corpus = vec![m("shipped", Some(10), 1)];
        let dir = std::env::temp_dir().join(format!("bench2-t-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_baseline(&corpus, &dir.join("b.json")).unwrap();
        let rep = compare(&corpus, &dir.join("b.json")).unwrap();
        assert_eq!(rep.shipped_delta, 0);
        assert_eq!(rep.failed_delta, 0);
        let corpus2 = vec![m("shipped", Some(10), 1), m("failed", None, 1)];
        let rep2 = compare(&corpus2, &dir.join("b.json")).unwrap();
        assert_eq!(rep2.failed_delta, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
