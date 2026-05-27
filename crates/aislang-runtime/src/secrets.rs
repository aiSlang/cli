//! Resolve `secret { source = "..." }` URIs into concrete string
//! values for injection into spawned MCP children (Decision 9, week 7 Fri).
//!
//! v0.1 supports two schemes — `env://VAR` and `file://path` — per
//! Decision 8 (LANGUAGE-SPEC §5.7). The parser (see
//! `aislang-core::parser::validate_secret_uri_scheme`) rejects every
//! other scheme at parse time, so the resolver here only ever sees
//! these two.
//!
//! ## Scoping by construction (Decision 9)
//!
//! The resolver works at the granularity of a single secret URI →
//! value. The CLI's chat-setup code calls it ONCE per
//! `(mcp_server, secret)` pair found in that server's `secrets = [...]`
//! list. Pairs are collected into a per-server `SpawnConfig.env`. A
//! secret not referenced by an MCP server's `secrets` list never
//! enters that server's env — there's no "deny list" mechanism
//! because there's no list to deny FROM; we only positively add what
//! was declared.

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("secret `{label}`: URI scheme `{scheme}://` not supported in v0.1 \
             (parser should have rejected this — bug in aislang)")]
    UnsupportedScheme { label: String, scheme: String },

    #[error("secret `{label}`: env var `{var}` is not set in the process \
             environment. Export it before running `aislang chat`.")]
    EnvVarMissing { label: String, var: String },

    #[error("secret `{label}`: file `{path}` not readable: {cause}")]
    FileIo { label: String, path: String, cause: String },

    #[error("secret `{label}`: file `{path}` is not valid UTF-8: {cause}")]
    FileNotUtf8 { label: String, path: String, cause: String },
}

