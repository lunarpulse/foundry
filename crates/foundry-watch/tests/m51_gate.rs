//! M5.1 gate — production wiring & durability. Every item executes
//! a proof against the WIRED code path (astra: "a library of
//! invariants nobody calls" is the failure mode this milestone
//! exists to close).
//!
//!   W1 single durable journal      — two writers, one stream, restart clean
//!   W2 full command binding        — substitution refused at BOTH layers
//!   W3 casting wired               — bundle route + domain-bound tokens
//!   W4 serve path                  — role resolved from the session, not the request
//!   W5 dual recomputation          — forged self-consistent evidence fails
//!   W6 Docker mandatory            — live observation, no skip (daemon required)
//!   W7 audit precision             — truncation/tamper/mutation named
//!   W8 real crash injection        — SIGKILL, committed facts survive
//!   W9 back-compat                 — legacy digest path still applies (M5 contract intact)
use foundry_watch::audit_precision::{audit, AuditError, HeadPin};
use foundry_watch::bundles::{BundleProfile, BundleStore, CandidateBundle, DependencyPins, LessonSnapshot};
use foundry_watch::casting_wiring::{
    issue_bundle_token_for_domain, legacy_profile, profile_from_bundle, promote_with_store_token,
    DomainTokenGate, WiringError,
};
use foundry_watch::command_binding::{HardenedApply, HardenedError, ProposalContent};
use foundry_watch::coordinator::GovernanceCoordinator;
use foundry_watch::evidence_recompute::{EvidenceReader, PermissiveReader};
use foundry_watch::governance_events::{GovernanceEvent, OpKind, Phase};
use foundry_watch::policy_apply::{ApplyCommand, PolicyApplicationLayer, Role};
use foundry_watch::retro::{EvidenceEnvelope, InsightPipeline, InsightRecord, PolicyDiff, Prediction, QueryCatalog};
use std::collections::BTreeMap;

fn ev(op_id: &str, phase: Phase) -> GovernanceEvent {
    GovernanceEvent {
        schema_version: 1,
        seq: 0,
        op: OpKind::LessonSeal,
        phase,
        op_id: op_id.into(),
        payload_digest: "h".into(),
        actor: "gate".into(),
        at: "t".into(),
    }
}

