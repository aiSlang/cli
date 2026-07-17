# aiSlang

**aiSlang is a declarative language and CLI for building multi-agent AI systems.** You describe the whole system — agents, capability-selected models with fallback and spend caps, prompts, MCP tools, RAG knowledge, evals, and how the agents compose — in one `.ais` file. The CLI type-checks it, resolves models against a bundled catalog, runs it locally, and emits the Docker Compose or Kubernetes stack (a LiteLLM router + Jaeger tracing + budget enforcement + your agents) that serves it.

> This repo is the **local + self-hosted `aislang` CLI**. It can also delegate operations to a managed control plane via `aislang remote --target …`; that control plane lives in a separate repo, [aiSlang/cloud](https://github.com/aiSlang/cloud).

## Why aiSlang

Agent frameworks make it easy to demo *one* agent. Real systems compose many — a router picks a specialist, an orchestrator delegates, a reflector critiques and retries — and they need model fallback, spend caps, distributed tracing, scoped secrets, and a real way to serve the agents to users. Today you close that gap two ways: **imperative Python glue** (LangChain, LlamaIndex, CrewAI, AutoGen) — great libraries for agent *behavior*, but you hand-write and maintain the orchestration, there's no declarative description of the infrastructure, and turning a notebook into a deployed service is do-it-yourself; or **hand-rolled infrastructure** — Kubernetes/Compose manifests plus a LiteLLM router plus OpenTelemetry plus budget code, weeks of YAML that drifts the moment your agents change. aiSlang is not another agent framework and doesn't compete with those tools — it's the **declarative infrastructure layer beneath them** that turns a *described* multi-agent system into a running, observable, budget-enforced stack on your own machine or cluster.

→ Read [Why aiSlang](docs-site/src/why.md) for the full rationale.

## Demo

<!-- TODO: replace this placeholder with a demo gif / short screencast + an architecture diagram of the emitted stack. -->
> 🎬 **Demo coming soon** — a short screencast of `validate → plan → apply → chat`, plus an architecture diagram of the stack `aislang apply` emits. *(placeholder)*

## Quickstart

Install (detects OS + arch, verifies the SHA-256 checksum, installs the `aislang` binary to `~/.local/bin`):

```sh
curl -sSfL https://github.com/aiSlang/cli/releases/latest/download/install.sh | sh
```

The [full quickstart](docs-site/src/quickstart.md) gives you a ready-to-copy `support-agent.ais`. With that file in place, the loop is:

```sh
aislang validate support-agent.ais            # parse + type-check + catalog-check (no network, no writes)
aislang plan     support-agent.ais            # resolve models, emit the Compose stack + .env.example
aislang apply    support-agent.ais            # stand up LiteLLM + Jaeger + budget counter in Docker
aislang chat     support support-agent.ais    # interactive REPL against the deployed agent
aislang eval     support-agent.ais            # run declared evals → JSON report (exit 1 on a miss)
aislang destroy  support-agent.ais            # stop + remove containers and volumes (idempotent)
```

Provider API keys come from a local `.env` — aiSlang never reads real secrets, it only emits a safe-to-commit `.env.example`. Docker is the only hard prerequisite; add `kubectl` / `helm` / `kind` for the Kubernetes target.

→ [Install](docs-site/src/install.md) · [Quickstart](docs-site/src/quickstart.md) · [Deploying stacks](docs-site/src/deploy.md)

## Who it's for

aiSlang is for engineers who have moved past a single-agent demo and need to **run and operate a multi-agent system** without hand-building the plumbing:

- **AI / platform engineers** shipping multi-agent features who want model fallback, spend caps, tracing, and a reproducible deploy from one file.
- **Teams self-hosting on their own cluster** — emit a Helm chart, `helm install` it, and keep your own provider keys.
- **Local-first / privacy-conscious builders** — run the same file entirely against local models (Ollama), no external API required.
- **MCP host users** (Claude Desktop, Cursor) — expose the whole CLI as MCP tools and drive stacks straight from your assistant.

Typical use cases: a support agent grounded in a RAG knowledge base; an orchestrator chaining an architect → coder → reviewer pipeline; a router that dispatches requests by intent; graded evals wired into CI. See the [language guide](docs-site/src/language.md) and the runnable [`examples/`](examples/).

## Documentation

The full docs are the single source of truth — they live in **[`docs-site/`](docs-site/src/SUMMARY.md)** (an mdBook; run `mdbook serve docs-site` to read them locally):

| | | |
|---|---|---|
| [Introduction](docs-site/src/introduction.md) | [Why aiSlang](docs-site/src/why.md) | [Install](docs-site/src/install.md) |
| [Quickstart](docs-site/src/quickstart.md) | [The aiSlang language](docs-site/src/language.md) | [CLI reference](docs-site/src/cli-reference.md) |
| [Deploying stacks](docs-site/src/deploy.md) | [MCP integration](docs-site/src/mcp.md) | [Observability](docs-site/src/observability.md) |

The language reference is [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md); the architectural decisions behind the design are collected in [docs/ARCH-DECISIONS.md](docs/ARCH-DECISIONS.md).

## Next steps

- ⭐ **Star the repo** if you find it useful — it helps other people discover the project.
- 💬 **Join the community** on [Discord](https://discord.gg/aislang) <!-- TODO: replace with the real invite link --> to ask questions and share what you build.
- 📖 **Read the full docs** in [`docs-site/`](docs-site/src/SUMMARY.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
