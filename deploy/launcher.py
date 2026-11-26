# Embedded after profile.py in the foundry binary. All Docker invocations use
# the local Unix socket and a clean client environment.
import base64
import hashlib
import io
import tarfile
import os
import selectors
import subprocess
import sys
import threading
import time

DOCKER = ["/usr/bin/docker", "--host", "unix:///var/run/docker.sock"]
PYTHON_IMAGE_ENV = ("LANG", "GPG_KEY", "PYTHON_VERSION", "PYTHON_SHA256")
ENV = {"PATH": "/usr/bin:/bin", "HOME": "/nonexistent", "GIT_CONFIG_NOSYSTEM": "1",
       "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_NO_LAZY_FETCH": "1",
       "GIT_NO_REPLACE_OBJECTS": "1", "GIT_ALLOW_PROTOCOL": ""}


def feed_stdin(pipe, data):
    try:
        with pipe:
            pipe.write(data)
    except BrokenPipeError:
        pass


def command(argv, until, data=b"", limit=1 << 20):
    # Drain stdout concurrently with stdin; never buffer arbitrary child logs.
    if time.monotonic() >= until:
        raise TimeoutError("command deadline")
    with subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, env=ENV) as proc:
        try:
            writer = threading.Thread(target=feed_stdin, args=(proc.stdin, data))
            writer.start()
            out = bytearray()
            with selectors.DefaultSelector() as selector:
                selector.register(proc.stdout, selectors.EVENT_READ)
                while selector.select(max(0, until - time.monotonic())):
                    chunk = os.read(proc.stdout.fileno(), 65536)
                    if not chunk:
                        break
                    out.extend(chunk)
                    if time.monotonic() >= until:
                        raise TimeoutError("command deadline")
                    if len(out) > limit:
                        raise ValueError("command limit")
                else:
                    raise TimeoutError()
            if proc.wait(timeout=max(0, until - time.monotonic())):
                raise ValueError("command failed")
            return bytes(out)
        finally:
            proc.kill()
            proc.wait()
            writer.join()


def fingerprint(info, service, cid):
    hc, cfg = info["HostConfig"], info["Config"]
    mounts = [{"type": m["Type"], "source": m["Source"], "target": m["Destination"],
               "rw": m["RW"], "propagation": m["Propagation"]} for m in info["Mounts"]]
    expected = service["volumes"][0]
    if (info["Id"] != cid or info["Image"] != service["image"] or cfg["User"] != service["user"]
        or not cfg["OpenStdin"] or cfg["Tty"]
        or cfg["WorkingDir"] != "/work" or cfg["Entrypoint"] != ENTRY[:1] or cfg["Cmd"] != ENTRY[1:]
        or hc["ReadonlyRootfs"] is not True or hc["Privileged"] or hc["CapAdd"]
        or hc["CapDrop"] != ["ALL"] or hc["SecurityOpt"] != service["security_opt"]
        or hc["NetworkMode"] != "none" or hc["PidMode"] or hc["IpcMode"] != "private" or hc["LogConfig"]["Type"] != "none"
        or hc["Devices"] or hc.get("DeviceRequests") or hc["VolumesFrom"]
        or hc["Tmpfs"] != {"/work": TMPFS.split(":", 1)[1]}
        or hc["PidsLimit"] != 64 or hc["Memory"] != 512 << 20
        or cfg.get("Healthcheck", {}).get("Test") != ["NONE"]
        or any(not v.startswith("PATH=") and v not in PYTHON_IMAGE_ENV for v in cfg["Env"] or [])
        or mounts != [{"type": "bind", "source": expected["source"], "target": expected["target"],
                       "rw": False, "propagation": "rprivate"}]):
        raise ValueError("container differs from render")
    return {"container": cid, "image": info["Image"], "user": cfg["User"], "mounts": mounts,
            "tmpfs": hc["Tmpfs"], "cap_drop": hc["CapDrop"], "security_opt": hc["SecurityOpt"],
            "readonly": hc["ReadonlyRootfs"], "network": hc["NetworkMode"]}


