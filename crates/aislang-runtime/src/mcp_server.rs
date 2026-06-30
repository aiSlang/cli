//! aiSlang MCP server — `aislang mcp-serve`.
//!
//! Week-12 Mon **scaffolding**: registers a single
//! [`aislang_ping`](AislangServer::ping) tool that returns the
//! string `"pong"`.  Exists to validate the rmcp-server
//! registration + stdio-transport pipeline on a trivial case
//! before scaling to the 9 substantive tool families (Tue: pure
//! tier; Wed: stateful tier; Thu: conversation tier) per
//! PLAN-90-days-v0_2.md Phase 1 Week 12.
//!
//! ## Why server-side lives in `aislang-runtime`
//!
//! v0.1 already uses this crate for the MCP **client** side
//! ([`crate::mcp::StdioChild`] — spawn an MCP server image as a
//! `docker run -i` child and call its tools).  Server-side scaffolding
//! reuses the same `rmcp` dep with additional features (`server` +
//! `transport-io` + `macros`).
//!
//! ## Transport posture (Decision 39)
//!
//! Stdio for Week 12.  HTTP/SSE for Week 13 Mon — same crate,
//! additional transport binding.  Both consume the SAME
//! [`AislangServer`] instance; only the wire integration changes.
//! This is the "proxy-via-MCP" architectural simplification
//! locked in the v0.2 planning doc (Track A architectural
//! decision #3): one server implementation, multiple deployment
//! shapes.
//!
//! ## Tool-annotation policy (Decision 40)
//!
//! For Mon's `aislang_ping`: no `destructive_hint`, no
//! parameters.  The destructive-hint pattern arrives Wed
//! alongside `aislang_apply` + `aislang_destroy` so the
//! confirmation-dialog UX in Claude Desktop / Cursor is exercised
//! on a tool that genuinely matters.

use rmcp::{
    ErrorData as RmcpError, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Content,
        Implementation, Meta, ProgressNotificationParam,
        ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::{Session, SessionProgress};

// ============================================================================
// Errors
// ============================================================================

/// Failure modes surfaced by [`serve_stdio`].
#[derive(Debug, Error)]
pub enum ServerError {
    /// rmcp's initial-handshake / capability-exchange failed.  Almost
    /// always indicates the connecting client speaks a wire format
    /// we don't.  Rare in practice (rmcp negotiates).
    #[error("rmcp service init failed: {0}")]
    Init(String),
    /// Runtime error during the long-running serve loop (EOF on
    /// stdin counts as a clean shutdown, not an error).
    #[error("rmcp service runtime error: {0}")]
    Runtime(String),
}

// ============================================================================
// Server
// ============================================================================

/// aiSlang's MCP server handler.
///
/// **Week-12 Mon scope**: stateless; the only tool is
/// [`aislang_ping`](Self::ping).  Tue–Thu add the substantive
/// tool surface (lang_spec / examples / describe / validate /
/// plan / apply / destroy / status / chat_turn / eval).  When
/// the chat_turn / eval tools land, this struct grows fields
/// for the Session / store handles — at which point we'll
/// migrate to `Arc<RwLock<…>>` for cross-tool state sharing
/// per rmcp's `Service` semantics.
///
/// The `tool_router` field is built by the `#[tool_router]` macro
/// from the `#[tool]`-annotated methods below; we never construct
/// it directly.
/// Identifies one conversational session.  v0.1's `Session` is bound
/// to (source_dir, agent_label) — same .ais file + same agent label
/// → same Session.  Week-12 Thu's MCP layer caches sessions by this
/// key in `AislangServer.sessions` so each `aislang_chat_turn` call
/// reuses the open LiteLLM connection + MCP children rather than
/// re-spawning them per turn.
///
/// Future v0.3+ may grow this with an explicit `conversation_id`
/// field for client-supplied identity (Claude Desktop conversation
/// ID, etc.) — for now we assume one conversation per (dir, agent).
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct SessionKey {
    source_dir:  PathBuf,
    agent_label: String,
}

#[derive(Clone)]
pub struct AislangServer {
    // Read indirectly by the `#[tool_handler]` macro's generated
    // `call_tool` / `list_tools` dispatcher.  Dead-code analysis
    // doesn't see through the macro, so we explicitly allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,

    // Week-12 Thu: session pool for `aislang_chat_turn`.
    //
    // Keyed by (source_dir, agent_label).  First call opens the
    // Session (validate source → find agent → resolve model/prompt
    // → spawn MCP children → open LiteLLM client → optional budget
    // tracker); subsequent calls reuse the cached Arc<Session>.
    //
    // Lifetime: as long as the mcp-serve process lives.  When the
    // host client (Claude Desktop, Cursor) shuts down the stdio
    // transport, mcp-serve exits and Drop runs through every
    // Session — including the MCP children which get killed cleanly.
    //
    // Outer Arc<RwLock<…>> lets multiple chat_turn invocations
    // share the cache safely; the inner Arc<Session> lets the
    // turn run after the map lock is released (the call to
    // `Session::turn` itself takes &self so multiple in-flight
    // turns against the same session don't need exclusion at this
    // layer — Session's internals handle their own state).
    sessions: Arc<RwLock<HashMap<SessionKey, Arc<Session>>>>,
}

// Hand-rolled Debug so AislangServer stays debuggable without
// requiring Session: Debug (which v0.1 doesn't impl because of the
// nested rmcp + reqwest client types).
impl std::fmt::Debug for AislangServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AislangServer")
            .field("tool_router", &"<ToolRouter>")
            .field("sessions", &"<RwLock<HashMap<…>>>")
            .finish()
    }
}

