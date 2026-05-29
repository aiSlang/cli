//! Integration tests for `aislang mcp-serve` — week 12 Mon.
//!
//! Wire round-trip: spawn the actual `aislang mcp-serve --stdio`
//! binary as a stdio child, connect to it via rmcp's client side,
//! verify the MCP handshake completes, list tools, and call
//! `aislang_ping`.  Pins the contract that "Claude Desktop /
//! Cursor / any rmcp-compatible client can talk to us" without
//! needing those clients running.
//!
//! These tests share the binary build with `cargo test`, so the
//! CARGO_BIN_EXE_aislang env var (set by cargo) points at the
//! freshly-compiled debug build.  No docker needed.

use std::time::Duration;

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, RawContent},
    transport::TokioChildProcess,
};
use tokio::process::Command;

/// Spawn `aislang mcp-serve --stdio` and return a connected rmcp
/// client.  The child is owned by the returned RunningService
/// (rmcp drops it when the service is cancelled / shut down).
async fn spawn_mcp_serve()
    -> rmcp::service::RunningService<rmcp::service::RoleClient, ()>
{
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aislang"));
    cmd.arg("mcp-serve").arg("--stdio");
    // Inherit stderr so any panic / startup log from the child is
    // surfaced in the test's output (cargo test --nocapture).  rmcp
    // captures stdin + stdout for the wire protocol.
    cmd.stderr(std::process::Stdio::inherit());

    let transport = TokioChildProcess::new(cmd)
        .expect("spawn aislang mcp-serve");

    // 5s handshake timeout — generous; the v0.1 mcp-client tests
    // use the same envelope.  Real handshakes complete in <100ms
    // on debug builds locally.
    tokio::time::timeout(
        Duration::from_secs(5),
        ().serve(transport),
    )
    .await
    .expect("MCP handshake timed out (5s)")
    .expect("MCP handshake failed")
}

#[tokio::test]
async fn handshake_succeeds_and_lists_aislang_ping() {
    let client = spawn_mcp_serve().await;

    let tools = client.peer().list_tools(Default::default()).await
        .expect("list_tools failed");

    // The week 12 Mon scaffold registers exactly one tool.  Tue
    // adds five more, so this assert will need to grow to ≥6 on
    // Tuesday — that's the point: each day's DoD pins a tool
    // count and the test catches accidental regressions.
    let names: Vec<&str> = tools.tools.iter()
        .map(|t| t.name.as_ref()).collect();
    assert!(names.contains(&"aislang_ping"),
        "tool list must contain `aislang_ping`; got: {names:?}");

    // Description carries the "Health check" copy from
    // `AislangServer::ping`'s #[tool(description = …)].
    let ping = tools.tools.iter()
        .find(|t| t.name.as_ref() == "aislang_ping")
        .unwrap();
    let desc = ping.description.as_deref().unwrap_or("");
    assert!(desc.to_lowercase().contains("health check"),
        "aislang_ping description should mention 'health check'; got: {desc:?}");

    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn ping_call_returns_pong() {
    let client = spawn_mcp_serve().await;

    // CallToolRequestParams is #[non_exhaustive] in rmcp 1.7 —
    // construct via ::new(), not a struct literal.  Matches the
    // pattern in v0.1's StdioChild::call_tool.
    let params = CallToolRequestParams::new("aislang_ping".to_string());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_ping) failed");

    // is_error: Some(false) (or None — rmcp omits when not set)
    // is the success path.  A Some(true) means the tool reported
    // failure via the CallToolResult shape.
    assert_ne!(result.is_error, Some(true),
        "aislang_ping must not report error; got: {result:?}");

    // `result.content` is Vec<Annotated<RawContent>> directly
    // (not Option<Vec<…>>).  Match on the first fragment's raw.
    let frag = result.content.first().expect("content must be non-empty");
    let text = match &frag.raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content; got: {other:?}"),
    };
    assert_eq!(text, "pong");

    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn server_info_advertises_aislang_via_handshake() {
    // The peer_info comes from the initial-handshake response —
    // that's where get_info() gets called.  Verifies the
    // ServerInfo (name + version + capabilities + instructions)
    // round-trips correctly over the wire, not just in the
    // unit-test direct invocation.
    let client = spawn_mcp_serve().await;
    let info = client.peer_info().cloned()
        .expect("peer_info should be set after handshake");

    assert_eq!(info.server_info.name, "aislang");
    assert!(!info.server_info.version.is_empty());
    assert!(info.capabilities.tools.is_some(),
        "tools capability must round-trip over the wire");
    let instr = info.instructions.unwrap_or_default();
    assert!(instr.contains("aiSlang"),
        "instructions should self-identify; got: {instr:?}");

    client.cancel().await.expect("clean shutdown");
}
