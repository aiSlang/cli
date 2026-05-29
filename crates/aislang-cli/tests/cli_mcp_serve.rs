//! Integration tests for `aislang mcp-serve` — week 12 Mon + Tue.
//!
//! Wire round-trip: spawn the actual `aislang mcp-serve --stdio`
//! binary as a stdio child, connect to it via rmcp's client side,
//! verify the MCP handshake completes, list tools, and exercise
//! each tool via a real call.  Pins the contract that "Claude
//! Desktop / Cursor / any rmcp-compatible client can talk to us"
//! without needing those clients running.
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

/// Extract the text body from a CallToolResult.  Panics on
/// non-text content; that's fine for tests asserting text shape.
fn body_text(result: &rmcp::model::CallToolResult) -> String {
    let frag = result.content.first().expect("at least one content fragment");
    match &frag.raw {
        RawContent::Text(t) => t.text.clone(),
        other => panic!("expected text content; got: {other:?}"),
    }
}

// ────────────────────────────────────────────────────────────────
// Tool list + ServerInfo
// ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn handshake_succeeds_and_lists_all_phase_1_tools() {
    let client = spawn_mcp_serve().await;

    let tools = client.peer().list_tools(Default::default()).await
        .expect("list_tools failed");

    // Week 12 Mon registered 1 tool; Tue brings the total to 6
    // (the pure tier).  Wed will add ~3 stateful tools (apply,
    // destroy, status); Thu adds 2 conversation tools (chat_turn,
    // eval).  Each day's DoD pins the expected tool count.
    let names: Vec<&str> = tools.tools.iter()
        .map(|t| t.name.as_ref()).collect();
    let expected = &[
        "aislang_ping",       // Mon
        "aislang_lang_spec",  // Tue
        "aislang_examples",   // Tue
        "aislang_describe",   // Tue
        "aislang_validate",   // Tue
        "aislang_plan",       // Tue
    ];
    for tool in expected {
        assert!(names.contains(tool),
            "tool list must contain `{tool}`; got: {names:?}");
    }
    assert_eq!(tools.tools.len(), expected.len(),
        "Tue's tool count should be exactly {}; got names: {names:?}",
        expected.len());

    // Spot-check that each tool carries a non-empty description —
    // host clients render this in their tool-call UI; a thin
    // description is a UX bug.
    for tool in &tools.tools {
        let desc = tool.description.as_deref().unwrap_or("");
        assert!(desc.len() >= 20,
            "tool `{}` description should be substantive; got: {desc:?}",
            tool.name);
    }

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

// ────────────────────────────────────────────────────────────────
// Mon's ping
// ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ping_call_returns_pong() {
    let client = spawn_mcp_serve().await;

    let params = CallToolRequestParams::new("aislang_ping".to_string());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_ping) failed");

    assert_ne!(result.is_error, Some(true),
        "aislang_ping must not report error; got: {result:?}");
    assert_eq!(body_text(&result), "pong");

    client.cancel().await.expect("clean shutdown");
}

// ────────────────────────────────────────────────────────────────
// Tue's pure-tier tools — one wire round-trip per tool
// ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn lang_spec_wire_returns_full_spec() {
    let client = spawn_mcp_serve().await;
    let params = CallToolRequestParams::new("aislang_lang_spec".to_string());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_lang_spec)");
    let text = body_text(&result);
    // Same shape checks as the unit test.  Pins that the bundled
    // spec content survives the MCP wire encoding (which means
    // multi-KB text bodies work end-to-end — important for the
    // upcoming aislang_describe + aislang_plan responses which can
    // be similar size).
    assert!(text.contains("aiSlang Language Specification"));
    assert!(text.contains("## 4. Resource types"));
    assert!(text.len() > 10_000, "spec body should be > 10KB");
    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn examples_wire_index_then_specific_example() {
    let client = spawn_mcp_serve().await;

    // 1. Default (no args) — index of available examples.
    let params = CallToolRequestParams::new("aislang_examples".to_string());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_examples) index");
    let text = body_text(&result);
    for name in &["minimal", "support-agent", "free-local"] {
        assert!(text.contains(name),
            "index should mention `{name}`; got: {text}");
    }

    // 2. Specific example by name — returns the .ais source.
    let params = CallToolRequestParams::new("aislang_examples".to_string())
        .with_arguments(serde_json::json!({ "name": "minimal" })
            .as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_examples name=minimal)");
    let text = body_text(&result);
    assert!(text.contains("project \"minimal\""),
        "minimal.ais should start with the project block; got: {text}");

    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn describe_wire_returns_project_json() {
    let client = spawn_mcp_serve().await;
    const MINIMAL: &str = include_str!("../../../examples/minimal.ais");
    let params = CallToolRequestParams::new("aislang_describe".to_string())
        .with_arguments(serde_json::json!({ "source": MINIMAL })
            .as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_describe)");
    let text = body_text(&result);
    let v: serde_json::Value = serde_json::from_str(&text)
        .expect("describe output must be valid JSON");
    assert_eq!(v.get("name").and_then(|n| n.as_str()), Some("minimal"));
    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn validate_wire_ok_summary_and_error_diagnostic() {
    let client = spawn_mcp_serve().await;
    const MINIMAL: &str = include_str!("../../../examples/minimal.ais");

    // 1. Valid source → "OK:" summary in success body.
    let params = CallToolRequestParams::new("aislang_validate".to_string())
        .with_arguments(serde_json::json!({ "source": MINIMAL })
            .as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_validate ok)");
    let text = body_text(&result);
    assert!(text.starts_with("OK:"),
        "valid source should give OK: summary; got: {text}");

    // 2. Malformed source → diagnostic text in success body.
    let bad = "this is definitely not aislang at all";
    let params = CallToolRequestParams::new("aislang_validate".to_string())
        .with_arguments(serde_json::json!({ "source": bad })
            .as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_validate bad)");
    let text = body_text(&result);
    assert!(!text.starts_with("OK:"),
        "malformed source must not give OK: summary; got: {text}");

    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn plan_wire_returns_all_artifact_sections() {
    // Spawn the server FROM a temp dir so the default source_dir
    // (cwd) actually contains the prompt file we materialise.
    let dir = std::env::temp_dir().join(format!(
        "aislang-mcp-wire-plan-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("system.md"),
        "you are a helpful test agent").unwrap();

    // Inline source — same shape as the unit test's PROJECT_WITH_PROMPT
    // (this test file is isolated from the runtime crate's tests so
    // we duplicate the literal here).
    let source = r#"project "demo" {
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

    let client = spawn_mcp_serve().await;
    let params = CallToolRequestParams::new("aislang_plan".to_string())
        .with_arguments(serde_json::json!({
            "source":     source,
            "source_dir": dir.to_string_lossy(),
        }).as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_plan)");
    let text = body_text(&result);

    // All four artifact sections should appear in the response.
    for section in &[
        "── Resolved IR (JSON) ──",
        "── docker-compose.yml ──",
        "── litellm_config.yaml ──",
        "── .env.example ──",
    ] {
        assert!(text.contains(section),
            "plan output should include `{section}`; got tail: {}",
            &text[text.len().saturating_sub(400)..]);
    }
    // Resolved IR pins fast → ollama.llama_3_1_8b.
    assert!(text.contains("ollama") && text.contains("llama_3_1_8b"));

    client.cancel().await.expect("clean shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
