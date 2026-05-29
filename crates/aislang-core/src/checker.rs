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
use serde::Serialize;
use std::collections::HashMap;
//
// NOTE: aislang-catalog is *not* a dependency of aislang-core — that
// would create a cycle (catalog already depends on core for the typed
// shims). Catalog ID validation lives in the `aislang-resolver` crate
// instead, which depends on both core and catalog. The CLI chains
// `aislang_core::validate` and `aislang_resolver::check_catalog_ids`,
// combining their `Vec<Warning>` outputs. See PLAN week-1 dep-graph
// note + week-3 architecture comment.

// ============================================================================
// Warning type — see PLAN-90-days-v0_1.md §Locked design decisions (deprecated
// catalog entries surface as warnings, not errors, for compliance audit)
// ============================================================================

/// A non-fatal semantic-check finding. CLI prints these under a
/// `warnings:` heading after the OK line; exit code remains 0.
///
/// In v0.1 a warning carries only a human-readable message. A future
/// version can extend with severity, source span, machine-readable
/// code — additive change, no API break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub message: String,
}

// ============================================================================
// Public entry points
// ============================================================================

/// Run all in-core semantic checks. Today: duplicate-label detection
/// (Mon) + cross-resource reference resolution (Tue). Catalog ID
/// validation lives in the sibling `aislang-resolver` crate (see
/// module-level NOTE on the dependency-cycle constraint).
///
/// Returns a `Vec<Warning>` for forward-compatibility — today always
/// empty, but the signature is stable so future in-core warnings
/// (e.g. deprecated grammar form, unused resource) can be added
/// additively without rippling through callers.
pub fn check(project: &Project) -> Result<Vec<Warning>, String> {
    let graph = build_resource_graph(project)?;
    check_refs(project, &graph)?;
    Ok(Vec::new())
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
    pub graph: DiGraph<NodeData, EdgeData>,
    pub nodes: HashMap<NodeKey, NodeIndex>,
}

#[derive(Debug, Clone)]
pub(crate) struct EdgeData {
    /// Field name on the source block that produced this reference.
    /// e.g. `"model"`, `"system_prompt"`, `"tools[0]"`, `"scope[1]"`.
    pub field: String,
}

