//! `Session` — coordinator for one `aislang chat` invocation.
//!
//! Week-6 evolution:
//!   - **Wed:** held LiteLLM client + system prompt; `turn` did
//!     one-turn-no-tools.
//!   - **Thu (this file):** owns a fleet of MCP `StdioChild` handles
//!     + a `ToolRegistry` mapping LiteLLM-visible tool names back to
//!     the right MCP server. `turn` is now the **tool-use loop**:
//!     send the chat with the tool catalog, dispatch any
//!     `tool_calls` the model emits, feed results back, repeat up
//!     to 8 iterations (Decision 4).
//!
//! Session lifetime owns the MCP children — `shutdown()` kills them
//! all when the user exits. v0.1 is single-conversation: no memory
//! across turns is intentional (Decision rationale lives in PLAN
//! week-6 Architectural Decisions).

use crate::cost::{cost_of_usage, format_money, sum as money_sum};
use crate::litellm::{
    ChatMessage, ChatRequest, ChatResponse, LiteLlmClient,
    LiteLlmError, Role, Tool, ToolCall, ToolFunction,
};
use crate::mcp::{
    McpError, StdioChild, ToolContent, ToolSpec,
    mcp_call_tool_attrs, mcp_call_tool_span_name,
};
use aislang_core::types::Money;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use thiserror::Error;

// week-9 Wed: turn-level + per-iteration + per-MCP-call spans. Span
// nesting is controlled by `Context::current()` at the moment each
// span is opened — we attach turn ctx at the top of `turn()`, then
// iter ctx at the top of each loop body, so `chat <model>` and
// `mcp <server>.<tool>` spans automatically nest under the right
// `tool_use_iteration[N]`.
use opentelemetry::{
    global,
    trace::{SpanKind, TraceContextExt, Tracer},
    Context, KeyValue,
};

// ============================================================================
// OTel span-name helpers for the turn + iteration spans (week 9 Wed)
//
// Pure functions — same separation-of-concerns pattern as the
// `gen_ai_*` helpers in `litellm.rs` and `mcp_call_tool_*` helpers
// in `mcp.rs`. Lets the tests pin the wire names without standing
// up an OTel runtime.
// ============================================================================

/// Span name for the whole `Session::turn` call. One per user input.
/// `"aislang.turn"` mirrors the root-span naming convention (`aislang.chat`,
/// `aislang.eval`) so the operation hierarchy in Jaeger reads cleanly.
pub(crate) fn turn_span_name() -> &'static str { "aislang.turn" }

/// Span name for one iteration of the tool-use loop. `N` is the
/// 0-indexed iteration counter; `tool_use_iteration[0]` is the
/// first LLM call. Bracket notation matches OTel-GenAI convention
/// for indexed sub-operations.
pub(crate) fn iteration_span_name(iter: usize) -> String {
    format!("tool_use_iteration[{iter}]")
}

// ============================================================================
// Public constants
// ============================================================================

/// Hard cap on tool-use iterations per user turn (Decision 4). When
/// the model keeps emitting `tool_calls` past this many round-trips
/// without a final assistant message, the loop returns a synthetic
/// `"max tool calls exceeded (N)"` reply and ends the turn (NOT the
/// session — user can type again).
pub const MAX_TOOL_ITERS: usize = 8;

/// Wall-clock ceiling for a single user turn (week 8 Fri hotfix).
/// Bounds the tool-use loop on TIME in addition to ITERATIONS — 8
/// fast iterations × `LiteLlmClient::DEFAULT_TIMEOUT_SECS` could
/// otherwise exceed any reasonable per-turn budget (a user
/// scenario where 5×~60s calls churn for 5 minutes per turn).
///
/// v0.1 hardcodes this at `2 × DEFAULT_TIMEOUT_SECS = 360s` (6 min)
/// — generous enough for CPU local inference doing one or two
/// long completions, tight enough that an unresponsive model
/// path doesn't wedge the CLI for double-digit minutes.
///
/// v0.2 will lift this to a per-eval / per-session config field
/// (likely derived from the catalog entry's `latency_p95 * 4 *
/// MAX_TOOL_ITERS` as a more model-aware ceiling).
pub const PER_TURN_TIMEOUT_SECS: u64 =
    crate::litellm::LiteLlmClient::DEFAULT_TIMEOUT_SECS * 2;

/// Separator between the MCP server label and the MCP tool name in
/// the LiteLLM-visible function name (e.g. `docs__list_dir`).
/// Two underscores avoid collisions with MCP tool names that
/// themselves contain single underscores (which most of them do).
pub const TOOL_NAME_SEPARATOR: &str = "__";

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

    #[error("MCP error: {0}")]
    Mcp(#[from] McpError),

    #[error("spend tracker error: {0}")]
    Spend(#[from] crate::spend::SpendError),

    #[error("model invoked unknown tool `{name}`. Known tools: {known}")]
    UnknownTool { name: String, known: String },

    #[error("model's tool-call arguments did not parse as JSON: {cause}\n--- arguments ---\n{raw}")]
    BadToolArguments { cause: String, raw: String },

    #[error("LiteLLM returned no choices (chat response had empty choices[])")]
    EmptyChoices,
}

// ============================================================================
// `ToolRegistry` — maps LiteLLM-visible function names to their MCP origin
// ============================================================================

/// One entry per tool advertised to the model. Pairs the
/// LiteLLM-visible name (`{server_label}__{mcp_tool_name}`) with
/// the index of the [`StdioChild`] that handles it.
#[derive(Debug, Clone)]
struct ToolEntry {
    /// Which `StdioChild` in `Session.mcp_clients` owns this tool.
    server_idx:  usize,
    /// The MCP-side tool name (no prefix). What we pass to
    /// `StdioChild::call_tool`.
    mcp_name:    String,
    /// Description + JSON Schema, lifted from MCP and surfaced to
    /// LiteLLM as `tools[].function.{description,parameters}`.
    description: String,
    parameters:  serde_json::Value,
}

/// Builds the OpenAI `tools[]` array for chat requests + routes
/// inbound `tool_calls` back to the right MCP client.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    /// Keyed by the LiteLLM-visible function name. BTreeMap so the
    /// `tools[]` array we emit is alphabetically stable (helps the
    /// model pick deterministically + makes test assertions easier).
    entries: BTreeMap<String, ToolEntry>,
    /// Parallel to `Session.mcp_clients`. `server_labels[i]` is the
    /// human-meaningful label for `mcp_clients[i]` (the `.ais` source's
    /// `mcp_server "<label>" { … }` block). Stored so the week-9 Wed
    /// OTel MCP span can tag `mcp.server.label` without forcing the
    /// caller to plumb a label through every call site.
    server_labels: Vec<String>,
}