impl Default for AislangServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl AislangServer {
    /// Construct a fresh server.  Cheap — just builds the rmcp
    /// tool-routing table from the `#[tool]` annotations in this
    /// `impl` block.  The session cache starts empty; sessions are
    /// created lazily on first `aislang_chat_turn` call against a
    /// given (source_dir, agent_label) key.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            sessions:    Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Health check.  Returns `"pong"`.  Idempotent, side-effect-free.
    ///
    /// Useful for verifying:
    ///   - The MCP server is reachable from the client's transport.
    ///   - rmcp's handshake + tool registration succeeded.
    ///   - Tool listing renders in the host UI.
    ///
    /// Zero parameters — rmcp's `#[tool]` macro emits the JSON
    /// schema accordingly (host clients render this as a "no input"
    /// tool).
    #[tool(
        name = "aislang_ping",
        description = "Health check — returns 'pong'. \
            Verifies the aiSlang MCP server is reachable and the \
            tool-dispatch pipeline is functional. No side effects."
    )]
    pub async fn ping(&self) -> String {
        "pong".to_string()
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Tue: pure tier — read-only tools that wire to existing
    // aislang-core / aislang-resolver / aislang-compose internals.
    // No new runtime state introduced.  Order chosen to match the
    // canonical "learn → describe → validate → plan" authoring loop
    // an MCP-aware LLM will follow.
    // ────────────────────────────────────────────────────────────────

    /// Returns the aiSlang language specification (the contents of
    /// `LANGUAGE-SPEC-v0.md` bundled at compile time via
    /// `include_str!`).  Use this to ground the LLM in aiSlang grammar
    /// before generating `.ais` source.
    ///
    /// The full spec is ~25 KB — small enough to return in a single
    /// MCP response without pagination.  v0.3+ may add section
    /// filtering if the spec grows beyond ~100 KB.
    #[tool(
        name = "aislang_lang_spec",
        description = "Returns the aiSlang language specification \
            (LANGUAGE-SPEC-v0.md).  Read this before generating `.ais` \
            source — it documents the grammar, type system, and all \
            resource blocks (project, budget, model, prompt, mcp_server, \
            agent, eval, secret, deploy, and the v0.2 additions main + \
            principal patterns)."
    )]
    pub async fn lang_spec(&self) -> Result<CallToolResult, RmcpError> {
        const SPEC: &str = include_str!("../../../LANGUAGE-SPEC-v0.md");
        Ok(CallToolResult::success(vec![Content::text(SPEC.to_string())]))
    }

    /// Returns one of the canonical example `.ais` files, or an index
    /// of available examples when `name` is omitted.  All examples are
    /// bundled at compile time via `include_str!`.
    ///
    /// v0.1 ships four examples:
    /// - `minimal` — smallest valid project (1 model + 1 agent).
    /// - `support-agent` — cost-optimized RAG with paid Anthropic/OpenAI
    ///   + Ollama fallback.
    /// - `support-agent-paid` — same demo, paid-Haiku-primary variant.
    /// - `free-local` — Ollama-only, $0 budget cap.
    ///
    /// The LLM should prefer reading the example *closest* to the
    /// user's described use case before drafting fresh `.ais`.
    #[tool(
        name = "aislang_examples",
        description = "Returns canonical `.ais` example files.  Call \
            with name=<example_name> to fetch one example, or with no \
            arguments to list available examples.  Available: minimal, \
            support-agent, support-agent-paid, free-local."
    )]
    pub async fn examples(&self,
        Parameters(p): Parameters<ExamplesParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        const MINIMAL:            &str = include_str!("../../../examples/minimal.ais");
        const SUPPORT_AGENT:      &str = include_str!("../../../examples/support-agent.ais");
        const SUPPORT_AGENT_PAID: &str = include_str!("../../../examples/support-agent-paid.ais");
        const FREE_LOCAL:         &str = include_str!("../../../examples/free-local.ais");

        let body = match p.name.as_deref() {
            None => {
                "Available examples (call again with name=<name>):\n\
                 \n\
                 - minimal\n\
                     Smallest valid project — one model + one agent. \
                     Useful for grammar smoke tests.\n\
                 \n\
                 - support-agent\n\
                     Cost-optimized agentic RAG demo. \
                     Resolves to Ollama (free) under cost-opt; Anthropic + \
                     OpenAI as fallback chain.\n\
                 \n\
                 - support-agent-paid\n\
                     Same demo, paid-Haiku as primary. \
                     Used for real-API integration tests.\n\
                 \n\
                 - free-local\n\
                     Ollama-only, $0 budget. \
                     The 'no API keys required' demo for new users."
                    .to_string()
            }
            Some("minimal")            => MINIMAL.to_string(),
            Some("support-agent")      => SUPPORT_AGENT.to_string(),
            Some("support-agent-paid") => SUPPORT_AGENT_PAID.to_string(),
            Some("free-local")         => FREE_LOCAL.to_string(),
            Some(other) => {
                return Err(RmcpError::invalid_params(
                    format!("unknown example `{other}`; available: \
                             minimal, support-agent, support-agent-paid, free-local"),
                    None,
                ));
            }
        };
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }

    /// Parse + type-check `.ais` source and return a structured JSON
    /// summary of the project's topology — useful for the LLM to
    /// reason about an existing or candidate `.ais` file without
    /// re-parsing it itself.
    ///
    /// Returns the full typed `Project` AST as JSON.  Includes the
    /// project name, every model / agent / prompt / mcp_server / eval
    /// / secret / deploy block, plus their resolved field values.
    ///
    /// Errors on a parse failure (use `aislang_validate` if you want
    /// human-readable diagnostics instead).
    #[tool(
        name = "aislang_describe",
        description = "Parse `.ais` source and return its typed AST as \
            JSON. Use this to inspect the structure of an existing or \
            candidate file — every resource block becomes a JSON object \
            with its declared fields. For source-aware diagnostics on a \
            broken file, use `aislang_validate` instead."
    )]
    pub async fn describe(&self,
        Parameters(p): Parameters<SourceOnlyParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        // SourceOnlyParameters has no `source_dir` field — relative
        // `source_path` resolves against the MCP server's cwd.
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = resolve_source(p.source, p.source_path, &cwd)?;

        let (project, _warnings) = aislang_core::validate(&source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;

        let json = serde_json::to_string_pretty(&project)
            .map_err(|e| RmcpError::internal_error(
                format!("serialize project: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    /// Validate `.ais` source: parse + type-check + cross-resource ref
    /// resolution + catalog ID check.  Returns either a success message
    /// (with resource counts + any warnings) or a source-aware
    /// diagnostic string — the same rendering the CLI's `aislang
    /// validate` produces (line:col + 2-3-line snippet + caret).
    ///
    /// Unlike `aislang_describe`, errors render *as text* in the
    /// successful tool response rather than as MCP-level errors.  That
    /// lets the LLM read the diagnostic message and iterate on the
    /// source without losing the conversation context to an error-path
    /// branch.  Distinguish success from failure by looking for `"OK:"`
    /// at the start of the response body, or for `error at line` in the
    /// failure case.
    #[tool(
        name = "aislang_validate",
        description = "Parse + type-check + catalog-id-check `.ais` \
            source.  Returns either a success summary (`OK: …`) or a \
            source-aware diagnostic (`error at line N, column M: …` \
            with a 2-3-line snippet + caret).  No side effects; safe to \
            call iteratively while drafting source."
    )]
    pub async fn validate(&self,
        Parameters(p): Parameters<SourceOnlyParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let source = resolve_source(p.source, p.source_path, &cwd)?;

        let body = match aislang_core::validate(&source) {
            Ok((project, mut warnings)) => {
                // Run the catalog check too — same as `aislang validate`
                // in the CLI surface.
                let cat_warnings = match aislang_resolver::check_against_bundled_catalog(&project) {
                    Ok(w)  => w,
                    Err(e) => return Ok(CallToolResult::success(vec![
                        Content::text(format!("catalog error: {e}"))])),
                };
                warnings.extend(cat_warnings);

                let mut out = format!(
                    "OK: parsed and type-checked\n\
                     \n\
                     project:     {}\n\
                     models:      {}\n\
                     prompts:     {}\n\
                     agents:      {}\n\
                     mcp_servers: {}\n\
                     evals:       {}\n\
                     secrets:     {}\n\
                     mains:       {}",
                    project.name,
                    project.models.len(),
                    project.prompts.len(),
                    project.agents.len(),
                    project.mcp_servers.len(),
                    project.evals.len(),
                    project.secrets.len(),
                    project.mains.len(),
                );
                if !warnings.is_empty() {
                    out.push_str("\n\nwarnings:");
                    for w in &warnings {
                        out.push_str(&format!("\n  - {}", w.message));
                    }
                }
                out
            }
            Err(err) => render_validate_error_to_string(&source, &err),
        };
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }

    /// Run the full resolve + emit pipeline against `.ais` source:
    /// pick a concrete `ModelId` per `model` block, hash prompt files,
    /// emit Compose YAML + LiteLLM config + .env.example **in
    /// memory** (no disk writes — keeps the tool side-effect-free).
    ///
    /// `source_dir` is the directory that `prompt.file` and
    /// `mcp_server.mount` paths are resolved against.  Defaults to the
    /// current working directory if unset; supply an absolute path to
    /// avoid surprises.  Prompt files referenced by the source MUST
    /// exist at the resolved paths or this tool returns a resolve
    /// error — materialize them first via a separate `Write` action.
    ///
    /// Returns the **resolved IR (JSON)** + the **emitted Compose
    /// YAML** concatenated in a single text response with section
    /// markers, so the LLM can inspect both the resolution choices
    /// and the final stack shape in one round-trip.
    #[tool(
        name = "aislang_plan",
        description = "Resolve `.ais` source against the bundled model \
            catalog, then emit Compose YAML + LiteLLM config in memory \
            (no disk writes).  Returns the resolved IR (JSON) + the \
            Compose YAML in one response.  Set `source_dir` to the \
            directory containing prompt files referenced by the source; \
            those files must already exist on disk for resolve to \
            succeed (hash-pinning).  For files-not-yet-materialized \
            flows, write the prompt files first then call this tool."
    )]
    pub async fn plan(&self,
        Parameters(p): Parameters<PlanParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        // Stage 0: resolve source_dir (with week-13 Fri follow-up
        // inference from absolute source_path) + source content.
        let source_dir = resolve_source_dir_or_infer_from_path(
            p.source_dir.as_deref(),
            p.source_path.as_deref(),
        )?;
        let source = resolve_source(p.source, p.source_path, &source_dir)?;

        // Stage 1: parse + typecheck.
        let (project, _warnings) = aislang_core::validate(&source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;

        // Stage 2: catalog ID check.
        aislang_resolver::check_against_bundled_catalog(&project)
            .map_err(|e| RmcpError::invalid_params(
                format!("catalog error: {e}"), None))?;

        // Stage 3: resolve.  source_dir was bound above so prompt
        // hash-pinning + mcp_server.mount path resolution work the
        // same way as the CLI's `aislang plan` without --output-dir.
        let resolved = aislang_resolver::resolve_against_bundled_catalog(
            &project, &source_dir,
        ).map_err(|e| RmcpError::invalid_params(
            format!("resolve error: {e}"), None))?;

        // Stage 4: emit.  In-memory only — Mon DoD pins "no new runtime
        // state", so we DON'T write litellm_config.yaml / .env.example /
        // aislang.lock to disk.  v0.2 Wed's `aislang_apply` is the
        // stateful complement that materialises these.
        let catalog = aislang_catalog::bundled()
            .map_err(|e| RmcpError::internal_error(
                format!("catalog load: {e}"), None))?;
        let artifacts = aislang_compose::emit(&resolved, &catalog, &source_dir)
            .map_err(|e| RmcpError::invalid_params(
                format!("compose emit: {e}"), None))?;

        let resolved_json = serde_json::to_string_pretty(&resolved)
            .map_err(|e| RmcpError::internal_error(
                format!("serialize resolved: {e}"), None))?;

        let body = format!(
            "── Resolved IR (JSON) ──\n\
             {}\n\
             \n\
             ── docker-compose.yml ──\n\
             {}\n\
             ── litellm_config.yaml ──\n\
             {}\n\
             ── .env.example ──\n\
             {}",
            resolved_json,
            artifacts.docker_compose,
            artifacts.litellm_config,
            artifacts.env_example,
        );
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Wed: stateful tier — apply / destroy / status.  These
    // are the first MCP tools that touch the filesystem + Docker.
    // Apply + destroy carry `destructive_hint = true` annotations so
    // compliant MCP clients (Claude Desktop, Cursor) gate them
    // behind a confirmation dialog before invocation (planning-doc
    // Track A architectural decision #5 — "PreToolUse via MCP
    // annotations").
    //
    // All three tools mirror the v0.1 CLI subcommands' logic
    // (`run_apply` / `run_destroy` / `run_status` in main.rs) but
    // accumulate their human-readable output into a String body
    // instead of writing to stderr.  We don't refactor the CLI
    // functions — the duplication is small, the surfaces have
    // genuinely different needs (CLI streams to stderr live; MCP
    // returns a single string when the operation completes), and
    // each can evolve independently without coupling.
    //
    // Long-running ops note: `aislang_apply` invokes `docker compose
    // up -d` which can take 30s+ on first image pull.  MCP's tool-
    // call shape is single-shot (no streaming) in v0.2; the client
    // sees a "Claude is thinking…" spinner for the duration.  v0.3
    // may add progress notifications via rmcp's progress API.
    // ────────────────────────────────────────────────────────────────

    /// Deploy the stack: resolve + emit artifacts to disk, then
    /// `docker compose -f stack.yml up -d`, then record state.
    /// Returns a single human-readable summary string capturing every
    /// generated file path + the docker outcome.
    ///
    /// **Destructive.** Writes `stack.yml`, `litellm_config.yaml`,
    /// `.env.example`, `aislang.lock` to `source_dir`; starts Docker
    /// containers; writes `.aislang/state.json` after the stack
    /// confirms up.  Compliant MCP clients gate this behind a
    /// confirmation prompt before invoking — see `destructive_hint`
    /// annotation below.
    ///
    /// Graceful degrade: if `docker` isn't on PATH, the artifacts are
    /// still written and the response body tells the user how to
    /// bring the stack up manually.  This matches the v0.1 CLI's
    /// `apply` behaviour for the same case.
    #[tool(
        name = "aislang_apply",
        description = "Deploy the aiSlang stack to Docker: resolve + \
            emit artifacts to disk, then `docker compose up -d`, then \
            record .aislang/state.json. Writes 5 files (stack.yml, \
            litellm_config.yaml, .env.example, aislang.lock, state.json) \
            into source_dir. Starts Docker containers — destructive. \
            On first run, image pulls can take 30-60s; the response \
            comes back when `docker compose up -d` returns. \
            \n\nCALLING CONVENTION: pass `source_path` (filesystem path \
            to the .ais file), NOT `source` (inline body).  The host \
            renders this destructive-hint confirmation dialog from the \
            raw request JSON; inline `source` produces a multi-line \
            wall of escaped text the user can't scan before approving.",
        annotations(
            title             = "Deploy aiSlang stack",
            destructive_hint  = true,
            idempotent_hint   = false,
            open_world_hint   = true,
            read_only_hint    = false,
        )
    )]
    pub async fn apply(&self,
        Parameters(p): Parameters<ApplyParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        let mut out = String::new();
        // Week 13 Fri follow-up: infer source_dir from absolute
        // source_path when no source_dir was supplied — common case
        // for MCP hosts (Claude Desktop, Cursor) that omit source_dir
        // and expect aislang to figure it out from the .ais file path.
        let source_dir = resolve_source_dir_or_infer_from_path(
            p.source_dir.as_deref(),
            p.source_path.as_deref(),
        )?;
        let source     = resolve_source(p.source, p.source_path, &source_dir)?;

        // ── Stage 1: full plan pipeline (validate → catalog → resolve → emit)
        let (project, _warnings) = aislang_core::validate(&source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;
        aislang_resolver::check_against_bundled_catalog(&project)
            .map_err(|e| RmcpError::invalid_params(
                format!("catalog error: {e}"), None))?;
        let resolved = aislang_resolver::resolve_against_bundled_catalog(
            &project, &source_dir,
        ).map_err(|e| RmcpError::invalid_params(
            format!("resolve error: {e}"), None))?;
        let catalog = aislang_catalog::bundled()
            .map_err(|e| RmcpError::internal_error(
                format!("catalog load: {e}"), None))?;
        let artifacts = aislang_compose::emit(&resolved, &catalog, &source_dir)
            .map_err(|e| RmcpError::invalid_params(
                format!("compose emit: {e}"), None))?;

        // ── Stage 2: write artifacts.  All four files use the same
        // location convention v0.1's `aislang plan -o <dir>` uses.
        let stack_path   = source_dir.join(aislang_compose::STACK_YAML_FILENAME);
        let litellm_path = source_dir.join(aislang_compose::LITELLM_CONFIG_FILENAME);
        let env_path     = source_dir.join(aislang_compose::ENV_EXAMPLE_FILENAME);
        let lock_path    = source_dir.join("aislang.lock");

        std::fs::write(&stack_path, &artifacts.docker_compose)
            .map_err(|e| RmcpError::internal_error(
                format!("write {}: {e}", stack_path.display()), None))?;
        std::fs::write(&litellm_path, &artifacts.litellm_config)
            .map_err(|e| RmcpError::internal_error(
                format!("write {}: {e}", litellm_path.display()), None))?;
        std::fs::write(&env_path, &artifacts.env_example)
            .map_err(|e| RmcpError::internal_error(
                format!("write {}: {e}", env_path.display()), None))?;
        // Lockfile via the resolver's helper so format matches v0.1
        // `aislang plan` exactly (byte-stable reproducibility).
        let lock = aislang_resolver::Lockfile::build(&source, &resolved);
        aislang_resolver::lockfile::write(&lock_path, &lock)
            .map_err(|e| RmcpError::internal_error(
                format!("write {}: {e}", lock_path.display()), None))?;

        use std::fmt::Write;
        let _ = writeln!(out, "generated:");
        let _ = writeln!(out, "  stack:           {}", stack_path.display());
        let _ = writeln!(out, "  litellm config:  {}", litellm_path.display());
        let _ = writeln!(out, "  env example:     {}", env_path.display());
        let _ = writeln!(out, "  lockfile:        {}", lock_path.display());

        // ── Stage 3: docker compose up -d (with graceful degrade).
        if !docker_on_path() {
            let _ = writeln!(out);
            let _ = writeln!(out, "docker not on PATH — artifacts written but stack NOT started.");
            let _ = writeln!(out, "install Docker and run:");
            let _ = writeln!(out, "  docker compose -f {} up -d", stack_path.display());
            return Ok(CallToolResult::success(vec![Content::text(out)]));
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "bringing stack up: docker compose -f {} up -d",
                         stack_path.display());
        let status = std::process::Command::new("docker")
            .args(["compose", "-f"])
            .arg(&stack_path)
            .args(["up", "-d"])
            // Swallow docker's stdout (compose logs) so it doesn't
            // pollute our return body; stderr is fine to drop too
            // since we report the exit status explicitly.
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| RmcpError::internal_error(
                format!("invoke docker compose: {e}"), None))?;
        if !status.success() {
            let _ = writeln!(out);
            let _ = writeln!(out, "docker compose up -d exited {:?}", status.code());
            let _ = writeln!(out, "(stack.yml + lockfile + .env.example are still on disk; \
                                   inspect with `docker compose -f {} logs`)",
                             stack_path.display());
            return Ok(CallToolResult::success(vec![Content::text(out)]));
        }

        // ── Stage 4: write state.json (after stack confirms up — same
        // ordering as v0.1's run_apply).  Failure here is a warning,
        // not an error: the stack IS up, we just couldn't persist
        // the audit trail.
        let state_path  = crate::state_path_in(&source_dir);
        let source_hash = crate::hex_sha256(source.as_bytes());
        let state = crate::new_applied_state(
            &project.name,
            source_hash,
            &resolved.catalog_version,
            stack_path.clone(),
        );
        match crate::write_state_atomic(&state_path, &state) {
            Ok(()) => { let _ = writeln!(out, "  state:           {}", state_path.display()); }
            Err(e) => { let _ = writeln!(out,
                "warning: stack is up but failed to write {}: {e}",
                state_path.display()); }
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "stack is up — project `{}` deployed.", project.name);
        let _ = writeln!(out, "next steps:");
        if let Some(agent) = project.agents.first() {
            let _ = writeln!(out, "  chat:    aislang chat {} <source.ais>", agent.label);
        }
        let _ = writeln!(out, "  teardown: call aislang_destroy with source_dir = {}",
                         source_dir.display());

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// Tear down a deployed stack: `docker compose -f stack.yml down
    /// -v`, then flip `applied_at` to null in state.json.  Idempotent
    /// — re-running on an already-torn-down stack returns "nothing to
    /// destroy" in the success body (Decision 27).
    ///
    /// **Destructive.** Removes Docker containers, networks, and
    /// volumes for the stack at `source_dir`.  Compliant MCP clients
    /// gate this behind a confirmation prompt — see annotation.
    ///
    /// `force = true` bypasses the idempotency check, exactly mirroring
    /// the CLI's `--force` flag (Decision 29).
    #[tool(
        name = "aislang_destroy",
        description = "Tear down a deployed aiSlang stack: \
            `docker compose down -v` removes containers, networks, and \
            volumes for the stack at source_dir, then state.json's \
            applied_at field flips to null. Idempotent — calling on an \
            already-torn-down stack returns 'nothing to destroy' as a \
            successful body. Use `force = true` to bypass the idempotency \
            check and run docker compose down -v unconditionally. \
            Destructive.",
        annotations(
            title             = "Tear down aiSlang stack",
            destructive_hint  = true,
            idempotent_hint   = true,
            open_world_hint   = true,
            read_only_hint    = false,
        )
    )]
    pub async fn destroy(&self,
        Parameters(p): Parameters<DestroyParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        let mut out = String::new();
        let source_dir = resolve_source_dir(p.source_dir.as_deref())?;
        let stack_path = source_dir.join(aislang_compose::STACK_YAML_FILENAME);
        let state_path = crate::state_path_in(&source_dir);
        let force      = p.force.unwrap_or(false);

        use std::fmt::Write;

        // ── Idempotency check (Decision 27) ─────────────────────────
        if !force {
            match crate::read_state(&state_path) {
                Ok(Some(state)) if state.applied_at.is_none() => {
                    let _ = writeln!(out,
                        "nothing to destroy: state file at `{}` shows the stack was already torn down.",
                        state_path.display());
                    let _ = writeln!(out,
                        "(set force=true to run `docker compose down -v` against `{}` anyway.)",
                        stack_path.display());
                    return Ok(CallToolResult::success(vec![Content::text(out)]));
                }
                Ok(None) if !stack_path.exists() => {
                    let _ = writeln!(out,
                        "nothing to destroy: neither `{}` nor `{}` exists.",
                        state_path.display(), stack_path.display());
                    return Ok(CallToolResult::success(vec![Content::text(out)]));
                }
                Ok(_) => {}   // proceed.
                Err(e) => {
                    return Err(RmcpError::internal_error(
                        format!("read state file `{}`: {e}", state_path.display()),
                        None));
                }
            }
        }

        // ── Stack.yml required for the actual docker compose down ──
        if !stack_path.exists() {
            return Err(RmcpError::invalid_params(
                format!("no stack.yml at `{}` — call aislang_apply first to recreate it",
                        stack_path.display()), None));
        }
        if !docker_on_path() {
            return Err(RmcpError::internal_error(
                format!("docker not on PATH; cannot tear down stack at `{}`",
                        stack_path.display()), None));
        }

        let _ = writeln!(out, "tearing down stack: docker compose -f {} down -v",
                         stack_path.display());
        let status = std::process::Command::new("docker")
            .args(["compose", "-f"])
            .arg(&stack_path)
            .args(["down", "-v"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| RmcpError::internal_error(
                format!("invoke docker compose: {e}"), None))?;
        if !status.success() {
            return Err(RmcpError::internal_error(
                format!("docker compose down -v exited {:?}", status.code()), None));
        }

        // ── Mark state torn down (Decision 27).  Log-and-continue
        // on failure: the stack IS down; we just couldn't update the
        // audit trail.
        if let Err(e) = crate::mark_state_torn_down(&state_path) {
            let _ = writeln!(out,
                "warning: stack torn down but failed to update state file `{}`: {e}",
                state_path.display());
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "stack torn down — containers, networks, and volumes removed.");
        let _ = writeln!(out, "stack.yml is preserved at `{}` (remove manually if desired).",
                         stack_path.display());
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    /// Render the same three sections the v0.1 `aislang status` CLI
    /// surfaces: stack (state.json + `docker compose ps`), optional
    /// today's spend total, optional by-model spend breakdown.
    /// Always returns a success body — drift is reported in-text, not
    /// as an MCP error, so the LLM can read the diagnostic and act.
    ///
    /// Read-only.  No destructive_hint, no side effects.
    #[tool(
        name = "aislang_status",
        description = "Render stack health (.aislang/state.json + \
            `docker compose ps` roll-up), optionally with today's spend \
            total and/or by-model breakdown. Read-only; reports drift \
            (state missing, applied_at null, services not up) in-text. \
            For the CLI exit-code spectrum (0 clean / 1 drift / 2 \
            docker-unavailable) use `aislang status` from a terminal — \
            MCP returns the same information as a single text body.",
        annotations(
            title             = "Inspect aiSlang stack status",
            destructive_hint  = false,
            read_only_hint    = true,
            open_world_hint   = true,
            idempotent_hint   = true,
        )
    )]
    pub async fn status(&self,
        Parameters(p): Parameters<StatusParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        use std::fmt::Write;
        let mut out = String::new();

        let source_dir = resolve_source_dir_or_infer_from_path(
            p.source_dir.as_deref(),
            p.source_path.as_deref(),
        )?;
        let source     = resolve_source(p.source, p.source_path, &source_dir)?;

        let (project, _warnings) = aislang_core::validate(&source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;
        let state_path = crate::state_path_in(&source_dir);
        let stack_path = source_dir.join(aislang_compose::STACK_YAML_FILENAME);

        let _ = writeln!(out, "project:  {}", project.name);
        let _ = writeln!(out, "source_dir: {}", source_dir.display());
        let _ = writeln!(out);
        let _ = writeln!(out, "stack:");

        match crate::read_state(&state_path) {
            Ok(Some(state)) => match state.applied_at {
                Some(ref ts) => {
                    let _ = writeln!(out, "  applied_at:      {ts}");
                    let _ = writeln!(out, "  catalog_version: {}", state.catalog_version);
                    let _ = writeln!(out, "  source_hash:     {}…",
                        &state.source_hash[..12.min(state.source_hash.len())]);
                    let _ = writeln!(out, "  stack.yml:       {}", state.stack_yaml_path.display());
                    // Docker check is best-effort — failures surface
                    // in-text, never bubble as MCP errors.
                    if stack_path.exists() && docker_on_path() {
                        match docker_compose_ps_lines(&stack_path) {
                            Ok(lines) if lines.is_empty() => {
                                let _ = writeln!(out, "  services: (none running — compose ps returned empty)");
                            }
                            Ok(lines) => {
                                let _ = writeln!(out, "  services:");
                                for line in lines {
                                    let _ = writeln!(out, "    {line}");
                                }
                            }
                            Err(e) => {
                                let _ = writeln!(out, "  services: (failed to query docker: {e})");
                            }
                        }
                    } else if !docker_on_path() {
                        let _ = writeln!(out, "  services: (docker not on PATH — cannot query live status)");
                    } else {
                        let _ = writeln!(out, "  services: (stack.yml missing at `{}`)",
                                         stack_path.display());
                    }
                }
                None => {
                    let _ = writeln!(out, "  status:  torn down (applied_at: null)");
                    let _ = writeln!(out, "  catalog_version: {}", state.catalog_version);
                    let _ = writeln!(out, "  hint:    call aislang_apply to redeploy");
                }
            }
            Ok(None) => {
                let _ = writeln!(out, "  status:  not applied (no state file at `{}`)",
                                 state_path.display());
                let _ = writeln!(out, "  hint:    call aislang_apply to deploy the stack");
            }
            Err(e) => {
                let _ = writeln!(out, "  status:  ERROR reading state file: {e}");
            }
        }

        // ── Optional spend total ───────────────────────────────────
        if p.spend.unwrap_or(false) {
            let tracker = crate::SpendTracker::open_default(&project.name)
                .map_err(|e| RmcpError::internal_error(
                    format!("open spend tracker: {e}"), None))?;
            let _ = writeln!(out);
            let _ = writeln!(out, "spend ({} UTC):",
                crate::spend::utc_today());
            let total = tracker.today_spend();
            let _ = writeln!(out, "  total:   {}",
                crate::format_money(&total));
            if let Some(b) = project.budget.as_ref() {
                if let (Some(cap_mu), Some(spent_mu)) =
                    (b.hard_cap_per_day.micro_units(), total.micro_units())
                {
                    if cap_mu > 0 {
                        let pct = (spent_mu as f64 / cap_mu as f64) * 100.0;
                        let _ = writeln!(out, "  cap:     {} ({:.1}% used)",
                            crate::format_money(&b.hard_cap_per_day),
                            pct);
                    }
                }
            }
        }

        // ── Optional by-model spend ─────────────────────────────────
        if let Some(ref filter) = p.model_spend {
            let tracker = crate::SpendTracker::open_default(&project.name)
                .map_err(|e| RmcpError::internal_error(
                    format!("open spend tracker: {e}"), None))?;
            let by_model = tracker.today_by_model();
            let _ = writeln!(out);
            if filter.is_empty() {
                let _ = writeln!(out, "spend by model ({} UTC):",
                    crate::spend::utc_today());
            } else {
                let _ = writeln!(out, "spend for model `{}` ({} UTC):",
                    filter, crate::spend::utc_today());
            }
            if by_model.is_empty() {
                let _ = writeln!(out, "  (no spend recorded today)");
            } else if filter.is_empty() {
                let mut rows: Vec<_> = by_model.iter().collect();
                rows.sort_by(|a, b| b.1.micro_units().cmp(&a.1.micro_units()));
                let pad = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
                for (model, amount) in rows {
                    let _ = writeln!(out, "  {model:<pad$}  {}",
                        crate::format_money(amount));
                }
            } else {
                match by_model.get(filter) {
                    Some(amount) => {
                        let _ = writeln!(out, "  {}",
                            crate::format_money(amount));
                    }
                    None => {
                        let _ = writeln!(out, "  (no spend recorded for `{filter}` today)");
                    }
                }
            }
        }

        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Thu: conversation tier — chat_turn + eval.
    //
    // The architectural payoff of v0.2 Track A decision #3
    // ("proxy-via-MCP"): the v0.1 Session that previously lived
    // inside `aislang chat`'s in-process loop now lives inside
    // `aislang mcp-serve`.  Same Session implementation; same
    // LiteLLM + MCP-children plumbing; one cache (sessions field on
    // AislangServer) keyed by (source_dir, agent_label) so each
    // (project, agent) conversation gets exactly one Session for
    // the lifetime of the mcp-serve process.
    //
    // Token counts: chat_turn attaches a SpendTracker to the Session
    // when a budget block is declared in the source.  Session::turn
    // (week 7 Wed) calls tracker.add(model_id, cost) after each
    // LiteLLM round-trip, which writes to ~/.aislang/spend.json
    // atomically — same persistence layer v0.1's chat / eval use.
    // ────────────────────────────────────────────────────────────────

    /// Run one turn of conversation against a deployed agent.  First
    /// call for a given (source_dir, agent_label) pair opens a
    /// Session (validates source → finds agent → resolves model +
    /// system_prompt → spawns MCP children → opens LiteLLM client →
    /// attaches optional budget + spend tracker); subsequent calls
    /// reuse the cached Session.
    ///
    /// The Session holds the full conversation context internally
    /// (LiteLLM message history) — every chat_turn call adds to it.
    /// Sessions live for the lifetime of the mcp-serve process; v0.3
    /// may add explicit `aislang_session_close`.
    ///
    /// Not marked destructive: chat_turn doesn't write to disk
    /// directly, but it DOES dispatch LLM calls that consume budget
    /// + emit OTel traces.  Best treated as `open_world_hint = true`
    /// (talks to external services) without confirmation gating.
    #[tool(
        name = "aislang_chat_turn",
        description = "Run one turn of conversation against a deployed \
            agent. Maintains per-(source_dir, agent_label) session state: \
            first call opens the Session (spawns MCP children, opens \
            LiteLLM client, hashes prompt), subsequent calls reuse it. \
            Returns the assistant's reply as text. Tool-use loop is hard-\
            capped at 8 iterations per turn (Decision 4). Budget caps \
            from the .ais source are enforced — over-cap aborts the turn \
            with a `budget exceeded:` synthetic reply. \
            \n\nCALLING CONVENTION: pass `source_path` (filesystem path \
            to the .ais file), NOT `source` (inline body).  The host's \
            tool-call preview renders the request JSON to the user; \
            inline `source` makes that preview a wall of escaped text \
            on every turn.",
        annotations(
            title             = "Send chat turn",
            destructive_hint  = false,
            read_only_hint    = false,
            idempotent_hint   = false,
            open_world_hint   = true,
        )
    )]
    pub async fn chat_turn(&self,
        Parameters(p): Parameters<ChatTurnParameters>,
        // Week 13 Wed: progress streaming.  Both extractors are
        // injected by the rmcp `#[tool]` macro via FromContextPart.
        // The client's `meta._meta.progressToken` (set in the JSON-
        // RPC request envelope) tells us whether to emit progress;
        // `Peer<RoleServer>` is the handle we call notify_progress
        // on.
        meta:   Meta,
        client: Peer<RoleServer>,
    ) -> Result<CallToolResult, RmcpError> {
        // v0.3 Week 17 Tue: dir-input dispatch.  When the caller
        // (CLI's `run_chat`) passes a project directory as
        // `source_path`, source_dir IS that dir and we skip the
        // `resolve_source` read-as-file step entirely (would fail
        // with "Is a directory").  `build_session_for_agent` detects
        // the multi-module shape via aislang.toml presence and
        // routes through `build_session_for_agent_multi_module`,
        // which reads each module via `load_project_input` —
        // the empty `source` string is unused on that path.
        // v0.3 Week 19 Thu: capture the original source_path for the
        // RAG prepend helper before resolve_source consumes p.source_path.
        // Empty-string fallback so load_project_input gets a clear
        // hint when the caller only supplied source_dir.
        let original_source_path_for_rag: PathBuf =
            p.source_path.as_deref().map(PathBuf::from).unwrap_or_default();

        let dir_input: Option<PathBuf> = p.source_path.as_deref()
            .map(std::path::Path::new)
            .filter(|p| p.is_dir())
            .map(|p| p.to_path_buf());
        let (source_dir, source) = if let Some(dir) = dir_input.as_ref() {
            let canonical = std::fs::canonicalize(dir).map_err(|e|
                RmcpError::invalid_params(
                    format!("source_path dir `{}` not readable: {e}",
                        dir.display()),
                    None))?;
            (canonical, String::new())
        } else {
            let sd = resolve_source_dir_or_infer_from_path(
                p.source_dir.as_deref(),
                p.source_path.as_deref(),
            )?;
            let s = resolve_source(p.source, p.source_path, &sd)?;
            (sd, s)
        };
        let key = SessionKey {
            source_dir:  source_dir.clone(),
            agent_label: p.agent_label.clone(),
        };

        // ── Get-or-create the session for this key.
        let session = {
            // Fast path: read lock, hit cache.
            let map = self.sessions.read().await;
            if let Some(s) = map.get(&key) {
                Arc::clone(s)
            } else {
                drop(map);
                // Slow path: write lock + build.  Double-check
                // under the write lock to handle the race where
                // another task built it while we waited.
                let mut map = self.sessions.write().await;
                if let Some(s) = map.get(&key) {
                    Arc::clone(s)
                } else {
                    let built = build_session_for_agent(
                        &source, &source_dir, &p.agent_label,
                    ).await
                    .map_err(|e| RmcpError::invalid_params(
                        format!("open session: {e}"), None))?;
                    let arc = Arc::new(built);
                    map.insert(key.clone(), Arc::clone(&arc));
                    arc
                }
            }
        };

        // ── Dispatch the turn.  Session::turn_with_progress does
        // the tool-use loop (LiteLLM call → if tool_calls, dispatch
        // via rmcp client → loop, hard-capped at 8 iters) AND fires
        // a SessionProgress callback at every iteration boundary +
        // per tool dispatch.  Token counts + cost accounting happen
        // inside Session.
        //
        // Why spawn_blocking + inner current_thread runtime:
        // Session::turn holds a tokio OTel `ContextGuard` across
        // awaits, which makes its future !Send.  rmcp's #[tool]
        // macro requires `dyn Future + Send` for tool dispatch.
        // We hand the !Send turn future off to a blocking-pool
        // thread that owns its own current_thread runtime, await
        // the join handle (which IS Send).  v0.3 may refactor
        // Session::turn to scope its guards tighter and make the
        // future Send — then this dance goes away.
        //
        // Progress streaming (Week 13 Wed):
        //   - Pull the client's `progressToken` from the request
        //     `_meta` envelope.  If absent, the user didn't opt
        //     into progress; we still wire the callback but the
        //     forwarder task is a no-op (we skip the channel).
        //   - Spin up a tokio::sync::mpsc::UnboundedChannel that
        //     the !Send turn callback writes to (synchronously)
        //     and a Send forwarder task drains, dispatching
        //     `client.notify_progress(...)` per event.
        //   - On turn completion, drop the sender to close the
        //     channel; the forwarder's `recv()` returns None and
        //     the task winds down cleanly.
        let user_input = p.user_input.clone();
        let progress_token = meta.get_progress_token();

        // v0.3 Week 19 Thu: RAG prepend.  When the agent declares
        // `knowledge = [vector_store.<l>, …]`, retrieve top-K chunks
        // from each declared store + prepend them to the system
        // prompt for THIS TURN ONLY (via Session::turn_with_progress_
        // and_extra_system_prefix; the cached Session itself isn't
        // mutated so non-RAG callers stay byte-stable).
        //
        // Cost: per-turn index reload from disk + per-turn embedding
        // HTTP round-trip.  Acceptable for v0.3 prototypes (~few ms
        // for index parse, ~50-200ms for the embedding call).
        // v0.5+ should cache indexes on AislangServer keyed by
        // (source_dir, vs_label) so each turn pays only the embedder.
        //
        // Input shape: pass `source_dir` (already resolved to either
        // the multi-module project root OR the single-file .ais
        // file's parent above); `build_rag_prefix_for_turn` invokes
        // `load_project_input` which handles both shapes.  For
        // single-file projects the parent dir works because we walk
        // upward to find `*.ais` siblings.
        //
        // Why `spawn_blocking`: the helper calls
        // `LiteLlmEmbedder::embed` which spins up its own
        // current-thread tokio runtime for the reqwest HTTP call.
        // Calling that from within chat_turn's outer tokio runtime
        // would panic ("Cannot start a runtime from within a
        // runtime") — spawn_blocking moves to a non-runtime
        // thread where the nested block_on works.  Same posture
        // session.turn uses for the same reason.
        let rag_src_path = original_source_path_for_rag.clone();
        let rag_src_dir  = source_dir.clone();
        let rag_agent    = p.agent_label.clone();
        let rag_input    = user_input.clone();
        let rag_prefix = tokio::task::spawn_blocking(move || {
            build_rag_prefix_for_turn(
                &rag_src_path, &rag_src_dir, &rag_agent, &rag_input,
            ).unwrap_or_default()
        }).await.unwrap_or_default();
        if !rag_prefix.is_empty() {
            eprintln!("  chat_turn: RAG prepend ({}B retrieved context)",
                rag_prefix.len());
        }

        // Channel cap: unbounded.  Per-turn progress event count is
        // bounded by `MAX_TOOL_ITERS * (1 + 2 * tool_calls_per_iter)
        // + 1` ≈ tens of events; the mpsc allocator dominates so
        // bounding is unnecessary overhead.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SessionProgress>();

        // Forwarder task — Send by construction (tokio Peer is
        // Clone-able + Send + Sync internally).  Drains the channel
        // and dispatches each event as an rmcp progress notification
        // if (and only if) the caller supplied a progressToken.
        // Errors from notify_progress are intentionally swallowed:
        // a broken transport WILL surface later through the main
        // `chat_turn` response; we don't want progress to fail the
        // whole turn.
        let forwarder = if let Some(token) = progress_token {
            let client_for_forward = client.clone();
            tokio::spawn(async move {
                while let Some(ev) = rx.recv().await {
                    let _ = client_for_forward.notify_progress(
                        ProgressNotificationParam {
                            progress_token: token.clone(),
                            progress:       ev.step() as f64,
                            total:          None,   // unknown — model
                                                    // decides how many
                                                    // iterations it
                                                    // wants.
                            message:        Some(ev.message()),
                        }
                    ).await;
                }
            })
        } else {
            // No progress token → drain + drop with no I/O.  Same
            // recv-loop shape so callers can still send without
            // checking.
            tokio::spawn(async move { while rx.recv().await.is_some() {} })
        };

        let tx_for_callback = tx.clone();
        let reply = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("build inner runtime: {e}"))?;
            rt.block_on(session.turn_with_progress_and_extra_system_prefix(
                &rag_prefix,
                &user_input,
                move |ev| {
                    // Best-effort send: if the forwarder task panicked
                    // (or the receiver was dropped), we lose progress
                    // for the rest of the turn but the turn itself
                    // continues normally.
                    let _ = tx_for_callback.send(ev);
                },
            )).map_err(|e| format!("{e}"))
        }).await
        .map_err(|e| RmcpError::internal_error(
            format!("join blocking turn task: {e}"), None))?
        .map_err(|e| RmcpError::internal_error(
            format!("chat turn: {e}"), None))?;

        // Close the channel + drain the forwarder so all queued
        // progress notifications go out before we return the final
        // CallToolResult.  Bounded by the channel's queued count
        // (small); a 5s safety timeout avoids hangs if the
        // transport is wedged.
        drop(tx);
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            forwarder,
        ).await;

        Ok(CallToolResult::success(vec![Content::text(reply)]))
    }

    // ────────────────────────────────────────────────────────────────
    // W26 — server-side pattern dispatch.  `chat_turn` runs ONE agent;
    // `run_pipeline` runs a multi-agent **pipeline** or **loop** in a
    // single call (the orchestration the CLI's `aislang chat
    // <pattern_label>` does client-side), so a remote MCP client (Claude
    // Desktop) can invoke `coder_review_cycle` / `full_design` directly
    // instead of hand-rolling the step-threading in a prompt.
    // ────────────────────────────────────────────────────────────────

    /// Run a declared **pipeline** or **loop** end-to-end: dispatch each
    /// step agent in order, threading every reply into the next step's
    /// input; for a loop, re-run the body pipeline (feeding its reply
    /// back) until an `ACCEPT…` verdict or `max_iter`.  Returns a
    /// per-step transcript + the final reply.
    #[tool(
        name = "aislang_run_pipeline",
        description = "Run a PIPELINE, LOOP, or single AGENT defined in the \
            project (e.g. `full_design`, `review_cycle`, or a bare agent like \
            `architect`) end-to-end in ONE call — the same dispatch \
            `aislang chat <label>` does locally.  A pipeline threads each step \
            agent's reply into the next; a loop re-runs its body until the \
            reply starts with `ACCEPT` (or its `.ais` condition) or `max_iter`; \
            a bare agent label runs that one agent.  Use this instead of \
            manually chaining `aislang_chat_turn` calls.  Pass `source_path` \
            (the project dir or .ais file) + `pattern_label` + `user_input`.  \
            Branches (router) are not supported here yet — use the CLI.",
        annotations(
            title             = "Run pipeline / loop",
            destructive_hint  = false,
            read_only_hint    = false,
            idempotent_hint   = false,
            open_world_hint   = true,
        )
    )]
    pub async fn run_pipeline(&self,
        Parameters(p): Parameters<RunPipelineParameters>,
        // W26: progress streaming.  A pipeline (or even a single slow
        // architect turn) can run for minutes; the LiteLLM client is
        // non-streaming, so without periodic progress the MCP client's
        // request timeout fires (`-32001 Request timed out`) with no
        // indication of what's happening.  We emit a step-boundary message
        // per step PLUS a periodic heartbeat while a step is in flight —
        // both reset the client's timeout and tell the user "step 2/3:
        // coder running…".  Same extractors `chat_turn` uses.
        meta:   Meta,
        client: Peer<RoleServer>,
    ) -> Result<CallToolResult, RmcpError> {
        // ── resolve source_dir + source (dir-input dispatch, same as chat_turn).
        let dir_input: Option<PathBuf> = p.source_path.as_deref()
            .map(std::path::Path::new)
            .filter(|pp| pp.is_dir())
            .map(|pp| pp.to_path_buf());
        let (source_dir, source) = if let Some(dir) = dir_input.as_ref() {
            let canonical = std::fs::canonicalize(dir).map_err(|e|
                RmcpError::invalid_params(
                    format!("source_path dir `{}` not readable: {e}", dir.display()),
                    None))?;
            (canonical, String::new())
        } else {
            let sd = resolve_source_dir_or_infer_from_path(
                p.source_dir.as_deref(), p.source_path.as_deref())?;
            let s = resolve_source(p.source.clone(), p.source_path.clone(), &sd)?;
            (sd, s)
        };

        // ── load the project to resolve the pattern label + its steps.
        let loaded = crate::load_project_input(&source_dir)
            .map_err(|e| RmcpError::invalid_params(
                format!("load project: {e}"), None))?;
        let project = &loaded.resolved.project;
        let label   = &p.pattern_label;

        // Resolve label → (qualified, step agents, max_iter, is_loop, condition).
        // Mirrors the CLI's run_chat dispatch setup.
        let (qualified, steps, max_iter, is_loop, loop_condition) =
            if let Some(resolved) = loaded.find_loop_label(label) {
                let lb = project.loops.iter().find(|l| l.label == resolved)
                    .expect("find_loop_label returned a missing label");
                let body_module = resolved.split_once('.').map(|(m, _)| m);
                let body_label = match (&lb.body.module, body_module) {
                    (Some(m), _)    => format!("{m}.{}", lb.body.label),
                    (None, Some(c)) => format!("{c}.{}", lb.body.label),
                    (None, None)    => lb.body.label.clone(),
                };
                let bp = project.pipelines.iter().find(|pp| pp.label == body_label)
                    .ok_or_else(|| RmcpError::invalid_params(format!(
                        "loop `{resolved}` body pipeline `{body_label}` does not exist"),
                        None))?;
                (resolved.clone(), qualify_pipeline_step_labels(bp), lb.max_iter,
                 true, Some(lb.condition.clone()))
            } else if let Some(resolved) = loaded.find_pipeline_label(label) {
                let pl = project.pipelines.iter().find(|pp| pp.label == resolved)
                    .expect("find_pipeline_label returned a missing label");
                (resolved.clone(), qualify_pipeline_step_labels(pl), 0u32, false, None)
            } else if loaded.find_branch_label(label).is_some() {
                return Err(RmcpError::invalid_params(format!(
                    "`{label}` is a branch (router) — not supported via MCP yet; \
                     run it from the CLI (`aislang chat {label} <dir>`)"), None));
            } else if let Some(resolved) = loaded.find_agent_label(label) {
                // A bare AGENT label → run it as a single-step pipeline (one
                // turn), so `run_pipeline` doubles as a single-agent dispatcher.
                // Self-contained agents (e.g. `architect`) declare no
                // pipeline/loop; `chat_turn` is the lower-level equivalent.
                (resolved.clone(), vec![resolved.clone()], 0u32, false, None)
            } else {
                let mut names: Vec<&str> = project.agents.iter()
                    .map(|a| a.label.as_str())
                    .chain(project.pipelines.iter().map(|pp| pp.label.as_str()))
                    .chain(project.loops.iter().map(|l| l.label.as_str()))
                    .collect();
                names.sort();
                return Err(RmcpError::invalid_params(format!(
                    "no agent, pipeline, or loop named `{label}`; available: {}",
                    if names.is_empty() { "(none declared)".to_string() }
                    else { names.join(", ") }), None));
            };
        if steps.is_empty() {
            return Err(RmcpError::invalid_params(
                format!("`{qualified}` has no steps to run"), None));
        }

        // ── RAG health (#2): warn — in the transcript the user actually sees
        //    — when a step agent declares `knowledge` but its index is
        //    missing/empty (retrieval silently off).  Cheap: reuses the
        //    already-loaded project + per-store file reads, deduped per agent.
        let rag_warnings: Vec<String> = {
            let project_root = source_dir.canonicalize()
                .unwrap_or_else(|_| source_dir.clone());
            let mut seen = std::collections::HashSet::new();
            let mut ws = Vec::new();
            for step in &steps {
                if seen.insert(step.clone()) {
                    if let Some(w) = rag_health_warning(project, &project_root, step) {
                        ws.push(w);
                    }
                }
            }
            ws
        };

        // ── Fresh run: drop any cached sessions for these step agents so each
        //    `run_pipeline` invocation starts with clean history (the CLI
        //    clears pipeline history per run too).  History still accumulates
        //    ACROSS iterations WITHIN this run (so a loop's worker sees the
        //    critic's prior feedback) — we only reset once, here.
        {
            let mut map = self.sessions.write().await;
            for step in &steps {
                map.remove(&SessionKey {
                    source_dir:  source_dir.clone(),
                    agent_label: step.clone(),
                });
            }
        }

        // ── Progress streaming (W26).  When the client supplied a
        //    `progressToken`, stream human-readable progress so (a) the
        //    client's request timeout keeps resetting across a multi-minute
        //    run and (b) the user sees which step is running.  Two sources
        //    feed one monotonic-counter forwarder:
        //      • step-boundary messages (sent from the loop), and
        //      • a HEARTBEAT every `PROGRESS_HEARTBEAT_SECS` while a step is
        //        in flight — the non-streaming LiteLLM call emits nothing on
        //        its own, so the heartbeat is what actually holds the
        //        connection open during a long single turn.
        //    No token → `progress_tx` is None and every send is a cheap no-op.
        const PROGRESS_HEARTBEAT_SECS: u64 = 10;
        let progress_token = meta.get_progress_token();
        let status = std::sync::Arc::new(std::sync::Mutex::new(
            format!("starting `{qualified}`…")));
        let (progress_tx, forwarder, heartbeat) = if let Some(token) = progress_token {
            let (tx, mut rx) =
                tokio::sync::mpsc::unbounded_channel::<String>();
            // Forwarder: owns the monotonic `progress` counter so every
            // notification strictly increases (clients require that).
            let client_fwd = client.clone();
            let fwd = tokio::spawn(async move {
                let mut n = 0f64;
                while let Some(msg) = rx.recv().await {
                    n += 1.0;
                    let _ = client_fwd.notify_progress(ProgressNotificationParam {
                        progress_token: token.clone(),
                        progress:       n,
                        total:          None,
                        message:        Some(msg),
                    }).await;
                }
            });
            // Heartbeat: re-announce the current status on a timer so a long
            // single turn (non-streaming LiteLLM) still resets the client
            // timeout.  Stops when the channel closes (run finished).
            let hb_tx     = tx.clone();
            let hb_status = status.clone();
            let hb = tokio::spawn(async move {
                let mut elapsed = 0u64;
                loop {
                    tokio::time::sleep(
                        std::time::Duration::from_secs(PROGRESS_HEARTBEAT_SECS)).await;
                    elapsed += PROGRESS_HEARTBEAT_SECS;
                    let s = hb_status.lock()
                        .map(|g| g.clone()).unwrap_or_default();
                    if hb_tx.send(format!("⏳ {s} ({elapsed}s elapsed)")).is_err() {
                        break; // channel closed → run done
                    }
                }
            });
            (Some(tx), Some(fwd), Some(hb))
        } else {
            (None, None, None)
        };
        // Update `status` + push an immediate progress message (no-op when
        // the caller didn't request progress).
        let announce = |progress_tx: &Option<tokio::sync::mpsc::UnboundedSender<String>>,
                        status: &std::sync::Arc<std::sync::Mutex<String>>,
                        msg: String| {
            if let Ok(mut g) = status.lock() { *g = msg.clone(); }
            if let Some(tx) = progress_tx { let _ = tx.send(msg); }
        };

        // ── orchestrate.  Pipeline → one pass; loop → up to max_iter passes,
        //    feeding each pass's final reply back, stopping when the loop's
        //    `.ais` condition says so (or, if it can't be evaluated, on ACCEPT).
        let passes = if is_loop { max_iter.max(1) } else { 1 };
        let mut transcript    = String::new();
        let mut current       = p.user_input.clone();
        let mut final_reply   = String::new();
        let mut stopped_early = false;
        let mut iters_run     = 0u32;
        // The very first dispatch gets the user's input verbatim; every later
        // step gets it FRAMED as the previous agent's output (see below).
        let mut prev_step: Option<String> = None;
        // Capture a step failure instead of `?`-returning straight out, so the
        // progress tasks below are always cleaned up before we propagate it.
        let mut run_error: Option<String> = None;
        'outer: for iter in 1..=passes {
            iters_run = iter;
            if is_loop {
                transcript.push_str(&format!("\n══ iteration {iter}/{passes} ══\n"));
            }
            for (i, step) in steps.iter().enumerate() {
                // Announce the step (immediate message + sets the heartbeat's
                // current status) so a long turn streams "step 2/3: coder …".
                announce(&progress_tx, &status, format!(
                    "{}step {}/{}: `{step}` running…",
                    if is_loop { format!("iteration {iter}/{passes} — ") }
                    else { String::new() },
                    i + 1, steps.len()));
                // Handoff framing: passing one agent's reply RAW as the next
                // agent's user_input causes identity bleed — the LLM reads the
                // prior agent's first-person prose ("I am the Architect…") and
                // adopts that role.  Wrap each handoff so the agent treats it
                // as upstream OUTPUT and stays in its own (`step`) role.
                let input = match &prev_step {
                    None => current.clone(),
                    Some(prev) => format!(
                        "[Pipeline handoff] The previous step `{prev}` produced the \
                         output below. It is THAT agent's response — not your own \
                         identity, role, or instructions. Stay in YOUR role (`{step}`) \
                         and act on it per your system prompt.\n\n\
                         ----- begin output from `{prev}` -----\n{current}\n\
                         ----- end output from `{prev}` -----"),
                };
                let reply = match self.run_one_agent_turn(
                    &source, &source_dir, step, &input).await
                {
                    Ok(r)  => r,
                    Err(e) => {
                        run_error = Some(format!("step[{i}] `{step}`: {e}"));
                        break 'outer;
                    }
                };
                transcript.push_str(&format!("\n▸ {step}\n{reply}\n"));
                current     = reply.clone();
                final_reply = reply;
                prev_step   = Some(step.clone());
            }
            // Stop when the loop's declared `condition` is no longer true.
            // The condition is a MiniJinja template over `output` (the body's
            // final reply); e.g. `{{ output | startswith('ACCEPT') | not }}`
            // means "keep looping while output does NOT start with ACCEPT".
            // If it can't be evaluated, fall back to the ACCEPT heuristic.
            if is_loop {
                let keep_going = match loop_condition.as_deref() {
                    Some(cond) if !cond.trim().is_empty() =>
                        eval_loop_condition(cond, &final_reply)
                            .unwrap_or_else(|_| !is_accept_verdict(&final_reply)),
                    _ => !is_accept_verdict(&final_reply),
                };
                if !keep_going { stopped_early = true; break; }
            }
        }

        // ── Tear down progress streaming: stop the heartbeat, close the
        //    channel, and drain the forwarder so any queued messages flush
        //    before we return (bounded by a short timeout so a wedged
        //    transport can't hang the response).
        if let Some(hb) = heartbeat { hb.abort(); }
        drop(progress_tx);
        if let Some(fwd) = forwarder {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5), fwd).await;
        }

        // A step failure is propagated here — AFTER the cleanup above — so the
        // progress tasks never outlive the call.
        if let Some(e) = run_error {
            return Err(RmcpError::internal_error(e, None));
        }

        let header = if is_loop {
            format!("{qualified} (loop) — {} after {iters_run} iteration(s)\n",
                if stopped_early { "condition satisfied" }
                else { "stopped (max_iter reached)" })
        } else {
            format!("{qualified} (pipeline) — {} step(s)\n", steps.len())
        };
        // Prepend any RAG warnings so they're the first thing the user reads.
        let warn_block = if rag_warnings.is_empty() { String::new() }
            else { format!("{}\n\n", rag_warnings.join("\n")) };
        Ok(CallToolResult::success(vec![Content::text(
            format!("{warn_block}{header}{transcript}\n── final reply ──\n{final_reply}"))]))
    }

    /// Run ONE agent turn against the (source_dir, agent_label) session
    /// (get-or-create from the shared pool), returning the reply.  The
    /// reusable core `run_pipeline` calls per step; mirrors `chat_turn`'s
    /// session + `spawn_blocking`-turn dance (incl. RAG prepend, W26).
    async fn run_one_agent_turn(
        &self,
        source:      &str,
        source_dir:  &Path,
        agent_label: &str,
        user_input:  &str,
    ) -> Result<String, String> {
        let key = SessionKey {
            source_dir:  source_dir.to_path_buf(),
            agent_label: agent_label.to_string(),
        };
        let session = {
            let map = self.sessions.read().await;
            if let Some(s) = map.get(&key) {
                Arc::clone(s)
            } else {
                drop(map);
                let mut map = self.sessions.write().await;
                if let Some(s) = map.get(&key) {
                    Arc::clone(s)
                } else {
                    let built = build_session_for_agent(source, source_dir, agent_label)
                        .await
                        .map_err(|e| format!("open session for `{agent_label}`: {e}"))?;
                    let arc = Arc::new(built);
                    map.insert(key.clone(), Arc::clone(&arc));
                    arc
                }
            }
        };
        // RAG prepend (best-effort, same as chat_turn): retrieve top-K chunks
        // from the agent's declared knowledge stores + prepend to the system
        // prompt for THIS turn.  No-op (empty prefix) when no index is present,
        // so pipeline steps stay functional even without RAG.  W26: this closes
        // the gap where run_pipeline agents saw zero retrieved context.
        let rag_prefix = {
            let sd = source_dir.to_path_buf();
            let al = agent_label.to_string();
            let ui = user_input.to_string();
            tokio::task::spawn_blocking(move || {
                build_rag_prefix_for_turn(&sd, &sd, &al, &ui).unwrap_or_default()
            }).await.unwrap_or_default()
        };
        // Log retrieval like chat_turn does — run_pipeline went through this
        // path silently, so "is RAG actually firing?" wasn't visible in the
        // pod logs (only chat_turn emitted a "RAG prepend" line).
        if !rag_prefix.is_empty() {
            eprintln!("  run_pipeline[{agent_label}]: RAG prepend \
                ({}B retrieved context)", rag_prefix.len());
        }
        let input = user_input.to_string();
        // Session::turn is !Send (holds an OTel guard across awaits); hand it
        // to a blocking thread with its own current-thread runtime, same as
        // chat_turn.  The JoinHandle is Send, so this stays #[tool]-compatible.
        tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build()
                .map_err(|e| format!("build inner runtime: {e}"))?;
            rt.block_on(session.turn_with_progress_and_extra_system_prefix(
                &rag_prefix, &input, |_ev| {})).map_err(|e| format!("{e}"))
        }).await.map_err(|e| format!("join turn task: {e}"))?
    }

    /// Run a declared eval against the deployed stack.  Loads the
    /// dataset, opens a fresh Session, scores every case, aggregates,
    /// returns the EvalReport as pretty JSON.
    ///
    /// `eval_label` selects which eval block to run; when omitted
    /// every `eval` block in the source runs and the report is a
    /// JSON array.  Without an `agent = agent.X` field on the eval
    /// block, the project's single agent is used (matches v0.1 CLI
    /// disambiguation rules).
    ///
    /// Each eval creates its own Session (no reuse from chat_turn's
    /// pool) — eval semantics need a fresh conversation per case,
    /// matching v0.1's `aislang eval` behaviour.
    #[tool(
        name = "aislang_eval",
        description = "Run a declared eval against the deployed stack. \
            Loads the dataset (JSONL file referenced by eval.dataset), \
            opens a Session, scores every case via the declared metric \
            (exact_match | f1; v0.2 may add llm_judge), aggregates with \
            the declared aggregator (mean by default), returns EvalReport \
            as pretty JSON. Without `eval_label`, every eval block runs \
            and the response is a JSON array of EvalReports.",
        annotations(
            title             = "Run aiSlang eval",
            destructive_hint  = false,
            read_only_hint    = false,
            idempotent_hint   = false,
            open_world_hint   = true,
        )
    )]
    pub async fn eval(&self,
        Parameters(p): Parameters<EvalParameters>,
    ) -> Result<CallToolResult, RmcpError> {
        let source_dir = resolve_source_dir_or_infer_from_path(
            p.source_dir.as_deref(),
            p.source_path.as_deref(),
        )?;
        let source     = resolve_source(p.source, p.source_path, &source_dir)?;
        let (project, _warnings) = aislang_core::validate(&source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;

        // Filter evals: explicit label or all.
        let evals: Vec<&aislang_core::ast::Eval> = match p.eval_label.as_deref() {
            Some(name) => match project.evals.iter().find(|e| e.label == name) {
                Some(e) => vec![e],
                None => {
                    let available: Vec<&str> = project.evals.iter()
                        .map(|e| e.label.as_str()).collect();
                    return Err(RmcpError::invalid_params(
                        format!("no eval named `{name}`; available: {}",
                            if available.is_empty() { "(none declared)".to_string() }
                            else { available.join(", ") }),
                        None));
                }
            },
            None => project.evals.iter().collect(),
        };
        if evals.is_empty() {
            return Err(RmcpError::invalid_params(
                "no `eval` blocks declared in source".to_string(), None));
        }

        // Run each eval, collect reports.  Same spawn_blocking +
        // inner current_thread runtime dance as chat_turn (see
        // comment there): run_eval_for_mcp's loop awaits Session::turn
        // whose future is !Send.  Project + eval refs are kept
        // by-value via clones into the closure.
        let mut reports: Vec<crate::EvalReport> = Vec::with_capacity(evals.len());
        for eval in evals {
            // Clone the project + eval into the blocking closure since
            // they're borrowed from p.source — the closure outlives
            // this method's stack frame.
            let project_owned = project.clone();
            let eval_owned    = eval.clone();
            let source_dir_owned = source_dir.clone();
            let eval_label_for_err = eval.label.clone();
            let report = tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| format!("build inner runtime: {e}"))?;
                rt.block_on(run_eval_for_mcp(&project_owned, &eval_owned, &source_dir_owned))
            }).await
            .map_err(|e| RmcpError::internal_error(
                format!("join eval task `{eval_label_for_err}`: {e}"), None))?
            .map_err(|e| RmcpError::internal_error(
                format!("eval `{eval_label_for_err}`: {e}"), None))?;
            reports.push(report);
        }

        // Render: single report stays as one JSON object; multiple
        // get wrapped in an array (matches v0.1 CLI behaviour where
        // multi-eval runs emit one JSON line per report on stdout).
        let body = if reports.len() == 1 {
            serde_json::to_string_pretty(&reports[0])
        } else {
            serde_json::to_string_pretty(&reports)
        }.map_err(|e| RmcpError::internal_error(
            format!("serialize eval report: {e}"), None))?;
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }
}

