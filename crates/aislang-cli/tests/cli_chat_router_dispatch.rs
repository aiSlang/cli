//! v0.3 Week 17 Fri — `aislang chat <branch-label>` router dispatch.
//!
//! Mirrors the Tue `cli_chat_dir_input` + Thu orchestrator integration
//! tests but pins the BRANCH-label dispatch path: typing the label of
//! a `branch { … }` block at the CLI must wire into the router
//! pattern + open a clean chat session (EOF closes 0; banner names
//! the branch + the case keys + the classifier agent).
//!
//! All tests EOF before any live classifier turn fires — no Docker /
//! no LiteLLM required in CI.  Live e2e (classifier picks a case +
//! dispatches the matched pipeline) is exercised by hand against
//! `examples/patterns/router-demo/router-demo.ais`; documented in
//! that file's top-of-file walk-through.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn router_demo_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..").join("..")
        .join("examples").join("patterns").join("router-demo")
        .join("router-demo.ais")
}

fn bin() -> Command { Command::new(env!("CARGO_BIN_EXE_aislang")) }

#[test] fn chat_branch_label_opens_router_session_and_eof_closes() {
    let mut child = bin().args(["chat", "triage"]).arg(router_demo_path())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    drop(child.stdin.take());   // EOF → clean close
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "branch-label chat must EOF-close cleanly; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Banner pins the router-pattern dispatch shape + case count.
    assert!(stderr.contains("branch `triage` (router pattern, 3 cases)"),
        "banner must name branch + pattern + case count; got:\n{stderr}");
    assert!(stderr.contains("classifier agent: `classifier`"),
        "banner must surface the chosen classifier (v0.3 convention = \
         first declared agent); got:\n{stderr}");
    // All three case keys must surface so the user can predict
    // routing behaviour from the banner alone.
    assert!(stderr.contains("billing") && stderr.contains("technical")
            && stderr.contains("general"),
        "banner must list all case keys; got:\n{stderr}");
    assert!(stderr.contains("end of input"),
        "stderr must mark clean EOF close; got:\n{stderr}");
}

#[test] fn chat_branch_label_exit_command_closes_cleanly() {
    let mut child = bin().args(["chat", "triage"]).arg(router_demo_path())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "/exit").expect("write /exit");
    }
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "/exit on branch-label chat must close 0; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("session ended"),
        "/exit should mark clean close; got:\n{stderr}");
}

#[test] fn chat_unknown_label_lists_branches_alongside_agents_and_pipelines() {
    // Wildly wrong label → no close match → full available-list
    // surfaces.  The list must now include the branch entry so the
    // user discovers router-pattern dispatch through the same hint
    // mechanism as orchestrator + single-agent.
    let out = bin().args(["chat", "zzzzzzzzz"]).arg(router_demo_path())
        .output().expect("spawn aislang");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no agent, pipeline, or branch named `zzzzzzzzz`"),
        "stderr must name all three dispatch kinds in the missing-label \
         message; got:\n{stderr}");
    assert!(stderr.contains("triage (branch)"),
        "available-list must mark the branch entry so users can find \
         router-pattern targets; got:\n{stderr}");
    // Individual agents + pipelines also stay listed.
    assert!(stderr.contains("classifier"),
        "available-list must include the classifier agent; got:\n{stderr}");
    assert!(stderr.contains("(pipeline)"),
        "available-list must mark pipeline entries; got:\n{stderr}");
}

#[test] fn chat_can_target_individual_case_agent_directly() {
    // Sanity: even with a branch declared, individual agents (e.g.
    // billing) remain chat-able directly so users can isolate one
    // case-pipeline's behaviour without routing through the
    // classifier.  Same EOF-close shape as the v0.2 single-agent path.
    let mut child = bin().args(["chat", "billing"]).arg(router_demo_path())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn aislang");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait");
    assert!(out.status.success(),
        "single-agent chat against router-demo must still work; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("agent `billing`"),
        "banner must name the picked agent (not the branch); got:\n{stderr}");
    // Single-agent dispatch must NOT surface the router-pattern banner.
    assert!(!stderr.contains("router pattern"),
        "single-agent path must not advertise router-pattern dispatch; \
         got:\n{stderr}");
}