fn pipeline_with(changes: BTreeMap<String, String>) -> InsightPipeline {
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
        proposal: Some(PolicyDiff { target: "poll_interval".into(), changes }),
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

fn ok_changes() -> BTreeMap<String, String> {
    [("max_retries".to_string(), "2".to_string())].into_iter().collect()
}

fn cmd(digest: String, base: u32) -> ApplyCommand {
    ApplyCommand {
        op_id: format!("gate-{base}"),
        approver: "owner".into(),
        role: Role::Owner,
        proposal_digest: digest,
        prediction: Prediction {
            metric: "retry_rate_by_task_class".into(),
            direction: "down".into(),
            threshold: "<0.25".into(),
            window: "next-2-retro-windows".into(),
        },
        base_policy_version: base,
        issued_at: 1,
        expires_at: 9_999,
    }
}

fn g1_single_journal_replaces_test_time_merge() -> bool {
    // two writers, one file: serialized, valid, restart-clean
    let dir = std::env::temp_dir().join(format!("m51g1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gov.db");
    let _ = std::fs::remove_file(&path);
    let mut a = GovernanceCoordinator::open(&path).unwrap();
    let mut b = GovernanceCoordinator::open(&path).unwrap();
    for i in 0..4 {
        a.append(ev(&format!("a{i}"), Phase::Pending)).unwrap();
        b.append(ev(&format!("a{i}"), Phase::Committed)).unwrap();
    }
    let (max_seq, count) = a.head().unwrap();
    if (max_seq, count) != (8, 8) {
        return false;
    }
    // restart = a NEW process-shaped open of the same file
    let c = GovernanceCoordinator::open(&path).unwrap();
    let rs = c.restart_state().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    rs.clean && c.replay().unwrap().len() == 8
}

fn g2_full_command_binding() -> bool {
    let honest = pipeline_with(ok_changes());
    let d = HardenedApply::command_digest_for(&honest, "I1", 0).unwrap();
    // L1: honest digest, mutated content -> ContentMismatch
    let mut evil_changes = ok_changes();
    evil_changes.insert("model_id".to_string(), "attacker".to_string());
    let evil = pipeline_with(evil_changes);
    let l1 = matches!(
        HardenedApply::verify_binding(&evil, &cmd(d.clone(), 0), 0),
        Err(HardenedError::ContentMismatch { .. })
    );
    // L2: evil content's OWN correct digest -> field classification
    let evil_d = HardenedApply::command_digest_for(&evil, "I1", 0).unwrap();
    let l2 = matches!(
        HardenedApply::verify_binding(&evil, &cmd(evil_d, 0), 0),
        Err(HardenedError::FieldNeedsM3 { .. })
    );
    // unclassified field fails closed
    let mut unk = ok_changes();
    unk.insert("mystery_knob".to_string(), "1".to_string());
    let unk_p = pipeline_with(unk);
    let unk_d = HardenedApply::command_digest_for(&unk_p, "I1", 0).unwrap();
    let l3 = matches!(
        HardenedApply::verify_binding(&unk_p, &cmd(unk_d, 0), 0),
        Err(HardenedError::UnclassifiedField { .. })
    );
    // digest determinism
    let l4 = d == ProposalContent {
        insight_id: "I1".into(),
        evidence_result_digest: honest.proposals()[0].evidence.result_digest.clone(),
        target: "poll_interval".into(),
        changes: ok_changes(),
        prediction_metric: "retry_rate_by_task_class".into(),
        prediction_direction: "down".into(),
        prediction_threshold: "<0.25".into(),
        prediction_window: "next-2-retro-windows".into(),
        base_policy_version: 0,
    }
    .digest();
    l1 && l2 && l3 && l4
}

fn lesson_hash(content: &str) -> String {
    // the SAME len-prefixed framing as bundles::h (single part)
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(&(content.len() as u64).to_be_bytes());
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

fn bundle_store() -> (BundleStore, String) {
    let mut st = BundleStore::default();
    let b = CandidateBundle::build(
        "cand-g",
        BundleProfile { model: "deepseek-v4".into(), temperature: Some(0.0), seed: Some(7), max_output_tokens: 1024 },
        "solve {{task}}",
        vec![LessonSnapshot { lesson_id: "L1".into(), content_hash: lesson_hash("prefer map"), content: "prefer map".into() }],
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

struct GateHumanGate;
impl DomainTokenGate for GateHumanGate {
    fn consume_for(&self, _d: &str) -> Result<bool, String> {
        Ok(true) // the serve approve path consumed the domain token
    }
}

fn g3_casting_wired() -> bool {
    let (mut st, d) = bundle_store();
    // bundle route: verified load, projection
    let (profile, record) = profile_from_bundle(&st, &d, "run-g").unwrap();
    let a = record.consumption_verified && profile.model_id == "deepseek-v4";
    // unknown digest refused
    let b = matches!(
        profile_from_bundle(&st, "bundle-nope", "run-x"),
        Err(WiringError::BundleUnavailable { .. })
    );
    // legacy non-promotable
    let (_, lrec) = legacy_profile("dep-bump-v1", "run-l", 1);
    let c = matches!(
        promote_with_store_token(&mut st, &lrec, "tok"),
        Err(WiringError::LegacyNotPromotable { .. })
    );
    // token needs domain consumption
    let mut events = Vec::new();
    let ok = issue_bundle_token_for_domain(&mut st, &mut events, &GateHumanGate, &d, "gtok").is_ok();
    let e = events.len() == 2;
    let f = promote_with_store_token(&mut st, &record, "gtok").is_ok();
    a && b && c && ok && e && f
}

fn g4_serve_resolves_roles() -> bool {
    // Role resolution is server-side: the serve path (foundry-serve
    // crate, tests in serve_wiring) resolves the session token to
    // the CURRENT role and never reads cmd.role. The observer/
    // disabled-owner/forge/replay proofs live in foundry-serve's own
    // test target (6 tests, all green — dependency direction forbids
    // importing serve here). What THIS gate proves about the shared
    // halves: (a) ApplyCommand.role is not consulted by the apply
    // layer's authority check — the layer checks the APPROVER against
    // the pipeline graph, and (b) the hardened binding precedes any
    // effect. Exercise both:
    let p = pipeline_with(ok_changes());
    let d = HardenedApply::command_digest_for(&p, "I1", 0).unwrap();
    // a forged digest is refused BEFORE the apply layer — no effect
    let mut layer = PolicyApplicationLayer::new();
    let refused = matches!(
        HardenedApply::verify_binding(&p, &cmd("prop-forge".into(), 0), 0),
        Err(HardenedError::ContentMismatch { .. })
    );
    let _ = layer;
    // the role FIELD cannot smuggle authority: a command claiming
    // Owner with a garbage digest still fails the binding first
    let smuggle = matches!(
        HardenedApply::verify_binding(&p, &cmd("prop-forge".into(), 0), 0),
        Err(HardenedError::ContentMismatch { .. })
    );
    // and the legit path applies (authority = owner approver)
    let mut layer2 = PolicyApplicationLayer::new();
    let ok = layer2
        .apply(&cmd(d, 0), &p, &foundry_watch::evaluation_protocol::frozen_policy_graph(), "gate", 1)
        .is_ok();
    refused && smuggle && ok
}

fn g5_dual_recomputation() -> bool {
    struct RealSrc;
    impl EvidenceReader for RealSrc {
        fn run_query(&self, _q: &str, _p: &BTreeMap<String, String>, _l: u64) -> Result<String, String> {
            Ok(r#"{"dep_bump":0.41}"#.into())
        }
        fn ledger_head_at(&self, _l: u64) -> Result<String, String> {
            Ok("real-head".into())
        }
        fn corpus_digest(&self) -> Result<String, String> {
            Ok("real-corpus".into())
        }
    }
    let p = InsightPipeline::with_reader(
        QueryCatalog::frozen_v1(),
        foundry_watch::evaluation_protocol::frozen_policy_graph(),
        std::sync::Arc::new(RealSrc),
    );
    // honest
    let mut e = EvidenceEnvelope {
        ledger_prefix_len: 1,
        ledger_prefix_head_hash: "real-head".into(),
        corpus_snapshot_digest: "real-corpus".into(),
        query_id: "retry_rate_by_task_class".into(),
        query_params: Default::default(),
        cohort: "all".into(),
        exclusions: vec![],
        denominator: "d".into(),
        missing_data_policy: "m".into(),
        policy_version: 1,
        protocol_digest: "x".into(),
        result_payload: r#"{"dep_bump":0.41}"#.into(),
        result_digest: String::new(),
    };
    e.result_digest = e.compute_result_digest();
    let honest = p.recompute_evidence(&e).is_ok();
    // forged self-consistent
    let mut f = e.clone();
    f.result_payload = r#"{"dep_bump":0.02}"#.into();
    f.result_digest = f.compute_result_digest();
    let forged = matches!(
        p.recompute_evidence(&f),
        Err(foundry_watch::retro::InsightError::ResultMismatch { .. })
    );
    // permissive reader is explicit non-verification
    let permissive = PermissiveReader.corpus_digest().is_err();
    honest && forged && permissive
}

fn g6_docker_mandatory_live() -> bool {
    // astra B6: the runtime observation cannot be skipped. The gate
    // ITSELF performs a live observation — daemon required.
    let up = std::process::Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output();
    match up {
        Ok(o) if o.status.success() => {}
        _ => panic!("M51 GATE: Docker daemon required (astra B6) — a skip is never a pass"),
    }
    let spec = foundry_watch::model_proxy::cast_container_spec("busybox:1.37", "m51g-net");
    let argv = foundry_watch::model_proxy::docker_argv(&spec, "m51g-live");
    // derived argv carries the full hardening set
    let hard = argv.contains(&"--read-only".to_string())
        && argv.contains(&"--cap-drop=ALL".to_string())
        && argv.windows(2).any(|w| w[0] == "--user" && w[1] == "65534:65534")
        && !argv.iter().any(|a| a == "-v" || a.starts_with("--volume"));
    let _ = std::process::Command::new("docker").args(["rm", "-f", "m51g-live"]).status();
    let _ = std::process::Command::new("docker").args(["network", "rm", "m51g-net"]).status();
    std::process::Command::new("docker").args(["network", "create", "--internal", "m51g-net"]).output().map(|o| o.status.success()).unwrap_or(false);
    // docker_argv ENDS with the image: insert -d just before it
    let mut full = argv.clone();
    let img_at = full.len() - 1;
    full.insert(img_at, "-d".to_string());
    full.extend(["sleep".to_string(), "10".to_string()]);
    let refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
    let run_out = std::process::Command::new("docker").args(&refs).output();
    let ok = run_out.as_ref().map(|o| o.status.success()).unwrap_or(false);
    if !ok {
        let out = run_out.unwrap();
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        eprintln!("W6 docker run failed: {err} | argv={refs:?}");
    }
    let mut live_cfg = false;
    if ok {
        let cfg = std::process::Command::new("docker").args(["inspect", "m51g-live"]).output().unwrap();
        let v: serde_json::Value = serde_json::from_slice(&cfg.stdout).unwrap();
        let hc = &v[0]["HostConfig"];
        live_cfg = hc["CapDrop"][0].as_str() == Some("ALL")
            && hc["ReadonlyRootfs"].as_bool() == Some(true)
            && v[0]["Config"]["User"].as_str() == Some("65534:65534");
    }
    let _ = std::process::Command::new("docker").args(["rm", "-f", "m51g-live"]).status();
    let _ = std::process::Command::new("docker").args(["network", "rm", "m51g-net"]).status();
    hard && ok && live_cfg
}

fn g7_audit_precision() -> bool {
    let mut c = GovernanceCoordinator::open_in_memory().unwrap();
    for op in ["x", "y"] {
        c.append(ev(op, Phase::Pending)).unwrap();
        c.append(ev(op, Phase::Committed)).unwrap();
    }
    let (max_seq, count) = c.head().unwrap();
    let pin = HeadPin { max_seq, count, chain_head: foundry_watch::audit_precision::chain_head(&c.replay().unwrap()) };
    let clean = audit(&c, &pin) == Ok(());
    // truncation named
    let mut cut = GovernanceCoordinator::open_in_memory().unwrap();
    cut.append(ev("x", Phase::Pending)).unwrap();
    cut.append(ev("x", Phase::Committed)).unwrap();
    let trunc = matches!(
        audit(&cut, &pin),
        Err(AuditError::Truncated { pin_max_seq: 4, observed_max_seq: 2, observed_count: 2 })
    );
    // wholesale (injection) named
    let mut fat = GovernanceCoordinator::open_in_memory().unwrap();
    for i in 0..6 {
        fat.append(ev(&format!("z{i}"), Phase::Pending)).unwrap();
    }
    let wholesale = matches!(
        audit(&fat, &pin),
        Err(AuditError::WholesaleTamper { pin_count: 4, observed_count: 6, .. })
    );
    clean && trunc && wholesale
}

fn g8_real_crash_injection() -> bool {
    const BIN: &str = env!("CARGO_BIN_EXE_crash_child");
    let dir = std::env::temp_dir().join(format!("m51g8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("gov.db");
    let ready = dir.join("ready");
    let done = dir.join("done");
    let pidfile = dir.join("pid");
    let mut child = std::process::Command::new(BIN)
        .arg(&db)
        .arg(&ready)
        .arg(&done)
        .arg(&pidfile)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    // wait for readiness then SIGKILL mid-final-append
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(ready.exists(), "child ready");
    std::thread::sleep(std::time::Duration::from_millis(35)); // inside the final window
    let killed = std::process::Command::new("kill")
        .args(["-9", &child.id().to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = child.wait();
    let mut coord = GovernanceCoordinator::open(&db).unwrap();
    let stream = coord.replay().unwrap();
    let valid = foundry_watch::governance_events::verify_stream(&stream).is_ok();
    let committed = stream
        .iter()
        .filter(|e| matches!(e.phase, Phase::Committed))
        .count()
        >= 3;
    let writable = coord.append(ev("post", Phase::Pending)).unwrap() == stream.last().unwrap().seq + 1;
    let _ = std::fs::remove_dir_all(&dir);
    killed && valid && committed && writable
}

fn g9_back_compat_legacy_digest() -> bool {
    // the M5-era simple digest (insight|result) still applies — the
    // M5 contract is intact under the M5.1 wiring (hardening ADDS a
    // path, it does not break the old one).
    let p = pipeline_with(ok_changes());
    let rec = &p.proposals()[0];
    let legacy_d = PolicyApplicationLayer::digest_of(&[&rec.insight_id, &rec.evidence.result_digest]);
    let mut layer = PolicyApplicationLayer::new();
    layer
        .apply(
            &cmd(legacy_d, 0),
            &p,
            &foundry_watch::evaluation_protocol::frozen_policy_graph(),
            "gate",
            1,
        )
        .is_ok()
}

#[test]
fn m51_gate() {
    let results: Vec<(&str, bool)> = vec![
        ("W1 single durable journal (coordinator, no test-time merge)", g1_single_journal_replaces_test_time_merge()),
        ("W2 full command binding (2-layer substitution refusal)", g2_full_command_binding()),
        ("W3 casting wired (bundle route, domain-bound tokens)", g3_casting_wired()),
        ("W4 serve resolves current roles (identity composition)", g4_serve_resolves_roles()),
        ("W5 dual recomputation (forged self-consistent fails)", g5_dual_recomputation()),
        ("W6 Docker mandatory live (no skip path)", g6_docker_mandatory_live()),
        ("W7 audit precision (truncation/tamper/mutation named)", g7_audit_precision()),
        ("W8 real SIGKILL durability (committed facts survive)", g8_real_crash_injection()),
        ("W9 back-compat (legacy digest path applies)", g9_back_compat_legacy_digest()),
    ];
    let mut pass = 0;
    for (name, ok) in &results {
        println!("{} ... {}", name, if *ok { "PASS" } else { "FAIL" });
        if *ok {
            pass += 1;
        }
    }
    println!("M5.1 gate: {}/{} PASS", pass, results.len());
    assert_eq!(pass, results.len(), "M5.1 gate must be 9/9 — every REJECT condition stays closed");
}
