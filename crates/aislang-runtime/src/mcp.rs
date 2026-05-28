//! MCP stdio client wrapper.
//!
//! Wraps `rmcp::transport::TokioChildProcess` + the `ServiceExt`
//! client-side handshake behind a four-method facade:
//!
//!   1. [`StdioChild::spawn`] — `docker run -i --rm -v … <image> <args>`,
//!      handshake, capture the running service handle.
//!   2. [`StdioChild::list_tools`] — enumerate the server's tool catalog.
//!   3. [`StdioChild::call_tool`] — invoke a tool by name with JSON args.
//!   4. [`StdioChild::shutdown`] — graceful teardown (kill child + drain
//!      reply pipe; bounded timeout, hard kill on overrun).
//!
//! The wire types ([`ToolSpec`], [`ToolResult`], [`ToolContent`]) are
//! aislang-owned newtypes, not rmcp re-exports. This means:
//!   - v0.2's `aislang-mcp-gateway` (SSE-fronted) reuses the same
//!     facade — only the transport changes, not the caller-visible
//!     surface.
//!   - `rmcp` major-version churn doesn't ripple into the chat loop.

use rmcp::{
    model::{CallToolRequestParams, RawContent},
    service::ServiceExt,
    transport::TokioChildProcess,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio::process::Command;

// week-9 Wed: OTel-GenAI instrumentation of MCP tool dispatch. Spans
// are opened in `Session::dispatch_tool_call` (the caller) — that's
// where the server *label* is known (from the resolver). `StdioChild`
// only knows the *image*, which is too low-level for a useful Jaeger
// tag. Keeping the wrap at the caller side also means `StdioChild`'s
// public API doesn't gain an OTel arg — direct programmatic uses
// stay zero-cost.
use opentelemetry::KeyValue;

// ============================================================================
// OTel-MCP attribute helpers (week 9 Wed)
//
// Convention: span name `"mcp <server>.<tool>"` (greppable in Jaeger
// search by typing `mcp `). Attributes use the `mcp.*` namespace —
// OTel-GenAI doesn't have a sanctioned MCP convention yet, so we pin
// these keys ourselves; tests below assert each constant verbatim.
//
// PII safety (Decision 21): we record *sizes* not *contents* of the
// argument map. Tool args can contain user-supplied secrets, paths,
// search queries — none of which should land in observability storage.
// `arguments_size_bytes` is enough to spot anomalies (a tool called
// with a 200KB payload is suspicious) without leaking the payload.
// ============================================================================

/// Pinned attribute keys for the MCP `call_tool` span.
pub(crate) mod mcp_otel {
    pub const KEY_SERVER_LABEL:  &str = "mcp.server.label";
    pub const KEY_TOOL_NAME:     &str = "mcp.tool.name";
    pub const KEY_ARGS_SIZE:     &str = "mcp.tool.arguments_size_bytes";
}

/// OTel span name for one MCP `call_tool` invocation. `"mcp <server>.<tool>"`
/// keeps the server-level grouping visible in Jaeger's search UI and
/// at the trace-tree level it appears as `mcp docs.read_text_file` —
/// distinct from the `chat <model>` LLM spans, easy to filter.
pub(crate) fn mcp_call_tool_span_name(server_label: &str, tool_name: &str) -> String {
    format!("mcp {server_label}.{tool_name}")
}

/// Build the three OTel attributes attached to an MCP call-tool span.
/// `arguments_size_bytes` is the byte length of the JSON-serialised
/// arg object, NOT the object's content — see PII rationale above.
pub(crate) fn mcp_call_tool_attrs(
    server_label: &str,
    tool_name:    &str,
    args_size_bytes: usize,
) -> Vec<KeyValue> {
    vec![
        KeyValue::new(mcp_otel::KEY_SERVER_LABEL, server_label.to_owned()),
        KeyValue::new(mcp_otel::KEY_TOOL_NAME,    tool_name.to_owned()),
        KeyValue::new(mcp_otel::KEY_ARGS_SIZE,    args_size_bytes as i64),
    ]
}

// ============================================================================
// Public types — aislang-owned newtypes (decoupled from rmcp internals)
// ============================================================================

/// A single tool exposed by an MCP server. Mirrors MCP `Tool` but
/// flattened to the fields v0.1's chat loop actually consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name:         String,
    pub description:  String,
    /// JSON Schema of the tool's input parameters. v0.1 passes this
    /// verbatim into LiteLLM's `tools[].function.parameters` field.
    pub input_schema: serde_json::Value,
}

