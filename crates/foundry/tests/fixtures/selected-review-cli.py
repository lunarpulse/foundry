"""Exercise the normal CLI on a private database. SQL observations are read-only."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import sqlite3
import struct
import subprocess
import sys
import tempfile
import tomllib
import time

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=Path)
parser.add_argument("--retain", type=Path)
parser.add_argument("--race-worker", type=Path)
args = parser.parse_args()
binary = args.binary.resolve(strict=True)
repo = Path(__file__).resolve().parents[4]
temporary = tempfile.TemporaryDirectory() if args.retain is None else None
root = Path(temporary.name) if temporary else args.retain
if not temporary:
    root.mkdir(mode=0o700)
os.umask(0o077)
git = root / "repository"
git.mkdir()
db = root / "product.sqlite"
registry = root / "registry.json"
session = root / "owner.session"
credential = root / "service.credential"
source = "commit-review-source"
environment = {k: os.environ[k] for k in ("PATH", "TMPDIR") if k in os.environ}
environment.update({
    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1", "GIT_ALLOW_PROTOCOL": "file",
    "FOUNDRY_SOURCE_COMMIT": subprocess.check_output(["git","-C",repo,"rev-parse","HEAD"],text=True).strip(),
    "FOUNDRY_PRODUCT_ARTIFACT_ROOT": str(root/"artifacts"),
    "FOUNDRY_PRODUCT_SESSION_ROOT": str(root/"sessions"),
    "FOUNDRY_PRODUCT_CLAIM_ROOT": str(root/"claims"),
    "FOUNDRY_PRODUCT_REVIEW_SOURCE": source, "FOUNDRY_REPO_PATH": str(git),
    "FOUNDRY_REVIEW_PACK_REGISTRY": str(registry),
    "FOUNDRY_DIE_BINARY": str(Path(__file__).with_name("commit-review-inspector.py").resolve()),
})
for cmd in (["init","-q"], ["config","user.name","Review fixture"], ["config","user.email","review@example.invalid"]):
    subprocess.run(["git","-C",git,*cmd],env=environment,check=True)
(git/"README").write_text("first requirement\nsecond requirement\n")
subprocess.run(["git","-C",git,"add","README"],env=environment,check=True)
subprocess.run(["git","-C",git,"commit","-qm","Review input"],env=environment,check=True)
commit = subprocess.check_output(["git","-C",git,"rev-parse","HEAD"],text=True).strip()

def make_pack(letter, needle, held):
    path = root / ("pack-" + letter)
    shutil.copytree(repo/"skills/fixture-min/v1", path)
    (path/"lens-a.md").write_text(needle+"\n")
    policy = path / "policy.toml"
    graph = policy.read_text().replace('role = "lens"', 'role = "verify"')
    if held:
        graph = graph.replace('do = "emit(InspectionPassed, summary)"', 'goto = "end_hold"')
        graph = graph.replace('resume_targets = []', 'resume_targets = ["confirm"]')
        graph += """
[[nodes]]
id = "confirm"
kind = "dispatch"
stages = [{ id = "recheck", lenses = ["lens-a.md"], role = "lens" }]
on_done = "finish"
hold_target = "error_hold"
on_verify_invalid = "hold"
on_no_valid = "hold"

[[nodes]]
id = "error_hold"
kind = "hold"
resume_targets = ["finish"]

