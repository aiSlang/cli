# aiSlang Language Specification — v0 (frozen for v0.1 MVP)

Status: **Authoritative for the v0.1 compiler.** Any change to this document after week 1 of the 90-day plan slips the schedule.

This spec defines exactly enough of the language to make `examples/support-agent.ais` (in [PLAN-90-days.md](PLAN-90-days.md)) compile, type-check, plan, deploy, and run. Anything not described here is out of scope for v0.1.

---

## 1. Overview

aiSlang is a typed declarative language for describing an AI project — its agents, models, prompts, MCP servers, evals, budgets, secrets, and deployment target — in a single versioned file.

Design principles, in order of precedence when they conflict:

1. **Type safety over expressiveness.** Every literal has a type. Type mismatches are caught at `validate`, never at runtime.
2. **Intent over implementation.** Users describe what they need (capabilities, constraints); the planner picks how (which provider, which model, which fallback).
3. **Reproducibility.** Plans are deterministic given the same source + lockfile + catalog.
4. **Standards-only output.** The compiled artifact is plain Docker Compose YAML. No proprietary runtime.
5. **Single file by default.** A project compiles from one `.ais` file. Multi-file is Phase 1.

Syntax is HCL-like: blocks with labels, `key = value` assignments, nested blocks.

---

## 2. Lexical structure

- **Encoding.** UTF-8 only.
- **Comments.** `#` to end of line. Block comments deferred.
- **Identifiers.** `[a-z][a-z0-9_]*` (snake_case). Resource labels follow the same rule.
- **Whitespace** is insignificant except as a token separator.
- **Reserved words** (v0): `project`, `budget`, `model`, `prompt`, `mcp_server`, `agent`, `eval`, `secret`, `deploy`, `true`, `false`, `null`, `per`.
- **Type literals** use suffixes (`32k`, `$50`, `2s`, `85%`) — see §3.

---

## 3. Type system

Every value has a static type. The type checker rejects mismatches at `aislang validate`.

### 3.1 Primitives

| Type | Literal | Notes |
|---|---|---|
| `String` | `"hello"` | UTF-8, double-quoted; `\n`, `\t`, `\"`, `\\` escapes |
| `Int` | `42`, `-7`, `0` | `i64` |
| `Float` | `0.85`, `-1.5e3` | `f64` |
| `Bool` | `true`, `false` | |
| `Identifier` | `claude_haiku_4_5` | Used inside enums and catalog references; never bare in user data |

### 3.2 Domain types

These are the types that move the needle. Each has a literal syntax and a normalized internal form. Mixing a `String` where a domain type is expected is a compile error.

| Type | Literal examples | Internal form |
|---|---|---|
| `Money` | `$50`, `$0.001`, `€100` | `(currency: enum, amount: decimal)` |
| `Duration` | `5s`, `2m`, `1h`, `1d`, `500ms` | `nanoseconds: i64` |
| `Size` | `32k`, `1MB`, `8GB`, `1024` | `bytes: u64` (k=1024, M=1024², G=1024³); `k` also accepted for token counts |
| `Percentage` | `85%`, `0.5%` | `ratio: f64` ∈ [0.0, 1.0] |
| `SemVer` | `1.0.0`, `^2.1`, `~3.0.5` | parsed semver range |
| `Path` | `"./prompts/system.md"` | resolved relative to the `.ais` file at validate time; existence checked |
| `OciImage` | `ghcr.io/foo/bar:1.2.3`, `ghcr.io/foo/bar:1.2.3@sha256:…` | parsed registry/repo/tag/digest; digest preferred |
| `URI` | `"https://…"`, `"s3://…"` | scheme-aware parsed URL |

**Money** and **Duration** support unit composition for rates: `$0.001 per 1k_tokens`, `5 per minute`. The `per` keyword constructs a `Rate<Num, Denom>`.

### 3.3 Composite types

- `List<T>` — `[a, b, c]`
- `Map<K, V>` — `{ port = 8080, host = "localhost" }`
- `Set<T>` — `[a, b, c]` where order is irrelevant (used for `requires`, `residency`)
- `T?` — optional; absence is `null`
- `Ref<R>` — typed reference to a resource of type `R` (e.g., `Ref<Model>` is the type of `model.fast`)

### 3.4 Enums (closed sets, v0 final)

Enum members are bare identifiers in expression position, looked up in the enum type expected by context.

