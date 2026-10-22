use foundry_pack::{EngineAction, GraphEngine, RunInput, load_pack};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

const PACK_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";
const FIXTURE: &str = include_str!("fixtures/golden_trace_v1.json");

#[derive(Deserialize)]
struct Fixture {
    pack_digest_expected: String,
    first_decision_60: Vec<FirstDecision>,
    bad_plan_iterated: BadPlan,
    intent_gap_iterated: IntentGap,
}
#[derive(Deserialize)]
struct FirstDecision {
    verdicts: Vec<String>,
    fix_rounds: u64,
    loopbacks: u64,
    expected_first: String,
}
#[derive(Deserialize)]
struct BadPlan {
    destinations: Vec<String>,
    final_counters: BTreeMap<String, u64>,
}
#[derive(Deserialize)]
struct IntentGap {
    destinations: Vec<String>,
    hold_entry_counters: BTreeMap<String, u64>,
    reverted_base: String,
}

fn setup() -> (GraphEngine, Fixture) {
    let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
    let pack_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/bmad-review-full/v1");
    let pack = load_pack(pack_path).unwrap();
    assert_eq!(pack.digest(), PACK_DIGEST);
    assert_eq!(fixture.pack_digest_expected, pack.digest());
    (GraphEngine::from_snapshot(&pack).unwrap(), fixture)
}
fn input(verdicts: Vec<String>) -> RunInput {
    let has_findings = verdicts
        .iter()
        .any(|v| matches!(v.as_str(), "patch" | "intent_gap" | "bad_plan"));
    RunInput {
        has_findings,
        verdicts,
        budget_exhausted: false,
        timeout: false,
    }
}
fn counters(fix_rounds: u64, loopbacks: u64) -> BTreeMap<String, u64> {
    BTreeMap::from([
        ("fix_rounds".into(), fix_rounds),
        ("loopbacks".into(), loopbacks),
    ])
}

#[test]
fn all_sixty_original_first_decisions_match_fixture() {
    let (engine, fixture) = setup();
    assert_eq!(fixture.first_decision_60.len(), 60);
    let mut observed = BTreeSet::new();
    let words = ["patch", "intent_gap", "bad_plan", "defer"];
    let expected: BTreeSet<_> = (1..16)
        .flat_map(|mask| {
            let mut verdicts: Vec<_> = words
                .iter()
                .enumerate()
                .filter(|(bit, _)| mask & (1 << bit) != 0)
                .map(|(_, word)| word.to_string())
                .collect();
            verdicts.sort();
            [2, 3].into_iter().flat_map(move |fix| {
                let verdicts = verdicts.clone();
                [4, 5]
                    .into_iter()
                    .map(move |loops| (verdicts.clone(), fix, loops))
            })
        })
        .collect();
    for case in fixture.first_decision_60 {
        let mut verdicts = case.verdicts.clone();
        verdicts.sort();
        assert!(
            observed.insert((verdicts, case.fix_rounds, case.loopbacks)),
            "duplicate fixture case"
        );
        let mut state = engine.initial_state(counters(case.fix_rounds, case.loopbacks));
        let outcome = engine.run(&mut state, &input(case.verdicts), 32).unwrap();
        assert!(outcome.destinations.len() >= 2);
        assert_eq!(
            outcome.destinations[1]
                .strip_prefix("hold:")
                .unwrap_or(&outcome.destinations[1]),
            case.expected_first
        );
    }
    assert_eq!(
        observed, expected,
        "all 15 nonempty verdict sets at all four counter boundaries"
    );
}

#[test]
fn repeated_bad_plan_walk_matches_fixture_and_counter_inheritance() {
    let (engine, fixture) = setup();
    let mut state = engine.initial_state(counters(0, 0));
    let outcome = engine
        .run(&mut state, &input(vec!["bad_plan".into()]), 64)
        .unwrap();
    assert_eq!(outcome.destinations, fixture.bad_plan_iterated.destinations);
    assert_eq!(state.counters, fixture.bad_plan_iterated.final_counters);
    assert_eq!(state.counters["loopbacks"], 6);
}

