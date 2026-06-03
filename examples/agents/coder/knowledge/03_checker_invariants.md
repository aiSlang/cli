# `aislang-core::checker` invariant patterns

The checker runs AFTER parsing.  Its job is to enforce
cross-block invariants the parser can't see at single-block
scope.  One `validate_<block>` fn per block.

## Standard validate fn

```rust
fn validate_datasets(proj: &Project) -> Result<(), CheckError> {
    reject_duplicate_labels(&proj.datasets, "dataset")?;
    for ds in &proj.datasets {
        // per-instance range/cross-field checks here
    }
    Ok(())
}
```

- Duplicate-label rejection is universal — every block gets it
  via `reject_duplicate_labels`.
- Per-instance checks come second.  Examples:
  - "field A must be > field B" (cross-field, single block).
  - "ref X must resolve to an existing block Y" (cross-block,
    checker-only).

## Dispatch hook in `Project::validate`

```rust
pub fn validate(&self) -> Result<(), CheckError> {
    validate_models(self)?;
    // ... existing arms ...
    validate_datasets(self)?;
    Ok(())
}
```

One call per validate fn.  Order matters when one block's
invariants depend on another being valid first (e.g.
`vector_store.embedding` must point at a real `embedding`
block — validate embeddings before vector_stores).

## Common CheckError variants

```rust
pub enum CheckError {
    DuplicateLabel  { block_kind: BlockKind, label: String, span: Span },
    DanglingRef     { from: BlockKind, to: RefKind, label: String, span: Span },
    InvalidRange    { field: &'static str, value: String, span: Span },
    InvalidCombination { msg: String, span: Span },
    // ...
}
```

When adding a new check, reach for an existing variant before
adding a new one.  `InvalidCombination` is the catch-all for
cross-field rules ("`exec` and `mcp` are mutually exclusive").

## Persistent-state rule (Decision 44.4)

If the new block writes any state to disk, it MUST write under
`<source_dir>/.aislang/<subtree>/`.  The Architect's "Decision
implications" section spells this out.  No checker assertion
needed at the Rust layer — the runtime is responsible — but
mention it in a `// Decision 44.4: writes to .aislang/<subtree>/`
comment next to the relevant validate fn so the merger can
spot it.
