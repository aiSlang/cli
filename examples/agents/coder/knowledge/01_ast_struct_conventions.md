# `aislang-core::ast` struct conventions

Every resource block follows the same shape.  Mirror it exactly.

## Standard struct

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dataset {
    pub label:  String,
    pub path:   PathBuf,
    pub format: DatasetFormat,
}
```

- Always derive `Debug + Clone + PartialEq + Eq + Serialize + Deserialize`.
  `Hash` only if the type goes into a `HashSet` or `HashMap` key
  (rare for top-level blocks).
- `label: String` first — every block has a label.
- Other fields in DESIGN-order (matches the user's mental model
  and keeps parser-fixture JSON readable).

## Optional + list fields — byte-stability discipline

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub description: Option<String>,

#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub tags: Vec<String>,
```

This is non-negotiable.  Decision 44.4 (`destroy -v` byte-stable
JSON contract) means any new optional field MUST round-trip
identically to v0.2 for projects that don't use it.  Add the
two attrs every time.

## Closed enums

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatasetFormat {
    Json,
    Jsonl,
    Csv,
    Parquet,
}
```

- `Copy` only for enums with no payload.
- `Hash` so the enum can be a `HashMap` key (useful in
  pattern-matching tests).
- `#[serde(rename_all = "lowercase")]` keeps the JSON wire
  format matching the .ais surface keyword (`format = jsonl`
  round-trips to `"format": "jsonl"`).

## Wiring into `Project`

```rust
pub struct Project {
    // existing fields ...
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub datasets: Vec<Dataset>,
}
```

Add the field with the same `serde(default,
skip_serializing_if = "Vec::is_empty")` discipline — this is
how the parser_fixtures JSON snapshots from v0.1/v0.2 still
round-trip after a new block lands.

## Ref kind (cross-block reference)

If a downstream block needs to point at the new one (e.g.
`eval.dataset = dataset.training`), add a variant to `RefKind`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Model,
    Prompt,
    Agent,
    Pipeline,
    VectorStore,
    Embedding,
    Dataset,        // <-- new
    // ...
}
```

Variant order matters for documentation only; serde uses the
discriminant name.

If no other block references the new one, do NOT add a
`RefKind` variant — keep the surface minimal.