#[test]
fn intent_gap_holds_after_bump_and_revert_then_resumes_to_common_cap() {
    let (engine, fixture) = setup();
    let mut state = engine.initial_state(BTreeMap::new());
    let first = engine
        .run(&mut state, &input(vec!["intent_gap".into()]), 16)
        .unwrap();
    assert_eq!(first.destinations, fixture.intent_gap_iterated.destinations);
    let entry = first.hold.as_ref().unwrap();
    assert_eq!(
        entry.entry_counters,
        fixture.intent_gap_iterated.hold_entry_counters
    );
    assert_eq!(
        entry.reverted_base.as_deref(),
        Some(fixture.intent_gap_iterated.reverted_base.as_str())
    );
    assert_eq!(
        first.actions,
        vec![
            EngineAction::Bump {
                counter: "loopbacks".into(),
                after: 1
            },
            EngineAction::Revert {
                base: "origin".into()
            },
        ]
    );

    let mut outcome = first;
    for _ in 0..5 {
        outcome = engine.resume(&mut state, "replan_full").unwrap();
    }
    assert_eq!(
        outcome.hold.as_ref().map(|hold| hold.node_id.as_str()),
        Some("human")
    );
    assert_eq!(outcome.hold.unwrap().entry_counters["loopbacks"], 6);
    assert!(engine.resume(&mut state, "replan_full").is_err());
}

#[test]
fn golden_trace_graph_cases_are_preserved() {
    let (engine, _) = setup();
    // #1 patch loops to fixdie, #2 bad_plan enters replan, #3 intent_gap holds,
    // #5 intent_gap wins over bad_plan, #6/#7 cap holds at human, #8 patch cap,
    // #9 no findings passes, and #10 budget takes precedence.
    let cases = [
        (vec!["patch"], 0, 0, false, false, "fixdie"),
        (vec!["bad_plan"], 0, 0, false, false, "replan"),
        (vec!["intent_gap"], 0, 0, false, false, "human_gap"),
        (
            vec!["intent_gap", "bad_plan"],
            0,
            0,
            false,
            false,
            "human_gap",
        ),
        (vec!["bad_plan", "patch"], 0, 5, false, false, "human"),
        (vec!["intent_gap"], 0, 5, false, false, "human"),
        (vec!["patch"], 3, 0, false, false, "human"),
        (vec![], 0, 0, false, false, "pass"),
    ];
    for (verdicts, fix, loops, budget, timeout, expected) in cases {
        let mut state = engine.initial_state(counters(fix, loops));
        let mut run_input = input(verdicts.into_iter().map(str::to_owned).collect());
        run_input.budget_exhausted = budget;
        run_input.timeout = timeout;
        let out = engine.run(&mut state, &run_input, 32).unwrap();
        let first = out.destinations.get(1).map(String::as_str).unwrap_or("");
        assert_eq!(first.strip_prefix("hold:").unwrap_or(first), expected);
    }
    for (budget_exhausted, timeout) in [(true, false), (false, true)] {
        let mut state = engine.initial_state(BTreeMap::new());
        let run_input = RunInput {
            budget_exhausted,
            timeout,
            ..input(vec![])
        };
        let outcome = engine
            .run_staged(&mut state, &run_input, 32, |_, _, _| {
                panic!("blocked node must not execute")
            })
            .unwrap();
        assert_eq!(outcome.hold.unwrap().node_id, "human");
        if budget_exhausted {
            assert_eq!(
                engine.resume(&mut state, "fixdie").unwrap_err().code,
                "E_EXEC_BUDGET"
            );
        } else {
            assert!(
                engine.resume(&mut state, "fixdie").unwrap().terminal,
                "timeout is local to the old execution attempt"
            );
        }
    }
    let mut state = engine.initial_state(BTreeMap::new());
    let mut ineligible = input(vec![]);
    ineligible.has_findings = true;
    let result = engine.run(&mut state, &ineligible, 16).unwrap();
    assert!(result.hold.is_some());
    assert!(
        !result
            .actions
            .iter()
            .any(|a| matches!(a, EngineAction::Emit { event, .. } if event == "InspectionPassed"))
    );
}

