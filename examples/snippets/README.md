# `examples/snippets/` — value-expression fragments

Each `.ais` file in this directory contains **a single value expression**, not a full project. They are *not* runnable through `aislang validate` — that command rejects them as missing a `project` block.

## What they're for

1. **Living documentation** of every literal kind the value parser accepts. New contributors can read these files instead of digging through `parser.rs` to learn what a `Money` literal looks like, or how `500ms` differs from `5s`, or what `1MB` actually expands to.

2. **Snapshot fixtures.** The `snippet_tests` module in `crates/aislang-core/src/parser.rs` loads each fragment via `std::fs` and parses it through `value_parser()`, asserting the expected `RawValue` shape. If the parser ever drifts on a literal kind, the matching snippet test fires with a clear pointer.

3. **Diff-stable parser-change reviews.** When a future change adds (or, deliberately, breaks) a literal form, the change to a snippet file is the human-readable evidence of intent, separately reviewable from the parser code.

## Convention

- Filename: `<kind>-<variant>.ais`, e.g. `money-50-usd.ais`, `duration-500ms.ais`.
- File body: one or more comment lines (`#` or `//`) explaining the literal, then the expression itself on its own line.
- Each fragment is the smallest text that exercises one parser path.

## Adding a new fragment

When a literal kind is added to `value_parser` (e.g. v0.2 ISO-code currency aliases like `USD 50`), drop a new file here in the same commit and add a matching `snippet_tests` case. The test scans this directory and fails on any new file lacking a test — keeping the two sides in sync mechanically.

## Why a separate directory rather than test data files

Test data files would be invisible to anyone browsing the repo. Putting these in `examples/snippets/` with a README means they're the first thing someone looking for "what does aiSlang syntax look like for X?" actually finds.