```
Capability     = chat | embedding | tool_use | vision | audio | reasoning | large_context | structured_output
Residency      = us | eu | uk | apac
DeployTarget   = compose                                   # v0.1 — k8s/helm/ecs are Phase 1
Currency       = usd | eur | gbp | jpy                     # implied by Money literal symbol
OptimizeFor    = cost | latency | quality
EvalMetric     = exact_match | f1 | bleu | rouge_l | llm_judge
EvalAggregator = mean | min | max | percentile_p95
```

The catalog (§5) defines two additional pseudo-enums populated at compile time from the catalog file:

```
ProviderId  = anthropic | openai | google | ollama | …       # one per catalog entry
ModelId     = anthropic.claude_haiku_4_5 | openai.gpt_4o_mini | …
```

### 3.5 Type coercion

There is no implicit coercion between domain types. `"$50"` (String) is not assignable to a `Money` field. The error message must show both the actual and expected type, and a suggested literal form.

---

## 4. Resource types

Nine resource types in v0.1. Each is a labeled block. Labels are unique within their type.

### 4.1 `project` (singleton, required)

```
project "<identifier>" {
  # contains all other resource blocks
}
```

The label is the project name. Used as a prefix for Compose service names, OTel resource attributes, and the state directory.

### 4.2 `budget` (singleton, required)

```
budget {
  hard_cap_per_day : Money
  hard_cap_per_run : Money
  optimize_for     : OptimizeFor = cost
}
```

`hard_cap_per_run` is enforced by the LiteLLM adapter; `hard_cap_per_day` is enforced by a sidecar counter persisted in `.aislang/state.json`.

### 4.3 `model`

```
model "<identifier>" {
  requires    : Set<Capability>
  context_min : Size                     # in tokens
  cost_max    : Rate<Money, Size>?       # optional; e.g. $0.001 per 1k_tokens
  latency_p95 : Duration?
  residency   : Set<Residency>?

  prefer      : List<ModelId>            # ordered preference
  fallback    : List<ModelId>            # used if no `prefer` candidate satisfies constraints
  pin         : ModelId?                 # if set, must equal the resolved model
}
```

The compiler resolves `requires + constraints + prefer/fallback` against the catalog; see §6.

### 4.4 `prompt`

```
prompt "<identifier>" {
  file    : Path
  version : SemVer
}
```

File contents are hashed at validate time; hash is included in the lockfile so a prompt edit is visible in `plan`.

### 4.5 `mcp_server`

```
mcp_server "<identifier>" {
  image   : OciImage
  mount   : Path?                # bind-mounted into the container
  env     : Map<String, String>? # plain env (no secrets — use secret refs)
  secrets : List<Ref<Secret>>?
}
```

### 4.6 `agent`

```
agent "<identifier>" {
  model         : Ref<Model>
  system_prompt : Ref<Prompt>
  tools         : List<Ref<McpServer>>
}
```

### 4.7 `eval`

```
eval "<identifier>" {
  dataset    : Path                 # JSONL: {input, expected}
  metric     : EvalMetric
  threshold  : Percentage
  aggregator : EvalAggregator = mean
  agent      : Ref<Agent>?          # defaults to the project's single agent if unambiguous
}
```

### 4.8 `secret`

```
secret "<identifier>" {
  source : URI                       # e.g. "env://OPENAI_API_KEY" or "file://./.secrets/foo"
  scope  : Set<Ref<Agent> | Ref<McpServer>>
}
```

Secrets are written into the container's environment only for the listed resources. Cross-resource leakage is a compile error.

### 4.9 `deploy`

```
deploy "<identifier>" {
  target : DeployTarget              # v0.1: must be `compose`
  expose : Map<String, Int>?         # port mappings
}
```

---

## 5. Model catalog

The catalog is a JSON file bundled in the `aislang` binary at v0.1. It defines the universe of `ModelId` values and their properties.

### 5.1 Schema

```json
{
  "version": "2026-05-22",
  "models": [
    {
      "id": "anthropic.claude_haiku_4_5",
      "provider": "anthropic",
      "litellm_name": "anthropic/claude-haiku-4-5",
      "capabilities": ["chat", "tool_use", "structured_output"],
      "context_max": "200k",
      "cost_input_per_1k_tokens": "$0.0008",
      "cost_output_per_1k_tokens": "$0.004",
      "latency_p95": "1.5s",
      "residency": ["us"],
      "deprecated": false
    }
  ]
}
```

