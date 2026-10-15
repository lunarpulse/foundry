//! Methodology-neutral loader and validator for Foundry review packs.
mod executor;
pub use executor::{EngineAction, EngineState, GraphEngine, HoldEntry, RunInput, StepOutcome};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct PackError {
    pub code: &'static str,
    pub message: String,
}
impl PackError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
pub type Result<T> = std::result::Result<T, PackError>;

#[derive(Clone, Debug)]
pub struct PackSnapshot {
    root: PathBuf,
    id: String,
    version: String,
    kind: String,
    digest: String,
    files: BTreeMap<String, Vec<u8>>,
    policy: toml::Value,
    nodes: Vec<Node>,
    counters: BTreeMap<String, u64>,
    outputs: Vec<Output>,
}
impl PackSnapshot {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.files
    }
    pub fn policy(&self) -> &toml::Value {
        &self.policy
    }
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }
    pub fn counters(&self) -> &BTreeMap<String, u64> {
        &self.counters
    }
    pub fn outputs(&self) -> &[Output] {
        &self.outputs
    }
}
#[derive(Clone, Debug)]
pub struct Node {
    pub id: String,
    pub kind: String,
    pub value: toml::Value,
}
#[derive(Clone, Debug)]
pub struct Output {
    pub name: String,
    pub from: Option<String>,
    pub schema: Option<String>,
}

/// Load, freeze, hash, parse, and validate a pack directory.
pub fn load_pack(path: impl AsRef<Path>) -> Result<PackSnapshot> {
    let root = path
        .as_ref()
        .canonicalize()
        .map_err(|e| PackError::new("E_PACK_IO", e.to_string()))?;
    let manifest_path = root.join("manifest.toml");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|e| PackError::new("E_MANIFEST_MISSING", e.to_string()))?;
    check_lf(&manifest_bytes, "manifest.toml")?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)
        .map_err(|e| PackError::new("E_MANIFEST_TOML", e.to_string()))?;
    if manifest_text
        .lines()
        .any(|l| l.trim_start().starts_with("\"digest\""))
    {
        return Err(PackError::new(
            "E_DIGEST_KEY_QUOTED",
            "digest key must be unquoted",
        ));
    }
    validate_digest_syntax(manifest_text)?;
    let manifest: Manifest = toml::from_str(manifest_text)
        .map_err(|e| PackError::new("E_MANIFEST_TOML", e.to_string()))?;
    if manifest.files.is_empty() {
        return Err(PackError::new(
            "E_MANIFEST_FILES",
            "files must not be empty",
        ));
    }
    let mut files = BTreeMap::new();
    let mut declared = BTreeSet::new();
    for name in &manifest.files {
        let rel = safe_relative(name)?;
        if !declared.insert(name.clone()) {
            return Err(PackError::new(
                "E_MANIFEST_FILES",
                format!("duplicate file {name}"),
            ));
        }
        let p = root.join(&rel);
        let bytes =
            fs::read(&p).map_err(|e| PackError::new("E_FILE_MISSING", format!("{name}: {e}")))?;
        check_lf(&bytes, name)?;
        files.insert(name.clone(), bytes);
    }
    // Every TOML file is parsed and participates in output uniqueness; files referenced
    // by the manifest (and manifest itself) alone participate in the canonical digest.
    let toml_paths = collect_toml(&root)?;
    for rel in &toml_paths {
        let bytes = if rel == "manifest.toml" {
            &manifest_bytes
        } else if let Some(bytes) = files.get(rel) {
            bytes
        } else {
            let bytes =
                fs::read(root.join(rel)).map_err(|e| PackError::new("E_PACK_IO", e.to_string()))?;
            files.insert(rel.clone(), bytes);
            files.get(rel).expect("just inserted")
        };
        check_lf(bytes, rel)?;
        let s = std::str::from_utf8(bytes)
            .map_err(|e| PackError::new("E_TOML", format!("{rel}: {e}")))?;
        let value: toml::Value =
            toml::from_str(s).map_err(|e| PackError::new("E_TOML", format!("{rel}: {e}")))?;
        if let Some(arr) = value.get("output").and_then(toml::Value::as_array) {
            for item in arr {
                if let Some(t) = item.as_table()
                    && t.contains_key("name")
                { /* collected below */ }
            }
        }
    }
    // Closure hygiene: every *.toml in the pack (recursively) except
    // manifest.toml itself must be declared in the manifest — an
    // undeclared TOML file could smuggle structure past the digest
    // closure. Declared NON-TOML assets (lens markdown etc.) are fine:
    // their existence was verified by the read above.
    for rel in &toml_paths {
        if rel != "manifest.toml" && !declared.contains(rel) {
            return Err(PackError::new(
                "E_MANIFEST_FILES",
                format!("undeclared toml file in pack: {rel}"),
            ));
        }
    }
    // Canonical bytes follow §3.1: UTF-8 byte order on POSIX paths, then
    // u64le(path length) || path || u64le(content length) || content.
    // The single manifest digest line is removed before inserting manifest.toml.
    let mut canonical = Vec::new();
    let mut digest_entries: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter(|(k, _)| declared.contains(*k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut manifest_canonical = manifest_bytes.clone();
    strip_digest_line(&mut manifest_canonical)?;
    digest_entries.push(("manifest.toml".to_string(), manifest_canonical));
    digest_entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (name, content) in digest_entries {
        put_field(&mut canonical, name.as_bytes());
        put_field(&mut canonical, &content);
    }
    let digest = hex(&Sha256::digest(&canonical));
    if digest != manifest.digest {
        return Err(PackError::new(
            "E_DIGEST_MISMATCH",
            format!("expected {}, got {digest}", manifest.digest),
        ));
    }

    let policy_bytes = files.get("policy.toml").ok_or_else(|| {
        PackError::new(
            "E_POLICY_MISSING",
            "manifest files must include policy.toml",
        )
    })?;
    let policy: toml::Value = toml::from_str(
        std::str::from_utf8(policy_bytes)
            .map_err(|e| PackError::new("E_POLICY_TOML", e.to_string()))?,
    )
    .map_err(|e| PackError::new("E_POLICY_TOML", e.to_string()))?;
    let counters = parse_counters(&policy)?;
    let nodes = parse_nodes(&policy)?;
    let outputs = collect_outputs(&files, &toml_paths)?;
    validate(
        manifest.kind.as_str(),
        &policy,
        &nodes,
        &counters,
        &outputs,
        &files,
        &root,
    )?;
    Ok(PackSnapshot {
        root,
        id: manifest.id,
        version: manifest.version,
        kind: manifest.kind,
        digest,
        files,
        policy,
        nodes,
        counters,
        outputs,
    })
}

#[derive(Deserialize)]
struct Manifest {
    id: String,
    version: String,
    kind: String,
    files: Vec<String>,
    digest: String,
}
fn check_lf(b: &[u8], name: &str) -> Result<()> {
    if !b.ends_with(b"\n") || b.windows(2).any(|w| w == b"\r\n") || b.contains(&b'\r') {
        return Err(PackError::new(
            "E_LF_REQUIRED",
            format!("{name} must use LF and end in LF"),
        ));
    }
    Ok(())
}
fn safe_relative(s: &str) -> Result<PathBuf> {
    let p = Path::new(s);
    if p.is_absolute()
        || p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(PackError::new(
            "E_MANIFEST_FILES",
            format!("unsafe path {s}"),
        ));
    }
    Ok(p.to_path_buf())
}
fn collect_toml(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out)?
            } else if p.extension().and_then(|x| x.to_str()) == Some("toml") {
                out.push(
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(root, root, &mut v).map_err(|e| PackError::new("E_PACK_IO", e.to_string()))?;
    v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(v)
}
fn strip_digest_line(b: &mut Vec<u8>) -> Result<()> {
    let s = std::str::from_utf8(b).map_err(|e| PackError::new("E_MANIFEST_TOML", e.to_string()))?;
    let mut out = Vec::new();
    let mut count = 0;
    for line in s.split_inclusive('\n') {
        let trim = line.trim_start();
        if trim.starts_with("digest") && trim[6..].trim_start().starts_with('=') {
            count += 1;
            continue;
        }
        out.extend_from_slice(line.as_bytes());
    }
    if count != 1 {
        return Err(PackError::new(
            "E_DIGEST_KEY",
            "manifest must have exactly one digest line",
        ));
    }
    *b = out;
    Ok(())
}
fn validate_digest_syntax(text: &str) -> Result<()> {
    let lines: Vec<_> = text
        .lines()
        .filter(|line| {
            let s = line.trim_start();
            s.starts_with("digest") && s[6..].trim_start().starts_with('=')
        })
        .collect();
    if lines.len() != 1 {
        return Err(PackError::new(
            "E_DIGEST_KEY",
            "manifest must have exactly one digest line",
        ));
    }
    let value = lines[0].trim_start()[6..].trim_start();
    let rhs = value.strip_prefix('=').unwrap().trim();
    if !rhs.starts_with('"') || rhs.starts_with("\"\"\"") {
        return Err(PackError::new(
            "E_DIGEST_SYNTAX",
            "digest must be a single-line basic string",
        ));
    }
    let end = rhs[1..].find('"').map(|i| i + 1).ok_or_else(|| {
        PackError::new(
            "E_DIGEST_SYNTAX",
            "digest must be a single-line basic string",
        )
    })?;
    let rest = rhs[end + 1..].trim();
    if !rest.is_empty() && !rest.starts_with('#') {
        return Err(PackError::new(
            "E_DIGEST_SYNTAX",
            "digest must be a single-line basic string",
        ));
    }
    Ok(())
}
fn put_field(out: &mut Vec<u8>, x: &[u8]) {
    out.extend_from_slice(&(x.len() as u64).to_le_bytes());
    out.extend_from_slice(x);
}
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
fn parse_counters(p: &toml::Value) -> Result<BTreeMap<String, u64>> {
    let mut out = BTreeMap::new();
    if let Some(t) = p.get("counters").and_then(toml::Value::as_table) {
        for (k, v) in t {
            let i = v.as_integer().filter(|_| !v.is_bool()).ok_or_else(|| {
                PackError::new("E_COUNTER_TYPE", format!("{k} must be a non-boolean u64"))
            })?;
            if i < 0 {
                return Err(PackError::new(
                    "E_COUNTER_TYPE",
                    format!("{k} must be nonnegative"),
                ));
            }
            // §2.4b: “카운터: u64, bump만 증가, 감소 없음, overflow 시 엔진 hold(래핑 방지).”
            // TOML integers are i64, so TOML can express only the nonnegative i64 subset;
            // u64::MAX remains available to the in-memory engine API, not TOML input.
            out.insert(k.clone(), i as u64);
        }
    }
    Ok(out)
}
fn parse_nodes(p: &toml::Value) -> Result<Vec<Node>> {
    let a = p
        .get("nodes")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| PackError::new("E_NODES", "nodes must be an array"))?;
    let mut out = Vec::new();
    for n in a {
        let t = n
            .as_table()
            .ok_or_else(|| PackError::new("E_NODE", "node must be table"))?;
        let id = t
            .get("id")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| PackError::new("E_NODE_ID", "node id required"))?
            .to_string();
        let kind = t
            .get("kind")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| PackError::new("E_NODE_KIND", "node kind required"))?
            .to_string();
        if !["dispatch", "route", "redrive", "emit", "hold"].contains(&kind.as_str()) {
            return Err(PackError::new(
                "E_NODE_KIND",
                format!("unknown kind {kind}"),
            ));
        }
        out.push(Node {
            id,
            kind,
            value: n.clone(),
        });
    }
    Ok(out)
}
fn collect_outputs(files: &BTreeMap<String, Vec<u8>>, paths: &[String]) -> Result<Vec<Output>> {
    let mut out = Vec::new();
    for f in paths {
        let Some(b) = files.get(f) else { continue };
        let s = std::str::from_utf8(b).map_err(|e| PackError::new("E_TOML", e.to_string()))?;
        let v: toml::Value =
            toml::from_str(s).map_err(|e| PackError::new("E_TOML", e.to_string()))?;
        if let Some(arr) = v.get("output").and_then(toml::Value::as_array) {
            for x in arr {
                let t = x.as_table().ok_or_else(|| {
                    PackError::new(
                        "E_SCHEMA_MISSING_FIELD",
                        "output declaration must be a table",
                    )
                })?;
                let n = t.get("name").and_then(toml::Value::as_str).ok_or_else(|| {
                    PackError::new(
                        "E_SCHEMA_MISSING_FIELD",
                        "output declaration requires string name",
                    )
                })?;
                let from = match t.get("from") {
                    None => None,
                    Some(v) => Some(
                        v.as_str()
                            .ok_or_else(|| {
                                PackError::new(
                                    "E_SCHEMA_MISSING_FIELD",
                                    "output from must be a string",
                                )
                            })?
                            .to_owned(),
                    ),
                };
                out.push(Output {
                    name: n.into(),
                    from,
                    schema: t
                        .get("schema")
                        .and_then(toml::Value::as_str)
                        .map(str::to_owned),
                });
            }
        }
    }
    let mut names = HashSet::new();
    let mut froms = HashSet::new();
    for o in &out {
        if !names.insert(o.name.clone()) {
            return Err(PackError::new(
                "E_OUTPUT_NAME_DUPLICATE",
                format!("duplicate output name {}", o.name),
            ));
        }
        if let Some(f) = &o.from {
            if f.is_empty() {
                return Err(PackError::new(
                    "E_OUTPUT_FROM_EMPTY",
                    "empty output from column",
                ));
            }
            if !froms.insert(f.clone()) {
                return Err(PackError::new(
                    "E_OUTPUT_FROM_DUPLICATE",
                    format!("duplicate output from {f}"),
                ));
            }
        }
    }
    Ok(out)
}

