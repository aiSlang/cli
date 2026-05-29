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
    ErrorData as RmcpError, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, Content,
        Implementation, ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;
use thiserror::Error;

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
#[derive(Debug, Clone)]
pub struct AislangServer {
    // Read indirectly by the `#[tool_handler]` macro's generated
    // `call_tool` / `list_tools` dispatcher.  Dead-code analysis
    // doesn't see through the macro, so we explicitly allow.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
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
    /// `impl` block.
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
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
        let (project, _warnings) = aislang_core::validate(&p.source)
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
        let body = match aislang_core::validate(&p.source) {
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
                     secrets:     {}",
                    project.name,
                    project.models.len(),
                    project.prompts.len(),
                    project.agents.len(),
                    project.mcp_servers.len(),
                    project.evals.len(),
                    project.secrets.len(),
                );
                if !warnings.is_empty() {
                    out.push_str("\n\nwarnings:");
                    for w in &warnings {
                        out.push_str(&format!("\n  - {}", w.message));
                    }
                }
                out
            }
            Err(err) => render_validate_error_to_string(&p.source, &err),
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
        // Stage 1: parse + typecheck.
        let (project, _warnings) = aislang_core::validate(&p.source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;

        // Stage 2: catalog ID check.
        aislang_resolver::check_against_bundled_catalog(&project)
            .map_err(|e| RmcpError::invalid_params(
                format!("catalog error: {e}"), None))?;

        // Stage 3: resolve.  source_dir defaults to the server's CWD,
        // matching the CLI's `aislang plan` behaviour when called
        // without --output-dir.
        let source_dir: PathBuf = match p.source_dir.as_deref() {
            Some(s) => PathBuf::from(s),
            None    => std::env::current_dir()
                .map_err(|e| RmcpError::internal_error(
                    format!("cwd: {e}"), None))?,
        };
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
            comes back when `docker compose up -d` returns.",
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
        let source_dir = resolve_source_dir(p.source_dir.as_deref())?;

        // ── Stage 1: full plan pipeline (validate → catalog → resolve → emit)
        let (project, _warnings) = aislang_core::validate(&p.source)
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
        let lock = aislang_resolver::Lockfile::build(&p.source, &resolved);
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
        let source_hash = crate::hex_sha256(p.source.as_bytes());
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

        let (project, _warnings) = aislang_core::validate(&p.source)
            .map_err(|e| RmcpError::invalid_params(
                format!("parse error: {e}"), None))?;

        let source_dir = resolve_source_dir(p.source_dir.as_deref())?;
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
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct SourceOnlyParameters {
    /// The complete `.ais` source file as a UTF-8 text body.  Must
    /// parse + typecheck for `describe`; can be malformed for
    /// `validate` (the diagnostic is the point).
    pub source: String,
}

/// `plan` extends `SourceOnlyParameters` with an optional
/// `source_dir` so prompt-file paths can be resolved against the
/// caller's project directory.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
pub struct PlanParameters {
    /// The complete `.ais` source file as a UTF-8 text body.
    pub source: String,
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
    /// The complete `.ais` source file as a UTF-8 text body.
    pub source: String,
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
    /// The complete `.ais` source file as a UTF-8 text body — used to
    /// resolve the project name (which keys spend.json's tracker).
    pub source: String,
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
            source: MINIMAL.to_string(),
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
            source: "this is not valid aiSlang at all".to_string(),
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
            source: MINIMAL.to_string(),
        })).await.expect("validate succeeds at MCP layer");
        let text = body_text(&result);
        assert!(text.starts_with("OK:"),
            "valid source should produce an OK: summary; got: {text:?}");
        assert!(text.contains("project:"),
            "OK summary should name the project block");
        assert!(text.contains("models:"),
            "OK summary should report model count");
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
            source: bad.to_string(),
        })).await.expect("validate never errors at MCP layer for parse failures");
        let text = body_text(&result);
        assert!(!text.starts_with("OK:"),
            "malformed source must NOT produce an OK: summary; got: {text:?}");
        // Source-aware rendering carries the "error at" + line marker.
        assert!(text.contains("error") || text.contains("Parse")
                || text.contains("expected") || text.contains("found"),
            "diagnostic should mention an error / expected / found; got: {text:?}");
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
            source:     PROJECT_WITH_PROMPT.to_string(),
            source_dir: Some(dir.to_string_lossy().to_string()),
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
            source:     PROJECT_WITH_PROMPT.to_string(),
            source_dir: Some(dir.to_string_lossy().to_string()),
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
            source:     "this is not valid".to_string(),
            source_dir: None,
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
    struct PathGuard(Option<std::ffi::OsString>);
    impl PathGuard {
        fn no_docker() -> Self {
            let prev = std::env::var_os("PATH");
            // SAFETY: tests run in cargo's default single-thread
            // ordering per #[tokio::test] only when --test-threads=1;
            // workspace tests already use the default multi-thread.
            // We accept the (small) chance of cross-test PATH races
            // by using a per-test guard that restores on drop —
            // matches the v0.1 cli_apply_destroy tempdir pattern.
            // For full isolation, future v0.3 work could pull docker
            // detection behind a trait + a test seam.
            unsafe { std::env::set_var("PATH", "/nonexistent-aislang-mcp-test-path"); }
            Self(prev)
        }
    }
    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("PATH", v),
                    None    => std::env::remove_var("PATH"),
                }
            }
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
            source:     APPLY_TEST_SOURCE.to_string(),
            source_dir: Some(dir.to_string_lossy().to_string()),
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
            source:     "this is not aislang".to_string(),
            source_dir: Some(dir.to_string_lossy().to_string()),
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
            source:     APPLY_TEST_SOURCE.to_string(),
            source_dir: Some(dir.to_string_lossy().to_string()),
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
            source:      APPLY_TEST_SOURCE.to_string(),
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
            source:      APPLY_TEST_SOURCE.to_string(),
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
            source:      APPLY_TEST_SOURCE.to_string(),
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
            source:      "not aislang".to_string(),
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
            source:      APPLY_TEST_SOURCE.to_string(),
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
}
