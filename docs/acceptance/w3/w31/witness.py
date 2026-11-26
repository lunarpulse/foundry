"""Test-binary instrumentation. Run the embedded production dispatch, pacing only its stdin."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
PIN = "sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60"
WRITER = """import os,time
with open('/work/w31-heartbeat','xb', buffering=0) as f:
    i = 0
    while True:
        line = ('%d\\n' % i).encode()
        f.write(line)
        os.write(1,line)
        i += 1
        time.sleep(0.1)
"""


def digest(data):
    return hashlib.sha256(data).hexdigest()


def save(path, value):
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def lines(path, rows):
    path.parent.mkdir(exist_ok=True)
    path.write_text("".join(json.dumps(row, sort_keys=True) + "\n" for row in rows))


def snapshot(paths, db):
    files = {}
    for name, value in paths.items():
        p = Path(value)
        mode = p.stat().st_mode
        files[name] = {"path": value, "mode": mode, "inode": p.stat().st_ino}
        if stat.S_ISREG(mode):
            files[name]["sha256"] = digest(p.read_bytes())
    # Hash the complete fixture SQL dump without persisting any credential rows.
    return {"files": files, "db_dump_sha256": digest("\n".join(db.iterdump()).encode())}


def docker(ns, args, **kwargs):
    return subprocess.run(ns["DOCKER"] + args, env=ns["ENV"], capture_output=True,
                          timeout=4, **kwargs)


def read_state(ns, cid):
    r = docker(ns, ["container", "inspect", "--format", "{{json .State}}", cid], check=True)
    s = json.loads(r.stdout)
    return {k: s[k] for k in ("Status", "Running", "ExitCode", "OOMKilled")}


def host_control(path):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(2)
        s.connect(str(path))
        s.sendall(b"GET /api/product/sources HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        return s.makefile("rb").readline().decode().strip()


def inspect_case(spec, ns, out, root, source):
    stage = Path(spec["workdir"])
    case = out.name
    assert case in ("default", "timeout") and spec["image"] == PIN
    assert spec["stage"]["role"] == "producer" and spec["provider"] == "none"
    # Validate the pre-Die guard against the actual immutable task base.
    guard = subprocess.check_output(["/usr/bin/git", "-C", spec["repo"], "show",
                                     spec["stage"]["artifact_commit"] + ":die-output.json"], env=ns["ENV"])
    assert guard == b"deliberate pre-Die refusal\n"
    db = sqlite3.connect((root / "product.sqlite").as_uri() + "?mode=ro", uri=True)
    wal = sqlite3.connect(root / "sidecars.sqlite")
    assert wal.execute("pragma journal_mode=wal").fetchone() == ("wal",)
    wal.execute("create table canary(value text)")
    wal.execute("insert into canary values ('w31')")
    wal.commit()
    epoch, fence, state = db.execute(
        "select epoch,fence,state from product_producers_v1 where order_id=? and operation_key=?",
        (spec["order"], spec["run"])).fetchone()
    assert state == "reserved" and spec["intent"]["epoch"] == fence
    paths = {k: str(root / v) for k, v in {"db": "product.sqlite", "wal_db": "sidecars.sqlite",
             "wal": "sidecars.sqlite-wal", "shm": "sidecars.sqlite-shm", "owner": "owner",
             "service": "service", "watch": "input", "other": ".foundry-work/w31-other/README",
             "serve": "serve.sock"}.items()}
    paths.update(home=str(root), management="/var/run/docker.sock")
    state = {}
    command = ns["command"]
    render = ns["render"]
    writer = None
    errors = []
    host_status = host_control(paths["serve"])
    assert " 401 " in host_status

    def row(**values):
        return {"join": state["join"], **values}

    def feed(pipe, data):
        # PID 1 is the unmodified worker waiting for EOF on the normal stage pipe.
        nonlocal writer
        with pipe:
            try:
                cid = state["join"]["container"]
                limit = state["until"]
                while not read_state(ns, cid)["Running"]:
                    if time.monotonic() >= limit - 2:
                        raise TimeoutError("worker did not become observable")
                    time.sleep(.05)
                with closing(db_reader()) as reader:
                    before = snapshot(paths, reader)
                save(out / "I02/controller-before.json", row(**before))
                r = docker(ns, ["exec", "-i", cid, "/usr/bin/python3", "-I", "-c",
                                (HERE / "child.py").read_text()], input=json.dumps(paths).encode(), check=True)
                child = json.loads(r.stdout)
                save(out / "I06/proc-self.json", row(worker=child["worker"], observer=child["observer"]))
                lines(out / "I02/child-open.jsonl", [row(surface=k, **a) for k, v in child["opens"].items()
                                                     if k not in ("watch", "other") for a in v])
                lines(out / "I03/workspaces.jsonl", [row(surface="own", **child["own"])] +
                      [row(surface=k, **a) for k in ("watch", "other") for a in child["opens"][k]])
                lines(out / "I03/serve-audit.jsonl", [row(host_control=host_status, route="/api/product/sources",
                      transport="fixture AF_UNIX", child_connect=child["serve"])])
                with closing(db_reader()) as reader:
                    after = snapshot(paths, reader)
                save(out / "I02/controller-after.json", row(**after))
                save(out / "I03/canaries.json", row(**{k: {"before": before["files"][k]["sha256"],
                     "after": after["files"][k]["sha256"]} for k in ("watch", "other")}))
                if case == "timeout":
                    with (out / "I06/heartbeat").open("xb") as output:
                        writer = subprocess.Popen(ns["DOCKER"] + ["exec", cid, "/usr/bin/python3", "-I", "-u", "-c", WRITER],
                            stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.DEVNULL, env=ns["ENV"])
                    # Record growth while the owned writer is live, before the deadline.
                    heartbeat = out / "I06/heartbeat"
                    while heartbeat.stat().st_size == 0 and time.monotonic() < limit - 1:
                        time.sleep(.05)
                    time.sleep(max(0, limit - .95 - time.monotonic()))
                    active = []
                    for _ in range(2):
                        b = heartbeat.read_bytes()
                        active.append(row(monotonic_ns=time.monotonic_ns(), bytes=len(b), sha256=digest(b)))
                        time.sleep(.35)
                    lines(out / "I06/active-hashes.jsonl", active)
                    # Keep the frame incomplete through the real command deadline.
                    time.sleep(max(0, limit + .25 - time.monotonic()))
                else:
                    pipe.write(data)
            except Exception as e:
                errors.append(e)

    def db_reader():
        # sqlite connections belong to their creating thread.
        return sqlite3.connect((root / "product.sqlite").as_uri() + "?mode=ro", uri=True)

    def observed(argv, until, *args, **kwargs):
        op = argv[len(ns["DOCKER"]):] if argv[:len(ns["DOCKER"])] == ns["DOCKER"] else []
        if op:
            # Stage input is private. Only the Docker argv goes into the audit.
            with (out / "docker-argv.jsonl").open("a") as log:
                print(json.dumps({"argv":argv, "monotonic_ns":time.monotonic_ns()}), file=log)
        if op[:2] == ["container", "inspect"]:
            raw = command(argv, until, *args, **kwargs)
            info = json.loads(raw)[0]
            receipt = json.loads((stage / "runtime-stage.json").read_bytes())
            cid = op[-1]
            assert re.fullmatch("[a-f0-9]{64}", cid) and info["Id"] == cid
            assert info["Name"] == "/foundry-w3-" + receipt["owner"]
            assert info["Config"]["Labels"]["foundry.w3.owner"] == receipt["owner"]
            state["join"] = {"source_sha256": source, "render_sha256":digest((stage / "render.json").read_bytes()),
                "image":info["Image"], "order":spec["order"], "epoch":epoch, "fence":fence,
                "run":spec["run"], "stage":spec["stage"]["stage_id"], "container":cid}
            cfg = info["Config"]
            hc = info["HostConfig"]
            save(out / "I06/runtime.json", row(id=info["Id"], image=info["Image"], name=info["Name"], owner=receipt["owner"],
                config={k: cfg.get(k) for k in ("User", "WorkingDir", "Entrypoint", "Cmd", "OpenStdin", "Tty", "Healthcheck")},
                env_keys=[v.split("=", 1)[0] for v in cfg.get("Env") or [] if "=" in v],
                unset_env=[v for v in cfg.get("Env") or [] if "=" not in v],
                host_config={k:hc.get(k) for k in ("Binds","Tmpfs","CapAdd","CapDrop","SecurityOpt","ReadonlyRootfs",
                    "Privileged","NetworkMode","PidMode","IpcMode","Devices","DeviceRequests","VolumesFrom","LogConfig",
                    "Memory","PidsLimit")}, mounts=info["Mounts"]))
            return raw
        if op[:1] == ["start"]:
            assert op == ["start", "--attach", "--interactive", state["join"]["container"]]
            state["until"] = until
            state["start_ns"] = time.monotonic_ns()
            normal_feed = ns["feed_stdin"]
            ns["feed_stdin"] = feed
            try:
                return command(argv, until, *args, **kwargs)
            finally:
                ns["feed_stdin"] = normal_feed
        if op[:1] == ["rm"] and "join" in state:
            assert op == ["rm", "--force", "--volumes", state["join"]["container"]]
            # An unsuccessful supplementary observation must never prevent cleanup.
            try:
                state["pre_rm"] = read_state(ns, op[-1])
                if writer:
                    state["pre_rm_bytes"] = (out / "I06/heartbeat").stat().st_size
                    state["writer_active"] = writer.poll() is None
            except Exception as e:
                errors.append(e)
        return command(argv, until, *args, **kwargs)

    ns["command"] = observed
    try:
        ns["dispatch"](spec)
    except (ValueError, TimeoutError, subprocess.TimeoutExpired) as e:
        state["exception"] = type(e).__name__
    finally:
        for name in ("binding.json", "render.json", "runtime-stage.json"):
            if (stage / name).is_file():
                shutil.copyfile(stage / name, out / name)
        db.close()
        wal.close()
        if writer:
            try:
                writer.wait(timeout=3)
            except subprocess.TimeoutExpired:
                writer.kill()  # local docker exec client only
                writer.wait()
                errors.append(RuntimeError("owned output stream survived removal"))
    receipt = json.loads((out / "runtime-stage.json").read_bytes())
    cid = state["join"]["container"]
    assert receipt.get("removed") == cid
    gone = docker(ns, ["container", "inspect", cid])
    absent = gone.returncode == 1 and re.search(rb"no such (object|container)", gone.stderr.lower()) is not None
    save(out / "I06/reap.json", row(writer_exit=writer.returncode if writer else None, removed=receipt["removed"], pre_rm=state.get("pre_rm"),
         deadline_monotonic_ns=int(state["until"] * 1e9) if "until" in state else None,
         start_monotonic_ns=state.get("start_ns"),
         observed_reap_ns=time.monotonic_ns(), deadline_expired=receipt.get("deadline_expired", False), error=state.get("exception"),
         pre_rm_bytes=state.get("pre_rm_bytes"), writer_active=state.get("writer_active"),
         absent=absent, inspect_exit=gone.returncode))
    if writer:
        observations = []
        for delay in (.25, 1):
            time.sleep(delay)
            b = (out / "I06/heartbeat").read_bytes()
            observations.append(row(monotonic_ns=time.monotonic_ns(), bytes=len(b), sha256=digest(b)))
        lines(out / "I06/output-hashes.jsonl", observations)
    if errors:
        raise errors[0]
    assert absent and state.get("exception") and receipt.get("start_requested")
    service = render(spec["profile"], PIN, spec["socket"])["services"]["cast"]
    assert service == json.loads((out / "render.json").read_bytes())
    config = json.loads((out / "controller-config.json").read_bytes())
    lines(out / "I01/overrides.jsonl", [row(case=case, repo=spec["repo"], workdir=str(stage),
          sessions=str(Path(spec["cache"]).parent), claims=str(root / "claims"),
          artifact_root=str(root / "artifacts"),
          timeout_setting=config["timeout_secs"], timeout_effective=spec["timeout"],
          lease_end_ms=spec["lease_end_ms"], socket=spec["socket"], start_requested=receipt.get("start_requested"))])
    save(out / "observed.json", row(case=case))
    # The parent Rust controller must record this deliberately failed stage as uncertain.
    raise SystemExit(1)


if __name__ == "__main__":
    out, root = map(Path, sys.argv[1:3])
    ns = {"__name__": "w31_embedded_launcher"}
    exec(compile(sys.argv[3], "embedded-product-launcher", "exec"), ns)
    manifest = (HERE / "source.sha256").read_bytes()
    inspect_case(json.load(sys.stdin), ns, out, root, digest(manifest))