/// Result of one tool call. `is_error` signals MCP-level errors
/// (the tool ran but the operation failed) — Rust-level errors
/// bubble up as [`McpError`] instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub is_error: bool,
    pub content:  Vec<ToolContent>,
}

/// One content fragment in a tool's response. MCP supports text,
/// image, audio, resource, … — v0.1 only consumes text for the
/// demo's filesystem use case; non-text fragments are surfaced
/// as `Unsupported` so the chat loop can show a stub message
/// rather than crash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    /// Any MCP content kind we haven't wired through yet. Carries
    /// the MCP type name for diagnostics.
    Unsupported { type_name: String },
}

// ============================================================================
// Errors
// ============================================================================

/// Errors surfaced by the MCP facade. Internal `rmcp` error variants
/// (`ClientInitializeError`, `ServiceError`, `ErrorData`) are
/// flattened into a `String` `cause` so v0.1 callers don't take a
/// dependency on rmcp's error hierarchy — which has churned across
/// 1.x point releases and will change again when v0.2's
/// `aislang-mcp-gateway` replaces the transport.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to spawn MCP child process for image `{image}`: {cause}")]
    Spawn { image: String, cause: String },

    #[error("MCP handshake failed for image `{image}`: {cause}")]
    Handshake { image: String, cause: String },

    #[error("MCP request failed: {0}")]
    Request(String),

    #[error("call_tool `{tool}` returned no content (server returned empty result)")]
    EmptyResult { tool: String },
}

impl From<rmcp::ServiceError> for McpError {
    fn from(e: rmcp::ServiceError) -> Self {
        McpError::Request(e.to_string())
    }
}

// ============================================================================
// `StdioChild` — the facade
// ============================================================================

/// Handle to a running stdio MCP server child process.
///
/// Owns the `rmcp` client handle. `Drop` does NOT auto-shut-down
/// the child — call [`StdioChild::shutdown`] explicitly so errors
/// surface to the caller. v0.1 chat loop calls `shutdown` in its
/// session-end path.
pub struct StdioChild {
    /// The rmcp `RunningService` client. Owns the transport.
    client: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,

    /// Echo of the image string for diagnostics — useful when
    /// multiple MCP children are spawned simultaneously and one
    /// errors.
    image:  String,
}

/// Per-server spawn configuration. Mirrors the v0.1 `mcp_server`
/// AST surface (image + optional mount + cli args) so the chat
/// loop can construct one of these directly from a `ResolvedProject`
/// without re-deriving fields.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub image:       String,
    pub args:        Vec<String>,
    /// Optional host directory bind-mounted into the container.
    /// In v0.1 we mount at `/data` (matches the mcp/filesystem
    /// convention); v0.2 may add a `mount_target` field to
    /// `mcp_server`. Source path is canonicalised at the caller.
    pub mount:       Option<PathBuf>,
    /// Extra `--env KEY=VALUE` pairs passed to docker. v0.1 doesn't
    /// wire `secret.scope`-driven env yet (week 7 owns that); the
    /// field is here so the public API stays stable.
    pub env:         Vec<(String, String)>,
}

