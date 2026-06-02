//! v0.3 Week 17 Thu — composition-pattern runtime.
//!
//! Decision 44.2: the pattern state machines live HERE (not in
//! aislang-cli).  The CLI just dispatches a user's chat input
//! through the right pattern; this module owns the per-pattern
//! execution shape.  Phase 1 ships orchestrator + router (Thu);
//! Fri adds supervisor + reflector; Week 18 Mon closes the
//! 6-pattern set with planner_executor + coordinator.
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
}
