//! Integration tests for `aislang chat <agent> <file>` — week 6 Wed.
//!
//! Exercises the CLI binary against the bundled demo. Since Wed wires
//! the live LiteLLM client, the tests use stdin EOF as the cleanest
//! way to verify the banner + clean-shutdown path without standing up
//! a real LiteLLM server. The wiremock-backed end-to-end test lives
//! in `aislang-runtime::session::tests::session_turn_returns_*`.
//!
//! Contract pinned here:
//!   1. `chat <known-agent> <demo>` with EOF stdin → exit 0; banner
//!      on stderr ("agent `support`…connected to LiteLLM…") +
//!      "(end of input)" on session close. Stdout stays clean
//!      (no model output without a turn).
//!   2. `chat <unknown-agent> <demo>` → non-zero exit + close-match
//!      hint.
//!   3. `chat <agent> <nonexistent-file>` → non-zero exit + clear
//!      file error.
//!   4. `chat <agent> <malformed-source>` → non-zero exit via the
//!      source-aware validate pipeline.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
// Happy path — EOF stdin closes the session cleanly
// ---------------------------------------------------------------------------

#[test] fn chat_with_known_agent_opens_session_and_exits_on_eof() {
    // No stdin → reader.next_line() returns Ok(None) immediately →
    // session closes with "(end of input)" + exit 0. Validates the
    // happy path through validate + agent lookup + prompt load +
    // session.open WITHOUT requiring a live LiteLLM (no `turn()`
    // is invoked — the very first action is the EOF check).
    let mut child = bin()
        .args(["chat", "support"])
        .arg(demo_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn aislang");
    // Close stdin immediately — child sees EOF.
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");

    assert!(out.status.success(),
        "expected exit 0 on EOF; stderr was:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Banner contents (all on stderr — stdout stays pipe-clean).
    assert!(stderr.contains("agent `support`"),
        "stderr must name the agent; got:\n{stderr}");
    assert!(stderr.contains("model alias `fast`"),
        "stderr must name the model alias; got:\n{stderr}");
    assert!(stderr.contains("LiteLLM"),
        "stderr must mention LiteLLM connection; got:\n{stderr}");
    assert!(stderr.contains("end of input"),
        "stderr must mark clean EOF close; got:\n{stderr}");
    // No turn was made → no assistant output on stdout.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.is_empty(),
        "no turns means no stdout; got: {stdout:?}");
}

#[test] fn chat_exit_command_ends_session_cleanly() {
    // `/exit` typed at the prompt → "(/exit — session ended)" + exit 0.
    // Same as EOF but via the explicit command keyword. Pin this so a
    // future refactor doesn't accidentally break the documented escape.
    let mut child = bin()
        .args(["chat", "support"])
        .arg(demo_path())
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
    assert!(out.status.success());
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