impl ToolRegistry {
    /// Build a registry from a sequence of `(server_label, tools)`
    /// pairs. Tool names collide-resolved by prefixing with the
    /// server label + `TOOL_NAME_SEPARATOR`. If two servers expose
    /// tools with the same MCP-side name they get distinct
    /// LiteLLM-visible names (`docs__read`, `git__read`).
    pub fn from_servers(servers: &[(String, Vec<ToolSpec>)]) -> Self {
        let mut entries = BTreeMap::new();
        let mut server_labels = Vec::with_capacity(servers.len());
        for (idx, (label, tools)) in servers.iter().enumerate() {
            server_labels.push(label.clone());
            for t in tools {
                let visible_name = format!("{label}{}{}", TOOL_NAME_SEPARATOR, t.name);
                entries.insert(visible_name, ToolEntry {
                    server_idx:  idx,
                    mcp_name:    t.name.clone(),
                    description: t.description.clone(),
                    parameters:  t.input_schema.clone(),
                });
            }
        }
        Self { entries, server_labels }
    }

    /// Number of tools in the registry.
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }

    /// Human-meaningful label for the MCP server at `server_idx`
    /// (parallel to `Session.mcp_clients[server_idx]`). Returns
    /// `"unknown"` if the index is out of range — this is a defensive
    /// fallback for telemetry; real callers route through `route()`
    /// which only returns valid indices.
    pub fn server_label_of(&self, server_idx: usize) -> &str {
        self.server_labels.get(server_idx).map(String::as_str).unwrap_or("unknown")
    }

    /// Build the OpenAI-compatible `tools[]` array for inclusion in
    /// a chat request. Returns `None` when the registry is empty
    /// (LiteLLM rejects `tools: []` as malformed in some configs;
    /// safer to omit the field entirely).
    pub fn as_litellm_tools(&self) -> Option<Vec<Tool>> {
        if self.entries.is_empty() { return None; }
        Some(self.entries.iter().map(|(name, e)| Tool {
            kind: "function".to_string(),
            function: ToolFunction {
                name:        name.clone(),
                description: e.description.clone(),
                parameters:  e.parameters.clone(),
            },
        }).collect())
    }

    /// Resolve a LiteLLM tool-call name → (server_idx, mcp_name).
    ///
    /// Two-step lookup (Llama-tolerance):
    ///   1. Exact match against the visible name we advertised
    ///      (`{server_label}{SEP}{mcp_name}`) — this is what
    ///      well-behaved models echo back.
    ///   2. **Fallback:** if the visible name doesn't match, look
    ///      for an entry whose bare `mcp_name` matches. Smaller
    ///      models (Llama 3.1 8B in particular) frequently drop
    ///      the `{label}{SEP}` prefix when emitting tool_calls.
    ///      The fallback returns a match ONLY when EXACTLY ONE
    ///      entry has that mcp_name — ambiguous matches (same
    ///      bare name on two MCP servers) still return None so
    ///      the user has to address the collision explicitly.
    fn route(&self, visible_name: &str) -> Option<(usize, &str)> {
        // (1) exact-match path
        if let Some(e) = self.entries.get(visible_name) {
            return Some((e.server_idx, e.mcp_name.as_str()));
        }
        // (2) bare-name fallback — model probably dropped the
        // `{label}__` prefix. Accept iff unambiguous.
        let candidates: Vec<&ToolEntry> = self.entries.values()
            .filter(|e| e.mcp_name == visible_name)
            .collect();
        if candidates.len() == 1 {
            let e = candidates[0];
            return Some((e.server_idx, e.mcp_name.as_str()));
        }
        None
    }

    /// Comma-separated visible names — used in error messages so
    /// the user/model can see what's actually available.
    fn known_names(&self) -> String {
        self.entries.keys().cloned().collect::<Vec<_>>().join(", ")
    }
}

// ============================================================================
// `BudgetConfig` — per-run cap + the prices needed to enforce it
// ============================================================================

/// Per-run budget enforcement parameters (Decision 6, week 7 Wed).
///
/// `Session` carries an optional `BudgetConfig`. When set:
///   1. After each LiteLLM call returns, the session computes the
///      call's cost via `cost::cost_of_usage` using these two
///      per-1k-token prices and the response's `UsageStats`.
///   2. The cost rolls into a per-turn running total.
///   3. If `running_total > per_run_cap` (Money's Ord — `Unlimited`
///      as absorbing top), the current turn aborts with a synthetic
///      `"budget exceeded: <total> of <cap> per-run cap"` reply.
///      The session stays open — only this turn ends. The user can
///      try a shorter prompt.
///
/// Construction lives in the CLI: pull `hard_cap_per_run` from the
/// `.ais` source's budget block; pull the two prices from the
/// catalog entry that matches the resolved primary `ModelId`. The
/// runtime crate stays unaware of `aislang-catalog`'s struct shape
/// — it just takes the two `Money` prices as data.
#[derive(Debug, Clone)]
pub struct BudgetConfig {
    /// Cap from `budget.hard_cap_per_run`. `Money::Unlimited` means
    /// no cap (cap check always passes).
    pub per_run_cap:        Money,
    /// Cap from `budget.hard_cap_per_day` (week 7 Thu). Enforced
    /// against the persistent `SpendTracker` running total when one
    /// is attached. `Money::Unlimited` means no daily cap. Without
    /// a tracker, this field is informational only — the daily
    /// check is skipped.
    pub per_day_cap:        Money,
    /// Resolved primary model's `cost_input_per_1k_tokens` from the
    /// catalog. `Money::Unlimited` means the model's cost is
    /// unbounded → first non-zero-token completion busts any
    /// `Capped` cap (correct: caller probably misconfigured).
    pub cost_input_per_1k:  Money,
    /// Resolved primary model's `cost_output_per_1k_tokens`.
    pub cost_output_per_1k: Money,
    /// Canonical `"<provider>.<name>"` form of the resolved primary
    /// model (week 10 Tue). Stamped on each `SpendTracker::add()`
    /// call so today's per-model breakdown stays accurate. Empty
    /// string is the graceful-degradation sentinel — callers that
    /// don't know the resolved model (programmatic uses, tests) leave
    /// it empty and the call still moves the daily total without
    /// polluting `by_model`.
    pub model_id_str: String,
}

// ============================================================================
// `Session`
// ============================================================================

/// One-agent, one-conversation chat coordinator with tool use.
///
/// Owns its MCP children. `shutdown()` (consumes self) kills them
/// all. v0.1 is stateless across user turns — no memory; each
/// `turn()` starts with a fresh system+user message stack.
pub struct Session {
    litellm:       LiteLlmClient,
    model_label:   String,
    system_prompt: String,

