use foundry_pack::{Expr, load_pack, parse_expr};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempPack(PathBuf);
impl Drop for TempPack {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn temp_pack() -> TempPack {
    let path = std::env::temp_dir().join(format!(
        "foundry-pack-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    let source = repo_path("skills/fixture-min/v1");
    for name in ["manifest.toml", "policy.toml", "lens-a.md"] {
        fs::copy(source.join(name), path.join(name)).unwrap();
    }
    TempPack(path)
}

fn rehash(path: &std::path::Path) {
    let manifest_path = path.join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path).unwrap();
    let value: toml::Value = toml::from_str(&manifest).unwrap();
    let names = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let mut files = BTreeMap::new();
    for name in names {
        files.insert(name.clone(), fs::read(path.join(name)).unwrap());
    }
    let mut canonical = Vec::new();
    let mut entries = files.into_iter().collect::<Vec<_>>();
    let manifest_without_digest = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with("digest ="))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    entries.push((
        "manifest.toml".to_owned(),
        manifest_without_digest.into_bytes(),
    ));
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (name, bytes) in entries {
        canonical.extend_from_slice(&(name.len() as u64).to_le_bytes());
        canonical.extend_from_slice(name.as_bytes());
        canonical.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        canonical.extend_from_slice(&bytes);
    }
    let digest = Sha256::digest(&canonical)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let updated = manifest
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("digest =") {
                format!("digest = \"{digest}\"")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(manifest_path, updated).unwrap();
}

fn add_nested_file(path: &std::path::Path, name: &str, body: &str) {
    let manifest_path = path.join("manifest.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("\"policy.toml\"]", &format!("\"policy.toml\", \"{name}\"]"));
    fs::write(&manifest_path, manifest).unwrap();
    let file = path.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, body).unwrap();
    rehash(path);
}

fn repo_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn accepts_canonical_packs() {
    load_pack(repo_path("skills/fixture-min/v1")).unwrap();
    load_pack(repo_path("skills/bmad-review-full/v1")).unwrap();
}

#[test]
fn rejects_all_frozen_negative_fixtures_with_their_codes() {
    let root = repo_path("skills/fixture-reject");
    let mut cases: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 18);
    for case in cases {
        let expected = fs::read_to_string(case.join("expected_error.txt")).unwrap();
        let actual = load_pack(&case).unwrap_err();
        let expected = match expected.trim() {
            // The frozen structural rule now names unordered same-session stages explicitly.
            "E_SESSION_PARALLEL" => "E_SESSION_ORDER",
            code => code,
        };
        assert_eq!(actual.code, expected, "fixture {}", case.display());
    }
}

#[test]
fn parser_preserves_literals_and_handles_nested_boolean_expressions() {
    let ast =
        parse_expr(r#"any_in(verdicts, ["", "a)b"]) && !(!count_lt(c, 7) && false)"#).unwrap();
    assert!(matches!(ast, Expr::And(_, _)));
    assert!(parse_expr(r#"any_in(verdicts, ['x'])"#).is_err());
    assert!(parse_expr(r#"true false"#).is_err());
}

#[test]
fn loader_regressions_cover_snapshot_inputs_and_structural_bypasses() {
    let renamed = temp_pack();
    assert!(
        load_pack(&renamed.0).is_ok(),
        "generic directory names must load"
    );

    for (needle, replacement, expected) in [
        ("on_done = \"done\"", "on_done = []", "E_NODE_FIELD"),
        (
            "[[nodes.branches]]\nwhen = \"true\"",
            "[[nodes.branches]]\nwhen = 'any_in(verdicts, [\"x\"])'\ngoto = \"end_hold\"\n\n[[nodes.branches]]\nwhen = \"true\"",
            "E_PROVENANCE",
        ),
        (
            "do = \"emit(InspectionPassed, summary)\"",
            "do = 'revert (origin); bump(c); emit (InspectionPassed, summary)'",
            "E_REVERT_THEN_EMIT",
        ),
    ] {
        let pack = temp_pack();
        let file = pack.0.join("policy.toml");
        let source = fs::read_to_string(&file).unwrap();
        let source = source.replace(needle, replacement);
        fs::write(file, source).unwrap();
        rehash(&pack.0);
        assert_eq!(load_pack(&pack.0).unwrap_err().code, expected);
    }

    let stage_cycle = temp_pack();
    let policy = fs::read_to_string(stage_cycle.0.join("policy.toml")).unwrap().replace(
        "stages = [{ id = \"lens-a\", lenses = [\"lens-a.md\"], role = \"lens\" }]",
        "stages = [{ id = \"a\", lenses = [\"lens-a.md\"], after = \"b\" }, { id = \"b\", lenses = [\"lens-a.md\"], after = \"a\" }]"
    );
    fs::write(stage_cycle.0.join("policy.toml"), policy).unwrap();
    rehash(&stage_cycle.0);
    assert_eq!(load_pack(&stage_cycle.0).unwrap_err().code, "E_STAGE_CYCLE");

    let empty_from = temp_pack();
    add_nested_file(
        &empty_from.0,
        "nested/mapping.toml",
        "[[output]]\nname = \"nested\"\nfrom = \"\"\n",
    );
    assert_eq!(
        load_pack(&empty_from.0).unwrap_err().code,
        "E_OUTPUT_FROM_EMPTY"
    );
    let nested_duplicate = temp_pack();
    add_nested_file(
        &nested_duplicate.0,
        "nested/mapping.toml",
        "[[output]]\nname = \"same\"\n\n[[output]]\nname = \"same\"\n",
    );
    assert_eq!(
        load_pack(&nested_duplicate.0).unwrap_err().code,
        "E_OUTPUT_NAME_DUPLICATE"
    );

    let max_counter = temp_pack();
    let policy = fs::read_to_string(max_counter.0.join("policy.toml")).unwrap()
        + "\n[counters]\nitems = 9223372036854775807\n";
    fs::write(max_counter.0.join("policy.toml"), policy).unwrap();
    rehash(&max_counter.0);
    assert!(load_pack(&max_counter.0).is_ok());
    let negative_counter = temp_pack();
    let policy = fs::read_to_string(negative_counter.0.join("policy.toml")).unwrap()
        + "\n[counters]\nitems = -1\n";
    fs::write(negative_counter.0.join("policy.toml"), policy).unwrap();
    rehash(&negative_counter.0);
    assert_eq!(
        load_pack(&negative_counter.0).unwrap_err().code,
        "E_COUNTER_TYPE"
    );

    let unowned_path = temp_pack();
    let policy = fs::read_to_string(unowned_path.0.join("policy.toml")).unwrap()
        .replace(
            "[[nodes.branches]]\nwhen = \"true\"\ndo = \"emit(InspectionPassed, summary)\"",
            "[[nodes.branches]]\nwhen = \"count_lt(items, 1)\"\ngoto = \"alt\"\n\n[[nodes.branches]]\nwhen = \"true\"\ndo = \"emit(InspectionPassed, summary)\"\n\n[[nodes]]\nid = \"alt\"\nkind = \"route\"\n[[nodes.branches]]\nwhen = \"true\"\ngoto = \"scan\"",
        ) + "\n[counters]\nitems = 0\n";
    fs::write(unowned_path.0.join("policy.toml"), policy).unwrap();
    rehash(&unowned_path.0);
    assert_eq!(
        load_pack(&unowned_path.0).unwrap_err().code,
        "E_COUNTER_GUARD_MISSING"
    );
}