### 5.2 v0.1 contents

Ship with at least these ten entries (placeholder list; freeze actual values during Tue–Wed of week 1):

- `anthropic.claude_opus_4_7`
- `anthropic.claude_sonnet_4_6`
- `anthropic.claude_haiku_4_5`
- `openai.gpt_4o`
- `openai.gpt_4o_mini`
- `openai.o1_mini`
- `google.gemini_2_0_pro`
- `google.gemini_2_0_flash`
- `ollama.llama_3_1_8b`
- `ollama.llama_3_1_70b`

### 5.3 Updates

In v0.1 the catalog is binary-bundled and immutable until the next `aislang` release. Remote catalog (`aislang catalog update`) is Phase 1.

---

## 6. Model resolution algorithm

Run by `aislang plan`. Deterministic given source + catalog + lockfile.

```
resolve(model_block, catalog, lockfile):
  if lockfile has binding for this model_block AND source unchanged:
    return lockfile.binding

  candidates = catalog.models filter where:
    requires ⊆ model.capabilities
    catalog.context_max >= context_min
    cost_input_per_1k_tokens <= cost_max (if set)
    catalog.latency_p95 <= latency_p95   (if set)
    residency ⊆ catalog.residency        (if set)
    not deprecated

  ordered = []
  for m in model_block.prefer:
    if m in candidates: ordered.append(m)
  for m in model_block.fallback:
    if m in candidates and m not in ordered: ordered.append(m)

  if model_block.pin is set:
    if pin not in ordered: error("pin violates constraints")
    chosen = pin
  else if ordered is empty:
    error("no model satisfies constraints", show rejection reason per model)
  else:
    if budget.optimize_for == cost:
      chosen = candidate in ordered with lowest cost_input_per_1k_tokens
    elif budget.optimize_for == latency:
      chosen = candidate in ordered with lowest latency_p95
    else:
      chosen = ordered[0]

  write to lockfile
  return chosen
```

### 6.1 Lockfile

`aislang.lock` is a JSON file committed to source control. It pins:

- The catalog version used at last resolution.
- For each `model` block: the resolved `ModelId`, the source hash, the catalog hash.
- For each `prompt` block: the file content hash.
- For each `mcp_server` block: the resolved OCI digest (if image was tag-only).

`aislang plan` shows a diff when any of these would change.

---

## 7. Out of scope for v0

The following are explicitly deferred. Do not implement them in the v0.1 compiler.

- Functions, expressions beyond literals + references
- Variables / interpolation (`${var.name}`)
- Modules / imports
- Conditional resources (`count`, `for_each`)
- Multi-file projects
- Remote state / shared lockfile
- Catalog updates over the network
- `k8s`, `helm`, `ecs` deploy targets
- LSP / IDE plugins
- Visual editor
- Any GUI

Anything in this list moves to `LANGUAGE-SPEC-v1.md` after launch.

---

## 8. EBNF (skeleton — finalize Friday of week 1)

```
project       = "project" StringLit "{" project_body "}"
project_body  = (budget | model | prompt | mcp_server | agent | eval | secret | deploy)+

budget        = "budget" "{" assignments "}"
model         = "model" StringLit "{" assignments "}"
prompt        = "prompt" StringLit "{" assignments "}"
mcp_server    = "mcp_server" StringLit "{" assignments "}"
agent         = "agent" StringLit "{" assignments "}"
eval          = "eval" StringLit "{" assignments "}"
secret        = "secret" StringLit "{" assignments "}"
deploy        = "deploy" StringLit "{" assignments "}"

assignments   = (assignment)*
assignment    = Ident "=" expr

expr          = literal | reference | list | map | rate
literal       = string | int | float | bool | money | duration | size | percentage
                | semver | path | oci_image | uri | enum_member
reference     = Ident ("." Ident)+
list          = "[" (expr ("," expr)*)? "]"
map           = "{" (Ident "=" expr ("," Ident "=" expr)*)? "}"
rate          = (money | int | float) "per" (size | duration | int Ident)

# Tokens
money         = ("$" | "€" | "£" | "¥") Number
duration      = Number ("ns"|"us"|"ms"|"s"|"m"|"h"|"d")
size          = Number ("k"|"K"|"MB"|"GB"|"KB")
percentage    = Number "%"
…
```

Finalize and commit by Friday of week 1.