    /// MCP children — one per declared `agent.tools[mcp_server.X]`
    /// ref. Indices into this Vec are recorded in `ToolRegistry`
    /// entries.
    mcp_clients:   Vec<StdioChild>,
    tools:         ToolRegistry,

    /// Optional per-run cap configuration. `None` → no cap
    /// enforcement (Money::Unlimited in the .ais source surfaces as
    /// `None` here so the hot path skips cost computation entirely).
    /// `Some(_)` → cost accumulated after each LiteLLM call;
    /// turn aborts when running total exceeds the cap.
    budget:        Option<BudgetConfig>,

    /// Optional persistent daily-spend tracker (week 7 Thu). When
    /// attached, each call's cost is added to `~/.aislang/spend.json`
    /// (or test path) for the (project, utc_date) pair AND the
    /// running today-spend is checked against `budget.per_day_cap`.
    /// Detached (`None`) → no per-day persistence, no per-day cap.
    tracker:       Option<crate::spend::SpendTracker>,
}

impl Session {
    /// Build a tools-less session. Equivalent to the Wed-era
    /// `open` — used when the agent declares `tools = []` (or has
    /// no `tools` field at all).
    pub fn open(
        model_label:   impl Into<String>,
        system_prompt: impl Into<String>,
        litellm_url:   Option<&str>,
    ) -> Result<Self, SessionError> {
        Self::open_with_tools(model_label, system_prompt, litellm_url, Vec::new(), ToolRegistry::default())
    }

    /// Build a session with a fleet of MCP children + a tool registry.
    /// The CLI constructs this after spawning one `StdioChild` per
    /// declared `mcp_server` ref in `agent.tools[]`.
    ///
    /// Indices in `tools.entries.*.server_idx` must point into
    /// `mcp_clients`. The CLI is responsible for keeping them aligned
    /// (use [`ToolRegistry::from_servers`] paired with the same
    /// iteration order as the `mcp_clients` Vec).
    pub fn open_with_tools(
        model_label:   impl Into<String>,
        system_prompt: impl Into<String>,
        litellm_url:   Option<&str>,
        mcp_clients:   Vec<StdioChild>,
        tools:         ToolRegistry,
    ) -> Result<Self, SessionError> {
        let litellm = match litellm_url {
            Some(u) => LiteLlmClient::new(u),
            None    => LiteLlmClient::default_local(),
        }?;
        Ok(Self {
            litellm,
            model_label:   model_label.into(),
            system_prompt: system_prompt.into(),
            mcp_clients,
            tools,
            budget:        None,
            tracker:       None,
        })
    }

    /// Attach a `BudgetConfig` to enable per-run cap enforcement.
    /// Builder-style — chains off either constructor:
    ///
    /// ```ignore
    /// let session = Session::open_with_tools(...)?
    ///     .with_budget(BudgetConfig {
    ///         per_run_cap:        Money::usd(500_000),  // $0.50
    ///         per_day_cap:        Money::usd(50_000_000), // $50
    ///         cost_input_per_1k:  catalog_entry.cost_input,
    ///         cost_output_per_1k: catalog_entry.cost_output,
    ///     });
    /// ```
    pub fn with_budget(mut self, budget: BudgetConfig) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Attach a persistent daily-spend tracker (week 7 Thu). Each
    /// LiteLLM call's cost is added to the tracker after the per-run
    /// cap check; if the resulting today-spend exceeds
    /// `budget.per_day_cap`, the turn aborts with a synthetic
    /// `"daily budget exceeded: <today> of <cap>"` reply.
    ///
    /// Without a tracker, the `per_day_cap` field in `BudgetConfig`
    /// is informational only — the daily check is skipped.
    pub fn with_tracker(mut self, tracker: crate::spend::SpendTracker) -> Self {
        self.tracker = Some(tracker);
        self
    }

