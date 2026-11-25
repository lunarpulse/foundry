"""Verify saved fixture and recovery evidence using Python and local Git."""
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile

root = Path(sys.argv[1]).resolve()
env = dict(PATH="/usr/bin:/bin", GIT_ALLOW_PROTOCOL="", GIT_NO_LAZY_FETCH="1",
           GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1", GIT_NO_REPLACE_OBJECTS="1")


def read(name):
    return json.loads((root / name).read_bytes())


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def git(repo, *args):
    return subprocess.check_output(["git", "-C", repo, *args], env=env)


before = read("pre-recovery.json")
after = read("post-recovery.json")
assert before["schema_sha256"] == after["schema_sha256"]
assert before["files"] == after["files"]
assert len(after["tables"]) == 82 and before["tables"].keys() == after["tables"].keys()
changed = sorted(k for k, v in before["tables"].items() if after["tables"][k] != v)
assert changed == ["product_producer_events_v1", "product_producers_v1"]
for retry in (1, 2):
    assert read(f"retry-{retry}.json") == after
    assert read(f"retry-{retry}-producer.json") == read("post-recovery-producer.json")
retained = {p: stamp for p, stamp in after["files"].items()
            if p.startswith(("artifacts/", "retained-producer/"))}
assert len(retained) == 6
for path, stamp in retained.items():
    saved = "loss-sealed.patch" if path == "retained-producer/output.patch" else path
    assert sha((root / saved).read_bytes()) == stamp["sha256"]
assert sha(json.dumps(read("schema.json")).encode()) == after["schema_sha256"]

def journal(phase):
    data = read(phase + "-producer.json")
    return {row[0]: dict(zip(data["columns"], row, strict=True)) for row in data["rows"]}


pre = journal("pre-recovery")
post = journal("post-recovery")
assert pre.keys() == post.keys()
assert pre["loss"]["state"] == "sealed" and post["loss"]["state"] == "uncertain"
assert {**pre["loss"], "state": "uncertain"} == post["loss"]
assert all(pre[k] == post[k] for k in pre if k != "loss")
events_before = read("pre-recovery-producer.json")["events"]
events_after = read("post-recovery-producer.json")["events"]
assert len(events_after) == len(events_before) + 1
assert [e for e in events_after if e[:2] != ["loss", "uncertain"]] == events_before

result = read("final-result.json")
tasks = {entry["receipt"]["order_id"]: entry for entry in read("pre-recovery-tasks.json")}
assert sha(tasks["good"]["canonical_utf8"].encode()) == tasks["good"]["receipt"]["task_sha256"]
assert result["source_instance_id"] == tasks["good"]["receipt"]["source"]
assert result["admission_hash"] == tasks["good"]["receipt"]["admission_hash"]
assert result["work"]["claim_fence"] == pre["good"]["fence"]
assert result["work"]["claim_epoch"] == pre["good"]["epoch"]
assert result["work"]["worker_id"] == pre["good"]["worker"]
assert sha(result["work"]["payload_snapshot"].encode()) == tasks["good"]["receipt"]["snapshot_sha256"]
for field, receipt_field in (("installation_id", "installation"), ("database_id", "database"),
                             ("lifecycle", "lifecycle"), ("admission_seq", "admission_sequence"),
                             ("source_revision", "source_revision"), ("source_generation", "generation"),
                             ("source_instance_revision", "instance_revision")):
    assert result[field] == tasks["good"]["receipt"][receipt_field]
with tempfile.TemporaryDirectory() as temp:
    for order, bundle, artifact in (("good", "fixture.bundle", "sealed.patch"),
                                    ("loss", "loss.bundle", "loss-sealed.patch")):
        repo = Path(temp) / order
        repo.mkdir()
        git(repo, "init", "--bare", "-q")
        git(repo, "bundle", "unbundle", str(root / bundle))
        head = pre[order]["result_commit"]
        base = tasks[order]["receipt"]["base_commit"]
        assert git(repo, "rev-list", "--parents", "-1", head).decode().strip() == f"{head} {base}"
        patch = git(repo, "diff", "--no-renames", "--no-ext-diff", "--no-textconv",
                    "--binary", "--full-index", base, head, "--")
        assert patch == (root / artifact).read_bytes()
        assert sha(patch) == pre[order]["artifact_sha256"]
        assert git(repo, "diff", "--name-only", base, head).strip() == b"README"
        if order == "good":
            report = result["artifacts"][-1]
            data = (root / "artifacts" / report["relative_path"]).read_bytes()
            assert sha(data) == report["artifact_sha256"]
            assert json.loads(data) == {"commit": head, "blob_sha256": sha(git(repo, "show", head + ":README"))}
            assert result["tuple"]["immutable_commit"] == head
            assert report["pack_digest"] == tasks["good"]["receipt"]["pack_digest"]

t = result["tuple"]
h = hashlib.sha256(b"foundry-artifact-tuple-v1")
for value in [t["order"].encode(), *(struct.pack(">Q", t[k]) for k in ("attempt", "run", "generation")),
              t["immutable_commit"].encode(), t["artifact_location"].encode()]:
    h.update(struct.pack(">Q", len(value)) + value)
generations = [{k: g[k] for k in ("generation", "commit", "fix_log_path")} for g in t["generations"]]
h.update(json.dumps(generations, separators=(",", ":")).encode())
assert h.hexdigest() == result["tuple_digest"]
assert read("withdrawn-before.json") == read("withdrawn-after.json")

if (root / "w21.json").exists():
    observations = read("observed-deliveries.json")
    first = {r["issue"]: r for r in read("first-observations.json")}
    assert len(first) == 13 and len(observations) == 15
    for issue, anchor in first.items():
        deliveries = [row for row in observations if row[0] == issue]
        assert all(row[2] == "acked" for row in deliveries)
        assert anchor["delivery"] == deliveries[0][1] and anchor["snapshot"] == deliveries[0][3]
        assert anchor["sha256"] == sha(anchor["snapshot"].encode())
        if issue in ("valid-first", "unsupported-first"):
            assert len(deliveries) == 2 and deliveries[1][1] < deliveries[0][1]
    (admitted,) = read("w21-tasks.json")
    assert admitted["receipt"]["delivery_id"] == first["valid-first"]["delivery"]
    assert sha(admitted["canonical_utf8"].encode()) == admitted["receipt"]["task_sha256"]
    assert admitted["receipt"]["actor"] == "owner"
    for name in ("work_claims", "producers", "producer_events", "review_runs", "result_bindings",
                 "grants", "approval_result_bindings", "publish_outcomes"):
        assert read("w21.json")["tables"]["product_" + name + "_v1"]["rows"] == 0
    (task,) = read("w20-tasks.json")
    assert sha(task["canonical_utf8"].encode()) == task["receipt"]["task_sha256"]
    print("Intake verified from saved rows: 13 first observations, 15 acknowledgements, one owner task, zero downstream effects.")

print(json.dumps(dict(tables=len(after["tables"]), changed=changed, retries_equal=2,
                      retained_review_and_producer_files=6, bundles_verified=2,
                      finalized_head=t["immutable_commit"], tuple_sha256=h.hexdigest(),
                      sealed_patch_sha256=pre["good"]["artifact_sha256"],
                      original_collection_missing_denial="see exact Cargo test",
                      owner_withdrawal_database_and_files_equal=True), indent=2))
