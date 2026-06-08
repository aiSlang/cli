//! v0.3 Week 17 Thu — composition-pattern runtime.
//!
//! Decision 44.2: the pattern state machines live HERE (not in
//! aislang-cli).  The CLI just dispatches a user's chat input
//! through the right pattern; this module owns the per-pattern
//! execution shape.  v0.3 Week 17 Thu shipped orchestrator + router;
//! Week 17 Fri shipped supervisor + reflector; Week 18 Mon (this
//! commit) closes the 6-pattern roster with planner_executor +
//! coordinator.
//!
//! ## v0.3 Fri grammar status (supervisor + reflector)
//!
//! Orchestrator + router were CLI-wired via Wed's `pipeline { … }`
//! + `branch { … }` grammar blocks.  Supervisor + reflector have
//! no top-level grammar block in v0.3 (Decision 44.2 deferred the
//! surface syntax to v0.5+, when richer agent-composition messaging
//! is in scope).  The runtime modules below ship NOW so:
//!   - the dispatch logic is pinned + unit-tested before the
//!     grammar lands (no rewriting at v0.5 grammar landing time);
//!   - Decision 44.2 #2 (supervisor) + #4 (reflector) have working
//!     implementations the CLI can call via library API, even
//!     without a chat-surface entry point;
//!   - Phase 2+ skill / agent-tool composition can call into these
//!     patterns directly from a `skill { … }` exec body.
//!
//! ## Why pure async fns, not a trait
//!
//! Each pattern has a different SHAPE of input + output:
//!   - orchestrator: `(Vec<Agent>, initial_input) → final_reply`
//!   - router: `(on_template, cases, default, user_input) → picked_pipeline`
//!   - supervisor: `(agents, policy, user_input) → reply | intervention`
//!   - reflector: `(target_agent, critic_agent, user_input, max_retries)
//!                 → accepted_reply`
//!
//! A unifying trait would be all-types-and-no-shapes ("Pattern: Run").
//! Concrete async fns let each pattern carry the types it actually
//! needs without `Box<dyn>` indirection or generic-parameter sprawl.
//!
//! ## Session-opening dependency injection
//!
//! Each pattern needs to "open a session against agent X and send
//! it message Y."  The actual session-opening logic lives in
//! `mcp_server::build_session_for_agent[_multi_module]` (private
//! to this crate), so patterns take a closure callback:
//!
//! ```text
//! type DispatchOnceFn = dyn Fn(agent_label, user_input)
//!     → Future<Result<reply_string, _>>
//! ```
//!
//! This keeps the patterns module testable in isolation (synthetic
//! dispatcher in tests; real mcp_server::dispatch_to_agent in
//! production) without dragging the entire `Session` build graph
//! into the patterns layer.

use crate::SessionError;
use aislang_core::ast::{Branch, Pipeline};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

// ============================================================================
// Error type
// ============================================================================

/// Failures specific to pattern dispatch.  The variant set is
/// deliberately small — pure dispatch logic only.  Errors that
/// happen INSIDE a session (LiteLLM HTTP error, budget exceeded,
/// tool-call timeout) bubble up as `Session(SessionError)`
/// unchanged.
#[derive(Debug, thiserror::Error)]
pub enum PatternError {
    /// `orchestrator::run` called on a pipeline with zero steps.
    /// The grammar rejects this at parse time
    /// (`pipeline_block_rejects_empty_steps_list`), but the runtime
    /// guards against the in-memory invariant anyway in case a
    /// caller skipped the parser path.
    #[error("pipeline `{0}` has no steps to dispatch")]
    EmptyPipeline(String),

    /// `router::route` couldn't classify the user input — the
    /// classifier model returned a reply that didn't match any
    /// declared case-key + falling back to `default` was either
    /// disabled or also failed.  Carries the raw classifier reply
    /// so the diagnostic shows what the LLM actually said.
    #[error("router on branch `{branch_label}` could not classify input \
             into any of cases {available:?} (classifier reply: {reply:?})")]
    UnclassifiableInput {
        branch_label: String,
        available:    Vec<String>,
        reply:        String,
    },

    /// Bubbled-up `SessionError` from any per-step session.
    /// Wrapped (not just `From`) so the pattern context (which
    /// step / case) can be preserved in the rendered message.
    #[error("session error in pattern `{pattern}` step `{step}`: {source}")]
    Session {
        pattern: &'static str,
        step:    String,
        #[source] source: SessionError,
    },
}

/// `supervisor::run` outcome.  Callers (CLI or library users) pick
/// which arm to surface to the end-user — typically `Allowed.draft`
/// for happy path, `Blocked.reason` for the intervention message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorOutcome {
    /// Supervisor approved the target agent's draft.  The carried
    /// string is the draft itself, unchanged.
    Allowed(String),
    /// Supervisor blocked the draft (policy-violation / low-confidence
    /// / harmful).  Both the supervisor's reason and the rejected
    /// draft are surfaced so callers can log the full chain.
    Blocked { reason: String, draft: String },
}

/// `reflector::run` outcome.  `Accepted` carries the final accepted
/// draft + how many critic-driven retries it took to get there;
/// `MaxRetries` carries the LAST draft (still useful as a degraded
/// response) + the retry count that was hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReflectorOutcome {
    Accepted   { draft: String, retry_count: u32 },
    MaxRetries { draft: String, attempted:   u32 },
}

/// `planner_executor::run` outcome.  Carries the planner's decisions
/// (each non-leaf DECOMPOSE reply, in source order) + the executor's
/// per-leaf-task replies + the aggregated final reply for the
/// chat surface.  Bounds metadata (`max_depth_reached`,
/// `steps_executed`) lets callers verify cap behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerExecutorOutcome {
    /// Each DECOMPOSE-verdict reply the planner produced, in order.
    /// Empty if the very first planner reply was EXECUTE (no tree
    /// expansion happened).
    pub plan_history:     Vec<String>,
    /// Per-leaf executor replies, BFS-order of leaf visits.
    pub leaf_results:     Vec<String>,
    /// Concatenated final reply ready to ship to the chat surface.
    pub final_reply:      String,
    /// Deepest BFS level reached (0 = no decomposition happened).
    pub max_depth_reached: u32,
    /// Total executor invocations (= leaf count).
    pub steps_executed:   u32,
    /// True when the BFS terminated because the global step budget
    /// (`max_total_steps`) was hit before the queue drained.  False
    /// on normal queue exhaustion.
    pub budget_exhausted: bool,
}

/// `coordinator::run` outcome.  `Converged` = consecutive rounds
/// produced identical messages (round-robin reached fixed point);
/// `MaxRounds` = budget exhausted before convergence (still surfaces
/// the last message as a degraded response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorOutcome {
    Converged { final_message: String, rounds: u32 },
    MaxRounds { final_message: String, rounds: u32 },
}

/// Convenience alias for the boxed-future shape every dispatcher
/// callback returns.  `'static` because the callback may need to
/// be cloned + reused across multiple pattern steps; the actual
/// caller (mcp_server) packs an `Arc<AislangServer>` into the
/// closure so the runtime state is shared.
pub type DispatchFuture =
    Pin<Box<dyn Future<Output = Result<String, SessionError>> + Send + 'static>>;

// ============================================================================
// Orchestrator pattern (Decision 44.5 #1 — sequential pipeline)
// ============================================================================

pub mod orchestrator {
    use super::*;

    /// Sequential agent composition.  Each pipeline step's reply
    /// becomes the next step's `user_input`.  Returns the FINAL
    /// step's reply.
    ///
    /// ## Algorithm
    ///
    /// ```text
    /// reply ← user_input
    /// for step_label in pipeline.step_labels:
    ///     reply ← dispatch_once(step_label, reply)
    /// return reply
    /// ```
    ///
    /// `pipeline.steps` is already resolved by the caller into a
    /// flat list of qualified agent labels (e.g.
    /// `["architect.architect", "coder.coder"]`).  Cross-module
    /// resolution happened in `load_project_input` /
    /// `resolve_multi`; this function operates on already-qualified
    /// names.
    ///
    /// ## v0.3 limitations (deferred to v0.5+)
    ///
    /// - **No early-stop**: every step runs in order regardless of
    ///   intermediate reply content.  v0.5+ may add a `stop_when`
    ///   MiniJinja condition.
    /// - **String-only handoff**: each step receives the previous
    ///   step's reply verbatim as `user_input`.  Structured
    ///   envelopes (typed messages, schema-validated payloads) are
    ///   v0.5+ per Decision 44.5.
    /// - **No retry / per-step error recovery**: a `SessionError`
    ///   in any step aborts the whole pipeline.  The `reflector`
    ///   pattern (Fri) provides retry semantics.
    pub async fn run<F>(
        pipeline:        &Pipeline,
        initial_input:   &str,
        mut dispatch_once: F,
    ) -> Result<String, PatternError>
    where
        F: FnMut(String /* agent_label */, String /* user_input */) -> DispatchFuture,
    {
        let step_labels = qualified_step_labels(pipeline);
        if step_labels.is_empty() {
            return Err(PatternError::EmptyPipeline(pipeline.label.clone()));
        }
        let mut current_input = initial_input.to_string();
        for (idx, step_label) in step_labels.iter().enumerate() {
            let fut = dispatch_once(step_label.clone(), current_input.clone());
            current_input = fut.await.map_err(|e| PatternError::Session {
                pattern: "orchestrator",
                step:    format!("step[{idx}]=`{step_label}`"),
                source:  e,
            })?;
        }
        Ok(current_input)
    }

