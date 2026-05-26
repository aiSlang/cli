//! `Session` — coordinator for one `aislang chat` invocation.
//!
//! v0.1 Wed: holds the LiteLLM client + the system prompt + the
//! model alias, exposes `turn(user_input) -> String` as the single
//! per-keystroke entry point. The CLI reads from stdin, calls `turn`,
//! prints the result.
//!
//! Thursday adds MCP `StdioChild` handles (one per declared
//! `agent.tools[]` ref) + the tool-use iteration loop on top of
//! the same struct.

use crate::litellm::{self, LiteLlmClient, LiteLlmError};
use sha2::{Digest, Sha256};
use std::path::Path;
use thiserror::Error;

// ============================================================================
// Errors
// ============================================================================

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("system prompt file `{path}` not readable: {cause}")]
    PromptIo { path: String, cause: String },

    #[error("system prompt hash mismatch for `{path}`: \
             lockfile has `{expected}`, on-disk file is `{actual}`. \
             Re-run `aislang plan` to refresh the lockfile.")]
    PromptHashMismatch { path: String, expected: String, actual: String },

    #[error("LiteLLM error: {0}")]
    LiteLlm(#[from] LiteLlmError),
}

// ============================================================================
// `Session`
// ============================================================================

/// One-agent, one-conversation chat coordinator.
///
/// Holds enough context to translate a single user-stdin line into
/// a single LiteLLM round-trip. v0.1 is stateless across turns — no
/// conversation memory — by design: the demo gif is one turn, real
/// memory lands when the v0.2 containerized agent gains persistent
/// state.
pub struct Session {
    litellm:       LiteLlmClient,
    /// Model alias from the `.ais` agent.model ref (e.g. `"fast"`).
    /// LiteLLM resolves it to a concrete provider via the
    /// `model_list` config generated in week 5.
    model_label:   String,
    /// Resolved system-prompt contents — read once at session open,
    /// reused for every turn.
    system_prompt: String,
}

impl Session {
    /// Build a session ready to take user input.
    ///
    /// - `model_label`: the `.ais` `model` block label (e.g. `"fast"`).
    /// - `system_prompt`: the contents of the agent's system prompt
    ///   file. Caller is responsible for loading + verifying the hash
    ///   via [`load_system_prompt`] below.
    /// - `litellm_url`: pass `None` for the compose-default
    ///   `http://localhost:4000`; pass `Some(url)` to point at an
    ///   external LiteLLM (testing, or a `--remote` flag in v0.2).
    pub fn open(
        model_label:   impl Into<String>,
        system_prompt: impl Into<String>,
        litellm_url:   Option<&str>,
    ) -> Result<Self, SessionError> {
        let litellm = match litellm_url {
            Some(u) => LiteLlmClient::new(u),
            None    => LiteLlmClient::default_local(),
        }?;
        Ok(Self {
            litellm,
            model_label:   model_label.into(),
            system_prompt: system_prompt.into(),
        })
    }

    /// Run one user turn → one assistant response. Non-streaming.
    /// No tool wiring yet (Thursday).
    pub async fn turn(&self, user_input: &str) -> Result<String, SessionError> {
        let req = litellm::one_turn_request(
            &self.model_label,
            &self.system_prompt,
            user_input,
        );
        let content = self.litellm.send_chat(&req).await?;
        Ok(content)
    }

    /// Accessors — useful for diagnostic output ("connected to <agent>
    /// with model <label>") and for tests that pin construction
    /// invariants.
    pub fn model_label(&self)   -> &str { &self.model_label }
    pub fn system_prompt(&self) -> &str { &self.system_prompt }
}

// ============================================================================
// Prompt loading + hash verification
// ============================================================================

/// Load the system-prompt file's contents from disk and return them.
///
/// If `expected_hash` is `Some`, the file's SHA-256 is computed and
/// compared; mismatch errors out so the session never starts against
/// a prompt that drifted from what the lockfile recorded. If `None`,
/// the file is read without verification — the CLI uses this when
/// `--no-lock` was passed at plan time and no lockfile exists.
///
/// Hashes are hex-encoded (lowercase) to match the lockfile format
/// written by `aislang-resolver::lockfile`.
pub fn load_system_prompt(
    path: &Path,
    expected_hash: Option<&str>,
) -> Result<String, SessionError> {
    let bytes = std::fs::read(path).map_err(|e| SessionError::PromptIo {
        path:  path.display().to_string(),
        cause: e.to_string(),
    })?;
    if let Some(expected) = expected_hash {
        let actual = hex_sha256(&bytes);
        if actual != expected {
            return Err(SessionError::PromptHashMismatch {
                path:     path.display().to_string(),
                expected: expected.to_string(),
                actual,
            });
        }
    }
    // Prompt files MUST be valid UTF-8 (we treat them as LLM input).
    // If a binary file slips through here we return a clean error
    // rather than panicking.
    String::from_utf8(bytes).map_err(|e| SessionError::PromptIo {
        path:  path.display().to_string(),
        cause: format!("invalid UTF-8 in prompt: {e}"),
    })
}

