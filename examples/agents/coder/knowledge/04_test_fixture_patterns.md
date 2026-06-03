# Parser-fixture test patterns

Tests live in `crates/aislang-core/tests/parser_fixtures.rs`.
Every block ships at least 4 tests: 1 happy-path, 1 per
range/enum check, 1 duplicate-label check, 1 byte-stable
round-trip.

## Happy-path fixture

```rust
#[test]
fn parser_accepts_dataset_block() {
    let src = r#"
project "p" {
  budget { hard_cap_per_day = $1, hard_cap_per_run = $0.1 }
  dataset "training" {
    path   = "./data/train.jsonl"
    format = jsonl
  }
}
"#;
    let proj = parse_str(src).unwrap();
    assert_eq!(proj.datasets.len(), 1);
    assert_eq!(proj.datasets[0].label, "training");
    assert_eq!(proj.datasets[0].format, DatasetFormat::Jsonl);
}
```

Wrap in a minimal `project "p" { budget { … } … }` so the test
exercises the full parse path, not just the standalone block
converter.

## Range/enum rejection fixture

```rust
#[test]
fn parser_rejects_dataset_block_with_unknown_format() {
    let src = r#"
project "p" {
  budget { hard_cap_per_day = $1, hard_cap_per_run = $0.1 }
  dataset "training" {
    path   = "./train.jsonl"
    format = yaml      # not in {json, jsonl, csv, parquet}
  }
}
"#;
    let err = parse_str(src).unwrap_err();
    assert!(matches!(err, ParseError::InvalidEnum { .. }));
}
```

One test per range/enum check.  Always assert on the specific
error variant — `matches!(err, ParseError::X { .. })` — not on
the error's debug-printed string.

## Duplicate-label check fixture

```rust
#[test]
fn checker_rejects_duplicate_dataset_labels() {
    let src = r#"
project "p" {
  budget { hard_cap_per_day = $1, hard_cap_per_run = $0.1 }
  dataset "x" { path = "./a", format = json }
  dataset "x" { path = "./b", format = json }
}
"#;
    let err = parse_str(src).unwrap().validate().unwrap_err();
    assert!(matches!(err,
        CheckError::DuplicateLabel { block_kind: BlockKind::Dataset, .. }));
}
```

## Byte-stable round-trip fixture

```rust
#[test]
fn roundtrip_dataset_block_is_byte_stable() {
    let src = include_str!("data/dataset_block.ais");
    let proj = parse_str(src).unwrap();
    let json = serde_json::to_string_pretty(&proj).unwrap();
    let expected = include_str!("data/dataset_block.json");
    assert_eq!(json.trim(), expected.trim());
}
```

Plus the static fixture pair under `tests/data/`:
- `dataset_block.ais` — minimal valid .ais using the block.
- `dataset_block.json` — expected serialized AST.

The `_block.json` snapshot is what enforces the
`skip_serializing_if` discipline — without the attr, the
snapshot needs updating every release, breaking
Decision 44.4's "v0.2 projects round-trip identically".

## V0.2-shaped projects must round-trip unchanged

```rust
#[test]
fn v0_2_project_without_new_block_roundtrip_unchanged() {
    let proj = Project {
        // all fields populated as a v0.2 project would
        datasets: vec![],        // new field, default-empty
        // ...
    };
    let json = serde_json::to_string_pretty(&proj).unwrap();
    let expected = include_str!("data/v0_2_baseline.json");
    assert_eq!(json.trim(), expected.trim());
}
```

This is the "did you remember the `skip_serializing_if` attr"
test.  If the JSON diff shows `"datasets": []` appearing in
the new output, the attr is missing.
