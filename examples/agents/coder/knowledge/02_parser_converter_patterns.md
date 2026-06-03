# `aislang-core::parser` converter patterns

The parser turns raw HCL AST into the typed `Project` struct.
One converter fn per block.  Mirror the shape exactly.

## Per-block converter fn

```rust
fn dataset_from_body(label: String, body: hcl::Body, source: &SourceMap)
    -> Result<Dataset, ParseError>
{
    let path   = take_required_path  (&body, "path",   source)?;
    let format = take_required_enum::<DatasetFormat>(&body, "format", source)?;
    Ok(Dataset { label, path, format })
}
```

- `label: String` is passed in by the caller (the dispatcher
  reads it from the HCL block's first label).
- `body: hcl::Body` is the block's `{ … }` contents.
- `source: &SourceMap` is needed by every `take_*` helper for
  good error spans.

## `take_*` helpers (from `parser::utils`)

- `take_required_string`, `take_optional_string`
- `take_required_path`, `take_optional_path`
- `take_required_int`, `take_required_int_range(min, max)`
- `take_required_enum::<E>`  — closed-enum extractor
- `take_required_ref(RefKind::X)`, `take_optional_ref(RefKind::X)`
- `take_required_list_of_refs(RefKind::X)`

Always use these — do NOT poke at `hcl::Body` directly.  The
helpers produce uniform error messages + span info.

## Dispatch hook in `Project::from_hcl`

```rust
match block.identifier.as_str() {
    // ... existing arms ...
    "dataset" => {
        let label = take_single_label(&block, source)?;
        proj.datasets.push(dataset_from_body(label, block.body, source)?);
    }
    // ...
}
```

One match arm.  Single label (`take_single_label`).  Push to
the `Project.datasets` field.

## Range checks at parse time vs. checker time

Pure-syntax checks (int in range, enum value in closed set) go
into the parser via `take_required_int_range` or the
`take_required_enum::<E>` machinery.  Cross-block invariants
(ref resolves to an existing block, duplicate labels) go into
the checker.

If the DESIGN says `ttl` is `1s..=24h`, that's a parser-level
range — emit:

```rust
let ttl = take_required_duration_range(&body, "ttl",
    Duration::from_secs(1),
    Duration::from_secs(24 * 3600),
    source)?;
```