def verify_image_files(cid, expected, until):
    for name, digest in expected.items():
        archive = command(DOCKER + ["cp", cid + ":/opt/foundry/" + name, "-"], until, limit=128 << 10)
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as tar:
            members = tar.getmembers()
            if len(members) != 1 or not members[0].isreg() or members[0].name != name:
                raise ValueError("image entrypoint file type mismatch")
            if hashlib.sha256(tar.extractfile(members[0]).read()).hexdigest() != digest:
                raise ValueError("image entrypoint content mismatch")


def launch(service, job, root, until, run, image_files):
    socket = service["volumes"][0]["source"]
    args = ["create", "--pull=never", "--interactive", "--read-only", "--network=none",
            "--user", service["user"], "--cap-drop=ALL", "--security-opt=no-new-privileges:true",
            "--no-healthcheck", "--ipc=private", "--log-driver=none", "--pids-limit=64", "--memory=512m", "--workdir=/work",
            "--tmpfs", TMPFS, "--mount", f"type=bind,source={socket},target=/run/foundry-model.sock,readonly",
            "--entrypoint", ENTRY[0], service["image"], *ENTRY[1:]]
    # Unset official Python image metadata in the clean client environment.
    # Docker's bare --env NAME drops an inherited value when NAME is unset.
    args[1:1] = [arg for key in PYTHON_IMAGE_ENV
                 for arg in ("--env", key)]
    # A random ownership label + name allow cleanup after a lost create reply.
    owner = os.urandom(24).hex()
    args[1:1] = ["--label", "foundry.w3.owner=" + owner, "--name", "foundry-w3-" + owner]
    cid = None
    record = {"phase": job["phase"], "run": run, "owner": owner,
              "render_sha256": hashlib.sha256(json.dumps(service, sort_keys=True).encode()).hexdigest()}
    path = root / ("runtime-" + job["phase"] + ".json")
    path.write_text(json.dumps(record))
    try:
        raw = command(DOCKER + args, until).decode().strip()
        if not re.fullmatch("[0-9a-f]{64}", raw):
            raise ValueError("invalid container ID")
        info = json.loads(command(DOCKER + ["container", "inspect", raw], until))[0]
        if (info["Config"]["Labels"].get("foundry.w3.owner") != owner or info["Id"] != raw
                or info["Name"] != "/foundry-w3-" + owner):
            raise ValueError("ownership mismatch")
        cid = raw
        record["container"] = cid
        path.write_text(json.dumps(record))
        record.update(fingerprint(info, service, cid))
        verify_image_files(cid, image_files, until)
        record["image_files_sha256"] = image_files
        path.write_text(json.dumps(record))
        record["start_requested"] = True
        path.write_text(json.dumps(record))
        result = command(DOCKER + ["start", "--attach", "--interactive", cid], until,
                         json.dumps(job).encode(), 32 << 20)
        status = json.loads(command(DOCKER + ["inspect", "--format", "{{json .State}}", cid], until))
        record["exit_state"] = {k: status[k] for k in ("Status", "ExitCode", "OOMKilled")}
        if status["Status"] != "exited" or status["ExitCode"] != 0 or status["OOMKilled"]:
            raise ValueError("worker did not complete")
        return json.loads(result)
    except (TimeoutError, subprocess.TimeoutExpired):
        record["deadline_expired"] = time.monotonic() >= until
        raise
    finally:
        deadline = time.monotonic() + 10
        if cid is None:
            # Only resolve the unique name allocated above; never list containers.
            try:
                info = json.loads(command(DOCKER + ["inspect", "foundry-w3-" + owner], deadline))[0]
                if (info["Config"]["Labels"].get("foundry.w3.owner") == owner
                        and info["Name"] == "/foundry-w3-" + owner
                        and re.fullmatch("[0-9a-f]{64}", info["Id"])):
                    cid = info["Id"]
            except (ValueError, TimeoutError, subprocess.TimeoutExpired):
                record["ownership_lookup"] = "unconfirmed"
        if cid:
            command(DOCKER + ["rm", "--force", "--volumes", cid], deadline)
            record["removed"] = cid
        path.write_text(json.dumps(record))


