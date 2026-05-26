//! Integration tests for `aislang chat <agent> <file>` — week 6 Mon stub.
//!
//! These exercise the actual CLI binary against the bundled demo file
//! and (intentionally) malformed inputs. They pin the v0.1 stub
//! contract:
//!
//!   1. `chat <known-agent> <demo>` → exit 0, prints the placeholder line.
//!   2. `chat <unknown-agent> <demo>` → non-zero exit, error message
//!      lists available labels and suggests close matches.
//!   3. `chat <agent> <nonexistent-file>` → non-zero exit, clear file
//!      error.
//!
//! All tests invoke the CARGO_BIN_EXE_aislang binary directly — no
//! shell parsing, no rebuild between tests. Cargo builds the binary
//! before this file's tests run.

use std::path::PathBuf;
use std::process::Command;

/// Absolute path to the bundled support-agent demo file.
fn demo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples").join("support-agent.ais")
}

/// Path to the compiled `aislang` binary cargo built for these tests.
fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_aislang"))
}

// ---------------------------------------------------------------------------
// Happy path
// ---------------------------------------------------------------------------

#[test] fn chat_with_known_agent_in_demo_succeeds() {
    let out = bin()
        .args(["chat", "support"])
        .arg(demo_path())
        .output()
        .expect("spawn aislang");
    assert!(out.status.success(),
        "expected exit 0; stderr was:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("agent `support` resolved"),
        "stdout should confirm agent resolution; got:\n{stdout}");
    assert!(stdout.contains("chat loop arrives Wednesday"),
        "stdout should carry the stub line; got:\n{stdout}");
}

#[test] fn chat_stub_lists_remaining_week6_schedule() {
    // The stub line surfaces what's coming in the next four days —
    // useful for anyone reading the output mid-week to know the
    // current dev state.
    let out = bin()
        .args(["chat", "support"])
        .arg(demo_path())
        .output()
        .expect("spawn aislang");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Tue: rmcp"));
    assert!(stdout.contains("Wed: LiteLLM"));
    assert!(stdout.contains("Thu: MCP tool-use"));
    assert!(stdout.contains("Fri: end-to-end"));
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