/// Resolve one `secret.source` URI into a string value suitable for
/// injection as an environment variable in a spawned MCP container.
///
/// - `env://VAR` → look up `VAR` in process env. Error if missing
///   (treating it as the empty string would silently weaken security
///   in a way the user can't detect).
/// - `file://path` → read the file. Relative paths are resolved
///   against `source_dir` (the `.ais` source file's parent dir),
///   matching how prompt paths resolve. Contents are returned with
///   trailing newline / carriage returns trimmed — common when the
///   secret file is `echo $TOKEN > .secrets/foo`.
///
/// Any other URI scheme returns `UnsupportedScheme`, but the parser
/// rejects them earlier so callers shouldn't see this in practice.
pub fn resolve(
    label:      &str,
    source_uri: &str,
    source_dir: &Path,
) -> Result<String, SecretError> {
    if let Some(var) = source_uri.strip_prefix("env://") {
        return std::env::var(var).map_err(|_| SecretError::EnvVarMissing {
            label: label.to_string(),
            var:   var.to_string(),
        });
    }
    if let Some(path) = source_uri.strip_prefix("file://") {
        let abs = if Path::new(path).is_absolute() {
            std::path::PathBuf::from(path)
        } else {
            source_dir.join(path)
        };
        let bytes = std::fs::read(&abs).map_err(|e| SecretError::FileIo {
            label: label.to_string(),
            path:  abs.display().to_string(),
            cause: e.to_string(),
        })?;
        let s = std::str::from_utf8(&bytes).map_err(|e| SecretError::FileNotUtf8 {
            label: label.to_string(),
            path:  abs.display().to_string(),
            cause: e.to_string(),
        })?;
        // Trim trailing whitespace (newlines especially — `echo $TOKEN
        // > file` leaves a `\n` that breaks any container expecting
        // an exact token).
        return Ok(s.trim_end().to_string());
    }
    // Belt-and-suspenders: the parser rejects non-env/file schemes,
    // but if a code path somehow bypasses the parser we surface a
    // sensible error rather than panicking.
    let scheme = source_uri.split(':').next().unwrap_or("").to_string();
    Err(SecretError::UnsupportedScheme {
        label:  label.to_string(),
        scheme,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aislang-secrets-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ---- env:// ----

    #[test] fn env_uri_reads_from_process_env() {
        // Set a temp env var with a unique name to avoid collisions
        // with whatever the test runner already has set.
        let var = format!("AISLANG_TEST_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        std::env::set_var(&var, "the-secret-value");
        let uri = format!("env://{var}");
        let v = resolve("test_secret", &uri, Path::new("/tmp")).unwrap();
        assert_eq!(v, "the-secret-value");
        std::env::remove_var(&var);
    }

    #[test] fn env_uri_missing_var_errors() {
        // Variable that definitively isn't set.
        let var = "AISLANG_TEST_DEFINITELY_UNSET_VAR_98274";
        std::env::remove_var(var);
        let err = resolve("test_secret",
            &format!("env://{var}"), Path::new("/tmp")).err().unwrap();
        match err {
            SecretError::EnvVarMissing { label, var: v } => {
                assert_eq!(label, "test_secret");
                assert_eq!(v, var);
            }
            other => panic!("expected EnvVarMissing; got {other:?}"),
        }
    }

    // ---- file:// ----

    #[test] fn file_uri_reads_relative_path() {
        let dir = tempdir();
        let secret_path = dir.join("token.txt");
        let mut f = std::fs::File::create(&secret_path).unwrap();
        f.write_all(b"super-secret-token").unwrap();
        // URI uses a relative path; resolution joins with source_dir.
        let v = resolve("token", "file://token.txt", &dir).unwrap();
        assert_eq!(v, "super-secret-token");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn file_uri_reads_absolute_path() {
        let dir = tempdir();
        let secret_path = dir.join("token.txt");
        std::fs::write(&secret_path, b"abs-path-value").unwrap();
        // Absolute path → source_dir is ignored.
        let abs = secret_path.canonicalize().unwrap();
        let uri = format!("file://{}", abs.display());
        let v = resolve("token", &uri, Path::new("/some/other/dir")).unwrap();
        assert_eq!(v, "abs-path-value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn file_uri_trims_trailing_newline() {
        // The common case: `echo $TOKEN > file` leaves a trailing \n.
        // Containers expecting an exact token value would break.
        let dir = tempdir();
        let secret_path = dir.join("token.txt");
        std::fs::write(&secret_path, b"value-with-newline\n\n  \t").unwrap();
        let v = resolve("token", "file://token.txt", &dir).unwrap();
        assert_eq!(v, "value-with-newline",
            "trailing whitespace must be stripped; got {v:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test] fn file_uri_missing_errors() {
        let err = resolve("token", "file://./does-not-exist-9b2f.txt",
                          Path::new("/tmp")).err().unwrap();
        match err {
            SecretError::FileIo { label, path, .. } => {
                assert_eq!(label, "token");
                assert!(path.contains("does-not-exist-9b2f.txt"));
            }
            other => panic!("expected FileIo; got {other:?}"),
        }
    }

    #[test] fn file_uri_non_utf8_errors() {
        let dir = tempdir();
        let secret_path = dir.join("binary.bin");
        std::fs::write(&secret_path, &[0xff, 0xfe, 0xfd]).unwrap();
        let err = resolve("bin_secret", "file://binary.bin", &dir).err().unwrap();
        match err {
            SecretError::FileNotUtf8 { label, .. } => {
                assert_eq!(label, "bin_secret");
            }
            other => panic!("expected FileNotUtf8; got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- unsupported scheme (defense in depth — parser rejects this) ----

    #[test] fn unsupported_scheme_errors_cleanly() {
        // Belt-and-suspenders: in case a code path bypasses the
        // parser's URI-scheme validation, the resolver must NOT
        // panic. Surface a clean error.
        let err = resolve("k", "vault://secret/x", Path::new("/tmp"))
            .err().unwrap();
        match err {
            SecretError::UnsupportedScheme { label, scheme } => {
                assert_eq!(label, "k");
                assert_eq!(scheme, "vault");
            }
            other => panic!("expected UnsupportedScheme; got {other:?}"),
        }
    }
}
