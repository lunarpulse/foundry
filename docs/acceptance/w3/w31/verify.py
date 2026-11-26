"""Independent, fail-closed comparison of W31 host observations. No Docker access."""
from collections import Counter
import hashlib
import json
from pathlib import Path
import stat
import subprocess
import sys

HERE = Path(__file__).resolve().parent
PIN = "sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60"
DENIED = {"ENOENT", "EPERM", "EACCES"}
ENTRY = ["/usr/bin/python3", "-I", "/opt/foundry/worker.py"]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def refusals(path):
    rows = [json.loads(s) for s in path.read_text().splitlines()]
    expected = ["user","privileged","cap_add","network_mode","read_only","volumes","entrypoint","environment",
                "security_opt","pull_policy","tmpfs","pid","image","image","image","socket"]
    assert Counter(r["case"] for r in rows) == Counter(expected)
    assert all(r["commands"] == [] and r["starts"] == 0 for r in rows)


def check(case, docker_prefix=("/usr/bin/docker", "--host", "unix:///var/run/docker.sock")):
    def read(name):
        return json.loads((case / name).read_bytes())

    j = read("observed.json")["join"]
    def single(name):
        data = read(name)
        assert data.pop("join") == j, name
        return data

    def many(name):
        rows = [json.loads(line) for line in (case / name).read_text().splitlines()]
        assert rows and all(r.pop("join") == j for r in rows), name
        return rows

    assert j["source_sha256"] == sha((HERE / "source.sha256").read_bytes())
    binding = read("binding.json")
    runtime = read("runtime-stage.json")
    service = read("render.json")
    op = json.loads((case / "controller-result.json").read_bytes())
    assert op["state"] == "uncertain" and op["order_id"] == j["order"]
    assert op["operation_key"] == j["run"] and op["epoch"] == j["epoch"] and op["fence"] == j["fence"]
    assert op["result_commit"] is None and op["artifact_sha256"] is None
    assert binding["intent"]["order"] == j["order"] and binding["intent"]["epoch"] == j["fence"]
    assert binding["stage"] == j["stage"] == "authorized-producer"
    assert binding["run"] == runtime["run"] == j["run"] and binding["calls"] == binding["max_cost_microusd"] == 0
    assert sha((case / "render.json").read_bytes()) == runtime["render_sha256"] == j["render_sha256"]
    assert runtime["container"] == runtime["removed"] == j["container"]
    trace = [json.loads(s)["argv"] for s in (case/"docker-argv.jsonl").read_text().splitlines()]
    assert all(argv[:len(docker_prefix)] == list(docker_prefix) for argv in trace)
    calls = [argv[len(docker_prefix):] for argv in trace]
    create, *next_calls = calls
    cid = j["container"]
    assert create[0] == "create" and "--pull=never" in create and create[-3:] == [PIN,*ENTRY[1:]]
    assert create[create.index("--name")+1] == "foundry-w3-" + runtime["owner"]
    expected_calls = [["container","inspect",cid],
                      *[["cp",cid+":/opt/foundry/"+name,"-"] for name in ("worker.py","model-client")],
                      ["start","--attach","--interactive",cid]]
    # serde_json may order the two image-file keys lexicographically.
    assert next_calls[0] == expected_calls[0] and sorted(next_calls[1:3]) == sorted(expected_calls[1:3])
    assert next_calls[3] == expected_calls[3]
    rm = ["rm","--force","--volumes",cid]
    assert next_calls[4:] in ([rm], [["inspect","--format","{{json .State}}",cid], rm])
    assert runtime["phase"] == "stage" and runtime["start_requested"] is True
    info = single("I06/runtime.json")
    c, h = info["config"], info["host_config"]
    assert info["image"] == j["image"] and info["id"] == j["container"]
    assert info["name"] == "/foundry-w3-" + runtime["owner"] and info["owner"] == runtime["owner"]
    assert service["image"] == j["image"] == runtime["image"] == PIN
    assert c["User"] == runtime["user"] == service["user"] == "65534:65534"
    assert c["Entrypoint"] + c["Cmd"] == service["entrypoint"] == ENTRY
    assert c["WorkingDir"] == service["working_dir"] == "/work" and c["OpenStdin"] and not c["Tty"]
    assert c["Healthcheck"]["Test"] == ["NONE"] and service["healthcheck"]["disable"]
    assert h["ReadonlyRootfs"] == service["read_only"] is True and not h["Privileged"]
    assert h["NetworkMode"] == service["network_mode"] == "none"
    assert not any(h[k] for k in ("Devices","DeviceRequests","VolumesFrom","CapAdd","PidMode","Binds"))
    assert h["IpcMode"] == service["ipc"] == "private" and h["LogConfig"]["Type"] == service["logging"]["driver"] == "none"
    assert h["SecurityOpt"] == service["security_opt"] and h["CapDrop"] == service["cap_drop"] == ["ALL"]
    assert h["Memory"] == 512 << 20 and service["mem_limit"] == "512m"
    assert h["PidsLimit"] == service["pids_limit"] == 64 and info["env_keys"] == ["PATH"]
    assert set(info["unset_env"]) <= {"LANG","GPG_KEY","PYTHON_VERSION","PYTHON_SHA256"}
    assert list(h["Tmpfs"]) == ["/work"] and ["/work:" + h["Tmpfs"]["/work"]] == service["tmpfs"]
    mount, = info["mounts"]
    vol, = service["volumes"]
    assert mount["Type"] == vol["type"] == "bind" and mount["Source"] == vol["source"]
    assert mount["Destination"] == vol["target"] and not mount["RW"] and vol["read_only"] and mount["Propagation"] == "rprivate"
    setting, = many("I01/overrides.jsonl")
    config = read("controller-config.json")
    for field, operand in (("repo","repo"),("artifact_root","artifacts"),("claims","claims"),("sessions","sessions")):
        assert setting[field] == config[operand]
    assert config["image"] == PIN and config["timeout_env"] == (None if case.name == "default" else "20")
    assert config["timeout_secs"] == setting["timeout_setting"]
    checker = HERE.parents[3] / "scripts/check-deploy.py"
    independent_render = subprocess.check_output(["/usr/bin/python3", "-I", str(checker), "--image", PIN,
                                                 "--socket", setting["socket"]])
    assert json.loads(independent_render)["services"]["cast"] == service
    assert setting["case"] == case.name and setting["socket"] == mount["Source"]
    assert setting["timeout_setting"] == (600 if case.name == "default" else 20)
    assert 0 < binding["timeout_secs"] == setting["timeout_effective"] <= setting["timeout_setting"]
    assert setting["start_requested"] and setting["lease_end_ms"] > 0
    assert runtime["image_files_sha256"] == {name:sha((HERE.parents[3]/"deploy"/file).read_bytes())
                     for name,file in (("worker.py","worker.py"),("model-client","model-client.py"))}
    before = single("I02/controller-before.json")
    assert before == single("I02/controller-after.json")
    files = before["files"]
    assert stat.S_ISDIR(files["home"]["mode"]) and stat.S_ISSOCK(files["management"]["mode"])
    assert stat.S_ISSOCK(files["serve"]["mode"])
    for k in ("db", "wal_db", "wal", "shm", "owner", "service", "watch", "other"):
        assert stat.S_ISREG(files[k]["mode"]) and len(files[k]["sha256"]) == 64
    opens = many("I02/child-open.jsonl")
    def denied(rows, names):
        assert sorted((r["surface"], r["op"]) for r in rows) == sorted((n,op) for n in names for op in ("read","write"))
        assert all(r["errno"] in DENIED and r["path"] == files[r["surface"]]["path"] for r in rows)
    denied(opens, ("db","wal_db","wal","shm","home","owner","service","management"))
    rows = many("I03/workspaces.jsonl")
    own, = [r for r in rows if r["surface"] == "own"]
    assert own["path"] == "/work/w31-canary" and bytes.fromhex(own["reread"]) == b"W31 workspace\n"
    denied([r for r in rows if r["surface"] != "own"], ("watch","other"))
    cans = single("I03/canaries.json")
    assert all(cans[k]["before"] == cans[k]["after"] == files[k]["sha256"] for k in ("watch","other"))
    serve, = many("I03/serve-audit.jsonl")
    assert " 401 " in serve["host_control"] and serve["child_connect"]["errno"] in DENIED
    assert serve["child_connect"]["path"] == files["serve"]["path"] and read("serve-count.json") == {"requests":1}
    proc = single("I06/proc-self.json")
    assert proc["worker"]["cmdline"] == ENTRY
    assert set(proc["worker"]["namespace"]) == {"mnt", "net", "pid", "user"}
    assert proc["worker"]["namespace"] == proc["observer"]["namespace"]
    for p in proc.values():
        assert all(isinstance(v, str) for v in p["namespace"].values())
        assert isinstance(p["env_keys"], list)
        assert not {"LANG","GPG_KEY","PYTHON_VERSION","PYTHON_SHA256"}.intersection(p["env_keys"])
        s = p["status"]
        assert s["Uid"] == s["Gid"] == ["65534"]*4 and s["NoNewPrivs"] == ["1"]
        assert all(int(s[k][0], 16) == 0 for k in ("CapInh", "CapPrm", "CapEff", "CapBnd", "CapAmb"))
        mounts = {line.split()[4]: line for line in p["mountinfo"]}
        assert "ro" in mounts["/"].split()[5].split(",")
        assert "rw" in mounts["/work"].split()[5].split(",") and " - tmpfs " in mounts["/work"]
        assert "ro" in mounts["/run/foundry-model.sock"].split()[5].split(",")
        assert p["net_devices"] == ["lo"]
    reap = single("I06/reap.json")
    assert reap["removed"] == j["container"] and reap["absent"] and reap["inspect_exit"] == 1
    if case.name == "timeout":
        assert reap["writer_exit"] is not None
        assert reap["deadline_expired"] and reap["error"] in ("TimeoutError","TimeoutExpired")
        a,b = many("I06/active-hashes.jsonl")
        assert 0 < a["bytes"] < b["bytes"] and a["sha256"] != b["sha256"]
        assert reap["start_monotonic_ns"] < a["monotonic_ns"] < b["monotonic_ns"] < reap["deadline_monotonic_ns"] <= reap["observed_reap_ns"]
        assert reap["deadline_monotonic_ns"] - b["monotonic_ns"] < 1_000_000_000
        assert reap["pre_rm_bytes"] >= b["bytes"]
        x,y = many("I06/output-hashes.jsonl")
        data = (case / "I06/heartbeat").read_bytes()
        assert data.splitlines() == [str(i).encode() for i in range(len(data.splitlines()))]
        assert x["bytes"] == y["bytes"] == len(data) >= b["bytes"]
        assert x["sha256"] == y["sha256"] == sha(data)
        assert all(sha(data[:s["bytes"]]) == s["sha256"] for s in (a,b))
        assert reap["observed_reap_ns"] < x["monotonic_ns"] < y["monotonic_ns"]
    else:
        assert not reap["deadline_expired"] and reap["pre_rm"] == {"Status":"exited","Running":False,"ExitCode":1,"OOMKilled":False}
    print(f"{case.name}: verified observed fields for {j['order']} / {j['run']} / {j['container']}")
    return j


if __name__ == "__main__":
    refusals(Path(sys.argv[1]) / "I01/overrides.jsonl")
    a,b = (check(Path(sys.argv[1]) / name) for name in ("default","timeout"))
    assert a["container"] != b["container"] and a["render_sha256"] != b["render_sha256"]
