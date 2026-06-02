//! v0.3 Week 18 Thu — `skill { … }` execution runtime.
//!
//! The grammar landed Wed (`crates/aislang-core::ast::Skill` +
//! parser/checker).  This module is the runtime executor: given a
//! resolved `Skill` + per-call context, run the declared command
//! (shell or MCP delegate) + parse the output per `output_type` +
//! return a typed [`SkillOutput`] to the caller.
//!
//! ## API surface
//!
//! - [`run_shell_skill`] — shell-out path.  Spawns `sh -c "<cmd>"`
//!   inside a per-skill sandbox (cwd + scrubbed PATH + pre-flight
//!   denylist), captures stdout/stderr, parses stdout per
//!   `output_type`.
//! - [`run_mcp_skill`]   — MCP-tool delegate path.  Calls the
//!   supplied tool on an already-spawned [`crate::mcp::StdioChild`]
//!   with the supplied JSON args, concatenates returned text
//!   fragments, parses per `output_type`.
//!
//! Both return `Result<SkillOutput, SkillError>` — the caller
//! (Phase-2 agent / template-context layer) decides how to surface
//! errors into the LLM-visible context.
//!
//! ## v0.3 Thu sandboxing — overview
//!
//! Shell skills are the higher-risk path (arbitrary command
//! execution).  Sandboxing in v0.3 Thu:
//!
//!   1. **Per-skill working directory.**  Default cwd =
//!      `<source_dir>/.aislang/skills/<label>/`.  Created on first
//!      run; subsequent runs reuse it (skills can persist state
//!      across calls within their own dir).
//!   2. **PATH scrub.**  Inherited PATH is REPLACED with a minimal
//!      `/usr/bin:/bin` (no `/usr/local/sbin` / `/usr/local/bin`
//!      where `sudo` typically lives in non-root user installs).
//!      Skills that need a specific binary should reference it by
//!      absolute path (the safer convention anyway).
//!   3. **Command-string denylist (pre-flight).**  Before spawning,
//!      the command is matched against a conservative set of
//!      patterns that signal destructive or escalation intent
//!      (`sudo `, `rm -rf /`, `:(){…}`, `mkfs`, etc.).  Match →
//!      reject with [`SkillError::DeniedCommand`] — never reaches
//!      the OS.
//!
//! **Not a full sandbox.**  Decision 44.x explicitly defers proper
//! seccomp / process-namespace isolation to v0.5+.  v0.3 Thu's
//! sandbox stops obvious shell-injection footguns + accidental
//! tampering; a determined attacker who controls .ais source can
//! still exfiltrate cwd-accessible files etc.  PII-conscious
//! deployments should run agent-runtime under a dedicated UID with
//! its own subdirectory tree until v0.5 hardens the surface.
//!
//! ## MCP path
//!
//! No sandboxing concerns at this layer — the MCP server is
//! already running inside its own container per the `mcp_server`
//! block.  Skill execution is just `StdioChild::call_tool` + output
//! shaping.  Reuses the same fleet of children the agent's `tools
//! = [mcp_server.<x>]` declaration spawned; no duplicate spawn.

use crate::mcp::{McpError, StdioChild, ToolContent};
use aislang_core::ast::OutputType;
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// Output type — runtime representation
// ============================================================================

/// One typed result of a skill run.  Variants mirror
/// [`OutputType`]: every successful `run_shell_skill` /
/// `run_mcp_skill` returns the variant matching the skill's
/// declared `output_type`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOutput {
    /// `output_type = string` — raw stdout / concatenated MCP text
    /// fragments, UTF-8 validated, trailing newline trimmed.
    String(String),
    /// `output_type = json` — stdout / MCP text parsed as a
    /// `serde_json::Value`.  Invalid JSON surfaces as
    /// [`SkillError::JsonParse`].
    Json(JsonValue),
    /// `output_type = list_string` — stdout / MCP text split on
    /// newline; trailing empty line dropped.  Useful for tools
    /// whose stdout is one item per line (`ls`, `git ls-files`).
    ListString(Vec<String>),
}

