//! v0.3 Week 18 Thu — `aislang-runtime::skills::run_mcp_skill`
//! integration test against the real `mcp/filesystem:1.0.2` image.
//!
//! Mirrors `mcp_filesystem.rs`'s scaffolding (tempdir + Docker-spawned
//! child) but exercises the SKILL execution layer (typed output
//! shaping) rather than the bare `StdioChild::call_tool` round-trip.
//!
//! `#[ignore]`d for the same reasons mcp_filesystem.rs is ignored:
//!   - Docker on PATH + daemon running
//!   - Network access (pull image first time)
//!   - ~30s wall-clock under cold cache
//!
//! Run explicitly:
//!   cargo test -p aislang-runtime --test skills_mcp -- --ignored

use aislang_core::ast::OutputType;
use aislang_runtime::mcp::{config, StdioChild};
use aislang_runtime::skills::{run_mcp_skill, SkillOutput};
use std::path::PathBuf;

const FS_IMAGE: &str = "mcp/filesystem:1.0.2";

/// Same tempdir shape as `mcp_filesystem::tempdir_with_one_file`.
fn tempdir_with_one_file() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aislang-skills-mcp-it-{}",
        std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir tempdir");
    std::fs::write(dir.join("hello.txt"), b"hello from aislang\n")
        .expect("write hello.txt");
    std::fs::canonicalize(&dir).expect("canonicalize tempdir")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + mcp/filesystem:1.0.2 image; run with --ignored"]
async fn run_mcp_skill_round_trip_against_real_filesystem_server() {
    // PLAN DoD: "mcp-skill round-trip test: delegate to a mock
    // mcp_server".  We use the real `mcp/filesystem` image instead
    // of a mock because it exercises the FULL stdio handshake +
    // tool-call wire path that a mock would have to fake.  Same
    // pattern + ignore-gate as `mcp_filesystem.rs`.
    let dir = tempdir_with_one_file();
    let cfg = config(FS_IMAGE, &[], Some(&dir));
    let child = StdioChild::spawn(&cfg).await.expect("spawn");

    // Pick a file-reading tool; mcp/filesystem may name it
    // `read_file` or `read_text_file` depending on patch level.
    let tools = child.list_tools().await.expect("list_tools");
    let read_tool = tools.iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            n.contains("read") && n.contains("file")
        })
        .expect("filesystem MCP must advertise a file-reading tool")
        .name.clone();

    // The container sees the tempdir at /projects; ask it to read
    // /projects/hello.txt (same shape as the existing
    // mcp_filesystem::read_file_via_filesystem_mcp test).
    let args = serde_json::json!({ "path": "/projects/hello.txt" });

    // ---- String output ---------------------------------------- //
    let out = run_mcp_skill(
        "read_hello",
        &child,
        &read_tool,
        args.clone(),
        OutputType::String,
    ).await.expect("run_mcp_skill string");
    match out {
        SkillOutput::String(s) => {
            // The filesystem MCP returns the file body verbatim;
            // we wrote `hello from aislang\n`.  trim() to drop the
            // trailing newline.
            assert!(s.contains("hello from aislang"),
                "string output must contain the file body; got: {s:?}");
        }
        other => panic!("expected String, got {other:?}"),
    }

    // ---- ListString output ------------------------------------ //
    // The same one-line file becomes a 1-element list (or 2 if the
    // server emits the trailing newline — both shapes are valid
    // depending on the MCP image's text-fragment formatting).
    let out = run_mcp_skill(
        "read_hello_as_list",
        &child,
        &read_tool,
        args.clone(),
        OutputType::ListString,
    ).await.expect("run_mcp_skill list");
    match out {
        SkillOutput::ListString(v) => {
            assert!(!v.is_empty(),
                "ListString output must be non-empty; got: {v:?}");
            assert!(v.iter().any(|s| s.contains("hello from aislang")),
                "list must contain the file body; got: {v:?}");
        }
        other => panic!("expected ListString, got {other:?}"),
    }

    child.shutdown().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}
