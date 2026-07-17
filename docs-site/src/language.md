# The aiSlang language

An aiSlang project is a single `.ais` file (or a directory of them) that
describes a whole multi-agent system: its models, prompts, tools, agents,
knowledge, evals, budgets, and deploy target. The syntax is HCL-like — labeled
blocks, `key = value` assignments, and nested blocks — and every value is
typed, so mistakes are caught at `aislang validate` rather than at runtime.

> This chapter is a practical guide. For the complete formal grammar and typed
> semantics, see the [aiSlang language reference (`LANGUAGE-SPEC-v0.md`)](https://github.com/aiSlang/cli/blob/main/LANGUAGE-SPEC-v0.md).

Typed literals use suffixes: money is `$50` / `$0.001`, sizes and token counts
are `32k` / `8GB`, durations are `5s` / `2m`, and percentages are `85%`.
Comments start with `#` or `//`.

This page walks through each construct with a minimal snippet. For the exact
grammar and type rules, the bundled language specification is authoritative
(`aislang mcp-serve` also exposes it as the `lang_spec` tool).

## `project` — the top-level container

Every file has exactly one `project` block. Its label is the project name and
is used as a prefix for generated service names, trace attributes, and the
local state directory. All other resources live inside it.

```hcl
project "support-agent" {
  # budget, model, prompt, agent, deploy … all go here
}
```

## `budget` — spend caps

A required, single `budget` block declares the hard spend caps and an
optimization hint. Caps are enforced at runtime: `hard_cap_per_run` is enforced
by the LiteLLM adapter on each turn, and `hard_cap_per_day` by a sidecar
counter that persists the day's spend. Use the `unlimited` keyword for free
local models.

```hcl
budget {
  hard_cap_per_day = $50
  hard_cap_per_run = $0.50
  optimize_for     = cost   # cost | latency | quality (informational hint)
}
```

## `model` — capability-based selection

A `model` block describes *what you need*, not which vendor. You declare
required capabilities and constraints; the resolver picks a concrete model from
the bundled catalog and builds a LiteLLM fallback chain.

```hcl
model "fast" {
  requires    = [chat, tool_use]           # capabilities the model must have
  context_min = 32k                        # minimum context window (tokens)
  cost_max    = $0.001 per 1k_tokens       # optional cost ceiling
  prefer      = [anthropic.claude_haiku_4_5, openai.gpt_4o_mini]
  fallback    = [ollama.llama_3_1_8b]
  pin         = anthropic.claude_haiku_4_5 # optional: must equal the resolved id
  source      = api                        # api (default) | subscription
}
```

- **`requires`** — a set of capabilities such as `chat`, `tool_use`, `vision`,
  `embedding`, `reasoning`, `structured_output`.
- **`context_min` / `cost_max`** — filters applied against the catalog; models
  that don't satisfy them are dropped from consideration.
- **`prefer`** — an *ordered* preference list. The resolver walks it in
  declared order and picks the first entry that satisfies the constraints.
- **`fallback`** — used only when no `prefer` candidate satisfies the
  constraints; also emitted as LiteLLM's fallback chain.
- **`pin`** — if set, the resolved model must equal this id, otherwise
  validation fails. Pins keep a deployment reproducible.
- **`source`** — `api` (default) routes the model through LiteLLM with hard
  budget enforcement; `subscription` marks a model whose inference runs on the
  host chat app's subscription tier (Claude Pro, Cursor Team, …). Subscription
  models are still resolved against the catalog, but they get no LiteLLM
  routing entry and their spend is reported rather than capped.

The chosen model and its fallback chain are written to `aislang.lock` so a plan
is deterministic given the same source, catalog, and lockfile.

## `prompt` — versioned, hashable prompts

A `prompt` block points at a markdown file and pins a version. The file
contents are hashed at validate time and the hash lands in the lockfile, so a
prompt edit is visible in the next `plan`.

```hcl
prompt "system" {
  file     = "./prompts/system.md"
  version  = "1.0.0"
  template = true    # optional: render as a MiniJinja template at chat time
}
```

With `template = true`, the file body is rendered as a
[MiniJinja](https://docs.rs/minijinja) template against a built-in context
`{ agent, prompt, model }` (all strings). HTML auto-escaping is on; opt out per
value with the `|safe` filter. Without it, the file is sent to the model
verbatim.

## `mcp_server` — tool servers

An `mcp_server` block declares an MCP tool server by container image. Secrets
are scoped: a server only receives the secrets it lists in its `secrets` field,
so credentials never leak across containers.

```hcl
mcp_server "docs" {
  image   = "mcp/filesystem:1.0.2"
  mount   = "./data"                 # optional bind mount
  env     = { ROOT = "/data" }       # plain env (never put secrets here)
  secrets = [secret.docs_token]      # only this server gets docs_token
}
```

## `secret` — scoped credentials

A `secret` block names a credential and its source, and lists which resources
may receive it. aiSlang never reads real secret values into the plan — it emits
a safe `.env.example` template and hands the actual values to the runtime.

```hcl
secret "docs_token" {
  source = "env://DOCS_TOKEN"        # env://VAR or file://path
  scope  = [mcp_server.docs]         # agents and/or mcp_servers
}
```

- **`env://VAR`** — read from a process environment variable at runtime.
- **`file://path`** — read the file contents as the secret value.
- **`scope`** — the set of agents and MCP servers allowed to see the secret.
  Referencing a secret from a resource outside its scope is a compile error.

## `agent` — a deployed agent

An `agent` binds a model to a system prompt and, optionally, a set of tools and
knowledge stores.

```hcl
agent "support" {
  model         = model.fast
  system_prompt = prompt.system
  tools         = [mcp_server.docs]         # optional MCP tool servers
  knowledge     = [vector_store.handbook]   # optional RAG stores
  skills        = [skill.list_files]        # optional named skills
}
```

## RAG — `embedding`, `vector_store`, and `agent.knowledge`

Retrieval-augmented generation is expressed with two blocks plus an
`agent.knowledge` reference. An `embedding` block names the embedding model; a
`vector_store` block declares the corpus to index; and an agent attaches one or
more stores through `knowledge`.

```hcl
embedding "small" {
  provider = "openai"                 # openai | cohere | ollama
  model    = "text-embedding-3-small"
}

vector_store "handbook" {
  embedding     = embedding.small
  source        = "git:acme/handbook#docs"   # path, URL, or git ref
  chunk_size    = 1k                          # 256..8192
  chunk_overlap = 128                         # optional, must be < chunk_size
}

agent "support" {
  model         = model.fast
  system_prompt = prompt.system
  knowledge     = [vector_store.handbook]
}
```

The `source` string is disambiguated by prefix:

- `"./docs/"` or `"/abs/path"` — a local file or directory.
- `"https://…"` — a single URL fetch (no recursion).
- `"crawl:https://…"` — a recursive crawl from the seed URL.
- `"git:owner/repo#subpath"` — a shallow clone of a GitHub repo; the optional
  `#subpath` narrows indexing to a subdirectory.

Build and inspect indexes with `aislang rag build` / `query` / `verify` (see
[CLI reference](./cli-reference.md)).

## `skill` — named, typed operations

A `skill` block declares an operation an agent can invoke, either by shelling
out (`exec`) or by delegating to a tool on an `mcp_server` (`mcp`). Exactly one
of the two modes is required. `output_type` declares how the result is parsed.

```hcl
# Shell-exec skill
skill "list_files" {
  exec        = "ls -1 ./data"
  output_type = list_string          # string | json | list_string
}

# MCP-delegate skill
skill "read_doc" {
  mcp         = mcp_server.docs.tool.read_file
  output_type = json
}
```

- **`exec`** — a shell command run in a sandboxed per-skill working directory
  with a scrubbed environment and a command denylist.
- **`mcp`** — a four-segment reference `mcp_server.<server>.tool.<tool>`. The
  server label is checked at validate time; the tool is resolved at runtime.
- **`output_type`** — `string` (verbatim), `json` (parsed), or `list_string`
  (split on newlines).

Run a skill end-to-end with `aislang skill run <label> <file>`.

## Composition — `pipeline`, `loop`, `branch`

Composition blocks describe how agents combine into a larger flow.

A **`pipeline`** runs agents in sequence; each step's output feeds the next.

```hcl
pipeline "draft_and_review" {
  steps  = [agent.drafter, agent.reviewer]
  input  = "customer question"     # optional descriptive metadata
  output = "reviewed answer"       # optional descriptive metadata
}
```

A **`loop`** re-runs a pipeline while a condition renders truthy, hard-capped by
`max_iter` (1..100) to guard against runaway loops.

```hcl
loop "refine" {
  body      = pipeline.draft_and_review
  condition = "{{ output.needs_more_work }}"   # MiniJinja
  max_iter  = 8
}
```

A **`branch`** dispatches to one of several pipelines based on a rendered
value, with a required `default`.

```hcl
branch "route" {
  on      = "{{ classifier_result }}"          # MiniJinja
  cases   = {
    "billing" = pipeline.billing_flow,
    "tech"    = pipeline.tech_flow,
  }
  default = pipeline.general_flow
}
```

### Built-in orchestration patterns

The user-facing LLM that drives your deployed agents is declared as a `main`
block. Its `pattern` field names the relationship between that entry point and
the deployed agents. The six patterns are:

| Pattern | The entry point… |
|---|---|
| `orchestrator` | plans the task, dispatches sub-tasks to agents in sequence, aggregates results. |
| `supervisor` | monitors agent outputs, can intervene, makes final approve/reject calls. |
| `router` | classifies input and routes to exactly one agent per turn. |
| `reflector` | critiques agent outputs and produces a refined final answer. |
| `planner_executor` | plans the steps and delegates execution to agents. |
| `coordinator` | facilitates communication between peer agents. |

```hcl
main "support_orchestrator" {
  model         = model.thinker
  chat_client   = claude_desktop      # claude_desktop | cursor | chatgpt | open_webui | custom
  pattern       = orchestrator
  system_prompt = prompt.system
}
```

A `main` block is never deployed itself — it describes the principal LLM's role
so an MCP host (Claude Desktop, Cursor, …) can act as the orchestrator with your
deployed agents as its tools. Pair it with `source = subscription` on its model
to run the orchestration on your existing subscription instead of the API.

## `eval` — datasets, metrics, thresholds

An `eval` block scores an agent against a JSONL dataset with a metric and a pass
threshold.

```hcl
eval "factual_accuracy" {
  dataset    = "./evals/faq.jsonl"    # JSONL of {input, expected}
  metric     = exact_match            # exact_match | f1
  threshold  = 85%
  aggregator = mean                   # mean | min | max | percentile_p95
  agent      = agent.support          # optional; inferred if unambiguous
}
```

Run with `aislang eval <file>`; the command exits non-zero if any eval falls
below its threshold.

## `deploy` — target and exposure

A `deploy` block chooses the emit target and how the stack is exposed.

```hcl
deploy "local" {
  target = compose                    # compose | kubernetes
  expose {
    port    = 8080                    # agent-runtime port (functional under kubernetes)
    web_ui  = true                    # Open WebUI sidecar at http://localhost:3000
    ingress = "support.example.com"   # kubernetes only; requires web_ui = true
  }
}
```

- **`target = compose`** emits a Docker Compose stack.
- **`target = kubernetes`** emits raw manifests plus a Helm chart.
- **`expose { … }`** — `port` binds the agent runtime, `web_ui` adds an Open
  WebUI browser-chat sidecar, and `ingress` (Kubernetes only) fronts that UI at
  a host name. See [Deploying stacks](./deploy.md).

## Multi-module projects

When one agent's resources should be reusable by another, split the project
across files. Put an `aislang.toml` at the project root with a `[modules]`
table, then reference sibling resources with `use <module>.<label>`.

```toml
# aislang.toml
[project]
name    = "support-stack"
version = "0.1.0"

[modules]
architect = "agents/architect.ais"
coder     = "agents/coder.ais"
```

Each `.ais` file still carries its own `project` block. A module's resources
live in the `<module-name>.` namespace, so a `model "thinker"` declared in
`architect.ais` is referenced elsewhere as `use architect.thinker`:

```hcl
# agents/coder.ais
project "coder-module" {
  budget { hard_cap_per_day = $1  hard_cap_per_run = $0.01 }
  prompt "system" { file = "./system.md"  version = "1.0.0" }
  agent "coder" {
    model         = use architect.thinker   # cross-module reference
    system_prompt = prompt.system           # same-module reference
    tools         = []
  }
}
```

The commands that take a positional `<file.ais>` (`validate`, `plan`, `apply`,
`destroy`) also accept a project directory holding an `aislang.toml` — the CLI
autodetects which shape it was handed.

---

Next: the [CLI reference](./cli-reference.md) for every command and flag, and
[Deploying stacks](./deploy.md) for Compose and Kubernetes targets.