// ============================================================================
// Parameter types for Tue's tools
// ============================================================================
//
// All `#[tool]`-annotated methods that take arguments need a single
// `Parameters<T>` argument where `T: Deserialize + JsonSchema`.  The
// schemars derive emits the input schema that hosts (Claude Desktop,
// Cursor) render in the tool-call UI.

/// `name = "<example>"` — picks one example.  Omit for the index.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ExamplesParameters {
    /// Example name: `minimal` | `support-agent` | `support-agent-paid`
    /// | `free-local`.  When omitted, returns a human-readable index.
    #[serde(default)]
    pub name: Option<String>,
}

/// Single-field shape used by both `describe` and `validate` — both
/// consume an `.ais` source-text body and nothing else.
///
/// Week 13 Thu follow-up: `source` is now optional; `source_path`
/// is the recommended alternative.  See [`resolve_source`] for the
/// resolution policy + the Claude-Desktop-preview rationale.
///
/// Field order is intentional — `source_path` first so JSON-schema
/// readers (including LLMs picking a calling shape) see the
/// recommended form first.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct SourceOnlyParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against the MCP server's current working
    /// directory; absolute paths are used as-is.  When set, wins
    /// over inline `source`.
    ///
    /// **MCP-host integrations (Claude Desktop, Cursor, ChatGPT-with-
    /// MCP, etc.) MUST use this form.**  The inline `source` field
    /// below puts the whole `.ais` file body into the JSON request
    /// envelope, which UI hosts render verbatim in the tool-call
    /// confirmation dialog — producing a multi-line wall of
    /// `\n`-escaped text the user can't scan before approving.
    /// Passing `source_path` keeps the dialog tiny + readable.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only** (synthetic test harnesses,
    /// programmatic callers that materialise source on the fly).
    /// The complete `.ais` source file as a UTF-8 text body.  Must
    /// parse + typecheck for `describe`; can be malformed for
    /// `validate` (the diagnostic is the point).  When `source_path`
    /// is also provided, it wins — see `source_path`'s docs for
    /// why UI hosts must prefer that form.
    #[serde(default)]
    pub source: Option<String>,
}

