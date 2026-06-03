//! Abstract syntax tree for the v0 grammar.
//!
//! Mirrors LANGUAGE-SPEC-v0.md §4. Field types are the domain types from
//! [`crate::types`]; the parser (week 2) produces this tree, the type
//! checker (week 3) validates it, the resolver (week 4) consumes it.
//!
//! Every node derives `Serialize` + `Deserialize` so the tree round-trips
//! through JSON. This powers the `examples/*.expected-ast.json` fixture
//! files (week 1 Thursday): the canonical "what the parser must produce"
//! for each `.ais` example. See `crates/aislang-core/tests/parser_fixtures.rs`.

use crate::types::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A reference to another resource by label. Resolved during validation.
///
/// v0.3 Week 16 Tue: `module` distinguishes same-module refs (`None` —
/// the v0.2 baseline; parsed from `<kind>.<label>`) from cross-module
/// refs (`Some(<module>)` — v0.3 multi-module projects; parsed from
/// `use <module>.<label>` where the kind is inferred from the attribute
/// context).  `#[serde(default, skip_serializing_if = Option::is_none)]`
/// keeps v0.2 fixture JSON files byte-stable — a Ref with `module:
/// None` serializes to `{"kind": "...", "label": "..."}` exactly as
/// before; the field only appears when populated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub kind:  RefKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

impl Ref {
    /// Resolve this ref to a merged-project lookup key given the
    /// label of its containing block.
    ///
    /// In a multi-module project, every block label after merge
    /// carries a `<module>.` prefix (`architect.architect_kb`,
    /// `orchestrator.full_design`, …) while the refs INSIDE the
    /// block still carry the parser-produced shape — either
    /// cross-module (`use coder.coder` → `Ref { module: Some("coder"),
    /// label: "coder" }`) or same-module (`embedding.local_bge` →
    /// `Ref { module: None, label: "local_bge" }`).
    ///
    /// `qualified_in` produces the merged-project lookup key for both
    /// shapes: cross-module passes the explicit module through;
    /// same-module mirrors the containing block's module prefix
    /// (extracted as the substring before the first `.` of
    /// `containing_block_label`); single-file projects (no `.` in
    /// the containing label) return the bare ref label.
    ///
    /// v0.3 Week 20 Mon — hoisted from the four duplicated `qualified`
    /// closures across `mcp_server.rs` (build_session + RAG prepend)
    /// and `main.rs` (RAG build + query) once the multi-module
    /// orchestrator surfaced the same lookup-by-bare-label bug at
    /// every embedding lookup site.
    pub fn qualified_in(&self, containing_block_label: &str) -> String {
        let containing_module = containing_block_label
            .split_once('.').map(|(m, _)| m);
        match (&self.module, containing_module) {
            (Some(m), _)    => format!("{m}.{}", self.label),
            (None, Some(c)) => format!("{c}.{}", self.label),
            (None, None)    => self.label.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Model,
    Prompt,
    McpServer,
    Agent,
    Secret,
    /// v0.3 Week 17 Wed (Decision 44.5).  `loop.body` references a
    /// pipeline; `branch.cases[].<case>` + `branch.default` also.
    /// Composition primitive #2 + #3 — Mon's `pipeline` block is the
    /// only kind that produces Pipeline refs today; v0.5+ may extend.
    Pipeline,
    /// v0.3 Week 19 Mon (Decision 44.6).  `vector_store.embedding`
    /// references an `embedding` block.  Mon adds the ref kind +
    /// the BlockKind so duplicate-label detection works; Tue lands
    /// the `embedding "<label>" { … }` block itself + the checker
    /// dispatch_ref edge for vector_store.embedding (until then
    /// the checker WILL flag any vector_store.embedding ref as
    /// "undeclared embedding" — expected, since no embedding blocks
    /// can parse this week).
    Embedding,
    /// v0.3 Week 19 Wed (Decision 44.6).  `agent.knowledge =
    /// [vector_store.<l>]` references a `vector_store` block —
    /// chat-time RAG context.  Resolves through `dispatch_ref` to
    /// `BlockKind::VectorStore` nodes that Mon's parser registers.
    VectorStore,
}

/// `anthropic.claude_haiku_4_5` etc. — resolved against the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelId {
    pub provider: String,
    pub name:     String,
}

// -------------------------------------------------------------------------- //
// Project — root resource, exactly one per file
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name:        String,
    pub budget:      Option<Budget>,
    #[serde(default)] pub models:      Vec<Model>,
    #[serde(default)] pub prompts:     Vec<Prompt>,
    #[serde(default)] pub mcp_servers: Vec<McpServer>,
    #[serde(default)] pub agents:      Vec<Agent>,
    #[serde(default)] pub evals:       Vec<Eval>,
    #[serde(default)] pub secrets:     Vec<Secret>,
    #[serde(default)] pub deploys:     Vec<Deploy>,
    // v0.2 Week-12 Fri: `main` entry blocks (planning-doc Track A
    // architectural decision #7).  Empty Vec for v0.1 files —
    // serde(default) makes existing JSON / lock files load without
    // the field present; skip_serializing_if = Vec::is_empty makes
    // them re-serialize without the field, so v0.1 expected-AST
    // JSON fixtures continue to round-trip byte-stably.  `main`
    // blocks are never deployed; they exist as declarative topology
    // so the MCP server can describe the principal-LLM's expected
    // role to the host client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mains: Vec<Main>,

