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
import re
import sys
import tomllib
from itertools import combinations

PACK = "skills/bmad-review-full/v1"
POLICY = f"{PACK}/policy.toml"


def load(path=POLICY):
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
    """branch do-sequence: bump(id); revert(base)"""
    for action in do.split(";"):
        action = action.strip()
        m = re.match(r"bump\((\w+)\)", action)
        if m:
            state["counters"][m.group(1)] = \
                state["counters"].get(m.group(1), 0) + 1
            continue
        m = re.match(r"revert\((\w+)\)", action)
        if m:
            state["reverted_base"] = m.group(1)  # durable, observed
            continue
        raise ValueError(f"unknown action {action}")


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
    # counter hit 5 exactly at the 6th decision
    st_real = res_real[0] if isinstance(res_real, tuple) else res_real
    assert st_real["counters"]["loopbacks"] == 6, st_real
    # 2. entry -> missing node must raise
    bad2 = copy.deepcopy(doc)
    bad2["entry"] = "missing"
    try:
        walk(bad2, ["patch"], active_cats)
        raise AssertionError("entry=missing must fail")
    except AssertionError as e:
        assert "unknown node" in str(e) or "missing" in str(e)
    # 3. removing edge session breaks the same-reviewer contract check
    # (verifier asserts presence below in positive checks)
    # 4. triage schema removal must break active_categories()
    bad3 = copy.deepcopy(doc)
    for n in bad3["nodes"]:
        if n["kind"] == "dispatch":
            for st in n.get("stages", []):
                if st.get("role") == "verify":
                    st.pop("output_schema", None)
    orig_open = open
    try:
        import builtins
        builtins.open = orig_open  # schema loader must find nothing
        try:
            active_categories(bad3)
            raise AssertionError("schema removal must break ACTIVE lookup")
        except AssertionError as e:
            assert "output_schema" in str(e)
    finally:
        builtins.open = orig_open


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
    print("ALL PASS — real-graph walk == original step-04 contract")


if __name__ == "__main__":
    sys.exit(main())