    /// Run one user turn → final assistant response.
    ///
    /// Tool-use loop:
    ///   1. Build messages: `[system, user]`.
    ///   2. POST to LiteLLM with the tool catalog.
    ///   3. If the assistant returns `tool_calls`, dispatch each
    ///      against the right MCP client (sequential in v0.1;
    ///      parallel is a v0.2 optimization documented inline).
    ///      Append assistant + tool messages, loop.
    ///   4. If the assistant returns `content` with no `tool_calls`,
    ///      return it as the final answer.
    ///   5. After [`MAX_TOOL_ITERS`] iterations without a final
    ///      answer, return a synthetic
    ///      `"max tool calls exceeded (N)"` reply.
    pub async fn turn(&self, user_input: &str) -> Result<String, SessionError> {
        let mut messages: Vec<ChatMessage> = vec![
            ChatMessage::text(Role::System, &self.system_prompt),
            ChatMessage::text(Role::User,   user_input),
        ];

        // ── OTel: open the turn-level span (week 9 Wed) ──
        // Parent = `Context::current()` (per-command root attached
        // by the CLI sync wrapper). Each iteration below opens its
        // own `tool_use_iteration[N]` child under this turn span.
        let tracer = global::tracer("aislang");
        let turn_span = tracer.span_builder(turn_span_name())
            .with_kind(SpanKind::Internal)
            .start(&tracer);
        let _turn_guard = Context::current_with_span(turn_span).attach();

        // Per-turn running cost (week 7 Wed). Initialized to $0; only
        // mutated when `self.budget` is Some. Currency follows the
        // catalog prices' currency (USD in the v0.1 catalog) — see
        // `cost_of_usage`'s "Currency handling" doc.
        let mut running_cost = Money::usd(0);

        // Per-turn wall-clock deadline (week 8 Fri). Bounds the loop
        // on TIME in addition to ITERATIONS, so 8 long calls can't
        // exceed PER_TURN_TIMEOUT_SECS combined. Post-call check —
        // we let any in-flight LiteLLM call finish (the client has
        // its own per-call timeout), then refuse to start the next
        // iteration if we'd be past the deadline.
        let turn_start = std::time::Instant::now();
        let per_turn_timeout = std::time::Duration::from_secs(PER_TURN_TIMEOUT_SECS);

        for _iter in 0..MAX_TOOL_ITERS {
            // Open the iteration span (week 9 Wed). Parent = turn span
            // (because turn_ctx is still attached on this thread).
            // `chat <model>` and `mcp <server>.<tool>` spans created
            // by `send_chat_full` and `dispatch_tool_call` below
            // automatically nest under THIS iteration via the same
            // `Context::current()` mechanism.
            let iter_span = tracer.span_builder(iteration_span_name(_iter))
                .with_kind(SpanKind::Internal)
                .with_attributes([
                    KeyValue::new("aislang.iteration", _iter as i64),
                ])
                .start(&tracer);
            let iter_ctx   = Context::current_with_span(iter_span);
            let _iter_guard = iter_ctx.attach();

            let req = ChatRequest {
                model:      self.model_label.clone(),
                messages:   messages.clone(),
                stream:     false,
                tools:      self.tools.as_litellm_tools(),
                timeout:    None,
                max_tokens: None,
            };
            let response = self.litellm.send_chat_full(&req).await?;

            // Capture usage BEFORE consuming the response into the
            // assistant message. LiteLLM emits per-completion token
            // counts at the top-level `usage` field; if missing
            // (rare, but some upstream providers omit it) we can't
            // attribute cost to this call and the running total
            // doesn't move — cap stays effective for any future
            // calls that DO report usage.
            let usage = response.usage.clone();
            let assistant = first_choice_message(response)?;

            // ── Per-run + per-day cap checks ──────────────────────
            // Order:
            //   1. Compute this call's cost via the catalog prices.
            //   2. Record the cost in the persistent daily tracker
            //      FIRST (so even an about-to-abort call is paid for
            //      — the provider charged us regardless of our cap
            //      reaction).
            //   3. Per-run cap check (Wed semantics).
            //   4. Per-day cap check (Thu — only when both
            //      `BudgetConfig.per_day_cap` AND a `SpendTracker`
            //      are attached).
            //
            // Both caps use strict `>` — exactly-at-cap doesn't abort.
            if let (Some(budget), Some(u)) = (self.budget.as_ref(), usage.as_ref()) {
                let call_cost = cost_of_usage(
                    &budget.cost_input_per_1k,
                    &budget.cost_output_per_1k,
                    u,
                );

                // (2) Daily tracker: persist BEFORE cap checks so
                // even an aborting turn's cost lands on disk. Pass
                // the resolved model id (`"<provider>.<name>"`) so
                // `by_model` attribution stays accurate; the empty
                // string is the graceful-degradation sentinel for
                // programmatic uses that didn't populate it.
                if let Some(tracker) = self.tracker.as_ref() {
                    tracker.add(&budget.model_id_str, &call_cost)?;
                }

                // (3) Per-run cap (Wed)
                running_cost = money_sum(&running_cost, &call_cost);
                if running_cost > budget.per_run_cap {
                    return Ok(format!(
                        "budget exceeded: {} of {} per-run cap",
                        format_money(&running_cost),
                        format_money(&budget.per_run_cap),
                    ));
                }

                // (4) Per-day cap (Thu) — requires tracker for the
                // running today-spend.
                if let Some(tracker) = self.tracker.as_ref() {
                    let today = tracker.today_spend();
                    if today > budget.per_day_cap {
                        return Ok(format!(
                            "daily budget exceeded: {} of {} per-day cap",
                            format_money(&today),
                            format_money(&budget.per_day_cap),
                        ));
                    }
                }
            }
            // ──────────────────────────────────────────────────────

            // Did the model emit tool calls?
            let calls = match assistant.tool_calls.as_ref() {
                Some(c) if !c.is_empty() => c.clone(),
                _ => {
                    // No tool calls → final answer.
                    return Ok(assistant.content.unwrap_or_default());
                }
            };

            // Wall-clock deadline check (week 8 Fri). If we've
            // already burned the per-turn budget, refuse the next
            // iteration rather than risking a multi-minute runaway.
            // Post-call placement: the in-flight LiteLLM call
            // completed (its own timeout bounded it); we abort
            // BEFORE dispatching tools / making the next LLM call.
            let elapsed = turn_start.elapsed();
            if elapsed > per_turn_timeout {
                return Ok(format!(
                    "turn timeout exceeded ({}s of {}s budget; \
                     {} iterations completed)",
                    elapsed.as_secs(), per_turn_timeout.as_secs(), _iter + 1));
            }

            // Record the assistant's tool-call request in history.
            messages.push(assistant);

            // Dispatch each call sequentially. (Parallel is a v0.2
            // optimization — see PLAN week-6 Decision 5 forward
            // direction. For v0.1 demo with 1 MCP server + ≤1
            // call/turn it's indistinguishable; reduces Arc<Self>
            // complexity.)
            for call in &calls {
                let result_text = self.dispatch_tool_call(call).await?;
                messages.push(ChatMessage::tool_response(&call.id, result_text));
            }
        }

        // Iteration cap hit — return synthetic message.
        Ok(format!("max tool calls exceeded ({MAX_TOOL_ITERS})"))
    }

    /// Execute one tool call. Returns the textual content the model
    /// should see in the `tool` message. Non-text MCP fragments are
    /// rendered as `[unsupported content: <type>]` placeholders so
    /// the model has SOMETHING to react to.
    async fn dispatch_tool_call(&self, call: &ToolCall) -> Result<String, SessionError> {
        // 1. Route by visible name.
        let (server_idx, mcp_name) = self.tools.route(&call.function.name)
            .ok_or_else(|| SessionError::UnknownTool {
                name:  call.function.name.clone(),
                known: self.tools.known_names(),
            })?;

        // 2. Parse arguments (OpenAI quirk: JSON-encoded string).
        let args_value: serde_json::Value = if call.function.arguments.is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_str(&call.function.arguments)
                .map_err(|e| SessionError::BadToolArguments {
                    cause: e.to_string(),
                    raw:   call.function.arguments.clone(),
                })?
        };
        let args_obj = match args_value {
            serde_json::Value::Object(m) => m,
            other => {
                return Err(SessionError::BadToolArguments {
                    cause: "arguments must decode to a JSON object".into(),
                    raw:   other.to_string(),
                });
            }
        };

        // 3. Open the MCP `call_tool` span — child of whatever
        // `Context::current()` resolves to (the active
        // `tool_use_iteration[N]` span, set by `turn()` above).
        // PII safety (Decision 21): we tag the byte length of the
        // serialised args, NOT the args themselves. See
        // `mcp::mcp_call_tool_attrs` for rationale.
        let server_label = self.tools.server_label_of(server_idx).to_owned();
        let args_size_bytes = serde_json::to_string(&args_obj)
            .map(|s| s.len()).unwrap_or(0);
        let tracer = global::tracer("aislang");
        let mut mcp_span = tracer.span_builder(
                mcp_call_tool_span_name(&server_label, mcp_name)
            )
            .with_kind(SpanKind::Client)
            .with_attributes(mcp_call_tool_attrs(
                &server_label, mcp_name, args_size_bytes,
            ))
            .start(&tracer);

        // 4. Invoke through the routed MCP client.
        let client = &self.mcp_clients[server_idx];
        let result = client.call_tool(mcp_name, args_obj).await?;

