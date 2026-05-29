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


def aggregate_tokens_per_date() -> dict[str, dict[str, int]]:
    """Walk every Claude session JSONL across all project dirs;
    return {YYYY-MM-DD: {input, output, cache_creation, cache_read, total}}.

    Why all projects: Claude Code records sessions under a dir named
    after the CWD where Claude was launched.  Your aiSlang work may
    live under a non-aiSlang project dir (e.g. you launched Claude
    from a different repo and then `cd`'d).  Aggregating all dirs
    captures the full token spend; date-bucketing keeps the per-week
    rollup honest (only dates with git commits to THIS repo count
    toward THIS repo's week table — see compute_velocity's loop).

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
    for project_dir in CLAUDE_PROJECTS_DIR.iterdir():
        if not project_dir.is_dir():
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
    weeks:           dict[int, dict[str, Any]],
    tokens_per_date: dict[str, dict[str, int]] | None = None,
) -> list[dict[str, Any]]:
    """Render the per-week table rows.  When `tokens_per_date` is
    provided (schema v2), each row gains a `tokens` dict summed
    across that week's dates + a tokens_per_sp ratio."""
    tokens_per_date = tokens_per_date or {}
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

        out.append({
            "week":                w,
            "sp":                  sp,
            "alex_days":           alex_days,
            "velocity":            round(velocity, 2),
            "tokens":              week_tokens,
            "tokens_per_sp":       int(tokens_per_sp),
            "tokens_per_alex_day": int(tokens_per_alex_day),
            "dates":               sorted(info["dates"]),
        })
    return out


def summary(
    rows:            list[dict[str, Any]],
    tokens_per_date: dict[str, dict[str, int]] | None = None,
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
    tokens_per_date = tokens_per_date or {}
    unique_dates: set[str] = set()
    for r in rows:
        for d in r.get("dates", []):
            unique_dates.add(d)

    # Per-class totals across unique dates.
    totals = {"input": 0, "output": 0, "cache_creation": 0, "cache_read": 0, "total": 0}
    for d in unique_dates:
        if d in tokens_per_date:
            for k in totals:
                totals[k] += tokens_per_date[d][k]

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


def render_table(rows: list[dict[str, Any]], s: dict[str, Any]) -> str:
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
        out.append("Week  SP   Days  Vel.   In       Cache-R  Out      $ cost")
        out.append("----  ---  ----  -----  -------  -------  -------  -------")
        for r in rows:
            tk = r.get("tokens", {})
            # "In" = uncached input + cache writes (both billed at
            # input-ish rates; combine for legibility).
            in_combined = tk.get("input", 0) + tk.get("cache_creation", 0)
            usd = compute_usd_cost(tk)
            out.append(
                f"{r['week']:<4}  {r['sp']:<3}  {r['alex_days']:<4}  "
                f"{r['velocity']:>5.2f}  "
                f"{_fmt_tokens(in_combined):>7}  "
                f"{_fmt_tokens(tk.get('cache_read', 0)):>7}  "
                f"{_fmt_tokens(tk.get('output', 0)):>7}  "
                f"{_fmt_usd(usd):>7}"
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
            # Bottom-line tokens/USD per LoC if code-stat cache exists.
            code_stat_cache = SCRIPT_DIR.parent / "code-stat" / "cache.txt"
            if code_stat_cache.exists():
                try:
                    loc_now = int(code_stat_cache.read_text().strip().replace(",", ""))
                    if loc_now > 0:
                        out.append("")
                        out.append("         ── per Rust LoC (rough, current LoC not delta) ──")
                        out.append(
                            f"         current Rust LoC            = {loc_now:,}"
                        )
                        out.append(
                            f"         tokens / LoC                = "
                            f"{total_tokens / loc_now:,.0f}"
                        )
                        out.append(
                            f"         $ / LoC                     = "
                            f"${total_usd / loc_now:.3f}"
                        )
                except (ValueError, OSError):
                    pass
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

def cmd_retrofit() -> int:
    rows_raw = git_log()
    if not rows_raw:
        print("perf-stat: no git history; run from a checked-out repo.",
              file=sys.stderr)
        return 1
    weeks = aggregate(rows_raw)
    # v2: token aggregation.  Returns {} on missing / empty
    # ~/.claude/projects/ — table render gracefully degrades to
    # the v1 shape (no token columns) when this is empty.
    tokens_per_date = aggregate_tokens_per_date()
    if not weeks:
        print("perf-stat: no `Week N <Day>:` commits found.", file=sys.stderr)
        return 1
    rows = compute_velocity(weeks, tokens_per_date)
    s = summary(rows, tokens_per_date)
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