impl StdioChild {
    /// Spawn the MCP server and complete the MCP handshake.
    ///
    /// Builds: `docker run -i --rm [--env K=V…] [-v MOUNT:/data:ro] IMAGE ARGS…`
    ///
    /// The `-i` (interactive stdin) + `--rm` (auto-cleanup) flags
    /// are the documented pattern in every Docker-published MCP
    /// image (see `mcp/filesystem` overview at
    /// <https://hub.docker.com/r/mcp/filesystem>). The `-v` host
    /// path is bind-mounted read-only at `/data` — matches the
    /// filesystem MCP server's expected layout.
    pub async fn spawn(cfg: &SpawnConfig) -> Result<Self, McpError> {
        let mut cmd = Command::new("docker");
        cmd.arg("run").arg("-i").arg("--rm");
        for (k, v) in &cfg.env {
            cmd.arg("--env").arg(format!("{k}={v}"));
        }
        if let Some(mount) = &cfg.mount {
            cmd.arg("-v").arg(format!("{}:/data:ro", mount.display()));
        }
        cmd.arg(&cfg.image);
        if cfg.mount.is_some() {
            // Most stdio MCP servers take the served directory as
            // their first positional arg. v0.1 hard-codes `/data`
            // to match the bind-mount target. If the image expects
            // something different the user can override via
            // SpawnConfig.args (which is appended LAST and wins).
            cmd.arg("/data");
        }
        for a in &cfg.args {
            cmd.arg(a);
        }

        let transport = TokioChildProcess::new(cmd).map_err(|e| McpError::Spawn {
            image: cfg.image.clone(),
            cause: e.to_string(),
        })?;

        let client = ().serve(transport).await.map_err(|e| McpError::Handshake {
            image: cfg.image.clone(),
            cause: e.to_string(),
        })?;

        Ok(Self { client, image: cfg.image.clone() })
    }

    /// Enumerate the tools the server advertises after handshake.
    ///
    /// Calls MCP `tools/list` with no pagination cursor — v0.1
    /// assumes the demo MCP servers return their full catalog in
    /// one page (which `mcp/filesystem`, `mcp/git`, et al. all do).
    /// Paginated catalogs are a v0.2 concern.
    pub async fn list_tools(&self) -> Result<Vec<ToolSpec>, McpError> {
        let result = self.client.peer().list_tools(Default::default()).await?;
        let tools = result.tools.into_iter().map(|t| ToolSpec {
            name:         t.name.into_owned(),
            description:  t.description.map(|d| d.into_owned()).unwrap_or_default(),
            // rmcp returns input_schema as Arc<JsonObject>; flatten
            // to a plain JSON value so callers don't carry an Arc
            // dependency.
            input_schema: serde_json::Value::Object((*t.input_schema).clone()),
        }).collect();
        Ok(tools)
    }

    /// Invoke a tool by name with a JSON-object argument map.
    ///
    /// The result's `content` vector preserves order. Non-text MCP
    /// content kinds are converted to [`ToolContent::Unsupported`]
    /// rather than dropped — v0.1's chat loop surfaces them with a
    /// stub message; v0.2 wires image/audio/resource fragments
    /// through proper LiteLLM multipart content.
    pub async fn call_tool(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ToolResult, McpError> {
        // CallToolRequestParams is #[non_exhaustive] in rmcp 1.7 — must
        // go through ::new + builders, not a struct literal.
        let params = CallToolRequestParams::new(name.to_string())
            .with_arguments(args);
        let result = self.client.peer().call_tool(params).await?;
        let is_error = result.is_error.unwrap_or(false);
        // `result.content` is Vec<Annotated<RawContent>> directly (not
        // Option<Vec<…>>) — no unwrap needed.
        let content: Vec<ToolContent> = result.content
            .into_iter()
            .map(|frag| match frag.raw {
                RawContent::Text(t)         => ToolContent::Text { text: t.text },
                RawContent::Image { .. }    => ToolContent::Unsupported { type_name: "image".into() },
                RawContent::Audio { .. }    => ToolContent::Unsupported { type_name: "audio".into() },
                RawContent::Resource(_)     => ToolContent::Unsupported { type_name: "resource".into() },
                RawContent::ResourceLink(_) => ToolContent::Unsupported { type_name: "resource_link".into() },
            })
            .collect();
        if content.is_empty() && !is_error {
            return Err(McpError::EmptyResult { tool: name.to_string() });
        }
        Ok(ToolResult { is_error, content })
    }

    /// Gracefully tear the child down. Waits up to 5 seconds for
    /// MCP to acknowledge shutdown + the process to exit; on
    /// timeout the child is hard-killed.
    ///
    /// Consumes `self` — the handle is unusable after.
    pub async fn shutdown(self) -> Result<(), McpError> {
        // `rmcp::service::RunningService::cancel` closes the
        // service + signals the transport to drop. The
        // TokioChildProcess transport handles SIGTERM + 5s wait
        // + SIGKILL internally per its docs.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            self.client.cancel(),
        ).await;
        Ok(())
    }

