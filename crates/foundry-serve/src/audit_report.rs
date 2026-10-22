//! Audit reporting — M4 T33 (plan v2; review findings #11/#12).
//!
//! Verifiable, not "tamper-proof" (terminology per review #11):
//! a report is an EVIDENCE ENVELOPE binding (a) the exact ledger
//! prefix it covers, (b) the generator version, (c) the metric
//! definitions, (d) a digest of its own content. Verification =
//! prefix chain re-verification + DETERMINISTIC RECOMPUTE of the
//! content from that prefix. Legitimate extension (the ledger grew
//! after the report) is distinguished from tampering.
//!
//! Metric definitions (review #12, stated IN the report):
//! - state counts are AS-OF the prefix head (kanban semantics)
//! - terminal counts are per-order first-reach in-window — retries
//!   cannot double-count
//! - cost includes reasoning tokens; coverage gaps disclosed,
//!   never silently zero-filled ("unavailable != 0")
use foundry_ledger::LedgerRow;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const GENERATOR_VERSION: u32 = 1;

/// ProductAtomicV1 source inventory. This is deliberately separate from the
/// v1 metric envelope: only a verified source snapshot is marked available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProductAuditSourceV2 {
    pub source: String,
    pub availability: String,
    pub identity: Option<String>,
    pub schema: Option<String>,
    pub prefix_sequence: Option<i64>,
    pub head_hash: Option<String>,
    pub snapshot_digest: Option<String>,
    pub cutoff: Option<String>,
    pub missing_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProductAuditReportV2 {
    pub schema_version: u32,
    pub generated_at: String,
    pub sources: Vec<ProductAuditSourceV2>,
}

/// Operational inventory for a ProductAtomicV1 scoped projection. It is not
/// an organization-wide security audit and carries no global S1 coordinates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopedProductAuditSourceV3 {
    pub source: String,
    pub availability: String,
    pub schema: Option<String>,
    pub projection_digest: Option<String>,
    pub coverage: Option<String>,
    pub missing_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopedProductAuditReportV3 {
    pub schema_version: u32,
    pub generated_at: String,
    pub projection: crate::privacy::ScopedProductExportDocument,
    pub sources: Vec<ScopedProductAuditSourceV3>,
}

pub fn scoped_product_inventory_v3(
    snapshot: &foundry_ledger::ScopedEvidenceSnapshot,
    generated_at: chrono::DateTime<chrono::Utc>,
    context: &str,
    key: &crate::privacy::ExportKey,
) -> Result<ScopedProductAuditReportV3, String> {
    let projection = crate::privacy::scoped_product_export(snapshot, context, key)?;
    let availability = if projection.source_coverage_complete && !projection.has_more {
        "available"
    } else {
        "partial"
    };
    let mut sources = vec![ScopedProductAuditSourceV3 {
        source: "S1".into(),
        availability: availability.into(),
        schema: Some("ProductAtomicV1 scoped lifecycle projection".into()),
        projection_digest: Some(projection.projected_digest.clone()),
        coverage: Some(projection.coverage.clone()),
        missing_reason: if projection.has_more {
            Some("authorized order selection is truncated to this page".into())
        } else if !projection.source_coverage_complete {
            Some("some selected S1 rows lack verified lifecycle scope provenance".into())
        } else {
            None
        },
    }];
    for (source, reason) in [
        (
            "S2",
            "no verified ProductAtomicV1 snapshot adapter is configured",
        ),
        (
            "S3",
            "no verified ProductAtomicV1 snapshot adapter is configured",
        ),
        (
            "S4",
            "mutable host evidence is not mounted in the scoped audit",
        ),
        (
            "S5",
            "no verified external publish evidence adapter is configured",
        ),
        (
            "S6",
            "tuple promotion evidence is not mounted in the scoped audit",
        ),
        (
            "M6",
            "governance evidence is not mounted in the scoped audit",
        ),
        (
            "governance",
            "governance evidence is not mounted in the scoped audit",
        ),
        (
            "casting",
            "casting evidence is not mounted in the scoped audit",
        ),
    ] {
        sources.push(ScopedProductAuditSourceV3 {
            source: source.into(),
            availability: "unavailable".into(),
            schema: None,
            projection_digest: None,
            coverage: None,
            missing_reason: Some(reason.into()),
        });
    }
    Ok(ScopedProductAuditReportV3 {
        schema_version: 3,
        generated_at: generated_at.to_rfc3339(),
        projection,
        sources,
    })
}

