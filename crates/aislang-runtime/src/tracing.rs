//! Week 9 Mon — OTel-GenAI tracing: SDK init + root-span lifecycle.
//!
//! Decision 18: use the official `opentelemetry` + `opentelemetry_sdk`
//! + `opentelemetry-otlp` crates (0.32). Decision 19: OTLP HTTP/proto
//! over reqwest-rustls. Decision 22: emit a Jaeger trace URL to stderr
//! at the end of each command (`http://<jaeger-host>:16686/trace/<id>`),
//! stdout stays pipe-clean.
//!
//! Module shape, per Mon DoD:
//!   - `init_tracer(service_name)` builds an `SdkTracerProvider` with a
//!     batch OTLP HTTP/proto exporter, stores it in a `OnceLock` for
//!     idempotent re-init + later flush, and makes it the global tracer
//!     provider.
//!   - `shutdown_tracer()` flushes pending spans on a clean exit.
//!   - `RootSpan` opens one parent span, attaches its `Context` to the
//!     current task so child spans inherit, prints the Jaeger URL on
//!     drop. The dropped guard ends the span and detaches the context.
//!   - `for_chat(agent)` / `for_eval(eval)` are the two entry points the
//!     CLI uses; future MCP-call + LiteLLM spans will be children of the
//!     attached context (week 9 Tue/Wed).
//!
//! Endpoint resolution order (init time):
//!   1. `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` env var (full URL)
//!   2. `OTEL_EXPORTER_OTLP_ENDPOINT` env var (base URL; `/v1/traces` is
//!      appended for HTTP/proto per OTel spec)
//!   3. fallback to `http://localhost:4318/v1/traces` — the bundled
//!      Jaeger sidecar from `aislang plan` (week 9 Decision 22).
//!
//! Jaeger UI URL resolution (printed at end):
//!   1. `AISLANG_JAEGER_UI` env var (full base URL, no trailing slash)
//!   2. fallback `http://localhost:16686`
//!
//! Why a OnceLock rather than `tracing-subscriber` global state: tests
//! and any future repeated CLI invocations within a single process
//! must not panic-on-reinit. The OnceLock guarantees a single provider
//! per process; second `init_tracer` calls are a clean no-op.

use std::sync::OnceLock;

use opentelemetry::{
    global,
    trace::{Span, SpanKind, TraceContextExt, TraceId, Tracer},
    Context, KeyValue,
};
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{trace::SdkTracerProvider, Resource};

// ───────────────────────────────────────────────────────────────────
// Errors
// ───────────────────────────────────────────────────────────────────

/// Errors raised while standing up the tracer. Surfaced from
/// `aislang chat` / `aislang eval` as warnings, never fatal — Decision
/// 21: telemetry MUST NOT block primary user workflows. A failed
/// OTLP exporter init means tracing is off for this run; the CLI
/// keeps running normally.
#[derive(Debug, thiserror::Error)]
pub enum TracingError {
    #[error("build OTLP HTTP span exporter: {0}")]
    BuildExporter(String),
}

// ───────────────────────────────────────────────────────────────────
// Endpoint / URL helpers (pure — see tests)
// ───────────────────────────────────────────────────────────────────

const DEFAULT_OTLP_ENDPOINT:  &str = "http://localhost:4318/v1/traces";
const DEFAULT_JAEGER_UI_BASE: &str = "http://localhost:16686";

/// Build the OTLP HTTP/proto traces endpoint URL from the standard
/// OTel env vars, falling back to the bundled-Jaeger default.
fn resolve_otlp_endpoint() -> String {
    if let Ok(v) = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
        if !v.trim().is_empty() { return v; }
    }
    if let Ok(v) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        let v = v.trim();
        if !v.is_empty() {
            let base = v.trim_end_matches('/');
            return format!("{base}/v1/traces");
        }
    }
    DEFAULT_OTLP_ENDPOINT.to_owned()
}

/// Format a Jaeger UI URL for the given trace id. Lower-case 32-hex
/// is the Jaeger convention; `TraceId`'s `Display` already lowercases.
pub fn jaeger_url(trace_id: TraceId) -> String {
    let base = std::env::var("AISLANG_JAEGER_UI")
        .ok()
        .map(|s| s.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| DEFAULT_JAEGER_UI_BASE.to_owned());
    format!("{base}/trace/{trace_id}")
}

