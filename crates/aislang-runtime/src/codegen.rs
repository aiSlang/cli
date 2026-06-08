//! v0.4 Week 21 Mon — code-templating skeleton for the Coder agent.
//!
//! ## Problem this solves
//!
//! W20 Thu's dogfood found the Coder hallucinating `hcl::Block` /
//! `hcl::Body` / non-existent `ParseError::*` enum variants when
//! emitting the parser-converter (`to_<block>`) section of a patch.
//! Even with the W20 Wed corpus rewrite explicitly saying "Don't
//! `use hcl::*` anywhere" + showing the real Chumsky `RawBlock` shape,
//! the model still slipped into the HCL prior under free-text Rust
//! emission.
//!
//! ## Decision 46.7 — tool-call code-templating
//!
//! v0.4 Phase 1 closes the gap by restructuring the Coder's
//! parser-converter emission to flow through a STRUCTURED tool call
//! (`propose_block_converter`) with typed args, then templating the
//! real Rust from those args.  Eliminates the "model fabricates
//! plausible-looking but wrong APIs" failure mode entirely — the
//! template only knows how to emit calls to helpers that actually
//! exist (`expect_path` / `expect_semver` / `expect_bool` / etc).
//!
//! ## Module shape
//!
//! - [`BlockConverterSpec`] — typed struct mirroring the JSON schema
//!   the Coder's tool call sends (block_name + fields + optional
//!   range/enum constraints).
//! - [`FieldSpec`] — per-field shape: name, type, required-or-default.
//! - [`render_block_converter`] — pure-string template renderer.
//!   Output mirrors the canonical `to_prompt` shape from
//!   `crates/aislang-core/src/parser.rs` verbatim:
//!     1. `fn to_<name>(raw: RawBlock) -> Result<<TypeName>, String>`
//!     2. `let label = require_single_label("<name>", raw.labels)?;`
//!     3. Per-field `let mut <field>: Option<T> = None;` (required)
//!        or `let mut <field>: T = T::default();` (optional)
//!     4. `for item in raw.body { let attr = expect_attr_only(…)?; …}`
//!     5. Match arms calling the right `expect_*` helper per field
//!     6. `other => return Err(format!("unknown attribute …"))`
//!     7. Struct construction with `.ok_or_else()` for required fields
//!
//! Output is a `String` of Rust code.  Caller is responsible for
//! splicing it into the right file context (the imports + the
//! `to_<name>` dispatcher arm are NOT emitted here — they're
//! single-line changes the Coder authors separately).

use serde::{Deserialize, Serialize};

/// Shape of the `propose_block_converter` tool call's input
/// args.  The Coder's system prompt instructs the model to emit
/// this JSON structure instead of free-text Rust for the
/// `## Patch` section's parser-converter subsection.
///
/// `block_name` is the .ais keyword (`prompt`, `dataset`, …).
/// `type_name` is the Rust struct name (`Prompt`, `Dataset`, …).
/// Conventionally CamelCase of the snake_case block_name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockConverterSpec {
    pub block_name: String,
    pub type_name:  String,
    pub fields:     Vec<FieldSpec>,
}

/// One field on a block.  Required fields are wrapped in
/// `Option<T>` during accumulation + unwrapped via `.ok_or_else()`
/// in the struct constructor; optional fields with a known default
/// use the type's `T::default()`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSpec {
    pub name:     String,
    pub ty:       FieldType,
    pub required: bool,
    /// Optional inclusive lower-bound for numeric/duration fields.
    /// String shape mirrors the .ais surface (`"1s"`, `"24h"`,
    /// `"256"`).  Renderer parses the bound + emits a range-check
    /// helper call (`expect_duration_range` / `expect_int_range`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_min: Option<String>,
    /// Optional inclusive upper-bound.  Same shape as `range_min`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_max: Option<String>,
    /// For `FieldType::Enum`: the closed-enum variant names in the
    /// declared order.  Renderer emits a typed `expect_enum::<E>`
    /// call where `E` is `<TypeName>Format`-style CamelCase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_type: Option<String>,
}

/// Closed set of field types the template can emit calls for.
/// Mirrors the `expect_*` helper family in
/// `crates/aislang-core::parser`.  Adding a new variant requires
/// adding a corresponding renderer arm + a matching helper in the
/// parser crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Path,
    String,
    SemVer,
    Bool,
    Duration,
    Money,
    Size,
    Int,
    /// Closed enum — `enum_type` MUST be set on the FieldSpec.
    Enum,
    Ref,
}