    /// v0.3 Week 17 Mon — `pipeline { … }` composition blocks
    /// (Decision 44.5).  Sequential agent composition: each step's
    /// output is the next's input.  Empty for v0.1 / v0.2 files —
    /// `serde(default)` makes existing JSON load without the field;
    /// `skip_serializing_if = Vec::is_empty` re-serializes the
    /// no-pipelines case identically to v0.2, so the parser_fixtures
    /// expected-AST JSON files stay byte-stable.
    ///
    /// Tue extends the block set with `loop` + `branch`; Wed-Fri
    /// wire `aislang-runtime::patterns::*` to actually execute
    /// pipelines at chat time.  Mon: AST + parser + checker only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipelines: Vec<Pipeline>,

    /// v0.3 Week 17 Wed — `loop { … }` composition blocks
    /// (Decision 44.5 #2).  Conditional re-execution of a pipeline
    /// body up to `max_iter` times until the MiniJinja `condition`
    /// (rendered against the loop's running output) is satisfied.
    /// Same byte-stability discipline as `mains` + `pipelines`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<Loop>,

    /// v0.3 Week 17 Wed — `branch { … }` composition blocks
    /// (Decision 44.5 #3).  Dispatch to one of N pipelines based on
    /// a MiniJinja `on` expression's rendered string value, with a
    /// required `default` fallback.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<Branch>,

    /// v0.3 Week 18 Wed — `skill "<label>" { … }` first-class
    /// blocks.  Two execution modes — shell command (`exec = …`)
    /// or MCP-tool delegate (`mcp = …`) — both produce typed output
    /// per `output_type`.  Wed lands AST + parser + checker;
    /// runtime executor + sandboxing lands Thu (`runtime::skills`).
    /// Same byte-stability discipline as `mains` / `pipelines` /
    /// `loops` / `branches`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Skill>,

    /// v0.3 Week 19 Mon — `vector_store "<label>" { … }` first-
    /// class blocks (Decision 44.6).  Backs Phase 2's RAG runtime
    /// (Wed): each vector_store declares an embedding model ref +
    /// a source URI + chunking parameters.  Mon ships AST + parser
    /// + checker; Tue adds the `embedding` block (currently a
    /// dangling ref kind — see `RefKind::Embedding` doc); Wed
    /// wires the usearch index at `aislang plan` time.  Same byte-
    /// stability discipline as `skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vector_stores: Vec<VectorStore>,

    /// v0.3 Week 19 Tue — `embedding "<label>" { … }` first-class
    /// blocks (Decision 44.6).  Declares an embedding model the
    /// RAG runtime uses to vectorise chunks at index-build time +
    /// queries at chat time.  Referenced by `vector_store.embedding`
    /// (Mon's block).  Same byte-stability discipline as
    /// `vector_stores` / `skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embeddings: Vec<Embedding>,
}