pub(crate) fn build_resource_graph(project: &Project) -> Result<ResourceGraph, String> {
    // Explicit type — needed because DiGraph::new() can't infer EdgeData
    // from add_node calls alone (only add_edge would force it, and we
    // don't add edges here — that's populate_edges' job, called by
    // graph_summary later).
    let mut graph: DiGraph<NodeData, EdgeData> = DiGraph::new();
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
    graph: &mut DiGraph<NodeData, EdgeData>,
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
// Cross-resource reference resolution — Tuesday
// ============================================================================

/// Walk every `Ref` field on the project and verify the target block
/// is declared in the resource graph. Errors include a "did you mean
/// `X`?" suggestion when a label within Levenshtein distance ≤2 exists,
/// otherwise list the available labels of the expected kind (or note
/// none are declared at all).
fn check_refs(project: &Project, graph: &ResourceGraph) -> Result<(), String> {
    for agent in &project.agents {
        check_one_ref(graph, &agent.model,
            "agent", &agent.label, "model")?;
        check_one_ref(graph, &agent.system_prompt,
            "agent", &agent.label, "system_prompt")?;
        for (i, tool) in agent.tools.iter().enumerate() {
            check_one_ref(graph, tool,
                "agent", &agent.label, &format!("tools[{i}]"))?;
        }
    }
    for secret in &project.secrets {
        for (i, scope_ref) in secret.scope.iter().enumerate() {
            check_one_ref(graph, scope_ref,
                "secret", &secret.label, &format!("scope[{i}]"))?;
        }
    }
    for eval in &project.evals {
        if let Some(agent_ref) = &eval.agent {
            check_one_ref(graph, agent_ref,
                "eval", &eval.label, "agent")?;
        }
    }
    for mcp in &project.mcp_servers {
        if let Some(secrets) = &mcp.secrets {
            for (i, sec_ref) in secrets.iter().enumerate() {
                check_one_ref(graph, sec_ref,
                    "mcp_server", &mcp.label, &format!("secrets[{i}]"))?;
            }
        }
    }
    Ok(())
}

/// Validate one `Ref` against the resource graph. On miss, produce a
/// diagnostic that ends with one of:
///   - `; did you mean `X`?`           — if a near-match exists
///   - `; available `kind` labels: …`  — if no near-match but the kind
///                                       has some declared blocks
///   - `; no `kind` block is declared` — if the kind has zero blocks
fn check_one_ref(
    graph:     &ResourceGraph,
    r:         &Ref,
    src_kind:  &str,
    src_label: &str,
    src_field: &str,
) -> Result<(), String> {
    let target_kind = BlockKind::from_ref_kind(r.kind);
    let key: NodeKey = (target_kind, r.label.clone());
    if graph.nodes.contains_key(&key) {
        return Ok(());
    }

    let tail = suggest_or_list(graph, target_kind, &r.label);
    Err(format!(
        "{src_kind} `{src_label}` field `{src_field}` references \
         undeclared {} `{}`{tail}",
        target_kind.as_str(),
        r.label,
    ))
}

fn suggest_or_list(graph: &ResourceGraph, kind: BlockKind, target: &str) -> String {
    // Closest match within Levenshtein distance ≤2.
    let mut best: Option<(usize, &str)> = None;
    for ((node_kind, label), _) in &graph.nodes {
        if *node_kind != kind { continue; }
        let dist = levenshtein(target, label);
        if dist == 0 || dist > 2 { continue; }
        match best {
            None                       => best = Some((dist, label)),
            Some((d, _)) if dist < d   => best = Some((dist, label)),
            _ => {}
        }
    }
    if let Some((_, suggestion)) = best {
        return format!("; did you mean `{suggestion}`?");
    }

    // No near-match — list what's available, or note nothing is declared.
    let mut available: Vec<&str> = graph.nodes.iter()
        .filter(|((k, _), _)| *k == kind)
        .map(|((_, label), _)| label.as_str())
        .collect();
    if available.is_empty() {
        format!("; no `{}` block is declared in the project", kind.as_str())
    } else {
        available.sort();
        format!("; available `{}` labels: {:?}", kind.as_str(), available)
    }
}

/// Standard Levenshtein distance. Allocations: two Vec<char> for the
/// inputs + one Vec<usize> of size n+1 for the DP row. O(m·n) time,
/// O(n) space. Adequate for v0.1 — labels are short (typically <30
/// chars) and the candidate set per kind is small.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());

    if m == 0 { return n; }
    if n == 0 { return m; }

    let mut dp: Vec<usize> = (0..=n).collect();
    for i in 1..=m {
        let mut prev = dp[0];
        dp[0] = i;
        for j in 1..=n {
            let temp = dp[j];
            dp[j] = if a[i - 1] == b[j - 1] {
                prev
            } else {
                1 + dp[j - 1].min(dp[j]).min(prev)
            };
            prev = temp;
        }
    }
    dp[n]
}

// ============================================================================
// Public graph-summary API (week 3 Friday) — populates edges for every
// Ref in the project and converts the petgraph DiGraph into a small
// serializable shape suitable for CLI -vv (Debug) and -vvv (JSON IR)
// display.
// ============================================================================