/// `plan` extends `SourceOnlyParameters` with an optional
/// `source_dir` so prompt-file paths can be resolved against the
/// caller's project directory.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct PlanParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against `source_dir` (or cwd if `source_dir`
    /// unset); absolute paths are used as-is.  MCP-host integrations
    /// (Claude Desktop, Cursor) MUST use this form — see
    /// [`SourceOnlyParameters::source_path`] for the rationale.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The complete `.ais` source
    /// file as a UTF-8 text body.  Inline form produces an unscannable
    /// wall of `\n`-escaped text in UI hosts' confirmation dialogs.
    /// When `source_path` is also provided, it wins.
    #[serde(default)]
    pub source: Option<String>,
    /// Absolute filesystem path that `prompt.file` and
    /// `mcp_server.mount` paths in the source are resolved against.
    /// Defaults to the MCP server's current working directory when
    /// omitted.  Prompt files referenced must exist at the resolved
    /// paths.
    #[serde(default)]
    pub source_dir: Option<String>,
}

// ── Wed: stateful-tier parameter types ──────────────────────────────

/// `apply` parameters — same shape as `plan` (source + source_dir).
/// Renaming for documentation clarity; the runtime semantics differ
/// (apply writes to disk + starts Docker, plan is in-memory).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ApplyParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against `source_dir`.  MCP-host integrations
    /// (Claude Desktop, Cursor) MUST use this form — keeps the
    /// destructive-hint confirmation dialog scannable instead of
    /// dumping the whole `.ais` body as JSON-escaped text.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The complete `.ais` source
    /// file as a UTF-8 text body.  When `source_path` is also
    /// provided, it wins.
    #[serde(default)]
    pub source: Option<String>,
    /// Filesystem path that will receive the emitted artifacts
    /// (stack.yml, litellm_config.yaml, .env.example, aislang.lock,
    /// .aislang/state.json) and where prompt-file paths in the source
    /// are resolved against. Defaults to the MCP server's current
    /// working directory when omitted. Prompt files referenced by the
    /// source MUST already exist at the resolved paths.
    #[serde(default)]
    pub source_dir: Option<String>,
}

/// `destroy` parameters.  Doesn't need the `source` field — state
/// + stack.yml living in `source_dir` are sufficient.  But the v0.1
/// CLI's `aislang destroy <file>` does take a source file, so we
/// keep the API surface symmetric with `apply` + `status` and just
/// ignore `source` here.  Future v0.3+ may use `source` for a
/// pre-destroy diff against the currently-applied state.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct DestroyParameters {
    /// Filesystem path holding the deployed stack's state.json +
    /// stack.yml.  Defaults to cwd when omitted.
    #[serde(default)]
    pub source_dir: Option<String>,
    /// When true, bypass the idempotency check (state-shows-torn-down
    /// + nothing-to-destroy) and run `docker compose down -v` against
    /// the stack.yml unconditionally.  Use when state.json was lost
    /// or manually edited away. Defaults to false.
    #[serde(default)]
    pub force: Option<bool>,
}

/// `status` parameters: source-for-project-name + source_dir + the
/// two optional spend toggles.  Mirrors the CLI flags 1:1.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct StatusParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against `source_dir`.  MCP-host integrations
    /// (Claude Desktop, Cursor) MUST use this form.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The complete `.ais` source
    /// file as a UTF-8 text body — used to resolve the project name
    /// (which keys spend.json's tracker).  When `source_path` is
    /// also provided, it wins.
    #[serde(default)]
    pub source: Option<String>,
    /// Filesystem path containing .aislang/state.json + stack.yml.
    /// Defaults to cwd.
    #[serde(default)]
    pub source_dir: Option<String>,
    /// When true, append today's UTC spend total + cap utilization %.
    #[serde(default)]
    pub spend: Option<bool>,
    /// When set, append today's by-model spend breakdown.  Empty
    /// string filters to "all models"; a non-empty string filters to
    /// one model id (e.g. `"anthropic.claude_haiku_4_5"`). Matches
    /// the CLI's `--model-spend [=MODEL]` semantics.
    #[serde(default)]
    pub model_spend: Option<String>,
}

// ── Thu: conversation-tier parameter types ─────────────────────────

/// `chat_turn` parameters.  The `(source_dir, agent_label)` pair
/// keys into the Session cache — same key → same Session
/// (conversation history persists across calls).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct ChatTurnParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against `source_dir`.  MCP-host integrations
    /// (Claude Desktop, Cursor) MUST use this form — keeps the
    /// chat-turn confirmation preview tiny + scannable instead of
    /// dumping the whole `.ais` source as JSON-escaped text.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The complete `.ais` source
    /// file as a UTF-8 text body.  Used to validate the project,
    /// find the agent by label, and resolve its model + system_prompt
    /// on first call.  Subsequent calls with the same (source_dir,
    /// agent_label) ignore changes here — the cached Session was
    /// built from the initial source.  When `source_path` is also
    /// provided, it wins.
    #[serde(default)]
    pub source: Option<String>,
    /// Filesystem path that prompt-file paths in the source resolve
    /// against, and where `aislang.lock` is read for prompt-hash
    /// verification.  Defaults to cwd.
    #[serde(default)]
    pub source_dir: Option<String>,
    /// Agent label declared in the source (e.g. `"support"`).  The
    /// chat turn runs against this agent — its model, system prompt,
    /// and tools.
    pub agent_label: String,
    /// User message to send.  Will be appended to the cached
    /// Session's message history; assistant reply is returned.
    pub user_input: String,
}

/// `run_pipeline` parameters — like `chat_turn` but targets a PIPELINE
/// or LOOP label instead of a single agent.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct RunPipelineParameters {
    /// **PREFERRED.**  Filesystem path to the project — a directory (a
    /// multi-module project root holding `aislang.toml`) or a single
    /// `.ais` file.  In a deployed agent this is the mount dir
    /// (`/etc/aislang`).
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The full `.ais` source body
    /// (single-file projects).  `source_path` wins when both are given.
    #[serde(default)]
    pub source: Option<String>,
    /// Directory that module/prompt paths resolve against.  Defaults to
    /// cwd / inferred from `source_path`.
    #[serde(default)]
    pub source_dir: Option<String>,
    /// The pipeline, loop, or single-agent label to run (e.g.
    /// `full_design`, `review_cycle`, or a bare agent like `architect`).
    /// Accepts a bare label or a module-qualified one; a unique suffix
    /// match also resolves.
    pub pattern_label: String,
    /// Initial input fed to the FIRST step (and, for a loop, the first
    /// pass).  Each step's reply threads into the next step's input.
    pub user_input: String,
}

/// `eval` parameters.  No agent_label — each `eval` block in the
/// source declares its own agent (or defaults to the project's
/// single agent).
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct EvalParameters {
    /// **PREFERRED.**  Filesystem path to the `.ais` file.  Relative
    /// paths resolve against `source_dir`.  MCP-host integrations
    /// (Claude Desktop, Cursor) MUST use this form.
    #[serde(default)]
    pub source_path: Option<String>,
    /// **For in-memory callers only.**  The complete `.ais` source
    /// file as a UTF-8 text body.  When `source_path` is also
    /// provided, it wins.
    #[serde(default)]
    pub source: Option<String>,
    /// Filesystem path that prompt files + JSONL datasets in the
    /// source resolve against.  Defaults to cwd.
    #[serde(default)]
    pub source_dir: Option<String>,
    /// Optional eval-block label.  When omitted, every `eval` block
    /// in the source runs and the response is a JSON array of
    /// EvalReports.  When set, only the named eval runs.
    #[serde(default)]
    pub eval_label: Option<String>,
}

// ── Thu: conversation-tier helpers ──────────────────────────────────

/// Build the OCI-image string from a parsed `OciImage`.  Mirrors the
/// CLI's `format_image` helper; copied here to keep the MCP layer
/// self-contained.
fn format_image(img: &aislang_core::types::OciImage) -> String {
    let base = format!("{}/{}", img.registry, img.repository);
    match (&img.tag, &img.digest) {
        (Some(t), Some(d)) => format!("{base}:{t}@{d}"),
        (Some(t), None)    => format!("{base}:{t}"),
        (None,    Some(d)) => format!("{base}@{d}"),
        (None,    None)    => base,
    }
}

/// Walk one mcp_server's `secrets = [secret.X, ...]` refs and resolve
/// them to `(label, value)` env pairs.  Mirrors the CLI's
/// `resolve_mcp_secrets` (Decision 9: secrets reach ONLY the declared
/// scope's child env — no cross-resource leakage).
fn resolve_mcp_secrets_for_agent(
    project:    &aislang_core::ast::Project,
    mcp:        &aislang_core::ast::McpServer,
    source_dir: &Path,
) -> Result<Vec<(String, String)>, String> {
    let refs = match &mcp.secrets {
        Some(refs) if !refs.is_empty() => refs,
        _ => return Ok(Vec::new()),
    };
    let mut env = Vec::with_capacity(refs.len());
    for s_ref in refs {
        let secret = project.secrets.iter()
            .find(|s| s.label == s_ref.label)
            .ok_or_else(|| format!(
                "mcp_server `{}` references secret `{}` which doesn't exist",
                mcp.label, s_ref.label))?;
        let value = crate::resolve_secret(&secret.label, &secret.source, source_dir)
            .map_err(|e| format!("resolve secret `{}`: {e}", secret.label))?;
        env.push((secret.label.clone(), value));
    }
    Ok(env)
}

/// Open a fresh Session for `(source, source_dir, agent_label)`.
/// Mirrors v0.1's `chat_interactive` setup logic exactly: validate
/// → find agent → resolve model + prompt → optional budget +
/// tracker → spawn MCP children → list tools → open LiteLLM client.
///
/// Used by both `chat_turn` (cached via the SessionKey-keyed map)
/// and `eval` (fresh per eval run, never cached).
/// W26 (#2) — cheap RAG health probe for the USER-FACING warning, no
/// embedding call.  For an agent that DECLARES `knowledge`, check each
/// referenced `vector_store` has a non-empty on-disk index.  Returns a
/// one-line warning naming the unhealthy stores, or `None` when the agent
/// declares no knowledge or every declared index is present + non-empty.
/// `run_pipeline` prepends this to the chat transcript so "RAG is broken /
/// off" is visible to the user, not just buried in pod logs (the
/// `rag build-all … || true` entrypoint otherwise hides a failed corpus
/// build).  `project`/`project_root` come from the already-loaded project so
/// this adds only per-store file reads.
fn rag_health_warning(
    project:      &aislang_core::ast::Project,
    project_root: &std::path::Path,
    agent_label:  &str,
) -> Option<String> {
    let agent = project.agents.iter().find(|a| a.label == agent_label)?;
    let knowledge = agent.knowledge.as_ref().filter(|k| !k.is_empty())?;
    let dir = crate::rag::vector_index_dir(project_root);
    let mut unhealthy: Vec<String> = Vec::new();
    for vs_ref in knowledge {
        let vs_key = vs_ref.qualified_in(&agent.label);
        // Resolve to the store's own (possibly cross-module) label.
        let label = project.vector_stores.iter()
            .find(|v| v.label == vs_key)
            .map(|v| v.label.clone())
            .unwrap_or(vs_key);
        let path = dir.join(format!("{label}.bin"));
        let healthy = crate::rag::Index::load(&path)
            .map(|i| !i.chunks.is_empty())
            .unwrap_or(false);
        if !healthy { unhealthy.push(label); }
    }
    if unhealthy.is_empty() { return None; }
    Some(format!(
        "⚠ RAG unavailable for `{agent_label}`: no usable index for [{}] — this \
         agent is running WITHOUT retrieval (grounding off, answers may be \
         ungrounded). Most likely a missing OPENAI_API_KEY / GITHUB_TOKEN or an \
         unbuilt corpus; check `aislang rag verify /etc/aislang` and the \
         agent-runtime logs.",
        unhealthy.join(", ")))
}

