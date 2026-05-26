---
name: code-stat
description: Report Rust LoC delta since the last skill run. Uses `cloc` under the hood; persists the previous total in a local cache so each run shows "LoC (Rust): <prev> -> <curr> (+/-<delta>)". Intended to sit alongside the `Tests: N -> M (+K)` line in end-of-turn summaries so the user sees both test growth AND code-size growth on each meaningful unit of work.
---

# code-stat

When the user invokes this skill (or when you're about to summarize a meaningful unit of work that included Rust-code changes), do the following:

1. **Run the helper script** from the project root:
   ```bash
   .claude/skills/code-stat/run.sh
   ```
   It prints one line on stdout in the format:
   ```
   LoC (Rust): <prev> -> <curr> (+|-<delta>)
   ```
   …and atomically updates `.claude/skills/code-stat/cache.txt` to the new value.

2. **Include that line in your summary** alongside any test-count line you'd normally emit. Example end-of-turn summary block:
   ```
   Tests: 404 → 420 (+16).
   LoC (Rust): 8738 → 8870 (+132).
   ```
   Format the delta with the literal `+` or `-` sign so additions and removals are equally visible.

3. **First-run behavior:** the cache starts empty. The first invocation reports `LoC (Rust): N → N (+0)` (zero delta) and seeds the cache. From the second run onward the delta is meaningful.

4. **Failure modes:** the script's exit code is non-zero (and stderr carries a one-line diagnostic) if `cloc` isn't on PATH or no Rust code was found. In that case, surface the error to the user and skip the LoC line in your summary — don't fail the entire turn over it.

## What the script counts

Mirrors the user-provided invocation:
```
cloc --fullpath --not-match-d=target .
```
…then extracts the "code" column from the `Rust` row only. Other languages (Markdown, JSON, TOML, YAML) are intentionally NOT reported — they're noise for this signal. If you want a broader breakdown, the user can run the bare `cloc` command themselves.

## Cache file

`.claude/skills/code-stat/cache.txt` is gitignored. Each developer's local
`prev` is their own; deltas are personal to whoever's running the skill
in their checkout.
