# Parser-fixture test patterns

Tests live in `crates/aislang-core/tests/parser_fixtures.rs`
(parser happy-path + rejection coverage) and in
`crates/aislang-core/tests/error_fixtures.rs` (chumsky-error
span-shape regression).  Every block ships:
- 1 happy-path test asserting on the parsed Project shape
- 1 rejection test per range/enum check from the DESIGN
- 1 duplicate-label rejection test
- 1 byte-stable round-trip fixture pair under `tests/data/`

## Happy-path fixture (the REAL shape — `aislang_core::validate`)

```rust
#[test]
fn dataset_block_parses() {
    let src = r#"
project "p" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.50
  }
  dataset "training" {
    path   = "./data/train.jsonl"
    format = jsonl
  }
}
"#;
    let (proj, _warnings) = aislang_core::validate(src)
        .expect("valid dataset block");
    assert_eq!(proj.datasets.len(), 1);
    assert_eq!(proj.datasets[0].label, "training");
    assert_eq!(proj.datasets[0].format, DatasetFormat::Jsonl);
}
```

- The public parse entry-point is `aislang_core::validate` —
  NOT `parse_str`, NOT `parse_project`.  Returns `Result<
  (Project, Vec<Warning>), ValidateError>`.
- Wrap in a minimal `project "p" { budget { … } … }` so the
  test exercises the full parse + validate path.
- Use the W20 Wed budget caps (`$1` day / `$0.50` run) — the
  exact value is irrelevant for the test, but match the
  prevailing style.

## Rejection-test shape — assert on `ValidateError`, not a fictional `ParseError::X`

```rust
#[test]
fn dataset_block_rejects_unknown_format() {
    let src = r#"
project "p" {
  budget { hard_cap_per_day = $1, hard_cap_per_run = $0.50 }
  dataset "training" {
    path   = "./train.jsonl"
    format = yaml      # not in {json, jsonl, csv, parquet}
  }
}
"#;
    let err = aislang_core::validate(src)
        .expect_err("yaml is not a closed-enum value");
    match err {
        aislang_core::ValidateError::Parse(errs) => {
            assert!(!errs.is_empty());
            assert!(errs[0].message.contains("format")
                 || errs[0].message.contains("yaml"),
                "rejection message must name the failing field or value; got: {}",
                errs[0].message);
        }
        other => panic!("expected ValidateError::Parse; got {other:?}"),
    }
}
```

- `aislang_core::ValidateError` has THREE variants:
  - `Parse(Vec<ParseError>)` — grammar / converter rejection
  - `Type(String)` — checker rejection
  - `UnresolvedRef(String)` — ref-resolution rejection
- `ParseError` itself is a STRUCT, not an enum:
  ```rust
  pub struct ParseError {
      pub message: String,
      pub span: Option<SimpleSpan>,
  }
  ```
- **Never write `matches!(err, ParseError::InvalidEnum { .. })`
  or `ParseError::UnknownAttribute { .. }`** — those variants
  do not exist.  Match on `ValidateError::Parse(errs)` then
  assert against `errs[0].message` substrings.

## Duplicate-label check fixture (checker — `ValidateError::Type`)

```rust
#[test]
fn checker_rejects_duplicate_dataset_labels() {
    let src = r#"
project "p" {
  budget { hard_cap_per_day = $1, hard_cap_per_run = $0.50 }
  dataset "x" { path = "./a", format = json }
  dataset "x" { path = "./b", format = json }
}
"#;
    let err = aislang_core::validate(src)
        .expect_err("duplicate dataset labels");
    match err {
        aislang_core::ValidateError::Type(msg) => {
            assert!(msg.contains("dataset") && msg.contains("duplicate"),
                "checker error must name the offending block kind + duplicate; got: {msg}");
        }
        other => panic!("expected ValidateError::Type; got {other:?}"),
    }
}
```

## Byte-stable round-trip fixture

```rust
#[test]
fn roundtrip_dataset_block_is_byte_stable() {
    let src = include_str!("data/dataset_block.ais");
    let (proj, _w) = aislang_core::validate(src).unwrap();
    let json = serde_json::to_string_pretty(&proj).unwrap();
    let expected = include_str!("data/dataset_block.json");
    assert_eq!(json.trim(), expected.trim());
}
```

Plus the static fixture pair under `tests/data/`:
- `dataset_block.ais` — minimal valid .ais using the block.
- `dataset_block.json` — expected serialized AST.

The `_block.json` snapshot is what enforces the
`#[serde(default, skip_serializing_if = ...)]` discipline —
without the attr, the snapshot needs updating every release,
breaking Decision 44.4's "v0.2 projects round-trip
identically".

## V0.2-shaped projects must round-trip unchanged

The "did you remember the `skip_serializing_if` attr" test:

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

If the JSON diff shows `"datasets": []` appearing in the new
output, the attr is missing.
