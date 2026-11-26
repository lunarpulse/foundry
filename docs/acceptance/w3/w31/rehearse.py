"""Offline exercise of the host recorder and reader. All simulated records are temporary."""
from contextlib import ExitStack
import io
import json
from pathlib import Path
import sqlite3
import stat
import subprocess
import sys
import tarfile
import tempfile
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(HERE))
import verify
import witness as w


def exercise(out, case):
    root = out / "fixture"
    root.mkdir()
    stage = root / "artifacts/w3-producers/run"
    stage.mkdir(parents=True)
    repo = root / "repo"
    repo.mkdir()
    env = dict(PATH="/usr/bin:/bin", HOME=str(root), GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               GIT_AUTHOR_NAME="fixture", GIT_COMMITTER_NAME="fixture", GIT_AUTHOR_EMAIL="fixture@invalid",
               GIT_COMMITTER_EMAIL="fixture@invalid", GIT_ALLOW_PROTOCOL="")
    def git(*args):
        return subprocess.check_output(["/usr/bin/git","-C",str(repo),*args],env=env)
    git("init","-q","--template=")
    (repo / "die-output.json").write_bytes(b"deliberate pre-Die refusal\n")
    git("add","die-output.json")
    git("commit","-qm","inert")
    base = git("rev-parse","HEAD").decode().strip()
    for path in ("owner","service","input","serve.sock",".foundry-work/w31-other/README"):
        p = root / path
        p.parent.mkdir(parents=True,exist_ok=True)
        p.write_text(path)
    with sqlite3.connect(root / "product.sqlite") as db:
        db.execute("create table product_producers_v1(epoch,fence,state,order_id,operation_key)")
        db.execute("insert into product_producers_v1 values (3,4,'reserved','subject','run')")
    ns = {"__name__":"rehearsal"}
    for file in ("profile.py","launcher.py"):
        exec(compile((ROOT/"deploy"/file).read_bytes(),file,"exec"),ns)
    # Invalid executable name provides a fail-closed sentinel on every transport call.
    ns["DOCKER"] = ["/nonexistent/w31-recording-transport"]
    command = ns["command"]
    cid = ("a" if case=="default" else "b")*64
    image_files = {n:(ROOT/"deploy"/f).read_bytes() for n,f in (("worker.py","worker.py"),("model-client","model-client.py"))}
    profile = json.loads((ROOT/"deploy/compose.yaml").read_bytes())
    sock = str(root / "model.sock")
    svc = ns["render"](profile,w.PIN,sock)["services"]["cast"]
    config = {"image":w.PIN,"timeout_env":None if case=="default" else "20",
              "timeout_secs":600 if case=="default" else 20,"artifacts":str(root/"artifacts"),
              "sessions":str(root/"sessions"),"claims":str(root/"claims"),"repo":str(repo)}
    w.save(out/"controller-config.json",config)
    spec = dict(image=w.PIN,profile=profile,socket=sock,stage=dict(artifact_commit=base,role="producer",stage_id="authorized-producer"),
                workdir=str(stage),cache=str(root/"sessions/w3-objects"),repo=str(repo),run="run",order="subject",
                intent={"order":"subject","epoch":4}, image_files={n:w.digest(b) for n,b in image_files.items()},
                timeout=4,lease_end_ms=9999999999999,task=None,inputs={},previous=None,manifest={"base":base},
                provider="none",budget_scope=[],token="inert")
    def transport(argv, until, *args, **kwargs):
        if argv[:1] == ["/usr/bin/git"]:
            return command(argv,until,*args,**kwargs)
        assert argv[:1] == ns["DOCKER"]
        op = argv[1:]
        if op[0] == "create":
            assert [op[i+1] for i,a in enumerate(op) if a=="--env"] == list(ns["PYTHON_IMAGE_ENV"])
            assert not set(ns["PYTHON_IMAGE_ENV"]).intersection(ns["ENV"])
            return cid.encode()
        if op[:2] == ["container","inspect"]:
            owner = json.loads((stage/"runtime-stage.json").read_bytes())["owner"]
            return json.dumps([{"Id":cid,"Name":"/foundry-w3-"+owner,"Image":w.PIN,
                "Config":{"Labels":{"foundry.w3.owner":owner},"User":svc["user"],"WorkingDir":"/work",
                          "Entrypoint":verify.ENTRY[:1],"Cmd":verify.ENTRY[1:],"OpenStdin":True,"Tty":False,
                          "Healthcheck":{"Test":["NONE"]},"Env":["PATH=/usr/bin", *ns["PYTHON_IMAGE_ENV"]]},
                "Mounts":[{"Type":"bind","Source":sock,"Destination":svc["volumes"][0]["target"],"RW":False,"Propagation":"rprivate"}],
                "HostConfig":{"Tmpfs":{"/work":ns["TMPFS"].split(":",1)[1]},"CapAdd":None,"CapDrop":["ALL"],
                    "SecurityOpt":svc["security_opt"],"ReadonlyRootfs":True,"Privileged":False,"NetworkMode":"none",
                    "PidMode":"","IpcMode":"private","Devices":[],"DeviceRequests":None,"VolumesFrom":None,
                    "LogConfig":{"Type":"none"},"Memory":512<<20,"PidsLimit":64}}]).encode()
        if op[0] == "cp":
            name = op[1].split("/")[-1]
            buf = io.BytesIO()
            with tarfile.open(fileobj=buf,mode="w") as tar:
                obj = tarfile.TarInfo(name)
                obj.size = len(image_files[name])
                tar.addfile(obj,io.BytesIO(image_files[name]))
            return buf.getvalue()
        if op[0] == "start":
            return command(["/usr/bin/python3","-I","-c","import sys; sys.stdin.buffer.read(); sys.exit(1)"],until,*args,**kwargs)
        assert op == ["rm","--force","--volumes",cid]
        if writers:
            writers[0].terminate()
        return b""
    ns["command"] = transport
    def observe(_, argv, **kwargs):
        if argv == ["container","inspect",cid]:
            return subprocess.CompletedProcess(argv,1,b"",b"no such container")
        assert argv[:3] == ["exec","-i",cid]
        paths = json.loads(kwargs["input"])
        status = {k:["0"] for k in ("CapAmb","CapPrm","CapInh","CapBnd","CapEff")}
        status.update(Uid=["65534"]*4,Gid=["65534"]*4,NoNewPrivs=["1"])
        proc = {"status":status,"cmdline":verify.ENTRY,"env_keys":["PATH"],"namespace":{k:"fixture" for k in ("mnt","net","pid","user")},
                "net_devices":["lo"],"mountinfo":["1 0 0:1 / / ro - fixture fixture ro",
                "2 0 0:2 / /work rw - tmpfs tmpfs rw","3 0 0:3 / /run/foundry-model.sock ro - fixture fixture ro"]}
        data = {"worker":proc,"observer":proc,"own":{"path":"/work/w31-canary","reread":b"W31 workspace\n".hex()},
                "serve":{"path":paths["serve"],"errno":"ENOENT"},"opens":{k:[{"op":op,"path":v,"errno":"ENOENT"}
                for op in ("read","write")] for k,v in paths.items() if k!="serve"}}
        return subprocess.CompletedProcess(argv,0,json.dumps(data).encode(),b"")
    snapshot = w.snapshot
    def fixture_snapshot(paths, db):
        harmless = dict(paths, management=str(root/"serve.sock"))
        s = snapshot(harmless,db)
        for k in ("management","serve"):
            s["files"][k] = dict(path=paths[k],mode=stat.S_IFSOCK,inode=1)
        return s
    writers = []
    popen = subprocess.Popen
    def owned_process(argv, **kw):
        if argv[:1] == ns["DOCKER"]:
            assert argv[1:3] == ["exec",cid] and argv[-1] == w.WRITER
            argv = ["/usr/bin/python3","-I","-u","-c",w.WRITER.replace("/work/w31-heartbeat",str(root/"stream"))]
            writers.append(popen(argv,**kw))
            return writers[-1]
        assert argv[0] in ("/usr/bin/git","/usr/bin/python3")
        return popen(argv,**kw)
    checks = iter((dict(Running=True,Status="running",ExitCode=0,OOMKilled=False),
                   dict(Running=False,Status="exited",ExitCode=1,OOMKilled=False)))
    with ExitStack() as ctx:
        for name,value in (("host_control",lambda p:"HTTP/1.1 401 Unauthorized"),("snapshot",fixture_snapshot),
                           ("docker",observe),("read_state",lambda *a:next(checks))):
            ctx.enter_context(patch.object(w,name,value))
        ctx.enter_context(patch.object(subprocess,"Popen",owned_process))
        try:
            w.inspect_case(spec,ns,out,root,w.digest((HERE/"source.sha256").read_bytes()))
        except SystemExit as expected:
            assert expected.code == 1
        else:
            raise AssertionError("inert frame adopted")
    w.save(out/"controller-result.json",dict(order_id="subject",operation_key="run",epoch=3,fence=4,
           state="uncertain",result_commit=None,artifact_sha256=None))
    w.save(out/"serve-count.json",{"requests":1})
    verify.check(out, ("/nonexistent/w31-recording-transport",))


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="w31-rehearsal-") as t:
        for case in ("default","timeout"):
            out = Path(t)/case
            out.mkdir()
            exercise(out,case)
        for name, alter in (("I06/runtime.json",lambda s:s.replace('"fence": 4','"fence": 5',1)),
                            ("I06/heartbeat",lambda s:s+"late write\n"),
                            ("I02/child-open.jsonl",lambda s:s.replace('"ENOENT"','"EISDIR"',1))):
            p = out/name
            original = p.read_text()
            p.write_text(alter(original))
            try:
                verify.check(out, ("/nonexistent/w31-recording-transport",))
            except AssertionError:
                print(f"synthetic corruption rejected: {name}")
            else:
                raise AssertionError(name)
            p.write_text(original)
    print("synthetic recorder and reader rehearsal only; no Docker CLI or runtime observation")
