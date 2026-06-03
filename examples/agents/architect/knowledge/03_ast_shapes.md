# AST shapes — what `aislang-core::ast` already declares

Distilled summary of `crates/aislang-core/src/ast.rs`.  Every new
resource the Architect designs ends up as one of these struct
shapes — read this before suggesting fields.

## Top-level `Project`

```rust
pub struct Project {
    pub name:        String,
    pub budget:      Option<Budget>,
    pub models:        Vec<Model>,
    pub prompts:       Vec<Prompt>,
    pub mcp_servers:   Vec<McpServer>,
    pub agents:        Vec<Agent>,
    pub evals:         Vec<Eval>,
    pub secrets:       Vec<Secret>,
    pub deploys:       Vec<Deploy>,
    pub mains:         Vec<Main>,         // v0.2 Wk12
    pub pipelines:     Vec<Pipeline>,     // v0.3 Wk17 Mon
    pub loops:         Vec<Loop>,         // v0.3 Wk17 Wed
    pub branches:      Vec<Branch>,       // v0.3 Wk17 Wed
    pub skills:        Vec<Skill>,        // v0.3 Wk18 Wed
    pub vector_stores: Vec<VectorStore>,  // v0.3 Wk19 Mon
    pub embeddings:    Vec<Embedding>,    // v0.3 Wk19 Tue
}
```

When designing a new block type, you're adding ONE more `Vec<X>`
field to `Project` + ONE more enum variant to `BlockKind`.

## Reference shape

```rust
pub struct Ref {
    pub kind:   RefKind,             // Model | Prompt | McpServer | ... | Embedding | VectorStore
    pub label:  String,
    pub module: Option<String>,      // None = same-module; Some = cross-module
}

pub enum RefKind {
    Model, Prompt, McpServer, Agent, Secret,
    Pipeline,    // Wk17 Mon
    Embedding,   // Wk19 Mon
    VectorStore, // Wk19 Wed
}
```

Adding a new ref-able resource = add a variant here + update
`parse_ref_from_access` + `ref_kind_str` + `BlockKind::from_ref_kind`.

## Closed-enum-as-attribute pattern

```rust
pub enum OutputType { String, Json, ListString }            // Skill.output_type
pub enum DeployTarget { Compose, Kubernetes }               // Deploy.target
pub enum ChatClient { ClaudeDesktop, Cursor, Custom { name: String } }
pub enum SkillExec { Shell { command: String }, Mcp { server: Ref, tool: String } }
```

Closed enums use `#[serde(tag = "kind", rename_all = "snake_case")]`
for tagged variants, OR `#[serde(rename_all = "snake_case")]` for
plain variants.  Parser accepts both bare-ident (`output_type =
string`) AND quoted-string (`output_type = "string"`) forms via
`expect_output_type`-style helpers.

## Optional-field pattern

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub knowledge: Option<Vec<Ref>>,
```

For empty-Vec-as-absent semantics:

```rust
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub pipelines: Vec<Pipeline>,
```

This preserves v0.2 byte-stable JSON round-trip for projects that
don't use the new field.

## Range-checked numeric attributes

```rust
pub max_iter: u32,        // Loop — parser enforces 1..=100
pub chunk_size: Size,     // VectorStore — parser enforces 256..=8192 tokens
```

Range checks live in the parser (e.g. `expect_max_iter`,
`expect_chunk_size`) not the checker.  Diagnostic message includes
both bounds + the rationale.

## Cross-module refs

Same-module: `model = model.fast` → `Ref { module: None }`.
Cross-module: `model = use architect.thinker` → `Ref { module:
Some("architect".into()) }`.  Resolver (`aislang-resolver::
resolve_multi`) qualifies all refs to `<module>.<label>` form
post-merge.  Designs MUST work in both modes — no extra grammar.
