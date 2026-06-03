# Composition primitives (v0.3 Week 17)

aiSlang ships three composition primitives, each a top-level block:

  - **`pipeline "<label>" { steps = [agent.a, agent.b, …] }`** —
    sequential agent composition.  Each step's reply becomes the
    next step's user_input.  Wired to the runtime's orchestrator
    pattern at chat time.

  - **`loop "<label>" { body = pipeline.<p>  condition = "…"
    max_iter = N }`** — conditional re-execution of a pipeline
    body up to `max_iter` times.  Condition is a MiniJinja template
    (Week 18 Tue's dynamic-prompts machinery).

  - **`branch "<label>" { on = "…"  cases = { … }  default =
    pipeline.<p> }`** — N-way dispatch.  A classifier picks one of
    the case-keys; matched case-pipeline runs.  Wired to the
    runtime's router pattern at chat time.

The 6 composition PATTERNS (orchestrator / router / supervisor /
reflector / planner_executor / coordinator) land in v0.3 Phase 1.
Each is ~100-200 LoC + 5-10 tests.
