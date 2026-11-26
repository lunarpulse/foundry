"""Run offline render refusals and the retained W30b inert component regression."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
PIN = "sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60"


def refusals(out):
    ns = {"__name__": "offline"}
    for name in ("profile.py", "launcher.py"):
        exec(compile((ROOT / "deploy" / name).read_bytes(), name, "exec"), ns)
    calls = []

    def forbidden(*args, **kw):
        calls.append(args[0][0])
        raise AssertionError("invalid profile reached command")
    ns["command"] = forbidden
    base = json.loads((ROOT / "deploy/compose.yaml").read_bytes())
    socket = "/tmp/w31-offline/model.sock"
    render = ns["render"](base, PIN, socket)
    cli = subprocess.check_output(["/usr/bin/python3", "-I", str(ROOT / "scripts/check-deploy.py"),
                                   "--image", PIN, "--socket", socket])
    assert json.loads(cli) == render
    (out.parent / "render-static.json").write_bytes(cli)
    cases = {"user": "0", "privileged": True, "cap_add": ["ALL"], "network_mode": "host",
             "read_only": False, "volumes": [], "entrypoint": ["sh"], "environment": {"HOME": "/root"},
             "security_opt": [], "pull_policy": "always", "tmpfs": [], "pid": "host"}
    rows = []
    for key, value in cases.items():
        profile = copy.deepcopy(base)
        profile["services"]["cast"][key] = value
        rows.append((key, profile, PIN, socket))
    rows += [("image", base, v, socket) for v in ("latest", "sha256:a", "sha256:" + "A"*64)]
    rows.append(("socket", base, PIN, "/tmp/../bad/model.sock"))
    with out.open("w") as f:
        for key, profile, image, sock in rows:
            try:
                ns["dispatch"]({"timeout": 20, "workdir": str(out.parent), "profile": profile,
                                "image": image, "socket": sock})
            except ValueError as e:
                print(json.dumps({"case": key, "operand": profile["services"]["cast"].get(key, image if key == "image" else sock),
                      "layer": "offline dispatch before any command", "error": str(e),
                      "commands": calls, "starts": calls.count("start")}), file=f)
            else:
                raise AssertionError(key)
    print(f"pinned render static SHA-256 {hashlib.sha256(cli).hexdigest()}, {len(rows)} pre-command refusals")


def wiring(binary):
    # Replay the historical simulator against today's launcher. Only extend its
    # synthetic inspect with the stdin/tty fields now required before start.
    original = (HERE.parent / "verify_wiring.py").read_text()
    token = '"WorkingDir":"/work","Entrypoint":'
    assert original.count(token) == 1
    with tempfile.TemporaryDirectory(prefix="w31-regression-") as temp:
        scratch = Path(temp)
        spec = original.replace(token, '"OpenStdin":True,"Tty":False,' + token)
        spec = spec.replace("ROOT = HERE.parents[2]", "ROOT = " + repr(str(ROOT)) + "\nROOT = Path(ROOT)")
        path = scratch / "verify_wiring.py"
        path.write_text(spec)
        subprocess.run(["/usr/bin/python3", "-I", str(path), binary], check=True)


if __name__ == "__main__":
    refusals(Path(sys.argv[1]))
    wiring(sys.argv[2])