/// v0.3 Week 17 Mon — composition primitive #1.
///
/// Grammar (Decision 44.5 sketch):
///
/// ```hcl
/// pipeline "<label>" {
///   steps  = [agent.<a>, agent.<b>, …]
///   input  = "<description>"   # optional, String for v0.3
///   output = "<description>"   # optional, String for v0.3
/// }
/// ```
///
/// Semantics (runtime — Wed/Thu/Fri):
///   - sequential composition; each `step[i]`'s output feeds
///     `step[i+1]` verbatim
///   - `input` / `output` are descriptive metadata in v0.3
///     (String-only); structured types v0.5+
///   - `steps[]` references resolve to `agent` blocks via the
///     existing `Ref` machinery (cross-module-aware as of Tue
///     Week 16)
///
/// Mon ships the AST + parser + checker only — no runtime
/// dispatch.  Composition primitive #2 + #3 (`loop`, `branch`)
/// land Tue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pipeline {
    pub label: String,
    pub steps: Vec<Ref>,
    /// Optional input-description.  String-only in v0.3 per
    /// Decision 44.5; structured types (typed message envelopes,
    /// schema refs) deferred to v0.5+.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input:  Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// v0.3 Week 17 Wed — composition primitive #2.
///
/// Grammar (Decision 44.5):
///
/// ```hcl
/// loop "<label>" {
///   body      = pipeline.<p>
///   condition = "{{ output.continue }}"   # MiniJinja — verbatim
///                                          # in v0.3 Wed; Week 18 Tue
///                                          # wires actual rendering
///   max_iter  = 8                          # 1..=100, hard cap
/// }
/// ```
///
/// Semantics (runtime — Week 17 Thu-Fri / Week 18 Mon-Tue):
///   - run `body`, then render `condition` against the running
///     output context;
///   - if `condition` renders to a truthy value (Jinja convention:
///     non-empty / non-zero / non-"false"), run `body` again;
///   - hard-cap at `max_iter` iterations regardless of `condition`
///     (defense against infinite loops when the condition template
///     references a missing variable).
///
/// `max_iter` range is 1..=100 (enforced by the parser).  v0.3 hard-
/// caps even when the user wrote a larger number; v0.5+ may relax
/// for batch agents but the default-100 ceiling stays the safe
/// shape for v0.3-era debuggability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loop {
    pub label:     String,
    pub body:      Ref,        // RefKind::Pipeline
    pub condition: String,     // MiniJinja template, verbatim in v0.3 Wed
    pub max_iter:  u32,        // 1..=100, enforced at parse time
}

/// v0.3 Week 17 Wed — composition primitive #3.
///
/// Grammar (Decision 44.5):
///
/// ```hcl
/// branch "<label>" {
///   on      = "{{ classifier_result }}"   # MiniJinja, verbatim
///   cases   = {
///     "approve" = pipeline.<p_yes>,
///     "reject"  = pipeline.<p_no>,
///   }
///   default = pipeline.<p_default>
/// }
/// ```
///
/// Semantics (runtime):
///   - render `on` against the context → string value;
///   - look up that string in `cases` → execute the matched
///     pipeline if present, else execute `default`.
///
/// `cases` is a `BTreeMap` for deterministic iteration order (helps
/// lockfile reproducibility + makes the emit pass byte-stable).
/// `default` is REQUIRED — there's no fallthrough-to-nothing
/// shape in v0.3.  The parser-time exhaustiveness check the PLAN
/// mentions ("branch cases exhaustive-checked against the on-
/// expression's value set") is relaxed in v0.3 to "at least one
/// case + a default" because the `on` expression is dynamic (a
/// MiniJinja template); compile-time set-membership analysis is
/// v0.5+ work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Branch {
    pub label:   String,
    pub on:      String,                     // MiniJinja template
    pub cases:   std::collections::BTreeMap<String, Ref>,
    pub default: Ref,                        // RefKind::Pipeline
}

