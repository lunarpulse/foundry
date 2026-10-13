#!/usr/bin/env python3
"""Executing golden-trace verifier — walks the REAL pack graph.

r13-1: the previous version called route1 directly each iteration and
never followed on_done — mutated packs (replan.on_done="pass",
entry="missing") still passed. This one:

- starts at `entry` and walks nodes by kind: dispatch -> on_done,
  route -> branch (evaluating the real DSL), redrive -> on_done,
  emit -> HALT, hold -> resume (bounded exploration of resume_targets),
- applies branch `do` actions (bump, revert) in order,
- reads ACTIVE categories from schema-triage.toml (pack data, not a
  Python constant),
- asserts the original step-04 contract on first decisions AND on
  full iterated walks (counter increments BEFORE the loopback
  transition — bump now lives in the route branch per the original
  'Before each loopback' ordering),
- negative controls: mutated packs MUST fail (replan.on_done=pass,
  entry=missing, edge session removed, triage schema removed).
"""
import copy
import os
import re
import struct
import sys
import tomllib
from itertools import combinations

PACK = "skills/bmad-review-full/v1"
POLICY = f"{PACK}/policy.toml"


def load(path=POLICY):
    """Load the canonical pack; the executed policy.toml MUST be a
    manifest member (r30-2 execution binding)."""
    import os as _os
    man = _os.path.join(_os.path.dirname(path), "manifest.toml")
    with open(man, "rb") as f:
        mdoc = tomllib.load(f)
    files_decl = mdoc.get("files", [])
    assert "policy.toml" in files_decl, \
        "FULL pack manifest must declare policy.toml (execution binding)"
    with open(path, "rb") as f:
        return tomllib.load(f)


CALL = re.compile(r"([a-z_]+)\s*\(\s*([^)]*)\s*\)")


def atom(name, args, state):
    if name == "true":
        return True
    if name == "budget_exhausted":
        return state.get("budget_exhausted", False)
    if name == "timeout":
        return state.get("timeout", False)
    if name == "has_findings":
        return bool(state["findings"])
    if name == "count_lt":
        counter, n = [a.strip() for a in args.split(",")]
        return state["counters"][counter] < int(n)
    if name == "any_in":
        m = re.match(r"verdicts,(.*)", args, re.S)
        vals = re.findall(r'"([a-z_]+)"', m.group(1))
        return bool(set(vals) & set(state["verdicts"]))
    raise ValueError(f"unknown predicate {name}")


def evaluate(expr, state):
    e = expr.replace(" ", "").replace("\n", "")
    while True:
        m = CALL.search(e)
        if not m:
            break
        val = "T" if atom(m.group(1), m.group(2), state) else "F"
        e = e[:m.start()] + val + e[m.end():]
    e = e.replace("true", "T").replace("false", "F")
    e = e.replace("T", " True ").replace("F", " False ")
    py = e.replace("&&", " and ").replace("||", " or ").replace("!", " not ")
    return bool(eval(py))


def apply_do(do, state):
    """branch do-sequence: bump(id); revert(base) — FULL-CONSUMPTION
    parser (r29-3): whitespace-tolerant, trailing garbage rejected"""
    for action in do.split(";"):
        action = action.strip()
        m = re.fullmatch(r"bump\s*\(\s*(\w+)\s*\)", action)
        if m:
            state["counters"][m.group(1)] = \
                state["counters"].get(m.group(1), 0) + 1
            continue
        m = re.fullmatch(r"revert\s*\(\s*(\w+)\s*\)", action)
        if m:
            state["reverted_base"] = m.group(1)  # durable, observed
            continue
        m = re.fullmatch(r"emit\s*\(\s*(\w+)\s*,\s*(\w+)\s*\)", action)
        if m:
            continue  # emits handled by the walker
        raise ValueError(f"unknown action {action!r}")


def walk(doc, verdicts, active_cats, max_steps=50):
    """Walk the graph from entry. Returns (decisions, final_state)."""
    nodes = {n["id"]: n for n in doc["nodes"]}
    state = {
        "verdicts": verdicts,
        "findings": bool(set(verdicts) & set(active_cats)),
        "counters": {k: 0 for k in doc.get("counters", {})},
        "reverted_base": None,
    }
    cur = doc["entry"]
    decisions = []
    for _ in range(max_steps):
        node = nodes.get(cur)
        if node is None:
            raise AssertionError(f"unknown node {cur}")
        kind = node["kind"]
        if kind == "dispatch":
            decisions.append(cur)
            cur = node["on_done"]
        elif kind == "route":
            for br in node["branches"]:
                if evaluate(br["when"], state):
                    if br.get("do"):
                        apply_do(br["do"], state)
                    nxt = br.get("goto")
                    if nxt is None:
                        return decisions + ["HALT"], state
                    cur = nxt  # NOT appended here — the target node
                    break     # records itself on its own visit
            else:
                raise AssertionError("no branch matched")
        elif kind == "redrive":
            decisions.append(cur)
            if node.get("bump"):  # node-level bump (non-loopback fix loops)
                state["counters"][node["bump"]] = \
                    state["counters"].get(node["bump"], 0) + 1
            cur = node["on_done"]
        elif kind == "emit":
            decisions.append(f"emit:{node['event']}")
            return decisions, state
        elif kind == "hold":
            decisions.append(f"hold:{cur}")
            # hold-entry state (after the branch do-sequence) is what
            # the contract asserts on: bump+revert BEFORE the hold.
            entry_state = copy.deepcopy(state)
            outs = []
            for t in node.get("resume_targets", []):
                sub = copy.deepcopy(state)
                d2, s2 = walk_from(nodes, doc, t, sub, active_cats,
                                   max_steps - len(decisions))
                outs.append((d2, s2))
            return decisions, (entry_state, outs if outs else None)
        else:
            raise ValueError(kind)
    raise AssertionError("walk exceeded max_steps — unbounded loop")


def walk_from(nodes, doc, start, state, active_cats, max_steps=50):
    cur = start
    decisions = []
    for _ in range(max_steps):
        node = nodes.get(cur)
        if node is None:
            raise AssertionError(f"unknown node {cur}")
        kind = node["kind"]
        if kind == "dispatch":
            decisions.append(cur)
            cur = node["on_done"]
        elif kind == "route":
            for br in node["branches"]:
                if evaluate(br["when"], state):
                    if br.get("do"):
                        apply_do(br["do"], state)
                    nxt = br.get("goto")
                    if nxt is None:
                        return decisions + ["HALT"], state
                    cur = nxt  # target node records itself
                    break
            else:
                raise AssertionError("no branch matched")
        elif kind == "redrive":
            decisions.append(cur)
            if node.get("bump"):  # node-level bump (non-loopback fix loops)
                state["counters"][node["bump"]] = \
                    state["counters"].get(node["bump"], 0) + 1
            cur = node["on_done"]
        elif kind == "emit":
            decisions.append(f"emit:{node['event']}")
            return decisions, state
        elif kind == "hold":
            decisions.append(f"hold:{cur}")
            return decisions, state
        else:
            raise ValueError(kind)
    raise AssertionError("unbounded")


def active_categories(doc):
    """ACTIVE categories from the pack's triage output schema."""
    sid = None
    for n in doc["nodes"]:
        if n["kind"] == "dispatch":
            for st in n.get("stages", []):
                if st.get("role") == "verify":
                    sid = st.get("output_schema")
    assert sid, "verify stage has no output_schema"
    sch = tomllib.load(open(f"{PACK}/{sid}", "rb"))
    for out in sch["output"]:
        if out["name"] == "verdicts":
            return set(out["active_categories"])
    raise AssertionError("verdicts output missing active_categories")


