#!/usr/bin/env python3
"""portfolio-pnl — persist the Orchestrator's explained recommendation.

The keep/rebalance **decision** is canonical and deterministic: the rule
engine (`portfolio.recommend()`) already wrote the verdict, the sized
actions, and the grounding flags to the `recommendations` table via
`analyze.py`.  The Orchestrator LLM only *explains* that fixed
recommendation and emits it back as JSON.

This script reads the agent transcript on stdin, extracts the last JSON
object the Orchestrator produced, and MERGES it onto the canonical row:

  * verdict, actions, sizes, grounded_refs  → kept from the ENGINE (never
    trusted to the LLM, so downstream sizes stay deterministic);
  * summary, fragility_read, explanation, per-action rationale  → taken
    from the LLM (its owner-facing prose).

The merged object is written back to the latest `recommendations` row and
printed to stdout as JSON so downstream systems can consume it.

    aislang chat advise portfolio.ais | python3 scripts/persist_recommendation.py
"""
from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod  # noqa: E402


def extract_last_json_object(text: str) -> dict | None:
    """Return the last balanced top-level {...} object in text, or None.

    Tolerant of surrounding prose / code fences the model may add despite
    instructions.  Scans for brace-balanced spans (ignoring braces inside
    strings) and returns the last one that parses as a dict.
    """
    found = None
    depth = 0
    start = -1
    in_str = False
    esc = False
    for i, ch in enumerate(text):
        if in_str:
            if esc:
                esc = False
            elif ch == "\\":
                esc = True
            elif ch == '"':
                in_str = False
            continue
        if ch == '"':
            in_str = True
        elif ch == "{":
            if depth == 0:
                start = i
            depth += 1
        elif ch == "}":
            if depth > 0:
                depth -= 1
                if depth == 0 and start >= 0:
                    span = text[start:i + 1]
                    try:
                        obj = json.loads(span)
                    except json.JSONDecodeError:
                        obj = None
                    if isinstance(obj, dict):
                        found = obj
    return found


def merge(engine: dict, llm: dict | None) -> dict:
    """Engine wins on the decision; LLM contributes only prose."""
    merged = dict(engine)  # canonical verdict / actions / sizes / grounded_refs
    if not llm:
        return merged

    for field in ("summary", "fragility_read", "explanation"):
        val = llm.get(field)
        if isinstance(val, str) and val.strip():
            merged[field] = val.strip()

    # Fold the LLM's per-action rationale onto the engine actions, matched by
    # (symbol, action) — but keep the engine's symbol/action/size untouched.
    llm_rat = {}
    for a in llm.get("actions") or []:
        key = ((a.get("symbol") or "").upper(), (a.get("action") or "").lower())
        rat = a.get("rationale")
        if isinstance(rat, str) and rat.strip():
            llm_rat[key] = rat.strip()
    for a in merged.get("actions") or []:
        key = ((a.get("symbol") or "").upper(), (a.get("action") or "").lower())
        if key in llm_rat:
            a["rationale"] = llm_rat[key]

    merged["explained_by"] = "orchestrator"
    return merged


def main() -> int:
    raw = sys.stdin.read()
    llm = extract_last_json_object(raw)

    conn = dbmod.connect()
    row = conn.execute(
        "SELECT ts, actions_json FROM recommendations ORDER BY ts DESC LIMIT 1"
    ).fetchone()
    if row is None:
        sys.stderr.write(
            "persist_recommendation: no engine recommendation in the DB yet — "
            "run analyze.py --persist first.\n")
        return 0

    ts = row["ts"]
    try:
        engine = json.loads(row["actions_json"])
    except (TypeError, json.JSONDecodeError):
        sys.stderr.write("persist_recommendation: stored recommendation is not JSON.\n")
        return 0

    if llm is None:
        sys.stderr.write(
            "persist_recommendation: no JSON object found in the Orchestrator "
            "output; keeping the engine recommendation as-is.\n")

    merged = merge(engine, llm)
    dbmod.write_recommendation(
        conn, ts,
        verdict=merged.get("verdict"),
        summary=merged.get("summary"),
        actions=merged,                       # store the whole object as actions_json
        grounded_refs=json.dumps(merged.get("grounded_refs", [])),
    )
    print(json.dumps(merged, indent=2, default=float))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