// ───────────────────────────────────────────────────────────────────
// Provider lifecycle
// ───────────────────────────────────────────────────────────────────

static PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();

/// Service-name tag used as the global tracer scope.
const TRACER_SCOPE: &str = "aislang";

/// Initialise the global tracer provider. Idempotent — only the first
/// successful call installs the provider; subsequent calls return
/// `Ok(())` without rebuilding anything.
///
/// Errors are non-fatal at the caller layer (see `TracingError` docs).
pub fn init_tracer(service_name: &str) -> Result<(), TracingError> {
    if PROVIDER.get().is_some() {
        return Ok(());
    }

    let endpoint = resolve_otlp_endpoint();

    // HTTP/proto over reqwest-rustls (Decision 19). Feature flags in
    // workspace Cargo.toml: `http-proto` + `reqwest-blocking-client` +
    // `reqwest-rustls`. The blocking client lets us pair the exporter
    // with the SDK's *simple* processor below — no background thread,
    // no runtime-lifecycle dance.
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| TracingError::BuildExporter(e.to_string()))?;

    let resource = Resource::builder()
        .with_service_name(service_name.to_owned())
        .with_attributes([
            // Pin our own version on the resource so Jaeger can show
            // "which aislang produced this trace" across daily runs.
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ])
        .build();

    // *Simple* span processor (Decision 18, revised Mon late). For
    // our scale — a few root + child spans per CLI invocation — sync
    // export at span-end time has trivial overhead and avoids the
    // background-thread / tokio-runtime-lifecycle pitfall a batch
    // processor would create at process exit.
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(span_exporter)
        .with_resource(resource)
        .build();

    global::set_tracer_provider(provider.clone());

    // First writer wins. If a parallel `init_tracer` raced us we
    // shutdown ours and let theirs stand — keeps the global provider
    // and the OnceLock entry referring to the same instance.
    if let Err(losing) = PROVIDER.set(provider) {
        // Best-effort flush so we don't drop any spans the loser might
        // have already created during the race.
        let _ = losing.shutdown();
    }
    Ok(())
}

/// Flush pending spans and shut the exporter down. Safe to call
/// even when `init_tracer` was never invoked (no-op then). Safe to
/// call multiple times — second call is a no-op because shutdown
/// short-circuits after first invocation.
pub fn shutdown_tracer() {
    if let Some(provider) = PROVIDER.get() {
        let _ = provider.shutdown();
    }
}

// ───────────────────────────────────────────────────────────────────
// RootSpan: parent-of-everything for one CLI command
// ───────────────────────────────────────────────────────────────────

/// One root span per CLI command.
///
/// Construct + drop `RootSpan` in **synchronous** code, *outside* any
/// `tokio::runtime::Runtime::block_on` block. The simple-processor +
/// blocking-reqwest exporter we use (see `init_tracer`) drives its
/// HTTP POST through reqwest's internal blocking client, which sets
/// up and tears down its own tiny tokio runtime per call. Dropping
/// that runtime inside an outer tokio async context would panic, so
/// the span's `.end()` (i.e. our `Drop`) must run in plain sync code.
///
/// The struct holds a cloneable `Context` rather than a thread-local
/// `ContextGuard` so the parent context can travel into a tokio task:
/// the async fn re-attaches it on its worker thread via
/// `root.context().attach()`. Child spans inside the async block
/// then read `Context::current()` and inherit this root as parent.
///
/// `Drop` ends the span (synchronously exporting via the simple
/// processor) and prints the Jaeger trace URL to stderr — Decision
/// 22's contract that stdout stays pipe-clean for JSON reports.
pub struct RootSpan {
    // Owned via Option so Drop can take() and let the Context drop in
    // sync ctx. The Context owns an Arc<Span> — when this is the last
    // ref, the SDK ends the span and the simple processor exports it.
    ctx:      Option<Context>,
    trace_id: TraceId,
    label:    String,
    kind:     RootKind,
}

#[derive(Debug, Clone, Copy)]
enum RootKind { Chat, Eval }

impl RootSpan {
    /// Open a root span for `aislang chat <agent> …`. The span name
    /// follows OTel-GenAI convention `chat <model>` only at the
    /// individual-turn layer (week 9 Tue); the *root* is named after
    /// our own operation so the trace is greppable by command.
    pub fn for_chat(agent_label: &str) -> Self {
        Self::open(RootKind::Chat, agent_label)
    }

