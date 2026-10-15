//! StageExecutor adapter for the external Die CLI.
use crate::review_host::{StageExecutor, StageOutput, StageTask};
use foundry_domain::ports::Harness;
use foundry_domain::{ExternalRef, Order};
use foundry_harness_cli::{BaseSelection, CliHarness, DieProtocol, InputManifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug)]
pub struct DieStageExecutor {
    binary: PathBuf,
    profile: String,
    timeout_secs: u64,
    repo_path: PathBuf,
    session_root: PathBuf,
    restored: BTreeMap<String, RestoredSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    manifest: InputManifest,
    output_copy: PathBuf,
}

#[derive(Debug, Clone)]
struct RestoredSession {
    manifest: InputManifest,
    output_path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DieOutputFile {
    produced_commit: Option<String>,
    findings: bool,
    verdict_category: Option<String>,
    #[serde(default)]
    verdict_categories: Vec<String>,
    inspected_commit: Option<String>,
}

impl DieStageExecutor {
    pub fn new(
        binary: PathBuf,
        profile: String,
        timeout_secs: u64,
        repo_path: PathBuf,
        session_root: PathBuf,
    ) -> Self {
        Self {
            binary,
            profile,
            timeout_secs,
            repo_path,
            session_root,
            restored: BTreeMap::new(),
        }
    }

    fn protocol(&self, task: &StageTask) -> DieProtocol {
        DieProtocol {
            base: BaseSelection::Commit(
                task.fix_context
                    .as_deref()
                    .unwrap_or(&task.base_commit)
                    .to_owned(),
            ),
            fix_context: task.fix_context.is_some(),
            context: task
                .inputs
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        }
    }