/// Build a truthful product source inventory from the selected ProductStore.
/// Missing subsystems remain explicit and are never represented as empty data.
pub fn product_inventory_v2(
    installation_id: &str,
    database_id: &str,
    snapshot: &foundry_ledger::product::ProductProjectionSnapshot,
    generated_at: chrono::DateTime<chrono::Utc>,
) -> Result<ProductAuditReportV2, String> {
    let rows_json = serde_json::to_vec(&snapshot.rows).map_err(|_| "S1 snapshot digest failed")?;
    let key = crate::privacy::ExportKey::generate().map_err(|_| "audit context unavailable")?;
    use rand_core::RngCore as _;
    let mut context_bytes = [0_u8; 16];
    rand_core::OsRng
        .try_fill_bytes(&mut context_bytes)
        .map_err(|_| "audit context unavailable")?;
    let context = hex(&context_bytes);
    context_bytes.fill(0);
    let source_identity = format!(
        "installation:{};database:{}",
        key.alias(&context, "installation", installation_id),
        key.alias(&context, "database", database_id),
    );
    let mut sources = vec![ProductAuditSourceV2 {
        source: "S1".into(),
        availability: "available".into(),
        identity: Some(source_identity),
        schema: Some("ProductAtomicV1 ledger".into()),
        prefix_sequence: Some(snapshot.head_sequence),
        head_hash: Some(snapshot.head_hash.clone()),
        snapshot_digest: Some(hex(&Sha256::digest(rows_json))),
        cutoff: Some(generated_at.to_rfc3339()),
        missing_reason: None,
    }];
    for (name, reason) in [
        ("S2", "no ProductAtomicV1 snapshot adapter is configured"),
        ("S3", "no ProductAtomicV1 snapshot adapter is configured"),
        (
            "S4",
            "mutable host evidence is not mounted in ProductAtomicV1",
        ),
        (
            "S5",
            "no verified external publish evidence adapter is configured",
        ),
        (
            "S6",
            "tuple promotion database is not mounted in ProductAtomicV1 audit",
        ),
        (
            "M6",
            "governance source is not mounted in ProductAtomicV1 audit",
        ),
        (
            "governance",
            "governance source is not mounted in ProductAtomicV1 audit",
        ),
        (
            "casting",
            "casting source is not mounted in ProductAtomicV1 audit",
        ),
    ] {
        sources.push(ProductAuditSourceV2 {
            source: name.into(),
            availability: "unavailable".into(),
            identity: None,
            schema: None,
            prefix_sequence: None,
            head_hash: None,
            snapshot_digest: None,
            cutoff: None,
            missing_reason: Some(reason.into()),
        });
    }
    Ok(ProductAuditReportV2 {
        schema_version: 2,
        generated_at: generated_at.to_rfc3339(),
        sources,
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The evidence envelope — everything needed to re-verify later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceEnvelope {
    /// number of ledger rows the report covers (prefix [0, prefix_len))
    pub prefix_len: i64,
    /// chain head hash AT the prefix boundary
    pub prefix_head_hash: String,
    /// full-chain head at generation time (context only)
    pub generated_at_chain_height: i64,
    pub generator_version: u32,
    /// metric definitions this report used (a definition change
    /// invalidates comparison)
    pub metric_definitions_version: u32,
    /// which journals fed the report
    pub source_set: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditMetrics {
    /// as-of prefix head: how many orders sit in each state NOW
    pub state_counts_asof: std::collections::BTreeMap<String, u64>,
    /// orders that first REACHED a terminal state within the prefix
    /// (counted once per order — retries cannot inflate)
    pub terminal_orders: std::collections::BTreeMap<String, u64>,
    /// approval latency median (FurnaceHold -> Publishing per order)
    pub approval_latency_ms_median: Option<u64>,
    /// casting promotion history with evidence links
    pub promotions: Vec<PromotionRecord>,
    /// cost aggregation from the casting journal: TOTAL tokens
    /// including reasoning; if NO covered run has cost data this is
    /// None — never 0 (unavailable != 0)
    pub cost_tokens_total: Option<u64>,
    pub cost_reasoning_tokens_total: Option<u64>,
    /// fraction of casting runs with cost data available (1.0 = full)
    pub cost_coverage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromotionRecord {
    pub casting_run_id: String,
    pub approved_by: String,
    pub input_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub envelope: EvidenceEnvelope,
    pub metrics: AuditMetrics,
    /// digest over envelope + canonical metrics — the binding
    pub content_digest: String,
}

/// Cost facts pulled from the casting journal (T22). The report
/// never fabricates them; the casting side supplies rows.
#[derive(Debug, Clone, Default)]
pub struct CastingFacts {
    pub promotions: Vec<PromotionRecord>,
    /// (total_tokens, reasoning_tokens) per run; None = unavailable
    pub run_costs: Vec<Option<(u64, u64)>>,
}

#[derive(Debug, PartialEq)]
pub enum VerifyOutcome {
    /// report matches its prefix exactly; prefix chain intact
    Valid,
    /// the ledger legitimately grew after the report; the report's
    /// own prefix still verifies and recomputes — NOT tampering
    ValidExtended { current_height: i64 },
}

#[derive(Debug)]
pub enum VerifyError {
    /// ledger is SHORTER than the report's prefix — rows were removed
    LedgerTruncated { prefix_len: i64, current_len: i64 },
    /// the chain at the prefix boundary differs — history rewritten
    PrefixMismatch { expect: String, got: String },
    /// recomputing from the (intact) prefix yields different numbers
    /// — the report content was altered
    ContentMismatch { expect: String, got: String },
    /// report references a generator/metric version we don't know
    UnknownVersion { generator: u32, metrics: u32 },
}

/// Verify rows[0..plen]: linkage AND full per-row hash RECOMPUTATION
/// (same semantics as SqliteLedger::verify_chain — a row whose stored
/// hash does not match its own content breaks verification here too).
fn chain_hash_prefix(rows: &[LedgerRow], prefix_len: usize) -> Option<String> {
    if rows.len() < prefix_len || prefix_len == 0 {
        return None;
    }
    use sha2::Digest;
    let mut prev = "GENESIS".to_string();
    for (i, r) in rows.iter().take(prefix_len).enumerate() {
        if r.seq != (i as i64) + 1 || r.prev_hash != prev {
            return None;
        }
        // recompute the row hash from its content (v1 payload shape;
        // v2 adds event_json — recomputed the same way the ledger does)
        let v1_payload = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}",
            r.seq, r.order_id, r.from_state, r.to_state, r.event, r.actor, r.intent, r.at, prev
        );
        let expect = match r.event_json.as_deref() {
            Some(json) if !json.is_empty() => {
                // same framing as foundry_ledger::event_json::v2_hash_payload
                let payload = format!("{v1_payload}|{json}");
                format!("{:x}", sha2::Sha256::digest(payload.as_bytes()))
            }
            _ => format!("{:x}", sha2::Sha256::digest(v1_payload.as_bytes())),
        };
        if expect != r.hash {
            return None;
        }
        prev = r.hash.clone();
    }
    Some(rows[prefix_len - 1].hash.clone())
}

fn content_digest(envelope: &EvidenceEnvelope, metrics: &AuditMetrics) -> String {
    let mut h = Sha256::new();
    h.update(b"foundry-audit-v1");
    h.update(envelope.prefix_len.to_le_bytes());
    h.update(envelope.prefix_head_hash.as_bytes());
    h.update(envelope.generated_at_chain_height.to_le_bytes());
    h.update(envelope.generator_version.to_le_bytes());
    h.update(envelope.metric_definitions_version.to_le_bytes());
    for s in &envelope.source_set {
        h.update(s.as_bytes());
        h.update(b";");
    }
    let m = serde_json::to_string(metrics).unwrap_or_default();
    h.update(m.as_bytes());
    hex(&h.finalize())
}

/// Compute metrics FROM a ledger prefix. Deterministic: same prefix
/// -> identical metrics -> identical digest. Single source of truth
/// for BOTH generation and verification.
fn compute_metrics(prefix: &[LedgerRow], casting: &CastingFacts) -> AuditMetrics {
    // per-order last state = as-of snapshot (kanban semantics)
    let mut last: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    // first time each order reaches a terminal state (once per order)
    let mut terminal_first: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // approval latency samples (hold -> publishing per order)
    let mut hold_at: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut latencies: Vec<u64> = Vec::new();

    for r in prefix {
        last.insert(r.order_id.clone(), r.to_state.clone());
        let ts = chrono::DateTime::parse_from_rfc3339(&r.at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        if r.to_state == "furnace_hold" {
            hold_at.insert(r.order_id.clone(), ts);
        }
        if r.to_state == "publishing"
            && let Some(t0) = hold_at.get(&r.order_id)
        {
            latencies.push((ts - t0).max(0) as u64);
        }
        if matches!(
            r.to_state.as_str(),
            "shipped" | "failed" | "rejected" | "canceled"
        ) && !terminal_first.contains_key(&r.order_id)
        {
            terminal_first.insert(r.order_id.clone(), r.to_state.clone());
        }
    }

    let mut state_counts_asof = std::collections::BTreeMap::new();
    for s in last.values() {
        *state_counts_asof.entry(s.clone()).or_insert(0) += 1;
    }
    let mut terminal_orders = std::collections::BTreeMap::new();
    for s in terminal_first.values() {
        *terminal_orders.entry(s.clone()).or_insert(0) += 1;
    }
    latencies.sort();
    let median = (!latencies.is_empty()).then(|| latencies[latencies.len() / 2]);

    // cost aggregation: unavailable != 0. Partial coverage reports
    // the covered total AND the gap fraction.
    let (total, reasoning, coverage) = if casting.run_costs.is_empty() {
        (None, None, None)
    } else {
        let avail = casting.run_costs.iter().filter(|c| c.is_some()).count();
        let cov = avail as f64 / casting.run_costs.len() as f64;
        let covered_total = casting
            .run_costs
            .iter()
            .filter_map(|c| c.map(|x| x.0))
            .sum::<u64>();
        let covered_reasoning = casting
            .run_costs
            .iter()
            .filter_map(|c| c.map(|x| x.1))
            .sum::<u64>();
        if avail == 0 {
            (None, None, Some(0.0))
        } else {
            (Some(covered_total), Some(covered_reasoning), Some(cov))
        }
    };

    AuditMetrics {
        state_counts_asof,
        terminal_orders,
        approval_latency_ms_median: median,
        promotions: casting.promotions.clone(),
        cost_tokens_total: total,
        cost_reasoning_tokens_total: reasoning,
        cost_coverage: coverage,
    }
}

pub fn generate(rows: &[LedgerRow], casting: &CastingFacts) -> Result<AuditReport, String> {
    let prefix_len = rows.len() as i64;
    let prefix_head = chain_hash_prefix(rows, rows.len()).ok_or("empty or broken ledger")?;
    let envelope = EvidenceEnvelope {
        prefix_len,
        prefix_head_hash: prefix_head,
        generated_at_chain_height: prefix_len,
        generator_version: GENERATOR_VERSION,
        metric_definitions_version: 1,
        source_set: vec!["ledger:orders".into(), "ledger:casting".into()],
    };
    let metrics = compute_metrics(rows, casting);
    let content_digest = content_digest(&envelope, &metrics);
    Ok(AuditReport {
        envelope,
        metrics,
        content_digest,
    })
}

pub fn verify(
    report: &AuditReport,
    current_rows: &[LedgerRow],
    casting: &CastingFacts,
) -> Result<VerifyOutcome, VerifyError> {
    if report.envelope.generator_version > GENERATOR_VERSION
        || report.envelope.metric_definitions_version > 1
    {
        return Err(VerifyError::UnknownVersion {
            generator: report.envelope.generator_version,
            metrics: report.envelope.metric_definitions_version,
        });
    }
    // SELF-CONSISTENCY FIRST: the digest must cover the report's OWN
    // metrics as published. Tampering the metrics body without
    // recomputing the digest breaks here; tampering BOTH still fails
    // the recompute below (attacker can't make the ledger say 99).
    let self_digest = content_digest(&report.envelope, &report.metrics);
    if self_digest != report.content_digest {
        return Err(VerifyError::ContentMismatch {
            expect: report.content_digest.clone(),
            got: self_digest,
        });
    }
    let plen = report.envelope.prefix_len as usize;
    // truncation: current ledger shorter than the report's prefix
    if current_rows.len() < plen {
        return Err(VerifyError::LedgerTruncated {
            prefix_len: report.envelope.prefix_len,
            current_len: current_rows.len() as i64,
        });
    }
    // prefix chain intact AND boundary head matches the envelope
    let got = chain_hash_prefix(current_rows, plen).ok_or_else(|| VerifyError::PrefixMismatch {
        expect: report.envelope.prefix_head_hash.clone(),
        got: "broken".into(),
    })?;
    if got != report.envelope.prefix_head_hash {
        return Err(VerifyError::PrefixMismatch {
            expect: report.envelope.prefix_head_hash.clone(),
            got,
        });
    }
    // deterministic recompute from the SAME prefix
    let recomputed = compute_metrics(&current_rows[..plen], casting);
    let expect_digest = content_digest(&report.envelope, &recomputed);
    if expect_digest != report.content_digest {
        return Err(VerifyError::ContentMismatch {
            expect: report.content_digest.clone(),
            got: expect_digest,
        });
    }
    if current_rows.len() as i64 == report.envelope.prefix_len {
        Ok(VerifyOutcome::Valid)
    } else {
        Ok(VerifyOutcome::ValidExtended {
            current_height: current_rows.len() as i64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// entry_hash mirror (v1 shape) — identical to the ledger's own.
    #[allow(clippy::too_many_arguments)]
    fn entry_hash(
        seq: i64,
        order: &str,
        from: &str,
        to: &str,
        event: &str,
        actor: &str,
        intent: &str,
        at: &str,
        prev: &str,
    ) -> String {
        let payload = format!("{seq}|{order}|{from}|{to}|{event}|{actor}|{intent}|{at}|{prev}");
        hex(&Sha256::digest(payload.as_bytes()))
    }

    fn mk_row(seq: i64, order: &str, to: &str, at: &str, prev: &str) -> LedgerRow {
        let hash = entry_hash(seq, order, "x", to, &format!("E{seq}"), "a", "i", at, prev);
        LedgerRow {
            seq,
            order_id: order.into(),
            from_state: "x".into(),
            to_state: to.into(),
            event: format!("E{seq}"),
            actor: "a".into(),
            intent: "i".into(),
            at: at.into(),
            prev_hash: prev.into(),
            hash,
            event_json: None,
        }
    }

    /// Build a properly hash-linked chain (like the real ledger).
    fn linked_rows(order_specs: &[(&str, &str)]) -> Vec<LedgerRow> {
        let mut rows = Vec::new();
        let mut prev = "GENESIS".to_string();
        for (i, (order, to)) in order_specs.iter().enumerate() {
            let at = chrono::DateTime::from_timestamp_millis(1_000 + i as i64)
                .unwrap()
                .to_rfc3339();
            let r = mk_row(i as i64 + 1, order, to, &at, &prev);
            prev = r.hash.clone();
            rows.push(r);
        }
        rows
    }

    fn append(rows: &mut Vec<LedgerRow>, order: &str, to: &str) {
        let prev = rows.last().unwrap().hash.clone();
        let at = chrono::Utc::now().to_rfc3339();
        let r = mk_row(rows.len() as i64 + 1, order, to, &at, &prev);
        rows.push(r);
    }

    #[test]
    fn deterministic_recompute() {
        let rows = linked_rows(&[("o-1", "queued"), ("o-2", "shipped"), ("o-1", "working")]);
        let casting = CastingFacts::default();
        let r1 = generate(&rows, &casting).unwrap();
        let r2 = generate(&rows, &casting).unwrap();
        assert_eq!(
            r1.content_digest, r2.content_digest,
            "same prefix -> identical digest"
        );
    }

    #[test]
    fn extension_is_legal_not_tampering() {
        let rows = linked_rows(&[("o-1", "queued"), ("o-2", "shipped")]);
        let casting = CastingFacts::default();
        let report = generate(&rows, &casting).unwrap();
        // ledger grows legitimately
        let mut grown = rows.clone();
        append(&mut grown, "o-3", "shipped");
        append(&mut grown, "o-3", "canceled");
        match verify(&report, &grown, &casting).unwrap() {
            VerifyOutcome::ValidExtended { current_height } => assert_eq!(current_height, 4),
            other => panic!("expected extension, got {other:?}"),
        }
        // and the report still verifies against the exact original
        assert_eq!(
            verify(&report, &rows, &casting).unwrap(),
            VerifyOutcome::Valid
        );
    }

    #[test]
    fn truncation_detected() {
        let rows = linked_rows(&[("o-1", "queued"), ("o-2", "queued"), ("o-3", "shipped")]);
        let casting = CastingFacts::default();
        let report = generate(&rows, &casting).unwrap();
        let truncated = &rows[..2];
        assert!(matches!(
            verify(&report, truncated, &casting),
            Err(VerifyError::LedgerTruncated {
                prefix_len: 3,
                current_len: 2
            })
        ));
    }

    #[test]
    fn prefix_rewrite_detected() {
        let rows = linked_rows(&[("o-1", "queued"), ("o-2", "shipped"), ("o-3", "queued")]);
        let casting = CastingFacts::default();
        let report = generate(&rows, &casting).unwrap();
        // rewrite a covered row's content, keeping length and links
        // intact is impossible without breaking the chain — but the
        // attacker also re-links. Simulate the naive rewrite first:
        let mut forged = rows.clone();
        forged[1].to_state = "failed".into();
        assert!(matches!(
            verify(&report, &forged, &casting),
            Err(VerifyError::PrefixMismatch { .. })
        ));
        // full re-link attack: rewrite AND rebuild the chain with real
        // entry_hash — the prefix head changes, which the envelope pins
        let mut relinked = Vec::new();
        let mut prev = "GENESIS".to_string();
        for (i, r) in rows.iter().enumerate() {
            let to = if i == 1 {
                "failed"
            } else {
                r.to_state.as_str()
            };
            let nr = mk_row(r.seq, &r.order_id, to, &r.at, &prev);
            prev = nr.hash.clone();
            relinked.push(nr);
        }
        let res = verify(&report, &relinked, &casting);
        assert!(
            matches!(res, Err(VerifyError::PrefixMismatch { .. })),
            "re-linked history has a different head — envelope catches it"
        );
    }

    #[test]
    fn report_content_tamper_detected() {
        let rows = linked_rows(&[("o-1", "queued"), ("o-2", "shipped")]);
        let casting = CastingFacts::default();
        let mut report = generate(&rows, &casting).unwrap();
        // someone bumps the shipped count without touching the ledger
        report.metrics.terminal_orders.insert("shipped".into(), 99);
        let res = verify(&report, &rows, &casting);
        assert!(matches!(res, Err(VerifyError::ContentMismatch { .. })));
        // attacker ALSO recomputes the digest over tampered metrics:
        // self-consistent, but the recompute-from-ledger catches it
        report.content_digest = content_digest(&report.envelope, &report.metrics);
        let res2 = verify(&report, &rows, &casting);
        assert!(matches!(res2, Err(VerifyError::ContentMismatch { .. })));
    }

    #[test]
    fn retries_do_not_double_count() {
        // one order: failed -> retried -> failed -> retried -> shipped
        let rows = linked_rows(&[
            ("o-1", "failed"),
            ("o-1", "queued"),
            ("o-1", "failed"),
            ("o-1", "queued"),
            ("o-1", "shipped"),
        ]);
        let m = compute_metrics(&rows, &CastingFacts::default());
        // first terminal reach recorded once per (order, state-on-first-reach):
        // 'failed' is o-1's FIRST terminal — recorded once, not twice
        assert_eq!(
            m.terminal_orders.get("failed"),
            Some(&1),
            "first-reach counted once despite 2 failures"
        );
        assert_eq!(
            m.state_counts_asof.get("shipped"),
            Some(&1),
            "as-of state is shipped"
        );
        // shipped never shows in terminal_orders: o-1's first terminal was failed
        assert_eq!(m.terminal_orders.get("shipped"), None);
    }

    #[test]
    fn unavailable_cost_is_none_not_zero() {
        let rows = linked_rows(&[("o-1", "queued")]);
        // partial coverage: covered total + gap disclosed
        let casting = CastingFacts {
            promotions: vec![],
            run_costs: vec![None, Some((100, 40))],
        };
        let m = compute_metrics(&rows, &casting);
        assert_eq!(m.cost_tokens_total, Some(100));
        assert_eq!(m.cost_reasoning_tokens_total, Some(40));
        assert_eq!(m.cost_coverage, Some(0.5), "coverage gap disclosed");
        // ALL unavailable -> None, never 0
        let casting2 = CastingFacts {
            promotions: vec![],
            run_costs: vec![None, None],
        };
        let m2 = compute_metrics(&rows, &casting2);
        assert_eq!(m2.cost_tokens_total, None, "unavailable != 0");
        assert_eq!(m2.cost_coverage, Some(0.0));
        // full coverage
        let casting3 = CastingFacts {
            promotions: vec![],
            run_costs: vec![Some((10, 5)), Some((20, 5))],
        };
        let m3 = compute_metrics(&rows, &casting3);
        assert_eq!(m3.cost_tokens_total, Some(30));
        assert_eq!(m3.cost_coverage, Some(1.0));
    }

    #[test]
    fn future_version_refused() {
        let rows = linked_rows(&[("o-1", "queued")]);
        let casting = CastingFacts::default();
        let mut report = generate(&rows, &casting).unwrap();
        report.envelope.generator_version = 99;
        assert!(matches!(
            verify(&report, &rows, &casting),
            Err(VerifyError::UnknownVersion { .. })
        ));
    }
}
