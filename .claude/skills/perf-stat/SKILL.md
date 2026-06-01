---
name: perf-stat
description: Story-Points-style velocity tracking against PLAN-90-days.md PLUS token-spend tracking PLUS chat-active-time tracking from Claude Code session JSONLs (schema v2.2, chat-time added post Week-16 Fri). Treats each `Week N <Mon|Tue|Wed|Thu|Fri>` day-row as 1 SP. Cross-references git log for completion timestamps; walks ~/.claude/projects/*/*.jsonl for per-message token usage AND inter-message active-time deltas (idle gaps ≥5 min excluded); aggregates all three per calendar date. Per-week report shows velocity (SP/Alex-day) + tokens (total, T/SP, T/day) + chat time. Summary appends project-wide token totals + tokens/SP + tokens/Rust-LoC bottom-line ratio + total active chat hours + chat-time/SP.
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

### Optional flags (perf-stat-filters follow-up)

Two flags refine the token + LoC attribution; either or both can be combined with any mode:

```bash
# Restrict token aggregation to one Claude-Code project dir.
# PATTERN is a case-insensitive SUBSTRING match against subdir
# names under ~/.claude/projects/.
.claude/skills/perf-stat/run.sh --project-filter aiSlang
.claude/skills/perf-stat/run.sh retrofit --project-filter DoorMonitor-Firmware-CubeIDE

# Compute per-LoC ratios for a non-Rust language.  Bypasses the
# code-stat cache.txt and runs `cloc` inline against the repo
# root.  Default: read code-stat's cache.txt (which is Rust for
# aiSlang).
.claude/skills/perf-stat/run.sh --language Python
.claude/skills/perf-stat/run.sh --language "C/C++ Header"
```

**Why `--project-filter`.**  Claude Code records sessions under a dir named after the launching CWD.  Users routinely work on project X from a CWD belonging to project Y; without a filter, perf-stat counts ALL Claude-Code spend on dates that have commits to THIS repo — including side-quests on other repos that happened the same day.  For per-project cost analysis (e.g. "how much did v0.2 actually cost vs. unrelated work on the same days"), `--project-filter aiSlang` narrows to one project subdir.  Wide-net default is kept because sessions can `cd` mid-stream; restricting too aggressively misses spend that genuinely belongs to the project under analysis.

**Why `--language`.**  perf-stat displays tokens-per-LoC and $/LoC as a "how much AI co-development cost per unit of delivered software" baseline.  The legacy path reads `code-stat`'s cache.txt (always Rust for aiSlang).  For non-Rust projects (or to probe Python tooling LoC like the perf-stat skill itself, ~500 lines), `--language Python` re-runs `cloc` inline and picks the matching row.  cloc must be on PATH.

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

`.claude/skills/perf-stat/cache.json` is gitignored. Schema v2 (Phase 1 Week 12 close):

```json
{
  "schema_version": 2,
  "computed_at": "2026-05-29T...",
  "weeks": [
    {
      "week": 12,
      "sp": 5,
      "alex_days": 1,
      "velocity": 5.00,
      "tokens": {
        "input":          12345,
        "output":         67890,
        "cache_creation": 11111,
        "cache_read":     22222,
        "total":         113568
      },
      "tokens_per_sp":       22713,
      "tokens_per_alex_day": 113568,
      "dates": ["2026-05-29"]
    },
    ...
  ],
  "summary": {
    "n_weeks":           11,
    "mean_velocity":     4.09,
    "std_velocity":      1.61,
    "min_velocity":      1.00,
    "max_velocity":      7.00,
    "total_tokens":      2933000000,
    "tokens_per_sp_avg": 55300000
  }
}
```

Two new schema-v2 derived ratios appear in the rendered summary but not in cache.json:

- **`current Rust LoC`** — read from `.claude/skills/code-stat/cache.txt` at report time.
- **`total tokens / current LoC`** — `summary.total_tokens / current_loc`. Rough proxy for "tokens per LoC produced." See the limitation note below — this divides by current LoC, not LoC delta over the token window.

The schema is intentionally explicit so future tooling (a v0.2+ "perf dashboard") can consume it.

## Token tracking — what's counted, what isn't

`aggregate_tokens_per_date()` walks **every** subdirectory of `~/.claude/projects/` and tallies tokens by message timestamp.  Why all subdirs: Claude Code names each subdir after the CWD it was launched from, but a session might touch multiple repos (you launch from one dir and `cd` to another).  Date-bucketing keeps the per-week numbers honest — only dates that have git commits to THIS repo land in THIS repo's week buckets.

What we count per message:
- `input_tokens`              — uncached input
- `output_tokens`             — model output
- `cache_creation_input_tokens` — input being cached now (~1.25× normal price)
- `cache_read_input_tokens`   — input read from cache (~0.1× normal price)
- `total` = sum of all four

**Caveats:**

1. **Cross-week date double-counting in per-week columns.**  A calendar day with commits attributed to multiple weeks (e.g. Week 10 Fri close-out and Week 11 Mon scaffold land on the same day) contributes its tokens to BOTH weeks' per-week totals.  The summary's `total_tokens` uses unique dates across all weeks so the project-wide total isn't inflated — but the per-week column will sometimes appear "duplicated" for cross-week days.  Documented; v0.3 may add per-commit token attribution if a finer grain is needed.

2. **All Claude work in the date window counts.**  If you spent half a day on aiSlang and half on another project, both halves' tokens land in this repo's perf-stat (because we filter by date, not by which repo the work was for).  This is a feature: tokens spent on a date with aiSlang commits represent the "total Claude Code spend that contributed to your aiSlang velocity that day," including reading docs, side-tasks, etc.  Not just aiSlang code generation.

3. **`tokens / LoC` is `total tokens / CURRENT LoC`**, NOT `tokens / LoC delta`.  The skill doesn't track historical LoC at past commits.  For a more accurate per-period ratio, divide per-week token totals by the LoC delta you tracked manually via `code-stat` that week.  v0.3 may track per-week LoC deltas natively.

4. **Failed parses are silent.**  Corrupt JSONL lines or missing `usage` fields contribute zero.  Token metric is informational; broken telemetry shouldn't break the perf-stat run.

## Chat active time — schema v2.2 (post-Week-16-Fri follow-up)

The `Chat` column on the per-week table + the **"chat time (active wall-clock)"** summary block answer a complementary question to tokens + Alex-days:

- **Alex-days** = how many CALENDAR days saw a commit attributable to the week.  Coarse — a 10-minute morning slot and an 8-hour focused session both count as one day.
- **Chat active time** = within-day active wall-clock spent typing with / waiting for Claude.  Captures the actual cognitive cost of the milestone.

### Algorithm (`aggregate_chat_active_time_per_date`)

Per session JSONL file:
1. Sort messages by timestamp.
2. For each consecutive pair `(cur, nxt)`:
   - If `nxt.ts - cur.ts < CHAT_ACTIVE_GAP_THRESHOLD_SEC` (default 300s / 5 min), attribute `delta` seconds to the **date of `cur.ts`**.
   - Otherwise (idle gap — user walked away, slept, switched repos), contribute zero.
3. First message of each session contributes nothing (active time is INTER-message).

### Tuning

The 300s threshold balances two failure modes:

- **Too low (e.g. 60s)**: a thoughtful pause where the user reads a long tool result resets the clock → undercounts.
- **Too high (e.g. 30m)**: overnight or coffee-break gaps get counted as continuous → wildly overcounts.

Tune via `CHAT_ACTIVE_GAP_THRESHOLD_SEC` at the top of `analyze.py`.

### When to look at it vs. Alex-days

| Question | Metric |
|---|---|
| "How many CALENDAR days did this milestone span?" | Alex-days |
| "How many HOURS of actual chat work did this milestone cost?" | Chat active time |
| "What's our SP/calendar-day velocity?" | `velocity` (= SP / Alex-days) |
| "How many minutes of chat per SP?" | `chat_seconds_per_sp` |

Chat-per-SP is a much tighter productivity proxy than velocity — it's robust against the "spent 6 hours over 3 calendar days" vs. "spent 1 hour over 1 day" inflation that Alex-days has.
