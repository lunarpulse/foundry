use foundry_pack::{load_pack, EngineAction, GraphEngine, PackError, RunInput};
use std::collections::BTreeMap;

const PLAIN_DIGEST: &str = "48029eed0f940c61c7d7a01132234a5d81eb4c261a2ee34af4ea18d60f6fcf29";
const GOLDEN_DIGEST: &str = "e925fd014d367c4c7f3d83fe92d03cd4bc4d3b7cbc1ecdf0d4e77320acab1db8";
const GOLDEN_FIXTURE: &str = include_str!("fixtures/golden_trace_v1.json");

fn pack_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../skills/{name}/v1"))
}

fn run_input(verdicts: &[&str], has_findings: bool) -> RunInput {
    RunInput {
        verdicts: verdicts.iter().map(|value| (*value).to_owned()).collect(),
        has_findings,
        budget_exhausted: false,
        timeout: false,
    }
}

fn plain_engine() -> (GraphEngine, foundry_pack::PackSnapshot) {
    let pack = load_pack(pack_path("plain-cr-review")).unwrap();
    assert_eq!(pack.id(), "plain-cr-review");
    assert_eq!(pack.digest(), PLAIN_DIGEST);
    let engine = GraphEngine::from_snapshot(&pack).unwrap();
    (engine, pack)
}

#[test]
fn plain_pack_loads_routes_and_runs_alongside_the_golden_pack_in_one_binary() {
    let (engine, pack) = plain_engine();
    assert_eq!(pack.counters(), &BTreeMap::from([("fix_rounds".to_owned(), 0)]));
    assert_eq!(
        pack.policy()["nodes"][0]["side_records"].as_array().unwrap()[0].as_str(),
        Some("comments")
    );

    // First blocking result uses one fix redrive; the blocking recheck reaches
    // the human hold at the cap. The redrive node alone increments the counter.
    let mut state = engine.initial_state(BTreeMap::new());
    let mut review_visits = 0;
    let outcome = engine
        .run_staged(&mut state, &run_input(&[], false), 32, |node, redrive, input| {
            match (node, redrive) {
                ("review", false) => {
                    review_visits += 1;
                    Ok(run_input(&["blocking"], true))
                }
                ("fix", true) => Ok(input.clone()),
                _ => panic!("unexpected boundary: {node}, redrive={redrive}"),
            }
        })
        .unwrap();
    assert_eq!(review_visits, 2);
    assert_eq!(outcome.hold.as_ref().unwrap().node_id, "hold");
    assert_eq!(outcome.hold.as_ref().unwrap().entry_counters["fix_rounds"], 1);
    assert_eq!(state.counters["fix_rounds"], 1);
    assert_eq!(outcome.actions, vec![EngineAction::Bump { counter: "fix_rounds".into(), after: 1 }]);
    assert!(outcome.destinations.contains(&"fix".to_owned()));

    // A clean confirmation emits InspectionPassed.
    let mut state = engine.initial_state(BTreeMap::new());
    let clean = engine
        .run_staged(&mut state, &run_input(&[], false), 16, |node, redrive, _| {
            assert_eq!((node, redrive), ("review", false));
            Ok(run_input(&["clean"], false))
        })
        .unwrap();
    assert!(clean.terminal);
    assert!(clean.actions.contains(&EngineAction::Emit {
        event: "InspectionPassed".into(),
        payload: "summary".into(),
    }));

    // Non-blocking findings are held at the comments terminal after dispatch
    // has published the configured comments side record.
    let mut state = engine.initial_state(BTreeMap::new());
    let nit = engine
        .run_staged(&mut state, &run_input(&[], false), 16, |node, redrive, _| {
            assert_eq!((node, redrive), ("review", false));
            Ok(run_input(&["nit"], true))
        })
        .unwrap();
    assert_eq!(nit.hold.as_ref().unwrap().node_id, "commented");

    // Budget exhaustion and invalid verify output both fail closed to hold.
    let mut state = engine.initial_state(BTreeMap::new());
    let budget = engine
        .run_staged(
            &mut state,
            &RunInput { budget_exhausted: true, ..run_input(&[], false) },
            16,
            |_, _, _| panic!("exhausted budget must stop before dispatch"),
        )
        .unwrap();
    assert_eq!(budget.hold.as_ref().unwrap().node_id, "hold");

    let mut state = engine.initial_state(BTreeMap::new());
    let invalid = engine
        .run_staged(&mut state, &run_input(&[], false), 16, |_, _, _| {
            Err(PackError { code: "E_EXEC_VERIFY_INVALID", message: "invalid verify".into() })
        })
        .unwrap();
    assert_eq!(invalid.hold.as_ref().unwrap().node_id, "hold");

    // Load and execute the existing golden review pack in this same integration
    // test binary, using its frozen fixture digest as the independent reference.
    let golden: serde_json::Value = serde_json::from_str(GOLDEN_FIXTURE).unwrap();
    let golden_pack = load_pack(pack_path("bmad-review-full")).unwrap();
    assert_eq!(golden_pack.digest(), GOLDEN_DIGEST);
    assert_eq!(golden["pack_digest_expected"].as_str(), Some(GOLDEN_DIGEST));
    let golden_engine = GraphEngine::from_snapshot(&golden_pack).unwrap();
    let mut state = golden_engine.initial_state(BTreeMap::new());
    let golden_outcome = golden_engine
        .run_staged(&mut state, &run_input(&[], false), 16, |node, redrive, input| {
            assert_eq!((node, redrive), ("lenses", false));
            Ok(input.clone())
        })
        .unwrap();
    assert!(golden_outcome.terminal);
    assert!(golden_outcome.actions.contains(&EngineAction::Emit {
        event: "InspectionPassed".into(),
        payload: "summary".into(),
    }));
}
