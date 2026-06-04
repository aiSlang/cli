# `aislang-core::parser` converter patterns

**The parser does NOT use the `hcl` crate.**  v0.1 ships a
custom Chumsky-based grammar that produces a `RawBlock` tree;
per-block converter fns walk the RawBlock body and produce
the typed AST struct.  This file pins the REAL shape — do not
invent `hcl::Body`, `hcl::Block`, or any other `hcl::*` types.
They don't exist in the codebase.

## Real per-block converter fn

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

**Memorise this shape.**  Every block converter follows it:
1. `require_single_label("<block>", raw.labels)?` to pull the
   block's single string label.
2. `let mut <field>: Option<T> = None;` for required fields;
   `let mut <field>: T = T::default();` for optional fields
   with a known default.
3. `for item in raw.body { … }` — iterate body items.
4. `expect_attr_only("<block>", item)?` to reject nested
   blocks (when only attrs are allowed).
5. `match attr.name.as_str() { "field1" => …, other => return
   Err(format!("unknown attribute `{other}` in <block>; …")) }`.
6. Wrap in `Ok(StructName { … })` at the end with
   `.ok_or_else(|| "<block> missing required attribute
   `<name>`".to_string())?` for required fields.

## Real `expect_*` helpers (in `crates/aislang-core::parser`)

Not `take_*`.  Reading the prompt block converter above:
- `expect_path(&attr.name, attr.value)` — `&str` label + raw
  value → `PathBuf`
- `expect_string(&attr.name, attr.value)` → `String`
- `expect_semver(&attr.name, attr.value)` → `SemVer`
- `expect_bool(&attr.name, attr.value)` → `bool`
- `expect_int_range(&attr.name, attr.value, min, max)` →
  `i64`
- `expect_money(&attr.name, attr.value)` → `Money`
- `expect_size(&attr.name, attr.value)` → `Size`
- `expect_duration(&attr.name, attr.value)` → `Duration`
- `expect_ref(&attr.name, attr.value, RefKind::X)` → `Ref`
- `expect_list_of_refs(&attr.name, attr.value, RefKind::X)`
  → `Vec<Ref>`
- `expect_enum::<E>(&attr.name, attr.value)` for closed enums

All take `(&attr.name, attr.value)` and return `Result<T,
String>`.  Errors are plain `String` — the top-level dispatcher
re-wraps as `ParseError` for span info.

## Dispatch into the converter — `Project::from_raw`

The dispatch site is in `convert_to_project` (or the
project-block converter, equivalent shape).  Adding a new
block = ONE match arm:

```rust
for block in body {
    match block.block_type.as_str() {
        "prompt"     => project.prompts.push(to_prompt(block)?),
        "agent"      => project.agents.push(to_agent(block)?),
        "dataset"    => project.datasets.push(to_dataset(block)?),  // <-- new
        // ...
        other => return Err(format!("unknown block type `{other}`")),
    }
}
```

`block.block_type` is the keyword (e.g. `"dataset"`).
`block.labels` is `Vec<String>` of the post-keyword
string labels.  `block.body` is `Vec<RawItem>` of attrs +
sub-blocks.

## Range checks at parse time vs. checker time

Pure-syntax checks (int in range, enum value in closed set) go
into the parser via `expect_int_range(...)` or
`expect_enum::<E>(...)`.  Cross-block invariants (ref resolves
to an existing block, duplicate labels) go into the checker.

If the DESIGN says `ttl` is `1s..=24h`, use
`expect_duration_range(&attr.name, attr.value,
Duration::from_secs(1), Duration::from_secs(24 * 3600))`.

## What you must NEVER do

- **Don't `use hcl::*` anywhere.**  The hcl crate is not a
  dependency.  `cargo build` will fail.
- **Don't write `body.attributes` / `body.blocks` / `block.
  body.attributes`.**  The real type is `Vec<RawItem>`
  unpacked via the `expect_*` helpers above.
- **Don't invent `parse_bool` / `parse_output_type` ad-hoc
  helpers.**  Use the existing `expect_*` family.
- **Don't define your own `ParseError` variants like
  `InvalidBoolValue` or `UnknownAttribute { block, attribute
  }`.**  The converter returns plain `String` errors with
  `format!(...)` messages following the existing
  `"unknown attribute `{other}` in <block>; allowed: <list>"`
  shape.

If the Reviewer flags any of the above as a violation, your
patch is wrong — re-read this file and the actual
`crates/aislang-core/src/parser.rs::to_prompt` /
`to_mcp_server` / `to_agent` functions before revising.
