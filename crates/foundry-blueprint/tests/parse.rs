//! T4 — foundry.yaml (Blueprint) parser, TDD.
use foundry_blueprint::{Blueprint, BlueprintError};

const VALID: &str = r#"
schemaVersion: v1alpha1
name: personal-factory
description: keeps my repos healthy
repositories:
  - owner: lunarpulse
    name: contextmesh-rs
agentDefaults:
  profile: coding
lines:
  - name: dep-bump
    intake:
      cron: "0 9 * * *"
    stages: [fit, make, bake, inspect, ship]
    harness:
      type: rustain-cli
      profile: coding
    approval: human
inspector:
  scorer: default
"#;

#[test]
fn parses_valid_blueprint() {
    let bp = Blueprint::parse(VALID).unwrap();
    assert_eq!(bp.name, "personal-factory");
    assert_eq!(bp.schema_version, "v1alpha1");
    assert_eq!(bp.repositories.len(), 1);
    assert_eq!(bp.repositories[0].owner, "lunarpulse");
    assert_eq!(bp.repositories[0].name, "contextmesh-rs");
    assert_eq!(bp.agent_defaults.profile, "coding");
    assert_eq!(bp.lines.len(), 1);
    let line = &bp.lines[0];
    assert_eq!(line.name, "dep-bump");
    assert_eq!(line.harness.r#type, "rustain-cli");
    assert_eq!(line.harness.profile, "coding");
    assert_eq!(line.approval, foundry_blueprint::ApprovalPolicy::Human);
    assert_eq!(line.stages, vec!["fit", "make", "bake", "inspect", "ship"]);
}

#[test]
fn rejects_missing_name() {
    let bad = "schemaVersion: v1alpha1\nrepositories: []\n";
    let err = Blueprint::parse(bad).unwrap_err();
    assert!(matches!(err, BlueprintError::Invalid(_)));
}

#[test]
fn rejects_unknown_schema_version() {
    let bad = format!("schemaVersion: v9\nname: x\n");
    assert!(Blueprint::parse(&bad).is_err());
}

#[test]
fn rejects_empty_repositories() {
    let bad = "schemaVersion: v1alpha1\nname: x\nrepositories: []\n";
    assert!(Blueprint::parse(bad).is_err());
}

#[test]
fn rejects_line_without_harness() {
    let bad = r#"
schemaVersion: v1alpha1
name: x
repositories:
  - owner: a
    name: b
lines:
  - name: dep-bump
    approval: human
"#;
    assert!(Blueprint::parse(bad).is_err());
}

#[test]
fn rejects_yaml_syntax_error() {
    assert!(matches!(
        Blueprint::parse("this: [is: not: valid"),
        Err(BlueprintError::Yaml(_))
    ));
}

#[test]
fn multiple_repositories_ok() {
    let multi = r#"
schemaVersion: v1alpha1
name: x
repositories:
  - owner: a
    name: b
  - owner: c
    name: d
"#;
    let bp = Blueprint::parse(multi).unwrap();
    assert_eq!(bp.repositories.len(), 2);
}