impl SkillOutput {
    /// Helper for callers that want to surface the result back into
    /// an LLM context as plain text regardless of the underlying
    /// variant.  `String` → as-is; `Json` → compact JSON;
    /// `ListString` → newline-joined.
    pub fn to_text(&self) -> String {
        match self {
            Self::String(s)     => s.clone(),
            Self::Json(v)       => v.to_string(),
            Self::ListString(v) => v.join("\n"),
        }
    }
}

// ============================================================================
// Errors
// ============================================================================

/// Failures specific to skill execution.  Distinct from
/// `SessionError` so callers can choose whether a skill failure
/// aborts the surrounding turn or just gets reported back to the
/// LLM as context.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// Pre-flight denylist matched — command never reached the OS.
    /// Carries the offending pattern so callers can surface it in
    /// the diagnostic.
    #[error("skill `{label}` shell command denied: matched pattern `{pattern}` \
             (denylist policy — v0.3 Thu sandbox; see `aislang-runtime::skills` docs)")]
    DeniedCommand { label: String, pattern: String },

    /// Spawning the shell command failed at the OS layer (binary
    /// not found, permission denied, etc.).
    #[error("skill `{label}` shell spawn failed: {cause}")]
    SpawnFailed { label: String, cause: String },

    /// Command exited with a non-zero status.  Stderr is captured
    /// for the diagnostic; stdout is dropped (incomplete results
    /// shouldn't be surfaced as the typed output).
    #[error("skill `{label}` exited with status {code}: {stderr}")]
    NonZeroExit { label: String, code: i32, stderr: String },

    /// `output_type = json` declared but stdout / MCP text wasn't
    /// valid JSON.
    #[error("skill `{label}` output_type=json but content failed to parse as JSON: {cause}")]
    JsonParse { label: String, cause: String },

    /// Command output was not valid UTF-8 (rare — most shell tools
    /// emit text — but binary stdout would land here).
    #[error("skill `{label}` produced non-UTF-8 output: {cause}")]
    InvalidUtf8 { label: String, cause: String },

    /// MCP child reported an error (transport failure, JSON-RPC
    /// error envelope, etc.).  `label` is the skill block label;
    /// `source` carries the underlying MCP error chain.
    #[error("skill `{label}` MCP delegate failed: {source}")]
    Mcp { label: String, #[source] source: McpError },

    /// Couldn't create the per-skill working directory.
    #[error("skill `{label}` working directory `{path}` not creatable: {cause}")]
    CwdSetup { label: String, path: String, cause: String },
}

// ============================================================================
// Sandbox policy — pre-flight denylist + PATH scrub
// ============================================================================

/// Conservative denylist of command-string substrings that signal
/// destructive or privilege-escalation intent.  Pre-flight match
/// short-circuits with [`SkillError::DeniedCommand`] before any
/// process is spawned.
///
/// **Not exhaustive** — a determined attacker who controls .ais
/// source can still craft commands that bypass these patterns
/// (whitespace games, base64-encoded payloads).  v0.3 Thu's
/// purpose is to stop accidental destruction + obvious footguns;
/// v0.5+ hardening would move to seccomp / namespace isolation
/// per Decision 44.x.
///
/// Patterns are matched as substrings — so e.g. `"sudo "` (trailing
/// space) catches `sudo apt install` but NOT `pseudonym=foo` (where
/// `sudo` appears as part of an unrelated word).
const DENYLIST_PATTERNS: &[&str] = &[
    "sudo ",
    "rm -rf /",
    "rm -rf ~",
    ":(){",          // fork-bomb signature
    "mkfs",
    "dd if=/dev/zero",
    "dd if=/dev/random",
    "/etc/passwd",   // tampering with system identity store
    "/etc/shadow",
    "> /dev/sda",    // raw-disk write
    "chmod -R 777 /",
];

/// Minimal PATH used for shell-skill execution.  Replaces whatever
/// the user's environment provides — `/usr/bin:/bin` covers POSIX-
/// standard tools (`ls`, `cat`, `grep`, `awk`, `sed`, `find`, `sh`,
/// `bash`) without exposing `/usr/local/sbin` or `~/bin` where
/// `sudo` / user-installed binaries typically live.
const SANDBOX_PATH: &str = "/usr/bin:/bin";