    fn session_path(&self, session_id: &str) -> Result<PathBuf, String> {
        if session_id.is_empty()
            || !session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        {
            return Err("invalid Die session id".into());
        }
        Ok(self.session_root.join(format!("{session_id}.json")))
    }
}

impl StageExecutor for DieStageExecutor {
    fn ensure_available(&self) -> Result<(), String> {
        let metadata =
            fs::metadata(&self.binary).map_err(|e| format!("Die binary unavailable: {e}"))?;
        if !metadata.is_file() {
            return Err(format!(
                "Die binary is not a file: {}",
                self.binary.display()
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(format!(
                    "Die binary is not executable: {}",
                    self.binary.display()
                ));
            }
        }
        if !self.repo_path.exists() {
            return Err(format!(
                "Die repository is unavailable: {}",
                self.repo_path.display()
            ));
        }
        Ok(())
    }

    fn execute(&mut self, task: &StageTask) -> Result<StageOutput, String> {
        self.ensure_available()?;
        let record_path = self.session_path(&task.session_id)?;
        if record_path.exists() && !self.restored.contains_key(&task.session_id) {
            self.restore_session(&task.session_id, &task.workdir)?;
        }
        prepare_workdir(
            &self.repo_path,
            &task.workdir,
            &task.base_commit,
            &self.session_root,
        )?;
        let output_path = task.workdir.join("die-output.json");
        match fs::remove_file(&output_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("remove stale Die output: {e}")),
        }
        let mut protocol = self.protocol(task);
        if let Some(restored) = self.restored.remove(&task.session_id) {
            protocol
                .context
                .push(format!("previous_task={}", restored.manifest.task));
            protocol.context.push(format!(
                "previous_die_output={}",
                restored.output_path.display()
            ));
        }
        let expected_manifest = InputManifest {
            order_id: task.session_id.clone(),
            attempt: 0,
            task: format!("{}:{}", task.role, task.stage_id),
            profile: self.profile.clone(),
            base: match &protocol.base {
                BaseSelection::Commit(base) => base.clone(),
                _ => unreachable!("stage protocol uses an explicit commit"),
            },
            fix_context: protocol.fix_context,
            context: protocol.context.clone(),
        };
        let harness =
            CliHarness::new(&self.binary, &self.profile, self.timeout_secs).with_protocol(protocol);
        let external = ExternalRef::intake(
            &task.session_id,
            &format!("{}:{}", task.role, task.stage_id),
        );
        let order = Order::new(task.session_id.clone(), external, chrono::Utc::now());
        // Line and the HTTP handlers call this synchronous port inside Tokio.
        // Keep the local runtime on a plain thread so neither current-thread
        // nor multithread Tokio callers attempt to nest Runtime::block_on.
        let die = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| format!("create Die runtime: {e}"))?;
                    runtime
                        .block_on(harness.run(&order, 0, &task.workdir))
                        .map_err(|e| format!("Die execution failed: {e:?}"))
                })
                .join()
                .map_err(|_| "Die runtime thread panicked".to_owned())?
        })?;
        if die.exit_code != 0 {
            return Err(format!("Die exited with code {}", die.exit_code));
        }
        let bytes =
            fs::read(&output_path).map_err(|e| format!("read {}: {e}", output_path.display()))?;
        let output: DieOutputFile =
            serde_json::from_slice(&bytes).map_err(|e| format!("parse Die output: {e}"))?;
        if matches!(task.role.as_str(), "verify" | "lens") {
            let raw: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| format!("parse Die output: {e}"))?;
            if !raw
                .get("verdict_categories")
                .is_some_and(serde_json::Value::is_array)
            {
                return Err("verification Die output missing verdict_categories".into());
            }
            let head = git_text(&task.workdir, &["rev-parse", "HEAD^{commit}"])?;
            if head != task.artifact_commit
                || output
                    .inspected_commit
                    .as_ref()
                    .is_some_and(|commit| commit != &head)
            {
                return Err(
                    "verification HEAD does not match artifact_commit (BindingMismatch)".into(),
                );
            }
        }
        if matches!(task.role.as_str(), "verify" | "lens") && output.produced_commit.is_some() {
            return Err(format!("{} Die output included produced_commit", task.role));
        }
        if task.role != "verify" && task.role != "lens" {
            let commit = output
                .produced_commit
                .as_deref()
                .ok_or("implementation Die output missing produced_commit")?;
            verify_produced_commit(
                &task.workdir,
                commit,
                task.fix_context.as_deref().unwrap_or(&task.base_commit),
            )?;
        }
        let manifest_path = task.workdir.join("input-manifest.json");
        let manifest: InputManifest = serde_json::from_slice(
            &fs::read(&manifest_path).map_err(|e| format!("read input manifest: {e}"))?,
        )
        .map_err(|e| format!("parse input manifest: {e}"))?;
        if manifest != expected_manifest {
            return Err("Die modified the input manifest (BindingMismatch)".into());
        }
        if let Some(commit) = output.produced_commit.as_deref() {
            cache_commit(&self.session_root, &task.workdir, commit)?;
        }
        save_session(&self.session_root, &record_path, &manifest, &output_path)?;
        Ok(StageOutput {
            verdict_category: output.verdict_category,
            findings: output.findings,
            // Seal the structured result, which downstream lenses consume.
            // A successful Die may be silent; its process log stays alongside
            // this file as attempt-0.log for execution diagnostics.
            log_path: output_path.to_string_lossy().into_owned(),
            inspected_commit: output.inspected_commit,
            verdict_categories: output.verdict_categories,
            produced_commit: output.produced_commit,
        })
    }

    fn restore_session(&mut self, session_id: &str, workdir: &Path) -> Result<(), String> {
        let path = self.session_path(session_id)?;
        let record: SessionRecord =
            serde_json::from_slice(&fs::read(&path).map_err(|e| format!("read Die session: {e}"))?)
                .map_err(|e| format!("parse Die session: {e}"))?;
        let session_dir = workdir.join(".die-session");
        fs::create_dir_all(&session_dir).map_err(|e| format!("create restore directory: {e}"))?;
        let restored_output = session_dir.join("die-output.json");
        fs::copy(&record.output_copy, &restored_output)
            .map_err(|e| format!("restore Die output: {e}"))?;
        fs::copy(&path, session_dir.join(format!("{session_id}.json")))
            .map_err(|e| format!("restore Die session record: {e}"))?;
        self.restored.insert(
            session_id.to_owned(),
            RestoredSession {
                manifest: record.manifest,
                output_path: restored_output,
            },
        );
        Ok(())
    }
}

