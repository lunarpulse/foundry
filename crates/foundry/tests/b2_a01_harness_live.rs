#![cfg(all(feature = "a01-harness-support", unix))]
//! Local fixtures exercise boundaries; only ignored gates claim live admission/Die evidence.
//! P04 registry order-change is NOT-ADMITTED. G40-G42 do not claim an OS restart or registry reorder.
#[allow(dead_code, unused_imports)]
#[path = "../src/main.rs"]
mod application;
use application::{
    a01_coordinator::{self, Producer},
    a01_harness_live::Creator,
    a01_real_git_producer::{RealGitProducer, git, git_text},
};
use chrono::{Duration, Utc};
use foundry_ledger::{product::*, *};
use foundry_line::{DieStageExecutor, StageExecutor, StageOutput, StageTask};
use rusqlite::{Connection, types::Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

#[track_caller]
fn must<T, E: std::fmt::Debug>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("A01 operation rejected: {e:?}"),
    }
}
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
struct Dir(PathBuf, bool);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "a01-live-{}-{}",
            std::process::id(),
            must(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)).as_nanos()
        ));
        must(fs::DirBuilder::new().mode(0o700).create(&p));
        Self(must(p.canonicalize()), false)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        if !self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn db(path: &Path) -> Connection {
    must(Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ))
}
type Snapshot = BTreeMap<String, Vec<Vec<Value>>>;
fn snapshot(path: &Path) -> Snapshot {
    let c = db(path);
    let mut names=must(c.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"));
    let names: Vec<String> = must(must(names.query_map([], |r| r.get(0))).collect());
    names
        .into_iter()
        .map(|name| {
            let mut s =
                must(c.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""))));
            let n = s.column_count();
            let mut rows: Vec<Vec<Value>> =
                must(must(s.query_map([], |r| (0..n).map(|i| r.get(i)).collect())).collect());
            rows.sort_by_key(|r| format!("{r:?}"));
            (name, rows)
        })
        .collect()
}
fn unchanged(a: &Snapshot, b: &Snapshot) {
    assert!(a == b, "A01 durable snapshot changed");
}
fn count(path: &Path, table: &str) -> i64 {
    must(db(path).query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)))
}
fn bridge_count(path: &Path) -> i64 {
    must(db(path).query_row("SELECT count(*) FROM product_transition_receipts_v1 WHERE operation='a01-passed-bridge-v1'",[],|r|r.get(0)))
}
fn no_host(path: &Path) {
    for t in [
        "product_review_runs_v1",
        "product_review_stages_v1",
        "artifact_tuples",
        "product_result_bindings_v1",
    ] {
        assert!(count(path, t) == 0, "A01 premature host/result evidence");
    }
}
fn pack() -> PathBuf {
    must(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/bmad-review-full/v1")
            .canonicalize(),
    )
}

struct Fixture {
    creator: Creator,
    dir: Dir,
    repo: PathBuf,
    base: String,
    binary: PathBuf,
    receipts: Vec<AdmissionReceipt>,
    issue: String,
}
impl Fixture {
    fn local() -> Self {
        Self::local_format("sha1")
    }
    fn local_format(format: &str) -> Self {
        let dir = Dir::new();
        let creator = must(Creator::create(
            dir.0.join("owned"),
            "acme",
            "project",
            "42",
        ));
        let repo = dir.0.join("source");
        must(fs::create_dir(&repo));
        must(git(
            &repo,
            &["init", "-q", &format!("--object-format={format}")],
        ));
        must(fs::write(repo.join("README"), b"fixture base\n"));
        must(git(&repo, &["add", "README"]));
        must(git(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        ));
        let base = must(git_text(&repo, &["rev-parse", "HEAD"]));
        let binary = dir.0.join("fixture-worker");
        // A deterministic external process for local boundary tests, never G30's positive proof.
        must(fs::write(&binary, LOCAL_WORKER));
        must(fs::set_permissions(
            &binary,
            fs::Permissions::from_mode(0o700),
        ));
        let mut f = Self {
            creator,
            dir,
            repo,
            base,
            binary,
            receipts: vec![],
            issue: "17".into(),
        };
        f.local_admit("17", "selected");
        f
    }
    fn local_admit(&mut self, issue: &str, order: &str) {
        let c = &mut self.creator;
        let at = c.clock.now();
        let payload=serde_json::json!({"issue_number":7,"title":"source snapshot","body":"untrusted source text"}).to_string();
        let updated = at.to_rfc3339();
        let version = delivery_version_digest(issue, &updated, "github-issue", &payload);
        let delivery = ProductDelivery {
            delivery_id: github_delivery_id(
                &c.config.installation_id,
                "github-primary",
                "https://api.github.com",
                "42",
                issue,
                &version,
            ),
            connector_id: "github-primary".into(),
            repository_id: "42".into(),
            issue_id: issue.into(),
            observed_updated_at: updated.clone(),
            version_digest: version,
            task_kind: "github-issue".into(),
            payload,
        };
        let cursor = ProductPollCursor {
            connector_id: "github-primary".into(),
            query_fingerprint: "local".into(),
            sweep_id: format!("local-{issue}"),
            next_page: 1,
            page_ceiling: 1,
            updated_at: updated,
        };
        must(c.store.persist_intake_page_for_service(
            &c.service,
            &cursor,
            &[delivery.clone()],
            &[],
            at,
        ));
        let receipt = must(c.store.admit_intake_delivery_for_service(
            &c.service,
            &delivery.delivery_id,
            order,
            at,
        ));
        must(c.store.mark_intake_acked_for_service(
            &c.service,
            &delivery.delivery_id,
            &receipt,
            at,
        ));
        self.receipts.push(receipt);
    }
    fn live() -> Self {
        fn env(k: &str) -> String {
            must(std::env::var(k))
        }
        let mut dir = Dir::new();
        dir.1 = true;
        let owner = env("A01_LIVE_OWNER");
        let repo_name = env("A01_LIVE_REPO");
        let repository_id = env("A01_LIVE_REPOSITORY_ID");
        let issue = env("A01_LIVE_ISSUE_ID");
        let creator = must(Creator::create(
            dir.0.join("owned"),
            &owner,
            &repo_name,
            &repository_id,
        ));
        let repo = must(PathBuf::from(env("A01_LIVE_REPOSITORY")).canonicalize());
        let base = env("A01_LIVE_BASE");
        let binary = must(PathBuf::from(env("A01_LIVE_DIE")).canonicalize());
        let real_binary = binary;
        let binary = dir.0.join("live-worker-observer");
        let target = must(serde_json::to_string(&real_binary));
        let log = must(serde_json::to_string(&dir.0.join("external-invocations")));
        let wrapper = format!(
            "#!/usr/bin/python3\nimport os,sys,json,pathlib\nm=json.loads(pathlib.Path('input-manifest.json').read_bytes())\nwith open({log},'a') as f: f.write(m['task']+'\\n')\nos.execv({target},[{target}]+sys.argv[1:])\n"
        );
        must(fs::write(&binary, wrapper));
        must(fs::set_permissions(
            &binary,
            fs::Permissions::from_mode(0o700),
        ));
        let cfg = foundry_watch::intake::ProductGithubConfig {
            connector_id: "github-primary".into(),
            api_origin: "https://api.github.com".into(),
            owner,
            repo: repo_name,
            repository_id,
            private: std::env::var("A01_LIVE_PRIVATE").as_deref() == Ok("1"),
            credential_ref: std::env::var("A01_LIVE_CREDENTIAL_REF").ok(),
            allow_loopback_http: false,
            max_pages_per_tick: 5,
            now: Utc::now(),
        };
        let before = snapshot(creator.store.path());
        let (poll, drain) = must(foundry_watch::intake_worker::product_github_tick(
            &mut must(creator.checked()),
            &cfg,
            &LiveSecrets,
            Utc::now(),
            1000,
            &creator.service,
        ));
        assert!(
            poll.pages > 0
                && drain.admitted > 0
                && drain.admitted == drain.acknowledged
                && !matches!(drain.work_claim, ProductWorkClaimResult::Claimed(_)),
            "A01 fresh live intake invalid"
        );
        for t in [
            "product_a01_task_revisions_v1",
            "product_a01_claim_snapshots_v1",
            "product_work_claims_v1",
            "product_review_runs_v1",
        ] {
            assert!(
                count(creator.store.path(), t) == 0 && before[t].is_empty(),
                "A01 fresh tick executed"
            );
        }
        Self {
            creator,
            dir,
            repo,
            base,
            binary,
            receipts: drain.receipts,
            issue,
        }
    }
    fn request(&mut self) -> A01AuthorizeTaskRequest {
        must(self.creator.request(&self.receipts, &self.issue))
    }
    fn claim(&mut self) -> (A01TaskReceipt, ProductWorkClaim) {
        let r = self.request();
        must(self.creator.authorize_claim(&r))
    }
    fn producer(&self) -> RealGitProducer {
        must(RealGitProducer::new(
            self.binary.clone(),
            300,
            self.repo.clone(),
            self.creator.root.clone(),
            &self.base,
        ))
    }
    fn reserve(&mut self) -> (ProductWorkClaim, A01ProducerReservation) {
        let (task, claim) = self.claim();
        let r = must(self.creator.store.reserve_a01_producer(
            &self.creator.service,
            &claim,
            task.revision,
            "producer",
        ));
        (claim, r)
    }
    fn accepted(&mut self) -> (ProductWorkClaim, A01AcceptedProducerOutput, RealGitProducer) {
        let (claim, r) = self.reserve();
        let mut producer = self.producer();
        let output = must(producer.produce(&r.task.task_bytes, &claim, &r));
        let a = must(self.creator.store.accept_a01_producer_output(
            &self.creator.service,
            &claim,
            &r,
            &output,
        ));
        (claim, a, producer)
    }
    fn executor(&self) -> TraceDie {
        let mut e = TraceDie::new(DieStageExecutor::new(
            self.binary.clone(),
            "coding".into(),
            300,
            self.repo.clone(),
            self.creator.root.join("sessions"),
        ));
        e.path = Some(self.creator.store.path().into());
        e
    }
    fn dispatch(
        &mut self,
    ) -> (
        a01_coordinator::AcceptedDispatch,
        RealGitProducer,
        Arc<Mutex<Vec<Trace>>>,
    ) {
        let (task, claim) = self.claim();
        let executor = self.executor();
        let trace = executor.trace.clone();
        let mut producer = self.producer();
        let mut host = must(self.creator.host(&pack(), executor, &self.base));
        let d = must(a01_coordinator::dispatch(
            &mut self.creator.store,
            &self.creator.service,
            &claim,
            task.revision,
            "producer",
            &mut producer,
            &mut host,
        ));
        (d, producer, trace)
    }
}
struct LiveSecrets;
impl foundry_domain::ports::SecretProvider for LiveSecrets {
    fn resolve(&self, name: &str) -> foundry_domain::ports::PortResult<String> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(foundry_domain::ports::PortError::Permanent(
                "reference rejected".into(),
            ));
        }
        std::env::var(name).map_err(|_| {
            foundry_domain::ports::PortError::Permanent("credential unavailable".into())
        })
    }
}