/// Walk the denylist + return the FIRST matching pattern.
///
/// Pure function — exposed so tests can pin the exact pattern set
/// + run_shell_skill can use it as a pre-flight without spawning.
pub fn check_denylist(command: &str) -> Option<&'static str> {
    DENYLIST_PATTERNS.iter().copied().find(|p| command.contains(p))
}

/// Construct + ensure-exists the per-skill working directory at
/// `<source_dir>/.aislang/skills/<label>/`.
fn ensure_skill_cwd(source_dir: &Path, label: &str) -> Result<PathBuf, SkillError> {
    let cwd = source_dir.join(".aislang").join("skills").join(label);
    std::fs::create_dir_all(&cwd).map_err(|e| SkillError::CwdSetup {
        label: label.to_string(),
        path:  cwd.display().to_string(),
        cause: e.to_string(),
    })?;
    Ok(cwd)
}

// ============================================================================
// run_shell_skill
// ============================================================================

/// Execute a shell-exec skill against the configured sandbox.
///
/// Inputs:
///   - `label`        — for diagnostics; matches the skill block's label.
///   - `command`      — the literal `exec = "<…>"` string.  Passed
///                      verbatim to `sh -c` so shell features (pipes,
///                      redirects, `$VAR` expansion) work.  Template-
///                      context substitution is a CALLER concern (the
///                      caller renders the template against context
///                      BEFORE invoking this fn).
///   - `source_dir`   — project root.  Per-skill cwd derived from this.
///   - `output_type`  — drives stdout parsing.
///
/// Sandboxing steps:
///   1. Pre-flight denylist check.
///   2. Ensure per-skill cwd exists.
///   3. Spawn `sh -c <command>` with `current_dir = cwd`, `PATH =
///      SANDBOX_PATH`, all other env vars CLEARED.
///   4. Wait for completion; capture stdout + stderr.
///   5. Non-zero exit → `NonZeroExit` (stdout discarded).
///   6. UTF-8 validate stdout, trim trailing newline.
///   7. Parse per `output_type` → [`SkillOutput`].
pub fn run_shell_skill(
    label:       &str,
    command:     &str,
    source_dir:  &Path,
    output_type: OutputType,
) -> Result<SkillOutput, SkillError> {
    // 1. Pre-flight denylist — stop obvious footguns before spawn.
    if let Some(pat) = check_denylist(command) {
        return Err(SkillError::DeniedCommand {
            label:   label.to_string(),
            pattern: pat.to_string(),
        });
    }
    // 2. Per-skill working directory.
    let cwd = ensure_skill_cwd(source_dir, label)?;

    // 3. Spawn with scrubbed environment.  `env_clear()` drops
    //    EVERY inherited var (HOME, USER, SHELL, ...); we add back
    //    only PATH + an explicit HOME pointing at the per-skill
    //    cwd so tools that want a writable home (`git config`, etc.)
    //    don't barf or escape the sandbox dir.
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&cwd)
        .env_clear()
        .env("PATH", SANDBOX_PATH)
        .env("HOME", &cwd)
        .output()
        .map_err(|e| SkillError::SpawnFailed {
            label: label.to_string(),
            cause: e.to_string(),
        })?;

    // 4. Status check.
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(SkillError::NonZeroExit {
            label:  label.to_string(),
            code,
            stderr,
        });
    }

    // 5. UTF-8 validate + trim trailing newline.
    let mut stdout = String::from_utf8(output.stdout)
        .map_err(|e| SkillError::InvalidUtf8 {
            label: label.to_string(),
            cause: e.to_string(),
        })?;
    if stdout.ends_with('\n') {
        stdout.pop();
        if stdout.ends_with('\r') { stdout.pop(); }
    }

    // 6. Shape per output_type.
    parse_output(label, stdout, output_type)
}

// ============================================================================
// run_mcp_skill
// ============================================================================

