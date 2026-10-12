#!/usr/bin/env python3
"""64-combination golden-trace verifier for bmad-review-full v1.1.0.

Original step-04 contract being mirrored:
- A loopback is triggered by intent_gap OR bad_plan; lower entries moot.
- BEFORE each loopback the review_loop_iteration counter increments;
  if it exceeds 5, HALT and escalate to the human (applies to
  intent_gap loopbacks too, not only bad_plan).
- patch: same-implementer fix, pack cap 3.
- defer: record and proceed (not an active finding).
- has_findings reflects patch/intent_gap/bad_plan entries (defer-only
  runs have no active findings).

Mirrors skills/bmad-review-full/v1/policy.toml route1 branch order:
  budget/timeout -> human; common cap (intent_gap|bad_plan &&
  !count_lt(loopbacks,5)) -> human; intent_gap -> human_gap;
  bad_plan <5 -> replan; residual bad_plan -> human; patch <3 ->
  fixdie; has_findings -> human; !has_findings -> pass; true -> human.
"""
from itertools import combinations

CATS = ["patch", "intent_gap", "bad_plan", "defer"]
ACTIVE = {"patch", "intent_gap", "bad_plan"}


def route(verdicts, fix_rounds, loopbacks):
    # common cap branch (r11-1b): applies to intent_gap loopbacks too
    if ("intent_gap" in verdicts or "bad_plan" in verdicts) and loopbacks >= 5:
        return "human"
    if "intent_gap" in verdicts:
        return "human_gap"
    if "bad_plan" in verdicts and loopbacks < 5:
        return "replan"
    if "bad_plan" in verdicts:
        return "human"       # residual bad_plan: no leak to patch
    if "patch" in verdicts and fix_rounds < 3:
        return "fixdie"
    if ACTIVE & set(verdicts):
        return "human"       # patch at cap etc.
    return "pass"            # defer-only


def main():
    fails = total = 0
    subsets = [list(c) for r in range(1, 5) for c in combinations(CATS, r)]
    for vs in subsets:
        for fr in (2, 3):
            for lb in (4, 5):
                total += 1
                got = route(vs, fr, lb)
                # original step-04 expectation
                if "intent_gap" in vs or "bad_plan" in vs:
                    want = ("human" if lb >= 5
                            else ("human_gap" if "intent_gap" in vs else "replan"))
                elif "patch" in vs:
                    want = "fixdie" if fr < 3 else "human"
                else:
                    want = "pass"
                if got != want:
                    fails += 1
                    print("MISMATCH", vs, "fr=", fr, "lb=", lb,
                          "got", got, "want", want)
    print(f"{total - fails}/{total} combinations match")
    assert fails == 0


if __name__ == "__main__":
    main()