#[test]
fn resume_target_and_unknown_state_are_rejected() {
    let (engine, _) = setup();
    let mut state = engine.initial_state(BTreeMap::new());
    engine
        .run(&mut state, &input(vec!["intent_gap".into()]), 16)
        .unwrap();
    assert_eq!(
        engine.resume(&mut state, "fixdie").unwrap_err().code,
        "E_EXEC_RESUME_TARGET"
    );
    assert_eq!(
        engine.resume(&mut state, "unknown").unwrap_err().code,
        "E_EXEC_RESUME_TARGET"
    );
    assert_eq!(
        engine.run(&mut state, &input(vec![]), 16).unwrap_err().code,
        "E_EXEC_HELD"
    );
    assert_eq!(
        engine
            .run_staged(&mut state, &input(vec![]), 16, |_, _, _| panic!(
                "held execution"
            ))
            .unwrap_err()
            .code,
        "E_EXEC_HELD"
    );
}

#[test]
fn staged_error_policies_hold_before_routing_and_failed_redrive_does_not_bump() {
    let (engine, _) = setup();
    let mut state = engine.initial_state(BTreeMap::new());
    let outcome = engine
        .run_staged(&mut state, &input(vec![]), 16, |_, _, _| {
            Err(foundry_pack::PackError {
                code: "E_EXEC_VERIFY_INVALID",
                message: "invalid verify".into(),
            })
        })
        .unwrap();
    assert_eq!(outcome.hold.unwrap().node_id, "human");
    assert!(!outcome.terminal);
    for verdict in ["patch", "bad_plan"] {
        let mut state = engine.initial_state(BTreeMap::new());
        let mut visits = vec![];
        let outcome = engine
            .run_staged(&mut state, &input(vec![]), 16, |node, redrive, _| {
                visits.push(node.to_owned());
                if redrive {
                    Err(foundry_pack::PackError {
                        code: "E_EXEC_REDRIVE_ERROR",
                        message: "pre-stage failed".into(),
                    })
                } else {
                    Ok(input(vec![verdict.into()]))
                }
            })
            .unwrap();
        assert_eq!(outcome.hold.unwrap().node_id, "human");
        assert_eq!(visits.len(), 2);
        assert_eq!(state.counters["fix_rounds"], 0);
        assert_eq!(
            state.counters["loopbacks"],
            u64::from(verdict == "bad_plan")
        );
    }
}

#[test]
fn counter_overflow_holds_and_missing_counter_never_falls_through_to_pass() {
    let (engine, _) = setup();
    let mut state = engine.initial_state(counters(0, u64::MAX));
    let outcome = engine
        .run(&mut state, &input(vec!["bad_plan".into()]), 16)
        .unwrap();
    assert_eq!(outcome.hold.unwrap().entry_counters["loopbacks"], u64::MAX);
    assert!(
        !outcome
            .actions
            .iter()
            .any(|a| matches!(a, EngineAction::Emit { .. }))
    );
    let mut state = engine.initial_state(BTreeMap::new());
    state.counters.remove("loopbacks");
    assert_eq!(
        engine
            .run(&mut state, &input(vec!["bad_plan".into()]), 16)
            .unwrap_err()
            .code,
        "E_EXEC_CONDITION"
    );
}

#[test]
fn engine_state_round_trip_preserves_the_next_resumed_outcome() {
    let (engine, _) = setup();
    let mut original = engine.initial_state(BTreeMap::new());
    engine
        .run(&mut original, &input(vec!["intent_gap".into()]), 16)
        .unwrap();
    let json = original.export();
    let mut restored = foundry_pack::EngineState::import(&json).unwrap();
    let left = engine
        .resume_staged(&mut original, "replan_full", |_, _, input, _| {
            Ok(input.clone())
        })
        .unwrap();
    let right = engine
        .resume_staged(&mut restored, "replan_full", |_, _, input, _| {
            Ok(input.clone())
        })
        .unwrap();
    assert_eq!(left, right);
    assert_eq!(original.counters, restored.counters);
}
