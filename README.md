# aiSlang

> Declarative infrastructure for AI projects. One file describes your agents, models, prompts, MCP servers, evals, budgets, and deployment — `aislang apply` ships it.

**Status:** pre-v0.1 (scaffolding). See [PLAN-90-days-v0_1.md](PLAN-90-days-v0_1.md) for the roadmap and [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) for the language reference.

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

```sh
# Stand up the stack (LiteLLM + Jaeger + budget counter) in Docker.
aislang apply examples/support-agent.ais

# Chat with the agent — `aislang chat` runs the loop in-CLI:
# the agent talks to the Compose-managed LiteLLM and spawns the
# declared MCP servers (e.g. `mcp/filesystem:1.0.2`) as stdio
# children for the duration of the session.
aislang chat support examples/support-agent.ais

# Run the declared evals against the deployed agent and print a
# JSON report (per-case scores + trace URLs that deep-link into
# the bundled Jaeger UI at localhost:16686 for each case).
aislang eval examples/support-agent.ais

# When you're done, tear it all down (containers + volumes).
aislang destroy examples/support-agent.ais
```

### Browser chat via Open WebUI (v0.2 Tue, Track A2)

Drop `web_ui = true` into your `deploy { expose { … } }` block to bring up an [Open WebUI](https://docs.openwebui.com/) sidecar pointed at the in-stack LiteLLM:

```hcl
deploy "local" {
  target = compose
  expose {
    web_ui = true     # → http://localhost:3000 after `aislang apply`
  }
}
```

After `aislang apply`, browse to **http://localhost:3000**, sign up (local SQLite — first user is admin), and pick any model alias visible in the dropdown.  All models LiteLLM is configured to route appear there — useful for debugging "is this agent's chosen model actually replying?" without writing client code.

`aislang destroy` runs `docker compose down -v` which wipes the `openwebui-data` named volume; chat history + local accounts go away alongside the rest of the stack.

### MCP integration — drive aiSlang from Claude Desktop / Cursor (v0.2 Thu, Track A)

`aislang mcp-serve` exposes the whole CLI surface as an MCP server, so MCP-aware LLM hosts can author, deploy, and chat with .ais agents without you touching a terminal.

**Claude Desktop one-time setup** — add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or the equivalent on Linux/Windows:

```json
{
  "mcpServers": {
    "aislang": {
      "command": "aislang",
      "args": ["mcp-serve", "--stdio"]
    }
  }
}
```

Restart Claude Desktop. The aislang tools appear under the 🔌 icon. Try:

- *"Validate /path/to/my.ais"* → calls `aislang_validate`, shows source-aware diagnostics.
- *"Apply the support-agent stack at examples/support-agent.ais"* → calls `aislang_apply` (Claude Desktop renders the destructive-hint confirmation dialog before bringing up the Compose stack).
- *"Run a chat turn against the support agent — ask it: what's the return policy?"* → calls `aislang_chat_turn` → progress notifications (week 13 Wed) render as the tool-use iterations unfold → final reply lands as the tool's response.

This is the **principal-pattern demo**: Claude Desktop is the orchestrator (`pattern = orchestrator`), the deployed agent (`agent "support"`) is one of its tools, mcp-serve glues them. The `main` block in your .ais source declares this topology explicitly (week 12 Fri); `aislang status --main-spend` (week 13 Thu) surfaces the cost-split between the host LLM's tokens and the deployed agents' tokens.

For HTTP-MCP hosts (k8s deployments, ChatGPT-with-MCP, custom rmcp clients): `aislang mcp-serve --http 0.0.0.0:8080` mounts the same toolset at `http://host:8080/mcp` (week 13 Mon).

### Observability

Every `aislang chat` and `aislang eval` invocation emits OpenTelemetry traces over OTLP/HTTP to the bundled Jaeger sidecar (`localhost:16686`). The trace tree mirrors the chat loop's structure — each user turn becomes one `aislang.turn` span, the tool-use loop fans out into `tool_use_iteration[N]` spans, and each iteration carries one `chat <model>` LLM call + zero-or-more `mcp <server>.<tool>` dispatch spans. Attributes follow the [OTel-GenAI semantic conventions](https://opentelemetry.io/docs/specs/semconv/gen-ai/) (`gen_ai.system`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, …) plus our own `mcp.*` namespace for tool calls (sizes only, never argument content — PII-safe by construction).

After each command, the trace URL prints to stderr — paste it into a browser to see the full call hierarchy:

```
trace (eval `factual_accuracy`): http://localhost:16686/trace/7ca996386817d4859e6921c26245e4e3
```

`aislang eval`'s JSON report carries the same URLs as machine-readable fields so CI artifacts can link straight into Jaeger:

```sh
aislang eval examples/support-agent.ais | jq -r '.trace_url, .cases[].trace_url'
```

Override the Jaeger UI URL with `AISLANG_JAEGER_UI` (e.g. for a shared collector) or the OTLP endpoint with the standard `OTEL_EXPORTER_OTLP_ENDPOINT` env var. v0.1 uses 100 % head sampling — eval volumes are small enough that the debuggability win pays for itself.

## Commands

`aislang` exposes eight subcommands. Four are read-only / offline (`validate`, `plan`, `cost`, `status`); four shell out to `docker compose` and the running LiteLLM router (`apply`, `destroy`, `chat`, `eval`). Run `aislang <cmd> --help` for the full flag list — the table is a quick reference, the flowcharts below show each command's decision tree.

| Command | Synopsis | What it does |
|---|---|---|
| `validate` | `aislang validate [-v…] <file>` | Parse, type-check, and catalog-id-check the .ais source. No network, no writes. |
| `plan`     | `aislang plan [-v…] [--no-lock] [-o DIR] [--smoke-test] [--smoke-test-live] <file>` | Resolve models, reconcile `aislang.lock`, emit Compose YAML to stdout + `litellm_config.yaml` + `.env.example` to disk. Optional tiered smoke test. |
| `apply`    | `aislang apply <file>` | Run `plan` internals, write `stack.yml`, then `docker compose up -d` and record `.aislang/state.json`. |
| `destroy`  | `aislang destroy [--force] <file>` | `docker compose down -v` against the recorded stack. Idempotent — re-runs on a torn-down stack exit 0. |
| `chat`     | `aislang chat <agent> <file>` | Open an interactive REPL against the deployed LiteLLM, spawning declared MCP servers as stdio children for the session. Tool-use loop hard-capped at 8 iterations. |
| `eval`     | `aislang eval [-e <label>] <file>` | Run every `eval` block (or one named via `-e`) against the deployed agent. JSON `EvalReport` on stdout; per-case `trace_url`s deep-link into Jaeger. Exit 1 if any eval fails threshold. |
| `cost`     | `aislang cost <file>` | Print today's UTC spend (total + per-model) from `~/.aislang/spend.json` against the budget caps in the source. Read-only. |
| `status`   | `aislang status [--spend] [--model-spend[=MODEL]] <file>` | Render stack health (state file + `docker compose ps`), optionally with spend sections. Exit `0/1/2` for clean / drift / docker-unavailable. |

### Flowcharts

Per-command decision trees, in the tree style used elsewhere in the docs for `destroy`'s idempotency design.

#### `validate`

```
read source
└── aislang_core::validate                          (lex → parse → typecheck)
    └── aislang_resolver::check_against_bundled_catalog   (verify catalog IDs)
        ├── error  → render source-aware snippet → exit 1
        └── ok     → "OK: <file> parsed and type-checked"   (exit 0)
                     ├── -v    resource counts + stage timings (stderr)
                     ├── -vv   AST Debug + resource-graph Debug
                     └── -vvv  full AST + graph as JSON IR
```

#### `plan`

```
validate → catalog check → resolve → lockfile reconcile → emit
                                       ├── Match  → reuse cached bindings
                                       ├── Fresh  → write aislang.lock
                                       └── Stale  → rewrite (-v shows reasons)
                                                    (skipped under --no-lock)
   stdout: docker-compose YAML
   stderr: paths of <out>/litellm_config.yaml + .env.example + aislang.lock,
           warnings, optional verbose tail (-v stage timings;
           -vv ResolvedProject Debug; -vvv combined JSON IR)

   --smoke-test       → pipe YAML through `docker compose -f - config -q`
                        (docker missing → exit 0 "skipped")
   --smoke-test-live  → docker compose up -d
                        → poll :4000 readiness
                        → real /v1/chat/completions vs. first resolved model
                        → docker compose down -v   (ALWAYS, even on failure)
```

#### `apply`

```
plan internals  (validate → resolve → emit)
   └── write <source_dir>/stack.yml
       └── docker on PATH?
           ├── no   → exit 0, print manual "docker compose -f stack.yml up -d" hint
           └── yes  → docker compose -f stack.yml up -d
                       └── write .aislang/state.json
                              (applied_at = now, source_hash, catalog_version)
                           └── print "stack is up" + next-step `chat` / `destroy` hints
```

#### `destroy`

```
read .aislang/state.json
├── applied_at == None    → exit 0 "already torn down"      [no docker]
├── absent + no stack.yml → exit 0 "nothing to destroy"     [no docker]
├── absent + stack.yml present  (v0.0.x back-compat)  → proceed
├── applied_at == Some    → proceed
│       └── docker compose -f stack.yml down -v
│           └── mark_torn_down() → applied_at = None
└── --force               → skip everything above, just proceed
```

#### `chat`

```
validate → find agent  (Levenshtein "did you mean…?" on miss)
   └── resolve model + system_prompt
       └── verify prompt hash against aislang.lock  (when present)
           └── spawn MCP stdio children — one per agent.tools[mcp_server.<X>]
               └── list_tools per server → ToolRegistry
                   └── open Session → LiteLLM @ localhost:4000
                       └── stdin loop:
                           read line → turn → assistant reply
                                       └── tool-use loop, hard cap = 8 iters
                                                          (Decision 4)
                           /exit or EOF → shutdown MCP children, flush trace,
                                          print Jaeger URL on stderr
```

#### `eval`

```
validate → filter to requested eval(s)
              ├── -e <label>  → that one
              └── (no flag)   → every `eval` block
                  │
                  ▼ per eval (RootSpan = aislang.eval):
              resolve agent + prompt + budget + MCP servers
                  └── open Session
                      └── for each case in dataset (JSONL):
                          open `aislang.case` span → run turn
                              → score with metric (exact_match | f1)
                          aggregate (mean) vs. threshold
                              ├── ≥ → eval pass
                              └── < → eval fail
   stdout: EvalReport JSON  (cases[], trace_url, per-case trace_urls)
   stderr: per-case progress + PASS/FAIL summary
   exit:   0 iff every eval passed its threshold   (Decision 16)
```

#### `cost`

```
validate → open ~/.aislang/spend.json   (read-only)
   └── today's UTC total + by_model breakdown
       └── compare to source's `budget { hard_cap_per_run, hard_cap_per_day }`
           └── stdout: spend report + day-cap utilization %
                       (no network, no docker)
```

#### `status`

```
validate → read .aislang/state.json + check stack.yml
   ├── state missing            → "not applied"   (drift, exit 1)
   ├── applied_at == None       → "torn down"     (drift, exit 1)
   └── applied_at == Some
         └── docker on PATH?
             ├── no  → "(cannot query live status)"   (exit 2)
             └── yes → docker compose -f stack.yml ps --format json
                       ├── any service ≠ running     (drift, exit 1)
                       └── all running               (clean, exit 0)

   --spend                → append today's total + cap utilization %
   --model-spend[=MODEL]  → append today's by-model breakdown
                            (=MODEL filters to one; bare flag shows all)
```

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
| 3. **Emit** | `aislang plan` / `aislang apply` | Translate the resolved IR to standard target artifacts. v0.1 emits a `stack.yml` (Compose) with LiteLLM as the central router, Jaeger for OTel-GenAI traces, and a budget-enforcement sidecar, plus a `litellm_config.yaml` (mounted into the LiteLLM container) and a `.env.example` (union of provider-derived credentials + env vars referenced by `secret` blocks — see [Secrets handling](#secrets-handling) below). MCP servers are declared in the `.ais` but **spawned as `docker run -i` stdio children by `aislang chat`**, not as long-running Compose services (the published MCP images are stdio-only — see PLAN-90-days-v0_1.md Decision 5; v0.2 changes this) | `aislang-compose` (v0.1); later: `aislang-k8s`, `aislang-helm`, `aislang-databricks`, etc. |
| 4. **Apply / operate** | `aislang apply` / `chat` / `eval` / `cost` / `destroy` | Hand artifacts to Docker / K8s / target platform. The running stack honours the declared budgets, exposes the declared port surface, runs declared evals on demand, and emits OTel traces | shells out to platform CLIs (`docker compose`, `kubectl apply`, etc.) |

### What's in the v0.1 scope vs. phased

- **v0.1** — Docker Compose target only. Catalog bundled in the binary (JSON, immutable per release). Single-file projects. Static prompts with a fixed set of template variables. No LSP yet. See [PLAN-90-days-v0_1.md](PLAN-90-days-v0_1.md) for the 12-week schedule and definition of done.
- **Phase 1** — Kubernetes + Helm targets; LSP via JSON Schema export of the AST; remote catalog updates; multi-file projects via `include`.
- **Phase 2** — Managed cloud (`aislang.cloud`), compliance pack (EU AI Act / ISO 42001 evidence export), ArgoCD / Flux GitOps wrappers, Databricks / MS Fabric / Spark targets, dynamic prompt composition (decorators / guardrails) if user demand pulls for it.

All Phase 1+ items are additive — they don't change the v0.1 grammar or AST. See [LANGUAGE-SPEC-v0.md](LANGUAGE-SPEC-v0.md) §7 for the forward-compatibility invariants that make this possible.

### Secrets handling

aiSlang never reads a real `.env` itself — secrets are the user's responsibility, handed to `docker compose` at runtime. `aislang plan` only emits a `.env.example` template (safe to commit), populated from **two sources merged into one alphabetized list**:

1. **Provider-derived credentials.** For each resolved primary model, a per-provider mapping contributes the env vars LiteLLM expects (e.g. `ANTHROPIC_API_KEY` for an anthropic primary, `OLLAMA_API_BASE` for an ollama primary).
2. **User-declared `secret` blocks.** Each `secret "<label>" { source = "env://VAR" … }` block contributes `VAR` to the same list. Non-`env://` sources (`file://`, future `vault://`) appear in an informational footer comment — `aislang` doesn't load them, but they're documented in one place. The `scope = [...]` field controls which containers receive each secret at compose time.

The two sources are deduplicated: declaring `secret { source = "env://OLLAMA_API_BASE" }` for a project that already resolves to Ollama yields one line, not two. This dual flow is **a v0.1 compromise** — v0.2 unifies it by making `secret` blocks the canonical credential surface (see [LANGUAGE-SPEC-v0.md §5.6](LANGUAGE-SPEC-v0.md)). The forward-compat invariant is that any v0.1 source with no `secret` blocks continues to work in v0.2 against a provider-creds fallback.

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
