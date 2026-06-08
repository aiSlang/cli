# Reviewer checklist — what to verify on every patch

Run this list against every `## Patch` section.  Reject if any
item is violated; accept if all pass.

The list is intentionally split into **NEGATIVE shape** (this
patch MUST NOT contain X) before **POSITIVE shape** (this patch
MUST contain Y).  The negative-shape gates catch the
hallucination classes the W20 Thu dogfood surfaced (Coder
inventing `hcl::*` types + `ParseError::X` enum variants); the
positive-shape gates enforce the in-repo conventions every real
block follows.

## NEGATIVE shape — REJECT immediately if any of these appear

These are the canonical hallucination patterns.  Each one
means the patch was authored against a fictional API that
doesn't exist in `aislang-core`.  Recognising them is the
Reviewer's load-bearing job.

### `hcl::*` imports or types — `aislang-core` is NOT HCL-based

The parser uses a custom Chumsky-based grammar that produces
`RawBlock` / `RawItem` / `RawAttr`.  The `hcl` crate is NOT a
dependency.  REJECT the patch if any of these appear:

```rust
use hcl::{Body, Block, Attribute, Value};   // ← FICTION
fn parse_dataset_block(block: &hcl::Block) -> ... { ... }
fn from_hcl(body: &hcl::Body) -> Result<Self, ParseError> { ... }
let attrs = block.body.attributes;          // RawBlock has no .body.attributes
```

Rejection feedback shape:

```
FAIL: parser-converter uses `hcl::*` types that don't exist
in aislang-core.  The parser is Chumsky-based + produces
RawBlock; per-block converters take `raw: RawBlock` + call
`expect_*` helpers.  See knowledge/02_parser_converter_
patterns.md for the canonical to_prompt shape.

Issues:
- `use hcl::Body` / `hcl::Block` / `hcl::Attribute` — DELETE.
  The hcl crate is not a dependency; `cargo build` fails.
- `fn parse_<block>(block: &hcl::Block)` — REWRITE as
  `fn to_<block>(raw: RawBlock) -> Result<<Type>, String>`.
- `body.attributes` / `block.body.attributes` — REPLACE with
  `for item in raw.body { let attr = expect_attr_only(...)?; }`.
```

### Invented `ParseError::X` enum variants

`ParseError` is a `pub struct`, NOT an enum.  Its public
shape is:

```rust
pub struct ParseError {
    pub message: String,
    pub span:    Option<SimpleSpan>,
}
```

REJECT the patch if it invents variants like:

```rust
Err(ParseError::InvalidEnum    { field, value, expected })  // ← FICTION
Err(ParseError::MissingField   { block, field })            // ← FICTION
Err(ParseError::TypeMismatch   { field, expected })         // ← FICTION
Err(ParseError::UnknownAttribute { block, attribute })      // ← FICTION
Err(ParseError::SemVerParseError { value })                 // ← FICTION
Err(ParseError::InvalidBoolValue)                           // ← FICTION
matches!(err, ParseError::InvalidEnum { .. })               // ← FICTION
```

The converter returns `Result<T, String>` — plain `String`
errors with `format!(...)` messages.  The top-level dispatcher
re-wraps as `ParseError` for span info.  The Reviewer's
rejection-test assertions match on `ValidateError::Parse(errs)`
+ substring-check `errs[0].message`.

### Ad-hoc `parse_*` helpers

REJECT patches that invent ad-hoc helpers like:

```rust
fn parse_output_type(value: &hcl::Value) -> Result<OutputType, ParseError> { ... }
fn parse_bool(value: &Value) -> Result<bool, ParseError> { ... }
```

The real codebase uses `expect_path` / `expect_string` /
`expect_semver` / `expect_bool` / `expect_int` /
`expect_int_range` / `expect_money` / `expect_size` /
`expect_duration` / `expect_duration_range` / `expect_ref` /
`expect_list_of_refs` / `expect_enum::<E>`.  Adding a new
helper requires adding it to `aislang-core::parser` FIRST;
the patch cannot ship its own.

### `take_*` helpers — also FICTION

Older corpus copies referred to `take_required_*` /
`take_optional_*`.  Those don't exist either.  The real prefix
is `expect_*`.  REJECT any patch using `take_required_path`,
`take_optional_string`, etc.  See knowledge/02_parser_
converter_patterns.md for the closed set.

