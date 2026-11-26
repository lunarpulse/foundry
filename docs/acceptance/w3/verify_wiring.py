"""W30b offline component checks. Docker and Die are never invoked."""
import base64
import copy
import hashlib
import io
import tarfile
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.dont_write_bytecode = True
ns = {"__name__": "offline_components"}
for file in ("profile.py", "launcher.py"):
    exec(compile((ROOT / "deploy" / file).read_bytes(), file, "exec"), ns)
actual_launch = ns["launch"]
profile = json.loads((ROOT / "deploy/compose.yaml").read_bytes())
# This is a *test operand*, the hash of known bytes, not a Docker image.
image = "sha256:" + hashlib.sha256(b"W30b render fixture; no image").hexdigest()
sock = "/tmp/w3-render-fixture/model.sock"
rendered = ns["render"](profile, image, sock)
cli = subprocess.check_output([sys.executable, ROOT/"scripts/check-deploy.py", "--image", image, "--socket", sock])
assert rendered == json.loads(cli)
(HERE / "render.json").write_bytes(cli)
print("render input = sha256 of UTF-8 'W30b render fixture; no image'; socket is a placeholder")
for field, value in {"user": "0", "privileged": True, "cap_add": ["ALL"], "network_mode": "host",
                     "read_only": False, "volumes": [], "entrypoint": ["sh"], "environment": {"HOME": "/root"},
                     "security_opt": [], "pull_policy": "always", "tmpfs": [], "pid": "host"}.items():
    modified = copy.deepcopy(profile)
    modified["services"]["cast"][field] = value
    try:
        ns["render"](modified, image, sock)
    except ValueError:
        continue
    raise AssertionError(field)
print("12 profile mutations rejected")
for bad in ("latest", "sha256:a", "sha256:"+"A"*64):
    try:
        ns["render"](profile, bad, sock)
    except ValueError:
        continue
    raise AssertionError(bad)
print("3 invalid image operands rejected")
env = dict(ns["ENV"], GIT_AUTHOR_NAME="fixture", GIT_COMMITTER_NAME="fixture",
           GIT_AUTHOR_EMAIL="fixture@localhost", GIT_COMMITTER_EMAIL="fixture@localhost",
           GIT_AUTHOR_DATE="2020-01-01T00:00:00Z", GIT_COMMITTER_DATE="2020-01-01T00:00:00Z")


def git(repo, *args, data=b""):
    return subprocess.run(["/usr/bin/git", "-C", repo, *args], input=data,
                          env=env, stdout=subprocess.PIPE, check=True).stdout


def worker(job):
    with tempfile.TemporaryDirectory(dir=tmp) as workdir:
        return subprocess.run([sys.executable, "-I", ROOT/"deploy/worker.py"], cwd=workdir,
                              input=json.dumps(job).encode(), env=env, capture_output=True)


with tempfile.TemporaryDirectory(prefix="w3-inert-") as tmp:
    repo = Path(tmp)/"repo"
    repo.mkdir()
    git(repo, "init", "-q", "--template=")
    for content in (b"base\n", b"base\naccepted\n"):
        (repo / "README").write_bytes(content)
        git(repo, "add", "README")
        git(repo, "commit", "-qm", "inert fixture")
    base, head = [git(repo, "rev-parse", ref).decode().strip() for ref in ("HEAD^", "HEAD")]
    pack = base64.b64encode(git(repo, "pack-objects", "--stdout", "--revs", data=(head+"\n").encode())).decode()
    expected = git(repo, "diff", "--binary", "--full-index", base, head, "--")
    calls = []

    def child(_service, job, _root, _until, run, _image_files):
        assert run == "fixture-journal"
        calls.append(job["phase"])
        if job["phase"] == "stage":
            assert git(repo, "rev-parse", base).decode().strip() == job["base"]
            role = job["stage"]["role"]
            if role in ("verify", "lens"):
                return {"output": {"inspected_commit":base, "findings":False, "verdict_categories":[]}}
            return {"output":{"produced_commit":head,"findings":False}, "pack":pack}
        result = worker(job)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    ns["launch"] = child
    for role in ("producer", "fix", "replan", "verify", "lens"):
        out = Path(tmp)/role
        out.mkdir()
        spec = {"image_files":{}, "profile":profile,"image":image,"socket":sock,"run":"fixture-journal","order":"fixture-order",
                "repo":repo,"cache":Path(tmp)/"cache","timeout":10,"workdir":out,
                "previous":None,"intent":{"order":"fixture-order","epoch":7},"manifest":{"base":base},"stage":{"stage_id":"inert","role":role,"artifact_commit":base},"inputs":{},
                "task":json.dumps({"allowed_paths":["README"],"checks":[{"timeout_secs":2}]}) if role=="producer" else None,
                "provider":"none","budget_scope":["fixture"],"token":"fixture-only"}
        calls.clear()
        ns["dispatch"](spec)
        if role in ("verify","lens"):
            assert calls==["stage"]
        else:
            assert calls==["stage","checks","collect"]
            assert (out/"collected.patch").read_bytes()==expected
        print(f"role={role} controller phases={calls}; stage is a recording double")
    assert git(Path(tmp)/"producer/objects.git","rev-parse",head+"^{commit}").decode().strip()==head
    for allowed, parent in [(["wrong"],base),(["README"],head)]:
        invalid={"phase":"checks","base":parent,"head":head,"pack":pack,"allowed_paths":allowed}
        assert worker(invalid).returncode!=0
    print(f"real Git worker check+collect: {base}..{head}, patch={len(expected)} bytes, sha256={hashlib.sha256(expected).hexdigest()}")
    print("wrong parent and disallowed path refused; no product container or model observation")

