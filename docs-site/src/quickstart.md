# Quickstart

The fastest way to see what aiSlang does is the flagship example,
[**`portfolio-pnl`**](https://github.com/aiSlang/cli/tree/main/examples/portfolio-pnl) —
a local-first, **multi-agent portfolio analyst**. It shows the whole shape of an
aiSlang system: deterministic **compute skills** do the math, a rule engine makes
a reproducible decision, and two RAG-grounded **agents** interpret it — all
declared in one `.ais` file.

> Want the bare-minimum single-agent loop instead? Jump to
> [the classic loop](#the-classic-single-agent-loop) with the self-contained
> [`support-agent.ais`](https://github.com/aiSlang/cli/blob/main/examples/support-agent.ais).

## 0. Install and clone

```sh
curl -sSfL https://github.com/aiSlang/cli/releases/latest/download/install.sh | sh
git clone https://github.com/aiSlang/cli
cd cli/examples/portfolio-pnl
```

## 1. Read the `.ais` file

[`portfolio.ais`](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl/portfolio.ais)
declares the whole system. The deterministic stages are **skills** (plain Python
the model never touches); the judgement stages are **agents** grounded in a Taleb
anti-fragility knowledge corpus:

```hcl
project "portfolio-pnl" {
  budget { hard_cap_per_day = $2  hard_cap_per_run = $0.20 }

  model "reasoner" {
    requires    = [chat, reasoning]
    context_min = 32k
    prefer      = [anthropic.claude_haiku_4_5]
    fallback    = [ollama.qwen3_8b]
  }

  embedding   "local_bge"   { provider = ollama  model = "bge-m3:latest" }
  vector_store "strategy_kb" { embedding = embedding.local_bge  source = "./knowledge/" }

  prompt "strategic_sys"    { file = "./prompts/strategic.md"    version = "1.0.0" }
  prompt "orchestrator_sys" { file = "./prompts/orchestrator.md" version = "1.0.0" }

  # ── deterministic pipeline stages = skills (no LLM) ──
  skill "fetch_prices" { exec = "python3 ../../../scripts/fetch.py"            output_type = json }
  skill "analyze"      { exec = "python3 ../../../scripts/analyze.py --persist" output_type = json }
  skill "visualize"    { exec = "python3 ../../../scripts/render_dashboard.py"  output_type = string }

  # ── judgement stages = LLM agents (grounded by the RAG corpus) ──
  agent "strategic" {
    model = model.reasoner  system_prompt = prompt.strategic_sys
    tools = []  knowledge = [vector_store.strategy_kb]
  }
  agent "orchestrator" {
    model = model.reasoner  system_prompt = prompt.orchestrator_sys
    tools = []  knowledge = [vector_store.strategy_kb]
  }

  pipeline "advise" { steps = [agent.strategic, agent.orchestrator] }

  deploy "local" { target = compose }
}
```

Type-check it (no network, no writes):

```sh
aislang validate portfolio.ais
```

## 2. Run the deterministic core

The compute stages need nothing but `python3`. Seed offline synthetic prices (or
run the `fetch_prices` skill for live Yahoo/CoinGecko data), then compute and render:

```sh
python3 scripts/seed_demo.py                      # offline synthetic prices (deterministic)
# or, for live prices:  aislang skill run fetch_prices portfolio.ais

aislang skill run analyze   portfolio.ais         # P&L, risk, anti-fragility + the keep/rebalance
                                                  # recommendation → local SQLite (generated/portfolio.db)
aislang skill run visualize portfolio.ais         # → generated/dashboard.html
```

**The keep/rebalance decision is deterministic** — a rule engine
(`portfolio.recommend()`) turns the metrics into a verdict and sized actions, so
identical inputs always yield the identical recommendation. The language model
never makes the call; it only *explains* it (step 4).

## 3. Open the dashboard

`generated/dashboard.html` is a single self-contained file — open it in a browser
for the holistic view: the recommendation, P&L overview, positions, per-instrument
price charts, and a full risk / anti-fragility panel (convexity, barbell
allocation, correlation heatmap, drawdown, tail stats).

[![portfolio-pnl dashboard](./images/portfolio-dashboard.png)](https://github.com/aiSlang/cli/blob/main/examples/portfolio-pnl/sample-dashboard.html)

## 4. Ask the advisory agents

The two-agent pipeline explains the fixed recommendation in prose (Strategic
recaps the anti-fragility posture; Orchestrator writes the owner-facing
explanation as JSON). This stage routes model calls through a local LiteLLM
router, so it needs the stack up:

```sh
./run.sh                          # fetch → analyze → dashboard → apply → advise, end to end
# or drive the pipeline directly once the stack is up:
aislang chat advise portfolio.ais
```

`run.sh` calls `aislang apply` to bring up the **LiteLLM router**, a **Jaeger**
tracing sidecar, and a **budget counter** that enforces the caps in the file. It
needs **Docker** and a chat model — set `ANTHROPIC_API_KEY` in a local `.env`, or
run Ollama with `qwen3_8b` for a $0 local run. (Ollama serving `bge-m3` is
optional; without it the agents still run, just ungrounded.) aiSlang never reads
real secrets itself — `plan` emits a safe-to-commit `.env.example`.

## 5. Eval and tear down

```sh
aislang eval    portfolio.ais     # run the declared evals → JSON report (exit 1 on a miss)
aislang destroy portfolio.ais     # stop + remove containers and volumes (idempotent)
```

## The classic single-agent loop

For the minimal `validate → plan → apply → chat → eval → destroy` loop against a
single self-contained file, use
[`support-agent.ais`](https://github.com/aiSlang/cli/blob/main/examples/support-agent.ais):

```sh
aislang validate support-agent.ais            # parse + type-check + catalog-check
aislang plan     support-agent.ais            # resolve models, emit the Compose stack + .env.example
aislang apply    support-agent.ais            # stand up LiteLLM + Jaeger + budget counter in Docker
aislang chat     support support-agent.ais    # interactive REPL against the deployed agent
aislang eval     support-agent.ais            # run declared evals → JSON report (exit 1 on a miss)
aislang destroy  support-agent.ais            # stop + remove containers and volumes (idempotent)
```

## Next steps

- [Agents catalog](./agents.md) — every runnable example at a glance.
- [Language](./language.md) — the full `.ais` language reference.
- [CLI reference](./cli-reference.md) — every subcommand and its flags.
