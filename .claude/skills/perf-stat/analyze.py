#!/usr/bin/env python3
"""perf-stat analyzer — see .claude/skills/perf-stat/SKILL.md.

Walks git log, attributes each `Week N <Day>:` commit to one SP, counts
distinct calendar dates per week as Alex-days, computes per-week and
overall velocity. stdlib only.

Modes:
    analyze.py retrofit      compute + write cache + print table
    analyze.py report        print table from cached data
    analyze.py estimate N    SP → Alex-days projection from cache

Exit codes:
    0  success
    1  no git history found / no SP-claiming commits matched
    2  cache.json missing in report / estimate mode
    3  bad CLI usage
"""
from __future__ import annotations

import json
import math
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SCRIPT_DIR = Path(__file__).resolve().parent
CACHE = SCRIPT_DIR / "cache.json"
SCHEMA_VERSION = 1

# `Week 9 Mon: …` or `Week 10 Wed+Thu (prio swap): …` etc.  We accept
# any single-day marker on the first hit per commit (multi-day combos
# like `Wed+Thu` still count as 1 SP — they're one commit closing
# multiple PLAN slots; double-counting would inflate velocity).
SP_RE = re.compile(
    r"^\s*Week\s+(\d+)\s+(Mon|Tue|Wed|Thu|Fri)",
    flags=re.IGNORECASE,
)
WEEK_PREFIX_RE = re.compile(r"^\s*Week\s+(\d+)\b", flags=re.IGNORECASE)


# ── git log ingestion ──────────────────────────────────────────────

def git_log() -> list[tuple[str, str]]:
    """Return list of (iso_date_with_tz, subject) from oldest to newest."""
    repo_root = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    out = subprocess.run(
        ["git", "log", "--pretty=%aI|%s", "--no-merges", "--reverse"],
        check=True, capture_output=True, text=True, cwd=repo_root,
    ).stdout
    rows: list[tuple[str, str]] = []
    for line in out.splitlines():
        if "|" not in line:
            continue
        iso, subject = line.split("|", 1)
        rows.append((iso, subject))
    return rows


def parse_iso_date(iso: str) -> str:
    """Return YYYY-MM-DD in commit's authored timezone (not normalized to UTC)."""
    # Author may live in any tz; we want "calendar day Alex was working in",
    # which is the local-tz date of the commit.  iso form: 2026-05-28T17:50:09+02:00
    return iso[:10]


# ── per-week aggregation ───────────────────────────────────────────

def aggregate(rows: list[tuple[str, str]]) -> dict[int, dict[str, Any]]:
    """Group commits by week number; track SP count + distinct dates."""
    weeks: dict[int, dict[str, Any]] = defaultdict(lambda: {
        "sp": 0,
        "dates": set(),
        "subjects": [],
    })
    # Track which weeks we've ever seen by ANY commit (so a week with
    # only hotfixes still appears in the report, signalling "0 SP, M
    # days spent" — useful for "we slipped a hotfix-only week" detection).
    for iso, subject in rows:
        sp_match = SP_RE.match(subject)
        week_match = WEEK_PREFIX_RE.match(subject)
        if not week_match:
            continue
        week_num = int(week_match.group(1))
        date = parse_iso_date(iso)
        weeks[week_num]["dates"].add(date)
        weeks[week_num]["subjects"].append((date, subject))
        if sp_match:
            weeks[week_num]["sp"] += 1
    return dict(weeks)


def compute_velocity(weeks: dict[int, dict[str, Any]]) -> list[dict[str, Any]]:
    """Render the per-week table rows."""
    out: list[dict[str, Any]] = []
    for w in sorted(weeks.keys()):
        info = weeks[w]
        sp = info["sp"]
        alex_days = len(info["dates"])
        velocity = sp / alex_days if alex_days > 0 else 0.0
        out.append({
            "week":      w,
            "sp":        sp,
            "alex_days": alex_days,
            "velocity":  round(velocity, 2),
            "dates":     sorted(info["dates"]),
        })
    return out


def summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """Mean + stdev of velocity, restricted to weeks with SP > 0."""
    velocities = [r["velocity"] for r in rows if r["sp"] > 0]
    n = len(velocities)
    if n == 0:
        return {"n_weeks": 0, "mean_velocity": 0.0, "std_velocity": 0.0}
    mean = sum(velocities) / n
    variance = sum((v - mean) ** 2 for v in velocities) / n if n > 1 else 0.0
    std = math.sqrt(variance)
    return {
        "n_weeks":       n,
        "mean_velocity": round(mean, 2),
        "std_velocity":  round(std, 2),
        # Also report extremes — useful when std is high.
        "min_velocity":  round(min(velocities), 2),
        "max_velocity":  round(max(velocities), 2),
    }


