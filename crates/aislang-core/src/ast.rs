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
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A reference to another resource by label. Resolved during validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ref {
    pub kind:  RefKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Model,
    Prompt,
    McpServer,
    Agent,
    Secret,
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
    /// Port mappings as a `BTreeMap` for deterministic JSON ordering. Source
    /// syntax `expose = { port = 8080 }` mirrors the map form directly.
    /// Spec §4.9: `Map<String, Int>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose: Option<BTreeMap<String, u16>>,
}
