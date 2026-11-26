//! Product pack stages cross this port only with a durable host intent and journal run.
#[cfg(feature = "product-pack-pins")]
pub(crate) mod dispatch;
mod listener;

use foundry_exec::{ExecutionIntent, ReviewInputManifest, ReviewRedriveArtifact};
use foundry_line::{StageExecutor, StageOutput, StageTask};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LAUNCHER: &str = concat!(
    include_str!("../../../deploy/profile.py"),
    "\n",
    include_str!("../../../deploy/launcher.py")
);
const PROFILE: &str = include_str!("../../../deploy/compose.yaml");

pub(super) struct ProductLauncher {
    image: String,
    repo: PathBuf,
    artifacts: PathBuf,
    cache: PathBuf,
    timeout: Duration,
    inspection_only: bool,
    producer_task: Option<String>,
    deadline: Option<chrono::DateTime<chrono::Utc>>,
}

fn reject() -> String {
    "product isolation configuration or evidence rejected".into()
}

impl ProductLauncher {
    pub fn configured(
        artifacts: &Path,
        sessions: &Path,
        inspection_only: bool,
    ) -> Result<Self, String> {
        let image = std::env::var("FOUNDRY_W3_IMAGE").map_err(|_| reject())?;
        if !image.strip_prefix("sha256:").is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(reject());
        }
        let repo = PathBuf::from(std::env::var_os("FOUNDRY_REPO_PATH").ok_or_else(reject)?)
            .canonicalize()
            .map_err(|_| reject())?;
        Ok(Self {
            image,
            repo,
            artifacts: artifacts.to_owned(),
            cache: sessions.join("w3-objects"),
            timeout: Duration::from_secs(foundry_line::DieStageExecutor::configured_timeout()?),
            inspection_only,
            producer_task: None,
            deadline: None,
        })
    }

    fn inputs(
        &self,
        task: &StageTask,
    ) -> Result<(BTreeMap<String, String>, BTreeMap<String, Vec<u8>>), String> {
        let root = self.artifacts.canonicalize().map_err(|_| reject())?;
        let mut files = BTreeMap::new();
        let mut total = 0;
        let mut materialize = |name: &str| -> Result<String, String> {
            let path = Path::new(name).canonicalize().map_err(|_| reject())?;
            if !path.starts_with(&root)
                || path.file_name().and_then(|s| s.to_str()) != Some("accepted-output")
            {
                return Err(reject());
            }
            let mut bytes = Vec::new();
            fs::File::open(path)
                .and_then(|f| f.take(1_048_577).read_to_end(&mut bytes))
                .map_err(|_| reject())?;
            total += bytes.len();
            if bytes.len() > 1_048_576 || total > 2_097_152 {
                return Err(reject());
            }
            let local = format!("input-{}", files.len());
            files.insert(local.clone(), bytes);
            Ok(format!("/work/{local}"))
        };
        let mut inputs = BTreeMap::new();
        for (key, val) in &task.inputs {
            let mapped = if task.fix_context.is_some() {
                let mut rows: Vec<ReviewRedriveArtifact> =
                    serde_json::from_str(val).map_err(|_| reject())?;
                for row in &mut rows {
                    row.log_path = materialize(&row.log_path)?;
                }
                serde_json::to_string(&rows).map_err(|_| reject())?
            } else {
                materialize(val)?
            };
            inputs.insert(key.clone(), mapped);
        }
        Ok((inputs, files))
    }
}

#[derive(Serialize, Deserialize)]
struct StageSession {
    manifest: ReviewInputManifest,
    output: Value,
}

impl ProductLauncher {
    fn session_file(&self, order: &str, id: &str) -> PathBuf {
        let key =
            Sha256::digest(serde_json::to_vec(&(&self.image, order, id)).expect("string tuple"));
        self.cache.join("sessions").join(hex::encode(key))
    }

    fn previous(&self, order: &str, id: &str) -> Result<Option<StageSession>, String> {
        match fs::read(self.session_file(order, id)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| reject()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(reject()),
        }
    }

    fn save_session(
        &self,
        order: &str,
        id: &str,
        manifest: ReviewInputManifest,
        bytes: &[u8],
    ) -> Result<(), String> {
        let dst = self.session_file(order, id);
        let parent = dst.parent().ok_or_else(reject)?;
        fs::create_dir_all(parent).map_err(|_| reject())?;
        let mut nonce = [0; 16];
        OsRng.try_fill_bytes(&mut nonce).map_err(|_| reject())?;
        let tmp = parent.join(hex::encode(nonce));
        let mut file = fs::File::create_new(&tmp).map_err(|_| reject())?;
        let output = serde_json::from_slice(bytes).map_err(|_| reject())?;
        serde_json::to_writer(&mut file, &StageSession { manifest, output })
            .map_err(|_| reject())?;
        file.sync_all()
            .and_then(|_| fs::rename(&tmp, dst))
            .map_err(|_| reject())
    }
}

struct SocketDir(PathBuf);
impl SocketDir {
    fn new() -> Result<Self, String> {
        let mut random = [0; 16];
        OsRng.try_fill_bytes(&mut random).map_err(|_| reject())?;
        let path = PathBuf::from("/tmp").join(format!("foundry-w3-{}", hex::encode(random)));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| reject())?;
        Ok(Self(path))
    }
}
impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DieOutput {
    findings: bool,
    verdict_category: Option<String>,
    #[serde(default)]
    verdict_categories: Vec<String>,
    inspected_commit: Option<String>,
    produced_commit: Option<String>,
}

