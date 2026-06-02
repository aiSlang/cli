//! v0.3 Week 16 Mon — multi-agent single-file integration coverage.
//!
//! v0.2 plumbing supports N agents per .ais file at every layer (AST
//! `Vec<Agent>`, checker duplicate-label detection across kinds,
//! resolver per-agent model+prompt binding, `aislang chat` finds-by-
//! label).  Every shipped example file (`support-agent.ais`,
//! `support-agent-k8s.ais`, `support-agent-paid.ais`, `free-local.ais`)
//! happens to declare exactly one agent, so the cross-cutting
//! "2 agents simultaneously" path was uncovered.  This file pins it.
//!
//! No Docker / no live LLM: every test exits the chat-loop before any
//! MCP spawn or LiteLLM call (EOF on the toolless happy path, or
//! agent-lookup-error on the bad-label paths).

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Build a tempdir holding a 2-agent .ais file + the two prompt files
/// it references.  Caller cleans up.
///
/// Topology pinned by the test: agent `architect` runs on model
/// `thinker` with prompt `architect_sys`; agent `coder` runs on model
/// `fast` with prompt `coder_sys`.  Models + prompts differ across
/// agents so any "first agent wins" regression at resolution time
/// surfaces immediately.
fn two_agent_path(suffix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aislang-multi-agent-{}-{suffix}",
        std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("architect.md"), "you design things")
        .expect("write architect prompt");
    std::fs::write(dir.join("coder.md"),     "you write code")
        .expect("write coder prompt");
    let ais_path = dir.join("two-agents.ais");
    let src = r#"project "two-agents" {
  budget {
    hard_cap_per_day = $1
    hard_cap_per_run = $0.01
  }
  model "thinker" {
    requires    = [chat]
    # v0.3 Week 17 Tue: run_chat now goes through load_project_input
    # (was validate-only) — context_min must satisfy the pinned
    # ollama model's 128k window.  Drop from 200k → 32k so resolve
    # passes against the bundled catalog.
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  model "fast" {
    requires    = [chat]
    context_min = 32k
    prefer      = [ollama.llama_3_1_8b]
    pin         = ollama.llama_3_1_8b
  }
  prompt "architect_sys" {
    file    = "./architect.md"
    version = "1.0.0"
  }
  prompt "coder_sys" {
    file    = "./coder.md"
    version = "1.0.0"
  }
  agent "architect" {
    model         = model.thinker
    system_prompt = prompt.architect_sys
    tools         = []
  }
  agent "coder" {
    model         = model.fast
    system_prompt = prompt.coder_sys
    tools         = []
  }
}
"#;
    std::fs::write(&ais_path, src).expect("write two-agents.ais");
    ais_path
}

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_aislang")) }

// ---------------------------------------------------------------------- //
// Validate path
// ---------------------------------------------------------------------- //

#[test] fn validate_accepts_two_agents_in_one_file() {
    let path = two_agent_path("validate");
    let out = bin().args(["validate"]).arg(&path).output().expect("spawn");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(out.status.success(),
        "validate should accept 2 agents; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("parsed and type-checked"),
        "validate should report success; got:\n{stdout}");
}

// ---------------------------------------------------------------------- //
// Chat dispatch — both agents reachable, neither shadowed
// ---------------------------------------------------------------------- //

#[test] fn chat_dispatches_to_each_agent_independently() {
    // Pin BOTH dispatch paths: the session-open banner names the
    // exact (agent-label, model-alias) pair the user asked for.  If
    // agent lookup ever regresses to "first agent wins", picking
    // `coder` would still surface `model alias 'thinker'`.
    for (agent, model_alias) in [
        ("architect", "thinker"),
        ("coder",     "fast"),
    ] {
        let path = two_agent_path(&format!("dispatch-{agent}"));
        let mut child = bin()
            .args(["chat", agent]).arg(&path)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().expect("spawn aislang");
        drop(child.stdin.take());
        let out = child.wait_with_output().expect("wait");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert!(out.status.success(),
            "agent `{agent}` should EOF-close cleanly; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(&format!("agent `{agent}`")),
            "banner should name the chosen agent; got:\n{stderr}");
        assert!(stderr.contains(&format!("model alias `{model_alias}`")),
            "banner should name the agent's resolved model alias \
             (`{model_alias}` for agent `{agent}`); got:\n{stderr}");
    }
}

// ---------------------------------------------------------------------- //
// Did-you-mean disambiguation across multiple agents
// ---------------------------------------------------------------------- //

#[test] fn typo_for_architect_does_not_suggest_coder() {
    // Tightest disambiguation: `archtect` is 1-edit from `architect`
    // and 8-edit from `coder`.  Suggester must prefer the closer one.
    let path = two_agent_path("typo-architect");
    let out = bin().args(["chat", "archtect"]).arg(&path).output().expect("spawn");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("did you mean `architect`?"),
        "typo should suggest the closer label `architect`; got:\n{stderr}");
    assert!(!stderr.contains("did you mean `coder`?"),
        "typo for `archtect` must NOT suggest `coder`; got:\n{stderr}");
}

#[test] fn unknown_agent_lists_all_available_labels() {
    // Wildly wrong label → no close match → full available-list is
    // surfaced.  Both agent labels must appear so the user can pick.
    let path = two_agent_path("unknown-list");
    let out = bin().args(["chat", "zzzzzzzz"]).arg(&path).output().expect("spawn");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no agent, pipeline, or branch named `zzzzzzzzz`")
            || stderr.contains("no agent, pipeline, or branch named `zzzzzzzz`"),
        "stderr should name the missing label; got:\n{stderr}");
    // Order is declaration-order today; pin both labels are listed,
    // not the exact separator (avoids fighting future hint-formatting
    // tweaks).
    assert!(stderr.contains("architect"),
        "available-list must include `architect`; got:\n{stderr}");
    assert!(stderr.contains("coder"),
        "available-list must include `coder`; got:\n{stderr}");
}

// ---------------------------------------------------------------------- //
// /exit command — second agent's session also closes cleanly
// ---------------------------------------------------------------------- //

#[test] fn second_agent_session_responds_to_exit_command() {
    // Mirror of cli_chat::chat_exit_command_ends_session_cleanly but
    // targeting agent #2 (`coder`).  Pins that the /exit handler is
    // not tied to the first-declared agent.
    let path = two_agent_path("exit-coder");
    let mut child = bin()
        .args(["chat", "coder"]).arg(&path)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "/exit").expect("write /exit");
    }
    let out = child.wait_with_output().expect("wait");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    assert!(out.status.success(),
        "/exit on agent #2 should close 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent `coder`"),
        "banner must name agent #2; got:\n{stderr}");
    assert!(stderr.contains("session ended"),
        "/exit should mark clean close on agent #2; got:\n{stderr}");
}