/// v0.3 Week 19 Thu — RAG prepend for chat_turn.
///
/// Given the agent + user_input, retrieve the top-K most-relevant
/// chunks from EVERY `vector_store` the agent's `knowledge` list
/// declares + return a markdown-formatted prefix string ready to
/// concatenate in front of the agent's system prompt.
///
/// Returns `Ok(String::new())` when the agent has no knowledge
/// declared (the cheap no-op path) — callers can treat empty
/// prefix as "no RAG happened" without checking Option.
///
/// Failure modes (all fail-soft per caller — empty prefix on Err):
///   - source_path / source_dir read errors
///   - agent label not found
///   - embedding catalog miss (provider/model mismatch)
///   - index file missing (`aislang apply` not run for the
///     vector_store), corrupt, or dim-mismatched
///   - LiteLLM /v1/embeddings down or non-200
fn build_rag_prefix_for_turn(
    source_path: &std::path::Path,
    source_dir:  &std::path::Path,
    agent_label: &str,
    user_input:  &str,
) -> std::io::Result<String> {
    use crate::project_loader::load_project_input;
    use crate::rag::{Index, LiteLlmEmbedder, Embedder};

    // 1. Load project + locate the agent.  Prefer the explicit
    //    source_path when supplied + valid (single-file path);
    //    fall back to source_dir (multi-module project root).
    //    The empty default in the caller is a sentinel meaning
    //    "no source_path supplied — use source_dir".
    let project_input = if source_path.as_os_str().is_empty()
        || !source_path.exists() {
        source_dir.to_path_buf()
    } else {
        source_path.to_path_buf()
    };
    let loaded = load_project_input(&project_input)
        .map_err(|e| std::io::Error::new(
            std::io::ErrorKind::Other, format!("{e:?}")))?;
    let project = &loaded.resolved.project;
    let agent = project.agents.iter()
        .find(|a| a.label == agent_label)
        .ok_or_else(|| std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("agent `{agent_label}` not in resolved project")))?;
    let knowledge = match &agent.knowledge {
        Some(k) if !k.is_empty() => k,
        _ => return Ok(String::new()),   // no RAG declared
    };

    // 2. For each knowledge ref → resolve the vector_store + its
    //    embedding + the catalog entry's dims + load the on-disk
    //    index + embed the user_input + take top-K matches.
    //    Default top-K = 4 per store (modest; v0.5+ may make
    //    configurable via `agent.knowledge_top_k`).
    let catalog = aislang_catalog::bundled()
        .map_err(|e| std::io::Error::new(
            std::io::ErrorKind::Other, format!("catalog: {e:?}")))?;
    let project_root = source_dir.canonicalize()
        .unwrap_or_else(|_| source_dir.to_path_buf());
    const TOP_K_PER_STORE: usize = 4;
    // LiteLLM URL: chat_turn lives inside mcp-serve, which the CLI
    // spawned as a child; LiteLLM runs in the user's Compose stack
    // at the conventional 4000.  v0.5+ will read this from a
    // project-level env / config knob.
    const LITELLM_URL: &str = "http://localhost:4000";

    let mut chunks_collected: Vec<(String, String, f32)> = Vec::new();
    //                              ^vs_label ^text     ^score

    for vs_ref in knowledge {
        // Multi-module discipline: knowledge refs (agent → vector_store)
        // and the vector_store's embedding ref are parser-shape; resolve
        // both via Ref::qualified_in against the containing block's
        // merged label.
        let vs_key = vs_ref.qualified_in(&agent.label);
        let vs = project.vector_stores.iter()
            .find(|v| v.label == vs_key)
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("vector_store `{vs_key}` not declared")))?;
        let emb_key = vs.embedding.qualified_in(&vs.label);
        let emb_block = project.embeddings.iter()
            .find(|e| e.label == emb_key)
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("embedding `{emb_key}` not declared")))?;
        let cat_entry = catalog.find_embedding(&emb_block.provider, &emb_block.model)
            .ok_or_else(|| std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("embedding ({}, {}) not in catalog",
                    emb_block.provider, emb_block.model)))?;
        let idx_path = crate::rag::vector_index_dir(&project_root)
            .join(format!("{}.bin", vs.label));
        let index = Index::load(&idx_path)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("index `{}` not loaded (run `aislang apply` to build): {e:?}",
                    idx_path.display())))?;
        let embedder = LiteLlmEmbedder::new(
            LITELLM_URL, &emb_block.label, cat_entry.dimensions as usize);
        let query_vec = embedder.embed(user_input)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("embed user_input: {e:?}")))?;
        let hits = index.query(&query_vec, TOP_K_PER_STORE)
            .map_err(|e| std::io::Error::new(
                std::io::ErrorKind::Other, format!("query: {e:?}")))?;
        for (text, score) in hits {
            chunks_collected.push((vs.label.clone(), text, score));
        }
    }

    if chunks_collected.is_empty() { return Ok(String::new()); }

    // 3. Sort all chunks globally by score descending so the
    //    LLM sees the most-relevant context first.
    chunks_collected.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(
        std::cmp::Ordering::Equal));

    // 4. Format the prefix — markdown with one section per chunk.
    //    Score is included in a small caption so downstream debug
    //    runs can see how "confident" the retrieval was.
    let mut out = String::from("## Retrieved context\n\n");
    out.push_str("The following chunks were retrieved from this \
                  agent's knowledge base via cosine similarity \
                  against the user's last message.  Use them to \
                  ground your reply.\n\n");
    for (i, (vs_label, text, score)) in chunks_collected.iter().enumerate() {
        out.push_str(&format!(
            "### Chunk {} (vector_store=`{vs_label}`, score={:.3})\n\n{}\n\n",
            i + 1, score, text));
    }
    Ok(out)
}

/// Fold a pipeline's `steps: Vec<Ref>` into qualified agent labels for
/// the `(source_dir, agent_label)` session-key lookup.  Mirrors the CLI's
/// `qualify_pipeline_steps` + `patterns::orchestrator::qualified_step_labels`:
/// a cross-module ref → `m.<label>`; a same-module ref inherits the
/// pipeline's own `<module>.` prefix; single-file → the bare label.
fn qualify_pipeline_step_labels(pipeline: &aislang_core::ast::Pipeline) -> Vec<String> {
    let containing = pipeline.label.split_once('.').map(|(m, _)| m);
    pipeline.steps.iter().map(|r| match (&r.module, containing) {
        (Some(m), _)    => format!("{m}.{}", r.label),
        (None, Some(c)) => format!("{c}.{}", r.label),
        (None, None)    => r.label.clone(),
    }).collect()
}

/// Evaluate a `loop.condition` MiniJinja template against the body's
/// final reply (bound as `output`), returning whether the loop should
/// CONTINUE (truthy = keep looping).  Registers the string filters the
/// DSL's conditions use (`startswith` / `endswith` / `contains` / `not`)
/// on top of MiniJinja's built-ins, e.g.
/// `{{ output | startswith('ACCEPT') | not }}` → keep looping while the
/// reply does NOT start with `ACCEPT`.  Errors (bad template / unknown
/// filter) propagate so the caller can fall back to the ACCEPT heuristic.
fn eval_loop_condition(condition: &str, output: &str) -> Result<bool, String> {
    let mut env = minijinja::Environment::new();
    env.add_filter("startswith", |s: String, p: String| s.starts_with(&p));
    env.add_filter("endswith",   |s: String, p: String| s.ends_with(&p));
    env.add_filter("contains",   |s: String, p: String| s.contains(&p));
    env.add_filter("not",        |b: bool| !b);
    let rendered = env.render_str(condition, minijinja::context! { output => output })
        .map_err(|e| format!("evaluate loop condition `{condition}`: {e}"))?;
    // The loop continues while the condition renders truthy.
    let t = rendered.trim().to_ascii_lowercase();
    Ok(t == "true" || t == "1" || t == "yes")
}

