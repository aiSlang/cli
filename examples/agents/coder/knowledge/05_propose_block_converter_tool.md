# The `propose_block_converter` tool call (v0.4 W21 Mon)

## What it does

Renders the `to_<block>` parser-converter Rust function from a
structured spec, eliminating the free-text-Rust hallucination
class W20 Thu's dogfood surfaced.  You SEND structured args
(block name + field list + types + range/enum constraints); the
runtime CALLS the `aislang_runtime::codegen::render_block_converter`
function with your spec; the resulting Rust gets spliced into
the patch verbatim.

You DO NOT write the parser-converter Rust by hand anymore.
Doing so re-introduces every hallucination the tool was
designed to prevent (`hcl::Block`, `hcl::Body`, fabricated
`ParseError::*` variants, ad-hoc `parse_bool` helpers).

## When to use it

ALWAYS, for every new block whose patch includes a per-block
`to_<name>(raw: RawBlock) -> Result<<TypeName>, String>`
converter.  Which is: every Coder reply for a new resource
block.  The AST struct + checker validate fn + parser-fixtures
tests stay free-text — only the parser-converter section flows
through the tool.

## Tool argument shape (JSON wire format)

```json
{
  "tool": "propose_block_converter",
  "args": {
    "block_name": "dataset",
    "type_name":  "Dataset",
    "fields": [
      {
        "name": "path",
        "ty":   "path",
        "required": true
      },
      {
        "name": "format",
        "ty":   "enum",
        "required": true,
        "enum_type": "DatasetFormat"
      }
    ]
  }
}
```

## Closed `ty` value set

Pick from this list verbatim — every value maps to a real
`expect_*` helper in `crates/aislang-core::parser`:

| `ty` value | Helper called | Rust struct field type |
|---|---|---|
| `path` | `expect_path` | `PathBuf` |
| `string` | `expect_string` | `String` |
| `semver` | `expect_semver` | `SemVer` |
| `bool` | `expect_bool` | `bool` |
| `duration` | `expect_duration` (or `expect_duration_range` w/ min+max) | `Duration` |
| `money` | `expect_money` | `Money` |
| `size` | `expect_size` | `Size` |
| `int` | `expect_int` (or `expect_int_range` w/ min+max) | `i64` |
| `enum` | `expect_enum::<E>` (E = `enum_type`) | the enum type |
| `ref` | `expect_ref` | `Ref` |

**Never invent a `ty` value not in this table.**  Adding a new
type requires adding the matching `expect_*` helper to the
parser crate first; the Coder's job is to USE the closed set,
not extend it.

## `required` semantics

- `required: true` — field MUST be present in the .ais source;
  parser accumulates as `Option<T>` + unwraps via `.ok_or_else()`
  in the struct constructor.
- `required: false` — optional; defaults via `T::default()`.  The
  AST struct field MUST carry the byte-stability discipline:
  `#[serde(default, skip_serializing_if = "<predicate>")]` (NOT
  the renderer's responsibility — you author that in the free-
  text AST section).

## Range checks on duration / int fields

When the Architect's design declares a range (e.g. `ttl: Duration
[1s..=24h]`), set `range_min` + `range_max` on the FieldSpec:

```json
{ "name": "ttl", "ty": "duration", "required": true,
  "range_min": "1s", "range_max": "24h" }
```

The renderer emits `expect_duration_range(..., parse_duration_str
("1s")?, parse_duration_str("24h")?)?`.  Same shape for `int`:
`range_min` / `range_max` as bare integer strings.

## Closed-enum fields

`ty: "enum"` REQUIRES `enum_type`:

```json
{ "name": "format", "ty": "enum", "required": true,
  "enum_type": "DatasetFormat" }
```

The renderer emits `expect_enum::<DatasetFormat>(...)`.  The
enum TYPE itself you declare in the free-text AST section
(`#[derive(...)] pub enum DatasetFormat { Json, Jsonl, Csv,
Parquet }`).  The renderer doesn't define the enum — it just
calls into it.

## What the rendered output looks like

For the dataset spec above, the renderer produces:

```rust
fn to_dataset(raw: RawBlock) -> Result<Dataset, String> {
    let label = require_single_label("dataset", raw.labels)?;
    let mut path: Option<PathBuf> = None;
    let mut format: Option<DatasetFormat> = None;

    for item in raw.body {
        let attr = expect_attr_only("dataset", item)?;
        match attr.name.as_str() {
            "path" => path = Some(expect_path(&attr.name, attr.value)?),
            "format" => format = Some(expect_enum::<DatasetFormat>(&attr.name, attr.value)?),
            other => return Err(format!(
                "unknown attribute `{other}` in dataset block; allowed: path, format"
            )),
        }
    }

    Ok(Dataset {
        label,
        path: path.ok_or_else(||
            "dataset missing required attribute `path`".to_string())?,
        format: format.ok_or_else(||
            "dataset missing required attribute `format`".to_string())?,
    })
}
```

This is the EXACT shape of every existing `to_<block>` in
`crates/aislang-core::parser` — no `hcl::`, no `ParseError::X`
variants, no ad-hoc parsers.  By construction.