impl StageExecutor for ProductLauncher {
    fn ensure_available(&self) -> Result<(), String> {
        let status = Command::new("/usr/bin/timeout")
            .args([
                "5",
                "/usr/bin/docker",
                "--host",
                "unix:///var/run/docker.sock",
                "image",
                "inspect",
                "--format",
                "{{.Id}}",
            ])
            .arg(&self.image)
            .env_clear()
            .env("HOME", "/nonexistent")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .map_err(|_| reject())?;
        if !status.status.success() || status.stdout != format!("{}\n", self.image).as_bytes() {
            return Err(reject());
        }
        Ok(())
    }

    fn restore_session_for_order(
        &mut self,
        order: &str,
        id: &str,
        _workdir: &Path,
    ) -> Result<(), String> {
        self.previous(order, id)?.ok_or_else(reject)?;
        Ok(())
    }

    fn execute(&mut self, _: &StageTask) -> Result<StageOutput, String> {
        Err("product execution requires a bound journal run".into())
    }

    fn execute_in_run(
        &mut self,
        intent: &ExecutionIntent,
        journal: &str,
        task: &StageTask,
    ) -> Result<StageOutput, String> {
        let inspect = matches!(task.role.as_str(), "verify" | "lens");
        if journal.is_empty()
            || (!inspect
                && (self.inspection_only
                    || !matches!(
                        task.role.as_str(),
                        "producer" | "implement" | "fix" | "replan"
                    )))
        {
            return Err(reject());
        }
        self.ensure_available()?;
        let timeout = match self.deadline {
            Some(deadline) => self.timeout.min(
                (deadline - chrono::Utc::now())
                    .to_std()
                    .map_err(|_| reject())?,
            ),
            None => self.timeout,
        };
        if timeout.as_secs() == 0 {
            return Err(reject());
        }
        let (inputs, files) = self.inputs(task)?;
        let scope = vec![
            intent.order.0.clone(),
            intent.epoch.0.to_string(),
            journal.to_owned(),
        ];
        let dir = SocketDir::new()?;
        let listener = listener::Listener::offline(
            dir.0.join("model.sock"),
            journal,
            &scope,
            Instant::now() + timeout,
        )
        .map_err(|_| reject())?;
        let previous = self.previous(&intent.order.0, &task.session_id)?;
        let mut manifest = ReviewInputManifest {
            order_id: task.session_id.clone(),
            attempt: 0,
            task: format!("{}:{}", task.role, task.stage_id),
            profile: "coding".into(),
            base: task.base_commit.clone(),
            fix_context: task.fix_context.is_some(),
            context: inputs.iter().map(|(k, v)| format!("{k}={v}")).collect(),
        };
        if let Some(saved) = &previous {
            manifest
                .context
                .push(format!("previous_task={}", saved.manifest.task));
            manifest
                .context
                .push("previous_die_output=/work/.die-session/die-output.json".into());
        }
        let image_files = json!({
            "worker.py":hex::encode(Sha256::digest(include_bytes!("../../../deploy/worker.py"))),
            "model-client":hex::encode(Sha256::digest(include_bytes!("../../../deploy/model-client.py"))),
        });
        let spec = json!({"image_files":image_files,"workdir":task.workdir,"cache":self.cache,"repo":self.repo,"image":self.image,
            "profile":serde_json::from_str::<Value>(PROFILE).map_err(|_|reject())?, "socket":listener.path,
            "intent":intent,"run":journal,"order":intent.order.0,"provider":"none","budget_scope":scope,"token":listener.token,
            "timeout":timeout.as_secs(),"lease_end_ms":self.deadline.map(|t|t.timestamp_millis()),"task":self.producer_task,"inputs":files, "manifest":manifest,"previous":previous,
            "stage":{"version":1,"role":task.role,"stage_id":task.stage_id,"artifact_commit":task.artifact_commit,
            "lens_bytes":task.lens_bytes,"inputs":inputs}});
        let mut child = Command::new("/usr/bin/python3")
            .args(["-I", "-c", LAUNCHER])
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| reject())?;
        let sent = serde_json::to_writer(child.stdin.take().ok_or_else(reject)?, &spec);
        let output = child.wait_with_output().map_err(|_| reject())?;
        if sent.is_err() || !output.status.success() || output.stdout.len() > 1_048_576 {
            return Err(reject());
        }
        let result: DieOutput = serde_json::from_slice(&output.stdout).map_err(|_| reject())?;
        let path = task.workdir.join("die-output.json");
        fs::File::create_new(&path)
            .and_then(|mut f| f.write_all(&output.stdout))
            .map_err(|_| reject())?;
        self.save_session(&intent.order.0, &task.session_id, manifest, &output.stdout)?;
        Ok(StageOutput {
            log_path: path.to_string_lossy().into_owned(),
            findings: result.findings,
            verdict_category: result.verdict_category,
            verdict_categories: result.verdict_categories,
            produced_commit: result.produced_commit,
            inspected_commit: result.inspected_commit,
        })
    }
}
