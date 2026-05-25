//! Semantic checks on the typed AST — runs *after* `parser::parse` and
//! before any downstream consumer (resolver, emitter).
//!
//! Week-3 daily build-up:
//!   Mon  resource graph foundation + duplicate-label detection (THIS WEEK)
//!   Tue  cross-resource reference resolution (Ref target validation)
//!   Wed  catalog ID validation against the bundled catalog (with warnings)
//!   Thu  source-aware error rendering (line:col + caret)
//!   Fri  pipeline integration + CLI graph display
//!
//! The internal `ResourceGraph` is built today; it carries only nodes at
//! the moment. Tuesday will add edges for every `Ref` in the project,
//! turning it into the structure ref-resolution walks.

use crate::ast::*;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

// ============================================================================
// Public entry point
// ============================================================================

/// Run all available semantic checks on the project. Today: duplicate-
/// label detection across the seven named-resource block kinds. The
/// signature stays `Result<(), String>` until Wednesday — when warnings
/// land, this becomes `Result<Vec<Warning>, String>`.
pub fn check(project: &Project) -> Result<(), String> {
    let _graph = build_resource_graph(project)?;
    // Tuesday: validate every Ref in the project against _graph
    // Wednesday: validate ModelIds against the bundled catalog
    Ok(())
}

// ============================================================================
// Block kind taxonomy (superset of RefKind — includes Eval / Deploy which
// can have labels and need duplicate detection, but cannot be Ref targets)
// ============================================================================

/// All seven named-resource block kinds in v0.1. Superset of [`RefKind`]
/// — adds `Eval` and `Deploy`, which can be declared (and so need
/// duplicate-label checking) but cannot be referenced from another block
/// (they're terminal nodes in the resource graph).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BlockKind {
    Model,
    Prompt,
    McpServer,
    Agent,
    Eval,
    Secret,
    Deploy,
}

impl BlockKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Model     => "model",
            Self::Prompt    => "prompt",
            Self::McpServer => "mcp_server",
            Self::Agent     => "agent",
            Self::Eval      => "eval",
            Self::Secret    => "secret",
            Self::Deploy    => "deploy",
        }
    }

    /// Used Tuesday to convert a `Ref::kind` into the graph's node key
    /// kind. Currently unused — silenced so dead_code doesn't fire.
    #[allow(dead_code)]
    pub(crate) fn from_ref_kind(k: RefKind) -> Self {
        match k {
            RefKind::Model     => Self::Model,
            RefKind::Prompt    => Self::Prompt,
            RefKind::McpServer => Self::McpServer,
            RefKind::Agent     => Self::Agent,
            RefKind::Secret    => Self::Secret,
        }
    }
}

// ============================================================================
// Resource graph — built Monday, edge-populated Tuesday
// ============================================================================

pub(crate) type NodeKey = (BlockKind, String);

#[derive(Debug, Clone)]
pub(crate) struct NodeData {
    #[allow(dead_code)]   // Used by Friday's CLI graph display
    pub kind:  BlockKind,
    #[allow(dead_code)]
    pub label: String,
}

pub(crate) struct ResourceGraph {
    #[allow(dead_code)]   // Edges + traversal land Tue; CLI display lands Fri
    pub graph: DiGraph<NodeData, ()>,
    #[allow(dead_code)]
    pub nodes: HashMap<NodeKey, NodeIndex>,
}

pub(crate) fn build_resource_graph(project: &Project) -> Result<ResourceGraph, String> {
    let mut graph = DiGraph::new();
    let mut nodes: HashMap<NodeKey, NodeIndex> = HashMap::new();

    for m in &project.models      { add_block(&mut graph, &mut nodes, BlockKind::Model,     &m.label)?; }
    for p in &project.prompts     { add_block(&mut graph, &mut nodes, BlockKind::Prompt,    &p.label)?; }
    for s in &project.mcp_servers { add_block(&mut graph, &mut nodes, BlockKind::McpServer, &s.label)?; }
    for a in &project.agents      { add_block(&mut graph, &mut nodes, BlockKind::Agent,     &a.label)?; }
    for e in &project.evals       { add_block(&mut graph, &mut nodes, BlockKind::Eval,      &e.label)?; }
    for s in &project.secrets     { add_block(&mut graph, &mut nodes, BlockKind::Secret,    &s.label)?; }
    for d in &project.deploys     { add_block(&mut graph, &mut nodes, BlockKind::Deploy,    &d.label)?; }

    Ok(ResourceGraph { graph, nodes })
}

