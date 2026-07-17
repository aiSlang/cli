# Quickstart

This is the five-minute path from a `.ais` file to a running, chattable agent stack.

## 1. Write a `.ais` file

Save this as `support-agent.ais`. It declares one model (selected by capability, with a local Ollama fallback), a versioned prompt, an MCP filesystem tool over a knowledge directory, one agent, an eval, and a local Compose deploy target:

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
    latency_p95 = 2s
    residency   = [eu, us]
    prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
    fallback    = [ollama.llama_3_1_8b]
  }

  prompt "system" {
    file    = "./prompts/system.md"
    version = "1.0.0"
  }

  mcp_server "docs" {
    image = "docker.io/mcp/filesystem:1.0.2"
    mount = "./knowledge/"
  }

  agent "support" {
    model         = model.fast
    system_prompt = prompt.system
    tools         = [mcp_server.docs]
  }

  eval "factual_accuracy" {
    dataset   = "./evals/qa.jsonl"
    metric    = f1
    threshold = 50%
  }

  deploy "local" {
    target = compose
    expose {
      port   = 8080
      web_ui = true
    }
  }
}
```

The `fast` model resolves to `anthropic.claude_haiku_4_5` (the first `prefer` entry that satisfies every constraint), so set `ANTHROPIC_API_KEY` in a local `.env` before you apply. For $0 local testing, put `ollama.llama_3_1_8b` first in `prefer` instead.

## 2. Validate, plan, apply

```sh
# Parse, type-check, and catalog-check the source. No network, no writes.
aislang validate support-agent.ais

# Resolve models against the catalog, reconcile the lockfile, and print
# the Compose stack it would deploy (plus litellm_config.yaml + .env.example).
aislang plan support-agent.ais

# Stand up the stack and record its state.
aislang apply support-agent.ais
```

`aislang apply` brings up a Docker Compose stack: a **LiteLLM router** (the single endpoint all agents route model calls through), a **Jaeger** sidecar for tracing, and a **budget counter** that enforces your caps fail-closed. Provider API keys are never read by aiSlang itself — they come from a local `.env` that Docker Compose passes to the containers at runtime; `plan` emits a safe-to-commit `.env.example` template listing exactly which variables you need.

## 3. Chat and eval

```sh
# Open an interactive REPL against the deployed agent. The CLI spawns the
# declared MCP servers as stdio children for the duration of the session.
aislang chat support support-agent.ais

# Run the declared evals against the deployed agent and print a JSON report
# with per-case scores and Jaeger trace URLs. Exit 1 if any eval misses its threshold.
aislang eval support-agent.ais
```

## 4. Tear it down

```sh
# Stop and remove the containers and volumes. Idempotent — safe to re-run.
aislang destroy support-agent.ais
```

## Next steps

- [Language](./language.md) — the full `.ais` language reference.
- [CLI reference](./cli-reference.md) — every subcommand and its flags.