/// Hex-encoded lowercase SHA-256 of the given bytes. Matches the
/// format `aislang-resolver` writes to `aislang.lock`.
pub fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        // Avoid format! per-byte allocations.
        const HEX: &[u8; 16] = b"0123456789abcdef";
        s.push(HEX[(b >> 4)   as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tempfile(name: &str, content: &[u8]) -> std::path::PathBuf {
        let dir  = std::env::temp_dir().join(format!("aislang-session-{}",
            std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create");
        f.write_all(content).expect("write");
        path
    }

    // ---- hex_sha256 ----

    #[test] fn hex_sha256_of_empty_input() {
        // Well-known: SHA-256 of "" is e3b0c44298fc1c149afbf4c8996fb924
        //                          27ae41e4649b934ca495991b7852b855
        assert_eq!(
            hex_sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
    }

    #[test] fn hex_sha256_is_lowercase_hex_64_chars() {
        let h = hex_sha256(b"aislang");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "expected lowercase hex; got `{h}`");
    }

    #[test] fn hex_sha256_matches_known_short_input() {
        // SHA-256("aislang") — cross-checked against `sha2` crate's
        // output as the canonical reference. Pin so a future refactor
        // of hex encoding can't quietly drift.
        assert_eq!(
            hex_sha256(b"aislang"),
            "ec3376490297c2f2c2db2851b7d82af21396ebd2ac3726becee65176192a611f",
        );
    }

    // ---- load_system_prompt ----

    #[test] fn load_prompt_returns_file_contents_when_hash_omitted() {
        let path = write_tempfile("ok.md", b"hello prompt");
        let out  = load_system_prompt(&path, None).expect("load");
        assert_eq!(out, "hello prompt");
        let _ = std::fs::remove_file(&path);
    }

    #[test] fn load_prompt_verifies_matching_hash() {
        let content = b"verify me";
        let expected = hex_sha256(content);
        let path = write_tempfile("verify.md", content);
        let out  = load_system_prompt(&path, Some(&expected)).expect("load");
        assert_eq!(out, "verify me");
        let _ = std::fs::remove_file(&path);
    }

    #[test] fn load_prompt_errors_on_hash_mismatch() {
        let path = write_tempfile("bad.md", b"on disk");
        let wrong = "0".repeat(64);
        let err   = load_system_prompt(&path, Some(&wrong))
            .err().expect("must error");
        match err {
            SessionError::PromptHashMismatch { expected, actual, .. } => {
                assert_eq!(expected, wrong);
                assert_ne!(actual, wrong);
                assert_eq!(actual.len(), 64);
            }
            other => panic!("expected PromptHashMismatch, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test] fn load_prompt_errors_when_file_missing() {
        let path = std::env::temp_dir().join("aislang-no-such-file-9b2f.md");
        let err  = load_system_prompt(&path, None).err().expect("must error");
        assert!(matches!(err, SessionError::PromptIo { .. }));
    }

    #[test] fn load_prompt_errors_on_non_utf8_content() {
        // Bytes 0xff 0xfe 0xfd are not valid UTF-8 start sequences.
        let path = write_tempfile("binary.md", &[0xff, 0xfe, 0xfd]);
        let err  = load_system_prompt(&path, None).err().expect("must error");
        match err {
            SessionError::PromptIo { cause, .. } => {
                assert!(cause.contains("invalid UTF-8"),
                    "cause must mention UTF-8; got {cause}");
            }
            other => panic!("expected PromptIo, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    // ---- Session.open ----

    #[test] fn session_open_accepts_explicit_url() {
        let s = Session::open("fast", "sys", Some("http://custom:8080"))
            .expect("open");
        assert_eq!(s.model_label(), "fast");
        assert_eq!(s.system_prompt(), "sys");
    }

    #[test] fn session_open_defaults_to_localhost_4000_when_url_none() {
        // Doesn't actually connect — just constructs the client.
        let _s = Session::open("fast", "sys", None).expect("open");
    }

    // ---- Session.turn against mock server ----

    #[tokio::test]
    async fn session_turn_returns_assistant_message_from_mock() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "ack: hi"}}]
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server).await;

        let s   = Session::open("fast", "you are helpful", Some(&server.uri()))
            .expect("open");
        let out = s.turn("hi").await.expect("turn ok");
        assert_eq!(out, "ack: hi");
    }

    #[tokio::test]
    async fn session_turn_surfaces_transport_error_when_litellm_down() {
        let s   = Session::open("fast", "sys", Some("http://127.0.0.1:1"))
            .expect("open");
        let err = s.turn("hi").await.err().expect("must error");
        assert!(matches!(err, SessionError::LiteLlm(LiteLlmError::Transport { .. })),
            "expected Transport; got {err:?}");
    }
}
