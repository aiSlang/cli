//! v0.4 Week 25 Mon — `aislang remote --target` HTTP client (Phase 3a,
//! mission deliverable #2 + Decision 46.9).
//!
//! The CLI's `aislang remote --target=<URL> <verb> …` delegates each
//! verb to the control plane over the Decision-46.1 REST surface.
//! This crate holds the request/response contract so a route or
//! body-shape change shows up against this client directly.
//!
//! Every request carries the two control-plane credentials:
//! - `Authorization: Bearer <token>` (the `AISLANG_CLOUD_TOKEN`)
//! - `X-Aislang-Tenant: <slug>` (Decision 46.5)
//!
//! ## Verb → endpoint map
//!
//! | CLI verb              | Endpoint                                  | Shape |
//! |-----------------------|-------------------------------------------|-------|
//! | `remote validate <id>`| `POST /v1/projects/<id>/plan?validate_only=true` | JSON |
//! | `remote plan <id>`    | `POST /v1/projects/<id>/plan`             | JSON  |
//! | `remote apply <id>`   | `POST /v1/projects/<id>/apply`            | SSE   |
//! | `remote destroy <id>` | `POST /v1/projects/<id>/destroy`          | SSE   |
//! | `remote status <id>`  | `GET  /v1/projects/<id>`                  | JSON  |
//!
//! `apply` / `destroy` stream Server-Sent Events; the client parses
//! the wire format incrementally + invokes a per-event callback so
//! the CLI can print progress live, then reports the terminal event
//! (`done` / `error`) so the caller can derive an exit code.

use reqwest::Client;
use serde_json::Value;

/// Tenant header name (Decision 46.5).  Sent as `X-Aislang-Tenant`
/// on every request; the control plane's auth middleware reads it to
/// scope the request to one tenant.
pub const TENANT_HEADER: &str = "X-Aislang-Tenant";

/// Errors the remote client surfaces.  `std::error::Error` so the
/// CLI's `anyhow`-based call sites get a clean `?`.
#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),
    /// Non-2xx response.  Carries the status + the (best-effort
    /// stringified) body so the CLI can show the server's error.
    #[error("control plane returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("response decode error: {0}")]
    Decode(String),
}

/// Terminal SSE event for a streaming verb (`apply` / `destroy`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseTerminal {
    /// Stream ended with `event: done` + its data payload.
    Done(Value),
    /// Stream ended with `event: error` + its data payload.
    Error(Value),
    /// Stream closed without a `done` or `error` event (e.g. the
    /// connection dropped mid-flight) — the CLI treats this as a
    /// failure.
    Incomplete,
}

/// Outcome of a streaming verb — every event seen, plus the terminal.
#[derive(Debug, Clone)]
pub struct SseOutcome {
    pub events:   Vec<(String, Value)>,
    pub terminal: SseTerminal,
}

impl SseOutcome {
    /// True when the stream ended on a `done` event.
    pub fn succeeded(&self) -> bool {
        matches!(self.terminal, SseTerminal::Done(_))
    }
}

/// Thin REST client over the control plane for one `(target, token,
/// tenant)` triple.
#[derive(Debug, Clone)]
pub struct RemoteClient {
    http:   Client,
    /// Base URL with no trailing slash, e.g. `https://api.aislang.cloud`.
    base:   String,
    token:  String,
    tenant: String,
}

impl RemoteClient {
    /// Build a client.  `target` may carry a trailing slash (trimmed).
    pub fn new(
        target: impl Into<String>,
        token:  impl Into<String>,
        tenant: impl Into<String>,
    ) -> Self {
        let base = target.into().trim_end_matches('/').to_string();
        Self { http: Client::new(), base, token: token.into(), tenant: tenant.into() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base, path)
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", self.token))
          .header(TENANT_HEADER, &self.tenant)
    }

    /// `GET /v1/projects/<id>` — `aislang remote status <id>`.
    /// Unwraps the server's `{"project": {…}}` envelope to the bare
    /// project row for caller ergonomics.
    pub async fn status(&self, id: &str) -> Result<Value, RemoteError> {
        let resp = self.auth(self.http.get(self.url(&format!("/projects/{id}"))))
            .send().await?;
        let body = Self::json_or_status(resp).await?;
        Ok(match body {
            Value::Object(mut m) => m.remove("project").unwrap_or(Value::Object(m)),
            other => other,
        })
    }