/// Execute an MCP-delegate skill against an already-spawned MCP
/// child.  The caller is responsible for routing the right
/// `StdioChild` here (typically by indexing into the agent's MCP
/// fleet by `server_label`).
///
/// Note `args` is `serde_json::Value` (not a `Map<String, Value>`)
/// because some MCP tools accept array-shaped args.  Internally the
/// rmcp call expects a `Map`, so non-object args are wrapped under
/// a single `"args"` key as a safety net.
pub async fn run_mcp_skill(
    label:       &str,
    server:      &StdioChild,
    tool_name:   &str,
    args:        JsonValue,
    output_type: OutputType,
) -> Result<SkillOutput, SkillError> {
    // Normalise args into the Map shape rmcp's call_tool wants.
    let args_obj = match args {
        JsonValue::Object(o) => o,
        other => {
            let mut m = serde_json::Map::new();
            m.insert("args".to_string(), other);
            m
        }
    };
    let result = server.call_tool(tool_name, args_obj).await
        .map_err(|e| SkillError::Mcp {
            label:  label.to_string(),
            source: e,
        })?;

    // Concatenate text fragments; non-text labelled inline.  Same
    // rule `session::dispatch_tool_call` uses so behaviour matches
    // between agent-tool-call paths and skill-MCP paths.
    let mut text = String::new();
    for frag in &result.content {
        if !text.is_empty() { text.push('\n'); }
        match frag {
            ToolContent::Text { text: t } => text.push_str(t),
            ToolContent::Unsupported { type_name } => {
                text.push_str(&format!("[unsupported content: {type_name}]"));
            }
        }
    }
    if result.is_error {
        // Convert MCP-error-flag results to runtime errors so callers
        // don't accidentally feed an error-payload back into context
        // as if it were a valid skill result.
        return Err(SkillError::NonZeroExit {
            label:  label.to_string(),
            code:   -1,
            stderr: text,
        });
    }
    parse_output(label, text, output_type)
}

// ============================================================================
// Output parsing — shared by shell + MCP paths
// ============================================================================

