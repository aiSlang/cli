//! v0.3 Week 20 Mon — integration test for the multi-module
//! orchestrator project under `examples/agents/orchestrator/`.
//!
//! Confirms three things end-to-end against the on-disk fixture:
//!   1. **Multi-module load + cross-module refs resolve.**
//!      `load_project_input` on the directory pulls in 4 modules
//!      (orchestrator + architect + coder + code_reviewer) and
//!      surfaces module-qualified labels.
//!   2. **`pipeline.full_design` dispatches via the orchestrator
//!      pattern** with the expected 3-step cross-module chain.
//!   3. **`loop.review_cycle` body parses as the inner
//!      `coder_review_cycle` pipeline + the runtime honours
//!      `max_iter`** when driven through `patterns::reflector::run`
//!      with mocked dispatch where the critic NEVER ACCEPTs.
//!
//! No LiteLLM, no Ollama, no network — every dispatch is mocked
//! to keep this test in the standard unit-test CI lane.  The
//! per-leg live e2e (qwen2.5-coder for Coder, qwen3:8b for the
//! reasoning legs) stays a user-side smoke at the CLI layer.

use aislang_runtime::{
    load_project_input,
    orchestrator, reflector,
    DispatchFuture, PatternError, ReflectorOutcome,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn orchestrator_dir() -> PathBuf {
    // Cargo runs tests with CWD set to the crate root
    // (`crates/aislang-runtime/`), so we hop two levels up to
    // reach the workspace + then into examples/.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join("examples/agents/orchestrator")
}

#[tokio::test]
async fn orchestrator_multi_module_loads_with_4_modules_and_qualified_labels() {
    // Load the on-disk fixture + assert the 4-module shape.  The
    // multi-module loader merges architect / coder / code_reviewer
    // / orchestrator namespaces; cross-module refs in
    // `orchestrator.full_design.steps` MUST resolve through the
    // merged symbol table or the load fails.
    let dir = orchestrator_dir();
    let loaded = load_project_input(&dir)
        .expect("multi-module orchestrator project must load");
    let proj = &loaded.resolved.project;

    // 4 agents: architect, coder, code_reviewer.  (Orchestrator
    // module itself declares no `agent` block — it's pure
    // composition surface.)
    let agent_labels: Vec<&str> = proj.agents.iter()
        .map(|a| a.label.as_str()).collect();
    assert!(agent_labels.iter().any(|l| *l == "architect.architect"),
        "architect.architect missing from merged labels: {agent_labels:?}");
    assert!(agent_labels.iter().any(|l| *l == "coder.coder"),
        "coder.coder missing from merged labels: {agent_labels:?}");
    assert!(agent_labels.iter().any(|l| *l == "code_reviewer.code_reviewer"),
        "code_reviewer.code_reviewer missing: {agent_labels:?}");

    // 2 pipelines on the orchestrator module: full_design (3-step)
    // + coder_review_cycle (2-step) — plus the placeholder
    // pipelines from each sub-module.
    let pipeline_labels: Vec<&str> = proj.pipelines.iter()
        .map(|p| p.label.as_str()).collect();
    assert!(pipeline_labels.iter().any(|l| *l == "orchestrator.full_design"),
        "orchestrator.full_design missing: {pipeline_labels:?}");
    assert!(pipeline_labels.iter().any(|l| *l == "orchestrator.coder_review_cycle"),
        "orchestrator.coder_review_cycle missing: {pipeline_labels:?}");

    // 1 loop: review_cycle, body = pipeline.coder_review_cycle,
    // max_iter = 3.
    let loop_labels: Vec<&str> = proj.loops.iter()
        .map(|l| l.label.as_str()).collect();
    assert!(loop_labels.iter().any(|l| *l == "orchestrator.review_cycle"),
        "orchestrator.review_cycle missing: {loop_labels:?}");
    let review_loop = proj.loops.iter()
        .find(|l| l.label == "orchestrator.review_cycle")
        .expect("review_cycle loop");
    assert_eq!(review_loop.max_iter, 3,
        "max_iter pinned at 3 per orchestrator.ais");
    // Body resolves to the inner pipeline; the ref's module is
    // None when the loop + body live in the same module — the
    // resolver flows the containing-module prefix through the
    // pipeline LABEL, not via the ref.
    assert_eq!(review_loop.body.label, "coder_review_cycle",
        "loop.body ref points at coder_review_cycle pipeline");
}

#[tokio::test]
async fn full_design_pipeline_dispatches_3_steps_in_order() {
    // PLAN DoD: "pipeline.full_design dispatches the 3 sub-agents
    // end-to-end via runtime patterns::orchestrator::run (mocked
    // dispatch unit test)".  Loads the on-disk fixture + drives
    // the pipeline through `orchestrator::run` with synthetic
    // sub-agent replies that thread the cumulative input forward.
    let dir = orchestrator_dir();
    let loaded = load_project_input(&dir).expect("load");
    let proj = &loaded.resolved.project;
    let full_design = proj.pipelines.iter()
        .find(|p| p.label == "orchestrator.full_design")
        .expect("full_design pipeline");

    let observed_steps = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let observed_clone = observed_steps.clone();
    let dispatch = move |agent: String, input: String| -> DispatchFuture {
        let observed = observed_clone.clone();
        Box::pin(async move {
            observed.lock().unwrap().push((agent.clone(), input.clone()));
            // Synthetic per-step reply that mimics the contract:
            // Architect → 5-section design; Coder → 3-section
            // patch; Reviewer → ACCEPT.
            let reply = match agent.as_str() {
                "architect.architect" =>
                    "## Block name\n`dataset`\n## Fields\n- path: Path\n".to_string(),
                "coder.coder" =>
                    "## Patch\n```rust\nstruct Dataset {}\n```\n## Test coverage\n- foo\n".to_string(),
                "code_reviewer.code_reviewer" =>
                    "ACCEPT\nbyte-stable.".to_string(),
                other => panic!("unexpected dispatch target {other}"),
            };
            Ok(reply)
        })
    };

    let final_reply = orchestrator::run(
        full_design,
        "Design a `dataset` block for declaring training data",
        dispatch,
    ).await.expect("run");

    assert!(final_reply.starts_with("ACCEPT"),
        "final reply is the reviewer's verdict; got:\n{final_reply}");

    let steps = observed_steps.lock().unwrap();
    assert_eq!(steps.len(), 3,
        "exactly 3 dispatches (architect → coder → reviewer)");
    assert_eq!(steps[0].0, "architect.architect");
    assert_eq!(steps[1].0, "coder.coder");
    assert_eq!(steps[2].0, "code_reviewer.code_reviewer");

    // Pin the cumulative-input-feeds-forward contract: each step
    // sees the PRIOR step's reply verbatim as its user input.
    assert_eq!(steps[0].1, "Design a `dataset` block for declaring training data",
        "step 0 input is the original user spec");
    assert!(steps[1].1.contains("## Block name"),
        "step 1 (coder) sees architect's design verbatim; got:\n{}", steps[1].1);
    assert!(steps[2].1.contains("## Patch"),
        "step 2 (reviewer) sees coder's patch verbatim; got:\n{}", steps[2].1);
}

#[tokio::test]
async fn review_cycle_loop_max_iter_3_caps_when_reviewer_never_accepts() {
    // PLAN DoD: "loop wrap of the coder ↔ reviewer cycle parses +
    // the runtime honours `max_iter`".  Drives the inner
    // `coder_review_cycle` pipeline's [coder, reviewer] pair
    // through `patterns::reflector::run` with the reviewer
    // ALWAYS returning a non-ACCEPT verdict.  Asserts the runtime
    // caps at `max_iter = 3` (= 3 + 1 = 4 total target attempts:
    // 1 initial + 3 revisions, per the reflector contract).
    let dir = orchestrator_dir();
    let loaded = load_project_input(&dir).expect("load");
    let proj = &loaded.resolved.project;

    // Resolve the loop's max_iter from the on-disk AST (don't
    // hardcode — this is the whole point of the integration
    // test: surface a PLAN-edit-drift if someone bumps max_iter
    // in orchestrator.ais without re-running this test).
    let review_loop = proj.loops.iter()
        .find(|l| l.label == "orchestrator.review_cycle")
        .expect("review_cycle loop");
    let max_iter = review_loop.max_iter;
    assert_eq!(max_iter, 3, "fixture pin — keep .ais + test in sync");

    let coder_calls = Arc::new(Mutex::new(0u32));
    let reviewer_calls = Arc::new(Mutex::new(0u32));
    let coder_clone = coder_calls.clone();
    let reviewer_clone = reviewer_calls.clone();
    let dispatch = move |agent: String, _input: String| -> DispatchFuture {
        let coder_clone = coder_clone.clone();
        let reviewer_clone = reviewer_clone.clone();
        Box::pin(async move {
            match agent.as_str() {
                "coder.coder" => {
                    let mut c = coder_clone.lock().unwrap();
                    *c += 1;
                    let n = *c;
                    Ok(format!("## Patch v{n}\n```rust\n// missing serde attrs\n```\n## Test coverage\n## Notes\n"))
                }
                "code_reviewer.code_reviewer" => {
                    *reviewer_clone.lock().unwrap() += 1;
                    Ok("Add #[serde(default, skip_serializing_if = \"Vec::is_empty\")].".into())
                }
                other => panic!("unexpected dispatch target {other}"),
            }
        })
    };

    let outcome = reflector::run(
        "coder.coder".into(),
        "code_reviewer.code_reviewer".into(),
        "## Block name\n`dataset`\n",
        max_iter,
        dispatch,
    ).await.expect("run");

    match outcome {
        ReflectorOutcome::MaxRetries { attempted, .. } => {
            // 1 initial + max_iter=3 revisions = 4 target attempts.
            assert_eq!(attempted, max_iter + 1,
                "attempted = 1 initial + max_iter revisions");
        }
        ReflectorOutcome::Accepted { .. } =>
            panic!("reviewer never returned ACCEPT — expected MaxRetries"),
    }
    assert_eq!(*coder_calls.lock().unwrap(), max_iter + 1,
        "coder dispatched exactly max_iter+1 times");
    assert_eq!(*reviewer_calls.lock().unwrap(), max_iter + 1,
        "reviewer dispatched exactly max_iter+1 times");
}

#[tokio::test]
async fn pipeline_dispatch_propagates_session_error_with_step_context() {
    // Sanity: a SessionError mid-pipeline must surface with the
    // failing step's qualified label so the user knows which
    // sub-agent broke.  Same shape as the existing single-file
    // orchestrator pattern tests; pinned here at the multi-module
    // boundary because the qualified labels are what the user
    // actually sees in real failure traces.
    let dir = orchestrator_dir();
    let loaded = load_project_input(&dir).expect("load");
    let proj = &loaded.resolved.project;
    let full_design = proj.pipelines.iter()
        .find(|p| p.label == "orchestrator.full_design")
        .expect("full_design");

    let dispatch = move |agent: String, _input: String| -> DispatchFuture {
        Box::pin(async move {
            if agent == "coder.coder" {
                Err(aislang_runtime::SessionError::EmptyChoices)
            } else if agent == "architect.architect" {
                Ok("## Block name\n`dataset`\n".to_string())
            } else {
                panic!("reviewer should never be reached")
            }
        })
    };

    let err = orchestrator::run(full_design, "spec", dispatch).await
        .expect_err("coder failure must abort the pipeline");
    match err {
        PatternError::Session { pattern, step, .. } => {
            assert_eq!(pattern, "orchestrator");
            assert!(step.contains("coder.coder"),
                "step context must name the failing qualified label; got: {step}");
        }
        other => panic!("expected Session error, got {other:?}"),
    }
}
