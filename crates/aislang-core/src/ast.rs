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
use semver::VersionReq;
use serde::{Deserialize, Serialize};
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub label:   String,
    pub file:    PathBuf,
    pub version: VersionReq,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose: Option<Vec<(String, u16)>>,
}