/// Fallback loop stop-condition when `loop.condition` can't be evaluated:
/// the body pipeline's final reply is an ACCEPT verdict.  Matches the
/// CLI's check — a leading `ACCEPT` (case-insensitive) on the trimmed
/// reply, or a `{"verdict":"ACCEPT…"}` JSON shape.
fn is_accept_verdict(reply: &str) -> bool {
    let t = reply.trim();
    if t.to_ascii_lowercase().starts_with("accept") {
        return true;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(t) {
        if let Some(s) = v.get("verdict").and_then(|x| x.as_str()) {
            return s.trim().to_ascii_lowercase().starts_with("accept");
        }
    }
    false
}

async fn build_session_for_agent(
    source:      &str,
    source_dir:  &Path,
    agent_label: &str,
) -> Result<Session, String> {
    // v0.3 Week 17 Tue: dir-input dispatch.  When the source_dir
    // holds an `aislang.toml`, route through the multi-module
    // builder which uses `load_project_input` (cross-module check,
    // qualified labels, pre-loaded prompt bodies).  Single-file
    // path below stays byte-stable for v0.2 chat sessions.
    if source_dir.join(aislang_core::manifest::MANIFEST_FILE_NAME).exists() {
        return build_session_for_agent_multi_module(source_dir, agent_label).await;
    }
    use crate::{BudgetConfig, SpawnConfig, StdioChild, ToolRegistry};

    // Single-file path.  Dir-input callers (chat_turn / run_pipeline with
    // source_path = <dir>) pass an EMPTY `source`; read the lone `*.ais` in the
    // dir (e.g. the agent-runtime's `/etc/aislang/project.ais` mount) so a
    // single-file project can be addressed by its DIRECTORY, not just the file.
    let source: String = if source.trim().is_empty() {
        let f = crate::project_loader::lone_ais_in_dir(source_dir).ok_or_else(|| format!(
            "no `aislang.toml` and no single `.ais` in `{}` — point at a .ais \
             file or add a manifest", source_dir.display()))?;
        std::fs::read_to_string(&f).map_err(|e| format!("read {}: {e}", f.display()))?
    } else {
        source.to_string()
    };

    // ── 1. validate + find agent ──
    let (project, _warnings) = aislang_core::validate(&source)
        .map_err(|e| format!("parse error: {e}"))?;
    let agent = project.agents.iter()
        .find(|a| a.label == agent_label)
        .ok_or_else(|| {
            let labels: Vec<&str> = project.agents.iter()
                .map(|a| a.label.as_str()).collect();
            format!("no agent named `{agent_label}`; available: {}",
                if labels.is_empty() { "(none declared)".to_string() }
                else { labels.join(", ") })
        })?;
    let model_label = agent.model.label.clone();

    // ── 2. resolve prompt + hash-verify against aislang.lock ──
    let prompt_block = project.prompts.iter()
        .find(|p| p.label == agent.system_prompt.label)
        .ok_or_else(|| format!(
            "agent `{}` references prompt `{}` which doesn't exist",
            agent.label, agent.system_prompt.label))?;
    let prompt_path = source_dir.join(&prompt_block.file);
    let lock_path   = source_dir.join("aislang.lock");
    let expected_hash: Option<String> =
        match aislang_resolver::lockfile::read(&lock_path) {
            Ok(Some(lock)) => lock.prompt_hashes.get(&prompt_block.label).cloned(),
            _              => None,
        };
    let system_prompt = crate::load_system_prompt(
        &prompt_path, expected_hash.as_deref(),
    ).map_err(|e| format!("load prompt: {e}"))?;
    // v0.3 Week 18 Tue: render as MiniJinja template when
    // `prompt.template = true` was declared.  Static prompts (the
    // v0.2 default) pass through unchanged — byte-stable.
    let system_prompt = crate::session::render_system_prompt_if_template(
        system_prompt,
        prompt_block.template,
        &agent.label,
        &prompt_block.label,
        &model_label,
    ).map_err(|e| format!("render template: {e}"))?;

    // ── 3. budget config + persistent spend tracker (when declared) ──
    let budget_cfg: Option<BudgetConfig> = match project.budget.as_ref() {
        Some(b) => {
            let resolved = aislang_resolver::resolve_against_bundled_catalog(
                &project, source_dir,
            ).map_err(|e| format!("resolve: {e}"))?;
            let model_id = resolved.model_bindings.get(&model_label)
                .ok_or_else(|| format!(
                    "model `{model_label}` has no resolved binding"))?;
            let catalog = aislang_catalog::bundled()
                .map_err(|e| format!("catalog load: {e}"))?;
            let entry = catalog.find(model_id)
                .ok_or_else(|| format!(
                    "catalog entry `{}.{}` not found",
                    model_id.provider, model_id.name))?;
            Some(BudgetConfig {
                per_run_cap:        b.hard_cap_per_run,
                per_day_cap:        b.hard_cap_per_day,
                cost_input_per_1k:  entry.cost_input,
                cost_output_per_1k: entry.cost_output,
                model_id_str:       format!("{}.{}", model_id.provider, model_id.name),
            })
        }
        None => None,
    };
    let tracker = match budget_cfg.as_ref() {
        Some(_) => Some(crate::SpendTracker::open_default(&project.name)
            .map_err(|e| format!("open spend tracker: {e}"))?),
        None => None,
    };

    // ── 4. collect MCP-child specs for the agent's tools ──
    let mut mcp_specs: Vec<(String, SpawnConfig)> = Vec::new();
    for tool_ref in &agent.tools {
        if tool_ref.kind != aislang_core::ast::RefKind::McpServer {
            return Err(format!(
                "agent `{}.tools` ref `{}` is not an mcp_server ref",
                agent.label, tool_ref.label));
        }
        let mcp = project.mcp_servers.iter()
            .find(|m| m.label == tool_ref.label)
            .ok_or_else(|| format!(
                "agent `{}` references mcp_server `{}` which doesn't exist",
                agent.label, tool_ref.label))?;
        let mount = mcp.mount.as_ref().map(|p| {
            let resolved = source_dir.join(p);
            std::fs::canonicalize(&resolved).unwrap_or(resolved)
        });
        let env = resolve_mcp_secrets_for_agent(&project, mcp, source_dir)?;
        mcp_specs.push((mcp.label.clone(), SpawnConfig {
            image: format_image(&mcp.image),
            args:  Vec::new(),
            mount,
            env,
        }));
    }

    // ── 5. spawn MCP children + list tools ──
    let mut mcp_clients = Vec::with_capacity(mcp_specs.len());
    let mut server_tools: Vec<(String, Vec<crate::ToolSpec>)> = Vec::new();
    for (label, cfg) in &mcp_specs {
        let child = StdioChild::spawn(cfg).await
            .map_err(|e| format!("spawn MCP `{label}`: {e}"))?;
        let tools = child.list_tools().await
            .map_err(|e| format!("list tools `{label}`: {e}"))?;
        server_tools.push((label.clone(), tools));
        mcp_clients.push(child);
    }
    let tool_registry = ToolRegistry::from_servers(&server_tools);

    // ── 6. open Session with optional budget + tracker ──
    // litellm_url from AISLANG_LITELLM_URL (the K8s emitter sets it to
    // http://litellm:4000); None → localhost default for the local CLI.
    let litellm_url = crate::litellm::LiteLlmClient::url_from_env();
    let s = Session::open_with_tools(
        &model_label, system_prompt, litellm_url.as_deref(),
        mcp_clients, tool_registry,
    ).map_err(|e| format!("open session: {e}"))?;
    let s = match budget_cfg { Some(b) => s.with_budget(b), None => s };
    let s = match tracker    { Some(t) => s.with_tracker(t),    None => s };
    Ok(s)
}

/// v0.3 Week 17 Tue — multi-module session builder.  Mirrors the
/// single-file `build_session_for_agent` but sources the merged
/// project from [`crate::load_project_input`] and uses module-
/// qualified label lookups for model_bindings + prompt_bodies.
///
/// `agent_label` is the module-qualified form (e.g. `coder.coder`)
/// — the CLI's `run_chat` calls `LoadedProject::find_agent_label`
/// to map a bare user input (`coder`) to this form before dispatch.
async fn build_session_for_agent_multi_module(
    source_dir:  &Path,
    agent_label: &str,
) -> Result<Session, String> {
    use crate::{BudgetConfig, SpawnConfig, StdioChild, ToolRegistry};

    let loaded = crate::load_project_input(source_dir)
        .map_err(|e| format!("load multi-module project: {e}"))?;
    let project = &loaded.resolved.project;

    // ── 1. find agent in the merged project (label is module-
    //       qualified — `coder.coder`, `architect.architect`).
    let agent = project.agents.iter()
        .find(|a| a.label == agent_label)
        .ok_or_else(|| {
            let labels: Vec<&str> = project.agents.iter()
                .map(|a| a.label.as_str()).collect();
            format!("no agent named `{agent_label}` in multi-module project; \
                available: {}",
                if labels.is_empty() { "(none declared)".to_string() }
                else { labels.join(", ") })
        })?;

    // Inner-ref resolver: see Ref::qualified_in for the rules
    // (cross-module passthrough; same-module mirrors the agent's
    // containing-module prefix; single-file returns the bare label).
    let model_key  = agent.model.qualified_in(&agent.label);
    let prompt_key = agent.system_prompt.qualified_in(&agent.label);

    // ── 2. load system prompt body (pre-loaded by load_project_input).
    let system_prompt = loaded.prompt_bodies.get(&prompt_key)
        .ok_or_else(|| format!(
            "agent `{}` references prompt `{}` which has no pre-loaded body \
             (load_project_input should have read every module's prompts)",
            agent.label, prompt_key))?
        .clone();

    // ── 3. budget config (uses module-qualified model_key for the
    //       resolved binding lookup).
    let budget_cfg: Option<BudgetConfig> = match project.budget.as_ref() {
        Some(b) => {
            let model_id = loaded.resolved.model_bindings.get(&model_key)
                .ok_or_else(|| format!(
                    "model `{model_key}` has no resolved binding in the \
                     merged project (multi-module resolve_multi should \
                     have populated it)"))?;
            let catalog = aislang_catalog::bundled()
                .map_err(|e| format!("catalog load: {e}"))?;
            let entry = catalog.find(model_id)
                .ok_or_else(|| format!(
                    "catalog entry `{}.{}` not found",
                    model_id.provider, model_id.name))?;
            Some(BudgetConfig {
                // W20 Wed follow-up: multi-module path had the
                // per_run_cap / per_day_cap fields SWAPPED — caused
                // the user-reported "daily budget exceeded: $0.10 of
                // $0.1 per-day cap" message when the actual exceed
                // was against the per-RUN cap.  The single-file
                // build_session_for_agent path + the eval path both
                // map them correctly; this was a stale copy-paste.
                per_run_cap:        b.hard_cap_per_run,
                per_day_cap:        b.hard_cap_per_day,
                cost_input_per_1k:  entry.cost_input,
                cost_output_per_1k: entry.cost_output,
                model_id_str:       format!("{}.{}", model_id.provider, model_id.name),
            })
        }
        None => None,
    };
    let tracker = match budget_cfg.as_ref() {
        Some(_) => Some(crate::SpendTracker::open_default(&project.name)
            .map_err(|e| format!("open spend tracker: {e}"))?),
        None => None,
    };

    // ── 4. collect MCP-child specs for the agent's tools (each
    //       tool_ref points at an mcp_server, qualified per the
    //       module-context rule above).
    let mut mcp_specs: Vec<(String, SpawnConfig)> = Vec::new();
    for tool_ref in &agent.tools {
        if tool_ref.kind != aislang_core::ast::RefKind::McpServer {
            return Err(format!(
                "agent `{}.tools` ref `{}` is not an mcp_server ref",
                agent.label, tool_ref.label));
        }
        let tool_key = tool_ref.qualified_in(&agent.label);
        let mcp = project.mcp_servers.iter()
            .find(|m| m.label == tool_key)
            .ok_or_else(|| format!(
                "agent `{}` references mcp_server `{}` which doesn't exist \
                 in the merged project",
                agent.label, tool_key))?;
        // v0.3 Week 17 Tue follow-up #2: resolve relative `mount`
        // paths against the CONTAINING MODULE's dir, NOT the project
        // root.  The author's mental model is "paths in coder.ais
        // are relative to coder.ais's dir" — so `mount =
        // "../knowledge/"` in `agents/coder.ais` resolves to
        // `agents/../knowledge/` = `examples/multi-module-demo/
        // knowledge/`, not `examples/knowledge/`.
        //
        // The containing module is the prefix before the FIRST `.`
        // in the qualified mcp_server label (`coder.docs` → `coder`).
        // Look that up in `loaded.module_dirs` to get the canonical
        // module-source dir + join the relative path against THAT.
        let mount_base: &Path = mcp.label.split_once('.')
            .and_then(|(m, _)| loaded.module_dirs.get(m))
            .map(PathBuf::as_path)
            .unwrap_or(source_dir);
        let mount = mcp.mount.as_ref().map(|p| {
            let resolved = mount_base.join(p);
            std::fs::canonicalize(&resolved).unwrap_or(resolved)
        });
        let env = resolve_mcp_secrets_for_agent(project, mcp, source_dir)?;
        // v0.3 Week 17 Tue follow-up: the merged project's mcp_server
        // labels are module-qualified (`coder.docs`).  Anthropic
        // Messages API requires tool names to match
        // `^[a-zA-Z0-9_-]{1,64}$` — DOTS aren't allowed.  The
        // ToolRegistry composes LLM-visible names as
        // `{server_label}__{mcp_tool_name}`, so a `.`-bearing
        // server_label produces tool names like
        // `coder.docs__read_text_file` which the LLM either rejects
        // outright (strict APIs) or struggles to select (Ollama
        // accepts but tool-call selection degrades — the user's
        // session looped 8× on `get_file_info` instead of picking
        // `read_text_file`).  Replace `.` with `_` so labels stay
        // unique per-module + spec-compliant.
        let llm_safe_label = mcp.label.replace('.', "_");
        mcp_specs.push((llm_safe_label, SpawnConfig {
            image: format_image(&mcp.image),
            args:  Vec::new(),
            mount,
            env,
        }));
    }

    // ── 5. spawn MCP children + list tools (identical to single-file).
    let mut mcp_clients = Vec::with_capacity(mcp_specs.len());
    let mut server_tools: Vec<(String, Vec<crate::ToolSpec>)> = Vec::new();
    for (label, cfg) in &mcp_specs {
        let child = StdioChild::spawn(cfg).await
            .map_err(|e| format!("spawn MCP `{label}`: {e}"))?;
        let tools = child.list_tools().await
            .map_err(|e| format!("list tools `{label}`: {e}"))?;
        server_tools.push((label.clone(), tools));
        mcp_clients.push(child);
    }
    let tool_registry = ToolRegistry::from_servers(&server_tools);

    // ── 6. open Session.  model_label passed to LiteLLM is the
    //       FULL qualified key (`coder.fast` etc) — LiteLLM's
    //       router config from `aislang plan` uses the same key,
    //       so the dispatch stays consistent across plan + chat.
    let litellm_url = crate::litellm::LiteLlmClient::url_from_env();
    let s = Session::open_with_tools(
        &model_key, system_prompt, litellm_url.as_deref(),
        mcp_clients, tool_registry,
    ).map_err(|e| format!("open session: {e}"))?;
    let s = match budget_cfg { Some(b) => s.with_budget(b), None => s };
    let s = match tracker    { Some(t) => s.with_tracker(t), None => s };
    Ok(s)
}

/// Run one eval block end-to-end: open a Session for its agent, load
/// the dataset, score every case via the declared metric, aggregate,
/// shut the Session down cleanly.  Mirrors v0.1's `run_eval_inner`
/// minus the OTel tracing + stderr progress (MCP returns a single
/// EvalReport JSON; the host LLM client owns the progress UX).
async fn run_eval_for_mcp(
    project:    &aislang_core::ast::Project,
    eval:       &aislang_core::ast::Eval,
    source_dir: &Path,
) -> Result<crate::EvalReport, String> {
    // ── 1. resolve which agent this eval runs against (Decision 13) ──
    let agent = match &eval.agent {
        Some(a_ref) => project.agents.iter().find(|a| a.label == a_ref.label)
            .ok_or_else(|| format!(
                "eval `{}` references agent `{}` which doesn't exist",
                eval.label, a_ref.label))?,
        None => match project.agents.len() {
            1 => &project.agents[0],
            0 => return Err(format!(
                "eval `{}` has no `agent = …` and the project declares zero agents",
                eval.label)),
            n => return Err(format!(
                "eval `{}` has no `agent = …` but the project declares {n} agents",
                eval.label)),
        },
    };

    // ── 2. open Session via the shared builder (validates + resolves
    // model/prompt + spawns MCP children + attaches budget/tracker).
    // We need the source to call the builder; rebuild it from the AST
    // would be expensive — pass the source as a parameter would be
    // cleaner.  For now, build the Session directly using the same
    // recipe.  (Refactor: extract `build_session_from_project` to
    // skip re-validating; v0.3.)
    let source_str = serde_json::to_string(&project)
        .map_err(|e| format!("re-serialize project: {e}"))?;
    let _ = source_str;  // unused — we use the AST directly below.

    use crate::{BudgetConfig, SpawnConfig, StdioChild, ToolRegistry};

    let model_label = agent.model.label.clone();
    let prompt_block = project.prompts.iter()
        .find(|p| p.label == agent.system_prompt.label)
        .ok_or_else(|| format!(
            "agent `{}` references prompt `{}` which doesn't exist",
            agent.label, agent.system_prompt.label))?;
    let prompt_path = source_dir.join(&prompt_block.file);
    let lock_path   = source_dir.join("aislang.lock");
    let expected_hash: Option<String> =
        match aislang_resolver::lockfile::read(&lock_path) {
            Ok(Some(lock)) => lock.prompt_hashes.get(&prompt_block.label).cloned(),
            _              => None,
        };
    let system_prompt = crate::load_system_prompt(
        &prompt_path, expected_hash.as_deref(),
    ).map_err(|e| format!("load prompt: {e}"))?;
    // v0.3 Week 18 Tue: same template-rendering hop as the single-
    // file path above.  `agent.label` here is module-qualified
    // (`coder.coder`) post-`resolve_multi`; `prompt_block.label`
    // similarly.  Both flow into the template context as-is.
    let system_prompt = crate::session::render_system_prompt_if_template(
        system_prompt,
        prompt_block.template,
        &agent.label,
        &prompt_block.label,
        &model_label,
    ).map_err(|e| format!("render template: {e}"))?;

    let budget_cfg: Option<BudgetConfig> = match project.budget.as_ref() {
        Some(b) => {
            let resolved = aislang_resolver::resolve_against_bundled_catalog(
                project, source_dir,
            ).map_err(|e| format!("resolve: {e}"))?;
            let model_id = resolved.model_bindings.get(&model_label)
                .ok_or_else(|| format!(
                    "model `{model_label}` has no resolved binding"))?;
            let catalog = aislang_catalog::bundled()
                .map_err(|e| format!("catalog load: {e}"))?;
            let entry = catalog.find(model_id)
                .ok_or_else(|| format!(
                    "catalog entry `{}.{}` not found",
                    model_id.provider, model_id.name))?;
            Some(BudgetConfig {
                per_run_cap:        b.hard_cap_per_run,
                per_day_cap:        b.hard_cap_per_day,
                cost_input_per_1k:  entry.cost_input,
                cost_output_per_1k: entry.cost_output,
                model_id_str:       format!("{}.{}", model_id.provider, model_id.name),
            })
        }
        None => None,
    };
    let tracker = match budget_cfg.as_ref() {
        Some(_) => Some(crate::SpendTracker::open_default(&project.name)
            .map_err(|e| format!("open spend tracker: {e}"))?),
        None => None,
    };

    let mut mcp_specs: Vec<(String, SpawnConfig)> = Vec::new();
    for tool_ref in &agent.tools {
        if tool_ref.kind != aislang_core::ast::RefKind::McpServer {
            return Err(format!("agent `{}.tools` non-mcp_server ref", agent.label));
        }
        let mcp = project.mcp_servers.iter()
            .find(|m| m.label == tool_ref.label)
            .ok_or_else(|| format!("mcp_server `{}` missing", tool_ref.label))?;
        let mount = mcp.mount.as_ref().map(|p| {
            let resolved = source_dir.join(p);
            std::fs::canonicalize(&resolved).unwrap_or(resolved)
        });
        let env = resolve_mcp_secrets_for_agent(project, mcp, source_dir)?;
        mcp_specs.push((mcp.label.clone(), SpawnConfig {
            image: format_image(&mcp.image),
            args:  Vec::new(),
            mount,
            env,
        }));
    }

    let mut mcp_clients = Vec::with_capacity(mcp_specs.len());
    let mut server_tools: Vec<(String, Vec<crate::ToolSpec>)> = Vec::new();
    for (label, cfg) in &mcp_specs {
        let child = StdioChild::spawn(cfg).await
            .map_err(|e| format!("spawn MCP `{label}`: {e}"))?;
        let tools = child.list_tools().await
            .map_err(|e| format!("list tools `{label}`: {e}"))?;
        server_tools.push((label.clone(), tools));
        mcp_clients.push(child);
    }
    let tool_registry = ToolRegistry::from_servers(&server_tools);

    let session = {
        let s = Session::open_with_tools(
            &model_label, system_prompt, None,
            mcp_clients, tool_registry,
        ).map_err(|e| format!("open session: {e}"))?;
        let s = match budget_cfg { Some(b) => s.with_budget(b), None => s };
        match tracker { Some(t) => s.with_tracker(t), None => s }
    };

    // ── 3. load dataset + iterate cases ──
    let dataset_path = source_dir.join(&eval.dataset);
    let dataset = crate::load_dataset(&dataset_path)
        .map_err(|e| format!("load dataset: {e}"))?;

    let mut case_results: Vec<crate::CaseResult> = Vec::with_capacity(dataset.len());
    for case in &dataset {
        let t0 = std::time::Instant::now();
        let (actual, _ok) = match session.turn(&case.input).await {
            Ok(s)  => (s, true),
            Err(e) => (format!("(turn errored: {e})"), false),
        };
        let latency_ms = t0.elapsed().as_millis() as u64;
        let score = crate::metric_score(eval.metric, &case.expected, &actual)
            .map_err(|e| format!("metric scoring: {e}"))?;
        case_results.push(crate::CaseResult {
            id:       case.id.clone(),
            input:    case.input.clone(),
            expected: case.expected.clone(),
            actual,
            score,
            latency_ms,
            trace_url: None,   // MCP layer doesn't open per-case spans (v0.3)
        });
    }

    let scores: Vec<f64> = case_results.iter().map(|c| c.score).collect();
    let aggregate_score = crate::eval_aggregate(eval.aggregator, &scores);
    let pass = aggregate_score >= eval.threshold.ratio;

    session.shutdown().await
        .map_err(|e| format!("shutdown MCP children: {e}"))?;

    let metric_str = serde_json::to_value(eval.metric).ok()
        .and_then(|j| j.as_str().map(str::to_owned))
        .unwrap_or_else(|| "(unserializable)".to_string());
    let aggregator_str = serde_json::to_value(eval.aggregator).ok()
        .and_then(|j| j.as_str().map(str::to_owned))
        .unwrap_or_else(|| "(unserializable)".to_string());

    Ok(crate::EvalReport {
        eval_label:      eval.label.clone(),
        dataset_path:    dataset_path.display().to_string(),
        metric:          metric_str,
        aggregator:      aggregator_str,
        threshold:       eval.threshold.ratio,
        aggregate_score,
        pass,
        trace_url:       None,   // MCP layer doesn't open root span (v0.3)
        cases:           case_results,
    })
}

// ── Wed: shared helpers (apply / destroy / status) ──────────────────

/// Resolve a caller-supplied `source_dir` parameter into a canonical
/// `PathBuf`.  Mirrors what the CLI does at the start of every
/// stateful command: explicit path wins, fall back to cwd, then
/// canonicalize so emitted Compose YAML embeds an unambiguous
/// absolute mount path.
fn resolve_source_dir(arg: Option<&str>) -> Result<PathBuf, rmcp::ErrorData> {
    let raw: PathBuf = match arg {
        Some(s) => PathBuf::from(s),
        None    => std::env::current_dir()
            .map_err(|e| rmcp::ErrorData::internal_error(
                format!("cwd: {e}"), None))?,
    };
    std::fs::canonicalize(&raw).map_err(|e| rmcp::ErrorData::invalid_params(
        format!("source_dir `{}` doesn't exist or unreadable: {e}", raw.display()),
        None))
}

/// Variant of [`resolve_source_dir`] that infers the project
/// directory from an absolute `source_path` when the caller didn't
/// supply an explicit `source_dir`.
///
/// **Why this exists** (week 13 Fri follow-up — surfaced by live
/// Claude Desktop testing): MCP hosts typically pass only
/// `source_path` (the absolute file path of the `.ais` file) and
/// expect aislang to figure out the project directory from there.
/// `source_dir` defaults to the MCP server's cwd, which for
/// Claude-Desktop-spawned mcp-serve is e.g. `/usr/lib/claude-desktop/`.
/// A `.ais` `prompt "system" { file = "./prompts/system.md" }` then
/// resolves to `/usr/lib/claude-desktop/./prompts/system.md` — file
/// not found.  Inferring source_dir from source_path's parent fixes
/// the common case without breaking the explicit form.
///
/// **Resolution policy** (priority order):
///   1. Explicit `source_dir` wins (back-compat + lets callers
///      override the inference for projects with a nested layout).
///   2. Absolute `source_path` → parent dir of `source_path`.  The
///      most common case for MCP clients; matches the v0.1 CLI's
///      semantics where `aislang apply /a/b/c.ais` resolves prompts
///      relative to `/a/b/`.
///   3. Fall back to mcp-serve's cwd.  Same as v0.1 / pre-this-PR
///      behaviour for the legacy in-memory caller that passes
///      neither `source_dir` NOR an absolute `source_path`.
///
/// Returns a canonical absolute path (via `std::fs::canonicalize`),
/// so all downstream path-joining is deterministic regardless of how
/// the caller specified the inputs.
fn resolve_source_dir_or_infer_from_path(
    source_dir_arg:  Option<&str>,
    source_path_arg: Option<&str>,
) -> Result<PathBuf, rmcp::ErrorData> {
    // (1) Explicit source_dir wins.  Same behaviour as
    // resolve_source_dir; we factor through that helper so any future
    // canonicalisation tweaks land in one place.
    if source_dir_arg.is_some() {
        return resolve_source_dir(source_dir_arg);
    }

    // (2) Infer from absolute source_path's parent.  Only kicks in
    // when source_path is set AND absolute — relative source_paths
    // need a cwd to resolve against, so we fall back to (3) for them.
    if let Some(path_str) = source_path_arg {
        let path = std::path::Path::new(path_str);
        if path.is_absolute() {
            if let Some(parent) = path.parent() {
                return std::fs::canonicalize(parent).map_err(|e|
                    rmcp::ErrorData::invalid_params(
                        format!("source_dir inferred from source_path `{}` \
                                 doesn't exist or unreadable: {e}",
                                parent.display()),
                        None));
            }
        }
    }

    // (3) Default: cwd (same as the legacy resolve_source_dir(None)).
    resolve_source_dir(None)
}

/// Resolve a tool call's `.ais` source content from either the
/// inline `source: Option<String>` (legacy, week 12 Tue→Thu) or
/// the new `source_path: Option<String>` (week 13 Thu follow-up).
///
/// **Why the path form was added.** The inline-string form makes
/// the entire .ais file content land in the JSON envelope of every
/// MCP request.  Claude Desktop's tool-call preview / confirmation
/// dialog renders that raw JSON to the user, which turns into a
/// wall of `\n`-escaped text — ugly, unscannable, hard to verify
/// before approving.  The path form keeps the .ais on disk and the
/// envelope tiny:
///
/// ```json
/// { "agent_label": "support",
///   "user_input":  "How can I contact support?",
///   "source_path": "/path/to/support-agent.ais" }
/// ```
///
/// **Resolution policy.** If `source_path` is set, it wins.
/// Relative paths are joined onto the resolved `source_dir` (so the
/// caller can pass `"./support-agent.ais"` and have it Just Work
/// for projects laid out alongside their .ais).  Absolute paths are
/// used as-is.  Missing-file / unreadable → InvalidParams with a
/// clear message naming the resolved absolute path.
///
/// If `source_path` is unset but `source` is, the inline string is
/// returned verbatim (legacy path, kept for back-compat + the rare
/// in-memory caller).
///
/// If both are unset, returns InvalidParams.  Better than silently
/// dispatching to a tool that needs source and getting a less-
/// helpful "parse error: empty input" downstream.
fn resolve_source(
    source:      Option<String>,
    source_path: Option<String>,
    source_dir:  &Path,
) -> Result<String, rmcp::ErrorData> {
    if let Some(path_str) = source_path {
        let path = std::path::Path::new(&path_str);
        let abs  = if path.is_absolute() {
            path.to_path_buf()
        } else {
            source_dir.join(path)
        };
        return std::fs::read_to_string(&abs).map_err(|e|
            rmcp::ErrorData::invalid_params(
                format!("cannot read source_path `{}`: {e}", abs.display()),
                None));
    }
    if let Some(s) = source {
        return Ok(s);
    }
    Err(rmcp::ErrorData::invalid_params(
        "one of `source` (inline `.ais` text body) or `source_path` \
         (filesystem path to an `.ais` file) must be provided.  The path \
         form is recommended — it keeps the MCP confirmation dialog \
         tiny + scannable instead of dumping the entire file as JSON-\
         escaped text.".to_string(),
        None))
}

/// Check whether `docker` is callable.  Same predicate as the CLI's
/// `docker_on_path()` (apply graceful-degrades when absent; destroy
/// hard-errors).
fn docker_on_path() -> bool {
    std::process::Command::new("docker")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Query `docker compose -f <stack> ps --format json` and return one
/// human-readable line per service.  Returns an empty vec when no
/// services are running.  Mirrors the CLI's `docker_compose_ps_json`
/// + `render_service_rows` pipeline but collapsed: we don't need the
/// "drift" accumulator at the MCP layer (drift reporting happens
/// in-text, never as exit codes).
fn docker_compose_ps_lines(stack_path: &std::path::Path)
    -> std::io::Result<Vec<String>>
{
    let out = std::process::Command::new("docker")
        .args(["compose", "-f"])
        .arg(stack_path)
        .args(["ps", "--format", "json"])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "docker compose ps exited {:?}: {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).trim())));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let body = body.trim();
    if body.is_empty() {
        return Ok(Vec::new());
    }
    // Docker Compose 24+ emits NDJSON; older versions emit a JSON
    // array.  Tolerate both — same logic as the CLI.
    #[derive(serde::Deserialize)]
    struct PsRow {
        #[serde(rename = "Name",    alias = "name",    default)]
        name:    String,
        #[serde(rename = "Service", alias = "service", default)]
        service: String,
        #[serde(rename = "State",   alias = "state",   default)]
        state:   String,
        #[serde(rename = "Status",  alias = "status",  default)]
        status:  String,
    }
    let rows: Vec<PsRow> = if body.starts_with('[') {
        serde_json::from_str(body).map_err(std::io::Error::other)?
    } else {
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<PsRow>(l).map_err(std::io::Error::other))
            .collect::<std::io::Result<Vec<_>>>()?
    };
    Ok(rows.into_iter().map(|r| {
        let label = if r.service.is_empty() { r.name } else { r.service };
        let mark  = if matches!(r.state.as_str(), "running" | "Up" | "up") { "✓" } else { "✗" };
        format!("{mark} {label}  {}", r.status)
    }).collect())
}

// ============================================================================
// Diagnostic rendering helper (Tue: shared by `aislang_validate`)
// ============================================================================
//
// Mirrors the CLI's `render_validate_error` (aislang-cli/src/main.rs)
// but writes to a String instead of stderr.  Same source-aware
// rendering pipeline: line:col + 2-3 lines of context + caret.

fn render_validate_error_to_string(
    source: &str,
    err: &aislang_core::ValidateError,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match err {
        aislang_core::ValidateError::Parse(errors) => {
            for e in errors {
                match &e.span {
                    Some(span) => {
                        let _ = write!(out, "{}", aislang_core::diagnostic::render_with_source(
                            source,
                            span.clone(),
                            aislang_core::diagnostic::Severity::Error,
                            &e.message,
                        ));
                    }
                    None => {
                        let _ = writeln!(out, "parse error: {}", e.message);
                    }
                }
            }
        }
        other => {
            let _ = writeln!(out, "{other}");
        }
    }
    out
}

#[tool_handler]
impl ServerHandler for AislangServer {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so we go through
        // `Default::default()` + field updates rather than a struct
        // literal — gives forward-compat against rmcp adding fields
        // we don't yet care about.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .build();
        info.instructions = Some(
            "aiSlang MCP server — exposes the aislang CLI surface as MCP tools. \
             Call `aislang_ping` to verify connectivity.  Substantive tool \
             surface (validate / plan / apply / chat / eval / destroy / status) \
             lands in subsequent v0.2 commits per PLAN-90-days-v0_2.md \
             Phase 1 Week 12.".to_string(),
        );

        // Same non-exhaustive treatment for `Implementation`.
        let mut impl_info = Implementation::default();
        impl_info.name    = "aislang".to_string();
        impl_info.version = env!("CARGO_PKG_VERSION").to_string();
        info.server_info  = impl_info;

        info
    }
}

// ============================================================================
// Stdio entry point
// ============================================================================

/// Run the aiSlang MCP server over stdio.
///
/// Blocks the calling task until the connected client disconnects
/// (EOF on stdin) or the future is cancelled.  Used by
/// `aislang mcp-serve --stdio` (the CLI subcommand registered in
/// `aislang-cli`).
///
/// Stdio is owned by the calling process (typically a parent MCP
/// client — Claude Desktop, Cursor, or a test harness — that has
/// spawned us with piped stdin/stdout).  Never call this from a
/// non-MCP context: it consumes stdin as wire bytes.
pub async fn serve_stdio() -> Result<(), ServerError> {
    let transport = (tokio::io::stdin(), tokio::io::stdout());
    let server    = AislangServer::new();

    // `ServiceExt::serve` runs the initial handshake (capability
    // exchange + protocol-version negotiation), then returns a
    // RunningService whose `waiting` future completes on disconnect.
    let running = server.serve(transport).await
        .map_err(|e| ServerError::Init(e.to_string()))?;

    running.waiting().await
        .map_err(|e| ServerError::Runtime(e.to_string()))?;
    Ok(())
}

// ============================================================================
// HTTP entry point — week 13 Mon
// ============================================================================