fn validate(
    kind: &str,
    p: &toml::Value,
    nodes: &[Node],
    counters: &BTreeMap<String, u64>,
    outputs: &[Output],
    files: &BTreeMap<String, Vec<u8>>,
    root: &Path,
) -> Result<()> {
    let mut by = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if by.insert(n.id.clone(), i).is_some() {
            return Err(PackError::new(
                "E_NODE_ID_DUPLICATE",
                format!("duplicate node {}", n.id),
            ));
        }
        if n.value.get("timeout_s").is_some() && n.kind != "hold" {
            return Err(PackError::new(
                "E_TIMEOUT_NODE_KIND",
                format!("{} timeout_s only allowed on hold", n.id),
            ));
        }
        if n.kind == "hold"
            && let Some(t) = n.value.get("timeout_s")
            && t.as_integer().filter(|x| *x > 0).is_none()
        {
            return Err(PackError::new(
                "E_TIMEOUT_TYPE",
                "hold timeout_s must be positive integer",
            ));
        }
    }
    let timeout = p
        .get("hold")
        .and_then(|x| x.get("timeout_s"))
        .ok_or_else(|| PackError::new("E_TIMEOUT_TYPE", "[hold].timeout_s is required"))?;
    if timeout.as_integer().filter(|x| *x >= 0).is_none() {
        return Err(PackError::new(
            "E_TIMEOUT_TYPE",
            "[hold].timeout_s must be nonnegative integer",
        ));
    }
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    let mut verify = 0usize;
    for node in nodes {
        if let Some(stages) = node.value.get("stages").and_then(toml::Value::as_array) {
            verify += stages
                .iter()
                .filter(|stage| stage.get("role").and_then(toml::Value::as_str) == Some("verify"))
                .count();
        }
    }
    if let Some(stages) = p.get("pack_stages").and_then(toml::Value::as_array) {
        verify += stages
            .iter()
            .filter(|stage| stage.get("role").and_then(toml::Value::as_str) == Some("verify"))
            .count();
    }
    if let Some(pack_stages) = p.get("pack_stages").and_then(toml::Value::as_array) {
        let mut ids = HashSet::new();
        for stage in pack_stages {
            let id = stage
                .get("id")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| PackError::new("E_STAGE", "pack stage id required"))?;
            if !ids.insert(id) {
                return Err(PackError::new("E_STAGE_ID_DUPLICATE", id));
            }
            if let Some(role) = stage.get("role").and_then(toml::Value::as_str)
                && !["lens", "verify", "replan"].contains(&role)
            {
                return Err(PackError::new("E_ROLE", "invalid pack stage role"));
            }
            let lenses = stage
                .get("lenses")
                .and_then(toml::Value::as_array)
                .ok_or_else(|| PackError::new("E_STAGE", "pack stage lenses required"))?;
            if lenses.is_empty()
                || lenses
                    .iter()
                    .any(|lens| lens.as_str().is_none_or(|name| !files.contains_key(name)))
            {
                return Err(PackError::new(
                    "E_STAGE",
                    format!("pack stage {id} must reference declared lens files"),
                ));
            }
        }
    }
    for n in nodes {
        let t = n
            .value
            .as_table()
            .ok_or_else(|| PackError::new("E_NODE", "node must be a table"))?;
        match n.kind.as_str() {
            "dispatch" => {
                // on_verify_invalid / on_no_valid are ERROR-POLICY bindings:
                // their absence is classified E_MISSING_ERROR_POLICY by the
                // frozen fixture 17 (F5-1 §2.5), not E_NODE_FIELD.
                for k in ["on_verify_invalid", "on_no_valid"] {
                    if !t.contains_key(k) {
                        return Err(PackError::new(
                            "E_MISSING_ERROR_POLICY",
                            format!("{} missing {k}", n.id),
                        ));
                    }
                    if !matches!(
                        t.get(k).and_then(toml::Value::as_str),
                        Some("hold" | "fail-run")
                    ) {
                        return Err(PackError::new(
                            "E_MISSING_ERROR_POLICY",
                            format!("{}.{} must be hold or fail-run", n.id, k),
                        ));
                    }
                }
                for k in ["stages", "on_done", "hold_target"] {
                    if !t.contains_key(k) {
                        return Err(PackError::new(
                            "E_NODE_FIELD",
                            format!("{} missing {k}", n.id),
                        ));
                    }
                }
                let stages = t["stages"]
                    .as_array()
                    .ok_or_else(|| PackError::new("E_STAGES", "stages must be array"))?;
                if stages.is_empty() {
                    return Err(PackError::new("E_STAGES", "dispatch needs stages"));
                }
                let mut sessions: HashMap<String, Vec<String>> = HashMap::new();
                let stage_ids: HashSet<String> = stages
                    .iter()
                    .filter_map(|stage| stage.get("id").and_then(toml::Value::as_str))
                    .map(str::to_owned)
                    .collect();
                if stage_ids.len() != stages.len() {
                    return Err(PackError::new("E_STAGE_ID_DUPLICATE", n.id.clone()));
                }
                for s in stages {
                    let st = s
                        .as_table()
                        .ok_or_else(|| PackError::new("E_STAGE", "stage must be a table"))?;
                    let id = st
                        .get("id")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| PackError::new("E_STAGE", "stage id required"))?;
                    let lenses = st
                        .get("lenses")
                        .and_then(toml::Value::as_array)
                        .ok_or_else(|| PackError::new("E_STAGE", "stage lenses required"))?;
                    if lenses.is_empty()
                        || lenses
                            .iter()
                            .any(|lens| lens.as_str().is_none_or(|name| !files.contains_key(name)))
                    {
                        return Err(PackError::new(
                            "E_STAGE",
                            format!("stage {id} must reference declared lens files"),
                        ));
                    }
                    if let Some(role) = st.get("role").and_then(toml::Value::as_str)
                        && !["lens", "verify", "replan"].contains(&role)
                    {
                        return Err(PackError::new("E_ROLE", "invalid stage role"));
                    }
                    if let Some(after) = st.get("after") {
                        let deps: Vec<&str> = match after {
                            toml::Value::String(value) => vec![value],
                            toml::Value::Array(values) => values
                                .iter()
                                .map(|value| {
                                    value.as_str().ok_or_else(|| {
                                        PackError::new(
                                            "E_STAGE_REF",
                                            "after entries must be strings",
                                        )
                                    })
                                })
                                .collect::<Result<_>>()?,
                            _ => {
                                return Err(PackError::new(
                                    "E_STAGE_REF",
                                    "after must be a string or string array",
                                ));
                            }
                        };
                        for dep in deps {
                            if !stage_ids.contains(dep) || dep == id {
                                return Err(PackError::new(
                                    "E_STAGE_REF",
                                    format!("invalid after reference {dep}"),
                                ));
                            }
                        }
                    }
                    if let Some(se) = st.get("session").and_then(toml::Value::as_str) {
                        sessions.entry(se.into()).or_default().push(id.into())
                    }
                    if let Some(sc) = st.get("output_schema").and_then(toml::Value::as_str)
                        && !files.contains_key(sc)
                    {
                        return Err(PackError::new(
                            "E_SCHEMA_MISSING_FIELD",
                            format!("missing schema {sc}"),
                        ));
                    }
                }
                for v in sessions.values() {
                    if v.len() > 1
                        && v.iter().enumerate().any(|(i, a)| {
                            v.iter().skip(i + 1).any(|b| {
                                !stage_reaches(stages, a, b) && !stage_reaches(stages, b, a)
                            })
                        })
                    {
                        return Err(PackError::new(
                            "E_SESSION_ORDER",
                            "same-session stages must be totally ordered by after chains",
                        ));
                    }
                }
                add_edge(&mut edges, &n.id, node_str(t, &n.id, "on_done")?);
                add_edge(&mut edges, &n.id, node_str(t, &n.id, "hold_target")?);
            }
            "route" => {
                if t.contains_key("on_done") {
                    return Err(PackError::new("E_NODE_FIELD", "route forbids on_done"));
                }
                let bs = t
                    .get("branches")
                    .and_then(toml::Value::as_array)
                    .ok_or_else(|| PackError::new("E_BRANCHES", "route branches required"))?;
                if bs.is_empty()
                    || bs
                        .last()
                        .and_then(|b| b.get("when"))
                        .and_then(toml::Value::as_str)
                        != Some("true")
                {
                    return Err(PackError::new("E_NO_TRUE_FALLBACK", n.id.clone()));
                }
                for b in bs {
                    if b.get("goto").is_some()
                        && b.get("goto").and_then(toml::Value::as_str).is_none()
                    {
                        return Err(PackError::new(
                            "E_NODE_FIELD",
                            "branch goto must be a string",
                        ));
                    }
                    let when = b
                        .get("when")
                        .and_then(toml::Value::as_str)
                        .ok_or_else(|| PackError::new("E_DSL_SYNTAX", "when required"))?;
                    let ast = parse_expr(when)?;
                    validate_expr(&ast, counters, verify > 0)?;
                    let go = b.get("goto").and_then(toml::Value::as_str);
                    let acts = b.get("do").and_then(toml::Value::as_str);
                    let actions = acts.map(parse_actions).transpose()?;
                    if let Some(ref a) = actions {
                        let emits = a.iter().filter(|x| matches!(x, Action::Emit(..))).count();
                        if emits > 1 {
                            return Err(PackError::new(
                                "E_MULTI_EMIT",
                                "branch has multiple emits",
                            ));
                        }
                        let mut saw_revert = false;
                        for action in a {
                            if matches!(action, Action::Emit(..)) && saw_revert {
                                return Err(PackError::new(
                                    "E_REVERT_THEN_EMIT",
                                    "emit follows revert in do sequence",
                                ));
                            }
                            saw_revert |= matches!(action, Action::Revert(_));
                        }
                        if emits == 1 && a.last().is_some_and(|x| !matches!(x, Action::Emit(..))) {
                            return Err(PackError::new("E_POST_EMIT_ACTION", "action after emit"));
                        }
                        if emits == 1 && go.is_some() {
                            return Err(PackError::new("E_EMIT_GOTO", "emit branch cannot goto"));
                        }
                        if emits == 0 && go.is_none() {
                            return Err(PackError::new(
                                "E_NONTERMINAL_DO_NO_GOTO",
                                "nonterminal action needs goto",
                            ));
                        }
                    }
                    if let Some(g) = go {
                        add_edge(&mut edges, &n.id, g)
                    }
                    if let Some(a) = actions {
                        for x in a {
                            match x {
                                Action::Bump(c) => {
                                    if !counters.contains_key(&c) {
                                        return Err(PackError::new("E_COUNTER_REF", c));
                                    }
                                }
                                Action::Emit(ev, pay) => {
                                    if !["InspectionPassed", "InspectionFailed"]
                                        .contains(&ev.as_str())
                                    {
                                        return Err(PackError::new("E_EVENT", ev));
                                    }
                                    if pay.is_empty()
                                        || (pay != "summary"
                                            && !outputs.iter().any(|o| o.name == pay))
                                    {
                                        return Err(PackError::new(
                                            "E_SCHEMA_MISSING_FIELD",
                                            format!("unknown output payload {pay}"),
                                        ));
                                    }
                                }
                                Action::Revert(base) => {
                                    if !["origin", "latest"].contains(&base.as_str()) {
                                        return Err(PackError::new(
                                            "E_ACTION",
                                            "invalid revert base",
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "redrive" => {
                // "payload" is validated separately with the frozen
                // fixture-03 code (E_SCHEMA_MISSING_FIELD), not here.
                for k in ["target", "on_done", "hold_target", "on_redrive_error"] {
                    if !t.contains_key(k) {
                        return Err(PackError::new(
                            "E_NODE_FIELD",
                            format!("{} missing {k}", n.id),
                        ));
                    }
                }
                if t.get("target").and_then(toml::Value::as_str).is_none() {
                    return Err(PackError::new(
                        "E_NODE_FIELD",
                        format!("{}.target must be a string", n.id),
                    ));
                }
                if !matches!(
                    t.get("on_redrive_error").and_then(toml::Value::as_str),
                    Some("hold" | "fail-run")
                ) {
                    return Err(PackError::new(
                        "E_MISSING_ERROR_POLICY",
                        format!("{}.on_redrive_error must be hold or fail-run", n.id),
                    ));
                }
                // (F12-2 / §2.3) payload is MANDATORY on redrive (frozen
                // fixture 03 classifies its absence as E_SCHEMA_MISSING_FIELD).
                // It is a non-empty string; §2.5 defines standard refs
                // (summary/findings/cumulative_findings) but pack-declared
                // names (e.g. replan outputs) are legal — no name-set
                // restriction at load time.
                match t.get("payload").and_then(toml::Value::as_str) {
                    Some(p) if !p.is_empty() => {}
                    _ => {
                        return Err(PackError::new(
                            "E_SCHEMA_MISSING_FIELD",
                            format!("redrive {} missing payload", n.id),
                        ));
                    }
                }
                for k in ["on_done", "hold_target"] {
                    add_edge(&mut edges, &n.id, node_str(t, &n.id, k)?)
                }
                if let Some(value) = t.get("base")
                    && !matches!(value.as_str(), Some("origin" | "latest"))
                {
                    return Err(PackError::new(
                        "E_NODE_FIELD",
                        "redrive base must be origin or latest",
                    ));
                }
                if let Some(value) = t.get("pre_stage") {
                    let stage = value.as_str().ok_or_else(|| {
                        PackError::new("E_NODE_FIELD", "redrive pre_stage must be a string")
                    })?;
                    let registered = p
                        .get("pack_stages")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|xs| {
                            xs.iter()
                                .any(|x| x.get("id").and_then(toml::Value::as_str) == Some(stage))
                        })
                        || nodes
                            .iter()
                            .filter_map(|x| x.value.get("stages").and_then(toml::Value::as_array))
                            .flatten()
                            .any(|x| x.get("id").and_then(toml::Value::as_str) == Some(stage));
                    if !matches!(stage, "KEEP" | "FRESH") && !registered {
                        return Err(PackError::new(
                            "E_NODE_FIELD",
                            "redrive pre_stage must be KEEP, FRESH, or a registered stage id",
                        ));
                    }
                }
                if let Some(value) = t.get("bump") {
                    let b = value.as_str().ok_or_else(|| {
                        PackError::new("E_NODE_FIELD", "redrive bump must be a string")
                    })?;
                    if !counters.contains_key(b) {
                        return Err(PackError::new("E_COUNTER_REF", b));
                    }
                }
            }
            "emit" => {
                if t.contains_key("on_done") {
                    return Err(PackError::new("E_NODE_FIELD", "emit forbids on_done"));
                }
                if !["InspectionPassed", "InspectionFailed"]
                    .contains(&t.get("event").and_then(toml::Value::as_str).unwrap_or(""))
                {
                    return Err(PackError::new("E_EVENT", "unknown emit event"));
                }
                if t.get("payload_from")
                    .and_then(toml::Value::as_str)
                    .is_none()
                {
                    return Err(PackError::new(
                        "E_SCHEMA_MISSING_FIELD",
                        "emit payload_from must be a string",
                    ));
                }
            }
            "hold" => {
                if t.contains_key("on_done") {
                    return Err(PackError::new("E_NODE_FIELD", "hold forbids on_done"));
                }
                let targets = t
                    .get("resume_targets")
                    .and_then(toml::Value::as_array)
                    .ok_or_else(|| {
                        PackError::new(
                            "E_NODE_FIELD",
                            format!("{} resume_targets must be a string array", n.id),
                        )
                    })?;
                for r in targets {
                    let s = r.as_str().ok_or_else(|| {
                        PackError::new(
                            "E_NODE_FIELD",
                            format!("{} resume_targets entries must be strings", n.id),
                        )
                    })?;
                    add_edge(&mut edges, &n.id, s)
                }
            }
            _ => {
                return Err(PackError::new(
                    "E_NODE_KIND",
                    format!("unknown kind {}", n.kind),
                ));
            }
        }
    }
    if verify > 1 {
        return Err(PackError::new(
            "E_VERIFY_DUPLICATE",
            "more than one verify stage",
        ));
    }
    validate_stage_graph(nodes, p)?;
    for n in nodes {
        let t = n
            .value
            .as_table()
            .ok_or_else(|| PackError::new("E_NODE", "node must be a table"))?;
        for key in ["on_done", "hold_target"] {
            if let Some(s) = t.get(key).and_then(toml::Value::as_str)
                && !by.contains_key(s)
            {
                return Err(PackError::new("E_NODE_REF", format!("unknown node {s}")));
            }
        }
        if n.kind == "hold" {
            for r in t
                .get("resume_targets")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(s) = r.as_str()
                    && !by.contains_key(s)
                {
                    return Err(PackError::new(
                        "E_NODE_REF",
                        format!("unknown resume target {s}"),
                    ));
                }
            }
        }
        if n.kind == "route" {
            for b in t
                .get("branches")
                .and_then(toml::Value::as_array)
                .ok_or_else(|| PackError::new("E_BRANCHES", "route branches required"))?
            {
                if let Some(s) = b.get("goto").and_then(toml::Value::as_str)
                    && !by.contains_key(s)
                {
                    return Err(PackError::new("E_NODE_REF", format!("unknown goto {s}")));
                }
            }
        }
    }
    for n in nodes {
        if let Some(target) = n.value.get("hold_target") {
            let target = target.as_str().ok_or_else(|| {
                PackError::new(
                    "E_NODE_FIELD",
                    format!("{}.hold_target must be a string", n.id),
                )
            })?;
            let Some(index) = by.get(target) else {
                return Err(PackError::new(
                    "E_NODE_REF",
                    format!("unknown hold_target {target}"),
                ));
            };
            if nodes[*index].kind != "hold" {
                return Err(PackError::new(
                    "E_HOLD_TARGET",
                    format!("hold_target {target} must name a hold node"),
                ));
            }
        }
    }
    for n in nodes {
        if n.kind == "emit" {
            let s = n
                .value
                .get("payload_from")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| {
                    PackError::new(
                        "E_SCHEMA_MISSING_FIELD",
                        "emit payload_from must be a string",
                    )
                })?;
            if s != "summary" && !outputs.iter().any(|o| o.name == s) {
                return Err(PackError::new(
                    "E_SCHEMA_MISSING_FIELD",
                    format!("unknown output {s}"),
                ));
            }
        }
    }
    for output in outputs {
        if let Some(schema) = &output.schema
            && !files.contains_key(schema)
        {
            return Err(PackError::new(
                "E_SCHEMA_MISSING_FIELD",
                format!("output {} references missing schema {schema}", output.name),
            ));
        }
    }
    for file in files.keys().filter(|name| name.ends_with(".toml")) {
        let text = std::str::from_utf8(&files[file])
            .map_err(|e| PackError::new("E_TOML", e.to_string()))?;
        let schema: toml::Value =
            toml::from_str(text).map_err(|e| PackError::new("E_TOML", e.to_string()))?;
        if let Some(items) = schema.get("output").and_then(toml::Value::as_array) {
            for item in items {
                if let Some(condition) = item.get("when").and_then(toml::Value::as_str) {
                    let ast = parse_expr(condition)?;
                    validate_expr(&ast, counters, verify > 0)?;
                }
            }
        }
    }
    validate_error_policies(p)?;
    let mut advisory_emits = false;
    if kind == "advisory-pack" {
        for node in nodes {
            advisory_emits |= node.kind == "emit";
            if let Some(branches) = node.value.get("branches").and_then(toml::Value::as_array) {
                for branch in branches {
                    if let Some(action) = branch.get("do").and_then(toml::Value::as_str) {
                        advisory_emits |= parse_actions(action)?
                            .iter()
                            .any(|a| matches!(a, Action::Emit(..)));
                    }
                }
            }
        }
    }
    if advisory_emits {
        return Err(PackError::new(
            "E_ADVISORY_EMIT",
            "advisory packs cannot emit domain events",
        ));
    }
    let entry = p
        .get("entry")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| PackError::new("E_ENTRY", "entry required"))?;
    if !by.get(entry).is_some_and(|i| nodes[*i].kind == "dispatch") {
        return Err(PackError::new("E_ENTRY", "entry must name dispatch node"));
    }
    validate_provenance(entry, nodes)?;
    detect_cycles(nodes, &edges)?;
    validate_counter_cycles(nodes, &edges)?;
    validate_caps(nodes, counters)?;
    let _ = root;
    Ok(())
}
fn validate_error_policies(p: &toml::Value) -> Result<()> {
    let a = p
        .get("output")
        .and_then(|o| o.get("on_error"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| PackError::new("E_MISSING_ERROR_POLICY", "missing output.on_error"))?;
    let mut states = HashSet::new();
    for x in a {
        let s = x
            .get("state")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| PackError::new("E_ERROR_POLICY", "missing state"))?;
        let pol = x
            .get("policy")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| PackError::new("E_ERROR_POLICY", "missing policy"))?;
        states.insert(s);
        let good = match s {
            "ok" => pol == "accept-stage",
            "parse_error" => ["discard-finding", "fail-run"].contains(&pol),
            "dispatch_error" => ["discard-stage", "fail-run"].contains(&pol),
            "timeout" => ["hold", "fail-run"].contains(&pol),
            _ => false,
        };
        if !good {
            return Err(PackError::new(
                "E_ERROR_POLICY",
                format!("invalid policy {pol} for {s}"),
            ));
        }
    }
    for s in ["ok", "parse_error", "dispatch_error", "timeout"] {
        if !states.contains(s) {
            return Err(PackError::new("E_MISSING_ERROR_POLICY", s));
        }
    }
    Ok(())
}
fn add_edge(e: &mut HashMap<String, Vec<String>>, a: &str, b: &str) {
    e.entry(a.into()).or_default().push(b.into())
}
fn stage_dependencies(stage: &toml::Value) -> Vec<&str> {
    match stage.get("after") {
        Some(toml::Value::String(s)) => vec![s],
        Some(toml::Value::Array(a)) => a.iter().filter_map(toml::Value::as_str).collect(),
        _ => Vec::new(),
    }
}
fn stage_reaches(stages: &[toml::Value], from: &str, to: &str) -> bool {
    fn walk(stages: &[toml::Value], cur: &str, to: &str, seen: &mut HashSet<String>) -> bool {
        if cur == to {
            return true;
        }
        if !seen.insert(cur.to_owned()) {
            return false;
        }
        stages
            .iter()
            .find(|s| s.get("id").and_then(toml::Value::as_str) == Some(cur))
            .is_some_and(|s| {
                stage_dependencies(s)
                    .into_iter()
                    .any(|dep| walk(stages, dep, to, seen))
            })
    }
    walk(stages, from, to, &mut HashSet::new())
}
fn validate_stage_graph(nodes: &[Node], policy: &toml::Value) -> Result<()> {
    fn check(stages: &[toml::Value]) -> Result<()> {
        let ids: HashSet<_> = stages
            .iter()
            .filter_map(|s| s.get("id").and_then(toml::Value::as_str))
            .collect();
        for stage in stages {
            let id = stage.get("id").and_then(toml::Value::as_str).unwrap_or("");
            let deps = stage_dependencies(stage);
            if deps.iter().any(|dep| !ids.contains(dep)) {
                return Err(PackError::new(
                    "E_STAGE_REF",
                    format!("stage {id} has an unknown after dependency"),
                ));
            }
            if deps.contains(&id) {
                return Err(PackError::new(
                    "E_STAGE_REF",
                    format!("stage {id} cannot depend on itself"),
                ));
            }
            if deps.iter().any(|dep| stage_reaches(stages, dep, id)) {
                return Err(PackError::new(
                    "E_STAGE_CYCLE",
                    format!("stage dependency cycle at {id}"),
                ));
            }
        }
        Ok(())
    }
    for n in nodes {
        if let Some(stages) = n.value.get("stages").and_then(toml::Value::as_array) {
            check(stages)?;
        }
    }
    if let Some(stages) = policy.get("pack_stages").and_then(toml::Value::as_array) {
        check(stages)?;
    }
    Ok(())
}
fn expr_uses_verdicts(expr: &Expr) -> bool {
    match expr {
        Expr::Call(name, args) if name == "any_in" => {
            args.first() == Some(&Expr::Ident("verdicts".into()))
        }
        Expr::Call(name, _) if name == "has_findings" => true,
        Expr::And(a, b) | Expr::Or(a, b) => expr_uses_verdicts(a) || expr_uses_verdicts(b),
        Expr::Not(x) => expr_uses_verdicts(x),
        _ => false,
    }
}
fn validate_provenance(entry: &str, nodes: &[Node]) -> Result<()> {
    let mut queue = vec![(entry.to_owned(), false)];
    let mut seen = HashSet::new();
    while let Some((id, verified)) = queue.pop() {
        if !seen.insert((id.clone(), verified)) {
            continue;
        }
        let Some(node) = nodes.iter().find(|n| n.id == id) else {
            continue;
        };
        match node.kind.as_str() {
            "route" => {
                if let Some(branches) = node.value.get("branches").and_then(toml::Value::as_array) {
                    for branch in branches {
                        if let Some(when) = branch.get("when").and_then(toml::Value::as_str)
                            && !verified
                            && expr_uses_verdicts(&parse_expr(when)?)
                        {
                            return Err(PackError::new(
                                "E_PROVENANCE",
                                format!(
                                    "route {} reads verdicts without a post-verify path",
                                    node.id
                                ),
                            ));
                        }
                        if let Some(target) = branch.get("goto").and_then(toml::Value::as_str) {
                            let reverted = branch
                                .get("do")
                                .and_then(toml::Value::as_str)
                                .map(parse_actions)
                                .transpose()?
                                .is_some_and(|a| a.iter().any(|x| matches!(x, Action::Revert(_))));
                            queue.push((target.to_owned(), verified && !reverted));
                        }
                    }
                }
            }
            "dispatch" => {
                let prior_verified = verified;
                let verified = verified
                    || node
                        .value
                        .get("stages")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|ss| {
                            ss.iter().any(|s| {
                                s.get("role").and_then(toml::Value::as_str) == Some("verify")
                            })
                        });
                if let Some(next) = node.value.get("on_done").and_then(toml::Value::as_str) {
                    queue.push((next.into(), verified));
                }
                if let Some(next) = node.value.get("hold_target").and_then(toml::Value::as_str) {
                    queue.push((next.into(), prior_verified));
                }
            }
            "redrive" => {
                for key in ["on_done", "hold_target"] {
                    if let Some(next) = node.value.get(key).and_then(toml::Value::as_str) {
                        queue.push((next.into(), false));
                    }
                }
            }
            "hold" => {
                if let Some(ts) = node
                    .value
                    .get("resume_targets")
                    .and_then(toml::Value::as_array)
                {
                    for target in ts.iter().filter_map(toml::Value::as_str) {
                        queue.push((target.into(), verified));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn node_str<'a>(
    t: &'a toml::map::Map<String, toml::Value>,
    node: &str,
    key: &str,
) -> Result<&'a str> {
    t.get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| PackError::new("E_NODE_FIELD", format!("{node}.{key} must be a string")))
}
fn detect_cycles(nodes: &[Node], edges: &HashMap<String, Vec<String>>) -> Result<()> {
    fn find_cycle(
        x: &str,
        e: &HashMap<String, Vec<String>>,
        path: &mut Vec<String>,
        done: &mut HashSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(index) = path.iter().position(|n| n == x) {
            return Some(path[index..].to_vec());
        }
        if done.contains(x) {
            return None;
        }
        path.push(x.to_owned());
        for y in e.get(x).into_iter().flatten() {
            if let Some(cycle) = find_cycle(y, e, path, done) {
                return Some(cycle);
            }
        }
        path.pop();
        done.insert(x.to_owned());
        None
    }
    // (F16-2, §2.3) cross-node revert→emit: after a revert action the
    // goto chain must not reach any emit. Route nodes only — hold
    // (human resume) and dispatch/redrive re-execution imply
    // re-verification, so a revert path through them is legal.
    fn reaches_emit(nodes: &[Node], start: &str, seen: &mut HashSet<String>) -> bool {
        if !seen.insert(start.to_owned()) {
            return false;
        }
        let Some(nd) = nodes.iter().find(|x| x.id == start) else {
            return false;
        };
        if nd.kind == "emit" {
            return true;
        }
        if nd.kind == "route"
            && let Some(bs) = nd.value.get("branches").and_then(toml::Value::as_array)
        {
            for b in bs {
                if let Some(g) = b.get("goto").and_then(toml::Value::as_str)
                    && reaches_emit(nodes, g, seen)
                {
                    return true;
                }
                let has_emit = b
                    .get("do")
                    .and_then(toml::Value::as_str)
                    .and_then(|s| parse_actions(s).ok())
                    .is_some_and(|actions| actions.iter().any(|a| matches!(a, Action::Emit(..))));
                if has_emit && b.get("goto").is_none() {
                    return true;
                }
            }
        }
        false
    }
    for nd in nodes {
        if nd.kind == "route"
            && let Some(bs) = nd.value.get("branches").and_then(toml::Value::as_array)
        {
            for b in bs {
                let has_revert = b
                    .get("do")
                    .and_then(toml::Value::as_str)
                    .and_then(|s| parse_actions(s).ok())
                    .is_some_and(|actions| actions.iter().any(|a| matches!(a, Action::Revert(_))));
                if has_revert && let Some(g) = b.get("goto").and_then(toml::Value::as_str) {
                    let mut seen = HashSet::new();
                    if reaches_emit(nodes, g, &mut seen) {
                        return Err(PackError::new(
                            "E_REVERT_THEN_EMIT",
                            format!("route {} revert path reaches emit", nd.id),
                        ));
                    }
                }
            }
        }
    }
    let allowed: HashSet<&str> = nodes
        .iter()
        .filter(|n| n.kind == "route" || n.kind == "hold")
        .map(|n| n.id.as_str())
        .collect();
    let filtered: HashMap<String, Vec<String>> = edges
        .iter()
        .filter(|(from, _)| allowed.contains(from.as_str()))
        .map(|(from, tos)| {
            (
                from.clone(),
                tos.iter()
                    .filter(|to| allowed.contains(to.as_str()))
                    .cloned()
                    .collect(),
            )
        })
        .collect();
    let mut d = HashSet::new();
    for n in nodes {
        if allowed.contains(n.id.as_str())
            && let Some(cycle) = find_cycle(&n.id, &filtered, &mut Vec::new(), &mut d)
        {
            let resume = cycle.iter().any(|id| {
                nodes
                    .iter()
                    .any(|node| node.id == *id && node.kind == "hold")
            });
            return Err(PackError::new(
                if resume {
                    "E_DAG_RESUME_CYCLE"
                } else {
                    "E_DAG_CYCLE"
                },
                "transition cycle",
            ));
        }
    }
    Ok(())
}

fn validate_counter_cycles(nodes: &[Node], edges: &HashMap<String, Vec<String>>) -> Result<()> {
    for route in nodes.iter().filter(|n| n.kind == "route") {
        let branches = route
            .value
            .get("branches")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| PackError::new("E_BRANCHES", "route branches required"))?;
        for branch in branches {
            let condition = parse_expr(
                branch
                    .get("when")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| PackError::new("E_DSL_SYNTAX", "when required"))?,
            )?;
            let mut guards = Vec::new();
            count_refs(&condition, false, &mut guards);
            let Some(target) = branch.get("goto").and_then(toml::Value::as_str) else {
                continue;
            };
            if guards.is_empty() {
                continue;
            }
            let paths = paths_back_to(target, &route.id, edges);
            if paths.is_empty() {
                continue;
            }
            let guard_names: BTreeSet<_> = guards.iter().map(|(name, _)| name.clone()).collect();
            let mut bumps = Vec::new();
            if let Some(actions) = branch.get("do").and_then(toml::Value::as_str) {
                for action in parse_actions(actions)? {
                    if let Action::Bump(name) = action {
                        bumps.push(name);
                    }
                }
            }
            for path in paths {
                let mut path_bumps = bumps.clone();
                for id in path {
                    if let Some(node) = nodes.iter().find(|n| n.id == id)
                        && let Some(value) = node.value.get("bump")
                    {
                        let name = value.as_str().ok_or_else(|| {
                            PackError::new(
                                "E_NODE_FIELD",
                                format!("{}.bump must be a string", node.id),
                            )
                        })?;
                        path_bumps.push(name.to_owned());
                    }
                }
                if path_bumps.is_empty() {
                    return Err(PackError::new(
                        "E_COUNTER_GUARD_MISSING",
                        "a loopback path has no counter increment",
                    ));
                }
                for guard in &guard_names {
                    let owned = path_bumps.iter().filter(|b| *b == guard).count();
                    if owned == 0
                        && !path_bumps.is_empty()
                        && path_bumps.iter().all(|b| !guard_names.contains(b))
                    {
                        return Err(PackError::new(
                            "E_COUNTER_GUARD_MISMATCH",
                            "loopback path increments only counters unrelated to its guard",
                        ));
                    }
                    if owned == 0 {
                        return Err(PackError::new(
                            "E_COUNTER_GUARD_MISSING",
                            format!("loopback path does not increment guarded counter {guard}"),
                        ));
                    }
                    if owned > 1 {
                        return Err(PackError::new(
                            "E_COUNTER_GUARD_DOUBLE",
                            format!("loopback path increments {guard} more than once"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn paths_back_to(
    start: &str,
    target: &str,
    edges: &HashMap<String, Vec<String>>,
) -> Vec<Vec<String>> {
    fn walk(
        cur: &str,
        target: &str,
        edges: &HashMap<String, Vec<String>>,
        seen: &mut HashSet<String>,
        path: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        if !seen.insert(cur.to_owned()) {
            return;
        }
        path.push(cur.to_owned());
        if cur == target {
            out.push(path.clone());
        } else {
            for next in edges.get(cur).into_iter().flatten() {
                walk(next, target, edges, seen, path, out);
            }
        }
        path.pop();
        seen.remove(cur);
    }
    let mut out = Vec::new();
    walk(
        start,
        target,
        edges,
        &mut HashSet::new(),
        &mut Vec::new(),
        &mut out,
    );
    out
}

fn validate_caps(nodes: &[Node], counters: &BTreeMap<String, u64>) -> Result<()> {
    for n in nodes.iter().filter(|n| n.kind == "route") {
        for b in n
            .value
            .get("branches")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| PackError::new("E_BRANCHES", "route branches required"))?
        {
            let ast = parse_expr(
                b.get("when")
                    .and_then(toml::Value::as_str)
                    .ok_or_else(|| PackError::new("E_DSL_SYNTAX", "when required"))?,
            )?;
            let mut refs = Vec::new();
            count_refs(&ast, false, &mut refs);
            for (counter, cap) in refs {
                if !counters.contains_key(&counter) {
                    continue;
                }
                let literals = string_literals(&ast);
                let words: Vec<_> = literals.into_iter().collect();
                let subsets = 1usize.checked_shl(words.len().min(10) as u32).unwrap_or(1);
                for mask in 0..subsets {
                    let verdicts: BTreeSet<String> = words
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| mask & (1 << i) != 0)
                        .map(|(_, s)| s.clone())
                        .collect();
                    for findings in [false, true] {
                        for timeout in [false, true] {
                            for budget in [false, true] {
                                for val in [cap, cap.saturating_add(1)] {
                                    let env = Env {
                                        counter: &counter,
                                        value: val,
                                        verdicts: &verdicts,
                                        findings,
                                        timeout,
                                        budget,
                                    };
                                    if eval(&ast, &env) {
                                        return Err(PackError::new(
                                            "E_COUNTER_CAP_NOT_ENFORCED",
                                            format!("guard for {counter} can be true at cap {cap}"),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
fn count_refs(e: &Expr, neg: bool, out: &mut Vec<(String, u64)>) {
    match e {
        Expr::Not(x) => count_refs(x, !neg, out),
        Expr::Call(n, a) if n == "count_lt" && a.len() == 2 => {
            if !neg && let (Ok(c), Expr::Integer(k)) = (arg_id(&a[0]), &a[1]) {
                out.push((c, *k))
            }
        }
        Expr::And(a, b) | Expr::Or(a, b) => {
            count_refs(a, neg, out);
            count_refs(b, neg, out)
        }
        _ => {}
    }
}
fn string_literals(e: &Expr) -> BTreeSet<String> {
    let mut s = BTreeSet::new();
    fn w(e: &Expr, s: &mut BTreeSet<String>) {
        match e {
            Expr::String(x) => {
                s.insert(x.clone());
            }
            Expr::List(v) => {
                for x in v {
                    w(x, s)
                }
            }
            Expr::Call(_, v) => {
                for x in v {
                    w(x, s)
                }
            }
            Expr::And(a, b) | Expr::Or(a, b) => {
                w(a, s);
                w(b, s)
            }
            Expr::Not(x) => w(x, s),
            _ => {}
        }
    }
    w(e, &mut s);
    s
}
struct Env<'a> {
    counter: &'a str,
    value: u64,
    verdicts: &'a BTreeSet<String>,
    findings: bool,
    timeout: bool,
    budget: bool,
}
// Both static cap validation and runtime routing share boolean evaluation and
// count_lt comparison. Their environments differ deliberately: cap validation
// supplies one hypothetical counter; runtime routing supplies actual counters.
fn eval_boolean(
    expr: &Expr,
    predicate: &impl Fn(&str, &[Expr]) -> Option<bool>,
) -> Option<bool> {
    match expr {
        Expr::Bool(value) => Some(*value),
        Expr::Not(inner) => eval_boolean(inner, predicate).map(|value| !value),
        Expr::And(left, right) => {
            if !eval_boolean(left, predicate)? {
                Some(false)
            } else {
                eval_boolean(right, predicate)
            }
        }
        Expr::Or(left, right) => {
            if eval_boolean(left, predicate)? {
                Some(true)
            } else {
                eval_boolean(right, predicate)
            }
        }
        Expr::Call(name, args) => predicate(name, args),
        _ => None,
    }
}

fn eval_count_lt(args: &[Expr], lookup: impl Fn(&str) -> Option<u64>) -> Option<bool> {
    let [Expr::Ident(counter), Expr::Integer(bound)] = args else {
        return None;
    };
    lookup(counter).map(|value| value < *bound)
}

fn eval(e: &Expr, x: &Env<'_>) -> bool {
    eval_boolean(e, &|name, args| {
        Some(match name {
            "count_lt" => eval_count_lt(args, |counter| {
                (counter == x.counter).then_some(x.value)
            }).unwrap_or(false),
            "any_in" => {
                if let [Expr::Ident(_), Expr::List(values)] = args {
                    values.iter().any(|value| {
                        matches!(value, Expr::String(s) if x.verdicts.contains(s))
                    })
                } else {
                    false
                }
            }
            "has_findings" => x.findings,
            "timeout" => x.timeout,
            "budget_exhausted" => x.budget,
            _ => false,
        })
    }).unwrap_or(false)
}

/// Evaluate a counter-only condition. Missing runtime evidence is unknown, even
/// under negation; callers that select ordered branches must stop on `None`
/// rather than skipping to a fallback that assumes the evidence was negative.
pub fn eval_condition_checked(expr: &Expr, counters: &BTreeMap<String, u64>) -> Option<bool> {
    eval_boolean(expr, &|name, args| {
        if name == "count_lt" {
            eval_count_lt(args, |counter| counters.get(counter).copied())
        } else {
            None
        }
    })
}

/// Counter-only convenience evaluation. Unknown context fails closed.
pub fn eval_condition(expr: &Expr, counters: &BTreeMap<String, u64>) -> bool {
    eval_condition_checked(expr, counters).unwrap_or(false)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn runtime_conditions_preserve_unknown_evidence_under_negation() {
        let counters = BTreeMap::from([("c".into(), 1)]);
        for condition in [
            "timeout()", "!timeout()", "!has_findings(any)",
            "!any_in(verdicts, [\"patch\"])", "budget_exhausted() || true",
            "!count_lt(missing, 2)", "count_lt()", "any_in()",
            "count_lt(c, 2, 3)",
        ] {
            let expr = parse_expr(condition).unwrap();
            assert_eq!(eval_condition_checked(&expr, &counters), None, "{condition}");
            assert!(!eval_condition(&expr, &counters), "{condition}");
        }
        assert_eq!(eval_condition_checked(&parse_expr("false && timeout()").unwrap(), &counters), Some(false));
        assert_eq!(eval_condition_checked(&parse_expr("true || timeout()").unwrap(), &counters), Some(true));
    }

    #[test]
    fn runtime_and_static_counter_evaluation_share_boundary_semantics() {
        let verdicts = BTreeSet::new();
        for value in [0, 1, 2, u64::MAX] {
            for bound in [0, 1, 2, u64::MAX] {
                let env = Env { counter: "c", value, verdicts: &verdicts,
                    findings: false, timeout: false, budget: false };
                let counters = BTreeMap::from([("c".into(), value)]);
                for condition in [
                    format!("count_lt(c, {bound})"),
                    format!("!count_lt(c, {bound})"),
                    format!("false || (count_lt(c, {bound}) && true)"),
                ] {
                    let expr = parse_expr(&condition).unwrap();
                    assert_eq!(eval_condition_checked(&expr, &counters), Some(eval(&expr, &env)));
                }
            }
        }
        // Static validation probes a hypothetical counter. Other references
        // intentionally evaluate false there; runtime requires an actual value.
        let expr = parse_expr("count_lt(other, 2)").unwrap();
        let env = Env { counter: "c", value: 0, verdicts: &verdicts,
            findings: false, timeout: false, budget: false };
        assert!(!eval(&expr, &env));
        assert_eq!(eval_condition_checked(&expr, &BTreeMap::new()), None);
        assert!(!eval_condition(&expr, &BTreeMap::new()));
    }

    fn cap_check(condition: &str, cap: u64) -> Result<()> {
        let node_value: toml::Value = toml::from_str(&format!(
            r#"
                id = "route"
                kind = "route"
                [[branches]]
                when = {condition:?}
                goto = "again"
            "#
        ))
        .unwrap();
        let nodes = vec![Node {
            id: "route".into(),
            kind: "route".into(),
            value: node_value,
        }];
        validate_caps(
            &nodes,
            &BTreeMap::from([("c".into(), 0), ("limit".into(), cap)]),
        )
    }

    #[test]
    fn semantic_cap_check_handles_boolean_equivalence_and_empty_literals() {
        assert!(
            cap_check(
                r#"any_in(verdicts, ["x"]) && !(!count_lt(c, 3) && false)"#,
                3
            )
            .is_err()
        );
        assert!(cap_check(r#"count_lt(c, 7) || any_in(verdicts, [""])"#, 7).is_err());
        assert!(cap_check(r#"!!count_lt(c, 7)"#, 7).is_ok());
        assert!(cap_check(r#"!(!count_lt(c, 7))"#, 7).is_ok());
    }

    #[test]
    fn cap_logic_accepts_u64_boundary_counters() {
        assert!(cap_check(r#"count_lt(c, 18446744073709551615)"#, u64::MAX).is_ok());
    }

    #[test]
    fn toml_counter_boundary_is_nonnegative_i64() {
        let max: toml::Value = toml::from_str("[counters]\nc = 9223372036854775807\n").unwrap();
        assert_eq!(parse_counters(&max).unwrap()["c"], i64::MAX as u64);
        let negative: toml::Value = toml::from_str("[counters]\nc = -1\n").unwrap();
        assert_eq!(
            parse_counters(&negative).unwrap_err().code,
            "E_COUNTER_TYPE"
        );
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Bool(bool),
    Ident(String),
    String(String),
    Integer(u64),
    List(Vec<Expr>),
    Call(String, Vec<Expr>),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}
/// Parse one complete DSL expression. String contents are opaque literal AST nodes.
pub fn parse_expr(s: &str) -> Result<Expr> {
    let mut p = Parser {
        b: s.as_bytes(),
        i: 0,
    };
    let e = p.or()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(PackError::new("E_DSL_SYNTAX", "trailing input"));
    }
    Ok(e)
}
struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}
impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1
        }
    }
    fn eat(&mut self, s: &[u8]) -> bool {
        self.ws();
        if self.b.get(self.i..self.i + s.len()) == Some(s) {
            self.i += s.len();
            true
        } else {
            false
        }
    }
    fn or(&mut self) -> Result<Expr> {
        let mut x = self.and()?;
        while self.eat(b"||") {
            x = Expr::Or(Box::new(x), Box::new(self.and()?));
        }
        Ok(x)
    }
    fn and(&mut self) -> Result<Expr> {
        let mut x = self.un()?;
        while self.eat(b"&&") {
            x = Expr::And(Box::new(x), Box::new(self.un()?));
        }
        Ok(x)
    }
    fn un(&mut self) -> Result<Expr> {
        if self.eat(b"!") {
            return Ok(Expr::Not(Box::new(self.un()?)));
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<Expr> {
        self.ws();
        if self.eat(b"(") {
            let e = self.or()?;
            if !self.eat(b")") {
                return Err(PackError::new("E_DSL_SYNTAX", "expected )"));
            }
            return Ok(e);
        }
        if self.b.get(self.i) == Some(&b'\'') {
            return Err(PackError::new(
                "E_DSL_SYNTAX",
                "single quoted strings are forbidden",
            ));
        }
        if self.b.get(self.i) == Some(&b'"') {
            self.i += 1;
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i] != b'"' {
                if self.b[self.i] == b'\\' {
                    self.i += 1;
                }
                self.i += 1;
            }
            if self.i >= self.b.len() {
                return Err(PackError::new("E_DSL_SYNTAX", "unterminated string"));
            }
            let val = String::from_utf8_lossy(&self.b[start..self.i]).to_string();
            self.i += 1;
            return Ok(Expr::String(val));
        }
        if self.b.get(self.i) == Some(&b'[') {
            self.i += 1;
            let mut v = Vec::new();
            if self.eat(b"]") {
                return Ok(Expr::List(v));
            }
            loop {
                v.push(self.primary()?);
                if self.eat(b"]") {
                    break;
                }
                if !self.eat(b",") {
                    return Err(PackError::new("E_DSL_SYNTAX", "expected , or ]"));
                }
            }
            return Ok(Expr::List(v));
        }
        let start = self.i;
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_alphanumeric() || self.b[self.i] == b'_')
        {
            self.i += 1;
        }
        if self.i == start {
            return Err(PackError::new("E_DSL_SYNTAX", "expected expression"));
        }
        let token = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        if token == "true" {
            return Ok(Expr::Bool(true));
        }
        if token == "false" {
            return Ok(Expr::Bool(false));
        }
        if let Ok(n) = token.parse::<u64>() {
            return Ok(Expr::Integer(n));
        }
        if self.eat(b"(") {
            let mut args = Vec::new();
            if self.eat(b")") {
                return Ok(Expr::Call(token.into(), args));
            }
            loop {
                args.push(self.or()?);
                if self.eat(b")") {
                    break;
                }
                if !self.eat(b",") {
                    return Err(PackError::new("E_DSL_SYNTAX", "expected , or )"));
                }
            }
            return Ok(Expr::Call(token.into(), args));
        }
        Ok(Expr::Ident(token.into()))
    }
}

enum Action {
    Emit(String, String),
    Bump(String),
    Revert(String),
}
fn parse_actions(s: &str) -> Result<Vec<Action>> {
    let mut out = Vec::new();
    for raw in s.split(';') {
        let e = parse_expr(raw.trim())?;
        match e {
            Expr::Call(n, a) if n == "bump" && a.len() == 1 => {
                out.push(Action::Bump(arg_id(&a[0])?))
            }
            Expr::Call(n, a) if n == "revert" && a.len() == 1 => {
                out.push(Action::Revert(arg_id(&a[0])?))
            }
            Expr::Call(n, a) if n == "emit" && a.len() == 2 => {
                out.push(Action::Emit(arg_id(&a[0])?, arg_id(&a[1])?))
            }
            _ => return Err(PackError::new("E_ACTION", "invalid action")),
        }
    }
    Ok(out)
}
fn arg_id(e: &Expr) -> Result<String> {
    match e {
        Expr::Ident(s) => Ok(s.clone()),
        _ => Err(PackError::new("E_DSL_SYNTAX", "identifier expected")),
    }
}
fn validate_expr(e: &Expr, c: &BTreeMap<String, u64>, has_verify: bool) -> Result<()> {
    match e {
        Expr::And(a, b) | Expr::Or(a, b) => {
            validate_expr(a, c, has_verify)?;
            validate_expr(b, c, has_verify)
        }
        Expr::Not(x) => validate_expr(x, c, has_verify),
        Expr::Call(n, args) => {
            match n.as_str() {
                "any_in" if args.len() == 2 => {
                    if args[0] != Expr::Ident("verdicts".into()) || !has_verify {
                        return Err(PackError::new(
                            "E_PROVENANCE",
                            "verdicts requires verify role",
                        ));
                    }
                    if !matches!(args[1], Expr::List(_)) {
                        return Err(PackError::new("E_DSL_TYPE", "any_in requires literal list"));
                    }
                    if let Expr::List(items) = &args[1]
                        && items.iter().any(|item| !matches!(item, Expr::String(_)))
                    {
                        return Err(PackError::new(
                            "E_DSL_SYNTAX",
                            "any_in list elements must be string literals",
                        ));
                    }
                }
                "count_lt" if args.len() == 2 => {
                    let name = arg_id(&args[0])?;
                    if !c.contains_key(&name) {
                        return Err(PackError::new("E_COUNTER_REF", name));
                    }
                    if !matches!(args[1], Expr::Integer(_)) {
                        return Err(PackError::new("E_DSL_TYPE", "count_lt requires u64"));
                    }
                }
                "has_findings" if args == &[Expr::Ident("any".into())] => {
                    if !has_verify {
                        return Err(PackError::new(
                            "E_PROVENANCE",
                            "findings requires verify role",
                        ));
                    }
                }
                "timeout" | "budget_exhausted" if args.is_empty() => {}
                _ => return Err(PackError::new("E_DSL_CALL", format!("invalid call {n}"))),
            }
            Ok(())
        }
        Expr::Bool(_) => Ok(()),
        _ => Err(PackError::new("E_DSL_TYPE", "expression must be boolean")),
    }
}