    /// Convert a pipeline's `steps: Vec<Ref>` into the list of
    /// qualified agent labels used by mcp-serve's
    /// `build_session_for_agent[_multi_module]` session-key lookup.
    ///
    /// Rules (mirror the multi-module session builder's
    /// `qualified()` helper):
    ///   - Cross-module ref (`module = Some(m)`): `m.<label>`
    ///   - Same-module ref in a multi-module project: the pipeline
    ///     itself carries no module info, so the caller is
    ///     responsible for passing in a containing-module
    ///     context if needed.  For v0.3 Thu the pipelines we
    ///     orchestrate are at the top-level (the `body` of a
    ///     `loop` / `branch.cases[*]`) — those refs are typed
    ///     `RefKind::Pipeline` and their `module` field flows
    ///     unchanged.  Agent refs inside `steps[]` follow the
    ///     resolver's containing-module convention.
    ///
    /// For Thu's MVP we use the simpler rule: same-module ref
    /// gets the bare label (single-file projects); cross-module
    /// ref gets `<module>.<label>`.  Multi-module-from-same-module-
    /// pipeline-stepping-into-same-module-agent uses the pipeline
    /// containing-module which we extract from the pipeline's
    /// own label prefix (set by `resolve_multi`).  v0.5+ may
    /// pass the containing module explicitly to remove the
    /// "extract from label" coupling.
    fn qualified_step_labels(pipeline: &Pipeline) -> Vec<String> {
        // The pipeline's own label is qualified `<module>.<label>`
        // post-merge in multi-module projects (e.g.
        // `coder.review_cycle`), or bare in single-file projects.
        // We mirror that prefix onto same-module step refs.
        let containing_module = pipeline.label.split_once('.').map(|(m, _)| m);
        pipeline.steps.iter().map(|r| match (&r.module, containing_module) {
            (Some(m), _)     => format!("{m}.{}", r.label),
            (None, Some(c))  => format!("{c}.{}", r.label),
            (None, None)     => r.label.clone(),
        }).collect()
    }
}

// ============================================================================
// Router pattern (Decision 44.5 #3 — classifier-based branch dispatch)
// ============================================================================

pub mod router {
    use super::*;

    /// Classify a user input against a branch's case-keys + return
    /// the matched pipeline ref (or the branch's `default` if no
    /// case matches).
    ///
    /// ## Algorithm
    ///
    /// 1. Build a classifier prompt that asks the model to pick
    ///    one of the declared case-keys based on the user input.
    /// 2. Dispatch the prompt against the supplied classifier
    ///    callback (typically opens a Session against the branch
    ///    container's model, but the caller chooses).
    /// 3. Strip / lower-case the classifier reply + look up in
    ///    `branch.cases`.  Falls back to `branch.default` if no
    ///    case-key matches.
    ///
    /// ## v0.3 limitations
    ///
    /// - **String-equality match, no fuzzy logic**: the classifier
    ///   reply must EXACTLY equal a case-key (after stripping
    ///   whitespace + matching case-insensitively).  Anthropic's
    ///   structured-output / JSON-mode would give us schema-pinned
    ///   replies; v0.5+ may switch.
    /// - **No re-prompting on failure**: a non-matching reply falls
    ///   straight to `default`.  PLAN says "default = pipeline.<p>"
    ///   is REQUIRED at parse time (Wed) precisely to make this
    ///   simple — there's always a safe target.
    /// - **MiniJinja `on` template ignored in v0.3 Thu**: the
    ///   `branch.on` field is stored verbatim but Thu's runtime
    ///   uses ONLY the raw `user_input` as the classifier input.
    ///   Week 18 Tue (MiniJinja integration) wires actual template
    ///   rendering so `on = "{{ classifier_result }}"` can extract
    ///   a pre-computed classifier output from the conversation
    ///   context.
    ///
    /// Returns a clone of the matched case's `Ref` (or
    /// `branch.default`) so the caller can dispatch through
    /// `orchestrator::run` against the matched pipeline.
    pub async fn route<F>(
        branch:     &Branch,
        user_input: &str,
        mut classifier: F,
    ) -> Result<aislang_core::ast::Ref, PatternError>
    where
        F: FnMut(String /* classifier prompt */) -> DispatchFuture,
    {
        let prompt = build_classifier_prompt(&branch.cases, user_input);
        let reply = classifier(prompt).await.map_err(|e| PatternError::Session {
            pattern: "router",
            step:    format!("classify on branch `{}`", branch.label),
            source:  e,
        })?;
        // Pick: normalize the reply + match case-keys case-insensitively.
        let normalized = reply.trim().to_lowercase();
        for (case_key, case_ref) in &branch.cases {
            if case_key.to_lowercase() == normalized
                || normalized.contains(&case_key.to_lowercase())
            {
                return Ok(case_ref.clone());
            }
        }
        // No match → fall back to default (Wed parser made `default`
        // mandatory, so it's always present).  Future v0.5+ may
        // surface UnclassifiableInput as a hard error when callers
        // opt into strict-mode.
        Ok(branch.default.clone())
    }

    /// Build the classifier system+user prompt.  Asks the LLM to
    /// output ONLY the case-key (no extra commentary), which makes
    /// the v0.3 string-match logic reliable enough for ~3-5-way
    /// branches.
    fn build_classifier_prompt(
        cases:      &BTreeMap<String, aislang_core::ast::Ref>,
        user_input: &str,
    ) -> String {
        let case_list = cases.keys().cloned().collect::<Vec<_>>().join(", ");
        format!(
            "You are a routing classifier.  Given the user input below, \
             reply with ONE of the case keys: {case_list}.  Output ONLY \
             the case key, no other text, no punctuation, no quotes.\n\
             \nUser input: {user_input}",
        )
    }
}

// ============================================================================
// Supervisor pattern (Decision 44.2 #2 — monitor + intervene)
// ============================================================================

pub mod supervisor {
    use super::*;

    /// Two-agent pattern: a primary `target` agent answers the user;
    /// a `supervisor` agent then audits the draft + decides whether
    /// to ALLOW or BLOCK.
    ///
    /// ## Algorithm
    ///
    /// ```text
    /// draft   ← dispatch(target,     user_input)
    /// verdict ← dispatch(supervisor, build_supervisor_prompt(user_input, draft))
    /// if verdict starts with "BLOCK"  →  Blocked { reason = strip-prefix(verdict), draft }
    /// else                            →  Allowed(draft)
    /// ```
    ///
    /// ## v0.3 limitations
    ///
    /// - **Prefix-string verdict** (`BLOCK:` / `ALLOW`): same
    ///   pragmatic shortcut router uses for case keys.  Reliable
    ///   enough for the ~3-token decision; v0.5+ may switch to
    ///   structured output (Anthropic JSON-mode) when the
    ///   verdict is more than binary.
    /// - **No retry on block**: a blocked draft is returned as-is
    ///   (the caller surfaces the intervention reason).  Combine
    ///   with `reflector` if you want auto-revision on block.
    /// - **Single-pass**: supervisor sees ONE draft + decides.  No
    ///   multi-round dialogue.  v0.5+ may add iterative supervision.
    pub async fn run<F>(
        target_agent:     String,
        supervisor_agent: String,
        user_input:       &str,
        mut dispatch_once: F,
    ) -> Result<SupervisorOutcome, PatternError>
    where
        F: FnMut(String /* agent_label */, String /* user_input */) -> DispatchFuture,
    {
        // 1. Target agent drafts a reply to the user.
        let draft = dispatch_once(target_agent.clone(), user_input.to_string()).await
            .map_err(|e| PatternError::Session {
                pattern: "supervisor",
                step:    format!("target=`{target_agent}`"),
                source:  e,
            })?;
        // 2. Supervisor agent audits the draft.
        let verdict_prompt = build_supervisor_prompt(user_input, &draft);
        let verdict = dispatch_once(supervisor_agent.clone(), verdict_prompt).await
            .map_err(|e| PatternError::Session {
                pattern: "supervisor",
                step:    format!("supervisor=`{supervisor_agent}`"),
                source:  e,
            })?;
        // 3. Interpret verdict + return outcome.
        if is_block_verdict(&verdict) {
            Ok(SupervisorOutcome::Blocked {
                reason: extract_block_reason(&verdict),
                draft,
            })
        } else {
            Ok(SupervisorOutcome::Allowed(draft))
        }
    }

    /// Verdict-prefix probe.  Matches `BLOCK` at the start of the
    /// trimmed reply (case-insensitive) — covers `BLOCK`, `BLOCK:`,
    /// `block — leaked PII`, etc.  Any other prefix is treated as
    /// ALLOW (the supervisor prompt asks for `ALLOW` explicitly,
    /// but accepting silence or any non-BLOCK reply errs on the
    /// permissive side — appropriate for v0.3 prototyping).
    fn is_block_verdict(reply: &str) -> bool {
        reply.trim().to_lowercase().starts_with("block")
    }

    /// Strip the `BLOCK[:]` prefix + return what comes after as the
    /// intervention reason.  Falls back to the full reply if no
    /// `:` separator is present.
    fn extract_block_reason(reply: &str) -> String {
        let trimmed = reply.trim();
        // Try `BLOCK: <reason>` first.
        if let Some((_, rest)) = trimmed.split_once(':') {
            return rest.trim().to_string();
        }
        // No colon — strip the literal `BLOCK` token + take the rest.
        let lc = trimmed.to_lowercase();
        if let Some(rest) = lc.strip_prefix("block") {
            let suffix_offset = trimmed.len() - rest.len();
            return trimmed[suffix_offset..].trim().to_string();
        }
        trimmed.to_string()   // unreachable given is_block_verdict's contract
    }