impl FieldType {
    /// Rust type the field deserializes into.  Used for the
    /// accumulator declaration + the struct field type.
    fn rust_type(self, enum_type: Option<&str>) -> String {
        match self {
            FieldType::Path     => "PathBuf".into(),
            FieldType::String   => "String".into(),
            FieldType::SemVer   => "SemVer".into(),
            FieldType::Bool     => "bool".into(),
            FieldType::Duration => "Duration".into(),
            FieldType::Money    => "Money".into(),
            FieldType::Size     => "Size".into(),
            FieldType::Int      => "i64".into(),
            FieldType::Enum     => enum_type.unwrap_or("UnknownEnum").into(),
            FieldType::Ref      => "Ref".into(),
        }
    }
}

/// Render a per-block converter fn from a structured spec.
///
/// Output mirrors the canonical `to_prompt` shape verbatim
/// (per the W20 Wed Coder corpus rewrite + the real
/// `crates/aislang-core/src/parser.rs`).  No HCL types, no
/// invented enum variants, no ad-hoc parsers — every helper call
/// resolves against the existing parser crate's public API.
///
/// The output is intentionally indented to 4 spaces (matches the
/// Rust default + the in-repo style).  Trailing newline included.
pub fn render_block_converter(spec: &BlockConverterSpec) -> String {
    let mut out = String::new();
    let bn = &spec.block_name;
    let tn = &spec.type_name;

    // Signature.
    out.push_str(&format!(
        "fn to_{bn}(raw: RawBlock) -> Result<{tn}, String> {{\n"));
    // Label.
    out.push_str(&format!(
        "    let label = require_single_label(\"{bn}\", raw.labels)?;\n"));

    // Per-field accumulator declarations.
    for f in &spec.fields {
        let rt = f.ty.rust_type(f.enum_type.as_deref());
        if f.required {
            out.push_str(&format!(
                "    let mut {fname}: Option<{rt}> = None;\n",
                fname = f.name));
        } else {
            out.push_str(&format!(
                "    let mut {fname}: {rt} = {rt}::default();\n",
                fname = f.name));
        }
    }

    // Body iteration + match dispatcher.
    out.push('\n');
    out.push_str(&format!(
        "    for item in raw.body {{\n        let attr = expect_attr_only(\"{bn}\", item)?;\n        match attr.name.as_str() {{\n"));

    for f in &spec.fields {
        let assign_op = if f.required { "Some(" } else { "" };
        let assign_close = if f.required { ")" } else { "" };
        let helper_call = render_helper_call(f);
        out.push_str(&format!(
            "            \"{name}\" => {fname} = {open}{call}{close},\n",
            name = f.name, fname = f.name,
            open = assign_op, call = helper_call, close = assign_close));
    }

    // Allowed-list for unknown-attribute error.
    let allowed: Vec<String> = spec.fields.iter()
        .map(|f| f.name.clone()).collect();
    out.push_str("            other => return Err(format!(\n");
    out.push_str(&format!(
        "                \"unknown attribute `{{other}}` in {bn} block; allowed: {allowed}\"\n",
        allowed = allowed.join(", ")));
    out.push_str("            )),\n");

    out.push_str("        }\n    }\n\n");

    // Struct constructor.
    out.push_str(&format!("    Ok({tn} {{\n        label,\n"));
    for f in &spec.fields {
        if f.required {
            out.push_str(&format!(
                "        {fname}: {fname}.ok_or_else(||\n",
                fname = f.name));
            out.push_str(&format!(
                "            \"{bn} missing required attribute `{fname}`\".to_string())?,\n",
                fname = f.name));
        } else {
            out.push_str(&format!("        {fname},\n", fname = f.name));
        }
    }
    out.push_str("    })\n}\n");

    out
}

