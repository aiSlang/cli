//! In-CLI agent runtime for v0.1 — the chat loop that `aislang chat`
//! runs in-process.
//!
//! Scope by week:
//!   - **Week 6 Tue:** `mcp::StdioChild` — a thin facade over `rmcp`'s
//!     `TokioChildProcess` transport. Spawns the declared MCP server
//!     via `docker run -i --rm -v … <image> <args>`, lists its tools,
//!     calls them with JSON arguments, and shuts down cleanly. The
//!     facade isolates the rest of the runtime from `rmcp` API churn
//!     — Decision 5's v0.2 successor (`aislang-mcp-gateway` over SSE)
//!     swaps the transport without touching callers.
//!   - **Week 6 Wed:** `LiteLlmClient` + non-streaming chat loop.
//!   - **Week 6 Thu:** Tool-use loop, 8-iter cap, OpenAI/LiteLLM
//!     `tools` schema conversion.
//!   - **Week 7 Tue (this file):** `cost::cost_of_usage` — translate
//!     LiteLLM `UsageStats` into `Money` via catalog per-1k prices.
//!     Pure computation. Consumed by `Session::turn` (Wed) for the
//!     per-run cap and the spend tracker (Thu) for the daily cap.
//!
//! Async-first via `tokio`. The synchronous CLI (`aislang-cli`)
//! drives this crate via a per-command runtime built on demand —
//! we avoid making the whole CLI tokio-aware in v0.1 to keep the
//! existing sync subcommands (validate, plan, apply, destroy)
//! exactly as they are.

pub mod mcp;
// week-12 Mon: aiSlang's MCP server-side scaffolding for `aislang
// mcp-serve`.  Lives alongside `mcp.rs` (client side) so the rmcp
// dep is shared and aislang-owned wire types can be reused as the
// tool surface grows Tue-Thu.  PLAN-90-days-v0_2.md Phase 1 Week 12.
pub mod mcp_server;
pub mod litellm;
pub mod session;
pub mod cost;
pub mod spend;
pub mod secrets;
pub mod eval;
// week-10 Mon: applied-stack state file. `aislang apply` writes
// `.aislang/state.json`; `aislang destroy` reads + marks-torn-down;
// `aislang status` reads to render the stack/project summary.
pub mod state;
// week-9 Mon: OTel-GenAI tracing — SDK init + RootSpan lifecycle.
// Decision 18 (official Rust SDK) + Decision 22 (Jaeger UI URL to
// stderr). Child-span instrumentation in litellm/session lands Tue/Wed.
pub mod tracing;
// v0.3 Week 17 Tue: dir-vs-file project loader, hoisted from
// aislang-cli so `aislang mcp-serve` can dispatch multi-module
// dir inputs at chat-turn time.
pub mod project_loader;
// v0.3 Week 17 Thu: composition-pattern state machines
// (orchestrator / router / supervisor / reflector / planner_
// executor / coordinator).  Decision 44.2: runtime owns the
// dispatchers, CLI just calls in.  Thu ships orchestrator +
// router; Fri adds supervisor + reflector; Week 18 Mon closes
// the set with planner_executor + coordinator.
pub mod patterns;

pub use session::{
    Session, SessionError, ToolRegistry, BudgetConfig, MAX_TOOL_ITERS, TOOL_NAME_SEPARATOR,
    load_system_prompt, hex_sha256,
    SessionProgress,   // week 13 Wed — chat-turn streaming
};
pub use litellm::{
    LiteLlmClient, LiteLlmError, ChatRequest, ChatMessage, Role,
    Tool, ToolCall, ToolFunction, FunctionCall, UsageStats,
};
pub use mcp::{StdioChild, SpawnConfig, ToolSpec, ToolContent, McpError};
pub use mcp_server::{
    AislangServer,
    ServerError as McpServerError,
    serve_stdio as mcp_serve_stdio,
    serve_http  as mcp_serve_http,   // week-13 Mon
};
pub use cost::{cost_of_usage, sum as money_sum, format_money};
pub use spend::{SpendTracker, SpendError};
pub use state::{
    StateFile, StateError,
    read as read_state, write_atomic as write_state_atomic,
    mark_torn_down as mark_state_torn_down,
    state_path_in, new_applied as new_applied_state,
};
pub use secrets::{resolve as resolve_secret, SecretError};
pub use eval::{
    load_dataset, parse_dataset, EvalCase, DatasetError,
    EvalReport, CaseResult, aggregate as eval_aggregate,
};
pub use eval::metric::{score as metric_score, MetricError};
pub use tracing::{
    init_tracer, shutdown_tracer, jaeger_url, jaeger_url_with_span,
    RootSpan, TracingError,
};
// v0.3 Week 17 Tue: re-export the dir-vs-file loader.  CLI +
// mcp-serve are the two callers.
pub use project_loader::{
    load_project_input, LoadedProject, ProjectLoadError,
};
// v0.3 Week 17 Thu+Fri + Week 18 Mon: re-export pattern dispatchers
// + the shared `DispatchFuture` type alias used by their callbacks.
// Week 18 Mon closes the 6-pattern roster with planner_executor +
// coordinator.
pub use patterns::{
    coordinator, orchestrator, planner_executor, reflector, router,
    supervisor, CoordinatorOutcome, DispatchFuture, PatternError,
    PlannerExecutorOutcome, ReflectorOutcome, SupervisorOutcome,
};
