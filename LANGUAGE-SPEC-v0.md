# aiSlang Language Specification — v0 (v0.1 baseline + v0.2 additions)

Status: **Authoritative for v0.2.0-rc2** (Phase 2 Week 14 close — Track B foundation).  The v0.1 baseline is frozen; Phase 1 (Week 12-13) additions are forward-compatible at the AST level; Phase 2 Week 14 (Track B) additions are likewise purely-additive (any valid v0.1 file produces an AST that v0.2-rc2 deserialises identically — see §7).  One controlled grammar break in v0.2 Tue: see §4.9.

This spec defines exactly enough of the language to make `examples/support-agent.ais` (in [PLAN-90-days-v0_1.md](PLAN-90-days-v0_1.md)) compile, type-check, plan, deploy, and run, **plus** the v0.2 grammar additions that landed in Phase 1 + Phase 2 Week 14:

- **Week 12 Fri.**  New `main { … }` block — declares the user-facing LLM as first-class topology (planning-doc Track A architectural decision #7).  New `model.source = subscription | api` enum — opt-in declaration that a model's inference path lives in the host chat app's subscription tier (not LiteLLM).  Three new closed enums in §3.4: `Pattern` (6 variants), `ChatClient` (5 variants), `ModelSource` (2 variants).
- **Week 13 Tue (Track A2).**  `deploy.expose` promoted from attribute (`expose = { port = N }`, v0.1) to nested block (`expose { port = N  web_ui = true }`, v0.2) — see §4.9.  New `web_ui: Bool?` field opt-ins an Open WebUI Compose sidecar.  Grammar break against v0.1; parser surfaces a migration hint for the legacy form.  Phase-1-pre-public-launch break per Decision 34 — no v0.1 users to migrate.
- **Week 13 Tue follow-up.**  `prefer / fallback` resolver semantics changed to match LANGUAGE-SPEC §6 prose exactly: `prefer` is now an ORDERED preference list (walked in declared order; first satisfier wins).  `fallback` only used when no `prefer` candidate satisfies constraints.  `optimize_for = cost | latency` no longer re-orders the walk — became informational at the resolver layer (kept on the AST as a v0.3+ hint for multi-row catalog matches).  v0.1 behaviour where `optimize_for = cost` silently overrode `prefer` is gone — see §6 for the pseudocode + the behaviour-change note.
- **Phase 2 Week 14 (Track B).**  Two grammar additions:
  - **Mon**: `target = kubernetes` joins `target = compose` as an accepted `DeployTarget` enum variant on `deploy` blocks (see §4.9 + §7.3 for the resource-mapping table).
  - **Thu**: `expose { … }` block gains an optional `ingress : String?` field — when set together with `web_ui = true`, the K8s emitter emits an `Ingress` resource fronting the Open WebUI Service at the named host.  Ignored under `target = compose` (Compose's `3000:8080` host-port mapping covers the same browser-access need without needing an Ingress controller).  Forward-compat: v0.1 / v0.2-pre-Thu files round-trip byte-stably because the field is `Option<String>` (`skip_serializing_if = "Option::is_none"`).
  - **Locked architectural decisions** for the K8s emitter are recorded as Decisions 43.1-43.8 in PLAN-90-days-v0_2.md §"Phase 2 — Track B foundation"; this spec stays grammar-only and points at the PLAN for the resource-mapping rationale.

Anything not described here is out of scope for v0.2.

---

## 1. Overview

aiSlang is a typed declarative language for describing an AI project — its agents, models, prompts, MCP servers, evals, budgets, secrets, and deployment target — in a single versioned file.

Design principles, in order of precedence when they conflict:

1. **Type safety over expressiveness.** Every literal has a type. Type mismatches are caught at `validate`, never at runtime.
2. **Intent over implementation.** Users describe what they need (capabilities, constraints); the planner picks how (which provider, which model, which fallback).
3. **Reproducibility.** Plans are deterministic given the same source + lockfile + catalog.
4. **Standards-only output.** The compiled artifact is plain Docker Compose YAML. No proprietary runtime.
5. **Single file by default.** A project compiles from one `.ais` file. Multi-file is Phase 1+ (§7.2). The v0.1 grammar reserves the syntactic room so multi-file additions are backward-compatible.

Syntax is HCL-like: blocks with labels, `key = value` assignments, nested blocks.

---

## 2. Lexical structure

- **Encoding.** UTF-8 only.
- **Comments.** Line comments: either `#` or `//` to end of line (both accepted, HCL convention). Block comments (`/* … */`) deferred to v0.2.
- **Identifiers.** `[a-z][a-z0-9_]*` (snake_case). Resource labels follow the same rule.
- **Whitespace** is insignificant except as a token separator.
- **Reserved words** (v0.1 baseline): `project`, `budget`, `model`, `prompt`, `mcp_server`, `agent`, `eval`, `secret`, `deploy`, `true`, `false`, `null`, `per`, `unlimited`. The `unlimited` keyword is a `Money` literal (Decision 10, week 7 Mon) — used in `budget.hard_cap_per_*` to mean "no cap" (free local models, infinite budget). Treated as the absorbing top in cost comparisons.
- **Reserved words added in v0.2** (Week 12 Fri):
  - `main` — new top-level block type inside `project { … }` (see §4.10).
  - `pattern`, `chat_client`, `source` — new attribute names on `main` and `model` blocks.
  - Closed enum members for the new fields: `orchestrator`, `supervisor`, `router`, `reflector`, `planner_executor`, `coordinator` (Pattern); `claude_desktop`, `cursor`, `chatgpt`, `open_webui`, `custom` (ChatClient); `api`, `subscription` (ModelSource).  Enum members are bare identifiers in expression position (§3.4) and don't collide with the user's identifier namespace — using `cursor` as a label is still legal.
- **Reserved words (forward-compatibility, no semantics in v0.2):** `decorator`, `prompt_pipeline`, `guardrail`, `middleware`, `import`, `module`, `package`, `var`, `include`, `export`, `catalog`, `catalog_model`, `secret_provider`. Using any of these as an identifier is a compile error. They are claimed for Phase 1+ features (dynamic prompt composition — see §7.1; multi-file projects — see §7.2; possible `.ais`-native model catalog — see §5.5; enterprise secret providers — see §5.7) so v0.1+v0.2 files remain forward-compatible.
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
| `SemVer` | `"1.0.0"`, `"1.2.3-alpha"`, `"2.0.0+build.123"` | exact semver-2.0 version; range operators (`^`, `~`, `>=`) deferred to v0.2 |
| `Path` | `"./prompts/system.md"` | resolved relative to the `.ais` file at validate time; existence checked |
| `OciImage` | `"ghcr.io/foo/bar:1.2.3"`, `"ghcr.io/foo/bar@sha256:…"` | string-literal at the grammar level; parsed into registry/repo/tag/digest by `OciImage::from_literal` based on field type. Quoted form is consistent with the other complex string types (`Path`, `URI`, `SemVer`) and keeps the lexer simple |
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
# v0.1 baseline:
Capability     = chat | embedding | tool_use | vision | audio | reasoning | large_context | structured_output
Residency      = us | eu | uk | apac
DeployTarget   = compose                                   # v0.1 — non-exhaustive across versions (see note below)
Currency       = usd | eur | gbp | jpy                     # implied by Money literal symbol
OptimizeFor    = cost | latency | quality
EvalMetric     = exact_match | f1 | bleu | rouge_l | llm_judge
EvalAggregator = mean | min | max | percentile_p95

# v0.2 additions (Week 12 Fri — `main` block + `model.source`):
Pattern        = orchestrator | supervisor | router | reflector | planner_executor | coordinator
ChatClient     = claude_desktop | cursor | chatgpt | open_webui | custom
ModelSource    = api | subscription                        # default: api (preserves v0.1 behaviour)
```

The catalog (§5) defines two additional pseudo-enums populated at compile time from the catalog file:

```
ProviderId  = anthropic | openai | google | ollama | …       # one per catalog entry
ModelId     = anthropic.claude_haiku_4_5 | openai.gpt_4o_mini | …
```

**Enum extensibility across versions.** All closed enums above are exhaustive *within a single version of the spec* but **non-exhaustive across versions** — future spec versions may add new members. `DeployTarget` in particular is expected to grow (`k8s`, `helm`, `ecs` in Phase 1; `databricks`, `fabric`, `spark` in Phase 2 — see §7.3). Adding a new member never invalidates an existing v0.1 file. The type-checker error message for an unknown enum member must include the version that introduced new alternatives so users know whether to upgrade.

### 3.5 Type coercion

There is no implicit coercion between domain types. `"$50"` (String) is not assignable to a `Money` field. The error message must show both the actual and expected type, and a suggested literal form.

---

## 4. Resource types

Ten resource types as of v0.2 (nine from v0.1 + `main` added Week 12 Fri).  Each is a labeled block; labels are unique within their type — meaning `main "x"` and `agent "x"` can both exist (different kinds, disjoint label spaces in the resource graph).

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

  source      : ModelSource = api        # NEW v0.2 (Week 12 Fri) — optional
                                         # api          → route through LiteLLM
                                         #                (v0.1 behaviour); hard
                                         #                budget cap enforcement.
                                         # subscription → host chat app handles
                                         #                inference (Claude
                                         #                Desktop / Cursor /
                                         #                ChatGPT subscription).
                                         #                Tokens counted +
                                         #                cost estimated, no
                                         #                hard cap (we can't
                                         #                gate the chat app's
                                         #                calls).
}
```

The compiler resolves `requires + constraints + prefer/fallback` against the catalog; see §6.

**`source` semantics (v0.2 addition)**:

- Default: `api`.  v0.1 files (no `source` declared) keep their existing v0.1 routing — LiteLLM + hard budget enforcement.
- `subscription` indicates the inference path lives in the host chat app's subscription tier.  The model is still resolved against the catalog (so the user's choice of provider stays declarative), but:
  - The compose-stage LiteLLM config does NOT include a routing entry for this model.
  - The runtime spend tracker counts tokens for observability but does NOT enforce the budget cap (over-cap reports a warning instead of aborting the turn).
  - `cost_max` is informational — the planner uses it for catalog filtering but doesn't enforce it at run time.

This enables the cost-saving pattern: orchestrator runs free on the user's existing Claude Pro / Cursor Team subscription; only deployed agents hit the API.  Typical savings: 40–60 % vs an API-only deployment.  Forward-compat is preserved (every v0.1 model block continues to parse + validate + plan identically).

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
  target : DeployTarget              # v0.1: `compose`; v0.2 Phase 2: also `kubernetes`
  expose { ... }?                    # v0.2 Tue: nested block, see below
}
```

**`DeployTarget` enum** (closed set):

| Variant | Since | Emitter | Apply path |
|---|---|---|---|
| `compose` | v0.1 | `aislang-compose` → `stack.yml` + `litellm_config.yaml` + `.env.example` | `docker compose up -d` |
| `kubernetes` | v0.2 Phase 2 Week 14 Mon | `aislang-k8s` → `manifests/*.yaml` (raw K8s) + `chart/` (Helm wrapper) | `helm upgrade --install <deploy-label> chart/` |

Other targets (ecs, nomad, databricks, vertex_ai, …) are v0.3+ — see §7.3 for the additive-emitter strategy that keeps them out of the language core.

**`expose { ... }` block** (v0.2 Tue, Track A2 — replaces the v0.1 `expose : Map<String, Int>` attribute; v0.2 Phase 2 Week 14 Thu adds the `ingress` field):

```
expose {
  port    : Int?           # informational under compose; functional under kubernetes
  web_ui  : Bool?          # default false; true → Open WebUI sidecar
  ingress : String?        # v0.2 Phase 2 Thu — only meaningful under target = kubernetes
}
```

Field semantics:

- **`port`** — under `target = compose`: informational only, surfaces as a comment in the emitted `stack.yml` header (no v0.1/v0.2 Compose service binds it).  Under `target = kubernetes` (Phase 2 Week 14 Wed): functional — becomes the agent-runtime `aislang mcp-serve --http 0.0.0.0:<port>` bind address + the Service `port` + container `containerPort`, all in lockstep.  Defaults to `8080` when absent (parity with the Compose informational header).
- **`web_ui = true`** — under `target = compose`: instructs the emitter to add an Open WebUI sidecar (`ghcr.io/open-webui/open-webui:main`) mapped `3000:8080`, pre-pointed at the in-stack LiteLLM via `OPENAI_API_BASE_URL=http://litellm:4000/v1`.  Persisted to a named volume (`openwebui-data`) that `aislang destroy` wipes (`docker compose down -v`).  Under `target = kubernetes` (Phase 2 Week 14 Thu): emits an Open WebUI Deployment + Service + PersistentVolumeClaim bundle into `manifests/openwebui.yaml`; PVC defaults to 1Gi (overridable via the chart's `values.yaml::openwebui.persistence.size`).
- **`ingress = "<host>"`** — v0.2 Phase 2 Week 14 Thu addition (Decision 43.6).  Only meaningful under `target = kubernetes`; ignored by the Compose emitter (Compose's `3000:8080` host-port mapping serves the same browser-access purpose).  When set together with `web_ui = true`, the K8s emitter writes a `networking.k8s.io/v1` Ingress to `manifests/ingress.yaml` routing `/` Prefix at the named host to the openwebui Service's named `http` port.  TLS not in v0.2's Ingress surface — wire cert-manager externally or terminate TLS upstream.  Empty-string value rejected at parse time (typo guard).  Setting `ingress` without `web_ui = true` is parseable but emit-silent (no upstream Service to route to).

**Grammar break vs. v0.1.** The v0.1 attribute form `expose = { port = 8080 }` (a `Map<String, Int>` value) is no longer accepted.  Parser rejects with a migration hint; replace with `expose { port = 8080 }` and optionally add `web_ui = true` / `ingress = "<host>"` inside.  Phase-1-pre-public-launch break per Decision 34 (no v0.1 users to migrate); v0.2+ files only.

### 4.10 `main` (v0.2, Week 12 Fri)

Declares the user-facing LLM as the explicit entry point of the agentic system — same role `fn main` plays in a Rust binary.  The `pattern` field names the *relationship* between this entry point and the deployed agents; the block name `main` is the pattern-agnostic role descriptor.

```
main "<identifier>" {
  model         : Ref<Model>
  chat_client   : ChatClient
  pattern       : Pattern
  system_prompt : Ref<Prompt>
}
```

**Semantics:**

- **`main` blocks are never deployed.**  Purely declarative — the resolver records them in the IR, the emitter ignores them (no Compose service, no k8s pod, no LiteLLM routing entry).  They exist so the MCP server can describe the principal-LLM's expected role to the host client.
- **Multiple `main` blocks per project are allowed.**  Example: a customer-facing entry (`main "user_facing" { pattern = orchestrator … }`) AND an admin escalation entry (`main "admin" { pattern = supervisor … }`).  The MCP server picks which one to activate based on the `chat_client` field matching the connecting client's identity.
- **`model` field**: references a `model` block, just like `agent.model`.  Typically paired with `source = subscription` on that model when the entry point runs on the user's chat-app subscription tier.
- **`chat_client` field**: identifies the host LLM client this entry point is designed for.  `claude_desktop`, `cursor`, `chatgpt`, `open_webui`, `custom`.
- **`pattern` field**: names the relationship to deployed agents.  Each pattern ships a default system-prompt template (v0.3 work).  All six variants are valid v0.2 syntax; the runtime behavioural distinction between them is v0.3+ implementation work.  Variants and intended roles:

  | Pattern | Role of the `main` entry |
  |---|---|
  | `orchestrator` | Plans the overall task, dispatches sub-tasks to deployed agents in sequence, aggregates results. |
  | `supervisor` | Monitors deployed-agent outputs, can intervene/redirect, makes final approve/reject calls. |
  | `router` | Classifies user input, routes to exactly one deployed agent per turn. |
  | `reflector` | Reviews/critiques outputs from deployed agents; produces refined final answers. |
  | `planner_executor` | Plans the steps; delegates execution to deployed agents; reports aggregated outcome. |
  | `coordinator` | Facilitates communication between multiple peer deployed agents (multi-agent collaboration). |

- **`system_prompt` field**: required.  References a `prompt` block whose file contains the role/persona description for the `main` entry.  Validated for ref-resolution exactly like `agent.system_prompt`.

**No `tools` field on `main` blocks.**  Deployed agents become the host LLM client's tools via the MCP server's `aislang_chat_turn` tool — there's no direct tool-call dispatch from `main` to MCP servers.  This is the architectural simplification of the proxy-via-MCP design (planning-doc Track A architectural decision #3).

**No `budget` interaction at validate time.**  Budget enforcement is conditional on `model.source`:

- `source = api` (default): hard fail-closed enforcement (v0.1 behaviour).
- `source = subscription`: report-only — we surface estimated cost in `aislang status` but don't gate the host chat app's calls.

**Example**:

```
main "user_facing" {
  model         = model.primary     # → references `model "primary"` declared in the same project
  chat_client   = claude_desktop
  pattern       = orchestrator
  system_prompt = prompt.main_brief
}

model "primary" {
  requires = [chat, tool_use]
  context_min = 200k
  prefer   = [anthropic.claude_sonnet_4_6]
  source   = subscription           # Claude Pro / Team subscription; no API charges
}

prompt "main_brief" {
  file    = "./prompts/main_brief.md"
  version = "1.0.0"
}
```

The customer's Claude Pro subscription becomes the orchestrator infrastructure for the agentic system; only the deployed agents (`agent "triage"`, `agent "drafter"`, etc.) hit the API.

### 4.11 Multi-module projects — `aislang.toml` + `use <module>.<label>` (v0.3, Week 16)

v0.3 lifts the v0.1 deferral of §7.2 (multi-file projects) for the narrow case the dogfood-agent workflow needs: cross-file resource references within a single project root.  Larger composition primitives (workspaces, package registries) remain v0.5+.

**Project manifest (`aislang.toml`).**  A multi-module project is a directory containing an `aislang.toml` plus one or more `.ais` files.  The manifest declares:

```toml
[project]
name    = "support-stack"
version = "0.1.0"

[modules]
architect = "agents/architect.ais"
coder     = "agents/coder.ais"
reviewer  = "agents/reviewer.ais"
```

- `[project] name` (String, required) — project label.  Same role as the in-`.ais` `project "<name>" { … }` block's label for single-file projects.
- `[project] version` (String, required) — semver-compatible.  Stamped into the lockfile.
- `[modules]` (table, required, non-empty) — `<module-name>` → `<path-relative-to-manifest>`.  Module name is the namespace used by `use <module>.<label>` references (below); path is the `.ais` file holding the module body.

Forward-compat: unknown top-level tables / fields are ignored at parse time — same `skip_serializing_if` discipline that keeps v0.2 expected-AST JSON fixtures byte-stable when v0.3 fields default to empty.

**Module shape.**  Each `.ais` file in a multi-module project still carries a `project "<label>" { … }` block declaring its resources.  Within the `[modules]` table the module's resources live in the `<module-name>.` namespace at resolution time — `model "fast"` declared in `agents/coder.ais` becomes `coder.fast` in the merged `ResolvedProject`.

**Cross-module references — `use <module>.<label>`.**  Reference a resource declared in a sibling module via the `use` keyword in any value position where a same-module reference (`<kind>.<label>`) is legal:

```aislang
agent "coder" {
  ## Same-module ref (v0.2 shape): kind explicit.
  system_prompt = prompt.system

  ## Cross-module ref (v0.3): `use` + module name + label.
  ## Kind is inferred from the attribute context (`model =` →
  ## `RefKind::Model`); the validator resolves `architect.thinker`
  ## against the cross-module symbol table.
  model         = use architect.thinker
}
```

Single-module `validate()` rejects `use <module>.<label>` refs with a diagnostic pointing the user at `aislang.toml`; multi-module `check_multi()` resolves them against the loaded module set, surfacing source-aware errors for unknown modules or unknown labels (with the same did-you-mean Levenshtein suggester v0.2 ships for same-module refs).

**CLI dispatch.**  `aislang validate <dir>` and `aislang plan <dir>` autodetect the manifest and switch to the multi-module pipeline (load every declared module → check_multi → resolve_multi → emit one merged stack).  Single-file inputs (`aislang plan file.ais`) follow the v0.2 path byte-stably.  `aislang apply <dir>` ships a deferred-friendly error in v0.3 pointing at the `plan <dir>` + `docker compose` / `helm upgrade --install` two-step workaround; full dir-input `apply` lands later in v0.3.

**Lockfile.**  Multi-module projects gain a `modules` section in `aislang.lock` carrying per-module SHA-256 source hashes.  Reconciliation is over-broad on purpose: any module's hash mismatch (or any add/remove of a manifest entry) triggers a full re-resolve.  Per-module incremental rebuilds are a v0.5+ carry-over.

**Decision 44.1 sub-points deferred.**  Cross-module access control (private vs public resources) is v0.5+; v0.3 makes every resource cross-module-accessible.  Cross-module refs in `secret.scope` lists are not supported in v0.3 — the union-kind syntax can't infer kind from context the way single-field attributes do.

See `examples/multi-module-demo/` for a worked example exercising the surface end-to-end + `crates/aislang-cli/tests/cli_plan_dir_input.rs` for the integration-test contract pinning the user-facing CLI flows.

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

### 5.4 File formats — JSON now, YAML later

| Surface | Format in v0.1 | Notes |
|---|---|---|
| `.ais` source | HCL-like (neither JSON nor YAML) | Typed literals (`$50`, `32k`, `2s`, `85%`) lose their type under YAML/JSON quoting; HCL-like is non-negotiable |
| Model catalog | JSON only | YAML acceptance added in v0.2 via `serde_yaml`; same Rust struct, zero spec change |
| Lockfile (`aislang.lock`) | JSON only | Machine-generated, PR-diffable; never hand-edited |
| State file (`.aislang/state.json`) | JSON only | Same reasoning as the lockfile |
| `aislang plan` output | Human text by default; JSON via `--format json` | The JSON shape is stable for agent consumption |
| Compose / K8s / Helm output | YAML | Format forced by the target tool, not chosen by aiSlang |

JSON and YAML coexistence costs one line of code (`serde` deserializes either into the same struct). Source-language format is the only one that materially matters — see Principle 1 in §1.

### 5.5 Possible `.ais`-native catalog (deferred, not committed)

In v0.1 the catalog is JSON because no IaC predecessor (Terraform, Pulumi, Helm, Crossplane) actually dogfoods its main DSL for its provider/registry metadata — the registry has a different audience (provider authors), different lifecycle, and different schema, so the friction of mixing them outweighs the elegance. v0.1 follows that pattern.

That said, the *value-level* type-safety of an `.ais` catalog is captured today: every JSON value string in the catalog (`"$0.0008"`, `"200k"`, `"5s"`) is deserialized through the same `from_literal` parsers the language uses, so malformed values fail at binary build time, not at runtime.

The `catalog` and `catalog_model` keywords are reserved (§2) so a future migration to an `.ais`-native catalog remains possible. Concrete criteria for adopting it:

- Catalog grows to ≥20 fields per entry, or
- Catalog gains complex inter-entry constraints that a JSON schema can't express, or
- Real user demand for catalog-side LSP / typeahead / refactoring tools emerges.

If none of these triggers fires by 6 months post-launch, the keywords can be unreserved.

### 5.6 `secret` blocks as the canonical credential source (v0.2 direction)

**v0.1 behavior — dual flow.** Today `aislang plan` populates `.env.example` from two independent sources:

1. **Implicit (provider-derived).** For each *resolved primary* `ModelId`, a small hard-coded table in `aislang-compose::litellm::provider_creds()` maps `provider → {api_key_env_var, api_base_env_var}` (anthropic → `ANTHROPIC_API_KEY`, ollama → `OLLAMA_API_BASE`, …). These vars appear in `.env.example` without any source-level declaration.
2. **Declared (user-authored).** Each `secret "<label>" { source = "env://VAR" … }` block contributes `VAR` to the same `.env.example` list. Non-`env://` sources (`file://`, future `vault://`) are surfaced in an informational footer comment block but not loaded as env vars.

Both sources are merged into a single alphabetized, deduplicated env-var list. If a user declares `secret { source = "env://OLLAMA_API_BASE" }` and the same var is provider-derived, it appears exactly once.

This dual flow is **deliberately temporary**. The implicit path makes the demo work without ceremony — a user writes one `model "fast"` block and `.env.example` is correct — but it also means provider credentials are invisible at the source-language level: there's no `.ais` syntax that says "this project depends on `ANTHROPIC_API_KEY`."

**v0.2 direction — secret blocks as canonical.** In v0.2 the declared path becomes the source of truth:

- The catalog gains a `provider_credentials` schema (per-provider list of required env vars + their roles: `api_key` / `api_base` / `region` / …).
- `aislang plan` walks the resolved providers, looks up their declared credential requirements in the catalog, and *requires* a matching `secret` block in the source (or implicit auto-generation with a `--auto-secrets` flag for ergonomics).
- The hard-coded `provider_creds()` table becomes a fallback only for projects that opted out of explicit declaration; the canonical answer to "what env vars does this project need?" is reading `project.secrets`.
- `mcp_server.secrets = [secret.x, secret.y]` already scopes env vars to specific containers in v0.1 — that mechanism extends naturally to LiteLLM-scoped provider creds in v0.2.

**Why not in v0.1.** Requiring `secret` blocks for every provider credential today would force the demo to grow three secret blocks (one per cloud provider in the prefer/fallback chain) for a project that resolves to a single primary. The catalog schema also needs the `provider_credentials` field, which is a multi-week change. v0.1 keeps both paths to ship the demo in 90 days; v0.2 unifies them once the catalog schema settles.

**Forward-compatibility.** v0.2 is purely additive: an existing v0.1 source file with no `secret` blocks continues to work (the provider-creds table remains a fallback). Users who already declared `secret` blocks for their provider credentials get the v0.2 unified flow for free.

The TODO marker lives at `crates/aislang-compose/src/litellm.rs::provider_creds` so the v0.2 migration point is visible from code search.

### 5.7 Enterprise secret providers — v0.2 direction (Decision 11)

**v0.1 behavior — URI-based sources only.** Today `secret.source` accepts a URI string with one of two schemes:

- `env://VAR` — read the value from process env at runtime.
- `file://path` — read the file's contents (UTF-8 string) as the secret value.

Other schemes are rejected at parse time with a diagnostic pointing to this section. Both schemes are zero-dep (`env::var` + `std::fs::read`); they cover the local-dev and single-file-secret cases that the v0.1 demo needs.

**Why this isn't enough long-term.** Enterprise deployments fetch secrets from centralized stores — HashiCorp Vault, AWS Secrets Manager, GCP Secret Manager, Azure Key Vault, Kubernetes Secrets, 1Password, etc. Talking to those stores requires more than a URI:

- The store's *address* (`https://vault.example.com:8200`).
- An *auth flow* (Kubernetes service account, AWS IAM role, JWT bearer token, Vault AppRole, …) — each with its own setup, refresh cadence, error semantics.
- A *path or key reference* inside the store, possibly with a *field* selector (e.g. `secret/data/production/openai_key#value`).
- Possibly TLS verification config (custom CA, mTLS).

Cramming all that into a URI string is brittle and unreadable. The conventional IaC pattern (Terraform `provider` blocks, Pulumi `Provider`, Helm `secretsManager.*`) separates the *store configuration* from the *secret reference*.

**v0.2 direction — `secret_provider` resource type.** A new resource block describes the store connection; existing `secret` blocks reference it:

```hcl
secret_provider "company_vault" {
  type    = vault
  address = "https://vault.example.com:8200"
  auth    = kubernetes
  role    = "aislang-agents"
}

secret "openai_key" {
  provider = secret_provider.company_vault
  path     = "secret/production/openai_key"
  field    = "value"            # which field of the store payload is THE secret
  scope    = [agent.support]
}
```

Initial v0.2 provider types under design (final list determined by user demand): `vault`, `aws_sm`, `gcp_sm`, `azure_kv`, `kubernetes`, `onepassword`. Auth flows specific per provider.

**Forward-compatibility — `secret_provider` is reserved in v0.1.** §2 reserves the `secret_provider` keyword today so a v0.1 source file that doesn't use it parses cleanly in v0.2 (no breaking change). When v0.2 lands, the `secret` block gains the optional `provider` / `path` / `field` fields in addition to the existing `source` field. Migration is opt-in per secret block; v0.1's `env://` and `file://` URIs continue to work as the "no provider" path.

**Out of scope until v0.2.** Auth-token refresh loops, secret rotation hooks, audit trails (who-fetched-what-when), per-secret-provider rate limiting, multi-region failover for the store. Phase-1+ behind cargo feature flags.

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

  # v0.2 Tue follow-up: spec-conformant prefer/fallback walk.
  # `prefer` is an ORDERED preference list; `fallback` is used
  # only if no prefer candidate satisfies constraints.
  # `optimize_for` does NOT re-order — it stays on the budget
  # block as a forward-compat hint for v0.3+ multi-row catalog
  # matches (currently a no-op because catalog IDs are unique).

  if model_block.pin is set:
    if pin in candidates and pin in (prefer ∪ fallback):
      chosen = pin
    else:
      error("pin violates constraints or not in prefer/fallback")
  else:
    chosen = None
    for m in model_block.prefer:                  # walk in declared order
      if m in candidates:
        chosen = m
        break
    if chosen is None:
      for m in model_block.fallback:              # walk in declared order
        if m in candidates:
          chosen = m
          break
    if chosen is None:
      error("no model satisfies constraints", show rejection reason per model)

  write to lockfile
  return chosen
```

> **v0.1 → v0.2 Tue follow-up behaviour change.**  v0.1 built a candidate pool from `prefer ∪ fallback` and let `optimize_for = cost | latency` re-sort it, which silently overrode the declared preference order — `prefer = [anthropic.claude_haiku_4_5]` could lose to a free `fallback = [ollama.llama_3_1_8b]` under the default `optimize_for = cost`.  v0.2 Tue follow-up makes the resolver match the prose above this code block: walk `prefer` in declared order, fall back only when no prefer satisfies, treat `optimize_for` as informational at the resolver layer.  No-`pin`-no-`prefer`-no-`fallback` projects still error as before (resolver has nothing to walk).

### 6.1 Lockfile

`aislang.lock` is a JSON file committed to source control. It pins:

- The catalog version used at last resolution.
- For each `model` block: the resolved `ModelId`, the source hash, the catalog hash.
- For each `prompt` block: the file content hash.
- For each `mcp_server` block: the resolved OCI digest (if image was tag-only).

`aislang plan` shows a diff when any of these would change.

---

## 7. Out of scope for v0

The following are explicitly deferred. Do not implement them in the v0.1 or v0.2 compiler.

**Forward-compat invariant for v0.2 additions** (Week 12 Fri): the `main` block and `model.source` field are purely additive — every valid v0.1 file remains valid under v0.2 with byte-identical typed-AST output and byte-identical lockfile output.  Pinned by `crates/aislang-core/tests/parser_fixtures.rs` (existing v0.1 expected-AST JSON fixtures stay unchanged) plus four dedicated JSON-round-trip tests in `mod project_tests` of `parser.rs` (`project_without_mains_omits_field_in_json`, `model_with_default_source_omits_field_in_json`, etc.).  The same invariant applies to all v0.3+ additions: new resource types, new optional fields, new enum members.  Removing or renaming existing v0.1/v0.2 surface is a major-version break.

- Functions, expressions beyond literals + references
- Variables / interpolation (`${var.name}`)
- Conditional resources (`count`, `for_each`)
- Remote state / shared lockfile
- Catalog updates over the network
- LSP / IDE plugins
- Visual editor
- Any GUI
- **Dynamic prompt composition** — decorators, `prompt_pipeline`, `guardrail`, `middleware` — see §7.1
- **Multi-file projects** — `include`, `import`, `module`, `package` — see §7.2
- **Non-Compose deploy targets** — `k8s`, `helm`, `ecs`, `databricks`, `fabric`, `spark`, etc. — see §7.3
- **Binary IR / wire format** — flatBuffers/protobuf/cap'n proto for AST, lockfile, state, or plan — see §7.4
- **JSON Schema export of the AST** — via `schemars`; targeted as first Phase 1 task — see §7.4

Anything in this list moves to `LANGUAGE-SPEC-v1.md` after launch.

### 7.1 Dynamic prompt composition — deliberately deferred

Decorators, prompt pipelines, and guardrail middleware (reactive injection of date/locale/RAG context, PII redaction, schema enforcement, policy gates) are not in v0.1. The decision and its reasoning are recorded here so it isn't re-litigated under feature-request pressure.

**Why not now**

1. The wedge is *static prompts done right* — versioning, content hashes in the lockfile, file-backed sources. P2 (prompt sprawl) is killed by auditability, not by adding dynamic behavior on top.
2. Runtimes (Anthropic context management, OpenAI Responses, MCP servers, dedicated guardrail providers) are absorbing these responsibilities. A DSL-level decorator system would reinvent them and age badly.
3. LangChain's prompt-template chains are the cautionary example — a "good idea" abstraction that became the layer everyone complains about. DSPy succeeded partly by *not* doing this at the language level.
4. Decorators imply composition rules, application order, conditional application, a type system for inputs/outputs, and a debugger story. That is 4+ weeks of design and implementation work hiding behind innocent-looking syntax.
5. Committing without user pull turns every example, doc, and breaking change into expensive work. Wait for demand.

**Partial substitute in v0.1**

Prompt *files* (markdown referenced by `prompt` blocks) may contain a closed set of template variables, substituted by the runtime before the prompt is sent:

```
{{date}}              ISO-8601 date at run time
{{project.name}}      project label
{{model.id}}          resolved ModelId from aislang.lock
{{agent.name}}        agent label
```

No user-defined variables. No conditionals. No loops. The type checker validates every `{{…}}` against this fixed set at `validate` time. This covers ~80% of "inject context into the prompt" needs without opening the decorator design.

**Reserved syntax for v0.2+**

`decorator`, `prompt_pipeline`, `guardrail`, `middleware` are reserved words (§2). Their grammar and semantics are deferred. Files written for v0.1 will remain valid when these are introduced.

**Signals to revisit**

Design these in Phase 1 *if and only if* one of these fires after launch:

- 5+ unsolicited GitHub issues specifically requesting prompt composition / guardrail middleware from real users.
- One paying enterprise customer naming this as a procurement requirement (e.g., "every prompt must pass a PII-redaction layer before the model sees it" under EU AI Act or HIPAA).

If neither fires within 6 months of launch, the feature is unnecessary and should be removed from the reserved-word list to free the identifiers.

### 7.2 Multi-file projects — DELIVERED in v0.3 Week 16 (see §4.11)

**Status update (v0.3 Week 16 Fri):** the narrow case of cross-file resource references within a single project root is now implemented; see §4.11 for the full `aislang.toml` + `use <module>.<label>` surface.  This subsection retains its v0.1 backward-compat invariants for historical reference + as the contract every successor surface must continue to honor.

The three invariants v0.1 locked in are still upheld by v0.3's implementation:

1. **A complete `project "name" { ... }` block always means exactly what it means today.** ✅ v0.3 multi-module modules each carry their own `project` block; the `[modules]` table in `aislang.toml` composes them — no redefinition.
2. **Resource block syntax (`model "fast" { ... }`, `prompt "x" { ... }`, etc.) never changes meaning.** ✅ v0.3 modules use the same resource block syntax verbatim.
3. **Multi-file constructs are added *above* or *alongside* the `project` block, never inside it.** ✅ `aislang.toml` lives ABOVE the `.ais` files; the `use <module>.<label>` syntax is additive in value position and doesn't repurpose any v0.1 reference syntax.

The v0.1 parser obligation — file-level production as a list of top-level statements — is still honored, leaving room for the post-v0.3 successor surfaces below.

**Three-stage evolution — status update**

| Phase | Capability | Status | Notes |
|---|---|---|---|
| v0.3 | **Manifest-driven multi-module** (Cargo-style) | ✅ shipped Week 16 | `aislang.toml` + `[modules]` + `use <module>.<label>`; see §4.11 |
| v0.5+ | **Package registry** (npm-style) | deferred | `package "myorg/aislang-stdlib" version "^1.2.0"` + registry-backed resolution — uses reserved `package` keyword |
| v0.5+ | **Workspaces** (Cargo-style) | deferred | Multiple projects under one manifest; cross-project deps + shared lockfile |

Reserved keywords for the v0.5+ tier are already claimed in §2.

**v0.3 sub-points deferred to v0.5+:** cross-module access control (private vs public resources — all v0.3 cross-module resources are public); cross-module refs in `secret.scope` lists (the union-kind list can't infer kind from context the way single-field attributes do); per-module incremental lockfile invalidation (v0.3 invalidates the whole project on any module change).

### 7.3 Multi-platform deploy targets — emitter-first, additive resources later

v0.1 supports `target = compose` only. v0.2 Phase 2 Week 14 lands `target = kubernetes` (`aislang-k8s` crate — see PLAN-90-days-v0_2.md §"Phase 2 — Track B foundation" for the 8 locked architectural decisions 43.1-43.8).  Future targets fall into two categories with different cost profiles:

| Target | Status | Conceptual match to v0.1 resources | Strategy |
|---|---|---|---|
| `compose` | v0.1 baseline | Direct — Docker Compose services + volumes | `aislang-compose` crate |
| `kubernetes` | v0.2 Phase 2 Week 14 Mon-Thu | Direct — Deployment / Service / ConfigMap / PVC / Ingress | `aislang-k8s` crate; Helm chart wrapper (Decision 43.7) over raw manifests (Decision 43.8) |
| ECS, Nomad | v0.3+ | Direct — task definitions, services, target groups | New emitter only; zero spec change |
| Databricks, MS Fabric, Spark, Vertex AI, Bedrock Agents | v0.3+ | Conceptual mismatch — endpoints, MLflow artifacts, lakehouse-native primitives | **New emitter + additive resource types.** Most v0.1 resources map with caveats; some platform-native concepts (Unity Catalog, MLflow registry, Databricks Functions) need new resource types like `databricks.endpoint`, `unity_catalog`, `mlflow_artifact`. These are introduced via the v0.3 modules/packages mechanism (§7.2) — `databricks.*` lives in an importable module, not in the language core. |

This keeps the v0.1 language small and stable while leaving room for the long tail of platforms. The `expose { ... }` block on `deploy` blocks (v0.2 Tue, replaces the v0.1 `Map<String, Int>` attribute) carries platform-agnostic fields (`port`, `web_ui`, `ingress`) by design — platform-specific emitters add their own fields under the same block as they grow (e.g. on Databricks, `expose { endpoint_name = "..." }` registers a model-serving endpoint).  Phase 2 Week 14 Thu's `ingress: String?` field follows the same additive-extension pattern; the Compose emitter silently ignores it.

**K8s resource mapping** (Decisions 43.1-43.8 in PLAN-90-days-v0_2.md):

| .ais source concept | K8s shape |
|---|---|
| `deploy "<label>" { target = kubernetes }` | One Helm chart named `<label>`; one `aislang.deploy=<label>` label on every emitted resource |
| Implicit LiteLLM router | Deployment + Service + ConfigMap (router YAML → CM) |
| Implicit Jaeger tracing | Deployment + Service (no CM — bundled defaults) |
| Implicit budget counter | Deployment only (no incoming traffic) |
| `agent "<label>" { … }` | Deployment + Service running `aislang mcp-serve --http 0.0.0.0:<expose.port>` |
| `.ais` source + `aislang.lock` + every `prompt` body | One `aislang-project` ConfigMap mounted at `/etc/aislang/` |
| Provider credentials | User-created Secret `aislang-providers`; `envFrom: secretRef` on LiteLLM + agent-runtime |
| `expose.web_ui = true` | Open WebUI Deployment + Service + PersistentVolumeClaim |
| `expose.ingress = "<host>"` (+ `web_ui = true`) | `networking.k8s.io/v1` Ingress routing `/` Prefix to the openwebui Service |
| OpenTelemetry traces (Week 15 Tue) | LiteLLM + agent-runtime pods carry `OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4318` + `OTEL_SERVICE_NAME=aislang.<component>.<deploy-label>` env vars; Jaeger pod is the receiver |

CLI behaviour: `aislang apply` shells to `helm upgrade --install <deploy-label> chart/`; `aislang destroy` shells to `helm uninstall <deploy-label>`.  Both graceful-degrade when `helm` isn't on PATH (apply writes artifacts + prints manual command; destroy hard-fails with a `kubectl delete -f manifests/` fallback hint).

**OTLP endpoint override** (Week 15 Tue): the chart's `values.yaml` exposes `observability.otlp_endpoint` so users running an external collector (Honeycomb, Tempo, Datadog, NewRelic, OTel Collector) can redirect spans without re-running `aislang plan` — `helm install --set observability.otlp_endpoint=https://otlp.honeycomb.io:443`.

### 7.4 Binary IR / wire format — not in v0.1

aiSlang has no execution VM. The compiled artifact is consumed by external tools (Docker, K8s, LiteLLM, OTel) that speak YAML/JSON. There is no aiSlang-specific bytecode target, so flatBuffers/protobuf/cap'n proto bring no benefit at v0.1 scale.

Decisions:

- **Internal AST:** Rust structs + `serde` + JSON. No separate IR.
- **Lockfile, state, plan output:** JSON. Diffable, inspectable, no schema compiler required.
- **Source language:** HCL-like (see §5.4).

Where binary IR *would* pay back, deferred until the use case appears:

- A **fleet-management plane** in Phase 2 that ships compiled plans to many remote agents — binary format would reduce wire size and parse cost. Decide when the use case is concrete, not before.
- A **machine-readable schema for the `.ais` language itself**, generated from the Rust AST via the `schemars` crate (JSON Schema output). Pays for LSP autocomplete, generated docs, and other-language SDKs. **Targeted for first Phase 1 task; not in v0.1.**

---

## 8. EBNF (finalized, week 1 Friday)

Aligned with HCL conventions where applicable — every resource in v0.1 (project, budget, model, prompt, mcp_server, agent, eval, secret, deploy) is grammatically *the same* construct: a `block`. This is the structural symmetry the parser exploits, and it lets future block-typed features (`decorator`, `catalog_model`, `include`, `module`, `package`) land without grammar changes — only with new entries in the semantic-recognition tables.

aiSlang-specific deviations from pure HCL: typed value literals (Money/Duration/Size/Percentage), the `per` infix keyword for `Rate` composition, and identifier-paths of arbitrary depth in `attribute_access` (HCL is single-dot; we keep the door open for v0.3+ module paths like `shared.models.fast`).

```ebnf
(* ========== file structure ========== *)

file              = { top_level_block } ;

top_level_block   = block ;
                       (* v0.1 semantic restriction: block_type must
                          be `project`. v0.2+ may also permit `include`,
                          `import`, `module`, `package` — reserved
                          keywords in §2. *)

block             = block_type , { label } , "{" , body , "}" ;
block_type        = identifier ;
label             = string_literal ;
                       (* v0.1: labels are quoted strings only.
                          HCL also permits identifier labels; aiSlang
                          defers that to v0.2+. *)

body              = { attribute | block } ;
                       (* whitespace (including newlines) separates
                          attributes and nested blocks inside a body.
                          v0.1 semantic restrictions:
                            • inside `project { }`: only blocks of the
                              nine resource types are valid
                            • inside resource bodies (budget / model /
                              prompt / mcp_server / agent / eval /
                              secret / deploy): only attributes, no
                              nested blocks.
                          v0.2 Tue (Track A2) carves an exception:
                            • inside `deploy { }`: one optional nested
                              `expose { ... }` block is also permitted.
                              See §4.9 for the field-set. *)

attribute         = identifier , "=" , expression ;

(* ========== expressions ========== *)

expression        = literal
                  | attribute_access
                  | list
                  | object
                  | rate ;

literal           = string_literal
                  | integer_literal
                  | float_literal
                  | boolean_literal
                  | money_literal
                  | duration_literal
                  | size_literal
                  | percentage_literal
                  | enum_member ;

list              = "[" , [ expression , { "," , expression } ] , "]" ;

object            = "{" , [ attribute , { "," , attribute } ] , "}" ;
                       (* attributes inside an object literal are
                          comma-separated; attributes inside a block
                          body are whitespace-separated.  v0.1 used
                          object literals for the `deploy.expose`
                          attribute value; v0.2 Tue promoted that to
                          a nested block (see §4.9).  Object literals
                          remain a grammar primitive and may be used
                          by future attributes. *)

attribute_access  = identifier , "." , identifier , { "." , identifier } ;
                       (* always at least one dot — distinguishes from
                          a bare-identifier `enum_member`.  v0.1 forms:
                            resource ref:   `model.fast`
                            catalog ref:    `anthropic.claude_haiku_4_5`
                          v0.3+ module path:`shared.models.fast` (the
                          variable-depth tail is reserved for this). *)

rate              = ( money_literal | integer_literal | float_literal ) ,
                    "per" ,
                    ( size_literal | duration_literal | unit_quantity ) ;

unit_quantity     = integer_literal , "_" , identifier ;
                       (* the only place a digit-then-underscore-then-
                          identifier token is legal.  Parses `1k_tokens`
                          as a Size literal `1k` bound to the unit name
                          `tokens`; the name is descriptive metadata
                          (the AST stores only the Size). *)

enum_member       = identifier ;
                       (* the type-checker validates the identifier is
                          a member of the enum type expected by the
                          enclosing attribute's declared field type —
                          e.g. `cost` is valid for `OptimizeFor`,
                          `chat` for `Capability`, `compose` for
                          `DeployTarget`.  See §3.4. *)

(* ========== ambiguity resolution ========== *)

(*  A bare identifier in expression position may be one of:
      1. boolean_literal — if the identifier is `true` or `false`
         (reserved tokens; the lexer never emits them as Ident)
      2. attribute_access — if the next token is "."
      3. enum_member — otherwise, validated against type context
    Reserved keywords from §2 (`project`, `budget`, `model`, ...)
    are tokenized as keywords and cannot appear as bare identifiers
    in expression position; their use there is a parse error. *)

(* ========== tokens (lexer level) ========== *)

identifier        = lowercase_letter , { lowercase_letter | digit | "_" } ;
                       (* strict snake_case; uppercase prefix reserved
                          for future type-name syntax. *)

string_literal    = '"' , { string_char | escape_sequence } , '"' ;
string_char       = ? any UTF-8 char other than `"`, `\`, or newline ? ;
escape_sequence   = "\" , ( '"' | "\" | "n" | "t" | "r" ) ;

integer_literal   = digit , { digit } ;
float_literal     = digit , { digit } , "." , digit , { digit } ;
boolean_literal   = "true" | "false" ;

(* --- Typed value literals (see §3.2; parsers in
       aislang_core::types::*::from_literal) --- *)

money_literal       = currency_symbol , number_value ;
currency_symbol     = "$" | "€" | "£" | "¥" ;
number_value        = digit , { digit } , [ "." , digit , { digit } ] ;

duration_literal    = digit , { digit } , duration_unit ;
duration_unit       = "ns" | "us" | "ms" | "s" | "m" | "h" | "d" ;

size_literal        = digit , { digit } , [ size_unit ] ;
size_unit           = "k" | "K" | "KB" | "MB" | "GB" ;

percentage_literal  = number_value , "%" ;

(*  Strings interpreted by the type-checker as `Path`, `URI`,
    `SemVer`, or `OciImage` based on the receiving field's declared
    type are *not* their own grammatical productions — they are
    `string_literal` at the grammar level.  See §3.2 for which
    domain types use this string-then-type-context pattern. *)

(* ========== comments ========== *)

(*  Line comments: `#` or `//` to end of line (HCL convention).
    Consumed by the lexer and not visible to the parser.  Block
    comments `/* … */` are deferred (§2). *)

(* ========== character classes ========== *)

lowercase_letter  = "a" | "b" | ... | "z" ;
digit             = "0" | "1" | ... | "9" ;
```

### 8.1 What "semantic restriction" means in practice

Several places above say a v0.1 source file is rejected by *semantic* checks rather than by the grammar:

| Restriction | Where checked | Why not in the grammar |
|---|---|---|
| Top-level block_type must be `project` | type-checker (week 3) | Lets v0.2+ add `include`/`module`/etc. without grammar changes |
| Inside `project {}`: only the 9 resource block_types | type-checker | Same — additive future block_types stay backward-compatible |
| Inside resource bodies: attributes only, no nested blocks | type-checker | A future resource type might want nested blocks (e.g. `agent { tool_choice {} }`); keeping it permissive in the grammar costs nothing |
| Enum member validity (e.g. `cost` is in `OptimizeFor`) | type-checker | Bare identifier is grammatical anywhere; only the type context decides if a particular identifier is valid |
| Reference resolution (e.g. `model.fast` resolves to an actual `model` block in the same project) | type-checker | Pure structural ref; resolution requires the full project tree |
| Catalog-ID validity (e.g. `anthropic.claude_haiku_4_5` is in the catalog) | resolver (week 4) | Catalog is loaded at resolution time, not parse time |

This separation means the parser is small (one block production for every resource type) and the type-checker / resolver hold all the domain knowledge.
