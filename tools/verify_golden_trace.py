#!/usr/bin/env python3
"""Golden-trace verifier that EXECUTES the actual pack policy.toml.

r12-1: the previous verifier hand-mirrored the branch order — it could
not catch a pack whose bump target diverges from the counter its
branches test (the v0.13 replans/loopbacks split). This one parses the
real TOML, evaluates the real `when` expressions against a simulated
run state (counters, verdicts, findings), follows goto/hold/resume
transitions, applies real `bump` fields, and asserts:

1. Original step-04 contract on 60 combinations (15 non-empty category
   subsets x fix_rounds {2,3} x loopbacks {4,5}) — first routing.
2. Iterated transitions: repeated bad_plan from a fresh run halts at
   human exactly on the 6th loopback entry (counter hits 5 -> cap
   branch fires). Same for repeated intent_gap (resume->replan_full).
   The counter that gates the cap MUST be the counter the redrive
   bumps.
"""
import re
import sys
import tomllib
from itertools import combinations

PACK = "skills/bmad-review-full/v1"
POLICY = f"{PACK}/policy.toml"


def load():
    with open(POLICY, "rb") as f:
        return tomllib.load(f)


class Pack:
    def __init__(self, doc):
        self.nodes = {n["id"]: n for n in doc["nodes"]}
        self.entry = doc["entry"]
        self.counters = {k: 0 for k in doc.get("counters", {})}
        self.pack_stages = {s["id"]: s for s in doc.get("pack_stages", [])}

    def bumps(self, node_id):
        return self.nodes[node_id].get("bump")

    def hold_targets(self, node_id):
        return self.nodes[node_id].get("resume_targets", [])


# ---- condition expression evaluator (the pack's tiny DSL) ----
CALL = re.compile(r"([a-z_]+)\s*\(\s*([^)]*)\s*\)")


def strip(expr):
    return expr.replace(" ", "").replace("\n", "")


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
    e = strip(expr)
    # single-pass: replace outermost predicate calls with T/F
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


def first_route(pack, route_id, state):
    node = pack.nodes[route_id]
    assert node["kind"] == "route"
    for br in node["branches"]:
        if evaluate(br["when"], state):
            return br.get("goto"), br.get("do")
    raise AssertionError("no branch matched (true fallback missing)")


def route_once(pack, verdicts, findings, counters):
    state = {"verdicts": verdicts, "findings": findings,
             "counters": dict(counters)}
    target, _ = first_route(pack, "route1", state)
    return target


def bump_loopback(pack, target, counters):
    b = pack.bumps(target)
    if b is None:
        raise AssertionError(f"{target} has no bump — the loopback counter "
                             f"its guard tests would never increment")
    counters[b] = counters.get(b, 0) + 1


def iterated_bad_plan(pack):
    counters = {k: 0 for k in pack.counters}
    decisions = []
    for _ in range(10):
        target = route_once(pack, ["bad_plan"], True, counters)
        decisions.append(target)
        if target in ("replan", "replan_full"):
            bump_loopback(pack, target, counters)
        else:
            break
    return decisions


def iterated_intent_gap(pack):
    counters = {k: 0 for k in pack.counters}
    decisions = []
    for _ in range(10):
        target = route_once(pack, ["intent_gap"], True, counters)
        decisions.append(target)
        if target == "human_gap":
            resumes = pack.hold_targets("human_gap")
            assert "replan_full" in resumes, f"human_gap resumes {resumes}"
            target = "replan_full"  # human resolved -> resume
            decisions.append(target)
        if target in ("replan", "replan_full"):
            bump_loopback(pack, target, counters)
        else:
            break
    return decisions


ACTIVE = {"patch", "intent_gap", "bad_plan"}


def original_first(verdicts, fr, lb):
    if "intent_gap" in verdicts or "bad_plan" in verdicts:
        if lb >= 5:
            return "human"
        return "human_gap" if "intent_gap" in verdicts else "replan"
    if "patch" in verdicts:
        return "fixdie" if fr < 3 else "human"
    return "pass"


def main():
    pack = Pack(load())
    # --- 1. first-decision combinations against the REAL policy ---
    fails = total = 0
    subsets = [list(c) for r in range(1, 5) for c in combinations(
        ["patch", "intent_gap", "bad_plan", "defer"], r)]
    for vs in subsets:
        for fr in (2, 3):
            for lb in (4, 5):
                total += 1
                counters = {"fix_rounds": fr, "loopbacks": lb}
                findings = bool(ACTIVE & set(vs))
                got = route_once(pack, vs, findings, counters)
                want = original_first(vs, fr, lb)
                if got != want:
                    fails += 1
                    print("MISMATCH", vs, fr, lb, "got", got, "want", want)
    print(f"first-decision: {total - fails}/{total}")
    assert fails == 0

    # --- 2. iterated bad_plan: 6th entry must halt (cap fires) ---
    d = iterated_bad_plan(pack)
    print("iterated bad_plan:", d)
    assert d == ["replan"] * 5 + ["human"], \
        f"bad_plan loop must halt on 6th entry: {d}"

    # --- 3. iterated intent_gap: resume loop also capped at 6th ---
    d2 = iterated_intent_gap(pack)
    print("iterated intent_gap:", d2)
    # each loopback = one human_gap->resume->replan_full pair; the 6th
    # route decision must hit the common-cap branch -> human
    pairs = 0
    i = 0
    while i < len(d2) - 1:
        if d2[i] == "human_gap" and d2[i + 1] == "replan_full":
            pairs += 1
            i += 2
        else:
            break
    assert pairs == 5, f"expected 5 loopback pairs, got {pairs}: {d2}"
    assert d2[-1] == "human", f"6th entry must be capped human: {d2}"

    print("ALL PASS — pack routing == original step-04 contract")


if __name__ == "__main__":
    sys.exit(main())
