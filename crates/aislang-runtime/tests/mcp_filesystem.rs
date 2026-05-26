//! Integration test: spawn the real `mcp/filesystem:1.0.2` container,
//! list its tools, call one of them, shut down cleanly.
//!
//! `#[ignore]`d because it requires:
//!   - Docker on PATH and the daemon running
//!   - Network access (to pull the image first time)
//!   - ~30s wall-clock under cold cache
//!
//! Run explicitly:
//!   cargo test -p aislang-runtime --test mcp_filesystem -- --ignored
//!
//! Cleanup: the container is launched with `--rm`, so the container
//! itself disappears on shutdown. The image stays cached.

use aislang_runtime::mcp::{config, StdioChild, ToolContent};
use std::path::PathBuf;

/// Demo MCP image: real, published, stdio-only. See
/// <https://hub.docker.com/r/mcp/filesystem>.
const FS_IMAGE: &str = "mcp/filesystem:1.0.2";

/// Build a tempdir with one file in it so the filesystem MCP server
/// has something to list / read. Returns the canonical absolute
/// path (rmcp's child-process spawn doesn't run a shell, so any
/// `~` or relative path won't expand).
fn tempdir_with_one_file() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aislang-mcp-it-{}",
        std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir tempdir");
    std::fs::write(dir.join("hello.txt"), b"hello from aislang\n")
        .expect("write hello.txt");
    std::fs::canonicalize(&dir).expect("canonicalize tempdir")
}

// ----------------------------------------------------------------------------
// Tool listing — the minimum smoke test for v0.1 Tuesday's DoD
// ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + mcp/filesystem:1.0.2 image; run with --ignored"]
async fn list_tools_against_real_filesystem_mcp() {
    let dir = tempdir_with_one_file();
    let cfg = config(FS_IMAGE, &[], Some(&dir));

    let child = StdioChild::spawn(&cfg).await
        .expect("spawn + handshake against mcp/filesystem:1.0.2");

    let tools = child.list_tools().await
        .expect("list_tools succeeds");

    // mcp/filesystem advertises several tools — at minimum it has
    // some form of directory listing. We assert there's at least one
    // tool and that names look like the expected MCP slug shape
    // (lowercase + underscores, no spaces). We do NOT pin exact
    // names because the upstream image may add/rename tools across
    // patch releases; for v0.1 Tue we just need the wire path to
    // work.
    assert!(!tools.is_empty(),
        "expected at least one tool from filesystem MCP; got none");
    for t in &tools {
        assert!(!t.name.is_empty(), "tool name must not be empty");
        assert!(!t.name.contains(' '),
            "MCP tool names are slugs (no spaces); got `{}`", t.name);
        // The schema must be a valid JSON Schema object (the chat
        // loop will pass it verbatim into LiteLLM's tools[].function
        // .parameters field on Thursday).
        assert!(t.input_schema.is_object(),
            "tool `{}` input_schema must be a JSON object; got {:?}",
            t.name, t.input_schema);
    }

    child.shutdown().await.expect("shutdown succeeds");

    // Cleanup tempdir.
    let _ = std::fs::remove_dir_all(&dir);
}

// ----------------------------------------------------------------------------
// Tool calling — one round-trip that actually reads the tempfile
// ----------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker + mcp/filesystem:1.0.2 image; run with --ignored"]
async fn read_file_via_filesystem_mcp() {
    let dir = tempdir_with_one_file();
    let cfg = config(FS_IMAGE, &[], Some(&dir));

    let child = StdioChild::spawn(&cfg).await.expect("spawn");
    let tools = child.list_tools().await.expect("list_tools");

    // Pick the tool whose name looks like file-reading. The exact
    // slug varies (`read_file`, `read_text_file`, etc.). We accept
    // any of them. If the image changes to drop file-reading
    // entirely, this test surfaces the regression loudly.
    let read_tool = tools.iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            n.contains("read") && n.contains("file")
        })
        .unwrap_or_else(|| panic!(
            "no read-file-shaped tool in catalog; got: {:?}",
            tools.iter().map(|t| &t.name).collect::<Vec<_>>()));

    // `/data/hello.txt` because we bind-mounted at /data.
    let mut args = serde_json::Map::new();
    args.insert("path".into(), serde_json::Value::String("/data/hello.txt".into()));

    let result = child.call_tool(&read_tool.name, args).await
        .expect("call_tool succeeds");
    assert!(!result.is_error,
        "tool reported error reading /data/hello.txt; content: {:?}",
        result.content);

    // Look for our canary string in the returned text content.
    let any_text = result.content.iter().any(|c| match c {
        ToolContent::Text { text } => text.contains("hello from aislang"),
        _ => false,
    });
    assert!(any_text,
        "expected `hello from aislang` in tool result; got {:?}",
        result.content);

    child.shutdown().await.expect("shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

// ----------------------------------------------------------------------------
// Non-ignored sanity check: spawn-and-shutdown loop without Docker
// ----------------------------------------------------------------------------
//
// Validates that the wrapper doesn't panic when Docker is missing —
// it returns a clean McpError::Spawn. Runs in CI without docker
// installed.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_errors_cleanly_when_docker_missing_or_image_unknown() {
    // Use an image name that's syntactically valid but won't pull —
    // `does-not-exist-9b2f/nothing:none`. If Docker is present and
    // tries to pull it, the spawn will fail at handshake (image not
    // found). If Docker is absent, the spawn fails earlier. Either
    // way the error must be McpError::Spawn or McpError::Handshake.
    let cfg = config("does-not-exist-9b2f/nothing:none", &[], None);
    let result = StdioChild::spawn(&cfg).await;
    match result {
        Err(aislang_runtime::mcp::McpError::Spawn { .. })
        | Err(aislang_runtime::mcp::McpError::Handshake { .. }) => {
            // Expected — the precise variant depends on whether
            // docker is on PATH.
        }
        Err(other) => panic!("unexpected error variant: {other:?}"),
        Ok(_) => panic!("must not actually connect to a fake image"),
    }
}
