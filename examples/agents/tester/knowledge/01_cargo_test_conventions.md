# `cargo test` conventions in this repo

Tests live in either `src/lib.rs` (`#[cfg(test)] mod tests`) or
`tests/<name>.rs` (integration).  This corpus describes the
naming + invocation rules the Tester agent's recommendations
follow.

## Canonical invocation

```bash
cargo test --workspace --no-fail-fast
```

- `--workspace` covers every crate in the merged Cargo.lock so
  a patch in `aislang-core` that breaks `aislang-runtime`
  surfaces.
- `--no-fail-fast` keeps the failure surface complete on first
  run.  Without it, the first per-crate failure aborts the run
  + masks parallel-crate failures.

## Test name convention

```
<verb>_<subject>_<condition>
```

Examples from the repo:
- `parser_accepts_dataset_block` — happy path.
- `parser_rejects_dataset_block_with_unknown_format` — range/
  enum check.
- `checker_rejects_duplicate_dataset_labels` — standard
  invariant.
- `roundtrip_dataset_block_is_byte_stable` — Decision-44.4
  byte-stability snapshot.

## Filter syntax

```bash
cargo test --workspace --no-fail-fast -- parser_accepts_dataset_block
```

The `--` passes everything after to the test harness, which
treats it as a substring filter against the test path.

Use a filter only when ≤4 specific names need running; ≥5 →
drop the filter (broad runs are cheap).

## Exit-status interpretation

- `0` — all matched tests passed.
- non-zero — at least one test failed; stdout/stderr lists
  which.  Parse `test <name> ... FAILED` lines for the failing
  set; their preceding `panicked at` lines have the assertion
  detail.

## When the Tester should rerun vs. re-design

- If the recommended invocation surfaces test failures whose
  assertions name fields the Coder DID NOT cover → the gap is
  test-side (Coder forgot a test).  Reflector pattern: feed
  back to the Coder for revision.
- If the recommended invocation surfaces compile errors in the
  Coder's patch → the gap is patch-side (syntax / type bug).
  Reflector pattern: feed back to the Coder for revision with
  the verbatim compiler error.
- If the recommended invocation passes but the user reports
  the runtime behaviour is wrong → out of scope for v0.3
  Tester.  The Tester checks Cargo-level correctness; semantic
  correctness is the Reviewer's job (with the Architect's
  design as the ground truth).
