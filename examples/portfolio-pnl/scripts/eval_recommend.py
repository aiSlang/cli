#!/usr/bin/env python3
"""portfolio-pnl — graded eval for the deterministic recommendation engine.

The keep/rebalance decision is made by a pure rule engine
(`portfolio.recommend()`), so it can be graded exactly — no LLM, no network.
This harness runs a set of golden scenarios (`evals/recommend_cases.jsonl`)
through the engine and checks, per case:

  * the verdict matches (`keep` / `rebalance`);
  * every expected action (`{symbol, action}`) is present in the output;
  * `keep` cases emit no actions;
  * the engine is deterministic (identical output when re-run).

Prints a JSON report with per-case results and exits non-zero if any case
misses (or if the overall pass-rate is below `--threshold`), so it slots into
CI next to the unit tests.

    python3 scripts/eval_recommend.py [--cases evals/recommend_cases.jsonl] [--threshold 1.0]
"""
from __future__ import annotations

import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import portfolio as pf  # noqa: E402

DEFAULT_CASES = os.path.join(ROOT, "evals", "recommend_cases.jsonl")
DEFAULT_BUCKETS = {"safe": ["cash", "tbill", "bond_short"],
                   "aggressive": ["crypto", "option"]}


def load_cases(path: str) -> list[dict]:
    cases = []
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                cases.append(json.loads(line))
    return cases


def grade(case: dict, buckets: dict) -> dict:
    analytic = case["analytic"]
    strategic = case["strategic"]
    bkt = case.get("buckets", buckets)
    expect = case.get("expect", {})

    rec1 = pf.recommend(analytic, strategic, bkt)
    rec2 = pf.recommend(analytic, strategic, bkt)
    deterministic = (rec1 == rec2)

    got_verdict = rec1.get("verdict")
    verdict_ok = (got_verdict == expect.get("verdict"))

    got_actions = {(a.get("symbol", "").upper(), a.get("action", "").lower())
                   for a in rec1.get("actions", [])}
    want_actions = [(a["symbol"].upper(), a["action"].lower())
                    for a in expect.get("actions", [])]
    missing = [f"{s}:{a}" for (s, a) in want_actions if (s, a) not in got_actions]
    actions_ok = not missing

    # keep verdicts must carry no actions
    keep_clean = True
    if expect.get("verdict") == "keep":
        keep_clean = (len(rec1.get("actions", [])) == 0)

    passed = verdict_ok and actions_ok and keep_clean and deterministic
    return {
        "id": case.get("id"),
        "passed": passed,
        "verdict_ok": verdict_ok,
        "actions_ok": actions_ok,
        "keep_clean": keep_clean,
        "deterministic": deterministic,
        "got_verdict": got_verdict,
        "want_verdict": expect.get("verdict"),
        "missing_actions": missing,
        "got_actions": [f"{s}:{a}" for (s, a) in sorted(got_actions)],
    }


def run(cases_path: str, buckets: dict) -> dict:
    cases = load_cases(cases_path)
    results = [grade(c, buckets) for c in cases]
    passed = sum(1 for r in results if r["passed"])
    total = len(results)
    return {
        "eval": "recommend_engine",
        "cases_path": os.path.relpath(cases_path, ROOT),
        "total": total,
        "passed": passed,
        "pass_rate": (passed / total) if total else 0.0,
        "results": results,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description="Grade the deterministic recommendation engine.")
    ap.add_argument("--cases", default=DEFAULT_CASES)
    ap.add_argument("--threshold", type=float, default=1.0,
                    help="minimum pass-rate to succeed (default 1.0 = all cases)")
    ap.add_argument("--compact", action="store_true")
    args = ap.parse_args()

    report = run(args.cases, DEFAULT_BUCKETS)
    print(json.dumps(report, default=float) if args.compact
          else json.dumps(report, indent=2, default=float))

    ok = report["pass_rate"] >= args.threshold
    if not ok:
        misses = [r["id"] for r in report["results"] if not r["passed"]]
        sys.stderr.write(f"eval_recommend: FAILED — {report['passed']}/{report['total']} "
                         f"passed; misses: {misses}\n")
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
