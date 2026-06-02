# `examples/patterns/` — composition-pattern demos

Worked `.ais` files exercising the v0.3 composition primitives + their
matching pattern-runtime dispatchers.  Each demo is self-contained: a
single `.ais` file + co-located prompts (+ knowledge base if needed)
inside its own subdirectory.  Run them with `aislang apply
<demo-dir>/<demo>.ais` (stand the Compose stack up) and then
`aislang chat <pipeline-or-branch-label> <demo-dir>/<demo>.ais`.

Each demo lives in its OWN subdirectory because `aislang plan` writes
`aislang.lock` + `litellm_config.yaml` next to the source file;
multiple demos sharing one parent dir would overwrite each other's
generated artifacts.

| Demo | Composition primitive | Pattern (runtime) | Week shipped |
|---|---|---|---|
| [`orchestrator-demo/`](./orchestrator-demo/orchestrator-demo.ais) | `pipeline { steps = [...] }` | orchestrator (sequential dispatch) | Week 17 Thu |
| [`router-demo/`](./router-demo/router-demo.ais) | `branch { cases = ... }` | router (classifier dispatch) | Week 17 Fri |
| `reflector-demo/` *(stub for Week 18+)* | `loop { body = pipeline.<p> }` | reflector (critique-and-retry) | deferred |
| `supervisor-demo/` *(stub for Week 18+)* | (no grammar block in v0.3) | supervisor (audit + intervene) | deferred |

## Why single-file vs multi-module?

These demos are single-file `.ais` deliberately — the goal is to make
the composition-primitive shape easy to read in one screen, without the
extra cognitive load of `aislang.toml` + per-module `agents/*.ais` +
cross-module refs.  Multi-module composition lands the SAME runtime;
see `examples/multi-module-demo/` for a fixture that exercises module-
qualified labels through the chat path.

## `orchestrator-demo/` walk-through

A two-agent pipeline:

1. **`agent.researcher`** reads `/data/company-info.md` via a
   bind-mounted `mcp_server.docs` and returns the verbatim quotes
   relevant to the user's question.  The agent's system prompt
   instructs it to NOT paraphrase — the next step handles that.
2. **`agent.summarizer`** receives the researcher's raw output and
   REWRITES it in 2nd-person customer-friendly voice ("We accept
   returns..." → "You can return your purchase...").

```hcl
pipeline "research_then_summarize" {
  steps  = [agent.researcher, agent.summarizer]
  input  = "the user's question — e.g. 'Where is Acme HQ?'"
  output = "2nd-person customer-friendly reply drawn from the KB"
}
```

Live trace:

```text
$ aislang apply examples/patterns/orchestrator-demo/orchestrator-demo.ais
$ aislang chat research_then_summarize examples/patterns/orchestrator-demo/orchestrator-demo.ais
> What is your returns policy?
  · orchestrator step 1/2 → agent `researcher`
  · iteration 1/8 — calling LLM
  · tool `docs.read_text_file` returned 1 fragment (1117B)
  · final answer ready (189B)
    We accept returns within **30 days from purchase**. The product
    must be in its **original packaging** with all accessories included.
    Refunds are processed within 5 business days of receipt.
  · orchestrator step 2/2 → agent `summarizer`
  · final answer ready (177B)
You can return your purchase within 30 days as long as it's in its
original packaging with all accessories included.  We'll process your
refund within 5 business days of receipt.
```

The single final stdout line is the summarizer's reply; the dim italic
stderr lines trace each pipeline step + the researcher's intermediate
output (the 1st-person company-voice version).  Both replies are
distinct — that's how you SEE the pipeline is doing real work, not
just passing the text through.

## `router-demo/` walk-through

A three-way customer-support intake router.  Each user message goes
through a classifier agent that picks one of `{billing, technical,
general}`, and the matched case's pipeline produces the actual reply.

```hcl
branch "triage" {
  on      = "{{ classifier_result }}"
  cases   = {
    billing   = pipeline.handle_billing,
    technical = pipeline.handle_technical,
    general   = pipeline.handle_general,
  }
  default = pipeline.handle_general
}
```

Live trace:

```text
$ aislang apply examples/patterns/router-demo/router-demo.ais
$ aislang chat triage examples/patterns/router-demo/router-demo.ais
> How do I reset my password?
  · router classifying via agent `classifier` against cases [billing, general, technical]
  · router → case `technical` (1 steps)
  · orchestrator step 1/1 → agent `technical`
You can reset your password from the login page → "Forgot password"
link.  We'll email you a reset link valid for 30 minutes.

> How do I update my payment method?
  · router → case `billing` (1 steps)
You can update your payment method in your account settings under
Billing → Payment Methods.  Changes apply to your next invoice.
```

**v0.3 classifier-agent convention**: the project's FIRST declared
agent acts as the router's classifier.  In `router-demo.ais` we
declare `agent "classifier"` first for this reason.  v0.5+ will add
an explicit `classifier = agent.<label>` field in the `branch { … }`
block.

## Comparison with single-agent chat

Both demos let you chat each individual agent directly to isolate
behaviour during prompt-tuning:

```sh
# Just the researcher — raw verbatim quotes from the KB
aislang chat researcher examples/patterns/orchestrator-demo/orchestrator-demo.ais

# Just the billing agent — no router in front
aislang chat billing examples/patterns/router-demo/router-demo.ais
```

Useful when debugging: chat each step alone, fix its shape, then
re-run the pipeline / branch to verify the chain still works.

## What lands later

- `reflector-demo/` — a `loop { body = pipeline.<p> condition = "{{
  critic.accept }}" }` block running a draft→critique cycle.  Runtime
  shipped Fri (`patterns::reflector`); the worked .ais demo lands once
  the loop-block runtime is wired through the chat surface.
- `supervisor-demo/` — supervisor pattern as a chat-callable demo.
  Runtime shipped Fri (`patterns::supervisor`); needs either a
  grammar block (v0.5+) or a tooling/skill harness to invoke.
- `planner_executor-demo/` (Week 18 Mon) — heuristic plan + execute.
- `coordinator-demo/` (Week 18 Mon) — peer-to-peer round-robin.
