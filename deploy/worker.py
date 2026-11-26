"""Image entrypoint. Each phase receives a fresh /work tmpfs and a bounded frame."""
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys

LIMIT = 16 << 20
GIT = "/usr/bin/git"


def oid(value):
    if not isinstance(value, str) or not re.fullmatch(r"[a-f0-9]{40}", value):
        raise ValueError("invalid commit")
    return value


def git(*args, data=None):
    return subprocess.run([GIT, "--no-replace-objects", "-c", "core.hooksPath=/dev/null", *args],
                          input=data or b"", stdout=subprocess.PIPE, check=True).stdout


def import_objects(data):
    pack = base64.b64decode(data, validate=True)
    if len(pack) > LIMIT:
        raise ValueError("object limit")
    git("init", "-q", "--template=")
    git("index-pack", "--stdin", "--strict", data=pack)


def export(head):
    pack = git("pack-objects", "--stdout", "--revs", data=(oid(head) + "\n").encode())
    if len(pack) > LIMIT:
        raise ValueError("object limit")
    return base64.b64encode(pack).decode()


def perform(job):
    base = oid(job["base"])
    import_objects(job["pack"])
    phase = job["phase"]
    if phase in ("checks", "collect"):
        head = oid(job["head"])
        if git("rev-list", "--parents", "-1", head).decode().split() != [head, base]:
            raise ValueError("commit parent mismatch")
        flags = ["--no-ext-diff", "--no-textconv", "--no-renames"]
        if phase == "checks":
            if job.get("allowed_paths") is not None:
                changed = git("diff-tree", "--no-renames", "--no-commit-id", "--name-only", "-z", "-r", base, head, "--")
                names = changed.decode().split("\0")
                if names[-1] or len(names) < 2 or not set(names[:-1]).issubset(job["allowed_paths"]):
                    raise ValueError("unapproved changed path")
            git("diff", *flags, "--check", base, head, "--")
            return {"checked": head}
        patch = git("diff", *flags, "--binary", "--full-index", base, head, "--")
        if not patch or len(patch) > 4 << 20:
            raise ValueError("artifact limit")
        return {"head": head, "patch": base64.b64encode(patch).decode()}
    if phase != "stage":
        raise ValueError("unknown phase")
    git("checkout", "-q", "--detach", base)
    manifest = job["manifest"]
    stage = job["stage"]
    # Staging files are created exclusively, including on trees with symlinks.
    for path, value in job["inputs"].items():
        if not re.fullmatch(r"input-[0-9]+", path):
            raise ValueError("unknown input")
        with open(path, "xb") as target:
            target.write(bytes(value))
    if job.get("previous") is not None:
        session = Path(".die-session")
        session.mkdir(mode=0o700)
        with (session / "die-output.json").open("x") as saved:
            json.dump(job["previous"]["output"], saved)
    if job.get("task") is not None:
        with open("code-change-task.json", "x") as task_file:
            task_file.write(job["task"])
    for path, value in [("stage-input.json", stage), ("input-manifest.json", manifest)]:
        with open(path, "x") as target:
            json.dump(value, target)
    if os.path.lexists("die-output.json"):
        raise ValueError("preexisting result")
    env = dict(os.environ, FOUNDRY_ORDER_ID=manifest["order_id"], FOUNDRY_ATTEMPT="0",
               FOUNDRY_MODEL_CLIENT="/opt/foundry/model-client", FOUNDRY_PROXY_ADDR="/run/foundry-model.sock",
               FOUNDRY_PROXY_TOKEN=job["token"], FOUNDRY_PROXY_RUN=job["run"],
               FOUNDRY_PROXY_PROVIDER=job["provider"],
               FOUNDRY_PROXY_BUDGET_SCOPE=json.dumps(job["budget_scope"]))
    args = ["/opt/foundry/die", "--profile", manifest["profile"], "--task", manifest["task"]]
    if manifest["fix_context"]:
        args.append("--fix-context")
    subprocess.run([*args, "--base", base], env=env, check=True, stdin=subprocess.DEVNULL,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with open("die-output.json", "rb") as result:
        raw = result.read((1 << 20) + 1)
    if len(raw) > 1 << 20 or json.loads(Path("input-manifest.json").read_bytes()) != manifest or json.loads(Path("stage-input.json").read_bytes()) != stage:
        raise ValueError("stage result or input rejected")
    output = json.loads(raw)
    head = oid(git("rev-parse", "HEAD^{commit}").decode().strip())
    if stage["role"] in ("verify", "lens"):
        if head != base or output.get("inspected_commit") != base or output.get("produced_commit") is not None or not isinstance(output.get("verdict_categories"), list):
            raise ValueError("inspection binding")
        return {"output": output}
    if output.get("produced_commit") != head:
        raise ValueError("production binding")
    return {"output": output, "pack": export(head)}


def main():
    os.environ.clear()
    os.environ.update(PATH="/usr/bin:/bin", HOME="/work", GIT_CONFIG_NOSYSTEM="1",
                      GIT_CONFIG_GLOBAL="/dev/null", GIT_ALLOW_PROTOCOL="", GIT_NO_LAZY_FETCH="1")
    raw = sys.stdin.buffer.read((32 << 20) + 1)
    if len(raw) > 32 << 20:
        raise ValueError("request limit")
    print(json.dumps(perform(json.loads(raw))))


if __name__ == "__main__":
    try:
        main()
    except Exception:
        sys.exit("stage rejected")