fn parse_output(
    label:       &str,
    raw:         String,
    output_type: OutputType,
) -> Result<SkillOutput, SkillError> {
    match output_type {
        OutputType::String => Ok(SkillOutput::String(raw)),
        OutputType::Json   => serde_json::from_str(&raw)
            .map(SkillOutput::Json)
            .map_err(|e| SkillError::JsonParse {
                label: label.to_string(),
                cause: e.to_string(),
            }),
        OutputType::ListString => {
            // Split on '\n', drop trailing empty (matches typical
            // `ls`/`git ls-files`-style output that has a trailing
            // newline-terminated final line).
            let mut v: Vec<String> = raw.split('\n')
                .map(|s| s.to_string()).collect();
            if v.last().map(|s| s.is_empty()).unwrap_or(false) {
                v.pop();
            }
            Ok(SkillOutput::ListString(v))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("aislang-skills-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    // ---- check_denylist + sandbox basics ------------------------ //

    #[test] fn denylist_catches_sudo_with_space() {
        assert_eq!(check_denylist("sudo apt install foo"),
            Some("sudo "));
    }

    #[test] fn denylist_does_not_catch_sudo_as_substring() {
        // PLAN DoD: "sandboxing denies the denylisted commands".
        // The flip: words that contain `sudo` as a substring (e.g.
        // a variable name `pseudonym=foo`) must NOT match.  We
        // require the trailing space to distinguish.
        assert_eq!(check_denylist("pseudonym=foo"), None);
        assert_eq!(check_denylist("echo studious"), None);
    }

    #[test] fn denylist_catches_fork_bomb_signature() {
        assert_eq!(check_denylist(":(){ :|:& };:"), Some(":(){"));
    }

    #[test] fn denylist_catches_rm_rf_root() {
        assert_eq!(check_denylist("rm -rf /"), Some("rm -rf /"));
        assert_eq!(check_denylist("rm -rf ~"), Some("rm -rf ~"));
    }

    #[test] fn denylist_catches_etc_passwd_tampering() {
        assert_eq!(check_denylist("cat > /etc/passwd"), Some("/etc/passwd"));
    }

    // ---- run_shell_skill happy paths --------------------------- //

    #[test] fn run_shell_skill_string_output_returns_stdout_trimmed() {
        // PLAN DoD: "shell-skill round-trip test: run `echo hello | jq .`
        // style + assert output".  Use plain echo to avoid relying
        // on `jq` being installed in the CI environment; the JSON
        // round-trip is exercised by the json_output test below.
        let src = tempdir();
        let out = run_shell_skill(
            "echo_hello", "echo hello",
            &src, OutputType::String).expect("run");
        match out {
            SkillOutput::String(s) => assert_eq!(s, "hello"),
            other => panic!("expected String, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_json_output_parses_stdout() {
        let src = tempdir();
        let out = run_shell_skill(
            "echo_json",
            r#"echo '{"name": "alice", "age": 30}'"#,
            &src, OutputType::Json).expect("run");
        match out {
            SkillOutput::Json(v) => {
                assert_eq!(v["name"], "alice");
                assert_eq!(v["age"], 30);
            }
            other => panic!("expected Json, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_list_string_output_splits_on_newlines() {
        let src = tempdir();
        // printf %s\\n emits "a\nb\nc\n" — trailing empty is dropped.
        let out = run_shell_skill(
            "list_three", r#"printf '%s\n' a b c"#,
            &src, OutputType::ListString).expect("run");
        match out {
            SkillOutput::ListString(v) => assert_eq!(v,
                vec!["a".to_string(), "b".to_string(), "c".to_string()]),
            other => panic!("expected ListString, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_creates_per_skill_cwd() {
        // Sandboxing: the per-skill cwd must exist after a run.
        // Pinned so the convention `<source_dir>/.aislang/skills/<label>/`
        // is testable / introspectable post-call.
        let src = tempdir();
        let _ = run_shell_skill(
            "cwd_creator", "true",
            &src, OutputType::String).expect("run");
        let cwd = src.join(".aislang").join("skills").join("cwd_creator");
        assert!(cwd.is_dir(),
            "per-skill cwd must exist after run; expected: {}",
            cwd.display());
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_path_is_scrubbed() {
        // Sandboxing: PATH inside the skill is reduced to
        // /usr/bin:/bin.  Echo back the PATH; assert it matches.
        let src = tempdir();
        let out = run_shell_skill(
            "echo_path", "echo $PATH",
            &src, OutputType::String).expect("run");
        if let SkillOutput::String(s) = out {
            assert_eq!(s, SANDBOX_PATH,
                "PATH inside skill must be scrubbed to SANDBOX_PATH");
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    // ---- run_shell_skill error paths ---------------------------- //

    #[test] fn run_shell_skill_denied_command_short_circuits_before_spawn() {
        // PLAN DoD: "sandboxing denies the denylisted commands".
        // No process is spawned — pre-flight returns DeniedCommand.
        let src = tempdir();
        let err = run_shell_skill(
            "evil", "sudo cat /etc/shadow",
            &src, OutputType::String).unwrap_err();
        match err {
            SkillError::DeniedCommand { label, pattern } => {
                assert_eq!(label, "evil");
                // Either pattern is fine — order in DENYLIST_PATTERNS
                // controls which fires first.  We just assert it's
                // one of the two patterns that match this command.
                assert!(pattern == "sudo " || pattern == "/etc/shadow",
                    "expected sudo or /etc/shadow pattern; got `{pattern}`");
            }
            other => panic!("expected DeniedCommand, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_non_zero_exit_surfaces_stderr() {
        let src = tempdir();
        let err = run_shell_skill(
            "fail", "echo oops 1>&2; exit 7",
            &src, OutputType::String).unwrap_err();
        match err {
            SkillError::NonZeroExit { label, code, stderr } => {
                assert_eq!(label, "fail");
                assert_eq!(code, 7);
                assert!(stderr.contains("oops"));
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    #[test] fn run_shell_skill_invalid_json_for_json_output_errors() {
        let src = tempdir();
        let err = run_shell_skill(
            "bad_json", "echo not-json-content",
            &src, OutputType::Json).unwrap_err();
        match err {
            SkillError::JsonParse { label, .. } => assert_eq!(label, "bad_json"),
            other => panic!("expected JsonParse, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&src);
    }

    // ---- SkillOutput::to_text ----------------------------------- //

    #[test] fn skill_output_to_text_handles_all_variants() {
        assert_eq!(SkillOutput::String("hi".into()).to_text(), "hi");
        assert_eq!(SkillOutput::Json(serde_json::json!({"k": 1})).to_text(),
            r#"{"k":1}"#);
        assert_eq!(SkillOutput::ListString(vec!["a".into(), "b".into()]).to_text(),
            "a\nb");
    }
}
