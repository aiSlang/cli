#!/usr/bin/env python3
"""perf-stat analyzer — see .claude/skills/perf-stat/SKILL.md.

Walks git log, attributes each `Week N <Day>:` commit to one SP, counts
distinct calendar dates per week as Alex-days, computes per-week and
overall velocity.  Schema v2 adds Claude-session token tallying: walks
~/.claude/projects/*/*.jsonl, sums input + output + cache_creation +
cache_read tokens per calendar date, buckets into the week dates.

stdlib only.

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
SCHEMA_VERSION = 2  # v2 adds Claude-session token tallying

# Where Claude Code persists session transcripts.  Each conversation
# is one `.jsonl` file under a directory whose name encodes the
# Claude-Code working directory.  We walk ALL project subdirs
# (your work isn't necessarily one-project-per-dir — the same
# Claude session might touch multiple repos) and filter by the
# message timestamps that fall on dates the week table claims.
CLAUDE_PROJECTS_DIR = Path.home() / ".claude" / "projects"

# ── Pricing reference (Anthropic Claude Sonnet 4.6, late 2025/early
# 2026 list prices, USD per 1M tokens).  These ratios are stable
# across providers within ~2× — Sonnet is the de-facto reference for
# v0.1 / v0.2 work.  Change the constants here to recompute against
# a different model class without touching the rest of the code.
# ──
PRICE_INPUT_PER_M:          float = 3.00   # uncached input
PRICE_OUTPUT_PER_M:         float = 15.00  # output (5× input)
PRICE_CACHE_CREATION_PER_M: float = 3.75   # 1.25× input (one-time)
PRICE_CACHE_READ_PER_M:     float = 0.30   # 0.1× input (subsequent reads)


def compute_usd_cost(tokens: dict[str, int]) -> float:
    """Return total USD-equivalent for one date / week's token bucket
    using the Sonnet-4.6 reference rates above.  Multiply by your
    model's ratio if comparing to e.g. Haiku ($0.80/$4 vs $3/$15 →
    ~3.75× cheaper) or Opus (more expensive)."""
    return (
        tokens.get("input",          0) / 1_000_000 * PRICE_INPUT_PER_M
      + tokens.get("output",         0) / 1_000_000 * PRICE_OUTPUT_PER_M
      + tokens.get("cache_creation", 0) / 1_000_000 * PRICE_CACHE_CREATION_PER_M
      + tokens.get("cache_read",     0) / 1_000_000 * PRICE_CACHE_READ_PER_M
    )

# `Week 9 Mon: …` or `Week 10 Wed+Thu (prio swap): …` etc.  We accept
# any single-day marker on the first hit per commit (multi-day combos
# like `Wed+Thu` still count as 1 SP — they're one commit closing
# multiple PLAN slots; double-counting would inflate velocity).
#
# v0.3 Week 17 Fri retrofit: commit-subject conventions drifted after
# Week 13 — v0.3 prefixes (`v0.3 Week 17 Thu:`) and Phase-N suffixes
# (`Phase 2 Week 15 Wed.`) both appeared.  The SP_RE now matches the
# `Week N <Day>` token ANYWHERE in the subject (not just at start)
# so the retrofit captures Weeks 14-17 retrospectively without
# rewriting commit history.  Each subject still counts at most once
# per (week, day) pair, so a passing mention can't double-count.
SP_RE = re.compile(
    r"\bWeek\s+(\d+)\s+(Mon|Tue|Wed|Thu|Fri)\b",
    flags=re.IGNORECASE,
)
WEEK_PREFIX_RE = re.compile(r"\bWeek\s+(\d+)\b", flags=re.IGNORECASE)


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
        # v0.3 Week 17 Fri retrofit: use re.search so the `Week N <Day>`
        # token is found ANYWHERE in the subject (not just at start),
        # accommodating both v0.X prefixes (`v0.3 Week 17 Thu:`) and
        # Phase-N suffixes (`Phase 2 Week 15 Wed.`).
        sp_match = SP_RE.search(subject)
        week_match = WEEK_PREFIX_RE.search(subject)
        if not week_match:
            continue
        week_num = int(week_match.group(1))
        date = parse_iso_date(iso)
        weeks[week_num]["dates"].add(date)
        weeks[week_num]["subjects"].append((date, subject))
        if sp_match:
            weeks[week_num]["sp"] += 1
    return dict(weeks)


# ── token aggregation (schema v2) ──────────────────────────────────

def _find_usage(obj: Any, depth: int = 0) -> dict | None:
    """Recursive search for an 'usage' dict inside a JSONL message.
    Claude Code's session lines are nested objects — the `usage` field
    can appear at different depths depending on message shape.  Caps
    at depth 6 to avoid stack blowup on adversarial input."""
    if depth > 6:
        return None
    if isinstance(obj, dict):
        if "usage" in obj and isinstance(obj["usage"], dict):
            return obj["usage"]
        for v in obj.values():
            r = _find_usage(v, depth + 1)
            if r is not None:
                return r
    elif isinstance(obj, list):
        for v in obj:
            r = _find_usage(v, depth + 1)
            if r is not None:
                return r
    return None


# ── chat active-time aggregation (post-Week-16 follow-up) ──────────

# Inter-message gap (seconds) below which we count the gap as
# "active chat time."  Above this we treat the gap as idle (user
# walked away, slept, switched repos) and stop the running clock
# until the next message.
#
# 5 minutes balances two failure modes:
#   - Too low (e.g. 60s): a single thoughtful pause where the user
#     reads a long tool result resets the clock, undercounting time.
#   - Too high (e.g. 30m): a coffee break / overnight session that
#     resumed the next morning gets counted as continuous, wildly
#     overcounting time.
#
# 300s captures "user is typing / model is replying / user is reading
# tool output" — anything where the chat session is the user's
# primary focus.  Tunable here without rerunning anything else.
CHAT_ACTIVE_GAP_THRESHOLD_SEC: int = 300


def aggregate_chat_active_time_per_date(
    project_filter: str | None = None,
) -> dict[str, float]:
    """Walk Claude session JSONLs and return per-date "active chat
    seconds".  Same project-filter semantics as
    [`aggregate_tokens_per_date`].

    Algorithm: per session (one .jsonl file), sort messages by
    timestamp.  For each consecutive pair (cur, nxt):
      - If `nxt.ts - cur.ts < THRESHOLD`, attribute the delta to the
        date of `cur.ts` (so cross-midnight sessions split between
        two date buckets the same way tokens do).
      - Else (idle gap), contribute nothing.

    The first message of each session contributes nothing on its
    own (active time is INTER-message).  This under-counts solo-
    message sessions (e.g. a quick `/perf-stat` invocation that
    doesn't reply) — fine for our use case where chat duration
    matters most for sessions that actually exchanged work.

    Returns `{YYYY-MM-DD: float seconds}` across all matched
    sessions.

    Failures swallowed silently: corrupt JSONL lines, missing
    timestamps, or unparseable date strings contribute zero.
    """
    out: dict[str, float] = defaultdict(float)
    if not CLAUDE_PROJECTS_DIR.exists():
        return {}
    needle = project_filter.lower() if project_filter else None
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if not project_dir.is_dir():
            continue
        if needle and needle not in project_dir.name.lower():
            continue
        for jsonl in project_dir.glob("*.jsonl"):
            # Per-session list of (parsed_datetime, date_string).
            stamps: list[tuple[datetime, str]] = []
            try:
                with jsonl.open() as f:
                    for line in f:
                        try:
                            r = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        ts = r.get("timestamp")
                        if not ts or not isinstance(ts, str):
                            continue
                        try:
                            # Normalize to a timezone-aware datetime;
                            # `fromisoformat` handles `+02:00` and `Z`
                            # suffix forms (Python 3.11+).
                            ts_norm = ts.replace("Z", "+00:00") if ts.endswith("Z") else ts
                            dt = datetime.fromisoformat(ts_norm)
                        except ValueError:
                            continue
                        stamps.append((dt, ts[:10]))
            except OSError:
                continue
            stamps.sort(key=lambda x: x[0])
            for i in range(len(stamps) - 1):
                delta = (stamps[i + 1][0] - stamps[i][0]).total_seconds()
                if 0 < delta < CHAT_ACTIVE_GAP_THRESHOLD_SEC:
                    out[stamps[i][1]] += delta
    return dict(out)


def _fmt_duration(seconds: float) -> str:
    """Compact human-readable duration.  0 → '—'; <60s → 'Ns'; <1h →
    'Nm'; >=1h → 'Hh Mm'.  Right-aligns in a 7-char field for table
    rendering."""
    if seconds <= 0:
        return "    —"
    if seconds < 60:
        return f"{int(seconds):>4}s"
    if seconds < 3600:
        return f"{int(seconds / 60):>4}m"
    hours = int(seconds // 3600)
    minutes = int((seconds % 3600) // 60)
    if minutes == 0:
        return f"{hours:>3}h  "
    return f"{hours:>2}h{minutes:02d}m"


def aggregate_tokens_per_date(
    project_filter: str | None = None,
) -> dict[str, dict[str, int]]:
    """Walk Claude session JSONLs and aggregate token usage by date.

    Returns {YYYY-MM-DD: {input, output, cache_creation, cache_read,
    total}} across all matching project dirs.

    `project_filter` (added in the perf-stat-filters follow-up):
    case-insensitive SUBSTRING match against each project-dir basename.
    Default behaviour (None / empty) walks every dir under
    ~/.claude/projects/ — the legacy "all projects" mode.

    **Why filter exists.**  Claude Code records sessions under a dir
    named after the CWD where Claude was launched.  Users routinely
    work on project X from a CWD belonging to project Y; perf-stat's
    "tokens per date" attribution then bundles X's spend into Y's
    bucket.  For per-project cost analysis (e.g. "how much did the
    v0.2 aiSlang work actually cost vs. side-quests on other repos")
    you want to filter to JUST the project dir whose name encodes
    the relevant CWD prefix.  Pass `--project-filter aiSlang` (or any
    substring of the encoded dir name) to do so.

    **Why all projects is still the default.**  Sessions can `cd`
    mid-stream; restricting too aggressively misses spend that
    belongs to the project under analysis.  The wide-net default
    over-counts; a substring filter narrows.

    Failures swallowed silently: a corrupt jsonl line returns no
    usage; an unparseable file is skipped.  Token metric is
    informational; broken telemetry shouldn't break perf-stat.
    """
    out: dict[str, dict[str, int]] = defaultdict(lambda: {
        "input":          0,
        "output":         0,
        "cache_creation": 0,
        "cache_read":     0,
        "total":          0,
    })
    if not CLAUDE_PROJECTS_DIR.exists():
        return {}
    needle = project_filter.lower() if project_filter else None
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if not project_dir.is_dir():
            continue
        if needle and needle not in project_dir.name.lower():
            continue
        for jsonl in project_dir.glob("*.jsonl"):
            try:
                with jsonl.open() as f:
                    for line in f:
                        try:
                            r = json.loads(line)
                        except json.JSONDecodeError:
                            continue
                        ts = r.get("timestamp")
                        if not ts or not isinstance(ts, str):
                            continue
                        # YYYY-MM-DD prefix of an ISO-8601 timestamp.
                        date = ts[:10]
                        if len(date) != 10:
                            continue
                        usage = _find_usage(r)
                        if not usage:
                            continue
                        bucket = out[date]
                        bucket["input"]          += int(usage.get("input_tokens", 0) or 0)
                        bucket["output"]         += int(usage.get("output_tokens", 0) or 0)
                        bucket["cache_creation"] += int(usage.get("cache_creation_input_tokens", 0) or 0)
                        bucket["cache_read"]     += int(usage.get("cache_read_input_tokens", 0) or 0)
            except OSError:
                continue
    for date, tok in out.items():
        tok["total"] = (tok["input"] + tok["output"]
                        + tok["cache_creation"] + tok["cache_read"])
    return dict(out)


def compute_velocity(
    weeks:               dict[int, dict[str, Any]],
    tokens_per_date:     dict[str, dict[str, int]] | None = None,
    chat_time_per_date:  dict[str, float] | None = None,
) -> list[dict[str, Any]]:
    """Render the per-week table rows.  When `tokens_per_date` is
    provided (schema v2), each row gains a `tokens` dict summed
    across that week's dates + a tokens_per_sp ratio.  Schema v2.2
    adds `chat_active_seconds` aggregated from `chat_time_per_date`
    (post Week-16-Fri follow-up — see SKILL.md §"Chat active time")."""
    tokens_per_date    = tokens_per_date    or {}
    chat_time_per_date = chat_time_per_date or {}
    out: list[dict[str, Any]] = []
    for w in sorted(weeks.keys()):
        info = weeks[w]
        sp = info["sp"]
        alex_days = len(info["dates"])
        velocity = sp / alex_days if alex_days > 0 else 0.0

        # Sum tokens across this week's dates.  A date that doesn't
        # appear in tokens_per_date contributes zero (Claude sessions
        # for that day either don't exist or carried no usage telemetry).
        week_tokens = {
            "input":          0,
            "output":         0,
            "cache_creation": 0,
            "cache_read":     0,
            "total":          0,
        }
        for d in info["dates"]:
            if d in tokens_per_date:
                for k in week_tokens:
                    week_tokens[k] += tokens_per_date[d][k]
        tokens_per_sp = (week_tokens["total"] / sp) if sp > 0 else 0
        tokens_per_alex_day = (week_tokens["total"] / alex_days) if alex_days > 0 else 0

        # Chat active time (schema v2.2) — sum across week's dates.
        # Same cross-week double-counting caveat as tokens (a single
        # date attributed to multiple weeks contributes to both per-
        # week sums; summary() uses unique-date totals for the
        # project-wide number to keep it honest).
        chat_seconds = 0.0
        for d in info["dates"]:
            chat_seconds += chat_time_per_date.get(d, 0.0)
        chat_seconds_per_sp = (chat_seconds / sp) if sp > 0 else 0.0

        out.append({
            "week":                  w,
            "sp":                    sp,
            "alex_days":             alex_days,
            "velocity":              round(velocity, 2),
            "tokens":                week_tokens,
            "tokens_per_sp":         int(tokens_per_sp),
            "tokens_per_alex_day":   int(tokens_per_alex_day),
            "chat_active_seconds":   round(chat_seconds, 1),
            "chat_seconds_per_sp":   round(chat_seconds_per_sp, 1),
            "dates":                 sorted(info["dates"]),
        })
    return out


def summary(
    rows:               list[dict[str, Any]],
    tokens_per_date:    dict[str, dict[str, int]] | None = None,
    chat_time_per_date: dict[str, float] | None = None,
) -> dict[str, Any]:
    """Mean + stdev of velocity, restricted to weeks with SP > 0.
    Schema v2 also rolls up project-wide token totals.

    Per-week token sums (in the table) may double-count a day that
    appears in multiple weeks' date sets (cross-week work commits
    can share a calendar date — e.g. Week 10 close-out + Week 11
    Mon both landed on the same day).  But the PROJECT-wide
    `total_tokens` here uses unique dates across all weeks so the
    bottom-line ratios aren't inflated.
    """
    velocities = [r["velocity"] for r in rows if r["sp"] > 0]
    n = len(velocities)
    if n == 0:
        return {"n_weeks": 0, "mean_velocity": 0.0, "std_velocity": 0.0}
    mean = sum(velocities) / n
    variance = sum((v - mean) ** 2 for v in velocities) / n if n > 1 else 0.0
    std = math.sqrt(variance)

    # Project-wide token total: sum across UNIQUE dates from all weeks.
    tokens_per_date    = tokens_per_date    or {}
    chat_time_per_date = chat_time_per_date or {}
    unique_dates: set[str] = set()
    for r in rows:
        for d in r.get("dates", []):
            unique_dates.add(d)

    # Per-class totals across unique dates.
    totals = {"input": 0, "output": 0, "cache_creation": 0, "cache_read": 0, "total": 0}
    chat_active_seconds_total = 0.0
    for d in unique_dates:
        if d in tokens_per_date:
            for k in totals:
                totals[k] += tokens_per_date[d][k]
        chat_active_seconds_total += chat_time_per_date.get(d, 0.0)

    # tokens / SP using unique-date totals divided by total SP across
    # SP-claiming weeks.  This is the most-honest per-SP number.
    total_sp = sum(r["sp"] for r in rows if r["sp"] > 0)
    tokens_per_sp = (totals["total"] / total_sp) if total_sp > 0 else 0

    # USD cost — weighted sum at the reference rates.  Single bottom-
    # line $ figure makes "tokens 1B vs 3B" comparable when one mix
    # is mostly cache-reads (cheap) and the other is heavy output
    # (expensive).
    total_usd     = compute_usd_cost(totals)
    usd_per_sp    = (total_usd / total_sp) if total_sp > 0 else 0

    # Schema v2.2: chat active time across unique dates.  See
    # `aggregate_chat_active_time_per_date` for the per-session
    # active-vs-idle gap heuristic.
    chat_secs_per_sp = (chat_active_seconds_total / total_sp) if total_sp > 0 else 0.0

    return {
        "n_weeks":               n,
        "mean_velocity":         round(mean, 2),
        "std_velocity":          round(std, 2),
        # Also report extremes — useful when std is high.
        "min_velocity":          round(min(velocities), 2),
        "max_velocity":          round(max(velocities), 2),
        # Schema v2.1: token totals — unique-date-summed, split by class
        # so the renderer can show the mix (cheap cache reads vs
        # expensive output).
        "total_tokens":          totals["total"],
        "total_input":           totals["input"],
        "total_output":          totals["output"],
        "total_cache_creation":  totals["cache_creation"],
        "total_cache_read":      totals["cache_read"],
        "tokens_per_sp_avg":     int(tokens_per_sp),
        # USD-equivalent cost at the reference rates (Sonnet 4.6).
        "total_usd":             round(total_usd, 4),
        "usd_per_sp_avg":        round(usd_per_sp, 4),
        # Schema v2.2 (post Week-16-Fri follow-up): active chat-time
        # spent on milestone implementation.  Distinct from Alex-days
        # (which counts CALENDAR days touched) — `chat_active_seconds`
        # is the WITHIN-day active time, capped per inter-message gap
        # at CHAT_ACTIVE_GAP_THRESHOLD_SEC (5 min).
        "chat_active_seconds":   round(chat_active_seconds_total, 1),
        "chat_seconds_per_sp":   round(chat_secs_per_sp, 1),
        "chat_gap_threshold_sec": CHAT_ACTIVE_GAP_THRESHOLD_SEC,
    }


# ── rendering ──────────────────────────────────────────────────────

def _fmt_tokens(n: int) -> str:
    """Compact: 1_234 → '1.2K', 1_234_567 → '1.2M'.  Empty cells show
    as '—' rather than '0K' so it's obvious there's no data."""
    if n <= 0:
        return "    —"
    if n >= 1_000_000:
        return f"{n/1_000_000:>5.1f}M"
    if n >= 1_000:
        return f"{n/1_000:>5.1f}K"
    return f"{n:>5}"


def _fmt_usd(d: float) -> str:
    """Format a small USD amount.  Below $1: dollars+cents.  $1+: round to
    nearest dollar.  Zero shows as '—'."""
    if d <= 0:
        return "    —"
    if d < 1:
        return f"${d:>4.2f}"
    if d < 1000:
        return f"${d:>4.0f}"
    return f"${d/1000:>4.1f}k"


def cloc_loc_for_language(language: str) -> int | None:
    """Run `cloc --fullpath --not-match-d=target .` from the git
    repo root and return the LoC count for the row whose Language
    column matches `language` (case-insensitive).

    Returns None on: cloc not on PATH; no matching language row; any
    parse failure.  Caller falls back to the code-stat cache.txt.

    Added in the perf-stat-filters follow-up.  Lets users compute
    tokens-per-LoC and $/LoC for non-Rust languages without first
    rewriting + re-running the code-stat skill.
    """
    try:
        repo_root = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            check=True, capture_output=True, text=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    try:
        result = subprocess.run(
            ["cloc", "--fullpath", "--not-match-d=target", "."],
            check=True, capture_output=True, text=True, cwd=repo_root,
        )
    except (subprocess.CalledProcessError, FileNotFoundError):
        return None
    needle = language.lower()
    # cloc's output table has rows like:
    #   Rust            33    2017    5848    16722
    # We want the rightmost numeric column (the `code` count) of the
    # row whose first column matches `language`.  Some language names
    # have spaces (e.g. "C/C++ Header"); use a regex that captures the
    # whole language portion up to multiple whitespace before digits.
    line_re = re.compile(r"^([A-Za-z0-9/_+#\.\- ]+?)\s{2,}\d+(?:\s+\d+){2,}\s+(\d+)\s*$")
    for line in result.stdout.splitlines():
        m = line_re.match(line)
        if not m:
            continue
        lang_in_line = m.group(1).strip().lower()
        if lang_in_line == needle:
            try:
                return int(m.group(2))
            except ValueError:
                return None
    return None


def render_table(
    rows:     list[dict[str, Any]],
    s:        dict[str, Any],
    language: str | None = None,
) -> str:
    out: list[str] = []
    # Token columns appear only when at least one week has tokens
    # data (i.e. ~/.claude/projects/ existed + had usable JSONLs).
    has_tokens = any(
        r.get("tokens", {}).get("total", 0) > 0 for r in rows
    )

    if has_tokens:
        # v2.1 split-by-token-class columns + USD cost.  The pricing-
        # reference USD column uses Anthropic Sonnet 4.6 list prices
        # (configurable as PRICE_*_PER_M constants at the top).  Cost
        # is weighted by class: output is 5× input, cache reads are
        # 10× cheaper than uncached input.  This makes per-week
        # comparison meaningful even when token mixes differ wildly.
        #
        # v2.2: trailing `chat` column carries within-day active chat
        # time (idle gaps ≥5 min excluded — see CHAT_ACTIVE_GAP_*).
        # Distinct from `Days` (calendar days touched) — `chat`
        # answers "how many hours of focused chat work did this week
        # cost?"  Empty (—) when JSONL telemetry is unavailable.
        out.append(
            "Week  SP   Days  Vel.   In       Cache-R  Out      $ cost   Chat")
        out.append(
            "----  ---  ----  -----  -------  -------  -------  -------  -------")
        for r in rows:
            tk = r.get("tokens", {})
            # "In" = uncached input + cache writes (both billed at
            # input-ish rates; combine for legibility).
            in_combined = tk.get("input", 0) + tk.get("cache_creation", 0)
            usd = compute_usd_cost(tk)
            chat = r.get("chat_active_seconds", 0.0)
            out.append(
                f"{r['week']:<4}  {r['sp']:<3}  {r['alex_days']:<4}  "
                f"{r['velocity']:>5.2f}  "
                f"{_fmt_tokens(in_combined):>7}  "
                f"{_fmt_tokens(tk.get('cache_read', 0)):>7}  "
                f"{_fmt_tokens(tk.get('output', 0)):>7}  "
                f"{_fmt_usd(usd):>7}  "
                f"{_fmt_duration(chat):>7}"
            )
    else:
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
        if has_tokens:
            total_tokens = s.get("total_tokens", 0)
            per_sp       = s.get("tokens_per_sp_avg", 0)
            total_usd    = s.get("total_usd", 0.0)
            usd_per_sp   = s.get("usd_per_sp_avg", 0.0)
            out.append("")
            out.append("         ── tokens (counts) ──")
            out.append(
                f"         total tokens                = {_fmt_tokens(total_tokens).strip()}"
            )
            out.append(
                f"         tokens / SP (mean)          = {_fmt_tokens(per_sp).strip()}"
            )
            # Class breakdown.
            tot_in     = s.get("total_input",          0)
            tot_out    = s.get("total_output",         0)
            tot_cr     = s.get("total_cache_read",     0)
            tot_cc     = s.get("total_cache_creation", 0)
            grand = max(tot_in + tot_out + tot_cr + tot_cc, 1)
            out.append(
                f"             input (uncached)        = {_fmt_tokens(tot_in).strip():>7}"
                f"  ({100*tot_in/grand:>4.1f}%)"
            )
            out.append(
                f"             cache writes (one-time) = {_fmt_tokens(tot_cc).strip():>7}"
                f"  ({100*tot_cc/grand:>4.1f}%)"
            )
            out.append(
                f"             cache reads (cheap)     = {_fmt_tokens(tot_cr).strip():>7}"
                f"  ({100*tot_cr/grand:>4.1f}%)"
            )
            out.append(
                f"             output (expensive)      = {_fmt_tokens(tot_out).strip():>7}"
                f"  ({100*tot_out/grand:>4.1f}%)"
            )
            out.append("")
            out.append("         ── cost (USD-equiv at Sonnet 4.6 rates) ──")
            out.append(
                f"         total $ cost                = ${total_usd:,.2f}"
            )
            out.append(
                f"         $ per SP (mean)             = ${usd_per_sp:,.2f}"
            )
            # Bottom-line tokens/USD per LoC.  Two sources for the
            # current LoC, in priority order:
            #   1. If `language` is set (perf-stat-filters follow-up),
            #      run `cloc` inline against the repo root and pick
            #      the row matching that language — bypasses
            #      code-stat's cache so the user can probe arbitrary
            #      languages without re-running code-stat each time.
            #   2. Otherwise fall back to the cached LoC code-stat
            #      wrote on its last run (the legacy path).
            loc_now: int | None = None
            loc_label = language if language else "Rust"
            if language:
                loc_now = cloc_loc_for_language(language)
            if loc_now is None:
                code_stat_cache = SCRIPT_DIR.parent / "code-stat" / "cache.txt"
                if code_stat_cache.exists():
                    try:
                        loc_now = int(code_stat_cache.read_text().strip().replace(",", ""))
                    except (ValueError, OSError):
                        loc_now = None
            if loc_now is not None and loc_now > 0:
                out.append("")
                out.append(f"         ── per {loc_label} LoC (rough, current LoC not delta) ──")
                out.append(
                    f"         current {loc_label} LoC            = {loc_now:,}"
                )
                out.append(
                    f"         tokens / LoC                = "
                    f"{total_tokens / loc_now:,.0f}"
                )
                out.append(
                    f"         $ / LoC                     = "
                    f"${total_usd / loc_now:.3f}"
                )
            # Chat active-time totals — v2.2 schema.  Distinct from
            # Alex-days (calendar days touched) — answers "how many
            # hours of actually-typing-with-Claude did the milestone
            # cost?"  Idle gaps ≥ CHAT_ACTIVE_GAP_THRESHOLD_SEC are
            # excluded so overnight / lunch-break sessions don't
            # inflate the number.
            chat_total = s.get("chat_active_seconds", 0.0)
            chat_per_sp = s.get("chat_seconds_per_sp", 0.0)
            gap = s.get("chat_gap_threshold_sec", CHAT_ACTIVE_GAP_THRESHOLD_SEC)
            if chat_total > 0:
                out.append("")
                out.append("         ── chat time (active wall-clock) ──")
                out.append(
                    f"         total active chat time      = "
                    f"{_fmt_duration(chat_total).strip()}"
                )
                out.append(
                    f"         chat time / SP (mean)       = "
                    f"{_fmt_duration(chat_per_sp).strip()}"
                )
                out.append(
                    f"         (gaps ≥{gap // 60} min treated as idle "
                    "+ excluded; tune CHAT_ACTIVE_GAP_THRESHOLD_SEC)"
                )

            # Pricing reference footer.
            out.append("")
            out.append(
                f"         (Sonnet 4.6: ${PRICE_INPUT_PER_M:.2f}/M in, "
                f"${PRICE_OUTPUT_PER_M:.2f}/M out, "
                f"${PRICE_CACHE_CREATION_PER_M:.2f}/M cache-write, "
                f"${PRICE_CACHE_READ_PER_M:.2f}/M cache-read; "
                f"adjust PRICE_*_PER_M in analyze.py for other models.)"
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

def cmd_retrofit(
    project_filter: str | None = None,
    language:       str | None = None,
) -> int:
    rows_raw = git_log()
    if not rows_raw:
        print("perf-stat: no git history; run from a checked-out repo.",
              file=sys.stderr)
        return 1
    weeks = aggregate(rows_raw)
    # v2: token aggregation.  Returns {} on missing / empty
    # ~/.claude/projects/ — table render gracefully degrades to
    # the v1 shape (no token columns) when this is empty.
    tokens_per_date = aggregate_tokens_per_date(project_filter)
    if not weeks:
        print("perf-stat: no `Week N <Day>:` commits found.", file=sys.stderr)
        return 1
    # v2.2: chat active-time aggregation (post Week-16-Fri follow-up).
    # Empty when ~/.claude/projects/ is missing or has no parseable
    # JSONLs — render gracefully degrades.
    chat_time_per_date = aggregate_chat_active_time_per_date(project_filter)
    rows = compute_velocity(weeks, tokens_per_date, chat_time_per_date)
    s = summary(rows, tokens_per_date, chat_time_per_date)
    write_cache(rows, s)
    print(render_table(rows, s, language))
    return 0


def cmd_report(language: str | None = None) -> int:
    cache = read_cache()
    if cache is None:
        print("perf-stat: no cache.json; run `perf-stat retrofit` first.",
              file=sys.stderr)
        return 2
    print(render_table(cache["weeks"], cache["summary"], language))
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
    # Two new optional flags (perf-stat-filters follow-up):
    #   --project-filter PATTERN   substring match against
    #                              ~/.claude/projects/* basenames
    #   --language LANG            cloc language row to use for
    #                              the per-LoC ratios (default: Rust
    #                              via code-stat's cached LoC)
    #
    # Flags can appear anywhere on the line; the remaining positional
    # arg (or absence) selects the mode (retrofit / report / estimate).
    # Hand-rolled flag parser to keep "stdlib only" honest and avoid
    # argparse's help-noise on this tiny surface.
    raw = list(argv[1:])
    project_filter: str | None = None
    language:       str | None = None
    positional: list[str]      = []
    i = 0
    while i < len(raw):
        tok = raw[i]
        if tok == "--project-filter":
            if i + 1 >= len(raw):
                print("perf-stat: --project-filter needs a PATTERN argument",
                      file=sys.stderr)
                return 3
            project_filter = raw[i + 1]
            i += 2
        elif tok.startswith("--project-filter="):
            project_filter = tok.split("=", 1)[1]
            i += 1
        elif tok == "--language":
            if i + 1 >= len(raw):
                print("perf-stat: --language needs a LANG argument",
                      file=sys.stderr)
                return 3
            language = raw[i + 1]
            i += 2
        elif tok.startswith("--language="):
            language = tok.split("=", 1)[1]
            i += 1
        else:
            positional.append(tok)
            i += 1

    if not positional or positional[0] == "retrofit":
        return cmd_retrofit(project_filter, language)
    if positional[0] == "report":
        return cmd_report(language)
    if positional[0] == "estimate":
        if len(positional) != 2:
            return usage()
        return cmd_estimate(positional[1])
    return usage()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