/// v0.3 Week 18 Wed — first-class `skill` block (Decision 44.x).
///
/// Grammar:
///
/// ```hcl
/// skill "<label>" {
///   exec        = "<shell command>"               # one of exec | mcp
///   mcp         = mcp_server.<server>.tool.<name> # one of exec | mcp
///   output_type = string | json | list<string>    # required
/// }
/// ```
///
/// Exactly ONE of `exec` / `mcp` is required (XOR).  The parser
/// rejects both-set and neither-set configurations; the checker
/// validates the `mcp` ref's server label resolves + the tool name
/// is non-empty (full MCP-tool-existence is a Thu/runtime concern —
/// the catalog of remote tools isn't known at parse time).
///
/// `output_type` is a closed enum (`String` / `Json` / `ListString`)
/// — open-ended types are deferred to v0.5+.  Runtime parses the
/// skill's stdout / MCP-tool result through this shape before
/// returning to the caller.
///
/// Wed lands AST + parser + checker only; runtime execution +
/// sandboxing land Thu under `aislang-runtime::skills`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub label:       String,
    pub exec:        SkillExec,
    pub output_type: OutputType,
}

/// One of the two execution modes a skill block can declare.  See
/// the [`Skill`] docstring for the XOR semantics + parser-time
/// rejection of both-set / neither-set configurations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillExec {
    /// Shell command — `"cargo test"` / `"echo hello | jq ."`.  The
    /// runtime (Thu) shells out via `std::process::Command` with a
    /// PATH-denylist sandbox per Decision 44.x.  Template-context
    /// substitution into the command string is a Thu deliverable.
    Shell { command: String },
    /// MCP-tool delegate — `mcp_server.<server>.tool.<name>`.
    /// Re-uses the StdioChild fleet built per the agent's `tools`
    /// list, so the same `mcp_server` declaration can be both an
    /// agent tool AND a skill backend without spawning two
    /// containers.
    Mcp {
        /// The `mcp_server` block this skill delegates to.  Stored
        /// as a `Ref` so cross-module qualification (Week 16 Tue's
        /// `module: Option<String>`) flows through unchanged.
        server: Ref,
        /// The tool name advertised by the MCP server.  Non-empty
        /// at parse time; existence-validated at runtime against
        /// the server's actual tool list.
        tool:   String,
    },
}

/// Shape the skill's output is parsed into before handing back to
/// the caller.  Closed enum for v0.3 — open-ended types (custom
/// schema refs, typed message envelopes) are v0.5+ per the
/// composition-primitive discipline (Decision 44.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputType {
    /// Raw UTF-8 text passed through as a `String`.
    String,
    /// Parsed as JSON; runtime returns `serde_json::Value`.  Invalid
    /// JSON surfaces as a runtime error (Thu).
    Json,
    /// Split into a `Vec<String>` on newline boundaries; empty
    /// trailing line dropped.  Useful for tools whose stdout is
    /// one-item-per-line (`ls`, `git ls-files`, etc.).
    ListString,
}

/// v0.3 Week 19 Mon — first-class `vector_store` block (Decision 44.6).
///
/// Grammar:
///
/// ```hcl
/// vector_store "<label>" {
///   embedding     = embedding.<label>       # ref into Tue's embedding block
///   source        = "<path | URL | crawl:URL>"
///   chunk_size    = 1k                       # 256..=8192 tokens, enforced at parse
///   chunk_overlap = 128                      # 0..=chunk_size; defaults to 0
/// }
/// ```
///
/// **Semantics** (Wed runtime — `aislang-runtime::rag`):
///   - `aislang plan`/`aislang apply` builds an embedded usearch
///     index at `<source_dir>/.aislang/vector/<label>.usearch`
///     by reading from `source`, chunking with the supplied
///     `chunk_size` (token count) + `chunk_overlap`, embedding
///     each chunk via the referenced `embedding` block's model.
///   - Agent system prompts that declare
///     `agent.knowledge = [vector_store.<label>]` auto-prepend
///     `Top-K retrieved chunks:\n<chunks>` to every chat turn.
///
/// **Mon (this block) ships AST + parser + checker.** Tue lands
/// the `embedding` block; Wed wires the index-build + query
/// runtime.  Cross-module `embedding = use other.docs` refs
/// follow Week 16 Tue's `module: Option<String>` convention —
/// no extra work needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorStore {
    pub label:         String,
    /// Ref into an `embedding` block (Tue's deliverable; Mon
    /// validates label uniqueness against [`RefKind::Embedding`]).
    pub embedding:     Ref,
    /// Where the chunkable content lives — see [`SourceUri`].
    pub source:        SourceUri,
    /// Tokens per chunk; parser enforces 256..=8192 range to
    /// keep retrieval relevance reasonable (too small loses
    /// context; too large dilutes match quality).
    pub chunk_size:    Size,
    /// Tokens of overlap between adjacent chunks; defaults to 0
    /// when omitted (parser fills the default).  Must be strictly
    /// less than `chunk_size` (enforced by the checker).
    #[serde(default, skip_serializing_if = "Size::is_zero")]
    pub chunk_overlap: Size,
}