    fn build_supervisor_prompt(user_input: &str, draft: &str) -> String {
        format!(
            "You are a supervisor agent reviewing another agent's reply \
             for policy violations or low-confidence answers.\n\
             \nThe user asked:\n{user_input}\n\
             \nThe primary agent's draft reply was:\n{draft}\n\
             \nDecide whether to ALLOW or BLOCK this reply.  Reply with \
             exactly one of:\n\
             - `ALLOW` (if the draft is safe, accurate, and on-topic)\n\
             - `BLOCK: <one-sentence reason>` (if the draft violates \
             policy, leaks confidential info, gives harmful advice, or \
             shows low confidence on a question that needs a clear \
             answer).\n\
             \nOutput ONLY the verdict line.  No preamble.  No markdown.",
        )
    }
}

// ============================================================================
// Reflector pattern (Decision 44.2 #4 — critique-and-retry)
// ============================================================================

pub mod reflector {
    use super::*;

    /// Two-agent loop: a `target` agent drafts a reply; a `critic`
    /// agent evaluates the draft + either ACCEPTs or returns
    /// revision feedback.  Up to `max_retries` revision rounds, then
    /// surface the last draft as a degraded `MaxRetries` outcome.
    ///
    /// ## Algorithm
    ///
    /// ```text
    /// target_input ← user_input
    /// retry_count  ← 0
    /// for attempt in 0..=max_retries:
    ///     draft   ← dispatch(target, target_input)
    ///     verdict ← dispatch(critic, build_critic_prompt(user_input, draft))
    ///     if verdict starts with "ACCEPT": return Accepted { draft, retry_count }
    ///     // else: feed the critique back as revision instructions
    ///     target_input ← build_revision_prompt(user_input, draft, verdict)
    ///     retry_count  += 1
    /// return MaxRetries { draft: <last>, attempted: retry_count }
    /// ```
    ///
    /// ## Semantics of `max_retries`
    ///
    /// `max_retries = 0` → exactly 1 draft attempt; no revisions.
    /// `max_retries = N` → 1 initial + up to N revisions = up to N+1
    /// total target invocations.  This matches the conventional
    /// "max retries" reading (the COUNT is how many extra tries
    /// after the first, not the total budget).
    ///
    /// ## v0.3 limitations
    ///
    /// - **Prefix-string verdict** (`ACCEPT` vs. anything-else):
    ///   same shortcut as supervisor.  Critic prompts ask for the
    ///   literal `ACCEPT` token to start the reply on acceptance.
    /// - **Verbatim feedback handoff**: the critic's reply (minus
    ///   any leading `ACCEPT` token) is fed back to the target as
    ///   raw text.  No structured feedback schema in v0.3.
    /// - **Single critic**: one critic agent, no ensemble / multi-
    ///   reviewer voting.  v0.5+ may layer that on top.
    pub async fn run<F>(
        target_agent: String,
        critic_agent: String,
        user_input:   &str,
        max_retries:  u32,
        mut dispatch_once: F,
    ) -> Result<ReflectorOutcome, PatternError>
    where
        F: FnMut(String /* agent_label */, String /* user_input */) -> DispatchFuture,
    {
        let mut target_input = user_input.to_string();
        let mut last_draft   = String::new();
        let mut retry_count: u32 = 0;
        // 1 initial + max_retries revisions = max_retries + 1 iterations.
        for attempt in 0..=max_retries {
            let draft = dispatch_once(target_agent.clone(), target_input.clone()).await
                .map_err(|e| PatternError::Session {
                    pattern: "reflector",
                    step:    format!("target attempt {} (agent `{target_agent}`)",
                                     attempt + 1),
                    source:  e,
                })?;
            last_draft = draft.clone();

            let critic_prompt = build_critic_prompt(user_input, &draft);
            let verdict = dispatch_once(critic_agent.clone(), critic_prompt).await
                .map_err(|e| PatternError::Session {
                    pattern: "reflector",
                    step:    format!("critic attempt {} (agent `{critic_agent}`)",
                                     attempt + 1),
                    source:  e,
                })?;

            if is_accept_verdict(&verdict) {
                return Ok(ReflectorOutcome::Accepted { draft, retry_count });
            }
            // Critic asks for revision — package its feedback into
            // the next target prompt.
            target_input = build_revision_prompt(user_input, &draft, &verdict);
            retry_count += 1;
        }
        Ok(ReflectorOutcome::MaxRetries {
            draft:     last_draft,
            attempted: retry_count,
        })
    }

    fn is_accept_verdict(reply: &str) -> bool {
        reply.trim().to_lowercase().starts_with("accept")
    }

    fn build_critic_prompt(user_input: &str, draft: &str) -> String {
        format!(
            "You are a critic agent evaluating another agent's draft \
             reply.  Your job is to decide whether the draft is good \
             enough to ship to the user, or whether it needs revision.\n\
             \nThe user asked:\n{user_input}\n\
             \nThe target agent's draft was:\n{draft}\n\
             \nReply with exactly one of:\n\
             - `ACCEPT` (if the draft is correct, clear, and answers \
             the user's question)\n\
             - Specific, actionable revision feedback (do NOT start \
             with the word ACCEPT).  Point at what's wrong + what to \
             change.\n\
             \nNo preamble.  No markdown headers.  Just the verdict.",
        )
    }

    fn build_revision_prompt(user_input: &str, draft: &str, critique: &str) -> String {
        format!(
            "You drafted a reply that a critic asked you to revise.\n\
             \nOriginal user question:\n{user_input}\n\
             \nYour previous draft:\n{draft}\n\
             \nCritic's feedback:\n{critique}\n\
             \nRevise your reply to address the feedback.  Output ONLY \
             the revised reply, no meta-commentary.",
        )
    }
}

// ============================================================================
// Planner-executor pattern (Decision 44.2 #5 — plan-then-execute)
// ============================================================================

pub mod planner_executor {
    use super::*;
    use std::collections::VecDeque;

    /// Two-agent pattern: a `planner` agent decomposes the user
    /// request into smaller tasks (possibly recursively, up to
    /// `max_depth`); each leaf task is dispatched to an `executor`
    /// agent + the per-leaf replies are concatenated into the
    /// final answer.
    ///
    /// ## Algorithm (depth-bound BFS)
    ///
    /// ```text
    /// queue ← [(user_input, depth=0)]
    /// plan_history ← []
    /// leaf_results ← []
    /// while queue and steps_executed < max_total_steps:
    ///     (task, depth) ← pop
    ///     if depth >= max_depth:
    ///         // forced executor — no more decomposition allowed
    ///         result ← dispatch(executor, task)
    ///         leaf_results.append(result)
    ///     else:
    ///         verdict ← dispatch(planner, build_planner_prompt(task, depth, max_depth))
    ///         if verdict starts with "EXECUTE":
    ///             result ← dispatch(executor, task)
    ///             leaf_results.append(result)
    ///         else (DECOMPOSE):
    ///             plan_history.append(verdict)
    ///             subtasks ← parse_subtasks(verdict)
    ///             for st in subtasks: queue.push_back((st, depth+1))
    /// return PlannerExecutorOutcome { plan_history, leaf_results,
    ///                                 final_reply = concat(leaf_results), ... }
    /// ```
    ///
    /// ## Bounds + termination
    ///
    /// - **max_depth**: caps the BFS depth.  At depth == max_depth
    ///   the planner is SKIPPED + the task goes straight to executor.
    ///   `max_depth = 0` ⇒ planner never runs; equivalent to a
    ///   single executor call against the user_input.
    /// - **max_total_steps**: hard global cap on executor invocations.
    ///   When hit, the BFS terminates EARLY + `budget_exhausted` is
    ///   set true on the outcome.  Defends against runaway planner
    ///   blow-ups (e.g. each level produces 5 subtasks).
    ///
    /// ## v0.3 verdict / subtask format
    ///
    /// The planner agent is asked to reply with EITHER:
    ///   - `EXECUTE` (single line, optional trailing whitespace) —
    ///     treat the task as a leaf; dispatch to executor.
    ///   - `DECOMPOSE:` followed by one task per line (numbered or
    ///     bulleted is fine; we strip leading `- `, `* `, `1.`, …).
    ///     Empty / comment lines are skipped.
    ///
    /// Anything that doesn't start with `EXECUTE` or `DECOMPOSE` is
    /// treated as a SINGLE-line decomposition (one subtask = the
    /// entire reply); this keeps things permissive on prompts that
    /// drift.  v0.5+ may switch to JSON-mode for stricter parsing.
    pub async fn run<F>(
        planner_agent:   String,
        executor_agent:  String,
        user_input:      &str,
        max_depth:       u32,
        max_total_steps: u32,
        mut dispatch_once: F,
    ) -> Result<PlannerExecutorOutcome, PatternError>
    where
        F: FnMut(String /* agent_label */, String /* user_input */) -> DispatchFuture,
    {
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        queue.push_back((user_input.to_string(), 0));
        let mut plan_history: Vec<String>  = Vec::new();
        let mut leaf_results: Vec<String>  = Vec::new();
        let mut max_depth_reached: u32 = 0;
        let mut steps_executed:   u32 = 0;
        let mut budget_exhausted: bool = false;

        while let Some((task, depth)) = queue.pop_front() {
            if steps_executed >= max_total_steps {
                budget_exhausted = true;
                break;
            }
            max_depth_reached = max_depth_reached.max(depth);

            // At depth cap → force executor (no planner consultation).
            let force_execute = depth >= max_depth;
            let verdict = if force_execute {
                None
            } else {
                let prompt = build_planner_prompt(&task, depth, max_depth);
                let reply = dispatch_once(planner_agent.clone(), prompt).await
                    .map_err(|e| PatternError::Session {
                        pattern: "planner_executor",
                        step:    format!("planner depth={depth} (agent `{planner_agent}`)"),
                        source:  e,
                    })?;
                Some(reply)
            };

            let should_execute = match verdict.as_deref() {
                None    => true,                            // depth cap
                Some(r) => is_execute_verdict(r),
            };

            if should_execute {
                let result = dispatch_once(executor_agent.clone(), task).await
                    .map_err(|e| PatternError::Session {
                        pattern: "planner_executor",
                        step:    format!("executor step {} (agent `{executor_agent}`)",
                                         steps_executed + 1),
                        source:  e,
                    })?;
                leaf_results.push(result);
                steps_executed += 1;
            } else {
                let verdict = verdict.expect("verdict present when not force-executing");
                let subtasks = parse_subtasks(&verdict);
                plan_history.push(verdict);
                // Empty decomposition: the planner declined to expand
                // but also didn't EXECUTE.  Coerce to executor to
                // keep the BFS moving (defensive — happens when the
                // LLM emits `DECOMPOSE:` with nothing after).
                if subtasks.is_empty() {
                    let result = dispatch_once(executor_agent.clone(), task).await
                        .map_err(|e| PatternError::Session {
                            pattern: "planner_executor",
                            step:    format!("executor step {} (empty-decompose fallback)",
                                             steps_executed + 1),
                            source:  e,
                        })?;
                    leaf_results.push(result);
                    steps_executed += 1;
                } else {
                    for st in subtasks {
                        queue.push_back((st, depth + 1));
                    }
                }
            }
        }

        let final_reply = aggregate_leaf_results(&leaf_results);
        Ok(PlannerExecutorOutcome {
            plan_history, leaf_results, final_reply,
            max_depth_reached, steps_executed, budget_exhausted,
        })
    }

