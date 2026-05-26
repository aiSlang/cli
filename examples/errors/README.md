# `examples/errors/` — intentionally-malformed fixtures

Each `.ais` file in this directory is **expected to fail** `aislang validate`. They exercise specific error categories so the source-aware diagnostic rendering (line:col, snippet, caret) stays stable across parser refactors.

## Convention

- Filename: `<category>-<specific>.ais` — e.g. `missing-list-comma.ais`, `unquoted-money.ais` (future).
- Header comment in each file: what's intentionally wrong + the precise location (line/column the parser should flag).
- Paired with a test case in `crates/aislang-core/tests/error_fixtures.rs` that loads the file, calls `aislang_core::validate`, and asserts the resulting `ValidateError::Parse(…)` renders with the expected source-snippet shape.

## Why a separate directory rather than inline test strings

1. **Visibility.** Anyone browsing the repo sees both the source that fails AND the rendered diagnostic alongside the test — easier to evaluate UX changes than reading `let src = r#"..."#;` blocks inside Rust test files.
2. **Stable regression coverage.** When the rendering format changes (e.g. switching to colored output later), the fixture text stays put; only the expected-output assertion adjusts.
3. **Documentation.** New contributors can see real-source examples of error categories without grepping the test crate.

## Adding a new fixture

When a new parser error category becomes worth pinning:
1. Drop a fixture file here with a header comment naming the intentional defect and its expected line/column.
2. Add a test case in `crates/aislang-core/tests/error_fixtures.rs` that loads it and asserts the rendered output contains the expected substrings.
3. Manually run `aislang validate examples/errors/<your-file>.ais` to confirm the diagnostic reads well to a human.

## Distinction from neighbouring directories

- `examples/snippets/` — value-expression fragments that parse successfully via `value_parser()`. NOT full projects.
- `examples/errors/` — full projects (or things that try to be) that fail at some specific stage. Pinned for error-rendering regression coverage.
- `examples/{support-agent, minimal}.ais` — canonical success-path fixtures.