/// `source = …` value on a `vector_store` block.  Three tagged
/// variants disambiguate at parse time:
///
///   - **Path** — bare filesystem path (relative to the .ais file
///     or absolute).  Indexer reads files under the path (single
///     file OR directory walk).
///   - **Url** — `http://…` or `https://…`.  Indexer fetches the
///     single URL + extracts text.  No recursion.
///   - **Crawl** — `crawl:http://…` or `crawl:https://…`.  Indexer
///     recursively crawls from the seed URL within the same host;
///     depth + politeness limits are runtime concerns (Wed/v0.5).
///
/// Tagged enum so JSON round-trip is unambiguous; serde
/// `#[serde(tag = "kind", rename_all = "snake_case")]` mirrors the
/// `SkillExec` shape established Week 18 Wed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceUri {
    Path  { path: PathBuf },
    Url   { url:  String  },
    Crawl { url:  String  },
}

/// v0.3 Week 19 Tue — first-class `embedding` block (Decision 44.6).
///
/// Grammar (matches the Decision 44.6 sketch):
///
/// ```hcl
/// embedding "<label>" {
///   provider = openai                     # identifier (openai | cohere | ollama)
///   model    = "text-embedding-3-small"   # exact upstream model name
/// }
/// ```
///
/// **Resolution** (Tue's contract): the catalog's `embeddings`
/// section is keyed by `<provider>.<sanitised-name>` (same shape
/// as model catalog entries — see
/// `crates/aislang-catalog/catalog.json::embeddings`).  The
/// checker resolves `(provider, model)` to a catalog entry +
/// reports "unknown embedding model" if absent.
///
/// **LiteLLM wire** (Tue's emitter delta): every declared embedding
/// block gets a `/embeddings` route in the generated
/// `litellm_config.yaml` so chat-time queries reach the right
/// upstream provider via LiteLLM's OpenAI-compatible
/// `/v1/embeddings` surface.
///
/// **Runtime** (Wed): the RAG indexer uses these to vectorise
/// chunks at `aislang apply` time.  Chat-time queries embed the
/// user input through the same model to keep query/key spaces
/// aligned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Embedding {
    pub label:    String,
    /// Identifier (`openai` / `cohere` / `ollama` / future).  The
    /// checker validates against the catalog's known-provider set.
    pub provider: String,
    /// Exact upstream model name (`"text-embedding-3-small"` /
    /// `"embed-english-v3.0"` / `"bge-small-en-v1.5"`).  Catalog
    /// entries map this verbatim to a `litellm_name` string.
    pub model:    String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub hard_cap_per_day: Money,
    pub hard_cap_per_run: Money,
    pub optimize_for:     OptimizeFor,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub label:       String,
    pub requires:    Vec<Capability>,
    pub context_min: Size,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_max:    Option<Rate<Money, Size>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p95: Option<Duration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub residency:   Option<Vec<Residency>>,
    pub prefer:      Vec<ModelId>,
    pub fallback:    Vec<ModelId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin:         Option<ModelId>,
    // v0.2 Week-12 Fri: where the inference path runs.  Defaults to
    // `Api` (v0.1 behaviour — route through LiteLLM with hard budget
    // enforcement).  `Subscription` indicates the inference happens
    // in the host chat app's subscription tier; we count tokens
    // and estimate cost but can't gate calls.  serde(default) makes
    // v0.1 files load without the field present;
    // skip_serializing_if = ModelSource::is_default re-serializes
    // the default-Api case without the field so v0.1 expected-AST
    // JSON fixtures round-trip byte-stably.
    #[serde(default, skip_serializing_if = "ModelSource::is_default")]
    pub source: ModelSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub label:   String,
    pub file:    PathBuf,
    /// Strict exact-version pin (LANGUAGE-SPEC-v0.md §3.2 SemVer row). Range
    /// operators are rejected by the literal parser; revisit in v0.2.
    pub version: SemVer,
    /// v0.3 Week 18 Tue: when true, the prompt body is rendered as a
    /// MiniJinja template at chat time (against a context built by
    /// `aislang_runtime::templates::build_default_context`).  Default
    /// false to keep v0.2 fixtures byte-stable — every existing
    /// `prompt "<label>" { file = … version = … }` block parses to a
    /// `Prompt { template: false, .. }` and the loader treats the file
    /// body verbatim, no template engine in the path.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub template: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub label:   String,
    pub image:   OciImage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount:   Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env:     Option<Vec<(String, String)>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Vec<Ref>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub label:         String,
    pub model:         Ref,
    pub system_prompt: Ref,
    pub tools:         Vec<Ref>,
    /// v0.3 Week 19 Wed (Decision 44.6) — vector_store refs that
    /// back this agent's RAG context.  At chat time the runtime
    /// queries each declared store with the user input + prepends
    /// the top-K retrieved chunks to the agent's system prompt
    /// before dispatching the LLM call.  Empty / absent → no RAG
    /// (v0.2 byte-stable: every existing `agent` block parses to
    /// `knowledge: None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge:     Option<Vec<Ref>>,
}