    /// Open a root span for `aislang eval <name> …`.
    pub fn for_eval(eval_label: &str) -> Self {
        Self::open(RootKind::Eval, eval_label)
    }

    fn open(kind: RootKind, label: &str) -> Self {
        let op_name = match kind {
            RootKind::Chat => "aislang.chat",
            RootKind::Eval => "aislang.eval",
        };
        let attr_key = match kind {
            RootKind::Chat => "aislang.agent",
            RootKind::Eval => "aislang.eval",
        };

        let tracer = global::tracer(TRACER_SCOPE);
        let span = tracer.span_builder(op_name.to_owned())
            .with_kind(SpanKind::Internal)
            .with_attributes([
                KeyValue::new(attr_key, label.to_owned()),
            ])
            .start(&tracer);

        let trace_id = span.span_context().trace_id();

        // Move the span into a fresh Context. We do NOT attach it
        // here — the caller is responsible for `.attach()` inside the
        // tokio worker thread (Context is thread-local). The
        // RootSpan struct holds the Context so it lives until Drop;
        // when Drop runs (in sync ctx), the Arc<Span> reaches zero
        // refs and the simple processor exports synchronously.
        let ctx = Context::current_with_span(span);

        Self {
            ctx:      Some(ctx),
            trace_id,
            label:    label.to_owned(),
            kind,
        }
    }

    /// A clone of the root context. Pass into the async tokio task
    /// and call `.attach()` on the worker thread so child spans
    /// inherit this root as parent via `Context::current()`. The
    /// returned `ContextGuard` is held inside the async block — let
    /// it drop at the end of the async fn.
    pub fn context(&self) -> Context {
        self.ctx.clone().unwrap_or_else(Context::current)
    }

    /// Raw OTel trace id. Useful when EvalReport needs to embed it
    /// (week 9 Thu — Decision 24).
    pub fn trace_id(&self) -> TraceId { self.trace_id }

    /// Lower-case 32-hex form. Stable across SDK versions, matches
    /// the W3C `traceparent` header field. Convenient for log lines.
    pub fn trace_id_hex(&self) -> String { format!("{}", self.trace_id) }

    /// The Jaeger UI URL pointing at this trace. Honors
    /// `AISLANG_JAEGER_UI` env override.
    pub fn jaeger_url(&self) -> String { jaeger_url(self.trace_id) }
}

impl Drop for RootSpan {
    fn drop(&mut self) {
        // 1. Take and drop the Context → drops the inner Arc<Span> →
        //    SDK ends the span → simple processor exports it
        //    synchronously via blocking reqwest. MUST run in a sync
        //    (non-tokio-async) context — see the struct-level
        //    rationale above.
        let _ = self.ctx.take();

        // 2. Print the trace URL to stderr. Decision 22: this is the
        //    single user-visible breadcrumb that says "your trace is
        //    here". Skip if the trace_id is the invalid all-zeros one
        //    (means tracing wasn't initialised; no point printing).
        if self.trace_id == TraceId::INVALID {
            return;
        }
        let what = match self.kind {
            RootKind::Chat => format!("chat `{}`", self.label),
            RootKind::Eval => format!("eval `{}`", self.label),
        };
        eprintln!("trace ({what}): {}", jaeger_url(self.trace_id));
    }
}

