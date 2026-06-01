//! v0.3 Week 17 Tue — `aislang chat <agent> <dir>` integration tests.
//!
//! Closes the v0.3 carry-over noted at Week 16 Fri close: chat-dir
//! input wires through `aislang_runtime::load_project_input` + the
//! new `build_session_for_agent_multi_module` mcp-serve path.
//!
//! All four tests exit before any live LiteLLM round-trip lands
//! (EOF on the toolless happy path, agent-lookup-error on the bad-
//! label paths), matching the cli_chat.rs harness pattern.  CI
//! doesn't need Docker / Ollama / network for these to pass.
//!
//! Fixture: `examples/multi-module-demo/` which holds:
//!   - `aislang.toml` declaring `[modules]` architect + coder
//!   - `agents/architect.ais` → agent `architect`, model `thinker`
//!     (toolless), prompt `system`
//!   - `agents/coder.ais` → agent `coder`, model = `use
//!     architect.thinker` (cross-module ref!), prompt `system`,
//!     toolless

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples")
}

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_aislang")) }

#[test] fn chat_dir_input_opens_session_for_architect_agent() {
    let dir = examples_dir().join("multi-module-demo");
    let mut child = bin().args(["chat", "architect"]).arg(&dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    drop(child.stdin.take());   // EOF → clean close
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "chat against dir-input must EOF-close cleanly; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Bare agent input `architect` should resolve to the qualified
    // label `architect.architect` (via find_agent_label suffix match).
    assert!(stderr.contains("agent `architect.architect`"),
        "banner must name the qualified agent label; got:\n{stderr}");
    assert!(stderr.contains("end of input"),
        "stderr must mark clean EOF close; got:\n{stderr}");
}

#[test] fn chat_dir_input_resolves_cross_module_ref_for_coder_agent() {
    // The coder agent's model is `use architect.thinker` — a v0.3
    // Week 16 Tue cross-module ref.  Multi-module session-builder
    // (`build_session_for_agent_multi_module`) qualifies that to
    // `architect.thinker` in the merged model_bindings.  Live-test:
    // session opens cleanly + the banner names the cross-referenced
    // model alias.
    let dir = examples_dir().join("multi-module-demo");
    let mut child = bin().args(["chat", "coder"]).arg(&dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "chat against cross-module-ref agent must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent `coder.coder`"),
        "banner must qualify the coder agent label; got:\n{stderr}");
    // The model alias is `architect.thinker` (cross-module qualified)
    // — pins that mcp-serve resolved the `use architect.thinker` ref
    // in the merged ResolvedProject's model_bindings successfully.
    assert!(stderr.contains("model alias `architect.thinker`"),
        "banner must show the cross-module model alias; got:\n{stderr}");
}

#[test] fn chat_dir_input_explicit_qualified_form_also_works() {
    // User-explicit `<module>.<agent>` form should match without
    // relying on the suffix-match disambiguator.
    let dir = examples_dir().join("multi-module-demo");
    let mut child = bin().args(["chat", "architect.architect"]).arg(&dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "chat with explicit qualified label must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent `architect.architect`"),
        "banner must name the agent; got:\n{stderr}");
}

#[test] fn chat_dir_input_unknown_agent_lists_available_qualified_labels() {
    let dir = examples_dir().join("multi-module-demo");
    let out = bin().args(["chat", "zzzzzzz"]).arg(&dir)
        .output().expect("spawn aislang");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no agent named `zzzzzzz`"),
        "stderr must name the missing label; got:\n{stderr}");
    // Both module-qualified labels must appear so the user can
    // pick.  Order is sorted alphabetically by the existing
    // available-list code.
    assert!(stderr.contains("architect.architect"),
        "available-list must include `architect.architect`; got:\n{stderr}");
    assert!(stderr.contains("coder.coder"),
        "available-list must include `coder.coder`; got:\n{stderr}");
}

#[test] fn chat_dir_input_exit_command_closes_cleanly() {
    let dir = examples_dir().join("multi-module-demo");
    let mut child = bin().args(["chat", "architect"]).arg(&dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "/exit").expect("write /exit");
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "/exit on dir-input chat must close 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("session ended"),
        "/exit should mark clean close; got:\n{stderr}");
}