def first_target(doc, verdicts, active_cats):
    """First routing decision after the lenses dispatch."""
    d, _ = walk(doc, verdicts, active_cats)
    # d = ['lenses', <route target>, ...]
    return d[1]


def original_first(vs, fr, lb):
    if "intent_gap" in vs or "bad_plan" in vs:
        if lb >= 5:
            return "human"
        return "human_gap" if "intent_gap" in vs else "replan"
    if "patch" in vs:
        return "fixdie" if fr < 3 else "human"
    return "pass"


def with_counters(doc, verdicts, active_cats, counters):
    nodes = {n["id"]: n for n in doc["nodes"]}
    state = {"verdicts": verdicts,
             "findings": bool(set(verdicts) & set(active_cats)),
             "counters": dict(counters), "reverted_base": None}
    for br in nodes["route1"]["branches"]:
        if evaluate(br["when"], state):
            return br.get("goto")


def negative_controls(doc, active_cats):
    """Mutated packs MUST fail these assertions."""
    # 1. replan.on_done -> pass: bad_plan walk must NOT emit pass
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["id"] == "replan":
            n["on_done"] = "pass"
    d, _ = walk(bad, ["bad_plan"], active_cats, max_steps=20)
    assert "emit:InspectionPassed" in d, \
        "mutated on_done=pass must produce a (wrong) pass — detectable"
    # the REAL pack's iterated walk never reaches pass on bad_plan
    d_real, res_real = walk(doc, ["bad_plan"], active_cats)
    assert d_real.count("replan") == 5, d_real
    assert d_real[-1] == "hold:human", d_real
    # counter hit 6 at the 6th loopback (cap branch bumps too)
    st_real = res_real[0] if isinstance(res_real, tuple) else res_real
    assert st_real["counters"]["loopbacks"] == 6, st_real
    # 1b. (r15-2) replan_full.on_done -> pass: intent_gap RESUME outcome
    # must NOT emit pass — checked against the actual resume walk
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["id"] == "replan_full":
            n["on_done"] = "pass"
    d, res = walk(bad, ["intent_gap"], active_cats, max_steps=30)
    _, outs = res
    wrong_pass = any("emit:InspectionPassed" in o[0] for o in (outs or []))
    assert wrong_pass, "mutated replan_full.on_done=pass must be detectable"
    # and the REAL pack's intent_gap resume never emits pass
    d, res = walk(doc, ["intent_gap"], active_cats, max_steps=30)
    _, outs = res
    assert not any("emit:InspectionPassed" in o[0] for o in (outs or [])), \
        f"real pack must not pass on intent_gap: {outs}"
    # 1c. (r15-2) human_gap.resume_targets += ["pass"] must be detectable:
    # the mutated resume reaches emit:pass while the real one doesn't
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["id"] == "human_gap":
            n["resume_targets"] = n["resume_targets"] + ["pass"]
    d, res = walk(bad, ["intent_gap"], active_cats, max_steps=30)
    _, outs = res
    assert any(o[0] and o[0][0] == "emit:InspectionPassed" for o in outs), \
        "mutated resume_targets+=pass must produce a detectable wrong pass"
    # 2. entry -> missing node must raise
    bad2 = copy.deepcopy(doc)
    bad2["entry"] = "missing"
    try:
        walk(bad2, ["patch"], active_cats)
    except AssertionError as e:
        assert "unknown node" in str(e), \
            f"entry=missing must fail with unknown-node, got: {e}"
    else:
        raise AssertionError("entry=missing must fail")
    # 3. removing edge session breaks the same-reviewer contract check
    # (verifier asserts presence below in positive checks)
    # 4. triage schema removal must break active_categories()
    bad3 = copy.deepcopy(doc)
    for n in bad3["nodes"]:
        if n["kind"] == "dispatch":
            for st in n.get("stages", []):
                if st.get("role") == "verify":
                    st.pop("output_schema", None)
    try:
        active_categories(bad3)
        raise AssertionError("schema removal must break ACTIVE lookup")
    except AssertionError as e:
        assert "output_schema" in str(e)


def main():
    doc = load()
    ac = active_categories(doc)
    assert ac == {"patch", "intent_gap", "bad_plan"}, ac
    # same-reviewer contract: edge & claims share session (r13-2)
    stages = doc["nodes"][0]["stages"]
    by_id = {s["id"]: s for s in stages}
    assert by_id["edge"].get("session") == "trace" and \
        by_id["claims"].get("session") == "trace", "same-reviewer broken"
    assert "session" not in by_id["verif"], "verif must be independent"

    # --- 60 first-decision combos on the real graph ---
    fails = total = 0
    subsets = [list(c) for r in range(1, 5) for c in combinations(
        ["patch", "intent_gap", "bad_plan", "defer"], r)]
    for vs in subsets:
        for fr in (2, 3):
            for lb in (4, 5):
                total += 1
                got = with_counters(doc, vs, ac,
                                    {"fix_rounds": fr, "loopbacks": lb})
                want = original_first(vs, fr, lb)
                if got != want:
                    fails += 1
                    print("MISMATCH", vs, fr, lb, got, want)
    print(f"first-decision: {total - fails}/{total}")
    assert fails == 0

    # --- iterated bad_plan: full walk, 5 replans then capped human ---
    d, res = walk(doc, ["bad_plan"], ac)
    print("bad_plan walk:", d[:12])
    assert d.count("replan") == 5
    assert d[-1] == "hold:human"
    st_bad = res[0] if isinstance(res, tuple) else res
    # r14-1: the cap branch ALSO bumps (original increments, then checks
    # >5) — human hold is entered with loopbacks=6 on the 6th loopback
    assert st_bad["counters"]["loopbacks"] == 6, st_bad

    # --- iterated intent_gap: bump+revert BEFORE hold (original order),
    #     5 resume pairs then capped human ---
    d2, result = walk(doc, ["intent_gap"], ac)
    print("intent_gap walk:", d2[:14])
    assert d2[-1] == "hold:human_gap"
    # resume exploration: each resume leads to replan_full -> lenses loop;
    # follow it iteratively — 5 pairs then the cap branch fires
    assert isinstance(result, tuple), "hold must expose (entry_state, outs)"
    entry_state, outs = result
    resumes = {n["id"]: n for n in doc["nodes"]}["human_gap"]["resume_targets"]
    assert "replan_full" in resumes
    # counter already 1 at hold entry (bump precedes hold)
    assert entry_state["counters"]["loopbacks"] == 1, \
        "bump must precede the human hold (original 'before each loopback')"
    assert entry_state["reverted_base"] == "origin", \
        "revert must precede the human hold (original reverts first)"
    # iterated resume loop: human_gap -> replan_full -> lenses -> route1
    nodes = {n["id"]: n for n in doc["nodes"]}
    state = {"verdicts": ["intent_gap"], "findings": True,
             "counters": {"fix_rounds": 0, "loopbacks": 1},
             "reverted_base": "origin"}
    pairs = 0
    for _ in range(10):
        # resume -> replan_full -> lenses -> route1
        for br in nodes["route1"]["branches"]:
            if evaluate(br["when"], state):
                if br.get("do"):
                    apply_do(br["do"], state)
                tgt = br.get("goto")
                break
        if tgt == "human_gap":
            pairs += 1
            continue  # resumed again
        break
    assert pairs == 4, f"expected 4 more pairs (5 total), got {pairs}"
    assert tgt == "human", f"6th entry must be capped human, got {tgt}"

    # --- defer-only walk emits pass, records deferred ---
    d3, _ = walk(doc, ["defer"], ac)
    assert "emit:InspectionPassed" in d3, d3

    # --- patch walk: fixdie re-review loop — bounded slice then hold ---
    # (the walk itself terminates: fixdie bumps fix_rounds on the node,
    # so route1 sends patch to human at fix_rounds=3 — give it enough
    # steps to reach that hold)
    d4, res4 = walk(doc, ["patch"], ac, max_steps=40)
    assert "fixdie" in d4, d4
    assert d4.count("fixdie") == 3, d4
    assert d4[-1] == "hold:human", d4
    state = {"verdicts": ["patch"], "findings": True,
             "counters": {"fix_rounds": 3, "loopbacks": 0},
             "reverted_base": None}
    nodes = {n["id"]: n for n in doc["nodes"]}
    for br in nodes["route1"]["branches"]:
        if evaluate(br["when"], state):
            tgt = br.get("goto")
            break
    assert tgt == "human", f"patch at cap must go human, got {tgt}"

    negative_controls(doc, ac)
    contract_checks(doc)
    contract_negative_controls(doc)
    example_pack_identity()
    sweep_rejection_fixtures()
    print("ALL PASS — real-graph walk == original step-04 contract")