def dispatch(spec):
    root = Path(spec["workdir"])
    remaining = spec["timeout"]
    if spec.get("lease_end_ms") is not None:
        remaining = min(remaining, spec["lease_end_ms"] / 1000 - time.time())
    until = time.monotonic() + remaining
    if remaining <= 0:
        raise TimeoutError("expired lease")
    profile = render(spec["profile"], spec["image"], spec["socket"])["services"]["cast"]
    (root / "render.json").write_text(json.dumps(profile, sort_keys=True))
    (root / "binding.json").write_text(json.dumps({
        "intent": spec["intent"], "run": spec["run"], "stage": spec["stage"]["stage_id"],
        "role": spec["stage"]["role"], "provider": spec["provider"], "budget_scope": spec["budget_scope"],
        "calls": 0, "max_cost_microusd": 0, "timeout_secs": spec["timeout"]
    }, sort_keys=True))
    base = spec["stage"]["artifact_commit"]
    if not re.fullmatch("[0-9a-f]{40}", base) or spec["manifest"]["base"] != base:
        raise ValueError("invalid base")
    # Scope generated object packs to this product order. Never run host Git
    # against a writable child repository.
    cache = Path(spec["cache"]) / hashlib.sha256(spec["order"].encode()).hexdigest()
    cache.mkdir(parents=True, exist_ok=True, mode=0o700)
    cached = cache / base
    if cached.exists():
        pack = cached.read_bytes()
    else:
        pack = command(["/usr/bin/git", "-C", spec["repo"], "pack-objects", "--stdout", "--revs"],
                       until, (base + "\n").encode(), 16 << 20)
    common = {"run": spec["run"], "base": base}
    result = launch(profile, dict(common, phase="stage", pack=base64.b64encode(pack).decode(),
                                  stage=spec["stage"], manifest=spec["manifest"], inputs=spec["inputs"], task=spec["task"], previous=spec["previous"],
                                  provider=spec["provider"], budget_scope=spec["budget_scope"],
                                  token=spec["token"]), root, until, spec["run"], spec["image_files"])
    output = result["output"]
    if spec["stage"]["role"] not in ("verify", "lens"):
        head = output["produced_commit"]
        if not re.fullmatch("[0-9a-f]{40}", head):
            raise ValueError("invalid produced commit")
        returned = base64.b64decode(result["pack"], validate=True)
        if len(returned) > 16 << 20:
            raise ValueError("object limit")
        payload = dict(common, head=head, pack=result["pack"])
        approved = json.loads(spec["task"]) if spec["task"] is not None else None
        check_until = min(until, time.monotonic() + approved["checks"][0]["timeout_secs"]) if approved else until
        paths = approved["allowed_paths"] if approved else None
        checked = launch(profile, dict(payload, phase="checks", allowed_paths=paths), root, check_until, spec["run"], spec["image_files"])
        collected = launch(profile, dict(payload, phase="collect"), root, until, spec["run"], spec["image_files"])
        if checked != {"checked": head} or collected["head"] != head:
            raise ValueError("collector binding")
        patch = base64.b64decode(collected["patch"], validate=True)
        if not patch or len(patch) > 4 << 20:
            raise ValueError("invalid collected patch")
        (root / "collected.patch").write_bytes(patch)
        if spec["task"] is not None:
            db = str(root / "objects.git")
            command(["/usr/bin/git", "init", "--bare", "--template=", db], until)
            command(["/usr/bin/git", "-C", db, "index-pack", "--stdin", "--strict"], until, returned)
        # Publish complete bytes atomically. No mutable child ever sees cache.
        temp = cache / os.urandom(24).hex()
        temp.write_bytes(returned)
        temp.replace(cache / head)
    elif output.get("inspected_commit") != base or output.get("produced_commit") is not None:
        raise ValueError("inspector binding")
    (root / "input-manifest.json").write_text(json.dumps(spec["manifest"]))
    print(json.dumps(output))


if __name__ == "__main__":
    try:
        dispatch(json.load(sys.stdin))
    except Exception as error:
        sys.exit("isolated launcher rejected " + type(error).__name__)