    fn is_execute_verdict(reply: &str) -> bool {
        reply.trim().to_lowercase().starts_with("execute")
    }

    /// Pull subtasks out of a DECOMPOSE-verdict reply.  Strips the
    /// leading `DECOMPOSE:` marker + walks lines, dropping empties
    /// and stripping common bullet/number prefixes (`- `, `* `,
    /// `1.`, `1)`, `(1)`).  Each non-empty surviving line is one
    /// subtask.  Permissive on purpose: LLM output varies.
    fn parse_subtasks(verdict: &str) -> Vec<String> {
        let trimmed = verdict.trim();
        // Strip a `DECOMPOSE[:]` prefix if present; otherwise treat
        // the whole reply as the task list.
        let body = if trimmed.to_lowercase().starts_with("decompose") {
            // Find the first colon or newline after `decompose`.
            let lc = trimmed.to_lowercase();
            let offset = "decompose".len();
            let rest = &trimmed[offset..];
            // Skip optional `:` + leading whitespace/newline.
            let lc_rest = &lc[offset..];
            let strip_colon = lc_rest.trim_start().starts_with(':');
            let after_colon = if strip_colon {
                let pos = rest.find(':').map(|p| p + 1).unwrap_or(0);
                &rest[pos..]
            } else {
                rest
            };
            after_colon
        } else {
            trimmed
        };
        body.lines()
            .map(|l| strip_bullet_prefix(l.trim()))
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect()
    }

    /// Strip `- `, `* `, `1.`, `1)`, `(1)` and similar from the
    /// front of a task line.  Leaves the actual task text intact.
    fn strip_bullet_prefix(line: &str) -> &str {
        let trimmed = line.trim_start();
        // Markdown bullets / asterisk.
        if let Some(rest) = trimmed.strip_prefix("- ") { return rest.trim_start(); }
        if let Some(rest) = trimmed.strip_prefix("* ") { return rest.trim_start(); }
        // Numbered: digits then `.` or `)` then space.
        let bytes = trimmed.as_bytes();
        let mut i = 0;
        // Optional `(` before digits.
        let opened = bytes.first().copied() == Some(b'(');
        if opened { i = 1; }
        let digit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
        if i > digit_start && i < bytes.len() {
            // Need to see a closing punctuator + a space.
            let punct = bytes[i];
            let closes_match = (opened && punct == b')') || punct == b'.' || punct == b')';
            if closes_match {
                // Consume the punctuator + any following whitespace.
                let after = &trimmed[i + 1..];
                let rest = after.trim_start();
                if rest.len() < after.len() || after.starts_with(' ') {
                    return rest;
                }
            }
        }
        trimmed
    }

    fn build_planner_prompt(task: &str, depth: u32, max_depth: u32) -> String {
        format!(
            "You are a planner agent.  You receive a task and decide \
             whether it is small enough to execute directly, or whether \
             it should be DECOMPOSED into smaller subtasks.\n\
             \nCurrent task:\n{task}\n\
             \nDepth: {depth} (max {max_depth}).  You CANNOT decompose \
             past max_depth, so be decisive near the cap.\n\
             \nReply with EXACTLY one of:\n\
             - `EXECUTE` — this task is small enough to handle directly.\n\
             - `DECOMPOSE:` followed by ONE subtask per line.  Use \
             plain text per line (no JSON, no markdown).\n\
             \nNo preamble.  No explanation.  Just the verdict.",
        )
    }

    fn aggregate_leaf_results(results: &[String]) -> String {
        // v0.3: simplest possible aggregation — concatenate with
        // newlines.  v0.5+ may add a final-aggregator agent ("read
        // these N results + write a unified reply") for non-trivial
        // synthesis.
        results.join("\n\n")
    }
}

// ============================================================================
// Coordinator pattern (Decision 44.2 #6 — peer-to-peer round-robin)
// ============================================================================

pub mod coordinator {
    use super::*;