# ── rendering ──────────────────────────────────────────────────────

def render_table(rows: list[dict[str, Any]], s: dict[str, Any]) -> str:
    out: list[str] = []
    out.append("Week  SP   Alex-days  Velocity (SP/day)")
    out.append("----  ---  ---------  -----------------")
    for r in rows:
        out.append(
            f"{r['week']:<4}  {r['sp']:<3}  {r['alex_days']:<9}  "
            f"{r['velocity']:>5.2f}"
        )
    out.append("")
    if s["n_weeks"] > 0:
        out.append(
            f"Summary: mean velocity = {s['mean_velocity']:.2f} SP/day  "
            f"(std {s['std_velocity']:.2f}, n={s['n_weeks']})"
        )
        out.append(
            f"         range: {s['min_velocity']:.2f} … {s['max_velocity']:.2f} SP/day"
        )
    else:
        out.append("Summary: no SP-claiming weeks yet.")
    return "\n".join(out)


def render_estimate(sp: int, s: dict[str, Any]) -> str:
    if s["n_weeks"] == 0 or s["mean_velocity"] == 0.0:
        return ("(no velocity history yet — run `perf-stat retrofit` first, "
                "or ship some `Week N <Day>:`-prefixed commits)")
    mean_v   = s["mean_velocity"]
    std_v    = s["std_velocity"]
    expected = sp / mean_v
    # ±1σ on velocity → asymmetric range on Alex-days.
    if std_v > 0:
        low_v  = mean_v + std_v
        high_v = max(mean_v - std_v, 0.01)  # avoid div-by-zero / negative tail
        lo = sp / low_v
        hi = sp / high_v
    else:
        lo = hi = expected
    out = [
        f"{sp} SP ≈ {expected:.1f} Alex-days   "
        f"(mean velocity {mean_v:.2f} SP/day, n={s['n_weeks']})",
        f"           range: {lo:.1f} … {hi:.1f} Alex-days (±1σ on velocity)",
    ]
    return "\n".join(out)


# ── cache I/O ──────────────────────────────────────────────────────

def write_cache(rows: list[dict[str, Any]], s: dict[str, Any]) -> None:
    payload = {
        "schema_version": SCHEMA_VERSION,
        "computed_at":    datetime.now(timezone.utc).isoformat(),
        "weeks":          rows,
        "summary":        s,
    }
    tmp = CACHE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, indent=2) + "\n")
    os.replace(tmp, CACHE)


def read_cache() -> dict[str, Any] | None:
    if not CACHE.exists():
        return None
    try:
        return json.loads(CACHE.read_text())
    except json.JSONDecodeError:
        return None


# ── entrypoints ────────────────────────────────────────────────────

def cmd_retrofit() -> int:
    rows_raw = git_log()
    if not rows_raw:
        print("perf-stat: no git history; run from a checked-out repo.",
              file=sys.stderr)
        return 1
    weeks = aggregate(rows_raw)
    if not weeks:
        print("perf-stat: no `Week N <Day>:` commits found.", file=sys.stderr)
        return 1
    rows = compute_velocity(weeks)
    s = summary(rows)
    write_cache(rows, s)
    print(render_table(rows, s))
    return 0


def cmd_report() -> int:
    cache = read_cache()
    if cache is None:
        print("perf-stat: no cache.json; run `perf-stat retrofit` first.",
              file=sys.stderr)
        return 2
    print(render_table(cache["weeks"], cache["summary"]))
    return 0


def cmd_estimate(sp_str: str) -> int:
    try:
        sp = int(sp_str)
    except ValueError:
        print(f"perf-stat: estimate expects an integer SP, got {sp_str!r}",
              file=sys.stderr)
        return 3
    if sp < 1:
        print("perf-stat: SP must be a positive integer", file=sys.stderr)
        return 3
    cache = read_cache()
    if cache is None:
        print("perf-stat: no cache.json; run `perf-stat retrofit` first.",
              file=sys.stderr)
        return 2
    print(render_estimate(sp, cache["summary"]))
    return 0


def usage() -> int:
    print(__doc__.strip(), file=sys.stderr)
    return 3


def main(argv: list[str]) -> int:
    args = argv[1:]
    if not args or args[0] == "retrofit":
        return cmd_retrofit()
    if args[0] == "report":
        return cmd_report()
    if args[0] == "estimate":
        if len(args) != 2:
            return usage()
        return cmd_estimate(args[1])
    return usage()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
