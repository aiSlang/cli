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
