"""Run exactly one compiled W31 fixture in a fresh evidence directory."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

PIN = "sha256:949511f82f9d8723b68f85c6c23c213a8b9ee0a3aaa5f78cbfce2ab950520a60"
FIXTURE = "w31_private_fixture"
UNIT = "isolation::dispatch::w31::owned_producer"


def binaries(log):
    outputs = {}
    for line in log.read_text().splitlines():
        if not line.startswith("{"):
            continue
        msg = json.loads(line)
        if msg["reason"] != "compiler-artifact" or not msg["executable"]:
            continue
        t = msg["target"]
        outputs[(t["name"], msg["profile"]["test"])] = msg["executable"]
    pair = outputs["w2", True], outputs["foundry", True]
    for exe, name in zip(pair, (FIXTURE, UNIT)):
        listing = subprocess.check_output([exe, "--list", "--ignored"], text=True).splitlines()
        assert listing.count(name + ": test") == 1, (exe, name)
    return pair


def run(root, case):
    assert case in ("default", "timeout") and os.environ["FOUNDRY_W3_IMAGE"] == PIN
    fixture, unit = binaries(root / "build.log")
    out = root / case
    out.mkdir(mode=0o700)
    with tempfile.TemporaryDirectory(prefix="w31-", dir="/tmp") as tmp:
        env = {"PATH": "/usr/bin:/bin", "HOME": tmp, "TMPDIR": tmp,
               "FOUNDRY_W3_IMAGE": PIN, "W31_CASE": case,
               "W31_WITNESS_DIR": str(out), "W31_UNIT_BINARY": unit}
        cmd = [fixture, "--exact", FIXTURE, "--ignored", "--nocapture"]
        meta = {"argv": cmd, "env": env,
                "binary_sha256": {p:hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in (fixture,unit)}}
        (out / "invocation.json").write_text(json.dumps(meta, indent=2) + "\n")
        with (out / "test.log").open("x") as log:
            status = subprocess.run(cmd, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
            print(f"exit={status}", file=log)
        if status:
            raise SystemExit(status)
        assert (out / "observed.json").is_file()


if __name__ == "__main__":
    run(Path(sys.argv[1]).resolve(strict=True), sys.argv[2])