// ===================================================================
// Tests
// ===================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- endpoint resolution -----------------------------------------

    /// With no env vars set, fall back to the bundled-Jaeger default.
    #[test]
    fn endpoint_falls_back_to_local_4318() {
        // Avoid leaking other tests' env state.
        let _g = EnvGuard::clear(&[
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "OTEL_EXPORTER_OTLP_ENDPOINT",
        ]);
        assert_eq!(resolve_otlp_endpoint(), "http://localhost:4318/v1/traces");
    }

    /// The traces-specific env var wins over the base one.
    #[test]
    fn endpoint_traces_env_var_wins() {
        let _g = EnvGuard::set(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT",        Some("http://base:4318")),
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", Some("http://override:4318/v1/traces")),
        ]);
        assert_eq!(resolve_otlp_endpoint(), "http://override:4318/v1/traces");
    }

    /// Base env var gets `/v1/traces` appended; trailing slash on the
    /// base must not produce a `//`.
    #[test]
    fn endpoint_base_env_var_appends_path() {
        let _g = EnvGuard::set(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT",        Some("http://otel:4318/")),
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", None),
        ]);
        assert_eq!(resolve_otlp_endpoint(), "http://otel:4318/v1/traces");
    }

    // ---- jaeger_url --------------------------------------------------

    /// URL format pin — fixed-shape, lower-case 32-hex tail. Locks in
    /// Decision 22's contract so a refactor doesn't silently break the
    /// stderr breadcrumb the user is told to copy.
    #[test]
    fn jaeger_url_format_pin() {
        let _g = EnvGuard::clear(&["AISLANG_JAEGER_UI"]);
        let id = TraceId::from_bytes([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
            0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10,
        ]);
        assert_eq!(
            jaeger_url(id),
            "http://localhost:16686/trace/0123456789abcdeffedcba9876543210",
        );
    }

    /// `AISLANG_JAEGER_UI` overrides the default base URL; trailing
    /// slash is normalised so we never emit `//trace/…`.
    #[test]
    fn jaeger_url_env_override_strips_trailing_slash() {
        let _g = EnvGuard::set(&[
            ("AISLANG_JAEGER_UI", Some("https://traces.example.com/")),
        ]);
        let id = TraceId::from_bytes([
            0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11,
            0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99,
        ]);
        assert_eq!(
            jaeger_url(id),
            "https://traces.example.com/trace/aabbccddeeff00112233445566778899",
        );
    }

    // ---- RootSpan lifecycle ------------------------------------------

    /// RootSpan must be constructible WITHOUT init_tracer ever being
    /// called — the global API hands out a no-op tracer in that case,
    /// and we want telemetry-off paths to remain panic-free.
    /// trace_id may be INVALID (all-zeros); accessor must still return
    /// a 32-hex string.
    #[test]
    fn rootspan_without_init_is_safe() {
        // NOTE: we deliberately do NOT call init_tracer() here.
        let root = RootSpan::for_chat("support");
        let hex  = root.trace_id_hex();
        assert_eq!(hex.len(), 32,
            "trace_id_hex must be 32 chars even when invalid; got `{hex}`");
        // Dropping must not panic.
        drop(root);
    }

    /// init_tracer is idempotent: a second call doesn't error, doesn't
    /// re-install a provider, doesn't double-shutdown later. Pin this
    /// behaviour because Cargo runs many tests in the same process.
    #[test]
    fn init_tracer_is_idempotent() {
        // Force a non-routable endpoint so the OTLP background task
        // doesn't actually try to deliver anything during tests.
        let _g = EnvGuard::set(&[
            ("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
             Some("http://127.0.0.1:1/v1/traces")),
        ]);
        // Either call may legitimately fail to build the exporter on
        // a stripped CI env; the contract we test is *idempotence*,
        // not unconditional success. So accept both Ok and Err from
        // the first call, but require the SAME outcome on the second.
        let r1 = init_tracer("aislang-test");
        let r2 = init_tracer("aislang-test");
        assert_eq!(r1.is_ok(), r2.is_ok(),
            "second init_tracer must mirror the first (idempotent), \
             got r1={r1:?} r2={r2:?}");
    }

    // ---- env guard ----
    //
    // Tests that touch process env vars must serialise via this guard
    // so concurrent test threads don't see each other's writes. We
    // can't use `cargo test --test-threads=1` selectively, so each
    // test that sets env owns its key for the duration of the test.

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        prior: Vec<(String, Option<String>)>,
        // Hold the lock for as long as the guard lives.
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(pairs: &[(&str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let mut prior = Vec::with_capacity(pairs.len());
            for (k, v) in pairs {
                prior.push(((*k).to_owned(), std::env::var(k).ok()));
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None      => std::env::remove_var(k),
                }
            }
            Self { prior, _lock: lock }
        }
        fn clear(keys: &[&str]) -> Self {
            let pairs: Vec<(&str, Option<&str>)> =
                keys.iter().map(|k| (*k, None)).collect();
            Self::set(&pairs)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.prior.drain(..) {
                match v {
                    Some(val) => std::env::set_var(&k, val),
                    None      => std::env::remove_var(&k),
                }
            }
        }
    }
}