# Exercise create/inspect/start/remove ordering, without storing simulated
# inspect data as a runtime observation.
def verify_ownership(case, launch):
    service = rendered["services"]["cast"]
    cid = "a"*64
    commands = []
    owner = None

    def docker(argv, _deadline, *_rest, **_bounds):
        nonlocal owner
        prefix = ns["DOCKER"]
        assert argv[:len(prefix)] == prefix
        args = argv[len(prefix):]
        op = args[0]
        commands.append(op)
        if op == "create":
            assert "--pull=never" in args and args[-3:] == [image,"-I","/opt/foundry/worker.py"]
            assert args[args.index("--mount")+1] == "type=bind,source="+sock+",target=/run/foundry-model.sock,readonly"
            owner = args[args.index("--label")+1].split("=")[1]
            if case == "lost-create":
                raise ValueError("lost create reply")
            return cid.encode()
        if op in ("container","inspect") and "--format" not in args:
            info = {"Id":cid,"Name":"/foundry-w3-"+owner,"Image":image,
                    "Config":{"Labels":{"foundry.w3.owner":"different" if case=="foreign" else owner},
                              "User":"0" if case=="wrong-uid" else "65534:65534",
                              "WorkingDir":"/work","Entrypoint":ns["ENTRY"][:1],"Cmd":ns["ENTRY"][1:],
                              "Healthcheck":{"Test":["NONE"]},"Env":["PATH=/usr/bin:/bin"]},
                    "HostConfig":{"ReadonlyRootfs":True,"Privileged":False,"CapAdd":None,"CapDrop":["ALL"],
                                  "SecurityOpt":service["security_opt"],"NetworkMode":"none","PidMode":"",
                                  "IpcMode":"private","LogConfig":{"Type":"none"},"Devices":None,"VolumesFrom":None,
                                  "Tmpfs":{"/work":ns["TMPFS"].split(":",1)[1]},"PidsLimit":64,"Memory":512<<20},
                    "Mounts":[{"Type":"bind","Source":sock,"Destination":"/run/foundry-model.sock",
                               "RW":False,"Propagation":"rprivate"}]}
            return json.dumps([info]).encode()
        if op == "cp":
            assert args == ["cp",cid+":/opt/foundry/worker.py","-"]
            contents = b"wrong" if case == "stale-file" else b"fixture"
            buffer = io.BytesIO()
            with tarfile.open(fileobj=buffer, mode="w") as tar:
                info = tarfile.TarInfo("worker.py")
                info.size = len(contents)
                tar.addfile(info, io.BytesIO(contents))
            return buffer.getvalue()
        assert args[-1] == cid
        if op == "start":
            if case=="timeout":
                raise TimeoutError("injected start timeout")
            return b'{"inert":true}'
        if op == "inspect":
            return b'{"Status":"exited","ExitCode":0,"OOMKilled":false}'
        assert args == ["rm","--force","--volumes",cid]
        return b""

    ns["command"] = docker
    with tempfile.TemporaryDirectory(prefix="w3-control-") as tmp:
        ok = True
        try:
            launch(service, {"phase":"checks"}, Path(tmp), 1, "fixture", {"worker.py":hashlib.sha256(b"fixture").hexdigest()})
        except (ValueError, TimeoutError):
            ok = False
    assert ok == (case=="ok")
    assert ("start" in commands) == (case in ("ok","timeout"))
    assert ("rm" in commands) == (case!="foreign")
    print(f"simulated Docker client {case}: {commands}")


for case in ("ok","wrong-uid","stale-file","foreign","timeout","lost-create"):
    verify_ownership(case, actual_launch)

if len(sys.argv) == 2:
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    with tempfile.TemporaryDirectory(prefix="w3-preflight-") as tmp:
        private = Path(tmp)/"unused.sqlite"
        for key in ("FOUNDRY_W3_IMAGE","FOUNDRY_REVIEW_PACK","FOUNDRY_DIE_BINARY"):
            result = subprocess.run([binary,"serve","--product","--db",str(private)],
                                    env={"PATH":"/usr/bin:/bin","HOME":tmp,key:"incomplete"},
                                    capture_output=True)
            assert result.returncode == 1 and b"requires a pack and pinned W3 image" in result.stderr
            assert not private.exists()
            print(f"actual serve --product: {key} alone rejected before store open, exit=1")