const LOCAL_WORKER: &str = r#"#!/usr/bin/python3
import json, pathlib, subprocess, sys, time
root = pathlib.Path(__file__).parent
mode = (root/'mode').read_text() if (root/'mode').exists() else ''
m = json.loads(pathlib.Path('input-manifest.json').read_bytes())
role = m['task'].split(':')[0]
def git(*a):
    return subprocess.check_output(['git',*a],stderr=subprocess.DEVNULL).decode().strip()
if role == 'producer':
    with (root/'calls').open('a') as f: f.write('producer\n')
    if mode == 'fail': sys.exit(1)
    if mode == 'timeout': time.sleep(5)
    if mode == 'missing-manifest': pathlib.Path('input-manifest.json').unlink()
    if mode == 'changed-manifest': pathlib.Path('input-manifest.json').write_text('{}')
    p = pathlib.Path('.foundry/a01-proof.txt'); p.parent.mkdir(exist_ok=True)
    p.write_bytes(b'wrong\n' if mode == 'wrong-proof' else b'foundry-a01-fixed-task-v1\n')
    if mode == 'extra-change': pathlib.Path('extra').write_text('extra')
    if mode == 'symlink':
        p.unlink(); p.symlink_to('../README')
    git('add','.foundry/a01-proof.txt')
    if mode == 'extra-change': git('add','extra')
    git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','proof')
    commit = git('rev-parse','HEAD')
    if mode == 'wrong-parent':
        git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-qm','second')
        commit = git('rev-parse','HEAD')
    if mode == 'wrong-head': git('checkout','--detach',m['base'])
    if mode == 'forged-commit': commit = '0'*40
    output = dict(produced_commit=commit,findings=False,verdict_category=None,verdict_categories=[],inspected_commit=None)
else:
    output = dict(produced_commit=None,findings=mode=='held',verdict_category='defer' if mode=='held' and role=='verify' else None,verdict_categories=[],inspected_commit=git('rev-parse','HEAD'))
pathlib.Path('die-output.json').write_text(json.dumps(output,separators=(',',':')))
"#;

#[derive(Clone)]
struct Trace {
    task: StageTask,
    output: StageOutput,
    durable: Option<Snapshot>,
}
type Before = Box<dyn FnMut(&foundry_exec::ReviewDispatchCompletion) -> Result<(), String> + Send>;
struct TraceDie {
    inner: DieStageExecutor,
    trace: Arc<Mutex<Vec<Trace>>>,
    calls: Arc<AtomicUsize>,
    available: bool,
    before: Option<Before>,
    path: Option<PathBuf>,
}
impl TraceDie {
    fn new(inner: DieStageExecutor) -> Self {
        Self {
            inner,
            trace: Arc::new(Mutex::new(vec![])),
            calls: Arc::new(AtomicUsize::new(0)),
            available: true,
            before: None,
            path: None,
        }
    }
    fn observe(&mut self) {
        if let Some(path) = self.path.as_ref() {
            if let Some(last) = must(self.trace.lock()).last_mut() {
                last.durable = Some(snapshot(path));
            }
        }
    }
}
impl StageExecutor for TraceDie {
    fn ensure_available(&self) -> Result<(), String> {
        if !self.available {
            Err("unavailable".into())
        } else {
            self.inner.ensure_available()
        }
    }
    fn execute(&mut self, t: &StageTask) -> Result<StageOutput, String> {
        self.observe();
        self.calls.fetch_add(1, Ordering::SeqCst);
        let o = self
            .inner
            .execute(t)
            .map_err(|_| "A01 review execution rejected")?;
        must(self.trace.lock()).push(Trace {
            task: t.clone(),
            output: o.clone(),
            durable: None,
        });
        Ok(o)
    }
    fn restore_session(&mut self, id: &str, path: &Path) -> Result<(), String> {
        self.inner.restore_session(id, path)
    }
    fn before_terminal_commit(
        &mut self,
        c: &foundry_exec::ReviewDispatchCompletion,
    ) -> Result<(), String> {
        self.observe();
        if let Some(f) = self.before.as_mut() {
            f(c)
        } else {
            Ok(())
        }
    }
}

// This observer deliberately does not call the producer's file/digest helpers.
// It re-reads all four physical files and the referenced retained output.
fn independently_check_evidence(root: &Path, a: &A01AcceptedProducerOutput) -> Result<(), ()> {
    let o = decode_a01_real_output(&a.reservation, &a.output).map_err(|_| ())?;
    let work = root.join("producer").join(&o.manifest.order_id);
    let paths = [
        work.join("input-manifest.json"),
        work.join("die-output.json"),
        work.join("attempt-0.log"),
        root.join("sessions")
            .join(format!("{}.json", o.manifest.order_id)),
    ];
    let expected = [
        &o.evidence.manifest_sha256,
        &o.evidence.die_output_sha256,
        &o.evidence.process_log_sha256,
        &o.evidence.session_record_sha256,
    ];
    let mut files = Vec::new();
    for (p, digest) in paths.iter().zip(expected) {
        if !fs::symlink_metadata(p)
            .map_err(|_| ())?
            .file_type()
            .is_file()
        {
            return Err(());
        }
        let bytes = fs::read(p).map_err(|_| ())?;
        if hex::encode(Sha256::digest(&bytes)) != *digest {
            return Err(());
        }
        files.push(bytes);
    }
    let session: serde_json::Value = serde_json::from_slice(&files[3]).map_err(|_| ())?;
    let copy = PathBuf::from(session["output_copy"].as_str().ok_or(())?);
    let allowed = root
        .join("sessions/outputs")
        .canonicalize()
        .map_err(|_| ())?
        .join(format!("{}.json", sha(&files[1])));
    if copy != allowed
        || fs::read(copy).map_err(|_| ())? != files[1]
        || session["manifest"] != serde_json::to_value(&o.manifest).map_err(|_| ())?
    {
        return Err(());
    }
    if files[0] != serde_json::to_vec(&o.manifest).map_err(|_| ())? {
        return Err(());
    }
    Ok(())
}
fn check_real_git(f: &Fixture, a: &A01AcceptedProducerOutput) {
    let o = must(decode_a01_real_output(&a.reservation, &a.output));
    let work = f.creator.root.join("producer").join(&o.manifest.order_id);
    assert!(
        must(git_text(&work, &["rev-parse", "HEAD^{commit}"])) == a.result_commit,
        "A01 real HEAD mismatch"
    );
    let object = must(git_text(&work, &["cat-file", "-p", &a.result_commit]));
    let parents: Vec<_> = object
        .lines()
        .take_while(|s| !s.is_empty())
        .filter_map(|s| s.strip_prefix("parent "))
        .collect();
    assert!(
        parents.as_slice() == [a.base_commit.as_str()],
        "A01 real parent mismatch"
    );
    assert!(
        must(git(
            &work,
            &[
                "diff-tree",
                "--no-commit-id",
                "--name-status",
                "--no-renames",
                "-r",
                &a.base_commit,
                &a.result_commit
            ]
        )) == b"A\t.foundry/a01-proof.txt\n",
        "A01 proof changes mismatch"
    );
    assert!(
        must(git(
            &work,
            &[
                "show",
                &format!("{}:.foundry/a01-proof.txt", a.result_commit)
            ]
        )) == b"foundry-a01-fixed-task-v1\n",
        "A01 proof bytes mismatch"
    );
    must(independently_check_evidence(&f.creator.root, a));
}