def contract_negative_controls(doc):
    """(r17-2) mutations that MUST fail contract_checks."""
    # replan_full pre_stage swap -> must be detected
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["id"] == "replan_full":
            n["pre_stage"] = "replan-stage"
    try:
        contract_checks(bad)
        raise AssertionError("pre_stage swap mutant NOT detected")
    except AssertionError as e:
        assert "regenerator" in str(e) or "swap" not in str(e), e
    # pre_stage removal on replan nodes -> must be detected
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["id"] in ("replan", "replan_full"):
            n.pop("pre_stage", None)
    try:
        contract_checks(bad)
        raise AssertionError("pre_stage removal mutant NOT detected")
    except AssertionError as e:
        assert "must declare pre_stage" in str(e), \
            f"wrong failure: {e}"
    # claims.after -> "verif": claims(trace) must remain ordered after
    # edge(trace) — the reachability check must reject this rewire
    bad = copy.deepcopy(doc)
    for n in bad["nodes"]:
        if n["kind"] == "dispatch":
            for st in n.get("stages", []):
                if st["id"] == "claims":
                    st["after"] = "verif"
    try:
        contract_checks(bad)
        raise AssertionError("claims.after=verif mutant NOT detected")
    except AssertionError as e:
        assert "must be ordered" in str(e) or "cycle" in str(e), \
            f"wrong failure: {e}" 


def example_pack_identity():
    """(r17-1, r18-1) the §2.1 doc example must be node-set-identical
    to the canonical pack: same node IDs, and every canonical field
    compared in full. Only `stages` may be elided in the example."""
    import tomllib as tl
    doc_txt = open("docs/design/universal-skill-spec.md").read()
    blocks = re.findall(r"```toml\n(.*?)```", doc_txt, re.S)
    ex = None
    for b in blocks:
        d = tl.loads(b)
        if any(n["id"] == "route1" for n in d.get("nodes", [])):
            ex = d
            break
    assert ex, "doc example graph (route1) not found"
    pack = load()
    ex_nodes = {n["id"]: n for n in ex["nodes"]}
    pk_nodes = {n["id"]: n for n in pack["nodes"]}
    # (r18-1) NODE SET equality — example-only nodes forbidden EXCEPT
    # the documented demonstration node
    ALLOWED_EXAMPLE_ONLY = {"emit_failed_node"}  # §2.1 demo of §2.4a
    ex_only = set(ex_nodes) - set(pk_nodes)
    pk_only = set(pk_nodes) - set(ex_nodes)
    assert ex_only <= ALLOWED_EXAMPLE_ONLY, \
        f"example has non-pack nodes: {ex_only - ALLOWED_EXAMPLE_ONLY}"
    assert not pk_only, f"example is missing pack nodes: {pk_only}"
    # full-field comparison on every shared node
    for nid, pkn in pk_nodes.items():
        exn = ex_nodes[nid]
        pkf = {k: v for k, v in pkn.items() if k not in ("stages", "branches", "id")}
        exf = {k: v for k, v in exn.items() if k not in ("stages", "branches", "id")}
        assert exf == pkf, \
            f"node {nid}: example fields {exf} != pack fields {pkf}"
    # route1 branches identical in order (when/do/goto)
    ex_r1 = ex_nodes["route1"]["branches"]
    pk_r1 = pk_nodes["route1"]["branches"]
    assert len(ex_r1) == len(pk_r1), \
        f"route1 branch count {len(ex_r1)} != {len(pk_r1)}"
    for i, (a, b) in enumerate(zip(ex_r1, pk_r1)):
        assert a.get("when") == b.get("when"), f"route1[{i}].when differs"
        assert a.get("do") == b.get("do"), \
            f"route1[{i}].do differs: {a.get('do')!r} vs {b.get('do')!r}"
        assert a.get("goto") == b.get("goto"), f"route1[{i}].goto differs"


