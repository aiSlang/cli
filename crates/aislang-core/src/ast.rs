//! Abstract syntax tree for the v0 grammar.
//!
//! Mirrors LANGUAGE-SPEC-v0.md §4. Field types are the domain types from
//! [`crate::types`]; the parser (week 2) produces this tree, the type
//! checker (week 3) validates it, the resolver (week 4) consumes it.

use crate::types::*;
use semver::VersionReq;
use std::path::PathBuf;

/// A reference to another resource by label. Resolved during validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ref {
    pub kind:  RefKind,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Model,
    Prompt,
    McpServer,
    Agent,
    Secret,
}

/// `anthropic.claude_haiku_4_5` etc. — resolved against the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelId {
    pub provider: String,
    pub name:     String,
}

// -------------------------------------------------------------------------- //
// Project — root resource, exactly one per file
// -------------------------------------------------------------------------- //

#[derive(Debug, Clone, Default)]
pub struct Project {
    pub name:        String,
    pub budget:      Option<Budget>,
    pub models:      Vec<Model>,
    pub prompts:     Vec<Prompt>,
    pub mcp_servers: Vec<McpServer>,
    pub agents:      Vec<Agent>,
    pub evals:       Vec<Eval>,
    pub secrets:     Vec<Secret>,
    pub deploys:     Vec<Deploy>,
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub hard_cap_per_day: Money,
    pub hard_cap_per_run: Money,
    pub optimize_for:     OptimizeFor,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub label:       String,
    pub requires:    Vec<Capability>,
    pub context_min: Size,
    pub cost_max:    Option<Rate<Money, Size>>,
    pub latency_p95: Option<Duration>,
    pub residency:   Option<Vec<Residency>>,
    pub prefer:      Vec<ModelId>,
    pub fallback:    Vec<ModelId>,
    pub pin:         Option<ModelId>,
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub label:   String,
    pub file:    PathBuf,
    pub version: VersionReq,
}

#[derive(Debug, Clone)]
pub struct McpServer {
    pub label:   String,
    pub image:   OciImage,
    pub mount:   Option<PathBuf>,
    pub env:     Option<Vec<(String, String)>>,
    pub secrets: Option<Vec<Ref>>,
}

#[derive(Debug, Clone)]
pub struct Agent {
    pub label:         String,
    pub model:         Ref,
    pub system_prompt: Ref,
    pub tools:         Vec<Ref>,
}

#[derive(Debug, Clone)]
pub struct Eval {
    pub label:      String,
    pub dataset:    PathBuf,
    pub metric:     EvalMetric,
    pub threshold:  Percentage,
    pub aggregator: EvalAggregator,
    pub agent:      Option<Ref>,
}

#[derive(Debug, Clone)]
pub struct Secret {
    pub label:  String,
    pub source: String,        // URI; parsed at validate-time
    pub scope:  Vec<Ref>,
}

#[derive(Debug, Clone)]
pub struct Deploy {
    pub label:  String,
    pub target: DeployTarget,
    pub expose: Option<Vec<(String, u16)>>,
}