    /// Diagnostic accessor — used in error messages when multiple
    /// children are alive and one misbehaves.
    pub fn image(&self) -> &str { &self.image }
}

// ============================================================================
// Helpers: Command construction is tested independently of rmcp
// ============================================================================

/// Render the `docker run …` argument list a [`SpawnConfig`] would
/// produce, WITHOUT spawning anything. Used by unit tests to pin
/// the command-shape contract — flags in expected order, mount
/// formatted correctly, image last (then args).
///
/// Returns the args after the literal `docker` program name, so
/// the first element is always `"run"`.
pub fn render_docker_args(cfg: &SpawnConfig) -> Vec<String> {
    let mut out = vec!["run".to_string(), "-i".into(), "--rm".into()];
    for (k, v) in &cfg.env {
        out.push("--env".into());
        out.push(format!("{k}={v}"));
    }
    if let Some(mount) = &cfg.mount {
        out.push("-v".into());
        out.push(format!("{}:/data:ro", mount.display()));
    }
    out.push(cfg.image.clone());
    if cfg.mount.is_some() {
        out.push("/data".into());
    }
    for a in &cfg.args {
        out.push(a.clone());
    }
    out
}

/// Convenience: build a [`SpawnConfig`] from the four primitive
/// fields. Equivalent to the struct literal but lets callers stay
/// in `&str` land at the boundary.
pub fn config(image: &str, args: &[&str], mount: Option<&Path>) -> SpawnConfig {
    SpawnConfig {
        image: image.to_string(),
        args:  args.iter().map(|s| s.to_string()).collect(),
        mount: mount.map(|p| p.to_path_buf()),
        env:   Vec::new(),
    }
}

// ============================================================================
// Tests — unit tests for command construction + type round-trips
// ============================================================================
//
// Integration tests against the real `mcp/filesystem:1.0.2` container
// live in `tests/mcp_filesystem.rs` and are `#[ignore]`d by default
// (require Docker + network access). Run with:
//   cargo test -p aislang-runtime --test mcp_filesystem -- --ignored

#[cfg(test)]
mod tests {
    use super::*;

    // ---- render_docker_args ----

