# `examples/patterns/` — composition-pattern demos

Worked `.ais` files exercising the v0.3 composition primitives + their
matching pattern-runtime dispatchers.  Each demo is self-contained: a
single `.ais` file + co-located prompts + knowledge base.  Run them
with `aislang apply <demo>.ais` (stand the Compose stack up) and then
`aislang chat <pipeline-or-branch-label> <demo>.ais`.

| Demo | Composition primitive | Pattern (runtime) | Week shipped |
|---|---|---|---|
| `orchestrator-demo.ais` | `pipeline { steps = [...] }` | orchestrator (sequential dispatch) | Week 17 Thu |
| `router-demo.ais` *(stub for Fri)* | `branch { cases = ... }` | router (classifier dispatch) | Week 17 Fri |
| `reflector-demo.ais` *(stub for Fri)* | `loop { body = pipeline.<p> }` | reflector (critique-and-retry) | Week 17 Fri |

## Why single-file vs multi-module?

These demos are single-file `.ais` deliberately — the goal is to make
the composition-primitive shape easy to read in one screen, without the
extra cognitive load of `aislang.toml` + per-module `agents/*.ais` +
cross-module refs.  Multi-module composition lands the SAME runtime;
see `examples/multi-module-demo/` for a fixture that exercises module-
qualified labels through the chat path.

## `orchestrator-demo.ais` walk-through

A two-agent pipeline:

1. **`agent.researcher`** reads `/data/company-info.md` via a
   bind-mounted `mcp_server.docs` and returns the verbatim quotes
   relevant to the user's question.  The agent's system prompt
   instructs it to NOT paraphrase — the next step handles that.
2. **`agent.summarizer`** receives the researcher's raw output and
   condenses to ONE plain-English sentence.

```hcl
pipeline "research_then_summarize" {
  steps  = [agent.researcher, agent.summarizer]
  input  = "the user's question — e.g. 'Where is Acme HQ?'"
  output = "one-sentence answer drawn from the knowledge base"
}
```

Live trace:

```text
$ aislang apply examples/patterns/orchestrator-demo.ais
$ aislang chat research_then_summarize examples/patterns/orchestrator-demo.ais
> Where is Acme headquartered?
  · orchestrator step 1/2 → agent `researcher`
  · iteration 1/8 — calling LLM
  · tool `docs.read_text_file` (33B args)
  · tool `docs.read_text_file` returned 1 fragment (1117B)
  · iteration 2/8 — calling LLM
  · final answer ready (252B)
  · orchestrator step 2/2 → agent `summarizer`
  · iteration 1/8 — calling LLM
  · final answer ready (78B)
Acme Demo Company is headquartered in Berlin, Germany.
```

The single final printed line is the summarizer's output; the dim
italic lines on stderr trace each pipeline step + each tool call
inside that step's session.

## Comparison with single-agent chat

The same `orchestrator-demo.ais` lets you chat each agent directly to
isolate behaviour:

```sh
# Just the researcher — raw verbatim quotes
aislang chat researcher examples/patterns/orchestrator-demo.ais

# Just the summarizer — needs you to paste raw input
aislang chat summarizer examples/patterns/orchestrator-demo.ais
```

Useful when tuning prompt templates: chat each step alone, debug its
shape, then re-run the pipeline to verify the chain still works.

## What lands here in Week 17 Fri

- `router-demo.ais` — a `branch { cases }` block dispatching to
  different pipelines based on a classifier model's verdict on the
  user input.  Wired by `patterns::router::route`.
- `reflector-demo.ais` — a `loop { body = pipeline.<p> condition =
  "{{ critic.accept }}" }` block running a draft→critique cycle
  with a max-iteration cap.  Wired by `patterns::reflector` (Fri).

## What lands in Week 18 Mon

- `planner_executor-demo.ais` — heuristic plan + execute leaf
  actions, slipped from Week 17 Fri at Mon close per the re-scheduled
  PLAN.
- `coordinator-demo.ais` — peer-to-peer round-robin agent
  coordination.
