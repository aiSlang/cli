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
}
