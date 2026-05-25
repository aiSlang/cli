# aiSlang

> Declarative infrastructure for AI projects. One file describes your agents, models, prompts, MCP servers, evals, budgets, and deployment — `aislang apply` ships it.

**Status:** pre-v0.1 (scaffolding). See [PLAN-90-days.md](PLAN-90-days.md) for the roadmap and [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) for the language reference.

## Quick taste

```hcl
project "support-agent" {
  budget {
    hard_cap_per_day = $50
    hard_cap_per_run = $0.50
    optimize_for     = cost
  }

  model "fast" {
    requires    = [chat, tool_use]
    context_min = 32k
    cost_max    = $0.001 per 1k_tokens
    prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
    fallback    = [ollama.llama_3_1_8b]
  }

  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = [mcp_server.docs]
  }

  # ...
}
```

See [examples/support-agent.ais](examples/support-agent.ais) for the full reference example.

## How it works — the pipeline

aiSlang is a *compile-then-deploy* tool, not an interpreter. A single `.ais` source file describes the whole AI infrastructure — models, agents, prompts, MCP servers, evals, budgets, deploy target. The CLI compiles it through four stages, then hands the resulting standard-format artifacts to your platform of choice.

```
        ┌──────────────────┐
        │   .ais source    │   one file = one project
        └────────┬─────────┘
                 │
                 ▼   parse  +  type-check  (validate)
        ┌──────────────────┐
        │   Project AST    │   typed tree (JSON-serializable —
        │   (typed tree)   │    see examples/*.expected-ast.json)
        └────────┬─────────┘
                 │
                 ▼   resolve  (plan)
   ┌─────────────┴─────────────┐         consult ──┐
   │                           │                   ▼
   │   • model selector picks one ModelId from the catalog
   │     per `model` block, subject to declared constraints
   │   • prompt files hashed; OCI image tags pinned to digests
   │   • result written/read from aislang.lock for reproducibility
   │                                              ┌──────────────┐
   └──────────────┬───────────────────────────────│ model catalog│
                  │                               │ + aislang.lock│
                  ▼                               └──────────────┘
        ┌──────────────────┐
        │   Resolved AST   │   the actual IR: every reference now
        │   (the real IR)  │   points to a concrete identity
        └────────┬─────────┘
                 │
                 ▼   emit  (plan output / apply input)
        ┌──────────────────┐
        │ Target artifacts │   v0.1: docker-compose.yml +
        │ (standard YAML / │          LiteLLM config + MCP
        │  manifests /     │          server configs + Jaeger +
        │  charts)         │          budget-enforcement sidecar
        └────────┬─────────┘   v0.2+: K8s manifests, Helm charts,
                 │             ECS task defs, ArgoCD applications,
                 │             Databricks bundles, MS Fabric, Spark
                 ▼   apply
        ┌──────────────────┐
        │  Running stack   │   agents addressable, OTel-GenAI traces
        │  on the chosen   │   emitted, evals runnable, cost caps
        │  platform        │   enforced fail-closed
        └──────────────────┘
```

### Stage-by-stage

| Stage | CLI command | What happens | Where it lives |
|---|---|---|---|
| 1. **Parse + type-check** | `aislang validate` | Lex source, build AST per LANGUAGE-SPEC §8 grammar, validate types and resource-shape semantics from §8.1 | `aislang-core::parser`, `aislang-core::ast` |
| 2. **Resolve** | `aislang plan` | For each `model` block: walk constraints (`requires`, `cost_max`, `latency_p95`, `residency`, `prefer`, `fallback`) against the bundled model catalog; pick the best satisfying entry. Hash prompt files. Pin image tags to digests. Reconcile with `aislang.lock`. Output the resolved IR | `aislang-core::resolver` (week 4) + `aislang-catalog` |
| 3. **Emit** | `aislang plan` / `aislang apply` | Translate the resolved IR to standard target artifacts. v0.1 emits a single `docker-compose.yml` with the agent runtime, LiteLLM as a sidecar, MCP servers, Jaeger for OTel-GenAI traces, and a budget-enforcement sidecar | `aislang-compose` (v0.1); later: `aislang-k8s`, `aislang-helm`, `aislang-databricks`, etc. |
| 4. **Apply / operate** | `aislang apply` / `chat` / `eval` / `cost` / `destroy` | Hand artifacts to Docker / K8s / target platform. The running stack honours the declared budgets, exposes the declared port surface, runs declared evals on demand, and emits OTel traces | shells out to platform CLIs (`docker compose`, `kubectl apply`, etc.) |

### What's in the v0.1 scope vs. phased

- **v0.1** — Docker Compose target only. Catalog bundled in the binary (JSON, immutable per release). Single-file projects. Static prompts with a fixed set of template variables. No LSP yet. See [PLAN-90-days.md](PLAN-90-days.md) for the 12-week schedule and definition of done.
- **Phase 1** — Kubernetes + Helm targets; LSP via JSON Schema export of the AST; remote catalog updates; multi-file projects via `include`.
- **Phase 2** — Managed cloud (`aislang.cloud`), compliance pack (EU AI Act / ISO 42001 evidence export), ArgoCD / Flux GitOps wrappers, Databricks / MS Fabric / Spark targets, dynamic prompt composition (decorators / guardrails) if user demand pulls for it.

All Phase 1+ items are additive — they don't change the v0.1 grammar or AST. See [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) §7 for the forward-compatibility invariants that make this possible.

## Build

```sh
cargo build --release
./target/release/aislang --help
```

## Layout

```
crates/
  aislang-cli      — `aislang` binary (clap subcommands)
  aislang-core     — parser, AST, types, type-checker, resolver
  aislang-catalog  — model catalog (bundled JSON + Rust types)
  aislang-compose  — Docker Compose emitter
examples/
  support-agent.ais — the v0.1 demo fixture
```

## License

Apache-2.0.
