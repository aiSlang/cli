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
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::{Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
};
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
}