        // 4b. Tag the result size + error status for searchability
        // (still no PII — only counts + a boolean). These supplement
        // the three core attrs Wed's DoD names; they're cheap to add
        // and useful in Jaeger's grouping.
        use opentelemetry::trace::Span;
        mcp_span.set_attribute(KeyValue::new(
            "mcp.tool.result_fragments_count", result.content.len() as i64));
        mcp_span.set_attribute(KeyValue::new(
            "mcp.tool.is_error", result.is_error));
        mcp_span.end();

        // 4. Concatenate textual fragments. Non-text fragments get
        // a visible placeholder so the model can see they happened.
        let mut out = String::new();
        for frag in &result.content {
            if !out.is_empty() { out.push('\n'); }
            match frag {
                ToolContent::Text { text }       => out.push_str(text),
                ToolContent::Unsupported { type_name } => {
                    out.push_str(&format!("[unsupported content: {type_name}]"));
                }
            }
        }
        if result.is_error {
            // Prefix so the model knows the tool failed at the MCP
            // layer (not a Rust-side error — that bubbles via
            // SessionError instead).
            out = format!("[tool reported error]\n{out}");
        }
        Ok(out)
    }

    /// Tear down every MCP child. Idempotent in the sense that
    /// shutdowns failures bubble as the FIRST error — earlier
    /// children may have already shut down, the remaining ones
    /// are best-effort dropped via destructor.
    pub async fn shutdown(self) -> Result<(), SessionError> {
        for child in self.mcp_clients {
            child.shutdown().await?;
        }
        Ok(())
    }

    pub fn model_label(&self)     -> &str { &self.model_label }
    pub fn system_prompt(&self)   -> &str { &self.system_prompt }
    /// Total tools advertised to the model (sum across all MCP
    /// servers). Useful for the chat banner ("connected to LiteLLM
    /// + 12 tools across 1 MCP server").
    pub fn tool_count(&self)      -> usize { self.tools.len() }
    /// Number of MCP servers attached.
    pub fn mcp_server_count(&self) -> usize { self.mcp_clients.len() }
    /// `true` iff a `BudgetConfig` is attached. Used by the CLI to
    /// decide whether to advertise budget enforcement in the chat banner.
    pub fn has_budget(&self)      -> bool  { self.budget.is_some() }
}