fn prepare_workdir(
    repo: &Path,
    workdir: &Path,
    base: &str,
    session_root: &Path,
) -> Result<(), String> {
    if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("base_commit is not a full git SHA (BindingMismatch)".into());
    }
    fs::create_dir_all(workdir).map_err(|e| format!("create workdir: {e}"))?;
    for entry in fs::read_dir(workdir).map_err(|e| format!("read workdir: {e}"))? {
        if entry
            .map_err(|e| format!("read workdir entry: {e}"))?
            .file_name()
            != ".die-session"
        {
            return Err("Die workdir must be empty except for a restored session".into());
        }
    }
    let repo = fs::canonicalize(repo).map_err(|e| format!("resolve repository: {e}"))?;
    let session_dir = workdir.join(".die-session");
    let session_stash = workdir.with_extension(format!("die-session-stash-{}", std::process::id()));
    if session_dir.exists() {
        fs::rename(&session_dir, &session_stash)
            .map_err(|e| format!("stage restored session: {e}"))?;
    }
    let clone = Command::new("git")
        .args(["clone", "--no-checkout"])
        .arg(&repo)
        .arg(workdir)
        .output()
        .map_err(|e| {
            if session_stash.exists() {
                let _ = fs::rename(&session_stash, &session_dir);
            }
            format!("git clone: {e}")
        })?;
    if !clone.status.success() {
        if session_stash.exists() {
            let _ = fs::create_dir_all(workdir);
            let _ = fs::rename(&session_stash, &session_dir);
        }
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        ));
    }
    let cache = session_root.join(".commits.git");
    if cache.exists() {
        let _lock = cache_lock(session_root)?;
        // Generated commits live in the shared object store rather than the
        // source repository. Fetch explicitly before checking out a new head.
        if git_text(workdir, &["cat-file", "-e", &format!("{base}^{{commit}}")]).is_err() {
            let fetched = Command::new("git")
                .arg("-C")
                .arg(workdir)
                .args(["fetch", "--no-tags"])
                .arg(fs::canonicalize(&cache).map_err(|e| e.to_string())?)
                .arg(base)
                .output()
                .map_err(|e| format!("fetch generated base: {e}"))?;
            if !fetched.status.success() {
                return Err(format!(
                    "fetch generated base: {}",
                    String::from_utf8_lossy(&fetched.stderr)
                ));
            }
        }
    }
    let checkout = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["checkout", "--detach", base])
        .output()
        .map_err(|e| format!("git checkout: {e}"))?;
    if !checkout.status.success() {
        if session_stash.exists() {
            let _ = fs::create_dir_all(workdir);
            let _ = fs::rename(&session_stash, &session_dir);
        }
        return Err(format!(
            "git base checkout failed: {}",
            String::from_utf8_lossy(&checkout.stderr)
        ));
    }
    if session_stash.exists() {
        fs::rename(&session_stash, &session_dir)
            .map_err(|e| format!("restore session staging: {e}"))?;
    }
    let head = git_text(workdir, &["rev-parse", "HEAD^{commit}"])?;
    if head != base {
        return Err("checked out base does not match StageTask base_commit".into());
    }
    Ok(())
}

fn cache_lock(root: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(root).map_err(|e| format!("create session root: {e}"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".commits.lock"))
        .map_err(|e| format!("open commit cache lock: {e}"))?;
    lock.lock().map_err(|e| format!("lock commit cache: {e}"))?;
    Ok(lock)
}

fn cache_commit(root: &Path, workdir: &Path, commit: &str) -> Result<(), String> {
    let _lock = cache_lock(root)?;
    let cache = root.join(".commits.git");
    if !cache.exists() {
        let init = Command::new("git")
            .args(["init", "--bare"])
            .arg(&cache)
            .output()
            .map_err(|e| format!("create commit cache: {e}"))?;
        if !init.status.success() {
            return Err("create commit cache failed".into());
        }
    }
    let fetched = Command::new("git")
        .arg("-C")
        .arg(&cache)
        .args(["fetch", "--no-tags"])
        .arg(fs::canonicalize(workdir).map_err(|e| e.to_string())?)
        .arg(format!("{commit}:refs/heads/{commit}"))
        .output()
        .map_err(|e| format!("save generated commit: {e}"))?;
    if !fetched.status.success() {
        return Err(format!(
            "save generated commit: {}",
            String::from_utf8_lossy(&fetched.stderr)
        ));
    }
    Ok(())
}

fn verify_produced_commit(workdir: &Path, commit: &str, base: &str) -> Result<(), String> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("produced_commit is not a full git SHA".into());
    }
    let actual = git_text(
        workdir,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )?;
    if actual != commit {
        return Err("produced_commit does not resolve to the claimed commit".into());
    }
    let head = git_text(workdir, &["rev-parse", "HEAD^{commit}"])?;
    if head != commit {
        return Err("produced_commit is not the workdir HEAD".into());
    }
    // Read the object's real headers, rather than a revision walk affected by
    // worker-written replacement refs, grafts, or shallow-boundary metadata.
    let object = git_text(workdir, &["cat-file", "-p", commit])?;
    let parents: Vec<_> = object
        .lines()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("parent "))
        .collect();
    if parents.as_slice() != [base] {
        return Err("produced_commit parent does not match base (BindingMismatch)".into());
    }
    Ok(())
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn save_session(
    root: &Path,
    record_path: &Path,
    manifest: &InputManifest,
    output_path: &Path,
) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("create session root: {e}"))?;
    let bytes = fs::read(output_path).map_err(|e| format!("read session output: {e}"))?;
    let outputs = root.join("outputs");
    fs::create_dir_all(&outputs).map_err(|e| format!("create session outputs: {e}"))?;
    let output_copy = fs::canonicalize(&outputs)
        .map_err(|e| e.to_string())?
        .join(format!("{:x}.json", Sha256::digest(&bytes)));
    fs::write(&output_copy, bytes).map_err(|e| format!("save session output: {e}"))?;
    let record = SessionRecord {
        manifest: manifest.clone(),
        output_copy,
    };
    let bytes = serde_json::to_vec(&record).map_err(|e| format!("serialize session: {e}"))?;
    fs::write(record_path, bytes).map_err(|e| format!("write session: {e}"))
}