/// Emit the `expect_*` helper call for a single field.  Pure
/// per-FieldType dispatch + the optional range/enum-variant
/// arguments.
fn render_helper_call(f: &FieldSpec) -> String {
    match f.ty {
        FieldType::Path     => "expect_path(&attr.name, attr.value)?".into(),
        FieldType::String   => "expect_string(&attr.name, attr.value)?".into(),
        FieldType::SemVer   => "expect_semver(&attr.name, attr.value)?".into(),
        FieldType::Bool     => "expect_bool(&attr.name, attr.value)?".into(),
        FieldType::Money    => "expect_money(&attr.name, attr.value)?".into(),
        FieldType::Size     => "expect_size(&attr.name, attr.value)?".into(),
        FieldType::Ref      => "expect_ref(&attr.name, attr.value, RefKind::Unknown)?".into(),
        FieldType::Duration => {
            // Range-check variant when min+max present, otherwise the
            // bare expect_duration call.
            match (f.range_min.as_ref(), f.range_max.as_ref()) {
                (Some(lo), Some(hi)) => format!(
                    "expect_duration_range(&attr.name, attr.value, \
                     parse_duration_str(\"{lo}\")?, parse_duration_str(\"{hi}\")?)?"),
                _ => "expect_duration(&attr.name, attr.value)?".into(),
            }
        }
        FieldType::Int => {
            match (f.range_min.as_ref(), f.range_max.as_ref()) {
                (Some(lo), Some(hi)) => format!(
                    "expect_int_range(&attr.name, attr.value, {lo}, {hi})?"),
                _ => "expect_int(&attr.name, attr.value)?".into(),
            }
        }
        FieldType::Enum => {
            // `enum_type` is required at the FieldSpec layer; the
            // renderer falls back to UnknownEnum if missing so the
            // emitted code still parses (it'll fail to compile, but
            // the failure is a clear "you forgot enum_type" error
            // rather than a silent shape divergence).
            let et = f.enum_type.as_deref().unwrap_or("UnknownEnum");
            format!("expect_enum::<{et}>(&attr.name, attr.value)?")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_prompt() -> BlockConverterSpec {
        // Mirrors the REAL to_prompt fn from
        // crates/aislang-core/src/parser.rs.  Renderer output for
        // this spec should match the existing fn byte-for-byte
        // (modulo cosmetic differences like exact line breaks).
        BlockConverterSpec {
            block_name: "prompt".into(),
            type_name:  "Prompt".into(),
            fields: vec![
                FieldSpec {
                    name: "file".into(), ty: FieldType::Path,
                    required: true,
                    range_min: None, range_max: None, enum_type: None,
                },
                FieldSpec {
                    name: "version".into(), ty: FieldType::SemVer,
                    required: true,
                    range_min: None, range_max: None, enum_type: None,
                },
                FieldSpec {
                    name: "template".into(), ty: FieldType::Bool,
                    required: false,
                    range_min: None, range_max: None, enum_type: None,
                },
            ],
        }
    }

    #[test]
    fn renders_prompt_converter_with_canonical_shape() {
        let out = render_block_converter(&spec_prompt());
        // Spot-check every load-bearing line of the canonical shape.
        assert!(out.starts_with("fn to_prompt(raw: RawBlock) -> Result<Prompt, String> {\n"),
            "signature must match canonical to_prompt shape; got:\n{out}");
        assert!(out.contains("let label = require_single_label(\"prompt\", raw.labels)?;"));
        assert!(out.contains("let mut file: Option<PathBuf> = None;"));
        assert!(out.contains("let mut version: Option<SemVer> = None;"));
        assert!(out.contains("let mut template: bool = bool::default();"));
        assert!(out.contains("let attr = expect_attr_only(\"prompt\", item)?;"));
        assert!(out.contains("\"file\" => file = Some(expect_path(&attr.name, attr.value)?),"));
        assert!(out.contains("\"version\" => version = Some(expect_semver(&attr.name, attr.value)?),"));
        assert!(out.contains("\"template\" => template = expect_bool(&attr.name, attr.value)?,"));
        assert!(out.contains("unknown attribute"));
        assert!(out.contains("allowed: file, version, template"));
        assert!(out.contains("Ok(Prompt {"));
        assert!(out.contains("file: file.ok_or_else"));
        assert!(out.contains("version: version.ok_or_else"));
    }

    #[test]
    fn renders_no_hcl_imports_anywhere() {
        // The whole POINT of this template is that the output
        // contains zero references to the `hcl` crate.  Pin this
        // contract loud + early so any future change to the
        // renderer that accidentally re-introduces hcl::* trips
        // the test immediately.
        let out = render_block_converter(&spec_prompt());
        assert!(!out.contains("hcl::"),
            "renderer output must not contain `hcl::` anywhere; got:\n{out}");
        assert!(!out.contains("hcl::Block"),
            "renderer output must not invent `hcl::Block`; got:\n{out}");
        assert!(!out.contains("hcl::Body"),
            "renderer output must not invent `hcl::Body`; got:\n{out}");
        assert!(!out.contains("hcl::Attribute"),
            "renderer output must not invent `hcl::Attribute`; got:\n{out}");
    }

    #[test]
    fn renders_no_invented_parse_error_variants() {
        // The second W20 Thu hallucination class was inventing
        // `ParseError::InvalidEnum { .. }` / `ParseError::Missing
        // Field { .. }` enum variants when ParseError is actually
        // a struct.  The renderer emits plain `format!(…)` Err
        // strings — pin that contract.
        let out = render_block_converter(&spec_prompt());
        assert!(!out.contains("ParseError::"),
            "renderer output must not invent ParseError:: enum \
             variants; got:\n{out}");
        assert!(!out.contains("InvalidEnum"),
            "renderer output must not invent InvalidEnum variant");
        assert!(!out.contains("InvalidBoolValue"),
            "renderer output must not invent InvalidBoolValue variant");
        assert!(!out.contains("MissingField"),
            "renderer output must not invent MissingField variant");
    }

    #[test]
    fn renders_dataset_block_with_closed_enum_field() {
        // Pin the synthetic-dataset spec the W20 Thu dogfood
        // exercised.  Field shape: path: Path, format: DatasetFormat
        // (closed enum).  The renderer must emit expect_enum::<
        // DatasetFormat>(...) for the format field.
        let spec = BlockConverterSpec {
            block_name: "dataset".into(),
            type_name:  "Dataset".into(),
            fields: vec![
                FieldSpec {
                    name: "path".into(), ty: FieldType::Path,
                    required: true,
                    range_min: None, range_max: None, enum_type: None,
                },
                FieldSpec {
                    name: "format".into(), ty: FieldType::Enum,
                    required: true,
                    range_min: None, range_max: None,
                    enum_type: Some("DatasetFormat".into()),
                },
            ],
        };
        let out = render_block_converter(&spec);
        assert!(out.contains("\"format\" => format = Some(expect_enum::<DatasetFormat>(&attr.name, attr.value)?),"),
            "closed-enum field must use expect_enum::<E> with the \
             concrete enum type from the spec; got:\n{out}");
        assert!(out.contains("let mut format: Option<DatasetFormat> = None;"),
            "accumulator type must be Option<DatasetFormat>");
    }

    #[test]
    fn renders_duration_field_with_range_check() {
        // Pin that range_min+range_max on a Duration field triggers
        // the expect_duration_range helper instead of the bare
        // expect_duration.  Mirrors the W20 Wed corpus's TTL
        // example (1s..=24h cache.ttl range).
        let spec = BlockConverterSpec {
            block_name: "cache".into(),
            type_name:  "Cache".into(),
            fields: vec![
                FieldSpec {
                    name: "ttl".into(), ty: FieldType::Duration,
                    required: true,
                    range_min: Some("1s".into()),
                    range_max: Some("24h".into()),
                    enum_type: None,
                },
            ],
        };
        let out = render_block_converter(&spec);
        assert!(out.contains(
            "expect_duration_range(&attr.name, attr.value, \
             parse_duration_str(\"1s\")?, parse_duration_str(\"24h\")?)?"),
            "Duration with range_min+range_max must call \
             expect_duration_range; got:\n{out}");
        // And the bare expect_duration MUST NOT appear.
        assert!(!out.contains("expect_duration(&attr.name, attr.value)?"),
            "renderer must use the range-checked variant when \
             range_min+range_max are present");
    }

    #[test]
    fn renders_int_field_with_range_check() {
        let spec = BlockConverterSpec {
            block_name: "loop_block".into(),
            type_name:  "LoopBlock".into(),
            fields: vec![
                FieldSpec {
                    name: "max_iter".into(), ty: FieldType::Int,
                    required: true,
                    range_min: Some("1".into()),
                    range_max: Some("100".into()),
                    enum_type: None,
                },
            ],
        };
        let out = render_block_converter(&spec);
        assert!(out.contains(
            "expect_int_range(&attr.name, attr.value, 1, 100)?"),
            "Int with range_min+range_max must call \
             expect_int_range; got:\n{out}");
    }

    #[test]
    fn renders_optional_field_with_default_initializer() {
        // Pin that required: false fields use T::default()
        // initializer + DO NOT get wrapped in Option<T>.
        let out = render_block_converter(&spec_prompt());
        // template is the only optional field in the prompt spec.
        assert!(out.contains("let mut template: bool = bool::default();"));
        assert!(!out.contains("let mut template: Option<bool>"),
            "optional field must NOT use Option<T> accumulator");
        // And the constructor uses the bare field name (no
        // .ok_or_else()).
        assert!(out.contains("        template,\n"),
            "optional field is constructed by name without \
             .ok_or_else(); got:\n{out}");
    }

    #[test]
    fn json_round_trip_through_serde() {
        // The tool-call wire format IS the serde-JSON encoding of
        // BlockConverterSpec.  Round-trip through serde to pin
        // that the schema the model sees matches what the renderer
        // consumes (Decision 46.6: JSON contract end-to-end).
        let spec = spec_prompt();
        let json = serde_json::to_string(&spec).expect("encode");
        let back: BlockConverterSpec = serde_json::from_str(&json)
            .expect("decode");
        assert_eq!(back, spec, "round-trip must be lossless");
        // And the optional skip_serializing_if discipline must
        // keep absent fields absent from the wire.
        assert!(!json.contains("range_min"),
            "absent range_min must be skipped from the JSON wire \
             (byte-stability discipline); got:\n{json}");
        assert!(!json.contains("enum_type"),
            "absent enum_type must be skipped from the JSON wire");
    }
}