### `Project::from_hcl` dispatcher hook

The real dispatcher fn is in `convert_to_project` and matches
on `block.block_type.as_str()`.  REJECT patches that name the
hook `Project::from_hcl` or use `block.identifier.as_str()`
— those are HCL-crate API shapes that don't exist.

## POSITIVE shape — REQUIRE every applicable item

### Byte-stability (Decision 44.4)

- [ ] Every new optional field on `Project` (or a sub-struct
      embedded in `Project`) carries `#[serde(default,
      skip_serializing_if = "Option::is_none")]` (for
      `Option<T>`) or `#[serde(default, skip_serializing_if =
      "Vec::is_empty")]` (for `Vec<T>`).
- [ ] A `roundtrip_<block>_block_is_byte_stable` test or
      equivalent snapshot exists.
- [ ] If the patch adds a field to `Project`, a
      `v0_2_project_without_new_block_roundtrip_unchanged`
      test (or equivalent snapshot) still passes.

### Parser hygiene (REAL shape — see knowledge/02 for the canonical fn)

- [ ] Per-block converter fn signature is exactly
      `fn to_<block>(raw: RawBlock) -> Result<<Type>, String>`.
- [ ] Body iteration uses
      `for item in raw.body { let attr = expect_attr_only(
      "<block>", item)?; match attr.name.as_str() { ... } }`.
- [ ] Match arms call the right `expect_*` helper per field.
- [ ] Unknown-attribute arm uses the exact `format!(...)`
      shape: `"unknown attribute `{other}` in <block> block;
      allowed: <field1>, <field2>, ..."`.
- [ ] Required fields wrap in `.ok_or_else(|| "<block> missing
      required attribute `<name>`".to_string())?` in the
      struct constructor.

### Checker hygiene

- [ ] `validate_<block>s` fn added.
- [ ] Standard duplicate-label rejection present.
- [ ] Per-instance cross-field checks present for every
      cross-field rule the DESIGN's "Decision implications"
      section called out.
- [ ] `Project::validate` calls the new `validate_<block>s`
      fn in the right order.

### Tests (REAL shape — `ValidateError`, NOT `ParseError::X`)

- [ ] One `<verb>_<subject>_<condition>`-named happy-path test
      asserting on `aislang_core::validate(src)` returning
      `Ok((Project, Vec<Warning>))`.
- [ ] One rejection test per range/enum check from the DESIGN
      asserting on `aislang_core::validate(src)` returning
      `Err(ValidateError::Parse(errs))` + substring-checking
      `errs[0].message` for the failing field name.
- [ ] One duplicate-label test asserting on
      `ValidateError::Type(msg)` + substring-checking `msg`
      for `"duplicate"` + the block keyword.
- [ ] One byte-stable round-trip test or fixture pair under
      `tests/data/`.

### RefKind hygiene

- [ ] If the DESIGN's "Decision implications" said `New
      RefKind needed = yes`, the patch adds the variant in
      snake_case discriminant order.
- [ ] If yes, a dangling-ref test exists for the new variant.
- [ ] If `RefKind = no` per the DESIGN, the patch does NOT
      add a variant.

### Persistent-state contract (Decision 44.4)

- [ ] If the block writes to disk, the writes go under
      `<source_dir>/.aislang/<subtree>/`.
- [ ] A `// Decision 44.4: writes to .aislang/<subtree>/`
      comment marks the validate fn (or runtime entry
      point).

### Type-system constraints (Decision 44.5)

- [ ] No new parameterised generics (`Map<K,V>`,
      `List<List<T>>`).
- [ ] New closed enums OK; documented in the LANGUAGE-SPEC
      snippet.
- [ ] Field types come from the closed set in
      `aislang-core::types`.

## Tool-call code-templating (v0.4 W21 Mon Decision 46.7)

If the Coder's reply emitted a `propose_block_converter` tool
call instead of free-text Rust for the parser-converter
subsection, the converter IS guaranteed-shape-correct by
construction (it came from `aislang_runtime::codegen::
render_block_converter`).  The negative-shape gates above are
still worth running on the AST struct + checker + tests
sections (those stay free-text), but the converter section
gets a fast-track ACCEPT for its hygiene checks.

If the Coder emitted free-text Rust for the converter
section (model slipped out of tool-call mode), run the full
negative-shape gate above.
