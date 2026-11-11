//! Feature-only fixed-task adapter. Diagnostics never include external bytes.
use super::a01_coordinator::Producer;
use foundry_ledger::{ProductWorkClaim, product::*};
use foundry_line::{DieStageExecutor, StageExecutor, StageTask};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const PROOF: &str = ".foundry/a01-proof.txt";

pub(crate) struct RealGitProducer {
    executor: DieStageExecutor,
    repo: PathBuf,
    root: PathBuf,
    base: String,
    pub(crate) invocations: usize,
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/local/bin")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(args)
        .output()
        .map_err(|_| "A01 Git unavailable")?;
    if !output.status.success() {
        return Err("A01 Git rejected".into());
    }
    Ok(output.stdout)
}
pub(crate) fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git(dir, args)?)
        .map(|s| s.trim().to_owned())
        .map_err(|_| "A01 Git encoding rejected".into())
}
fn file(path: &Path) -> Result<Vec<u8>, String> {
    let m = fs::symlink_metadata(path).map_err(|_| "A01 evidence missing")?;
    if !m.file_type().is_file() {
        return Err("A01 evidence type rejected".into());
    }
    fs::read(path).map_err(|_| "A01 evidence unreadable".into())
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

impl RealGitProducer {
    pub(crate) fn new(
        binary: PathBuf,
        timeout: u64,
        repo: PathBuf,
        root: PathBuf,
        base: &str,
    ) -> Result<Self, String> {
        let repo = repo.canonicalize().map_err(|_| "A01 repository missing")?;
        let resolved = git_text(
            &repo,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )?;
        if !matches!(resolved.len(), 40 | 64)
            || !resolved
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("A01 base rejected".into());
        }
        if !git(&repo, &["ls-tree", &resolved, "--", PROOF])?.is_empty() {
            return Err("A01 proof already exists at base".into());
        }
        let executor = DieStageExecutor::new(
            binary,
            "coding".into(),
            timeout,
            repo.clone(),
            root.join("sessions"),
        );
        Ok(Self {
            executor,
            repo,
            root,
            base: resolved,
            invocations: 0,
        })
    }
    pub(crate) fn base(&self) -> &str {
        &self.base
    }
    pub(crate) fn stage_task(&self, r: &A01ProducerReservation) -> Result<StageTask, String> {
        let manifest = a01_real_manifest(r, &self.base).map_err(|_| "A01 task mapping rejected")?;
        Ok(StageTask {
            role: "producer".into(),
            stage_id: "a01-fixed-v1".into(),
            session_id: manifest.order_id.clone(),
            base_commit: self.base.clone(),
            artifact_commit: self.base.clone(),
            fix_context: None,
            lens_bytes: Vec::new(),
            inputs: a01_real_inputs(r).map_err(|_| "A01 task inputs rejected")?,
            workdir: self.root.join("producer").join(&manifest.order_id),
        })
    }
    fn verify_result(&self, task: &StageTask, result: &str) -> Result<(), String> {
        if git_text(&task.workdir, &["rev-parse", "--verify", "HEAD^{commit}"])? != result {
            return Err("A01 result HEAD mismatch".into());
        }
        let object = git_text(&task.workdir, &["cat-file", "-p", result])?;
        let parents: Vec<_> = object
            .lines()
            .take_while(|s| !s.is_empty())
            .filter_map(|s| s.strip_prefix("parent "))
            .collect();
        if parents.as_slice() != [self.base.as_str()] {
            return Err("A01 result parent mismatch".into());
        }
        if !git(&task.workdir, &["ls-tree", &self.base, "--", PROOF])?.is_empty()
            || git(
                &task.workdir,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--name-status",
                    "--no-renames",
                    "-r",
                    &self.base,
                    result,
                    "--",
                ],
            )? != b"A\t.foundry/a01-proof.txt\n"
        {
            return Err("A01 result changes rejected".into());
        }
        let entry = git_text(&task.workdir, &["ls-tree", result, "--", PROOF])?;
        if !entry.starts_with("100644 blob ")
            || !entry.ends_with("\t.foundry/a01-proof.txt")
            || git(&task.workdir, &["show", &format!("{result}:{PROOF}")])?
                != b"foundry-a01-fixed-task-v1\n"
            || file(&task.workdir.join(PROOF))? != b"foundry-a01-fixed-task-v1\n"
        {
            return Err("A01 proof content or mode rejected".into());
        }
        git(&task.workdir, &["diff", "--exit-code", "HEAD", "--"])?;
        git(
            &task.workdir,
            &["diff", "--cached", "--exit-code", "HEAD", "--"],
        )?;
        Ok(())
    }

    fn prepare_cache(&self) -> Result<(), String> {
        // The existing executor shares this cache with review. Initialize its
        // object format from the actual source before its first generated fetch.
        let format = git_text(&self.repo, &["rev-parse", "--show-object-format"])?;
        if !matches!(format.as_str(), "sha1" | "sha256") {
            return Err("A01 repository object format rejected".into());
        }
        let sessions = self.root.join("sessions");
        fs::create_dir_all(&sessions).map_err(|_| "A01 cache directory failed")?;
        if sessions
            .canonicalize()
            .map_err(|_| "A01 session root missing")?
            != sessions
        {
            return Err("A01 session root changed".into());
        }
        let cache = sessions.join(".commits.git");
        if !cache.exists() {
            let path = cache.to_str().ok_or("A01 cache path rejected")?;
            git(
                &sessions,
                &["init", "--bare", &format!("--object-format={format}"), path],
            )?;
        }
        if git_text(&cache, &["rev-parse", "--show-object-format"])? != format {
            return Err("A01 cache object format mismatch".into());
        }
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRecord {
    manifest: A01RealManifest,
    output_copy: PathBuf,
}

impl Producer for RealGitProducer {
    fn ensure_available(&self) -> Result<(), String> {
        self.executor
            .ensure_available()
            .map_err(|_| "A01 Die unavailable".into())
    }
    fn produce(
        &mut self,
        task_bytes: &[u8],
        claim: &ProductWorkClaim,
        r: &A01ProducerReservation,
    ) -> Result<String, String> {
        let binding = claim.review_binding();
        if self.invocations != 0
            || task_bytes != r.task.task_bytes
            || claim.order_id() != r.claim.order_id
            || claim.lifecycle() != r.claim.lifecycle
            || claim.delivery_id() != r.claim.delivery_id
            || claim.task_kind() != r.task.admission.raw_kind
            || claim.lease_until().timestamp_nanos_opt() != Some(r.claim.lease_until_ns)
            || binding.worker_id != r.claim.worker_id
            || binding.source_revision != r.claim.source_revision
            || binding.payload_digest != r.claim.payload_digest
            || claim.review_binding().fence != r.claim.fence
            || claim.review_binding().epoch != r.claim.epoch
        {
            return Err("A01 producer input or replay rejected".into());
        }
        let task = self.stage_task(r)?;
        let manifest = a01_real_manifest(r, &self.base).map_err(|_| "A01 manifest rejected")?;
        if task.workdir.exists()
            || self
                .root
                .join("sessions")
                .join(format!("{}.json", task.session_id))
                .exists()
        {
            return Err("A01 prior execution evidence exists".into());
        }
        self.prepare_cache()?;
        self.invocations += 1;
        let output = self
            .executor
            .execute(&task)
            .map_err(|_| "A01 external execution failed")?;
        if task
            .workdir
            .canonicalize()
            .map_err(|_| "A01 workdir missing")?
            != task.workdir
        {
            return Err("A01 workdir identity changed".into());
        }
        let result = output
            .produced_commit
            .ok_or("A01 produced commit missing")?;
        self.verify_result(&task, &result)?;
        let manifest_bytes = file(&task.workdir.join("input-manifest.json"))?;
        if manifest_bytes
            != serde_json::to_vec(&manifest).map_err(|_| "A01 manifest encoding failed")?
        {
            return Err("A01 manifest bytes changed".into());
        }
        let die = file(&task.workdir.join("die-output.json"))?;
        let log = file(&task.workdir.join("attempt-0.log"))?;
        let sessions = self.root.join("sessions");
        if sessions
            .canonicalize()
            .map_err(|_| "A01 session root missing")?
            != sessions
        {
            return Err("A01 session root changed".into());
        }
        let record_bytes = file(&sessions.join(format!("{}.json", task.session_id)))?;
        let record: SessionRecord =
            serde_json::from_slice(&record_bytes).map_err(|_| "A01 session record rejected")?;
        let outputs = sessions.join("outputs");
        if outputs
            .canonicalize()
            .map_err(|_| "A01 retained outputs missing")?
            != outputs
        {
            return Err("A01 retained output root changed".into());
        }
        let expected_copy = outputs.join(format!("{}.json", sha(&die)));
        if record.manifest != manifest
            || record.output_copy != expected_copy
            || file(&expected_copy)? != die
            || serde_json::to_vec(&record).map_err(|_| "A01 session encoding failed")?
                != record_bytes
        {
            return Err("A01 retained output mismatch".into());
        }
        let value = A01RealOutput {
            version: 1,
            kind: "real-git".into(),
            task_digest: r.task.task_digest.clone(),
            task_receipt_sha256: a01_real_task_receipt_sha256(&r.task)
                .map_err(|_| "A01 task binding failed")?,
            reservation_sha256: a01_real_reservation_sha256(r)
                .map_err(|_| "A01 reservation binding failed")?,
            base_commit: self.base.clone(),
            result_commit: result,
            manifest,
            evidence: A01RealEvidence {
                manifest_sha256: sha(&manifest_bytes),
                die_output_sha256: sha(&die),
                process_log_sha256: sha(&log),
                session_record_sha256: sha(&record_bytes),
                exit_code: 0,
            },
        };
        encode_a01_real_output(r, &value).map_err(|_| "A01 output encoding rejected".into())
    }
}
