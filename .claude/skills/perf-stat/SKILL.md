---
name: perf-stat
description: Story-Points-style velocity tracking against PLAN-90-days.md. Treats each `Week N <Mon|Tue|Wed|Thu|Fri>` day-row as 1 SP (Story Point — one planned man-day for an unaccelerated solo dev). Cross-references git log for actual completion timestamps, clusters by calendar date, and reports per-week velocity (SP/Alex-day). Use after each milestone closes to record factual velocity; use before any new sprint to convert SP estimates into Alex-day projections.
---

# perf-stat

When the user invokes this skill (e.g. `/perf-stat`, `/perf-stat estimate 12`, or at the end of a milestone), do the following.

## What this skill does

The 12-week PLAN-90-days.md was written assuming a generic solo developer working unaccelerated. Real wall-clock time per "planned day" is dramatically lower with Claude Code in the loop — but until we measure it, every v0.2+ estimate is a guess. This skill:

1. **Treats each commit message starting `Week N <Day>:`** as one delivered Story Point. The convention is already established across Weeks 1–11 of v0.1.
2. **Counts distinct calendar dates per week** as the Alex-day denominator. (Calendar days are a stable proxy; trying to measure hours would over-complicate without adding signal.)
3. **Velocity (per week) = SP delivered / Alex-days actual.** A velocity of 3.0 SP/day means Alex compresses 3 planned man-days into one calendar day of real work.
4. **Persists history** to `.claude/skills/perf-stat/cache.json` so each run shows historical velocity + new deltas.

## Operating modes

Invoke via the helper script:

```bash
.claude/skills/perf-stat/run.sh                    # retrofit + report (default)
.claude/skills/perf-stat/run.sh estimate <SP>      # SP → Alex-days projection
.claude/skills/perf-stat/run.sh report             # show table without re-running git
```

### Default (`retrofit + report`)

Walks the git log, recomputes the per-week velocity table, atomically rewrites `cache.json`, then prints a human-readable summary on stdout:

```
Week  SP   Alex-days  Velocity (SP/day)
----  ---  ---------  -----------------
1     5    2          2.50
2     5    3          1.67
...
mean       —          2.10  (std=0.45, n=11)
```

### `estimate <SP>` mode

Reads `cache.json` (no git access), multiplies the input by the rolling-mean velocity, and emits:

```
12 SP ≈ 5.7 Alex-days   (mean velocity 2.10 SP/day, n=11)
                         range: 4.3 .. 7.8 Alex-days (±1σ)
```

The ±1σ range is the honest "this is the spread, not a guaranteed estimate" hedge — actual Alex-days for the next sprint will land somewhere in that band more often than not.

## When to use it

- **After each milestone closes** (e.g. on v0.1 close, v0.2 mid-track checkpoint): run with no args to refresh `cache.json` + print the new row.
- **Before starting a new sprint**: run with `estimate <SP>` where SP is the sum of day-rows in the planned section. Include the projected Alex-days in any new PLAN entry so the calendar reflects reality, not the unaccelerated-baseline fantasy.
- **In end-of-turn summaries** if the turn closed a milestone: append a `Velocity: X SP/day` line alongside the existing `Tests:` and `LoC (Rust):` lines from `code-stat`.

## Including it in summaries

When a turn closes a numbered week, add velocity to the end-of-turn summary alongside other metrics:

```
Tests: 599 → 599 (+0).
LoC (Rust): 12684 → 12711 (+27).
Velocity (this week): 5 SP / 1 Alex-day = 5.00 SP/day.
```

When the turn is mid-sprint (no week-close), skip the velocity line — only a closed week produces a clean SP/day reading.

## Implementation notes

The helper script (`run.sh`) is a thin bash dispatcher; the actual analysis lives in `analyze.py` (Python 3, stdlib only — no `pip install`). Algorithm:

1. `git log --pretty='%aI|%s' --no-merges --reverse` from repo root.
2. Match commit subject against `^Week (\d+) (Mon|Tue|Wed|Thu|Fri)\b` (case-insensitive). Each match = +1 SP for that week. Commits not matching this pattern (e.g. mid-week hotfixes like `Pin LiteLLM default URL …`) are NOT counted as SP — they're work but not against a planned slot. They DO count toward Alex-days if their date intersects an SP-claimed week.
3. For each week, extract distinct `YYYY-MM-DD` dates across all commits attributable to that week (whether SP-claiming or hotfix-shape).
4. Velocity = SP / distinct_dates.
5. Mean + stdev across all completed weeks.

## Limitations & future work

- **Mid-week hotfixes** (e.g. `Eval UX: longer LiteLLM timeout`) currently inflate the Alex-days count without contributing SP. This is a feature, not a bug — hotfix work IS real time spent. But it means a hotfix-heavy week looks slower than a hotfix-free one. v0.2 may add a `--exclude-pattern` flag.
- **The 5-day work week assumption** is implicit. A week that genuinely needed 7 SP (Sat/Sun additions) would over-count Alex-days. None of v0.1's weeks did this, so it's fine for the retrofit.
- **Cross-project comparison** isn't supported. This skill measures velocity in *this* repo only. Personal-baseline-across-projects would need a different tool.

## Cache file

`.claude/skills/perf-stat/cache.json` is gitignored. Shape:

```json
{
  "schema_version": 1,
  "computed_at": "2026-05-28T18:00:00+02:00",
  "weeks": [
    { "week": 1, "sp": 5, "alex_days": 2, "velocity": 2.50, "dates": ["2026-..."] },
    ...
  ],
  "summary": { "n_weeks": 11, "mean_velocity": 2.10, "std_velocity": 0.45 }
}
```

The schema is intentionally explicit so future tooling (a v0.2+ "perf dashboard") can consume it.
