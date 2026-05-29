//! Integration tests for `aislang-runtime::mcp_serve_http` —
//! week 13 Mon HTTP transport.
//!
//! Each test:
//!   1. Binds a `TcpListener` on `127.0.0.1:0` so the kernel hands
//!      back an ephemeral port (no port-collision risk between
//!      parallel tests).
//!   2. Spawns `mcp_serve_http(listener, ct)` in a task.
//!   3. Connects an rmcp client via the Streamable-HTTP client
//!      transport.
//!   4. Exercises the contract under test (handshake, tool list,
//!      ping round-trip, multi-client session-pool sharing).
//!   5. Fires the `CancellationToken` and joins the server task so
//!      the test cleans up.
//!
//! These tests are the engineering counterpart to v0.1's
//! `cli_mcp_serve.rs` stdio tests: same handshake + tool surface,
//! different wire transport.  Validates the "proxy-via-MCP" design
//! end-to-end over the network — what `aislang mcp-serve --http`
//! actually delivers.

use std::time::Duration;

use rmcp::{
    ServiceExt,
    model::CallToolRequestParams,
    transport::StreamableHttpClientTransport,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Bring up an in-process HTTP MCP server on a random local port.
/// Returns `(url, ct, server_join_handle)` so the test can:
///   - connect a client to `url`
///   - cancel `ct` to trigger graceful shutdown
///   - `await` the join handle to assert the server returned cleanly
async fn start_test_server() -> (String, CancellationToken, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await
        .expect("bind 127.0.0.1:0");
    let addr = listener.local_addr().expect("local_addr");
    let url = format!("http://127.0.0.1:{}/mcp", addr.port());

    let ct = CancellationToken::new();
    let ct_server = ct.clone();
    let join = tokio::spawn(async move {
        aislang_runtime::mcp_serve_http(listener, ct_server).await
            .expect("mcp_serve_http returned an error");
    });

    // Tiny yield: give axum::serve a chance to enter the accept
    // loop before the test client tries to connect.  100ms is
    // overkill on debug locally but pinned by rmcp's own
    // integration tests (test_sse_concurrent_streams) so we
    // mirror their envelope.
    tokio::time::sleep(Duration::from_millis(100)).await;

    (url, ct, join)
}

/// Clean shutdown helper: cancel, then join (timeout-bounded so a
/// stuck server fails the test loudly instead of hanging CI).
async fn shutdown(ct: CancellationToken, join: tokio::task::JoinHandle<()>) {
    ct.cancel();
    tokio::time::timeout(Duration::from_secs(5), join).await
        .expect("server join timed out (5s)")
        .expect("server task panicked");
}

// ───────────────────────────────────────────────────────────────
// Test 1 — handshake + tool list round-trip over HTTP
// ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn http_handshake_succeeds_and_lists_all_phase_1_tools() {
    let (url, ct, join) = start_test_server().await;

    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = tokio::time::timeout(
        Duration::from_secs(5),
        ().serve(transport),
    )
    .await
    .expect("MCP handshake timed out (5s)")
    .expect("MCP handshake failed over HTTP");

    let tools = client.peer().list_tools(Default::default()).await
        .expect("list_tools failed over HTTP");

    // Pin the same 11-tool surface the stdio test pins.  Sharing
    // the same expected-set across both transports is the
    // architectural assertion: stdio vs. HTTP is a wire choice;
    // the tool router is one piece of code.
    let names: Vec<&str> = tools.tools.iter()
        .map(|t| t.name.as_ref()).collect();
    let expected = &[
        "aislang_ping",
        "aislang_lang_spec",
        "aislang_examples",
        "aislang_describe",
        "aislang_validate",
        "aislang_plan",
        "aislang_apply",
        "aislang_destroy",
        "aislang_status",
        "aislang_chat_turn",
        "aislang_eval",
    ];
    assert_eq!(tools.tools.len(), expected.len(),
        "HTTP tool count should match stdio's; got: {names:?}");
    for tool in expected {
        assert!(names.contains(tool),
            "HTTP tool list missing `{tool}`; got: {names:?}");
    }

    client.cancel().await.expect("client cancel");
    shutdown(ct, join).await;
}

