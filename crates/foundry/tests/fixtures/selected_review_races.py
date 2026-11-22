"""Crash only the two ProductStore operands started for each private order."""
import hashlib
import json
import subprocess
import time


def barrier(child, path):
    deadline = time.monotonic() + 40
    while not path.exists():
        if child.poll() is not None or time.monotonic() >= deadline:
            raise AssertionError(f"operand stopped at {path}; see {path.parent / 'output.log'}")
        time.sleep(0.01)


def launch(worker, directory, spec, environment):
    directory.mkdir(mode=0o700)
    (directory / "job.json").write_text(json.dumps(spec))
    with (directory / "output.log").open("w") as log:
        return subprocess.Popen(
            [worker, "--exact", "first_pin_across_processes", "--nocapture"],
            env={**environment, "FOUNDRY_W1_RACE_OPERAND": str(directory)},
            stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
        )


def response(child, directory, turn):
    file = directory / f"reply-{turn}"
    barrier(child, file)
    return json.loads(file.read_bytes())


def fingerprint(state):
    return {
        "database": hashlib.sha256("\n".join(state[0]).encode()).hexdigest(),
        "calls": len(state[2].splitlines()),
    }


def exercise(worker, root, environment, commit, x, y, admit, select, run, rows, state, unchanged):
    observations = []
    for first in ("admission", "selection"):
        for boundary in ("before", "after"):
            order = f"race-{first}-{boundary}"
            receipt, dispatch = admit(order)
            select(order, x)
            jobs = {}
            try:
                # Both connections (and the original scoped X claim) are ready
                # before either writer takes the commit lock.
                for role in ("admission", "selection"):
                    directory = root / (order + "-" + role)
                    process = launch(worker, directory, {
                        "root": str(root), "order": order, "commit": commit,
                        "admission": receipt["admission_hash"], "admission_operand": role == "admission",
                        "hold_commit": role == first,
                    }, environment)
                    jobs[role] = (process, directory)
                    barrier(process, directory / "ready")

                winner, winner_dir = jobs[first]
                loser_role = "selection" if first == "admission" else "admission"
                loser, loser_dir = jobs[loser_role]
                initial = state()
                (winner_dir / "go-1").touch()
                barrier(winner, winner_dir / "before-commit")
                assert state() == initial
                (loser_dir / "go-1").touch()
                busy = response(loser, loser_dir, 1)
                assert busy["status"] == "busy", busy
                assert state() == initial

                committed = None
                if boundary == "after":
                    (winner_dir / "release").touch()
                    committed = response(winner, winner_dir, 1)
                    assert committed["status"] == "committed", committed
                winner.kill()
                assert winner.wait(timeout=5) < 0
                reopened = state()
                assert (initial == reopened) == (boundary == "before")
                (loser_dir / "go-2").touch()
                retried = response(loser, loser_dir, 2)
                assert loser.wait(timeout=5) == 0
                if boundary == "after":
                    assert retried["status"] == "denied", retried
                    reason = ("w1 selection blocked by review" if loser_role == "selection"
                              else "selected review activation binding is stale")
                    assert reason in retried["error"], retried
                    assert state() == reopened
                    winning_role = first
                else:
                    assert retried["status"] == "committed", retried
                    winning_role = loser_role
                winning_digest = x if winning_role == "admission" else y

                if winning_role == "selection":
                    assert rows("SELECT COUNT(*) FROM product_w1_lifecycle_pins_v1 WHERE order_id=?", (order,)) == [(0,)]
                    assert rows("SELECT COUNT(*) FROM product_review_runs_v1 WHERE order_id=?", (order,)) == [(0,)]
                    new_dispatch = {**dispatch, "selection-revision": 2, "request-key": "new-" + order}
                    result = run("product-review-dispatch", new_dispatch)
                    assert result["pack_digest"] == y and result["commit"] == commit, result
                    stable = state()
                    assert run("product-review-dispatch", new_dispatch) == result and state() == stable
                    key = new_dispatch["request-key"]
                    revision = 2
                else:
                    key = "admit-" + order
                    revision = 1
                proof = rows("""SELECT p.pack_digest,p.request_key,p.selection_revision,r.run_id,r.status,p.claim_epoch,p.claim_fence
                    FROM product_w1_lifecycle_pins_v1 p
                    JOIN product_review_runs_v1 r ON r.order_id=p.order_id AND r.product_lifecycle=p.lifecycle AND r.pack_digest=p.pack_digest
                      AND r.admission_hash=p.admission_hash AND r.admission_seq=p.admission_seq
                    JOIN product_review_work_bindings_v1 w ON w.order_id=r.order_id AND w.run_id=r.run_id AND w.claim_fence=p.claim_fence
                      AND w.claim_epoch=p.claim_epoch AND w.payload_digest=p.payload_digest
                    JOIN product_review_authority_v1 a ON a.order_id=r.order_id AND a.journal_run=r.run_id AND a.service_id=p.service_id
                    JOIN product_review_active_v1 h ON h.order_id=r.order_id AND h.journal_run=r.run_id
                    JOIN product_review_host_accept_v1 accepted ON accepted.order_id=r.order_id AND accepted.attempt=1
                      AND accepted.run_id=1 AND accepted.activation=1
                    WHERE p.order_id=?""", (order,))
                assert len(proof) == 1, proof
                assert proof[0][:5] == (winning_digest, key, revision, order + ":1:1:1",
                                        "running" if winning_role == "admission" else "held"), proof
                assert rows("SELECT COUNT(*) FROM product_review_stages_v1 WHERE order_id=?", (order,)) == [
                    (0 if winning_role == "admission" else 1,)
                ]
                if winning_role == "admission":
                    exact = {**dispatch, "request-key": key}
                    result = run("product-review-dispatch", exact)
                    assert result["pack_digest"] == x and result["exit_code"] == 0, result
                    assert rows("SELECT COUNT(*) FROM product_review_stages_v1 WHERE order_id=?", (order,)) == [(1,)]
                    stable = state()
                    assert run("product-review-dispatch", exact) == result and state() == stable
                unchanged("product-review-dispatch", {**dispatch, "selection-revision": revision, "request-key": "alias-" + key})
                observations.append({
                    "order": order, "first_writer": first, "kill": boundary + "_commit",
                    "writer_pid": winner.pid, "contender_pid": loser.pid, "busy": busy, "retry": retried,
                    "winning_api_return": committed, "before": fingerprint(initial), "reopened": fingerprint(reopened),
                    "winning_role": winning_role, "pin_run_and_fence": proof[0],
                })
                (root / "races.json").write_text(json.dumps(observations, indent=2))
            finally:
                for child, _ in jobs.values():
                    if child.poll() is None:
                        child.kill()
                    child.wait(timeout=5)

    # The target order is independently authorized, selected and still
    # unclaimed. Only its service request key aliases another order.
    alias_order = "race-key-control"
    _, eligible = admit(alias_order)
    select(alias_order, x)
    previous_key = observations[1]["pin_run_and_fence"][1]
    unchanged("product-review-dispatch", {**eligible, "request-key": previous_key})
    assert rows("SELECT COUNT(*) FROM product_work_claims_v1 WHERE order_id=?", (alias_order,)) == [(0,)]
    control = run("product-review-dispatch", eligible)
    assert control["pack_digest"] == x and control["exit_code"] == 0
    (root / "alias-control.json").write_text(json.dumps({
        "order": alias_order, "rejected_foreign_key": previous_key, "authorized_result": control
    }, indent=2))