#[derive(Debug, Clone, Serialize)]
pub struct GraphSummary {
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeSummary {
    pub kind:  String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgeSummary {
    pub from:  NodeSummary,
    pub to:    NodeSummary,
    /// Source-side field on `from` that produced this reference.
    pub field: String,
}

/// Build the resource graph (nodes + edges) and convert to a
/// serializable summary. Must be called on a project that has already
/// passed [`check`] — otherwise unresolved refs cause this to error.
pub fn graph_summary(project: &Project) -> Result<GraphSummary, String> {
    let mut graph = build_resource_graph(project)?;
    populate_edges(project, &mut graph)?;
    Ok(to_summary(&graph))
}

fn populate_edges(project: &Project, graph: &mut ResourceGraph) -> Result<(), String> {
    for agent in &project.agents {
        add_ref_edge(graph, BlockKind::Agent, &agent.label, "model",         &agent.model)?;
        add_ref_edge(graph, BlockKind::Agent, &agent.label, "system_prompt", &agent.system_prompt)?;
        for (i, tool) in agent.tools.iter().enumerate() {
            add_ref_edge(graph, BlockKind::Agent, &agent.label,
                &format!("tools[{i}]"), tool)?;
        }
    }
    for secret in &project.secrets {
        for (i, scope_ref) in secret.scope.iter().enumerate() {
            add_ref_edge(graph, BlockKind::Secret, &secret.label,
                &format!("scope[{i}]"), scope_ref)?;
        }
    }
    for eval in &project.evals {
        if let Some(agent_ref) = &eval.agent {
            add_ref_edge(graph, BlockKind::Eval, &eval.label, "agent", agent_ref)?;
        }
    }
    for mcp in &project.mcp_servers {
        if let Some(secrets) = &mcp.secrets {
            for (i, sec_ref) in secrets.iter().enumerate() {
                add_ref_edge(graph, BlockKind::McpServer, &mcp.label,
                    &format!("secrets[{i}]"), sec_ref)?;
            }
        }
    }
    Ok(())
}

fn add_ref_edge(
    graph:     &mut ResourceGraph,
    src_kind:  BlockKind,
    src_label: &str,
    field:     &str,
    r:         &Ref,
) -> Result<(), String> {
    let src_key: NodeKey = (src_kind, src_label.to_string());
    let src_idx = *graph.nodes.get(&src_key).ok_or_else(|| {
        format!("graph: source `{} {src_label}` not in node map (bug?)",
            src_kind.as_str())
    })?;

    let tgt_kind = BlockKind::from_ref_kind(r.kind);
    let tgt_key: NodeKey = (tgt_kind, r.label.clone());
    let tgt_idx = *graph.nodes.get(&tgt_key).ok_or_else(|| {
        format!("graph: target `{} {}` not in node map — graph_summary \
                 called before check() succeeded?",
            tgt_kind.as_str(), r.label)
    })?;

    graph.graph.add_edge(src_idx, tgt_idx, EdgeData { field: field.to_string() });
    Ok(())
}

fn to_summary(graph: &ResourceGraph) -> GraphSummary {
    let mut nodes: Vec<NodeSummary> = graph.graph.node_indices()
        .map(|idx| {
            let n = &graph.graph[idx];
            NodeSummary { kind: n.kind.as_str().to_string(), label: n.label.clone() }
        })
        .collect();
    // Deterministic order: sort by (kind, label). petgraph's
    // node_indices() iterates in insertion order, which depends on
    // project body ordering — that's also deterministic, but sorting
    // gives a stable JSON shape regardless of source ordering.
    nodes.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.label.cmp(&b.label)));

    let mut edges: Vec<EdgeSummary> = graph.graph.edge_indices()
        .map(|eidx| {
            let (src_idx, tgt_idx) = graph.graph.edge_endpoints(eidx).unwrap();
            let edge = &graph.graph[eidx];
            let src = &graph.graph[src_idx];
            let tgt = &graph.graph[tgt_idx];
            EdgeSummary {
                from:  NodeSummary { kind: src.kind.as_str().to_string(), label: src.label.clone() },
                to:    NodeSummary { kind: tgt.kind.as_str().to_string(), label: tgt.label.clone() },
                field: edge.field.clone(),
            }
        })
        .collect();
    edges.sort_by(|a, b| {
        a.from.kind.cmp(&b.from.kind)
            .then_with(|| a.from.label.cmp(&b.from.label))
            .then_with(|| a.field.cmp(&b.field))
    });