    /// N-peer round-robin: each round, every peer agent sees the
    /// current shared message in order + emits its contribution.
    /// The pattern terminates when consecutive rounds produce
    /// IDENTICAL messages (fixed point = consensus) or when
    /// `max_rounds` is exhausted.
    ///
    /// ## Algorithm
    ///
    /// ```text
    /// message ← user_input
    /// for round in 1..=max_rounds:
    ///     round_input ← message
    ///     for peer in peers:           # round-robin in declared order
    ///         message ← dispatch(peer, build_peer_prompt(round, peer_idx, message))
    ///     if message == round_input:   # no peer added anything this round
    ///         return Converged { final_message = message, rounds = round }
    /// return MaxRounds { final_message = message, rounds = max_rounds }
    /// ```
    ///
    /// ## Convergence rule (v0.3)
    ///
    /// "Identical message" = exact string equality after the full
    /// round-robin pass.  Pragmatic + cheap: if no peer changed the
    /// message this round, the conversation has plateaued + we
    /// stop.  v0.5+ may switch to semantic-similarity (embedding
    /// cosine ≥ threshold) for cases where peers reformat without
    /// adding info.
    ///
    /// ## v0.3 limitations
    ///
    /// - **No early termination per-peer**: even when peer N writes
    ///   "we're done", peers N+1..|peers| still get called this
    ///   round.  Convergence check fires only after the FULL
    ///   round-robin.  Cheap to fix later if needed.
    /// - **String-only message**: same as orchestrator's String-
    ///   only handoff (no structured envelopes).  v0.5+ may add.
    /// - **Static peer list**: peers cannot drop out / be added
    ///   mid-coordination.  v0.5+ may add a "leave" verdict.
    pub async fn run<F>(
        peers:       Vec<String>,
        user_input:  &str,
        max_rounds:  u32,
        mut dispatch_once: F,
    ) -> Result<CoordinatorOutcome, PatternError>
    where
        F: FnMut(String /* agent_label */, String /* user_input */) -> DispatchFuture,
    {
        if peers.len() < 2 {
            // Single-peer coordination is meaningless — there's no
            // round-robin to do.  Reject early so callers don't
            // silently get a 1-peer "echo" loop.
            return Err(PatternError::Session {
                pattern: "coordinator",
                step:    format!("peer-list-validation: got {} peers, need ≥2",
                                 peers.len()),
                // Reuse EmptyChoices as the lightest existing variant
                // for "caller misconfiguration"; v0.5+ may add a
                // dedicated PatternError::BadConfig variant.
                source:  SessionError::EmptyChoices,
            });
        }
        if max_rounds == 0 {
            // 0 rounds = no work + nothing to converge on.  Surface
            // the initial input as MaxRounds at round 0 — caller's
            // explicit "skip" should not be reinterpreted.
            return Ok(CoordinatorOutcome::MaxRounds {
                final_message: user_input.to_string(),
                rounds:        0,
            });
        }

        let mut message = user_input.to_string();
        for round in 1..=max_rounds {
            let round_input = message.clone();
            for (peer_idx, peer) in peers.iter().enumerate() {
                // v0.3 design choice: dispatch passes the RAW message
                // (no per-call prompt wrap).  Each peer's "you are a
                // coordinator peer; contribute or echo to converge"
                // framing belongs in the peer agent's SYSTEM prompt
                // (declared in the .ais file), not in this runtime.
                // This keeps the threading semantic clean — a peer
                // that just echoes its input causes the convergence
                // check at end-of-round to fire correctly.  Compare
                // to router/reflector/supervisor which DO wrap input
                // (those patterns need per-call structure to drive
                // verdict parsing); coordinator's value IS the
                // verbatim round-tripping.
                let reply = dispatch_once(peer.clone(), message.clone()).await
                    .map_err(|e| PatternError::Session {
                        pattern: "coordinator",
                        step:    format!("round {round}/{max_rounds} peer {} (`{peer}`)",
                                         peer_idx + 1),
                        source:  e,
                    })?;
                message = reply;
            }
            if message == round_input {
                return Ok(CoordinatorOutcome::Converged {
                    final_message: message,
                    rounds:        round,
                });
            }
        }
        Ok(CoordinatorOutcome::MaxRounds {
            final_message: message,
            rounds:        max_rounds,
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use aislang_core::ast::{Ref, RefKind};

    fn agent_ref(label: &str) -> Ref {
        Ref { kind: RefKind::Agent, label: label.into(), module: None }
    }

    fn pipeline_ref(label: &str) -> Ref {
        Ref { kind: RefKind::Pipeline, label: label.into(), module: None }
    }

    fn cross_module_agent(module: &str, label: &str) -> Ref {
        Ref { kind: RefKind::Agent, label: label.into(),
              module: Some(module.into()) }
    }

    fn synthetic_pipeline(label: &str, steps: Vec<Ref>) -> Pipeline {
        Pipeline {
            label: label.into(),
            steps,
            input:  None,
            output: None,
        }
    }

    fn synthetic_branch(
        label:   &str,
        cases:   &[(&str, Ref)],
        default: Ref,
    ) -> Branch {
        let mut map = BTreeMap::new();
        for (k, v) in cases { map.insert((*k).to_string(), v.clone()); }
        Branch {
            label:   label.into(),
            on:      "{{ classifier_result }}".to_string(),
            cases:   map,
            default,
        }
    }

    // -- orchestrator ---------------------------------------------- //

    #[tokio::test]
    async fn orchestrator_threads_replies_through_pipeline_steps() {
        // 3-step pipeline: each agent's reply is "step-N-saw:<input>".
        // Final reply should reflect the chained inputs.
        let pipeline = synthetic_pipeline("p",
            vec![agent_ref("a"), agent_ref("b"), agent_ref("c")]);
        let dispatched = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let dispatched_clone = dispatched.clone();
        let mut step_counter = 0u32;
        let dispatch = move |agent: String, input: String| {
            step_counter += 1;
            let n = step_counter;
            dispatched_clone.lock().unwrap().push((agent.clone(), input.clone()));
            let reply = format!("step-{n}-saw:{input}");
            Box::pin(async move { Ok(reply) }) as DispatchFuture
        };

        let final_reply = orchestrator::run(&pipeline, "hello", dispatch)
            .await.expect("must succeed");
        assert_eq!(final_reply, "step-3-saw:step-2-saw:step-1-saw:hello");

        let log = dispatched.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0], ("a".into(), "hello".into()));
        assert_eq!(log[1], ("b".into(), "step-1-saw:hello".into()));
        assert_eq!(log[2], ("c".into(), "step-2-saw:step-1-saw:hello".into()));
    }

    #[tokio::test]
    async fn orchestrator_returns_initial_input_for_zero_steps() {
        // Parser rejects pipelines with empty steps[], but the
        // runtime guards anyway.
        let pipeline = synthetic_pipeline("empty", vec![]);
        let err = orchestrator::run(&pipeline, "hello",
            |_, _| Box::pin(async { Ok("never called".into()) }) as DispatchFuture)
            .await.unwrap_err();
        match err {
            PatternError::EmptyPipeline(label) => assert_eq!(label, "empty"),
            other => panic!("expected EmptyPipeline; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn orchestrator_qualifies_step_labels_in_multi_module_projects() {
        // Pipeline label is qualified (`coder.flow`) — same-module
        // step refs (no module field) should be prefixed with the
        // pipeline's containing module (`coder`); cross-module
        // refs use their own module.
        let pipeline = synthetic_pipeline("coder.flow", vec![
            agent_ref("local"),                       // → coder.local
            cross_module_agent("architect", "remote"), // → architect.remote
        ]);
        let dispatched = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let dispatched_clone = dispatched.clone();
        let dispatch = move |agent: String, _input: String| {
            dispatched_clone.lock().unwrap().push(agent.clone());
            Box::pin(async { Ok("ok".into()) }) as DispatchFuture
        };
        orchestrator::run(&pipeline, "x", dispatch).await.expect("run");
        let log = dispatched.lock().unwrap();
        assert_eq!(log[0], "coder.local",
            "same-module step ref should pick up pipeline's containing module");
        assert_eq!(log[1], "architect.remote",
            "cross-module step ref should keep its explicit module");
    }

    #[tokio::test]
    async fn orchestrator_bubbles_per_step_session_errors_with_context() {
        let pipeline = synthetic_pipeline("p",
            vec![agent_ref("a"), agent_ref("b")]);
        // Second step fails.
        let step_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let counter_clone = step_counter.clone();
        let dispatch = move |_agent: String, _input: String| {
            let mut c = counter_clone.lock().unwrap();
            *c += 1;
            let n = *c;
            Box::pin(async move {
                if n == 1 {
                    Ok("ok".to_string())
                } else {
                    // SessionError::EmptyChoices is the lightest-
                    // weight variant we can construct in a test —
                    // proxies any real per-step session failure.
                    Err(SessionError::EmptyChoices)
                }
            }) as DispatchFuture
        };
        let err: PatternError = orchestrator::run(&pipeline, "in", dispatch).await.unwrap_err();
        match err {
            PatternError::Session { pattern, step, .. } => {
                assert_eq!(pattern, "orchestrator");
                assert!(step.contains("step[1]"));
                assert!(step.contains("`b`"));
            }
            other => panic!("expected Session error; got {other:?}"),
        }
    }

    // -- router ---------------------------------------------------- //

    #[tokio::test]
    async fn router_picks_matching_case_when_classifier_returns_case_key() {
        let branch = synthetic_branch("b",
            &[("approve", pipeline_ref("yes")),
              ("reject",  pipeline_ref("no"))],
            pipeline_ref("fallback"));
        let classifier = |_prompt: String|
            Box::pin(async { Ok("approve".to_string()) }) as DispatchFuture;
        let picked = router::route(&branch, "looks good to me", classifier)
            .await.expect("route");
        assert_eq!(picked.label, "yes");
        assert_eq!(picked.kind,  RefKind::Pipeline);
    }

    #[tokio::test]
    async fn router_normalizes_classifier_reply_whitespace_and_case() {
        let branch = synthetic_branch("b",
            &[("approve", pipeline_ref("yes")),
              ("reject",  pipeline_ref("no"))],
            pipeline_ref("fallback"));
        let classifier = |_prompt: String|
            Box::pin(async { Ok("  APPROVE  \n".to_string()) }) as DispatchFuture;
        let picked = router::route(&branch, "ship it", classifier)
            .await.expect("route");
        assert_eq!(picked.label, "yes",
            "router should normalize whitespace + case before matching");
    }

    #[tokio::test]
    async fn router_falls_back_to_default_when_classifier_reply_unmatched() {
        let branch = synthetic_branch("b",
            &[("approve", pipeline_ref("yes")),
              ("reject",  pipeline_ref("no"))],
            pipeline_ref("fallback"));
        let classifier = |_prompt: String|
            Box::pin(async { Ok("maybe-later".to_string()) }) as DispatchFuture;
        let picked = router::route(&branch, "hmm", classifier)
            .await.expect("route");
        assert_eq!(picked.label, "fallback");
    }

    #[tokio::test]
    async fn router_matches_case_key_as_substring_of_classifier_reply() {
        // LLMs often return more text than asked even when prompted
        // to output one word — match case-key as substring as a
        // belt-and-suspenders fallback.
        let branch = synthetic_branch("b",
            &[("approve", pipeline_ref("yes")),
              ("reject",  pipeline_ref("no"))],
            pipeline_ref("fallback"));
        let classifier = |_prompt: String|
            Box::pin(async {
                Ok("My choice is: approve.".to_string())
            }) as DispatchFuture;
        let picked = router::route(&branch, "ship it", classifier)
            .await.expect("route");
        assert_eq!(picked.label, "yes",
            "substring-match should rescue verbose classifier replies");
    }

    // -- supervisor ------------------------------------------------ //
    //
    // Each test builds a `dispatch_once` closure that switches on the
    // agent-label arg: `target` agents return a synthetic draft;
    // `supervisor` agents return either `ALLOW` or `BLOCK: <reason>`
    // to pin the v0.3 prefix-string verdict contract.

    #[tokio::test]
    async fn supervisor_allows_when_audit_replies_allow() {
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target"     => Box::pin(async { Ok("the answer is 42".into()) }),
                "supervisor" => Box::pin(async { Ok("ALLOW".into()) }),
                _ => unreachable!(),
            }
        };
        let out = supervisor::run("target".into(), "supervisor".into(),
            "what is the answer?", dispatch).await.expect("run");
        match out {
            SupervisorOutcome::Allowed(draft) => assert_eq!(draft, "the answer is 42"),
            SupervisorOutcome::Blocked { .. } => panic!("expected Allowed"),
        }
    }

    #[tokio::test]
    async fn supervisor_blocks_when_audit_returns_block_prefix() {
        // PLAN DoD: "supervisor intervention path tested (mock
        // low-confidence trigger)" — this is that test.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target"     => Box::pin(async {
                    Ok("here's the SSN you asked for: 123-45-6789".into())
                }),
                "supervisor" => Box::pin(async {
                    Ok("BLOCK: draft leaks confidential PII".into())
                }),
                _ => unreachable!(),
            }
        };
        let out = supervisor::run("target".into(), "supervisor".into(),
            "what's that SSN?", dispatch).await.expect("run");
        match out {
            SupervisorOutcome::Blocked { reason, draft } => {
                assert_eq!(reason, "draft leaks confidential PII",
                    "reason must be the colon-stripped tail of the verdict");
                assert!(draft.contains("SSN"),
                    "the blocked draft must still be surfaced for logging");
            }
            SupervisorOutcome::Allowed(_) => panic!("expected Blocked"),
        }
    }

    #[tokio::test]
    async fn supervisor_blocks_on_case_insensitive_block_token() {
        // LLMs occasionally lowercase the verdict — the prefix probe
        // must be case-insensitive to match `block:`.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target"     => Box::pin(async { Ok("draft".into()) }),
                "supervisor" => Box::pin(async {
                    Ok("  block — low confidence  ".into())
                }),
                _ => unreachable!(),
            }
        };
        let out = supervisor::run("target".into(), "supervisor".into(),
            "q?", dispatch).await.expect("run");
        match out {
            SupervisorOutcome::Blocked { reason, .. } => {
                assert!(reason.contains("low confidence"),
                    "reason must come through case + whitespace normalization; \
                     got: {reason:?}");
            }
            SupervisorOutcome::Allowed(_) => panic!("expected Blocked"),
        }
    }

    #[tokio::test]
    async fn supervisor_bubbles_target_session_error_with_context() {
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => Box::pin(async { Err(SessionError::EmptyChoices) }),
                _        => Box::pin(async { Ok("never".into()) }),
            }
        };
        let err = supervisor::run("target".into(), "supervisor".into(),
            "q?", dispatch).await.unwrap_err();
        match err {
            PatternError::Session { pattern, step, .. } => {
                assert_eq!(pattern, "supervisor");
                assert!(step.contains("target"));
            }
            other => panic!("expected Session error; got {other:?}"),
        }
    }

    // -- reflector ------------------------------------------------- //

    #[tokio::test]
    async fn reflector_accepts_on_first_try() {
        // Critic immediately ACCEPTs the initial draft → retry_count = 0.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => Box::pin(async { Ok("draft v1".into()) }),
                "critic" => Box::pin(async { Ok("ACCEPT".into()) }),
                _ => unreachable!(),
            }
        };
        let out = reflector::run("target".into(), "critic".into(),
            "q?", /* max_retries = */ 3, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::Accepted { draft, retry_count } => {
                assert_eq!(draft, "draft v1");
                assert_eq!(retry_count, 0,
                    "no retries should have happened — critic accepted v1");
            }
            ReflectorOutcome::MaxRetries { .. } => panic!("expected Accepted"),
        }
    }

    #[tokio::test]
    async fn reflector_retries_with_feedback_then_accepts() {
        // PLAN DoD: "reflector accept + retry-with-feedback both
        // tested" — this is the retry-then-accept path.  Critic
        // rejects draft #1, accepts draft #2.  Target sees the
        // critic's feedback in its second invocation.
        let target_inputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let target_inputs_clone = target_inputs.clone();
        let target_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let target_counter_clone = target_counter.clone();
        let critic_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let critic_counter_clone = critic_counter.clone();
        let dispatch = move |agent: String, input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => {
                    target_inputs_clone.lock().unwrap().push(input);
                    let mut c = target_counter_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move { Ok(format!("draft v{n}")) })
                }
                "critic" => {
                    let mut c = critic_counter_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        // First critique rejects; second accepts.
                        if n == 1 {
                            Ok("draft is too vague — add concrete examples".into())
                        } else {
                            Ok("ACCEPT".into())
                        }
                    })
                }
                _ => unreachable!(),
            }
        };
        let out = reflector::run("target".into(), "critic".into(),
            "explain monads", 3, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::Accepted { draft, retry_count } => {
                assert_eq!(draft, "draft v2",
                    "the SECOND draft is what the critic accepted");
                assert_eq!(retry_count, 1,
                    "exactly one revision happened before acceptance");
            }
            ReflectorOutcome::MaxRetries { .. } => panic!("expected Accepted"),
        }
        // The target agent's second invocation must carry the critic's
        // feedback verbatim — pin that the feedback round-trips.
        let inputs = target_inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2, "two target invocations: initial + revision");
        assert_eq!(inputs[0], "explain monads",
            "first target input is the raw user question");
        assert!(inputs[1].contains("too vague"),
            "second target input must thread the critic's feedback; got:\n{}",
            inputs[1]);
    }

    #[tokio::test]
    async fn reflector_honours_max_retries_cap() {
        // PLAN DoD: "max-retry cap honoured" — critic NEVER accepts.
        // With max_retries = 2, expect 1 initial + 2 revisions = 3
        // target invocations, then MaxRetries outcome.
        let target_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let critic_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let target_counter_clone = target_counter.clone();
        let critic_counter_clone = critic_counter.clone();
        let dispatch = move |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => {
                    let mut c = target_counter_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move { Ok(format!("draft v{n}")) })
                }
                "critic" => {
                    *critic_counter_clone.lock().unwrap() += 1;
                    Box::pin(async { Ok("still not good enough".into()) })
                }
                _ => unreachable!(),
            }
        };
        let out = reflector::run("target".into(), "critic".into(),
            "q?", 2, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::MaxRetries { draft, attempted } => {
                assert_eq!(draft, "draft v3",
                    "the LAST attempted draft is surfaced as the degraded reply");
                assert_eq!(attempted, 3,
                    "attempt count = 1 initial + max_retries=2 revisions");
            }
            ReflectorOutcome::Accepted { .. } => panic!("expected MaxRetries"),
        }
        // Hard cap on dispatcher calls — defensive against off-by-one
        // in the loop bound.
        assert_eq!(*target_counter.lock().unwrap(), 3,
            "target invoked exactly max_retries+1 = 3 times");
        assert_eq!(*critic_counter.lock().unwrap(), 3,
            "critic invoked exactly max_retries+1 = 3 times");
    }

    #[tokio::test]
    async fn reflector_with_zero_max_retries_runs_once_then_caps() {
        // Edge: max_retries = 0 means "one attempt, no revisions".
        // If critic doesn't accept, we go straight to MaxRetries.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => Box::pin(async { Ok("draft".into()) }),
                "critic" => Box::pin(async { Ok("needs work".into()) }),
                _ => unreachable!(),
            }
        };
        let out = reflector::run("target".into(), "critic".into(),
            "q?", 0, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::MaxRetries { attempted, .. } => {
                assert_eq!(attempted, 1, "exactly one attempt for max_retries=0");
            }
            ReflectorOutcome::Accepted { .. } => panic!("expected MaxRetries"),
        }
    }

    #[tokio::test]
    async fn reflector_coder_reviewer_shape_retries_on_missing_serde_attrs() {
        // v0.3 Week 19 Fri — pins the reflector contract for the
        // Coder + Code-reviewer agents that ship under
        // `examples/agents/coder/` + `examples/agents/code_reviewer/`.
        // Coder emits a patch missing the Decision-44.4 serde attrs;
        // Reviewer rejects with a feedback hint that includes the
        // verbatim phrase "skip_serializing_if" (per reviewer prompt
        // contract); Coder revises; Reviewer ACCEPTs.
        let coder_inputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let coder_inputs_clone = coder_inputs.clone();
        let coder_call = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let coder_call_clone = coder_call.clone();
        let reviewer_call = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let reviewer_call_clone = reviewer_call.clone();
        let dispatch = move |agent: String, input: String| -> DispatchFuture {
            match agent.as_str() {
                "coder" => {
                    coder_inputs_clone.lock().unwrap().push(input);
                    let mut c = coder_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            // v1: missing serde attrs (Decision 44.4 violation)
                            Ok("## Patch\n```rust\npub datasets: Vec<Dataset>,\n```\n\
                                ## Test coverage\n- parser_accepts_dataset_block\n\
                                ## Notes\n".into())
                        } else {
                            // v2: serde attrs added
                            Ok("## Patch\n```rust\n\
                                #[serde(default, skip_serializing_if = \"Vec::is_empty\")]\n\
                                pub datasets: Vec<Dataset>,\n```\n\
                                ## Test coverage\n- parser_accepts_dataset_block\n\
                                - roundtrip_dataset_block_is_byte_stable\n\
                                ## Notes\n".into())
                        }
                    })
                }
                "code_reviewer" => {
                    let mut c = reviewer_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            // Per knowledge/01_review_checklist.md, the
                            // missing serde attrs are the canonical
                            // reject-with-feedback case.
                            Ok("Missing serde byte-stability attrs.\n\
                                \nIssues:\n\
                                - `pub datasets: Vec<Dataset>` in `Project` — \
                                add `#[serde(default, skip_serializing_if = \
                                \"Vec::is_empty\")]` so v0.2 projects \
                                round-trip unchanged (Decision 44.4).".into())
                        } else {
                            Ok("ACCEPT\npatch now byte-stable; coverage adequate.".into())
                        }
                    })
                }
                _ => unreachable!(),
            }
        };
        let architect_design = "## Block name\n`dataset`\n\n## Fields\n- path: Path\n- format: DatasetFormat\n";
        let out = reflector::run("coder".into(), "code_reviewer".into(),
            architect_design, /* max_retries = */ 3, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::Accepted { draft, retry_count } => {
                assert_eq!(retry_count, 1,
                    "exactly one revision before reviewer accepts");
                assert!(draft.contains("skip_serializing_if"),
                    "accepted draft must be the revised one with serde attrs");
            }
            ReflectorOutcome::MaxRetries { .. } => panic!("expected Accepted"),
        }
        // Pin that the reviewer's feedback flowed back to the Coder's
        // second invocation verbatim — the runtime's revision-prompt
        // builder must include the reviewer's "skip_serializing_if"
        // hint in the next-turn input.
        let inputs = coder_inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2,
            "coder invoked twice: initial + one revision");
        assert!(inputs[0].contains("## Block name"),
            "first coder input is the Architect's design verbatim");
        assert!(inputs[1].contains("skip_serializing_if"),
            "second coder input must thread the reviewer's feedback; got:\n{}",
            inputs[1]);
    }

    #[tokio::test]
    async fn reflector_coder_reviewer_rejects_hcl_block_hallucination() {
        // v0.4 Week 21 Wed — pins the Reviewer's NEGATIVE-shape
        // gate from the corpus parity rewrite: a patch that uses
        // `hcl::Block` / `hcl::Body` must be REJECTED on first
        // pass (closes the W20 Thu dogfood-finding gap where the
        // Reviewer ACCEPTed the hallucinated patch).  Coder v1
        // emits the canonical hallucination shape; Reviewer
        // rejects with feedback naming the `hcl::*` issue + the
        // canonical to_<block> shape; Coder v2 emits a fix;
        // Reviewer ACCEPTs.
        let coder_inputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let coder_inputs_clone = coder_inputs.clone();
        let coder_call    = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let coder_call_clone = coder_call.clone();
        let reviewer_call = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let reviewer_call_clone = reviewer_call.clone();
        let dispatch = move |agent: String, input: String| -> DispatchFuture {
            match agent.as_str() {
                "coder" => {
                    coder_inputs_clone.lock().unwrap().push(input);
                    let mut c = coder_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            // v1: the W20 Thu dogfood-finding shape
                            // — `hcl::Block` parser-converter that
                            // doesn't compile against the real codebase.
                            Ok("## Patch\n```rust\nuse hcl::Block;\nfn parse_dataset_block(block: &hcl::Block) -> Result<Dataset, ParseError> {\n  // ...\n}\n```\n".into())
                        } else {
                            // v2: real shape — Chumsky RawBlock +
                            // expect_* helpers per the W21 Wed
                            // corpus rewrite.
                            Ok("## Patch\n```rust\nfn to_dataset(raw: RawBlock) -> Result<Dataset, String> {\n    let label = require_single_label(\"dataset\", raw.labels)?;\n    // ...\n}\n```\n## Test coverage\n- parser_accepts_dataset_block\n## Notes\n".into())
                        }
                    })
                }
                "code_reviewer" => {
                    let mut c = reviewer_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            // The W21 Wed corpus's NEGATIVE-shape
                            // gate verbatim: name the hcl::* + the
                            // canonical replacement.
                            Ok("FAIL: parser-converter uses `hcl::*` types that don't exist in aislang-core.\n\
                                \nIssues:\n\
                                - `use hcl::Block` — DELETE.  The hcl crate is not a dependency.\n\
                                - `fn parse_dataset_block(block: &hcl::Block)` — REWRITE as \
                                `fn to_dataset(raw: RawBlock) -> Result<Dataset, String>` + use \
                                the canonical to_prompt shape from knowledge/04_real_parser_shape.md.".into())
                        } else {
                            Ok("ACCEPT\nparser-converter now follows the canonical RawBlock shape.".into())
                        }
                    })
                }
                _ => unreachable!(),
            }
        };
        let architect_design = "## Block name\n`dataset`\n\n## Fields\n- path: Path\n";
        let out = reflector::run("coder".into(), "code_reviewer".into(),
            architect_design, /* max_retries = */ 3, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::Accepted { draft, retry_count } => {
                assert_eq!(retry_count, 1,
                    "exactly one revision before reviewer accepts the hcl::* fix");
                assert!(draft.contains("RawBlock"),
                    "accepted draft must use the canonical RawBlock shape");
                assert!(!draft.contains("hcl::"),
                    "accepted draft must not contain the rejected `hcl::*` shape");
            }
            ReflectorOutcome::MaxRetries { .. } => panic!("expected Accepted"),
        }
        // Pin that the reviewer's hcl-specific feedback flowed back
        // to the Coder's second invocation verbatim — the W21 Wed
        // corpus rewrite hinges on this round-trip working.
        let inputs = coder_inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2,
            "coder invoked twice: initial + one revision");
        assert!(inputs[1].contains("hcl::") || inputs[1].contains("RawBlock"),
            "second coder input must thread the hcl-specific feedback; got:\n{}",
            inputs[1]);
    }

    #[tokio::test]
    async fn reflector_coder_tester_shape_retries_on_missing_rejection_test() {
        // v0.3 Week 20 Tue — pins the reflector contract for the
        // Coder + Tester pairing.  Mirrors Mon's coder ↔ reviewer
        // test (W19 Fri's contract) but with the Tester as critic:
        // Coder v1 emits a patch + test coverage that SKIPS a
        // rejection test the Architect's design called for;
        // Tester returns `FAIL: <gap>` pointing at the specific
        // missing test name; Coder v2 adds the rejection test;
        // Tester ACCEPTs.
        let coder_inputs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let coder_inputs_clone = coder_inputs.clone();
        let coder_call = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let coder_call_clone = coder_call.clone();
        let tester_call = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let tester_call_clone = tester_call.clone();
        let dispatch = move |agent: String, input: String| -> DispatchFuture {
            match agent.as_str() {
                "coder" => {
                    coder_inputs_clone.lock().unwrap().push(input);
                    let mut c = coder_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            // v1: happy-path test only; rejection test
                            // missing for the closed-enum `format` field.
                            Ok("## Patch\n```rust\npub datasets: Vec<Dataset>,\n```\n\
                                ## Test coverage\n- parser_accepts_dataset_block\n\
                                ## Notes\n".into())
                        } else {
                            // v2: rejection test added per Tester
                            // feedback.
                            Ok("## Patch\n```rust\npub datasets: Vec<Dataset>,\n```\n\
                                ## Test coverage\n- parser_accepts_dataset_block\n\
                                - parser_rejects_dataset_block_with_unknown_format\n\
                                ## Notes\n".into())
                        }
                    })
                }
                "tester" => {
                    let mut c = tester_call_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        if n == 1 {
                            Ok("FAIL: rejection test missing for `format` enum.\n\
                                \nGaps:\n\
                                - `parser_rejects_dataset_block_with_unknown_format` \
                                — assert on `matches!(err, ParseError::InvalidEnum \
                                { .. })`.".into())
                        } else {
                            Ok("ACCEPT\ncoverage hits every Architect-declared \
                                check.".into())
                        }
                    })
                }
                _ => unreachable!(),
            }
        };
        let architect_design = "## Block name\n`dataset`\n\n## Fields\n- format: DatasetFormat (closed enum)\n";
        let out = reflector::run("coder".into(), "tester".into(),
            architect_design, /* max_retries = */ 3, dispatch).await.expect("run");
        match out {
            ReflectorOutcome::Accepted { draft, retry_count } => {
                assert_eq!(retry_count, 1,
                    "exactly one revision before tester accepts");
                assert!(draft.contains("parser_rejects_dataset_block_with_unknown_format"),
                    "accepted draft must be the revised one with the rejection test");
            }
            ReflectorOutcome::MaxRetries { .. } => panic!("expected Accepted"),
        }
        // Pin that the tester's feedback flowed back to the Coder's
        // second invocation verbatim — same revision-prompt builder
        // as the coder ↔ reviewer test (W19 Fri).
        let inputs = coder_inputs.lock().unwrap();
        assert_eq!(inputs.len(), 2,
            "coder invoked twice: initial + one revision");
        assert!(inputs[1].contains("parser_rejects_dataset_block_with_unknown_format"),
            "second coder input must thread the tester's feedback verbatim; got:\n{}",
            inputs[1]);
    }

    #[tokio::test]
    async fn reflector_bubbles_critic_session_error_with_context() {
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "target" => Box::pin(async { Ok("draft".into()) }),
                "critic" => Box::pin(async { Err(SessionError::EmptyChoices) }),
                _ => unreachable!(),
            }
        };
        let err = reflector::run("target".into(), "critic".into(),
            "q?", 2, dispatch).await.unwrap_err();
        match err {
            PatternError::Session { pattern, step, .. } => {
                assert_eq!(pattern, "reflector");
                assert!(step.contains("critic"));
                assert!(step.contains("attempt 1"));
            }
            other => panic!("expected Session error; got {other:?}"),
        }
    }

    // -- planner_executor ----------------------------------------- //
    //
    // Each test's `dispatch_once` switches on the agent-label arg:
    // `planner` agents return EXECUTE or DECOMPOSE verdicts;
    // `executor` agents return synthetic leaf results.  The synthetic
    // planner replies mock LLM output: DECOMPOSE listings include the
    // various bullet/number prefixes parse_subtasks must strip.

    #[tokio::test]
    async fn planner_executor_runs_single_leaf_when_planner_says_execute() {
        // Planner sees the user input + says EXECUTE → executor runs
        // once → final reply is just the executor's result.  No
        // decomposition history.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner"  => Box::pin(async { Ok("EXECUTE".into()) }),
                "executor" => Box::pin(async { Ok("answer: 42".into()) }),
                _ => unreachable!(),
            }
        };
        let out = planner_executor::run("planner".into(), "executor".into(),
            "what's 6*7?", /* max_depth = */ 3, /* max_total_steps = */ 16,
            dispatch).await.expect("run");
        assert_eq!(out.steps_executed, 1);
        assert_eq!(out.max_depth_reached, 0);
        assert!(out.plan_history.is_empty(),
            "no decomposition happened → plan_history must be empty");
        assert_eq!(out.leaf_results, vec!["answer: 42".to_string()]);
        assert_eq!(out.final_reply, "answer: 42");
        assert!(!out.budget_exhausted);
    }

    #[tokio::test]
    async fn planner_executor_decomposes_then_executes_leaves() {
        // First planner call (depth=0) returns DECOMPOSE with 2 subtasks.
        // Subsequent planner calls (depth=1) return EXECUTE for each.
        // Outcome: 2 executor results aggregated; max_depth_reached=1.
        let planner_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let pc_clone = planner_counter.clone();
        let exec_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let ec_clone = exec_counter.clone();
        let dispatch = move |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner" => {
                    let mut c = pc_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move {
                        // First call: decompose into 2 subtasks (with
                        // mixed bullet styles to exercise the stripper).
                        if n == 1 {
                            Ok("DECOMPOSE:\n- subtask one\n2. subtask two".into())
                        } else {
                            Ok("EXECUTE".into())
                        }
                    })
                }
                "executor" => {
                    let mut c = ec_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Box::pin(async move { Ok(format!("leaf-{n}")) })
                }
                _ => unreachable!(),
            }
        };
        let out = planner_executor::run("planner".into(), "executor".into(),
            "do the thing", 3, 16, dispatch).await.expect("run");
        assert_eq!(out.steps_executed, 2,
            "exactly 2 leaves were executed");
        assert_eq!(out.max_depth_reached, 1,
            "BFS reached depth 1 (the 2 subtasks at depth=1)");
        assert_eq!(out.plan_history.len(), 1,
            "exactly one DECOMPOSE happened (the root)");
        assert_eq!(out.leaf_results, vec!["leaf-1".to_string(), "leaf-2".to_string()]);
        assert!(out.final_reply.contains("leaf-1")
                && out.final_reply.contains("leaf-2"),
            "final reply must aggregate both leaf results");
    }

    #[tokio::test]
    async fn planner_executor_forces_executor_at_max_depth() {
        // PLAN DoD: "planner_executor depth-bound BFS terminates
        // within max-depth budget".  We use max_depth = 1: the
        // root decomposes (planner called at depth 0), but at
        // depth 1 the planner is SKIPPED entirely + executor runs
        // directly.  Counting planner calls verifies the cap.
        let planner_counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let pc_clone = planner_counter.clone();
        let dispatch = move |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner" => {
                    *pc_clone.lock().unwrap() += 1;
                    Box::pin(async {
                        // Always decompose into 3 subtasks; depth cap
                        // is what stops the recursion.
                        Ok("DECOMPOSE:\n- a\n- b\n- c".into())
                    })
                }
                "executor" => Box::pin(async { Ok("leaf".into()) }),
                _ => unreachable!(),
            }
        };
        let out = planner_executor::run("planner".into(), "executor".into(),
            "do something", 1, 16, dispatch).await.expect("run");
        assert_eq!(*planner_counter.lock().unwrap(), 1,
            "planner called exactly ONCE (depth=0); at depth=1 the cap forces \
             executor without consulting planner");
        assert_eq!(out.steps_executed, 3, "3 subtasks at depth=1 all executed");
        assert_eq!(out.max_depth_reached, 1);
    }

    #[tokio::test]
    async fn planner_executor_honours_max_total_steps_budget() {
        // Hard global budget cap: planner keeps decomposing
        // recursively into 2 subtasks per level; we set
        // max_total_steps = 4 → BFS terminates early after 4
        // executor calls.  budget_exhausted must be true.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner" => Box::pin(async {
                    // Each planner call says decompose into 2; with
                    // max_depth=3 and bushy decomposition the queue
                    // grows fast.  Budget cap should fire first.
                    Ok("DECOMPOSE:\n- sub-a\n- sub-b".into())
                }),
                "executor" => Box::pin(async { Ok("leaf".into()) }),
                _ => unreachable!(),
            }
        };
        let out = planner_executor::run("planner".into(), "executor".into(),
            "task", /* max_depth = */ 3, /* max_total_steps = */ 4,
            dispatch).await.expect("run");
        assert_eq!(out.steps_executed, 4,
            "budget cap = 4 → exactly 4 executor invocations");
        assert!(out.budget_exhausted,
            "budget_exhausted must be set when we stopped on the cap");
    }

    #[tokio::test]
    async fn planner_executor_coerces_executor_on_empty_decompose() {
        // Defensive: a planner that says `DECOMPOSE:` with no body
        // (empty subtask list) must still make progress.  We fall
        // back to executor for that task so the BFS doesn't stall.
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner"  => Box::pin(async { Ok("DECOMPOSE:".into()) }),
                "executor" => Box::pin(async { Ok("fallback-result".into()) }),
                _ => unreachable!(),
            }
        };
        let out = planner_executor::run("planner".into(), "executor".into(),
            "task", 2, 16, dispatch).await.expect("run");
        assert_eq!(out.steps_executed, 1,
            "empty-decompose must coerce to a single executor call");
        assert_eq!(out.leaf_results, vec!["fallback-result".to_string()]);
        // Empty DECOMPOSE still gets recorded — useful for tracing
        // why the planner stalled.
        assert_eq!(out.plan_history.len(), 1);
    }

    #[tokio::test]
    async fn planner_executor_bubbles_planner_session_error_with_context() {
        let dispatch = |agent: String, _input: String| -> DispatchFuture {
            match agent.as_str() {
                "planner" => Box::pin(async { Err(SessionError::EmptyChoices) }),
                _         => Box::pin(async { Ok("never".into()) }),
            }
        };
        let err = planner_executor::run("planner".into(), "executor".into(),
            "task", 3, 16, dispatch).await.unwrap_err();
        match err {
            PatternError::Session { pattern, step, .. } => {
                assert_eq!(pattern, "planner_executor");
                assert!(step.contains("planner"));
                assert!(step.contains("depth=0"));
            }
            other => panic!("expected Session error; got {other:?}"),
        }
    }

    // -- coordinator ---------------------------------------------- //

    #[tokio::test]
    async fn coordinator_converges_when_round_produces_no_change() {
        // PLAN DoD: peer-to-peer convergence on fixed point.  Two
        // peers; round 1 both peers append distinct contributions;
        // round 2 both peers echo unchanged → converged after round 2.
        let counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let c_clone = counter.clone();
        let dispatch = move |_agent: String, input: String| -> DispatchFuture {
            let mut c = c_clone.lock().unwrap();
            *c += 1;
            let n = *c;
            Box::pin(async move {
                // Calls 1, 2 (round 1) modify the message; calls 3, 4
                // (round 2) echo unchanged.
                if n <= 2 {
                    Ok(format!("{input}\npeer-call-{n}"))
                } else {
                    Ok(input)
                }
            })
        };
        let out = coordinator::run(vec!["peer-a".into(), "peer-b".into()],
            "kickoff", /* max_rounds = */ 5, dispatch).await.expect("run");
        match out {
            CoordinatorOutcome::Converged { final_message, rounds } => {
                assert_eq!(rounds, 2,
                    "convergence on round 2 (round 1 changed, round 2 didn't)");
                assert!(final_message.contains("peer-call-1")
                        && final_message.contains("peer-call-2"));
            }
            CoordinatorOutcome::MaxRounds { .. } => panic!("expected Converged"),
        }
    }

    #[tokio::test]
    async fn coordinator_honours_max_rounds_cap() {
        // PLAN DoD: "coordinator round-robin honours max-rounds cap".
        // Peers NEVER converge — every call appends a unique tag.
        // With max_rounds=2 + 3 peers, expect exactly 2×3 = 6 calls
        // + MaxRounds outcome.
        let counter = std::sync::Arc::new(std::sync::Mutex::new(0u32));
        let c_clone = counter.clone();
        let dispatch = move |_agent: String, input: String| -> DispatchFuture {
            let mut c = c_clone.lock().unwrap();
            *c += 1;
            let n = *c;
            Box::pin(async move { Ok(format!("{input}+{n}")) })
        };
        let out = coordinator::run(
            vec!["a".into(), "b".into(), "c".into()],
            "start", 2, dispatch).await.expect("run");
        match out {
            CoordinatorOutcome::MaxRounds { final_message: _, rounds } => {
                assert_eq!(rounds, 2);
            }
            CoordinatorOutcome::Converged { .. } => panic!("expected MaxRounds"),
        }
        assert_eq!(*counter.lock().unwrap(), 6,
            "exactly max_rounds × peers = 2 × 3 = 6 dispatch calls");
    }

    #[tokio::test]
    async fn coordinator_visits_peers_in_declared_round_robin_order() {
        // Pin the round-robin order: with peers [a, b, c] declared,
        // dispatch must visit a → b → c each round.
        let visits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let v_clone = visits.clone();
        let dispatch = move |agent: String, input: String| -> DispatchFuture {
            v_clone.lock().unwrap().push(agent);
            Box::pin(async move { Ok(input) })   // immediate convergence (round 1)
        };
        let _ = coordinator::run(
            vec!["a".into(), "b".into(), "c".into()],
            "x", 3, dispatch).await.expect("run");
        let order = visits.lock().unwrap();
        assert_eq!(*order, vec!["a".to_string(), "b".to_string(), "c".to_string()],
            "round-robin must follow declared peer order; got {order:?}");
    }

    #[tokio::test]
    async fn coordinator_rejects_fewer_than_two_peers() {
        // Defensive: 1-peer (or 0-peer) coordination is a misconfig.
        let dispatch = |_agent: String, _input: String| -> DispatchFuture {
            Box::pin(async { Ok("never called".into()) })
        };
        let err = coordinator::run(vec!["solo".into()], "x", 3, dispatch).await
            .unwrap_err();
        match err {
            PatternError::Session { pattern, step, .. } => {
                assert_eq!(pattern, "coordinator");
                assert!(step.contains("peer-list-validation"));
                assert!(step.contains("got 1"));
            }
            other => panic!("expected validation error; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn coordinator_with_zero_max_rounds_returns_input_unchanged() {
        // Edge: max_rounds = 0 → no work; surface user_input as the
        // final message under MaxRounds (NOT converged, since there
        // was no opportunity to converge).
        let dispatch = |_agent: String, _input: String| -> DispatchFuture {
            Box::pin(async { Ok("never called".into()) })
        };
        let out = coordinator::run(
            vec!["a".into(), "b".into()],
            "hello", 0, dispatch).await.expect("run");
        match out {
            CoordinatorOutcome::MaxRounds { final_message, rounds } => {
                assert_eq!(rounds, 0);
                assert_eq!(final_message, "hello");
            }
            CoordinatorOutcome::Converged { .. } => panic!("expected MaxRounds"),
        }
    }
}