/// Run the aiSlang MCP server over Streamable HTTP (the rmcp
/// transport that wraps both POST request/response and SSE
/// streaming under one tower service).
///
/// Unlike [`serve_stdio`], this entry point is multi-client: each
/// MCP `initialize` handshake spins up its own MCP session within
/// the underlying `StreamableHttpService`.  rmcp's
/// `service_factory` callback hands every new session a freshly-
/// cloned [`AislangServer`].
///
/// Crucially, **all clones share the same `sessions` map**
/// (`Arc<RwLock<HashMap<…>>>` inside `AislangServer`), so the
/// `aislang_chat_turn` Session pool is shared across HTTP clients
/// — two MCP clients hitting the same `(source_dir, agent_label)`
/// pair reuse the same in-process conversational Session.  Same
/// posture stdio gives you (only client) but extended to N.
///
/// The caller owns the `TcpListener` so tests can bind to
/// `127.0.0.1:0` and read back the kernel-assigned port; the CLI
/// binds to the user-supplied `--http` socket address.
///
/// The caller also owns the [`tokio_util::sync::CancellationToken`]:
/// firing it triggers axum's graceful-shutdown path (drain active
/// SSE streams, refuse new connections) AND propagates into the
/// per-session rmcp config so in-flight `chat_turn` futures see
/// the cancellation too.  CLI wires Ctrl-C to it; tests fire it
/// from the test body.
///
/// Routing: the MCP traffic is mounted at `/mcp`.  Same path
/// rmcp's own examples use; the CLI banner advertises the full
/// `http://host:port/mcp` URL so users can paste it straight into
/// Claude Desktop / Cursor / curl.
pub async fn serve_http(
    listener: tokio::net::TcpListener,
    ct:       tokio_util::sync::CancellationToken,
) -> Result<(), ServerError> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService,
        session::local::LocalSessionManager,
    };

    // ── Master AislangServer.  The Arc-shared `sessions` map
    // inside it is what survives across per-session clones below.
    let master = AislangServer::new();

    // ── rmcp config.  `stateful_mode = true` (default) is what
    // matches Claude Desktop / Cursor's HTTP-MCP expectations:
    // each client sends an `initialize` and gets a session ID
    // back, then carries it on subsequent requests.  We hand the
    // shutdown-CT child to rmcp so it terminates active sessions
    // when we fire the parent CT in the CLI.
    let cfg = {
        let mut c = StreamableHttpServerConfig::default();
        c.cancellation_token = ct.child_token();
        c
    };

    let service = StreamableHttpService::new(
        // service_factory: called per new MCP session.  Cloning
        // the master gives every session its own ToolRouter
        // (cheap — it's a fn-pointer table) BUT shares the
        // `sessions` Arc<RwLock<…>>, so the Session pool is
        // process-wide across HTTP clients.
        {
            let master = master.clone();
            move || Ok(master.clone())
        },
        Arc::new(LocalSessionManager::default()),
        cfg,
    );

    // Mount the rmcp tower::Service under axum at `/mcp`.  Same
    // path rmcp's own integration tests use; matches the de-facto
    // convention for Streamable-HTTP MCP servers.
    let router = axum::Router::new().nest_service("/mcp", service);

    // axum::serve drives the bound TcpListener.  Graceful
    // shutdown completes after either the cancellation token
    // fires or the listener is otherwise closed.
    let ct_shutdown = ct.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move { ct_shutdown.cancelled_owned().await })
        .await
        .map_err(|e| ServerError::Runtime(format!("axum::serve: {e}")))?;

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs() {
        // Smoke: AislangServer::new() doesn't panic.  Confirms the
        // #[tool_router] macro produces a buildable tool_router field.
        let _s = AislangServer::new();
    }

    #[test]
    fn server_info_advertises_aislang() {
        let s = AislangServer::new();
        let info = s.get_info();
        assert_eq!(info.server_info.name, "aislang",
            "ServerInfo.name pins the wire-visible server identity; \
             Claude Desktop / Cursor display this in their MCP server lists");
        // Version pulled from this crate's Cargo.toml at compile time.
        // Pin the shape (non-empty) rather than the exact value so the
        // test doesn't break on every version bump.
        assert!(!info.server_info.version.is_empty(),
            "ServerInfo.version must be a non-empty string");
        // tools capability advertised — required for clients to even
        // call `tools/list` against us.
        assert!(info.capabilities.tools.is_some(),
            "tools capability must be advertised so clients enumerate our tool list");
    }

    #[test]
    fn server_info_carries_instructions() {
        // The instructions field is what users see in Claude Desktop's
        // server-detail view.  Required to be present + non-trivial so
        // the v0.2 demo doesn't look unfinished.
        let s = AislangServer::new();
        let info = s.get_info();
        let instr = info.instructions.expect("instructions must be set");
        assert!(instr.contains("aiSlang"),
            "instructions should self-identify the server");
        assert!(instr.contains("aislang_ping"),
            "instructions should mention the available ping tool as a starter");
    }

    #[tokio::test]
    async fn ping_returns_pong_text() {
        // Direct invocation — bypasses the MCP wire format.  Pins the
        // tool's *behaviour*; integration tests (later in v0.2) will
        // pin the *wire round-trip* by spawning the binary and using
        // rmcp's client side to call ping.
        let s = AislangServer::new();
        let result = s.ping().await;
        assert_eq!(result, "pong");
    }

    #[test]
    fn default_constructs_same_as_new() {
        // `Default` is required by some rmcp internals + makes the
        // type usable in container types.  Both paths must produce
        // equivalent state (currently: same empty tool_router).
        let _a = AislangServer::default();
        let _b = AislangServer::new();
        // Can't easily Eq-compare because ToolRouter doesn't impl
        // PartialEq; smoke construction is enough at this scope.
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Tue: pure-tier tools
    // ────────────────────────────────────────────────────────────────
    //
    // Helpers to extract the text body from CallToolResult — keeps
    // every test's assertion focused on the body, not on the wire
    // shape.

    fn body_text(result: &rmcp::model::CallToolResult) -> String {
        let frag = result.content.first().expect("at least one content fragment");
        match &frag.raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            other => panic!("expected text content; got: {other:?}"),
        }
    }

    // ── aislang_lang_spec ─────────────────────────────────────────

    #[tokio::test]
    async fn lang_spec_returns_full_spec() {
        let s = AislangServer::new();
        let result = s.lang_spec().await.expect("lang_spec succeeds");
        let text = body_text(&result);
        // Pin a few section markers from LANGUAGE-SPEC-v0.md so any
        // structural change to the spec surfaces here.  Title is
        // "aiSlang Language Specification" (capitalized — verbatim
        // from the .md file's H1).
        assert!(text.contains("aiSlang Language Specification"),
            "spec should self-identify; got prefix: {:?}",
            &text[..200.min(text.len())]);
        assert!(text.contains("## 2. Lexical structure"),
            "spec should carry §2 heading verbatim");
        assert!(text.contains("## 4. Resource types"),
            "spec should carry §4 heading verbatim");
        // Sanity on size: spec is ~25KB.
        assert!(text.len() > 10_000,
            "spec should be more than 10KB; got {} bytes", text.len());
    }

    // ── aislang_examples ──────────────────────────────────────────

    #[tokio::test]
    async fn examples_default_lists_all_four_examples() {
        let s = AislangServer::new();
        let result = s.examples(Parameters(ExamplesParameters::default()))
            .await.expect("examples index succeeds");
        let text = body_text(&result);
        for name in &["minimal", "support-agent", "support-agent-paid", "free-local"] {
            assert!(text.contains(name),
                "index should mention `{name}`; got: {text:?}");
        }
    }

    #[tokio::test]
    async fn examples_minimal_returns_minimal_ais() {
        let s = AislangServer::new();
        let result = s.examples(Parameters(ExamplesParameters {
            name: Some("minimal".to_string()),
        })).await.expect("examples minimal succeeds");
        let text = body_text(&result);
        assert!(text.contains("project"),
            "minimal.ais should start with a project block; got: {text:?}");
    }

    #[tokio::test]
    async fn examples_unknown_name_errors() {
        let s = AislangServer::new();
        let err = s.examples(Parameters(ExamplesParameters {
            name: Some("does-not-exist".to_string()),
        })).await.expect_err("unknown example must error");
        // RmcpError surfaces the cause text in its Display impl.
        let msg = err.to_string();
        assert!(msg.contains("unknown example") || msg.contains("does-not-exist"),
            "error should name the missing example; got: {msg}");
    }

    // ── aislang_describe ──────────────────────────────────────────

    #[tokio::test]
    async fn describe_returns_project_json() {
        // Use the embedded minimal example so the test doesn't depend
        // on the user's working directory.
        const MINIMAL: &str = include_str!("../../../examples/minimal.ais");
        let s = AislangServer::new();
        let result = s.describe(Parameters(SourceOnlyParameters {
            source: Some(MINIMAL.to_string()),
            source_path: None,
        })).await.expect("describe minimal succeeds");
        let text = body_text(&result);
        // Should be valid JSON.
        let v: serde_json::Value = serde_json::from_str(&text)
            .expect("describe output must be valid JSON");
        // Should have the canonical project name from minimal.ais.
        let name = v.get("name").and_then(|n| n.as_str());
        assert_eq!(name, Some("minimal"),
            "describe output should expose project.name = `minimal`");
    }

    #[tokio::test]
    async fn describe_parse_error_propagates_as_rmcp_error() {
        let s = AislangServer::new();
        let err = s.describe(Parameters(SourceOnlyParameters {
            source: Some("this is not valid aiSlang at all".to_string()),
            source_path: None,
        })).await.expect_err("malformed source must error");
        let msg = err.to_string();
        assert!(msg.contains("parse error") || msg.contains("Parse"),
            "error should mention parse failure; got: {msg}");
    }

    // ── aislang_validate ──────────────────────────────────────────

    #[tokio::test]
    async fn validate_ok_summarises_resource_counts() {
        const MINIMAL: &str = include_str!("../../../examples/minimal.ais");
        let s = AislangServer::new();
        let result = s.validate(Parameters(SourceOnlyParameters {
            source: Some(MINIMAL.to_string()),
            source_path: None,
        })).await.expect("validate succeeds at MCP layer");
        let text = body_text(&result);
        assert!(text.starts_with("OK:"),
            "valid source should produce an OK: summary; got: {text:?}");
        assert!(text.contains("project:"),
            "OK summary should name the project block");
        assert!(text.contains("models:"),
            "OK summary should report model count");
        // v0.2 follow-up: every resource type the spec declares
        // appears in the summary, including `mains` (zero for v0.1
        // demo files).  Pinned so future grammar additions don't
        // accidentally drop a kind.
        for kind in &["models:", "prompts:", "agents:", "mcp_servers:",
                      "evals:", "secrets:", "mains:"] {
            assert!(text.contains(kind),
                "OK summary should report `{kind}` count; got: {text:?}");
        }
    }

    #[tokio::test]
    async fn validate_error_renders_source_diagnostic_in_body() {
        // Deliberately malformed: missing the project's opening brace.
        let bad = r#"project "demo"
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
}
"#;
        let s = AislangServer::new();
        // Unlike describe, validate never errors at the MCP layer —
        // diagnostic text comes back as a successful response body
        // so the LLM can read it without an error-branch context loss.
        let result = s.validate(Parameters(SourceOnlyParameters {
            source: Some(bad.to_string()),
            source_path: None,
        })).await.expect("validate never errors at MCP layer for parse failures");
        let text = body_text(&result);
        assert!(!text.starts_with("OK:"),
            "malformed source must NOT produce an OK: summary; got: {text:?}");
        // Source-aware rendering carries the "error at" + line marker.
        assert!(text.contains("error") || text.contains("Parse")
                || text.contains("expected") || text.contains("found"),
            "diagnostic should mention an error / expected / found; got: {text:?}");
    }

    // ── source_path / source resolution policy (week 13 Thu follow-up) ──
    //
    // resolve_source is the shared dispatcher every source-taking
    // tool calls.  These tests pin the four reachable code paths:
    //   - source_path set → file read from disk
    //   - source_path absolute vs relative-to-source_dir
    //   - source set only → returned verbatim (legacy in-memory path)
    //   - both set → source_path wins (more authoritative)
    //   - neither set → InvalidParams with migration hint
    // Plus the unhappy path: source_path points at a missing file →
    // clean error naming the resolved absolute path.

    #[tokio::test]
    async fn source_path_absolute_is_read_verbatim() {
        // Validate via the absolute-path form.  Keeps the test
        // independent of cwd so it works on macOS / Linux CI / NFS
        // mounts alike.
        const MINIMAL: &str = include_str!("../../../examples/minimal.ais");
        let dir = fresh_tempdir("source-path-abs");
        let path = dir.join("minimal.ais");
        std::fs::write(&path, MINIMAL).unwrap();

        let s = AislangServer::new();
        let result = s.validate(Parameters(SourceOnlyParameters {
            source:      None,
            source_path: Some(path.to_string_lossy().to_string()),
        })).await.expect("validate via source_path succeeds");
        let text = body_text(&result);
        assert!(text.starts_with("OK:"),
            "validate(source_path=absolute) should mirror inline path; got: {text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn source_path_relative_resolves_against_source_dir() {
        // plan with source_path = "./project.ais" + source_dir = dir
        // should read dir.join("project.ais").  Mirrors how Claude
        // Desktop / Cursor users will typically configure paths.
        let dir = fresh_tempdir("source-path-rel");
        std::fs::write(dir.join("system.md"),
            "you are a helpful test agent").unwrap();
        std::fs::write(dir.join("project.ais"), PROJECT_WITH_PROMPT).unwrap();

        let s = AislangServer::new();
        let result = s.plan(Parameters(PlanParameters {
            source:      None,
            source_path: Some("./project.ais".to_string()),
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect("plan via relative source_path resolves against source_dir");
        let text = body_text(&result);
        assert!(text.contains("── docker-compose.yml ──"),
            "plan via source_path should produce the full plan output");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn source_path_wins_when_both_set() {
        // resolve_source: source_path is more authoritative because
        // the user explicitly named a file rather than embedding
        // content — pin the precedence ordering.  Test by having
        // source contain garbage AND source_path point at a valid
        // file; if source were used, validate would surface "error
        // at line N"; if source_path wins, we get "OK:".
        const MINIMAL: &str = include_str!("../../../examples/minimal.ais");
        let dir = fresh_tempdir("source-path-wins");
        let path = dir.join("minimal.ais");
        std::fs::write(&path, MINIMAL).unwrap();

        let s = AislangServer::new();
        let result = s.validate(Parameters(SourceOnlyParameters {
            source:      Some("@@@ this is garbage @@@".to_string()),
            source_path: Some(path.to_string_lossy().to_string()),
        })).await.expect("validate succeeds at the MCP layer");
        let text = body_text(&result);
        assert!(text.starts_with("OK:"),
            "source_path must win over inline source; got: {text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn source_path_missing_file_returns_invalid_params() {
        // Unhappy path: source_path points at a nonexistent file →
        // InvalidParams naming the resolved absolute path.  Drives
        // the user to either fix the path or write the file first.
        let s = AislangServer::new();
        let err = s.validate(Parameters(SourceOnlyParameters {
            source:      None,
            source_path: Some("/tmp/aislang-does-not-exist-7f.ais".to_string()),
        })).await.expect_err("missing source_path file must error");
        let msg = err.to_string();
        assert!(msg.contains("cannot read source_path"),
            "error should call out the missing file; got: {msg}");
        assert!(msg.contains("/tmp/aislang-does-not-exist-7f.ais"),
            "error should name the resolved absolute path; got: {msg}");
    }

    #[tokio::test]
    async fn neither_source_nor_source_path_returns_invalid_params() {
        // Empty params → InvalidParams with a migration-hint message
        // pointing at the recommended source_path form.
        let s = AislangServer::new();
        let err = s.validate(Parameters(SourceOnlyParameters {
            source:      None,
            source_path: None,
        })).await.expect_err("neither set must error");
        let msg = err.to_string();
        assert!(msg.contains("`source`") && msg.contains("`source_path`"),
            "error should name both fields; got: {msg}");
        assert!(msg.contains("recommended"),
            "error should nudge users toward source_path; got: {msg}");
    }

    // ── source_dir inference from source_path (week 13 Fri follow-up) ──
    //
    // Surfaced by live Claude Desktop test: MCP hosts typically pass
    // only `source_path` (absolute) and omit `source_dir`.  Without
    // inference, source_dir defaults to mcp-serve's cwd
    // (`/usr/lib/claude-desktop/` when spawned by Claude Desktop), and
    // the .ais's `prompt "system" { file = "./prompts/system.md" }`
    // resolves to a nonexistent path.  Fix: when source_dir is unset
    // AND source_path is absolute, infer source_dir from
    // source_path's parent.

    #[tokio::test]
    async fn plan_infers_source_dir_from_absolute_source_path() {
        // The bug case: source_path=/tmp/.../proj.ais, source_dir=None,
        // prompt path = "./system.md".  Should resolve the prompt at
        // /tmp/.../system.md (NOT cwd/system.md).
        let dir = fresh_tempdir("infer-from-absolute");
        std::fs::write(dir.join("system.md"),
            "you are helpful").unwrap();
        let ais_path = dir.join("project.ais");
        std::fs::write(&ais_path, PROJECT_WITH_PROMPT).unwrap();

        let s = AislangServer::new();
        let result = s.plan(Parameters(PlanParameters {
            source:      None,
            source_path: Some(ais_path.to_string_lossy().to_string()),
            source_dir:  None,   // ← the key — unset, must be inferred
        })).await.expect("plan succeeds via source_path-inferred source_dir");
        let text = body_text(&result);
        assert!(text.contains("── docker-compose.yml ──"),
            "plan should produce its standard output via inferred source_dir; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plan_explicit_source_dir_wins_over_inference() {
        // Defence-in-depth: when source_dir IS provided, it must
        // override the inference path.  Catches a future regression
        // where someone "simplifies" the helper by always inferring.
        let proj_dir   = fresh_tempdir("infer-override-proj");
        let prompt_dir = fresh_tempdir("infer-override-prompts");
        std::fs::write(prompt_dir.join("system.md"),
            "you are helpful").unwrap();
        let ais_path = proj_dir.join("project.ais");
        std::fs::write(&ais_path, PROJECT_WITH_PROMPT).unwrap();

        let s = AislangServer::new();
        // With ONLY source_path: inference says prompt lives at
        // proj_dir/system.md — which doesn't exist → fails.
        let r_no_dir = s.plan(Parameters(PlanParameters {
            source:      None,
            source_path: Some(ais_path.to_string_lossy().to_string()),
            source_dir:  None,
        })).await;
        assert!(r_no_dir.is_err(),
            "plan should fail when inferred source_dir lacks the prompt file");

        // With explicit source_dir = prompt_dir: caller's path wins,
        // prompt is found, plan succeeds.
        let r_with_dir = s.plan(Parameters(PlanParameters {
            source:      None,
            source_path: Some(ais_path.to_string_lossy().to_string()),
            source_dir:  Some(prompt_dir.to_string_lossy().to_string()),
        })).await.expect("plan succeeds when explicit source_dir overrides inference");
        let text = body_text(&r_with_dir);
        assert!(text.contains("── docker-compose.yml ──"),
            "plan should succeed via explicit source_dir; got: {text}");

        let _ = std::fs::remove_dir_all(&proj_dir);
        let _ = std::fs::remove_dir_all(&prompt_dir);
    }

    #[tokio::test]
    async fn relative_source_path_does_not_trigger_inference() {
        // Edge case: source_path is RELATIVE (not absolute).  The
        // inference path uses `Path::is_absolute()`, so relative
        // source_paths should fall through to the cwd default.
        //
        // We can't easily test the "cwd default" branch directly
        // because tests share cwd.  Instead, verify a relative
        // source_path with NO source_dir RAISES the same error shape
        // it always did when the file isn't reachable from cwd — i.e.,
        // the inference path didn't accidentally interpret the
        // relative source_path's parent (which is "") as a dir.
        let s = AislangServer::new();
        let err = s.validate(Parameters(SourceOnlyParameters {
            source:      None,
            source_path: Some("./does-not-exist-7f.ais".to_string()),
        })).await.expect_err("missing relative source_path must error");
        let msg = err.to_string();
        assert!(msg.contains("cannot read source_path"),
            "error should be the resolve_source missing-file error; got: {msg}");
        // The inference SHOULD NOT have produced an absolute dir for
        // an empty parent.  If it had, we'd see a different error
        // shape (source_dir-related rather than source_path-related).
        assert!(!msg.contains("source_dir inferred"),
            "relative source_path must NOT trigger source_dir inference; got: {msg}");
    }

    // ── aislang_plan ──────────────────────────────────────────────

    // Project source that DOES reference a prompt file — needed for
    // plan tests to exercise the resolver's prompt-hashing path (the
    // bundled minimal.ais has no prompts at all).
    const PROJECT_WITH_PROMPT: &str = r#"project "demo" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "system" {
    file    = "./system.md"
    version = "1.0.0"
  }
  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = []
  }
}
"#;

    fn fresh_tempdir(suffix: &str) -> std::path::PathBuf {
        // Per-process atomic counter — same uniqueness pattern as
        // spend/state/secrets test helpers (macOS µs-clock fix).
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "aislang-mcp-{}-{}-{}-{suffix}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn plan_succeeds_with_materialised_prompt_file() {
        // The `plan` flow hashes prompt files referenced by the
        // source — we materialise system.md at a temp dir and pass
        // it as source_dir.
        let dir = fresh_tempdir("plan-ok");
        std::fs::write(dir.join("system.md"),
            "you are a helpful test agent").unwrap();

        let s = AislangServer::new();
        let result = s.plan(Parameters(PlanParameters {
            source:      Some(PROJECT_WITH_PROMPT.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect("plan succeeds with materialised prompt");
        let text = body_text(&result);
        assert!(text.contains("── Resolved IR (JSON) ──"),
            "plan output should include the resolved IR section");
        assert!(text.contains("── docker-compose.yml ──"),
            "plan output should include the compose YAML section");
        assert!(text.contains("── litellm_config.yaml ──"),
            "plan output should include the litellm config section");
        assert!(text.contains("── .env.example ──"),
            "plan output should include the .env.example section");
        // Sanity: resolved IR is JSON and includes a model_bindings
        // entry for `fast` (pinned to ollama).
        assert!(text.contains("model_bindings"),
            "resolved IR should expose model_bindings");
        assert!(text.contains("ollama") && text.contains("llama_3_1_8b"),
            "resolved bindings should pin to the ollama llama model");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plan_missing_prompt_file_returns_resolve_error() {
        // Source references ./system.md but we don't write it — resolve
        // fails at the hash step.
        let dir = fresh_tempdir("plan-missing");

        let s = AislangServer::new();
        let err = s.plan(Parameters(PlanParameters {
            source:      Some(PROJECT_WITH_PROMPT.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect_err("plan should fail when prompt file missing");
        let msg = err.to_string();
        assert!(msg.contains("resolve") || msg.contains("prompt")
                || msg.contains("system.md") || msg.contains("hash"),
            "error should mention resolve / prompt / hash / system.md; got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn plan_parse_error_short_circuits_before_resolve() {
        let s = AislangServer::new();
        let err = s.plan(Parameters(PlanParameters {
            source:      Some("this is not valid".to_string()),
            source_path: None,
            source_dir:  None,
        })).await.expect_err("malformed source must error");
        let msg = err.to_string();
        assert!(msg.contains("parse error"),
            "should fail with parse error before reaching resolve; got: {msg}");
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Wed: stateful-tier tools
    // ────────────────────────────────────────────────────────────────
    //
    // These exercise the apply/destroy/status code paths *without*
    // requiring Docker — we use PATH-clobbering to force the
    // graceful-degrade branches.  Tests that need a real docker
    // shellout are deferred to integration tests (cli_mcp_serve.rs)
    // where the binary is actually spawned.

    /// Same shape as `PROJECT_WITH_PROMPT` from the plan tests but
    /// kept inline so the Wed test block can be moved later without
    /// dragging the Tue constant with it.
    const APPLY_TEST_SOURCE: &str = r#"project "demo" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "system" {
    file    = "./system.md"
    version = "1.0.0"
  }
  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = []
  }
}
"#;

    /// PATH clobber: force `docker_on_path()` to return false.  Used
    /// in tests that exercise the no-docker graceful-degrade paths
    /// without needing a real docker daemon.
    ///
    /// **Serialization** (Phase 2 Week 14 Wed fix-up): `PATH` is a
    /// process-global env var; cargo runs `#[tokio::test]` tests in
    /// parallel by default; without a mutex, one test's `Drop`-time
    /// `set_var("PATH", prev)` could restore `PATH` to a docker-
    /// containing value WHILE a sibling test's `apply()` call was
    /// running, letting `docker compose up -d` actually succeed —
    /// state.json then existed when the no-docker test asserted it
    /// shouldn't.  Surfaced on GitHub CI as the flaky
    /// `apply_writes_artifacts_then_graceful_degrade_without_docker`
    /// failure.  Local runs usually serialized far enough by chance
    /// to mask the race; the cloud runner's higher contention exposed
    /// it.
    ///
    /// Fix: every `PathGuard::no_docker()` acquires the process-wide
    /// `PATH_LOCK` Mutex BEFORE mutating PATH and holds it until
    /// drop.  All PathGuard-using tests serialize against each
    /// other; tests that don't touch PATH stay parallel.  Mutex
    /// poisoning is tolerated (.unwrap_or_else(into_inner)) because
    /// a panicking test still wants the next test to make progress
    /// with a fresh PATH state.
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct PathGuard {
        // `MutexGuard<'static, ()>` — held until the test function
        // exits, so the next `PathGuard::no_docker()` blocks until
        // this one's `Drop::drop` runs the PATH restoration first.
        _lock: std::sync::MutexGuard<'static, ()>,
        prev:  Option<std::ffi::OsString>,
    }
    impl PathGuard {
        fn no_docker() -> Self {
            // Tolerate Mutex poisoning — a panicking test still
            // wants the next test to make progress with a sensible
            // PATH state, not deadlock.
            let lock = PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("PATH");
            // SAFETY: lock acquired; no other PathGuard test can
            // observe or mutate PATH until our Drop releases it.
            unsafe { std::env::set_var("PATH", "/nonexistent-aislang-mcp-test-path"); }
            Self { _lock: lock, prev }
        }
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            // SAFETY: lock still held; restore PATH atomically with
            // respect to other PathGuard-using tests.
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var("PATH", v),
                    None    => std::env::remove_var("PATH"),
                }
            }
            // `_lock` drops after this; the next PathGuard
            // observer sees PATH in its pre-test state.
        }
    }

    // ── aislang_apply ──────────────────────────────────────────────

    #[tokio::test]
    async fn apply_writes_artifacts_then_graceful_degrade_without_docker() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("apply-no-docker");
        std::fs::write(dir.join("system.md"), "test prompt").unwrap();

        let s = AislangServer::new();
        let result = s.apply(Parameters(ApplyParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect("apply degrades cleanly without docker");
        let text = body_text(&result);

        // All four artifacts written to disk regardless of docker.
        assert!(dir.join("stack.yml").exists(),
            "stack.yml must be written even without docker");
        assert!(dir.join("litellm_config.yaml").exists(),
            "litellm_config.yaml must be written");
        assert!(dir.join(".env.example").exists(),
            ".env.example must be written");
        assert!(dir.join("aislang.lock").exists(),
            "aislang.lock must be written");
        // State file is NOT written when stack failed to come up
        // (no docker == no `docker compose up -d`).  Same as v0.1 CLI.
        assert!(!dir.join(".aislang/state.json").exists(),
            "state.json must NOT be written when stack didn't come up");

        // Response body mentions the manual fallback command.
        assert!(text.contains("docker not on PATH"),
            "response should call out docker absence; got: {text}");
        assert!(text.contains("docker compose -f") && text.contains("up -d"),
            "response should print the manual command; got: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn apply_parse_error_returns_invalid_params() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("apply-bad-source");
        let s = AislangServer::new();
        let err = s.apply(Parameters(ApplyParameters {
            source:      Some("this is not aislang".to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect_err("malformed source must error");
        assert!(err.to_string().contains("parse error"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn apply_resolve_error_when_prompt_file_missing() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("apply-no-prompt");
        // Deliberately DO NOT write system.md.
        let s = AislangServer::new();
        let err = s.apply(Parameters(ApplyParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
        })).await.expect_err("resolve must fail when prompt missing");
        let msg = err.to_string();
        assert!(msg.contains("resolve") || msg.contains("hash") || msg.contains("system.md"),
            "error should mention resolve/hash/system.md; got: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── aislang_destroy ────────────────────────────────────────────

    #[tokio::test]
    async fn destroy_returns_nothing_to_destroy_when_no_state_no_stack() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("destroy-clean-dir");
        let s = AislangServer::new();
        let result = s.destroy(Parameters(DestroyParameters {
            source_dir: Some(dir.to_string_lossy().to_string()),
            force:      None,
        })).await.expect("destroy on clean dir succeeds");
        let text = body_text(&result);
        assert!(text.contains("nothing to destroy"),
            "should say `nothing to destroy`; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn destroy_idempotent_when_state_shows_torn_down() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("destroy-already-torn-down");
        // Seed a torn-down state file directly.
        let state_path = crate::state_path_in(&dir);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let mut torn = crate::new_applied_state(
            "demo",
            "deadbeef".to_string(),
            "2026-05-25",
            dir.join("stack.yml"),
        );
        torn.applied_at = None;
        crate::write_state_atomic(&state_path, &torn).unwrap();

        let s = AislangServer::new();
        let result = s.destroy(Parameters(DestroyParameters {
            source_dir: Some(dir.to_string_lossy().to_string()),
            force:      None,
        })).await.expect("destroy on already-torn-down state succeeds");
        let text = body_text(&result);
        assert!(text.contains("already torn down"),
            "should mention already-torn-down state; got: {text}");
        // Note: response also hints about force=true.
        assert!(text.contains("force"),
            "should mention the force option as a workaround; got: {text}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn destroy_no_stack_yml_and_no_force_is_clean_exit() {
        // No state file + no stack.yml is the "nothing was ever
        // applied here" case — clean success body, no error.
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("destroy-no-stack");
        let s = AislangServer::new();
        let result = s.destroy(Parameters(DestroyParameters {
            source_dir: Some(dir.to_string_lossy().to_string()),
            force:      None,
        })).await.expect("destroy on empty dir is clean exit");
        let text = body_text(&result);
        assert!(text.contains("nothing to destroy"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── aislang_status ─────────────────────────────────────────────

    #[tokio::test]
    async fn status_not_applied_when_no_state_file() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("status-no-state");
        let s = AislangServer::new();
        let result = s.status(Parameters(StatusParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
            spend:       None,
            model_spend: None,
        })).await.expect("status with no state succeeds");
        let text = body_text(&result);
        assert!(text.contains("project:  demo"),
            "header should print project name");
        assert!(text.contains("not applied"),
            "stack section should say not applied; got: {text}");
        assert!(text.contains("aislang_apply") || text.contains("hint"),
            "should hint at the apply tool; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_reports_torn_down_when_applied_at_null() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("status-torn-down");
        let state_path = crate::state_path_in(&dir);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let mut torn = crate::new_applied_state(
            "demo", "deadbeef".to_string(), "2026-05-25",
            dir.join("stack.yml"),
        );
        torn.applied_at = None;
        crate::write_state_atomic(&state_path, &torn).unwrap();

        let s = AislangServer::new();
        let result = s.status(Parameters(StatusParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
            spend:       None,
            model_spend: None,
        })).await.expect("status succeeds on torn-down state");
        let text = body_text(&result);
        assert!(text.contains("torn down"),
            "stack section should say torn down; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_with_spend_flag_appends_spend_section() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("status-spend");
        let s = AislangServer::new();
        let result = s.status(Parameters(StatusParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
            spend:       Some(true),
            model_spend: None,
        })).await.expect("status with spend flag succeeds");
        let text = body_text(&result);
        assert!(text.contains("spend ("),
            "spend section should be present; got: {text}");
        // Project has $1/day cap → expect cap-utilization line too.
        assert!(text.contains("cap:") || text.contains("total:"),
            "spend section should include total + cap; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_parse_error_returns_invalid_params() {
        let _path = PathGuard::no_docker();
        let s = AislangServer::new();
        let err = s.status(Parameters(StatusParameters {
            source:      Some("not aislang".to_string()),
            source_path: None,
            source_dir:  None,
            spend:       None,
            model_spend: None,
        })).await.expect_err("malformed source must error");
        assert!(err.to_string().contains("parse error"));
    }

    #[tokio::test]
    async fn status_with_model_spend_empty_filter_shows_all_models() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("status-model-spend-all");
        let s = AislangServer::new();
        let result = s.status(Parameters(StatusParameters {
            source:      Some(APPLY_TEST_SOURCE.to_string()),
            source_path: None,
            source_dir:  Some(dir.to_string_lossy().to_string()),
            spend:       None,
            // Empty string filter = "all models" (matches CLI --model-spend semantics).
            model_spend: Some(String::new()),
        })).await.expect("status with empty model_spend filter succeeds");
        let text = body_text(&result);
        assert!(text.contains("spend by model"),
            "should show by-model breakdown header; got: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ────────────────────────────────────────────────────────────────
    // Week-12 Thu: conversation-tier tools
    // ────────────────────────────────────────────────────────────────
    //
    // We can't drive a real Session::turn at the unit-test level
    // without standing up a wiremock LiteLLM (v0.1 does this in
    // session.rs but it's a substantial harness).  Thu's unit tests
    // therefore focus on:
    //   1. Tool annotations + parameter shapes (compile-time pinned).
    //   2. SessionKey semantics — equality + cache lookup.
    //   3. Error-path branches that don't require an LLM (parse
    //      errors, missing agent, missing prompt file, no eval blocks).
    // Full chat_turn happy path is exercised live via Claude Desktop
    // after merge (DoD note).

    /// Same project shape as Wed's APPLY_TEST_SOURCE but with an
    /// agent the chat_turn caller can ask for.
    const CHAT_TURN_TEST_SOURCE: &str = r#"project "demo" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "system" {
    file    = "./system.md"
    version = "1.0.0"
  }
  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = []
  }
}
"#;

    /// Project with multiple agents but no eval block — useful for
    /// the "no eval declared" error path.
    const NO_EVAL_SOURCE: &str = r#"project "noeval" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
}
"#;

    // ── SessionKey unit tests ────────────────────────────────────

    #[test]
    fn session_key_equality_pins_dir_plus_label() {
        let a = SessionKey {
            source_dir:  PathBuf::from("/tmp/x"),
            agent_label: "support".to_string(),
        };
        let b = SessionKey {
            source_dir:  PathBuf::from("/tmp/x"),
            agent_label: "support".to_string(),
        };
        let c = SessionKey {
            source_dir:  PathBuf::from("/tmp/y"),
            agent_label: "support".to_string(),
        };
        let d = SessionKey {
            source_dir:  PathBuf::from("/tmp/x"),
            agent_label: "admin".to_string(),
        };
        assert_eq!(a, b);
        assert_ne!(a, c, "different source_dir → different key");
        assert_ne!(a, d, "different agent_label → different key");
    }

    // ── run_pipeline orchestration helpers ───────────────────────
    // The tool's happy path needs a live LiteLLM (exercised via Claude
    // Desktop like chat_turn); these cover the pure logic underneath.

    #[test]
    fn pipeline_step_labels_qualify_by_module_then_pipeline_prefix() {
        use aislang_core::ast::{Pipeline, Ref, RefKind};
        let mk = |module: Option<&str>, label: &str| Ref {
            kind:   RefKind::Agent,
            label:  label.to_string(),
            module: module.map(str::to_string),
        };
        // Multi-module pipeline `coder.coder_review_cycle`: a cross-module
        // step keeps its module; a same-module step inherits the pipeline's
        // `coder.` prefix.
        let p = Pipeline {
            label:  "coder.coder_review_cycle".to_string(),
            steps:  vec![mk(Some("coder"), "coder"),
                         mk(Some("code_reviewer"), "code_reviewer"),
                         mk(None, "tester")],
            input:  None,
            output: None,
        };
        assert_eq!(qualify_pipeline_step_labels(&p), vec![
            "coder.coder".to_string(),
            "code_reviewer.code_reviewer".to_string(),
            "coder.tester".to_string(),
        ]);
        // Single-file pipeline (bare label) → bare step labels.
        let s = Pipeline {
            label:  "p".to_string(),
            steps:  vec![mk(None, "a"), mk(None, "b")],
            input:  None,
            output: None,
        };
        assert_eq!(qualify_pipeline_step_labels(&s),
            vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn loop_condition_honours_the_ais_template() {
        // The orchestrator.ais `review_cycle` condition: keep looping while
        // the reply does NOT start with ACCEPT.
        let cond = "{{ output | startswith('ACCEPT') | not }}";
        // Reviewer NOT done → condition true → keep looping.
        assert_eq!(eval_loop_condition(cond, "Needs revision: rename field").unwrap(), true);
        // Reviewer accepted → condition false → stop.
        assert_eq!(eval_loop_condition(cond, "ACCEPT — ship it").unwrap(), false);
        // Other filters wire up too.
        assert_eq!(eval_loop_condition("{{ output | contains('TODO') }}", "has TODO left").unwrap(), true);
        // An unknown filter / bad template errors (caller falls back to ACCEPT).
        assert!(eval_loop_condition("{{ output | no_such_filter }}", "x").is_err());
    }

    #[test]
    fn accept_verdict_detection_matches_cli() {
        assert!(is_accept_verdict("ACCEPT — looks good"));
        assert!(is_accept_verdict("  accept\n"));
        assert!(is_accept_verdict("ACCEPTED, ship it"));
        assert!(is_accept_verdict(r#"{"verdict":"ACCEPT"}"#));
        assert!(!is_accept_verdict("Needs revision: rename the field"));
        assert!(!is_accept_verdict(r#"{"verdict":"reject"}"#));
        assert!(!is_accept_verdict("the reviewer may ACCEPT next round"),
            "a non-leading ACCEPT must NOT count as a verdict");
    }

    // ── chat_turn parameter-validation paths ─────────────────────
    //
    // Week 13 Wed: chat_turn grew Meta + Peer<RoleServer> extractor
    // parameters (for progress-notification streaming).  Both come
    // from the live MCP wire and don't have easy in-test constructors,
    // so the early-error tests below dial `build_session_for_agent`
    // directly — same code path chat_turn invokes for these paths,
    // tested at the lower layer to dodge the Peer plumbing.

    // Session doesn't impl Debug; `expect_err` requires Ok-arm Debug.
    // Use a tiny adapter that maps Ok(_) → panic without touching the
    // Session value.  Reused by all three chat_turn early-bail tests.
    fn err_or_panic<T>(r: Result<T, String>, label: &str) -> String {
        match r {
            Ok(_)    => panic!("expected error in `{label}`; got Ok"),
            Err(msg) => msg,
        }
    }

    #[tokio::test]
    async fn chat_turn_parse_error_returns_invalid_params() {
        let _path = PathGuard::no_docker();
        // build_session_for_agent calls `aislang_core::validate`
        // first thing; bad source → "parse error: …" String.
        let r = build_session_for_agent(
            "not aislang",
            std::path::Path::new("."),
            "support",
        ).await;
        let err = err_or_panic(r, "parse error");
        assert!(err.contains("parse error"),
            "should mention parse error; got: {err}");
    }

    #[tokio::test]
    async fn chat_turn_unknown_agent_returns_invalid_params() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("chat-unknown-agent");
        // Don't write system.md — we expect the agent-lookup to fail
        // BEFORE the prompt-file load.
        let r = build_session_for_agent(
            CHAT_TURN_TEST_SOURCE,
            &dir,
            "nope",
        ).await;
        let err = err_or_panic(r, "unknown agent");
        assert!(err.contains("no agent named") || err.contains("`nope`"),
            "should name the missing agent + list available; got: {err}");
        // Available list should mention the actual agent.
        assert!(err.contains("support"),
            "should mention available agent names; got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_turn_missing_prompt_file_errors() {
        let _path = PathGuard::no_docker();
        let dir = fresh_tempdir("chat-no-prompt");
        // Source references system.md but we don't write it.  The
        // build_session_for_agent path tries to load the prompt and
        // fails.
        let r = build_session_for_agent(
            CHAT_TURN_TEST_SOURCE,
            &dir,
            "support",
        ).await;
        let err = err_or_panic(r, "missing prompt");
        assert!(err.contains("prompt") || err.contains("system.md")
                || err.contains("load"),
            "should mention prompt loading failure; got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── eval parameter-validation paths ──────────────────────────

    #[tokio::test]
    async fn eval_parse_error_returns_invalid_params() {
        let _path = PathGuard::no_docker();
        let s = AislangServer::new();
        let err = s.eval(Parameters(EvalParameters {
            source:      Some("not aislang".to_string()),
            source_path: None,
            source_dir:  None,
            eval_label:  None,
        })).await.expect_err("parse error must bubble");
        assert!(err.to_string().contains("parse error"));
    }

    #[tokio::test]
    async fn eval_no_eval_blocks_errors() {
        let _path = PathGuard::no_docker();
        let s = AislangServer::new();
        let err = s.eval(Parameters(EvalParameters {
            source:      Some(NO_EVAL_SOURCE.to_string()),
            source_path: None,
            source_dir:  None,
            eval_label:  None,
        })).await.expect_err("no eval blocks → error");
        let msg = err.to_string();
        assert!(msg.contains("no `eval` blocks"),
            "should mention missing eval blocks; got: {msg}");
    }

    #[tokio::test]
    async fn eval_unknown_label_lists_available() {
        let _path = PathGuard::no_docker();
        // Use the support-agent example: it has one `eval` block
        // ("factual_accuracy").
        const SOURCE: &str = include_str!("../../../examples/support-agent.ais");
        let s = AislangServer::new();
        let err = s.eval(Parameters(EvalParameters {
            source:      Some(SOURCE.to_string()),
            source_path: None,
            source_dir:  None,
            eval_label:  Some("does_not_exist".to_string()),
        })).await.expect_err("unknown eval label must error");
        let msg = err.to_string();
        assert!(msg.contains("no eval named") && msg.contains("does_not_exist"),
            "should name the missing label; got: {msg}");
        assert!(msg.contains("factual_accuracy"),
            "should list `factual_accuracy` as available; got: {msg}");
    }

    #[tokio::test]
    async fn build_session_for_agent_missing_agent_errors() {
        // Direct unit test of the helper (not via the tool wrapper)
        // so we exercise the "available agents" listing logic.
        // Session doesn't impl Debug (rmcp client types nested
        // inside) so we can't use expect_err — match manually.
        let dir = fresh_tempdir("build-session-unknown");
        let err = match build_session_for_agent(
            CHAT_TURN_TEST_SOURCE, &dir, "missing_agent_label"
        ).await {
            Ok(_)  => panic!("expected error for unknown agent"),
            Err(e) => e,
        };
        assert!(err.contains("no agent named"));
        assert!(err.contains("support"),
            "should list the actual agent in the suggestion");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
