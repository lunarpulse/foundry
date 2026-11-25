//! Controller-owned read-only comparison of immutable Git objects.
use super::{CodeChangeTask, ProductError, denied, full_hex};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn git(repo: &Path, args: &[&str], until: Instant) -> Result<Vec<u8>, ProductError> {
    if Instant::now() >= until {
        return Err(denied());
    }
    let mut child = Command::new("/usr/bin/git")
        .current_dir(repo)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ALLOW_PROTOCOL", "")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| denied())?;
    let mut stdout = child.stdout.take().ok_or_else(denied)?;
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = output.join().map_err(|_| denied())?.map_err(|_| denied())?;
    if !status.is_some_and(|s| s.success()) || bytes.len() > 4 * 1024 * 1024 {
        return Err(denied());
    }
    Ok(bytes)
}

pub(super) fn verify(
    repo: &Path,
    t: &CodeChangeTask,
    head: &str,
    remaining: Duration,
) -> Result<Vec<u8>, ProductError> {
    if !full_hex(head, 40) {
        return Err(denied());
    }
    let until =
        Instant::now() + remaining.min(Duration::from_secs(t.checks[0].timeout_secs.into()));
    let parents = git(repo, &["rev-list", "--parents", "-n", "1", head], until)?;
    if parents != format!("{} {}\n", head, t.base_commit).as_bytes() {
        return Err(denied());
    }
    let paths = git(
        repo,
        &[
            "diff-tree",
            "--no-renames",
            "--no-commit-id",
            "--name-only",
            "-z",
            "-r",
            &t.base_commit,
            head,
            "--",
        ],
        until,
    )?;
    let files = paths.strip_suffix(&[0]).ok_or_else(denied)?;
    if files.is_empty()
        || files
            .split(|b| *b == 0)
            .any(|p| !t.allowed_paths.iter().any(|a| a.as_bytes() == p))
    {
        return Err(denied());
    }
    git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--check",
            &t.base_commit,
            head,
            "--",
        ],
        until,
    )?;
    git(
        repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "--binary",
            "--full-index",
            &t.base_commit,
            head,
            "--",
        ],
        until,
    )
}