    #[test] fn minimal_command_is_run_i_rm_image() {
        // No mount, no args, no env → "run -i --rm <image>"
        let cfg = config("mcp/filesystem:1.0.2", &[], None);
        assert_eq!(
            render_docker_args(&cfg),
            vec!["run", "-i", "--rm", "mcp/filesystem:1.0.2"],
        );
    }

    #[test] fn mount_inserts_v_flag_and_data_arg() {
        // Mount adds `-v HOST:/data:ro` AND a trailing `/data`
        // positional (most stdio MCP servers take the served
        // directory as the first arg).
        let cfg = config(
            "mcp/filesystem:1.0.2",
            &[],
            Some(Path::new("/abs/knowledge")),
        );
        assert_eq!(
            render_docker_args(&cfg),
            vec![
                "run", "-i", "--rm",
                "-v", "/abs/knowledge:/data:ro",
                "mcp/filesystem:1.0.2",
                "/data",
            ],
        );
    }

    #[test] fn explicit_args_appended_after_image_and_data() {
        // User-supplied args follow the auto-inserted /data. This
        // matches docker's positional semantics: image first, then
        // every arg in order.
        let cfg = config(
            "mcp/filesystem:1.0.2",
            &["--read-only"],
            Some(Path::new("/abs/knowledge")),
        );
        let rendered = render_docker_args(&cfg);
        assert_eq!(rendered.last().unwrap(), "--read-only");
        let data_pos = rendered.iter().position(|s| s == "/data").unwrap();
        let ro_pos   = rendered.iter().position(|s| s == "--read-only").unwrap();
        assert!(data_pos < ro_pos,
            "/data must precede user args; got {rendered:?}");
    }

    #[test] fn env_flags_inserted_before_mount() {
        // `--env K=V` pairs come before `-v` for readability of
        // the resulting command (dockerfile-style: env, then volume,
        // then image).
        let cfg = SpawnConfig {
            image: "mcp/filesystem:1.0.2".into(),
            args:  vec![],
            mount: Some("/abs/knowledge".into()),
            env:   vec![("GITHUB_TOKEN".into(), "tok123".into())],
        };
        let rendered = render_docker_args(&cfg);
        let env_pos = rendered.iter().position(|s| s == "--env").unwrap();
        let v_pos   = rendered.iter().position(|s| s == "-v").unwrap();
        assert!(env_pos < v_pos,
            "--env must precede -v; got {rendered:?}");
        // The K=V pair stays one token (no splitting on `=`).
        let kv_pos = rendered.iter().position(|s| s == "GITHUB_TOKEN=tok123");
        assert!(kv_pos.is_some(),
            "K=V token must be intact; got {rendered:?}");
    }

    #[test] fn multiple_env_flags_emitted_in_order() {
        let cfg = SpawnConfig {
            image: "x".into(),
            args:  vec![],
            mount: None,
            env:   vec![
                ("A".into(), "1".into()),
                ("B".into(), "2".into()),
            ],
        };
        let rendered = render_docker_args(&cfg);
        let a = rendered.iter().position(|s| s == "A=1").unwrap();
        let b = rendered.iter().position(|s| s == "B=2").unwrap();
        assert!(a < b, "env iteration order must be preserved");
    }

    #[test] fn no_data_arg_when_no_mount() {
        // Without a mount we don't fabricate `/data` — the image
        // either doesn't need a served path or the user passes
        // it explicitly via args.
        let cfg = config("mcp/git:latest", &["--repo", "/tmp"], None);
        let rendered = render_docker_args(&cfg);
        assert!(!rendered.contains(&"/data".to_string()),
            "no auto /data when mount is None; got {rendered:?}");
        // User args still present.
        assert!(rendered.contains(&"--repo".to_string()));
    }

    // ---- ToolSpec / ToolResult JSON round-trips ----

    #[test] fn tool_spec_round_trips_through_json() {
        let ts = ToolSpec {
            name:         "list_dir".into(),
            description:  "List files in a directory".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        };
        let s = serde_json::to_string(&ts).unwrap();
        let back: ToolSpec = serde_json::from_str(&s).unwrap();
        assert_eq!(ts, back);
    }

    #[test] fn tool_result_text_content_round_trips() {
        let tr = ToolResult {
            is_error: false,
            content:  vec![
                ToolContent::Text { text: "hello".into() },
                ToolContent::Text { text: "world".into() },
            ],
        };
        let s = serde_json::to_string(&tr).unwrap();
        let back: ToolResult = serde_json::from_str(&s).unwrap();
        assert_eq!(tr, back);
    }

    #[test] fn tool_result_unsupported_content_carries_type_name() {
        // Pin the surface format for v0.2 wire-compat. The chat
        // loop will print `(unsupported content: image)` when it
        // sees this; the type_name preserves diagnostics.
        let tr = ToolResult {
            is_error: true,
            content:  vec![
                ToolContent::Unsupported { type_name: "image".into() },
            ],
        };
        let s = serde_json::to_string(&tr).unwrap();
        assert!(s.contains("\"kind\":\"unsupported\""),
            "tag field must be `kind` with snake_case value; got {s}");
        assert!(s.contains("\"type_name\":\"image\""),
            "type_name must be preserved; got {s}");
    }

    #[test] fn tool_content_tag_is_kind_field() {
        // Serialization shape contract: external tag at `kind`,
        // snake_case. Locked so downstream (LiteLLM tool message
        // formatting in Wed/Thu) can pattern-match without a
        // version dance.
        let frag = ToolContent::Text { text: "x".into() };
        let s = serde_json::to_string(&frag).unwrap();
        assert!(s.contains("\"kind\":\"text\""),
            "expected `kind:text`; got {s}");
        assert!(s.contains("\"text\":\"x\""));
    }

    // ---- config() helper ----

    #[test] fn config_helper_defaults_env_to_empty() {
        let cfg = config("img", &["a"], None);
        assert!(cfg.env.is_empty());
        assert_eq!(cfg.args, vec!["a"]);
        assert_eq!(cfg.image, "img");
    }

    // ---- OTel MCP span helpers (week 9 Wed) ----

    fn find_attr<'a>(attrs: &'a [KeyValue], key: &str) -> Option<&'a KeyValue> {
        attrs.iter().find(|kv| kv.key.as_str() == key)
    }

    #[test] fn mcp_otel_key_constants_match_documented_spec() {
        // Pinned strings — anything that breaks one of these breaks
        // downstream dashboards. PR diff that touches a key value
        // must touch this test too.
        assert_eq!(mcp_otel::KEY_SERVER_LABEL, "mcp.server.label");
        assert_eq!(mcp_otel::KEY_TOOL_NAME,    "mcp.tool.name");
        assert_eq!(mcp_otel::KEY_ARGS_SIZE,    "mcp.tool.arguments_size_bytes");
    }

    #[test] fn mcp_call_tool_span_name_follows_server_dot_tool_convention() {
        // "mcp <server>.<tool>" — distinct from "chat <model>" so the
        // Jaeger search panel can filter LLM vs MCP spans by prefix.
        assert_eq!(mcp_call_tool_span_name("docs", "read_text_file"),
                   "mcp docs.read_text_file");
        assert_eq!(mcp_call_tool_span_name("git",  "commit"),
                   "mcp git.commit");
        // Even if labels carry punctuation, the format stays composable.
        assert_eq!(mcp_call_tool_span_name("a-b", "c_d"), "mcp a-b.c_d");
    }

    #[test] fn mcp_call_tool_attrs_emits_all_three_keys() {
        let attrs = mcp_call_tool_attrs("docs", "read_text_file", 42);
        assert_eq!(attrs.len(), 3, "exactly three attrs (no more, no less)");
        let srv = find_attr(&attrs, mcp_otel::KEY_SERVER_LABEL)
            .expect("server.label must be present");
        assert_eq!(srv.value.as_str(), "docs");
        let tool = find_attr(&attrs, mcp_otel::KEY_TOOL_NAME)
            .expect("tool.name must be present");
        assert_eq!(tool.value.as_str(), "read_text_file");
        let size = find_attr(&attrs, mcp_otel::KEY_ARGS_SIZE)
            .expect("arguments_size_bytes must be present");
        assert_eq!(size.value.as_str(), "42");
    }

    #[test] fn mcp_call_tool_attrs_records_size_not_content() {
        // PII safety pin (Decision 21): a "sensitive" argument payload
        // — `{"password":"hunter2"}` — must NOT appear in attrs. Only
        // its byte length.
        let secret_args = serde_json::json!({"password": "hunter2"});
        let s = serde_json::to_string(&secret_args).unwrap();
        let len = s.len();

        let attrs = mcp_call_tool_attrs("docs", "auth_check", len);

        // None of the three attributes should contain the secret.
        for kv in &attrs {
            let rendered = kv.value.as_str();
            assert!(!rendered.contains("hunter2"),
                "attribute value {:?} leaked content (`hunter2`)", kv.key);
            assert!(!rendered.contains("password"),
                "attribute value {:?} leaked content (`password`)", kv.key);
        }

        // …but the byte-length must be present so anomalies are
        // still observable.
        let size = find_attr(&attrs, mcp_otel::KEY_ARGS_SIZE).unwrap();
        assert_eq!(size.value.as_str(), len.to_string());
    }

    #[test] fn mcp_call_tool_attrs_handles_empty_args() {
        // `{}` serializes to "{}" → 2 bytes. We should NOT skip the
        // attribute (zero would be misleading: it'd look like the
        // attribute is absent). Pin: empty args → exactly 2 bytes.
        let empty = serde_json::Value::Object(serde_json::Map::new());
        let s = serde_json::to_string(&empty).unwrap();
        assert_eq!(s, "{}");
        let attrs = mcp_call_tool_attrs("docs", "list_dir", s.len());
        let size = find_attr(&attrs, mcp_otel::KEY_ARGS_SIZE).unwrap();
        assert_eq!(size.value.as_str(), "2",
            "empty-arg-object should record 2 bytes (the `{{}}`), not 0");
    }
}
