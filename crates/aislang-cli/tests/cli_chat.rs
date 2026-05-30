//! Integration tests for `aislang chat <agent> <file>` — week 6 Thu.
//!
//! Two test categories:
//!
//!   1. **Tool-less agent** (`toolless_path()`): an inline .ais that
//!      declares an agent with `tools = []`. EOF + `/exit` happy-path
//!      tests run against this file — no Docker required, no MCP
//!      spawn happens, so they pass in any CI environment.
//!
//!   2. **Demo agent** (`demo_path()`, has `tools = [mcp_server.docs]`):
//!      used only for the agent-name-error tests, where we exit BEFORE
//!      the MCP spawn step. Argument-validation paths thread through
//!      validate → agent lookup before any Docker call.
//!
//! Real-MCP integration (the tool-use loop end-to-end against a live
//! `mcp/filesystem:1.0.2`) is tested via `--ignored` runtime crate
//! tests, not here.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Absolute path to the bundled support-agent demo file (has MCP
/// servers; use for agent-name-error tests that exit before spawn).
fn demo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples").join("support-agent.ais")
}

/// Write a tool-less .ais source to a tempfile + the matching prompt
/// file. Caller is responsible for removing them. Returns the .ais
/// path.
///
/// **Per-test `suffix` parameter**: cargo runs tests in this binary
/// in parallel by default; without a per-test discriminator every
/// `toolless_path()` call would resolve to the SAME `/tmp/aislang-
/// chat-toolless-<pid>` directory (PID is the test-binary PID,
/// constant across all tests in the same binary).  That race let
/// one test's `remove_dir_all` win the cleanup while another test's
/// child process was still reading the .ais — surfaced on GitHub
/// CI as a flaky "source file ... not readable" failure.  Each
/// caller now supplies a unique suffix so the dirs are disjoint.
fn toolless_path(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aislang-chat-toolless-{}-{suffix}",
        std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let prompt_path = dir.join("system.md");
    std::fs::write(&prompt_path, "you are helpful").expect("write prompt");
    let ais_path = dir.join("toolless.ais");
    let src = r#"project "toolless" {
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
    std::fs::write(&ais_path, src).expect("write toolless.ais");
    ais_path
}

/// Path to the compiled `aislang` binary cargo built for these tests.
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aislang"))
}

// ---------------------------------------------------------------------------
// Happy path — EOF stdin closes the session cleanly
// ---------------------------------------------------------------------------

#[test] fn chat_with_toolless_agent_opens_session_and_exits_on_eof() {
    // No tools → no MCP spawn → no Docker required. Validates the
    // happy path through validate + agent lookup + prompt load +
    // session.open + EOF clean-close WITHOUT requiring Docker.
    let path = toolless_path("eof");
    let mut child = bin()
        .args(["chat", "support"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aislang");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());

    assert!(out.status.success(),
        "expected exit 0 on EOF; stderr was:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent `support`"),
        "stderr must name the agent; got:\n{stderr}");
    assert!(stderr.contains("model alias `fast`"),
        "stderr must name the model alias; got:\n{stderr}");
    assert!(stderr.contains("LiteLLM"),
        "stderr must mention LiteLLM connection; got:\n{stderr}");
    assert!(stderr.contains("end of input"),
        "stderr must mark clean EOF close; got:\n{stderr}");
    // Tool-less agent → no "tools: N available" banner line.
    assert!(!stderr.contains("tools:"),
        "tool-less session must NOT advertise a tool count; got:\n{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.is_empty(),
        "no turns means no stdout; got: {stdout:?}");
}

#[test] fn chat_exit_command_ends_session_cleanly() {
    // `/exit` typed at the prompt → "(/exit — session ended)" + exit 0.
    // Tool-less for the same Docker-free reasons.
    let path = toolless_path("exit-cmd");
    let mut child = bin()
        .args(["chat", "support"])
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aislang");
    {
        let stdin = child.stdin.as_mut().expect("stdin handle");
        stdin.write_all(b"/exit\n").expect("write /exit");
    }
    let out = child.wait_with_output().expect("wait");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(out.status.success(),
        "expected exit 0 on /exit; stderr was:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/exit — session ended"),
        "stderr must mark /exit close; got:\n{stderr}");
}

// ---------------------------------------------------------------------------
// Agent-label errors — "did you mean…?" suggestions
// ---------------------------------------------------------------------------

#[test] fn chat_with_typo_agent_label_suggests_match() {
    // Demo has agent "support"; typo it as "suport".
    let out = bin()
        .args(["chat", "suport"])
        .arg(demo_path())
        .output()
        .expect("spawn aislang");
    assert!(!out.status.success(),
        "expected non-zero exit on bad agent label");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no agent named `suport`"),
        "stderr should name the missing label; got:\n{stderr}");
    assert!(stderr.contains("did you mean `support`?"),
        "stderr should suggest the close match; got:\n{stderr}");
}

#[test] fn chat_with_unknown_agent_lists_available_labels() {
    // Wildly wrong label (no close match) — error should still
    // surface what's available so the user can pick.
    let out = bin()
        .args(["chat", "zzzzzzzzz"])
        .arg(demo_path())
        .output()
        .expect("spawn aislang");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no agent named `zzzzzzzzz`"));
    assert!(stderr.contains("available: support"),
        "stderr should list available labels when no close match; got:\n{stderr}");
}

// ---------------------------------------------------------------------------
// File errors
// ---------------------------------------------------------------------------

#[test] fn chat_with_nonexistent_file_errors_cleanly() {
    let out = bin()
        .args(["chat", "support", "/tmp/does-not-exist-9b2f.ais"])
        .output()
        .expect("spawn aislang");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("read"),
        "stderr should report a read error; got:\n{stderr}");
    assert!(stderr.contains("does-not-exist-9b2f.ais"),
        "stderr should name the missing file; got:\n{stderr}");
}

#[test] fn chat_with_malformed_source_errors_via_validate() {
    // Write a deliberately broken .ais file to a temp path and feed
    // it to `chat`. The validate pipeline produces the source-aware
    // diagnostic; chat should propagate that (not its own stub message).
    let tmp = std::env::temp_dir().join("aislang-chat-malformed.ais");
    std::fs::write(&tmp, "project").expect("write temp .ais");
    let out = bin()
        .args(["chat", "support"])
        .arg(&tmp)
        .output()
        .expect("spawn aislang");
    let _ = std::fs::remove_file(&tmp);
    assert!(!out.status.success(),
        "expected non-zero exit on parse error");
    // Validate's source-aware renderer goes to stderr. We don't pin a
    // specific message (parser error wording can drift) — just that
    // *something* shows up.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.is_empty(),
        "expected stderr diagnostic; got nothing");
}
