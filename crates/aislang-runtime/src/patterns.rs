//! v0.3 Week 17 Thu — composition-pattern runtime.
//!
//! Decision 44.2: the pattern state machines live HERE (not in
//! aislang-cli).  The CLI just dispatches a user's chat input
//! through the right pattern; this module owns the per-pattern
//! execution shape.  Phase 1 ships orchestrator + router (Thu) +
//! supervisor + reflector (Fri); Week 18 Mon closes the 6-pattern
//! set with planner_executor + coordinator.
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
}
