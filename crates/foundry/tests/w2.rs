//! Offline W20–22 acceptance. No connector transport or real Die is installed.
#![cfg(all(feature = "p2-test-support", feature = "product-pack-pins", unix))]
#[allow(dead_code)]
#[path = "support/b2_w1.rs"]
mod support;
use chrono::{Duration, Utc};
use foundry_ledger::{
    ProductCapability, ProductDelivery, ProductPollCursor, ProductServiceAction, ProductStore,
    ProductWorkClaimResult, ScopeMutation,
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use support::{RuntimeFixture, count, schema_snapshot, snapshot};

const SOURCE: &str = "b2-w1-source";
const SERVICE: &str = "b2-w1-service";
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn reader(path: &Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}
fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("/usr/bin/git")
        .current_dir(repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    out.stdout
}
fn oid(repo: &Path) -> String {
    String::from_utf8(git(repo, &["rev-parse", "HEAD"]))
        .unwrap()
        .trim()
        .into()
}

fn capture(f: &Fixture, phase: &str) {
    if let Some(out) = std::env::var_os("W2_EVIDENCE_DIR") {
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/acceptance/w2/observe.py");
        let run = Command::new("python3")
            .arg(script)
            .arg(&f.f.root.0)
            .arg(out)
            .arg(phase)
            .status()
            .unwrap();
        assert!(run.success());
    }
}

struct Fixture {
    f: RuntimeFixture,
    repo: PathBuf,
    base: String,
    head: String,
    pack: String,
}
impl Fixture {
    fn new() -> Self {
        let mut f = RuntimeFixture::new(&[]);
        let repo = f.root.0.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "local@example.invalid"]);
        fs::write(repo.join("README"), "first\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-qm", "base"]);
        let base = oid(&repo);
        fs::write(repo.join("README"), "first\nsecond\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-qm", "candidate"]);
        let head = oid(&repo);
        f.pack = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../skills/fixture-min/v1")
            .canonicalize()
            .unwrap();
        let verify_pack = f.root.0.join("verify-pack");
        fs::create_dir(&verify_pack).unwrap();
        for name in ["manifest.toml", "lens-a.md", "policy.toml"] {
            fs::copy(f.pack.join(name), verify_pack.join(name)).unwrap();
        }
        let policy = verify_pack.join("policy.toml");
        fs::write(
            &policy,
            fs::read_to_string(&policy)
                .unwrap()
                .replace("role = \"lens\"", "role = \"verify\""),
        )
        .unwrap();
        f.pack = verify_pack;
        let pack = support::refresh_fixture_manifest(&f.pack);
        let mut bootstrap = f.store();
        let phc =
            foundry_ledger::product_identity::ProductPasswordKdf::hash("fixture-admin-password")
                .unwrap();
        bootstrap
            .create_account(&f.owner_session, "scope-admin", "owner", &phc, Utc::now())
            .unwrap();
        let admin = bootstrap
            .login("scope-admin", "fixture-admin-password", Utc::now())
            .unwrap();
        fs::write(f.root.0.join("admin-verifier"), admin.digest()).unwrap();
        scope(
            &f,
            "owner",
            ProductCapability::AuthorizeTask,
            ScopeMutation::Grant,
            "task-authority",
        );
        f.store()
            .change_pack_approval(
                &f.owner_session,
                SOURCE,
                &pack,
                0,
                true,
                "offline-pack",
                Utc::now(),
            )
            .unwrap();
        for (file, bytes) in [("owner", &f.owner_bearer), ("service", &f.service_token)] {
            let path = f.root.0.join(file);
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            f,
            repo,
            base,
            head,
            pack,
        }
    }
    fn task(&self) -> String {
        // Canonical struct field order, independently spelled out by the reader.
        format!(
            r#"{{"kind":"code-change-v1","request":"Add a line","acceptance":["README updated"],"base_commit":"{}","pack_digest":"{}","allowed_paths":["README"],"checks":[{{"argv":["git","diff","--check"],"timeout_secs":5}}],"total_timeout_secs":120,"model":"none","max_model_calls":0,"max_cost_microusd":0,"max_fix_generations":0}}"#,
            self.base, self.pack
        )
    }
    fn cli(&self, cmd: &str, args: &[&str]) -> std::process::Output {
        self.cli_command(cmd, args).output().unwrap()
    }
    fn cli_command(&self, cmd: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_foundry"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.f.root.0)
            .env("TMPDIR", &self.f.root.0)
            .args([
                cmd,
                "--db",
                self.f.db_path.to_str().unwrap(),
                "--installation-id",
                &self.f.config.installation_id,
                "--database-id",
                &self.f.config.database_id,
            ])
            .args(args);
        command
    }
    fn delivery(&self, issue: &str, title: &str, body: &str) -> ProductDelivery {
        let payload = json!({"issue_number":17,"title":title,"body":body}).to_string();
        let updated = self.f.now.to_rfc3339();
        let version = foundry_ledger::product::delivery_version_digest(
            issue,
            &updated,
            "github-issue",
            &payload,
        );
        ProductDelivery {
            delivery_id: foundry_ledger::product::github_delivery_id(
                &self.f.config.installation_id,
                "github-primary",
                "https://api.github.com",
                "repo:1",
                issue,
                &version,
            ),
            connector_id: "github-primary".into(),
            repository_id: "repo:1".into(),
            issue_id: issue.into(),
            observed_updated_at: updated,
            version_digest: version,
            task_kind: "github-issue".into(),
            payload,
        }
    }
    fn persist(&self, items: &[ProductDelivery]) {
        let cursor = ProductPollCursor {
            connector_id: "github-primary".into(),
            query_fingerprint: "offline".into(),
            sweep_id: "offline".into(),
            next_page: 1,
            page_ceiling: 1,
            updated_at: self.f.now.to_rfc3339(),
        };
        self.f
            .store()
            .persist_intake_page_for_service(&self.f.service, &cursor, items, &[], Utc::now())
            .unwrap();
    }
    fn intake(&self, order: &str, d: &ProductDelivery) -> Value {
        let path = self.f.root.0.join("input");
        fs::write(&path, &d.payload).unwrap();
        let out = self.cli(
            "product-intake",
            &[
                "--connector",
                "github-primary",
                "--repository-id",
                "repo:1",
                "--service-file",
                self.f.root.0.join("service").to_str().unwrap(),
                "--order",
                order,
                "--issue-id",
                &d.issue_id,
                "--issue-snapshot",
                path.to_str().unwrap(),
                "--observed-updated-at",
                &d.observed_updated_at,
            ],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn select(&self, order: &str) {
        self.f
            .store()
            .select_order_pack(
                &self.f.owner_session,
                order,
                1,
                SOURCE,
                &self.pack,
                0,
                &format!("select-{order}"),
                Utc::now(),
            )
            .unwrap();
    }
    fn authorize(&self, order: &str, admission: &str) -> std::process::Output {
        self.authorize_digest(order, admission, &sha(self.task().as_bytes()))
    }
    fn authorize_digest(&self, order: &str, admission: &str, digest: &str) -> std::process::Output {
        self.cli(
            "product-task",
            &[
                "--session-file",
                self.f.root.0.join("owner").to_str().unwrap(),
                "--order",
                order,
                "--lifecycle",
                "1",
                "--admission-hash",
                admission,
                "--task-sha256",
                digest,
                "--request-key",
                &format!("owner-{order}"),
            ],
        )
    }
    fn approved(&self, order: &str) {
        let admission = self.intake(order, &self.delivery(order, "first", &self.task()));
        self.select(order);
        let out = self.authorize(order, admission["admission_hash"].as_str().unwrap());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
fn scope(f: &RuntimeFixture, actor: &str, cap: ProductCapability, op: ScopeMutation, key: &str) {
    let revision:i64=reader(&f.db_path).query_row("SELECT coalesce((SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2),0)",params![actor,SOURCE],|r|r.get(0)).unwrap();
    f.store()
        .change_product_scope(
            &fs::read_to_string(f.root.0.join("admin-verifier")).unwrap(),
            actor,
            SOURCE,
            cap,
            op,
            revision,
            key,
            Utc::now(),
        )
        .unwrap();
}

mod w20_contract {
    use super::*;
    #[test]
    fn canonical_bytes_and_owner_coordinates() {
        let f = Fixture::new();
        let canonical = f.task().replace(r#"["README"]"#, r#"["A","README"]"#);
        let mut value: Value = serde_json::from_str(&canonical).unwrap();
        value["allowed_paths"] = json!(["README", "A"]);
        let body = serde_json::to_string_pretty(&value).unwrap();
        let d = f.delivery("w20", "original", &body);
        let receipt = f.intake("w20", &d);
        let hash = receipt["admission_hash"].as_str().unwrap();
        f.select("w20");
        let mut store = f.f.store();
        let before = snapshot(&f.f.db_path);
        let normalized = sha(canonical.as_bytes());
        for (session, lifecycle, anchor, digest) in [
            (f.f.service_token.as_str(), 1, hash, normalized.as_str()),
            (f.f.owner_session.as_str(), 2, hash, normalized.as_str()),
            (f.f.owner_session.as_str(), 1, "wrong", normalized.as_str()),
            (f.f.owner_session.as_str(), 1, hash, "wrong"),
        ] {
            assert!(
                store
                    .authorize_code_change(session, "w20", lifecycle, anchor, digest, "approve-w20")
                    .is_err()
            );
        }
        assert!(before == snapshot(&f.f.db_path));
        let r = store
            .authorize_code_change(
                &f.f.owner_session,
                "w20",
                1,
                hash,
                &normalized,
                "approve-w20",
            )
            .unwrap();
        let bytes: Vec<u8> = reader(&f.f.db_path)
            .query_row(
                "SELECT task_bytes FROM product_code_tasks_v1 WHERE order_id='w20'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bytes, canonical.as_bytes());
        assert_eq!(sha(&bytes), r.task_sha256);
        assert_eq!(sha(d.payload.as_bytes()), r.snapshot_sha256);
        assert_eq!(r.delivery_id, d.delivery_id);
        assert_ne!(sha(body.as_bytes()), r.task_sha256);
        let before = snapshot(&f.f.db_path);
        assert_eq!(
            r,
            store
                .authorize_code_change(
                    &f.f.owner_session,
                    "w20",
                    1,
                    hash,
                    &normalized,
                    "approve-w20"
                )
                .unwrap()
        );
        assert!(
            store
                .authorize_code_change(
                    &f.f.owner_session,
                    "w20",
                    1,
                    hash,
                    &normalized,
                    "another-key"
                )
                .is_err()
        );
        assert!(before == snapshot(&f.f.db_path));
        println!(
            "W20 canonical_bytes={} raw_body_bytes={} receipt={}",
            bytes.len(),
            body.len(),
            serde_json::to_string(&r).unwrap()
        );
        capture(&f, "w20");
    }
}

mod w21_intake {
    use super::*;
    struct Ack(Vec<String>);
    impl foundry_watch::intake_worker::DeliveryAcknowledger for Ack {
        fn acknowledge(
            &mut self,
            d: &ProductDelivery,
        ) -> Result<(), foundry_watch::intake_worker::AckError> {
            self.0.push(d.delivery_id.clone());
            Ok(())
        }
    }
    #[test]
    fn disallowed_and_edited_observations() {
        let f = Fixture::new();
        let canary = f.f.root.0.join("shell-entered");
        let malicious = format!("$(touch {})", canary.display());
        let valid = f.task();
        let invalids = vec![
            "free text".to_owned(),
            malicious.clone(),
            valid.replace("\"README\"", "\"../README\""),
            valid.replace("\"git\",\"diff\",\"--check\"", "\"sh\",\"-c\",\"true\""),
            valid.replace("timeout_secs\":5", "timeout_secs\":121"),
            valid.replace("timeout_secs\":5", "timeout_secs\":0"),
            valid.replace("max_model_calls\":0", "max_model_calls\":1"),
            valid.replace("total_timeout_secs\":120", "total_timeout_secs\":0"),
            valid.replace("total_timeout_secs\":120", "total_timeout_secs\":3601"),
            valid.replace("\"none\"", "\"other\""),
            valid.replace("max_cost_microusd\":0", "max_cost_microusd\":1"),
        ];
        for (n, body) in invalids.iter().enumerate() {
            let order = format!("unsupported-{n}");
            let a = f.intake(&order, &f.delivery(&order, "invalid", body));
            f.select(&order);
            let before = snapshot(&f.f.db_path);
            assert!(
                !f.authorize_digest(
                    &order,
                    a["admission_hash"].as_str().unwrap(),
                    &sha(body.as_bytes())
                )
                .status
                .success()
            );
            assert!(before == snapshot(&f.f.db_path));
            assert!(!canary.exists());
        }
        for (issue, first, edit, supported) in [
            (
                "valid-first",
                valid.clone(),
                valid.replace("Add a line", "Replace the task"),
                true,
            ),
            ("unsupported-first", "later edit".into(), valid, false),
        ] {
            let a = f.delivery(issue, "A", &first);
            let mut b = f.delivery(issue, "B", &edit);
            // Make the edit sort before A under the old digest-ordered drain.
            for n in 0..10000 {
                b = f.delivery(issue, &format!("B{n}"), &edit);
                if b.delivery_id < a.delivery_id {
                    break;
                }
            }
            assert!(b.delivery_id < a.delivery_id);
            f.persist(&[a.clone(), b.clone()]);
            let mut store = f.f.store();
            assert!(
                store
                    .admit_intake_delivery_for_service(
                        &f.f.service,
                        &b.delivery_id,
                        issue,
                        Utc::now()
                    )
                    .is_err()
            );
            let mut ack = Ack(vec![]);
            let drain = foundry_watch::intake_worker::drain_admissions_for_service(
                &mut store,
                &mut ack,
                &f.f.service,
                Utc::now(),
                10,
            )
            .unwrap();
            assert_eq!(ack.0, [a.delivery_id.clone(), b.delivery_id.clone()]);
            assert_eq!(drain.receipts.len(), 2);
            assert_eq!(drain.receipts[0], drain.receipts[1]);
            let r = &drain.receipts[0];
            f.select(&r.order_id);
            let receipt = f.authorize(&r.order_id, &r.event_hash);
            assert_eq!(receipt.status.success(), supported);
            let stored:(String,String)=reader(&f.f.db_path).query_row("SELECT payload_snapshot,delivery_id FROM product_orders_v1 JOIN product_work_backlog_v1 USING(order_id,lifecycle) WHERE order_id=?1",[&r.order_id],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
            assert_eq!(stored, (a.payload.clone(), a.delivery_id.clone()));
            if supported {
                let before = snapshot(&f.f.db_path);
                assert!(
                    !f.authorize_digest(&r.order_id, &r.event_hash, &sha(edit.as_bytes()))
                        .status
                        .success()
                );
                assert!(before == snapshot(&f.f.db_path));
            }
            let retry = f.intake(&r.order_id, &b);
            assert_eq!(retry["admission_hash"], r.event_hash);
            assert_eq!(
                receipt.stdout,
                f.authorize(&r.order_id, &r.event_hash).stdout
            );
            if supported {
                let receipt: Value = serde_json::from_slice(&receipt.stdout).unwrap();
                assert_eq!(receipt["snapshot_sha256"], sha(a.payload.as_bytes()));
                assert_eq!(receipt["task_sha256"], sha(first.as_bytes()));
                assert_eq!(receipt["delivery_id"], a.delivery_id);
                assert_eq!(receipt["actor"], "owner");
                println!("E02b {issue} receipt={receipt}");
            }
            println!(
                "E02b {issue} deliveries={:?} admission={} order={}",
                ack.0, r.event_hash, r.order_id
            );
        }
        let mut store = f.f.store();
        assert!(!matches!(
            store
                .claim_product_work_for_service(
                    &f.f.service,
                    "watch",
                    Utc::now(),
                    Duration::minutes(1)
                )
                .unwrap(),
            ProductWorkClaimResult::Claimed(_)
        ));
        for table in [
            "product_producers_v1",
            "product_producer_events_v1",
            "product_work_claims_v1",
            "product_review_runs_v1",
            "product_result_bindings_v1",
            "product_approval_result_bindings_v1",
            "product_grants_v1",
            "product_publish_outcomes_v1",
        ] {
            assert_eq!(count(&f.f.db_path, table), 0);
        }
        assert_eq!(count(&f.f.db_path, "product_code_tasks_v1"), 1);
        assert!(!canary.exists());
        assert!(!f.f.artifact_root().exists());
        capture(&f, "w21");
        println!(
            "E02 disallowed={} claims=0 producer=0 review=0 grant=0 approval=0 canary_absent=true",
            invalids.len()
        );
    }
}

mod w22_code_change {
    use super::*;
    use foundry_ledger::{
        ProductApprovalIntent, ProductClock, ProductResultExpectation, ProductServiceGrant,
    };
    use foundry_line::{PackReviewHost, StageExecutor, StageOutput, StageTask};
    use std::{
        collections::BTreeMap,
        io::Read,
        process::Stdio,
        sync::Arc,
        time::{Duration as Wait, Instant},
    };
    const TEST: &str = "w22_code_change::fenced_producer_and_loss_boundaries";
    struct Inspect(PathBuf);
    impl StageExecutor for Inspect {
        fn offline_inspector(&self) -> bool {
            true
        }
        fn execute(&mut self, stage: &StageTask) -> Result<StageOutput, String> {
            assert!(matches!(stage.role.as_str(), "verify" | "lens"));
            let bytes = git(
                &self.0,
                &["show", &format!("{}:README", stage.artifact_commit)],
            );
            if bytes != b"first\nsecond\n" {
                return Err("unexpected Git blob".into());
            }
            let path = stage.workdir.join("inspection.json");
            fs::write(
                &path,
                json!({"commit":stage.artifact_commit,"blob_sha256":sha(&bytes)}).to_string(),
            )
            .unwrap();
            Ok(StageOutput {
                log_path: path.display().to_string(),
                inspected_commit: Some(stage.artifact_commit.clone()),
                ..Default::default()
            })
        }
    }
    fn tree(root: &Path) -> BTreeMap<PathBuf, (u64, Vec<u8>)> {
        use std::os::unix::fs::MetadataExt;
        fn walk(base: &Path, dir: &Path, rows: &mut BTreeMap<PathBuf, (u64, Vec<u8>)>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(base, &p, rows)
                } else {
                    rows.insert(
                        p.strip_prefix(base).unwrap().into(),
                        (p.metadata().unwrap().ino(), fs::read(p).unwrap()),
                    );
                }
            }
        }
        let mut rows = BTreeMap::new();
        walk(root, root, &mut rows);
        rows
    }
    fn sequence(db: &Path) -> Vec<(String, i64)> {
        reader(db)
            .prepare("SELECT name,seq FROM sqlite_sequence ORDER BY name")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
    fn worker(root: &Path) {
        let pack = root.join("verify-pack");
        let digest = foundry_pack::load_pack(&pack).unwrap().digest().to_owned();
        let mut store = ProductStore::open(
            &root.join("product.sqlite"),
            foundry_ledger::ProductConfig {
                installation_id: "b2-w1-runtime-install".into(),
                database_id: "b2-w1-runtime-database".into(),
            },
        )
        .unwrap()
        .with_product_registry(foundry_ledger::PackRegistryConfig {
            snapshots: [(digest, pack)].into(),
        });
        let service = store
            .authenticate_product_service_for_action(
                &fs::read_to_string(root.join("service")).unwrap(),
                "github-primary",
                "https://api.github.com",
                "repo:1",
                ProductServiceAction::DriveReview,
                Utc::now(),
            )
            .unwrap();
        let claim = store
            .reserve_code_change(&service, "loss", "lost-producer")
            .unwrap();
        store
            .validate_product_work_claim(&claim, Utc::now())
            .unwrap();
        let repo = root.join("loss-repo");
        let source = root.join("repo");
        // Local Git only. Start the owned producer at the authorized parent.
        git(
            root,
            &[
                "clone",
                "--local",
                "--no-hardlinks",
                "-q",
                source.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        let r = store.code_change_for_producer(&service, "loss").unwrap();
        git(&repo, &["checkout", "-q", "-B", "loss", &r.base_commit]);
        git(&repo, &["config", "user.name", "Owned producer"]);
        git(&repo, &["config", "user.email", "owned@example.invalid"]);
        fs::write(repo.join("README"), "first\nfixture child output\n").unwrap();
        git(&repo, &["add", "README"]);
        git(&repo, &["commit", "-qm", "Owned producer output"]);
        let head = oid(&repo);
        let out = root.join("retained-producer");
        fs::create_dir(&out).unwrap();
        fs::write(
            out.join("output.patch"),
            git(
                &repo,
                &[
                    "diff",
                    "--full-index",
                    "--binary",
                    &r.base_commit,
                    &head,
                    "--",
                ],
            ),
        )
        .unwrap();
        store
            .seal_code_change(&service, "loss", "lost-producer", &repo, &oid(&repo))
            .unwrap();
        fs::write(root.join("sealed-barrier"), "sealed").unwrap();
        // Parent owns this process. Stand at the real seal/collect boundary.
        let mut token = [0u8];
        std::io::stdin().read_exact(&mut token).unwrap();
        store
            .collect_code_change(&service, "loss", "lost-producer")
            .unwrap();
    }
    fn lost_child(f: &Fixture) {
        f.approved("loss");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("W2_PRODUCER_CHILD", &f.f.root.0)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let then = Instant::now();
        while !f.f.root.0.join("sealed-barrier").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "owned producer exited early"
            );
            if then.elapsed() > Wait::from_secs(30) {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("producer barrier timed out")
            }
            std::thread::sleep(Wait::from_millis(10));
        }
        child.kill().unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(9));
        let db = &f.f.db_path;
        let before = snapshot(db);
        let schema = schema_snapshot(db);
        let seq = sequence(db);
        let objects = tree(&f.repo.join(".git/objects"));
        assert_eq!(objects.len(), 6);
        let artifacts = tree(&f.f.artifact_root());
        let p =
            f.f.store()
                .code_change_status(&f.f.service, "loss", "lost-producer")
                .unwrap();
        assert_eq!(p.state, "sealed");
        let loss_repo = f.f.root.0.join("loss-repo");
        let loss_objects = tree(&loss_repo.join(".git/objects"));
        let producer_files = tree(&f.f.root.0.join("retained-producer"));
        assert_eq!(producer_files.len() + artifacts.len(), 6);
        assert_eq!(p.result_commit, Some(oid(&loss_repo)));
        assert_eq!(
            p.artifact_sha256,
            Some(sha(&fs::read(
                f.f.root.0.join("retained-producer/output.patch")
            )
            .unwrap()))
        );
        assert_eq!(
            git(&loss_repo, &["rev-parse", "HEAD^"]),
            format!("{}\n", f.base).as_bytes()
        );
        capture(f, "pre-recovery");
        let q =
            f.f.store()
                .mark_code_change_uncertain(&f.f.service, "loss", "lost-producer")
                .unwrap();
        assert_eq!(q.state, "uncertain");
        assert_eq!(q.artifact_sha256, p.artifact_sha256);
        assert_eq!(q.fence, p.fence);
        capture(f, "post-recovery");
        let after = snapshot(db);
        let changed: Vec<_> = before
            .keys()
            .filter(|k| before[*k] != after[*k])
            .map(String::as_str)
            .collect();
        assert_eq!(
            changed,
            ["product_producer_events_v1", "product_producers_v1"]
        );
        assert_eq!(seq, sequence(db));
        assert_eq!(after.len() + 1, 82);
        for retry in 1..=2 {
            let mut store = f.f.store();
            assert_eq!(
                store
                    .mark_code_change_uncertain(&f.f.service, "loss", "lost-producer")
                    .unwrap(),
                q
            );
            assert!(
                store
                    .reserve_code_change(&f.f.service, "loss", "lost-producer")
                    .is_err()
            );
            assert!(
                store
                    .collect_code_change(&f.f.service, "loss", "lost-producer")
                    .is_err()
            );
            assert!(
                store
                    .seal_code_change(&f.f.service, "loss", "lost-producer", &f.repo, &f.head)
                    .is_err()
            );
            assert!(after == snapshot(db));
            assert_eq!(seq, sequence(db));
            assert!(schema == schema_snapshot(db));
            assert!(artifacts == tree(&f.f.artifact_root()));
            assert!(objects == tree(&f.repo.join(".git/objects")));
            assert!(loss_objects == tree(&loss_repo.join(".git/objects")));
            assert!(producer_files == tree(&f.f.root.0.join("retained-producer")));
            capture(f, &format!("retry-{retry}"));
            println!(
                "E04 retry={retry} tables_equal={} git_object_files_equal={} host_files_equal={} producer_file_equal=1 schema_equal=true",
                after.len() + 1,
                objects.len(),
                artifacts.len()
            );
        }
        println!(
            "E04 owned_child_signal=9 changed={changed:?} operation={}",
            serde_json::to_string(&q).unwrap()
        );
    }

    #[test]
    fn fenced_producer_and_loss_boundaries() {
        if let Some(root) = std::env::var_os("W2_SERVE_PREFLIGHT") {
            let ledger = PathBuf::from(root).join("serve-preflight.db");
            assert!(
                matches!(foundry_serve::ServeState::new(&ledger),Err(e) if e=="invalid FOUNDRY_DIE_TIMEOUT")
            );
            assert!(!ledger.exists());
            return;
        }
        if let Some(root) = std::env::var_os("W2_PRODUCER_CHILD") {
            worker(Path::new(&root));
            return;
        }
        let f = Fixture::new();
        configuration_boundaries(&f);
        f.approved("good");
        let mut store = f.f.store();
        let service = &f.f.service;
        let claim = store
            .reserve_code_change(service, "good", "produce-good")
            .unwrap();
        store
            .validate_product_work_claim(&claim, Utc::now())
            .unwrap();
        assert!(
            store
                .collect_code_change(service, "good", "produce-good")
                .is_err()
        );
        assert!(
            store
                .seal_code_change(service, "good", "produce-good", &f.repo, &f.base)
                .is_err()
        );
        let sealed = store
            .seal_code_change(service, "good", "produce-good", &f.repo, &f.head)
            .unwrap();
        assert_stale_operations_refused(&mut at_deadline(&f, &claim), &f, "good", "produce-good");
        let (intent, claim, binding) = store
            .collect_code_change(service, "good", "produce-good")
            .unwrap();
        let mut unavailable = PackReviewHost::with_selected_product_pack(
            &f.f.pack,
            foundry_line::UnavailableStageExecutor,
            f.f.store(),
            f.f.artifact_root(),
            f.f.session_root(),
            f.f.claim_root(),
        )
        .unwrap();
        let before = snapshot(&f.f.db_path);
        assert!(
            unavailable
                .try_dispatch_selected_product_work(
                    &intent,
                    &claim,
                    binding.clone(),
                    "produce-good"
                )
                .is_err()
        );
        assert!(before == snapshot(&f.f.db_path));
        let mut host = PackReviewHost::with_selected_product_pack(
            &f.f.pack,
            Inspect(f.repo.clone()),
            f.f.store(),
            f.f.artifact_root(),
            f.f.session_root(),
            f.f.claim_root(),
        )
        .unwrap()
        .with_source_commit("e3631b1ba40511159b58ff7b2c2c0817a44d2cc5")
        .unwrap();
        let result = host
            .try_dispatch_selected_product_work(&intent, &claim, binding, "produce-good")
            .unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.held.is_none());
        let mut finalizer = f.f.store();
        finalizer
            .configure_product_artifact_root(f.f.artifact_root())
            .unwrap();
        let expect = ProductResultExpectation {
            host_key: foundry_exec::HostKey {
                order: "good".into(),
                attempt: 1,
                run: 1,
                activation: 1,
            },
            host_result_digest: foundry_ledger::product_host_result_digest(&result).unwrap(),
        };
        let finalized = finalizer
            .finalize_product_review_result(service, &expect)
            .unwrap();
        let db = reader(&f.f.db_path);
        let (bytes,digest,admission,head,base,epoch,fence,pack,tuple):(Vec<u8>,String,String,String,String,i64,i64,String,String)=db.query_row(
            "SELECT p.artifact,p.artifact_sha256,o.admission_hash,r.result_commit,r.base_commit,p.epoch,p.fence,r.pack_digest,b.tuple_digest FROM product_producers_v1 p JOIN product_orders_v1 o USING(order_id)
             JOIN product_review_runs_v1 r ON r.order_id=p.order_id AND r.admission_hash=o.admission_hash AND r.result_commit=p.result_commit AND r.status='passed'
             JOIN product_w1_lifecycle_pins_v1 pin ON pin.order_id=p.order_id AND pin.request_key=p.operation_key AND pin.claim_fence=p.fence AND pin.claim_epoch=p.epoch AND pin.pack_digest=r.pack_digest
             JOIN product_result_bindings_v1 b ON b.order_id=p.order_id AND b.run_id=1 AND b.generation=0
             WHERE p.order_id='good' AND p.state='collected'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).unwrap();
        assert_eq!(
            bytes,
            git(
                &f.repo,
                &["diff", "--binary", "--full-index", &f.base, &f.head, "--"]
            )
        );
        assert_eq!(sha(&bytes), digest);
        assert_eq!(sealed.artifact_sha256.as_ref(), Some(&digest));
        assert_eq!((&base, &head, &pack), (&f.base, &f.head, &f.pack));
        assert_eq!(tuple, finalized.tuple_digest);
        assert_eq!(
            (&finalized.base_commit, &finalized.result_commit),
            (&base, &head)
        );
        let expected_tuple = foundry_exec::ArtifactTuple {
            order: intent.order.clone(),
            attempt: intent.attempt,
            run: intent.run,
            generation: intent.generation,
            immutable_commit: foundry_exec::ImmutableCommit(head.clone()),
            artifact_location: foundry_exec::ArtifactLocation(result.log_path.clone()),
            generations: result.generations.clone(),
        };
        assert_eq!(expected_tuple.digest(), tuple);
        assert!(Path::new(&result.log_path).is_file());
        println!(
            "E03 independent reader base={base} head={head} pack={pack} epoch={epoch} fence={fence} admission={admission} sealed_patch_bytes={} sealed_sha256={digest} finalized={}",
            bytes.len(),
            serde_json::to_string(&finalized).unwrap()
        );
        lost_child(&f);
        authority_boundaries(&f, &finalized);
    }

    fn configuration_boundaries(f: &Fixture) {
        f.approved("config");
        let fake_die = f.f.root.0.join("never-executed");
        let canary = f.f.root.0.join("die-entered");
        fs::write(
            &fake_die,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", canary.display()),
        )
        .unwrap();
        fs::set_permissions(&fake_die, fs::Permissions::from_mode(0o700)).unwrap();
        let missing = f.f.root.0.join("absent");
        let service_file = f.f.root.0.join("service");
        let args = [
            "--action",
            "reserve",
            "--service-file",
            service_file.to_str().unwrap(),
            "--connector",
            "github-primary",
            "--repository-id",
            "repo:1",
            "--order",
            "config",
            "--request-key",
            "config-producer",
        ];
        let cmd = || {
            let mut c = f.cli_command("product-producer", &args);
            c.env("FOUNDRY_REPO_PATH", &f.repo)
                .env("FOUNDRY_DIE_BINARY", &fake_die)
                .env("FOUNDRY_REVIEW_PACK", &f.f.pack);
            c
        };
        for key in [
            "FOUNDRY_REVIEW_PACK",
            "FOUNDRY_DIE_BINARY",
            "FOUNDRY_REPO_PATH",
        ] {
            for value in [Some(&missing), None] {
                let before = snapshot(&f.f.db_path);
                let mut c = cmd();
                if let Some(v) = value {
                    c.env(key, v);
                } else {
                    c.env_remove(key);
                }
                assert!(!c.output().unwrap().status.success());
                assert!(before == snapshot(&f.f.db_path));
                assert!(!canary.exists());
            }
        }
        for timeout in ["bad", "0", "3601"] {
            let before = snapshot(&f.f.db_path);
            assert!(
                !cmd()
                    .env("FOUNDRY_DIE_TIMEOUT", timeout)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            assert!(before == snapshot(&f.f.db_path));
            let out = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env_clear()
                .env("W2_SERVE_PREFLIGHT", &f.f.root.0)
                .env("FOUNDRY_DIE_TIMEOUT", timeout)
                .output()
                .unwrap();
            assert!(out.status.success(), "serve timeout {timeout}");
        }
        let out = cmd().env("FOUNDRY_DIE_TIMEOUT", "1").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let p =
            f.f.store()
                .mark_code_change_uncertain(&f.f.service, "config", "config-producer")
                .unwrap();
        assert_eq!(p.state, "uncertain");
        assert_eq!(p.result_commit, None);
        assert_eq!(p.artifact_sha256, None);
        assert!(!canary.exists());
        println!(
            "E04 six missing/partial config denials, three serve timeout denials, valid reserve, injected pre-seal loss; Die entries=0"
        );
    }

    fn private_copy(db: &Path, dst: &Path) {
        reader(db)
            .execute("VACUUM INTO ?1", [dst.to_str().unwrap()])
            .unwrap();
    }

    fn assert_stale_operations_refused(
        store: &mut ProductStore,
        f: &Fixture,
        order: &str,
        key: &str,
    ) {
        let before = snapshot(&f.f.db_path);
        assert!(store.reserve_code_change(&f.f.service, order, key).is_err());
        assert!(
            store
                .seal_code_change(&f.f.service, order, key, &f.repo, &f.head)
                .is_err()
        );
        assert!(store.collect_code_change(&f.f.service, order, key).is_err());
        assert!(before == snapshot(&f.f.db_path));
    }

    fn at_deadline(f: &Fixture, claim: &foundry_ledger::ProductWorkClaim) -> ProductStore {
        struct Later(chrono::DateTime<Utc>);
        impl ProductClock for Later {
            fn now(&self) -> chrono::DateTime<Utc> {
                self.0
            }
        }
        ProductStore::open_with_clock(
            &f.f.db_path,
            f.f.config.clone(),
            Arc::new(Later(claim.lease_until())),
        )
        .unwrap()
        .with_product_registry(foundry_ledger::PackRegistryConfig {
            snapshots: [(f.pack.clone(), f.f.pack.clone())].into(),
        })
    }
    fn source_boundaries(f: &Fixture, claim: &foundry_ledger::ProductWorkClaim) {
        let db = f.f.root.0.join("source-control.db");
        private_copy(&f.f.db_path, &db);
        let mut store = ProductStore::open(&db, f.f.config.clone()).unwrap();
        store
            .validate_product_work_claim(claim, Utc::now())
            .unwrap();
        for (enabled, key) in [(false, "disable-source"), (true, "restore-source")] {
            let rev = reader(&db)
                .query_row(
                    "SELECT revision FROM product_sources_v1 WHERE connector_id='github-primary'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            store
                .configure_scoped_source(
                    &f.f.owner_session,
                    "github-primary",
                    foundry_ledger::RepositoryIdentity::canonicalized(
                        "acme",
                        "github",
                        "https://api.github.com",
                        "repo:1",
                    )
                    .unwrap(),
                    "acme",
                    "project",
                    "repo:1",
                    "open+foundry:make",
                    enabled,
                    rev,
                    key,
                    Utc::now(),
                )
                .unwrap();
            assert!(
                store
                    .validate_product_work_claim(claim, Utc::now())
                    .is_err()
            );
            assert!(
                store
                    .seal_code_change(
                        &f.f.service,
                        "service-revoke",
                        "service-producer",
                        &f.repo,
                        &f.head
                    )
                    .is_err()
            );
        }
    }

    fn authority_boundaries(f: &Fixture, finalized: &foundry_ledger::ProductResultInfo) {
        let mut store = f.f.store();
        let service = &f.f.service;
        f.approved("stale");
        let claim = store
            .reserve_code_change(service, "stale", "stale-producer")
            .unwrap();
        store
            .validate_product_work_claim(&claim, Utc::now())
            .unwrap();
        assert!(
            store
                .validate_product_work_claim(&claim, claim.lease_until())
                .is_err()
        );
        assert_stale_operations_refused(&mut at_deadline(f, &claim), f, "stale", "stale-producer");
        let copy = f.f.root.0.join("lease-control.db");
        private_copy(&f.f.db_path, &copy);
        let mut copy_store = ProductStore::open(&copy, f.f.config.clone()).unwrap();
        let now = Utc::now();
        copy_store.validate_product_work_claim(&claim, now).unwrap();
        let extended = claim.lease_until() + Duration::seconds(1);
        assert_eq!(
            Connection::open(&copy)
                .unwrap()
                .execute(
                    "UPDATE product_work_claims_v1 SET lease_until=?1 WHERE order_id='stale'",
                    [extended.to_rfc3339()],
                )
                .unwrap(),
            1
        );
        assert!(
            copy_store.validate_product_work_claim(&claim, now).is_err(),
            "a live but different lease passed the adapter check"
        );
        assert!(
            copy_store
                .seal_code_change(service, "stale", "stale-producer", &f.repo, &f.head)
                .is_err()
        );
        let next = store
            .claim_product_work_for_service(
                service,
                "reassigned",
                claim.lease_until() + Duration::seconds(1),
                Duration::minutes(1),
            )
            .unwrap();
        assert!(matches!(next, ProductWorkClaimResult::Claimed(ref c) if c.order_id()=="stale"));
        assert_stale_operations_refused(&mut store, f, "stale", "stale-producer");
        assert!(
            store
                .validate_product_work_claim(&claim, Utc::now())
                .is_err()
        );
        println!(
            "E03 exact live lease mismatch refused; expired and reassigned reserve/seal/collect refused with equal database snapshots"
        );
        f.approved("service-revoke");
        let worker = store
            .reserve_code_change(service, "service-revoke", "service-producer")
            .unwrap();
        store
            .validate_product_work_claim(&worker, Utc::now())
            .unwrap();
        let sr: i64 = reader(&f.f.db_path)
            .query_row(
                "SELECT revision FROM product_services_v1 WHERE service_id=?1",
                [SERVICE],
                |r| r.get(0),
            )
            .unwrap();
        let service_db = f.f.root.0.join("service-control.db");
        private_copy(&f.f.db_path, &service_db);
        let mut service_store = ProductStore::open(&service_db, f.f.config.clone()).unwrap();
        service_store
            .manage_product_service(
                &f.f.owner_session,
                SERVICE,
                &[],
                false,
                Utc::now() + Duration::hours(1),
                sr,
                "revoke-service",
                Utc::now(),
            )
            .unwrap();
        assert!(
            service_store
                .validate_product_work_claim(&worker, Utc::now())
                .is_err()
        );
        let grants = [
            ProductServiceAction::IntakeRead,
            ProductServiceAction::AdmitDelivery,
            ProductServiceAction::AckDelivery,
            ProductServiceAction::DriveReview,
        ]
        .map(|action| ProductServiceGrant {
            source_instance_id: SOURCE.into(),
            action,
        });
        service_store
            .manage_product_service(
                &f.f.owner_session,
                SERVICE,
                &grants,
                true,
                Utc::now() + Duration::hours(1),
                sr + 1,
                "restore-service",
                Utc::now(),
            )
            .unwrap();
        assert!(
            service_store
                .validate_product_work_claim(&worker, Utc::now())
                .is_err()
        );
        assert!(
            service_store
                .seal_code_change(
                    service,
                    "service-revoke",
                    "service-producer",
                    &f.repo,
                    &f.head
                )
                .is_err()
        );
        // Distinct human approver with a live scoped grant and genuine prepared reservation.
        let phc =
            foundry_ledger::product_identity::ProductPasswordKdf::hash("fixture-approval-password")
                .unwrap();
        store
            .create_account(&f.f.owner_session, "approver", "approver", &phc, Utc::now())
            .unwrap();
        scope(
            &f.f,
            "approver",
            ProductCapability::ApproveOrder,
            ScopeMutation::Grant,
            "grant-approve",
        );
        let actor = store
            .login("approver", "fixture-approval-password", Utc::now())
            .unwrap();
        scope(
            &f.f,
            "approver",
            ProductCapability::ReadArtifact,
            ScopeMutation::Grant,
            "read-result",
        );
        let token = sha(b"fixture-one-time-approval");
        store
            .issue_approval_grant(actor.digest(), "grant", &token, "good", Utc::now())
            .unwrap();
        let intent = ProductApprovalIntent {
            request_key: "approval".into(),
            grant_id: "grant".into(),
            token_digest: token,
            order_id: "good".into(),
            expected_source_revision: 1,
            expected_attempt: 1,
            run_id: 1,
            generation: 0,
            artifact_digest: f.head.clone(),
            tuple_digest: finalized.tuple_digest.clone(),
            intent: "approve local result".into(),
            now: Utc::now(),
        };
        let proof = store.prepare_approval(actor.digest(), &intent).unwrap();
        let good_copy = f.f.root.0.join("approval-positive.db");
        private_copy(&f.f.db_path, &good_copy);
        let receipt = ProductStore::open(&good_copy, f.f.config.clone())
            .unwrap()
            .approve_product(actor.digest(), &intent, &proof)
            .unwrap();
        assert_eq!(receipt.artifact_digest, f.head);
        let revoked = f.f.root.0.join("approval-service-revoked.db");
        private_copy(&f.f.db_path, &revoked);
        let mut revoked_store = ProductStore::open(&revoked, f.f.config.clone()).unwrap();
        revoked_store
            .manage_product_service(
                &f.f.owner_session,
                SERVICE,
                &[],
                false,
                Utc::now() + Duration::hours(1),
                sr,
                "revoke-before-approve",
                Utc::now(),
            )
            .unwrap();
        let before = snapshot(&revoked);
        assert!(
            revoked_store
                .approve_product(actor.digest(), &intent, &proof)
                .is_err()
        );
        assert!(before == snapshot(&revoked));
        let missing = f.f.root.0.join("missing-collection.db");
        private_copy(&f.f.db_path, &missing);
        Connection::open(&missing)
            .unwrap()
            .execute_batch("DROP TRIGGER product_producer_event_no_delete")
            .unwrap();
        Connection::open(&missing).unwrap().execute("DELETE FROM product_producer_events_v1 WHERE order_id='good' AND state='collected'",[]).unwrap();
        let prior = snapshot(&missing);
        assert!(
            ProductStore::open(&missing, f.f.config.clone())
                .unwrap()
                .approve_product(actor.digest(), &intent, &proof)
                .is_err()
        );
        assert!(prior == snapshot(&missing));
        source_boundaries(f, &worker);
        scope(
            &f.f,
            "owner",
            ProductCapability::AuthorizeTask,
            ScopeMutation::Revoke,
            "withdraw-task",
        );
        let before = snapshot(&f.f.db_path);
        capture(f, "withdrawn-before");
        assert!(
            store
                .approve_product(actor.digest(), &intent, &proof)
                .is_err()
        );
        assert!(before == snapshot(&f.f.db_path));
        capture(f, "withdrawn-after");
        scope(
            &f.f,
            "owner",
            ProductCapability::AuthorizeTask,
            ScopeMutation::Grant,
            "restore-task",
        );
        assert!(
            store
                .approve_product(actor.digest(), &intent, &proof)
                .is_err()
        );
        store
            .configure_product_artifact_root(f.f.artifact_root())
            .unwrap();
        assert_eq!(
            store
                .read_product_result(actor.digest(), "good", &finalized.result_id)
                .unwrap(),
            *finalized
        );
        for table in [
            "product_outcomes_v1",
            "product_token_consumptions_v1",
            "product_approval_result_bindings_v1",
        ] {
            assert_eq!(count(&f.f.db_path, table), 0);
        }
        assert_eq!(count(&f.f.db_path, "product_result_bindings_v1"), 1);
        println!(
            "E03 service revoke/ABA, lease expiry, reassignment and owner revoke/ABA rejected; retained finalized result=1 approval=0"
        );
    }
}

// W30b uses the accepted W2 fixture to exercise the producer's new read port.
// Only fixture Git and the ledger run; no external stage is launched.
#[test]
fn w30b_launcher_reads_exact_reserved_task() {
    let f = Fixture::new();
    f.approved("w3-input");
    let mut store = f.f.store();
    let service = &f.f.service;
    let claim = store
        .reserve_code_change(service, "w3-input", "w3-op")
        .unwrap();
    assert_eq!(
        store
            .reserved_code_change_task(service, &claim, "w3-op")
            .unwrap(),
        f.task()
    );
    assert!(
        store
            .reserved_code_change_task(service, &claim, "other-op")
            .is_err()
    );
    store
        .mark_code_change_uncertain(service, "w3-input", "w3-op")
        .unwrap();
    assert!(
        store
            .reserved_code_change_task(service, &claim, "w3-op")
            .is_err()
    );
}

#[test]
#[ignore = "host W31 witness or explicit fixture-only verification"]
fn w31_private_fixture() {
    let mut f = Fixture::new();
    // The frozen worker checks this tracked name before it can invoke Die.
    fs::write(
        f.repo.join("die-output.json"),
        b"deliberate pre-Die refusal\n",
    )
    .unwrap();
    git(&f.repo, &["add", "die-output.json"]);
    git(&f.repo, &["commit", "-qm", "W31 inert base"]);
    f.base = oid(&f.repo);
    let order = "w31-subject";
    f.approved("w31-other");
    // The last --issue-snapshot read by product-intake belongs to this order.
    f.approved(order);
    let root = &f.f.root.0;
    for name in ["artifacts", "sessions", "claims", ".foundry-work"] {
        fs::create_dir(root.join(name)).unwrap();
    }
    let other = root.join(".foundry-work/w31-other");
    let other_receipt =
        f.f.store()
            .code_change_for_producer(&f.f.service, "w31-other")
            .unwrap();
    git(
        &f.repo,
        &[
            "worktree",
            "add",
            "--detach",
            other.to_str().unwrap(),
            &other_receipt.base_commit,
        ],
    );
    assert_eq!(oid(&other), other_receipt.base_commit);
    assert_eq!(
        fs::read(root.join("input")).unwrap(),
        f.delivery(order, "first", &f.task()).payload.as_bytes()
    );
    let receipt =
        f.f.store()
            .code_change_for_producer(&f.f.service, order)
            .unwrap();
    let image = "sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60";
    let req = json!({"order": order, "lifecycle":receipt.lifecycle, "admission_hash":receipt.admission_hash,
        "connector":"github-primary","repository_id":"repo:1","api_origin":"https://api.github.com",
        "request_key":"w31-inert","operation":{"action":"produce","task_sha256":receipt.task_sha256,"image":image}});
    fs::write(root.join("request.json"), req.to_string()).unwrap();
    assert_eq!(
        git(&f.repo, &["show", &format!("{}:die-output.json", f.base)]),
        b"deliberate pre-Die refusal\n"
    );
    if std::env::var("W31_CASE").as_deref() == Ok("fixture-only") {
        return;
    }
    assert_eq!(std::env::var("FOUNDRY_W3_IMAGE").unwrap(), image);
    let case = std::env::var("W31_CASE").unwrap();
    assert!(matches!(case.as_str(), "default" | "timeout"));
    let mut test = Command::new(std::env::var_os("W31_UNIT_BINARY").unwrap());
    test.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("W31_FIXTURE", root)
        .env(
            "W31_WITNESS_DIR",
            std::env::var_os("W31_WITNESS_DIR").unwrap(),
        )
        .env("FOUNDRY_W3_IMAGE", image)
        .env("FOUNDRY_PRODUCT_REVIEW_SOURCE", SOURCE)
        .env("FOUNDRY_REPO_PATH", &f.repo)
        .env("FOUNDRY_REVIEW_PACK", &f.f.pack)
        .env("FOUNDRY_PRODUCT_ARTIFACT_ROOT", root.join("artifacts"))
        .env("FOUNDRY_PRODUCT_SESSION_ROOT", root.join("sessions"))
        .env("FOUNDRY_PRODUCT_CLAIM_ROOT", root.join("claims"));
    if case == "timeout" {
        test.env("FOUNDRY_DIE_TIMEOUT", "20");
    }
    assert!(
        test.args([
            "--exact",
            "isolation::dispatch::w31::owned_producer",
            "--ignored",
            "--nocapture"
        ])
        .status()
        .unwrap()
        .success()
    );
}