    /// `POST /v1/projects/<id>/plan?validate_only=true` —
    /// `aislang remote validate <id>`.
    pub async fn validate(&self, id: &str) -> Result<Value, RemoteError> {
        let resp = self.auth(self.http
            .post(self.url(&format!("/projects/{id}/plan")))
            .query(&[("validate_only", "true")]))
            .send().await?;
        Self::json_or_status(resp).await
    }

    /// `POST /v1/projects/<id>/plan` — `aislang remote plan <id>`.
    pub async fn plan(&self, id: &str) -> Result<Value, RemoteError> {
        let resp = self.auth(self.http
            .post(self.url(&format!("/projects/{id}/plan"))))
            .send().await?;
        Self::json_or_status(resp).await
    }

    /// `POST /v1/projects/<id>/apply` — `aislang remote apply <id>`.
    /// Streams SSE; `on_event(event_name, data)` fires per event.
    pub async fn apply<F>(&self, id: &str, on_event: F) -> Result<SseOutcome, RemoteError>
    where F: FnMut(&str, &Value) {
        let resp = self.auth(self.http
            .post(self.url(&format!("/projects/{id}/apply"))))
            .send().await?;
        self.stream_sse(resp, on_event).await
    }

    /// `POST /v1/projects/<id>/destroy` — `aislang remote destroy <id>`.
    pub async fn destroy<F>(&self, id: &str, on_event: F) -> Result<SseOutcome, RemoteError>
    where F: FnMut(&str, &Value) {
        let resp = self.auth(self.http
            .post(self.url(&format!("/projects/{id}/destroy"))))
            .send().await?;
        self.stream_sse(resp, on_event).await
    }

    /// Parse a JSON response, mapping non-2xx to [`RemoteError::Status`].
    async fn json_or_status(resp: reqwest::Response) -> Result<Value, RemoteError> {
        let status = resp.status();
        let bytes  = resp.bytes().await?;
        if status.is_success() {
            serde_json::from_slice(&bytes)
                .map_err(|e| RemoteError::Decode(e.to_string()))
        } else {
            Err(RemoteError::Status {
                status: status.as_u16(),
                body:   String::from_utf8_lossy(&bytes).to_string(),
            })
        }
    }

    /// Read an SSE stream chunk-by-chunk, parse the wire format, fire
    /// `on_event` per event, and return the full outcome.  A non-2xx
    /// status (e.g. 404 before the stream opens, or 429) is surfaced
    /// as [`RemoteError::Status`] — the server sends those as a normal
    /// JSON body, not a stream.
    async fn stream_sse<F>(
        &self, mut resp: reqwest::Response, mut on_event: F,
    ) -> Result<SseOutcome, RemoteError>
    where F: FnMut(&str, &Value) {
        let status = resp.status();
        if !status.is_success() {
            let bytes = resp.bytes().await?;
            return Err(RemoteError::Status {
                status: status.as_u16(),
                body:   String::from_utf8_lossy(&bytes).to_string(),
            });
        }

        let mut events   = Vec::new();
        let mut terminal = SseTerminal::Incomplete;
        let mut buf      = String::new();

        // Read chunks; SSE records are separated by a blank line
        // ("\n\n").  Accumulate into `buf` + drain complete records.
        while let Some(chunk) = resp.chunk().await? {
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(idx) = buf.find("\n\n") {
                let record: String = buf.drain(..idx + 2).collect();
                if let Some((name, data)) = parse_sse_record(&record) {
                    on_event(&name, &data);
                    if name == "done" {
                        terminal = SseTerminal::Done(data.clone());
                    } else if name == "error" {
                        terminal = SseTerminal::Error(data.clone());
                    }
                    events.push((name, data));
                }
            }
        }
        // Flush any trailing record that didn't end in a blank line.
        if !buf.trim().is_empty() {
            if let Some((name, data)) = parse_sse_record(&buf) {
                on_event(&name, &data);
                if name == "done" { terminal = SseTerminal::Done(data.clone()); }
                else if name == "error" { terminal = SseTerminal::Error(data.clone()); }
                events.push((name, data));
            }
        }

        Ok(SseOutcome { events, terminal })
    }
}

/// Parse one SSE record (`event: <name>\ndata: <json>`) into
/// `(name, data)`.  Returns None for comment/keep-alive records (a
/// leading `:` line with no `event:`/`data:`).
fn parse_sse_record(record: &str) -> Option<(String, Value)> {
    let mut name = None;
    let mut data = None;
    for line in record.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            name = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data = Some(rest.trim().to_string());
        }
    }
    match (name, data) {
        (Some(n), Some(d)) => {
            let v = serde_json::from_str(&d).unwrap_or(Value::String(d));
            Some((n, v))
        }
        _ => None,
    }
}