fn cargo(args: &[&str]) -> std::process::Output {
    cargo_target(args, None)
}
fn cargo_target(args: &[&str], target: Option<&Path>) -> std::process::Output {
    let mut command = Command::new("cargo");
    command
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .env("TMPDIR", "/home/cosmo/.hermes/cache/scratch")
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_INCREMENTAL", "0")
        .args(args);
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    }
    must(command.output())
}
#[test]
fn a01_g25_live_assembly_boundary() {
    let f = Fixture::local();
    let before = snapshot(f.creator.store.path());
    let bytes = must(fs::read(f.creator.store.path()));
    let help = cargo(&[
        "run",
        "--locked",
        "-p",
        "foundry",
        "--offline",
        "--no-default-features",
        "--",
        "--help",
    ]);
    assert!(
        help.status.success()
            && !String::from_utf8_lossy(&help.stdout).contains("a01-harness-live"),
        "A01 default CLI surface escaped"
    );
    let path = must(f.creator.store.path().to_str().ok_or(()));
    let rejected = cargo(&[
        "run",
        "--locked",
        "-p",
        "foundry",
        "--offline",
        "--no-default-features",
        "--",
        "product-watch",
        "--db",
        path,
        "--installation-id",
        &f.creator.config.installation_id,
        "--database-id",
        &f.creator.config.database_id,
        "--service-file",
        "/nonexistent-a01-service",
        "--repository-id",
        "42",
        "--once",
    ]);
    assert!(
        !rejected.status.success()
            && String::from_utf8_lossy(&rejected.stderr).contains("product database rejected"),
        "A01 default marked opener not exercised"
    );
    assert!(
        must(fs::read(f.creator.store.path())) == bytes,
        "A01 default opener changed marked file"
    );
    unchanged(&before, &snapshot(f.creator.store.path()));
    let feature_help = cargo(&[
        "run",
        "--locked",
        "-p",
        "foundry",
        "--features",
        "a01-harness-support",
        "--offline",
        "--",
        "--help",
    ]);
    assert!(
        feature_help.status.success()
            && String::from_utf8_lossy(&feature_help.stdout).contains("a01-harness-live"),
        "A01 feature CLI missing"
    );
    let main = include_str!("../src/main.rs");
    for module in ["a01_harness_live", "a01_real_git_producer"] {
        assert!(
            main.contains(&format!(
                "#[cfg(feature = \"a01-harness-support\")]\npub(crate) mod {module};"
            )),
            "A01 module feature boundary changed"
        );
    }
    assert!(
        main.contains("let work = ProductWorkClass::from(&drain.work_claim)")
            && main.contains("UnavailableProductProducer"),
        "A01 ordinary watch boundary changed"
    );
    // Compile-fail probes link the actual default ledger artifact from cargo's JSON output.
    let built = cargo(&[
        "build",
        "--locked",
        "-p",
        "foundry-ledger",
        "--offline",
        "--no-default-features",
        "--message-format=json",
    ]);
    assert!(built.status.success(), "A01 default ledger build failed");
    let artifact = String::from_utf8_lossy(&built.stdout)
        .lines()
        .filter_map(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .find(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "foundry_ledger")
        .expect("A01 default ledger artifact missing");
    let rlib = artifact["filenames"]
        .as_array()
        .expect("A01 default filenames missing")
        .iter()
        .filter_map(|v| v.as_str())
        .find(|p| p.ends_with(".rlib"))
        .expect("A01 default rlib missing");
    for code in [
        "use foundry_ledger::product::A01RealOutput; fn main() {}",
        "fn main() { let _ = foundry_ledger::ProductStore::prepare_a01_authorization_request; }",
    ] {
        let probe = f.dir.0.join("feature-probe.rs");
        must(fs::write(&probe, code));
        let result = must(
            Command::new("rustc")
                .args(["--edition=2024", "--crate-name", "feature_probe"])
                .arg(&probe)
                .arg("--extern")
                .arg(format!("foundry_ledger={rlib}"))
                .arg("-L")
                .arg(format!(
                    "dependency={}",
                    must(Path::new(rlib).parent().ok_or(())).display()
                ))
                .arg("--out-dir")
                .arg(&f.dir.0)
                .output(),
        );
        assert!(
            !result.status.success(),
            "A01 default DTO/helper escaped feature"
        );
    }
}
#[test]
fn a01_g28_live_creator_identity_is_retained() {
    let f = Fixture::local();
    let other = Fixture::local();
    let before = snapshot(f.creator.store.path());
    let intake = must(f.creator.checked());
    let host = must(f.creator.backend());
    let finalizer = must(f.creator.backend());
    for handle in [&intake, &host, &finalizer] {
        assert!(
            handle.path() == f.creator.store.path(),
            "A01 checked physical path mismatch"
        );
    }
    let after = snapshot(f.creator.store.path());
    drop((intake, host, finalizer));
    unchanged(&after, &snapshot(f.creator.store.path()));
    assert!(
        Creator::create(f.creator.root.clone(), "acme", "project", "42").is_err(),
        "A01 existing directory adopted"
    );
    let original = f.creator.root.join("original.db");
    must(fs::rename(f.creator.store.path(), &original));
    must(fs::copy(other.creator.store.path(), f.creator.store.path()));
    let replacement = must(fs::read(f.creator.store.path()));
    assert!(f.creator.checked().is_err(), "A01 replacement accepted");
    assert!(
        must(fs::read(f.creator.store.path())) == replacement,
        "A01 replacement repaired"
    );
    must(fs::remove_file(f.creator.store.path()));
    must(fs::rename(&original, f.creator.store.path()));
    // A retained creator from B cannot reopen the replaced A file either.
    let b_original = other.creator.root.join("original.db");
    must(fs::rename(other.creator.store.path(), &b_original));
    must(fs::copy(f.creator.store.path(), other.creator.store.path()));
    assert!(
        other.creator.checked().is_err(),
        "A01 foreign retained identity accepted"
    );
    assert!(
        before["product_orders_v1"] == snapshot(f.creator.store.path())["product_orders_v1"],
        "A01 identity probe changed admission"
    );
}
fn real_value(r: &A01ProducerReservation) -> A01RealOutput {
    A01RealOutput {
        version: 1,
        kind: "real-git".into(),
        task_digest: r.task.task_digest.clone(),
        task_receipt_sha256: must(a01_real_task_receipt_sha256(&r.task)),
        reservation_sha256: must(a01_real_reservation_sha256(r)),
        base_commit: "a".repeat(40),
        result_commit: "b".repeat(40),
        manifest: must(a01_real_manifest(r, &"a".repeat(40))),
        evidence: A01RealEvidence {
            manifest_sha256: "c".repeat(64),
            die_output_sha256: "d".repeat(64),
            process_log_sha256: "e".repeat(64),
            session_record_sha256: "f".repeat(64),
            exit_code: 0,
        },
    }
}
fn frame(o: &A01RealOutput) -> String {
    format!(
        "foundry-a01-real-output/v1\n{}",
        must(serde_json::to_string(o))
    )
}
#[test]
fn a01_g32_live_output_codec_and_limits() {
    let mut f = Fixture::local();
    let (claim, r) = f.reserve();
    let o = real_value(&r);
    let good = must(encode_a01_real_output(&r, &o));
    assert!(
        good.len() <= 4096
            && must(decode_a01_real_output(&r, &good)) == o
            && must(encode_a01_real_output(
                &r,
                &must(decode_a01_real_output(&r, &good))
            )) == good,
        "A01 canonical round trip failed"
    );
    let mut bad = vec![
        String::new(),
        "x".repeat(4097),
        format!("{good}\0"),
        good.replace("real-output/v1", "real-output/v2"),
        format!("{good}\n"),
        good.replacen("{", "{\"unknown\":0,", 1),
        good.replacen("\"version\":1,", "", 1),
        good.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
        good.replacen("\"version\":1", "\"version\":1.0", 1),
        good.replacen("real-git", "real\\u002dgit", 1),
    ];
    for i in 0..20 {
        let mut x = o.clone();
        match i {
            0 => x.version = 2,
            1 => x.kind = "synthetic".into(),
            2 => x.task_digest = "0".repeat(64),
            3 => x.task_receipt_sha256 = "0".repeat(64),
            4 => x.reservation_sha256 = "0".repeat(64),
            5 => x.base_commit = "A".repeat(40),
            6 => x.result_commit = "b".repeat(64),
            7 => x.manifest.order_id.push('x'),
            8 => x.manifest.attempt = 1,
            9 => x.manifest.task = "producer:other".into(),
            10 => x.manifest.profile = "other".into(),
            11 => x.manifest.base = "c".repeat(40),
            12 => x.manifest.fix_context = true,
            13 => x.manifest.context.reverse(),
            14 => x.evidence.manifest_sha256 = "G".repeat(64),
            15 => x.evidence.die_output_sha256.clear(),
            16 => x.evidence.process_log_sha256 = "a".repeat(63),
            17 => x.evidence.session_record_sha256.push('a'),
            18 => x.evidence.exit_code = 1,
            _ => x.result_commit = x.base_commit.clone(),
        };
        bad.push(frame(&x));
    }
    let before = snapshot(f.creator.store.path());
    for output in bad {
        assert!(
            decode_a01_real_output(&r, &output).is_err(),
            "A01 invalid codec accepted"
        );
        assert!(
            f.creator
                .store
                .accept_a01_producer_output(&f.creator.service, &claim, &r, &output)
                .is_err(),
            "A01 invalid output appended"
        );
        unchanged(&before, &snapshot(f.creator.store.path()));
    }
    // Full task/reservation binding, independently of the public string fields.
    for i in 0..8 {
        let mut changed = r.clone();
        match i {
            0 => changed.task.owner.scope_revision += 1,
            1 => changed.task.bridge_permission.grant_revision += 1,
            2 => changed.claim.fence += 1,
            3 => changed.claim.epoch += 1,
            4 => changed.request_key.push('x'),
            5 => changed.sequence += 1,
            6 => changed.task.admission.store.database_id.push('x'),
            _ => changed.task.service.credential_revision += 1,
        };
        assert!(
            decode_a01_real_output(&changed, &good).is_err(),
            "A01 binding substitution accepted"
        );
        unchanged(&before, &snapshot(f.creator.store.path()));
    }
}
#[test]
fn a01_g33_live_acceptance_and_readback_agree() {
    let mut f = Fixture::local();
    let (claim, a, _) = f.accepted();
    let read = must(must(f.creator.checked()).read_a01_producer_output(
        &f.creator.service,
        &claim,
        &a.reservation,
    ));
    assert!(read == Some(a.clone()), "A01 readback disagreement");
    check_real_git(&f, &a);
    let exec = f.executor();
    let calls = exec.calls.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let path = f.creator.store.path().to_path_buf();
    for i in 0..6 {
        let mut m = a.clone();
        match i {
            0 => m.output.push(' '),
            1 => m.reservation.task.owner.scope_revision += 1,
            2 => m.reservation.claim.fence += 1,
            3 => {
                let mut o = must(decode_a01_real_output(&a.reservation, &a.output));
                o.manifest.attempt = 1;
                m.output = frame(&o);
            }
            4 => m.intent.activation += 1,
            _ => m.result_commit = "a".repeat(40),
        };
        let write = must(Connection::open(&path));
        must(write.execute("UPDATE product_review_redrive_v1 SET artifact_json=?1 WHERE payload='a01-producer-output-v1'",[must(serde_json::to_string(&m))]));
        drop(write);
        let before = snapshot(&path);
        assert!(
            f.creator
                .store
                .read_a01_producer_output(&f.creator.service, &claim, &a.reservation)
                .is_err(),
            "A01 altered readback accepted"
        );
        assert!(
            host.try_dispatch_product_work(&a.intent, &claim).is_err(),
            "A01 altered output admitted host"
        );
        unchanged(&before, &snapshot(&path));
        assert!(
            calls.load(Ordering::SeqCst) == 0,
            "A01 altered output invoked executor"
        );
        must(must(Connection::open(&path)).execute("UPDATE product_review_redrive_v1 SET artifact_json=?1 WHERE payload='a01-producer-output-v1'",[must(serde_json::to_string(&a))]));
    }
    // Corrupt the immutable original only inside this disposable fixture.
    let write = must(Connection::open(&path));
    let trigger:String=must(write.query_row("SELECT name FROM sqlite_schema WHERE type='trigger' AND tbl_name='ledger' AND sql LIKE '%BEFORE UPDATE%' LIMIT 1",[],|r|r.get(0)));
    must(write.execute_batch(&format!(
        "DROP TRIGGER \"{}\"",
        trigger.replace('"', "\"\"")
    )));
    let original: String = must(write.query_row(
        "SELECT intent FROM ledger WHERE seq=?1",
        [a.sequence],
        |r| r.get(0),
    ));
    let changed = original.replacen("real-git", "real-bad", 1);
    must(write.execute(
        "UPDATE ledger SET intent=?1 WHERE seq=?2",
        rusqlite::params![changed, a.sequence],
    ));
    drop(write);
    let before = snapshot(&path);
    assert!(
        f.creator
            .store
            .read_a01_producer_output(&f.creator.service, &claim, &a.reservation)
            .is_err()
            && host.try_dispatch_product_work(&a.intent, &claim).is_err(),
        "A01 changed original accepted"
    );
    unchanged(&before, &snapshot(&path));
}
#[test]
fn a01_g34_live_synthetic_cannot_satisfy_success() {
    let mut f = Fixture::local();
    let (claim, r) = f.reserve();
    let text = "historical synthetic fixture";
    let a = must(
        f.creator
            .store
            .accept_a01_producer_output(&f.creator.service, &claim, &r, text),
    );
    assert!(
        a.base_commit.len() == 64
            && a.result_commit.len() == 64
            && a.base_commit != a.result_commit,
        "A01 synthetic computation changed"
    );
    assert!(
        must(must(f.creator.checked()).read_a01_producer_output(&f.creator.service, &claim, &r))
            == Some(a),
        "A01 historical synthetic read failed"
    );
    assert!(
        decode_a01_real_output(&r, text).is_err(),
        "A01 synthetic satisfied live success"
    );
    let mut f = Fixture::local();
    let (claim, r) = f.reserve();
    let before = snapshot(f.creator.store.path());
    for text in [
        "foundry-a01-real-output/v0\n{}",
        "foundry-a01-real-output/v1\n{}",
        "foundry-a01-real-output/broken",
    ] {
        assert!(
            f.creator
                .store
                .accept_a01_producer_output(&f.creator.service, &claim, &r, text)
                .is_err(),
            "A01 malformed namespace fell back"
        );
        unchanged(&before, &snapshot(f.creator.store.path()));
    }
}
#[test]
fn a01_g35_live_preflight_has_no_execution_effect() {
    for producer_missing in [true, false] {
        let mut f = Fixture::local();
        let (task, claim) = f.claim();
        let mut producer = f.producer();
        let mut exec = f.executor();
        let calls = exec.calls.clone();
        if producer_missing {
            must(fs::remove_file(&f.binary));
        } else {
            exec.available = false;
        }
        let mut host = must(f.creator.host(&pack(), exec, &f.base));
        let before = snapshot(f.creator.store.path());
        assert!(
            a01_coordinator::dispatch(
                &mut f.creator.store,
                &f.creator.service,
                &claim,
                task.revision,
                "producer",
                &mut producer,
                &mut host
            )
            .is_err(),
            "A01 unavailable execution accepted"
        );
        assert!(
            producer.invocations == 0
                && calls.load(Ordering::SeqCst) == 0
                && count(f.creator.store.path(), "product_review_redrive_v1") == 0,
            "A01 preflight caused execution"
        );
        unchanged(&before, &snapshot(f.creator.store.path()));
        no_host(f.creator.store.path());
    }
}
#[test]
fn a01_g36_live_failed_execution_never_reaches_host() {
    for mode in [
        "fail",
        "timeout",
        "missing-manifest",
        "changed-manifest",
        "forged-commit",
        "wrong-head",
        "wrong-parent",
        "wrong-proof",
        "extra-change",
        "symlink",
    ] {
        let mut f = Fixture::local();
        must(fs::write(f.dir.0.join("mode"), mode));
        let (task, claim) = f.claim();
        let mut p = must(RealGitProducer::new(
            f.binary.clone(),
            1,
            f.repo.clone(),
            f.creator.root.clone(),
            &f.base,
        ));
        let exec = f.executor();
        let calls = exec.calls.clone();
        let mut host = must(f.creator.host(&pack(), exec, &f.base));
        assert!(
            a01_coordinator::dispatch(
                &mut f.creator.store,
                &f.creator.service,
                &claim,
                task.revision,
                "producer",
                &mut p,
                &mut host
            )
            .is_err(),
            "A01 bad execution accepted"
        );
        assert!(
            p.invocations == 1
                && calls.load(Ordering::SeqCst) == 0
                && count(f.creator.store.path(), "product_review_redrive_v1") == 0,
            "A01 failed execution reached host"
        );
        no_host(f.creator.store.path());
        let before = snapshot(f.creator.store.path());
        assert!(
            a01_coordinator::dispatch(
                &mut f.creator.store,
                &f.creator.service,
                &claim,
                task.revision,
                "retry",
                &mut p,
                &mut host
            )
            .is_err(),
            "A01 automatic retry accepted"
        );
        assert!(p.invocations == 1, "A01 producer re-invoked");
        unchanged(&before, &snapshot(f.creator.store.path()));
        assert!(
            count(f.creator.store.path(), "product_transition_receipts_v1") > 0,
            "A01 reservation evidence lost"
        );
    }
    evidence_negative_probes(&mut Fixture::local());
}
fn evidence_negative_probes(f: &mut Fixture) {
    let (_, a, _) = f.accepted();
    let o = must(decode_a01_real_output(&a.reservation, &a.output));
    let work = f.creator.root.join("producer").join(&o.manifest.order_id);
    for p in [
        work.join("input-manifest.json"),
        work.join("die-output.json"),
        work.join("attempt-0.log"),
        f.creator
            .root
            .join("sessions")
            .join(format!("{}.json", o.manifest.order_id)),
    ] {
        let bytes = must(fs::read(&p));
        must(fs::remove_file(&p));
        assert!(
            independently_check_evidence(&f.creator.root, &a).is_err(),
            "A01 missing evidence accepted"
        );
        must(fs::write(&p, b"changed"));
        assert!(
            independently_check_evidence(&f.creator.root, &a).is_err(),
            "A01 changed evidence accepted"
        );
        must(fs::write(p, bytes));
    }
    let record: serde_json::Value = must(serde_json::from_slice(&must(fs::read(
        f.creator
            .root
            .join("sessions")
            .join(format!("{}.json", o.manifest.order_id)),
    ))));
    let copy = PathBuf::from(
        record["output_copy"]
            .as_str()
            .expect("A01 retained path missing"),
    );
    let original = must(fs::read(&copy));
    must(fs::write(&copy, b"changed retained output"));
    assert!(
        independently_check_evidence(&f.creator.root, &a).is_err(),
        "A01 changed retained output accepted"
    );
    must(fs::write(copy, original));
    must(independently_check_evidence(&f.creator.root, &a));
}

fn hold_expectation(f: &Fixture) -> ProductHoldExpectation {
    must(db(f.creator.store.path()).query_row("SELECT lifecycle,attempt,originating_run,activation,host_epoch,hold_revision,source_revision,expected_predecessor,snapshot_digest FROM product_review_holds_v1 ORDER BY rowid DESC LIMIT 1",[],|r|Ok(ProductHoldExpectation{lifecycle:r.get(0)?,attempt:r.get(1)?,originating_run:r.get(2)?,activation:r.get(3)?,host_epoch:r.get(4)?,hold_revision:r.get(5)?,source_revision:r.get(6)?,predecessor_seq:r.get(7)?,snapshot_digest:r.get(8)?})))
}
#[test]
fn a01_g41_live_p04_missing_host_http() {
    let mut f = Fixture::local();
    must(fs::write(f.dir.0.join("mode"), "held"));
    let (d, _, _) = f.dispatch();
    let hold = d.host.held.as_ref().expect("A01 local active hold missing");
    let expected = hold_expectation(&f);
    let state = foundry_serve::auth_http::ProductHttpState::new(must(f.creator.backend()));
    let router = foundry_serve::review_hold::product_router(state);
    let runtime = must(tokio::runtime::Runtime::new());
    let body = must(serde_json::to_vec(
        &serde_json::json!({"expected":expected,"decision":"commit_pass","target":null}),
    ));
    let route = format!(
        "/api/product/orders/{}/review/holds/{}/accept",
        d.output.intent.order.0, hold.hold_instance_id
    );
    // A real HTTP request traverses the unchanged local router, without a socket.
    async fn request<S: axum::ServiceExt<axum::http::Request<axum::body::Body>>>(
        mut service: S,
        request: axum::http::Request<axum::body::Body>,
    ) -> Result<S::Response, S::Error> {
        futures::future::poll_fn(|cx| service.poll_ready(cx)).await?;
        service.call(request).await
    }
    let post = |key: &str| {
        let req = must(
            axum::http::Request::builder()
                .method("POST")
                .uri(&route)
                .header(
                    "Authorization",
                    format!("Bearer {}", f.creator.session_bearer),
                )
                .header("Idempotency-Key", key)
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(body.clone())),
        );
        let response = runtime.block_on(async { must(request(router.clone(), req).await) });
        let status = response.status();
        let bytes = runtime
            .block_on(async { must(axum::body::to_bytes(response.into_body(), 65536).await) });
        (status, must(String::from_utf8(bytes.to_vec())))
    };
    let before = snapshot(f.creator.store.path());
    let (status, response) = post("missing-runtime");
    assert!(
        status == axum::http::StatusCode::SERVICE_UNAVAILABLE
            && response.contains("product review runtime unavailable"),
        "A01 missing runtime response wrong"
    );
    unchanged(&before, &snapshot(f.creator.store.path()));
    // An exact accepted decision retry is independent of runtime presence.
    let receipt = must(must(f.creator.backend()).accept_product_review_hold(
        &f.creator.session,
        &d.output.intent.order.0,
        &hold.hold_instance_id,
        &expected,
        "already-accepted",
        ProductHoldDecision::CommitPass,
        Utc::now(),
    ));
    let before = snapshot(f.creator.store.path());
    let (status, response) = post("already-accepted");
    assert!(
        (status == axum::http::StatusCode::OK || status == axum::http::StatusCode::ACCEPTED)
            && response.contains(&receipt.operation_id),
        "A01 accepted retry lost receipt"
    );
    // Projection catch-up is allowed, while acceptance/progress/run/stage/result remain exact.
    let after = snapshot(f.creator.store.path());
    for t in [
        "product_review_hold_acceptances_v1",
        "product_review_hold_progress_v1",
        "product_review_runs_v1",
        "product_review_stages_v1",
        "artifact_tuples",
        "product_result_bindings_v1",
    ] {
        assert!(
            before[t] == after[t],
            "A01 missing host retry changed durable execution"
        );
    }
}
#[test]
fn a01_g42_live_p04_wrong_store_host() {
    for format in ["sha1", "sha256"] {
        let mut a = Fixture::local_format(format);
        let b = Fixture::local_format(format);
        let (claim, output, _) = a.accepted();
        let exec = b.executor();
        let calls = exec.calls.clone();
        let mut wrong = must(b.creator.host(&pack(), exec, &b.base));
        let mut correct = must(a.creator.host(&pack(), a.executor(), &a.base));
        let before_a = snapshot(a.creator.store.path());
        let before_b = snapshot(b.creator.store.path());
        assert!(
            wrong
                .try_dispatch_product_work(&output.intent, &claim)
                .is_err(),
            "A01 wrong-store host accepted"
        );
        assert!(
            calls.load(Ordering::SeqCst) == 0,
            "A01 wrong-store executor invoked"
        );
        unchanged(&before_a, &snapshot(a.creator.store.path()));
        unchanged(&before_b, &snapshot(b.creator.store.path()));
        no_host(a.creator.store.path());
        no_host(b.creator.store.path());
        assert!(
            must(a.creator.store.read_a01_producer_output(
                &a.creator.service,
                &claim,
                &output.reservation
            )) == Some(output.clone()),
            "A01 wrong-store probe lost output/claim"
        );
        let result = must(correct.try_dispatch_product_work(&output.intent, &claim));
        assert!(
            result.exit_code == 0
                && result.held.is_none()
                && result.generations[0].commit == output.result_commit,
            "A01 correct-store positive control failed"
        );
        assert!(
            count(a.creator.store.path(), "product_review_runs_v1") == 1,
            "A01 correct-store host missing"
        );
        no_host(b.creator.store.path());
        let d = a01_coordinator::AcceptedDispatch {
            output,
            host: result,
        };
        let first = must(a.creator.finalize(&d));
        let before = snapshot(a.creator.store.path());
        assert!(
            must(a.creator.finalize(&d)) == first && first.result_commit == d.output.result_commit,
            "A01 local real finalizer replay failed"
        );
        unchanged(&before, &snapshot(a.creator.store.path()));
    }
}
#[test]
fn a01_g43_live_existing_contracts_remain_green() {
    let mut failures = Vec::new();
    for (index, args) in [
        vec![
            "test",
            "--locked",
            "-p",
            "foundry",
            "--features",
            "a01-harness-support",
            "--offline",
            "--test",
            "b2_w2a_harness",
            "--",
            "--test-threads=1",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry-ledger",
            "--offline",
            "--test",
            "a01_marker_feature_off",
            "--test",
            "w1_marker_feature_off",
            "--test",
            "b1_product_atomic",
            "--test",
            "b1_product_results",
            "--test",
            "b1_scope",
            "--test",
            "b1_service_authority",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry-ledger",
            "--features",
            "w1-isolated-test-support",
            "--offline",
            "--test",
            "w1_isolated_store",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry-ledger",
            "--features",
            "a01-harness-support",
            "--offline",
            "--test",
            "a01_isolated_store",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry-ledger",
            "--features",
            "a01-harness-support",
            "--offline",
            "--lib",
            "a01_binding_foundation",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry-ledger",
            "--features",
            "a01-harness-support",
            "--offline",
            "--lib",
            "a01_task_tests",
            "--",
            "--test-threads=1",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry",
            "--offline",
            "--test",
            "b1_auth_cli",
            "--test",
            "b1_approve_cli",
            "--test",
            "b1_scope_cli",
            "--test",
            "b1_service_cli",
            "--test",
            "b1_watch_cli",
            "--test",
            "b1_publish_cli_default",
            "--test",
            "b2_w2a_harness_default",
        ],
        vec![
            "test",
            "--locked",
            "-p",
            "foundry",
            "--features",
            "p2-test-support",
            "--offline",
            "--test",
            "b1_publish_cli",
            "--test",
            "b1_approve_cli",
        ],
    ]
    .into_iter()
    .enumerate()
    {
        // The existing marker probe requires exactly one ledger rlib. Keep
        // its default build isolated from feature artifacts without changing
        // any existing test assertion.
        let isolated = (index == 1).then(Dir::new);
        let output = cargo_target(&args, isolated.as_ref().map(|d| d.0.as_path()));
        if !output.status.success() {
            let mut log = output.stdout;
            log.extend(output.stderr);
            must(fs::write(format!("/tmp/a01-g43-failure-{index}.log"), log));
            failures.push(index);
        }
    }
    assert!(
        failures.is_empty(),
        "A01 regression command failures; diagnostics retained locally"
    );
}
#[test]
#[ignore = "requires real GitHub admission; A01_LIVE_* configuration and real Die"]
fn a01_g26_live_real_admission_and_ack() {
    let mut f = Fixture::live();
    let before = snapshot(f.creator.store.path());
    let request = f.request();
    unchanged(&before, &snapshot(f.creator.store.path()));
    let a = &request.admission;
    let c = db(f.creator.store.path());
    let (kind,payload,issue,version,updated,state):(String,String,String,String,String,String)=must(c.query_row("SELECT o.task_kind,o.payload_snapshot,i.issue_id,i.version_digest,m.observed_updated_at,i.state FROM product_orders_v1 o JOIN product_work_backlog_v1 b USING(order_id) JOIN product_intake_inbox_v1 i USING(delivery_id) JOIN product_intake_delivery_meta_v1 m USING(delivery_id) WHERE o.order_id=?1",[&a.order_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))));
    assert!(
        kind == "github-issue"
            && issue == f.issue
            && state == "acked"
            && version == delivery_version_digest(&issue, &updated, &kind, &payload),
        "A01 live snapshot/version/ack mismatch"
    );
    assert!(
        github_delivery_id(
            &a.store.installation_id,
            &a.connector_id,
            &a.api_origin,
            &a.stable_repository_id,
            &issue,
            &version
        ) == a.delivery_id,
        "A01 live delivery mismatch"
    );
    assert!(
        f.receipts.iter().any(|r| r.order_id == a.order_id
            && r.sequence == a.admission_seq
            && r.event_hash == a.admission_hash),
        "A01 selected receipt not returned by tick"
    );
    no_host(f.creator.store.path());
    assert!(
        count(f.creator.store.path(), "product_a01_task_revisions_v1") == 0
            && count(f.creator.store.path(), "product_work_claims_v1") == 0,
        "A01 first tick authorized or claimed"
    );
}
#[test]
#[ignore = "requires real GitHub admission; A01_LIVE_* configuration and real Die"]
fn a01_g27_live_receipt_selection_and_claim() {
    let mut f = Fixture::live();
    let request = f.request();
    let before = snapshot(f.creator.store.path());
    assert!(
        f.creator.request(&[], &f.issue).is_err(),
        "A01 empty selection authorized"
    );
    let mut foreign = f.receipts.clone();
    foreign[0].event_hash = "0".repeat(64);
    assert!(
        f.creator.request(&foreign, &f.issue).is_err(),
        "A01 foreign receipt selected"
    );
    let duplicates: Vec<_> = f
        .receipts
        .iter()
        .chain(f.receipts.iter())
        .cloned()
        .collect();
    let same = must(f.creator.request(&duplicates, &f.issue));
    // expires_at_ns is derived from the wall clock at each call, so it is
    // compared field-wise excluding it (duplicate selection equivalence).
    assert!(
        same.request_key == request.request_key
            && same.expected_revision == request.expected_revision
            && same.admission == request.admission
            && same.owner == request.owner
            && same.service == request.service
            && same.bridge_permission == request.bridge_permission,
        "A01 identical duplicate receipt changed selection"
    );
    unchanged(&before, &snapshot(f.creator.store.path()));
    let (task, claim) = must(f.creator.authorize_claim(&request));
    let view = must(
        must(f.creator.store.read_a01_task_claim(
            &f.creator.session,
            claim.order_id(),
            task.admission.lifecycle,
        ))
        .ok_or(()),
    );
    assert!(
        view.order_id == request.admission.order_id
            && view.delivery_id == request.admission.delivery_id
            && view.task_revision == task.revision
            && view.service_id == request.service.service_id
            && view.source_instance_id == request.admission.source_instance_id
            && view.payload_digest == request.admission.raw_payload_digest
            && view.worker_id == claim.review_binding().worker_id
            && view.epoch == claim.review_binding().epoch
            && view.fence == claim.review_binding().fence,
        "A01 selected claim binding mismatch"
    );
    assert!(
        count(f.creator.store.path(), "product_a01_task_revisions_v1") == 1
            && count(f.creator.store.path(), "product_work_claims_v1") == 1,
        "A01 other admission executed"
    );
}
#[test]
#[ignore = "requires real GitHub admission; A01_LIVE_* configuration and real Die"]
fn a01_g29_live_fixed_task_manifest_mapping() {
    let mut f = Fixture::live();
    let original = snapshot(f.creator.store.path());
    let (_, a, producer) = f.accepted();
    let r = &a.reservation;
    let o = must(decode_a01_real_output(r, &a.output));
    let task = must(producer.stage_task(r));
    assert!(
        r.task.task_kind == "a01-fixed-v1"
            && r.task.task_bytes == b"foundry-a01-fixed-task-v1\n"
            && task.role == "producer"
            && task.stage_id == "a01-fixed-v1"
            && task.base_commit == task.artifact_commit
            && task.fix_context.is_none(),
        "A01 fixed stage mapping mismatch"
    );
    let mut digest = Sha256::new();
    digest.update(b"foundry-product-task-v1\0");
    for v in [
        b"a01-fixed-v1".as_slice(),
        b"foundry-a01-fixed-task-v1\n".as_slice(),
    ] {
        digest.update((v.len() as u64).to_be_bytes());
        digest.update(v);
    }
    assert!(
        hex::encode(digest.finalize()) == r.task.task_digest,
        "A01 marker digest changed"
    );
    assert!(
        o.manifest == must(a01_real_manifest(r, &a.base_commit))
            && o.manifest.context
                == task
                    .inputs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>(),
        "A01 actual manifest mapping mismatch"
    );
    fn bound<T: serde::Serialize>(domain: &str, value: &T) -> String {
        let bytes = must(serde_json::to_vec(value));
        let mut h = Sha256::new();
        h.update((domain.len() as u64).to_be_bytes());
        h.update(domain.as_bytes());
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
        hex::encode(h.finalize())
    }
    let reservation_digest = bound("foundry-a01-real-reservation-v1", r);
    assert!(
        o.reservation_sha256 == reservation_digest
            && o.task_receipt_sha256 == bound("foundry-a01-real-task-receipt-v1", &r.task),
        "A01 independent reservation/task binding mismatch"
    );
    let expected_inputs: BTreeMap<String, String> = [
        ("action", "create-a01-proof-v1".to_owned()),
        ("proof_path", ".foundry/a01-proof.txt".to_owned()),
        ("task_kind", "a01-fixed-v1".to_owned()),
        ("task_utf8", "foundry-a01-fixed-task-v1\n".to_owned()),
        ("task_digest", r.task.task_digest.clone()),
        ("reservation_sha256", reservation_digest.clone()),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect();
    assert!(
        task.inputs == expected_inputs
            && o.manifest.order_id == format!("a01p-{reservation_digest}")
            && o.manifest.task == "producer:a01-fixed-v1"
            && o.manifest.profile == "coding"
            && o.manifest.attempt == 0
            && !o.manifest.fix_context,
        "A01 independent manifest mapping mismatch"
    );
    let after = snapshot(f.creator.store.path());
    for t in [
        "product_orders_v1",
        "product_intake_inbox_v1",
        "product_order_payload_digests_v1",
    ] {
        assert!(
            original[t] == after[t],
            "A01 original issue snapshot changed"
        );
    }
    check_real_git(&f, &a);
}
fn external_producer_calls(f: &Fixture) -> usize {
    must(fs::read_to_string(f.dir.0.join("external-invocations")))
        .lines()
        .filter(|s| *s == "producer:a01-fixed-v1")
        .count()
}
fn live_external_proof(
    f: &mut Fixture,
) -> (ProductWorkClaim, A01AcceptedProducerOutput, RealGitProducer) {
    let (claim, a, p) = f.accepted();
    assert!(
        p.invocations == 1 && external_producer_calls(f) == 1,
        "A01 external Die invocation count wrong"
    );
    check_real_git(f, &a);
    // Amendment 1: fail closed at every evidence boundary, including retained bytes.
    let o = must(decode_a01_real_output(&a.reservation, &a.output));
    let work = f.creator.root.join("producer").join(&o.manifest.order_id);
    for path in [
        work.join("input-manifest.json"),
        work.join("die-output.json"),
        work.join("attempt-0.log"),
        f.creator
            .root
            .join("sessions")
            .join(format!("{}.json", o.manifest.order_id)),
    ] {
        let saved = must(fs::read(&path));
        must(fs::remove_file(&path));
        assert!(
            independently_check_evidence(&f.creator.root, &a).is_err(),
            "A01 lineage accepted missing evidence"
        );
        must(fs::write(&path, b"altered"));
        assert!(
            independently_check_evidence(&f.creator.root, &a).is_err(),
            "A01 lineage accepted altered evidence"
        );
        must(fs::write(&path, saved));
    }
    let record: serde_json::Value = must(serde_json::from_slice(&must(fs::read(
        f.creator
            .root
            .join("sessions")
            .join(format!("{}.json", o.manifest.order_id)),
    ))));
    let copy = PathBuf::from(
        record["output_copy"]
            .as_str()
            .expect("A01 retained output missing"),
    );
    let saved = must(fs::read(&copy));
    must(fs::write(&copy, b"changed"));
    assert!(
        independently_check_evidence(&f.creator.root, &a).is_err(),
        "A01 lineage accepted changed retained output"
    );
    must(fs::write(copy, saved));
    must(independently_check_evidence(&f.creator.root, &a));
    (claim, a, p)
}
#[test]
#[ignore = "positive evidence requires actual external Die and live GitHub, never the local fixture worker"]
fn a01_g30_live_external_die_and_real_commit() {
    let mut f = Fixture::live();
    live_external_proof(&mut f);
}
#[test]
#[ignore = "requires real GitHub admission and real Die review using the shared commit cache"]
fn a01_g31_live_generated_commit_cache_is_shared() {
    let mut f = Fixture::live();
    let (claim, a, _) = live_external_proof(&mut f);
    assert!(
        git(
            &f.repo,
            &["cat-file", "-e", &format!("{}^{{commit}}", a.result_commit)]
        )
        .is_err(),
        "A01 producer result was planted in source"
    );
    assert!(
        f.creator.root.join("sessions/.commits.git").is_dir(),
        "A01 generated cache missing"
    );
    let exec = f.executor();
    let trace = exec.trace.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let result = must(host.try_dispatch_product_work(&a.intent, &claim));
    assert!(
        result.generations[0].commit == a.result_commit,
        "A01 generation zero changed"
    );
    for t in must(trace.lock()).iter() {
        assert!(
            t.task.base_commit == a.result_commit
                && must(git_text(&t.task.workdir, &["rev-parse", "HEAD^{commit}"]))
                    == a.result_commit,
            "A01 review failed cache checkout"
        );
    }
    assert!(
        !must(trace.lock()).is_empty(),
        "A01 review checkout not observed"
    );
    assert!(
        git(
            &f.repo,
            &["cat-file", "-e", &format!("{}^{{commit}}", a.result_commit)]
        )
        .is_err(),
        "A01 source modified to hide cache disconnect"
    );
}

fn verify_stage_evidence(f: &Fixture, trace: &Arc<Mutex<Vec<Trace>>>, artifact: &str) {
    let trace = must(trace.lock());
    assert!(!trace.is_empty(), "A01 real review not invoked");
    let c = db(f.creator.store.path());
    let mut stages=must(c.prepare("SELECT stage_id,output_json,sequence FROM product_review_stages_v1 WHERE status='complete' ORDER BY sequence"));
    let rows: Vec<(String, String, i64)> =
        must(must(stages.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))).collect());
    assert!(
        rows.len() <= trace.len(),
        "A01 stage trace cardinality mismatch"
    );
    // Durable stage evidence references the preserved accepted-output copy while
    // the recorded invocation log is the raw die-output.json; equality is proven
    // at the byte-digest level (G30 guarantees the two are byte-identical), and
    // held stages may have invocations without a complete durable row.
    let mut consumed = vec![false; trace.len()];
    for (id, bytes, seq) in rows {
        let evidence: serde_json::Value = must(serde_json::from_str(&bytes));
        let expected = evidence["sha256"].as_str().unwrap_or("");
        let matches: Vec<usize> = trace
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                !consumed[*i]
                    && t.task.stage_id == id
                    && &sha(&must(fs::read(&t.output.log_path))) == expected
            })
            .map(|(i, _)| i)
            .collect();
        assert!(
            matches.len() == 1,
            "A01 stage trace not one-to-one: id={id} evidence_sha={expected} trace_matches={} all_stages={:?}",
            matches.len(),
            trace.iter().map(|t| (t.task.stage_id.clone(), t.output.log_path.clone())).collect::<Vec<_>>(),
        );
        consumed[matches[0]] = true;
        let t = &trace[matches[0]];
        let log = must(fs::read(&t.output.log_path));
        assert!(
            evidence["sha256"] == sha(&log),
            "A01 stage file digest mismatch"
        );
        if t.task.role == "verify" || t.task.role == "lens" {
            assert!(
                t.task.artifact_commit == artifact
                    && t.output.inspected_commit.as_deref() == Some(artifact)
                    && evidence["inspected_commit"] == artifact
                    && must(git_text(&t.task.workdir, &["rev-parse", "HEAD^{commit}"])) == artifact,
                "A01 review inspected different artifact"
            );
        }
        let physical: serde_json::Value = must(serde_json::from_slice(&must(fs::read(
            t.task.workdir.join("input-manifest.json"),
        ))));
        assert!(
            physical["order_id"] == t.task.session_id
                && physical["base"] == t.task.base_commit
                && physical["task"] == format!("{}:{}", t.task.role, t.task.stage_id),
            "A01 review manifest trace mismatch"
        );
        assert!(
            evidence["input_manifest"] == physical,
            "A01 durable stage manifest mismatch"
        );
        let durable = t
            .durable
            .as_ref()
            .expect("A01 stage acceptance observation missing");
        let matching_sessions = durable["product_review_sessions_v1"]
            .iter()
            .filter(|row| {
                row.iter().any(|value| {
                    if let Value::Text(json) = value {
                        if let Ok(session) = serde_json::from_str::<serde_json::Value>(json) {
                            return session["session_id"] == t.task.session_id
                                && session["sha256"] == sha(&log);
                        }
                    }
                    false
                })
            })
            .count();
        assert!(
            matching_sessions == 1,
            "A01 durable session boundary mismatch"
        );
        let artifacts:i64=must(c.query_row("SELECT count(*) FROM product_artifacts_v1 WHERE stage_id=?1 AND stage_sequence=?2 AND artifact_sha256=?3",rusqlite::params![id,seq,sha(&log)],|r|r.get(0)));
        assert!(artifacts == 1, "A01 durable artifact not one-to-one");
    }
}
#[test]
#[ignore = "requires actual Die review executor and real GitHub admission"]
fn a01_g37_live_review_uses_real_stage_evidence() {
    let mut f = Fixture::live();
    let (d, p, trace) = f.dispatch();
    assert!(
        p.invocations == 1 && d.host.exit_code == 0 && d.host.held.is_none(),
        "A01 real review did not pass"
    );
    check_real_git(&f, &d.output);
    verify_stage_evidence(&f, &trace, &d.output.result_commit);
}
fn check_bridge(f: &Fixture, d: &a01_coordinator::AcceptedDispatch) {
    let c = db(f.creator.store.path());
    let bytes:String=must(c.query_row("SELECT intent FROM ledger WHERE json_valid(intent) AND json_extract(intent,'$.kind')='a01-passed-bridge-v1'",[],|r|r.get(0)));
    let v: serde_json::Value = must(serde_json::from_str(&bytes));
    assert!(
        v["task"] == must(serde_json::to_value(&d.output.reservation.task))
            && v["claim"] == must(serde_json::to_value(&d.output.reservation.claim))
            && v["output_sequence"] == d.output.sequence
            && v["output_event_hash"] == d.output.event_hash
            && v["host_result_digest"] == must(product_host_result_digest(&d.host)),
        "A01 exact bridge lineage mismatch"
    );
    let exact:bool=must(c.query_row("SELECT EXISTS(SELECT 1 FROM ledger l JOIN product_transition_receipts_v1 r ON r.seq=l.seq AND r.event_hash=l.hash WHERE r.operation='a01-passed-bridge-v1' AND l.event='InspectionPassed' AND l.to_state='waiting' AND r.expected_predecessor=json_extract(l.intent,'$.predecessor_seq'))",[],|r|r.get(0)));
    assert!(
        exact
            && bridge_count(f.creator.store.path()) == 1
            && count(f.creator.store.path(), "product_transition_intents_v1") == 0,
        "A01 bridge receipt/intent mismatch"
    );
    let terminal:bool=must(c.query_row("SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 c JOIN product_work_backlog_v1 b USING(order_id,lifecycle) WHERE c.state='completed' AND b.state='completed')",[],|r|r.get(0)));
    assert!(terminal, "A01 bridge claim/backlog incomplete");
}
#[test]
#[ignore = "requires real GitHub/Die Passed lineage and separate checked finalizer"]
fn a01_g38_live_bridge_and_finalizer_are_exact() {
    let mut f = Fixture::live();
    let (d, _, _) = f.dispatch();
    assert!(
        d.host.exit_code == 0 && d.host.held.is_none(),
        "A01 live pass missing"
    );
    check_bridge(&f, &d);
    assert!(
        count(f.creator.store.path(), "artifact_tuples") == 0
            && count(f.creator.store.path(), "product_result_bindings_v1") == 0,
        "A01 finalizer ran early"
    );
    let first = must(f.creator.finalize(&d));
    let before = snapshot(f.creator.store.path());
    let second = must(f.creator.finalize(&d));
    assert!(
        first == second
            && first.result_commit == d.output.result_commit
            && first.base_commit == d.output.base_commit,
        "A01 exact finalizer replay failed"
    );
    unchanged(&before, &snapshot(f.creator.store.path()));
    assert!(
        count(f.creator.store.path(), "artifact_tuples") == 1
            && count(f.creator.store.path(), "product_result_bindings_v1") == 1,
        "A01 finalizer pair wrong"
    );
}
fn permission_change(c: &mut Creator, mutation: ScopeMutation, key: &str) {
    let at = c.clock.now();
    let rev = must(
        c.store
            .list_product_scope_grants(&c.admin, "a01-owner", "a01-source", at),
    )
    .first()
    .expect("A01 scope missing")
    .scope_revision;
    must(c.store.change_product_scope(
        &c.admin,
        "a01-owner",
        "a01-source",
        ProductCapability::A01BridgePassed,
        mutation,
        rev,
        key,
        at,
    ));
}
#[test]
#[ignore = "requires real GitHub/Die stage evidence, revocation and ABA boundaries"]
fn a01_g39_live_permission_revocation_and_aba() {
    let mut f = Fixture::live();
    let request = f.request();
    let before = snapshot(f.creator.store.path());
    assert!(
        before["product_a01_task_revisions_v1"].is_empty(),
        "A01 bridge issuance authorized work"
    );
    let task = must(f.creator.store.authorize_a01_task(
        &f.creator.session,
        &f.creator.service,
        &request,
    ));
    let after = snapshot(f.creator.store.path());
    for (table, rows) in before
        .iter()
        .filter(|(t, _)| t.starts_with("product_scope_"))
    {
        assert!(after[table] == *rows, "A01 authorize changed scope grants");
    }
    permission_change(&mut f.creator, ScopeMutation::Revoke, "revoke");
    permission_change(&mut f.creator, ScopeMutation::Grant, "regrant");
    let current = must(
        must(
            f.creator
                .store
                .read_a01_bridge_permission(&f.creator.session, "a01-source"),
        )
        .ok_or(()),
    );
    assert!(
        current != task.bridge_permission,
        "A01 permission ABA identity revived"
    );
    assert!(
        match f.creator.store.claim_product_work_for_service(
            &f.creator.service,
            "old-task",
            Utc::now(),
            Duration::minutes(5)
        ) {
            Ok(ProductWorkClaimResult::Claimed(_)) => false,
            Ok(_) => true,
            // Fail-closed: after revoke→regrant the live authority no longer
            // matches the stale receipt, so the store rejects the claim.
            Err(foundry_ledger::ProductError::Invariant(_)) => true,
            Err(_) => false,
        },
        "A01 old task revived after ABA"
    );
    no_host(f.creator.store.path());
    let mut f = Fixture::live();
    let (claim, a, _) = f.accepted();
    let mut exec = f.executor();
    let mut revoker = must(f.creator.checked());
    let admin = f.creator.admin.clone();
    exec.before = Some(Box::new(move |_| {
        let at = Utc::now();
        let rev = must(revoker.list_product_scope_grants(&admin, "a01-owner", "a01-source", at))
            .first()
            .expect("A01 scope missing")
            .scope_revision;
        must(revoker.change_product_scope(
            &admin,
            "a01-owner",
            "a01-source",
            ProductCapability::A01BridgePassed,
            ScopeMutation::Revoke,
            rev,
            "terminal-revoke",
            at,
        ));
        Ok(())
    }));
    let trace = exec.trace.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let _ = host.try_dispatch_product_work(&a.intent, &claim);
    assert!(
        !must(trace.lock()).is_empty()
            && count(f.creator.store.path(), "product_review_stages_v1") > 0
            && count(f.creator.store.path(), "product_review_host_results_v1") == 1,
        "A01 revoked terminal evidence lost"
    );
    assert!(
        bridge_count(f.creator.store.path()) == 0
            && count(f.creator.store.path(), "artifact_tuples") == 0
            && count(f.creator.store.path(), "product_result_bindings_v1") == 0,
        "A01 revoked terminal created result"
    );
}
#[test]
#[ignore = "requires real GitHub, A01_LIVE_HELD_DIE yielding Held and actual Die continuation"]
fn a01_g40_live_hold_and_accepted_continuation() {
    let mut f = Fixture::live();
    let (claim, a, _) = f.accepted();
    let normal = f.binary.clone();
    f.binary = must(PathBuf::from(must(std::env::var("A01_LIVE_HELD_DIE"))).canonicalize());
    let exec = f.executor();
    let trace = exec.trace.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let held = must(host.try_dispatch_product_work(&a.intent, &claim));
    drop(host);
    let hold = held.held.as_ref().expect("A01 actual Held missing");
    assert!(
        bridge_count(f.creator.store.path()) == 0
            && count(f.creator.store.path(), "artifact_tuples") == 0,
        "A01 Held created result"
    );
    let claimed: bool = must(db(f.creator.store.path()).query_row(
        "SELECT EXISTS(SELECT 1 FROM product_work_claims_v1 WHERE state='claimed')",
        [],
        |r| r.get(0),
    ));
    assert!(claimed, "A01 Held lost claim");
    verify_stage_evidence(&f, &trace, &a.result_commit);
    let expected = hold_expectation(&f);
    let target = hold
        .resume_targets
        .first()
        .expect("A01 resume target missing")
        .clone();
    let decision = must(must(f.creator.backend()).accept_product_review_hold(
        &f.creator.session,
        &a.intent.order.0,
        &hold.hold_instance_id,
        &expected,
        "accept-held",
        ProductHoldDecision::Resume(target),
        Utc::now(),
    ));
    let before = snapshot(f.creator.store.path())["product_review_hold_acceptances_v1"].clone();
    f.binary = normal;
    let mut reassembled = must(f.creator.host(&pack(), f.executor(), &f.base));
    must(reassembled.continue_accepted_product_hold(
        &f.creator.session,
        &decision.operation_id,
        Utc::now(),
    ));
    assert!(
        before == snapshot(f.creator.store.path())["product_review_hold_acceptances_v1"],
        "A01 immutable decision changed"
    );
    let progress = must(must(f.creator.backend()).product_review_progress(
        &f.creator.session,
        &decision.operation_id,
        Utc::now(),
    ));
    assert!(
        progress.phase == "completed",
        "A01 creator-lifetime continuation incomplete"
    );
    // This is host reassembly while the original creator remains alive, not an OS restart.
    must(f.creator.checked());
}