[[nodes]]
id = "finish"
kind = "emit"
event = "InspectionPassed"
payload_from = "summary"
"""
    policy.write_text(graph)
    manifest=(path/"manifest.toml").read_text()
    entries={name:(path/name).read_bytes() for name in tomllib.loads(manifest)["files"]}
    entries["manifest.toml"]=re.sub(r'^digest = .*\n','',manifest,flags=re.M).encode()
    h=hashlib.sha256()
    for name, content in sorted(entries.items()):
        for part in (name.encode(),content):
            h.update(struct.pack("<Q", len(part)))
            h.update(part)
    digest=h.hexdigest()
    (path/"manifest.toml").write_text(re.sub(r'^digest = .*',f'digest = "{digest}"',manifest,flags=re.M))
    return path,digest

px,x = make_pack("x","first requirement",False)
py,y = make_pack("y","second requirement",True)
registry.write_text(json.dumps([str(py),str(px)]))
assert x != y
events=[]
def command(cmd, opts=None):
    flags={"db":db,"installation-id":"review-test","database-id":"review-db",**(opts or {})}
    argv=[str(binary),cmd]
    for name,value in flags.items():
        if value is True:
            argv.append("--"+name)
        else:
            argv.extend(["--"+name,str(value)])
    return argv

def run(cmd, opts=None, stdin=None, reject=False, expected_code=0):
    argv = command(cmd, opts)
    result=subprocess.run(argv,env=environment,input=stdin,text=True,capture_output=True)
    events.append(dict(argv=argv[1:],code=result.returncode,stdout=result.stdout,stderr=result.stderr))
    (root/"commands.json").write_text(json.dumps(events,indent=2))
    if reject:
        assert (result.returncode,result.stdout,result.stderr)==(1,"","product review request rejected\n"),events[-1]
    else:
        assert result.returncode==expected_code,events[-1]
        if result.stdout.lstrip().startswith("{"):
            return json.loads(result.stdout)
        return result.stdout

def rows(sql,args=()):
    with sqlite3.connect(db.as_uri()+"?mode=ro",uri=True) as conn:
        return conn.execute(sql,args).fetchall()

def state():
    with sqlite3.connect(db.as_uri()+"?mode=ro",uri=True) as conn:
        data=tuple(conn.iterdump())
    inspections={str(p):p.read_bytes() for p in (root/"artifacts").rglob("inspection.json")}
    calls = root / "inspector-calls.jsonl"
    return data, inspections, calls.read_bytes() if calls.exists() else b""

def unchanged(cmd,opts):
    before=state()
    run(cmd,opts,reject=True)
    after = state()
    assert after == before, (cmd, opts)
    logical = lambda state: hashlib.sha256("\n".join(state[0]).encode()).hexdigest()
    with (root / "denials.jsonl").open("a") as report:
        report.write(json.dumps({
            "command_index": len(events) - 1, "command": cmd,
            "database_before": logical(before), "database_after": logical(after),
            "calls_before": len(before[2].splitlines()), "calls_after": len(after[2].splitlines()),
            "artifact_reports": len(after[1]),
        }) + "\n")

password = secrets.token_hex(20)
manifest=root/"bootstrap.json"
manifest.write_text(json.dumps({
    "sources":[{"connector_id":"github-primary","source_instance_id":source,"generation":1,
                "identity":{"organization":"local","provider":"github","api_origin":"https://api.github.com","stable_repository_id":"42"},
                "api_owner":"local","api_repo":"review-test","legacy_repository_locator":"42","scope_query":"open+foundry:make","enabled":True}],
    "grants":[{"actor_id":"owner","source_instance_id":source,"capabilities":["read_order","read_review","resolve_review_hold"]}],
}))
run("product-bootstrap",{"manifest":manifest},"owner\n"+password+"\n")
run("product-login",{"session-file":session},"owner\n"+password+"\n")
viewer=root/"viewer.session"
run("product-account",{"actor":"viewer","role":"viewer","session-file":session},password+"\n")
run("product-login",{"session-file":viewer},"viewer\n"+password+"\n")
bad=root/"invalid.session"
bad.write_text("0"*64)
now=datetime.datetime.now(datetime.timezone.utc)
def configure(service_id,actions):
    common={"session-file":session,"service-id":service_id}
    run("product-service",{**common,"action":"configure","expected-revision":0,"request-key":"configure-"+service_id,"lease-until":(now+datetime.timedelta(hours=2)).isoformat(),"source-instance":source,"actions":actions})
    file=credential if service_id=="review-service" else root/(service_id+".token")
    run("product-service",{**common,"action":"issue","expected-revision":1,"request-key":"issue-"+service_id,"expires-at":(now+datetime.timedelta(hours=1)).isoformat(),"credential-file":file})
    return file
configure("review-service","intake_read,admit_delivery,ack_delivery,drive_review")
read_only=configure("reader","intake_read")
service={"service-file":credential,"connector":"github-primary","repository-id":"42"}
admissions={}
unchanged("product-intake", {
    **service, "service-file": read_only, "order": "order-a", "issue-id": "issue-order-a",
    "commit": commit, "observed-updated-at": now.isoformat(),
})
for order in ("order-a","order-b"):
    admissions[order]=run("product-intake",{**service,"order":order,"issue-id":"issue-"+order,"commit":commit,"observed-updated-at":now.isoformat()})
    assert admissions[order]["order_id"]==order and admissions[order]["lifecycle"]==1
def pack(action,digest,key,rev=0,order=None):
    opts={"session-file":session,"source-instance":source,"digest":digest,"action":action,"expected-revision":rev,"request-key":key}
    if order:
        opts.update(order=order, lifecycle=1)
    return opts
ax=pack("approve",x,"approve-x")
configured_registry = registry.read_bytes()
for invalid_configuration in (["pack-x"], [str(px), str(px)]):
    registry.write_text(json.dumps(invalid_configuration))
    unchanged("product-pack", ax)
registry.write_bytes(configured_registry)
environment["FOUNDRY_REVIEW_PACK"] = str(px)
unchanged("product-pack", ax)
del environment["FOUNDRY_REVIEW_PACK"]
ax_receipt=run("product-pack",ax)
sx=pack("select",x,"select-a",order="order-a")
for changes in ({"session-file":bad},{"session-file":viewer},{"source-instance":"foreign"},{"digest":y},{"digest":"d"*64}):
    unchanged("product-pack",{**sx,**changes})
rx=run("product-pack",sx)
run("product-pack",pack("approve",y,"approve-y"))
run("product-pack",pack("select",x,"initial-b",order="order-b"))
sy=pack("select",y,"select-b",1,"order-b")
ry=run("product-pack",sy)
for changes in ({"expected-revision":0,"request-key":"stale"},{"digest":x}):
    unchanged("product-pack",{**sy,**changes})
assert rx==run("product-pack",sx) and ry==run("product-pack",sy)
assert [rx["revision"],ry["revision"]]==[1,2]
assert all(rows("SELECT COUNT(*) FROM "+t)==[(0,)] for t in ("product_w1_lifecycle_pins_v1","product_review_runs_v1","product_review_stages_v1","product_work_claims_v1","product_review_holds_v1","product_review_host_results_v1"))
def dispatch(order,revision):
    return {**service,"session-file":session,"order":order,"lifecycle":1,"admission-hash":admissions[order]["admission_hash"],"commit":commit,"selection-revision":revision,"request-key":"dispatch-"+order}
if args.race_worker:
    sys.dont_write_bytecode = True
    from selected_review_races import exercise

    def admit_race(order):
        receipt = run("product-intake", {
            **service, "order": order, "issue-id": "issue-" + order, "commit": commit,
            "observed-updated-at": now.isoformat(),
        })
        admissions[order] = receipt
        return receipt, dispatch(order, 1)

    def select_race(order, digest):
        return run("product-pack", pack("select", digest, "select-" + order, order=order))

    exercise(args.race_worker, root, environment, commit, x, y, admit_race, select_race,
             run, rows, state, unchanged)
    raise SystemExit(0)

a=dispatch("order-a",1)
b=dispatch("order-b",2)
def denied_dispatches():
    for changes in ({"order":"order-b"},{"repository-id":"43"},{"session-file":viewer},{"admission-hash":admissions["order-b"]["admission_hash"]},{"service-file":read_only}):
        unchanged("product-review-dispatch",{**a,**changes})
denied_dispatches()
results=[]
with binary.open("rb") as binary_bytes:
    binary_hash = hashlib.file_digest(binary_bytes, "sha256").hexdigest()
registry_hash = hashlib.sha256(registry.read_bytes()).hexdigest()
for opts,digest,path in ((a,x,px),(b,y,py)):
    order=opts["order"]
    result=run("product-review-dispatch",opts)
    assert result["pack_digest"]==digest and result["commit"]==commit
    before=state()
    assert run("product-review-dispatch",opts)==result and before==state()
    unchanged("product-review-dispatch",{**opts,"request-key":"another-key"})
    proof=rows("""SELECT p.pack_digest,r.pack_digest,p.selection_revision,p.referenced_approval_revision,p.claim_epoch,p.claim_fence,w.claim_epoch,w.claim_fence,p.request_key,a.service_id,r.run_id,r.status,r.provenance_json
                  FROM product_w1_lifecycle_pins_v1 p JOIN product_review_runs_v1 r ON r.order_id=p.order_id AND r.product_lifecycle=p.lifecycle
                  JOIN product_review_work_bindings_v1 w ON w.order_id=r.order_id AND w.run_id=r.run_id
                  JOIN product_review_authority_v1 a ON a.order_id=r.order_id AND a.journal_run=r.run_id
                  JOIN product_review_active_v1 active ON active.order_id=r.order_id AND active.journal_run=r.run_id WHERE p.order_id=?""",(order,))
    transitions=rows("SELECT operation FROM product_transition_receipts_v1 WHERE order_id=? ORDER BY seq",(order,))
    assert transitions==[(s,) for s in ("github-admission","commit-review-inspection-start","commit-review-inspection-output","commit-review-graph-outcome")],transitions
    assert rows("SELECT to_state FROM ledger WHERE order_id=? ORDER BY seq DESC LIMIT 1",(order,))==[("waiting",)]
    assert len(proof)==1,proof
    pd,rd,sr,ar,epoch,fence,we,wf,key,svc,run_id,status,prov=proof[0]
    assert (pd,rd,sr,ar,epoch,fence,key,svc)==(digest,digest,opts["selection-revision"],1,we,wf,opts["request-key"],"review-service")
    provenance = json.loads(prov)
    assert provenance["sha256"] == binary_hash and Path(provenance["executable"]) == binary
    assert provenance["pack_digest"] == digest
    assert hashlib.sha256(registry.read_bytes()).hexdigest() == registry_hash
    artifact=root/"artifacts"/hashlib.sha256(run_id.encode()).hexdigest()
    for file in ("manifest.toml","policy.toml","lens-a.md"):
        assert (artifact/"pack-snapshot"/file).read_bytes()==(path/file).read_bytes()
    reports=list(artifact.rglob("inspection.json"))
    assert len(reports)==1,(reports,status)
    inspection=json.loads(reports[0].read_bytes())
    assert inspection==dict(commit=commit,matched=True,blob_sha256=hashlib.sha256((git/"README").read_bytes()).hexdigest(),lens_sha256=hashlib.sha256((path/"lens-a.md").read_bytes()+b"\n").hexdigest())
    results.append(dict(order=order,run_id=run_id,selection_revision=sr,approval_revision=ar,epoch=epoch,fence=fence,status=status,provenance=provenance,registry_sha256=registry_hash,snapshot=str(artifact/"pack-snapshot"),inspection=inspection,result=result))
denied_dispatches()
assert [row["status"] for row in results]==["passed","held"],results
assert len((root/"inspector-calls.jsonl").read_text().splitlines()) == 2
assert results[0]["result"]["hold"] is None and results[1]["result"]["hold"]["reason"]=="end_hold"
(root/"results.json").write_text(json.dumps(results,indent=2))
print(json.dumps(results,indent=2))

decision={"session-file":session,"action":"resume","order":"order-b","hold":results[1]["result"]["hold"]["hold_instance_id"],
          "target":"confirm","idempotency-key":"resume-b"}
# Independently omit the host, supply the other fixed host, omit the selected
# registry entry, and omit only the executor before a new decision.
registry_file = environment.pop("FOUNDRY_REVIEW_PACK_REGISTRY")
unchanged("product-review", decision)
environment["FOUNDRY_REVIEW_PACK"] = str(px)
unchanged("product-review", decision)
del environment["FOUNDRY_REVIEW_PACK"]
environment["FOUNDRY_REVIEW_PACK_REGISTRY"] = registry_file
registry.write_text(json.dumps([str(px)]))
unchanged("product-review",decision)
registry.write_text(json.dumps([str(py),str(px)]))
die=environment.pop("FOUNDRY_DIE_BINARY")
unchanged("product-review",decision)
environment["FOUNDRY_DIE_BINARY"]=die
accepted=run("product-review",{**decision,"accept-only":True})
assert accepted["phase"]=="accepted"
receipt=accepted["receipt"]
assert receipt["order_id"]=="order-b" and receipt["hold_instance_id"]==decision["hold"]
assert rows("SELECT COUNT(*) FROM product_review_stages_v1")==[(2,)]
# After durable acceptance a missing host or persisted snapshot reports the
# original receipt. Each perturbation is restored before the next one.
before=state()
registry.write_text(json.dumps([str(px)]))
unavailable=run("product-review",decision,expected_code=1)
assert unavailable["receipt"]==receipt and unavailable["phase"]=="accepted" and before==state()
registry.write_text(json.dumps([str(py),str(px)]))
snapshot=Path(results[1]["snapshot"])
hidden=snapshot.with_name("retained-snapshot")
snapshot.rename(hidden)
missing=run("product-review",decision,expected_code=1)
assert missing["receipt"]==receipt and missing["phase"]=="accepted" and before==state()
hidden.rename(snapshot)
lens=snapshot/"lens-a.md"
original=lens.read_bytes()
lens.write_bytes(original+b"changed")
tampered=run("product-review",decision,expected_code=1)
assert tampered["receipt"]==receipt and tampered["phase"]=="accepted" and before==state()
lens.write_bytes(original)
# Move the originally held pack to second position before restarting.
registry.write_text(json.dumps([str(px),str(py)]))
(root/"swap-snapshot.json").write_text(json.dumps(str(snapshot.relative_to(root))))
continued=run("product-review",decision)
assert (root/"swapped.json").is_file()
assert (snapshot/"manifest.toml").read_bytes() == (px/"manifest.toml").read_bytes()
snapshot.rename(root/"swapped-in-x")
(root/"saved-snapshot").rename(snapshot)
assert continued["receipt"]==receipt and continued["phase"]=="completed",continued
assert rows("SELECT COUNT(*) FROM product_review_stages_v1")==[(3,)]
assert len((root/"inspector-calls.jsonl").read_text().splitlines()) == 3
followup=rows("SELECT run_id,pack_digest,result_json FROM product_review_runs_v1 WHERE order_id='order-b' AND run_id!=?",(results[1]["run_id"],))
assert len(followup)==1 and followup[0][1]==y,followup
followup_result = json.loads(followup[0][2])
assert followup_result["exit_code"] == 0 and followup_result["held"] is None
new_root=root/"artifacts"/hashlib.sha256(followup[0][0].encode()).hexdigest()
new_inspection=list(new_root.rglob("inspection.json"))
assert len(new_inspection)==1 and json.loads(new_inspection[0].read_bytes())==results[1]["inspection"],new_inspection
before=state()
registry.write_text("[]")
assert run("product-review",decision)==continued and state()==before
registry.write_text(json.dumps([str(py),str(px)]))
(root/"continuation.json").write_text(json.dumps({"acceptance":accepted,"restart":continued,"run":followup[0][0],"pack":y},indent=2))

def extra_order(order, obj, digest):
    admissions[order] = run("product-intake", {
        **service, "order": order, "issue-id": "issue-" + order, "commit": obj,
        "observed-updated-at": now.isoformat()
    })
    selection = run("product-pack", pack("select", digest, "select-" + order, order=order))
    return {**dispatch(order, selection["revision"]), "commit": obj}

def wait_for_marker(name, timeout):
    deadline = time.monotonic() + timeout
    while not (root / name).exists():
        if time.monotonic() >= deadline:
            raise AssertionError("barrier timed out: " + name)
        time.sleep(0.025)

def interrupt_reserved_stage(cmd, options, receipt=None):
    for marker in ("inspector-entered", "release-inspector", "inspector-exited"):
        (root / marker).unlink(missing_ok=True)
    (root / "pause-inspector").touch()
    argv = command(cmd, options)
    with subprocess.Popen(argv, env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) as child:
        try:
            wait_for_marker("inspector-entered", 30)
            if receipt is not None:
                assert rows("SELECT phase FROM product_review_hold_progress_v1 WHERE operation_id=?",
                            (receipt["operation_id"],)) == [("continuing",)]
            assert rows("SELECT COUNT(*) FROM product_review_stages_v1 WHERE order_id=? AND status='accepted'",
                        (options["order"],)) == [(1,)]
            child.kill()
            killed_status = child.wait(timeout=10)
            assert killed_status < 0
            interruption = {"argv": argv[1:], "pid": child.pid, "exit": killed_status, "reserved_stage_count": 1}
        finally:
            (root / "release-inspector").touch()
            if child.poll() is None:
                child.kill()
    wait_for_marker("inspector-exited", 10)
    (root / "pause-inspector").unlink()
    return interruption

loss = extra_order("order-e", commit, y)
loss_hold = run("product-review-dispatch", loss)["hold"]["hold_instance_id"]
loss_decision = {**decision, "order": "order-e", "hold": loss_hold, "idempotency-key": "resume-e"}
loss_accepted = run("product-review", {**loss_decision, "accept-only": True})
assert loss_accepted["phase"] == "accepted"
killed_process = interrupt_reserved_stage("product-review", loss_decision, loss_accepted["receipt"])
before_calls = (root / "inspector-calls.jsonl").read_bytes()
recovery = run("product-review", loss_decision, expected_code=1)
assert recovery["phase"] == "recovery_required" and recovery["receipt"] == loss_accepted["receipt"], recovery
assert (root / "inspector-calls.jsonl").read_bytes() == before_calls
registry.write_text("[]")
before = state()
assert run("product-review", loss_decision, expected_code=1) == recovery and state() == before
registry.write_text(json.dumps([str(py), str(px)]))
(root / "process-loss.json").write_text(json.dumps({"accepted": loss_accepted, "interruption": killed_process, "retry": recovery}, indent=2))

interrupted = extra_order("order-i", commit, x)
interruption = interrupt_reserved_stage("product-review-dispatch", interrupted)
assert rows("SELECT status FROM product_review_runs_v1 WHERE order_id='order-i'") == [("running",)]
assert rows("SELECT COUNT(*) FROM product_review_host_results_v1 WHERE order_id='order-i'") == [(0,)]
unchanged("product-review-dispatch", interrupted)
(root / "initial-stage-loss.json").write_text(json.dumps(interruption, indent=2))

failed = extra_order("order-f", commit, x)
(root / "fail-inspector").touch()
run("product-review-dispatch", failed, reject=True)
(root / "fail-inspector").unlink()
assert rows("SELECT status FROM product_review_runs_v1 WHERE order_id='order-f'") == [("failed",)]
assert rows("SELECT operation FROM product_transition_receipts_v1 WHERE order_id='order-f' ORDER BY seq") == [
    ("github-admission",), ("commit-review-inspection-start",), ("commit-review-inspection-failed",),
]
assert rows("SELECT COUNT(*) FROM product_review_failures_v1 WHERE order_id='order-f'") == [(1,)]
for table in ("product_review_holds_v1", "product_review_host_results_v1", "product_artifacts_v1"):
    assert rows("SELECT COUNT(*) FROM " + table + " WHERE order_id='order-f'") == [(0,)]
unchanged("product-review-dispatch", failed)

missing_commit = extra_order("order-d", "0" * 40, x)
unchanged("product-review-dispatch", missing_commit)
live_abort = extra_order("order-g", commit, y)
live_hold = run("product-review-dispatch", live_abort)["hold"]["hold_instance_id"]
stages_before_abort = rows("SELECT COUNT(*) FROM product_review_stages_v1")
live_stopped = run("product-review", {
    "session-file": session, "order": "order-g", "action": "abort",
    "hold": live_hold, "idempotency-key": "abort-g",
})
assert live_stopped["phase"] == "completed", live_stopped
assert rows("SELECT COUNT(*) FROM product_review_stages_v1") == stages_before_abort

revoked_order = extra_order("order-revoked", commit, y)
revoked_hold = run("product-review-dispatch", revoked_order)["hold"]["hold_instance_id"]
third = extra_order("order-c", commit, y)
third_hold = run("product-review-dispatch", third)["hold"]["hold_instance_id"]
new_resume = {**decision, "order": "order-c", "hold": third_hold, "idempotency-key": "resume-c"}

def assert_aborted(order, hold, key):
    opts = {"session-file": session, "order": order, "hold": hold,
            "action": "abort", "idempotency-key": key}
    pins = rows("SELECT * FROM product_w1_lifecycle_pins_v1 ORDER BY order_id")
    stages = rows("SELECT * FROM product_review_stages_v1 ORDER BY order_id,run_id,sequence")
    _, inspections, calls = state()
    stopped = run("product-review", opts)
    assert stopped["phase"] == "completed", stopped
    assert stopped["receipt"]["decision"] == {"kind": "abort"}
    assert rows("SELECT status FROM product_review_runs_v1 WHERE order_id=?", (order,)) == [("review_aborted",)]
    assert rows("SELECT * FROM product_w1_lifecycle_pins_v1 ORDER BY order_id") == pins
    assert rows("SELECT * FROM product_review_stages_v1 ORDER BY order_id,run_id,sequence") == stages
    before = state()
    assert before[1:] == (inspections, calls)
    assert run("product-review", opts) == stopped and before == state()
    for table in ("product_grants_v1", "product_approval_result_bindings_v1"):
        assert rows("SELECT COUNT(*) FROM " + table) == [(0,)]
    return stopped

# An approval ABA cannot reinterpret the retained selection or create new work.
old_selection = run("product-pack", sy)
old_approval = run("product-pack", pack("approve", y, "approve-y"))
revoked = run("product-pack", pack("revoke", y, "revoke-y", 1))
assert revoked["revision"] == 2
unchanged("product-review", new_resume)
unchanged("product-review-dispatch", {**b, "request-key": "after-revoke"})
while_revoked = assert_aborted("order-revoked", revoked_hold, "abort-revoked")
reapproved = run("product-pack", pack("approve", y, "reapprove-y", 2))
assert reapproved["revision"] == 3
unchanged("product-review", new_resume)
unchanged("product-review-dispatch", {**b, "request-key": "after-aba"})
before = state()
assert run("product-pack", sy) == old_selection
assert run("product-pack", pack("approve", y, "approve-y")) == old_approval
assert run("product-review", decision) == continued
assert state() == before
stopped = assert_aborted("order-c", third_hold, "abort-c")
(root/"approval-aba.json").write_text(json.dumps({
    "original_selection": old_selection, "original_approval": old_approval,
    "revoked": revoked, "reapproved": reapproved, "accepted_decision": receipt, "abort_with_live_authority": live_stopped,
    "abort_while_revoked": while_revoked, "abort_after_aba": stopped,
    "frozen_abort_completion_met": stopped["phase"] == "completed"
}, indent=2))
