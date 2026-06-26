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
        "aislang_apply",      // Wed
        "aislang_destroy",    // Wed
        "aislang_status",     // Wed
        "aislang_chat_turn",    // Thu
        "aislang_eval",         // Thu
        "aislang_run_pipeline", // W26 — pipeline/loop dispatch
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
async fn wed_destructive_tools_carry_destructive_hint_annotation() {
    // Verifies the `destructive_hint: true` annotation reaches the
    // client over the wire — that's what Claude Desktop / Cursor look
    // at when deciding whether to show a confirmation dialog before
    // tool invocation.  Pins the v0.2 architectural decision #5
    // (planning doc Track A — "PreToolUse via MCP annotations").
    let client = spawn_mcp_serve().await;

    let tools = client.peer().list_tools(Default::default()).await
        .expect("list_tools failed");

    let lookup = |name: &str| tools.tools.iter()
        .find(|t| t.name.as_ref() == name)
        .unwrap_or_else(|| panic!("tool `{name}` not in tool list"));

    let apply = lookup("aislang_apply");
    let destroy = lookup("aislang_destroy");
    let status = lookup("aislang_status");

    // apply + destroy must carry destructive_hint = true.
    let apply_ann = apply.annotations.as_ref()
        .expect("apply must have annotations");
    assert_eq!(apply_ann.destructive_hint, Some(true),
        "aislang_apply must declare destructive_hint = true");
    assert_eq!(apply_ann.read_only_hint, Some(false),
        "aislang_apply is not read-only");

    let destroy_ann = destroy.annotations.as_ref()
        .expect("destroy must have annotations");
    assert_eq!(destroy_ann.destructive_hint, Some(true),
        "aislang_destroy must declare destructive_hint = true");
    assert_eq!(destroy_ann.idempotent_hint, Some(true),
        "aislang_destroy must declare idempotent_hint = true (Decision 27 idempotency)");

    // status must carry read_only_hint = true; never destructive.
    let status_ann = status.annotations.as_ref()
        .expect("status must have annotations");
    assert_eq!(status_ann.read_only_hint, Some(true),
        "aislang_status must declare read_only_hint = true");
    assert_eq!(status_ann.destructive_hint, Some(false),
        "aislang_status must NOT be destructive");

    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn wed_status_wire_returns_not_applied_for_fresh_dir() {
    // End-to-end wire roundtrip for aislang_status against an empty
    // source_dir → expects the `not applied` body.
    let client = spawn_mcp_serve().await;

    let dir = std::env::temp_dir().join(format!(
        "aislang-mcp-wire-status-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let source = r#"project "demo" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
}
"#;

    let params = CallToolRequestParams::new("aislang_status".to_string())
        .with_arguments(serde_json::json!({
            "source":     source,
            "source_dir": dir.to_string_lossy(),
        }).as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_status)");
    let text = body_text(&result);
    assert!(text.contains("project:  demo"),
        "status header should include project name; got: {text}");
    assert!(text.contains("not applied"),
        "fresh dir status should report not applied; got: {text}");

    client.cancel().await.expect("clean shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn wed_destroy_wire_returns_nothing_to_destroy_for_clean_dir() {
    let client = spawn_mcp_serve().await;

    let dir = std::env::temp_dir().join(format!(
        "aislang-mcp-wire-destroy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let params = CallToolRequestParams::new("aislang_destroy".to_string())
        .with_arguments(serde_json::json!({
            "source_dir": dir.to_string_lossy(),
        }).as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await
        .expect("call_tool(aislang_destroy)");
    let text = body_text(&result);
    assert!(text.contains("nothing to destroy"),
        "destroy on clean dir should report nothing to destroy; got: {text}");

    client.cancel().await.expect("clean shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

// ────────────────────────────────────────────────────────────────
// Thu — chat_turn + eval (wire annotations + error paths)
// ────────────────────────────────────────────────────────────────
//
// chat_turn happy-path requires a real LiteLLM endpoint — can't
// pin in integration tests without a wiremock server, which is
// deferred to v0.3 polish.  The wire-side annotations + error
// paths ARE testable cleanly and that's what we cover here.

#[tokio::test]
async fn thu_conversation_tools_have_open_world_annotations() {
    // Verifies the annotation shape: chat_turn + eval are NOT
    // marked destructive (no confirmation gate), but ARE marked
    // open_world (they talk to LiteLLM + MCP children + spend
    // tracker — observable side effects beyond the call's body).
    let client = spawn_mcp_serve().await;
    let tools = client.peer().list_tools(Default::default()).await
        .expect("list_tools failed");

    let lookup = |name: &str| tools.tools.iter()
        .find(|t| t.name.as_ref() == name)
        .unwrap_or_else(|| panic!("tool `{name}` not in tool list"));

    let chat_turn = lookup("aislang_chat_turn");
    let eval      = lookup("aislang_eval");

    let chat_ann = chat_turn.annotations.as_ref()
        .expect("chat_turn must have annotations");
    assert_eq!(chat_ann.destructive_hint, Some(false),
        "chat_turn should NOT be destructive (no confirmation gate)");
    assert_eq!(chat_ann.read_only_hint, Some(false),
        "chat_turn modifies external state via LLM/MCP calls");
    assert_eq!(chat_ann.open_world_hint, Some(true),
        "chat_turn observably interacts with external services");

    let eval_ann = eval.annotations.as_ref()
        .expect("eval must have annotations");
    assert_eq!(eval_ann.destructive_hint, Some(false));
    assert_eq!(eval_ann.open_world_hint, Some(true));
}

#[tokio::test]
async fn thu_chat_turn_wire_parse_error_returns_clean_error() {
    let client = spawn_mcp_serve().await;
    let params = CallToolRequestParams::new("aislang_chat_turn".to_string())
        .with_arguments(serde_json::json!({
            "source":      "not aislang",
            "agent_label": "support",
            "user_input":  "hi",
        }).as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await;
    // Parse errors come back as MCP-level errors (RmcpError invalid_params).
    assert!(result.is_err(),
        "malformed source should produce an MCP error; got: {result:?}");
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("parse error"),
        "error should mention parse error; got: {err_msg}");
    client.cancel().await.expect("clean shutdown");
}

#[tokio::test]
async fn thu_eval_wire_no_eval_blocks_errors_cleanly() {
    let client = spawn_mcp_serve().await;
    // Project with no eval blocks at all (block bodies use
    // whitespace-separated attributes, NOT commas).
    let source = r#"project "noeval" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
}
"#;
    let params = CallToolRequestParams::new("aislang_eval".to_string())
        .with_arguments(serde_json::json!({ "source": source })
            .as_object().unwrap().clone());
    let result = client.peer().call_tool(params).await;
    assert!(result.is_err(),
        "eval against project-with-no-evals should error; got: {result:?}");
    let err_msg = format!("{:?}", result.unwrap_err());
    assert!(err_msg.contains("no `eval` blocks") ||
            err_msg.contains("no eval"),
        "error should explain there's nothing to evaluate; got: {err_msg}");
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