/// v0.2 Week-12 Fri.  Declares the user-facing LLM as the explicit
/// entry point of the agentic system — same role `fn main` plays in
/// a Rust binary.  The `pattern` field names the relationship to
/// deployed agents (orchestrator, supervisor, router, reflector,
/// planner_executor, coordinator).
///
/// Multiple `main` blocks per project are allowed (e.g. a customer-
/// facing entry + an admin escalation entry).  The MCP server picks
/// which one to activate based on the `chat_client` field matching
/// the connecting client's identity.
///
/// `main` blocks are NEVER deployed — no Compose service, no k8s
/// pod, no LiteLLM routing entry.  Declarative-only; the emitter
/// ignores them.  See planning doc Track A architectural decision
/// #7 for the full design + the naming-iteration history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Main {
    pub label:         String,
    pub model:         Ref,
    pub system_prompt: Ref,
    pub chat_client:   ChatClient,
    pub pattern:       Pattern,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Eval {
    pub label:      String,
    pub dataset:    PathBuf,
    pub metric:     EvalMetric,
    pub threshold:  Percentage,
    pub aggregator: EvalAggregator,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent:      Option<Ref>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Secret {
    pub label:  String,
    pub source: String,        // URI; parsed at validate-time
    pub scope:  Vec<Ref>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deploy {
    pub label:  String,
    pub target: DeployTarget,
    /// Optional `expose { … }` block on the deploy stanza — v0.2 Tue.
    /// Holds Compose-stack-shaping flags (currently `port` —
    /// informational — and `web_ui` — bring up Open WebUI).
    ///
    /// Grammar break vs. v0.1: this used to be an attribute-shape
    /// `expose = { port = 8080 }` (a `Map<String, Int>`).  v0.2 Tue
    /// promotes it to a typed nested block so the new `web_ui` flag
    /// can join `port` without the heterogeneous-Map typing hazard.
    /// Forward-compat note: v0.1 files with the old attribute form
    /// hit a parser error with a migration hint pointing at the new
    /// block syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose: Option<Expose>,
}

/// `expose { … }` block on a `deploy` stanza — v0.2 Tue (Track A2).
///
/// Field-set is intentionally small in v0.2; future fields land here
/// (e.g. `web_ui_port = N`, `metrics = true`, …) without further
/// grammar reshape.
///
/// Forward-compat for v0.1 callers reading the serialized form: both
/// fields use `skip_serializing_if`, so an Expose with no port and
/// `web_ui = false` round-trips as `{}` — same JSON byte shape as a
/// v0.1 project that never had `expose` (which is `null` because of
/// `Option<Expose>`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Expose {
    /// Optional port the user INTENDS the deployed agent runtime to
    /// listen on.  Preserved from v0.1 as informational only — no
    /// service in v0.2's emitted stack binds it yet (the v0.2+
    /// containerized agent runtime via `aislang mcp-serve --http`
    /// will).  Surfaced in the YAML header comment block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// When true, the Compose emitter inserts an `openwebui` sidecar
    /// (`ghcr.io/open-webui/open-webui:main`) into the stack.yml,
    /// mapped `3000:8080`, pre-pointed at the in-stack LiteLLM
    /// (`OPENAI_API_BASE_URL=http://litellm:4000/v1`).  Persisted to a
    /// named volume that `aislang destroy -v` wipes per Decision X
    /// ("the OpenWebUI container is part of the aiSlang runtime;
    /// `destroy` applies").  Default false → no sidecar emitted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub web_ui: bool,
    /// Optional ingress host (Phase 2 Week 14 Thu — Decision 43.6).
    /// When set, the K8s emitter emits an `Ingress` resource pointing
    /// at the Open WebUI Service so the browser UI is reachable at
    /// `http://<ingress>` (or `https://` if TLS terminator is wired
    /// upstream of the cluster — v0.2 doesn't emit TLS configuration
    /// for the Ingress).
    ///
    /// **Two conditions for the Ingress to be emitted**:
    ///   - `web_ui = true` (the Ingress points at the openwebui
    ///     Service, so without an Open WebUI pod there's nothing
    ///     to route to)
    ///   - `ingress` is `Some("<host>")`
    ///
    /// Ignored by the Compose target — Compose's `3000:8080` host
    /// port mapping covers the same browser-access need without
    /// needing an Ingress controller in front.  Forward-compat: v0.1
    /// .ais files (no `expose` block at all) round-trip cleanly
    /// because the field is `Option<String>` skipped when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(label: &str, module: Option<&str>) -> Ref {
        Ref {
            kind:   RefKind::Embedding,
            label:  label.to_string(),
            module: module.map(str::to_string),
        }
    }

    #[test]
    fn qualified_in_passes_through_explicit_cross_module_ref() {
        // `use coder.coder` inside an orchestrator module → must
        // resolve to `coder.coder` regardless of the containing
        // block's module prefix.
        let ref_ = r("coder", Some("coder"));
        assert_eq!(ref_.qualified_in("orchestrator.full_design"),
            "coder.coder");
    }

    #[test]
    fn qualified_in_mirrors_containing_module_for_same_module_ref() {
        // `embedding.local_bge` inside `vector_store.architect_kb`
        // (which post-merge is `architect.architect_kb`) → must
        // resolve to `architect.local_bge` so the lookup against the
        // merged-project embeddings vec finds the qualified entry.
        // This is the W20 Mon orchestrator bug that broke the
        // apply-time RAG index build.
        let ref_ = r("local_bge", None);
        assert_eq!(ref_.qualified_in("architect.architect_kb"),
            "architect.local_bge");
    }

    #[test]
    fn qualified_in_returns_bare_label_for_single_file_project() {
        // No `.` in the containing label → single-file project →
        // no module to mirror.  Refs stay bare.
        let ref_ = r("local_bge", None);
        assert_eq!(ref_.qualified_in("architect_kb"), "local_bge");
    }

    #[test]
    fn qualified_in_handles_multiple_dots_in_containing_label() {
        // Defensive: a hypothetical resource label with multiple
        // dots should still split on the FIRST `.` for the module
        // prefix (consistent with `qualified_step_labels` +
        // `build_session_for_agent_multi_module`).  v0.3 doesn't
        // produce such labels but the helper should be robust.
        let ref_ = r("local_bge", None);
        assert_eq!(ref_.qualified_in("architect.architect_kb.shadow"),
            "architect.local_bge");
    }
}