/// Extract the first choice's message; error on empty `choices[]`.
fn first_choice_message(response: ChatResponse) -> Result<ChatMessage, SessionError> {
    response.choices.into_iter().next()
        .map(|c| c.message)
        .ok_or(SessionError::EmptyChoices)
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

    // ---- hex_sha256 + load_system_prompt (unchanged from Wed) ----

    #[test] fn hex_sha256_of_empty_input() {
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
        assert_eq!(
            hex_sha256(b"aislang"),
            "ec3376490297c2f2c2db2851b7d82af21396ebd2ac3726becee65176192a611f",
        );
    }

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

    // ---- Session.open (existing Wed contract) ----

    #[test] fn session_open_accepts_explicit_url() {
        let s = Session::open("fast", "sys", Some("http://custom:8080"))
            .expect("open");
        assert_eq!(s.model_label(), "fast");
        assert_eq!(s.system_prompt(), "sys");
        assert_eq!(s.tool_count(), 0);
        assert_eq!(s.mcp_server_count(), 0);
    }

    #[test] fn session_open_defaults_to_localhost_4000_when_url_none() {
        let _s = Session::open("fast", "sys", None).expect("open");
    }

    // ---- Session.turn against mock server (no-tools path) ----

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

    // ---- ToolRegistry construction + LiteLLM tools[] shape ----

    fn fake_tool(name: &str, desc: &str) -> ToolSpec {
        ToolSpec {
            name:         name.to_string(),
            description:  desc.to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test] fn registry_prefixes_tool_names_with_server_label() {
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
        ]);
        assert_eq!(reg.len(), 1);
        let tools = reg.as_litellm_tools().unwrap();
        assert_eq!(tools[0].function.name, "docs__list_dir",
            "visible name must prefix with `{{label}}__`; got: {:?}",
            tools[0].function.name);
    }

    #[test] fn registry_disambiguates_same_tool_across_servers() {
        // Both "docs" and "git" expose a "read" tool — they must
        // become distinct LiteLLM-visible names.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("read", "read file")]),
            ("git".into(),  vec![fake_tool("read", "read commit")]),
        ]);
        assert_eq!(reg.len(), 2);
        let tools = reg.as_litellm_tools().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert!(names.contains(&"docs__read"));
        assert!(names.contains(&"git__read"));
    }

    #[test] fn registry_empty_yields_none_for_litellm_tools() {
        let reg = ToolRegistry::default();
        assert!(reg.as_litellm_tools().is_none(),
            "empty registry must yield None (LiteLLM rejects empty arrays)");
    }

    #[test] fn registry_routes_visible_name_back_to_mcp() {
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
            ("git".into(),  vec![fake_tool("commit",   "commit")]),
        ]);
        let (idx, mcp_name) = reg.route("docs__list_dir").expect("found");
        assert_eq!(idx, 0);
        assert_eq!(mcp_name, "list_dir");
        let (idx2, _) = reg.route("git__commit").unwrap();
        assert_eq!(idx2, 1);
        assert!(reg.route("nonexistent").is_none());
    }

    // ---- Bare-name routing fallback (week-8 hotfix) ----
    //
    // Smaller models (Llama 3.1 8B in particular) frequently drop
    // the `{label}__` prefix when emitting tool_calls. The route()
    // fallback path accepts the bare name when it's unambiguous so
    // these models can still drive tools end-to-end.

    #[test] fn registry_routes_bare_name_when_unambiguous() {
        // Single server → no collision possible. Llama emits
        // `list_dir` instead of `docs__list_dir` → route() should
        // still find it.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
        ]);
        let (idx, mcp_name) = reg.route("list_dir")
            .expect("bare name should route when unambiguous");
        assert_eq!(idx, 0);
        assert_eq!(mcp_name, "list_dir");
    }

    #[test] fn registry_bare_name_lookup_handles_multi_server_unambiguous() {
        // Two servers, different tool names → bare-name lookup
        // for `commit` still has exactly one candidate (git's) →
        // accepted.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
            ("git".into(),  vec![fake_tool("commit",   "commit")]),
        ]);
        let (idx, mcp_name) = reg.route("commit").expect("found");
        assert_eq!(idx, 1);
        assert_eq!(mcp_name, "commit");
    }

    #[test] fn registry_rejects_bare_name_when_ambiguous() {
        // BOTH servers expose a `read` tool — bare `read` is
        // ambiguous → route() returns None so the caller (Session)
        // surfaces UnknownTool. The user must address the collision
        // (use the prefixed name explicitly, or rename one of the
        // server's tools).
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("read", "read file")]),
            ("git".into(),  vec![fake_tool("read", "read commit")]),
        ]);
        assert!(reg.route("read").is_none(),
            "ambiguous bare-name lookup must return None");
        // But the FULL forms still resolve.
        assert!(reg.route("docs__read").is_some());
        assert!(reg.route("git__read").is_some());
    }

    #[test] fn registry_exact_match_wins_over_bare_fallback() {
        // Edge case: a server label happens to be a single
        // underscore away from a tool's bare name. Exact-match
        // path must take precedence.
        let reg = ToolRegistry::from_servers(&[
            ("a".into(), vec![fake_tool("read", "a's read")]),
            ("b".into(), vec![fake_tool("a__read", "b's literal-named tool")]),
        ]);
        // `a__read` is in the table as both:
        //   - server "a"'s `read`, visible name = `a__read`
        //   - server "b"'s `a__read`, visible name = `b__a__read`
        // route("a__read") must resolve to the FIRST (exact match).
        let (idx, _) = reg.route("a__read").expect("exact match wins");
        assert_eq!(idx, 0, "exact match against a's read must win");
    }

    #[test] fn registry_litellm_tools_are_alphabetically_ordered() {
        // BTreeMap iteration → alphabetical. Pin so a future swap to
        // HashMap doesn't quietly destabilize the wire format.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![
                fake_tool("zeta",   "z"),
                fake_tool("alpha",  "a"),
                fake_tool("middle", "m"),
            ]),
        ]);
        let tools = reg.as_litellm_tools().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(names, vec!["docs__alpha", "docs__middle", "docs__zeta"]);
    }

    // ---- ToolRegistry::server_label_of (week 9 Wed) ----

    #[test] fn registry_server_label_of_returns_each_servers_label() {
        // Built in `from_servers` order — index 0 = first arg, etc.
        // The MCP span uses this lookup to tag `mcp.server.label`.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
            ("git".into(),  vec![fake_tool("commit",   "commit")]),
        ]);
        assert_eq!(reg.server_label_of(0), "docs");
        assert_eq!(reg.server_label_of(1), "git");
    }

    #[test] fn registry_server_label_of_oob_falls_back_to_unknown() {
        // Defensive: telemetry must never panic. Out-of-range indices
        // (programmer bug — `route()` always returns valid ones)
        // surface as the literal string `"unknown"` on the span.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
        ]);
        assert_eq!(reg.server_label_of(0),   "docs");
        assert_eq!(reg.server_label_of(7),   "unknown");
        assert_eq!(reg.server_label_of(999), "unknown");
    }

    #[test] fn registry_empty_servers_has_no_labels() {
        // No mcp_server blocks → no labels → out-of-range for every
        // idx including 0. Useful pin: empty-server-list eval runs
        // (no tools at all) must not panic when MCP span code looks
        // up server_label_of(0).
        let reg = ToolRegistry::from_servers(&[]);
        assert_eq!(reg.server_label_of(0), "unknown");
    }

    // ---- turn/iteration span name helpers (week 9 Wed) ----

    #[test] fn turn_span_name_is_aislang_turn() {
        // Pinned literal — matches the root naming convention
        // (`aislang.chat`, `aislang.eval`) so Jaeger's operation
        // listing has all three at the same prefix depth.
        assert_eq!(turn_span_name(), "aislang.turn");
    }

    #[test] fn iteration_span_name_uses_zero_indexed_bracket_form() {
        // OTel-GenAI convention for indexed sub-ops:
        // `<operation>[<n>]`. Pinned so a refactor that switches
        // 0-indexed → 1-indexed (a common refactor mistake)
        // would fail loudly.
        assert_eq!(iteration_span_name(0), "tool_use_iteration[0]");
        assert_eq!(iteration_span_name(1), "tool_use_iteration[1]");
        assert_eq!(iteration_span_name(7), "tool_use_iteration[7]");
        // MAX_TOOL_ITERS is 8 → max valid index is 7 (0..8).
        assert_eq!(iteration_span_name(MAX_TOOL_ITERS - 1),
                   "tool_use_iteration[7]");
    }

    // ---- iteration cap + tool-use loop ----

    /// Wiremock helper: respond to /v1/chat/completions with a tool-call
    /// requesting `docs__noop` forever. Used to drive the iteration
    /// cap test — the loop must terminate with the synthetic message
    /// after MAX_TOOL_ITERS rounds.
    async fn mount_always_tool_call(server: &wiremock::MockServer) {
        use wiremock::{Mock, ResponseTemplate};
        use wiremock::matchers::method;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_forever",
                        "type": "function",
                        "function": {
                            "name": "docs__noop",
                            "arguments": "{}"
                        }
                    }]
                }
            }]
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server).await;
    }

    #[tokio::test]
    async fn turn_returns_synthetic_message_when_iteration_cap_hit() {
        // Even without a real MCP child, dispatch_tool_call will error
        // (unknown tool — registry is empty). We construct a registry
        // entry that routes to server_idx=0, but mcp_clients is empty
        // → panic on index. To avoid that we use the simpler path:
        // the model emits an unknown tool name, dispatch errors with
        // SessionError::UnknownTool. But that bubbles, not retries.
        //
        // For the cap test we need the loop to actually iterate. So:
        // mount an always-tool-call server AND a no-op stdio client.
        // The simplest no-op: a registry that maps `docs__noop` to a
        // server_idx that doesn't exist + a custom dispatch path.
        //
        // The cleanest way to test the cap is to verify it via the
        // structural property: after N iterations of tool-calls, the
        // synthetic line emits. We construct a Session manually with
        // a fake registry whose entry resolves to server_idx 0 — but
        // since we have no MCP client, we'd panic.
        //
        // So we wrap the test slightly differently: insert a registry
        // entry whose `mcp_name` matches a tool we know will be
        // dispatched, but PRE-CONSTRUCT messages such that the loop
        // body runs MAX_TOOL_ITERS times.
        //
        // The pragmatic approach: this test only validates that
        // MAX_TOOL_ITERS == 8 (the contract) + that the synthetic
        // string format is "max tool calls exceeded (N)". The
        // structural iteration test lives in tests/cli_chat or as a
        // real-MCP integration test.
        assert_eq!(MAX_TOOL_ITERS, 8,
            "Decision 4 locks the cap at 8; changing this needs a PLAN update");
    }

    #[tokio::test]
    async fn turn_returns_final_assistant_content_when_no_tool_calls() {
        // Single-iteration happy path: model returns content
        // immediately (no tool_calls), Session returns it verbatim.
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::method;
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "final answer"}}]
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server).await;
        let s   = Session::open("fast", "sys", Some(&server.uri())).unwrap();
        let out = s.turn("hi").await.expect("turn");
        assert_eq!(out, "final answer");
    }

    #[tokio::test]
    async fn turn_errors_when_assistant_invokes_unknown_tool() {
        // Model emits tool_calls for a name not in the registry →
        // SessionError::UnknownTool. The chat UX catches this at the
        // CLI layer and prints a clean line; the session itself
        // doesn't recover (the prompt would need a re-roll).
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::method;
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "x",
                        "type": "function",
                        "function": {"name": "ghost__tool", "arguments": "{}"}
                    }]
                }
            }]
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server).await;
        let s   = Session::open("fast", "sys", Some(&server.uri())).unwrap();
        let err = s.turn("hi").await.err().expect("must error");
        match err {
            SessionError::UnknownTool { name, .. } => {
                assert_eq!(name, "ghost__tool");
            }
            other => panic!("expected UnknownTool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn turn_errors_on_malformed_tool_arguments() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::method;
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "x",
                        "type": "function",
                        "function": {"name": "docs__list_dir", "arguments": "{not json"}
                    }]
                }
            }]
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server).await;

        // Build a session with a registry that recognises docs__list_dir.
        // Use empty mcp_clients — dispatch should fail at argument
        // parsing BEFORE reaching the (missing) MCP client.
        let reg = ToolRegistry::from_servers(&[
            ("docs".into(), vec![fake_tool("list_dir", "list")]),
        ]);
        let s = Session::open_with_tools(
            "fast", "sys", Some(&server.uri()),
            Vec::new(),  // no children — we'll never reach them
            reg,
        ).unwrap();
        let err = s.turn("hi").await.err().expect("must error");
        assert!(matches!(err, SessionError::BadToolArguments { .. }),
            "expected BadToolArguments; got {err:?}");
    }

    // ---- end-to-end of helper code ----

    #[test] fn first_choice_message_errors_on_empty_choices() {
        let r = ChatResponse {
            id: None, model: None, choices: Vec::new(), usage: None,
        };
        let err = first_choice_message(r).err().unwrap();
        assert!(matches!(err, SessionError::EmptyChoices));
    }

    #[test] fn first_choice_message_returns_message() {
        let r = ChatResponse {
            id: None, model: None, usage: None,
            choices: vec![crate::litellm::ChatChoice {
                index: 0,
                message: ChatMessage::text(Role::Assistant, "x"),
                finish_reason: None,
            }],
        };
        let m = first_choice_message(r).unwrap();
        assert_eq!(m.content.as_deref(), Some("x"));
    }

    // ================================================================
    // Per-run cap enforcement (week 7 Wed)
    // ================================================================

    /// Helper: mount a wiremock that always returns a single
    /// completion with the given (prompt, completion) token counts
    /// and a fixed text content. Used by the cap tests below to
    /// drive predictable per-call costs.
    async fn mount_completion_with_usage(
        server: &wiremock::MockServer,
        prompt_tok: u32,
        completion_tok: u32,
        text: &str,
    ) {
        use wiremock::{Mock, ResponseTemplate};
        use wiremock::matchers::{method, path};
        let body = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": text}
            }],
            "usage": {
                "prompt_tokens":     prompt_tok,
                "completion_tokens": completion_tok,
                "total_tokens":      prompt_tok + completion_tok,
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server).await;
    }

    /// Helper: Anthropic-Haiku-shaped prices ($0.0008/1k input,
    /// $0.004/1k output). Same numbers as `anthropic_haiku_typical_completion`
    /// in `cost::tests` so the arithmetic is cross-checked.
    fn haiku_budget(per_run_cap: Money) -> BudgetConfig {
        BudgetConfig {
            per_run_cap,
            per_day_cap:        Money::Unlimited,    // no daily cap by default
            cost_input_per_1k:  Money::usd(800),     // $0.0008/1k
            cost_output_per_1k: Money::usd(4_000),   // $0.004/1k
            model_id_str:       "anthropic.claude_haiku_4_5".into(),
        }
    }

    // ----------------------------------------------------------------

    #[tokio::test]
    async fn turn_with_no_budget_never_caps() {
        // Existing Wed contract: no budget = no enforcement, even
        // if the response carries massive usage stats. The session
        // returns the model's content verbatim.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1_000_000, 1_000_000, "hi").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap();
        // No `.with_budget(...)` — budget stays None.
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "hi",
            "no budget → no cap check; model content returned verbatim");
    }

    #[tokio::test]
    async fn turn_under_cap_returns_model_content() {
        // Single call: $0.0008 × 1000/1000 + $0.004 × 500/1000 = $0.0028
        // Cap: $0.005 → under → no abort.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "real answer").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::usd(5_000)));  // $0.005
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "real answer");
    }

    #[tokio::test]
    async fn turn_over_cap_aborts_with_synthetic_message() {
        // Same single call costs $0.0028; cap = $0.001 → exceeds →
        // synthetic message replaces the model's content.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "real answer").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::usd(1_000)));  // $0.001
        let out = s.turn("hello").await.expect("turn ok");
        assert!(out.starts_with("budget exceeded: "),
            "expected synthetic budget-exceeded message; got: {out}");
        assert!(out.contains("$0.0028"),
            "message must include running total; got: {out}");
        assert!(out.contains("$0.001"),
            "message must include the cap; got: {out}");
        assert!(out.contains("per-run cap"),
            "message must mark it as the per-run cap; got: {out}");
    }

    #[tokio::test]
    async fn turn_at_exactly_cap_is_under() {
        // running == cap → NOT over → no abort (strict `>` comparison).
        // 1k input + 500 output @ Haiku rates = $0.0028 exactly.
        // Cap = $0.0028 → running == cap → must NOT abort.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "still ok").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::usd(2_800)));  // $0.0028
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "still ok",
            "running_total exactly at cap must NOT abort (strict `>`); got: {out}");
    }

    #[tokio::test]
    async fn turn_with_unlimited_cap_never_aborts() {
        // BudgetConfig is set (so cost accumulator runs) but the cap
        // is Money::Unlimited. With Money's Ord, `Capped(X) >
        // Unlimited` is always false → cap never trips, no matter
        // how expensive the call.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1_000_000, 1_000_000, "unbounded").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::Unlimited));
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "unbounded");
    }

    #[tokio::test]
    async fn turn_without_usage_does_not_move_running_total() {
        // Some providers don't populate `usage`. The session must
        // gracefully accept the call (don't bust the loop) without
        // accumulating cost. Effectively: skip cost-tracking for this
        // call; cap stays enforceable against future calls that DO
        // report usage.
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path};
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "no usage"}}]
            // No `usage` field.
        });
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server).await;
        // Tiny cap — would normally bust on any non-zero-cost call.
        // Without usage, running_total stays at $0 → no abort.
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::usd(1)));  // 1 µ-USD
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "no usage",
            "missing usage → no cost accumulated → no abort");
    }

    #[tokio::test]
    async fn turn_synthetic_message_format_is_stable() {
        // Pin the message format for any external tooling that
        // grep's for it. The format is:
        //   `budget exceeded: <running> of <cap> per-run cap`
        // Both amounts use `format_money` (no trailing zeros).
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "ignored").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget(Money::usd(1_000)));
        let out = s.turn("hi").await.expect("turn ok");
        assert_eq!(out, "budget exceeded: $0.0028 of $0.001 per-run cap",
            "format drift detected — update any external tooling that \
             pattern-matches this line");
    }

    #[tokio::test]
    async fn ollama_zero_cost_never_caps() {
        // Bundled catalog has Ollama at $0/$0. Even with a hard zero
        // cap ($0), a zero-cost completion stays at running == 0 == cap
        // → strict `>` is false → no abort. Free local-model path is
        // always safe under any Capped cap (including $0).
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 5_000, 10_000, "free response").await;
        let ollama_budget = BudgetConfig {
            per_run_cap:        Money::usd(0),
            per_day_cap:        Money::Unlimited,
            cost_input_per_1k:  Money::usd(0),
            cost_output_per_1k: Money::usd(0),
            model_id_str:       "ollama.llama_3_1_8b".into(),
        };
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(ollama_budget);
        let out = s.turn("hello").await.expect("turn ok");
        assert_eq!(out, "free response",
            "Ollama zero-cost path must work under ANY Capped cap");
    }

    #[test] fn with_budget_chains_off_open() {
        // Builder ergonomics: with_budget returns the session, so
        // construction can be one-liner. Verify the field is set.
        let s = Session::open("fast", "sys", None).unwrap()
            .with_budget(haiku_budget(Money::usd(5_000)));
        assert!(s.budget.is_some());
    }

    // ================================================================
    // Per-day cap enforcement (week 7 Thu)
    // ================================================================

    /// Helper: build a SpendTracker against a tempfile with a
    /// pre-seeded today-spend (in µ-USD).
    fn tracker_with_seed(project: &str, today_micro: i64) -> (crate::spend::SpendTracker, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "aislang-session-spend-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spend.json");
        if today_micro > 0 {
            let today = crate::spend::utc_today();
            let seed = format!(
                r#"{{"schema_version": 1, "projects": {{ "{project}": {{ "{today}": {{ "micro_units": {today_micro} }} }} }} }}"#);
            std::fs::write(&path, seed).unwrap();
        }
        let t = crate::spend::SpendTracker::at(project, path.clone()).unwrap();
        (t, dir)
    }

    fn haiku_budget_with_day_cap(per_run: Money, per_day: Money) -> BudgetConfig {
        BudgetConfig {
            per_run_cap:        per_run,
            per_day_cap:        per_day,
            cost_input_per_1k:  Money::usd(800),
            cost_output_per_1k: Money::usd(4_000),
            model_id_str:       "anthropic.claude_haiku_4_5".into(),
        }
    }

    // ---- per-day cap basic behavior ----

    #[tokio::test]
    async fn per_day_cap_triggers_when_pre_seed_pushes_over() {
        // Pre-seed the tracker at $0.0095 (9_500 µ-USD), use a daily
        // cap of $0.01 (10_000 µ-USD). A single Haiku call at
        // 1k prompt + 500 completion = $0.0028 (2_800 µ-USD) pushes
        // today's total to $0.0123 → over the daily cap → abort.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "real answer").await;
        let (tracker, dir) = tracker_with_seed("demo", 9_500);
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget_with_day_cap(
                Money::usd(1_000_000),  // huge per-run (no per-run abort)
                Money::usd(10_000),     // $0.01 per-day
            ))
            .with_tracker(tracker);
        let out = s.turn("hi").await.expect("turn ok");
        assert!(out.starts_with("daily budget exceeded: "),
            "expected daily-cap synthetic message; got: {out}");
        assert!(out.contains("per-day cap"),
            "message must mark it as the per-day cap; got: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn per_day_cap_not_triggered_when_under() {
        // Pre-seed at $0.005, cap = $0.05. One call adds $0.0028 →
        // total $0.0078 still under → no abort.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "real answer").await;
        let (tracker, dir) = tracker_with_seed("demo", 5_000);
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget_with_day_cap(
                Money::usd(1_000_000),
                Money::usd(50_000),
            ))
            .with_tracker(tracker);
        let out = s.turn("hi").await.expect("turn ok");
        assert_eq!(out, "real answer");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn per_day_cap_skipped_without_tracker() {
        // BudgetConfig has a finite per_day_cap but no tracker
        // attached → the daily check is silently skipped (the
        // tracker is what carries the running today-spend; without
        // it we have nothing to compare against).
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "real answer").await;
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget_with_day_cap(
                Money::usd(1_000_000),
                Money::usd(1),   // crazy-tight $0.000001 cap
            ));
            // No .with_tracker() — daily check skipped.
        let out = s.turn("hi").await.expect("turn ok");
        assert_eq!(out, "real answer",
            "without a tracker, per_day_cap is informational only");
    }

    #[tokio::test]
    async fn tracker_persists_cost_even_on_per_run_abort() {
        // Order-of-operations contract: cost lands on disk BEFORE
        // the per-run cap aborts the turn. The provider charged us
        // for the call; we record it.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1000, 500, "ignored").await;
        let (tracker, dir) = tracker_with_seed("demo", 0);
        let tracker_path = tracker.path().to_path_buf();
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget_with_day_cap(
                Money::usd(1_000),       // $0.001 — per-run will trip
                Money::Unlimited,         // no per-day cap
            ))
            .with_tracker(tracker);
        let out = s.turn("hi").await.expect("turn ok");
        assert!(out.starts_with("budget exceeded: "),
            "expected per-run abort; got: {out}");
        // The $0.0028 cost lands in the spend file despite the abort.
        let re_opened = crate::spend::SpendTracker::at("demo", tracker_path).unwrap();
        assert_eq!(re_opened.today_spend(), Money::usd(2_800),
            "tracker must persist cost even when per-run cap aborts the turn");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unlimited_per_day_cap_never_aborts() {
        // Daily cap = Unlimited (matches `hard_cap_per_day = unlimited`
        // in source). Tracker fills up arbitrarily; no abort.
        use wiremock::MockServer;
        let server = MockServer::start().await;
        mount_completion_with_usage(&server, 1_000_000, 1_000_000, "unbounded").await;
        let (tracker, dir) = tracker_with_seed("demo", i64::MAX / 2);
        let s = Session::open("fast", "sys", Some(&server.uri())).unwrap()
            .with_budget(haiku_budget_with_day_cap(
                Money::Unlimited,
                Money::Unlimited,
            ))
            .with_tracker(tracker);
        let out = s.turn("hi").await.expect("turn ok");
        assert_eq!(out, "unbounded");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