struct CommitBarrierProducer {
    inner: RealGitProducer,
    path: PathBuf,
    reader: Option<Connection>,
}
impl Producer for CommitBarrierProducer {
    fn ensure_available(&self) -> Result<(), String> {
        self.inner.ensure_available()
    }
    fn produce(
        &mut self,
        task: &[u8],
        claim: &ProductWorkClaim,
        r: &A01ProducerReservation,
    ) -> Result<String, String> {
        let output = self.inner.produce(task, claim, r)?;
        let c = Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| "A01 barrier failed")?;
        c.execute_batch("BEGIN; SELECT count(*) FROM ledger;")
            .map_err(|_| "A01 barrier failed")?;
        self.reader = Some(c);
        Ok(output)
    }
}
#[test]
#[ignore = "requires real GitHub/Die full lineage, evidence integrity and commit-uncertain observation"]
fn a01_g44_live_evidence_and_uncertain_commit() {
    let mut f = Fixture::live();
    let admission = snapshot(f.creator.store.path());
    let request = f.request();
    unchanged(&admission, &snapshot(f.creator.store.path()));
    let task = must(f.creator.store.authorize_a01_task(
        &f.creator.session,
        &f.creator.service,
        &request,
    ));
    let authorized = snapshot(f.creator.store.path());
    assert!(
        authorized["product_a01_task_revisions_v1"].len() == 1
            && authorized["product_work_claims_v1"].is_empty(),
        "A01 authorization boundary missing"
    );
    let claim = match must(f.creator.store.claim_product_work_for_service(
        &f.creator.service,
        "lineage",
        Utc::now(),
        Duration::hours(1),
    )) {
        ProductWorkClaimResult::Claimed(c) => c,
        _ => panic!("A01 lineage claim missing"),
    };
    let claimed = snapshot(f.creator.store.path());
    assert!(
        claimed["product_work_claims_v1"].len() == 1
            && claimed["product_review_redrive_v1"].is_empty(),
        "A01 claim boundary missing"
    );
    let r = must(f.creator.store.reserve_a01_producer(
        &f.creator.service,
        &claim,
        task.revision,
        "producer",
    ));
    let reserved = snapshot(f.creator.store.path());
    assert!(
        reserved["product_review_redrive_v1"].is_empty(),
        "A01 reservation produced output"
    );
    let mut producer = f.producer();
    let output = must(producer.produce(&r.task.task_bytes, &claim, &r));
    unchanged(&reserved, &snapshot(f.creator.store.path()));
    let a = must(f.creator.store.accept_a01_producer_output(
        &f.creator.service,
        &claim,
        &r,
        &output,
    ));
    let accepted = snapshot(f.creator.store.path());
    assert!(
        producer.invocations == 1
            && a.output == output
            && a.reservation == r
            && r.task == task
            && task.admission == request.admission,
        "A01 output lineage mismatch"
    );
    assert!(
        external_producer_calls(&f) == 1,
        "A01 external lineage invocation count wrong"
    );
    // Amendment 1 is part of the lineage observation itself, before host admission.
    must(independently_check_evidence(&f.creator.root, &a));
    check_real_git(&f, &a);
    assert!(
        must(must(f.creator.checked()).read_a01_producer_output(&f.creator.service, &claim, &r))
            == Some(a.clone()),
        "A01 lineage readback mismatch"
    );
    unchanged(&accepted, &snapshot(f.creator.store.path()));
    let exec = f.executor();
    let trace = exec.trace.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let result = must(host.try_dispatch_product_work(&a.intent, &claim));
    let d = a01_coordinator::AcceptedDispatch {
        output: a,
        host: result,
    };
    must(independently_check_evidence(&f.creator.root, &d.output));
    verify_stage_evidence(&f, &trace, &d.output.result_commit);
    check_bridge(&f, &d);
    let first = must(f.creator.finalize(&d));
    let finalized = snapshot(f.creator.store.path());
    assert!(
        must(f.creator.finalize(&d)) == first,
        "A01 lineage finalizer replay changed"
    );
    unchanged(&finalized, &snapshot(f.creator.store.path()));
    // A second independent live lineage forces COMMIT to return SQLITE_BUSY
    // after physical execution. The coordinator must stop without dispatch/retry.
    let mut f = Fixture::live();
    let (task, claim) = f.claim();
    let mut producer = CommitBarrierProducer {
        inner: f.producer(),
        path: f.creator.store.path().into(),
        reader: None,
    };
    let exec = f.executor();
    let calls = exec.calls.clone();
    let mut host = must(f.creator.host(&pack(), exec, &f.base));
    let result = a01_coordinator::dispatch(
        &mut f.creator.store,
        &f.creator.service,
        &claim,
        task.revision,
        "uncertain",
        &mut producer,
        &mut host,
    );
    assert!(
        matches!(result,Err(ref s) if s=="A01 output acceptance rejected or uncertain")
            && producer.inner.invocations == 1
            && calls.load(Ordering::SeqCst) == 0,
        "A01 uncertain commit retried or dispatched"
    );
    drop(producer.reader.take());
    no_host(f.creator.store.path());
    assert!(
        count(f.creator.store.path(), "product_review_redrive_v1") == 0
            && f.creator.root.join("producer").is_dir(),
        "A01 uncertain commit evidence lost"
    );
    let before = snapshot(f.creator.store.path());
    must(f.creator.checked());
    unchanged(&before, &snapshot(f.creator.store.path()));
}