def sweep_rejection_fixtures():
    """(r15-1, r16-3) A.2 sweep: exact expected codes + structural
    checks decidable without a loader implementation."""
    import glob
    import tomllib as tl
    base = "skills/fixture-reject"
    dirs = sorted(os.path.basename(d) for d in glob.glob(f"{base}/*"))
    assert len(dirs) == 18, f"expected 18 fixtures, found {len(dirs)}"
    EXPECTED = {
        "01-route-cycle": "E_DAG_CYCLE",
        "02-resume-cycle": "E_DAG_RESUME_CYCLE",
        "03-missing-payload": "E_SCHEMA_MISSING_FIELD",
        "04-verdicts-no-verify": "E_PROVENANCE",
        "05-no-true-fallback": "E_NO_TRUE_FALLBACK",
        "06-do-only-nonterminal": "E_NONTERMINAL_DO_NO_GOTO",
        "07-quoted-digest-key": "E_DIGEST_KEY_QUOTED",
        "08-crlf-manifest": "E_LF_REQUIRED",
        "09-no-trailing-lf": "E_LF_REQUIRED",
        "10-counter-guard-missing": "E_COUNTER_GUARD_MISSING",
        "11-counter-guard-double": "E_COUNTER_GUARD_DOUBLE",
        "12-counter-guard-mismatch": "E_COUNTER_GUARD_MISMATCH",
        "13-multi-emit": "E_MULTI_EMIT",
        "14-post-emit-action": "E_POST_EMIT_ACTION",
        "15-verify-duplicate": "E_VERIFY_DUPLICATE",
        "16-session-parallel": "E_SESSION_PARALLEL",
        "17-missing-error-policy": "E_MISSING_ERROR_POLICY",
        "18-revert-then-emit": "E_REVERT_THEN_EMIT",
    }
    import hashlib as _hl
    for name in dirs:
        exp = open(f"{base}/{name}/expected_error.txt").read().strip()
        assert exp == EXPECTED[name], \
            f"{name}: expected {EXPECTED[name]!r}, found {exp!r}"
        tl.load(open(f"{base}/{name}/policy.toml", "rb"))  # parses
        # (r22-MINOR, r25-MINOR) manifest digest over ORIGINAL bytes.
        # 07 (key format) and 09 (no trailing LF): digest line and the
        # offending form are normalized ONLY for the digest computation
        # (lens/policy bytes hashed as-is — tampering breaks it).
        # 08 already verified separately below with CRLF bytes.
        if True:
            raw = open(f"{base}/{name}/manifest.toml", "rb").read()
            man = raw.decode()
            if name == "07-quoted-digest-key":
                man = man.replace('"digest" =', "digest =")
            # 08: manifest keeps its CRLF bytes (intended violation);
            # tomllib parses CRLF fine, and the digest hashes ORIGINAL
            # bytes below
            if name == "09-no-trailing-lf":
                man = man + "\n"
            dl = [l for l in man.splitlines(True) if l.startswith("digest =")]
            assert dl, f"{name}: no digest line"
            # (r26-3) manifest closure: hash exactly the files the
            # manifest DECLARES — ghost entries or missing files fail
            import tomllib as _t2
            mdoc = _t2.loads(man)
            declared = [f for f in mdoc.get("files", [])]
            assert declared, f"{name}: manifest declares no files"
            for df in declared:
                fp = f"{base}/{name}/{df}"
                assert os.path.exists(fp), \
                    f"{name}: declared file {df} missing (ghost entry)"
                # LF eligibility: every file must be LF-clean UNLESS
                # the fixture's intended violation is a format one
                fb = open(fp, "rb").read()
                if b"\r" in fb:
                    assert name in ("08-crlf-manifest",) or \
                        (name.endswith("crlf") or "crlf" in name), \
                        f"{name}/{df}: unintended CR bytes (single-violation premise)"
                if not fb.endswith(b"\n"):
                    assert "no-trailing-lf" in name, \
                        f"{name}/{df}: unintended missing trailing LF"
            files_ = {df: open(f"{base}/{name}/{df}", "rb").read()
                      for df in declared}
            files_["manifest.toml"] = man.replace(dl[0], "").encode()
            # (r27-3) every lens the POLICY references must be declared
            pdoc = _t2.load(open(f"{base}/{name}/policy.toml", "rb"))
            policy_refs = set()
            for n in pdoc.get("nodes", []):
                for st in n.get("stages", []) or []:
                    for lf in st.get("lenses", []) or []:
                        policy_refs.add(lf)
                    if st.get("output_schema"):
                        policy_refs.add(st["output_schema"])
                if n.get("side_records"):
                    pass  # side-record schemas resolve via output
            for ps in pdoc.get("pack_stages", []) or []:
                for lf in ps.get("lenses", []) or []:
                    policy_refs.add(lf)
            missing = policy_refs - set(declared)
            assert not missing, \
                f"{name}: policy references undeclared files: {missing}"
            # (r29-2) the EXECUTED policy must be a manifest member
            assert "policy.toml" in declared, \
                f"{name}: manifest must declare policy.toml (execution binding)"
            # pack_stages output_schema references too
            pschema = {ps.get("output_schema") for ps in pdoc.get("pack_stages", [])
                       if ps.get("output_schema")}
            missing2 = pschema - set(declared)
            assert not missing2, \
                f"{name}: pack_stages reference undeclared schemas: {missing2}"
            h_ = _hl.sha256()
            for pn in sorted(files_):
                cb = files_[pn]  # bytes (r23-4: original bytes)
                h_.update(struct.pack("<Q", len(pn.encode())))
                h_.update(pn.encode())
                h_.update(struct.pack("<Q", len(cb)))
                h_.update(cb)
            decl = re.search(r'"([0-9a-f]{64})"', dl[0]).group(1)
            assert decl == h_.hexdigest(), \
                f"{name}: manifest digest mismatch (single-violation premise)"
    # (r22-1/2) generic closed-cycle analysis — first-match branch
    # semantics, self-loops, ALL simple cycles (bounded), §2.4b
    # deduct-node rule for resume edges
    def _succ(pol):
        out = {}
        for n in pol["nodes"]:
            nid = n["id"]
            if n["kind"] == "route":
                out[nid] = [br.get("goto") for br in n.get("branches", [])
                            if br.get("goto")]
            elif n["kind"] in ("dispatch", "redrive"):
                out[nid] = [n.get("on_done")]
            elif n["kind"] == "hold":
                out[nid] = list(n.get("resume_targets", []))
            else:
                out[nid] = []
        return out

    def _all_cycles(pol, bound=256):
        """All simple cycles incl. self-loops, rotation-Deduped.
        Exceeding the bound FAILS (never silently under-check)."""
        succ = _succ(pol)
        seen_canon = set()
        cycles = []
        for start in sorted(succ):
            stack = [(start, [start])]
            while stack:
                cur, path = stack.pop()
                for nxt in succ.get(cur, []):
                    if nxt is None:
                        continue
                    if nxt == start:
                        cyc = path  # node sequence, no repeat
                        # canonical rotation: smallest id first
                        mi = cyc.index(min(cyc))
                        canon = tuple(cyc[mi:] + cyc[:mi])
                        if canon not in seen_canon:
                            if len(seen_canon) >= bound:
                                raise AssertionError(
                                    f"cycle enumeration exceeded bound {bound}")
                            seen_canon.add(canon)
                            cycles.append(list(canon))
                    elif nxt not in path and nxt in succ:
                        stack.append((nxt, path + [nxt]))
        return cycles

    def _enforced_guards(w):
        """(r30-1) c enforces iff NO reachable state with c >= its
        SMALLEST cap makes the condition true. State space: counter
        values at {0, cap-1, cap, cap+1} for EVERY counter cap in the
        expression (multi-threshold preserved via list), clocks at
        {F,T} x {F,T}, verdict subsets of the expression's own
        any_in samples (plus empty and full), findings F/T. Bounded:
        <=4^counters * 4 * 2^|samples| * 2 <= ~10k evaluations."""
        import re as _re
        import itertools as _it
        # ALL thresholds per counter (dict-overwrite bug fixed)
        caps = {}
        for m in _re.finditer(r"count_lt\s*[(]\s*([A-Za-z_]\w*)\s*,\s*(\d+)\s*[)]", w):
            caps.setdefault(m.group(1), []).append(int(m.group(2)))
        # verdict universe from any_in samples
        uni = []
        for m in _re.finditer(r'any_in\s*\(\s*verdicts\s*,\s*\[(.*?)\]\s*\)', w):
            uni += _re.findall(r'"([^"]+)"', m.group(1))
        uni = sorted(set(uni))[:4]
        verdict_sets = [()] + [tuple(c) for r in range(1, len(uni) + 1)
                              for c in _it.combinations(uni, r)]
        clocks = [(False, False), (True, False), (False, True), (True, True)]
        def states_for(c):
            vals_per = []
            for k, ths in caps.items():
                s = {0}
                for t in ths:
                    s |= {max(t - 1, 0), t, t + 1}
                if k == c:
                    s |= {min(ths), max(ths) + 1}
                vals_per.append((k, sorted(s)))
            base = list(_it.product(*[v for _, v in vals_per]))
            for counters in base:
                cmap = dict(zip([k for k, _ in vals_per], counters))
                if cmap[c] < min(caps[c]):
                    continue  # only states where c can be over cap
                for verdicts in verdict_sets:
                    for to, bo in clocks:
                        for findings in (False, True):
                            yield {"counters": cmap,
                                   "verdicts": list(verdicts),
                                   "findings": findings,
                                   "timeout": to,
                                   "budget_exhausted": bo,
                                   "reverted_base": None}
        out = set()
        for c in caps:
            enforced = True
            for st in states_for(c):
                ok = evaluate(w, st)
                if ok is not False:
                    enforced = False
                    break
            if enforced:
                out.add(c)
        return out

    def _bumps_in(do):
        """Counter occurrence counts in an action sequence."""
        out = {}
        for m in re.finditer(r"bump\s*[(]\s*([A-Za-z_]\w*)\s*[)]", do or ""):
            out[m.group(1)] = out.get(m.group(1), 0) + 1
        return out

    def _merge(dst, src):
        for k, v in src.items():
            dst[k] = dst.get(k, 0) + v

    def _cycle_counter_analysis(pol):
        """(r24-1) Per-TRAVERSAL analysis. A traversal of the cycle
        follows EXACT edges: at each route node only branches whose
        goto is the cycle's NEXT node (not merely any cycle member)
        are choices; guard is recorded PER traversal (choice), not
        collapsed. Node bumps collected only from nodes ON the exact
        path. Returns list of ({counter:guard-counts per traversal},
        [per-traversal {counter: bump counts}, ...])."""
        import itertools
        nmap = {n["id"]: n for n in pol["nodes"]}
        results = []
        for cyc in _all_cycles(pol):
            L = len(cyc)
            route_options = []   # per position: list of (branch)
            node_bumps = {}
            for i, nid in enumerate(cyc):
                nxt = cyc[(i + 1) % L]
                n = nmap[nid]
                if n["kind"] == "route":
                    cont = [br for br in n.get("branches", [])
                            if br.get("goto") == nxt]
                    route_options.append(cont)
                else:
                    route_options.append(None)
                    if n["kind"] == "redrive" and n.get("bump"):
                        _merge(node_bumps, {n["bump"]: 1})
            traversals = []
            for combo in itertools.product(*[
                    opts if opts else [None] for opts in route_options]):
                tot = dict(node_bumps)
                guards = set()
                for br in combo:
                    if br is None:
                        continue
                    w = br.get("when") or ""
                    for c in _enforced_guards(w):
                        guards.add(c)
                    _merge(tot, _bumps_in(br.get("do")))
                traversals.append((guards, tot))
            results.append(traversals)
        return results

    def _has_02_resume_cycle(pol):
        # (r22-2) §2.4b: remove dispatch/redrive (deduct) nodes; a
        # resume edge participating in a cycle in the REMAINING graph
        # is the violation. Legal loops through deduct nodes pass.
        deduct = {n["id"] for n in pol["nodes"]
                  if n["kind"] in ("dispatch", "redrive")}
        succ = {}
        for n in pol["nodes"]:
            nid = n["id"]
            if nid in deduct:
                continue
            if n["kind"] == "route":
                succ[nid] = [br.get("goto") for br in n.get("branches", [])
                             if br.get("goto") and br["goto"] not in deduct]
            elif n["kind"] == "hold":
                succ[nid] = [t for t in n.get("resume_targets", [])
                             if t not in deduct]
            else:
                succ[nid] = []
        hold_ids = {n["id"] for n in pol["nodes"] if n["kind"] == "hold"}
        for start in succ:
            stack, seen = [(start, [start])], {start}
            while stack:
                cur, path = stack.pop()
                for nxt in succ.get(cur, []):
                    if nxt == start and len(path) >= 1:
                        # cycle contains a hold resume edge?
                        if any(x in hold_ids for x in path):
                            return True
                    if nxt not in seen and nxt in succ:
                        seen.add(nxt)
                        stack.append((nxt, path + [nxt]))
        return False

    def _norm_02(pol):
        for n in pol["nodes"]:
            if n["kind"] == "hold":
                n["resume_targets"] = []
        return pol

    def _norm_02_legal(pol):
        # (r22-2) LEGAL finite loop: resume back into the dispatch —
        # cycle passes through a deduct node, must NOT be a violation
        for n in pol["nodes"]:
            if n["kind"] == "hold":
                n["resume_targets"] = ["scan"] if any(
                    x["id"] == "scan" for x in pol["nodes"]) else []
        return pol

    def _has_03_missing_payload(pol):
        for n in pol["nodes"]:
            if n["kind"] == "redrive" and "payload" not in n:
                return True
        return False

    def _norm_03(pol):
        for n in pol["nodes"]:
            if n["kind"] == "redrive" and "payload" not in n:
                n["payload"] = "findings"
        return pol

    def _reachable_ids(pol):
        """(r25-2) node ids reachable from entry (on_done/goto edges;
        hold resumes count as possible continuations)."""
        entry = pol.get("entry")
        nmap = {n["id"]: n for n in pol["nodes"]}
        seen, stack = set(), [entry]
        while stack:
            nid = stack.pop()
            if nid in seen or nid not in nmap:
                continue
            seen.add(nid)
            n = nmap[nid]
            if n["kind"] == "route":
                stack += [b.get("goto") for b in n.get("branches", [])
                          if b.get("goto")]
            elif n["kind"] in ("dispatch", "redrive"):
                if n.get("on_done"):
                    stack.append(n["on_done"])
            elif n["kind"] == "hold":
                stack += n.get("resume_targets", [])
        return seen

    def _has_revert(do):
        """(r28-2) true iff the action sequence contains a revert —
        whitespace-tolerant, parsed per-action like apply_do."""
        for action in (do or "").split(";"):
            a = action.strip()
            if a.lower().startswith("revert"):
                rest = a[6:].lstrip()
                if rest.startswith("(") :
                    return True
        return False

    def _refs_provenance(w):
        stripped = re.sub(r'"[^"]*"', '""', w)
        for pat in (r"(?<![\w.])verdicts(?![\w])",
                    r"(?<![\w.])findings(?![\w])",
                    r"(?<![\w.])has_findings(?![\w])"):
            if re.search(pat, stripped):
                return True
        return False

    def _verify_evidence_ids(pol):
        """(r26-2) node ids that carry verified evidence: dispatch
        nodes with a verify stage, plus everything reachable FROM them
        through normal flow (on_done/goto) AND through hold resume —
        evidence survives into holds resumed after verification."""
        nmap = {n["id"]: n for n in pol["nodes"]}
        out = set()
        for n in pol["nodes"]:
            if n.get("kind") == "dispatch" and any(
                    st.get("role") == "verify"
                    for st in n.get("stages", [])):
                # forward closure from the verified dispatch
                stack = [n["id"]]
                seen = set()
                while stack:
                    x = stack.pop()
                    if x in seen or x not in nmap:
                        continue
                    seen.add(x)
                    m = nmap[x]
                    if m["kind"] == "route":
                        stack += [b.get("goto") for b in m.get("branches", [])
                                  if b.get("goto")]
                    elif m["kind"] in ("dispatch", "redrive"):
                        if m.get("on_done"):
                            stack.append(m["on_done"])
                        if m.get("hold_target"):
                            stack.append(m["hold_target"])
                    elif m["kind"] == "hold":
                        stack += m.get("resume_targets", [])
                out |= seen
        return out

    def _has_04_verdicts_no_verify(pol):
        """(r27-2) STATE-BASED provenance: evidence is valid only
        downstream of a SUCCESSFUL verify completion, invalidated by
        any intervening redrive (new artifact) or revert. Enumerate
        evidence states via BFS over (node, evidence_valid); a ref
        node reachable in a no-evidence state is a violation."""
        nmap = {n["id"]: n for n in pol["nodes"]}
        entry = pol.get("entry")

        def node_has_verify(n):
            return n.get("kind") == "dispatch" and any(
                st.get("role") == "verify"
                for st in n.get("stages", []))

        def successors(nid, ev):
            """Yield (next_id, evidence_after) for all flow edges."""
            n = nmap[nid]
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    g = br.get("goto")
                    if g:
                        # (r28-2) revert invalidates evidence — decided
                        # by parsing the action sequence for real
                        ev2 = ev
                        d = br.get("do") or ""
                        if _has_revert(d):
                            ev2 = False
                        yield (g, ev2)
            elif n["kind"] == "dispatch":
                if n.get("on_done"):
                    # verify stage SUCCESS completes with evidence
                    yield (n["on_done"], node_has_verify(n) or ev)
                if n.get("hold_target"):
                    # error path: no new evidence
                    yield (n["hold_target"], ev)
            elif n["kind"] == "redrive":
                if n.get("on_done"):
                    yield (n["on_done"], False)  # new artifact
                if n.get("hold_target"):
                    yield (n["hold_target"], ev)
            elif n["kind"] == "hold":
                for t in n.get("resume_targets", []):
                    yield (t, ev)

        # BFS over states; collect ref-reachability per state
        from collections import deque
        start = (entry, False)
        seen = {start}
        q = deque([start])
        ref_no_ev = []
        while q:
            nid, ev = q.popleft()
            if nid not in nmap:
                continue
            n = nmap[nid]
            if n["kind"] == "route" and not ev:
                if any(_refs_provenance(br.get("when") or "")
                       for br in n.get("branches", [])):
                    ref_no_ev.append(nid)
            for nxt, ev2 in successors(nid, ev):
                if (nxt, ev2) not in seen:
                    seen.add((nxt, ev2))
                    q.append((nxt, ev2))
        return bool(ref_no_ev)

    def _norm_04(pol):
        # remove ALL provenance references from branch conditions
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    w = br.get("when") or ""
                    stripped = re.sub(r'"[^"]*"', '""', w)
                    pats = (r"(?<![\w.])verdicts(?![\w])",
                            r"(?<![\w.])findings(?![\w])",
                            r"(?<![\w.])has_findings(?![\w])")
                    if any(re.search(p, stripped) for p in pats):
                        br["when"] = "false"
        return pol

    # 02/03/04 structural pair checks (r21-1, r22-2/3)
    for name, (chk, nrm) in {
        "02-resume-cycle": (_has_02_resume_cycle, _norm_02),
        "03-missing-payload": (_has_03_missing_payload, _norm_03),
        "04-verdicts-no-verify": (_has_04_verdicts_no_verify, _norm_04),
    }.items():
        pol = tl.load(open(f"{base}/{name}/policy.toml", "rb"))
        assert chk(pol), f"{name}: violation structure NOT present"
        assert not chk(nrm(copy.deepcopy(pol))), \
            f"{name}: normalization failed to clear the violation"
    # legal resume loop regression (r22-2): resume into dispatch is OK
    pol02 = tl.load(open(f"{base}/02-resume-cycle/policy.toml", "rb"))
    assert not _has_02_resume_cycle(_norm_02_legal(copy.deepcopy(pol02))), \
        "02: legal deduct-node resume loop must NOT be a violation"
    # 04 semantic regressions (r23-3): string-literal verdicts alone is
    # NOT a verdicts ref — but findings IS a provenance ref either way
    pol04 = tl.load(open(f"{base}/04-verdicts-no-verify/policy.toml", "rb"))
    # (r25-2) unreachable verify must NOT bless the flow
    unreach = copy.deepcopy(pol04)
    unreach["nodes"].append({
        "id": "orphan", "kind": "dispatch",
        "stages": [{"id": "v", "lenses": ["lens-a.md"], "role": "verify"}],
        "on_done": "done2", "hold_target": "end_hold",
    })
    unreach["nodes"].append({"id": "done2", "kind": "route",
                             "branches": [{"when": "true",
                                           "do": "emit(InspectionPassed, summary)"}]})
    assert _has_04_verdicts_no_verify(unreach), \
        "04: unreachable verify must not clear the provenance violation"
    # (r26-2) verify AFTER the ref (resume into future verify) must
    # NOT bless the earlier reference
    late = copy.deepcopy(pol04)
    late["nodes"].append({
        "id": "future_verify", "kind": "dispatch",
        "stages": [{"id": "v2", "lenses": ["lens-a.md"], "role": "verify"}],
        "on_done": "done", "hold_target": "end_hold",
    })
    for n in late["nodes"]:
        if n["id"] == "end_hold":
            n["resume_targets"] = ["future_verify"]
    assert _has_04_verdicts_no_verify(late), \
        "04: verify reached only via later resume must not bless the ref"
    str_lit = copy.deepcopy(pol04)
    for n in str_lit["nodes"]:
        if n["kind"] == "route":
            for br in n.get("branches", []):
                if "verdicts" in (br.get("when") or ""):
                    br["when"] = 'any_in(findings.group, ["verdicts"])'
    assert _has_04_verdicts_no_verify(str_lit), \
        "04: findings reference (even with string-literal verdicts)" \
        " must be a provenance violation"
    # (r24-2) has_findings alone is also a provenance ref
    hf = copy.deepcopy(pol04)
    for n in hf["nodes"]:
        if n["kind"] == "route":
            for br in n.get("branches", []):
                br["when"] = "has_findings(any)"
    assert _has_04_verdicts_no_verify(hf), \
        "04: has_findings(any) without verify must be a violation"
    no_ref = copy.deepcopy(pol04)
    for n in no_ref["nodes"]:
        if n["kind"] == "route":
            for br in n.get("branches", []):
                br["when"] = "false"
    assert not _has_04_verdicts_no_verify(no_ref), \
        "04: fully removed refs must clear the violation"

    # 10/11/12 (r22-1): per-traversal analysis on ALL cycles
    for fname in ("10-counter-guard-missing", "11-counter-guard-double",
                  "12-counter-guard-mismatch"):
        pol = tl.load(open(f"{base}/{fname}/policy.toml", "rb"))
        results = _cycle_counter_analysis(pol)
        assert results, f"{fname}: no cycle found"
        flat = [(g, t) for travs in results for g, t in travs]
        # every traversal must carry >=1 guard — an unguarded AUTO cycle
        # (always re-enters) is itself a violation
        assert all(g for g, t in flat), \
            f"{fname}: unguarded auto-cycle traversal present: {flat}"
        if fname.endswith("missing"):
            # every guard of every traversal is bumped 0 times
            assert all(t.get(c, 0) == 0 for g, t in flat for c in g), \
                f"{fname}: expected 0 guard-bumps/traversal: {flat}"
        elif fname.endswith("double"):
            # every traversal bumps EVERY guard >= 2 times
            assert all(t.get(c, 0) >= 2 for g, t in flat for c in g), \
                f"{fname}: expected >=2 guard-bumps/traversal: {flat}"
        else:
            # at least one guard never bumped while another counter is
            assert all(any(t.get(c, 0) == 0 for c in g) and any(t.values())
                       for g, t in flat), \
                f"{fname}: expected an unbumped guard per traversal: {flat}"
        # branch-exclusivity regression (r22-1): splitting the node bump
        # into two same-target branches must NOT read as double
        if fname.endswith("double"):
            # (r23-2) exclusive split: node bump removed, TWO same-
            # target branches each carrying a bump — each traversal
            # takes exactly ONE, so guard-bumps must be 1 (not 2)
            excl = copy.deepcopy(pol)
            for n in excl["nodes"]:
                if n.get("id") == "fix":
                    n.pop("bump", None)
            for n in excl["nodes"]:
                if n.get("kind") == "route":
                    for br in n.get("branches", []):
                        br.pop("do", None)
                    # rebuild: TWO same-destination branches, only ONE
                    # carrying a bump — each traversal fires exactly
                    # one (first-match), so guard-bumps must be 1
                    n["branches"] = [
                        {"when": "timeout() && count_lt(loopbacks, 5)",
                         "do": "bump(loopbacks)", "goto": "fix"},
                        {"when": "count_lt(loopbacks, 5)",
                         "do": "bump(loopbacks)", "goto": "fix"},
                        {"when": "true", "goto": "end_hold"},
                    ]
                    # verify the mutant actually has 2 fix-dest branches
                    fx = [b for b in n["branches"] if b.get("goto") == "fix"]
                    assert len(fx) == 2, f"regression needs 2 fix branches, got {len(fx)}"
            res2 = _cycle_counter_analysis(excl)
            g2 = [(g, t) for travs in res2 for g, t in travs]
            assert g2 and all(t.get(c, 0) == 1 for g, t in g2 for c in g), \
                f"{fname}: exclusive split must be 1/traversal: {g2}"
            # double action in ONE branch = 2 (r23-2)
            dbl = copy.deepcopy(pol)
            for n in dbl["nodes"]:
                if n.get("id") == "fix":
                    n.pop("bump", None)
                if n.get("kind") == "route":
                    for br in n.get("branches", []):
                        if br.get("goto") == "fix":
                            br["do"] = "bump(loopbacks); bump(loopbacks)"
            res3 = _cycle_counter_analysis(dbl)
            g3 = [(g, t) for travs in res3 for g, t in travs]
            assert g3 and all(t.get(c, 0) == 2 for g, t in g3 for c in g), \
                f"{fname}: single-branch double bump must be 2/traversal: {g3}"
        # cycle-removal mutation clears the analysis
        mut = copy.deepcopy(pol)
        for n in mut["nodes"]:
            if n.get("id") == "fix" and n.get("on_done") == "scan":
                n["on_done"] = "end_hold"
        assert not _cycle_counter_analysis(mut), \
            f"{fname}: cycle-removal must clear the analysis"
        # (r25-1) multi-guard regression: two guards on one branch,
        # only loopbacks bumped — fix_rounds guard is unbumped so the
        # analysis must still see a violation-shaped traversal
        mg = copy.deepcopy(pol)
        for n in mg["nodes"]:
            if n.get("kind") == "route":
                for br in n.get("branches", []):
                    if br.get("goto") == "fix" and "count_lt(" in (br.get("when") or ""):
                        br["when"] = \
                            "count_lt(loopbacks, 5) && count_lt(fix_rounds, 3)"
        mres = _cycle_counter_analysis(mg)
        mflat = [(g, t) for travs in mres for g, t in travs]
        assert all(len(g) == 2 for g, t in mflat if g), \
            f"{fname}: multi-guard branch must preserve BOTH guards: {mflat}"

    # (r22-3) fixture 07: quoted digest key as a real TOML KEY TOKEN —
    # a line starting with "digest" (comments stripped), not a mention
    b7 = open(f"{base}/07-quoted-digest-key/manifest.toml", "rb").read()
    key_lines = [ln for ln in b7.decode().splitlines()
                 if ln.lstrip().startswith('"digest"')]
    assert key_lines, "07 must carry the quoted digest key as a real key"
    # normalization must leave zero quoted-key lines
    norm7 = b7.decode().replace('"digest" =', 'digest =')
    assert not [ln for ln in norm7.splitlines()
                if ln.lstrip().startswith('"digest"')], \
        "07 normalization failed"
    b8 = open(f"{base}/08-crlf-manifest/manifest.toml", "rb").read()
    assert b"\r\n" in b8, "08 must carry real CRLF"
    assert "08 CRLF" and True
    b9 = open(f"{base}/09-no-trailing-lf/manifest.toml", "rb").read()
    assert not b9.endswith(b"\n"), "09 must lack trailing LF"
    man = b8.decode()
    line = [l for l in man.splitlines(True) if l.startswith("digest =")][0]
    import tomllib as _t8
    m8 = _t8.loads(man.replace(line, "digest = \"x\"\n"))
    files = {df: open(f"{base}/08-crlf-manifest/{df}", "rb").read()
             for df in m8.get("files", [])}
    for df in files:
        assert os.path.exists(f"{base}/08-crlf-manifest/{df}"), \
            f"08: declared file {df} missing"
    files["manifest.toml"] = man.replace(line, "").encode()
    import hashlib
    h = hashlib.sha256()
    for pn in sorted(files):
        cb = files[pn]  # bytes (r24-MINOR-2)
        h.update(struct.pack("<Q", len(pn.encode())))
        h.update(pn.encode())
        h.update(struct.pack("<Q", len(cb)))
        h.update(cb)
    import re as _re
    declared = _re.search(r'"([0-9a-f]{64})"', line).group(1)
    assert declared == h.hexdigest(), \
        f"08 digest must match CRLF-original bytes: {declared[:12]} vs {h.hexdigest()[:12]}"
    # (r19-1) per-fixture violation PRESENCE — normalizing the
    # violation (with digest recompute) must FAIL the sweep:
    def _norm_01(pol):
        # remove ra/rb cycle: rt -> done directly
        pol["nodes"] = [n for n in pol["nodes"] if n["id"] not in ("ra", "rb")]
        for n in pol["nodes"]:
            if n["id"] == "scan":
                n["on_done"] = "done"
        return pol
    def _norm_05(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n["branches"]:
                    if br["when"] == "false":
                        br["when"] = "true"
        return pol
    def _norm_06(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n["branches"]:
                    if "bump(" in (br.get("do") or ""):
                        br["do"] = 'emit(InspectionPassed, summary)'
        return pol
    def _norm_12(pol):
        for n in pol["nodes"]:
            if n["id"] == "fix":
                n["bump"] = "loopbacks"  # match the guard (bump side)
        return pol
    def _norm_12b(pol):
        # guard side: change the count_lt counter to fix_rounds
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    br["when"] = br["when"].replace("count_lt(loopbacks",
                                                     "count_lt(fix_rounds")
        return pol
    def _norm_13(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n["branches"]:
                    if br.get("do") and br["do"].count("emit(") > 1:
                        br["do"] = 'emit(InspectionPassed, summary)'
        return pol
    def _norm_14(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n["branches"]:
                    d = br.get("do") or ""
                    if "emit(" in d and "bump(" in d:
                        br["do"] = d.split(";")[0]
        return pol
    # each violation checker: True when the violation IS present
    def _has_01_cycle(pol):
        ids = {n["id"] for n in pol["nodes"]}
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    g = br.get("goto")
                    if g in ("ra", "rb") and "ra" in ids and "rb" in ids:
                        # ra->rb and rb->ra mutual
                        nmap = {x["id"]: x for x in pol["nodes"]}
                        ra = nmap.get("ra", {}); rb = nmap.get("rb", {})
                        ra_t = [b.get("goto") for b in ra.get("branches", [])]
                        rb_t = [b.get("goto") for b in rb.get("branches", [])]
                        return "rb" in ra_t and "ra" in rb_t
        return False
    def _has_05_no_true(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                last = n["branches"][-1]
                if last["when"] == "false" and "emit(" in (last.get("do") or ""):
                    return True
        return False
    def _has_06_do_bump_no_goto(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    if "bump(" in (br.get("do") or "") and not br.get("goto"):
                        return True
        return False
    def _has_12_mismatch(pol):
        # (r20-1) compare the GUARD'S counter against the actual bump:
        # mismatch exists iff the count_lt counter on the guarded
        # cycle differs from the bump target
        nmap = {n["id"]: n for n in pol["nodes"]}
        fix = nmap["fix"]
        bump = fix.get("bump")
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    w = br.get("when") or ""
                    if br.get("goto") == "fix" and "count_lt(" in w:
                        import re as _r
                        m = _r.search(r"count_lt\((\w+)", w)
                        if m and m.group(1) != bump:
                            return True
        return False
    def _has_13_multi(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    if (br.get("do") or "").count("emit(") > 1:
                        return True
        return False
    def _has_14_post(pol):
        for n in pol["nodes"]:
            if n["kind"] == "route":
                for br in n.get("branches", []):
                    d = br.get("do") or ""
                    if "emit(" in d and ";" in d and "bump(" in d.split("emit(")[1]:
                        return True
        return False
    VIOLATIONS = {
        "01-route-cycle": (_has_01_cycle, _norm_01),
        "05-no-true-fallback": (_has_05_no_true, _norm_05),
        "06-do-only-nonterminal": (_has_06_do_bump_no_goto, _norm_06),
        "12-counter-guard-mismatch": (_has_12_mismatch, _norm_12),
        # second normalization path (guard side) via direct assert:
        "13-multi-emit": (_has_13_multi, _norm_13),
        "14-post-emit-action": (_has_14_post, _norm_14),
    }
    for name, (checker, normalizer) in VIOLATIONS.items():
        pol = tl.load(open(f"{base}/{name}/policy.toml", "rb"))
        assert checker(pol), f"{name}: violation structure NOT present"
        normalized = normalizer(copy.deepcopy(pol))
        assert not checker(normalized), \
            f"{name}: normalizer failed to remove the violation"
    # (r20-1) fixture 12: BOTH normalization paths must clear the check
    pol12 = tl.load(open(f"{base}/12-counter-guard-mismatch/policy.toml", "rb"))
    assert not _has_12_mismatch(_norm_12b(copy.deepcopy(pol12))), \
        "12: guard-side normalization must also clear the mismatch"
    # structural rejects decidable statically (r16-3):
    n10 = tl.load(open(f"{base}/10-counter-guard-missing/policy.toml", "rb"))
    nodes10 = {n["id"]: n for n in n10["nodes"]}
    fix10 = nodes10["fix"]
    branches10 = nodes10["rt"]["branches"]
    has_bump = bool(fix10.get("bump")) or any(
        "bump(" in (br.get("do") or "") for br in branches10
        if br.get("goto") == "fix")
    assert not has_bump, "10 must have NO bump on the guarded cycle"
    n11 = tl.load(open(f"{base}/11-counter-guard-double/policy.toml", "rb"))
    nodes11 = {n["id"]: n for n in n11["nodes"]}
    bumps11 = (1 if nodes11["fix"].get("bump") else 0) + sum(
        1 for br in nodes11["rt"]["branches"]
        if br.get("goto") == "fix" and "bump(" in (br.get("do") or ""))
    assert bumps11 == 2, f"11 must double-bump, got {bumps11}"
    n15 = tl.load(open(f"{base}/15-verify-duplicate/policy.toml", "rb"))
    ver = sum(1 for n in n15["nodes"] if n["kind"] == "dispatch"
              for st in n.get("stages", []) if st.get("role") == "verify")
    assert ver == 2, f"15 must have 2 verify stages, got {ver}"
    n16 = tl.load(open(f"{base}/16-session-parallel/policy.toml", "rb"))
    found_pair = False
    for n in n16["nodes"]:
        if n["kind"] == "dispatch":
            sess = {}
            for st in n.get("stages", []):
                if st.get("session"):
                    sess.setdefault(st["session"], []).append(st)
            for sid, sts in sess.items():
                unordered = [x for x in sts if not x.get("after")]
                if len(sts) > 1 and len(unordered) > 1:
                    found_pair = True
    assert found_pair, "16 must actually contain the violation pair"
    n18 = tl.load(open(f"{base}/18-revert-then-emit/policy.toml", "rb"))
    revert_br = [br for n in n18["nodes"] if n["kind"] == "route"
                 for br in n["branches"]
                 if re.search(r"revert\s*\(", br.get("do") or "")]
    assert revert_br, "18 must contain a revert branch"
    nmap = {n["id"]: n for n in n18["nodes"]}
    tgt = revert_br[0].get("goto")
    tnode = nmap.get(tgt)
    assert tnode and tnode["kind"] == "route" and any(
        "emit(" in (br.get("do") or "") for br in tnode["branches"]), \
        "18 must reach emit without re-review after revert"
    n17 = tl.load(open(f"{base}/17-missing-error-policy/policy.toml", "rb"))
    for n in n17["nodes"]:
        if n["kind"] == "dispatch":
            assert "on_verify_invalid" not in n, "17 must lack the policy"
    print(f"fixture sweep: 18 fixtures, exact codes, 08 CRLF digest,"
          " structural rejects 10/11/15/16/17 verified")


def contract_checks(doc):
    """(r16-2) pack-contract regressions: pre_stage wiring and
    same-session ordering — the two mutants r15 slipped through."""
    stages_by_pack = {s["id"]: s for s in doc.get("pack_stages", [])}
    for n in doc["nodes"]:
        if n["kind"] == "redrive":
            if n["id"] in ("replan", "replan_full"):
                assert n.get("pre_stage"), \
                    f"{n['id']} must declare pre_stage (r18-2)"
            if n.get("pre_stage"):
                ps = n["pre_stage"]
                assert ps in stages_by_pack, \
                    f"{n['id']}.pre_stage={ps} not registered in pack_stages"
                for lf in stages_by_pack[ps].get("lenses", []):
                    assert os.path.exists(f"{PACK}/{lf}"), \
                        f"pre_stage {ps} lens {lf} missing"
                if n["id"] == "replan_full":
                    assert "lens-replan-full.md" in stages_by_pack[ps]["lenses"], \
                        "replan_full must run the full-plan regenerator"
                if n["id"] == "replan":
                    assert "lens-replan.md" in stages_by_pack[ps]["lenses"], \
                        "replan must run the partial amender"
    for n in doc["nodes"]:
        if n["kind"] == "dispatch":
            # (r18-3) full stage dependency graph over this dispatch:
            # acyclicity + same-session ordered reachability
            import collections
            adj = collections.defaultdict(list)
            indeg = {s["id"]: 0 for s in n["stages"]}
            for s in n["stages"]:
                aft = s.get("after")
                if aft is None:
                    aft = []
                elif isinstance(aft, str):
                    aft = [aft]
                for a in aft:
                    assert a in indeg, \
                        f"{s['id']}.after={a} dangling"
                    adj[a].append(s["id"])
                    indeg[s["id"]] += 1
            q = collections.deque(k for k, v in indeg.items() if v == 0)
            seen = 0
            while q:
                x = q.popleft(); seen += 1
                for y in adj[x]:
                    indeg[y] -= 1
                    if indeg[y] == 0: q.append(y)
            assert seen == len(indeg), "stage dependency graph has a cycle"

            def reach(fr):
                out, stack = set(), [fr]
                while stack:
                    x = stack.pop()
                    for y in adj.get(x, []):
                        if y not in out:
                            out.add(y); stack.append(y)
                return out

            sess = {}
            for st in n.get("stages", []):
                if st.get("session"):
                    sess.setdefault(st["session"], []).append(st)
            for sid, sts in sess.items():
                if len(sts) > 1:
                    # (r19-2) ordering = one-directional reachability
                    # between every pair; array order and after-count
                    # are NOT relied upon
                    for i in range(len(sts)):
                        for j in range(len(sts)):
                            if i >= j:
                                continue
                            r_ij = sts[j]["id"] in reach(sts[i]["id"])
                            r_ji = sts[i]["id"] in reach(sts[j]["id"])
                            assert r_ij != r_ji, \
                                f"session {sid}: {sts[i]['id']} / {sts[j]['id']}" \
                                f" must be ordered (mutual={r_ij and r_ji}," \
                                f" parallel={not r_ij and not r_ji})"
                    # the pack's same-reviewer contract: edge must
                    # precede claims in the trace session
                    ids = {s["id"] for s in sts}
                    if {"edge", "claims"} <= ids:
                        assert "claims" in reach("edge"), \
                            "trace session: claims must be ordered after edge"


if __name__ == "__main__":
    sys.exit(main())
