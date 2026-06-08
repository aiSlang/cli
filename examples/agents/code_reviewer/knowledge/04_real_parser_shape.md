# Canonical parser shape — what every `to_<block>` REALLY looks like

This file is the Reviewer's ground-truth reference for the
parser-converter section of every patch.  Compare the Coder's
output against THIS shape, byte-for-byte.  Any deviation that
isn't a field-set difference is a hallucination → REJECT.

## The canonical `to_prompt` fn (verbatim from `crates/aislang-core/src/parser.rs`)

```rust
fn to_prompt(raw: RawBlock) -> Result<Prompt, String> {
    let label = require_single_label("prompt", raw.labels)?;
    let mut file:     Option<PathBuf> = None;
    let mut version:  Option<SemVer>  = None;
    let mut template: bool            = false;

    for item in raw.body {
        let attr = expect_attr_only("prompt", item)?;
        match attr.name.as_str() {
            "file"     => file     = Some(expect_path(&attr.name, attr.value)?),
            "version"  => version  = Some(expect_semver(&attr.name, attr.value)?),
            "template" => template = expect_bool(&attr.name, attr.value)?,
            other => return Err(format!(
                "unknown attribute `{other}` in prompt block; \
                 allowed: file, version, template"
            )),
        }
    }

    Ok(Prompt {
        label,
        file: file.ok_or_else(||
            "prompt missing required attribute `file`".to_string())?,
        version: version.ok_or_else(||
            "prompt missing required attribute `version`".to_string())?,
        template,
    })
}
```

**Every load-bearing line of the shape**:

1. Signature: `fn to_<block>(raw: RawBlock) -> Result<<Type>, String>`.
2. Label: `let label = require_single_label("<block>", raw.labels)?;`.
3. Required-field accumulators: `let mut <name>: Option<T> = None;`.
4. Optional-with-default accumulators: `let mut <name>: T = T::default();` (or a literal default).
5. Iteration: `for item in raw.body { let attr = expect_attr_only("<block>", item)?; match attr.name.as_str() { ... } }`.
6. Match arms calling `expect_<type>(&attr.name, attr.value)?` per field.
7. Unknown-attribute arm with `format!("unknown attribute `{{other}}` in <block> block; allowed: <list>")`.
8. Struct construction with `.ok_or_else(|| "<block> missing required attribute `<name>`".to_string())?` for required fields.

If the patch diverges from steps 1-8 (different signature
types, different helper names, different error wire shape),
REJECT.

## Closed set of `expect_*` helpers

Pick from this exact list — every helper resolves against a
real fn in `crates/aislang-core::parser`:

| Helper | Returns |
|---|---|
| `expect_path(&attr.name, attr.value)?` | `PathBuf` |
| `expect_string(&attr.name, attr.value)?` | `String` |
| `expect_semver(&attr.name, attr.value)?` | `SemVer` |
| `expect_bool(&attr.name, attr.value)?` | `bool` |
| `expect_int(&attr.name, attr.value)?` | `i64` |
| `expect_int_range(&attr.name, attr.value, min, max)?` | `i64` |
| `expect_money(&attr.name, attr.value)?` | `Money` |
| `expect_size(&attr.name, attr.value)?` | `Size` |
| `expect_duration(&attr.name, attr.value)?` | `Duration` |
| `expect_duration_range(&attr.name, attr.value, min, max)?` | `Duration` |
| `expect_ref(&attr.name, attr.value, RefKind::X)?` | `Ref` |
| `expect_list_of_refs(&attr.name, attr.value, RefKind::X)?` | `Vec<Ref>` |
| `expect_enum::<E>(&attr.name, attr.value)?` | `E` (closed enum) |

REJECT any patch using helpers that aren't in this table,
e.g.:

- `take_required_string` / `take_optional_*` — FICTION
- `parse_bool` / `parse_output_type` (ad-hoc) — FICTION
- `parse_value` / `value.as_string()` — HCL-shape FICTION

## Dispatch hook — `convert_to_project`

The real top-level dispatcher matches on `block.block_type.
as_str()`, NOT `block.identifier.as_str()` (which is HCL
crate API).  Adding a new block keyword = ONE match arm:

```rust
for block in body {
    match block.block_type.as_str() {
        "prompt"  => project.prompts.push(to_prompt(block)?),
        "agent"   => project.agents.push(to_agent(block)?),
        "dataset" => project.datasets.push(to_dataset(block)?),  // ← new
        // ...
        other => return Err(format!("unknown block type `{other}`")),
    }
}
```

REJECT patches that:
- Hook into a fictional `Project::from_hcl` fn.
- Match on `block.identifier.as_str()`.
- Use `block.body` / `block.labels` differently than `RawBlock`'s real fields (`block_type` / `labels` / `body`).

## Test-shape ground truth (`ValidateError`)

The Coder's test fixtures must assert on the REAL error type
hierarchy:

```rust
pub enum ValidateError {
    Parse(Vec<ParseError>),       // grammar / converter rejection
    Type(String),                  // checker rejection
    UnresolvedRef(String),         // ref-resolution rejection
}

pub struct ParseError {
    pub message: String,
    pub span: Option<SimpleSpan>,
}
```

### Canonical rejection-test shape

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

REJECT any rejection test that:
- Uses `matches!(err, ParseError::InvalidEnum { .. })` — `ParseError` has no variants.
- Asserts against a `parse_str` / `parse_project` entry point — the real one is `aislang_core::validate(src)`.
- Tries to match on a `ParseError::X { .. }` enum variant.

## Quick-reference: when in doubt, grep

The Reviewer should mentally simulate a grep against
`crates/aislang-core/src/parser.rs`:

- `grep "fn to_" parser.rs` — every per-block converter.
- `grep "expect_" parser.rs` — every helper available.
- `grep "RawBlock\|RawItem\|RawAttr" parser.rs` — the real
  AST shape the converters consume.
- `grep "ParseError" parser.rs core.rs` — confirms it's a
  struct, not an enum.

If the Coder's patch references a name that wouldn't survive
that grep, it's a hallucination → REJECT with the specific
non-existent name called out in the feedback.
