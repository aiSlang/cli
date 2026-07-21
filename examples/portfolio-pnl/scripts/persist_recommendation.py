#!/usr/bin/env python3
"""portfolio-pnl — persist the advisor's recommendation into the DB.

Reads the advisor output on stdin (the full `aislang chat advise` transcript
is fine — the Orchestrator's JSON is extracted from it), validates it looks
like a recommendation, and writes it to the `recommendations` table so the
dashboard can render it.  Non-fatal: if no recommendation JSON is found
(e.g. the model ran ungrounded or errored) it warns and exits 0.

    aislang chat advise portfolio.ais | python3 scripts/persist_recommendation.py
"""
from __future__ import annotations

import datetime
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import db as dbmod  # noqa: E402


def extract_recommendation(text: str) -> dict | None:
    """Return the last balanced JSON object in `text` that has a 'verdict'."""
    dec = json.JSONDecoder()
    best = None
    i = 0
    n = len(text)
    while i < n:
        if text[i] == "{":
            try:
                obj, end = dec.raw_decode(text, i)
            except ValueError:
                i += 1
                continue
            if isinstance(obj, dict) and "verdict" in obj:
                best = obj
            i = end
        else:
            i += 1
    return best


def main() -> int:
    reco = extract_recommendation(sys.stdin.read())
    if not reco:
        print("persist_recommendation: no recommendation JSON found — skipping "
              "(advisor may have run ungrounded or errored)", file=sys.stderr)
        return 0

    conn = dbmod.connect()
    dbmod.init_db(conn)
    ts = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    # Store the WHOLE recommendation object in actions_json so the dashboard
    # has verdict/summary/fragility_read/actions/confidence in one place.
    conn.execute(
        "INSERT OR REPLACE INTO recommendations(ts, verdict, summary, actions_json, grounded_refs) "
        "VALUES(?,?,?,?,?)",
        (ts, reco.get("verdict"), reco.get("summary", ""),
         json.dumps(reco), json.dumps(reco.get("grounded_refs", []))),
    )
    conn.commit()
    print(f"persist_recommendation: saved '{reco.get('verdict')}' recommendation "
          f"with {len(reco.get('actions', []))} action(s)", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