    GraphSummary { nodes, edges }
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
        // Tests that don't care about warnings drop them on the floor.
        // Tests that DO care use `check_src_with_warnings` (catalog
        // tests use this for the deprecated path).
        check_src_with_warnings(body).map(|_warnings| ())
    }

    fn check_src_with_warnings(body: &str) -> Result<Vec<Warning>, String> {
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

    // ----------------------------------------------------------------
    // Cross-resource reference resolution — Tuesday
    // ----------------------------------------------------------------

    fn assert_unresolved_ref(body: &str, needle: &str) {
        let err = check_src(body).err().unwrap_or_else(||
            panic!("expected unresolved-ref error containing `{needle}`, got Ok"));
        assert!(err.contains(needle),
            "error did not contain `{needle}`; got:\n{err}");
    }

    #[test] fn agent_model_ref_to_nonexistent_model_fails() {
        let body = r#"
            prompt "p" { file = "./x"  version = "1.0.0" }
            agent "a" { model = model.nonexistent  system_prompt = prompt.p }
        "#;
        assert_unresolved_ref(body,
            "agent `a` field `model` references undeclared model `nonexistent`");
    }

    #[test] fn agent_system_prompt_ref_to_nonexistent_prompt_fails() {
        let body = r#"
            model "m" { requires = [chat]  context_min = 32k }
            agent "a" { model = model.m  system_prompt = prompt.gone }
        "#;
        assert_unresolved_ref(body,
            "field `system_prompt` references undeclared prompt `gone`");
    }

    #[test] fn agent_tools_indexed_in_error_message() {
        // Position of the bad ref inside the tools list is named.
        let body = r#"
            model "m" { requires = [chat]  context_min = 32k }
            prompt "p" { file = "./x"  version = "1.0.0" }
            mcp_server "good" { image = "ghcr.io/a/b:1" }
            agent "a" {
                model = model.m
                system_prompt = prompt.p
                tools = [mcp_server.good, mcp_server.bad]
            }
        "#;
        assert_unresolved_ref(body, "field `tools[1]`");
    }

    #[test] fn typo_suggests_did_you_mean() {
        // Levenshtein-distance-1 typo: `fust` → `fast`.
        let body = r#"
            model "fast" { requires = [chat]  context_min = 32k }
            prompt "p"   { file = "./x"  version = "1.0.0" }
            agent "a"    { model = model.fust  system_prompt = prompt.p }
        "#;
        assert_unresolved_ref(body, "did you mean `fast`?");
    }

    #[test] fn typo_distance_2_still_suggests() {
        // Two character changes within the threshold.
        let body = r#"
            model "fast" { requires = [chat]  context_min = 32k }
            prompt "p"   { file = "./x"  version = "1.0.0" }
            agent "a"    { model = model.fxst  system_prompt = prompt.p }
        "#;
        assert_unresolved_ref(body, "did you mean `fast`?");
    }

    #[test] fn no_close_match_lists_available_labels() {
        // Distance > 2 — no suggestion; instead list what's there.
        let body = r#"
            model "fast" { requires = [chat]  context_min = 32k }
            prompt "p"   { file = "./x"  version = "1.0.0" }
            agent "a"    { model = model.qrtxyz  system_prompt = prompt.p }
        "#;
        assert_unresolved_ref(body, "available `model` labels: [\"fast\"]");
    }

    #[test] fn no_blocks_declared_says_so() {
        // The agent references a model, but the project has no model
        // blocks at all.
        let body = r#"
            prompt "p" { file = "./x"  version = "1.0.0" }
            agent "a"  { model = model.fast  system_prompt = prompt.p }
        "#;
        assert_unresolved_ref(body, "no `model` block is declared in the project");
    }

    #[test] fn secret_scope_unresolved_agent_fails() {
        let body = r#"
            secret "k" { source = "env://X"  scope = [agent.missing] }
        "#;
        assert_unresolved_ref(body,
            "secret `k` field `scope[0]` references undeclared agent `missing`");
    }

    #[test] fn eval_agent_ref_optional_but_validated_when_present() {
        let body = r#"
            eval "e" {
                dataset = "./q"
                metric = exact_match
                threshold = 80%
                agent = agent.missing
            }
        "#;
        assert_unresolved_ref(body,
            "eval `e` field `agent` references undeclared agent `missing`");
    }

    #[test] fn mcp_server_secrets_ref_validated() {
        let body = r#"
            mcp_server "docs" {
                image = "ghcr.io/x/y:1"
                secrets = [secret.absent]
            }
        "#;
        assert_unresolved_ref(body,
            "mcp_server `docs` field `secrets[0]` references undeclared secret `absent`");
    }

    #[test] fn forward_reference_is_fine() {
        // Refs to a block declared LATER in the source — parser
        // accepts them, checker validates against the complete graph
        // (built before refs are walked), so forward refs work.
        let body = r#"
            agent "a" { model = model.later  system_prompt = prompt.later }
            model "later" { requires = [chat]  context_min = 32k }
            prompt "later" { file = "./x"  version = "1.0.0" }
        "#;
        assert!(check_src(body).is_ok());
    }

    // White-box on the Levenshtein helper itself — small unit checks
    // pinning the table on which the "did you mean" suggestions rest.
    #[test] fn levenshtein_basics() {
        assert_eq!(super::levenshtein("", ""), 0);
        assert_eq!(super::levenshtein("a", ""), 1);
        assert_eq!(super::levenshtein("", "abc"), 3);
        assert_eq!(super::levenshtein("fast", "fast"), 0);
        assert_eq!(super::levenshtein("fast", "fust"), 1);   // sub
        assert_eq!(super::levenshtein("fast", "fasts"), 1);  // ins
        assert_eq!(super::levenshtein("fast", "fas"), 1);    // del
        assert_eq!(super::levenshtein("kitten", "sitting"), 3);   // classic
    }

    // Catalog ID validation tests live in `crates/aislang-resolver/`
    // (separate crate to avoid the aislang-core ↔ aislang-catalog
    // dependency cycle). See module-level NOTE.

    // ----------------------------------------------------------------
    // Graph summary — Friday
    // ----------------------------------------------------------------

    fn graph_for(body: &str) -> GraphSummary {
        let src = project_src(body);
        let project = parse(&src).expect("parse must succeed");
        super::graph_summary(&project).expect("graph_summary must succeed")
    }

    #[test] fn graph_summary_empty_project_has_no_nodes_or_edges() {
        let g = graph_for("");
        assert!(g.nodes.is_empty());
        assert!(g.edges.is_empty());
    }

    #[test] fn graph_summary_nodes_for_each_kind() {
        let body = r#"
            model "m"      { requires = [chat]  context_min = 32k }
            prompt "p"     { file = "./x"  version = "1.0.0" }
            mcp_server "s" { image = "ghcr.io/x/y:1.0" }
            agent "a"      { model = model.m  system_prompt = prompt.p }
            eval "e"       { dataset = "./q"  metric = exact_match  threshold = 85% }
            deploy "d"     { target = compose }
        "#;
        let g = graph_for(body);
        assert_eq!(g.nodes.len(), 6,
            "expected one node per declared block; got {g:?}");
        // Sorted by (kind, label) — first by kind alphabetically.
        let kinds: Vec<&str> = g.nodes.iter().map(|n| n.kind.as_str()).collect();
        assert_eq!(kinds, vec!["agent", "deploy", "eval", "mcp_server", "model", "prompt"]);
    }

    #[test] fn graph_summary_edges_record_field_origin() {
        let body = r#"
            model "m"      { requires = [chat]  context_min = 32k }
            prompt "p"     { file = "./x"  version = "1.0.0" }
            mcp_server "s" { image = "ghcr.io/x/y:1.0" }
            agent "a"      {
                model         = model.m
                system_prompt = prompt.p
                tools         = [mcp_server.s]
            }
        "#;
        let g = graph_for(body);
        // agent.a should have 3 outgoing edges (model, system_prompt, tools[0]).
        let from_a: Vec<&EdgeSummary> = g.edges.iter()
            .filter(|e| e.from.kind == "agent" && e.from.label == "a")
            .collect();
        assert_eq!(from_a.len(), 3);
        let fields: Vec<&str> = from_a.iter().map(|e| e.field.as_str()).collect();
        // Sorted by field name (the to_summary sort).
        assert_eq!(fields, vec!["model", "system_prompt", "tools[0]"]);
    }

    #[test] fn graph_summary_indexed_tools_become_separate_edges() {
        let body = r#"
            model "m"      { requires = [chat]  context_min = 32k }
            prompt "p"     { file = "./x"  version = "1.0.0" }
            mcp_server "s1" { image = "ghcr.io/x/y:1.0" }
            mcp_server "s2" { image = "ghcr.io/x/z:1.0" }
            agent "a"      {
                model         = model.m
                system_prompt = prompt.p
                tools         = [mcp_server.s1, mcp_server.s2]
            }
        "#;
        let g = graph_for(body);
        let tools_edges: Vec<&EdgeSummary> = g.edges.iter()
            .filter(|e| e.field.starts_with("tools["))
            .collect();
        assert_eq!(tools_edges.len(), 2);
        let fields: Vec<&str> = tools_edges.iter().map(|e| e.field.as_str()).collect();
        assert_eq!(fields, vec!["tools[0]", "tools[1]"]);
    }

    #[test] fn graph_summary_json_round_trips() {
        // Sanity: the summary is structurally JSON-serializable and
        // deserializes to the same nodes/edges (using serde_json::Value
        // for round-trip — GraphSummary doesn't itself derive Deserialize
        // because the type is a one-way IR-export shape).
        let body = r#"
            model "m" { requires = [chat]  context_min = 32k }
            prompt "p" { file = "./x"  version = "1.0.0" }
            agent "a" { model = model.m  system_prompt = prompt.p }
        "#;
        let g = graph_for(body);
        let json = serde_json::to_string_pretty(&g).expect("serializes");
        assert!(json.contains("\"nodes\""));
        assert!(json.contains("\"edges\""));
        assert!(json.contains("\"model\""));
        assert!(json.contains("\"system_prompt\""));
    }
}
