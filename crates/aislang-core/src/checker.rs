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
use std::collections::{BTreeMap, HashMap};
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
///
/// v0.3 Week 16 Tue: single-file (`check`) errors on any cross-module
/// `Ref` (`r.module = Some(_)`) — single-file projects have no module
/// symbol table.  Multi-module projects go through [`check_multi`].
pub fn check(project: &Project) -> Result<Vec<Warning>, String> {
    let graph = build_resource_graph(project)?;
    check_refs(project, &graph, None)?;
    Ok(Vec::new())
}

/// v0.3 Week 16 Tue — multi-module semantic check.
///
/// Builds one `ResourceGraph` per module, then walks every `Ref` in
/// every module and routes the lookup:
///
/// - Same-module ref (`r.module = None`) → look up in the *source*
///   module's local graph.  Identical behaviour to [`check`].
/// - Cross-module ref (`r.module = Some(target)`) → look up in
///   `target`'s graph; errors if `target` isn't a declared module
///   ("module … not declared in the project manifest") or if `target`
///   doesn't export the labelled resource ("module `target` does not
///   export …").  Same source-aware diagnostic style as v0.2's
///   undeclared-ref errors.
///
/// `modules` is the project manifest's `[modules]` table mapping
/// module name to parsed AST.  Iteration order is `BTreeMap`-sorted
/// for deterministic error messages.
///
/// Decision 44.1 sub-point: all cross-module resources are public
/// (private-resource access control deferred to v0.5+).
pub fn check_multi(modules: &BTreeMap<String, Project>) -> Result<Vec<Warning>, String> {
    let mut graphs: BTreeMap<String, ResourceGraph> = BTreeMap::new();
    for (name, project) in modules {
        let g = build_resource_graph(project)
            .map_err(|e| format!("module `{name}`: {e}"))?;
        graphs.insert(name.clone(), g);
    }
    for (name, project) in modules {
        let local = graphs.get(name).expect("preloaded above");
        check_refs(project, local, Some((name.as_str(), &graphs)))
            .map_err(|e| format!("module `{name}`: {e}"))?;
    }
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
    Main,      // v0.2 Fri
    Pipeline,    // v0.3 Week 17 Mon (Decision 44.5)
    Loop,        // v0.3 Week 17 Wed (Decision 44.5 #2)
    Branch,      // v0.3 Week 17 Wed (Decision 44.5 #3)
    Skill,       // v0.3 Week 18 Wed (Decision 44.x)
    VectorStore, // v0.3 Week 19 Mon (Decision 44.6)
    Embedding,   // v0.3 Week 19 Mon (kind only; grammar lands Tue)
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
            Self::Main      => "main",
            Self::Pipeline    => "pipeline",
            Self::Loop        => "loop",
            Self::Branch      => "branch",
            Self::Skill       => "skill",
            Self::VectorStore => "vector_store",
            Self::Embedding   => "embedding",
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
            RefKind::Pipeline  => Self::Pipeline,
            RefKind::Embedding => Self::Embedding,
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
    // v0.2 Fri: main blocks join the graph so duplicate-label
    // detection works (`main "x"` + `main "x"` → error) AND so
    // future graph-renderers visualize them alongside agents.
    for m in &project.mains       { add_block(&mut graph, &mut nodes, BlockKind::Main,      &m.label)?; }
    // v0.3 Week 17 Mon: pipeline blocks join the graph for the same
    // duplicate-label detection.  Pipelines are terminal nodes in
    // the resource graph (no other block kind references a pipeline
    // — yet; Tue's `loop` + `branch` will, and they go through the
    // edges pass like agents/secrets do today).
    for p in &project.pipelines   { add_block(&mut graph, &mut nodes, BlockKind::Pipeline,  &p.label)?; }
    // v0.3 Week 17 Wed: loop + branch blocks join the graph.  Same
    // duplicate-label discipline as pipeline/main/agent.
    for l in &project.loops       { add_block(&mut graph, &mut nodes, BlockKind::Loop,      &l.label)?; }
    for b in &project.branches    { add_block(&mut graph, &mut nodes, BlockKind::Branch,    &b.label)?; }
    // v0.3 Week 18 Wed: skill blocks join the graph for duplicate-
    // label detection.  Skills are terminal nodes today — Thu's
    // runtime executor reads the `exec`/`mcp` field directly; no
    // other block kind references a skill yet (agent.skills wiring
    // is Phase-2 work).
    for s in &project.skills      { add_block(&mut graph, &mut nodes, BlockKind::Skill,     &s.label)?; }
    // v0.3 Week 19 Mon: vector_store blocks join the graph for
    // duplicate-label detection + as targets for future agent.
    // knowledge refs (Phase 2).  The embedding-ref edge into Tue's
    // embedding block is wired in `check_refs` below.
    for v in &project.vector_stores {
        add_block(&mut graph, &mut nodes, BlockKind::VectorStore, &v.label)?;
    }

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
///
/// v0.3 Week 16 Tue: `multi` is `None` for single-file projects
/// (cross-module refs error) or `Some((src_module, graphs))` for
/// multi-module projects (cross-module refs route to `graphs[target]`).
fn check_refs(
    project: &Project,
    graph:   &ResourceGraph,
    multi:   Option<(&str, &BTreeMap<String, ResourceGraph>)>,
) -> Result<(), String> {
    for agent in &project.agents {
        dispatch_ref(graph, multi, &agent.model,
            "agent", &agent.label, "model")?;
        dispatch_ref(graph, multi, &agent.system_prompt,
            "agent", &agent.label, "system_prompt")?;
        for (i, tool) in agent.tools.iter().enumerate() {
            dispatch_ref(graph, multi, tool,
                "agent", &agent.label, &format!("tools[{i}]"))?;
        }
    }
    // v0.2 Fri: `main` block ref validation.  Same shape as agent —
    // both fields are required Ref's.  `chat_client` + `pattern` are
    // enum members already validated at parse time, so no ref work
    // for them.
    for main in &project.mains {
        dispatch_ref(graph, multi, &main.model,
            "main", &main.label, "model")?;
        dispatch_ref(graph, multi, &main.system_prompt,
            "main", &main.label, "system_prompt")?;
    }
    for secret in &project.secrets {
        for (i, scope_ref) in secret.scope.iter().enumerate() {
            dispatch_ref(graph, multi, scope_ref,
                "secret", &secret.label, &format!("scope[{i}]"))?;
        }
    }
    for eval in &project.evals {
        if let Some(agent_ref) = &eval.agent {
            dispatch_ref(graph, multi, agent_ref,
                "eval", &eval.label, "agent")?;
        }
    }
    for mcp in &project.mcp_servers {
        if let Some(secrets) = &mcp.secrets {
            for (i, sec_ref) in secrets.iter().enumerate() {
                dispatch_ref(graph, multi, sec_ref,
                    "mcp_server", &mcp.label, &format!("secrets[{i}]"))?;
            }
        }
    }
    // v0.3 Week 17 Mon: pipeline steps[] — each entry references an
    // `agent` block.  Same dispatch machinery as `agent.tools[i]` —
    // cross-module aware, source-aware diagnostics, did-you-mean
    // suggester all reused as-is.
    for pipeline in &project.pipelines {
        for (i, step_ref) in pipeline.steps.iter().enumerate() {
            dispatch_ref(graph, multi, step_ref,
                "pipeline", &pipeline.label, &format!("steps[{i}]"))?;
        }
    }
    // v0.3 Week 17 Wed: loop.body + branch.cases[case] + branch.default
    // — each references a `pipeline` block.  Cross-module-aware via
    // the existing dispatch_ref helper.
    for lp in &project.loops {
        dispatch_ref(graph, multi, &lp.body,
            "loop", &lp.label, "body")?;
    }
    for br in &project.branches {
        // Iteration over BTreeMap is sorted — deterministic
        // diagnostic order when multiple cases reference unknown
        // pipelines.
        for (case_key, case_ref) in &br.cases {
            dispatch_ref(graph, multi, case_ref,
                "branch", &br.label, &format!("cases[{case_key}]"))?;
        }
        dispatch_ref(graph, multi, &br.default,
            "branch", &br.label, "default")?;
    }
    // v0.3 Week 18 Wed: skill blocks with `mcp = mcp_server.<x>.tool.
    // <y>` execution carry a Ref<McpServer>.  Validate the server
    // label resolves — same dispatch_ref machinery as agent.tools.
    // Shell-exec skills (`SkillExec::Shell`) carry no ref + need no
    // checking at this layer; the runtime (Thu) handles command
    // sandboxing.
    for sk in &project.skills {
        if let crate::ast::SkillExec::Mcp { server, .. } = &sk.exec {
            dispatch_ref(graph, multi, server,
                "skill", &sk.label, "mcp.server")?;
        }
    }
    // v0.3 Week 19 Mon: vector_store.embedding is a Ref<Embedding>.
    // The actual `embedding "<label>" { … }` block lands Tue — until
    // then this edge will report "undeclared embedding `X`" for
    // every vector_store, which is the correct behaviour
    // (vector_store can't be used without an embedding).  Same
    // dispatch_ref machinery as agent.tools / skill.mcp.server.
    //
    // **Check order**: structural invariants (chunk_overlap <
    // chunk_size) run BEFORE the embedding-ref check so the
    // ref-resolution failure doesn't shadow a malformed
    // chunk_overlap value — gives the caller the better diagnostic
    // when both are wrong.
    for vs in &project.vector_stores {
        // (a) Structural: chunk_overlap must be strictly less than
        //     chunk_size.  Parser enforces the 256..=8192 range on
        //     chunk_size; this invariant is the chunker's correctness
        //     contract — equal overlap means every chunk is identical
        //     to its predecessor (zero-progress; can't index).
        if vs.chunk_overlap.units >= vs.chunk_size.units {
            return Err(format!(
                "vector_store `{}` has chunk_overlap ({} units) ≥ \
                 chunk_size ({} units); overlap must be strictly less \
                 (a zero-progress chunker can't index anything)",
                vs.label, vs.chunk_overlap.units, vs.chunk_size.units));
        }
        // (b) Reference: embedding must resolve to a declared block.
        dispatch_ref(graph, multi, &vs.embedding,
            "vector_store", &vs.label, "embedding")?;
    }
    Ok(())
}

/// v0.3 Week 16 Tue — picks the lookup graph for one ref based on
/// `r.module` + dispatches to the (unchanged) [`check_one_ref`]
/// primitive.  Diagnostic strings match the v0.2 style.
fn dispatch_ref(
    local_graph: &ResourceGraph,
    multi:       Option<(&str, &BTreeMap<String, ResourceGraph>)>,
    r:           &Ref,
    src_kind:    &str,
    src_label:   &str,
    src_field:   &str,
) -> Result<(), String> {
    let Some(target_module) = &r.module else {
        return check_one_ref(local_graph, r, src_kind, src_label, src_field);
    };

    // Cross-module ref.  Either single-file (no module map → error) or
    // multi-module (look up the target).
    let Some((_src_module, graphs)) = multi else {
        return Err(format!(
            "{src_kind} `{src_label}` field `{src_field}` is a cross-module \
             reference (`use {target_module}.{label}`) but the project is a \
             single-file build with no module manifest; declare an \
             `aislang.toml` listing `{target_module}` to enable this ref",
            label = r.label,
        ));
    };
    let Some(target_graph) = graphs.get(target_module) else {
        let mut known: Vec<&str> = graphs.keys().map(|s| s.as_str()).collect();
        known.sort();
        return Err(format!(
            "{src_kind} `{src_label}` field `{src_field}` references module \
             `{target_module}` which is not declared in the project \
             manifest; declared modules: {known:?}",
        ));
    };
    // Reuse the same diagnostic surface as same-module refs, but
    // prefix the source field hint so the error names the cross-
    // module ownership.
    check_one_ref(target_graph, r,
        src_kind, src_label,
        &format!("{src_field} (cross-module → `{target_module}`)"))
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
    // v0.2 Fri: main edges (same shape as agent — model + system_prompt).
    for main in &project.mains {
        add_ref_edge(graph, BlockKind::Main, &main.label, "model",         &main.model)?;
        add_ref_edge(graph, BlockKind::Main, &main.label, "system_prompt", &main.system_prompt)?;
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

    // ────────────────────────────────────────────────────────────────
    // v0.2 Week-12 Fri: main-block checker tests.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn main_block_with_valid_refs_passes() {
        let body = r#"
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
            }
            prompt "main_brief" {
                file    = "./main_brief.md"
                version = "1.0.0"
            }
            main "user_facing" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.main_brief
            }
        "#;
        assert!(check_src(body).is_ok(),
            "main block with valid refs should pass checker");
    }

    #[test]
    fn main_block_with_undeclared_model_errors() {
        let body = r#"
            prompt "main_brief" { file = "./b.md"  version = "1.0.0" }
            main "user_facing" {
                model         = model.nonexistent
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.main_brief
            }
        "#;
        let err = check_src(body).expect_err("undeclared model should error");
        assert!(err.contains("main `user_facing` field `model` references undeclared model"),
            "error should name source main + field + missing model; got: {err}");
    }

    #[test]
    fn main_block_with_undeclared_system_prompt_errors() {
        let body = r#"
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
            }
            main "user_facing" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.missing
            }
        "#;
        let err = check_src(body).expect_err("undeclared prompt should error");
        assert!(err.contains("main `user_facing` field `system_prompt` references undeclared prompt"),
            "error should name source main + field + missing prompt; got: {err}");
    }

    #[test]
    fn duplicate_main_labels_error() {
        let body = r#"
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
            }
            prompt "b1" { file = "./b1.md"  version = "1.0.0" }
            prompt "b2" { file = "./b2.md"  version = "1.0.0" }
            main "user_facing" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.b1
            }
            main "user_facing" {
                model         = model.primary
                chat_client   = cursor
                pattern       = supervisor
                system_prompt = prompt.b2
            }
        "#;
        let err = check_src(body).expect_err("duplicate main labels should error");
        assert!(err.contains("main") && err.contains("user_facing"),
            "error should name the duplicate kind+label; got: {err}");
    }

    #[test]
    fn main_block_label_shares_space_with_agent_but_disjoint_kinds() {
        // `agent "x"` and `main "x"` MAY both be declared — they're
        // different BlockKinds in the graph, so they don't collide.
        // Pinned because future graph-rendering work may erroneously
        // collapse the namespace.
        let body = r#"
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
            }
            prompt "b" { file = "./b.md"  version = "1.0.0" }
            agent "x" {
                model         = model.primary
                system_prompt = prompt.b
                tools         = []
            }
            main "x" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.b
            }
        "#;
        assert!(check_src(body).is_ok(),
            "agent and main blocks can share a label (different kinds)");
    }

    #[test]
    fn graph_includes_main_nodes() {
        // Pin that `main` blocks land in the resource graph so future
        // graph-rendering tooling can visualize them.
        let body = r#"
            model "primary" {
                requires    = [chat]
                context_min = 32k
                prefer      = [anthropic.claude_sonnet_4_6]
            }
            prompt "b" { file = "./b.md"  version = "1.0.0" }
            main "user_facing" {
                model         = model.primary
                chat_client   = claude_desktop
                pattern       = orchestrator
                system_prompt = prompt.b
            }
        "#;
        let g = graph_for(body);
        let json = serde_json::to_string(&g).expect("serializes");
        assert!(json.contains("\"main\""),
            "graph should expose main node kind; got: {json}");
        assert!(json.contains("\"user_facing\""),
            "graph should expose the main's label");
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 16 Tue — cross-module ref resolution (check_multi).
    // ---------------------------------------------------------------- //

    /// Helper: parse a labelled source string into a Project so the
    /// multi-module tests can compose two-module fixtures.
    fn parse_labelled(label: &str, src: &str) -> Project {
        parse(src).unwrap_or_else(|errs| {
            let joined = errs.iter().map(|e| e.message.as_str())
                .collect::<Vec<_>>().join("\n");
            panic!("module `{label}` source failed to parse:\n{joined}")
        })
    }

    /// Helper: build the two-module fixture used by most cross-module
    /// tests.  Module `app` references resources in module `shared`.
    fn two_module_fixture() -> BTreeMap<String, Project> {
        let shared = parse_labelled("shared", r#"project "shared" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "fast" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
        }"#);
        let app = parse_labelled("app", r#"project "app" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          agent "support" {
            model         = use shared.fast
            system_prompt = use shared.system
            tools         = []
          }
        }"#);
        let mut m = BTreeMap::new();
        m.insert("shared".to_string(), shared);
        m.insert("app".to_string(),    app);
        m
    }

    #[test] fn check_multi_resolves_cross_module_refs() {
        let modules = two_module_fixture();
        check_multi(&modules).expect(
            "two-module project with valid cross-module refs must validate");
    }

    #[test] fn check_multi_rejects_unknown_module() {
        // App refers to module `missing` which isn't in the manifest.
        let mut modules = BTreeMap::new();
        modules.insert("app".to_string(), parse_labelled("app", r#"project "app" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "support" {
            model         = use missing.fast
            system_prompt = prompt.system
            tools         = []
          }
        }"#));
        let err = check_multi(&modules).unwrap_err();
        assert!(err.contains("module `missing`"),
            "error should name the missing module; got: {err}");
        assert!(err.contains("not declared in the project manifest"),
            "error should hint at the manifest; got: {err}");
        assert!(err.contains("declared modules"),
            "error should list the modules that ARE declared; got: {err}");
    }

    #[test] fn check_multi_rejects_unknown_label_in_known_module() {
        // App refers to module `shared.zzz` but `shared` only exports
        // `fast` (model) + `system` (prompt).  Pin the source-aware
        // suggestion shape: name the source module + the available
        // labels of the target kind.
        let shared = parse_labelled("shared", r#"project "shared" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "fast" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
        }"#);
        let app = parse_labelled("app", r#"project "app" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          agent "support" {
            model         = use shared.zzz
            system_prompt = use shared.system
            tools         = []
          }
        }"#);
        let mut m = BTreeMap::new();
        m.insert("shared".to_string(), shared);
        m.insert("app".to_string(),    app);
        let err = check_multi(&m).unwrap_err();
        assert!(err.contains("undeclared model `zzz`"),
            "error should name the missing label + kind; got: {err}");
        assert!(err.contains("cross-module → `shared`"),
            "error should name the source module to disambiguate; \
             got: {err}");
    }

    #[test] fn check_multi_did_you_mean_works_across_modules() {
        // Typo `fst` for `fast` → Levenshtein within ≤2 → suggester
        // should fire just like the same-module case.
        let shared = parse_labelled("shared", r#"project "shared" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "fast" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
        }"#);
        let app = parse_labelled("app", r#"project "app" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "support" {
            model         = use shared.fst
            system_prompt = prompt.system
            tools         = []
          }
        }"#);
        let mut m = BTreeMap::new();
        m.insert("shared".to_string(), shared);
        m.insert("app".to_string(),    app);
        let err = check_multi(&m).unwrap_err();
        assert!(err.contains("did you mean `fast`?"),
            "near-match should be suggested across modules too; \
             got: {err}");
    }

    #[test] fn single_file_check_rejects_cross_module_ref() {
        // A .ais that uses `use <module>.<label>` syntax but is run
        // through the single-file checker (no manifest) → must error
        // with a useful hint.
        let project = parse_labelled("p", r#"project "p" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          prompt "system" { file = "./s.md"  version = "1.0.0" }
          agent "support" {
            model         = use shared.fast
            system_prompt = prompt.system
            tools         = []
          }
        }"#);
        let err = check(&project).unwrap_err();
        assert!(err.contains("cross-module reference"),
            "error should explain WHY single-file rejects this ref; \
             got: {err}");
        assert!(err.contains("aislang.toml"),
            "error should point at the manifest as the fix; got: {err}");
    }

    #[test] fn check_multi_per_module_duplicate_labels_still_caught() {
        // Same-module duplicate-label detection must keep working
        // when running through check_multi.
        let bad = parse_labelled("bad", r#"project "bad" {
          budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
          model "fast" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
          model "fast" {
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }
        }"#);
        let mut m = BTreeMap::new();
        m.insert("bad".to_string(), bad);
        let err = check_multi(&m).unwrap_err();
        assert!(err.contains("duplicate"),
            "duplicate-label detection should still fire in multi-module; \
             got: {err}");
        assert!(err.contains("module `bad`"),
            "error should be prefixed with the source module name; \
             got: {err}");
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 17 Mon — `pipeline { … }` block validation.
    // ---------------------------------------------------------------- //

    /// Helper for the pipeline-checker tests.  Same shape as the
    /// other helpers — builds a synthetic project body and runs
    /// `check()` against it.
    fn pipeline_body(steps_block: &str) -> String {
        format!(r#"
          model "m" {{
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }}
          prompt "sys" {{ file = "./s.md"  version = "1.0.0" }}
          agent "a" {{ model = model.m  system_prompt = prompt.sys  tools = [] }}
          agent "b" {{ model = model.m  system_prompt = prompt.sys  tools = [] }}
          {steps_block}
        "#)
    }

    #[test] fn pipeline_with_valid_step_refs_passes() {
        check_src(&pipeline_body(r#"
          pipeline "two_step" {
            steps = [agent.a, agent.b]
          }
        "#)).expect("valid agent refs in steps[] must pass");
    }

    #[test] fn pipeline_step_ref_to_undeclared_agent_fails() {
        let err = check_src(&pipeline_body(r#"
          pipeline "broken" {
            steps = [agent.a, agent.ghost]
          }
        "#)).unwrap_err();
        assert!(err.contains("pipeline `broken`"),
            "error should name the source pipeline; got: {err}");
        assert!(err.contains("field `steps[1]`"),
            "error should index the bad step; got: {err}");
        assert!(err.contains("undeclared agent `ghost`"),
            "error should name the missing label + kind; got: {err}");
    }

    #[test] fn pipeline_step_typo_suggests_did_you_mean() {
        // Typo `agent.a` vs `agent.aa` (1-edit) — Levenshtein
        // suggester should fire from the same `suggest_or_list`
        // path same-module refs use.
        let err = check_src(&pipeline_body(r#"
          pipeline "typo" {
            steps = [agent.aa]
          }
        "#)).unwrap_err();
        assert!(err.contains("did you mean `a`?"),
            "near-match should be suggested for pipeline steps too; \
             got: {err}");
    }

    #[test] fn duplicate_pipeline_label_rejected() {
        let err = check_src(&pipeline_body(r#"
          pipeline "dup" {
            steps = [agent.a]
          }
          pipeline "dup" {
            steps = [agent.b]
          }
        "#)).unwrap_err();
        assert!(err.contains("duplicate `pipeline` block label `dup`"),
            "duplicate-label detection should fire for pipelines; \
             got: {err}");
    }

    #[test] fn pipeline_and_agent_can_share_label() {
        // Pipelines + agents live in disjoint namespaces (different
        // BlockKind variants), so `pipeline "foo"` + `agent "foo"`
        // is legal — same rule v0.2 Fri shipped for `agent` vs `main`.
        check_src(&pipeline_body(r#"
          pipeline "a" {
            steps = [agent.a]
          }
        "#)).expect(
            "pipeline + agent sharing a label must be allowed \
             (different BlockKinds)");
    }

    // ---------------------------------------------------------------- //
    // v0.3 Week 17 Wed — `loop` + `branch` block validation.
    // ---------------------------------------------------------------- //

    /// Helper: same shape as `pipeline_body` but the body fragment
    /// pre-declares 4 pipelines (body / yes / no / fallback) that
    /// the loop + branch tests reference.
    fn composition_body(extra: &str) -> String {
        format!(r#"
          model "m" {{
            requires    = [chat]
            context_min = 32k
            prefer      = [ollama.llama_3_1_8b]
            pin         = ollama.llama_3_1_8b
          }}
          prompt "sys" {{ file = "./s.md"  version = "1.0.0" }}
          agent "a" {{ model = model.m  system_prompt = prompt.sys  tools = [] }}
          pipeline "body"     {{ steps = [agent.a] }}
          pipeline "yes"      {{ steps = [agent.a] }}
          pipeline "no"       {{ steps = [agent.a] }}
          pipeline "fallback" {{ steps = [agent.a] }}
          {extra}
        "#)
    }

    #[test] fn loop_with_valid_body_ref_passes() {
        check_src(&composition_body(r#"
          loop "review_cycle" {
            body      = pipeline.body
            condition = "{{ x }}"
            max_iter  = 5
          }
        "#)).expect("valid loop must check");
    }

    #[test] fn loop_body_ref_to_undeclared_pipeline_fails() {
        let err = check_src(&composition_body(r#"
          loop "broken" {
            body      = pipeline.ghost
            condition = "{{ x }}"
            max_iter  = 1
          }
        "#)).unwrap_err();
        assert!(err.contains("loop `broken`"),
            "error must name the source loop; got: {err}");
        assert!(err.contains("field `body`"),
            "error must name the failing field; got: {err}");
        assert!(err.contains("undeclared pipeline `ghost`"),
            "error must name the missing label + kind; got: {err}");
    }

    #[test] fn branch_with_valid_refs_passes() {
        check_src(&composition_body(r#"
          branch "review_outcome" {
            on      = "{{ x }}"
            cases   = { approve = pipeline.yes, reject = pipeline.no }
            default = pipeline.fallback
          }
        "#)).expect("valid branch must check");
    }

    #[test] fn branch_case_ref_to_undeclared_pipeline_fails() {
        let err = check_src(&composition_body(r#"
          branch "b" {
            on      = "{{ x }}"
            cases   = { ok = pipeline.ghost }
            default = pipeline.fallback
          }
        "#)).unwrap_err();
        assert!(err.contains("branch `b`"),
            "error must name the source branch; got: {err}");
        assert!(err.contains("cases[ok]"),
            "error must name the failing case; got: {err}");
        assert!(err.contains("undeclared pipeline `ghost`"),
            "error must explain what's missing; got: {err}");
    }

    #[test] fn branch_default_ref_to_undeclared_pipeline_fails() {
        let err = check_src(&composition_body(r#"
          branch "b" {
            on      = "{{ x }}"
            cases   = { ok = pipeline.yes }
            default = pipeline.ghost
          }
        "#)).unwrap_err();
        assert!(err.contains("field `default`"),
            "error must name the default field; got: {err}");
        assert!(err.contains("undeclared pipeline `ghost`"),
            "error must explain what's missing; got: {err}");
    }

    #[test] fn duplicate_loop_label_rejected() {
        let err = check_src(&composition_body(r#"
          loop "dup" {
            body      = pipeline.body
            condition = "{{ x }}"
            max_iter  = 1
          }
          loop "dup" {
            body      = pipeline.body
            condition = "{{ x }}"
            max_iter  = 1
          }
        "#)).unwrap_err();
        assert!(err.contains("duplicate `loop` block label `dup`"),
            "duplicate-label detection should fire for loops; got: {err}");
    }

    #[test] fn duplicate_branch_label_rejected() {
        let err = check_src(&composition_body(r#"
          branch "dup" {
            on      = "{{ x }}"
            cases   = { ok = pipeline.yes }
            default = pipeline.fallback
          }
          branch "dup" {
            on      = "{{ x }}"
            cases   = { ok = pipeline.no }
            default = pipeline.fallback
          }
        "#)).unwrap_err();
        assert!(err.contains("duplicate `branch` block label `dup`"),
            "duplicate-label detection should fire for branches; got: {err}");
    }

    #[test] fn loop_branch_pipeline_can_share_labels() {
        // Different BlockKinds → disjoint namespaces.  `loop "foo"`
        // + `branch "foo"` + `pipeline "foo"` all coexist.
        check_src(&composition_body(r#"
          loop "shared" {
            body      = pipeline.body
            condition = "{{ x }}"
            max_iter  = 1
          }
          branch "shared" {
            on      = "{{ x }}"
            cases   = { ok = pipeline.yes }
            default = pipeline.fallback
          }
          pipeline "shared" { steps = [agent.a] }
        "#)).expect(
            "loop + branch + pipeline sharing a label must be allowed \
             (different BlockKinds)");
    }

    // ---- skill block (v0.3 Week 18 Wed) ------------------------- //

    #[test] fn skill_with_shell_exec_passes() {
        // Shell-exec skills carry no Ref → nothing for the checker
        // to resolve.  Validate the block lands in the graph + the
        // checker is happy.
        check_src(&composition_body(r#"
          skill "list_files" {
            exec        = "git ls-files"
            output_type = list_string
          }
        "#)).expect("shell-exec skill must check cleanly");
    }

    #[test] fn skill_with_mcp_ref_to_existing_server_passes() {
        // The composition_body fixture has no mcp_server; declare
        // one inline.  Skill's `mcp = mcp_server.docs.tool.<name>`
        // ref must resolve against it.
        check_src(&composition_body(r#"
          mcp_server "docs" { image = "ghcr.io/x/y:1.0" mount = "./k/" }
          skill "read_doc" {
            mcp         = mcp_server.docs.tool.read_text_file
            output_type = string
          }
        "#)).expect("skill referencing a declared mcp_server must check");
    }

    #[test] fn skill_with_mcp_ref_to_undeclared_server_fails() {
        // Mirror of `branch_default_ref_to_undeclared_pipeline_fails`
        // but for the skill→mcp_server edge.  Diagnostic must name
        // both the source skill and the missing mcp_server label.
        let err = check_src(&composition_body(r#"
          skill "broken" {
            mcp         = mcp_server.ghost.tool.anything
            output_type = string
          }
        "#)).unwrap_err();
        assert!(err.contains("skill `broken`"),
            "error must name the source skill; got: {err}");
        assert!(err.contains("mcp.server"),
            "error must name the failing field; got: {err}");
        assert!(err.contains("undeclared mcp_server `ghost`"),
            "error must name the missing label + kind; got: {err}");
    }

    #[test] fn duplicate_skill_label_rejected() {
        let err = check_src(&composition_body(r#"
          skill "dup" {
            exec        = "ls"
            output_type = string
          }
          skill "dup" {
            exec        = "pwd"
            output_type = string
          }
        "#)).unwrap_err();
        assert!(err.contains("duplicate `skill` block label `dup`"),
            "duplicate-label detection should fire for skills; got: {err}");
    }

    #[test] fn skill_label_can_collide_with_other_block_kinds() {
        // Skills are their own BlockKind → label `foo` is independent
        // from `pipeline foo` / `agent foo` / `mcp_server foo`.
        check_src(&composition_body(r#"
          mcp_server "shared" { image = "ghcr.io/x/y:1.0" mount = "./k/" }
          skill "shared" {
            exec        = "ls"
            output_type = string
          }
          pipeline "shared" { steps = [agent.a] }
        "#)).expect(
            "skill + pipeline + mcp_server sharing a label must be allowed");
    }

    // ---- vector_store block (v0.3 Week 19 Mon) ------------------ //

    #[test] fn vector_store_with_undeclared_embedding_ref_fails() {
        // Tue lands the `embedding "<label>" { … }` block; Mon's
        // checker treats vector_store.embedding refs as already
        // resolvable through the existing graph machinery, so an
        // unknown embedding label correctly errors as "undeclared
        // embedding `X`".  Once Tue lands the block, declared
        // embeddings will resolve normally — no checker code change
        // needed.
        let err = check_src(&composition_body(r#"
          vector_store "docs" {
            embedding     = embedding.ghost
            source        = "./k/"
            chunk_size    = 1k
            chunk_overlap = 128
          }
        "#)).unwrap_err();
        assert!(err.contains("vector_store `docs`"),
            "error must name the source vector_store; got: {err}");
        assert!(err.contains("embedding"),
            "error must name the failing field; got: {err}");
        assert!(err.contains("undeclared embedding `ghost`"),
            "error must name the missing label + kind; got: {err}");
    }

    #[test] fn vector_store_with_overlap_ge_chunk_size_fails() {
        // Parser allows it (chunk_overlap is just a Size, no
        // independent range check); checker catches the semantic
        // invariant that overlap < chunk_size (zero-progress
        // chunker can't index).
        let err = check_src(&composition_body(r#"
          vector_store "bad" {
            embedding     = embedding.e
            source        = "./k/"
            chunk_size    = 1k
            chunk_overlap = 1k
          }
        "#)).unwrap_err();
        assert!(err.contains("vector_store `bad`"),
            "error must name the vector_store; got: {err}");
        assert!(err.contains("chunk_overlap")
                && err.contains("chunk_size"),
            "error must name both fields; got: {err}");
        assert!(err.contains("strictly less"),
            "error must explain the invariant; got: {err}");
    }

    #[test] fn duplicate_vector_store_label_rejected() {
        let err = check_src(&composition_body(r#"
          vector_store "dup" {
            embedding  = embedding.e
            source     = "./k/"
            chunk_size = 1k
          }
          vector_store "dup" {
            embedding  = embedding.e
            source     = "./k2/"
            chunk_size = 1k
          }
        "#)).unwrap_err();
        assert!(err.contains("duplicate `vector_store` block label `dup`"),
            "duplicate-label detection must fire; got: {err}");
    }

    #[test] fn vector_store_label_can_collide_with_other_block_kinds() {
        // VectorStore is its own BlockKind — independent namespace
        // from pipeline / agent / etc.  The reference here is
        // expected to fail (no embedding block yet) so this test
        // asserts the DUPLICATE-LABEL machinery accepts the
        // shared label by inspecting the diagnostic — if duplicates
        // were the problem we'd see that error; instead we see the
        // unrelated embedding-ref error.
        let err = check_src(&composition_body(r#"
          pipeline "shared" { steps = [agent.a] }
          vector_store "shared" {
            embedding  = embedding.e
            source     = "./k/"
            chunk_size = 1k
          }
        "#)).unwrap_err();
        assert!(!err.contains("duplicate"),
            "shared labels across BlockKinds must NOT trigger \
             duplicate-label; got: {err}");
        assert!(err.contains("undeclared embedding"),
            "should fail only on the embedding ref, not on the \
             shared label; got: {err}");
    }
}
