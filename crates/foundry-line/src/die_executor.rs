//! StageExecutor adapter for the external Die CLI.
use crate::review_host::{StageExecutor, StageOutput, StageTask};
use foundry_domain::ports::Harness;
use foundry_domain::{ExternalRef, Order};
use foundry_harness_cli::{BaseSelection, CliHarness, DieProtocol, InputManifest};
use serde::{Deserialize, Serialize};
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
        prepare_workdir(&self.repo_path, &task.workdir, &task.base_commit)?;
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
        let harness =
            CliHarness::new(&self.binary, &self.profile, self.timeout_secs).with_protocol(protocol);
        let external = ExternalRef::intake(
            &task.session_id,
            &format!("{}:{}", task.role, task.stage_id),
        );
        let order = Order::new(task.session_id.clone(), external, chrono::Utc::now());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("create Die runtime: {e}"))?;
        let die = runtime
            .block_on(harness.run(&order, 0, &task.workdir))
            .map_err(|e| format!("Die execution failed: {e:?}"))?;
        if die.exit_code != 0 {
            return Err(format!("Die exited with code {}", die.exit_code));
        }
        let output_path = task.workdir.join("die-output.json");
        let bytes =
            fs::read(&output_path).map_err(|e| format!("read {}: {e}", output_path.display()))?;
        let output: DieOutputFile =
            serde_json::from_slice(&bytes).map_err(|e| format!("parse Die output: {e}"))?;
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
        save_session(
            &self.session_root,
            &self.session_path(&task.session_id)?,
            &manifest,
            &output_path,
        )?;
        Ok(StageOutput {
            verdict_category: output.verdict_category,
            findings: output.findings,
            log_path: die.log_path,
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

fn prepare_workdir(repo: &Path, workdir: &Path, base: &str) -> Result<(), String> {
    fs::create_dir_all(workdir).map_err(|e| format!("create workdir: {e}"))?;
    let session_dir = workdir.join(".die-session");
    let session_stash = workdir.with_extension(format!("die-session-stash-{}", std::process::id()));
    if session_dir.exists() {
        fs::rename(&session_dir, &session_stash)
            .map_err(|e| format!("stage restored session: {e}"))?;
    }
    let clone = Command::new("git")
        .args(["clone", "--no-checkout"])
        .arg(repo)
        .arg(workdir)
        .output()
        .map_err(|e| {
            if session_stash.exists() {
                let _ = fs::rename(&session_stash, &session_dir);
            }
            format!("git clone: {e}")
        })?;
    if !clone.status.success() {
        let _ = fs::remove_dir_all(workdir);
        if session_stash.exists() {
            let _ = fs::create_dir_all(workdir);
            let _ = fs::rename(&session_stash, &session_dir);
        }
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        ));
    }
    let checkout = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["checkout", "--detach", base])
        .output()
        .map_err(|e| format!("git checkout: {e}"))?;
    if !checkout.status.success() {
        let _ = fs::remove_dir_all(workdir);
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
    let parents = git_text(workdir, &["rev-list", "--parents", "-n", "1", commit])?;
    let fields: Vec<_> = parents.split_whitespace().collect();
    if fields.len() != 2 || fields[1] != base {
        return Err("produced_commit parent does not match base (BindingMismatch)".into());
    }
    Ok(())
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
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
    let output_copy = root.join(format!("{}.output.json", manifest.order_id));
    fs::copy(output_path, &output_copy).map_err(|e| format!("save session output: {e}"))?;
    let record = SessionRecord {
        manifest: manifest.clone(),
        output_copy,
    };
    let bytes = serde_json::to_vec(&record).map_err(|e| format!("serialize session: {e}"))?;
    fs::write(record_path, bytes).map_err(|e| format!("write session: {e}"))
}
