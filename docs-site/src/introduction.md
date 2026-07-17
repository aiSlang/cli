# Introduction

aiSlang is a declarative language (`.ais`) plus a single CLI (`aislang`) for building multi-agent AI systems. One file describes the whole system, and one CLI validates it, compiles it, and runs it — locally on your own machine or as a stack you deploy to your own cluster. The full `.ais` grammar is defined in the [language reference (`LANGUAGE-SPEC-v0.md`)](https://github.com/aiSlang/cli/blob/main/LANGUAGE-SPEC-v0.md).

Agent frameworks make it easy to demo *one* agent. Real systems compose many — a `router` picks a specialist, an `orchestrator` delegates, a `reflector` critiques and retries — and need model fallback, spend caps, tracing, secret scoping, and a real way to serve them. aiSlang makes **the system** the unit: you declare the agents and how they combine, then `aislang apply` ships it.

## One file describes the whole system

A `.ais` project captures everything about a multi-agent system in one place:

- **Agents** and how they compose — routing, orchestration, reflection, and other patterns.
- **Models** by capability, not vendor lock-in: declare what a model must do (`requires = [chat, tool_use]`, `context_min`, `cost_max`, latency, residency) and let the resolver pick a concrete model from a bundled catalog, with `fallback` chains and per-run / per-day **budgets**.
- **Prompts**, versioned and hashed, with template variables.
- **MCP tools** the agents can call.
- **RAG knowledge** — vector stores and embeddings attached to an agent.
- **Evals** — datasets, metrics, and pass thresholds.
- **Deploy targets** — Docker Compose or Kubernetes.

## What the CLI does

The `aislang` CLI is a *compile-then-deploy* tool, not an interpreter:

- **Validates and type-checks** the `.ais` source, with source-aware diagnostics.
- **Resolves models** against a bundled catalog and pins the choices in a lockfile for reproducibility.
- **Emits a stack** — a Docker Compose or Kubernetes deployment made of a LiteLLM router, Jaeger tracing, budget enforcement, and your agents.
- **Runs chat and evals locally** against the deployed stack.
- **Exposes its whole surface as an MCP server**, so MCP-aware hosts like Claude Desktop or Cursor can author, deploy, and chat with your agents.

`aislang remote --target` can also talk to **aiSlang Cloud**, a managed control plane; these docs cover the local, self-hosted CLI only.

## What these docs cover

- [Why aiSlang](./why.md) — the problem it solves and where it sits relative to existing tools.
- [Install](./install.md) — get the `aislang` binary and its prerequisites.
- [Quickstart](./quickstart.md) — a running multi-agent stack in five minutes.
- [Language](./language.md) — the `.ais` language reference.
- [CLI reference](./cli-reference.md) — every subcommand and flag.
- [Deploy](./deploy.md) — Docker Compose and Kubernetes targets.
- [MCP](./mcp.md) — drive aiSlang from Claude Desktop, Cursor, and other MCP hosts.
- [Observability](./observability.md) — OpenTelemetry traces, Jaeger, and external collectors.