/// Add one block as a node, returning a `duplicate label` error if a
/// node with the same (kind, label) already exists. v0.1 uses a flat
/// String error message; Thursday's source-aware rendering will attach
/// span info pointing at both occurrences.
fn add_block(
    graph: &mut DiGraph<NodeData, ()>,
    nodes: &mut HashMap<NodeKey, NodeIndex>,
    kind: BlockKind,
    label: &str,
) -> Result<(), String> {
    let key: NodeKey = (kind, label.to_string());
    if nodes.contains_key(&key) {
        return Err(format!(
            "duplicate `{}` block label `{}` declared in project \
             (source-position pointers arrive in week-3 Thursday's \
             source-aware error rendering)",
            kind.as_str(),
            label,
        ));
    }
    let idx = graph.add_node(NodeData {
        kind,
        label: label.to_string(),
    });
    nodes.insert(key, idx);
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    /// Test helper: wrap a body fragment in a minimum valid project.
    fn project_src(body: &str) -> String {
        format!(
            r#"project "p" {{
                budget {{ hard_cap_per_day = $1   hard_cap_per_run = $0.01 }}
                {body}
            }}"#
        )
    }

    fn check_src(body: &str) -> Result<(), String> {
        let src = project_src(body);
        let project = parse(&src).expect("parse must succeed in checker tests");
        check(&project)
    }

    // --------- happy paths ---------

    #[test]
    fn empty_project_passes() {
        // Just `project + budget`, no other resources.
        assert!(check_src("").is_ok());
    }

    #[test]
    fn full_project_with_each_kind_passes() {
        let body = r#"
            model "m"      { requires = [chat]  context_min = 32k }
            prompt "p"     { file = "./x"  version = "1.0.0" }
            mcp_server "s" { image = "ghcr.io/x/y:1.0" }
            agent "a"      { model = model.m  system_prompt = prompt.p }
            eval "e"       { dataset = "./q"  metric = exact_match  threshold = 85% }
            secret "k"     { source = "env://X"  scope = [agent.a] }
            deploy "d"     { target = compose }
        "#;
        assert!(check_src(body).is_ok());
    }

    #[test]
    fn resource_graph_node_count_matches() {
        // White-box: confirm the graph has exactly the expected nodes.
        let body = r#"
            model "m"      { requires = [chat]  context_min = 32k }
            prompt "p"     { file = "./x"  version = "1.0.0" }
            agent "a"      { model = model.m  system_prompt = prompt.p }
        "#;
        let src = project_src(body);
        let project = parse(&src).expect("parse");
        let graph = build_resource_graph(&project).expect("build");
        assert_eq!(graph.graph.node_count(), 3);
        assert!(graph.nodes.contains_key(&(BlockKind::Model,  "m".into())));
        assert!(graph.nodes.contains_key(&(BlockKind::Prompt, "p".into())));
        assert!(graph.nodes.contains_key(&(BlockKind::Agent,  "a".into())));
    }

    // --------- duplicate-label rejections, one per kind ---------

    fn assert_duplicate(body: &str, kind: &str, label: &str) {
        let err = check_src(body).err().unwrap_or_else(||
            panic!("expected duplicate-label error for {kind} `{label}`, got Ok"));
        assert!(
            err.contains(&format!("duplicate `{kind}` block label `{label}`")),
            "error did not name the duplicate; got: {err}"
        );
    }

    #[test] fn duplicate_model_label_rejected() {
        let body = r#"
            model "fast" { requires = [chat]  context_min = 32k }
            model "fast" { requires = [chat]  context_min = 64k }
        "#;
        assert_duplicate(body, "model", "fast");
    }

    #[test] fn duplicate_prompt_label_rejected() {
        let body = r#"
            prompt "sys" { file = "./a"  version = "1.0.0" }
            prompt "sys" { file = "./b"  version = "2.0.0" }
        "#;
        assert_duplicate(body, "prompt", "sys");
    }

    #[test] fn duplicate_mcp_server_label_rejected() {
        let body = r#"
            mcp_server "docs" { image = "ghcr.io/a/b:1" }
            mcp_server "docs" { image = "ghcr.io/c/d:2" }
        "#;
        assert_duplicate(body, "mcp_server", "docs");
    }

    #[test] fn duplicate_agent_label_rejected() {
        let body = r#"
            model "m" { requires = [chat]  context_min = 32k }
            prompt "p" { file = "./x"  version = "1.0.0" }
            agent "support" { model = model.m  system_prompt = prompt.p }
            agent "support" { model = model.m  system_prompt = prompt.p }
        "#;
        assert_duplicate(body, "agent", "support");
    }

    #[test] fn duplicate_eval_label_rejected() {
        let body = r#"
            eval "acc" { dataset = "./q"  metric = exact_match  threshold = 80% }
            eval "acc" { dataset = "./r"  metric = f1           threshold = 90% }
        "#;
        assert_duplicate(body, "eval", "acc");
    }

    #[test] fn duplicate_secret_label_rejected() {
        let body = r#"
            model "m" { requires = [chat]  context_min = 32k }
            prompt "p" { file = "./x"  version = "1.0.0" }
            agent "a" { model = model.m  system_prompt = prompt.p }
            secret "k" { source = "env://X"  scope = [agent.a] }
            secret "k" { source = "env://Y"  scope = [agent.a] }
        "#;
        assert_duplicate(body, "secret", "k");
    }

    #[test] fn duplicate_deploy_label_rejected() {
        let body = r#"
            deploy "local" { target = compose }
            deploy "local" { target = compose }
        "#;
        assert_duplicate(body, "deploy", "local");
    }

    // --------- cross-kind collision is fine ---------

    #[test] fn same_label_across_different_kinds_is_ok() {
        // `model "x"` and `agent "x"` are independent — they live in
        // different namespaces (the graph key is (kind, label), not
        // label alone).
        let body = r#"
            model "x"  { requires = [chat]  context_min = 32k }
            prompt "x" { file = "./y"  version = "1.0.0" }
            agent "x"  { model = model.x  system_prompt = prompt.x }
        "#;
        assert!(check_src(body).is_ok());
    }
}
