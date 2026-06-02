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
