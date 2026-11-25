"""Read a private W2 fixture and retain reproducible evidence without bearer material."""
import hashlib
import json
from pathlib import Path
import sqlite3
import struct
import subprocess
import sys

root, out = map(Path, sys.argv[1:3])
phase = sys.argv[3]
out.mkdir(parents=True, exist_ok=True)
db = sqlite3.connect((root / "product.sqlite").as_uri() + "?mode=ro", uri=True)
db.execute("BEGIN")


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def frame(raw):
    return struct.pack(">Q", len(raw)) + raw


def field(value):
    if value is None:
        return b"N"
    if isinstance(value, bytes):
        return b"B" + frame(value)
    if isinstance(value, str):
        return b"T" + frame(value.encode())
    if isinstance(value, int):
        return b"I" + struct.pack(">q", value)
    return b"F" + struct.pack(">d", value)


def record(name, value):
    (out / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


schema = db.execute("SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY 1,2,3,4").fetchall()
tables = {}
for kind, name, _, _ in schema:
    if kind == "table":
        quoted = '"' + name.replace('"', '""') + '"'
        rows = sorted(b"".join(map(field, row)) for row in db.execute("SELECT * FROM " + quoted))
        tables[name] = {"rows": len(rows), "sha256": digest(b"".join(map(frame, rows)))}

files = {}
for directory in ("artifacts", "repo/.git/objects", "loss-repo/.git/objects", "retained-producer"):
    for path in sorted((root / directory).rglob("*")):
        if path.is_file():
            files[str(path.relative_to(root))] = {"sha256": digest(path.read_bytes()), "inode": path.stat().st_ino}
record(phase + ".json", {"tables": tables, "schema_sha256": digest(json.dumps(schema).encode()), "files": files})

if phase == "w21":
    first = db.execute("SELECT issue,delivery,snapshot,snapshot_sha256 FROM product_issue_first_v1 ORDER BY issue")
    anchors = []
    for issue, delivery, snapshot, committed in first:
        assert digest(snapshot) == committed
        anchors.append(dict(issue=issue, delivery=delivery, snapshot=snapshot.decode(), sha256=committed))
    record("first-observations.json", anchors)
    record("observed-deliveries.json", db.execute("SELECT issue_id,delivery_id,state,payload FROM product_intake_inbox_v1 ORDER BY rowid").fetchall())

if phase in ("w20", "w21", "pre-recovery"):
    tasks = db.execute("SELECT task_bytes,receipt FROM product_code_tasks_v1 ORDER BY order_id")
    record(phase + "-tasks.json", [{"canonical_utf8": b.decode(), "receipt": json.loads(r)} for b, r in tasks])

if phase == "pre-recovery":
    record("schema.json", schema)
    repo = root / "repo"
    subprocess.run(["git", "-C", repo, "bundle", "create", str(out / "fixture.bundle"), "--all"], check=True)
    subprocess.run(["git", "-C", root / "loss-repo", "bundle", "create", str(out / "loss.bundle"), "--all"], check=True)
    (out / "loss-sealed.patch").write_bytes((root / "retained-producer/output.patch").read_bytes())
    head, artifact, commitment = db.execute("SELECT result_commit,artifact,artifact_sha256 FROM product_producers_v1 WHERE order_id='good'").fetchone()
    assert digest(artifact) == commitment
    (out / "sealed.patch").write_bytes(artifact)
    record("final-result.json", json.loads(db.execute("SELECT payload_json FROM product_result_bindings_v1 WHERE order_id='good'").fetchone()[0]))
    for path in (root / "artifacts").rglob("*"):
        if path.is_file():
            target = out / path.relative_to(root)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(path.read_bytes())

if phase in ("pre-recovery", "post-recovery", "retry-1", "retry-2"):
    columns = "order_id,operation_key,service,service_revision,credential_revision,worker,epoch,fence,deadline,state,result_commit,artifact_sha256"
    record(phase + "-producer.json", {"rows": db.execute("SELECT " + columns + " FROM product_producers_v1 ORDER BY order_id").fetchall(),
           "columns": columns.split(","), "events": db.execute("SELECT order_id,state,at FROM product_producer_events_v1 ORDER BY order_id,at,state").fetchall()})