// ───────────────────────────────────────────────────────────────
// Test 2 — aislang_ping round-trip over HTTP
// ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn http_ping_round_trips_pong() {
    let (url, ct, join) = start_test_server().await;

    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = ().serve(transport).await
        .expect("MCP handshake failed");

    let result = client.peer().call_tool(
        CallToolRequestParams::new("aislang_ping".to_string()),
    ).await.expect("aislang_ping call failed");

    let body = result.content.first().expect("non-empty content");
    let text = match &body.raw {
        rmcp::model::RawContent::Text(t) => &t.text,
        other => panic!("expected text; got {other:?}"),
    };
    assert!(text.contains("pong"),
        "ping tool should return body containing `pong`; got: {text:?}");

    client.cancel().await.expect("client cancel");
    shutdown(ct, join).await;
}

// ───────────────────────────────────────────────────────────────
// Test 3 — two concurrent HTTP clients share the same server
// ───────────────────────────────────────────────────────────────
//
// The architectural payoff of HTTP transport (over stdio): one
// running mcp-serve handles N clients.  Each client gets its own
// MCP session (handshake + session ID) but they're talking to the
// same AislangServer instance, so the Session pool inside is
// shared.
//
// We don't probe the pool internals here (that'd need a chat_turn
// call with a live LiteLLM, which the stdio test doesn't do
// either).  Instead we verify the basic claim that two HTTP
// clients can both initialize + list tools + call ping without
// interfering — proves the per-session factory is creating
// independent MCP sessions while sharing the rmcp service.

#[tokio::test]
async fn http_two_clients_can_connect_concurrently() {
    let (url, ct, join) = start_test_server().await;

    // Spawn both clients in parallel and assert each completes its
    // initialize + list_tools + ping cycle independently.
    let url_a = url.clone();
    let url_b = url.clone();
    let client_a = tokio::spawn(async move {
        let t = StreamableHttpClientTransport::from_uri(url_a);
        let c = ().serve(t).await.expect("client A handshake");
        let tools = c.peer().list_tools(Default::default()).await
            .expect("client A list_tools");
        let ping = c.peer().call_tool(
            CallToolRequestParams::new("aislang_ping".to_string()),
        ).await.expect("client A ping");
        c.cancel().await.expect("client A cancel");
        (tools.tools.len(), ping.content.len())
    });
    let client_b = tokio::spawn(async move {
        let t = StreamableHttpClientTransport::from_uri(url_b);
        let c = ().serve(t).await.expect("client B handshake");
        let tools = c.peer().list_tools(Default::default()).await
            .expect("client B list_tools");
        let ping = c.peer().call_tool(
            CallToolRequestParams::new("aislang_ping".to_string()),
        ).await.expect("client B ping");
        c.cancel().await.expect("client B cancel");
        (tools.tools.len(), ping.content.len())
    });

    let (a, b) = tokio::join!(client_a, client_b);
    let (tools_a, ping_a) = a.expect("client A task");
    let (tools_b, ping_b) = b.expect("client B task");

    // Both clients should see the full 11-tool surface and a
    // non-empty ping reply.
    assert_eq!(tools_a, 11, "client A tool count");
    assert_eq!(tools_b, 11, "client B tool count");
    assert!(ping_a >= 1, "client A ping reply content");
    assert!(ping_b >= 1, "client B ping reply content");

    shutdown(ct, join).await;
}

// ───────────────────────────────────────────────────────────────
// Test 4 — graceful shutdown drains on cancellation
// ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn http_server_shuts_down_on_cancellation_token() {
    let (url, ct, join) = start_test_server().await;

    // Connect + use + drop a client cleanly first.
    let transport = StreamableHttpClientTransport::from_uri(url);
    let client = ().serve(transport).await.expect("handshake");
    let _ = client.peer().list_tools(Default::default()).await
        .expect("list_tools");
    client.cancel().await.expect("client cancel");

    // Now fire the server CT and check it returns in bounded time.
    // 5s timeout in `shutdown()` is the assertion that graceful
    // shutdown isn't a misnomer — the server actually exits.
    shutdown(ct, join).await;
}
