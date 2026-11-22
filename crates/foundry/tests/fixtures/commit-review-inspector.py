#!/usr/bin/env python3
"""Trusted offline Die protocol adapter. Inspect a Git blob as data."""
import hashlib
import json
import subprocess
import shutil
import time
from pathlib import Path

# Count entry even when the adapter later fails to parse its stage input.
artifact_root = next(parent for parent in Path.cwd().parents if parent.name == "artifacts")
with (artifact_root.parent / "inspector-calls.jsonl").open("a") as calls:
    calls.write(json.dumps({"workdir": str(Path.cwd().relative_to(artifact_root))}) + "\n")

fixture = artifact_root.parent
# These controls are created only by the restart and process-loss cases.
swap = fixture / "swap-snapshot.json"
if swap.exists():
    relative = Path(json.loads(swap.read_text()))
    assert not relative.is_absolute() and relative.parent.parent == Path("artifacts")
    assert relative.name == "pack-snapshot"
    snapshot = fixture / relative
    snapshot.rename(fixture / "saved-snapshot")
    shutil.copytree(fixture / "pack-x", snapshot)
    swap.rename(fixture / "swapped.json")

if (fixture / "fail-inspector").exists():
    raise SystemExit(1)

if (fixture / "pause-inspector").exists():
    (fixture / "inspector-entered").touch()
    limit = time.monotonic() + 30
    while not (fixture / "release-inspector").exists() and time.monotonic() < limit:
        time.sleep(0.025)
    (fixture / "inspector-exited").touch()
    raise SystemExit(1)

stage = json.loads(Path("stage-input.json").read_bytes())
lens = bytes(stage["lens_bytes"])
head = subprocess.check_output(["git", "rev-parse", "HEAD"]).decode().strip()
assert head == stage["artifact_commit"]
blob = subprocess.check_output(["git", "show", head + ":README"])
matched = lens.strip() in blob.splitlines()
Path("inspection.json").write_text(json.dumps({
    "commit": head, "blob_sha256": hashlib.sha256(blob).hexdigest(),
    "lens_sha256": hashlib.sha256(lens).hexdigest(), "matched": matched,
}))
Path("die-output.json").write_text(json.dumps({
    "findings": not matched, "verdict_categories": [] if matched else ["blocking"],
    "inspected_commit": head,
}))
